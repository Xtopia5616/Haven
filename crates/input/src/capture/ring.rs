/// Fixed-capacity sample ring buffer. Pushing past capacity overwrites the
/// oldest samples; `drain` removes everything currently held, oldest first.
/// All access happens under the engine's `StdMutex` — the producer side is a
/// real-time audio callback, so the lock is held for the shortest possible
/// window (a single memcpy-style extend).
#[derive(Debug)]
pub(super) struct RingBuffer {
    buf: Vec<f32>,
    head: usize,
    len: usize,
    cap: usize,
}

impl RingBuffer {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            buf: vec![0.0f32; cap],
            head: 0,
            len: 0,
            cap,
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn buffered_len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.cap
    }

    /// Push samples and return how many samples were discarded, including
    /// incoming samples that cannot fit in a zero-capacity ring.
    pub(super) fn push(&mut self, samples: &[f32]) -> usize {
        if samples.is_empty() {
            return 0;
        }
        if self.cap == 0 {
            return samples.len();
        }
        let dropped = samples.len().saturating_sub(self.cap - self.len);
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
        self.len = self.len.saturating_add(samples.len()).min(self.cap);
        dropped
    }

    /// Copy everything into a caller-owned buffer, oldest first. The caller
    /// must ensure `out.capacity() >= self.len`; capture consumers reserve
    /// outside the mutex before calling this method.
    pub(super) fn drain_into(&mut self, out: &mut Vec<f32>) {
        debug_assert!(out.capacity() >= self.len);
        out.clear();
        if self.len == 0 {
            return;
        }
        // The oldest sample sits at `head - len` (circularly). Using the
        // plain `head - len` window is wrong once the writer has wrapped
        // around the buffer at least once: `len < cap` no longer implies the
        // data starts at index 0.
        let tail_start = (self.head + self.cap - self.len) % self.cap;
        let first_len = self.len.min(self.cap - tail_start);
        out.extend_from_slice(&self.buf[tail_start..tail_start + first_len]);
        if first_len < self.len {
            out.extend_from_slice(&self.buf[..self.len - first_len]);
        }
        self.len = 0;
    }

    #[cfg(test)]
    pub(super) fn drain(&mut self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.len);
        self.drain_into(&mut out);
        out
    }

    pub(super) fn clear(&mut self) {
        self.len = 0;
        self.head = 0;
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
    fn zero_capacity_buffer_ignores_pushes() {
        let mut rb = RingBuffer::new(0);
        rb.push(&[1.0, 2.0, 3.0]);

        assert_eq!(rb.capacity(), 0);
        assert_eq!(rb.len(), 0);
        assert!(rb.is_empty());
        assert!(rb.drain().is_empty());
        rb.clear();
        assert_eq!(rb.len(), 0);
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
}
