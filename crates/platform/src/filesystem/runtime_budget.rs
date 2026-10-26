//! Bounded, non-reparse directory accounting shared by process adapters.

use super::DirectoryCapability;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirectoryUsage {
    pub bytes: u64,
    pub entries: usize,
}

pub fn directory_usage(
    root: &Path,
    max_bytes: u64,
    max_entries: usize,
) -> Result<DirectoryUsage, String> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DirectoryUsage::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    if super::is_link_or_reparse_point(&root_metadata) || !root_metadata.is_dir() {
        return Err("runtime directory is not a safe local directory".into());
    }
    DirectoryCapability::open(root)
        .map_err(|_| "runtime directory could not be safely opened".to_owned())?;

    let mut pending = vec![root.to_path_buf()];
    let mut usage = DirectoryUsage::default();
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                return Err("runtime directory could not be inspected safely".into());
            }
        };
        for entry in entries {
            let entry = entry.map_err(|_| "runtime directory could not be inspected safely")?;
            usage.entries = usage.entries.saturating_add(1);
            if usage.entries > max_entries {
                return Ok(usage);
            }
            let metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err("runtime directory could not be inspected safely".into());
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() || super::is_link_or_reparse_point(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                usage.bytes = usage.bytes.saturating_add(metadata.len());
                if usage.bytes > max_bytes {
                    return Ok(usage);
                }
            }
        }
    }
    Ok(usage)
}

pub fn directory_size_exceeds(root: &Path, limit: u64, max_entries: usize) -> Result<bool, String> {
    let usage = directory_usage(root, limit, max_entries)?;
    Ok(usage.bytes > limit || usage.entries > max_entries)
}

#[derive(Default)]
struct DirectoryBudgetState {
    observed_bytes: HashMap<PathBuf, u64>,
    aggregate_violation_owner: Option<PathBuf>,
}

/// Tracks the observed size of active scratch directories against a shared
/// aggregate budget. The checks remain polling based, so this is a monitored
/// limit rather than an operating-system quota.
pub struct DirectoryBudgetPool {
    max_total_bytes: u64,
    state: Mutex<DirectoryBudgetState>,
}

impl DirectoryBudgetPool {
    pub fn new(max_total_bytes: u64) -> Self {
        Self {
            max_total_bytes,
            state: Mutex::new(DirectoryBudgetState::default()),
        }
    }

    pub fn track(
        self: &Arc<Self>,
        path: &Path,
        max_bytes: u64,
        max_entries: usize,
    ) -> Result<DirectoryBudgetLease, String> {
        DirectoryCapability::open(path)
            .map_err(|_| "runtime directory could not be safely opened".to_owned())?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.observed_bytes.contains_key(path) {
            return Err("runtime directory is already tracked".into());
        }
        state.observed_bytes.insert(path.to_path_buf(), 0);
        drop(state);
        Ok(DirectoryBudgetLease {
            inner: Arc::new(DirectoryBudgetLeaseInner {
                pool: Arc::clone(self),
                path: path.to_path_buf(),
                max_bytes,
                max_entries,
                last_usage_log: Mutex::new(None),
            }),
        })
    }

    fn update(&self, path: &Path, bytes: u64) -> (u64, bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.observed_bytes.insert(path.to_path_buf(), bytes);
        let total_bytes = state
            .observed_bytes
            .values()
            .copied()
            .fold(0u64, u64::saturating_add);
        if total_bytes > self.max_total_bytes {
            if state.aggregate_violation_owner.is_none() {
                state.aggregate_violation_owner = Some(path.to_path_buf());
            }
        } else {
            state.aggregate_violation_owner = None;
        }
        (
            total_bytes,
            state.aggregate_violation_owner.as_deref() == Some(path),
        )
    }

    fn remove(&self, path: &Path) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.observed_bytes.remove(path);
        if state.aggregate_violation_owner.as_deref() == Some(path) {
            state.aggregate_violation_owner = None;
        }
    }
}

struct DirectoryBudgetLeaseInner {
    pool: Arc<DirectoryBudgetPool>,
    path: PathBuf,
    max_bytes: u64,
    max_entries: usize,
    last_usage_log: Mutex<Option<Instant>>,
}

impl Drop for DirectoryBudgetLeaseInner {
    fn drop(&mut self) {
        self.pool.remove(&self.path);
    }
}

/// Cloneable observation handle for one active scratch directory. Dropping
/// the final clone removes its observed size from the shared budget.
#[derive(Clone)]
pub struct DirectoryBudgetLease {
    inner: Arc<DirectoryBudgetLeaseInner>,
}

impl DirectoryBudgetLease {
    pub fn check_exceeded(&self) -> Result<bool, String> {
        let scan_started = Instant::now();
        let usage = directory_usage(
            &self.inner.path,
            self.inner.max_bytes,
            self.inner.max_entries,
        )?;
        let (total_bytes, aggregate_exceeded) =
            self.inner.pool.update(&self.inner.path, usage.bytes);
        let local_exceeded =
            usage.bytes > self.inner.max_bytes || usage.entries > self.inner.max_entries;
        let now = Instant::now();
        let mut last_log = self
            .inner
            .last_usage_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last_log.is_none_or(|last_log| now.duration_since(last_log) >= Duration::from_secs(60)) {
            tracing::debug!(
                bytes = usage.bytes,
                entries = usage.entries,
                scan_duration_ms = scan_started.elapsed().as_millis() as u64,
                total_scratch_bytes = total_bytes,
                directory_limit_bytes = self.inner.max_bytes,
                aggregate_limit_bytes = self.inner.pool.max_total_bytes,
                "runtime scratch capacity sample"
            );
            *last_log = Some(now);
        }
        if local_exceeded || aggregate_exceeded {
            tracing::warn!(
                bytes = usage.bytes,
                entries = usage.entries,
                total_scratch_bytes = total_bytes,
                directory_limit_bytes = self.inner.max_bytes,
                aggregate_limit_bytes = self.inner.pool.max_total_bytes,
                local_exceeded,
                aggregate_exceeded,
                "runtime scratch capacity exceeded"
            );
        }
        Ok(local_exceeded || aggregate_exceeded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_budget_enforces_local_and_aggregate_limits_and_releases_usage() {
        let root = tempfile::tempdir().unwrap();
        let first_path = root.path().join("first");
        let second_path = root.path().join("second");
        std::fs::create_dir(&first_path).unwrap();
        std::fs::create_dir(&second_path).unwrap();
        std::fs::write(first_path.join("payload"), b"123456").unwrap();
        std::fs::write(second_path.join("payload"), b"abcdef").unwrap();

        let pool = Arc::new(DirectoryBudgetPool::new(10));
        let first = pool.track(&first_path, 8, 10).unwrap();
        let second = pool.track(&second_path, 8, 10).unwrap();
        assert!(!first.check_exceeded().unwrap());
        assert!(second.check_exceeded().unwrap());
        drop(second);
        assert!(!first.check_exceeded().unwrap());
        std::fs::write(first_path.join("extra"), b"123").unwrap();
        assert!(first.check_exceeded().unwrap());
    }

    #[test]
    fn directory_size_exceeds_keeps_missing_directory_behavior() {
        let root = tempfile::tempdir().unwrap();
        assert!(!directory_size_exceeds(&root.path().join("missing"), 0, 0).unwrap());
    }
}
