//! Bounded ownership of one HCS guest process and its standard I/O pipes.
//!
//! The broker guest runner uses this owner for process and handle cleanup.
//! Production requests stay unavailable until writable-layer quota enforcement
//! is attested on a supported host.

use super::hcs_process::HcsGuestProcessParameters;
use super::hcs_runtime::{
    HcsApi, HcsProcessInformation, RawComputeSystem, RawGuestHandle, RawGuestProcess,
    validate_operation_timeout,
};
use serde::Deserialize;
use std::io;
use std::time::Duration;

const MAX_PROCESS_STATUS_BYTES: usize = 64 * 1024;

/// Stable status returned after HCS confirms guest process exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GuestProcessExitStatus {
    pub(super) process_id: u32,
    pub(super) exit_code: u32,
}

/// Owns the HCS process handle and any standard I/O handles not transferred
/// to stream adapters. The borrow keeps the HCS API/module and its parent
/// compute-system owner alive for at least this process owner's lifetime.
pub(super) struct HcsGuestProcessOwner<'a> {
    api: &'a dyn HcsApi,
    process: Option<RawGuestProcess>,
    process_id: u32,
    stdin: Option<OwnedGuestHandle<'a>>,
    stdout: Option<OwnedGuestHandle<'a>>,
    stderr: Option<OwnedGuestHandle<'a>>,
    exit_status: Option<GuestProcessExitStatus>,
    termination_requested: bool,
}

impl<'a> HcsGuestProcessOwner<'a> {
    pub(super) fn create(
        api: &'a dyn HcsApi,
        system: RawComputeSystem,
        parameters: &HcsGuestProcessParameters,
        operation_timeout: Duration,
    ) -> io::Result<Self> {
        validate_operation_timeout(operation_timeout)?;
        let json = parameters.to_json()?;
        let info = api.create_guest_process(system, &json, operation_timeout)?;
        Self::from_process_information(api, info, parameters.creates_stdin_pipe())
    }

    fn from_process_information(
        api: &'a dyn HcsApi,
        info: HcsProcessInformation,
        stdin_expected: bool,
    ) -> io::Result<Self> {
        let stdin_shape_matches = if stdin_expected {
            !info.stdin.is_null()
        } else {
            info.stdin.is_null()
        };
        if info.process.is_null()
            || info.process_id == 0
            || !stdin_shape_matches
            || info.stdout.is_null()
            || info.stderr.is_null()
        {
            close_partial_information(api, info);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HCS returned incomplete guest process handles",
            ));
        }
        Ok(Self {
            api,
            process: Some(info.process),
            process_id: info.process_id,
            stdin: (!info.stdin.is_null()).then(|| OwnedGuestHandle::new(api, info.stdin)),
            stdout: Some(OwnedGuestHandle::new(api, info.stdout)),
            stderr: Some(OwnedGuestHandle::new(api, info.stderr)),
            exit_status: None,
            termination_requested: false,
        })
    }

    #[cfg(test)]
    pub(super) fn process_id(&self) -> u32 {
        self.process_id
    }

    /// Wait for at most the supplied bounded duration. `None` means HCS did
    /// not observe process exit before that deadline.
    pub(super) fn poll_exit(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<GuestProcessExitStatus>> {
        validate_operation_timeout(timeout)?;
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let process = self.process.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCS guest process handle has already been closed",
            )
        })?;
        let Some(document) = self.api.wait_for_guest_process_exit(process, timeout)? else {
            return Ok(None);
        };
        let status = parse_process_status(&document, self.process_id)?;
        if let Some(status) = status {
            self.exit_status = Some(status);
        }
        Ok(status)
    }

    /// Request process termination with the HCS API's fixed bounded wait.
    /// Failure leaves the owner live so Drop can make one best-effort retry.
    pub(super) fn terminate(&mut self) -> io::Result<()> {
        if self.exit_status.is_some() || self.termination_requested {
            return Ok(());
        }
        let Some(process) = self.process else {
            return Ok(());
        };
        self.api.terminate_guest_process(process)?;
        self.termination_requested = true;
        Ok(())
    }

    pub(super) fn take_stdout(&mut self) -> Option<OwnedGuestHandle<'a>> {
        self.stdout.take()
    }

    pub(super) fn take_stderr(&mut self) -> Option<OwnedGuestHandle<'a>> {
        self.stderr.take()
    }

    /// Transfer the optional stdin endpoint to the bounded MCP stream adapter.
    #[allow(dead_code)] // Used when the dedicated broker MCP session is wired.
    pub(super) fn take_stdin(&mut self) -> Option<OwnedGuestHandle<'a>> {
        self.stdin.take()
    }
}

impl Drop for HcsGuestProcessOwner<'_> {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            if self.exit_status.is_none() && !self.termination_requested {
                let _ = self.api.terminate_guest_process(process);
            }
            drop(self.stdin.take());
            drop(self.stdout.take());
            drop(self.stderr.take());
            self.api.close_guest_process(process);
        }
    }
}

/// Owns one HCS-provided pipe handle. A future bounded reader can take this
/// handle without transferring the HCS process owner or a raw HANDLE.
pub(super) struct OwnedGuestHandle<'a> {
    api: &'a dyn HcsApi,
    handle: Option<RawGuestHandle>,
}

impl<'a> OwnedGuestHandle<'a> {
    fn new(api: &'a dyn HcsApi, handle: RawGuestHandle) -> Self {
        Self {
            api,
            handle: Some(handle),
        }
    }

    pub(super) fn raw(&self) -> Option<RawGuestHandle> {
        self.handle
    }

    /// Transfer this Win32 pipe handle into a `File`, which then owns its
    /// `CloseHandle` lifetime independently of the HCS API object.
    #[allow(dead_code)] // Used when the dedicated broker MCP session is wired.
    pub(super) fn into_file(mut self) -> io::Result<std::fs::File> {
        use std::os::windows::io::FromRawHandle;

        let handle = self.handle.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCS guest handle has already been transferred",
            )
        })?;
        // SAFETY: HCS returns a Win32 pipe HANDLE. Ownership is removed from
        // this wrapper immediately before constructing the owning File.
        Ok(unsafe { std::fs::File::from_raw_handle(handle) })
    }
}

impl Drop for OwnedGuestHandle<'_> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.api.close_guest_handle(handle);
        }
    }
}

fn close_partial_information(api: &dyn HcsApi, info: HcsProcessInformation) {
    for handle in [info.stdin, info.stdout, info.stderr] {
        if !handle.is_null() {
            api.close_guest_handle(handle);
        }
    }
    if !info.process.is_null() {
        let _ = api.terminate_guest_process(info.process);
        api.close_guest_process(info.process);
    }
}

#[derive(Deserialize)]
struct ProcessStatusDocument {
    #[serde(rename = "ProcessId")]
    process_id: u32,
    #[serde(rename = "Exited")]
    exited: bool,
    #[serde(rename = "ExitCode")]
    exit_code: Option<u32>,
    #[serde(rename = "LastWaitResult")]
    last_wait_result: i32,
}

fn parse_process_status(
    json: &str,
    expected_process_id: u32,
) -> io::Result<Option<GuestProcessExitStatus>> {
    if json.len() > MAX_PROCESS_STATUS_BYTES {
        return Err(invalid_process_status(
            "HCS guest process status exceeds its byte bound",
        ));
    }
    let document: ProcessStatusDocument = serde_json::from_str(json)
        .map_err(|_| invalid_process_status("HCS guest process status document is malformed"))?;
    if document.process_id != expected_process_id {
        return Err(invalid_process_status(
            "HCS guest process status ID does not match the created process",
        ));
    }
    if !document.exited {
        return Ok(None);
    }
    if document.last_wait_result != 0 {
        return Err(invalid_process_status(
            "HCS could not confirm the guest process exit status",
        ));
    }
    let exit_code = document
        .exit_code
        .ok_or_else(|| invalid_process_status("HCS guest process exit status has no exit code"))?;
    Ok(Some(GuestProcessExitStatus {
        process_id: document.process_id,
        exit_code,
    }))
}

fn invalid_process_status(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::super::hcs_runtime::HcsComputeSystemOwner;
    use super::*;
    use crate::filesystem::{DirectoryTreeCopyLimits, DirectoryTreeDiffLimits};
    use crate::windows_container::execution::{
        WindowsContainerExecutionLimits, WindowsContainerExecutionRequest,
        WindowsContainerOutputSink, WindowsContainerOutputStream, WindowsContainerShell,
    };
    use crate::windows_container::hcs_process_input::HcsGuestProcessInputWriter;
    use crate::windows_container::hcs_process_output::HcsGuestProcessOutputReader;
    use crate::windows_container::{WindowsContainerLayer, WindowsContainerSandboxConfig};
    use std::collections::VecDeque;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::windows::io::FromRawHandle;
    use std::sync::Mutex;

    struct MockHcsApi {
        calls: Mutex<Vec<&'static str>>,
        statuses: Mutex<VecDeque<io::Result<Option<String>>>>,
        fail_first_termination: Mutex<bool>,
        process_output_handles: Mutex<Option<(RawGuestHandle, RawGuestHandle)>>,
        real_output_handles: bool,
        real_guest_handles: bool,
    }

    impl MockHcsApi {
        fn new(statuses: impl IntoIterator<Item = io::Result<Option<String>>>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                statuses: Mutex::new(statuses.into_iter().collect()),
                fail_first_termination: Mutex::new(false),
                process_output_handles: Mutex::new(None),
                real_output_handles: false,
                real_guest_handles: false,
            }
        }

        fn with_output_handles(
            statuses: impl IntoIterator<Item = io::Result<Option<String>>>,
            stdout: RawGuestHandle,
            stderr: RawGuestHandle,
        ) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                statuses: Mutex::new(statuses.into_iter().collect()),
                fail_first_termination: Mutex::new(false),
                process_output_handles: Mutex::new(Some((stdout, stderr))),
                real_output_handles: true,
                real_guest_handles: false,
            }
        }

        fn with_real_guest_handles() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                statuses: Mutex::new(VecDeque::new()),
                fail_first_termination: Mutex::new(false),
                process_output_handles: Mutex::new(None),
                real_output_handles: false,
                real_guest_handles: true,
            }
        }

        fn record(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
        }
    }

    impl HcsApi for MockHcsApi {
        fn create_compute_system(
            &self,
            id: &str,
            configuration: &str,
            _timeout: Duration,
        ) -> io::Result<RawComputeSystem> {
            assert_eq!(id, "HavenSandbox-0123456789abcdef0123456789abcdef");
            assert!(configuration.contains("\"HvPartition\":true"));
            self.record("create_system");
            Ok(1_usize as RawComputeSystem)
        }

        fn start_compute_system(
            &self,
            system: RawComputeSystem,
            _timeout: Duration,
        ) -> io::Result<()> {
            assert_eq!(system, 1_usize as RawComputeSystem);
            self.record("start_system");
            Ok(())
        }

        fn terminate_compute_system(&self, system: RawComputeSystem) -> io::Result<()> {
            assert_eq!(system, 1_usize as RawComputeSystem);
            self.record("terminate_system");
            Ok(())
        }

        fn close_compute_system(&self, system: RawComputeSystem) {
            assert_eq!(system, 1_usize as RawComputeSystem);
            self.record("close_system");
        }

        fn create_guest_process(
            &self,
            system: RawComputeSystem,
            parameters: &str,
            _timeout: Duration,
        ) -> io::Result<HcsProcessInformation> {
            assert_eq!(system, 1_usize as RawComputeSystem);
            assert!(parameters.contains("\"RestrictedToken\":true"));
            self.record("create_process");
            let (stdout, stderr) = self
                .process_output_handles
                .lock()
                .unwrap()
                .take()
                .unwrap_or((3_usize as RawGuestHandle, 4_usize as RawGuestHandle));
            Ok(HcsProcessInformation {
                process: 2_usize as RawGuestProcess,
                process_id: 42,
                stdin: std::ptr::null_mut(),
                stdout,
                stderr,
            })
        }

        fn wait_for_guest_process_exit(
            &self,
            process: RawGuestProcess,
            timeout: Duration,
        ) -> io::Result<Option<String>> {
            assert_eq!(process, 2_usize as RawGuestProcess);
            assert!(!timeout.is_zero());
            self.record("wait_exit");
            self.statuses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(None))
        }

        fn terminate_guest_process(&self, process: RawGuestProcess) -> io::Result<()> {
            assert_eq!(process, 2_usize as RawGuestProcess);
            self.record("terminate_process");
            let mut fail_first = self.fail_first_termination.lock().unwrap();
            if *fail_first {
                *fail_first = false;
                return Err(io::Error::other("mock termination failure"));
            }
            Ok(())
        }

        fn close_guest_process(&self, process: RawGuestProcess) {
            assert_eq!(process, 2_usize as RawGuestProcess);
            self.record("close_process");
        }

        fn close_guest_handle(&self, handle: RawGuestHandle) {
            if self.real_output_handles || self.real_guest_handles {
                assert_ne!(
                    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) },
                    0
                );
                self.record("close_guest_handle");
            } else {
                let name = match handle as usize {
                    3 => "close_stdout",
                    4 => "close_stderr",
                    5 => "close_stdin",
                    _ => panic!("unexpected mock guest handle"),
                };
                self.record(name);
            }
        }
    }

    fn parameters() -> HcsGuestProcessParameters {
        let copy = DirectoryTreeCopyLimits::new(32, 8, 1024 * 1024, 4 * 1024 * 1024)
            .expect("bounded workspace limits");
        let diff = DirectoryTreeDiffLimits::new(copy, 16, 1024 * 1024, 4 * 1024 * 1024)
            .expect("bounded output limits");
        let limits = WindowsContainerExecutionLimits::new(
            16 * 1024 * 1024,
            Duration::from_secs(10),
            1024,
            1024,
            copy,
            diff,
        )
        .expect("bounded execution limits");
        let request = WindowsContainerExecutionRequest::new(
            WindowsContainerShell::Cmd,
            "echo bounded",
            "",
            1,
            1,
            limits,
        )
        .expect("typed shell request");
        HcsGuestProcessParameters::from_request(&request).expect("closed process parameters")
    }

    fn output_request() -> WindowsContainerExecutionRequest {
        let copy = DirectoryTreeCopyLimits::new(32, 8, 1024 * 1024, 4 * 1024 * 1024)
            .expect("bounded workspace limits");
        let diff = DirectoryTreeDiffLimits::new(copy, 16, 1024 * 1024, 4 * 1024 * 1024)
            .expect("bounded output limits");
        let limits = WindowsContainerExecutionLimits::new(
            16 * 1024 * 1024,
            Duration::from_secs(10),
            3,
            2,
            copy,
            diff,
        )
        .expect("bounded execution limits");
        WindowsContainerExecutionRequest::new(
            WindowsContainerShell::Cmd,
            "echo bounded",
            "",
            1,
            1,
            limits,
        )
        .expect("typed shell request")
    }

    fn create_pipe() -> (RawGuestHandle, File) {
        let mut read = std::ptr::null_mut();
        let mut write = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                windows_sys::Win32::System::Pipes::CreatePipe(
                    &mut read,
                    &mut write,
                    std::ptr::null(),
                    0,
                )
            },
            0
        );
        let writer = unsafe { File::from_raw_handle(write) };
        (read, writer)
    }

    fn create_pipe_handles() -> (RawGuestHandle, RawGuestHandle) {
        let mut read = std::ptr::null_mut();
        let mut write = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                windows_sys::Win32::System::Pipes::CreatePipe(
                    &mut read,
                    &mut write,
                    std::ptr::null(),
                    0,
                )
            },
            0
        );
        (read, write)
    }

    fn process_with_stdin<'a>(
        api: &'a MockHcsApi,
        stdin: RawGuestHandle,
        stdout: RawGuestHandle,
        stderr: RawGuestHandle,
    ) -> HcsGuestProcessOwner<'a> {
        HcsGuestProcessOwner::from_process_information(
            api,
            HcsProcessInformation {
                process: 2_usize as RawGuestProcess,
                process_id: 42,
                stdin,
                stdout,
                stderr,
            },
            true,
        )
        .expect("all test guest handles are valid")
    }

    fn owner<'a>(api: &'a MockHcsApi) -> HcsGuestProcessOwner<'a> {
        HcsGuestProcessOwner::create(
            api,
            1_usize as RawComputeSystem,
            &parameters(),
            Duration::from_secs(30),
        )
        .expect("mock process is created")
    }

    fn sandbox_config() -> WindowsContainerSandboxConfig {
        WindowsContainerSandboxConfig::new(
            "HavenSandbox-0123456789abcdef0123456789abcdef",
            1024 * 1024 * 1024,
            vec![
                WindowsContainerLayer::new("trusted-base-layer", r"C:\Haven\images\base")
                    .expect("test layer"),
            ],
        )
        .expect("test sandbox configuration")
        .with_test_writable_layer_folder(std::path::PathBuf::from(
            r"C:\Haven\runtime\attempt\layer",
        ))
        .expect("test writable layer path is valid")
    }

    #[test]
    fn compute_system_must_be_started_before_guest_process_creation() {
        let api = MockHcsApi::new([]);
        let parameters = parameters();
        let mut system =
            HcsComputeSystemOwner::create(&api, &sandbox_config(), Duration::from_secs(30))
                .expect("compute system owner is created");
        assert!(
            system
                .create_guest_process(&parameters, Duration::from_secs(30))
                .is_err()
        );
        system
            .start(Duration::from_secs(30))
            .expect("compute system starts");
        let process = system
            .create_guest_process(&parameters, Duration::from_secs(30))
            .expect("started system can create a typed guest process");
        assert_eq!(process.process_id(), 42);
        drop(process);
        system.terminate().expect("compute system terminates");

        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                "create_system",
                "start_system",
                "create_process",
                "terminate_process",
                "close_stdout",
                "close_stderr",
                "close_process",
                "terminate_system",
                "close_system"
            ]
        );
    }

    #[test]
    fn process_exit_is_parsed_once_and_stdio_handles_are_independently_owned() {
        let api = MockHcsApi::new([
            Ok(None),
            Ok(Some(
                r#"{"ProcessId":42,"Exited":true,"ExitCode":7,"LastWaitResult":0}"#.to_owned(),
            )),
        ]);
        let mut process = owner(&api);
        assert_eq!(process.process_id(), 42);
        assert!(process.poll_exit(Duration::from_secs(1)).unwrap().is_none());
        let status = process
            .poll_exit(Duration::from_secs(1))
            .unwrap()
            .expect("second bounded wait observes exit");
        assert_eq!(status.exit_code, 7);
        assert_eq!(
            process.poll_exit(Duration::from_secs(1)).unwrap(),
            Some(status)
        );

        let stdout = process.take_stdout().expect("stdout handle is transferred");
        drop(process);
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                "create_process",
                "wait_exit",
                "wait_exit",
                "close_stderr",
                "close_process"
            ]
        );
        drop(stdout);
        assert_eq!(api.calls.lock().unwrap().last(), Some(&"close_stdout"));
    }

    #[test]
    fn process_owner_accepts_stdin_only_when_requested_and_closes_it_once() {
        let api = MockHcsApi::new([]);
        let mut process = HcsGuestProcessOwner::from_process_information(
            &api,
            HcsProcessInformation {
                process: 2_usize as RawGuestProcess,
                process_id: 42,
                stdin: 5_usize as RawGuestHandle,
                stdout: 3_usize as RawGuestHandle,
                stderr: 4_usize as RawGuestHandle,
            },
            true,
        )
        .expect("expected stdin handle is owned");
        let stdin = process.take_stdin().expect("stdin is transferred once");
        assert_eq!(stdin.raw(), Some(5_usize as RawGuestHandle));
        assert!(process.take_stdin().is_none());
        drop(process);
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                "terminate_process",
                "close_stdout",
                "close_stderr",
                "close_process"
            ]
        );

        drop(stdin);
        assert_eq!(api.calls.lock().unwrap().last(), Some(&"close_stdin"));
    }

    #[test]
    fn stdin_writer_owns_process_alongside_output_reader_and_drains_to_eof() {
        let (stdin_read, stdin_write) = create_pipe_handles();
        let (stdout_read, stdout_write) = create_pipe_handles();
        let (stderr_read, stderr_write) = create_pipe_handles();
        let stdin_reader = unsafe { File::from_raw_handle(stdin_read) };
        let stdout_writer = unsafe { File::from_raw_handle(stdout_write) };
        let stderr_writer = unsafe { File::from_raw_handle(stderr_write) };
        let api = MockHcsApi::with_real_guest_handles();
        let mut process = process_with_stdin(&api, stdin_write, stdout_read, stderr_read);
        let output_reader =
            super::super::hcs_process_output::HcsGuestProcessOutputReader::from_process(
                &mut process,
            )
            .expect("output pipes can move to a peer adapter before process ownership transfer");
        {
            let mut writer = HcsGuestProcessInputWriter::new(process)
                .expect("HCS stdin handle transfers to its bounded writer");
            assert!(
                writer
                    .poll_process_exit(Duration::from_millis(1))
                    .expect("process status remains available during stdin writes")
                    .is_none()
            );
            writer
                .try_write_chunk(b"first-")
                .expect("small input is queued");
            writer
                .try_write_chunk(b"second")
                .expect("a second small chunk is queued");
            writer.close_input();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match writer.poll() {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(error) => panic!("stdin drain failed: {error}"),
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(
                writer.try_write_chunk(b"late").unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
        }

        let mut received = Vec::new();
        let mut stdin_reader = stdin_reader;
        stdin_reader
            .read_to_end(&mut received)
            .expect("closed HCS stdin pipe reaches EOF");
        assert_eq!(received, b"first-second");
        drop(output_reader);
        drop(stdout_writer);
        drop(stderr_writer);
    }

    #[test]
    fn stdin_writer_applies_backpressure_and_cancels_a_blocked_pipe_write() {
        let (stdin_read, stdin_write) = create_pipe_handles();
        let (stdout_read, stdout_write) = create_pipe_handles();
        let (stderr_read, stderr_write) = create_pipe_handles();
        let _stdin_reader = unsafe { File::from_raw_handle(stdin_read) };
        let stdout_writer = unsafe { File::from_raw_handle(stdout_write) };
        let stderr_writer = unsafe { File::from_raw_handle(stderr_write) };
        let api = MockHcsApi::with_real_guest_handles();
        let process = process_with_stdin(&api, stdin_write, stdout_read, stderr_read);
        let mut writer = HcsGuestProcessInputWriter::new(process)
            .expect("HCS stdin handle transfers to its bounded writer");
        let chunk = vec![b'x'; 16 * 1024];
        assert_eq!(
            writer
                .try_write_chunk(&vec![b'y'; 16 * 1024 + 1])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let mut accepted = 0;
        let mut observed_backpressure = false;
        for _ in 0..8 {
            match writer.try_write_chunk(&chunk) {
                Ok(()) => accepted += 1,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    observed_backpressure = true;
                    break;
                }
                Err(error) => panic!("unexpected HCS stdin write error: {error}"),
            }
        }
        assert!(observed_backpressure);
        assert!(accepted <= 4, "at most 64 KiB can be retained");

        let started = std::time::Instant::now();
        writer
            .abort()
            .expect("cancel stops the blocked synchronous write");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(api.calls.lock().unwrap().contains(&"terminate_process"));
        drop(writer);
        drop(stdout_writer);
        drop(stderr_writer);
    }

    #[test]
    fn process_owner_closes_partial_handles_when_stdin_shape_does_not_match_parameters() {
        for (stdin, stdin_expected) in [
            (std::ptr::null_mut(), true),
            (5_usize as RawGuestHandle, false),
        ] {
            let api = MockHcsApi::new([]);
            let result = HcsGuestProcessOwner::from_process_information(
                &api,
                HcsProcessInformation {
                    process: 2_usize as RawGuestProcess,
                    process_id: 42,
                    stdin,
                    stdout: 3_usize as RawGuestHandle,
                    stderr: 4_usize as RawGuestHandle,
                },
                stdin_expected,
            );
            assert!(result.is_err());
            assert_eq!(
                *api.calls.lock().unwrap(),
                if stdin.is_null() {
                    vec![
                        "close_stdout",
                        "close_stderr",
                        "terminate_process",
                        "close_process",
                    ]
                } else {
                    vec![
                        "close_stdin",
                        "close_stdout",
                        "close_stderr",
                        "terminate_process",
                        "close_process",
                    ]
                }
            );
        }
    }

    #[test]
    fn output_poll_drains_both_streams_and_preserves_sink_caps() {
        let (stdout_handle, mut stdout_writer) = create_pipe();
        let (stderr_handle, mut stderr_writer) = create_pipe();
        let api = MockHcsApi::with_output_handles([], stdout_handle, stderr_handle);
        let mut process = owner(&api);
        let mut reader = HcsGuestProcessOutputReader::from_process(&mut process)
            .expect("both guest output handles transfer together");
        let request = output_request();
        let mut on_output = |_: WindowsContainerOutputStream, _: &[u8]| {};
        let mut sink = WindowsContainerOutputSink::new(&request, &mut on_output);

        stdout_writer
            .write_all(b"abcdef")
            .expect("stdout writer accepts test bytes");
        stderr_writer
            .write_all(b"12345")
            .expect("stderr writer accepts test bytes");
        let progress = reader
            .drain_available(&mut sink)
            .expect("available bytes drain from each owned pipe");
        assert_eq!(progress.stdout_bytes_read, 6);
        assert_eq!(progress.stderr_bytes_read, 5);
        assert!(!progress.stdout_closed);
        assert!(!progress.stderr_closed);
        drop(stdout_writer);
        drop(stderr_writer);

        let closed = reader
            .drain_available(&mut sink)
            .expect("closed pipe handles are recognized");
        assert!(closed.is_complete());
        drop(reader);
        drop(process);

        let output = sink.finish();
        assert_eq!(output.stdout(), b"abc");
        assert_eq!(output.stderr(), b"12");
        assert!(output.stdout_truncated());
        assert!(output.stderr_truncated());
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                "create_process",
                "close_guest_handle",
                "close_guest_handle",
                "terminate_process",
                "close_process"
            ]
        );
    }

    #[test]
    fn output_poll_enforces_per_stream_work_budget_while_writers_continue() {
        let (stdout_handle, stdout_writer) = create_pipe();
        let (stderr_handle, stderr_writer) = create_pipe();
        let api = MockHcsApi::with_output_handles([], stdout_handle, stderr_handle);
        let mut process = owner(&api);
        let mut reader = HcsGuestProcessOutputReader::from_process(&mut process)
            .expect("both guest output handles transfer together");
        let request = output_request();
        let mut on_output = |_: WindowsContainerOutputStream, _: &[u8]| {};
        let mut sink = WindowsContainerOutputSink::new(&request, &mut on_output);
        let stdout_total = 192 * 1024;
        let stderr_total = 160 * 1024;
        let stdout_thread = std::thread::spawn(move || {
            let mut writer = stdout_writer;
            writer
                .write_all(&vec![b'o'; stdout_total])
                .expect("stdout test pipe accepts the full bounded burst");
        });
        let stderr_thread = std::thread::spawn(move || {
            let mut writer = stderr_writer;
            writer
                .write_all(&vec![b'e'; stderr_total])
                .expect("stderr test pipe accepts the full bounded burst");
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stdout_read = 0usize;
        let mut stderr_read = 0usize;
        loop {
            let progress = reader
                .drain_available(&mut sink)
                .expect("bounded polling drains available bytes");
            assert!(progress.stdout_bytes_read <= 64 * 1024);
            assert!(progress.stderr_bytes_read <= 64 * 1024);
            stdout_read += progress.stdout_bytes_read;
            stderr_read += progress.stderr_bytes_read;
            if progress.is_complete() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pipe drain has a deadline"
            );
            if progress.stdout_bytes_read == 0 && progress.stderr_bytes_read == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        stdout_thread.join().expect("stdout writer exits");
        stderr_thread.join().expect("stderr writer exits");
        drop(reader);
        drop(process);

        assert_eq!(stdout_read, stdout_total);
        assert_eq!(stderr_read, stderr_total);
        let output = sink.finish();
        assert_eq!(output.stdout(), b"ooo");
        assert_eq!(output.stderr(), b"ee");
        assert!(output.stdout_truncated());
        assert!(output.stderr_truncated());
    }

    #[test]
    fn dropping_running_process_terminates_then_closes_pipes_and_process() {
        let api = MockHcsApi::new([]);
        drop(owner(&api));
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                "create_process",
                "terminate_process",
                "close_stdout",
                "close_stderr",
                "close_process"
            ]
        );
    }

    #[test]
    fn failed_explicit_termination_is_retried_once_by_drop() {
        let api = MockHcsApi::new([]);
        *api.fail_first_termination.lock().unwrap() = true;
        let mut process = owner(&api);
        assert!(process.terminate().is_err());
        drop(process);
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                "create_process",
                "terminate_process",
                "terminate_process",
                "close_stdout",
                "close_stderr",
                "close_process"
            ]
        );
    }

    #[test]
    fn malformed_or_mismatched_process_status_fails_closed() {
        assert!(parse_process_status("{}", 42).is_err());
        assert!(
            parse_process_status(
                r#"{"ProcessId":43,"Exited":true,"ExitCode":0,"LastWaitResult":0}"#,
                42
            )
            .is_err()
        );
        assert!(
            parse_process_status(
                r#"{"ProcessId":42,"Exited":true,"ExitCode":0,"LastWaitResult":-1}"#,
                42
            )
            .is_err()
        );
        assert!(
            parse_process_status(r#"{"ProcessId":42,"Exited":true,"LastWaitResult":0}"#, 42)
                .is_err()
        );
        assert!(
            parse_process_status(
                &format!(
                    "{{\"ProcessId\":42,\"Padding\":\"{}\"}}",
                    "x".repeat(MAX_PROCESS_STATUS_BYTES)
                ),
                42
            )
            .is_err()
        );
    }
}
