//! Phase 1 of `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md`.
//!
//! Ergonomic facade over [`super::allocate_and_materialize_with_claim`]
//! for callers that say "I intend to edit these repos" and want a
//! materialized worktree + held claim + automatic release on drop —
//! WITHOUT having to resolve coord URL, `device_id`, canonical paths,
//! and `parent_sha` themselves.
//!
//! Phase 2 wires this into `terminal_create` and `spawn_worker_session`;
//! Phase 3 wires the same shape into the skill orchestrators
//! (`/manual-test-loop`, `/manual-test`, `/implement-plan`) through this
//! in-process facade directly (the former `/agents/allocate-local` HTTP
//! wrapper was unused and has been removed).
//!
//! Gated behind [`super::worktree_mode_enabled`] — when the flag is off,
//! [`acquire`] returns `Ok(None)` so callers fall back to the legacy
//! shared-checkout flow without forking their code path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use super::{
    allocate_and_materialize_with_claim, release_claim_best_effort, spawn_heartbeat_task,
    worktree_mode_enabled, ActiveClaim, AllocateError, ClaimHeartbeatHandle, MaterializeOutcome,
    MaterializedWorktree, RepoRequest,
};

/// A held isolated edit context. While this value is live, the agent's
/// claim is being heartbeated. Dropping it stops the heartbeat task and
/// fires a best-effort claim release.
///
/// `worktrees` carries every materialized per-repo worktree; the typical
/// caller uses `worktrees[0].worktree_path` as its session cwd.
pub struct IsolatedEditContext {
    pub agent_id: String,
    pub worktrees: Vec<MaterializedWorktree>,
    /// Every claim the spawn flow pre-acquired — one `kind=worktree`
    /// claim per materialized repo (always), plus the optional
    /// phase/file_glob claim. Empty when no claim was acquired (worktree
    /// mode off path never reaches here). Drop releases EVERY entry.
    /// Phase 1 of plan 2026-06-06-session-scoped-multi-repo-workspace.
    pub active_claims: Vec<ActiveClaim>,

    // One heartbeat handle per claim in `active_claims` — each task lives
    // as long as the context; the handle's own Drop notifies its cancel
    // token. Kept in lockstep with `active_claims` (parallel vectors are
    // fine here — every claim spawns exactly one heartbeat). Keep-alive
    // only; we never inspect them.
    _heartbeats: Vec<ClaimHeartbeatHandle>,

    // Captured for the Drop-time release POSTs. `coord_http_base` and
    // `device_id` aren't otherwise exposed on the context — Drop and the
    // grow/shrink primitives are the only consumers.
    coord_http_base: String,
    device_id: uuid::Uuid,

    // ---- Ambient edit-effect loop (Phase 3, plan
    // 2026-06-05-edit-effect-loop-adoption) ----
    // The `predict`-side stash filled at acquisition and read by the
    // `verify`-side spawn in `Drop`. Shared `Arc<Mutex<_>>` so the
    // fire-and-forget predict task can fill it after `acquire` returns.
    edit_loop_stash: super::edit_effect_loop::PredictionStash,
    // The single correlation id tying this context's predict to its verify.
    // Minted at acquire so both calls (predict at acquire, verify at drop)
    // share it.
    edit_loop_correlation_id: uuid::Uuid,
    // The declared touch set, captured so `Drop`'s verify call can scope the
    // FS-observation lookup to the same paths the predict declared.
    edit_loop_declared_paths: Vec<String>,
}

impl Drop for IsolatedEditContext {
    fn drop(&mut self) {
        // Ξ_FS observer producer (plan 2026-05-31 §6.2): on session end,
        // diff each materialized worktree against its base `parent_sha` and
        // push the per-path {pre_sha, post_sha, hunks} batch to coord's
        // `POST /coord/fs/observations`. Gated behind
        // `QONTINUI_FS_OBSERVER_ENABLED` (default off) inside the producer;
        // best-effort and fire-and-forget — a coord failure NEVER blocks the
        // session teardown. One `correlation_id` per drop groups a multi-repo
        // logical edit (matching coord's predict/verify correlation model).
        if super::fs_observer::fs_observer_enabled() {
            let correlation_id = Some(uuid::Uuid::new_v4());
            for w in &self.worktrees {
                super::fs_observer::spawn_push_worktree_observations(
                    self.coord_http_base.clone(),
                    w.worktree_path.clone(),
                    w.parent_sha.clone(),
                    w.repo.clone(),
                    Some(self.agent_id.clone()),
                    correlation_id,
                    None, // tenant_id not resolved in this context
                );
            }
        }

        // Ambient edit-effect loop — verify side (Phase 3, plan
        // 2026-06-05-edit-effect-loop-adoption). AFTER the Ξ_FS observations
        // push so coord has the realized post-shas to verify against, and
        // under the SAME `correlation_id` minted at acquire so the prediction
        // and its verification tie together. Flag-checked inside `spawn_verify`
        // (default off, flips with `QONTINUI_FS_OBSERVER_ENABLED` once quiet)
        // and drop-safe: it spawns onto the current runtime, never blocking
        // `Drop` — same mechanism the fs_observations push uses.
        for w in &self.worktrees {
            super::edit_effect_loop::spawn_verify(
                self.coord_http_base.clone(),
                w.worktree_path.clone(),
                w.repo.clone(),
                self.edit_loop_declared_paths.clone(),
                self.edit_loop_correlation_id,
                self.edit_loop_stash.clone(),
            );
        }

        // Stop EVERY heartbeat first so no tick races with a release POST.
        // `ClaimHeartbeatHandle`'s own Drop notifies its cancel token; each
        // task exits on its next tick boundary.
        self._heartbeats.clear();

        // Best-effort release of every active claim on the current Tokio
        // runtime. A failed release is observability-only — the session
        // that drops the context is shutting down anyway.
        //
        // Guard the spawn on a live runtime: outside one — a sync `#[test]`,
        // or a Drop that runs while unwinding a panic — `tokio::spawn` itself
        // panics ("no reactor running"), and a panic during unwind aborts the
        // process (SIGABRT). Release is best-effort, so skipping it when no
        // runtime is present is the safe degrade; coord's TTL reaps the claim.
        let claims = std::mem::take(&mut self.active_claims);
        if !claims.is_empty() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let base = self.coord_http_base.clone();
                let mid = self.device_id;
                handle.spawn(async move {
                    for claim in claims {
                        release_claim_best_effort(
                            &base,
                            mid,
                            claim.agent_session_id,
                            &claim.kind,
                            &claim.resource_key,
                        )
                        .await;
                    }
                });
            }
        }
    }
}

impl IsolatedEditContext {
    /// Grow primitive (Phase 1c, plan
    /// 2026-06-06-session-scoped-multi-repo-workspace-coordination):
    /// materialize ONE more repo's worktree, acquire its `kind=worktree`
    /// claim, spawn its heartbeat, and join it to this live context (push
    /// onto `worktrees`, `active_claims`, `_heartbeats`).
    ///
    /// Idempotent on an already-joined repo: if `repo` is already present in
    /// this context's `worktrees`, returns `Ok(())` without a second
    /// allocate (the caller can blindly request a repo it may already hold).
    ///
    /// A `Held` on the new repo's worktree claim surfaces as
    /// `Err(AllocateError::ClaimConflict)` — the existing repos stay joined
    /// and claimed; only the new repo is rejected. Any other failure
    /// (resolution, transport, `git worktree add`) returns
    /// `Err(AllocateError::Other)`, again leaving the existing context
    /// untouched.
    ///
    /// `intent` is the human-readable intent stored on the new claim; the
    /// new repo is materialized as a `kind=worktree`-only claim (no
    /// phase/file_glob — those are an acquisition-time concern).
    pub async fn acquire_additional(
        &mut self,
        repo: &str,
        intent: Option<&str>,
    ) -> Result<(), AllocateError> {
        if self.worktrees.iter().any(|w| w.repo == repo) {
            info!(
                repo = %repo,
                "isolated_edit: acquire_additional — repo already joined; no-op"
            );
            return Ok(());
        }

        let repos = [repo.to_string()];
        let session_id = self.session_id();
        let mut result = materialize_repos(
            &self.coord_http_base,
            self.device_id,
            &repos,
            intent,
            None,
            None,
            None,
            session_id,
        )
        .await?;

        // Spawn a heartbeat for each newly-acquired claim (one worktree
        // claim for the single repo) and join everything onto the context.
        for claim in &result.active_claims {
            let hb =
                spawn_claim_heartbeat(&self.coord_http_base, self.device_id, &self.agent_id, claim);
            self._heartbeats.push(hb);
        }
        self.active_claims.append(&mut result.active_claims);
        self.worktrees.append(&mut result.worktrees);
        Ok(())
    }

    /// Shrink primitive (Phase 1c): release `repo`'s `kind=worktree` claim,
    /// abort its heartbeat, and drop it from this context.
    ///
    /// Coordination state only — the on-disk worktree directory is LEFT in
    /// place for the existing reclaim machinery
    /// (`agent_worktree::reclaim`) to prune; releasing the claim merely
    /// hands the checkout's coordination ownership back. This avoids racing
    /// a `git worktree remove` against a process that may still hold the
    /// dir open, and keeps removal centralized in one sweeper.
    ///
    /// Returns `true` if `repo` was present and released, `false` if it was
    /// not joined (idempotent no-op).
    pub async fn release_repo(&mut self, repo: &str) -> bool {
        // Find the repo's worktree so we can compute its canonical-path
        // claim key (the worktree's `worktree_path` is the canonical
        // checkout for the SharedBranch case; for a real worktree it is the
        // worktree dir, so we re-derive the canonical checkout from the repo
        // slug to match the acquire-side key exactly).
        let Some(pos) = self.worktrees.iter().position(|w| w.repo == repo) else {
            return false;
        };

        // Re-derive the canonical checkout path → resource_key the SAME way
        // the acquire side did (`worktree_for_path` over
        // `default_canonical_path`), so we release the byte-identical key.
        let want_key = super::canonical_paths::default_canonical_path(repo)
            .ok()
            .map(|p| super::worktree_resource_key(&p));

        // Drop the matching worktree claim + its heartbeat. Match on the
        // worktree kind AND the derived key (so we never release a
        // phase/file_glob claim that happens to coexist).
        if let Some(key) = want_key {
            if let Some(cpos) = self
                .active_claims
                .iter()
                .position(|c| c.kind == "worktree" && c.resource_key == key)
            {
                let claim = self.active_claims.remove(cpos);
                // Abort the heartbeat whose resource_key matches.
                if let Some(hpos) = self
                    ._heartbeats
                    .iter()
                    .position(|h| h.kind == "worktree" && h.resource_key == key)
                {
                    // Dropping the handle notifies its cancel token.
                    let _ = self._heartbeats.remove(hpos);
                }
                release_claim_best_effort(
                    &self.coord_http_base,
                    self.device_id,
                    claim.agent_session_id,
                    &claim.kind,
                    &claim.resource_key,
                )
                .await;
            }
        }

        // Drop the worktree bookkeeping (leave the on-disk dir for reclaim).
        self.worktrees.remove(pos);
        info!(
            repo = %repo,
            "isolated_edit: release_repo — worktree claim released, dir left for reclaim"
        );
        true
    }

    /// The per-session owner-token discriminator carried by this context's
    /// claims (all claims share it). `None` for a context whose claims were
    /// acquired machine-level. Used to keep `acquire_additional`'s new claim
    /// on the same owner token as the rest of the context.
    fn session_id(&self) -> Option<uuid::Uuid> {
        self.active_claims.first().and_then(|c| c.agent_session_id)
    }
}

/// Inputs for [`acquire`]. `repos` are bare slugs (e.g.
/// `"qontinui-runner"`); the facade resolves canonical checkout paths
/// from per-platform defaults and reads `parent_sha` from `HEAD` of each
/// canonical checkout.
pub struct AcquireRequest<'a> {
    pub repos: &'a [String],
    /// Optional opaque human-readable intent. Stored on
    /// `coord.agent_worktrees.intent`.
    pub intent: Option<&'a str>,
    /// Optional pre-derived overlap path set (Phase 1B). When absent,
    /// coord may LLM-derive from `intent`.
    pub declared_overlap_paths: Option<&'a [String]>,
    /// Optional `(plan_id, phase)` pair for a `ClaimKind::Phase`
    /// pre-flight — see [`super::ClaimSpawnContext::from_intent_and_paths`]
    /// precedence rules.
    pub plan_id: Option<&'a str>,
    pub phase: Option<&'a str>,
    /// coord-assigned per-agent session id, folded into the claim owner token.
    pub agent_session_id: Option<uuid::Uuid>,
}

/// Allocate isolated worktrees for the given repos.
///
/// Returns:
/// - `Ok(Some(ctx))` — worktree mode is on; allocation succeeded.
/// - `Ok(None)` — worktree mode is off; the caller should fall back to
///   the legacy shared-checkout flow.
/// - `Err(AllocateError::ClaimConflict)` — pre-flight claim was held by
///   another agent; the caller decides abort/wait/steal.
/// - `Err(AllocateError::Other(_))` — anything else (resolution failure,
///   coord transport error, `git worktree add` failure, etc.).
pub async fn acquire(
    req: AcquireRequest<'_>,
) -> Result<Option<IsolatedEditContext>, AllocateError> {
    if !worktree_mode_enabled() {
        return Ok(None);
    }
    if req.repos.is_empty() {
        return Err(AllocateError::Other("repos must not be empty".into()));
    }

    let coord_http_base = crate::mcp::agent_worktrees::coord_http_base()
        .map_err(|e| AllocateError::Other(format!("coord URL not configured: {e}")))?;
    let device_id = read_device_id()
        .map_err(|e| AllocateError::Other(format!("device_id not available: {e}")))?;

    let result = materialize_repos(
        &coord_http_base,
        device_id,
        req.repos,
        req.intent,
        req.declared_overlap_paths,
        req.plan_id,
        req.phase,
        req.agent_session_id,
    )
    .await?;

    // One heartbeat task per claim (worktree claims + the optional
    // phase/file_glob claim), kept index-aligned with `active_claims`.
    let heartbeats: Vec<ClaimHeartbeatHandle> = result
        .active_claims
        .iter()
        .map(|claim| spawn_claim_heartbeat(&coord_http_base, device_id, &result.agent_id, claim))
        .collect();

    // Ambient edit-effect loop — predict side (Phase 3, plan
    // 2026-06-05-edit-effect-loop-adoption). Fire-and-forget a
    // `predict-and-check` per materialized worktree right after acquisition,
    // stashing the response for the verify side that fires in `Drop`. One
    // correlation id ties the two. Flag-checked inside `spawn_predict`
    // (default off) so this stays an unconditional call. `declared_overlap_paths`
    // is the predicted touch set AND the declared scope at acquisition time.
    let edit_loop_declared_paths: Vec<String> = req
        .declared_overlap_paths
        .map(|p| p.to_vec())
        .unwrap_or_default();
    let edit_loop_correlation_id = uuid::Uuid::new_v4();
    let edit_loop_stash = super::edit_effect_loop::new_stash();
    for w in &result.worktrees {
        super::edit_effect_loop::spawn_predict(
            coord_http_base.clone(),
            w.repo.clone(),
            w.parent_sha.clone(),
            edit_loop_declared_paths.clone(),
            req.intent.map(|s| s.to_string()),
            edit_loop_correlation_id,
            edit_loop_stash.clone(),
        );
    }

    Ok(Some(IsolatedEditContext {
        agent_id: result.agent_id,
        worktrees: result.worktrees,
        active_claims: result.active_claims,
        _heartbeats: heartbeats,
        coord_http_base,
        device_id,
        edit_loop_stash,
        edit_loop_correlation_id,
        edit_loop_declared_paths,
    }))
}

/// Resolve canonical paths + parent SHAs for `repos`, call
/// [`allocate_and_materialize_with_claim`], and normalize coord's isolation
/// directive into a worktree-shaped [`super::AllocateResult`]. Shared by
/// [`acquire`] (all repos up front) and
/// [`IsolatedEditContext::acquire_additional`] (one extra repo). Does NOT
/// spawn heartbeats — the caller decides where the heartbeat handles live.
///
/// Isolation normalization:
/// - `Worktrees` → as-is.
/// - `SharedBranch` → the canonical checkout IS the cwd; map each per-repo
///   branch into a `MaterializedWorktree` whose `worktree_path` is the
///   canonical checkout so callers reading `worktrees[0].worktree_path` get
///   the right cwd (the shared checkout) transparently.
/// - `Wait` → `Err(Other)` with a `wait:` prefix so the caller logs/retries
///   (it does NOT spin). `acquire_for_terminal` degrades to the shared cwd.
#[allow(clippy::too_many_arguments)]
async fn materialize_repos(
    coord_http_base: &str,
    device_id: uuid::Uuid,
    repos: &[String],
    intent: Option<&str>,
    declared_overlap_paths: Option<&[String]>,
    plan_id: Option<&str>,
    phase: Option<&str>,
    agent_session_id: Option<uuid::Uuid>,
) -> Result<super::AllocateResult, AllocateError> {
    let mut canonical_paths: HashMap<String, PathBuf> = HashMap::with_capacity(repos.len());
    for repo in repos {
        let path = super::canonical_paths::default_canonical_path(repo)
            .map_err(|e| AllocateError::Other(format!("canonical path for repo {repo:?}: {e}")))?;
        canonical_paths.insert(repo.clone(), path);
    }

    let mut repo_reqs: Vec<RepoRequest> = Vec::with_capacity(repos.len());
    for repo in repos {
        let canonical = canonical_paths
            .get(repo)
            .ok_or_else(|| AllocateError::Other(format!("no canonical path for repo '{repo}'")))?;
        let parent_sha = resolve_head_sha(canonical)
            .map_err(|e| AllocateError::Other(format!("git rev-parse HEAD on {repo}: {e}")))?;
        repo_reqs.push(RepoRequest {
            repo: repo.clone(),
            parent_sha,
        });
    }

    let outcome = allocate_and_materialize_with_claim(
        coord_http_base,
        &device_id,
        agent_session_id,
        &repo_reqs,
        intent,
        declared_overlap_paths,
        &canonical_paths,
        plan_id,
        phase,
    )
    .await?;

    match outcome {
        MaterializeOutcome::Worktrees(r) => Ok(r),
        MaterializeOutcome::SharedBranch(sb) => Ok(super::AllocateResult {
            agent_id: sb.agent_id,
            worktrees: sb
                .branches
                .into_iter()
                .map(|b| MaterializedWorktree {
                    repo: b.repo,
                    branch: b.branch,
                    parent_sha: b.parent_sha,
                    worktree_path: b.checkout_path,
                    push_ref: b.push_ref,
                })
                .collect(),
            token: sb.token,
            token_jti: sb.token_jti,
            token_exp: sb.token_exp,
            active_claims: sb.active_claims,
        }),
        MaterializeOutcome::Wait(w) => Err(AllocateError::Other(format!(
            "wait: coord declined to materialize agent_id={} reason={:?} blocking={:?} retry_when={:?}",
            w.agent_id, w.reason, w.blocking, w.retry_when
        ))),
    }
}

/// Spawn the TTL/3 heartbeat task for one claim. Shared by [`acquire`] and
/// the grow/shrink primitive [`IsolatedEditContext::acquire_additional`] so
/// the log line + stolen-claim warning stay in one place.
fn spawn_claim_heartbeat(
    coord_http_base: &str,
    device_id: uuid::Uuid,
    agent_id: &str,
    claim: &ActiveClaim,
) -> ClaimHeartbeatHandle {
    info!(
        agent_id = %agent_id,
        kind = %claim.kind,
        resource_key = %claim.resource_key,
        ttl_seconds = claim.ttl_seconds,
        "isolated_edit: spawning heartbeat"
    );
    spawn_heartbeat_task(
        coord_http_base.to_string(),
        device_id,
        claim.agent_session_id,
        &claim.kind,
        claim.resource_key.clone(),
        claim.ttl_seconds,
        |displaced_by| {
            warn!(
                displaced_by = ?displaced_by,
                "isolated_edit: claim stolen — edits in this worktree may now race"
            );
        },
    )
}

/// Environment-variable name carrying the full set of materialized
/// per-repo worktrees onto a launched agent process (Phase 2c). The value
/// is a `;`-separated list of `<repo>=<abs_worktree_path>` pairs covering
/// EVERY worktree in the launch's `IsolatedEditContext` (including the
/// first / cwd repo), so an agent-agnostic launch can locate sibling
/// repos that materialized on disk but aren't the process cwd.
pub const SESSION_WORKTREES_ENV: &str = "QONTINUI_SESSION_WORKTREES";

/// Build the [`SESSION_WORKTREES_ENV`] value for a set of materialized
/// worktrees: `"<repo>=<abs_path>;<repo2>=<abs_path2>;…"` over ALL
/// worktrees in order (the first entry is the session cwd repo). Returns
/// `None` when there are no worktrees (worktree mode off / no allocate) so
/// the caller can skip setting an empty env var.
///
/// Paths are emitted via `Path::display()` so they stay platform-native
/// (`D:\…` on Windows, `/home/…` on POSIX). `repo` slugs never contain
/// `=` or `;`, so the delimiter set is unambiguous.
pub fn session_worktrees_env_value(worktrees: &[MaterializedWorktree]) -> Option<String> {
    if worktrees.is_empty() {
        return None;
    }
    let joined = worktrees
        .iter()
        .map(|w| format!("{}={}", w.repo, w.worktree_path.display()))
        .collect::<Vec<_>>()
        .join(";");
    Some(joined)
}

/// Build the `--add-dir <path>` argument pairs for the SIBLING worktrees of
/// a launch — every worktree except the first (the first is the process
/// cwd, which `claude` already roots at, so it needs no `--add-dir`). Used
/// to append to a `claude` command so a multi-repo session can edit the
/// non-cwd repos too. Returns an empty vec for single-repo (or empty)
/// launches.
///
/// Order matches `worktrees[1..]` so the resulting args are deterministic.
pub fn claude_add_dir_args(worktrees: &[MaterializedWorktree]) -> Vec<String> {
    worktrees
        .iter()
        .skip(1)
        .flat_map(|w| {
            [
                "--add-dir".to_string(),
                w.worktree_path.display().to_string(),
            ]
        })
        .collect()
}

impl IsolatedEditContext {
    /// [`session_worktrees_env_value`] over this context's worktrees — the
    /// [`SESSION_WORKTREES_ENV`] value to export onto the launched process.
    pub fn session_worktrees_env_value(&self) -> Option<String> {
        session_worktrees_env_value(&self.worktrees)
    }

    /// [`claude_add_dir_args`] over this context's worktrees — the
    /// `--add-dir <sibling>` args to append to a `claude` launch command.
    pub fn claude_add_dir_args(&self) -> Vec<String> {
        claude_add_dir_args(&self.worktrees)
    }
}

/// Convenience helper for the runner terminal-spawn entry points
/// (`commands/terminal.rs::terminal_create`, `commands/productivity.rs::
/// spawn_worker_session` + `launch_coordinator_session`,
/// `mcp/terminals.rs::handle_terminal_create`, `mcp/tauri_proxy.rs::
/// "terminal_create"`, `mcp/backend_relay.rs::handle_terminal_create`).
/// Given the caller's `intent_repo` + `purpose` + original `working_dir`,
/// returns the effective working dir for the PTY plus an
/// `IsolatedEditContext` to park on the resulting `TerminalSession`.
///
/// Behavior:
/// - `intent_repo == None` → returns `(working_dir, None)`. Observation
///   session; legacy shared-checkout path.
/// - `intent_repo == Some(_)` but `worktree_mode_enabled()` is false →
///   returns `(working_dir, None)`. Caller's existing behavior preserved.
/// - `intent_repo == Some(_)` and worktree mode is on, allocate succeeds
///   → returns `(Some(worktree_path), Some(ctx))`.
/// - `intent_repo == Some(_)` and worktree mode is on, allocate fails →
///   returns `(working_dir, None)`. Failure is logged as a warning;
///   the spawn flow falls back to the primary checkout (this matches
///   the in-place Phase 2 wireup of `terminal_create` and
///   `spawn_worker_session`).
pub async fn acquire_for_terminal(
    intent_repo: Option<&str>,
    purpose: &str,
    working_dir: Option<String>,
    agent_session_id: Option<uuid::Uuid>,
) -> (Option<String>, Option<IsolatedEditContext>) {
    let Some(repo) = intent_repo else {
        return (working_dir, None);
    };
    let repos = vec![repo.to_string()];
    match acquire(AcquireRequest {
        repos: &repos,
        intent: Some(purpose),
        declared_overlap_paths: None,
        plan_id: None,
        phase: None,
        agent_session_id,
    })
    .await
    {
        Ok(Some(ctx)) => {
            let wt_path = ctx
                .worktrees
                .first()
                .map(|w| w.worktree_path.to_string_lossy().to_string());
            (wt_path.or(working_dir), Some(ctx))
        }
        Ok(None) => (working_dir, None),
        Err(e) => {
            warn!(
                intent_repo = %repo,
                error = %e,
                "acquire_for_terminal: isolated worktree allocate failed — falling back to shared cwd"
            );
            (working_dir, None)
        }
    }
}

/// Read the device UUID from `~/.qontinui/machine.json`. Accepts the
/// canonical `"device_id"` field (post-unified-devices) and falls back
/// to legacy `"machine_id"`.
fn read_device_id() -> Result<uuid::Uuid, String> {
    let path = dirs::home_dir()
        .map(|h| h.join(".qontinui").join("machine.json"))
        .ok_or_else(|| "no HOME dir".to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    let id_str = v
        .get("device_id")
        .or_else(|| v.get("machine_id"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{}: missing device_id (or machine_id)", path.display()))?;
    uuid::Uuid::parse_str(id_str).map_err(|e| format!("invalid UUID: {e}"))
}

fn resolve_head_sha(canonical: &Path) -> Result<String, String> {
    use std::process::Command;
    let output = Command::new("git")
        .arg("-C")
        .arg(canonical)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse HEAD failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        return Err("git rev-parse HEAD: empty SHA".into());
    }
    Ok(sha)
}

#[cfg(test)]
impl IsolatedEditContext {
    /// Test-only constructor that builds a context with synthetic claims and
    /// NO live heartbeats, bypassing the network acquire path. Used to
    /// exercise the multi-claim bookkeeping (`acquire_additional` joins,
    /// `release_repo` shrink, Drop releases all) without coord. The
    /// `coord_http_base` points at an unroutable address so any best-effort
    /// release POST fails instantly instead of hitting a real coord.
    pub(crate) fn for_test(
        worktrees: Vec<MaterializedWorktree>,
        active_claims: Vec<ActiveClaim>,
    ) -> Self {
        IsolatedEditContext {
            agent_id: "test-agent".to_string(),
            worktrees,
            active_claims,
            _heartbeats: Vec::new(),
            // 127.0.0.1:1 refuses instantly — release POSTs fail fast, the
            // bookkeeping under test runs regardless.
            coord_http_base: "http://127.0.0.1:1".to_string(),
            device_id: uuid::Uuid::nil(),
            edit_loop_stash: super::edit_effect_loop::new_stash(),
            edit_loop_correlation_id: uuid::Uuid::nil(),
            edit_loop_declared_paths: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `(MaterializedWorktree, worktree-kind ActiveClaim)` pair for
    /// `repo`, with the claim keyed on the SAME canonical-path resource_key
    /// the acquire side derives — so `release_repo`'s key match works.
    fn repo_fixture(repo: &str) -> (MaterializedWorktree, ActiveClaim) {
        let canonical = super::super::canonical_paths::default_canonical_path(repo).unwrap();
        let key = super::super::worktree_resource_key(&canonical);
        let wt = MaterializedWorktree {
            repo: repo.to_string(),
            branch: format!("agent/test-{repo}"),
            parent_sha: "0".repeat(40),
            worktree_path: canonical,
            push_ref: format!("refs/agent/test-{repo}"),
        };
        let claim = ActiveClaim {
            kind: "worktree".to_string(),
            resource_key: key,
            ttl_seconds: 300,
            agent_session_id: Some(uuid::Uuid::nil()),
        };
        (wt, claim)
    }

    #[test]
    fn multi_repo_context_has_one_worktree_claim_per_repo() {
        // Mirrors the multi-repo acquire outcome: a context materialized for
        // [A, B] carries exactly one kind=worktree claim per repo, each keyed
        // on that repo's canonical checkout path (the guard's by-resource
        // form). Asserts the bookkeeping the acquire path produces without
        // needing a live coord.
        let (wa, ca) = repo_fixture("qontinui-runner");
        let (wb, cb) = repo_fixture("qontinui-coord");
        let mut ctx = IsolatedEditContext::for_test(vec![wa, wb], vec![ca, cb]);

        let worktree_claims: Vec<&ActiveClaim> = ctx
            .active_claims
            .iter()
            .filter(|c| c.kind == "worktree")
            .collect();
        assert_eq!(worktree_claims.len(), 2, "one worktree claim per repo");
        // Derive the expected keys from the SAME helpers the acquire side uses,
        // so the assert is platform-portable: `default_canonical_path` resolves
        // to `D:/qontinui-root/...` on Windows and `$HOME/qontinui-root/...` on
        // POSIX, and `worktree_resource_key` normalizes both to the guard form.
        for repo in ["qontinui-runner", "qontinui-coord"] {
            let expected = super::super::worktree_resource_key(
                &super::super::canonical_paths::default_canonical_path(repo).unwrap(),
            );
            assert!(
                worktree_claims.iter().any(|c| c.resource_key == expected),
                "missing worktree claim keyed on {expected}"
            );
        }

        // Drain before drop: this is a non-async test, and Drop's release
        // path `tokio::spawn`s — which panics outside a runtime. Clearing
        // trips Drop's `!claims.is_empty()` guard so no spawn is attempted.
        ctx.active_claims.clear();
        ctx.worktrees.clear();
    }

    #[tokio::test]
    async fn release_repo_shrinks_claims_and_worktrees() {
        // acquire([A,B]) shape → 2 worktree claims; release_repo(A) → 1.
        // Exercises the shrink primitive's bookkeeping (the network release
        // is best-effort and fails fast against the unroutable base).
        let (wa, ca) = repo_fixture("qontinui-runner");
        let (wb, cb) = repo_fixture("qontinui-coord");
        let mut ctx = IsolatedEditContext::for_test(vec![wa, wb], vec![ca, cb]);
        assert_eq!(ctx.active_claims.len(), 2);
        assert_eq!(ctx.worktrees.len(), 2);

        let released = ctx.release_repo("qontinui-runner").await;
        assert!(released, "release_repo returns true for a joined repo");
        assert_eq!(ctx.active_claims.len(), 1, "one worktree claim remains");
        assert_eq!(ctx.worktrees.len(), 1);
        assert_eq!(ctx.worktrees[0].repo, "qontinui-coord");
        let expected_coord = super::super::worktree_resource_key(
            &super::super::canonical_paths::default_canonical_path("qontinui-coord").unwrap(),
        );
        assert_eq!(ctx.active_claims[0].resource_key, expected_coord);

        // Idempotent: releasing a repo that's no longer joined is a no-op.
        let again = ctx.release_repo("qontinui-runner").await;
        assert!(!again, "release_repo returns false for an absent repo");
        assert_eq!(ctx.active_claims.len(), 1);

        // Drain so the Drop release path doesn't fire a stray POST after the
        // test runtime is gone.
        ctx.active_claims.clear();
        ctx.worktrees.clear();
    }

    #[tokio::test]
    async fn release_repo_only_drops_matching_worktree_claim() {
        // A coexisting phase claim must survive a worktree release.
        let (wa, ca) = repo_fixture("qontinui-runner");
        let phase_claim = ActiveClaim {
            kind: "phase".to_string(),
            resource_key: "plan:p:phase:1".to_string(),
            ttl_seconds: 7200,
            agent_session_id: Some(uuid::Uuid::nil()),
        };
        let mut ctx = IsolatedEditContext::for_test(vec![wa], vec![ca, phase_claim]);
        assert_eq!(ctx.active_claims.len(), 2);

        let released = ctx.release_repo("qontinui-runner").await;
        assert!(released);
        // The phase claim is untouched — only the worktree claim went.
        assert_eq!(ctx.active_claims.len(), 1);
        assert_eq!(ctx.active_claims[0].kind, "phase");

        ctx.active_claims.clear();
        ctx.worktrees.clear();
    }

    #[tokio::test]
    async fn acquire_additional_is_idempotent_for_a_joined_repo() {
        // The grow primitive must early-return Ok(()) WITHOUT a network
        // allocate when the repo is already joined — so a caller can blindly
        // request a repo it may already hold. We assert it neither errors nor
        // mutates the context (a network path against the unroutable base
        // would instead surface an Err).
        let (wa, ca) = repo_fixture("qontinui-runner");
        let mut ctx = IsolatedEditContext::for_test(vec![wa], vec![ca]);
        assert_eq!(ctx.worktrees.len(), 1);
        assert_eq!(ctx.active_claims.len(), 1);

        let r = ctx
            .acquire_additional("qontinui-runner", Some("already joined"))
            .await;
        assert!(r.is_ok(), "joined-repo acquire_additional is a no-op Ok");
        assert_eq!(ctx.worktrees.len(), 1, "no duplicate worktree joined");
        assert_eq!(ctx.active_claims.len(), 1, "no duplicate claim joined");

        ctx.active_claims.clear();
        ctx.worktrees.clear();
    }

    #[test]
    fn session_id_is_carried_from_the_first_claim() {
        // acquire_additional reuses the context's owner-token discriminator
        // so the new repo's claim shares the session owner token. Verify the
        // accessor returns the first claim's session id.
        let sid = uuid::Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
        let (wa, mut ca) = repo_fixture("qontinui-runner");
        ca.agent_session_id = Some(sid);
        let mut ctx = IsolatedEditContext::for_test(vec![wa], vec![ca]);
        assert_eq!(ctx.session_id(), Some(sid));

        // Non-async test: drain before drop (see the note in
        // `multi_repo_context_has_one_worktree_claim_per_repo`).
        ctx.active_claims.clear();
        ctx.worktrees.clear();
    }

    #[tokio::test]
    async fn drop_releases_all_claims() {
        // Drop must release EVERY active claim, not just the first. We assert
        // via the observable side effect available without a coord seam: Drop
        // drains `active_claims` (std::mem::take) and spawns the release. We
        // verify the take-and-spawn happens for a multi-claim context by
        // confirming the spawned task is accepted by the runtime (no panic)
        // and that a SECOND drop of an emptied context is inert.
        let (wa, ca) = repo_fixture("qontinui-runner");
        let (wb, cb) = repo_fixture("qontinui-coord");
        let ctx = IsolatedEditContext::for_test(vec![wa, wb], vec![ca, cb]);
        assert_eq!(ctx.active_claims.len(), 2);
        // Dropping inside a tokio runtime: the Drop impl `tokio::spawn`s the
        // multi-claim release loop. If Drop only released one claim or
        // panicked on >1, this would surface here.
        drop(ctx);

        // An emptied context drops without spawning anything (the
        // `!claims.is_empty()` guard), proving the guard path too.
        let empty = IsolatedEditContext::for_test(Vec::new(), Vec::new());
        drop(empty);

        // Yield so the spawned release task gets a chance to run (and fail
        // fast against the unroutable base) before the runtime tears down.
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn returns_none_when_flag_off() {
        // Mirrors the `flag_off_by_default` test in `mod.rs` — if any
        // other test has flipped `QONTINUI_AGENT_WORKTREE_MODE`, this
        // assertion is moot (the env mutation is global). Skip in that
        // case to keep the suite parallel-safe.
        if !worktree_mode_enabled() {
            let repos = vec!["qontinui-runner".to_string()];
            let ctx = acquire(AcquireRequest {
                repos: &repos,
                intent: None,
                declared_overlap_paths: None,
                plan_id: None,
                phase: None,
                agent_session_id: None,
            })
            .await
            .expect("flag-off should return Ok(None), not Err");
            assert!(ctx.is_none(), "flag-off must return None");
        }
    }

    // ---- Phase 2c: QONTINUI_SESSION_WORKTREES env + --add-dir args ----

    /// Expected `<repo>=<path>` segment for a `repo_fixture`-built worktree —
    /// its `worktree_path` is the repo's `default_canonical_path`, so we
    /// derive both sides from the same helpers (platform-portable; never a
    /// hardcoded `d:/qontinui-root/...` literal).
    fn expected_env_segment(repo: &str) -> String {
        let canonical = super::super::canonical_paths::default_canonical_path(repo).unwrap();
        format!("{}={}", repo, canonical.display())
    }

    #[test]
    fn session_worktrees_env_value_covers_all_repos_in_order() {
        // The env string lists EVERY materialized worktree (incl. repo[0]) as
        // `<repo>=<abs_path>` joined by `;`, in worktree order.
        let (wa, _ca) = repo_fixture("qontinui-runner");
        let (wb, _cb) = repo_fixture("qontinui-coord");
        let worktrees = vec![wa, wb];

        let value = session_worktrees_env_value(&worktrees)
            .expect("multi-repo context yields an env value");

        let expected = format!(
            "{};{}",
            expected_env_segment("qontinui-runner"),
            expected_env_segment("qontinui-coord"),
        );
        assert_eq!(value, expected);

        // Every repo is represented, with its canonical path.
        for repo in ["qontinui-runner", "qontinui-coord"] {
            assert!(
                value.contains(&expected_env_segment(repo)),
                "env value missing segment for {repo}"
            );
        }
        // Exactly one delimiter for two repos.
        assert_eq!(value.matches(';').count(), 1, "one ';' between two repos");
    }

    #[test]
    fn session_worktrees_env_value_is_none_for_empty() {
        assert!(
            session_worktrees_env_value(&[]).is_none(),
            "no worktrees → no env var"
        );
    }

    #[test]
    fn claude_add_dir_args_skips_the_first_worktree() {
        // `--add-dir` is appended once per SIBLING (worktrees[1..]); the first
        // worktree is the process cwd and needs no `--add-dir`.
        let (wa, _ca) = repo_fixture("qontinui-runner");
        let (wb, _cb) = repo_fixture("qontinui-coord");
        let worktrees = vec![wa, wb];

        let args = claude_add_dir_args(&worktrees);

        let coord_path = super::super::canonical_paths::default_canonical_path("qontinui-coord")
            .unwrap()
            .display()
            .to_string();
        assert_eq!(
            args,
            vec!["--add-dir".to_string(), coord_path],
            "exactly one --add-dir pair, for the sibling (non-cwd) repo"
        );
    }

    #[test]
    fn claude_add_dir_args_empty_for_single_repo() {
        let (wa, _ca) = repo_fixture("qontinui-runner");
        assert!(
            claude_add_dir_args(&[wa]).is_empty(),
            "single-repo launch has no sibling --add-dir args"
        );
        assert!(
            claude_add_dir_args(&[]).is_empty(),
            "empty launch has no --add-dir args"
        );
    }

    #[test]
    fn context_accessors_match_freestanding_helpers() {
        // The `IsolatedEditContext` convenience accessors must agree with the
        // freestanding helpers over the same worktrees.
        let (wa, ca) = repo_fixture("qontinui-runner");
        let (wb, cb) = repo_fixture("qontinui-coord");
        let mut ctx = IsolatedEditContext::for_test(vec![wa, wb], vec![ca, cb]);

        assert_eq!(
            ctx.session_worktrees_env_value(),
            session_worktrees_env_value(&ctx.worktrees)
        );
        assert_eq!(
            ctx.claude_add_dir_args(),
            claude_add_dir_args(&ctx.worktrees)
        );

        // Non-async test: drain before drop.
        ctx.active_claims.clear();
        ctx.worktrees.clear();
    }
}
