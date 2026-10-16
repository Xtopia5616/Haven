//! Supervised lifetime for one fixed managed MCP guest process generation.
//!
//! This owner holds the compute system and the bounded stdio adapters on the
//! caller's thread. The broker service still owns authorization, session-pipe
//! relay, workspace-attempt recovery and the decision to accept a generation.

use super::WindowsContainerSandboxConfig;
use super::execution::McpNodePackageProof;
use super::hcs_process::HcsGuestProcessParameters;
use super::hcs_process_input::HcsGuestProcessStdioOwner;
use super::hcs_process_output::{HcsOutputChunkSink, HcsOutputDrainProgress};
use super::hcs_process_runtime::GuestProcessExitStatus;
use super::hcs_runtime::{HcsApi, HcsComputeSystemOwner, HcsStoppedComputeSystem};
use std::io;
use std::time::Duration;

/// Owns one active HCS compute system and at most one fixed-profile MCP
/// process. It intentionally has no host workspace input mapping.
#[allow(dead_code)] // Connected to BrokerPipeListener by the following integration slice.
pub(super) struct HcsMcpGuestSessionOwner<'api> {
    stdio: Option<HcsGuestProcessStdioOwner<'api>>,
    system: Option<HcsComputeSystemOwner<'api>>,
}

#[allow(dead_code)] // Connected to BrokerPipeListener by the following integration slice.
impl<'api> HcsMcpGuestSessionOwner<'api> {
    /// Create and start a quota-configured compute system without mounting an
    /// ExecutionWorkspace. Process launch waits for the caller's attach gate.
    pub(super) fn create(
        api: &'api dyn HcsApi,
        config: &WindowsContainerSandboxConfig,
        operation_timeout: Duration,
    ) -> io::Result<Self> {
        let mut system = HcsComputeSystemOwner::create(api, config, operation_timeout)?;
        system.start(operation_timeout)?;
        Ok(Self {
            stdio: None,
            system: Some(system),
        })
    }

    /// Start only the catalog-pinned Node package represented by the opaque
    /// startup-attested proof. Caller-supplied command, argv and environment
    /// never enter this boundary.
    pub(super) fn launch_package(
        &mut self,
        package_proof: &McpNodePackageProof,
        operation_timeout: Duration,
    ) -> io::Result<()> {
        if self.stdio.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "MCP guest process has already been started",
            ));
        }
        let parameters = HcsGuestProcessParameters::from_mcp_package(package_proof)?;
        let system = self.system.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "MCP guest session has already been stopped",
            )
        })?;
        let process = system.create_session_guest_process(&parameters, operation_timeout)?;
        self.stdio = Some(HcsGuestProcessStdioOwner::new(process)?);
        Ok(())
    }

    pub(super) fn try_write_stdin_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stdio_mut()?.try_write_stdin_chunk(bytes)
    }

    pub(super) fn close_stdin(&mut self) -> io::Result<()> {
        self.stdio_mut()?.close_stdin();
        Ok(())
    }

    pub(super) fn poll_stdin_writer(&mut self) -> io::Result<bool> {
        self.stdio_mut()?.poll_stdin_writer()
    }

    pub(super) fn poll_process_exit(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<GuestProcessExitStatus>> {
        self.stdio_mut()?.poll_process_exit(timeout)
    }

    pub(super) fn drain_output(
        &mut self,
        sink: &mut dyn HcsOutputChunkSink,
    ) -> io::Result<HcsOutputDrainProgress> {
        self.stdio_mut()?.drain_output(sink)
    }

    /// Stop the guest process and compute system before returning the proof
    /// that makes its writable layer safe to inspect or remove.
    pub(super) fn stop(&mut self) -> io::Result<HcsStoppedComputeSystem> {
        let process_stop = self
            .stdio
            .as_mut()
            .map(HcsGuestProcessStdioOwner::abort)
            .transpose();
        self.stdio.take();

        let system = self.system.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "MCP guest session has already been stopped",
            )
        })?;
        let stopped = system.terminate()?;
        self.system.take();
        process_stop?;
        Ok(stopped)
    }

    fn stdio_mut(&mut self) -> io::Result<&mut HcsGuestProcessStdioOwner<'api>> {
        self.stdio.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "MCP guest process has not been started",
            )
        })
    }
}

impl Drop for HcsMcpGuestSessionOwner<'_> {
    fn drop(&mut self) {
        if let Some(stdio) = self.stdio.as_mut() {
            let _ = stdio.abort();
        }
        self.stdio.take();
        if let Some(system) = self.system.as_mut() {
            let _ = system.terminate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows_container::WindowsContainerLayer;
    use crate::windows_container::execution::image_catalog::McpNodePackageProfile;
    use crate::windows_container::execution::{
        WindowsContainerOutputStream, test_mcp_node_package_proof,
    };
    use crate::windows_container::hcs_runtime::{
        HcsProcessInformation, RawComputeSystem, RawGuestHandle, RawGuestProcess,
    };
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::windows::io::FromRawHandle;
    use std::path::PathBuf;
    use std::ptr::{null, null_mut};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::TempDir;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Pipes::CreatePipe;

    struct GuestPipePeers {
        stdin: File,
        stdout: File,
        stderr: File,
    }

    struct MockHcsApi {
        calls: Mutex<Vec<&'static str>>,
        process_exited: AtomicBool,
        pipe_peers: Mutex<Option<GuestPipePeers>>,
    }

    impl MockHcsApi {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                process_exited: AtomicBool::new(false),
                pipe_peers: Mutex::new(None),
            }
        }

        fn write_output(&self, stdout: &[u8], stderr: &[u8]) {
            let mut guard = self.pipe_peers.lock().unwrap();
            let peers = guard.as_mut().expect("package process has started");
            peers.stdout.write_all(stdout).unwrap();
            peers.stderr.write_all(stderr).unwrap();
        }

        fn read_guest_stdin(&self) -> Vec<u8> {
            let mut stdin = self
                .pipe_peers
                .lock()
                .unwrap()
                .as_mut()
                .expect("package process has started")
                .stdin
                .try_clone()
                .unwrap();
            let mut bytes = Vec::new();
            stdin.read_to_end(&mut bytes).unwrap();
            bytes
        }

        fn mark_process_exited(&self) {
            self.process_exited.store(true, Ordering::SeqCst);
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
            assert!(!configuration.contains("workspace-input"));
            self.calls.lock().unwrap().push("create_system");
            Ok(1_usize as RawComputeSystem)
        }

        fn start_compute_system(
            &self,
            _system: RawComputeSystem,
            _timeout: Duration,
        ) -> io::Result<()> {
            self.calls.lock().unwrap().push("start_system");
            Ok(())
        }

        fn terminate_compute_system(&self, _system: RawComputeSystem) -> io::Result<()> {
            self.calls.lock().unwrap().push("terminate_system");
            Ok(())
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
            assert!(parameters.contains("\"CreateStdInPipe\":true"));
            assert!(parameters.contains("C:\\\\Haven\\\\McpPackages\\\\filesystem"));
            self.calls.lock().unwrap().push("create_mcp_process");
            let (stdin_read, stdin_write) = anonymous_pipe()?;
            let (stdout_read, stdout_write) = anonymous_pipe()?;
            let (stderr_read, stderr_write) = anonymous_pipe()?;
            *self.pipe_peers.lock().unwrap() = Some(GuestPipePeers {
                stdin: unsafe { File::from_raw_handle(stdin_read) },
                stdout: unsafe { File::from_raw_handle(stdout_write) },
                stderr: unsafe { File::from_raw_handle(stderr_write) },
            });
            Ok(HcsProcessInformation {
                process: 42_usize as RawGuestProcess,
                process_id: 42,
                stdin: stdin_write,
                stdout: stdout_read,
                stderr: stderr_read,
            })
        }

        fn wait_for_guest_process_exit(
            &self,
            _process: RawGuestProcess,
            _timeout: Duration,
        ) -> io::Result<Option<String>> {
            self.calls.lock().unwrap().push("poll_process");
            if self.process_exited.load(Ordering::SeqCst) {
                Ok(Some(
                    r#"{"ProcessId":42,"Exited":true,"ExitCode":0,"LastWaitResult":0}"#.to_owned(),
                ))
            } else {
                Ok(None)
            }
        }

        fn terminate_guest_process(&self, _process: RawGuestProcess) -> io::Result<()> {
            self.calls.lock().unwrap().push("terminate_process");
            self.process_exited.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn close_guest_process(&self, _process: RawGuestProcess) {
            self.calls.lock().unwrap().push("close_process");
        }

        fn close_guest_handle(&self, handle: RawGuestHandle) {
            unsafe { CloseHandle(handle) };
        }
    }

    #[derive(Default)]
    struct CapturedOutput(Vec<(WindowsContainerOutputStream, Vec<u8>)>);

    impl HcsOutputChunkSink for CapturedOutput {
        fn write_chunk(&mut self, stream: WindowsContainerOutputStream, bytes: &[u8]) {
            self.0.push((stream, bytes.to_vec()));
        }
    }

    fn anonymous_pipe() -> io::Result<(HANDLE, HANDLE)> {
        let mut read = null_mut();
        let mut write = null_mut();
        if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((read, write))
    }

    fn config(temp: &TempDir) -> WindowsContainerSandboxConfig {
        WindowsContainerSandboxConfig::new(
            "test-mcp-session",
            16 * 1024 * 1024,
            vec![WindowsContainerLayer::new("base", temp.path()).unwrap()],
        )
        .unwrap()
        .with_test_writable_layer_folder(PathBuf::from(temp.path()))
        .unwrap()
    }

    fn package_proof() -> McpNodePackageProof {
        test_mcp_node_package_proof(McpNodePackageProfile::test_profile(
            "filesystem",
            "1.0.0",
            "index.mjs",
            [7; 32],
        ))
    }

    #[test]
    fn package_session_owns_guest_stdio_until_confirmed_system_stop() {
        let temp = tempfile::tempdir().unwrap();
        let api = MockHcsApi::new();
        let mut session =
            HcsMcpGuestSessionOwner::create(&api, &config(&temp), Duration::from_secs(5)).unwrap();
        session
            .launch_package(&package_proof(), Duration::from_secs(5))
            .unwrap();

        api.write_output(b"{\"jsonrpc\":\"2.0\"}", b"diagnostic");
        let mut output = CapturedOutput::default();
        let progress = session.drain_output(&mut output).unwrap();
        assert_eq!(progress.stdout_bytes_read, b"{\"jsonrpc\":\"2.0\"}".len());
        assert_eq!(progress.stderr_bytes_read, b"diagnostic".len());
        assert!(output.0.contains(&(
            WindowsContainerOutputStream::Stdout,
            b"{\"jsonrpc\":\"2.0\"}".to_vec()
        )));
        assert!(
            output
                .0
                .contains(&(WindowsContainerOutputStream::Stderr, b"diagnostic".to_vec()))
        );

        session.try_write_stdin_chunk(b"request frame").unwrap();
        session.close_stdin().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if session.poll_stdin_writer().unwrap() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(api.read_guest_stdin(), b"request frame");

        api.mark_process_exited();
        assert_eq!(
            session
                .poll_process_exit(Duration::from_millis(1))
                .unwrap()
                .unwrap()
                .exit_code,
            0
        );
        let stopped = session.stop().unwrap();
        assert_eq!(stopped.writable_layer_path(), temp.path());
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                "create_system",
                "start_system",
                "create_mcp_process",
                "poll_process",
                "close_process",
                "terminate_system",
                "close_system",
            ]
        );
    }

    #[test]
    fn dropping_live_package_session_terminates_process_before_compute_system() {
        let temp = tempfile::tempdir().unwrap();
        let api = MockHcsApi::new();
        let mut session =
            HcsMcpGuestSessionOwner::create(&api, &config(&temp), Duration::from_secs(5)).unwrap();
        session
            .launch_package(&package_proof(), Duration::from_secs(5))
            .unwrap();
        drop(session);

        let calls = api.calls.lock().unwrap();
        let process_stop = calls
            .iter()
            .position(|call| *call == "terminate_process")
            .unwrap();
        let system_stop = calls
            .iter()
            .position(|call| *call == "terminate_system")
            .unwrap();
        assert!(process_stop < system_stop);
    }
}
