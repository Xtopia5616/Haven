use super::*;

// ── sanitize_shell_output ─────────────────────────────────────────────

#[test]
fn test_sanitize_shell_output_normalizes_crlf() {
    let out = sanitize_shell_output("line1\r\nline2\r\n", "cmd");
    assert_eq!(out, "line1\nline2\n");
}
#[test]
fn test_sanitize_shell_output_keeps_lone_cr_for_progress() {
    // Progress-redraw lines use lone `\r`; they must survive so
    // summarize_error can collapse them.
    let out = sanitize_shell_output("downloading 42%\rfinal", "cmd");
    assert!(out.contains('\r'), "lone \\r must be kept: {out:?}");
}

#[test]
fn test_sanitize_shell_output_non_powershell_unchanged() {
    let text = "NativeCommandError\nreal line";
    assert_eq!(sanitize_shell_output(text, "cmd"), text);
}

#[test]
fn test_sanitize_shell_output_strips_native_command_error_noise() {
    // Windows PowerShell 5.1 error-record formatting around a real error.
    let text = "NativeCommandError\ncurl.exe : curl: (7) Failed to connect\nAt line:1 char:1\n+ & curl.exe https://x\n+ ~~~~~~~~~~~~~~~~~~~~\n    + CategoryInfo          : NotSpecified: (curl.exe : ...)\n    + FullyQualifiedErrorId : NativeCommandError\n";
    let out = sanitize_shell_output(text, "powershell");
    assert!(!out.contains("NativeCommandError"), "got: {out}");
    assert!(!out.contains("CategoryInfo"), "got: {out}");
    assert!(!out.contains("At line:"), "got: {out}");
    assert!(
        out.contains("curl.exe : curl: (7) Failed to connect"),
        "real error must survive, got: {out}"
    );
}

#[test]
fn test_sanitize_shell_output_strips_clixml_wrapper() {
    // Windows PowerShell 5.1 serializes a merged native error stream as a
    // CLIXML document on stderr. Payloads are XML-escaped (`&gt;`, `&amp;`)
    // and char-escaped (`_x000D_`/`_x000A_`), and the record carries
    // localized position/category noise lines that must be dropped.
    let text = concat!(
        "#< CLIXML\r\n",
        "<Objs Version=\"1.1.0.1\" xmlns=\"http://schemas.microsoft.com/powershell/2004/04\">",
        "<S S=\"Error\">cmd : boom: connection refused _x000D__x000A_</S>",
        "<S S=\"Error\">所在位置 行:1 字符: 77_x000D__x000A_</S>",
        "<S S=\"Error\">+ ... 2&gt;&amp;1_x000D__x000A_</S>",
        "<S S=\"Error\">+ ~~~~~~~~~~~~~~~~~~~~~_x000D__x000A_</S>",
        "<S S=\"Error\">    + CategoryInfo          : NotSpecified: (boom :String) [], RemoteException_x000D__x000A_</S>",
        "<S S=\"Error\">    + FullyQualifiedErrorId : NativeCommandError_x000D__x000A_</S>",
        "<S S=\"Error\"> _x000D__x000A_</S>",
        "</Objs>"
    );
    let out = sanitize_shell_output(text, "powershell");
    assert!(
        out.contains("boom: connection refused"),
        "message must survive CLIXML, got: {out}"
    );
    assert!(!out.contains("CLIXML"), "got: {out}");
    assert!(
        !out.contains("_x000D_"),
        "char escapes must be decoded, got: {out}"
    );
    assert!(
        !out.contains("&gt;") && !out.contains("&amp;"),
        "xml escapes must be decoded, got: {out}"
    );
    assert!(
        !out.contains("CategoryInfo"),
        "noise must be dropped, got: {out}"
    );
    assert!(
        !out.contains("所在位置"),
        "localized position line must be dropped, got: {out}"
    );
}

#[test]
fn test_sanitize_shell_output_clixml_preserves_preceding_stdout() {
    // Real stdout captured next to a CLIXML stderr blob must not be lost:
    // only the CLIXML document is replaced, not the whole text.
    let text = "build succeeded\n#< CLIXML\n\u{1f}<Objs V=\"1\" S=\"Err\"><S S=\"Error\">boom: connection refused</S></Objs>";
    let out = sanitize_shell_output(text, "powershell");
    assert!(
        out.contains("build succeeded"),
        "stdout must survive, got: {out}"
    );
    assert!(out.contains("boom: connection refused"), "got: {out}");
    assert!(!out.contains("CLIXML"), "got: {out}");
}

#[test]
fn test_sanitize_shell_output_drops_progress_clixml() {
    // ProgressRecords (module first-use / Write-Progress) serialize as
    // CLIXML with no <S S="Error">. The old strip_xml_markup fallback
    // concatenated type names + Activity into a fake error like
    // "System.Management.Automation.PSCustomObject…正在准备首次使用模块".
    let text = concat!(
        "#< CLIXML\r\n",
        "<Objs Version=\"1.1.0.1\" xmlns=\"http://schemas.microsoft.com/powershell/2004/04\">",
        "<Obj S=\"progress\" RefId=\"0\">",
        "<TN RefId=\"0\"><T>System.Management.Automation.PSCustomObject</T>",
        "<T>System.Object</T></TN>",
        "<MS><S N=\"Activity\">正在准备首次使用模块。</S>",
        "<S N=\"StatusDescription\">Completed</S>",
        "<I32 N=\"PercentComplete\">100</I32></MS></Obj></Objs>"
    );
    let out = sanitize_shell_output(text, "powershell");
    assert!(
        out.is_empty(),
        "progress-only CLIXML must be dropped, got: {out:?}"
    );
    assert!(!out.contains("PSCustomObject"));
    assert!(!out.contains("正在准备首次使用模块"));
}

#[test]
fn test_sanitize_shell_output_truncated_progress_clixml_dropped() {
    // Large-output caps can cut mid-CLIXML (no </Objs>). Truncated
    // progress markup must not leak as the failure text.
    let text = concat!(
        "real stdout\n",
        "#< CLIXML\r\n",
        "<Objs Version=\"1.1.0.1\">",
        "<Obj S=\"progress\" RefId=\"0\">",
        "<TN RefId=\"0\"><T>System.Management.Automation.PSCustomObject</T>",
        "<T>System.Object</T></TN>",
        "<MS><S N=\"Activity\">正在准备首次使用模块。</S>",
        "<S N=\"StatusDescription\">Comp" // truncated
    );
    let out = sanitize_shell_output(text, "powershell");
    assert_eq!(out, "real stdout");
    assert!(!out.contains("PSCustomObject"));
    assert!(!out.contains("CLIXML"));
}

#[test]
fn test_sanitize_shell_output_truncated_clixml_keeps_complete_and_partial_errors() {
    // Truncated doc keeps complete Error segments and unfinished trailing
    // Error inner text (cap cut mid-</S>).
    let text = concat!(
        "#< CLIXML\r\n",
        "<Objs Version=\"1.1.0.1\">",
        "<S S=\"Error\">boom: connection refused _x000D__x000A_</S>",
        "<S S=\"Error\">partially cut" // no closing </S> / </Objs>
    );
    let out = sanitize_shell_output(text, "powershell");
    assert!(
        out.contains("boom: connection refused"),
        "complete Error segment must survive truncation, got: {out}"
    );
    assert!(
        out.contains("partially cut"),
        "unfinished Error inner must survive, got: {out}"
    );
    assert!(!out.contains("CLIXML"));
}

#[test]
fn test_sanitize_shell_output_truncated_unknown_clixml_kept() {
    // Truncated non-progress CLIXML with no Error tags must not be wiped
    // (raw remainder stays recoverable).
    let text = concat!(
        "stdout ok\n",
        "#< CLIXML\r\n",
        "<Objs Version=\"1.1.0.1\"><S S=\"Warning\">disk almost full"
    );
    let out = sanitize_shell_output(text, "powershell");
    assert!(out.contains("stdout ok"), "got: {out}");
    assert!(
        out.contains("disk almost full") || out.contains("CLIXML"),
        "non-progress truncated remainder must survive, got: {out}"
    );
}

#[test]
fn test_sanitize_shell_output_strips_ansi_escapes() {
    // pwsh 7 renders error records with $PSStyle ANSI colors even into a
    // pipe; the escapes must not reach the model.
    let out = sanitize_shell_output("\u{1b}[31;1mboom\u{1b}[0m", "powershell");
    assert_eq!(out, "boom");
    let out = sanitize_shell_output("\u{1b}[31;1mboom\u{1b}[0m", "cmd");
    assert_eq!(out, "boom");
}

#[test]
fn test_sanitize_shell_output_strips_ansi_escaped_inside_clixml() {
    // Inside a CLIXML payload the ESC byte is char-escaped as `_x001B_`;
    // the escape must be stripped after unescaping, not just on the raw
    // text (where no literal ESC exists yet).
    let text = concat!(
        "#< CLIXML\r\n",
        "<Objs V=\"1\" S=\"Err\">",
        "<S S=\"Error\">_x001B_[31;1mboom: refused_x001B_[0m _x000D__x000A_</S>",
        "</Objs>"
    );
    let out = sanitize_shell_output(text, "powershell");
    assert_eq!(out, "boom: refused", "got: {out:?}");
}

#[test]
fn test_sanitize_shell_output_non_powershell_still_strips_ansi() {
    let out = sanitize_shell_output("\u{1b}[2K\r34%", "cmd");
    assert_eq!(out, "\r34%", "non-ANSI control must be untouched");
}

#[test]
fn test_sanitize_shell_output_keeps_plus_prefixed_content() {
    // A real `+line` (e.g. git diff addition) must not be mistaken for a
    // PowerShell continuation line.
    let out = sanitize_shell_output("+added line", "powershell");
    assert_eq!(out, "+added line");
}

// ── append_windows_diagnostics ────────────────────────────────────────

#[cfg(windows)]
#[test]
fn test_windows_diagnostics_curl_alias() {
    let out = append_windows_diagnostics(
        "powershell",
        "curl https://example.com",
        "curl.exe : Invoke-WebRequest failed",
    );
    assert!(
        out.contains("Windows trap"),
        "a hint must be appended, got: {out}"
    );
    assert!(
        out.contains("curl.exe"),
        "hint must name curl.exe, got: {out}"
    );
}

#[cfg(windows)]
#[test]
fn test_windows_diagnostics_no_hint_for_clean_error() {
    let out = append_windows_diagnostics("cmd", "echo hi", "some unrelated failure");
    assert_eq!(out, "some unrelated failure", "no hint, no change: {out}");
}

#[cfg(windows)]
#[test]
fn test_windows_diagnostics_cmd_syntax_error_cn() {
    let out = append_windows_diagnostics("cmd", "dir /q \\x", "该命令的语法不正确。");
    assert!(out.contains("Windows trap"), "got: {out}");
    assert!(
        out.contains("syntax"),
        "hint must explain quoting, got: {out}"
    );
}

#[cfg(windows)]
#[test]
fn test_windows_diagnostics_powershell_and_chaining() {
    let out = append_windows_diagnostics(
        "powershell",
        "git clone x && cd y",
        "The token '&&' is not a valid statement separator in this version.",
    );
    assert!(out.contains("`;`"), "hint must suggest `;`, got: {out}");
}

#[cfg(windows)]
#[test]
fn test_windows_diagnostics_execution_policy() {
    let out = append_windows_diagnostics(
        "powershell",
        "npm install",
        "npm : 无法加载文件 npm.ps1，因为在此系统上禁止运行脚本",
    );
    assert!(
        out.contains("npm.cmd"),
        "hint must suggest the .cmd wrapper, got: {out}"
    );
}

// ── summarize_error ───────────────────────────────────────────────────

#[test]
fn test_summarize_error_drops_progress_and_keeps_tail() {
    // Real progress bars redraw one line with mid-line carriage returns
    // (no newlines); the whole redraw must collapse to nothing while the
    // actual error line survives.
    let mut text = String::new();
    for i in 0..50 {
        text.push_str(&format!("[download] {i}% of 1000MiB in 00:0{i}\r"));
    }
    text.push_str("\ncurl: (7) Failed to connect to x port 443");
    let out = summarize_error(&text, 1200);
    assert!(
        !out.contains('%'),
        "progress lines must be dropped, got: {out}"
    );
    assert!(out.contains("Failed to connect"), "got: {out}");
}

#[test]
fn test_summarize_error_caps_length() {
    let text = (0..200)
        .map(|i| format!("error line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = summarize_error(&text, 100);
    assert!(
        out.chars().count() <= 130,
        "got {} chars",
        out.chars().count()
    );
    assert!(out.contains("omitted"), "cap notice expected, got: {out}");
}

#[test]
fn test_summarize_error_blank_lines_removed() {
    let out = summarize_error("a\n\n\nb", 1200);
    assert_eq!(out, "a\nb");
}
