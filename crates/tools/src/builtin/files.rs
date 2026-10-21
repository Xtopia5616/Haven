use haven_common::config::{
    MAX_FILE_BYTE_READ_MAX_BYTES, MAX_FILE_LINE_MAX_BYTES, MAX_FILE_MAX_LIST_ENTRIES,
    MAX_FILE_OPERATION_BYTES, MAX_FILE_SUMMARY_INPUT_CHARS, MAX_FILE_SUMMARY_TIMEOUT_SECS,
};
use haven_common::media::MediaRepresentationKind;
use haven_llm::LlmRouter;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::file_outline;
use super::file_search::{FileSearchEngine, FileSearchMode, SearchOptions, SearchRequest};
use super::media::{MediaParams, MediaTool};
use crate::security::{AuthorizationEngine, FileReadPolicy};
use crate::{ManagedAssetRegistry, ToolResult};

use file_media_handoff::{media_operation_for, register_rich_path_asset};
use file_paths::{
    annotate_file_result, inspect_open_file, missing_file, redact_managed_file_result,
    resolve_workspace_path,
};
use file_read::{read_bytes, read_full, read_lines};
use file_summary::summarize;

#[path = "file_classification.rs"]
mod file_classification;
#[path = "file_contract.rs"]
mod file_contract;
#[path = "file_media_handoff.rs"]
mod file_media_handoff;
#[path = "file_mutations.rs"]
mod file_mutations;
#[path = "file_paths.rs"]
mod file_paths;
#[path = "file_read.rs"]
mod file_read;
#[path = "file_summary.rs"]
mod file_summary;

const MAX_SUMMARY_FOCUS_CHARS: usize = 2_000;
const UNTRUSTED_DOCUMENT_START: &str = "[BEGIN DERIVED ATTACHMENT";
const UNTRUSTED_DOCUMENT_END: &str = "[END DERIVED ATTACHMENT]";
const MAX_PATCH_EDITS: usize = 64;
const MAX_PATCH_MATCHES: usize = 100_000;
const MAX_PATCH_INPUT_BYTES: usize = 256 * 1024;
const MAX_INSPECT_HASH_BYTES: u64 = 64 * 1024 * 1024;

pub struct FilesTool {
    summarizer: Option<Arc<LlmRouter>>,
    max_output_chars: usize,
    max_full_file_bytes: u64,
    max_patch_file_bytes: u64,
    line_span: u64,
    max_line_bytes: usize,
    summary_input_chars: usize,
    max_list_entries: usize,
    max_byte_read_bytes: u64,
    max_write_bytes: u64,
    summary_timeout_secs: u64,
    search: FileSearchEngine,
    managed_assets: ManagedAssetRegistry,
    media_tool: Option<Arc<MediaTool>>,
    authorization: Arc<AuthorizationEngine>,
}

impl Default for FilesTool {
    fn default() -> Self {
        Self {
            summarizer: None,
            max_output_chars: 20_000,
            max_full_file_bytes: 128_000,
            max_patch_file_bytes: 8 * 1024 * 1024,
            line_span: 100,
            max_line_bytes: 128_000,
            summary_input_chars: 60_000,
            max_list_entries: 1_000,
            max_byte_read_bytes: 16 * 1024 * 1024,
            max_write_bytes: 16 * 1024 * 1024,
            summary_timeout_secs: 120,
            search: FileSearchEngine::default(),
            managed_assets: ManagedAssetRegistry::default(),
            media_tool: None,
            authorization: Arc::new(AuthorizationEngine::new()),
        }
    }
}

/// Files operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesOperation {
    Read,
    Inspect,
    Write,
    CreateDir,
    Patch,
    Copy,
    Move,
    Delete,
    List,
    Summary,
    Search,
    Outline,
}

impl FilesOperation {
    fn tool_name(self) -> &'static str {
        match self {
            Self::Read => "files.read",
            Self::Inspect => "files.inspect",
            Self::Write => "files.write",
            Self::CreateDir => "files.create_dir",
            Self::Patch => "files.patch",
            Self::Copy => "files.copy",
            Self::Move => "files.move",
            Self::Delete => "files.delete",
            Self::List => "files.list",
            Self::Summary => "files.summary",
            Self::Search => "files.search",
            Self::Outline => "files.outline",
        }
    }
}

/// One exact replacement in a files.patch request.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct FilesPatchEdit {
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub expected_matches: Option<usize>,
    #[serde(default)]
    pub replace_all: bool,
}

/// Typed parameters for FilesTool.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct FilesParams {
    #[serde(default)]
    pub operation: Option<FilesOperation>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub dry_run: Option<bool>,
    #[serde(default)]
    pub edits: Option<Vec<FilesPatchEdit>>,
    #[serde(default)]
    pub unified_diff: Option<String>,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub start_line: Option<u64>,
    #[serde(default)]
    pub end_line: Option<u64>,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub max_chars: Option<u64>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub max_depth: Option<i64>,
    #[serde(default)]
    pub max_results: Option<i64>,
    #[serde(default)]
    pub ignore_hidden: Option<bool>,
    #[serde(default)]
    pub max_file_size: Option<u64>,
    #[serde(default)]
    pub max_symbols: Option<u64>,
    #[serde(rename = "_session_id", default, skip_serializing)]
    pub(crate) session_id: Option<String>,
}

impl FilesTool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        summarizer: Option<Arc<LlmRouter>>,
        max_output_chars: usize,
        max_full_file_bytes: u64,
        max_patch_file_bytes: u64,
        line_span: u64,
        max_line_bytes: usize,
        summary_input_chars: usize,
        max_list_entries: usize,
        max_byte_read_bytes: u64,
        summary_timeout_secs: u64,
        search: FileSearchEngine,
        managed_assets: ManagedAssetRegistry,
    ) -> Self {
        Self {
            summarizer,
            max_output_chars,
            max_full_file_bytes: max_full_file_bytes.min(MAX_FILE_OPERATION_BYTES),
            max_patch_file_bytes: max_patch_file_bytes.clamp(1, MAX_FILE_OPERATION_BYTES),
            line_span,
            max_line_bytes: max_line_bytes.clamp(1, MAX_FILE_LINE_MAX_BYTES),
            summary_input_chars: summary_input_chars.min(MAX_FILE_SUMMARY_INPUT_CHARS),
            max_list_entries: max_list_entries.min(MAX_FILE_MAX_LIST_ENTRIES),
            max_byte_read_bytes: max_byte_read_bytes.clamp(1, MAX_FILE_BYTE_READ_MAX_BYTES),
            max_write_bytes: 16 * 1024 * 1024,
            summary_timeout_secs: summary_timeout_secs.clamp(1, MAX_FILE_SUMMARY_TIMEOUT_SECS),
            search,
            managed_assets,
            media_tool: None,
            authorization: Arc::new(AuthorizationEngine::new()),
        }
    }

    pub(crate) fn with_media_tool(mut self, media_tool: Arc<MediaTool>) -> Self {
        self.media_tool = Some(media_tool);
        self
    }

    pub(crate) fn with_max_write_bytes(mut self, max_write_bytes: u64) -> Self {
        self.max_write_bytes = max_write_bytes.clamp(1, MAX_FILE_OPERATION_BYTES);
        self
    }

    pub(crate) fn with_authorization_engine(
        mut self,
        authorization: Arc<AuthorizationEngine>,
    ) -> Self {
        self.authorization = authorization;
        self
    }

    async fn file_read_policy(&self, operation: FilesOperation) -> FileReadPolicy {
        self.authorization
            .file_read_policy(operation.tool_name())
            .await
    }

    async fn open_file_for_read(
        &self,
        operation: FilesOperation,
        path: &str,
        managed_asset: Option<&crate::ManagedAsset>,
    ) -> anyhow::Result<tokio::fs::File> {
        let file = if let Some(asset) = managed_asset {
            self.managed_assets.open_for_read(asset)?
        } else {
            let policy = self.file_read_policy(operation).await;
            policy.open_read(Path::new(path))?
        };
        Ok(tokio::fs::File::from_std(file))
    }

    async fn open_path_for_metadata(
        &self,
        operation: FilesOperation,
        path: &str,
    ) -> anyhow::Result<tokio::fs::File> {
        let policy = self.file_read_policy(operation).await;
        let file = policy.open_metadata(Path::new(path))?;
        Ok(tokio::fs::File::from_std(file))
    }

    /// Dispatch all file capabilities after common path, managed-asset, and
    /// result-boundary handling.
    #[cfg(test)]
    async fn run(
        &self,
        params: FilesParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let context = crate::ToolExecutionContext {
            execution_workspace: Some(self.authorization.execution_workspace().await),
            file_write_policy: Some(crate::security::FileWritePolicy::ambient_for_test()),
            execution_authorization: Some(crate::security::ExecutionPolicyAuthorization::for_test()),
            ..Default::default()
        };
        self.run_with_execution_context(params, cancel, &context)
            .await
    }

    pub(super) async fn run_with_execution_context(
        &self,
        params: FilesParams,
        cancel: CancellationToken,
        context: &crate::ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        let op = params.operation.unwrap_or(FilesOperation::Read);
        let mutates_files = matches!(
            op,
            FilesOperation::Write
                | FilesOperation::CreateDir
                | FilesOperation::Patch
                | FilesOperation::Copy
                | FilesOperation::Move
                | FilesOperation::Delete
        );
        if mutates_files && context.file_write_policy.is_none() {
            anyhow::bail!("file mutation requires proof-derived directory authority");
        }
        let _mutation_permit = if mutates_files {
            Some(
                context
                    .acquire_execution_policy_permit()
                    .await
                    .map_err(crate::security::execution_policy_denied)?
                    .ok_or_else(|| {
                        crate::security::execution_policy_denied(
                            "file mutation requires a live proof-derived policy permit".into(),
                        )
                    })?,
            )
        } else {
            None
        };
        let mutation_context = file_mutations::MutationContext {
            cancel: &cancel,
            write_policy: context.file_write_policy.as_ref(),
        };
        let execution_workspace = match &context.execution_workspace {
            Some(workspace) => workspace.snapshot(),
            None => self.authorization.execution_workspace_snapshot().await,
        };
        let search_root = params
            .root
            .as_deref()
            .map(|path| resolve_workspace_path(&execution_workspace, path))
            .transpose()?;
        let requested_path = params
            .path
            .as_deref()
            .map(|path| resolve_workspace_path(&execution_workspace, path))
            .transpose()?;
        let mut managed_asset = if let Some(asset_id) = params.asset_id.as_deref() {
            crate::tool_contract::validate_entity_id(asset_id, "asset_id", "asset")?;
            if params.path.is_some() {
                anyhow::bail!("provide either asset_id or path, not both");
            }
            if !matches!(op, FilesOperation::Read | FilesOperation::Summary) {
                anyhow::bail!("asset_id is supported only for read and summary operations");
            }
            Some(
                self.managed_assets
                    .resolve(asset_id)
                    .ok_or_else(|| anyhow::anyhow!("managed asset is unavailable or expired"))?,
            )
        } else {
            None
        };
        let mut path = managed_asset
            .as_ref()
            .map(|asset| asset.path.to_string_lossy().into_owned())
            .map(Ok)
            .unwrap_or_else(|| {
                if op == FilesOperation::Search {
                    Ok(String::new())
                } else {
                    requested_path
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("path is required"))
                }
            })?;

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        if let Some(asset) = managed_asset.as_ref()
            && !self.managed_assets.revalidate(asset)
        {
            anyhow::bail!("managed asset changed or is no longer inside its managed root");
        }

        if managed_asset.is_none()
            && matches!(op, FilesOperation::Read | FilesOperation::Summary)
            && let Some(requested_path) = requested_path.as_deref()
        {
            let access_policy = self.file_read_policy(op).await;
            managed_asset = tokio::select! {
                biased;
                _ = cancel.cancelled() => anyhow::bail!("cancelled"),
                asset = register_rich_path_asset(
                    &self.managed_assets,
                    params.session_id.as_deref(),
                    requested_path,
                    &access_policy,
                ) => asset?,
            };
            if let Some(asset) = managed_asset.as_ref() {
                path = asset.path.to_string_lossy().into_owned();
            }
        }

        if matches!(op, FilesOperation::Read | FilesOperation::Summary)
            && let Some(asset) = managed_asset.as_ref()
            && let Some(operation) = media_operation_for(asset)
        {
            let media_tool = self
                .media_tool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("media runtime is not wired"))?;
            let operation_result = media_tool
                .run(
                    MediaParams {
                        operation,
                        asset_id: Some(asset.asset_id.clone()),
                        focus: params.focus.clone(),
                        prompt: None,
                        page_index: None,
                        file_path: None,
                        text: None,
                        duration: None,
                        volume: None,
                        muted: None,
                        session_id: params.session_id.clone(),
                    },
                    cancel,
                )
                .await;
            let mut result = match operation_result {
                Ok(result) => result,
                Err(_) => {
                    let mut output = media_tool.media_result_output(
                        operation,
                        Some(asset),
                        Some(MediaRepresentationKind::ManagedFileRef),
                        None,
                    );
                    output["available"] = serde_json::Value::Bool(false);
                    ToolResult::failed(output, "media operation failed")
                }
            };
            result = annotate_file_result(result, op, None, None);
            redact_managed_file_result(&mut result, asset);
            return Ok(result);
        }

        let operation_result: anyhow::Result<ToolResult> = match op {
            FilesOperation::Read => {
                let file = self
                    .open_file_for_read(op, &path, managed_asset.as_ref())
                    .await?;
                let has_line_args = params.start_line.is_some() || params.end_line.is_some();
                let has_byte_args = params.offset.is_some() || params.limit.is_some();
                if has_line_args {
                    let start_line = params.start_line.unwrap_or(1).max(1);
                    let end_line = params
                        .end_line
                        .unwrap_or(start_line + self.line_span.saturating_sub(1))
                        .max(start_line);
                    read_lines(
                        &path,
                        file,
                        start_line,
                        end_line,
                        self.max_output_chars,
                        self.max_line_bytes,
                    )
                    .await
                } else if has_byte_args {
                    let offset = params.offset.unwrap_or(0);
                    let limit = params.limit.unwrap_or(self.max_full_file_bytes);
                    read_bytes(
                        &path,
                        file,
                        offset,
                        limit,
                        self.max_output_chars,
                        self.max_byte_read_bytes,
                    )
                    .await
                } else {
                    read_full(
                        &path,
                        file,
                        self.max_output_chars,
                        self.max_full_file_bytes,
                        cancel.clone(),
                    )
                    .await
                }
            }
            FilesOperation::Inspect => match self.open_path_for_metadata(op, &path).await {
                Ok(file) => inspect_open_file(&path, file, self.max_byte_read_bytes).await,
                Err(error) if is_not_found(&error) => Ok(missing_file(&path)),
                Err(error) => Err(error),
            },
            FilesOperation::Outline => {
                let file = self.open_file_for_read(op, &path, None).await?;
                let start_line = params.start_line.unwrap_or(1).max(1);
                let max_symbols = params.max_symbols.unwrap_or(100).clamp(1, 500) as usize;
                file_outline::outline(
                    &path,
                    file,
                    start_line,
                    max_symbols,
                    self.max_line_bytes,
                    cancel.clone(),
                )
                .await
            }
            FilesOperation::Write => {
                file_mutations::write(
                    &path,
                    params.content.unwrap_or_default().as_str(),
                    params.expected_hash.as_deref(),
                    self.max_write_bytes,
                    params.dry_run.unwrap_or(false),
                    cancel.clone(),
                    context.file_write_policy.as_ref(),
                )
                .await
            }
            FilesOperation::CreateDir => file_mutations::create_dir(&path, mutation_context).await,
            FilesOperation::Patch => {
                file_mutations::patch(
                    &path,
                    file_mutations::PatchRequest {
                        edits: params.edits.as_deref(),
                        unified_diff: params.unified_diff.as_deref(),
                        expected_hash: params.expected_hash.as_deref(),
                        dry_run: params.dry_run.unwrap_or(false),
                    },
                    self.max_patch_file_bytes,
                    self.max_write_bytes,
                    mutation_context,
                )
                .await
            }
            FilesOperation::Copy => {
                let dest = resolve_workspace_path(
                    &execution_workspace,
                    &params.destination.unwrap_or_default(),
                )?;
                file_mutations::copy(&path, &dest, mutation_context).await
            }
            FilesOperation::Move => {
                let dest = resolve_workspace_path(
                    &execution_workspace,
                    &params.destination.unwrap_or_default(),
                )?;
                file_mutations::move_file(&path, &dest, mutation_context).await
            }
            FilesOperation::Delete => file_mutations::delete(&path, mutation_context).await,
            FilesOperation::List => {
                file_mutations::list(&path, self.max_list_entries, cancel.clone()).await
            }
            FilesOperation::Summary => {
                let file = self
                    .open_file_for_read(op, &path, managed_asset.as_ref())
                    .await?;
                let input_budget = params
                    .max_chars
                    .unwrap_or(self.summary_input_chars as u64)
                    .min(self.summary_input_chars as u64)
                    .max(1) as usize;
                summarize(
                    &path,
                    file,
                    params.start_line.unwrap_or(1).max(1),
                    params.end_line.unwrap_or(0),
                    params.focus.as_deref(),
                    input_budget,
                    self.max_line_bytes,
                    self.summarizer.clone(),
                    cancel.clone(),
                    self.summary_timeout_secs,
                )
                .await
            }
            FilesOperation::Search => {
                let policy = self.file_read_policy(op).await;
                let request = SearchRequest::new(
                    search_root.clone().unwrap_or_default(),
                    params.pattern.clone().unwrap_or_default(),
                    SearchOptions {
                        mode: Some(FileSearchMode::parse(params.mode.as_deref())?),
                        max_depth: params.max_depth,
                        max_results: params.max_results,
                        ignore_hidden: params.ignore_hidden,
                        max_file_size: params.max_file_size,
                        start_line: params.start_line,
                        end_line: params.end_line,
                    },
                    self.search.max_results_cap,
                    self.search.max_file_size,
                );
                self.search
                    .search_request_with_policy(request, cancel, policy)
                    .await
            }
        };
        let mut result = match operation_result {
            Ok(result) => result,
            Err(error) if managed_asset.is_some() => {
                tracing::debug!(
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "managed asset operation failed"
                );
                anyhow::bail!("managed asset operation failed")
            }
            Err(error) => return Err(error),
        };

        let result_path = if matches!(op, FilesOperation::Search) || managed_asset.is_some() {
            None
        } else {
            Some(path.as_str())
        };
        result = annotate_file_result(result, op, result_path, search_root.as_deref());
        if let Some(asset) = managed_asset.as_ref() {
            redact_managed_file_result(&mut result, asset);
        }
        Ok(result)
    }
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

#[cfg(test)]
use crate::{OperationIdempotency, Tool};
#[cfg(test)]
use async_trait::async_trait;
#[cfg(test)]
use file_classification::classify_file_by_extension;
#[cfg(test)]
use file_mutations::{apply_patch_edits, encode_patched_text};
#[cfg(test)]
use file_paths::{binary_result, looks_like_binary, sanitize_path, sha256_bytes};
#[cfg(test)]
use file_summary::{build_summary_messages, read_for_summary};
#[cfg(test)]
use haven_common::config::RequestKind;
#[cfg(test)]
use haven_common::types::RiskLevel;
#[cfg(test)]
use haven_common::types::{CanonicalMessage, ContentPart};
#[cfg(test)]
use serde_json::Value;
include!("files_tests.rs");
