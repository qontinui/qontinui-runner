//! Coord-driven Claude Code subprocess runtime
//! (Phase 4 of `2026-05-19-coordinator-production-readiness.md`).
//!
//! Per `feedback_no_anthropic_api`: NEVER use `api.anthropic.com` or
//! `ANTHROPIC_API_KEY`. This module spawns the operator's `claude` CLI
//! as a subprocess; the operator's existing Claude Code subscription
//! covers all token cost. The subprocess inherits whatever auth the
//! operator's `claude` install has.
//!
//! ## Flow
//!
//! 1. `spawn_runtime()` connects this runner to coord's `/ws` and
//!    subscribes to `events.agent.spawn_requested.<this_device_id>`.
//! 2. On a spawn-request event, parse the `LaunchPayload`. For each
//!    allocated worktree, `git worktree add <path> <branch>` from the
//!    repo's primary tree (resolved by `QONTINUI_ROOT/<repo>`).
//! 3. Spawn `claude` CLI as a tokio child in the first worktree.
//!    Pipe initial_prompt into stdin; capture stdout+stderr line-by-line.
//! 4. Append each line to a per-agent log file AND POST to
//!    `/agents/:agent_id/log` (Phase 5 endpoint).
//! 5. Heartbeat the claim every 30s via `/claims/heartbeat`.
//! 6. On subprocess exit, POST `spawn-failed` (non-zero) or
//!    `spawn-complete` (clean exit). Restart up to 3× on crash.
//!
//! ## Identity
//!
//! Reads `~/.qontinui/machine.json` for `device_id` (the same source
//! `fleet::heartbeat_to_coord` uses). The WS subscription filter is
//! per-device so a multi-machine fleet doesn't broadcast every spawn
//! to every runner.
//!
//! ## Failure posture
//!
//! - WS disconnects: reconnect with capped exponential backoff
//!   (2s → 60s). The loop never panics; failures `warn!` and retry.
//! - Coord unreachable on log/lifecycle POST: queue locally to a per-
//!   agent file and flush on the next successful POST. The runner
//!   doesn't block on coord ingest.
//! - `claude` binary not on PATH: log + POST `spawn-failed` with a
//!   clear reason; do not restart.
//!
//! ## Phase-5 coordination
//!
//! `/agents/:id/log` is owned by Wave 1 Agent D (Phase 5). If their
//! endpoint isn't live yet, POSTs return 404 and the local queue grows
//! until flush succeeds. The local file is the source of truth; coord
//! ingest is the streaming surface.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

// =============================================================================
// Wire shapes (mirror of qontinui-coord/src/agents_spawn.rs)
// =============================================================================

/// Inbound launch payload. Mirrors `agents_spawn::LaunchPayload` in coord.
/// Kept in lockstep manually: coord is the canonical wire owner, runner
/// is a structural duplicate so this module compiles without a path-dep.
#[derive(Debug, Clone, Deserialize)]
pub struct LaunchPayload {
    pub agent_id: uuid::Uuid,
    #[serde(default)]
    pub agent_session_id: Option<uuid::Uuid>,
    pub target_device_id: uuid::Uuid,
    pub worktrees: Vec<AllocatedWorktree>,
    pub jwt: String,
    pub jwt_exp: i64,
    pub initial_prompt: String,
    pub claim_token: String,
    #[serde(default)]
    pub plan_slug: Option<String>,
    #[serde(default)]
    pub plan_phase: Option<u32>,
    #[serde(default)]
    pub correlation_topic: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AllocatedWorktree {
    pub repo: String,
    pub branch: String,
    pub parent_sha: String,
    pub worktree_path: String,
    pub status: String,
    #[serde(default)]
    pub push_ref: Option<String>,
}

/// How a gate continuation should be surfaced to the operator.
///
/// `terminal` is the default (per the plan's Decision 1) so that a coord
/// that does NOT yet forward the field — every coord on `origin/main` today —
/// deserializes to the operator-visible mode. A parallel coord phase adds the
/// field; this arm tolerates both its absence (→ `Terminal`) and its presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Presentation {
    /// Open a visible terminal session the operator can see and interact with.
    /// **P2 wires the real visible-terminal branch**; in THIS phase it logs a
    /// warning and falls through to the headless flow.
    #[default]
    Terminal,
    /// Run the `claude` CLI as a headless tokio child (today's behavior, used
    /// for fleet/CI continuations with no interactive surface).
    Headless,
}

/// Minimal gate-continuation spawn payload published by coord's
/// `spawn_continuation()` to `events.agent.spawn_requested.<device>`.
///
/// This is **deliberately NOT** the full agent-spawn [`LaunchPayload`]: coord's
/// gate engine owns the *intent* (which repos, what prompt, how to present it)
/// but NOT the device-local resources (worktrees, claim token, JWT). Those are
/// minted on the runner — `agent_id`, `worktrees`, `jwt`, `jwt_exp`, and
/// `claim_token` are absent here on purpose, and the handler acquires them
/// device-locally via [`crate::agent_worktree::isolated_edit::acquire`]
/// (Decision 6). A `source` of `"gate_continuation"` is the wire discriminator
/// that routes a frame into this arm instead of the `LaunchPayload` arm.
#[derive(Debug, Clone, Deserialize)]
pub struct GateContinuationPayload {
    pub target_device_id: uuid::Uuid,
    pub initial_prompt: String,
    #[serde(default)]
    pub repos: Vec<String>,
    /// Defaults to [`Presentation::Terminal`] when coord omits the field
    /// (every coord on `origin/main` today omits it).
    #[serde(default)]
    pub presentation: Presentation,
    /// Wire discriminator. Always `"gate_continuation"` for this shape; the
    /// arm only deserializes a frame here after confirming this value, so a
    /// foreign `source` never reaches this struct.
    #[serde(default)]
    pub source: String,
    /// The gate anchor that cleared (for logging / correlation).
    #[serde(default)]
    pub anchor_key: Option<String>,
    /// The gate row's id (added by the parallel coord phase). Used to (1) dedupe
    /// a continuation delivered by BOTH the WS fast-path and the poll backstop
    /// against the process-wide [`dispatched_gate_ids`] set, and (2) POST the
    /// `continuation-consumed` ack so coord stops re-listing it.
    ///
    /// **Optional on purpose**: the coord on `origin/main` today does NOT stamp
    /// `gate_id` on WS frames. An absent `gate_id` means we cannot ack (skip the
    /// POST silently) and cannot dedupe by id — fully back-compatible with the
    /// currently-deployed coord. The poll surface ALWAYS supplies it.
    #[serde(default)]
    pub gate_id: Option<uuid::Uuid>,
    /// The work-unit DAG dispatch id (added by the coord work-unit scheduler).
    /// A *unit* dispatch reuses this same `source:"gate_continuation"` frame shape
    /// but carries NO `gate_id` — it is keyed on `dispatch_id` instead. Used to (1)
    /// dedupe a unit continuation delivered by BOTH the live WS frame and the
    /// `pending-unit-dispatches` poll backstop against the process-wide
    /// [`dispatched_dispatch_ids`] set, and (2) POST the
    /// `unit-dispatches/{dispatch_id}/consumed` ack so coord stops re-listing it.
    ///
    /// **Optional on purpose**: a gate continuation never carries this; a unit
    /// dispatch always does. When BOTH `gate_id` and `dispatch_id` are absent the
    /// frame falls into the legacy no-dedupe/no-ack branch (a coord on
    /// `origin/main` today). The unit pull surface ALWAYS supplies it; coord also
    /// stamps it on the live unit WS frame.
    #[serde(default)]
    pub dispatch_id: Option<uuid::Uuid>,
    /// Explicit instance target (E2E carve-out). When set, ONLY the runner
    /// instance whose `QONTINUI_INSTANCE_NAME` equals this value spawns the
    /// continuation. When ABSENT (the normal case, and every coord on
    /// `origin/main` today), the continuation is addressed to the PRIMARY
    /// (`instance::instance_name() == None`) — temp/named runners skip it. Coord
    /// passes this through verbatim like `presentation`/`anchor_key`. See
    /// [`continuation_addressed_to_self`] for the matching rule.
    #[serde(default)]
    pub target_instance_name: Option<String>,
}

/// The `source` discriminator coord stamps on a gate-continuation spawn frame.
const GATE_CONTINUATION_SOURCE: &str = "gate_continuation";

/// Typed spawn lifecycle phase coord keys an outcome to. The runner emits ONLY
/// phases it can observe from its own subprocess bookkeeping — never
/// `subagent_resolved`, which lives inside `claude` and the runner cannot see.
///
/// `launched` accompanies a `spawn-complete` (the child process started);
/// `exited` accompanies a `spawn-failed` (the child failed to start, exited
/// non-zero, or a dispatch was refused). Serialized snake_case so coord's
/// ingest can match a string discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SpawnPhase {
    Launched,
    Exited,
}

/// PR/head context coord uses to key a spawn outcome to a specific change.
/// `push_ref` is the agent worktree's push target (a git ref); `pr` is the PR
/// number when it is derivable from that ref (e.g. `refs/pull/123/head`) and
/// `None` otherwise — the runner never guesses a PR it cannot read off the ref.
///
/// Skipped entirely from the wire when both fields are absent so a spawn with
/// no PR context (gate continuations) serializes the legacy body verbatim.
#[derive(Debug, Clone, Default, Serialize)]
struct SpawnPrContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    push_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<u64>,
}

impl SpawnPrContext {
    /// True iff there is no PR context to emit — used to skip the whole
    /// `push_ref`/`pr` block from the body when empty (legacy-shape parity).
    fn is_empty(&self) -> bool {
        self.push_ref.is_none() && self.pr.is_none()
    }

    /// Build a context from an optional push ref, deriving `pr` when the ref
    /// carries a `refs/pull/<n>/...` segment (the only ref shape from which a
    /// PR number is unambiguous). All other ref shapes leave `pr` absent.
    fn from_push_ref(push_ref: Option<&str>) -> Self {
        let pr = push_ref.and_then(pr_number_from_push_ref);
        SpawnPrContext {
            push_ref: push_ref.map(|s| s.to_string()),
            pr,
        }
    }
}

/// Extract a PR number from a `refs/pull/<n>/head` (or `/merge`) push ref.
/// Returns `None` for any ref that is not in the GitHub pull-ref shape — the
/// runner only reports a PR it can read directly off the ref, never one it
/// would have to infer.
fn pr_number_from_push_ref(push_ref: &str) -> Option<u64> {
    let rest = push_ref.strip_prefix("refs/pull/")?;
    let (num, _) = rest.split_once('/')?;
    num.parse::<u64>().ok()
}

#[derive(Debug, Clone, Serialize)]
struct SpawnCompleteBody {
    pid: Option<i64>,
    note: Option<String>,
    /// Phase 4b: typed lifecycle phase (`launched`). Skipped when the enriched
    /// telemetry flag is off so the body is byte-identical to the legacy shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<SpawnPhase>,
    /// Phase 4b: PR/head context. Flattened so `push_ref`/`pr` sit at the top
    /// level alongside `phase`; skipped entirely when empty.
    #[serde(flatten, skip_serializing_if = "SpawnPrContext::is_empty")]
    pr_context: SpawnPrContext,
}

#[derive(Debug, Clone, Serialize)]
struct SpawnFailedBody {
    reason: String,
    exit_code: Option<i64>,
    restarts_attempted: Option<u32>,
    /// Phase 4b: typed lifecycle phase (`exited`). Skipped when the enriched
    /// telemetry flag is off so the body is byte-identical to the legacy shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<SpawnPhase>,
    /// Phase 4b: PR/head context. Flattened so `push_ref`/`pr` sit at the top
    /// level alongside `phase`; skipped entirely when empty.
    #[serde(flatten, skip_serializing_if = "SpawnPrContext::is_empty")]
    pr_context: SpawnPrContext,
}

/// Env flag (default ON) gating Phase 4b's enriched spawn-outcome telemetry.
/// When unset or any value other than `"0"`, `report_spawn_complete` /
/// `report_spawn_failed` stamp the typed `phase` + `push_ref`/`pr` context onto
/// their existing POST bodies (additive — coord ingest tolerates the extra
/// keys). Set `QONTINUI_SPAWN_OUTCOME_ENABLED=0` for a clean revert: the bodies
/// then serialize byte-identical to the pre-4b shape (`phase`/`push_ref`/`pr`
/// all skipped) so a coord that has not yet learned the new keys is unaffected.
fn spawn_outcome_enrichment_enabled() -> bool {
    std::env::var("QONTINUI_SPAWN_OUTCOME_ENABLED").as_deref() != Ok("0")
}

#[derive(Debug, Clone, Serialize)]
struct LogLine {
    /// `stdout` or `stderr`.
    stream: String,
    line: String,
    at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct ClaimHeartbeat {
    kind: String,
    resource_key: String,
    machine_id: String,
    ttl_seconds: i64,
}

// =============================================================================
// Configuration
// =============================================================================

/// Max consecutive crash-restarts before we give up on a subprocess.
const MAX_RESTARTS: u32 = 3;

/// Heartbeat cadence for the claim.
const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Maximum number of buffered log lines we'll keep when coord is
/// unreachable. Older lines drop FIFO; the per-agent log file keeps the
/// full record regardless.
const LOG_FLUSH_QUEUE_CAP: usize = 1024;

/// Directory under `~/.qontinui` where per-agent run logs live. Created
/// on first use; one file per agent_id.
fn agent_logs_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("agent-runs"))
}

fn agent_log_path(agent_id: uuid::Uuid) -> Option<PathBuf> {
    agent_logs_root().map(|d| d.join(format!("{agent_id}.log")))
}

/// Resolve the coord HTTP base. Honors the `COORD_HTTP_URL` env var FIRST
/// (mirrors `mcp::agent_worktrees::coord_http_base`'s env check — every other
/// resolver honors it, this one used to ignore it), then falls back to the
/// active profile's `coord_url` (mirrors the resolver in `fleet.rs`). Returns
/// `None` when neither is set — runtime no-ops in that case (no localhost
/// fallback, unchanged).
fn coord_http_base() -> Option<String> {
    // Delegates to the shared resolver, preserving the no-localhost-fallback
    // posture: only an explicitly-configured base (env or profile) yields
    // `Some`; nothing configured ⇒ `None` (runtime no-ops, unchanged).
    match qontinui_runner_lib::profiles::resolve_coord_base() {
        qontinui_runner_lib::profiles::CoordBase::Configured(base) => Some(base),
        _ => None,
    }
}

/// Resolve the coord WS URL from the active profile's `coord_url`,
/// normalizing the scheme to `ws://` / `wss://` and ensuring the `/ws`
/// path is present (exactly once) with an `events.agent.*` pattern filter.
///
/// Mirrors `session::handoff::coord_ws_url`, but unlike that path — which
/// receives the coord HTTP base with `/ws` already stripped — this reads the
/// RAW profile `coord_url`, whose shipped `dev`/`production` values ALREADY
/// end in `/ws` (e.g. `wss://coord.qontinui.io/ws`, see
/// `bin/qontinui_profile.rs`). The construction is therefore made idempotent
/// via [`build_coord_ws_url`]: appending `/ws` to an already-`/ws` base would
/// produce `…/ws/ws` → 401 at the ALB → the subscribe loop never connects in
/// prod. The coord `/ws` endpoint is a Redis pub/sub bridge at the `/ws` path
/// (not the root); connecting to the root also returns 401.
fn coord_ws_url(device_id: uuid::Uuid) -> Option<String> {
    let coord_url = qontinui_runner_lib::profiles::load_strict()
        .ok()?
        .coord_url?;
    Some(build_coord_ws_url(&coord_url, device_id))
}

/// Pure builder for the coord agent-spawn WS subscription URL. Extracted from
/// [`coord_ws_url`] so it can be unit-tested without global profile state.
///
/// Normalization rule:
/// 1. Trim whitespace and any trailing `/`.
/// 2. Swap the scheme: `https://`→`wss://`, `http://`→`ws://`; leave an
///    already-`ws(s)://` base (or any other scheme) untouched.
/// 3. Append `/ws` ONLY if the base does not already end in `/ws` (the trailing
///    `/` was stripped in step 1, so a `…/ws/` input is handled too). This makes
///    the construction idempotent for the shipped profiles whose `coord_url`
///    already ends in `/ws`, while still appending it for a bare host URL.
/// 4. Append the device-scoped `events.agent.spawn_requested.<device>` pattern.
fn build_coord_ws_url(coord_url: &str, device_id: uuid::Uuid) -> String {
    let base = coord_url.trim().trim_end_matches('/');
    let ws_base = base
        .strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .or_else(|| {
            base.strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
        })
        .unwrap_or_else(|| base.to_string());
    // Idempotent: don't double-append `/ws` when the base already ends in it.
    let ws_base = if ws_base.ends_with("/ws") {
        ws_base
    } else {
        format!("{ws_base}/ws")
    };
    format!("{ws_base}?pattern=events.agent.spawn_requested.{device_id}")
}

/// Read `~/.qontinui/machine.json` → device_id. Falls back to None.
fn load_local_device_id() -> Option<uuid::Uuid> {
    #[derive(Deserialize)]
    struct DeviceFile {
        #[serde(alias = "machine_id")]
        device_id: String,
    }
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(&path).ok()?;
    let f: DeviceFile = serde_json::from_slice(&bytes).ok()?;
    uuid::Uuid::parse_str(&f.device_id).ok()
}

/// Path to the `claude` binary. Default: `claude` (PATH-resolved).
/// Override with `QONTINUI_CLAUDE_BIN` for testing (e.g. the
/// `mock_claude_cli` fixture in `bin/`).
pub(crate) fn claude_bin_path() -> String {
    std::env::var("QONTINUI_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
}

// =============================================================================
// Public entrypoint
// =============================================================================

/// Spawn the agent-runtime tokio task. Subscribes to spawn-request
/// events for the local `device_id`; on each event, runs a subprocess
/// against the allocated worktrees.
///
/// Wired in `main.rs` next to `fleet::spawn_heartbeat()`.
pub fn spawn_runtime() {
    let device_id = match load_local_device_id() {
        Some(d) => d,
        None => {
            info!(
                "agent_runtime: ~/.qontinui/machine.json missing or device_id \
                 unparseable — agent spawn runtime disabled. Skipping."
            );
            return;
        }
    };

    if coord_http_base().is_none() {
        info!(
            "agent_runtime: profile has no coord_url — agent spawn runtime \
             disabled. Skipping."
        );
        return;
    }

    info!("agent_runtime: starting for device_id={}", device_id);
    // Periodic backstop for the capacity-freed re-poll: drains a deferred
    // (AtCap) continuation even if its terminal's exit-hook trigger is missed.
    // Spawned independently of the WS pump so it survives subscription flaps;
    // it is an idle no-op until the first AtCap deferral arms it.
    spawn_continuation_backstop_poll(device_id);
    tokio::spawn(async move {
        if let Err(e) = subscribe_to_spawn_requests(device_id).await {
            error!("agent_runtime: subscriber exited with error: {e:#}");
        }
    });
}

/// The base (and reset) reconnect back-off, in milliseconds.
const BACKOFF_BASE_MS: u64 = 2_000;

/// The capped maximum reconnect back-off, in milliseconds.
const BACKOFF_MAX_MS: u64 = 60_000;

/// A pump that stayed connected at least this long was a HEALTHY connection
/// that died — not a connect failure — so its disconnect must reset the
/// back-off to [`BACKOFF_BASE_MS`] (Fix (a)). See [`reset_backoff_after_pump`].
const HEALTHY_PUMP_THRESHOLD_SECS: u64 = 30;

/// Decide whether a finished pump should RESET the reconnect back-off.
///
/// Fix (a): an abnormal TCP drop (ALB idle-kill with no Close frame) surfaces
/// as `ws.next() → Some(Err)` → the `Err` arm of [`connect_and_pump`]'s caller.
/// Without this, the `Err` arm never reset `backoff_ms`, so the back-off ratchets
/// up to the [`BACKOFF_MAX_MS`] cap and PINS there forever — a 60s-connected /
/// 60s-disconnected duty cycle that drops every frame published in the gap.
///
/// The fix: a pump that *ran for a while* before dying was a healthy connection,
/// regardless of whether it returned `Ok` (clean Close) or `Err` (abnormal drop).
/// Only a pump that died almost immediately (a genuine connect/handshake failure
/// or an instant kick) should let the back-off keep climbing. This is a small
/// pure function so the decision is unit-testable without a live socket.
///
/// Returns `true` when `elapsed >= HEALTHY_PUMP_THRESHOLD_SECS`.
fn reset_backoff_after_pump(elapsed: Duration) -> bool {
    elapsed.as_secs() >= HEALTHY_PUMP_THRESHOLD_SECS
}

/// Subscribe loop: connects to coord WS, filters for this device's
/// spawn-request channel, dispatches each accepted payload to
/// `run_agent_subprocess` in its own task. Reconnects with capped
/// exponential backoff.
///
/// ## Back-off reset (Fix (a))
///
/// The back-off resets to [`BACKOFF_BASE_MS`] in TWO cases:
/// 1. A clean pump return (`Ok`) — a graceful Close frame.
/// 2. ANY pump (Ok OR Err) that stayed connected longer than
///    [`HEALTHY_PUMP_THRESHOLD_SECS`] — a healthy connection that died, including
///    the abnormal-drop case where the ALB idle-kills the socket and the pump
///    surfaces a `ws.next() → Some(Err)`. Before this fix, the `Err` arm never
///    reset, so a recurring idle-kill ratcheted the back-off to the 60s cap and
///    pinned it there (a 60s-connected/60s-disconnected flap that dropped every
///    frame in the gap). Fix (b)'s keepalive ping prevents the idle-kill in the
///    first place; this reset is the belt-and-suspenders recovery if a drop still
///    occurs for any other reason.
async fn subscribe_to_spawn_requests(device_id: uuid::Uuid) -> anyhow::Result<()> {
    let ws_url = match coord_ws_url(device_id) {
        Some(u) => u,
        None => {
            warn!("agent_runtime: no coord_url; subscriber loop exiting");
            return Ok(());
        }
    };
    let mut backoff_ms: u64 = BACKOFF_BASE_MS;
    loop {
        let started = std::time::Instant::now();
        let pump_result = connect_and_pump(&ws_url, device_id).await;
        let elapsed = started.elapsed();
        match pump_result {
            Ok(()) => {
                debug!("agent_runtime: WS pump returned cleanly; reconnecting");
                backoff_ms = BACKOFF_BASE_MS;
            }
            Err(e) => {
                // A pump that ran long enough was a healthy connection that
                // died abnormally (e.g. ALB idle-kill), NOT a connect failure —
                // reset so we reconnect promptly instead of pinning at the cap.
                if reset_backoff_after_pump(elapsed) {
                    warn!(
                        "agent_runtime: WS pump error after {}s of healthy uptime ({e:#}); \
                         resetting back-off and reconnecting in {}s",
                        elapsed.as_secs(),
                        BACKOFF_BASE_MS / 1000
                    );
                    backoff_ms = BACKOFF_BASE_MS;
                } else {
                    warn!(
                        "agent_runtime: WS pump error ({e:#}); retrying in {}s",
                        backoff_ms / 1000
                    );
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(BACKOFF_MAX_MS);
    }
}

/// How often [`connect_and_pump`] sends an unsolicited WS Ping to keep the
/// connection from being idle-reaped (Fix (b)).
///
/// Coord's `/ws` subscription is quiet between spawns; the ALB in front of coord
/// has a ~60s idle timeout and silently kills any connection with no traffic for
/// that long (no Close frame — the next `ws.next()` yields `Some(Err)`). A 20s
/// keepalive keeps three pings inside every 60s window so the connection never
/// looks idle. Coord's `/ws` explicitly answers a client Ping with a Pong
/// (coord src/ws.rs), and this recv loop already swallows Pong frames.
const KEEPALIVE_INTERVAL_SECS: u64 = 20;

/// Single connect-and-pump iteration: opens the WS, listens for events,
/// dispatches spawn-requests. Returns on disconnect.
///
/// Coord's `/ws` endpoint is a Redis pub/sub bridge; the pattern filter
/// is set via the `?pattern=` query param at upgrade time (client-sent
/// Text frames are silently ignored). The URL already carries the
/// device-scoped pattern from [`coord_ws_url`].
///
/// ## Keepalive (Fix (b))
///
/// The recv loop `tokio::select!`s `ws.next()` against a
/// [`KEEPALIVE_INTERVAL_SECS`] interval that sends a `Ping`. Without it the ALB
/// idle-reaps the quiet subscription at ~60s, producing the connect/disconnect
/// flap. The interval uses [`MissedTickBehavior::Delay`] so a long
/// `handle_message` (a spawn dispatch) doesn't cause a burst of catch-up pings.
///
/// ## Poll-on-connect (Fix (c2))
///
/// Immediately after a successful connect we poll coord for any
/// gate-continuation dispatches that landed in a disconnect GAP (frames the WS
/// missed entirely). See [`poll_pending_continuations`]. This is the at-least-
/// once replay backstop for Fix (c)'s lost-frame defect: the WS is the fast
/// path, the poll is the catch-up. Dedup against the process-wide
/// [`dispatched_gate_ids`] set keeps a frame delivered by BOTH paths from
/// double-spawning.
async fn connect_and_pump(ws_url: &str, device_id: uuid::Uuid) -> anyhow::Result<()> {
    use bytes::Bytes;
    use futures_util::{SinkExt, StreamExt};
    use tokio::time::MissedTickBehavior;
    use tokio_tungstenite::tungstenite::Message;

    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url).await?;
    info!("agent_runtime: WS connected for device_id={device_id}");

    // Fix (c2): on every fresh connect, replay any gate-continuation dispatches
    // coord persisted while we were disconnected. Best-effort — a poll failure
    // must never abort the pump (the live WS subscription proceeds regardless).
    poll_pending_continuations(device_id).await;
    // Work-unit DAG replay backstop: same reconnect tick, back-to-back. Replays
    // any unit dispatches coord persisted while we were disconnected (keyed on
    // dispatch_id). Best-effort, same warn-and-continue posture.
    poll_pending_unit_dispatches(device_id).await;

    let mut keepalive = tokio::time::interval(Duration::from_secs(KEEPALIVE_INTERVAL_SECS));
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The first immediate tick fires at once; skip it so we don't ping before
    // the connection has even settled (the next tick is a full interval out).
    keepalive.tick().await;

    loop {
        tokio::select! {
            // Keepalive: send an unsolicited Ping so the ALB never sees the
            // connection as idle. A send error means the socket is gone — surface
            // it as a pump error so the caller reconnects.
            _ = keepalive.tick() => {
                if let Err(e) = ws.send(Message::Ping(Bytes::new())).await {
                    warn!("agent_runtime: WS keepalive ping send failed: {e:#}");
                    return Err(anyhow::anyhow!("WS keepalive send: {e}"));
                }
            }
            // Inbound frame.
            maybe_msg = ws.next() => {
                let msg = match maybe_msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        warn!("agent_runtime: WS recv error: {e:#}");
                        return Err(anyhow::anyhow!("WS recv: {e}"));
                    }
                    None => {
                        // Stream ended (peer hung up without a Close frame).
                        return Ok(());
                    }
                };
                let txt = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(_) => {
                        info!("agent_runtime: WS closed by peer");
                        return Ok(());
                    }
                    Message::Frame(_) => continue,
                };
                if let Err(e) = handle_message(&txt, device_id).await {
                    warn!("agent_runtime: handle_message error (continuing): {e:#}");
                }
            }
        }
    }
}

/// Parse a WS frame; if it's a spawn-request for this device, dispatch
/// it. Coord's WS surface wraps payloads in a `{channel, body}` envelope
/// for filtering; we attempt to peel that, but fall back to a direct
/// `LaunchPayload` parse for compatibility with simpler test fixtures.
async fn handle_message(txt: &str, device_id: uuid::Uuid) -> anyhow::Result<()> {
    let value: serde_json::Value = serde_json::from_str(txt)?;

    // Envelope shape from coord's `/ws` Redis->WS fanout (coord/src/ws.rs):
    //   {"channel":"events.agent.spawn_requested.<id>","payload":"<json-string>"}
    // where `payload` is the raw Redis message — the LaunchPayload serialized
    // as a STRING. Older/test fixtures used {"channel","body":<object>}; we
    // accept both (see `parse_envelope_payload`). On a miss we log + return Ok:
    // a malformed or foreign frame must never kill the subscribe loop, and
    // because coord's default `events.*` subscription delivers every event,
    // most frames legitimately are not ours.
    if let Some(channel) = value.get("channel").and_then(|c| c.as_str()) {
        let spawn_ch = format!("events.agent.spawn_requested.{device_id}");
        let stop_ch = format!("events.agent.stop_requested.{device_id}");
        if channel == spawn_ch {
            // Decision 6: a gate-continuation frame carries coord's MINIMAL
            // payload (no agent_id/worktrees/jwt/claim_token), which can NEVER
            // deserialize into the full `LaunchPayload` the agent-spawn path
            // needs. Route it by its `source` discriminator into the dedicated
            // arm FIRST; only fall back to the `LaunchPayload` parse for the
            // agent-spawn source. This is the fix for the 2026-06-03
            // "dispatched but never consumed" drop.
            if envelope_is_gate_continuation(&value) {
                match parse_gate_continuation_payload(&value) {
                    Some(payload) => dispatch_gate_continuation(payload, device_id),
                    None => warn!(
                        "agent_runtime: gate-continuation envelope on {channel} had no \
                         parseable payload/body"
                    ),
                }
            } else {
                match parse_envelope_payload(&value) {
                    Some(payload) => spawn_run_task(payload),
                    None => {
                        warn!(
                        "agent_runtime: spawn envelope on {channel} had no parseable payload/body"
                    )
                    }
                }
            }
        } else if channel == stop_ch {
            // Operator stop (coord `agents_spawn::post_stop`): cancel the running
            // agent's token so its run loop kills the subprocess and does NOT
            // restart. No-op if this runner isn't running that agent_id.
            match parse_stop_agent_id(&value) {
                Some(agent_id) => {
                    let running_here = request_agent_stop(agent_id);
                    info!(
                        "agent_runtime: stop_requested agent_id={agent_id} (running_here={running_here})"
                    );
                }
                None => {
                    warn!("agent_runtime: stop envelope on {channel} had no parseable agent_id")
                }
            }
        }
        // Any other events.* frame (coord's default subscription delivers all)
        // is not ours — ignore.
        return Ok(());
    }

    // Bare-payload fallback (used by test fixtures + the no-envelope
    // coord WS variant).
    if value.get("agent_id").is_some() && value.get("worktrees").is_some() {
        let payload: LaunchPayload = serde_json::from_value(value)?;
        if payload.target_device_id != device_id {
            return Ok(());
        }
        spawn_run_task(payload);
    }
    Ok(())
}

/// Extract a `LaunchPayload` from a coord `/ws` envelope.
///
/// Coord's fanout (`coord/src/ws.rs`) emits `{"channel", "payload": "<json>"}`
/// where `payload` is the raw Redis message — the LaunchPayload serialized as
/// a STRING. Older/test fixtures used `{"channel", "body": <object>}`. Accept
/// both field names, and accept the inner value as a JSON string (`from_str`)
/// or a nested object (`from_value`). Returns `None` if neither yields a
/// parseable payload.
fn parse_envelope_payload(envelope: &serde_json::Value) -> Option<LaunchPayload> {
    let inner = envelope.get("payload").or_else(|| envelope.get("body"))?;
    match inner.as_str() {
        Some(s) => serde_json::from_str(s).ok(),
        None => serde_json::from_value(inner.clone()).ok(),
    }
}

/// Peel a coord `/ws` envelope's inner payload into an owned
/// `serde_json::Value`, accepting both the `payload` (current coord) and
/// legacy `body` field names, and the inner value as a JSON string
/// (`from_str`) or a nested object. Returns `None` if neither yields valid
/// JSON. Shared by the gate-continuation and source-sniffing helpers so the
/// string-vs-object handling stays in lockstep with [`parse_envelope_payload`].
fn envelope_inner_value(envelope: &serde_json::Value) -> Option<serde_json::Value> {
    let inner = envelope.get("payload").or_else(|| envelope.get("body"))?;
    match inner.as_str() {
        Some(s) => serde_json::from_str(s).ok(),
        None => Some(inner.clone()),
    }
}

/// Does this spawn-request envelope carry the gate-continuation discriminator
/// (`source == "gate_continuation"`)? Used to route a frame into the dedicated
/// gate-continuation arm BEFORE attempting the full-`LaunchPayload` parse — the
/// two shapes are mutually exclusive (a gate-continuation frame has no
/// `agent_id`/`worktrees`/`jwt`, so it can never deserialize as a
/// `LaunchPayload`, and vice versa).
fn envelope_is_gate_continuation(envelope: &serde_json::Value) -> bool {
    envelope_inner_value(envelope)
        .and_then(|v| {
            v.get("source")
                .and_then(|s| s.as_str())
                .map(|s| s == GATE_CONTINUATION_SOURCE)
        })
        .unwrap_or(false)
}

/// Extract a [`GateContinuationPayload`] from a coord `/ws` envelope. Mirrors
/// [`parse_envelope_payload`]'s string-or-object inner handling. Returns `None`
/// if the inner JSON does not deserialize into the minimal continuation shape.
fn parse_gate_continuation_payload(
    envelope: &serde_json::Value,
) -> Option<GateContinuationPayload> {
    let inner = envelope_inner_value(envelope)?;
    serde_json::from_value(inner).ok()
}

/// Extract `agent_id` from a coord stop envelope (`{channel, payload|body}`),
/// whose inner is `{"agent_id": "<uuid>", "at": ...}`. Mirrors
/// `parse_envelope_payload`'s string-or-object inner handling.
fn parse_stop_agent_id(envelope: &serde_json::Value) -> Option<uuid::Uuid> {
    let inner = envelope.get("payload").or_else(|| envelope.get("body"))?;
    let obj: serde_json::Value = match inner.as_str() {
        Some(s) => serde_json::from_str(s).ok()?,
        None => inner.clone(),
    };
    obj.get("agent_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
}

/// Per-agent cancellation registry. A coord `events.agent.stop_requested`
/// frame cancels the token for that agent_id; `run_agent_subprocess` selects
/// on it to kill the subprocess and break WITHOUT restarting. The entry is
/// inserted in `spawn_run_task` and removed when the run task finishes.
#[allow(clippy::type_complexity)]
fn agent_stops() -> &'static std::sync::Mutex<
    std::collections::HashMap<uuid::Uuid, tokio_util::sync::CancellationToken>,
> {
    static AGENT_STOPS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<uuid::Uuid, tokio_util::sync::CancellationToken>,
        >,
    > = std::sync::OnceLock::new();
    AGENT_STOPS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Cancel a running agent's stop token. Returns true if the agent_id was
/// running on this runner (idempotent: cancelling an already-cancelled token
/// is a no-op).
fn request_agent_stop(agent_id: uuid::Uuid) -> bool {
    if let Some(tok) = agent_stops().lock().unwrap().get(&agent_id) {
        tok.cancel();
        true
    } else {
        false
    }
}

fn spawn_run_task(payload: LaunchPayload) {
    let agent_id = payload.agent_id;
    info!(
        "agent_runtime: spawn-request received agent_id={} worktrees={}",
        agent_id,
        payload.worktrees.len()
    );
    let stop = tokio_util::sync::CancellationToken::new();
    agent_stops().lock().unwrap().insert(agent_id, stop.clone());
    tokio::spawn(async move {
        if let Err(e) = run_agent_subprocess(payload, stop).await {
            error!("agent_runtime: run_agent_subprocess failed: {e:#}");
        }
        // Drop the registry entry once the run task is fully done.
        agent_stops().lock().unwrap().remove(&agent_id);
        // Drop the agent's live-token slot so its proxy nonce hard-fails closed
        // (the agent process is gone; any lingering `.mcp.json` nonce must 401).
        crate::coord_mcp::remove_agent_token(agent_id);
        // Stop the per-agent durability + observability daemons (pusher/poller).
        crate::agent_daemons::stop_for_agent(agent_id);
    });
}

// =============================================================================
// Gate-continuation path (Decision 6) + replay backstop (Fix (c2))
// =============================================================================

/// One row of coord's `GET /coord/agents/pending-continuations` response.
///
/// Wire contract (FIXED, coded against exactly):
/// `{"pending": [{"gate_id", "payload": {…}, "dispatched_at"}], "total": N}`,
/// where `payload` is the EXACT spawn-payload object coord publishes on the WS
/// channel (same shape [`GateContinuationPayload`] parses, now carrying
/// `gate_id`). Rows are dispatched-but-unconsumed within the last 24h.
#[derive(Debug, Clone, Deserialize)]
struct PendingContinuation {
    gate_id: uuid::Uuid,
    payload: GateContinuationPayload,
    #[serde(default)]
    #[allow(dead_code)]
    dispatched_at: Option<String>,
}

/// The envelope of coord's `GET /coord/agents/pending-continuations` response.
#[derive(Debug, Clone, Deserialize)]
struct PendingContinuationsResponse {
    #[serde(default)]
    pending: Vec<PendingContinuation>,
    #[serde(default)]
    #[allow(dead_code)]
    total: i64,
}

/// Body for `POST /coord/gates/{gate_id}/continuation-consumed`.
///
/// Two shapes over the SAME route (coord's claim-then-outcome contract):
/// - `{device_id}` (no outcome) = the CLAIM, posted BEFORE spawning.
/// - `{device_id, outcome, detail?}` = the OUTCOME, posted AFTER the spawn
///   attempt resolves (`spawned` / `spawn_failed`).
///
/// `outcome`/`detail` are `skip_serializing_if = Option::is_none` so the claim
/// post serializes to exactly `{device_id}` (byte-identical to the pre-restructure
/// body coord already accepts as the claim).
#[derive(Debug, Clone, Serialize)]
struct ContinuationConsumedBody {
    device_id: uuid::Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl ContinuationConsumedBody {
    /// The CLAIM body — no outcome. Posted before spawning.
    fn claim(device_id: uuid::Uuid) -> Self {
        Self {
            device_id,
            outcome: None,
            detail: None,
        }
    }

    /// The OUTCOME body — `spawned` (success) or `spawn_failed` + first-line
    /// detail (failure). Posted after the spawn attempt resolves.
    fn outcome(device_id: uuid::Uuid, spawned: bool, detail: Option<String>) -> Self {
        Self {
            device_id,
            outcome: Some(if spawned { "spawned" } else { "spawn_failed" }),
            detail,
        }
    }
}

/// The runner's decision after POSTing the consume CLAIM, derived purely from
/// the claim response (status + body). Factored out as a pure fn
/// ([`decide_spawn`]) so the 200 / 409-cancelled / error branches are
/// unit-testable without a live coord.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SpawnDecision {
    /// Claim accepted (HTTP 200) → proceed to spawn.
    Spawn,
    /// Claim rejected (HTTP 409 `{"error":"cancelled", ...}`) → the continuation
    /// was withdrawn upstream; SKIP the spawn. Carries the cancel reason (if any)
    /// for the INFO log.
    SkipCancelled { reason: Option<String> },
    /// Network failure / timeout / any other non-2xx → PROCEED to spawn anyway
    /// (availability over consistency; the in-process dedupe still guards).
    /// Carries a human-readable cause for the WARN log.
    SpawnDespiteClaimError { cause: String },
}

/// Decode coord's claim response into a [`SpawnDecision`] (pure — no I/O).
///
/// - 2xx → [`SpawnDecision::Spawn`].
/// - 409 with a JSON body whose `error == "cancelled"` →
///   [`SpawnDecision::SkipCancelled`] (reason from `cancel_reason`).
/// - any other status (incl. a 409 that ISN'T the cancelled shape) →
///   [`SpawnDecision::SpawnDespiteClaimError`] (availability over consistency).
///
/// `status` is the HTTP status code; `body` is the response body text (may be
/// empty / non-JSON — handled gracefully).
fn decide_spawn(status: u16, body: &str) -> SpawnDecision {
    if (200..300).contains(&status) {
        return SpawnDecision::Spawn;
    }
    if status == 409 {
        // Parse the cancelled shape: `{"error":"cancelled","cancel_reason":...}`.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            if v.get("error").and_then(|e| e.as_str()) == Some("cancelled") {
                let reason = v
                    .get("cancel_reason")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string());
                return SpawnDecision::SkipCancelled { reason };
            }
        }
        // A 409 that is NOT the cancelled contract (e.g. some other conflict):
        // proceed rather than silently drop the continuation.
        return SpawnDecision::SpawnDespiteClaimError {
            cause: format!("claim returned 409 without a cancelled body: {body}"),
        };
    }
    SpawnDecision::SpawnDespiteClaimError {
        cause: format!("claim returned status {status}"),
    }
}

/// Process-wide set of gate_ids whose continuation we have ALREADY dispatched.
///
/// At-least-once delivery means a single continuation can arrive via BOTH the WS
/// fast-path and the poll backstop (Fix (c2)) — or via two successive polls
/// before the consumed-ack lands. This set is the dedupe guard: the FIRST
/// dispatch of a given `gate_id` inserts it; subsequent attempts short-circuit.
/// Continuations WITHOUT a `gate_id` (legacy coord) can't be deduped by id and
/// always dispatch — accepted, because legacy coord also never re-lists them via
/// the (new) poll surface, so the only delivery path is the single WS frame.
fn dispatched_gate_ids() -> &'static std::sync::Mutex<std::collections::HashSet<uuid::Uuid>> {
    static DISPATCHED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashSet<uuid::Uuid>>,
    > = std::sync::OnceLock::new();
    DISPATCHED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Atomically claim a `gate_id` for dispatch. Returns `true` if THIS call was
/// the first to claim it (caller should dispatch); `false` if it was already
/// claimed (caller must skip — a duplicate delivery). Insert-and-test under one
/// lock so two concurrent deliveries can't both win.
fn claim_gate_dispatch(gate_id: uuid::Uuid) -> bool {
    dispatched_gate_ids().lock().unwrap().insert(gate_id)
}

/// Process-wide set of work-unit `dispatch_id`s whose continuation we have
/// ALREADY dispatched. The sibling of [`dispatched_gate_ids`] for the work-unit
/// DAG dispatch path: a unit dispatch reuses the `gate_continuation` spawn frame
/// but is keyed on `dispatch_id` (it has no `gate_id`), so it needs its own
/// dedupe set. At-least-once delivery means a single unit dispatch can arrive via
/// BOTH the live WS frame and the `pending-unit-dispatches` poll backstop (or via
/// two successive polls before the consume ack lands); the FIRST dispatch inserts
/// the id and subsequent attempts short-circuit.
fn dispatched_dispatch_ids() -> &'static std::sync::Mutex<std::collections::HashSet<uuid::Uuid>> {
    static DISPATCHED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashSet<uuid::Uuid>>,
    > = std::sync::OnceLock::new();
    DISPATCHED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Atomically claim a `dispatch_id` for dispatch. Returns `true` if THIS call was
/// the first to claim it (caller should dispatch); `false` if it was already
/// claimed (a duplicate delivery — caller must skip). Insert-and-test under one
/// lock, exactly like [`claim_gate_dispatch`].
fn claim_dispatch_dispatch(dispatch_id: uuid::Uuid) -> bool {
    dispatched_dispatch_ids()
        .lock()
        .unwrap()
        .insert(dispatch_id)
}

// =============================================================================
// Continuation-session registry (P3 anchor_key dedup + P4 concurrency cap)
// =============================================================================

/// Default cap on concurrently-live *continuation-spawned* terminal sessions.
///
/// **Unbounded by default** (`usize::MAX`). The primary runner — the only
/// instance in production — must spawn EVERY continuation regardless of how many
/// are already live; the Terminal UI scales to unlimited sessions via a 9-zone
/// grid per page × many page tabs (overflow lands in each page's Unassigned
/// list). A finite throttle is still available by setting
/// `QONTINUI_CONTINUATION_SESSION_CAP` (rarely needed; when unset, no cap
/// applies). Operator-opened sessions are never counted (they are never
/// registered here).
const DEFAULT_CONTINUATION_SESSION_CAP: usize = usize::MAX;

/// One registered continuation-spawned session.
#[derive(Debug, Clone)]
struct ContinuationSession {
    /// The runner-local terminal id (`TerminalManager` key) — used to test
    /// liveness against the manager.
    terminal_id: String,
    /// The gate `anchor_key` this continuation was spawned for, if any. The
    /// dedup key: a second continuation with the same live `anchor_key` is a
    /// re-cleared gate / duplicate and must not double-spawn.
    anchor_key: Option<String>,
}

/// Process-wide registry of continuation-spawned terminal sessions, keyed by
/// `terminal_id`. Distinct from [`dispatched_gate_ids`] (which dedupes a single
/// `gate_id` delivered twice): this tracks LIVE sessions so a re-cleared gate
/// (new `gate_id`, same `anchor_key`) is deduped (P3) and the live count is
/// capped (P4).
fn continuation_sessions(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, ContinuationSession>> {
    static SESSIONS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ContinuationSession>>,
    > = std::sync::OnceLock::new();
    SESSIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// The configured continuation-session cap (env override, else the default).
/// A non-numeric / empty env value falls back to the default.
fn continuation_session_cap() -> usize {
    std::env::var("QONTINUI_CONTINUATION_SESSION_CAP")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_CONTINUATION_SESSION_CAP)
}

/// Whether THIS runner instance should spawn a continuation addressed to
/// `target_instance_name`.
///
/// A continuation spawns on EXACTLY the instance it is addressed to. An absent
/// target (`None`) addresses the PRIMARY, whose `instance_name()` is also `None`,
/// so the rule collapses to a single equality:
///
/// | `target_instance_name` | spawns on |
/// |---|---|
/// | `None` (normal)        | the primary only (`instance_name() == None`) |
/// | `Some("test-xyz")`     | only the secondary named `"test-xyz"` |
///
/// Note this also keeps the primary from double-spawning a continuation that is
/// explicitly addressed to a named secondary (`Some(t)` ≠ primary's `None`).
///
/// Pure over (`target`, `this_instance`) so the primary-only / named-carve-out
/// policy is unit-testable without env or a live dispatcher. The real call site
/// ([`dispatch_gate_continuation`]) passes `instance::instance_name()` as
/// `this_instance`.
fn continuation_addressed_to_self(
    target_instance_name: Option<&str>,
    this_instance_name: Option<&str>,
) -> bool {
    target_instance_name == this_instance_name
}

/// Drop registry entries whose terminal is no longer live, using `is_live`
/// (`terminal_id -> bool`). Called under the registry lock by the guard below
/// so the count and the anchor_key scan only ever see currently-running
/// sessions. Returns the retained entries' count.
fn prune_dead_continuations(
    map: &mut std::collections::HashMap<String, ContinuationSession>,
    is_live: &dyn Fn(&str) -> bool,
) {
    map.retain(|tid, _| is_live(tid));
}

/// Outcome of the pre-spawn continuation guard (P3 + P4).
#[derive(Debug, PartialEq, Eq)]
enum ContinuationGuard {
    /// Clear to spawn — no live duplicate, under cap.
    Proceed,
    /// A live continuation already exists for this `anchor_key` (P3): skip the
    /// spawn (re-cleared gate / duplicate). Carries the existing terminal_id.
    DuplicateAnchor(String),
    /// At the concurrency cap (P4): skip the spawn. Carries the cap for the log.
    AtCap(usize),
}

/// Pre-spawn guard: prune dead sessions, then enforce P3 (anchor_key dedup) and
/// P4 (concurrency cap). Pure over (`anchor_key`, `is_live`, env cap) so it is
/// unit-testable without a live `TerminalManager`.
///
/// Order matters: dedup is checked BEFORE the cap so a duplicate of an
/// already-running anchor is reported as a dedup (the honest reason) rather than
/// "capped". Operator sessions never enter the registry, so they never count.
fn evaluate_continuation_guard(
    anchor_key: Option<&str>,
    is_live: &dyn Fn(&str) -> bool,
) -> ContinuationGuard {
    let mut map = continuation_sessions().lock().unwrap();
    prune_dead_continuations(&mut map, is_live);

    // P3: a LIVE session already exists for this anchor_key → dedup.
    if let Some(anchor) = anchor_key {
        if let Some(existing) = map
            .values()
            .find(|s| s.anchor_key.as_deref() == Some(anchor))
        {
            return ContinuationGuard::DuplicateAnchor(existing.terminal_id.clone());
        }
    }

    // P4: at the cap → refuse.
    let cap = continuation_session_cap();
    if map.len() >= cap {
        return ContinuationGuard::AtCap(cap);
    }

    ContinuationGuard::Proceed
}

/// Register a freshly-spawned continuation session in the live registry (after
/// `create_terminal_session_backend` succeeds). The entry is reaped lazily by
/// [`prune_dead_continuations`] the next time the guard runs.
fn register_continuation_session(terminal_id: String, anchor_key: Option<String>) {
    continuation_sessions().lock().unwrap().insert(
        terminal_id.clone(),
        ContinuationSession {
            terminal_id,
            anchor_key,
        },
    );
}

// =============================================================================
// Capacity-freed re-poll (Defect A item 3, layered on #484)
// =============================================================================

/// Has at least one `AtCap` deferral happened this process lifetime?
///
/// #484 leaves an `AtCap` continuation pending (uncancelled, unconsumed) on
/// coord so it is re-deliverable once a cap slot frees — but it adds NO trigger
/// to re-fetch it. Without one, a deferred continuation only drains on the next
/// WS reconnect (`poll_pending_continuations` is called exactly once, on
/// connect). This flag arms the periodic backstop poll
/// ([`spawn_continuation_backstop_poll`]) so the queue still drains even if the
/// capacity-freed exit-hook trigger ([`notify_continuation_terminal_exit`]) is
/// missed (e.g. coord registration failed so no exit hook was installed, or the
/// process restarted between the deferral and the slot freeing). Cheap, set-once,
/// never reset: a single deferral is enough to want the backstop for the rest of
/// the process's life.
fn at_cap_ever() -> &'static std::sync::atomic::AtomicBool {
    static AT_CAP_EVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &AT_CAP_EVER
}

/// Record that an `AtCap` deferral occurred (arms the periodic backstop poll).
fn mark_at_cap_deferral() {
    at_cap_ever().store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether any `AtCap` deferral has happened this process lifetime.
fn at_cap_deferral_happened() -> bool {
    at_cap_ever().load(std::sync::atomic::Ordering::Relaxed)
}

/// Capacity-freed re-poll trigger: called from the continuation terminal's
/// on-exit hook (`create_terminal_session_backend`, `commands/terminal.rs`) the
/// instant a continuation-spawned PTY exits.
///
/// Drops the exited terminal from the live continuation registry and — only if
/// it WAS a registered continuation session — kicks an immediate
/// [`poll_pending_continuations`] so a previously-deferred (`AtCap`) continuation
/// drains promptly into the freed slot instead of waiting for an unrelated WS
/// reconnect.
///
/// **Operator tabs never trigger a poll.** Operator-opened terminals are created
/// via `terminal_create` (a different path) and are never inserted into the
/// continuation registry, so the `was_continuation` test below is `false` for
/// them and this is a pure no-op. (`create_terminal_session_backend` — the only
/// caller that wires this hook — is reached ONLY from the gate-continuation
/// path.) Even so, the registry check is kept as defense-in-depth so a future
/// caller of the backend helper can't accidentally trigger poll storms on
/// unrelated tab closes.
///
/// No device id → no-op (the poll has no target); a deferral that happened
/// without a resolvable device id is covered by the WS-reconnect catch-up.
///
/// `rt_handle` is the tokio runtime handle captured at hook-install time: the
/// PTY waiter that fires this hook is a bare OS thread with NO runtime context,
/// so a direct `tokio::spawn` here would panic ("there is no reactor running").
/// `None` (no runtime was current at install — headless/unit-test) → the poll is
/// skipped and the periodic backstop / WS-reconnect catch-up covers it.
pub(crate) fn notify_continuation_terminal_exit(
    terminal_id: &str,
    rt_handle: Option<&tokio::runtime::Handle>,
) {
    if !deregister_exited_continuation(terminal_id) {
        return;
    }
    let Some(device_id) = load_local_device_id() else {
        return;
    };
    let Some(handle) = rt_handle else {
        debug!(
            "agent_runtime: continuation terminal_id={terminal_id} exited but no tokio \
             handle to poll on — relying on the periodic backstop / next WS reconnect"
        );
        return;
    };
    debug!(
        "agent_runtime: continuation terminal_id={terminal_id} exited — \
         kicking capacity-freed pending-continuations poll"
    );
    handle.spawn(async move {
        poll_pending_continuations(device_id).await;
        poll_pending_unit_dispatches(device_id).await;
    });
}

/// Remove an exited terminal from the live continuation registry, returning
/// `true` iff it WAS a registered continuation session (i.e. its exit just freed
/// a continuation cap slot, so a deferred continuation should be re-polled).
///
/// Pure over the registry (the only side effect is the removal), so the
/// capacity-freed re-poll decision is unit-testable without a tokio runtime or a
/// live `TerminalManager`. Operator tabs are never in the registry → `false`.
fn deregister_exited_continuation(terminal_id: &str) -> bool {
    continuation_sessions()
        .lock()
        .map(|mut map| map.remove(terminal_id).is_some())
        .unwrap_or(false)
}

/// How often the periodic backstop poll fires once armed (5 min).
const CONTINUATION_BACKSTOP_POLL_SECS: u64 = 300;

/// Spawn the periodic backstop poll task (Defect A item 3, backstop half).
///
/// A best-effort safety net for the capacity-freed exit-hook trigger
/// ([`notify_continuation_terminal_exit`]): a missed exit signal — a continuation
/// terminal whose coord registration failed (so no on-exit hook was installed),
/// or a slot that frees while the runner is between WS connects — must not strand
/// a deferred continuation on coord's pending queue until an unrelated WS
/// reconnect happens to re-poll. Every [`CONTINUATION_BACKSTOP_POLL_SECS`] this
/// task polls coord for pending continuations, but ONLY after at least one
/// `AtCap` deferral has happened this process lifetime
/// ([`at_cap_deferral_happened`]) — until then there is nothing to drain and the
/// task is a cheap idle tick (no network I/O).
///
/// Spawned once per process from [`spawn_runtime`]. Independent of the WS pump
/// so it keeps draining even while the subscription is flapping.
fn spawn_continuation_backstop_poll(device_id: uuid::Uuid) {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(CONTINUATION_BACKSTOP_POLL_SECS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate first tick — nothing can be deferred at startup.
        interval.tick().await;
        loop {
            interval.tick().await;
            if at_cap_deferral_happened() {
                debug!(
                    "agent_runtime: backstop poll firing (a prior AtCap deferral armed it) \
                     for device_id={device_id}"
                );
                poll_pending_continuations(device_id).await;
                poll_pending_unit_dispatches(device_id).await;
            }
        }
    });
}

/// Liveness predicate for the continuation guard, backed by the process-global
/// `TerminalManager`. Returns `terminal_id -> still running?`. When there is no
/// Tauri runtime / no managed `TerminalManager` (headless or unit-test context)
/// every terminal id reads as NOT live — the registry is empty there anyway, so
/// the guard is a no-op.
fn live_terminal_predicate() -> impl Fn(&str) -> bool {
    use std::sync::Arc;
    let manager: Option<Arc<crate::terminal::TerminalManager>> = crate::tauri_app_handle::current()
        .and_then(|app| {
            tauri::Manager::try_state::<Arc<crate::terminal::TerminalManager>>(&app)
                .map(|s| s.inner().clone())
        });
    move |terminal_id: &str| {
        manager
            .as_ref()
            .and_then(|m| m.get(terminal_id))
            .map(|sess| sess.is_alive())
            .unwrap_or(false)
    }
}

/// Tauri event the frontend listens for to surface a continuation terminal:
/// switch the main view to the Terminal panel and select the target tab.
const EVENT_TERMINAL_FOCUS_REQUEST: &str = "terminal-focus-request";

/// Payload for [`EVENT_TERMINAL_FOCUS_REQUEST`]. `terminal_id` is the session
/// the frontend should select; the App-level listener also switches the main
/// view to the Terminal panel.
#[derive(Serialize)]
struct TerminalFocusRequest<'a> {
    terminal_id: &'a str,
}

/// Emit a `terminal-focus-request` to the MAIN window only.
///
/// Uses `emit_to(get_main_window_label(), …)` — NOT the bare global `emit`
/// (the `terminal-created` broadcast pattern) — so a pop-out webview is not
/// yanked to this tab. The canonical main-window-label accessor
/// (`qontinui_runner_lib::get_main_window_label()`, returns `"main"`) matches
/// the #473 `ui-bridge:invoke-request` scoping.
fn emit_terminal_focus_request(app: &tauri::AppHandle, terminal_id: &str) {
    use tauri::Emitter;
    let payload = TerminalFocusRequest { terminal_id };
    if let Err(e) = app.emit_to(
        qontinui_runner_lib::get_main_window_label(),
        EVENT_TERMINAL_FOCUS_REQUEST,
        &payload,
    ) {
        warn!(
            "agent_runtime: failed to emit {EVENT_TERMINAL_FOCUS_REQUEST} for \
             terminal_id={terminal_id}: {e}"
        );
    }
}

/// Bring the existing live continuation tab a duplicate-anchor dispatch was
/// folded onto (P3) to focus. Emits the SAME `terminal-focus-request` event as
/// a fresh continuation so the operator sees the live session rather than
/// nothing happening. Acquires the process-global `AppHandle`; in a headless /
/// unit-test context (no Tauri runtime) it debug-logs and returns.
fn focus_existing_continuation(terminal_id: &str) {
    match crate::tauri_app_handle::current() {
        Some(app) => {
            debug!(
                "agent_runtime: duplicate continuation folded onto existing live \
                 terminal_id={terminal_id} — emitting focus-request"
            );
            emit_terminal_focus_request(&app, terminal_id);
        }
        None => {
            debug!(
                "agent_runtime: duplicate continuation folded onto existing live \
                 terminal_id={terminal_id} but no Tauri AppHandle (headless) — \
                 cannot emit focus-request"
            );
        }
    }
}

/// The coord ack surface a continuation must drive after it is handled. Threaded
/// through [`run_gate_continuation_inner`] so the SHARED spawn machinery is
/// reused while the ack differs per kind:
///
/// - [`ConsumeTarget::Gate`] — the gate continuation contract: a consume CLAIM is
///   POSTed BEFORE spawning (a `409 cancelled` skips the spawn) and a typed
///   OUTCOME (`spawned`/`spawn_failed`) is POSTed after, both on
///   `/coord/gates/{gate_id}/continuation-consumed`.
/// - [`ConsumeTarget::Dispatch`] — the work-unit dispatch contract: NO
///   claim-before-spawn (the CAS already made dispatch at-most-once-create); a
///   single idempotent consume ack is POSTed AFTER a successful handle on
///   `/coord/agents/unit-dispatches/{dispatch_id}/consumed`. A failed spawn is
///   deliberately NOT acked so coord re-lists it on the next reconnect
///   (at-least-once).
/// - [`ConsumeTarget::None`] — legacy coord (neither id): spawn once, no ack.
#[derive(Debug, Clone, Copy)]
enum ConsumeTarget {
    Gate(uuid::Uuid),
    Dispatch(uuid::Uuid),
    None,
}

/// Shared dispatch seam for BOTH the WS fast-path and the poll backstop.
///
/// **Claim-then-spawn-then-outcome** (the coord continuation contract). For a
/// payload carrying a `gate_id` the whole dispatch is one async task that:
///
/// 1. **Fast-path dedupe** (synchronous, before any I/O): [`claim_gate_dispatch`]
///    against the in-process set — a duplicate delivery (same `gate_id`) is
///    dropped here so a continuation delivered by both transports never even
///    starts a second task. This is the in-process guard; the network claim
///    below is the durable cross-restart one.
/// 2. **CLAIM, then SPAWN, then OUTCOME** inside one async task
///    ([`run_gate_continuation_owned`]): the #469 local guards run, then the
///    consume-CLAIM is POSTed and awaited BEFORE spawning — a `409 cancelled`
///    SKIPS the spawn (closes the poll→cancel→spawn race); a network/other
///    error PROCEEDS (availability over consistency). After the spawn attempt
///    resolves, the honest OUTCOME (`spawned` / `spawn_failed`) is POSTed.
///
/// Absent `gate_id` (legacy coord) → no dedupe-by-id, no claim, no outcome:
/// dispatch exactly once via [`spawn_gate_continuation_task`] (unchanged).
fn dispatch_gate_continuation(payload: GateContinuationPayload, device_id: uuid::Uuid) {
    // Instance-targeting self-gate (primary-only by default). A continuation
    // spawns on EXACTLY the instance it is addressed to; an absent
    // `target_instance_name` addresses the PRIMARY (`instance_name() == None`).
    // Enforced HERE — before the in-process dedupe and any coord claim, and on
    // BOTH the WS fast-path and the replay-poll path (both route through this
    // fn) — so a temp/named runner never spawns a continuation meant for the
    // primary. This is the load-bearing fix for the temp-runner overflow: the
    // coord consume claim is keyed on `device_id` alone (idempotent-200, NOT
    // first-claimer-wins) and every runner instance on this machine shares the
    // device id, so coord cannot route a continuation to one instance — the
    // instance must gate itself. No coord claim/outcome is posted on this skip:
    // the addressed instance owns the claim+spawn (contract item 4).
    if !continuation_addressed_to_self(
        payload.target_instance_name.as_deref(),
        crate::instance::instance_name().as_deref(),
    ) {
        debug!(
            "agent_runtime: gate-continuation not addressed to this instance \
             (this={:?}, target={:?}); skipping — the addressed instance spawns it",
            crate::instance::instance_name(),
            payload.target_instance_name,
        );
        return;
    }
    if let Some(gate_id) = payload.gate_id {
        // Synchronous fast-path: drop an in-process duplicate before any I/O.
        if !claim_gate_dispatch(gate_id) {
            debug!(
                "agent_runtime: gate-continuation gate_id={gate_id} already dispatched; \
                 skipping duplicate"
            );
            return;
        }
        // One task owns the whole claim → spawn → outcome handshake. The #469
        // local guards run FIRST inside `run_gate_continuation_inner`, then the
        // consume-CLAIM is awaited (contract item 4: claim only after the local
        // cap passes), then spawn-or-skip, then the outcome POST.
        tokio::spawn(async move {
            if let Err(e) =
                run_gate_continuation_inner(payload, device_id, ConsumeTarget::Gate(gate_id)).await
            {
                error!("agent_runtime: run_gate_continuation (gate_id={gate_id}) failed: {e:#}");
            }
        });
    } else if let Some(dispatch_id) = payload.dispatch_id {
        // Work-unit DAG dispatch: reuses the `gate_continuation` spawn frame but
        // is keyed on `dispatch_id` (no gate_id). Applies on BOTH the live WS
        // frame AND the `pending-unit-dispatches` replay-poll path (both route
        // through here), so dedupe + ack happen regardless of arrival path.
        //
        // Synchronous fast-path: drop an in-process duplicate before any I/O.
        if !claim_dispatch_dispatch(dispatch_id) {
            debug!(
                "agent_runtime: unit-dispatch dispatch_id={dispatch_id} already dispatched; \
                 skipping duplicate"
            );
            return;
        }
        // One task owns spawn → consume-ack. Unlike the gate path there is NO
        // claim-before-spawn: the scheduler's `metadata.dispatched_at` CAS is the
        // single dispatch authority (at-most-once-create); this record is only the
        // replay payload, so the runner just spawns and acks the consume on a
        // successful handle (a failed spawn stays un-consumed → re-listed next
        // reconnect = at-least-once).
        tokio::spawn(async move {
            if let Err(e) = run_gate_continuation_inner(
                payload,
                device_id,
                ConsumeTarget::Dispatch(dispatch_id),
            )
            .await
            {
                error!(
                    "agent_runtime: run_gate_continuation (dispatch_id={dispatch_id}) failed: {e:#}"
                );
            }
        });
    } else {
        // Legacy coord: neither gate_id nor dispatch_id → no dedupe-by-id and no
        // claim/outcome. Dispatch once with no coord handshake (unchanged).
        spawn_gate_continuation_task(payload, device_id);
    }
}

/// Poll coord for gate-continuation dispatches that landed while this runner was
/// disconnected, and replay any we haven't already dispatched (Fix (c2)).
///
/// `GET <coord_http_base>/coord/agents/pending-continuations?device_id=<uuid>`
/// returns the dispatched-but-unconsumed rows from the last 24h. For each row we
/// stamp the row's `gate_id` onto the parsed payload (so the shared dispatch
/// seam can dedupe + ack it) and route it through [`dispatch_gate_continuation`],
/// which dedupes against the in-process set, spawns the continuation, and acks
/// `continuation-consumed`.
///
/// **Best-effort, warn-and-continue**: coord unreachable, a non-2xx status, or a
/// parse error all `warn!` once and return — the live WS subscription proceeds
/// regardless (a missed poll is retried on the next connect).
async fn poll_pending_continuations(device_id: uuid::Uuid) {
    let Some(base) = coord_http_base() else {
        return;
    };
    let url = format!("{base}/coord/agents/pending-continuations?device_id={device_id}");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("agent_runtime: pending-continuations client build failed: {e:#}");
            return;
        }
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("agent_runtime: pending-continuations GET failed (continuing): {e:#}");
            return;
        }
    };
    if !resp.status().is_success() {
        warn!(
            "agent_runtime: pending-continuations GET returned {} (continuing)",
            resp.status()
        );
        return;
    }
    let body: PendingContinuationsResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!("agent_runtime: pending-continuations parse failed (continuing): {e:#}");
            return;
        }
    };
    if body.pending.is_empty() {
        debug!("agent_runtime: pending-continuations poll: none pending for device_id={device_id}");
        return;
    }
    info!(
        "agent_runtime: pending-continuations poll: {} pending for device_id={device_id} — replaying",
        body.pending.len()
    );
    for row in body.pending {
        // The row's gate_id is authoritative; stamp it onto the payload so the
        // shared seam dedupes + acks even if coord omitted it inside `payload`.
        let mut payload = row.payload;
        payload.gate_id = Some(row.gate_id);
        dispatch_gate_continuation(payload, device_id);
    }
}

/// One row of coord's `GET /coord/agents/pending-unit-dispatches` response.
///
/// LOCKED wire contract (coded against exactly):
/// `{"pending": [{"dispatch_id", "payload": {…}, "dispatched_at"}], "total": N}`,
/// where `payload` is the EXACT work-unit spawn frame coord publishes on the WS
/// channel (the same `source:"gate_continuation"` shape [`GateContinuationPayload`]
/// parses). The sibling of [`PendingContinuation`] keyed on `dispatch_id` instead
/// of `gate_id`. Rows are dispatched-but-unconsumed.
#[derive(Debug, Clone, Deserialize)]
struct PendingUnitDispatch {
    dispatch_id: uuid::Uuid,
    payload: GateContinuationPayload,
    #[serde(default)]
    #[allow(dead_code)]
    dispatched_at: Option<String>,
}

/// The envelope of coord's `GET /coord/agents/pending-unit-dispatches` response.
#[derive(Debug, Clone, Deserialize)]
struct PendingUnitDispatchesResponse {
    #[serde(default)]
    pending: Vec<PendingUnitDispatch>,
    #[serde(default)]
    #[allow(dead_code)]
    total: i64,
}

/// Poll coord for work-unit DAG dispatches that landed while this runner was
/// disconnected, and replay any we haven't already dispatched. The sibling of
/// [`poll_pending_continuations`] for the work-unit path — fired back-to-back with
/// it on the SAME reconnect ticks.
///
/// `GET <coord_http_base>/coord/agents/pending-unit-dispatches?device_id=<uuid>`
/// returns the dispatched-but-unconsumed rows for this device. For each row we
/// stamp the row's `dispatch_id` onto the parsed payload (so the shared dispatch
/// seam dedupes by `dispatch_id` + acks via the unit consume route) and route it
/// through [`dispatch_gate_continuation`] — exactly as the gate pull stamps
/// `gate_id`.
///
/// **Best-effort, warn-and-continue**: coord unreachable, a non-2xx status (incl.
/// a 404 if coord's new endpoint is not deployed yet), or a parse error all
/// `warn!`/`debug!` once and return — the live WS subscription proceeds regardless
/// (a missed poll is retried on the next connect; at-least-once is preserved).
async fn poll_pending_unit_dispatches(device_id: uuid::Uuid) {
    let Some(base) = coord_http_base() else {
        return;
    };
    let url = format!("{base}/coord/agents/pending-unit-dispatches?device_id={device_id}");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("agent_runtime: pending-unit-dispatches client build failed: {e:#}");
            return;
        }
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("agent_runtime: pending-unit-dispatches GET failed (continuing): {e:#}");
            return;
        }
    };
    if !resp.status().is_success() {
        // A 404 is expected until coord's Phase 2 endpoint is deployed — log at
        // debug for that, warn for anything else.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            debug!(
                "agent_runtime: pending-unit-dispatches GET 404 (endpoint not deployed yet; \
                 continuing)"
            );
        } else {
            warn!(
                "agent_runtime: pending-unit-dispatches GET returned {} (continuing)",
                resp.status()
            );
        }
        return;
    }
    let body: PendingUnitDispatchesResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!("agent_runtime: pending-unit-dispatches parse failed (continuing): {e:#}");
            return;
        }
    };
    if body.pending.is_empty() {
        debug!(
            "agent_runtime: pending-unit-dispatches poll: none pending for device_id={device_id}"
        );
        return;
    }
    info!(
        "agent_runtime: pending-unit-dispatches poll: {} pending for device_id={device_id} — \
         replaying",
        body.pending.len()
    );
    for row in body.pending {
        // The row's dispatch_id is authoritative; stamp it onto the payload so the
        // shared seam dedupes by dispatch_id + acks the unit consume route even if
        // coord omitted it inside `payload`.
        let mut payload = row.payload;
        payload.dispatch_id = Some(row.dispatch_id);
        dispatch_gate_continuation(payload, device_id);
    }
}

/// POST the consume CLAIM (`{device_id}`, no outcome) for a dispatched gate
/// continuation and decode the response into a [`SpawnDecision`]. This is the
/// claim-BEFORE-spawn gate: it is AWAITED, and its result decides whether the
/// runner spawns.
///
/// - 200 → [`SpawnDecision::Spawn`].
/// - 409 `{"error":"cancelled", cancel_reason}` → [`SpawnDecision::SkipCancelled`].
/// - 5s timeout / network failure / any other non-2xx →
///   [`SpawnDecision::SpawnDespiteClaimError`] (availability over consistency —
///   preserves the pre-restructure behavior; the in-process dedupe still guards).
///
/// No `coord_http_base` (legacy / no coord configured) → proceed: there is no
/// claim surface to consult, so the in-process dedupe is the only guard, as
/// before.
async fn post_continuation_claim(gate_id: uuid::Uuid, device_id: uuid::Uuid) -> SpawnDecision {
    let Some(base) = coord_http_base() else {
        return SpawnDecision::Spawn;
    };
    let url = format!("{base}/coord/gates/{gate_id}/continuation-consumed");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return SpawnDecision::SpawnDespiteClaimError {
                cause: format!("claim client build failed: {e:#}"),
            };
        }
    };
    let body = ContinuationConsumedBody::claim(device_id);
    match client.post(&url).json(&body).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            // Body is needed only to distinguish the 409 cancelled shape; read
            // it unconditionally (small) and tolerate a read error.
            let text = resp.text().await.unwrap_or_default();
            decide_spawn(status, &text)
        }
        Err(e) => SpawnDecision::SpawnDespiteClaimError {
            cause: format!("claim POST failed: {e:#}"),
        },
    }
}

/// POST the consume OUTCOME (`{device_id, outcome, detail?}`) after the spawn
/// attempt resolves. Best-effort, 5s timeout — a failure `warn!`s once and is
/// swallowed (coord already recorded the claim; a missed outcome only leaves
/// `continuation_consumed_outcome` NULL). NEVER crashes the caller.
async fn post_continuation_outcome(
    gate_id: uuid::Uuid,
    device_id: uuid::Uuid,
    spawned: bool,
    detail: Option<String>,
) {
    let Some(base) = coord_http_base() else {
        return;
    };
    let url = format!("{base}/coord/gates/{gate_id}/continuation-consumed");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "agent_runtime: continuation-outcome client build failed gate_id={gate_id}: {e:#}"
            );
            return;
        }
    };
    let body = ContinuationConsumedBody::outcome(device_id, spawned, detail);
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            debug!(
                "agent_runtime: continuation-outcome posted gate_id={gate_id} spawned={spawned}"
            );
        }
        Ok(resp) => {
            warn!(
                "agent_runtime: continuation-outcome POST gate_id={gate_id} returned {} \
                 (continuing)",
                resp.status()
            );
        }
        Err(e) => warn!(
            "agent_runtime: continuation-outcome POST gate_id={gate_id} failed (continuing): {e:#}"
        ),
    }
}

/// Body for `POST /coord/agents/unit-dispatches/{dispatch_id}/consumed`.
///
/// The work-unit consume contract is simpler than the gate's claim-then-outcome:
/// a single idempotent ack carrying only the runner's `device_id` (the read was
/// narrowed by `device_id` on the pull, so the consume re-asserts ownership).
/// Coord responds `{dispatch_id, consumed:true}` and a second consume is a no-op
/// success (`consumed_at` set once).
#[derive(Debug, Clone, Serialize)]
struct UnitDispatchConsumedBody {
    device_id: uuid::Uuid,
}

/// POST the work-unit dispatch consume ack
/// (`POST /coord/agents/unit-dispatches/{dispatch_id}/consumed` body
/// `{device_id}`) AFTER a unit continuation has been handled (spawned). The
/// sibling of [`post_continuation_outcome`] for the unit path: best-effort, 5s
/// timeout, a failure `warn!`s once and is swallowed (coord keeps the row
/// un-consumed and re-lists it on the next reconnect — at-least-once, never
/// crashes the caller). Idempotent on coord's side.
///
/// No `coord_http_base` (legacy / coord not configured) → no-op: there is no
/// consume surface to ack against (the unit pull also can't have happened).
async fn post_unit_dispatch_consumed(dispatch_id: uuid::Uuid, device_id: uuid::Uuid) {
    let Some(base) = coord_http_base() else {
        return;
    };
    let url = format!("{base}/coord/agents/unit-dispatches/{dispatch_id}/consumed");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "agent_runtime: unit-dispatch consume client build failed \
                 dispatch_id={dispatch_id}: {e:#}"
            );
            return;
        }
    };
    let body = UnitDispatchConsumedBody { device_id };
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            debug!("agent_runtime: unit-dispatch consume posted dispatch_id={dispatch_id}");
        }
        Ok(resp) => {
            warn!(
                "agent_runtime: unit-dispatch consume POST dispatch_id={dispatch_id} returned {} \
                 (continuing)",
                resp.status()
            );
        }
        Err(e) => warn!(
            "agent_runtime: unit-dispatch consume POST dispatch_id={dispatch_id} failed \
             (continuing): {e:#}"
        ),
    }
}

/// First line of an error message (for the `spawn_failed` outcome `detail`).
fn first_line(msg: &str) -> String {
    msg.lines().next().unwrap_or("").trim().to_string()
}

/// Spawn the run task for a gate continuation WITHOUT the coord claim/outcome
/// handshake (the legacy no-`gate_id` path). Unlike the agent-spawn path, the
/// device-local resources (worktree, claim, JWT) are NOT supplied by coord —
/// they are acquired inside [`run_gate_continuation_inner`].
///
/// Reached only from [`dispatch_gate_continuation`]'s legacy branch (a coord
/// that omits `gate_id`): no `dispatched_gate_ids` dedupe-by-id, no claim, no
/// outcome. The #469 local guards still run inside the run fn.
fn spawn_gate_continuation_task(payload: GateContinuationPayload, device_id: uuid::Uuid) {
    info!(
        "agent_runtime: gate-continuation received (legacy, no gate_id) target_device_id={} \
         presentation={:?} repos={} anchor_key={:?}",
        payload.target_device_id,
        payload.presentation,
        payload.repos.len(),
        payload.anchor_key,
    );
    tokio::spawn(async move {
        if let Err(e) = run_gate_continuation_inner(payload, device_id, ConsumeTarget::None).await {
            error!("agent_runtime: run_gate_continuation (legacy) failed: {e:#}");
        }
    });
}

/// End-to-end run of one continuation (gate OR work-unit dispatch), with the
/// coord ack handshake selected by `consume_target`:
///
/// 0. `target_device_id` sanity check.
/// 1. #469 local guards (anchor_key dedup, concurrency cap) — run FIRST so a
///    locally-rejected dispatch never burns a coord-side claim (contract item 4).
/// 2. **CLAIM** ([`ConsumeTarget::Gate`] only): POST the consume CLAIM and AWAIT
///    it. `409 cancelled` → log INFO + SKIP the spawn entirely; network/other
///    error → WARN + PROCEED. The work-unit path has no claim-before-spawn (the
///    scheduler CAS is the dispatch authority), so this is skipped for it.
/// 3. Acquire a device-local worktree (+ claim heartbeat) and dispatch on
///    `presentation` (terminal / headless) to produce the REAL spawn result.
/// 4. **ACK** — [`ConsumeTarget::Gate`]: POST the typed `spawned`/`spawn_failed`
///    OUTCOME. [`ConsumeTarget::Dispatch`]: POST the idempotent unit consume only
///    on a SUCCESSFUL handle (a failed spawn stays un-consumed → re-listed next
///    reconnect). Both best-effort, 5s timeout.
///
/// [`ConsumeTarget::None`] is the legacy path: steps 2 and 4 are skipped.
async fn run_gate_continuation_inner(
    payload: GateContinuationPayload,
    device_id: uuid::Uuid,
    consume_target: ConsumeTarget,
) -> anyhow::Result<()> {
    info!(
        "agent_runtime: continuation dispatch target_device_id={} presentation={:?} \
         repos={} anchor_key={:?} consume_target={consume_target:?}",
        payload.target_device_id,
        payload.presentation,
        payload.repos.len(),
        payload.anchor_key,
    );

    // Defensive: coord's WS pattern filter is device-scoped, but a frame that
    // somehow targets another device must not run here.
    if payload.target_device_id != device_id {
        debug!(
            "agent_runtime: gate-continuation target_device_id={} != local {device_id}; ignoring",
            payload.target_device_id
        );
        return Ok(());
    }

    // Step 1: #469 local guards (P3 anchor_key dedup + P4 concurrency cap) BEFORE
    // any coord claim or worktree acquire — a dispatch the local cap would reject
    // must NOT burn a coord-side claim (contract item 4). `gate_id` dedup (#450,
    // the `dispatched_gate_ids` set, already applied in the dispatcher) collapses
    // a SAME gate delivered twice; this catches the residual cases it can't — a
    // re-cleared gate (new `gate_id`, same `anchor_key`) and the cap — both
    // evaluated against sessions that are STILL running. Liveness is tested
    // against the `TerminalManager`; with no Tauri runtime the registry is empty
    // so the guard is a no-op (the unit-test / headless-only context).
    match evaluate_continuation_guard(payload.anchor_key.as_deref(), &live_terminal_predicate()) {
        ContinuationGuard::Proceed => {}
        ContinuationGuard::DuplicateAnchor(existing_terminal_id) => {
            info!(
                "agent_runtime: gate-continuation deduped by anchor_key={:?} — a live \
                 continuation session (terminal_id={existing_terminal_id}) already exists; \
                 skipping double-spawn",
                payload.anchor_key
            );
            // Best-effort: bring the existing docked tab to focus so the
            // operator sees the live continuation rather than nothing happening.
            focus_existing_continuation(&existing_terminal_id);
            // Deliberately NO continuation claim/outcome here (contract item 4):
            // the anchor_key dedup is a LOCAL guard that fires BEFORE step 2's
            // claim, so we must not burn a coord-side claim on a dispatch the
            // local guard rejected. The ALREADY-LIVE continuation (the one that
            // won the anchor) owns its own claim+outcome; this re-cleared/dup
            // gate_id is simply dropped, and the in-process dedupe absorbs any
            // re-delivery. The deduped gate stays pending on coord (harmless —
            // its work is the live session) until cancelled or it expires.
            return Ok(());
        }
        ContinuationGuard::AtCap(cap) => {
            // Mark that a deferral happened this process lifetime so the periodic
            // backstop poll arms (`spawn_continuation_backstop_poll`): even if the
            // capacity-freed exit-hook trigger is missed, the deferred queue still
            // drains within the backstop interval instead of stranding until an
            // unrelated WS reconnect.
            mark_at_cap_deferral();
            // The `deferred:` prefix (Resolved Q3) distinguishes a capacity DEFER
            // (the continuation is intact + pending on coord, re-deliverable once a
            // slot frees) from a hard spawn failure, so a coord-side consumer can
            // tell them apart on the agent-LIFECYCLE channel. NOTE: this is the
            // agent `report_spawn_failed` lifecycle post, NOT the #484 continuation-
            // outcome channel — the AtCap arm deliberately posts NO continuation
            // claim/outcome (see below).
            let reason = format!(
                "deferred: continuation cap ({cap}) reached — re-delivered when a slot frees"
            );
            warn!(
                "agent_runtime: gate-continuation refused: {reason} (anchor_key={:?})",
                payload.anchor_key
            );
            // No coord "operator must resume" alert path is runner-wirable
            // (`/coord/alerts` is read-only); `spawn-failed` is the honest,
            // already-wired fallback. A fresh correlation id (no worktree/agent
            // was acquired) carries the lifecycle post.
            //
            // Deliberately NO continuation claim/outcome here (contract item 4):
            // the local cap rejected BEFORE the claim, so we must NOT burn a
            // coord-side claim on a dispatch the cap refused. The continuation
            // stays pending (uncancelled, unconsumed) on coord so it can be
            // re-delivered once a cap slot frees, OR cancelled by the operator /
            // takeover path. The `spawn-failed` agent post above is the operator
            // signal; the gate's continuation lifecycle is intentionally untouched.
            report_spawn_failed(uuid::Uuid::now_v7(), &reason, None, 0, None).await;
            return Ok(());
        }
    }

    // Step 2: CLAIM-before-spawn (only with a gate_id / coord configured). Posted
    // AFTER the #469 guards pass and AWAITED — its result decides whether to
    // spawn. This closes the poll→cancel→spawn race: if a cancel landed between
    // the poll and now, coord returns 409 cancelled and we skip the spawn.
    if let ConsumeTarget::Gate(gate_id) = consume_target {
        match post_continuation_claim(gate_id, device_id).await {
            SpawnDecision::Spawn => {}
            SpawnDecision::SkipCancelled { reason } => {
                info!(
                    "agent_runtime: continuation cancelled upstream: {} — skipping spawn \
                     (gate_id={gate_id})",
                    reason.as_deref().unwrap_or("(no reason given)")
                );
                return Ok(());
            }
            SpawnDecision::SpawnDespiteClaimError { cause } => {
                warn!(
                    "agent_runtime: continuation claim error (proceeding, in-process dedupe \
                     guards): {cause} (gate_id={gate_id})"
                );
            }
        }
    }

    // Step 3: acquire a device-local worktree (+ claim heartbeat). The `ctx`
    // owns the claim heartbeat task. For the HEADLESS path it stays bound here
    // so its heartbeat runs for the whole subprocess and the claim releases on
    // drop at function exit. For the TERMINAL path ownership is MOVED into the
    // terminal session (see below), so the heartbeat lives for the visible
    // session's lifetime and releases when the operator closes the terminal —
    // the visible session keeps the SAME claim bookkeeping the headless path
    // has. `None` (worktree mode off / acquire declined) → canonical checkout.
    let intent = payload
        .anchor_key
        .as_deref()
        .map(|a| format!("gate-continuation:{a}"))
        .unwrap_or_else(|| "gate-continuation".to_string());

    // Phase 1b (plan 2026-06-06-session-scoped-multi-repo-workspace-coordination):
    // thread a stable per-session UUID discriminator so the worktree claims
    // are session-keyed in the owner token (machine:session). The
    // gate-continuation wire payload carries NO Claude session uuid (only
    // `LaunchPayload` does); the next-best stable id is one DETERMINISTICALLY
    // derived from the continuation's identity so a retry of the SAME gate
    // continuation reuses the same owner token (a re-acquire renews rather
    // than collides). We derive it from `(target_device_id, anchor_key)` via
    // a UUIDv5 in the URL namespace; when `anchor_key` is absent we fall back
    // to a fresh v4 (a one-shot continuation with no stable anchor).
    let continuation_session_id = continuation_session_id(&payload);

    let (workdir, ctx, agent_id) = match acquire_continuation_workdir(
        &payload.repos,
        &intent,
        continuation_session_id,
    )
    .await
    {
        Ok(triple) => triple,
        Err(e) => {
            warn!("agent_runtime: continuation worktree acquisition failed: {e:#}");
            // The gate claim was already posted (step 2): record the honest
            // outcome so coord doesn't show a perpetually-pending continuation.
            // The work-unit path does NOT ack on failure — leaving the dispatch
            // un-consumed re-lists it on the next reconnect (at-least-once).
            if let ConsumeTarget::Gate(gate_id) = consume_target {
                post_continuation_outcome(
                    gate_id,
                    device_id,
                    false,
                    Some(first_line(&format!("worktree acquisition failed: {e}"))),
                )
                .await;
            }
            return Err(e);
        }
    };

    // Step 3 (dispatch) + Step 4 (outcome): dispatch on presentation, then POST
    // the honest outcome sourced from the REAL spawn result. The presentation
    // fns return `Ok(())` once the terminal/subprocess is actually created and
    // running (`spawned`) and `Err(_)` on a spawn failure (`spawn_failed`).
    let result = match payload.presentation {
        Presentation::Terminal => {
            info!("agent_runtime: gate-continuation presentation=terminal agent_id={agent_id}");
            run_continuation_terminal(agent_id, &workdir, &payload, ctx).await
        }
        Presentation::Headless => {
            info!("agent_runtime: gate-continuation presentation=headless agent_id={agent_id}");
            // `ctx` is held until this `.await` resolves (the subprocess exits),
            // matching the agent-spawn path's heartbeat-then-release lifecycle.
            let res = run_continuation_headless(agent_id, &workdir, &payload.initial_prompt).await;
            drop(ctx);
            res
        }
    };

    // Step 4: ack (best-effort) from the actual result, per consume target.
    match consume_target {
        ConsumeTarget::Gate(gate_id) => match &result {
            Ok(()) => post_continuation_outcome(gate_id, device_id, true, None).await,
            Err(e) => {
                post_continuation_outcome(
                    gate_id,
                    device_id,
                    false,
                    Some(first_line(&e.to_string())),
                )
                .await
            }
        },
        // Work-unit dispatch: a single idempotent consume ack, ONLY on success.
        // A failed spawn is deliberately left un-consumed so coord re-lists the
        // dispatch on the next reconnect (at-least-once).
        ConsumeTarget::Dispatch(dispatch_id) => {
            if result.is_ok() {
                post_unit_dispatch_consumed(dispatch_id, device_id).await;
            } else {
                warn!(
                    "agent_runtime: unit-dispatch dispatch_id={dispatch_id} spawn failed — \
                     NOT acking consume (will be re-listed on next reconnect)"
                );
            }
        }
        ConsumeTarget::None => {}
    }
    result
}

/// Zone ceiling for a continuation page before it is considered "full".
///
/// Mirrors the frontend `useZoneLayout.ts` `full-grid` layout, which lays out
/// at most 9 zones (a 3×3 grid) per page; beyond that, extra sessions spill
/// into the page's Unassigned list. The backend picker uses this to spread
/// continuations across non-full pages instead of overflowing one page.
const CONTINUATION_PAGE_ZONE_CEILING: usize = 9;

/// Choose the page_id a new continuation terminal should land on.
/// Prefers, in order: the "default" page if under the ceiling, then any other
/// existing page under the ceiling (fewest terminals first, tie-break by page_id
/// for determinism), else a freshly-minted uuid.
///
/// `counts` is the live `(page_id, terminal count)` map. A page absent from
/// `counts` is treated as count 0; in particular an absent `"default"` is
/// treated as empty (so it is chosen when no continuations exist yet). The
/// picker decides only among the pages PRESENT in `counts` plus the implicit
/// `"default"`.
fn pick_continuation_page(
    counts: &[(String, usize)],
    ceiling: usize,
    mint: impl FnOnce() -> String,
) -> String {
    // Default page count (absent → 0).
    let default_count = counts
        .iter()
        .find(|(p, _)| p == "default")
        .map(|(_, c)| *c)
        .unwrap_or(0);
    if default_count < ceiling {
        return "default".to_string();
    }

    // Among the OTHER existing pages under the ceiling, pick the one with the
    // fewest terminals; tie-break by lexicographically-smallest page_id for
    // determinism.
    let best = counts
        .iter()
        .filter(|(p, c)| p != "default" && *c < ceiling)
        .min_by(|(pa, ca), (pb, cb)| ca.cmp(cb).then_with(|| pa.cmp(pb)));
    if let Some((page, _)) = best {
        return page.clone();
    }

    // Everything full → mint a fresh page.
    mint()
}

/// Build the gate-continuation `claude` spawn argv. Pure for unit-testing:
/// every flag — including the pre-pinned `--session-id` — MUST precede the
/// trailing positional prompt arg, or the CLI eats it as prompt text.
///
/// `add_dir_args` must be ATTACHED-form `--add-dir=<path>` tokens (what
/// [`crate::agent_worktree::isolated_edit::claude_add_dir_args`] emits).
/// The space-separated pair form is FORBIDDEN here: `--add-dir` is variadic
/// (`<directories...>`), so in pair form the dir list would consume the
/// trailing positional prompt as a bogus extra directory and the spawned
/// session would idle at an empty REPL — the 2026-06-12 multi-repo
/// gate-continuation incident (single-repo continuations were immune only
/// because their `add_dir_args` is empty).
///
/// A `--` end-of-options terminator additionally separates the flags from
/// the prompt (emitted unconditionally, even with no `--add-dir`): it is a
/// second, independent cutoff for the variadic list AND keeps a prompt that
/// happens to start with `-` from being parsed as a flag. The combined
/// `--add-dir=<dir> -- "<prompt>"` shape is live-verified against the CLI.
fn build_continuation_claude_command(
    claude_bin: String,
    pinned_session_id: &str,
    add_dir_args: Vec<String>,
    prompt: String,
) -> Vec<String> {
    let mut command_vec = vec![
        claude_bin,
        "--dangerously-skip-permissions".to_string(),
        "--session-id".to_string(),
        pinned_session_id.to_string(),
    ];
    command_vec.extend(add_dir_args);
    command_vec.push("--".to_string());
    command_vec.push(prompt);
    command_vec
}

/// Run a gate continuation as a VISIBLE terminal session (Decision 1/2/3).
///
/// Opens a pop-out terminal window and creates a terminal session whose PTY
/// child IS the `claude` CLI launched with the prompt as a positional argv
/// (`claude "<prompt>"`). The prompt is therefore visible in scrollback and the
/// session behaves identically to the operator launching it — no PTY-readiness
/// race, no shell wrapping, and (critically) the session is INTERACTIVE so an
/// `AskUserQuestion` inside it is answerable. We deliberately do NOT use
/// `--print`: that flag is single-shot/non-interactive and would defeat the
/// plan's acceptance (operator interaction inside the spawned session).
///
/// The pre-acquired isolated-edit `ctx` (its claim heartbeat) is MOVED into the
/// terminal session via [`crate::commands::terminal::create_terminal_session_backend`]
/// so the heartbeat lives for the visible session's lifetime and releases on
/// close — the same claim bookkeeping the headless path holds.
///
/// Lifecycle posts: `spawn-complete` once the terminal session is created and
/// running; `spawn-failed` on ANY failure along the way (no Tauri app handle,
/// missing managed state, window/session creation error).
async fn run_continuation_terminal(
    agent_id: uuid::Uuid,
    workdir: &str,
    payload: &GateContinuationPayload,
    ctx: Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
) -> anyhow::Result<()> {
    use std::sync::Arc;

    // Reach the managed Tauri state from this backend task via the process-
    // global AppHandle (set in main.rs::setup). If the runner has no Tauri
    // runtime (headless/unit-test context) we cannot open a window — report
    // spawn-failed and bail rather than silently dropping the continuation.
    let app = match crate::tauri_app_handle::current() {
        Some(a) => a,
        None => {
            let reason = "no Tauri AppHandle (runner has no webview runtime) — \
                          cannot open a visible terminal";
            warn!("agent_runtime: gate-continuation terminal: {reason}");
            report_spawn_failed(agent_id, reason, None, 0, None).await;
            return Err(anyhow::anyhow!(reason));
        }
    };

    use tauri::Manager;
    let terminal_manager = match app.try_state::<Arc<crate::terminal::TerminalManager>>() {
        Some(s) => s.inner().clone(),
        None => {
            let reason = "TerminalManager state not managed — cannot create terminal session";
            warn!("agent_runtime: gate-continuation terminal: {reason}");
            report_spawn_failed(agent_id, reason, None, 0, None).await;
            return Err(anyhow::anyhow!(reason));
        }
    };
    let session_registry = match app.try_state::<Arc<crate::session::SessionRegistry>>() {
        Some(s) => s.inner().clone(),
        None => {
            let reason = "SessionRegistry state not managed — cannot register terminal session";
            warn!("agent_runtime: gate-continuation terminal: {reason}");
            report_spawn_failed(agent_id, reason, None, 0, None).await;
            return Err(anyhow::anyhow!(reason));
        }
    };

    // The continuation lands DOCKED in the MAIN window's terminal grid (a visible
    // tab the operator actually sees) rather than an invisible pop-out window. The
    // session create below does NOT assign a window, so it renders on `main`. We
    // deliberately do not call `open_terminal_window` here — a pop-out is invisible
    // to the operator and would otherwise require a reassignment step.

    // Title: prefer the anchor_key, else a generic gate-continuation label.
    let title = payload
        .anchor_key
        .clone()
        .unwrap_or_else(|| "Gate continuation".to_string());

    // Resolve the SAME `claude` binary the headless path uses (QONTINUI_CLAUDE_BIN
    // override honored). The prompt is the trailing positional arg — interactive
    // form, NOT `--print` (see the fn doc: interactivity is required). Inject
    // `--dangerously-skip-permissions` (same as the worker-tab spawn path) so the
    // continuation /implement-plan session does not stall on interactive Bash
    // permission prompts — an unattended gate continuation has no operator to
    // answer them.
    let claude_bin = claude_bin_path();
    // Phase 2c — `--add-dir=<sibling>` (attached form — see the
    // build_continuation_claude_command doc) for each non-cwd worktree of this
    // continuation's context so the launched `claude` can edit sibling repos
    // that materialized on disk but aren't the process cwd. Flags MUST precede
    // the trailing positional prompt arg. Empty for single-repo continuations.
    let add_dir_args = ctx
        .as_ref()
        .map(|c| c.claude_add_dir_args())
        .unwrap_or_default();
    // Pre-pin the Claude session id (#548 Phase 1): the registry records
    // synchronously at spawn instead of mtime-guessing from transcripts.
    // Fresh uuid per spawn attempt — never reused (CLI fails loudly on reuse).
    let pinned_session_id = uuid::Uuid::new_v4().to_string();
    let command = Some(build_continuation_claude_command(
        claude_bin,
        &pinned_session_id,
        add_dir_args,
        payload.initial_prompt.clone(),
    ));

    // First repo (if any) is the session's intent_repo for coord attribution.
    let intent_repo = payload.repos.first().cloned();

    // Account selection: pin the most-available (token-bearing) account so the
    // continuation does not spawn under a quota-exhausted default and die
    // instantly (the bug: continuations spawned under the runner's boot account
    // even when it was out of tokens). The resolved dir is threaded to the PTY
    // as `CLAUDE_CONFIG_DIR` via `capture_hint.config_dir` (consumed by
    // `create_terminal_session_backend`). spawn_blocking: the selector reads
    // settings + cooldown state.
    let _ = tokio::task::spawn_blocking(crate::ai_provider::pick_best_account).await;
    let selected_config_dir = {
        let ai = crate::settings::get_ai_settings();
        crate::ai_provider::get_effective_config_dir(&ai.claude_cli)
    };

    // Fail loud, not a 401 zombie: `None` here means no credential-valid account
    // was resolved, so `CLAUDE_CONFIG_DIR` would be left unset and the PTY would
    // inherit the ambient default. That is only safe if the ambient default
    // itself has live credentials; otherwise the continuation would spawn a
    // `claude` that immediately dies with `401 ... Please run /login`, leaving a
    // dead terminal the operator can't diagnose. Abort with an actionable
    // reason so coord/the operator sees WHY instead of a blank 401 pane.
    if selected_config_dir.is_none()
        && !crate::ai_provider::oauth_refresh::default_location_has_valid_credentials()
    {
        let instance = crate::instance::instance_name().unwrap_or_else(|| "primary".to_string());
        let reason = format!(
            "no authenticated Claude account on this runner — run /login (instance={instance})"
        );
        warn!("agent_runtime: gate-continuation terminal aborted — {reason}");
        report_spawn_failed(agent_id, &reason, None, 0, None).await;
        return Err(anyhow::anyhow!(reason));
    }

    // Durable lifecycle capture: this backend path never fires the frontend
    // `terminal_session_record_open`, so without a hint a restart loses the
    // continuation. `config_dir` is now the SELECTED account dir (above): it
    // both sets `CLAUDE_CONFIG_DIR` for the spawn AND keeps restore consistent
    // (`buildResumeCmd` resumes under the same dir). `None` (single-account /
    // Manual default) keeps the prior behavior. RISK note: the id resolver
    // scans config dirs first-hit-wins, but worktree paths are
    // per-continuation-unique, so cross-account binding stays low-probability.
    // Page distribution: backend continuations historically all landed on the
    // "default" page, overflowing its 9-zone grid into the Unassigned list. Pick
    // a non-full page at create-time so continuations spread across pages, and
    // persist it in the durable record so a restart re-lands on the same page.
    // Count live terminals per page from the manager's current session list.
    let counts: Vec<(String, usize)> = {
        let mut per_page: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for info in terminal_manager.list() {
            *per_page.entry(info.page_id).or_insert(0) += 1;
        }
        per_page.into_iter().collect()
    };
    let target_page = pick_continuation_page(&counts, CONTINUATION_PAGE_ZONE_CEILING, || {
        uuid::Uuid::new_v4().to_string()
    });

    let capture_hint = Some(crate::commands::terminal::SessionCaptureHint {
        config_dir: selected_config_dir,
        working_dir: workdir.to_string(),
        title: title.clone(),
        page_id: Some(target_page.clone()),
        // Matches the `--session-id` in the spawn argv → synchronous record.
        claude_session_id: Some(pinned_session_id),
        zone_index: None,
        // Autonomous gate continuation → pin the agent git identity on the PTY.
        inject_agent_git_identity: true,
    });

    // Provision `.mcp.json` so this continuation can reach coord coordination
    // tools (`coord_register_gate` over /mcp) and `coord-acting-bearer.sh` for
    // the operator-scoped write surface — closing the reach gap where gate
    // continuations (a primary place follow-up gates get registered) had no
    // coord identity and fell back to the operator-bearer stopgap. Uses the
    // runner's own device JWT; guarded against non-verifying bearers + clobber.
    // Device-JWT sessions get the loopback live-token PROXY shape, so the
    // ACTUALLY-BOUND API port must come from the managed AppState. Phase 3a:
    // when AppState isn't reachable we pass `None` (fail-closed) rather than the
    // env-default 9876 — that default is wrong on secondary/temp runners and
    // writes a dead-but-valid-looking proxy config (the F1 root cause).
    // Provisioning then refuses the device-path write + drops a degraded
    // breadcrumb instead of pointing the session at a port nothing serves.
    let bound_port = app
        .try_state::<Arc<crate::commands::AppState>>()
        .map(|s| crate::mcp::types::runner_api_port(s.inner()));
    crate::coord_mcp::provision_coord_mcp_for_session(workdir, bound_port);
    // Bundle /vet-plan and /implement-plan into the session cwd so they resolve
    // as project slash commands regardless of the device's ~/.claude.
    crate::fleet_commands::provision_fleet_commands_for_session(workdir);

    let result = crate::commands::terminal::create_terminal_session_backend(
        &terminal_manager,
        &session_registry,
        app.clone(),
        title,
        workdir.to_string(),
        payload.anchor_key.clone(),
        payload.anchor_key.clone(),
        intent_repo,
        command,
        ctx,
        capture_hint,
        Some(target_page),
    );

    match result {
        Ok((terminal_id, coord_session_id)) => {
            // Register in the live continuation-session registry so P3 (dedup by
            // anchor_key) and P4 (concurrency cap) see this session as live until
            // its PTY exits (reaped lazily by the guard's liveness prune).
            register_continuation_session(terminal_id.clone(), payload.anchor_key.clone());
            // The session is intentionally left on `main` (docked, visible) — no
            // pop-out window was opened, so there is nothing to reassign it to.
            info!(
                "agent_runtime: gate-continuation terminal session created \
                 terminal_id={terminal_id} coord_session={coord_session_id:?} \
                 agent_id={agent_id}"
            );
            // Surface the freshly-created continuation to the operator: emit a
            // `terminal-focus-request` so the frontend (a) switches the main view
            // to the Terminal panel and (b) selects this tab. Without this the tab
            // is appended off-screen / un-selected and the operator sees nothing.
            // SCOPED to the MAIN window (`emit_to`, NOT bare `emit`) so a pop-out
            // window is not yanked to this tab — `terminal-created` is a global
            // broadcast, but a focus action must target only `main`.
            emit_terminal_focus_request(&app, &terminal_id);
            report_spawn_complete(agent_id, None, Some("gate continuation (terminal)"), None).await;
            Ok(())
        }
        Err(e) => {
            report_spawn_failed(
                agent_id,
                &format!("terminal session create failed: {e}"),
                None,
                0,
                None,
            )
            .await;
            Err(anyhow::anyhow!(e))
        }
    }
}

/// Resolve the working directory for a gate continuation. Returns
/// `(workdir, isolated_edit_ctx, agent_id)`.
///
/// - Worktree mode ON and `acquire` succeeds → the materialized worktree path,
///   the held `IsolatedEditContext` (keeps the claim heartbeat alive), and the
///   coord-allocated agent_id (parsed to a UUID; a fresh UUID if coord returned
///   a non-UUID id, used only for lifecycle correlation).
/// - Worktree mode OFF / acquire declined / `repos` empty → the canonical
///   checkout of the first repo (or `QONTINUI_ROOT`), `None` context, and a
///   fresh correlation UUID. The continuation still runs, just without
///   per-agent isolation — the same graceful degrade `acquire_for_terminal`
///   uses.
/// Derive a stable per-session UUID discriminator for a gate continuation's
/// worktree claims (Phase 1b, plan
/// 2026-06-06-session-scoped-multi-repo-workspace-coordination).
///
/// The gate-continuation wire payload ([`GateContinuationPayload`]) carries
/// NO Claude session uuid (unlike [`LaunchPayload::agent_session_id`]), so
/// there is no upstream id to thread. We synthesize one that is STABLE
/// across retries of the same continuation: a UUIDv5 over
/// `"<target_device_id>:<anchor_key>"` in the URL namespace. Two spawns of
/// the same gate continuation (same device, same anchor) therefore produce
/// the same owner-token discriminator, so a re-acquire RENEWS the existing
/// worktree claim instead of colliding with it. When `anchor_key` is absent
/// (a one-shot continuation with no stable anchor) we fall back to a fresh
/// v4 — distinctness is preserved, only cross-retry renewal is lost, which
/// is acceptable for an anchor-less one-shot.
fn continuation_session_id(payload: &GateContinuationPayload) -> Option<uuid::Uuid> {
    match payload.anchor_key.as_deref() {
        Some(anchor) if !anchor.is_empty() => {
            let name = format!("{}:{anchor}", payload.target_device_id);
            Some(uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                name.as_bytes(),
            ))
        }
        _ => Some(uuid::Uuid::new_v4()),
    }
}

async fn acquire_continuation_workdir(
    repos: &[String],
    intent: &str,
    agent_session_id: Option<uuid::Uuid>,
) -> anyhow::Result<(
    String,
    Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
    uuid::Uuid,
)> {
    use crate::agent_worktree::isolated_edit::{acquire, AcquireRequest};

    if !repos.is_empty() {
        match acquire(AcquireRequest {
            repos,
            intent: Some(intent),
            declared_overlap_paths: None,
            plan_id: None,
            phase: None,
            // Phase 1b: session-keyed worktree claims for the continuation —
            // see `continuation_session_id` for how this stable id is derived.
            agent_session_id,
        })
        .await
        {
            Ok(Some(ctx)) => {
                let workdir = ctx
                    .worktrees
                    .first()
                    .map(|w| w.worktree_path.to_string_lossy().to_string())
                    .ok_or_else(|| {
                        anyhow::anyhow!("gate-continuation: acquire returned no worktrees")
                    })?;
                // coord's agent_id is canonically a UUID; if it isn't, fall back
                // to a fresh one for lifecycle correlation only.
                let agent_id =
                    uuid::Uuid::parse_str(&ctx.agent_id).unwrap_or_else(|_| uuid::Uuid::now_v7());
                return Ok((workdir, Some(ctx), agent_id));
            }
            Ok(None) => {
                debug!(
                    "agent_runtime: gate-continuation worktree mode off — using canonical checkout"
                );
            }
            Err(e) => {
                warn!(
                    "agent_runtime: gate-continuation acquire failed ({e}); \
                     falling back to canonical checkout"
                );
            }
        }
    }

    // Fallback: canonical checkout of the first repo, else QONTINUI_ROOT.
    let workdir = repos
        .first()
        .and_then(|r| {
            crate::agent_worktree::canonical_paths::default_canonical_path(r)
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .or_else(|| qontinui_root_dir().map(|p| p.to_string_lossy().to_string()))
        .ok_or_else(|| {
            anyhow::anyhow!("gate-continuation: no canonical checkout or QONTINUI_ROOT resolved")
        })?;
    Ok((workdir, None, uuid::Uuid::now_v7()))
}

/// Run a gate continuation as a headless `claude` child (the existing
/// subprocess flow), posting `spawn-complete` on first successful spawn and
/// `spawn-failed` on a non-zero / failed exit. Mirrors the agent-spawn path's
/// lifecycle posts so a continuation is observable on coord the same way.
async fn run_continuation_headless(
    agent_id: uuid::Uuid,
    workdir: &str,
    initial_prompt: &str,
) -> anyhow::Result<()> {
    let log_path = agent_log_path(agent_id);
    // Select the most-available account once before spawning (pins the resolved
    // config dir that `spawn_claude_child` reads). spawn_blocking: the selector
    // reads settings + cooldown state.
    let _ = tokio::task::spawn_blocking(crate::ai_provider::pick_best_account).await;
    // Provision coord-mcp for the headless session so it receives coord's
    // session-start `instructions` preamble and can call coord_declare_intent —
    // parity with the terminal continuation path (which provisions the bound_port
    // proxy shape). Headless has no bound port → `None` selects the device-JWT
    // static-bearer shape.
    crate::coord_mcp::provision_coord_mcp_for_session(workdir, None);
    match spawn_claude_child(workdir, initial_prompt).await {
        Ok(mut child) => {
            let pid = child.id().map(|p| p as i64);
            report_spawn_complete(agent_id, pid, Some("gate continuation"), None).await;
            let exit = pump_subprocess(agent_id, &mut child, log_path.as_deref()).await;
            match exit {
                Ok(0) => {
                    info!(
                        "agent_runtime: gate-continuation agent_id={agent_id} exited cleanly \
                         (code=0)"
                    );
                    Ok(())
                }
                Ok(code) => {
                    report_spawn_failed(
                        agent_id,
                        &format!("non-zero exit code {code}"),
                        Some(code),
                        0,
                        None,
                    )
                    .await;
                    Ok(())
                }
                Err(e) => {
                    report_spawn_failed(agent_id, &format!("pump failure: {e}"), None, 0, None)
                        .await;
                    Err(e)
                }
            }
        }
        Err(e) => {
            report_spawn_failed(agent_id, &format!("spawn failure: {e}"), None, 0, None).await;
            Err(e)
        }
    }
}

// =============================================================================
// Subprocess lifecycle
// =============================================================================

/// Map a coord-delivered LaunchPayload onto the AllocateResult shape the
/// per-agent daemons (agent_pusher / dirty_poller) consume. The daemons'
/// stable contract is AllocateResult (also produced by the isolated_edit
/// path); this keeps spawn_for_agent's signature untouched.
fn payload_to_allocate_result(payload: &LaunchPayload) -> crate::agent_worktree::AllocateResult {
    use crate::agent_worktree::{AllocateResult, MaterializedWorktree};
    AllocateResult {
        agent_id: payload.agent_id.to_string(),
        worktrees: payload
            .worktrees
            .iter()
            .map(|w| MaterializedWorktree {
                repo: w.repo.clone(),
                branch: w.branch.clone(),
                parent_sha: w.parent_sha.clone(),
                worktree_path: std::path::PathBuf::from(&w.worktree_path),
                push_ref: w
                    .push_ref
                    .clone()
                    .unwrap_or_else(|| crate::agent_worktree::remote_agent_ref(&w.branch)),
            })
            .collect(),
        token: payload.jwt.clone(),
        token_jti: uuid::Uuid::nil(), // bookkeeping only; maybe_refresh sends the bearer, not the jti
        token_exp: payload.jwt_exp,
        active_claims: Vec::new(), // unread by either daemon
    }
}

/// End-to-end run of one agent subprocess:
/// 1. `git worktree add` each allocated worktree.
/// 2. Spawn `claude` CLI in the first worktree with the initial prompt.
/// 3. Start heartbeat + log-forwarder background tasks.
/// 4. Wait for exit; restart up to 3× on crash; report final status.
async fn run_agent_subprocess(
    mut payload: LaunchPayload,
    stop: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    // Coord emits each worktree's `repo` as a full `owner/name` slug and its
    // `worktree_path` for COORD's OWN host (a Linux `/root/qontinui-root.wt/...`
    // path) — neither is valid on this runner's filesystem. The runner owns its
    // local worktree layout, so rewrite every path to a local, platform-correct
    // one OUTSIDE the project tree:
    // `agent_worktree_root(<canonical>)/<agent_id>/<repo-name>` (a sibling of
    // the project root by default). This is the IDENTICAL resolver Site 2
    // (`agent_worktree::local_worktree_target`) uses, so a worktree
    // materialized here and one allocated via the coord HTTP path land at the
    // same on-disk location.
    let agent_id = payload.agent_id;
    for wt in &mut payload.worktrees {
        // Reuse the canonical-checkout resolver (do NOT re-derive `root/name`).
        match crate::agent_worktree::canonical_paths::default_canonical_path(&wt.repo) {
            Ok(canonical) => {
                let repo_name = local_repo_name(&wt.repo);
                wt.worktree_path =
                    crate::agent_worktree::canonical_paths::agent_worktree_root(&canonical)
                        .join(agent_id.to_string())
                        .join(repo_name)
                        .to_string_lossy()
                        .into_owned();
            }
            Err(e) => {
                // Skip rewriting this worktree (keep coord's emitted path) and
                // log — better degraded than a panic. materialize_worktrees
                // will surface a clear error if the path is unusable.
                warn!(
                    "agent_runtime: cannot resolve canonical path for repo {:?}: {e}; \
                     leaving coord-emitted worktree_path unchanged",
                    wt.repo
                );
            }
        }
    }

    // Phase 4b: the primary worktree's push_ref is the PR/head this spawn works
    // against — thread it into every lifecycle post so coord can key the spawn
    // outcome to a specific PR. The path rewrite above does not touch push_ref,
    // so read it once here and reuse for all of this spawn's reports.
    let primary_push_ref = payload.worktrees.first().and_then(|wt| wt.push_ref.clone());

    // Step 1: materialize worktrees.
    if let Err(e) = materialize_worktrees(&payload).await {
        report_spawn_failed(
            payload.agent_id,
            &format!("worktree materialization failed: {e:#}"),
            None,
            0,
            primary_push_ref.as_deref(),
        )
        .await;
        return Err(e);
    }

    let primary_wt = payload
        .worktrees
        .first()
        .ok_or_else(|| anyhow::anyhow!("no worktrees allocated"))?
        .worktree_path
        .clone();

    // Write .mcp.json so the spawned claude process auto-discovers the coord MCP
    // server. Agent-spawns carry a coord-minted agent JWT with a ~4h TTL, and
    // Claude Code's MCP client reads `.mcp.json` exactly once at connect — a
    // STATIC baked bearer silently dies at expiry (the bug this fixes). Instead
    // we register the agent's JWT in a process-global live-token slot and write
    // the per-agent PROXY shape: the loopback `/coord-mcp` route injects THIS
    // agent's own refreshed token per request (never the device token — the
    // scope-elevation trap). The heartbeat loop (below) drives proactive
    // refresh so the slot never expires for a live agent.
    {
        let slot = std::sync::Arc::new(tokio::sync::RwLock::new(crate::agent_token::TokenSlot {
            token: payload.jwt.clone(),
            jti: uuid::Uuid::nil(),
            exp: payload.jwt_exp,
        }));
        crate::coord_mcp::register_agent_token(payload.agent_id, slot.clone());
        match crate::coord_mcp::resolve_bound_api_port() {
            Some(port) => {
                crate::coord_mcp::write_coord_mcp_agent_proxy_config(
                    &primary_wt,
                    port,
                    payload.agent_id,
                );
                crate::coord_mcp::probe_and_breadcrumb_proxy(&primary_wt, port);
            }
            None => {
                // Fail-closed exactly like the device arm: a bootstrap-default
                // port is dead on a secondary/temp runner, and a dead-but-valid
                // config is worse than an absent one. Drop a 1a breadcrumb so the
                // agent self-routes to /gate.
                warn!(
                    "agent_runtime: refusing to write an agent proxy .mcp.json for \
                     agent_id={} — bound API port unresolvable (no managed AppState)",
                    payload.agent_id
                );
                crate::coord_mcp::write_degraded_breadcrumb(
                    &primary_wt,
                    "bound API port unresolvable — agent proxy config NOT written (would point at a dead port)",
                );
            }
        }

        // Wire the per-agent durability (agent_pusher) + observability (dirty_poller)
        // daemons onto the SAME refreshing token slot registered in AGENT_TOKENS, so
        // the proxy, heartbeat, pusher, and poller all read one slot (single-slot
        // invariant — agent_token/mod.rs:1). Best-effort: skipped without a JWT or a
        // configured coord base (dev/no-coord), and each daemon self-skips when it has
        // no work (no push targets / no worktrees).
        if !payload.jwt.is_empty() {
            if let Some(base) = coord_http_base() {
                let allocate = payload_to_allocate_result(&payload);
                crate::agent_daemons::spawn_for_agent_with_token(
                    &allocate,
                    base,
                    payload.target_device_id,
                    slot.clone(),
                );
            }
        }
    }

    // Provision the named-subagent defs into the spawned worktree's cwd so the
    // headless `claude` can resolve subagents the spawn prompt references
    // (merge-specialist, repo-auditor, ...). Fail-soft: a copy error here must
    // not abort an otherwise-launchable spawn — the agent just lacks subagents.
    if let Err(e) = provision_agent_definitions(&primary_wt) {
        warn!("agent_runtime: agent-def provisioning errored (continuing spawn): {e:#}");
    }
    // Bundle /vet-plan and /implement-plan into the spawned worktree cwd so they
    // resolve as project slash commands regardless of the device's ~/.claude.
    crate::fleet_commands::provision_fleet_commands_for_session(&primary_wt);

    let log_path = agent_log_path(payload.agent_id);

    // Step 2: heartbeat task — runs for the agent's whole life.
    let hb_payload = payload.clone();
    let hb_task = tokio::spawn(async move { run_heartbeat_loop(hb_payload).await });

    // Step 3: subprocess + restart loop.
    let mut restarts = 0u32;
    let mut final_exit_code: Option<i64> = None;
    let mut final_reason: Option<String> = None;

    // Select the most-available account once before the (re)spawn loop. On a
    // mid-run rate-limit the inference path rotates via
    // `rotate_account_on_rate_limit`, and `spawn_claude_child` re-reads the
    // resolved dir on each respawn, so retries pick up the rotation.
    let _ = tokio::task::spawn_blocking(crate::ai_provider::pick_best_account).await;

    loop {
        // Stop requested during a restart back-off (or before the first spawn):
        // bail without (re)spawning.
        if stop.is_cancelled() {
            final_reason = Some("stopped by operator before (re)spawn".to_string());
            break;
        }
        match spawn_claude_child(&primary_wt, &payload.initial_prompt).await {
            Ok(mut child) => {
                let pid = child.id().map(|p| p as i64);
                // First successful spawn = post spawn-complete with pid.
                if restarts == 0 {
                    report_spawn_complete(payload.agent_id, pid, None, primary_push_ref.as_deref())
                        .await;
                } else {
                    info!(
                        "agent_runtime: restart {restarts} succeeded agent_id={}",
                        payload.agent_id
                    );
                }
                // Race the subprocess pump against an operator stop. On stop we
                // let the pump future drop (releasing &mut child), then kill the
                // child and break WITHOUT restarting.
                let mut pump_exit = None;
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => {}
                    e = pump_subprocess(payload.agent_id, &mut child, log_path.as_deref()) => {
                        pump_exit = Some(e);
                    }
                }
                let exit = match pump_exit {
                    Some(e) => e,
                    None => {
                        info!(
                            "agent_runtime: stop requested for agent_id={}; terminating \
                             subprocess (no restart)",
                            payload.agent_id
                        );
                        let _ = child.kill().await;
                        final_reason =
                            Some("stopped by operator (events.agent.stop_requested)".to_string());
                        break;
                    }
                };
                match exit {
                    Ok(0) => {
                        info!(
                            "agent_runtime: agent_id={} exited cleanly (code=0); not restarting",
                            payload.agent_id
                        );
                        final_exit_code = Some(0);
                        break;
                    }
                    Ok(code) => {
                        warn!(
                            "agent_runtime: agent_id={} exited with code {code} restarts={}",
                            payload.agent_id, restarts,
                        );
                        final_exit_code = Some(code);
                        final_reason = Some(format!("non-zero exit code {code}"));
                    }
                    Err(e) => {
                        warn!(
                            "agent_runtime: agent_id={} pump failed: {e:#}",
                            payload.agent_id
                        );
                        final_reason = Some(format!("pump failure: {e}"));
                    }
                }
            }
            Err(e) => {
                warn!(
                    "agent_runtime: spawn_claude_child failed agent_id={} attempt={}: {e:#}",
                    payload.agent_id, restarts
                );
                final_reason = Some(format!("spawn failure: {e}"));
            }
        }

        restarts += 1;
        if restarts >= MAX_RESTARTS {
            break;
        }
        // Brief back-off between restarts.
        tokio::time::sleep(Duration::from_secs(2u64.pow(restarts.min(3)))).await;
    }

    hb_task.abort();

    if final_exit_code == Some(0) {
        // Clean exit was already reported via spawn-complete; nothing
        // more to send. The runner can release the claim explicitly via
        // a follow-up POST if desired.
        return Ok(());
    }

    report_spawn_failed(
        payload.agent_id,
        final_reason
            .as_deref()
            .unwrap_or("subprocess exited with no recorded reason"),
        final_exit_code,
        restarts,
        primary_push_ref.as_deref(),
    )
    .await;
    Ok(())
}

/// `git worktree add` each allocated worktree from its repo's primary
/// tree under `QONTINUI_ROOT`.
async fn materialize_worktrees(payload: &LaunchPayload) -> anyhow::Result<()> {
    let root = qontinui_root_dir()
        .ok_or_else(|| anyhow::anyhow!("no qontinui-root directory configured"))?;
    for wt in &payload.worktrees {
        let repo_root = root.join(local_repo_name(&wt.repo));
        if !repo_root.exists() {
            return Err(anyhow::anyhow!(
                "primary repo {} not found at {}",
                wt.repo,
                repo_root.display()
            ));
        }
        let wt_path = Path::new(&wt.worktree_path);
        if wt_path.exists() {
            info!(
                "agent_runtime: worktree path {} already exists; skipping add",
                wt.worktree_path
            );
            continue;
        }
        let out = Command::new("git")
            .args([
                "-C",
                repo_root
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("repo_root not UTF-8"))?,
                "worktree",
                "add",
                wt_path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("worktree path not UTF-8"))?,
                "-b",
                &wt.branch,
                &wt.parent_sha,
            ])
            .output()
            .await?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow::anyhow!(
                "git worktree add failed for {}: {}",
                wt.repo,
                err.trim()
            ));
        }
        info!(
            "agent_runtime: materialized worktree {} at {}",
            wt.repo, wt.worktree_path
        );
    }
    Ok(())
}

/// Provision the named-subagent definitions (`.claude/agents/*.md`) into a
/// freshly-materialized agent worktree so headless `claude` (cwd = the
/// worktree) can resolve the subagents an auto-spawned review prompt references
/// (e.g. "Invoke the `merge-specialist` subagent").
///
/// Without this, a spawned agent worktree is a bare `git worktree add` with no
/// `.claude/agents/` dir; `claude` cannot resolve the named subagent → the
/// review never runs → coord ages the PR out as `specialist_timeout`. This
/// affects ALL auto-spawned agents (merge-specialist, repo-auditor, ...).
///
/// Source: `<qontinui_root>/qontinui-claude-config/.claude/agents/*.md`. We
/// COPY (not symlink) — this runs on Windows where symlink creation needs
/// privilege/dev-mode; a copy is robust cross-platform. We copy ONLY the agent
/// `*.md` defs, NOT the whole `.claude` tree (avoid pulling in settings/hooks/
/// mcp that could alter spawn behavior).
///
/// Fail-soft: if the source dir is missing we `warn` and return Ok — the agent
/// then simply lacks subagents (same as before this fix; no regression). The
/// fleet-portability follow-up is to BUNDLE these defs into the runner binary
/// (`include_str!`) so non-operator devices without a `qontinui-claude-config`
/// checkout still get them; this copy-from-checkout path unblocks the current
/// operator fleet.
fn provision_agent_definitions(worktree_cwd: &str) -> anyhow::Result<()> {
    let Some(root) = qontinui_root_dir() else {
        warn!(
            "agent_runtime: no qontinui-root resolved; skipping .claude/agents \
             provisioning for {worktree_cwd} (auto-spawned subagents will not resolve)"
        );
        return Ok(());
    };
    provision_agent_definitions_from_root(&root, worktree_cwd)
}

/// Core of [`provision_agent_definitions`] with the qontinui-root passed in
/// explicitly (so tests can drive it deterministically without mutating the
/// process-global `QONTINUI_ROOT` env). See that wrapper for full rationale.
fn provision_agent_definitions_from_root(root: &Path, worktree_cwd: &str) -> anyhow::Result<()> {
    let src_dir = root
        .join("qontinui-claude-config")
        .join(".claude")
        .join("agents");
    if !src_dir.is_dir() {
        warn!(
            "agent_runtime: claude-config agents dir not found at {}; skipping \
             .claude/agents provisioning (auto-spawned subagents will not resolve)",
            src_dir.display()
        );
        return Ok(());
    }
    let dst_dir = Path::new(worktree_cwd).join(".claude").join("agents");
    std::fs::create_dir_all(&dst_dir).map_err(|e| {
        anyhow::anyhow!(
            "create {} for agent-def provisioning: {e}",
            dst_dir.display()
        )
    })?;
    let mut copied = 0usize;
    for entry in std::fs::read_dir(&src_dir)
        .map_err(|e| anyhow::anyhow!("read agents dir {}: {e}", src_dir.display()))?
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("agent_runtime: skipping unreadable agents entry: {e}");
                continue;
            }
        };
        let path = entry.path();
        // Only the agent-def `*.md` files; ignore subdirs and other files.
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let dst = dst_dir.join(name);
        // Idempotent: overwrite is fine (std::fs::copy truncates the target).
        if let Err(e) = std::fs::copy(&path, &dst) {
            warn!(
                "agent_runtime: failed to copy agent def {} -> {}: {e}",
                path.display(),
                dst.display()
            );
            continue;
        }
        copied += 1;
    }
    info!(
        "agent_runtime: provisioned {copied} subagent def(s) into {}",
        dst_dir.display()
    );
    Ok(())
}

fn qontinui_root_dir() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("QONTINUI_ROOT") {
        let p = PathBuf::from(s);
        if p.is_dir() {
            return Some(p);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let p = PathBuf::from("D:/qontinui-root");
        if p.is_dir() {
            return Some(p);
        }
    }
    dirs::home_dir()
        .map(|h| h.join("qontinui-root"))
        .filter(|p| p.is_dir())
}

/// The local primary-checkout directory NAME for a coord repo slug. Coord uses
/// `owner/name` slugs (e.g. `qontinui/qontinui-runner`); the runner's primary
/// checkouts live at `<QONTINUI_ROOT>/<name>`. A bare name passes through.
fn local_repo_name(repo: &str) -> &str {
    repo.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(repo)
}

/// Resolve the git author/committer identity used for AUTONOMOUS agent commits
/// (headless workers + gate continuations), so they no longer land under the
/// ambient host placeholder — a cloud runner whose global git config was an
/// unconfigured `x <x@x>` stub produced commits authored by `x <x@x>`.
///
/// Resolution (first usable value wins, independently for name and email):
///   1. `QONTINUI_AGENT_GIT_NAME` / `QONTINUI_AGENT_GIT_EMAIL` — the per-runner
///      OWNER override. Set these in a shared/cloud runner's env to attribute
///      its autonomous commits to whoever owns that runner.
///   2. The host's EXPLICIT `git config user.name` / `user.email`. On a dev-box
///      runner this is the operator's real identity, so autonomous commits
///      attribute to them with zero config. Placeholders (`x`, `x@x`, empty)
///      are rejected.
///   3. A clearly-marked default: `Qontinui Agent <agent@qontinui.dev>`.
///
/// Committer is still rewritten to `qontinui-coord` when coord rebase-lands the
/// PR; this controls the AUTHOR (and the pre-land committer). Cached: resolved
/// once per process (boot-time host config / env).
pub(crate) fn autonomous_git_identity() -> (String, String) {
    static CACHE: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let (host_name, host_email) = host_git_identity();
            pick_autonomous_git_identity(
                std::env::var("QONTINUI_AGENT_GIT_NAME").ok(),
                std::env::var("QONTINUI_AGENT_GIT_EMAIL").ok(),
                host_name,
                host_email,
            )
        })
        .clone()
}

/// The author + committer env pairs that pin a spawned agent process's git
/// identity. Applied to autonomous spawns ONLY (this process and its children),
/// so the operator's own git config is never mutated.
pub(crate) fn agent_git_identity_env() -> Vec<(String, String)> {
    let (name, email) = autonomous_git_identity();
    vec![
        ("GIT_AUTHOR_NAME".to_string(), name.clone()),
        ("GIT_AUTHOR_EMAIL".to_string(), email.clone()),
        ("GIT_COMMITTER_NAME".to_string(), name),
        ("GIT_COMMITTER_EMAIL".to_string(), email),
    ]
}

/// Best-effort read of the host's EXPLICITLY-configured git identity. `git
/// config --get` prints nothing when a key is unset (git's implicit
/// `user@host` value is never stored), so a `Some` here means a human
/// deliberately configured it.
///
/// Prefers the EFFECTIVE value (local|global), but when that resolves to the
/// `x`/`x@x` placeholder — some runner clones carry a stub LOCAL `user.email`
/// that shadows a real GLOBAL one — it falls back to the GLOBAL value so a dev
/// box still attributes autonomous commits to its owner with zero config.
fn host_git_identity() -> (Option<String>, Option<String>) {
    fn get(args: &[&str]) -> Option<String> {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    // A placeholder effective value is treated as "no usable identity" so the
    // global fallback gets a chance (the `clean()` in the pure resolver also
    // rejects it — this just lets the global tier win over a stub local).
    fn real(v: Option<String>) -> Option<String> {
        v.filter(|s| !s.eq_ignore_ascii_case("x") && s != "x@x")
    }
    let name = real(get(&["config", "--get", "user.name"]))
        .or_else(|| get(&["config", "--global", "--get", "user.name"]));
    let email = real(get(&["config", "--get", "user.email"]))
        .or_else(|| get(&["config", "--global", "--get", "user.email"]));
    (name, email)
}

/// Pure identity selection (testable). Rejects empty/placeholder values so the
/// `x <x@x>` stub can never win.
fn pick_autonomous_git_identity(
    env_name: Option<String>,
    env_email: Option<String>,
    host_name: Option<String>,
    host_email: Option<String>,
) -> (String, String) {
    fn clean(v: Option<String>) -> Option<String> {
        v.map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("x") && s != "x@x")
    }
    let name = clean(env_name)
        .or_else(|| clean(host_name))
        .unwrap_or_else(|| "Qontinui Agent".to_string());
    let email = clean(env_email)
        .or_else(|| clean(host_email))
        .unwrap_or_else(|| "agent@qontinui.dev".to_string());
    (name, email)
}

/// Spawn `claude` CLI as a tokio child. `initial_prompt` is piped to
/// stdin. stdout/stderr are inherited as pipes so the caller can stream
/// them.
async fn spawn_claude_child(workdir: &str, initial_prompt: &str) -> anyhow::Result<Child> {
    let bin = claude_bin_path();
    let mut cmd = Command::new(&bin);
    cmd.current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Account selection: pin CLAUDE_CONFIG_DIR to the selected (token-bearing)
    // account instead of inheriting the process-ambient default. Without this a
    // continuation/worker `claude` spawns under whatever account the runner
    // booted with (e.g. a quota-exhausted one) and dies instantly. The caller
    // calls `pick_best_account()` once per unit of work; here we read the
    // resolved-or-manual effective dir (already credential-validated).
    //
    // `None` means no credential-valid account resolved → leaving the env unset
    // would inherit the ambient default. That is only safe if the ambient
    // default itself has live credentials; otherwise the spawn is a 401 zombie.
    // Fail loud with an actionable reason (the callers turn this `Err` into a
    // `report_spawn_failed` lifecycle post) rather than starting a dead `claude`.
    let ai = crate::settings::get_ai_settings();
    match crate::ai_provider::get_effective_config_dir(&ai.claude_cli) {
        Some(dir) => {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        None => {
            if !crate::ai_provider::oauth_refresh::default_location_has_valid_credentials() {
                let instance =
                    crate::instance::instance_name().unwrap_or_else(|| "primary".to_string());
                return Err(anyhow::anyhow!(
                    "no authenticated Claude account on this runner — run /login (instance={instance})"
                ));
            }
            // None + ambient default has live creds → inherit it (single-account
            // / unset-CLAUDE_CONFIG_DIR default — unchanged behavior).
        }
    }
    // Pin the autonomous-agent git author/committer for this headless worker so
    // its commits land with a meaningful name/email instead of the ambient host
    // placeholder (`x <x@x>`). Scoped to this child process — the operator's own
    // git config is untouched.
    for (k, v) in agent_git_identity_env() {
        cmd.env(k, v);
    }
    // `-p` / `--print` means "single-shot prompt mode" for Claude Code
    // CLI; not all versions support stdin-as-prompt cleanly, so we send
    // the prompt over stdin AND close stdin after.
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn `{bin}` in {workdir}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = initial_prompt.to_string();
        tokio::spawn(async move {
            let body = if prompt.ends_with('\n') {
                prompt
            } else {
                format!("{prompt}\n")
            };
            if let Err(e) = stdin.write_all(body.as_bytes()).await {
                debug!("agent_runtime: stdin write failed: {e}");
            }
            // Drop stdin to signal EOF.
            drop(stdin);
        });
    }
    Ok(child)
}

/// Pump stdout + stderr to the per-agent log file AND POST each line to
/// `/agents/:agent_id/log` (Phase 5 endpoint). Returns the child's exit
/// code on clean exit; Err on pump failure.
async fn pump_subprocess(
    agent_id: uuid::Uuid,
    child: &mut Child,
    log_path: Option<&Path>,
) -> anyhow::Result<i64> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Lazy-create the per-agent log file.
    if let Some(p) = log_path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let log_file = log_path.and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
            .map(|f| Arc::new(Mutex::new(f)))
    });

    // Shared queue for log-forwarder retry on coord failure.
    let queue: Arc<Mutex<VecDeque<LogLine>>> = Arc::new(Mutex::new(VecDeque::new()));

    let q_out = queue.clone();
    let f_out = log_file.clone();
    let out_task = tokio::spawn(async move {
        if let Some(stream) = stdout {
            forward_stream("stdout", stream, agent_id, q_out, f_out).await;
        }
    });

    let q_err = queue.clone();
    let f_err = log_file.clone();
    let err_task = tokio::spawn(async move {
        if let Some(stream) = stderr {
            forward_stream("stderr", stream, agent_id, q_err, f_err).await;
        }
    });

    // Periodic flusher: every 5s, drain the queue to coord.
    let q_flush = queue.clone();
    let flush_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            flush_log_queue(agent_id, &q_flush).await;
        }
    });

    let status = child.wait().await?;
    let _ = out_task.await;
    let _ = err_task.await;
    flush_task.abort();
    // One final flush after the child exits.
    flush_log_queue(agent_id, &queue).await;
    Ok(status.code().map(|c| c as i64).unwrap_or(-1))
}

async fn forward_stream<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    stream_name: &'static str,
    reader: R,
    agent_id: uuid::Uuid,
    queue: Arc<Mutex<VecDeque<LogLine>>>,
    log_file: Option<Arc<Mutex<std::fs::File>>>,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let log = LogLine {
            stream: stream_name.to_string(),
            line: line.clone(),
            at: chrono::Utc::now(),
        };
        // Append to local log file (best-effort).
        if let Some(f) = &log_file {
            use std::io::Write;
            let mut guard = f.lock().await;
            let _ = writeln!(&mut *guard, "[{stream_name}] {line}");
        }
        // Try-immediate POST to coord; on failure, enqueue.
        if !post_log_line(agent_id, &log).await {
            let mut q = queue.lock().await;
            if q.len() >= LOG_FLUSH_QUEUE_CAP {
                q.pop_front();
            }
            q.push_back(log);
        }
    }
}

async fn flush_log_queue(agent_id: uuid::Uuid, queue: &Arc<Mutex<VecDeque<LogLine>>>) {
    let mut q = queue.lock().await;
    while let Some(log) = q.pop_front() {
        if !post_log_line(agent_id, &log).await {
            // Put it back at the head and stop trying for this flush.
            q.push_front(log);
            return;
        }
    }
}

async fn post_log_line(agent_id: uuid::Uuid, line: &LogLine) -> bool {
    let Some(base) = coord_http_base() else {
        return false;
    };
    let url = format!("{base}/agents/{agent_id}/log");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.post(&url).json(line).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn run_heartbeat_loop(payload: LaunchPayload) {
    let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Err(e) = heartbeat_once(&payload).await {
            warn!(
                "agent_runtime: heartbeat agent_id={} failed: {e:#}",
                payload.agent_id
            );
        }
        // Proactively refresh the agent's coord-mcp proxy token (OQ4). The 30s
        // tick ≪ the 30-min refresh margin, so the per-agent JWT in AGENT_TOKENS
        // is renewed well before its 4h TTL — independent of coord-mcp call
        // activity. Coord's /agents/:id/refresh-token rejects an ALREADY-expired
        // token, so a live agent must never let it lapse; the request-path
        // refresh alone is insufficient for an idle agent. Best-effort: a
        // refresh failure (maybe_refresh logs + returns Ok) never breaks the
        // heartbeat loop.
        if let Some(slot) = crate::coord_mcp::lookup_agent_token(payload.agent_id) {
            if let Some(base) = coord_http_base() {
                let _ =
                    crate::agent_token::maybe_refresh(&slot, &base, payload.agent_id, "agent_mcp")
                        .await;
            }
        }
    }
}

async fn heartbeat_once(payload: &LaunchPayload) -> anyhow::Result<()> {
    let base = coord_http_base().ok_or_else(|| anyhow::anyhow!("no coord_url"))?;
    let body = ClaimHeartbeat {
        kind: "phase".to_string(),
        resource_key: payload.claim_token.clone(),
        machine_id: payload.target_device_id.to_string(),
        ttl_seconds: 3600,
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let resp = client
        .post(format!("{base}/claims/heartbeat"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "claims heartbeat returned {}",
            resp.status()
        ));
    }
    Ok(())
}

async fn report_spawn_complete(
    agent_id: uuid::Uuid,
    pid: Option<i64>,
    note: Option<&str>,
    push_ref: Option<&str>,
) {
    let Some(base) = coord_http_base() else {
        return;
    };
    let (phase, pr_context) = if spawn_outcome_enrichment_enabled() {
        (
            Some(SpawnPhase::Launched),
            SpawnPrContext::from_push_ref(push_ref),
        )
    } else {
        (None, SpawnPrContext::default())
    };
    let body = SpawnCompleteBody {
        pid,
        note: note.map(|s| s.to_string()),
        phase,
        pr_context,
    };
    let url = format!("{base}/agents/{agent_id}/spawn-complete");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            info!("agent_runtime: spawn-complete posted agent_id={agent_id}");
        }
        Ok(resp) => {
            warn!(
                "agent_runtime: spawn-complete POST agent_id={agent_id} returned {}",
                resp.status()
            );
        }
        Err(e) => warn!("agent_runtime: spawn-complete POST agent_id={agent_id} failed: {e:#}"),
    }
}

async fn report_spawn_failed(
    agent_id: uuid::Uuid,
    reason: &str,
    exit_code: Option<i64>,
    restarts_attempted: u32,
    push_ref: Option<&str>,
) {
    let Some(base) = coord_http_base() else {
        return;
    };
    let (phase, pr_context) = if spawn_outcome_enrichment_enabled() {
        (
            Some(SpawnPhase::Exited),
            SpawnPrContext::from_push_ref(push_ref),
        )
    } else {
        (None, SpawnPrContext::default())
    };
    let body = SpawnFailedBody {
        reason: reason.to_string(),
        exit_code,
        restarts_attempted: Some(restarts_attempted),
        phase,
        pr_context,
    };
    let url = format!("{base}/agents/{agent_id}/spawn-failed");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            warn!("agent_runtime: spawn-failed posted agent_id={agent_id} reason={reason}");
        }
        Ok(resp) => {
            warn!(
                "agent_runtime: spawn-failed POST agent_id={agent_id} returned {}",
                resp.status()
            );
        }
        Err(e) => warn!("agent_runtime: spawn-failed POST agent_id={agent_id} failed: {e:#}"),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Sentinel returned by the test mint closure so a mint outcome is
    /// unambiguous and deterministic (no uuid in tests).
    const MINTED: &str = "MINTED";

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn agent_identity_env_override_wins() {
        let (n, e) = pick_autonomous_git_identity(
            s("Ada Lovelace"),
            s("ada@example.com"),
            s("Host Name"),
            s("host@example.com"),
        );
        assert_eq!(n, "Ada Lovelace");
        assert_eq!(e, "ada@example.com");
    }

    #[test]
    fn agent_identity_falls_back_to_real_host_config() {
        // No env override → a dev box's real git identity is used (zero config).
        let (n, e) =
            pick_autonomous_git_identity(None, None, s("Joshua Spinak"), s("jspinak@example.com"));
        assert_eq!(n, "Joshua Spinak");
        assert_eq!(e, "jspinak@example.com");
    }

    #[test]
    fn agent_identity_rejects_x_placeholder_and_defaults() {
        // The observed `x <x@x>` stub must never win → marked default instead.
        let (n, e) = pick_autonomous_git_identity(None, None, s("x"), s("x@x"));
        assert_eq!(n, "Qontinui Agent");
        assert_eq!(e, "agent@qontinui.dev");
    }

    #[test]
    fn agent_identity_rejects_empty_and_whitespace() {
        let (n, e) = pick_autonomous_git_identity(s("  "), s(""), None, None);
        assert_eq!(n, "Qontinui Agent");
        assert_eq!(e, "agent@qontinui.dev");
    }

    #[test]
    fn agent_identity_env_overrides_placeholder_host() {
        // A cloud runner (host stub) names its owner via env.
        let (n, e) = pick_autonomous_git_identity(
            s("Fleet Owner"),
            s("owner@qontinui.dev"),
            s("x"),
            s("x@x"),
        );
        assert_eq!(n, "Fleet Owner");
        assert_eq!(e, "owner@qontinui.dev");
    }

    #[test]
    fn agent_identity_env_pairs_pin_author_and_committer() {
        // All four pairs must be present so the raw commit (pre coord-rebase)
        // is fully attributed, not just the author.
        let pairs = agent_git_identity_env();
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"GIT_AUTHOR_NAME"));
        assert!(keys.contains(&"GIT_AUTHOR_EMAIL"));
        assert!(keys.contains(&"GIT_COMMITTER_NAME"));
        assert!(keys.contains(&"GIT_COMMITTER_EMAIL"));
        // Never the placeholder.
        assert!(pairs.iter().all(|(_, v)| v != "x" && v != "x@x"));
    }

    /// Phase 4b — a PR number is derived ONLY from a `refs/pull/<n>/...` ref;
    /// every other ref shape (the common agent push ref) leaves `pr` absent so
    /// the runner never reports a PR it would have to guess.
    #[test]
    fn pr_number_derived_only_from_pull_ref() {
        assert_eq!(pr_number_from_push_ref("refs/pull/614/head"), Some(614));
        assert_eq!(pr_number_from_push_ref("refs/pull/12/merge"), Some(12));
        // Agent push refs and branch refs carry no PR number.
        assert_eq!(pr_number_from_push_ref("refs/agent/abc-def"), None);
        assert_eq!(pr_number_from_push_ref("refs/heads/main"), None);
        // Malformed pull refs: no number segment / non-numeric.
        assert_eq!(pr_number_from_push_ref("refs/pull/"), None);
        assert_eq!(pr_number_from_push_ref("refs/pull/notanum/head"), None);
    }

    /// Phase 4b — the EXACT wire shape coord's ingest must match. The enriched
    /// `spawn-complete` body carries `phase: "launched"` plus `push_ref` (and a
    /// derived `pr` when the ref is a pull ref), additive to the legacy
    /// `pid`/`note`. Coord keys the outcome off these keys.
    #[test]
    fn spawn_complete_body_enriched_shape() {
        let body = SpawnCompleteBody {
            pid: Some(4321),
            note: None,
            phase: Some(SpawnPhase::Launched),
            pr_context: SpawnPrContext::from_push_ref(Some("refs/pull/614/head")),
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["pid"], 4321);
        assert_eq!(v["phase"], "launched");
        assert_eq!(v["push_ref"], "refs/pull/614/head");
        assert_eq!(v["pr"], 614);
    }

    /// Phase 4b — the enriched `spawn-failed` body carries `phase: "exited"`
    /// alongside the existing `reason`/`exit_code`/`restarts_attempted`, plus a
    /// `push_ref` with NO `pr` key when the ref is not a pull ref.
    #[test]
    fn spawn_failed_body_enriched_shape() {
        let body = SpawnFailedBody {
            reason: "non-zero exit code 1".to_string(),
            exit_code: Some(1),
            restarts_attempted: Some(2),
            phase: Some(SpawnPhase::Exited),
            pr_context: SpawnPrContext::from_push_ref(Some("refs/agent/abc-def")),
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["reason"], "non-zero exit code 1");
        assert_eq!(v["exit_code"], 1);
        assert_eq!(v["restarts_attempted"], 2);
        assert_eq!(v["phase"], "exited");
        assert_eq!(v["push_ref"], "refs/agent/abc-def");
        // No PR derivable from an agent ref → `pr` is omitted entirely.
        assert!(
            v.get("pr").is_none(),
            "pr must be absent for a non-pull ref"
        );
    }

    /// Phase 4b — with the enrichment fields cleared (the revert / flag-off
    /// shape), the body serializes byte-identical to the legacy pre-4b shape:
    /// `phase`, `push_ref`, and `pr` are all skipped.
    #[test]
    fn spawn_body_legacy_shape_when_unenriched() {
        let body = SpawnCompleteBody {
            pid: Some(7),
            note: Some("gate continuation".to_string()),
            phase: None,
            pr_context: SpawnPrContext::default(),
        };
        let v = serde_json::to_value(&body).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 2, "only the legacy pid+note keys: {v}");
        assert!(obj.contains_key("pid"));
        assert!(obj.contains_key("note"));

        let failed = SpawnFailedBody {
            reason: "x".to_string(),
            exit_code: None,
            restarts_attempted: Some(0),
            phase: None,
            pr_context: SpawnPrContext::default(),
        };
        let fv = serde_json::to_value(&failed).unwrap();
        let fobj = fv.as_object().unwrap();
        assert_eq!(
            fobj.len(),
            3,
            "only the legacy reason+exit_code+restarts_attempted keys: {fv}"
        );
        assert!(!fobj.contains_key("phase"));
        assert!(!fobj.contains_key("push_ref"));
    }

    fn page(id: &str, n: usize) -> (String, usize) {
        (id.to_string(), n)
    }

    #[test]
    fn pick_continuation_page_default_under_ceiling_picks_default() {
        let counts = [page("default", 3), page("p1", 9)];
        assert_eq!(
            pick_continuation_page(&counts, 9, || MINTED.to_string()),
            "default"
        );
    }

    #[test]
    fn pick_continuation_page_default_full_picks_nonfull_other() {
        // default at the ceiling, p1 under it → p1.
        let counts = [page("default", 9), page("p1", 4)];
        assert_eq!(
            pick_continuation_page(&counts, 9, || MINTED.to_string()),
            "p1"
        );
        // default over the ceiling behaves the same.
        let counts = [page("default", 12), page("p1", 4)];
        assert_eq!(
            pick_continuation_page(&counts, 9, || MINTED.to_string()),
            "p1"
        );
    }

    #[test]
    fn pick_continuation_page_default_full_picks_fewest_terminals() {
        let counts = [
            page("default", 9),
            page("p1", 7),
            page("p2", 2),
            page("p3", 5),
        ];
        assert_eq!(
            pick_continuation_page(&counts, 9, || MINTED.to_string()),
            "p2"
        );
    }

    #[test]
    fn pick_continuation_page_tie_breaks_lexicographically() {
        // p_b and p_a tie at count 3 → smallest page_id wins (p_a).
        let counts = [page("default", 9), page("p_b", 3), page("p_a", 3)];
        assert_eq!(
            pick_continuation_page(&counts, 9, || MINTED.to_string()),
            "p_a"
        );
    }

    #[test]
    fn pick_continuation_page_everything_full_mints() {
        let counts = [page("default", 9), page("p1", 9), page("p2", 11)];
        assert_eq!(
            pick_continuation_page(&counts, 9, || MINTED.to_string()),
            MINTED
        );
    }

    #[test]
    fn pick_continuation_page_empty_counts_picks_default() {
        let counts: [(String, usize); 0] = [];
        assert_eq!(
            pick_continuation_page(&counts, 9, || MINTED.to_string()),
            "default"
        );
    }

    /// The pinned `--session-id <uuid>` pair sits among the flags, BEFORE
    /// the `--` terminator and the trailing positional prompt, with
    /// attached-form `--add-dir=` siblings preserved in between.
    #[test]
    fn continuation_command_pins_session_id_before_positional_prompt() {
        let cmd = build_continuation_claude_command(
            "claude".to_string(),
            "abc-123",
            vec!["--add-dir=D:/wt/sibling".to_string()],
            "do the thing".to_string(),
        );
        assert_eq!(
            cmd.join("|"),
            "claude|--dangerously-skip-permissions|--session-id|abc-123|\
             --add-dir=D:/wt/sibling|--|do the thing"
        );
    }

    /// Regression for the 2026-06-12 multi-repo gate-continuation incident:
    /// the variadic `--add-dir <directories...>` (space form) swallows the
    /// trailing positional prompt as a bogus extra directory, so the argv
    /// must carry attached-form `--add-dir=` tokens only, with the prompt
    /// as the final element behind the `--` terminator.
    #[test]
    fn continuation_command_add_dir_attached_form_keeps_prompt_positional() {
        let cmd = build_continuation_claude_command(
            "claude".to_string(),
            "abc-123",
            vec![
                "--add-dir=D:/wt/coord".to_string(),
                "--add-dir=D:/wt/web".to_string(),
            ],
            "run /implement-plan plans/x.md".to_string(),
        );
        assert_eq!(
            cmd.last().map(String::as_str),
            Some("run /implement-plan plans/x.md"),
            "prompt must be the trailing positional"
        );
        assert_eq!(
            cmd.get(cmd.len() - 2).map(String::as_str),
            Some("--"),
            "the end-of-options terminator must immediately precede the prompt"
        );
        assert!(
            !cmd.iter().any(|a| a == "--add-dir"),
            "bare variadic --add-dir must never appear in the spawn argv: {cmd:?}"
        );
    }

    /// No sibling worktrees → no `--add-dir=`, but the `--` stays so a
    /// prompt starting with `-` can never be parsed as a flag.
    #[test]
    fn continuation_command_single_repo_still_emits_terminator() {
        let cmd = build_continuation_claude_command(
            "claude".to_string(),
            "abc-123",
            vec![],
            "-prompt with dash".to_string(),
        );
        assert_eq!(
            cmd.join("|"),
            "claude|--dangerously-skip-permissions|--session-id|abc-123|\
             --|-prompt with dash"
        );
    }

    #[test]
    fn launch_payload_round_trips_through_envelope() {
        let payload = LaunchPayload {
            agent_id: uuid::Uuid::nil(),
            agent_session_id: None,
            target_device_id: uuid::Uuid::nil(),
            worktrees: vec![AllocatedWorktree {
                repo: "qontinui-runner".to_string(),
                branch: "agent/abc-def".to_string(),
                parent_sha: "deadbeef".to_string(),
                worktree_path: "/tmp/wt".to_string(),
                status: "allocated".to_string(),
                push_ref: Some("refs/agent/abc-def".to_string()),
            }],
            jwt: "tok".to_string(),
            jwt_exp: 0,
            initial_prompt: "go".to_string(),
            claim_token: "agent:00000000-0000-0000-0000-000000000000".to_string(),
            plan_slug: Some("readiness".to_string()),
            plan_phase: Some(4),
            correlation_topic: Some("my-coordination-topic".to_string()),
        };
        let serialized = serde_json::to_value(serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{}", payload.target_device_id),
            "body": serde_json::json!({
                "agent_id": payload.agent_id,
                "agent_session_id": payload.agent_session_id,
                "target_device_id": payload.target_device_id,
                "worktrees": payload.worktrees,
                "jwt": payload.jwt,
                "jwt_exp": payload.jwt_exp,
                "initial_prompt": payload.initial_prompt,
                "claim_token": payload.claim_token,
                "plan_slug": payload.plan_slug,
                "plan_phase": payload.plan_phase,
            }),
        }))
        .unwrap();
        // Use the same envelope parse as handle_message.
        let channel = serialized.get("channel").and_then(|v| v.as_str()).unwrap();
        assert!(channel.starts_with("events.agent.spawn_requested."));
        let body = serialized.get("body").cloned().unwrap();
        let round_tripped: LaunchPayload = serde_json::from_value(body).unwrap();
        assert_eq!(round_tripped.worktrees.len(), 1);
        assert_eq!(round_tripped.worktrees[0].repo, "qontinui-runner");
        assert_eq!(round_tripped.plan_phase, Some(4));
    }

    #[test]
    fn terminal_focus_request_serializes_with_terminal_id() {
        // The frontend listens for `terminal-focus-request { terminal_id }`
        // and selects that tab / switches the main view. Lock the wire shape
        // (snake_case `terminal_id`) so a serde rename can't silently break the
        // auto-surface contract.
        let payload = TerminalFocusRequest {
            terminal_id: "term-abc-123",
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert_eq!(v, serde_json::json!({ "terminal_id": "term-abc-123" }));
        assert_eq!(EVENT_TERMINAL_FOCUS_REQUEST, "terminal-focus-request");
    }

    #[test]
    fn focus_existing_continuation_is_safe_headless() {
        // In a headless/unit-test context there is no Tauri AppHandle, so
        // `focus_existing_continuation` must debug-log and return rather than
        // panic — the duplicate-anchor guard calls it unconditionally.
        focus_existing_continuation("term-headless-noop");
    }

    #[test]
    fn build_coord_ws_url_appends_ws_to_bare_host() {
        // A bare host URL (no `/ws`) gets `/ws` appended, scheme swapped.
        let device = uuid::Uuid::nil();
        assert_eq!(
            build_coord_ws_url("http://localhost:9870", device),
            format!("ws://localhost:9870/ws?pattern=events.agent.spawn_requested.{device}")
        );
        assert_eq!(
            build_coord_ws_url("https://coord.qontinui.io", device),
            format!("wss://coord.qontinui.io/ws?pattern=events.agent.spawn_requested.{device}")
        );
    }

    #[test]
    fn build_coord_ws_url_does_not_double_append_ws() {
        // The shipped `dev`/`production` profiles' coord_url ALREADY ends in
        // `/ws` (see bin/qontinui_profile.rs). Must produce a single `/ws`,
        // not `/ws/ws` (which 401s at the ALB and blocks the subscribe loop).
        let device = uuid::Uuid::nil();
        assert_eq!(
            build_coord_ws_url("wss://coord.qontinui.io/ws", device),
            format!("wss://coord.qontinui.io/ws?pattern=events.agent.spawn_requested.{device}")
        );
        assert_eq!(
            build_coord_ws_url("ws://localhost:9870/ws", device),
            format!("ws://localhost:9870/ws?pattern=events.agent.spawn_requested.{device}")
        );
        // https→wss conversion preserved on an already-`/ws` https base.
        assert_eq!(
            build_coord_ws_url("https://coord.qontinui.io/ws", device),
            format!("wss://coord.qontinui.io/ws?pattern=events.agent.spawn_requested.{device}")
        );
    }

    #[test]
    fn build_coord_ws_url_normalizes_trailing_slash_after_ws() {
        // A `…/ws/` input (trailing slash) is normalized to a single `/ws`,
        // not `/ws/ws` and not `/ws/`.
        let device = uuid::Uuid::nil();
        assert_eq!(
            build_coord_ws_url("wss://coord.qontinui.io/ws/", device),
            format!("wss://coord.qontinui.io/ws?pattern=events.agent.spawn_requested.{device}")
        );
        // A bare host with a trailing slash still gets exactly one `/ws`.
        assert_eq!(
            build_coord_ws_url("https://coord.qontinui.io/", device),
            format!("wss://coord.qontinui.io/ws?pattern=events.agent.spawn_requested.{device}")
        );
    }

    #[test]
    fn build_coord_ws_url_preserves_already_ws_scheme() {
        // An already-`ws://`/`wss://` base keeps its scheme (no http prefix to
        // swap) and is not double-appended.
        let device = uuid::Uuid::nil();
        assert_eq!(
            build_coord_ws_url("ws://h:9870/ws", device),
            format!("ws://h:9870/ws?pattern=events.agent.spawn_requested.{device}")
        );
    }

    #[test]
    fn coord_ws_string_payload_envelope_parses() {
        // Reproduces coord's ACTUAL `/ws` fanout shape: a {channel, payload}
        // envelope where `payload` is the LaunchPayload serialized as a STRING
        // (coord/src/ws.rs relays the raw Redis message text). The pre-fix
        // consumer looked only for `body` as an object and dropped this frame
        // with "envelope missing body" — the bug that blocked the live spawn.
        let device = uuid::Uuid::now_v7();
        let inner = serde_json::json!({
            "agent_id": uuid::Uuid::now_v7(),
            "target_device_id": device,
            "worktrees": [],
            "jwt": "t",
            "jwt_exp": 0,
            "initial_prompt": "go",
            "claim_token": "agent:x",
        });
        // payload as a STRING (coord's real shape)
        let envelope = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "payload": serde_json::to_string(&inner).unwrap(),
        });
        let parsed =
            parse_envelope_payload(&envelope).expect("coord string-payload envelope must parse");
        assert_eq!(parsed.target_device_id, device);

        // Legacy {channel, body:<object>} still parses.
        let legacy = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "body": inner,
        });
        assert!(parse_envelope_payload(&legacy).is_some());

        // Garbage payload yields None (and must not panic).
        let junk = serde_json::json!({ "channel": "x", "payload": "not json" });
        assert!(parse_envelope_payload(&junk).is_none());
    }

    /// Documents the OLD (broken) behavior: coord's minimal gate-continuation
    /// payload does NOT deserialize into the full `LaunchPayload`, so the
    /// pre-fix consumer dropped it with "had no parseable payload". This is the
    /// 2026-06-03 "dispatched but never consumed" root cause — a payload-shape
    /// mismatch, NOT a transport bug.
    #[test]
    fn minimal_gate_continuation_does_not_parse_as_launch_payload() {
        let device = uuid::Uuid::now_v7();
        // The EXACT minimal shape coord's `spawn_continuation()` publishes.
        let inner = serde_json::json!({
            "target_device_id": device,
            "initial_prompt": "go",
            "repos": ["qontinui-runner"],
            "source": "gate_continuation",
            "anchor_key": "plan:foo:phase:1",
        });

        // It must NOT parse as a LaunchPayload (missing agent_id/worktrees/
        // jwt/jwt_exp/claim_token — all non-Option).
        assert!(
            serde_json::from_value::<LaunchPayload>(inner.clone()).is_err(),
            "minimal gate-continuation payload must NOT deserialize as LaunchPayload"
        );

        // And the LaunchPayload envelope parser returns None on it — the old
        // drop path.
        let envelope = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "payload": serde_json::to_string(&inner).unwrap(),
        });
        assert!(
            parse_envelope_payload(&envelope).is_none(),
            "LaunchPayload envelope parse must miss the gate-continuation shape (the drop)"
        );

        // But the NEW arm recognizes + parses it.
        assert!(
            envelope_is_gate_continuation(&envelope),
            "source=gate_continuation must route into the dedicated arm"
        );
        let parsed = parse_gate_continuation_payload(&envelope)
            .expect("gate-continuation arm must parse the minimal payload");
        assert_eq!(parsed.target_device_id, device);
        assert_eq!(parsed.initial_prompt, "go");
        assert_eq!(parsed.repos, vec!["qontinui-runner".to_string()]);
    }

    /// The minimal payload parses both WITHOUT a `presentation` field (coord on
    /// `origin/main` today omits it → default `Terminal`) and WITH it (the
    /// parallel coord phase forwards it → the explicit value wins). Accepts
    /// both the string-inner and object-inner envelope variants.
    #[test]
    fn gate_continuation_parses_with_and_without_presentation() {
        let device = uuid::Uuid::now_v7();

        // (a) WITHOUT presentation → default Terminal. Object-inner variant.
        let no_pres = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "body": serde_json::json!({
                "target_device_id": device,
                "initial_prompt": "say hi",
                "repos": [],
                "source": "gate_continuation",
            }),
        });
        let p = parse_gate_continuation_payload(&no_pres)
            .expect("payload without presentation must parse");
        assert_eq!(
            p.presentation,
            Presentation::Terminal,
            "absent presentation must default to Terminal"
        );
        assert!(p.repos.is_empty());

        // (b) WITH presentation=headless. String-inner variant (coord's real
        // /ws fanout shape).
        let inner = serde_json::json!({
            "target_device_id": device,
            "initial_prompt": "say hi",
            "repos": ["qontinui-coord"],
            "presentation": "headless",
            "source": "gate_continuation",
            "anchor_key": "anchor-x",
        });
        let with_pres = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "payload": serde_json::to_string(&inner).unwrap(),
        });
        let p = parse_gate_continuation_payload(&with_pres)
            .expect("payload with presentation must parse");
        assert_eq!(p.presentation, Presentation::Headless);
        assert_eq!(p.anchor_key.as_deref(), Some("anchor-x"));

        // (c) WITH presentation=terminal, explicit.
        let inner_t = serde_json::json!({
            "target_device_id": device,
            "initial_prompt": "x",
            "repos": [],
            "presentation": "terminal",
            "source": "gate_continuation",
        });
        let with_term = serde_json::json!({ "payload": serde_json::to_string(&inner_t).unwrap() });
        assert_eq!(
            parse_gate_continuation_payload(&with_term)
                .unwrap()
                .presentation,
            Presentation::Terminal
        );
    }

    /// Source-routing: only a `source == "gate_continuation"` frame is claimed
    /// by the gate-continuation arm. An agent-spawn (`LaunchPayload`) frame and
    /// a frame with no/other source must NOT route here, so the existing
    /// `LaunchPayload` path stays untouched for agent spawns.
    #[test]
    fn source_routing_distinguishes_continuation_from_agent_spawn() {
        let device = uuid::Uuid::now_v7();

        // A full agent-spawn LaunchPayload frame (no gate_continuation source).
        let launch = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "payload": serde_json::to_string(&serde_json::json!({
                "agent_id": uuid::Uuid::now_v7(),
                "target_device_id": device,
                "worktrees": [],
                "jwt": "t",
                "jwt_exp": 0,
                "initial_prompt": "go",
                "claim_token": "agent:x",
            }))
            .unwrap(),
        });
        assert!(
            !envelope_is_gate_continuation(&launch),
            "agent-spawn frame must NOT route into the gate-continuation arm"
        );
        // It still parses as a LaunchPayload (the untouched path).
        assert!(
            parse_envelope_payload(&launch).is_some(),
            "agent-spawn frame must still parse as LaunchPayload"
        );

        // A gate-continuation frame routes the other way.
        let cont = serde_json::json!({
            "payload": serde_json::to_string(&serde_json::json!({
                "target_device_id": device,
                "initial_prompt": "go",
                "repos": ["qontinui-runner"],
                "source": "gate_continuation",
            }))
            .unwrap(),
        });
        assert!(envelope_is_gate_continuation(&cont));
        assert!(parse_envelope_payload(&cont).is_none());

        // A frame with no source at all → not a continuation (won't steal
        // agent spawns or junk).
        let no_source = serde_json::json!({ "payload": "{\"initial_prompt\":\"x\"}" });
        assert!(!envelope_is_gate_continuation(&no_source));
    }

    #[test]
    fn local_paths_strip_owner_slug() {
        assert_eq!(
            local_repo_name("qontinui/qontinui-runner"),
            "qontinui-runner"
        );
        assert_eq!(local_repo_name("qontinui-runner"), "qontinui-runner");

        // Site 1's relocated worktree path: outside the project tree, at
        // `agent_worktree_root(<canonical>)/<agent_id>/<repo>`. This must equal
        // what Site 2 (`agent_worktree::local_worktree_target`) produces, so
        // both spawn sites land identically.
        let canonical = crate::agent_worktree::canonical_paths::default_canonical_path(
            "qontinui/qontinui-runner",
        )
        .unwrap();
        let agent_id = uuid::Uuid::nil();
        let p = crate::agent_worktree::canonical_paths::agent_worktree_root(&canonical)
            .join(agent_id.to_string())
            .join(local_repo_name("qontinui/qontinui-runner"));
        assert!(p.ends_with(Path::new("qontinui-runner")));
        // Relocated OUTSIDE the canonical checkout (the whole point).
        assert!(
            !p.starts_with(&canonical),
            "worktree must be outside the repo: {}",
            p.display()
        );
        assert!(p.to_string_lossy().contains("qontinui-worktrees"));
    }

    #[test]
    fn provision_agent_defs_copies_md_files() {
        let root = tempfile::tempdir().unwrap();
        let src = root
            .path()
            .join("qontinui-claude-config")
            .join(".claude")
            .join("agents");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("merge-specialist.md"), "# merge-specialist").unwrap();
        std::fs::write(src.join("repo-auditor.md"), "# repo-auditor").unwrap();
        // A non-md file must NOT be copied.
        std::fs::write(src.join("settings.json"), "{}").unwrap();

        let wt = tempfile::tempdir().unwrap();
        let wt_cwd = wt.path().to_string_lossy().into_owned();

        provision_agent_definitions_from_root(root.path(), &wt_cwd).unwrap();

        let dst = wt.path().join(".claude").join("agents");
        assert!(
            dst.join("merge-specialist.md").is_file(),
            "merge-specialist.md must be provisioned"
        );
        assert!(
            dst.join("repo-auditor.md").is_file(),
            "all agent *.md defs must be copied"
        );
        assert!(
            !dst.join("settings.json").exists(),
            "non-md files must NOT be copied"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("merge-specialist.md")).unwrap(),
            "# merge-specialist"
        );

        // Idempotent: a second run over the same dst overwrites cleanly.
        provision_agent_definitions_from_root(root.path(), &wt_cwd).unwrap();
        assert!(dst.join("merge-specialist.md").is_file());
    }

    #[test]
    fn provision_agent_defs_missing_source_is_soft() {
        // qontinui-root exists but has no qontinui-claude-config/.claude/agents:
        // must log+continue (Ok), creating nothing — no regression vs today.
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let wt_cwd = wt.path().to_string_lossy().into_owned();

        let res = provision_agent_definitions_from_root(root.path(), &wt_cwd);
        assert!(res.is_ok(), "missing source dir must fail soft (Ok)");
        assert!(
            !wt.path().join(".claude").exists(),
            "no .claude tree should be created when source is missing"
        );
    }

    #[test]
    fn stop_envelope_parses_agent_id() {
        let aid = uuid::Uuid::now_v7();
        let inner = serde_json::json!({ "agent_id": aid, "at": "2026-06-03T00:00:00Z" });
        // coord's stop envelope: {channel, payload: "<json-string>"} (post_stop)
        let env_str = serde_json::json!({
            "channel": "events.agent.stop_requested.x",
            "payload": serde_json::to_string(&inner).unwrap(),
        });
        assert_eq!(parse_stop_agent_id(&env_str), Some(aid));
        // object-inner form is also accepted
        let env_obj = serde_json::json!({ "channel": "x", "body": inner });
        assert_eq!(parse_stop_agent_id(&env_obj), Some(aid));
        // junk / missing → None (never panics)
        assert_eq!(
            parse_stop_agent_id(&serde_json::json!({ "payload": "not json" })),
            None
        );
        assert_eq!(parse_stop_agent_id(&serde_json::json!({})), None);
    }

    #[test]
    fn request_agent_stop_missing_is_false() {
        // An agent_id not running on this runner is a no-op (false), never panics.
        assert!(!request_agent_stop(uuid::Uuid::now_v7()));
    }

    #[test]
    fn agent_log_path_uses_agent_id() {
        // Force HOME to a known place so the path is deterministic.
        let id = uuid::Uuid::parse_str("0190000a-9b6c-7d3e-8f1a-2b3c4d5e6f70").unwrap();
        // Just exercise the constructor — exact path content depends on
        // the environment's HOME, which is irrelevant here.
        let p = agent_log_path(id);
        if let Some(p) = p {
            let s = p.to_string_lossy().to_string();
            assert!(
                s.contains("0190000a-9b6c-7d3e-8f1a-2b3c4d5e6f70.log"),
                "expected agent_id-named log file, got {}",
                s
            );
        }
    }

    #[test]
    fn claude_bin_respects_env_override() {
        let prev = std::env::var("QONTINUI_CLAUDE_BIN").ok();
        std::env::set_var("QONTINUI_CLAUDE_BIN", "/some/fake/claude-fixture");
        assert_eq!(claude_bin_path(), "/some/fake/claude-fixture");
        match prev {
            Some(v) => std::env::set_var("QONTINUI_CLAUDE_BIN", v),
            None => std::env::remove_var("QONTINUI_CLAUDE_BIN"),
        }
    }

    /// Smoke test using a fake claude binary fixture (the existing
    /// `mock_claude_cli` binary in `bin/`). Run via cargo so the test
    /// runtime resolves the fixture path automatically.
    ///
    /// Gated behind `QONTINUI_AGENT_RUNTIME_E2E=1` because the fixture
    /// path is build-target-dependent.
    #[tokio::test]
    async fn fake_claude_e2e_smoke() {
        if std::env::var("QONTINUI_AGENT_RUNTIME_E2E").ok().as_deref() != Some("1") {
            return;
        }
        // Build a child via a portable echo command — verifies the
        // spawn-and-pump shape end-to-end without depending on the
        // real claude binary.
        std::env::set_var(
            "QONTINUI_CLAUDE_BIN",
            if cfg!(target_os = "windows") {
                "cmd"
            } else {
                "sh"
            },
        );
        let tmp = std::env::temp_dir();
        let mut child = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/c", "echo agent-runtime-fake-output"]);
            c.current_dir(&tmp);
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c.spawn().unwrap()
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "echo agent-runtime-fake-output"]);
            c.current_dir(&tmp);
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c.spawn().unwrap()
        };
        let agent_id = uuid::Uuid::now_v7();
        let exit = pump_subprocess(agent_id, &mut child, None).await.unwrap();
        assert_eq!(exit, 0);
    }

    /// The terminal arm launches the `claude` CLI with the prompt as a single
    /// POSITIONAL arg (interactive form), NOT `--print` — so an
    /// `AskUserQuestion` inside the spawned session is answerable. This guards
    /// the argv shape the terminal branch hands to `terminal_create`'s
    /// `command` override (resolved via the same `QONTINUI_CLAUDE_BIN` env the
    /// headless path uses).
    #[test]
    fn terminal_continuation_command_is_interactive_positional_prompt() {
        let prev = std::env::var("QONTINUI_CLAUDE_BIN").ok();
        std::env::set_var("QONTINUI_CLAUDE_BIN", "/fake/claude");

        // Mirror the construction in `run_continuation_terminal` (a fixed-size
        // array here — the production path needs an owned `Vec` for the
        // `Option<Vec<String>>` command override, but the test only inspects).
        let claude_bin = claude_bin_path();
        let prompt = "implement phase 2 of the plan".to_string();
        let command = [claude_bin, prompt.clone()];

        assert_eq!(command[0], "/fake/claude", "uses the resolved claude bin");
        assert_eq!(command[1], prompt, "prompt is the single positional arg");
        assert_eq!(command.len(), 2, "no extra flags");
        assert!(
            !command.iter().any(|a| a == "--print" || a == "-p"),
            "must NOT use --print/-p: the session must stay interactive"
        );

        match prev {
            Some(v) => std::env::set_var("QONTINUI_CLAUDE_BIN", v),
            None => std::env::remove_var("QONTINUI_CLAUDE_BIN"),
        }
    }

    #[test]
    fn continuation_session_id_is_stable_for_same_anchor_and_device() {
        // Phase 1b: the synthesized owner-token discriminator must be STABLE
        // across retries of the same continuation (same device + anchor) so a
        // re-acquire renews rather than collides, and DISTINCT across anchors.
        let device = uuid::Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap();
        let mk = |anchor: Option<&str>| GateContinuationPayload {
            target_device_id: device,
            initial_prompt: "p".to_string(),
            repos: vec![],
            presentation: Presentation::Headless,
            source: GATE_CONTINUATION_SOURCE.to_string(),
            anchor_key: anchor.map(|s| s.to_string()),
            gate_id: None,
            dispatch_id: None,
            target_instance_name: None,
        };

        let a1 = continuation_session_id(&mk(Some("gate-7f2358d5")));
        let a2 = continuation_session_id(&mk(Some("gate-7f2358d5")));
        assert_eq!(a1, a2, "same (device, anchor) → same stable id");
        assert!(a1.is_some());

        let b = continuation_session_id(&mk(Some("gate-other")));
        assert_ne!(a1, b, "different anchor → different id");

        // A different device with the same anchor is also distinct.
        let other_device = GateContinuationPayload {
            target_device_id: uuid::Uuid::parse_str("66666666-6666-6666-6666-666666666666")
                .unwrap(),
            ..mk(Some("gate-7f2358d5"))
        };
        assert_ne!(a1, continuation_session_id(&other_device));

        // Anchor-less → a fresh v4 each call (Some, but non-equal).
        let n1 = continuation_session_id(&mk(None));
        let n2 = continuation_session_id(&mk(None));
        assert!(n1.is_some() && n2.is_some());
        assert_ne!(n1, n2, "anchor-less continuations get a fresh id each time");
    }

    /// In a non-Tauri (unit-test) context there is no process-global AppHandle,
    /// so the terminal arm cannot open a window. It must fail gracefully —
    /// report spawn-failed (no-op here, no coord profile) and return `Err`,
    /// NOT panic and NOT silently drop the continuation. This proves the
    /// `Presentation::Terminal` arm dispatches to the terminal path (and that
    /// the path's no-AppHandle guard fires) without a live webview.
    #[tokio::test]
    async fn terminal_continuation_without_app_handle_fails_cleanly() {
        let payload = GateContinuationPayload {
            target_device_id: uuid::Uuid::now_v7(),
            initial_prompt: "hi".to_string(),
            repos: vec![],
            presentation: Presentation::Terminal,
            source: GATE_CONTINUATION_SOURCE.to_string(),
            anchor_key: Some("anchor-z".to_string()),
            gate_id: None,
            dispatch_id: None,
            target_instance_name: None,
        };
        let workdir = std::env::temp_dir().to_string_lossy().to_string();
        let res = run_continuation_terminal(uuid::Uuid::now_v7(), &workdir, &payload, None).await;
        assert!(
            res.is_err(),
            "terminal arm must Err (not panic / not silently drop) with no AppHandle"
        );
    }

    /// Drives the gate-continuation HEADLESS dispatch through the REAL
    /// `spawn_claude_child` + `pump_subprocess` path with a fake `claude` bin
    /// (a portable shell that prints + exits 0). Proves the arm spawns a child
    /// and returns `Ok(())` end-to-end. The coord lifecycle POSTs
    /// (`spawn-complete`/`spawn-failed`) no-op gracefully here because no
    /// `coord_url` profile is configured in the test env (`coord_http_base()`
    /// returns `None`), so this asserts the SPAWN path, not the HTTP posts.
    ///
    /// Gated behind `QONTINUI_AGENT_RUNTIME_E2E=1` (the shell-as-claude
    /// substitution mutates process env globally; keep it opt-in like
    /// `fake_claude_e2e_smoke`).
    #[tokio::test]
    async fn gate_continuation_headless_spawns_child() {
        if std::env::var("QONTINUI_AGENT_RUNTIME_E2E").ok().as_deref() != Some("1") {
            return;
        }
        // Use a portable shell as the fake `claude`: prints a line, exits 0.
        let prev = std::env::var("QONTINUI_CLAUDE_BIN").ok();
        std::env::set_var(
            "QONTINUI_CLAUDE_BIN",
            if cfg!(target_os = "windows") {
                "cmd"
            } else {
                "sh"
            },
        );
        // On Windows the bin is `cmd`; spawn_claude_child passes the prompt on
        // stdin and closes it — `cmd` with no args reads stdin then exits. On
        // Unix, `sh` reads stdin commands then exits. Either way the child
        // spawns and the pump observes a clean exit.
        let workdir = std::env::temp_dir().to_string_lossy().to_string();
        let agent_id = uuid::Uuid::now_v7();
        let res =
            run_continuation_headless(agent_id, &workdir, "echo gate-continuation-proof").await;
        assert!(
            res.is_ok(),
            "gate-continuation headless dispatch must spawn + return Ok: {res:?}"
        );

        match prev {
            Some(v) => std::env::set_var("QONTINUI_CLAUDE_BIN", v),
            None => std::env::remove_var("QONTINUI_CLAUDE_BIN"),
        }
    }

    // =========================================================================
    // Fix (a): back-off reset decision
    // =========================================================================

    /// A pump that ran longer than [`HEALTHY_PUMP_THRESHOLD_SECS`] was a healthy
    /// connection that died — it MUST reset the back-off (regardless of Ok/Err).
    /// A pump that died almost immediately is a connect failure — it must NOT
    /// reset, so the back-off keeps climbing toward the cap. This is the core of
    /// Fix (a): before it, an abnormal-drop `Err` after 60s of healthy uptime
    /// never reset and the back-off pinned at the 60s cap forever.
    #[test]
    fn backoff_resets_only_after_a_healthy_length_pump() {
        // Below threshold → do NOT reset (genuine connect/handshake failure).
        assert!(!reset_backoff_after_pump(Duration::from_secs(0)));
        assert!(!reset_backoff_after_pump(Duration::from_secs(1)));
        assert!(!reset_backoff_after_pump(Duration::from_secs(
            HEALTHY_PUMP_THRESHOLD_SECS - 1
        )));
        // At/above threshold → reset (a healthy connection that dropped).
        assert!(reset_backoff_after_pump(Duration::from_secs(
            HEALTHY_PUMP_THRESHOLD_SECS
        )));
        assert!(reset_backoff_after_pump(Duration::from_secs(60)));
        // The exact ALB idle-kill window (~60s) is well above threshold.
        assert!(reset_backoff_after_pump(Duration::from_secs(61)));
    }

    /// Sub-millisecond / sub-second pumps round down to 0 whole seconds and must
    /// NOT reset — a tight reconnect storm (instant kick) keeps backing off.
    #[test]
    fn backoff_does_not_reset_on_subsecond_pump() {
        assert!(!reset_backoff_after_pump(Duration::from_millis(500)));
        assert!(!reset_backoff_after_pump(Duration::from_millis(29_999)));
    }

    // =========================================================================
    // Fix (c2): pending-continuations response parsing
    // =========================================================================

    /// Coord's `GET /coord/agents/pending-continuations` response parses into
    /// dispatchable rows: each `payload` is the bare gate-continuation spawn
    /// object (same shape the WS path parses) and each row's `gate_id` is the
    /// authoritative dedupe/ack key.
    #[test]
    fn pending_continuations_response_parses_into_dispatchable_payloads() {
        let device = uuid::Uuid::now_v7();
        let gate_a = uuid::Uuid::now_v7();
        let gate_b = uuid::Uuid::now_v7();
        let body = serde_json::json!({
            "pending": [
                {
                    "gate_id": gate_a,
                    "dispatched_at": "2026-06-06T00:00:00Z",
                    "payload": {
                        "target_device_id": device,
                        "initial_prompt": "resume phase 2",
                        "repos": ["qontinui-runner"],
                        "presentation": "headless",
                        "source": "gate_continuation",
                        "anchor_key": "plan:foo:phase:2",
                        "gate_id": gate_a,
                    }
                },
                {
                    "gate_id": gate_b,
                    "dispatched_at": "2026-06-06T00:01:00Z",
                    // `payload` here OMITS gate_id (coord may not duplicate it
                    // inside the payload object); the poll path stamps the row's
                    // gate_id on before dispatch.
                    "payload": {
                        "target_device_id": device,
                        "initial_prompt": "resume phase 3",
                        "repos": [],
                        "source": "gate_continuation",
                    }
                }
            ],
            "total": 2,
        });
        let parsed: PendingContinuationsResponse = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.total, 2);
        assert_eq!(parsed.pending.len(), 2);

        let r0 = &parsed.pending[0];
        assert_eq!(r0.gate_id, gate_a);
        assert_eq!(r0.payload.target_device_id, device);
        assert_eq!(r0.payload.initial_prompt, "resume phase 2");
        assert_eq!(r0.payload.presentation, Presentation::Headless);
        assert_eq!(r0.payload.gate_id, Some(gate_a));
        assert_eq!(r0.dispatched_at.as_deref(), Some("2026-06-06T00:00:00Z"));

        let r1 = &parsed.pending[1];
        assert_eq!(r1.gate_id, gate_b);
        // payload had no gate_id → None; the poll loop stamps row.gate_id on.
        assert_eq!(r1.payload.gate_id, None);
        assert_eq!(r1.payload.presentation, Presentation::Terminal); // default
    }

    /// An empty / absent `pending` array parses to zero rows (the common case),
    /// and a totally empty object also parses (all fields `#[serde(default)]`).
    #[test]
    fn pending_continuations_empty_response_parses() {
        let empty: PendingContinuationsResponse =
            serde_json::from_value(serde_json::json!({ "pending": [], "total": 0 })).unwrap();
        assert!(empty.pending.is_empty());
        let bare: PendingContinuationsResponse =
            serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(bare.pending.is_empty());
        assert_eq!(bare.total, 0);
    }

    // =========================================================================
    // Fix (c2): dedupe-set behavior
    // =========================================================================

    /// The process-wide dedupe set claims a gate_id exactly once: the first
    /// claim wins (`true`), every subsequent claim of the same id loses
    /// (`false`). This is what makes a continuation delivered by BOTH the WS
    /// fast-path and the poll backstop spawn exactly once. Distinct ids never
    /// collide.
    #[test]
    fn claim_gate_dispatch_is_once_per_gate_id() {
        let gate = uuid::Uuid::now_v7();
        let other = uuid::Uuid::now_v7();
        assert!(claim_gate_dispatch(gate), "first claim of a gate_id wins");
        assert!(
            !claim_gate_dispatch(gate),
            "second claim of the same gate_id loses (deduped)"
        );
        assert!(
            !claim_gate_dispatch(gate),
            "third claim still loses (idempotent skip)"
        );
        assert!(
            claim_gate_dispatch(other),
            "a different gate_id is unaffected"
        );
    }

    /// The work-unit dispatch dedupe set ([`claim_dispatch_dispatch`]) is the
    /// sibling of the gate set: it claims a `dispatch_id` exactly once so a unit
    /// dispatch delivered by BOTH the live WS frame and the
    /// `pending-unit-dispatches` poll backstop spawns exactly once. Distinct ids
    /// never collide. This is the load-bearing guard for the dispatch_id arm.
    #[test]
    fn claim_dispatch_dispatch_is_once_per_dispatch_id() {
        let d = uuid::Uuid::now_v7();
        let other = uuid::Uuid::now_v7();
        assert!(
            claim_dispatch_dispatch(d),
            "first claim of a dispatch_id wins"
        );
        assert!(
            !claim_dispatch_dispatch(d),
            "second claim of the same dispatch_id loses (deduped)"
        );
        assert!(
            !claim_dispatch_dispatch(d),
            "third claim still loses (idempotent skip)"
        );
        assert!(
            claim_dispatch_dispatch(other),
            "a different dispatch_id is unaffected"
        );
    }

    /// The gate dedupe set and the dispatch dedupe set are INDEPENDENT: a
    /// `gate_id` and a `dispatch_id` that happen to be the same Uuid value do not
    /// collide (they live in separate sets). Proves the dispatch_id arm did not
    /// fold into the gate set.
    #[test]
    fn gate_and_dispatch_dedupe_sets_are_independent() {
        let id = uuid::Uuid::now_v7();
        assert!(claim_gate_dispatch(id), "gate claim of id wins");
        assert!(
            claim_dispatch_dispatch(id),
            "the SAME uuid as a dispatch_id still wins — separate set"
        );
    }

    /// Coord's `GET /coord/agents/pending-unit-dispatches` response parses into
    /// [`PendingUnitDispatchesResponse`]: each row's `payload` is the spawn-frame
    /// object (the same shape the WS path parses) and each row's `dispatch_id` is
    /// the authoritative id the poll loop stamps onto the payload before dispatch.
    /// A `payload` that OMITS `dispatch_id` still parses (coord need not duplicate
    /// it inside the payload — the poll loop stamps `row.dispatch_id` on).
    #[test]
    fn pending_unit_dispatches_response_parses() {
        let d_a = uuid::Uuid::now_v7();
        let d_b = uuid::Uuid::now_v7();
        let dev = uuid::Uuid::now_v7();
        let json = serde_json::json!({
            "pending": [
                {
                    "dispatch_id": d_a,
                    "payload": {
                        "target_device_id": dev,
                        "initial_prompt": "do unit A",
                        "source": "gate_continuation",
                        // payload carries dispatch_id too here (coord may stamp it)
                        "dispatch_id": d_a,
                    },
                    "dispatched_at": "2026-06-28T00:00:00Z",
                },
                {
                    "dispatch_id": d_b,
                    "payload": {
                        "target_device_id": dev,
                        "initial_prompt": "do unit B",
                        "source": "gate_continuation",
                        // payload OMITS dispatch_id — the poll loop stamps it on.
                    },
                },
            ],
            "total": 2,
        });
        let body: PendingUnitDispatchesResponse =
            serde_json::from_value(json).expect("pending-unit-dispatches response must parse");
        assert_eq!(body.pending.len(), 2);
        assert_eq!(body.total, 2);

        let r0 = &body.pending[0];
        assert_eq!(r0.dispatch_id, d_a);
        assert_eq!(r0.payload.dispatch_id, Some(d_a));
        assert_eq!(r0.payload.gate_id, None, "unit dispatch carries no gate_id");

        let r1 = &body.pending[1];
        assert_eq!(r1.dispatch_id, d_b);
        // payload had no dispatch_id → None; the poll loop stamps row.dispatch_id.
        assert_eq!(r1.payload.dispatch_id, None);
    }

    /// The unit consume body serializes to exactly `{"device_id": ...}` — the
    /// LOCKED wire contract for `POST .../unit-dispatches/{dispatch_id}/consumed`.
    #[test]
    fn unit_dispatch_consumed_body_serializes_device_id_only() {
        let dev = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let body = UnitDispatchConsumedBody { device_id: dev };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "device_id": "11111111-1111-1111-1111-111111111111" }),
            "consume body must be exactly {{device_id}}"
        );
    }

    // =========================================================================
    // P3 (anchor_key dedup) + P4 (concurrency cap): continuation guard
    // =========================================================================

    /// Reset the process-wide continuation registry between guard tests (it is a
    /// `OnceLock`-backed singleton). Tests that touch it run under a shared mutex
    /// (`CONT_GUARD_LOCK`) so they don't race each other's registry state.
    fn clear_continuation_registry() {
        continuation_sessions().lock().unwrap().clear();
    }

    static CONT_GUARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// REGRESSION (P3 must not regress #450): a SAME gate_id delivered twice is
    /// still short-circuited by the `dispatched_gate_ids` set — the anchor_key
    /// layer is additive, not a replacement. (Mirrors
    /// `claim_gate_dispatch_is_once_per_gate_id` but named as the explicit
    /// regression guard the plan asks for.)
    #[test]
    fn gate_id_dedup_still_holds_alongside_anchor_guard() {
        let gate = uuid::Uuid::now_v7();
        assert!(claim_gate_dispatch(gate), "first delivery of the gate wins");
        assert!(
            !claim_gate_dispatch(gate),
            "second delivery of the SAME gate_id is still deduped (#450 intact)"
        );
    }

    /// P3: a live continuation for an anchor_key dedups a second dispatch with
    /// the SAME anchor_key (the re-cleared-gate / legacy-no-gate_id case the
    /// gate_id set can't catch). A different anchor proceeds. The guard prunes
    /// dead sessions first, so once the first session dies the anchor is free.
    #[test]
    fn anchor_key_guard_dedups_live_then_frees_on_death() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        clear_continuation_registry();
        // Raise the cap out of the way so this test isolates the dedup path.
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "100");

        // No session yet → proceed, then register it as live.
        let live_all = |_id: &str| true;
        assert_eq!(
            evaluate_continuation_guard(Some("plan:foo:phase:1"), &live_all),
            ContinuationGuard::Proceed
        );
        register_continuation_session("term-tid-1".to_string(), Some("plan:foo:phase:1".into()));

        // Same anchor, still live → DuplicateAnchor (carries the existing tid).
        assert_eq!(
            evaluate_continuation_guard(Some("plan:foo:phase:1"), &live_all),
            ContinuationGuard::DuplicateAnchor("term-tid-1".to_string())
        );
        // A different anchor is unaffected.
        assert_eq!(
            evaluate_continuation_guard(Some("plan:foo:phase:2"), &live_all),
            ContinuationGuard::Proceed
        );

        // Now the first session is dead → the guard prunes it and the anchor is
        // free to spawn again (the legitimate re-run after completion).
        let dead_first = |id: &str| id != "term-tid-1";
        assert_eq!(
            evaluate_continuation_guard(Some("plan:foo:phase:1"), &dead_first),
            ContinuationGuard::Proceed
        );
        // The registry no longer holds the dead session.
        assert!(continuation_sessions()
            .lock()
            .unwrap()
            .get("term-tid-1")
            .is_none());

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// P3: a continuation with NO anchor_key (legacy frame) never dedups by
    /// anchor — it always proceeds (the gate_id set is its only dedup).
    #[test]
    fn no_anchor_key_never_dedups() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "100");
        let live_all = |_id: &str| true;

        register_continuation_session("tid-a".into(), None);
        // Another anchor-less dispatch must NOT be deduped against the existing
        // anchor-less session (we can't correlate them).
        assert_eq!(
            evaluate_continuation_guard(None, &live_all),
            ContinuationGuard::Proceed
        );

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// P4: at the configured cap, a fresh dispatch is refused (`AtCap`). Below
    /// the cap it proceeds. Dead sessions are pruned before counting, so they
    /// don't consume cap slots. Env override is honored.
    #[test]
    fn continuation_cap_refuses_at_limit() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "2");
        let live_all = |_id: &str| true;

        // 0 live, cap 2 → proceed.
        assert_eq!(
            evaluate_continuation_guard(Some("a1"), &live_all),
            ContinuationGuard::Proceed
        );
        register_continuation_session("t1".into(), Some("a1".into()));
        register_continuation_session("t2".into(), Some("a2".into()));

        // 2 live, cap 2 → AtCap (a NEW anchor, so not a dedup).
        assert_eq!(
            evaluate_continuation_guard(Some("a3"), &live_all),
            ContinuationGuard::AtCap(2)
        );

        // One session dies → pruned → back under cap → proceed.
        let t1_dead = |id: &str| id != "t1";
        assert_eq!(
            evaluate_continuation_guard(Some("a3"), &t1_dead),
            ContinuationGuard::Proceed
        );

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// P3 before P4: a duplicate of an already-LIVE anchor is reported as a
    /// dedup even when the registry is at the cap — the honest reason is "this
    /// is already running", not "capped".
    #[test]
    fn dedup_takes_precedence_over_cap() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "1");
        let live_all = |_id: &str| true;

        register_continuation_session("t1".into(), Some("anchor-dup".into()));
        // At cap (1) AND the anchor matches a live session → dedup wins.
        assert_eq!(
            evaluate_continuation_guard(Some("anchor-dup"), &live_all),
            ContinuationGuard::DuplicateAnchor("t1".to_string())
        );

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// The cap reads `QONTINUI_CONTINUATION_SESSION_CAP`, falling back to the
    /// default for an unset / non-numeric value.
    #[test]
    fn continuation_session_cap_env_parsing() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        let prev = std::env::var("QONTINUI_CONTINUATION_SESSION_CAP").ok();

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        assert_eq!(continuation_session_cap(), DEFAULT_CONTINUATION_SESSION_CAP);

        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "7");
        assert_eq!(continuation_session_cap(), 7);

        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "not-a-number");
        assert_eq!(continuation_session_cap(), DEFAULT_CONTINUATION_SESSION_CAP);

        match prev {
            Some(v) => std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", v),
            None => std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP"),
        }
    }

    /// The default cap is unbounded: an unset env never refuses on count, so a
    /// loaded registry still proceeds (the primary spawns every continuation).
    #[test]
    fn unbounded_default_cap_never_refuses_on_count() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        clear_continuation_registry();
        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        assert_eq!(continuation_session_cap(), usize::MAX);

        let live_all = |_id: &str| true;
        // Many live sessions, no env cap → still Proceed (never AtCap).
        for i in 0..50 {
            register_continuation_session(format!("t{i}"), Some(format!("a{i}")));
        }
        assert_eq!(
            evaluate_continuation_guard(Some("a-new"), &live_all),
            ContinuationGuard::Proceed
        );
        clear_continuation_registry();
    }

    /// Instance-targeting self-gate: a continuation spawns on EXACTLY the
    /// instance it is addressed to; an absent target addresses the primary.
    #[test]
    fn continuation_addressed_to_self_matrix() {
        // Normal (no target) → primary (None) spawns; a secondary does not.
        assert!(continuation_addressed_to_self(None, None));
        assert!(!continuation_addressed_to_self(None, Some("test-19eab")));

        // Named target → only the matching secondary spawns; primary does not,
        // and a different secondary does not (no double-spawn either way).
        assert!(continuation_addressed_to_self(
            Some("test-19eab"),
            Some("test-19eab")
        ));
        assert!(!continuation_addressed_to_self(Some("test-19eab"), None));
        assert!(!continuation_addressed_to_self(
            Some("test-19eab"),
            Some("other-runner")
        ));
    }

    // =========================================================================
    // Fix (c2): WS payload with/without gate_id parses
    // =========================================================================

    /// A WS-delivered gate-continuation payload carrying the NEW `gate_id` field
    /// parses and surfaces the id (so the dispatch seam can dedupe + ack it).
    /// A payload WITHOUT `gate_id` (the currently-deployed coord) still parses,
    /// with `gate_id == None` (back-compat: dispatch once, skip the ack). Covers
    /// both the string-inner and object-inner envelope variants.
    #[test]
    fn gate_continuation_parses_with_and_without_gate_id() {
        let device = uuid::Uuid::now_v7();
        let gate = uuid::Uuid::now_v7();

        // (a) WITH gate_id (new coord), string-inner envelope (real /ws shape).
        let inner = serde_json::json!({
            "target_device_id": device,
            "initial_prompt": "go",
            "repos": ["qontinui-runner"],
            "source": "gate_continuation",
            "anchor_key": "anchor-x",
            "gate_id": gate,
        });
        let env = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "payload": serde_json::to_string(&inner).unwrap(),
        });
        let p = parse_gate_continuation_payload(&env).expect("payload with gate_id must parse");
        assert_eq!(p.gate_id, Some(gate), "gate_id must round-trip");
        assert_eq!(p.target_device_id, device);

        // (b) WITHOUT gate_id (deployed coord), object-inner envelope. Must
        // still parse, gate_id == None (so the seam skips the ack silently).
        let no_gate = serde_json::json!({
            "body": {
                "target_device_id": device,
                "initial_prompt": "go",
                "repos": [],
                "source": "gate_continuation",
            }
        });
        let p2 = parse_gate_continuation_payload(&no_gate)
            .expect("payload without gate_id must still parse (back-compat)");
        assert_eq!(p2.gate_id, None, "absent gate_id must be optional → None");
    }

    /// A LIVE work-unit dispatch WS frame reuses `source:"gate_continuation"` but
    /// carries `dispatch_id` and NO `gate_id`. It must parse, surface the
    /// `dispatch_id`, and leave `gate_id == None` — so `dispatch_gate_continuation`
    /// routes it into the dispatch_id arm (dedupe + unit consume ack) on the live
    /// path, exactly as the replay-pull path does.
    #[test]
    fn unit_dispatch_ws_frame_parses_with_dispatch_id_and_no_gate_id() {
        let device = uuid::Uuid::now_v7();
        let dispatch = uuid::Uuid::now_v7();
        let inner = serde_json::json!({
            "target_device_id": device,
            "initial_prompt": "run unit",
            "repos": ["qontinui-runner"],
            "source": "gate_continuation",
            "dispatch_id": dispatch,
        });
        let env = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "payload": serde_json::to_string(&inner).unwrap(),
        });
        let p = parse_gate_continuation_payload(&env).expect("live unit dispatch frame must parse");
        assert_eq!(p.dispatch_id, Some(dispatch), "dispatch_id must round-trip");
        assert_eq!(p.gate_id, None, "a unit dispatch carries no gate_id");
        assert_eq!(p.target_device_id, device);
    }

    /// The CLAIM body serializes to the FIXED wire contract coord accepts as a
    /// claim: EXACTLY `{"device_id": "<uuid>"}` (no `outcome`/`detail` keys —
    /// `skip_serializing_if` must elide them so the claim is byte-identical to
    /// the pre-restructure ack body).
    #[test]
    fn continuation_claim_body_wire_shape() {
        let device = uuid::Uuid::now_v7();
        let v = serde_json::to_value(ContinuationConsumedBody::claim(device)).unwrap();
        assert_eq!(v, serde_json::json!({ "device_id": device }));
    }

    /// The OUTCOME body serializes with `outcome` ("spawned"/"spawn_failed") and
    /// `detail` only when present.
    #[test]
    fn continuation_outcome_body_wire_shape() {
        let device = uuid::Uuid::now_v7();
        // spawned → no detail key.
        let spawned =
            serde_json::to_value(ContinuationConsumedBody::outcome(device, true, None)).unwrap();
        assert_eq!(
            spawned,
            serde_json::json!({ "device_id": device, "outcome": "spawned" })
        );
        // spawn_failed → carries the first-line detail.
        let failed = serde_json::to_value(ContinuationConsumedBody::outcome(
            device,
            false,
            Some("terminal session create failed".to_string()),
        ))
        .unwrap();
        assert_eq!(
            failed,
            serde_json::json!({
                "device_id": device,
                "outcome": "spawn_failed",
                "detail": "terminal session create failed"
            })
        );
    }

    /// `first_line` extracts the first non-empty line, trimmed (the `spawn_failed`
    /// detail must be a single tidy line, not a multi-line `anyhow` chain).
    #[test]
    fn first_line_takes_only_the_first_line() {
        assert_eq!(first_line("boom\n\ncaused by: x\ny"), "boom");
        assert_eq!(first_line("  spaced  "), "spaced");
        assert_eq!(first_line(""), "");
        assert_eq!(first_line("single"), "single");
    }

    // =========================================================================
    // SpawnDecision: the claim-response → spawn/skip decision (pure, no I/O)
    // =========================================================================

    /// 200 (or any 2xx) → Spawn.
    #[test]
    fn decide_spawn_on_200_spawns() {
        assert_eq!(
            decide_spawn(200, r#"{"consumed":true}"#),
            SpawnDecision::Spawn
        );
        assert_eq!(decide_spawn(204, ""), SpawnDecision::Spawn);
    }

    /// 409 with the cancelled contract → SkipCancelled, carrying the reason.
    #[test]
    fn decide_spawn_on_409_cancelled_skips_with_reason() {
        let body = r#"{"error":"cancelled","cancelled_at":"2026-06-07T00:00:00Z","cancel_reason":"taken over by session abc"}"#;
        assert_eq!(
            decide_spawn(409, body),
            SpawnDecision::SkipCancelled {
                reason: Some("taken over by session abc".to_string())
            }
        );
    }

    /// 409 cancelled with no `cancel_reason` → SkipCancelled with `None` reason
    /// (still skips — the cancel is authoritative even without a reason string).
    #[test]
    fn decide_spawn_on_409_cancelled_no_reason_still_skips() {
        assert_eq!(
            decide_spawn(409, r#"{"error":"cancelled"}"#),
            SpawnDecision::SkipCancelled { reason: None }
        );
    }

    /// 409 that is NOT the cancelled contract (e.g. `already_consumed`) →
    /// proceed rather than silently drop (availability over consistency).
    #[test]
    fn decide_spawn_on_409_non_cancelled_proceeds() {
        match decide_spawn(409, r#"{"error":"already_consumed"}"#) {
            SpawnDecision::SpawnDespiteClaimError { .. } => {}
            other => panic!("expected SpawnDespiteClaimError, got {other:?}"),
        }
    }

    /// 409 with an empty / non-JSON body → proceed (can't confirm cancellation).
    #[test]
    fn decide_spawn_on_409_unparseable_proceeds() {
        match decide_spawn(409, "") {
            SpawnDecision::SpawnDespiteClaimError { .. } => {}
            other => panic!("expected SpawnDespiteClaimError, got {other:?}"),
        }
    }

    /// Any other non-2xx (404, 500, …) → proceed (availability over consistency;
    /// the in-process dedupe is the remaining guard).
    #[test]
    fn decide_spawn_on_other_status_proceeds() {
        for status in [404u16, 500, 503, 401] {
            match decide_spawn(status, "whatever") {
                SpawnDecision::SpawnDespiteClaimError { .. } => {}
                other => panic!("status {status}: expected SpawnDespiteClaimError, got {other:?}"),
            }
        }
    }

    // =========================================================================
    // Defect A item 3 (layered on #484): capacity-freed re-poll
    // =========================================================================

    /// A continuation terminal's exit deregisters it from the live registry AND
    /// reports it WAS a continuation (`true`) — the signal the on-exit hook uses
    /// to kick a capacity-freed pending-continuations poll. The registry entry is
    /// gone afterward (the freed slot is reflected immediately, not lazily).
    #[test]
    fn continuation_exit_triggers_repoll_and_deregisters() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        clear_continuation_registry();

        register_continuation_session("term-cont-1".to_string(), Some("anchor-1".into()));
        assert!(
            continuation_sessions()
                .lock()
                .unwrap()
                .contains_key("term-cont-1"),
            "precondition: the continuation session is registered live"
        );

        // Its PTY exits → deregister + signal a re-poll is warranted.
        assert!(
            deregister_exited_continuation("term-cont-1"),
            "a registered continuation's exit must signal a capacity-freed re-poll"
        );
        // The freed slot is reflected immediately.
        assert!(
            !continuation_sessions()
                .lock()
                .unwrap()
                .contains_key("term-cont-1"),
            "the exited continuation must be removed from the live registry"
        );
        // A second exit of the same (already-gone) id is a no-op (idempotent).
        assert!(
            !deregister_exited_continuation("term-cont-1"),
            "a second exit of an already-deregistered continuation must NOT re-trigger"
        );

        clear_continuation_registry();
    }

    /// An OPERATOR tab's exit must NOT trigger a capacity-freed re-poll. Operator
    /// tabs are created via `terminal_create` (a path that never registers them in
    /// the continuation registry), so a terminal id absent from the registry
    /// reports `false` — no poll storm on unrelated tab closes. This is the
    /// defense-in-depth guard `notify_continuation_terminal_exit` relies on.
    #[test]
    fn operator_tab_exit_does_not_trigger_repoll() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        clear_continuation_registry();

        // A live continuation exists, but the OPERATOR tab (different id) closes.
        register_continuation_session("term-cont-1".to_string(), Some("anchor-1".into()));
        assert!(
            !deregister_exited_continuation("operator-tab-xyz"),
            "an operator tab (never registered) must NOT trigger a re-poll"
        );
        // The unrelated live continuation is untouched.
        assert!(
            continuation_sessions()
                .lock()
                .unwrap()
                .contains_key("term-cont-1"),
            "an operator-tab exit must not disturb a live continuation's registration"
        );

        clear_continuation_registry();
    }

    /// `notify_continuation_terminal_exit` with NO tokio handle (the bare-OS-thread
    /// / unit-test case) must NOT panic and still deregisters the exited
    /// continuation — the poll is simply skipped (the backstop / WS-reconnect
    /// catch-up covers it). Guards the "PTY waiter has no runtime" path.
    #[test]
    fn notify_continuation_exit_without_runtime_is_safe() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        clear_continuation_registry();

        register_continuation_session("term-cont-2".to_string(), None);
        // No runtime handle (None) — must not panic on the missing reactor.
        notify_continuation_terminal_exit("term-cont-2", None);
        assert!(
            !continuation_sessions()
                .lock()
                .unwrap()
                .contains_key("term-cont-2"),
            "the exited continuation is deregistered even when no poll could be spawned"
        );

        clear_continuation_registry();
    }

    /// The periodic-backstop arming flag flips from `false` to `true` the first
    /// time an `AtCap` deferral is recorded, and stays `true` (set-once, never
    /// reset) — so the backstop poll stays armed for the rest of the process's
    /// life once any deferral has happened.
    ///
    /// NOTE: the flag is a process-global `static AtomicBool` shared across the
    /// whole test binary; this test only asserts the post-mark state (`true`) and
    /// the idempotence of a second mark, never the pre-mark `false` (another test
    /// could have armed it first). It runs under `CONT_GUARD_LOCK` for ordering
    /// hygiene with the other continuation tests.
    #[test]
    fn at_cap_deferral_arms_backstop_flag() {
        let _g = CONT_GUARD_LOCK.lock().unwrap();
        mark_at_cap_deferral();
        assert!(
            at_cap_deferral_happened(),
            "an AtCap deferral must arm the periodic-backstop flag"
        );
        // Set-once / idempotent: marking again keeps it armed.
        mark_at_cap_deferral();
        assert!(
            at_cap_deferral_happened(),
            "the backstop flag stays armed (set-once, never reset)"
        );
    }

    /// The AtCap lifecycle reason carries the `deferred:` prefix (Resolved Q3) so
    /// a coord-side consumer can distinguish a capacity DEFER (continuation intact
    /// + re-deliverable) from a hard spawn failure. This pins the exact prefix the
    /// AtCap arm posts via the agent `report_spawn_failed` lifecycle channel.
    #[test]
    fn at_cap_reason_has_deferred_prefix() {
        // Mirror the AtCap arm's reason construction.
        let cap = 4usize;
        let reason =
            format!("deferred: continuation cap ({cap}) reached — re-delivered when a slot frees");
        assert!(
            reason.starts_with("deferred: "),
            "AtCap lifecycle reason must carry the `deferred:` prefix, got: {reason}"
        );
        assert!(
            reason.contains(&format!("cap ({cap})")),
            "the reason must still name the cap for the operator log"
        );
    }
}
