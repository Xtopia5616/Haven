//! OS process containment shared by every child-process adapter.
//!
//! On Windows this uses a Job Object with kill-on-close. Children are created
//! suspended, assigned to the Job, and only then resumed so their descendants
//! inherit containment. Filesystem and network authorization still belongs to
//! the operation gateway; a Job Object is deliberately not presented as an
//! AppContainer.

use std::io;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

/// Raw process handle accepted by [`ProcessContainment::attach_and_resume`].
#[cfg(windows)]
pub type ChildProcessHandle = std::os::windows::io::RawHandle;
/// Placeholder handle type on platforms where Job Objects are not used.
#[cfg(not(windows))]
pub type ChildProcessHandle = ();

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

    /// Configure a command for the contained-spawn sequence.
    ///
    /// `windows_creation_flags` must contain every other Windows creation
    /// flag the caller needs. `CommandExt::creation_flags` replaces the full
    /// flag set, so callers must not set flags separately and then expect this
    /// method to preserve them.
    pub fn prepare_command(
        &self,
        command: &mut std::process::Command,
        windows_creation_flags: u32,
    ) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

            command.creation_flags(windows_creation_flags | CREATE_SUSPENDED);
        }
        #[cfg(not(windows))]
        {
            let _ = (command, windows_creation_flags);
        }
    }

    /// Assign a suspended child to this Job Object and resume its one initial
    /// thread. On any failure, terminate the child while it is still held by
    /// its creation-time suspension (or immediately if an unexpected thread
    /// state is observed).
    pub fn attach_and_resume(
        &self,
        pid: u32,
        process_handle: ChildProcessHandle,
    ) -> io::Result<()> {
        #[cfg(windows)]
        {
            let process_handle = process_handle as HANDLE;
            self.attach_and_resume_with_thread_lookup(
                process_handle,
                pid,
                suspended_initial_thread_id,
            )
        }
        #[cfg(not(windows))]
        {
            let _ = (pid, process_handle);
            Ok(())
        }
    }

    #[cfg(windows)]
    fn attach_and_resume_with_thread_lookup(
        &self,
        process: HANDLE,
        pid: u32,
        find_thread: impl FnOnce(u32) -> io::Result<u32>,
    ) -> io::Result<()> {
        match self.assign_and_resume(process, pid, find_thread) {
            Ok(()) => Ok(()),
            Err(error) => {
                // The process handle belongs to the caller and stays valid
                // for the duration of this call. Terminating here makes every
                // failure path fail closed; caller-side kill_on_drop remains
                // a second cleanup path.
                let terminated =
                    unsafe { windows_sys::Win32::System::Threading::TerminateProcess(process, 1) };
                if terminated == 0 {
                    let terminate_error = io::Error::last_os_error();
                    return Err(io::Error::new(
                        error.kind(),
                        format!("{error}; failed to terminate suspended child: {terminate_error}"),
                    ));
                }
                Err(error)
            }
        }
    }

    #[cfg(windows)]
    fn assign_and_resume(
        &self,
        process: HANDLE,
        pid: u32,
        find_thread: impl FnOnce(u32) -> io::Result<u32>,
    ) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        use windows_sys::Win32::System::Threading::{
            GetProcessId, GetProcessIdOfThread, OpenThread, ResumeThread,
            THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
        };

        // Keep the Job assignment and thread lookup tied to the same process
        // handle. A mismatched PID could otherwise assign one child but resume
        // a different, uncontained process.
        let actual_pid = unsafe { GetProcessId(process) };
        if actual_pid == 0 {
            return Err(io::Error::last_os_error());
        }
        if actual_pid != pid {
            return Err(io::Error::other(format!(
                "child handle belongs to process {actual_pid}, not requested process {pid}"
            )));
        }

        if unsafe { AssignProcessToJobObject(self.job, process) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let thread_id = find_thread(pid)?;
        // SAFETY: `thread_id` was found in a Toolhelp snapshot and its owning
        // process was matched to the suspended child. The handle is closed
        // after checking ownership and resuming it.
        let thread = unsafe {
            OpenThread(
                THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                0,
                thread_id,
            )
        };
        if thread.is_null() {
            return Err(io::Error::last_os_error());
        }
        let owner_pid = unsafe { GetProcessIdOfThread(thread) };
        if owner_pid != pid {
            unsafe { CloseHandle(thread) };
            return Err(io::Error::other(format!(
                "initial thread owner changed (expected process {pid}, got {owner_pid})"
            )));
        }

        let previous_suspend_count = unsafe { ResumeThread(thread) };
        let resume_error = if previous_suspend_count == u32::MAX {
            Some(io::Error::last_os_error())
        } else if previous_suspend_count != 1 {
            Some(io::Error::other(format!(
                "initial thread had unexpected suspend count {previous_suspend_count}"
            )))
        } else {
            None
        };
        unsafe { CloseHandle(thread) };
        resume_error.map_or(Ok(()), Err)
    }
}

#[cfg(windows)]
fn suspended_initial_thread_id(pid: u32) -> io::Result<u32> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };

    // SAFETY: this requests a system thread snapshot; the returned handle is
    // closed below regardless of enumeration outcome.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot.is_null() || snapshot as isize == -1 {
        return Err(io::Error::last_os_error());
    }

    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut matching = Vec::new();
    // SAFETY: `entry` is writable and `dwSize` is initialized as required by
    // the Toolhelp API. `snapshot` remains valid until enumeration completes.
    let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    let mut enumeration_error = None;
    while has_entry {
        if entry.th32OwnerProcessID == pid {
            matching.push(entry.th32ThreadID);
        }
        // SAFETY: same valid snapshot and initialized output structure.
        has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        if !has_entry {
            let error = io::Error::last_os_error();
            if error.raw_os_error()
                != Some(windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES as i32)
            {
                enumeration_error = Some(error);
            }
        }
    }
    // SAFETY: snapshot is the handle returned by CreateToolhelp32Snapshot.
    unsafe { CloseHandle(snapshot) };

    if let Some(error) = enumeration_error {
        return Err(error);
    }
    match matching.as_slice() {
        [thread_id] => Ok(*thread_id),
        [] => Err(io::Error::other(format!(
            "suspended process {pid} has no discoverable initial thread"
        ))),
        _ => Err(io::Error::other(format!(
            "suspended process {pid} has {} threads; expected one initial thread",
            matching.len()
        ))),
    }
}

#[cfg(windows)]
impl Drop for ProcessContainment {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this guard and is closed exactly once.
        unsafe { CloseHandle(self.job) };
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::windows::io::AsRawHandle,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, WAIT_OBJECT_0},
        System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
            WaitForSingleObject,
        },
    };

    const DESCENDANT_PID_FILE: &str = "HAVEN_CONTAINMENT_DESCENDANT_PID_FILE";

    #[test]
    fn job_contains_child_and_descendant_from_first_instruction() {
        let pid_file = std::env::temp_dir().join(format!(
            "haven-containment-{}-{}.pid",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos()
        ));
        let mut descendant_cleanup = DescendantCleanup::new(pid_file.clone());
        let test_exe = std::env::current_exe().expect("test executable path");
        let mut command = Command::new(test_exe);
        command
            .args([
                "--exact",
                "process_containment::tests::spawn_descendant_helper",
                "--nocapture",
            ])
            .env(DESCENDANT_PID_FILE, &pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let containment = ProcessContainment::new().expect("create Job Object");
        containment.prepare_command(&mut command, 0);
        let mut child = command.spawn().expect("spawn suspended test child");
        let child_pid = child.id();

        // Give the test harness time to enter the helper if the creation-time
        // suspension were missing. It must not create a descendant yet.
        let pre_attach_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_file.exists() && Instant::now() < pre_attach_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let ran_before_attach = pid_file.exists();
        let attach = containment.attach_and_resume(child_pid, child.as_raw_handle());
        if let Err(error) = attach {
            terminate_descendant_from_pid_file(&pid_file);
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&pid_file);
            panic!("attach and resume suspended child: {error}");
        }
        if ran_before_attach {
            drop(containment);
            terminate_descendant_from_pid_file(&pid_file);
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&pid_file);
            panic!("child executed before it was assigned to the Job Object");
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_file.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let descendant_pid = fs::read_to_string(&pid_file)
            .expect("resumed child launched its descendant")
            .trim()
            .parse::<u32>()
            .expect("descendant pid is numeric");

        drop(containment);
        let child_exit = process_exits(child_pid);
        let descendant_exit = process_exits(descendant_pid);
        let child_status = child.wait();
        if child_exit.is_ok() && descendant_exit.is_ok() {
            descendant_cleanup.disarm();
        }
        assert!(
            child_exit.is_ok(),
            "child exit check failed: {child_exit:?}"
        );
        assert!(
            descendant_exit.is_ok(),
            "descendant exit check failed: {descendant_exit:?}"
        );
        assert!(
            child_status.is_ok(),
            "wait for child failed: {child_status:?}"
        );
    }

    #[test]
    fn process_handle_pid_mismatch_terminates_the_suspended_child() {
        let pid_file = std::env::temp_dir().join(format!(
            "haven-containment-failure-{}-{}.pid",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos()
        ));
        let _descendant_cleanup = DescendantCleanup::new(pid_file.clone());
        let test_exe = std::env::current_exe().expect("test executable path");
        let mut command = Command::new(test_exe);
        command
            .args([
                "--exact",
                "process_containment::tests::spawn_descendant_helper",
                "--nocapture",
            ])
            .env(DESCENDANT_PID_FILE, &pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let containment = ProcessContainment::new().expect("create Job Object");
        containment.prepare_command(&mut command, 0);
        let mut child = command.spawn().expect("spawn suspended test child");
        let child_pid = child.id();
        let pre_attach_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_file.exists() && Instant::now() < pre_attach_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let result = containment.attach_and_resume(u32::MAX, child.as_raw_handle());
        terminate_descendant_from_pid_file(&pid_file);
        assert!(result.is_err(), "handle/PID mismatch must fail containment");
        let child_exit = process_exits(child_pid);
        assert!(
            child_exit.is_ok(),
            "failed attach must terminate the child: {child_exit:?}"
        );
        assert!(
            !child.wait().expect("wait for terminated child").success(),
            "failed attach must terminate the child"
        );
        assert!(
            !pid_file.exists(),
            "failed thread discovery must not let the child execute"
        );
        drop(containment);
        let _ = fs::remove_file(pid_file);
    }

    #[test]
    fn thread_discovery_failure_terminates_the_suspended_child() {
        let pid_file = std::env::temp_dir().join(format!(
            "haven-containment-thread-failure-{}-{}.pid",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos()
        ));
        let _descendant_cleanup = DescendantCleanup::new(pid_file.clone());
        let test_exe = std::env::current_exe().expect("test executable path");
        let mut command = Command::new(test_exe);
        command
            .args([
                "--exact",
                "process_containment::tests::spawn_descendant_helper",
                "--nocapture",
            ])
            .env(DESCENDANT_PID_FILE, &pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let containment = ProcessContainment::new().expect("create Job Object");
        containment.prepare_command(&mut command, 0);
        let mut child = command.spawn().expect("spawn suspended test child");
        let child_pid = child.id();
        let pre_attach_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_file.exists() && Instant::now() < pre_attach_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let result = containment.attach_and_resume_with_thread_lookup(
            child.as_raw_handle() as HANDLE,
            child_pid,
            |_| Err(std::io::Error::other("injected thread discovery failure")),
        );
        terminate_descendant_from_pid_file(&pid_file);
        assert!(result.is_err(), "thread discovery failure must fail closed");
        let child_exit = process_exits(child_pid);
        assert!(
            child_exit.is_ok(),
            "thread discovery failure must terminate the child: {child_exit:?}"
        );
        assert!(
            !child.wait().expect("wait for terminated child").success(),
            "failed thread discovery must terminate the child"
        );
        assert!(
            !pid_file.exists(),
            "failed thread discovery must not let the child execute"
        );
        drop(containment);
    }

    #[test]
    fn spawn_descendant_helper() {
        let Some(pid_file) = std::env::var_os(DESCENDANT_PID_FILE) else {
            return;
        };
        let exe = std::env::current_exe().expect("test executable path");
        let mut descendant = Command::new(exe)
            .args([
                "--exact",
                "process_containment::tests::long_lived_descendant_helper",
                "--nocapture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn long-lived descendant");
        fs::write(pid_file, descendant.id().to_string()).expect("write descendant pid");
        let _ = descendant.wait();
    }

    #[test]
    fn long_lived_descendant_helper() {
        if std::env::var_os(DESCENDANT_PID_FILE).is_some() {
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
    }

    fn process_exits(pid: u32) -> Result<(), String> {
        // SAFETY: OpenProcess returns an owned synchronization handle that is
        // closed below; the process ID came from a child spawned by this test.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            // The process may have exited between the caller's check and this
            // open. In this isolated same-user test, access denial is not an
            // expected outcome.
            let error = unsafe { GetLastError() };
            return if error == ERROR_INVALID_PARAMETER {
                Ok(())
            } else {
                Err(format!("OpenProcess failed for process {pid}: {error}"))
            };
        }
        // SAFETY: process is a valid handle with PROCESS_SYNCHRONIZE access.
        let wait = unsafe { WaitForSingleObject(process, 5_000) };
        if wait != WAIT_OBJECT_0 {
            let _ = unsafe { TerminateProcess(process, 1) };
            let _ = unsafe { WaitForSingleObject(process, 5_000) };
        }
        unsafe { CloseHandle(process) };
        if wait == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(format!("process {pid} survived Job close"))
        }
    }

    fn terminate_descendant_from_pid_file(pid_file: &Path) {
        let Ok(pid) = fs::read_to_string(pid_file)
            .map(|pid| pid.trim().parse::<u32>())
            .and_then(|pid| pid.map_err(std::io::Error::other))
        else {
            return;
        };
        // SAFETY: this test-created child PID receives a temporary process
        // handle with terminate/synchronize rights, which is closed below.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return;
        }
        let _ = unsafe { TerminateProcess(process, 1) };
        let _ = unsafe { WaitForSingleObject(process, 5_000) };
        unsafe { CloseHandle(process) };
    }

    struct DescendantCleanup {
        pid_file: PathBuf,
        armed: bool,
    }

    impl DescendantCleanup {
        fn new(pid_file: PathBuf) -> Self {
            Self {
                pid_file,
                armed: true,
            }
        }

        fn disarm(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for DescendantCleanup {
        fn drop(&mut self) {
            if self.armed {
                terminate_descendant_from_pid_file(&self.pid_file);
            }
            let _ = fs::remove_file(&self.pid_file);
        }
    }
}
