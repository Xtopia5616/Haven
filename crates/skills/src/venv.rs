use std::future::Future;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use haven_common::encoding;

const REQUIREMENTS_FINGERPRINT_FILE: &str = ".haven-requirements-fingerprint";

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
            let output = tokio::process::Command::new("python")
                .arg("-m")
                .arg("venv")
                .arg(&venv)
                .current_dir(haven_common::default_work_dir())
                .output()
                .await
                .map_err(|e| {
                    anyhow::anyhow!("failed to create venv for '{}': {}", skill_name, e)
                })?;
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
                let output = tokio::process::Command::new(&python)
                    .arg("-m")
                    .arg("pip")
                    .arg("install")
                    .arg("-r")
                    .arg(&req_path)
                    .current_dir(haven_common::default_work_dir())
                    .output()
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("pip install failed for '{}': {}", skill_name, e)
                    })?;
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

        let failure = first
            .ensure_requirements_with("example-skill", &requirements_path, || async {
                anyhow::bail!("pip failed")
            })
            .await;
        assert!(failure.is_err());
        assert!(!marker_path.exists());

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
        assert_eq!(
            tokio::fs::read(marker_path).await.unwrap(),
            VenvManager::checksum_file(&requirements_path)
                .unwrap()
                .as_bytes()
        );
    }
}
