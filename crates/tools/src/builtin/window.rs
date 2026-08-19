use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct WindowTool;

/// Window operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowOperation {
    List,
    Foreground,
    Focus,
    Close,
    Screenshot,
}

/// Typed parameters for `WindowTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `WindowTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WindowParams {
    /// Operation to perform; defaults to `list`.
    #[serde(default)]
    pub operation: Option<WindowOperation>,
    /// Window title to match (substring, used for focus/close).
    #[serde(default)]
    pub title: Option<String>,
    /// Filter windows by PID.
    #[serde(default)]
    pub pid: Option<i64>,
    /// Optional output path for screenshot; defaults to a temp file.
    #[serde(default)]
    pub path: Option<String>,
}

impl WindowTool {
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

        let filter_pid = params.pid.map(|p| p as u32);
        let title = params.title;

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
                Ok(ToolResult::ok(result))
            }
            WindowOperation::Foreground => {
                let fg = imp::get_foreground_window_info()?;
                Ok(ToolResult::ok(fg))
            }
            WindowOperation::Focus => {
                let t = title.ok_or_else(|| anyhow::anyhow!("title is required for focus"))?;
                imp::focus_window_by_title(&t)?;
                Ok(ToolResult::ok(serde_json::json!({"focused": t})))
            }
            WindowOperation::Close => {
                let t = title.ok_or_else(|| anyhow::anyhow!("title is required for close"))?;
                imp::close_window_by_title(&t)?;
                Ok(ToolResult::ok(serde_json::json!({"closed": t})))
            }
            WindowOperation::Screenshot => {
                let path = params
                    .path
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| std::path::PathBuf::from(p.trim()));
                let shot = imp::capture_screen(path)?;
                Ok(ToolResult::ok(shot))
            }
        }
    }
}

#[async_trait]
impl Tool for WindowTool {
    fn name(&self) -> String {
        "window".into()
    }
    fn description(&self) -> String {
        "List, query, and manage desktop windows".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("close") => RiskLevel::High,
            Some("focus") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["list", "foreground", "focus", "close", "screenshot"]
                },
                "title": { "type": "string", "description": "Window title to match (substring, used for focus/close)" },
                "pid": { "type": "integer", "description": "Filter windows by PID" },
                "path": { "type": "string", "description": "Optional output path for screenshot; defaults to a temp file" }
            },
            "required": ["operation"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `WindowParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<WindowParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(windows)]
mod imp {
    use serde_json::Value;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{BOOL, FALSE, HWND, LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

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

    pub fn focus_window_by_title(title: &str) -> anyhow::Result<()> {
        unsafe {
            let hwnd = find_window_by_title(title)?;
            if hwnd.is_null() {
                anyhow::bail!("no window found matching '{}'", title);
            }
            SetForegroundWindow(hwnd);
            Ok(())
        }
    }

    pub fn close_window_by_title(title: &str) -> anyhow::Result<()> {
        unsafe {
            let hwnd = find_window_by_title(title)?;
            if hwnd.is_null() {
                anyhow::bail!("no window found matching '{}'", title);
            }
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
            Ok(())
        }
    }

    unsafe fn find_window_by_title(substring: &str) -> anyhow::Result<HWND> {
        let mut found: HWND = std::ptr::null_mut();
        let substr_wide: Vec<u16> = substring.encode_utf16().chain(std::iter::once(0)).collect();

        extern "system" fn search_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                let (found_ptr, substr_ptr) = *(lparam as *mut (*mut HWND, *const u16));
                let Some(title) = visible_window_title(hwnd) else {
                    return TRUE;
                };

                let substr = String::from_utf16_lossy(std::slice::from_raw_parts(substr_ptr, {
                    let mut i = 0;
                    while *substr_ptr.add(i) != 0 {
                        i += 1;
                    }
                    i
                }));

                if title.contains(&substr) {
                    *found_ptr = hwnd;
                    return FALSE;
                }
                TRUE
            }
        }

        let mut pair = (&mut found as *mut HWND, substr_wide.as_ptr());
        unsafe {
            EnumWindows(Some(search_callback), &mut pair as *mut _ as LPARAM);
        }

        Ok(found)
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
            for px in rgba.chunks_exact_mut(4) {
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
                "hint": "Open the image with the file tool (read) to view it.",
            }))
        }
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

    pub fn focus_window_by_title(_title: &str) -> anyhow::Result<()> {
        anyhow::bail!("window operations require Windows")
    }

    pub fn close_window_by_title(_title: &str) -> anyhow::Result<()> {
        anyhow::bail!("window operations require Windows")
    }

    pub fn capture_screen(_path: Option<std::path::PathBuf>) -> anyhow::Result<Value> {
        anyhow::bail!("screenshot requires Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_window_tool_name() {
        assert_eq!(WindowTool.name(), "window");
    }

    #[test]
    fn test_window_tool_risk_level() {
        assert_eq!(
            WindowTool.risk_level(&json!({"operation": "list"})),
            RiskLevel::Low
        );
        assert_eq!(
            WindowTool.risk_level(&json!({"operation": "focus"})),
            RiskLevel::Medium
        );
        assert_eq!(
            WindowTool.risk_level(&json!({"operation": "close"})),
            RiskLevel::High
        );
    }

    #[test]
    fn test_window_tool_input_schema() {
        let schema = WindowTool.input_schema();
        assert!(
            schema["properties"]["operation"]["enum"]
                .as_array()
                .is_some()
        );
    }

    #[tokio::test]
    async fn test_window_execute_list() {
        let result = WindowTool
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
        let result = WindowTool
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
        let result = WindowTool
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
        let result = WindowTool
            .execute(
                json!({"operation": "focus", "title": "haven-test-no-such-window-xyz"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_execute_close_no_match() {
        let result = WindowTool
            .execute(
                json!({"operation": "close", "title": "haven-test-no-such-window-xyz"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_execute_focus_requires_title() {
        let result = WindowTool
            .execute(json!({"operation": "focus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_execute_unknown_operation() {
        let result = WindowTool
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = WindowTool
            .execute(json!({"operation": "list"}), cancel)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_window_native_entry_lands_in_run() {
        let result = WindowTool
            .run(
                WindowParams {
                    operation: Some(WindowOperation::Focus),
                    title: Some("haven-test-no-such-window-xyz".into()),
                    pid: None,
                    path: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }
}
