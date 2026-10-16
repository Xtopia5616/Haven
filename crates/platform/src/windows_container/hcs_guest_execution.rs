//! End-to-end owner for one guest seed plus one bounded Shell process.
//!
//! The result carries a stopped-system proof. Callers must keep the writable
//! layer alive until this function returns, and must quarantine the attempt
//! when a failure does not include a confirmed stop.

use super::execution::{
    WindowsContainerCapturedOutput, WindowsContainerExecutionRequest, WindowsContainerOutputSink,
};
use super::hcs_process::HcsGuestProcessParameters;
use super::hcs_process_output::{HcsGuestProcessOutputReader, HcsOutputChunkSink};
use super::hcs_runtime::{HcsApi, HcsComputeSystemOwner, HcsStoppedComputeSystem, NativeHcsApi};
use std::io;
use std::thread::sleep;
use std::time::{Duration, Instant};

const MAX_HCS_OPERATION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);
const IDLE_POLL_SLEEP: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HcsGuestExecutionFailureKind {
    Cancelled,
    TimedOut,
    HcsFailed,
    WorkspaceSeedFailed,
}

/// Whether the enclosing attempt may be removed after this failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HcsGuestExecutionFailure {
    pub(super) kind: HcsGuestExecutionFailureKind,
    pub(super) cleanup_safe: bool,
}

/// Guest output can be exported only while this successful stop proof is held.
pub(super) struct HcsGuestExecutionOutcome {
    pub(super) stopped_system: HcsStoppedComputeSystem,
    pub(super) exit_code: u32,
    pub(super) output: WindowsContainerCapturedOutput,
}

/// Start one fixed compute system, seed its writable workspace, run one shell,
/// drain both output pipes, and explicitly stop the system before returning.
/// This function does not create scratch storage or make quota claims; the
/// caller must hold the formatted quota-bound layer owner for the full call.
pub(super) fn execute_guest(
    api: &dyn HcsApi,
    config: &super::WindowsContainerSandboxConfig,
    request: &WindowsContainerExecutionRequest,
    mut is_cancelled: impl FnMut() -> bool,
    output: WindowsContainerOutputSink<'_>,
) -> Result<HcsGuestExecutionOutcome, HcsGuestExecutionFailure> {
    let deadline = Instant::now()
        .checked_add(request.limits().timeout())
        .ok_or_else(|| failure(HcsGuestExecutionFailureKind::TimedOut, true))?;
    if is_cancelled() {
        return Err(failure(HcsGuestExecutionFailureKind::Cancelled, true));
    }

    let create_timeout = operation_timeout(deadline).map_err(|kind| failure(kind, true))?;
    let mut system = HcsComputeSystemOwner::create(api, config, create_timeout)
        .map_err(|_| failure(HcsGuestExecutionFailureKind::HcsFailed, false))?;

    let execution = (|| {
        check_cancellation(&mut is_cancelled, deadline)?;
        system
            .start(operation_timeout(deadline)?)
            .map_err(|_| HcsGuestExecutionFailureKind::HcsFailed)?;

        let seed_parameters = HcsGuestProcessParameters::workspace_seed()
            .map_err(|_| HcsGuestExecutionFailureKind::HcsFailed)?;
        let mut discard_output = DiscardHcsOutput;
        let seed_status = run_guest_process(
            &system,
            &seed_parameters,
            deadline,
            &mut is_cancelled,
            &mut discard_output,
        )?;
        if seed_status.exit_code != 0 {
            return Err(HcsGuestExecutionFailureKind::WorkspaceSeedFailed);
        }

        check_cancellation(&mut is_cancelled, deadline)?;
        let command_parameters = HcsGuestProcessParameters::from_request(request)
            .map_err(|_| HcsGuestExecutionFailureKind::HcsFailed)?;
        let mut output = output;
        let command_status = run_guest_process(
            &system,
            &command_parameters,
            deadline,
            &mut is_cancelled,
            &mut output,
        )?;
        Ok((command_status.exit_code, output.finish()))
    })();

    match execution {
        Ok((exit_code, output)) => match system.terminate() {
            Ok(stopped_system) => Ok(HcsGuestExecutionOutcome {
                stopped_system,
                exit_code,
                output,
            }),
            Err(_) => Err(failure(HcsGuestExecutionFailureKind::HcsFailed, false)),
        },
        Err(kind) => Err(stop_after_failure(&mut system, kind)),
    }
}

fn run_guest_process(
    system: &HcsComputeSystemOwner<'_>,
    parameters: &HcsGuestProcessParameters,
    deadline: Instant,
    is_cancelled: &mut impl FnMut() -> bool,
    output: &mut dyn HcsOutputChunkSink,
) -> Result<super::hcs_process_runtime::GuestProcessExitStatus, HcsGuestExecutionFailureKind> {
    check_cancellation(is_cancelled, deadline)?;
    let mut process = system
        .create_guest_process(parameters, operation_timeout(deadline)?)
        .map_err(|_| HcsGuestExecutionFailureKind::HcsFailed)?;
    let mut reader = HcsGuestProcessOutputReader::from_process(&mut process)
        .map_err(|_| HcsGuestExecutionFailureKind::HcsFailed)?;

    loop {
        check_cancellation(is_cancelled, deadline).inspect_err(|_| {
            let _ = process.terminate();
        })?;

        let wait_timeout = operation_timeout(deadline)?.min(PROCESS_POLL_INTERVAL);
        let exit_status = process
            .poll_exit(wait_timeout)
            .map_err(|_| HcsGuestExecutionFailureKind::HcsFailed)?;
        let progress = reader
            .drain_available(output)
            .map_err(|_| HcsGuestExecutionFailureKind::HcsFailed)?;
        if let Some(status) = exit_status
            && progress.is_complete()
        {
            return Ok(status);
        }
        if progress.stdout_bytes_read == 0
            && progress.stderr_bytes_read == 0
            && let Some(remaining) = deadline.checked_duration_since(Instant::now())
        {
            sleep(remaining.min(IDLE_POLL_SLEEP));
        }
    }
}

fn check_cancellation(
    is_cancelled: &mut impl FnMut() -> bool,
    deadline: Instant,
) -> Result<(), HcsGuestExecutionFailureKind> {
    if is_cancelled() {
        Err(HcsGuestExecutionFailureKind::Cancelled)
    } else if Instant::now() >= deadline {
        Err(HcsGuestExecutionFailureKind::TimedOut)
    } else {
        Ok(())
    }
}

fn operation_timeout(deadline: Instant) -> Result<Duration, HcsGuestExecutionFailureKind> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(HcsGuestExecutionFailureKind::TimedOut)?;
    Ok(remaining.min(MAX_HCS_OPERATION_TIMEOUT))
}

fn stop_after_failure(
    system: &mut HcsComputeSystemOwner<'_>,
    kind: HcsGuestExecutionFailureKind,
) -> HcsGuestExecutionFailure {
    failure(kind, system.terminate().is_ok())
}

fn failure(kind: HcsGuestExecutionFailureKind, cleanup_safe: bool) -> HcsGuestExecutionFailure {
    HcsGuestExecutionFailure { kind, cleanup_safe }
}

struct DiscardHcsOutput;

impl HcsOutputChunkSink for DiscardHcsOutput {
    fn write_chunk(
        &mut self,
        _stream: super::execution::WindowsContainerOutputStream,
        _bytes: &[u8],
    ) {
    }
}

/// Load the native HCS API only inside the privileged broker execution path.
#[cfg_attr(test, allow(dead_code))]
pub(super) fn native_api() -> io::Result<NativeHcsApi> {
    NativeHcsApi::load()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::{
        DirectoryCapability, DirectoryTreeCopyLimits, DirectoryTreeDiffLimits,
    };
    use crate::windows_container::execution::{
        WindowsContainerExecutionLimits, WindowsContainerOutputStream, WindowsContainerShell,
    };
    use crate::windows_container::hcs_runtime::{
        HcsProcessInformation, RawComputeSystem, RawGuestHandle, RawGuestProcess,
    };
    use crate::windows_container::{WindowsContainerLayer, WindowsContainerSandboxConfig};
    use std::collections::{HashMap, VecDeque};
    use std::fs::File;
    use std::io::Write;
    use std::os::windows::io::FromRawHandle;
    use std::path::PathBuf;
    use std::ptr::null_mut;
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Pipes::CreatePipe;

    struct MockHcsApi {
        calls: Mutex<Vec<&'static str>>,
        exit_codes: Mutex<VecDeque<u32>>,
        process_status: Mutex<HashMap<u32, u32>>,
        process_count: std::sync::atomic::AtomicU32,
        started: std::sync::atomic::AtomicBool,
        fail_system_termination: bool,
    }

    impl MockHcsApi {
        fn new(exit_codes: impl IntoIterator<Item = u32>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                exit_codes: Mutex::new(exit_codes.into_iter().collect()),
                process_status: Mutex::new(HashMap::new()),
                process_count: std::sync::atomic::AtomicU32::new(0),
                started: std::sync::atomic::AtomicBool::new(false),
                fail_system_termination: false,
            }
        }

        fn process_count(&self) -> u32 {
            self.process_count.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl HcsApi for MockHcsApi {
        fn create_compute_system(
            &self,
            _id: &str,
            configuration: &str,
            _timeout: Duration,
        ) -> io::Result<RawComputeSystem> {
            assert!(configuration.contains("\"HvPartition\":true"));
            self.calls.lock().unwrap().push("create_system");
            Ok(1usize as RawComputeSystem)
        }

        fn start_compute_system(
            &self,
            _system: RawComputeSystem,
            _timeout: Duration,
        ) -> io::Result<()> {
            self.calls.lock().unwrap().push("start_system");
            self.started
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn terminate_compute_system(&self, _system: RawComputeSystem) -> io::Result<()> {
            self.calls.lock().unwrap().push("terminate_system");
            if self.fail_system_termination {
                Err(io::Error::other("mock termination failure"))
            } else {
                Ok(())
            }
        }

        fn close_compute_system(&self, _system: RawComputeSystem) {
            self.calls.lock().unwrap().push("close_system");
        }

        fn create_guest_process(
            &self,
            _system: RawComputeSystem,
            parameters: &str,
            _timeout: Duration,
        ) -> io::Result<HcsProcessInformation> {
            self.calls.lock().unwrap().push("create_process");
            let index = self
                .process_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let process_id = 41 + index;
            let exit_code = self.exit_codes.lock().unwrap().pop_front().unwrap_or(0);
            self.process_status
                .lock()
                .unwrap()
                .insert(process_id, exit_code);
            let output = if index == 0 {
                b"seed output".as_slice()
            } else {
                b"command output".as_slice()
            };
            let error = if index == 0 {
                b"seed error".as_slice()
            } else {
                b"command error".as_slice()
            };
            assert!(parameters.contains("\"RestrictedToken\":true"));
            let stdout = pipe_with_bytes(output)?;
            let stderr = pipe_with_bytes(error)?;
            Ok(HcsProcessInformation {
                process: process_id as usize as RawGuestProcess,
                process_id,
                stdin: null_mut(),
                stdout,
                stderr,
            })
        }

        fn wait_for_guest_process_exit(
            &self,
            process: RawGuestProcess,
            _timeout: Duration,
        ) -> io::Result<Option<String>> {
            self.calls.lock().unwrap().push("wait_process");
            let process_id = process as usize as u32;
            let exit_code = *self
                .process_status
                .lock()
                .unwrap()
                .get(&process_id)
                .unwrap();
            Ok(Some(format!(
                r#"{{"ProcessId":{process_id},"Exited":true,"ExitCode":{exit_code},"LastWaitResult":0}}"#
            )))
        }

        fn terminate_guest_process(&self, _process: RawGuestProcess) -> io::Result<()> {
            self.calls.lock().unwrap().push("terminate_process");
            Ok(())
        }

        fn close_guest_process(&self, _process: RawGuestProcess) {
            self.calls.lock().unwrap().push("close_process");
        }

        fn close_guest_handle(&self, handle: RawGuestHandle) {
            self.calls.lock().unwrap().push("close_pipe");
            unsafe { CloseHandle(handle as HANDLE) };
        }
    }

    fn pipe_with_bytes(bytes: &[u8]) -> io::Result<RawGuestHandle> {
        let mut read_handle: HANDLE = null_mut();
        let mut write_handle: HANDLE = null_mut();
        if unsafe { CreatePipe(&mut read_handle, &mut write_handle, null_mut(), 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut writer = unsafe { File::from_raw_handle(write_handle) };
        writer.write_all(bytes)?;
        drop(writer);
        Ok(read_handle)
    }

    fn config() -> (tempfile::TempDir, WindowsContainerSandboxConfig) {
        let root = tempfile::tempdir().unwrap();
        let workspace = DirectoryCapability::open(root.path()).unwrap();
        let layer = WindowsContainerLayer::new("base-id", r"C:\Haven\image\base").unwrap();
        let config = WindowsContainerSandboxConfig::new("HavenSandbox-test", 1024, vec![layer])
            .unwrap()
            .with_test_writable_layer_folder(PathBuf::from(r"C:\Haven\scratch\layer"))
            .unwrap()
            .with_read_only_workspace_input(workspace)
            .unwrap();
        (root, config)
    }

    fn request() -> WindowsContainerExecutionRequest {
        let copy = DirectoryTreeCopyLimits::new(16, 4, 1024, 4096).unwrap();
        let diff = DirectoryTreeDiffLimits::new(copy, 16, 1024, 4096).unwrap();
        let limits =
            WindowsContainerExecutionLimits::new(1024, Duration::from_secs(2), 32, 32, copy, diff)
                .unwrap();
        WindowsContainerExecutionRequest::new(
            WindowsContainerShell::Cmd,
            "echo hello",
            "",
            1,
            1,
            limits,
        )
        .unwrap()
    }

    fn failure_from(
        result: Result<HcsGuestExecutionOutcome, HcsGuestExecutionFailure>,
    ) -> HcsGuestExecutionFailure {
        match result {
            Err(error) => error,
            Ok(_) => panic!("guest execution unexpectedly succeeded"),
        }
    }

    #[test]
    fn guest_runner_seeds_then_runs_and_stops_before_returning_output() {
        let (_root, config) = config();
        let request = request();
        let api = MockHcsApi::new([0, 7]);
        let mut chunks = Vec::new();
        let mut on_output = |stream, bytes: &[u8]| chunks.push((stream, bytes.to_vec()));
        let sink = WindowsContainerOutputSink::new(&request, &mut on_output);
        let outcome = execute_guest(&api, &config, &request, || false, sink).unwrap();

        assert_eq!(outcome.exit_code, 7);
        assert_eq!(outcome.output.stdout(), b"command output");
        assert_eq!(outcome.output.stderr(), b"command error");
        assert_eq!(
            chunks,
            [
                (
                    WindowsContainerOutputStream::Stdout,
                    b"command output".to_vec()
                ),
                (
                    WindowsContainerOutputStream::Stderr,
                    b"command error".to_vec()
                ),
            ]
        );
        assert_eq!(api.process_count(), 2);
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                "create_system",
                "start_system",
                "create_process",
                "wait_process",
                "close_pipe",
                "close_pipe",
                "close_process",
                "create_process",
                "wait_process",
                "close_pipe",
                "close_pipe",
                "close_process",
                "terminate_system",
                "close_system",
            ]
        );
        assert!(
            outcome
                .stopped_system
                .writable_layer_path()
                .ends_with("layer")
        );
    }

    #[test]
    fn seed_failure_stops_the_system_and_never_starts_the_shell() {
        let (_root, config) = config();
        let request = request();
        let api = MockHcsApi::new([9]);
        let mut on_output = |_: WindowsContainerOutputStream, _: &[u8]| {};
        let sink = WindowsContainerOutputSink::new(&request, &mut on_output);
        let error = failure_from(execute_guest(&api, &config, &request, || false, sink));

        assert_eq!(
            error.kind,
            HcsGuestExecutionFailureKind::WorkspaceSeedFailed
        );
        assert!(error.cleanup_safe);
        assert_eq!(api.process_count(), 1);
        assert_eq!(api.calls.lock().unwrap().last(), Some(&"close_system"));
    }

    #[test]
    fn failed_compute_system_stop_marks_the_attempt_unsafe_to_clean() {
        let (_root, config) = config();
        let request = request();
        let api = MockHcsApi {
            fail_system_termination: true,
            ..MockHcsApi::new([])
        };
        let mut on_output = |_: WindowsContainerOutputStream, _: &[u8]| {};
        let sink = WindowsContainerOutputSink::new(&request, &mut on_output);
        let error = failure_from(execute_guest(
            &api,
            &config,
            &request,
            || api.started.load(std::sync::atomic::Ordering::SeqCst),
            sink,
        ));

        assert_eq!(error.kind, HcsGuestExecutionFailureKind::Cancelled);
        assert!(!error.cleanup_safe);
    }

    #[test]
    fn cancellation_before_creation_has_no_hcs_side_effects() {
        let (_root, config) = config();
        let request = request();
        let api = MockHcsApi::new([]);
        let mut on_output = |_: WindowsContainerOutputStream, _: &[u8]| {};
        let sink = WindowsContainerOutputSink::new(&request, &mut on_output);
        let error = failure_from(execute_guest(&api, &config, &request, || true, sink));

        assert_eq!(error.kind, HcsGuestExecutionFailureKind::Cancelled);
        assert!(error.cleanup_safe);
        assert!(api.calls.lock().unwrap().is_empty());
    }
}
