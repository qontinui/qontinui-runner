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

/// The runner's connected-vs-isolated decision, imported (not re-wrapped) from
/// its single definition in `profiles`. Every coord surface in this module
/// no-ops when it is `None` (the runner is standalone).
use qontinui_runner_lib::profiles::connected_coord_base;

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
    /// Work unit this spawn belongs to, under the post-rename key.
    ///
    /// Read it through [`LaunchPayload::work_unit_slug`], never directly —
    /// coord emits BOTH keys during the dual window and the accessor applies
    /// the precedence.
    ///
    /// The `_new` suffix exists only to free the `work_unit_slug` *identifier*
    /// for the accessor; the WIRE key is plain `work_unit_slug` via the
    /// `rename`. Dropping that rename makes the runner silently look for a
    /// `work_unit_slug_new` key coord never sends, so the new-key path goes
    /// dead while the legacy fallback keeps it looking healthy.
    #[serde(default, rename = "work_unit_slug")]
    pub work_unit_slug_new: Option<String>,
    /// The legacy key. **Two fields, deliberately NOT `#[serde(alias)]`.**
    ///
    /// An alias would make serde treat the two spellings as the SAME field, so
    /// a body carrying both would fail with `duplicate field`. That hard-fail
    /// is the right behaviour for a REQUEST coord receives (one caller sends
    /// one name; both is genuinely ambiguous — see coord's `SpawnRequest`),
    /// but it is exactly wrong here: this struct parses a payload coord
    /// *deliberately* dual-emits during the rename window, so an alias would
    /// reject every spawn from a renamed coord. Verified: coord's
    /// `LaunchPayload` serializes `{"plan_slug":…,"work_unit_slug":…}`, which
    /// an aliased field rejects with ``duplicate field `work_unit_slug` ``.
    #[serde(default)]
    pub plan_slug: Option<String>,
    #[serde(default)]
    pub plan_phase: Option<u32>,
    #[serde(default)]
    pub correlation_topic: Option<String>,
    /// Optional per-spawn Claude account pin
    /// (plan `2026-08-25-general-purpose-session-spawn-machine-account-prompt`
    /// Phase 3). The value is the account LABEL — the config-dir basename, the
    /// same identity the per-device account feed publishes — or, equivalently,
    /// a friendly name or a full roster `config_dir`; all three resolve
    /// through [`crate::ai_provider::resolve_requested_account`].
    ///
    /// `Some` OVERRIDES [`crate::settings::AccountSelectionMode`] for this
    /// spawn only (a per-child `CLAUDE_CONFIG_DIR`, never the machine-global
    /// `switch_claude_account` mutation — that would leak one spawn's choice
    /// into every other session on the box). `None` leaves today's least-usage
    /// rotation untouched.
    ///
    /// An unresolvable pin FAILS the spawn (see [`resolve_spawn_account_with`]);
    /// it never degrades to rotation, because a pinned account silently ignored
    /// is indistinguishable from one honoured.
    #[serde(default)]
    pub account: Option<String>,
}

impl LaunchPayload {
    /// The work-unit slug under either wire spelling, new key winning.
    ///
    /// Coord dual-emits `work_unit_slug` alongside the legacy `plan_slug` for
    /// one release; after it drops the legacy key this collapses to a plain
    /// read of `work_unit_slug_new` and the `plan_slug` field goes away.
    pub fn work_unit_slug(&self) -> Option<&str> {
        self.work_unit_slug_new
            .as_deref()
            .or(self.plan_slug.as_deref())
    }
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

/// The `source` discriminator coord stamps on a condition-check spawn frame.
///
/// A condition check is coord dispatching an AI session to this runner to verify
/// UI "conditions" via the UI Bridge and report the result back with a curl. It
/// reuses the same `events.agent.spawn_requested.<device>` WS surface as a gate
/// continuation but carries an even MORE minimal payload (no repos, no gate id,
/// no worktree) — see [`ConditionCheckPayload`]. Routed by this `source` value
/// into [`dispatch_condition_check`] BEFORE the gate/`LaunchPayload` parses.
const CONDITION_CHECK_SOURCE: &str = "condition_check";

/// Minimal condition-check spawn payload published by coord to
/// `events.agent.spawn_requested.<device>`.
///
/// A condition check does NOT edit code — it drives the UI Bridge to verify a set
/// of UI conditions at `target_url` and curls the outcome back to coord. So,
/// unlike [`GateContinuationPayload`], it carries no `repos`, no worktree, no
/// gate id and needs no coord ack handshake: the operator-visible terminal spawn
/// is the whole contract. `initial_prompt` already contains the full UI Bridge
/// instructions and the report-back curl (coord composes it).
///
/// A `source` of `"condition_check"` is the wire discriminator that routes a
/// frame into [`dispatch_condition_check`] instead of the gate / `LaunchPayload`
/// arms.
#[derive(Debug, Clone, Deserialize)]
pub struct ConditionCheckPayload {
    /// The coord run id this check belongs to (used for the terminal title and
    /// correlation logging). A UUID string.
    pub run_id: String,
    /// Explicit target device (defensive). Coord's WS pattern filter is already
    /// device-scoped, so this is usually redundant; when present and it does not
    /// match this runner's device id the frame is ignored. `Option` because a
    /// coord that relies solely on the WS filter may omit it.
    #[serde(default)]
    pub target_device_id: Option<String>,
    /// The URL whose UI conditions are being checked. Informational here (the
    /// prompt already embeds it); surfaced in logs for correlation.
    pub target_url: String,
    /// The full prompt to run in the spawned session — already contains the UI
    /// Bridge instructions and the report-back curl.
    pub initial_prompt: String,
    /// How to surface the session. Defaults to [`Presentation::Terminal`] when
    /// coord omits the field. A condition check is inherently operator-visible,
    /// so this handler always spawns a visible terminal; the field is carried for
    /// wire-compat and logged.
    #[serde(default)]
    pub presentation: Presentation,
    /// Wire discriminator. Always `"condition_check"` for this shape; the arm
    /// only deserializes a frame here after confirming this value.
    #[serde(default)]
    pub source: String,
}

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

/// Env var (debug / `test-fixtures` builds only) that compresses the per-agent
/// TokenSlot's BOOKKEEPING `exp` to `now + n` seconds. Lets a test fire the
/// proactive-refresh boundary in seconds while the REAL JWT in the slot is still
/// valid and authenticates the refresh POST. Absent in release builds.
#[cfg(any(debug_assertions, feature = "test-fixtures"))]
const AGENT_JWT_EXP_COMPRESS_ENV: &str = "QONTINUI_AGENT_JWT_EXP_COMPRESS_SECS";

/// Resolve the bookkeeping `exp` to stamp into a freshly-seeded per-agent
/// `TokenSlot`. The real `payload.jwt` is NEVER touched — only this bookkeeping
/// value, which `agent_token::maybe_refresh` reads to decide whether to refresh.
///
/// In debug / `test-fixtures` builds, if `QONTINUI_AGENT_JWT_EXP_COMPRESS_SECS`
/// parses to an `i64` `n`, the result is clamped to `min(jwt_exp, now + n)` so a
/// test can compress the ~4h refresh boundary to seconds. In release builds (no
/// cfg) it is always `jwt_exp`.
#[cfg(any(debug_assertions, feature = "test-fixtures"))]
fn compressed_jwt_exp(jwt_exp: i64) -> i64 {
    match std::env::var(AGENT_JWT_EXP_COMPRESS_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
    {
        Some(n) => {
            let compressed = std::cmp::min(jwt_exp, chrono::Utc::now().timestamp() + n);
            tracing::warn!(
                "agent_runtime: {AGENT_JWT_EXP_COMPRESS_ENV}={n} set — compressing agent \
                 TokenSlot bookkeeping exp {jwt_exp} -> {compressed} (real JWT untouched; \
                 debug/test-fixtures only)"
            );
            compressed
        }
        None => jwt_exp,
    }
}

/// Release variant: no env knob exists, so the bookkeeping `exp` is always the
/// payload's real `exp`.
#[cfg(not(any(debug_assertions, feature = "test-fixtures")))]
fn compressed_jwt_exp(jwt_exp: i64) -> i64 {
    jwt_exp
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

/// Resolve the coord WS URL, normalizing the scheme to `ws://` / `wss://` and
/// ensuring the `/ws` path is present (exactly once) with an `events.agent.*`
/// pattern filter.
///
/// Resolved through [`connected_coord_base`] — the SAME door
/// [`spawn_runtime`]'s gate uses. It previously read the raw profile
/// `coord_url` directly (`load_strict().ok()?.coord_url?`), which honored
/// neither `COORD_HTTP_URL` nor the hosted-tier default. That disagreed with
/// the gate: on a `qontinui_account`-tier runner whose profiles.json has no
/// `coord_url` — the shipped end-user configuration — the gate passed, then
/// this returned `None`, the subscriber loop exited `Ok(())`, and
/// `spawn_supervised_forever` read that as a restart. The result was a
/// permanent 5s→300s respawn loop with agent-spawn delivery dead (there is no
/// poll backstop for `events.agent.spawn_requested`) while the logs claimed
/// the runtime was up. The gate and the resolver must read the same fact.
///
/// [`connected_coord_base`] hands back the coord HTTP base with any `/ws`
/// suffix already stripped by `profiles::coord_ws_to_http` (so the shipped
/// `wss://coord.qontinui.io/ws` arrives here as `https://coord.qontinui.io`);
/// [`build_coord_ws_url`] then flips the scheme and appends `/ws`. That append
/// stays idempotent because the builder also accepts a base that already ends
/// in `/ws`: producing `…/ws/ws` would 401 at the ALB and the subscribe loop
/// would never connect in prod. The coord `/ws` endpoint is a Redis pub/sub
/// bridge at the `/ws` path (not the root); connecting to the root also 401s.
fn coord_ws_url(device_id: uuid::Uuid) -> Option<String> {
    let coord_base = connected_coord_base()?;
    Some(build_coord_ws_url(&coord_base, device_id))
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
/// `pub(crate)` so `mcp::session_message_poller` can stamp its
/// delivery-blocked surfacing POSTs with the device identity.
pub(crate) fn load_local_device_id() -> Option<uuid::Uuid> {
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

/// Resolve `claude` to an absolute, directly-launchable executable path for
/// PTY-spawned continuation terminals (condition-check, gate-continuation).
///
/// These terminals spawn `claude` as the PTY child DIRECTLY (portable-pty
/// `CommandBuilder::new(program)` → Windows `CreateProcessW`), which does NOT
/// apply `PATHEXT`. A bare `"claude"` then resolves against the terminal's
/// PATH — where the always-on identity-shim dir is PREPENDED — to the
/// EXTENSIONLESS shim script, and `CreateProcessW` rejects that non-PE file
/// with `%1 is not a valid Win32 application (os error 193)`. Resolving to an
/// absolute executable in the RUNNER's env (which has no per-terminal shim
/// dir) sidesteps both the PATHEXT gap and the shim shadowing.
///
/// Only `.exe`/`.com` candidates are matched — `CreateProcessW` cannot launch
/// a `.bat`/`.cmd` script directly (that requires a `cmd.exe /c` wrapper,
/// which this direct-PTY-spawn path does not build), so matching one here
/// would trade one os-error-193 cause for another. An npm-only `claude`
/// install (ships only `claude.cmd`/`claude.ps1`, no `.exe`) has no native
/// candidate to find; this correctly falls through to the bare-name fallback
/// below — no worse than pre-fix behavior for that install shape.
///
/// Falls back to [`claude_bin_path`] when nothing resolves, so behavior is
/// never worse than the bare name. Does blocking filesystem stats — callers
/// on an async task must run this via `spawn_blocking`.
pub(crate) fn resolve_claude_bin() -> String {
    let bare = claude_bin_path();
    // An explicit `QONTINUI_CLAUDE_BIN` override (or any absolute path) is
    // launchable as-is — no PATH search, no shim shadowing.
    if std::path::Path::new(&bare).is_absolute() {
        return bare;
    }
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return bare,
    };
    // Skip our own per-terminal shim/identity dirs — they hold the
    // extensionless script that triggers os error 193 under a direct
    // `CreateProcessW`.
    use crate::install_effects_producer::intercept::shim_materializer::{
        IDENTITY_DIR_PREFIX, SHIM_DIR_PREFIX,
    };
    let is_shim_dir = |d: &std::path::Path| {
        d.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(IDENTITY_DIR_PREFIX) || n.starts_with(SHIM_DIR_PREFIX))
    };
    // Only extensions CreateProcessW can natively launch — see doc comment.
    #[cfg(windows)]
    let exts: &[&str] = &[".exe", ".com"];
    for dir in std::env::split_paths(&path) {
        if is_shim_dir(&dir) {
            continue;
        }
        #[cfg(windows)]
        {
            for ext in exts {
                let cand = dir.join(format!("{bare}{ext}"));
                if cand.is_file() {
                    return cand.to_string_lossy().into_owned();
                }
            }
        }
        #[cfg(not(windows))]
        {
            let cand = dir.join(&bare);
            if cand.is_file() {
                return cand.to_string_lossy().into_owned();
            }
        }
    }
    bare
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

    if connected_coord_base().is_none() {
        info!(
            "agent_runtime: runner is ISOLATED (no coord configured, not a hosted \
             tier) — agent spawn runtime disabled. Skipping."
        );
        return;
    }

    info!("agent_runtime: starting for device_id={}", device_id);
    // Periodic backstop poll (always armed): drains a pending continuation /
    // unit dispatch even when every push-shaped trigger is missed — a lost WS
    // frame while connected, a deferred (AtCap) continuation whose terminal
    // exit-hook trigger never fired, a slot freed between WS connects.
    // Spawned independently of the WS pump so it survives subscription flaps.
    spawn_continuation_backstop_poll(device_id);
    // Supervised (panic net): a panic anywhere inside the WS pump used to kill
    // this bare task silently and permanently disable push delivery for the
    // process lifetime. The supervisor restarts it with backoff instead.
    spawn_supervised_delivery("spawn-request-subscriber", move || async move {
        if let Err(e) = subscribe_to_spawn_requests(device_id).await {
            error!("agent_runtime: subscriber exited with error: {e:#}");
        }
    });
}

/// Initial (and reset) restart backoff for a supervised delivery task.
const SUPERVISED_BACKOFF_BASE: Duration = Duration::from_secs(5);

/// Capped maximum restart backoff for a supervised delivery task.
const SUPERVISED_BACKOFF_CAP: Duration = Duration::from_secs(300);

/// A supervised delivery task that ran at least this long before dying counts
/// as a HEALTHY run: its next restart resets the backoff to
/// [`SUPERVISED_BACKOFF_BASE`] instead of continuing the doubling ladder.
const SUPERVISED_HEALTHY_RUN: Duration = Duration::from_secs(600);

/// Spawn a long-lived delivery task under a restart supervisor (panic net).
///
/// Both continuation-delivery tasks (the WS subscriber and the periodic
/// backstop poll) used to be bare `tokio::spawn`s with dropped `JoinHandle`s:
/// a single panic anywhere in their bodies killed the task SILENTLY and
/// PERMANENTLY — no more push frames / no more backstop polls for the process
/// lifetime, with nothing in the logs but the default panic print. This is an
/// OUTER panic net only — each task's internal reconnect / interval loop is
/// unchanged.
///
/// Thin wiring over the crate's ONE supervision idiom
/// ([`crate::mcp::task_supervisor`]) in its process-lifetime (no shutdown
/// signal) form, with the delivery ladder: base 5s, doubling, cap 300s, reset
/// after a 10-min healthy run — slower than the local relay/refresher
/// defaults because every restart here re-hits coord.
fn spawn_supervised_delivery<F, Fut>(name: &'static str, make_run: F)
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let _handle = crate::mcp::task_supervisor::spawn_supervised_forever(
        name,
        SUPERVISED_BACKOFF_BASE,
        SUPERVISED_BACKOFF_CAP,
        SUPERVISED_HEALTHY_RUN,
        make_run,
    );
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
            warn!(
                "agent_runtime: runner is ISOLATED (no coord configured, not a hosted \
                 tier) — subscriber loop exiting"
            );
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
            if envelope_is_condition_check(&value) {
                // Condition check: coord dispatched an AI session to verify UI
                // conditions via the UI Bridge and report back. Spawn an
                // operator-visible terminal — parallel to gate_continuation but
                // with no worktree / gate ack (there is no gate). Routed FIRST by
                // its `source` discriminator; the three spawn shapes are mutually
                // exclusive.
                match parse_condition_check_payload(&value) {
                    Some(payload) => dispatch_condition_check(payload, device_id),
                    None => warn!(
                        "agent_runtime: condition-check envelope on {channel} had no \
                         parseable payload/body"
                    ),
                }
            } else if envelope_is_gate_continuation(&value) {
                match parse_gate_continuation_payload(&value) {
                    Some(payload) => {
                        // DETACHED, deliberately: `dispatch_gate_continuation`
                        // now awaits the agent-registry check, which can pay a
                        // coord round-trip. `handle_message` runs INLINE in the
                        // WS read loop, so awaiting it here would stall the pump
                        // (missing keepalive ticks → coord drops the connection →
                        // reconnect churn → replay storm). Same reason
                        // `dispatch_condition_check` spawns detached. Outcome
                        // counts matter only on the poll path, which still awaits.
                        tokio::spawn(async move {
                            let _ = dispatch_gate_continuation(payload, device_id).await;
                        });
                    }
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

/// Does this spawn-request envelope carry the condition-check discriminator
/// (`source == "condition_check"`)? Routes a frame into the dedicated
/// condition-check arm BEFORE the gate / full-`LaunchPayload` parses — the three
/// shapes are mutually exclusive by `source`. Mirrors
/// [`envelope_is_gate_continuation`].
fn envelope_is_condition_check(envelope: &serde_json::Value) -> bool {
    envelope_inner_value(envelope)
        .and_then(|v| {
            v.get("source")
                .and_then(|s| s.as_str())
                .map(|s| s == CONDITION_CHECK_SOURCE)
        })
        .unwrap_or(false)
}

/// Extract a [`ConditionCheckPayload`] from a coord `/ws` envelope. Mirrors
/// [`parse_gate_continuation_payload`]'s string-or-object inner handling.
/// Returns `None` if the inner JSON does not deserialize into the condition-check
/// shape.
fn parse_condition_check_payload(envelope: &serde_json::Value) -> Option<ConditionCheckPayload> {
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
        // Agent-registry spawn authorization (plan
        // `2026-07-28-migrate-claude-md-into-qontinui.md` Phase 4c, served
        // clause `agent-spawn-authorization`). This is coord's primary
        // auto-dispatch funnel (`events.agent.spawn_requested.<device_id>`):
        // coord hands the runner a full launch payload and the agent runs on
        // the user's own AI quota, outliving the request entirely. Standing
        // per-path opt-in, default OFF for a fresh user.
        //
        // Inside the task, not before it: `authorize_spawn` can pay a coord
        // round-trip and both callers sit inline in the WS pump, which must
        // keep serving keepalives. The stop-token entry is registered
        // synchronously above (so an immediate stop request still finds the
        // agent) and is removed here on refusal; nothing else has been
        // created yet, so there is no other state to unwind.
        let authz = crate::agent_authorization::authorize_spawn(
            None,
            crate::agent_authorization::SpawnPath::StandingContinuation,
        )
        .await;
        if !authz.allows_spawn() {
            let reason = format!(
                "agent-registry spawn authorization refused this launch ({}): {}",
                authz.label(),
                authz.reason().unwrap_or("no reason recorded")
            );
            warn!("agent_runtime: coord spawn-request agent_id={agent_id} NOT launched — {reason}");
            agent_stops().lock().unwrap().remove(&agent_id);
            // Report a TERMINAL outcome to coord. `run_agent_subprocess` is the
            // only thing that posts spawn_complete/spawn_failed, so returning
            // silently would leave coord's agent row in `spawning` forever and
            // re-dispatch the same request on every reconnect — an endless
            // dispatch/refuse loop. Unlike the gate-continuation path (whose
            // row must stay PENDING and re-listable), a launch payload has no
            // deferral shape: the honest answer is "this device will not run
            // it, and here is why".
            report_spawn_failed(agent_id, &reason, None, 0, None).await;
            return;
        }
        if let Err(e) = run_agent_subprocess(payload, stop).await {
            error!("agent_runtime: run_agent_subprocess failed: {e:#}");
        }
        // Drop the registry entry once the run task is fully done.
        agent_stops().lock().unwrap().remove(&agent_id);
        // Drop the agent's live-token slot so its proxy nonce hard-fails closed
        // (the agent process is gone; any lingering `.mcp.json` nonce must 401)
        // — and revoke the nonce registration itself (credential-hygiene
        // Task 5): a torn-down agent's nonce should disappear from the
        // registry, not linger as a permanently-401ing entry.
        crate::coord_mcp::remove_agent_token(agent_id);
        crate::coord_mcp::revoke_agent_proxy_nonces(agent_id);
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

/// One-shot marker so a recovered lock poisoning is reported loudly exactly
/// once per process (recoveries after the first would be pure log noise —
/// every subsequent acquisition of a poisoned mutex re-observes the poison).
static LOCK_POISON_REPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Lock a continuation-runtime `std::sync::Mutex`, RECOVERING from poisoning
/// instead of propagating the panic.
///
/// These registries (`dispatched_gate_ids`, `dispatched_dispatch_ids`,
/// `continuation_sessions`, the deferred-stamp rate-limit map) hold plain
/// collections whose contents are valid even if a holder panicked mid-update —
/// recovery is always safe. A bare `.lock().unwrap()` here turned ONE panic
/// while holding the lock into a cascading panic at EVERY later delivery (a
/// permanently-dead continuation consumer); with this helper the poisoning is
/// loud (one `error!`) instead of lethal.
fn lock_recover<'a, T>(
    mutex: &'a std::sync::Mutex<T>,
    what: &'static str,
) -> std::sync::MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        if !LOCK_POISON_REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            error!(
                "agent_runtime: recovered a POISONED {what} lock — a prior holder \
                 panicked mid-update; continuing with the recovered state \
                 (reported once per process)"
            );
        }
        poisoned.into_inner()
    })
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
    lock_recover(dispatched_gate_ids(), "dispatched_gate_ids").insert(gate_id)
}

/// Release an in-process gate-dispatch claim ([`claim_gate_dispatch`]).
///
/// **The load-bearing half of the delivery-stall fix.** The dedupe set's ONLY
/// purpose is the WS+poll double-delivery race: an id must stay claimed only
/// while a dispatch is IN-FLIGHT or after the continuation was actually
/// CONSUMED on coord (the consume claim POSTed). Every LOCAL skip that leaves
/// the row pending on coord (AtCap, DuplicateAnchor, device-mismatch, an `Err`
/// before/around the consume claim) must release, or the skip is permanent for
/// the process lifetime: the backstop poll re-lists the row every tick and the
/// dispatcher drops it at the dedupe check forever — the exact mechanism that
/// stranded 51 continuations pending-with-null-outcomes over 2 days.
fn release_gate_dispatch(gate_id: uuid::Uuid) {
    lock_recover(dispatched_gate_ids(), "dispatched_gate_ids").remove(&gate_id);
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
    lock_recover(dispatched_dispatch_ids(), "dispatched_dispatch_ids").insert(dispatch_id)
}

/// Release an in-process unit-dispatch claim ([`claim_dispatch_dispatch`]) —
/// the sibling of [`release_gate_dispatch`] for the work-unit path. Load
/// bearing for the unit contract's at-least-once promise: a failed spawn is
/// deliberately left un-consumed so coord re-lists it, but WITHOUT this
/// release the re-listed row would be dropped at the in-process dedupe check
/// forever (the same permanent-deferral defect as the gate path).
fn release_dispatch_dispatch(dispatch_id: uuid::Uuid) {
    lock_recover(dispatched_dispatch_ids(), "dispatched_dispatch_ids").remove(&dispatch_id);
}

/// Release whichever in-process dedupe claim the dispatcher took for this
/// continuation, per its [`ConsumeTarget`]. Called from every LOCAL-skip exit
/// of [`run_gate_continuation_inner`] that leaves the row pending on coord
/// (see [`release_gate_dispatch`] for the invariant). [`ConsumeTarget::None`]
/// (legacy, no id) never claimed, so there is nothing to release.
fn release_local_dispatch_claim(consume_target: ConsumeTarget) {
    match consume_target {
        ConsumeTarget::Gate(gate_id) => release_gate_dispatch(gate_id),
        ConsumeTarget::Dispatch(dispatch_id) => release_dispatch_dispatch(dispatch_id),
        ConsumeTarget::None => {}
    }
}

// =============================================================================
// Continuation-session registry (P3 anchor_key dedup + P4 concurrency cap)
// =============================================================================

/// Default cap on concurrently-live *continuation-spawned* terminal sessions.
///
/// **64.** Finite since Phase 1 of
/// `2026-08-30-load-aware-spawn-admission-control`; it was `usize::MAX` until
/// the 2026-08-29 wedge, and the reasoning behind that default is the thing the
/// incident actually falsified.
///
/// ## Why the old `usize::MAX` was wrong
///
/// The retired doc argued the cap on the **UI-display** axis: "the Terminal UI
/// scales to unlimited sessions via a 9-zone grid × many page tabs". That is
/// true, and it is not the axis a spawn cap protects. A grid that can *render*
/// 130 tabs says nothing about whether the box can *carry* 130 concurrent
/// `CreateProcess` calls, and on 2026-08-29 it could not: ~130 continuation
/// spawns landed on an already-loaded primary, the process reached **540 OS
/// threads** against tokio's 512-slot blocking pool with 119 of them parked
/// mid-`CreateProcess`, and the runner wedged. The one guard purpose-built to
/// sit in that exact path was this cap, and it was infinite. A default reasoned
/// on the wrong axis is indistinguishable from no guard at all.
///
/// ## Why 64, and why it is deliberately the WEAKER of the two limits
///
/// The wedge's own arithmetic gives the conversion: 540 threads at ~130 sessions
/// against a 150-151-thread idle baseline is roughly **3 OS threads per
/// continuation session**. So the shipped thread ceilings
/// ([`crate::settings::SessionGuardSettings::warn_thread_count`] 256 /
/// `critical_thread_count` 400) correspond to about **35** and **83** concurrent
/// sessions. 64 sits between them — which means that in ordinary conditions the
/// thread lane trips FIRST and this count never binds.
///
/// That is intended, not an oversight. The two limits are measuring different
/// things and the honest one is the thread count: it is a live reading of the
/// resource that actually ran out. This cap is the **backstop for the case the
/// thread count cannot see** — sessions that are cheap in threads but expensive
/// in something else the process does not spend a thread on: Postgres
/// connections, coord-mcp JWT-mint round trips, and the per-`CreateProcess`
/// kernel/csrss overhead that a parked thread understates. In that regime the
/// thread lane reads calm while the machine is not, and a finite count is the
/// only thing left holding the line.
///
/// It is therefore sized as a *ceiling on absurdity*, not as a tuned capacity
/// number: at ~3 threads apiece, 64 sessions is ~192 threads of continuation
/// load on top of the idle 150 — over the 256 warn ceiling, under the 400
/// critical one. Nothing on this fleet has ever legitimately wanted more than 64
/// concurrent continuations; the observed peak that broke the box was twice it.
///
/// `QONTINUI_CONTINUATION_SESSION_CAP` remains the operator override, unchanged
/// and in both directions (a bigger number is as settable as a smaller one).
/// Operator-opened sessions are never counted — they are never registered here.
const DEFAULT_CONTINUATION_SESSION_CAP: usize = 64;

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
/// (`terminal_id -> bool`).
///
/// **Never runs `is_live` under the registry lock.** `is_live` reaches into
/// Tauri-managed state (`live_terminal_predicate` → `try_state` →
/// `TerminalManager::is_alive()`) — the highest-probability
/// panic-while-holding-the-lock site. Instead: snapshot the terminal ids under
/// the lock, RELEASE, evaluate liveness on the snapshot, then re-acquire and
/// remove the dead ids. An entry registered between snapshot and re-acquire is
/// simply not liveness-tested this round (it is freshly spawned, i.e. live);
/// an entry removed concurrently makes the removal a no-op.
fn prune_dead_continuations(is_live: &dyn Fn(&str) -> bool) {
    let ids: Vec<String> = lock_recover(continuation_sessions(), "continuation_sessions")
        .keys()
        .cloned()
        .collect();
    // Lock released — evaluate liveness on the snapshot only.
    let dead: Vec<String> = ids.into_iter().filter(|tid| !is_live(tid)).collect();
    if dead.is_empty() {
        return;
    }
    let mut map = lock_recover(continuation_sessions(), "continuation_sessions");
    for tid in &dead {
        map.remove(tid);
    }
}

/// Outcome of the pre-spawn continuation guard (P3 + thread pressure + P4).
#[derive(Debug, PartialEq, Eq)]
enum ContinuationGuard {
    /// Clear to spawn — no live duplicate, machine not under thread pressure,
    /// under cap.
    Proceed,
    /// A live continuation already exists for this `anchor_key` (P3): skip the
    /// spawn (re-cleared gate / duplicate). Carries the existing terminal_id.
    DuplicateAnchor(String),
    /// The machine is out of THREADS, not out of slots: the spawn gate's thread
    /// lane ([`crate::resource_guard::thread_pressure`]) returned something
    /// other than `Proceed`. Defer the spawn.
    ///
    /// Carries the severity word (`"warn"` / `"critical"`) beside the
    /// observation that produced it, so the log and the coord stamp can name the
    /// REAL numbers — the thread count that was read and the ceiling it crossed.
    ///
    /// Deliberately NOT folded into [`ContinuationGuard::AtCap`]. A deferral
    /// reported as "cap reached" when the cap was never reached is a lie in the
    /// runner log, and the log is exactly what the next incident's forensics
    /// reads: the 2026-08-29 investigation spent its time on the cap because the
    /// cap is what the log talks about.
    ThreadPressure {
        /// `"warn"` or `"critical"` — which ceiling the reading crossed. Comes
        /// from [`crate::resource_guard::SpawnGate::tripped`], never re-derived
        /// here, so the word and the number can never disagree.
        severity: &'static str,
        observation: crate::resource_guard::GateObservation,
    },
    /// At the concurrency cap (P4): skip the spawn. Carries the cap for the log.
    AtCap(usize),
}

/// Pre-spawn guard: prune dead sessions, then enforce P3 (anchor_key dedup),
/// machine thread pressure, and P4 (concurrency cap). Pure over (`anchor_key`,
/// `is_live`, the injected thread verdict, env cap) so it is unit-testable
/// without a live `TerminalManager` and without a live thread reading.
///
/// Order matters, and it is now three-deep:
///
/// 1. **Dedup first.** A duplicate of an already-running anchor is reported as a
///    dedup (the honest reason) rather than "capped" or "loaded" — and it is
///    also the one verdict that is true regardless of machine state: spawning it
///    would be wrong on an idle box too.
/// 2. **Thread pressure next.** It goes AHEAD of the count cap because it is the
///    real signal — a live reading of the resource that actually ran out on
///    2026-08-29 — and because it is the earlier, cheaper catch: on this fleet
///    it binds at roughly 35 concurrent sessions (warn) where the count cap
///    binds at 64, so on the path to a wedge it is what fires. Reporting a
///    thread-starved machine as "capped" would send the next investigation to
///    the wrong constant, which is precisely what happened last time.
/// 3. **The count cap last**, as the backstop for load the thread count cannot
///    see (see [`DEFAULT_CONTINUATION_SESSION_CAP`]).
///
/// Operator sessions never enter the registry, so they never count.
///
/// ## Any non-`Proceed` verdict defers — deliberately more eager than `admit_spawn`
///
/// [`crate::resource_guard::admit_spawn`] refuses an operator's own spawn only
/// at [`crate::resource_guard::SpawnGate::Critical`]. This guard defers at
/// `Warn` as well, and the asymmetry is the whole reason `thread_pressure()`
/// hands back the verdict instead of a bool.
///
/// The two callers pay completely different prices for being wrong. A gate
/// continuation is a queued, unattended dispatch: deferring it costs nothing but
/// latency, leaves the row pending and unconsumed on coord, and self-heals —
/// [`spawn_continuation_backstop_poll`] re-delivers it within one interval, and
/// the capacity-freed exit hook usually sooner. Back-pressure that arrives early
/// is the entire point of a queue. An operator's terminal has a human waiting in
/// front of it and no re-delivery path at all; refusing that on a soft signal is
/// the false positive `resource_guard`'s doctrine ranks worst.
///
/// So: cheap and reversible ⇒ act on the light verdict; expensive and terminal
/// ⇒ act only on the heavy one. Same numbers, folded once in `resource_guard`;
/// different trip points, chosen by the caller that bears the cost.
fn evaluate_continuation_guard(
    anchor_key: Option<&str>,
    is_live: &dyn Fn(&str) -> bool,
    thread_pressure: &crate::resource_guard::SpawnGate,
) -> ContinuationGuard {
    // Prune runs `is_live` OUTSIDE the registry lock (see its doc); the P3/P4
    // scan below then runs under a freshly-acquired lock.
    prune_dead_continuations(is_live);
    let map = lock_recover(continuation_sessions(), "continuation_sessions");

    // P3: a LIVE session already exists for this anchor_key → dedup.
    if let Some(anchor) = anchor_key {
        if let Some(existing) = map
            .values()
            .find(|s| s.anchor_key.as_deref() == Some(anchor))
        {
            return ContinuationGuard::DuplicateAnchor(existing.terminal_id.clone());
        }
    }

    // Thread pressure: ANY verdict that is not `Proceed` defers (see the
    // asymmetry argument above). An unreadable thread sensor produces `Proceed`
    // inside `evaluate_threads` — UNKNOWN ⇒ spawn, the fail-open doctrine this
    // whole subsystem is built on — so a missing reading can never wedge the
    // continuation queue shut.
    if let Some((severity, observation)) = thread_pressure.tripped() {
        return ContinuationGuard::ThreadPressure {
            severity,
            observation: observation.clone(),
        };
    }

    // P4: at the cap → refuse.
    let cap = continuation_session_cap();
    if map.len() >= cap {
        return ContinuationGuard::AtCap(cap);
    }

    ContinuationGuard::Proceed
}

/// [`evaluate_continuation_guard`] with the thread verdict taken LIVE from
/// [`crate::resource_guard::thread_pressure`].
///
/// The split exists so the guard itself stays pure over its inputs (the property
/// its own doc claims and its ~12 unit tests rely on): a test injects a
/// [`crate::resource_guard::SpawnGate`] directly, while the one production call
/// site — [`run_gate_continuation_inner`]'s step 1 — takes this wrapper and pays
/// the reading. `thread_pressure()` short-circuits on a disabled session guard
/// before touching the sensor, so a machine owner who turned the guard off pays
/// nothing here either.
fn evaluate_continuation_guard_live(
    anchor_key: Option<&str>,
    is_live: &dyn Fn(&str) -> bool,
) -> ContinuationGuard {
    evaluate_continuation_guard(
        anchor_key,
        is_live,
        &crate::resource_guard::thread_pressure(),
    )
}

/// Register a freshly-spawned continuation session in the live registry (after
/// `create_terminal_session_backend` succeeds). The entry is reaped lazily by
/// [`prune_dead_continuations`] the next time the guard runs.
fn register_continuation_session(terminal_id: String, anchor_key: Option<String>) {
    lock_recover(continuation_sessions(), "continuation_sessions").insert(
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

/// Default backstop-poll cadence (5 min). Env-tunable via
/// [`CONTINUATION_BACKSTOP_POLL_SECS_ENV`].
const CONTINUATION_BACKSTOP_POLL_SECS_DEFAULT: u64 = 300;

/// Floor for the backstop-poll cadence — a misconfigured tiny value must not
/// turn the safety net into a coord hammer.
const CONTINUATION_BACKSTOP_POLL_SECS_FLOOR: u64 = 30;

/// Env var overriding the backstop-poll cadence (seconds). Default
/// [`CONTINUATION_BACKSTOP_POLL_SECS_DEFAULT`], floored at
/// [`CONTINUATION_BACKSTOP_POLL_SECS_FLOOR`].
const CONTINUATION_BACKSTOP_POLL_SECS_ENV: &str = "RUNNER_CONTINUATION_BACKSTOP_POLL_SECS";

/// Resolve the backstop-poll cadence from a raw env value. Pure over the input
/// so the default / floor / garbage-fallback policy is unit-testable without
/// env mutation: unset or non-numeric → default; numeric → floored.
fn resolve_backstop_poll_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v.max(CONTINUATION_BACKSTOP_POLL_SECS_FLOOR))
        .unwrap_or(CONTINUATION_BACKSTOP_POLL_SECS_DEFAULT)
}

/// The configured backstop-poll cadence (env override, else the default).
fn continuation_backstop_poll_secs() -> u64 {
    resolve_backstop_poll_secs(
        std::env::var(CONTINUATION_BACKSTOP_POLL_SECS_ENV)
            .ok()
            .as_deref(),
    )
}

/// Spawn the periodic backstop poll task (Defect A item 3, backstop half;
/// always-on since the `2026-07-03-subagent-stall-watchdog` plan).
///
/// A best-effort safety net for EVERY lost-wakeup path, not just the
/// capacity-freed exit-hook trigger ([`notify_continuation_terminal_exit`]):
/// a WS frame lost while connected, a continuation terminal whose coord
/// registration failed (so no on-exit hook was installed), or a slot that
/// frees while the runner is between WS connects — none of these may strand a
/// pending continuation on coord's queue until an unrelated WS reconnect
/// happens to re-poll. Every [`continuation_backstop_poll_secs`] this task
/// polls coord for pending continuations and unit dispatches
/// **unconditionally**.
///
/// It used to poll only after at least one `AtCap` deferral this process
/// lifetime (the deleted `at_cap_deferral_happened` arming flag) — but that
/// gate meant a WS frame lost while connected, with no AtCap deferral and no
/// terminal exit, stranded a continuation until the next reconnect. The tick
/// is two cheap authenticated GETs against coord; paying them every interval
/// buys at-least-once delivery for the whole class.
///
/// Spawned once per process from [`spawn_runtime`]. Independent of the WS pump
/// so it keeps draining even while the subscription is flapping.
fn spawn_continuation_backstop_poll(device_id: uuid::Uuid) {
    // Supervised (panic net): a panic inside one tick used to kill this bare
    // task silently — no more backstop polls for the process lifetime. See
    // [`spawn_supervised_delivery`]; the interval loop itself is unchanged.
    spawn_supervised_delivery("continuation-backstop-poll", move || {
        continuation_backstop_poll_loop(device_id)
    });
}

/// The backstop poll's interval loop body (runs forever; restarted by the
/// supervisor if it ever panics or returns).
async fn continuation_backstop_poll_loop(device_id: uuid::Uuid) {
    let secs = continuation_backstop_poll_secs();
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Skip the immediate first tick — the WS-connect catch-up poll
    // (`poll_pending_continuations` on connect) covers startup.
    interval.tick().await;
    loop {
        interval.tick().await;
        debug!(
            "agent_runtime: backstop poll firing (every {secs}s, always armed) \
             for device_id={device_id}"
        );
        poll_pending_continuations(device_id).await;
        poll_pending_unit_dispatches(device_id).await;
    }
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
pub(crate) fn emit_terminal_focus_request(app: &tauri::AppHandle, terminal_id: &str) {
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
/// 1. **Agent-registry authorization** (`agent-spawn-authorization`), then
///    **fast-path dedupe**: [`claim_gate_dispatch`]
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
///
/// Returns the fast-path outcome so the poll path can aggregate honest per-run
/// counts for the coord self-report ([`post_continuation_poll_report`]); the WS
/// fast-path ignores it (and dispatches this fn detached, since the
/// agent-registry check below may pay a coord round-trip and the WS read loop
/// must keep serving keepalives).
async fn dispatch_gate_continuation(
    payload: GateContinuationPayload,
    device_id: uuid::Uuid,
) -> DispatchOutcome {
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
        return DispatchOutcome::NotAddressedToSelf;
    }

    // Agent-registry spawn authorization (plan
    // `2026-07-28-migrate-claude-md-into-qontinui.md` Phase 4c, served clause
    // `agent-spawn-authorization`). A gate continuation / work-unit dispatch
    // OUTLIVES the request that created it, so it is a `standing_continuation`:
    // standing per-path opt-in, default OFF for a fresh user, never implied by
    // a task. Checked HERE — after the addressed-to-self gate (so a
    // continuation meant for another instance costs no lookup) and BEFORE the
    // dedupe claim, so a denied continuation is never claimed and stays
    // re-listable if the user later opts in.
    //
    // Note: an anchor key is a gate label, not a registry agent name, so this
    // resolves against the per-path row. `crate::agent_authorization::SpawnDecision`
    // is spelled out because this module has its OWN unrelated `SpawnDecision`
    // (coord's claim verdict) — do not import it.
    let authz = crate::agent_authorization::authorize_spawn(
        None,
        crate::agent_authorization::SpawnPath::StandingContinuation,
    )
    .await;
    if !authz.allows_spawn() {
        let reason = authz.reason().unwrap_or("no reason recorded").to_string();
        warn!(
            "agent_runtime: gate-continuation (gate_id={:?}, dispatch_id={:?}, source={}) \
             NOT dispatched — {}: {}",
            payload.gate_id,
            payload.dispatch_id,
            payload.source,
            authz.label(),
            reason
        );
        // Stamp a NON-CONSUMING reason so coord can see WHY the row is sitting
        // pending, exactly as the `at_cap` / `duplicate_anchor` guards do. The
        // row stays pending and re-listable (no consume claim is taken), but
        // without this stamp coord sees a pending row with a null reason — the
        // shape that stranded 51 continuations for two days.
        if let Some(gate_id) = payload.gate_id {
            if should_post_deferred_stamp(gate_id, std::time::Instant::now()) {
                post_continuation_deferred(
                    gate_id,
                    device_id,
                    format!("spawn_authorization_{}", authz.label()),
                )
                .await;
            }
        }
        return DispatchOutcome::Denied;
    }
    if let crate::agent_authorization::SpawnDecision::Warn { reason } = &authz {
        warn!(
            "agent_runtime: gate-continuation (gate_id={:?}) proceeding under a \
             warn_proceed disposition: {}",
            payload.gate_id, reason
        );
    }

    if let Some(gate_id) = payload.gate_id {
        // Fast-path dedupe: drop an in-process duplicate before any claim.
        // (The agent-registry check above is the one I/O that precedes it — it
        // must, so a refused dispatch never claims anything.)
        if !claim_gate_dispatch(gate_id) {
            debug!(
                "agent_runtime: gate-continuation gate_id={gate_id} already dispatched; \
                 skipping duplicate"
            );
            return DispatchOutcome::AlreadyDispatched;
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
                // An errored run is no longer in-flight and posted no successful
                // consume outcome path we can rely on — release the in-process
                // claim so a re-listed row can retry (see release_gate_dispatch).
                release_gate_dispatch(gate_id);
            }
        });
        DispatchOutcome::Dispatched
    } else if let Some(dispatch_id) = payload.dispatch_id {
        // Work-unit DAG dispatch: reuses the `gate_continuation` spawn frame but
        // is keyed on `dispatch_id` (no gate_id). Applies on BOTH the live WS
        // frame AND the `pending-unit-dispatches` replay-poll path (both route
        // through here), so dedupe + ack happen regardless of arrival path.
        //
        // Fast-path dedupe: drop an in-process duplicate before any claim.
        // (The agent-registry check above is the one I/O that precedes it — it
        // must, so a refused dispatch never claims anything.)
        if !claim_dispatch_dispatch(dispatch_id) {
            debug!(
                "agent_runtime: unit-dispatch dispatch_id={dispatch_id} already dispatched; \
                 skipping duplicate"
            );
            return DispatchOutcome::AlreadyDispatched;
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
                // A failed spawn is deliberately left un-consumed so coord
                // re-lists it — the re-listed row must not be dropped at the
                // in-process dedupe forever (see release_dispatch_dispatch).
                release_dispatch_dispatch(dispatch_id);
            }
        });
        DispatchOutcome::Dispatched
    } else {
        // Legacy coord: neither gate_id nor dispatch_id → no dedupe-by-id and no
        // claim/outcome. Dispatch once with no coord handshake (unchanged).
        spawn_gate_continuation_task(payload, device_id);
        DispatchOutcome::Dispatched
    }
}

/// Synchronous fast-path outcome of [`dispatch_gate_continuation`] — what the
/// dispatcher decided BEFORE any I/O. Consumed by the poll path to build the
/// per-run self-report counts; the WS fast-path ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchOutcome {
    /// A run task was spawned (or the legacy no-id dispatch fired): the id (if
    /// any) was NEWLY claimed by this delivery.
    Dispatched,
    /// Dropped at the in-process dedupe — this id is in-flight or consumed.
    AlreadyDispatched,
    /// Not addressed to this instance (the addressed instance spawns it).
    NotAddressedToSelf,
    /// Refused by the agent registry (`agent-spawn-authorization`): the
    /// tenant/user has not opted this device into standing continuations, or
    /// has disabled them with a `block`/`degrade` disposition. No coord claim
    /// is taken, so the row stays re-listable if the user opts in later.
    Denied,
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
    let Some(base) = connected_coord_base() else {
        return;
    };
    let url = format!("{base}/coord/agents/pending-continuations?device_id={device_id}");
    // Every fetch-failure exit below still self-reports (all-zeros +
    // `skip_reasons.fetch_failed`): coord must be able to tell "poll loop
    // alive but the pull route failing" apart from "poll loop dead".
    let Some(client) = crate::coord_http::coord_client() else {
        warn!("agent_runtime: pending-continuations: no shared coord client");
        post_continuation_poll_report(device_id, PollRunCounts::fetch_failure()).await;
        return;
    };
    let resp = match client
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("agent_runtime: pending-continuations GET failed (continuing): {e:#}");
            post_continuation_poll_report(device_id, PollRunCounts::fetch_failure()).await;
            return;
        }
    };
    if !resp.status().is_success() {
        warn!(
            "agent_runtime: pending-continuations GET returned {} (continuing)",
            resp.status()
        );
        post_continuation_poll_report(device_id, PollRunCounts::fetch_failure()).await;
        return;
    }
    let body: PendingContinuationsResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!("agent_runtime: pending-continuations parse failed (continuing): {e:#}");
            post_continuation_poll_report(device_id, PollRunCounts::fetch_failure()).await;
            return;
        }
    };
    if body.pending.is_empty() {
        debug!("agent_runtime: pending-continuations poll: none pending for device_id={device_id}");
        // Still self-report: a zero-listed report every tick is the liveness
        // evidence coord uses to tell "poll running, queue empty" apart from
        // "poll dead" (the failure mode this plan exists for).
        post_continuation_poll_report(device_id, PollRunCounts::default()).await;
        return;
    }
    info!(
        "agent_runtime: pending-continuations poll: {} pending for device_id={device_id} — replaying",
        body.pending.len()
    );
    let mut counts = PollRunCounts {
        listed_n: body.pending.len(),
        ..Default::default()
    };
    for row in body.pending {
        // The row's gate_id is authoritative; stamp it onto the payload so the
        // shared seam dedupes + acks even if coord omitted it inside `payload`.
        let mut payload = row.payload;
        payload.gate_id = Some(row.gate_id);
        match dispatch_gate_continuation(payload, device_id).await {
            DispatchOutcome::Dispatched => counts.dispatched_n += 1,
            DispatchOutcome::AlreadyDispatched => counts.already_dispatched += 1,
            DispatchOutcome::NotAddressedToSelf => counts.not_addressed_to_self += 1,
            DispatchOutcome::Denied => counts.spawn_authorization_denied += 1,
        }
    }
    post_continuation_poll_report(device_id, counts).await;
}

/// Aggregate counts from one [`poll_pending_continuations`] run, feeding the
/// coord self-report. `listed_n` = rows fetched; `dispatched_n` = ids NEWLY
/// claimed this run; the two skip counters are the dedupe-drops and the
/// not-addressed-to-this-instance drops (skipped = listed − dispatched).
/// `fetch_failed` marks a run whose pending-list pull itself failed (client
/// build / GET / non-2xx / parse) — reported so coord can tell "poll loop
/// alive but the pull route failing" apart from "poll loop dead" (the exact
/// ambiguity the self-report exists to kill).
#[derive(Debug, Default, Clone, Copy)]
struct PollRunCounts {
    listed_n: usize,
    dispatched_n: usize,
    already_dispatched: usize,
    not_addressed_to_self: usize,
    /// Refused by the agent registry (`agent-spawn-authorization`). Counted
    /// separately so coord can tell "the user has not opted this device into
    /// standing continuations" apart from a dedupe drop or a dead poll loop —
    /// the two look identical in `skipped_n` alone.
    spawn_authorization_denied: usize,
    fetch_failed: bool,
}

impl PollRunCounts {
    /// The counts for a run whose pending-list pull failed before any row was
    /// listed: all zeros + the `fetch_failed` skip reason.
    fn fetch_failure() -> Self {
        Self {
            fetch_failed: true,
            ..Default::default()
        }
    }
}

/// Wire body for `POST /coord/agents/continuation-poll-report`.
#[derive(Debug, Serialize)]
struct ContinuationPollReportBody {
    device_id: uuid::Uuid,
    listed_n: usize,
    dispatched_n: usize,
    skipped_n: usize,
    skip_reasons: std::collections::BTreeMap<&'static str, usize>,
}

impl ContinuationPollReportBody {
    fn new(device_id: uuid::Uuid, counts: PollRunCounts) -> Self {
        let mut skip_reasons = std::collections::BTreeMap::new();
        if counts.already_dispatched > 0 {
            skip_reasons.insert("already_dispatched", counts.already_dispatched);
        }
        if counts.not_addressed_to_self > 0 {
            skip_reasons.insert("not_addressed_to_self", counts.not_addressed_to_self);
        }
        if counts.spawn_authorization_denied > 0 {
            skip_reasons.insert(
                "spawn_authorization_denied",
                counts.spawn_authorization_denied,
            );
        }
        if counts.fetch_failed {
            skip_reasons.insert("fetch_failed", 1);
        }
        Self {
            device_id,
            listed_n: counts.listed_n,
            dispatched_n: counts.dispatched_n,
            skipped_n: counts.listed_n.saturating_sub(counts.dispatched_n),
            skip_reasons,
        }
    }
}

/// POST the per-run poll self-report to coord
/// (`POST /coord/agents/continuation-poll-report`). Pure observability: lets
/// coord tell a live-but-empty poll apart from a dead poll loop, and surfaces
/// a runner that lists rows every tick but dispatches none (the permanent
/// dedupe-drop stall this plan fixes). **Best-effort, never affects
/// delivery**: a 404 (coord's route not deployed yet — parallel phase) is
/// `debug!`; any other non-2xx or transport error is one `warn!`.
async fn post_continuation_poll_report(device_id: uuid::Uuid, counts: PollRunCounts) {
    let Some(base) = connected_coord_base() else {
        return;
    };
    let url = format!("{base}/coord/agents/continuation-poll-report");
    let Some(client) = crate::coord_http::coord_client() else {
        warn!("agent_runtime: poll-report: no shared coord client (continuing)");
        return;
    };
    let body = ContinuationPollReportBody::new(device_id, counts);
    // coord-tenant-scope(device): body is ContinuationPollReportBody::new(device_id, counts) (:2241); coord's poll-report row has no tenant column and the handler takes no auth extractor.
    match crate::auth::attach_device_auth(client.post(&url))
        .timeout(Duration::from_secs(5))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            debug!(
                "agent_runtime: poll self-report posted (listed={} dispatched={} skipped={})",
                body.listed_n, body.dispatched_n, body.skipped_n
            );
        }
        Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
            debug!("agent_runtime: poll-report POST 404 (endpoint not deployed yet; continuing)");
        }
        Ok(resp) => {
            warn!(
                "agent_runtime: poll-report POST returned {} (continuing)",
                resp.status()
            );
        }
        Err(e) => warn!("agent_runtime: poll-report POST failed (continuing): {e:#}"),
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
    let Some(base) = connected_coord_base() else {
        return;
    };
    let url = format!("{base}/coord/agents/pending-unit-dispatches?device_id={device_id}");
    let Some(client) = crate::coord_http::coord_client() else {
        warn!("agent_runtime: pending-unit-dispatches: no shared coord client");
        return;
    };
    let resp = match client
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
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
        // Outcome counts are aggregated only on the gate-continuations poll.
        let _ = dispatch_gate_continuation(payload, device_id).await;
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
    let Some(base) = connected_coord_base() else {
        return SpawnDecision::Spawn;
    };
    let url = format!("{base}/coord/gates/{gate_id}/continuation-consumed");
    let Some(client) = crate::coord_http::coord_client() else {
        return SpawnDecision::SpawnDespiteClaimError {
            cause: "no shared coord client".to_string(),
        };
    };
    let body = ContinuationConsumedBody::claim(device_id);
    // coord-tenant-scope(device): only gate_id and device_id are in scope (:2390); the continuation-consumed ack is keyed gate_id+device_id, with no auth extractor and no tenant.
    match crate::auth::attach_device_auth(client.post(&url))
        .timeout(Duration::from_secs(5))
        .json(&body)
        .send()
        .await
    {
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
    let Some(base) = connected_coord_base() else {
        return;
    };
    let url = format!("{base}/coord/gates/{gate_id}/continuation-consumed");
    let Some(client) = crate::coord_http::coord_client() else {
        warn!("agent_runtime: continuation-outcome: no shared coord client gate_id={gate_id}");
        return;
    };
    let body = ContinuationConsumedBody::outcome(device_id, spawned, detail);
    // coord-tenant-scope(device): ContinuationConsumedBody::outcome(device_id, ..) (:2438); same device-keyed route, no session id anywhere in scope.
    match crate::auth::attach_device_auth(client.post(&url))
        .timeout(Duration::from_secs(5))
        .json(&body)
        .send()
        .await
    {
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

/// Minimum interval between deferred-stamp posts for the SAME gate id. The
/// backstop re-lists a deferred row every ~300s; without this limit a 2-day
/// AtCap stall would post ~576 stamps per gate.
const CONTINUATION_DEFERRED_STAMP_INTERVAL: Duration = Duration::from_secs(3600);

/// Per-gate last-posted times for the deferred stamp (in-process rate limit).
fn deferred_stamp_last_post(
) -> &'static std::sync::Mutex<std::collections::HashMap<uuid::Uuid, std::time::Instant>> {
    static LAST: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<uuid::Uuid, std::time::Instant>>,
    > = std::sync::OnceLock::new();
    LAST.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Decide-and-record: should a deferred stamp be posted for `gate_id` at
/// `now`? `true` at most once per [`CONTINUATION_DEFERRED_STAMP_INTERVAL`] per
/// gate (and records `now` as the last post when it says `true`). Takes `now`
/// as a parameter so the rate-limit window is unit-testable.
fn should_post_deferred_stamp(gate_id: uuid::Uuid, now: std::time::Instant) -> bool {
    let mut map = lock_recover(deferred_stamp_last_post(), "deferred_stamp_last_post");
    if let Some(last) = map.get(&gate_id) {
        if now.saturating_duration_since(*last) < CONTINUATION_DEFERRED_STAMP_INTERVAL {
            return false;
        }
    }
    // Opportunistic bound: entries older than the window can never suppress
    // again — drop them once the map is large so it can't grow unboundedly.
    if map.len() > 1024 {
        map.retain(|_, t| now.saturating_duration_since(*t) < CONTINUATION_DEFERRED_STAMP_INTERVAL);
    }
    map.insert(gate_id, now);
    true
}

/// Wire body for `POST /coord/gates/{gate_id}/continuation-deferred`.
#[derive(Debug, Clone, Serialize)]
struct ContinuationDeferredBody {
    device_id: uuid::Uuid,
    reason: String,
}

/// POST the NON-CONSUMING deferred stamp for a locally-skipped continuation
/// (`POST /coord/gates/{gate_id}/continuation-deferred`, body
/// `{device_id, reason}` with reason `at_cap:<cap>` /
/// `duplicate_anchor:<terminal_id>`).
///
/// This replaces the AtCap arm's old `report_spawn_failed` lifecycle post,
/// which polluted the agent-lifecycle channel with fake spawn failures for
/// rows that were merely deferred. The stamp leaves the gate's continuation
/// lifecycle untouched (still pending, still re-deliverable) — it only gives
/// coord an honest, queryable "a runner saw this and deferred it" signal.
/// Rate-limited per gate via [`should_post_deferred_stamp`] (once per hour).
/// **Best-effort**: the route may 404 until coord's parallel phase deploys —
/// ANY non-2xx or transport error is `debug!` and we move on.
async fn post_continuation_deferred(gate_id: uuid::Uuid, device_id: uuid::Uuid, reason: String) {
    let Some(base) = connected_coord_base() else {
        return;
    };
    let url = format!("{base}/coord/gates/{gate_id}/continuation-deferred");
    let Some(client) = crate::coord_http::coord_client() else {
        debug!(
            "agent_runtime: continuation-deferred: no shared coord client \
             gate_id={gate_id} (continuing)"
        );
        return;
    };
    let body = ContinuationDeferredBody { device_id, reason };
    // coord-tenant-scope(device): ContinuationDeferredBody{device_id, reason} (:2529); same unauthenticated device-keyed posture, no tenant column.
    match crate::auth::attach_device_auth(client.post(&url))
        .timeout(Duration::from_secs(5))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            debug!(
                "agent_runtime: continuation-deferred posted gate_id={gate_id} reason={}",
                body.reason
            );
        }
        Ok(resp) => {
            debug!(
                "agent_runtime: continuation-deferred POST gate_id={gate_id} returned {} \
                 (route may not be deployed yet; continuing)",
                resp.status()
            );
        }
        Err(e) => debug!(
            "agent_runtime: continuation-deferred POST gate_id={gate_id} failed \
             (continuing): {e:#}"
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
    let Some(base) = connected_coord_base() else {
        return;
    };
    let url = format!("{base}/coord/agents/unit-dispatches/{dispatch_id}/consumed");
    let Some(client) = crate::coord_http::coord_client() else {
        warn!(
            "agent_runtime: unit-dispatch consume: no shared coord client \
             dispatch_id={dispatch_id}"
        );
        return;
    };
    let body = UnitDispatchConsumedBody { device_id };
    // coord-tenant-scope(device): UnitDispatchConsumedBody{device_id} (:2590); coord marks the dispatch consumed by dispatch_id alone -- no tenant, no auth extractor.
    match crate::auth::attach_device_auth(client.post(&url))
        .timeout(Duration::from_secs(5))
        .json(&body)
        .send()
        .await
    {
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

/// The shared tail of EVERY local-guard deferral in
/// [`run_gate_continuation_inner`]'s step 1: stamp coord with a non-consuming
/// reason, then release the in-process dedupe claim.
///
/// One helper rather than three copies of the same eight lines, because the two
/// invariants it encodes are the ones the 2026-07 delivery-stall incident was
/// caused by getting wrong, and an invariant maintained in three places is
/// maintained in two.
///
/// ## Deliberately NO continuation claim/outcome here (contract item 4)
///
/// Every caller is a LOCAL guard that fires BEFORE step 2's consume claim, so
/// none of them may burn a coord-side claim on a dispatch the local guard
/// rejected. The row stays pending on coord — uncancelled, unconsumed, and
/// therefore re-listable — so it can be re-delivered once the local condition
/// clears (the anchor's session exits, threads free up, a cap slot opens), or be
/// cancelled by the operator / takeover path.
///
/// The old `report_spawn_failed` lifecycle post is GONE and must not come back:
/// it polluted the agent-lifecycle channel with fake spawn failures for rows
/// that were merely deferred. The honest signal is the NON-CONSUMING
/// `continuation-deferred` stamp, rate-limited hourly per gate
/// ([`should_post_deferred_stamp`]); without it coord sees a pending row with a
/// null reason, which is the exact shape that stranded 51 continuations for two
/// days. The route is best-effort — it may 404 against an older coord.
///
/// ## CRITICAL: the release is the delivery-stall root fix
///
/// The dispatcher claimed this id BEFORE the guard ran. Without the release a
/// deferral is PERMANENT: the backstop re-lists the row every tick and the
/// in-process dedupe check drops it forever. That is not a theory — it is the
/// measured failure (the primary's 9 boot-drained slots plus
/// `QONTINUI_CONTINUATION_SESSION_CAP=9` stranded 51 continuations over 2 days).
/// It applies verbatim to the thread-pressure deferral, which is why that arm
/// routes through here rather than growing its own tail.
///
/// `reason` is the machine-matchable stamp string in the established
/// `<class>:<detail>` shape (`duplicate_anchor:<tid>`, `thread_pressure:
/// <severity>:<observed>_over_<limit>`, `at_cap:<cap>`). Callers log their own
/// human-readable line first, at the severity their verdict deserves.
async fn defer_continuation_unclaimed(
    consume_target: ConsumeTarget,
    device_id: uuid::Uuid,
    reason: String,
) {
    if let ConsumeTarget::Gate(gate_id) = consume_target {
        if should_post_deferred_stamp(gate_id, std::time::Instant::now()) {
            post_continuation_deferred(gate_id, device_id, reason).await;
        }
    }
    release_local_dispatch_claim(consume_target);
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
/// 1. Local guards (anchor_key dedup, machine thread pressure, concurrency cap)
///    — run FIRST so a locally-rejected dispatch never burns a coord-side claim
///    (contract item 4), and so a machine already out of threads never pays a
///    `git worktree add` before being told to wait.
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
        // Local skip, row left pending on coord → release the in-process claim
        // (invariant: claimed only while in-flight or after the consume claim).
        release_local_dispatch_claim(consume_target);
        return Ok(());
    }

    // Step 1: the local guards (P3 anchor_key dedup + thread pressure + P4
    // concurrency cap) BEFORE any coord claim or worktree acquire — a dispatch a
    // local guard would reject must NOT burn a coord-side claim (contract item
    // 4), and must not leave a directory behind either: `resource_guard`'s
    // `precheck_spawn` gives the same argument for the same placement, since a
    // refusal issued after `git worktree add` plus a coord claim leaks one of
    // each on EVERY refusal — and a load deferral is, by design, the verdict
    // that repeats.
    //
    // `gate_id` dedup (#450, the `dispatched_gate_ids` set, already applied in
    // the dispatcher) collapses a SAME gate delivered twice; this catches the
    // residual cases it can't — a re-cleared gate (new `gate_id`, same
    // `anchor_key`), a machine out of OS threads, and the count cap. The first
    // and last are evaluated against sessions that are STILL running; liveness
    // is tested against the `TerminalManager`, and with no Tauri runtime the
    // registry is empty so those two lanes are a no-op (the unit-test /
    // headless-only context). The thread lane needs no registry at all — it
    // reads the process — so it is the one guard here that still has an opinion
    // in a headless context.
    match evaluate_continuation_guard_live(
        payload.anchor_key.as_deref(),
        &live_terminal_predicate(),
    ) {
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
            // The ALREADY-LIVE continuation (the one that won the anchor) owns
            // its own claim+outcome; the deduped gate stays pending on coord
            // (its work IS the live session) until cancelled or it expires.
            // Once the anchor's live session exits, a re-delivery of this same
            // gate_id must be able to dispatch — which is what the release
            // inside `defer_continuation_unclaimed` buys.
            defer_continuation_unclaimed(
                consume_target,
                device_id,
                format!("duplicate_anchor:{existing_terminal_id}"),
            )
            .await;
            return Ok(());
        }
        ContinuationGuard::ThreadPressure {
            severity,
            observation,
        } => {
            // The machine is out of THREADS, not out of slots. The deferred
            // continuation stays pending on coord and the periodic backstop poll
            // (`spawn_continuation_backstop_poll`, always armed) re-fetches it
            // within one interval — plus the capacity-freed exit hook fires the
            // moment a live continuation's PTY exits, which is also the moment
            // its ~3 threads go back to the pool. So this deferral self-heals on
            // exactly the event that relieves the pressure.
            //
            // Both the WARN and the CRITICAL verdict land here, unlike
            // `resource_guard::admit_spawn`, which refuses only at CRITICAL —
            // see `evaluate_continuation_guard`'s doc for why a queued dispatch
            // gets back-pressured a band earlier than a human's own terminal.
            warn!(
                "agent_runtime: gate-continuation deferred under machine load: {} \
                 — re-delivered when threads free up (anchor_key={:?})",
                observation.clause(severity),
                payload.anchor_key
            );
            // The stamp reason names the REAL numbers, in the same
            // `<class>:<detail>` shape as `at_cap:` / `duplicate_anchor:` so
            // coord-side grouping still works:
            // `thread_pressure:warn:412_over_256`.
            defer_continuation_unclaimed(
                consume_target,
                device_id,
                format!(
                    "thread_pressure:{severity}:{}_over_{}",
                    observation.observed, observation.limit
                ),
            )
            .await;
            return Ok(());
        }
        ContinuationGuard::AtCap(cap) => {
            // The deferred continuation stays pending on coord; the periodic
            // backstop poll (`spawn_continuation_backstop_poll`, always armed)
            // re-fetches it within one interval even if the capacity-freed
            // exit-hook trigger is missed — no per-process arming flag needed.
            warn!(
                "agent_runtime: gate-continuation refused: deferred: continuation cap ({cap}) \
                 reached — re-delivered when a slot frees (anchor_key={:?})",
                payload.anchor_key
            );
            defer_continuation_unclaimed(consume_target, device_id, format!("at_cap:{cap}")).await;
            return Ok(());
        }
    }

    // Step 1b: agent-registry spawn authorization (plan
    // `2026-07-28-migrate-claude-md-into-qontinui.md` Phase 4c, served clause
    // `agent-spawn-authorization`). Placed with the other LOCAL guards and
    // BEFORE step 2's coord claim, for the same reason they are: a dispatch
    // this gate refuses must NOT burn a coord-side claim (contract item 4). If
    // it ran after the claim, a refusal would consume the gate row at coord,
    // leave it un-re-listable, and strand the work permanently the moment the
    // user opts in.
    //
    // `dispatch_gate_continuation` already checked at the delivery seam; this
    // is the backstop for any re-entry that reaches the inner run directly.
    // Both read the same TTL cache, so the second check costs no round-trip.
    // On refusal: release the in-process claim and post NO lifecycle
    // spawn-failure — an authorization refusal is a standing decision, not a
    // spawn fault, and the `at_cap` precedent above is explicit that fake
    // spawn failures pollute the lifecycle channel.
    let authz = crate::agent_authorization::authorize_spawn(
        None,
        crate::agent_authorization::SpawnPath::StandingContinuation,
    )
    .await;
    if !authz.allows_spawn() {
        warn!(
            "agent_runtime: gate-continuation refused by the agent registry ({}): {} \
             (anchor_key={:?})",
            authz.label(),
            authz.reason().unwrap_or("no reason recorded"),
            payload.anchor_key
        );
        release_local_dispatch_claim(consume_target);
        return Ok(());
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
                // Deliberately NOT released: coord answered the consume claim
                // with 409 cancelled, so the row is TERMINAL there (never
                // re-listed). Keeping the id claimed cheaply absorbs any
                // in-flight duplicate delivery of the same cancelled gate.
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
pub(crate) const CONTINUATION_PAGE_ZONE_CEILING: usize = 9;

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
pub(crate) fn pick_continuation_page(
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
// (pub(crate): also composed by `looping_agent_supervisor` — the Tier-0
// looping-agent spawn reuses this exact argv recipe.)
pub(crate) fn build_continuation_claude_command(
    claude_bin: String,
    pinned_session_id: &str,
    add_dir_args: Vec<String>,
    prompt: String,
    system_prompt: Option<String>,
    // The `--settings <path>` pair delivering the runner's bundled Claude hook
    // block, from
    // [`crate::session::claude_hook::direct_spawn_settings_args`]. Empty ⇒ no
    // hooks for this session (the fail-open arm, and what tests pass).
    //
    // Passed in rather than resolved here ON PURPOSE: it makes every call site
    // state whether its spawn is a direct exec that needs the flag spelled out,
    // and it keeps this builder pure so the argv-shape regressions below assert
    // against a fixed vector instead of whatever is on the machine's disk.
    hook_settings_args: Vec<String>,
    launch_cfg: &crate::claude_session::launch_spec::LaunchConfig,
) -> Vec<String> {
    use crate::claude_session::launch_spec::{render_argv, LaunchSpec, PermissionMode};

    // `extra_required` is the caller-authoritative, verbatim tail — never
    // reordered or deduped by the seam. It carries, IN ORDER:
    //
    //  - the canonical runner-context briefing as `--append-system-prompt <sp>`.
    //    Autonomous spawns exec `claude` directly (no shell wrapping), so unlike
    //    interactive panes they never pick up the shell-integration wrapper's
    //    briefing; injecting it here gives fleet/gate-continuation sessions the
    //    same capability + guardrail context an operator's pane gets.
    //  - the `--settings <hook file>` pair, for the SAME reason and from the
    //    same blind spot: the identity shim is what appends it for a hand-typed
    //    `claude`, and a direct exec has no shim in the chain, so an autonomous
    //    session got NO `SessionStart`/`PreCompact`/`Stop` hook at all —
    //    including the `SessionStart` policy injection that
    //    `QONTINUI_POLICY_INJECTION` now defaults to ON. One value, not
    //    variadic, so unlike `--add-dir` it cannot swallow the trailing prompt;
    //    it still sits ahead of the `--` with everything else.
    //  - the attached-form `--add-dir=<sibling>` tokens.
    //  - the `--` end-of-options terminator immediately before the trailing
    //    positional prompt.
    //
    // Everything sits BEFORE the `--`, so the trailing-positional-prompt
    // discipline the regression tests below guard is preserved. `spec.permission`
    // (DangerouslySkip) and `spec.session_id` (the pinned id) are emitted ahead
    // of this tail by the seam, and the operator's launch template layers in
    // between — with no operator config the output is byte-identical to the
    // historical hand-built argv.
    let mut extra_required = Vec::with_capacity(add_dir_args.len() + hook_settings_args.len() + 4);
    if let Some(sp) = system_prompt {
        extra_required.push("--append-system-prompt".to_string());
        extra_required.push(sp);
    }
    extra_required.extend(hook_settings_args);
    extra_required.extend(add_dir_args);
    extra_required.push("--".to_string());
    extra_required.push(prompt);

    let spec = LaunchSpec {
        permission: PermissionMode::DangerouslySkip,
        session_id: Some(pinned_session_id.to_string()),
        extra_required,
        ..Default::default()
    };
    render_argv(&spec, launch_cfg, &claude_bin)
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

    // NOTE: agent-registry spawn authorization for this path is enforced in
    // `run_gate_continuation_inner` (step 1b), alongside the other local
    // guards and BEFORE the coord consume claim. It deliberately does NOT run
    // here: by this point the claim has been taken, so a refusal would consume
    // the gate row at coord and strand the work. Do not add a check here.

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

    // Resolve `claude` to an ABSOLUTE launchable path, same as the
    // condition-check terminal and for the same reason: this spawns via the
    // identical direct-PTY/CreateProcessW backend, so a bare "claude" would
    // resolve to the extensionless identity-shim script and fail with os
    // error 193 (see `resolve_claude_bin` doc comment). The prompt is the
    // trailing positional arg — interactive form, NOT `--print` (see the fn
    // doc: interactivity is required). Inject `--dangerously-skip-permissions`
    // (same as the worker-tab spawn path) so the continuation /implement-plan
    // session does not stall on interactive Bash permission prompts — an
    // unattended gate continuation has no operator to answer them.
    let claude_bin = tokio::task::spawn_blocking(resolve_claude_bin)
        .await
        .unwrap_or_else(|_| claude_bin_path());
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

    // Account selection: pin the most-available (token-bearing) account so the
    // continuation does not spawn under a quota-exhausted default and die
    // instantly (the bug: continuations spawned under the runner's boot account
    // even when it was out of tokens). The resolved dir is threaded to the PTY
    // as `CLAUDE_CONFIG_DIR` via `capture_hint.config_dir` (consumed by
    // `create_terminal_session_backend`) AND resolves the per-account operator
    // launch template for the shared launch seam below. Resolved BEFORE the argv
    // build so its per-account command can layer in. spawn_blocking: the
    // selector reads settings + cooldown state.
    let _ = tokio::task::spawn_blocking(crate::ai_provider::pick_best_account).await;
    let (selected_config_dir, _config_dir_source) = {
        let ai = crate::settings::get_ai_settings();
        crate::ai_provider::get_effective_config_dir(&ai.claude_cli)
    };

    // Compose the spawn argv through the shared launch seam so the operator's
    // global + per-account launch flags layer onto the required autonomous
    // flags (see build_continuation_claude_command). With no operator config
    // the argv is byte-identical to the historical hand-built vector.
    let launch_cfg = crate::claude_session::launch_spec::LaunchConfig::from_settings(
        selected_config_dir.as_deref(),
    );
    let command = Some(build_continuation_claude_command(
        claude_bin,
        &pinned_session_id,
        add_dir_args,
        payload.initial_prompt.clone(),
        Some(crate::terminal::runner_context(
            crate::terminal::spawn_seam_api_port(),
        )),
        // Direct exec — no identity shim in the chain to append `--settings`,
        // so the hook carrier has to be spelled out here or this session runs
        // with no SessionStart/PreCompact/Stop hook at all.
        crate::session::claude_hook::direct_spawn_settings_args(),
        &launch_cfg,
    ));

    // First repo (if any) is the session's intent_repo for coord attribution.
    let intent_repo = payload.repos.first().cloned();

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

    let capture_hint = crate::commands::terminal::SessionCaptureHint {
        config_dir: selected_config_dir,
        working_dir: workdir.to_string(),
        title: title.clone(),
        page_id: Some(target_page.clone()),
        // Matches the `--session-id` in the spawn argv → synchronous record.
        claude_session_id: Some(pinned_session_id),
        zone_index: None,
        // Autonomous gate continuation → pin the agent git identity on the PTY.
        inject_agent_git_identity: true,
        // A gate continuation is NEW work, not the continuation of a coord
        // session row — no lineage claim.
        coord_lineage: None,
    };

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
    // Same for the fleet SKILLS (.claude/skills/<name>/SKILL.md) — a device with
    // no qontinui-claude-config checkout has no skills dir at all.
    crate::fleet_skills::provision_fleet_skills_for_session(workdir);

    let result = crate::commands::terminal::create_tracked_terminal_session_backend(
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
        // UNATTENDED spawn — resource_override: false, i.e. this path RESPECTS
        // the critical floor. A gate continuation is brand-new autonomous work
        // with nobody at the keyboard to answer a dialog, and it is exactly the
        // class of spawn that piled `claude` + `rustc` onto an already-starved
        // box on the night of the incident. Refusing here is NOT a silent
        // forever-refusal: the `Err` arm below calls `report_spawn_failed`, so
        // coord learns the continuation did not start and can re-dispatch it
        // once the box breathes — the same defer-don't-reject posture
        // `ci_node::admission` takes for CI work.
        false,
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

/// Dispatch a condition-check spawn frame (coord `source: "condition_check"`).
///
/// Parallel to [`dispatch_gate_continuation`] but deliberately simpler: a
/// condition check has NO gate/dispatch id, so there is no in-process dedupe,
/// no coord consume claim, and no outcome ack — the live WS publish plus the
/// visible terminal spawn IS the whole contract. Spawns the terminal on a
/// detached task so a slow spawn never blocks the WS pump.
fn dispatch_condition_check(payload: ConditionCheckPayload, device_id: uuid::Uuid) {
    info!(
        "agent_runtime: condition-check received run_id={} target_url={} presentation={:?} \
         target_device_id={:?}",
        payload.run_id, payload.target_url, payload.presentation, payload.target_device_id,
    );
    let run_id = payload.run_id.clone();
    tokio::spawn(async move {
        if let Err(e) = run_condition_check_terminal(payload, device_id).await {
            error!("agent_runtime: run_condition_check_terminal (run_id={run_id}) failed: {e:#}");
        }
    });
}

/// Run a condition check as a VISIBLE terminal session.
///
/// Mirrors [`run_continuation_terminal`] — opens a docked, operator-visible
/// terminal whose PTY child IS the `claude` CLI launched with the condition
/// prompt as a positional argv (interactive, NOT `--print`) — but drops the
/// gate-specific machinery a condition check does not need:
///
/// - **No worktree isolation.** A condition check drives the UI Bridge and curls
///   a report back; it does not edit code, so it runs from `QONTINUI_ROOT` with
///   no `IsolatedEditContext` / `--add-dir` and no `.mcp.json`/fleet-command
///   provisioning (which would write into the shared canonical checkout).
/// - **No coord ack.** There is no gate/dispatch id, so no consume claim and no
///   outcome POST — the WS publish + spawn is the contract.
/// - **Host git identity.** No autonomous-agent git author is injected (no
///   commits happen), so the PTY keeps the ambient host identity.
///
/// The account-selection fail-loud posture IS kept from the gate path: without a
/// credential-valid account the spawned `claude` would die instantly with a 401,
/// so we abort with an actionable reason instead of leaving a dead pane.
async fn run_condition_check_terminal(
    payload: ConditionCheckPayload,
    device_id: uuid::Uuid,
) -> anyhow::Result<()> {
    use std::sync::Arc;

    // Defensive device targeting. Coord's WS pattern filter is already
    // device-scoped, so this is usually redundant; honor an explicit mismatch.
    // An absent or unparseable id proceeds (the WS filter already gated delivery).
    if let Some(target) = payload.target_device_id.as_deref() {
        match uuid::Uuid::parse_str(target) {
            Ok(t) if t != device_id => {
                debug!(
                    "agent_runtime: condition-check target_device_id={t} != local {device_id}; \
                     ignoring"
                );
                return Ok(());
            }
            Ok(_) => {}
            Err(_) => debug!(
                "agent_runtime: condition-check target_device_id={target} unparseable; proceeding \
                 (device-scoped WS filter already gated delivery)"
            ),
        }
    }

    // Agent-registry spawn authorization (plan
    // `2026-07-28-migrate-claude-md-into-qontinui.md` Phase 4c, served clause
    // `agent-spawn-authorization`). A coord-dispatched condition check spawns
    // an AI terminal that outlives the request that scheduled it — the same
    // auto-dispatch class as a gate continuation, so it takes the same
    // standing per-path opt-in.
    let authz = crate::agent_authorization::authorize_spawn(
        None,
        crate::agent_authorization::SpawnPath::StandingContinuation,
    )
    .await;
    if !authz.allows_spawn() {
        warn!(
            "agent_runtime: condition-check run_id={} NOT spawned — {}: {}",
            payload.run_id,
            authz.label(),
            authz.reason().unwrap_or("no reason recorded")
        );
        return Ok(());
    }

    // A condition check is inherently operator-visible; there is no headless
    // variant. Log if coord ever asks for headless, then spawn a terminal anyway.
    if payload.presentation != Presentation::Terminal {
        debug!(
            "agent_runtime: condition-check requested presentation={:?}; a condition check has no \
             headless surface — spawning a visible terminal",
            payload.presentation
        );
    }

    // Reach the managed Tauri state via the process-global AppHandle. No webview
    // runtime (headless/unit-test) → cannot open a visible terminal.
    let app = match crate::tauri_app_handle::current() {
        Some(a) => a,
        None => {
            let reason = "no Tauri AppHandle (runner has no webview runtime) — \
                          cannot open a visible condition-check terminal";
            warn!("agent_runtime: condition-check: {reason}");
            return Err(anyhow::anyhow!(reason));
        }
    };

    use tauri::Manager;
    let terminal_manager = match app.try_state::<Arc<crate::terminal::TerminalManager>>() {
        Some(s) => s.inner().clone(),
        None => {
            let reason = "TerminalManager state not managed — cannot create terminal session";
            warn!("agent_runtime: condition-check: {reason}");
            return Err(anyhow::anyhow!(reason));
        }
    };
    let session_registry = match app.try_state::<Arc<crate::session::SessionRegistry>>() {
        Some(s) => s.inner().clone(),
        None => {
            let reason = "SessionRegistry state not managed — cannot register terminal session";
            warn!("agent_runtime: condition-check: {reason}");
            return Err(anyhow::anyhow!(reason));
        }
    };

    // Title from a short run-id prefix: "Condition check <8 chars>".
    let run_id_short: String = payload.run_id.chars().take(8).collect();
    let title = format!("Condition check {run_id_short}");

    // A condition check does not edit code, so no worktree isolation — run from
    // QONTINUI_ROOT. We intentionally do NOT provision `.mcp.json`/fleet commands
    // here (the gate path writes those into its per-continuation worktree; doing
    // so against the shared canonical root would clobber the operator's files).
    let workdir = qontinui_root_dir()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("condition-check: no QONTINUI_ROOT resolved"))?;

    // Resolve `claude` to an ABSOLUTE launchable path (not the bare name a
    // shell-wrapped spawn could get away with): this terminal spawns
    // `claude` directly via CreateProcessW, so a bare "claude" would resolve
    // to the extensionless identity-shim script and fail with os error 193.
    // Pinned session id + interactive positional-prompt argv (no `--add-dir`:
    // no sibling worktrees). spawn_blocking: resolve_claude_bin does blocking
    // PATH filesystem stats.
    let claude_bin = tokio::task::spawn_blocking(resolve_claude_bin)
        .await
        .unwrap_or_else(|_| claude_bin_path());
    let pinned_session_id = uuid::Uuid::new_v4().to_string();

    // Account selection (fail-loud) — identical posture to the gate path so the
    // spawned `claude` never dies instantly under a quota-exhausted default.
    // Resolved BEFORE the argv build so its per-account launch command can layer
    // into the shared launch seam.
    let _ = tokio::task::spawn_blocking(crate::ai_provider::pick_best_account).await;
    let (selected_config_dir, _config_dir_source) = {
        let ai = crate::settings::get_ai_settings();
        crate::ai_provider::get_effective_config_dir(&ai.claude_cli)
    };
    let launch_cfg = crate::claude_session::launch_spec::LaunchConfig::from_settings(
        selected_config_dir.as_deref(),
    );
    let command = Some(build_continuation_claude_command(
        claude_bin,
        &pinned_session_id,
        Vec::new(),
        payload.initial_prompt.clone(),
        Some(crate::terminal::runner_context(
            crate::terminal::spawn_seam_api_port(),
        )),
        // Direct exec — no identity shim in the chain to append `--settings`,
        // so the hook carrier has to be spelled out here or this session runs
        // with no SessionStart/PreCompact/Stop hook at all.
        crate::session::claude_hook::direct_spawn_settings_args(),
        &launch_cfg,
    ));

    if selected_config_dir.is_none()
        && !crate::ai_provider::oauth_refresh::default_location_has_valid_credentials()
    {
        let instance = crate::instance::instance_name().unwrap_or_else(|| "primary".to_string());
        let reason = format!(
            "no authenticated Claude account on this runner — run /login (instance={instance})"
        );
        warn!("agent_runtime: condition-check aborted — {reason}");
        return Err(anyhow::anyhow!(reason));
    }

    // Spread across non-full pages (same picker + ceiling as the gate path).
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

    let capture_hint = crate::commands::terminal::SessionCaptureHint {
        config_dir: selected_config_dir,
        working_dir: workdir.clone(),
        title: title.clone(),
        page_id: Some(target_page.clone()),
        // Matches the `--session-id` in the spawn argv → synchronous record.
        claude_session_id: Some(pinned_session_id),
        zone_index: None,
        // A condition check commits nothing → keep the ambient host git identity.
        inject_agent_git_identity: false,
        coord_lineage: None,
    };

    let result = crate::commands::terminal::create_tracked_terminal_session_backend(
        &terminal_manager,
        &session_registry,
        app.clone(),
        title,
        workdir,
        None, // work_unit_slug — a condition check is not work-unit-scoped
        None, // correlation_topic
        None, // intent_repo — no repo edited
        command,
        None, // isolated_ctx — no worktree acquired
        capture_hint,
        Some(target_page),
        // UNATTENDED spawn — respect the critical floor (see the gate-
        // continuation site above for the full rationale). A condition check is
        // the *cheapest* thing to drop when the box is out of commit: it edits
        // nothing, commits nothing, and coord re-issues the check on its next
        // pass, so a refusal here costs one deferred probe rather than a lost
        // session.
        false,
    );

    match result {
        Ok((terminal_id, coord_session_id)) => {
            info!(
                "agent_runtime: condition-check terminal session created terminal_id={terminal_id} \
                 coord_session={coord_session_id:?} run_id={}",
                payload.run_id
            );
            // Surface it: switch the main view to the Terminal panel + select the
            // tab (scoped to the MAIN window, same as the gate path).
            emit_terminal_focus_request(&app, &terminal_id);
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!(
            "condition-check terminal session create failed: {e}"
        )),
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

/// The `bound_port` argument the headless gate continuation hands to coord-mcp
/// provisioning.
///
/// Exists as its own function (with an INJECTED resolver) because the defect it
/// closes was a hardcoded `None` at the call site: the headless path passed
/// `None` unconditionally, so the device arm of `provision_coord_mcp_with_jwt`
/// hit its unresolvable-port refusal every time and that path never provisioned
/// anything. A literal at a call site is untestable; a resolver seam is not.
fn headless_continuation_bound_port(resolve: impl FnOnce() -> Option<u16>) -> Option<u16> {
    let port = resolve();
    match port {
        Some(p) => info!(
            "agent_runtime: headless gate-continuation: resolved bound API port {p} \
             for coord-mcp provisioning"
        ),
        None => warn!(
            "agent_runtime: headless gate-continuation: bound API port is \
             unresolvable (no managed AppState) — coord-mcp provisioning will \
             fail closed; the session gets a degraded breadcrumb, not a config \
             pointing at a dead port"
        ),
    }
    port
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
    // parity with the terminal continuation path.
    //
    // The bearer is the runner's DEVICE JWT, so provisioning takes the device
    // arm and writes the loopback PROXY shape, which needs the ACTUALLY-BOUND
    // API port. Resolve it from managed Tauri state exactly as the terminal
    // continuation does; when no `AppState` is reachable this stays `None` and
    // provisioning fails closed (refuses the write + drops a degraded
    // breadcrumb) rather than emitting a config pointing at the bootstrap
    // default `:9876`, which is dead on any secondary/temp runner.
    let bound_port = headless_continuation_bound_port(crate::coord_mcp::resolve_bound_api_port);
    crate::coord_mcp::provision_coord_mcp_for_session(workdir, bound_port);
    // No per-spawn pin here: a gate continuation carries no account field —
    // the `pick_best_account` call above is the whole selection.
    match spawn_claude_child(workdir, initial_prompt, None).await {
        Ok(mut child) => {
            let pid = child.id().map(|p| p as i64);
            // Exempt this headless child from the session-tracking health
            // check for its lifetime — it legitimately has no lifecycle
            // record (no PTY, no capture_hint).
            let health_pid = child.id();
            if let Some(p) = health_pid {
                crate::session::tracking_health::register_headless_claude_pid(p);
            }
            report_spawn_complete(agent_id, pid, Some("gate continuation"), None).await;
            let exit = pump_subprocess(agent_id, &mut child, log_path.as_deref()).await;
            if let Some(p) = health_pid {
                crate::session::tracking_health::unregister_headless_claude_pid(p);
            }
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

/// Resolve a spawn's optional [`LaunchPayload::account`] pin into the
/// `CLAUDE_CONFIG_DIR` override the child will run under.
///
/// Pure core, parameterised over the resolver so the FAIL-LOUD contract is
/// unit-testable without the roster / credential / cooldown singletons
/// (mirrors `ai_provider::account_usage::resolve_from`).
///
/// Three outcomes, and the middle one is the whole point:
/// - no pin (`None`, or a blank string — which names no account at all, so it
///   is absence rather than a wrong name) ⇒ `Ok(None)`, today's least-usage
///   rotation runs unchanged;
/// - a pin that does not resolve — off-roster name, or a roster account with no
///   live credentials ⇒ **`Err`**, which the caller turns into a
///   `report_spawn_failed` lifecycle post. It NEVER falls back to rotation: a
///   pinned account that is silently ignored is indistinguishable from one that
///   was honoured, so the operator would have no way to tell which account
///   actually ran;
/// - a pin that resolves ⇒ `Ok(Some(resolved))`, pinned for this child only.
///
/// A rate-limited-but-valid account still resolves (the caller asked for it
/// explicitly); the cooldown rides along on
/// [`crate::ai_provider::ResolvedAccount::cooldown_remaining_secs`] and the
/// caller warns.
pub(crate) fn resolve_spawn_account_with(
    account: Option<&str>,
    resolve: impl FnOnce(
        &str,
    ) -> Result<
        crate::ai_provider::ResolvedAccount,
        crate::ai_provider::AccountSelectError,
    >,
) -> anyhow::Result<Option<crate::ai_provider::ResolvedAccount>> {
    let Some(requested) = account.map(str::trim).filter(|a| !a.is_empty()) else {
        return Ok(None);
    };
    resolve(requested).map(Some).map_err(|e| {
        anyhow::anyhow!(
            "spawn requested account '{requested}' but it cannot be used: {} \
             — refusing to fall back to account rotation (an ignored pin is \
             indistinguishable from an honoured one)",
            e.message()
        )
    })
}

/// Production wiring of [`resolve_spawn_account_with`] against the live roster.
///
/// The ONE seam a caller pinning an account must go through — never
/// [`crate::ai_provider::pick_best_account`], which is a side-effect-only
/// ROTATION helper (it returns `()`, no-ops unless `account_selection_mode ==
/// LeastUsage`, and every call site discards it), and never
/// `switch_claude_account`, which would leak one spawn's account choice into
/// every later spawn on this runner.
pub(crate) fn resolve_spawn_account(
    account: Option<&str>,
) -> anyhow::Result<Option<crate::ai_provider::ResolvedAccount>> {
    resolve_spawn_account_with(account, crate::ai_provider::resolve_requested_account)
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

    // Step 0: resolve the optional per-spawn account pin — BEFORE materializing
    // anything, so an unusable pin costs no worktrees. An unresolvable pin is a
    // terminal spawn failure reported to coord, never a quiet demotion to
    // least-usage rotation (see `resolve_spawn_account_with`).
    let pinned_account = match resolve_spawn_account(payload.account.as_deref()) {
        Ok(a) => a,
        Err(e) => {
            let reason = format!("{e:#}");
            warn!("agent_runtime: agent_id={agent_id} NOT launched — {reason}");
            report_spawn_failed(agent_id, &reason, None, 0, primary_push_ref.as_deref()).await;
            return Err(e);
        }
    };
    if let Some(acct) = &pinned_account {
        info!(
            "agent_runtime: agent_id={agent_id} pinned to Claude account '{}' \
             (config_dir override — account_selection_mode does not apply to this spawn)",
            acct.account_name
        );
        if let Some(secs) = acct.cooldown_remaining_secs {
            warn!(
                "agent_runtime: pinned account '{}' is rate-limited for another {secs}s; \
                 spawning anyway per the explicit pin",
                acct.account_name
            );
        }
    }
    let pinned_config_dir = pinned_account.as_ref().map(|a| a.config_dir.clone());

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
            // The REAL JWT is untouched (still ~4h valid); only the bookkeeping
            // `exp` is clamped here. In debug / `test-fixtures` builds a test can
            // compress it via QONTINUI_AGENT_JWT_EXP_COMPRESS_SECS so the refresh
            // boundary fires in seconds. In release this is always `payload.jwt_exp`.
            exp: compressed_jwt_exp(payload.jwt_exp),
            ..Default::default()
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
            if let Some(base) = connected_coord_base() {
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
    // Same for the fleet SKILLS. Note provision_agent_definitions above still
    // COPIES .claude/agents from a claude-config checkout, so agents remain
    // absent on a device without one; skills no longer do.
    crate::fleet_skills::provision_fleet_skills_for_session(&primary_wt);

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
    //
    // SKIPPED when this spawn carries an account pin: the picker mutates the
    // process-global resolved dir, which is precisely the leak a per-spawn
    // override exists to avoid — and its result would be ignored anyway, since
    // the override is passed to every (re)spawn below.
    if pinned_config_dir.is_none() {
        let _ = tokio::task::spawn_blocking(crate::ai_provider::pick_best_account).await;
    }

    loop {
        // Stop requested during a restart back-off (or before the first spawn):
        // bail without (re)spawning.
        if stop.is_cancelled() {
            final_reason = Some("stopped by operator before (re)spawn".to_string());
            break;
        }
        match spawn_claude_child(
            &primary_wt,
            &payload.initial_prompt,
            pinned_config_dir.as_deref(),
        )
        .await
        {
            Ok(mut child) => {
                let pid = child.id().map(|p| p as i64);
                // Exempt this headless child from the session-tracking health
                // check for its lifetime — WS agent spawns legitimately have
                // no lifecycle record (no PTY, no capture_hint BY DESIGN).
                let health_pid = child.id();
                if let Some(p) = health_pid {
                    crate::session::tracking_health::register_headless_claude_pid(p);
                }
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
                        if let Some(p) = health_pid {
                            crate::session::tracking_health::unregister_headless_claude_pid(p);
                        }
                        final_reason =
                            Some("stopped by operator (events.agent.stop_requested)".to_string());
                        break;
                    }
                };
                if let Some(p) = health_pid {
                    crate::session::tracking_health::unregister_headless_claude_pid(p);
                }
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
        let out = crate::process_helpers::tokio_no_window("git")
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
    let dst_dir = Path::new(worktree_cwd).join(".claude").join("agents");

    // FLOOR FIRST: write the defs bundled into this binary, so a device with no
    // qontinui-claude-config checkout gets a working subagent set instead of
    // none. This is the fleet-portability follow-up this function's docstring
    // has named since it was written; see `crate::fleet_agents`.
    //
    // Deliberately NOT fatal: if the embedded write fails we warn and continue
    // to the checkout overlay, because a checkout present on this device is a
    // complete answer on its own.
    let embedded = match crate::fleet_agents::provision_fleet_agents_into(&dst_dir) {
        Ok(n) => n,
        Err(e) => {
            warn!(
                "agent_runtime: embedded agent-def write into {} failed; using checkout only: {e}",
                dst_dir.display()
            );
            0
        }
    };

    // CHECKOUT WINS: the operator's live copies are overlaid on top below, so
    // editing qontinui-claude-config/.claude/agents behaves exactly as before.
    if !src_dir.is_dir() {
        warn!(
            "agent_runtime: no claude-config agents dir at {}; keeping the \
             {embedded} embedded subagent def(s) already provisioned into {}",
            src_dir.display(),
            dst_dir.display()
        );
        return Ok(());
    }
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
        "agent_runtime: overlaid {copied} checkout subagent def(s) onto {embedded} embedded default(s) in {}",
        dst_dir.display()
    );
    Ok(())
}

/// The workspace root holding the runner's canonical checkouts.
///
/// One of the four byte-similar copies collapsed in Phase 2 of
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`; its
/// `D:/qontinui-root` Windows arm shipped the author's machine layout inside an
/// open-source binary. [`crate::workspace_paths`] is now the single door.
pub(crate) fn qontinui_root_dir() -> Option<PathBuf> {
    crate::workspace_paths::workspace_root()
}

/// The local primary-checkout directory NAME for a coord repo slug. Coord uses
/// `owner/name` slugs (e.g. `qontinui/qontinui-runner`); the runner's primary
/// checkouts live at `<QONTINUI_ROOT>/<name>`. A bare name passes through.
pub(crate) fn local_repo_name(repo: &str) -> &str {
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
        crate::process_helpers::no_window("git")
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

/// The final env mutations applied to a headless `claude -p` child before it
/// is spawned: the runner-context marker + API port, then the credential scrub.
///
/// **Runner-context marker + API port — PTY parity for the HEADLESS seam.**
/// `QONTINUI_RUNNER_CONTEXT` is the fleet's canonical "am I inside the runner?"
/// predicate, but before plan
/// `2026-08-07-runner-context-visibility-and-session-env-secret-hygiene` only
/// the PTY path set it (`terminal/session.rs`). A headless `claude -p` worker
/// therefore read the predicate as EMPTY while running inside the runner — a
/// false negative that made every consumer of it wrong on this path.
///
/// Why an env var rather than a probe: identity must be answerable without a
/// `/health` round trip (the port may be busy, wedged, or slow — a doomed IPv6
/// connect alone has been measured at ~2s on this fleet), and the marker
/// survives a runner restart because the child keeps the env it was spawned
/// with.
///
/// Rendered from `crate::terminal::runner_context` — the SINGLE source of
/// truth, the same call the three PTY callers make (`agent_runtime.rs`
/// continuation + fleet spawns, `looping_agent_supervisor.rs`) — so the
/// headless and PTY seams cannot drift. See the contract docs on that function
/// (`terminal/mod.rs`) for the briefing's attributable-source marker and its
/// fleet-gated clause.
///
/// Note this seam only EXPORTS the briefing; a direct-exec `claude` sources no
/// shell integration, so whether it also reaches the model depends on the
/// caller's `--append-system-prompt` argv (see
/// `build_continuation_claude_command`). The env var is what makes the
/// predicate answerable either way.
///
/// **The credential scrub is last.** Same control as the PTY seam in
/// `terminal/session.rs`, single-sourced name list — see
/// `crate::terminal::CREDENTIAL_VALUE_ENV_VARS` for why the runner is the
/// chokepoint and why `JWT|KEY|TOKEN|SECRET` redaction misses these. Nothing in
/// [`spawn_claude_child`] sets env after this call, so the strip is
/// last-write-wins by construction.
///
/// Extracted from [`spawn_claude_child`] because that function spawns a real
/// process and cannot run in a unit test — with the scrub inlined there,
/// deleting it reddened nothing.
pub(crate) fn finalize_headless_child_env(cmd: &mut tokio::process::Command) {
    // Resolved HERE, not passed in. The port is the one value this seam ships
    // that a call site can get wrong invisibly, and `spawn_claude_child` — the
    // only production caller — did: it passed `mcp::types::get_mcp_api_port()`,
    // the bootstrap default, while `run_continuation_headless` one frame up
    // resolved the ACTUALLY-BOUND port for the same child's `.mcp.json`. A
    // parameter is exactly what made that divergence untestable, and it is the
    // same reason the rest of this function was extracted in the first place.
    // The PTY seam has always resolved its own; now neither can drift.
    let runner_api_port = crate::terminal::spawn_seam_api_port();
    cmd.env(
        "QONTINUI_RUNNER_CONTEXT",
        crate::terminal::runner_context(runner_api_port),
    );
    cmd.env("QONTINUI_RUNNER_API_PORT", runner_api_port.to_string());

    crate::terminal::scrub_credential_env_tokio(cmd);
}

/// Spawn `claude` CLI as a tokio child. `initial_prompt` is piped to
/// stdin. stdout/stderr are inherited as pipes so the caller can stream
/// them.
async fn spawn_claude_child(
    workdir: &str,
    initial_prompt: &str,
    account_config_dir_override: Option<&str>,
) -> anyhow::Result<Child> {
    let bin = claude_bin_path();
    let mut cmd = crate::process_helpers::tokio_no_window(&bin);
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
    //
    // `account_config_dir_override` is the caller's per-spawn PIN
    // (`LaunchPayload::account`, already validated against the roster + its
    // credentials by `resolve_spawn_account`). When present it wins over
    // `account_selection_mode` entirely, for this child only — nothing global
    // is mutated, so a sibling session on the same box is unaffected.
    let ai = crate::settings::get_ai_settings();
    let (resolved_config_dir, _config_dir_source) =
        crate::ai_provider::get_effective_config_dir_with_override(
            &ai.claude_cli,
            account_config_dir_override,
        );
    match resolved_config_dir {
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
    // P7 — non-interactive git credential posture (plan Phase 6). AllHosts scope:
    // an autonomous agent has no human to answer a credential UI, so ANY host's
    // GUI/terminal prompt is an infinite hang — GCM_INTERACTIVE=never +
    // GIT_TERMINAL_PROMPT=0 make every host fail cleanly non-interactively, and a
    // github.com `gh auth git-credential` fallback covers GitHub. Registered-repo
    // pushes still use the per-session `--local` coord helper (higher
    // precedence). See `credential_helper::non_interactive_git_env`.
    for (k, v) in crate::credential_helper::non_interactive_git_env(
        crate::credential_helper::GitCredentialScope::AllHosts,
    ) {
        cmd.env(k, v);
    }
    // Runner-context marker + API port, then the credential scrub — the LAST
    // env mutations before the spawn. Extracted so the production call site is
    // unit-testable; see the function's doc comment.
    finalize_headless_child_env(&mut cmd);
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
    let Some(base) = connected_coord_base() else {
        return false;
    };
    let url = format!("{base}/agents/{agent_id}/log");
    let Some(client) = crate::coord_http::coord_client() else {
        return false;
    };
    // coord-tenant-scope(session-owed): agent_id is the fn's first parameter (:4799), but coord resolves the tenant from coord.agent_worktrees by that agent_id and never reads the bearer -- correctness is downstream of allocate. Phase 5.
    match crate::auth::attach_device_auth(client.post(&url))
        .timeout(Duration::from_secs(3))
        .json(line)
        .send()
        .await
    {
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
            if let Some(base) = connected_coord_base() {
                let _ =
                    crate::agent_token::maybe_refresh(&slot, &base, payload.agent_id, "agent_mcp")
                        .await;
            }
        }
    }
}

async fn heartbeat_once(payload: &LaunchPayload) -> anyhow::Result<()> {
    let base = connected_coord_base().ok_or_else(|| anyhow::anyhow!("no coord_url"))?;
    let body = ClaimHeartbeat {
        kind: "phase".to_string(),
        resource_key: payload.claim_token.clone(),
        machine_id: payload.target_device_id.to_string(),
        ttl_seconds: 3600,
    };
    let client = crate::coord_http::coord_client()
        .ok_or_else(|| anyhow::anyhow!("no shared coord client"))?;
    // coord-tenant-scope(session-owed): payload.agent_session_id (:77) and payload.agent_id (:75) are in scope, yet the claim body (:4849-4854) sends neither, nor ClaimRequest.tenant_id. Phase 5.
    let resp = crate::auth::attach_device_auth(client.post(format!("{base}/claims/heartbeat")))
        .timeout(Duration::from_secs(5))
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
    let Some(base) = connected_coord_base() else {
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
    let Some(client) = crate::coord_http::coord_client() else {
        return;
    };
    // coord-tenant-scope(session-owed): agent_id is a parameter (:4872); spawn-complete only flips agent_worktrees.status by agent_id and persists no tenant, so no bearer is read. Phase 5.
    match crate::auth::attach_device_auth(client.post(&url))
        .timeout(Duration::from_secs(5))
        .json(&body)
        .send()
        .await
    {
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
    let Some(base) = connected_coord_base() else {
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
    let Some(client) = crate::coord_http::coord_client() else {
        return;
    };
    // coord-tenant-scope(session-owed): agent_id is a parameter; spawn-failed likewise only sets status=abandoned by agent_id and persists no tenant. Phase 5.
    match crate::auth::attach_device_auth(client.post(&url))
        .timeout(Duration::from_secs(5))
        .json(&body)
        .send()
        .await
    {
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

    use crate::test_env::env_lock;

    // =======================================================================
    // Headless spawn seam — production call-site coverage for the credential
    // scrub (plan
    // 2026-08-07-runner-context-visibility-and-session-env-secret-hygiene).
    //
    // This tests `finalize_headless_child_env`, which IS the production env
    // tail of `spawn_claude_child`. Deleting the `scrub_credential_env_tokio`
    // call from it reddens this test.
    // =======================================================================

    #[test]
    fn headless_finalize_child_env_scrubs_credential_values() {
        let mut cmd = tokio::process::Command::new("dummy");
        // As the inherited process env would have supplied them.
        for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }

        finalize_headless_child_env(&mut cmd);

        crate::terminal::assert_credentials_scrubbed_tokio(&cmd, "finalize_headless_child_env");

        // The runner-context half of the same tail must survive — this seam is
        // load-bearing for the "am I inside the runner?" predicate, and a scrub
        // that also ate the marker would be a regression in the other direction.
        let envs: Vec<(String, Option<String>)> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        // Presence, not a specific value: the seam resolves the bound port
        // itself now, and WHICH port that is belongs to
        // `headless_finalize_child_env_stamps_the_bound_port_not_the_configured_one`
        // (which pins it under the env lock). Asserting a literal here would
        // make this scrub test depend on a process-global another test owns.
        assert!(
            envs.iter().any(|(k, v)| k == "QONTINUI_RUNNER_API_PORT"
                && v.as_deref()
                    .is_some_and(|p| p.parse::<u16>().is_ok_and(|p| p != 0))),
            "the runner API port must still be exported"
        );
        assert!(
            envs.iter()
                .any(|(k, v)| k == "QONTINUI_RUNNER_CONTEXT" && v.is_some()),
            "the runner-context briefing must still be exported"
        );
    }

    /// The headless seam must stamp the ACTUALLY-BOUND port into both values a
    /// session reads to find its runner — `QONTINUI_RUNNER_API_PORT` and the
    /// `runner_context` briefing — even when `QONTINUI_PORT` names a different
    /// one (a runner that fell back off a blocked port; a secondary whose
    /// launcher set a port it did not get).
    ///
    /// This is the assertion the old `runner_api_port: u16` parameter made
    /// impossible: the wrong value was chosen at `spawn_claude_child`'s call
    /// site, which spawns a real process and cannot run in a unit test, so the
    /// seam could be tested green while production shipped `:9876` to a session
    /// whose runner was on `:9877` — and whose `.mcp.json`, provisioned one
    /// frame up, correctly named `:9877`.
    #[test]
    fn headless_finalize_child_env_stamps_the_bound_port_not_the_configured_one() {
        use crate::install_effects_producer::intercept::{set_bound_port, BoundPortRestore};
        let _env_lock = env_lock();
        // `runner_context` reads the process-global plan-capture level AND the
        // briefing-document cache, whose only serializer is this pin (its `Drop`
        // clears the cache). Without it a sibling test that plants a briefing
        // body — `session_briefing`'s and `fleet_policy_poller`'s do — can render
        // a body with no `{{runner_api_base}}` placeholder at all and redden the
        // port assertions below for reasons unrelated to the port. Same guard,
        // same reason, as `runner_context_briefing_is_appended_to_the_argv` and
        // `terminal::the_api_port_reaches_the_rendered_briefing`.
        //
        // Order is env_lock → pin, deliberately and consistently: nothing in the
        // crate takes them the other way round, so there is one global ordering
        // and no deadlock to introduce later.
        let _pin = crate::mcp::fleet_policy_poller::pin_plan_capture_level_for_test("off");
        let _env = crate::test_env::EnvVarRestore::capture(&["QONTINUI_PORT"]);
        let _bound = BoundPortRestore::capture();

        std::env::set_var("QONTINUI_PORT", "9876");
        set_bound_port(41_238);

        let mut cmd = tokio::process::Command::new("dummy");
        finalize_headless_child_env(&mut cmd);

        let envs: std::collections::HashMap<String, String> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                v.map(|v| {
                    (
                        k.to_string_lossy().to_string(),
                        v.to_string_lossy().to_string(),
                    )
                })
            })
            .collect();

        assert_eq!(
            envs.get("QONTINUI_RUNNER_API_PORT").map(String::as_str),
            Some("41238"),
            "the child must be told the port its runner actually bound"
        );
        let briefing = envs
            .get("QONTINUI_RUNNER_CONTEXT")
            .expect("the seam must still export the runner-context briefing");
        assert!(
            briefing.contains("41238"),
            "the briefing's endpoints must name the bound port: {briefing}"
        );
        assert!(
            !briefing.contains("127.0.0.1:9876"),
            "the briefing must not point the session at the DESIRED port: {briefing}"
        );
    }

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
            None,
            Vec::new(),
            &crate::claude_session::launch_spec::LaunchConfig::default(),
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
            None,
            Vec::new(),
            &crate::claude_session::launch_spec::LaunchConfig::default(),
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
            None,
            Vec::new(),
            &crate::claude_session::launch_spec::LaunchConfig::default(),
        );
        assert_eq!(
            cmd.join("|"),
            "claude|--dangerously-skip-permissions|--session-id|abc-123|\
             --|-prompt with dash"
        );
    }

    /// The autonomous (direct-exec) path injects the runner-context briefing as
    /// `--append-system-prompt <text>` — placed AFTER the pinned `--session-id`
    /// and BEFORE the `--` terminator, so it never disturbs the trailing
    /// positional prompt even when sibling `--add-dir=` args are present.
    #[test]
    fn continuation_command_injects_system_prompt_before_terminator() {
        let cmd = build_continuation_claude_command(
            "claude".to_string(),
            "abc-123",
            vec!["--add-dir=D:/wt/coord".to_string()],
            "do the thing".to_string(),
            Some("You are inside the Qontinui Runner.".to_string()),
            Vec::new(),
            &crate::claude_session::launch_spec::LaunchConfig::default(),
        );
        // The prompt stays the trailing positional behind `--`.
        assert_eq!(cmd.last().map(String::as_str), Some("do the thing"));
        assert_eq!(
            cmd.get(cmd.len() - 2).map(String::as_str),
            Some("--"),
            "the terminator must still immediately precede the prompt"
        );
        // The flag + value pair is present, together, ahead of the terminator.
        let flag = cmd
            .iter()
            .position(|a| a == "--append-system-prompt")
            .expect("--append-system-prompt must be injected");
        let term = cmd.iter().position(|a| a == "--").unwrap();
        assert!(
            flag < term,
            "system prompt flag must precede the terminator"
        );
        assert_eq!(
            cmd.get(flag + 1).map(String::as_str),
            Some("You are inside the Qontinui Runner."),
            "the briefing text must immediately follow its flag"
        );
        // Session id is still pinned before the injected flag.
        let sid = cmd.iter().position(|a| a == "--session-id").unwrap();
        assert!(sid < flag, "--session-id must precede the injected flag");
    }

    /// The real briefing injected on the direct-exec path carries the
    /// attributable source marker as its FIRST line (attributability contract
    /// on `terminal::runner_context` — incident coord #1242). Guards the
    /// argv delivery path end-to-end: build the command with the actual
    /// briefing and assert the `--append-system-prompt` value starts with it.
    #[test]
    fn continuation_command_system_prompt_carries_source_marker() {
        // `runner_context` reads the process-global plan-capture level, so pin
        // it through the shared guard rather than racing the `terminal` tests
        // that pin it too. The marker is line 1 at either level; the pin is
        // about running in a DEFINED state, not about the assertion.
        let _pin = crate::mcp::fleet_policy_poller::pin_plan_capture_level_for_test("off");
        let briefing = crate::terminal::runner_context(9876);
        let cmd = build_continuation_claude_command(
            "claude".to_string(),
            "abc-123",
            vec![],
            "do the thing".to_string(),
            Some(briefing),
            Vec::new(),
            &crate::claude_session::launch_spec::LaunchConfig::default(),
        );
        let flag = cmd
            .iter()
            .position(|a| a == "--append-system-prompt")
            .expect("--append-system-prompt must be injected");
        let prompt = cmd
            .get(flag + 1)
            .expect("the briefing text must immediately follow its flag");
        assert!(
            prompt.starts_with(crate::terminal::RUNNER_CONTEXT_SOURCE_MARKER),
            "injected system prompt must start with the source marker, got: {}",
            prompt.chars().take(80).collect::<String>()
        );
    }

    /// THE GAP THIS CLOSES. An autonomous spawn execs `claude` directly, so the
    /// identity shim — the thing that appends `--settings` for a hand-typed
    /// `claude` — is not in the chain. Before this pair was threaded through,
    /// every runner-spawned session ran with NO `SessionStart` hook, which
    /// silently voided the `SessionStart` policy injection that
    /// `QONTINUI_POLICY_INJECTION` defaults to ON.
    ///
    /// The pair must land AHEAD of the `--` terminator, like every other
    /// runner-injected flag, and must not disturb the trailing positional.
    #[test]
    fn continuation_command_injects_hook_settings_before_terminator() {
        let cmd = build_continuation_claude_command(
            "claude".to_string(),
            "abc-123",
            vec!["--add-dir=D:/wt/coord".to_string()],
            "do the thing".to_string(),
            Some("briefing".to_string()),
            vec![
                "--settings".to_string(),
                "C:/hooks/claude_hook_settings.json".to_string(),
            ],
            &crate::claude_session::launch_spec::LaunchConfig::default(),
        );
        assert_eq!(cmd.last().map(String::as_str), Some("do the thing"));
        assert_eq!(cmd.get(cmd.len() - 2).map(String::as_str), Some("--"));

        let flag = cmd
            .iter()
            .position(|a| a == "--settings")
            .expect("--settings must be injected on the direct-exec path");
        let term = cmd.iter().position(|a| a == "--").unwrap();
        assert!(flag < term, "--settings must precede the terminator");
        assert_eq!(
            cmd.get(flag + 1).map(String::as_str),
            Some("C:/hooks/claude_hook_settings.json"),
            "the settings path must immediately follow its flag"
        );
    }

    /// Fail-open: a runner that could not materialize the hook carrier passes an
    /// EMPTY vector, and the argv must then be byte-identical to the historical
    /// one. A bare `--settings` with no value, or one pointing at a file that
    /// does not exist, would break the session start this path must never break.
    #[test]
    fn continuation_command_omits_hook_settings_when_unavailable() {
        let cmd = build_continuation_claude_command(
            "claude".to_string(),
            "abc-123",
            vec![],
            "do the thing".to_string(),
            None,
            Vec::new(),
            &crate::claude_session::launch_spec::LaunchConfig::default(),
        );
        assert_eq!(
            cmd.join("|"),
            "claude|--dangerously-skip-permissions|--session-id|abc-123|--|do the thing"
        );
    }

    /// `--settings` takes exactly ONE value, so unlike the variadic `--add-dir`
    /// it cannot swallow the trailing prompt — but the ORDER still has to hold
    /// when both are present alongside the briefing. Pins the whole composed
    /// tail rather than one flag, because the 2026-06-12 incident was an
    /// ordering bug, not a missing-flag bug.
    #[test]
    fn continuation_command_orders_the_full_injected_tail() {
        let cmd = build_continuation_claude_command(
            "claude".to_string(),
            "abc-123",
            vec!["--add-dir=D:/wt/coord".to_string()],
            "do the thing".to_string(),
            Some("briefing".to_string()),
            vec!["--settings".to_string(), "C:/hooks/s.json".to_string()],
            &crate::claude_session::launch_spec::LaunchConfig::default(),
        );
        assert_eq!(
            cmd.join("|"),
            concat!(
                "claude|--dangerously-skip-permissions|--session-id|abc-123|",
                "--append-system-prompt|briefing|--settings|C:/hooks/s.json|",
                "--add-dir=D:/wt/coord|--|do the thing"
            )
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
            work_unit_slug_new: Some("readiness".to_string()),
            plan_slug: None,
            plan_phase: Some(4),
            correlation_topic: Some("my-coordination-topic".to_string()),
            account: None,
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
                // Legacy coord emit — the key coord still writes today.
                "plan_slug": payload.work_unit_slug(),
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
        // Legacy `plan_slug` still resolves through the accessor.
        assert_eq!(round_tripped.work_unit_slug(), Some("readiness"));
    }

    /// Minimal `LaunchPayload` body with the slug key left to the caller, so
    /// the dual-read tests below differ ONLY in which key they carry.
    fn launch_body_with(slug_keys: serde_json::Value) -> serde_json::Value {
        let mut body = serde_json::json!({
            "agent_id": uuid::Uuid::nil(),
            "target_device_id": uuid::Uuid::nil(),
            "worktrees": [],
            "jwt": "tok",
            "jwt_exp": 0,
            "initial_prompt": "go",
            "claim_token": "agent:00000000-0000-0000-0000-000000000000",
        });
        let map = body.as_object_mut().unwrap();
        for (k, v) in slug_keys.as_object().unwrap() {
            map.insert(k.clone(), v.clone());
        }
        body
    }

    // --- per-spawn account pin (plan 2026-08-25, Phase 3) -------------------

    fn ok_account(name: &str) -> crate::ai_provider::ResolvedAccount {
        crate::ai_provider::ResolvedAccount {
            config_dir: format!("C:\\claude\\.claude-{name}"),
            account_name: name.to_string(),
            cooldown_remaining_secs: None,
        }
    }

    /// THE fail-loud contract: a pinned account that does not resolve must
    /// ERROR, not quietly hand the spawn back to least-usage rotation. A
    /// silently-ignored pin is indistinguishable from an honoured one, so the
    /// operator could never tell which account actually ran.
    #[test]
    fn unknown_pinned_account_errors_instead_of_rotating() {
        let err = resolve_spawn_account_with(Some("nope"), |requested| {
            Err(crate::ai_provider::AccountSelectError::NotInRoster {
                requested: requested.to_string(),
                roster: vec!["hotmail".to_string(), "gmail".to_string()],
            })
        })
        .expect_err("an unresolvable pin must fail the spawn, not fall back");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("nope"),
            "the rejected name must be named: {msg}"
        );
        assert!(
            msg.contains("hotmail") && msg.contains("gmail"),
            "the operator needs the roster to correct the typo: {msg}"
        );
        assert!(
            msg.contains("refusing to fall back to account rotation"),
            "the refusal must be explicit, not implied: {msg}"
        );
    }

    /// A roster account with no live credentials is the same class of failure:
    /// spawning under it would 401-zombie the child, and rotating away from it
    /// would hide that the pin was ignored.
    #[test]
    fn logged_out_pinned_account_errors_instead_of_rotating() {
        let err = resolve_spawn_account_with(Some("gmail"), |_| {
            Err(crate::ai_provider::AccountSelectError::NotLoggedIn {
                config_dir: "C:\\claude\\.claude-gmail".to_string(),
            })
        })
        .expect_err("a logged-out pin must fail the spawn");
        assert!(format!("{err:#}").contains("no valid credentials"));
    }

    /// The resolver is never even consulted without a pin — `None` leaves
    /// today's rotation untouched.
    #[test]
    fn absent_pin_leaves_rotation_untouched() {
        let resolved = resolve_spawn_account_with(None, |_| {
            panic!("the resolver must not run when no account is pinned")
        })
        .expect("no pin is not an error");
        assert!(resolved.is_none());
    }

    /// A blank string names no account, so it is absence — not a wrong name to
    /// fail loudly on. (Coord normalizes empty-to-`None` at its boundary; this
    /// is the runner's belt-and-braces.)
    #[test]
    fn blank_pin_is_absence_not_a_bad_name() {
        for blank in ["", "   "] {
            let resolved = resolve_spawn_account_with(Some(blank), |_| {
                panic!("a blank pin must not reach the resolver")
            })
            .expect("a blank pin is not an error");
            assert!(resolved.is_none(), "blank {blank:?} must mean no pin");
        }
    }

    #[test]
    fn resolved_pin_is_returned_for_the_child_env() {
        let resolved = resolve_spawn_account_with(Some(".claude-hotmail"), |requested| {
            assert_eq!(requested, ".claude-hotmail");
            Ok(ok_account("hotmail"))
        })
        .expect("a resolvable pin succeeds")
        .expect("a pin yields an override");
        assert_eq!(resolved.config_dir, "C:\\claude\\.claude-hotmail");
    }

    /// The wire key coord sends is `account`, and its absence must stay
    /// tolerated (`#[serde(default)]`) — every coord shipping today omits it.
    #[test]
    fn launch_payload_reads_optional_account_key() {
        let without: LaunchPayload =
            serde_json::from_value(launch_body_with(serde_json::json!({}))).unwrap();
        assert_eq!(without.account, None);

        let with: LaunchPayload = serde_json::from_value(launch_body_with(serde_json::json!({
            "account": ".claude-hotmail"
        })))
        .unwrap();
        assert_eq!(with.account.as_deref(), Some(".claude-hotmail"));
    }

    #[test]
    fn launch_payload_accepts_legacy_plan_slug_key() {
        // Un-renamed coord (every coord shipping today) emits `plan_slug`.
        let body = launch_body_with(serde_json::json!({ "plan_slug": "some-unit" }));
        let p: LaunchPayload = serde_json::from_value(body).unwrap();
        assert_eq!(p.work_unit_slug(), Some("some-unit"));
    }

    #[test]
    fn launch_payload_accepts_new_work_unit_slug_key() {
        // Post-rename coord emits `work_unit_slug`.
        let body = launch_body_with(serde_json::json!({ "work_unit_slug": "some-unit" }));
        let p: LaunchPayload = serde_json::from_value(body).unwrap();
        assert_eq!(p.work_unit_slug(), Some("some-unit"));
    }

    /// THE cross-repo compatibility test.
    ///
    /// coord's Stage 2 `LaunchPayload` dual-emits BOTH keys for one release
    /// (two real fields, each `skip_serializing_if`), so the body on the wire
    /// is literally `{…,"plan_slug":"x","work_unit_slug":"x"}`. An earlier
    /// draft of this struct used `#[serde(alias = "plan_slug")]`, which made
    /// exactly that body fail with ``duplicate field `work_unit_slug` `` —
    /// i.e. EVERY spawn from a renamed coord would have failed to parse.
    /// This test is what keeps the two repos' windows compatible; if it is
    /// ever "simplified" back to an alias, spawns break fleet-wide.
    #[test]
    fn launch_payload_accepts_coords_dual_emitted_both_keys() {
        let body = launch_body_with(serde_json::json!({
            "plan_slug": "2026-07-28-some-unit",
            "work_unit_slug": "2026-07-28-some-unit",
        }));
        let p: LaunchPayload = serde_json::from_value(body)
            .expect("coord dual-emits both keys — this MUST parse, not duplicate-field");
        // Assert the NEW field specifically, not just the accessor. Coord emits
        // the same value under both keys, so an accessor-only assertion would
        // still pass with the new key entirely unwired (resolving through the
        // legacy fallback) — it would not catch a missing
        // `#[serde(rename = "work_unit_slug")]`, which is exactly the bug that
        // reached this test once already.
        assert_eq!(
            p.work_unit_slug_new.as_deref(),
            Some("2026-07-28-some-unit"),
            "the post-rename key must be READ, not merely tolerated"
        );
        assert_eq!(p.work_unit_slug(), Some("2026-07-28-some-unit"));
    }

    #[test]
    fn launch_payload_new_key_wins_when_both_disagree() {
        // Defensive: if a mid-rename coord ever emits divergent values, the
        // post-rename key is authoritative.
        let body = launch_body_with(serde_json::json!({
            "plan_slug": "old-name",
            "work_unit_slug": "new-name",
        }));
        let p: LaunchPayload = serde_json::from_value(body).unwrap();
        assert_eq!(p.work_unit_slug(), Some("new-name"));
    }

    #[test]
    fn launch_payload_absent_slug_is_none() {
        let body = launch_body_with(serde_json::json!({}));
        let p: LaunchPayload = serde_json::from_value(body).unwrap();
        assert_eq!(p.work_unit_slug(), None);
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

    /// REGRESSION (P2a review #2): the spawn gate and the WS resolver must
    /// read the SAME fact.
    ///
    /// `spawn_runtime` gates on `connected_coord_base().is_none()`. When
    /// `coord_ws_url` read the raw profile `coord_url` instead, a hosted
    /// (`qontinui_account`-tier) runner with no `coord_url` — the shipped
    /// end-user configuration — passed the gate, then got `None` here. The
    /// subscriber loop exited `Ok(())`, `spawn_supervised_forever` read that
    /// as a restart, and agent-spawn delivery stayed dead in a permanent
    /// 5s→300s respawn loop while the logs said the runtime was up.
    ///
    /// Hermetic: `COORD_HTTP_URL` removed, `QONTINUI_ENV` pointed at a profile
    /// name that cannot exist (so the profile arm misses on any machine), and
    /// `QONTINUI_CONFIG_DIR` pointed at a temp `settings.json` we own.
    #[test]
    fn coord_ws_url_resolves_on_hosted_tier_with_no_profile_coord_url() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[
            "COORD_HTTP_URL",
            "QONTINUI_ENV",
            "QONTINUI_CONFIG_DIR",
        ]);
        let dir = tempfile::tempdir().unwrap();
        std::env::remove_var("COORD_HTTP_URL");
        std::env::set_var("QONTINUI_ENV", "__qontinui_test_no_such_profile__");
        std::env::set_var("QONTINUI_CONFIG_DIR", dir.path());
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"tier":"qontinui_account"}"#,
        )
        .unwrap();

        let device = uuid::Uuid::nil();
        // The gate says connected…
        assert_eq!(
            qontinui_runner_lib::profiles::connected_coord_base().as_deref(),
            Some(qontinui_runner_lib::profiles::PROD_COORD_BASE),
        );
        // …so the resolver behind it must agree, and produce exactly one `/ws`.
        assert_eq!(
            coord_ws_url(device),
            Some(format!(
                "wss://coord.qontinui.io/ws?pattern=events.agent.spawn_requested.{device}"
            )),
            "gate and WS resolver disagreed — the respawn-loop regression"
        );
    }

    /// The inverse: a NON-hosted runner with nothing configured is isolated, so
    /// the gate refuses AND the resolver yields `None`. Also covers the
    /// unreadable-settings.json case, which must NOT dial production.
    #[test]
    fn coord_ws_url_is_none_when_isolated_or_tier_unknown() {
        let _g = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[
            "COORD_HTTP_URL",
            "QONTINUI_ENV",
            "QONTINUI_CONFIG_DIR",
        ]);
        for settings in [r#"{"tier":"local"}"#, "{not json"] {
            let dir = tempfile::tempdir().unwrap();
            std::env::remove_var("COORD_HTTP_URL");
            std::env::set_var("QONTINUI_ENV", "__qontinui_test_no_such_profile__");
            std::env::set_var("QONTINUI_CONFIG_DIR", dir.path());
            std::fs::write(dir.path().join("settings.json"), settings).unwrap();
            assert_eq!(
                qontinui_runner_lib::profiles::connected_coord_base(),
                None,
                "settings {settings:?}"
            );
            assert_eq!(
                coord_ws_url(uuid::Uuid::nil()),
                None,
                "settings {settings:?} must not open a prod WS subscription"
            );
        }
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
    fn provision_agent_defs_missing_source_falls_back_to_the_embedded_floor() {
        // qontinui-root exists but has no qontinui-claude-config/.claude/agents —
        // i.e. every non-operator fleet device.
        //
        // CONTRACT CHANGED DELIBERATELY. This test previously asserted
        // `!wt/.claude.exists()` — "creating nothing". That WAS the behaviour, and
        // it was the defect: a spawned agent then had no subagents at all, so
        // `claude` could not resolve the named subagent, the review never ran, and
        // coord aged the PR out as `specialist_timeout` with no error at the point
        // of cause. `crate::fleet_agents` now supplies an embedded floor, so the
        // correct assertion is the opposite one: the defs ARE there.
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let wt_cwd = wt.path().to_string_lossy().into_owned();

        let res = provision_agent_definitions_from_root(root.path(), &wt_cwd);
        assert!(res.is_ok(), "missing source dir must still fail soft (Ok)");

        let dst = wt.path().join(".claude").join("agents");
        let written = std::fs::read_dir(&dst)
            .expect("the embedded floor must create the agents dir")
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
            .count();
        assert_eq!(
            written,
            crate::fleet_agents::embedded_agent_count(),
            "with no checkout, every embedded default must be provisioned"
        );
        assert!(
            dst.join("code-reviewer.md").is_file(),
            "code-reviewer must resolve on a checkout-less device — the fleet's              pre-PR-review policy names that subagent specifically"
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
        let _env_lock = env_lock();
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
        let _env_lock = env_lock();
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
        let _env_lock = env_lock();
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

    /// The headless gate-continuation must hand coord-mcp provisioning a REAL
    /// resolved bound port — not the hardcoded `None` it passed before, which
    /// made the device arm refuse the write every single time (that path had
    /// therefore never provisioned anything).
    ///
    /// Pairs with `coord_mcp::device_path_with_bound_port_writes_proxy_and_no_
    /// synchronous_breadcrumb`, which proves the other half: a `Some(port)`
    /// reaching `provision_coord_mcp_with_jwt` writes the proxy `.mcp.json` on
    /// exactly that port.
    #[test]
    fn headless_continuation_passes_the_resolved_port_through() {
        assert_eq!(
            headless_continuation_bound_port(|| Some(19_876)),
            Some(19_876),
            "a resolvable bound port must reach provisioning verbatim (the write happens)"
        );
    }

    /// ...and where the port is GENUINELY unresolvable the headless path must
    /// still fail closed: `None` in, `None` out. Substituting a default (`:9876`)
    /// would write a dead-but-valid-looking proxy config on any secondary/temp
    /// runner — the F1 root cause. Downstream, `provision_coord_mcp_with_jwt`
    /// turns this `None` into a refusal + a degraded breadcrumb.
    #[test]
    fn headless_continuation_stays_fail_closed_on_an_unresolvable_port() {
        assert_eq!(
            headless_continuation_bound_port(|| None),
            None,
            "an unresolvable port must NOT be defaulted — provisioning must refuse"
        );
    }

    /// The production resolver the headless path is wired to — `coord_mcp`'s,
    /// which has owned this read since 2026-06-12 and is what `coord_doctor`,
    /// `config_report` and `agent_worktree::isolated_edit` already call. (This
    /// module briefly carried a second, byte-equivalent copy; a duplicated
    /// security control is two things to keep in step, so the copy is gone.)
    ///
    /// With no Tauri runtime — this unit-test context has no process-global
    /// `AppHandle` — it must return `None` rather than fabricating the
    /// env/bootstrap default port.
    #[test]
    fn resolve_bound_api_port_is_none_without_a_tauri_runtime() {
        let port = crate::coord_mcp::resolve_bound_api_port();
        assert_eq!(
            port, None,
            "no AppHandle ⇒ no managed AppState ⇒ the port is UNKNOWN, never a default"
        );
    }

    /// Drives the gate-continuation HEADLESS dispatch through the REAL
    /// `spawn_claude_child` + `pump_subprocess` path with a fake `claude` bin
    /// (a portable shell that prints + exits 0). Proves the arm spawns a child
    /// and returns `Ok(())` end-to-end. The coord lifecycle POSTs
    /// (`spawn-complete`/`spawn-failed`) no-op gracefully here because no
    /// `coord_url` profile is configured in the test env (`connected_coord_base()`
    /// returns `None`), so this asserts the SPAWN path, not the HTTP posts.
    ///
    /// Gated behind `QONTINUI_AGENT_RUNTIME_E2E=1` (the shell-as-claude
    /// substitution mutates process env globally; keep it opt-in like
    /// `fake_claude_e2e_smoke`).
    #[tokio::test]
    async fn gate_continuation_headless_spawns_child() {
        let _env_lock = env_lock();
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

    /// THE delivery-stall regression: claim → release → claim must succeed
    /// again. Before `release_gate_dispatch` existed, nothing ever removed an
    /// id from the dedupe set, so a locally-skipped (AtCap/DuplicateAnchor)
    /// continuation was dropped at the dedupe check on EVERY subsequent
    /// re-delivery for the process lifetime.
    #[test]
    fn release_gate_dispatch_allows_reclaim() {
        let gate = uuid::Uuid::now_v7();
        assert!(claim_gate_dispatch(gate), "first claim wins");
        assert!(!claim_gate_dispatch(gate), "duplicate is deduped");
        release_gate_dispatch(gate);
        assert!(
            claim_gate_dispatch(gate),
            "after release, a re-delivery of the SAME gate_id claims again"
        );
        assert!(
            !claim_gate_dispatch(gate),
            "…and the re-claim dedupes duplicates as usual"
        );
        // Releasing an id that was never claimed is a harmless no-op.
        release_gate_dispatch(uuid::Uuid::now_v7());
    }

    /// The work-unit sibling: a failed unit spawn is left un-consumed so coord
    /// re-lists it — the re-listed row must be claimable again after release.
    #[test]
    fn release_dispatch_dispatch_allows_reclaim() {
        let d = uuid::Uuid::now_v7();
        assert!(claim_dispatch_dispatch(d), "first claim wins");
        assert!(!claim_dispatch_dispatch(d), "duplicate is deduped");
        release_dispatch_dispatch(d);
        assert!(
            claim_dispatch_dispatch(d),
            "after release, the re-listed dispatch_id claims again"
        );
    }

    /// [`release_local_dispatch_claim`] routes to the right set per target
    /// (and is a no-op for the legacy no-id target).
    #[test]
    fn release_local_dispatch_claim_routes_per_target() {
        let g = uuid::Uuid::now_v7();
        let d = uuid::Uuid::now_v7();
        assert!(claim_gate_dispatch(g));
        assert!(claim_dispatch_dispatch(d));
        release_local_dispatch_claim(ConsumeTarget::Gate(g));
        release_local_dispatch_claim(ConsumeTarget::Dispatch(d));
        release_local_dispatch_claim(ConsumeTarget::None); // no-op, must not panic
        assert!(claim_gate_dispatch(g), "gate id was released");
        assert!(claim_dispatch_dispatch(d), "dispatch id was released");
    }

    /// End-to-end sequencing of the incident fix at the unit level: a gate id
    /// claimed by the dispatcher, then rejected AtCap by the guard, is
    /// RELEASED — so when a slot frees, the next delivery of the SAME gate id
    /// passes both the dedupe claim and the guard. (Before the fix, step 4's
    /// claim returned false forever: the primary's 9 boot-drained slots +
    /// `QONTINUI_CONTINUATION_SESSION_CAP=9` stranded 51 continuations.)
    #[test]
    fn atcap_release_lets_same_gate_redispatch_after_slot_frees() {
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "1");
        let gate = uuid::Uuid::now_v7();

        // Delivery 1: dispatcher claims the id, guard says AtCap (cap full).
        register_continuation_session("busy-slot".into(), Some("other-anchor".into()));
        let live_all = |_id: &str| true;
        assert!(claim_gate_dispatch(gate), "delivery 1 claims the id");
        assert_eq!(
            evaluate_continuation_guard(Some("new-anchor"), &live_all, &calm()),
            ContinuationGuard::AtCap(1)
        );
        // …the AtCap arm releases the in-process claim (the critical fix).
        release_gate_dispatch(gate);

        // A slot frees (the busy session exits) and the backstop re-lists the
        // row: the SAME gate id must now claim AND pass the guard.
        let busy_dead = |id: &str| id != "busy-slot";
        assert!(
            claim_gate_dispatch(gate),
            "re-delivery after the release claims the id again"
        );
        assert_eq!(
            evaluate_continuation_guard(Some("new-anchor"), &busy_dead, &calm()),
            ContinuationGuard::Proceed,
            "freed slot → the deferred continuation finally dispatches"
        );

        release_gate_dispatch(gate);
        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// The deferred stamp is rate-limited to once per gate per hour, per gate
    /// id (independent gates don't suppress each other).
    #[test]
    fn deferred_stamp_rate_limits_per_gate_per_hour() {
        let gate_a = uuid::Uuid::now_v7();
        let gate_b = uuid::Uuid::now_v7();
        let t0 = std::time::Instant::now();

        assert!(
            should_post_deferred_stamp(gate_a, t0),
            "first stamp for a gate posts"
        );
        assert!(
            !should_post_deferred_stamp(gate_a, t0),
            "an immediate second stamp for the SAME gate is suppressed"
        );
        assert!(
            !should_post_deferred_stamp(
                gate_a,
                t0 + CONTINUATION_DEFERRED_STAMP_INTERVAL - Duration::from_secs(1)
            ),
            "still suppressed just inside the window"
        );
        assert!(
            should_post_deferred_stamp(gate_a, t0 + CONTINUATION_DEFERRED_STAMP_INTERVAL),
            "posts again once the window has elapsed"
        );
        assert!(
            should_post_deferred_stamp(gate_b, t0),
            "a different gate is independent"
        );
    }

    /// The poll self-report body serializes to the locked wire shape:
    /// `{device_id, listed_n, dispatched_n, skipped_n, skip_reasons:{…}}` with
    /// `skipped_n = listed − dispatched` and only non-zero skip reasons keyed.
    #[test]
    fn continuation_poll_report_body_wire_shape() {
        let dev = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let body = ContinuationPollReportBody::new(
            dev,
            PollRunCounts {
                listed_n: 5,
                dispatched_n: 2,
                already_dispatched: 2,
                not_addressed_to_self: 1,
                spawn_authorization_denied: 0,
                fetch_failed: false,
            },
        );
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "device_id": "11111111-1111-1111-1111-111111111111",
                "listed_n": 5,
                "dispatched_n": 2,
                "skipped_n": 3,
                "skip_reasons": {
                    "already_dispatched": 2,
                    "not_addressed_to_self": 1,
                },
            })
        );

        // Agent-registry refusals get their OWN skip reason (Phase 4c), so
        // coord can tell "the user has not opted this device into standing
        // continuations" apart from a dedupe drop.
        let denied = ContinuationPollReportBody::new(
            dev,
            PollRunCounts {
                listed_n: 3,
                dispatched_n: 0,
                spawn_authorization_denied: 3,
                ..Default::default()
            },
        );
        let v = serde_json::to_value(&denied).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "device_id": "11111111-1111-1111-1111-111111111111",
                "listed_n": 3,
                "dispatched_n": 0,
                "skipped_n": 3,
                "skip_reasons": { "spawn_authorization_denied": 3 },
            })
        );

        // Zero-skip run: skip_reasons is an EMPTY object, not omitted.
        let clean = ContinuationPollReportBody::new(
            dev,
            PollRunCounts {
                listed_n: 1,
                dispatched_n: 1,
                ..Default::default()
            },
        );
        let v = serde_json::to_value(&clean).unwrap();
        assert_eq!(v["skipped_n"], 0);
        assert_eq!(v["skip_reasons"], serde_json::json!({}));

        // A pending-list fetch failure self-reports as all-zeros +
        // skip_reasons.fetch_failed = 1, so coord can tell a failing pull
        // route apart from a dead poll loop.
        let failed = ContinuationPollReportBody::new(dev, PollRunCounts::fetch_failure());
        let v = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "device_id": "11111111-1111-1111-1111-111111111111",
                "listed_n": 0,
                "dispatched_n": 0,
                "skipped_n": 0,
                "skip_reasons": { "fetch_failed": 1 },
            })
        );
    }

    // NOTE: the delivery-task panic net is `spawn_supervised_delivery` — thin
    // wiring over `crate::mcp::task_supervisor::spawn_supervised_forever`,
    // whose respawn-after-panic behavior is covered by that module's own tests
    // (`forever_variant_respawns_after_panic` et al.).

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
        // Poison-recovering to match `CONT_GUARD_LOCK` / the shared `env_lock`:
        // a prior test that panicked while holding this registry mutex must not
        // cascade-poison the continuation-guard tests that reset it here.
        continuation_sessions()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// Poison-recovering (`unwrap_or_else(into_inner)`), matching the shared
    /// `env_lock()`: a panicking guard test must not cascade-poison the rest.
    static CONT_GUARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A thread verdict that says nothing: the machine has headroom, or the
    /// sensor had no opinion. The default for every guard test that is about the
    /// dedup / cap lanes rather than about load — passing it keeps those tests
    /// measuring exactly what they measured before the thread lane existed.
    fn calm() -> crate::resource_guard::SpawnGate {
        crate::resource_guard::SpawnGate::Proceed
    }

    /// The REAL thread verdict for an injected reading, folded through the same
    /// pure evaluator the live path uses
    /// ([`crate::resource_guard::evaluate_threads`]) against the SHIPPED
    /// ceilings (256 warn / 400 critical).
    ///
    /// Deliberately not a hand-built `SpawnGate`: a test that constructs its own
    /// `Warn(…)` proves the guard reacts to a value the test wrote, not that any
    /// reachable thread count produces it. Going through `evaluate_threads`
    /// means these tests also fail if the ceilings are moved out from under
    /// them, which is the coupling worth having.
    ///
    /// `None` is the UNKNOWN reading — what
    /// [`crate::health_monitor::thread_count_reading`] returns when the OS
    /// thread table cannot be read.
    fn thread_verdict(reading: Option<usize>) -> crate::resource_guard::SpawnGate {
        crate::resource_guard::evaluate_threads(
            reading,
            &crate::settings::SessionGuardSettings::default(),
        )
    }

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
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        // Raise the cap out of the way so this test isolates the dedup path.
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "100");

        // No session yet → proceed, then register it as live.
        let live_all = |_id: &str| true;
        assert_eq!(
            evaluate_continuation_guard(Some("plan:foo:phase:1"), &live_all, &calm()),
            ContinuationGuard::Proceed
        );
        register_continuation_session("term-tid-1".to_string(), Some("plan:foo:phase:1".into()));

        // Same anchor, still live → DuplicateAnchor (carries the existing tid).
        assert_eq!(
            evaluate_continuation_guard(Some("plan:foo:phase:1"), &live_all, &calm()),
            ContinuationGuard::DuplicateAnchor("term-tid-1".to_string())
        );
        // A different anchor is unaffected.
        assert_eq!(
            evaluate_continuation_guard(Some("plan:foo:phase:2"), &live_all, &calm()),
            ContinuationGuard::Proceed
        );

        // Now the first session is dead → the guard prunes it and the anchor is
        // free to spawn again (the legitimate re-run after completion).
        let dead_first = |id: &str| id != "term-tid-1";
        assert_eq!(
            evaluate_continuation_guard(Some("plan:foo:phase:1"), &dead_first, &calm()),
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
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "100");
        let live_all = |_id: &str| true;

        register_continuation_session("tid-a".into(), None);
        // Another anchor-less dispatch must NOT be deduped against the existing
        // anchor-less session (we can't correlate them).
        assert_eq!(
            evaluate_continuation_guard(None, &live_all, &calm()),
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
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "2");
        let live_all = |_id: &str| true;

        // 0 live, cap 2 → proceed.
        assert_eq!(
            evaluate_continuation_guard(Some("a1"), &live_all, &calm()),
            ContinuationGuard::Proceed
        );
        register_continuation_session("t1".into(), Some("a1".into()));
        register_continuation_session("t2".into(), Some("a2".into()));

        // 2 live, cap 2 → AtCap (a NEW anchor, so not a dedup).
        assert_eq!(
            evaluate_continuation_guard(Some("a3"), &live_all, &calm()),
            ContinuationGuard::AtCap(2)
        );

        // One session dies → pruned → back under cap → proceed.
        let t1_dead = |id: &str| id != "t1";
        assert_eq!(
            evaluate_continuation_guard(Some("a3"), &t1_dead, &calm()),
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
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "1");
        let live_all = |_id: &str| true;

        register_continuation_session("t1".into(), Some("anchor-dup".into()));
        // At cap (1) AND the anchor matches a live session → dedup wins.
        assert_eq!(
            evaluate_continuation_guard(Some("anchor-dup"), &live_all, &calm()),
            ContinuationGuard::DuplicateAnchor("t1".to_string())
        );

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// The cap reads `QONTINUI_CONTINUATION_SESSION_CAP`, falling back to the
    /// default for an unset / non-numeric value — and the operator override wins
    /// in BOTH directions over a default that is now finite.
    ///
    /// The two default assertions here used to compare `continuation_session_cap()`
    /// against `DEFAULT_CONTINUATION_SESSION_CAP`, which is a tautology: it
    /// passed identically when the default was `usize::MAX` and it would pass if
    /// the default were 0. They now pin the two properties that actually matter
    /// — the default is FINITE (the whole point of Phase 1 of
    /// `2026-08-30-load-aware-spawn-admission-control`), and an explicit env
    /// value displaces it in either direction.
    #[test]
    fn continuation_session_cap_env_parsing() {
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("QONTINUI_CONTINUATION_SESSION_CAP").ok();

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        let default_cap = continuation_session_cap();
        assert_eq!(default_cap, DEFAULT_CONTINUATION_SESSION_CAP);
        assert!(
            default_cap < usize::MAX,
            "the unset default must be FINITE — an infinite cap is the guard that \
             failed to fire on 2026-08-29"
        );
        assert_eq!(
            default_cap, 64,
            "the shipped default is 64; see DEFAULT_CONTINUATION_SESSION_CAP for why"
        );

        // Override DOWN (an operator throttling a small box)…
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "7");
        assert_eq!(continuation_session_cap(), 7);
        // …and UP, past the default: the override is not a ceiling-only knob.
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "200");
        assert_eq!(continuation_session_cap(), 200);

        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "not-a-number");
        assert_eq!(
            continuation_session_cap(),
            DEFAULT_CONTINUATION_SESSION_CAP,
            "garbage falls back to the default, not to unbounded"
        );

        match prev {
            Some(v) => std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", v),
            None => std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP"),
        }
    }

    /// REGRESSION (the 2026-08-29 wedge, count lane): the DEFAULT cap — no env
    /// override at all — refuses. This test is the inverse of the one it
    /// replaces (`unbounded_default_cap_never_refuses_on_count`, which asserted
    /// `continuation_session_cap() == usize::MAX` and that 50 live sessions
    /// still proceed); that assertion encoded the very default the incident
    /// falsified, so it is deleted rather than adjusted.
    ///
    /// Fill the registry to exactly the default cap and confirm the NEXT
    /// dispatch is `AtCap` with the default in the payload. Deleting the finite
    /// default fails this test.
    #[test]
    fn default_cap_is_finite_and_refuses_at_the_limit() {
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        let cap = continuation_session_cap();
        assert!(cap < usize::MAX, "the default cap must be finite");

        let live_all = |_id: &str| true;
        // One under the cap → still Proceed: the cap must not fire early.
        for i in 0..cap - 1 {
            register_continuation_session(format!("t{i}"), Some(format!("a{i}")));
        }
        assert_eq!(
            evaluate_continuation_guard(Some("a-new"), &live_all, &calm()),
            ContinuationGuard::Proceed,
            "cap-1 live sessions is under the cap"
        );

        // At the cap → the next one is refused, naming the default.
        register_continuation_session(format!("t{}", cap - 1), Some("a-last".into()));
        assert_eq!(
            evaluate_continuation_guard(Some("a-new"), &live_all, &calm()),
            ContinuationGuard::AtCap(cap),
            "at {cap} live sessions the {n}th continuation is refused on count alone",
            n = cap + 1
        );
        clear_continuation_registry();
    }

    /// The three-valued thread lane maps onto two guard outcomes: `Proceed`
    /// proceeds, and BOTH `Warn` and `Critical` defer.
    ///
    /// The asymmetry with `resource_guard::admit_spawn` (which refuses only at
    /// CRITICAL) is the point, not an inconsistency — a queued continuation can
    /// wait and be re-delivered, so it is back-pressured a band earlier than an
    /// operator's own terminal, which cannot be refused on a soft signal. Both
    /// callers read the SAME folded numbers; only the trip point differs.
    ///
    /// Readings are chosen against the shipped ceilings (256 warn / 400
    /// critical) and the measured 150-151-thread idle baseline: 151 is a live
    /// idle runner, 300 is half the box gone, 540 is the 2026-08-29 wedge.
    #[test]
    fn thread_pressure_defers_at_warn_and_critical_but_not_below() {
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "100");
        let live_all = |_id: &str| true;

        // A live idle runner (151 threads) is BELOW the 256 warn ceiling →
        // nothing to say, spawn.
        assert_eq!(
            evaluate_continuation_guard(Some("a1"), &live_all, &thread_verdict(Some(151))),
            ContinuationGuard::Proceed,
            "an idle machine must not defer — a guard that fires at rest is a \
             permanently-closed queue, not a guard"
        );
        // Exactly ON the warn ceiling is AT it, not over it (the lane's
        // boundaries are strictly-above).
        assert_eq!(
            evaluate_continuation_guard(Some("a1"), &live_all, &thread_verdict(Some(256))),
            ContinuationGuard::Proceed,
            "256 is the ceiling, not a crossing of it"
        );

        // WARN band (257..=400): defer, naming the warn ceiling.
        match evaluate_continuation_guard(Some("a1"), &live_all, &thread_verdict(Some(300))) {
            ContinuationGuard::ThreadPressure {
                severity,
                observation,
            } => {
                assert_eq!(severity, "warn");
                assert_eq!(observation.observed, 300);
                assert_eq!(
                    observation.limit, 256,
                    "the WARN ceiling, not the critical one"
                );
            }
            other => panic!("300 threads must defer at warn severity, got {other:?}"),
        }

        // CRITICAL band (>400): still a deferral, now naming the critical
        // ceiling. The guard does not escalate past deferral — there is nothing
        // heavier for a queued row than leaving it queued.
        match evaluate_continuation_guard(Some("a1"), &live_all, &thread_verdict(Some(540))) {
            ContinuationGuard::ThreadPressure {
                severity,
                observation,
            } => {
                assert_eq!(severity, "critical");
                assert_eq!(observation.observed, 540, "the wedge's own thread count");
                assert_eq!(observation.limit, 400);
            }
            other => panic!("540 threads must defer at critical severity, got {other:?}"),
        }

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// REGRESSION (the 2026-08-29 wedge, thread lane — the load-bearing fix).
    ///
    /// Delete the thread check from `evaluate_continuation_guard` and this test
    /// fails: with an empty registry and a cap of 100, EVERY other lane says
    /// `Proceed`, so the wedge's own 540-thread reading is the only thing that
    /// can stop the spawn. That is exactly the shape of the incident — ~130
    /// continuations admitted onto a box carrying 540 OS threads because no
    /// guard in this path was looking at threads.
    ///
    /// The second half is the other half of the requirement: the deferral must
    /// be SELF-HEALING. Once the reading drops back to the idle baseline the
    /// very next call proceeds, with no flag to reset and no state to clear —
    /// which is what makes the backstop poll's re-delivery sufficient.
    #[test]
    fn loaded_machine_defers_continuation_then_recovers_when_threads_free() {
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        // Cap and dedup both deliberately out of the way: nothing but the thread
        // reading can produce a non-Proceed verdict here.
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "100");
        let live_all = |_id: &str| true;

        let loaded = evaluate_continuation_guard(
            Some("wedge-anchor"),
            &live_all,
            &thread_verdict(Some(540)),
        );
        assert!(
            matches!(loaded, ContinuationGuard::ThreadPressure { .. }),
            "a 540-thread machine must NOT admit another continuation; got {loaded:?}"
        );
        assert_ne!(
            loaded,
            ContinuationGuard::Proceed,
            "this is the assertion the incident would have failed"
        );

        // Sessions exit, threads go back to the pool, the same anchor is
        // re-delivered by the backstop poll → it dispatches.
        assert_eq!(
            evaluate_continuation_guard(
                Some("wedge-anchor"),
                &live_all,
                &thread_verdict(Some(151))
            ),
            ContinuationGuard::Proceed,
            "the deferral self-heals on the reading alone — no arming flag, no reset"
        );

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// Ordering, step 1 before step 2: a duplicate of an already-LIVE anchor is
    /// reported as `DuplicateAnchor` even on a critically loaded machine.
    ///
    /// The honest reason wins. Spawning this dispatch would be wrong on an idle
    /// box too, and "deferred under load" would imply it will be re-delivered
    /// into a spawn — it will not; its work IS the live session.
    #[test]
    fn dedup_takes_precedence_over_thread_pressure() {
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "100");
        let live_all = |_id: &str| true;

        register_continuation_session("t-live".into(), Some("anchor-dup".into()));
        assert_eq!(
            evaluate_continuation_guard(Some("anchor-dup"), &live_all, &thread_verdict(Some(540))),
            ContinuationGuard::DuplicateAnchor("t-live".to_string()),
            "a live duplicate is a dedup, not a load deferral"
        );

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// Ordering, step 2 before step 3: when BOTH the thread lane and the count
    /// cap would fire, the verdict names THREADS.
    ///
    /// Thread count is the live reading of the resource that actually ran out;
    /// the count cap is a static backstop. Reporting a thread-starved machine as
    /// `AtCap` would send the next investigation to the wrong constant — which
    /// is precisely what happened on 2026-08-29, when the cap was the thing
    /// everyone looked at and threads were the thing that broke.
    #[test]
    fn thread_pressure_trips_ahead_of_the_count_cap() {
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        // Cap 1 with 1 live session: the count lane WOULD say AtCap(1).
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "1");
        register_continuation_session("t1".into(), Some("other-anchor".into()));
        let live_all = |_id: &str| true;

        // Sanity: with no thread pressure this is unambiguously AtCap.
        assert_eq!(
            evaluate_continuation_guard(Some("a-new"), &live_all, &calm()),
            ContinuationGuard::AtCap(1)
        );

        // Add thread pressure and the honest, earlier signal wins.
        match evaluate_continuation_guard(Some("a-new"), &live_all, &thread_verdict(Some(300))) {
            ContinuationGuard::ThreadPressure {
                severity,
                observation,
            } => {
                assert_eq!(severity, "warn");
                assert_eq!(observation.observed, 300);
            }
            other => panic!("threads must outrank the count cap, got {other:?}"),
        }

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
        clear_continuation_registry();
    }

    /// FAIL OPEN: an UNKNOWN thread reading proceeds.
    ///
    /// `health_monitor::thread_count_reading()` returns `None` when the OS
    /// thread table cannot be read, and `evaluate_threads` turns that into
    /// `Proceed` — the doctrine the whole `resource_guard` module is built on. A
    /// sensor that stops answering must not silently wedge the continuation
    /// queue shut; an unreadable machine is UNKNOWN, not overloaded. (The older
    /// `usize` form of that sensor reported failure as `0`, which against a
    /// CEILING reads as "perfectly idle" — the same fail-open answer by
    /// accident. `Option` makes it the deliberate one.)
    #[test]
    fn unknown_thread_reading_fails_open() {
        let _env_lock = env_lock();
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_continuation_registry();
        std::env::set_var("QONTINUI_CONTINUATION_SESSION_CAP", "100");
        let live_all = |_id: &str| true;

        assert_eq!(
            thread_verdict(None),
            crate::resource_guard::SpawnGate::Proceed,
            "an unreadable sensor has no opinion"
        );
        assert_eq!(
            evaluate_continuation_guard(Some("a1"), &live_all, &thread_verdict(None)),
            ContinuationGuard::Proceed,
            "UNKNOWN must never defer — fail open"
        );

        std::env::remove_var("QONTINUI_CONTINUATION_SESSION_CAP");
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
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
        let _g = CONT_GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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

    /// The periodic backstop poll is ALWAYS armed — its cadence resolution is
    /// the only remaining policy knob. Unset / garbage env values fall back to
    /// the 300s default; explicit values are honored but floored at 30s so a
    /// misconfiguration can't turn the safety net into a coord hammer.
    ///
    /// (Replaces `at_cap_deferral_arms_backstop_flag`: the AtCap arming
    /// predicate was deleted with the gate — a WS frame lost while connected,
    /// with no AtCap deferral this process lifetime and no terminal exit,
    /// stranded a continuation until the next reconnect. The loop body now
    /// polls unconditionally on every tick; there is no arming state left to
    /// test.)
    #[test]
    fn backstop_poll_secs_default_floor_and_override() {
        // Unset → default.
        assert_eq!(
            resolve_backstop_poll_secs(None),
            CONTINUATION_BACKSTOP_POLL_SECS_DEFAULT,
            "unset env must fall back to the 300s default"
        );
        // Garbage → default (never panics, never zero).
        assert_eq!(
            resolve_backstop_poll_secs(Some("not-a-number")),
            CONTINUATION_BACKSTOP_POLL_SECS_DEFAULT
        );
        assert_eq!(
            resolve_backstop_poll_secs(Some("")),
            CONTINUATION_BACKSTOP_POLL_SECS_DEFAULT
        );
        // Explicit override honored (whitespace-tolerant).
        assert_eq!(resolve_backstop_poll_secs(Some(" 600 ")), 600);
        // Below-floor values are clamped up; zero can never disable the poll.
        assert_eq!(
            resolve_backstop_poll_secs(Some("5")),
            CONTINUATION_BACKSTOP_POLL_SECS_FLOOR
        );
        assert_eq!(
            resolve_backstop_poll_secs(Some("0")),
            CONTINUATION_BACKSTOP_POLL_SECS_FLOOR,
            "the backstop is always-on; 0 must clamp to the floor, not park the loop"
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

    /// `compressed_jwt_exp` (debug / test-fixtures variant): with the env knob
    /// set it clamps the bookkeeping exp to ~`now + n`; without it the real
    /// `jwt_exp` passes through untouched. This is the Phase-2 Tier-B override.
    ///
    /// Touches the process env var, so it must run serially w.r.t. any other
    /// test reading the same var. It is the only test that reads it, and it
    /// restores the prior value, so a `set/remove` here is self-contained.
    #[test]
    #[cfg(any(debug_assertions, feature = "test-fixtures"))]
    fn compressed_jwt_exp_honors_env_override() {
        let _env_lock = env_lock();
        let prior = std::env::var(AGENT_JWT_EXP_COMPRESS_ENV).ok();

        // A far-future real expiry (~4h out), like a real agent JWT.
        let real_exp = chrono::Utc::now().timestamp() + 4 * 3600;

        // Unset → pass-through.
        std::env::remove_var(AGENT_JWT_EXP_COMPRESS_ENV);
        assert_eq!(
            compressed_jwt_exp(real_exp),
            real_exp,
            "without the env knob the real exp passes through unchanged"
        );

        // Set to n=5 → clamp to min(real_exp, now+5) == now+5.
        std::env::set_var(AGENT_JWT_EXP_COMPRESS_ENV, "5");
        let before = chrono::Utc::now().timestamp();
        let got = compressed_jwt_exp(real_exp);
        let after = chrono::Utc::now().timestamp();
        assert!(
            (before + 5..=after + 5).contains(&got),
            "compressed exp must be ~now+5 (got {got}, window {}..={})",
            before + 5,
            after + 5
        );
        assert!(
            got < real_exp,
            "compressed exp must be earlier than real exp"
        );

        // A non-numeric value is ignored → pass-through.
        std::env::set_var(AGENT_JWT_EXP_COMPRESS_ENV, "not-a-number");
        assert_eq!(
            compressed_jwt_exp(real_exp),
            real_exp,
            "an unparseable env value is ignored and the real exp passes through"
        );

        // Restore prior env state.
        match prior {
            Some(v) => std::env::set_var(AGENT_JWT_EXP_COMPRESS_ENV, v),
            None => std::env::remove_var(AGENT_JWT_EXP_COMPRESS_ENV),
        }
    }
}
