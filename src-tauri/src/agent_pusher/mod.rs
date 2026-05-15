//! Background pusher daemon (Row 9 Phase 3, runner side).
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-14-failure-modes-at-scale-design.md` §3.2.
//!
//! ## What this does
//!
//! For every allocated agent on this machine, an in-process Tokio task
//! wakes every `~5 minutes` (jittered ±60s per design §3.2 footnote on
//! ref-update storms) and runs:
//!
//! ```text
//! git -C <worktree_path> push <coord-origin>/git/<repo>.git <branch>
//! ```
//!
//! against the coord-hosted git origin (Row 9 Phase 2, §3.4). The push
//! is authenticated by the agent's coord-issued JWT injected into the
//! URL as the `x-access-token` basic-auth user (same scheme coord uses
//! when async-mirroring out to GitHub — see
//! `qontinui-coord/src/outbound_mirror.rs::inject_token`).
//!
//! ## Why it exists
//!
//! Without it, agents on a machine that goes offline have unpushed
//! commits sitting in their local worktrees. When the machine returns
//! from `partitioned`, those commits live only on the local disk —
//! nothing in coord knows about them. The pusher's 5-min cadence
//! ensures the typical worst-case work-loss window is bounded by the
//! cadence + reconnection-recovery time, not by "however long the
//! agent has been working since the last manual push."
//!
//! ## JWT lifecycle
//!
//! Tokens live 4h per Row 9 §3.3. Long-running agent sessions outlive
//! a single token, so the pusher refreshes proactively when the
//! current token has < `TOKEN_REFRESH_MARGIN_SECS` (default 30 min)
//! of life left. The refresh round-trip uses `POST
//! /agents/:id/refresh-token` from the same coord; the new token
//! replaces the cached one and the next push uses it. If the refresh
//! itself fails (coord down, token expired, etc.), the pusher retries
//! on the next tick — failed refreshes don't crash the agent.
//!
//! ## Process model
//!
//! The pusher is a Tokio task, not a separate process — keeping it
//! in-process means it shares the runner's tracing pipeline, dies
//! with the runner, and doesn't need its own credential management.
//! The trade-off (a runner restart kills in-flight pushes) is
//! acceptable: pushes are idempotent (re-running pushes the same
//! refs), so the next tick after restart drains anything that was
//! mid-flight.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Default cadence — 5 minutes per §3.2. Tunable for tests.
const DEFAULT_PUSH_INTERVAL_SECS: u64 = 300;

/// ±jitter window applied to each tick. §3.2's "spread to ~1/sec
/// sustained at 300 agents" math depends on this.
const DEFAULT_JITTER_SECS: u64 = 60;

/// Refresh the JWT proactively when this much time-to-expiry remains.
/// Tokens are 4h per §3.3; refreshing at 30min remaining means the
/// pusher gets ~7 refresh opportunities per token's lifetime in the
/// happy path.
const TOKEN_REFRESH_MARGIN_SECS: i64 = 30 * 60;

/// One worktree the pusher pushes for. Mirrors the shape coord
/// returns from `POST /agents/allocate`, less the suggested-path
/// (we hold the actually-materialized path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushTarget {
    pub repo: String,
    pub branch: String,
    pub worktree_path: PathBuf,
}

/// State shared between the pusher task and any caller that needs to
/// observe / influence it (token refresh, manual flush). The
/// `current_token`, `jti`, and `exp` fields are mutated when refresh
/// succeeds.
#[derive(Debug)]
pub struct PusherState {
    pub agent_id: uuid::Uuid,
    pub coord_http_base: String,
    pub origin_repo_alias: String,
    pub targets: Vec<PushTarget>,
    pub token: RwLock<TokenSlot>,
}

/// Mutable token + bookkeeping. Lives behind an `RwLock` so the
/// pusher can refresh without dropping in-flight push attempts.
#[derive(Debug, Clone)]
pub struct TokenSlot {
    pub token: String,
    pub jti: uuid::Uuid,
    /// Unix-seconds expiry. Refresh fires when `exp - now() <
    /// TOKEN_REFRESH_MARGIN_SECS`.
    pub exp: i64,
}

impl TokenSlot {
    /// Seconds until expiry from `now`.
    pub fn ttl_secs(&self, now_unix: i64) -> i64 {
        self.exp - now_unix
    }

    pub fn needs_refresh(&self, now_unix: i64) -> bool {
        self.ttl_secs(now_unix) < TOKEN_REFRESH_MARGIN_SECS
    }
}

impl PusherState {
    /// Build state directly from an
    /// [`crate::agent_worktree::AllocateResult`]. Returns `None` when
    /// the allocation didn't include a token (coord JWT keys not
    /// configured, dev fallback) — pusher spawn is skipped in that
    /// case and the caller should log + continue.
    pub fn from_allocate_result(
        allocate: &crate::agent_worktree::AllocateResult,
        coord_http_base: String,
    ) -> Option<Self> {
        if allocate.token.is_empty() {
            return None;
        }
        let agent_id = uuid::Uuid::from_str(&allocate.agent_id).ok()?;
        // §3.4 pilot has only qontinui-coord.git; the alias is the
        // repo slug. We collect targets from the materialized worktree
        // list verbatim.
        let targets: Vec<PushTarget> = allocate
            .worktrees
            .iter()
            .map(|w| PushTarget {
                repo: w.repo.clone(),
                branch: w.branch.clone(),
                worktree_path: w.worktree_path.clone(),
            })
            .collect();
        let origin_repo_alias = targets
            .first()
            .map(|t| t.repo.clone())
            .unwrap_or_default();
        Some(Self {
            agent_id,
            coord_http_base,
            origin_repo_alias,
            targets,
            token: RwLock::new(TokenSlot {
                token: allocate.token.clone(),
                jti: allocate.token_jti,
                exp: allocate.token_exp,
            }),
        })
    }
}

/// Spawn the pusher daemon for one agent. Returns immediately;
/// the task lives on the runtime until the agent's worktree set is
/// torn down (the supervisor that owns the agent should drop the
/// returned `PusherHandle`).
///
/// `repo_to_origin_url` builds the per-repo coord origin URL given
/// the coord HTTP base + repo alias. Default form for the pilot is
/// `<base>/git/<repo>.git`.
///
/// Background: a single coord-side scope token covers all repos in
/// the agent's worktree set (one branch name = one `git_push` glob
/// per Row 9 §3.3 + the `default_scopes` helper). So one token serves
/// every push target.
pub fn spawn(state: Arc<PusherState>) -> PusherHandle {
    let interval_secs = std::env::var("QONTINUI_PUSHER_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PUSH_INTERVAL_SECS);
    let jitter_secs = std::env::var("QONTINUI_PUSHER_JITTER_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_JITTER_SECS);
    let cancel = Arc::new(tokio::sync::Notify::new());
    let cancel_for_task = cancel.clone();
    let state_for_task = state.clone();
    let join = tokio::spawn(async move {
        run(state_for_task, interval_secs, jitter_secs, cancel_for_task).await;
    });
    PusherHandle {
        cancel,
        join: Some(join),
        state,
    }
}

/// Returned from [`spawn`]. Drop = signal the pusher to stop on its
/// next tick. Holds the `Arc<PusherState>` so the spawning code can
/// still inspect tokens / forcibly trigger a refresh.
pub struct PusherHandle {
    cancel: Arc<tokio::sync::Notify>,
    join: Option<tokio::task::JoinHandle<()>>,
    pub state: Arc<PusherState>,
}

impl PusherHandle {
    /// Ask the pusher to wake on its next tick rather than wait the
    /// full interval. Useful when the caller already knows there's a
    /// new commit (post-merge-request, etc.).
    pub fn nudge(&self) {
        self.cancel.notify_one();
    }
}

impl Drop for PusherHandle {
    fn drop(&mut self) {
        // notify the task; let it observe shutdown on the next loop
        // iteration. We don't await the JoinHandle in Drop — runner
        // shutdown is best-effort, and a missed final push will be
        // re-attempted on the next runner boot.
        self.cancel.notify_waiters();
        // Detach the join handle. Tokio drops it cleanly.
        let _ = self.join.take();
    }
}

async fn run(
    state: Arc<PusherState>,
    interval_secs: u64,
    jitter_secs: u64,
    cancel: Arc<tokio::sync::Notify>,
) {
    info!(
        "agent_pusher: started agent_id={} repos={} interval={}s ±{}s",
        state.agent_id,
        state.targets.len(),
        interval_secs,
        jitter_secs
    );
    loop {
        let sleep_secs = jittered_interval(interval_secs, jitter_secs);
        let sleep = tokio::time::sleep(Duration::from_secs(sleep_secs));
        tokio::select! {
            _ = sleep => {}
            _ = cancel.notified() => {
                debug!("agent_pusher: agent_id={} cancel notify", state.agent_id);
                // distinguish "nudge" from "shutdown" — a nudge
                // should re-tick. We always re-loop after a notify;
                // PusherHandle::drop calls `notify_waiters` which
                // wakes us. Net effect: one final push attempt then
                // exit on the next iteration if `targets` was emptied.
            }
        }

        if let Err(e) = tick_once(&state).await {
            warn!(
                "agent_pusher: agent_id={} tick failed: {e:#}",
                state.agent_id
            );
        }
    }
}

/// One push cycle. Refresh the token if it's near expiry; iterate
/// every push target; report per-target outcomes.
///
/// Public for the integration test — doesn't depend on the spawn
/// machinery.
pub async fn tick_once(state: &Arc<PusherState>) -> Result<()> {
    maybe_refresh_token(state).await?;
    let token_clone = {
        let guard = state.token.read().await;
        guard.clone()
    };
    for target in &state.targets {
        match push_one(state, target, &token_clone.token).await {
            Ok(true) => info!(
                "agent_pusher: agent_id={} repo={} branch={} pushed",
                state.agent_id, target.repo, target.branch
            ),
            Ok(false) => debug!(
                "agent_pusher: agent_id={} repo={} branch={} up-to-date",
                state.agent_id, target.repo, target.branch
            ),
            Err(e) => warn!(
                "agent_pusher: agent_id={} repo={} branch={} push failed: {e:#}",
                state.agent_id, target.repo, target.branch
            ),
        }
    }
    Ok(())
}

/// Refresh the cached token if it's within `TOKEN_REFRESH_MARGIN_SECS`
/// of expiry. Best-effort: a failed refresh logs a warning and
/// returns Ok — the next tick will retry.
async fn maybe_refresh_token(state: &Arc<PusherState>) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let needs = {
        let guard = state.token.read().await;
        guard.needs_refresh(now)
    };
    if !needs {
        return Ok(());
    }
    info!(
        "agent_pusher: agent_id={} refreshing token (ttl_secs={})",
        state.agent_id,
        state.token.read().await.ttl_secs(now)
    );
    let url = format!(
        "{}/agents/{}/refresh-token",
        state.coord_http_base.trim_end_matches('/'),
        state.agent_id
    );
    let current = state.token.read().await.token.clone();
    let resp = match reqwest::Client::new()
        .post(&url)
        .bearer_auth(&current)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(
                "agent_pusher: refresh request failed: {e} — will retry on next tick"
            );
            return Ok(());
        }
    };
    if !resp.status().is_success() {
        warn!(
            "agent_pusher: refresh returned {} — will retry next tick",
            resp.status()
        );
        return Ok(());
    }
    let body: RefreshResponseBody = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!("agent_pusher: refresh body decode failed: {e}");
            return Ok(());
        }
    };
    let mut guard = state.token.write().await;
    *guard = TokenSlot {
        token: body.token,
        jti: body.jti,
        exp: body.exp,
    };
    info!(
        "agent_pusher: agent_id={} token refreshed jti={} exp={}",
        state.agent_id, guard.jti, guard.exp
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RefreshResponseBody {
    token: String,
    #[allow(dead_code)]
    agent_id: uuid::Uuid,
    jti: uuid::Uuid,
    exp: i64,
}

/// Push one branch to coord-origin via `git push`. Returns
/// `Ok(true)` if anything was pushed, `Ok(false)` for "already up to
/// date", and `Err` for actual failures.
async fn push_one(
    state: &Arc<PusherState>,
    target: &PushTarget,
    token: &str,
) -> Result<bool> {
    let origin_url = build_origin_url(&state.coord_http_base, &target.repo, token)?;
    debug!(
        "agent_pusher: pushing repo={} branch={} from {}",
        target.repo,
        target.branch,
        target.worktree_path.display()
    );
    let out = Command::new("git")
        .arg("-C")
        .arg(&target.worktree_path)
        .arg("push")
        .arg("--porcelain")
        .arg("--no-verify")
        .arg(&origin_url)
        .arg(format!("{}:{}", target.branch, target.branch))
        .output()
        .await
        .with_context(|| format!("invoking git push for {}", target.branch))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Exit code 1 with "Everything up-to-date" is normal in some
        // git versions — treat as success-no-op.
        if stderr.contains("Everything up-to-date") {
            return Ok(false);
        }
        anyhow::bail!(
            "git push exited {:?}: {}",
            out.status.code(),
            stderr.trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Same parsing rule as outbound_mirror — count lines whose first
    // non-space char isn't `=` (= meaning "up to date") or markers.
    let pushed = stdout.lines().any(|l| {
        let l = l.trim_start();
        !l.is_empty()
            && !l.starts_with('=')
            && !l.starts_with("To ")
            && !l.starts_with("Done")
    });
    Ok(pushed)
}

/// `<base>/git/<repo>.git` → `<authed-base>/git/<repo>.git`. Token is
/// injected as the `x-access-token` basic-auth user, matching coord's
/// outbound_mirror::inject_token convention.
pub fn build_origin_url(base: &str, repo: &str, token: &str) -> Result<String> {
    let base = base.trim_end_matches('/');
    let prefix = if let Some(rest) = base.strip_prefix("https://") {
        ("https://", rest)
    } else if let Some(rest) = base.strip_prefix("http://") {
        ("http://", rest)
    } else {
        anyhow::bail!("coord_http_base must be http[s]://, got {base:?}");
    };
    let repo_path = repo.strip_suffix(".git").unwrap_or(repo);
    Ok(format!(
        "{}x-access-token:{}@{}/git/{}.git",
        prefix.0, token, prefix.1, repo_path
    ))
}

/// Pick a sleep duration in `[interval - jitter, interval + jitter]`.
/// Jitter is symmetric so the long-run mean equals `interval`.
pub fn jittered_interval(interval_secs: u64, jitter_secs: u64) -> u64 {
    use rand::Rng;
    if jitter_secs == 0 {
        return interval_secs;
    }
    let mut rng = rand::rng();
    let lo = interval_secs.saturating_sub(jitter_secs);
    let hi = interval_secs.saturating_add(jitter_secs);
    rng.random_range(lo..=hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_slot_needs_refresh() {
        let now = 1_700_000_000;
        // Plenty of time left → no refresh.
        let t = TokenSlot {
            token: "x".into(),
            jti: uuid::Uuid::nil(),
            exp: now + 4 * 3600,
        };
        assert!(!t.needs_refresh(now));
        // 10 min left → refresh.
        let t2 = TokenSlot {
            token: "x".into(),
            jti: uuid::Uuid::nil(),
            exp: now + 600,
        };
        assert!(t2.needs_refresh(now));
        // 30 min - 1s left → refresh (just under the margin).
        let t3 = TokenSlot {
            token: "x".into(),
            jti: uuid::Uuid::nil(),
            exp: now + TOKEN_REFRESH_MARGIN_SECS - 1,
        };
        assert!(t3.needs_refresh(now));
    }

    #[test]
    fn build_origin_url_injects_token_https() {
        let url = build_origin_url("https://coord.example/", "qontinui-coord", "abc123").unwrap();
        assert_eq!(
            url,
            "https://x-access-token:abc123@coord.example/git/qontinui-coord.git"
        );
    }

    #[test]
    fn build_origin_url_strips_dot_git_suffix() {
        let url = build_origin_url("http://h:9870", "qontinui-coord.git", "tk").unwrap();
        assert_eq!(
            url,
            "http://x-access-token:tk@h:9870/git/qontinui-coord.git"
        );
    }

    #[test]
    fn build_origin_url_rejects_non_http() {
        assert!(build_origin_url("ws://h:9870", "qontinui-coord", "x").is_err());
        assert!(build_origin_url("ftp://h", "r", "t").is_err());
    }

    #[test]
    fn jitter_zero_yields_exact_interval() {
        assert_eq!(jittered_interval(300, 0), 300);
    }

    #[test]
    fn jitter_stays_in_band() {
        for _ in 0..100 {
            let v = jittered_interval(300, 60);
            assert!((240..=360).contains(&v), "out-of-band: {v}");
        }
    }

    #[test]
    fn jitter_saturates_at_zero_for_large_jitter() {
        // jitter > interval should not panic; lo saturates to 0.
        for _ in 0..10 {
            let v = jittered_interval(30, 100);
            assert!(v <= 130);
        }
    }

    #[tokio::test]
    async fn pusher_handle_drops_cleanly() {
        let state = Arc::new(PusherState {
            agent_id: uuid::Uuid::nil(),
            coord_http_base: "http://invalid:1".into(),
            origin_repo_alias: "qontinui-coord".into(),
            targets: vec![],
            token: RwLock::new(TokenSlot {
                token: "x".into(),
                jti: uuid::Uuid::nil(),
                exp: chrono::Utc::now().timestamp() + 4 * 3600,
            }),
        });
        let handle = spawn(state);
        // Drop immediately — should not hang or panic.
        drop(handle);
        // Give the runtime a moment to run the cancellation path.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
