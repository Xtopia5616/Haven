//! Explicit SCM lifecycle for Haven's optional Windows Container broker.
//!
//! The broker is a separate executable. Installation is demand-start, uses a
//! LocalSystem service token with a restricted service SID and a small
//! required-privilege set, and accepts no user-selected service configuration.

use super::BROKER_SERVICE_NAME;
use super::broker_workspace::BrokerWorkspaceScratchRoot;
use super::image_catalog::BrokerContainerImageCatalog;
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_CALL_NOT_IMPLEMENTED, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST,
    ERROR_SERVICE_NOT_ACTIVE, ERROR_SERVICE_SPECIFIC_ERROR, ERROR_TIMEOUT, GetLastError, HANDLE,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, CloseServiceHandle, ControlService, CreateServiceW, DeleteService,
    OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, RegisterServiceCtrlHandlerExW,
    SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SC_STATUS_PROCESS_INFO,
    SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
    SERVICE_CONFIG_SERVICE_SID_INFO, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL,
    SERVICE_QUERY_STATUS, SERVICE_REQUIRED_PRIVILEGES_INFOW, SERVICE_RUNNING, SERVICE_SID_INFO,
    SERVICE_START, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_HANDLE,
    SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
    StartServiceW,
};
use windows_sys::Win32::System::Threading::{ResetEvent, SetEvent, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath};

use super::broker_protocol::{BrokerControlMessage, BrokerFailureCode, BrokerFrame};
use super::named_pipe::{BrokerPeerPolicy, BrokerPipeCancellation, BrokerPipeListener};

const SERVICE_ACCOUNT: &str = r"NT AUTHORITY\LocalSystem";
const SERVICE_STOP_TIMEOUT: Duration = Duration::from_secs(15);
const SERVICE_SID_TYPE_RESTRICTED: u32 = 3;
const SERVICE_DELETE_ACCESS: u32 = 0x0001_0000;
const SERVICE_START_NAME: &str = "Haven Container Broker";
const BROKER_BINARY_NAME: &str = "haven-container-broker.exe";
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const REQUIRED_PRIVILEGES: &[&str] = &[
    "SeChangeNotifyPrivilege",
    "SeImpersonatePrivilege",
    "SeManageVolumePrivilege",
    "SeTcbPrivilege",
];

static SERVICE_STATUS_HANDLE_VALUE: AtomicIsize = AtomicIsize::new(0);
static SERVICE_CANCELLATION_EVENT: AtomicIsize = AtomicIsize::new(0);
static SERVICE_CONTROL_SIGNAL: AtomicU32 = AtomicU32::new(CONTROL_SIGNAL_NONE);
static SERVICE_CURRENT_STATE: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(SERVICE_STOPPED);
static SERVICE_CONTROL_LOCK: Mutex<()> = Mutex::new(());

const CONTROL_SIGNAL_NONE: u32 = 0;
const CONTROL_SIGNAL_STOP: u32 = 1;
const CONTROL_SIGNAL_SESSION_CHANGE: u32 = 2;

/// Observable SCM state for the optional broker service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerServiceState {
    NotInstalled,
    Stopped,
    StartPending,
    Running,
    StopPending,
    Other(u32),
}

/// Install the fixed broker service from the protected Haven installation
/// directory. The caller must be elevated; this function does not prompt.
pub fn install() -> io::Result<()> {
    let executable = std::env::current_exe()?;
    let program_files = program_files_directory()?;
    let executable = validate_install_image(&executable, &program_files)?;
    let executable_text = executable
        .to_str()
        .ok_or_else(|| invalid_input("broker executable path is not valid Unicode"))?;
    if executable_text.contains('"') {
        return Err(invalid_input("broker executable path contains a quote"));
    }
    let image_command = wide_null(&format!("\"{executable_text}\" --service"));
    let service_name = wide_null(BROKER_SERVICE_NAME);
    let display_name = wide_null(SERVICE_START_NAME);
    let account = wide_null(SERVICE_ACCOUNT);

    let manager = open_manager(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE)?;
    let service = unsafe {
        CreateServiceW(
            manager.0,
            service_name.as_ptr(),
            display_name.as_ptr(),
            SERVICE_CHANGE_CONFIG
                | SERVICE_QUERY_STATUS
                | SERVICE_START
                | SERVICE_STOP
                | SERVICE_DELETE_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_DEMAND_START,
            SERVICE_ERROR_NORMAL,
            image_command.as_ptr(),
            null(),
            null_mut(),
            null(),
            account.as_ptr(),
            null(),
        )
    };
    if service.is_null() {
        return Err(io::Error::last_os_error());
    }
    let service = OwnedServiceHandle(service);

    let setup = configure_service_identity(service.0)
        .and_then(|()| BrokerWorkspaceScratchRoot::install(&program_files))
        .and_then(|()| BrokerContainerImageCatalog::install(&program_files));
    if let Err(error) = setup {
        unsafe { DeleteService(service.0) };
        return Err(error);
    }
    Ok(())
}

/// Start the explicitly installed service and wait for its running state.
pub fn start() -> io::Result<()> {
    let manager = open_manager(SC_MANAGER_CONNECT)?;
    let service = open_service(manager.0, SERVICE_START | SERVICE_QUERY_STATUS)?;
    let status = query_service_status(service.0)?;
    if status.dwCurrentState == SERVICE_RUNNING {
        return Ok(());
    }
    if status.dwCurrentState == SERVICE_STOP_PENDING {
        wait_for_state(service.0, SERVICE_STOPPED, SERVICE_STOP_TIMEOUT)?;
    }
    if unsafe { StartServiceW(service.0, 0, null()) } == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_SERVICE_ALREADY_RUNNING {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }
    wait_for_state(service.0, SERVICE_RUNNING, SERVICE_STOP_TIMEOUT)?;
    Ok(())
}

/// Stop the service and wait until its process reports `SERVICE_STOPPED`.
pub fn stop() -> io::Result<()> {
    let manager = open_manager(SC_MANAGER_CONNECT)?;
    let service = open_service(manager.0, SERVICE_STOP | SERVICE_QUERY_STATUS)?;
    stop_service(service.0)
}

/// Stop and delete the service registration. The protected executable remains
/// in the installation directory so package uninstall can own file removal.
pub fn uninstall() -> io::Result<()> {
    let manager = open_manager(SC_MANAGER_CONNECT)?;
    let service_name = wide_null(BROKER_SERVICE_NAME);
    let handle = unsafe {
        OpenServiceW(
            manager.0,
            service_name.as_ptr(),
            SERVICE_STOP | SERVICE_QUERY_STATUS | SERVICE_DELETE_ACCESS,
        )
    };
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        if error == ERROR_SERVICE_DOES_NOT_EXIST {
            return Ok(());
        }
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let service = OwnedServiceHandle(handle);
    stop_service(service.0)?;
    if unsafe { DeleteService(service.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Query SCM without opening the privileged service control surface.
pub fn state() -> io::Result<BrokerServiceState> {
    let manager = open_manager(SC_MANAGER_CONNECT)?;
    let service_name = wide_null(BROKER_SERVICE_NAME);
    let handle = unsafe { OpenServiceW(manager.0, service_name.as_ptr(), SERVICE_QUERY_STATUS) };
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        if error == ERROR_SERVICE_DOES_NOT_EXIST {
            return Ok(BrokerServiceState::NotInstalled);
        }
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let service = OwnedServiceHandle(handle);
    let status = query_service_status(service.0)?;
    Ok(match status.dwCurrentState {
        SERVICE_STOPPED => BrokerServiceState::Stopped,
        SERVICE_START_PENDING => BrokerServiceState::StartPending,
        SERVICE_RUNNING => BrokerServiceState::Running,
        SERVICE_STOP_PENDING => BrokerServiceState::StopPending,
        value => BrokerServiceState::Other(value),
    })
}

/// Print a compact service status for the broker's explicit CLI.
pub fn print_status() -> io::Result<()> {
    println!("{:#?}", state()?);
    Ok(())
}

/// Enter the SCM dispatcher. Only SCM may launch this mode.
pub fn run_service_dispatcher() -> io::Result<()> {
    let mut service_name = wide_null(BROKER_SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: service_name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Run the private synchronous HCS export helper mode. It accepts one bounded
/// request only after verifying the inherited broker service token.
pub fn run_hcs_export_worker() -> io::Result<()> {
    super::hcs_export_worker::run_worker_mode()
}

fn configure_service_identity(
    service: windows_sys::Win32::System::Services::SC_HANDLE,
) -> io::Result<()> {
    let sid_info = SERVICE_SID_INFO {
        dwServiceSidType: SERVICE_SID_TYPE_RESTRICTED,
    };
    if unsafe {
        ChangeServiceConfig2W(
            service,
            SERVICE_CONFIG_SERVICE_SID_INFO,
            (&sid_info as *const SERVICE_SID_INFO).cast(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut privileges = Vec::new();
    for privilege in REQUIRED_PRIVILEGES {
        privileges.extend(privilege.encode_utf16());
        privileges.push(0);
    }
    privileges.push(0);
    let privilege_info = SERVICE_REQUIRED_PRIVILEGES_INFOW {
        pmszRequiredPrivileges: privileges.as_mut_ptr(),
    };
    if unsafe {
        ChangeServiceConfig2W(
            service,
            SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
            (&privilege_info as *const SERVICE_REQUIRED_PRIVILEGES_INFOW).cast(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn stop_service(service: windows_sys::Win32::System::Services::SC_HANDLE) -> io::Result<()> {
    let status = query_service_status(service)?;
    if status.dwCurrentState == SERVICE_STOPPED {
        return Ok(());
    }
    if status.dwCurrentState != SERVICE_STOP_PENDING {
        let mut service_status = SERVICE_STATUS::default();
        if unsafe { ControlService(service, SERVICE_CONTROL_STOP, &mut service_status) } == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_SERVICE_NOT_ACTIVE {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
        }
    }
    wait_for_state(service, SERVICE_STOPPED, SERVICE_STOP_TIMEOUT)?;
    Ok(())
}

fn wait_for_state(
    service: windows_sys::Win32::System::Services::SC_HANDLE,
    expected: u32,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if query_service_status(service)?.dwCurrentState == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::from_raw_os_error(ERROR_TIMEOUT as i32));
        }
        sleep(Duration::from_millis(100));
    }
}

fn query_service_status(
    service: windows_sys::Win32::System::Services::SC_HANDLE,
) -> io::Result<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut returned = 0;
    if unsafe {
        QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut returned,
        )
    } == 0
        || returned < std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32
    {
        return Err(io::Error::last_os_error());
    }
    Ok(status)
}

fn open_manager(access: u32) -> io::Result<OwnedServiceHandle> {
    let handle = unsafe { OpenSCManagerW(null(), null(), access) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedServiceHandle(handle))
}

fn open_service(
    manager: windows_sys::Win32::System::Services::SC_HANDLE,
    access: u32,
) -> io::Result<OwnedServiceHandle> {
    let name = wide_null(BROKER_SERVICE_NAME);
    let handle = unsafe { OpenServiceW(manager, name.as_ptr(), access) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedServiceHandle(handle))
}

struct OwnedServiceHandle(windows_sys::Win32::System::Services::SC_HANDLE);

impl Drop for OwnedServiceHandle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

unsafe extern "system" fn service_main(_argument_count: u32, _arguments: *mut *mut u16) {
    if let Err(error) = run_service_main() {
        report_service_failure(error);
    }
}

fn run_service_main() -> io::Result<()> {
    let program_files = program_files_directory()?;
    validate_install_image(&std::env::current_exe()?, &program_files)?;
    let workspace_root = BrokerWorkspaceScratchRoot::open(&program_files)?;
    // Keep the catalog and all layer directory capabilities pinned for the
    // broker lifetime. An absent or malformed image catalog prevents startup.
    let _image_catalog = BrokerContainerImageCatalog::open(&program_files)?;

    let cancellation = BrokerPipeCancellation::new()?;
    let service_name = wide_null(BROKER_SERVICE_NAME);
    let status_handle = unsafe {
        RegisterServiceCtrlHandlerExW(service_name.as_ptr(), Some(service_control_handler), null())
    };
    if status_handle.is_null() {
        return Err(io::Error::last_os_error());
    }

    let start_result = {
        let _guard = lock_service_control();
        SERVICE_STATUS_HANDLE_VALUE.store(status_handle as isize, Ordering::SeqCst);
        SERVICE_CANCELLATION_EVENT.store(cancellation.raw_handle() as isize, Ordering::SeqCst);
        SERVICE_CONTROL_SIGNAL.store(CONTROL_SIGNAL_NONE, Ordering::SeqCst);
        report_status(status_handle, SERVICE_START_PENDING, 0, 5_000).and_then(|()| {
            report_status(
                status_handle,
                SERVICE_RUNNING,
                SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE,
                0,
            )
        })
    };
    if let Err(error) = start_result {
        let _guard = lock_service_control();
        report_service_failure_locked(status_handle, &error);
        clear_service_control_state();
        return Err(error);
    }

    let result = run_service_listener_loop(&cancellation, &workspace_root);

    {
        let _guard = lock_service_control();
        let status_result = match result {
            Ok(()) => report_status(status_handle, SERVICE_STOPPED, 0, 0),
            Err(ref error) => {
                report_service_failure_locked(status_handle, error);
                Ok(())
            }
        };
        clear_service_control_state();
        status_result?;
    }
    Ok(())
}

fn run_service_listener_loop(
    cancellation: &BrokerPipeCancellation,
    workspace_root: &BrokerWorkspaceScratchRoot,
) -> io::Result<()> {
    loop {
        match consume_control_signal(cancellation)? {
            CONTROL_SIGNAL_STOP => return Ok(()),
            CONTROL_SIGNAL_SESSION_CHANGE => continue,
            CONTROL_SIGNAL_NONE => {}
            _ => {
                return Err(invalid_data(
                    "broker service received an unknown control signal",
                ));
            }
        }

        let policy = match BrokerPeerPolicy::from_active_session() {
            Ok(policy) => policy,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::WouldBlock
                ) =>
            {
                wait_for_control_signal(cancellation)?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut listener = BrokerPipeListener::new(policy)?;

        loop {
            let outcome = match listener.accept(cancellation) {
                Ok(mut connection) => {
                    serve_authenticated_connection(&mut connection, workspace_root)
                }
                Err(error) => Err(error),
            };
            match outcome {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    match consume_control_signal(cancellation)? {
                        CONTROL_SIGNAL_STOP => return Ok(()),
                        CONTROL_SIGNAL_SESSION_CHANGE => break,
                        CONTROL_SIGNAL_NONE => return Err(error),
                        _ => {
                            return Err(invalid_data(
                                "broker service received an unknown control signal",
                            ));
                        }
                    }
                }
                Err(error) if is_peer_pipe_error(&error) => {}
                Err(error) => return Err(error),
            }
        }
        // Dropping the listener closes its old logon-scoped DACL before the
        // next WTS snapshot is read.
    }
}

fn serve_authenticated_connection(
    connection: &mut super::named_pipe::BrokerPipeConnection<'_>,
    workspace_root: &BrokerWorkspaceScratchRoot,
) -> io::Result<()> {
    let request_message = connection
        .read_frame(Duration::from_secs(5))?
        .decode_control();
    let request = match request_message {
        Ok(BrokerControlMessage::Execute { request }) => request,
        _ => {
            return write_broker_failure(connection, None, BrokerFailureCode::InvalidRequest);
        }
    };

    let request_id = request.request_id;
    let limits = match request.validate() {
        Ok(limits) => limits,
        Err(_) => {
            return write_broker_failure(
                connection,
                Some(request_id),
                BrokerFailureCode::InvalidRequest,
            );
        }
    };
    if let Err(code) = quota_runtime_attestation() {
        return write_broker_failure(connection, Some(request_id), code);
    }

    let mut attempt = match workspace_root.create_attempt() {
        Ok(attempt) => attempt,
        Err(_) => {
            return write_broker_failure(
                connection,
                Some(request_id),
                BrokerFailureCode::StagingFailed,
            );
        }
    };
    let deadline = Instant::now()
        .checked_add(limits.timeout())
        .ok_or_else(|| invalid_data("broker execution deadline is invalid"))?;
    let ready = BrokerControlMessage::WorkspaceSnapshotRequested { request_id };
    connection.write_frame_until(&BrokerFrame::control(&ready)?, deadline)?;

    let cancellation = connection.cancellation();
    let transfer = super::workspace_transfer::receive_workspace_snapshot(
        attempt.staging()?.clone(),
        limits.workspace_copy(),
        || connection.read_frame_until(deadline),
        || cancellation.is_cancelled(),
    );
    let (snapshot, _summary) = match transfer {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let cleanup_succeeded = attempt.cleanup().is_ok();
            let code = if !cleanup_succeeded {
                BrokerFailureCode::CleanupFailed
            } else {
                match error.kind() {
                    io::ErrorKind::Interrupted => BrokerFailureCode::Cancelled,
                    io::ErrorKind::TimedOut => BrokerFailureCode::TimedOut,
                    io::ErrorKind::InvalidData | io::ErrorKind::PermissionDenied => {
                        BrokerFailureCode::InvalidRequest
                    }
                    _ => BrokerFailureCode::StagingFailed,
                }
            };
            return write_broker_failure(connection, Some(request_id), code);
        }
    };
    drop(snapshot);
    let cleanup_succeeded = attempt.cleanup().is_ok();
    let code = if cleanup_succeeded {
        BrokerFailureCode::BackendUnavailable
    } else {
        BrokerFailureCode::CleanupFailed
    };
    write_broker_failure(connection, Some(request_id), code)
}

/// Quota enforcement has no runtime attestation yet. Keep this single gate
/// ahead of staging allocation and workspace bytes until the HCS provider can
/// prove the requested limit is enforced by the guest storage device.
fn quota_runtime_attestation() -> Result<(), BrokerFailureCode> {
    Err(BrokerFailureCode::QuotaUnattested)
}

fn write_broker_failure(
    connection: &mut super::named_pipe::BrokerPipeConnection<'_>,
    request_id: Option<[u8; 16]>,
    code: BrokerFailureCode,
) -> io::Result<()> {
    let response = BrokerControlMessage::Failure { request_id, code };
    connection.write_frame(&BrokerFrame::control(&response)?, Duration::from_secs(5))
}

fn is_peer_pipe_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied
            | io::ErrorKind::InvalidData
            | io::ErrorKind::TimedOut
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

fn wait_for_control_signal(cancellation: &BrokerPipeCancellation) -> io::Result<()> {
    if unsafe { WaitForSingleObject(cancellation.raw_handle(), u32::MAX) }
        != windows_sys::Win32::Foundation::WAIT_OBJECT_0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Consume a session-change notification and reset the shared event while the
/// handler lock is held. A concurrent stop always takes precedence.
fn consume_control_signal(cancellation: &BrokerPipeCancellation) -> io::Result<u32> {
    let _guard = lock_service_control();
    let signal = SERVICE_CONTROL_SIGNAL.load(Ordering::SeqCst);
    if signal == CONTROL_SIGNAL_SESSION_CHANGE {
        if unsafe { ResetEvent(cancellation.raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        SERVICE_CONTROL_SIGNAL.store(CONTROL_SIGNAL_NONE, Ordering::SeqCst);
    }
    Ok(signal)
}

fn clear_service_control_state() {
    SERVICE_STATUS_HANDLE_VALUE.store(0, Ordering::SeqCst);
    SERVICE_CANCELLATION_EVENT.store(0, Ordering::SeqCst);
    SERVICE_CONTROL_SIGNAL.store(CONTROL_SIGNAL_NONE, Ordering::SeqCst);
}

unsafe extern "system" fn service_control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut std::ffi::c_void,
    _context: *mut std::ffi::c_void,
) -> u32 {
    let _guard = lock_service_control();
    let status_handle = SERVICE_STATUS_HANDLE_VALUE.load(Ordering::SeqCst) as SERVICE_STATUS_HANDLE;
    match control {
        SERVICE_CONTROL_INTERROGATE => {
            if status_handle.is_null() {
                return ERROR_SERVICE_NOT_ACTIVE;
            }
            let state = SERVICE_CURRENT_STATE.load(Ordering::SeqCst);
            let (accepted, wait_hint) = if state == SERVICE_RUNNING {
                (
                    SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE,
                    0,
                )
            } else if state == SERVICE_STOP_PENDING {
                (0, 10_000)
            } else {
                (0, 5_000)
            };
            match report_status(status_handle, state, accepted, wait_hint) {
                Ok(()) => 0,
                Err(error) => error.raw_os_error().unwrap_or(1) as u32,
            }
        }
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            let cancellation_event = SERVICE_CANCELLATION_EVENT.load(Ordering::SeqCst) as HANDLE;
            if status_handle.is_null() || cancellation_event.is_null() {
                return ERROR_SERVICE_NOT_ACTIVE;
            }
            SERVICE_CONTROL_SIGNAL.store(CONTROL_SIGNAL_STOP, Ordering::SeqCst);
            if report_status(status_handle, SERVICE_STOP_PENDING, 0, 10_000).is_err() {
                return unsafe { GetLastError() };
            }
            if unsafe { SetEvent(cancellation_event) } == 0 {
                return unsafe { GetLastError() };
            }
            0
        }
        SERVICE_CONTROL_SESSIONCHANGE => {
            let cancellation_event = SERVICE_CANCELLATION_EVENT.load(Ordering::SeqCst) as HANDLE;
            if status_handle.is_null() || cancellation_event.is_null() {
                return ERROR_SERVICE_NOT_ACTIVE;
            }
            if SERVICE_CONTROL_SIGNAL.load(Ordering::SeqCst) != CONTROL_SIGNAL_STOP {
                SERVICE_CONTROL_SIGNAL.store(CONTROL_SIGNAL_SESSION_CHANGE, Ordering::SeqCst);
            }
            if unsafe { SetEvent(cancellation_event) } == 0 {
                return unsafe { GetLastError() };
            }
            0
        }
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

fn report_service_failure(error: io::Error) {
    let status_handle = SERVICE_STATUS_HANDLE_VALUE.load(Ordering::SeqCst) as SERVICE_STATUS_HANDLE;
    if status_handle.is_null() {
        return;
    }
    let _guard = lock_service_control();
    report_service_failure_locked(status_handle, &error);
}

fn report_service_failure_locked(status_handle: SERVICE_STATUS_HANDLE, error: &io::Error) {
    let code = error.raw_os_error().unwrap_or(1).unsigned_abs();
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: SERVICE_STOPPED,
        dwControlsAccepted: 0,
        dwWin32ExitCode: ERROR_SERVICE_SPECIFIC_ERROR,
        dwServiceSpecificExitCode: code,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    if unsafe { SetServiceStatus(status_handle, &status) } != 0 {
        SERVICE_CURRENT_STATE.store(SERVICE_STOPPED, Ordering::SeqCst);
    }
}

fn report_status(
    handle: SERVICE_STATUS_HANDLE,
    state: u32,
    accepted_controls: u32,
    wait_hint_ms: u32,
) -> io::Result<()> {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted_controls,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: wait_hint_ms,
    };
    if unsafe { SetServiceStatus(handle, &status) } == 0 {
        return Err(io::Error::last_os_error());
    }
    SERVICE_CURRENT_STATE.store(state, Ordering::SeqCst);
    Ok(())
}

fn lock_service_control() -> std::sync::MutexGuard<'static, ()> {
    SERVICE_CONTROL_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn program_files_directory() -> io::Result<PathBuf> {
    let mut path = null_mut();
    let result = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, null_mut(), &mut path) };
    if result < 0 || path.is_null() {
        return Err(io::Error::from_raw_os_error(result));
    }
    let _allocated = CoTaskMemString(path);
    let mut length = 0;
    while length < 32_768 && unsafe { *path.add(length) } != 0 {
        length += 1;
    }
    if length == 32_768 {
        return Err(invalid_data("Program Files path is too long"));
    }
    let value = unsafe { std::slice::from_raw_parts(path, length) };
    PathBuf::from(OsString::from_wide(value))
        .canonicalize()
        .map_err(|error| io::Error::new(error.kind(), "Program Files directory is unavailable"))
}

fn validate_install_image(executable: &Path, program_files: &Path) -> io::Result<PathBuf> {
    let expected_directory = program_files.join("Haven");
    let directory_metadata = std::fs::symlink_metadata(&expected_directory)
        .map_err(|error| io::Error::new(error.kind(), "protected Haven installation is missing"))?;
    if !directory_metadata.is_dir()
        || directory_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(permission_denied(
            "Haven installation directory is not a protected ordinary directory",
        ));
    }
    let directory = expected_directory
        .canonicalize()
        .map_err(|error| io::Error::new(error.kind(), "protected Haven installation is missing"))?;
    let metadata = std::fs::symlink_metadata(executable)?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(permission_denied(
            "broker image must be a regular non-reparse file",
        ));
    }
    let canonical = executable.canonicalize()?;
    let filename = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_input("broker image filename is invalid"))?;
    if !filename.eq_ignore_ascii_case(BROKER_BINARY_NAME)
        || !canonical
            .parent()
            .is_some_and(|parent| same_windows_path(parent, &directory))
    {
        return Err(permission_denied(
            "broker image must be installed directly under Program Files\\Haven",
        ));
    }
    Ok(canonical)
}

fn same_windows_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

struct CoTaskMemString(*mut u16);

impl Drop for CoTaskMemString {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(self.0.cast()) };
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn permission_denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_attestation_gate_fails_closed_until_runtime_proof_exists() {
        assert_eq!(
            quota_runtime_attestation().unwrap_err(),
            BrokerFailureCode::QuotaUnattested
        );
    }

    #[test]
    fn install_image_must_be_the_fixed_binary_under_haven_program_files() {
        let root = tempfile::tempdir().unwrap();
        let program_files = root.path().join("Program Files");
        let install = program_files.join("Haven");
        std::fs::create_dir_all(&install).unwrap();
        let executable = install.join(BROKER_BINARY_NAME);
        std::fs::write(&executable, b"broker image fixture").unwrap();

        assert_eq!(
            validate_install_image(&executable, &program_files).unwrap(),
            executable.canonicalize().unwrap()
        );
        let misplaced = root.path().join(BROKER_BINARY_NAME);
        std::fs::write(&misplaced, b"untrusted location").unwrap();
        assert_eq!(
            validate_install_image(&misplaced, &program_files)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
