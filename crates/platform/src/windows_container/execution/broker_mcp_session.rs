//! Broker-owned MCP stdio generations.
//!
//! One authenticated control request creates a broker-minted session pipe.
//! The service thread keeps the quota proof, attempt, scratch VHDX, HCS owner,
//! and both authenticated pipe connections in one supervised lifetime.

use super::broker_protocol::{
    BrokerControlMessage, BrokerFailureCode, BrokerFrame, BrokerFrameKind,
    BrokerMcpSessionOpenRequest, BrokerTreeLimits,
};
use super::broker_service::{
    McpSessionControlSignal, execution_gate_status, quota_runtime_attestation, write_broker_failure,
};
use super::broker_workspace::{BrokerWorkspaceAttempt, BrokerWorkspaceScratchRoot};
use super::named_pipe::{BrokerPipeCancellation, BrokerPipeConnection, BrokerPipeListener};
use super::quota_attestation::QuotaRuntimeAttestation;
use crate::windows_container::execution::WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
use crate::windows_container::execution::WindowsContainerOutputStream;
use crate::windows_container::hcs_mcp_session::HcsMcpGuestSessionOwner;
use crate::windows_container::hcs_process_output::HcsOutputChunkSink;
use crate::windows_container::hcs_process_output::HcsOutputDrainProgress;
use crate::windows_container::hcs_process_runtime::GuestProcessExitStatus;
use crate::windows_container::hcs_runtime::HcsStoppedComputeSystem;
use crate::windows_container::scratch_vhd::BoundedScratchVhd;
use std::io;
use std::time::Duration;

const HCS_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_PIPE_POLL_TIMEOUT: Duration = Duration::from_millis(10);
const SESSION_PIPE_POLL_TIMEOUT: Duration = Duration::from_millis(10);
const PROCESS_POLL_TIMEOUT: Duration = Duration::from_millis(1);
const FINAL_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const FRAME_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const WORKSPACE_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) struct PendingMcpSession {
    request_id: [u8; 16],
    session_id: [u8; 16],
    package_proof: crate::windows_container::execution::McpNodePackageProof,
    workspace_copy: Option<BrokerTreeLimits>,
    session_listener: BrokerPipeListener,
}

pub(super) enum McpSessionDisposition {
    Continue,
    RefreshPeer,
    StopService,
}

enum ControlPoll {
    Idle,
    StopSession,
}

pub(super) fn prepare_open(
    connection: &mut BrokerPipeConnection<'_>,
    request: BrokerMcpSessionOpenRequest,
    quota_attestation: Option<&QuotaRuntimeAttestation>,
) -> io::Result<Option<PendingMcpSession>> {
    if request.validate().is_err() {
        write_broker_failure(
            connection,
            Some(request.request_id),
            BrokerFailureCode::InvalidRequest,
        )?;
        return Ok(None);
    }
    let attestation =
        match quota_runtime_attestation(quota_attestation, WORKSPACE_WRITE_STORAGE_QUOTA_BYTES) {
            Ok(attestation) => attestation,
            Err(code) => {
                write_broker_failure(connection, Some(request.request_id), code)?;
                return Ok(None);
            }
        };
    let Some(package_proof) = attestation.mcp_node_package_proof(&request.package_id) else {
        write_broker_failure(
            connection,
            Some(request.request_id),
            BrokerFailureCode::McpPackageUnattested,
        )?;
        return Ok(None);
    };

    let session_id = uuid::Uuid::new_v4().into_bytes();
    let session_listener = connection.new_mcp_session_listener(session_id)?;
    let accepted = BrokerControlMessage::McpSessionOpenAccepted {
        request_id: request.request_id,
        session_id,
    };
    connection.write_frame(&BrokerFrame::control(&accepted)?, FRAME_WRITE_TIMEOUT)?;
    Ok(Some(PendingMcpSession {
        request_id: request.request_id,
        session_id,
        package_proof,
        workspace_copy: request.workspace_copy,
        session_listener,
    }))
}

pub(super) fn serve_pending(
    pending: PendingMcpSession,
    control_listener: &mut BrokerPipeListener,
    cancellation: &BrokerPipeCancellation,
    workspace_root: &BrokerWorkspaceScratchRoot,
    quota_attestation: Option<&QuotaRuntimeAttestation>,
    mut control_signal: impl FnMut() -> io::Result<McpSessionControlSignal>,
) -> io::Result<McpSessionDisposition> {
    let PendingMcpSession {
        request_id,
        session_id,
        package_proof,
        workspace_copy,
        mut session_listener,
    } = pending;
    let session_connection =
        match session_listener.accept_mcp_session(cancellation, request_id, session_id) {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                return match control_signal()? {
                    McpSessionControlSignal::StopService => Ok(McpSessionDisposition::StopService),
                    McpSessionControlSignal::RefreshPeer => Ok(McpSessionDisposition::RefreshPeer),
                    McpSessionControlSignal::None if peer_pipe_error(&error) => {
                        Ok(McpSessionDisposition::Continue)
                    }
                    McpSessionControlSignal::None => Err(error),
                };
            }
            Err(error) if peer_pipe_error(&error) => return Ok(McpSessionDisposition::Continue),
            Err(error) => return Err(error),
        };

    let Some(attestation) = quota_attestation else {
        write_session_failure(
            &session_connection,
            request_id,
            BrokerFailureCode::QuotaUnattested,
        )?;
        return Ok(McpSessionDisposition::Continue);
    };
    let mut attempt = match workspace_root.create_attempt() {
        Ok(attempt) => attempt,
        Err(_) => {
            write_session_failure(
                &session_connection,
                request_id,
                BrokerFailureCode::StagingFailed,
            )?;
            return Ok(McpSessionDisposition::Continue);
        }
    };

    if let Some(workspace_copy) = workspace_copy {
        let limits = match workspace_copy.to_platform_limits() {
            Ok(limits) => limits,
            Err(_) => {
                cleanup_quiescent_attempt(&mut attempt, None)?;
                write_session_failure(
                    &session_connection,
                    request_id,
                    BrokerFailureCode::InvalidRequest,
                )?;
                return Ok(McpSessionDisposition::Continue);
            }
        };
        let requested = BrokerControlMessage::WorkspaceSnapshotRequested { request_id };
        if session_connection
            .write_frame(&BrokerFrame::control(&requested)?, FRAME_WRITE_TIMEOUT)
            .is_err()
        {
            cleanup_quiescent_attempt(&mut attempt, None)?;
            return Ok(McpSessionDisposition::Continue);
        }
        let session_cancellation = session_connection.cancellation();
        let snapshot_deadline = std::time::Instant::now() + WORKSPACE_SNAPSHOT_TIMEOUT;
        let transfer = super::workspace_transfer::receive_workspace_snapshot(
            attempt.staging()?.clone(),
            limits,
            || session_connection.read_frame_until(snapshot_deadline),
            || {
                session_cancellation.is_cancelled()
                    || std::time::Instant::now() >= snapshot_deadline
            },
        );
        if let Err(error) = transfer {
            let timed_out = std::time::Instant::now() >= snapshot_deadline;
            cleanup_quiescent_attempt(&mut attempt, None)?;
            let code = match (timed_out, error.kind()) {
                (true, _) | (_, io::ErrorKind::TimedOut) => BrokerFailureCode::TimedOut,
                (_, io::ErrorKind::Interrupted) => BrokerFailureCode::Cancelled,
                (_, io::ErrorKind::InvalidData | io::ErrorKind::PermissionDenied) => {
                    BrokerFailureCode::InvalidRequest
                }
                _ => BrokerFailureCode::StagingFailed,
            };
            let _ = write_session_failure(&session_connection, request_id, code);
            return Ok(McpSessionDisposition::Continue);
        }

        let accepted = BrokerControlMessage::WorkspaceSnapshotAccepted {
            request_id,
            session_id,
        };
        if session_connection
            .write_frame(&BrokerFrame::control(&accepted)?, FRAME_WRITE_TIMEOUT)
            .is_err()
        {
            cleanup_quiescent_attempt(&mut attempt, None)?;
            return Ok(McpSessionDisposition::Continue);
        }
        let commit = match session_connection.read_frame_until(snapshot_deadline) {
            Ok(frame) => frame.decode_control(),
            Err(error) => Err(error),
        };
        match commit {
            Ok(BrokerControlMessage::WorkspaceSnapshotCommit {
                request_id: commit_request,
                session_id: commit_session,
            }) if commit_request == request_id && commit_session == session_id => {}
            Ok(_) => {
                cleanup_quiescent_attempt(&mut attempt, None)?;
                write_session_failure(
                    &session_connection,
                    request_id,
                    BrokerFailureCode::InvalidRequest,
                )?;
                return Ok(McpSessionDisposition::Continue);
            }
            Err(error) => {
                let timed_out = std::time::Instant::now() >= snapshot_deadline;
                cleanup_quiescent_attempt(&mut attempt, None)?;
                if peer_pipe_error(&error) {
                    return Ok(McpSessionDisposition::Continue);
                }
                let code = if timed_out || error.kind() == io::ErrorKind::TimedOut {
                    BrokerFailureCode::TimedOut
                } else if error.kind() == io::ErrorKind::Interrupted {
                    BrokerFailureCode::Cancelled
                } else {
                    BrokerFailureCode::InvalidRequest
                };
                let _ = write_session_failure(&session_connection, request_id, code);
                return Ok(McpSessionDisposition::Continue);
            }
        }
    }

    if attempt.mark_hcs_operation_active().is_err() {
        let _ = quarantine_attempt(&mut attempt, None);
        write_session_failure(
            &session_connection,
            request_id,
            BrokerFailureCode::CleanupFailed,
        )?;
        return Ok(McpSessionDisposition::Continue);
    }

    let attempt_root = match attempt.attempt_root() {
        Ok(root) => root.clone(),
        Err(_) => {
            cleanup_quiescent_attempt(&mut attempt, None)?;
            write_session_failure(
                &session_connection,
                request_id,
                BrokerFailureCode::StagingFailed,
            )?;
            return Ok(McpSessionDisposition::Continue);
        }
    };
    let mut scratch =
        match BoundedScratchVhd::create(attempt_root, WORKSPACE_WRITE_STORAGE_QUOTA_BYTES) {
            Ok(scratch) => scratch,
            Err(_) => {
                cleanup_quiescent_attempt(&mut attempt, None)?;
                write_session_failure(
                    &session_connection,
                    request_id,
                    BrokerFailureCode::StagingFailed,
                )?;
                return Ok(McpSessionDisposition::Continue);
            }
        };
    if scratch.format_for_container().is_err() {
        cleanup_quiescent_attempt(&mut attempt, Some(&mut scratch))?;
        write_session_failure(
            &session_connection,
            request_id,
            BrokerFailureCode::HcsFailed,
        )?;
        return Ok(McpSessionDisposition::Continue);
    }

    let config = match attestation
        .image_catalog()
        .sandbox_config(
            mcp_system_name(session_id),
            WORKSPACE_WRITE_STORAGE_QUOTA_BYTES,
        )
        .and_then(|config| config.with_bounded_writable_layer(&scratch))
        .and_then(|config| match workspace_copy {
            Some(_) => config.with_read_only_workspace_input(attempt.staging()?.clone()),
            None => Ok(config),
        }) {
        Ok(config) => config,
        Err(_) => {
            cleanup_quiescent_attempt(&mut attempt, Some(&mut scratch))?;
            write_session_failure(
                &session_connection,
                request_id,
                BrokerFailureCode::StagingFailed,
            )?;
            return Ok(McpSessionDisposition::Continue);
        }
    };

    let mut guest =
        match HcsMcpGuestSessionOwner::create(attestation.api(), &config, HCS_OPERATION_TIMEOUT) {
            Ok(guest) => guest,
            Err(_) => {
                quarantine_attempt(&mut attempt, Some(&mut scratch))?;
                write_session_failure(
                    &session_connection,
                    request_id,
                    BrokerFailureCode::HcsFailed,
                )?;
                return Ok(McpSessionDisposition::Continue);
            }
        };
    if guest
        .launch_package(&package_proof, HCS_OPERATION_TIMEOUT)
        .is_err()
    {
        match guest.stop() {
            Ok(stopped) if stopped_layer_matches(&stopped, &scratch)? => {
                let quota_verified = scratch.verify_maximum_size().is_ok();
                drop(stopped);
                cleanup_quiescent_attempt(&mut attempt, Some(&mut scratch))?;
                write_session_failure(
                    &session_connection,
                    request_id,
                    if quota_verified {
                        BrokerFailureCode::HcsFailed
                    } else {
                        BrokerFailureCode::QuotaUnattested
                    },
                )?;
                return Ok(McpSessionDisposition::Continue);
            }
            _ => {
                quarantine_attempt(&mut attempt, Some(&mut scratch))?;
                return Err(io::Error::other(
                    "MCP guest stop could not be confirmed after package launch failed",
                ));
            }
        }
    }

    let ready = BrokerControlMessage::McpSessionReady {
        request_id,
        session_id,
    };
    if session_connection
        .write_frame(&BrokerFrame::control(&ready)?, FRAME_WRITE_TIMEOUT)
        .is_err()
    {
        let stopped = guest.stop();
        match stopped {
            Ok(stopped) if stopped_layer_matches(&stopped, &scratch)? => {
                cleanup_quiescent_attempt(&mut attempt, Some(&mut scratch))?;
                return Ok(McpSessionDisposition::Continue);
            }
            _ => {
                quarantine_attempt(&mut attempt, Some(&mut scratch))?;
                return Err(io::Error::other(
                    "MCP guest stop could not be confirmed after ready delivery failed",
                ));
            }
        }
    }

    let end = pump_session(
        &session_connection,
        control_listener,
        cancellation,
        execution_gate_status(Some(attestation)),
        &mut guest,
        session_id,
        &mut control_signal,
    );
    let stopped = guest.stop();
    let stopped = match stopped {
        Ok(stopped) if stopped_layer_matches(&stopped, &scratch)? => stopped,
        _ => {
            quarantine_attempt(&mut attempt, Some(&mut scratch))?;
            return Err(io::Error::other(
                "MCP guest system stop could not be confirmed",
            ));
        }
    };
    let quota_verified = scratch.verify_maximum_size().is_ok();
    drop(stopped);

    match cleanup_quiescent_attempt(&mut attempt, Some(&mut scratch)) {
        Ok(()) => {}
        Err(error) => {
            quarantine_attempt(&mut attempt, Some(&mut scratch))?;
            return Err(error);
        }
    }

    let end = if quota_verified {
        end?
    } else {
        failed_pump(BrokerFailureCode::QuotaUnattested)
    };
    if end.report_exit {
        let exited = BrokerControlMessage::McpSessionExited {
            session_id,
            exit_code: end.exit_code,
        };
        let _ =
            session_connection.write_frame(&BrokerFrame::control(&exited)?, FRAME_WRITE_TIMEOUT);
    } else if let Some(code) = end.failure_code {
        let _ = write_session_failure(&session_connection, request_id, code);
    }
    Ok(end.disposition)
}

struct PumpEnd {
    exit_code: Option<u32>,
    report_exit: bool,
    failure_code: Option<BrokerFailureCode>,
    disposition: McpSessionDisposition,
}

trait McpSessionIo {
    fn try_write_stdin_chunk(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn close_stdin(&mut self) -> io::Result<()>;
    fn poll_stdin_writer(&mut self) -> io::Result<bool>;
    fn poll_process_exit(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<GuestProcessExitStatus>>;
    fn drain_output(
        &mut self,
        sink: &mut dyn HcsOutputChunkSink,
    ) -> io::Result<HcsOutputDrainProgress>;
}

impl McpSessionIo for HcsMcpGuestSessionOwner<'_> {
    fn try_write_stdin_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        HcsMcpGuestSessionOwner::try_write_stdin_chunk(self, bytes)
    }

    fn close_stdin(&mut self) -> io::Result<()> {
        HcsMcpGuestSessionOwner::close_stdin(self)
    }

    fn poll_stdin_writer(&mut self) -> io::Result<bool> {
        HcsMcpGuestSessionOwner::poll_stdin_writer(self)
    }

    fn poll_process_exit(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<GuestProcessExitStatus>> {
        HcsMcpGuestSessionOwner::poll_process_exit(self, timeout)
    }

    fn drain_output(
        &mut self,
        sink: &mut dyn HcsOutputChunkSink,
    ) -> io::Result<HcsOutputDrainProgress> {
        HcsMcpGuestSessionOwner::drain_output(self, sink)
    }
}

fn pump_session<G: McpSessionIo>(
    session_connection: &BrokerPipeConnection<'_>,
    control_listener: &mut BrokerPipeListener,
    cancellation: &BrokerPipeCancellation,
    gate_status: super::broker_protocol::BrokerExecutionGateStatus,
    guest: &mut G,
    session_id: [u8; 16],
    control_signal: &mut impl FnMut() -> io::Result<McpSessionControlSignal>,
) -> io::Result<PumpEnd> {
    let mut pending_input: Option<Vec<u8>> = None;
    let mut input_closed = false;
    let mut process_exit: Option<GuestProcessExitStatus> = None;
    let mut final_drain_deadline = None;

    loop {
        match control_signal()? {
            McpSessionControlSignal::StopService => {
                return Ok(PumpEnd {
                    exit_code: None,
                    report_exit: false,
                    failure_code: None,
                    disposition: McpSessionDisposition::StopService,
                });
            }
            McpSessionControlSignal::RefreshPeer => {
                return Ok(PumpEnd {
                    exit_code: None,
                    report_exit: false,
                    failure_code: None,
                    disposition: McpSessionDisposition::RefreshPeer,
                });
            }
            McpSessionControlSignal::None => {}
        }
        if session_connection.peer_disconnected() {
            return Ok(PumpEnd {
                exit_code: None,
                report_exit: false,
                failure_code: None,
                disposition: McpSessionDisposition::Continue,
            });
        }

        match poll_control_request(control_listener, cancellation, gate_status, session_id) {
            Ok(ControlPoll::Idle) => {}
            Ok(ControlPoll::StopSession) => {
                return Ok(PumpEnd {
                    exit_code: None,
                    report_exit: true,
                    failure_code: None,
                    disposition: McpSessionDisposition::Continue,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => match control_signal()? {
                McpSessionControlSignal::StopService => {
                    return Ok(PumpEnd {
                        exit_code: None,
                        report_exit: false,
                        failure_code: None,
                        disposition: McpSessionDisposition::StopService,
                    });
                }
                McpSessionControlSignal::RefreshPeer => {
                    return Ok(PumpEnd {
                        exit_code: None,
                        report_exit: false,
                        failure_code: None,
                        disposition: McpSessionDisposition::RefreshPeer,
                    });
                }
                McpSessionControlSignal::None => return Err(error),
            },
            Err(error) => return Err(error),
        }

        if let Some(bytes) = pending_input.as_deref() {
            match guest.try_write_stdin_chunk(bytes) {
                Ok(()) => pending_input = None,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    return Ok(failed_pump(BrokerFailureCode::HcsFailed));
                }
            }
        }

        if pending_input.is_none() && process_exit.is_none() {
            match session_connection.read_frame(SESSION_PIPE_POLL_TIMEOUT) {
                Ok(frame) if frame.kind() == BrokerFrameKind::McpStdinChunk => {
                    if input_closed {
                        return Ok(failed_pump(BrokerFailureCode::InvalidRequest));
                    }
                    match guest.try_write_stdin_chunk(frame.payload()) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            pending_input = Some(frame.payload().to_vec());
                        }
                        Err(_) => return Ok(failed_pump(BrokerFailureCode::HcsFailed)),
                    }
                }
                Ok(frame) if frame.kind() == BrokerFrameKind::Control => {
                    let control = match frame.decode_control() {
                        Ok(control) => control,
                        Err(_) => return Ok(failed_pump(BrokerFailureCode::InvalidRequest)),
                    };
                    match control {
                        BrokerControlMessage::McpSessionInputClosed {
                            session_id: received,
                        } if received == session_id && !input_closed => {
                            guest.close_stdin()?;
                            input_closed = true;
                        }
                        BrokerControlMessage::McpSessionStop {
                            session_id: received,
                        } if received == session_id => {
                            return Ok(PumpEnd {
                                exit_code: None,
                                report_exit: true,
                                failure_code: None,
                                disposition: McpSessionDisposition::Continue,
                            });
                        }
                        _ => return Ok(failed_pump(BrokerFailureCode::InvalidRequest)),
                    }
                }
                Ok(_) => return Ok(failed_pump(BrokerFailureCode::InvalidRequest)),
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    match control_signal()? {
                        McpSessionControlSignal::StopService => {
                            return Ok(PumpEnd {
                                exit_code: None,
                                report_exit: false,
                                failure_code: None,
                                disposition: McpSessionDisposition::StopService,
                            });
                        }
                        McpSessionControlSignal::RefreshPeer => {
                            return Ok(PumpEnd {
                                exit_code: None,
                                report_exit: false,
                                failure_code: None,
                                disposition: McpSessionDisposition::RefreshPeer,
                            });
                        }
                        McpSessionControlSignal::None => return Err(error),
                    }
                }
                Err(error) if peer_pipe_error(&error) => {
                    return Ok(PumpEnd {
                        exit_code: None,
                        report_exit: false,
                        failure_code: None,
                        disposition: McpSessionDisposition::Continue,
                    });
                }
                Err(error) => return Err(error),
            }
        }

        if guest.poll_stdin_writer().is_err() && process_exit.is_none() {
            return Ok(failed_pump(BrokerFailureCode::HcsFailed));
        }

        let mut output = McpSessionOutputSink::new(session_connection);
        let progress = match guest.drain_output(&mut output) {
            Ok(progress) => progress,
            Err(_) => return Ok(failed_pump(BrokerFailureCode::HcsFailed)),
        };
        if output.failed {
            return Ok(failed_pump(BrokerFailureCode::TimedOut));
        }

        if process_exit.is_none() {
            match guest.poll_process_exit(PROCESS_POLL_TIMEOUT) {
                Ok(Some(status)) => {
                    process_exit = Some(status);
                    if !input_closed {
                        guest.close_stdin()?;
                        input_closed = true;
                    }
                    final_drain_deadline =
                        Some(std::time::Instant::now() + FINAL_OUTPUT_DRAIN_TIMEOUT);
                }
                Ok(None) => {}
                Err(_) => return Ok(failed_pump(BrokerFailureCode::HcsFailed)),
            }
        }

        if let Some(status) = process_exit {
            if progress.is_complete() {
                return Ok(PumpEnd {
                    exit_code: Some(status.exit_code),
                    report_exit: true,
                    failure_code: None,
                    disposition: McpSessionDisposition::Continue,
                });
            }
            if final_drain_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Ok(failed_pump(BrokerFailureCode::TimedOut));
            }
        }
    }
}

fn poll_control_request(
    listener: &mut BrokerPipeListener,
    cancellation: &BrokerPipeCancellation,
    gate_status: super::broker_protocol::BrokerExecutionGateStatus,
    session_id: [u8; 16],
) -> io::Result<ControlPoll> {
    let mut connection = match listener.accept_with_timeout(cancellation, CONTROL_PIPE_POLL_TIMEOUT)
    {
        Ok(connection) => connection,
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            return Ok(ControlPoll::Idle);
        }
        Err(error) if peer_pipe_error(&error) => return Ok(ControlPoll::Idle),
        Err(error) => return Err(error),
    };
    let result = (|| match connection
        .read_frame(FRAME_WRITE_TIMEOUT)?
        .decode_control()?
    {
        BrokerControlMessage::ExecutionGateStatusRequest => {
            connection.write_frame(
                &BrokerFrame::control(&BrokerControlMessage::ExecutionGateStatus {
                    status: gate_status,
                })?,
                FRAME_WRITE_TIMEOUT,
            )?;
            match connection
                .read_frame(FRAME_WRITE_TIMEOUT)?
                .decode_control()?
            {
                BrokerControlMessage::ExecutionGateStatusAcknowledged => Ok(ControlPoll::Idle),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "broker execution gate status was not acknowledged",
                )),
            }
        }
        BrokerControlMessage::Execute { request } => write_broker_failure(
            &mut connection,
            Some(request.request_id),
            BrokerFailureCode::BackendUnavailable,
        )
        .map(|()| ControlPoll::Idle),
        BrokerControlMessage::McpSessionOpen { request } => write_broker_failure(
            &mut connection,
            Some(request.request_id),
            BrokerFailureCode::BackendUnavailable,
        )
        .map(|()| ControlPoll::Idle),
        BrokerControlMessage::McpSessionStop {
            session_id: requested_session,
        } if requested_session == session_id => Ok(ControlPoll::StopSession),
        _ => write_broker_failure(&mut connection, None, BrokerFailureCode::InvalidRequest)
            .map(|()| ControlPoll::Idle),
    })();
    match result {
        Err(error) if peer_pipe_error(&error) => Ok(ControlPoll::Idle),
        result => result,
    }
}

struct McpSessionOutputSink<'connection, 'pipe> {
    connection: &'connection BrokerPipeConnection<'pipe>,
    failed: bool,
}

impl<'connection, 'pipe> McpSessionOutputSink<'connection, 'pipe> {
    fn new(connection: &'connection BrokerPipeConnection<'pipe>) -> Self {
        Self {
            connection,
            failed: false,
        }
    }
}

impl HcsOutputChunkSink for McpSessionOutputSink<'_, '_> {
    fn write_chunk(&mut self, stream: WindowsContainerOutputStream, bytes: &[u8]) {
        if self.failed {
            return;
        }
        let kind = match stream {
            WindowsContainerOutputStream::Stdout => BrokerFrameKind::McpStdoutChunk,
            WindowsContainerOutputStream::Stderr => BrokerFrameKind::McpStderrChunk,
        };
        self.failed = BrokerFrame::new(kind, bytes.to_vec())
            .and_then(|frame| self.connection.write_frame(&frame, FRAME_WRITE_TIMEOUT))
            .is_err();
    }
}

fn failed_pump(code: BrokerFailureCode) -> PumpEnd {
    PumpEnd {
        exit_code: None,
        report_exit: false,
        failure_code: Some(code),
        disposition: McpSessionDisposition::Continue,
    }
}

fn stopped_layer_matches(
    stopped: &HcsStoppedComputeSystem,
    scratch: &BoundedScratchVhd,
) -> io::Result<bool> {
    Ok(stopped.writable_layer_path() == scratch.layer_path()?)
}

fn cleanup_quiescent_attempt(
    attempt: &mut BrokerWorkspaceAttempt,
    scratch: Option<&mut BoundedScratchVhd>,
) -> io::Result<()> {
    if let Some(scratch) = scratch {
        scratch.cleanup()?;
    }
    attempt.mark_hcs_operation_quiescent()?;
    attempt.cleanup()
}

fn quarantine_attempt(
    attempt: &mut BrokerWorkspaceAttempt,
    scratch: Option<&mut BoundedScratchVhd>,
) -> io::Result<()> {
    if let Some(scratch) = scratch {
        scratch.preserve_for_recovery();
    }
    attempt.preserve_for_recovery()
}

fn write_session_failure(
    connection: &BrokerPipeConnection<'_>,
    request_id: [u8; 16],
    code: BrokerFailureCode,
) -> io::Result<()> {
    let response = BrokerControlMessage::Failure {
        request_id: Some(request_id),
        code,
    };
    match connection.write_frame(&BrokerFrame::control(&response)?, FRAME_WRITE_TIMEOUT) {
        Err(error) if peer_pipe_error(&error) || error.kind() == io::ErrorKind::Interrupted => {
            Ok(())
        }
        result => result,
    }
}

fn mcp_system_name(session_id: [u8; 16]) -> String {
    let mut name = String::from("HavenMcp-");
    for byte in session_id {
        use std::fmt::Write as _;
        let _ = write!(name, "{byte:02x}");
    }
    name
}

fn peer_pipe_error(error: &io::Error) -> bool {
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

#[cfg(test)]
#[path = "broker_mcp_session_tests.rs"]
mod tests;
