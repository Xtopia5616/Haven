use serde_json::Value;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use windows_sys::Win32::Foundation::{FALSE, HWND, LPARAM, TRUE};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::BOOL;

pub(crate) fn window_id(hwnd: HWND) -> String {
    format!("hwnd:{:x}", hwnd as usize)
}

pub(crate) fn hwnd_from_window_id(id: &str) -> anyhow::Result<HWND> {
    let value = id
        .strip_prefix("hwnd:")
        .ok_or_else(|| anyhow::anyhow!("window_id must have the form hwnd:<hex>"))?;
    let raw = usize::from_str_radix(value, 16)
        .map_err(|_| anyhow::anyhow!("window_id contains an invalid handle"))?;
    if raw == 0 {
        anyhow::bail!("window_id must not be zero");
    }
    Ok(raw as HWND)
}

/// Read a window's visible title text, or None if the window is not
/// visible or has no title. Shared by window enumeration and search.
/// Callers must be inside an `unsafe` context.
pub(crate) fn visible_window_title(hwnd: HWND) -> Option<String> {
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

pub(crate) fn enumerate_windows(filter_pid: Option<u32>) -> anyhow::Result<Vec<Value>> {
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
                "window_id": window_id(hwnd),
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

pub(crate) fn get_foreground_window_info() -> anyhow::Result<Value> {
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
            "window_id": window_id(hwnd),
            "hwnd": hwnd as usize,
            "title": title,
            "pid": pid,
        }))
    }
}

pub(crate) fn focus_window(
    window_id: Option<&str>,
    title: Option<&str>,
    pid: Option<u32>,
) -> anyhow::Result<()> {
    let hwnd = find_window(window_id, title, pid)?
        .ok_or_else(|| window_not_found(window_id, title, pid))?;
    unsafe {
        SetForegroundWindow(hwnd);
    }
    Ok(())
}

pub(crate) fn close_window(
    window_id: Option<&str>,
    title: Option<&str>,
    pid: Option<u32>,
) -> anyhow::Result<()> {
    let hwnd = find_window(window_id, title, pid)?
        .ok_or_else(|| window_not_found(window_id, title, pid))?;
    unsafe {
        PostMessageW(hwnd, WM_CLOSE, 0, 0);
    }
    Ok(())
}

fn find_window(
    window_id: Option<&str>,
    title: Option<&str>,
    pid: Option<u32>,
) -> anyhow::Result<Option<HWND>> {
    if let Some(window_id) = window_id {
        let hwnd = hwnd_from_window_id(window_id)?;
        let windows = enumerate_windows(pid)?;
        return Ok(windows
            .iter()
            .any(|window| window["window_id"].as_str() == Some(window_id))
            .then_some(hwnd));
    }
    let windows = enumerate_windows(pid)?;
    let matches: Vec<&Value> = windows
        .iter()
        .filter(|window| {
            title
                .map(|needle| {
                    window["title"]
                        .as_str()
                        .map(|value| value.contains(needle))
                        .unwrap_or(false)
                })
                .unwrap_or(true)
        })
        .collect();
    if matches.len() > 1 {
        anyhow::bail!(
            "title '{}' matched {} windows; provide window_id or pid",
            title.unwrap_or(""),
            matches.len()
        );
    }
    Ok(matches
        .first()
        .copied()
        .and_then(|window| window["hwnd"].as_u64())
        .map(|hwnd| hwnd as usize as HWND))
}

fn window_not_found(
    window_id: Option<&str>,
    title: Option<&str>,
    pid: Option<u32>,
) -> anyhow::Error {
    match (window_id, title, pid) {
        (Some(window_id), _, _) => {
            anyhow::anyhow!("no window found for window_id '{}'", window_id)
        }
        (_, Some(title), Some(pid)) => {
            anyhow::anyhow!("no window found matching title '{}' for pid {}", title, pid)
        }
        (_, Some(title), None) => anyhow::anyhow!("no window found matching '{}'", title),
        (_, None, Some(pid)) => anyhow::anyhow!("no window found for pid {}", pid),
        (_, None, None) => anyhow::anyhow!("a title or pid is required"),
    }
}

pub(crate) fn find_window_by_title(title: &str) -> anyhow::Result<HWND> {
    find_window(None, Some(title), None)?.ok_or_else(|| window_not_found(None, Some(title), None))
}

pub(crate) fn any_title_contains(needle: &str) -> anyhow::Result<bool> {
    let windows = enumerate_windows(None)?;
    Ok(windows.iter().any(|w| {
        w["title"]
            .as_str()
            .map(|t| t.contains(needle))
            .unwrap_or(false)
    }))
}

pub(crate) fn foreground_title_contains(needle: &str) -> anyhow::Result<bool> {
    let fg = get_foreground_window_info()?;
    Ok(fg["title"]
        .as_str()
        .map(|t| t.contains(needle))
        .unwrap_or(false))
}
