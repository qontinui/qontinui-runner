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
            if let Err(e) = deliver_once(&api_state, &mut guard).await {
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
/// resolution miss or idle-skip is normal and silent.
async fn deliver_once(api_state: &Arc<ApiState>, guard: &mut DeliveryGuard) -> anyhow::Result<()> {
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
}
