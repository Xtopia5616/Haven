//! Agent-facing LSP operations and the one-shot local server lifecycle.

mod protocol;

use crate::security::{AuthorizationEngine, FileReadPolicy};
use crate::{
    ConfirmationRequirement, DataSensitivity, ExecutionBoundary, NetworkAccess, OperationEffect,
    OperationIdempotency, OperationPolicy, OperationSpec, Tool, ToolConcurrency,
    ToolExecutionContext, ToolOperationScope, ToolResult,
};
use haven_common::config::{LspServerConfig, MAX_FILE_OPERATION_BYTES};
use haven_common::tools::{ToolCatalogGroup, ToolPresentation, ToolPrompt, ToolSource};
use haven_common::types::{CapabilityScope, RiskLevel};
use protocol::LspConnection;
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const MAX_LSP_DOCUMENT_BYTES: u64 = if MAX_FILE_OPERATION_BYTES < 1024 * 1024 {
    MAX_FILE_OPERATION_BYTES
} else {
    1024 * 1024
};
const MAX_LSP_RESULT_BYTES: usize = 2 * 1024 * 1024;
const MAX_LSP_CONCURRENT_SERVERS: usize = 2;
const LSP_PUSH_DIAGNOSTIC_WAIT: Duration = Duration::from_secs(5);
const LSP_SHUTDOWN_WAIT: Duration = Duration::from_secs(1);

pub(crate) struct LspTool {
    config_source: crate::lsp_server_config::LspServerConfigSource,
    authorization: Arc<AuthorizationEngine>,
    concurrent_servers: Arc<Semaphore>,
}

impl LspTool {
    pub(crate) fn new(
        config_source: crate::lsp_server_config::LspServerConfigSource,
        authorization: Arc<AuthorizationEngine>,
    ) -> Self {
        Self {
            config_source,
            authorization,
            concurrent_servers: Arc::new(Semaphore::new(MAX_LSP_CONCURRENT_SERVERS)),
        }
    }

    async fn run(
        &self,
        input: Value,
        context: &ToolExecutionContext,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let operation = LspOperation::parse(
            input
                .get("operation")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("LSP operation is required"))?,
        )?;
        let path_input = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("path is required"))?;
        let workspace = context
            .execution_workspace
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("LSP execution workspace is unavailable"))?;
        let workspace_root = workspace
            .root()
            .ok_or_else(|| anyhow::anyhow!("LSP execution workspace is unavailable"))?
            .to_path_buf();
        let workspace_directory = workspace
            .directory()
            .ok_or_else(|| anyhow::anyhow!("LSP execution workspace is unavailable"))?;

        let path = resolve_lsp_path(path_input, &workspace_root)?;
        let file_read_policy = self.authorization.file_read_policy("lsp").await;
        let LspDocument {
            canonical_path: document_path,
            relative_path: relative_document_path,
            text: document_text,
        } = read_lsp_document(
            &file_read_policy,
            &workspace_directory,
            &workspace_root,
            &path,
        )
        .await?;
        let server = select_lsp_server(self.config_source.list()?, &document_path)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no enabled LSP server matches this document; add an entry to lsp_servers in config.toml"
                )
            })?;

        let line = input.get("line").and_then(Value::as_u64);
        let character = input.get("character").and_then(Value::as_u64);
        let position = if operation.needs_position() {
            Some(document_position(
                &document_text,
                line.ok_or_else(|| anyhow::anyhow!("line is required"))?,
                character.ok_or_else(|| anyhow::anyhow!("character is required"))?,
            )?)
        } else {
            None
        };
        let document_uri = url::Url::from_file_path(&document_path)
            .map_err(|_| anyhow::anyhow!("document path cannot be represented as a file URI"))?
            .to_string();
        let workspace_uri = url::Url::from_directory_path(&workspace_root)
            .map_err(|_| anyhow::anyhow!("workspace path cannot be represented as a file URI"))?
            .to_string();

        let concurrent = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::cancelled("LSP request cancelled while waiting for a server slot")),
            permit = self.concurrent_servers.clone().acquire_owned() => permit
                .map_err(|_| anyhow::anyhow!("LSP server capacity is unavailable"))?,
        };
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::cancelled("LSP request cancelled before process start")),
            result = context.acquire_lsp_process_start_permit() => {
                result.map_err(crate::security::execution_policy_denied)?
            }
        };
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled(
                "LSP request cancelled before process start",
            ));
        }
        let mut process = spawn_lsp_server(&server, &workspace_root, permit).await?;
        let mut connection = LspConnection::new(
            process
                .stdout
                .take()
                .ok_or_else(|| anyhow::anyhow!("LSP server stdout is unavailable"))?,
            process
                .stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("LSP server stdin is unavailable"))?,
            workspace_uri.clone(),
        );
        connection.set_document_uri(document_uri.clone());
        let initialize = connection
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "rootPath": workspace_root.to_string_lossy(),
                    "rootUri": workspace_uri,
                    "workspaceFolders": [{"uri":workspace_uri,"name":workspace_root.file_name().and_then(|name| name.to_str()).unwrap_or("workspace")}],
                    "capabilities": {
                        "workspace": {
                            "configuration": true,
                            "workspaceFolders": {"supported": true}
                        },
                        "textDocument": {
                            "synchronization": {"didOpen": true, "didClose": true},
                            "diagnostic": {"dynamicRegistration": false, "relatedDocumentSupport": true},
                            "publishDiagnostics": {"relatedInformation": true}
                        }
                    }
                }),
            )
            .await?;
        if let Some(error) = initialize.error {
            anyhow::bail!(
                "configured LSP server rejected initialization ({})",
                error.code
            );
        }
        connection.notify("initialized", json!({})).await?;
        connection
            .notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": document_uri,
                        "languageId": server.language_id,
                        "version": 1,
                        "text": document_text
                    }
                }),
            )
            .await?;

        let params = operation.params(&input, &document_uri, position);
        let (result, source) = if matches!(operation, LspOperation::Diagnostics) {
            self.diagnostics(&mut connection, params).await?
        } else {
            let outcome = connection
                .request(operation.method(), params)
                .await
                .map_err(|_| {
                    anyhow::anyhow!("configured LSP server did not return a valid response")
                })?;
            if let Some(error) = outcome.error {
                anyhow::bail!(
                    "configured LSP server rejected the request ({})",
                    error.code
                );
            }
            (outcome.result.unwrap_or(Value::Null), "pull")
        };
        let mut result = result;
        redact_lsp_file_uris(&mut result, &workspace_directory);
        let result_bytes = serde_json::to_vec(&result)?;
        anyhow::ensure!(
            result_bytes.len() <= MAX_LSP_RESULT_BYTES,
            "LSP result exceeds the output limit"
        );

        // LSP servers must receive shutdown followed by exit before their
        // stdio pipes close. If they do not respond promptly, dropping the
        // process owner terminates the contained process tree.
        let shutdown = tokio::time::timeout(
            LSP_SHUTDOWN_WAIT,
            connection.request("shutdown", Value::Null),
        )
        .await;
        if matches!(shutdown, Ok(Ok(_))) {
            let _ = connection.notify("exit", Value::Null).await;
        }
        drop(connection);
        let _ = tokio::time::timeout(LSP_SHUTDOWN_WAIT, process.child.wait()).await;
        process.stop_stderr_drain();
        drop(concurrent);

        Ok(ToolResult::ok(json!({
            "operation": operation.as_str(),
            "server": server.name,
            "language_id": server.language_id,
            "path": relative_document_path.to_string_lossy(),
            "source": source,
            "result": result
        })))
    }

    async fn diagnostics<R, W>(
        &self,
        connection: &mut LspConnection<R, W>,
        params: Value,
    ) -> anyhow::Result<(Value, &'static str)>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        let outcome = connection
            .request("textDocument/diagnostic", params)
            .await
            .map_err(|_| {
                anyhow::anyhow!("configured LSP server did not return a valid diagnostic response")
            })?;
        if outcome.error.is_none()
            && let Some(result) = outcome.result
        {
            return Ok((result, "pull"));
        }
        if outcome
            .error
            .as_ref()
            .is_some_and(|error| error.code != -32601)
        {
            let code = outcome
                .error
                .as_ref()
                .map(|error| error.code)
                .unwrap_or_default();
            anyhow::bail!("configured LSP server rejected the diagnostic request ({code})");
        }
        let pushed = outcome.pushed_diagnostics.or(connection
            .wait_for_pushed_diagnostics(LSP_PUSH_DIAGNOSTIC_WAIT)
            .await?);
        match pushed {
            Some(report) => Ok((report, "push")),
            None => anyhow::bail!(
                "configured LSP server supports neither pull diagnostics nor a diagnostic notification"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LspOperation {
    Definition,
    References,
    Hover,
    DocumentSymbols,
    Diagnostics,
}

impl LspOperation {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "definition" => Ok(Self::Definition),
            "references" => Ok(Self::References),
            "hover" => Ok(Self::Hover),
            "document_symbols" => Ok(Self::DocumentSymbols),
            "diagnostics" => Ok(Self::Diagnostics),
            _ => anyhow::bail!("unsupported LSP operation"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::References => "references",
            Self::Hover => "hover",
            Self::DocumentSymbols => "document_symbols",
            Self::Diagnostics => "diagnostics",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::References => "textDocument/references",
            Self::Hover => "textDocument/hover",
            Self::DocumentSymbols => "textDocument/documentSymbol",
            Self::Diagnostics => "textDocument/diagnostic",
        }
    }

    fn needs_position(self) -> bool {
        matches!(self, Self::Definition | Self::References | Self::Hover)
    }

    fn params(self, input: &Value, uri: &str, position: Option<(u64, u64)>) -> Value {
        match self {
            Self::Definition | Self::Hover => json!({
                "textDocument":{"uri":uri},
                "position":{"line":position.map(|position| position.0).unwrap_or_default(),"character":position.map(|position| position.1).unwrap_or_default()}
            }),
            Self::References => json!({
                "textDocument":{"uri":uri},
                "position":{"line":position.map(|position| position.0).unwrap_or_default(),"character":position.map(|position| position.1).unwrap_or_default()},
                "context":{"includeDeclaration":input.get("include_declaration").and_then(Value::as_bool).unwrap_or(true)}
            }),
            Self::DocumentSymbols | Self::Diagnostics => {
                json!({"textDocument":{"uri":uri}})
            }
        }
    }
}

#[async_trait::async_trait]
impl Tool for LspTool {
    fn name(&self) -> String {
        "lsp".into()
    }

    fn description(&self) -> String {
        crate::prompts::LSP_DESCRIPTION.into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn idempotency(&self, _input: &Value) -> OperationIdempotency {
        OperationIdempotency::NonIdempotent
    }

    fn concurrency(&self, _input: &Value) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    fn default_timeout_secs(&self) -> u64 {
        60
    }

    fn timeout_outcome(&self) -> crate::ToolExecutionOutcome {
        crate::ToolExecutionOutcome::TimedOutUnknown
    }

    async fn execute_with_authorization_context(
        &self,
        input: Value,
        cancel: CancellationToken,
        _target_binding: Option<&str>,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        let authorization = context.clone_execution_authorization().ok_or_else(|| {
            crate::security::execution_policy_denied(
                "LSP server process requires proof-derived execution authorization".into(),
            )
        })?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(ToolResult::cancelled("LSP request cancelled")),
            _ = authorization.wait_for_policy_revocation() => Ok(ToolResult::cancelled("LSP request cancelled because execution policy changed")),
            result = self.run(input, context, cancel.clone()) => result,
        }
    }

    async fn execute(
        &self,
        _input: Value,
        _cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        anyhow::bail!("LSP requests require proof-derived execution authorization")
    }

    fn input_schema(&self) -> Value {
        json!({"type":"object", "additionalProperties":false})
    }
}

pub(super) fn operation_specs() -> Vec<OperationSpec> {
    [
        (
            "lsp.definition",
            "definition",
            position_schema(false),
            "代码定义",
        ),
        (
            "lsp.references",
            "references",
            position_schema(true),
            "代码引用",
        ),
        ("lsp.hover", "hover", position_schema(false), "代码悬停信息"),
        (
            "lsp.document_symbols",
            "document_symbols",
            document_schema(),
            "文档符号",
        ),
        (
            "lsp.diagnostics",
            "diagnostics",
            document_schema(),
            "代码诊断",
        ),
    ]
    .into_iter()
    .map(|(name, operation, schema, label)| {
        let contract = super::operation_contract(name);
        let capability = CapabilityScope::try_new(name).expect("LSP operation name is canonical");
        OperationSpec {
            name: name.into(),
            description: crate::prompts::operation_text(name).description.into(),
            fixed: vec![("operation".into(), json!(operation))],
            schema,
            policy: OperationPolicy {
                risk_level: contract.risk_override.unwrap_or(RiskLevel::High),
                capability,
                confirmation: ConfirmationRequirement::SecurityPolicy,
                idempotency: OperationIdempotency::NonIdempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::Exclusive,
                effect: OperationEffect::ExternalEffect,
                data_sensitivity: DataSensitivity::UserData,
                network_access: NetworkAccess::Opaque,
                execution_boundary: ExecutionBoundary::HostProcess,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::Files,
            presentation: ToolPresentation {
                label: label.into(),
                renderer: "lsp".into(),
                icon: "code".into(),
                represented_source: ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: crate::prompts::operation_text(name).when_to_use.into(),
                when_not_to_use: String::new(),
                key_operations: vec![name.into()],
            },
            identity: None,
        }
    })
    .collect()
}

fn document_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{"path":{"type":"string","minLength":1,"description":"Workspace-relative source file path; absolute paths are limited to the active execution workspace"}},
        "required":["path"]
    })
}

fn position_schema(include_declaration: bool) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("path".into(), json!({"type":"string","minLength":1,"description":"Workspace-relative source file path; absolute paths are limited to the active execution workspace"}));
    properties.insert(
        "line".into(),
        json!({"type":"integer","minimum":1,"description":"1-based source line"}),
    );
    properties.insert("character".into(), json!({"type":"integer","minimum":1,"description":"1-based UTF-16 code-unit column, as used by LSP position fields"}));
    if include_declaration {
        properties.insert("include_declaration".into(), json!({"type":"boolean","default":true,"description":"Whether reference results include the declaration"}));
    }
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":["path","line","character"]})
}

fn resolve_lsp_path(path: &str, workspace_root: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(!path.trim().is_empty(), "path is required");
    let input = Path::new(path);
    if input
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        anyhow::bail!("path traversal is not allowed for LSP requests");
    }
    if input.is_absolute() {
        Ok(input.to_path_buf())
    } else {
        Ok(workspace_root.join(input))
    }
}

fn select_lsp_server(
    servers: Vec<LspServerConfig>,
    document_path: &Path,
) -> Option<LspServerConfig> {
    let file_name = document_path.file_name()?.to_str()?.to_ascii_lowercase();
    servers
        .into_iter()
        .filter_map(|server| {
            if !server.enabled {
                return None;
            }
            let matching_extension_length = server
                .extensions
                .iter()
                .filter(|extension| file_name.ends_with(&extension.to_ascii_lowercase()))
                .map(String::len)
                .max()?;
            Some((server, matching_extension_length))
        })
        .max_by_key(|(_, extension_length)| *extension_length)
        .map(|(server, _)| server)
}

struct LspDocument {
    canonical_path: PathBuf,
    relative_path: PathBuf,
    text: String,
}

async fn read_lsp_document(
    policy: &FileReadPolicy,
    workspace_directory: &haven_platform::filesystem::DirectoryCapability,
    workspace_root: &Path,
    path: &Path,
) -> anyhow::Result<LspDocument> {
    let policy = policy.clone();
    let directory = workspace_directory.clone();
    let workspace_root = workspace_root.to_path_buf();
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = policy.open_read(&path)?;
        let metadata = file.metadata()?;
        anyhow::ensure!(metadata.is_file(), "LSP path must be a regular file");
        anyhow::ensure!(
            metadata.len() <= MAX_LSP_DOCUMENT_BYTES,
            "LSP document exceeds the 1 MiB size limit"
        );
        let resolved = std::fs::canonicalize(&path)?;
        let relative_path = directory.relative_path(&resolved).ok_or_else(|| {
            anyhow::anyhow!("LSP documents must be inside the active execution workspace")
        })?;
        let canonical_path = workspace_root.join(&relative_path);
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_LSP_DOCUMENT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_LSP_DOCUMENT_BYTES,
            "LSP document exceeds the 1 MiB size limit"
        );
        let text = String::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("LSP documents must be UTF-8 text"))?;
        Ok(LspDocument {
            canonical_path,
            relative_path,
            text,
        })
    })
    .await
    .map_err(|_| anyhow::anyhow!("LSP document reader failed"))?
}

fn redact_lsp_file_uris(
    value: &mut Value,
    workspace_directory: &haven_platform::filesystem::DirectoryCapability,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                redact_lsp_file_uris(value, workspace_directory);
            }
        }
        Value::Object(fields) => {
            for (name, value) in fields {
                if matches!(name.as_str(), "uri" | "targetUri") {
                    let Some(uri) = value.as_str() else {
                        continue;
                    };
                    let Ok(uri) = url::Url::parse(uri) else {
                        continue;
                    };
                    if uri.scheme() != "file" {
                        continue;
                    }
                    *value = match uri
                        .to_file_path()
                        .ok()
                        .and_then(|path| workspace_directory.relative_path(&path))
                    {
                        Some(relative_path) => {
                            Value::String(relative_path.to_string_lossy().into_owned())
                        }
                        None => Value::Null,
                    };
                } else {
                    redact_lsp_file_uris(value, workspace_directory);
                }
            }
        }
        _ => {}
    }
}

fn document_position(text: &str, line: u64, character: u64) -> anyhow::Result<(u64, u64)> {
    let line_index = usize::try_from(
        line.checked_sub(1)
            .ok_or_else(|| anyhow::anyhow!("line must be at least 1"))?,
    )?;
    let character_index = usize::try_from(
        character
            .checked_sub(1)
            .ok_or_else(|| anyhow::anyhow!("character must be at least 1"))?,
    )?;
    let line_text = text
        .lines()
        .nth(line_index)
        .ok_or_else(|| anyhow::anyhow!("line is outside the source document"))?;
    let utf16_length = line_text.encode_utf16().count();
    anyhow::ensure!(
        character_index < utf16_length,
        "character must point to a code unit inside the source line"
    );
    Ok((line_index as u64, character_index as u64))
}

struct LspProcess {
    child: Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: Option<tokio::process::ChildStdout>,
    _containment: haven_platform::process_containment::ProcessContainment,
    stderr_drain: Option<tokio::task::JoinHandle<()>>,
}

impl LspProcess {
    fn stop_stderr_drain(&mut self) {
        if let Some(drain) = self.stderr_drain.take() {
            drain.abort();
        }
    }
}

async fn spawn_lsp_server(
    server: &LspServerConfig,
    workspace_root: &Path,
    permit: crate::security::ExecutionPolicyPermit,
) -> anyhow::Result<LspProcess> {
    let containment = haven_platform::process_containment::ProcessContainment::new()?;
    let mut command = tokio::process::Command::new(&server.command);
    command
        .args(&server.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(workspace_root)
        .env_clear()
        .envs(
            haven_platform::process_environment::filter_runtime_baseline_environment(
                std::env::vars_os(),
            ),
        );
    #[cfg(windows)]
    containment.prepare_command(command.as_std_mut(), crate::CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    containment.prepare_command(command.as_std_mut(), 0);
    let spawned = {
        let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
        command.kill_on_drop(true).spawn()
    };
    let mut child =
        spawned.map_err(|_| anyhow::anyhow!("could not start the configured LSP server"))?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("configured LSP server did not expose a process id"))?;
    #[cfg(windows)]
    let attach = child
        .raw_handle()
        .ok_or_else(|| std::io::Error::other("LSP server process handle is unavailable"))
        .and_then(|handle| containment.attach_and_resume(pid, handle));
    #[cfg(not(windows))]
    let attach = containment.attach_and_resume(pid, ());
    if attach.is_err() {
        drop(permit);
        let _ = child.kill().await;
        anyhow::bail!("could not contain the configured LSP server process");
    }
    drop(permit);
    let stderr = child.stderr.take();
    let stderr_drain = stderr.map(drain_stderr);
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    Ok(LspProcess {
        child,
        stdin,
        stdout,
        _containment: containment,
        stderr_drain,
    })
}

fn drain_stderr(stderr: ChildStderr) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut stderr = stderr;
        let mut buffer = [0u8; 8192];
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests;
