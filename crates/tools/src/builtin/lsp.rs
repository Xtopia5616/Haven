//! Agent-facing LSP operations and the one-shot local server lifecycle.

mod operations;
mod protocol;

pub(super) use operations::operation_specs;

use crate::security::{AuthorizationEngine, FileReadPolicy};
use crate::{OperationIdempotency, Tool, ToolConcurrency, ToolExecutionContext, ToolResult};
use haven_common::config::{LspServerConfig, MAX_FILE_OPERATION_BYTES};
use haven_common::types::RiskLevel;
use protocol::LspConnection;
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::process::Child;
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

        if operation == LspOperation::WorkspaceSymbols {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("query is required"))?;
            anyhow::ensure!(!query.trim().is_empty(), "query must not be empty");
        }

        let (server, document) = if operation == LspOperation::WorkspaceSymbols {
            let server_name = input
                .get("server")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("server is required"))?;
            let server = select_lsp_server_by_name(self.config_source.list()?, server_name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no enabled LSP server has that name; check the lsp_servers entries in config.toml"
                    )
                })?;
            (server, None)
        } else {
            let path_input = input
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("path is required"))?;
            let path = resolve_lsp_path(path_input, &workspace_root)?;
            let file_read_policy = self.authorization.file_read_policy("lsp").await;
            let document = read_lsp_document(
                &file_read_policy,
                &workspace_directory,
                &workspace_root,
                &path,
            )
            .await?;
            let server = select_lsp_server(self.config_source.list()?, &document.canonical_path)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no enabled LSP server matches this document; add an entry to lsp_servers in config.toml"
                    )
                })?;
            (server, Some(document))
        };
        let relative_document_path = document
            .as_ref()
            .map(|document| document.relative_path.clone());
        let document_uri = document
            .as_ref()
            .map(|document| {
                url::Url::from_file_path(&document.canonical_path)
                    .map(|uri| uri.to_string())
                    .map_err(|_| {
                        anyhow::anyhow!("document path cannot be represented as a file URI")
                    })
            })
            .transpose()?;
        let document_text = document.as_ref().map(|document| document.text.as_str());

        let line = input.get("line").and_then(Value::as_u64);
        let character = input.get("character").and_then(Value::as_u64);
        let position = if operation.needs_position() {
            Some(document_position(
                document_text.ok_or_else(|| anyhow::anyhow!("source document is required"))?,
                line.ok_or_else(|| anyhow::anyhow!("line is required"))?,
                character.ok_or_else(|| anyhow::anyhow!("character is required"))?,
            )?)
        } else {
            None
        };
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
        let mut process = spawn_lsp_server(&server, &workspace_root, permit, &cancel).await?;
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
        if let Some(document_uri) = &document_uri {
            connection.set_document_uri(document_uri.clone());
        }
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
                            "symbol": {"dynamicRegistration": false},
                            "workspaceFolders": {"supported": true}
                        },
                        "textDocument": {
                            "definition": {"dynamicRegistration": false, "linkSupport": true},
                            "implementation": {"dynamicRegistration": false, "linkSupport": true},
                            "typeDefinition": {"dynamicRegistration": false, "linkSupport": true},
                            "references": {"dynamicRegistration": false},
                            "hover": {"dynamicRegistration": false, "contentFormat": ["markdown", "plaintext"]},
                            "documentSymbol": {"dynamicRegistration": false, "hierarchicalDocumentSymbolSupport": true},
                            "synchronization": {"dynamicRegistration": false, "didOpen": true, "didClose": true},
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
        if let Some(document_uri) = &document_uri {
            connection
                .notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": document_uri,
                            "languageId": server.language_id,
                            "version": 1,
                            "text": document_text.unwrap_or_default()
                        }
                    }),
                )
                .await?;
        }

        let params = operation.params(&input, document_uri.as_deref(), position)?;
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
        let sandbox_exit = match tokio::time::timeout(LSP_SHUTDOWN_WAIT, process.child.wait()).await
        {
            Ok(Ok(exit)) => exit,
            _ => {
                let _ = process.child.kill().await;
                tokio::time::timeout(LSP_SHUTDOWN_WAIT, process.child.wait())
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten()
            }
        };
        process.stop_stderr_drain();
        if let Some(monitor) = process.scratch_monitor.take() {
            monitor.abort();
        }
        drop(concurrent);

        let mut output = json!({
            "operation": operation.as_str(),
            "server": server.name,
            "language_id": server.language_id,
            "path": relative_document_path.map(|path| path.to_string_lossy().into_owned()),
            "source": source,
            "result": result
        });
        if let Some(notice) = &process.sandbox_notice {
            output["sandbox_notice"] = json!(notice);
        }
        if let Some(isolation) = sandbox_exit.and_then(|exit| exit.isolation) {
            output["sandbox_isolation"] = serde_json::to_value(isolation)?;
        }
        Ok(ToolResult::ok(output))
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
    Implementation,
    TypeDefinition,
    References,
    Hover,
    DocumentSymbols,
    WorkspaceSymbols,
    Diagnostics,
}

impl LspOperation {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "definition" => Ok(Self::Definition),
            "implementation" => Ok(Self::Implementation),
            "type_definition" => Ok(Self::TypeDefinition),
            "references" => Ok(Self::References),
            "hover" => Ok(Self::Hover),
            "document_symbols" => Ok(Self::DocumentSymbols),
            "workspace_symbols" => Ok(Self::WorkspaceSymbols),
            "diagnostics" => Ok(Self::Diagnostics),
            _ => anyhow::bail!("unsupported LSP operation"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Implementation => "implementation",
            Self::TypeDefinition => "type_definition",
            Self::References => "references",
            Self::Hover => "hover",
            Self::DocumentSymbols => "document_symbols",
            Self::WorkspaceSymbols => "workspace_symbols",
            Self::Diagnostics => "diagnostics",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::Implementation => "textDocument/implementation",
            Self::TypeDefinition => "textDocument/typeDefinition",
            Self::References => "textDocument/references",
            Self::Hover => "textDocument/hover",
            Self::DocumentSymbols => "textDocument/documentSymbol",
            Self::WorkspaceSymbols => "workspace/symbol",
            Self::Diagnostics => "textDocument/diagnostic",
        }
    }

    fn needs_position(self) -> bool {
        matches!(
            self,
            Self::Definition
                | Self::Implementation
                | Self::TypeDefinition
                | Self::References
                | Self::Hover
        )
    }

    fn params(
        self,
        input: &Value,
        uri: Option<&str>,
        position: Option<(u64, u64)>,
    ) -> anyhow::Result<Value> {
        let document_uri = || {
            uri.ok_or_else(|| anyhow::anyhow!("source document is required for this LSP operation"))
        };
        let lsp_position = || {
            position.ok_or_else(|| {
                anyhow::anyhow!("source position is required for this LSP operation")
            })
        };
        Ok(match self {
            Self::Definition | Self::Implementation | Self::TypeDefinition | Self::Hover => json!({
                "textDocument":{"uri":document_uri()?},
                "position":{"line":lsp_position()?.0,"character":lsp_position()?.1}
            }),
            Self::References => json!({
                "textDocument":{"uri":document_uri()?},
                "position":{"line":lsp_position()?.0,"character":lsp_position()?.1},
                "context":{"includeDeclaration":input.get("include_declaration").and_then(Value::as_bool).unwrap_or(true)}
            }),
            Self::DocumentSymbols | Self::Diagnostics => {
                json!({"textDocument":{"uri":document_uri()?}})
            }
            Self::WorkspaceSymbols => json!({
                "query": input.get("query").and_then(Value::as_str).unwrap_or_default()
            }),
        })
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

    async fn authorization_contract(
        &self,
        input: &Value,
    ) -> Result<crate::ToolAuthorizationContract, String> {
        let mut policy = self.try_operation_policy(input)?;
        match self.authorization.sandbox_mode().await {
            haven_common::types::SandboxMode::WorkspaceWrite
            | haven_common::types::SandboxMode::ReadOnly => {
                policy.execution_boundary = crate::ExecutionBoundary::SandboxedHost;
                policy.network_access = crate::NetworkAccess::Opaque;
            }
            _ => {
                policy.execution_boundary = crate::ExecutionBoundary::HostProcess;
                policy.network_access = crate::NetworkAccess::Opaque;
            }
        }
        Ok(crate::ToolAuthorizationContract {
            policy,
            target_binding: None,
            resources: self.authorization_resources(input)?,
        })
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

fn select_lsp_server_by_name(
    servers: Vec<LspServerConfig>,
    requested_name: &str,
) -> Option<LspServerConfig> {
    servers
        .into_iter()
        .find(|server| server.enabled && server.name.eq_ignore_ascii_case(requested_name))
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
    child: LspProcessChild,
    stdin: Option<Box<dyn AsyncWrite + Unpin + Send>>,
    stdout: Option<Box<dyn AsyncRead + Unpin + Send>>,
    stderr_drain: Option<tokio::task::JoinHandle<()>>,
    _scratch: Option<crate::shell_runtime::ProcessScratchDirectory>,
    scratch_monitor: Option<tokio::task::JoinHandle<()>>,
    sandbox_notice: Option<String>,
}

enum LspProcessChild {
    Host {
        child: Child,
        _containment: haven_platform::process_containment::ProcessContainment,
    },
    Sandboxed(haven_platform::sandboxed_process::SandboxedProcess),
}

impl LspProcessChild {
    async fn wait(
        &mut self,
    ) -> anyhow::Result<Option<haven_platform::sandboxed_process::SandboxedProcessExit>> {
        match self {
            Self::Host { child, .. } => {
                child.wait().await?;
                Ok(None)
            }
            Self::Sandboxed(child) => child.wait().await.map(Some).map_err(Into::into),
        }
    }

    async fn kill(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Host { child, .. } => child.kill().await.map_err(Into::into),
            Self::Sandboxed(child) => child.kill().await.map_err(Into::into),
        }
    }
}

impl LspProcess {
    fn stop_stderr_drain(&mut self) {
        if let Some(drain) = self.stderr_drain.take() {
            drain.abort();
        }
    }
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        self.stop_stderr_drain();
        if let Some(monitor) = self.scratch_monitor.take() {
            monitor.abort();
        }
    }
}

async fn spawn_lsp_server(
    server: &LspServerConfig,
    workspace_root: &Path,
    permit: crate::security::ExecutionPolicyPermit,
    cancel: &CancellationToken,
) -> anyhow::Result<LspProcess> {
    match permit.sandbox_mode() {
        haven_common::types::SandboxMode::FullAccess => {
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
            let mut child = spawned
                .map_err(|_| anyhow::anyhow!("could not start the configured LSP server"))?;
            let pid = child.id().ok_or_else(|| {
                anyhow::anyhow!("configured LSP server did not expose a process id")
            })?;
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
            let stderr_drain = child.stderr.take().map(drain_stderr);
            let stdin = child
                .stdin
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn AsyncWrite + Unpin + Send>);
            let stdout = child
                .stdout
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn AsyncRead + Unpin + Send>);
            Ok(LspProcess {
                child: LspProcessChild::Host {
                    child,
                    _containment: containment,
                },
                stdin,
                stdout,
                stderr_drain,
                _scratch: None,
                scratch_monitor: None,
                sandbox_notice: None,
            })
        }
        haven_common::types::SandboxMode::WorkspaceWrite => {
            #[cfg(windows)]
            {
                let scratch = crate::shell_runtime::ProcessScratchDirectory::create_lsp()?;
                scratch.ensure_within_capacity().await?;
                let mut command = std::process::Command::new(&server.command);
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
                    )
                    .env("TEMP", scratch.path())
                    .env("TMP", scratch.path())
                    .env("TMPDIR", scratch.path());
                let mut runtime_roots = crate::process_sandbox::runtime_dependency_roots(&command);
                if let Some(parent) = Path::new(&server.command)
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    && parent.is_dir()
                {
                    runtime_roots.push(parent.to_path_buf());
                }
                let filesystem = permit
                    .workspace_lsp_directories(workspace_root, &runtime_roots, scratch.path())
                    .map_err(crate::security::execution_policy_denied)?;
                let request = crate::process_sandbox::request_from_command(
                    &command,
                    filesystem
                        .readwrite_roots
                        .iter()
                        .map(|root| root.root().to_path_buf())
                        .collect(),
                    filesystem
                        .readonly_roots
                        .iter()
                        .map(|root| root.root().to_path_buf())
                        .collect(),
                    filesystem.protected_readonly_paths,
                    filesystem.denied_paths,
                    permit.process_network_policy(),
                    false,
                )?;
                let mut child = haven_platform::sandboxed_process::SandboxedProcess::spawn(
                    request,
                    permit.mxc_isolation_mode(),
                )
                .await?;
                drop(permit);
                if cancel.is_cancelled() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    anyhow::bail!("LSP request cancelled during isolated process start");
                }
                let notice = child.sandbox_notice().to_owned();
                let stderr_drain = child.take_stderr().map(drain_stderr);
                let stdin = child
                    .take_stdin()
                    .map(|pipe| Box::new(pipe) as Box<dyn AsyncWrite + Unpin + Send>);
                let stdout = child
                    .take_stdout()
                    .map(|pipe| Box::new(pipe) as Box<dyn AsyncRead + Unpin + Send>);
                let monitor_cancel = cancel.clone();
                let monitor_path = scratch.path().to_path_buf();
                let scratch_monitor = tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = monitor_cancel.cancelled() => return,
                            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                        }
                        let path = monitor_path.clone();
                        let exceeded = tokio::task::spawn_blocking(move || {
                            crate::shell_runtime::directory_size_exceeds(
                                &path,
                                crate::shell_runtime::LSP_SCRATCH_MAX_BYTES,
                                crate::shell_runtime::LSP_SCRATCH_MAX_ENTRIES,
                            )
                        })
                        .await;
                        match exceeded {
                            Ok(Ok(false)) => {}
                            Ok(Ok(true)) | Ok(Err(_)) | Err(_) => {
                                tracing::warn!(
                                    "isolated LSP scratch exceeded its capacity or could not be inspected"
                                );
                                monitor_cancel.cancel();
                                return;
                            }
                        }
                    }
                });
                Ok(LspProcess {
                    child: LspProcessChild::Sandboxed(child),
                    stdin,
                    stdout,
                    stderr_drain,
                    _scratch: Some(scratch),
                    scratch_monitor: Some(scratch_monitor),
                    sandbox_notice: Some(notice),
                })
            }
            #[cfg(not(windows))]
            {
                let _ = (server, workspace_root, permit, cancel);
                anyhow::bail!("Workspace Write LSP isolation requires Windows MXC")
            }
        }
        haven_common::types::SandboxMode::ReadOnly => {
            anyhow::bail!("configured LSP servers are blocked by the read-only sandbox")
        }
    }
}

fn drain_stderr<R>(stderr: R) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
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
