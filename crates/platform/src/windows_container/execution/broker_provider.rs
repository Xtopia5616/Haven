//! Windows implementation of the typed container execution port.

use super::named_pipe::{
    BrokerClientPolicy, BrokerPipeCancellation, BrokerPipeClient, BrokerPipeExecutionResponse,
};
use super::{
    WindowsContainerExecutionCallbacks, WindowsContainerExecutionPort,
    WindowsContainerExecutionRequest, WindowsContainerExecutionResult, WindowsContainerOutputSink,
    WindowsContainerProcessStartCallbacks, WindowsContainerSkillBundleInput,
    WindowsContainerSkillExecutionRequest, WindowsContainerWorkspaceInput, private,
};
use crate::windows_container::execution::broker_protocol::BrokerFailureCode;
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(20);
static ACTIVE_EXECUTION: Mutex<()> = Mutex::new(());

pub(super) struct BrokerExecutionProvider;

impl private::Sealed for BrokerExecutionProvider {}

impl WindowsContainerExecutionPort for BrokerExecutionProvider {
    fn execute(
        &self,
        request: &WindowsContainerExecutionRequest,
        workspace: &WindowsContainerWorkspaceInput,
        callbacks: &mut WindowsContainerExecutionCallbacks<'_>,
    ) -> io::Result<WindowsContainerExecutionResult> {
        let WindowsContainerExecutionCallbacks {
            before_process_start,
            process_started,
            is_cancelled,
            on_output,
        } = callbacks;
        if is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "container execution was cancelled",
            ));
        }
        let _execution_slot = acquire_execution_slot()?;
        let cancellation = BrokerPipeCancellation::new()?;
        with_cancellation_watcher(is_cancelled, cancellation.clone(), || {
            let policy = BrokerClientPolicy::for_current_process().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "broker client identity is invalid",
                )
            })?;
            let mut client =
                BrokerPipeClient::connect(&policy, cancellation).map_err(map_connection_error)?;
            let request_id = uuid::Uuid::new_v4().into_bytes();
            let output = WindowsContainerOutputSink::new(request, on_output);
            let mut start_callbacks =
                WindowsContainerProcessStartCallbacks::new(before_process_start, process_started);
            match client
                .execute_with_workspace(
                    request,
                    request_id,
                    workspace,
                    &mut start_callbacks,
                    output,
                )
                .map_err(map_connection_error)?
            {
                BrokerPipeExecutionResponse::Failed(code) => Err(map_broker_failure(code)),
                BrokerPipeExecutionResponse::Completed {
                    exit_code,
                    captured_output,
                    output_review,
                } => Ok(WindowsContainerExecutionResult::new(
                    exit_code,
                    captured_output,
                    output_review,
                )),
            }
        })
    }

    fn execute_skill(
        &self,
        request: &WindowsContainerSkillExecutionRequest,
        workspace: &WindowsContainerWorkspaceInput,
        skill_bundle: &WindowsContainerSkillBundleInput,
        callbacks: &mut WindowsContainerExecutionCallbacks<'_>,
    ) -> io::Result<WindowsContainerExecutionResult> {
        let WindowsContainerExecutionCallbacks {
            before_process_start,
            process_started,
            is_cancelled,
            on_output,
        } = callbacks;
        if is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "container Skill execution was cancelled",
            ));
        }
        let _execution_slot = acquire_execution_slot()?;
        let cancellation = BrokerPipeCancellation::new()?;
        with_cancellation_watcher(is_cancelled, cancellation.clone(), || {
            let policy = BrokerClientPolicy::for_current_process().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "broker client identity is invalid",
                )
            })?;
            let mut client =
                BrokerPipeClient::connect(&policy, cancellation).map_err(map_connection_error)?;
            let request_id = uuid::Uuid::new_v4().into_bytes();
            let output = WindowsContainerOutputSink::new_with_limits(request.limits(), on_output);
            let mut start_callbacks =
                WindowsContainerProcessStartCallbacks::new(before_process_start, process_started);
            match client
                .execute_with_skill(
                    request,
                    request_id,
                    workspace,
                    skill_bundle,
                    &mut start_callbacks,
                    output,
                )
                .map_err(map_connection_error)?
            {
                BrokerPipeExecutionResponse::Failed(code) => Err(map_broker_failure(code)),
                BrokerPipeExecutionResponse::Completed {
                    exit_code,
                    captured_output,
                    output_review,
                } => Ok(WindowsContainerExecutionResult::new(
                    exit_code,
                    captured_output,
                    output_review,
                )),
            }
        })
    }
}

/// Monitor the caller's cancellation token while named-pipe I/O waits. Setting
/// the event interrupts client I/O; dropping that connection makes the broker
/// stop the guest through its peer-disconnect cancellation path.
fn with_cancellation_watcher<T>(
    is_cancelled: &mut (dyn FnMut() -> bool + Send),
    cancellation: BrokerPipeCancellation,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let watcher_cancellation = cancellation.clone();
        let stop_ref = &stop;
        let cancellation_check = &mut *is_cancelled;
        let watcher = scope.spawn(move || {
            while !stop_ref.load(Ordering::Acquire) {
                if cancellation_check() {
                    let _ = watcher_cancellation.cancel();
                    break;
                }
                thread::sleep(CANCELLATION_POLL_INTERVAL);
            }
        });

        struct StopOnDrop<'a>(&'a AtomicBool);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let stop_guard = StopOnDrop(&stop);
        let result = operation();
        drop(stop_guard);
        watcher
            .join()
            .map_err(|_| io::Error::other("broker cancellation monitor failed"))?;
        result
    })
}

fn acquire_execution_slot() -> io::Result<std::sync::MutexGuard<'static, ()>> {
    match ACTIVE_EXECUTION.try_lock() {
        Ok(guard) => Ok(guard),
        Err(std::sync::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "another container execution is active",
        )),
        Err(std::sync::TryLockError::Poisoned(_)) => Err(io::Error::other(
            "container execution provider is unavailable",
        )),
    }
}

fn map_connection_error(error: io::Error) -> io::Error {
    let kind = match error.kind() {
        io::ErrorKind::Interrupted => io::ErrorKind::Interrupted,
        io::ErrorKind::TimedOut => io::ErrorKind::TimedOut,
        io::ErrorKind::PermissionDenied => io::ErrorKind::PermissionDenied,
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => error.kind(),
        _ => io::ErrorKind::NotConnected,
    };
    io::Error::new(kind, "container broker request failed")
}

fn map_broker_failure(code: BrokerFailureCode) -> io::Error {
    let (kind, message) = match code {
        BrokerFailureCode::UnsupportedProtocolVersion => (
            io::ErrorKind::Unsupported,
            "container broker protocol version is unsupported",
        ),
        BrokerFailureCode::BackendUnavailable => (
            io::ErrorKind::NotConnected,
            "container broker backend is unavailable",
        ),
        BrokerFailureCode::UnauthorizedPeer => (
            io::ErrorKind::PermissionDenied,
            "container broker rejected the local peer",
        ),
        BrokerFailureCode::InvalidRequest => (
            io::ErrorKind::InvalidInput,
            "container broker rejected the execution request",
        ),
        BrokerFailureCode::PolicyRevoked | BrokerFailureCode::Cancelled => (
            io::ErrorKind::Interrupted,
            "container execution was cancelled",
        ),
        BrokerFailureCode::TimedOut => (io::ErrorKind::TimedOut, "container execution timed out"),
        BrokerFailureCode::QuotaUnattested => (
            io::ErrorKind::PermissionDenied,
            "container storage quota is not verified",
        ),
        BrokerFailureCode::SkillRuntimeUnattested => (
            io::ErrorKind::PermissionDenied,
            "container Skill Python runtime is not verified",
        ),
        BrokerFailureCode::McpPackageUnattested => (
            io::ErrorKind::PermissionDenied,
            "container MCP package runtime is not verified",
        ),
        BrokerFailureCode::StagingFailed => {
            (io::ErrorKind::Other, "container workspace staging failed")
        }
        BrokerFailureCode::HcsFailed => (io::ErrorKind::Other, "container execution failed"),
        BrokerFailureCode::CleanupFailed => {
            (io::ErrorKind::Other, "container attempt cleanup failed")
        }
    };
    io::Error::new(kind, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn caller_cancellation_interrupts_waiting_broker_io() {
        let caller_cancelled = Arc::new(AtomicBool::new(false));
        let cancel_signal = Arc::clone(&caller_cancelled);
        let cancellation = BrokerPipeCancellation::new().unwrap();
        let operation_cancellation = cancellation.clone();
        let cancel_task = thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            cancel_signal.store(true, Ordering::Release);
        });
        let callback_cancelled = Arc::clone(&caller_cancelled);
        let mut is_cancelled = move || callback_cancelled.load(Ordering::Acquire);

        let result: io::Result<()> =
            with_cancellation_watcher(&mut is_cancelled, cancellation, || {
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while !operation_cancellation.is_cancelled() {
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "watchdog test timed out",
                        ));
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "broker I/O was interrupted",
                ))
            });
        cancel_task.join().unwrap();

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn broker_failure_mapping_keeps_quota_gate_fail_closed() {
        let error = map_broker_failure(BrokerFailureCode::QuotaUnattested);
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("not verified"));

        let error = map_broker_failure(BrokerFailureCode::McpPackageUnattested);
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("MCP package"));
    }

    #[test]
    fn transport_error_mapping_does_not_expose_os_paths() {
        let error = map_connection_error(io::Error::new(
            io::ErrorKind::PermissionDenied,
            r"C:\Users\private\broker.exe",
        ));
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "container broker request failed");
    }

    #[test]
    fn provider_allows_only_one_active_broker_attempt() {
        let _active = acquire_execution_slot().unwrap();
        assert_eq!(
            acquire_execution_slot().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
