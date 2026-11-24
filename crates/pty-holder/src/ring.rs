//! The holder's bounded output ring, addressed by ABSOLUTE offset.
//!
//! Every byte the pane's PTY produces gets an absolute offset: its position in
//! the stream since the child started. The ring keeps the newest `capacity`
//! bytes, `[start_offset, end_offset)`. A consumer that remembers the offset of
//! the next byte it has not seen can always resume exactly there — or learn,
//! exactly, how many bytes rolled out of the ring while it was away.
//!
//! This is the holder-side twin of the runner's offset machinery
//! (`AttachedRing` / `remote_offset` in `terminal/remote_pane_io.rs`,
//! `attach_tail` / `slice_ring` in `mcp/remote_terminal.rs`): the same
//! coordinates, the same clamping, so `splice_replay`'s gap honesty carries
//! over to a local holder unchanged.
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::collections::VecDeque;

/// A bounded byte ring with absolute offsets.
#[derive(Debug)]
pub struct OutputRing {
    buf: VecDeque<u8>,
    capacity: usize,
    /// Absolute offset of `buf[0]`.
    start: u64,
}

impl OutputRing {
    /// A ring that keeps at most `capacity` bytes (at least 1).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        OutputRing {
            buf: VecDeque::with_capacity(capacity.min(64 * 1024)),
            capacity,
            start: 0,
        }
    }

    /// A ring of `capacity` holding exactly `bytes`, the first of which is at
    /// absolute offset `start` — a successor holder's copy of its
    /// predecessor's ring (plan Phase 8). Bytes past the capacity are dropped
    /// from the FRONT, advancing the start, exactly as [`OutputRing::push`]
    /// would have.
    pub fn restore(capacity: usize, start: u64, bytes: &[u8]) -> Self {
        let mut ring = OutputRing::new(capacity);
        ring.start = start;
        ring.push(bytes);
        ring
    }

    /// Every byte held, oldest first, with the absolute offset of the first.
    pub fn contents(&self) -> (u64, Vec<u8>) {
        (self.start, self.buf.iter().copied().collect())
    }

    /// Absolute offset of the oldest byte still held.
    pub fn start_offset(&self) -> u64 {
        self.start
    }

    /// Absolute offset one past the newest byte ever produced.
    pub fn end_offset(&self) -> u64 {
        self.start + self.buf.len() as u64
    }

    /// Append `data`, evicting the oldest bytes past capacity.
    pub fn push(&mut self, data: &[u8]) {
        if data.len() >= self.capacity {
            // Everything currently held, and the head of `data`, roll out.
            let keep_from = data.len() - self.capacity;
            self.start = self.end_offset() + keep_from as u64;
            self.buf.clear();
            self.buf.extend(data.iter().skip(keep_from));
            return;
        }
        let overflow = (self.buf.len() + data.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            self.buf.drain(..overflow);
            self.start += overflow as u64;
        }
        self.buf.extend(data);
    }

    /// Up to `max` bytes starting at absolute offset `from`, clamped to what the
    /// ring holds. Returns the absolute offset of the first returned byte
    /// (`max(from, start)`, or `end` when `from` is past it) and the bytes.
    /// A caller that asked for `from < start` must report `[from, start)` as
    /// lost — this function never pretends those bytes exist.
    ///
    /// Copies with two `memcpy`s over the deque's halves, not byte by byte:
    /// callers hold the pane lock while it runs, and a tail can be a MiB.
    pub fn slice_from(&self, from: u64, max: usize) -> (u64, Vec<u8>) {
        let end = self.end_offset();
        let lo = from.clamp(self.start, end);
        let skip = (lo - self.start) as usize;
        let take = ((end - lo) as usize).min(max);
        let (front, back) = self.buf.as_slices();
        let mut out = Vec::with_capacity(take);
        if skip < front.len() {
            let n = (front.len() - skip).min(take);
            out.extend_from_slice(&front[skip..skip + n]);
            let rest = take - n;
            out.extend_from_slice(&back[..rest]);
        } else {
            let b = skip - front.len();
            out.extend_from_slice(&back[b..b + take]);
        }
        (lo, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_holder_ring_offsets_are_absolute_and_eviction_is_counted() {
        let mut r = OutputRing::new(8);
        r.push(b"abc");
        assert_eq!((r.start_offset(), r.end_offset()), (0, 3));
        r.push(b"defgh");
        assert_eq!((r.start_offset(), r.end_offset()), (0, 8));
        r.push(b"ij");
        assert_eq!((r.start_offset(), r.end_offset()), (2, 10));
        assert_eq!(r.slice_from(2, 100), (2, b"cdefghij".to_vec()));
        assert_eq!(r.slice_from(5, 3), (5, b"fgh".to_vec()));
        // Below the ring: clamped up, never invented.
        assert_eq!(r.slice_from(0, 2), (2, b"cd".to_vec()));
        // Past the end: empty, anchored at the end.
        assert_eq!(r.slice_from(99, 4), (10, Vec::new()));
        // A single push larger than the ring keeps only its tail.
        r.push(b"0123456789AB");
        assert_eq!((r.start_offset(), r.end_offset()), (14, 22));
        assert_eq!(r.slice_from(0, 100), (14, b"456789AB".to_vec()));
    }

    #[test]
    fn pty_holder_ring_slice_spans_a_wrapped_deque() {
        // Push and evict until the deque's storage wraps, then check every
        // (from, max) against the plain byte sequence.
        let mut r = OutputRing::new(64);
        let all: Vec<u8> = (0u8..200).collect();
        for chunk in all.chunks(13) {
            r.push(chunk);
        }
        let (start, end) = (r.start_offset(), r.end_offset());
        assert_eq!((start, end), (136, 200));
        for from in start..=end {
            for max in [0, 1, 7, 30, 64, 100] {
                let (lo, got) = r.slice_from(from, max);
                assert_eq!(lo, from);
                let want_end = (from as usize + max).min(end as usize);
                assert_eq!(got, all[from as usize..want_end], "from {from} max {max}");
            }
        }
    }

    #[test]
    fn pty_holder_handoff_ring_restore_keeps_absolute_offsets() {
        let mut r = OutputRing::new(8);
        r.push(b"0123456789");
        let (start, bytes) = r.contents();
        assert_eq!((start, bytes.as_slice()), (2, &b"23456789"[..]));
        let back = OutputRing::restore(8, start, &bytes);
        assert_eq!((back.start_offset(), back.end_offset()), (2, 10));
        assert_eq!(back.slice_from(0, 100), (2, b"23456789".to_vec()));
        // A smaller capacity keeps the newest bytes, at their true offsets.
        let small = OutputRing::restore(4, start, &bytes);
        assert_eq!(small.slice_from(0, 100), (6, b"6789".to_vec()));
    }

    #[test]
    fn pty_holder_ring_is_byte_exact() {
        let all: Vec<u8> = (0u8..=255).collect();
        let mut r = OutputRing::new(1024);
        for chunk in all.chunks(7) {
            r.push(chunk);
        }
        assert_eq!(r.slice_from(0, 1024), (0, all));
    }
}
