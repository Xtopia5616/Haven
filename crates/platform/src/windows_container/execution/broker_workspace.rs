//! Service-private ownership for broker execution attempts.
//!
//! The root lives below the protected Haven installation directory and its
//! DACL grants access only to SYSTEM, Administrators, and the restricted
//! broker service SID. One attempt at a time owns the workspace input,
//! capacity-bounded HCS transport, output staging, and recovery marker. Failed
//! cleanup keeps the owner marker; uncertain HCS work is never auto-reclaimed.

use crate::filesystem::{DirectoryCapability, DirectoryEntryKind};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT};

use super::named_pipe::broker_service_sid_string;

const RUNTIME_DIRECTORY_NAME: &str = "ContainerBrokerRuntime";
const OWNER_PREFIX: &str = "owner-";
const OWNER_SUFFIX: &str = ".lock";
const MAX_ROOT_ENTRIES: usize = 4_096;
const MAX_CLEANUPS_PER_PASS: usize = 8;
const STALE_ATTEMPT_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const ACTIVE_HCS_ATTEMPT_MARKER: &[u8] = b"hcs-export-active:v1";
const UNCERTAIN_ATTEMPT_MARKER: &[u8] = b"hcs-export-uncertain:v1";

/// Pins and cleans the single service-private staging root.
pub(crate) struct BrokerWorkspaceScratchRoot {
    root: DirectoryCapability,
}

/// Owns one private workspace attempt and its crash-recovery lock record.
pub(crate) struct BrokerWorkspaceAttempt {
    root: DirectoryCapability,
    attempt_name: String,
    owner_name: String,
    attempt_root: Option<DirectoryCapability>,
    workspace: Option<DirectoryCapability>,
    owner_lock: Option<File>,
    cleaned: bool,
}

impl BrokerWorkspaceScratchRoot {
    #[cfg(test)]
    pub(crate) fn for_test(root: DirectoryCapability) -> Self {
        Self { root }
    }

    /// Prepare the fixed runtime directory during explicit elevated install.
    /// Its parent is the protected `%ProgramFiles%\Haven` directory.
    pub(crate) fn install(program_files: &Path) -> io::Result<()> {
        let haven_directory = program_files.join("Haven");
        DirectoryCapability::open(&haven_directory)?;
        let runtime_directory = haven_directory.join(RUNTIME_DIRECTORY_NAME);
        let sid = broker_service_sid_string()?;
        install_protected_directory(&runtime_directory, &sid, BrokerServiceDirectoryAccess::Full)
    }

    /// Open the installed runtime root under the broker's restricted token.
    pub(crate) fn open(program_files: &Path) -> io::Result<Self> {
        let runtime_directory = program_files.join("Haven").join(RUNTIME_DIRECTORY_NAME);
        Ok(Self {
            root: DirectoryCapability::open(&runtime_directory)?,
        })
    }

    /// Reclaim only old, unlocked and strictly described attempt directories.
    /// A young or malformed marker is retained and will block a new attempt.
    pub(crate) fn cleanup_stale(&self, now: SystemTime) -> io::Result<usize> {
        let entries = self.root.entries(MAX_ROOT_ENTRIES)?;
        let mut cleaned = 0;
        for entry in entries {
            if cleaned >= MAX_CLEANUPS_PER_PASS || entry.kind != DirectoryEntryKind::File {
                continue;
            }
            let Some(owner_name) = entry.name.to_str() else {
                continue;
            };
            let Some(attempt_name) = attempt_for_owner_name(owner_name) else {
                continue;
            };
            let Some(modified) = entry.modified else {
                continue;
            };
            let Ok(age) = now.duration_since(modified) else {
                continue;
            };
            if age < STALE_ATTEMPT_AGE {
                continue;
            }
            if self.cleanup_stale_owner(owner_name, &attempt_name)? {
                cleaned += 1;
            }
        }
        Ok(cleaned)
    }

    /// Create a new attempt only when the service root is empty. Input,
    /// transport, and output staging share this one private attempt directory.
    pub(crate) fn create_attempt(&self) -> io::Result<BrokerWorkspaceAttempt> {
        self.cleanup_stale(SystemTime::now())?;
        if !self.root.entries(MAX_ROOT_ENTRIES)?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "broker workspace root is occupied by an active or unrecovered attempt",
            ));
        }

        let attempt_name = haven_common::types::new_id("file");
        let owner_name = owner_file_name(&attempt_name);
        let owner_lock = self.root.create_new_file(Path::new(&owner_name))?;
        if let Err(error) = owner_lock.try_lock() {
            drop(owner_lock);
            let _ = self.root.remove_file(Path::new(&owner_name));
            return Err(error.into());
        }
        if let Err(error) = owner_lock.sync_all() {
            drop(owner_lock);
            let _ = self.root.remove_file(Path::new(&owner_name));
            return Err(error);
        }
        if let Err(error) = self.root.create_dir(Path::new(&attempt_name)) {
            drop(owner_lock);
            let _ = self.root.remove_file(Path::new(&owner_name));
            return Err(error);
        }
        let attempt_path = self.root.root().join(&attempt_name);
        let attempt_root = match DirectoryCapability::open(&attempt_path) {
            Ok(attempt_root) => attempt_root,
            Err(error) => {
                let removed = remove_attempt_directory(&self.root, &attempt_name).is_ok();
                if removed {
                    drop(owner_lock);
                    let _ = self.root.remove_file(Path::new(&owner_name));
                }
                return Err(error);
            }
        };
        if let Err(error) = attempt_root.create_dir(Path::new("workspace-input")) {
            drop(attempt_root);
            let removed = remove_attempt_directory(&self.root, &attempt_name).is_ok();
            if removed {
                drop(owner_lock);
                let _ = self.root.remove_file(Path::new(&owner_name));
            }
            return Err(error);
        }
        let workspace_path = attempt_root.root().join("workspace-input");
        let workspace = match DirectoryCapability::open(&workspace_path) {
            Ok(workspace) => workspace,
            Err(error) => {
                drop(attempt_root);
                let removed = remove_attempt_directory(&self.root, &attempt_name).is_ok();
                if removed {
                    drop(owner_lock);
                    let _ = self.root.remove_file(Path::new(&owner_name));
                }
                return Err(error);
            }
        };

        Ok(BrokerWorkspaceAttempt {
            root: self.root.clone(),
            attempt_name,
            owner_name,
            attempt_root: Some(attempt_root),
            workspace: Some(workspace),
            owner_lock: Some(owner_lock),
            cleaned: false,
        })
    }

    fn cleanup_stale_owner(&self, owner_name: &str, expected_attempt: &str) -> io::Result<bool> {
        let mut owner_lock = self.root.open_read_write(Path::new(owner_name))?;
        if owner_lock.try_lock().is_err() {
            return Ok(false);
        }
        let mut marker = Vec::new();
        (&mut owner_lock).take(64).read_to_end(&mut marker)?;
        if !marker.is_empty() {
            // Unknown or explicitly uncertain state is never automatically
            // reclaimed. Only the original empty active-attempt marker is
            // eligible for stale cleanup.
            return Ok(false);
        }
        // The marker name carries a strict canonical attempt ID and is
        // created and synced before that random path. The root is
        // service-private, so it remains authoritative if the process stops
        // between marker creation and mkdir.
        remove_attempt_directory(&self.root, expected_attempt)?;
        self.root.remove_file(Path::new(owner_name))?;
        drop(owner_lock);
        Ok(true)
    }
}

/// Create or re-secure a broker-owned directory with a protected DACL.
/// `service_read_only` grants the restricted service SID read/execute only;
/// SYSTEM and Administrators retain full control for recovery and provisioning.
pub(super) fn install_protected_directory(
    path: &Path,
    service_sid: &str,
    service_access: BrokerServiceDirectoryAccess,
) -> io::Result<()> {
    let descriptor = RuntimeDirectoryDescriptor::new(service_sid, service_access)?;
    let wide_path = wide_null_path(path)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };
    if unsafe { CreateDirectoryW(wide_path.as_ptr(), &attributes) } == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_ALREADY_EXISTS {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(permission_denied(
                "broker-owned path is not a regular directory",
            ));
        }
    }
    descriptor.apply(path)?;
    DirectoryCapability::open(path)?;
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum BrokerServiceDirectoryAccess {
    Full,
    ReadOnly,
}

impl BrokerWorkspaceAttempt {
    pub(crate) fn attempt_root(&self) -> io::Result<&DirectoryCapability> {
        self.attempt_root
            .as_ref()
            .ok_or_else(|| invalid_data("broker workspace attempt is already cleaned"))
    }

    pub(crate) fn staging(&self) -> io::Result<&DirectoryCapability> {
        self.workspace
            .as_ref()
            .ok_or_else(|| invalid_data("broker workspace attempt is already cleaned"))
    }

    /// Create a distinct, empty host-side root for read-only Skill code and
    /// parameters. It is never merged into the project workspace snapshot.
    pub(crate) fn create_skill_input(&self) -> io::Result<DirectoryCapability> {
        let attempt_root = self.attempt_root()?;
        attempt_root.create_dir(Path::new("skill-input"))?;
        DirectoryCapability::open(&attempt_root.root().join("skill-input"))
    }

    pub(crate) fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        if self.owner_marker_is_nonempty() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "HCS attempt must be quiescent or quarantined before cleanup",
            ));
        }
        self.workspace.take();
        self.attempt_root.take();
        remove_attempt_directory(&self.root, &self.attempt_name)?;
        self.root.remove_file(Path::new(&self.owner_name))?;
        self.owner_lock.take();
        self.cleaned = true;
        Ok(())
    }

    /// Quarantine an attempt when native HCS work may still use its staging.
    /// The non-empty owner marker prevents both new attempts and automatic
    /// stale cleanup; an operator must resolve the HCS state before removal.
    pub(crate) fn preserve_for_recovery(&mut self) -> io::Result<()> {
        let marker_result = self.write_owner_marker(UNCERTAIN_ATTEMPT_MARKER);
        self.workspace.take();
        self.attempt_root.take();
        self.owner_lock.take();
        self.cleaned = true;
        marker_result
    }

    /// Persist an active HCS marker before attaching a transport disk or
    /// starting native HCS work. A process crash then cannot make the stale
    /// sweeper reclaim storage whose OS owner is uncertain.
    pub(crate) fn mark_hcs_operation_active(&mut self) -> io::Result<()> {
        self.write_owner_marker(ACTIVE_HCS_ATTEMPT_MARKER)
    }

    /// Clear the active marker only after all HCS callers have returned, the
    /// compute system is stopped, and any transport VHD has been detached.
    pub(crate) fn mark_hcs_operation_quiescent(&mut self) -> io::Result<()> {
        let owner_lock = self
            .owner_lock
            .as_mut()
            .ok_or_else(|| invalid_data("broker attempt has no owner marker"))?;
        owner_lock.seek(SeekFrom::Start(0))?;
        let mut marker = Vec::new();
        (&mut *owner_lock).take(64).read_to_end(&mut marker)?;
        if marker.is_empty() {
            return Ok(());
        }
        if marker != ACTIVE_HCS_ATTEMPT_MARKER {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "uncertain HCS attempt cannot be marked quiescent automatically",
            ));
        }
        owner_lock.set_len(0)?;
        owner_lock.sync_all()
    }

    fn write_owner_marker(&mut self, marker: &[u8]) -> io::Result<()> {
        let owner_lock = self
            .owner_lock
            .as_mut()
            .ok_or_else(|| invalid_data("broker attempt has no owner marker"))?;
        // Never truncate a durable active marker before writing its stronger
        // recovery state. A failed/short disk write must not turn an uncertain
        // attempt back into the empty marker eligible for stale cleanup.
        owner_lock.seek(SeekFrom::Start(0))?;
        owner_lock.write_all(marker)?;
        owner_lock.sync_all()
    }

    #[cfg(test)]
    fn abandon_for_test(mut self) {
        self.workspace.take();
        self.attempt_root.take();
        self.owner_lock.take();
        self.cleaned = true;
    }
}

impl Drop for BrokerWorkspaceAttempt {
    fn drop(&mut self) {
        if !self.cleaned && self.owner_marker_is_nonempty() {
            // HCS may still own a mounted volume or be writing the export
            // transport. A non-empty marker opts this attempt out of Drop
            // cleanup just as it opts it out of stale recovery.
            self.workspace.take();
            self.attempt_root.take();
            self.owner_lock.take();
            self.cleaned = true;
            return;
        }
        let _ = self.cleanup();
    }
}

impl BrokerWorkspaceAttempt {
    fn owner_marker_is_nonempty(&mut self) -> bool {
        let Some(owner_lock) = self.owner_lock.as_mut() else {
            return true;
        };
        if owner_lock.seek(SeekFrom::Start(0)).is_err() {
            return true;
        }
        let mut marker = [0_u8; 1];
        match owner_lock.read(&mut marker) {
            Ok(length) => length != 0,
            Err(_) => true,
        }
    }
}

fn owner_file_name(attempt_name: &str) -> String {
    format!("{OWNER_PREFIX}{attempt_name}{OWNER_SUFFIX}")
}

fn attempt_for_owner_name(owner_name: &str) -> Option<String> {
    let attempt_name = owner_name
        .strip_prefix(OWNER_PREFIX)?
        .strip_suffix(OWNER_SUFFIX)?;
    haven_common::types::is_canonical_id(attempt_name, "file").then(|| attempt_name.to_owned())
}

fn remove_attempt_directory(root: &DirectoryCapability, name: &str) -> io::Result<()> {
    match root.remove_dir_all(Path::new(name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn wide_null_path(path: &Path) -> io::Result<Vec<u16>> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid_input("broker runtime path is not valid Unicode"))?;
    if !path.is_absolute()
        || text.starts_with("\\\\")
        || text.chars().any(char::is_control)
        || text
            .split(['\\', '/'])
            .any(|component| component == "." || component == "..")
    {
        return Err(invalid_input(
            "broker runtime path is not local and absolute",
        ));
    }
    Ok(text.encode_utf16().chain([0]).collect())
}

struct RuntimeDirectoryDescriptor(*mut core::ffi::c_void);

impl RuntimeDirectoryDescriptor {
    fn new(service_sid: &str, service_access: BrokerServiceDirectoryAccess) -> io::Result<Self> {
        if service_sid.is_empty() || service_sid.chars().any(char::is_control) {
            return Err(invalid_input("broker service SID string is invalid"));
        }
        let service_rights = match service_access {
            BrokerServiceDirectoryAccess::Full => "FA",
            BrokerServiceDirectoryAccess::ReadOnly => "GRGX",
        };
        let sddl =
            format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;{service_rights};;;{service_sid})");
        let wide_sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide_sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
            || descriptor.is_null()
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn as_ptr(&self) -> *mut core::ffi::c_void {
        self.0
    }

    fn apply(&self, path: &Path) -> io::Result<()> {
        let wide_path = wide_null_path(path)?;
        let mut dacl_present = 0;
        let mut dacl = std::ptr::null_mut();
        let mut dacl_defaulted = 0;
        if unsafe {
            GetSecurityDescriptorDacl(self.0, &mut dacl_present, &mut dacl, &mut dacl_defaulted)
        } == 0
            || dacl_present == 0
            || dacl.is_null()
            || dacl_defaulted != 0
        {
            return Err(invalid_data("broker runtime DACL is malformed"));
        }
        let result = unsafe {
            SetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null_mut(),
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(result as i32))
        }
    }
}

impl Drop for RuntimeDirectoryDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn permission_denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_service_directory_dacl_is_valid() {
        let descriptor = RuntimeDirectoryDescriptor::new(
            "S-1-5-80-1-2-3-4-5",
            BrokerServiceDirectoryAccess::ReadOnly,
        )
        .unwrap();
        assert!(!descriptor.as_ptr().is_null());
    }

    fn root() -> (tempfile::TempDir, BrokerWorkspaceScratchRoot) {
        let temp = tempfile::tempdir().unwrap();
        let root = BrokerWorkspaceScratchRoot {
            root: DirectoryCapability::open(temp.path()).unwrap(),
        };
        (temp, root)
    }

    #[test]
    fn attempt_cleanup_removes_only_its_workspace_and_owner_record() {
        let (_temp, root) = root();
        let mut attempt = root.create_attempt().unwrap();
        let attempt_root = attempt.attempt_root().unwrap();
        assert_eq!(
            attempt.staging().unwrap().root().parent(),
            Some(attempt_root.root())
        );
        assert_eq!(
            attempt.staging().unwrap().root().file_name(),
            Some(std::ffi::OsStr::new("workspace-input"))
        );
        attempt
            .staging()
            .unwrap()
            .atomic_replace(Path::new("input.txt"), b"private workspace")
            .unwrap();

        attempt.cleanup().unwrap();

        assert!(root.root.entries(MAX_ROOT_ENTRIES).unwrap().is_empty());
    }

    #[test]
    fn active_attempt_cannot_be_reclaimed_or_overlapped() {
        let (_temp, root) = root();
        let attempt = root.create_attempt().unwrap();
        let future = SystemTime::now() + STALE_ATTEMPT_AGE + Duration::from_secs(1);

        assert_eq!(root.cleanup_stale(future).unwrap(), 0);
        assert_eq!(
            root.create_attempt().err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(attempt);
        assert!(root.root.entries(MAX_ROOT_ENTRIES).unwrap().is_empty());
    }

    #[test]
    fn preserved_attempt_blocks_new_work_and_is_never_auto_reclaimed() {
        let (_temp, root) = root();
        let mut attempt = root.create_attempt().unwrap();
        let attempt_name = attempt.attempt_name.clone();
        attempt.preserve_for_recovery().unwrap();
        drop(attempt);

        assert_eq!(
            root.create_attempt().err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        let stale_time = SystemTime::now() + STALE_ATTEMPT_AGE + Duration::from_secs(1);
        assert_eq!(root.cleanup_stale(stale_time).unwrap(), 0);
        assert!(root.root.root().join(attempt_name).exists());
        assert_eq!(root.root.entries(MAX_ROOT_ENTRIES).unwrap().len(), 2);
    }

    #[test]
    fn active_hcs_marker_prevents_crash_left_attempt_reclamation() {
        let (_temp, root) = root();
        let mut attempt = root.create_attempt().unwrap();
        let attempt_name = attempt.attempt_name.clone();
        let owner_name = attempt.owner_name.clone();
        attempt.mark_hcs_operation_active().unwrap();
        attempt.abandon_for_test();

        let now = SystemTime::now() + STALE_ATTEMPT_AGE + Duration::from_secs(1);
        assert_eq!(root.cleanup_stale(now).unwrap(), 0);
        assert!(root.root.root().join(attempt_name).exists());
        assert_eq!(
            std::fs::read(root.root.root().join(owner_name)).unwrap(),
            ACTIVE_HCS_ATTEMPT_MARKER
        );
    }

    #[test]
    fn dropping_an_active_hcs_attempt_retains_its_staging() {
        let (_temp, root) = root();
        let mut attempt = root.create_attempt().unwrap();
        let attempt_name = attempt.attempt_name.clone();
        attempt.mark_hcs_operation_active().unwrap();
        drop(attempt);

        assert_eq!(root.root.entries(MAX_ROOT_ENTRIES).unwrap().len(), 2);
        assert!(root.root.root().join(attempt_name).exists());
    }

    #[test]
    fn active_hcs_attempt_requires_quiescence_before_explicit_cleanup() {
        let (_temp, root) = root();
        let mut attempt = root.create_attempt().unwrap();
        attempt.mark_hcs_operation_active().unwrap();

        assert_eq!(
            attempt.cleanup().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(attempt.attempt_root().unwrap().root().exists());
        attempt.mark_hcs_operation_quiescent().unwrap();
        attempt.cleanup().unwrap();
        assert!(root.root.entries(MAX_ROOT_ENTRIES).unwrap().is_empty());
    }

    #[test]
    fn stale_unlocked_attempt_is_reclaimed_from_its_strict_owner_record() {
        let (_temp, root) = root();
        let attempt = root.create_attempt().unwrap();
        let attempt_name = attempt.attempt_name.clone();
        attempt.abandon_for_test();

        let now = SystemTime::now() + STALE_ATTEMPT_AGE + Duration::from_secs(1);
        assert_eq!(root.cleanup_stale(now).unwrap(), 1);
        assert!(root.root.entries(MAX_ROOT_ENTRIES).unwrap().is_empty());
        assert!(!root.root.root().join(attempt_name).exists());
    }

    #[test]
    fn unowned_or_ambiguous_entries_are_preserved_and_block_new_attempts() {
        let (_temp, root) = root();
        let orphan_name = haven_common::types::new_id("file");
        root.root.create_dir(Path::new(&orphan_name)).unwrap();
        let now = SystemTime::now() + STALE_ATTEMPT_AGE + Duration::from_secs(1);

        assert_eq!(root.cleanup_stale(now).unwrap(), 0);
        assert!(root.root.root().join(&orphan_name).exists());
        assert_eq!(
            root.create_attempt().err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
