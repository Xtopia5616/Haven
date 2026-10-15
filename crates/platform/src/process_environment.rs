//! Minimal ambient environment inherited by Haven-managed runtime subprocesses.
//!
//! This is environment hygiene, not a process sandbox: children still run as
//! the current user and can access whatever files and system resources that
//! user can access. Callers may add operation-specific values after filtering.

use std::ffi::{OsStr, OsString};

const BASELINE_ENVIRONMENT_KEYS: &[&str] = &["PATH", "SYSTEMROOT", "TEMP", "TMP"];

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

    #[cfg(windows)]
    #[test]
    fn runtime_baseline_keys_are_case_insensitive_on_windows() {
        assert!(is_runtime_baseline_variable(OsStr::new("Path")));
        assert!(is_runtime_baseline_variable(OsStr::new("ComSpec")));
        assert!(!is_runtime_baseline_variable(OsStr::new("OpenAI_API_KEY")));
    }
}
