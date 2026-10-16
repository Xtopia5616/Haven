//! Authenticated client for one broker-owned MCP stdio generation.

use super::broker_protocol::{
    BrokerControlMessage, BrokerFailureCode, BrokerFrame, BrokerFrameKind,
    BrokerMcpSessionOpenRequest,
};
use super::named_pipe::{BrokerClientPolicy, BrokerPipeCancellation, BrokerPipeClient};
use super::{
    WindowsContainerMcpSession, WindowsContainerMcpSessionCallbacks,
    WindowsContainerMcpSessionEvent, WindowsContainerMcpSessionPort,
    WindowsContainerMcpSessionStopHandle, private,
};
use std::io;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(20);
const STARTUP_READ_INTERVAL: Duration = Duration::from_millis(20);
const STOP_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const STOP_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) struct BrokerMcpSessionProvider;

impl private::McpSessionSealed for BrokerMcpSessionProvider {}

impl WindowsContainerMcpSessionPort for BrokerMcpSessionProvider {
    fn open_session(
        &self,
        package_id: &str,
        callbacks: &mut WindowsContainerMcpSessionCallbacks<'_>,
    ) -> io::Result<Box<dyn WindowsContainerMcpSession>> {
        if (callbacks.is_cancelled)() {
            return Err(interrupted("MCP broker launch was cancelled"));
        }
        (callbacks.before_session_start)()?;

        let control_cancellation = BrokerPipeCancellation::new()?;
        let (policy, request_id, session_id) = with_cancellation_watcher(
            callbacks.is_cancelled,
            control_cancellation.clone(),
            || {
                let policy = BrokerClientPolicy::for_current_process()
                    .map_err(|_| permission_denied("MCP broker client identity is invalid"))?;
                let mut control = BrokerPipeClient::connect(&policy, control_cancellation.clone())?;
                let request_id = uuid::Uuid::new_v4().into_bytes();
                let open = BrokerControlMessage::McpSessionOpen {
                    request: BrokerMcpSessionOpenRequest {
                        request_id,
                        package_id: package_id.to_owned(),
                    },
                };
                control.write_frame(&BrokerFrame::control(&open)?, HANDSHAKE_TIMEOUT)?;
                let session_id = match control.read_frame(HANDSHAKE_TIMEOUT)?.decode_control()? {
                    BrokerControlMessage::McpSessionOpenAccepted {
                        request_id: received_request,
                        session_id,
                    } if received_request == request_id => session_id,
                    BrokerControlMessage::Failure {
                        request_id: Some(received_request),
                        code,
                    } if received_request == request_id => return Err(map_failure(code)),
                    _ => {
                        return Err(invalid_data(
                            "MCP broker did not accept the session request",
                        ));
                    }
                };
                drop(control);
                Ok((policy, request_id, session_id))
            },
        )?;

        if (callbacks.is_cancelled)() {
            return Err(interrupted("MCP broker launch was cancelled before attach"));
        }

        // Session-pipe cancellation must not be wired to the launch token:
        // after attach, disconnecting this pipe would hide the broker's
        // stop-proof event and could release the start permit too early.
        let session_cancellation = BrokerPipeCancellation::new()?;
        let mut session =
            BrokerPipeClient::connect_mcp_session_pipe(&policy, session_id, session_cancellation)?;
        if (callbacks.is_cancelled)() {
            return Err(interrupted("MCP broker launch was cancelled before attach"));
        }

        let stop_handle = Arc::new(BrokerMcpSessionStopHandle { policy, session_id });
        if let Err(error) = session.attach_mcp_session(request_id, session_id) {
            // Attach is the point after which a complete frame could have
            // reached the broker. Best-effort stop covers an ambiguous write;
            // never treat this path as a confirmed stop.
            let _ = stop_handle.request_stop();
            return Err(error);
        }

        await_session_startup(
            stop_handle.as_ref(),
            callbacks.is_cancelled,
            callbacks.session_started,
            |timeout| read_startup_event(&mut session, request_id, session_id, timeout),
        )?;

        Ok(Box::new(BrokerMcpSession {
            client: session,
            session_id,
            stop_handle,
        }) as Box<dyn WindowsContainerMcpSession>)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupEvent {
    Ready,
    Exited,
    Failure(BrokerFailureCode),
}

fn read_startup_event(
    session: &mut BrokerPipeClient,
    request_id: [u8; 16],
    session_id: [u8; 16],
    timeout: Duration,
) -> io::Result<StartupEvent> {
    let frame = session.read_frame(timeout)?;
    match frame.kind() {
        BrokerFrameKind::Control => match frame.decode_control()? {
            BrokerControlMessage::McpSessionReady {
                request_id: received_request,
                session_id: received_session,
            } if received_request == request_id && received_session == session_id => {
                Ok(StartupEvent::Ready)
            }
            BrokerControlMessage::McpSessionExited {
                session_id: received_session,
                ..
            } if received_session == session_id => Ok(StartupEvent::Exited),
            BrokerControlMessage::Failure {
                request_id: Some(received_request),
                code,
            } if received_request == request_id => Ok(StartupEvent::Failure(code)),
            _ => Err(invalid_data("MCP broker sent an invalid startup frame")),
        },
        _ => Err(invalid_data("MCP broker sent a non-control startup frame")),
    }
}

fn await_session_startup(
    stop_handle: &dyn WindowsContainerMcpSessionStopHandle,
    is_cancelled: &mut (dyn FnMut() -> bool + Send),
    process_started: &mut dyn FnMut(),
    mut read_event: impl FnMut(Duration) -> io::Result<StartupEvent>,
) -> io::Result<()> {
    let mut cancellation_seen = false;
    let mut confirmation_deadline = None;
    let mut next_stop_attempt = None;
    let mut stop_delivered = false;

    loop {
        if (is_cancelled)() && !cancellation_seen {
            cancellation_seen = true;
            let now = std::time::Instant::now();
            confirmation_deadline = Some(now + STOP_CONFIRMATION_TIMEOUT);
            next_stop_attempt = Some(now);
        }

        if cancellation_seen {
            let now = std::time::Instant::now();
            if confirmation_deadline.is_some_and(|deadline| now >= deadline) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "MCP broker did not confirm stopped-session cleanup",
                ));
            }
            if !stop_delivered && next_stop_attempt.is_some_and(|deadline| now >= deadline) {
                // Retry transient control-pipe failures, but once the frame is
                // written keep the pipe open and wait without duplicating Stop.
                match stop_handle.request_stop() {
                    Ok(()) => stop_delivered = true,
                    Err(_) => next_stop_attempt = Some(now + STOP_RETRY_INTERVAL),
                }
            }
        }

        match read_event(STARTUP_READ_INTERVAL) {
            Ok(StartupEvent::Ready) if !cancellation_seen => {
                if (is_cancelled)() {
                    continue;
                }
                process_started();
                return Ok(());
            }
            Ok(StartupEvent::Ready) => {
                // The guest became ready after cancellation. Keep the pipe
                // open and wait for Exited after the independent Stop request.
            }
            Ok(StartupEvent::Exited) if cancellation_seen => {
                return Err(interrupted(
                    "MCP broker stopped and cleaned the cancelled session",
                ));
            }
            Ok(StartupEvent::Exited) => {
                return Err(io::Error::other(
                    "MCP broker session exited before becoming ready",
                ));
            }
            Ok(StartupEvent::Failure(code)) => return Err(map_failure(code)),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => {
                if cancellation_seen {
                    let _ = stop_handle.request_stop();
                }
                return Err(error);
            }
        }
    }
}

struct BrokerMcpSession {
    client: BrokerPipeClient,
    session_id: [u8; 16],
    stop_handle: Arc<BrokerMcpSessionStopHandle>,
}

impl WindowsContainerMcpSession for BrokerMcpSession {
    fn write_stdin_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        let frame = BrokerFrame::new(BrokerFrameKind::McpStdinChunk, bytes.to_vec())?;
        self.client.write_frame(&frame, HANDSHAKE_TIMEOUT)
    }

    fn close_stdin(&mut self) -> io::Result<()> {
        let message = BrokerControlMessage::McpSessionInputClosed {
            session_id: self.session_id,
        };
        self.client
            .write_frame(&BrokerFrame::control(&message)?, HANDSHAKE_TIMEOUT)
    }

    fn read_event(&mut self, timeout: Duration) -> io::Result<WindowsContainerMcpSessionEvent> {
        let frame = self.client.read_frame(timeout)?;
        match frame.kind() {
            BrokerFrameKind::McpStdoutChunk => Ok(WindowsContainerMcpSessionEvent::Stdout(
                frame.payload().to_vec(),
            )),
            BrokerFrameKind::McpStderrChunk => Ok(WindowsContainerMcpSessionEvent::Stderr(
                frame.payload().to_vec(),
            )),
            BrokerFrameKind::Control => match frame.decode_control()? {
                BrokerControlMessage::McpSessionExited {
                    session_id,
                    exit_code,
                } if session_id == self.session_id => {
                    Ok(WindowsContainerMcpSessionEvent::Exited(exit_code))
                }
                BrokerControlMessage::Failure { code, .. } => Err(map_failure(code)),
                _ => Err(invalid_data("MCP broker sent an invalid session event")),
            },
            _ => Err(invalid_data("MCP broker sent an unexpected session frame")),
        }
    }

    fn stop_handle(&self) -> Arc<dyn WindowsContainerMcpSessionStopHandle> {
        self.stop_handle.clone()
    }
}

struct BrokerMcpSessionStopHandle {
    policy: BrokerClientPolicy,
    session_id: [u8; 16],
}

impl WindowsContainerMcpSessionStopHandle for BrokerMcpSessionStopHandle {
    fn request_stop(&self) -> io::Result<()> {
        let cancellation = BrokerPipeCancellation::new()?;
        let mut control = BrokerPipeClient::connect(&self.policy, cancellation)?;
        let message = BrokerControlMessage::McpSessionStop {
            session_id: self.session_id,
        };
        control.write_frame(&BrokerFrame::control(&message)?, STOP_TIMEOUT)
    }
}

fn with_cancellation_watcher<T>(
    is_cancelled: &mut (dyn FnMut() -> bool + Send),
    cancellation: BrokerPipeCancellation,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let stop = std::sync::atomic::AtomicBool::new(false);
    thread::scope(|scope| {
        let watcher_cancellation = cancellation.clone();
        let stop_ref = &stop;
        let cancellation_check = &mut *is_cancelled;
        let watcher = scope.spawn(move || {
            while !stop_ref.load(std::sync::atomic::Ordering::Acquire) {
                if cancellation_check() {
                    let _ = watcher_cancellation.cancel();
                    break;
                }
                thread::sleep(CANCELLATION_POLL_INTERVAL);
            }
        });

        struct StopOnDrop<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
        }

        let stop_guard = StopOnDrop(&stop);
        let result = operation();
        drop(stop_guard);
        watcher
            .join()
            .map_err(|_| io::Error::other("MCP broker cancellation monitor failed"))?;
        result
    })
}

fn map_failure(code: BrokerFailureCode) -> io::Error {
    let (kind, message) = match code {
        BrokerFailureCode::UnsupportedProtocolVersion => (
            io::ErrorKind::Unsupported,
            "MCP broker protocol version is unsupported",
        ),
        BrokerFailureCode::BackendUnavailable => (
            io::ErrorKind::NotConnected,
            "MCP broker backend is unavailable",
        ),
        BrokerFailureCode::UnauthorizedPeer => (
            io::ErrorKind::PermissionDenied,
            "MCP broker rejected the local peer",
        ),
        BrokerFailureCode::InvalidRequest => (
            io::ErrorKind::InvalidInput,
            "MCP broker rejected the session request",
        ),
        BrokerFailureCode::PolicyRevoked | BrokerFailureCode::Cancelled => (
            io::ErrorKind::Interrupted,
            "MCP broker session was cancelled",
        ),
        BrokerFailureCode::TimedOut => (io::ErrorKind::TimedOut, "MCP broker session timed out"),
        BrokerFailureCode::QuotaUnattested | BrokerFailureCode::McpPackageUnattested => (
            io::ErrorKind::PermissionDenied,
            "MCP broker package or storage quota is not verified",
        ),
        BrokerFailureCode::SkillRuntimeUnattested => (
            io::ErrorKind::PermissionDenied,
            "MCP broker rejected the runtime proof",
        ),
        BrokerFailureCode::StagingFailed | BrokerFailureCode::HcsFailed => (
            io::ErrorKind::Other,
            "MCP broker could not start or stage the guest",
        ),
        BrokerFailureCode::CleanupFailed => (
            io::ErrorKind::Other,
            "MCP broker could not safely clean the session",
        ),
    };
    io::Error::new(kind, message)
}

fn interrupted(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn permission_denied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct RecordingStop(AtomicUsize);

    impl WindowsContainerMcpSessionStopHandle for RecordingStop {
        fn request_stop(&self) -> io::Result<()> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    #[test]
    fn cancelled_startup_sends_stop_and_waits_for_cleanup_exit() {
        let stop = RecordingStop::default();
        let mut started = false;
        let mut is_cancelled = || true;
        let mut events = VecDeque::from([StartupEvent::Ready, StartupEvent::Exited]);

        let error = await_session_startup(&stop, &mut is_cancelled, &mut || started = true, |_| {
            Ok(events.pop_front().expect("test provides startup events"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(stop.0.load(Ordering::Acquire), 1);
        assert!(
            !started,
            "a cancelled generation must not be published ready"
        );
    }

    #[test]
    fn startup_publishes_ready_only_after_uncancelled_ready_frame() {
        let stop = RecordingStop::default();
        let mut started = false;
        let mut is_cancelled = || false;
        let mut events = VecDeque::from([StartupEvent::Ready]);

        await_session_startup(&stop, &mut is_cancelled, &mut || started = true, |_| {
            Ok(events.pop_front().expect("test provides startup event"))
        })
        .unwrap();

        assert_eq!(stop.0.load(Ordering::Acquire), 0);
        assert!(started);
    }

    #[test]
    fn cancellation_at_ready_boundary_stops_before_publishing_started() {
        let stop = RecordingStop::default();
        let mut started = false;
        let calls = AtomicUsize::new(0);
        let mut is_cancelled = || calls.fetch_add(1, Ordering::AcqRel) >= 1;
        let mut events = VecDeque::from([StartupEvent::Ready, StartupEvent::Exited]);

        let error = await_session_startup(&stop, &mut is_cancelled, &mut || started = true, |_| {
            Ok(events.pop_front().expect("test provides startup events"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(stop.0.load(Ordering::Acquire), 1);
        assert!(!started);
    }
}
