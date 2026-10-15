use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use haven_platform::process_containment::ProcessContainment;

const REQUIREMENTS_FINGERPRINT_FILE: &str = ".haven-requirements-fingerprint";
const MAX_SETUP_STREAM_BYTES: usize = 1024 * 1024;
const MAX_REQUIREMENTS_BYTES: usize = 1024 * 1024;

#[cfg(test)]
fn is_skill_setup_env_allowed(key: &std::ffi::OsStr) -> bool {
    haven_platform::process_environment::is_runtime_baseline_variable(key)
}

fn filtered_skill_setup_environment(
    environment: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    haven_platform::process_environment::filter_runtime_baseline_environment(environment)
}

fn configure_skill_setup_environment(command: &mut Command) {
    command
        .env_clear()
        .envs(filtered_skill_setup_environment(std::env::vars_os()))
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1");
}

fn venv_creation_command(venv: &Path) -> Command {
    let mut command = Command::new("python");
    command
        .arg("-m")
        .arg("venv")
        .arg(venv)
        .current_dir(haven_common::config::default_runtime_temp_root());
    configure_skill_setup_environment(&mut command);
    command
}

fn pip_install_command(python: &Path, requirements_path: &Path) -> Command {
    let mut command = Command::new(python);
    command
        .arg("-m")
        .arg("pip")
        .arg("install")
        .arg("--no-cache-dir")
        .arg("-r")
        .arg(requirements_path)
        .current_dir(haven_common::config::default_runtime_temp_root());
    configure_skill_setup_environment(&mut command);
    command
}

/// Spawn and collect a Python setup command while keeping its full process tree
/// inside the platform containment boundary for the entire wait.
async fn run_contained_output(
    mut command: Command,
    skill_name: &str,
    spawn_error_context: &str,
) -> anyhow::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let containment = ProcessContainment::new()
        .map_err(|_| anyhow::anyhow!("failed to contain setup process for skill '{skill_name}'"))?;
    containment.prepare_command(command.as_std_mut(), 0);
    let child_spawn_result = {
        let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
        command.kill_on_drop(true).spawn()
    };
    let mut child = child_spawn_result
        .map_err(|_| anyhow::anyhow!("{spawn_error_context} for skill '{skill_name}'"))?;
    let pid = child.id().ok_or_else(|| {
        anyhow::anyhow!("setup process for skill '{skill_name}' has no process id")
    })?;

    #[cfg(windows)]
    let attach_result = child
        .raw_handle()
        .ok_or_else(|| std::io::Error::other("skill child process handle is unavailable"))
        .and_then(|handle| containment.attach_and_resume(pid, handle));
    #[cfg(not(windows))]
    let attach_result = containment.attach_and_resume(pid, ());

    if attach_result.is_err() {
        let _ = child.kill().await;
        anyhow::bail!("failed to attach setup process for skill '{skill_name}' to containment");
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (stdout, stderr, status) = tokio::join!(
        read_stream_capped(stdout),
        read_stream_capped(stderr),
        child.wait(),
    );
    let status =
        status.map_err(|_| anyhow::anyhow!("{spawn_error_context} for skill '{skill_name}'"))?;
    Ok(Output {
        status,
        stdout: stdout?,
        stderr: stderr?,
    })
}

async fn read_stream_capped<R>(stream: Option<R>) -> anyhow::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let Some(mut stream) = stream else {
        return Ok(Vec::new());
    };
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|_| anyhow::anyhow!("failed to read setup process output"))?;
        if read == 0 {
            return Ok(retained);
        }
        let available = MAX_SETUP_STREAM_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(available)]);
    }
}

/// Read a requirements file without blocking the async executor and without
/// retaining more than the configured cap. Dropping the caller's future drops
/// the file reader, so setup cancellation cannot proceed into pip installation.
async fn read_requirements_capped<R>(mut reader: R, skill_name: &str) -> anyhow::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut content = Vec::with_capacity(MAX_REQUIREMENTS_BYTES);
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|_| anyhow::anyhow!("failed to read requirements for skill '{skill_name}'"))?;
        if read == 0 {
            return Ok(content);
        }
        if content.len().saturating_add(read) > MAX_REQUIREMENTS_BYTES {
            anyhow::bail!(
                "requirements for skill '{skill_name}' exceed the {MAX_REQUIREMENTS_BYTES}-byte limit"
            );
        }
        content.extend_from_slice(&buffer[..read]);
    }
}

/// Manages per-skill virtual environments (M4-02).
///
/// Each skill gets an isolated venv at `<venv_root>/<skill_name>/`.
/// Creation is idempotent: `ensure` skips if the venv already exists.
/// The venv stores the SHA-256 fingerprint of successfully installed
/// `requirements.txt` content, so later manager instances can skip unchanged
/// dependencies and reinstall changed ones.
#[derive(Clone)]
pub struct VenvManager {
    root: PathBuf,
}

impl VenvManager {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn venv_dir(&self, skill_name: &str) -> PathBuf {
        self.root.join(skill_name)
    }

    /// Return the path to the Python executable inside the venv.
    fn python_path(&self, skill_name: &str) -> PathBuf {
        let venv = self.venv_dir(skill_name);
        if cfg!(windows) {
            venv.join("Scripts").join("python.exe")
        } else {
            venv.join("bin").join("python")
        }
    }

    async fn requirements_snapshot(
        skill_root: &Path,
        skill_name: &str,
    ) -> anyhow::Result<Option<(PathBuf, Vec<u8>)>> {
        let canonical_root = tokio::fs::canonicalize(skill_root)
            .await
            .map_err(|_| anyhow::anyhow!("failed to resolve skill root"))?;
        let candidate = canonical_root.join("requirements.txt");
        // Open first. Validation then checks that the currently resolved path
        // still identifies this handle, and all later reads use the handle.
        let file = match haven_platform::filesystem::open_read_only(&candidate) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => anyhow::bail!("failed to inspect skill requirements"),
        };
        let canonical_requirements = tokio::fs::canonicalize(&candidate)
            .await
            .map_err(|_| anyhow::anyhow!("failed to resolve skill requirements"))?;
        let resolved_file = haven_platform::filesystem::open_read_only(&canonical_requirements)
            .map_err(|_| anyhow::anyhow!("failed to inspect skill requirements"))?;
        super::validate_open_file_path(
            &canonical_root,
            &canonical_requirements,
            &file,
            &resolved_file,
        )
        .map_err(|_| anyhow::anyhow!("skill requirements failed path validation"))?;
        let content = read_requirements_capped(tokio::fs::File::from_std(file), skill_name).await?;
        Ok(Some((canonical_root, content)))
    }

    #[cfg(test)]
    async fn checksum_file(path: &Path, skill_name: &str) -> anyhow::Result<String> {
        let root = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("failed to resolve skill root"))?;
        let (_, content) = Self::requirements_snapshot(root, skill_name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("failed to read requirements for skill '{skill_name}'")
            })?;
        Ok(format!("sha256:{:x}", Sha256::digest(content)))
    }

    #[cfg(test)]
    async fn ensure_requirements_with<F, Fut>(
        &self,
        skill_name: &str,
        requirements_path: &Path,
        install: F,
    ) -> anyhow::Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        let parent = requirements_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("failed to resolve skill root"))?;
        let (canonical_root, content) = Self::requirements_snapshot(parent, skill_name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("failed to read requirements for skill '{skill_name}'")
            })?;
        self.ensure_requirements_snapshot_with(skill_name, &canonical_root, &content, |_| install())
            .await
    }

    async fn ensure_requirements_snapshot_with<F, Fut>(
        &self,
        skill_name: &str,
        requirements_root: &Path,
        content: &[u8],
        install: F,
    ) -> anyhow::Result<()>
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        let fingerprint = format!("sha256:{:x}", Sha256::digest(content));
        let marker_path = self
            .venv_dir(skill_name)
            .join(REQUIREMENTS_FINGERPRINT_FILE);
        let installed_fingerprint = match tokio::fs::read(&marker_path).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                let _ = error;
                return Err(anyhow::anyhow!(
                    "failed to read requirements fingerprint for skill '{skill_name}'"
                ));
            }
        };

        if installed_fingerprint.as_deref() == Some(fingerprint.as_bytes()) {
            return Ok(());
        }

        // Keep the path next to the original requirements file so pip resolves
        // relative includes with the same directory context. The name is
        // collision-resistant and the file is created exclusively.
        let mut staged = tempfile::Builder::new()
            .prefix(".haven-requirements-")
            .suffix(".txt")
            .tempfile_in(requirements_root)
            .map_err(|_| {
                anyhow::anyhow!("failed to stage requirements for skill '{skill_name}'")
            })?;
        staged
            .write_all(content)
            .and_then(|()| staged.flush())
            .map_err(|_| {
                anyhow::anyhow!("failed to stage requirements for skill '{skill_name}'")
            })?;
        let (file, staged_path_guard) = staged.into_parts();
        let staged_path = staged_path_guard.to_path_buf();
        drop(file);
        install(staged_path).await?;
        drop(staged_path_guard);

        // Persist only after pip succeeds. A failed install therefore remains
        // eligible for retry on the next ensure call.
        tokio::fs::write(&marker_path, fingerprint.as_bytes())
            .await
            .map_err(|error| {
                let _ = error;
                anyhow::anyhow!("failed to save requirements fingerprint for skill '{skill_name}'")
            })?;
        Ok(())
    }

    /// Ensure the venv exists for `skill_name`, optionally installing
    /// dependencies from `requirements.txt`. Idempotent.
    pub async fn ensure(&self, skill_name: &str, skill_root: &Path) -> anyhow::Result<PathBuf> {
        crate::validate_skill_name(skill_name)?;
        let venv = self.venv_dir(skill_name);
        let python = self.python_path(skill_name);

        if !python.exists() {
            tracing::info!("creating venv for skill");
            tokio::fs::create_dir_all(&venv)
                .await
                .map_err(|_| anyhow::anyhow!("failed to prepare venv for skill '{skill_name}'"))?;
            let output = run_contained_output(
                venv_creation_command(&venv),
                skill_name,
                "failed to create venv",
            )
            .await?;
            if !output.status.success() {
                anyhow::bail!("venv creation failed for skill '{skill_name}'");
            }
        }

        if let Some((requirements_root, content)) =
            Self::requirements_snapshot(skill_root, skill_name).await?
        {
            let python_for_install = python.clone();
            self.ensure_requirements_snapshot_with(
                skill_name,
                &requirements_root,
                &content,
                |req_path| async move {
                    tracing::info!("installing requirements for skill");
                    let output = run_contained_output(
                        pip_install_command(&python_for_install, &req_path),
                        skill_name,
                        "pip install failed",
                    )
                    .await?;
                    if !output.status.success() {
                        anyhow::bail!("pip install failed for skill '{skill_name}'");
                    }
                    Ok(())
                },
            )
            .await?;
        }

        Ok(python)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::encoding;
    use std::io;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use tempfile::tempdir;

    const CONTAINED_CHILD_MODE: &str = "HAVEN_SKILLS_CONTAINED_CHILD_MODE";

    fn contained_test_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("test executable path"));
        command
            .args([
                "--exact",
                "venv::tests::contained_command_test_child",
                "--nocapture",
            ])
            .env(CONTAINED_CHILD_MODE, mode);
        command
    }

    async fn contained_test_install(mode: &str) -> anyhow::Result<()> {
        let output = run_contained_output(
            contained_test_command(mode),
            "example-skill",
            "pip install failed",
        )
        .await?;
        if !output.status.success() {
            anyhow::bail!("pip install failed for skill 'example-skill'");
        }
        Ok(())
    }

    fn test_paths(root: &Path) -> (PathBuf, PathBuf) {
        let venv = root.join("venvs").join("example-skill");
        let skill_root = root.join("skills").join("example-skill");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::create_dir_all(&skill_root).unwrap();
        let requirements_path = skill_root.join("requirements.txt");
        std::fs::write(&requirements_path, "example-package==1.0\n").unwrap();
        (venv, requirements_path)
    }

    #[cfg(unix)]
    fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[tokio::test]
    async fn venv_dir_preserves_valid_skill_name() {
        let mgr = VenvManager::new(PathBuf::from("/tmp/venvs"));
        let dir = mgr.venv_dir("Echo_2");
        assert_eq!(dir, PathBuf::from("/tmp/venvs/Echo_2"));
    }

    #[tokio::test]
    async fn python_path_respects_platform() {
        let mgr = VenvManager::new(PathBuf::from("/tmp/venvs"));
        let p = mgr.python_path("test");
        let expected = if cfg!(windows) {
            PathBuf::from("\\tmp\\venvs\\test\\Scripts\\python.exe")
        } else {
            PathBuf::from("/tmp/venvs/test/bin/python")
        };
        assert_eq!(p, expected);
    }

    #[tokio::test]
    async fn ensure_rejects_invalid_names_before_creating_venv() {
        let temp = tempdir().unwrap();
        let venv_root = temp.path().join("venvs");
        let manager = VenvManager::new(venv_root.clone());

        for name in ["bad/name", "CON"] {
            assert!(manager.ensure(name, temp.path()).await.is_err());
        }
        assert!(!venv_root.exists());
    }

    #[test]
    fn setup_commands_keep_the_existing_arguments_and_working_directory() {
        let runtime_temp_root = haven_common::config::default_runtime_temp_root();
        let venv = PathBuf::from("skill-venvs/example");
        let create = venv_creation_command(&venv);
        assert_eq!(create.as_std().get_program(), "python");
        assert_eq!(
            create
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["-m", "venv", venv.to_string_lossy().as_ref()]
        );
        assert_eq!(
            create.as_std().get_current_dir(),
            Some(runtime_temp_root.as_path())
        );
        assert_setup_environment_is_restricted(&create);

        let python = PathBuf::from("skill-venvs/example/Scripts/python.exe");
        let requirements = PathBuf::from("skills/example/requirements.txt");
        let install = pip_install_command(&python, &requirements);
        assert_eq!(install.as_std().get_program(), python);
        assert_eq!(
            install
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "-m",
                "pip",
                "install",
                "--no-cache-dir",
                "-r",
                requirements.to_string_lossy().as_ref()
            ]
        );
        assert_eq!(
            install.as_std().get_current_dir(),
            Some(runtime_temp_root.as_path())
        );
        assert_setup_environment_is_restricted(&install);
    }

    fn assert_setup_environment_is_restricted(command: &Command) {
        let configured = command
            .as_std()
            .get_envs()
            .filter_map(|(key, value)| Some((key.to_str()?, value?)))
            .collect::<Vec<_>>();
        for (key, _) in &configured {
            assert!(
                is_skill_setup_env_allowed(std::ffi::OsStr::new(key))
                    || matches!(*key, "PYTHONIOENCODING" | "PYTHONUTF8"),
                "unexpected inherited setup environment variable: {key}"
            );
        }
        assert!(
            configured
                .iter()
                .any(|(key, value)| { *key == "PYTHONIOENCODING" && *value == "utf-8" })
        );
        assert!(
            configured
                .iter()
                .any(|(key, value)| *key == "PYTHONUTF8" && *value == "1")
        );
    }

    #[test]
    fn setup_environment_filter_excludes_credentials_and_runtime_overrides() {
        let environment = [
            ("PATH", "C:/Python"),
            ("SYSTEMROOT", "C:/Windows"),
            ("TEMP", "C:/Temp"),
            ("TMP", "C:/Temp"),
            ("COMSPEC", "C:/Windows/System32/cmd.exe"),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD"),
            ("PYTHONPATH", "C:/untrusted"),
            ("PYTHONHOME", "C:/untrusted"),
            (
                "PIP_INDEX_URL",
                "https://user:secret@example.invalid/simple",
            ),
            ("HTTPS_PROXY", "https://user:secret@example.invalid"),
            ("GITHUB_TOKEN", "secret"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()));

        let filtered = filtered_skill_setup_environment(environment);
        let keys = filtered
            .iter()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(keys.contains(&"PATH".to_string()));
        assert!(keys.contains(&"SYSTEMROOT".to_string()));
        assert!(keys.contains(&"TEMP".to_string()));
        assert!(keys.contains(&"TMP".to_string()));
        assert_eq!(keys.contains(&"COMSPEC".to_string()), cfg!(windows));
        assert_eq!(keys.contains(&"PATHEXT".to_string()), cfg!(windows));
        assert!(!keys.iter().any(|key| {
            matches!(
                key.as_str(),
                "PYTHONPATH"
                    | "PYTHONHOME"
                    | "PIP_INDEX_URL"
                    | "HTTPS_PROXY"
                    | "GITHUB_TOKEN"
                    | "AWS_SECRET_ACCESS_KEY"
            )
        }));
    }

    #[tokio::test]
    async fn setup_process_does_not_inherit_explicit_secret_environment() {
        let mut command = contained_test_command("secrets-cleared");
        command.env("GITHUB_TOKEN", "test-secret");
        configure_skill_setup_environment(&mut command);
        // Test-only child selectors are applied after the same sanitizer used by
        // production setup commands; the injected secret must stay removed.
        command.env(CONTAINED_CHILD_MODE, "secrets-cleared");

        let output = run_contained_output(command, "example-skill", "test command failed")
            .await
            .unwrap();

        assert!(output.status.success());
    }

    #[tokio::test]
    async fn requirements_fingerprint_is_reused_by_a_new_manager() {
        let temp = tempdir().unwrap();
        let (_, requirements_path) = test_paths(temp.path());
        let root = temp.path().join("venvs");
        let first = VenvManager::new(root.clone());
        let second = VenvManager::new(root);
        let mut installs = 0;

        first
            .ensure_requirements_with("example-skill", &requirements_path, || async {
                installs += 1;
                Ok(())
            })
            .await
            .unwrap();
        second
            .ensure_requirements_with("example-skill", &requirements_path, || async {
                installs += 1;
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(installs, 1);
    }

    #[tokio::test]
    async fn changed_requirements_are_installed_by_a_new_manager() {
        let temp = tempdir().unwrap();
        let (_, requirements_path) = test_paths(temp.path());
        let manager = VenvManager::new(temp.path().join("venvs"));
        manager
            .ensure_requirements_with("example-skill", &requirements_path, || async { Ok(()) })
            .await
            .unwrap();

        std::fs::write(&requirements_path, "example-package==2.0\n").unwrap();
        let restarted_manager = VenvManager::new(temp.path().join("venvs"));
        let mut installs = 0;
        restarted_manager
            .ensure_requirements_with("example-skill", &requirements_path, || async {
                installs += 1;
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(installs, 1);
    }

    #[tokio::test]
    async fn oversized_requirements_are_rejected_before_install() {
        let temp = tempdir().unwrap();
        let (venv, requirements_path) = test_paths(temp.path());
        std::fs::write(&requirements_path, vec![b'x'; MAX_REQUIREMENTS_BYTES + 1]).unwrap();
        let manager = VenvManager::new(temp.path().join("venvs"));
        let mut installs = 0;

        let error = manager
            .ensure_requirements_with("example-skill", &requirements_path, || async {
                installs += 1;
                Ok(())
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("exceed the"));
        assert_eq!(installs, 0);
        assert!(!venv.join(REQUIREMENTS_FINGERPRINT_FILE).exists());
    }

    #[tokio::test]
    async fn requirements_snapshot_rejects_symlink_outside_skill_root() {
        let temp = tempdir().unwrap();
        let skill_root = temp.path().join("skills").join("example-skill");
        std::fs::create_dir_all(&skill_root).unwrap();
        let outside = temp.path().join("outside-requirements.txt");
        std::fs::write(&outside, "example-package==1.0\n").unwrap();
        if symlink_file(&outside, &skill_root.join("requirements.txt")).is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }

        let error = VenvManager::requirements_snapshot(&skill_root, "example-skill")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("path validation"));
    }

    #[tokio::test]
    async fn pip_install_receives_the_verified_requirements_snapshot() {
        let temp = tempdir().unwrap();
        let (_, requirements_path) = test_paths(temp.path());
        let (requirements_root, snapshot) = VenvManager::requirements_snapshot(
            requirements_path.parent().unwrap(),
            "example-skill",
        )
        .await
        .unwrap()
        .unwrap();
        std::fs::write(&requirements_path, "example-package==2.0\n").unwrap();

        let python = PathBuf::from("venvs/example-skill/python");
        let manager = VenvManager::new(temp.path().join("venvs"));
        let expected_snapshot = snapshot.clone();
        manager
            .ensure_requirements_snapshot_with(
                "example-skill",
                &requirements_root,
                &snapshot,
                |snapshot_path| async move {
                    let command = pip_install_command(&python, &snapshot_path);
                    let args = command
                        .as_std()
                        .get_args()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();

                    assert_eq!(
                        args.last(),
                        Some(&snapshot_path.to_string_lossy().into_owned())
                    );
                    assert_ne!(snapshot_path, requirements_path);
                    assert_eq!(
                        tokio::fs::read(snapshot_path).await.unwrap(),
                        expected_snapshot
                    );
                    Ok(())
                },
            )
            .await
            .unwrap();

        assert_eq!(snapshot, b"example-package==1.0\n");
    }

    #[tokio::test]
    async fn requirements_read_future_drops_its_reader_when_cancelled() {
        struct PendingReader(Arc<AtomicBool>);

        impl AsyncRead for PendingReader {
            fn poll_read(
                self: Pin<&mut Self>,
                _context: &mut Context<'_>,
                _buffer: &mut tokio::io::ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Pending
            }
        }

        impl Drop for PendingReader {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(read_requirements_capped(
            PendingReader(Arc::clone(&dropped)),
            "example-skill",
        ));
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn failed_install_does_not_persist_fingerprint_and_can_retry() {
        let temp = tempdir().unwrap();
        let (venv, requirements_path) = test_paths(temp.path());
        let first = VenvManager::new(temp.path().join("venvs"));
        let marker_path = venv.join(REQUIREMENTS_FINGERPRINT_FILE);
        let mut installs = 0;

        let failure = first
            .ensure_requirements_with("example-skill", &requirements_path, || async {
                installs += 1;
                contained_test_install("failure").await
            })
            .await;
        assert!(
            failure
                .expect_err("failed contained install must be reported")
                .to_string()
                .contains("pip install failed for skill 'example-skill'")
        );
        assert!(!marker_path.exists());

        let restarted_manager = VenvManager::new(temp.path().join("venvs"));
        restarted_manager
            .ensure_requirements_with("example-skill", &requirements_path, || async {
                installs += 1;
                contained_test_install("success").await
            })
            .await
            .unwrap();

        assert_eq!(installs, 2);
        assert_eq!(
            tokio::fs::read(marker_path).await.unwrap(),
            VenvManager::checksum_file(&requirements_path, "example-skill")
                .await
                .unwrap()
                .as_bytes()
        );
    }

    #[tokio::test]
    async fn contained_command_captures_stdout_and_stderr() {
        let output = run_contained_output(
            contained_test_command("success"),
            "example-skill",
            "test command failed",
        )
        .await
        .unwrap();

        assert!(output.status.success());
        assert!(encoding::decode_lossy(&output.stdout).contains("contained stdout marker"));
        assert!(encoding::decode_lossy(&output.stderr).contains("contained stderr marker"));
    }

    #[tokio::test]
    async fn contained_command_preserves_nonzero_status_and_stderr() {
        let output = run_contained_output(
            contained_test_command("failure"),
            "example-skill",
            "test command failed",
        )
        .await
        .unwrap();

        assert_eq!(output.status.code(), Some(17));
        assert!(encoding::decode_lossy(&output.stderr).contains("pip diagnostic marker"));
    }

    #[tokio::test]
    async fn contained_command_caps_retained_output_without_stopping_pipe_draining() {
        let output = run_contained_output(
            contained_test_command("large"),
            "example-skill",
            "test command failed",
        )
        .await
        .unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout.len(), MAX_SETUP_STREAM_BYTES);
        assert_eq!(output.stderr.len(), MAX_SETUP_STREAM_BYTES);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn dropping_contained_wait_kills_a_spawned_descendant() {
        const DESCENDANT_READY: &str = "HAVEN_SKILLS_CONTAINED_DESCENDANT_READY";
        const DESCENDANT_MARKER: &str = "HAVEN_SKILLS_CONTAINED_DESCENDANT_MARKER";

        let temp = tempdir().unwrap();
        let ready_path = temp.path().join("descendant-ready");
        let marker_path = temp.path().join("descendant-finished");
        let mut command = contained_test_command("tree");
        command
            .env(DESCENDANT_READY, &ready_path)
            .env(DESCENDANT_MARKER, &marker_path);
        let task = tokio::spawn(run_contained_output(
            command,
            "example-skill",
            "test command failed",
        ));

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready_path.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let descendant_started = ready_path.exists();
        task.abort();
        assert!(
            task.await.unwrap_err().is_cancelled(),
            "the contained command waiter should be cancelled"
        );
        assert!(
            descendant_started,
            "the contained child must start its descendant"
        );

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(
            !marker_path.exists(),
            "closing containment after dropping the waiter must kill the descendant"
        );
    }

    #[tokio::test]
    async fn contained_command_test_child() {
        match std::env::var(CONTAINED_CHILD_MODE).as_deref() {
            Ok("success") => {
                println!("contained stdout marker");
                eprintln!("contained stderr marker");
            }
            Ok("failure") => {
                eprintln!("pip diagnostic marker");
                std::process::exit(17);
            }
            Ok("large") => {
                let chunk = "x".repeat(8192);
                for _ in 0..160 {
                    println!("{chunk}");
                    eprintln!("{chunk}");
                }
            }
            Ok("secrets-cleared") => {
                assert!(std::env::var_os("GITHUB_TOKEN").is_none());
                assert!(std::env::var_os("PYTHONPATH").is_none());
            }
            #[cfg(windows)]
            Ok("tree") => {
                const DESCENDANT_READY: &str = "HAVEN_SKILLS_CONTAINED_DESCENDANT_READY";
                const DESCENDANT_MARKER: &str = "HAVEN_SKILLS_CONTAINED_DESCENDANT_MARKER";
                let ready_path: PathBuf = std::env::var_os(DESCENDANT_READY)
                    .expect("descendant ready path")
                    .into();
                let marker_path: PathBuf = std::env::var_os(DESCENDANT_MARKER)
                    .expect("descendant marker path")
                    .into();
                let mut command = std::process::Command::new(
                    std::env::current_exe().expect("test executable path"),
                );
                command
                    .args([
                        "--exact",
                        "venv::tests::contained_descendant_test_child",
                        "--nocapture",
                    ])
                    .env(DESCENDANT_MARKER, &marker_path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let mut child = {
                    let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
                    command.spawn()
                }
                .expect("spawn contained descendant");
                std::fs::write(ready_path, child.id().to_string())
                    .expect("record contained descendant pid");
                let _ = child.wait();
            }
            _ => {}
        }
    }

    #[cfg(windows)]
    #[test]
    fn contained_descendant_test_child() {
        let Some(marker_path) = std::env::var_os("HAVEN_SKILLS_CONTAINED_DESCENDANT_MARKER") else {
            return;
        };
        std::thread::sleep(std::time::Duration::from_secs(2));
        std::fs::write(marker_path, b"descendant survived").expect("write descendant marker");
    }
}
