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
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const MAX_EVERYTHING_CONCURRENT: usize = 2;
const MAX_STDOUT_BYTES: usize = MAX_SEARCH_RESULT_BYTES;
const MAX_STDERR_BYTES: usize = 16 * 1024;
const EVERYTHING_TIMEOUT: Duration = Duration::from_secs(15);
type EverythingScratch = crate::shell_runtime::ProcessScratchDirectory;

enum EverythingProcess {
    Host {
        child: Child,
        _containment: haven_platform::process_containment::ProcessContainment,
    },
    Mxc(haven_platform::sandboxed_process::SandboxedProcess),
}

impl EverythingProcess {
    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        match self {
            Self::Host { child, .. } => child
                .stdout
                .take()
                .map(|stream| Box::new(stream) as Box<dyn AsyncRead + Send + Unpin>),
            Self::Mxc(process) => process
                .take_stdout()
                .map(|stream| Box::new(stream) as Box<dyn AsyncRead + Send + Unpin>),
        }
    }

    fn take_stderr(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        match self {
            Self::Host { child, .. } => child
                .stderr
                .take()
                .map(|stream| Box::new(stream) as Box<dyn AsyncRead + Send + Unpin>),
            Self::Mxc(process) => process
                .take_stderr()
                .map(|stream| Box::new(stream) as Box<dyn AsyncRead + Send + Unpin>),
        }
    }

    async fn wait(&mut self) -> std::io::Result<i32> {
        match self {
            Self::Host { child, .. } => child
                .wait()
                .await
                .map(|status| status.code().unwrap_or(i32::MIN)),
            Self::Mxc(process) => process.wait().await.map(|exit| exit.exit_code),
        }
    }

    async fn terminate(&mut self) {
        match self {
            Self::Host { child, .. } => {
                let _ = child.kill().await;
            }
            Self::Mxc(process) => {
                let _ = process.kill().await;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), self.wait()).await;
    }
}

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
        let check_root = root.clone();
        let check_policy = policy.clone();
        let root_metadata = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
            result = tokio::task::spawn_blocking(move || check_policy.open_metadata(&check_root).and_then(|file| file.metadata())) => {
                result?.map_err(|_| anyhow::anyhow!("search root is outside the active file access policy"))?
            },
        };
        if !root_metadata.is_dir() && !root_metadata.is_file() {
            anyhow::bail!("search root is not a file or directory");
        }
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
            permit = self.slots.clone().acquire_owned() => permit.map_err(|_| anyhow::anyhow!("Everything search capacity is unavailable"))?,
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
        let scratch = EverythingScratch::create_everything().await?;
        #[cfg(windows)]
        let mut runtime_read_roots =
            crate::process_sandbox::runtime_dependency_roots(command.as_std());
        #[cfg(not(windows))]
        let mut runtime_read_roots = Vec::new();
        let program_path = PathBuf::from(command.as_std().get_program());
        if program_path.is_absolute()
            && let Some(parent) = program_path.parent()
            && haven_platform::filesystem::DirectoryCapability::open(parent).is_ok()
        {
            runtime_read_roots.push(parent.to_path_buf());
        }
        let execution_permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
            permit = context.acquire_everything_process_start_permit(
                crate::security::EverythingProcessStartRequest {
                    search_root: root.clone(),
                    scratch_directory: scratch.path().to_path_buf(),
                    runtime_read_roots,
                }
            ) => permit.map_err(crate::security::execution_policy_denied)?,
        };

        let mut process = if let Some(policy) = execution_permit.sandboxed_process_policy() {
            command
                .current_dir(scratch.path())
                .env("TEMP", scratch.path())
                .env("TMP", scratch.path())
                .env("TMPDIR", scratch.path());
            let request = haven_platform::sandboxed_process::SandboxedProcessRequest::from_command(
                command.as_std(),
                policy,
                false,
            )?;
            let process = haven_platform::sandboxed_process::SandboxedProcess::spawn(
                request,
                policy.isolation_mode,
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!("could not start Everything CLI in the workspace sandbox")
            })?;
            drop(execution_permit);
            EverythingProcess::Mxc(process)
        } else {
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
                .ok_or_else(|| {
                    std::io::Error::other("Everything CLI process handle is unavailable")
                })
                .and_then(|handle| containment.attach_and_resume(pid, handle));
            #[cfg(not(windows))]
            let attach = containment.attach_and_resume(pid, ());
            if attach.is_err() {
                drop(execution_permit);
                let _ = child.kill().await;
                anyhow::bail!("could not contain the Everything CLI process");
            }
            drop(execution_permit);
            EverythingProcess::Host {
                child,
                _containment: containment,
            }
        };
        let stdout = process
            .take_stdout()
            .ok_or_else(|| anyhow::anyhow!("Everything output is unavailable"))?;
        let stderr = process
            .take_stderr()
            .ok_or_else(|| anyhow::anyhow!("Everything diagnostics are unavailable"))?;
        let stdout_task = tokio::spawn(read_bounded(stdout, MAX_STDOUT_BYTES));
        let stderr_task = tokio::spawn(read_bounded(stderr, MAX_STDERR_BYTES));
        let capacity_watch = async {
            loop {
                scratch.ensure_within_capacity().await?;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        };
        let status = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                process.terminate().await;
                anyhow::bail!("cancelled")
            }
            result = capacity_watch => {
                process.terminate().await;
                result?;
                anyhow::bail!("Everything scratch capacity monitor stopped")
            }
            result = tokio::time::timeout(EVERYTHING_TIMEOUT, process.wait()) => match result {
                Ok(result) => result.map_err(|_| anyhow::anyhow!("Everything search process failed"))?,
                Err(_) => {
                    process.terminate().await;
                    anyhow::bail!("Everything search timed out")
                }
            }
        };
        let (stdout, stdout_overflow) = stdout_task.await??;
        let (_stderr, stderr_overflow) = stderr_task.await??;
        if stdout_overflow || stderr_overflow {
            anyhow::bail!("Everything search output exceeded its safety limit");
        }
        if status != 0 && status != 9 {
            anyhow::bail!(match status {
                8 => "Everything is not running or its IPC interface is unavailable",
                7 => "Everything rejected the search request",
                _ => "Everything search failed",
            });
        }
        let response = if status == 9 && stdout.is_empty() {
            Vec::new()
        } else {
            parse_entries(&stdout)?
        };
        let provider_result_count = response.len();
        let index_coverage_unknown = provider_result_count == 0;
        let mut results = Vec::new();
        let mut rejected = 0usize;
        // Provider paths are only candidates. Resolve them against the same
        // handle-based traversal and rule loading used by native search.
        let wanted: std::collections::HashSet<_> = response
            .iter()
            .filter_map(|entry| entry.path.as_ref().or(entry.filename.as_ref()))
            .map(|path| search_path_key(Path::new(path)))
            .collect();
        let scan_root = root.clone();
        let scan_cancel = cancel.clone();
        let depth = request.max_depth;
        let hidden = request.ignore_hidden;
        let (accepted, walk) = tokio::task::spawn_blocking(move || {
            filter_candidates(&scan_root, depth, hidden, &policy, &scan_cancel, &wanted)
        })
        .await?;
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        drop(permit);
        for entry in response {
            let path = entry.path.or(entry.filename).unwrap_or_default();
            if let Some((path, is_dir)) = accepted.get(&search_path_key(Path::new(&path))) {
                results.push(serde_json::json!({"path": path, "match_reason": "filename", "kind": if *is_dir { "directory" } else { "file" }}));
            } else {
                rejected += 1;
            }
        }
        rejected += walk.errors + usize::from(walk.limit_hit);
        results.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        results.dedup_by(|a, b| a["path"] == b["path"]);
        let has_more = provider_result_count > max_results;
        results.truncate(max_results);
        let complete = !has_more && rejected == 0 && !index_coverage_unknown;
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
            "index_coverage": if index_coverage_unknown { "unknown" } else { "matches_returned" },
            "hint": if index_coverage_unknown { Some("Everything returned no matches, but its index may still be loading or may not include this directory. Check Everything's indexing settings and wait for initial indexing to finish." ) } else if !complete { Some("Everything results may be incomplete because candidates were filtered, paths were inaccessible, or a search limit was reached." ) } else { None },
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

fn search_path_key(path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(windows)]
    {
        path.replace('/', "\\").to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.into_owned()
    }
}

fn filter_candidates(
    root: &Path,
    depth: usize,
    hidden: bool,
    policy: &FileReadPolicy,
    cancel: &CancellationToken,
    wanted: &std::collections::HashSet<String>,
) -> (
    std::collections::HashMap<String, (PathBuf, bool)>,
    super::search_walk::WalkResult,
) {
    let mut accepted = std::collections::HashMap::new();
    let walk = super::search_walk::walk(root, depth, hidden, policy, cancel, 100_000, |entry| {
        let key = search_path_key(&entry.path);
        if wanted.contains(&key)
            && let Ok(metadata) = entry.file.metadata()
        {
            accepted.insert(key, (entry.path, metadata.is_dir()));
        }
        true
    });
    (accepted, walk)
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

#[cfg(test)]
mod tests {
    use super::{
        MAX_STDOUT_BYTES, filter_candidates, parse_entries, read_bounded, search_path_key,
    };

    #[test]
    fn candidate_filter_uses_handle_traversal_and_ignore_rules() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".ignore"), "ignored.txt\n").unwrap();
        let visible = temp.path().join("visible.txt");
        let ignored = temp.path().join("ignored.txt");
        std::fs::write(&visible, "visible").unwrap();
        std::fs::write(&ignored, "ignored").unwrap();
        let policy = crate::security::FileReadPolicy::for_test_roots(&[temp.path().to_path_buf()]);
        let wanted = [
            search_path_key(&visible),
            search_path_key(&ignored),
            search_path_key(&temp.path().join("../outside.txt")),
        ]
        .into_iter()
        .collect();
        let (accepted, walk) = filter_candidates(
            temp.path(),
            0,
            true,
            &policy,
            &tokio_util::sync::CancellationToken::new(),
            &wanted,
        );
        assert_eq!(walk.errors, 0);
        assert!(accepted.contains_key(&search_path_key(&visible)));
        assert!(!accepted.contains_key(&search_path_key(&ignored)));
        assert_eq!(accepted.len(), 1);
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

    #[test]
    fn empty_provider_result_is_not_a_complete_search() {
        let result_count = parse_entries(b"[]").unwrap().len();
        let index_coverage_unknown = result_count == 0;
        let complete = result_count <= 50 && !index_coverage_unknown;
        assert!(index_coverage_unknown);
        assert!(!complete);
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
