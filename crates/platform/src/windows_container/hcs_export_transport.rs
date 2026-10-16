//! Broker-private bounded VHDX transport volume for HCS layer export.

use super::scratch_vhd::{BoundedScratchVhd, OwnedVhdHandle};
use crate::filesystem::DirectoryCapability;
use crate::windows_container::execution::broker_workspace::BrokerWorkspaceAttempt;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, FreeLibrary, HANDLE, HMODULE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::Win32::Storage::Vhd::{
    ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER, ATTACH_VIRTUAL_DISK_FLAG_PERMANENT_LIFETIME,
    ATTACH_VIRTUAL_DISK_PARAMETERS, ATTACH_VIRTUAL_DISK_PARAMETERS_0,
    ATTACH_VIRTUAL_DISK_PARAMETERS_0_0, ATTACH_VIRTUAL_DISK_VERSION_1, AttachVirtualDisk,
    DETACH_VIRTUAL_DISK_FLAG_NONE, DetachVirtualDisk, VIRTUAL_DISK_ACCESS_ATTACH_RW,
    VIRTUAL_DISK_ACCESS_DETACH, VIRTUAL_DISK_ACCESS_GET_INFO, VIRTUAL_STORAGE_TYPE,
    VIRTUAL_STORAGE_TYPE_DEVICE_VHDX, VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
const EXPORT_TRANSPORT_DIRECTORY: &str = "export";
#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
const MAX_HCS_VOLUME_PATH_UNITS: usize = 512;

/// Owns a per-attempt, capacity-bounded HCS-formatted volume for synchronous
/// layer export. The VHDX stays attached with permanent lifetime until
/// explicit cleanup; uncertain HCS outcomes must call `preserve_for_recovery`
/// and keep the containing broker attempt quarantined.
#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
pub(crate) struct BoundedHcsExportTransport {
    scratch: Option<BoundedScratchVhd>,
    attached: Option<AttachedVhd>,
    volume_root: PathBuf,
    directory: Option<DirectoryCapability>,
    cleaned: bool,
}

#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
impl BoundedHcsExportTransport {
    /// Create, HCS-format, attach, and protect one transport volume. The
    /// active owner marker is written before any privileged storage operation
    /// so a service crash cannot hand this disk to stale cleanup.
    pub(crate) fn create(
        attempt: &mut BrokerWorkspaceAttempt,
        maximum_size_bytes: u64,
    ) -> io::Result<Self> {
        attempt.mark_hcs_operation_active()?;
        let attempt_root = attempt.attempt_root()?.clone();
        let mut scratch = BoundedScratchVhd::create(attempt_root, maximum_size_bytes)?;
        scratch.format_for_container()?;

        let attached = AttachedVhd::attach(&scratch.path())?;
        let volume_root = get_layer_vhd_mount_path(attached.raw())?;
        if !crate::filesystem::is_canonical_hcs_volume_root(&volume_root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HCS returned an invalid transport volume path",
            ));
        }

        let mut transport = Self {
            scratch: Some(scratch),
            attached: Some(attached),
            volume_root,
            directory: None,
            cleaned: false,
        };
        transport.initialize_export_directory()?;
        Ok(transport)
    }

    /// Return a pinned capability for the empty export destination.
    pub(crate) fn directory(&self) -> io::Result<&DirectoryCapability> {
        self.directory.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "HCS export transport is not mounted and ready",
            )
        })
    }

    /// Return the backing disk path for the helper's disk-to-volume check.
    pub(crate) fn vhdx_path(&self) -> io::Result<PathBuf> {
        self.scratch
            .as_ref()
            .map(BoundedScratchVhd::path)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "HCS export transport is already cleaned",
                )
            })
    }

    pub(crate) fn maximum_size_bytes(&self) -> io::Result<u64> {
        self.scratch
            .as_ref()
            .map(BoundedScratchVhd::maximum_size_bytes)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "HCS export transport is already cleaned",
                )
            })
    }

    /// Detach the volume before deleting its backing VHDX. A failure retains
    /// both the volume and backing disk for explicit recovery.
    pub(crate) fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        self.directory.take();
        if let Some(attached) = self.attached.as_mut()
            && let Err(error) = attached.detach()
        {
            self.preserve_for_recovery();
            return Err(error);
        }
        self.attached.take();

        if let Some(scratch) = self.scratch.as_mut()
            && let Err(error) = scratch.cleanup()
        {
            scratch.preserve_for_recovery();
            self.scratch.take();
            self.cleaned = true;
            return Err(error);
        }
        self.scratch.take();
        self.cleaned = true;
        Ok(())
    }

    /// Leave a permanently attached disk in place when HCS may still be
    /// writing through the caller's path. The broker attempt marker must also
    /// remain non-empty so stale cleanup cannot remove its backing VHDX.
    pub(crate) fn preserve_for_recovery(&mut self) {
        self.directory.take();
        if let Some(attached) = self.attached.as_mut() {
            attached.preserve_for_recovery();
        }
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.preserve_for_recovery();
        }
        self.attached.take();
        self.scratch.take();
        self.cleaned = true;
    }

    fn initialize_export_directory(&mut self) -> io::Result<()> {
        let export_path = self.volume_root.join(EXPORT_TRANSPORT_DIRECTORY);
        std::fs::create_dir(&export_path)?;
        secure_export_directory(&export_path)?;
        let directory = DirectoryCapability::open_hcs_volume_child(
            &self.volume_root,
            EXPORT_TRANSPORT_DIRECTORY,
        )?;
        if !directory.entries(1)?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "new HCS export transport directory is not empty",
            ));
        }
        self.directory = Some(directory);
        Ok(())
    }
}

impl Drop for BoundedHcsExportTransport {
    fn drop(&mut self) {
        if !self.cleaned {
            // Only the orchestration path that observed HCS completion may
            // detach. A dropped owner cannot prove that the synchronous HCS
            // caller or service-side writer has stopped.
            self.preserve_for_recovery();
        }
    }
}

#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
struct AttachedVhd {
    disk: OwnedVhdHandle,
    attached: bool,
    detach_on_drop: bool,
}

#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
impl AttachedVhd {
    fn attach(path: &Path) -> io::Result<Self> {
        use windows_sys::Win32::Storage::Vhd::{
            OPEN_VIRTUAL_DISK_FLAG_NONE, OPEN_VIRTUAL_DISK_PARAMETERS,
            OPEN_VIRTUAL_DISK_PARAMETERS_0, OPEN_VIRTUAL_DISK_PARAMETERS_0_0,
            OPEN_VIRTUAL_DISK_VERSION_1, OpenVirtualDisk,
        };

        let wide_path = wide_null(path.as_os_str())?;
        let storage_type = VIRTUAL_STORAGE_TYPE {
            DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
            VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
        };
        let parameters = OPEN_VIRTUAL_DISK_PARAMETERS {
            Version: OPEN_VIRTUAL_DISK_VERSION_1,
            Anonymous: OPEN_VIRTUAL_DISK_PARAMETERS_0 {
                Version1: OPEN_VIRTUAL_DISK_PARAMETERS_0_0 { RWDepth: 1 },
            },
        };
        let mut raw_handle: HANDLE = std::ptr::null_mut();
        let access = VIRTUAL_DISK_ACCESS_ATTACH_RW
            | VIRTUAL_DISK_ACCESS_DETACH
            | VIRTUAL_DISK_ACCESS_GET_INFO;
        // SAFETY: all input structures and the terminated path remain valid
        // for the synchronous open call, and the output handle is initialized.
        let result = unsafe {
            OpenVirtualDisk(
                &storage_type,
                wide_path.as_ptr(),
                access,
                OPEN_VIRTUAL_DISK_FLAG_NONE,
                &parameters,
                &mut raw_handle,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let disk = OwnedVhdHandle::new(raw_handle)?;
        let attach_parameters = ATTACH_VIRTUAL_DISK_PARAMETERS {
            Version: ATTACH_VIRTUAL_DISK_VERSION_1,
            Anonymous: ATTACH_VIRTUAL_DISK_PARAMETERS_0 {
                Version1: ATTACH_VIRTUAL_DISK_PARAMETERS_0_0 { Reserved: 0 },
            },
        };
        let attach_flags =
            ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER | ATTACH_VIRTUAL_DISK_FLAG_PERMANENT_LIFETIME;
        // SAFETY: `disk` is an owned OpenVirtualDisk handle opened for RW
        // attachment; the versioned parameters live through this call.
        let result = unsafe {
            AttachVirtualDisk(
                disk.raw(),
                std::ptr::null_mut(),
                attach_flags,
                0,
                &attach_parameters,
                std::ptr::null(),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result as i32));
        }

        Ok(Self {
            disk,
            attached: true,
            detach_on_drop: true,
        })
    }

    fn raw(&self) -> HANDLE {
        self.disk.raw()
    }

    fn detach(&mut self) -> io::Result<()> {
        if !self.attached {
            return Ok(());
        }
        // SAFETY: `disk` is the still-open handle used for the successful
        // attach, and the API takes no borrowed data beyond this call.
        let result =
            unsafe { DetachVirtualDisk(self.disk.raw(), DETACH_VIRTUAL_DISK_FLAG_NONE, 0) };
        if result != ERROR_SUCCESS {
            self.detach_on_drop = false;
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        self.attached = false;
        Ok(())
    }

    fn preserve_for_recovery(&mut self) {
        self.detach_on_drop = false;
    }
}

impl Drop for AttachedVhd {
    fn drop(&mut self) {
        if self.attached && self.detach_on_drop {
            let _ = self.detach();
        }
    }
}

#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
fn get_layer_vhd_mount_path(handle: HANDLE) -> io::Result<PathBuf> {
    let library_name = wide_null(OsStr::new("computestorage.dll"))?;
    // SAFETY: the System32-only load path is a terminated constant and flags
    // prevent DLL search outside the trusted system directory.
    let module = unsafe {
        LoadLibraryExW(
            library_name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(io::Error::last_os_error());
    }
    let module = OwnedModule(module);
    // SAFETY: `module` remains loaded for the lifetime of the resolved call;
    // the name is a static NUL-terminated C string.
    let function = unsafe { GetProcAddress(module.0, c"HcsGetLayerVhdMountPath".as_ptr().cast()) }
        .ok_or_else(io::Error::last_os_error)?;
    // SAFETY: this export's documented signature is HRESULT(HANDLE, PWSTR*),
    // and Rust's system ABI function pointer representation matches FARPROC.
    let get_path: unsafe extern "system" fn(HANDLE, *mut *mut u16) -> i32 =
        unsafe { std::mem::transmute(function) };
    let mut raw_path = std::ptr::null_mut();
    // SAFETY: `handle` is the successfully attached VHD handle and `raw_path`
    // is a valid out pointer for the documented HCS function signature.
    let result = unsafe { get_path(handle, &mut raw_path) };
    if raw_path.is_null() {
        return Err(io::Error::other(format!(
            "HcsGetLayerVhdMountPath returned no path (HRESULT 0x{:08X})",
            result as u32
        )));
    }
    let path = OwnedLocalWideString(raw_path);
    if result < 0 {
        return Err(io::Error::other(format!(
            "HcsGetLayerVhdMountPath failed (HRESULT 0x{:08X})",
            result as u32
        )));
    }
    let mut length = 0;
    // SAFETY: HcsGetLayerVhdMountPath returns a LocalAlloc-owned,
    // NUL-terminated UTF-16 string; the scan is bounded to the API path limit.
    while length <= MAX_HCS_VOLUME_PATH_UNITS && unsafe { *path.0.add(length) } != 0 {
        length += 1;
    }
    if length == 0 || length > MAX_HCS_VOLUME_PATH_UNITS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HCS transport volume path is empty or exceeds its bound",
        ));
    }
    // SAFETY: the preceding bounded scan found the terminator, so `length`
    // UTF-16 units are initialized in the HCS-owned allocation.
    let value = String::from_utf16(unsafe { std::slice::from_raw_parts(path.0, length) })
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "HCS volume path is not UTF-16"))?;
    let path = PathBuf::from(value);
    if !crate::filesystem::is_canonical_hcs_volume_root(&path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HCS returned a non-canonical volume GUID path",
        ));
    }
    Ok(path)
}

#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
fn secure_export_directory(path: &Path) -> io::Result<()> {
    let service_sid = crate::windows_container::execution::named_pipe::broker_service_sid_string()?;
    let sddl = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{service_sid})");
    let wide_sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: the SDDL input is NUL-terminated and the descriptor out pointer
    // remains valid for the duration of the conversion.
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
    let descriptor = OwnedSecurityDescriptor(descriptor);
    let mut dacl_present = 0;
    let mut dacl = std::ptr::null_mut();
    let mut dacl_defaulted = 0;
    // SAFETY: `descriptor` is the valid descriptor returned above and all
    // output pointers refer to initialized local storage.
    if unsafe {
        GetSecurityDescriptorDacl(
            descriptor.0,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        )
    } == 0
        || dacl_present == 0
        || dacl.is_null()
        || dacl_defaulted != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HCS export transport DACL is malformed",
        ));
    }
    let wide_path = wide_null(path.as_os_str())?;
    // SAFETY: the path is NUL-terminated, `dacl` belongs to `descriptor`, and
    // both remain alive for this synchronous security update.
    let result = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr().cast_mut(),
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

#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
struct OwnedLocalWideString(*mut u16);

impl Drop for OwnedLocalWideString {
    fn drop(&mut self) {
        // SAFETY: HcsGetLayerVhdMountPath documents LocalFree as the matching
        // release operation for its returned string.
        unsafe { LocalFree(self.0.cast()) };
    }
}

#[expect(
    dead_code,
    reason = "the bounded transport is connected after HCS runtime acceptance"
)]
struct OwnedSecurityDescriptor(*mut core::ffi::c_void);

impl Drop for OwnedSecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: the conversion API allocated this descriptor with LocalAlloc.
        unsafe { LocalFree(self.0) };
    }
}

struct OwnedModule(HMODULE);

impl Drop for OwnedModule {
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.0) };
    }
}

fn wide_null(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = value.encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HCS transport path contains a null character",
        ));
    }
    wide.push(0);
    Ok(wide)
}
