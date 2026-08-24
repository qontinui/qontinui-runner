//! TerminalSession — PTY lifecycle management for a single terminal instance.
//!
//! Spawns a shell via `portable-pty`, manages reader/writer threads,
//! and emits Tauri events for output and exit.

use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, oneshot};

use base64::{engine::general_purpose::STANDARD, Engine};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter};
use tracing::{debug, info, warn};

use super::grid::{Grid, GridPerformer};
use super::interceptor::OutputInterceptor;
use super::types::{TerminalExitEvent, TerminalId, TerminalInfo};
use super::visibility::{
    ActivityDigestState, BackgroundHold, TerminalActivityWire, VisibilityState, VisibilityTier,
    ACTIVITY_DIGEST_LINES, ACTIVITY_EVENT,
};

/// Shell integration scripts embedded at compile time.
#[cfg(target_os = "windows")]
const PS1_INTEGRATION: &str = include_str!("../../resources/shell-integration.ps1");
#[cfg(not(target_os = "windows"))]
const BASH_INTEGRATION: &str = include_str!("../../resources/shell-integration.bash");
const ZSH_INTEGRATION: &str = include_str!("../../resources/shell-integration.zsh");

/// Write a shell integration script to a temp file, returning the path on success.
fn write_integration_script(content: &str, name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, content).ok()?;
    Some(path)
}

/// Write the zsh integration as `<dir>/.zshrc` and return `<dir>` for use as
/// `ZDOTDIR`. zsh has no `--rcfile`; pointing `ZDOTDIR` at a dir makes zsh
/// source `$ZDOTDIR/.zshrc` instead of `~/.zshrc` (our script re-sources the
/// user's real `~/.z*` files). Returns `None` so the caller can fall back to a
/// plain shell if the temp dir can't be created.
fn write_zsh_zdotdir(content: &str) -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("qontinui-zsh-integration");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(dir.join(".zshrc"), content).ok()?;
    Some(dir)
}

/// Default maximum scrollback buffer capacity (1 MB).
///
/// Since Phase 8 of `plans/2026-07-28-runner-many-sessions-performance.md`
/// this is the *default* of `settings.performance.scrollback_capacity_bytes`
/// rather than a hard constant — [`resolved_scrollback_capacity`] reads the
/// setting (floored at [`crate::settings::MIN_SCROLLBACK_CAPACITY`]) once per
/// spawn and the reader thread carries the resolved value.
///
/// It stays here, and `settings::default_scrollback_capacity_bytes` returns
/// **this** constant, so the ring's historical size has exactly one
/// definition: a settings.json with no `performance` key resolves back to the
/// same 1 MiB the reader always used.
pub(crate) const SCROLLBACK_CAPACITY: usize = 1_048_576;

/// The scrollback ring capacity this process should give a session spawned
/// now: the operator's setting with the floor applied.
///
/// Read once per spawn from the process-cached performance snapshot (no
/// settings-file I/O on the spawn path — that is root cause B5 of the same
/// plan), so a saved change applies to the next terminal and existing rings
/// keep the size they were allocated with. That is deliberate: resizing a
/// live ring would either drop replayable history or silently reallocate a
/// megabyte per open session.
fn resolved_scrollback_capacity() -> usize {
    crate::settings::get_performance_settings().effective_scrollback_capacity()
}

/// The visibility-tier flush cadences this process should give a session
/// spawned now: `(background, unwatched)`, where the second is `None` when the
/// `unwatched` tier is to emit nothing at all (the stock default).
///
/// Resolved once per spawn from the same process-cached snapshot
/// [`resolved_scrollback_capacity`] reads, and for the same reason: no
/// settings-file I/O on the spawn path, and a saved change applies to the next
/// terminal rather than mutating the cadence a live reader thread and the
/// sweeper have already agreed on. Both halves of a session's emission — the
/// reader thread's on-chunk flush and the sweeper's quiet-tail flush — read the
/// session's own copy, so they can never disagree mid-life.
fn resolved_flush_intervals() -> (Duration, Option<Duration>) {
    let perf = crate::settings::get_performance_settings();
    (
        perf.background_flush_interval(),
        perf.unwatched_flush_interval(),
    )
}

/// Append a chunk of processed PTY output to a session's scrollback ring
/// buffer and bump its monotonic byte counter — the exact teeing step the
/// reader thread performs per read (see [`TerminalSession::spawn`]).
///
/// Extracted as a free function so it can be exercised in tests against a
/// real PTY without constructing a Tauri `AppHandle` (which the reader
/// thread otherwise needs only for event emission, orthogonal to buffer
/// state). `scrollback` and `total_produced` are this session's OWN Arcs —
/// the per-session isolation that [`TerminalSession::get_scrollback_buffer`]
/// reads back depends on each session holding distinct instances, which is
/// what the distinct-buffers regression test locks in.
///
/// Returns the chunk's absolute START offset in the session's output stream
/// (the counter value before this chunk) — the reader thread stamps it onto
/// the `terminal-output` event so the frontend can dedup a scrollback-ring
/// replay against concurrently-delivered live chunks by byte offset.
///
/// The counter bump happens INSIDE the ring lock so that a reader of
/// `(ring contents, total)` under the same lock sees a mutually consistent
/// pair — [`TerminalSession::get_scrollback_buffer`] relies on this to
/// compute an exact `end_offset` (off-by-one-chunk here would make the
/// frontend double-write or drop a chunk at the replay boundary).
///
/// `capacity` is the session's ring size, resolved once at spawn from
/// [`resolved_scrollback_capacity`] and carried by the reader thread — passed
/// in rather than re-read here so the per-chunk hot path never touches
/// settings state.
fn tee_into_scrollback(
    scrollback: &Arc<Mutex<VecDeque<u8>>>,
    total_produced: &Arc<AtomicU64>,
    data: &[u8],
    capacity: usize,
) -> u64 {
    if let Ok(mut sb) = scrollback.lock() {
        // Slice append, not byte-by-byte: `extend` copies the run in one go and
        // a single up-front `drain` makes room, instead of a bounds check +
        // `pop_front` per byte on every chunk of every session.
        if data.len() >= capacity {
            // The chunk alone overflows the ring — keep only its tail.
            sb.clear();
            sb.extend(&data[data.len() - capacity..]);
        } else {
            let overflow = (sb.len() + data.len()).saturating_sub(capacity);
            if overflow > 0 {
                sb.drain(..overflow);
            }
            sb.extend(data);
        }
        total_produced.fetch_add(data.len() as u64, Ordering::Relaxed)
    } else {
        // Poisoned ring lock: the chunk was still produced — keep the
        // monotonic counter truthful even though buffering failed.
        total_produced.fetch_add(data.len() as u64, Ordering::Relaxed)
    }
}

/// Feed `data` through the VT parser into the session's cell grid, then bump
/// the session's grid-generation counter.
///
/// THE ORDER IS LOAD-BEARING. The counter moves only AFTER the grid lock has
/// been released, so any observer that reads generation `g` and *then* takes
/// the grid lock is guaranteed to render a screen that has already absorbed
/// every byte counted up to `g`. Bumping before (or inside) the advance — which
/// is what gating on `total_bytes_produced` effectively did, since that counter
/// moves in `tee_into_scrollback` well before this call — lets a scanner record
/// a watermark for bytes the grid has not yet drawn, and then skip forever once
/// the terminal goes idle and the counter stops moving. See the `terminal::scan_gate` module docs.
///
/// The counter is bumped even when the grid lock is poisoned: an extra scan is
/// always safe, a missed one is not.
///
/// Shared with the scan-gate regression test so both drive the identical path.
fn advance_grid(
    grid: &Arc<Mutex<Grid>>,
    grid_generation: &Arc<AtomicU64>,
    parser: &mut vte::Parser,
    data: &[u8],
) {
    if let Ok(mut g) = grid.lock() {
        let mut perf = GridPerformer::new(&mut g);
        parser.advance(&mut perf, data);
    }
    // Release: pairs with the Acquire load in `TerminalSession::grid_generation`
    // so observing this value implies the grid mutation above is visible.
    grid_generation.fetch_add(1, Ordering::Release);
}

/// Flow-control watermarks (bytes), mirroring VS Code's `FlowControlConstants`
/// (High=100000, Low=5000 chars). EMISSION to the webview pauses once the
/// unacked gap (`bytes_sent − bytes_acked`) exceeds [`FLOW_HIGH_WATERMARK`]
/// and resumes only once it drops back below [`FLOW_LOW_WATERMARK`] —
/// hysteresis so we don't thrash pause/resume one byte at a time. The
/// frontend acks render-completed bytes in ~5000-byte units (see
/// `flowControl.ts`), so the Low watermark matches one ack quantum.
///
/// CRITICAL INVARIANT: backpressure gates the `terminal-output` webview
/// emission ONLY — it must NEVER pause the PTY read itself. The original
/// implementation paused the reader loop, which stopped feeding the VT grid
/// and, once the kernel PTY buffer filled, hard-blocked the child process on
/// stdout. Any session without a mounted `TerminalInstance` (every terminal
/// on a non-selected terminal page — nothing acks for those) froze ~100KB
/// into its next output burst, and the frozen grid also blinded the
/// `auto_response` grid-scan rules that would have advanced it. The reader
/// now always reads and always feeds interceptor + scrollback + grid; when
/// the webview falls behind (or is absent) only the event stream is dropped,
/// and the frontend recovers via the offset-gap resync in
/// `TerminalInstance.tsx` (scrollback-ring replay, which also resets the
/// counters via [`TerminalSession::reset_flow_control`]).
const FLOW_HIGH_WATERMARK: u64 = 100_000;
const FLOW_LOW_WATERMARK: u64 = 5_000;

/// Hysteresis state machine deciding whether a chunk is emitted to the
/// webview given the current unacked gap. Pure and single-threaded (lives on
/// the reader thread) so the policy is unit-testable in isolation.
///
/// - Not paused: emits until `gap > FLOW_HIGH_WATERMARK`, then pauses.
/// - Paused: stays paused until `gap <= FLOW_LOW_WATERMARK` (acks caught up
///   or [`TerminalSession::reset_flow_control`] zeroed the gap), then emits
///   again.
struct EmissionGate {
    paused: bool,
}

impl EmissionGate {
    fn new() -> Self {
        Self { paused: false }
    }

    /// Returns true when the chunk should be emitted to the webview. Skipped
    /// chunks are NOT counted into `bytes_sent`, so the gap freezes at its
    /// pause-time value until acks (or a flow-control reset) shrink it.
    fn should_emit(&mut self, gap: u64) -> bool {
        if self.paused {
            if gap <= FLOW_LOW_WATERMARK {
                self.paused = false;
            }
        } else if gap > FLOW_HIGH_WATERMARK {
            self.paused = true;
        }
        !self.paused
    }
}

/// What the webview leg does with one chunk, given the session's visibility
/// tier (Phase 5 / A4). Pure so the tiering policy is unit-testable without a
/// PTY, a Tauri app or a webview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebviewAdmission {
    /// Emit this payload as its own `terminal-output` event now.
    Now,
    /// Accumulate it into the session's hold window; it leaves with the next
    /// flush (the tier's configured interval, or the byte cap).
    Hold,
    /// Do not deliver it to the webview at all. The chunk still reached the
    /// scrollback ring, SSE, WS and coord upstream of this decision.
    Skip,
}

/// Decide the webview leg for one chunk.
///
/// **This function governs the webview and nothing else.** Its callers have
/// already served the SSE broadcast, the WS relay and the scrollback ring, so
/// no tier can cost an external consumer a byte.
///
/// - `Focused` — today's behavior exactly: the flow-control [`EmissionGate`]
///   decides, because a mounted visible pane is the one consumer that actually
///   render-acks.
/// - `Background` — a pane is mounted but hidden, so nothing renders and
///   nothing render-acks; the gate has no ack source and would wedge at the
///   high watermark (which is precisely why the frontend used to synthesize
///   proxy-acks for it). The tier itself is the flow control instead: at most
///   one event per configured flush interval (default
///   [`BACKGROUND_FLUSH_INTERVAL`]), bounded by [`BACKGROUND_HOLD_BYTE_CAP`].
///   Deterministic, and one fewer IPC round trip per frame than the acks it
///   replaces.
/// - `Unwatched` — no pane anywhere. By default nothing is emitted; `Skip`
///   reports back whether the caller should raise `emission_skipped`, which is
///   what makes a later upgrade emit a resume marker and pull a ring resync.
///   When the operator has set a positive
///   `PerformanceSettings::unwatched_flush_interval_ms` (`unwatched_holds`),
///   the tier coalesces at that cadence instead of going dark — so nothing is
///   ever missed, and no resume marker is owed.
///
/// `gated` is false only for the reader's exit flush: a dying terminal's final
/// frame has no successor chunk to reveal a gap, so it ships regardless of tier
/// (bounded by `SYNC_FLUSH_BYTE_CAP`).
fn admit_to_webview(
    tier: VisibilityTier,
    gated: bool,
    gate: &mut EmissionGate,
    gap: u64,
    unwatched_holds: bool,
) -> WebviewAdmission {
    if !gated {
        return WebviewAdmission::Now;
    }
    match tier {
        VisibilityTier::Unwatched if unwatched_holds => WebviewAdmission::Hold,
        VisibilityTier::Unwatched => WebviewAdmission::Skip,
        VisibilityTier::Background => WebviewAdmission::Hold,
        VisibilityTier::Focused => {
            if gate.should_emit(gap) {
                WebviewAdmission::Now
            } else {
                WebviewAdmission::Skip
            }
        }
    }
}

/// The flush spacing that governs a held window for `tier`.
///
/// A session's hold is one buffer shared by both tiers that can fill it, and
/// the tier can change while bytes sit in it — so the interval is resolved per
/// call from the CURRENT tier rather than fixed on the hold. `Focused` cannot
/// hold (its bytes go out under the gate, draining the hold first), so the
/// background cadence is the harmless fallback rather than a claim about it.
fn hold_interval(
    tier: VisibilityTier,
    background: Duration,
    unwatched: Option<Duration>,
) -> Duration {
    match tier {
        VisibilityTier::Unwatched => unwatched.unwrap_or(background),
        VisibilityTier::Background | VisibilityTier::Focused => background,
    }
}

/// Whether the `terminal-activity` digest is this session's job right now.
///
/// Two conditions that look similar and are not: `unwatched_interval` is the
/// session's CONFIG, frozen at spawn, while `tier` is its LIVE service level.
/// A session can be `unwatched` and still owe no digest (the operator gave the
/// tier a cadence, so the page tap is fed by real `terminal-output` and a
/// digest would double-count the sparkline), and a session configured for a
/// dark `unwatched` tier owes one only while it is actually in that tier.
///
/// Extracted rather than left as two early returns in
/// [`TerminalSession::emit_activity_digest_if_due`] so the decision is
/// unit-testable: the emitter itself needs a Tauri handle and bails before
/// either condition is observable, which is exactly how the D3 defect
/// ("the field exists, nothing reads it") stayed invisible.
fn digest_is_owed(tier: VisibilityTier, unwatched_interval: Option<Duration>) -> bool {
    unwatched_interval.is_none() && tier == VisibilityTier::Unwatched
}

/// Byte cap for a held DEC-2026 (synchronized-output) frame: flush the
/// accumulated frame once it reaches this size even if `?2026l` hasn't
/// arrived, so a runaway/never-closed `?2026h` block can't buffer unbounded.
const SYNC_FLUSH_BYTE_CAP: usize = 256 * 1024;
/// Time cap for a held sync frame: flush once the block has been open this
/// long, so a slow producer mid-frame can't stall output past ~one frame.
const SYNC_FLUSH_TIME_CAP: std::time::Duration = std::time::Duration::from_millis(50);

/// Synchronized-output (DEC 2026) frame coalescer for the reader thread.
///
/// Full-screen TUIs (Claude Code) bracket each whole-frame redraw in
/// `?2026h … ?2026l`. Emitting per-PTY-read would split a frame across
/// several `terminal-output` events, so xterm paints partial frames —
/// mid-frame overdraw (symptom a). This accumulates the processed bytes of a
/// frame that spans multiple reads and emits the whole frame as ONE event so
/// xterm receives it in a single write (its RenderDebouncer then paints one
/// reflow). The common case — a whole frame in one read — never holds: the
/// parser's `sync_output` is already false post-advance, so [`Self::feed`]
/// emits immediately.
///
/// `feed` is called once per read with the post-interceptor `data`, that
/// data's stamped stream offset, and the live `sync_output` parser state
/// observed AFTER the data was advanced into the grid. The `emit` callback
/// receives `(payload, offset)` to broadcast as one event; `offset` is the
/// absolute stream offset of the payload's first byte (a correct replay
/// boundary — the first held byte's offset for a coalesced frame).
struct SyncFrameCoalescer {
    pending: Vec<u8>,
    pending_offset: u64,
    started_at: Option<std::time::Instant>,
}

impl SyncFrameCoalescer {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            pending_offset: 0,
            started_at: None,
        }
    }

    /// Feed one read's processed `data` (stamped at `offset`) plus the live
    /// `in_sync` parser state observed right after advancing it into the grid.
    /// Emits zero or one coalesced event via `emit`.
    fn feed<F: FnMut(&[u8], u64)>(&mut self, data: &[u8], offset: u64, in_sync: bool, mut emit: F) {
        if in_sync {
            // Mid-frame: hold this chunk. Stamp the held frame's offset on the
            // FIRST held byte (a correct replay boundary).
            if self.pending.is_empty() {
                self.pending_offset = offset;
                self.started_at = Some(std::time::Instant::now());
            }
            self.pending.extend_from_slice(data);
            // Byte-cap a runaway/never-closed block.
            if self.pending.len() >= SYNC_FLUSH_BYTE_CAP {
                emit(&self.pending, self.pending_offset);
                self.pending.clear();
                self.started_at = None;
            }
        } else if self.pending.is_empty() {
            // Common case: no open frame, nothing held — emit immediately.
            emit(data, offset);
        } else {
            // The block just closed in this read (it carried `?2026l`): emit
            // the held prefix + this chunk as the single frame, stamped at the
            // held frame's start offset.
            self.pending.extend_from_slice(data);
            emit(&self.pending, self.pending_offset);
            self.pending.clear();
            self.started_at = None;
        }
    }

    /// Flush a held frame if it has been open at least [`SYNC_FLUSH_TIME_CAP`].
    /// Called before each (blocking) read so a slow producer mid-frame still
    /// flushes within ~one frame rather than waiting for the next byte.
    fn flush_if_timed_out<F: FnMut(&[u8], u64)>(&mut self, mut emit: F) {
        if let Some(started) = self.started_at {
            if started.elapsed() >= SYNC_FLUSH_TIME_CAP && !self.pending.is_empty() {
                emit(&self.pending, self.pending_offset);
                self.pending.clear();
                self.started_at = None;
            }
        }
    }

    /// Flush any remaining held frame (reader exit on EOF / read error) so the
    /// last frame of a never-closed block isn't lost.
    fn flush_remaining<F: FnMut(&[u8], u64)>(&mut self, mut emit: F) {
        if !self.pending.is_empty() {
            emit(&self.pending, self.pending_offset);
            self.pending.clear();
            self.started_at = None;
        }
    }
}

/// Tauri `terminal-output` wire shape: the shared `TerminalOutputEvent`
/// (qontinui-types) is `deny_unknown_fields`, so the extra `offset` rides on
/// this runner-local struct instead of forcing a schemas version bump. Only
/// the runner frontend consumes the Tauri event; the backend-relay broadcast
/// keeps its own offset-free payload.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutputWire<'a> {
    terminal_id: &'a str,
    data: &'a str,
    /// Absolute byte offset of this chunk's first byte in the session's
    /// output stream (the value of `total_bytes_produced` before this chunk
    /// was teed). Lets the frontend dedup a scrollback-ring replay against
    /// live chunks — see `terminal_get_scrollback` and the frontend's
    /// `scrollbackReplay.ts`.
    offset: u64,
}

/// Emit one `terminal-output` event to the webview.
///
/// Shared by the reader thread, the background-window flush (which the
/// visibility sweeper can also drive) and the flow-control resume marker, so
/// every webview emission for a session goes through one place with one wire
/// shape. `encoded` is already base64; `offset` is the absolute stream offset
/// of its first byte.
fn emit_terminal_output(app: &AppHandle, terminal_id: &str, encoded: &str, offset: u64) {
    let event = TerminalOutputWire {
        terminal_id,
        data: encoded,
        offset,
    };
    if let Err(e) = app.emit("terminal-output", &event) {
        warn!(
            terminal_id = %terminal_id,
            error = %e,
            "Failed to emit terminal output event"
        );
    }
}

/// ANSI bracketed-paste begin marker.
const BRACKETED_PASTE_BEGIN: &[u8] = b"\x1b[200~";
/// ANSI bracketed-paste end marker.
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
/// Bare CR — the submit keystroke after the paste window closes.
/// Intentionally NOT `\r\n` (see [`TerminalSession::submit_prompt`]).
const SUBMIT_ENTER: &[u8] = b"\r";

/// Delay between the bracketed-paste block and the trailing submit keystroke.
///
/// **Why this exists:** the original `submit_prompt` wrote
/// `\x1b[200~<msg>\x1b[201~\r` as four `write_all`s under a single locked
/// writer with one final `flush`. From Claude Code's readline perspective
/// these landed in one read cycle — its bracketed-paste handler consumed
/// the begin/body/end and then ate the trailing `\r` as paste-tail bleed,
/// never submitting. Empirically reproduced on §6 E2E 2026-05-10
/// (`bracketed-paste-submit-doesnt-fully-submit.md`): brief visible in
/// the input area, no `❯` cursor at the end, no working indicators, task
/// stuck at `assigned`. A SEPARATE write of `\r` via `/terminals/{id}/write`
/// did submit, confirming the issue is timing / single-read-cycle
/// consumption, not the byte itself.
///
/// 150ms is enough for one Claude readline cycle on typical hardware.
/// Tunable: bump if E2E shows submits still racing.
const POST_PASTE_DELAY: std::time::Duration = std::time::Duration::from_millis(150);

/// Does `next` legitimately introduce an ANSI escape sequence?
///
/// Used by [`sanitize_submit_body`] to tell a real escape sequence (which
/// must survive — agents paste coloured build logs and diffs) from a bare
/// `ESC` control byte (which must not reach the PTY). Deliberately a
/// whitelist of the standard introducers rather than "any printable", so
/// `ESC b` is treated as a stray control byte and dropped.
///
/// `\` — **ST**, the string terminator — is on the list because the
/// string-introducing sequences below are only well formed once terminated.
/// Dropping ST's `ESC` turns a complete `OSC … ST` into an unterminated one,
/// which corrupts exactly the content the negative control promises to
/// preserve *and* leaves a sequence that consumes whatever follows it.
fn is_escape_introducer(next: char) -> bool {
    matches!(
        next,
        // CSI, OSC, DCS, SOS, PM, APC
        '[' | ']' | 'P' | 'X' | '^' | '_'
        // ST — terminates the string sequences above
        | '\\'
        // charset designators
        | '(' | ')' | '*' | '+'
        // line-size / charset selection
        | '#' | '%'
        // save/restore cursor, keypad mode
        | '7' | '8' | '=' | '>'
        // IND, NEL, HTS, RI, SS2, SS3, DECID, RIS
        | 'D' | 'E' | 'H' | 'M' | 'N' | 'O' | 'Z' | 'c'
    )
}

/// Does `ESC <intro>` open a *string* sequence — one that runs until an
/// explicit terminator (`ST`, or `BEL` for `OSC`) rather than ending at a
/// bounded final byte the way `CSI` and the two-character escapes do?
///
/// [`sanitize_submit_body`] needs the distinction twice over: the terminator
/// is otherwise a control byte it would strip, and an unterminated string
/// sequence consumes every byte after it — including the paste END marker
/// [`paste_block`] appends.
fn opens_string_sequence(intro: char) -> bool {
    matches!(intro, ']' | 'P' | 'X' | '^' | '_')
}

/// Enforce the no-embedded-paste-marker invariant on the tail of `out`.
///
/// **Both** markers are neutralized, not only the END one: an END marker
/// closes the block early and the remainder is read as terminal INPUT, while
/// a BEGIN marker re-opens it, so [`paste_block`]'s real END closes the
/// *inner* block and the terminal is still in paste mode for whatever the
/// operator types next. Dropping the `ESC` leaves the text readable as a
/// literal `[201~` / `[200~`.
///
/// This is an invariant on the OUTPUT, re-checked after every push, rather
/// than a search over the input: stripping a control byte can otherwise
/// reconstitute a marker (`ESC [ 2 NUL 0 1 ~`). Only the characters just
/// pushed can have completed one, so checking the tail here is exhaustive;
/// the loop repeats because removing an `ESC` can expose another marker.
///
/// Returns how many `ESC`s it removed. The caller needs that: the `ESC` it
/// drops is a `CSI` introducer, and a `CSI` arriving mid-string is what
/// [`sanitize_submit_body`] took as *ending* an open string sequence. Once
/// the `ESC` is gone that sequence is open again.
fn neutralize_trailing_paste_marker(out: &mut String) -> usize {
    let mut removed = 0;
    loop {
        let marker_len = if out.as_bytes().ends_with(BRACKETED_PASTE_END) {
            BRACKETED_PASTE_END.len()
        } else if out.as_bytes().ends_with(BRACKETED_PASTE_BEGIN) {
            BRACKETED_PASTE_BEGIN.len()
        } else {
            return removed;
        };
        // Both markers are pure ASCII, so this is a char boundary.
        let esc_at = out.len() - marker_len;
        out.remove(esc_at);
        removed += 1;
    }
}

/// Neutralize a message body before it is framed into a bracketed-paste
/// block by [`paste_block`].
///
/// This is a **narrow** neutralizer, deliberately NOT
/// [`crate::terminal::strip_ansi`]: that one drops every CSI/OSC/two-char
/// escape and is correct for the OUTBOUND reader, but applied inbound it
/// would silently corrupt exactly the content agents legitimately send
/// (coloured build logs, diffs). Two things are removed and nothing else:
///
/// 1. **Either bracketed-paste marker (`\x1b[201~`, `\x1b[200~`)** — see
///    [`neutralize_trailing_paste_marker`] for what each one buys a caller
///    and why the rule is an invariant on the output.
/// 2. **C0/C1 control characters other than `\n`, `\r`, `\t`** — plus
///    `DEL`, and any `ESC` not introducing a real escape sequence.
///
/// The one exception to rule 2 is a **string sequence's own terminator**.
/// `OSC` / `DCS` / `SOS` / `PM` / `APC` run until an explicit `ST`
/// (`\x1b\\`) or, for `OSC`, a `BEL` — and `BEL` is otherwise a C0 control
/// this function strips. Dropping it would leave an unterminated sequence
/// that consumes everything after it, the paste END marker included, so a
/// terminator is preserved while a string sequence is open, and one that is
/// never closed gets an `ST` appended.
fn sanitize_submit_body(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut chars = message.chars().peekable();
    // Inside an `OSC` / `DCS` / `SOS` / `PM` / `APC` that is not terminated
    // yet?
    let mut in_string_sequence = false;
    // What that flag was immediately before the most recent kept `ESC`, so
    // the transition can be undone if the neutralizer removes that `ESC`.
    let mut string_state_before_escape = false;
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\r' | '\t' => out.push(c),
            // BEL is a C0 control everywhere except here, where it is OSC's
            // terminator and load-bearing.
            '\u{7}' if in_string_sequence => {
                out.push(c);
                in_string_sequence = false;
            }
            '\u{1b}' => match chars.peek().copied() {
                Some(next) if is_escape_introducer(next) => {
                    // Consume the introducer alongside the ESC so the
                    // sequence is classified once, here, rather than
                    // re-derived from the output later.
                    chars.next();
                    out.push(c);
                    out.push(next);
                    // An ESC inside a string sequence ends it — whether it
                    // is the ST that closes it properly or another sequence
                    // aborting it, which is what a real parser does.
                    string_state_before_escape = in_string_sequence;
                    in_string_sequence = opens_string_sequence(next);
                }
                // Bare or dangling ESC: a control byte, not a sequence.
                _ => {}
            },
            // C0 controls, DEL, and C1 controls.
            c if (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32)) => {}
            c => out.push(c),
        }
        if neutralize_trailing_paste_marker(&mut out) > 0 {
            // That `ESC` is gone, so the `CSI` it introduced no longer ends
            // an open string sequence: `ESC ] 0 ; ESC [ 2 0 1 ~` must not
            // leave the OSC open to swallow the paste END marker.
            in_string_sequence = string_state_before_escape;
        }
    }
    // A string sequence left open would swallow the paste END marker and
    // whatever the terminal prints after it. Close it.
    if in_string_sequence {
        out.push_str("\u{1b}\\");
    }
    out
}

/// Frame a message as a bracketed-paste block: begin marker, the
/// [`sanitize_submit_body`]-neutralized body, end marker.
///
/// The single choke point through which every inbound prompt reaches the
/// PTY — [`TerminalSession::submit_prompt`] (production) and
/// [`build_submit_payload`] (tests) both go through here, so the framing
/// cannot drift between them and the sanitizer cannot be bypassed by any
/// of the inbound producers (coord session-bus injection, the
/// caller-supplied `POST /terminals/{id}/submit-prompt` route, and the
/// regex auto-responder).
fn paste_block(message: &str) -> Vec<u8> {
    let body = sanitize_submit_body(message);
    let mut out =
        Vec::with_capacity(BRACKETED_PASTE_BEGIN.len() + body.len() + BRACKETED_PASTE_END.len());
    out.extend_from_slice(BRACKETED_PASTE_BEGIN);
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(BRACKETED_PASTE_END);
    out
}

/// Build the exact byte sequence [`TerminalSession::submit_prompt`] writes.
/// Exposed so tests (and the worker_session unit test) can assert the
/// submit framing without spinning up a real PTY.
///
/// Shares [`paste_block`] with the production path so the two shapes cannot
/// drift; the trailing CR is appended here because production writes it in a
/// separate lock acquisition after [`POST_PASTE_DELAY`].
pub(crate) fn build_submit_payload(message: &str) -> Vec<u8> {
    let mut out = paste_block(message);
    out.extend_from_slice(SUBMIT_ENTER);
    out
}

/// What [`TerminalSession::submit_prompt`] will actually put on the wire for
/// a given message — see [`submit_payload_info`].
pub(crate) struct SubmitPayload {
    /// Total bytes written to the PTY: the bracketed-paste block plus the
    /// trailing CR.
    pub bytes: usize,
    /// Did [`sanitize_submit_body`] change the body? A caller whose message
    /// carried a paste marker or a control byte is told so, rather than
    /// having it altered silently.
    pub sanitized: bool,
}

/// Describe the write [`TerminalSession::submit_prompt`] would perform,
/// without performing it.
///
/// Exists because the body is neutralized at the PTY choke point, so
/// `message.len() + framing` — what the `POST /terminals/{id}/submit-prompt`
/// route reported before the sanitizer landed — is no longer the number of
/// bytes that reach the terminal. Both fields come from
/// [`sanitize_submit_body`] and the framing constants, so the reported count
/// cannot drift from what [`paste_block`] emits.
pub(crate) fn submit_payload_info(message: &str) -> SubmitPayload {
    let body = sanitize_submit_body(message);
    SubmitPayload {
        bytes: BRACKETED_PASTE_BEGIN.len()
            + body.len()
            + BRACKETED_PASTE_END.len()
            + SUBMIT_ENTER.len(),
        sanitized: body != message,
    }
}

/// Pure line-assembly core of the typed-input observer
/// ([`TerminalSession::observe_input`]): walk one chunk of raw keystroke
/// bytes, mutating the per-terminal line buffer, and return any lines this
/// chunk completed (CR/LF-submitted, non-blank). Heuristic by design:
/// backspace/DEL pop one char, printable ASCII accumulates, control bytes
/// and non-ASCII are dropped. Capped at 4 KiB so a pathological no-newline
/// stream (e.g. a huge paste) can't grow unbounded.
fn consume_input_bytes(buf: &mut String, data: &[u8]) -> Vec<String> {
    let mut completed: Vec<String> = Vec::new();
    for &b in data {
        match b {
            // CR or LF — submit the line.
            0x0D | 0x0A => {
                if !buf.trim().is_empty() {
                    completed.push(std::mem::take(buf));
                } else {
                    buf.clear();
                }
            }
            // Backspace / DEL — approximate edit handling.
            0x08 | 0x7F => {
                buf.pop();
            }
            // Printable ASCII (incl. space).
            0x20..=0x7E => buf.push(b as char),
            _ => {}
        }
    }
    if buf.len() > 4096 {
        buf.clear();
    }
    completed
}

/// A single PTY-backed terminal session.
pub struct TerminalSession {
    /// Unique identifier for this terminal.
    id: TerminalId,
    /// Display title. Mutated post-spawn by [`Self::set_title`] (Phase 2 of
    /// the bi-directional title sync — frontend OSC 0 observers in xterm.js
    /// call back via `terminal_set_title` Tauri command).
    title: Arc<Mutex<String>>,
    /// Working directory the shell was started in.
    working_dir: String,
    /// Which terminal page this session belongs to. Mutated post-spawn by
    /// [`Self::set_page`] (the `POST /terminals/{id}/move` surface routes
    /// through `TerminalManager::set_page` to here), so it mirrors `title`'s
    /// `Arc<Mutex<String>>` shape rather than a frozen spawn-time `String`.
    page_id: Arc<Mutex<String>>,
    /// Thread-safe writer to PTY stdin.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Handle to the PTY master (needed for resize).
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Child process PID.
    child_pid: Option<u32>,
    /// Current terminal dimensions (atomic for lock-free resize from &self).
    cols: AtomicU16,
    rows: AtomicU16,
    /// Whether the shell process is still alive.
    is_alive: Arc<AtomicBool>,
    /// Exit code (set when process exits).
    exit_code: Arc<Mutex<Option<i32>>>,
    /// Handle to the reader thread (for join on cleanup).
    reader_join: Mutex<Option<thread::JoinHandle<()>>>,
    /// Handle to the waiter thread (for join on cleanup).
    waiter_join: Mutex<Option<thread::JoinHandle<()>>>,
    /// Bytes received by the frontend (for flow control).
    bytes_sent: Arc<AtomicU64>,
    bytes_acked: Arc<AtomicU64>,
    /// True when the reader skipped at least one webview emission while the
    /// [`EmissionGate`] was paused and no resume-marker has been sent yet.
    /// Read + cleared by [`Self::ack`]: once acks bring the gap back under
    /// the LOW watermark, a zero-length marker event stamped at the current
    /// produced offset is emitted so the frontend's offset-gap detection
    /// triggers a scrollback-ring resync — without it, a burst that ENDS
    /// while emission is paused would leave the pane stale forever (no
    /// further chunk arrives to reveal the gap).
    emission_skipped: Arc<AtomicBool>,
    /// Phase 5 (A4 backend half) — how much webview service this session is
    /// entitled to, merged across every window that reports on it. Read by the
    /// reader thread on every chunk (one relaxed atomic load); written by
    /// `terminal_set_visibility` on layout changes only.
    visibility: Arc<VisibilityState>,
    /// The open webview window of whichever tier is holding: `background`
    /// always, and `unwatched` too once the operator configures a cadence for
    /// it. One buffer, not one per tier — the tier can change with bytes still
    /// in it, which is why the spacing is resolved per flush from the CURRENT
    /// tier ([`hold_interval`]) rather than fixed when the window opened.
    ///
    /// Chunks accumulate here since the last flush, and are drained by the
    /// reader thread on the next chunk, by the visibility sweeper when the
    /// session went quiet mid-window, or by the reader's exit flush. The mutex
    /// is also the serialization point for this session's webview emission,
    /// which is what keeps held bytes ahead of anything emitted after a tier
    /// change.
    background_hold: Arc<Mutex<BackgroundHold>>,
    /// Flush spacing for the `background` tier, resolved once at spawn from
    /// `settings.performance` (many-sessions plan Phase 8). Read by the
    /// sweeper's quiet-tail flush; the reader thread carries its own copy.
    background_flush_interval: Duration,
    /// Flush spacing for the `unwatched` tier, or `None` (the stock default)
    /// when that tier emits nothing to the webview at all. `Some` turns
    /// [`Self::emit_activity_digest_if_due`] off for this session — the page
    /// tap is then feeding tracking from real output and a digest would
    /// double-count the activity sparkline.
    unwatched_flush_interval: Option<Duration>,
    /// Cadence gate for the `terminal-activity` digest that feeds state
    /// tracking while this session is `unwatched`.
    activity_digest: Arc<Mutex<ActivityDigestState>>,
    /// Ring buffer of recent raw PTY output for reconnection.
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    /// Monotonic counter of all bytes ever produced by the PTY.
    total_bytes_produced: Arc<AtomicU64>,
    /// Monotonic counter of grid mutations, bumped AFTER the mutation is
    /// visible (see [`advance_grid`] and [`Self::resize`]). This — not
    /// `total_bytes_produced` — is what the periodic grid scanners gate on;
    /// see the `terminal::scan_gate` module docs for why the ordering is
    /// load-bearing.
    grid_generation: Arc<AtomicU64>,
    /// Unix timestamp in milliseconds when the session was created.
    created_at: u64,
    /// Broadcast channel for HTTP/SSE subscribers to receive base64-encoded output chunks.
    output_tx: broadcast::Sender<String>,
    /// Server-side cell grid produced by the VT parser tee in the reader thread.
    grid: Arc<Mutex<Grid>>,
    /// One-shot sender fired by the reader thread when it observes the
    /// first OSC 0 / OSC 2 title from the child process. Used by
    /// `spawn_worker_session` (Phase 1) to gate `Initializing → Ready` on
    /// readline visibility: Claude Code's CLI emits an OSC 0 title
    /// (`"✳ Claude Code"`) on startup, so this resolves ~150–300 ms after
    /// child spawn. Wrapped in `Mutex<Option<...>>` because the sender is
    /// consumed (`take()`'d) on first fire — subsequent OSC 0s do not
    /// re-fire.
    first_osc_title_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// One-shot receiver, taken at most once by
    /// [`Self::subscribe_first_osc_title`]. After the take, callers that
    /// ask again get `None` and should treat the worker as already ready
    /// (the OSC may have fired before they could subscribe, in which case
    /// the sender already consumed the slot above).
    first_osc_title_rx: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
    /// Coord-native session id, set after `register_external()` wires this
    /// terminal into the coordinator's session plane. `None` until wired;
    /// read by `terminal_close` so it can close the coord mirror.
    coord_session_id: Arc<Mutex<Option<uuid::Uuid>>>,
    /// R1 (session-lifecycle-cleanup) — best-effort hook invoked by the
    /// waiter thread the instant the backing PTY process exits. Wired by
    /// `terminal_create` alongside [`Self::set_coord_session_id`]: it
    /// closes the coord session mirror for `coord_session_id` so a process
    /// that dies (operator types `exit`, shell crashes, etc.) doesn't leave
    /// a ghost `active` row on the dashboard until coord's own stale→closed
    /// watcher reaps it (the runner no longer self-closes abandoned
    /// sessions; see coord_sync plan A3). Held in a shared
    /// slot so the already-spawned waiter thread can read it once the
    /// registration completes; cloned into the waiter at spawn time.
    /// Idempotent against the frontend `terminal_close` path because
    /// `SessionRegistry::close` is itself idempotent.
    on_exit: Arc<Mutex<Option<Box<dyn Fn(uuid::Uuid) + Send + Sync>>>>,
    /// Phase 2 of `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md`.
    /// When the session declared edit intent on a registered repo and
    /// `worktree_mode_enabled()` was true at spawn time, this carries
    /// the allocated `IsolatedEditContext`. The context's `Drop` impl
    /// stops the heartbeat task and fires a best-effort claim release;
    /// `close()` clears the slot first so release fires before PTY
    /// teardown.
    isolated_edit_ctx:
        Arc<Mutex<Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>>>,
    /// App handle, retained so the typed-input observer
    /// ([`Self::observe_input`]) can dispatch its effects (coord warning
    /// event; lifecycle-store registration + bypass event of the typed
    /// claude resume sniff) off the PTY write path.
    /// `None` only in unit-test fixtures that don't drive a real app.
    app_handle: Option<AppHandle>,
    /// Accumulates printable keystroke bytes between line submits so
    /// completed lines can be matched by the typed-input consumers (L3
    /// branch-mutating-git warn; typed claude resume sniff). Drained on
    /// CR/LF; see [`consume_input_bytes`]. Cheap on the hot path.
    input_line_buf: Arc<Mutex<String>>,
}

impl TerminalSession {
    /// Spawn a new terminal session with a shell process.
    ///
    /// `command` is an optional program-and-args override (Decision 3 of
    /// `plans/2026-06-05-visible-gate-continuations-and-plan-ready-predicate.md`):
    /// when `Some([program, args…])` the session runs that program as its PTY
    /// child instead of the interactive shell — this is how the gate-continuation
    /// terminal branch launches the `claude` CLI directly with the prompt as
    /// argv. When `None` (every operator-opened / frontend terminal), the session
    /// falls back to [`Self::build_shell_command`] byte-for-byte, so the
    /// interactive-terminal path is untouched.
    ///
    /// `resource_override` carries the operator's explicit "start it anyway"
    /// past the spawn-time resource gate (plan
    /// `2026-08-07-runner-resource-guard-and-session-protection` §Part D). The
    /// gate lives HERE, in the PTY seam, and not in the React handler, because
    /// this function is reachable from the gate-continuation branch (via its
    /// `command` override) and from every backend/HTTP spawn surface — a guard
    /// that only runs in the frontend is not a guard for the paths that spawn
    /// unattended. See [`crate::resource_guard`] for the verdict ladder and for
    /// why it fails open.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        id: TerminalId,
        title: String,
        working_dir: String,
        page_id: String,
        cols: u16,
        rows: u16,
        app_handle: AppHandle,
        interceptor: Arc<OutputInterceptor>,
        command: Option<Vec<String>>,
        extra_env: Option<Vec<(String, String)>>,
        resource_override: bool,
    ) -> Result<Self, String> {
        // Spawn-time resource gate — BEFORE the PTY is opened, so a refusal
        // leaves no half-built session behind and nothing already running is
        // touched. Below the warn floor this emits a notice and returns
        // `Ok(())`; below the critical floor it returns the typed refusal that
        // `src/lib/resourceGuard.ts` turns into the "Start anyway" dialog. Any
        // unreadable sensor proceeds silently.
        crate::resource_guard::admit_spawn(
            "terminal session",
            resource_override,
            Some(&app_handle),
        )?;

        let pty_system = native_pty_system();

        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {}", e))?;

        // Build the PTY child command: an explicit program+args override
        // (Decision 3) when supplied, else the interactive shell.
        let mut cmd = Self::build_command_from(command);

        // Set working directory
        let cwd = if working_dir.is_empty() {
            dirs::home_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string())
        } else {
            working_dir.clone()
        };
        cmd.cwd(&cwd);

        // The terminal-invariant half of the child env: markers, TERM, the
        // runner port + briefing, and the non-interactive git posture. Extracted
        // so `config_report`'s G3 generation can CALL it instead of restating
        // what it does — the same reason `finalize_child_env` below is extracted.
        Self::apply_base_child_env(&mut cmd);

        // Phase 2c — caller-supplied launch env (e.g.
        // `QONTINUI_SESSION_WORKTREES`, the agent-agnostic pointer to every
        // materialized sibling worktree of this session). Set after the
        // built-in runner vars so a caller can intentionally override them.
        // Capture any caller-pinned CLAUDE_CONFIG_DIR BEFORE `extra_env` is
        // consumed by the loop below — a caller pin (backend continuation /
        // account-migration respawn) is the authoritative account for this
        // session and is what the spawn-time record must bind.
        let caller_config_dir_value: Option<String> = extra_env.as_ref().and_then(|env| {
            env.iter()
                .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
                .map(|(_, v)| v.clone())
        });
        let caller_pinned_config_dir = caller_config_dir_value.is_some();
        if let Some(env) = extra_env {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        // Resolve the EFFECTIVE Claude config dir that is (or is about to be)
        // placed into the PTY env — the caller pin wins, else the credential-
        // aware resolved account dir. Computed ONCE here so the same value both
        // stamps the spawn-time authoritative record (so an autonomous boot-
        // resume runs under the CORRECT account instead of account-blind) AND is
        // set on the child env below. `get_effective_config_dir` runs only when
        // no caller pin — identical to the prior behavior.
        let effective_claude_config_dir: Option<String> = caller_config_dir_value.or_else(|| {
            // Phase 0 instrumentation: this is B3/B5 — an uncached
            // `settings.json` read + double parse, plus a possible INLINE
            // blocking OAuth refresh POST inside `get_effective_config_dir`.
            let _span =
                tracing::debug_span!("terminal_spawn.resolve_config_dir", terminal_id = %id)
                    .entered();
            let ai_settings = crate::settings::get_ai_settings();
            let (dir, _config_dir_source) =
                crate::ai_provider::get_effective_config_dir(&ai_settings.claude_cli);
            dir
        });

        // ---- ALWAYS-ON session-restore identity seam (plan §3b) -------------
        // NOT gated by any flag — the out-of-box session-restore guarantee
        // (Principle 2) must apply to every user with zero setup. Generates a
        // per-terminal session UUID, injects it + the terminal id as env,
        // materializes the always-on `claude`/`gemini` identity shims and
        // prepends their dir to PATH, and records the session AUTHORITATIVELY at
        // spawn (zero transcript race — the §3b determinism mechanism). Runs
        // AFTER caller `extra_env` so the identity dir wins on PATH. Fail-open:
        // any failure injects nothing and the terminal still spawns.
        {
            // Phase 0 instrumentation: the identity seam is the largest single
            // block of synchronous I/O on the spawn path (hook files, coord-mcp
            // provisioning, shim materialization, the lifecycle-store record).
            // The inner segments carry their own child spans.
            let _span =
                tracing::debug_span!("terminal_spawn.identity_seam", terminal_id = %id).entered();
            Self::apply_identity_seam(
                &mut cmd,
                &id,
                &app_handle,
                &cwd,
                &title,
                &page_id,
                effective_claude_config_dir.clone(),
            );
        }

        // ---- Install-interception PATH-shim seam (plan §4 Phase 1) ----------
        // Behind the master flag `QONTINUI_INSTALL_INTERCEPT_ENABLED` (default
        // OFF — ships dark). SEPARATE from the always-on identity seam above:
        // they SHARE the materializer/seam/loopback plumbing but NOT the master
        // enable flag (robustness — the out-of-box guarantee cannot ride a
        // default-dark flag). When OFF this block is a no-op. When ON,
        // materialize the per-terminal install-shim bin dir and prepend it to
        // PATH + set the loopback port/mode so an agent that types `npm install …`
        // is transparently declared/observed. Fail-open: any materialize failure
        // injects nothing.
        Self::apply_install_intercept_env(&mut cmd, &id);

        // The LAST env mutations before the spawn — account pin, then the
        // credential scrub. Extracted so the ordering is unit-testable; see the
        // function's doc comment for why it must stay last.
        Self::finalize_child_env(
            &mut cmd,
            effective_claude_config_dir.as_deref(),
            caller_pinned_config_dir,
        );

        // Spawn the child process
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Failed to spawn shell: {}", e))?;

        let child_pid = child.process_id();
        info!(
            terminal_id = %id,
            pid = ?child_pid,
            cwd = %cwd,
            "Terminal session spawned"
        );

        // Assign to Windows Job Object for crash safety
        #[cfg(target_os = "windows")]
        if let Some(pid) = child_pid {
            Self::assign_to_job_object(pid);
        }

        // Get writer and master from the PTY pair
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Failed to take PTY writer: {}", e))?;
        let writer = Arc::new(Mutex::new(writer));

        let is_alive = Arc::new(AtomicBool::new(true));
        let exit_code: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let bytes_sent = Arc::new(AtomicU64::new(0));
        let bytes_acked = Arc::new(AtomicU64::new(0));
        let emission_skipped = Arc::new(AtomicBool::new(false));
        let visibility = Arc::new(VisibilityState::new());
        let background_hold = Arc::new(Mutex::new(BackgroundHold::new(Instant::now())));
        // Operator-tunable emission cadences (Phase 8), resolved ONCE here for
        // the same reason the ring size is: the reader thread and the sweeper
        // must agree for this session's whole life even if the setting changes
        // underneath them.
        let (background_flush_interval, unwatched_flush_interval) = resolved_flush_intervals();
        let activity_digest = Arc::new(Mutex::new(ActivityDigestState::new(Instant::now())));
        // Operator-tunable ring size (Phase 8). Resolved ONCE here, then
        // carried by the reader thread — the ring is allocated to it and the
        // per-chunk tee is bounded by it, so both halves agree for this
        // session's whole life even if the setting changes underneath.
        let scrollback_capacity = resolved_scrollback_capacity();
        let scrollback_buffer = Arc::new(Mutex::new(VecDeque::with_capacity(scrollback_capacity)));
        let total_bytes_produced = Arc::new(AtomicU64::new(0));
        let grid_generation = Arc::new(AtomicU64::new(0));
        let grid = Arc::new(Mutex::new(Grid::new(cols, rows)));
        let (output_tx, _) = broadcast::channel::<String>(256);
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        // Phase 1: one-shot fired by the reader thread on the first OSC
        // 0/2 title transition. The session owns the receiver until a
        // caller (`spawn_worker_session`) takes it via
        // `subscribe_first_osc_title`; the reader owns the sender via
        // an Arc<Mutex<Option<...>>> slot and `take`s it on first fire.
        let (osc_title_tx, osc_title_rx) = oneshot::channel::<()>();
        let first_osc_title_tx = Arc::new(Mutex::new(Some(osc_title_tx)));
        let first_osc_title_rx: Arc<Mutex<Option<oneshot::Receiver<()>>>> =
            Arc::new(Mutex::new(Some(osc_title_rx)));

        // Get a reader from the master PTY
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Failed to clone PTY reader: {}", e))?;

        // Coord mirror identity, populated by `terminal_create` after
        // `register_external` returns. Declared HERE (ahead of the reader
        // thread) rather than beside the waiter below because the reader needs
        // a clone for the OSC 9999 agent-status sideband: a payload for a
        // terminal with no coord mirror has nowhere to go and is dropped.
        let coord_session_id: Arc<Mutex<Option<uuid::Uuid>>> = Arc::new(Mutex::new(None));

        // Spawn reader thread: reads PTY output → interceptor → scrollback + Tauri event
        let reader_id = id.clone();
        let reader_app = app_handle.clone();
        let reader_alive = is_alive.clone();
        let reader_bytes_sent = bytes_sent.clone();
        let reader_bytes_acked = bytes_acked.clone();
        let reader_emission_skipped = emission_skipped.clone();
        let reader_visibility = visibility.clone();
        let reader_background_hold = background_hold.clone();
        let reader_background_flush_interval = background_flush_interval;
        let reader_unwatched_flush_interval = unwatched_flush_interval;
        let reader_scrollback = scrollback_buffer.clone();
        let reader_total_bytes = total_bytes_produced.clone();
        let reader_output_tx = output_tx.clone();
        let reader_grid = grid.clone();
        let reader_grid_generation = grid_generation.clone();
        let reader_osc_title_tx = first_osc_title_tx.clone();
        let reader_coord_session_id = coord_session_id.clone();
        // Per-session OSC 9999 coalescer — see
        // `terminal::agent_status_sideband::SidebandRateLimiter`. Lives on the
        // reader thread (plus its deferred-flush tasks), NOT in a
        // process-global map, so it dies with the terminal and needs no reaping.
        let reader_agent_status_limiter = Arc::new(Mutex::new(
            crate::terminal::agent_status_sideband::SidebandRateLimiter::new(),
        ));
        let reader_handle = thread::Builder::new()
            .name(format!("terminal-reader-{}", &id))
            .spawn(move || {
                let mut parser = vte::Parser::new();
                let mut buf = [0u8; 8192];
                // Phase 4 — sync-output (DEC 2026) frame coalescing. While the
                // VT parser is inside a `?2026h … ?2026l` block (Claude's TUI
                // brackets each full-frame redraw this way), accumulate the
                // processed bytes here instead of emitting per-read. Once the
                // block closes — or a safety cap trips so a never-closed block
                // can't stall output — emit the whole frame as ONE
                // `terminal-output` event so xterm receives the frame in a
                // single write and its RenderDebouncer paints one reflow.
                //
                // The common case (a whole frame in one read) is unaffected:
                // `sync_output` is already false post-advance, so we emit
                // immediately with no holding. Only frames that span multiple
                // reads are held.
                let mut coalescer = SyncFrameCoalescer::new();
                // Flow-control hysteresis state (see EmissionGate): once the
                // unacked gap crosses the High watermark, webview EMISSION
                // pauses until the gap falls back under Low. The PTY read
                // itself is NEVER paused — pausing reads hard-blocked child
                // processes on stdout for every terminal without a mounted
                // pane (deselected terminal pages) and froze the VT grid the
                // auto-response rules scan. RefCell because `emit_chunk` must
                // stay `Fn` (it is shared with the coalescer by `&`-borrow);
                // the reader thread is the sole accessor.
                let emission_gate = std::cell::RefCell::new(EmissionGate::new());

                // Emit one `terminal-output` chunk stamped at absolute
                // `offset`: mirror it to SSE + the backend relay
                // unconditionally, then — when `gated`, only while the
                // webview's unacked gap is under the flow-control watermarks
                // — deliver it to the webview and advance the "sent" counter
                // by the emitted length (so backpressure tracks bytes
                // actually delivered to the webview, not bytes still held in
                // the coalescer and not bytes dropped while emission is
                // paused). Skipped chunks set `emission_skipped` (drives the
                // resume-marker in [`TerminalSession::ack`]) and leave an
                // offset discontinuity in the event stream; the frontend
                // detects it and resyncs from the scrollback ring. The exit
                // flush passes `gated: false` — the dying terminal's final
                // frame is bounded (≤ SYNC_FLUSH_BYTE_CAP) and there may be
                // no future chunk to reveal a gap after it.
                //
                // Consumer gating (perf): each of the three legs is checked
                // BEFORE the shared base64 encode, so a chunk nobody can
                // receive — webview emission paused by flow control or turned
                // off by the visibility tier, no SSE subscriber, no WS relay
                // client — costs zero encoding and zero allocation. Previously
                // the encode AND a full `String` clone for the SSE send ran
                // unconditionally on every chunk of every session. The
                // flow-control decision itself is unchanged; it is only
                // evaluated earlier in the same call.
                //
                // Visibility tiering (Phase 5 / A4) applies to the webview leg
                // ONLY, and is evaluated in [`admit_to_webview`] AFTER the SSE
                // and WS legs have been served: the SSE broadcast, the WS relay
                // and (through them) the coord output pipe receive every chunk,
                // in order, byte-identical, in every tier.
                let emit_impl = |payload: &[u8], offset: u64, gated: bool| {
                    let tier = reader_visibility.tier();
                    let sent = reader_bytes_sent.load(Ordering::Relaxed);
                    let acked = reader_bytes_acked.load(Ordering::Relaxed);
                    let gap = sent.saturating_sub(acked);
                    let admission = admit_to_webview(
                        tier,
                        gated,
                        &mut emission_gate.borrow_mut(),
                        gap,
                        reader_unwatched_flush_interval.is_some(),
                    );
                    if admission == WebviewAdmission::Skip {
                        // The webview is missing these bytes; raise the flag
                        // that makes `ack` (or a tier upgrade) emit a resume
                        // marker so the frontend resyncs from the ring.
                        reader_emission_skipped.store(true, Ordering::Relaxed);
                    }

                    let to_sse = reader_output_tx.receiver_count() > 0;
                    let to_ws = crate::event_system::ws_notification_has_receivers(&reader_app);
                    if admission == WebviewAdmission::Skip && !to_sse && !to_ws {
                        return;
                    }

                    // A held chunk is not the payload the webview will receive
                    // (the flush ships the whole accumulated window), so it
                    // does not need encoding here — one encode per flush
                    // window instead of one per chunk.
                    let encoded = if to_sse || to_ws || admission == WebviewAdmission::Now {
                        Some(STANDARD.encode(payload))
                    } else {
                        None
                    };

                    // Broadcast to HTTP/SSE subscribers (skipped when none —
                    // the `send` would drop the value anyway, and the clone is
                    // a full copy of the base64 payload).
                    if let (true, Some(encoded)) = (to_sse, encoded.as_ref()) {
                        let _ = reader_output_tx.send(encoded.clone());
                    }
                    // Broadcast to backend relay for remote mobile access
                    if let (true, Some(encoded)) = (to_ws, encoded.as_ref()) {
                        crate::event_system::broadcast_ws_notification(
                            &reader_app,
                            "terminal-output",
                            &serde_json::json!({
                                "terminal_id": &reader_id,
                                "data": encoded,
                            }),
                        );
                    }

                    if admission == WebviewAdmission::Skip {
                        return;
                    }

                    // Serialize the webview leg on the hold mutex so held
                    // background bytes can never be overtaken by a chunk the
                    // tier let through after them.
                    let mut hold = reader_background_hold
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let now = Instant::now();
                    match admission {
                        WebviewAdmission::Hold => {
                            hold.push(payload, offset);
                            let interval = hold_interval(
                                tier,
                                reader_background_flush_interval,
                                reader_unwatched_flush_interval,
                            );
                            if let Some((window, window_offset)) =
                                hold.take_if_due(now, interval)
                            {
                                emit_terminal_output(
                                    &reader_app,
                                    &reader_id,
                                    &STANDARD.encode(&window),
                                    window_offset,
                                );
                            }
                        }
                        WebviewAdmission::Now => {
                            // Drain anything the previous tier held first.
                            if let Some((window, window_offset)) = hold.take_now(now) {
                                emit_terminal_output(
                                    &reader_app,
                                    &reader_id,
                                    &STANDARD.encode(&window),
                                    window_offset,
                                );
                            }
                            let encoded = encoded.expect("Now always encodes");
                            emit_terminal_output(&reader_app, &reader_id, &encoded, offset);
                            // Flow control accounts only for bytes delivered
                            // under the gate — i.e. the `focused` tier and the
                            // exit flush. Counting held/hidden bytes would let
                            // the gap balloon while nobody can ack, so the pane
                            // would find the gate already paused on its way
                            // back to `focused`.
                            reader_bytes_sent.fetch_add(payload.len() as u64, Ordering::Relaxed);
                        }
                        WebviewAdmission::Skip => unreachable!("returned above"),
                    }
                };
                let emit_chunk = |payload: &[u8], offset: u64| emit_impl(payload, offset, true);

                loop {
                    if !reader_alive.load(Ordering::Relaxed) {
                        break;
                    }

                    // Time-cap a held sync block: if the block has been open ≥
                    // SYNC_FLUSH_TIME_CAP, flush it now rather than waiting for
                    // the next read to return. (The read below blocks, so a
                    // slow PTY mid-frame could otherwise hold a frame past one
                    // frame interval until the next byte arrives.)
                    coalescer.flush_if_timed_out(&emit_chunk);

                    match reader.read(&mut buf) {
                        Ok(0) => {
                            debug!(terminal_id = %reader_id, "PTY reader got EOF");
                            break;
                        }
                        Ok(n) => {
                            let data = interceptor.process(&reader_id, &buf[..n]);

                            // Tee processed output into the per-session
                            // scrollback ring buffer + byte counter. Shared
                            // with the distinct-buffers regression test so
                            // both drive the identical teeing path. The
                            // returned start offset is stamped onto the
                            // event below for replay dedup.
                            let chunk_offset =
                                tee_into_scrollback(
                                    &reader_scrollback,
                                    &reader_total_bytes,
                                    &data,
                                    scrollback_capacity,
                                );

                            // Tee through the VT parser into the per-session cell grid.
                            // Detect the first OSC 0/2 title transition by
                            // checking whether the grid's title became
                            // `Some` *during* this parser advance. The
                            // sender lives in an `Arc<Mutex<Option<...>>>`
                            // slot we drain on first fire — subsequent
                            // title changes don't re-fire. Worker dispatch
                            // gating in `spawn_worker_session` only needs
                            // the one-shot signal.
                            //
                            // The OSC 9999 agent-status sideband (plan
                            // `2026-08-11-coord-hook-sourced-agent-status`
                            // Channel 2) rides the SAME before/after shape: read
                            // the monotonic sideband seq here, compare it after
                            // the advance, and only clone/drain the payload when
                            // it actually moved. The common case — no sideband in
                            // this chunk — costs one extra `u64` read inside a
                            // lock we were already taking, and no allocation.
                            let (title_was_none, sideband_seq_before) = reader_grid
                                .lock()
                                .ok()
                                .map(|g| (g.title().is_none(), g.agent_status_sideband_seq()))
                                .unwrap_or((false, 0));
                            advance_grid(
                                &reader_grid,
                                &reader_grid_generation,
                                &mut parser,
                                &data,
                            );
                            if title_was_none {
                                let title_is_now_some = reader_grid
                                    .lock()
                                    .ok()
                                    .map(|g| g.title().is_some())
                                    .unwrap_or(false);
                                if title_is_now_some {
                                    if let Ok(mut slot) = reader_osc_title_tx.lock() {
                                        if let Some(tx) = slot.take() {
                                            // Receiver may have been
                                            // dropped (caller didn't
                                            // subscribe). Ignore — the
                                            // sender simply discards the
                                            // signal.
                                            let _ = tx.send(());
                                        }
                                    }
                                }
                            }

                            // Sync-output-aware emit (Phase 4). Read the live
                            // DEC-2026 state straight after the advance above:
                            // if a `?2026h` is still open we're mid-frame, so
                            // accumulate and defer the emit; otherwise flush
                            // (any held prefix + this chunk) as one event.
                            // The sideband's "after" read piggybacks on this
                            // same lock so the no-sideband path takes no extra
                            // one.
                            let (in_sync, sideband_seq_after) = reader_grid
                                .lock()
                                .ok()
                                .map(|g| (g.sync_output(), g.agent_status_sideband_seq()))
                                .unwrap_or((false, sideband_seq_before));

                            // OSC 9999 agent-status sideband — drain + forward.
                            // Only reached when a payload actually arrived in
                            // this chunk. `dispatch` parses in-thread (cheap,
                            // panic-free, never logs the raw payload) and hands
                            // everything past the rate limiter to the async
                            // runtime, so the PTY hot path is never stalled by a
                            // coord write.
                            if sideband_seq_after != sideband_seq_before {
                                let payload = reader_grid
                                    .lock()
                                    .ok()
                                    .and_then(|mut g| g.take_agent_status_sideband());
                                if let Some(payload) = payload {
                                    crate::terminal::agent_status_sideband::dispatch(
                                        &reader_id,
                                        &reader_coord_session_id,
                                        &reader_agent_status_limiter,
                                        payload,
                                    );
                                }
                            }

                            // Coalesce sync-output frames (Phase 4). The
                            // scrollback ring + total counter were already fed
                            // per-read above, so total-byte accounting stays
                            // correct whether or not the coalescer holds.
                            coalescer.feed(&data, chunk_offset, in_sync, &emit_chunk);
                        }
                        Err(e) => {
                            // On Windows, the PTY reader returns an error when the child exits
                            debug!(terminal_id = %reader_id, error = %e, "PTY read error (likely process exit)");
                            break;
                        }
                    }
                }
                // Flush any frame still held in a never-closed sync block so
                // the last frame isn't lost when the reader exits (EOF / read
                // error on child exit). Ungated: after exit no further chunk
                // can reveal an emission gap, so the final frame must reach
                // the webview even when the gate is paused (bounded by
                // SYNC_FLUSH_BYTE_CAP).
                coalescer.flush_remaining(|payload: &[u8], offset: u64| {
                    emit_impl(payload, offset, false)
                });
                // Ship whatever the visibility tier was still HOLDING. That is
                // a different buffer from the coalescer's: `flush_remaining`
                // above fires only when a sync block was open, and the common
                // exit leaves it empty while the tier's window still holds the
                // tail of the last burst.
                //
                // Without this the tail depends on the sweeper still finding
                // the session — and `TerminalManager::close` REMOVES it from
                // the map before calling `close()`, so a terminal that exits
                // and is closed inside one flush interval loses those bytes
                // outright. Ungated for the same reason the coalescer flush is:
                // after exit no later chunk can reveal a gap, so the final
                // frame must land in every tier.
                //
                // A no-op when `flush_remaining` did fire: that goes through
                // `emit_impl(..., gated: false)` -> `Now`, which drains the hold
                // first (older bytes ship first, as everywhere else).
                {
                    let mut hold = reader_background_hold
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if let Some((window, window_offset)) = hold.take_now(Instant::now()) {
                        emit_terminal_output(
                            &reader_app,
                            &reader_id,
                            &STANDARD.encode(&window),
                            window_offset,
                        );
                    }
                }
                debug!(terminal_id = %reader_id, "Reader thread exiting");
            })
            .map_err(|e| format!("Failed to spawn reader thread: {}", e))?;

        // R1 — shared slots the waiter thread reads on PTY exit so it can
        // close the coord session mirror immediately (vs. leaving it for
        // coord's stale→closed watcher; see coord_sync plan A3). Both are
        // populated AFTER spawn by
        // `terminal_create` once `register_external` returns the coord id,
        // so the waiter reads them at exit time rather than capturing a
        // value that isn't known yet at spawn. (`coord_session_id` itself is
        // declared above the reader thread, which also needs a clone of it.)
        let on_exit: Arc<Mutex<Option<Box<dyn Fn(uuid::Uuid) + Send + Sync>>>> =
            Arc::new(Mutex::new(None));

        // Spawn waiter thread: detects process exit
        let waiter_id = id.clone();
        let waiter_title = title.clone();
        let waiter_alive = is_alive.clone();
        let waiter_exit = exit_code.clone();
        // Retain a clone for the session struct (input-line warn hook)
        // before the original handle is moved into the waiter thread.
        let session_app_handle = app_handle.clone();
        let waiter_app = app_handle;
        let waiter_coord_session_id = coord_session_id.clone();
        let waiter_on_exit = on_exit.clone();
        let waiter_handle = thread::Builder::new()
            .name(format!("terminal-waiter-{}", &id))
            .spawn(move || {
                // portable-pty's child is not Send, so we must wait in the thread that has it
                let mut child = child;
                let status = child.wait();
                let code = match status {
                    Ok(exit) => {
                        // ExitStatus doesn't expose the code directly on all platforms
                        // via portable-pty. Use success() check.
                        if exit.success() {
                            Some(0)
                        } else {
                            // Try to get the exit code; fall back to 1 for non-zero
                            Some(1)
                        }
                    }
                    Err(e) => {
                        warn!(terminal_id = %waiter_id, error = %e, "Failed to wait on child process");
                        None
                    }
                };

                waiter_alive.store(false, Ordering::Relaxed);
                if let Ok(mut ec) = waiter_exit.lock() {
                    *ec = code;
                }

                info!(terminal_id = %waiter_id, exit_code = ?code, "Terminal process exited");

                let event = TerminalExitEvent {
                    terminal_id: waiter_id.clone(),
                    exit_code: code,
                };
                if let Err(e) = waiter_app.emit("terminal-exit", &event) {
                    warn!(
                        terminal_id = %waiter_id,
                        error = %e,
                        "Failed to emit terminal exit event"
                    );
                }
                // Broadcast to backend relay for remote mobile access
                crate::event_system::broadcast_ws_notification(
                    &waiter_app,
                    "terminal-exit",
                    &serde_json::json!({
                        "terminal_id": &waiter_id,
                        "exit_code": code,
                    }),
                );

                // Push notification for mobile (fire-and-forget)
                crate::commands::workflow_events::emit_terminal_exited(
                    &waiter_id,
                    &waiter_title,
                    code,
                );

                // R1 — close the coord session mirror the instant the PTY
                // process exits, instead of letting it linger `active`
                // until coord's own stale→closed watcher reaps it (the
                // runner no longer self-closes abandoned sessions; see
                // coord_sync plan A3). Read the coord id + hook
                // populated by `terminal_create` after registration; if the
                // terminal was never wired into coord (registration failed,
                // or close raced ahead via the frontend `terminal_close`
                // path which clears nothing here but is idempotent on the
                // registry side), there's simply nothing to do. The hook
                // calls `SessionRegistry::close_by_id`, which is idempotent
                // — a no-op when the session is already Closed.
                let coord_id = waiter_coord_session_id
                    .lock()
                    .ok()
                    .and_then(|slot| *slot);
                if let Some(coord_id) = coord_id {
                    if let Ok(slot) = waiter_on_exit.lock() {
                        if let Some(cb) = slot.as_ref() {
                            debug!(
                                terminal_id = %waiter_id,
                                coord_session = %coord_id,
                                "terminal exit — closing coord session mirror"
                            );
                            cb(coord_id);
                        }
                    }
                }

                // P1 (close-on-clean-exit): on a CLEAN exit only, close the
                // owning `term-N` pop-out window iff this was its last live
                // session. A non-zero / unknown exit stays visible (honesty).
                // Docked sessions (owner "main") and the main window are never
                // auto-closed. Best-effort + cheap; guarded inside the helper.
                if let Some(assignments) = tauri::Manager::try_state::<
                    std::sync::Arc<crate::window_assignments::WindowAssignments>,
                >(&waiter_app)
                {
                    crate::commands::terminal_windows::auto_close_owner_window_if_empty(
                        &waiter_app,
                        assignments.inner(),
                        &waiter_id,
                        code,
                    );
                }
            })
            .map_err(|e| format!("Failed to spawn waiter thread: {}", e))?;

        // Store the master for resize operations
        let master: Box<dyn MasterPty + Send> = pair.master;

        Ok(Self {
            id,
            title: Arc::new(Mutex::new(title)),
            working_dir: cwd,
            page_id: Arc::new(Mutex::new(page_id)),
            writer,
            master: Arc::new(Mutex::new(master)),
            child_pid,
            cols: AtomicU16::new(cols),
            rows: AtomicU16::new(rows),
            is_alive,
            exit_code,
            reader_join: Mutex::new(Some(reader_handle)),
            waiter_join: Mutex::new(Some(waiter_handle)),
            bytes_sent,
            bytes_acked,
            emission_skipped,
            visibility,
            background_hold,
            background_flush_interval,
            unwatched_flush_interval,
            activity_digest,
            scrollback_buffer,
            total_bytes_produced,
            grid_generation,
            created_at,
            output_tx,
            grid,
            first_osc_title_tx,
            first_osc_title_rx,
            coord_session_id,
            on_exit,
            isolated_edit_ctx: Arc::new(Mutex::new(None)),
            app_handle: Some(session_app_handle),
            input_line_buf: Arc::new(Mutex::new(String::new())),
        })
    }

    /// The terminal-INVARIANT half of the PTY child's environment: the nested-
    /// session markers this seam strips, `TERM`, the runner-context markers and
    /// port, the continuation-verdict forward, the runner briefing, and the
    /// non-interactive GitHub credential posture.
    ///
    /// Runs FIRST, before the caller-supplied `extra_env`, so a caller can
    /// intentionally override any of it; the credential scrub in
    /// [`Self::finalize_child_env`] runs LAST and cannot be overridden.
    ///
    /// # Why it is a function
    ///
    /// Exactly [`Self::finalize_child_env`]'s argument, plus one more.
    /// `spawn` opens a real PTY and cannot run in a unit test, so anything
    /// inlined there is untested by construction. And `config_report`'s G3
    /// generation ("what a PTY child spawned RIGHT NOW inherits") has to be able
    /// to CALL this seam rather than restate it: a G3 built from
    /// `new_default_prog()` + `finalize_child_env` alone omitted `TERM`,
    /// `QONTINUI_RUNNER_TERMINAL`, `QONTINUI_RUNNER_API_PORT`, the briefing, the
    /// git posture and both marker strips — so the G1→G3 divergence, whose
    /// heading reads *"anything listed above is a variable the runner process
    /// itself does NOT hold the current value of"*, silently under-reported.
    ///
    /// Every value here is a pure function of the runner's own process state
    /// (env reads and `terminal::runner_context`, whose contract forbids I/O),
    /// which is what makes it safe for a diagnostic to call. The parts of the
    /// spawn seam that are NOT — the identity-shim and install-interception
    /// materializers, the coord-mcp provisioning, the lifecycle record — stay in
    /// [`Self::apply_identity_seam`] / [`Self::apply_install_intercept_env`],
    /// and G3 names them as excluded rather than pretending they are not there.
    pub(crate) fn apply_base_child_env(cmd: &mut CommandBuilder) {
        // Remove CLAUDECODE env var so Claude CLI works inside the terminal
        cmd.env_remove("CLAUDECODE");
        // Same reason, same class of marker: CLAUDE_CODE_CHILD_SESSION says
        // "you are a nested session". A PTY tab is a TOP-LEVEL session, but the
        // runner inherits the marker from whatever launched it (typically the
        // supervisor, which inherits it from a Claude Code session) and would
        // otherwise pass it to every `claude` typed into a pane. Defense in
        // depth — the supervisor strips it at the runner spawn, this covers a
        // runner started by any other means.
        cmd.env_remove(qontinui_runner_lib::claude_env::CLAUDE_CHILD_SESSION_ENV);

        // Set TERM for proper color/capability support.
        // xterm.js is a full xterm-compatible terminal, so use xterm-256color on all
        // platforms. The previous "cygwin" setting on Windows caused issues with tools
        // like Claude Code that check TERM for capability detection.
        cmd.env("TERM", "xterm-256color");

        // Mark this terminal as running inside the Qontinui Runner so that tools
        // (e.g. Claude Code via the shell integration wrapper) can detect the context.
        cmd.env("QONTINUI_RUNNER_TERMINAL", "1");
        // The ACTUALLY-BOUND port, not the bootstrap default — see
        // `crate::terminal::spawn_seam_api_port`. `apply_identity_seam` and
        // `apply_install_intercept_env` in this same file already read the bound
        // port; this seam, which renders the number the session actually READS
        // (its `QONTINUI_RUNNER_API_PORT` and the briefing's endpoints), was the
        // one that did not.
        let runner_api_port = crate::terminal::spawn_seam_api_port();
        cmd.env("QONTINUI_RUNNER_API_PORT", runner_api_port.to_string());

        // Forward the continuation-verdict mode to the bundled `Stop` hook so it
        // can skip its `curl` + `python` round trip while the feature is dark.
        //
        // `claude_stop_hook.sh` USED to fire on every assistant turn of every
        // session in this terminal, and with the flag at its `off` default the
        // endpoint "answers `allow` with zero coord traffic" — so those spawns
        // bought nothing. Measured 1.0-3.4s per turn on this fleet
        // (Windows/MSYS process creation is 0.5-2.3s per spawn).
        //
        // The hook is now gated at REGISTRATION time (`session::claude_hook`):
        // in the default dark posture the delivered `--settings` file carries no
        // `hooks.Stop` key at all, so the per-turn `bash` spawn does not happen
        // in the first place. This forward is therefore belt-and-braces — it
        // still covers a HAND-STARTED `claude` that picks up a settings file
        // written by a previously-ARMED runner, where the script's own dark-mode
        // short-circuit is the only thing standing between the turn and a `curl`.
        //
        // Forward the PARSED mode, not the raw env string: `Mode::from_flag` maps
        // an unknown value to `Off`, so the hook receives the same fail-safe
        // value the runner itself resolved rather than re-parsing it in bash.
        //
        // Freshness, stated rather than hidden: a terminal started before the
        // operator flips the flag keeps the value it was spawned with. That is
        // NOT a regression — `Mode::from_env()` reads the RUNNER's own process
        // env, which itself only changes on a runner restart, so the PTY value is
        // exactly as fresh as the runner's has ever been.
        //
        // Plan: 2026-08-06-stop-hook-per-turn-latency (P3).
        cmd.env(
            crate::mcp::continuation_verdict::FLAG_ENV,
            crate::mcp::continuation_verdict::Mode::from_env().as_str(),
        );
        // Canonical runner-context briefing (pull-first autonomy protocol +
        // links), rendered from the SINGLE source of truth. The shell
        // integration wrapper reads this env var and passes it to
        // `--append-system-prompt` for interactive `claude` panes. Autonomous
        // direct-exec spawns bypass shell integration and inject the same text
        // into their argv instead (see
        // `agent_runtime::build_continuation_claude_command`). Purely additive
        // and fail-open: an empty/unset value simply means no briefing.
        cmd.env(
            "QONTINUI_RUNNER_CONTEXT",
            crate::terminal::runner_context(runner_api_port),
        );

        // P7 — non-interactive GitHub credential posture (plan Phase 6). Stops a
        // git op run from this terminal (ANY cwd, incl. the non-repo umbrella
        // root or an unregistered repo) from reaching Git Credential Manager's
        // blocking GUI popup FOR GITHUB: github.com is made GCM-non-interactive
        // and falls back to the user's `gh` auth. Scope is GithubOnly — this is
        // an interactive human terminal, so other hosts (gitlab/azure/bitbucket)
        // keep their normal interactive auth. Set BEFORE `extra_env` below so a
        // caller may override, and BEFORE the per-session `--local` coord helper
        // (installed elsewhere) which — read earlier in git's config precedence —
        // still wins for coord-registered repos. See
        // `credential_helper::non_interactive_git_env` for the precedence rationale.
        for (k, v) in crate::credential_helper::non_interactive_git_env(
            crate::credential_helper::GitCredentialScope::GithubOnly,
        ) {
            cmd.env(k, v);
        }
    }

    /// The final env mutations applied to a PTY child before it is spawned:
    /// the resolved-account pin, then the credential-value scrub.
    ///
    /// **Why the scrub is last.** Same class as the `CLAUDECODE` /
    /// `CLAUDE_CHILD_SESSION_ENV` strips in [`Self::apply_base_child_env`], but for
    /// credential VALUES: the runner inherits plaintext passwords (Windows
    /// USER-scope vars; the supervisor's forwarded auto-login password) and
    /// would otherwise hand them to every `claude` typed into a pane, where an
    /// `env` dump prints them into the transcript — the habitual
    /// `JWT|KEY|TOKEN|SECRET` redaction filter does not match `PASSWORD`. Name
    /// list and rationale: `crate::terminal::CREDENTIAL_VALUE_ENV_VARS`.
    ///
    /// It runs AFTER the caller-supplied `extra_env` loop, which makes the
    /// strip last-write-wins by construction: no present or future caller can
    /// reintroduce one of these names through `extra_env`. Placing it beside the
    /// marker strips would have left exactly that hole (no caller exploits it
    /// today, but nothing enforced it either).
    ///
    /// **Why it is a function.** Extracting it is what makes the production
    /// call site testable at all — `spawn` opens a real PTY and cannot run in a
    /// unit test, so with the scrub inlined there, deleting it reddened nothing.
    ///
    /// `caller_pinned_config_dir` says the caller already set `CLAUDE_CONFIG_DIR`
    /// through `extra_env` (backend continuation spawns, account-migration
    /// respawns). That pin is a deliberate per-session account choice and must
    /// not be clobbered by the process-global resolved dir, which may point at a
    /// different (or freshly-exhausted) account.
    pub(crate) fn finalize_child_env(
        cmd: &mut CommandBuilder,
        effective_claude_config_dir: Option<&str>,
        caller_pinned_config_dir: bool,
    ) {
        if !caller_pinned_config_dir {
            if let Some(config_dir) = effective_claude_config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", config_dir);
            }
        }

        crate::terminal::scrub_credential_env_pty(cmd);
    }

    /// Build the PTY child [`CommandBuilder`] from an optional program+args
    /// override (Decision 3).
    ///
    /// - `Some([program, args…])` → run `program` directly with `args` as the
    ///   session's child. The gate-continuation terminal branch uses this to
    ///   launch `claude "<prompt>"` so the prompt is visible in scrollback and
    ///   the session behaves identically to the operator launching it (no
    ///   PTY-readiness race, no shell wrapping).
    /// - `Some([])` (empty) → no program to run; fall back to the shell so we
    ///   never spawn an empty command.
    /// - `None` → [`Self::build_shell_command`], the interactive-shell path
    ///   every operator-opened terminal takes. Back-compat by construction.
    fn build_command_from(command: Option<Vec<String>>) -> CommandBuilder {
        match command {
            Some(parts) if !parts.is_empty() => {
                let mut cmd = CommandBuilder::new(&parts[0]);
                for arg in &parts[1..] {
                    cmd.arg(arg);
                }
                cmd
            }
            _ => Self::build_shell_command(),
        }
    }

    /// Build the platform-appropriate shell command, injecting shell integration if possible.
    fn build_shell_command() -> CommandBuilder {
        #[cfg(target_os = "windows")]
        {
            let mut cmd = CommandBuilder::new("powershell.exe");
            // Try to write and source the integration script. Fall back to plain -NoExit on failure.
            if let Some(script_path) =
                write_integration_script(PS1_INTEGRATION, "qontinui-shell-integration.ps1")
            {
                // -Command mode: dot-source the script then keep shell alive.
                // The script itself sources $PROFILE and sets up OSC 633 hooks.
                let source_cmd = format!(". '{}'", script_path.display());
                cmd.args(["-NoLogo", "-NoExit", "-Command", source_cmd.as_str()]);
            } else {
                cmd.args(["-NoLogo", "-NoExit"]);
            }
            cmd
        }
        #[cfg(not(target_os = "windows"))]
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            let shell_name = std::path::Path::new(&shell)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let mut cmd = CommandBuilder::new(&shell);
            // Shell integration is shell-specific. `--rcfile` is a BASH flag —
            // passing it to zsh (the macOS default) fails with
            // "no such option: rcfile", so each shell needs its own mechanism.
            match shell_name {
                "zsh" => {
                    // zsh: point ZDOTDIR at a dir whose `.zshrc` is our
                    // integration (it re-sources the user's ~/.z* files). If we
                    // can't write it, fall through to a plain zsh that sources
                    // ~/.zshrc normally (no OSC 633, but a working terminal).
                    if let Some(zdotdir) = write_zsh_zdotdir(ZSH_INTEGRATION) {
                        cmd.env("ZDOTDIR", zdotdir.to_string_lossy().as_ref());
                    }
                }
                // bash (and the empty/unknown-path default) keep the --rcfile
                // path, with --login as the no-script fallback.
                "bash" | "" => {
                    if let Some(script_path) = write_integration_script(
                        BASH_INTEGRATION,
                        "qontinui-shell-integration.bash",
                    ) {
                        cmd.arg("--rcfile");
                        cmd.arg(script_path.to_string_lossy().as_ref());
                    } else {
                        cmd.arg("--login");
                    }
                }
                // Any other shell (fish, etc.): launch it plainly — never pass
                // bash-only flags it would reject.
                _ => {}
            }
            cmd
        }
    }

    /// The canonical PATH env-var key for the PTY child process. Windows env is
    /// case-insensitive with undefined duplicate-key resolution, so a shim
    /// prepended under a different casing than the inherited key can be shadowed
    /// by the original. We therefore always set the SAME casing the OS itself
    /// uses (`Path` on Windows, `PATH` elsewhere) and remove every inherited
    /// case-variant first (see [`Self::set_child_path`]).
    const PATH_ENV_KEY: &'static str = if cfg!(windows) { "Path" } else { "PATH" };

    /// Set the child's path env var to `value` under the single canonical key,
    /// removing every case-variant first so the child has EXACTLY ONE path key.
    ///
    /// Why this matters on Windows: the runner's own env var is `Path`;
    /// `portable_pty`'s `CommandBuilder` seeds the child from the parent env and
    /// a naive `cmd.env("PATH", …)` used to be able to leave the child with both
    /// an inherited `Path` (no shim) and an added `PATH` (shimmed), letting the
    /// loader resolve tools via the original `Path` and bypass the identity
    /// shim. Removing all three variants then setting the OS-preferred casing
    /// guarantees the shim-prepended value is the one Windows actually reads.
    ///
    /// (portable_pty 0.8 folds env keys case-insensitively on Windows, so this
    /// is belt-and-suspenders there; the removal is a harmless no-op when no
    /// variant is present, and the fix is correct regardless of the library's
    /// internal folding.)
    fn set_child_path(cmd: &mut CommandBuilder, value: &str) {
        cmd.env_remove("PATH");
        cmd.env_remove("Path");
        cmd.env_remove("path");
        cmd.env(Self::PATH_ENV_KEY, value);
    }

    /// Install `identity_dir` at the head of the child's path and tell the shim
    /// its own dir, under the single canonical key (see [`Self::set_child_path`]).
    ///
    /// The `identity_dir` is supplied rather than resolved here because the two
    /// callers get it differently and only one of them may write:
    /// [`Self::apply_identity_seam`] passes the dir
    /// `shim_materializer::materialize_identity` just brought into existence,
    /// and `config_report`'s G3 passes
    /// `shim_materializer::identity_dir_if_materialized`'s pure `stat`. The
    /// env mutation is one definition either way, so the report cannot carry a
    /// drifting second copy of what the PATH seam does — which matters more
    /// here than anywhere else in the seam, since `PATH` is the variable that
    /// decides which binary a child resolves.
    pub(crate) fn apply_identity_path_shim(
        cmd: &mut CommandBuilder,
        identity_dir: &std::path::Path,
    ) {
        use crate::install_effects_producer::intercept::shim_materializer;

        let current_path = std::env::var("PATH").ok();
        let new_path = shim_materializer::prepend_path(identity_dir, current_path.as_deref());
        Self::set_child_path(cmd, &new_path);
        // The identity shim reads this to skip its own dir in the
        // real-tool scan (reusing the install shim's env contract).
        cmd.env(
            "QONTINUI_INSTALL_INTERCEPT_SHIM_DIR",
            identity_dir.to_string_lossy().as_ref(),
        );
    }

    /// Extract the explicit session id a built PTY child command's argv NAMES,
    /// if any. Recognizes every flag that carries a CONCRETE id:
    ///
    /// - `--session-id <id>` / `--session-id=<id>`
    /// - `--resume <id>`     / `--resume=<id>`
    /// - `-r <id>`           (claude's short `--resume`; head-gated, below)
    ///
    /// Used by the identity seam to adopt the id a runner-launched
    /// direct-command spawn actually carries as its authoritative pin — so the
    /// recorded id equals the id the session runs under (no phantom). Returns
    /// `None` for the interactive-shell path, whose argv is the shell program
    /// and names no id.
    ///
    /// The resume forms are NOT optional extras: the account-migration respawn
    /// ([`crate::terminal::account_migration`]) composes its argv through
    /// [`crate::claude_session::launch_spec::render_argv`], which renders
    /// `--resume <id>` and DROPS `--session-id`. Without them this function
    /// returned `None` for that whole class and the seam minted a fresh uuid no
    /// process ever ran under — an authoritative row unconfirmable by
    /// construction.
    ///
    /// `--continue` / `-c` name NO id (the CLI picks the most recent session on
    /// its own), so they deliberately do not match: there is nothing to adopt,
    /// and inventing an id would recreate the very phantom this exists to
    /// prevent. Mint-a-fresh-uuid stays the correct fallback there.
    ///
    /// PRECEDENCE — when argv somehow carries BOTH a session-id and a resume
    /// flag, the RESUME id wins, because that is the id the launched process
    /// will actually run under. This mirrors `launch_spec::render_argv` step 3
    /// ("Session — … resume takes precedence over session-id",
    /// `claude_session/launch_spec.rs:225-235`), which emits `--resume` and
    /// omits `--session-id` when both are specified. Getting this backwards
    /// would re-introduce the phantom for that argv shape.
    fn explicit_session_id_from(cmd: &CommandBuilder) -> Option<String> {
        let argv = cmd.get_argv();
        // The ambiguous SHORT form only when the child IS claude — see
        // `argv_head_is_claude`.
        let allow_short_r = Self::argv_head_is_claude(argv);

        let mut session_id: Option<String> = None;
        let mut resume_id: Option<String> = None;

        let mut it = argv.iter().peekable();
        while let Some(arg) = it.next() {
            let s = arg.to_string_lossy();
            // `(is_resume, id)` for the flags that NAME an id; everything else
            // (including `--continue` / `-c`) falls through unmatched.
            let matched = if let Some(rest) = s.strip_prefix("--session-id") {
                Self::flag_id_value(rest, &mut it).map(|v| (false, v))
            } else if let Some(rest) = s.strip_prefix("--resume") {
                Self::flag_id_value(rest, &mut it).map(|v| (true, v))
            } else if s.as_ref() == "-r" && allow_short_r {
                Self::flag_id_value("", &mut it).map(|v| (true, v))
            } else {
                None
            };
            if let Some((is_resume, id)) = matched {
                let slot = if is_resume {
                    &mut resume_id
                } else {
                    &mut session_id
                };
                if slot.is_none() {
                    *slot = Some(id); // first occurrence of each class wins
                }
            }
        }

        // resume beats session-id (launch_spec.rs:225-235).
        resume_id.or(session_id)
    }

    /// The id a `--flag`-prefixed argv token names: the attached `=<id>` form,
    /// else the NEXT argv token. `None` when the token was only a glued prefix
    /// match (`--session-idX`, `--resumeX`), when no value follows, or when the
    /// following token does not look like an id.
    ///
    /// The next token is PEEKED and consumed only on acceptance, so a rejected
    /// lookahead (`--resume --session-id <id>`) stays visible to the scan.
    fn flag_id_value<'a, I>(rest: &str, it: &mut std::iter::Peekable<I>) -> Option<String>
    where
        I: Iterator<Item = &'a std::ffi::OsString>,
    {
        if let Some(attached) = rest.strip_prefix('=') {
            return Self::accept_id_value(attached);
        }
        if rest.is_empty() {
            let id = Self::accept_id_value(it.peek()?.to_string_lossy().as_ref())?;
            it.next(); // consume only once accepted
            return Some(id);
        }
        None // glued prefix match — not this flag
    }

    /// Accept a captured token as an explicit session id: non-empty, and not
    /// itself a flag. The flag guard is what keeps `--resume --verbose` from
    /// capturing `--verbose`, and `--resume` as the final arg from capturing
    /// nothing.
    ///
    /// Deliberately NOT a strict UUID gate: this argv is BUILT BY the runner's
    /// own direct-command callers, whose ids are passed through verbatim, and
    /// the pin must equal whatever the child actually runs under — uuid-shaped
    /// or not. (The UUID gate belongs on the untrusted side, where
    /// `process_capture::process_tree::parse_session_id_from_cmdline` scrapes a
    /// foreign process's command line.)
    fn accept_id_value(raw: &str) -> Option<String> {
        let t = raw.trim();
        if t.is_empty() || t.starts_with('-') {
            return None;
        }
        Some(t.to_string())
    }

    /// True when argv's head names the claude CLI image (basename compare,
    /// tolerant of a path and a `.exe`/`.cmd`/`.bat` suffix).
    ///
    /// Gates the SHORT `-r` form only. `--session-id` / `--resume` are
    /// unambiguous long flags, but `-r` is an extremely common unrelated short
    /// flag (`cp -r`, `grep -r`, `ls -r`) and this seam builds a command for
    /// ARBITRARY direct spawns — an ungated `-r` could pin a path as a session
    /// id. This mirrors the identity shim, which reaches its own
    /// `--session-id | --resume | -r | …` equivalence class
    /// (`bin/qontinui_shim.rs::user_chose_session`) only after
    /// `detect_identity_tool` has confirmed argv0 is claude/gemini.
    fn argv_head_is_claude(argv: &[std::ffi::OsString]) -> bool {
        let Some(head) = argv.first() else {
            return false;
        };
        let lower = head.to_string_lossy().to_ascii_lowercase();
        let base = lower.rsplit(['/', '\\']).next().unwrap_or(lower.as_str());
        let stem = base
            .strip_suffix(".exe")
            .or_else(|| base.strip_suffix(".cmd"))
            .or_else(|| base.strip_suffix(".bat"))
            .unwrap_or(base);
        stem == "claude"
    }

    /// Install-interception env-seam (plan §4 Phase 1). Behind the master flag
    /// `QONTINUI_INSTALL_INTERCEPT_ENABLED` (default OFF). When ON, materialize
    /// the per-terminal PATH-shim bin dir and mutate the child `cmd`:
    /// prepend the shim dir to `PATH`, set `QONTINUI_INSTALL_INTERCEPT_PORT`
    /// (the BOUND runner port) + `QONTINUI_INSTALL_INTERCEPT_MODE` (the
    /// runner-env-resolved `observe`/`gate` — plan §3 step 3; default observe).
    ///
    /// Fail-open: a materialize failure injects nothing (the terminal still
    /// spawns, un-shimmed). When OFF, this is a pure no-op — the child env is
    /// byte-identical to pre-interception.
    ///
    /// The prepended value is written under the OS-canonical path key via
    /// [`Self::set_child_path`], which first removes every inherited case-variant
    /// (`PATH`/`Path`/`path`) — on Windows the runner env var is `Path`, so
    /// setting a bare `"PATH"` could leave the original `Path` (un-shimmed) to
    /// win the loader's case-insensitive resolution.
    fn apply_install_intercept_env(cmd: &mut CommandBuilder, terminal_id: &str) {
        use crate::install_effects_producer::intercept::shim_materializer;

        if !shim_materializer::intercept_enabled() {
            return; // ships dark — byte-identical child env
        }
        let base_dir = std::env::temp_dir();
        let port = crate::install_effects_producer::intercept::bound_port();
        // The MODE the terminal gets is resolved from the runner's OWN env
        // (`QONTINUI_INSTALL_INTERCEPT_MODE`; default observe, `gate` enables
        // Phase-3 gating, anything else fails open to observe — plan §3 step 3).
        let mode = shim_materializer::resolve_mode();
        let seam = match shim_materializer::materialize(&base_dir, terminal_id, port, mode) {
            Some(s) => s,
            None => return, // fail-open: couldn't write shims ⇒ inject nothing
        };

        let current_path = std::env::var("PATH").ok();
        let new_path = shim_materializer::prepend_path(&seam.shim_dir, current_path.as_deref());
        Self::set_child_path(cmd, &new_path);
        cmd.env(shim_materializer::PORT_ENV, seam.port.to_string());
        cmd.env(shim_materializer::MODE_ENV, &seam.mode);
        // Tell the compiled `.exe` stub its own dir so its real-tool PATH scan
        // can skip it even if `current_exe()` is unavailable (plan §6 / §4
        // Phase 4 — the `qontinui-shim` stub reads this).
        cmd.env(
            "QONTINUI_INSTALL_INTERCEPT_SHIM_DIR",
            seam.shim_dir.to_string_lossy().as_ref(),
        );
        info!(
            terminal_id = %terminal_id,
            shim_dir = %seam.shim_dir.display(),
            port = seam.port,
            mode = %seam.mode,
            "install-intercept: shim dir injected onto terminal PATH"
        );
    }

    /// Always-on session-restore identity seam (plan §3b). NOT gated by any
    /// flag — the out-of-box guarantee. For EVERY terminal:
    ///
    /// 1. Generate a per-terminal session UUID (`QONTINUI_PINNED_SESSION_ID`).
    /// 2. Inject it + `QONTINUI_TERMINAL_ID` + the bound loopback port into the
    ///    child env (the identity shim reads them to pin + confirm).
    /// 3. Materialize the always-on `claude`/`gemini` identity shims and prepend
    ///    their dir to `PATH` so a hand-started provider is pinned to the id.
    /// 4. Record the session AUTHORITATIVELY at spawn via
    ///    [`crate::commands::terminal::record_pinned_session_open`] — identity is
    ///    fixed with zero round-trip; the hook POST is confirmation/liveness
    ///    only (§3b determinism mechanism).
    ///
    /// Fail-open at every step: a materialize failure injects nothing (the
    /// terminal still spawns un-shimmed); a missing lifecycle store skips the
    /// record (the confirming hook still records via `/control/session-open`).
    #[allow(clippy::too_many_arguments)]
    fn apply_identity_seam(
        cmd: &mut CommandBuilder,
        terminal_id: &str,
        app_handle: &AppHandle,
        cwd: &str,
        title: &str,
        page_id: &str,
        // The EFFECTIVE Claude config dir the caller resolved for this PTY child
        // (caller pin, else the resolved account). Stamped onto the spawn-time
        // authoritative record so an autonomous boot-resume runs under the
        // CORRECT account. `None` = default account (no CLAUDE_CONFIG_DIR).
        config_dir: Option<String>,
    ) {
        use crate::install_effects_producer::intercept::shim_materializer;
        use tauri::Manager;

        // 1. Pinned session id — the runner KNOWS it up front.
        //
        // If the PTY child command's argv NAMES a session id — `--session-id
        // <id>` (the gate-continuation / runner-launched direct-command path
        // builds `[claude, --session-id, <id>, …]`) or `--resume <id>` / `-r
        // <id>` (the account-migration respawn path builds `[claude, …,
        // --resume, <id>]` and carries NO `--session-id`) — ADOPT that id as the
        // authoritative pin instead of minting a fresh one. Otherwise the seam
        // would record a fresh uuid the session never runs under (the identity
        // shim's don't-double-pin passes the explicit id straight through), and
        // the caller's own capture-hint record would carry the REAL id — a
        // two-record split where the seam's row is a phantom. Recording the id
        // the command actually carries makes recorded id == run id == the id
        // that gets the SessionStart confirmation hook (plan Phase 2). When no
        // explicit id is present (the interactive-shell path), generate one.
        let pinned =
            Self::explicit_session_id_from(cmd).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // 2. Identity env — ALWAYS injected (zero-setup capture).
        cmd.env(shim_materializer::TERMINAL_ID_ENV, terminal_id);
        cmd.env(shim_materializer::PINNED_SESSION_ID_ENV, &pinned);
        // The identity shim's confirmation POST targets the runner loopback on
        // this port. Inject it even when install-interception is OFF so the
        // confirmation works out of the box (the var is shared but harmless when
        // install interception is dark — no install shims are on PATH then).
        let port = crate::install_effects_producer::intercept::bound_port();
        cmd.env(shim_materializer::PORT_ENV, port.to_string());

        // ---- Claude SessionStart hook delivery (plan §4, Phase 2) -----------
        // Materialize the bundled `--settings` hook into the runner's OWN
        // app-data dir (~/.qontinui/runner/session-restore/) — NEVER ~/.claude —
        // and inject its absolute path. The identity shim's `claude` wrapper
        // appends `--settings $that` (additive: Claude merges the hook on top of
        // any ~/.claude config WITHOUT writing to it). The SessionStart hook then
        // POSTs a confirmation/liveness signal to /control/session-open on
        // startup AND --resume. Fail-open: a materialize failure injects nothing
        // (identity still rides the spawn-time --session-id pin; only the
        // confirmation hook is absent).
        let hook_dir = crate::session::claude_hook::session_restore_dir();
        // Phase 0 instrumentation: 4 hook files + 3 `set_executable` calls per
        // spawn, with no content/mtime skip.
        let hook_span =
            tracing::debug_span!("terminal_spawn.claude_hook_materialize", terminal_id = %terminal_id)
                .entered();
        let hook_result = crate::session::claude_hook::materialize(&hook_dir);
        drop(hook_span);
        match hook_result {
            Some(settings_path) => {
                cmd.env(
                    crate::session::claude_hook::CLAUDE_SETTINGS_ENV,
                    settings_path.to_string_lossy().as_ref(),
                );
            }
            None => {
                // Hook delivery off for this terminal — identity is still pinned
                // + recorded at spawn below; only the confirmation hook is absent.
            }
        }

        // ---- Universal coord-mcp delivery (mcp-config-universal-provisioning) --
        // Give EVERY device-scope session coord-mcp with zero setup — a plain
        // hand-typed `claude` in an arbitrary cwd, a restore-re-spawned session, a
        // fresh install — by materializing a runner-owned DEVICE `--mcp-config`
        // file (app-data, never the cwd) and exporting its path as
        // QONTINUI_MCP_CONFIG. The identity shim appends `--mcp-config $that` for
        // claude, exactly parallel to the `--settings` hook above.
        //
        // SKIP when the cwd already declares a coord-mcp server in its own
        // `.mcp.json`: the operator's repo-root config, or a gate-continuation
        // terminal whose device `.mcp.json` was written by
        // `provision_coord_mcp_for_session` BEFORE this spawn. Re-injecting would
        // give the session two coord-mcp entries racing the per-workdir nonce, and
        // the loser 401s / shows FAILED — so let the existing file own it.
        //
        // DEVICE scope only + fail-open/fail-closed: this seam runs solely for
        // interactive + continuation terminals (headless agent subprocesses use a
        // separate direct-spawn path), so it can never elevate an agent session;
        // and `provision_coord_mcp_config_file` returns None on an unresolvable
        // bound port — then we inject nothing (no broken `--mcp-config`, no cwd
        // breadcrumb pollution), mirroring the `--settings` fail-open.
        //
        // THIS terminal's id is passed through: it keys both the app-data
        // filename and the minted nonce binding, so two terminals sharing one
        // cwd get two config files and two live nonces instead of racing a
        // single per-workdir one. That is what makes coord-caller
        // self-identification resolvable — the proxy maps `nonce → terminal_id
        // → the open lifecycle record → claude_session_id`, all 1:1, where the
        // workdir leg is 1:N and could only ever guess.
        {
            // Phase 0 instrumentation: `.mcp.json` read+parse and, on the
            // provisioning branch, a nonce registration that re-encrypts the
            // WHOLE secure-storage token store (B2).
            let _span =
                tracing::debug_span!("terminal_spawn.coord_mcp_provision", terminal_id = %terminal_id)
                    .entered();
            if crate::coord_mcp::workdir_declares_coord_mcp(cwd) {
                info!(
                    terminal_id = %terminal_id,
                    "coord-mcp: cwd already declares coord-mcp — skipping --mcp-config injection"
                );
            } else if let Some(cfg_path) =
                crate::coord_mcp::provision_coord_mcp_config_file(cwd, Some(terminal_id))
            {
                cmd.env(
                    crate::coord_mcp::MCP_CONFIG_ENV,
                    cfg_path.to_string_lossy().as_ref(),
                );
                info!(
                    terminal_id = %terminal_id,
                    path = %cfg_path.display(),
                    "coord-mcp: QONTINUI_MCP_CONFIG injected for universal --mcp-config delivery"
                );
            }
        }

        // 3. Materialize the always-on identity shims + prepend their dir.
        // Phase 6 (B2): the dir is CONTENT-ADDRESSED per runner build and
        // shared by every terminal, so this costs 4 rendered scripts + 2 exe
        // copies + a hardlink ONCE per build instead of once per spawn. See
        // `shim_materializer::identity_build_tag` for the staleness check that
        // replaces the old "every spawn re-copies, so it is accidentally
        // fresh" behavior.
        let base_dir = std::env::temp_dir();
        let shim_span =
            tracing::debug_span!("terminal_spawn.shim_materialize", terminal_id = %terminal_id)
                .entered();
        let identity = shim_materializer::materialize_identity(&base_dir);
        drop(shim_span);
        match identity {
            Some(identity_dir) => {
                Self::apply_identity_path_shim(cmd, &identity_dir);
                info!(
                    terminal_id = %terminal_id,
                    identity_dir = %identity_dir.display(),
                    "session-restore: always-on identity shim dir injected onto terminal PATH"
                );
            }
            None => {
                // Fail-open: env vars are still set so a directly-invoked
                // provider could still be pinned by a future absolute-path
                // wrapper, but no PATH shim this terminal.
            }
        }

        // 4. Record AUTHORITATIVELY at spawn (zero transcript race). The
        // lifecycle store is shared in Tauri state by main.rs; absent in test
        // fixtures / a boot window — then the confirming hook records instead.
        if let Some(store) = app_handle
            .try_state::<std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        {
            // Phase 0 instrumentation: B1 — a full-map clone + whole-file JSON
            // rewrite + a full-registry snapshot-history line, i.e. the O(N)
            // term in spawn latency.
            let _span =
                tracing::debug_span!("terminal_spawn.record_open", terminal_id = %terminal_id)
                    .entered();
            crate::commands::terminal::record_pinned_session_open(
                store.inner(),
                pinned.clone(),
                terminal_id.to_string(),
                // The effective account dir placed into the PTY env — stamped
                // authoritatively at spawn so restore is account-correct without
                // waiting on the hook echo. The store normalizes empty→None.
                config_dir,
                cwd.to_string(),
                title.to_string(),
                page_id.to_string(),
                0,
                crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
            );
            info!(
                terminal_id = %terminal_id,
                session_id = %pinned,
                "session-restore: session recorded authoritatively at spawn"
            );
        }
    }

    /// Assign a process to the Windows Job Object for crash safety.
    #[cfg(target_os = "windows")]
    fn assign_to_job_object(pid: u32) {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

        unsafe {
            let handle = OpenProcess(PROCESS_ALL_ACCESS, 0, pid);
            if !handle.is_null() && !std::ptr::eq(handle, INVALID_HANDLE_VALUE as *mut _) {
                crate::job_object::assign_process_to_job(handle as _);
                CloseHandle(handle as _);
            }
        }
    }

    /// Write data (keystrokes) to the PTY stdin.
    ///
    /// This is the SINGLE funnel for terminal input — every write surface
    /// (Tauri `terminal_write`, HTTP `write_terminal_handler`, the WS input
    /// loop, transports, backend relay) routes through here, so the
    /// typed-input observer ([`Self::observe_input`]) covers every present
    /// and future write path. Observation happens AFTER the bytes hit the
    /// PTY and the writer lock is released — it never delays keystrokes.
    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        {
            let mut writer = self
                .writer
                .lock()
                .map_err(|e| format!("Writer lock poisoned: {}", e))?;
            writer
                .write_all(data)
                .map_err(|e| format!("Failed to write to PTY: {}", e))?;
            writer
                .flush()
                .map_err(|e| format!("Failed to flush PTY: {}", e))?;
        } // writer lock released before observation

        self.observe_input(data);
        Ok(())
    }

    /// Observe raw typed input bytes — feed them through the per-terminal
    /// input-line buffer ([`consume_input_bytes`]) and dispatch each
    /// completed (CR/LF-submitted) line to the typed-input consumers:
    ///
    /// 1. **L3 git-warn**: a branch-mutating git line kicks off a
    ///    best-effort coord lookup that emits a soft
    ///    `terminal-coord-warning` event when a PEER holds this repo's
    ///    worktree claim.
    /// 2. **Typed claude resume sniff** (#548 Phase 2): a
    ///    `claude … --resume <id>` / `--session-id <id>` line registers the
    ///    session in the durable lifecycle store and — for the bypass form —
    ///    emits the `terminal-bypass-permissions` event, mirroring the
    ///    spawn-argv sniff in `TerminalManager::create`.
    ///
    /// SOFT + CHEAP: never blocks, never affects the PTY write. Called from
    /// [`Self::write`] AFTER the bytes are written; all effects run on
    /// detached async tasks.
    fn observe_input(&self, data: &[u8]) {
        // Collect any completed lines this chunk produced. We hold the
        // buffer lock only for the cheap byte-walk, then release before
        // spawning the async effect tasks.
        let mut completed: Vec<String> = Vec::new();
        if let Ok(mut buf) = self.input_line_buf.lock() {
            completed = consume_input_bytes(&mut buf, data);
        }
        if completed.is_empty() {
            return;
        }

        let Some(app_handle) = self.app_handle.clone() else {
            return;
        };
        for line in completed {
            if let Some(parsed) = super::claude_resume_sniff::parse_typed_claude_resume(&line) {
                let title = self
                    .title
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_else(|e| e.into_inner().clone());
                super::claude_resume_sniff::spawn_register_typed_resume(
                    app_handle.clone(),
                    self.id.clone(),
                    self.working_dir.clone(),
                    self.page_id(),
                    title,
                    parsed,
                );
            }
            if super::coord_warn::is_branch_mutating_git(&line) {
                super::coord_warn::spawn_check_and_warn(
                    app_handle.clone(),
                    self.id.clone(),
                    self.working_dir.clone(),
                    self.coord_session_id(),
                    line,
                );
            }
        }
    }

    /// Submit `message` to a Claude-style interactive prompt running in
    /// the PTY. The bytes are wrapped in ANSI bracketed-paste markers
    /// (`\x1b[200~` ... `\x1b[201~`) so the TUI treats the content as a
    /// single paste block, then a bare `\r` is written **outside** the
    /// paste window so the prompt actually submits.
    ///
    /// CR-LF (`\r\n`) is intentionally NOT used: bracketed-paste TUIs
    /// like Claude Code consume the `\n` as the closing newline of the
    /// paste rather than as the submit keystroke. Sending only `\r`
    /// after the end marker reliably triggers send. See Phase 6 §6
    /// remediation plan, Issue 1.
    pub fn submit_prompt(&self, message: &str) -> Result<(), String> {
        // Frame + neutralize BEFORE taking the writer lock — the body is
        // untrusted (the `POST /terminals/{id}/submit-prompt` route is
        // caller-supplied) and an embedded `\x1b[201~` would otherwise
        // close the paste block early, turning the remainder into
        // terminal INPUT. See [`sanitize_submit_body`].
        let block = paste_block(message);

        // Phase 1: write the bracketed-paste block, flush, release the
        // writer lock. The lock release lets concurrent reads on this
        // pty (output drain) interleave during the post-paste delay.
        {
            let mut writer = self
                .writer
                .lock()
                .map_err(|e| format!("Writer lock poisoned: {}", e))?;
            writer
                .write_all(&block)
                .map_err(|e| format!("Failed to write paste block: {}", e))?;
            writer
                .flush()
                .map_err(|e| format!("Failed to flush PTY: {}", e))?;
        } // writer lock released

        // Sleep so Claude Code's readline can fully process the paste
        // sequence BEFORE the submit byte arrives. Without this, the
        // bracketed-paste handler consumes the trailing CR as paste-tail
        // and never submits — see [`POST_PASTE_DELAY`] doc for the §6 E2E
        // reproduction. Sync sleep is acceptable here; callers
        // (`coordinator/act.rs::send_message_to_worker`) tolerate ~150ms
        // blocking on the multi-threaded tokio runtime. Move to
        // `tokio::task::spawn_blocking` if this ever goes hot-path.
        std::thread::sleep(POST_PASTE_DELAY);

        // Phase 2: bare CR (Enter) as a separate read cycle. Re-acquires
        // the writer lock; if the worker pty has been closed in the
        // meantime, this Err propagates cleanly.
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| format!("Writer lock poisoned: {}", e))?;
        writer
            .write_all(SUBMIT_ENTER)
            .map_err(|e| format!("Failed to write submit enter: {}", e))?;
        writer
            .flush()
            .map_err(|e| format!("Failed to flush PTY: {}", e))?;
        Ok(())
    }

    /// Resize the PTY dimensions.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let master = self
            .master
            .lock()
            .map_err(|e| format!("Master lock poisoned: {}", e))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to resize PTY: {}", e))?;
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        if let Ok(mut g) = self.grid.lock() {
            g.resize(cols, rows);
        }
        // A resize rewrites rendered text (rows are truncated/padded) without a
        // single byte reaching the parser, so the byte counter does not move —
        // bump the grid generation so the scanners re-read the new screen.
        // AFTER the lock is released, for the same ordering reason as
        // `advance_grid`.
        self.grid_generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Acknowledge bytes received by the frontend (flow control).
    ///
    /// Resume-marker: when webview emission was skipped while the gate was
    /// paused (`emission_skipped`) and this ack brings the gap back under
    /// the LOW watermark, a burst that already ENDED would otherwise leave
    /// the pane stale forever — the gate has reopened but no further chunk
    /// arrives to reveal the gap. Emit a zero-length `terminal-output`
    /// marker stamped at the current produced offset; the frontend's
    /// offset-gap detection treats it like any other jump and resyncs from
    /// the scrollback ring.
    pub fn ack(&self, bytes: u64) {
        self.bytes_acked.fetch_add(bytes, Ordering::Relaxed);
        let sent = self.bytes_sent.load(Ordering::Relaxed);
        let acked = self.bytes_acked.load(Ordering::Relaxed);
        if sent.saturating_sub(acked) <= FLOW_LOW_WATERMARK {
            self.emit_resume_marker_if_skipped();
        }
    }

    /// Emit the zero-length resume marker if the webview is known to be missing
    /// bytes, clearing the flag.
    ///
    /// Two callers raise the flag: the flow-control gate pausing a `focused`
    /// session, and the `unwatched` tier declining to emit at all. Both recover
    /// the same way — the marker's offset jumps past what the frontend last
    /// saw, its gap detection fires, and it replays the scrollback ring.
    fn emit_resume_marker_if_skipped(&self) {
        if !self.emission_skipped.swap(false, Ordering::Relaxed) {
            return;
        }
        let Some(app) = &self.app_handle else {
            return;
        };
        emit_terminal_output(
            app,
            &self.id,
            "",
            self.total_bytes_produced.load(Ordering::Relaxed),
        );
    }

    // ---- Phase 5: visibility-tiered webview emission (A4 backend half) ----

    /// This session's merged visibility tier. Lock-free.
    pub fn visibility_tier(&self) -> VisibilityTier {
        self.visibility.tier()
    }

    /// Record one window's view of this session (the `terminal_set_visibility`
    /// command) and settle any work the resulting tier change implies.
    pub fn set_visibility(&self, window: &str, tier: VisibilityTier) {
        let before = self.visibility.tier();
        let after = self.visibility.report(window, tier);
        self.on_tier_changed(before, after);
    }

    /// Drop reports from windows that no longer exist (driven by the sweeper on
    /// the window-membership edge). A closed pop-out that last said `focused`
    /// would otherwise pin its terminals at full rate forever.
    pub fn retain_visibility_windows(&self, live: &BTreeSet<String>) {
        let before = self.visibility.tier();
        let after = self.visibility.retain_windows(live);
        self.on_tier_changed(before, after);
    }

    /// Settle the two things a tier change owes the webview.
    fn on_tier_changed(&self, before: VisibilityTier, after: VisibilityTier) {
        if before == after {
            return;
        }
        // Leaving a tier that holds, with bytes still held: ship them now, so
        // nothing emitted under the new tier can overtake them.
        //
        // Unconditional rather than `before == Background`, because
        // `unwatched` holds too once the operator configures a cadence for it.
        // Making it conditional on the *previous* tier's holding-ness would
        // read the setting a second time to decide, for no gain: the hold is
        // empty on the `focused` edge anyway (that tier drains it before every
        // emit), so `take_now` is a no-op there.
        self.flush_background_window(true);
        // Leaving `unwatched`: the webview may have missed everything produced
        // while the session was dark. Emit the resume marker so the frontend's
        // gap detection pulls a ring replay, and re-baseline the digest so the
        // next dark window reports only what it actually misses.
        //
        // Both are correctly no-ops when the tier was configured to emit:
        // `emission_skipped` was never raised (nothing was `Skip`ped) so no
        // marker goes out, and the digest never fired so its baseline is
        // already current.
        if before == VisibilityTier::Unwatched {
            self.emit_resume_marker_if_skipped();
            if let Ok(mut st) = self.activity_digest.lock() {
                st.rebaseline(
                    Instant::now(),
                    self.total_bytes_produced.load(Ordering::Relaxed),
                );
            }
        }
    }

    /// Flush the held window if its configured spacing has elapsed (or its
    /// byte cap tripped). Driven by the visibility sweeper so the tail of a
    /// burst still lands when the session goes quiet mid-window — the reader
    /// thread is parked in a blocking `read()` at exactly that moment and could
    /// only act on the next byte, which may never come.
    pub fn flush_background_window_if_due(&self) {
        self.flush_background_window(false);
    }

    fn flush_background_window(&self, force: bool) {
        let Some(app) = &self.app_handle else {
            return;
        };
        let mut hold = self
            .background_hold
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let taken = if force {
            hold.take_now(now)
        } else {
            hold.take_if_due(
                now,
                hold_interval(
                    self.visibility.tier(),
                    self.background_flush_interval,
                    self.unwatched_flush_interval,
                ),
            )
        };
        // Emit under the lock: it is this session's webview ordering point.
        if let Some((window, offset)) = taken {
            emit_terminal_output(app, &self.id, &STANDARD.encode(&window), offset);
        }
    }

    /// Emit the `terminal-activity` digest if this session is `unwatched` and
    /// its ≤1 Hz budget allows.
    ///
    /// Only `unwatched` sessions emit one: `focused` and `background` sessions
    /// still deliver `terminal-output`, which is what the page-level tap feeds
    /// state tracking from. Emitting for them too would duplicate that feed and
    /// double-count the activity sparkline.
    ///
    /// An operator who sets `unwatched_flush_interval_ms` puts this session's
    /// `unwatched` tier back on `terminal-output` — so the page tap is feeding
    /// tracking again and the digest falls under exactly the same
    /// double-counting rule as `background`. It stands down for the session's
    /// whole life, matching the cadence resolved at spawn.
    pub fn emit_activity_digest_if_due(&self) {
        if !digest_is_owed(self.visibility.tier(), self.unwatched_flush_interval) {
            return;
        }
        let Some(app) = &self.app_handle else {
            return;
        };
        let total = self.total_bytes_produced.load(Ordering::Relaxed);
        let delta = {
            let mut st = self
                .activity_digest
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match st.take_if_due(Instant::now(), total) {
                Some(delta) => delta,
                None => return,
            }
        };
        // The RENDERED screen tail, not the raw bytes: the grid resolves cursor
        // motion, line rewrites and full-frame TUI redraws the same way xterm
        // does, so a digest-fed tab's `lastOutputLines` match a mounted tab's.
        let lines = self
            .grid
            .lock()
            .map(|g| g.lines())
            .unwrap_or_else(|e| e.into_inner().lines());
        let mut lines: Vec<String> = lines.into_iter().filter(|l| !l.trim().is_empty()).collect();
        if lines.len() > ACTIVITY_DIGEST_LINES {
            lines.drain(..lines.len() - ACTIVITY_DIGEST_LINES);
        }

        let event = TerminalActivityWire {
            terminal_id: &self.id,
            total_bytes_produced: total,
            bytes_delta: delta,
            lines,
        };
        if let Err(e) = app.emit(ACTIVITY_EVENT, &event) {
            warn!(
                terminal_id = %self.id,
                error = %e,
                "Failed to emit terminal activity digest"
            );
        }
    }

    /// Monotonic count of every byte this session's PTY has ever produced.
    ///
    /// Bumped by the reader thread inside `tee_into_scrollback`, i.e. BEFORE
    /// the bytes reach the VT parser — so it is a "bytes accepted" counter, not
    /// a "screen updated" counter. It is reported to the frontend and used for
    /// replay offsets. Do NOT gate grid scanning on it: use
    /// [`Self::grid_generation`], which moves only once the grid has actually
    /// absorbed the bytes (see the `terminal::scan_gate` module docs).
    pub fn total_bytes_produced(&self) -> u64 {
        self.total_bytes_produced.load(Ordering::Relaxed)
    }

    /// Monotonic count of mutations to this session's rendered cell grid.
    ///
    /// LOCK-FREE by construction: a single atomic load, no grid lock. The
    /// periodic full-fleet grid scanners (`auto_response`, `usage_limit`,
    /// `context_watcher`) use it as a change detector so they can skip a
    /// session whose screen cannot have moved since their last pass. Going via
    /// [`Self::info`] instead would take the title lock AND the exit-code lock
    /// per session per tick, which is exactly the contention the skip exists to
    /// avoid.
    ///
    /// The counter is bumped only AFTER the mutation is visible to any
    /// subsequent grid-lock holder ([`advance_grid`], [`Self::resize`]), which
    /// is the property the scan gate needs: a value read here and used as a
    /// watermark can never cover a change the following render will miss. A
    /// stale read is therefore safe in the only direction that matters — it
    /// costs one extra scan on the next tick, never a skipped one.
    ///
    /// Acquire pairs with the Release bumps at the two mutation sites.
    pub fn grid_generation(&self) -> u64 {
        self.grid_generation.load(Ordering::Acquire)
    }

    /// Get terminal info for the frontend.
    pub fn info(&self) -> TerminalInfo {
        // Phase 2: title is now `Arc<Mutex<String>>`. Clone under the lock
        // so the returned snapshot doesn't observe a torn write from a
        // concurrent `set_title`.
        let title = self
            .title
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        TerminalInfo {
            id: self.id.clone(),
            title,
            pid: self.child_pid,
            cols: self.cols.load(Ordering::Relaxed),
            rows: self.rows.load(Ordering::Relaxed),
            working_dir: self.working_dir.clone(),
            is_alive: self.is_alive.load(Ordering::Relaxed),
            exit_code: self.exit_code.lock().ok().and_then(|ec| *ec),
            created_at: self.created_at,
            total_bytes_produced: self.total_bytes_produced.load(Ordering::Relaxed),
            page_id: self.page_id(),
        }
    }

    /// Update the session's display title. Phase 2 of bi-directional
    /// title sync: the frontend's xterm.js `onTitleChange` handler relays
    /// observed OSC 0/2 titles back to the runner via the
    /// `terminal_set_title` Tauri command, which routes through
    /// `TerminalManager::set_title` to here. Subsequent `info()` calls
    /// (and `GET /terminals`) see the new value; other observers get the
    /// `terminal-title-changed` event emitted by the manager.
    pub fn set_title(&self, title: String) {
        match self.title.lock() {
            Ok(mut g) => *g = title,
            // Poisoned: recover and overwrite. Title is a pure-text field
            // with no invariants to preserve, so this is safe.
            Err(e) => *e.into_inner() = title,
        }
    }

    /// Move this session onto a different terminal page. The
    /// `POST /terminals/{id}/move` surface routes through
    /// `TerminalManager::set_page` to here; subsequent `info()` calls (and
    /// `GET /terminals` / `GET /terminal-pages`) see the new value, and the
    /// manager emits a `terminal-page-changed` event so the grid re-mounts
    /// the tab under its new page. Mirrors [`Self::set_title`] exactly.
    pub fn set_page(&self, page_id: String) {
        match self.page_id.lock() {
            Ok(mut g) => *g = page_id,
            // Poisoned: recover and overwrite. page_id is a pure-text field
            // with no invariants to preserve, so this is safe.
            Err(e) => *e.into_inner() = page_id,
        }
    }

    /// Read the session's current page id (see [`Self::set_page`]).
    /// Poison-tolerant: page_id is pure text with no invariants to preserve.
    pub fn page_id(&self) -> String {
        self.page_id
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    /// Take the one-shot receiver that resolves when the reader thread
    /// observes the first OSC 0/2 title from the child process. Returns
    /// `None` if a previous caller already took the receiver — callers
    /// should treat that as "already initialized" and skip the wait.
    /// `Some(rx)` resolves to `Ok(())` when the title lands, or
    /// `Err(oneshot::error::RecvError)` if the session closes before any
    /// OSC 0/2 arrives (caller should fall back to a timeout regardless).
    ///
    /// Used by `spawn_worker_session` (Phase 1) to gate
    /// `Initializing → Ready` on Claude CLI readline visibility — the CLI
    /// emits its OSC 0 title (`"✳ Claude Code"`) ~150–300 ms after
    /// startup, so the rx resolves well before the 8 s fallback timeout
    /// the dispatcher uses.
    pub fn subscribe_first_osc_title(&self) -> Option<oneshot::Receiver<()>> {
        self.first_osc_title_rx
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    /// Get the scrollback buffer contents and the byte offset where the data starts.
    /// Returns `(data, start_offset)` where `start_offset = total_bytes_produced - data.len()`.
    ///
    /// `total` is read while HOLDING the ring lock: `tee_into_scrollback`
    /// bumps the counter inside the same lock, so the pair is mutually
    /// consistent — `start_offset + data.len()` is an exact replay boundary
    /// for offset-stamped `terminal-output` chunks (no chunk is ever half
    /// inside the snapshot).
    pub fn get_scrollback_buffer(&self) -> (Vec<u8>, u64) {
        let (data, total) = match self.scrollback_buffer.lock() {
            Ok(sb) => (
                sb.iter().copied().collect::<Vec<u8>>(),
                self.total_bytes_produced.load(Ordering::Relaxed),
            ),
            Err(_) => (
                Vec::new(),
                self.total_bytes_produced.load(Ordering::Relaxed),
            ),
        };
        let start_offset = total.saturating_sub(data.len() as u64);
        (data, start_offset)
    }

    /// Reset flow control counters so a reconnecting frontend doesn't hit backpressure.
    ///
    /// Over-ack tolerance: render callbacks for writes issued BEFORE this
    /// reset may still ack afterwards, briefly pushing `bytes_acked` above
    /// `bytes_sent`. The gate computes the gap with `saturating_sub`, so the
    /// skew reads as gap 0 — a bounded, one-shot blind margin (at most the
    /// pre-reset in-flight bytes) before backpressure re-engages. This is
    /// intentional; do not "fix" the saturating_sub.
    pub fn reset_flow_control(&self) {
        let sent = self.bytes_sent.load(Ordering::Relaxed);
        self.bytes_acked.store(sent, Ordering::Relaxed);
        // A reset supersedes any pending resume-marker: the resetting
        // consumer refetches the ring, which already covers the skipped
        // bytes.
        self.emission_skipped.store(false, Ordering::Relaxed);
    }

    /// Subscribe to the terminal output broadcast channel.
    /// Returns a receiver that yields base64-encoded output chunks.
    pub fn subscribe_output(&self) -> broadcast::Receiver<String> {
        self.output_tx.subscribe()
    }

    /// Set the coord-native session id after `register_external()` wires
    /// this terminal into the coordinator's session plane.
    pub fn set_coord_session_id(&self, id: uuid::Uuid) {
        if let Ok(mut slot) = self.coord_session_id.lock() {
            *slot = Some(id);
        }
    }

    /// Read the coord-native session id, if one has been wired.
    pub fn coord_session_id(&self) -> Option<uuid::Uuid> {
        self.coord_session_id.lock().ok().and_then(|g| *g)
    }

    /// The directory this terminal's shell was started in — the same value
    /// [`Self::info`] reports, without cloning the whole snapshot. Frozen at
    /// spawn (a `cd` inside the shell does not move it), which is what makes
    /// it the right seed for a continuation session: it names the workspace
    /// the terminal was opened against.
    pub fn working_dir(&self) -> &str {
        &self.working_dir
    }

    /// Read the session's current display title (see [`Self::set_title`]).
    /// Poison-tolerant: title is pure text with no invariants to preserve.
    pub fn title(&self) -> String {
        self.title
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    /// R1 — install the on-exit hook the waiter thread fires the instant
    /// the PTY process exits. `terminal_create` wires this (alongside
    /// [`Self::set_coord_session_id`]) to close the coord session mirror
    /// immediately rather than leaving it for coord's stale→closed watcher
    /// (the runner no longer self-closes abandoned sessions; coord_sync A3).
    /// The callback receives the coord session id and must be idempotent
    /// (it shares the close path with the frontend `terminal_close`
    /// command — `SessionRegistry::close_by_id` is already idempotent).
    pub fn set_on_exit(&self, hook: Box<dyn Fn(uuid::Uuid) + Send + Sync>) {
        if let Ok(mut slot) = self.on_exit.lock() {
            *slot = Some(hook);
        }
    }

    /// Phase 2 of the worktree-isolation plan — park the
    /// `IsolatedEditContext` returned by
    /// `agent_worktree::isolated_edit::acquire` so its heartbeat task
    /// + claim live as long as the terminal session. The slot is
    /// cleared in `close()`, which drops the context and fires
    /// best-effort claim release.
    pub fn set_isolated_edit_ctx(
        &self,
        ctx: crate::agent_worktree::isolated_edit::IsolatedEditContext,
    ) {
        if let Ok(mut slot) = self.isolated_edit_ctx.lock() {
            *slot = Some(ctx);
        }
    }

    /// Clone the per-session grid handle so callers can snapshot or read text.
    pub fn grid(&self) -> Arc<Mutex<Grid>> {
        self.grid.clone()
    }

    /// Check if the shell process is still alive.
    pub fn is_alive(&self) -> bool {
        self.is_alive.load(Ordering::Relaxed)
    }

    /// Kill the shell process and clean up threads.
    pub fn close(&self) {
        info!(terminal_id = %self.id, "Closing terminal session");
        self.is_alive.store(false, Ordering::Relaxed);

        // Phase 2 — drop the isolated edit context first so the
        // claim-release fire-and-forget posts ahead of the PTY teardown
        // (release uses tokio::spawn; running it before we drain threads
        // makes ordering observable in coord audit logs).
        if let Ok(mut slot) = self.isolated_edit_ctx.lock() {
            slot.take();
        }

        // Kill the child process via PID if still alive
        if let Some(pid) = self.child_pid {
            #[cfg(target_os = "windows")]
            {
                let _ = crate::process_helpers::no_window("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
        }

        // Drop the writer to signal EOF on stdin
        if let Ok(mut writer) = self.writer.lock() {
            drop(writer.flush());
        }

        // Drop the master PTY handle — this closes the OS pipe and unblocks the
        // reader thread which may be stuck in a blocking read() call.
        if let Ok(mut master) = self.master.lock() {
            // Replace with a placeholder so the Drop actually runs now.
            // MasterPty is trait-object-boxed, so we swap it out.
            let _dropped = std::mem::replace(&mut *master, create_noop_master());
        }

        // Join threads with a timeout so we never hang the UI
        if let Ok(mut handle) = self.reader_join.lock() {
            if let Some(h) = handle.take() {
                join_with_timeout(h, "reader", &self.id);
            }
        }
        if let Ok(mut handle) = self.waiter_join.lock() {
            if let Some(h) = handle.take() {
                join_with_timeout(h, "waiter", &self.id);
            }
        }

        // Install-interception cleanup (plan §4 Phase 4): remove this terminal's
        // per-terminal shim bin dir so it does not leak after the PTY child is
        // reaped. Best-effort + no-op when interception was never enabled (the
        // dir simply won't exist). The stale-sweep at the next materialize reaps
        // anything a crash left behind.
        //
        // This reaps the INSTALL-INTERCEPT dir only. The always-on IDENTITY dir
        // is shared by every terminal of this runner build (Phase 6, B2), so
        // deleting it here would pull the PATH shims out from under every other
        // live pane; `sweep_stale` owns its lifetime instead.
        crate::install_effects_producer::intercept::shim_materializer::cleanup(
            &std::env::temp_dir(),
            &self.id,
        );

        info!(terminal_id = %self.id, "Terminal session closed");
    }
}

/// Join a thread with a timeout, logging a warning if it doesn't finish in time.
fn join_with_timeout(handle: thread::JoinHandle<()>, name: &str, terminal_id: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_name = name.to_string();
    let tid = terminal_id.to_string();

    // Spawn a helper thread that joins and signals completion
    let _ = thread::Builder::new()
        .name(format!("join-{}-{}", thread_name, tid))
        .spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });

    // Wait up to 2 seconds
    if rx.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
        warn!(
            terminal_id = %terminal_id,
            thread = %name,
            "Thread did not finish within 2s timeout — detaching"
        );
    }
}

/// Create a no-op MasterPty placeholder used when dropping the real master during close.
fn create_noop_master() -> Box<dyn MasterPty + Send> {
    Box::new(NoopMaster)
}

/// Minimal MasterPty that does nothing — used as a swap target during close().
struct NoopMaster;

impl MasterPty for NoopMaster {
    fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
        Ok(())
    }
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(PtySize {
            rows: 0,
            cols: 0,
            pixel_width: 0,
            pixel_height: 0,
        })
    }
    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(std::io::empty()))
    }
    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(std::io::sink()))
    }
    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<i32> {
        None
    }
    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<i32> {
        None
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.is_alive.load(Ordering::Relaxed) {
            self.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =======================================================================
    // PTY spawn seam — production call-site coverage for the credential scrub
    // (plan 2026-08-07-runner-context-visibility-and-session-env-secret-hygiene)
    //
    // These test `TerminalSession::finalize_child_env`, which IS the production
    // env tail of `TerminalSession::spawn` — not a re-implementation of it. If
    // the `scrub_credential_env_pty` call is deleted from that function, these
    // fail.
    // =======================================================================

    /// The seam must remove a credential value that is already present in the
    /// child's env map — the production case, where `CommandBuilder::new` seeds
    /// the map from the process env (and, on Windows, from a fresh HKLM/HKCU
    /// `Environment` read).
    #[test]
    fn pty_finalize_child_env_scrubs_credential_values() {
        let mut cmd = CommandBuilder::new("dummy");
        for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }

        TerminalSession::finalize_child_env(&mut cmd, Some("/tmp/claude-config"), false);

        crate::terminal::assert_credentials_scrubbed_pty(
            &cmd,
            "TerminalSession::finalize_child_env",
        );
        assert_eq!(
            cmd.get_env("CLAUDE_CONFIG_DIR").and_then(|v| v.to_str()),
            Some("/tmp/claude-config"),
            "the resolved account pin must still be applied"
        );
    }

    /// The ordering contract: the scrub runs AFTER the caller's `extra_env`
    /// loop, so a caller that re-supplies a credential name cannot defeat it.
    /// Simulated here by seeding the value the way `extra_env` would, then
    /// running the same tail `spawn` runs.
    #[test]
    fn pty_finalize_child_env_scrub_beats_a_caller_supplied_credential() {
        let mut cmd = CommandBuilder::new("dummy");
        // As if `extra_env` had carried both a legitimate pin and a credential.
        cmd.env("CLAUDE_CONFIG_DIR", "/caller/pinned");
        for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }

        TerminalSession::finalize_child_env(&mut cmd, Some("/resolved/dir"), true);

        crate::terminal::assert_credentials_scrubbed_pty(
            &cmd,
            "TerminalSession::finalize_child_env (caller-pinned)",
        );
        assert_eq!(
            cmd.get_env("CLAUDE_CONFIG_DIR").and_then(|v| v.to_str()),
            Some("/caller/pinned"),
            "a caller pin must not be clobbered by the resolved dir"
        );
    }

    /// In-memory `Write` whose bytes can be inspected after the writes.
    /// Backed by an `Arc<Mutex<Vec<u8>>>` so the test can read it after
    /// the `TerminalSession` fakery has consumed the writer.
    struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut g = self.0.lock().unwrap();
            g.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Construct a minimum-viable `TerminalSession` whose `writer` field
    /// is a `CapturingWriter` and whose other fields are inert. The
    /// reader/waiter threads are NOT spawned — `submit_prompt` only
    /// touches `self.writer`, so this is enough.
    fn make_test_session(buf: Arc<Mutex<Vec<u8>>>) -> TerminalSession {
        let writer: Box<dyn Write + Send> = Box::new(CapturingWriter(buf));
        let (output_tx, _) = broadcast::channel::<String>(1);
        let (osc_title_tx, osc_title_rx) = oneshot::channel::<()>();
        TerminalSession {
            id: "test".to_string(),
            title: Arc::new(Mutex::new("test".to_string())),
            working_dir: ".".to_string(),
            page_id: Arc::new(Mutex::new("default".to_string())),
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(create_noop_master())),
            child_pid: None,
            cols: AtomicU16::new(80),
            rows: AtomicU16::new(24),
            // Mark the session as already-dead so Drop doesn't try to
            // join nonexistent reader/waiter threads.
            is_alive: Arc::new(AtomicBool::new(false)),
            exit_code: Arc::new(Mutex::new(None)),
            reader_join: Mutex::new(None),
            waiter_join: Mutex::new(None),
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bytes_acked: Arc::new(AtomicU64::new(0)),
            emission_skipped: Arc::new(AtomicBool::new(false)),
            visibility: Arc::new(VisibilityState::new()),
            background_hold: Arc::new(Mutex::new(BackgroundHold::new(Instant::now()))),
            background_flush_interval: crate::terminal::visibility::BACKGROUND_FLUSH_INTERVAL,
            unwatched_flush_interval: None,
            activity_digest: Arc::new(Mutex::new(ActivityDigestState::new(Instant::now()))),
            scrollback_buffer: Arc::new(Mutex::new(VecDeque::new())),
            total_bytes_produced: Arc::new(AtomicU64::new(0)),
            grid_generation: Arc::new(AtomicU64::new(0)),
            created_at: 0,
            output_tx,
            grid: Arc::new(Mutex::new(Grid::new(80, 24))),
            first_osc_title_tx: Arc::new(Mutex::new(Some(osc_title_tx))),
            first_osc_title_rx: Arc::new(Mutex::new(Some(osc_title_rx))),
            coord_session_id: Arc::new(Mutex::new(None)),
            on_exit: Arc::new(Mutex::new(None)),
            isolated_edit_ctx: Arc::new(Mutex::new(None)),
            // No real Tauri app in unit fixtures — the input-line warn
            // hook no-ops when this is `None`.
            app_handle: None,
            input_line_buf: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Drive a `SyncFrameCoalescer` across a sequence of `(data, offset,
    /// in_sync)` reads and return the emitted `(payload, offset)` events.
    fn run_coalescer(reads: &[(&[u8], u64, bool)]) -> Vec<(Vec<u8>, u64)> {
        let mut c = SyncFrameCoalescer::new();
        let emitted = Arc::new(Mutex::new(Vec::<(Vec<u8>, u64)>::new()));
        for (data, offset, in_sync) in reads {
            let sink = emitted.clone();
            c.feed(data, *offset, *in_sync, move |p, o| {
                sink.lock().unwrap().push((p.to_vec(), o));
            });
        }
        let sink = emitted.clone();
        c.flush_remaining(move |p, o| sink.lock().unwrap().push((p.to_vec(), o)));
        Arc::try_unwrap(emitted).unwrap().into_inner().unwrap()
    }

    // ---- Phase 5: visibility-tiered webview admission ----

    /// `focused` is today's behavior, byte for byte: the flow-control gate is
    /// the only thing that can hold a chunk back.
    #[test]
    fn focused_tier_defers_entirely_to_the_flow_control_gate() {
        let mut gate = EmissionGate::new();
        assert_eq!(
            admit_to_webview(VisibilityTier::Focused, true, &mut gate, 0, false),
            WebviewAdmission::Now
        );
        // Cross the high watermark → the gate pauses, as before Phase 5.
        assert_eq!(
            admit_to_webview(
                VisibilityTier::Focused,
                true,
                &mut gate,
                FLOW_HIGH_WATERMARK + 1,
                false
            ),
            WebviewAdmission::Skip
        );
        // Hysteresis: still paused between LOW and HIGH.
        assert_eq!(
            admit_to_webview(
                VisibilityTier::Focused,
                true,
                &mut gate,
                FLOW_LOW_WATERMARK + 1,
                false
            ),
            WebviewAdmission::Skip
        );
        assert_eq!(
            admit_to_webview(
                VisibilityTier::Focused,
                true,
                &mut gate,
                FLOW_LOW_WATERMARK,
                false
            ),
            WebviewAdmission::Now
        );
    }

    /// `background` never consults the gate: nothing renders a hidden pane, so
    /// no render-ack can ever arrive and an ack-gated tier would wedge at the
    /// high watermark (the wedge the frontend proxy-ack existed to paper over).
    /// The configured flush-interval hold is the flow control instead.
    #[test]
    fn background_tier_holds_and_never_wedges_on_acks() {
        let mut gate = EmissionGate::new();
        for gap in [0, FLOW_LOW_WATERMARK, FLOW_HIGH_WATERMARK * 100] {
            assert_eq!(
                admit_to_webview(VisibilityTier::Background, true, &mut gate, gap, false),
                WebviewAdmission::Hold,
                "gap {gap} must not change the background decision"
            );
        }
        // The gate was never advanced, so a return to `focused` starts open.
        assert_eq!(
            admit_to_webview(VisibilityTier::Focused, true, &mut gate, 0, false),
            WebviewAdmission::Now
        );
    }

    /// The headline Phase 5 property: an `unwatched` session emits nothing to
    /// the webview no matter how far ahead or behind flow control is.
    #[test]
    fn unwatched_tier_never_emits_to_the_webview() {
        let mut gate = EmissionGate::new();
        for gap in [0, 1, FLOW_LOW_WATERMARK, FLOW_HIGH_WATERMARK * 100] {
            assert_eq!(
                admit_to_webview(VisibilityTier::Unwatched, true, &mut gate, gap, false),
                WebviewAdmission::Skip
            );
        }
    }

    /// The knob D3 wired through: a positive
    /// `unwatched_flush_interval_ms` turns the dark tier into a coalescing
    /// one. `Hold`, never `Now` — an unwatched pane render-acks nothing, so
    /// consulting the gate would wedge it exactly as it would for
    /// `background`.
    #[test]
    fn a_configured_unwatched_interval_holds_instead_of_skipping() {
        let mut gate = EmissionGate::new();
        for gap in [0, 1, FLOW_LOW_WATERMARK, FLOW_HIGH_WATERMARK * 100] {
            assert_eq!(
                admit_to_webview(VisibilityTier::Unwatched, true, &mut gate, gap, true),
                WebviewAdmission::Hold,
                "gap {gap} must not change the configured-unwatched decision"
            );
        }
        // The gate was never advanced, so a return to `focused` starts open —
        // the same property `background` has.
        assert_eq!(
            admit_to_webview(VisibilityTier::Focused, true, &mut gate, 0, false),
            WebviewAdmission::Now
        );
    }

    /// Which cadence a held window is measured against follows the CURRENT
    /// tier, because one hold buffer serves both tiers and the tier can change
    /// with bytes still in it. Stock config (`unwatched` = `None`) falls back
    /// to the background cadence.
    ///
    /// That fallback is REACHED, not merely defensive: the reader samples the
    /// tier before it takes the hold lock, so a `background` -> `unwatched`
    /// change landing in that window pushes an already-decided `Hold` into the
    /// hold of a now-`unwatched` session. Those bytes were produced while the
    /// webview was entitled to them, so flushing them late is correct — but it
    /// must flush at a real cadence, never a zero-length window.
    #[test]
    fn hold_interval_follows_the_tier() {
        let bg = Duration::from_millis(250);
        let unwatched = Duration::from_millis(4000);
        assert_eq!(
            hold_interval(VisibilityTier::Unwatched, bg, Some(unwatched)),
            unwatched
        );
        assert_eq!(
            hold_interval(VisibilityTier::Background, bg, Some(unwatched)),
            bg
        );
        assert_eq!(
            hold_interval(VisibilityTier::Focused, bg, Some(unwatched)),
            bg
        );
        assert_eq!(
            hold_interval(VisibilityTier::Unwatched, bg, None),
            bg,
            "stock config must not yield a zero window"
        );
    }

    /// The reader's exit flush is ungated on purpose: after the child exits no
    /// further chunk can reveal a gap, so the last frame must land in every
    /// tier — including `unwatched`, whose pane may mount moments later.
    #[test]
    fn the_exit_flush_ships_in_every_tier() {
        let mut gate = EmissionGate::new();
        for tier in [
            VisibilityTier::Focused,
            VisibilityTier::Background,
            VisibilityTier::Unwatched,
        ] {
            assert_eq!(
                admit_to_webview(tier, false, &mut gate, FLOW_HIGH_WATERMARK * 100, false),
                WebviewAdmission::Now,
                "{tier:?}"
            );
        }
    }

    /// Tier transitions in both directions, driven through the same state a
    /// live session uses. `focused` after `unwatched` must not inherit a paused
    /// gate — the `unwatched` stretch never touched it.
    #[test]
    fn tier_transitions_switch_the_admission_immediately() {
        let state = VisibilityState::new();
        let mut gate = EmissionGate::new();
        let decide = |state: &VisibilityState, gate: &mut EmissionGate| {
            admit_to_webview(state.tier(), true, gate, 0, false)
        };

        assert_eq!(decide(&state, &mut gate), WebviewAdmission::Now);
        state.report("main", VisibilityTier::Background);
        assert_eq!(decide(&state, &mut gate), WebviewAdmission::Hold);
        state.report("main", VisibilityTier::Unwatched);
        assert_eq!(decide(&state, &mut gate), WebviewAdmission::Skip);
        state.report("main", VisibilityTier::Focused);
        assert_eq!(decide(&state, &mut gate), WebviewAdmission::Now);
    }

    /// SSE, WS and the scrollback ring are served UPSTREAM of the tier
    /// decision, so no tier can cost an external consumer a byte. This asserts
    /// the ring half directly against the real teeing path; the SSE/WS half is
    /// structural (`admit_to_webview` returns a webview-only verdict and the
    /// reader serves both broadcasts before consulting it).
    #[test]
    fn every_tier_still_tees_every_byte_into_the_ring() {
        for tier in [
            VisibilityTier::Focused,
            VisibilityTier::Background,
            VisibilityTier::Unwatched,
        ] {
            let state = VisibilityState::new();
            state.report("main", tier);
            let ring = Arc::new(Mutex::new(VecDeque::new()));
            let produced = Arc::new(AtomicU64::new(0));
            let first = tee_into_scrollback(&ring, &produced, b"alpha", SCROLLBACK_CAPACITY);
            let second = tee_into_scrollback(&ring, &produced, b"beta", SCROLLBACK_CAPACITY);
            assert_eq!((first, second), (0, 5), "{tier:?}");
            assert_eq!(produced.load(Ordering::Relaxed), 9, "{tier:?}");
            let bytes: Vec<u8> = ring.lock().unwrap().iter().copied().collect();
            assert_eq!(bytes, b"alphabeta", "{tier:?}");
        }
    }

    #[test]
    fn whole_frame_in_one_read_emits_immediately() {
        // sync_output already false post-advance → no holding, one event.
        let events = run_coalescer(&[(b"\x1b[?2026hFRAME\x1b[?2026l", 0, false)]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, b"\x1b[?2026hFRAME\x1b[?2026l");
        assert_eq!(events[0].1, 0);
    }

    #[test]
    fn multi_read_sync_frame_coalesces_into_one_emit() {
        // A frame split across three reads: open (in_sync), middle (in_sync),
        // close (in_sync flips false in the read that carried `?2026l`).
        // Offsets are the absolute stream positions of each read.
        let events = run_coalescer(&[
            (b"\x1b[?2026hPART1", 10, true),
            (b"PART2", 23, true),
            (b"PART3\x1b[?2026l", 28, false),
        ]);
        assert_eq!(events.len(), 1, "the frame must coalesce into ONE event");
        assert_eq!(events[0].0, b"\x1b[?2026hPART1PART2PART3\x1b[?2026l");
        // Offset must be the FIRST held byte's offset (the replay boundary).
        assert_eq!(events[0].1, 10);
    }

    #[test]
    fn non_sync_chunks_each_emit_separately() {
        let events = run_coalescer(&[(b"alpha", 0, false), (b"beta", 5, false)]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], (b"alpha".to_vec(), 0));
        assert_eq!(events[1], (b"beta".to_vec(), 5));
    }

    #[test]
    fn byte_cap_flushes_a_never_closed_sync_block() {
        let mut c = SyncFrameCoalescer::new();
        let emitted = Arc::new(Mutex::new(Vec::<(Vec<u8>, u64)>::new()));
        // First read opens the block and is under the cap.
        let big = vec![b'x'; SYNC_FLUSH_BYTE_CAP];
        {
            let sink = emitted.clone();
            c.feed(b"\x1b[?2026h", 0, true, move |p, o| {
                sink.lock().unwrap().push((p.to_vec(), o));
            });
        }
        // Second read pushes the held buffer past the byte cap → forced flush
        // even though `?2026l` never arrived.
        {
            let sink = emitted.clone();
            c.feed(&big, 8, true, move |p, o| {
                sink.lock().unwrap().push((p.to_vec(), o));
            });
        }
        let events = emitted.lock().unwrap();
        assert_eq!(events.len(), 1, "byte cap must force a flush");
        assert_eq!(events[0].1, 0, "flush keeps the first held byte's offset");
        assert!(events[0].0.len() >= SYNC_FLUSH_BYTE_CAP);
    }

    #[test]
    fn flush_remaining_emits_a_held_unclosed_frame_on_exit() {
        // Reader exits (EOF) mid-frame — the held prefix must still be emitted.
        let events = run_coalescer(&[(b"\x1b[?2026hHALF", 100, true)]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, b"\x1b[?2026hHALF");
        assert_eq!(events[0].1, 100);
    }

    #[test]
    fn build_command_from_override_uses_program_and_args() {
        // Decision 3: an explicit override runs that program with its args as
        // the PTY child (argv[0]=program, argv[1..]=args).
        let cmd = TerminalSession::build_command_from(Some(vec![
            "claude".to_string(),
            "do the thing".to_string(),
        ]));
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert_eq!(argv, vec!["claude".to_string(), "do the thing".to_string()]);
    }

    #[test]
    fn build_command_from_program_only_has_no_extra_args() {
        let cmd = TerminalSession::build_command_from(Some(vec!["claude".to_string()]));
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert_eq!(argv, vec!["claude".to_string()]);
    }

    #[test]
    fn build_command_from_none_falls_back_to_shell() {
        // None → the interactive shell path (back-compat by construction). The
        // shell program is platform-specific, but it must NOT be `claude` and
        // must match what `build_shell_command` produces.
        let fallback = TerminalSession::build_command_from(None);
        let shell = TerminalSession::build_shell_command();
        assert_eq!(
            fallback.get_argv().first(),
            shell.get_argv().first(),
            "None override must fall back to the shell program"
        );
    }

    #[test]
    fn build_command_from_empty_vec_falls_back_to_shell() {
        // An empty override is meaningless (no program) → fall back to the
        // shell rather than spawning nothing.
        let fallback = TerminalSession::build_command_from(Some(vec![]));
        let shell = TerminalSession::build_shell_command();
        assert_eq!(fallback.get_argv().first(), shell.get_argv().first());
    }

    #[test]
    fn path_env_key_is_os_canonical_casing() {
        // Windows env is case-insensitive with undefined duplicate resolution,
        // so the child path key MUST be the OS-preferred casing (`Path`), not a
        // bare `PATH` that the inherited `Path` could shadow.
        #[cfg(windows)]
        assert_eq!(TerminalSession::PATH_ENV_KEY, "Path");
        #[cfg(not(windows))]
        assert_eq!(TerminalSession::PATH_ENV_KEY, "PATH");
    }

    #[test]
    fn set_child_path_leaves_exactly_one_shim_first_path_key() {
        // The env-injection invariant (plan Phase 1): after set_child_path the
        // child has EXACTLY ONE path key (any casing) and it carries the shim
        // dir as its FIRST segment. Seed a pre-existing case-variant to prove
        // the removal defeats the Windows `Path`-shadows-`PATH` collision.
        let mut cmd = CommandBuilder::new("dummy");
        cmd.env("Path", "C:\\real\\bin;C:\\windows");
        cmd.env("PATH", "/usr/bin:/bin");

        let sep = if cfg!(windows) { ';' } else { ':' };
        let shim_dir = "SHIM_DIR_MARKER";
        let existing = if cfg!(windows) {
            "C:\\real\\bin;C:\\windows"
        } else {
            "/usr/bin:/bin"
        };
        let new_path = format!("{shim_dir}{sep}{existing}");
        TerminalSession::set_child_path(&mut cmd, &new_path);

        // Exactly one path key survives (case-folded), and it is shim-first.
        let path_keys: Vec<(String, String)> = cmd
            .iter_full_env_as_str()
            .filter(|(k, _)| k.eq_ignore_ascii_case("path"))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(
            path_keys.len(),
            1,
            "expected exactly one path key, got {path_keys:?}"
        );
        let (key, value) = &path_keys[0];
        assert_eq!(key, TerminalSession::PATH_ENV_KEY, "canonical casing");
        assert!(
            value.starts_with(shim_dir),
            "shim dir must be the FIRST path segment; got {value:?}"
        );
        // The readback via the canonical key must also find the shim-first value
        // (guards against a stray un-removed variant winning).
        let read = cmd
            .get_env(TerminalSession::PATH_ENV_KEY)
            .map(|v| v.to_string_lossy().to_string());
        assert_eq!(read.as_deref(), Some(new_path.as_str()));
    }

    #[test]
    fn explicit_session_id_from_space_separated() {
        // The runner-launched direct-command path builds
        // `[claude, --session-id, <id>, …]`; the seam must adopt <id> as its
        // authoritative pin so recorded id == the id claude runs under.
        let cmd = TerminalSession::build_command_from(Some(vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "--session-id".to_string(),
            "abc-123".to_string(),
            "--".to_string(),
            "do the thing".to_string(),
        ]));
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn explicit_session_id_from_attached_form() {
        let cmd = TerminalSession::build_command_from(Some(vec![
            "claude".to_string(),
            "--session-id=xyz-789".to_string(),
        ]));
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("xyz-789".to_string())
        );
    }

    #[test]
    fn explicit_session_id_from_none_for_interactive_shell() {
        // The interactive-shell path carries no --session-id → the seam mints a
        // fresh id (unit-tested here only that it returns None so the caller
        // falls back to Uuid::new_v4).
        let cmd = TerminalSession::build_command_from(None);
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
    }

    /// Build a claude argv for the explicit-id tests.
    fn claude_argv(args: &[&str]) -> CommandBuilder {
        let mut parts = vec!["claude".to_string()];
        parts.extend(args.iter().map(|a| a.to_string()));
        TerminalSession::build_command_from(Some(parts))
    }

    #[test]
    fn explicit_session_id_from_resume_space_separated() {
        // The account-migration respawn path renders `--resume <id>` and drops
        // `--session-id` entirely (launch_spec::render_argv step 3). Before this
        // was recognized the seam minted a phantom uuid no process ever ran under.
        let cmd = claude_argv(&[
            "--permission-mode",
            "bypassPermissions",
            "--resume",
            "abc-123",
        ]);
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn explicit_session_id_from_resume_attached_form() {
        let cmd = claude_argv(&["--resume=xyz-789"]);
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("xyz-789".to_string())
        );
    }

    #[test]
    fn explicit_session_id_from_short_r_flag() {
        let cmd = claude_argv(&["-r", "abc-123"]);
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn explicit_session_id_from_short_r_ignored_for_non_claude_head() {
        // `-r` is a common unrelated short flag; only a claude child may claim it
        // (mirrors the shim's detect_identity_tool head gate).
        let cmd = TerminalSession::build_command_from(Some(vec![
            "cp".to_string(),
            "-r".to_string(),
            "src".to_string(),
            "dst".to_string(),
        ]));
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
    }

    #[test]
    fn explicit_session_id_from_none_for_continue_flags_that_name_no_id() {
        // `--continue` / `-c` name NO id — the seam must fall back to minting one
        // rather than invent an id the process will not run under.
        let cmd = claude_argv(&["--continue"]);
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
        let cmd = claude_argv(&["-c"]);
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
    }

    #[test]
    fn explicit_session_id_from_never_captures_a_following_flag() {
        let cmd = claude_argv(&["--resume", "--verbose"]);
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
        // …and the rejected lookahead is still scanned as a flag in its own right.
        let cmd = claude_argv(&["--resume", "--session-id", "abc-123"]);
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn explicit_session_id_from_none_when_resume_is_the_final_arg() {
        let cmd = claude_argv(&["--resume"]);
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
        let cmd = claude_argv(&["-r"]);
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
    }

    #[test]
    fn explicit_session_id_from_prefers_resume_over_session_id_when_both_present() {
        // launch_spec::render_argv step 3: "resume takes precedence over
        // session-id" — so the resume id is the one claude actually runs under.
        let cmd = claude_argv(&["--session-id", "aaa-111", "--resume", "bbb-222"]);
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("bbb-222".to_string())
        );
        // Argv order must not change the answer.
        let cmd = claude_argv(&["--resume", "bbb-222", "--session-id", "aaa-111"]);
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("bbb-222".to_string())
        );
    }

    #[test]
    fn explicit_session_id_from_ignores_glued_false_prefixes() {
        let cmd = claude_argv(&["--session-idX", "abc-123"]);
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
        let cmd = claude_argv(&["--resumeX", "abc-123"]);
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
    }

    #[test]
    fn apply_identity_seam_helpers_never_mutate_argv() {
        // The seam is env-only by contract: adopting an id must never inject or
        // rewrite argv. Guard the read path used by the seam.
        let before = claude_argv(&["--resume", "abc-123"]);
        let argv_before: Vec<String> = before
            .get_argv()
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let _ = TerminalSession::explicit_session_id_from(&before);
        let argv_after: Vec<String> = before
            .get_argv()
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(argv_before, argv_after);
    }

    #[test]
    fn build_submit_payload_emits_paste_markers_and_bare_cr() {
        let payload = build_submit_payload("hello");
        assert_eq!(payload, b"\x1b[200~hello\x1b[201~\r");
    }

    #[test]
    fn build_submit_payload_handles_empty_message() {
        let payload = build_submit_payload("");
        assert_eq!(payload, b"\x1b[200~\x1b[201~\r");
    }

    // ---- Inbound prompt sanitization (2026-08-20 ci-nudge-dedup Phase 1) ----
    //
    // Every literal below is spelled out as raw bytes on purpose. Asserting
    // against `BRACKETED_PASTE_END` would re-derive the expectation from the
    // same constant the implementation uses and pin nothing.

    /// NEGATIVE CONTROL. An over-broad stripper (e.g. the outbound
    /// `terminal::strip_ansi`) fails this: colour codes and diffs are exactly
    /// what agents legitimately paste, and must survive byte-for-byte.
    #[test]
    fn sanitize_submit_body_leaves_sgr_and_unicode_untouched() {
        assert_eq!(
            sanitize_submit_body("\x1b[31mred\x1b[0m"),
            "\x1b[31mred\x1b[0m"
        );
        // A coloured diff with tabs, newlines and non-ASCII text.
        let diff = "\x1b[32m+\tnéw ✓\x1b[0m\n\x1b[31m-\told\x1b[0m\n";
        assert_eq!(sanitize_submit_body(diff), diff);
    }

    #[test]
    fn sanitize_submit_body_strips_control_bytes_but_keeps_whitespace() {
        // \x00 NUL, \x07 BEL, a bare ESC (not introducing a sequence), and
        // \u{85} (C1 NEL) are removed; \t \n \r survive.
        assert_eq!(
            sanitize_submit_body("a\u{0}b\u{7}c\u{1b}d\u{85}e\tf\ng\rh"),
            "abcde\tf\ng\rh"
        );
        // A dangling ESC at end-of-string has no successor at all.
        assert_eq!(sanitize_submit_body("tail\u{1b}"), "tail");
        // DEL is a control byte too.
        assert_eq!(sanitize_submit_body("x\u{7f}y"), "xy");
    }

    #[test]
    fn sanitize_submit_body_neutralizes_embedded_paste_end() {
        assert_eq!(sanitize_submit_body("a\x1b[201~b"), "a[201~b");
    }

    /// Stripping a control byte must not be able to *create* the end marker.
    /// `\x1b[2<NUL>01~` becomes `\x1b[201~` under a naive strip-then-scan
    /// implementation; the output invariant catches it.
    #[test]
    fn sanitize_submit_body_cannot_be_reconstituted_by_control_byte_removal() {
        assert_eq!(sanitize_submit_body("\x1b[2\u{0}01~"), "[201~");
    }

    /// The falsifiable core: assert what `submit_prompt` ACTUALLY writes to
    /// the PTY, not what the test-only `build_submit_payload` returns.
    #[test]
    fn submit_prompt_neutralizes_embedded_paste_end_marker() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session
            .submit_prompt("before\x1b[201~after")
            .expect("submit_prompt failed");
        let written = buf.lock().unwrap().clone();

        assert_eq!(written, b"\x1b[200~before[201~after\x1b[201~\r");

        // Count + position, stated independently of the equality above.
        let end_marker: &[u8] = b"\x1b[201~";
        let occurrences: Vec<usize> = written
            .windows(end_marker.len())
            .enumerate()
            .filter(|(_, w)| *w == end_marker)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            occurrences.len(),
            1,
            "exactly one paste-end marker expected, got {:?} in {:?}",
            occurrences,
            String::from_utf8_lossy(&written)
        );
        assert_eq!(
            occurrences[0],
            written.len() - end_marker.len() - 1,
            "the only paste-end marker must be the trailing terminator, \
             immediately before the bare CR"
        );
        assert!(written.starts_with(b"\x1b[200~"));
        assert!(written.ends_with(b"\r"));
    }

    #[test]
    fn submit_prompt_strips_control_bytes_from_the_body() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session
            .submit_prompt("ok\u{0}\u{7}\u{1b}zdone")
            .expect("submit_prompt failed");
        let written = buf.lock().unwrap().clone();
        // 'z' is not an escape introducer, so the ESC is a stray control byte.
        assert_eq!(written, b"\x1b[200~okzdone\x1b[201~\r");
    }

    /// `build_submit_payload` must go through the same choke point, or the
    /// test-only shape drifts from production again.
    #[test]
    fn build_submit_payload_shares_the_sanitizer_with_submit_prompt() {
        assert_eq!(
            build_submit_payload("x\x1b[201~y"),
            b"\x1b[200~x[201~y\x1b[201~\r"
        );

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session
            .submit_prompt("x\x1b[201~y")
            .expect("submit_prompt failed");
        assert_eq!(
            buf.lock().unwrap().clone(),
            build_submit_payload("x\x1b[201~y")
        );
    }

    // ---- String-sequence terminators, the paste BEGIN marker, and an
    // honest wire-length report (post-merge follow-up to the sanitizer).

    /// NEGATIVE CONTROL, second form. The first one pins SGR (a `CSI`
    /// sequence, self-terminating). A **string** sequence is only well
    /// formed once terminated, and both of its terminators — `ST` and, for
    /// `OSC`, `BEL` — are bytes the control-character rule would otherwise
    /// remove. Dropping either leaves an unterminated sequence that eats
    /// whatever the terminal sees next.
    #[test]
    fn sanitize_submit_body_preserves_string_sequence_terminators() {
        // OSC 0 (window title) terminated by ST.
        let osc_st = "\x1b]0;build ok\x1b\\";
        assert_eq!(sanitize_submit_body(osc_st), osc_st);
        // The same sequence terminated by BEL, the xterm-legacy form.
        let osc_bel = "\x1b]0;build ok\x07";
        assert_eq!(sanitize_submit_body(osc_bel), osc_bel);
        // OSC 8 hyperlinks — what a modern build log actually carries.
        let link = "\x1b]8;;https://example.test\x1b\\click\x1b]8;;\x1b\\";
        assert_eq!(sanitize_submit_body(link), link);
        // DCS is a string sequence too.
        let dcs = "\x1bPq#0;2;0;0;0\x1b\\";
        assert_eq!(sanitize_submit_body(dcs), dcs);
    }

    /// BEL is load-bearing only *inside* a string sequence; on its own it is
    /// still a control byte, and the existing strip test stays true.
    #[test]
    fn sanitize_submit_body_still_strips_bel_outside_a_string_sequence() {
        assert_eq!(sanitize_submit_body("a\x07b"), "ab");
        // Terminated OSC, then a stray BEL: the first survives, the second
        // does not.
        assert_eq!(sanitize_submit_body("\x1b]0;t\x07x\x07y"), "\x1b]0;t\x07xy");
    }

    /// A string sequence the caller never closes would consume the paste END
    /// marker `paste_block` appends. It is closed with an `ST`.
    #[test]
    fn sanitize_submit_body_closes_a_dangling_string_sequence() {
        assert_eq!(
            sanitize_submit_body("\x1b]0;no terminator"),
            "\x1b]0;no terminator\x1b\\"
        );
        // Nothing is appended when the sequence is already closed...
        assert_eq!(sanitize_submit_body("\x1b]0;t\x1b\\"), "\x1b]0;t\x1b\\");
        // ...nor when a later escape sequence aborts it, as a real parser
        // treats an ESC arriving mid-string.
        assert_eq!(sanitize_submit_body("\x1b]0;t\x1b[0m"), "\x1b]0;t\x1b[0m");
    }

    /// Neutralizing a marker removes the `ESC` that introduced its `CSI` —
    /// and a `CSI` mid-string is what ends an open string sequence. Without
    /// undoing that transition, `ESC ] 0 ; ESC [ 2 0 1 ~` leaves the `OSC`
    /// open and it swallows the paste END marker `paste_block` appends.
    #[test]
    fn sanitize_submit_body_reopens_a_string_sequence_when_it_strips_the_csi() {
        assert_eq!(
            sanitize_submit_body("\x1b]0;\x1b[201~tail"),
            "\x1b]0;[201~tail\x1b\\"
        );
        // The same undo must not fire when no string sequence was open.
        assert_eq!(sanitize_submit_body("a\x1b[201~b"), "a[201~b");
    }

    /// An embedded BEGIN marker re-opens the paste block, so `paste_block`'s
    /// own END closes the INNER one and the terminal stays in paste mode for
    /// whatever the operator types next. Neutralized the same way as END.
    #[test]
    fn sanitize_submit_body_neutralizes_embedded_paste_begin() {
        assert_eq!(sanitize_submit_body("a\x1b[200~b"), "a[200~b");
        // And it cannot be reconstituted by control-byte removal either.
        assert_eq!(sanitize_submit_body("\x1b[2\u{0}00~"), "[200~");
    }

    /// The falsifiable core for the BEGIN rule: what `submit_prompt` writes.
    #[test]
    fn submit_prompt_neutralizes_embedded_paste_begin_marker() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session
            .submit_prompt("before\x1b[200~after")
            .expect("submit_prompt failed");
        let written = buf.lock().unwrap().clone();

        assert_eq!(written, b"\x1b[200~before[200~after\x1b[201~\r");

        let begin_marker: &[u8] = b"\x1b[200~";
        let occurrences: Vec<usize> = written
            .windows(begin_marker.len())
            .enumerate()
            .filter(|(_, w)| *w == begin_marker)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            occurrences,
            vec![0],
            "the only paste-begin marker must be the leading one, got {:?}",
            String::from_utf8_lossy(&written)
        );
    }

    /// An unterminated string sequence in the body must not be able to
    /// swallow the trailing paste END marker.
    #[test]
    fn submit_prompt_closes_a_dangling_string_sequence_before_the_end_marker() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session
            .submit_prompt("\x1b]0;title")
            .expect("submit_prompt failed");
        assert_eq!(
            buf.lock().unwrap().clone(),
            b"\x1b[200~\x1b]0;title\x1b\\\x1b[201~\r"
        );
    }

    /// `bytes` is what the route reports to its caller. It must equal what
    /// `submit_prompt` really wrote — not `message.len() + framing`, which
    /// the sanitizer made wrong.
    #[test]
    fn submit_payload_info_reports_the_bytes_actually_written() {
        for message in [
            "hello",
            "",
            "before\x1b[201~after",
            "ok\u{0}\u{7}\u{1b}zdone",
            "\x1b]0;title",
            "\x1b[31mred\x1b[0m",
        ] {
            let info = submit_payload_info(message);
            let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
            let session = make_test_session(buf.clone());
            session
                .submit_prompt(message)
                .expect("submit_prompt failed");
            let written = buf.lock().unwrap().clone();
            assert_eq!(
                info.bytes,
                written.len(),
                "reported byte count disagrees with the PTY write for {:?}",
                message
            );
            assert_eq!(
                info.bytes,
                build_submit_payload(message).len(),
                "reported byte count disagrees with build_submit_payload for {:?}",
                message
            );
        }

        // The pre-sanitizer formula is genuinely wrong now, so the equality
        // above is not a tautology: the neutralizer drops the embedded
        // marker's ESC, one byte the old `message.len() + 13` still counted.
        assert_eq!(submit_payload_info("before\x1b[201~after").bytes, 29);
        assert_eq!("before\x1b[201~after".len() + 13, 30);
    }

    /// A caller whose message was altered is told so.
    #[test]
    fn submit_payload_info_flags_only_an_altered_body() {
        assert!(!submit_payload_info("plain text").sanitized);
        assert!(!submit_payload_info("\x1b[31mred\x1b[0m").sanitized);
        assert!(!submit_payload_info("").sanitized);
        assert!(submit_payload_info("a\x1b[201~b").sanitized);
        assert!(submit_payload_info("a\x1b[200~b").sanitized);
        assert!(submit_payload_info("nul\u{0}here").sanitized);
        // Same length in and out — the dangling OSC gains a two-byte ST
        // while two NULs are dropped — so only comparing content catches it.
        assert_eq!(submit_payload_info("\x1b]a\u{0}\u{0}").bytes, 18);
        assert!(submit_payload_info("\x1b]a\u{0}\u{0}").sanitized);
    }

    #[test]
    fn submit_prompt_writes_bracketed_paste_then_bare_cr() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session
            .submit_prompt("hello")
            .expect("submit_prompt failed");
        let written = buf.lock().unwrap().clone();
        assert_eq!(written, b"\x1b[200~hello\x1b[201~\r");
    }

    #[test]
    fn submit_prompt_does_not_emit_lf() {
        // Regression guard: the Phase 6 §6 bug was that send_user_message
        // sent CR-LF, and Claude Code's bracketed-paste handler ate the
        // LF as the paste-block terminator instead of submitting. The
        // submit byte must be a bare CR; no LF anywhere in the output.
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session
            .submit_prompt("line1 line2")
            .expect("submit_prompt failed");
        let written = buf.lock().unwrap().clone();
        assert!(
            !written.contains(&b'\n'),
            "submit_prompt must not emit LF bytes; got {:?}",
            written
        );
    }

    #[test]
    fn set_title_updates_info() {
        // Phase 2: bi-directional title sync. set_title must be visible
        // via info() so subsequent /terminals reads see the new value.
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf);
        assert_eq!(session.info().title, "test");
        session.set_title("✳ Claude Code".to_string());
        assert_eq!(session.info().title, "✳ Claude Code");
        // Idempotent overwrite.
        session.set_title("Worker 1".to_string());
        assert_eq!(session.info().title, "Worker 1");
    }

    // --- typed-input observer: line assembly + the write() funnel ----------

    #[test]
    fn consume_input_bytes_assembles_edits_and_caps_lines() {
        let mut buf = String::new();
        // Partial chunk — no completed line, buffer accumulates across calls.
        assert!(consume_input_bytes(&mut buf, b"claude --re").is_empty());
        assert_eq!(buf, "claude --re");
        let done = consume_input_bytes(&mut buf, b"sume abc\r");
        assert_eq!(done, vec!["claude --resume abc".to_string()]);
        assert!(buf.is_empty());
        // LF completes too; blank lines are not emitted.
        assert!(consume_input_bytes(&mut buf, b"   \n").is_empty());
        // 0x08 backspace and 0x7F DEL both pop the last char.
        let done = consume_input_bytes(&mut buf, b"claude -x\x08-resume\r");
        assert_eq!(done, vec!["claude --resume".to_string()]);
        let done = consume_input_bytes(&mut buf, b"git statuz\x7Fs\r");
        assert_eq!(done, vec!["git status".to_string()]);
        // Escape sequences / non-ASCII bytes are dropped, printables kept.
        let done = consume_input_bytes(&mut buf, b"ls\x1b\x01\xc3\xa9 -la\r");
        assert_eq!(done, vec!["ls -la".to_string()]);
        // > 4 KiB with no newline — buffer cleared, not unbounded growth.
        assert!(consume_input_bytes(&mut buf, &vec![b'a'; 5000]).is_empty());
        assert!(buf.is_empty(), "over-cap buffer must be cleared");
        // Still usable afterwards.
        let done = consume_input_bytes(&mut buf, b"echo hi\r");
        assert_eq!(done, vec!["echo hi".to_string()]);
    }

    #[test]
    fn write_funnels_input_into_the_observer() {
        // The observation funnel lives in `TerminalSession::write` itself —
        // ANY caller of write() feeds the input-line buffer, with no
        // per-caller observe call (the fixture has no app_handle, so effect
        // dispatch is skipped but line assembly still runs).
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session.write(b"claude --re").expect("write failed");
        assert_eq!(
            session.input_line_buf.lock().unwrap().as_str(),
            "claude --re",
            "write() must feed the typed-input observer"
        );
        // Completing the line drains the buffer (the line was dispatched)...
        session.write(b"sume\r").expect("write failed");
        assert!(session.input_line_buf.lock().unwrap().is_empty());
        // ...and the PTY itself still received every byte, unchanged.
        assert_eq!(buf.lock().unwrap().as_slice(), b"claude --resume\r");
    }

    #[test]
    fn subscribe_first_osc_title_returns_some_once() {
        // The receiver can be taken at most once. Subsequent takes return
        // `None` — `spawn_worker_session` interprets that as "already
        // ready, skip the wait".
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf);
        assert!(session.subscribe_first_osc_title().is_some());
        assert!(session.subscribe_first_osc_title().is_none());
    }

    /// Spawn a real `portable-pty` child that prints `marker`, drive a
    /// reader loop that tees its output into `session`'s OWN scrollback +
    /// byte-counter Arcs via the production [`tee_into_scrollback`] path,
    /// and stop once the child has exited and its output is drained.
    ///
    /// This is the exact teeing the reader thread in
    /// [`TerminalSession::spawn`] performs per read — only the Tauri event
    /// emission (irrelevant to buffer state) is omitted, which is why the
    /// test can run without an `AppHandle`.
    fn drive_real_pty_into(session: &TerminalSession, marker: &str) {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        // Cross-platform single-shot echo: `cmd /C echo MARKER` on Windows,
        // `sh -c "echo MARKER"` elsewhere. Both exit immediately after
        // printing, so the reader loop terminates on EOF.
        let mut cmd = if cfg!(windows) {
            let mut c = CommandBuilder::new("cmd");
            c.arg("/C");
            c.arg(format!("echo {}", marker));
            c
        } else {
            let mut c = CommandBuilder::new("sh");
            c.arg("-c");
            c.arg(format!("echo {}", marker));
            c
        };
        cmd.env("TERM", "xterm-256color");

        let mut child = pair.slave.spawn_command(cmd).expect("spawn echo child");
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        // Drop the slave handle so the master sees EOF after the child exits.
        drop(pair.slave);

        let scrollback = session.scrollback_buffer.clone();
        let total = session.total_bytes_produced.clone();

        // Reader thread: tee everything into the session's own Arcs via the
        // production path until the PTY closes. This MUST be its own thread
        // (exactly like the production reader thread): on Windows/ConPTY,
        // `reader.read()` keeps blocking after the child exits — only
        // closing the MASTER unblocks it — so an inline read loop with a
        // between-reads deadline check hangs (it wedged the windows-latest
        // CI job for the full 90-minute timeout).
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        tee_into_scrollback(&scrollback, &total, &buf[..n], SCROLLBACK_CAPACITY);
                    }
                }
            }
        });

        // The echo child exits immediately after printing.
        let _ = child.wait();

        // Bounded poll until the marker lands in the session's scrollback
        // (the reader thread races the child's output in). On timeout we
        // fall through — the caller's assertions then fail with the actual
        // buffer content rather than hanging the test harness.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let (sb, _) = session.get_scrollback_buffer();
            if String::from_utf8_lossy(&sb).contains(marker) || std::time::Instant::now() > deadline
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Closing the master tears down the PTY (ClosePseudoConsole on
        // Windows), which unblocks the reader thread's pending `read()`.
        drop(pair.master);
        let _ = reader_thread.join();
    }

    /// REGRESSION (plan
    /// `2026-06-06-gate-continuation-terminal-visibility-and-execution`):
    /// a live observation showed `GET /terminals/{id}/buffer` returning
    /// byte-identical stale content for two DIFFERENT terminals. The read
    /// path proved sound and the defect did not reproduce, but no buffer
    /// or route-handler test existed. This locks in the core invariant the
    /// handler relies on: each session owns an independent scrollback
    /// buffer, so two sessions running distinct commands read back distinct
    /// content — never each other's, never byte-identical.
    ///
    /// Uses TWO real `portable-pty` children (cross-platform echo) feeding
    /// the production [`tee_into_scrollback`] path into each session's own
    /// Arcs, then asserts isolation through the real
    /// [`TerminalSession::get_scrollback_buffer`].
    #[test]
    fn scrollback_buffers_are_per_session_distinct() {
        const MARKER_A: &str = "QONTINUI_DISTINCT_MARKER_AAAAA";
        const MARKER_B: &str = "QONTINUI_DISTINCT_MARKER_BBBBB";

        let session_a = make_test_session(Arc::new(Mutex::new(Vec::new())));
        let session_b = make_test_session(Arc::new(Mutex::new(Vec::new())));

        // Confirm the two sessions hold DISTINCT buffer Arcs (the precise
        // aliasing the live defect would represent). If these ever pointed
        // at the same allocation, the assertions below would also fail —
        // this is the cheap, direct guard.
        assert!(
            !Arc::ptr_eq(&session_a.scrollback_buffer, &session_b.scrollback_buffer),
            "sessions must not share a scrollback buffer Arc"
        );

        drive_real_pty_into(&session_a, MARKER_A);
        drive_real_pty_into(&session_b, MARKER_B);

        let (buf_a, _) = session_a.get_scrollback_buffer();
        let (buf_b, _) = session_b.get_scrollback_buffer();
        let text_a = String::from_utf8_lossy(&buf_a);
        let text_b = String::from_utf8_lossy(&buf_b);

        // (a) A has its own marker and NOT B's.
        assert!(
            text_a.contains(MARKER_A),
            "session A buffer missing MARKER_A; got {:?}",
            text_a
        );
        assert!(
            !text_a.contains(MARKER_B),
            "session A buffer leaked MARKER_B; got {:?}",
            text_a
        );

        // (b) B has its own marker and NOT A's.
        assert!(
            text_b.contains(MARKER_B),
            "session B buffer missing MARKER_B; got {:?}",
            text_b
        );
        assert!(
            !text_b.contains(MARKER_A),
            "session B buffer leaked MARKER_A; got {:?}",
            text_b
        );

        // (c) The two buffers are not byte-identical — the exact stale-read
        // symptom from the live observation.
        assert_ne!(
            buf_a, buf_b,
            "two terminals returned byte-identical scrollback buffers"
        );
    }

    // ── Configurable scrollback ring (many-sessions plan Phase 8) ────────

    /// The stock capacity is the historical 1 MiB: with no `performance`
    /// key in settings.json, the ring behaves exactly as it always did.
    #[test]
    fn default_scrollback_capacity_is_the_historical_1mib() {
        assert_eq!(
            crate::settings::PerformanceSettings::default().effective_scrollback_capacity(),
            SCROLLBACK_CAPACITY
        );
    }

    /// The value a spawn will actually allocate follows the setting, with the
    /// floor applied. Without this, `spawn` could be reverted to the bare
    /// constant and the rest of the ring tests would stay green.
    #[test]
    fn resolved_capacity_follows_the_setting() {
        let _guard = crate::settings::perf_test_lock();
        crate::settings::set_performance_cache(crate::settings::PerformanceSettings::default());
        assert_eq!(resolved_scrollback_capacity(), SCROLLBACK_CAPACITY);

        crate::settings::set_performance_cache(crate::settings::PerformanceSettings {
            scrollback_capacity_bytes: 4 * 1024 * 1024,
            ..crate::settings::PerformanceSettings::default()
        });
        assert_eq!(resolved_scrollback_capacity(), 4 * 1024 * 1024);

        // Under the floor the SPAWN value is clamped, not the stored one.
        crate::settings::set_performance_cache(crate::settings::PerformanceSettings {
            scrollback_capacity_bytes: 1,
            ..crate::settings::PerformanceSettings::default()
        });
        assert_eq!(
            resolved_scrollback_capacity(),
            crate::settings::MIN_SCROLLBACK_CAPACITY
        );

        crate::settings::set_performance_cache(crate::settings::PerformanceSettings::default());
    }

    /// The two cadences a spawn will actually give a session follow the
    /// settings, through the same process-cached snapshot the ring size reads.
    ///
    /// This is the layer D3's defect lived one step below: the accessors on
    /// `PerformanceSettings` were already tested, and the emission path is
    /// tested against an injected interval — but nothing pinned that `spawn`
    /// resolves the setting rather than the constant, which is exactly the
    /// shape of "the field exists, is served, and nothing reads it".
    #[test]
    fn resolved_flush_intervals_follow_the_settings() {
        let _guard = crate::settings::perf_test_lock();
        crate::settings::set_performance_cache(crate::settings::PerformanceSettings::default());
        assert_eq!(
            resolved_flush_intervals(),
            (crate::terminal::visibility::BACKGROUND_FLUSH_INTERVAL, None),
            "stock: the historical background spacing, and a dark unwatched tier"
        );

        crate::settings::set_performance_cache(crate::settings::PerformanceSettings {
            background_flush_interval_ms: 1000,
            unwatched_flush_interval_ms: 4000,
            ..crate::settings::PerformanceSettings::default()
        });
        assert_eq!(
            resolved_flush_intervals(),
            (
                Duration::from_millis(1000),
                Some(Duration::from_millis(4000))
            )
        );

        // `0` is the unwatched tier's OFF switch, not a zero-length window —
        // collapsing the two would make every stock install's silent tier an
        // uncoalesced firehose.
        crate::settings::set_performance_cache(crate::settings::PerformanceSettings {
            background_flush_interval_ms: 0,
            unwatched_flush_interval_ms: 0,
            ..crate::settings::PerformanceSettings::default()
        });
        assert_eq!(resolved_flush_intervals(), (Duration::ZERO, None));

        crate::settings::set_performance_cache(crate::settings::PerformanceSettings::default());
    }

    /// The digest's two conditions are orthogonal: the session's frozen CONFIG
    /// and its live TIER. Pinned as a predicate because the emitter needs a
    /// Tauri handle and returns before either is observable, so a regression
    /// in this decision would be invisible to every other test.
    #[test]
    fn digest_is_owed_only_by_a_dark_unwatched_session() {
        // Stock config: owed while unwatched, never in a tier that already
        // delivers `terminal-output` (the page tap feeds tracking there, and a
        // digest on top would double-count the sparkline).
        assert!(digest_is_owed(VisibilityTier::Unwatched, None));
        assert!(!digest_is_owed(VisibilityTier::Background, None));
        assert!(!digest_is_owed(VisibilityTier::Focused, None));

        // Configured cadence: the tier emits, so the digest stands down for
        // the session's whole life — in every tier, including `unwatched`.
        let configured = Some(Duration::from_millis(4000));
        assert!(!digest_is_owed(VisibilityTier::Unwatched, configured));
        assert!(!digest_is_owed(VisibilityTier::Background, configured));
        assert!(!digest_is_owed(VisibilityTier::Focused, configured));
    }

    /// The ring honors the capacity it was given: it keeps the newest
    /// `capacity` bytes and drops the oldest, while the monotonic byte
    /// counter still reports everything that was produced. (The counter is
    /// the offset the frontend dedups a replay against, so a smaller ring
    /// must NOT make it lie.)
    #[test]
    fn tee_honors_a_configured_capacity() {
        let ring = Arc::new(Mutex::new(VecDeque::<u8>::new()));
        let total = Arc::new(AtomicU64::new(0));

        // 100 bytes into a 16-byte ring.
        let data: Vec<u8> = (0u8..100).collect();
        let start = tee_into_scrollback(&ring, &total, &data, 16);

        assert_eq!(start, 0, "first chunk starts at offset 0");
        assert_eq!(
            total.load(Ordering::Relaxed),
            100,
            "the counter counts produced bytes, not retained ones"
        );
        let retained: Vec<u8> = ring.lock().unwrap().iter().copied().collect();
        assert_eq!(retained.len(), 16, "ring is bounded by the given capacity");
        assert_eq!(
            retained,
            (84u8..100).collect::<Vec<u8>>(),
            "the ring keeps the NEWEST bytes"
        );
    }

    /// A larger configured capacity retains more — the knob moves in both
    /// directions, and under the cap nothing is dropped at all.
    #[test]
    fn tee_retains_everything_under_the_configured_capacity() {
        let ring = Arc::new(Mutex::new(VecDeque::<u8>::new()));
        let total = Arc::new(AtomicU64::new(0));
        let data: Vec<u8> = (0u8..100).collect();

        tee_into_scrollback(&ring, &total, &data, 4096);

        let retained: Vec<u8> = ring.lock().unwrap().iter().copied().collect();
        assert_eq!(retained, data, "nothing evicted below the capacity");
    }

    /// Successive chunks return their absolute start offsets regardless of
    /// ring size — the replay-dedup contract survives a small ring.
    #[test]
    fn tee_start_offsets_are_absolute_under_a_small_ring() {
        let ring = Arc::new(Mutex::new(VecDeque::<u8>::new()));
        let total = Arc::new(AtomicU64::new(0));

        assert_eq!(tee_into_scrollback(&ring, &total, &[1, 2, 3, 4], 4), 0);
        assert_eq!(tee_into_scrollback(&ring, &total, &[5, 6], 4), 4);
        assert_eq!(tee_into_scrollback(&ring, &total, &[7], 4), 6);
        assert_eq!(total.load(Ordering::Relaxed), 7);
    }

    // ── EmissionGate hysteresis (webview emission backpressure) ──────────
    //
    // Regression guard for the deselected-page hard-freeze: flow control
    // must gate webview EMISSION only, via this hysteresis policy — the
    // reader loop itself contains no pause path anymore. These tests pin
    // the policy's exact transition points.

    #[test]
    fn emission_gate_emits_below_high_watermark() {
        let mut gate = EmissionGate::new();
        assert!(gate.should_emit(0));
        assert!(gate.should_emit(FLOW_LOW_WATERMARK));
        assert!(gate.should_emit(FLOW_HIGH_WATERMARK)); // exactly High: still emits
    }

    #[test]
    fn emission_gate_pauses_above_high_and_holds_until_low() {
        let mut gate = EmissionGate::new();
        assert!(!gate.should_emit(FLOW_HIGH_WATERMARK + 1)); // crosses High: pause
                                                             // Anywhere above Low stays paused — including values far below High.
        assert!(!gate.should_emit(FLOW_HIGH_WATERMARK));
        assert!(!gate.should_emit(FLOW_LOW_WATERMARK + 1));
        // At (or below) Low the gate reopens on the same call.
        assert!(gate.should_emit(FLOW_LOW_WATERMARK));
    }

    #[test]
    fn emission_gate_reset_to_zero_reopens_immediately() {
        // reset_flow_control() sets acked = sent, i.e. gap == 0: the next
        // chunk must emit — this is the "pane mounted / buffer fetched"
        // recovery path.
        let mut gate = EmissionGate::new();
        assert!(!gate.should_emit(FLOW_HIGH_WATERMARK * 3));
        assert!(gate.should_emit(0));
        // And it stays open afterwards until High is crossed again.
        assert!(gate.should_emit(FLOW_HIGH_WATERMARK));
        assert!(!gate.should_emit(FLOW_HIGH_WATERMARK + 1));
    }

    /// REGRESSION (scan-gate watermark, PR #961 review F1).
    ///
    /// Replays the exact reader-loop interleaving that permanently wedged the
    /// grid scanners: the reader tees a chunk into the scrollback ring — which
    /// bumps `total_bytes_produced` — and is preempted BEFORE it takes the grid
    /// lock. A scanner tick lands in that window, then the reader completes the
    /// parser advance, and the terminal blocks on input so no further byte ever
    /// arrives.
    ///
    /// With the byte counter as the gate, the mid-window tick records a
    /// watermark covering bytes the grid has not drawn, renders the pre-advance
    /// screen, and every later tick compares equal and skips — the prompt is
    /// never seen. With `grid_generation`, the mid-window tick correctly sees
    /// no change, and the post-advance tick MUST scan.
    #[test]
    fn counted_but_unparsed_chunk_does_not_let_the_gate_skip_the_next_scan() {
        use super::super::scan_gate::ScanGate;

        let session = make_test_session(Arc::new(Mutex::new(Vec::new())));
        let mut parser = vte::Parser::new();

        // Two independent gates driven from the same session, so the test
        // compares the OLD signal against the NEW one on identical history.
        let mut byte_gate = ScanGate::new();
        let mut gen_gate = ScanGate::new();

        // Tick 0: first sighting — always scans, and primes both watermarks.
        assert!(byte_gate.should_scan("t", session.total_bytes_produced()));
        assert!(gen_gate.should_scan("t", session.grid_generation()));
        // Tick 1: genuinely idle — both correctly skip.
        assert!(!byte_gate.should_scan("t", session.total_bytes_produced()));
        assert!(!gen_gate.should_scan("t", session.grid_generation()));

        // Reader step 1: the chunk that paints the auto-response prompt is teed
        // into the ring + byte counter. The grid has NOT absorbed it yet.
        let prompt = b"Do you want to proceed? (y/n)";
        tee_into_scrollback(
            &session.scrollback_buffer,
            &session.total_bytes_produced,
            prompt,
            SCROLLBACK_CAPACITY,
        );
        assert!(
            {
                let g = session.grid.lock().unwrap();
                !g.text_snapshot().text.contains("proceed")
            },
            "precondition: the grid must not yet show the prompt"
        );

        // A scanner tick lands in the preemption window.
        let byte_scanned_midwindow = byte_gate.should_scan("t", session.total_bytes_produced());
        let gen_scanned_midwindow = gen_gate.should_scan("t", session.grid_generation());
        assert!(
            byte_scanned_midwindow,
            "the byte counter moved, so the old gate scanned here — and \
             recorded a watermark for bytes the grid had not drawn"
        );
        assert!(
            !gen_scanned_midwindow,
            "the grid did not move, so the generation gate skips — nothing to see yet"
        );

        // Reader step 2: the preempted advance completes.
        advance_grid(&session.grid, &session.grid_generation, &mut parser, prompt);
        assert!(
            {
                let g = session.grid.lock().unwrap();
                g.text_snapshot().text.contains("proceed")
            },
            "the grid now shows the prompt"
        );

        // The terminal is now blocked on input: no further bytes will arrive,
        // so `total_bytes_produced` is frozen at the mid-window value.
        assert!(
            !byte_gate.should_scan("t", session.total_bytes_produced()),
            "THE BUG: the byte-counter gate skips the tick that would have \
             seen the prompt, and keeps skipping forever"
        );
        assert!(
            gen_gate.should_scan("t", session.grid_generation()),
            "THE FIX: the generation counter moved after the advance, so the \
             prompt is scanned"
        );
        // ...and once scanned, the session settles back to skipping.
        assert!(!gen_gate.should_scan("t", session.grid_generation()));
    }

    /// A resize rewrites rendered text without any byte reaching the parser, so
    /// the byte counter cannot see it. The generation counter must.
    #[test]
    fn resize_bumps_the_grid_generation() {
        let session = make_test_session(Arc::new(Mutex::new(Vec::new())));
        let before = session.grid_generation();
        session.resize(100, 40).expect("noop master resize");
        assert!(
            session.grid_generation() > before,
            "resize must rearm the scan gate"
        );
        assert_eq!(
            session.total_bytes_produced(),
            0,
            "no byte reached the parser — the old gate signal is blind to this"
        );
    }
}
