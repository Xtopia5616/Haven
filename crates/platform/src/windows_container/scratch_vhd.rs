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
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::Storage::Vhd::{
    CREATE_VIRTUAL_DISK_FLAG_NONE, CREATE_VIRTUAL_DISK_PARAMETERS,
    CREATE_VIRTUAL_DISK_PARAMETERS_0, CREATE_VIRTUAL_DISK_PARAMETERS_0_1,
    CREATE_VIRTUAL_DISK_VERSION_2, CreateVirtualDisk, GET_VIRTUAL_DISK_INFO,
    GET_VIRTUAL_DISK_INFO_0, GET_VIRTUAL_DISK_INFO_0_0, GET_VIRTUAL_DISK_INFO_SIZE,
    GetVirtualDiskInformation, VIRTUAL_DISK_ACCESS_NONE, VIRTUAL_STORAGE_TYPE,
    VIRTUAL_STORAGE_TYPE_DEVICE_VHDX, VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
};

const MINIMUM_VHDX_CAPACITY_BYTES: u64 = 3 * 1024 * 1024;
const MAXIMUM_SCRATCH_CAPACITY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const VHDX_SECTOR_SIZE_BYTES: u32 = 512;
const VHDX_BLOCK_SIZE_BYTES: u32 = 2 * 1024 * 1024;

/// Owns an unmounted, dynamically allocated VHDX whose virtual size is the
/// selected per-attempt storage ceiling. The owner must outlive the HCS
/// compute system that consumes the path.
pub struct BoundedScratchVhd {
    root: DirectoryCapability,
    name: String,
    maximum_size_bytes: u64,
    cleaned: bool,
}

impl BoundedScratchVhd {
    /// Create a dynamic VHDX beneath a capability-pinned root and verify its
    /// maximum virtual size before returning ownership.
    pub fn create(root: DirectoryCapability, maximum_size_bytes: u64) -> io::Result<Self> {
        validate_capacity(maximum_size_bytes)?;

        let name = format!("{}.vhdx", haven_common::types::new_id("file"));
        let path = root.root().join(&name);
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
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let handle = match OwnedVhdHandle::new(raw_handle) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = root.remove_file(Path::new(&name));
                return Err(error);
            }
        };

        let actual_capacity = match query_virtual_size(handle.raw()) {
            Ok(capacity) => capacity,
            Err(error) => {
                drop(handle);
                let _ = root.remove_file(Path::new(&name));
                return Err(error);
            }
        };
        if actual_capacity != maximum_size_bytes {
            drop(handle);
            let _ = root.remove_file(Path::new(&name));
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "created scratch VHDX maximum size does not match its requested quota",
            ));
        }
        drop(handle);

        Ok(Self {
            root,
            name,
            maximum_size_bytes,
            cleaned: false,
        })
    }

    /// Remove the VHDX through its pinned root after its consumer has stopped.
    /// Keep the owner on error so the caller can retry before releasing any
    /// host storage reservation.
    pub fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        self.root.remove_file(Path::new(&self.name))?;
        self.cleaned = true;
        Ok(())
    }

    /// Return the host path passed to Windows when creating this VHDX.
    pub fn path(&self) -> PathBuf {
        self.root.root().join(&self.name)
    }

    /// Return the verified maximum virtual capacity of the VHDX.
    pub fn maximum_size_bytes(&self) -> u64 {
        self.maximum_size_bytes
    }
}

impl Drop for BoundedScratchVhd {
    fn drop(&mut self) {
        // During normal execution callers drop this only after HCS stopped.
        // If Windows still has the VHD open, retain the file. A provider that
        // needs crash recovery must wrap this primitive in a durable lease.
        if !self.cleaned {
            let _ = self.root.remove_file(Path::new(&self.name));
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

impl Drop for OwnedVhdHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
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
        scratch
            .cleanup()
            .expect("owner removes the VHDX through its pinned root");
        assert!(!path.exists());
        drop(scratch);
        assert!(!path.exists());
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
