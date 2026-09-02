/// Strip PowerShell-specific noise from captured command output so the real
/// message survives instead of NativeCommandError formatting:
/// - pwsh 7 serializes native stderr as CLIXML (`#< CLIXML` + escape chars);
///   the message text inside `<S S="Error">...</S>` segments is extracted and
///   its XML entities (`&amp;`, `&gt;`) and `_xHHHH_` char escapes are decoded.
/// - Windows PowerShell 5.1 wraps error records with header lines
///   (`NativeCommandError`, `At line:`/`所在位置 行:`, `+ `, `~~~`,
///   `+ CategoryInfo`, `+ FullyQualifiedErrorId`) that add no information.
/// - pwsh 7 renders error records with `$PSStyle` ANSI colors even into a
///   pipe (`ESC[31;1m … ESC[0m`); those escapes are removed for every shell.
///
/// CRLF line endings (cmd.exe / native Windows tools) are normalized to LF
/// for every shell so downstream line-based processing and the model never
/// see stray `\r` characters. Lone `\r` (progress-redraw lines) is kept —
/// `summarize_error` relies on it to collapse progress bars.
///
/// Non-PowerShell output is returned unchanged apart from line-ending and
/// ANSI cleanup.
pub fn sanitize_shell_output(text: &str, shell: &str) -> String {
    let text = strip_ansi_escapes(text);
    let text = text.replace("\r\n", "\n");
    if shell != "powershell" && shell != "pwsh" {
        return text;
    }
    let text = if text.contains("#< CLIXML") {
        // ANSI escapes inside a CLIXML payload arrive as `_x001B_` and only
        // become literal ESC after unescaping, so strip again after extraction.
        strip_ansi_escapes(&replace_clixml_documents(&text))
    } else {
        text
    };
    let mut out = Vec::with_capacity(text.len() / 32);
    for line in text.split('\n') {
        let trimmed = line.trim();
        if is_powershell_noise_line(trimmed) {
            continue;
        }
        out.push(line);
    }
    let joined = out.join("\n");
    joined.trim().to_string()
}

/// Replace every CLIXML document (`#< CLIXML` … `</Objs>`) in `text` with the
/// human-readable messages inside it. Content before/after a document (e.g.
/// real stdout lines captured next to a CLIXML stderr blob) is preserved —
/// otherwise sanitizing would silently drop the actual command output.
///
/// Progress-only CLIXML (module first-use / Write-Progress) has no
/// `<S S="Error">` segments; those documents are dropped entirely. Truncated
/// captures (cap cut mid-document, no `</Objs>`) still extract complete and
/// unfinished Error segments. Truncated progress-only markup is discarded;
/// other truncated CLIXML without recoverable Error text keeps the raw
/// remainder so a real failure is not silenced.
fn replace_clixml_documents(text: &str) -> String {
    const HEADER: &str = "#< CLIXML";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(HEADER) {
        out.push_str(&rest[..start]);
        let doc_tail = &rest[start + HEADER.len()..];
        match doc_tail.find("</Objs>") {
            Some(end_rel) => {
                let doc_end = HEADER.len() + end_rel + "</Objs>".len();
                let doc = &rest[start..start + doc_end];
                let messages = extract_clixml_messages(doc);
                if !messages.is_empty() {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&messages);
                }
                rest = &rest[start + doc_end..];
            }
            None => {
                // Truncated: try to recover Error text; drop only clear progress.
                let doc = &rest[start..];
                let messages = extract_clixml_messages(doc);
                if !messages.is_empty() {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&messages);
                } else if !is_progress_clixml(doc) {
                    out.push_str(doc);
                }
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// True when a CLIXML blob is a ProgressRecord stream (module first-use /
/// Write-Progress), not an error document.
pub fn is_progress_clixml(doc: &str) -> bool {
    doc.contains("S=\"progress\"") || doc.contains("S='progress'")
}

/// Pull the human-readable messages out of a CLIXML stderr blob. Each native
/// stderr / error-record line arrives as `<S S="Error">text</S>`; the text is
/// XML-escaped (`&amp;`, `&gt;`) and PowerShell char-escaped (`_x000D_`,
/// `_x000A_` for CR/LF), so both are decoded and newlines normalized before
/// the messages are joined.
///
/// An unfinished trailing `<S S="Error">…` (truncated capture, no `</S>`) keeps
/// its inner text so a mid-document cap does not wipe the only failure reason.
/// Documents with no Error segments (ProgressRecords, empty wrappers) return
/// an empty string — never markup stripping that concatenates type names into
/// a fake error.
fn extract_clixml_messages(text: &str) -> String {
    const TAG: &str = "<S S=\"Error\">";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(TAG) {
        let inner_start = start + TAG.len();
        match rest[inner_start..].find("</S>") {
            Some(end_rel) => {
                let inner = &rest[inner_start..inner_start + end_rel];
                if !inner.trim().is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&clixml_unescape(inner));
                }
                rest = &rest[inner_start + end_rel + 4..];
            }
            None => {
                let inner = &rest[inner_start..];
                if !inner.trim().is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&clixml_unescape(inner));
                }
                break;
            }
        }
    }
    if out.is_empty() {
        String::new()
    } else {
        out.replace("\r\n", "\n").replace('\r', "\n")
    }
}

/// Decode a CLIXML message payload: PowerShell `_xHHHH_` char escapes first
/// (`_x000D_` = CR, `_x000A_` = LF), then XML entities via the shared
/// `haven_common::encoding::xml_unescape` (`&amp;` last so a literal
/// `&amp;lt;` cannot be double-unescaped into `<`).
fn clixml_unescape(text: &str) -> String {
    let text = decode_x_escapes(text);
    haven_common::encoding::xml_unescape(&text)
}

/// Decode PowerShell's `_xHHHH_` character escapes (`_x000D_`, `_x000A_`,
/// `_x001B_`, …) to their code points. Any other text passes through.
fn decode_x_escapes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '_'
            && i + 6 < chars.len()
            && chars[i + 1] == 'x'
            && chars[i + 2..i + 6].iter().all(|c| c.is_ascii_hexdigit())
            && chars[i + 6] == '_'
        {
            let hex: String = chars[i + 2..i + 6].iter().collect();
            if let Ok(v) = u32::from_str_radix(&hex, 16) {
                out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
                i += 7;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Remove ANSI escape sequences (CSI, `ESC[…`) that pwsh 7 emits when it
/// renders error records with `$PSStyle` colors into a pipe.
fn strip_ansi_escapes(text: &str) -> String {
    // Most output has no escapes: return the input unchanged (memchr fast
    // path) instead of allocating a full-size copy on every shell command.
    if !text.contains('\u{1b}') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameter bytes (0x30–0x3F), then the final byte (0x40–0x7E).
            while let Some(&n) = chars.peek() {
                if ('\u{30}'..='\u{3f}').contains(&n) {
                    chars.next();
                } else {
                    break;
                }
            }
            if let Some(&n) = chars.peek()
                && ('\u{40}'..='\u{7e}').contains(&n)
            {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// True when a trimmed PowerShell output line is error-record formatting that
/// should be dropped rather than surfaced to the model.
fn is_powershell_noise_line(trimmed: &str) -> bool {
    trimmed.is_empty()
        || trimmed == "NativeCommandError"
        || trimmed.starts_with("At line:")
        // Localized position header (e.g. zh-CN: "所在位置 行:1 字符: 77").
        || trimmed.starts_with("所在位置")
        || trimmed == "+"
        || trimmed.starts_with("+ ")
        || trimmed.starts_with("+~")
        || trimmed.starts_with("~")
        || trimmed.starts_with("+ CategoryInfo")
        || trimmed.starts_with("+ FullyQualifiedErrorId")
        || trimmed.starts_with("CategoryInfo")
        || trimmed.starts_with("FullyQualifiedErrorId")
}

/// Condense a failed command's captured output into a short, readable reason:
/// progress-bar/spinner lines are dropped, only the last few meaningful lines
/// are kept, and the result is capped at `max_chars`. Used for the failed
/// action's `error_reason` and the foreground shell tool's error text, so a
/// multi-KB progress dump cannot drown the actual error (e.g. a 416 from a
/// failed download).
pub fn summarize_error(text: &str, max_chars: usize) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Progress/spinner lines: carriage-return redraws (mid-line `\r`),
        // bare percentages, and bar glyphs carry no diagnostic value. The
        // trailing `\r` of a Windows CRLF line is trimmed above and ignored.
        if line.contains('\r') {
            continue;
        }
        if trimmed.ends_with('%') && trimmed.len() < 16 {
            continue;
        }
        lines.push(line);
    }
    let start = lines.len().saturating_sub(12);
    let mut out = lines[start..].join("\n");
    let count = out.chars().count();
    if count > max_chars {
        let cutoff = out.floor_char_boundary(max_chars);
        out = format!("{}[... {} chars omitted]", &out[..cutoff], count - cutoff);
    }
    out
}

/// Append a short diagnostic hint when a failed command output matches a
/// common Windows trap (PowerShell aliases, execution policy, cmd-only
/// syntax, missing commands). Returns the original text plus hint lines so
/// the agent can fix the cause instead of blind-retrying.
#[cfg(windows)]
pub fn append_windows_diagnostics(shell: &str, command: &str, text: &str) -> String {
    let lower = text.to_lowercase();
    let cmd_lower = command.to_lowercase();
    let mut hints: Vec<&str> = Vec::new();

    if shell == "powershell" || shell == "pwsh" {
        // `curl` is an alias for Invoke-WebRequest in PowerShell; the user
        // almost always wants the real curl (curl.exe).
        if cmd_lower.contains("curl ") && !cmd_lower.contains("curl.exe") {
            hints.push(
                "PowerShell's `curl` is an alias for Invoke-WebRequest — use `curl.exe` (e.g. curl.exe -L <url>) for real curl behavior.",
            );
        }
        // `&&` is a parse error in PowerShell ("The token '&&' is not a valid
        // statement separator" / 标记“&&”不是此版本中的有效语句分隔符).
        if lower.contains("not a valid statement separator") || lower.contains("有效语句分隔符")
        {
            hints.push("`&&` is not valid in PowerShell — use `;` instead (or pass shell: cmd).");
        }
        // .ps1 scripts are blocked by the execution policy.
        if lower.contains("execution policy") || lower.contains("running scripts is disabled") {
            hints.push(
                "Windows script execution policy blocked a .ps1 — use the .cmd wrapper (e.g. npm.cmd instead of npm) or run: Set-ExecutionPolicy -Scope Process Bypass",
            );
        }
        if lower.contains("无法加载模块") || lower.contains("cannot load module") {
            hints.push(
                "A PowerShell module failed to load (e.g. Expand-Archive) — check the module path or install it with Install-Module.",
            );
        }
    } else {
        // cmd: Chinese Windows reports plain syntax errors with no detail.
        if lower.contains("语法不正确") {
            hints.push(
                "cmd reports a syntax error — check quoting and escaping: use double quotes around paths with spaces and escape & with ^ inside cmd.",
            );
        }
    }
    if lower.contains("禁止运行脚本") || lower.contains("执行策略") {
        hints.push(
            "Windows 脚本执行策略拦截了 .ps1 —— 改用 .cmd 包装（如 npm.cmd）或先执行 Set-ExecutionPolicy -Scope Process Bypass",
        );
    }
    if lower.contains("不是内部或外部命令")
        || lower.contains("not recognized as an internal or external command")
    {
        hints.push(
            "Command not found — check PATH or use the full path (e.g. C:\\Users\\<name>\\AppData\\Roaming\\npm\\npm.cmd).",
        );
    }
    if lower.contains("%1 不是有效的 win32") || lower.contains("not a valid win32 application")
    {
        hints.push(
            "'not a valid Win32 application' usually means a script without a launcher — invoke it via its interpreter (node/python) with the full script path, or use its .cmd wrapper.",
        );
    }
    if hints.is_empty() {
        text.to_string()
    } else {
        format!("{}\n\n[Windows trap?] {}", text, hints.join("\n"))
    }
}

#[cfg(not(windows))]
pub fn append_windows_diagnostics(_shell: &str, _command: &str, text: &str) -> String {
    text.to_string()
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
