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
use std::sync::{mpsc, Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tracing::{debug, warn};

use super::pane_io::{CredentialScrub, PaneIo};

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

/// In-band notice written when the TARGET's relay socket is gone while this
/// pane is live or reattaching — the backend answered `target_not_connected`.
/// Written once per supervision; the reattach supervisor keeps retrying.
pub const TARGET_NOT_CONNECTED_MARKER: &[u8] =
    b"\r\n\x1b[1;33m[qontinui] the remote machine is not connected to the relay \xe2\x80\x94 \
retrying until it returns\x1b[0m\r\n";

/// In-band notice written when a reattach that followed a lost marker
/// succeeded. Without it the lost marker stayed the pane's last word even
/// after the tab was live again, whenever the target had produced nothing new.
pub const REATTACHED_MARKER: &[u8] = b"\r\n\x1b[1;32m[qontinui] reattached\x1b[0m\r\n";

/// In-band notice written when the pane closes on a fatal remote error, so a
/// dead tab says it is dead (and why) instead of leaving an earlier "will
/// reattach" notice as its last line.
pub fn closed_marker(code: &str, message: &str) -> Vec<u8> {
    let detail = if message.is_empty() {
        String::new()
    } else {
        format!(": {message}")
    };
    format!(
        "\r\n\x1b[1;31m[qontinui] remote tab closed ({code}){detail} — close this tab and \
         attach again\x1b[0m\r\n"
    )
    .into_bytes()
}

/// The grant a pane presents. Shared with the pane's writers and swapped in
/// place when the reattach supervisor renews an expired grant, so every frame
/// after the swap carries the new jti.
#[derive(Debug, Clone)]
pub struct GrantIdent {
    pub jti: String,
    pub grant: String,
    /// When coord says this grant expires. `None` when the mint did not say
    /// (or said something unparseable): UNKNOWN, not "never" — such a grant
    /// gets no scheduled renewal and relies on the reactive path alone.
    pub expires_at: Option<DateTime<Utc>>,
}

/// Coord's `expires_at` on a grant mint, read as RFC 3339 (coord serialises a
/// `DateTime<Utc>`). Anything else — absent, not a string, unparseable — is
/// `None`, the UNKNOWN arm.
pub fn parse_grant_expiry(raw: Option<&Value>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw?.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Shared between a pane and every writer it hands out: once the pane has
/// finished, no frame but its detach may leave (plan
/// `2026-10-03-a-live-remote-tab-dies-when-its-attach-grant-expires`, D2 — a
/// closed tab kept sending keystrokes, flow and resizes under its dead grant,
/// and the relay answered each one `attach_not_registered`).
#[derive(Debug, Default)]
struct CloseGate {
    closed: AtomicBool,
    /// Frames refused because the pane had closed. The first is logged at
    /// WARN with its type, so the caller still sending is named once.
    refused: AtomicU64,
}

impl CloseGate {
    /// `Err` (and the frame is NOT sent) once the pane has closed.
    fn admit(&self, frame_type: &str, grant_jti: &str, terminal_id: &str) -> Result<(), String> {
        if !self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.refused.fetch_add(1, Ordering::AcqRel) == 0 {
            warn!(
                grant_jti,
                terminal_id,
                frame_type,
                "remote pane: a frame was sent after the pane closed — refused, not queued \
                 (further refusals on this pane are counted, not logged)"
            );
        }
        Err(REMOTE_PANE_CLOSED.to_string())
    }
}

/// The error a closed pane's writer, resize, flow and probe answer with.
pub const REMOTE_PANE_CLOSED: &str = "remote pane is closed";

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
    /// The grant JWT and its jti, kept so a relay reconnect can re-present it
    /// — and replaced when an expired one is renewed.
    ident: Arc<RwLock<GrantIdent>>,
    terminal_id: String,
    /// The coord session this pane views — what a grant renewal mints for.
    /// `None` for a pane built without one, which therefore cannot renew.
    session_id: Option<String>,
    /// The device the pane was attached to. A renewal coord now places on a
    /// different device is refused rather than silently followed.
    target_device_id: Option<String>,
    /// A reattach supervisor owns this pane right now (at most one does).
    reattaching: AtomicBool,
    /// A "relay lost" / "target not connected" notice is the pane's latest
    /// word, so a successful reattach must say it recovered.
    awaiting_reattach: AtomicBool,
    /// The last `remote_terminal_flow` this pane queued asked for a pause.
    /// The target keys its flow gate by grant and drops it on detach, so a
    /// reattach (a relay reconnect or a renewal) comes back unpaused unless
    /// this is re-asserted — see [`Self::reassert_flow`].
    flow_paused: AtomicBool,
    sink: Arc<dyn RemoteFrameSink>,
    /// Sender half of the output channel. `None` once closed — the reader
    /// sees EOF when the last sender drops.
    output_tx: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    /// Receiver half, taken exactly once by [`PaneIo::reader`].
    output_rx: Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    /// The settled exit code; `wait` parks on the condvar until it is `Some`.
    exit: Mutex<Option<i32>>,
    exit_cv: Condvar,
    /// `remote_terminal_detach` is QUEUED at most once per pane. A failed
    /// attempt does not latch, so the `release()` that follows a failed
    /// `kill()` on the close path retries it.
    detach: Mutex<DetachOutcome>,
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
    /// Input seq + the read/write receipts. See [`RemoteInteractivity`].
    interactivity: Arc<Mutex<InteractivityState>>,
    /// Shut in [`Self::mark_exit`]; every writer holds it too.
    close_gate: Arc<CloseGate>,
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
            ident: Arc::new(RwLock::new(GrantIdent {
                jti: grant_jti.into(),
                grant: grant.into(),
                expires_at: None,
            })),
            terminal_id: terminal_id.into(),
            session_id: None,
            target_device_id: None,
            reattaching: AtomicBool::new(false),
            awaiting_reattach: AtomicBool::new(false),
            flow_paused: AtomicBool::new(false),
            sink,
            output_tx: Mutex::new(Some(tx)),
            output_rx: Mutex::new(Some(rx)),
            exit: Mutex::new(None),
            exit_cv: Condvar::new(),
            detach: Mutex::new(DetachOutcome::NotAttempted),
            remote_offset: AtomicU64::new(next_offset),
            cols: AtomicU16::new(cols),
            rows: AtomicU16::new(rows),
            seed_start: seed.start_offset,
            history_start: seed.history_start,
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
            close_gate: Arc::new(CloseGate::default()),
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
                grant_jti = %self.grant_jti(),
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
    ///
    /// When a lost / not-connected notice is the pane's latest word, the
    /// recovery is written into the pane too — otherwise a reattach that
    /// delivered no new bytes left "relay connection lost" as the last line
    /// of a tab that was live again.
    pub fn note_reattached(&self) {
        {
            let mut g = self.interactivity.lock().unwrap_or_else(|e| e.into_inner());
            g.snapshot.acks_since_attach = 0;
        }
        if self.awaiting_reattach.swap(false, Ordering::AcqRel) {
            self.push_local(REATTACHED_MARKER);
        }
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
        self.close_gate.admit(
            "remote_terminal_input",
            &self.grant_jti(),
            &self.terminal_id,
        )?;
        let mut g = self.interactivity.lock().unwrap_or_else(|e| e.into_inner());
        if g.snapshot.acks_since_attach == 0 {
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
            "grant_jti": self.grant_jti(),
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
        let mut g = self.interactivity.lock().unwrap_or_else(|e| e.into_inner());
        g.snapshot.last_frame_received = Some(FrameReceived {
            at_ms: now_epoch_ms(),
            through_offset,
        });
    }

    /// The `[from, to)` target range OLDER than the attach seed that the
    /// target still holds — `None` when the seed already began at the ring's
    /// first byte (or the target reported no ring start). Phase 5 lazy
    /// scrollback: fetched only when the operator asks for earlier output.
    pub fn history_range(&self) -> Option<(u64, u64)> {
        let start = self.history_start?;
        (start < self.seed_start).then_some((start, self.seed_start))
    }

    /// Record the coord session this pane views, so an expired grant can be
    /// renewed for it.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Record the device this pane is attached to, so a grant renewal can
    /// refuse a session coord has since placed elsewhere.
    pub fn with_target_device_id(mut self, device_id: impl Into<String>) -> Self {
        self.target_device_id = Some(device_id.into());
        self
    }

    pub fn target_device_id(&self) -> Option<&str> {
        self.target_device_id.as_deref()
    }

    /// Record when the pane's FIRST grant expires (see
    /// [`GrantIdent::expires_at`]); a renewal replaces it via [`Self::set_grant`].
    pub fn with_grant_expires_at(self, expires_at: Option<DateTime<Utc>>) -> Self {
        self.ident
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .expires_at = expires_at;
        self
    }

    /// When the CURRENT grant expires, if coord said.
    pub fn grant_expires_at(&self) -> Option<DateTime<Utc>> {
        self.ident
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .expires_at
    }

    /// Frames refused because the pane had already closed.
    pub fn frames_refused_after_close(&self) -> u64 {
        self.close_gate.refused.load(Ordering::Acquire)
    }

    pub fn grant_jti(&self) -> String {
        self.ident
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .jti
            .clone()
    }

    pub fn grant(&self) -> String {
        self.ident
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .grant
            .clone()
    }

    /// Replace the grant this pane presents (a renewal). Every frame built
    /// after this — input, resize, flow, detach, reattach — carries the new
    /// jti. Returns the jti it replaced.
    pub fn set_grant(
        &self,
        jti: impl Into<String>,
        grant: impl Into<String>,
        expires_at: Option<DateTime<Utc>>,
    ) -> String {
        let mut g = self.ident.write().unwrap_or_else(|e| e.into_inner());
        let old = std::mem::replace(&mut g.jti, jti.into());
        g.grant = grant.into();
        g.expires_at = expires_at;
        old
    }

    /// Claim the pane for a reattach supervisor. `false` when one already
    /// owns it — the caller must not start a second.
    pub fn try_begin_reattach(&self) -> bool {
        self.reattaching
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Release the claim taken by [`Self::try_begin_reattach`].
    pub fn end_reattach(&self) {
        self.reattaching.store(false, Ordering::Release);
    }

    pub fn is_reattaching(&self) -> bool {
        self.reattaching.load(Ordering::Acquire)
    }

    /// The target's relay socket is gone: say so, and remember that a
    /// recovery must be announced.
    pub fn note_target_not_connected(&self) {
        self.awaiting_reattach.store(true, Ordering::Release);
        self.push_local(TARGET_NOT_CONNECTED_MARKER);
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
        let delivered = match tx.as_ref() {
            Some(tx) if tx.send(bytes.to_vec()).is_ok() => {
                self.remote_offset
                    .fetch_add(bytes.len() as u64, Ordering::AcqRel);
                true
            }
            _ => false,
        };
        drop(tx);
        if delivered {
            self.note_frame_received();
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
        self.awaiting_reattach.store(true, Ordering::Release);
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
                grant_jti = %self.grant_jti(),
                have,
                ring_end = end,
                "remote pane: reattach ring adds nothing new"
            );
            // Still a frame the target answered with — the read half holds —
            // but only while the pane is open, as in `push_output`: a frame
            // after exit went nowhere and is not a receipt.
            if self.output_open() {
                self.note_frame_received();
            }
            return;
        }
        let skip = if have > start {
            (have - start) as usize
        } else {
            if have < start {
                let lost = start - have;
                warn!(
                    grant_jti = %self.grant_jti(),
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
        // `push_output` stamps the receipt when it delivers.
        self.push_output(&ring.buffer[skip..]);
    }

    /// Settle the exit code (first writer wins) and close the output channel
    /// so a reader blocked in `read()` sees EOF. From here on the pane sends
    /// nothing but its detach.
    pub fn mark_exit(&self, code: i32) {
        self.close_gate.closed.store(true, Ordering::Release);
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
            grant_jti = %self.grant_jti(),
            terminal_id = %self.terminal_id,
            code,
            message,
            "remote pane: target reported an error — closing the pane"
        );
        // Written BEFORE the channel closes, so the reader delivers it ahead
        // of EOF and a dead tab says why it is dead.
        self.push_local(&closed_marker(code, message));
        self.mark_exit(ERROR_EXIT_CODE);
    }

    fn output_open(&self) -> bool {
        self.output_tx
            .lock()
            .map(|tx| tx.is_some())
            .unwrap_or(false)
    }

    fn close_output(&self) {
        if let Ok(mut tx) = self.output_tx.lock() {
            drop(tx.take());
        }
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
    ///
    /// The close gate is shut HERE, under the detach lock: a reattach checks
    /// the gate under the same lock ([`Self::send_reattach`]), so none can be
    /// queued behind this detach and re-bind the terminal to a closed tab.
    fn send_detach_once(&self) -> Result<(), String> {
        let mut slot = match self.detach.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.close_gate.closed.store(true, Ordering::Release);
        if *slot == DetachOutcome::Queued {
            return Ok(());
        }
        let sent = self.send(json!({
            "type": "remote_terminal_detach",
            "grant_jti": self.grant_jti(),
            "terminal_id": self.terminal_id,
        }));
        *slot = match &sent {
            Ok(()) => DetachOutcome::Queued,
            Err(e) => {
                warn!(
                    grant_jti = %self.grant_jti(),
                    terminal_id = %self.terminal_id,
                    error = %e,
                    "remote pane: remote_terminal_detach could not be queued — the relay keeps the binding until the source socket drops or the grant expires"
                );
                DetachOutcome::Failed(e.clone())
            }
        };
        sent
    }

    /// Queue this pane's [`Self::reattach_frame`] — refused with
    /// [`REMOTE_PANE_CLOSED`] once the pane has closed or detached. Checked
    /// under the detach lock, so a reattach can never follow the detach.
    pub fn send_reattach(&self, request_id: &str) -> Result<(), String> {
        let _detach = match self.detach.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if self.close_gate.closed.load(Ordering::Acquire) {
            return Err(REMOTE_PANE_CLOSED.to_string());
        }
        self.send(self.reattach_frame(request_id))
    }

    /// `Err` once the pane has closed, for a frame built OUTSIDE the pane (a
    /// history request). Counted and logged like the pane's own refusals.
    pub fn admit_frame(&self, frame_type: &str) -> Result<(), String> {
        self.close_gate
            .admit(frame_type, &self.grant_jti(), &self.terminal_id)
    }

    /// After a reattach: re-send a pause that was in force, under the CURRENT
    /// jti. The target dropped the old attachment's flow gate with it, so
    /// without this a paused tab streams again until its next flow edge.
    /// Nothing is sent for a pane that was not paused.
    pub fn reassert_flow(&self) -> Result<(), String> {
        if !self.flow_paused.load(Ordering::Acquire) {
            return Ok(());
        }
        self.send_flow(true)
    }

    fn send_flow(&self, paused: bool) -> Result<(), String> {
        self.close_gate
            .admit("remote_terminal_flow", &self.grant_jti(), &self.terminal_id)?;
        self.send(json!({
            "type": "remote_terminal_flow",
            "grant_jti": self.grant_jti(),
            "terminal_id": self.terminal_id,
            "paused": paused,
        }))?;
        self.flow_paused.store(paused, Ordering::Release);
        Ok(())
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
            "grant": self.grant(),
            "cols": cols,
            "rows": rows,
            "have_offset": self.remote_offset(),
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

/// `Write` that ships each write as one `remote_terminal_input` frame,
/// stamped with the pane's next `seq`.
struct FrameWriter {
    ident: Arc<RwLock<GrantIdent>>,
    terminal_id: String,
    sink: Arc<dyn RemoteFrameSink>,
    interactivity: Arc<Mutex<InteractivityState>>,
    close_gate: Arc<CloseGate>,
}

impl Write for FrameWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let jti = self
            .ident
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .jti
            .clone();
        // What a local PTY's writer answers after exit, which the session
        // layer already handles.
        self.close_gate
            .admit("remote_terminal_input", &jti, &self.terminal_id)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))?;
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
                "grant_jti": jti,
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
            ident: self.ident.clone(),
            terminal_id: self.terminal_id.clone(),
            sink: self.sink.clone(),
            interactivity: self.interactivity.clone(),
            close_gate: self.close_gate.clone(),
        }))
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        self.close_gate.admit(
            "remote_terminal_resize",
            &self.grant_jti(),
            &self.terminal_id,
        )?;
        self.send(json!({
            "type": "remote_terminal_resize",
            "grant_jti": self.grant_jti(),
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
        self.send_flow(paused)
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
        let bytes = read_to_end_blocking(pane.reader().unwrap());
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
        let reader = pane.reader().unwrap();
        pane.mark_error("session_not_local", "no such session here");
        // The close is announced in the pane before EOF — a dead tab says
        // why it is dead rather than leaving an older notice as its last line.
        assert_eq!(
            read_to_end_blocking(reader),
            closed_marker("session_not_local", "no such session here")
        );
        assert_eq!(pane.wait(), Ok(ERROR_EXIT_CODE));
    }

    /// Plan 2026-10-02 D3: a reattach after a lost notice says it recovered;
    /// a reattach with no notice outstanding writes nothing (an ordinary
    /// flow resync must not spam the pane).
    #[test]
    fn a_reattach_after_a_lost_notice_announces_the_recovery() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        let reader = pane.reader().unwrap();
        pane.note_reattached();
        pane.note_relay_lost();
        pane.note_reattached();
        pane.note_reattached();
        pane.note_target_not_connected();
        pane.note_reattached();
        pane.mark_exit(0);
        let mut expected = RELAY_LOST_MARKER.to_vec();
        expected.extend_from_slice(REATTACHED_MARKER);
        expected.extend_from_slice(TARGET_NOT_CONNECTED_MARKER);
        expected.extend_from_slice(REATTACHED_MARKER);
        assert_eq!(read_to_end_blocking(reader), expected);
    }

    /// Plan 2026-10-02 D2: a renewed grant replaces the jti on EVERY frame
    /// the pane builds afterwards — including a writer taken before the swap.
    #[test]
    fn a_renewed_grant_is_carried_by_every_later_frame() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        let mut w = pane.writer().unwrap();
        assert_eq!(pane.set_grant("jti-2", "grant-2.jwt", None), "jti-1");
        assert_eq!(pane.grant_jti(), "jti-2");
        w.write_all(b"x").unwrap();
        pane.resize(90, 30).unwrap();
        let f = pane.reattach_frame("reattach:jti-2");
        assert_eq!(f["grant"], "grant-2.jwt");
        for frame in sink.frames() {
            assert_eq!(frame["grant_jti"], "jti-2", "{frame}");
        }
    }

    /// Plan 2026-10-03 D2: once the pane has finished, its writer (taken
    /// before the close), resize, flow and probe queue nothing and answer
    /// "closed" — counted per pane — while its detach still goes out.
    #[test]
    fn a_finished_pane_sends_nothing_but_its_detach() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        let mut w = pane.writer().unwrap();
        pane.record_input_ack(&json!({"seq": 1, "accepted": true, "bytes": 1}));
        pane.mark_exit(0);

        let err = w
            .write(b"ls\r")
            .expect_err("a closed pane's writer refuses");
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
        assert_eq!(
            pane.resize(90, 30).unwrap_err(),
            REMOTE_PANE_CLOSED.to_string()
        );
        assert!(pane.set_paused(true).is_err());
        assert!(pane.send_input_probe().is_err());
        assert!(pane.writer().unwrap().write(b"x").is_err());
        assert!(sink.frames().is_empty(), "{:?}", sink.frames());
        assert_eq!(pane.frames_refused_after_close(), 5);

        pane.release(Duration::from_millis(10)).unwrap();
        let frames = sink.frames();
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert_eq!(frames[0]["type"], "remote_terminal_detach");
        assert_eq!(pane.detach_outcome(), DetachOutcome::Queued);
    }

    /// Coord's `expires_at` is an RFC 3339 string; anything else is UNKNOWN,
    /// never a guessed time.
    #[test]
    fn grant_expiry_parses_rfc3339_and_nothing_else() {
        let at = parse_grant_expiry(Some(&json!("2026-10-03T11:30:16Z"))).unwrap();
        assert_eq!(at.to_rfc3339(), "2026-10-03T11:30:16+00:00");
        let offset = parse_grant_expiry(Some(&json!("2026-10-03T13:30:16.5+02:00"))).unwrap();
        assert_eq!(offset.timestamp(), at.timestamp());
        assert_eq!(parse_grant_expiry(None), None);
        assert_eq!(parse_grant_expiry(Some(&json!(1_790_000_000))), None);
        assert_eq!(parse_grant_expiry(Some(&json!("tomorrow"))), None);
        assert_eq!(parse_grant_expiry(Some(&Value::Null)), None);

        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default()).with_grant_expires_at(Some(at));
        assert_eq!(pane.grant_expires_at(), Some(at));
        pane.set_grant("jti-2", "g2", None);
        assert_eq!(
            pane.grant_expires_at(),
            None,
            "a renewal replaces the expiry"
        );
    }

    /// Only one supervisor may claim a pane at a time.
    #[test]
    fn reattach_claim_is_exclusive_until_released() {
        let sink = Arc::new(RecordingSink::default());
        let pane = pane(&sink, AttachedRing::default());
        assert!(pane.try_begin_reattach());
        assert!(!pane.try_begin_reattach());
        assert!(pane.is_reattaching());
        pane.end_reattach();
        assert!(pane.try_begin_reattach());
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
