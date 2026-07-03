//! In-session continuation delivery — the runner-side push-executor
//! (plan `2026-06-21-in-session-continuation-delivery.md`, Phase 2).
//!
//! A device-scoped background loop that consumes coord's directed-message
//! mailbox (`GET /coord/session-messages/pending`) and delivers each message
//! as a prompt injected into the *live local session* it targets — queued
//! safely if the session is mid-turn (SDK path), gated on terminal idle for
//! PTY/Worker sessions, then acked via
//! `POST /coord/session-messages/mark-delivered`.
//!
//! ## Why this supersedes `session_bus.rs`
//!
//! `session_bus::spawn_session_bus_executor` (Phase 3b of the older
//! `2026-06-15-inter-session-session-bus.md`) already polled the same mailbox
//! and injected via an HTTP round-trip to the runner's own `submit-prompt`
//! endpoint — but with the hazards this module exists to fix:
//!
//! - **NO idle gate.** It injected into a PTY unconditionally, clobbering a
//!   mid-turn session (its own doc-comment flags "Idle-only refinement ... is
//!   a follow-up"). THIS is the load-bearing fix: turn arbitration must never
//!   clobber a live PTY turn.
//! - **NO dedup / cooldown** beyond the mark-delivered round-trip — a
//!   double-inject window exists before the ack lands.
//! - It bypassed the in-process injection primitive
//!   (`coordinator::act::send_message_to_worker`), which prefers the SDK queue
//!   (safe by construction when `state == Processing`).
//!
//! To avoid TWO executors racing the same mailbox (double injection), the old
//! `session_bus` spawn is retired in `main.rs`; this poller is the single
//! consumer.
//!
//! ## Turn arbitration (the top correctness priority)
//!
//! For each pending message we resolve `to_session` → a live local session and
//! choose a path:
//!
//! - **SDK `ClaudeSession`** — inject immediately. `ClaudeSession::send_user_message`
//!   QUEUES the message when the session is `Processing`
//!   (`claude_session/session.rs`), so it is safe by construction and never
//!   clobbers a turn.
//! - **PTY / `WorkerSession`** — `submit_prompt` writes raw bracketed-paste +
//!   CR with NO state check, so injecting mid-turn corrupts the running turn.
//!   We FIRST check the idle gate ([`terminal_looks_idle`]); only inject when
//!   the terminal is quiescent and showing its input prompt. If not idle we
//!   SKIP this tick (leave the message unacked; retry next poll).
//!
//! ## Safety rails
//!
//! - **Device-JWT-gated.** No device JWT (unpaired) ⇒ the tick is skipped
//!   quietly (no log spam) — same posture as `fleet_policy_poller`.
//! - **Kill-switch.** `RUNNER_SESSION_MESSAGE_DELIVERY_DISABLED=1` parks the
//!   loop (no polling, no injection).
//! - **Fail-open.** Any coord error / decode failure / resolution miss NEVER
//!   injects garbage and never panics the loop — it logs and continues. The
//!   loop is spawned under `task_supervisor::spawn_supervised` so a panic
//!   self-heals.
//! - **Idempotency + rate-limit.** A per-`message_id` delivered-set guards
//!   against a double-inject within a tick or before the ack lands. A
//!   per-`to_session` cooldown debounces a flapping source so it cannot spam a
//!   session.
//! - **Blocked-delivery surfacing.** A `priority="blocking"` message that
//!   stays undeliverable (target not live, or PTY never idle) past
//!   `RUNNER_BLOCKING_MSG_SURFACE_SECS` is reported to coord via a fail-open
//!   `delivery-blocked` POST — evidence for the stall-watchdog supervisor,
//!   never a change to delivery behavior. Gated (default ON) by
//!   `RUNNER_DELIVERY_SURFACING_ENABLED`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::Manager;
use tokio::sync::{watch, Mutex};
use tracing::{debug, info, warn};

use crate::mcp::types::ApiState;

/// How often the loop polls the mailbox. 10s sits in the plan's ~5-15s window
/// — responsive enough that an author's CI-red rework prompt lands within a
/// few seconds, light enough to not hammer coord.
const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Per-`to_session` cooldown. After a successful injection into a session we
/// will not inject AGAIN into the same session for this long, so a flapping
/// source (e.g. a merge wave red-ing many PRs authored by one session) cannot
/// spam a single session with back-to-back prompts. Messages held off by the
/// cooldown stay UNACKED and are retried on a later tick.
const PER_SESSION_COOLDOWN: Duration = Duration::from_secs(30);

/// Idle-gate quiescence debounce. The PTY idle gate reads the grid, waits this
/// long, then reads again; idle requires the input prompt visible AND the
/// rendered text UNCHANGED across both reads (no streaming). Short enough to
/// keep a poll tick snappy, long enough to catch a mid-output frame.
const IDLE_QUIESCENCE_DEBOUNCE: Duration = Duration::from_millis(600);

/// How long a `message_id` lingers in the delivered-set after a successful
/// ack. Long enough to cover coord's mark-delivered eventual consistency (so a
/// just-acked message that momentarily reappears in a `pending` response is not
/// re-injected); short enough that the set does not grow unbounded.
const DELIVERED_SET_TTL: Duration = Duration::from_secs(300);

/// Kill-switch env flag. When set to a truthy value the loop parks (no poll, no
/// inject). Named for the runner's `_DISABLED` opt-out convention (cf.
/// `session_attribution`'s `COORD_SESSION_ATTRIBUTION_ENABLED`, but inverted to
/// a disable flag because in-session delivery is ON by default once paired).
const KILL_SWITCH_ENV: &str = "RUNNER_SESSION_MESSAGE_DELIVERY_DISABLED";

// ===========================================================================
// Wire types (coord response subset)
// ===========================================================================

/// One pending message from `GET /coord/session-messages/pending`. Only the
/// fields the executor needs are deserialized; every optional field defaults
/// so a coord that trims/renames a sibling doesn't break the decode.
#[derive(Debug, Clone, Deserialize)]
struct PendingMessage {
    message_id: String,
    #[serde(default)]
    to_session: Option<String>,
    #[serde(default)]
    from_session: Option<String>,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    priority: String,
    #[serde(default)]
    body: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PendingResponse {
    #[serde(default)]
    messages: Vec<PendingMessage>,
}

// ===========================================================================
// Idempotency + cooldown state
// ===========================================================================

/// Tracks what we've delivered and when we last injected into each session, so
/// a message is never injected twice and a flapping source can't spam a
/// session. Lives for the life of the loop (one per runner).
#[derive(Default)]
struct DeliveryGuard {
    /// `message_id -> Instant we acked it`. Guards re-injection of an
    /// already-delivered message within the [`DELIVERED_SET_TTL`] window
    /// (coord's mark-delivered is eventually consistent). Pruned each tick.
    delivered: HashMap<String, Instant>,
    /// `to_session -> Instant of last successful injection`. Enforces the
    /// per-session [`PER_SESSION_COOLDOWN`].
    last_injected: HashMap<String, Instant>,
}

impl DeliveryGuard {
    /// Drop expired delivered-set entries so the map can't grow unbounded.
    fn prune(&mut self, now: Instant) {
        self.delivered
            .retain(|_, at| now.duration_since(*at) < DELIVERED_SET_TTL);
        // last_injected is bounded by live sessions; prune well past the
        // cooldown so it stays small without churning the common case.
        self.last_injected
            .retain(|_, at| now.duration_since(*at) < PER_SESSION_COOLDOWN * 4);
    }

    /// True if this message was already acked recently (don't re-inject).
    fn already_delivered(&self, message_id: &str) -> bool {
        self.delivered.contains_key(message_id)
    }

    /// True if `to_session` was injected into within the cooldown window.
    fn in_cooldown(&self, to_session: &str, now: Instant) -> bool {
        self.last_injected
            .get(to_session)
            .is_some_and(|at| now.duration_since(*at) < PER_SESSION_COOLDOWN)
    }

    /// Record a successful injection (sets cooldown + delivered mark).
    fn mark_injected(&mut self, to_session: &str, message_id: &str, now: Instant) {
        self.last_injected.insert(to_session.to_string(), now);
        self.delivered.insert(message_id.to_string(), now);
    }
}

// ===========================================================================
// Delivery-blocked surfacing (lost-wakeup fixes 2-3,
// plan 2026-07-03-subagent-stall-watchdog)
// ===========================================================================
//
// A `priority="blocking"` message that cannot be delivered used to fail
// SILENTLY forever: an unresolvable `to_session` just logged-and-waited (until
// the 14d TTL), and a PTY that never passes the idle gate deferred injection
// on every tick with no aging signal. Both are exactly the invisible-stall
// class the stall-watchdog plan exists to kill, so once a blocking message has
// been blocked past `RUNNER_BLOCKING_MSG_SURFACE_SECS` we POST a typed
// delivery-failure to coord (`.../delivery-blocked`) as EVIDENCE for coord's
// expectation supervisor. Delivery behavior itself is UNCHANGED — the POST is
// reporting-only, fail-open, and fired at most once per threshold window per
// message.

/// Flag gating the surfacing POSTs (fixes 2-3). **Default ON** — they are
/// reporting-only (no mutation, no injection change; plan vet decision).
/// `0`/`false`/`no`/`off` disables the POSTs (blocked-time tracking still
/// runs; it is cheap and keeps `blocked_since` honest if re-enabled).
const SURFACING_ENABLED_ENV: &str = "RUNNER_DELIVERY_SURFACING_ENABLED";

/// How long a blocking message must be continuously undeliverable before the
/// first surfacing POST, and the minimum spacing between POSTs for the same
/// message thereafter (once per window). Seconds; env-tunable.
const SURFACE_SECS_ENV: &str = "RUNNER_BLOCKING_MSG_SURFACE_SECS";

/// Default surfacing threshold: 30 minutes.
const SURFACE_SECS_DEFAULT: u64 = 1800;

/// Resolve the surfacing flag from a raw env value. Pure for unit tests.
/// Absent ⇒ ON (default); only an explicit falsy value disables.
fn resolve_surfacing_enabled(raw: Option<&str>) -> bool {
    match raw {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

/// Is delivery-blocked surfacing enabled? (env, default ON)
fn surfacing_enabled() -> bool {
    resolve_surfacing_enabled(std::env::var(SURFACING_ENABLED_ENV).ok().as_deref())
}

/// Resolve the surfacing threshold from a raw env value. Pure for unit tests.
/// Unset / non-numeric ⇒ the 1800s default.
fn resolve_surface_threshold(raw: Option<&str>) -> Duration {
    Duration::from_secs(
        raw.and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(SURFACE_SECS_DEFAULT),
    )
}

/// The configured surfacing threshold (env override, else 1800s).
fn surface_threshold() -> Duration {
    resolve_surface_threshold(std::env::var(SURFACE_SECS_ENV).ok().as_deref())
}

/// Why a blocking message could not be delivered — the typed `reason` the
/// surfacing POST carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BlockReason {
    /// Fix 2: `to_session` does not resolve to a live local session.
    TargetNotLive,
    /// Fix 3: the target PTY keeps failing the idle gate, deferring injection.
    PtyNeverIdle,
}

impl BlockReason {
    /// The wire value for the POST body's `reason` field.
    fn as_str(self) -> &'static str {
        match self {
            BlockReason::TargetNotLive => "target_not_live",
            BlockReason::PtyNeverIdle => "pty_never_idle",
        }
    }
}

/// Pure surfacing decision: should a POST fire NOW for a message first seen
/// blocked at `first_seen`, last surfaced at `last_posted`?
///
/// - Disabled flag ⇒ never.
/// - Blocked for less than `threshold` ⇒ not yet.
/// - Never posted ⇒ fire.
/// - Already posted ⇒ fire again only after a full `threshold` cooldown window
///   since the last POST (at most one POST per message per window).
fn should_surface(
    first_seen: Instant,
    last_posted: Option<Instant>,
    now: Instant,
    threshold: Duration,
    enabled: bool,
) -> bool {
    if !enabled {
        return false;
    }
    if now.duration_since(first_seen) < threshold {
        return false;
    }
    match last_posted {
        None => true,
        Some(at) => now.duration_since(at) >= threshold,
    }
}

/// Per-(message, reason) blocked-delivery bookkeeping.
struct BlockEntry {
    /// Monotonic first-seen, for threshold/cooldown math.
    first_seen: Instant,
    /// Wall-clock first-seen, for the POST's `blocked_since` (RFC 3339).
    first_seen_wall: chrono::DateTime<chrono::Utc>,
    /// Monotonic time of the last surfacing POST attempt (None = never).
    last_posted: Option<Instant>,
}

/// Tracks how long each blocking message has been undeliverable, per reason.
///
/// IN-PROCESS ONLY (restart caveat): a runner restart resets the clock, which
/// only DELAYS surfacing by up to one threshold window — it can never spam,
/// because a fresh map means no entry is past the threshold yet.
#[derive(Default)]
struct SurfacingTracker {
    entries: HashMap<(String, BlockReason), BlockEntry>,
}

impl SurfacingTracker {
    /// Record that `message_id` is blocked for `reason` as of `now`. Returns
    /// `Some(blocked_since)` when a surfacing POST should fire (and stamps the
    /// cooldown so the same window never fires twice), else `None`.
    fn note_blocked(
        &mut self,
        message_id: &str,
        reason: BlockReason,
        now: Instant,
        threshold: Duration,
        enabled: bool,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let entry = self
            .entries
            .entry((message_id.to_string(), reason))
            .or_insert_with(|| BlockEntry {
                first_seen: now,
                first_seen_wall: chrono::Utc::now(),
                last_posted: None,
            });
        if should_surface(entry.first_seen, entry.last_posted, now, threshold, enabled) {
            // Stamp BEFORE the (fail-open) POST attempt: at most one attempt
            // per window even if the POST errors — never a retry storm.
            entry.last_posted = Some(now);
            Some(entry.first_seen_wall)
        } else {
            None
        }
    }

    /// A successful delivery clears every tracking entry for the message.
    fn clear_message(&mut self, message_id: &str) {
        self.entries.retain(|(mid, _), _| mid != message_id);
    }

    /// Drop entries whose message is no longer in coord's pending set (it was
    /// delivered elsewhere, cancelled, or expired) so the map stays bounded.
    fn retain_pending(&mut self, pending_ids: &std::collections::HashSet<&str>) {
        self.entries
            .retain(|(mid, _), _| pending_ids.contains(mid.as_str()));
    }
}

/// Fire the delivery-blocked surfacing POST for `message_id` if it has been
/// blocked past the threshold (once per window). **Fail-open by contract**:
/// any error / non-2xx — including 404 while the coord route (shipped in the
/// plan's PR 3) hasn't landed — is a debug log; delivery behavior is never
/// affected.
///
/// Auth: the same device-JWT bearer the poller's `pending` / `mark-delivered`
/// calls use (the `token` threaded from `deliver_once`).
async fn surface_blocked_delivery(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    tracker: &mut SurfacingTracker,
    message_id: &str,
    reason: BlockReason,
    now: Instant,
) {
    let Some(blocked_since) = tracker.note_blocked(
        message_id,
        reason,
        now,
        surface_threshold(),
        surfacing_enabled(),
    ) else {
        return;
    };
    let Some(device_id) = crate::agent_runtime::load_local_device_id() else {
        debug!(
            "session_message_poller: delivery-blocked surfacing for msg {message_id} \
             skipped — no local device id"
        );
        return;
    };
    let url = format!("{base}/coord/session-messages/{message_id}/delivery-blocked");
    let body = serde_json::json!({
        "device_id": device_id.to_string(),
        "reason": reason.as_str(),
        "blocked_since": blocked_since.to_rfc3339(),
    });
    match client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            info!(
                "session_message_poller: surfaced delivery-blocked msg {message_id} \
                 (reason={}) to coord",
                reason.as_str()
            );
        }
        Ok(resp) => {
            // 404 expected until coord's delivery-blocked route ships (PR 3).
            debug!(
                "session_message_poller: delivery-blocked POST for msg {message_id} \
                 -> HTTP {} (fail-open, continuing)",
                resp.status()
            );
        }
        Err(e) => {
            debug!(
                "session_message_poller: delivery-blocked POST for msg {message_id} \
                 failed: {e} (fail-open, continuing)"
            );
        }
    }
}

// ===========================================================================
// PTY idle gate
// ===========================================================================

/// Working/processing indicators that mean Claude is mid-turn. If ANY appears
/// on the rendered screen the terminal is NOT idle. Lowercased before match.
/// Sourced from the Claude Code TUI working line ("… esc to interrupt") and the
/// spinner glyphs it cycles; conservative — any hit vetoes idle.
const PROCESSING_INDICATORS: &[&str] = &[
    "esc to interrupt",
    "to interrupt)",
    "interrupt)",
    "tokens ·",
    "thinking…",
    "thinking...",
    "compacting",
    "summarizing",
];

/// Spinner glyphs the Claude Code TUI animates while working. Their presence on
/// screen is a strong "busy" signal independent of the text indicators above.
const SPINNER_GLYPHS: &[char] = &['✻', '✶', '✳', '✽', '✢', '·', '∗'];

/// The Claude Code input-prompt marker. When Claude is ready for input the
/// input box shows this caret. (We also accept a bare `>` box-input fallback
/// row in case the glyph is stripped, but `❯` is the canonical tell.)
const PROMPT_MARKER: char = '❯';

/// Decide whether a single rendered grid snapshot looks IDLE / ready for input.
///
/// Pure over the snapshot so it is unit-testable against synthetic grids.
/// CONSERVATIVE: returns false (NOT idle) on any ambiguity. Idle requires ALL:
///
/// 1. NO processing indicator text anywhere on screen
///    ([`PROCESSING_INDICATORS`]).
/// 2. NO spinner glyph on screen ([`SPINNER_GLYPHS`]) — except that the `·`
///    middot is common in static UI, so it only counts when it co-occurs with a
///    working line; we treat it via the text indicators, not as a bare glyph.
/// 3. A prompt row containing [`PROMPT_MARKER`] (`❯`) is visible.
/// 4. The cursor sits AT OR BELOW the prompt row (i.e. in the input area), not
///    up in streaming output.
fn snapshot_looks_idle(lines: &[String], cursor_row: u16) -> bool {
    // (1) any processing-indicator text ⇒ busy.
    let lower: Vec<String> = lines.iter().map(|l| l.to_ascii_lowercase()).collect();
    for line in &lower {
        for ind in PROCESSING_INDICATORS {
            if line.contains(ind) {
                return false;
            }
        }
    }

    // (2) spinner glyphs (excluding bare `·`, handled via text) ⇒ busy.
    for line in lines {
        for ch in line.chars() {
            if ch != '·' && SPINNER_GLYPHS.contains(&ch) {
                return false;
            }
        }
    }

    // (3) prompt marker visible — and remember its row for (4).
    let prompt_row = lines.iter().position(|l| l.contains(PROMPT_MARKER));
    let Some(prompt_row) = prompt_row else {
        // No visible input prompt ⇒ we can't confirm ready-for-input ⇒ not idle.
        return false;
    };

    // (4) cursor in the input area (at/below the prompt row). A cursor up in
    // the scrollback/output region means output is still being drawn.
    (cursor_row as usize) >= prompt_row
}

/// Read a terminal's rendered grid as `(lines, cursor_row)`. Lock-poison
/// tolerant (reads the inner value) so a poisoned grid never wedges the loop.
fn read_grid(session: &crate::terminal::session::TerminalSession) -> (Vec<String>, u16) {
    let grid = session.grid();
    let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
    let snap = guard.text_snapshot();
    (snap.lines, snap.cursor_row)
}

/// PTY idle gate: the terminal looks idle AND has not mutated across a short
/// quiescence debounce. Two reads [`IDLE_QUIESCENCE_DEBOUNCE`] apart must both
/// look idle and render identical text (no streaming between them).
///
/// Async because it sleeps for the debounce; the two grid reads themselves are
/// cheap synchronous lock-and-snapshot calls.
async fn terminal_looks_idle(session: &crate::terminal::session::TerminalSession) -> bool {
    let (lines_a, cursor_a) = read_grid(session);
    if !snapshot_looks_idle(&lines_a, cursor_a) {
        return false;
    }
    tokio::time::sleep(IDLE_QUIESCENCE_DEBOUNCE).await;
    let (lines_b, cursor_b) = read_grid(session);
    if !snapshot_looks_idle(&lines_b, cursor_b) {
        return false;
    }
    // Quiescent: identical render across the debounce ⇒ nothing streaming.
    lines_a == lines_b && cursor_a == cursor_b
}

// ===========================================================================
// Session resolution
// ===========================================================================

/// Where a `to_session` resolved to, and how to inject into it.
enum ResolvedTarget {
    /// SDK `ClaudeSession` — inject immediately; it queues if Processing.
    /// Carries the runner `task_run_id` to pass to `send_message_to_worker`.
    Sdk { task_run_id: String },
    /// PTY/Worker — gate on idle first. Carries the worker's `task_run_id`
    /// (for `send_message_to_worker`) and `terminal_id` (for the grid read).
    Pty {
        task_run_id: String,
        terminal_id: String,
    },
}

/// Resolve coord's `to_session` to a live local session, or `None` if this
/// device is not currently hosting it (leave the message pending — not ours /
/// not live).
///
/// `to_session` is the runner-side session identity coord stores. We resolve it
/// against, in order:
///
/// 1. The durable lifecycle store (`claude_session_id -> terminal_id`, the
///    proven `session_bus` path) — the terminal_id then finds a live
///    WorkerSession (PTY) via `SessionManager::find_worker_by_terminal_id`.
/// 2. A direct SDK `SessionManager::get(to_session)` — covers a session whose
///    runner `task_run_id` IS what coord addressed (SDK sessions).
/// 3. The `AiCoordRegistrar` forward index (coord UUIDv7 → runner
///    `task_run_id`), if `to_session` parses as a coord session UUID — covers
///    agentic SDK sessions registered via the registrar.
///
/// SDK matches win over PTY (the SDK queue is clobber-safe), so we probe (2)/(3)
/// before falling back to the PTY terminal from (1).
fn resolve_target(
    session_manager: &crate::claude_session::SessionManager,
    registrar: Option<&crate::claude_session::coord_register::AiCoordRegistrar>,
    lifecycle_store: &crate::session::session_lifecycle_store::SessionLifecycleStore,
    to_session: &str,
) -> Option<ResolvedTarget> {
    // (2) Direct SDK session keyed by the runner task_run_id == to_session.
    if session_manager.get(to_session).is_some() {
        return Some(ResolvedTarget::Sdk {
            task_run_id: to_session.to_string(),
        });
    }

    // (3) Coord UUIDv7 → runner task_run_id via the registrar.
    if let Some(reg) = registrar {
        if let Ok(uuid) = to_session.parse::<uuid::Uuid>() {
            if let Some(task_run_id) = reg.task_run_id_for(&uuid) {
                if session_manager.get(&task_run_id).is_some() {
                    return Some(ResolvedTarget::Sdk { task_run_id });
                }
                // Resolved to a worker task_run_id?
                if let Some(worker) = session_manager.get_worker(&task_run_id) {
                    return Some(ResolvedTarget::Pty {
                        task_run_id,
                        terminal_id: worker.terminal_id().to_string(),
                    });
                }
            }
        }
    }

    // (1) Lifecycle store: claude_session_id == to_session → terminal_id →
    // live WorkerSession (PTY). This is the proven `session_bus` resolution.
    if let Some(rec) = lifecycle_store.get(to_session) {
        if rec.state == "open" {
            if let Some(worker) = session_manager.find_worker_by_terminal_id(&rec.terminal_id) {
                return Some(ResolvedTarget::Pty {
                    task_run_id: worker.task_run_id().to_string(),
                    terminal_id: rec.terminal_id.clone(),
                });
            }
        }
    }

    None
}

/// Frame the message body as an out-of-band inter-session system-reminder so
/// the recipient knows it's a coord-directed continuation, not operator input.
/// Mirrors the framing the retired `session_bus` used.
fn frame_message(msg: &PendingMessage) -> String {
    let from = msg
        .from_session
        .as_deref()
        .map(|f| format!(", from session {f}"))
        .unwrap_or_default();
    let kind = if msg.kind.is_empty() {
        "directed"
    } else {
        msg.kind.as_str()
    };
    let priority = if msg.priority.is_empty() {
        "normal"
    } else {
        msg.priority.as_str()
    };
    format!(
        "<system-reminder>Session Bus {kind} message ({priority} priority{from}). \
         Act on it, then coord_ack_message message_id={}. Message: {}</system-reminder>",
        msg.message_id, msg.body
    )
}

// ===========================================================================
// Poll loop
// ===========================================================================

/// Is the kill-switch set? `RUNNER_SESSION_MESSAGE_DELIVERY_DISABLED` truthy ⇒
/// the loop parks.
fn killed() -> bool {
    std::env::var(KILL_SWITCH_ENV)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// State for the poller task — shutdown channel + join handle.
pub struct PollerState {
    shutdown_tx: watch::Sender<bool>,
    task_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PollerState {
    /// Stop the poller task, giving it up to 3 seconds to shut down cleanly.
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.task_handle.lock().await.take() {
            match tokio::time::timeout(Duration::from_secs(3), handle).await {
                Ok(_) => info!("session_message_poller stopped gracefully"),
                Err(_) => warn!(
                    "session_message_poller did not stop in 3s; shutdown signal sent, moving on"
                ),
            }
        }
    }
}

/// Spawn the poller task under the supervisor (a panic self-heals — a dead
/// poller would silently stop delivering continuation prompts).
pub fn start_poller(api_state: Arc<ApiState>) -> Arc<PollerState> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let shutdown_rx_loop = shutdown_rx.clone();
    let task_handle = crate::mcp::task_supervisor::spawn_supervised(
        "session_message_poller",
        shutdown_rx,
        move || poller_loop(api_state.clone(), shutdown_rx_loop.clone()),
    );
    Arc::new(PollerState {
        shutdown_tx,
        task_handle: Mutex::new(Some(task_handle)),
    })
}

async fn poller_loop(api_state: Arc<ApiState>, mut shutdown_rx: watch::Receiver<bool>) {
    info!(
        "session_message_poller started (interval={}s, cooldown={}s, kill-switch={KILL_SWITCH_ENV})",
        POLL_INTERVAL.as_secs(),
        PER_SESSION_COOLDOWN.as_secs(),
    );

    let mut guard = DeliveryGuard::default();
    let mut tracker = SurfacingTracker::default();
    // Edge-trigger the "killed" / "unpaired" steady-state logs.
    let mut last_killed_logged = false;

    loop {
        if *shutdown_rx.borrow() {
            info!("session_message_poller shutting down");
            return;
        }

        if killed() {
            if !last_killed_logged {
                info!(
                    "session_message_poller: disabled via {KILL_SWITCH_ENV} — parked (no delivery)"
                );
                last_killed_logged = true;
            }
        } else {
            if last_killed_logged {
                info!("session_message_poller: {KILL_SWITCH_ENV} cleared — resuming delivery");
                last_killed_logged = false;
            }
            // Fail-open: a tick error NEVER panics the loop.
            if let Err(e) = deliver_once(&api_state, &mut guard, &mut tracker).await {
                warn!("session_message_poller: delivery tick failed: {e}");
            }
            guard.prune(Instant::now());
        }

        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("session_message_poller shutting down");
                return;
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// One delivery pass: pull pending → resolve → (idle-gate for PTY) → inject via
/// the in-process primitive → mark delivered. Returns `Err` only for a
/// tick-level failure (no JWT, coord unreachable, decode) — a per-message
/// resolution miss or idle-skip is normal and silent, except that a BLOCKING
/// message blocked past the surfacing threshold fires a (fail-open, once per
/// window) delivery-blocked POST via `tracker`.
async fn deliver_once(
    api_state: &Arc<ApiState>,
    guard: &mut DeliveryGuard,
    tracker: &mut SurfacingTracker,
) -> anyhow::Result<()> {
    // Device JWT — unpaired ⇒ skip the tick quietly (no spam).
    let token = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => {
            debug!("session_message_poller: no device JWT yet (unpaired) — skipping tick");
            return Ok(());
        }
    };

    let base = match crate::mcp::agent_worktrees::coord_http_base() {
        Ok(b) => b,
        Err(e) => {
            debug!("session_message_poller: coord base unresolved ({e}) — skipping tick");
            return Ok(());
        }
    };
    let base = base.trim_end_matches('/').to_string();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    // 1. Pull undelivered messages for this device's sessions (device from JWT).
    let pending_url = format!("{base}/coord/session-messages/pending");
    let resp = client.get(&pending_url).bearer_auth(&token).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("pull {pending_url} -> HTTP {}", resp.status());
    }
    let pending: PendingResponse = resp.json().await?;
    // Drop surfacing entries for messages no longer pending (delivered
    // elsewhere / cancelled / expired) so the tracker stays bounded.
    let pending_ids: std::collections::HashSet<&str> = pending
        .messages
        .iter()
        .map(|m| m.message_id.as_str())
        .collect();
    tracker.retain_pending(&pending_ids);
    if pending.messages.is_empty() {
        return Ok(());
    }

    // 2. Resolution substrate (read once per tick).
    let session_manager = match api_state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        Some(sm) => sm.inner().clone(),
        None => {
            debug!("session_message_poller: SessionManager not available — skipping tick");
            return Ok(());
        }
    };
    let registrar = api_state
        .app_handle
        .try_state::<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
        .map(|r| r.inner().clone());
    let terminal_manager = api_state
        .app_handle
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|t| t.inner().clone());
    let lifecycle_store = crate::session::session_lifecycle_store::SessionLifecycleStore::open(
        lifecycle_store_path(),
    )?;

    let now = Instant::now();
    let mut delivered = 0usize;

    for msg in &pending.messages {
        // Idempotency: never re-inject an already-acked message (covers
        // coord's eventual-consistency reappearance + within-tick dup).
        if guard.already_delivered(&msg.message_id) {
            continue;
        }
        let Some(to_session) = msg.to_session.as_deref() else {
            continue; // unaddressed (Phase 4 resolve) — not this executor's job
        };

        // Rate-limit: a session in cooldown waits — leave the message pending.
        if guard.in_cooldown(to_session, now) {
            debug!(
                "session_message_poller: session {to_session} in cooldown — deferring msg {}",
                msg.message_id
            );
            continue;
        }

        let Some(target) = resolve_target(
            &session_manager,
            registrar.as_deref(),
            &lifecycle_store,
            to_session,
        ) else {
            // Not live on this device. Leave pending — delivered on its next
            // open (its spawn preamble pulls coord_inbox), or by another
            // device hosting it.
            if msg.priority == "blocking" {
                info!(
                    "session_message_poller: BLOCKING msg {} for session {to_session} not live \
                     here — pending until it next opens",
                    msg.message_id
                );
                // Fix 2: a blocking message unresolvable past the threshold is
                // surfaced to coord (once per window, fail-open).
                surface_blocked_delivery(
                    &client,
                    &base,
                    &token,
                    tracker,
                    &msg.message_id,
                    BlockReason::TargetNotLive,
                    now,
                )
                .await;
            } else {
                debug!(
                    "session_message_poller: msg {} target {to_session} not live — pending",
                    msg.message_id
                );
            }
            continue;
        };

        // Turn arbitration: SDK queues safely; PTY must be idle.
        let task_run_id = match &target {
            ResolvedTarget::Sdk { task_run_id } => task_run_id.clone(),
            ResolvedTarget::Pty {
                task_run_id,
                terminal_id,
            } => {
                let Some(tm) = terminal_manager.as_ref() else {
                    debug!("session_message_poller: TerminalManager unavailable — skip PTY inject");
                    continue;
                };
                let Some(term) = tm.get(terminal_id) else {
                    debug!(
                        "session_message_poller: terminal {terminal_id} gone — skip msg {}",
                        msg.message_id
                    );
                    continue;
                };
                if !terminal_looks_idle(&term).await {
                    debug!(
                        "session_message_poller: terminal {terminal_id} not idle — deferring msg {}",
                        msg.message_id
                    );
                    // Fix 3: a blocking message whose PTY never quiesces past
                    // the threshold is surfaced to coord as evidence for the
                    // stuck-not-dead classifier (once per window, fail-open).
                    // Injection behavior is unchanged — we still just defer.
                    if msg.priority == "blocking" {
                        surface_blocked_delivery(
                            &client,
                            &base,
                            &token,
                            tracker,
                            &msg.message_id,
                            BlockReason::PtyNeverIdle,
                            now,
                        )
                        .await;
                    }
                    continue;
                }
                task_run_id.clone()
            }
        };

        // 3. Inject via the in-process primitive (reuses the SDK queue / PTY
        // submit dispatch — no second injection primitive).
        let framed = frame_message(msg);
        crate::coordinator::act::send_message_to_worker(api_state, &task_run_id, &framed).await;

        // 4. Mark delivered. Record locally FIRST (cooldown + delivered-set)
        // so even if the ack POST fails we won't re-inject within the TTL.
        guard.mark_injected(to_session, &msg.message_id, now);
        // A successful delivery clears the message's blocked-surfacing state.
        tracker.clear_message(&msg.message_id);

        let mark_url = format!("{base}/coord/session-messages/mark-delivered");
        if let Err(e) = client
            .post(&mark_url)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "message_id": msg.message_id }))
            .send()
            .await
        {
            warn!(
                "session_message_poller: injected msg {} but mark-delivered failed: {e} \
                 (local delivered-set prevents re-inject for {}s)",
                msg.message_id,
                DELIVERED_SET_TTL.as_secs()
            );
        } else {
            delivered += 1;
            info!(
                "session_message_poller: delivered msg {} to session {to_session}",
                msg.message_id
            );
        }
    }

    if delivered > 0 {
        debug!("session_message_poller: delivered {delivered} message(s) this tick");
    }
    Ok(())
}

/// The runner's lifecycle store path (`~/.qontinui/runner/terminal-sessions.json`).
/// Mirrors `session_bus::lifecycle_store_path` and the boot path in `main.rs`.
fn lifecycle_store_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
        .join("terminal-sessions.json")
}

// ===========================================================================
// Boot entry — mirrors fleet_policy_poller::commands
// ===========================================================================

pub mod commands {
    use super::*;
    use std::sync::OnceLock;

    static POLLER_STATE: OnceLock<tokio::sync::Mutex<Option<Arc<PollerState>>>> = OnceLock::new();

    fn get_holder() -> &'static tokio::sync::Mutex<Option<Arc<PollerState>>> {
        POLLER_STATE.get_or_init(|| tokio::sync::Mutex::new(None))
    }

    /// Idempotent start. If a live task already exists, no-op. If the prior
    /// task ended, restart. Wired beside `auto_start_fleet_policy_poller` in
    /// `mcp_api::start_server` — runs ONCE per runner (device-scoped),
    /// supervised, regardless of agents. The loop itself no-ops while unpaired
    /// or while the kill-switch is set, so spawning unconditionally is safe.
    pub async fn auto_start_session_message_poller(api_state: Arc<ApiState>) {
        let mut holder = get_holder().lock().await;

        if let Some(ref existing) = *holder {
            let handle_guard = existing.task_handle.lock().await;
            let is_alive = handle_guard.as_ref().is_some_and(|h| !h.is_finished());
            drop(handle_guard);
            if is_alive {
                info!("session_message_poller already running; leaving it");
                return;
            }
            info!("session_message_poller task has ended, restarting...");
            existing.stop().await;
            *holder = None;
        }

        info!("Starting session_message_poller");
        let state = start_poller(api_state);
        *holder = Some(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|s| s.to_string()).collect()
    }

    // ---- idle gate: snapshot predicate ----------------------------------

    #[test]
    fn idle_when_prompt_visible_and_quiescent() {
        // Canonical ready-for-input frame: an input box with the ❯ caret and
        // the cursor on the prompt row, no working indicators.
        let grid = lines(&[
            "Some earlier output line.",
            "Another line of a finished turn.",
            "",
            "╭──────────────────────────────────────────╮",
            "│ ❯                                          │",
            "╰──────────────────────────────────────────╯",
        ]);
        // cursor on the prompt row (row 4, 0-indexed).
        assert!(snapshot_looks_idle(&grid, 4));
    }

    #[test]
    fn not_idle_when_working_indicator_present() {
        // The Claude Code working line — even with a ❯ elsewhere, the
        // "esc to interrupt" veto wins.
        let grid = lines(&[
            "✻ Thinking…",
            "  Reticulating splines… (esc to interrupt)",
            "│ ❯                                          │",
        ]);
        assert!(!snapshot_looks_idle(&grid, 2));
    }

    #[test]
    fn not_idle_when_spinner_glyph_present() {
        // A spinner glyph alone (no text indicator) still vetoes idle.
        let grid = lines(&[
            "✶ Working",
            "│ ❯                                          │",
        ]);
        assert!(!snapshot_looks_idle(&grid, 1));
    }

    #[test]
    fn not_idle_when_no_prompt_marker() {
        // Streaming output, no input box yet ⇒ can't confirm ready ⇒ not idle.
        let grid = lines(&[
            "Here is a long answer still being written",
            "and another line of output",
            "and more output",
        ]);
        assert!(!snapshot_looks_idle(&grid, 2));
    }

    #[test]
    fn not_idle_when_cursor_above_prompt_row() {
        // Prompt visible but the cursor is up in the output region — output is
        // still being drawn above the (stale) input box.
        let grid = lines(&[
            "streaming output line being drawn",
            "│ ❯                                          │",
        ]);
        // cursor on row 0, prompt on row 1 ⇒ cursor ABOVE prompt ⇒ not idle.
        assert!(!snapshot_looks_idle(&grid, 0));
    }

    #[test]
    fn middot_glyph_alone_does_not_veto_idle() {
        // A bare `·` middot is common static UI chrome; it must NOT, by itself,
        // mark a session busy (only the "tokens ·" text indicator does).
        let grid = lines(&[
            "Context · 42% used",
            "│ ❯                                          │",
        ]);
        assert!(snapshot_looks_idle(&grid, 1));
    }

    #[test]
    fn tokens_middot_text_indicator_vetoes_idle() {
        // The working line's "<n> tokens ·" form IS a busy indicator.
        let grid = lines(&[
            "  12.3k tokens · esc to interrupt",
            "│ ❯                                          │",
        ]);
        assert!(!snapshot_looks_idle(&grid, 1));
    }

    // ---- delivery guard: dedup + cooldown -------------------------------

    #[test]
    fn delivered_message_is_not_redelivered() {
        let mut g = DeliveryGuard::default();
        let now = Instant::now();
        assert!(!g.already_delivered("m1"));
        g.mark_injected("sess-a", "m1", now);
        assert!(g.already_delivered("m1"));
    }

    #[test]
    fn session_in_cooldown_after_injection() {
        let mut g = DeliveryGuard::default();
        let now = Instant::now();
        assert!(!g.in_cooldown("sess-a", now));
        g.mark_injected("sess-a", "m1", now);
        // Immediately after, still in cooldown.
        assert!(g.in_cooldown("sess-a", now));
        // A different session is unaffected.
        assert!(!g.in_cooldown("sess-b", now));
    }

    #[test]
    fn cooldown_expires_after_window() {
        let mut g = DeliveryGuard::default();
        let past = Instant::now() - (PER_SESSION_COOLDOWN + Duration::from_secs(1));
        g.mark_injected("sess-a", "m1", past);
        assert!(!g.in_cooldown("sess-a", Instant::now()));
    }

    #[test]
    fn prune_drops_expired_delivered_entries() {
        let mut g = DeliveryGuard::default();
        let old = Instant::now() - (DELIVERED_SET_TTL + Duration::from_secs(1));
        g.delivered.insert("old".to_string(), old);
        g.delivered.insert("fresh".to_string(), Instant::now());
        g.prune(Instant::now());
        assert!(!g.already_delivered("old"));
        assert!(g.already_delivered("fresh"));
    }

    // ---- kill-switch + cadence ------------------------------------------

    #[test]
    fn poll_interval_in_5_to_15s_window() {
        let s = POLL_INTERVAL.as_secs();
        assert!(
            (5..=15).contains(&s),
            "poll interval {s}s out of 5-15s window"
        );
    }

    #[test]
    fn frame_message_carries_ack_instruction() {
        let msg = PendingMessage {
            message_id: "m-42".to_string(),
            to_session: Some("sess".to_string()),
            from_session: Some("author".to_string()),
            kind: "ci_red_triage".to_string(),
            priority: "blocking".to_string(),
            body: "Your PR is red, please rework.".to_string(),
        };
        let framed = frame_message(&msg);
        assert!(framed.contains("coord_ack_message message_id=m-42"));
        assert!(framed.contains("ci_red_triage"));
        assert!(framed.contains("blocking priority"));
        assert!(framed.contains("from session author"));
        assert!(framed.contains("Your PR is red"));
    }

    #[test]
    fn frame_message_defaults_blank_kind_and_priority() {
        let msg = PendingMessage {
            message_id: "m-1".to_string(),
            to_session: Some("sess".to_string()),
            from_session: None,
            kind: String::new(),
            priority: String::new(),
            body: "hi".to_string(),
        };
        let framed = frame_message(&msg);
        assert!(framed.contains("directed message"));
        assert!(framed.contains("normal priority"));
        assert!(!framed.contains("from session"));
    }

    // ---- delivery-blocked surfacing (fixes 2-3) ---------------------------

    const THRESH: Duration = Duration::from_secs(1800);

    // NOTE: all instants below are built ADDITIVELY from a fresh `Instant::now()`
    // base (`base + offset`), never `Instant::now() - big_offset` — `Instant`
    // subtraction panics on underflow, and on a freshly booted CI VM the
    // monotonic clock's zero point can be closer than the offsets used here.

    #[test]
    fn surfacing_disabled_flag_never_fires() {
        let first_seen = Instant::now();
        let now = first_seen + THRESH * 3;
        assert!(!should_surface(first_seen, None, now, THRESH, false));
    }

    #[test]
    fn surfacing_waits_for_threshold() {
        let first_seen = Instant::now();
        // Just became blocked — not yet.
        assert!(!should_surface(first_seen, None, first_seen, THRESH, true));
        // Blocked one second short of the threshold — still not yet.
        let now = first_seen + THRESH - Duration::from_secs(1);
        assert!(!should_surface(first_seen, None, now, THRESH, true));
        // Past the threshold, never posted — fire.
        let now = first_seen + THRESH + Duration::from_secs(1);
        assert!(should_surface(first_seen, None, now, THRESH, true));
    }

    #[test]
    fn surfacing_cooldown_is_once_per_window() {
        let first_seen = Instant::now();
        let now = first_seen + THRESH * 3;
        // Posted moments ago — the same window must NOT fire again.
        let just_posted = now - Duration::from_secs(5);
        assert!(!should_surface(
            first_seen,
            Some(just_posted),
            now,
            THRESH,
            true
        ));
        // A full window since the last POST — fires again (next window).
        let window_ago = now - THRESH;
        assert!(should_surface(
            first_seen,
            Some(window_ago),
            now,
            THRESH,
            true
        ));
    }

    #[test]
    fn tracker_fires_once_then_cools_down_then_fires_next_window() {
        let mut t = SurfacingTracker::default();
        let t0 = Instant::now();
        // First sighting: entry created, nothing fires (below threshold).
        assert!(t
            .note_blocked("m1", BlockReason::TargetNotLive, t0, THRESH, true)
            .is_none());
        // Past the threshold: fires exactly once...
        let t1 = t0 + THRESH + Duration::from_secs(1);
        let since = t.note_blocked("m1", BlockReason::TargetNotLive, t1, THRESH, true);
        assert!(since.is_some(), "first over-threshold sighting must fire");
        // ...and the immediate next tick is in cooldown.
        let t2 = t1 + Duration::from_secs(10);
        assert!(t
            .note_blocked("m1", BlockReason::TargetNotLive, t2, THRESH, true)
            .is_none());
        // A full window later it fires again, carrying the SAME blocked_since
        // (first-seen is never reset by a POST).
        let t3 = t1 + THRESH;
        let again = t.note_blocked("m1", BlockReason::TargetNotLive, t3, THRESH, true);
        assert_eq!(
            again, since,
            "blocked_since must remain the first-seen time"
        );
    }

    #[test]
    fn tracker_reasons_are_tracked_independently() {
        let mut t = SurfacingTracker::default();
        let t0 = Instant::now();
        let t1 = t0 + THRESH + Duration::from_secs(1);
        // target_not_live aged past the threshold...
        t.note_blocked("m1", BlockReason::TargetNotLive, t0, THRESH, true);
        assert!(t
            .note_blocked("m1", BlockReason::TargetNotLive, t1, THRESH, true)
            .is_some());
        // ...but a FRESH pty_never_idle sighting of the same message starts
        // its own clock and does not fire yet.
        assert!(t
            .note_blocked("m1", BlockReason::PtyNeverIdle, t1, THRESH, true)
            .is_none());
    }

    #[test]
    fn successful_delivery_clears_tracking() {
        let mut t = SurfacingTracker::default();
        let t0 = Instant::now();
        t.note_blocked("m1", BlockReason::TargetNotLive, t0, THRESH, true);
        t.note_blocked("m1", BlockReason::PtyNeverIdle, t0, THRESH, true);
        t.note_blocked("m2", BlockReason::TargetNotLive, t0, THRESH, true);
        t.clear_message("m1");
        // m1's clocks restart from scratch; m2 is untouched.
        let t1 = t0 + THRESH + Duration::from_secs(1);
        assert!(
            t.note_blocked("m1", BlockReason::TargetNotLive, t1, THRESH, true)
                .is_none(),
            "delivery must reset m1's first-seen clock"
        );
        assert!(
            t.note_blocked("m2", BlockReason::TargetNotLive, t1, THRESH, true)
                .is_some(),
            "m2's clock must be unaffected by m1's delivery"
        );
    }

    #[test]
    fn retain_pending_drops_vanished_messages() {
        let mut t = SurfacingTracker::default();
        let t0 = Instant::now();
        t.note_blocked("gone", BlockReason::TargetNotLive, t0, THRESH, true);
        t.note_blocked("kept", BlockReason::PtyNeverIdle, t0, THRESH, true);
        let pending: std::collections::HashSet<&str> = ["kept"].into_iter().collect();
        t.retain_pending(&pending);
        assert!(!t
            .entries
            .contains_key(&("gone".to_string(), BlockReason::TargetNotLive)));
        assert!(t
            .entries
            .contains_key(&("kept".to_string(), BlockReason::PtyNeverIdle)));
    }

    #[test]
    fn surfacing_env_parsing_defaults() {
        // Flag: absent ⇒ ON; only explicit falsy values disable.
        assert!(resolve_surfacing_enabled(None));
        assert!(resolve_surfacing_enabled(Some("1")));
        assert!(resolve_surfacing_enabled(Some("weird")));
        assert!(!resolve_surfacing_enabled(Some("0")));
        assert!(!resolve_surfacing_enabled(Some("false")));
        assert!(!resolve_surfacing_enabled(Some("No")));
        assert!(!resolve_surfacing_enabled(Some(" off ")));
        // Threshold: default 1800s, numeric override, garbage ⇒ default.
        assert_eq!(resolve_surface_threshold(None), Duration::from_secs(1800));
        assert_eq!(
            resolve_surface_threshold(Some("60")),
            Duration::from_secs(60)
        );
        assert_eq!(
            resolve_surface_threshold(Some("nope")),
            Duration::from_secs(1800)
        );
    }

    #[test]
    fn block_reason_wire_values() {
        // Pins the coord route contract (`POST .../delivery-blocked` body).
        assert_eq!(BlockReason::TargetNotLive.as_str(), "target_not_live");
        assert_eq!(BlockReason::PtyNeverIdle.as_str(), "pty_never_idle");
    }
}
