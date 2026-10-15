//! Minimal ambient environment inherited by Haven-managed runtime subprocesses.
//!
//! This is environment hygiene, not a process sandbox: children still run as
//! the current user and can access whatever files and system resources that
//! user can access. Callers may add operation-specific values after filtering.

use std::ffi::{OsStr, OsString};

const BASELINE_ENVIRONMENT_KEYS: &[&str] = &["PATH", "SYSTEMROOT", "TEMP", "TMP"];
const SHELL_CONTEXT_ENVIRONMENT_KEYS: &[&str] = &[
    "APPDATA",
    "COLORTERM",
    "HOME",
    "HOMEDRIVE",
    "HOMEPATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOCALAPPDATA",
    "LOGNAME",
    "NO_COLOR",
    "TERM",
    "TERM_PROGRAM",
    "TZ",
    "USER",
    "USERNAME",
    "USERPROFILE",
    "WINDIR",
];
const PROXY_ENVIRONMENT_KEYS: &[&str] = &[
    "ALL_PROXY",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "all_proxy",
    "http_proxy",
    "https_proxy",
    "no_proxy",
];

/// Whether a process environment variable belongs to the minimal runtime
/// baseline required to locate executables and create temporary files.
pub fn is_runtime_baseline_variable(key: &OsStr) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };

    BASELINE_ENVIRONMENT_KEYS.iter().any(|allowed| {
        #[cfg(windows)]
        {
            key.eq_ignore_ascii_case(allowed)
        }
        #[cfg(not(windows))]
        {
            key == *allowed
        }
    }) || cfg!(windows)
        && (key.eq_ignore_ascii_case("COMSPEC") || key.eq_ignore_ascii_case("PATHEXT"))
}

/// Keep only the runtime baseline from a supplied environment snapshot.
///
/// Supplying the iterator makes filtering deterministic in tests and lets
/// callers avoid changing the process-global environment.
pub fn filter_runtime_baseline_environment(
    environment: impl Iterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    environment
        .filter(|(key, _)| is_runtime_baseline_variable(key))
        .collect()
}

/// Keep the environment needed by an interactive user shell without passing
/// unrelated application credentials and runtime overrides to arbitrary
/// commands.
///
/// Explicit proxy variables are retained because users commonly configure
/// shell networking through them. This is environment hygiene only: commands
/// still run as the current user and are not isolated from files or networks.
pub fn filter_shell_environment(
    environment: impl Iterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    environment
        .filter(|(key, _)| {
            is_runtime_baseline_variable(key)
                || matches_environment_key(key, SHELL_CONTEXT_ENVIRONMENT_KEYS)
                || matches_environment_key(key, PROXY_ENVIRONMENT_KEYS)
        })
        .collect()
}

fn matches_environment_key(key: &OsStr, allowed_keys: &[&str]) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };
    allowed_keys.iter().any(|allowed| {
        #[cfg(windows)]
        {
            key.eq_ignore_ascii_case(allowed)
        }
        #[cfg(not(windows))]
        {
            key == *allowed
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_baseline_keeps_launch_requirements_and_drops_ambient_secrets() {
        let environment = [
            ("PATH", "C:/runtime"),
            ("SYSTEMROOT", "C:/Windows"),
            ("TEMP", "C:/Temp"),
            ("TMP", "C:/Temp"),
            ("COMSPEC", "C:/Windows/System32/cmd.exe"),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD"),
            ("OPENAI_API_KEY", "provider-secret"),
            ("AWS_SECRET_ACCESS_KEY", "cloud-secret"),
            ("HTTPS_PROXY", "https://user:secret@example.invalid"),
            ("PYTHONPATH", "C:/untrusted"),
            (
                "PIP_INDEX_URL",
                "https://user:secret@example.invalid/simple",
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()));

        let filtered = filter_runtime_baseline_environment(environment);
        let keys = filtered
            .iter()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        for key in ["PATH", "SYSTEMROOT", "TEMP", "TMP"] {
            assert!(keys.iter().any(|actual| actual == key));
        }
        assert_eq!(keys.iter().any(|key| key == "COMSPEC"), cfg!(windows));
        assert_eq!(keys.iter().any(|key| key == "PATHEXT"), cfg!(windows));
        for key in [
            "OPENAI_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "HTTPS_PROXY",
            "PYTHONPATH",
            "PIP_INDEX_URL",
        ] {
            assert!(!keys.iter().any(|actual| actual == key));
        }
    }

    #[test]
    fn shell_environment_keeps_user_context_and_proxy_but_drops_unrelated_values() {
        let environment = [
            ("PATH", "C:/runtime"),
            ("SYSTEMROOT", "C:/Windows"),
            ("TEMP", "C:/Temp"),
            ("USERPROFILE", "C:/Users/Alice"),
            ("APPDATA", "C:/Users/Alice/AppData/Roaming"),
            ("LANG", "zh_CN.UTF-8"),
            ("HTTPS_PROXY", "http://user:proxy-secret@127.0.0.1:8080"),
            ("OPENAI_API_KEY", "provider-secret"),
            ("AWS_SECRET_ACCESS_KEY", "cloud-secret"),
            ("PYTHONPATH", "C:/untrusted"),
            ("PIP_INDEX_URL", "https://packages.invalid/simple"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()));

        let filtered = filter_shell_environment(environment);
        let values = filtered
            .into_iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(values.get("PATH").map(String::as_str), Some("C:/runtime"));
        assert_eq!(
            values.get("USERPROFILE").map(String::as_str),
            Some("C:/Users/Alice")
        );
        assert_eq!(
            values.get("HTTPS_PROXY").map(String::as_str),
            Some("http://user:proxy-secret@127.0.0.1:8080")
        );
        for key in [
            "OPENAI_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "PYTHONPATH",
            "PIP_INDEX_URL",
        ] {
            assert!(
                !values.contains_key(key),
                "unexpected environment key {key}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn runtime_baseline_keys_are_case_insensitive_on_windows() {
        assert!(is_runtime_baseline_variable(OsStr::new("Path")));
        assert!(is_runtime_baseline_variable(OsStr::new("ComSpec")));
        assert!(!is_runtime_baseline_variable(OsStr::new("OpenAI_API_KEY")));
    }
}
