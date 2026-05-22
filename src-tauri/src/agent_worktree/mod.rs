//! Worktree-per-agent spawn path (Coordination Phase 1).
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-14-branch-per-agent-coordination-plan.md`
//! §4.1. New code path that, on session creation, calls qontinui-coord's
//! `POST /agents/allocate`, materializes the per-repo worktrees via
//! `git worktree add`, and returns the agent_id + per-repo materialized
//! paths so the caller (PTY-spawn, slash-command, UI) can use them as
//! CWD.
//!
//! Gated behind the `QONTINUI_AGENT_WORKTREE_MODE` env var (default off).
//! When off, this module is dead code — the existing shared-tree spawn
//! path stays exclusively active. Reversible per the plan's Phase 7
//! commit ("feature-flag-flip the new spawn path on for everyone,
//! monitor a week, then delete").
//!
//! ## Scope (Phase 1)
//!
//! - Call coord `/agents/allocate` with `{ machine_id, repos: [{repo,
//!   parent_sha}], intent? }`.
//! - `git worktree add <suggested-path> -b <branch> <parent_sha>` for
//!   each returned worktree row.
//! - Return materialized rows.
//!
//! ## Not yet in scope
//!
//! - Cross-repo Cargo.toml path-dep rewriting. Memory
//!   `feedback_worktree_path_dep_hooks` documents the gotcha for
//!   committed rewrites; uncommitted rewrites get stashed by the
//!   pre-commit cargo hook. Phase 1 surfaces the worktrees with the
//!   path deps pointing at the **canonical** sibling tree (not the
//!   sibling worktree). Cross-repo work that needs the sibling
//!   worktree's HEAD is a follow-up (see tracker Row 5 "Cross-repo
//!   path-deps").
//! - Status lifecycle (`allocated → active`). Phase 1 writes the row
//!   in `allocated` state via coord, and the runner doesn't yet
//!   transition it. Phase 3+ (merge proposal API) drives the rest of
//!   the state machine.
//! - Cleanup of materialized worktrees. The sweeper on coord prunes
//!   `coord.agent_worktrees` rows; pruning the on-disk worktree
//!   directories is Phase 6+ territory.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::worktree::run_git_command;

/// Env var that turns the new spawn path on. Default off — `feature
/// flag agent_worktree_mode` per plan §5 Phase 1.
const FLAG_ENV: &str = "QONTINUI_AGENT_WORKTREE_MODE";

// =============================================================================
// Pre-allocate claim acquire + heartbeat task — Phase 3 of
// plan 2026-05-18-agent-spawn-coordination.
// =============================================================================

/// Spawn-time claim conflict — surfaced when coord's
/// `POST /claims/acquire` returns `Held`. The runner caller bubbles
/// this up to the webview via the `agent-claim-conflict` Tauri event;
/// the user decides abort / wait / steal.
///
/// `current_holder` is coord's best-effort `machine_id` of the agent
/// currently holding the claim (per `AcquireResult::Held` at
/// `coord:claims.rs:162-167`). Empty when the Redis key vanished
/// between coord's SET-NX and follow-up GET.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimConflict {
    pub kind: String,
    pub resource_key: String,
    pub current_holder: String,
    pub intent: Option<String>,
}

/// Pre-allocate claim parameters passed into [`allocate_and_materialize`].
///
/// Constructed via [`ClaimSpawnContext::from_intent_and_paths`] and the
/// callers' optional explicit plan-id / phase / file-glob hints.
#[derive(Debug, Clone)]
pub struct ClaimSpawnContext {
    /// snake_case ClaimKind matching coord's enum (`phase`, `file_glob`).
    pub kind: &'static str,
    pub resource_key: String,
    pub intent: Option<String>,
    pub plan_id: Option<String>,
    pub phase: Option<String>,
}

impl ClaimSpawnContext {
    /// Derive a claim context from the spawn payload's optional fields.
    ///
    /// Precedence (per plan Phase 3 spec):
    /// 1. Both `plan_id` AND `phase` present → `ClaimKind::Phase` with
    ///    `resource_key = "plan:<plan_id>:phase:<phase>"`.
    /// 2. Otherwise, if `declared_overlap_paths` non-empty → first non-empty
    ///    path joined with `,` becomes a `ClaimKind::FileGlob` claim.
    /// 3. Otherwise `None` — proceed to allocate without claim pre-flight
    ///    (preserves existing behavior for legacy callers).
    pub fn from_intent_and_paths(
        intent: Option<&str>,
        plan_id: Option<&str>,
        phase: Option<&str>,
        declared_overlap_paths: Option<&[String]>,
    ) -> Option<Self> {
        let intent_s = intent.map(|s| s.to_string());
        if let (Some(p), Some(n)) = (plan_id, phase) {
            if !p.is_empty() && !n.is_empty() {
                return Some(Self {
                    kind: "phase",
                    resource_key: format!("plan:{p}:phase:{n}"),
                    intent: intent_s,
                    plan_id: Some(p.to_string()),
                    phase: Some(n.to_string()),
                });
            }
        }
        if let Some(paths) = declared_overlap_paths {
            let non_empty: Vec<&str> = paths
                .iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.as_str())
                .collect();
            if !non_empty.is_empty() {
                return Some(Self {
                    kind: "file_glob",
                    resource_key: non_empty.join(","),
                    intent: intent_s,
                    plan_id: None,
                    phase: None,
                });
            }
        }
        None
    }
}

/// Outcome of the pre-allocate `/claims/acquire` call.
#[derive(Debug)]
enum AcquireOutcome {
    /// `result: claimed` or `renewed` — proceed to `/agents/allocate`.
    Acquired { ttl_seconds: i64 },
    /// `result: held` — return [`ClaimConflict`] to the caller.
    Held(ClaimConflict),
    /// Topic / unknown / invalid_topic results from
    /// `AcquireResult::TopicConflict | TopicUnknown | InvalidTopic`.
    /// Treated as a hard error — the spawn path doesn't pass `topic`
    /// today so these shouldn't fire, but handle defensively.
    Other(String),
}

async fn pre_allocate_claim(
    coord_http_base: &str,
    machine_id: &uuid::Uuid,
    ctx: &ClaimSpawnContext,
) -> Result<AcquireOutcome, String> {
    let url = format!("{}/claims/acquire", coord_http_base.trim_end_matches('/'));
    let mut metadata = serde_json::json!({});
    if let Some(intent) = &ctx.intent {
        metadata["intent"] = serde_json::json!(intent);
    }
    if let Some(plan_id) = &ctx.plan_id {
        metadata["plan_id"] = serde_json::json!(plan_id);
    }
    if let Some(phase) = &ctx.phase {
        metadata["phase"] = serde_json::json!(phase);
    }
    let body = serde_json::json!({
        "kind": ctx.kind,
        "resource_key": ctx.resource_key,
        "machine_id": machine_id.to_string(),
        "metadata": metadata,
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build claim http client: {e}"))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("read /claims/acquire body: {e}"))?;
    if !status.is_success() && status != reqwest::StatusCode::CONFLICT {
        return Err(format!(
            "POST /claims/acquire returned {} — body: {body_text}",
            status.as_u16()
        ));
    }
    let v: serde_json::Value = serde_json::from_str(&body_text)
        .map_err(|e| format!("parse /claims/acquire body: {e} (raw: {body_text})"))?;
    match v.get("result").and_then(|r| r.as_str()) {
        Some("claimed") | Some("renewed") => {
            let ttl = v
                .get("ttl_seconds")
                .and_then(|n| n.as_i64())
                .unwrap_or_else(|| default_ttl_seconds_for(ctx.kind));
            Ok(AcquireOutcome::Acquired { ttl_seconds: ttl })
        }
        Some("held") => {
            let current_holder = v
                .get("current_holder")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            Ok(AcquireOutcome::Held(ClaimConflict {
                kind: ctx.kind.to_string(),
                resource_key: ctx.resource_key.clone(),
                current_holder,
                intent: ctx.intent.clone(),
            }))
        }
        Some(other) => Ok(AcquireOutcome::Other(format!(
            "/claims/acquire unexpected result `{other}` — body: {body_text}"
        ))),
        None => Ok(AcquireOutcome::Other(format!(
            "/claims/acquire body missing `result` discriminator: {body_text}"
        ))),
    }
}

/// Per-kind default TTL — must stay in sync with coord's `default_ttl_for`
/// at `claims.rs:119-128`. Used only as a fallback when coord's response
/// omits `ttl_seconds` (shouldn't happen on success but be defensive).
fn default_ttl_seconds_for(kind: &str) -> i64 {
    match kind {
        "phase" => 7200,
        "file_glob" => 90,
        "worktree" => 300,
        "branch_name" => 1800,
        "alembic_revision" => 600,
        "ci_wait" => 1800,
        _ => 300,
    }
}

/// Handle to the heartbeat task spawned for a claim. Dropping it
/// signals the task to exit on its next tick.
#[derive(Debug)]
pub struct ClaimHeartbeatHandle {
    cancel: Arc<Notify>,
    join: Option<tokio::task::JoinHandle<()>>,
    /// Snake-case ClaimKind — for the eventual release call.
    pub kind: String,
    pub resource_key: String,
    pub machine_id: uuid::Uuid,
    pub coord_http_base: String,
}

impl Drop for ClaimHeartbeatHandle {
    fn drop(&mut self) {
        self.cancel.notify_waiters();
        let _ = self.join.take();
    }
}

/// Spawn a tokio task that posts `/claims/heartbeat` every TTL/3 seconds.
/// On `HeartbeatResult::Stolen`, the task invokes `on_stolen` with the
/// optional `current_holder` and exits — the displaced agent's runner
/// is responsible for surfacing the banner from there.
///
/// The cancellation token returned via the handle's `Drop` is the
/// shutdown signal — the task exits on the next tick boundary.
pub fn spawn_heartbeat_task<F>(
    coord_http_base: String,
    machine_id: uuid::Uuid,
    kind: &str,
    resource_key: String,
    ttl_seconds: i64,
    on_stolen: F,
) -> ClaimHeartbeatHandle
where
    F: Fn(Option<String>) + Send + Sync + 'static,
{
    let cancel = Arc::new(Notify::new());
    let cancel_clone = cancel.clone();
    let kind_owned = kind.to_string();
    let resource_clone = resource_key.clone();
    let base_clone = coord_http_base.clone();

    let interval_secs: u64 = (ttl_seconds / 3).max(15) as u64;

    let join = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip the first tick (fires immediately) — we just acquired
        // the claim, no need to heartbeat in the same second.
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = cancel_clone.notified() => {
                    debug!(
                        "claim-heartbeat: stopping kind={kind_owned} key={resource_clone}"
                    );
                    return;
                }
            }
            match heartbeat_once(&base_clone, machine_id, &kind_owned, &resource_clone).await {
                Ok(HeartbeatTickOutcome::Ok) => {}
                Ok(HeartbeatTickOutcome::Stolen { current_holder }) => {
                    warn!(
                        "claim-heartbeat: stolen kind={kind_owned} key={resource_clone} \
                         current_holder={current_holder:?}"
                    );
                    on_stolen(current_holder);
                    return;
                }
                Err(e) => {
                    warn!(
                        "claim-heartbeat: tick failed kind={kind_owned} key={resource_clone}: {e}"
                    );
                }
            }
        }
    });

    ClaimHeartbeatHandle {
        cancel,
        join: Some(join),
        kind: kind.to_string(),
        resource_key,
        machine_id,
        coord_http_base,
    }
}

enum HeartbeatTickOutcome {
    Ok,
    Stolen { current_holder: Option<String> },
}

async fn heartbeat_once(
    coord_http_base: &str,
    machine_id: uuid::Uuid,
    kind: &str,
    resource_key: &str,
) -> Result<HeartbeatTickOutcome, String> {
    let url = format!("{}/claims/heartbeat", coord_http_base.trim_end_matches('/'));
    let body = serde_json::json!({
        "kind": kind,
        "resource_key": resource_key,
        "machine_id": machine_id.to_string(),
        "metadata": {},
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build heartbeat client: {e}"))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    let body_text = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "POST /claims/heartbeat {} — body: {body_text}",
            status.as_u16()
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(&body_text).map_err(|e| format!("parse heartbeat body: {e}"))?;
    match v.get("result").and_then(|r| r.as_str()) {
        Some("ok") => Ok(HeartbeatTickOutcome::Ok),
        Some("stolen") => {
            let h = v
                .get("current_holder")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            Ok(HeartbeatTickOutcome::Stolen { current_holder: h })
        }
        _ => Err(format!("heartbeat: unexpected body: {body_text}")),
    }
}

/// Best-effort release of a claim. Idempotent — returns `Ok(())` on
/// `Released` AND `NotHeld` outcomes, and on transport-level errors
/// (the caller is shutting down the spawn flow; observability matters
/// but correctness does not).
pub async fn release_claim_best_effort(
    coord_http_base: &str,
    machine_id: uuid::Uuid,
    kind: &str,
    resource_key: &str,
) {
    let url = format!("{}/claims/release", coord_http_base.trim_end_matches('/'));
    let body = serde_json::json!({
        "kind": kind,
        "resource_key": resource_key,
        "machine_id": machine_id.to_string(),
        "metadata": {},
    });
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("release_claim_best_effort: build client failed: {e}");
            return;
        }
    };
    match client.post(&url).json(&body).send().await {
        Ok(r) => {
            debug!(
                "release_claim_best_effort: kind={kind} key={resource_key} status={}",
                r.status().as_u16()
            );
        }
        Err(e) => {
            debug!("release_claim_best_effort: kind={kind} key={resource_key} err={e}");
        }
    }
}

/// Returns true iff the worktree-per-agent spawn mode is enabled.
/// Accepts the usual truthy values; anything else (including unset) is
/// false.
pub fn worktree_mode_enabled() -> bool {
    matches!(
        std::env::var(FLAG_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// A repo the caller wants a worktree for, paired with the commit the
/// worktree should branch off of. The runner is the host so it
/// resolves `parent_sha` from its own checkout — coord doesn't
/// re-resolve.
#[derive(Debug, Clone, Serialize)]
pub struct RepoRequest {
    pub repo: String,
    pub parent_sha: String,
}

/// A single materialized worktree as returned by `allocate_and_materialize`.
/// `worktree_path` is the actual on-disk path the runner created (matches
/// coord's `suggested_path` in the happy path, but the runner is allowed
/// to deviate — e.g. tighter disk).
#[derive(Debug, Clone, Serialize)]
pub struct MaterializedWorktree {
    pub repo: String,
    pub branch: String,
    pub parent_sha: String,
    pub worktree_path: PathBuf,
    /// Coordination Phase 5 / bottleneck Row 4 — the non-`heads`
    /// remote ref this worktree's branch pushes to
    /// (`refs/agent/<m>-<a>`). Source of truth is coord's allocate
    /// response; [`remote_agent_ref`] recomputes it as a fallback
    /// when talking to a pre-Phase-5 coord.
    pub push_ref: String,
}

/// Mirror of `qontinui-coord::ref_namespace::remote_agent_ref` — kept
/// in lockstep (no shared crate). Logical branch `agent/<m>-<a>` maps
/// to the non-`heads` remote ref `refs/agent/<m>-<a>` so the default
/// `+refs/heads/*:...` fetch refspec skips the ~10K agent refs at
/// fleet scale (Row 4). A non-agent branch maps verbatim under the
/// prefix (defensive — allocated agents always start with `agent/`).
pub fn remote_agent_ref(branch: &str) -> String {
    let rest = branch.strip_prefix("agent/").unwrap_or(branch);
    format!("refs/agent/{rest}")
}

/// Result of a full allocate + materialize round-trip.
///
/// Row 9 Phase 2 added `token`/`token_jti`/`token_exp`: the scoped
/// JWT coord issued at allocation. Phase 3's pusher daemon
/// (`crate::agent_pusher`) consumes these to authenticate
/// pushes to the coord-hosted git origin. Empty `token` (JWT keys
/// not configured on coord) means "skip pusher spawn for this
/// allocation."
///
/// Plan 2026-05-18-agent-spawn-coordination Phase 3 added
/// `active_claim`: when the spawn flow pre-acquired a coord claim via
/// `POST /claims/acquire`, this carries the claim's `(kind,
/// resource_key, ttl_seconds)` so callers can spawn the heartbeat
/// task and release on agent completion. `None` when no claim was
/// pre-acquired (legacy callers that don't pass plan_id/phase or
/// `declared_overlap_paths`).
#[derive(Debug, Clone, Serialize)]
pub struct AllocateResult {
    pub agent_id: String,
    pub worktrees: Vec<MaterializedWorktree>,
    pub token: String,
    pub token_jti: uuid::Uuid,
    pub token_exp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_claim: Option<ActiveClaim>,
}

/// Per-spawn active claim — the resource_key / kind / TTL acquired
/// before `/agents/allocate`. Surfaced via [`AllocateResult`] so the
/// caller can spawn a heartbeat task and release on agent completion.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveClaim {
    pub kind: String,
    pub resource_key: String,
    pub ttl_seconds: i64,
}

/// Coord's JSON response shape for `POST /agents/allocate`. Mirrored
/// here so we don't have to share a crate just for two structs.
#[derive(Debug, Deserialize)]
struct CoordAllocateResponse {
    agent_id: String,
    worktrees: Vec<CoordAllocatedWorktree>,
    /// Row 9 Phase 2 — scoped JWT covering all branches in this
    /// allocation. Empty when coord's JWT keys aren't configured
    /// (dev fallback).
    #[serde(default)]
    token: String,
    #[serde(default)]
    token_jti: Option<uuid::Uuid>,
    #[serde(default)]
    token_exp: Option<i64>,
    /// Coordination Phase 5 — per-agent out-of-tree Cargo path-dep
    /// override. Absent (pre-Phase-5 coord) or null (single-repo
    /// agent) → no override written.
    #[serde(default)]
    cargo_config: Option<CoordCargoConfig>,
}

#[derive(Debug, Deserialize)]
struct CoordCargoConfig {
    /// Absolute target path —
    /// `<COORD_WORKTREE_ROOT>/<agent_id>/.cargo/config.toml`.
    path: String,
    contents: String,
}

#[derive(Debug, Deserialize)]
struct CoordAllocatedWorktree {
    repo: String,
    branch: String,
    parent_sha: String,
    worktree_path: String,
    #[allow(dead_code)]
    status: String,
    /// Coordination Phase 5 — non-`heads` push ref. `#[serde(default)]`
    /// so a pre-Phase-5 coord (no field) still deserializes; the
    /// empty string triggers the [`remote_agent_ref`] fallback at
    /// materialization.
    #[serde(default)]
    push_ref: String,
}

/// Error variants returned by [`allocate_and_materialize`]. Distinct
/// from a plain `String` so the runner-side caller can pattern-match
/// the spawn-time claim conflict and surface a structured Tauri event
/// to the webview (per plan 2026-05-18-agent-spawn-coordination Phase 3).
#[derive(Debug, Clone)]
pub enum AllocateError {
    /// `POST /claims/acquire` returned `result: held`. The webview's
    /// ConflictModal listens for `agent-claim-conflict` events with
    /// this payload.
    ClaimConflict(ClaimConflict),
    /// Anything else — config errors, transport failures, coord 5xx,
    /// `git worktree add` failures, etc.
    Other(String),
}

impl std::fmt::Display for AllocateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AllocateError::ClaimConflict(c) => write!(
                f,
                "claim already held: kind={} resource_key={} current_holder={}",
                c.kind, c.resource_key, c.current_holder
            ),
            AllocateError::Other(s) => f.write_str(s),
        }
    }
}

impl From<String> for AllocateError {
    fn from(s: String) -> Self {
        AllocateError::Other(s)
    }
}

/// Call coord's `/agents/allocate` and then `git worktree add` for each
/// returned row.
///
/// `coord_http_base` is the HTTP base URL of qontinui-coord (e.g.
/// `http://localhost:9870`). The runner's profile stores `coord_url` as
/// a `ws://` URL — callers should convert via [`coord_ws_to_http`]
/// before calling here.
///
/// `repo_canonical_paths` maps `repo` slug to the canonical checkout
/// path on the runner's host. The runner's host typically holds one
/// canonical checkout per repo at `D:/qontinui-root/<repo>/`; this map
/// is the dependency injection point so tests can substitute scratch
/// repos.
///
/// Per plan 2026-05-18-agent-spawn-coordination Phase 3, when a
/// `ClaimSpawnContext` can be derived from the inputs (either
/// plan_id + phase, or non-empty `declared_overlap_paths`), this
/// function first calls coord's `POST /claims/acquire` with the
/// derived `(kind, resource_key)`. On `Held`, returns
/// `Err(AllocateError::ClaimConflict)` WITHOUT touching
/// `/agents/allocate`. On `Claimed`/`Renewed`, proceeds to allocate;
/// the caller is responsible for spawning a heartbeat task and
/// releasing the claim on agent completion (see [`spawn_heartbeat_task`]
/// and [`release_claim_best_effort`]). The returned [`AllocateResult`]
/// carries the active claim context for that purpose.
///
/// On success returns `AllocateResult`. On any non-claim error,
/// returns `Err(AllocateError::Other(String))`. Partial failure is
/// handled at the boundary: if any `git worktree add` fails after coord
/// has minted rows, the partial materialization stops; coord's sweeper
/// will eventually reclaim the unused rows once they age into
/// `abandoned`.
pub async fn allocate_and_materialize(
    coord_http_base: &str,
    machine_id: &uuid::Uuid,
    repos: &[RepoRequest],
    intent: Option<&str>,
    declared_overlap_paths: Option<&[String]>,
    repo_canonical_paths: &std::collections::HashMap<String, PathBuf>,
) -> Result<AllocateResult, AllocateError> {
    allocate_and_materialize_with_claim(
        coord_http_base,
        machine_id,
        repos,
        intent,
        declared_overlap_paths,
        repo_canonical_paths,
        None,
        None,
    )
    .await
}

/// Like [`allocate_and_materialize`] but accepts explicit `plan_id` /
/// `phase` strings for `ClaimKind::Phase` pre-flight. When both are
/// supplied, the claim shape is `phase / plan:<plan>:phase:<phase>`
/// per plan 2026-05-18-agent-spawn-coordination Phase 3.
pub async fn allocate_and_materialize_with_claim(
    coord_http_base: &str,
    machine_id: &uuid::Uuid,
    repos: &[RepoRequest],
    intent: Option<&str>,
    declared_overlap_paths: Option<&[String]>,
    repo_canonical_paths: &std::collections::HashMap<String, PathBuf>,
    plan_id: Option<&str>,
    phase: Option<&str>,
) -> Result<AllocateResult, AllocateError> {
    if !worktree_mode_enabled() {
        return Err(AllocateError::Other(format!(
            "{} is not enabled; spawn path is disabled",
            FLAG_ENV
        )));
    }
    if repos.is_empty() {
        return Err(AllocateError::Other("repos must not be empty".to_string()));
    }

    // Phase 3: pre-allocate `/claims/acquire`. When a claim context can
    // be derived from the inputs, call coord's atomic acquire-or-fail
    // BEFORE `/agents/allocate`. On `Held`, return ClaimConflict.
    let claim_ctx =
        ClaimSpawnContext::from_intent_and_paths(intent, plan_id, phase, declared_overlap_paths);
    let active_claim = if let Some(ref ctx) = claim_ctx {
        match pre_allocate_claim(coord_http_base, machine_id, ctx).await {
            Ok(AcquireOutcome::Acquired { ttl_seconds }) => {
                info!(
                    "claims/acquire ok kind={} key={} ttl={}s",
                    ctx.kind, ctx.resource_key, ttl_seconds
                );
                Some(ActiveClaim {
                    kind: ctx.kind.to_string(),
                    resource_key: ctx.resource_key.clone(),
                    ttl_seconds,
                })
            }
            Ok(AcquireOutcome::Held(conflict)) => {
                warn!(
                    "claims/acquire held kind={} key={} current_holder={}",
                    conflict.kind, conflict.resource_key, conflict.current_holder
                );
                return Err(AllocateError::ClaimConflict(conflict));
            }
            Ok(AcquireOutcome::Other(msg)) => {
                return Err(AllocateError::Other(msg));
            }
            Err(e) => return Err(AllocateError::Other(e)),
        }
    } else {
        None
    };

    // Pre-flight: every requested repo must have a canonical path the
    // runner can `git worktree add` from. Surface this before bothering
    // coord with the request.
    for r in repos {
        if !repo_canonical_paths.contains_key(&r.repo) {
            return Err(AllocateError::Other(format!(
                "no canonical checkout known for repo '{}' — pass it in \
                 repo_canonical_paths",
                r.repo
            )));
        }
    }

    // Phase 1B: declared_overlap_paths is optional; when present, coord
    // skips its LLM-based derivation step and uses our paths directly.
    // When absent, coord derives from `intent` (or falls back to empty).
    let body = serde_json::json!({
        "machine_id": machine_id.to_string(),
        "repos": repos.iter().map(|r| serde_json::json!({
            "repo": r.repo,
            "parent_sha": r.parent_sha,
        })).collect::<Vec<_>>(),
        "intent": intent,
        "declared_overlap_paths": declared_overlap_paths,
    });

    let url = format!("{}/agents/allocate", coord_http_base.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| AllocateError::Other(format!("POST {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        return Err(AllocateError::Other(format!(
            "POST {url} returned {} — body: {}",
            status.as_u16(),
            body_text
        )));
    }
    let coord_resp: CoordAllocateResponse = resp
        .json()
        .await
        .map_err(|e| AllocateError::Other(format!("decode coord response: {e}")))?;

    info!(
        "coord allocated agent_id={} repos={}",
        coord_resp.agent_id,
        coord_resp.worktrees.len()
    );

    let mut materialized: Vec<MaterializedWorktree> = Vec::with_capacity(repos.len());
    for w in coord_resp.worktrees {
        let canonical = repo_canonical_paths.get(&w.repo).ok_or_else(|| {
            AllocateError::Other(format!("missing canonical path for repo '{}'", w.repo))
        })?;
        let target = PathBuf::from(&w.worktree_path);

        // Ensure the parent dir exists. `git worktree add` will create
        // the leaf, but the parent (`D:/qontinui-root.wt/<agent>/`)
        // doesn't exist on first allocation.
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AllocateError::Other(format!(
                    "create parent dir {} for worktree {}: {}",
                    parent.display(),
                    w.repo,
                    e
                ))
            })?;
        }

        // `git -C <canonical> worktree add <target> -b <branch> <parent_sha>`.
        // Plan §4.1 step 4 spelled this exact command.
        let target_str = target.to_string_lossy().to_string();
        let args: [&str; 6] = [
            "worktree",
            "add",
            &target_str,
            "-b",
            &w.branch,
            &w.parent_sha,
        ];
        match run_git_command(canonical, &args) {
            Ok(stdout) => {
                info!(
                    "git worktree add ok: repo={} branch={} path={} stdout={}",
                    w.repo,
                    w.branch,
                    target.display(),
                    stdout.trim()
                );
            }
            Err(e) => {
                warn!(
                    "git worktree add failed: repo={} branch={} path={}: {}",
                    w.repo,
                    w.branch,
                    target.display(),
                    e
                );
                return Err(AllocateError::Other(format!(
                    "git worktree add for repo '{}' (branch {}) failed: {}",
                    w.repo, w.branch, e
                )));
            }
        }

        let push_ref = if w.push_ref.is_empty() {
            remote_agent_ref(&w.branch)
        } else {
            w.push_ref
        };
        materialized.push(MaterializedWorktree {
            repo: w.repo,
            branch: w.branch,
            parent_sha: w.parent_sha,
            worktree_path: target,
            push_ref,
        });
    }

    // Coordination Phase 5 / Row 5 — write coord's per-agent Cargo
    // path-dep override. It lands at
    // <COORD_WORKTREE_ROOT>/<agent_id>/.cargo/config.toml, i.e. the
    // PARENT of the per-repo worktree dirs — outside every git repo,
    // so it never merges and pre-commit cargo hooks never see it
    // (feedback_worktree_path_dep_hooks). Best-effort: a write
    // failure logs + continues (the agent then builds against the
    // canonical sibling, i.e. today's non-deterministic behaviour —
    // degraded, not broken).
    if let Some(cc) = &coord_resp.cargo_config {
        if let Some(parent) = std::path::Path::new(&cc.path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(
                    "phase5 cargo override: mkdir {} failed: {e}",
                    parent.display()
                );
            }
        }
        match std::fs::write(&cc.path, &cc.contents) {
            Ok(()) => info!(
                "phase5 cargo override written: {} ({} bytes)",
                cc.path,
                cc.contents.len()
            ),
            Err(e) => warn!("phase5 cargo override: write {} failed: {e}", cc.path),
        }
    }

    Ok(AllocateResult {
        agent_id: coord_resp.agent_id,
        worktrees: materialized,
        token: coord_resp.token,
        token_jti: coord_resp.token_jti.unwrap_or(uuid::Uuid::nil()),
        token_exp: coord_resp.token_exp.unwrap_or(0),
        active_claim,
    })
}

/// Convert a `ws://` or `wss://` coord URL into the matching HTTP base.
/// Profiles store `coord_url` as a WebSocket URL because the `/ws`
/// endpoint is the dominant runner-side use case; HTTP callers (this
/// module, build_events POSTs, fleet-health panel) flip the scheme AND
/// strip the trailing `/ws` so paths like `/coord/fleet/health` don't
/// get appended onto the WS upgrade path (which JWT-rejects with 401).
/// Mirrors the supervisor's resolver at
/// `qontinui-supervisor/src/fleet.rs::coord_http_base`.
pub fn coord_ws_to_http(coord_url: &str) -> String {
    let trimmed = coord_url.trim_end_matches('/').trim_end_matches("/ws");
    if let Some(rest) = trimmed.strip_prefix("ws://") {
        format!("http://{}", rest)
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        format!("https://{}", rest)
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_off_by_default() {
        // Tests run with no env var set unless tested explicitly. We
        // don't mutate process env here because the runner test
        // harness mutates env globally (memory
        // `feedback_env_var_tests_serialize`); this assertion holds
        // as long as no test sets QONTINUI_AGENT_WORKTREE_MODE before
        // this runs.
        if std::env::var(FLAG_ENV).is_err() {
            assert!(!worktree_mode_enabled());
        }
    }

    #[test]
    fn coord_ws_to_http_swaps_scheme() {
        assert_eq!(coord_ws_to_http("ws://h:9870"), "http://h:9870");
        assert_eq!(coord_ws_to_http("wss://h:9870"), "https://h:9870");
        assert_eq!(coord_ws_to_http("http://h:9870"), "http://h:9870");
        assert_eq!(coord_ws_to_http("https://h:9870"), "https://h:9870");
    }

    #[test]
    fn coord_ws_to_http_strips_ws_suffix() {
        // The dominant profile shape is `ws://host:port/ws` — the
        // resolver MUST strip `/ws` so callers building
        // `format!("{base}/coord/fleet/health")` don't end up at
        // `/ws/coord/fleet/health` (the WS upgrade path, which 401s).
        assert_eq!(coord_ws_to_http("ws://h:9870/ws"), "http://h:9870");
        assert_eq!(coord_ws_to_http("wss://h:9870/ws"), "https://h:9870");
        assert_eq!(coord_ws_to_http("http://h:9870/ws"), "http://h:9870");
        // Trailing-slash tolerance: `ws://h:9870/ws/` collapses too.
        assert_eq!(coord_ws_to_http("ws://h:9870/ws/"), "http://h:9870");
        // Bare trailing slash (no `/ws`) is also stripped so callers
        // don't double-slash when appending a path.
        assert_eq!(coord_ws_to_http("ws://h:9870/"), "http://h:9870");
    }

    #[test]
    fn remote_agent_ref_mirrors_coord_mapping() {
        // Must stay byte-identical to
        // qontinui-coord::ref_namespace::remote_agent_ref.
        assert_eq!(remote_agent_ref("agent/m12-a34"), "refs/agent/m12-a34");
        // No redundant agent/agent/ segment.
        assert!(!remote_agent_ref("agent/x-y").contains("agent/agent"));
        // Result is outside refs/heads/* — the whole Row 4 point.
        assert!(!remote_agent_ref("agent/x-y").starts_with("refs/heads/"));
        assert_eq!(remote_agent_ref("weird"), "refs/agent/weird");
    }
}
