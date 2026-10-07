use std::io::{self, Read};

use tokio_util::sync::CancellationToken;

/// A reader that interrupts an I/O operation when its owning task is cancelled.
pub(crate) struct CancellableReader<R> {
    inner: R,
    cancel: Option<CancellationToken>,
    max_read_bytes: Option<usize>,
    cancelled_error_kind: io::ErrorKind,
    cancelled_error_message: &'static str,
}

impl<R> CancellableReader<R> {
    pub(crate) fn new(inner: R, cancel: Option<&CancellationToken>) -> Self {
        Self {
            inner,
            cancel: cancel.cloned(),
            max_read_bytes: None,
            cancelled_error_kind: io::ErrorKind::Interrupted,
            cancelled_error_message: "operation cancelled",
        }
    }

    pub(crate) fn with_max_read_bytes(mut self, max_read_bytes: usize) -> Self {
        assert!(max_read_bytes > 0, "read limit must be nonzero");
        self.max_read_bytes = Some(max_read_bytes);
        self
    }

    pub(crate) fn with_cancel_error(mut self, kind: io::ErrorKind, message: &'static str) -> Self {
        self.cancelled_error_kind = kind;
        self.cancelled_error_message = message;
        self
    }
}

impl<R: Read> Read for CancellableReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self
            .cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(io::Error::new(
                self.cancelled_error_kind,
                self.cancelled_error_message,
            ));
        }

        let buffer = match self.max_read_bytes {
            Some(max_read_bytes) => {
                let len = buffer.len().min(max_read_bytes);
                &mut buffer[..len]
            }
            None => buffer,
        };
        self.inner.read(buffer)
    }
}
