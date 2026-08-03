/// Fixed-capacity sample ring buffer. Pushing past capacity overwrites the
/// oldest samples; `drain` removes everything currently held, oldest first.
/// All access happens under the engine's `StdMutex` — the producer side is a
/// real-time audio callback, so the lock is held for the shortest possible
/// window (a single memcpy-style extend).
#[derive(Debug)]
pub struct RingBuffer {
    buf: Vec<f32>,
    head: usize,
    len: usize,
    cap: usize,
}

impl RingBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: vec![0.0f32; cap],
            head: 0,
            len: 0,
            cap,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            self.buf[self.head] = s;
            self.head = (self.head + 1) % self.cap;
            if self.len < self.cap {
                self.len += 1;
            }
        }
    }

    /// Take everything, oldest first. The buffer is left empty.
    pub fn drain(&mut self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.len);
        if self.len == 0 {
            return out;
        }
        let tail_start = if self.len < self.cap { 0 } else { self.head };
        for i in 0..self.len {
            out.push(self.buf[(tail_start + i) % self.cap]);
        }
        self.len = 0;
        out
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.head = 0;
    }

    /// RMS of the currently held samples. Used by the engine's silent-capture
    /// check (the recording loop drains continuously, so the engine must be
    /// able to measure what is *in flight* without consuming it).
    pub fn rms(&self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        let tail_start = if self.len < self.cap { 0 } else { self.head };
        let sum_sq: f64 = (0..self.len)
            .map(|i| {
                let s = self.buf[(tail_start + i) % self.cap];
                (s as f64) * (s as f64)
            })
            .sum();
        ((sum_sq / self.len as f64).sqrt()) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_drain() {
        let mut rb = RingBuffer::new(10);
        rb.push(&[1.0, 2.0, 3.0]);
        assert_eq!(rb.len(), 3);
        let data = rb.drain();
        assert_eq!(data, vec![1.0, 2.0, 3.0]);
        assert_eq!(rb.len(), 0);
    }

    #[test]
    fn overwrite_oldest() {
        let mut rb = RingBuffer::new(5);
        rb.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(rb.len(), 5);
        rb.push(&[6.0, 7.0]);
        assert_eq!(rb.len(), 5);
        let data = rb.drain();
        assert_eq!(data, vec![3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn clear() {
        let mut rb = RingBuffer::new(10);
        rb.push(&[1.0, 2.0]);
        rb.clear();
        assert_eq!(rb.len(), 0);
        assert!(rb.drain().is_empty());
    }

    #[test]
    fn rms_measures_held_samples_without_consuming() {
        let mut rb = RingBuffer::new(10);
        rb.push(&[0.0, 0.0, 0.0]);
        assert_eq!(rb.rms(), 0.0);
        rb.push(&[0.5, -0.5]);
        // (0.25 + 0.25) / 5
        assert!((rb.rms() - 0.31622776).abs() < 1e-5);
        assert_eq!(rb.len(), 5);
    }
}
