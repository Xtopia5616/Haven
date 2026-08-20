use crate::commands::log_err;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Open a URL in the default browser, or a local filesystem path in the file manager.
///
/// - `http://` / `https://` → default browser (no shell metacharacter interpretation)
/// - absolute local paths only → reveal file / open folder
/// - UNC / relative / other schemes → rejected
#[tauri::command]
pub async fn open_external(target: String) -> Result<(), String> {
    let value = target.trim();
    if value.is_empty() {
        return Err("empty target".into());
    }

    if looks_like_http_url(value) {
        validate_http_url(value)?;
        open_url(value).map_err(|e| log_err("open_external", e))
    } else {
        let path = validate_local_path(value)?;
        open_path(&path).map_err(|e| log_err("open_external", e))
    }
}

fn looks_like_http_url(value: &str) -> bool {
    let lower = value.as_bytes();
    lower.len() >= 7
        && (value[..7].eq_ignore_ascii_case("http://")
            || (lower.len() >= 8 && value[..8].eq_ignore_ascii_case("https://")))
}

fn validate_http_url(url: &str) -> Result<(), String> {
    if url.chars().any(|c| c.is_control() || c == '\0') {
        return Err("url contains control characters".into());
    }
    // Scheme already checked by looks_like_http_url; still reject embedded
    // whitespace that could confuse handlers.
    if url.chars().any(|c| c.is_whitespace()) {
        return Err("url contains whitespace".into());
    }
    Ok(())
}

/// Accept only absolute local paths. Rejects UNC (`\\server\...`, `//server/...`),
/// `file:` URLs, and relative paths (those resolve against the process CWD, which
/// is not the workspace).
fn validate_local_path(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty path".into());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("file:") {
        return Err("file: URLs are not supported".into());
    }
    if trimmed.starts_with("\\\\") || trimmed.starts_with("//") {
        return Err("UNC paths are not supported".into());
    }

    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err("relative paths are not supported".into());
    }
    // Windows: require a normal drive prefix (C:\...), not `\\?\UNC\...` etc.
    #[cfg(windows)]
    {
        let mut comps = path.components();
        match comps.next() {
            Some(Component::Prefix(prefix)) => {
                use std::path::Prefix;
                match prefix.kind() {
                    Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {}
                    _ => return Err("only local drive paths are supported".into()),
                }
            }
            _ => return Err("only local drive paths are supported".into()),
        }
    }
    Ok(path)
}

fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // ShellExecuteW opens with the default handler and does not re-parse
        // through cmd.exe, so `&` / `|` in query strings stay part of the URL.
        open_with_shell_execute(url)
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn()?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn()?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn open_with_shell_execute(target: &str) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let operation = wide("open");
    let file = wide(target);
    // Per MSDN, return values ≤ 32 are errors.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as usize <= 32 {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("ShellExecuteW failed ({result:?})"),
        ))
    } else {
        Ok(())
    }
}

fn open_path(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        if path.is_file() {
            // Two separate args: Explorer wants `/select,` then the path, so
            // spaces inside the path stay intact.
            Command::new("explorer")
                .arg("/select,")
                .arg(path.as_os_str())
                .spawn()?;
        } else if path.is_dir() {
            Command::new("explorer").arg(path.as_os_str()).spawn()?;
        } else if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            // Path does not exist yet — open the nearest existing ancestor, or
            // the parent even if missing so Explorer can show an error.
            if parent.exists() {
                Command::new("explorer").arg(parent.as_os_str()).spawn()?;
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("path not found: {}", path.display()),
                ));
            }
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("path not found: {}", path.display()),
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        // Always reveal with `-R`. Plain `open` on a `.app` bundle (directory)
        // would launch the application — Ctrl+open must never execute code.
        if path.exists() {
            Command::new("open").args(["-R", path.as_os_str()]).spawn()?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("path not found: {}", path.display()),
            ));
        }
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if path.is_file() {
            path.parent().unwrap_or(path)
        } else if path.exists() {
            path
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("path not found: {}", path.display()),
            ));
        };
        Command::new("xdg-open").arg(target).spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_urls() {
        assert!(looks_like_http_url("https://example.com/a?b=1&c=2"));
        assert!(looks_like_http_url("HTTP://EXAMPLE.COM"));
        assert!(validate_http_url("https://example.com/a?b=1&c=2").is_ok());
    }

    #[test]
    fn rejects_control_chars_in_urls() {
        assert!(validate_http_url("https://example.com/\ncalc").is_err());
    }

    #[test]
    fn rejects_unc_and_relative_paths() {
        assert!(validate_local_path(r"\\server\share\file").is_err());
        assert!(validate_local_path("//server/share/file").is_err());
        assert!(validate_local_path("crates/llm/src/foo.rs").is_err());
        assert!(validate_local_path("file:///C:/a.txt").is_err());
    }

    #[test]
    #[cfg(windows)]
    fn accepts_windows_drive_paths() {
        assert!(validate_local_path(r"D:\Workspace\Haven\README.md").is_ok());
        assert!(validate_local_path(r"C:\Program Files\App\a.txt").is_ok());
    }

    #[test]
    #[cfg(not(windows))]
    fn accepts_unix_absolute_paths() {
        assert!(validate_local_path("/home/user/file.rs").is_ok());
    }
}
