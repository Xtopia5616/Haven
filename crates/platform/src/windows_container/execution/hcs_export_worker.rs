//! Process boundary for the synchronous HCS writable-layer export API.
//!
//! The broker sends one bounded, internally derived request to its own fixed
//! executable. A single-process Job Object owns the helper from its first
//! instruction through completion. Timeout or cancellation terminates the
//! helper; uncertain native export outcomes quarantine the attempt.

use super::broker_workspace::BrokerWorkspaceAttempt;
use crate::process_containment::{ProcessContainment, process_spawn_lock};
use crate::windows_container::hcs_layer_export::HcsLayerExportPlan;
use std::io::{self, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const WORKER_ARGUMENT: &str = "--hcs-export-worker";
const MAX_WORKER_TIMEOUT: Duration = super::MAX_EXECUTION_TIMEOUT;
const TERMINATION_TIMEOUT: Duration = Duration::from_secs(10);
const CHILD_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HcsExportWorkerFailureKind {
    InvalidRequest,
    ServiceIdentity,
    ContainmentUnavailable,
    SpawnFailed,
    RequestTransferFailed,
    WorkerFailed,
    TimedOut,
    Cancelled,
    ChildWaitFailed,
    RecoveryMarkerFailed,
}

/// Failure classification states whether attempt staging may be removed.
/// Callers must preserve the attempt when this is false.
#[derive(Debug)]
pub(crate) struct HcsExportWorkerFailure {
    kind: HcsExportWorkerFailureKind,
    cleanup_safe: bool,
}

impl HcsExportWorkerFailure {
    pub(crate) fn kind(&self) -> HcsExportWorkerFailureKind {
        self.kind
    }

    pub(crate) fn cleanup_safe(&self) -> bool {
        self.cleanup_safe
    }
}

/// Run one validated export plan in a fixed helper process. The closure is
/// polled while the native call is running and should include caller
/// disconnect and SCM cancellation state.
#[expect(
    dead_code,
    reason = "wired into broker execution after this worker slice"
)]
pub(super) fn export_stopped_layer(
    attempt: &mut BrokerWorkspaceAttempt,
    plan: &HcsLayerExportPlan,
    timeout: Duration,
    is_cancelled: impl FnMut() -> bool,
) -> Result<(), HcsExportWorkerFailure> {
    let result = export_stopped_layer_inner(plan, timeout, is_cancelled);
    if result.as_ref().is_err_and(|error| !error.cleanup_safe())
        && attempt.preserve_for_recovery().is_err()
    {
        return Err(failure(
            HcsExportWorkerFailureKind::RecoveryMarkerFailed,
            false,
        ));
    }
    result
}

fn export_stopped_layer_inner(
    plan: &HcsLayerExportPlan,
    timeout: Duration,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<(), HcsExportWorkerFailure> {
    if timeout.is_zero() || timeout > MAX_WORKER_TIMEOUT {
        return Err(failure(HcsExportWorkerFailureKind::InvalidRequest, true));
    }
    let request = plan
        .encode_worker_request()
        .map_err(|_| failure(HcsExportWorkerFailureKind::InvalidRequest, true))?;
    super::named_pipe::verify_current_broker_service_identity()
        .map_err(|_| failure(HcsExportWorkerFailureKind::ServiceIdentity, true))?;
    let deadline = Instant::now() + timeout;

    let containment = ProcessContainment::new_single_process()
        .map_err(|_| failure(HcsExportWorkerFailureKind::ContainmentUnavailable, true))?;
    let executable = std::env::current_exe()
        .map_err(|_| failure(HcsExportWorkerFailureKind::SpawnFailed, true))?;
    let mut command = Command::new(executable);
    command
        .arg(WORKER_ARGUMENT)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    containment.prepare_command(&mut command, 0);
    let spawn_result = {
        let _spawn_guard = process_spawn_lock();
        command.spawn()
    };
    let mut child =
        spawn_result.map_err(|_| failure(HcsExportWorkerFailureKind::SpawnFailed, true))?;
    let child_pid = child.id();
    use std::os::windows::io::AsRawHandle;
    if containment
        .attach_and_resume(child_pid, child.as_raw_handle())
        .is_err()
    {
        let _ = child.kill();
        let exited = wait_child_until(&mut child, Instant::now() + CHILD_EXIT_TIMEOUT);
        let tree_stopped = containment.terminate_and_wait(TERMINATION_TIMEOUT).is_ok();
        return Err(failure(
            HcsExportWorkerFailureKind::ContainmentUnavailable,
            exited && tree_stopped,
        ));
    }

    let Some(stdin) = child.stdin.take() else {
        let cleanup_safe = terminate_and_reap(&containment, &mut child);
        return Err(failure(
            HcsExportWorkerFailureKind::RequestTransferFailed,
            cleanup_safe,
        ));
    };
    let (writer_result_tx, writer_result_rx) = mpsc::sync_channel(1);
    if thread::Builder::new()
        .name("haven-hcs-export-request".to_owned())
        .spawn(move || write_worker_request(stdin, &request, writer_result_tx))
        .is_err()
    {
        let cleanup_safe = terminate_and_reap(&containment, &mut child);
        return Err(failure(
            HcsExportWorkerFailureKind::RequestTransferFailed,
            cleanup_safe,
        ));
    }

    loop {
        if is_cancelled() {
            let helper_stopped = terminate_and_reap(&containment, &mut child);
            if helper_stopped {
                let _ = writer_result_rx.recv_timeout(CHILD_EXIT_TIMEOUT);
            }
            // The public HCS contract does not say whether killing its caller
            // also stops any host-side export work; keep this attempt private
            // even after the local Job Object has drained.
            return Err(failure(HcsExportWorkerFailureKind::Cancelled, false));
        }
        if Instant::now() >= deadline {
            let helper_stopped = terminate_and_reap(&containment, &mut child);
            if helper_stopped {
                let _ = writer_result_rx.recv_timeout(CHILD_EXIT_TIMEOUT);
            }
            return Err(failure(HcsExportWorkerFailureKind::TimedOut, false));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // A helper that exits successfully must also leave an empty
                // Job Object. This kills and drains any unexpected descendant.
                let tree_stopped = containment.terminate_and_wait(TERMINATION_TIMEOUT).is_ok();
                let child_reaped = child.wait().is_ok();
                let writer_ok = matches!(
                    writer_result_rx.recv_timeout(CHILD_EXIT_TIMEOUT),
                    Ok(Ok(()))
                );
                let cleanup_safe = tree_stopped && child_reaped;
                if cleanup_safe && status.success() && writer_ok {
                    return Ok(());
                }
                return Err(failure(
                    if writer_ok {
                        HcsExportWorkerFailureKind::WorkerFailed
                    } else {
                        HcsExportWorkerFailureKind::RequestTransferFailed
                    },
                    cleanup_safe,
                ));
            }
            Ok(None) => {}
            Err(_) => {
                let helper_stopped = terminate_and_reap(&containment, &mut child);
                if helper_stopped {
                    let _ = writer_result_rx.recv_timeout(CHILD_EXIT_TIMEOUT);
                }
                return Err(failure(HcsExportWorkerFailureKind::ChildWaitFailed, false));
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Child entry called only after checking the inherited service token.
pub(crate) fn run_worker_mode() -> io::Result<()> {
    super::named_pipe::verify_current_broker_service_identity()?;
    let stdin = io::stdin();
    crate::windows_container::hcs_layer_export::run_native_worker(&mut stdin.lock())
}

fn write_worker_request(
    mut stdin: std::process::ChildStdin,
    request: &[u8],
    result: mpsc::SyncSender<io::Result<()>>,
) {
    let write_result = (|| {
        let length = u32::try_from(request.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "worker request is too large")
        })?;
        stdin.write_all(&length.to_le_bytes())?;
        stdin.write_all(request)?;
        stdin.flush()
    })();
    let _ = result.send(write_result);
}

fn terminate_and_reap(containment: &ProcessContainment, child: &mut Child) -> bool {
    let tree_stopped = containment.terminate_and_wait(TERMINATION_TIMEOUT).is_ok();
    if !tree_stopped {
        // Closing the kill-on-close Job Object is a second termination path.
        // The caller still receives cleanup_safe=false because the tree was
        // not observed empty before that close.
        let _ = child.kill();
        return false;
    }
    let child_stopped = wait_child_until(child, Instant::now() + CHILD_EXIT_TIMEOUT);
    tree_stopped && child_stopped
}

fn wait_child_until(child: &mut Child, deadline: Instant) -> bool {
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) | Err(_) => return false,
        }
    }
}

fn failure(kind: HcsExportWorkerFailureKind, cleanup_safe: bool) -> HcsExportWorkerFailure {
    HcsExportWorkerFailure { kind, cleanup_safe }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_cleanup_state_is_explicit() {
        let safe = failure(HcsExportWorkerFailureKind::WorkerFailed, true);
        let uncertain = failure(HcsExportWorkerFailureKind::TimedOut, false);

        assert_eq!(safe.kind(), HcsExportWorkerFailureKind::WorkerFailed);
        assert!(safe.cleanup_safe());
        assert!(!uncertain.cleanup_safe());
    }
}
