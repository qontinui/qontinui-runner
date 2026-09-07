//! `RemotePaneIo` — a [`PaneIo`] whose bytes live on another machine.
//!
//! Plan `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 3c (source
//! role), D1/D6. The SOURCE runner opens an ordinary `TerminalSession` around
//! this type: the reader thread, grid, scrollback ring, emission gate and
//! waiter thread above it are the same code a local PTY drives, and none of
//! them learn a second byte source. What differs is only where the bytes come
//! from and go to:
//!
//! - **Output** arrives as `remote_terminal_output` frames on the runner's one
//!   backend socket (`mcp/backend_relay.rs`), routed here by `grant_jti` by
//!   [`crate::mcp::remote_terminal::RemoteAttachClient`], base64-decoded, and
//!   queued into a channel the [`PaneIo::reader`] drains. `remote_terminal_exit`
//!   and `remote_terminal_error` close that channel (reader EOF) and settle the
//!   exit code [`PaneIo::wait`] returns.
//! - **Input** goes out as `remote_terminal_input` frames through a
//!   [`RemoteFrameSink`] the relay owns; `resize`, `set_paused`, `kill` and
//!   `release` map to `remote_terminal_resize`, `remote_terminal_flow` and
//!   `remote_terminal_detach` per the Phase 3 wire contract.
//!
//! # The credential-scrub obligation
//!
//! This implementation launches no child and hands no environment to
//! anything — the process whose environment matters runs on the TARGET
//! machine, where the target runner's own `LocalPty` scrubbed it. So the
//! honest answer to [`PaneIo::credential_scrub`] is
//! [`CredentialScrub::NoChildEnv`], and this file imports nothing from
//! `portable_pty`.
//!
//! # Exit-code mapping
//!
//! `wait` returns the target's `exit_code` when the remote process ends. A
//! LOCAL detach — the operator closing the tab, which reaches this type as
//! `kill`/`release` — settles `wait` with [`DETACH_EXIT_CODE`] (`0`): the
//! remote session is still alive, so a non-zero code would mis-report a clean
//! close as a failure. A `remote_terminal_error` settles it with
//! [`ERROR_EXIT_CODE`] (`1`), matching `LocalPty`'s "non-zero falls back to 1".

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use tracing::{debug, warn};

use super::pane_io::{CredentialScrub, PaneIo};

/// Exit code `wait` reports after a LOCAL detach (`kill`/`release`).
pub const DETACH_EXIT_CODE: i32 = 0;
/// Exit code `wait` reports after a `remote_terminal_error`.
pub const ERROR_EXIT_CODE: i32 = 1;

/// Where a remote pane's outbound frames go. The relay owns the socket and
/// hands the pane only this; a test hands it a recorder.
pub trait RemoteFrameSink: Send + Sync {
    /// Queue one frame for the backend socket. `Err` means the frame was NOT
    /// queued (relay backlog full or the client torn down) — the caller
    /// surfaces it as an I/O error rather than pretending the keystroke went.
    fn send_frame(&self, frame: Value) -> Result<(), String>;
}

impl RemoteFrameSink for tokio::sync::mpsc::Sender<Value> {
    fn send_frame(&self, frame: Value) -> Result<(), String> {
        self.try_send(frame).map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                "remote attach: relay outbound backlog is full".to_string()
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                "remote attach: relay outbound channel is closed".to_string()
            }
        })
    }
}

/// The target's reply to `remote_terminal_attach` — what a pane is seeded
/// with. `buffer` is the target's scrollback ring, already base64-decoded;
/// `start_offset` is the absolute offset of its first byte on the target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachedRing {
    pub buffer: Vec<u8>,
    pub start_offset: u64,
    pub total_bytes_produced: u64,
    /// Absolute offset of the FIRST byte the target's ring still holds. The
    /// attach reply ships only a bounded tail (Phase 5, lazy scrollback), so
    /// this can sit below `start_offset`; the gap is fetchable on demand with
    /// `remote_terminal_buffer {from_offset, to_offset}`. `None` on a target
    /// that predates the field — then nothing earlier is offered.
    pub history_start: Option<u64>,
}

/// In-band notice for bytes the target produced while this pane could not
/// receive them (a relay drop that outlived the target's ring). Same wording
/// as the frontend's `lostOutputMarker` in `scrollbackReplay.ts`, so the
/// operator reads one vocabulary whichever hop lost the bytes.
pub fn lost_output_marker(lost_bytes: u64) -> Vec<u8> {
    format!(
        "\r\n\x1b[1;33m[qontinui] {lost_bytes} bytes of output were lost here — the remote \
         ring rolled past them while this tab was detached\x1b[0m\r\n"
    )
    .into_bytes()
}

/// In-band notice written when the relay connection carrying this pane
/// drops. The pane stays open: the client re-presents the grant on
/// reconnect and splices the target's ring from the last byte seen.
pub const RELAY_LOST_MARKER: &[u8] =
    b"\r\n\x1b[1;33m[qontinui] relay connection lost \xe2\x80\x94 the remote session is still \
running; this tab reattaches when the relay returns\x1b[0m\r\n";

/// A [`PaneIo`] over the backend relay for one remote terminal.
pub struct RemotePaneIo {
    grant_jti: String,
    terminal_id: String,
    /// The grant JWT, kept so a relay reconnect can re-present it.
    grant: String,
    sink: Arc<dyn RemoteFrameSink>,
    /// Sender half of the output channel. `None` once closed — the reader
    /// sees EOF when the last sender drops.
    output_tx: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    /// Receiver half, taken exactly once by [`PaneIo::reader`].
    output_rx: Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    /// The settled exit code; `wait` parks on the condvar until it is `Some`.
    exit: Mutex<Option<i32>>,
    exit_cv: Condvar,
    /// `remote_terminal_detach` is sent at most once per pane.
    detach_sent: AtomicBool,
    /// Absolute target offset of the next byte this pane expects — the
    /// reconnect splice point.
    remote_offset: AtomicU64,
    cols: AtomicU16,
    rows: AtomicU16,
    /// Absolute target offset of the first seed byte — the upper bound of the
    /// history the target still holds but did not ship at attach.
    seed_start: u64,
    /// See [`AttachedRing::history_start`].
    history_start: Option<u64>,
}

impl RemotePaneIo {
    /// Build a pane already seeded with the target's ring: the seed bytes are
    /// the first thing the reader yields, so the local grid and scrollback
    /// ring are populated through the SAME path every later chunk takes.
    pub fn new(
        grant_jti: impl Into<String>,
        terminal_id: impl Into<String>,
        grant: impl Into<String>,
        sink: Arc<dyn RemoteFrameSink>,
        cols: u16,
        rows: u16,
        seed: AttachedRing,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let next_offset = seed.start_offset.saturating_add(seed.buffer.len() as u64);
        debug!(
            seed_bytes = seed.buffer.len(),
            start_offset = seed.start_offset,
            target_total = seed.total_bytes_produced,
            "remote pane: seeded from the target's ring"
        );
        if !seed.buffer.is_empty() {
            // The receiver is alive (we hold it), so this cannot fail.
            let _ = tx.send(seed.buffer);
        }
        Self {
            grant_jti: grant_jti.into(),
            terminal_id: terminal_id.into(),
            grant: grant.into(),
            sink,
            output_tx: Mutex::new(Some(tx)),
            output_rx: Mutex::new(Some(rx)),
            exit: Mutex::new(None),
            exit_cv: Condvar::new(),
            detach_sent: AtomicBool::new(false),
            remote_offset: AtomicU64::new(next_offset),
            cols: AtomicU16::new(cols),
            rows: AtomicU16::new(rows),
            seed_start: seed.start_offset,
            history_start: seed.history_start,
        }
    }

    /// The `[from, to)` target range OLDER than the attach seed that the
    /// target still holds — `None` when the seed already began at the ring's
    /// first byte (or the target reported no ring start). Phase 5 lazy
    /// scrollback: fetched only when the operator asks for earlier output.
    pub fn history_range(&self) -> Option<(u64, u64)> {
        let start = self.history_start?;
        (start < self.seed_start).then_some((start, self.seed_start))
    }

    pub fn grant_jti(&self) -> &str {
        &self.grant_jti
    }

    pub fn terminal_id(&self) -> &str {
        &self.terminal_id
    }

    /// Absolute target offset of the next byte this pane expects.
    pub fn remote_offset(&self) -> u64 {
        self.remote_offset.load(Ordering::Acquire)
    }

    /// The viewport last announced with `resize` (or the attach size).
    pub fn dims(&self) -> (u16, u16) {
        (
            self.cols.load(Ordering::Relaxed),
            self.rows.load(Ordering::Relaxed),
        )
    }

    /// True once `wait` has an answer — the routing client sweeps these.
    pub fn is_finished(&self) -> bool {
        self.exit.lock().map(|e| e.is_some()).unwrap_or(true)
    }

    /// Queue one output chunk from the target (already decoded). Silently
    /// dropped once the pane is closed — a late frame after exit has nowhere
    /// to go, exactly like bytes after a local PTY's EOF.
    pub fn push_output(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let Ok(tx) = self.output_tx.lock() else {
            return;
        };
        if let Some(tx) = tx.as_ref() {
            if tx.send(bytes.to_vec()).is_ok() {
                self.remote_offset
                    .fetch_add(bytes.len() as u64, Ordering::AcqRel);
            }
        }
    }

    /// Queue bytes that originate HERE rather than on the target — an in-band
    /// notice — without moving the remote offset, so the splice arithmetic
    /// stays anchored to the target's stream.
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

    /// The relay carrying this pane dropped. Say so in the pane; the session
    /// stays open (a drop is not an exit) and the client reattaches on
    /// reconnect.
    pub fn note_relay_lost(&self) {
        self.push_local(RELAY_LOST_MARKER);
    }

    /// Splice a ring the target sent on RE-attach: bytes this pane has already
    /// delivered are skipped, the rest is queued. A ring that starts past what
    /// we have seen means the target produced more than its ring holds while
    /// we were away — the loss is written into the pane as an in-band marker
    /// (never silently) and the whole ring is delivered after it.
    pub fn splice_replay(&self, ring: &AttachedRing) {
        let have = self.remote_offset();
        let start = ring.start_offset;
        let end = start.saturating_add(ring.buffer.len() as u64);
        if have >= end {
            debug!(
                grant_jti = %self.grant_jti,
                have,
                ring_end = end,
                "remote pane: reattach ring adds nothing new"
            );
            return;
        }
        let skip = if have > start {
            (have - start) as usize
        } else {
            if have < start {
                let lost = start - have;
                warn!(
                    grant_jti = %self.grant_jti,
                    lost_bytes = lost,
                    "remote pane: reattach ring starts past the last byte seen — output was lost while detached"
                );
                self.push_local(&lost_output_marker(lost));
            }
            0
        };
        // `push_output` advances the offset by the delivered length; align it
        // to the ring's own start first so the arithmetic lands on `end`.
        self.remote_offset
            .store(start.saturating_add(skip as u64), Ordering::Release);
        self.push_output(&ring.buffer[skip..]);
    }

    /// Settle the exit code (first writer wins) and close the output channel
    /// so a reader blocked in `read()` sees EOF.
    pub fn mark_exit(&self, code: i32) {
        if let Ok(mut slot) = self.exit.lock() {
            if slot.is_none() {
                *slot = Some(code);
            }
        }
        self.exit_cv.notify_all();
        self.close_output();
    }

    /// A `remote_terminal_error` for this pane: logged, then treated as an
    /// exit with [`ERROR_EXIT_CODE`].
    pub fn mark_error(&self, code: &str, message: &str) {
        warn!(
            grant_jti = %self.grant_jti,
            terminal_id = %self.terminal_id,
            code,
            message,
            "remote pane: target reported an error — closing the pane"
        );
        self.mark_exit(ERROR_EXIT_CODE);
    }

    fn close_output(&self) {
        if let Ok(mut tx) = self.output_tx.lock() {
            drop(tx.take());
        }
    }

    fn send(&self, frame: Value) -> Result<(), String> {
        self.sink.send_frame(frame)
    }

    fn send_detach_once(&self) -> Result<(), String> {
        if self.detach_sent.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.send(json!({
            "type": "remote_terminal_detach",
            "grant_jti": self.grant_jti,
            "terminal_id": self.terminal_id,
        }))
    }

    /// The `remote_terminal_attach` frame a reconnect re-presents for this
    /// pane. `request_id` is the caller's correlation key.
    pub fn reattach_frame(&self, request_id: &str) -> Value {
        let (cols, rows) = self.dims();
        json!({
            "type": "remote_terminal_attach",
            "request_id": request_id,
            "grant": self.grant,
            "cols": cols,
            "rows": rows,
        })
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

/// `Write` that ships each write as one `remote_terminal_input` frame.
struct FrameWriter {
    grant_jti: String,
    terminal_id: String,
    sink: Arc<dyn RemoteFrameSink>,
}

impl Write for FrameWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.sink
            .send_frame(json!({
                "type": "remote_terminal_input",
                "grant_jti": self.grant_jti,
                "terminal_id": self.terminal_id,
                "data": STANDARD.encode(buf),
            }))
            .map_err(std::io::Error::other)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl PaneIo for RemotePaneIo {
    fn reader(&self) -> Result<Box<dyn Read + Send>, String> {
        let rx = self
            .output_rx
            .lock()
            .map_err(|e| format!("Remote pane reader lock poisoned: {}", e))?
            .take()
            .ok_or_else(|| "remote pane reader already taken".to_string())?;
        Ok(Box::new(ChannelReader {
            rx,
            pending: Vec::new(),
            pos: 0,
        }))
    }

    fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
        Ok(Box::new(FrameWriter {
            grant_jti: self.grant_jti.clone(),
            terminal_id: self.terminal_id.clone(),
            sink: self.sink.clone(),
        }))
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        self.send(json!({
            "type": "remote_terminal_resize",
            "grant_jti": self.grant_jti,
            "terminal_id": self.terminal_id,
            "cols": cols,
            "rows": rows,
        }))
    }

    fn wait(&self) -> Result<i32, String> {
        let mut slot = self
            .exit
            .lock()
            .map_err(|e| format!("Remote pane exit lock poisoned: {}", e))?;
        loop {
            if let Some(code) = *slot {
                return Ok(code);
            }
            slot = self
                .exit_cv
                .wait(slot)
                .map_err(|e| format!("Remote pane exit lock poisoned: {}", e))?;
        }
    }

    fn kill(&self, _budget: Duration) -> Result<(), String> {
        // A local kill detaches; it never terminates the remote process.
        let sent = self.send_detach_once();
        self.mark_exit(DETACH_EXIT_CODE);
        sent
    }

    fn set_paused(&self, paused: bool) -> Result<(), String> {
        self.send(json!({
            "type": "remote_terminal_flow",
            "grant_jti": self.grant_jti,
            "terminal_id": self.terminal_id,
            "paused": paused,
        }))
    }

    fn pid(&self) -> Option<u32> {
        None
    }

    fn credential_scrub(&self) -> CredentialScrub {
        CredentialScrub::NoChildEnv
    }

    fn release(&self, _budget: Duration) -> Result<(), String> {
        // Closing the channel unblocks a reader parked in `recv()`; the
        // detach tells the target this viewer is gone.
        self.close_output();
        self.send_detach_once()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::thread;

    /// Records every frame a pane sends, for assertions.
    #[derive(Default)]
    pub(crate) struct RecordingSink {
        pub frames: Mutex<Vec<Value>>,
    }

    impl RemoteFrameSink for RecordingSink {
        fn send_frame(&self, frame: Value) -> Result<(), String> {
            self.frames.lock().unwrap().push(frame);
            Ok(())
        }
    }

    impl RecordingSink {
        pub(crate) fn frames(&self) -> Vec<Value> {
            self.frames.lock().unwrap().clone()
        }
    }

    fn pane(sink: &Arc<RecordingSink>, seed: AttachedRing) -> RemotePaneIo {
        let dyn_sink: Arc<dyn RemoteFrameSink> = sink.clone();
        RemotePaneIo::new("jti-1", "term-9", "grant.jwt", dyn_sink, 100, 40, seed)
    }

    fn read_to_end_blocking(mut r: Box<dyn Read + Send>) -> Vec<u8> {
        let mut out = Vec::new();
        r.read_to_end(&mut out).expect("read to EOF");
        out
    }

    /// The seed ring is the first thing the reader yields, then live output
    /// frames, then EOF once the exit lands — and the exit code reaches `wait`.
    #[test]
    fn output_frames_reach_the_reader_and_exit_settles_wait() {
        let sink = Arc::new(RecordingSink::default());
        let pane = Arc::new(pane(
            &sink,
            AttachedRing {
                buffer: b"seed:".to_vec(),
                start_offset: 100,
                total_bytes_produced: 105,
                history_start: None,
            },
        ));
        assert_eq!(pane.remote_offset(), 105);
        assert_eq!(pane.history_range(), None);

        let reader = pane.reader().expect("reader");
        let feeder = pane.clone();
        let t = thread::spawn(move || {
            feeder.push_output(b"hello ");
            feeder.push_output(b"world");
            feeder.mark_exit(7);
        });
        let bytes = read_to_end_blocking(reader);
        t.join().unwrap();

        assert_eq!(bytes, b"seed:hello world");
        assert_eq!(pane.remote_offset(), 105 + 11);
        assert_eq!(pane.wait(), Ok(7));
        assert!(pane.is_finished());
        // A second reader is refused — the first owns the channel.
        assert!(pane.reader().is_err());
    }

    /// Every write becomes one `remote_terminal_input` frame carrying the
    /// bytes base64-encoded under the pane's jti + terminal id.
    #[test]
    fn writer_ships_input_frames() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        let mut w = pane.writer().expect("writer");
        w.write_all(b"ls -la\r").unwrap();
        w.flush().unwrap();

        let frames = sink.frames();
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert_eq!(frames[0]["type"], "remote_terminal_input");
        assert_eq!(frames[0]["grant_jti"], "jti-1");
        assert_eq!(frames[0]["terminal_id"], "term-9");
        assert_eq!(frames[0]["data"], STANDARD.encode(b"ls -la\r"));
    }

    /// resize / set_paused / kill / release map to the contract's frames;
    /// detach is sent exactly once even when kill AND release both run, and a
    /// local kill settles `wait` with the detach code.
    #[test]
    fn control_calls_map_to_contract_frames() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        pane.resize(132, 50).unwrap();
        pane.set_paused(true).unwrap();
        pane.set_paused(false).unwrap();
        pane.kill(Duration::from_millis(10)).unwrap();
        pane.release(Duration::from_millis(10)).unwrap();

        let types: Vec<String> = sink
            .frames()
            .iter()
            .map(|f| f["type"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            types,
            vec![
                "remote_terminal_resize",
                "remote_terminal_flow",
                "remote_terminal_flow",
                "remote_terminal_detach",
            ]
        );
        let frames = sink.frames();
        assert_eq!(frames[0]["cols"], 132);
        assert_eq!(frames[0]["rows"], 50);
        assert_eq!(pane.dims(), (132, 50));
        assert_eq!(frames[1]["paused"], true);
        assert_eq!(frames[2]["paused"], false);
        assert_eq!(pane.wait(), Ok(DETACH_EXIT_CODE));
        assert_eq!(pane.pid(), None);
        assert_eq!(pane.credential_scrub(), CredentialScrub::NoChildEnv);
        // The reader sees EOF after release even with no exit frame.
        let bytes = read_to_end_blocking(pane.reader().unwrap());
        assert!(bytes.is_empty());
    }

    /// A target error closes the pane with the error exit code.
    #[test]
    fn remote_error_closes_reader_with_error_code() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        let reader = pane.reader().unwrap();
        pane.mark_error("session_not_local", "no such session here");
        assert!(read_to_end_blocking(reader).is_empty());
        assert_eq!(pane.wait(), Ok(ERROR_EXIT_CODE));
    }

    /// Reattach splice: bytes already delivered are skipped; a ring that
    /// starts past the last seen byte is delivered whole (lost window logged).
    #[test]
    fn splice_replay_skips_what_was_already_seen() {
        let sink = Arc::new(RecordingSink::default());
        let pane = Arc::new(pane(
            &sink,
            AttachedRing {
                buffer: b"0123456789".to_vec(),
                start_offset: 0,
                total_bytes_produced: 10,
                history_start: Some(0),
            },
        ));
        let reader = pane.reader().unwrap();
        // Ring [5, 15): we have [0, 10) → only "ABCDE" is new.
        pane.splice_replay(&AttachedRing {
            buffer: b"56789ABCDE".to_vec(),
            start_offset: 5,
            total_bytes_produced: 15,
            history_start: None,
        });
        assert_eq!(pane.remote_offset(), 15);
        // Ring [10, 15): entirely seen → nothing.
        pane.splice_replay(&AttachedRing {
            buffer: b"ABCDE".to_vec(),
            start_offset: 10,
            total_bytes_produced: 15,
            history_start: None,
        });
        assert_eq!(pane.remote_offset(), 15);
        // Ring [20, 23): a 5-byte gap → whole ring delivered, offset = 23.
        pane.splice_replay(&AttachedRing {
            buffer: b"XYZ".to_vec(),
            start_offset: 20,
            total_bytes_produced: 23,
            history_start: None,
        });
        assert_eq!(pane.remote_offset(), 23);
        pane.mark_exit(0);
        // The 5-byte gap is announced IN the stream, before the ring that
        // followed it — and the marker moves no remote offset.
        let mut expected = b"0123456789ABCDE".to_vec();
        expected.extend_from_slice(&lost_output_marker(5));
        expected.extend_from_slice(b"XYZ");
        assert_eq!(read_to_end_blocking(reader), expected);
    }

    /// Lazy scrollback: the history range is exactly the target ring below
    /// the seed, and absent when the seed began at the ring's first byte or
    /// the target reported no ring start.
    #[test]
    fn history_range_is_the_ring_below_the_seed() {
        let sink = Arc::new(RecordingSink::default());
        let with = pane(
            &sink,
            AttachedRing {
                buffer: b"tail".to_vec(),
                start_offset: 1_000,
                total_bytes_produced: 1_004,
                history_start: Some(200),
            },
        );
        assert_eq!(with.history_range(), Some((200, 1_000)));
        let flush = pane(
            &sink,
            AttachedRing {
                buffer: b"tail".to_vec(),
                start_offset: 1_000,
                total_bytes_produced: 1_004,
                history_start: Some(1_000),
            },
        );
        assert_eq!(flush.history_range(), None);
        let unknown = pane(&sink, AttachedRing::default());
        assert_eq!(unknown.history_range(), None);
    }

    /// A relay drop writes the in-band notice and leaves the pane OPEN: no
    /// exit code, no detach frame, the remote offset untouched.
    #[test]
    fn relay_loss_is_announced_without_closing_the_pane() {
        let sink = Arc::new(RecordingSink::default());
        let pane = Arc::new(pane(
            &sink,
            AttachedRing {
                buffer: b"abc".to_vec(),
                start_offset: 0,
                total_bytes_produced: 3,
                history_start: None,
            },
        ));
        let reader = pane.reader().unwrap();
        pane.note_relay_lost();
        assert!(!pane.is_finished());
        assert_eq!(pane.remote_offset(), 3);
        assert!(sink.frames().is_empty());
        pane.mark_exit(0);
        let mut expected = b"abc".to_vec();
        expected.extend_from_slice(RELAY_LOST_MARKER);
        assert_eq!(read_to_end_blocking(reader), expected);
    }

    /// The reattach frame re-presents the grant with the CURRENT viewport.
    #[test]
    fn reattach_frame_carries_grant_and_current_dims() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        pane.resize(90, 30).unwrap();
        let f = pane.reattach_frame("reattach:jti-1");
        assert_eq!(f["type"], "remote_terminal_attach");
        assert_eq!(f["request_id"], "reattach:jti-1");
        assert_eq!(f["grant"], "grant.jwt");
        assert_eq!(f["cols"], 90);
        assert_eq!(f["rows"], 30);
    }
}
