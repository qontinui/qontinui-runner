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
//!     push <coord-origin>/git/<owner>/<repo>.git refs/heads/<branch>:refs/agent/<m>-<a>
//! ```
//!
//! against the coord-hosted git origin (Row 9 Phase 2, §3.4). Since the
//! 2026-07-12 incident (89 pushers re-sending full packs every 5 min for
//! ~5.5h against a 503ing git door — 3,335 silent refusals) each push is
//! bounded: a hard tokio timeout with `kill_on_drop`, git low-speed
//! abort thresholds, and per-target exponential backoff (base cadence
//! doubling to a 1h cap) with failures promoted to `warn!` after
//! [`NOISY_FAILURE_THRESHOLD`] consecutive misses. The push
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
use tokio_util::sync::CancellationToken;
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

/// Hard wall-clock limit on a single `git push` child (2026-07-12
/// incident hardening: a bare `.output().await` against a stalled
/// server hangs forever). Overridable via
/// `QONTINUI_PUSHER_PUSH_TIMEOUT_SECS`.
const DEFAULT_PUSH_TIMEOUT_SECS: u64 = 120;

/// Ceiling for the per-target exponential backoff (1h). With the
/// default 5-min cadence the delay ladder is 5m→10m→20m→40m→60m.
const BACKOFF_CAP_SECS: u64 = 3600;

/// Consecutive-failure count at which transient-failure logging is
/// promoted from `debug!` to `warn!`. The 2026-07-12 incident produced
/// 3,335 silent 503 refusals over ~5.5h — after this change the third
/// consecutive failure per target is loudly visible.
const NOISY_FAILURE_THRESHOLD: u32 = 3;

/// The tick cadence in seconds (`QONTINUI_PUSHER_INTERVAL_SECS`,
/// default 300). Also the base of the backoff ladder.
fn push_interval_secs() -> u64 {
    std::env::var("QONTINUI_PUSHER_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PUSH_INTERVAL_SECS)
}

/// Hard per-push timeout (`QONTINUI_PUSHER_PUSH_TIMEOUT_SECS`,
/// default 120s). Thin env wrapper — the timeout itself is injected
/// into [`push_one`] as an argument so tests never mutate global env.
fn push_timeout() -> Duration {
    let secs = std::env::var("QONTINUI_PUSHER_PUSH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PUSH_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

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
    /// Per-target retry/backoff bookkeeping, indexed parallel to
    /// `targets` (2026-07-12 incident hardening). Behind a Mutex only
    /// because the state lives in an `Arc` — ticks are single-flight
    /// ([`run`] awaits [`tick_once`] before sleeping again), so the
    /// lock is never contended; it is NOT concurrency control.
    pub backoff: tokio::sync::Mutex<Vec<BackoffState>>,
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
        let backoff = tokio::sync::Mutex::new(vec![BackoffState::default(); targets.len()]);
        Some(Self {
            agent_id,
            coord_http_base,
            origin_repo_alias,
            targets,
            token,
            backoff,
        })
    }
}

/// Per-target push backoff state (2026-07-12 incident hardening).
/// Pure data — all transitions take `now` / base / cap as arguments so
/// tests drive the machine without clocks or global env.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackoffState {
    /// Consecutive failed push attempts (transient, timed-out, or
    /// permanent). Reset to 0 on any success (incl. up-to-date no-op).
    pub consecutive_failures: u32,
    /// Unix seconds before which the target is skipped on a tick.
    pub next_attempt_epoch_secs: u64,
}

impl BackoffState {
    /// True while the target is inside its backoff window.
    pub fn should_skip(&self, now_secs: u64) -> bool {
        now_secs < self.next_attempt_epoch_secs
    }

    /// Record one failed attempt: bump the counter and push
    /// `next_attempt_epoch_secs` out by the (doubled, capped) delay.
    pub fn record_failure(&mut self, now_secs: u64, base_secs: u64, cap_secs: u64) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.next_attempt_epoch_secs = now_secs.saturating_add(backoff_delay_secs(
            base_secs,
            self.consecutive_failures,
            cap_secs,
        ));
    }

    /// Any successful push (or up-to-date no-op) fully resets the ladder.
    pub fn record_success(&mut self) {
        *self = Self::default();
    }
}

/// Delay before the next attempt after `consecutive_failures` failures:
/// starts at the base cadence and doubles per further failure, capped
/// (with the defaults: 5m→10m→20m→40m→60m, then 60m forever).
pub fn backoff_delay_secs(base_secs: u64, consecutive_failures: u32, cap_secs: u64) -> u64 {
    if consecutive_failures == 0 {
        return 0;
    }
    let mut delay = base_secs.min(cap_secs);
    for _ in 1..consecutive_failures {
        if delay >= cap_secs {
            return cap_secs;
        }
        delay = delay.saturating_mul(2);
    }
    delay.min(cap_secs)
}

/// Fold one push attempt's outcome into the target's backoff state.
/// Returns `true` when the failure streak has reached
/// [`NOISY_FAILURE_THRESHOLD`] and the caller must log at `warn!`.
/// Pure over its arguments — unit-testable without env or clocks.
fn record_outcome(
    b: &mut BackoffState,
    failed: bool,
    now_secs: u64,
    base_secs: u64,
    cap_secs: u64,
) -> bool {
    if !failed {
        b.record_success();
        return false;
    }
    b.record_failure(now_secs, base_secs, cap_secs);
    b.consecutive_failures >= NOISY_FAILURE_THRESHOLD
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Spawn the pusher daemon for one agent. Returns immediately;
/// the task lives on the runtime until the agent's worktree set is
/// torn down (the supervisor that owns the agent should drop the
/// returned `PusherHandle`).
///
/// `repo_to_origin_url` builds the per-repo coord origin URL given
/// the coord HTTP base + repo alias. Form is the owner-qualified
/// `<base>/git/<owner>/<repo>.git` (see [`build_origin_url`]).
///
/// Background: a single coord-side scope token covers all repos in
/// the agent's worktree set (one branch name = one `git_push` glob
/// per Row 9 §3.3 + the `default_scopes` helper). So one token serves
/// every push target.
pub fn spawn(state: Arc<PusherState>) -> PusherHandle {
    let interval_secs = push_interval_secs();
    let jitter_secs = std::env::var("QONTINUI_PUSHER_JITTER_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_JITTER_SECS);
    let cancel = CancellationToken::new();
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

/// Returned from [`spawn`]. Drop = stop the pusher, now. Holds the
/// `Arc<PusherState>` so the spawning code can still inspect tokens /
/// forcibly trigger a refresh.
///
/// There used to be a `nudge()` here — "wake on the next tick rather than
/// wait the full interval" — which shared ONE `Notify` with shutdown. That
/// made "push now" and "stop forever" literally the same signal, which is
/// why [`run`]'s cancel branch was written to fall through and re-tick
/// instead of returning: it could not tell the two apart. `nudge()` had no
/// callers anywhere in the tree, so it is deleted rather than given a
/// second channel — the ambiguity it created was the whole cost of keeping
/// it. If an on-demand push is wanted later, add a dedicated channel then.
pub struct PusherHandle {
    cancel: CancellationToken,
    join: Option<tokio::task::JoinHandle<()>>,
    pub state: Arc<PusherState>,
}

impl Drop for PusherHandle {
    /// Cancel, then abort — the same fix as [`crate::dirty_poller`]'s
    /// handle, for the same two reasons.
    ///
    /// `Notify::notify_waiters()` stored no permit, so a shutdown that
    /// arrived while the task was inside `tick_once` was discarded; and
    /// dropping the `JoinHandle` detached the task rather than aborting
    /// it, so nothing caught the miss. The pusher had a third fault on
    /// top: its `run` loop had no `return` on the cancel branch at all,
    /// so even a perfectly delivered signal only produced one more push.
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

async fn run(
    state: Arc<PusherState>,
    interval_secs: u64,
    jitter_secs: u64,
    cancel: CancellationToken,
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
            _ = cancel.cancelled() => {
                debug!("agent_pusher: agent_id={} stopping", state.agent_id);
                return;
            }
        }

        // Cancellation must beat the tick, not merely the sleep: a push
        // can take a long time, and the whole point of the latched token
        // is that teardown arriving mid-push is still honoured.
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                debug!(
                    "agent_pusher: agent_id={} cancelled mid-push",
                    state.agent_id
                );
                return;
            }
            res = tick_once(&state) => res,
        };

        if let Err(e) = result {
            warn!(
                "agent_pusher: agent_id={} tick failed: {e:#}",
                state.agent_id
            );
        }
    }
}

/// One push cycle. Refresh the token if it's near expiry; iterate
/// every push target (skipping any inside its backoff window); report
/// per-target outcomes and update per-target backoff state.
///
/// Public for the integration test — doesn't depend on the spawn
/// machinery.
///
/// Single-flight: [`run`] awaits `tick_once` before sleeping again, so
/// ticks never overlap for one pusher. The `state.backoff` lock below
/// is therefore uncontended — it exists only to mutate through the
/// `Arc`, not as concurrency control; do not add further locking here.
pub async fn tick_once(state: &Arc<PusherState>) -> Result<()> {
    let refresh = agent_token::maybe_refresh(
        &state.token,
        &state.coord_http_base,
        state.agent_id,
        "agent_pusher",
    )
    .await?;
    // Coord refused this bearer, and the push targets authenticate with
    // the same token — every push below would be rejected. Skip rather
    // than climb the per-target backoff ladder over a cause no target
    // can fix. Sibling of the `dirty_poller` guard; see
    // `agent_token::RefreshOutcome`.
    if refresh.should_skip_work() {
        debug!(
            "agent_pusher: agent_id={} skipping tick — token rejected by coord",
            state.agent_id
        );
        return Ok(());
    }
    let token_clone = {
        let guard = state.token.read().await;
        guard.clone()
    };
    // Backoff base = the tick cadence: one failure delays the target by
    // one normal tick, then doubles per consecutive failure up to
    // BACKOFF_CAP_SECS (2026-07-12 incident: 89 pushers re-sent full
    // packs every 5 min for ~5.5h against a 503ing door — 3,335
    // refusals with zero visible signal).
    let base_secs = push_interval_secs();
    let timeout = push_timeout();
    let mut backoff = state.backoff.lock().await;
    for (idx, target) in state.targets.iter().enumerate() {
        let b = match backoff.get_mut(idx) {
            Some(b) => b,
            None => {
                // targets/backoff are built parallel; defensive only.
                backoff.resize_with(idx + 1, BackoffState::default);
                &mut backoff[idx]
            }
        };
        let now = now_epoch_secs();
        if b.should_skip(now) {
            debug!(
                "agent_pusher: agent_id={} repo={} branch={} skipping — \
                 {} consecutive failure(s), backing off until epoch {} ({}s left)",
                state.agent_id,
                target.repo,
                target.branch,
                b.consecutive_failures,
                b.next_attempt_epoch_secs,
                b.next_attempt_epoch_secs.saturating_sub(now)
            );
            continue;
        }
        match push_one(state, target, &token_clone.token, timeout).await {
            Ok(PushOutcome::Pushed) => {
                b.record_success();
                info!(
                    "agent_pusher: agent_id={} repo={} branch={} pushed",
                    state.agent_id, target.repo, target.branch
                );
            }
            Ok(PushOutcome::UpToDate) => {
                b.record_success();
                debug!(
                    "agent_pusher: agent_id={} repo={} branch={} up-to-date",
                    state.agent_id, target.repo, target.branch
                );
            }
            Ok(PushOutcome::Transient(msg)) => {
                let loud = record_outcome(b, true, now, base_secs, BACKOFF_CAP_SECS);
                if loud {
                    warn!(
                        "agent_pusher: agent_id={} repo={} branch={} transient push \
                         failure #{} (backing off until epoch {}, ~{}s): {msg}",
                        state.agent_id,
                        target.repo,
                        target.branch,
                        b.consecutive_failures,
                        b.next_attempt_epoch_secs,
                        b.next_attempt_epoch_secs.saturating_sub(now)
                    );
                } else {
                    debug!(
                        "agent_pusher: agent_id={} repo={} branch={} transient push \
                         failure #{} (best-effort, next attempt after epoch {}): {msg}",
                        state.agent_id,
                        target.repo,
                        target.branch,
                        b.consecutive_failures,
                        b.next_attempt_epoch_secs
                    );
                }
            }
            Ok(PushOutcome::TimedOut(elapsed)) => {
                // Always loud — push_one already warned with the
                // worktree + refspec; this records it for backoff.
                record_outcome(b, true, now, base_secs, BACKOFF_CAP_SECS);
                warn!(
                    "agent_pusher: agent_id={} repo={} branch={} push timed out \
                     after {:.1}s (failure #{}, backing off until epoch {})",
                    state.agent_id,
                    target.repo,
                    target.branch,
                    elapsed.as_secs_f64(),
                    b.consecutive_failures,
                    b.next_attempt_epoch_secs
                );
            }
            Err(e) => {
                record_outcome(b, true, now, base_secs, BACKOFF_CAP_SECS);
                warn!(
                    "agent_pusher: agent_id={} repo={} branch={} push failed \
                     (failure #{}, backing off until epoch {}): {e:#}",
                    state.agent_id,
                    target.repo,
                    target.branch,
                    b.consecutive_failures,
                    b.next_attempt_epoch_secs
                );
            }
        }
    }
    Ok(())
}

/// Push one branch to coord-origin via `git push`. Returns
/// [`PushOutcome::Pushed`] if anything moved, [`PushOutcome::UpToDate`]
/// for a no-op, [`PushOutcome::Transient`] for a retryable failure,
/// [`PushOutcome::TimedOut`] when the child exceeded `push_timeout`
/// (killed via `kill_on_drop`), and `Err` for a permanent one (see
/// [`is_transient_push_error`]).
///
/// `push_timeout` is injected by the caller (see [`push_timeout()`])
/// so tests never need to mutate global env.
async fn push_one(
    state: &Arc<PusherState>,
    target: &PushTarget,
    token: &str,
    push_timeout: Duration,
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
    //
    // Bounds (2026-07-12 incident hardening):
    // - http.lowSpeedLimit/Time abort a transfer trickling below
    //   1 KiB/s for 60s (a stalled server otherwise hangs git forever);
    // - the tokio timeout is the hard wall-clock backstop; on expiry
    //   the output-future is dropped and `kill_on_drop(true)` reaps the
    //   git child instead of leaking it.
    //
    // `kill_on_drop` reaps the DIRECT child only, which is not enough:
    // `git push` spawns `git-remote-https` to carry the transfer, and that
    // grandchild survives its parent's death still holding the TLS
    // connection and its committed memory. Under a push storm those
    // orphans accumulate for the runner's whole lifetime — the global
    // job object only reaps them when the runner itself exits. On
    // 2026-08-19 that leak walked this machine into the Windows commit
    // limit (STATUS_COMMITMENT_LIMIT, 0xC000012D) and the kernel killed
    // the WebView2 browser process, blanking the runner UI.
    //
    // So the child also goes into a scoped kill-on-close job: dropping it
    // (timeout, error, or normal return) terminates the whole tree.
    // Nested jobs make this compose with the global one (Win8+).
    //
    // The argument vector (including those bounds and the prompt-proofing
    // overrides) is built by the pure [`push_args`]; `GIT_TERMINAL_PROMPT`
    // must be applied here on the command itself.
    let started = std::time::Instant::now();
    let child = crate::process_helpers::tokio_no_window("git")
        .args(push_args(
            &target.worktree_path,
            token,
            &origin_url,
            &refspec,
        ))
        .env(GIT_TERMINAL_PROMPT_ENV.0, GIT_TERMINAL_PROMPT_ENV.1)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning git push for {}", target.branch))?;

    // Held until this function returns; drop closes the job and reaps the
    // tree. Assignment is best-effort — on failure we still have
    // `kill_on_drop` plus the global job, i.e. exactly today's behaviour.
    //
    // Residual race: git could spawn its transport helper between `spawn`
    // and `assign`. In practice git parses config and resolves the remote
    // first, so the window is microseconds; closing it fully needs
    // CREATE_SUSPENDED, which `tokio::process` does not expose.
    #[cfg(windows)]
    let _tree_job = {
        let job = crate::job_object::ScopedKillOnCloseJob::create(None);
        match (job.as_ref(), child.raw_handle()) {
            (Some(j), Some(handle)) => j.assign(handle as _),
            (None, _) => debug!(
                "agent_pusher: scoped push job unavailable — \
                 falling back to kill_on_drop + the global job"
            ),
            (_, None) => debug!("agent_pusher: git child handle unavailable — no scoped job"),
        }
        job
    };

    let out = match tokio::time::timeout(push_timeout, child.wait_with_output()).await {
        Ok(res) => res.with_context(|| format!("invoking git push for {}", target.branch))?,
        Err(_) => {
            let elapsed = started.elapsed();
            warn!(
                "agent_pusher: git push timed out after {:.1}s (limit {}s) — \
                 killed child; worktree={} refspec={}",
                elapsed.as_secs_f64(),
                push_timeout.as_secs(),
                target.worktree_path.display(),
                refspec
            );
            return Ok(PushOutcome::TimedOut(elapsed));
        }
    };
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

/// Env var forced on every pusher `git push` so git never prompts on a
/// terminal. Paired with the empty `credential.helper=` / `core.askPass=`
/// overrides in [`push_args`] — see the comment there.
const GIT_TERMINAL_PROMPT_ENV: (&str, &str) = ("GIT_TERMINAL_PROMPT", "0");

/// Argument vector for the `git push` invocation in [`push_one`]. Pure
/// (no I/O) so tests can assert the exact command shape without spawning
/// git. Callers must ALSO set [`GIT_TERMINAL_PROMPT_ENV`] on the command.
///
/// Prompt-proofing: the Bearer `http.extraHeader` is the SOLE intended
/// auth for this push. Without the overrides below, a 401 (expired /
/// invalid token) makes git walk the credential chain (GCM → configured
/// helpers → askpass → terminal), which can pop an interactive password
/// prompt from this background daemon. `-c credential.helper=` (empty
/// value) disables the entire helper chain for this process, `-c
/// core.askPass=` (empty) kills the askpass fallback, and
/// `GIT_TERMINAL_PROMPT=0` forbids terminal prompting — so a 401 fails
/// the push (retried next tick after the token refresh) instead of
/// prompting.
///
/// Transfer bounds (2026-07-12 incident hardening): `http.lowSpeedLimit`
/// / `http.lowSpeedTime` abort a transfer trickling below 1 KiB/s for
/// 60s. The hard wall-clock backstop is the `tokio::time::timeout` in
/// [`push_one`], not an argument.
fn push_args(
    worktree_path: &std::path::Path,
    token: &str,
    origin_url: &str,
    refspec: &str,
) -> Vec<std::ffi::OsString> {
    vec![
        "-C".into(),
        worktree_path.into(),
        "-c".into(),
        format!("http.extraHeader=Authorization: Bearer {token}").into(),
        "-c".into(),
        "credential.helper=".into(),
        "-c".into(),
        "core.askPass=".into(),
        "-c".into(),
        "http.lowSpeedLimit=1024".into(),
        "-c".into(),
        "http.lowSpeedTime=60".into(),
        "push".into(),
        "--porcelain".into(),
        "--no-verify".into(),
        origin_url.into(),
        refspec.into(),
    ]
}

/// The coord git-origin URL for `repo`: `<base>/git/<owner>/<name>.git`.
///
/// Owner-qualified per coord's registry-driven git origin (cutover a+b+c,
/// coord `d535ee3`): routes are `/git/:owner/:repo/...` and the old
/// single-segment `/git/<name>.git` routes are GONE with no compat — a
/// basename-only URL misses the route entirely.
///
/// **Auth is NOT injected here.** coord's git-http gate is Bearer-only and
/// rejects GitHub-style `x-access-token:<jwt>@host` basic-auth in the URL
/// (qontinui-coord `src/git_replication.rs`: "must NOT use GitHub-style
/// Basic auth — coord's git-http gate rejects it (401)"). The agent JWT is
/// instead handed to git as an `Authorization: Bearer` header via
/// `-c http.extraHeader` in [`push_one`], so the URL stays credential-free.
///
/// `repo` may be canonical (`qontinui/qontinui-coord[.git]`) or a legacy
/// bare slug (`qontinui-coord[.git]`). Bare slugs map under the same
/// default owner coord applies server-side to legacy scope entries and
/// persisted rows (`git_origin::split_repo_slug` / `default_repo_owner`),
/// so both ends of the wire agree on where a bare slug lives.
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
    let (owner, name) = match repo.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() => {
            (owner.to_string(), name.to_string())
        }
        _ => (default_repo_owner(), repo.to_string()),
    };
    Ok(format!(
        "{}{}/git/{}/{}.git",
        prefix.0, prefix.1, owner, name
    ))
}

/// The owner legacy bare repo slugs map under — mirrors coord's
/// `git_origin::default_repo_owner` (same env var, same default) so the
/// client and server resolve a bare `<name>.git` to the same repo.
fn default_repo_owner() -> String {
    std::env::var("GITHUB_REPO_OWNER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "qontinui".to_string())
}

/// Outcome of one [`push_one`] attempt. `Transient` carries the git
/// stderr of a best-effort-retryable failure (coord write-proxy /
/// replication hiccups, network faults) — logged quietly at first,
/// promoted to `warn!` at [`NOISY_FAILURE_THRESHOLD`] consecutive
/// failures, and retried with per-target exponential backoff.
/// Auth 401s are transient too — the per-tick token refresh in
/// [`tick_once`] heals an expired token before the next attempt; a
/// genuinely revoked agent therefore rides the same backoff ladder
/// (warned from the third consecutive failure) instead of retrying
/// forever. Permanent failures (scope / allowlist 4xx, or anything
/// unrecognized) come back as `Err` and are surfaced loudly.
enum PushOutcome {
    Pushed,
    UpToDate,
    Transient(String),
    /// The `git push` child exceeded the hard timeout and was killed
    /// (`kill_on_drop`). Carries elapsed time; counts as a failure for
    /// backoff purposes and is always logged at `warn!`.
    TimedOut(Duration),
}

impl PushOutcome {
    /// True for outcomes that count as a failure toward the per-target
    /// backoff ladder (successes and up-to-date no-ops reset it).
    /// `tick_once` matches the variants directly for per-arm log
    /// wording; this is the canonical mapping the unit tests pin down.
    #[cfg_attr(not(test), allow(dead_code))]
    fn is_failure(&self) -> bool {
        matches!(self, PushOutcome::Transient(_) | PushOutcome::TimedOut(_))
    }
}

/// Classify a `git push` failure as transient (worth a quiet retry) vs
/// permanent. Transient = coord's git write-proxy / replication being
/// momentarily unavailable (5xx, "leader unreachable"), ordinary network
/// faults (DNS / connection / TLS / timeout), and auth 401s (see below);
/// these self-heal, so the best-effort pusher retries next tick without a
/// warn. Everything else — scope/allowlist (400/403/404) and unrecognized
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
        // Auth 401 is transient HERE (not in general): `tick_once`
        // refreshes the agent token via `agent_token::maybe_refresh`
        // every tick, so an expired-token 401 genuinely self-heals on
        // the next tick. A persistently-revoked agent still surfaces
        // loudly — `maybe_refresh` itself errors and `tick_once`
        // propagates that to the run loop's `warn!`.
        "error: 401",
        "authentication failed",
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
            ..Default::default()
        };
        assert!(!t.needs_refresh(now));
        // 10 min left → refresh.
        let t2 = TokenSlot {
            token: "x".into(),
            jti: uuid::Uuid::nil(),
            exp: now + 600,
            ..Default::default()
        };
        assert!(t2.needs_refresh(now));
        // 30 min - 1s left → refresh (just under the margin).
        let t3 = TokenSlot {
            token: "x".into(),
            jti: uuid::Uuid::nil(),
            exp: now + TOKEN_REFRESH_MARGIN_SECS - 1,
            ..Default::default()
        };
        assert!(t3.needs_refresh(now));
    }

    #[test]
    fn build_origin_url_is_credential_free() {
        // coord's git gate is Bearer-only; the URL must carry NO basic-auth
        // (the JWT goes in an http.extraHeader instead).
        let url = build_origin_url("https://coord.example/", "qontinui-coord").unwrap();
        assert_eq!(url, "https://coord.example/git/qontinui/qontinui-coord.git");
        assert!(
            !url.contains("x-access-token"),
            "url must not embed creds: {url}"
        );
        assert!(!url.contains('@'), "url must not embed creds: {url}");
    }

    #[test]
    fn build_origin_url_is_owner_qualified() {
        // canonical owner/name passes through owner-qualified — coord's
        // routes are `/git/:owner/:repo/...` (the single-segment routes
        // are gone, no compat).
        let url = build_origin_url("http://h:9870", "qontinui/qontinui-coord.git").unwrap();
        assert_eq!(url, "http://h:9870/git/qontinui/qontinui-coord.git");
        // a non-default owner is preserved verbatim
        let url = build_origin_url("http://h:9870", "acme/widget").unwrap();
        assert_eq!(url, "http://h:9870/git/acme/widget.git");
    }

    #[test]
    fn build_origin_url_maps_bare_slug_under_default_owner() {
        // legacy bare slug → default owner, mirroring coord's
        // `split_repo_slug`/`default_repo_owner` mapping so client and
        // server resolve the same repo.
        let url = build_origin_url("http://h:9870", "qontinui-coord.git").unwrap();
        assert_eq!(url, "http://h:9870/git/qontinui/qontinui-coord.git");
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
        // scope / allowlist + unknown → permanent (surfaced loudly).
        assert!(!is_transient_push_error(
            "error: 403 Forbidden: no git_push scope on token"
        ));
        assert!(!is_transient_push_error(
            "remote: repo not in allowlist\n... error: 400"
        ));
    }

    #[test]
    fn push_401_is_transient_because_token_refreshes_per_tick() {
        // 401 / auth-failed stderr is transient HERE: tick_once refreshes
        // the agent token every tick, so an expired-token 401 self-heals
        // on the next tick (a revoked agent still warns loudly via the
        // maybe_refresh error path). Realistic git stderr shapes:
        assert!(is_transient_push_error(
            "fatal: Authentication failed for 'https://coord.qontinui.io/git/qontinui/x.git/'"
        ));
        assert!(is_transient_push_error(
            "fatal: unable to access 'https://coord.qontinui.io/git/qontinui/x.git/': \
             The requested URL returned error: 401"
        ));
        // ...while a genuinely-permanent failure stays permanent.
        assert!(!is_transient_push_error("fatal: repository not found"));
    }

    #[test]
    fn push_args_are_prompt_proof() {
        // The Bearer extraHeader is the SOLE intended auth: the helper
        // chain, askpass, and terminal prompting must all be disabled so
        // a 401 fails the push instead of popping an interactive prompt.
        let args = push_args(
            std::path::Path::new("/tmp/wt"),
            "tok123",
            "https://coord.example/git/qontinui/x.git",
            "refs/heads/b:refs/agent/m-a",
        );
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let pos = |needle: &str| {
            args.iter()
                .position(|a| a == needle)
                .unwrap_or_else(|| panic!("missing arg {needle:?} in {args:?}"))
        };
        let push_pos = pos("push");
        // (1) empty credential.helper override, before the subcommand,
        // preceded by `-c` so it's a config override not a stray arg.
        let cred = pos("credential.helper=");
        assert!(cred < push_pos, "credential.helper= must precede 'push'");
        assert_eq!(args[cred - 1], "-c");
        // (2) empty core.askPass override, likewise.
        let askpass = pos("core.askPass=");
        assert!(askpass < push_pos, "core.askPass= must precede 'push'");
        assert_eq!(args[askpass - 1], "-c");
        // (3) the terminal-prompt kill switch lives in the env pair the
        // caller applies alongside these args.
        assert_eq!(GIT_TERMINAL_PROMPT_ENV, ("GIT_TERMINAL_PROMPT", "0"));
        // (4) the 2026-07-12 transfer bounds must survive the move into
        // push_args: a stalled server otherwise hangs git indefinitely.
        for bound in ["http.lowSpeedLimit=1024", "http.lowSpeedTime=60"] {
            let at = pos(bound);
            assert!(at < push_pos, "{bound} must precede 'push'");
            assert_eq!(args[at - 1], "-c");
        }
        // Sanity: Bearer header config + push targets still present.
        assert!(args
            .iter()
            .any(|a| a == "http.extraHeader=Authorization: Bearer tok123"));
        assert_eq!(
            &args[args.len() - 2..],
            &[
                "https://coord.example/git/qontinui/x.git".to_string(),
                "refs/heads/b:refs/agent/m-a".to_string()
            ]
        );
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

    // ---- backoff state machine (2026-07-12 incident hardening) ----
    // All params are injected as arguments — no env mutation (the
    // parallel test harness races `std::env::set_var`).

    #[test]
    fn backoff_delay_doubles_from_base_and_caps() {
        // 5m base, 1h cap: 5m→10m→20m→40m→60m, then 60m forever.
        assert_eq!(backoff_delay_secs(300, 0, 3600), 0);
        assert_eq!(backoff_delay_secs(300, 1, 3600), 300);
        assert_eq!(backoff_delay_secs(300, 2, 3600), 600);
        assert_eq!(backoff_delay_secs(300, 3, 3600), 1200);
        assert_eq!(backoff_delay_secs(300, 4, 3600), 2400);
        assert_eq!(backoff_delay_secs(300, 5, 3600), 3600);
        assert_eq!(backoff_delay_secs(300, 6, 3600), 3600);
        assert_eq!(backoff_delay_secs(300, 100, 3600), 3600);
        // base already over the cap → clamp to cap immediately.
        assert_eq!(backoff_delay_secs(7200, 1, 3600), 3600);
        // no overflow panic on absurd counts.
        assert_eq!(backoff_delay_secs(u64::MAX, 64, 3600), 3600);
    }

    #[test]
    fn backoff_failure_ladder_then_reset_on_success() {
        let mut b = BackoffState::default();
        let (base, cap) = (300u64, 3600u64);
        // Fresh state: never skips.
        assert!(!b.should_skip(1000));
        // failure 1 → next attempt = now + base.
        b.record_failure(1000, base, cap);
        assert_eq!(b.consecutive_failures, 1);
        assert_eq!(b.next_attempt_epoch_secs, 1300);
        assert!(b.should_skip(1299));
        assert!(!b.should_skip(1300));
        // failure 2 → doubled.
        b.record_failure(1300, base, cap);
        assert_eq!(b.consecutive_failures, 2);
        assert_eq!(b.next_attempt_epoch_secs, 1300 + 600);
        // walk to the cap.
        b.record_failure(2000, base, cap); // #3 → 1200
        b.record_failure(3000, base, cap); // #4 → 2400
        b.record_failure(4000, base, cap); // #5 → 3600 (cap)
        assert_eq!(b.next_attempt_epoch_secs, 4000 + 3600);
        b.record_failure(9000, base, cap); // #6 → still capped
        assert_eq!(b.consecutive_failures, 6);
        assert_eq!(b.next_attempt_epoch_secs, 9000 + 3600);
        // success fully resets the ladder.
        b.record_success();
        assert_eq!(b, BackoffState::default());
        assert!(!b.should_skip(9001));
        // and the next failure starts back at the base delay.
        b.record_failure(9001, base, cap);
        assert_eq!(b.next_attempt_epoch_secs, 9001 + 300);
    }

    #[test]
    fn record_outcome_goes_loud_at_third_consecutive_failure() {
        // The incident produced 3,335 silent refusals — the third
        // consecutive failure must be loudly visible (warn!).
        let mut b = BackoffState::default();
        assert!(!record_outcome(&mut b, true, 100, 300, 3600)); // #1 quiet
        assert!(!record_outcome(&mut b, true, 500, 300, 3600)); // #2 quiet
        assert!(record_outcome(&mut b, true, 1200, 300, 3600)); // #3 LOUD
        assert!(record_outcome(&mut b, true, 2500, 300, 3600)); // #4 stays loud
                                                                // success resets both the ladder and the loudness.
        assert!(!record_outcome(&mut b, false, 3000, 300, 3600));
        assert_eq!(b, BackoffState::default());
        assert!(!record_outcome(&mut b, true, 3100, 300, 3600)); // quiet again
    }

    #[test]
    fn timeout_counts_as_failure_for_backoff() {
        // A timed-out push (child killed via kill_on_drop) must advance
        // the backoff ladder exactly like a transient refusal.
        let timed_out = PushOutcome::TimedOut(Duration::from_secs(120));
        assert!(timed_out.is_failure());
        assert!(PushOutcome::Transient("error: 503".into()).is_failure());
        assert!(!PushOutcome::Pushed.is_failure());
        assert!(!PushOutcome::UpToDate.is_failure());

        let mut b = BackoffState::default();
        record_outcome(&mut b, timed_out.is_failure(), 1000, 300, 3600);
        assert_eq!(b.consecutive_failures, 1);
        assert_eq!(b.next_attempt_epoch_secs, 1300);
        assert!(b.should_skip(1100));
    }

    #[tokio::test]
    async fn state_constructor_sizes_backoff_parallel_to_targets() {
        // with_shared_token builds one BackoffState per push target.
        let allocate = crate::agent_worktree::AllocateResult {
            agent_id: uuid::Uuid::nil().to_string(),
            worktrees: vec![],
            token: "t".into(),
            token_jti: uuid::Uuid::nil(),
            token_exp: 0,
            active_claims: vec![],
        };
        let token: SharedToken = Arc::new(tokio::sync::RwLock::new(TokenSlot {
            token: "t".into(),
            jti: uuid::Uuid::nil(),
            exp: 0,
            ..Default::default()
        }));
        let state = PusherState::with_shared_token(&allocate, "http://h:1".into(), token).unwrap();
        assert_eq!(state.targets.len(), state.backoff.lock().await.len());
    }

    /// Cancelling must ACTUALLY stop the task, not merely avoid panicking.
    ///
    /// The previous version of this test dropped the handle immediately
    /// and asserted nothing. It could not fail: the task was still parked
    /// in `select!`, i.e. registered as a `Notify` waiter, which is the one
    /// state in which the old `notify_waiters()` did work. The bug lived
    /// entirely in the other state — mid-tick — so the test that was
    /// supposed to cover this path was structurally incapable of seeing it.
    ///
    /// This version asserts on `JoinHandle::is_finished()` instead of on
    /// the absence of a panic, which is the only thing that distinguishes
    /// a stopped task from a detached one.
    ///
    /// **Named for what it covers.** It drives `run` against a token it
    /// owns; it does NOT construct a `PusherHandle` and so does not
    /// exercise `Drop`, nor the `abort()` backstop behind it. Calling it
    /// `..._drop_actually_stops_the_task` would repeat, in the very change
    /// that removes it, the false-coverage claim this work exists to fix.
    /// End-to-end `spawn → Drop → no further requests` is covered once, in
    /// `dirty_poller::tests::no_requests_are_issued_after_the_handle_is_dropped`;
    /// the pusher's tick is a `git push`, which has no equivalently cheap
    /// observable, so it is left to that shared coverage plus this test.
    #[tokio::test]
    async fn cancelling_stops_the_pusher_task() {
        let state = Arc::new(PusherState {
            agent_id: uuid::Uuid::nil(),
            coord_http_base: "http://invalid:1".into(),
            origin_repo_alias: "qontinui-coord".into(),
            targets: vec![],
            token: Arc::new(tokio::sync::RwLock::new(TokenSlot {
                token: "x".into(),
                jti: uuid::Uuid::nil(),
                exp: chrono::Utc::now().timestamp() + 4 * 3600,
                ..Default::default()
            })),
            backoff: tokio::sync::Mutex::new(Vec::new()),
        });
        // Keep our own handle on the task so we can observe it after the
        // PusherHandle is gone.
        let cancel = CancellationToken::new();
        let join = tokio::spawn(run(state, 3600, 0, cancel.clone()));
        tokio::task::yield_now().await;
        assert!(!join.is_finished(), "task should be running before cancel");

        cancel.cancel();
        for _ in 0..100 {
            if join.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            join.is_finished(),
            "cancelling must stop the pusher task; a detached task here is the \
             leak that put 1,353 orphaned daemons on one box"
        );
    }
}
