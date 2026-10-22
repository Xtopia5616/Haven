//! Optional adapter for voidtools' Everything command line interface (`es.exe`).
//!
//! The upstream executable is user supplied and is never bundled by Haven.

use super::file_search::{FileSearchMode, SearchRequest};
use crate::security::FileReadPolicy;
use crate::{ToolExecutionContext, ToolResult, everything_config::EverythingConfigSource};
use haven_common::config::{MAX_SEARCH_RESULT_BYTES, MAX_SEARCH_ROOT_UTF16_UNITS};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const MAX_EVERYTHING_CONCURRENT: usize = 2;
const MAX_STDOUT_BYTES: usize = MAX_SEARCH_RESULT_BYTES;
const MAX_STDERR_BYTES: usize = 16 * 1024;
const EVERYTHING_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub(super) struct EverythingSearch {
    config: EverythingConfigSource,
    slots: std::sync::Arc<Semaphore>,
}

impl Default for EverythingSearch {
    fn default() -> Self {
        Self::new(EverythingConfigSource::default())
    }
}

impl EverythingSearch {
    pub(super) fn new(config: EverythingConfigSource) -> Self {
        Self {
            config,
            slots: std::sync::Arc::new(Semaphore::new(MAX_EVERYTHING_CONCURRENT)),
        }
    }

    pub(super) fn is_enabled(&self) -> anyhow::Result<bool> {
        Ok(self.config.get()?.enabled)
    }

    pub(super) async fn search_request(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
        policy: FileReadPolicy,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        if request.mode != FileSearchMode::Filename {
            anyhow::bail!("Everything supports filename search only");
        }
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        if request.root.trim().is_empty()
            || request.root.encode_utf16().count() > MAX_SEARCH_ROOT_UTF16_UNITS
        {
            anyhow::bail!("search root is invalid or exceeds the supported path length");
        }
        let config = self.config.get()?;
        if !config.enabled {
            anyhow::bail!("Everything search is disabled");
        }
        let executable = config
            .command
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("configure everything.command to the ES executable path")
            })?;
        let max_results = request.max_results.clamp(1, 1_000);
        let root = PathBuf::from(&request.root);
        let root_metadata = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
            result = tokio::fs::metadata(&root) => result.map_err(|_| anyhow::anyhow!("search root is inaccessible"))?,
        };
        if !root_metadata.is_dir() && !root_metadata.is_file() {
            anyhow::bail!("search root is not a file or directory");
        }
        if !policy.open_metadata(&root).is_ok() {
            anyhow::bail!("search root is outside the active file access policy");
        }
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
            permit = self.slots.clone().acquire_owned() => permit.map_err(|_| anyhow::anyhow!("Everything search capacity is unavailable"))?,
        };
        let execution_permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
            permit = context.acquire_everything_process_start_permit() => {
                permit.map_err(crate::security::execution_policy_denied)?
            }
        };
        let query = request.pattern.clone();
        let root_arg = if root_metadata.is_file() {
            root.parent()
                .unwrap_or(&root)
                .to_string_lossy()
                .into_owned()
        } else {
            root.to_string_lossy().into_owned()
        };
        let mut command = Command::new(executable);
        command
            .env_clear()
            .envs(
                haven_platform::process_environment::filter_runtime_baseline_environment(
                    std::env::vars_os(),
                ),
            )
            .arg("-json")
            .arg("-full-path-and-name")
            .arg("-no-folder-append-path-separator")
            .arg("-no-highlight")
            .arg("-path")
            .arg(root_arg)
            .arg("-n")
            .arg((max_results + 1).to_string())
            .arg("-timeout")
            .arg("5000")
            .arg("-search")
            .arg(query)
            .arg("-no-result-error")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let containment = haven_platform::process_containment::ProcessContainment::new()?;
        #[cfg(windows)]
        containment.prepare_command(command.as_std_mut(), crate::CREATE_NO_WINDOW);
        #[cfg(not(windows))]
        containment.prepare_command(command.as_std_mut(), 0);
        let spawned = {
            let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
            command.spawn()
        };
        let mut child = spawned.map_err(|_| {
            anyhow::anyhow!("failed to start Everything CLI; check everything.command")
        })?;
        let pid = child
            .id()
            .ok_or_else(|| anyhow::anyhow!("Everything CLI did not expose a process id"))?;
        #[cfg(windows)]
        let attach = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("Everything CLI process handle is unavailable"))
            .and_then(|handle| containment.attach_and_resume(pid, handle));
        #[cfg(not(windows))]
        let attach = containment.attach_and_resume(pid, ());
        if attach.is_err() {
            drop(execution_permit);
            let _ = child.kill().await;
            anyhow::bail!("could not contain the Everything CLI process");
        }
        drop(execution_permit);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Everything output is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Everything diagnostics are unavailable"))?;
        let stdout_task = tokio::spawn(read_bounded(stdout, MAX_STDOUT_BYTES));
        let stderr_task = tokio::spawn(read_bounded(stderr, MAX_STDERR_BYTES));
        let status = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                anyhow::bail!("cancelled")
            }
            result = tokio::time::timeout(EVERYTHING_TIMEOUT, child.wait()) => match result {
                Ok(result) => result.map_err(|_| anyhow::anyhow!("Everything search process failed"))?,
                Err(_) => {
                    let _ = child.kill().await;
                    anyhow::bail!("Everything search timed out")
                }
            }
        };
        let (stdout, stdout_overflow) = stdout_task.await??;
        let (_stderr, stderr_overflow) = stderr_task.await??;
        drop(permit);
        if stdout_overflow || stderr_overflow {
            anyhow::bail!("Everything search output exceeded its safety limit");
        }
        if !status.success() && status.code() != Some(9) {
            anyhow::bail!(match status.code() {
                Some(8) => "Everything is not running or its IPC interface is unavailable",
                Some(7) => "Everything rejected the search request",
                _ => "Everything search failed",
            });
        }
        let response = if status.code() == Some(9) && stdout.is_empty() {
            Vec::new()
        } else {
            parse_entries(&stdout)?
        };
        let provider_result_count = response.len();
        let mut results = Vec::new();
        let mut rejected = 0usize;
        for entry in response {
            let path = entry.path.or(entry.filename).unwrap_or_default();
            if path.is_empty() {
                rejected += 1;
                continue;
            }
            let path = PathBuf::from(&path);
            if !path_is_within_root(&path, &root) {
                rejected += 1;
                continue;
            }
            // Validate current metadata under the same file-read allowlist as
            // native search. Everything's index is not an authorization source.
            match policy.open_metadata(&path) {
                Ok(file) => {
                    let is_dir = file.metadata()?.is_dir();
                    results.push(serde_json::json!({
                        "path": path,
                        "match_reason": "filename",
                        "kind": if is_dir { "directory" } else { "file" },
                    }));
                }
                Err(_) => rejected += 1,
            }
        }
        results.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        results.dedup_by(|a, b| a["path"] == b["path"]);
        let has_more = provider_result_count > max_results;
        results.truncate(max_results);
        let complete = !has_more && rejected == 0;
        let output = serde_json::json!({
            "results": results,
            "count": results.len(),
            "mode": "filename",
            "root": request.root,
            "pattern": request.pattern,
            "has_more": has_more,
            "search_complete": complete,
            "io_error_count": rejected,
            "search_backend": "everything",
            "hint": if !complete { Some("Everything results may be incomplete because paths were rejected by the active file policy or the result cap was reached." ) } else { None },
        });
        Ok(ToolResult::from_output(output, has_more))
    }
}

#[derive(Deserialize)]
struct EverythingEntry {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    filename: Option<String>,
}

fn path_is_within_root(path: &Path, root: &Path) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(root) = std::fs::canonicalize(root) else {
        return false;
    };
    path.starts_with(root)
}

fn parse_entries(bytes: &[u8]) -> anyhow::Result<Vec<EverythingEntry>> {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(_) => {
            let mut entries = Vec::new();
            for line in bytes.split(|byte| *byte == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                entries.push(
                    serde_json::from_slice::<serde_json::Value>(line)
                        .map_err(|_| anyhow::anyhow!("Everything returned invalid JSON output"))?,
                );
            }
            serde_json::Value::Array(entries)
        }
    };
    let values = match value {
        serde_json::Value::Array(entries) => entries,
        serde_json::Value::Object(_) => vec![value],
        _ => anyhow::bail!("Everything returned an unsupported JSON shape"),
    };
    values
        .into_iter()
        .map(|entry| {
            serde_json::from_value(entry)
                .map_err(|_| anyhow::anyhow!("Everything returned a malformed result entry"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{MAX_STDOUT_BYTES, parse_entries, path_is_within_root, read_bounded};

    #[test]
    fn path_scope_uses_components_instead_of_prefixes() {
        let current = std::env::current_dir().unwrap();
        assert!(path_is_within_root(&current.join("Cargo.toml"), &current));
        assert!(!path_is_within_root(
            &current.join("..\\Haven-other\\Cargo.toml"),
            &current,
        ));
    }

    #[test]
    fn parses_array_and_newline_separated_json_records() {
        let array = parse_entries(br#"[{"path":"C:\\one.txt"}]"#).unwrap();
        assert_eq!(array.len(), 1);
        let lines = parse_entries(
            br#"{"path":"C:\\one.txt"}
{"path":"C:\\two.txt"}"#,
        )
        .unwrap();
        assert_eq!(lines.len(), 2);
    }

    #[tokio::test]
    async fn bounded_reader_rejects_overflow_without_retaining_it() {
        let input = vec![b'x'; MAX_STDOUT_BYTES + 10];
        let (retained, overflow) = read_bounded(input.as_slice(), MAX_STDOUT_BYTES)
            .await
            .unwrap();
        assert!(overflow);
        assert_eq!(retained.len(), MAX_STDOUT_BYTES);
    }
}

async fn read_bounded<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    cap: usize,
) -> anyhow::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut overflow = false;
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = cap.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..read.min(remaining)]);
        overflow |= read > remaining;
        if overflow {
            // Drain only through the pipe; retained memory stays bounded.
            while reader.read(&mut chunk).await? != 0 {}
            break;
        }
    }
    Ok((output, overflow))
}
