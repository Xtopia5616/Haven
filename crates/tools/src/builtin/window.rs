use async_trait::async_trait;
use base64::Engine;
use haven_common::prompts::OCR_SYSTEM_PROMPT;
use haven_common::types::RiskLevel;
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_llm::LlmRouter;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolConcurrency, ToolResult};

/// Default vision byte / timeout limits (aligned with FilesTool defaults).
const DEFAULT_VISION_MAX_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_VISION_TIMEOUT_SECS: u64 = 60;
const DEFAULT_WAIT_SECS: u64 = 10;
const MAX_WAIT_SECS: u64 = 120;
const UI_TREE_CAP: usize = 100;
const WAIT_POLL_MS: u64 = 200;
const WAIT_UI_POLL_MS: u64 = 500;

pub struct WindowTool {
    router: Option<Arc<LlmRouter>>,
    vision_max_bytes: u64,
    vision_timeout_secs: u64,
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
    /// Filter windows by PID.
    #[serde(default)]
    pub pid: Option<i64>,
    /// Optional output path for screenshot; defaults to a temp file.
    #[serde(default)]
    pub path: Option<String>,
    /// Wait condition (`title_contains` / `foreground_contains` / `ui_text`).
    #[serde(default)]
    pub condition: Option<WaitCondition>,
    /// Text needle for wait conditions.
    #[serde(default)]
    pub text: Option<String>,
    /// Wait timeout in seconds (default 10, max 120).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl WindowTool {
    pub fn new(router: Option<Arc<LlmRouter>>) -> Self {
        Self {
            router,
            vision_max_bytes: DEFAULT_VISION_MAX_BYTES,
            vision_timeout_secs: DEFAULT_VISION_TIMEOUT_SECS,
        }
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
                let windows = imp::enumerate_windows(filter_pid)?;
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
                Ok(with_operation(ToolResult::ok(result), "list"))
            }
            WindowOperation::Foreground => {
                let fg = imp::get_foreground_window_info()?;
                Ok(with_operation(ToolResult::ok(fg), "foreground"))
            }
            WindowOperation::Focus => {
                let target = title.as_deref().filter(|t| !t.trim().is_empty());
                if target.is_none() && filter_pid.is_none() {
                    anyhow::bail!("title or pid is required for focus");
                }
                imp::focus_window(target, filter_pid)?;
                Ok(window_target_result("focus", target, filter_pid))
            }
            WindowOperation::Close => {
                let target = title.as_deref().filter(|t| !t.trim().is_empty());
                if target.is_none() && filter_pid.is_none() {
                    anyhow::bail!("title or pid is required for close");
                }
                imp::close_window(target, filter_pid)?;
                Ok(window_target_result("close", target, filter_pid))
            }
            WindowOperation::Screenshot => {
                let path = params
                    .path
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| std::path::PathBuf::from(p.trim()));
                let shot = imp::capture_screen(path)?;
                Ok(with_operation(ToolResult::ok(shot), "screenshot"))
            }
            WindowOperation::Ocr => self.ocr(params.path, cancel).await,
            WindowOperation::UiTree => {
                let title_owned = title;
                let elements = tokio::task::spawn_blocking(move || {
                    imp::enumerate_ui_tree(title_owned.as_deref())
                })
                .await??;
                let count = elements.len();
                let truncated = count >= UI_TREE_CAP;
                Ok(ToolResult::ok(serde_json::json!({
                    "operation": "ui_tree",
                    "elements": elements,
                    "count": count,
                    "truncated": truncated,
                })))
            }
            WindowOperation::Wait => self.wait(params, cancel).await,
        }
    }

    async fn ocr(
        &self,
        path: Option<String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        // Capturing the desktop is a high-risk operation in its own right.
        // When OCR cannot run because no vision router is configured, do not
        // capture a screenshot only to discard it. This also keeps the
        // unavailable-capability path independent of an interactive desktop.
        let Some(client) = &self.router else {
            return Ok(ToolResult::ok(serde_json::json!({
                "operation": "ocr",
                "ocr": true,
                "ocr_unavailable": true,
                "reason": "No LLM router installed, so OCR cannot run."
            })));
        };
        let path_buf = path
            .filter(|p| !p.trim().is_empty())
            .map(|p| std::path::PathBuf::from(p.trim()));
        let shot = tokio::task::spawn_blocking(move || imp::capture_screen(path_buf)).await??;
        let shot_path = shot["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("screenshot path missing"))?
            .to_string();

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let meta = tokio::fs::metadata(&shot_path).await?;
        let size = meta.len();
        if size > self.vision_max_bytes {
            return Ok(ToolResult::ok(serde_json::json!({
                "operation": "ocr",
                "ocr": true,
                "path": shot_path,
                "size": size,
                "too_large": true,
                "hint": format!(
                    "Screenshot is {} bytes, above the {} byte vision limit.",
                    size, self.vision_max_bytes
                ),
            })));
        }

        let bytes = tokio::fs::read(&shot_path).await?;
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let role = client.vision_role().await;

        let messages = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text(OCR_SYSTEM_PROMPT)],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::Image {
                    content_type: "image_url".into(),
                    media_type: "image/png".into(),
                    data,
                }],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
            },
        ];

        let timeout = self.vision_timeout_secs;
        let response =
            match tokio::time::timeout(Duration::from_secs(timeout), client.chat(role, messages))
                .await
            {
                Ok(Ok(resp)) => resp,
                Ok(Err(e)) => {
                    return Ok(ToolResult {
                        success: false,
                        output: serde_json::json!({
                            "operation": "ocr",
                            "ocr": true,
                            "path": shot_path,
                            "ocr_error": true,
                        }),
                        error: Some(format!("OCR vision call failed: {e}")),
                        truncated: false,
                        outcome: crate::ToolExecutionOutcome::Failed,
                        attempts: 1,
                        signals: crate::tool_contract::ToolSignals::default(),
                    });
                }
                Err(_) => {
                    return Ok(ToolResult {
                        success: false,
                        output: serde_json::json!({
                            "operation": "ocr",
                            "ocr": true,
                            "path": shot_path,
                            "ocr_error": true,
                        }),
                        error: Some(format!("OCR vision call timed out after {timeout}s")),
                        truncated: false,
                        outcome: crate::ToolExecutionOutcome::TimedOutUnknown,
                        attempts: 1,
                        signals: crate::tool_contract::ToolSignals::default(),
                    });
                }
            };

        Ok(ToolResult::ok(serde_json::json!({
            "operation": "ocr",
            "ocr": true,
            "path": shot_path,
            "size": size,
            "text": response.text.trim().to_string(),
            "model": response.model,
            "width": shot["width"],
            "height": shot["height"],
        })))
    }

    async fn wait(
        &self,
        params: WindowParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let condition = params.condition.ok_or_else(|| {
            anyhow::anyhow!(
                "condition is required for wait (title_contains | foreground_contains | ui_text)"
            )
        })?;
        let text = params
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| anyhow::anyhow!("text is required for wait"))?
            .to_string();
        let timeout = params
            .timeout_secs
            .unwrap_or(DEFAULT_WAIT_SECS)
            .clamp(1, MAX_WAIT_SECS);
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let title_filter = params.title.clone();
        let poll_ms = match condition {
            WaitCondition::UiText => WAIT_UI_POLL_MS,
            _ => WAIT_POLL_MS,
        };
        // One blocking wait session: reuse COM/UIA setup cost across polls.
        let cancel_flag = cancel.clone();
        tokio::task::spawn_blocking(move || {
            loop {
                if cancel_flag.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let matched = match condition {
                    WaitCondition::TitleContains => imp::any_title_contains(&text)?,
                    WaitCondition::ForegroundContains => imp::foreground_title_contains(&text)?,
                    WaitCondition::UiText => {
                        imp::any_ui_name_contains(title_filter.as_deref(), &text)?
                    }
                };
                if matched {
                    return Ok(ToolResult::ok(serde_json::json!({
                        "operation": "wait",
                        "waited": true,
                        "timed_out": false,
                        "matched": true,
                        "condition": condition,
                        "text": text,
                    })));
                }
                if Instant::now() >= deadline {
                    return Ok(ToolResult::ok(serde_json::json!({
                        "operation": "wait",
                        "waited": true,
                        "timed_out": true,
                        "matched": false,
                        "condition": condition,
                        "text": text,
                        "timeout_secs": timeout,
                    })));
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
            }
        })
        .await?
    }
}

fn window_target_result(operation: &str, title: Option<&str>, pid: Option<u32>) -> ToolResult {
    let mut output = serde_json::json!({"operation": operation});
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

#[async_trait]
impl Tool for WindowTool {
    fn name(&self) -> String {
        "window".into()
    }
    fn description(&self) -> String {
        "List, query, and manage desktop windows: list/foreground/focus/close/screenshot; \
         `ocr` captures the screen and extracts text via vision; `ui_tree` enumerates \
         interactive UI Automation elements; `wait` polls until a title/UI text condition \
         matches. For monitor layout use system scope=display."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("close") => RiskLevel::High,
            // OCR uploads a full-screen capture to the vision model.
            Some("ocr") => RiskLevel::High,
            Some("focus") => RiskLevel::Medium,
            Some("ui_tree") | Some("wait") => RiskLevel::Low,
            _ => RiskLevel::Low,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        match input["operation"].as_str() {
            Some("list") | Some("foreground") | Some("ui_tree") | Some("wait") => {
                ToolConcurrency::SharedResource("desktop".into())
            }
            _ => ToolConcurrency::Resource("desktop".into()),
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["list", "foreground", "focus", "close", "screenshot", "ocr", "ui_tree", "wait"] },
                "title": { "type": "string" },
                "pid": { "type": "integer", "minimum": 1 },
                "path": { "type": "string", "minLength": 1 },
                "condition": { "type": "string", "enum": ["title_contains", "foreground_contains", "ui_text"] },
                "text": { "type": "string", "minLength": 1 },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120 }
            },
            "required": ["operation"],
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "list" },
                        "pid": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "foreground" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "focus" }, "title": { "type": "string", "minLength": 1 } },
                    "required": ["operation", "title"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "focus" }, "pid": { "type": "integer", "minimum": 1 } },
                    "required": ["operation", "pid"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "close" }, "title": { "type": "string", "minLength": 1 } },
                    "required": ["operation", "title"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "close" }, "pid": { "type": "integer", "minimum": 1 } },
                    "required": ["operation", "pid"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "screenshot" }, "path": { "type": "string", "minLength": 1 } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "ocr" }, "path": { "type": "string", "minLength": 1 } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "ui_tree" }, "title": { "type": "string", "minLength": 1 } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "wait" },
                        "condition": { "type": "string", "enum": ["title_contains", "foreground_contains", "ui_text"] },
                        "text": { "type": "string", "minLength": 1 },
                        "title": { "type": "string", "minLength": 1 },
                        "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120 }
                    },
                    "required": ["operation", "condition", "text"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `WindowParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<WindowParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(windows)]
mod imp {
    use serde_json::Value;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{FALSE, HWND, LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    use windows_sys::core::BOOL;

    /// Read a window's visible title text, or None if the window is not
    /// visible or has no title. Shared by window enumeration and search.
    /// Callers must be inside an `unsafe` context.
    fn visible_window_title(hwnd: HWND) -> Option<String> {
        unsafe {
            if IsWindowVisible(hwnd) == FALSE {
                return None;
            }
            let mut title_buf = [0u16; 512];
            let len = GetWindowTextW(hwnd, title_buf.as_mut_ptr(), 512);
            if len == 0 {
                return None;
            }
            Some(
                OsString::from_wide(&title_buf[..len as usize])
                    .to_string_lossy()
                    .to_string(),
            )
        }
    }

    pub fn enumerate_windows(filter_pid: Option<u32>) -> anyhow::Result<Vec<Value>> {
        let mut windows: Vec<Value> = Vec::new();

        unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                let windows = &mut *(lparam as *mut Vec<Value>);

                let Some(title) = visible_window_title(hwnd) else {
                    return TRUE;
                };

                let mut pid: u32 = 0;
                GetWindowThreadProcessId(hwnd, &mut pid);

                windows.push(serde_json::json!({
                    "hwnd": hwnd as usize,
                    "title": title,
                    "pid": pid,
                }));

                TRUE
            }
        }

        unsafe {
            EnumWindows(Some(enum_callback), &mut windows as *mut _ as LPARAM);

            if let Some(pid) = filter_pid
                && pid != 0
            {
                windows.retain(|w| w["pid"].as_u64() == Some(pid as u64));
            }

            windows.retain(|w| !w["title"].as_str().unwrap_or("").is_empty());
        }

        Ok(windows)
    }

    pub fn get_foreground_window_info() -> anyhow::Result<Value> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return Ok(serde_json::json!({"hwnd": 0, "title": "", "pid": 0}));
            }
            let mut title_buf = [0u16; 512];
            let len = GetWindowTextW(hwnd, title_buf.as_mut_ptr(), 512);
            let title = if len > 0 {
                OsString::from_wide(&title_buf[..len as usize])
                    .to_string_lossy()
                    .to_string()
            } else {
                String::new()
            };
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            Ok(serde_json::json!({
                "hwnd": hwnd as usize,
                "title": title,
                "pid": pid,
            }))
        }
    }

    pub fn focus_window(title: Option<&str>, pid: Option<u32>) -> anyhow::Result<()> {
        let hwnd = find_window(title, pid)?.ok_or_else(|| window_not_found(title, pid))?;
        unsafe {
            SetForegroundWindow(hwnd);
        }
        Ok(())
    }

    pub fn close_window(title: Option<&str>, pid: Option<u32>) -> anyhow::Result<()> {
        let hwnd = find_window(title, pid)?.ok_or_else(|| window_not_found(title, pid))?;
        unsafe {
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
        }
        Ok(())
    }

    fn find_window(title: Option<&str>, pid: Option<u32>) -> anyhow::Result<Option<HWND>> {
        let windows = enumerate_windows(pid)?;
        let window = windows.iter().find(|window| {
            title
                .map(|needle| {
                    window["title"]
                        .as_str()
                        .map(|value| value.contains(needle))
                        .unwrap_or(false)
                })
                .unwrap_or(true)
        });
        Ok(window
            .and_then(|window| window["hwnd"].as_u64())
            .map(|hwnd| hwnd as usize as HWND))
    }

    fn window_not_found(title: Option<&str>, pid: Option<u32>) -> anyhow::Error {
        match (title, pid) {
            (Some(title), Some(pid)) => {
                anyhow::anyhow!("no window found matching title '{}' for pid {}", title, pid)
            }
            (Some(title), None) => anyhow::anyhow!("no window found matching '{}'", title),
            (None, Some(pid)) => anyhow::anyhow!("no window found for pid {}", pid),
            (None, None) => anyhow::anyhow!("a title or pid is required"),
        }
    }

    fn find_window_by_title(title: &str) -> anyhow::Result<HWND> {
        find_window(Some(title), None)?.ok_or_else(|| window_not_found(Some(title), None))
    }

    pub fn any_title_contains(needle: &str) -> anyhow::Result<bool> {
        let windows = enumerate_windows(None)?;
        Ok(windows.iter().any(|w| {
            w["title"]
                .as_str()
                .map(|t| t.contains(needle))
                .unwrap_or(false)
        }))
    }

    pub fn foreground_title_contains(needle: &str) -> anyhow::Result<bool> {
        let fg = get_foreground_window_info()?;
        Ok(fg["title"]
            .as_str()
            .map(|t| t.contains(needle))
            .unwrap_or(false))
    }

    /// Capture the primary screen and save it as a PNG. `path` defaults to a
    /// fresh file in the system temp directory. The pixel buffer is copied
    /// out of the GDI device context before it is released, then encoded
    /// with the `image` crate — the capture itself never touches the file.
    pub fn capture_screen(path: Option<std::path::PathBuf>) -> anyhow::Result<Value> {
        use windows_sys::Win32::Foundation::GetLastError;
        use windows_sys::Win32::Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap,
            CreateCompatibleDC, CreateDCW, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
            GetDIBits, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
        };

        unsafe {
            let width = GetSystemMetrics(SM_CXSCREEN);
            let height = GetSystemMetrics(SM_CYSCREEN);
            if width <= 0 || height <= 0 {
                anyhow::bail!("failed to query screen size ({width}x{height})");
            }

            // Primary-screen DC. `CreateDCW("DISPLAY")` is the classic
            // full-screen DC, but some environments (no interactive desktop
            // access, disconnected RDP session) reject it. `GetDC(NULL)`
            // retrieves the screen DC directly and is more lenient, so fall
            // back to it. The two are released differently (DeleteDC vs
            // ReleaseDC), tracked by `screen_dc_is_getdc`.
            let (screen_dc, screen_dc_is_getdc) = {
                let dc = CreateDCW(
                    std::ptr::null(),
                    "DISPLAY"
                        .encode_utf16()
                        .chain(std::iter::once(0))
                        .collect::<Vec<u16>>()
                        .as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                );
                if !dc.is_null() {
                    (dc, false)
                } else {
                    let dc = GetDC(std::ptr::null_mut());
                    if dc.is_null() {
                        anyhow::bail!("failed to create screen DC (GDI error {})", GetLastError());
                    }
                    (dc, true)
                }
            };
            let mem_dc = CreateCompatibleDC(screen_dc);
            if mem_dc.is_null() {
                if screen_dc_is_getdc {
                    ReleaseDC(std::ptr::null_mut(), screen_dc);
                } else {
                    DeleteDC(screen_dc);
                }
                anyhow::bail!("failed to create memory DC (GDI error {})", GetLastError());
            }
            let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
            if bitmap.is_null() {
                DeleteDC(mem_dc);
                if screen_dc_is_getdc {
                    ReleaseDC(std::ptr::null_mut(), screen_dc);
                } else {
                    DeleteDC(screen_dc);
                }
                anyhow::bail!(
                    "failed to create compatible bitmap (GDI error {})",
                    GetLastError()
                );
            }
            let old_obj = SelectObject(mem_dc, bitmap as HGDIOBJ);
            let ok = BitBlt(mem_dc, 0, 0, width, height, screen_dc, 0, 0, SRCCOPY);

            // Read the pixel data out before releasing the DCs.
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = width;
            bmi.bmiHeader.biHeight = -height; // top-down rows
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
            let copied = GetDIBits(
                mem_dc,
                bitmap,
                0,
                height as u32,
                pixels.as_mut_ptr() as *mut _,
                &mut bmi,
                DIB_RGB_COLORS,
            );

            if !old_obj.is_null() {
                SelectObject(mem_dc, old_obj);
            }
            DeleteObject(bitmap as _);
            DeleteDC(mem_dc);
            if screen_dc_is_getdc {
                ReleaseDC(std::ptr::null_mut(), screen_dc);
            } else {
                DeleteDC(screen_dc);
            }

            if ok == 0 {
                anyhow::bail!("BitBlt failed (GDI error {})", GetLastError());
            }
            if copied == 0 {
                anyhow::bail!("GetDIBits failed (GDI error {})", GetLastError());
            }

            // BGRA (GDI) -> RGBA for the image crate.
            let mut rgba = pixels.clone();
            for px in rgba.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
            let img = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
                .ok_or_else(|| anyhow::anyhow!("invalid screenshot buffer"))?;

            let path = match path {
                Some(p) => p,
                None => std::env::temp_dir().join(format!(
                    "haven-screenshot-{}.png",
                    uuid::Uuid::new_v4().simple()
                )),
            };
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            img.save(&path)?;

            Ok(serde_json::json!({
                "path": path.to_string_lossy().to_string(),
                "width": width,
                "height": height,
                "format": "png",
                "hint": "Open the image with the files tool (read) to view it.",
            }))
        }
    }

    fn control_type_name(id: i32) -> &'static str {
        use windows::Win32::UI::Accessibility::*;
        // Match against known UIA control type ids.
        if id == UIA_ButtonControlTypeId.0 {
            "Button"
        } else if id == UIA_CalendarControlTypeId.0 {
            "Calendar"
        } else if id == UIA_CheckBoxControlTypeId.0 {
            "CheckBox"
        } else if id == UIA_ComboBoxControlTypeId.0 {
            "ComboBox"
        } else if id == UIA_EditControlTypeId.0 {
            "Edit"
        } else if id == UIA_HyperlinkControlTypeId.0 {
            "Hyperlink"
        } else if id == UIA_ImageControlTypeId.0 {
            "Image"
        } else if id == UIA_ListItemControlTypeId.0 {
            "ListItem"
        } else if id == UIA_ListControlTypeId.0 {
            "List"
        } else if id == UIA_MenuControlTypeId.0 {
            "Menu"
        } else if id == UIA_MenuBarControlTypeId.0 {
            "MenuBar"
        } else if id == UIA_MenuItemControlTypeId.0 {
            "MenuItem"
        } else if id == UIA_ProgressBarControlTypeId.0 {
            "ProgressBar"
        } else if id == UIA_RadioButtonControlTypeId.0 {
            "RadioButton"
        } else if id == UIA_ScrollBarControlTypeId.0 {
            "ScrollBar"
        } else if id == UIA_SliderControlTypeId.0 {
            "Slider"
        } else if id == UIA_SpinnerControlTypeId.0 {
            "Spinner"
        } else if id == UIA_StatusBarControlTypeId.0 {
            "StatusBar"
        } else if id == UIA_TabControlTypeId.0 {
            "Tab"
        } else if id == UIA_TabItemControlTypeId.0 {
            "TabItem"
        } else if id == UIA_TextControlTypeId.0 {
            "Text"
        } else if id == UIA_ToolBarControlTypeId.0 {
            "ToolBar"
        } else if id == UIA_ToolTipControlTypeId.0 {
            "ToolTip"
        } else if id == UIA_TreeControlTypeId.0 {
            "Tree"
        } else if id == UIA_TreeItemControlTypeId.0 {
            "TreeItem"
        } else if id == UIA_CustomControlTypeId.0 {
            "Custom"
        } else if id == UIA_GroupControlTypeId.0 {
            "Group"
        } else if id == UIA_ThumbControlTypeId.0 {
            "Thumb"
        } else if id == UIA_DataGridControlTypeId.0 {
            "DataGrid"
        } else if id == UIA_DataItemControlTypeId.0 {
            "DataItem"
        } else if id == UIA_DocumentControlTypeId.0 {
            "Document"
        } else if id == UIA_SplitButtonControlTypeId.0 {
            "SplitButton"
        } else if id == UIA_WindowControlTypeId.0 {
            "Window"
        } else if id == UIA_PaneControlTypeId.0 {
            "Pane"
        } else if id == UIA_HeaderControlTypeId.0 {
            "Header"
        } else if id == UIA_HeaderItemControlTypeId.0 {
            "HeaderItem"
        } else if id == UIA_TableControlTypeId.0 {
            "Table"
        } else if id == UIA_TitleBarControlTypeId.0 {
            "TitleBar"
        } else if id == UIA_SeparatorControlTypeId.0 {
            "Separator"
        } else {
            "Unknown"
        }
    }

    fn is_interactive_control(id: i32) -> bool {
        use windows::Win32::UI::Accessibility::*;
        [
            UIA_ButtonControlTypeId.0,
            UIA_CheckBoxControlTypeId.0,
            UIA_ComboBoxControlTypeId.0,
            UIA_EditControlTypeId.0,
            UIA_HyperlinkControlTypeId.0,
            UIA_ListItemControlTypeId.0,
            UIA_MenuItemControlTypeId.0,
            UIA_RadioButtonControlTypeId.0,
            UIA_SliderControlTypeId.0,
            UIA_SpinnerControlTypeId.0,
            UIA_SplitButtonControlTypeId.0,
            UIA_TabItemControlTypeId.0,
            UIA_TreeItemControlTypeId.0,
            UIA_DataItemControlTypeId.0,
            UIA_ScrollBarControlTypeId.0,
            UIA_ThumbControlTypeId.0,
            UIA_CalendarControlTypeId.0,
            UIA_DocumentControlTypeId.0,
        ]
        .contains(&id)
    }

    /// Enumerate interactive UI Automation elements for the foreground window
    /// (or the first window whose title contains `title`).
    pub fn enumerate_ui_tree(title: Option<&str>) -> anyhow::Result<Vec<Value>> {
        use windows::Win32::Foundation::HWND as WinHwnd;
        use windows::Win32::System::Com::*;
        use windows::Win32::UI::Accessibility::*;

        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

        let target_hwnd: HWND = if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
            let hwnd = find_window_by_title(t.trim())?;
            if hwnd.is_null() {
                anyhow::bail!("no window found matching '{}'", t);
            }
            hwnd
        } else {
            unsafe { GetForegroundWindow() }
        };
        if target_hwnd.is_null() {
            anyhow::bail!("no target window for ui_tree");
        }

        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
        let element = unsafe { automation.ElementFromHandle(WinHwnd(target_hwnd))? };
        let condition = unsafe { automation.CreateTrueCondition()? };
        let array = unsafe { element.FindAll(TreeScope_Descendants, &condition)? };
        let len = unsafe { array.Length()? }.max(0) as usize;

        let mut out = Vec::new();
        for i in 0..len {
            if out.len() >= super::UI_TREE_CAP {
                break;
            }
            let el = match unsafe { array.GetElement(i as i32) } {
                Ok(e) => e,
                Err(_) => continue,
            };
            let control_type = match unsafe { el.CurrentControlType() } {
                Ok(ct) => ct.0,
                Err(_) => continue,
            };
            if !is_interactive_control(control_type) {
                continue;
            }
            let name = unsafe { el.CurrentName() }
                .ok()
                .map(|b| b.to_string())
                .unwrap_or_default();
            let enabled = unsafe { el.CurrentIsEnabled() }
                .ok()
                .map(|b| b.as_bool())
                .unwrap_or(false);
            let bounds = unsafe { el.CurrentBoundingRectangle() }.ok();
            let bounds_json = match bounds {
                Some(r) => serde_json::json!({
                    "left": r.left,
                    "top": r.top,
                    "right": r.right,
                    "bottom": r.bottom,
                }),
                None => serde_json::json!({
                    "left": 0, "top": 0, "right": 0, "bottom": 0
                }),
            };
            out.push(serde_json::json!({
                "name": name,
                "control_type": control_type_name(control_type),
                "bounds": bounds_json,
                "enabled": enabled,
            }));
        }

        Ok(out)
    }

    /// Early-exit name scan for `wait`/`ui_text` — no JSON materialization.
    pub fn any_ui_name_contains(title: Option<&str>, needle: &str) -> anyhow::Result<bool> {
        use windows::Win32::Foundation::HWND as WinHwnd;
        use windows::Win32::System::Com::*;
        use windows::Win32::UI::Accessibility::*;

        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

        let target_hwnd: HWND = if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
            let hwnd = find_window_by_title(t.trim())?;
            if hwnd.is_null() {
                return Ok(false);
            }
            hwnd
        } else {
            unsafe { GetForegroundWindow() }
        };
        if target_hwnd.is_null() {
            return Ok(false);
        }

        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
        let element = unsafe { automation.ElementFromHandle(WinHwnd(target_hwnd))? };
        let condition = unsafe { automation.CreateTrueCondition()? };
        let array = unsafe { element.FindAll(TreeScope_Descendants, &condition)? };
        let len = unsafe { array.Length()? }.max(0) as i32;
        for i in 0..len {
            let el = match unsafe { array.GetElement(i) } {
                Ok(e) => e,
                Err(_) => continue,
            };
            let control_type = match unsafe { el.CurrentControlType() } {
                Ok(ct) => ct.0,
                Err(_) => continue,
            };
            if !is_interactive_control(control_type) {
                continue;
            }
            let name = unsafe { el.CurrentName() }
                .ok()
                .map(|b| b.to_string())
                .unwrap_or_default();
            if name.contains(needle) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(not(windows))]
mod imp {
    use serde_json::Value;

    pub fn enumerate_windows(_filter_pid: Option<u32>) -> anyhow::Result<Vec<Value>> {
        Ok(Vec::new())
    }

    pub fn get_foreground_window_info() -> anyhow::Result<Value> {
        Ok(serde_json::json!({"available": false, "note": "window operations require Windows"}))
    }

    pub fn focus_window(_title: Option<&str>, _pid: Option<u32>) -> anyhow::Result<()> {
        anyhow::bail!("window operations require Windows")
    }

    pub fn close_window(_title: Option<&str>, _pid: Option<u32>) -> anyhow::Result<()> {
        anyhow::bail!("window operations require Windows")
    }

    pub fn capture_screen(_path: Option<std::path::PathBuf>) -> anyhow::Result<Value> {
        anyhow::bail!("screenshot requires Windows")
    }

    pub fn any_title_contains(_needle: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    pub fn foreground_title_contains(_needle: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    pub fn enumerate_ui_tree(_title: Option<&str>) -> anyhow::Result<Vec<Value>> {
        anyhow::bail!("ui_tree requires Windows")
    }

    pub fn any_ui_name_contains(_title: Option<&str>, _needle: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    fn tool() -> WindowTool {
        WindowTool::new(None)
    }

    #[test]
    fn test_window_tool_name() {
        assert_eq!(tool().name(), "window");
    }

    #[test]
    fn test_window_tool_risk_level() {
        let t = tool();
        assert_eq!(t.risk_level(&json!({"operation": "list"})), RiskLevel::Low);
        assert_eq!(
            t.risk_level(&json!({"operation": "focus"})),
            RiskLevel::Medium
        );
        assert_eq!(
            t.risk_level(&json!({"operation": "close"})),
            RiskLevel::High
        );
        assert_eq!(t.risk_level(&json!({"operation": "ocr"})), RiskLevel::High);
        assert_eq!(
            t.risk_level(&json!({"operation": "ui_tree"})),
            RiskLevel::Low
        );
        assert_eq!(t.risk_level(&json!({"operation": "wait"})), RiskLevel::Low);
    }

    #[test]
    fn test_window_tool_input_schema() {
        let schema = tool().input_schema();
        let ops = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        let names: Vec<&str> = ops.iter().map(|v| v.as_str().unwrap()).collect();
        for expected in [
            "list",
            "foreground",
            "focus",
            "close",
            "screenshot",
            "ocr",
            "ui_tree",
            "wait",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        assert!(
            schema["properties"]["condition"]["enum"]
                .as_array()
                .is_some()
        );
        assert!(
            tool()
                .validate_input(&json!({"operation": "focus", "pid": 1}))
                .is_ok()
        );
        assert!(
            tool()
                .validate_input(&json!({"operation": "close", "pid": 1}))
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_window_execute_list() {
        let result = tool()
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        let windows = result.output["windows"].as_array().unwrap();
        for w in windows {
            assert!(w["title"].as_str().is_some());
            assert!(w["pid"].is_number());
        }
        assert!(result.output["count"].as_u64().unwrap() == windows.len() as u64);
    }

    #[tokio::test]
    async fn test_window_execute_list_filtered_by_pid() {
        let result = tool()
            .execute(
                json!({"operation": "list", "pid": 99999999}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let windows = result.output["windows"].as_array().unwrap();
        for w in windows {
            assert_eq!(w["pid"].as_u64().unwrap(), 99999999);
        }
    }

    #[tokio::test]
    async fn test_window_execute_foreground() {
        let result = tool()
            .execute(json!({"operation": "foreground"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        #[cfg(windows)]
        {
            assert!(result.output["hwnd"].is_number());
            assert!(result.output["title"].is_string());
            assert!(result.output["pid"].is_number());
        }
        #[cfg(not(windows))]
        {
            assert_eq!(result.output["available"], false);
        }
    }

    #[tokio::test]
    async fn test_window_execute_focus_no_match() {
        let result = tool()
            .execute(
                json!({"operation": "focus", "title": "haven-test-no-such-window-xyz"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_execute_close_no_match() {
        let result = tool()
            .execute(
                json!({"operation": "close", "title": "haven-test-no-such-window-xyz"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_execute_focus_requires_target() {
        let result = tool()
            .execute(json!({"operation": "focus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_execute_unknown_operation() {
        let result = tool()
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = tool().execute(json!({"operation": "list"}), cancel).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_native_entry_lands_in_run() {
        let result = tool()
            .run(
                WindowParams {
                    operation: Some(WindowOperation::Focus),
                    title: Some("haven-test-no-such-window-xyz".into()),
                    pid: None,
                    path: None,
                    condition: None,
                    text: None,
                    timeout_secs: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_ocr_without_router() {
        let result = tool()
            .execute(json!({"operation": "ocr"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["ocr_unavailable"], true);
        assert!(result.output.get("path").is_none());
    }

    #[tokio::test]
    async fn test_window_ui_tree_rejects_missing_target() {
        let result = tool()
            .execute(
                json!({"operation": "ui_tree", "title": "haven-test-no-such-window-xyz"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_wait_title_timeout() {
        let result = tool()
            .execute(
                json!({
                    "operation": "wait",
                    "condition": "title_contains",
                    "text": "haven-wait-no-such-title-xyz-999",
                    "timeout_secs": 1
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["timed_out"], true);
        assert_eq!(result.output["matched"], false);
        assert_eq!(result.output["waited"], true);
    }

    #[tokio::test]
    async fn test_window_wait_requires_condition() {
        let err = tool()
            .execute(
                json!({"operation": "wait", "text": "x"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("condition"));
    }
}
