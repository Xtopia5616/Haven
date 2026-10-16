//! Bounded writes to an HCS guest stdin pipe.
//!
//! HCS supplies a synchronous pipe handle. A single private writer thread
//! keeps a full guest pipe from blocking the caller's stdout/stderr drain and
//! process supervision loop. The adapter accepts at most four 16 KiB chunks
//! total: one possibly blocked write and three queued chunks.

use super::hcs_process_runtime::HcsGuestProcessOwner;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::Threading::{GetCurrentThreadId, OpenThread, THREAD_TERMINATE};

const INPUT_CHUNK_BYTES: usize = 16 * 1024;
const QUEUED_INPUT_CHUNKS: usize = 3;
const WORKER_START_TIMEOUT: Duration = Duration::from_secs(1);
const WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One HCS stdin writer that owns the process so its pipes and termination
/// boundary remain alive for the full adapter lifetime.
pub(super) struct HcsGuestProcessInputWriter<'api> {
    process: HcsGuestProcessOwner<'api>,
    sender: Option<SyncSender<Vec<u8>>>,
    result_receiver: Receiver<io::Result<()>>,
    worker: Option<JoinHandle<()>>,
    worker_thread: Option<HANDLE>,
    input_closed: bool,
    worker_detached: bool,
    worker_error: Option<io::ErrorKind>,
    error_reported: bool,
    failed: bool,
}

impl<'api> HcsGuestProcessInputWriter<'api> {
    pub(super) fn new(mut process: HcsGuestProcessOwner<'api>) -> io::Result<Self> {
        let stdin = process.take_stdin().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCS guest process has no available stdin pipe",
            )
        })?;
        let stdin = stdin.into_file()?;
        let (sender, receiver) = mpsc::sync_channel(QUEUED_INPUT_CHUNKS);
        let (thread_id_sender, thread_id_receiver) = mpsc::sync_channel(1);
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("haven-hcs-guest-stdin".to_owned())
            .spawn(move || {
                // The thread ID is published before the worker can enter a
                // blocking WriteFile call, so cancellation always targets the
                // exact thread that owns this synchronous pipe writer.
                let thread_id = unsafe { GetCurrentThreadId() };
                let _ = thread_id_sender.send(thread_id);
                let result = write_chunks(stdin, receiver);
                let _ = result_sender.send(result);
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(_) => {
                let _ = process.terminate();
                return Err(io::Error::other("HCS guest stdin writer could not start"));
            }
        };

        let thread_id = match thread_id_receiver.recv_timeout(WORKER_START_TIMEOUT) {
            Ok(thread_id) if thread_id != 0 => thread_id,
            _ => {
                drop(sender);
                let _ = process.terminate();
                let _ = worker.join();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "HCS guest stdin writer did not start in time",
                ));
            }
        };
        let worker_thread = unsafe { OpenThread(THREAD_TERMINATE, 0, thread_id) };
        if worker_thread.is_null() {
            let error = io::Error::last_os_error();
            drop(sender);
            let _ = process.terminate();
            let _ = worker.join();
            return Err(io::Error::new(
                error.kind(),
                "HCS guest stdin writer could not be supervised",
            ));
        }

        Ok(Self {
            process,
            sender: Some(sender),
            result_receiver,
            worker: Some(worker),
            worker_thread: Some(worker_thread),
            input_closed: false,
            worker_detached: false,
            worker_error: None,
            error_reported: false,
            failed: false,
        })
    }

    /// Queue one copied chunk without waiting for the guest to read it.
    /// `WouldBlock` means the caller must stop reading upstream until a later
    /// poll frees capacity. No accepted chunk exceeds 16 KiB.
    pub(super) fn try_write_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.input_closed || self.sender.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "HCS guest stdin is closed",
            ));
        }
        if bytes.len() > INPUT_CHUNK_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCS guest stdin chunk exceeds its byte bound",
            ));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let mut chunk = Vec::new();
        chunk.try_reserve_exact(bytes.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                "HCS guest stdin chunk allocation failed",
            )
        })?;
        chunk.extend_from_slice(bytes);

        match self.sender.as_ref().expect("checked above").try_send(chunk) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "HCS guest stdin buffer is full",
            )),
            Err(TrySendError::Disconnected(_)) => {
                self.failed = true;
                self.sender.take();
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "HCS guest stdin writer has stopped",
                ))
            }
        }
    }

    /// Stop accepting input. The worker drains accepted chunks, then closes
    /// its `File`, which delivers EOF to the guest.
    pub(super) fn close_input(&mut self) {
        self.sender.take();
    }

    /// Poll worker completion without blocking the caller's supervision loop.
    /// Returns true only after the worker has exited and the pipe was closed.
    pub(super) fn poll(&mut self) -> io::Result<bool> {
        if self.worker_detached {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HCS guest stdin writer exit could not be confirmed",
            ));
        }
        if !self.input_closed && self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            self.collect_finished_worker();
        }
        if self.input_closed {
            if let Some(kind) = self.worker_error
                && !self.error_reported
            {
                self.error_reported = true;
                return Err(io::Error::new(kind, "HCS guest stdin write failed"));
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Poll guest exit while the adapter keeps the process owner borrowed.
    pub(super) fn poll_process_exit(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<super::hcs_process_runtime::GuestProcessExitStatus>> {
        self.process.poll_exit(timeout)
    }

    /// Request process exit, cancel a pending synchronous write, and wait at
    /// most two seconds for the writer thread. If Windows does not complete
    /// cancellation, the thread is detached while it retains ownership of its
    /// own pipe `File`; no live I/O handle is closed from underneath it.
    pub(super) fn abort(&mut self) -> io::Result<()> {
        if self.input_closed && !self.failed {
            return Ok(());
        }
        self.close_input();
        let termination_result = self.process.terminate();
        if self.worker_detached {
            return termination_result.and_then(|()| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "HCS guest stdin writer exit could not be confirmed",
                ))
            });
        }
        let deadline = Instant::now() + WORKER_STOP_TIMEOUT;
        loop {
            if self
                .worker
                .as_ref()
                .is_none_or(|worker| worker.is_finished())
            {
                if self.worker.is_none() {
                    return termination_result;
                }
                self.collect_finished_worker();
                return termination_result;
            }
            if let Some(worker_thread) = self.worker_thread {
                // Cancellation can miss a transition between writes; retry
                // until the channel is drained or the deadline expires.
                let _ = unsafe { CancelSynchronousIo(worker_thread) };
            }
            if Instant::now() >= deadline {
                self.failed = true;
                self.worker.take(); // Detach; the worker still owns its File.
                self.close_worker_thread_handle();
                self.worker_detached = true;
                return termination_result.and_then(|()| {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "HCS guest stdin writer did not stop within its bound",
                    ))
                });
            }
            thread::sleep(WORKER_POLL_INTERVAL);
        }
    }

    fn collect_finished_worker(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        if worker.join().is_err() {
            self.worker_error = Some(io::ErrorKind::Other);
            self.failed = true;
        }
        self.close_worker_thread_handle();
        self.sender.take();
        self.input_closed = true;
        match self.result_receiver.try_recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.worker_error = Some(error.kind());
                self.failed = true;
            }
            Err(_) => {
                self.worker_error = Some(io::ErrorKind::Other);
                self.failed = true;
            }
        }
    }

    fn close_worker_thread_handle(&mut self) {
        if let Some(thread) = self.worker_thread.take() {
            unsafe { CloseHandle(thread) };
        }
    }
}

impl Drop for HcsGuestProcessInputWriter<'_> {
    fn drop(&mut self) {
        if !self.input_closed || self.failed {
            let _ = self.abort();
        }
        self.close_worker_thread_handle();
    }
}

fn write_chunks(mut stdin: std::fs::File, receiver: Receiver<Vec<u8>>) -> io::Result<()> {
    while let Ok(chunk) = receiver.recv() {
        stdin.write_all(&chunk)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_limits_include_one_in_flight_chunk_and_the_queued_chunks() {
        assert_eq!(INPUT_CHUNK_BYTES, 16 * 1024);
        assert_eq!(QUEUED_INPUT_CHUNKS, 3);
        assert_eq!(INPUT_CHUNK_BYTES * (QUEUED_INPUT_CHUNKS + 1), 64 * 1024);
    }

    #[test]
    fn cancellation_wait_is_bounded() {
        assert_eq!(WORKER_STOP_TIMEOUT, Duration::from_secs(2));
        assert!(WORKER_POLL_INTERVAL <= Duration::from_millis(10));
    }
}
