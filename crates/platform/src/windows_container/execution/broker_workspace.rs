//! Service-private scratch ownership for broker workspace snapshots.
//!
//! The root lives below the protected Haven installation directory and its
//! DACL grants access only to SYSTEM, Administrators, and the restricted
//! broker service SID. A single attempt may occupy the root at a time, which
//! bounds retained workspace payload bytes to one request's existing 4 GiB
//! transfer ceiling. Failed cleanup keeps an owner marker for recovery; an
//! explicitly uncertain HCS export is quarantined from automatic recovery.

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
const UNCERTAIN_ATTEMPT_MARKER: &[u8] = b"hcs-export-uncertain:v1";

/// Pins and cleans the single service-private staging root.
pub(crate) struct BrokerWorkspaceScratchRoot {
    root: DirectoryCapability,
}

/// Owns one private input snapshot and its crash-recovery lock record.
pub(crate) struct BrokerWorkspaceAttempt {
    root: DirectoryCapability,
    workspace_name: String,
    owner_name: String,
    workspace: Option<DirectoryCapability>,
    owner_lock: Option<File>,
    cleaned: bool,
}

impl BrokerWorkspaceScratchRoot {
    /// Prepare the fixed runtime directory during explicit elevated install.
    /// Its parent is the protected `%ProgramFiles%\Haven` directory.
    pub(crate) fn install(program_files: &Path) -> io::Result<()> {
        let haven_directory = program_files.join("Haven");
        DirectoryCapability::open(&haven_directory)?;
        let runtime_directory = haven_directory.join(RUNTIME_DIRECTORY_NAME);
        let sid = broker_service_sid_string()?;
        let descriptor = RuntimeDirectoryDescriptor::new(&sid)?;
        let wide_path = wide_null_path(&runtime_directory)?;
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
            let metadata = std::fs::symlink_metadata(&runtime_directory)?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(permission_denied(
                    "broker runtime path is not a regular directory",
                ));
            }
        }
        descriptor.apply(&runtime_directory)?;
        DirectoryCapability::open(&runtime_directory)?;
        Ok(())
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
            let Some(workspace_name) = workspace_for_owner_name(owner_name) else {
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
            if self.cleanup_stale_owner(owner_name, &workspace_name)? {
                cleaned += 1;
            }
        }
        Ok(cleaned)
    }

    /// Create a new attempt only when the service root is empty. This makes
    /// active and crash-left workspaces mutually exclusive and caps retained
    /// workspace bytes to the per-request transfer ceiling.
    pub(crate) fn create_attempt(&self) -> io::Result<BrokerWorkspaceAttempt> {
        self.cleanup_stale(SystemTime::now())?;
        if !self.root.entries(MAX_ROOT_ENTRIES)?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "broker workspace root is occupied by an active or unrecovered attempt",
            ));
        }

        let workspace_name = haven_common::types::new_id("file");
        let owner_name = owner_file_name(&workspace_name);
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
        if let Err(error) = self.root.create_dir(Path::new(&workspace_name)) {
            drop(owner_lock);
            let _ = self.root.remove_file(Path::new(&owner_name));
            return Err(error);
        }
        let workspace_path = self.root.root().join(&workspace_name);
        let workspace = match DirectoryCapability::open(&workspace_path) {
            Ok(workspace) => workspace,
            Err(error) => {
                let removed = remove_workspace_directory(&self.root, &workspace_name).is_ok();
                if removed {
                    drop(owner_lock);
                    let _ = self.root.remove_file(Path::new(&owner_name));
                }
                return Err(error);
            }
        };

        Ok(BrokerWorkspaceAttempt {
            root: self.root.clone(),
            workspace_name,
            owner_name,
            workspace: Some(workspace),
            owner_lock: Some(owner_lock),
            cleaned: false,
        })
    }

    fn cleanup_stale_owner(&self, owner_name: &str, expected_workspace: &str) -> io::Result<bool> {
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
        // The marker name carries a strict canonical workspace ID and is
        // created and synced before that random path. The root is
        // service-private, so it remains authoritative if the process stops
        // between marker creation and mkdir.
        remove_workspace_directory(&self.root, expected_workspace)?;
        self.root.remove_file(Path::new(owner_name))?;
        drop(owner_lock);
        Ok(true)
    }
}

impl BrokerWorkspaceAttempt {
    pub(crate) fn staging(&self) -> io::Result<&DirectoryCapability> {
        self.workspace
            .as_ref()
            .ok_or_else(|| invalid_data("broker workspace attempt is already cleaned"))
    }

    pub(crate) fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        self.workspace.take();
        remove_workspace_directory(&self.root, &self.workspace_name)?;
        self.root.remove_file(Path::new(&self.owner_name))?;
        self.owner_lock.take();
        self.cleaned = true;
        Ok(())
    }

    /// Quarantine an attempt when native HCS work may still use its staging.
    /// The non-empty owner marker prevents both new attempts and automatic
    /// stale cleanup; an operator must resolve the HCS state before removal.
    pub(crate) fn preserve_for_recovery(&mut self) -> io::Result<()> {
        let marker_result = (|| {
            let owner_lock = self
                .owner_lock
                .as_mut()
                .ok_or_else(|| invalid_data("broker attempt has no owner marker to quarantine"))?;
            owner_lock.set_len(0)?;
            owner_lock.seek(SeekFrom::Start(0))?;
            owner_lock.write_all(UNCERTAIN_ATTEMPT_MARKER)?;
            owner_lock.sync_all()
        })();
        self.workspace.take();
        self.owner_lock.take();
        self.cleaned = true;
        marker_result
    }

    #[cfg(test)]
    fn abandon_for_test(mut self) {
        self.workspace.take();
        self.owner_lock.take();
        self.cleaned = true;
    }
}

impl Drop for BrokerWorkspaceAttempt {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn owner_file_name(workspace_name: &str) -> String {
    format!("{OWNER_PREFIX}{workspace_name}{OWNER_SUFFIX}")
}

fn workspace_for_owner_name(owner_name: &str) -> Option<String> {
    let workspace_name = owner_name
        .strip_prefix(OWNER_PREFIX)?
        .strip_suffix(OWNER_SUFFIX)?;
    haven_common::types::is_canonical_id(workspace_name, "file").then(|| workspace_name.to_owned())
}

fn remove_workspace_directory(root: &DirectoryCapability, name: &str) -> io::Result<()> {
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
    fn new(service_sid: &str) -> io::Result<Self> {
        if service_sid.is_empty() || service_sid.chars().any(char::is_control) {
            return Err(invalid_input("broker service SID string is invalid"));
        }
        let sddl = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{service_sid})");
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
        let workspace_name = attempt.workspace_name.clone();
        attempt.preserve_for_recovery().unwrap();
        drop(attempt);

        assert_eq!(
            root.create_attempt().err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        let stale_time = SystemTime::now() + STALE_ATTEMPT_AGE + Duration::from_secs(1);
        assert_eq!(root.cleanup_stale(stale_time).unwrap(), 0);
        assert!(root.root.root().join(workspace_name).exists());
        assert_eq!(root.root.entries(MAX_ROOT_ENTRIES).unwrap().len(), 2);
    }

    #[test]
    fn stale_unlocked_attempt_is_reclaimed_from_its_strict_owner_record() {
        let (_temp, root) = root();
        let attempt = root.create_attempt().unwrap();
        let workspace_name = attempt.workspace_name.clone();
        attempt.abandon_for_test();

        let now = SystemTime::now() + STALE_ATTEMPT_AGE + Duration::from_secs(1);
        assert_eq!(root.cleanup_stale(now).unwrap(), 1);
        assert!(root.root.entries(MAX_ROOT_ENTRIES).unwrap().is_empty());
        assert!(!root.root.root().join(workspace_name).exists());
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
