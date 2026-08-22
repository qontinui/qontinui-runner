//! OSC 9999 agent-status sideband — parse, bound, rate-limit, forward.
//!
//! Plan `2026-08-11-coord-hook-sourced-agent-status` §3.3, **Channel 2**.
//!
//! Channel 1 is a Claude Code `PostToolUse` hook that POSTs tool-grain status
//! straight to coord. This is the INDEPENDENT second channel: it rides the VT
//! stream the runner already parses, so a session whose hooks failed to
//! install is *degraded* (coarser, PTY-sourced status) rather than **invisible**.
//!
//! ## Untrusted input
//!
//! The payload arrives over a PTY from whatever is running in the terminal —
//! a hostile or merely buggy program can emit anything at PTY speed. So:
//!
//! * the payload is size-capped in the grid before it ever reaches here
//!   (`grid::MAX_AGENT_STATUS_SIDEBAND_BYTES`), dropped whole rather than
//!   truncated;
//! * it is **never logged raw** — logs carry field presence, never content;
//! * every field is length-capped and, where it has a shape, shape-checked;
//! * an unrecognized `state` is DROPPED rather than forwarded. This is not
//!   defensive politeness: coord's `SessionStatus` has a hand-written
//!   `Deserialize` that **hard-errors** on an unknown word, so forwarding one
//!   would make coord reject the whole `PATCH` body. A sideband must not
//!   generate error traffic.
//! * nothing here can panic on malformed input, and nothing here can block the
//!   PTY reader thread.
//!
//! ## Never a raw tool input
//!
//! The wire carries `tool_input_digest`, never a tool input. That is a
//! deliberate security choice recorded in plan §3.2 — tool inputs routinely
//! contain file contents, paths and secrets, and this channel crosses a
//! machine boundary. Recorded here so it is not "simplified" away later.
//!
//! ## Wire contract
//!
//! ```text
//! ESC ] 9999 ; {"state":"working","tool_name":"Bash",
//!               "tool_input_digest":"9f86d081…","model":"opus"} BEL
//! ```
//!
//! (ST — `ESC \` — terminates equally well.) Every key is optional; a payload
//! that yields no usable field at all is dropped. Anything that is not a JSON
//! object is dropped.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value as JsonValue};
use tracing::{debug, warn};
use uuid::Uuid;

/// Cap on every free-text field we forward (`tool_name`, `model`) and on the
/// digest. Mirrors coord's own column bounds — keeping the runner's cap equal
/// to coord's means an over-length value is trimmed HERE rather than causing a
/// coord-side rejection we would then have to interpret.
const MAX_FIELD_CHARS: usize = 128;

/// Minimum spacing between two sideband enqueues for one session.
///
/// A terminal can emit OSC sequences at PTY speed, and every accepted payload
/// would otherwise become an outbox row AND a coord `PATCH`. The cap protects
/// **both**: the runner's local outbox (unbounded growth, disk) and coord
/// (request volume from a single chatty terminal). Coalescing is latest-wins —
/// superseded payloads are dropped, never queued, because a status is a
/// *level*, not an event stream: only the newest one is true.
const MIN_ENQUEUE_INTERVAL: Duration = Duration::from_secs(2);

/// A parsed, bounded, forwardable agent status.
///
/// Every field is optional and every field that survived parsing is already
/// validated — construct one only via [`parse_sideband`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentStatus {
    /// Coord's `session_status` vocabulary, canonicalized. Only ever one of
    /// `working | blocked | stalled | waiting_human | finished`.
    pub state: Option<String>,
    pub tool_name: Option<String>,
    /// A DIGEST of the tool input — never the input. Hex-shaped, ≤128 chars.
    pub tool_input_digest: Option<String>,
    pub model: Option<String>,
}

impl AgentStatus {
    fn is_empty(&self) -> bool {
        self.state.is_none()
            && self.tool_name.is_none()
            && self.tool_input_digest.is_none()
            && self.model.is_none()
    }

    /// Render the FLAT payload shape a `progress` outbox row carries. The
    /// drain side (`session::coord_sync::progress_body`) is what nests these
    /// under `{progress:{…}}` for `PATCH /sessions/:id`.
    pub fn to_outbox_payload(&self) -> JsonValue {
        let mut map = serde_json::Map::new();
        if let Some(state) = &self.state {
            map.insert("session_status".into(), json!(state));
        }
        if let Some(tool_name) = &self.tool_name {
            map.insert("tool_name".into(), json!(tool_name));
        }
        if let Some(digest) = &self.tool_input_digest {
            map.insert("tool_input_digest".into(), json!(digest));
        }
        if let Some(model) = &self.model {
            map.insert("model".into(), json!(model));
        }
        JsonValue::Object(map)
    }
}

/// Canonicalize a `state` word against coord's shipped `SessionStatus`
/// vocabulary. `None` for anything else — including the empty string.
///
/// The legacy wire word `done` is accepted as `finished`, matching coord's
/// `SessionStatus::parse`. We emit only canonical words, so a `done` on the
/// wire is normalized here rather than passed through.
fn canonical_state(raw: &str) -> Option<&'static str> {
    match raw {
        "working" => Some("working"),
        "blocked" => Some("blocked"),
        "stalled" => Some("stalled"),
        "waiting_human" => Some("waiting_human"),
        "finished" => Some("finished"),
        // Legacy alias — coord accepts it, but we normalize rather than relay.
        "done" => Some("finished"),
        _ => None,
    }
}

/// Read a string field, trimmed, empty→absent, capped to [`MAX_FIELD_CHARS`]
/// **characters** (never bytes — slicing a UTF-8 string at a byte offset would
/// panic, and this is untrusted input).
fn bounded_text(obj: &JsonValue, key: &str) -> Option<String> {
    let raw = obj.get(key)?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.chars().take(MAX_FIELD_CHARS).collect())
}

/// A digest is dropped, never trimmed to fit: a truncated digest is a
/// *different* digest, and a wrong one is worse than none. Must be non-empty
/// hex within the cap.
fn bounded_digest(obj: &JsonValue) -> Option<String> {
    let raw = obj.get("tool_input_digest")?.as_str()?.trim();
    if raw.is_empty() || raw.len() > MAX_FIELD_CHARS {
        return None;
    }
    if !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(raw.to_string())
}

/// Parse one raw sideband payload into a bounded [`AgentStatus`].
///
/// `None` — silently — for: not JSON, JSON that is not an object, and an
/// object from which no field survived validation. Never panics, for any
/// input. An individual bad FIELD is dropped without dropping its siblings;
/// the whole payload is dropped only when nothing usable remains.
pub fn parse_sideband(raw: &str) -> Option<AgentStatus> {
    let value: JsonValue = serde_json::from_str(raw).ok()?;
    if !value.is_object() {
        return None;
    }
    let status = AgentStatus {
        state: value
            .get("state")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .and_then(canonical_state)
            .map(str::to_string),
        tool_name: bounded_text(&value, "tool_name"),
        tool_input_digest: bounded_digest(&value),
        model: bounded_text(&value, "model"),
    };
    if status.is_empty() {
        return None;
    }
    Some(status)
}

// ---- Rate limiting -------------------------------------------------------

/// What the caller must do with an offered status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimiterDecision {
    /// Enqueue this now — the interval had elapsed.
    EmitNow(AgentStatus),
    /// Held as pending (superseding any earlier pending). The caller owes one
    /// [`SidebandRateLimiter::flush`] after `delay`; no flush is scheduled yet.
    Defer { delay: Duration },
    /// Held as pending, and a flush is ALREADY scheduled — do nothing. This is
    /// the arm that makes a burst cost one timer, not one timer per payload.
    Held,
}

/// Leading-edge-plus-trailing-flush coalescer, one per terminal session.
///
/// The first status after a quiet period goes out immediately (status is
/// useless if it is always 2s late); everything that arrives inside the window
/// collapses into a single trailing flush carrying the LATEST value.
///
/// The clock is passed in rather than read internally so the whole policy is
/// unit-testable with no sleeping.
#[derive(Debug, Default)]
pub struct SidebandRateLimiter {
    last_emit: Option<Instant>,
    pending: Option<AgentStatus>,
    flush_scheduled: bool,
}

impl SidebandRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a status at `now`.
    pub fn offer(&mut self, status: AgentStatus, now: Instant) -> LimiterDecision {
        let elapsed_enough = self
            .last_emit
            .is_none_or(|last| now.duration_since(last) >= MIN_ENQUEUE_INTERVAL);
        if elapsed_enough && !self.flush_scheduled {
            self.last_emit = Some(now);
            self.pending = None;
            return LimiterDecision::EmitNow(status);
        }
        // Latest-wins: an earlier pending payload is DROPPED, not queued.
        self.pending = Some(status);
        if self.flush_scheduled {
            return LimiterDecision::Held;
        }
        self.flush_scheduled = true;
        let delay = self
            .last_emit
            .map(|last| MIN_ENQUEUE_INTERVAL.saturating_sub(now.duration_since(last)))
            .unwrap_or(MIN_ENQUEUE_INTERVAL);
        LimiterDecision::Defer { delay }
    }

    /// Redeem a scheduled flush at `now`. `Some` only when a payload is still
    /// pending; the scheduled-flush flag is cleared either way, so the next
    /// `offer` can arm a fresh timer.
    pub fn flush(&mut self, now: Instant) -> Option<AgentStatus> {
        self.flush_scheduled = false;
        let status = self.pending.take()?;
        self.last_emit = Some(now);
        Some(status)
    }
}

// ---- Dispatch ------------------------------------------------------------

/// Handle one drained OSC 9999 payload.
///
/// Called from the PTY reader thread, so it MUST NOT block: parsing is cheap
/// and in-thread, and everything past the limiter (resolving the session
/// registry, writing the outbox row) is handed to the async runtime — the same
/// `tauri::async_runtime::spawn` detachment `context_watcher` uses to keep
/// coord round-trips off the grid tick.
///
/// Silently drops the payload when the terminal has no coord mirror
/// (`coord_session_id` still `None` — registration never happened or failed).
/// No error, no retry: there is nowhere to send it.
pub fn dispatch(
    terminal_id: &str,
    coord_session_id: &Arc<Mutex<Option<Uuid>>>,
    limiter: &Arc<Mutex<SidebandRateLimiter>>,
    raw: String,
) {
    let Some(coord_id) = coord_session_id.lock().ok().and_then(|slot| *slot) else {
        return;
    };
    // NB: `raw` is untrusted PTY bytes and is never logged.
    let Some(status) = parse_sideband(&raw) else {
        debug!(
            terminal = %terminal_id,
            "agent_status_sideband: OSC 9999 payload dropped (unparseable or no usable field)"
        );
        return;
    };

    let decision = {
        let Ok(mut guard) = limiter.lock() else {
            return;
        };
        guard.offer(status, Instant::now())
    };

    match decision {
        LimiterDecision::EmitNow(status) => {
            let terminal_id = terminal_id.to_string();
            tauri::async_runtime::spawn(async move {
                enqueue_progress(&terminal_id, coord_id, &status);
            });
        }
        LimiterDecision::Defer { delay } => {
            let terminal_id = terminal_id.to_string();
            let limiter = limiter.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(delay).await;
                let flushed = limiter
                    .lock()
                    .ok()
                    .and_then(|mut guard| guard.flush(Instant::now()));
                if let Some(status) = flushed {
                    enqueue_progress(&terminal_id, coord_id, &status);
                }
            });
        }
        LimiterDecision::Held => {}
    }
}

/// Write the `progress` outbox row. Drains to
/// `PATCH /sessions/:id {progress:{…}}` via `session::coord_sync` — the
/// runner's EXISTING durable, at-least-once, offline-tolerant coord push path.
/// Best-effort throughout: a sideband must never disturb the live session.
fn enqueue_progress(terminal_id: &str, coord_session_id: Uuid, status: &AgentStatus) {
    use tauri::Manager;

    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(registry) = app
        .try_state::<Arc<crate::session::SessionRegistry>>()
        .map(|s| s.inner().clone())
    else {
        return;
    };

    if let Err(e) = registry.coord_sync().outbox().record(
        registry.machine_id(),
        coord_session_id,
        crate::session::SessionEventKind::Progress,
        status.to_outbox_payload(),
    ) {
        warn!(
            terminal = %terminal_id,
            coord_session = %coord_session_id,
            error = %e,
            "agent_status_sideband: coord progress enqueue failed (best-effort)"
        );
    } else {
        debug!(
            terminal = %terminal_id,
            coord_session = %coord_session_id,
            state = ?status.state,
            "agent_status_sideband: coord progress enqueued"
        );
    }
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: &str) -> AgentStatus {
        AgentStatus {
            state: Some(state.to_string()),
            ..Default::default()
        }
    }

    // ---- parsing ---------------------------------------------------------

    #[test]
    fn parses_the_full_documented_payload() {
        let parsed = parse_sideband(
            r#"{"state":"working","tool_name":"Bash","tool_input_digest":"9f86d081AF","model":"opus"}"#,
        )
        .expect("full payload parses");
        assert_eq!(parsed.state.as_deref(), Some("working"));
        assert_eq!(parsed.tool_name.as_deref(), Some("Bash"));
        assert_eq!(parsed.tool_input_digest.as_deref(), Some("9f86d081AF"));
        assert_eq!(parsed.model.as_deref(), Some("opus"));
    }

    #[test]
    fn accepts_every_word_of_coords_vocabulary() {
        for word in ["working", "blocked", "stalled", "waiting_human", "finished"] {
            let raw = format!(r#"{{"state":"{word}"}}"#);
            assert_eq!(
                parse_sideband(&raw).and_then(|s| s.state).as_deref(),
                Some(word),
                "{word} is coord vocabulary"
            );
        }
    }

    #[test]
    fn legacy_done_maps_to_finished() {
        assert_eq!(
            parse_sideband(r#"{"state":"done"}"#)
                .and_then(|s| s.state)
                .as_deref(),
            Some("finished")
        );
    }

    #[test]
    fn unknown_state_is_dropped_not_forwarded() {
        // Coord's SessionStatus Deserialize HARD-ERRORS on an unknown word, so
        // forwarding one would 422 the whole PATCH body.
        assert!(
            parse_sideband(r#"{"state":"vibing"}"#).is_none(),
            "unknown state, and nothing else usable → whole payload dropped"
        );
        // Case matters — the vocabulary is exact.
        assert!(parse_sideband(r#"{"state":"WORKING"}"#).is_none());
        // …but an unknown state does not take a valid sibling down with it.
        let parsed = parse_sideband(r#"{"state":"vibing","tool_name":"Bash"}"#)
            .expect("sibling field survives");
        assert_eq!(parsed.state, None);
        assert_eq!(parsed.tool_name.as_deref(), Some("Bash"));
    }

    #[test]
    fn over_length_tool_name_and_model_are_capped() {
        let long = "t".repeat(MAX_FIELD_CHARS + 50);
        let raw = format!(r#"{{"tool_name":"{long}","model":"{long}"}}"#);
        let parsed = parse_sideband(&raw).expect("capped, not dropped");
        assert_eq!(
            parsed.tool_name.map(|s| s.chars().count()),
            Some(MAX_FIELD_CHARS)
        );
        assert_eq!(
            parsed.model.map(|s| s.chars().count()),
            Some(MAX_FIELD_CHARS)
        );
    }

    #[test]
    fn multibyte_tool_name_caps_on_a_char_boundary_without_panicking() {
        let long = "é".repeat(MAX_FIELD_CHARS + 10);
        let raw = format!(r#"{{"tool_name":"{long}"}}"#);
        let parsed = parse_sideband(&raw).expect("capped");
        assert_eq!(
            parsed.tool_name.map(|s| s.chars().count()),
            Some(MAX_FIELD_CHARS)
        );
    }

    #[test]
    fn non_hex_or_over_length_digest_is_dropped() {
        // Not hex.
        assert!(
            parse_sideband(r#"{"tool_input_digest":"not-a-digest"}"#).is_none(),
            "non-hex digest dropped, nothing else usable"
        );
        // Over the cap — dropped whole, NOT trimmed (a trimmed digest is a
        // different digest).
        let long_hex = "a".repeat(MAX_FIELD_CHARS + 1);
        let raw = format!(r#"{{"tool_input_digest":"{long_hex}"}}"#);
        assert!(parse_sideband(&raw).is_none());
        // At the cap, hex → kept.
        let at_cap = "a".repeat(MAX_FIELD_CHARS);
        let raw = format!(r#"{{"tool_input_digest":"{at_cap}"}}"#);
        assert_eq!(
            parse_sideband(&raw).and_then(|s| s.tool_input_digest),
            Some(at_cap)
        );
        // A dropped digest does not take its siblings with it.
        let parsed = parse_sideband(r#"{"state":"working","tool_input_digest":"zzz"}"#)
            .expect("state survives");
        assert_eq!(parsed.state.as_deref(), Some("working"));
        assert_eq!(parsed.tool_input_digest, None);
    }

    #[test]
    fn hostile_and_malformed_payloads_are_dropped_without_panicking() {
        let hostile = [
            "",
            " ",
            "not json at all",
            "{",
            "}",
            "[]",
            "[1,2,3]",
            "null",
            "true",
            "42",
            "\"a bare string\"",
            "{}",
            r#"{"state":null}"#,
            r#"{"state":123}"#,
            r#"{"state":{"nested":"object"}}"#,
            r#"{"state":["working"]}"#,
            r#"{"state":""}"#,
            r#"{"state":"   "}"#,
            r#"{"tool_name":""}"#,
            r#"{"tool_name":null,"model":null}"#,
            r#"{"unrelated":"key"}"#,
            r#"{"tool_input":"rm -rf / --no-preserve-root"}"#,
            r#"{"state":"working""#,
            "\u{0}\u{1}\u{2}",
            "\u{feff}{\"state\":\"working\"}",
            r#"{"state":"working "}"#,
        ];
        for raw in hostile {
            // The contract is "no panic"; most of these also parse to None.
            let _ = parse_sideband(raw);
        }
        // Spot-check the ones that MUST be dropped rather than forwarded.
        for raw in [
            "not json at all",
            "[]",
            "null",
            "42",
            "{}",
            r#"{"unrelated":"key"}"#,
            r#"{"tool_input":"rm -rf / --no-preserve-root"}"#,
            r#"{"state":""}"#,
        ] {
            assert!(parse_sideband(raw).is_none(), "must drop: {raw}");
        }
    }

    #[test]
    fn a_raw_tool_input_is_never_carried() {
        // The wire has no `tool_input` key by design (plan §3.2). Prove it is
        // ignored rather than quietly relayed under some other name.
        let parsed = parse_sideband(r#"{"state":"working","tool_input":"secret payload"}"#)
            .expect("state survives");
        let rendered = parsed.to_outbox_payload().to_string();
        assert!(!rendered.contains("secret payload"));
        assert!(!rendered.contains("tool_input\""));
    }

    #[test]
    fn outbox_payload_is_flat_and_omits_absent_fields() {
        let parsed = parse_sideband(r#"{"state":"blocked","tool_name":"Read"}"#).unwrap();
        let payload = parsed.to_outbox_payload();
        assert_eq!(payload["session_status"], json!("blocked"));
        assert_eq!(payload["tool_name"], json!("Read"));
        assert!(payload.get("model").is_none());
        assert!(payload.get("tool_input_digest").is_none());
    }

    // ---- rate limiting ---------------------------------------------------

    #[test]
    fn rate_limiter_emits_the_first_payload_immediately() {
        let mut limiter = SidebandRateLimiter::new();
        let t0 = Instant::now();
        assert_eq!(
            limiter.offer(status("working"), t0),
            LimiterDecision::EmitNow(status("working"))
        );
    }

    #[test]
    fn rate_limiter_coalesces_a_burst_to_one_enqueue_keeping_the_latest() {
        let mut limiter = SidebandRateLimiter::new();
        let t0 = Instant::now();
        assert!(matches!(
            limiter.offer(status("working"), t0),
            LimiterDecision::EmitNow(_)
        ));

        // A PTY-speed burst inside the window: exactly ONE of them arms a
        // timer, the rest are Held, and each supersedes the last.
        let burst = ["blocked", "working", "waiting_human", "stalled", "finished"];
        let mut deferrals = 0;
        for (i, state) in burst.iter().enumerate() {
            let now = t0 + Duration::from_millis(10 * (i as u64 + 1));
            match limiter.offer(status(state), now) {
                LimiterDecision::Defer { delay } => {
                    deferrals += 1;
                    assert!(delay <= MIN_ENQUEUE_INTERVAL);
                }
                LimiterDecision::Held => {}
                LimiterDecision::EmitNow(_) => panic!("burst member escaped the window"),
            }
        }
        assert_eq!(deferrals, 1, "a burst arms exactly one flush timer");

        // The single trailing flush carries the LATEST payload; the four
        // superseded ones were dropped, never queued.
        let flushed = limiter.flush(t0 + MIN_ENQUEUE_INTERVAL);
        assert_eq!(flushed, Some(status("finished")));
        assert_eq!(
            limiter.flush(t0 + MIN_ENQUEUE_INTERVAL),
            None,
            "nothing is left queued behind the latest"
        );
    }

    #[test]
    fn rate_limiter_emits_again_once_the_interval_has_passed() {
        let mut limiter = SidebandRateLimiter::new();
        let t0 = Instant::now();
        assert!(matches!(
            limiter.offer(status("working"), t0),
            LimiterDecision::EmitNow(_)
        ));
        // Just inside the window → deferred.
        assert!(matches!(
            limiter.offer(status("blocked"), t0 + Duration::from_millis(1999)),
            LimiterDecision::Defer { .. }
        ));
        // Redeem the flush, then a later offer emits immediately again.
        let t1 = t0 + MIN_ENQUEUE_INTERVAL;
        assert_eq!(limiter.flush(t1), Some(status("blocked")));
        assert_eq!(
            limiter.offer(status("finished"), t1 + MIN_ENQUEUE_INTERVAL),
            LimiterDecision::EmitNow(status("finished"))
        );
    }

    #[test]
    fn rate_limiter_flush_with_nothing_pending_is_a_noop() {
        let mut limiter = SidebandRateLimiter::new();
        assert_eq!(limiter.flush(Instant::now()), None);
    }
}
