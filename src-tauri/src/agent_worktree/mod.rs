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
//! - Call coord `/agents/allocate` with `{ device_id, repos: [{repo,
//!   parent_sha}], intent? }`. The body's `device_id` key matches
//!   coord's `AllocateRequest.device_id` field
//!   (`qontinui-coord/src/agent_worktrees.rs:77`) after the
//!   `coord.machines → coord.devices` rename. The runner's local
//!   variable is still `machine_id` because the rest of the runner
//!   (claims acquire/heartbeat/release, JWT issuance) hasn't been
//!   renamed; only the on-the-wire JSON body to `/agents/allocate`
//!   needs the new key.
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

pub mod canonical_paths;
pub mod census;
pub mod edit_effect_loop;
pub mod fs_observer;
pub mod isolated_edit;
pub mod reclaim;

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

/// Pre-allocate claim parameters passed into
/// [`allocate_and_materialize_with_claim`].
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

/// Normalize a canonical checkout path into the exact `resource_key` form
/// the coord-guard derives for a `kind=worktree` claim: forward slashes,
/// lowercase (so a `D:/` drive letter and a `d:/` one collide), and no
/// trailing slash. The guard observes the form `d:/qontinui-root/<repo>`.
///
/// This MUST stay byte-identical to whatever the guard produces when it
/// looks up `GET /coord/claims/by-resource?kind=worktree&key=<path>`. The
/// guard runs its own `realpath`-style resolution which can emit a
/// `/d/qontinui-root/...` (MSYS) shape; the contract test below pins the
/// Windows-native form so a guard-side divergence surfaces as a failing
/// contract test rather than a silent never-matching claim in prod.
pub fn worktree_resource_key(canonical: &std::path::Path) -> String {
    canonical
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

impl ClaimSpawnContext {
    /// Build a `kind=worktree` claim context for one canonical checkout
    /// path. The `resource_key` is normalized via [`worktree_resource_key`]
    /// so it matches the coord-guard's `by-resource` lookup byte-for-byte.
    ///
    /// Unlike [`Self::from_intent_and_paths`] this is unconditional — a
    /// session always claims the worktree(s) it materializes regardless of
    /// whether a plan_id/phase/file_glob was declared. This closes the
    /// gate-continuation gap where worktrees materialized with NO claim at
    /// all (intent/plan_id/phase/declared_overlap_paths all `None`).
    pub fn worktree_for_path(canonical: &std::path::Path, intent: Option<&str>) -> Self {
        Self {
            kind: "worktree",
            resource_key: worktree_resource_key(canonical),
            intent: intent.map(|s| s.to_string()),
            plan_id: None,
            phase: None,
        }
    }

    /// Ξ_Worktree Phase 7.5b — build a `kind=canonical_checkout` claim
    /// context for one repo. coord's Rule-2 precondition P3 probe
    /// (`load_canonical_lease_claim` in `agent_worktrees.rs`) looks up a
    /// `ClaimKind::CanonicalCheckout` holder on `resource_key = "repo:<repo>"`
    /// (the bare repo slug, NOT a filesystem path) and serializes the
    /// shared-branch grant when a DIFFERENT machine holds it. coord's
    /// `normalize_resource_key` is a PASS-THROUGH for non-`Worktree` kinds, so
    /// the key must be the verbatim `repo:<repo>` form — no path canonicalization.
    /// When the runner honors a `SharedBranch` decision it acquires this lease
    /// per repo so the P3 probe observes the live holder on the next decision.
    pub fn canonical_checkout_for_repo(repo: &str, intent: Option<&str>) -> Self {
        Self {
            kind: "canonical_checkout",
            resource_key: format!("repo:{repo}"),
            intent: intent.map(|s| s.to_string()),
            plan_id: None,
            phase: None,
        }
    }

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

/// Build the JSON body shared by coord's
/// `/claims/{acquire,heartbeat,release}` endpoints.
///
/// `agent_session_id` is ALWAYS emitted — as a JSON string when present,
/// as `null` when absent. It is the per-session discriminator coord folds
/// into the claim **owner token** (`machine:session`) so two agents on a
/// single machine are distinct holders rather than silently renewing each
/// other's claim (plan 2026-06-03-coord-session-scoped-claim-owner).
/// Machine-level acquirers (daemons, the merge scheduler) pass `None`,
/// which coord maps to the legacy bare-machine owner token.
fn claim_request_body(
    kind: &str,
    resource_key: &str,
    machine_id: &uuid::Uuid,
    agent_session_id: Option<uuid::Uuid>,
    metadata: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "resource_key": resource_key,
        "machine_id": machine_id.to_string(),
        "agent_session_id": agent_session_id,
        "metadata": metadata,
    })
}

async fn pre_allocate_claim(
    coord_http_base: &str,
    machine_id: &uuid::Uuid,
    agent_session_id: Option<uuid::Uuid>,
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
    let body = claim_request_body(
        ctx.kind,
        &ctx.resource_key,
        machine_id,
        agent_session_id,
        metadata,
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build claim http client: {e}"))?;
    let resp = crate::auth::attach_device_auth(client.post(&url).json(&body))
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

/// Acquire ONE claim from a [`ClaimSpawnContext`] and map the outcome into
/// an [`ActiveClaim`] (on `Acquired`) or an [`AllocateError`] (on `Held` →
/// `ClaimConflict`, on `Other`/transport → `Other`). Shared by both the
/// pre-allocate phase/file_glob claim and the per-repo `kind=worktree`
/// claims (Phase 1) so the acquire-and-map logic lives in one place.
async fn acquire_one_claim(
    coord_http_base: &str,
    machine_id: &uuid::Uuid,
    agent_session_id: Option<uuid::Uuid>,
    ctx: &ClaimSpawnContext,
) -> Result<ActiveClaim, AllocateError> {
    match pre_allocate_claim(coord_http_base, machine_id, agent_session_id, ctx).await {
        Ok(AcquireOutcome::Acquired { ttl_seconds }) => {
            info!(
                "claims/acquire ok kind={} key={} ttl={}s",
                ctx.kind, ctx.resource_key, ttl_seconds
            );
            Ok(ActiveClaim {
                kind: ctx.kind.to_string(),
                resource_key: ctx.resource_key.clone(),
                ttl_seconds,
                agent_session_id,
            })
        }
        Ok(AcquireOutcome::Held(conflict)) => {
            warn!(
                "claims/acquire held kind={} key={} current_holder={}",
                conflict.kind, conflict.resource_key, conflict.current_holder
            );
            Err(AllocateError::ClaimConflict(conflict))
        }
        Ok(AcquireOutcome::Other(msg)) => Err(AllocateError::Other(msg)),
        Err(e) => Err(AllocateError::Other(e)),
    }
}

/// Release every claim in `claims` best-effort (used to unwind a partial
/// multi-claim acquire when a later claim is `Held`, and on the `Wait`
/// teardown path). Each release matches the owner token its acquire used.
async fn release_all_claims_best_effort(
    coord_http_base: &str,
    machine_id: &uuid::Uuid,
    claims: &[ActiveClaim],
) {
    for c in claims {
        release_claim_best_effort(
            coord_http_base,
            *machine_id,
            c.agent_session_id,
            &c.kind,
            &c.resource_key,
        )
        .await;
    }
}

/// Ξ_Worktree Phase 7.5b — outcome of planning the per-repo
/// `canonical_checkout` lease acquire for a [`Isolation::SharedBranch`]
/// honor. Pure decision logic, no I/O: given the acquire outcome for each
/// repo in turn it decides whether the honor proceeds (all leases acquired,
/// appended to the session's `active_claims`) or bails (a peer holds one →
/// the partial set must be released and an [`AllocateError`] returned).
///
/// Extracted so the honor's serialize-or-bail contract is unit-testable
/// WITHOUT a live coord: the production loop performs the same per-repo
/// acquire and the same unwind-on-`Held`; this models that exact shape over
/// pre-computed per-repo outcomes.
#[derive(Debug)]
enum CanonicalLeasePlan {
    /// Every repo's lease acquired — append these to `active_claims` and
    /// proceed with the branch switch.
    Proceed { acquired: Vec<ActiveClaim> },
    /// A repo's lease was `Held`/errored — release `to_release` (the leases
    /// acquired BEFORE the failing repo) and bail with `error`.
    Bail {
        to_release: Vec<ActiveClaim>,
        error: AllocateError,
    },
}

/// Pure planner mirroring the production canonical-lease acquire loop: walk
/// the per-repo acquire outcomes in order, accumulating successes; the first
/// `Err` stops the walk and yields a [`CanonicalLeasePlan::Bail`] carrying the
/// successes-so-far (to release) and the error. All-`Ok` yields
/// [`CanonicalLeasePlan::Proceed`].
fn plan_canonical_leases(outcomes: Vec<Result<ActiveClaim, AllocateError>>) -> CanonicalLeasePlan {
    let mut acquired: Vec<ActiveClaim> = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        match outcome {
            Ok(claim) => acquired.push(claim),
            Err(error) => {
                return CanonicalLeasePlan::Bail {
                    to_release: acquired,
                    error,
                };
            }
        }
    }
    CanonicalLeasePlan::Proceed { acquired }
}

/// Per-kind default TTL — must stay in sync with coord's `default_ttl_for`
/// at `claims.rs:119-128`. Used only as a fallback when coord's response
/// omits `ttl_seconds` (shouldn't happen on success but be defensive).
fn default_ttl_seconds_for(kind: &str) -> i64 {
    match kind {
        "phase" => 7200,
        "file_glob" => 90,
        "worktree" => 300,
        "canonical_checkout" => 300,
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
    /// Same value carried so the release POST matches the acquire's owner token.
    pub agent_session_id: Option<uuid::Uuid>,
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
    agent_session_id: Option<uuid::Uuid>,
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
            match heartbeat_once(
                &base_clone,
                machine_id,
                agent_session_id,
                &kind_owned,
                &resource_clone,
            )
            .await
            {
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
        agent_session_id,
    }
}

enum HeartbeatTickOutcome {
    Ok,
    Stolen { current_holder: Option<String> },
}

async fn heartbeat_once(
    coord_http_base: &str,
    machine_id: uuid::Uuid,
    agent_session_id: Option<uuid::Uuid>,
    kind: &str,
    resource_key: &str,
) -> Result<HeartbeatTickOutcome, String> {
    let url = format!("{}/claims/heartbeat", coord_http_base.trim_end_matches('/'));
    let body = claim_request_body(
        kind,
        resource_key,
        &machine_id,
        agent_session_id,
        serde_json::json!({}),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build heartbeat client: {e}"))?;
    let resp = crate::auth::attach_device_auth(client.post(&url).json(&body))
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
    agent_session_id: Option<uuid::Uuid>,
    kind: &str,
    resource_key: &str,
) {
    let url = format!("{}/claims/release", coord_http_base.trim_end_matches('/'));
    let body = claim_request_body(
        kind,
        resource_key,
        &machine_id,
        agent_session_id,
        serde_json::json!({}),
    );
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
    match crate::auth::attach_device_auth(client.post(&url).json(&body))
        .send()
        .await
    {
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

/// A single materialized worktree as returned by
/// `allocate_and_materialize_with_claim`.
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
/// Plan 2026-05-18-agent-spawn-coordination Phase 3 added the spawn-time
/// claim; plan 2026-06-06-session-scoped-multi-repo-workspace-coordination
/// Phase 1 widened it from a single claim to a collection:
/// `active_claims` carries EVERY claim the spawn flow pre-acquired —
/// one `kind=worktree` claim per materialized repo (always), plus the
/// optional `phase`/`file_glob` claim as just one more entry when the
/// caller declared a plan_id/phase or `declared_overlap_paths`. Callers
/// spawn one heartbeat task per entry and release every entry on agent
/// completion. Empty when no claim was acquired (worktree mode off, or
/// the no-op test builders).
#[derive(Debug, Clone, Serialize)]
pub struct AllocateResult {
    pub agent_id: String,
    pub worktrees: Vec<MaterializedWorktree>,
    pub token: String,
    pub token_jti: uuid::Uuid,
    pub token_exp: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub active_claims: Vec<ActiveClaim>,
}

/// Per-spawn active claim — the resource_key / kind / TTL acquired
/// before `/agents/allocate`. Surfaced via [`AllocateResult`] so the
/// caller can spawn a heartbeat task and release on agent completion.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveClaim {
    pub kind: String,
    pub resource_key: String,
    pub ttl_seconds: i64,
    /// coord-assigned per-agent session id; folded into the claim owner
    /// token so two agents on one machine are distinct holders. Sent
    /// identically on acquire/heartbeat/release. Plan
    /// 2026-06-03-coord-session-scoped-claim-owner follow-up.
    pub agent_session_id: Option<uuid::Uuid>,
}

// =============================================================================
// Phase 3 — allocate-response `isolation` directive.
// =============================================================================

/// coord's per-allocation isolation directive (Ξ_Worktree Phase 3). The
/// runner honors whichever mode coord picks; an absent or unknown
/// `isolation` field on the response defaults to
/// [`Isolation::Worktree`] with [`TargetMode::Junctioned`] — today's
/// behavior — so an older coord (no field) keeps working unchanged.
///
/// Wire shape (serde-tagged by `mode`):
/// ```json
/// {"mode":"worktree","target":"junctioned"}
/// {"mode":"worktree","target":"dedicated"}
/// {"mode":"shared_branch"}
/// {"mode":"wait","reason":"...","blocking":"...","retry_when":"..."}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Isolation {
    /// Materialize a `git worktree add`. `target` controls whether the
    /// build `target` dir is junctioned to the canonical tree (today's
    /// default) or left as a real dedicated dir.
    Worktree {
        #[serde(default)]
        target: TargetMode,
    },
    /// Do NOT create a worktree — the agent works on a feature branch in
    /// the canonical checkout.
    SharedBranch,
    /// Do NOT materialize anything; surface the wait reason to the caller
    /// (e.g. a held upstream resource). The runner does not spin — it
    /// returns this as a typed outcome the caller logs / retries on.
    Wait {
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        blocking: Option<String>,
        #[serde(default)]
        retry_when: Option<String>,
    },
}

impl Default for Isolation {
    fn default() -> Self {
        // Back-compat: a response with no `isolation` field (older coord)
        // deserializes the `Option<Isolation>` as `None`; the caller maps
        // that to this default — a junctioned worktree, today's behavior.
        Isolation::Worktree {
            target: TargetMode::Junctioned,
        }
    }
}

/// Build-`target` junction policy for [`Isolation::Worktree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TargetMode {
    /// Junction the build `target` dir into the worktree from the
    /// canonical tree (shared compiled artifacts). Today's default.
    #[default]
    Junctioned,
    /// Leave the build `target` dir as a real, dedicated dir for this
    /// worktree (no `target` junction step). `node_modules` may still
    /// junction per the runner's default.
    Dedicated,
}

/// Outcome of a full allocate + materialize round-trip. Widened (Phase 3)
/// from a bare [`AllocateResult`] so coord's `isolation` directive can
/// steer the runner into a non-worktree materialization without a silent
/// fallthrough.
///
/// - [`MaterializeOutcome::Worktrees`] — coord chose `worktree` (junctioned
///   or dedicated target); carries the same [`AllocateResult`] the legacy
///   flow returned, so the heartbeat / daemon / pusher wiring is unchanged.
/// - [`MaterializeOutcome::SharedBranch`] — coord chose `shared_branch`;
///   the runner created / checked out the feature branch in the canonical
///   checkout. The materialized `path` is the canonical checkout itself.
/// - [`MaterializeOutcome::Wait`] — coord chose `wait`; nothing was
///   materialized. The caller logs and retries later; it MUST NOT spin.
#[derive(Debug, Clone, Serialize)]
pub enum MaterializeOutcome {
    Worktrees(AllocateResult),
    SharedBranch(SharedBranchResult),
    Wait(WaitOutcome),
}

/// Result of an [`Isolation::SharedBranch`] materialization: a real
/// feature branch checked out in the canonical checkout (NOT a worktree).
#[derive(Debug, Clone, Serialize)]
pub struct SharedBranchResult {
    pub agent_id: String,
    /// Per-repo branches created/checked out in the canonical checkout.
    pub branches: Vec<SharedBranchRepo>,
    pub token: String,
    pub token_jti: uuid::Uuid,
    pub token_exp: i64,
    /// Every pre-acquired claim for this shared-branch allocation — one
    /// `kind=worktree` claim per repo (keyed on the canonical checkout the
    /// branch lives in) plus the optional phase/file_glob claim. Converted
    /// in lockstep with [`AllocateResult::active_claims`] (Phase 1) so the
    /// shared-branch path is never single-claim.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub active_claims: Vec<ActiveClaim>,
}

/// One repo's shared-branch checkout.
#[derive(Debug, Clone, Serialize)]
pub struct SharedBranchRepo {
    pub repo: String,
    pub branch: String,
    pub parent_sha: String,
    /// The canonical checkout path the branch was created in (the agent's
    /// cwd is this path, NOT a separate worktree dir).
    pub checkout_path: PathBuf,
    pub push_ref: String,
}

/// Result of an [`Isolation::Wait`] directive — nothing materialized.
#[derive(Debug, Clone, Serialize)]
pub struct WaitOutcome {
    pub agent_id: String,
    pub reason: Option<String>,
    pub blocking: Option<String>,
    pub retry_when: Option<String>,
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
    /// Ξ_Worktree Phase 3 — per-allocation isolation directive. Absent
    /// (older coord) → `None`, which the materializer maps to
    /// [`Isolation::default`] (junctioned worktree, today's behavior).
    #[serde(default)]
    isolation: Option<Isolation>,
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

/// Error variants returned by [`allocate_and_materialize_with_claim`]. Distinct
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
/// `agent_session_id` is the per-session claim-owner discriminator
/// threaded onto acquire/heartbeat/release (see [`claim_request_body`]).
/// `plan_id` / `phase`, when both present, select `ClaimKind::Phase` with
/// `resource_key = "plan:<plan>:phase:<phase>"`; when only
/// `declared_overlap_paths` is present the claim kind is `FileGlob`.
///
/// Claim acquisition (Phase 1, plan
/// 2026-06-06-session-scoped-multi-repo-workspace-coordination). BEFORE
/// `/agents/allocate` and before any `git worktree add`, this acquires:
///   1. One `kind=worktree` claim per repo, keyed on the canonical
///      checkout path normalized to the guard's `by-resource` form
///      (always — even when no plan_id/phase/paths were declared, which is
///      the gate-continuation case the old code left unclaimed).
///   2. The optional `phase`/`file_glob` claim (Phase 3 behavior) when a
///      plan_id + phase or non-empty `declared_overlap_paths` was supplied.
/// A `Held` on ANY claim unwinds the claims already acquired and returns
/// `Err(AllocateError::ClaimConflict)` WITHOUT touching `/agents/allocate`.
/// On full success it proceeds to allocate; the caller spawns one heartbeat
/// task per claim and releases every claim on agent completion (see
/// [`spawn_heartbeat_task`] and [`release_claim_best_effort`]). The returned
/// [`AllocateResult::active_claims`] carries them all for that purpose.
///
/// On success returns `AllocateResult`. On any non-claim error,
/// returns `Err(AllocateError::Other(String))`. Partial failure is
/// handled at the boundary: if any `git worktree add` fails after coord
/// has minted rows, the partial materialization stops; coord's sweeper
/// will eventually reclaim the unused rows once they age into
/// `abandoned`.
pub async fn allocate_and_materialize_with_claim(
    coord_http_base: &str,
    machine_id: &uuid::Uuid,
    agent_session_id: Option<uuid::Uuid>,
    repos: &[RepoRequest],
    intent: Option<&str>,
    declared_overlap_paths: Option<&[String]>,
    repo_canonical_paths: &std::collections::HashMap<String, PathBuf>,
    plan_id: Option<&str>,
    phase: Option<&str>,
) -> Result<MaterializeOutcome, AllocateError> {
    if !worktree_mode_enabled() {
        return Err(AllocateError::Other(format!(
            "{} is not enabled; spawn path is disabled",
            FLAG_ENV
        )));
    }
    if repos.is_empty() {
        return Err(AllocateError::Other("repos must not be empty".to_string()));
    }

    // Pre-flight: every requested repo must have a canonical path the
    // runner can `git worktree add` from. Surface this BEFORE acquiring any
    // claim (canonical paths are also the source of the worktree claim
    // keys, so they must resolve first).
    for r in repos {
        if !repo_canonical_paths.contains_key(&r.repo) {
            return Err(AllocateError::Other(format!(
                "no canonical checkout known for repo '{}' — pass it in \
                 repo_canonical_paths",
                r.repo
            )));
        }
    }

    // Phase 1 (plan 2026-06-06-session-scoped-multi-repo-workspace-coordination):
    // acquire the per-repo `kind=worktree` claims BEFORE materializing any
    // worktree — fail-fast, mirroring the original `pre_allocate_claim`
    // placement. The claim key is the canonical checkout path normalized to
    // the guard's `by-resource` form (see `worktree_resource_key`), so a
    // gate-continuation (which declares no plan_id/phase/paths) still claims
    // every checkout it touches. A `Held` on ANY repo means another session
    // owns that checkout → unwind the claims already acquired and return the
    // ClaimConflict so the caller surfaces it instead of clobbering.
    //
    // Then acquire the OPTIONAL phase/file_glob claim (when a plan_id/phase
    // or `declared_overlap_paths` was declared) as just one more entry.
    let mut active_claims: Vec<ActiveClaim> = Vec::with_capacity(repos.len() + 1);

    for r in repos {
        let canonical = repo_canonical_paths
            .get(&r.repo)
            .expect("canonical path presence checked above");
        let ctx = ClaimSpawnContext::worktree_for_path(canonical, intent);
        match acquire_one_claim(coord_http_base, machine_id, agent_session_id, &ctx).await {
            Ok(claim) => active_claims.push(claim),
            Err(e) => {
                // Unwind every worktree claim we acquired before this one so
                // a partial acquire doesn't strand held checkouts.
                release_all_claims_best_effort(coord_http_base, machine_id, &active_claims).await;
                return Err(e);
            }
        }
    }

    let claim_ctx =
        ClaimSpawnContext::from_intent_and_paths(intent, plan_id, phase, declared_overlap_paths);
    if let Some(ref ctx) = claim_ctx {
        match acquire_one_claim(coord_http_base, machine_id, agent_session_id, ctx).await {
            Ok(claim) => active_claims.push(claim),
            Err(e) => {
                release_all_claims_best_effort(coord_http_base, machine_id, &active_claims).await;
                return Err(e);
            }
        }
    }

    // Phase 1B: declared_overlap_paths is optional; when present, coord
    // skips its LLM-based derivation step and uses our paths directly.
    // When absent, coord derives from `intent` (or falls back to empty).
    //
    // Body key is `device_id` to match coord's `AllocateRequest.device_id`
    // (`qontinui-coord/src/agent_worktrees.rs:77`) after the
    // `coord.machines → coord.devices` rename. The runner-side variable
    // stays `machine_id` for symmetry with claims acquire/heartbeat/release,
    // which coord's `claims.rs` still expects as `machine_id`.
    let body = serde_json::json!({
        "device_id": machine_id.to_string(),
        "repos": repos.iter().map(|r| serde_json::json!({
            "repo": r.repo,
            "parent_sha": r.parent_sha,
        })).collect::<Vec<_>>(),
        "intent": intent,
        "declared_overlap_paths": declared_overlap_paths,
    });

    let url = format!("{}/agents/allocate", coord_http_base.trim_end_matches('/'));
    let resp = crate::auth::attach_device_auth(reqwest::Client::new().post(&url).json(&body))
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

    // Phase 3 — honor coord's `isolation` directive. Absent (older
    // coord) → the junctioned-worktree default (today's behavior).
    let isolation = coord_resp.isolation.clone().unwrap_or_default();
    info!(
        "coord allocated agent_id={} repos={} isolation={:?}",
        coord_resp.agent_id,
        coord_resp.worktrees.len(),
        isolation
    );

    // `wait` — coord declined to materialize. Surface a typed Wait
    // outcome WITHOUT touching the disk; the caller logs / retries. We do
    // NOT spin here.
    if let Isolation::Wait {
        reason,
        blocking,
        retry_when,
    } = isolation
    {
        info!(
            "allocate: coord returned wait agent_id={} reason={:?} blocking={:?} retry_when={:?}",
            coord_resp.agent_id, reason, blocking, retry_when
        );
        // Pre-acquired claims must not linger while we wait — release every
        // one so no held resource (worktree checkout or phase/file_glob) is
        // pinned by an agent that never materialized.
        release_all_claims_best_effort(coord_http_base, machine_id, &active_claims).await;
        return Ok(MaterializeOutcome::Wait(WaitOutcome {
            agent_id: coord_resp.agent_id,
            reason,
            blocking,
            retry_when,
        }));
    }

    // `shared_branch` — do NOT create a worktree. Create / check out the
    // feature branch in the canonical checkout and return the canonical
    // path as the materialized cwd. A real branch, never a silent
    // worktree fallthrough.
    if matches!(isolation, Isolation::SharedBranch) {
        // Ξ_Worktree Phase 7.5b — BEFORE switching any canonical checkout's
        // branch, acquire an exclusive `canonical_checkout` lease per repo so
        // coord's already-deployed Rule-2 P3 probe (`load_canonical_lease_claim`,
        // resource_key `repo:<repo>`, kind `canonical_checkout`, TTL 300s) sees a
        // live holder on the next isolation decision and serializes any peer's
        // shared-branch grant for the same canonical tree. The lease is pushed
        // into `active_claims` so it RELEASES on the existing session-end drain
        // (the caller spawns one heartbeat per entry and releases every entry on
        // agent completion) — no separate release path. A `Held` by another owner
        // means a peer already owns this canonical tree → unwind every claim
        // acquired so far and bail with an AllocateError so coord re-decides as
        // an isolated Worktree on retry; we never proceed to switch the branch.
        let mut lease_outcomes: Vec<Result<ActiveClaim, AllocateError>> =
            Vec::with_capacity(coord_resp.worktrees.len());
        for w in &coord_resp.worktrees {
            let ctx = ClaimSpawnContext::canonical_checkout_for_repo(&w.repo, intent);
            let outcome =
                acquire_one_claim(coord_http_base, machine_id, agent_session_id, &ctx).await;
            // Stop at the first failure — never acquire a lease for a later repo
            // once an earlier one is Held (it would leak past the unwind, which
            // only releases up to the failing repo).
            let failed = outcome.is_err();
            lease_outcomes.push(outcome);
            if failed {
                break;
            }
        }
        match plan_canonical_leases(lease_outcomes) {
            CanonicalLeasePlan::Proceed { mut acquired } => {
                // Append the leases to the session's release-tracked collection
                // so they release on the existing session-end drain.
                active_claims.append(&mut acquired);
            }
            CanonicalLeasePlan::Bail { to_release, error } => {
                // Unwind the canonical leases acquired before the held repo,
                // plus every worktree/phase claim already in `active_claims`,
                // then bail so coord re-decides as an isolated Worktree on retry.
                active_claims.extend(to_release);
                release_all_claims_best_effort(coord_http_base, machine_id, &active_claims).await;
                return Err(error);
            }
        }
        let mut branches: Vec<SharedBranchRepo> = Vec::with_capacity(repos.len());
        for w in &coord_resp.worktrees {
            let canonical = repo_canonical_paths.get(&w.repo).ok_or_else(|| {
                AllocateError::Other(format!("missing canonical path for repo '{}'", w.repo))
            })?;
            checkout_shared_branch(canonical, &w.branch, &w.parent_sha).map_err(|e| {
                AllocateError::Other(format!(
                    "shared_branch checkout for repo '{}' (branch {}) failed: {}",
                    w.repo, w.branch, e
                ))
            })?;
            let push_ref = if w.push_ref.is_empty() {
                remote_agent_ref(&w.branch)
            } else {
                w.push_ref.clone()
            };
            branches.push(SharedBranchRepo {
                repo: w.repo.clone(),
                branch: w.branch.clone(),
                parent_sha: w.parent_sha.clone(),
                checkout_path: canonical.clone(),
                push_ref,
            });
        }
        return Ok(MaterializeOutcome::SharedBranch(SharedBranchResult {
            agent_id: coord_resp.agent_id,
            branches,
            token: coord_resp.token,
            token_jti: coord_resp.token_jti.unwrap_or(uuid::Uuid::nil()),
            token_exp: coord_resp.token_exp.unwrap_or(0),
            active_claims,
        }));
    }

    // `worktree` — junctioned or dedicated target. The only distinction
    // the runner draws today is the build-`target` junction step (which
    // is performed out-of-band today, not in this fn): `Dedicated`
    // suppresses it so the worktree gets a real, isolated target dir.
    let target_mode = match isolation {
        Isolation::Worktree { target } => target,
        // Wait/SharedBranch already returned above.
        _ => TargetMode::Junctioned,
    };

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
                    "git worktree add ok: repo={} branch={} path={} target_mode={:?} stdout={}",
                    w.repo,
                    w.branch,
                    target.display(),
                    target_mode,
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

        // Build-`target` junction policy (Phase 3). `Junctioned` (the
        // default) shares the canonical tree's compiled `target`;
        // `Dedicated` leaves a real per-worktree target. The junction
        // step itself runs out-of-band (the runner does not junction
        // `target` inside this fn today — node_modules/dist/target
        // junctioning is performed by the spawn wrapper), so honoring
        // `Dedicated` here means recording the intent + skipping any
        // junction the wrapper would otherwise apply. We surface the
        // decision via the log line above; a dedicated target needs no
        // affirmative action (absence of the junction IS the dedicated
        // target).
        match target_mode {
            TargetMode::Junctioned => {
                debug!(
                    "isolation: repo={} target=junctioned (shares canonical build target)",
                    w.repo
                );
            }
            TargetMode::Dedicated => {
                debug!(
                    "isolation: repo={} target=dedicated (no build-target junction)",
                    w.repo
                );
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

    // Install the git credential helper in each materialized worktree so
    // that direct `git push` from within the worktree routes through coord.
    // Best-effort: failure to install degrades to direct GitHub push (the
    // agent pusher daemon still pushes through coord regardless).
    for w in &materialized {
        let sid = coord_resp.agent_id.clone();
        let wt_path = w.worktree_path.clone();
        tokio::spawn(async move {
            crate::credential_helper::setup_credential_helper_for_worktree(&wt_path, &sid).await;
        });
    }

    Ok(MaterializeOutcome::Worktrees(AllocateResult {
        agent_id: coord_resp.agent_id,
        worktrees: materialized,
        token: coord_resp.token,
        token_jti: coord_resp.token_jti.unwrap_or(uuid::Uuid::nil()),
        token_exp: coord_resp.token_exp.unwrap_or(0),
        active_claims,
    }))
}

/// Create or check out a feature branch in the canonical checkout for the
/// [`Isolation::SharedBranch`] path. Idempotent w.r.t. an existing branch:
/// if `<branch>` already exists locally we `git checkout <branch>`,
/// otherwise we `git checkout -b <branch> <parent_sha>`. Never creates a
/// worktree.
fn checkout_shared_branch(
    canonical: &std::path::Path,
    branch: &str,
    parent_sha: &str,
) -> Result<(), String> {
    // Ξ_Worktree Phase 7.1 — NEVER switch the branch of a checkout that carries
    // uncommitted work. SharedBranch mutates the *canonical* checkout (the
    // operator's primary), so a bare `git checkout` here would either carry
    // dirty WIP onto the agent's branch or fail the switch. Fail CLOSED: if the
    // tree is dirty — or we can't prove it clean — bail with a typed error
    // (never `-f`, never stash). coord's allocate treats a materialize Err as a
    // fall-back, so the request re-decides as an isolated worktree.
    //
    // Carve-out: if we're already ON the target branch, no switch happens and
    // nothing can be clobbered — allow it (and skip the dirty check) so a
    // re-allocate to an already-active shared branch doesn't spuriously fail.
    // `symbolic-ref` yields empty on detached HEAD → treated as "not on branch"
    // → dirty check applies (detached + dirty correctly bails).
    let current = run_git_command(canonical, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if current != branch {
        // A FAILED status command means we cannot prove the tree clean → unsafe
        // → bail. (Distinct from the reclaim-side `worktree_is_dirty`, which
        // fails OPEN because it's deciding whether to REMOVE, not to mutate.)
        let porcelain = run_git_command(canonical, &["status", "--porcelain"]).map_err(|e| {
            format!(
                "refusing shared-branch switch to '{branch}' in {}: could not determine \
                 working-tree state ({e})",
                canonical.display()
            )
        })?;
        if !porcelain.trim().is_empty() {
            return Err(format!(
                "refusing shared-branch switch to '{branch}' in {}: working tree has \
                 uncommitted changes (would carry WIP across the switch). Commit, stash, \
                 or isolate in a worktree instead.",
                canonical.display()
            ));
        }
    }

    // Does the branch already exist? `git rev-parse --verify --quiet
    // refs/heads/<branch>` exits non-zero when absent.
    let exists = run_git_command(
        canonical,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();

    if exists {
        run_git_command(canonical, &["checkout", branch]).map(|_| ())
    } else {
        run_git_command(canonical, &["checkout", "-b", branch, parent_sha]).map(|_| ())
    }
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

    #[test]
    fn claim_body_sends_session_id_as_owner_discriminator() {
        // The whole point of plan 2026-06-03-coord-session-scoped-claim-owner:
        // when a session id is present it MUST reach coord on the wire so the
        // owner token becomes `machine:session` and two same-machine agents
        // are distinct holders. Asserts the wire contract of every
        // acquire/heartbeat/release body the runner emits.
        let machine = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let session = uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let body = claim_request_body(
            "phase",
            "plan:p:phase:1",
            &machine,
            Some(session),
            serde_json::json!({}),
        );
        assert_eq!(body["kind"], "phase");
        assert_eq!(body["resource_key"], "plan:p:phase:1");
        assert_eq!(body["machine_id"], machine.to_string());
        // uuid serializes hyphenated-lowercase, matching to_string().
        assert_eq!(body["agent_session_id"], session.to_string());
    }

    #[test]
    fn worktree_resource_key_normalizes_to_guard_form() {
        // Pins the canonical worktree-claim resource_key form the
        // coord-guard's `by-resource` lookup expects: forward slashes,
        // lowercase drive letter, no trailing slash. A guard-side
        // `realpath` (/d/… MSYS) divergence is caught by THIS contract.
        let p = std::path::Path::new("D:\\qontinui-root\\qontinui-runner");
        assert_eq!(worktree_resource_key(p), "d:/qontinui-root/qontinui-runner");

        // Forward-slash input + uppercase drive collapses identically.
        let p2 = std::path::Path::new("D:/qontinui-root/qontinui-runner");
        assert_eq!(
            worktree_resource_key(p2),
            "d:/qontinui-root/qontinui-runner"
        );

        // Trailing slash is stripped so `<root>/<repo>` and `<root>/<repo>/`
        // produce the same key.
        let p3 = std::path::Path::new("D:/qontinui-root/qontinui-coord/");
        assert_eq!(worktree_resource_key(p3), "d:/qontinui-root/qontinui-coord");
    }

    #[test]
    fn worktree_for_path_builds_worktree_kind_claim_ctx() {
        let p = std::path::Path::new("D:/qontinui-root/qontinui-runner");
        let ctx = ClaimSpawnContext::worktree_for_path(p, Some("editing the runner"));
        assert_eq!(ctx.kind, "worktree");
        assert_eq!(ctx.resource_key, "d:/qontinui-root/qontinui-runner");
        assert_eq!(ctx.intent.as_deref(), Some("editing the runner"));
        assert!(ctx.plan_id.is_none());
        assert!(ctx.phase.is_none());
        // worktree claims use the coord-default 300s TTL.
        assert_eq!(default_ttl_seconds_for(ctx.kind), 300);
    }

    #[test]
    fn claim_body_emits_null_session_id_for_machine_level_acquirers() {
        // Daemons / merge-scheduler acquire at machine scope (no session).
        // The field must still be PRESENT and explicitly null so coord
        // falls back to the legacy bare-machine owner token rather than
        // mis-parsing a missing key.
        let machine = uuid::Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let body = claim_request_body("file_glob", "src/**", &machine, None, serde_json::json!({}));
        assert!(
            body.as_object().unwrap().contains_key("agent_session_id"),
            "agent_session_id key must always be present on the wire"
        );
        assert!(body["agent_session_id"].is_null());
    }

    #[test]
    fn checkout_shared_branch_bails_on_dirty_tree() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let p = path.to_str().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args([&["-C", p], args].concat())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);
        let parent = String::from_utf8(
            Command::new("git")
                .args(["-C", p, "rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let parent = parent.trim();

        // Clean tree on `main`, switching to a NEW branch → allowed.
        assert!(checkout_shared_branch(path, "agent/clean", parent).is_ok());
        // Switch back to main so the next case starts off-target.
        Command::new("git")
            .args(["-C", p, "checkout", "-q", "main"])
            .output()
            .unwrap();

        // Dirty the tree, then attempt a switch to a different branch → bail.
        std::fs::write(path.join("a.txt"), b"dirty").unwrap();
        let err = checkout_shared_branch(path, "agent/dirty", parent).unwrap_err();
        assert!(
            err.contains("uncommitted changes"),
            "expected dirty bail, got: {err}"
        );

        // Carve-out: already ON the target branch + dirty → no switch, allowed.
        git(&["checkout", "-q", "-b", "agent/oncurrent"]);
        std::fs::write(path.join("a.txt"), b"still dirty").unwrap();
        assert!(
            checkout_shared_branch(path, "agent/oncurrent", parent).is_ok(),
            "same-branch re-checkout must not bail on dirty"
        );
    }

    // ----- Ξ_Worktree Phase 7.5b: canonical_checkout lease ---------------

    #[test]
    fn canonical_checkout_for_repo_builds_repo_scoped_lease_ctx() {
        // The resource_key MUST be the verbatim `repo:<repo>` form coord's P3
        // probe (`load_canonical_lease_claim`) looks up — coord's
        // `normalize_resource_key` is a PASS-THROUGH for non-Worktree kinds, so
        // NO path canonicalization is applied. A drift here = a lease the probe
        // never sees → no serialization → the exact race 7.5b closes.
        let ctx = ClaimSpawnContext::canonical_checkout_for_repo(
            "qontinui-coord",
            Some("shared-branch on coord"),
        );
        assert_eq!(ctx.kind, "canonical_checkout");
        assert_eq!(ctx.resource_key, "repo:qontinui-coord");
        assert_eq!(ctx.intent.as_deref(), Some("shared-branch on coord"));
        assert!(ctx.plan_id.is_none());
        assert!(ctx.phase.is_none());
        // Kind wire-name + TTL match coord's enum (claims.rs:183 / 302).
        assert_eq!(default_ttl_seconds_for(ctx.kind), 300);
    }

    #[test]
    fn shared_branch_acquires_canonical_checkout_lease() {
        // A SharedBranch honor acquires one canonical_checkout claim per repo
        // with the correct kind + resource_key, and the lease lands in the
        // release-tracked collection (`active_claims`) so it RELEASES on the
        // existing session-end drain. Models the production per-repo acquire
        // loop over an all-success outcome set.
        let session = uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let outcomes: Vec<Result<ActiveClaim, AllocateError>> = ["qontinui-coord", "qontinui-web"]
            .iter()
            .map(|repo| {
                let ctx = ClaimSpawnContext::canonical_checkout_for_repo(repo, Some("sb"));
                Ok(ActiveClaim {
                    kind: ctx.kind.to_string(),
                    resource_key: ctx.resource_key,
                    ttl_seconds: default_ttl_seconds_for(ctx.kind),
                    agent_session_id: Some(session),
                })
            })
            .collect();

        match plan_canonical_leases(outcomes) {
            CanonicalLeasePlan::Proceed { acquired } => {
                // One lease per repo, all in the release-tracked collection.
                assert_eq!(acquired.len(), 2);
                assert!(acquired
                    .iter()
                    .all(|c| c.kind == "canonical_checkout" && c.ttl_seconds == 300));
                assert_eq!(acquired[0].resource_key, "repo:qontinui-coord");
                assert_eq!(acquired[1].resource_key, "repo:qontinui-web");
                // The same vec the session-end release path drains.
                assert!(acquired.iter().all(|c| c.agent_session_id == Some(session)));
            }
            CanonicalLeasePlan::Bail { .. } => panic!("expected Proceed on all-Ok outcomes"),
        }
    }

    #[test]
    fn shared_branch_bails_when_canonical_lease_held() {
        // When a peer holds the canonical_checkout lease for a repo, the honor
        // bails with an AllocateError and releases any partial leases acquired
        // for the earlier repos (so a partial acquire never strands a held
        // canonical tree). Models the production unwind-on-`Held`.
        let session = uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let first_ctx = ClaimSpawnContext::canonical_checkout_for_repo("qontinui-coord", None);
        let first = ActiveClaim {
            kind: first_ctx.kind.to_string(),
            resource_key: first_ctx.resource_key,
            ttl_seconds: default_ttl_seconds_for(first_ctx.kind),
            agent_session_id: Some(session),
        };
        let conflict = ClaimConflict {
            kind: "canonical_checkout".to_string(),
            resource_key: "repo:qontinui-web".to_string(),
            current_holder: "machine-b:session-x".to_string(),
            intent: None,
        };
        // First repo acquires; second is Held by a peer.
        let outcomes: Vec<Result<ActiveClaim, AllocateError>> =
            vec![Ok(first), Err(AllocateError::ClaimConflict(conflict))];

        match plan_canonical_leases(outcomes) {
            CanonicalLeasePlan::Bail { to_release, error } => {
                // The earlier-acquired lease is handed back for release.
                assert_eq!(to_release.len(), 1);
                assert_eq!(to_release[0].kind, "canonical_checkout");
                assert_eq!(to_release[0].resource_key, "repo:qontinui-coord");
                // Bails with a ClaimConflict so coord re-decides as Worktree.
                match error {
                    AllocateError::ClaimConflict(c) => {
                        assert_eq!(c.resource_key, "repo:qontinui-web");
                        assert_eq!(c.current_holder, "machine-b:session-x");
                    }
                    other => panic!("expected ClaimConflict, got {other:?}"),
                }
            }
            CanonicalLeasePlan::Proceed { .. } => panic!("expected Bail when a lease is Held"),
        }
    }
}
