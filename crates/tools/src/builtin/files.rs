use haven_common::media::MediaRepresentationKind;
use haven_llm::LlmRouter;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::file_outline;
use super::file_search::{FileSearchEngine, SearchOptions, SearchRequest};
use super::media::{MediaParams, MediaTool};
use crate::{ManagedAssetRegistry, ToolResult};

use file_media_handoff::{media_operation_for, register_rich_path_asset};
use file_paths::{
    annotate_file_result, inspect_file, redact_managed_file_result, resolve_workspace_path,
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
const UNTRUSTED_DOCUMENT_START: &str = "【附件派生内容开始";
const UNTRUSTED_DOCUMENT_END: &str = "【附件派生内容结束】";
const MAX_PATCH_EDITS: usize = 64;
const MAX_PATCH_INPUT_BYTES: usize = 256 * 1024;
const MAX_INSPECT_HASH_BYTES: u64 = 64 * 1024 * 1024;

pub struct FilesTool {
    summarizer: Option<Arc<LlmRouter>>,
    max_output_chars: usize,
    max_read_chars: u64,
    line_span: u64,
    max_line_chars: usize,
    summary_input_chars: usize,
    max_list_entries: usize,
    max_byte_read: u64,
    max_write_bytes: u64,
    summary_timeout_secs: u64,
    search: FileSearchEngine,
    managed_assets: ManagedAssetRegistry,
    media_tool: Option<Arc<MediaTool>>,
}

impl Default for FilesTool {
    fn default() -> Self {
        Self {
            summarizer: None,
            max_output_chars: 20_000,
            max_read_chars: 128_000,
            line_span: 100,
            max_line_chars: 128_000,
            summary_input_chars: 60_000,
            max_list_entries: 1_000,
            max_byte_read: 16 * 1024 * 1024,
            max_write_bytes: 16 * 1024 * 1024,
            summary_timeout_secs: 120,
            search: FileSearchEngine::default(),
            managed_assets: ManagedAssetRegistry::default(),
            media_tool: None,
        }
    }
}

/// Files operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesOperation {
    Read,
    Inspect,
    Stat,
    Hash,
    Write,
    CreateDir,
    Edit,
    Patch,
    Copy,
    Move,
    Delete,
    List,
    Summary,
    Search,
    Outline,
}

/// One exact replacement in a files.patch request.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct FilesPatchEdit {
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub expected_matches: Option<usize>,
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
    pub old_string: Option<String>,
    #[serde(default)]
    pub new_string: Option<String>,
    #[serde(default)]
    pub edits: Option<Vec<FilesPatchEdit>>,
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
        max_read_chars: u64,
        line_span: u64,
        max_line_chars: usize,
        summary_input_chars: usize,
        max_list_entries: usize,
        max_byte_read: u64,
        summary_timeout_secs: u64,
        search: FileSearchEngine,
        managed_assets: ManagedAssetRegistry,
    ) -> Self {
        Self {
            summarizer,
            max_output_chars,
            max_read_chars,
            line_span,
            max_line_chars,
            summary_input_chars,
            max_list_entries,
            max_byte_read,
            max_write_bytes: max_byte_read,
            summary_timeout_secs,
            search,
            managed_assets,
            media_tool: None,
        }
    }

    pub(crate) fn with_media_tool(mut self, media_tool: Arc<MediaTool>) -> Self {
        self.media_tool = Some(media_tool);
        self
    }

    pub(crate) fn with_max_write_bytes(mut self, max_write_bytes: u64) -> Self {
        self.max_write_bytes = max_write_bytes.max(1);
        self
    }

    /// Dispatch all file capabilities after common path, managed-asset, and
    /// result-boundary handling.
    pub async fn run(
        &self,
        params: FilesParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let op = params.operation.unwrap_or(FilesOperation::Read);
        let search_root = params
            .root
            .as_deref()
            .map(resolve_workspace_path)
            .transpose()?;
        let requested_path = params
            .path
            .as_deref()
            .map(resolve_workspace_path)
            .transpose()?;
        let mut managed_asset = if let Some(asset_id) = params.asset_id.as_deref() {
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
            managed_asset = register_rich_path_asset(
                &self.managed_assets,
                params.session_id.as_deref(),
                requested_path,
            )
            .await?;
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
                Err(error) => {
                    let mut output = media_tool.media_result_output(
                        operation,
                        Some(asset),
                        Some(MediaRepresentationKind::ManagedFileRef),
                        None,
                    );
                    output["available"] = serde_json::Value::Bool(false);
                    ToolResult::failed(output, format!("media operation failed: {error}"))
                }
            };
            result = annotate_file_result(result, op, None, None);
            redact_managed_file_result(&mut result, asset);
            return Ok(result);
        }

        let operation_result: anyhow::Result<ToolResult> = match op {
            FilesOperation::Read => {
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
                        start_line,
                        end_line,
                        self.max_output_chars,
                        self.max_line_chars,
                    )
                    .await
                } else if has_byte_args {
                    let offset = params.offset.unwrap_or(0);
                    let limit = params.limit.unwrap_or(self.max_read_chars);
                    read_bytes(
                        &path,
                        offset,
                        limit,
                        self.max_output_chars,
                        self.max_byte_read,
                    )
                    .await
                } else {
                    read_full(
                        &path,
                        self.max_output_chars,
                        self.max_read_chars,
                        cancel.clone(),
                    )
                    .await
                }
            }
            FilesOperation::Inspect => {
                inspect_file(Path::new(&path), true, self.max_byte_read).await
            }
            FilesOperation::Stat => inspect_file(Path::new(&path), false, self.max_byte_read).await,
            FilesOperation::Hash => {
                let result = inspect_file(Path::new(&path), true, self.max_byte_read).await?;
                let mut output = result.output;
                if let Some(object) = output.as_object_mut() {
                    object.retain(|key, _| {
                        matches!(
                            key.as_str(),
                            "exists"
                                | "file_type"
                                | "path"
                                | "size"
                                | "mtime"
                                | "hash"
                                | "encoding"
                        )
                    });
                }
                Ok(ToolResult::ok(output))
            }
            FilesOperation::Outline => {
                let start_line = params.start_line.unwrap_or(1).max(1);
                let max_symbols = params.max_symbols.unwrap_or(100).clamp(1, 500) as usize;
                file_outline::outline(
                    &path,
                    start_line,
                    max_symbols,
                    self.max_line_chars,
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
                )
                .await
            }
            FilesOperation::CreateDir => file_mutations::create_dir(&path, cancel.clone()).await,
            FilesOperation::Edit => {
                file_mutations::edit(
                    &path,
                    params.old_string.as_deref(),
                    params.new_string.unwrap_or_default().as_str(),
                    params.expected_hash.as_deref(),
                    file_mutations::MutationLimits {
                        max_read_bytes: self.max_read_chars,
                        max_write_bytes: self.max_write_bytes,
                    },
                    params.dry_run.unwrap_or(false),
                    cancel.clone(),
                )
                .await
            }
            FilesOperation::Patch => {
                file_mutations::patch(
                    &path,
                    params.edits.as_deref(),
                    params.expected_hash.as_deref(),
                    self.max_read_chars,
                    self.max_write_bytes,
                    params.dry_run.unwrap_or(false),
                    cancel.clone(),
                )
                .await
            }
            FilesOperation::Copy => {
                let dest = resolve_workspace_path(&params.destination.unwrap_or_default())?;
                file_mutations::copy(&path, &dest, cancel.clone()).await
            }
            FilesOperation::Move => {
                let dest = resolve_workspace_path(&params.destination.unwrap_or_default())?;
                file_mutations::move_file(&path, &dest, cancel.clone()).await
            }
            FilesOperation::Delete => file_mutations::delete(&path, cancel.clone()).await,
            FilesOperation::List => {
                file_mutations::list(&path, self.max_list_entries, cancel.clone()).await
            }
            FilesOperation::Summary => {
                let input_budget = params
                    .max_chars
                    .unwrap_or(self.summary_input_chars as u64)
                    .min(self.summary_input_chars as u64)
                    .max(1) as usize;
                summarize(
                    &path,
                    params.start_line.unwrap_or(1).max(1),
                    params.end_line.unwrap_or(0),
                    params.focus.as_deref(),
                    input_budget,
                    self.max_line_chars,
                    self.summarizer.clone(),
                    cancel.clone(),
                    self.summary_timeout_secs,
                )
                .await
            }
            FilesOperation::Search => {
                let request = SearchRequest::new(
                    search_root.clone().unwrap_or_default(),
                    params.pattern.clone().unwrap_or_default(),
                    SearchOptions {
                        mode: params.mode.clone(),
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
                self.search.search_request(request, cancel).await
            }
        };
        let mut result = match operation_result {
            Ok(result) => result,
            Err(error) if managed_asset.is_some() => {
                tracing::debug!(error = %error, "managed asset operation failed");
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

#[cfg(test)]
use crate::{OperationIdempotency, Tool};
#[cfg(test)]
use async_trait::async_trait;
#[cfg(test)]
use file_classification::classify_by_extension;
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
