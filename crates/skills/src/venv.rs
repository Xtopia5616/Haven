use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use sha2::{Digest, Sha256};
use tokio::process::Command;

use haven_common::encoding;
use haven_platform::process_containment::ProcessContainment;

const REQUIREMENTS_FINGERPRINT_FILE: &str = ".haven-requirements-fingerprint";

fn venv_creation_command(venv: &Path) -> Command {
    let mut command = Command::new("python");
    command
        .arg("-m")
        .arg("venv")
        .arg(venv)
        .current_dir(haven_common::default_work_dir());
    command
}

fn pip_install_command(python: &Path, requirements_path: &Path) -> Command {
    let mut command = Command::new(python);
    command
        .arg("-m")
        .arg("pip")
        .arg("install")
        .arg("-r")
        .arg(requirements_path)
        .current_dir(haven_common::default_work_dir());
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

    let containment = ProcessContainment::new().map_err(|error| {
        anyhow::anyhow!(
            "failed to create process containment for skill '{}': {}",
            skill_name,
            error
        )
    })?;
    containment.prepare_command(command.as_std_mut(), 0);
    let mut child = command
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| anyhow::anyhow!("{spawn_error_context} for '{skill_name}': {error}"))?;
    let pid = child.id().ok_or_else(|| {
        anyhow::anyhow!("skill '{}' child process did not expose a pid", skill_name)
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
            skill_name,
            error
        );
    }

    child
        .wait_with_output()
        .await
        .map_err(|error| anyhow::anyhow!("{spawn_error_context} for '{skill_name}': {error}"))
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

    fn requirements_path(skill_root: &Path) -> PathBuf {
        skill_root.join("requirements.txt")
    }

    fn checksum_file(path: &Path) -> anyhow::Result<String> {
        let content = std::fs::read(path)
            .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?;
        Ok(format!("sha256:{:x}", Sha256::digest(content)))
    }

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
        let fingerprint = Self::checksum_file(requirements_path)?;
        let marker_path = self
            .venv_dir(skill_name)
            .join(REQUIREMENTS_FINGERPRINT_FILE);
        let installed_fingerprint = match tokio::fs::read(&marker_path).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "failed to read requirements fingerprint for skill '{}': {}",
                    skill_name,
                    error
                ));
            }
        };

        if installed_fingerprint.as_deref() == Some(fingerprint.as_bytes()) {
            return Ok(());
        }

        install().await?;

        // Persist only after pip succeeds. A failed install therefore remains
        // eligible for retry on the next ensure call.
        tokio::fs::write(&marker_path, fingerprint.as_bytes())
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to save requirements fingerprint for skill '{}': {}",
                    skill_name,
                    error
                )
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
            tracing::info!(
                "Creating venv for skill '{}' at {}",
                skill_name,
                venv.display()
            );
            tokio::fs::create_dir_all(&venv).await?;
            let output = run_contained_output(
                venv_creation_command(&venv),
                skill_name,
                "failed to create venv",
            )
            .await?;
            if !output.status.success() {
                let stderr = encoding::decode_lossy(&output.stderr);
                anyhow::bail!("venv creation failed for '{}': {}", skill_name, stderr);
            }
        }

        let req_path = Self::requirements_path(skill_root);
        let requirements_file = match tokio::fs::metadata(&req_path).await {
            Ok(metadata) if metadata.is_file() => true,
            Ok(_) => {
                anyhow::bail!(
                    "requirements path for skill '{}' is not a file: {}",
                    skill_name,
                    req_path.display()
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "failed to inspect requirements for skill '{}': {}",
                    skill_name,
                    error
                ));
            }
        };
        if requirements_file {
            self.ensure_requirements_with(skill_name, &req_path, || async {
                tracing::info!("Installing requirements for skill '{}'", skill_name);
                let output = run_contained_output(
                    pip_install_command(&python, &req_path),
                    skill_name,
                    "pip install failed",
                )
                .await?;
                if !output.status.success() {
                    let stderr = encoding::decode_lossy(&output.stderr);
                    anyhow::bail!("pip install for '{}' failed: {}", skill_name, stderr);
                }
                Ok(())
            })
            .await?;
        }

        Ok(python)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            let stderr = encoding::decode_lossy(&output.stderr);
            anyhow::bail!("pip install for 'example-skill' failed: {stderr}");
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
        let work_dir = haven_common::default_work_dir();
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
        assert_eq!(create.as_std().get_current_dir(), Some(work_dir.as_path()));

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
                "-r",
                requirements.to_string_lossy().as_ref()
            ]
        );
        assert_eq!(install.as_std().get_current_dir(), Some(work_dir.as_path()));
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
                .contains("pip diagnostic marker")
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
            VenvManager::checksum_file(&requirements_path)
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
                let child = std::process::Command::new(
                    std::env::current_exe().expect("test executable path"),
                )
                .args([
                    "--exact",
                    "venv::tests::contained_descendant_test_child",
                    "--nocapture",
                ])
                .env(DESCENDANT_MARKER, &marker_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn contained descendant");
                std::fs::write(ready_path, child.id().to_string())
                    .expect("record contained descendant pid");
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
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
