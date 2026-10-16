//! Per-attempt virtual disk with an explicit maximum virtual capacity.
//!
//! This primitive proves and retains the VHDX maximum size only. It does not
//! prove that HCS mounts the disk as the guest's writable layer or enforces a
//! guest write limit; that still requires the HCS runtime probe before any
//! production execution is connected.

use crate::filesystem::DirectoryCapability;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, FreeLibrary, HANDLE, HMODULE};
use windows_sys::Win32::Storage::Vhd::{
    CREATE_VIRTUAL_DISK_FLAG_NONE, CREATE_VIRTUAL_DISK_PARAMETERS,
    CREATE_VIRTUAL_DISK_PARAMETERS_0, CREATE_VIRTUAL_DISK_PARAMETERS_0_1,
    CREATE_VIRTUAL_DISK_VERSION_2, CreateVirtualDisk, GET_VIRTUAL_DISK_INFO,
    GET_VIRTUAL_DISK_INFO_0, GET_VIRTUAL_DISK_INFO_0_0, GET_VIRTUAL_DISK_INFO_SIZE,
    GetVirtualDiskInformation, VIRTUAL_DISK_ACCESS_NONE, VIRTUAL_STORAGE_TYPE,
    VIRTUAL_STORAGE_TYPE_DEVICE_VHDX, VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};

const MINIMUM_VHDX_CAPACITY_BYTES: u64 = 3 * 1024 * 1024;
const MAXIMUM_SCRATCH_CAPACITY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const VHDX_SECTOR_SIZE_BYTES: u32 = 512;
const VHDX_BLOCK_SIZE_BYTES: u32 = 2 * 1024 * 1024;
const WRITABLE_VHDX_NAME: &str = "sandbox.vhdx";

/// Owns an unmounted, dynamically allocated VHDX whose virtual size is the
/// selected per-attempt storage ceiling. The owner must outlive the HCS
/// compute system that consumes the path.
pub(crate) struct BoundedScratchVhd {
    parent_root: DirectoryCapability,
    layer_name: String,
    layer_path: PathBuf,
    maximum_size_bytes: u64,
    formatted: bool,
    cleaned: bool,
}

impl BoundedScratchVhd {
    /// Create a private writable-layer directory and unformatted VHDX whose
    /// maximum virtual size matches the attempt quota.
    pub(crate) fn create(root: DirectoryCapability, maximum_size_bytes: u64) -> io::Result<Self> {
        validate_capacity(maximum_size_bytes)?;

        let layer_name = haven_common::types::new_id("file");
        root.create_dir(Path::new(&layer_name))?;
        let layer_root = match DirectoryCapability::open(&root.root().join(&layer_name)) {
            Ok(layer_root) => layer_root,
            Err(error) => {
                let _ = root.remove_dir_all(Path::new(&layer_name));
                return Err(error);
            }
        };
        let layer_path = layer_root.root().to_path_buf();
        let path = layer_path.join(WRITABLE_VHDX_NAME);
        let wide_path = wide_null(path.as_os_str())?;
        let parameters = create_parameters(maximum_size_bytes);
        let storage_type = VIRTUAL_STORAGE_TYPE {
            DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
            VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
        };
        let mut raw_handle: HANDLE = std::ptr::null_mut();
        let result = unsafe {
            CreateVirtualDisk(
                &storage_type,
                wide_path.as_ptr(),
                VIRTUAL_DISK_ACCESS_NONE,
                std::ptr::null_mut(),
                CREATE_VIRTUAL_DISK_FLAG_NONE,
                0,
                &parameters,
                std::ptr::null(),
                &mut raw_handle,
            )
        };
        if result != ERROR_SUCCESS {
            let _ = root.remove_dir_all(Path::new(&layer_name));
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let handle = match OwnedVhdHandle::new(raw_handle) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = root.remove_dir_all(Path::new(&layer_name));
                return Err(error);
            }
        };

        let actual_capacity = match query_virtual_size(handle.raw()) {
            Ok(capacity) => capacity,
            Err(error) => {
                drop(handle);
                let _ = root.remove_dir_all(Path::new(&layer_name));
                return Err(error);
            }
        };
        if actual_capacity != maximum_size_bytes {
            drop(handle);
            let _ = root.remove_dir_all(Path::new(&layer_name));
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "formatted scratch VHDX maximum size does not match its requested quota",
            ));
        }
        drop(handle);

        Ok(Self {
            parent_root: root,
            layer_name,
            layer_path,
            maximum_size_bytes,
            formatted: false,
            cleaned: false,
        })
    }

    /// Format the VHDX for use as a Windows container writable layer.
    ///
    /// HCS storage formatting requires privileges unavailable to Haven's
    /// ordinary desktop process. Call this only from the broker that owns the
    /// required OS authority. A failed call leaves the VHDX owned so it can be
    /// explicitly cleaned up or retried after privilege correction.
    pub(crate) fn format_for_container(&mut self) -> io::Result<()> {
        if self.formatted {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scratch VHDX has already been formatted",
            ));
        }
        let vhd = OpenedVhd::open(&self.path())?;
        format_writable_layer_vhd(vhd.raw())?;
        let actual_capacity = query_virtual_size(vhd.raw())?;
        if actual_capacity != self.maximum_size_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "formatted scratch VHDX maximum size no longer matches its quota",
            ));
        }
        self.formatted = true;
        Ok(())
    }

    /// Remove the layer through its pinned parent after its consumer has stopped.
    /// Keep the owner on error so the caller can retry before releasing any
    /// host storage reservation.
    pub(crate) fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        self.parent_root
            .remove_dir_all(Path::new(&self.layer_name))?;
        self.cleaned = true;
        Ok(())
    }

    /// Return the host path passed to Windows when creating the VHDX.
    pub(crate) fn path(&self) -> PathBuf {
        self.layer_path.join(WRITABLE_VHDX_NAME)
    }

    /// Return the writable-layer directory after HCS formatting succeeds.
    pub(crate) fn layer_path(&self) -> io::Result<&Path> {
        if !self.formatted {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unformatted scratch VHDX cannot be used as an HCS writable layer",
            ));
        }
        Ok(&self.layer_path)
    }

    /// Return the verified maximum virtual capacity of the VHDX.
    pub(crate) fn maximum_size_bytes(&self) -> u64 {
        self.maximum_size_bytes
    }
}

impl Drop for BoundedScratchVhd {
    fn drop(&mut self) {
        // During normal execution callers drop this only after HCS stopped.
        // If Windows still has the VHD open, retain the layer. A provider that
        // needs crash recovery must wrap this primitive in a durable lease.
        if !self.cleaned {
            let _ = self.parent_root.remove_dir_all(Path::new(&self.layer_name));
        }
    }
}

fn validate_capacity(maximum_size_bytes: u64) -> io::Result<()> {
    if !(MINIMUM_VHDX_CAPACITY_BYTES..=MAXIMUM_SCRATCH_CAPACITY_BYTES).contains(&maximum_size_bytes)
        || !maximum_size_bytes.is_multiple_of(u64::from(VHDX_SECTOR_SIZE_BYTES))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scratch VHDX capacity must be sector-aligned and between 3 MiB and 4 GiB",
        ));
    }
    Ok(())
}

fn create_parameters(maximum_size_bytes: u64) -> CREATE_VIRTUAL_DISK_PARAMETERS {
    CREATE_VIRTUAL_DISK_PARAMETERS {
        Version: CREATE_VIRTUAL_DISK_VERSION_2,
        Anonymous: CREATE_VIRTUAL_DISK_PARAMETERS_0 {
            Version2: CREATE_VIRTUAL_DISK_PARAMETERS_0_1 {
                UniqueId: Default::default(),
                MaximumSize: maximum_size_bytes,
                BlockSizeInBytes: VHDX_BLOCK_SIZE_BYTES,
                SectorSizeInBytes: VHDX_SECTOR_SIZE_BYTES,
                PhysicalSectorSizeInBytes: 4096,
                ParentPath: std::ptr::null(),
                SourcePath: std::ptr::null(),
                OpenFlags: 0,
                ParentVirtualStorageType: VIRTUAL_STORAGE_TYPE::default(),
                SourceVirtualStorageType: VIRTUAL_STORAGE_TYPE::default(),
                ResiliencyGuid: Default::default(),
            },
        },
    }
}

fn query_virtual_size(handle: HANDLE) -> io::Result<u64> {
    let mut information = GET_VIRTUAL_DISK_INFO {
        Version: GET_VIRTUAL_DISK_INFO_SIZE,
        Anonymous: GET_VIRTUAL_DISK_INFO_0 {
            Size: GET_VIRTUAL_DISK_INFO_0_0::default(),
        },
    };
    let mut information_size = std::mem::size_of::<GET_VIRTUAL_DISK_INFO>() as u32;
    let mut size_used = 0;
    let result = unsafe {
        GetVirtualDiskInformation(
            handle,
            &mut information_size,
            &mut information,
            &mut size_used,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    if size_used < std::mem::size_of::<GET_VIRTUAL_DISK_INFO>() as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned incomplete virtual disk size information",
        ));
    }
    Ok(unsafe { information.Anonymous.Size.VirtualSize })
}

fn format_writable_layer_vhd(handle: HANDLE) -> io::Result<()> {
    let library_name = wide_null(OsStr::new("computestorage.dll"))?;
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
    let function_name = b"HcsFormatWritableLayerVhd\0";
    let function = unsafe { GetProcAddress(module.0, function_name.as_ptr()) };
    let function = function.ok_or_else(io::Error::last_os_error)?;
    let format: unsafe extern "system" fn(HANDLE) -> i32 = unsafe { std::mem::transmute(function) };
    let result = unsafe { format(handle) };
    if result < 0 {
        return Err(io::Error::other(format!(
            "HcsFormatWritableLayerVhd failed with HRESULT 0x{:08X}",
            result as u32
        )));
    }
    Ok(())
}

fn wide_null(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = value.encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scratch VHDX path contains a null character",
        ));
    }
    wide.push(0);
    Ok(wide)
}

struct OwnedVhdHandle(HANDLE);

impl OwnedVhdHandle {
    fn new(handle: HANDLE) -> io::Result<Self> {
        if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows created a VHDX without returning a valid handle",
            ));
        }
        Ok(Self(handle))
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

struct OpenedVhd(OwnedVhdHandle);

impl OpenedVhd {
    fn open(path: &Path) -> io::Result<Self> {
        use windows_sys::Win32::Storage::Vhd::{
            OPEN_VIRTUAL_DISK_FLAG_NONE, OPEN_VIRTUAL_DISK_PARAMETERS,
            OPEN_VIRTUAL_DISK_PARAMETERS_0, OPEN_VIRTUAL_DISK_PARAMETERS_0_0,
            OPEN_VIRTUAL_DISK_VERSION_1, OpenVirtualDisk, VIRTUAL_DISK_ACCESS_NONE,
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
        let mut handle: HANDLE = std::ptr::null_mut();
        let result = unsafe {
            OpenVirtualDisk(
                &storage_type,
                wide_path.as_ptr(),
                VIRTUAL_DISK_ACCESS_NONE,
                OPEN_VIRTUAL_DISK_FLAG_NONE,
                &parameters,
                &mut handle,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        OwnedVhdHandle::new(handle).map(Self)
    }

    fn raw(&self) -> HANDLE {
        self.0.raw()
    }
}

impl Drop for OwnedVhdHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct OwnedModule(HMODULE);

impl Drop for OwnedModule {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_vhd_has_the_requested_virtual_size_and_is_removed_with_its_owner() {
        let directory = tempfile::tempdir().expect("temporary scratch root can be created");
        let root = DirectoryCapability::open(directory.path())
            .expect("temporary scratch root can be pinned");
        let quota = 16 * 1024 * 1024;
        let mut scratch = BoundedScratchVhd::create(root, quota)
            .expect("Windows can create a VHDX with an exact maximum size");
        let path = scratch.path();

        assert_eq!(scratch.maximum_size_bytes(), quota);
        assert!(path.is_file());
        assert_eq!(path.file_name(), Some(OsStr::new("sandbox.vhdx")));
        assert!(scratch.layer_path().is_err());
        scratch
            .cleanup()
            .expect("owner removes the unformatted writable layer through its pinned parent");
        assert!(!path.exists());
        drop(scratch);
        assert!(!path.exists());
    }

    #[test]
    #[ignore = "requires an elevated process with HCS storage formatting privilege"]
    fn hcs_formats_the_bounded_vhd_as_a_writable_layer() {
        let directory = tempfile::tempdir().expect("temporary scratch root can be created");
        let root = DirectoryCapability::open(directory.path())
            .expect("temporary scratch root can be pinned");
        let quota = 16 * 1024 * 1024;
        let mut scratch = BoundedScratchVhd::create(root, quota)
            .expect("Windows can create a VHDX with an exact maximum size");

        scratch
            .format_for_container()
            .expect("HCS formats the unmounted VHDX for container writable storage");
        assert!(scratch.layer_path().expect("format succeeded").is_dir());
        assert_eq!(scratch.maximum_size_bytes(), quota);
        scratch
            .cleanup()
            .expect("formatted layer is removed after use");
    }

    #[test]
    fn scratch_vhd_capacity_rejects_out_of_range_or_unaligned_sizes() {
        for quota in [0, 1, 3 * 1024 * 1024 - 512, 4 * 1024 * 1024 * 1024 + 512] {
            assert!(validate_capacity(quota).is_err(), "quota {quota} must fail");
        }
        assert!(validate_capacity(3 * 1024 * 1024).is_ok());
        assert!(validate_capacity(4 * 1024 * 1024 * 1024).is_ok());
        assert!(validate_capacity(3 * 1024 * 1024 + 1).is_err());
    }
}
