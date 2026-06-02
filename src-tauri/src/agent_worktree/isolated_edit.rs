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
//! (`/manual-test-loop`, `/manual-test`, `/implement-plan`) via the
//! existing HTTP face at
//! [`crate::mcp::agent_worktrees::post_allocate_local`].
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
    /// Set when the spawn flow pre-acquired a coord claim. `None` means
    /// no claim was acquired (the caller passed neither `plan_id`/`phase`
    /// nor `declared_overlap_paths`).
    pub active_claim: Option<ActiveClaim>,

    // Held so the heartbeat task lives as long as the context — its
    // own Drop notifies the cancel token. Underscore-prefixed because
    // we never inspect it; the handle is keep-alive only.
    _heartbeat: Option<ClaimHeartbeatHandle>,

    // Captured for the Drop-time release POST. `coord_http_base` and
    // `device_id` aren't otherwise exposed on the context — Drop is the
    // only consumer.
    coord_http_base: String,
    device_id: uuid::Uuid,
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

        // Stop the heartbeat first so its tick can't race with the
        // release POST. `ClaimHeartbeatHandle`'s own Drop notifies its
        // cancel token; the task exits on the next tick boundary.
        let _ = self._heartbeat.take();

        // Best-effort release on the current Tokio runtime. If there
        // was no active claim, there's nothing to release. A failed
        // release is observability-only — the session that drops the
        // context is shutting down anyway.
        if let Some(claim) = self.active_claim.take() {
            let base = self.coord_http_base.clone();
            let mid = self.device_id;
            tokio::spawn(async move {
                release_claim_best_effort(&base, mid, &claim.kind, &claim.resource_key).await;
            });
        }
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

    let mut canonical_paths: HashMap<String, PathBuf> = HashMap::with_capacity(req.repos.len());
    for repo in req.repos {
        let path = super::canonical_paths::default_canonical_path(repo)
            .map_err(|e| AllocateError::Other(format!("canonical path for repo {repo:?}: {e}")))?;
        canonical_paths.insert(repo.clone(), path);
    }

    let mut repo_reqs: Vec<RepoRequest> = Vec::with_capacity(req.repos.len());
    for repo in req.repos {
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
        &coord_http_base,
        &device_id,
        &repo_reqs,
        req.intent,
        req.declared_overlap_paths,
        &canonical_paths,
        req.plan_id,
        req.phase,
    )
    .await?;

    // Phase 3 — normalize coord's isolation directive into the
    // worktree-shaped `IsolatedEditContext`:
    // - `Worktrees` → as-is.
    // - `SharedBranch` → the canonical checkout IS the cwd; map each
    //   per-repo branch into a `MaterializedWorktree` whose
    //   `worktree_path` is the canonical checkout, so callers that read
    //   `worktrees[0].worktree_path` get the right cwd (the shared
    //   checkout) transparently.
    // - `Wait` → surface as `Err(Other)` with a `wait:` prefix so the
    //   caller logs/retries (it does NOT spin here). `acquire_for_terminal`
    //   degrades to the shared cwd on this Err, which is the safe fallback.
    let result: super::AllocateResult = match outcome {
        MaterializeOutcome::Worktrees(r) => r,
        MaterializeOutcome::SharedBranch(sb) => super::AllocateResult {
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
            active_claim: sb.active_claim,
        },
        MaterializeOutcome::Wait(w) => {
            return Err(AllocateError::Other(format!(
                "wait: coord declined to materialize agent_id={} reason={:?} blocking={:?} retry_when={:?}",
                w.agent_id, w.reason, w.blocking, w.retry_when
            )));
        }
    };

    let heartbeat = result.active_claim.as_ref().map(|claim| {
        info!(
            agent_id = %result.agent_id,
            kind = %claim.kind,
            resource_key = %claim.resource_key,
            ttl_seconds = claim.ttl_seconds,
            "isolated_edit: spawning heartbeat"
        );
        spawn_heartbeat_task(
            coord_http_base.clone(),
            device_id,
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
    });

    Ok(Some(IsolatedEditContext {
        agent_id: result.agent_id,
        worktrees: result.worktrees,
        active_claim: result.active_claim,
        _heartbeat: heartbeat,
        coord_http_base,
        device_id,
    }))
}

/// Convenience helper for the three runner terminal-spawn entry points
/// (`commands/terminal.rs::terminal_create`, `commands/productivity.rs::
/// spawn_worker_session`, `mcp/tauri_proxy.rs::"terminal_create"`,
/// `mcp/backend_relay.rs::handle_terminal_create`). Given the caller's
/// `intent_repo` + `purpose` + original `working_dir`, returns the
/// effective working dir for the PTY plus an `IsolatedEditContext` to
/// park on the resulting `TerminalSession`.
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
mod tests {
    use super::*;

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
            })
            .await
            .expect("flag-off should return Ok(None), not Err");
            assert!(ctx.is_none(), "flag-off must return None");
        }
    }
}
