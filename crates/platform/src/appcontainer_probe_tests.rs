//! Windows-only proof that AppContainer can combine an explicitly granted
//! workspace directory with default-deny file and network access.

use std::ffi::{OsStr, OsString};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::Duration;

use crate::filesystem::{DirectoryCapability, DirectoryTreeCopyLimits};
use crate::windows_sandbox::AppContainerWorkspace;
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Security::SECURITY_CAPABILITIES;
use windows_sys::Win32::System::Threading::PROCESS_CREATION_FLAGS;
use windows_sys::Win32::System::Threading::TerminateProcess;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION,
    STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::process_containment::{ChildProcessHandle, ProcessContainment};

const APP_CONTAINER_TEST_TIMEOUT: Duration = Duration::from_secs(25);

#[test]
fn appcontainer_process_is_limited_to_granted_workspace_and_has_no_network_capability() {
    let fixture = tempfile::tempdir().expect("create AppContainer test fixture");
    let source_path = fixture.path().join("source");
    std::fs::create_dir(&source_path).expect("create snapshot source");
    let source_executable = source_path.join("appcontainer-probe.exe");
    std::fs::copy(
        std::env::current_exe().expect("current test executable"),
        &source_executable,
    )
    .expect("copy test executable into snapshot source");
    let outside_file = fixture.path().join("outside-sentinel.txt");
    std::fs::write(&outside_file, "outside-sentinel").expect("create outside sentinel");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local-only network probe");
    listener
        .set_nonblocking(true)
        .expect("make the local probe nonblocking");
    let probe_address = listener.local_addr().expect("read probe address");
    let baseline_connection = TcpStream::connect_timeout(&probe_address, Duration::from_secs(2))
        .expect("host process can reach its local probe listener");
    let _ = listener
        .accept()
        .expect("accept the host process baseline connection");
    drop(baseline_connection);
    let network_probe_port = probe_address.port();

    let source = DirectoryCapability::open(&source_path).expect("open snapshot source capability");
    let limits = DirectoryTreeCopyLimits::new(16, 4, 512 * 1024 * 1024, 1024 * 1024 * 1024)
        .expect("valid bounded snapshot limits");
    let workspace = AppContainerWorkspace::create(&source, 7, 11, limits, || false)
        .expect("create a policy-bound low-integrity AppContainer workspace");
    assert_eq!(workspace.execution_workspace_generation(), 7);
    assert_eq!(workspace.policy_revision(), 11);
    assert_eq!(workspace.snapshot_summary().file_count, 1);

    let containment = ProcessContainment::new().expect("create process-tree Job Object");
    let mut process = spawn_appcontainer_child_probe(
        &containment,
        workspace.app_container_sid(),
        workspace.root(),
        &outside_file,
        network_probe_port,
    )
    .expect("spawn a suspended AppContainer probe process");

    let exit_code = wait_for_process(&mut process, APP_CONTAINER_TEST_TIMEOUT)
        .expect("wait for the AppContainer probe");
    let sandbox_root = workspace.root().to_path_buf();
    let result_path = workspace.root().join("appcontainer-result.txt");
    let result = std::fs::read_to_string(&result_path).unwrap_or_default();
    let inside_path = workspace.root().join("inside-workspace.txt");
    let inside_file_was_written = inside_path.is_file();
    let outside_content = std::fs::read_to_string(&outside_file).expect("read outside sentinel");
    let accepted_network_connection = match listener.accept() {
        Ok((_stream, _address)) => true,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("local network probe failed: {error}"),
    };

    drop(process);
    drop(containment);
    workspace
        .cleanup()
        .expect("remove the workspace and unique AppContainer profile");
    assert!(!sandbox_root.exists(), "sandbox workspace was removed");
    assert_eq!(source.entries(2).unwrap().len(), 1);
    assert!(
        !source_path.join("inside-workspace.txt").exists(),
        "AppContainer writes did not reach the canonical source"
    );

    assert_eq!(
        exit_code, 0,
        "AppContainer probe process failed; result={result:?}, inside_file={}",
        inside_file_was_written
    );
    assert!(
        inside_file_was_written,
        "container could not write its granted workspace"
    );
    assert_eq!(outside_content, "outside-sentinel");
    assert_eq!(
        result.trim(),
        "read_denied=true;write_denied=true;network_denied=true",
        "the probe must report file and network denial, got {result:?}"
    );
    assert!(
        !accepted_network_connection,
        "AppContainer without network capabilities connected to loopback"
    );
}

#[test]
fn appcontainer_child_probe() {
    let args = std::env::args().collect::<Vec<_>>();
    let Some(probe_index) = args.iter().position(|argument| argument == "--probe-child") else {
        return;
    };
    let outside_file = PathBuf::from(&args[probe_index + 1]);
    let workspace = PathBuf::from(&args[probe_index + 2]);
    let network_probe_port = args[probe_index + 3]
        .parse::<u16>()
        .expect("probe port is numeric");

    std::fs::write(workspace.join("inside-workspace.txt"), "inside-ok")
        .expect("AppContainer writes its explicitly granted workspace");
    let read_denied = std::fs::read(&outside_file).is_err();
    let write_denied = std::fs::write(&outside_file, "unexpected").is_err();
    let network_denied = TcpStream::connect_timeout(
        &format!("127.0.0.1:{network_probe_port}")
            .parse()
            .expect("local probe address is valid"),
        Duration::from_secs(2),
    )
    .is_err();
    let result = format!(
        "read_denied={read_denied};write_denied={write_denied};network_denied={network_denied}"
    );
    std::fs::write(workspace.join("appcontainer-result.txt"), result)
        .expect("AppContainer writes the probe result to its granted workspace");
}

fn spawn_appcontainer_child_probe(
    containment: &ProcessContainment,
    app_container_sid: windows_sys::Win32::Security::PSID,
    workspace: &Path,
    outside_file: &Path,
    network_probe_port: u16,
) -> io::Result<ProcessInformationGuard> {
    let probe_executable = workspace.join("appcontainer-probe.exe");
    std::fs::copy(std::env::current_exe()?, &probe_executable)?;
    let args = [
        OsString::from("--exact"),
        OsString::from("appcontainer_probe_tests::appcontainer_child_probe"),
        OsString::from("--nocapture"),
        OsString::from("--"),
        OsString::from("--probe-child"),
        outside_file.as_os_str().to_os_string(),
        workspace.as_os_str().to_os_string(),
        network_probe_port.to_string().into(),
    ];
    let command_line = windows_command_line(&probe_executable, &args);
    let mut command_line_wide = command_line.encode_utf16().collect::<Vec<_>>();
    command_line_wide.push(0);
    let mut executable_wide = wide_null(probe_executable.as_os_str());
    let mut workspace_wide = wide_null(workspace.as_os_str());

    let mut attribute_list_size = 0usize;
    // SAFETY: the documented sizing call expects a null list and writes only
    // the required byte count to `attribute_list_size`.
    let size_result =
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut attribute_list_size) };
    if size_result != 0 || attribute_list_size == 0 {
        return Err(io::Error::other(
            "Windows did not return an AppContainer attribute-list size",
        ));
    }
    let mut attribute_storage =
        vec![0usize; attribute_list_size.div_ceil(std::mem::size_of::<usize>())];
    let attribute_list = attribute_storage.as_mut_ptr().cast::<core::ffi::c_void>();
    // SAFETY: storage is sufficiently large and aligned for the attribute
    // list. It remains alive until CreateProcessW has copied the attributes.
    if unsafe { InitializeProcThreadAttributeList(attribute_list, 1, 0, &mut attribute_list_size) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _attribute_list_guard = AttributeListGuard(attribute_list);

    let mut security_capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: app_container_sid,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    // SAFETY: the attribute list and capabilities remain valid for the call;
    // the zero capability list intentionally denies optional network access.
    if unsafe {
        UpdateProcThreadAttribute(
            attribute_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            (&mut security_capabilities as *mut SECURITY_CAPABILITIES).cast(),
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            null_mut(),
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attribute_list;
    let mut process_information = PROCESS_INFORMATION::default();
    let creation_flags: PROCESS_CREATION_FLAGS =
        CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT;

    // SAFETY: all strings are nul-terminated UTF-16; the command line and
    // command line is writable and remains alive through the call; the
    // extended startup structure points at the initialized attribute list.
    let created = unsafe {
        CreateProcessW(
            executable_wide.as_mut_ptr(),
            command_line_wide.as_mut_ptr(),
            null(),
            null(),
            0,
            creation_flags,
            // Let Windows inherit the test runner's environment for this
            // test-only helper. Production integration must use Haven's
            // filtered child environment.
            null_mut(),
            workspace_wide.as_mut_ptr(),
            (&startup as *const STARTUPINFOEXW).cast(),
            &mut process_information,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }

    let process = ProcessInformationGuard(process_information);
    containment.attach_and_resume(
        process.0.dwProcessId,
        process.0.hProcess as ChildProcessHandle,
    )?;
    Ok(process)
}

fn windows_command_line(executable: &Path, args: &[OsString]) -> String {
    std::iter::once(executable.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(quote_windows_argument)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_windows_argument(argument: &OsStr) -> String {
    let argument = argument.to_string_lossy();
    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                quoted.push(character);
                backslashes = 0;
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn wait_for_process(process: &mut ProcessInformationGuard, timeout: Duration) -> io::Result<u32> {
    let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: the process handle is owned by the guard and remains valid.
    match unsafe { WaitForSingleObject(process.0.hProcess, milliseconds) } {
        WAIT_OBJECT_0 => {
            let mut exit_code = 0u32;
            // SAFETY: `exit_code` is writable and the process handle is valid.
            if unsafe { GetExitCodeProcess(process.0.hProcess, &mut exit_code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(exit_code)
        }
        WAIT_TIMEOUT => {
            // SAFETY: terminating the exact process created by this test is
            // required to keep the regression test bounded.
            unsafe { TerminateProcess(process.0.hProcess, 1) };
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "AppContainer probe exceeded its test deadline",
            ))
        }
        WAIT_FAILED => Err(io::Error::last_os_error()),
        _ => Err(io::Error::other("unexpected process wait result")),
    }
}

struct AttributeListGuard(LPPROC_THREAD_ATTRIBUTE_LIST);

impl Drop for AttributeListGuard {
    fn drop(&mut self) {
        // SAFETY: the list was initialized successfully and is deleted once.
        unsafe { DeleteProcThreadAttributeList(self.0) };
    }
}

struct ProcessInformationGuard(PROCESS_INFORMATION);

impl Drop for ProcessInformationGuard {
    fn drop(&mut self) {
        if !self.0.hProcess.is_null()
            // SAFETY: the process handle is owned by this guard and remains
            // valid until it is closed below.
            && unsafe { WaitForSingleObject(self.0.hProcess, 0) } == WAIT_TIMEOUT
        {
            // SAFETY: this guard owns the exact child process created by the
            // current test. Kill it on unwinding so a failed assertion cannot
            // leave an AppContainer process behind.
            unsafe {
                TerminateProcess(self.0.hProcess, 1);
                WaitForSingleObject(self.0.hProcess, 5_000);
            }
        }
        if !self.0.hThread.is_null() {
            // SAFETY: this guard owns the initial thread handle.
            unsafe { CloseHandle(self.0.hThread) };
        }
        if !self.0.hProcess.is_null() {
            // SAFETY: this guard owns the process handle.
            unsafe { CloseHandle(self.0.hProcess) };
        }
    }
}
