//! Always-on transport counters for terminal output (plan
//! `2026-09-20-terminal-output-transport-is-unmeasured-encoded-broadcast`,
//! Phase 1, "in a temp runner" layer).
//!
//! Process-global, relaxed-atomic counters on the PTY reader / fan-out path,
//! served by `GET /terminals/transport-stats` and reset by
//! `POST /terminals/transport-stats` (`mcp::terminals`). Every record is a
//! handful of `fetch_add(Relaxed)`s. The one path that does more is the waste
//! classification ([`classify_encode`]), and only for an encode the webview
//! did NOT consume: it makes two NON-BLOCKING lock attempts — the server-mode
//! `RwLock::try_read` and the attach-grant table's `Mutex::try_lock`
//! (`RemoteAttachGrants::has_bound_try`) — and walks the grant map without
//! collecting. If either lock is held it gives up and counts no waste. No
//! counter path blocks the PTY reader thread or allocates. (The encode that
//! [`encode`] times allocates its output `String`; that is the cost being
//! measured, not the counter's.)
//!
//! # What each counter measures, exactly
//!
//! Unless stated otherwise a `bytes` counter is RAW payload bytes (PTY bytes
//! after the output interceptor), never base64 characters; `encode.bytes_out`
//! is the one base64-length counter, so `bytes_out / bytes_in` is the
//! encoding's inflation.
//!
//! - `reader.chunks` / `reader.bytes` — every successful `read()` on a PTY
//!   reader thread, and the byte count that read returned (≤ 8 KiB each).
//! - `encode.*` — every base64 encode the terminal output path performs for a
//!   live leg: the shared per-frame encode in the reader's `emit_impl`, the
//!   background/unwatched hold-window flush encodes (reader thread, visibility
//!   sweeper, exit drain). `ns` is `Instant`-timed around `STANDARD.encode`
//!   alone. The scrollback-ring replay encode is NOT here — it is
//!   `ring_replay`.
//! - `encode.frame_count` — the shared per-frame encodes alone (one per
//!   `emit_impl` call that encoded): the population `encode.waste` is drawn
//!   from. `count` also includes hold-window flush encodes, which are never
//!   waste-eligible, so the waste RATE is `waste / frame_count`, not
//!   `waste / count`.
//! - `encode.waste` — the K2 counter. Counts shared per-frame encodes (the
//!   `emit_impl` encode only) for which ALL of the following held at encode
//!   time:
//!     1. the webview leg did not consume that encode (its admission was
//!        `Hold` or `Skip`, not `Now`), AND
//!     2. the SSE leg was either not taken, or taken while no external
//!        (non-pipe) output subscriber existed anywhere in the process — i.e.
//!        its only receivers were in-process coord output pipes, which decode
//!        the base64 straight back to bytes. External subscribers are the
//!        `GET /terminals/{id}/ws` streams, counted process-wide by
//!        [`ExternalOutputSubscriber`]; while any one is live, NO SSE-leg
//!        encode counts as waste (a process-wide count, so this undercounts
//!        rather than overcounts), AND
//!     3. the WS leg was either not taken, or taken while the backend relay's
//!        own flood-control predicate would drop the frame: the live
//!        `ServerModeState::terminal_subscriber_count()` is 0 (or no server
//!        mode is installed) AND `remote_terminal::grants().has_bound_try`
//!        finds no unexpired grant bound to this terminal. That is the same
//!        pair of facts `mcp::backend_relay` reads when it discards a
//!        `terminal-output` frame. If either lock is momentarily held the
//!        encode is NOT counted (unknown ≠ waste).
//!
//!   Limits, stated rather than hidden: (a) a bound grant whose remote flow
//!   gate is paused makes the relay withhold the frame too, but that path is
//!   not counted (reading it has a side effect — it marks the gate skipped);
//!   (b) `event_broadcast` receivers OTHER than the relay (local
//!   `/ws/events` and `/events` clients, GraphQL subscriptions, task-run SSE,
//!   the cascade buffer task) are not distinguished — a `terminal-output`
//!   frame consumed only by such a local client would be counted as waste.
//!   Phase 2 replaces `receiver_count()` with counted interest registration,
//!   which removes (b).
//! - `legs.sse` — frames sent on the per-session `broadcast::Sender<String>`
//!   (the SSE leg; its receivers are coord output pipes and `/terminals/{id}/ws`
//!   streams), and their raw bytes.
//! - `legs.ws` — frames handed to `broadcast_ws_notification` as
//!   `terminal-output` (the relay leg), and their raw bytes. Counted at the
//!   hand-off, whether or not the relay later drops them.
//! - `legs.webview` — every `emit_terminal_output` call (live frames, hold
//!   flushes, resume markers), the `Instant`-timed ns spent inside
//!   `AppHandle::emit` (serialization + IPC dispatch to every webview), and
//!   the raw bytes those events carry (computed exactly from the base64
//!   length).
//! - `pipe.*` — the coord output pipe (`session::output_pipe`): chunks
//!   received from the SSE leg, ns spent base64-decoding them, ns spent in
//!   `redact_secrets` (only when redaction is on), and ns spent re-encoding
//!   its coalesced buffer at flush.
//! - `ring_replay.*` — `terminal_get_scrollback` calls and the raw ring bytes
//!   each returned (the whole ring, every call).
//! - `frame_size_hist` — raw size of every frame the reader path hands to the
//!   legs (after DEC-2026 sync coalescing: one per `emit_impl` call).
//!   `counts[i]` is frames with `size <= bounds[i]` and `> bounds[i-1]`;
//!   `counts[6]` is `> 262144`. `counts[4..]` together are the `> 49152`
//!   hazard: a frame whose base64 exceeds the web relay's 65 536-character
//!   `terminal_output` truncation.
//!
//! Counters are individually atomic, not mutually consistent: a snapshot taken
//! while output flows can see a chunk counted in one field and not yet in the
//! next. At the rates measured that skew is one frame.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Upper bounds (inclusive) of the frame-size histogram buckets; the last
/// bucket is unbounded. 49 152 raw bytes is 65 536 base64 characters — the
/// web relay's truncation point.
pub const FRAME_SIZE_BOUNDS: [u64; 6] = [1024, 4096, 16384, 49152, 65536, 262144];
const BUCKETS: usize = FRAME_SIZE_BOUNDS.len() + 1;

/// Which live leg a frame was handed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leg {
    Sse,
    Ws,
}

/// One set of counters. The process uses the single [`GLOBAL`] instance; tests
/// build their own so parallel tests cannot race each other's counts.
pub struct TransportStats {
    epoch: OnceLock<Instant>,
    reset_at_ms: AtomicU64,
    reader_chunks: AtomicU64,
    reader_bytes: AtomicU64,
    encode_count: AtomicU64,
    encode_ns: AtomicU64,
    encode_bytes_in: AtomicU64,
    encode_bytes_out: AtomicU64,
    encode_waste: AtomicU64,
    encode_frame_count: AtomicU64,
    sse_chunks: AtomicU64,
    sse_bytes: AtomicU64,
    ws_chunks: AtomicU64,
    ws_bytes: AtomicU64,
    webview_emits: AtomicU64,
    webview_emit_ns: AtomicU64,
    webview_bytes: AtomicU64,
    pipe_chunks: AtomicU64,
    pipe_decode_ns: AtomicU64,
    pipe_redact_ns: AtomicU64,
    pipe_reencode_ns: AtomicU64,
    ring_calls: AtomicU64,
    ring_bytes: AtomicU64,
    hist: [AtomicU64; BUCKETS],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReaderSnapshot {
    pub chunks: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EncodeSnapshot {
    pub count: u64,
    pub ns: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub waste: u64,
    /// Shared per-frame encodes — the denominator for `waste`.
    pub frame_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegSnapshot {
    pub chunks: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebviewSnapshot {
    pub emits: u64,
    pub emit_ns: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegsSnapshot {
    pub sse: LegSnapshot,
    pub ws: LegSnapshot,
    pub webview: WebviewSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipeSnapshot {
    pub chunks: u64,
    pub decode_ns: u64,
    pub redact_ns: u64,
    pub reencode_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RingReplaySnapshot {
    pub calls: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistSnapshot {
    pub bounds: [u64; 6],
    pub counts: [u64; BUCKETS],
}

/// The `GET /terminals/transport-stats` payload. Key names are a contract with
/// the TypeScript perf harness (`scripts/perf-harness.mjs`); do not rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TransportSnapshot {
    pub since_reset_ms: u64,
    pub reader: ReaderSnapshot,
    pub encode: EncodeSnapshot,
    pub legs: LegsSnapshot,
    pub pipe: PipeSnapshot,
    pub ring_replay: RingReplaySnapshot,
    pub frame_size_hist: HistSnapshot,
}

fn ns(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// Index of the histogram bucket a frame of `size` raw bytes falls in.
pub fn bucket_index(size: u64) -> usize {
    FRAME_SIZE_BOUNDS
        .iter()
        .position(|&bound| size <= bound)
        .unwrap_or(FRAME_SIZE_BOUNDS.len())
}

/// Exact decoded length of a padded STANDARD base64 string.
fn decoded_len(encoded: &str) -> u64 {
    let len = encoded.len();
    let padding = encoded.bytes().rev().take(2).filter(|&b| b == b'=').count();
    ((len / 4) * 3).saturating_sub(padding) as u64
}

impl TransportStats {
    pub const fn new() -> Self {
        Self {
            epoch: OnceLock::new(),
            reset_at_ms: AtomicU64::new(0),
            reader_chunks: AtomicU64::new(0),
            reader_bytes: AtomicU64::new(0),
            encode_count: AtomicU64::new(0),
            encode_ns: AtomicU64::new(0),
            encode_bytes_in: AtomicU64::new(0),
            encode_bytes_out: AtomicU64::new(0),
            encode_waste: AtomicU64::new(0),
            encode_frame_count: AtomicU64::new(0),
            sse_chunks: AtomicU64::new(0),
            sse_bytes: AtomicU64::new(0),
            ws_chunks: AtomicU64::new(0),
            ws_bytes: AtomicU64::new(0),
            webview_emits: AtomicU64::new(0),
            webview_emit_ns: AtomicU64::new(0),
            webview_bytes: AtomicU64::new(0),
            pipe_chunks: AtomicU64::new(0),
            pipe_decode_ns: AtomicU64::new(0),
            pipe_redact_ns: AtomicU64::new(0),
            pipe_reencode_ns: AtomicU64::new(0),
            ring_calls: AtomicU64::new(0),
            ring_bytes: AtomicU64::new(0),
            hist: [const { AtomicU64::new(0) }; BUCKETS],
        }
    }

    /// Milliseconds since this instance was first touched — the time base
    /// `since_reset_ms` is measured on.
    fn now_ms(&self) -> u64 {
        let epoch = self.epoch.get_or_init(Instant::now);
        u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub fn record_reader_chunk(&self, bytes: usize) {
        // Seed the `since_reset_ms` time base without reading the clock.
        self.epoch.get_or_init(Instant::now);
        self.reader_chunks.fetch_add(1, Relaxed);
        self.reader_bytes.fetch_add(bytes as u64, Relaxed);
    }

    pub fn record_frame(&self, bytes: usize) {
        self.hist[bucket_index(bytes as u64)].fetch_add(1, Relaxed);
    }

    pub fn record_encode(&self, bytes_in: usize, bytes_out: usize, took: Duration) {
        self.encode_count.fetch_add(1, Relaxed);
        self.encode_ns.fetch_add(ns(took), Relaxed);
        self.encode_bytes_in.fetch_add(bytes_in as u64, Relaxed);
        self.encode_bytes_out.fetch_add(bytes_out as u64, Relaxed);
    }

    pub fn record_waste(&self) {
        self.encode_waste.fetch_add(1, Relaxed);
    }

    /// One shared per-frame encode (the waste-eligible population).
    pub fn record_frame_encode(&self) {
        self.encode_frame_count.fetch_add(1, Relaxed);
    }

    pub fn record_leg(&self, leg: Leg, bytes: usize) {
        let (chunks, total) = match leg {
            Leg::Sse => (&self.sse_chunks, &self.sse_bytes),
            Leg::Ws => (&self.ws_chunks, &self.ws_bytes),
        };
        chunks.fetch_add(1, Relaxed);
        total.fetch_add(bytes as u64, Relaxed);
    }

    pub fn record_webview_emit(&self, raw_bytes: u64, took: Duration) {
        self.webview_emits.fetch_add(1, Relaxed);
        self.webview_emit_ns.fetch_add(ns(took), Relaxed);
        self.webview_bytes.fetch_add(raw_bytes, Relaxed);
    }

    pub fn record_pipe_chunk(&self, decode: Duration) {
        self.pipe_chunks.fetch_add(1, Relaxed);
        self.pipe_decode_ns.fetch_add(ns(decode), Relaxed);
    }

    pub fn record_pipe_redact(&self, took: Duration) {
        self.pipe_redact_ns.fetch_add(ns(took), Relaxed);
    }

    pub fn record_pipe_reencode(&self, took: Duration) {
        self.pipe_reencode_ns.fetch_add(ns(took), Relaxed);
    }

    pub fn record_ring_replay(&self, bytes: usize) {
        self.ring_calls.fetch_add(1, Relaxed);
        self.ring_bytes.fetch_add(bytes as u64, Relaxed);
    }

    /// Read (`take == false`) or read-and-zero (`take == true`) every counter.
    fn collect(&self, take: bool) -> TransportSnapshot {
        let read = |c: &AtomicU64| if take { c.swap(0, Relaxed) } else { c.load(Relaxed) };
        let now = self.now_ms();
        let since_reset_ms = if take {
            now.saturating_sub(self.reset_at_ms.swap(now, Relaxed))
        } else {
            now.saturating_sub(self.reset_at_ms.load(Relaxed))
        };
        let mut counts = [0u64; BUCKETS];
        for (slot, counter) in counts.iter_mut().zip(self.hist.iter()) {
            *slot = read(counter);
        }
        TransportSnapshot {
            since_reset_ms,
            reader: ReaderSnapshot {
                chunks: read(&self.reader_chunks),
                bytes: read(&self.reader_bytes),
            },
            encode: EncodeSnapshot {
                count: read(&self.encode_count),
                ns: read(&self.encode_ns),
                bytes_in: read(&self.encode_bytes_in),
                bytes_out: read(&self.encode_bytes_out),
                waste: read(&self.encode_waste),
                frame_count: read(&self.encode_frame_count),
            },
            legs: LegsSnapshot {
                sse: LegSnapshot {
                    chunks: read(&self.sse_chunks),
                    bytes: read(&self.sse_bytes),
                },
                ws: LegSnapshot {
                    chunks: read(&self.ws_chunks),
                    bytes: read(&self.ws_bytes),
                },
                webview: WebviewSnapshot {
                    emits: read(&self.webview_emits),
                    emit_ns: read(&self.webview_emit_ns),
                    bytes: read(&self.webview_bytes),
                },
            },
            pipe: PipeSnapshot {
                chunks: read(&self.pipe_chunks),
                decode_ns: read(&self.pipe_decode_ns),
                redact_ns: read(&self.pipe_redact_ns),
                reencode_ns: read(&self.pipe_reencode_ns),
            },
            ring_replay: RingReplaySnapshot {
                calls: read(&self.ring_calls),
                bytes: read(&self.ring_bytes),
            },
            frame_size_hist: HistSnapshot {
                bounds: FRAME_SIZE_BOUNDS,
                counts,
            },
        }
    }

    pub fn snapshot(&self) -> TransportSnapshot {
        self.collect(false)
    }

    /// Zero every counter and return what they held just before.
    pub fn reset(&self) -> TransportSnapshot {
        self.collect(true)
    }
}

impl Default for TransportStats {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide counters the runner serves.
pub static GLOBAL: TransportStats = TransportStats::new();

/// Live `GET /terminals/{id}/ws` output streams, process-wide. Read by
/// [`classify_encode`] to tell "the SSE leg's only receivers are coord output
/// pipes" from "someone outside the process is reading it".
static EXTERNAL_OUTPUT_SUBSCRIBERS: AtomicU64 = AtomicU64::new(0);

/// RAII registration of one external (non-pipe) consumer of a terminal's
/// per-session output broadcast. Hold it for the life of the stream.
#[must_use = "the registration ends when the guard drops"]
pub struct ExternalOutputSubscriber(());

impl ExternalOutputSubscriber {
    pub fn acquire() -> Self {
        EXTERNAL_OUTPUT_SUBSCRIBERS.fetch_add(1, Relaxed);
        Self(())
    }
}

impl Drop for ExternalOutputSubscriber {
    fn drop(&mut self) {
        EXTERNAL_OUTPUT_SUBSCRIBERS.fetch_sub(1, Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Call-site helpers — one line each at the instrumented sites.
// ---------------------------------------------------------------------------

pub fn record_reader_chunk(bytes: usize) {
    GLOBAL.record_reader_chunk(bytes);
}

pub fn record_frame(bytes: usize) {
    GLOBAL.record_frame(bytes);
}

pub fn record_leg(leg: Leg, bytes: usize) {
    GLOBAL.record_leg(leg, bytes);
}

pub fn record_ring_replay(bytes: usize) {
    GLOBAL.record_ring_replay(bytes);
}

/// `STANDARD.encode`, timed and counted. The terminal output path's live-leg
/// encodes go through here so `encode.*` sees every one of them.
pub fn encode(payload: &[u8]) -> String {
    let start = Instant::now();
    let encoded = STANDARD.encode(payload);
    GLOBAL.record_encode(payload.len(), encoded.len(), start.elapsed());
    encoded
}

/// Time one webview emission. `encoded` is the base64 payload the event
/// carries; its raw length is recovered exactly.
pub fn record_webview_emit(encoded: &str, took: Duration) {
    GLOBAL.record_webview_emit(decoded_len(encoded), took);
}

/// Base64-decode one pipe chunk, timed and counted.
pub fn pipe_decode(encoded: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let start = Instant::now();
    let out = STANDARD.decode(encoded.as_bytes());
    GLOBAL.record_pipe_chunk(start.elapsed());
    out
}

/// Run the pipe's redaction, timed.
pub fn pipe_redact(raw: &[u8]) -> Vec<u8> {
    let start = Instant::now();
    let out = crate::session::redact::redact_secrets(raw);
    GLOBAL.record_pipe_redact(start.elapsed());
    out
}

/// The pipe's flush re-encode, timed.
pub fn pipe_reencode(buffer: &[u8]) -> String {
    let start = Instant::now();
    let out = STANDARD.encode(buffer);
    GLOBAL.record_pipe_reencode(start.elapsed());
    out
}

/// Pure waste predicate — see the module doc, `encode.waste`.
///
/// `relay_would_drop` is `Some(true)` when the relay's flood control would
/// discard this terminal's frame, `Some(false)` when it would forward it, and
/// `None` when that could not be read without blocking (never waste).
pub fn is_waste(
    encoded: bool,
    webview_took_it: bool,
    to_sse: bool,
    external_sse_subscribers: u64,
    to_ws: bool,
    relay_would_drop: impl FnOnce() -> Option<bool>,
) -> bool {
    if !encoded || webview_took_it {
        return false;
    }
    if to_sse && external_sse_subscribers > 0 {
        return false;
    }
    if !to_ws {
        return true;
    }
    relay_would_drop().unwrap_or(false)
}

/// The relay's flood-control predicate for one terminal, read without
/// blocking: `Some(true)` = no terminal subscriber and no bound grant.
fn relay_would_drop(app: &AppHandle, terminal_id: &str) -> Option<bool> {
    let state = app.try_state::<Arc<crate::commands::AppState>>()?;
    let subscribed = {
        let guard = state.server_mode.try_read().ok()?;
        guard
            .as_ref()
            .map(|sm| sm.terminal_subscriber_count() > 0)
            .unwrap_or(false)
    };
    if subscribed {
        return Some(false);
    }
    let bound = crate::mcp::remote_terminal::grants()
        .has_bound_try(terminal_id, crate::mcp::remote_terminal::now_epoch_secs())?;
    Some(!bound)
}

/// Classify one reader-path encode for the waste counter. Cheap when the
/// webview consumed the encode (returns before any read).
pub fn classify_encode(
    app: &AppHandle,
    terminal_id: &str,
    encoded: bool,
    webview_took_it: bool,
    to_sse: bool,
    to_ws: bool,
) {
    if encoded {
        GLOBAL.record_frame_encode();
    }
    let external = EXTERNAL_OUTPUT_SUBSCRIBERS.load(Relaxed);
    if is_waste(encoded, webview_took_it, to_sse, external, to_ws, || {
        relay_would_drop(app, terminal_id)
    }) {
        GLOBAL.record_waste();
    }
}

pub fn snapshot() -> TransportSnapshot {
    GLOBAL.snapshot()
}

pub fn reset() -> TransportSnapshot {
    GLOBAL.reset()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_boundaries_are_inclusive_upper_bounds() {
        assert_eq!(bucket_index(0), 0);
        assert_eq!(bucket_index(1024), 0);
        assert_eq!(bucket_index(1025), 1);
        assert_eq!(bucket_index(4096), 1);
        assert_eq!(bucket_index(16384), 2);
        assert_eq!(bucket_index(49152), 3);
        // The relay-truncation hazard starts one byte past 49 152.
        assert_eq!(bucket_index(49153), 4);
        assert_eq!(bucket_index(65536), 4);
        assert_eq!(bucket_index(65537), 5);
        assert_eq!(bucket_index(262144), 5);
        assert_eq!(bucket_index(262145), 6);
        assert_eq!(bucket_index(u64::MAX), 6);
    }

    #[test]
    fn snapshot_reads_without_zeroing_and_reset_zeroes() {
        let s = TransportStats::new();
        s.record_reader_chunk(100);
        s.record_reader_chunk(28);
        s.record_frame(50_000);
        s.record_frame(10);
        s.record_encode(3, 4, Duration::from_nanos(7));
        s.record_waste();
        s.record_frame_encode();
        s.record_frame_encode();
        s.record_leg(Leg::Sse, 11);
        s.record_leg(Leg::Ws, 13);
        s.record_leg(Leg::Ws, 1);
        s.record_webview_emit(6, Duration::from_nanos(9));
        s.record_pipe_chunk(Duration::from_nanos(2));
        s.record_pipe_redact(Duration::from_nanos(3));
        s.record_pipe_reencode(Duration::from_nanos(4));
        s.record_ring_replay(1 << 20);

        let a = s.snapshot();
        assert_eq!(a.reader, ReaderSnapshot { chunks: 2, bytes: 128 });
        assert_eq!(
            a.encode,
            EncodeSnapshot {
                count: 1,
                ns: 7,
                bytes_in: 3,
                bytes_out: 4,
                waste: 1,
                frame_count: 2,
            }
        );
        assert_eq!(a.legs.sse, LegSnapshot { chunks: 1, bytes: 11 });
        assert_eq!(a.legs.ws, LegSnapshot { chunks: 2, bytes: 14 });
        assert_eq!(a.legs.webview, WebviewSnapshot { emits: 1, emit_ns: 9, bytes: 6 });
        assert_eq!(
            a.pipe,
            PipeSnapshot { chunks: 1, decode_ns: 2, redact_ns: 3, reencode_ns: 4 }
        );
        assert_eq!(a.ring_replay, RingReplaySnapshot { calls: 1, bytes: 1 << 20 });
        assert_eq!(a.frame_size_hist.counts, [1, 0, 0, 0, 1, 0, 0]);
        // A plain read leaves the counters where they were.
        assert_eq!(s.snapshot().reader, a.reader);

        let pre = s.reset();
        assert_eq!(pre.reader, a.reader, "reset returns the pre-reset values");
        let after = s.snapshot();
        assert_eq!(after.reader, ReaderSnapshot { chunks: 0, bytes: 0 });
        assert_eq!(after.encode.count, 0);
        assert_eq!(after.encode.waste, 0);
        assert_eq!(after.encode.frame_count, 0);
        assert_eq!(after.frame_size_hist.counts, [0; BUCKETS]);
        assert_eq!(after.ring_replay.calls, 0);
        assert!(after.since_reset_ms <= pre.since_reset_ms + 1_000);
    }

    #[test]
    fn snapshot_serializes_to_the_harness_contract() {
        let v = serde_json::to_value(TransportStats::new().snapshot()).unwrap();
        for path in [
            "/since_reset_ms",
            "/reader/chunks",
            "/reader/bytes",
            "/encode/count",
            "/encode/ns",
            "/encode/bytes_in",
            "/encode/bytes_out",
            "/encode/waste",
            "/encode/frame_count",
            "/legs/sse/chunks",
            "/legs/sse/bytes",
            "/legs/ws/chunks",
            "/legs/ws/bytes",
            "/legs/webview/emits",
            "/legs/webview/emit_ns",
            "/legs/webview/bytes",
            "/pipe/chunks",
            "/pipe/decode_ns",
            "/pipe/redact_ns",
            "/pipe/reencode_ns",
            "/ring_replay/calls",
            "/ring_replay/bytes",
        ] {
            assert!(v.pointer(path).and_then(|x| x.as_u64()).is_some(), "{path} missing");
        }
        assert_eq!(
            v.pointer("/frame_size_hist/bounds").unwrap(),
            &serde_json::json!([1024, 4096, 16384, 49152, 65536, 262144])
        );
        assert_eq!(
            v.pointer("/frame_size_hist/counts").and_then(|c| c.as_array()).map(Vec::len),
            Some(7)
        );
    }

    #[test]
    fn decoded_len_is_exact_for_every_padding() {
        for n in 0..20usize {
            let raw = vec![0xA5u8; n];
            assert_eq!(decoded_len(&STANDARD.encode(&raw)), n as u64, "n={n}");
        }
    }

    #[test]
    fn waste_predicate_matches_the_documented_rule() {
        let never = || -> Option<bool> { panic!("relay predicate must not be read") };
        // Not encoded, or consumed by the webview: never waste, no relay read.
        assert!(!is_waste(false, false, true, 0, true, never));
        assert!(!is_waste(true, true, true, 0, true, never));
        // SSE with an external subscriber anywhere: not waste.
        assert!(!is_waste(true, false, true, 1, false, never));
        // Only the pipe on SSE, no WS leg: waste.
        assert!(is_waste(true, false, true, 0, false, never));
        // WS leg the relay would drop: waste (with or without the pipe).
        assert!(is_waste(true, false, true, 0, true, || Some(true)));
        assert!(is_waste(true, false, false, 0, true, || Some(true)));
        // WS leg the relay forwards: not waste.
        assert!(!is_waste(true, false, true, 0, true, || Some(false)));
        // Unknown relay state is not waste.
        assert!(!is_waste(true, false, false, 0, true, || None));
        // An external SSE subscriber settles it before the WS read.
        assert!(!is_waste(true, false, true, 2, true, never));
    }

    #[test]
    fn encode_helper_counts_into_the_global() {
        // The global is shared with every other test in the process, so assert
        // on a lower bound of the delta rather than an exact value.
        let before = GLOBAL.snapshot().encode;
        let out = encode(b"hello");
        assert_eq!(out, "aGVsbG8=");
        let after = GLOBAL.snapshot().encode;
        assert!(after.count > before.count);
        assert!(after.bytes_in >= before.bytes_in + 5);
        assert!(after.bytes_out >= before.bytes_out + 8);
    }

    #[test]
    fn external_subscriber_guard_is_counted_while_held() {
        let base = EXTERNAL_OUTPUT_SUBSCRIBERS.load(Relaxed);
        let g = ExternalOutputSubscriber::acquire();
        // No other test touches this counter, so the delta is exact.
        assert_eq!(EXTERNAL_OUTPUT_SUBSCRIBERS.load(Relaxed), base + 1);
        drop(g);
        assert_eq!(EXTERNAL_OUTPUT_SUBSCRIBERS.load(Relaxed), base);
    }
}
