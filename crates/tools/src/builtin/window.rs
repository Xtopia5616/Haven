use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct WindowTool;

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

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let op = input["operation"].as_str().unwrap_or("list").to_string();
        let filter_pid = input["pid"].as_i64().map(|p| p as u32);
        let title = input["title"].as_str().map(|s| s.to_string());

        match op.as_str() {
            "list" => {
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
            "foreground" => {
                let fg = imp::get_foreground_window_info()?;
                Ok(ToolResult::ok(fg))
            }
            "focus" => {
                let t = title.ok_or_else(|| anyhow::anyhow!("title is required for focus"))?;
                imp::focus_window_by_title(&t)?;
                Ok(ToolResult::ok(serde_json::json!({"focused": t})))
            }
            "close" => {
                let t = title.ok_or_else(|| anyhow::anyhow!("title is required for close"))?;
                imp::close_window_by_title(&t)?;
                Ok(ToolResult::ok(serde_json::json!({"closed": t})))
            }
            "screenshot" => {
                let path = input["path"]
                    .as_str()
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| std::path::PathBuf::from(p.trim()));
                let shot = imp::capture_screen(path)?;
                Ok(ToolResult::ok(shot))
            }
            _ => anyhow::bail!("unknown window operation: {}", op),
        }
    }
}

#[cfg(windows)]
mod imp {
    use serde_json::Value;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{BOOL, FALSE, HWND, LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    pub fn enumerate_windows(filter_pid: Option<u32>) -> anyhow::Result<Vec<Value>> {
        let mut windows: Vec<Value> = Vec::new();

        unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                let windows = &mut *(lparam as *mut Vec<Value>);

                if IsWindowVisible(hwnd) == FALSE {
                    return TRUE;
                }

                let mut title_buf = [0u16; 512];
                let len = GetWindowTextW(hwnd, title_buf.as_mut_ptr(), 512);
                if len == 0 {
                    return TRUE;
                }
                let title = OsString::from_wide(&title_buf[..len as usize])
                    .to_string_lossy()
                    .to_string();

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
                if IsWindowVisible(hwnd) == FALSE {
                    return TRUE;
                }
                let mut title_buf = [0u16; 512];
                let len = GetWindowTextW(hwnd, title_buf.as_mut_ptr(), 512);
                if len == 0 {
                    return TRUE;
                }
                let title = OsString::from_wide(&title_buf[..len as usize])
                    .to_string_lossy()
                    .to_string();

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
        use windows_sys::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDCW, DeleteDC,
            DeleteObject, GetDIBits, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
            BI_RGB, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

        unsafe {
            let width = GetSystemMetrics(SM_CXSCREEN);
            let height = GetSystemMetrics(SM_CYSCREEN);
            if width <= 0 || height <= 0 {
                anyhow::bail!("failed to query screen size ({width}x{height})");
            }

            let screen_dc = CreateDCW(std::ptr::null(), "DISPLAY".encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>().as_ptr(), std::ptr::null(), std::ptr::null());
            if screen_dc.is_null() {
                anyhow::bail!("failed to create screen DC");
            }
            let mem_dc = CreateCompatibleDC(screen_dc);
            if mem_dc.is_null() {
                DeleteDC(screen_dc);
                anyhow::bail!("failed to create memory DC");
            }
            let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
            if bitmap.is_null() {
                DeleteDC(mem_dc);
                DeleteDC(screen_dc);
                anyhow::bail!("failed to create compatible bitmap");
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
            DeleteDC(screen_dc);

            if ok == 0 {
                anyhow::bail!("BitBlt failed");
            }
            if copied == 0 {
                anyhow::bail!("GetDIBits failed");
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
}
