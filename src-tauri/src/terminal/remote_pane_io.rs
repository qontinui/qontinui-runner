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
//!   queued into the channel [`PaneIo::output`] hands the reader thread.
//!   `remote_terminal_exit` and `remote_terminal_error` close that channel
//!   (the receiver disconnects) and settle the
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

use std::io::Write;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use tracing::{debug, warn};

use super::pane_io::{CredentialScrub, PaneIo};
use super::pane_output::PaneOutput;

/// Exit code `wait` reports after a LOCAL detach (`kill`/`release`).
pub const DETACH_EXIT_CODE: i32 = 0;
/// Exit code `wait` reports after a `remote_terminal_error`.
pub const ERROR_EXIT_CODE: i32 = 1;

/// What happened to this pane's `remote_terminal_detach` — the one frame
/// that asks the relay to drop the `(target, terminal)` binding.
///
/// Plan `2026-09-16-remote-tab-cannot-be-released-so-the-target-terminal-stays-claimed`
/// (Phase 1). `Queued` means the frame was accepted by the relay's outbound
/// queue — NOT that the relay or the target acted on it; only a re-attach of
/// the same terminal observes that. A close reports this value rather than a
/// bare success, because a detach that never queued leaves the binding held
/// until the source socket drops or the grant expires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachOutcome {
    /// No detach has been tried (the pane was never closed).
    NotAttempted,
    /// The frame was accepted by the outbound queue. Terminal: no second
    /// detach is ever queued for this pane.
    Queued,
    /// The last attempt could not queue the frame; carries the sink's error.
    /// Not terminal — the next `kill`/`release` tries again.
    Failed(String),
}

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

/// Wall-clock milliseconds since the Unix epoch — the clock every
/// [`RemoteInteractivity`] timestamp is on, so the frontend's "Ns ago" reads
/// the SOURCE's own clock and never compares across machines.
fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The last `remote_terminal_input` this pane queued (plan
/// `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
/// A1). `at_ms` is when the frame was accepted by the relay's outbound queue.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSent {
    pub seq: u64,
    pub at_ms: u64,
    pub bytes: u64,
}

/// The last `remote_terminal_input_ack` the target returned for this pane.
/// `at_ms` is when it ARRIVED here (source clock); `target_accepted_at` is the
/// target's own RFC3339 stamp, carried for the record and never compared.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputAcked {
    /// `None` when the ack carried no `seq` (a relay that dropped it).
    pub seq: Option<u64>,
    pub at_ms: u64,
    pub bytes: u64,
    pub accepted: bool,
    /// The target's closed error code, present only when `accepted` is false.
    pub error: Option<String>,
    /// `traffic` or `probe`.
    pub via: String,
    pub target_accepted_at: Option<String>,
}

/// The last frame from the target this pane spliced (`attached` seed, live
/// `output`, a `buffer` resync or a reattach ring). `through_offset` is
/// [`RemotePaneIo::remote_offset`] after the splice — the read half's proof.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameReceived {
    pub at_ms: u64,
    pub through_offset: u64,
}

/// What this pane KNOWS about whether its remote session is readable and
/// writable from here — served by `terminal_remote_interactivity`.
///
/// Every field is an observation or `None`; nothing is inferred. In particular
/// `acks_received == 0` with inputs sent is not "input failed": a target that
/// predates acknowledgements never sends one, and the tab footer says so.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInteractivity {
    /// When this pane was attached (constructed from the attach reply).
    pub attached_at_ms: u64,
    pub last_input_sent: Option<InputSent>,
    /// The last ack for a KEYSTROKE (`via: traffic`). Probe acks never land
    /// here: an accepted probe must not hide a refused keystroke, nor count as
    /// acknowledging keystrokes sent before it.
    pub last_input_acked: Option<InputAcked>,
    /// The last ack for a write PROBE (`via: probe`).
    pub last_probe_acked: Option<InputAcked>,
    /// Acks received over this pane's whole life, accepted or not.
    pub acks_received: u64,
    /// Acks received since the CURRENT attachment was (re)established — reset
    /// on every reattach, because the target behind a reattach may be a
    /// different build. Gates [`RemotePaneIo::send_input_probe`].
    pub acks_since_attach: u64,
    /// The last write probe queued (never a keystroke).
    pub last_probe_sent: Option<InputSent>,
    pub last_frame_received: Option<FrameReceived>,
}

/// The refusal [`RemotePaneIo::send_input_probe`] answers until the target has
/// proven it acknowledges input on this attachment. Same spelling as the
/// coord `unknown` reason, so a caller can report it verbatim.
pub const TARGET_PREDATES_INPUT_ACK: &str = "target_predates_input_ack";

/// The mutable half of [`RemoteInteractivity`], shared between the pane and
/// every writer it hands out.
#[derive(Debug)]
struct InteractivityState {
    /// The next `seq` a `remote_terminal_input` carries. Starts at 1 and only
    /// ever increments, under this lock, so seq is strictly increasing per
    /// grant (one pane == one grant) whichever writer sends.
    next_seq: u64,
    snapshot: RemoteInteractivity,
}

/// A [`PaneIo`] over the backend relay for one remote terminal.
pub struct RemotePaneIo {
    grant_jti: String,
    terminal_id: String,
    /// The grant JWT, kept so a relay reconnect can re-present it.
    grant: String,
    sink: Arc<dyn RemoteFrameSink>,
    /// The output channel, the absolute target offset of the next byte this
    /// pane expects (the reconnect splice point), and the exit slot — shared
    /// machinery with `DaemonPaneIo` (see [`PaneOutput`]), keyed by
    /// `grant_jti`.
    out: PaneOutput,
    /// `remote_terminal_detach` is QUEUED at most once per pane. A failed
    /// attempt does not latch, so the `release()` that follows a failed
    /// `kill()` on the close path retries it.
    detach: Mutex<DetachOutcome>,
    cols: AtomicU16,
    rows: AtomicU16,
    /// Absolute target offset of the first seed byte — the upper bound of the
    /// history the target still holds but did not ship at attach.
    seed_start: u64,
    /// See [`AttachedRing::history_start`].
    history_start: Option<u64>,
    /// Input seq + the read/write receipts. See [`RemoteInteractivity`].
    interactivity: Arc<Mutex<InteractivityState>>,
    /// Where this pane REPORTS what it measures (plan
    /// `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
    /// A2): set once, by the attach flow that knows the coord session and the
    /// grant. Unset — a test pane, or the probe sweep, which reports its own
    /// outcome explicitly — reports nothing.
    report: OnceLock<(
        crate::mcp::remote_interactivity::SourceReportContext,
        Arc<crate::mcp::remote_interactivity::Reporter>,
    )>,
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
        debug!(
            seed_bytes = seed.buffer.len(),
            start_offset = seed.start_offset,
            target_total = seed.total_bytes_produced,
            "remote pane: seeded from the target's ring"
        );
        let grant_jti = grant_jti.into();
        let seed_start = seed.start_offset;
        let history_start = seed.history_start;
        let out = PaneOutput::new(
            grant_jti.clone(),
            seed.buffer,
            seed.start_offset,
            lost_output_marker,
        );
        let next_offset = out.offset();
        Self {
            grant_jti,
            terminal_id: terminal_id.into(),
            grant: grant.into(),
            sink,
            out,
            detach: Mutex::new(DetachOutcome::NotAttempted),
            cols: AtomicU16::new(cols),
            rows: AtomicU16::new(rows),
            seed_start,
            history_start,
            interactivity: Arc::new(Mutex::new(InteractivityState {
                next_seq: 1,
                snapshot: RemoteInteractivity {
                    attached_at_ms: now_epoch_ms(),
                    last_input_sent: None,
                    last_input_acked: None,
                    last_probe_acked: None,
                    acks_received: 0,
                    acks_since_attach: 0,
                    last_probe_sent: None,
                    // The attach reply IS a frame from the target: the seed
                    // ring (possibly empty) was received and spliced.
                    last_frame_received: Some(FrameReceived {
                        at_ms: now_epoch_ms(),
                        through_offset: next_offset,
                    }),
                },
            })),
            report: OnceLock::new(),
        }
    }

    /// Start reporting this pane's measurements to coord through the
    /// process-wide reporter. See [`Self::attach_report_context_to`].
    pub fn attach_report_context(
        &self,
        ctx: crate::mcp::remote_interactivity::SourceReportContext,
    ) {
        self.attach_report_context_to(ctx, crate::mcp::remote_interactivity::reporter().clone());
    }

    /// Start reporting to `reporter`. The attach reply that built this pane IS
    /// a frame from the target, so the read half is reported at once; every
    /// later spliced frame offers another `read: ok`, which the reporter
    /// coalesces to one row per `REPORT_EVERY` — never one per frame. A second
    /// call is ignored (the context is the grant's, and a pane has one grant).
    pub fn attach_report_context_to(
        &self,
        ctx: crate::mcp::remote_interactivity::SourceReportContext,
        reporter: Arc<crate::mcp::remote_interactivity::Reporter>,
    ) {
        if self.report.set((ctx, reporter)).is_ok() {
            self.report_read_ok();
        }
    }

    fn report_read_ok(&self) {
        if let Some((ctx, reporter)) = self.report.get() {
            reporter.observe(ctx.read_ok(chrono::Utc::now()));
        }
    }

    /// A snapshot of what this pane has observed about its session's
    /// interactivity. See [`RemoteInteractivity`].
    pub fn interactivity(&self) -> RemoteInteractivity {
        match self.interactivity.lock() {
            Ok(g) => g.snapshot.clone(),
            Err(poisoned) => poisoned.into_inner().snapshot.clone(),
        }
    }

    /// Record a `remote_terminal_input_ack` routed here by `grant_jti`, into
    /// the traffic or the probe slot by its `via`.
    ///
    /// Recorded as it arrives: the relay preserves order per attachment, so the
    /// newest ack is the newest answer. `seq` is echoed from the frame the
    /// target admitted (`null` when a relay dropped it on the way).
    pub fn record_input_ack(&self, ack: &Value) {
        let accepted = ack
            .get("accepted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let acked = InputAcked {
            seq: ack.get("seq").and_then(|v| v.as_u64()),
            at_ms: now_epoch_ms(),
            bytes: ack.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0),
            accepted,
            error: if accepted {
                None
            } else {
                Some(
                    ack.get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("input_rejected")
                        .to_string(),
                )
            },
            via: ack
                .get("via")
                .and_then(|v| v.as_str())
                .unwrap_or("traffic")
                .to_string(),
            target_accepted_at: ack
                .get("accepted_at")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        };
        if !accepted {
            warn!(
                grant_jti = %self.grant_jti,
                terminal_id = %self.terminal_id,
                seq = ?acked.seq,
                error = ?acked.error,
                detail = ack.get("error_detail").and_then(|v| v.as_str()).unwrap_or(""),
                "remote pane: the target refused input it had admitted"
            );
        }
        let mut g = self.interactivity.lock().unwrap_or_else(|e| e.into_inner());
        g.snapshot.acks_received = g.snapshot.acks_received.saturating_add(1);
        // Any ack — probe or traffic — proves the target speaks the protocol.
        g.snapshot.acks_since_attach = g.snapshot.acks_since_attach.saturating_add(1);
        if acked.via == "probe" {
            g.snapshot.last_probe_acked = Some(acked);
        } else {
            g.snapshot.last_input_acked = Some(acked);
        }
    }

    /// The relay re-established this pane's attachment (a reattach after a
    /// relay drop). The target behind it may now be a different build, so
    /// what it proved about acknowledging input no longer holds.
    pub fn note_reattached(&self) {
        let mut g = self.interactivity.lock().unwrap_or_else(|e| e.into_inner());
        g.snapshot.acks_since_attach = 0;
    }

    /// Queue a zero-byte WRITE PROBE (`probe: true`) — plan
    /// `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`.
    /// Returns the probe's `seq`; its answer arrives as an ordinary input ack
    /// with `via: "probe"`.
    ///
    /// REFUSED with [`TARGET_PREDATES_INPUT_ACK`] until this attachment has
    /// received at least one ack. A target that predates A1 ignores `probe`
    /// and would run `write_input(terminal_id, b"")` — a real write that lands
    /// a 0-byte observation in the session's `last_input` and masks the
    /// phantom turns that slot exists to catch. Only a target that has ALREADY
    /// acked on this attachment is known to honour the flag.
    pub fn send_input_probe(&self) -> Result<u64, String> {
        self.send_input_probe_given(false)
    }

    /// [`Self::send_input_probe`], with `target_known_to_ack` standing in for
    /// the prior ack when something OTHER than this attachment has positively
    /// established that the target honours `probe` — coord's target readiness
    /// answering `supports` for input acknowledgement. An unknown capability
    /// is `false`, never a guess: the probe sweep passes exactly that.
    pub fn send_input_probe_given(&self, target_known_to_ack: bool) -> Result<u64, String> {
        let mut g = self.interactivity.lock().unwrap_or_else(|e| e.into_inner());
        if g.snapshot.acks_since_attach == 0 && !target_known_to_ack {
            return Err(format!(
                "{TARGET_PREDATES_INPUT_ACK}: no input ack has been received on this attachment, \
                 so the target is not known to honour `probe` — an older build would write the \
                 probe into the session as input"
            ));
        }
        let seq = g.next_seq;
        g.next_seq = g.next_seq.saturating_add(1);
        self.sink.send_frame(json!({
            "type": "remote_terminal_input",
            "grant_jti": self.grant_jti,
            "terminal_id": self.terminal_id,
            "data": "",
            "seq": seq,
            "probe": true,
        }))?;
        g.snapshot.last_probe_sent = Some(InputSent {
            seq,
            at_ms: now_epoch_ms(),
            bytes: 0,
        });
        Ok(seq)
    }

    /// Stamp the read half: a frame from the target was just spliced.
    fn note_frame_received(&self) {
        let through_offset = self.remote_offset();
        {
            let mut g = self.interactivity.lock().unwrap_or_else(|e| e.into_inner());
            g.snapshot.last_frame_received = Some(FrameReceived {
                at_ms: now_epoch_ms(),
                through_offset,
            });
        }
        self.report_read_ok();
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
        self.out.offset()
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
        self.out.is_finished()
    }

    /// Queue one output chunk from the target (already decoded). Silently
    /// dropped once the pane is closed — a late frame after exit has nowhere
    /// to go, exactly like bytes after a local PTY's EOF.
    pub fn push_output(&self, bytes: &[u8]) {
        if self.out.push_output(bytes) {
            self.note_frame_received();
        }
    }

    /// Queue bytes that originate HERE rather than on the target — an in-band
    /// notice — without moving the remote offset, so the splice arithmetic
    /// stays anchored to the target's stream.
    pub fn push_local(&self, bytes: &[u8]) {
        self.out.push_local(bytes);
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
        // A delivered ring, or one adding nothing while the pane is open, is a
        // frame the target answered with — the read half holds. After exit it
        // went nowhere and is not a receipt (`PaneOutput::splice` decides).
        if self.out.splice(ring.start_offset, &ring.buffer) {
            self.note_frame_received();
        }
    }

    /// Settle the exit code (first writer wins) and close the output channel
    /// so a reader blocked on the receiver sees it disconnect.
    pub fn mark_exit(&self, code: i32) {
        self.out.settle(Ok(code));
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
        // Only FATAL codes reach here (`settle_pane_error`), i.e. the
        // attachment is gone: neither half works from this source any more.
        // The reporter's classifier files a wire refusal as `failed`, a busy
        // terminal / unreachable target as `unknown`, and a local code not at
        // all.
        if let Some((ctx, reporter)) = self.report.get() {
            for obs in ctx.refusal(
                code,
                &crate::mcp::remote_interactivity::Half::BOTH,
                chrono::Utc::now(),
            ) {
                reporter.observe(obs);
            }
        }
        self.mark_exit(ERROR_EXIT_CODE);
    }

    fn close_output(&self) {
        self.out.close_output();
    }

    fn send(&self, frame: Value) -> Result<(), String> {
        self.sink.send_frame(frame)
    }

    /// What this pane's detach has come to so far. See [`DetachOutcome`].
    pub fn detach_outcome(&self) -> DetachOutcome {
        match self.detach.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Queue `remote_terminal_detach` unless one is already queued. The lock
    /// is held across the (non-blocking) queue attempt so a concurrent
    /// `kill`/`release` cannot queue a second frame. Only a successful queue
    /// latches: a failure is recorded and left retryable.
    fn send_detach_once(&self) -> Result<(), String> {
        let mut slot = match self.detach.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *slot == DetachOutcome::Queued {
            return Ok(());
        }
        let sent = self.send(json!({
            "type": "remote_terminal_detach",
            "grant_jti": self.grant_jti,
            "terminal_id": self.terminal_id,
        }));
        *slot = match &sent {
            Ok(()) => DetachOutcome::Queued,
            Err(e) => {
                warn!(
                    grant_jti = %self.grant_jti,
                    terminal_id = %self.terminal_id,
                    error = %e,
                    "remote pane: remote_terminal_detach could not be queued — the relay keeps the binding until the source socket drops or the grant expires"
                );
                DetachOutcome::Failed(e.clone())
            }
        };
        sent
    }

    /// The `remote_terminal_attach` frame a reconnect re-presents for this
    /// pane. `request_id` is the caller's correlation key.
    /// The frame that re-presents this pane's grant after a relay drop.
    ///
    /// `have_offset` is the absolute offset of the last byte this pane has
    /// delivered, and it is what stops a reconnect from claiming a loss that
    /// did not happen. A plain attach ships the ring's last
    /// `REMOTE_ATTACH_TAIL_BYTES` because a fresh tab has no history to
    /// reconcile; a REattach that got the same bounded tail would look, to
    /// `splice_replay`, exactly like a ring that had rolled past what we hold
    /// — so a 20-second drop over a chatty session wrote a "N bytes of output
    /// were lost here" marker for bytes the target's ring still held. Telling
    /// the target where to start makes the reported loss the real one.
    pub fn reattach_frame(&self, request_id: &str) -> Value {
        let (cols, rows) = self.dims();
        json!({
            "type": "remote_terminal_attach",
            "request_id": request_id,
            "grant": self.grant,
            "cols": cols,
            "rows": rows,
            "have_offset": self.remote_offset(),
        })
    }
}

/// `Write` that ships each write as one `remote_terminal_input` frame,
/// stamped with the pane's next `seq`.
struct FrameWriter {
    grant_jti: String,
    terminal_id: String,
    sink: Arc<dyn RemoteFrameSink>,
    interactivity: Arc<Mutex<InteractivityState>>,
}

impl Write for FrameWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // The lock is held across the (non-blocking) queue attempt so seq
        // order on the wire is seq order here, whichever writer sends. A seq
        // whose frame failed to queue is consumed anyway: seq need only be
        // strictly increasing, and reusing one would let a late ack for it
        // acknowledge a different keystroke.
        let mut g = self.interactivity.lock().unwrap_or_else(|e| e.into_inner());
        let seq = g.next_seq;
        g.next_seq = g.next_seq.saturating_add(1);
        self.sink
            .send_frame(json!({
                "type": "remote_terminal_input",
                "grant_jti": self.grant_jti,
                "terminal_id": self.terminal_id,
                "data": STANDARD.encode(buf),
                "seq": seq,
            }))
            .map_err(std::io::Error::other)?;
        g.snapshot.last_input_sent = Some(InputSent {
            seq,
            at_ms: now_epoch_ms(),
            bytes: buf.len() as u64,
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl PaneIo for RemotePaneIo {
    fn output(&self) -> Result<std::sync::mpsc::Receiver<Vec<u8>>, String> {
        self.out.take_output()
    }

    fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
        Ok(Box::new(FrameWriter {
            grant_jti: self.grant_jti.clone(),
            terminal_id: self.terminal_id.clone(),
            sink: self.sink.clone(),
            interactivity: self.interactivity.clone(),
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
        self.out.wait()
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

    fn read_to_end_blocking(rx: std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
        rx.iter().flatten().collect()
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

        let reader = pane.output().expect("reader");
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
        assert!(pane.output().is_err());
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

    /// A1: every input frame carries a strictly increasing `seq` — across
    /// writers too, since the counter is the pane's, not the writer's — and
    /// the last one sent is what `interactivity()` reports.
    #[test]
    fn input_frames_carry_a_strictly_increasing_seq() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        assert_eq!(pane.interactivity().last_input_sent, None);
        let mut w1 = pane.writer().expect("writer");
        let mut w2 = pane.writer().expect("second writer");
        w1.write_all(b"a").unwrap();
        w2.write_all(b"bc").unwrap();
        w1.write_all(b"d").unwrap();
        let seqs: Vec<u64> = sink
            .frames()
            .iter()
            .map(|f| f["seq"].as_u64().expect("seq on every input frame"))
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        let sent = pane.interactivity().last_input_sent.expect("sent");
        assert_eq!((sent.seq, sent.bytes), (3, 1));
    }

    /// A frame the relay could not queue (backlog full) still CONSUMES its seq
    /// — reusing it would let a late ack for it acknowledge a different
    /// keystroke — but is not recorded as sent, and the write errors.
    #[test]
    fn a_frame_that_fails_to_queue_consumes_its_seq_but_is_not_recorded_sent() {
        struct FlakySink {
            fail_next: Mutex<bool>,
            frames: Mutex<Vec<Value>>,
        }
        impl RemoteFrameSink for FlakySink {
            fn send_frame(&self, frame: Value) -> Result<(), String> {
                let mut f = self.fail_next.lock().unwrap();
                if *f {
                    *f = false;
                    return Err("remote attach: relay outbound backlog is full".into());
                }
                self.frames.lock().unwrap().push(frame);
                Ok(())
            }
        }
        let sink = Arc::new(FlakySink {
            fail_next: Mutex::new(true),
            frames: Mutex::new(Vec::new()),
        });
        let dyn_sink: Arc<dyn RemoteFrameSink> = sink.clone();
        let pane = RemotePaneIo::new("j", "t", "g", dyn_sink, 80, 24, AttachedRing::default());
        let mut w = pane.writer().unwrap();
        assert!(
            w.write(b"a").is_err(),
            "a frame that did not queue must error"
        );
        assert_eq!(pane.interactivity().last_input_sent, None);
        w.write_all(b"b").unwrap();
        let frames = sink.frames.lock().unwrap().clone();
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0]["seq"], 2,
            "seq 1 was consumed by the failed frame"
        );
        assert_eq!(pane.interactivity().last_input_sent.map(|s| s.seq), Some(2));
    }

    /// Probe acks land in their own slot: an accepted probe after a refused
    /// keystroke leaves the keystroke's refusal visible.
    #[test]
    fn a_probe_ack_does_not_overwrite_the_keystroke_ack() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        pane.record_input_ack(&json!({
            "seq": 4, "accepted": false, "error": "terminal_exited", "via": "traffic",
        }));
        pane.record_input_ack(&json!({"seq": 5, "accepted": true, "via": "probe", "bytes": 0}));
        let i = pane.interactivity();
        let key = i.last_input_acked.expect("keystroke ack");
        assert_eq!((key.seq, key.accepted), (Some(4), false));
        assert_eq!(i.last_probe_acked.map(|p| p.seq), Some(Some(5)));
        assert_eq!(
            i.acks_since_attach, 2,
            "a probe ack still proves the protocol"
        );
    }

    /// A1: an ack updates `last_input_acked` (source-clock arrival time,
    /// the target's code on a refusal) and counts toward `acks_received`.
    #[test]
    fn an_input_ack_is_recorded_on_the_pane() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        assert_eq!(pane.interactivity().acks_received, 0);
        pane.record_input_ack(&json!({
            "type": "remote_terminal_input_ack", "seq": 4, "bytes": 3,
            "accepted": true, "via": "traffic", "accepted_at": "2026-09-27T00:00:00.000Z",
        }));
        let i = pane.interactivity();
        let acked = i.last_input_acked.expect("acked");
        assert_eq!(acked.seq, Some(4));
        assert_eq!(acked.bytes, 3);
        assert!(acked.accepted);
        assert_eq!(acked.error, None);
        assert_eq!(acked.via, "traffic");
        assert!(acked.at_ms > 0);
        assert_eq!(i.acks_received, 1);

        pane.record_input_ack(&json!({
            "seq": 5, "accepted": false, "error": "terminal_exited", "via": "traffic",
        }));
        let i = pane.interactivity();
        let acked = i.last_input_acked.expect("acked");
        assert!(!acked.accepted);
        assert_eq!(acked.error.as_deref(), Some("terminal_exited"));
        assert_eq!(i.acks_received, 2);
    }

    /// A write probe is REFUSED until the target has acked on this attachment
    /// — an older target would write it into the session as real input — and
    /// the refusal resets on a reattach, whose target may be another build.
    #[test]
    fn a_probe_is_refused_until_the_target_has_acked_on_this_attachment() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        let err = pane.send_input_probe().expect_err("no ack yet");
        assert!(err.starts_with(TARGET_PREDATES_INPUT_ACK), "{err}");
        assert!(sink.frames().is_empty(), "a refused probe sends nothing");

        let mut w = pane.writer().unwrap();
        w.write_all(b"k").unwrap(); // seq 1
        pane.record_input_ack(&json!({"seq": 1, "bytes": 1, "accepted": true, "via": "traffic"}));
        let seq = pane.send_input_probe().expect("acked target may be probed");
        assert_eq!(seq, 2, "a probe takes the next seq in the same series");
        let probe = sink.frames().last().cloned().unwrap();
        assert_eq!(probe["type"], "remote_terminal_input");
        assert_eq!(probe["probe"], true);
        assert_eq!(probe["data"], "");
        assert_eq!(probe["seq"], 2);
        let i = pane.interactivity();
        assert_eq!(i.last_probe_sent.map(|p| p.seq), Some(2));
        assert_eq!(
            i.last_input_sent.map(|p| p.seq),
            Some(1),
            "a probe is not a keystroke"
        );

        pane.note_reattached();
        assert!(pane
            .send_input_probe()
            .unwrap_err()
            .starts_with(TARGET_PREDATES_INPUT_ACK));
        assert_eq!(
            pane.interactivity().acks_received,
            1,
            "lifetime count survives"
        );
    }

    /// A2 SOURCE role: a tab pane with a report context files `read: ok`
    /// (via traffic, under its grant, as this device) on attach, and a burst
    /// of spliced frames is ONE report, not one per frame; a fatal refusal
    /// files `failed` on both halves with the wire code verbatim.
    #[tokio::test]
    async fn a_reporting_pane_files_read_ok_coalesced_and_refusals_on_both_halves() {
        use crate::mcp::remote_interactivity::{
            FactState, Half, Observation, ObservationSender, Reporter, Role, SourceReportContext,
            Via, REPORT_EVERY,
        };
        #[derive(Default)]
        struct Rec(Mutex<Vec<Observation>>);
        #[async_trait::async_trait]
        impl ObservationSender for Rec {
            async fn send(&self, obs: &Observation) -> Result<(), String> {
                self.0.lock().unwrap().push(obs.clone());
                Ok(())
            }
        }
        let rec = Arc::new(Rec::default());
        let reporter = Reporter::new(rec.clone(), REPORT_EVERY, 64, false);
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        let ctx = SourceReportContext {
            session_id: uuid::Uuid::from_u128(1),
            grant_jti: uuid::Uuid::from_u128(2),
            source_device_id: uuid::Uuid::from_u128(3),
            via: Via::Traffic,
        };
        pane.attach_report_context_to(ctx.clone(), reporter.clone());
        for _ in 0..500 {
            pane.push_output(b"x");
        }
        pane.mark_error(
            "session_not_local",
            "no local terminal hosts that coord session",
        );
        while reporter.drain_once().await.is_some() {}
        let sent = rec.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 3, "one read-ok + two refusals: {sent:?}");
        assert_eq!(
            (sent[0].role, sent[0].half, sent[0].state, sent[0].via),
            (Role::Source, Half::Read, FactState::Ok, Some(Via::Traffic))
        );
        assert_eq!(sent[0].grant_jti, Some(ctx.grant_jti));
        assert_eq!(sent[0].source_device_id, ctx.source_device_id);
        for (o, half) in sent[1..].iter().zip([Half::Read, Half::Write]) {
            assert_eq!(o.half, half);
            assert_eq!(o.state, FactState::Failed);
            assert_eq!(o.reason.as_deref(), Some("session_not_local"));
        }
    }

    /// A pane with NO report context (a test pane, a probe pane) reports
    /// nothing — the probe sweep files its own outcome explicitly.
    #[test]
    fn a_pane_without_a_context_reports_nothing() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        pane.push_output(b"x");
        pane.mark_error("session_not_local", "gone");
        assert!(pane.report.get().is_none());
    }

    /// The write probe may go to a target that has not acked on this
    /// attachment only when the caller KNOWS it acks.
    #[test]
    fn a_probe_to_a_known_acker_needs_no_prior_ack() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        assert!(pane.send_input_probe_given(false).is_err());
        let seq = pane.send_input_probe_given(true).expect("known acker");
        let f = sink.frames().last().cloned().unwrap();
        assert_eq!(
            (f["probe"].clone(), f["data"].clone(), f["seq"].clone()),
            (json!(true), json!(""), json!(seq))
        );
    }

    /// A1 read half: the attach seed, live output and a reattach ring (even
    /// one adding nothing) each stamp `last_frame_received` with the offset
    /// the pane has delivered through.
    #[test]
    fn spliced_frames_stamp_last_frame_received() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(
            &sink,
            AttachedRing {
                buffer: b"seed".to_vec(),
                start_offset: 10,
                total_bytes_produced: 14,
                history_start: None,
            },
        );
        let seeded = pane.interactivity().last_frame_received.expect("seed");
        assert_eq!(seeded.through_offset, 14);
        pane.push_output(b"xyz");
        assert_eq!(
            pane.interactivity()
                .last_frame_received
                .unwrap()
                .through_offset,
            17
        );
        // A ring wholly behind what we have is still a receipt.
        pane.splice_replay(&AttachedRing {
            buffer: b"eedx".to_vec(),
            start_offset: 11,
            total_bytes_produced: 15,
            history_start: None,
        });
        assert_eq!(
            pane.interactivity()
                .last_frame_received
                .unwrap()
                .through_offset,
            17
        );
        // After exit, output goes nowhere and is NOT a receipt — neither a
        // live chunk nor a reattach ring that adds nothing.
        let before = pane.interactivity().last_frame_received;
        pane.mark_exit(0);
        std::thread::sleep(Duration::from_millis(2));
        pane.push_output(b"late");
        pane.splice_replay(&AttachedRing {
            buffer: b"x".to_vec(),
            start_offset: 11,
            total_bytes_produced: 12,
            history_start: None,
        });
        assert_eq!(pane.interactivity().last_frame_received, before);
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
        let bytes = read_to_end_blocking(pane.output().unwrap());
        assert!(bytes.is_empty());
    }

    /// A sink that refuses the first `fail_first` frames, then records.
    #[derive(Default)]
    struct FlakySink {
        fail_first: Mutex<usize>,
        frames: Mutex<Vec<Value>>,
    }

    impl RemoteFrameSink for FlakySink {
        fn send_frame(&self, frame: Value) -> Result<(), String> {
            let mut left = self.fail_first.lock().unwrap();
            if *left > 0 {
                *left -= 1;
                return Err("remote attach: relay outbound backlog is full".to_string());
            }
            self.frames.lock().unwrap().push(frame);
            Ok(())
        }
    }

    fn flaky_pane(fail_first: usize) -> (Arc<FlakySink>, RemotePaneIo) {
        let sink = Arc::new(FlakySink {
            fail_first: Mutex::new(fail_first),
            frames: Mutex::new(Vec::new()),
        });
        let dyn_sink: Arc<dyn RemoteFrameSink> = sink.clone();
        let pane = RemotePaneIo::new(
            "jti-1",
            "term-9",
            "grant.jwt",
            dyn_sink,
            100,
            40,
            AttachedRing::default(),
        );
        (sink, pane)
    }

    fn detach_frames(frames: &[Value]) -> usize {
        frames
            .iter()
            .filter(|f| f["type"] == "remote_terminal_detach")
            .count()
    }

    /// Plan 2026-09-16 Phase 1, R1: the close path runs `kill` then
    /// `release`. When `kill`'s queue attempt fails, the failure must NOT
    /// latch — `release` retries and exactly one detach reaches the sink.
    #[test]
    fn a_failed_detach_is_retried_by_the_following_release() {
        let (sink, pane) = flaky_pane(1);
        assert_eq!(pane.detach_outcome(), DetachOutcome::NotAttempted);

        let killed = pane.kill(Duration::from_millis(10));
        assert!(
            killed.is_err(),
            "a refused queue is reported, not swallowed"
        );
        assert!(
            matches!(pane.detach_outcome(), DetachOutcome::Failed(ref e) if e.contains("backlog"))
        );

        pane.release(Duration::from_millis(10))
            .expect("retry queues");
        assert_eq!(pane.detach_outcome(), DetachOutcome::Queued);
        assert_eq!(detach_frames(&sink.frames.lock().unwrap()), 1);
        // A local kill still settles `wait` with the detach code.
        assert_eq!(pane.wait(), Ok(DETACH_EXIT_CODE));
    }

    /// A successful detach latches: a later `kill`/`release` never queues a
    /// second frame.
    #[test]
    fn a_queued_detach_is_never_queued_twice() {
        let (sink, pane) = flaky_pane(0);
        pane.kill(Duration::from_millis(10)).unwrap();
        pane.release(Duration::from_millis(10)).unwrap();
        pane.kill(Duration::from_millis(10)).unwrap();
        assert_eq!(pane.detach_outcome(), DetachOutcome::Queued);
        assert_eq!(detach_frames(&sink.frames.lock().unwrap()), 1);
    }

    /// The lock is held across the queue attempt, so racing closers still
    /// queue exactly one detach.
    #[test]
    fn concurrent_kill_and_release_queue_exactly_one_detach() {
        let sink = Arc::new(RecordingSink::default());
        let pane = Arc::new(pane(&sink, AttachedRing::default()));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let p = pane.clone();
                thread::spawn(move || {
                    if i % 2 == 0 {
                        let _ = p.kill(Duration::from_millis(10));
                    } else {
                        let _ = p.release(Duration::from_millis(10));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(pane.detach_outcome(), DetachOutcome::Queued);
        assert_eq!(detach_frames(&sink.frames()), 1);
    }

    /// Both attempts failing leaves the outcome `Failed` with the last
    /// error — the state a close must report as "not released".
    #[test]
    fn a_detach_that_never_queues_stays_failed() {
        let (sink, pane) = flaky_pane(2);
        assert!(pane.kill(Duration::from_millis(10)).is_err());
        assert!(pane.release(Duration::from_millis(10)).is_err());
        assert!(matches!(pane.detach_outcome(), DetachOutcome::Failed(_)));
        assert_eq!(detach_frames(&sink.frames.lock().unwrap()), 0);
    }

    /// A target error closes the pane with the error exit code.
    #[test]
    fn remote_error_closes_reader_with_error_code() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        let reader = pane.output().unwrap();
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
        let reader = pane.output().unwrap();
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
        let reader = pane.output().unwrap();
        pane.note_relay_lost();
        assert!(!pane.is_finished());
        assert_eq!(pane.remote_offset(), 3);
        assert!(sink.frames().is_empty());
        pane.mark_exit(0);
        let mut expected = b"abc".to_vec();
        expected.extend_from_slice(RELAY_LOST_MARKER);
        assert_eq!(read_to_end_blocking(reader), expected);
    }

    /// R1: the reattach frame must carry `have_offset`. Without it the target
    /// falls back to the bounded tail, and `splice_replay` then reads a normal
    /// reconnect as a rolled ring and writes a data-loss marker for bytes the
    /// target still holds.
    #[test]
    fn reattach_frame_carries_the_offset_the_source_already_has() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        // Nothing delivered yet: the source has whatever the seed put it at.
        let seeded = pane.reattach_frame("reattach:jti-1")["have_offset"]
            .as_u64()
            .expect("reattach frame must carry have_offset");
        pane.push_output(b"0123456789");
        let after = pane.reattach_frame("reattach:jti-1")["have_offset"]
            .as_u64()
            .unwrap();
        assert_eq!(
            after,
            seeded + 10,
            "have_offset must track what this pane has actually delivered"
        );
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
