mod contract;
mod platform;
mod screenshot;
mod ui_automation;
mod wait;

pub(crate) use ui_automation::{
    UIA_CONTROL_TYPE_NAMES, UiElementQuery, focus_ui_element, is_known_ui_control_type,
    resolve_ui_element,
};

use haven_common::config::default_generated_media_dir;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::media::MediaTool;
use crate::{ManagedAssetRegistry, ToolResult};

pub struct WindowTool {
    managed_assets: ManagedAssetRegistry,
    media_tool: Option<Arc<MediaTool>>,
    capture_root: PathBuf,
}

/// Window operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowOperation {
    List,
    Foreground,
    Focus,
    Close,
    Screenshot,
    Ocr,
    UiTree,
    Observe,
    Invoke,
    SetValue,
    Toggle,
    Select,
    Wait,
}

/// Wait condition for `operation=wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitCondition {
    TitleContains,
    ForegroundContains,
    UiText,
}

/// Typed parameters for `WindowTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `WindowTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WindowParams {
    /// Operation to perform; defaults to `list`.
    #[serde(default)]
    pub operation: Option<WindowOperation>,
    /// Window title to match (substring, used for focus/close/ui_tree).
    #[serde(default)]
    pub title: Option<String>,
    /// Stable HWND-derived identity returned by `window.list`/`foreground`.
    #[serde(default)]
    pub window_id: Option<String>,
    /// Filter windows by PID.
    #[serde(default)]
    pub pid: Option<i64>,
    /// Wait condition (`title_contains` / `foreground_contains` / `ui_text`).
    #[serde(default)]
    pub condition: Option<WaitCondition>,
    /// Text needle for wait conditions.
    #[serde(default)]
    pub text: Option<String>,
    /// Wait timeout in seconds (default 10, max 120).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// UI Automation element token returned by `ui_tree`/`observe`.
    #[serde(default)]
    pub element_token: Option<String>,
    /// UI Automation element name (fallback when no token is available).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub control_type: Option<String>,
    #[serde(default)]
    pub index: Option<usize>,
    /// Value for `set_value`; desired boolean for `toggle` is optional and
    /// currently means toggle once when omitted.
    #[serde(default)]
    pub value: Option<String>,
    /// Include OCR in `observe` when the dedicated OCR capability exists.
    #[serde(default)]
    pub ocr: Option<bool>,
    /// Private runtime context injected by `ToolsManager`; never part of the
    /// LLM-facing schema.
    #[serde(rename = "_session_id", default, skip_serializing)]
    pub(crate) session_id: Option<String>,
}

impl WindowTool {
    pub fn new(managed_assets: ManagedAssetRegistry) -> Self {
        Self {
            managed_assets,
            media_tool: None,
            capture_root: default_generated_media_dir(),
        }
    }

    pub(crate) fn with_media_tool(mut self, media_tool: Arc<MediaTool>) -> Self {
        self.media_tool = Some(media_tool);
        self
    }

    #[cfg(test)]
    fn with_capture_root(mut self, capture_root: PathBuf) -> Self {
        self.capture_root = capture_root;
        self
    }

    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: WindowParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let filter_pid = match params.pid {
            Some(pid) if pid > 0 => Some(pid as u32),
            Some(_) => anyhow::bail!("pid must be a positive integer"),
            None => None,
        };
        let title = params.title.clone();

        match params.operation.unwrap_or(WindowOperation::List) {
            WindowOperation::List => {
                let windows = platform::enumerate_windows(filter_pid)?;
                let count = windows.len();
                let max = 200usize;
                let truncated = count > max;
                let windows = if truncated {
                    windows.into_iter().take(max).collect::<Vec<_>>()
                } else {
                    windows
                };
                let mut result = serde_json::json!({"windows": windows, "count": count});
                if truncated {
                    result["truncated"] = serde_json::Value::Bool(true);
                    result["hint"] = serde_json::json!(format!(
                        "More than {} windows are open; only the first {} are listed. Filter by pid to narrow the result.",
                        max, max
                    ));
                }
                Ok(with_operation(
                    ToolResult::from_output(result, truncated),
                    "list",
                ))
            }
            WindowOperation::Foreground => {
                let fg = platform::get_foreground_window_info()?;
                Ok(with_operation(ToolResult::ok(fg), "foreground"))
            }
            WindowOperation::Focus => {
                let target = title.as_deref().filter(|t| !t.trim().is_empty());
                if params.window_id.is_none() && target.is_none() && filter_pid.is_none() {
                    anyhow::bail!("title or pid is required for focus");
                }
                platform::focus_window(params.window_id.as_deref(), target, filter_pid)?;
                Ok(window_target_result(
                    "focus",
                    params.window_id.as_deref(),
                    target,
                    filter_pid,
                ))
            }
            WindowOperation::Close => {
                let target = title.as_deref().filter(|t| !t.trim().is_empty());
                if params.window_id.is_none() && target.is_none() && filter_pid.is_none() {
                    anyhow::bail!("title or pid is required for close");
                }
                platform::close_window(params.window_id.as_deref(), target, filter_pid)?;
                Ok(window_target_result(
                    "close",
                    params.window_id.as_deref(),
                    target,
                    filter_pid,
                ))
            }
            WindowOperation::Screenshot => {
                let capture = self
                    .capture_screen(params.session_id.as_deref(), cancel)
                    .await?;
                let media_tool = self
                    .media_tool
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("media runtime is not wired"))?;
                let mut output = media_tool.media_result_output_named(
                    "screenshot",
                    Some(&capture.asset),
                    Some(haven_common::media::MediaRepresentationKind::ManagedFileRef),
                    None,
                );
                output["width"] = serde_json::json!(capture.width);
                output["height"] = serde_json::json!(capture.height);
                output["format"] = serde_json::json!(capture.format);
                Ok(ToolResult::ok(output))
            }
            WindowOperation::Ocr => self.ocr(params.session_id.as_deref(), cancel).await,
            WindowOperation::UiTree => {
                let title_owned = title;
                let window_id = params.window_id.clone();
                let elements = tokio::task::spawn_blocking(move || {
                    platform::enumerate_ui_tree(window_id.as_deref(), title_owned.as_deref())
                })
                .await??;
                let count = elements.len();
                let truncated = count >= ui_automation::UI_TREE_CAP;
                Ok(ToolResult::from_output(
                    serde_json::json!({
                        "operation": "ui_tree",
                        "elements": elements,
                        "count": count,
                        "truncated": truncated,
                    }),
                    truncated,
                ))
            }
            WindowOperation::Observe => self.observe(params, cancel).await,
            WindowOperation::Invoke
            | WindowOperation::SetValue
            | WindowOperation::Toggle
            | WindowOperation::Select => {
                let operation = params.operation.expect("matched semantic operation");
                let query = ui_automation::window_element_query(&params)?;
                let value = params.value.clone();
                let target = tokio::task::spawn_blocking(move || match operation {
                    WindowOperation::Invoke => ui_automation::invoke_ui_element(&query),
                    WindowOperation::SetValue => ui_automation::set_ui_element_value(
                        &query,
                        value
                            .as_deref()
                            .ok_or_else(|| anyhow::anyhow!("value is required for set_value"))?,
                    ),
                    WindowOperation::Toggle => ui_automation::toggle_ui_element(&query),
                    WindowOperation::Select => ui_automation::select_ui_element(&query),
                    _ => unreachable!(),
                })
                .await??;
                Ok(ToolResult::ok(serde_json::json!({
                    "operation": operation,
                    "window_id": target.window_id,
                    "element_token": target.element_token,
                    "name": target.name,
                    "control_type": target.control_type,
                    "index": target.index,
                    "changed": true,
                })))
            }
            WindowOperation::Wait => self.wait(params, cancel).await,
        }
    }
}

fn window_target_result(
    operation: &str,
    window_id: Option<&str>,
    title: Option<&str>,
    pid: Option<u32>,
) -> ToolResult {
    let mut output = serde_json::json!({"operation": operation});
    if let Some(window_id) = window_id {
        output["window_id"] = serde_json::json!(window_id);
    }
    if let Some(title) = title {
        output[if operation == "focus" {
            "focused"
        } else {
            "closed"
        }] = serde_json::json!(title);
    }
    if let Some(pid) = pid {
        output["pid"] = serde_json::json!(pid);
    }
    ToolResult::ok(output)
}

fn with_operation(mut result: ToolResult, operation: &str) -> ToolResult {
    if let Some(object) = result.output.as_object_mut() {
        object.insert("operation".into(), Value::String(operation.into()));
    }
    result
}

#[cfg(test)]
mod tests;
