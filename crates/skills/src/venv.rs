use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

use haven_common::encoding;

/// Manages per-skill virtual environments (M4-02).
///
/// Each skill gets an isolated venv at `<venv_root>/<skill_name>/`.
/// Creation is idempotent: `ensure` skips if the venv already exists.
/// If `requirements.txt` changes (detected by checksum), dependencies are
/// re-installed on the next `ensure` call.
#[derive(Clone)]
pub struct VenvManager {
    root: PathBuf,
    checksums: Arc<Mutex<Vec<(String, String)>>>,
}

impl VenvManager {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            checksums: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn venv_dir(&self, skill_name: &str) -> PathBuf {
        self.root.join(sanitize_name(skill_name))
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

    fn checksum_file(path: &Path) -> String {
        let content = match std::fs::read(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("checksum_file: failed to read {}: {}", path.display(), e);
                Vec::new()
            }
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// Ensure the venv exists for `skill_name`, optionally installing
    /// dependencies from `requirements.txt`. Idempotent.
    pub async fn ensure(&self, skill_name: &str, skill_root: &Path) -> anyhow::Result<PathBuf> {
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
        if req_path.exists() {
            let new_checksum = Self::checksum_file(&req_path);
            let changed = {
                let guard = self.checksums.lock().await;
                guard
                    .iter()
                    .find(|(name, _)| name == skill_name)
                    .map(|(_, cs)| cs != &new_checksum)
                    .unwrap_or(true)
            };

            if changed {
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
                let mut guard = self.checksums.lock().await;
                guard.retain(|(name, _)| name != skill_name);
                guard.push((skill_name.to_string(), new_checksum));
            }
        }

        Ok(python)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sanitize_name_replaces_special_chars() {
        let result = sanitize_name("my cool skill!");
        assert_eq!(result, "my_cool_skill_");
    }

    #[tokio::test]
    async fn venv_dir_returns_expected_path() {
        let mgr = VenvManager::new(PathBuf::from("/tmp/venvs"));
        let dir = mgr.venv_dir("test_skill");
        assert_eq!(dir, PathBuf::from("/tmp/venvs/test_skill"));
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
}
