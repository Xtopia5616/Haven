//! Broker-side owner for one received HCS execution attempt.

use super::broker_protocol::{
    BrokerControlMessage, BrokerExecutionRequest, BrokerFailureCode, BrokerFrame, BrokerFrameKind,
    BrokerShell,
};
use super::broker_workspace::BrokerWorkspaceAttempt;
use super::hcs_export_worker::export_stopped_workspace_output;
use super::image_catalog::BrokerContainerImageCatalog;
use super::named_pipe::BrokerPipeConnection;
use super::output_transfer::send_output_snapshot;
use crate::filesystem::DirectoryCapability;
use crate::windows_container::execution::{
    WindowsContainerExecutionLimits, WindowsContainerExecutionRequest, WindowsContainerOutputSink,
    WindowsContainerOutputStream, WindowsContainerShell,
};
use crate::windows_container::hcs_guest_execution::{
    HcsGuestExecutionFailureKind, execute_guest, native_api,
};
use std::cell::Cell;
use std::io;
use std::path::PathBuf;
use std::time::Instant;

const MAX_EXPORT_TRANSPORT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

enum AttemptFailure {
    Safe(BrokerFailureCode),
    Quarantined(BrokerFailureCode),
}

pub(super) fn serve_received_execution(
    connection: &BrokerPipeConnection<'_>,
    attempt: &mut BrokerWorkspaceAttempt,
    image_catalog: &BrokerContainerImageCatalog,
    wire_request: BrokerExecutionRequest,
    limits: WindowsContainerExecutionLimits,
    initial_snapshot_bytes: u64,
    deadline: Instant,
) -> io::Result<()> {
    let request_id = wire_request.request_id;
    if initial_snapshot_bytes > limits.storage_quota_bytes()
        || initial_snapshot_bytes > limits.output_diff().scan().max_total_bytes()
    {
        return fail_and_cleanup(
            connection,
            attempt,
            request_id,
            BrokerFailureCode::InvalidRequest,
            deadline,
        );
    }

    let request = match platform_request(wire_request, limits, deadline) {
        Ok(request) => request,
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            return fail_and_cleanup(
                connection,
                attempt,
                request_id,
                BrokerFailureCode::TimedOut,
                deadline,
            );
        }
        Err(_) => {
            return fail_and_cleanup(
                connection,
                attempt,
                request_id,
                BrokerFailureCode::InvalidRequest,
                deadline,
            );
        }
    };

    let (exit_code, captured_output, output) = match run_hcs_attempt(
        connection,
        attempt,
        image_catalog,
        request_id,
        &request,
        deadline,
    ) {
        Ok(result) => result,
        Err(AttemptFailure::Safe(code)) => {
            return fail_and_cleanup(connection, attempt, request_id, code, deadline);
        }
        Err(AttemptFailure::Quarantined(code)) => {
            return write_failure(connection, request_id, code, deadline);
        }
    };

    let cancellation = connection.cancellation();
    let transfer_result = send_output_snapshot(
        &output,
        attempt.staging()?,
        request.limits().output_diff(),
        initial_snapshot_bytes,
        |frame| connection.write_frame_until(&frame, deadline),
        || {
            Instant::now() >= deadline
                || cancellation.is_cancelled()
                || connection.peer_disconnected()
        },
    );
    drop(output);
    if let Err(error) = transfer_result {
        return fail_and_cleanup(
            connection,
            attempt,
            request_id,
            if Instant::now() >= deadline {
                BrokerFailureCode::TimedOut
            } else {
                failure_code_for_io(&error)
            },
            deadline,
        );
    }

    if attempt.cleanup().is_err() {
        return write_failure(
            connection,
            request_id,
            BrokerFailureCode::CleanupFailed,
            deadline,
        );
    }
    write_execution_complete(
        connection,
        request_id,
        exit_code,
        captured_output.stdout_truncated(),
        captured_output.stderr_truncated(),
        deadline,
    )
}

fn run_hcs_attempt(
    connection: &BrokerPipeConnection<'_>,
    attempt: &mut BrokerWorkspaceAttempt,
    image_catalog: &BrokerContainerImageCatalog,
    request_id: [u8; 16],
    request: &WindowsContainerExecutionRequest,
    deadline: Instant,
) -> Result<
    (
        u32,
        crate::windows_container::execution::WindowsContainerCapturedOutput,
        DirectoryCapability,
    ),
    AttemptFailure,
> {
    if attempt.mark_hcs_operation_active().is_err() {
        return Err(quarantine_attempt(
            attempt,
            None,
            BrokerFailureCode::CleanupFailed,
        ));
    }

    let attempt_root = match attempt.attempt_root() {
        Ok(root) => root.clone(),
        Err(_) => {
            return Err(quiescent_failure(
                attempt,
                None,
                BrokerFailureCode::StagingFailed,
            ));
        }
    };
    let mut scratch = match crate::windows_container::scratch_vhd::BoundedScratchVhd::create(
        attempt_root,
        request.limits().storage_quota_bytes(),
    ) {
        Ok(scratch) => scratch,
        Err(_) => {
            return Err(quiescent_failure(
                attempt,
                None,
                BrokerFailureCode::StagingFailed,
            ));
        }
    };
    if scratch.format_for_container().is_err() {
        return Err(quiescent_failure(
            attempt,
            Some(&mut scratch),
            BrokerFailureCode::HcsFailed,
        ));
    }

    let mut config = match image_catalog.sandbox_config(
        hcs_system_name(request_id),
        request.limits().storage_quota_bytes(),
    ) {
        Ok(config) => config,
        Err(_) => {
            return Err(quiescent_failure(
                attempt,
                Some(&mut scratch),
                BrokerFailureCode::HcsFailed,
            ));
        }
    };
    config = match config
        .with_bounded_writable_layer(&scratch)
        .and_then(|config| config.with_read_only_workspace_input(attempt.staging()?.clone()))
    {
        Ok(config) => config,
        Err(_) => {
            return Err(quiescent_failure(
                attempt,
                Some(&mut scratch),
                BrokerFailureCode::StagingFailed,
            ));
        }
    };

    let api = match native_api() {
        Ok(api) => api,
        Err(_) => {
            return Err(quiescent_failure(
                attempt,
                Some(&mut scratch),
                BrokerFailureCode::BackendUnavailable,
            ));
        }
    };

    let cancellation = connection.cancellation();
    let output_write_failed = Cell::new(false);
    let mut on_output = |stream: WindowsContainerOutputStream, bytes: &[u8]| {
        if output_write_failed.get() {
            return;
        }
        let kind = match stream {
            WindowsContainerOutputStream::Stdout => BrokerFrameKind::StdoutChunk,
            WindowsContainerOutputStream::Stderr => BrokerFrameKind::StderrChunk,
        };
        let result = BrokerFrame::new(kind, bytes.to_vec())
            .and_then(|frame| connection.write_frame_until(&frame, deadline));
        if result.is_err() {
            output_write_failed.set(true);
        }
    };
    let output_sink = WindowsContainerOutputSink::new(request, &mut on_output);
    let mut is_cancelled = || {
        Instant::now() >= deadline
            || cancellation.is_cancelled()
            || connection.peer_disconnected()
            || output_write_failed.get()
    };
    let guest_result = execute_guest(&api, &config, request, &mut is_cancelled, output_sink);

    let outcome = match guest_result {
        Ok(outcome) => outcome,
        Err(error) if error.cleanup_safe => {
            return Err(quiescent_failure(
                attempt,
                Some(&mut scratch),
                if Instant::now() >= deadline {
                    BrokerFailureCode::TimedOut
                } else {
                    failure_code_for_guest(error.kind)
                },
            ));
        }
        Err(error) => {
            return Err(quarantine_attempt(
                attempt,
                Some(&mut scratch),
                failure_code_for_guest(error.kind),
            ));
        }
    };

    let export_timeout = match deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
    {
        Some(timeout) => timeout,
        None => {
            return Err(quiescent_failure(
                attempt,
                Some(&mut scratch),
                BrokerFailureCode::TimedOut,
            ));
        }
    };
    let output = match export_stopped_workspace_output(
        attempt,
        &outcome.stopped_system,
        &config,
        MAX_EXPORT_TRANSPORT_BYTES,
        request.limits().output_diff(),
        export_timeout,
        || {
            Instant::now() >= deadline
                || cancellation.is_cancelled()
                || connection.peer_disconnected()
        },
    ) {
        Ok(output) => output,
        Err(error) if error.cleanup_safe() => {
            return Err(quiescent_failure(
                attempt,
                Some(&mut scratch),
                if Instant::now() >= deadline {
                    BrokerFailureCode::TimedOut
                } else {
                    failure_code_for_export(error.kind())
                },
            ));
        }
        Err(error) => {
            return Err(quarantine_attempt(
                attempt,
                Some(&mut scratch),
                failure_code_for_export(error.kind()),
            ));
        }
    };

    if scratch.cleanup().is_err() {
        return Err(quarantine_attempt(
            attempt,
            Some(&mut scratch),
            BrokerFailureCode::CleanupFailed,
        ));
    }

    Ok((outcome.exit_code, outcome.output, output))
}

fn platform_request(
    request: BrokerExecutionRequest,
    limits: WindowsContainerExecutionLimits,
    deadline: Instant,
) -> io::Result<WindowsContainerExecutionRequest> {
    let remaining_timeout = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "execution deadline elapsed"))?;
    let limits = WindowsContainerExecutionLimits::new(
        limits.storage_quota_bytes(),
        remaining_timeout.min(limits.timeout()),
        limits.max_stdout_bytes(),
        limits.max_stderr_bytes(),
        limits.workspace_copy(),
        limits.output_diff(),
    )?;
    WindowsContainerExecutionRequest::new(
        match request.shell {
            BrokerShell::Cmd => WindowsContainerShell::Cmd,
            BrokerShell::PowerShell => WindowsContainerShell::PowerShell,
        },
        request.command,
        PathBuf::from(request.working_directory.replace('/', "\\")),
        request.execution_workspace_generation,
        request.policy_revision,
        limits,
    )
}

fn hcs_system_name(request_id: [u8; 16]) -> String {
    let mut name = String::from("HavenBroker-");
    for byte in request_id {
        use std::fmt::Write as _;
        let _ = write!(name, "{byte:02x}");
    }
    name
}

fn quiescent_failure(
    attempt: &mut BrokerWorkspaceAttempt,
    scratch: Option<&mut crate::windows_container::scratch_vhd::BoundedScratchVhd>,
    code: BrokerFailureCode,
) -> AttemptFailure {
    if let Some(scratch) = scratch
        && scratch.cleanup().is_err()
    {
        return quarantine_attempt(attempt, Some(scratch), BrokerFailureCode::CleanupFailed);
    }
    if attempt.mark_hcs_operation_quiescent().is_ok() {
        AttemptFailure::Safe(code)
    } else {
        quarantine_attempt(attempt, None, BrokerFailureCode::CleanupFailed)
    }
}

fn quarantine_attempt(
    attempt: &mut BrokerWorkspaceAttempt,
    scratch: Option<&mut crate::windows_container::scratch_vhd::BoundedScratchVhd>,
    code: BrokerFailureCode,
) -> AttemptFailure {
    if let Some(scratch) = scratch {
        scratch.preserve_for_recovery();
    }
    if attempt.preserve_for_recovery().is_err() {
        AttemptFailure::Quarantined(BrokerFailureCode::CleanupFailed)
    } else {
        AttemptFailure::Quarantined(code)
    }
}

fn fail_and_cleanup(
    connection: &BrokerPipeConnection<'_>,
    attempt: &mut BrokerWorkspaceAttempt,
    request_id: [u8; 16],
    code: BrokerFailureCode,
    deadline: Instant,
) -> io::Result<()> {
    let code = if attempt.cleanup().is_err() {
        BrokerFailureCode::CleanupFailed
    } else {
        code
    };
    write_failure(connection, request_id, code, deadline)
}

fn write_failure(
    connection: &BrokerPipeConnection<'_>,
    request_id: [u8; 16],
    code: BrokerFailureCode,
    deadline: Instant,
) -> io::Result<()> {
    let frame = BrokerFrame::control(&BrokerControlMessage::Failure {
        request_id: Some(request_id),
        code,
    })?;
    connection.write_frame_until(&frame, deadline)?;
    await_acknowledgement(connection, request_id, deadline)
}

fn write_execution_complete(
    connection: &BrokerPipeConnection<'_>,
    request_id: [u8; 16],
    exit_code: u32,
    stdout_truncated: bool,
    stderr_truncated: bool,
    deadline: Instant,
) -> io::Result<()> {
    let frame = BrokerFrame::control(&BrokerControlMessage::ExecutionComplete {
        request_id,
        exit_code,
        stdout_truncated,
        stderr_truncated,
    })?;
    connection.write_frame_until(&frame, deadline)?;
    await_acknowledgement(connection, request_id, deadline)
}

fn await_acknowledgement(
    connection: &BrokerPipeConnection<'_>,
    request_id: [u8; 16],
    deadline: Instant,
) -> io::Result<()> {
    match connection.read_frame_until(deadline)?.decode_control()? {
        BrokerControlMessage::ExecutionAcknowledged {
            request_id: acknowledged_id,
        } if acknowledged_id == request_id => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker terminal acknowledgement does not match the request",
        )),
    }
}

fn failure_code_for_guest(kind: HcsGuestExecutionFailureKind) -> BrokerFailureCode {
    match kind {
        HcsGuestExecutionFailureKind::Cancelled => BrokerFailureCode::Cancelled,
        HcsGuestExecutionFailureKind::TimedOut => BrokerFailureCode::TimedOut,
        HcsGuestExecutionFailureKind::HcsFailed
        | HcsGuestExecutionFailureKind::WorkspaceSeedFailed => BrokerFailureCode::HcsFailed,
    }
}

fn failure_code_for_export(
    kind: super::hcs_export_worker::HcsExportWorkerFailureKind,
) -> BrokerFailureCode {
    use super::hcs_export_worker::HcsExportWorkerFailureKind as Failure;
    match kind {
        Failure::Cancelled => BrokerFailureCode::Cancelled,
        Failure::TimedOut => BrokerFailureCode::TimedOut,
        Failure::TransportSetupFailed
        | Failure::OutputStagingFailed
        | Failure::OutputDecodeFailed => BrokerFailureCode::StagingFailed,
        Failure::InvalidRequest
        | Failure::ServiceIdentity
        | Failure::ContainmentUnavailable
        | Failure::SpawnFailed
        | Failure::RequestTransferFailed
        | Failure::WorkerFailed
        | Failure::ChildWaitFailed
        | Failure::RecoveryMarkerFailed => BrokerFailureCode::HcsFailed,
    }
}

fn failure_code_for_io(error: &io::Error) -> BrokerFailureCode {
    match error.kind() {
        io::ErrorKind::Interrupted => BrokerFailureCode::Cancelled,
        io::ErrorKind::TimedOut => BrokerFailureCode::TimedOut,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
            BrokerFailureCode::InvalidRequest
        }
        io::ErrorKind::PermissionDenied => BrokerFailureCode::StagingFailed,
        _ => BrokerFailureCode::HcsFailed,
    }
}
