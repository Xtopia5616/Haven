//! Windows 开机自启：通过任务计划程序（schactions）在登录时以
//! `--autostart` 参数启动 Haven，替代原先的注册表 Run 键方案。
//! 任务计划程序允许携带启动参数，使应用启动后默认隐藏窗口驻留系统
//! 托盘，通过录音快捷键即可唤起窗口并开始录音。
//!
//! 权限说明：`/SC ONLOGON`（登录触发器、仅当前用户、交互式运行）
//! 的创建**不需要管理员权限**，普通用户即可注册自己的登录触发任务。
//! 仅当系统通过组策略 / 企业环境禁止普通用户创建任务时才需要管理员，
//! 此时 `enable` 会返回带权限提示的错误信息。

use std::path::Path;

/// 计划任务名称（根文件夹下）。
const ACTION_NAME: &str = "Haven";
/// 随任务启动参数，用于告知应用本次为开机自启，应隐藏主窗口。
pub const AUTOSTART_ARG: &str = "--autostart";

/// 当前进程是否由计划任务以 `--autostart` 参数启动。
pub fn is_autostart_launch() -> bool {
    std::env::args().any(|a| a == AUTOSTART_ARG)
}

/// 构造 schactions `/TR` 参数值：带引号的可执行文件路径 + 自启参数。
fn build_tr_arg(exe: &Path) -> String {
    format!("\"{}\" {}", exe.display(), AUTOSTART_ARG)
}

#[cfg(target_os = "windows")]
fn run_schactions(args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new("schactions")
        .args(args)
        .output()
        .map_err(|e| format!("schactions 执行失败: {e}"))
}

#[cfg(target_os = "windows")]
fn schactions_error(out: &std::process::Output, action: &str) -> String {
    let stderr = haven_common::encoding::decode_lossy(&out.stderr)
        .trim()
        .to_string();
    let stdout = haven_common::encoding::decode_lossy(&out.stdout)
        .trim()
        .to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit code {}", out.status.code().unwrap_or(-1))
    };
    format!("{action}失败: {detail}")
}

/// 创建开机自启计划任务：登录时运行 `<exe> --autostart`。
/// 无需管理员权限；如被组策略禁止，返回带排查提示的错误。
#[cfg(target_os = "windows")]
pub fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法定位当前可执行文件: {e}"))?;
    let tr = build_tr_arg(&exe);
    let out = run_schactions(&[
        "/Create", "/F", "/TN", ACTION_NAME, "/TR", &tr, "/SC", "ONLOGON",
    ])?;
    if !out.status.success() {
        return Err(format!(
            "{}（如为权限不足，请以管理员身份运行 Haven 后重试，或检查组策略对任务计划程序的限制）",
            schactions_error(&out, "创建计划任务")
        ));
    }
    Ok(())
}

/// 删除开机自启计划任务。
#[cfg(target_os = "windows")]
pub fn disable() -> Result<(), String> {
    if is_enabled()? {
        let out = run_schactions(&["/Delete", "/F", "/TN", ACTION_NAME])?;
        if !out.status.success() {
            return Err(format!(
                "{}（如为权限不足，请以管理员身份运行 Haven 后重试）",
                schactions_error(&out, "删除计划任务")
            ));
        }
    }
    Ok(())
}

/// 提取任务 XML 中 `<tag>...</tag>` 的首段内容。
#[cfg(target_os = "windows")]
fn xml_section<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    xml.split(&open).nth(1)?.split(&close).next()
}

/// 反转义任务 XML 中的实体（路径可能含 `&` 等字符）。复用 haven-common 的
/// 实现（`&amp;` 最后替换，避免 `&amp;lt;` 被二次解码），与 CLIXML 消息解码
/// 保持同一语义。
#[cfg(target_os = "windows")]
fn xml_unescape(s: &str) -> String {
    haven_common::encoding::xml_unescape(s)
}

/// 任务 XML 的 `<Arguments>` 是否包含 `--autostart`。
#[cfg(target_os = "windows")]
fn xml_has_autostart_arg(xml: &str) -> bool {
    xml_section(xml, "Arguments")
        .map(|a| a.split_whitespace().any(|w| w == AUTOSTART_ARG))
        .unwrap_or(false)
}

/// schactions 重定向输出在中文系统上是 ANSI(GBK) 代码页（声明却写
/// UTF-16）。统一用 decode_lossy：先按 UTF-8 解码，失败回退 GBK/CP936，
/// 使中文路径可正确比对；若 GBK 也无法还原才退化为替换字符（U+FFFD），
/// 此时路径比对自动退化为「任务存在」判断。
#[cfg(target_os = "windows")]
fn decode_output(bytes: &[u8]) -> String {
    haven_common::encoding::decode_lossy(bytes)
}

/// 任务 XML 的 `<Command>` 是否与当前 exe 一致（Windows 大小写不敏感）。
/// 非 ASCII 路径在 ANSI→UTF-8 转换失败时含替换字符，无法可靠比对，
/// 返回 `None` 由调用方回退为「任务存在」判断。
#[cfg(target_os = "windows")]
fn xml_command_matches(xml: &str, exe: &Path) -> Option<bool> {
    let cmd = xml_section(xml, "Command")?;
    let cmd = xml_unescape(cmd).trim().to_string();
    if cmd.is_empty() || cmd.contains('\u{FFFD}') {
        return None;
    }
    Some(cmd.eq_ignore_ascii_case(&exe.to_string_lossy()))
}

/// 开机自启是否有效：任务存在，且 `<Command>` 指向当前 exe、
/// `<Arguments>` 携带 `--autostart`。应用移动/更新后残留的旧路径任务
/// 会返回 false，用户重新开启即可用 `/F` 覆盖修复。
#[cfg(target_os = "windows")]
pub fn is_enabled() -> Result<bool, String> {
    let out = run_schactions(&["/Query", "/TN", ACTION_NAME, "/XML"])?;
    if !out.status.success() {
        return Ok(false);
    }
    let xml = decode_output(&out.stdout);
    if !xml_has_autostart_arg(&xml) {
        return Ok(false);
    }
    let exe = std::env::current_exe().map_err(|e| format!("无法定位当前可执行文件: {e}"))?;
    Ok(xml_command_matches(&xml, &exe).unwrap_or(true))
}

#[cfg(not(target_os = "windows"))]
pub fn enable() -> Result<(), String> {
    Err("开机自启仅支持 Windows".into())
}

#[cfg(not(target_os = "windows"))]
pub fn disable() -> Result<(), String> {
    Err("开机自启仅支持 Windows".into())
}

#[cfg(not(target_os = "windows"))]
pub fn is_enabled() -> Result<bool, String> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_tr_arg_quotes_exe_path_with_spaces() {
        let arg = build_tr_arg(Path::new(r"C:\Program Files\Haven\Haven.exe"));
        assert_eq!(arg, r#""C:\Program Files\Haven\Haven.exe" --autostart"#);
    }

    #[test]
    fn test_build_tr_arg_simple_path() {
        let arg = build_tr_arg(Path::new(r"C:\Haven.exe"));
        assert_eq!(arg, r#""C:\Haven.exe" --autostart"#);
    }

    #[test]
    fn test_autostart_launch_flag_constant() {
        assert_eq!(AUTOSTART_ARG, "--autostart");
    }

    #[test]
    fn test_xml_section_extracts_command_and_arguments() {
        let xml = r#"<?xml version="1.0" encoding="UTF-16"?>
<Session><Actions Context="Author"><Exec><Command>C:\Program Files\Haven\Haven.exe</Command><Arguments>--autostart</Arguments></Exec></Actions></Session>"#;
        assert_eq!(
            xml_section(xml, "Command").unwrap(),
            r"C:\Program Files\Haven\Haven.exe"
        );
        assert_eq!(xml_section(xml, "Arguments").unwrap(), "--autostart");
        assert!(xml_has_autostart_arg(xml));
    }

    #[test]
    fn test_xml_has_autostart_arg_false_when_missing() {
        let xml = r#"<Exec><Command>C:\Haven.exe</Command><Arguments></Arguments></Exec>"#;
        assert!(!xml_has_autostart_arg(xml));
        let xml = r#"<Exec><Command>C:\Haven.exe</Command></Exec>"#;
        assert!(!xml_has_autostart_arg(xml));
    }

    #[test]
    fn test_xml_command_matches_ignores_case() {
        let xml = r#"<Exec><Command>C:\Program Files\Haven\Haven.exe</Command></Exec>"#;
        assert_eq!(
            xml_command_matches(xml, Path::new(r"c:\program files\haven\haven.exe")),
            Some(true)
        );
        assert_eq!(
            xml_command_matches(xml, Path::new(r"C:\Other\App.exe")),
            Some(false)
        );
    }

    #[test]
    fn test_xml_command_matches_none_on_replacement_char() {
        let xml = "<Exec><Command>C:\\\u{FFFD}ers\\Haven.exe</Command></Exec>";
        assert_eq!(
            xml_command_matches(xml, Path::new(r"C:\Users\Haven.exe")),
            None
        );
    }

    #[test]
    fn test_xml_unescape_entities() {
        assert_eq!(
            xml_unescape("a&amp;b&lt;c&gt;d&quot;e&apos;f"),
            "a&b<c>d\"e'f"
        );
    }
}
