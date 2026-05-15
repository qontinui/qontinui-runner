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
//! It replaces `coord.session_touched_files` as the heatmap's data
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
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Default cadence — 5 s per §4.8.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

/// Refresh the JWT proactively when this much time-to-expiry remains.
/// Mirrors `agent_pusher::TOKEN_REFRESH_MARGIN_SECS` (30 min).
const TOKEN_REFRESH_MARGIN_SECS: i64 = 30 * 60;

/// One worktree the poller watches.
#[derive(Debug, Clone)]
pub struct DirtyTarget {
    pub repo: String,
    pub branch: String,
    pub worktree_path: PathBuf,
}

/// Mutable token + bookkeeping. Same shape as
/// `agent_pusher::TokenSlot`; kept local so the poller doesn't depend
/// on the (not-yet-wired) pusher spawn machinery.
#[derive(Debug, Clone)]
pub struct TokenSlot {
    pub token: String,
    pub jti: uuid::Uuid,
    pub exp: i64,
}

impl TokenSlot {
    pub fn ttl_secs(&self, now_unix: i64) -> i64 {
        self.exp - now_unix
    }
    pub fn needs_refresh(&self, now_unix: i64) -> bool {
        self.ttl_secs(now_unix) < TOKEN_REFRESH_MARGIN_SECS
    }
}

/// State shared between the poller task and its handle.
#[derive(Debug)]
pub struct DirtyPollerState {
    pub agent_id: uuid::Uuid,
    pub machine_id: uuid::Uuid,
    pub coord_http_base: String,
    pub targets: Vec<DirtyTarget>,
    pub token: RwLock<TokenSlot>,
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
        if allocate.token.is_empty() {
            return None;
        }
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
            token: RwLock::new(TokenSlot {
                token: allocate.token.clone(),
                jti: allocate.token_jti,
                exp: allocate.token_exp,
            }),
            seen: RwLock::new(SeenState::default()),
        })
    }
}

/// Spawn the poller for one agent. Mirrors `agent_pusher::spawn` —
/// returns a handle whose `Drop` signals shutdown on the next tick.
pub fn spawn(state: Arc<DirtyPollerState>) -> DirtyPollerHandle {
    let interval_secs = std::env::var("QONTINUI_DIRTY_POLL_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n: &u64| *n > 0)
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);
    let cancel = Arc::new(tokio::sync::Notify::new());
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

/// Returned from [`spawn`]. Drop = stop the poller on its next tick.
pub struct DirtyPollerHandle {
    cancel: Arc<tokio::sync::Notify>,
    join: Option<tokio::task::JoinHandle<()>>,
    pub state: Arc<DirtyPollerState>,
}

impl Drop for DirtyPollerHandle {
    fn drop(&mut self) {
        self.cancel.notify_waiters();
        let _ = self.join.take();
    }
}

/// Process-global registry holding live poller handles so they aren't
/// `Drop`ped (which would stop the poller) the moment the allocation
/// handler returns. Agents are long-lived; teardown is best-effort and
/// dies with the runner — same trade-off `agent_pusher` documents.
/// Keyed by `agent_id` so a re-allocation of the same agent replaces
/// (and thereby stops) the prior poller.
static DIRTY_POLLERS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<uuid::Uuid, DirtyPollerHandle>>,
> = std::sync::OnceLock::new();

/// Spawn a poller for `state` and retain its handle for the runner's
/// lifetime. Replaces any prior poller for the same `agent_id`.
pub fn spawn_and_register(state: Arc<DirtyPollerState>) {
    let agent_id = state.agent_id;
    let handle = spawn(state);
    let map = DIRTY_POLLERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(mut guard) = map.lock() {
        // Inserting drops any prior handle → its poller stops cleanly.
        guard.insert(agent_id, handle);
        info!(
            "dirty_poller: registered agent_id={} (live pollers={})",
            agent_id,
            guard.len()
        );
    }
}

async fn run(state: Arc<DirtyPollerState>, interval_secs: u64, cancel: Arc<tokio::sync::Notify>) {
    info!(
        "dirty_poller: started agent_id={} repos={} interval={}s",
        state.agent_id,
        state.targets.len(),
        interval_secs
    );
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = cancel.notified() => {
                debug!("dirty_poller: agent_id={} stopping", state.agent_id);
                return;
            }
        }
        if let Err(e) = tick_once(&state).await {
            warn!(
                "dirty_poller: agent_id={} tick failed: {e:#}",
                state.agent_id
            );
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

/// One poll cycle. Public so an integration test can drive it without
/// the spawn machinery (same pattern as `agent_pusher::tick_once`).
///
/// Returns the `heartbeat` flag that was sent (true = no change since
/// the prior tick) so tests can assert change-detection.
pub async fn tick_once(state: &Arc<DirtyPollerState>) -> Result<bool> {
    maybe_refresh_token(state).await?;

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
    let (changed, seq) = {
        let mut seen = state.seen.write().await;
        let changed = new_fps.len() != seen.fingerprints.len()
            || new_fps
                .iter()
                .any(|(k, v)| seen.fingerprints.get(k) != Some(v));
        if changed {
            seen.seq += 1;
            seen.fingerprints = new_fps;
        }
        (changed, seen.seq)
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
    Ok(!changed)
}

/// `git status --porcelain` + `git diff --shortstat HEAD` for one
/// worktree.
async fn read_worktree_dirty(t: &DirtyTarget) -> Result<WorktreeDirty> {
    let status_out = Command::new("git")
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

    let shortstat_out = Command::new("git")
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
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .json(req)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
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

/// Token refresh — same contract as `agent_pusher::maybe_refresh_token`.
/// Best-effort: a failed refresh logs + returns Ok; next tick retries.
async fn maybe_refresh_token(state: &Arc<DirtyPollerState>) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    if !state.token.read().await.needs_refresh(now) {
        return Ok(());
    }
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
            warn!("dirty_poller: token refresh request failed: {e} — retry next tick");
            return Ok(());
        }
    };
    if !resp.status().is_success() {
        warn!(
            "dirty_poller: token refresh returned {} — retry next tick",
            resp.status()
        );
        return Ok(());
    }
    #[derive(Deserialize)]
    struct RefreshBody {
        token: String,
        jti: uuid::Uuid,
        exp: i64,
    }
    match resp.json::<RefreshBody>().await {
        Ok(b) => {
            let mut guard = state.token.write().await;
            *guard = TokenSlot {
                token: b.token,
                jti: b.jti,
                exp: b.exp,
            };
            info!(
                "dirty_poller: agent_id={} token refreshed jti={}",
                state.agent_id, guard.jti
            );
        }
        Err(e) => warn!("dirty_poller: refresh body decode failed: {e}"),
    }
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
        };
        assert!(!fresh.needs_refresh(now));
        let stale = TokenSlot {
            token: "x".into(),
            jti: uuid::Uuid::nil(),
            exp: now + 600,
        };
        assert!(stale.needs_refresh(now));
    }
}
