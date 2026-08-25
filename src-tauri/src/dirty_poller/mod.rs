//! Per-worktree live-state poller (Coordination Phase 6, runner side).
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-14-branch-per-agent-coordination-plan.md`
//! §4.8 ("Live-state observability").
//!
//! ## What this does
//!
//! For every allocated agent on this machine, an in-process Tokio task
//! wakes every `~5 s` (no jitter — a steady cadence keeps the
//! dashboard heatmap smooth; the ref-update-storm concern that drives
//! `agent_pusher`'s jitter doesn't apply, a dirty-state POST is one
//! small request, ~60 req/s fleet-wide at 300 agents, which coord
//! handles comfortably per bottleneck-tracker Row 6) and runs, per
//! worktree:
//!
//! ```text
//! git -C <worktree_path> status --porcelain
//! git -C <worktree_path> diff --shortstat HEAD
//! ```
//!
//! then POSTs the parsed result to coord's
//! `POST /agents/:agent_id/dirty-state` (Row 9 Phase 2 JWT auth, same
//! token lifecycle as `agent_pusher`).
//!
//! ## Why it exists
//!
//! It replaces `project.session_touched_files` as the heatmap's data
//! source. That signal is fed by Edit/Write tool intercepts and is
//! structurally lossy (memory
//! `proj_arch_coord_session_touched_files_signal`): reads are
//! invisible (a no-op agent looks hung), deletes/renames don't count,
//! in-flight stash is invisible. `git status` reports the *real*
//! working-tree delta — deletes, renames, untracked files included;
//! reads are absent by construction. Phase 6 is the **pivot**: the
//! old `auto_register_file` → `session_touched_files` path keeps
//! working until Phase 7's deletion sweep.
//!
//! ## Change vs. heartbeat
//!
//! Each tick fingerprints every worktree's `(status, shortstat)`. If
//! nothing changed since the prior tick, the poller sends a compact
//! `heartbeat: true` POST with no worktree payload — coord just
//! refreshes the Redis TTL and does **not** re-fan-out, so a fleet of
//! idle agents doesn't flood every dashboard. On any change it sends
//! the full per-worktree state, which coord caches and publishes on
//! `events.worktree.dirty.<agent_id>`.
//!
//! ## JWT lifecycle
//!
//! Identical to `agent_pusher`: a coord-issued token, refreshed
//! proactively via `POST /agents/:id/refresh-token` when within
//! `TOKEN_REFRESH_MARGIN_SECS` of expiry. The poller holds its own
//! token slot independent of the pusher's — both are valid agent
//! tokens; coord's refresh endpoint is idempotent per call. Keeping
//! them independent decouples the two daemons' lifecycles (the pusher
//! spawn site is not yet wired; Phase 6 must not depend on it).
//!
//! ## Process model
//!
//! In-process Tokio task, dies with the runner, shares its tracing
//! pipeline — same rationale as `agent_pusher`. A missed poll is
//! self-healing: the next tick re-reads `git status` from scratch
//! (the signal is level-triggered, not edge-triggered).

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::agent_token::{self, SharedToken};
// Re-exported so existing `dirty_poller::TokenSlot` references (incl.
// this module's tests) keep resolving after the token logic moved to
// `crate::agent_token` (shared with `agent_pusher`).
pub use crate::agent_token::TokenSlot;

/// Default cadence — 5 s per §4.8.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

/// One worktree the poller watches.
#[derive(Debug, Clone)]
pub struct DirtyTarget {
    pub repo: String,
    pub branch: String,
    pub worktree_path: PathBuf,
}

/// State shared between the poller task and its handle.
#[derive(Debug)]
pub struct DirtyPollerState {
    pub agent_id: uuid::Uuid,
    pub machine_id: uuid::Uuid,
    pub coord_http_base: String,
    pub targets: Vec<DirtyTarget>,
    /// Shared with every other daemon spawned for this agent (one
    /// refresh path, not one per daemon). See [`crate::agent_token`].
    pub token: SharedToken,
    /// Per-repo fingerprint of the last *sent* state. `tick_once`
    /// compares against this to decide change-vs-heartbeat and to
    /// drive the monotonic `seq`.
    seen: RwLock<SeenState>,
}

#[derive(Debug, Default)]
struct SeenState {
    /// repo → fingerprint of the last successfully-built payload.
    fingerprints: HashMap<String, u64>,
    /// Monotonic per-agent publish counter (gap-detection on the
    /// dashboard side).
    seq: u64,
}

impl DirtyPollerState {
    /// Build from an [`crate::agent_worktree::AllocateResult`] — same
    /// entry point shape as `agent_pusher::PusherState::
    /// from_allocate_result`. Returns `None` when the allocation
    /// carried no token (coord JWT keys unconfigured, dev fallback):
    /// the poller's POST is JWT-gated, so without a token there's
    /// nothing to do — caller logs + continues.
    pub fn from_allocate_result(
        allocate: &crate::agent_worktree::AllocateResult,
        coord_http_base: String,
        machine_id: uuid::Uuid,
    ) -> Option<Self> {
        let token = agent_token::from_allocate_result(allocate)?;
        Self::with_shared_token(allocate, coord_http_base, machine_id, token)
    }

    /// Same as [`from_allocate_result`] but the caller supplies the
    /// shared token slot — used by `agent_daemons::spawn_for_agent`
    /// so the poller and the pusher refresh through one slot.
    pub fn with_shared_token(
        allocate: &crate::agent_worktree::AllocateResult,
        coord_http_base: String,
        machine_id: uuid::Uuid,
        token: SharedToken,
    ) -> Option<Self> {
        let agent_id = uuid::Uuid::from_str(&allocate.agent_id).ok()?;
        let targets: Vec<DirtyTarget> = allocate
            .worktrees
            .iter()
            .map(|w| DirtyTarget {
                repo: w.repo.clone(),
                branch: w.branch.clone(),
                worktree_path: w.worktree_path.clone(),
            })
            .collect();
        if targets.is_empty() {
            return None;
        }
        Some(Self {
            agent_id,
            machine_id,
            coord_http_base,
            targets,
            token,
            seen: RwLock::new(SeenState::default()),
        })
    }
}

/// Spawn the poller for one agent. Mirrors `agent_pusher::spawn` —
/// returns a handle whose `Drop` stops the poller immediately.
pub fn spawn(state: Arc<DirtyPollerState>) -> DirtyPollerHandle {
    let interval_secs = std::env::var("QONTINUI_DIRTY_POLL_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n: &u64| *n > 0)
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();
    let state_for_task = state.clone();
    let join = tokio::spawn(async move {
        run(state_for_task, interval_secs, cancel_for_task).await;
    });
    DirtyPollerHandle {
        cancel,
        join: Some(join),
        state,
    }
}

/// Returned from [`spawn`]. Drop = stop the poller, now.
pub struct DirtyPollerHandle {
    cancel: CancellationToken,
    join: Option<tokio::task::JoinHandle<()>>,
    pub state: Arc<DirtyPollerState>,
}

impl Drop for DirtyPollerHandle {
    /// Cancel, then abort. Both halves are load-bearing.
    ///
    /// This used to be `Notify::notify_waiters()` + `let _ = join.take()`,
    /// and it leaked a task per agent. `notify_waiters` stores NO permit —
    /// it wakes only tasks already parked as waiters at that instant. The
    /// poller is a waiter only while sitting in [`run`]'s `select!`; while
    /// it is inside `tick_once` (two `git` subprocesses plus a coord POST
    /// with a 30 s ceiling) it is not, so the signal was discarded with no
    /// second chance. Dropping the `JoinHandle` then DETACHES the task
    /// rather than aborting it, so nothing caught the miss.
    ///
    /// Measured consequence on one workstation, 2026-08-13: all 586 agents
    /// that logged `pusher+poller stopped` kept ticking afterwards — one of
    /// them still POSTing 23 hours after teardown — with 1,353 distinct
    /// agent ids ticking against a registry high-water mark of 28. Roughly
    /// 20M requests/day at coord from orphans reporting on worktrees that
    /// no longer existed. Self-reinforcing, too: more orphans meant more
    /// timeouts, which meant more time inside `tick_once`, which widened
    /// the very window where the signal got lost.
    ///
    /// [`CancellationToken`] latches, so it is observed even when
    /// cancellation lands mid-request; `abort()` is the backstop behind it.
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

// The process-global handle registry moved to `crate::agent_daemons`,
// which now owns the single per-agent spawn site (pusher + poller).
// `dirty_poller::spawn` stays the standalone primitive (used by
// `agent_daemons` and the integration test).

/// Consecutive failures before one ERROR escalation is emitted. Same
/// threshold and shape as `agent_worktree::reclaim`'s poller-down
/// escalation (runner #930) — a streak, not a per-failure line.
const FAILURE_ESCALATE_AFTER: u32 = 5;

/// Ceiling for the failure backoff. A poller that cannot reach coord
/// drops from `interval_secs` towards this rather than hammering at full
/// cadence; a single success restores the normal interval immediately.
const FAILURE_BACKOFF_MAX_SECS: u64 = 300;

/// Backoff delay after `streak` consecutive failures: the normal interval
/// doubled per failure, capped. Returns `None` while the streak is 0, i.e.
/// "use the ordinary interval".
fn failure_backoff_secs(interval_secs: u64, streak: u32) -> Option<u64> {
    if streak == 0 {
        return None;
    }
    let shift = streak.min(16);
    let scaled = interval_secs.saturating_mul(1u64 << shift);
    Some(scaled.min(FAILURE_BACKOFF_MAX_SECS).max(interval_secs))
}

async fn run(state: Arc<DirtyPollerState>, interval_secs: u64, cancel: CancellationToken) {
    info!(
        "dirty_poller: started agent_id={} repos={} interval={}s",
        state.agent_id,
        state.targets.len(),
        interval_secs
    );
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Consecutive-failure streak. Drives both the log level and the
    // backoff, so a coord outage costs one ERROR and a slow trickle
    // instead of one WARN every `interval_secs` per agent.
    let mut failures: u32 = 0;
    loop {
        match failure_backoff_secs(interval_secs, failures) {
            // Healthy: the ordinary metronome.
            None => {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = cancel.cancelled() => {
                        debug!("dirty_poller: agent_id={} stopping", state.agent_id);
                        return;
                    }
                }
            }
            // Failing: sleep the backoff instead. `interval` is reset on
            // the way out so the first healthy tick is a full interval
            // away rather than firing instantly on missed-tick catch-up.
            Some(backoff) => {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(backoff)) => {
                        interval.reset();
                    }
                    _ = cancel.cancelled() => {
                        debug!("dirty_poller: agent_id={} stopping", state.agent_id);
                        return;
                    }
                }
            }
        }

        // Cancellation must win against the TICK ITSELF, not just the
        // wait. `tick_once` can block for ~30 s on a coord timeout, and
        // it is precisely that window in which teardown used to be lost.
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                debug!(
                    "dirty_poller: agent_id={} cancelled mid-tick",
                    state.agent_id
                );
                return;
            }
            res = tick_once(&state) => res,
        };

        match outcome {
            // Terminal. `RefreshHealth::rejected` is a latch whose only
            // exit is `adopt_fresh`, and `maybe_refresh` short-circuits
            // before ever attempting one — so this poller can never
            // recover and every further tick is pure load. A
            // re-allocation spawns a fresh poller with a fresh slot.
            Ok(TickOutcome::SkippedTokenRejected) => {
                info!(
                    "dirty_poller: agent_id={} stopping — coord rejected this agent's \
                     bearer, which is terminal for this token slot",
                    state.agent_id
                );
                return;
            }
            Ok(TickOutcome::Posted { .. }) => {
                if failures > 0 {
                    info!(
                        "dirty_poller: agent_id={} recovered after {failures} consecutive \
                         failures",
                        state.agent_id
                    );
                }
                failures = 0;
            }
            Err(e) => {
                failures = failures.saturating_add(1);
                // One WARN on the way in, one ERROR when the streak
                // crosses, DEBUG for the rest. The old code logged every
                // failure at WARN and produced 314,467 identical lines in
                // a single day — 99.99 % of the file, burying every other
                // signal in it.
                if failures == 1 {
                    warn!(
                        "dirty_poller: agent_id={} tick failed: {e:#}",
                        state.agent_id
                    );
                } else if failures == FAILURE_ESCALATE_AFTER {
                    error!(
                        "dirty_poller: DIRTY POLLER DOWN — agent_id={} {failures} consecutive \
                         failed posts; dirty state is not reaching coord while this persists: \
                         {e:#}",
                        state.agent_id
                    );
                } else {
                    debug!(
                        "dirty_poller: agent_id={} tick failed ({failures} consecutive): {e:#}",
                        state.agent_id
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types — must match `qontinui_coord::dirty_state` exactly.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DirtyFile {
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeDirty {
    pub repo: String,
    pub branch: String,
    pub files: Vec<DirtyFile>,
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirtyStateReq {
    pub machine_id: uuid::Uuid,
    pub worktrees: Vec<WorktreeDirty>,
    pub polled_at: chrono::DateTime<chrono::Utc>,
    pub heartbeat: bool,
    pub seq: u64,
}

/// What one poll cycle actually did.
///
/// A bare `bool` (the old return type, meaning `heartbeat`) cannot
/// express "nothing was sent" — the same conflation runner #930 removed
/// from `agent_worktree::reclaim::TickOutcome`. A skipped tick is
/// neither a heartbeat nor a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// Coord has rejected this agent's bearer, so nothing was collected
    /// or sent. Not a success and not a failure — posting would only add
    /// load, and `git status` per target would spawn subprocesses whose
    /// result could never be delivered.
    SkippedTokenRejected,
    /// State was posted. `heartbeat` is true iff nothing changed since
    /// the prior tick.
    Posted { heartbeat: bool },
}

/// One poll cycle. Public so an integration test can drive it without
/// the spawn machinery (same pattern as `agent_pusher::tick_once`).
pub async fn tick_once(state: &Arc<DirtyPollerState>) -> Result<TickOutcome> {
    let refresh = agent_token::maybe_refresh(
        &state.token,
        &state.coord_http_base,
        state.agent_id,
        "dirty_poller",
    )
    .await?;
    // A rejected token makes every POST below a guaranteed 401. On
    // 2026-08-07 this path sent 19,790 of them in 55 minutes, each on
    // its own socket, plus a `git status` per target per tick. Stop at
    // the top instead.
    if refresh.should_skip_work() {
        debug!(
            "dirty_poller: agent_id={} skipping tick — token rejected by coord",
            state.agent_id
        );
        return Ok(TickOutcome::SkippedTokenRejected);
    }

    let mut worktrees = Vec::with_capacity(state.targets.len());
    let mut new_fps: HashMap<String, u64> = HashMap::new();
    for t in &state.targets {
        if !t.worktree_path.exists() {
            // Worktree torn down out from under us — skip it; the
            // agent_worktrees sweeper owns lifecycle. Don't fail the
            // whole tick over one gone path.
            debug!(
                "dirty_poller: agent_id={} worktree gone, skipping repo={}",
                state.agent_id, t.repo
            );
            continue;
        }
        let wt = read_worktree_dirty(t).await?;
        new_fps.insert(t.repo.clone(), fingerprint(&wt));
        worktrees.push(wt);
    }

    // Decide change vs. heartbeat against the last *sent* fingerprints.
    //
    // READ-ONLY here. The commit moves to AFTER the post succeeds, below —
    // `fingerprints` is documented as the last *successfully sent* state,
    // and advancing it before the POST made that false. The old ordering
    // consumed the edge and then dropped it: tree changes, POST fails,
    // agent goes idle, and the next tick sees `changed == false` and sends
    // a heartbeat carrying no payload. Coord then never learns about the
    // change — not on the next tick, but ever, until the tree moves again.
    // That falsified this module's own "a missed poll is self-healing …
    // level-triggered, not edge-triggered" claim.
    //
    // The failure backoff makes it worse, which is why it is fixed in the
    // same change: coord's dirty cache TTL is 30 s, this backoff reaches
    // 300 s, and an expired key + a heartbeat with no payload makes coord
    // cache an EMPTY snapshot — the dashboard shows the agent *clean*
    // while it has dozens of dirty files. A false clean is worse than
    // dropping off the heatmap, which is what the TTL was designed to do.
    let (changed, seq) = {
        let seen = state.seen.read().await;
        let changed = new_fps.len() != seen.fingerprints.len()
            || new_fps
                .iter()
                .any(|(k, v)| seen.fingerprints.get(k) != Some(v));
        // The seq a CHANGE would carry; unchanged ticks reuse the last one.
        let seq = if changed { seen.seq + 1 } else { seen.seq };
        (changed, seq)
    };

    let req = if changed {
        DirtyStateReq {
            machine_id: state.machine_id,
            worktrees,
            polled_at: chrono::Utc::now(),
            heartbeat: false,
            seq,
        }
    } else {
        DirtyStateReq {
            machine_id: state.machine_id,
            worktrees: Vec::new(),
            polled_at: chrono::Utc::now(),
            heartbeat: true,
            seq,
        }
    };

    post_dirty_state(state, &req).await?;
    Ok(TickOutcome::Posted {
        heartbeat: !changed,
    })
}

/// `git status --porcelain` + `git diff --shortstat HEAD` for one
/// worktree.
async fn read_worktree_dirty(t: &DirtyTarget) -> Result<WorktreeDirty> {
    let status_out = crate::process_helpers::tokio_no_window("git")
        .arg("-C")
        .arg(&t.worktree_path)
        .arg("status")
        .arg("--porcelain")
        .output()
        .await
        .with_context(|| format!("git status in {}", t.worktree_path.display()))?;
    if !status_out.status.success() {
        anyhow::bail!(
            "git status exited {:?}: {}",
            status_out.status.code(),
            String::from_utf8_lossy(&status_out.stderr).trim()
        );
    }
    let files = parse_porcelain(&String::from_utf8_lossy(&status_out.stdout));

    let shortstat_out = crate::process_helpers::tokio_no_window("git")
        .arg("-C")
        .arg(&t.worktree_path)
        .arg("diff")
        .arg("--shortstat")
        .arg("HEAD")
        .output()
        .await
        .with_context(|| format!("git diff --shortstat in {}", t.worktree_path.display()))?;
    // shortstat can legitimately exit non-zero on an unborn HEAD; treat
    // any failure as "no tracked-diff stats" rather than erroring out.
    let (files_changed, insertions, deletions) = if shortstat_out.status.success() {
        parse_shortstat(&String::from_utf8_lossy(&shortstat_out.stdout))
    } else {
        (0, 0, 0)
    };

    Ok(WorktreeDirty {
        repo: t.repo.clone(),
        branch: t.branch.clone(),
        files,
        files_changed,
        insertions,
        deletions,
    })
}

async fn post_dirty_state(state: &Arc<DirtyPollerState>, req: &DirtyStateReq) -> Result<()> {
    let token = { state.token.read().await.token.clone() };
    let url = format!(
        "{}/agents/{}/dirty-state",
        state.coord_http_base.trim_end_matches('/'),
        state.agent_id
    );
    // Shared pooled client — never `Client::new()` here. A per-request
    // client reuses no connection, and this is a per-tick path across
    // every agent (see `crate::agent_http` for the 2026-08-07 outage).
    // coord-auth-exempt(agent-jwt): presents the per-agent coord JWT this poller
    // was handed; the route is scoped to that agent, not to the device.
    let resp = crate::agent_http::client()
        .post(&url)
        .bearer_auth(&token)
        .json(req)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // Deliberately NO `record_rejected()` latch here, and the reason
        // is worth keeping: an earlier cut of this change latched on
        // 401-or-403 from this endpoint, and it was badly wrong.
        //
        // On `POST /agents/:id/dirty-state`, coord's 403 does NOT mean
        // "bearer refused". Both of its 403 sites fire on a fully valid,
        // signed, unexpired token: `agent_id` claim absent (which is every
        // DEVICE token) or claim/path mismatch. And `state.token` is a
        // SHARED slot — `agent_daemons` hands the same `Arc` to the pusher
        // and `agent_runtime` registers it for the MCP proxy. Since
        // `maybe_refresh` short-circuits on the latch, and `adopt_fresh`
        // (its only clear) sits behind that short-circuit, one stray 403
        // would have frozen the agent's ONLY credential: no proactive
        // refresh, no pusher, no MCP proxy, and coord refuses to refresh an
        // already-expired token — so the agent dies within hours,
        // unrecoverable short of a respawn. A transient identity mismatch
        // must not become permanent credential loss.
        //
        // The flood that latch was meant to stop (1,546 unthrottled 401
        // lines across 74 agents in one day) is already handled correctly
        // one level up: repeated failures are a streak, and `run` backs the
        // interval off and stops re-logging. Bearer validity stays the
        // business of `agent_token::maybe_refresh`, which owns the refresh
        // endpoint's verdict — one authority, not two.
        anyhow::bail!("dirty-state POST returned {status}: {}", body.trim());
    }
    debug!(
        "dirty_poller: agent_id={} posted (heartbeat={} seq={} worktrees={})",
        state.agent_id,
        req.heartbeat,
        req.seq,
        req.worktrees.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Parsing (pure — unit-tested; rules must match
// `qontinui_coord::dirty_state::parse_porcelain`).
// ---------------------------------------------------------------------------

/// Parse `git status --porcelain` v1 into `DirtyFile`s. Rename/copy
/// lines (`R`/`C`) carry `orig -> new`; both ends matter to the
/// heatmap so the original path is preserved.
pub fn parse_porcelain(raw: &str) -> Vec<DirtyFile> {
    let mut out = Vec::new();
    for line in raw.lines() {
        if line.len() < 4 {
            continue;
        }
        let (code, rest) = line.split_at(2);
        let code = code.trim();
        let rest = rest.trim_start();
        if rest.is_empty() {
            continue;
        }
        if code.starts_with('R') || code.starts_with('C') {
            if let Some((orig, new)) = rest.split_once(" -> ") {
                out.push(DirtyFile {
                    path: new.trim().to_string(),
                    status: code.to_string(),
                    orig_path: Some(orig.trim().to_string()),
                });
                continue;
            }
        }
        out.push(DirtyFile {
            path: rest.to_string(),
            status: code.to_string(),
            orig_path: None,
        });
    }
    out
}

/// Parse `git diff --shortstat` → `(files_changed, insertions,
/// deletions)`. Example input:
/// ` 3 files changed, 12 insertions(+), 4 deletions(-)`.
pub fn parse_shortstat(raw: &str) -> (i64, i64, i64) {
    let line = raw.trim();
    if line.is_empty() {
        return (0, 0, 0);
    }
    let mut files = 0;
    let mut ins = 0;
    let mut del = 0;
    for part in line.split(',') {
        let p = part.trim();
        let n: i64 = p
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if p.contains("file") {
            files = n;
        } else if p.contains("insertion") {
            ins = n;
        } else if p.contains("deletion") {
            del = n;
        }
    }
    (files, ins, del)
}

/// Stable fingerprint of a worktree's dirty state. Order-independent
/// over files (git status order is stable but we don't want to depend
/// on it) so a pure reordering doesn't read as a change.
fn fingerprint(wt: &WorktreeDirty) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut entries: Vec<(&str, &str, &str)> = wt
        .files
        .iter()
        .map(|f| {
            (
                f.path.as_str(),
                f.status.as_str(),
                f.orig_path.as_deref().unwrap_or(""),
            )
        })
        .collect();
    entries.sort_unstable();
    let mut h = DefaultHasher::new();
    entries.hash(&mut h);
    wt.files_changed.hash(&mut h);
    wt.insertions.hash(&mut h);
    wt.deletions.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a poller state with one target that does not exist on disk,
    /// so `tick_once` never shells out to git. Enough to drive `run`.
    fn test_state(coord_http_base: &str) -> Arc<DirtyPollerState> {
        Arc::new(DirtyPollerState {
            agent_id: uuid::Uuid::nil(),
            machine_id: uuid::Uuid::nil(),
            coord_http_base: coord_http_base.to_string(),
            targets: vec![DirtyTarget {
                repo: "qontinui-coord".into(),
                branch: "main".into(),
                worktree_path: PathBuf::from("/nonexistent/qontinui-coord"),
            }],
            token: Arc::new(RwLock::new(TokenSlot {
                token: "x".into(),
                jti: uuid::Uuid::nil(),
                exp: chrono::Utc::now().timestamp() + 4 * 3600,
                ..Default::default()
            })),
            seen: RwLock::new(SeenState::default()),
        })
    }

    /// The regression test for the leak. Cancelling must actually end the
    /// task — a detached task that keeps ticking is the defect, and
    /// "did not panic" (what the old pusher test asserted) cannot see it.
    #[tokio::test]
    async fn cancelling_stops_the_poller_task() {
        let cancel = CancellationToken::new();
        // Long interval: the task is parked in the wait when we cancel.
        let join = tokio::spawn(run(test_state("http://127.0.0.1:1"), 3600, cancel.clone()));
        tokio::task::yield_now().await;
        assert!(!join.is_finished(), "task should be running before cancel");

        cancel.cancel();
        for _ in 0..100 {
            if join.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(join.is_finished(), "cancelling must stop the poller task");
    }

    /// A rejected bearer is terminal for the token slot: `maybe_refresh`
    /// short-circuits on the latch and never attempts a refresh, so the
    /// only exit is `adopt_fresh` — which that short-circuit makes
    /// unreachable. The poller must therefore stop rather than spin at
    /// full cadence doing nothing, which is what it used to do.
    #[tokio::test]
    async fn rejected_token_terminates_the_poller() {
        let state = test_state("http://127.0.0.1:1");
        assert!(state.token.write().await.record_rejected());

        // Short interval so the first tick lands promptly.
        let cancel = CancellationToken::new();
        let join = tokio::spawn(run(state, 1, cancel));
        for _ in 0..200 {
            if join.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            join.is_finished(),
            "a poller whose bearer coord has rejected must stop, not spin"
        );
    }

    #[test]
    fn failure_backoff_grows_then_caps_and_clears_on_success() {
        // Healthy: no backoff, use the ordinary interval.
        assert_eq!(failure_backoff_secs(5, 0), None);
        // Doubling per consecutive failure.
        assert_eq!(failure_backoff_secs(5, 1), Some(10));
        assert_eq!(failure_backoff_secs(5, 2), Some(20));
        assert_eq!(failure_backoff_secs(5, 3), Some(40));
        // Capped, and never below the ordinary interval.
        assert_eq!(failure_backoff_secs(5, 30), Some(FAILURE_BACKOFF_MAX_SECS));
        assert_eq!(
            failure_backoff_secs(600, 1),
            Some(600),
            "an interval already above the cap must not be shortened by backoff"
        );
        // No overflow panic on an absurd interval x streak. The result
        // stays the interval, because "never shorter than the ordinary
        // interval" outranks the cap — the cap exists to stop a FAST
        // poller hammering, not to speed a slow one up.
        assert_eq!(failure_backoff_secs(u64::MAX, 99), Some(u64::MAX));
    }

    #[test]
    fn porcelain_modify_add_delete_untracked() {
        let raw = " M src/a.rs\nA  src/b.rs\n D src/c.rs\n?? src/d.rs\n";
        let p = parse_porcelain(raw);
        assert_eq!(p.len(), 4);
        assert_eq!(p[0].status, "M");
        assert_eq!(p[2].status, "D"); // delete counts
        assert_eq!(p[3].status, "??"); // untracked counts
    }

    #[test]
    fn porcelain_rename_both_ends() {
        let p = parse_porcelain("R  old.rs -> new.rs\n");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].path, "new.rs");
        assert_eq!(p[0].orig_path.as_deref(), Some("old.rs"));
    }

    #[test]
    fn shortstat_full() {
        assert_eq!(
            parse_shortstat(" 3 files changed, 12 insertions(+), 4 deletions(-)"),
            (3, 12, 4)
        );
        assert_eq!(
            parse_shortstat(" 1 file changed, 2 insertions(+)"),
            (1, 2, 0)
        );
        assert_eq!(
            parse_shortstat(" 1 file changed, 5 deletions(-)"),
            (1, 0, 5)
        );
        assert_eq!(parse_shortstat(""), (0, 0, 0));
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let a = WorktreeDirty {
            repo: "r".into(),
            branch: "b".into(),
            files: vec![
                DirtyFile {
                    path: "a".into(),
                    status: "M".into(),
                    orig_path: None,
                },
                DirtyFile {
                    path: "b".into(),
                    status: "M".into(),
                    orig_path: None,
                },
            ],
            files_changed: 2,
            insertions: 0,
            deletions: 0,
        };
        let mut b = a.clone();
        b.files.reverse();
        assert_eq!(fingerprint(&a), fingerprint(&b));
        // A real change flips the fingerprint.
        let mut c = a.clone();
        c.files[0].status = "D".into();
        assert_ne!(fingerprint(&a), fingerprint(&c));
    }

    #[test]
    fn token_slot_refresh_threshold() {
        let now = 1_700_000_000;
        let fresh = TokenSlot {
            token: "x".into(),
            jti: uuid::Uuid::nil(),
            exp: now + 4 * 3600,
            ..Default::default()
        };
        assert!(!fresh.needs_refresh(now));
        let stale = TokenSlot {
            token: "x".into(),
            jti: uuid::Uuid::nil(),
            exp: now + 600,
            ..Default::default()
        };
        assert!(stale.needs_refresh(now));
    }
}
