//! `PaneOutput` — the offset-anchored output channel shared by every
//! out-of-process [`super::pane_io::PaneIo`].
//!
//! Plan `2026-08-31-remote-session-tabs-in-runner-terminal` built this inside
//! `RemotePaneIo`: an output channel the session's reader thread drains, an
//! absolute stream offset advanced only by bytes that really came from the
//! source, an in-band loss marker whenever the source's ring rolled past bytes
//! this pane never received, and an exit slot `wait` parks on. Plan
//! `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions` (D2,
//! Phase 2) has a second consumer — `DaemonPaneIo`, whose bytes come from a
//! local PTY holder — and says to reuse that machinery wholesale rather than
//! re-implement it. So it lives here, once, and both panes hold one:
//! `RemotePaneIo` keyed by its `grant_jti`, `DaemonPaneIo` by its local pane
//! id. The key only attributes log lines; nothing routes on it here.
//!
//! What differs between the two consumers is only the WORDING of the loss
//! marker (a remote ring rolled past the bytes vs. the holder's ring did), so
//! the marker is a constructor argument.
//!
//! # The exit slot
//!
//! `wait` returns `Result<i32, String>`, the [`super::pane_io::PaneIo::wait`]
//! contract, and the slot stores exactly that: `Err` is how a pane says its
//! exit code is UNKNOWN (the session layer records `None`, never a fabricated
//! number). First writer wins.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

/// The in-band loss notice for `lost_bytes` bytes. A plain `fn` so a pane can
/// carry its own wording without a closure allocation.
pub type LossMarker = fn(u64) -> Vec<u8>;

/// See the module docs.
pub struct PaneOutput {
    /// The correlation key, for log lines only.
    key: String,
    /// Sender half of the output channel. `None` once closed — the reader
    /// sees EOF when the last sender drops.
    output_tx: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    /// Receiver half, taken exactly once by [`Self::take_reader`].
    output_rx: Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    /// The settled exit; `wait` parks on the condvar until it is `Some`.
    exit: Mutex<Option<Result<i32, String>>>,
    exit_cv: Condvar,
    /// Absolute source offset of the next byte this pane expects — the
    /// reconnect splice point.
    offset: AtomicU64,
    loss_marker: LossMarker,
}

impl PaneOutput {
    /// A channel whose first bytes are `seed`, which begins at absolute
    /// source offset `start_offset`.
    pub fn new(
        key: impl Into<String>,
        seed: Vec<u8>,
        start_offset: u64,
        loss_marker: LossMarker,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let next = start_offset.saturating_add(seed.len() as u64);
        if !seed.is_empty() {
            // The receiver is alive (we hold it), so this cannot fail.
            let _ = tx.send(seed);
        }
        Self {
            key: key.into(),
            output_tx: Mutex::new(Some(tx)),
            output_rx: Mutex::new(Some(rx)),
            exit: Mutex::new(None),
            exit_cv: Condvar::new(),
            offset: AtomicU64::new(next),
            loss_marker,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Absolute source offset of the next byte this pane expects.
    pub fn offset(&self) -> u64 {
        self.offset.load(Ordering::Acquire)
    }

    /// The blocking reader, handed out once.
    pub fn take_reader(&self) -> Result<Box<dyn Read + Send>, String> {
        let rx = self
            .output_rx
            .lock()
            .map_err(|e| format!("pane output reader lock poisoned: {e}"))?
            .take()
            .ok_or_else(|| "pane output reader already taken".to_string())?;
        Ok(Box::new(ChannelReader {
            rx,
            pending: Vec::new(),
            pos: 0,
        }))
    }

    /// Queue one chunk of SOURCE bytes, advancing the offset. Silently dropped
    /// once the channel is closed — a late frame after exit has nowhere to
    /// go, exactly like bytes after a local PTY's EOF.
    ///
    /// Returns whether the chunk was DELIVERED (queued and the offset
    /// advanced) — a caller that records receipts (`RemotePaneIo`'s
    /// interactivity) stamps one only then. An empty chunk delivers nothing.
    pub fn push_output(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        let Ok(tx) = self.output_tx.lock() else {
            return false;
        };
        match tx.as_ref() {
            Some(tx) if tx.send(bytes.to_vec()).is_ok() => {
                self.offset.fetch_add(bytes.len() as u64, Ordering::AcqRel);
                true
            }
            _ => false,
        }
    }

    /// True while the channel is open — false once the pane exited or was
    /// released, when a late frame goes nowhere.
    pub fn is_open(&self) -> bool {
        self.output_tx
            .lock()
            .map(|tx| tx.is_some())
            .unwrap_or(false)
    }

    /// Queue bytes that originate HERE (an in-band notice) without moving the
    /// offset, so the splice arithmetic stays anchored to the source stream.
    pub fn push_local(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let Ok(tx) = self.output_tx.lock() else {
            return;
        };
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(bytes.to_vec());
        }
    }

    /// Splice `buffer`, which begins at absolute source offset `start`: bytes
    /// this pane has already delivered are skipped, the rest is queued. A
    /// chunk that starts past what we have seen means the source produced
    /// more than its ring holds while we were not reading — the loss is
    /// written into the pane as an in-band marker (never silently) and the
    /// whole chunk is delivered after it.
    ///
    /// Returns whether the source's chunk counts as a RECEIPT: it delivered
    /// new bytes, or it added nothing new while the pane is still open (the
    /// source answered; we already had it). A chunk arriving after exit went
    /// nowhere and is not a receipt.
    pub fn splice(&self, start: u64, buffer: &[u8]) -> bool {
        let have = self.offset();
        let end = start.saturating_add(buffer.len() as u64);
        if have >= end {
            debug!(
                pane = %self.key,
                have,
                ring_end = end,
                "pane output: spliced chunk adds nothing new"
            );
            return self.is_open();
        }
        let skip = if have > start {
            (have - start) as usize
        } else {
            if have < start {
                self.note_gap(start - have);
            }
            0
        };
        // `push_output` advances the offset by the delivered length; align it
        // to the chunk's own start first so the arithmetic lands on `end`.
        self.offset
            .store(start.saturating_add(skip as u64), Ordering::Release);
        self.push_output(buffer.get(skip..).unwrap_or_default())
    }

    /// The source reported `[from, to)` gone before it reached us. Writes the
    /// marker for the part we had not already seen and moves the offset to
    /// `to`; a range entirely behind us is ignored.
    pub fn note_lost(&self, from: u64, to: u64) {
        let have = self.offset();
        if to <= have {
            return;
        }
        let lost = to - from.max(have);
        if lost > 0 {
            self.note_gap(lost);
        }
        self.offset.store(to, Ordering::Release);
    }

    fn note_gap(&self, lost: u64) {
        warn!(
            pane = %self.key,
            lost_bytes = lost,
            "pane output: the source's ring rolled past bytes this pane never received"
        );
        self.push_local(&(self.loss_marker)(lost));
    }

    /// Settle the exit (first writer wins) and close the channel so a reader
    /// blocked in `read()` sees EOF.
    pub fn settle(&self, exit: Result<i32, String>) {
        if let Ok(mut slot) = self.exit.lock() {
            if slot.is_none() {
                *slot = Some(exit);
            }
        }
        self.exit_cv.notify_all();
        self.close_output();
    }

    /// True once `wait` has an answer.
    pub fn is_finished(&self) -> bool {
        self.exit.lock().map(|e| e.is_some()).unwrap_or(true)
    }

    /// Close the channel without settling the exit.
    pub fn close_output(&self) {
        if let Ok(mut tx) = self.output_tx.lock() {
            drop(tx.take());
        }
    }

    /// Block until the exit is settled.
    pub fn wait(&self) -> Result<i32, String> {
        let mut slot = self
            .exit
            .lock()
            .map_err(|e| format!("pane exit lock poisoned: {e}"))?;
        loop {
            if let Some(exit) = slot.as_ref() {
                return exit.clone();
            }
            slot = self
                .exit_cv
                .wait(slot)
                .map_err(|e| format!("pane exit lock poisoned: {e}"))?;
        }
    }

    /// [`Self::wait`] bounded by `timeout`: `None` when the exit was not
    /// settled in time.
    pub fn wait_for(&self, timeout: Duration) -> Option<Result<i32, String>> {
        let deadline = Instant::now() + timeout;
        let mut slot = self.exit.lock().ok()?;
        loop {
            if let Some(exit) = slot.as_ref() {
                return Some(exit.clone());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            if left.is_zero() {
                return None;
            }
            slot = self.exit_cv.wait_timeout(slot, left).ok()?.0;
        }
    }
}

/// Blocking `Read` over the output channel: yields queued chunks in order,
/// EOF once every sender is gone.
struct ChannelReader {
    rx: mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pos: usize,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.pending.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.pos = 0;
                }
                // Every sender dropped: the pane exited or was released.
                Err(mpsc::RecvError) => return Ok(0),
            }
        }
        let n = (self.pending.len() - self.pos).min(buf.len());
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(n: u64) -> Vec<u8> {
        format!("<lost {n}>").into_bytes()
    }

    fn drain(out: &PaneOutput) -> Vec<u8> {
        let mut r = out.take_reader().unwrap();
        let mut v = Vec::new();
        r.read_to_end(&mut v).unwrap();
        v
    }

    /// `note_lost` marks only the unseen part, moves the offset to the end of
    /// the range, and ignores a range already behind us.
    #[test]
    fn note_lost_marks_only_the_unseen_part() {
        let out = PaneOutput::new("k", b"abc".to_vec(), 0, marker);
        out.note_lost(0, 2); // behind us
        assert_eq!(out.offset(), 3);
        out.note_lost(1, 10); // [3, 10) unseen
        assert_eq!(out.offset(), 10);
        out.splice(10, b"XY");
        out.settle(Ok(0));
        assert_eq!(drain(&out), b"abc<lost 7>XY");
    }

    /// `wait_for` is bounded and sees a later settle; first settle wins.
    #[test]
    fn wait_for_is_bounded_and_first_settle_wins() {
        let out = PaneOutput::new("k", Vec::new(), 0, marker);
        assert!(out.wait_for(Duration::from_millis(20)).is_none());
        out.settle(Err("unknown".into()));
        out.settle(Ok(0));
        assert_eq!(out.wait_for(Duration::ZERO), Some(Err("unknown".into())));
        assert_eq!(out.wait(), Err("unknown".into()));
    }
}
