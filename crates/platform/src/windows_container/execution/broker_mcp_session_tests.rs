use super::*;
use crate::windows_container::execution::broker_protocol::{
    BrokerExecutionGateStatus, BrokerMcpNodeRuntimeStatus, BrokerQuotaGateStatus,
    BrokerSkillPythonRuntimeStatus,
};
use crate::windows_container::execution::named_pipe::{
    test_connect_control_pipe, test_connect_mcp_session, test_control_pipe,
    test_mcp_session_listener,
};
use std::sync::mpsc;

struct FakeGuest {
    input: Vec<Vec<u8>>,
    input_closed: bool,
    output_emitted: bool,
    exit_on_close: bool,
    block_next_write: bool,
    block_writes: bool,
    first_write_attempt: Option<mpsc::Sender<()>>,
}

impl McpSessionIo for FakeGuest {
    fn try_write_stdin_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        if let Some(signal) = self.first_write_attempt.take() {
            let _ = signal.send(());
        }
        if self.block_writes {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "test stdin remains backpressured",
            ));
        }
        if self.block_next_write {
            self.block_next_write = false;
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "test stdin is applying backpressure",
            ));
        }
        self.input.push(bytes.to_vec());
        Ok(())
    }

    fn close_stdin(&mut self) -> io::Result<()> {
        self.input_closed = true;
        Ok(())
    }

    fn poll_stdin_writer(&mut self) -> io::Result<bool> {
        Ok(true)
    }

    fn poll_process_exit(
        &mut self,
        _timeout: Duration,
    ) -> io::Result<Option<GuestProcessExitStatus>> {
        Ok(
            (self.input_closed && self.exit_on_close).then_some(GuestProcessExitStatus {
                process_id: 13,
                exit_code: 23,
            }),
        )
    }

    fn drain_output(
        &mut self,
        sink: &mut dyn HcsOutputChunkSink,
    ) -> io::Result<HcsOutputDrainProgress> {
        let (stdout_bytes_read, stderr_bytes_read) = if self.output_emitted {
            (0, 0)
        } else {
            self.output_emitted = true;
            sink.write_chunk(WindowsContainerOutputStream::Stdout, b"rpc-bytes");
            sink.write_chunk(WindowsContainerOutputStream::Stderr, b"diagnostic");
            (9, 10)
        };
        Ok(HcsOutputDrainProgress {
            stdout_bytes_read,
            stderr_bytes_read,
            stdout_closed: true,
            stderr_closed: true,
        })
    }
}

#[test]
fn supervisor_relays_stdio_while_answering_control_status() {
    let request_id = [0x27; 16];
    let session_id = [0x52; 16];
    let mut session_listener = test_mcp_session_listener(session_id).unwrap();
    let (mut control_listener, control_name) = test_control_pipe().unwrap();
    let cancellation = BrokerPipeCancellation::new().unwrap();
    let expected_status = BrokerExecutionGateStatus {
        quota: BrokerQuotaGateStatus::Attested,
        skill_python_runtime: BrokerSkillPythonRuntimeStatus::NotConfigured,
        mcp_node_runtime: BrokerMcpNodeRuntimeStatus::Attested,
    };
    let (client_done, client_result) = mpsc::channel();
    let client_control_name = control_name.clone();
    let client = std::thread::spawn(move || -> io::Result<()> {
        let mut session = test_connect_mcp_session(session_id, request_id)?;
        match session
            .read_frame(Duration::from_secs(5))?
            .decode_control()?
        {
            BrokerControlMessage::McpSessionReady {
                request_id: ready_request,
                session_id: ready_session,
            } if ready_request == request_id && ready_session == session_id => {}
            _ => return Err(io::Error::other("MCP session was not marked ready")),
        }
        session.write_frame(
            &BrokerFrame::new(BrokerFrameKind::McpStdinChunk, b"input-jsonrpc".to_vec())?,
            FRAME_WRITE_TIMEOUT,
        )?;

        let concurrent_request_id = [0x39; 16];
        let mut concurrent = test_connect_control_pipe(&client_control_name)?;
        concurrent.write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionOpen {
                request: BrokerMcpSessionOpenRequest {
                    request_id: concurrent_request_id,
                    package_id: "filesystem".to_owned(),
                },
            })?,
            FRAME_WRITE_TIMEOUT,
        )?;
        match concurrent
            .read_frame(FRAME_WRITE_TIMEOUT)?
            .decode_control()?
        {
            BrokerControlMessage::Failure {
                request_id: Some(received),
                code: BrokerFailureCode::BackendUnavailable,
            } if received == concurrent_request_id => {}
            _ => return Err(io::Error::other("concurrent MCP open was not rejected")),
        }
        concurrent.write_frame(
            &BrokerFrame::control(&BrokerControlMessage::ExecutionAcknowledged {
                request_id: concurrent_request_id,
            })?,
            FRAME_WRITE_TIMEOUT,
        )?;

        session.write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionInputClosed { session_id })?,
            FRAME_WRITE_TIMEOUT,
        )?;

        let status_client = std::thread::spawn(move || -> io::Result<BrokerExecutionGateStatus> {
            test_connect_control_pipe(&client_control_name)?.execution_gate_status()
        });
        let status = status_client
            .join()
            .map_err(|_| io::Error::other("status client panicked"))??;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exited = None;
        while exited.is_none() {
            let frame = session.read_frame(Duration::from_secs(5))?;
            match frame.kind() {
                BrokerFrameKind::McpStdoutChunk => stdout.extend_from_slice(frame.payload()),
                BrokerFrameKind::McpStderrChunk => stderr.extend_from_slice(frame.payload()),
                BrokerFrameKind::Control => {
                    if let BrokerControlMessage::McpSessionExited {
                        session_id: received,
                        exit_code,
                    } = frame.decode_control()?
                    {
                        if received == session_id {
                            exited = Some(exit_code);
                        }
                    }
                }
                _ => return Err(io::Error::other("unexpected session frame")),
            }
        }
        if status != expected_status
            || stdout != b"rpc-bytes"
            || stderr != b"diagnostic"
            || exited != Some(Some(23))
        {
            return Err(io::Error::other("MCP stdio relay result did not match"));
        }
        client_done.send(()).unwrap();
        Ok(())
    });

    let session_connection = session_listener
        .accept_mcp_session(&cancellation, request_id, session_id)
        .unwrap();
    session_connection
        .write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionReady {
                request_id,
                session_id,
            })
            .unwrap(),
            FRAME_WRITE_TIMEOUT,
        )
        .unwrap();
    let mut guest = FakeGuest {
        input: Vec::new(),
        input_closed: false,
        output_emitted: false,
        exit_on_close: true,
        block_next_write: true,
        block_writes: false,
        first_write_attempt: None,
    };
    let status = pump_session(
        &session_connection,
        &mut control_listener,
        &cancellation,
        expected_status,
        &mut guest,
        session_id,
        &mut || Ok(McpSessionControlSignal::None),
    )
    .unwrap();
    assert_eq!(status.exit_code, Some(23));
    assert!(status.report_exit);
    session_connection
        .write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionExited {
                session_id,
                exit_code: status.exit_code,
            })
            .unwrap(),
            FRAME_WRITE_TIMEOUT,
        )
        .unwrap();
    client_result.recv_timeout(Duration::from_secs(5)).unwrap();
    client.join().unwrap().unwrap();
    assert_eq!(guest.input, [b"input-jsonrpc".to_vec()]);
    assert!(guest.input_closed);
}

#[test]
fn session_stop_is_read_after_stdin_eof_and_reports_no_natural_exit_code() {
    let request_id = [0x2a; 16];
    let session_id = [0x53; 16];
    let mut session_listener = test_mcp_session_listener(session_id).unwrap();
    let (mut control_listener, _control_name) = test_control_pipe().unwrap();
    let cancellation = BrokerPipeCancellation::new().unwrap();
    let client = std::thread::spawn(move || -> io::Result<Option<u32>> {
        let mut session = test_connect_mcp_session(session_id, request_id)?;
        match session
            .read_frame(Duration::from_secs(5))?
            .decode_control()?
        {
            BrokerControlMessage::McpSessionReady { .. } => {}
            _ => return Err(io::Error::other("MCP session was not marked ready")),
        }
        session.write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionInputClosed { session_id })?,
            FRAME_WRITE_TIMEOUT,
        )?;
        session.write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionStop { session_id })?,
            FRAME_WRITE_TIMEOUT,
        )?;
        loop {
            let frame = session.read_frame(Duration::from_secs(5))?;
            if let BrokerFrameKind::Control = frame.kind()
                && let BrokerControlMessage::McpSessionExited {
                    session_id: received,
                    exit_code,
                } = frame.decode_control()?
                && received == session_id
            {
                return Ok(exit_code);
            }
        }
    });
    let session_connection = session_listener
        .accept_mcp_session(&cancellation, request_id, session_id)
        .unwrap();
    session_connection
        .write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionReady {
                request_id,
                session_id,
            })
            .unwrap(),
            FRAME_WRITE_TIMEOUT,
        )
        .unwrap();
    let mut guest = FakeGuest {
        input: Vec::new(),
        input_closed: false,
        output_emitted: false,
        exit_on_close: false,
        block_next_write: false,
        block_writes: false,
        first_write_attempt: None,
    };
    let status = pump_session(
        &session_connection,
        &mut control_listener,
        &cancellation,
        BrokerExecutionGateStatus {
            quota: BrokerQuotaGateStatus::Attested,
            skill_python_runtime: BrokerSkillPythonRuntimeStatus::NotConfigured,
            mcp_node_runtime: BrokerMcpNodeRuntimeStatus::Attested,
        },
        &mut guest,
        session_id,
        &mut || Ok(McpSessionControlSignal::None),
    )
    .unwrap();
    assert!(status.report_exit);
    assert_eq!(status.exit_code, None);
    session_connection
        .write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionExited {
                session_id,
                exit_code: status.exit_code,
            })
            .unwrap(),
            FRAME_WRITE_TIMEOUT,
        )
        .unwrap();
    assert_eq!(client.join().unwrap().unwrap(), None);
    assert!(guest.input_closed);
}

#[test]
fn control_pipe_stop_interrupts_a_blocked_stdin_frame() {
    let request_id = [0x2b; 16];
    let session_id = [0x54; 16];
    let mut session_listener = test_mcp_session_listener(session_id).unwrap();
    let (mut control_listener, control_name) = test_control_pipe().unwrap();
    let cancellation = BrokerPipeCancellation::new().unwrap();
    let client = std::thread::spawn(move || -> io::Result<()> {
        let mut session = test_connect_mcp_session(session_id, request_id)?;
        match session
            .read_frame(Duration::from_secs(5))?
            .decode_control()?
        {
            BrokerControlMessage::McpSessionReady { .. } => {}
            _ => return Err(io::Error::other("MCP session was not marked ready")),
        }
        session.write_frame(
            &BrokerFrame::new(BrokerFrameKind::McpStdinChunk, b"blocked-input".to_vec())?,
            FRAME_WRITE_TIMEOUT,
        )?;
        loop {
            let frame = session.read_frame(Duration::from_secs(5))?;
            if frame.kind() == BrokerFrameKind::Control
                && let BrokerControlMessage::McpSessionExited {
                    session_id: received,
                    exit_code: None,
                } = frame.decode_control()?
                && received == session_id
            {
                return Ok(());
            }
        }
    });
    let (attempt_sender, attempt_receiver) = mpsc::channel();
    let stop_control_name = control_name.clone();
    let stop_client = std::thread::spawn(move || -> io::Result<()> {
        attempt_receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "stdin was not attempted"))?;
        let mut control = test_connect_control_pipe(&stop_control_name)?;
        control.write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionStop { session_id })?,
            FRAME_WRITE_TIMEOUT,
        )
    });

    let session_connection = session_listener
        .accept_mcp_session(&cancellation, request_id, session_id)
        .unwrap();
    session_connection
        .write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionReady {
                request_id,
                session_id,
            })
            .unwrap(),
            FRAME_WRITE_TIMEOUT,
        )
        .unwrap();
    let mut guest = FakeGuest {
        input: Vec::new(),
        input_closed: false,
        output_emitted: false,
        exit_on_close: false,
        block_next_write: false,
        block_writes: true,
        first_write_attempt: Some(attempt_sender),
    };
    let status = pump_session(
        &session_connection,
        &mut control_listener,
        &cancellation,
        BrokerExecutionGateStatus {
            quota: BrokerQuotaGateStatus::Attested,
            skill_python_runtime: BrokerSkillPythonRuntimeStatus::NotConfigured,
            mcp_node_runtime: BrokerMcpNodeRuntimeStatus::Attested,
        },
        &mut guest,
        session_id,
        &mut || Ok(McpSessionControlSignal::None),
    )
    .unwrap();
    assert!(status.report_exit);
    assert_eq!(status.exit_code, None);
    assert!(guest.input.is_empty());
    stop_client.join().unwrap().unwrap();
    session_connection
        .write_frame(
            &BrokerFrame::control(&BrokerControlMessage::McpSessionExited {
                session_id,
                exit_code: status.exit_code,
            })
            .unwrap(),
            FRAME_WRITE_TIMEOUT,
        )
        .unwrap();
    client.join().unwrap().unwrap();
}

#[test]
fn mcp_system_name_is_fixed_and_bound_to_the_broker_session_id() {
    let left = mcp_system_name([0xabu8; 16]);
    let right = mcp_system_name([0xcdu8; 16]);
    assert_eq!(left, format!("HavenMcp-{}", "ab".repeat(16)));
    assert_ne!(left, right);
}
