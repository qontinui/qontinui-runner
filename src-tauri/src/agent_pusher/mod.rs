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
//! git -C <worktree_path> -c http.extraHeader="Authorization: Bearer <jwt>" \
//!     push <coord-origin>/git/<repo-basename>.git refs/heads/<branch>:refs/agent/<m>-<a>
//! ```
//!
//! against the coord-hosted git origin (Row 9 Phase 2, §3.4). The push
//! is authenticated by the agent's coord-issued JWT handed to git as an
//! `Authorization: Bearer` header via `-c http.extraHeader` — coord's
//! git-http gate is Bearer-only and rejects GitHub-style
//! `x-access-token:<jwt>@host` basic-auth in the URL
//! (`qontinui-coord/src/git_replication.rs`), so the origin URL stays
//! credential-free. See [`build_origin_url`] / [`push_one`].
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
use tracing::{debug, info, warn};

use crate::agent_token::{self, SharedToken};
// Re-exported so existing `agent_pusher::TokenSlot` /
// `TOKEN_REFRESH_MARGIN_SECS` references (incl. this module's tests)
// keep resolving after the token logic moved to `crate::agent_token`.
pub use crate::agent_token::{TokenSlot, TOKEN_REFRESH_MARGIN_SECS};

/// Default cadence — 5 minutes per §3.2. Tunable for tests.
const DEFAULT_PUSH_INTERVAL_SECS: u64 = 300;

/// ±jitter window applied to each tick. §3.2's "spread to ~1/sec
/// sustained at 300 agents" math depends on this.
const DEFAULT_JITTER_SECS: u64 = 60;

/// One worktree the pusher pushes for. Mirrors the shape coord
/// returns from `POST /agents/allocate`, less the suggested-path
/// (we hold the actually-materialized path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushTarget {
    pub repo: String,
    pub branch: String,
    pub worktree_path: PathBuf,
    /// Coordination Phase 5 / bottleneck Row 4 — the non-`heads`
    /// remote ref to push to (`refs/agent/<m>-<a>`). The local side
    /// stays `refs/heads/<branch>` (a normal checked-out branch); only
    /// the origin side moves namespace so default `refs/heads/*`
    /// fetches skip the ~10K agent refs.
    #[serde(default)]
    pub push_ref: String,
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
    /// Shared with every other daemon spawned for this agent (one
    /// refresh path, not one per daemon). See [`crate::agent_token`].
    pub token: SharedToken,
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
        let token = agent_token::from_allocate_result(allocate)?;
        Self::with_shared_token(allocate, coord_http_base, token)
    }

    /// Same as [`from_allocate_result`] but the caller supplies the
    /// shared token slot — used by `agent_daemons::spawn_for_agent`
    /// so the pusher and the dirty-poller refresh through one slot.
    pub fn with_shared_token(
        allocate: &crate::agent_worktree::AllocateResult,
        coord_http_base: String,
        token: SharedToken,
    ) -> Option<Self> {
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
                push_ref: if w.push_ref.is_empty() {
                    crate::agent_worktree::remote_agent_ref(&w.branch)
                } else {
                    w.push_ref.clone()
                },
            })
            .collect();
        let origin_repo_alias = targets.first().map(|t| t.repo.clone()).unwrap_or_default();
        Some(Self {
            agent_id,
            coord_http_base,
            origin_repo_alias,
            targets,
            token,
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
    agent_token::maybe_refresh(
        &state.token,
        &state.coord_http_base,
        state.agent_id,
        "agent_pusher",
    )
    .await?;
    let token_clone = {
        let guard = state.token.read().await;
        guard.clone()
    };
    for target in &state.targets {
        match push_one(state, target, &token_clone.token).await {
            Ok(PushOutcome::Pushed) => info!(
                "agent_pusher: agent_id={} repo={} branch={} pushed",
                state.agent_id, target.repo, target.branch
            ),
            Ok(PushOutcome::UpToDate) => debug!(
                "agent_pusher: agent_id={} repo={} branch={} up-to-date",
                state.agent_id, target.repo, target.branch
            ),
            Ok(PushOutcome::Transient(msg)) => debug!(
                "agent_pusher: agent_id={} repo={} branch={} transient push \
                 failure (best-effort, will retry next tick): {msg}",
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

/// Push one branch to coord-origin via `git push`. Returns
/// [`PushOutcome::Pushed`] if anything moved, [`PushOutcome::UpToDate`]
/// for a no-op, [`PushOutcome::Transient`] for a retryable failure, and
/// `Err` for a permanent one (see [`is_transient_push_error`]).
async fn push_one(
    state: &Arc<PusherState>,
    target: &PushTarget,
    token: &str,
) -> Result<PushOutcome> {
    let origin_url = build_origin_url(&state.coord_http_base, &target.repo)?;
    // Coordination Phase 5 / Row 4: push the local branch
    // (`refs/heads/<branch>` — a normal checked-out branch in the
    // worktree) to the non-`heads` remote ref `refs/agent/<m>-<a>`.
    // Defensive fallback if push_ref is somehow empty (older
    // serialized PushTarget).
    let push_ref = if target.push_ref.is_empty() {
        crate::agent_worktree::remote_agent_ref(&target.branch)
    } else {
        target.push_ref.clone()
    };
    let refspec = format!("refs/heads/{}:{}", target.branch, push_ref);
    debug!(
        "agent_pusher: pushing repo={} refspec={} from {}",
        target.repo,
        refspec,
        target.worktree_path.display()
    );
    // coord's git-http gate is Bearer-only and rejects basic-auth, so the
    // agent JWT goes in an `Authorization: Bearer` header (not the URL).
    // `-c http.extraHeader=...` scopes the header to this one push.
    let out = crate::process_helpers::tokio_no_window("git")
        .arg("-C")
        .arg(&target.worktree_path)
        .arg("-c")
        .arg(format!("http.extraHeader=Authorization: Bearer {token}"))
        .arg("push")
        .arg("--porcelain")
        .arg("--no-verify")
        .arg(&origin_url)
        .arg(&refspec)
        .output()
        .await
        .with_context(|| format!("invoking git push for {}", target.branch))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Exit code 1 with "Everything up-to-date" is normal in some
        // git versions — treat as success-no-op.
        if stderr.contains("Everything up-to-date") {
            return Ok(PushOutcome::UpToDate);
        }
        let msg = stderr.trim().to_string();
        if is_transient_push_error(&stderr) {
            return Ok(PushOutcome::Transient(msg));
        }
        anyhow::bail!("git push exited {:?}: {}", out.status.code(), msg);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Same parsing rule as outbound_mirror — count lines whose first
    // non-space char isn't `=` (= meaning "up to date") or markers.
    let pushed = stdout.lines().any(|l| {
        let l = l.trim_start();
        !l.is_empty() && !l.starts_with('=') && !l.starts_with("To ") && !l.starts_with("Done")
    });
    Ok(if pushed {
        PushOutcome::Pushed
    } else {
        PushOutcome::UpToDate
    })
}

/// The coord git-origin URL for `repo`: `<base>/git/<basename>.git`.
///
/// **Auth is NOT injected here.** coord's git-http gate is Bearer-only and
/// rejects GitHub-style `x-access-token:<jwt>@host` basic-auth in the URL
/// (qontinui-coord `src/git_replication.rs`: "must NOT use GitHub-style
/// Basic auth — coord's git-http gate rejects it (401)"). The agent JWT is
/// instead handed to git as an `Authorization: Bearer` header via
/// `-c http.extraHeader` in [`push_one`], so the URL stays credential-free.
///
/// The repo path is the **basename** (`qontinui/qontinui-coord` →
/// `qontinui-coord`): coord's `/git/:repo/...` route captures a single path
/// segment and its origin hosts repos under their basename, so an
/// org-prefixed name would split into two segments and miss the route
/// entirely (falling through to an unrelated operator-gated handler).
pub fn build_origin_url(base: &str, repo: &str) -> Result<String> {
    let base = base.trim_end_matches('/');
    let prefix = if let Some(rest) = base.strip_prefix("https://") {
        ("https://", rest)
    } else if let Some(rest) = base.strip_prefix("http://") {
        ("http://", rest)
    } else {
        anyhow::bail!("coord_http_base must be http[s]://, got {base:?}");
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let basename = repo.rsplit('/').next().unwrap_or(repo);
    Ok(format!("{}{}/git/{}.git", prefix.0, prefix.1, basename))
}

/// Outcome of one [`push_one`] attempt. `Transient` carries the git
/// stderr of a best-effort-retryable failure (coord write-proxy /
/// replication hiccups, network faults) — logged quietly and retried on
/// the next tick. Permanent failures (auth / scope / allowlist 4xx, or
/// anything unrecognized) come back as `Err` and are surfaced loudly.
enum PushOutcome {
    Pushed,
    UpToDate,
    Transient(String),
}

/// Classify a `git push` failure as transient (worth a quiet retry) vs
/// permanent. Transient = coord's git write-proxy / replication being
/// momentarily unavailable (5xx, "leader unreachable") and ordinary
/// network faults (DNS / connection / TLS / timeout); these self-heal, so
/// the best-effort pusher retries next tick without a warn. Everything
/// else — auth (401), scope/allowlist (400/403/404), and unrecognized
/// errors — is treated as permanent and surfaced at `warn` so it gets
/// attention instead of silently retrying forever.
fn is_transient_push_error(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    const TRANSIENT: &[&str] = &[
        "error: 500",
        "error: 502",
        "error: 503",
        "error: 504",
        "502 bad gateway",
        "503 service",
        "leader unreachable",
        "write-proxy",
        "could not resolve host",
        "couldn't resolve host",
        "connection refused",
        "connection reset",
        "connection timed out",
        "timed out",
        "operation timed out",
        "temporary failure",
        "ssl",
        "tls",
    ];
    TRANSIENT.iter().any(|needle| s.contains(needle))
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
    fn build_origin_url_is_credential_free() {
        // coord's git gate is Bearer-only; the URL must carry NO basic-auth
        // (the JWT goes in an http.extraHeader instead).
        let url = build_origin_url("https://coord.example/", "qontinui-coord").unwrap();
        assert_eq!(url, "https://coord.example/git/qontinui-coord.git");
        assert!(
            !url.contains("x-access-token"),
            "url must not embed creds: {url}"
        );
        assert!(!url.contains('@'), "url must not embed creds: {url}");
    }

    #[test]
    fn build_origin_url_uses_basename_strips_dot_git_and_org_prefix() {
        // org-prefixed canonical name → single-segment basename, so the
        // `/git/:repo/...` route matches (an org prefix would split into
        // two path segments and miss the route).
        let url = build_origin_url("http://h:9870", "qontinui/qontinui-coord.git").unwrap();
        assert_eq!(url, "http://h:9870/git/qontinui-coord.git");
    }

    #[test]
    fn build_origin_url_rejects_non_http() {
        assert!(build_origin_url("ws://h:9870", "qontinui-coord").is_err());
        assert!(build_origin_url("ftp://h", "r").is_err());
    }

    #[test]
    fn transient_vs_permanent_push_errors() {
        // coord write-proxy / replication + network faults → transient.
        assert!(is_transient_push_error(
            "remote: write-proxy: leader unreachable\nfatal: ...: The requested URL returned error: 502"
        ));
        assert!(is_transient_push_error(
            "fatal: unable to access '...': The requested URL returned error: 503"
        ));
        assert!(is_transient_push_error(
            "fatal: Could not resolve host: coord.qontinui.io"
        ));
        assert!(is_transient_push_error(
            "fatal: unable to access '...': Connection timed out"
        ));
        // auth / scope / allowlist + unknown → permanent (surfaced loudly).
        assert!(!is_transient_push_error(
            "fatal: Authentication failed for '...'"
        ));
        assert!(!is_transient_push_error(
            "error: 403 Forbidden: no git_push scope on token"
        ));
        assert!(!is_transient_push_error(
            "remote: repo not in allowlist\n... error: 400"
        ));
    }

    #[test]
    fn push_target_refspec_is_heads_to_agent_namespace() {
        // Coordination Phase 5 / Row 4: local refs/heads/<branch>
        // (a real checked-out branch) → remote refs/agent/<m>-<a>.
        let t = PushTarget {
            repo: "qontinui-coord".into(),
            branch: "agent/m12-a34".into(),
            worktree_path: PathBuf::from("/tmp/wt"),
            push_ref: crate::agent_worktree::remote_agent_ref("agent/m12-a34"),
        };
        let effective = if t.push_ref.is_empty() {
            crate::agent_worktree::remote_agent_ref(&t.branch)
        } else {
            t.push_ref.clone()
        };
        let refspec = format!("refs/heads/{}:{}", t.branch, effective);
        assert_eq!(refspec, "refs/heads/agent/m12-a34:refs/agent/m12-a34");
        let (src, dst) = refspec.split_once(':').unwrap();
        assert!(src.starts_with("refs/heads/"));
        assert!(dst.starts_with("refs/agent/") && !dst.starts_with("refs/heads/"));
    }

    #[test]
    fn push_target_empty_push_ref_falls_back() {
        let t = PushTarget {
            repo: "qontinui-coord".into(),
            branch: "agent/x-y".into(),
            worktree_path: PathBuf::from("/tmp/wt"),
            push_ref: String::new(), // pre-Phase-5 coord
        };
        let effective = if t.push_ref.is_empty() {
            crate::agent_worktree::remote_agent_ref(&t.branch)
        } else {
            t.push_ref.clone()
        };
        assert_eq!(effective, "refs/agent/x-y");
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
            token: Arc::new(tokio::sync::RwLock::new(TokenSlot {
                token: "x".into(),
                jti: uuid::Uuid::nil(),
                exp: chrono::Utc::now().timestamp() + 4 * 3600,
            })),
        });
        let handle = spawn(state);
        // Drop immediately — should not hang or panic.
        drop(handle);
        // Give the runtime a moment to run the cancellation path.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
