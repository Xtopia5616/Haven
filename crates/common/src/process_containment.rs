//! Best-effort OS process containment shared by every child-process adapter.
//!
//! On Windows this uses a Job Object with kill-on-close. It contains the
//! process tree and prevents orphaned descendants after cancellation or app
//! exit. Filesystem and network authorization still belongs to the operation
//! gateway; a Job Object is deliberately not presented as an AppContainer.

use std::io;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

/// Owns the OS handle that contains one spawned child and its descendants.
pub struct ProcessContainment {
    #[cfg(windows)]
    job: HANDLE,
}

// Windows kernel handles are process-wide capabilities and can be closed from
// any thread. The guard never exposes the raw handle or mutates the job after
// construction, so moving or sharing the owner across async task boundaries
// is safe; Drop remains the single close point.
unsafe impl Send for ProcessContainment {}
unsafe impl Sync for ProcessContainment {}

impl ProcessContainment {
    /// Create a containment boundary before spawning the child.
    pub fn new() -> io::Result<Self> {
        #[cfg(windows)]
        {
            use std::ptr::null;
            use windows_sys::Win32::System::JobObjects::{
                CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            };

            // SAFETY: null attributes/name request an unnamed private Job
            // Object owned by this process.
            let job = unsafe { CreateJobObjectW(null(), std::ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is the documented structure for the selected
            // information class and remains alive for the call.
            let ok = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                // SAFETY: `job` was returned by CreateJobObjectW and is owned
                // by this object after the failure path.
                unsafe { CloseHandle(job) };
                return Err(error);
            }
            Ok(Self { job })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    /// Attach a just-spawned process. Callers must kill the child when this
    /// returns an error; failing open would make the boundary cosmetic.
    pub fn attach(&self, pid: u32) -> io::Result<()> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
            use windows_sys::Win32::System::Threading::{
                OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
            };
            // SAFETY: pid comes from the Child handle returned by std/tokio;
            // the returned process handle is closed on every path.
            let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
            if process.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: both handles are valid for the duration of this call.
            let ok = unsafe { AssignProcessToJobObject(self.job, process) };
            let error = if ok == 0 {
                Some(io::Error::last_os_error())
            } else {
                None
            };
            // SAFETY: process is the handle returned by OpenProcess.
            unsafe { CloseHandle(process) };
            if let Some(error) = error {
                return Err(error);
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = pid;
            Ok(())
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessContainment {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this guard and is closed exactly once.
        unsafe { CloseHandle(self.job) };
    }
}
