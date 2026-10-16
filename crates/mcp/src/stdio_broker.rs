//! Adapt the platform's framed broker session to MCP's async stdio process port.

use crate::{
    McpStdioLaunchSpec, McpStdioProcess, McpStdioProcessLauncher, McpStdioProcessStartPermit,
    McpStdioReader, McpStdioWriter,
};
use futures_util::future::BoxFuture;
use haven_common::config::McpStdioExecution;
use haven_platform::windows_container::execution::{
    WindowsContainerMcpSession, WindowsContainerMcpSessionCallbacks,
    WindowsContainerMcpSessionEvent, WindowsContainerMcpSessionPort,
    WindowsContainerMcpSessionStopHandle,
};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const DUPLEX_CAPACITY_BYTES: usize = 64 * 1024;
const SESSION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SESSION_READ_TIMEOUT: Duration = Duration::from_millis(10);
const STOP_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const STOP_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(60);

struct BrokerGenerationContext {
    worker_stdin: DuplexStream,
    worker_stdout: DuplexStream,
    worker_stderr: DuplexStream,
    cancellation: CancellationToken,
    alive: Arc<AtomicBool>,
    ready_sender: oneshot::Sender<Result<(), String>>,
}

pub(crate) struct BrokeredMcpStdioProcessLauncher {
    port: Arc<dyn WindowsContainerMcpSessionPort>,
}

impl BrokeredMcpStdioProcessLauncher {
    pub(crate) fn new(port: Arc<dyn WindowsContainerMcpSessionPort>) -> Self {
        Self { port }
    }
}

impl McpStdioProcessLauncher for BrokeredMcpStdioProcessLauncher {
    fn launch<'a>(
        &'a self,
        spec: &'a McpStdioLaunchSpec,
        permit: Box<dyn McpStdioProcessStartPermit>,
    ) -> BoxFuture<'a, anyhow::Result<Box<dyn McpStdioProcess>>> {
        Box::pin(async move {
            let McpStdioExecution::ManagedPackage { package_id } = spec.stdio_execution() else {
                anyhow::bail!("brokered MCP launcher requires a managed package profile");
            };

            let (stdin_writer, worker_stdin) = tokio::io::duplex(DUPLEX_CAPACITY_BYTES);
            let (worker_stdout, stdout_reader) = tokio::io::duplex(DUPLEX_CAPACITY_BYTES);
            let (worker_stderr, stderr_reader) = tokio::io::duplex(DUPLEX_CAPACITY_BYTES);
            let cancellation = CancellationToken::new();
            let worker_cancellation = cancellation.clone();
            let alive = Arc::new(AtomicBool::new(false));
            let worker_alive = alive.clone();
            let (ready_sender, ready_receiver) = oneshot::channel::<Result<(), String>>();
            let port = self.port.clone();
            let package_id = package_id.clone();
            let context = BrokerGenerationContext {
                worker_stdin,
                worker_stdout,
                worker_stderr,
                cancellation: worker_cancellation,
                alive: worker_alive,
                ready_sender,
            };
            let worker = tokio::task::spawn_blocking(move || {
                run_broker_generation(port, package_id, permit, context)
            });
            let mut cancellation_guard = LaunchCancellationGuard::new(cancellation.clone());
            ready_receiver
                .await
                .map_err(|_| anyhow::anyhow!("MCP broker launch ended before session readiness"))?
                .map_err(|error| anyhow::anyhow!("MCP broker session launch failed: {error}"))?;
            cancellation_guard.disarm();

            Ok(Box::new(BrokeredMcpStdioProcess {
                stdin_writer: Some(stdin_writer),
                stdout_reader: Some(stdout_reader),
                stderr_reader: Some(stderr_reader),
                cancellation,
                alive,
                worker: Some(worker),
            }) as Box<dyn McpStdioProcess>)
        })
    }
}

fn run_broker_generation(
    port: Arc<dyn WindowsContainerMcpSessionPort>,
    package_id: String,
    permit: Box<dyn McpStdioProcessStartPermit>,
    context: BrokerGenerationContext,
) -> io::Result<()> {
    let BrokerGenerationContext {
        worker_stdin,
        worker_stdout,
        worker_stderr,
        cancellation,
        alive,
        ready_sender,
    } = context;
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready_sender.send(Err("MCP session relay runtime is unavailable".into()));
            return Err(io::Error::other(error));
        }
    };

    let mut before_start = || {
        if cancellation.is_cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "MCP broker launch was cancelled",
            ))
        } else {
            Ok(())
        }
    };
    let worker_alive = alive.clone();
    let mut session_started = move || worker_alive.store(true, Ordering::Release);
    let mut is_cancelled = || cancellation.is_cancelled();
    let mut callbacks = WindowsContainerMcpSessionCallbacks::new(
        &mut before_start,
        &mut session_started,
        &mut is_cancelled,
    );

    // Keep the Tools-owned policy read permit until the broker confirms the
    // fixed guest process is ready. Every error path releases it on return.
    let session = match port.open_session(&package_id, &mut callbacks) {
        Ok(session) => session,
        Err(error) => {
            let safe_error = haven_common::error::sanitize_error_text(&error.to_string());
            let _ = ready_sender.send(Err(safe_error));
            return Err(error);
        }
    };
    drop(permit);
    let stop_handle = session.stop_handle();
    let _ = ready_sender.send(Ok(()));

    let alive_guard = AliveGuard(alive);
    runtime.block_on(relay_session(
        session,
        worker_stdin,
        worker_stdout,
        worker_stderr,
        cancellation,
        stop_handle,
    ))?;
    drop(alive_guard);
    Ok(())
}

async fn relay_session(
    mut session: Box<dyn WindowsContainerMcpSession>,
    mut stdin: DuplexStream,
    mut stdout: DuplexStream,
    mut stderr: DuplexStream,
    cancellation: CancellationToken,
    stop_handle: Arc<dyn WindowsContainerMcpSessionStopHandle>,
) -> io::Result<()> {
    let mut input = vec![0u8; 16 * 1024];
    let mut input_closed = false;
    let mut cancellation_observed = false;
    let mut stop_delivered = false;
    let mut next_stop_attempt = None;
    let mut stop_confirmation_deadline = None;
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let mut poll = tokio::time::interval(SESSION_POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if cancellation_observed && !stop_delivered {
            let now = std::time::Instant::now();
            if stop_confirmation_deadline.is_some_and(|deadline| now >= deadline) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "MCP broker did not confirm stopped-session cleanup",
                ));
            }
            if next_stop_attempt.is_some_and(|deadline| now >= deadline) {
                let stop_handle = stop_handle.clone();
                let stop_result = tokio::task::spawn_blocking(move || stop_handle.request_stop())
                    .await
                    .map_err(|error| {
                        io::Error::other(format!("MCP stop worker failed: {error}"))
                    })?;
                if stop_result.is_ok() {
                    stop_delivered = true;
                } else {
                    next_stop_attempt = Some(now + STOP_RETRY_INTERVAL);
                }
            }
        }

        tokio::select! {
            biased;
            _ = cancellation.cancelled(), if !cancellation_observed => {
                cancellation_observed = true;
                stdout_closed = true;
                stderr_closed = true;
                let now = std::time::Instant::now();
                next_stop_attempt = Some(now);
                stop_confirmation_deadline = Some(now + STOP_CONFIRMATION_TIMEOUT);
                // The next loop iteration sends Stop through the independent
                // control pipe, then keeps this pipe open until cleanup proof.
            }
            read = stdin.read(&mut input), if !input_closed && !cancellation_observed => {
                match read? {
                    0 => {
                        session.close_stdin()?;
                        input_closed = true;
                    }
                    count => session.write_stdin_chunk(&input[..count])?,
                }
            }
            _ = poll.tick() => {
                match session.read_event(SESSION_READ_TIMEOUT) {
                    Ok(WindowsContainerMcpSessionEvent::Stdout(bytes)) if !stdout_closed => {
                        if !write_output(&mut stdout, &bytes, &cancellation).await? {
                            stdout_closed = true;
                        }
                    }
                    Ok(WindowsContainerMcpSessionEvent::Stderr(bytes)) if !stderr_closed => {
                        if !write_output(&mut stderr, &bytes, &cancellation).await? {
                            stderr_closed = true;
                        }
                    }
                    Ok(WindowsContainerMcpSessionEvent::Stdout(_))
                    | Ok(WindowsContainerMcpSessionEvent::Stderr(_)) => {}
                    Ok(WindowsContainerMcpSessionEvent::Exited(_)) => return Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

async fn write_output(
    stream: &mut DuplexStream,
    bytes: &[u8],
    cancellation: &CancellationToken,
) -> io::Result<bool> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Ok(false),
        result = stream.write_all(bytes) => result.map(|()| true),
    }
}

struct BrokeredMcpStdioProcess {
    stdin_writer: Option<DuplexStream>,
    stdout_reader: Option<DuplexStream>,
    stderr_reader: Option<DuplexStream>,
    cancellation: CancellationToken,
    alive: Arc<AtomicBool>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl McpStdioProcess for BrokeredMcpStdioProcess {
    fn take_stdin(&mut self) -> io::Result<McpStdioWriter> {
        self.stdin_writer
            .take()
            .map(|stream| Box::new(stream) as McpStdioWriter)
            .ok_or_else(|| io::Error::other("MCP stdio stdin was already taken"))
    }

    fn take_stdout(&mut self) -> io::Result<McpStdioReader> {
        self.stdout_reader
            .take()
            .map(|stream| Box::new(stream) as McpStdioReader)
            .ok_or_else(|| io::Error::other("MCP stdio stdout was already taken"))
    }

    fn take_stderr(&mut self) -> io::Result<Option<McpStdioReader>> {
        Ok(self
            .stderr_reader
            .take()
            .map(|stream| Box::new(stream) as McpStdioReader))
    }

    fn is_alive(&mut self) -> io::Result<bool> {
        Ok(self.alive.load(Ordering::Acquire))
    }

    fn start_kill(&mut self) -> io::Result<()> {
        self.request_stop()
    }

    fn wait(&mut self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let result = match self.worker.as_mut() {
                Some(worker) => worker.await.map_err(|error| {
                    io::Error::other(format!("MCP broker relay failed: {error}"))
                })?,
                None => return Ok(()),
            };
            self.worker.take();
            result
        })
    }
}

impl BrokeredMcpStdioProcess {
    fn request_stop(&mut self) -> io::Result<()> {
        if !self.alive.load(Ordering::Acquire) {
            return Ok(());
        }
        // The relay owns the session and its independent stop capability. A
        // single cancellation path keeps Stop serialized with Exited draining.
        self.cancellation.cancel();
        Ok(())
    }
}

impl Drop for BrokeredMcpStdioProcess {
    fn drop(&mut self) {
        let _ = self.request_stop();
    }
}

struct LaunchCancellationGuard {
    cancellation: CancellationToken,
    armed: bool,
}

impl LaunchCancellationGuard {
    fn new(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for LaunchCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

struct AliveGuard(Arc<AtomicBool>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_platform::windows_container::execution::WindowsContainerMcpSessionStopHandle;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct RecordedState {
        stdin: Mutex<Vec<Vec<u8>>>,
        stdin_closed: AtomicBool,
        events: Mutex<VecDeque<WindowsContainerMcpSessionEvent>>,
        stop: Arc<RecordedStop>,
    }

    struct RecordedSession(Arc<RecordedState>);

    impl WindowsContainerMcpSession for RecordedSession {
        fn write_stdin_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.0
                .stdin
                .lock()
                .map_err(|_| io::Error::other("recording lock poisoned"))?
                .push(bytes.to_vec());
            Ok(())
        }

        fn close_stdin(&mut self) -> io::Result<()> {
            self.0.stdin_closed.store(true, Ordering::Release);
            Ok(())
        }

        fn read_event(
            &mut self,
            _timeout: Duration,
        ) -> io::Result<WindowsContainerMcpSessionEvent> {
            self.0
                .events
                .lock()
                .map_err(|_| io::Error::other("recording lock poisoned"))?
                .pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "no event"))
        }

        fn stop_handle(&self) -> Arc<dyn WindowsContainerMcpSessionStopHandle> {
            self.0.stop.clone()
        }
    }

    #[derive(Default)]
    struct RecordedStop(AtomicBool);

    impl WindowsContainerMcpSessionStopHandle for RecordedStop {
        fn request_stop(&self) -> io::Result<()> {
            self.0.store(true, Ordering::Release);
            Ok(())
        }
    }

    struct FailOnceStop(AtomicUsize);

    impl WindowsContainerMcpSessionStopHandle for FailOnceStop {
        fn request_stop(&self) -> io::Result<()> {
            if self.0.fetch_add(1, Ordering::AcqRel) == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "temporary control-pipe failure",
                ));
            }
            Ok(())
        }
    }

    struct RetrySession {
        remaining_timeouts: usize,
        stop: Arc<FailOnceStop>,
    }

    impl WindowsContainerMcpSession for RetrySession {
        fn write_stdin_chunk(&mut self, _bytes: &[u8]) -> io::Result<()> {
            Ok(())
        }

        fn close_stdin(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn read_event(
            &mut self,
            _timeout: Duration,
        ) -> io::Result<WindowsContainerMcpSessionEvent> {
            if self.remaining_timeouts > 0 {
                self.remaining_timeouts -= 1;
                return Err(io::Error::new(io::ErrorKind::TimedOut, "not exited yet"));
            }
            Ok(WindowsContainerMcpSessionEvent::Exited(None))
        }

        fn stop_handle(&self) -> Arc<dyn WindowsContainerMcpSessionStopHandle> {
            self.stop.clone()
        }
    }

    #[tokio::test]
    async fn relay_forwards_stdio_and_preserves_broker_exit_boundary() {
        let state = Arc::new(RecordedState::default());
        state.events.lock().unwrap().extend([
            WindowsContainerMcpSessionEvent::Stdout(b"rpc-response".to_vec()),
            WindowsContainerMcpSessionEvent::Stderr(b"diagnostic".to_vec()),
            WindowsContainerMcpSessionEvent::Exited(None),
        ]);
        let (mut caller_stdin, worker_stdin) = tokio::io::duplex(1024);
        let (worker_stdout, mut caller_stdout) = tokio::io::duplex(1024);
        let (worker_stderr, mut caller_stderr) = tokio::io::duplex(1024);

        caller_stdin.write_all(b"rpc-request").await.unwrap();
        caller_stdin.shutdown().await.unwrap();
        relay_session(
            Box::new(RecordedSession(state.clone())),
            worker_stdin,
            worker_stdout,
            worker_stderr,
            CancellationToken::new(),
            state.stop.clone(),
        )
        .await
        .unwrap();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        caller_stdout.read_to_end(&mut stdout).await.unwrap();
        caller_stderr.read_to_end(&mut stderr).await.unwrap();
        assert_eq!(stdout, b"rpc-response");
        assert_eq!(stderr, b"diagnostic");
        assert_eq!(*state.stdin.lock().unwrap(), [b"rpc-request".to_vec()]);
        assert!(state.stdin_closed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn relay_keeps_reading_until_broker_exit_after_cancellation() {
        let state = Arc::new(RecordedState::default());
        state
            .events
            .lock()
            .unwrap()
            .push_back(WindowsContainerMcpSessionEvent::Exited(None));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (_caller_stdin, worker_stdin) = tokio::io::duplex(1024);
        let (worker_stdout, _caller_stdout) = tokio::io::duplex(1024);
        let (worker_stderr, _caller_stderr) = tokio::io::duplex(1024);

        relay_session(
            Box::new(RecordedSession(state.clone())),
            worker_stdin,
            worker_stdout,
            worker_stderr,
            cancellation,
            state.stop.clone(),
        )
        .await
        .unwrap();
        assert!(state.stop.0.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn relay_retries_a_failed_stop_request_without_dropping_session_pipe() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (_caller_stdin, worker_stdin) = tokio::io::duplex(1024);
        let (worker_stdout, _caller_stdout) = tokio::io::duplex(1024);
        let (worker_stderr, _caller_stderr) = tokio::io::duplex(1024);
        let stop = Arc::new(FailOnceStop(AtomicUsize::default()));
        relay_session(
            Box::new(RetrySession {
                remaining_timeouts: 30,
                stop: stop.clone(),
            }),
            worker_stdin,
            worker_stdout,
            worker_stderr,
            cancellation,
            stop.clone(),
        )
        .await
        .unwrap();

        assert!(stop.0.load(Ordering::Acquire) >= 2);
    }
}
