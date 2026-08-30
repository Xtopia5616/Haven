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
        if samples.is_empty() {
            return;
        }
        // The queue keeps the last `cap` samples of the combined stream: if
        // the incoming chunk is longer than the ring, only its tail survives
        // (matching the per-sample overwrite semantics exactly).
        let take = samples.len().min(self.cap);
        let input = &samples[samples.len() - take..];
        // Two contiguous copies instead of a per-sample `% cap` loop: this
        // runs on the real-time audio callback thread, and a modulo per
        // sample costs more than a memcpy per segment.
        let first_len = take.min(self.cap - self.head);
        self.buf[self.head..self.head + first_len].copy_from_slice(&input[..first_len]);
        if first_len < take {
            let rest = &input[first_len..];
            self.buf[..rest.len()].copy_from_slice(rest);
            self.head = rest.len();
        } else {
            self.head = (self.head + first_len) % self.cap;
        }
        self.len = (self.len + samples.len()).min(self.cap);
    }

    /// Take everything, oldest first. The buffer is left empty.
    pub fn drain(&mut self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.len);
        if self.len == 0 {
            return out;
        }
        // The oldest sample sits at `head - len` (circularly). Using the
        // plain `head - len` window is wrong once the writer has wrapped
        // around the buffer at least once: `len < cap` no longer implies the
        // data starts at index 0.
        let tail_start = (self.head + self.cap - self.len) % self.cap;
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
        let tail_start = (self.head + self.cap - self.len) % self.cap;
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
    fn push_oversize_chunk_keeps_tail() {
        let mut rb = RingBuffer::new(5);
        rb.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(rb.len(), 5);
        assert_eq!(rb.drain(), vec![3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn push_matches_per_sample_semantics_across_chunks() {
        let mut rb = RingBuffer::new(7);
        rb.push(&[1.0, 2.0]);
        rb.push(&[3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        assert_eq!(rb.len(), 7);
        assert_eq!(rb.drain(), vec![4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
    }

    #[test]
    fn push_wraps_and_drops_partial_tail() {
        let mut rb = RingBuffer::new(8);
        rb.push(&[1.0, 2.0, 3.0]);
        // head = 3; the chunk crosses the end of the buffer twice.
        rb.push(&[4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        assert_eq!(rb.len(), 8);
        assert_eq!(rb.drain(), vec![3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
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
    fn drain_after_wraparound_reads_latest_data() {
        let mut rb = RingBuffer::new(10);
        // Fill exactly to capacity so `head` wraps to 0, then overwrite the
        // two oldest entries.
        rb.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        rb.push(&[11.0, 12.0]);
        assert_eq!(
            rb.drain(),
            vec![3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0]
        );
        // head=2 now. New data lands past the wrap point; a drain that reads
        // from index 0 would return the stale [11, 12] instead of [13, 14].
        rb.push(&[13.0, 14.0]);
        assert_eq!(rb.drain(), vec![13.0, 14.0]);
    }

    #[test]
    fn rms_after_wraparound_uses_latest_window() {
        let mut rb = RingBuffer::new(4);
        rb.push(&[1.0, 2.0, 3.0, 4.0]);
        rb.push(&[5.0, 6.0]);
        // Held samples are [5, 6, 3, 4]; RMS = sqrt((25+36+9+16)/4).
        assert_eq!(rb.len(), 4);
        assert!((rb.rms() - 4.636809).abs() < 1e-4);
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
