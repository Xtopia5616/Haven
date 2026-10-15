//! Crash recovery for Haven-owned AppContainer scratch workspaces.
//!
//! Each workspace has a sibling owner lock and a bounded manifest in a
//! dedicated runtime-temp root. The lock prevents one Haven process from
//! reclaiming another process's workspace. Recovery only removes strictly
//! named directories and AppContainer profiles recorded by an aged, unlocked
//! owner file.

use crate::filesystem::{DirectoryCapability, DirectoryEntryKind};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};

const SANDBOX_SCRATCH_ROOT: &str = "appcontainer-workspaces";
const WORKSPACE_PREFIX: &str = "haven-appcontainer-workspace-";
const BASELINE_PREFIX: &str = "haven-sandbox-baseline-";
const OWNER_PREFIX: &str = "haven-appcontainer-owner-";
const OWNER_SUFFIX: &str = ".lock";
const MAX_ENTRIES_PER_SCAN: usize = 4096;
const MAX_OWNER_BYTES: usize = 1024;
const MAX_CLEANUPS_PER_PASS: usize = 8;
const DIRECTORY_DELETE_RETRIES: u8 = 20;
const DIRECTORY_DELETE_RETRY_DELAY: Duration = Duration::from_millis(25);
const STALE_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Owns both scratch trees and the inter-process marker protecting them.
///
/// Directory removal always uses the pinned capability root, and a failed
/// cleanup leaves the owner marker for a later recovery pass.
pub(crate) struct SandboxScratchLease {
    root: DirectoryCapability,
    workspace_name: String,
    baseline_name: String,
    workspace_path: PathBuf,
    baseline_path: PathBuf,
    owner_name: String,
    profile_name: String,
    owner_lock: Option<File>,
    workspace_created: bool,
    baseline_created: bool,
    profile_created: bool,
    marker_removed: bool,
}

impl SandboxScratchLease {
    pub(crate) fn create(profile_name: &str) -> io::Result<Self> {
        let root_path =
            haven_common::config::default_runtime_temp_root().join(SANDBOX_SCRATCH_ROOT);
        create_root_if_missing(&root_path)?;
        let root = DirectoryCapability::open(&root_path)?;
        cleanup_stale_workspaces(&root, SystemTime::now())?;
        Self::create_under_root(root, profile_name)
    }

    fn create_under_root(root: DirectoryCapability, profile_name: &str) -> io::Result<Self> {
        if !is_profile_name(profile_name) {
            return Err(invalid_data("invalid AppContainer profile identity"));
        }
        let workspace_name = format!("{WORKSPACE_PREFIX}{}", haven_common::types::new_id("file"));
        let baseline_name = format!("{BASELINE_PREFIX}{}", haven_common::types::new_id("file"));
        let owner_name = format!(
            "{OWNER_PREFIX}{}{OWNER_SUFFIX}",
            haven_common::types::new_id("file")
        );
        let mut owner_lock = root.create_new_file(Path::new(&owner_name))?;
        if let Err(error) = owner_lock.try_lock() {
            drop(owner_lock);
            let _ = root.remove_file(Path::new(&owner_name));
            return Err(error.into());
        }
        let record = OwnerRecord {
            workspace_name: workspace_name.clone(),
            baseline_name: baseline_name.clone(),
            profile_name: profile_name.to_owned(),
            workspace_created: false,
            baseline_created: false,
            profile_created: false,
        };
        if let Err(error) = owner_lock
            .write_all(record.encode().as_bytes())
            .and_then(|()| owner_lock.sync_all())
        {
            drop(owner_lock);
            let _ = root.remove_file(Path::new(&owner_name));
            return Err(error);
        }

        let mut lease = Self {
            workspace_path: root.root().join(&workspace_name),
            baseline_path: root.root().join(&baseline_name),
            root,
            workspace_name,
            baseline_name,
            owner_name,
            profile_name: profile_name.to_owned(),
            owner_lock: Some(owner_lock),
            workspace_created: false,
            baseline_created: false,
            profile_created: false,
            marker_removed: false,
        };
        if let Err(error) = lease.root.create_dir(Path::new(&lease.workspace_name)) {
            lease.cleanup_failed_creation();
            return Err(error);
        }
        lease.workspace_created = true;
        if let Err(error) = lease.persist_record() {
            lease.cleanup_failed_creation();
            return Err(error);
        }
        if let Err(error) = lease.root.create_dir(Path::new(&lease.baseline_name)) {
            lease.cleanup_failed_creation();
            return Err(error);
        }
        lease.baseline_created = true;
        if let Err(error) = lease.persist_record() {
            lease.cleanup_failed_creation();
            return Err(error);
        }
        Ok(lease)
    }

    pub(crate) fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }

    pub(crate) fn baseline_path(&self) -> &Path {
        &self.baseline_path
    }

    pub(crate) fn record_profile_created(&mut self) -> io::Result<()> {
        self.profile_created = true;
        if let Err(error) = self.persist_record() {
            self.profile_created = false;
            return Err(error);
        }
        Ok(())
    }

    /// Remove both trees relative to the pinned runtime root. The owner file
    /// stays locked until the caller has also removed the matching profile.
    pub(crate) fn remove_directories(&mut self) -> io::Result<()> {
        let mut first_error = None;
        if self.workspace_created
            && let Err(error) = remove_owned_directory(&self.root, &self.workspace_name)
        {
            first_error = Some(error);
        }
        if self.baseline_created
            && let Err(error) = remove_owned_directory(&self.root, &self.baseline_name)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
        first_error.map_or(Ok(()), Err)
    }

    fn persist_record(&mut self) -> io::Result<()> {
        let record = OwnerRecord {
            workspace_name: self.workspace_name.clone(),
            baseline_name: self.baseline_name.clone(),
            profile_name: self.profile_name.clone(),
            workspace_created: self.workspace_created,
            baseline_created: self.baseline_created,
            profile_created: self.profile_created,
        };
        let owner_lock = self
            .owner_lock
            .as_mut()
            .ok_or_else(|| invalid_data("sandbox owner lock is unavailable"))?;
        owner_lock.set_len(0)?;
        owner_lock.seek(SeekFrom::Start(0))?;
        owner_lock.write_all(record.encode().as_bytes())?;
        owner_lock.sync_all()
    }

    fn cleanup_failed_creation(&mut self) {
        let directories_removed = self.remove_directories().is_ok();
        let profile_removed = !self.profile_created || delete_profile(&self.profile_name).is_ok();
        if directories_removed && profile_removed {
            let _ = self.remove_owner_marker();
        }
    }

    pub(crate) fn remove_owner_marker(&mut self) -> io::Result<()> {
        self.root.remove_file(Path::new(&self.owner_name))?;
        self.marker_removed = true;
        self.owner_lock.take();
        Ok(())
    }
}

impl Drop for SandboxScratchLease {
    fn drop(&mut self) {
        if !self.marker_removed {
            // Keep any remaining data available to the next locked recovery
            // pass. No ambient-path cleanup runs from this fallback.
            self.owner_lock.take();
        }
    }
}

fn create_root_if_missing(path: &Path) -> io::Result<()> {
    match std::fs::create_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) => Err(io::Error::new(
            error.kind(),
            "could not prepare the private AppContainer scratch root",
        )),
    }
}

fn is_owned_name(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|id| haven_common::types::is_canonical_id(id, "file"))
}

fn is_owner_name(value: &str) -> bool {
    value
        .strip_prefix(OWNER_PREFIX)
        .and_then(|suffix| suffix.strip_suffix(OWNER_SUFFIX))
        .is_some_and(|id| haven_common::types::is_canonical_id(id, "file"))
}

fn is_profile_name(value: &str) -> bool {
    value
        .strip_prefix("HavenSandbox-")
        .is_some_and(|id| haven_common::types::is_canonical_id(id, "file"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnerRecord {
    workspace_name: String,
    baseline_name: String,
    profile_name: String,
    workspace_created: bool,
    baseline_created: bool,
    profile_created: bool,
}

impl OwnerRecord {
    fn encode(&self) -> String {
        format!(
            "haven-appcontainer-scratch-v1\nworkspace={}\nbaseline={}\nprofile={}\nworkspace_created={}\nbaseline_created={}\nprofile_created={}\n",
            self.workspace_name,
            self.baseline_name,
            self.profile_name,
            self.workspace_created,
            self.baseline_created,
            self.profile_created,
        )
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| invalid_data("sandbox scratch owner record is not UTF-8"))?;
        let mut lines = text.lines();
        if lines.next() != Some("haven-appcontainer-scratch-v1") {
            return Err(invalid_data(
                "sandbox scratch owner record version is invalid",
            ));
        }
        let workspace_name = line_value(lines.next(), "workspace")?;
        let baseline_name = line_value(lines.next(), "baseline")?;
        let profile_name = line_value(lines.next(), "profile")?;
        let workspace_created = line_bool(lines.next(), "workspace_created")?;
        let baseline_created = line_bool(lines.next(), "baseline_created")?;
        let profile_created = line_bool(lines.next(), "profile_created")?;
        if lines.next().is_some()
            || !is_owned_name(&workspace_name, WORKSPACE_PREFIX)
            || !is_owned_name(&baseline_name, BASELINE_PREFIX)
            || !is_profile_name(&profile_name)
        {
            return Err(invalid_data(
                "sandbox scratch owner record fields are invalid",
            ));
        }
        Ok(Self {
            workspace_name,
            baseline_name,
            profile_name,
            workspace_created,
            baseline_created,
            profile_created,
        })
    }
}

fn line_value(line: Option<&str>, key: &str) -> io::Result<String> {
    line.and_then(|line| line.strip_prefix(key))
        .and_then(|value| value.strip_prefix('='))
        .map(str::to_owned)
        .ok_or_else(|| invalid_data("sandbox scratch owner record is incomplete"))
}

fn line_bool(line: Option<&str>, key: &str) -> io::Result<bool> {
    match line_value(line, key)?.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid_data("sandbox scratch owner flag is invalid")),
    }
}

fn cleanup_stale_workspaces(root: &DirectoryCapability, now: SystemTime) -> io::Result<usize> {
    cleanup_stale_workspaces_with(root, now, delete_profile)
}

fn cleanup_stale_workspaces_with(
    root: &DirectoryCapability,
    now: SystemTime,
    mut remove_profile: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<usize> {
    let entries = root.entries(MAX_ENTRIES_PER_SCAN)?;
    let mut removed = 0;
    for entry in entries {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        if entry.kind != DirectoryEntryKind::File || !is_owner_name(name) {
            continue;
        }
        let relative = Path::new(name);
        let mut owner_lock = match root.open_read_write(relative) {
            Ok(owner_lock) => owner_lock,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => continue,
        };
        if owner_lock.try_lock().is_err() {
            continue;
        }
        let modified = match owner_lock
            .metadata()
            .and_then(|metadata| metadata.modified())
        {
            Ok(modified) => modified,
            Err(_) => continue,
        };
        if now.duration_since(modified).unwrap_or_default() < STALE_AGE {
            continue;
        }
        let record = match read_owner_record(&mut owner_lock) {
            Ok(record) => record,
            Err(_) => continue,
        };
        if (record.workspace_created
            && remove_owned_directory(root, &record.workspace_name).is_err())
            || (record.baseline_created
                && remove_owned_directory(root, &record.baseline_name).is_err())
            || (record.profile_created && remove_profile(&record.profile_name).is_err())
        {
            continue;
        }
        if root.remove_file(relative).is_ok() {
            removed += 1;
            if removed >= MAX_CLEANUPS_PER_PASS {
                break;
            }
        }
    }
    Ok(removed)
}

fn read_owner_record(owner_lock: &mut File) -> io::Result<OwnerRecord> {
    owner_lock.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    (&mut *owner_lock)
        .take((MAX_OWNER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_OWNER_BYTES {
        return Err(invalid_data("sandbox scratch owner record is too large"));
    }
    OwnerRecord::decode(&bytes)
}

fn remove_owned_directory(root: &DirectoryCapability, name: &str) -> io::Result<()> {
    let relative = Path::new(name);
    if !is_owned_name(name, WORKSPACE_PREFIX) && !is_owned_name(name, BASELINE_PREFIX) {
        return Err(invalid_data("sandbox scratch directory name is invalid"));
    }
    let mut retries = 0;
    loop {
        match root.remove_dir_all(relative) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error)
                if retries < DIRECTORY_DELETE_RETRIES && is_transient_delete_lock(&error) =>
            {
                retries += 1;
                std::thread::sleep(DIRECTORY_DELETE_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
}

fn is_transient_delete_lock(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        code == ERROR_SHARING_VIOLATION as i32 || code == ERROR_LOCK_VIOLATION as i32
    })
}

fn delete_profile(profile_name: &str) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Isolation::DeleteAppContainerProfile;

    if !is_profile_name(profile_name) {
        return Err(invalid_data("AppContainer profile name is invalid"));
    }
    let name = std::ffi::OsStr::new(profile_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: the profile name is validated and NUL-terminated UTF-16.
    let status = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    if status >= 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status))
    }
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_name() -> String {
        format!("HavenSandbox-{}", haven_common::types::new_id("file"))
    }

    #[test]
    fn owner_record_round_trips_and_rejects_untrusted_names() {
        let record = OwnerRecord {
            workspace_name: format!("{WORKSPACE_PREFIX}{}", haven_common::types::new_id("file")),
            baseline_name: format!("{BASELINE_PREFIX}{}", haven_common::types::new_id("file")),
            profile_name: profile_name(),
            workspace_created: false,
            baseline_created: false,
            profile_created: false,
        };
        assert_eq!(
            OwnerRecord::decode(record.encode().as_bytes()).unwrap(),
            record
        );

        let mut traversal = record.clone();
        traversal.workspace_name = "..\\outside".into();
        let mut unknown_profile = record.clone();
        unknown_profile.profile_name = "HavenSandbox-arbitrary".into();
        assert!(OwnerRecord::decode(traversal.encode().as_bytes()).is_err());
        assert!(OwnerRecord::decode(unknown_profile.encode().as_bytes()).is_err());
    }

    #[test]
    fn stale_cleanup_keeps_live_fresh_malformed_and_unrelated_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("scratch-root");
        std::fs::create_dir(&root_path).unwrap();
        let root = DirectoryCapability::open(&root_path).unwrap();
        let profile = profile_name();
        let stale_workspace = format!("{WORKSPACE_PREFIX}{}", haven_common::types::new_id("file"));
        let stale_baseline = format!("{BASELINE_PREFIX}{}", haven_common::types::new_id("file"));
        let stale_marker = format!(
            "{OWNER_PREFIX}{}{}",
            haven_common::types::new_id("file"),
            OWNER_SUFFIX
        );
        root.create_dir(Path::new(&stale_workspace)).unwrap();
        root.create_dir(Path::new(&stale_baseline)).unwrap();
        let stale_record = OwnerRecord {
            workspace_name: stale_workspace.clone(),
            baseline_name: stale_baseline.clone(),
            profile_name: profile,
            workspace_created: true,
            baseline_created: true,
            profile_created: true,
        };
        let mut stale_lock = root.create_new_file(Path::new(&stale_marker)).unwrap();
        stale_lock
            .write_all(stale_record.encode().as_bytes())
            .unwrap();
        stale_lock.sync_all().unwrap();
        stale_lock
            .set_modified(SystemTime::now() - STALE_AGE - Duration::from_secs(1))
            .unwrap();
        drop(stale_lock);

        let live_marker = format!(
            "{OWNER_PREFIX}{}{}",
            haven_common::types::new_id("file"),
            OWNER_SUFFIX
        );
        let live_lock = root.create_new_file(Path::new(&live_marker)).unwrap();
        live_lock.try_lock().unwrap();
        live_lock
            .set_modified(SystemTime::now() - STALE_AGE - Duration::from_secs(1))
            .unwrap();

        let fresh_marker = format!(
            "{OWNER_PREFIX}{}{}",
            haven_common::types::new_id("file"),
            OWNER_SUFFIX
        );
        let mut fresh_lock = root.create_new_file(Path::new(&fresh_marker)).unwrap();
        fresh_lock.write_all(b"incomplete").unwrap();

        let malformed_marker = format!(
            "{OWNER_PREFIX}{}{}",
            haven_common::types::new_id("file"),
            OWNER_SUFFIX
        );
        let mut malformed_lock = root.create_new_file(Path::new(&malformed_marker)).unwrap();
        malformed_lock
            .write_all(b"invalid stale owner record")
            .unwrap();
        malformed_lock
            .set_modified(SystemTime::now() - STALE_AGE - Duration::from_secs(1))
            .unwrap();
        drop(malformed_lock);

        std::fs::write(root_path.join("notes.txt"), b"keep").unwrap();
        let removed = cleanup_stale_workspaces(&root, SystemTime::now()).unwrap();
        assert_eq!(
            removed, 1,
            "only stale, unlocked owner records are reclaimed"
        );
        assert!(!root_path.join(&stale_workspace).exists());
        assert!(!root_path.join(&stale_baseline).exists());
        assert!(!root_path.join(&stale_marker).exists());
        assert!(root_path.join("notes.txt").exists());
        assert!(root_path.join(&live_marker).exists());
        assert!(root_path.join(&fresh_marker).exists());
        assert!(root_path.join(&malformed_marker).exists());
        drop(live_lock);
        drop(fresh_lock);
    }

    #[test]
    fn scratch_lease_uses_a_pinned_root_and_unique_owner_marker() {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("scratch-root");
        std::fs::create_dir(&root_path).unwrap();
        let root = DirectoryCapability::open(&root_path).unwrap();
        let lease = SandboxScratchLease::create_under_root(root, &profile_name()).unwrap();

        assert!(lease.workspace_path().starts_with(&root_path));
        assert!(lease.baseline_path().starts_with(&root_path));
        assert!(lease.owner_lock.is_some());
        assert!(is_owner_name(&lease.owner_name));
    }

    #[test]
    fn failed_profile_cleanup_keeps_the_owner_marker_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("scratch-root");
        std::fs::create_dir(&root_path).unwrap();
        let root = DirectoryCapability::open(&root_path).unwrap();
        let workspace_name = format!("{WORKSPACE_PREFIX}{}", haven_common::types::new_id("file"));
        let baseline_name = format!("{BASELINE_PREFIX}{}", haven_common::types::new_id("file"));
        root.create_dir(Path::new(&workspace_name)).unwrap();
        root.create_dir(Path::new(&baseline_name)).unwrap();
        let marker = format!(
            "{OWNER_PREFIX}{}{}",
            haven_common::types::new_id("file"),
            OWNER_SUFFIX
        );
        let record = OwnerRecord {
            workspace_name: workspace_name.clone(),
            baseline_name: baseline_name.clone(),
            profile_name: profile_name(),
            workspace_created: true,
            baseline_created: true,
            profile_created: true,
        };
        let mut owner = root.create_new_file(Path::new(&marker)).unwrap();
        owner.write_all(record.encode().as_bytes()).unwrap();
        owner.sync_all().unwrap();
        owner
            .set_modified(SystemTime::now() - STALE_AGE - Duration::from_secs(1))
            .unwrap();
        drop(owner);

        let removed = cleanup_stale_workspaces_with(&root, SystemTime::now(), |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected profile cleanup failure",
            ))
        })
        .unwrap();

        assert_eq!(removed, 0);
        assert!(root_path.join(marker).exists());
        assert!(!root_path.join(workspace_name).exists());
        assert!(!root_path.join(baseline_name).exists());
    }

    #[test]
    fn stale_owner_record_does_not_remove_paths_it_did_not_create() {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("scratch-root");
        std::fs::create_dir(&root_path).unwrap();
        let root = DirectoryCapability::open(&root_path).unwrap();
        let workspace_name = format!("{WORKSPACE_PREFIX}{}", haven_common::types::new_id("file"));
        let baseline_name = format!("{BASELINE_PREFIX}{}", haven_common::types::new_id("file"));
        root.create_dir(Path::new(&workspace_name)).unwrap();
        root.create_dir(Path::new(&baseline_name)).unwrap();
        std::fs::write(root_path.join(&workspace_name).join("keep.txt"), b"keep").unwrap();
        let marker = format!(
            "{OWNER_PREFIX}{}{}",
            haven_common::types::new_id("file"),
            OWNER_SUFFIX
        );
        let record = OwnerRecord {
            workspace_name: workspace_name.clone(),
            baseline_name: baseline_name.clone(),
            profile_name: profile_name(),
            workspace_created: false,
            baseline_created: false,
            profile_created: false,
        };
        let mut owner = root.create_new_file(Path::new(&marker)).unwrap();
        owner.write_all(record.encode().as_bytes()).unwrap();
        owner.sync_all().unwrap();
        owner
            .set_modified(SystemTime::now() - STALE_AGE - Duration::from_secs(1))
            .unwrap();
        drop(owner);

        let removed = cleanup_stale_workspaces_with(&root, SystemTime::now(), |_| {
            panic!("a profile not owned by this record must not be deleted")
        })
        .unwrap();

        assert_eq!(removed, 1);
        assert!(root_path.join(&workspace_name).join("keep.txt").exists());
        assert!(root_path.join(&baseline_name).exists());
        assert!(!root_path.join(marker).exists());
    }

    #[test]
    fn capability_created_scratch_entries_are_new_and_confined() {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("scratch-root");
        std::fs::create_dir(&root_path).unwrap();
        let root = DirectoryCapability::open(&root_path).unwrap();

        root.create_dir(Path::new("attempt")).unwrap();
        assert!(root.create_dir(Path::new("attempt")).is_err());
        assert!(root.create_dir(Path::new("..\\outside")).is_err());
        let file = root
            .create_new_file(Path::new("attempt/owner.lock"))
            .unwrap();
        drop(file);
        assert!(
            root.create_new_file(Path::new("attempt/owner.lock"))
                .is_err()
        );
        assert!(!temp.path().join("outside").exists());
    }
}
