//! Windows AppContainer workspace provisioning.
//!
//! This module prepares a private, low-integrity directory and a unique
//! AppContainer identity. It does not launch or contain a process; callers
//! must keep the owner alive until every process using the identity has exited.

use crate::filesystem::{
    DirectoryCapability, DirectoryTreeCopyLimits, DirectoryTreeCopySummary,
    DirectoryTreeDiffLimits, DirectoryTreeOutputManifest,
};
use crate::sandbox_output::SandboxOutputWorkspaceOwner;
use crate::sandbox_scratch::SandboxScratchLease;
use std::io;
use std::path::Path;
use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    ACL, ACL_REVISION, AddMandatoryAce, CONTAINER_INHERIT_ACE, CreateWellKnownSid,
    DACL_SECURITY_INFORMATION, FreeSid, InitializeAcl, LABEL_SECURITY_INFORMATION,
    OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, SECURITY_MAX_SID_SIZE, WinLowLabelSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ADD_SUBDIRECTORY, FILE_DELETE_CHILD, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows_sys::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_NO_WRITE_UP;

/// One unique AppContainer identity and its disposable writable file snapshot.
///
/// `execution_workspace_generation` and `policy_revision` are binding stamps
/// supplied by the authorization owner. They do not grant authority by
/// themselves. The profile has no optional capabilities, so it has no network
/// capability. The AppContainer process launcher must consume this owner and
/// revalidate the stamps immediately before process creation.
pub struct AppContainerWorkspace {
    directory: Option<DirectoryCapability>,
    baseline_directory: Option<DirectoryCapability>,
    scratch: Option<SandboxScratchLease>,
    profile: Option<AppContainerProfile>,
    execution_workspace_generation: u64,
    policy_revision: u64,
    snapshot: DirectoryTreeCopySummary,
}

impl AppContainerWorkspace {
    /// Create a unique AppContainer profile and a bounded, low-integrity copy
    /// of `source` in Haven's dedicated runtime-temp sandbox root. The root
    /// uses inter-process owner markers to recover crashed workspaces later.
    ///
    /// The source must not be modified concurrently when callers require a
    /// point-in-time snapshot. Cancellation or failure discards the temporary
    /// workspace. The returned owner must outlive every process launched with
    /// its AppContainer identity; dropping it early can orphan a live process
    /// from its intended workspace lifecycle.
    pub fn create(
        source: &DirectoryCapability,
        execution_workspace_generation: u64,
        policy_revision: u64,
        limits: DirectoryTreeCopyLimits,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<Self> {
        if is_cancelled() {
            return Err(cancelled());
        }
        let profile_name = AppContainerProfile::new_name();
        let mut scratch = SandboxScratchLease::create(&profile_name).map_err(|error| {
            io::Error::new(
                error.kind(),
                "could not create a private AppContainer scratch owner",
            )
        })?;
        let directory = DirectoryCapability::open(scratch.workspace_path())?;
        let baseline_directory = DirectoryCapability::open(scratch.baseline_path())?;
        let profile = AppContainerProfile::create(&profile_name)?;
        if let Err(error) = scratch.record_profile_created() {
            let profile_removed = profile.remove().is_ok();
            let directories_removed = scratch.remove_directories().is_ok();
            if profile_removed && directories_removed {
                let _ = scratch.remove_owner_marker();
            }
            return Err(error);
        }
        if is_cancelled() {
            return Err(cancelled());
        }

        let mut security_lease = WorkspaceSecurityLease::capture(directory.root())?;
        security_lease.grant_package_sid(profile.sid())?;
        security_lease.apply_low_integrity_label()?;
        let snapshot = source.copy_tree_to(&directory, limits, &mut is_cancelled)?;
        let baseline_summary =
            directory.copy_tree_to(&baseline_directory, limits, &mut is_cancelled)?;
        if baseline_summary != snapshot {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sandbox input baseline did not match the initial workspace snapshot",
            ));
        }
        security_lease.commit();

        Ok(Self {
            directory: Some(directory),
            baseline_directory: Some(baseline_directory),
            scratch: Some(scratch),
            profile: Some(profile),
            execution_workspace_generation,
            policy_revision,
            snapshot,
        })
    }

    pub fn root(&self) -> &Path {
        self.directory
            .as_ref()
            .expect("live AppContainer workspace retains its directory capability")
            .root()
    }

    pub(crate) fn directory(&self) -> &DirectoryCapability {
        self.directory
            .as_ref()
            .expect("live AppContainer workspace retains its directory capability")
    }

    pub fn execution_workspace_generation(&self) -> u64 {
        self.execution_workspace_generation
    }

    pub fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    pub fn snapshot_summary(&self) -> DirectoryTreeCopySummary {
        self.snapshot
    }

    /// Build a bounded manifest for file and directory changes without
    /// promoting any sandbox output into the canonical execution workspace.
    /// Call only after every process using this workspace has exited.
    pub(crate) fn output_manifest(
        &self,
        limits: DirectoryTreeDiffLimits,
        is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<DirectoryTreeOutputManifest> {
        let output = self
            .directory
            .as_ref()
            .expect("live AppContainer workspace retains its output directory");
        let baseline = self
            .baseline_directory
            .as_ref()
            .expect("live AppContainer workspace retains its input baseline");
        output.output_manifest(baseline, limits, is_cancelled)
    }

    #[cfg(test)]
    pub(crate) fn baseline_root_for_test(&self) -> &Path {
        self.baseline_directory
            .as_ref()
            .expect("live AppContainer workspace retains its input baseline")
            .root()
    }

    /// Explicitly discard the temporary tree and delete its unique profile.
    /// Call only after all AppContainer processes have exited.
    pub(crate) fn cleanup(mut self) -> io::Result<()> {
        self.cleanup_inner()
    }

    pub(crate) fn app_container_sid(&self) -> windows_sys::Win32::Security::PSID {
        self.profile
            .as_ref()
            .expect("live AppContainer workspace retains its profile")
            .sid()
    }

    fn cleanup_inner(&mut self) -> io::Result<()> {
        self.directory.take();
        self.baseline_directory.take();
        let mut cleanup_error = None;
        if let Some(mut scratch) = self.scratch.take() {
            let directories = scratch.remove_directories();
            if let Err(error) = &directories {
                cleanup_error = Some(io::Error::new(
                    error.kind(),
                    "could not remove private AppContainer scratch directories",
                ));
            }

            let profile = self
                .profile
                .take()
                .map_or(Ok(()), AppContainerProfile::remove);
            if let Err(error) = &profile
                && cleanup_error.is_none()
            {
                cleanup_error = Some(io::Error::new(
                    error.kind(),
                    "could not remove the private AppContainer profile",
                ));
            }

            if directories.is_ok()
                && profile.is_ok()
                && let Err(error) = scratch.remove_owner_marker()
                && cleanup_error.is_none()
            {
                cleanup_error = Some(io::Error::new(
                    error.kind(),
                    "could not remove the AppContainer scratch owner marker",
                ));
            }
        }
        cleanup_error.map_or(Ok(()), Err)
    }
}

impl SandboxOutputWorkspaceOwner for AppContainerWorkspace {
    fn execution_workspace_generation(&self) -> u64 {
        self.execution_workspace_generation
    }

    fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    fn initial_snapshot_byte_count(&self) -> u64 {
        self.snapshot.byte_count
    }

    fn promote_created_file(
        &self,
        change: &crate::filesystem::DirectoryTreeChange,
        destination: &DirectoryCapability,
        destination_relative: &Path,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> io::Result<crate::filesystem::DirectoryTreeFileCopySummary> {
        self.directory().copy_reviewed_created_file_to(
            change,
            destination,
            destination_relative,
            is_cancelled,
        )
    }

    fn cleanup(self: Box<Self>) -> io::Result<()> {
        (*self).cleanup()
    }
}

impl Drop for AppContainerWorkspace {
    fn drop(&mut self) {
        let _ = self.cleanup_inner();
    }
}

struct AppContainerProfile {
    name: Vec<u16>,
    sid: windows_sys::Win32::Security::PSID,
    active: bool,
}

// SAFETY: the SID pointer refers to immutable memory uniquely owned by this
// profile. Moving the owner does not move that allocation, and only `Drop` or
// `remove` frees it after callers stop borrowing the profile.
unsafe impl Send for AppContainerProfile {}

impl AppContainerProfile {
    fn new_name() -> String {
        let profile_id = haven_common::types::new_id("file");
        format!("HavenSandbox-{profile_id}")
    }

    fn create(profile_name: &str) -> io::Result<Self> {
        let name = wide_null(profile_name);
        let display_name = wide_null("Haven sandbox workspace");
        let description = wide_null("Short-lived isolated execution workspace");
        let mut sid = std::ptr::null_mut();
        // SAFETY: strings are nul-terminated and the output SID pointer is
        // writable. The returned profile and SID are owned by this guard.
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display_name.as_ptr(),
                description.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if result < 0 {
            if !sid.is_null() {
                // SAFETY: CreateAppContainerProfile allocated this SID.
                unsafe { FreeSid(sid) };
            }
            return Err(io::Error::from_raw_os_error(result));
        }
        if sid.is_null() {
            return Err(io::Error::other(
                "AppContainer profile creation returned no package SID",
            ));
        }
        Ok(Self {
            name,
            sid,
            active: true,
        })
    }

    fn sid(&self) -> windows_sys::Win32::Security::PSID {
        self.sid
    }

    fn remove(mut self) -> io::Result<()> {
        let result = self.delete_profile();
        if !self.sid.is_null() {
            // SAFETY: this is the owned package SID returned by profile create.
            unsafe { FreeSid(self.sid) };
            self.sid = std::ptr::null_mut();
        }
        result
    }

    fn delete_profile(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        // SAFETY: `name` is a valid NUL-terminated UTF-16 profile name.
        let result = unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        let _ = self.delete_profile();
        if !self.sid.is_null() {
            // SAFETY: the profile owner retains this allocation until drop.
            unsafe { FreeSid(self.sid) };
            self.sid = std::ptr::null_mut();
        }
    }
}

struct WorkspaceSecurityLease {
    path: Vec<u16>,
    original_dacl: *mut ACL,
    dacl_descriptor: PSECURITY_DESCRIPTOR,
    original_sacl: *mut ACL,
    label_descriptor: PSECURITY_DESCRIPTOR,
    dacl_changed: bool,
    label_changed: bool,
}

impl WorkspaceSecurityLease {
    fn capture(path: &Path) -> io::Result<Self> {
        let path = wide_null_os(path.as_os_str());
        let mut original_dacl = std::ptr::null_mut();
        let mut dacl_descriptor = std::ptr::null_mut();
        // SAFETY: `path` is nul-terminated and all output pointers are valid.
        let status = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut original_dacl,
                std::ptr::null_mut(),
                &mut dacl_descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if dacl_descriptor.is_null() || original_dacl.is_null() {
            if !dacl_descriptor.is_null() {
                // SAFETY: GetNamedSecurityInfoW allocated this descriptor.
                unsafe { LocalFree(dacl_descriptor.cast::<core::ffi::c_void>() as HLOCAL) };
            }
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private workspace has no explicit DACL",
            ));
        }

        let mut original_sacl = std::ptr::null_mut();
        let mut label_descriptor = std::ptr::null_mut();
        // LABEL_SECURITY_INFORMATION reads only the mandatory label and does
        // not require the audit-SACL privilege.
        let status = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut original_sacl,
                &mut label_descriptor,
            )
        };
        if status != 0 {
            // SAFETY: GetNamedSecurityInfoW allocated this descriptor.
            unsafe { LocalFree(dacl_descriptor.cast::<core::ffi::c_void>() as HLOCAL) };
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if label_descriptor.is_null() {
            // SAFETY: both descriptors are owned outputs from successful calls.
            unsafe {
                LocalFree(dacl_descriptor.cast::<core::ffi::c_void>() as HLOCAL);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "workspace label descriptor was not returned",
            ));
        }
        Ok(Self {
            path,
            original_dacl,
            dacl_descriptor,
            original_sacl,
            label_descriptor,
            dacl_changed: false,
            label_changed: false,
        })
    }

    fn grant_package_sid(
        &mut self,
        app_container_sid: windows_sys::Win32::Security::PSID,
    ) -> io::Result<()> {
        let trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: app_container_sid.cast(),
        };
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_GENERIC_READ
                | FILE_GENERIC_WRITE
                | FILE_ADD_SUBDIRECTORY
                | FILE_DELETE_CHILD
                | DELETE,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
            Trustee: trustee,
        };
        let mut updated_dacl = std::ptr::null_mut();
        // SAFETY: the package SID and original ACL stay valid for the call.
        let status = unsafe { SetEntriesInAclW(1, &access, self.original_dacl, &mut updated_dacl) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        // SAFETY: Windows copies the new ACL into the directory descriptor.
        let status = unsafe {
            SetNamedSecurityInfoW(
                self.path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                updated_dacl,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: SetEntriesInAclW allocated this ACL with LocalAlloc.
        unsafe { LocalFree(updated_dacl.cast::<core::ffi::c_void>() as HLOCAL) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        self.dacl_changed = true;
        Ok(())
    }

    fn apply_low_integrity_label(&mut self) -> io::Result<()> {
        let mut low_label_sid = [0_u8; SECURITY_MAX_SID_SIZE as usize];
        let mut sid_size = low_label_sid.len() as u32;
        // SAFETY: the fixed buffer is large enough for the well-known low SID.
        if unsafe {
            CreateWellKnownSid(
                WinLowLabelSid,
                std::ptr::null_mut(),
                low_label_sid.as_mut_ptr().cast(),
                &mut sid_size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }

        let mut label_acl_storage = vec![0_u64; 128];
        let label_acl = label_acl_storage.as_mut_ptr().cast::<ACL>();
        // SAFETY: the aligned buffer holds an ACL header and one mandatory ACE.
        if unsafe {
            InitializeAcl(
                label_acl,
                (label_acl_storage.len() * std::mem::size_of::<u64>()) as u32,
                ACL_REVISION,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both the ACL and low-label SID remain alive for the call.
        if unsafe {
            AddMandatoryAce(
                label_acl,
                ACL_REVISION,
                OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
                low_label_sid.as_mut_ptr().cast(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: LABEL_SECURITY_INFORMATION changes the inheritable mandatory
        // label on this disposable workspace, never the source project tree.
        let status = unsafe {
            SetNamedSecurityInfoW(
                self.path.as_ptr(),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                label_acl,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        self.label_changed = true;
        Ok(())
    }

    fn commit(mut self) {
        self.dacl_changed = false;
        self.label_changed = false;
    }

    fn restore(&mut self) -> io::Result<()> {
        let mut first_error = None;
        if self.label_changed {
            // SAFETY: the original SACL remains owned by label_descriptor.
            let status = unsafe {
                SetNamedSecurityInfoW(
                    self.path.as_ptr(),
                    SE_FILE_OBJECT,
                    LABEL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    self.original_sacl,
                )
            };
            if status == 0 {
                self.label_changed = false;
            } else {
                first_error = Some(io::Error::from_raw_os_error(status as i32));
            }
        }
        if self.dacl_changed {
            // SAFETY: the original DACL remains owned by dacl_descriptor.
            let status = unsafe {
                SetNamedSecurityInfoW(
                    self.path.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    self.original_dacl,
                    std::ptr::null_mut(),
                )
            };
            if status == 0 {
                self.dacl_changed = false;
            } else if first_error.is_none() {
                first_error = Some(io::Error::from_raw_os_error(status as i32));
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for WorkspaceSecurityLease {
    fn drop(&mut self) {
        let _ = self.restore();
        if !self.dacl_descriptor.is_null() {
            // SAFETY: GetNamedSecurityInfoW allocated this descriptor.
            unsafe {
                LocalFree(self.dacl_descriptor.cast::<core::ffi::c_void>() as HLOCAL);
            }
            self.dacl_descriptor = std::ptr::null_mut();
        }
        if !self.label_descriptor.is_null() {
            // SAFETY: GetNamedSecurityInfoW allocated this descriptor.
            unsafe {
                LocalFree(self.label_descriptor.cast::<core::ffi::c_void>() as HLOCAL);
            }
            self.label_descriptor = std::ptr::null_mut();
        }
    }
}

fn cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "sandbox workspace creation cancelled",
    )
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_null_os(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().chain(std::iter::once(0)).collect()
}
