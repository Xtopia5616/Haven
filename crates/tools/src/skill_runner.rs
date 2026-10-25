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
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Bound retained output independently for stdout and stderr. Both streams
/// continue draining after the cap so a child can never deadlock on a full
/// pipe merely because its result is too large for Haven to keep.
const MAX_SKILL_STREAM_BYTES: usize = 8 * 1024 * 1024;
const MAX_SKILL_SCRIPT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CONCURRENT_SKILL_EXECUTIONS: usize = 4;
const SKILL_WORK_DIRECTORY_PREFIX: &str = "haven-skill-run-";
const SKILL_WORK_DIRECTORY_OWNER_LOCK: &str = ".haven-owner.lock";
const SKILL_WORK_DIRECTORY_MAX_ENTRIES: usize = 4096;
const SKILL_WORK_DIRECTORY_MAX_CLEANUPS_PER_PASS: usize = 16;
const SKILL_WORK_DIRECTORY_STALE_AGE: Duration =
    Duration::from_secs(MAX_SKILLS_EXEC_TIMEOUT_SECS + 60);
const SKILL_WORK_DIRECTORY_CLEANUP_ATTEMPTS: usize = 8;
const SKILL_WORK_DIRECTORY_CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(25);
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

async fn create_skill_work_directory(
    configured_root: &std::path::Path,
) -> anyhow::Result<SkillWorkDirectory> {
    if !haven_common::path::is_absolute_local_path(configured_root) {
        anyhow::bail!("Skill scratch root must be an absolute local directory");
    }
    let root = match haven_platform::filesystem::DirectoryCapability::open(configured_root) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir_all(configured_root)
                .await
                .map_err(|_| anyhow::anyhow!("failed to prepare Skill scratch root"))?;
            haven_platform::filesystem::DirectoryCapability::open(configured_root).map_err(
                |_| anyhow::anyhow!("Skill scratch root must be a local non-reparse directory"),
            )?
        }
        Err(_) => {
            anyhow::bail!("Skill scratch root must be a local non-reparse directory");
        }
    };
    match cleanup_stale_skill_work_directories(&root, SystemTime::now()) {
        Ok(removed) if removed > 0 => {
            tracing::debug!(removed, "removed stale Skill scratch directories");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            anyhow::bail!("Skill scratch root exceeds its safe inspection limit");
        }
        Err(error) => {
            tracing::warn!(
                error = %sanitize_error_text(&error.to_string()),
                "could not inspect stale Skill scratch directories"
            );
        }
    }
    let temporary = tempfile::Builder::new()
        .prefix(SKILL_WORK_DIRECTORY_PREFIX)
        .tempdir_in(root.root())
        .map_err(|_| anyhow::anyhow!("failed to create per-execution Skill scratch directory"))?;
    let owner_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(temporary.path().join(SKILL_WORK_DIRECTORY_OWNER_LOCK))
        .map_err(|_| anyhow::anyhow!("failed to claim per-execution Skill scratch directory"))?;
    owner_lock
        .try_lock()
        .map_err(|_| anyhow::anyhow!("failed to lock per-execution Skill scratch directory"))?;
    Ok(SkillWorkDirectory {
        root,
        temporary: Some(temporary),
        owner_lock: Some(owner_lock),
    })
}

struct SkillWorkDirectory {
    root: haven_platform::filesystem::DirectoryCapability,
    temporary: Option<tempfile::TempDir>,
    owner_lock: Option<std::fs::File>,
}

impl SkillWorkDirectory {
    fn path(&self) -> &std::path::Path {
        self.temporary
            .as_ref()
            .expect("skill work directory is present until drop")
            .path()
    }
}

impl Drop for SkillWorkDirectory {
    fn drop(&mut self) {
        // Release the inter-process owner marker before removing its directory.
        self.owner_lock.take();
        let Some(temporary) = self.temporary.take() else {
            return;
        };
        let path = temporary.path().to_path_buf();
        let Some(relative) = self.root.relative_path(&path) else {
            tracing::warn!("Skill scratch directory escaped its configured root");
            return;
        };
        drop(temporary);

        for attempt in 0..SKILL_WORK_DIRECTORY_CLEANUP_ATTEMPTS {
            match self.root.remove_dir_all(&relative) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(_error) if attempt + 1 < SKILL_WORK_DIRECTORY_CLEANUP_ATTEMPTS => {
                    std::thread::sleep(SKILL_WORK_DIRECTORY_CLEANUP_RETRY_DELAY);
                }
                Err(error) => {
                    tracing::warn!(
                        error = %sanitize_error_text(&error.to_string()),
                        "failed to remove Skill scratch directory"
                    );
                }
            }
        }
    }
}

fn cleanup_stale_skill_work_directories(
    root: &haven_platform::filesystem::DirectoryCapability,
    now: SystemTime,
) -> std::io::Result<usize> {
    use haven_platform::filesystem::DirectoryEntryKind;
    use std::path::Path;

    let entries = root.entries(SKILL_WORK_DIRECTORY_MAX_ENTRIES)?;
    let mut removed = 0;
    for entry in entries {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        let suffix = name.strip_prefix(SKILL_WORK_DIRECTORY_PREFIX);
        if !suffix.is_some_and(|suffix| {
            (6..=32).contains(&suffix.len())
                && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }) || entry.kind != DirectoryEntryKind::Directory
        {
            continue;
        }

        let relative = Path::new(name);
        let lock_path = relative.join(SKILL_WORK_DIRECTORY_OWNER_LOCK);
        let mut stale_lock = None;
        let modified = match root.open_read_write(&lock_path) {
            Ok(lock) => {
                let modified = lock.metadata()?.modified()?;
                if lock.try_lock().is_err() {
                    // A live Skill attempt still owns this directory.
                    continue;
                }
                stale_lock = Some(lock);
                modified
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Reclaim pre-lock leftovers and directories whose process
                // crashed after creation but before publishing the lock.
                let Some(modified) = entry.modified else {
                    continue;
                };
                modified
            }
            Err(_) => continue,
        };
        if now.duration_since(modified).unwrap_or_default() < SKILL_WORK_DIRECTORY_STALE_AGE {
            continue;
        }
        match root.remove_dir_all(relative) {
            Ok(()) => {
                removed += 1;
                if removed >= SKILL_WORK_DIRECTORY_MAX_CLEANUPS_PER_PASS {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        drop(stale_lock);
    }
    Ok(removed)
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
    exit_code: i32,
    stdout: CappedStreamRead,
    stderr: CappedStreamRead,
}

type SkillReadPipe = Box<dyn AsyncRead + Unpin + Send>;
type SkillWritePipe = Box<dyn AsyncWrite + Unpin + Send>;

enum SkillChild {
    Host {
        child: tokio::process::Child,
        _containment: ProcessContainment,
    },
    #[cfg(windows)]
    Sandboxed(haven_platform::sandboxed_process::SandboxedProcess),
}

impl SkillChild {
    fn take_stdin(&mut self) -> Option<SkillWritePipe> {
        match self {
            Self::Host { child, .. } => child
                .stdin
                .take()
                .map(|pipe| Box::new(pipe) as SkillWritePipe),
            #[cfg(windows)]
            Self::Sandboxed(child) => child
                .take_stdin()
                .map(|pipe| Box::new(pipe) as SkillWritePipe),
        }
    }

    fn take_stdout(&mut self) -> Option<SkillReadPipe> {
        match self {
            Self::Host { child, .. } => child
                .stdout
                .take()
                .map(|pipe| Box::new(pipe) as SkillReadPipe),
            #[cfg(windows)]
            Self::Sandboxed(child) => child
                .take_stdout()
                .map(|pipe| Box::new(pipe) as SkillReadPipe),
        }
    }

    fn take_stderr(&mut self) -> Option<SkillReadPipe> {
        match self {
            Self::Host { child, .. } => child
                .stderr
                .take()
                .map(|pipe| Box::new(pipe) as SkillReadPipe),
            #[cfg(windows)]
            Self::Sandboxed(child) => child
                .take_stderr()
                .map(|pipe| Box::new(pipe) as SkillReadPipe),
        }
    }

    async fn wait(&mut self) -> std::io::Result<i32> {
        match self {
            Self::Host { child, .. } => Ok(child.wait().await?.code().unwrap_or(-1)),
            #[cfg(windows)]
            Self::Sandboxed(child) => child.wait().await.map(|exit| exit.exit_code),
        }
    }

    async fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Host { child, .. } => child.kill().await,
            #[cfg(windows)]
            Self::Sandboxed(child) => child.kill().await,
        }
    }
}

async fn terminate_skill_child(child: &mut SkillChild) {
    let _ = child.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
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

#[cfg(test)]
struct TestUnrestrictedProcessStartAuthorization;

#[cfg(test)]
impl haven_skills::ProcessStartAuthorization for TestUnrestrictedProcessStartAuthorization {
    async fn acquire_process_start_permit(
        &self,
        _request: haven_skills::SkillProcessRequest,
    ) -> anyhow::Result<Box<dyn haven_skills::ProcessStartPermit>> {
        Ok(Box::new(()))
    }
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
    #[cfg(test)]
    pub async fn execute(
        &self,
        skill: &Skill,
        params: &Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.execute_inner(skill, params, cancel, None).await
    }

    pub(crate) async fn execute_with_authorization(
        &self,
        skill: &Skill,
        params: &Value,
        cancel: CancellationToken,
        execution_authorization: crate::security::ExecutionPolicyAuthorization,
    ) -> anyhow::Result<ToolResult> {
        self.execute_inner(skill, params, cancel, Some(execution_authorization))
            .await
    }

    async fn execute_inner(
        &self,
        skill: &Skill,
        params: &Value,
        cancel: CancellationToken,
        execution_authorization: Option<crate::security::ExecutionPolicyAuthorization>,
    ) -> anyhow::Result<ToolResult> {
        #[cfg(not(test))]
        if execution_authorization.is_none() {
            anyhow::bail!("Skill execution requires an authorization proof");
        }

        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_secs);
        let policy_revoked = async {
            match execution_authorization.as_ref() {
                Some(authorization) => authorization.wait_for_policy_revocation().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(policy_revoked);
        let _execution_permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Ok(ToolResult::cancelled(format!("skill '{}' cancelled", skill.name())));
            }
            _ = &mut policy_revoked => {
                return Ok(ToolResult::cancelled(format!("skill '{}' cancelled because execution policy changed", skill.name())));
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
            _ = &mut policy_revoked => {
                return Ok(ToolResult::cancelled(format!("skill '{}' cancelled because execution policy changed", skill.name())));
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
            _ = &mut policy_revoked => {
                return Ok(ToolResult::cancelled(format!("skill '{}' cancelled because execution policy changed", skill.name())));
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("skill '{}' timed out after {}s", skill.name(), self.config.timeout_secs),
                ));
            }
            result = async {
                match execution_authorization.as_ref() {
                    Some(authorization) => self
                        .venv
                        .ensure(skill.name(), skill.root(), authorization)
                        .await,
                    None => {
                        #[cfg(test)]
                        {
                            self.venv
                                .ensure(
                                    skill.name(),
                                    skill.root(),
                                    &TestUnrestrictedProcessStartAuthorization,
                                )
                                .await
                        }
                        #[cfg(not(test))]
                        {
                            anyhow::bail!("Skill setup requires an execution authorization")
                        }
                    }
                }
            } => result?,
        };

        // Skill scripts share only the configured scratch parent. Each attempt
        // gets a private cwd that is removed on every normal return path.
        // This organizes relative outputs; it is not an OS filesystem sandbox.
        let work_dir = create_skill_work_directory(&self.config.work_dir).await?;

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
            .current_dir(work_dir.path())
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

        let venv_directory = self.venv.environment_directory(skill.name())?;
        let mut runtime_read_roots = Vec::new();
        #[cfg(windows)]
        runtime_read_roots.extend(crate::process_sandbox::runtime_dependency_roots(
            cmd.as_std(),
        ));
        runtime_read_roots.push(skill.root().to_path_buf());
        runtime_read_roots.push(venv_directory.clone());
        runtime_read_roots.extend(self.venv.runtime_read_roots(skill.name())?);
        let script_snapshot_path: &std::path::Path = script_snapshot.as_ref();
        if let Some(parent) = script_snapshot_path.parent() {
            runtime_read_roots.push(parent.to_path_buf());
        }
        if let Some(parent) = python.parent() {
            runtime_read_roots.push(parent.to_path_buf());
        }
        runtime_read_roots.sort();
        runtime_read_roots.dedup();
        runtime_read_roots
            .retain(|path| haven_platform::filesystem::DirectoryCapability::open(path).is_ok());

        let spawn_permit = match execution_authorization.as_ref() {
            Some(authorization) => {
                let permit = tokio::select! {
                    biased;
                    _ = &mut policy_revoked => {
                        return Ok(ToolResult::cancelled(format!("skill '{}' cancelled because execution policy changed", skill.name())));
                    }
                    result = authorization.acquire_skill_process_start_permit(
                        haven_skills::SkillProcessRequest {
                            purpose: Some(haven_skills::SkillProcessPurpose::Execute),
                            skill_root: skill.root().to_path_buf(),
                            venv_directory,
                            scratch_directory: work_dir.path().to_path_buf(),
                            runtime_read_roots,
                        },
                    ) => {
                        result.map_err(crate::security::execution_policy_denied)?
                    }
                };
                Some(permit)
            }
            None => None,
        };
        let mut child = if let Some(policy) = spawn_permit
            .as_ref()
            .and_then(|permit| permit.sandboxed_process_policy())
        {
            #[cfg(windows)]
            {
                cmd.env("TEMP", work_dir.path())
                    .env("TMP", work_dir.path())
                    .env("TMPDIR", work_dir.path());
                let request =
                    haven_platform::sandboxed_process::SandboxedProcessRequest::from_command(
                        cmd.as_std(),
                        policy,
                        false,
                    )?;
                SkillChild::Sandboxed(
                    haven_platform::sandboxed_process::SandboxedProcess::spawn(
                        request,
                        policy.isolation_mode,
                    )
                    .await?,
                )
            }
            #[cfg(not(windows))]
            {
                let _ = (policy, cmd);
                anyhow::bail!("Workspace Write Skill processes require Windows MXC isolation")
            }
        } else {
            let containment = ProcessContainment::new().map_err(|error| {
                anyhow::anyhow!(
                    "failed to create process containment for skill '{}': {}",
                    pid_label,
                    sanitize_error_text(&error.to_string())
                )
            })?;
            containment.prepare_command(cmd.as_std_mut(), 0);
            let child_spawn_result = {
                let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
                cmd.kill_on_drop(true).spawn()
            };
            let child = child_spawn_result.map_err(|error| {
                anyhow::anyhow!(
                    "failed to spawn skill '{}': {}",
                    pid_label,
                    sanitize_error_text(&error.to_string())
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
                let mut child = SkillChild::Host {
                    child,
                    _containment: containment,
                };
                terminate_skill_child(&mut child).await;
                drop(spawn_permit);
                anyhow::bail!(
                    "failed to attach skill '{}' to process containment: {}",
                    pid_label,
                    sanitize_error_text(&error.to_string())
                );
            }
            SkillChild::Host {
                child,
                _containment: containment,
            }
        };
        drop(spawn_permit);

        let stdin = child.take_stdin();
        let stdout = child.take_stdout();
        let stderr = child.take_stderr();
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
                let exit_code = status.map_err(|error| {
                    anyhow::anyhow!(
                        "skill '{}' wait error: {}",
                        pid_label,
                        sanitize_error_text(&error.to_string())
                    )
                })?;
                Ok::<_, anyhow::Error>(SkillProcessOutput {
                    exit_code,
                    stdout,
                    stderr,
                })
            } => result?,
            _ = cancel.cancelled() => {
                // Wait for the script process to exit before dropping its
                // current directory, which may still be held by the process.
                terminate_skill_child(&mut child).await;
                return Ok(ToolResult::cancelled(format!(
                    "skill '{}' cancelled",
                    pid_label
                )));
            }
            _ = &mut policy_revoked => {
                terminate_skill_child(&mut child).await;
                return Ok(ToolResult::cancelled(format!(
                    "skill '{}' cancelled because execution policy changed",
                    pid_label
                )));
            }
            _ = tokio::time::sleep_until(deadline) => {
                terminate_skill_child(&mut child).await;
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
        let exit_code = process_output.exit_code;

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

    fn python_path_literal(path: &Path) -> String {
        serde_json::to_string(&path.to_string_lossy()).unwrap()
    }

    fn symlink_directory(target: &Path, link: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link)
        }
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
        runner
            .venv
            .ensure(
                skill.name(),
                skill.root(),
                &TestUnrestrictedProcessStartAuthorization,
            )
            .await
            .unwrap();
        let cancel = CancellationToken::new();

        let result = runner
            .execute(&skill, &serde_json::json!({"text": "hi"}), cancel)
            .await;
        assert!(matches!(
            result.unwrap().outcome,
            crate::ToolExecutionOutcome::TimedOutUnknown
        ));
        assert!(
            std::fs::read_dir(&runner.config.work_dir)
                .unwrap()
                .next()
                .is_none()
        );
        assert!(
            std::fs::read_dir(&runner.script_snapshot_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn runner_cancellation_waits_for_process_exit_before_removing_work_dir() {
        let temp = tempfile::tempdir().unwrap();
        let started = temp.path().join("script-started");
        let script = format!(
            "from pathlib import Path\nimport time\nPath({}).write_text('started')\ntime.sleep(30)\nprint('{{}}')\n",
            python_path_literal(&started)
        );
        let skill = scripted_skill(temp.path(), &script).await;
        let config = SkillsExecConfig {
            timeout_secs: 10,
            venv_root: temp.path().join("venvs"),
            work_dir: temp.path().join("work"),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let mut runner = SkillRunner::new(venv, config);
        runner.script_snapshot_root = temp.path().join("script-snapshots");
        runner
            .venv
            .ensure(
                skill.name(),
                skill.root(),
                &TestUnrestrictedProcessStartAuthorization,
            )
            .await
            .unwrap();

        let runner = std::sync::Arc::new(runner);
        let cancel = CancellationToken::new();
        let execution_cancel = cancel.clone();
        let execution_runner = std::sync::Arc::clone(&runner);
        let execution = tokio::spawn(async move {
            execution_runner
                .execute(&skill, &serde_json::json!({}), execution_cancel)
                .await
        });
        let script_started = tokio::time::timeout(Duration::from_secs(10), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        cancel.cancel();
        let result = execution.await.unwrap().unwrap();

        assert!(
            script_started,
            "the subprocess should reach its start marker"
        );
        assert_eq!(result.outcome, crate::ToolExecutionOutcome::Cancelled);
        assert!(
            std::fs::read_dir(&runner.config.work_dir)
                .unwrap()
                .next()
                .is_none()
        );
        assert!(
            std::fs::read_dir(&runner.script_snapshot_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn runner_cancellation_terminates_descendant_processes() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("skill-descendant.pid");
        let mut descendant =
            crate::process_test_support::DescendantProcessMarker::new(pid_file.clone());
        let script = format!(
            "from pathlib import Path\nimport subprocess, sys, time\nchild = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\nPath({}).write_text(str(child.pid), encoding='ascii')\ntime.sleep(60)\nprint('{{}}')\n",
            python_path_literal(&pid_file)
        );
        let skill = scripted_skill(temp.path(), &script).await;
        let config = SkillsExecConfig {
            timeout_secs: 60,
            venv_root: temp.path().join("venvs"),
            work_dir: temp.path().join("work"),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let mut runner = SkillRunner::new(venv, config);
        runner.script_snapshot_root = temp.path().join("script-snapshots");
        runner
            .venv
            .ensure(
                skill.name(),
                skill.root(),
                &TestUnrestrictedProcessStartAuthorization,
            )
            .await
            .unwrap();

        let runner = std::sync::Arc::new(runner);
        let cancel = CancellationToken::new();
        let execution_cancel = cancel.clone();
        let execution_runner = std::sync::Arc::clone(&runner);
        let execution = tokio::spawn(async move {
            execution_runner
                .execute(&skill, &serde_json::json!({}), execution_cancel)
                .await
        });
        let marker_ready = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if tokio::fs::read_to_string(&pid_file)
                    .await
                    .is_ok_and(|pid| !pid.trim().is_empty())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(10), execution)
            .await
            .expect("Skill cancellation must complete")
            .expect("Skill task must not panic")
            .expect("Skill runner must report cancellation as a result");

        assert!(marker_ready, "Skill child must start its descendant");
        let pid = descendant
            .pid()
            .expect("Skill child must write the descendant PID");
        assert_eq!(result.outcome, crate::ToolExecutionOutcome::Cancelled);
        descendant
            .wait_for_exit(pid, Duration::from_secs(5))
            .expect("Skill cancellation must close the Job Object");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn runner_policy_revocation_terminates_descendant_processes() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("revoked-skill-descendant.pid");
        let mut descendant =
            crate::process_test_support::DescendantProcessMarker::new(pid_file.clone());
        let script = format!(
            "from pathlib import Path\nimport subprocess, sys, time\nchild = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\nPath({}).write_text(str(child.pid), encoding='ascii')\ntime.sleep(60)\nprint('{{}}')\n",
            python_path_literal(&pid_file)
        );
        let skill = scripted_skill(temp.path(), &script).await;
        let config = SkillsExecConfig {
            timeout_secs: 60,
            venv_root: temp.path().join("venvs"),
            work_dir: temp.path().join("work"),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let mut runner = SkillRunner::new(venv, config);
        runner.script_snapshot_root = temp.path().join("script-snapshots");
        runner
            .venv
            .ensure(
                skill.name(),
                skill.root(),
                &TestUnrestrictedProcessStartAuthorization,
            )
            .await
            .unwrap();

        let runner = std::sync::Arc::new(runner);
        let (execution_authorization, authorization) =
            crate::security::ExecutionPolicyAuthorization::for_test_with_engine_and_sandbox_mode(
                haven_common::types::SandboxMode::FullAccess,
            );
        let execution_runner = std::sync::Arc::clone(&runner);
        let execution = tokio::spawn(async move {
            execution_runner
                .execute_with_authorization(
                    &skill,
                    &serde_json::json!({}),
                    CancellationToken::new(),
                    execution_authorization,
                )
                .await
        });
        let descendant_pid = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Ok(pid) = descendant.pid()
                    && pid > 0
                {
                    break Some(pid);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(None);

        authorization
            .set_execution_mode(haven_common::types::ExecutionMode::Analyze)
            .await;
        let result = tokio::time::timeout(Duration::from_secs(10), execution)
            .await
            .expect("policy revocation must stop the Skill call")
            .expect("Skill task must not panic")
            .expect("Skill runner must report cancellation as a result");

        let pid = descendant_pid.expect("Skill child must write the descendant PID");
        assert_eq!(result.outcome, crate::ToolExecutionOutcome::Cancelled);
        descendant
            .wait_for_exit(pid, Duration::from_secs(5))
            .expect("policy revocation must close the Skill Job Object");
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires the Windows MXC ProcessContainer runtime"]
    async fn workspace_write_skill_setup_and_script_obey_mxc_file_policy() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let skills_root = temp.path().join("skills");
        let workspace_output = workspace.join("skill-output.txt");
        let outside_path = temp.path().join("outside.txt");
        let script = format!(
            "import json\nfrom pathlib import Path\nPath('inside.txt').write_text('scratch', encoding='utf-8')\nPath({}).write_text('workspace', encoding='utf-8')\noutside_denied = False\ntry:\n    Path({}).write_text('escape', encoding='utf-8')\nexcept OSError:\n    outside_denied = True\nprint(json.dumps({{'inside': Path('inside.txt').read_text(encoding='utf-8'), 'workspace_written': True, 'outside_denied': outside_denied}}))\n",
            python_path_literal(&workspace_output),
            python_path_literal(&outside_path)
        );
        let skill = scripted_skill(&skills_root, &script).await;
        let config = SkillsExecConfig {
            timeout_secs: 30,
            venv_root: temp.path().join("venvs"),
            work_dir: temp.path().join("runtime/work"),
            ..Default::default()
        };
        let mut runner = SkillRunner::new(VenvManager::new(config.venv_root.clone()), config);
        runner.script_snapshot_root = temp.path().join("runtime/script-snapshots");
        let (authorization, _) =
            crate::security::ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                haven_common::types::NetworkPolicy::Deny,
            );

        runner
            .venv
            .ensure(skill.name(), skill.root(), &authorization)
            .await
            .unwrap_or_else(|error| {
                panic!("MXC must create the private Skill environment: {error:#}")
            });
        let result = runner
            .execute_with_authorization(
                &skill,
                &serde_json::json!({}),
                CancellationToken::new(),
                authorization,
            )
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        assert_eq!(result.output["inside"], "scratch");
        assert_eq!(result.output["workspace_written"], true);
        assert_eq!(result.output["outside_denied"], true);
        assert_eq!(
            std::fs::read_to_string(workspace_output).unwrap(),
            "workspace"
        );
        assert!(!outside_path.exists());
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
            // The fixture writes large payloads to both Windows pipes; leave
            // headroom for loaded workspace test runs after venv prewarming.
            timeout_secs: 45,
            max_output_lines: 5000,
            venv_root: temp.path().join("venvs"),
            work_dir: temp.path().join("work"),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let mut runner = SkillRunner::new(venv, config);
        runner.script_snapshot_root = temp.path().join("script-snapshots");
        runner
            .venv
            .ensure(
                skill.name(),
                skill.root(),
                &TestUnrestrictedProcessStartAuthorization,
            )
            .await
            .unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(55),
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
        let error = result.error.unwrap();
        assert!(error.contains("output exceeded its limit"), "{error}");
        assert_eq!(result.output["truncated"], true);
        assert!(result.truncated);
        assert!(
            std::fs::read_dir(&runner.config.work_dir)
                .unwrap()
                .next()
                .is_none()
        );
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
        let markers = temp.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        let marker_root_literal = python_path_literal(&markers);
        let release = markers.join("release");
        let mut skills = Vec::new();
        for index in 0..(MAX_CONCURRENT_SKILL_EXECUTIONS + 1) {
            let name = format!("capacity-skill-{index}");
            let script = format!(
                "from pathlib import Path\nimport time\nroot = Path({marker_root_literal})\nmarker = root / 'skill-{index}.active'\nmarker.write_text('started', encoding='utf-8')\nrelease = root / 'release'\ndeadline = time.monotonic() + 20\nwhile not release.exists() and time.monotonic() < deadline:\n    time.sleep(0.01)\nmarker.unlink(missing_ok=True)\nif not release.exists():\n    raise SystemExit(2)\nprint('{{}}')\n"
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
                .ensure(
                    skill.name(),
                    skill.root(),
                    &TestUnrestrictedProcessStartAuthorization,
                )
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
                let started = std::fs::read_dir(&markers)
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
        let stable_capacity = std::fs::read_dir(&markers)
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
        assert!(std::fs::read_dir(&work_dir).unwrap().next().is_none());
        assert!(
            std::fs::read_dir(&runner.script_snapshot_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn runner_removes_per_invocation_work_files_after_success() {
        let temp = tempfile::tempdir().unwrap();
        let skill = scripted_skill(
            temp.path(),
            "from pathlib import Path\nPath('artifact.txt').write_text('temporary')\nprint('{}')\n",
        )
        .await;
        let work_dir = temp.path().join("work");
        let config = SkillsExecConfig {
            venv_root: temp.path().join("venvs"),
            work_dir: work_dir.clone(),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let mut runner = SkillRunner::new(venv, config);
        runner.script_snapshot_root = temp.path().join("script-snapshots");

        let result = runner
            .execute(&skill, &serde_json::json!({}), CancellationToken::new())
            .await
            .unwrap();

        assert!(result.success);
        assert!(std::fs::read_dir(work_dir).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn runner_rejects_a_reparse_work_root_before_starting_a_script() {
        let temp = tempfile::tempdir().unwrap();
        let skill = scripted_skill(
            temp.path(),
            "from pathlib import Path\nPath('started').write_text('yes')\nprint('{}')\n",
        )
        .await;
        let target = temp.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        let work_dir = temp.path().join("work-link");
        if symlink_directory(&target, &work_dir).is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }
        let config = SkillsExecConfig {
            venv_root: temp.path().join("venvs"),
            work_dir,
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let mut runner = SkillRunner::new(venv, config);
        runner.script_snapshot_root = temp.path().join("script-snapshots");

        let result = runner
            .execute(&skill, &serde_json::json!({}), CancellationToken::new())
            .await;

        assert!(result.is_err());
        assert!(!target.join("started").exists());
        assert!(
            std::fs::read_dir(&runner.script_snapshot_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn scratch_root_checks_existing_reparse_ancestors_before_creation() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        std::fs::create_dir(&target).unwrap();
        if symlink_directory(&target, &link).is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }

        let result = create_skill_work_directory(&link.join("new-root")).await;

        assert!(result.is_err());
        assert!(!target.join("new-root").exists());
    }

    #[tokio::test]
    async fn oversized_scratch_root_fails_closed_before_creating_a_run_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        std::fs::create_dir(&root).unwrap();
        for index in 0..=SKILL_WORK_DIRECTORY_MAX_ENTRIES {
            std::fs::write(root.join(format!("entry-{index}")), []).unwrap();
        }

        let result = create_skill_work_directory(&root).await;

        assert!(result.is_err());
        assert!(
            std::fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(SKILL_WORK_DIRECTORY_PREFIX))
        );
    }

    #[test]
    fn stale_skill_scratch_cleanup_preserves_live_fresh_and_unowned_paths() {
        use haven_platform::filesystem::DirectoryCapability;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        let stale = root.join("haven-skill-run-Abc123");
        let live = root.join("haven-skill-run-Def456");
        let fresh = root.join("haven-skill-run-Ghi789");
        let malformed = root.join("haven-skill-run-short");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::create_dir(&live).unwrap();
        std::fs::create_dir(&fresh).unwrap();
        std::fs::create_dir(&malformed).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"outside").unwrap();

        let now = SystemTime::now();
        let stale_time = now - SKILL_WORK_DIRECTORY_STALE_AGE - Duration::from_secs(1);
        let stale_lock_path = stale.join(SKILL_WORK_DIRECTORY_OWNER_LOCK);
        let stale_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(stale_lock_path)
            .unwrap();
        stale_lock.set_modified(stale_time).unwrap();
        drop(stale_lock);

        let live_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(live.join(SKILL_WORK_DIRECTORY_OWNER_LOCK))
            .unwrap();
        live_lock.try_lock().unwrap();
        live_lock.set_modified(stale_time).unwrap();

        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(fresh.join(SKILL_WORK_DIRECTORY_OWNER_LOCK))
            .unwrap();
        std::fs::write(malformed.join(SKILL_WORK_DIRECTORY_OWNER_LOCK), b"unowned").unwrap();
        let link = root.join("haven-skill-run-Jkl012");
        let linked = symlink_directory(&outside, &link).is_ok();

        let capability = DirectoryCapability::open(&root).unwrap();
        let removed = cleanup_stale_skill_work_directories(&capability, now).unwrap();

        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(live.exists());
        assert!(fresh.exists());
        assert!(malformed.exists());
        if linked {
            assert!(link.exists());
        }
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"outside");
    }

    #[test]
    fn stale_skill_scratch_cleanup_respects_locks_across_process_exit() {
        use haven_platform::filesystem::DirectoryCapability;
        use std::process::{Command, Stdio};

        const LOCK_PATH_ENV: &str = "HAVEN_TEST_SKILL_SCRATCH_LOCK_PATH";
        const READY_PATH_ENV: &str = "HAVEN_TEST_SKILL_SCRATCH_READY_PATH";

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        let stale = root.join("haven-skill-run-Abc123");
        std::fs::create_dir_all(&stale).unwrap();
        let lock_path = stale.join(SKILL_WORK_DIRECTORY_OWNER_LOCK);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .unwrap();
        lock.set_modified(
            SystemTime::now() - SKILL_WORK_DIRECTORY_STALE_AGE - Duration::from_secs(1),
        )
        .unwrap();
        drop(lock);
        let ready_path = temp.path().join("child-ready");
        let capability = DirectoryCapability::open(&root).unwrap();

        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "skill_runner::tests::skill_scratch_lock_child_process_helper",
                "--nocapture",
            ])
            .env(LOCK_PATH_ENV, &lock_path)
            .env(READY_PATH_ENV, &ready_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = {
            let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
            command.spawn()
        }
        .unwrap();
        let ready_deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !ready_path.exists() && std::time::Instant::now() < ready_deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            ready_path.exists(),
            "child process did not acquire its lock"
        );
        assert_eq!(
            cleanup_stale_skill_work_directories(&capability, SystemTime::now()).unwrap(),
            0,
            "a different process still owns the scratch directory"
        );
        assert!(child.wait().unwrap().success());
        assert_eq!(
            cleanup_stale_skill_work_directories(&capability, SystemTime::now()).unwrap(),
            1,
            "the directory becomes reclaimable after its owner exits"
        );
    }

    #[test]
    fn skill_scratch_lock_child_process_helper() {
        const LOCK_PATH_ENV: &str = "HAVEN_TEST_SKILL_SCRATCH_LOCK_PATH";
        const READY_PATH_ENV: &str = "HAVEN_TEST_SKILL_SCRATCH_READY_PATH";
        let (Some(lock_path), Some(ready_path)) = (
            std::env::var_os(LOCK_PATH_ENV),
            std::env::var_os(READY_PATH_ENV),
        ) else {
            return;
        };
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        lock.try_lock().unwrap();
        std::fs::write(ready_path, b"locked").unwrap();
        std::thread::sleep(Duration::from_millis(250));
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
