//! Private, bounded ownership of one HCS compute-system handle.
//!
//! This lifecycle owner is not wired into the broker request path. The broker
//! quota attestation remains closed until a supported host proves the guest
//! storage ceiling, so this code only prepares handle lifecycle semantics.

use super::WindowsContainerSandboxConfig;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::time::Duration;
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE, LocalFree};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};

type RawOperation = *mut std::ffi::c_void;
type RawComputeSystem = *mut std::ffi::c_void;
type OperationCompletion = Option<unsafe extern "system" fn(RawOperation, *const std::ffi::c_void)>;

const MAX_OPERATION_WAIT: Duration = Duration::from_secs(5 * 60);
const TERMINATION_WAIT: Duration = Duration::from_secs(15);
const CANCEL_DRAIN_WAIT: Duration = Duration::from_secs(1);

/// Narrow API seam used by the lifecycle owner and its unit tests.
trait HcsApi {
    fn create_compute_system(
        &self,
        id: &str,
        configuration: &str,
        timeout: Duration,
    ) -> io::Result<RawComputeSystem>;

    fn start_compute_system(&self, system: RawComputeSystem, timeout: Duration) -> io::Result<()>;

    fn terminate_compute_system(&self, system: RawComputeSystem) -> io::Result<()>;

    fn close_compute_system(&self, system: RawComputeSystem);
}

/// Owns exactly one HCS system handle; dropping it requests termination and
/// closes the handle even if termination reports a failure.
struct HcsComputeSystemOwner<'a> {
    api: &'a dyn HcsApi,
    system: Option<RawComputeSystem>,
    started: bool,
}

impl<'a> HcsComputeSystemOwner<'a> {
    fn create(
        api: &'a dyn HcsApi,
        config: &WindowsContainerSandboxConfig,
        operation_timeout: Duration,
    ) -> io::Result<Self> {
        validate_operation_timeout(operation_timeout)?;
        let configuration = config.to_hcs_json()?;
        let system =
            api.create_compute_system(config.hcs_system_id(), &configuration, operation_timeout)?;
        if system.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HCS created a compute system without returning a handle",
            ));
        }
        Ok(Self {
            api,
            system: Some(system),
            started: false,
        })
    }

    fn start(&mut self, operation_timeout: Duration) -> io::Result<()> {
        validate_operation_timeout(operation_timeout)?;
        if self.started {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCS compute system has already been started",
            ));
        }
        let system = self.system.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCS compute system has already been terminated",
            )
        })?;
        self.api.start_compute_system(system, operation_timeout)?;
        self.started = true;
        Ok(())
    }

    /// Terminate once and release the handle only after HCS confirms success.
    /// A failure leaves the owner intact so callers can retry before drop.
    fn terminate(&mut self) -> io::Result<()> {
        let Some(system) = self.system else {
            return Ok(());
        };
        self.api.terminate_compute_system(system)?;
        self.api.close_compute_system(system);
        self.system = None;
        self.started = false;
        Ok(())
    }
}

impl Drop for HcsComputeSystemOwner<'_> {
    fn drop(&mut self) {
        if let Some(system) = self.system.take() {
            let _ = self.api.terminate_compute_system(system);
            self.api.close_compute_system(system);
        }
    }
}

fn validate_operation_timeout(timeout: Duration) -> io::Result<u32> {
    let millis = timeout.as_millis();
    if millis == 0 || millis > MAX_OPERATION_WAIT.as_millis() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HCS operation timeout is outside its bound",
        ));
    }
    u32::try_from(millis).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "HCS operation timeout cannot be represented",
        )
    })
}

struct NativeHcsApi {
    _module: OwnedModule,
    create_operation:
        unsafe extern "system" fn(*const std::ffi::c_void, OperationCompletion) -> RawOperation,
    close_operation: unsafe extern "system" fn(RawOperation),
    wait_for_operation_result: unsafe extern "system" fn(RawOperation, u32, *mut *mut u16) -> i32,
    cancel_operation: unsafe extern "system" fn(RawOperation) -> i32,
    create_compute_system: unsafe extern "system" fn(
        *const u16,
        *const u16,
        RawOperation,
        *const std::ffi::c_void,
        *mut RawComputeSystem,
    ) -> i32,
    start_compute_system:
        unsafe extern "system" fn(RawComputeSystem, RawOperation, *const u16) -> i32,
    terminate_compute_system:
        unsafe extern "system" fn(RawComputeSystem, RawOperation, *const u16) -> i32,
    close_compute_system: unsafe extern "system" fn(RawComputeSystem),
}

impl NativeHcsApi {
    fn load() -> io::Result<Self> {
        let library_name = wide_null(OsStr::new("computecore.dll"))?;
        let module = unsafe {
            LoadLibraryExW(
                library_name.as_ptr(),
                null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(io::Error::last_os_error());
        }
        let module = OwnedModule(module);

        // Keep HCS optional on hosts without the container runtime, and only
        // resolve functions from the system-directory copy of computecore.
        unsafe {
            Ok(Self {
                create_operation: resolve(module.0, b"HcsCreateOperation\0")?,
                close_operation: resolve(module.0, b"HcsCloseOperation\0")?,
                wait_for_operation_result: resolve(module.0, b"HcsWaitForOperationResult\0")?,
                cancel_operation: resolve(module.0, b"HcsCancelOperation\0")?,
                create_compute_system: resolve(module.0, b"HcsCreateComputeSystem\0")?,
                start_compute_system: resolve(module.0, b"HcsStartComputeSystem\0")?,
                terminate_compute_system: resolve(module.0, b"HcsTerminateComputeSystem\0")?,
                close_compute_system: resolve(module.0, b"HcsCloseComputeSystem\0")?,
                _module: module,
            })
        }
    }

    fn create_operation(&self) -> io::Result<OwnedOperation<'_>> {
        let operation = unsafe { (self.create_operation)(null(), None) };
        if operation.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedOperation {
            api: self,
            raw: operation,
        })
    }

    fn submit_and_wait(
        &self,
        operation: &OwnedOperation<'_>,
        timeout: Duration,
        submit: impl FnOnce(RawOperation) -> i32,
    ) -> io::Result<()> {
        let timeout_millis = validate_operation_timeout(timeout)?;
        let result = submit(operation.raw);
        if result < 0 {
            return Err(hresult_error("HCS operation submission failed", result));
        }

        if let Err(error) = self.wait_for_result(operation.raw, timeout_millis) {
            let _ = unsafe { (self.cancel_operation)(operation.raw) };
            let _ = self.wait_for_result(
                operation.raw,
                u32::try_from(CANCEL_DRAIN_WAIT.as_millis()).unwrap_or(1_000),
            );
            return Err(error);
        }
        Ok(())
    }

    fn wait_for_result(&self, operation: RawOperation, timeout_millis: u32) -> io::Result<()> {
        let mut result_document = null_mut();
        let result = unsafe {
            (self.wait_for_operation_result)(operation, timeout_millis, &mut result_document)
        };
        let _result_document = OwnedLocalString(result_document);
        if result < 0 {
            return Err(hresult_error(
                "HCS operation did not complete successfully",
                result,
            ));
        }
        Ok(())
    }
}

impl HcsApi for NativeHcsApi {
    fn create_compute_system(
        &self,
        id: &str,
        configuration: &str,
        timeout: Duration,
    ) -> io::Result<RawComputeSystem> {
        let id = wide_null(OsStr::new(id))?;
        let configuration = wide_null(OsStr::new(configuration))?;
        let operation = self.create_operation()?;
        let mut system = null_mut();
        let result = self.submit_and_wait(&operation, timeout, |raw_operation| unsafe {
            (self.create_compute_system)(
                id.as_ptr(),
                configuration.as_ptr(),
                raw_operation,
                null(),
                &mut system,
            )
        });
        drop(operation);
        if let Err(error) = result {
            if !system.is_null() {
                let _ = self.terminate_compute_system(system);
                self.close_compute_system(system);
            }
            return Err(error);
        }
        if system.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HCS completed compute-system creation without a handle",
            ));
        }
        Ok(system)
    }

    fn start_compute_system(&self, system: RawComputeSystem, timeout: Duration) -> io::Result<()> {
        let operation = self.create_operation()?;
        self.submit_and_wait(&operation, timeout, |raw_operation| unsafe {
            (self.start_compute_system)(system, raw_operation, null())
        })
    }

    fn terminate_compute_system(&self, system: RawComputeSystem) -> io::Result<()> {
        let operation = self.create_operation()?;
        self.submit_and_wait(&operation, TERMINATION_WAIT, |raw_operation| unsafe {
            (self.terminate_compute_system)(system, raw_operation, null())
        })
    }

    fn close_compute_system(&self, system: RawComputeSystem) {
        unsafe { (self.close_compute_system)(system) };
    }
}

struct OwnedOperation<'a> {
    api: &'a NativeHcsApi,
    raw: RawOperation,
}

impl Drop for OwnedOperation<'_> {
    fn drop(&mut self) {
        unsafe { (self.api.close_operation)(self.raw) };
    }
}

struct OwnedModule(HMODULE);

impl Drop for OwnedModule {
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.0) };
    }
}

struct OwnedLocalString(*mut u16);

impl Drop for OwnedLocalString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0.cast()) };
        }
    }
}

unsafe fn resolve<T: Copy>(module: HMODULE, name: &'static [u8]) -> io::Result<T> {
    let symbol = unsafe { GetProcAddress(module, name.as_ptr()) }.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "required HCS function is unavailable",
        )
    })?;
    if std::mem::size_of::<T>() != std::mem::size_of_val(&symbol) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HCS function pointer has an unexpected representation",
        ));
    }
    Ok(unsafe { std::mem::transmute_copy(&symbol) })
}

fn wide_null(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = value.encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HCS input contains a null character",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn hresult_error(message: &str, result: i32) -> io::Error {
    io::Error::other(format!("{message} (HRESULT 0x{:08X})", result as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockHcsApi {
        calls: Mutex<Vec<&'static str>>,
        fail_start: bool,
        fail_first_termination: Mutex<bool>,
    }

    impl MockHcsApi {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_start: false,
                fail_first_termination: Mutex::new(false),
            }
        }

        fn record(&self, call: &'static str) {
            self.calls
                .lock()
                .expect("mock call log is available")
                .push(call);
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
            self.record("create");
            Ok(1_usize as RawComputeSystem)
        }

        fn start_compute_system(
            &self,
            system: RawComputeSystem,
            _timeout: Duration,
        ) -> io::Result<()> {
            assert_eq!(system, 1_usize as RawComputeSystem);
            self.record("start");
            if self.fail_start {
                return Err(io::Error::other("mock start failure"));
            }
            Ok(())
        }

        fn terminate_compute_system(&self, system: RawComputeSystem) -> io::Result<()> {
            assert_eq!(system, 1_usize as RawComputeSystem);
            self.record("terminate");
            let mut fail_first = self
                .fail_first_termination
                .lock()
                .expect("mock termination state is available");
            if *fail_first {
                *fail_first = false;
                return Err(io::Error::other("mock termination failure"));
            }
            Ok(())
        }

        fn close_compute_system(&self, system: RawComputeSystem) {
            assert_eq!(system, 1_usize as RawComputeSystem);
            self.record("close");
        }
    }

    fn config() -> WindowsContainerSandboxConfig {
        WindowsContainerSandboxConfig::new(
            "HavenSandbox-0123456789abcdef0123456789abcdef",
            1024 * 1024 * 1024,
            vec![
                super::super::WindowsContainerLayer::new(
                    "trusted-base-layer",
                    r"C:\Haven\images\base",
                )
                .expect("test layer is valid"),
            ],
        )
        .expect("test config is valid")
    }

    #[test]
    fn owner_starts_terminates_and_closes_in_order() {
        let api = MockHcsApi::new();
        let mut owner = HcsComputeSystemOwner::create(&api, &config(), Duration::from_secs(30))
            .expect("closed config creates an owned system");
        owner
            .start(Duration::from_secs(30))
            .expect("created system starts once");
        owner.terminate().expect("system terminates");
        drop(owner);

        assert_eq!(
            *api.calls.lock().unwrap(),
            ["create", "start", "terminate", "close"]
        );
    }

    #[test]
    fn start_failure_drop_terminates_and_closes_the_created_system() {
        let api = MockHcsApi {
            fail_start: true,
            ..MockHcsApi::new()
        };
        let mut owner = HcsComputeSystemOwner::create(&api, &config(), Duration::from_secs(30))
            .expect("closed config creates an owned system");
        assert!(owner.start(Duration::from_secs(30)).is_err());
        drop(owner);

        assert_eq!(
            *api.calls.lock().unwrap(),
            ["create", "start", "terminate", "close"]
        );
    }

    #[test]
    fn failed_explicit_termination_retains_handle_for_retry() {
        let api = MockHcsApi {
            fail_first_termination: Mutex::new(true),
            ..MockHcsApi::new()
        };
        let mut owner = HcsComputeSystemOwner::create(&api, &config(), Duration::from_secs(30))
            .expect("closed config creates an owned system");
        assert!(owner.terminate().is_err());
        owner.terminate().expect("retry releases the system");
        drop(owner);

        assert_eq!(
            *api.calls.lock().unwrap(),
            ["create", "terminate", "terminate", "close"]
        );
    }

    #[test]
    fn operation_timeouts_must_be_positive_and_bounded() {
        assert!(validate_operation_timeout(Duration::ZERO).is_err());
        assert!(validate_operation_timeout(MAX_OPERATION_WAIT + Duration::from_millis(1)).is_err());
        assert_eq!(
            validate_operation_timeout(Duration::from_secs(30)).unwrap(),
            30_000
        );
    }
}
