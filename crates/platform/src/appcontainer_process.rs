//! Suspended Windows AppContainer process creation with an explicit stdio
//! handle list and an owner that keeps its workspace alive through tree exit.

use crate::filesystem::{
    DirectoryTreeChange, DirectoryTreeDiffLimits, DirectoryTreeFileCopySummary,
    DirectoryTreeOutputManifest,
};
use crate::process_containment::{ProcessContainment, process_spawn_lock};
use crate::windows_sandbox::AppContainerWorkspace;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

const PROCESS_TREE_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_WINDOWS_COMMAND_LINE_UNITS: usize = 32_766;
const MAX_WINDOWS_ENVIRONMENT_UNITS: usize = 32_766;

/// A deliberately small AppContainer command description. The environment is
/// empty unless explicitly supplied, the executable must be absolute, and the
/// working directory is always relative to the owned sandbox workspace.
#[derive(Debug)]
pub struct AppContainerCommand {
    program: OsString,
    args: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    current_dir: PathBuf,
    no_window: bool,
}

impl AppContainerCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            environment: Vec::new(),
            current_dir: PathBuf::new(),
            no_window: true,
        }
    }

    pub fn arg(mut self, argument: impl AsRef<OsStr>) -> Self {
        self.args.push(argument.as_ref().to_os_string());
        self
    }

    pub fn args(mut self, arguments: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Self {
        self.args.extend(
            arguments
                .into_iter()
                .map(|argument| argument.as_ref().to_os_string()),
        );
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.environment
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    pub fn envs(mut self, environment: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        self.environment.extend(environment);
        self
    }

    /// Select a working directory below the private workspace root. Parent
    /// traversal, rooted paths, and prefixes are rejected before process start.
    pub fn current_dir(mut self, relative_path: impl AsRef<Path>) -> Self {
        self.current_dir = relative_path.as_ref().to_path_buf();
        self
    }

    pub fn no_window(mut self, no_window: bool) -> Self {
        self.no_window = no_window;
        self
    }

    pub fn spawn(self, workspace: AppContainerWorkspace) -> io::Result<AppContainerChild> {
        self.validate()?;
        let cwd_path = resolve_workspace_cwd(workspace.root(), &self.current_dir)?;
        let command_line = windows_command_line(&self.program, &self.args)?;
        let mut command_line = wide_null(&command_line);
        let application_name = wide_null(&self.program);
        let cwd = wide_null(cwd_path.as_os_str());
        let environment = environment_block(self.environment, &cwd_path)?;
        let _spawn_guard = process_spawn_lock();
        let (stdin, stdout, stderr, child_handles) = create_stdio_pipes()?;
        let raw_child_handles = child_handles
            .each_ref()
            .map(|handle| handle.as_raw_handle() as HANDLE);

        let attribute_list = AttributeList::new(2)?;
        let mut security_capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: workspace.app_container_sid(),
            Capabilities: std::ptr::null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        // SAFETY: the initialized attribute list, workspace-owned SID, empty
        // capability array, and exact inherited handle array outlive process
        // creation. No ambient handles are added to the child allowlist.
        unsafe {
            attribute_list.update(
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                (&mut security_capabilities as *mut SECURITY_CAPABILITIES).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
            )?;
            attribute_list.update(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                raw_child_handles.as_ptr().cast_mut().cast(),
                std::mem::size_of_val(raw_child_handles.as_slice()),
            )?;
        }

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = raw_child_handles[0];
        startup.StartupInfo.hStdOutput = raw_child_handles[1];
        startup.StartupInfo.hStdError = raw_child_handles[2];
        startup.lpAttributeList = attribute_list.as_ptr();
        let mut process_information = PROCESS_INFORMATION::default();
        let mut creation_flags = CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT;
        creation_flags |= CREATE_UNICODE_ENVIRONMENT;
        if self.no_window {
            creation_flags |= CREATE_NO_WINDOW;
        }

        // SAFETY: every pointer references live writable UTF-16 storage or a
        // valid Windows structure. Inheritable handles are restricted by the
        // handle-list attribute and the process starts suspended.
        if unsafe {
            CreateProcessW(
                application_name.as_ptr(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                creation_flags,
                environment.as_ptr().cast(),
                cwd.as_ptr(),
                (&startup as *const STARTUPINFOEXW).cast(),
                &mut process_information,
            )
        } == 0
        {
            return Err(with_context(
                io::Error::last_os_error(),
                "Windows rejected AppContainer process creation",
            ));
        }
        let suspended = SuspendedProcess::new(process_information);
        drop(child_handles);
        drop(_spawn_guard);
        let containment = ProcessContainment::new()
            .map_err(|error| with_context(error, "could not create the process-tree Job Object"))?;
        let pid =
            unsafe { windows_sys::Win32::System::Threading::GetProcessId(suspended.process()) };
        if pid == 0 {
            return Err(io::Error::last_os_error());
        }
        containment
            .attach_and_resume(pid, suspended.process() as std::os::windows::io::RawHandle)
            .map_err(|error| with_context(error, "could not contain the AppContainer process"))?;
        let (process, thread) = suspended.into_resumed_handles();
        drop(thread);

        Ok(AppContainerChild {
            process,
            stdin: Some(file_from_owned_handle(stdin)?),
            stdout: Some(file_from_owned_handle(stdout)?),
            stderr: Some(file_from_owned_handle(stderr)?),
            containment,
            workspace: Some(workspace),
            tree_reaped: false,
        })
    }

    fn validate(&self) -> io::Result<()> {
        if !Path::new(&self.program).is_absolute() {
            return Err(invalid_input(
                "AppContainer executable path must be absolute",
            ));
        }
        if self.program.encode_wide().any(|unit| unit == 0)
            || self
                .args
                .iter()
                .any(|value| value.encode_wide().any(|unit| unit == 0))
        {
            return Err(invalid_input("process arguments cannot contain NUL"));
        }
        for (key, value) in &self.environment {
            let Some(key) = key.to_str() else {
                return Err(invalid_input("environment names must be ASCII identifiers"));
            };
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || value.encode_wide().any(|unit| unit == 0)
            {
                return Err(invalid_input("invalid process environment entry"));
            }
        }
        Ok(())
    }
}

/// Owns an AppContainer process, its three standard pipes, Job Object, and the
/// workspace whose package SID and files authorize that process.
pub struct AppContainerChild {
    process: OwnedHandle,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    containment: ProcessContainment,
    workspace: Option<AppContainerWorkspace>,
    tree_reaped: bool,
}

/// Owns one reaped sandbox workspace while a caller presents its bounded
/// output manifest for review. Dropping or cleaning up this owner removes the
/// private output and baseline directories.
pub struct AppContainerOutputReview {
    workspace: AppContainerWorkspace,
    manifest: DirectoryTreeOutputManifest,
}

impl AppContainerOutputReview {
    pub fn manifest(&self) -> &DirectoryTreeOutputManifest {
        &self.manifest
    }

    /// Conservative upper bound for the private file contents retained by
    /// this review: the initial snapshot, its immutable baseline, and the
    /// post-run bytes for every changed regular file. It intentionally
    /// overcounts modified files because their old bytes are included in the
    /// initial snapshot. Callers can use this value to reserve aggregate
    /// review storage before retaining the owner.
    pub fn retained_storage_upper_bound_bytes(&self) -> u64 {
        let changed_file_bytes = self
            .manifest
            .changes
            .iter()
            .filter_map(|change| change.after_bytes)
            .fold(0_u64, u64::saturating_add);
        self.workspace
            .snapshot_summary()
            .byte_count
            .saturating_mul(2)
            .saturating_add(changed_file_bytes)
    }

    /// Promote one manifest-created regular output file to a missing
    /// destination below an already-authorized directory capability. This
    /// does not authorize the destination: callers must hold the live policy
    /// lease and supply the workspace/policy stamps bound to this review.
    pub fn promote_output_file_to(
        &self,
        change: &DirectoryTreeChange,
        expected_execution_workspace_generation: u64,
        expected_policy_revision: u64,
        destination: &crate::filesystem::DirectoryCapability,
        destination_relative: &Path,
        is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<DirectoryTreeFileCopySummary> {
        if self.workspace.execution_workspace_generation()
            != expected_execution_workspace_generation
            || self.workspace.policy_revision() != expected_policy_revision
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "sandbox output is bound to a stale execution workspace or policy revision",
            ));
        }
        if !self.manifest.changes.iter().any(|entry| entry == change) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "sandbox output change is not part of this reviewed manifest",
            ));
        }
        self.workspace.directory().copy_reviewed_created_file_to(
            change,
            destination,
            destination_relative,
            is_cancelled,
        )
    }

    /// Explicitly discard this review's sandbox output and private baseline.
    pub fn cleanup(self) -> io::Result<()> {
        self.workspace.cleanup()
    }
}

impl AppContainerChild {
    pub fn id(&self) -> u32 {
        // SAFETY: the process handle remains valid for this owner's lifetime.
        unsafe {
            windows_sys::Win32::System::Threading::GetProcessId(
                self.process.as_raw_handle() as HANDLE
            )
        }
    }

    pub fn stdin(&mut self) -> Option<&mut File> {
        self.stdin.as_mut()
    }

    pub fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    /// Return a bounded output manifest only after the root process has exited
    /// and the complete Job Object tree has been reaped.
    pub fn output_manifest(
        &self,
        limits: DirectoryTreeDiffLimits,
        is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<DirectoryTreeOutputManifest> {
        if !self.tree_reaped {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "sandbox output is unavailable until the process tree exits",
            ));
        }
        self.workspace
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "sandbox output ownership has been transferred to a review",
                )
            })?
            .output_manifest(limits, is_cancelled)
    }

    /// Transfer a reaped private workspace into a review owner. The child
    /// handle can then be dropped while the output remains private and alive
    /// until the caller explicitly cleans up or drops the review.
    pub fn take_output_review(
        &mut self,
        limits: DirectoryTreeDiffLimits,
        is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<AppContainerOutputReview> {
        if !self.tree_reaped {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "sandbox output cannot be retained until the process tree exits",
            ));
        }
        let manifest = self.output_manifest(limits, is_cancelled)?;
        let workspace = self
            .workspace
            .take()
            .expect("a live AppContainer child retains its workspace");
        Ok(AppContainerOutputReview {
            workspace,
            manifest,
        })
    }

    /// Wait up to `timeout` for the root process. Once it exits, stop any
    /// descendants and wait for the Job Object to become empty before callers
    /// may consume or remove the sandbox workspace.
    pub fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<std::process::ExitStatus>> {
        let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: `process` is a valid owned process handle.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle() as HANDLE, milliseconds) } {
            WAIT_OBJECT_0 => {
                let mut exit_code = 0u32;
                // SAFETY: the process handle and output code are valid.
                if unsafe {
                    GetExitCodeProcess(self.process.as_raw_handle() as HANDLE, &mut exit_code)
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                self.containment
                    .terminate_and_wait(PROCESS_TREE_EXIT_TIMEOUT)?;
                self.tree_reaped = true;
                use std::os::windows::process::ExitStatusExt;
                Ok(Some(std::process::ExitStatus::from_raw(exit_code)))
            }
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("unexpected process wait result")),
        }
    }

    /// Stop the complete process tree and wait until the workspace is no
    /// longer held by any process in it.
    pub fn kill(&mut self) -> io::Result<()> {
        self.containment
            .terminate_and_wait(PROCESS_TREE_EXIT_TIMEOUT)?;
        self.tree_reaped = true;
        Ok(())
    }
}

impl Drop for AppContainerChild {
    fn drop(&mut self) {
        // Closing the parent pipe ends first also releases a child blocked on
        // standard I/O. Job termination is still the authority that stops the
        // whole tree before the private workspace is removed.
        self.stdin.take();
        self.stdout.take();
        self.stderr.take();
        if !self.tree_reaped {
            self.tree_reaped = self
                .containment
                .terminate_and_wait(PROCESS_TREE_EXIT_TIMEOUT)
                .is_ok();
        }
        if !self.tree_reaped
            && let Some(workspace) = self.workspace.take()
        {
            // If Windows could not confirm an empty Job Object, removing the
            // directory could race a surviving descendant. Keep the owner
            // alive for process lifetime; the stale-workspace sweeper handles
            // crash/abnormal leftovers in a later lifecycle slice.
            std::mem::forget(workspace);
        }
    }
}

fn resolve_workspace_cwd(root: &Path, relative: &Path) -> io::Result<PathBuf> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid_input(
            "working directory must stay below the sandbox root",
        ));
    }
    let mut cwd = root.to_path_buf();
    ensure_regular_workspace_directory(&cwd)?;
    for component in relative.components() {
        match component {
            Component::CurDir => continue,
            Component::Normal(part) => cwd.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(invalid_input(
                    "working directory must stay below the sandbox root",
                ));
            }
        }
        ensure_regular_workspace_directory(&cwd)?;
    }
    Ok(cwd)
}

fn ensure_regular_workspace_directory(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(invalid_input(
            "sandbox working directory contains a non-directory or reparse point",
        ));
    }
    Ok(())
}

fn environment_block(
    mut environment: Vec<(OsString, OsString)>,
    current_dir: &Path,
) -> io::Result<Vec<u16>> {
    let current_dir_units = current_dir.as_os_str().encode_wide().collect::<Vec<_>>();
    let drive_start = if current_dir_units.starts_with(&[
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
    ]) {
        4
    } else {
        0
    };
    if current_dir_units.len() < drive_start + 3
        || current_dir_units[drive_start + 1] != b':' as u16
        || !((b'A' as u16..=b'Z' as u16).contains(&current_dir_units[drive_start])
            || (b'a' as u16..=b'z' as u16).contains(&current_dir_units[drive_start]))
    {
        return Err(invalid_input(
            "AppContainer workspace must use a local drive path",
        ));
    }
    use std::os::windows::ffi::OsStringExt;
    let drive = OsString::from_wide(&current_dir_units[drive_start..drive_start + 2]);
    environment.push((
        OsString::from(format!("={}", drive.to_string_lossy())),
        current_dir.as_os_str().to_os_string(),
    ));
    let mut unique = BTreeMap::<String, (OsString, OsString)>::new();
    for (key, value) in environment {
        unique.insert(key.to_string_lossy().to_ascii_uppercase(), (key, value));
    }
    let mut block = Vec::new();
    for (key, value) in unique.into_values() {
        block.extend(key.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    if block.len() > MAX_WINDOWS_ENVIRONMENT_UNITS {
        return Err(invalid_input("process environment exceeds Windows limits"));
    }
    Ok(block)
}

fn windows_command_line(program: &OsStr, args: &[OsString]) -> io::Result<OsString> {
    let mut units = Vec::new();
    for argument in std::iter::once(program).chain(args.iter().map(OsString::as_os_str)) {
        if !units.is_empty() {
            units.push(b' ' as u16);
        }
        quote_windows_argument(argument, &mut units);
    }
    if units.len() >= MAX_WINDOWS_COMMAND_LINE_UNITS {
        return Err(invalid_input("process command line exceeds Windows limits"));
    }
    use std::os::windows::ffi::OsStringExt;
    Ok(OsString::from_wide(&units))
}

fn quote_windows_argument(argument: &OsStr, output: &mut Vec<u16>) {
    output.push(b'"' as u16);
    let mut slashes = 0usize;
    for unit in argument.encode_wide() {
        if unit == b'\\' as u16 {
            slashes += 1;
        } else if unit == b'"' as u16 {
            output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
            output.push(unit);
            slashes = 0;
        } else {
            output.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            output.push(unit);
            slashes = 0;
        }
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
}

fn create_stdio_pipes() -> io::Result<(OwnedHandle, OwnedHandle, OwnedHandle, [OwnedHandle; 3])> {
    let (stdin_read, stdin_write) = pipe()?;
    let (stdout_read, stdout_write) = pipe()?;
    let (stderr_read, stderr_write) = pipe()?;
    clear_inherit_flag(&stdin_write)?;
    clear_inherit_flag(&stdout_read)?;
    clear_inherit_flag(&stderr_read)?;
    let child_handles = [stdin_read, stdout_write, stderr_write];
    Ok((stdin_write, stdout_read, stderr_read, child_handles))
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: all output pointers are valid and the security structure remains
    // alive. Both handles are made inheritable for the exact-list spawn call.
    if unsafe { CreatePipe(&mut read, &mut write, &security, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe returned two distinct owned kernel handles.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

fn clear_inherit_flag(handle: &OwnedHandle) -> io::Result<()> {
    // SAFETY: the handle is owned and valid; clearing inheritance affects only
    // the parent-side endpoint, which is never passed to the child.
    if unsafe { SetHandleInformation(handle.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, 0) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn file_from_owned_handle(handle: OwnedHandle) -> io::Result<File> {
    // SAFETY: the owned anonymous-pipe handle is transferred to File, which
    // closes it exactly once.
    Ok(unsafe { File::from_raw_handle(handle.into_raw_handle()) })
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn with_context(error: io::Error, context: &str) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{context} ({})", error.raw_os_error().unwrap_or(0)),
    )
}

struct AttributeList {
    storage: Vec<usize>,
    initialized: bool,
}

impl AttributeList {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: the sizing call receives a null list and a valid output size.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size) };
        if size == 0 {
            return Err(io::Error::other(
                "Windows did not return an AppContainer attribute-list size",
            ));
        }
        let mut result = Self {
            storage: vec![0usize; size.div_ceil(std::mem::size_of::<usize>())],
            initialized: false,
        };
        // SAFETY: the vector is pointer-aligned and large enough for the
        // documented attribute list; it stays alive until CreateProcessW.
        if unsafe { InitializeProcThreadAttributeList(result.as_ptr(), count, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        result.initialized = true;
        Ok(result)
    }

    fn as_ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_ptr().cast_mut().cast()
    }

    unsafe fn update(
        &self,
        attribute: u32,
        value: *mut std::ffi::c_void,
        size: usize,
    ) -> io::Result<()> {
        // SAFETY: upheld by the caller, who keeps the value storage alive for
        // the subsequent CreateProcessW call.
        if unsafe {
            UpdateProcThreadAttribute(
                self.as_ptr(),
                0,
                attribute as usize,
                value,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: this list was initialized successfully and is deleted
            // before its backing allocation is dropped.
            unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
        }
    }
}

struct SuspendedProcess {
    process: Option<OwnedHandle>,
    thread: Option<OwnedHandle>,
    resumed: bool,
}

impl SuspendedProcess {
    fn new(info: PROCESS_INFORMATION) -> Self {
        // SAFETY: CreateProcessW returned these owned handles on success.
        Self {
            process: Some(unsafe { OwnedHandle::from_raw_handle(info.hProcess) }),
            thread: Some(unsafe { OwnedHandle::from_raw_handle(info.hThread) }),
            resumed: false,
        }
    }

    fn process(&self) -> HANDLE {
        self.process
            .as_ref()
            .expect("suspended process retains its process handle")
            .as_raw_handle() as HANDLE
    }

    fn into_resumed_handles(mut self) -> (OwnedHandle, OwnedHandle) {
        self.resumed = true;
        (
            self.process.take().expect("process handle is present"),
            self.thread.take().expect("thread handle is present"),
        )
    }
}

impl Drop for SuspendedProcess {
    fn drop(&mut self) {
        if !self.resumed
            && let Some(process) = &self.process
        {
            // SAFETY: this owner holds the exact suspended process returned by
            // CreateProcessW. Failure paths must not leave it running unowned.
            unsafe {
                TerminateProcess(process.as_raw_handle() as HANDLE, 1);
                WaitForSingleObject(process.as_raw_handle() as HANDLE, 5_000);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_rejects_ambient_program_paths_and_environment_block_uses_last_case_variant() {
        assert!(AppContainerCommand::new("cmd.exe").validate().is_err());

        let last_wins =
            AppContainerCommand::new(std::env::current_exe().expect("test executable path"))
                .env("PATH", "first")
                .env("Path", "second");
        assert!(last_wins.validate().is_ok());
        let block =
            environment_block(last_wins.environment, Path::new("C:\\HavenSandbox")).unwrap();
        let block = String::from_utf16_lossy(&block);
        assert!(block.contains("Path=second"));
        assert!(!block.contains("PATH=first"));
    }

    #[test]
    fn windows_argument_quoting_preserves_backslashes_quotes_and_unicode() {
        let argument = OsStr::new("D:\\数据\\name \"quoted\" end");
        let mut quoted = Vec::new();
        quote_windows_argument(argument, &mut quoted);
        use std::os::windows::ffi::OsStringExt;
        let quoted = OsString::from_wide(&quoted);
        assert_eq!(quoted.to_string_lossy(), r#""D:\数据\name \"quoted\" end""#);
    }

    #[test]
    fn environment_block_is_explicitly_empty_or_sorted_and_double_terminated() {
        let current_dir = Path::new("C:\\HavenSandbox");
        let empty = environment_block(Vec::new(), current_dir).unwrap();
        assert!(
            empty.starts_with(
                &"=C:=C:\\HavenSandbox\0\0"
                    .encode_utf16()
                    .collect::<Vec<_>>()
            )
        );
        let block = environment_block(
            vec![
                ("TEMP".into(), "C:\\Temp".into()),
                ("PATH".into(), "C:\\Windows".into()),
            ],
            current_dir,
        )
        .unwrap();
        assert!(
            block.starts_with(
                &"=C:=C:\\HavenSandbox\0PATH=C:\\Windows\0TEMP=C:\\Temp\0\0"
                    .encode_utf16()
                    .collect::<Vec<_>>()
            )
        );
    }

    #[test]
    fn cwd_rejects_parent_traversal_before_joining_the_workspace_root() {
        let root = Path::new("C:\\HavenSandbox");
        assert!(resolve_workspace_cwd(root, Path::new("..\\outside")).is_err());
        assert!(resolve_workspace_cwd(root, Path::new("C:\\outside")).is_err());
    }

    #[test]
    fn cwd_rejects_reparse_points_in_every_workspace_component() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sandbox");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(root.join("ordinary")).unwrap();
        std::fs::create_dir_all(outside.join("nested")).unwrap();
        assert_eq!(
            resolve_workspace_cwd(&root, Path::new("ordinary")).unwrap(),
            root.join("ordinary")
        );
        if std::os::windows::fs::symlink_dir(&outside, root.join("junction")).is_err() {
            // Some Windows test hosts do not grant symlink creation rights.
            return;
        }
        assert!(resolve_workspace_cwd(&root, Path::new("junction\\nested")).is_err());
    }
}
