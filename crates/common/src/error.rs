//! Error text normalization shared by all process boundaries.

const MAX_PUBLIC_ERROR_LENGTH: usize = 240;

/// Convert an arbitrary diagnostic into a bounded, single-line message that is
/// safe to put in a UI, notification, IPC event, or ordinary application log.
/// This is intentionally conservative because errors from HTTP clients and
/// external processes may include URLs, local paths, credentials, or output
/// bodies.
pub fn sanitize_error_text(raw: &str) -> String {
    let normalized = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    let normalized = collapse_whitespace(&normalized);
    let normalized = redact_key_value_segments(&normalized);
    let normalized = redact_prefixed_tokens(&normalized);
    let normalized = redact_windows_paths(&normalized);
    truncate_chars(&normalized, MAX_PUBLIC_ERROR_LENGTH)
}

fn collapse_whitespace(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

fn is_boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn redact_key_value_segments(value: &str) -> String {
    const KEYS: &[&str] = &[
        "client_secret",
        "access_token",
        "api_key",
        "api-key",
        "apikey",
        "authorization",
        "password",
        "secret",
        "token",
        "key",
    ];

    let chars = value.chars().collect::<Vec<_>>();
    let lower = chars
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;

    while i < chars.len() {
        let matched = KEYS.iter().find_map(|key| {
            let key_chars = key.chars().collect::<Vec<_>>();
            let end = i + key_chars.len();
            if end > chars.len()
                || !is_boundary(i.checked_sub(1).and_then(|index| lower.get(index).copied()))
                || lower[i..end] != key_chars[..]
            {
                return None;
            }
            Some(key_chars.len())
        });

        let Some(key_len) = matched else {
            out.push(chars[i]);
            i += 1;
            continue;
        };

        let mut value_start = i + key_len;
        while value_start < chars.len() && chars[value_start].is_whitespace() {
            value_start += 1;
        }
        if value_start >= chars.len() || !matches!(chars[value_start], '=' | ':') {
            out.extend(chars[i..i + key_len].iter().copied());
            i += key_len;
            continue;
        }
        value_start += 1;
        while value_start < chars.len() && chars[value_start].is_whitespace() {
            value_start += 1;
        }
        let mut end = value_start;
        while end < chars.len()
            && !matches!(
                chars[end],
                '&' | ' ' | '\t' | '\r' | '\n' | ',' | ';' | '"' | '\'' | ')' | ']' | '}' | '#'
            )
        {
            end += 1;
        }

        out.extend(chars[i..value_start].iter().copied());
        if value_start < end {
            out.push_str("[REDACTED]");
        }
        i = end;
    }
    out
}

fn redact_prefixed_tokens(value: &str) -> String {
    const PREFIXES: &[&str] = &["bearer ", "sk-", "gsk_", "AIza", "xai-", "AKIA"];
    let chars = value.chars().collect::<Vec<_>>();
    let lower = chars
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;

    while i < chars.len() {
        let matched = PREFIXES.iter().find_map(|prefix| {
            let prefix_chars = prefix
                .chars()
                .map(|c| c.to_ascii_lowercase())
                .collect::<Vec<_>>();
            let end = i + prefix_chars.len();
            if end <= chars.len()
                && is_boundary(i.checked_sub(1).and_then(|index| lower.get(index).copied()))
                && lower[i..end] == prefix_chars[..]
            {
                Some(prefix_chars.len())
            } else {
                None
            }
        });
        let Some(prefix_len) = matched else {
            out.push(chars[i]);
            i += 1;
            continue;
        };

        let mut end = i + prefix_len;
        while end < chars.len()
            && !matches!(
                chars[end],
                ' ' | '\t' | '\r' | '\n' | ',' | ';' | '"' | '\'' | ')' | ']' | '}'
            )
        {
            end += 1;
        }
        out.extend(chars[i..i + prefix_len].iter().copied());
        if end > i + prefix_len {
            out.push_str("[REDACTED]");
        }
        i = end;
    }
    out
}

fn redact_windows_paths(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        let drive_path = i + 2 < chars.len()
            && chars[i].is_ascii_alphabetic()
            && chars[i + 1] == ':'
            && matches!(chars[i + 2], '\\' | '/');
        let unc_path = i + 1 < chars.len() && chars[i] == '\\' && chars[i + 1] == '\\';
        if (drive_path || unc_path)
            && is_boundary(i.checked_sub(1).and_then(|index| chars.get(index).copied()))
        {
            let mut end = i;
            while end < chars.len()
                && !matches!(
                    chars[end],
                    ' ' | '\t' | '\r' | '\n' | ',' | ';' | '"' | '\'' | ')' | ']' | '}'
                )
            {
                end += 1;
            }
            out.push_str("[PATH]");
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secrets_and_windows_paths() {
        let value = sanitize_error_text(
            "request failed https://example.test/?api_key=sk-secret&client_secret=topsecret at C:\\Users\\olive\\haven.db",
        );
        assert!(!value.contains("sk-secret"));
        assert!(!value.contains("topsecret"));
        assert!(!value.contains("C:\\Users"));
        assert!(value.contains("[REDACTED]"));
        assert!(value.contains("[PATH]"));
    }

    #[test]
    fn truncates_without_splitting_utf8() {
        let value = sanitize_error_text(&"错误".repeat(200));
        assert!(value.chars().count() <= MAX_PUBLIC_ERROR_LENGTH);
        assert!(value.ends_with('…'));
    }
}
