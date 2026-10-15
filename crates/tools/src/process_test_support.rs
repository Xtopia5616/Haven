//! Windows process helpers for adapter-level containment tests.

use std::{fs, io, path::PathBuf, time::Duration};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, WAIT_OBJECT_0},
    System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    },
};

pub(crate) struct DescendantProcessMarker {
    pid_file: PathBuf,
    armed: bool,
}

impl DescendantProcessMarker {
    pub(crate) fn new(pid_file: PathBuf) -> Self {
        Self {
            pid_file,
            armed: true,
        }
    }

    pub(crate) fn pid(&self) -> io::Result<u32> {
        fs::read_to_string(&self.pid_file)?
            .trim()
            .parse::<u32>()
            .map_err(io::Error::other)
    }

    pub(crate) fn wait_for_exit(&mut self, pid: u32, timeout: Duration) -> Result<(), String> {
        // SAFETY: `pid` is read from a marker written by the test-created
        // descendant. The synchronization handle is closed after the wait.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            let error = unsafe { GetLastError() };
            return if error == ERROR_INVALID_PARAMETER {
                self.armed = false;
                Ok(())
            } else {
                Err(format!("OpenProcess failed for descendant {pid}: {error}"))
            };
        }
        let wait = unsafe {
            WaitForSingleObject(process, timeout.as_millis().min(u32::MAX as u128) as u32)
        };
        unsafe { CloseHandle(process) };
        if wait == WAIT_OBJECT_0 {
            self.armed = false;
            Ok(())
        } else {
            Err(format!(
                "descendant process {pid} survived containment cleanup"
            ))
        }
    }
}

impl Drop for DescendantProcessMarker {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(pid) = self.pid() else {
            return;
        };
        // SAFETY: this test-created descendant PID is opened with the exact
        // rights used below; the owned handle closes after best-effort cleanup.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return;
        }
        unsafe {
            let _ = TerminateProcess(process, 1);
            let _ = WaitForSingleObject(process, 5_000);
            CloseHandle(process);
        }
    }
}
