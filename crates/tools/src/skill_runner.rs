use crate::process::{CappedStreamRead, drain_stream_with_byte_cap};
use crate::tool_contract::ToolResult;
use haven_common::config::{
    MAX_SKILLS_EXEC_OUTPUT_LINES, MAX_SKILLS_EXEC_TIMEOUT_SECS, SkillsExecConfig,
};
use haven_common::encoding;
use haven_common::error::sanitize_error_text;
use haven_platform::process_containment::ProcessContainment;
use haven_skills::{Skill, VenvManager};
use serde_json::Value;
#[cfg(not(test))]
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Bound retained output independently for stdout and stderr. Both streams
/// continue draining after the cap so a child can never deadlock on a full
/// pipe merely because its result is too large for Haven to keep.
const MAX_SKILL_STREAM_BYTES: usize = 8 * 1024 * 1024;
const MAX_SKILL_SCRIPT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CONCURRENT_SKILL_EXECUTIONS: usize = 4;
#[cfg(not(test))]
const SKILL_SCRIPT_SNAPSHOT_DIRECTORY: &str = "skill-script-snapshots";
const SKILL_SCRIPT_SNAPSHOT_MAX_AGE: Duration =
    Duration::from_secs(MAX_SKILLS_EXEC_TIMEOUT_SECS + 60);

const SKILL_SCRIPT_LAUNCHER: &str = r#"
def _haven_run_skill_snapshot():
    import pathlib
    import sys

    snapshot_path, source_path = sys.argv[1:3]
    sys.argv = [source_path]
    source_dir = str(pathlib.Path(source_path).parent)
    if sys.path:
        sys.path[0] = source_dir
    else:
        sys.path.insert(0, source_dir)

    namespace = sys.modules["__main__"].__dict__
    namespace.update({
        "__file__": source_path,
        "__package__": None,
        "__cached__": None,
        "__spec__": None,
        "__loader__": None,
    })
    del namespace["_haven_run_skill_snapshot"]
    source = pathlib.Path(snapshot_path).read_bytes()
    exec(compile(source, source_path, "exec"), namespace, namespace)

_haven_run_skill_snapshot()
"#;

async fn stage_skill_script(
    source: std::fs::File,
    skill_name: &str,
    temp_root: &std::path::Path,
) -> anyhow::Result<tempfile::TempPath> {
    let metadata = source
        .metadata()
        .map_err(|_| anyhow::anyhow!("failed to inspect entry script for skill '{skill_name}'"))?;
    if !metadata.is_file() {
        anyhow::bail!("entry script for skill '{skill_name}' is not a file");
    }
    if metadata.len() > MAX_SKILL_SCRIPT_BYTES {
        anyhow::bail!(
            "entry script for skill '{skill_name}' exceeds the {MAX_SKILL_SCRIPT_BYTES}-byte limit"
        );
    }

    tokio::fs::create_dir_all(temp_root).await.map_err(|_| {
        anyhow::anyhow!("failed to prepare script snapshot for skill '{skill_name}'")
    })?;
    let (destination, snapshot_path) = tempfile::Builder::new()
        .prefix("haven-skill-script-")
        .suffix(".py")
        .tempfile_in(temp_root)
        .map_err(|_| anyhow::anyhow!("failed to prepare script snapshot for skill '{skill_name}'"))?
        .into_parts();
    let mut source = tokio::fs::File::from_std(source);
    let mut destination = tokio::fs::File::from_std(destination);
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = source
            .read(&mut buffer)
            .await
            .map_err(|_| anyhow::anyhow!("failed to read entry script for skill '{skill_name}'"))?;
        if read == 0 {
            break;
        }
        copied = copied.saturating_add(read as u64);
        if copied > MAX_SKILL_SCRIPT_BYTES {
            anyhow::bail!(
                "entry script for skill '{skill_name}' exceeds the {MAX_SKILL_SCRIPT_BYTES}-byte limit"
            );
        }
        destination.write_all(&buffer[..read]).await.map_err(|_| {
            anyhow::anyhow!("failed to stage entry script for skill '{skill_name}'")
        })?;
    }
    destination
        .flush()
        .await
        .map_err(|_| anyhow::anyhow!("failed to stage entry script for skill '{skill_name}'"))?;
    drop(destination);
    Ok(snapshot_path)
}

#[cfg(not(test))]
fn default_skill_script_snapshot_root() -> PathBuf {
    let runtime_temp_root = haven_common::config::default_runtime_temp_root();
    let snapshot_root = runtime_temp_root.join(SKILL_SCRIPT_SNAPSHOT_DIRECTORY);
    for directory in [&runtime_temp_root, &snapshot_root] {
        match cleanup_stale_skill_script_snapshots_at(
            directory,
            SystemTime::now(),
            SKILL_SCRIPT_SNAPSHOT_MAX_AGE,
        ) {
            Ok(0) => {}
            Ok(removed) => tracing::debug!(removed, "removed expired Skill script snapshots"),
            Err(error) => tracing::warn!(
                error = %sanitize_error_text(&error.to_string()),
                "failed to clean expired Skill script snapshots"
            ),
        }
    }
    snapshot_root
}

fn cleanup_stale_skill_script_snapshots_at(
    directory: &std::path::Path,
    now: SystemTime,
    max_age: Duration,
) -> std::io::Result<usize> {
    use haven_platform::filesystem::{DirectoryCapability, is_link_or_reparse_point};

    let capability = match DirectoryCapability::open(directory) {
        Ok(capability) => capability,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in std::fs::read_dir(capability.root())? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_text) = name.to_str() else {
            continue;
        };
        if !is_skill_script_snapshot_name(name_text) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || is_link_or_reparse_point(&metadata) {
            continue;
        }
        let stale = now
            .duration_since(metadata.modified()?)
            .is_ok_and(|age| age > max_age);
        if !stale {
            continue;
        }
        match capability.remove_file(std::path::Path::new(&name)) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(removed)
}

fn is_skill_script_snapshot_name(name: &str) -> bool {
    let Some(nonce) = name
        .strip_prefix("haven-skill-script-")
        .and_then(|name| name.strip_suffix(".py"))
    else {
        return false;
    };
    (6..=32).contains(&nonce.len()) && nonce.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

struct SkillProcessOutput {
    exit_status: std::process::ExitStatus,
    stdout: CappedStreamRead,
    stderr: CappedStreamRead,
}

/// Sandbox executor for skill scripts (M4-02).
///
/// Spawns a subprocess running the skill's entry script inside its isolated
/// venv, with stdin carrying the serialised `params` JSON, a clean environment,
/// and a wall-clock timeout.
#[derive(Clone)]
pub struct SkillRunner {
    venv: VenvManager,
    config: SkillsExecConfig,
    script_snapshot_root: std::path::PathBuf,
    execution_slots: std::sync::Arc<Semaphore>,
}

impl SkillRunner {
    pub fn new(venv: VenvManager, mut config: SkillsExecConfig) -> Self {
        config.timeout_secs = config.timeout_secs.clamp(1, MAX_SKILLS_EXEC_TIMEOUT_SECS);
        config.max_output_lines = config.max_output_lines.min(MAX_SKILLS_EXEC_OUTPUT_LINES);
        Self {
            venv,
            config,
            #[cfg(not(test))]
            script_snapshot_root: default_skill_script_snapshot_root(),
            // Tests must set an isolated root before execution; an empty path
            // fails closed instead of touching the process Temp directory.
            #[cfg(test)]
            script_snapshot_root: std::path::PathBuf::new(),
            execution_slots: std::sync::Arc::new(Semaphore::new(MAX_CONCURRENT_SKILL_EXECUTIONS)),
        }
    }

    /// The subprocess wall-clock timeout. Tool adapters use this to set an
    /// outer timeout with a small cleanup margin instead of racing the same
    /// deadline at two layers.
    pub fn timeout_secs(&self) -> u64 {
        self.config.timeout_secs
    }

    /// Execute a skill's script with the given parameters.
    pub async fn execute(
        &self,
        skill: &Skill,
        params: &Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_secs);
        let _execution_permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Ok(ToolResult::cancelled(format!("skill '{}' cancelled", skill.name())));
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("skill '{}' timed out after {}s", skill.name(), self.config.timeout_secs),
                ));
            }
            permit = std::sync::Arc::clone(&self.execution_slots).acquire_owned() => {
                permit.map_err(|_| anyhow::anyhow!("skill execution capacity is unavailable"))?
            }
        };
        let (entry, entry_file) = skill.open_entry_script().ok_or_else(|| {
            anyhow::anyhow!(
                "skill '{}' has no entry script; expected scripts/main.py or scripts/{}.py",
                skill.name(),
                skill.name()
            )
        })?;
        if !matches!(skill.language(), haven_skills::Language::Python) {
            anyhow::bail!(
                "unsupported language '{}' for skill '{}'",
                skill.language().as_str(),
                skill.name()
            );
        }

        // Snapshot the checked file before setup can run for a long time. The
        // open handle denies Windows write/delete sharing until the copy ends.
        let script_snapshot = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Ok(ToolResult::cancelled(format!("skill '{}' cancelled", skill.name())));
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("skill '{}' timed out after {}s", skill.name(), self.config.timeout_secs),
                ));
            }
            result = stage_skill_script(
                entry_file,
                skill.name(),
                &self.script_snapshot_root,
            ) => result?,
        };

        let python = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Ok(ToolResult::cancelled(format!("skill '{}' cancelled", skill.name())));
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("skill '{}' timed out after {}s", skill.name(), self.config.timeout_secs),
                ));
            }
            result = self.venv.ensure(skill.name(), skill.root()) => result?,
        };

        let work_dir = &self.config.work_dir;
        tokio::fs::create_dir_all(work_dir).await?;

        let input_json = serde_json::to_string(params)?;
        let pid_label = skill.name().to_string();

        let mut cmd = tokio::process::Command::new(&python);
        cmd.arg("-c")
            .arg(SKILL_SCRIPT_LAUNCHER)
            .arg(script_snapshot.as_os_str())
            .arg(&entry)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(work_dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env(
                "SYSTEMROOT",
                std::env::var("SYSTEMROOT").unwrap_or_default(),
            )
            .env("TEMP", std::env::var("TEMP").unwrap_or_default())
            .env("TMP", std::env::var("TMP").unwrap_or_default())
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1");

        #[cfg(windows)]
        {
            cmd.env("COMSPEC", std::env::var("COMSPEC").unwrap_or_default());
        }

        let containment = ProcessContainment::new().map_err(|error| {
            anyhow::anyhow!(
                "failed to create process containment for skill '{}': {}",
                pid_label,
                sanitize_error_text(&error.to_string())
            )
        })?;
        containment.prepare_command(cmd.as_std_mut(), 0);
        let mut child = cmd.kill_on_drop(true).spawn().map_err(|e| {
            anyhow::anyhow!(
                "failed to spawn skill '{}': {}",
                pid_label,
                sanitize_error_text(&e.to_string())
            )
        })?;
        let pid = child.id().ok_or_else(|| {
            anyhow::anyhow!("skill '{}' did not expose a child process id", pid_label)
        })?;
        #[cfg(windows)]
        let attach_result = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("skill child process handle is unavailable"))
            .and_then(|handle| containment.attach_and_resume(pid, handle));
        #[cfg(not(windows))]
        let attach_result = containment.attach_and_resume(pid, ());
        if let Err(error) = attach_result {
            let _ = child.kill().await;
            anyhow::bail!(
                "failed to attach skill '{}' to process containment: {}",
                pid_label,
                sanitize_error_text(&error.to_string())
            );
        }

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let max_lines = self.config.max_output_lines;

        // Feed stdin, drain both output pipes, and wait for process exit as one
        // deadline-bound operation. Waiting first can deadlock when a child
        // fills either OS pipe before it exits.
        let process_output = tokio::select! {
            result = async {
                use tokio::io::AsyncWriteExt;

                let write_input = async move {
                    if let Some(mut stdin) = stdin {
                        stdin.write_all(input_json.as_bytes()).await?;
                        stdin.shutdown().await?;
                    }
                    Ok::<(), std::io::Error>(())
                };
                let read_stdout = drain_stream_with_byte_cap(
                    stdout,
                    MAX_SKILL_STREAM_BYTES,
                    |_| {},
                );
                let read_stderr = drain_stream_with_byte_cap(
                    stderr,
                    MAX_SKILL_STREAM_BYTES,
                    |_| {},
                );
                let wait = child.wait();
                let (write_result, stdout, stderr, status) =
                    tokio::join!(write_input, read_stdout, read_stderr, wait);

                write_result.map_err(|error| {
                    anyhow::anyhow!(
                        "failed to write params to skill '{}': {}",
                        pid_label,
                        sanitize_error_text(&error.to_string())
                    )
                })?;
                let mut stdout = stdout;
                if let Some(error) = stdout.error.take() {
                    anyhow::bail!(
                        "failed to read stdout from skill '{}': {}",
                        pid_label,
                        sanitize_error_text(&error.to_string())
                    );
                }
                let mut stderr = stderr;
                if let Some(error) = stderr.error.take() {
                    anyhow::bail!(
                        "failed to read stderr from skill '{}': {}",
                        pid_label,
                        sanitize_error_text(&error.to_string())
                    );
                }
                let exit_status = status.map_err(|error| {
                    anyhow::anyhow!(
                        "skill '{}' wait error: {}",
                        pid_label,
                        sanitize_error_text(&error.to_string())
                    )
                })?;
                Ok::<_, anyhow::Error>(SkillProcessOutput {
                    exit_status,
                    stdout,
                    stderr,
                })
            } => result?,
            _ = cancel.cancelled() => {
                // Dropping the child and its containment guard terminates the
                // process tree; no detached pipe reader survives this return.
                return Ok(ToolResult::cancelled(format!(
                    "skill '{}' cancelled",
                    pid_label
                )));
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("skill '{}' timed out after {}s", pid_label, self.config.timeout_secs),
                ));
            }
        };

        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled(format!(
                "skill '{}' cancelled by user",
                pid_label
            )));
        }

        if process_output.stdout.overflowed || process_output.stderr.overflowed {
            let stdout_preview = encoding::decode_lossy(
                &process_output.stdout.bytes[..process_output.stdout.bytes.len().min(8 * 1024)],
            );
            let stderr_preview = sanitize_error_text(&encoding::decode_lossy(
                &process_output.stderr.bytes[..process_output.stderr.bytes.len().min(8 * 1024)],
            ));
            return Ok(ToolResult::failed(
                serde_json::json!({
                    "stdout_preview": stdout_preview,
                    "stderr_preview": stderr_preview,
                    "truncated": true,
                }),
                format!(
                    "skill '{}' output exceeded its limit ({} bytes per stream)",
                    pid_label, MAX_SKILL_STREAM_BYTES
                ),
            ));
        }

        let stdout = encoding::decode_lossy(&process_output.stdout.bytes);
        let stderr = encoding::decode_lossy(&process_output.stderr.bytes);
        let exit_code = process_output.exit_status.code().unwrap_or(-1);

        let out_line_count = stdout.lines().take(max_lines.saturating_add(1)).count();
        let err_line_count = stderr.lines().take(max_lines.saturating_add(1)).count();
        let out_lines: Vec<&str> = stdout.lines().take(max_lines).collect();
        let err_lines: Vec<&str> = stderr.lines().take(max_lines).collect();
        let out_text = out_lines.join("\n");
        let err_text = sanitize_error_text(&err_lines.join("\n"));

        let over_line_limit = out_line_count > max_lines || err_line_count > max_lines;
        if over_line_limit {
            return Ok(ToolResult::failed(
                serde_json::json!({
                    "stdout": out_text,
                    "stderr": err_text,
                    "truncated": true,
                }),
                format!(
                    "skill '{}' output exceeded its limit ({} lines per stream)",
                    pid_label, max_lines
                ),
            ));
        }

        if exit_code != 0 || !err_text.is_empty() {
            Ok(ToolResult::failed(
                serde_json::json!({ "stdout": out_text, "stderr": err_text }),
                format!(
                    "skill '{}' exited with code {}: {}",
                    pid_label, exit_code, err_text
                ),
            ))
        } else {
            let output: Value = serde_json::from_str(&out_text).map_err(|error| {
                let safe_error = sanitize_error_text(&error.to_string());
                tracing::error!("skill returned invalid JSON on stdout");
                anyhow::anyhow!(
                    "skill '{}' returned invalid JSON: {}",
                    pid_label,
                    safe_error
                )
            })?;
            Ok(ToolResult::ok(output))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_skills::Skill;
    use std::path::Path;

    async fn scripted_skill(skills_root: &Path, script: &str) -> Skill {
        scripted_skill_named(skills_root, "test-skill", script).await
    }

    async fn scripted_skill_named(skills_root: &Path, name: &str, script: &str) -> Skill {
        crate::test_support::discover_skill_fixture(
            skills_root,
            name,
            "Temporary runner test",
            Some(script),
        )
        .await
    }

    fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link)
        }
    }

    #[tokio::test]
    async fn runner_total_deadline_returns_timed_out() {
        let temp = tempfile::tempdir().unwrap();
        let skill = scripted_skill(temp.path(), "import time\ntime.sleep(10)\nprint('{}')\n").await;

        let config = SkillsExecConfig {
            timeout_secs: 1,
            venv_root: temp.path().join("venvs"),
            work_dir: temp.path().join("work"),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let mut runner = SkillRunner::new(venv, config);
        runner.script_snapshot_root = temp.path().join("script-snapshots");
        let cancel = CancellationToken::new();

        let result = runner
            .execute(&skill, &serde_json::json!({"text": "hi"}), cancel)
            .await;
        assert!(matches!(
            result.unwrap().outcome,
            crate::ToolExecutionOutcome::TimedOutUnknown
        ));
        assert!(
            std::fs::read_dir(&runner.script_snapshot_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn runner_drains_both_pipes_and_rejects_bounded_output_overflow() {
        let temp = tempfile::tempdir().unwrap();
        let skill = scripted_skill(
            temp.path(),
            concat!(
                "import sys, threading\n",
                "payload = b'x' * (9 * 1024 * 1024)\n",
                "out = threading.Thread(target=lambda: sys.stdout.buffer.write(payload))\n",
                "err = threading.Thread(target=lambda: sys.stderr.buffer.write(payload))\n",
                "out.start(); err.start(); out.join(); err.join()\n",
            ),
        )
        .await;
        let config = SkillsExecConfig {
            timeout_secs: 15,
            max_output_lines: 5000,
            venv_root: temp.path().join("venvs"),
            work_dir: temp.path().join("work"),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let mut runner = SkillRunner::new(venv, config);
        runner.script_snapshot_root = temp.path().join("script-snapshots");

        let result = tokio::time::timeout(
            Duration::from_secs(20),
            runner.execute(
                &skill,
                &serde_json::json!({"input": "ok"}),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("pipe readers must let a noisy child exit")
        .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("output exceeded its limit"));
        assert_eq!(result.output["truncated"], true);
        assert!(result.truncated);
        assert!(
            std::fs::read_dir(&runner.script_snapshot_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn runner_bounds_concurrent_skill_processes_and_snapshot_capacity() {
        let temp = tempfile::tempdir().unwrap();
        let work_dir = temp.path().join("work");
        std::fs::create_dir_all(&work_dir).unwrap();
        let release = work_dir.join("release");
        let mut skills = Vec::new();
        for index in 0..(MAX_CONCURRENT_SKILL_EXECUTIONS + 1) {
            let name = format!("capacity-skill-{index}");
            let script = format!(
                "from pathlib import Path\nimport time\nmarker = Path('skill-{index}.active')\nmarker.write_text('started', encoding='utf-8')\nrelease = Path('release')\ndeadline = time.monotonic() + 20\nwhile not release.exists() and time.monotonic() < deadline:\n    time.sleep(0.01)\nmarker.unlink(missing_ok=True)\nif not release.exists():\n    raise SystemExit(2)\nprint('{{}}')\n"
            );
            let skill = scripted_skill_named(temp.path(), &name, &script).await;
            skills.push(skill);
        }

        let config = SkillsExecConfig {
            timeout_secs: 30,
            venv_root: temp.path().join("venvs"),
            work_dir: work_dir.clone(),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let mut runner = SkillRunner::new(venv, config);
        runner.script_snapshot_root = temp.path().join("script-snapshots");
        for skill in &skills {
            runner
                .venv
                .ensure(skill.name(), skill.root())
                .await
                .unwrap();
        }

        let runner = std::sync::Arc::new(runner);
        let mut executions = tokio::task::JoinSet::new();
        for skill in skills {
            let runner = std::sync::Arc::clone(&runner);
            executions.spawn(async move {
                runner
                    .execute(&skill, &serde_json::json!({}), CancellationToken::new())
                    .await
            });
        }

        let observed_capacity = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let started = std::fs::read_dir(&work_dir)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_name().to_string_lossy().ends_with(".active"))
                    .count();
                if started >= MAX_CONCURRENT_SKILL_EXECUTIONS {
                    break started;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .ok();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let stable_capacity = std::fs::read_dir(&work_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".active"))
            .count();
        std::fs::write(&release, b"release").unwrap();

        let mut successful = 0;
        while let Some(result) = executions.join_next().await {
            let result = result.unwrap().unwrap();
            successful += usize::from(result.success);
        }

        assert_eq!(
            observed_capacity,
            Some(MAX_CONCURRENT_SKILL_EXECUTIONS),
            "runner should fill its bounded execution capacity"
        );
        assert_eq!(stable_capacity, MAX_CONCURRENT_SKILL_EXECUTIONS);
        assert_eq!(successful, MAX_CONCURRENT_SKILL_EXECUTIONS + 1);
        assert!(
            std::fs::read_dir(&runner.script_snapshot_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn stale_snapshot_cleanup_only_removes_owned_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let stale = temp.path().join("haven-skill-script-Abc123.py");
        let malformed = temp.path().join("haven-skill-script-short.py");
        let unrelated = temp.path().join("notes.py");
        let directory = temp.path().join("haven-skill-script-Dir123.py");
        std::fs::write(&stale, b"old snapshot").unwrap();
        std::fs::write(&malformed, b"not a generated name").unwrap();
        std::fs::write(&unrelated, b"unrelated").unwrap();
        std::fs::create_dir(&directory).unwrap();

        let removed = cleanup_stale_skill_script_snapshots_at(
            temp.path(),
            SystemTime::now() + Duration::from_secs(1),
            Duration::ZERO,
        )
        .unwrap();

        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(malformed.exists());
        assert!(unrelated.exists());
        assert!(directory.exists());
    }

    #[test]
    fn snapshot_cleanup_keeps_files_within_the_maximum_execution_age() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("haven-skill-script-abc123.py");
        std::fs::write(&current, b"active snapshot").unwrap();

        let removed = cleanup_stale_skill_script_snapshots_at(
            temp.path(),
            SystemTime::now(),
            SKILL_SCRIPT_SNAPSHOT_MAX_AGE,
        )
        .unwrap();

        assert_eq!(removed, 0);
        assert!(current.exists());
    }

    #[test]
    fn stale_snapshot_cleanup_never_follows_or_removes_a_symlink_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("outside.py");
        let link = temp.path().join("haven-skill-script-Link123.py");
        std::fs::write(&target, b"outside").unwrap();
        if symlink_file(&target, &link).is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }

        let removed = cleanup_stale_skill_script_snapshots_at(
            temp.path(),
            SystemTime::now() + Duration::from_secs(1),
            Duration::ZERO,
        )
        .unwrap();

        assert_eq!(removed, 0);
        assert!(link.exists());
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }
}
