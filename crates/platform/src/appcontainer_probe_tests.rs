//! Windows-only proof that AppContainer can combine an explicitly granted
//! workspace directory with default-deny file and network access.

use std::ffi::{OsStr, OsString};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    ACL_REVISION, AddMandatoryAce, CONTAINER_INHERIT_ACE, CreateWellKnownSid,
    DACL_SECURITY_INFORMATION, FreeSid, InitializeAcl, LABEL_SECURITY_INFORMATION,
    OBJECT_INHERIT_ACE, SECURITY_CAPABILITIES, SECURITY_MAX_SID_SIZE, WinLowLabelSid,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
use windows_sys::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_NO_WRITE_UP;
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
    let workspace = tempfile::tempdir().expect("create isolated workspace");
    let outside_file = workspace
        .path()
        .parent()
        .expect("temp workspace has a parent")
        .join(format!(
            "haven-appcontainer-outside-{}.txt",
            unique_suffix()
        ));
    assert!(
        !outside_file.exists(),
        "unique outside fixture must not already exist"
    );
    std::fs::write(&outside_file, "outside-sentinel").expect("create outside sentinel");
    let _outside_cleanup = RemoveFileOnDrop(outside_file.clone());

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

    let profile = AppContainerProfile::create(&format!(
        "haven-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ))
    .expect("create unique AppContainer profile");
    let dacl_lease = grant_appcontainer_workspace_access(workspace.path(), profile.sid())
        .expect("grant the test AppContainer SID access to only the workspace");
    let integrity_lease = apply_low_integrity_label(workspace.path())
        .expect("allow low-integrity AppContainer writes within the workspace");

    let containment = ProcessContainment::new().expect("create process-tree Job Object");
    let mut process = spawn_appcontainer_child_probe(
        &containment,
        profile.sid(),
        workspace.path(),
        &outside_file,
        network_probe_port,
    )
    .expect("spawn a suspended AppContainer PowerShell process");

    let exit_code = wait_for_process(&mut process, APP_CONTAINER_TEST_TIMEOUT)
        .expect("wait for the AppContainer probe");
    let result_path = workspace.path().join("appcontainer-result.txt");
    let result = std::fs::read_to_string(&result_path).unwrap_or_default();
    let inside_path = workspace.path().join("inside-workspace.txt");
    let outside_content = std::fs::read_to_string(&outside_file).expect("read outside sentinel");
    let accepted_network_connection = match listener.accept() {
        Ok((_stream, _address)) => true,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("local network probe failed: {error}"),
    };

    drop(process);
    drop(containment);
    dacl_lease
        .restore()
        .expect("restore the test directory's original DACL");
    integrity_lease
        .restore()
        .expect("restore the test directory's original integrity label");
    profile
        .remove()
        .expect("delete the temporary AppContainer profile");

    assert_eq!(
        exit_code,
        0,
        "AppContainer probe process failed; result={result:?}, inside_file={}",
        inside_path.is_file()
    );
    assert!(
        inside_path.is_file(),
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

fn grant_appcontainer_workspace_access(
    path: &Path,
    app_container_sid: windows_sys::Win32::Security::PSID,
) -> io::Result<DirectoryDaclLease> {
    use windows_sys::Win32::Security::ACL;

    let path = wide_null(path.as_os_str());
    let mut original_dacl: *mut ACL = null_mut();
    let mut security_descriptor = null_mut();
    // SAFETY: `path` is nul-terminated and the outputs point to writable
    // storage. Windows allocates the descriptor, released by LocalFree.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut original_dacl,
            null_mut(),
            &mut security_descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if security_descriptor.is_null() || original_dacl.is_null() {
        // A null or absent DACL would grant overly broad access. Do not run an
        // AppContainer test against a directory whose ACL cannot be restored.
        if !security_descriptor.is_null() {
            // SAFETY: GetNamedSecurityInfoW returned this owned descriptor.
            unsafe { LocalFree(security_descriptor) };
        }
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing AppContainer test with an absent or null workspace DACL",
        ));
    }

    let mut lease = DirectoryDaclLease {
        path,
        original_dacl,
        security_descriptor,
        changed: false,
    };
    let trustee = TRUSTEE_W {
        pMultipleTrustee: null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_USER,
        ptstrName: app_container_sid.cast(),
    };
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
        Trustee: trustee,
    };
    let mut updated_dacl = null_mut();
    // SAFETY: the trustee SID and original ACL remain valid for the call.
    let acl_status = unsafe { SetEntriesInAclW(1, &access, original_dacl, &mut updated_dacl) };
    if acl_status != 0 {
        return Err(io::Error::from_raw_os_error(acl_status as i32));
    }
    // SAFETY: path and updated ACL are valid; Windows copies the ACL into the
    // directory security descriptor before the temporary ACL is freed.
    let set_status = unsafe {
        SetNamedSecurityInfoW(
            lease.path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            updated_dacl,
            null_mut(),
        )
    };
    // SAFETY: SetEntriesInAclW allocated the returned ACL with LocalAlloc.
    unsafe { LocalFree(updated_dacl.cast()) };
    if set_status != 0 {
        return Err(io::Error::from_raw_os_error(set_status as i32));
    }
    lease.changed = true;
    Ok(lease)
}

fn apply_low_integrity_label(path: &Path) -> io::Result<IntegrityLabelLease> {
    use windows_sys::Win32::Security::{ACL, PSECURITY_DESCRIPTOR};

    let path = wide_null(path.as_os_str());
    let mut original_sacl: *mut ACL = null_mut();
    let mut security_descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // LABEL_SECURITY_INFORMATION reads only the mandatory label and does not
    // require the audit-SACL privilege. The descriptor owns `original_sacl`.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut original_sacl,
            &mut security_descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }

    let mut lease = IntegrityLabelLease {
        path,
        original_sacl,
        security_descriptor,
        changed: false,
    };
    let mut low_label_sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut low_label_sid_size = low_label_sid.len() as u32;
    // SAFETY: the fixed SID buffer is large enough for a well-known SID and
    // the returned byte count is writable.
    if unsafe {
        CreateWellKnownSid(
            WinLowLabelSid,
            null_mut(),
            low_label_sid.as_mut_ptr().cast(),
            &mut low_label_sid_size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut label_acl_storage = vec![0u64; 128];
    let label_acl = label_acl_storage.as_mut_ptr().cast::<ACL>();
    // SAFETY: the aligned storage is larger than the ACL header plus low-label
    // ACE, and remains alive through SetNamedSecurityInfoW.
    if unsafe {
        InitializeAcl(
            label_acl,
            (label_acl_storage.len() * std::mem::size_of::<u64>()) as u32,
            ACL_REVISION,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the ACL is initialized and the low-label SID buffer remains
    // valid. Inheriting the label ensures files created in this workspace
    // receive a compatible low-integrity label.
    if unsafe {
        AddMandatoryAce(
            label_acl,
            ACL_REVISION,
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
            SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
            low_label_sid.as_mut_ptr().cast(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this changes only the mandatory label. The workspace is a
    // test-owned temp directory; the guard restores its prior label on every
    // return path.
    let status = unsafe {
        SetNamedSecurityInfoW(
            lease.path.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            label_acl,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    lease.changed = true;
    Ok(lease)
}

struct AppContainerProfile {
    name: Vec<u16>,
    sid: windows_sys::Win32::Security::PSID,
    active: bool,
}

impl AppContainerProfile {
    fn create(name: &str) -> io::Result<Self> {
        let mut name = name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let display_name = wide_null(OsStr::new("Haven AppContainer probe"));
        let description = wide_null(OsStr::new("Temporary integration test profile"));
        let mut sid = null_mut();
        // SAFETY: inputs are nul-terminated and the output SID pointer is
        // writable. The profile is deleted by the returned guard.
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_mut_ptr(),
                display_name.as_ptr(),
                description.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if result < 0 {
            if !sid.is_null() {
                // SAFETY: CreateAppContainerProfile allocated this SID.
                unsafe { FreeSid(sid) };
            }
            return Err(io::Error::from_raw_os_error(result));
        }
        if sid.is_null() {
            return Err(io::Error::other(
                "CreateAppContainerProfile succeeded without returning a SID",
            ));
        }
        Ok(Self {
            name,
            sid,
            active: true,
        })
    }

    fn sid(&self) -> windows_sys::Win32::Security::PSID {
        self.sid
    }

    fn remove(mut self) -> io::Result<()> {
        let result = self.delete_profile();
        // The SID came from CreateAppContainerProfile and must be released by
        // FreeSid even when profile deletion reports an error.
        if !self.sid.is_null() {
            // SAFETY: this is the owned AppContainer SID returned at creation.
            unsafe { FreeSid(self.sid) };
            self.sid = null_mut();
        }
        result
    }

    fn delete_profile(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        // SAFETY: the profile name is a valid NUL-terminated UTF-16 string.
        let result = unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        let _ = self.delete_profile();
        if !self.sid.is_null() {
            // SAFETY: this is the owned AppContainer SID returned at creation.
            unsafe { FreeSid(self.sid) };
        }
    }
}

struct DirectoryDaclLease {
    path: Vec<u16>,
    original_dacl: *mut windows_sys::Win32::Security::ACL,
    security_descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    changed: bool,
}

impl DirectoryDaclLease {
    fn restore(mut self) -> io::Result<()> {
        self.restore_inner()
    }

    fn restore_inner(&mut self) -> io::Result<()> {
        if self.changed {
            // SAFETY: the saved descriptor remains allocated and owns the
            // original ACL until this guard drops.
            let status = unsafe {
                SetNamedSecurityInfoW(
                    self.path.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    self.original_dacl,
                    null_mut(),
                )
            };
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            self.changed = false;
        }
        Ok(())
    }
}

impl Drop for DirectoryDaclLease {
    fn drop(&mut self) {
        let _ = self.restore_inner();
        if !self.security_descriptor.is_null() {
            // SAFETY: GetNamedSecurityInfoW allocated this descriptor.
            unsafe { LocalFree(self.security_descriptor) };
            self.security_descriptor = null_mut();
        }
    }
}

struct IntegrityLabelLease {
    path: Vec<u16>,
    original_sacl: *mut windows_sys::Win32::Security::ACL,
    security_descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    changed: bool,
}

impl IntegrityLabelLease {
    fn restore(mut self) -> io::Result<()> {
        self.restore_inner()
    }

    fn restore_inner(&mut self) -> io::Result<()> {
        if self.changed {
            // SAFETY: the original label pointer remains owned by the saved
            // security descriptor until this guard drops.
            let status = unsafe {
                SetNamedSecurityInfoW(
                    self.path.as_ptr(),
                    SE_FILE_OBJECT,
                    LABEL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    self.original_sacl,
                )
            };
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            self.changed = false;
        }
        Ok(())
    }
}

impl Drop for IntegrityLabelLease {
    fn drop(&mut self) {
        let _ = self.restore_inner();
        if !self.security_descriptor.is_null() {
            // SAFETY: GetNamedSecurityInfoW allocated this descriptor.
            unsafe { LocalFree(self.security_descriptor) };
            self.security_descriptor = null_mut();
        }
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

struct RemoveFileOnDrop(PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_nanos()
}
