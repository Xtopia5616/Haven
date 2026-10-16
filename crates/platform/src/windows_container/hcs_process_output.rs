//! Bounded polling for HCS guest stdout and stderr pipes.
//!
//! The caller polls both streams while supervising process and system
//! lifetime. Reads are limited to bytes already reported available by
//! `PeekNamedPipe`, so an empty stream is not read synchronously while the
//! other stream is drained. A single owner must remain the only I/O user for
//! each handle because synchronous pipe APIs may serialize concurrent access.

use super::execution::{WindowsContainerOutputSink, WindowsContainerOutputStream};
use super::hcs_process_runtime::{HcsGuestProcessOwner, OwnedGuestHandle};
use std::io;
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, GetLastError,
};
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::Pipes::PeekNamedPipe;

const OUTPUT_READ_CHUNK_BYTES: usize = 16 * 1024;
const MAX_BYTES_PER_STREAM_PER_POLL: usize = 64 * 1024;

pub(super) trait HcsOutputChunkSink {
    fn write_chunk(&mut self, stream: WindowsContainerOutputStream, bytes: &[u8]);
}

impl HcsOutputChunkSink for WindowsContainerOutputSink<'_> {
    fn write_chunk(&mut self, stream: WindowsContainerOutputStream, bytes: &[u8]) {
        WindowsContainerOutputSink::write_chunk(self, stream, bytes);
    }
}

/// Bytes consumed during one bounded poll and the current pipe state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HcsOutputDrainProgress {
    pub(super) stdout_bytes_read: usize,
    pub(super) stderr_bytes_read: usize,
    pub(super) stdout_closed: bool,
    pub(super) stderr_closed: bool,
}

impl HcsOutputDrainProgress {
    pub(super) fn is_complete(self) -> bool {
        self.stdout_closed && self.stderr_closed
    }
}

/// Owns both process output handles while polling a bounded amount from each.
pub(super) struct HcsGuestProcessOutputReader<'a> {
    stdout: Option<OwnedGuestHandle<'a>>,
    stderr: Option<OwnedGuestHandle<'a>>,
}

impl<'a> HcsGuestProcessOutputReader<'a> {
    pub(super) fn from_process(process: &mut HcsGuestProcessOwner<'a>) -> io::Result<Self> {
        let stdout = process.take_stdout().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCS guest stdout handle has already been transferred",
            )
        })?;
        let stderr = process.take_stderr().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCS guest stderr handle has already been transferred",
            )
        })?;
        Ok(Self {
            stdout: Some(stdout),
            stderr: Some(stderr),
        })
    }

    /// Drain at most 64 KiB per stream, reading only bytes already available.
    /// Excess retained output is discarded by the existing sink, but pipe
    /// bytes continue to be consumed so the guest cannot block on a full pipe.
    /// No other thread may read from or duplicate these handles while polling.
    pub(super) fn drain_available(
        &mut self,
        sink: &mut dyn HcsOutputChunkSink,
    ) -> io::Result<HcsOutputDrainProgress> {
        let (stdout_bytes_read, stdout_closed) =
            drain_pipe(&mut self.stdout, WindowsContainerOutputStream::Stdout, sink)?;
        let (stderr_bytes_read, stderr_closed) =
            drain_pipe(&mut self.stderr, WindowsContainerOutputStream::Stderr, sink)?;
        Ok(HcsOutputDrainProgress {
            stdout_bytes_read,
            stderr_bytes_read,
            stdout_closed,
            stderr_closed,
        })
    }
}

fn drain_pipe(
    handle: &mut Option<OwnedGuestHandle<'_>>,
    stream: WindowsContainerOutputStream,
    sink: &mut dyn HcsOutputChunkSink,
) -> io::Result<(usize, bool)> {
    let Some(owned) = handle.as_ref() else {
        return Ok((0, true));
    };
    let raw = owned.raw().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "HCS guest output handle has already been closed",
        )
    })?;
    let mut total_read = 0usize;
    let mut buffer = [0u8; OUTPUT_READ_CHUNK_BYTES];

    while total_read < MAX_BYTES_PER_STREAM_PER_POLL {
        let mut available = 0u32;
        if unsafe {
            PeekNamedPipe(
                raw,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            let error = unsafe { GetLastError() };
            if is_closed_pipe_error(error) {
                handle.take();
                return Ok((total_read, true));
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        if available == 0 {
            return Ok((total_read, false));
        }

        let read_limit = (available as usize)
            .min(buffer.len())
            .min(MAX_BYTES_PER_STREAM_PER_POLL - total_read);
        let mut bytes_read = 0u32;
        if unsafe {
            ReadFile(
                raw,
                buffer.as_mut_ptr(),
                read_limit as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        } == 0
        {
            let error = unsafe { GetLastError() };
            if is_closed_pipe_error(error) {
                handle.take();
                return Ok((total_read, true));
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        if bytes_read == 0 {
            return Ok((total_read, false));
        }
        let bytes_read = bytes_read as usize;
        sink.write_chunk(stream, &buffer[..bytes_read]);
        total_read = total_read.saturating_add(bytes_read);
    }

    Ok((total_read, false))
}

fn is_closed_pipe_error(error: u32) -> bool {
    matches!(
        error,
        ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_pipe_errors_are_classified_without_masking_other_failures() {
        assert!(is_closed_pipe_error(ERROR_BROKEN_PIPE));
        assert!(is_closed_pipe_error(ERROR_PIPE_NOT_CONNECTED));
        assert!(is_closed_pipe_error(ERROR_NO_DATA));
        assert!(!is_closed_pipe_error(5));
    }

    #[test]
    fn output_poll_limits_each_stream_and_each_read_chunk() {
        assert_eq!(OUTPUT_READ_CHUNK_BYTES, 16 * 1024);
        assert_eq!(MAX_BYTES_PER_STREAM_PER_POLL, 64 * 1024);
    }
}
