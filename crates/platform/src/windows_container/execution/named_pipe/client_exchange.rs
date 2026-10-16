//! One-request client protocol exchange and private output review ownership.

use super::super::broker_protocol::{
    BrokerControlMessage, BrokerExecutionRequest, BrokerFailureCode, BrokerFrame, BrokerFrameKind,
};
use super::super::output_transfer::receive_output_snapshot;
use super::super::workspace_transfer::{
    send_skill_bundle_snapshot, send_skill_parameters, send_workspace_snapshot,
};
use super::super::{
    WindowsContainerCapturedOutput, WindowsContainerExecutionLimits,
    WindowsContainerExecutionRequest, WindowsContainerOutputSink, WindowsContainerOutputStream,
    WindowsContainerProcessStartCallbacks, WindowsContainerSkillBundleInput,
    WindowsContainerSkillExecutionRequest, WindowsContainerWorkspaceInput,
};
use super::{BrokerPipeClient, deadline_after, invalid_data};
use crate::filesystem::DirectoryCapability;
use crate::sandbox_output::{SandboxOutputReview, SandboxOutputWorkspaceOwner};
use std::io;
use std::path::Path;

pub(crate) enum BrokerPipeExecutionResponse {
    Failed(BrokerFailureCode),
    Completed {
        exit_code: u32,
        captured_output: WindowsContainerCapturedOutput,
        output_review: SandboxOutputReview,
    },
}

struct BrokerClientReviewWorkspace {
    storage: tempfile::TempDir,
    output: DirectoryCapability,
    execution_workspace_generation: u64,
    policy_revision: u64,
    initial_snapshot_bytes: u64,
}

impl BrokerClientReviewWorkspace {
    fn new(
        storage: tempfile::TempDir,
        output: DirectoryCapability,
        execution_workspace_generation: u64,
        policy_revision: u64,
        initial_snapshot_bytes: u64,
    ) -> Self {
        Self {
            storage,
            output,
            execution_workspace_generation,
            policy_revision,
            initial_snapshot_bytes,
        }
    }
}

impl SandboxOutputWorkspaceOwner for BrokerClientReviewWorkspace {
    fn execution_workspace_generation(&self) -> u64 {
        self.execution_workspace_generation
    }

    fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    fn initial_snapshot_byte_count(&self) -> u64 {
        self.initial_snapshot_bytes
    }

    fn promote_created_file(
        &self,
        change: &crate::filesystem::DirectoryTreeChange,
        destination: &DirectoryCapability,
        destination_relative: &Path,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> io::Result<crate::filesystem::DirectoryTreeFileCopySummary> {
        self.output.copy_reviewed_created_file_to(
            change,
            destination,
            destination_relative,
            is_cancelled,
        )
    }

    fn cleanup(self: Box<Self>) -> io::Result<()> {
        let Self {
            output, storage, ..
        } = *self;
        drop(output);
        storage.close()
    }
}

pub(super) fn execute_with_workspace(
    client: &mut BrokerPipeClient,
    request: &WindowsContainerExecutionRequest,
    request_id: [u8; 16],
    workspace: &WindowsContainerWorkspaceInput,
    start_callbacks: &mut WindowsContainerProcessStartCallbacks<'_>,
    output_sink: WindowsContainerOutputSink<'_>,
) -> io::Result<BrokerPipeExecutionResponse> {
    let wire_request = BrokerExecutionRequest::from_execution_request(request, request_id)?;
    execute_with_inputs(
        client,
        BrokerClientExecutionPlan {
            wire_request: &wire_request,
            limits: request.limits(),
            execution_workspace_generation: request.execution_workspace_generation(),
            policy_revision: request.policy_revision(),
            skill_input: None,
        },
        workspace,
        start_callbacks,
        output_sink,
    )
}

pub(super) fn execute_with_skill(
    client: &mut BrokerPipeClient,
    request: &WindowsContainerSkillExecutionRequest,
    request_id: [u8; 16],
    workspace: &WindowsContainerWorkspaceInput,
    skill_bundle: &WindowsContainerSkillBundleInput,
    start_callbacks: &mut WindowsContainerProcessStartCallbacks<'_>,
    output_sink: WindowsContainerOutputSink<'_>,
) -> io::Result<BrokerPipeExecutionResponse> {
    let wire_request = BrokerExecutionRequest::from_skill_request(request, request_id)?;
    execute_with_inputs(
        client,
        BrokerClientExecutionPlan {
            wire_request: &wire_request,
            limits: request.limits(),
            execution_workspace_generation: request.execution_workspace_generation(),
            policy_revision: request.policy_revision(),
            skill_input: Some((skill_bundle, request)),
        },
        workspace,
        start_callbacks,
        output_sink,
    )
}

struct BrokerClientExecutionPlan<'a> {
    wire_request: &'a BrokerExecutionRequest,
    limits: &'a WindowsContainerExecutionLimits,
    execution_workspace_generation: u64,
    policy_revision: u64,
    skill_input: Option<(
        &'a WindowsContainerSkillBundleInput,
        &'a WindowsContainerSkillExecutionRequest,
    )>,
}

fn execute_with_inputs(
    client: &mut BrokerPipeClient,
    plan: BrokerClientExecutionPlan<'_>,
    workspace: &WindowsContainerWorkspaceInput,
    start_callbacks: &mut WindowsContainerProcessStartCallbacks<'_>,
    mut output_sink: WindowsContainerOutputSink<'_>,
) -> io::Result<BrokerPipeExecutionResponse> {
    let BrokerClientExecutionPlan {
        wire_request,
        limits,
        execution_workspace_generation,
        policy_revision,
        skill_input,
    } = plan;
    let request_id = wire_request.request_id;
    let deadline = deadline_after(limits.timeout())?;
    client.write_frame_until(
        &BrokerFrame::control(&BrokerControlMessage::Execute {
            request: wire_request.clone(),
        })?,
        deadline,
    )?;

    // Keep the exact bounded snapshot sent to the broker as the diff baseline.
    // This prevents concurrent workspace changes from making the returned
    // output review refer to a different tree than the one the guest received.
    let storage = tempfile::tempdir()?;
    let root = DirectoryCapability::open(storage.path())?;
    let cancellation = client.cancellation.clone();
    if let Some((skill_bundle, skill_request)) = skill_input {
        match client.read_frame_until(deadline)?.decode_control()? {
            BrokerControlMessage::Failure {
                request_id: Some(response_id),
                code,
            } if response_id == request_id => {
                acknowledge_response(client, request_id, deadline)?;
                return Ok(BrokerPipeExecutionResponse::Failed(code));
            }
            BrokerControlMessage::SkillBundleSnapshotRequested {
                request_id: response_id,
            } if response_id == request_id => {}
            _ => return Err(invalid_data("broker did not request the Skill bundle")),
        }
        root.create_dir(Path::new("skill-bundle"))?;
        let bundle_path = storage.path().join("skill-bundle");
        let bundle_snapshot = DirectoryCapability::open(&bundle_path)?;
        skill_bundle.copy_snapshot_to(
            &bundle_snapshot,
            skill_request.bundle_copy_limits(),
            || cancellation.is_cancelled(),
        )?;
        send_skill_bundle_snapshot(
            &bundle_snapshot,
            skill_request.bundle_copy_limits(),
            |frame| client.write_frame_until(&frame, deadline),
            || cancellation.is_cancelled(),
        )?;
        send_skill_parameters(
            skill_request.contract().parameters_json(),
            |frame| client.write_frame_until(&frame, deadline),
            || cancellation.is_cancelled(),
        )?;
        drop(bundle_snapshot);
        root.remove_dir_all(Path::new("skill-bundle"))?;
    } else {
        match client.read_frame_until(deadline)?.decode_control()? {
            BrokerControlMessage::Failure {
                request_id: Some(response_id),
                code,
            } if response_id == request_id => {
                acknowledge_response(client, request_id, deadline)?;
                return Ok(BrokerPipeExecutionResponse::Failed(code));
            }
            BrokerControlMessage::WorkspaceSnapshotRequested {
                request_id: response_id,
            } if response_id == request_id => {}
            _ => {
                return Err(invalid_data(
                    "broker did not request the workspace snapshot",
                ));
            }
        }
    }
    if skill_input.is_some() {
        match client.read_frame_until(deadline)?.decode_control()? {
            BrokerControlMessage::Failure {
                request_id: Some(response_id),
                code,
            } if response_id == request_id => {
                acknowledge_response(client, request_id, deadline)?;
                return Ok(BrokerPipeExecutionResponse::Failed(code));
            }
            BrokerControlMessage::WorkspaceSnapshotRequested {
                request_id: response_id,
            } if response_id == request_id => {}
            _ => {
                return Err(invalid_data(
                    "broker did not request the workspace snapshot",
                ));
            }
        }
    }

    root.create_dir(Path::new("baseline"))?;
    root.create_dir(Path::new("output"))?;
    root.create_dir(Path::new("file-staging"))?;
    let baseline = DirectoryCapability::open(&storage.path().join("baseline"))?;
    let output_directory = DirectoryCapability::open(&storage.path().join("output"))?;
    let file_staging = DirectoryCapability::open(&storage.path().join("file-staging"))?;
    let input_summary = workspace.copy_snapshot_to(&baseline, limits.workspace_copy(), || {
        cancellation.is_cancelled()
    })?;

    send_workspace_snapshot(
        &baseline,
        limits.workspace_copy(),
        |frame| client.write_frame_until(&frame, deadline),
        || cancellation.is_cancelled(),
    )?;

    let mut received_stdout_bytes = 0usize;
    let mut received_stderr_bytes = 0usize;
    let mut start_requested = false;
    let mut start_confirmed = false;
    let first_output_frame = loop {
        let frame = client.read_frame_until(deadline)?;
        match frame.kind() {
            BrokerFrameKind::StdoutChunk if start_confirmed => receive_stream_chunk(
                &mut output_sink,
                WindowsContainerOutputStream::Stdout,
                frame.payload(),
                &mut received_stdout_bytes,
                limits.max_stdout_bytes(),
            )?,
            BrokerFrameKind::StderrChunk if start_confirmed => receive_stream_chunk(
                &mut output_sink,
                WindowsContainerOutputStream::Stderr,
                frame.payload(),
                &mut received_stderr_bytes,
                limits.max_stderr_bytes(),
            )?,
            BrokerFrameKind::StdoutChunk | BrokerFrameKind::StderrChunk => {
                return Err(invalid_data(
                    "broker output preceded Shell start confirmation",
                ));
            }
            BrokerFrameKind::Control => match frame.decode_control()? {
                BrokerControlMessage::Failure {
                    request_id: Some(response_id),
                    code,
                } if response_id == request_id => {
                    acknowledge_response(client, request_id, deadline)?;
                    return Ok(BrokerPipeExecutionResponse::Failed(code));
                }
                BrokerControlMessage::ExecutionStartRequested {
                    request_id: response_id,
                } if response_id == request_id && !start_requested && !start_confirmed => {
                    (start_callbacks.before_process_start)()?;
                    let authorized = BrokerControlMessage::ExecutionStartAuthorized { request_id };
                    client.write_frame_until(&BrokerFrame::control(&authorized)?, deadline)?;
                    start_requested = true;
                }
                BrokerControlMessage::ExecutionStarted {
                    request_id: response_id,
                } if response_id == request_id && start_requested && !start_confirmed => {
                    start_confirmed = true;
                    (start_callbacks.process_started)();
                }
                BrokerControlMessage::OutputEntry { .. }
                | BrokerControlMessage::OutputSnapshotComplete { .. }
                    if start_confirmed =>
                {
                    break frame;
                }
                _ => return Err(invalid_data("broker output stream started unexpectedly")),
            },
            _ => return Err(invalid_data("broker output stream started unexpectedly")),
        }
    };

    let mut pending_frame = Some(first_output_frame);
    let received = receive_output_snapshot(
        &baseline,
        output_directory,
        file_staging,
        limits.output_diff(),
        || loop {
            let frame = match pending_frame.take() {
                Some(frame) => frame,
                None => client.read_frame_until(deadline)?,
            };
            match frame.kind() {
                BrokerFrameKind::StdoutChunk => receive_stream_chunk(
                    &mut output_sink,
                    WindowsContainerOutputStream::Stdout,
                    frame.payload(),
                    &mut received_stdout_bytes,
                    limits.max_stdout_bytes(),
                )?,
                BrokerFrameKind::StderrChunk => receive_stream_chunk(
                    &mut output_sink,
                    WindowsContainerOutputStream::Stderr,
                    frame.payload(),
                    &mut received_stderr_bytes,
                    limits.max_stderr_bytes(),
                )?,
                _ => return Ok(frame),
            }
        },
        || cancellation.is_cancelled(),
    )?;
    if received.initial_snapshot_bytes != input_summary.byte_count {
        return Err(invalid_data(
            "broker output baseline does not match the transferred workspace",
        ));
    }

    drop(baseline);
    root.remove_dir_all(Path::new("baseline"))?;
    root.remove_dir_all(Path::new("file-staging"))?;
    drop(root);

    let terminal = loop {
        let frame = client.read_frame_until(deadline)?;
        match frame.kind() {
            BrokerFrameKind::StdoutChunk => receive_stream_chunk(
                &mut output_sink,
                WindowsContainerOutputStream::Stdout,
                frame.payload(),
                &mut received_stdout_bytes,
                limits.max_stdout_bytes(),
            )?,
            BrokerFrameKind::StderrChunk => receive_stream_chunk(
                &mut output_sink,
                WindowsContainerOutputStream::Stderr,
                frame.payload(),
                &mut received_stderr_bytes,
                limits.max_stderr_bytes(),
            )?,
            BrokerFrameKind::Control => {
                let message = frame.decode_control()?;
                match message {
                    BrokerControlMessage::Failure {
                        request_id: Some(response_id),
                        code,
                    } if response_id == request_id => {
                        acknowledge_response(client, request_id, deadline)?;
                        return Ok(BrokerPipeExecutionResponse::Failed(code));
                    }
                    BrokerControlMessage::ExecutionComplete {
                        request_id: response_id,
                        exit_code,
                        stdout_truncated,
                        stderr_truncated,
                    } if response_id == request_id => {
                        break (exit_code, stdout_truncated, stderr_truncated);
                    }
                    _ => return Err(invalid_data("broker terminal response is invalid")),
                }
            }
            _ => return Err(invalid_data("broker sent an unexpected terminal frame")),
        }
    };

    let mut captured_output = output_sink.finish();
    if (terminal.1 && captured_output.stdout.len() != limits.max_stdout_bytes())
        || (terminal.2 && captured_output.stderr.len() != limits.max_stderr_bytes())
    {
        return Err(invalid_data(
            "broker output truncation status does not match its stream limit",
        ));
    }
    acknowledge_response(client, request_id, deadline)?;
    captured_output.stdout_truncated = terminal.1;
    captured_output.stderr_truncated = terminal.2;

    let output_review = SandboxOutputReview::new(
        Box::new(BrokerClientReviewWorkspace::new(
            storage,
            received.output,
            execution_workspace_generation,
            policy_revision,
            received.initial_snapshot_bytes,
        )),
        received.manifest,
    );
    Ok(BrokerPipeExecutionResponse::Completed {
        exit_code: terminal.0,
        captured_output,
        output_review,
    })
}

fn acknowledge_response(
    client: &mut BrokerPipeClient,
    request_id: [u8; 16],
    deadline: std::time::Instant,
) -> io::Result<()> {
    let frame = BrokerFrame::control(&BrokerControlMessage::ExecutionAcknowledged { request_id })?;
    client.write_frame_until(&frame, deadline)
}

fn receive_stream_chunk(
    sink: &mut WindowsContainerOutputSink<'_>,
    stream: WindowsContainerOutputStream,
    bytes: &[u8],
    received_bytes: &mut usize,
    maximum_bytes: usize,
) -> io::Result<()> {
    *received_bytes = received_bytes
        .checked_add(bytes.len())
        .filter(|count| *count <= maximum_bytes)
        .ok_or_else(|| invalid_data("broker process output exceeds its stream limit"))?;
    sink.write_chunk(stream, bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::{DirectoryTreeCopyLimits, DirectoryTreeDiffLimits};
    use crate::windows_container::execution::{
        WindowsContainerExecutionLimits, WindowsContainerShell,
    };

    #[test]
    fn broker_stream_overrun_is_rejected_before_live_publication() {
        let copy = DirectoryTreeCopyLimits::new(8, 4, 16, 32).unwrap();
        let diff = DirectoryTreeDiffLimits::new(copy, 8, 16, 32).unwrap();
        let limits = WindowsContainerExecutionLimits::new(
            1024,
            std::time::Duration::from_secs(1),
            4,
            4,
            copy,
            diff,
        )
        .unwrap();
        let request = WindowsContainerExecutionRequest::new(
            WindowsContainerShell::Cmd,
            "echo bounded",
            "",
            1,
            1,
            limits,
        )
        .unwrap();
        let mut published = Vec::new();
        let mut on_output =
            |_: WindowsContainerOutputStream, bytes: &[u8]| published.extend_from_slice(bytes);
        let mut sink = WindowsContainerOutputSink::new(&request, &mut on_output);
        let mut received = 0;

        receive_stream_chunk(
            &mut sink,
            WindowsContainerOutputStream::Stdout,
            b"four",
            &mut received,
            4,
        )
        .unwrap();
        let error = receive_stream_chunk(
            &mut sink,
            WindowsContainerOutputStream::Stdout,
            b"!",
            &mut received,
            4,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let captured = sink.finish();
        assert_eq!(published, b"four");
        assert_eq!(captured.stdout(), b"four");
    }
}
