//! Ξ_Worktree reclaim executor (Phase 4, runner side).
//!
//! coord can decide that an on-disk worktree is reclaimable (orphaned, or
//! its junctions drifted) but it has no host filesystem access — only the
//! runner can act on the operator's Windows disk. coord therefore exposes
//! pending reclaim *instructions* per device; this module periodically
//! pulls them and executes the **INV-W4 safe path**.
//!
//! ## INV-W4 — the safe path (the field incident this guards against)
//!
//! The load-bearing rule: when removing a worktree that has junctioned
//! `node_modules` / `target` dirs, you MUST unlink each junction (remove
//! the reparse point WITHOUT recursing into it) BEFORE any recursive
//! delete of the worktree. A `remove_dir_all` that follows a junction
//! recurses into the *canonical* tree the junction points at and deletes
//! its contents — the worktree-junction-cleanup hazard
//! (`feedback_worktree_junction_cleanup_hazard`). So for every
//! `junctioned_path` we:
//!   1. verify it is actually a reparse point (via
//!      [`census::is_junction`]); a non-junction (a real dir that drifted)
//!      is left untouched — we never recursively delete a real dir we
//!      didn't expect to be a link;
//!   2. `remove_dir` (NOT `remove_dir_all`) the reparse point — on Windows
//!      this removes only the link, never its target;
//! and ONLY after all junctions are unlinked do we
//! `git worktree remove --force` (or remove the dir).
//!
//! ## Posture
//!
//! Machine-wide, anonymous, device-keyed — the same identity / coord-base
//! resolution as [`census`] (no agent JWT). Periodic poller (default 300s,
//! env `QONTINUI_WORKTREE_RECLAIM_INTERVAL_SECS`). Best-effort: a failing
//! tick `warn!`s and retries; the loop never panics.
//!
//! ## Arming (per-action, fail-safe)
//!
//! Arming is **per-action**, so the recoverable `rejunction` path can
//! graduate to default-on while destructive `remove` stays behind the
//! hardest gate (Phase 6.4 / Q1). coord ships two booleans in the pull:
//! `rejunction_armed` and `remove_armed`. Both default `false` runner-side
//! (`#[serde(default)]`), so a missing field — or an older coord that only
//! sends `dry_run` — fails SAFE: the action is advisory-only and every
//! instruction is merely LOGGED ("would do X"), nothing destructive
//! happens. This is exactly the posture the single `dry_run=true` default
//! used to give, now split per action.
//!
//! * `remove_armed`     ← coord's `COORD_WORKTREE_RECLAIM_ENABLED`.
//! * `rejunction_armed` ← graduates to default-on server-side once G6 is
//!   proven (rejunction never touches source, so it's recoverable).
//!
//! Defense in depth: even when armed, the runner NEVER acts on an
//! `is_dirty` worktree (coord also filters these), and G6 (below) SKIPS any
//! worktree that is currently building.
//!
//! "Dirty" here is **reclaim-scoped** ([`super::dirty`]): real work only, with
//! the runner's own untracked scaffolding (`.claude/`, `.coord-mcp-status`,
//! `.mcp.json`) excluded. Plain porcelain non-emptiness made ~34% of agent
//! worktrees permanently unreclaimable, because provisioning one is what made
//! it "dirty".
//!
//! ## G6 — in-flight-build guard (runner-only)
//!
//! Before executing ANY instruction's steps for a worktree, the runner
//! checks whether that worktree is currently being built and SKIPS it this
//! tick if so (logged at info with reason `building`). Two conservative
//! signals, either of which trips the guard:
//!
//!   (a) **cargo lock probe** — for each profile dir under the worktree's
//!       `target/` (`target/debug`, `target/release`, and `target/*/` one
//!       level deep), if a `.cargo-lock` file is present we try to open it
//!       with write share-mode. While `cargo` holds the build, that open
//!       fails with a sharing violation on Windows → building.
//!   (b) **recent-activity window** — if the mtime of any sink top-level
//!       (`target/`, `node_modules/`) or the worktree root is younger than
//!       `QONTINUI_WORKTREE_RECLAIM_ACTIVITY_WINDOW_SECS` (default 600s),
//!       the tree was just touched → treat as active.
//!
//! Conservative on error: a missing path is fine (not building); an
//! unexpected IO error is treated AS building (skip, retry next tick).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::process_helpers::{run_probe, ProbeOutcome};

/// Budget for every external command in the reclaim tick.
///
/// All of them are local: `git worktree remove/prune`, `git status
/// --porcelain`, `mklink /J`. A healthy call is milliseconds; the hang class
/// is an `index.lock` held by a concurrent git, or a `mklink` against a
/// wedged filesystem. The reclaim loop ticks every 300s on a single
/// blocking-pool thread, so an unbounded hang here removed that thread from
/// the pool permanently — the 2026-08-30 defect class. 30s absorbs a busy
/// tree without ever letting the tick outlive its own interval.
const RECLAIM_CMD_TIMEOUT: Duration = Duration::from_secs(30);

use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::census::{is_junction, CloneRootness};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// Default reclaim poll cadence — 300s (5 min), matching the census.
/// Override via `QONTINUI_WORKTREE_RECLAIM_INTERVAL_SECS`.
const DEFAULT_RECLAIM_INTERVAL_SECS: u64 = 300;

/// Default G6 "recently active" window — 600s. A worktree whose `target/`,
/// `node_modules/`, or root was touched within this window is treated as
/// actively building and skipped. Override via
/// `QONTINUI_WORKTREE_RECLAIM_ACTIVITY_WINDOW_SECS`.
const DEFAULT_ACTIVITY_WINDOW_SECS: u64 = 600;

/// Default cadence of the low-frequency local `git worktree prune` pass
/// (Phase 3 registration hygiene) — daily. Override via
/// `QONTINUI_WORKTREE_PRUNE_INTERVAL_SECS`.
const DEFAULT_PRUNE_INTERVAL_SECS: u64 = 86_400;

/// Default age ceiling for the coord-absent local backstop sweep (Phase 4)
/// — 14 days. A session-worktree dir must be OLDER than this (by mtime)
/// before the backstop may even consider it. Override via
/// `QONTINUI_WORKTREE_BACKSTOP_MAX_AGE_SECS`.
const DEFAULT_BACKSTOP_MAX_AGE_SECS: u64 = 14 * 86_400;

/// Default age floor for the empty-session-dir sweep — 1 hour. Allocation
/// creates `<session-uuid>/` and only then runs a network fetch + `git
/// worktree add`, so a *legitimately* empty session dir exists for as long as
/// that takes. An hour is far beyond any allocation and far below the age of
/// a real husk. Override via
/// `QONTINUI_WORKTREE_EMPTY_SESSION_MIN_AGE_SECS`.
const DEFAULT_EMPTY_SESSION_MIN_AGE_SECS: u64 = 3_600;

/// After this many CONSECUTIVE failed pulls the poller stops warning at `warn`
/// and starts reporting at `error` with a distinct marker. At the 300s default
/// cadence this is ~25 minutes — long enough that an ordinary coord blip or
/// deploy stays quiet, short enough that a genuine outage is loud the same
/// hour rather than five days later.
const RECLAIM_FAILURE_ESCALATION_STREAK: u64 = 5;

// ---------------------------------------------------------------------------
// Poller health — published so "the reaper is dead" is a READ, not an autopsy.
// ---------------------------------------------------------------------------

/// Consecutive failed pulls; reset to 0 by any success.
static POLLER_CONSECUTIVE_FAILURES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// Unix seconds of the last SUCCESSFUL pull. 0 = never succeeded this process.
static POLLER_LAST_SUCCESS_UNIX: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// The most recent failure message (already carries its source chain).
static POLLER_LAST_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// A snapshot of the background poller's health.
///
/// This exists because of the 2026-07-28 → 08-01 outage: the poller failed
/// every pull for five days while the endpoint that REPORTS on reclaim kept
/// answering `coord_reachable: true` — because that field describes the
/// reporting request the endpoint had just made, not the executor. A health
/// signal that reports on its own inputs rather than on the subsystem's output
/// cannot detect that subsystem being dead. These fields describe the EXECUTOR.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PollerHealth {
    /// Consecutive failed pulls. `0` with a recent `last_success_unix` is the
    /// only combination that means "the executor is working".
    pub consecutive_failures: u64,
    /// Unix seconds of the last successful pull; `None` = never, this process.
    pub last_success_unix: Option<u64>,
    /// Last failure, with its full cause chain. `None` while healthy.
    pub last_error: Option<String>,
}

/// Read the poller's health. Cheap; safe from any thread.
pub fn poller_health() -> PollerHealth {
    let last_success = POLLER_LAST_SUCCESS_UNIX.load(std::sync::atomic::Ordering::Relaxed);
    PollerHealth {
        consecutive_failures: POLLER_CONSECUTIVE_FAILURES
            .load(std::sync::atomic::Ordering::Relaxed),
        last_success_unix: (last_success != 0).then_some(last_success),
        last_error: POLLER_LAST_ERROR.lock().ok().and_then(|g| g.clone()),
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Record a successful pull.
fn note_poll_success() {
    POLLER_CONSECUTIVE_FAILURES.store(0, std::sync::atomic::Ordering::Relaxed);
    POLLER_LAST_SUCCESS_UNIX.store(now_unix(), std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = POLLER_LAST_ERROR.lock() {
        *g = None;
    }
}

/// Record a failed pull and log it at a level that reflects the STREAK, not
/// just the one tick. A lone failure is a blip; an unbroken run of them is an
/// outage, and it must not keep reading as routine noise.
fn note_poll_failure(err: &str) {
    let streak = POLLER_CONSECUTIVE_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if let Ok(mut g) = POLLER_LAST_ERROR.lock() {
        *g = Some(err.to_string());
    }
    if streak >= RECLAIM_FAILURE_ESCALATION_STREAK {
        let since = match POLLER_LAST_SUCCESS_UNIX.load(std::sync::atomic::Ordering::Relaxed) {
            0 => "never succeeded this process".to_string(),
            t => format!("last success {}s ago", now_unix().saturating_sub(t)),
        };
        tracing::error!(
            "worktree_reclaim: RECLAIM POLLER DOWN — {streak} consecutive failed pulls \
             ({since}); NO worktree can be reclaimed while this persists: {err}"
        );
    } else {
        warn!("worktree_reclaim: {err}");
    }
}

// ---------------------------------------------------------------------------
// Wire types — coord serializes these.
// ---------------------------------------------------------------------------

/// The pull-endpoint body: `GET {coord}/coord/worktree-reclaim/{device_id}`.
///
/// Per-action arming (Q1): `remove` and `rejunction` are armed
/// independently. BOTH flags default `false` (`#[serde(default)]`), so a
/// missing field — or an older coord that only sends the legacy `dry_run` —
/// fails SAFE: the action is advisory-only (logged "would do X"), never
/// destructive. This replaces the single global `dry_run` bool.
#[derive(Debug, Clone, Deserialize)]
pub struct ReclaimPull {
    /// Arms the non-destructive `rejunction` action. Defaults `false`
    /// (fail-safe). coord graduates this to default-on once G6 is proven.
    #[serde(default)]
    pub rejunction_armed: bool,
    /// Arms the destructive `remove` action. Defaults `false` (fail-safe).
    /// coord drives this from `COORD_WORKTREE_RECLAIM_ENABLED`.
    #[serde(default)]
    pub remove_armed: bool,
    #[serde(default)]
    pub instructions: Vec<ReclaimInstruction>,
    /// Forward-compatible: the candidates coord DEFERRED, with the typed
    /// reason its §4 gate refused them for. Absent on today's coord (the
    /// route only ships cleared instructions), so the runner's on-demand
    /// survey ([`super::on_demand`]) falls back to deriving blocked items
    /// from its own census. `#[serde(default)]` keeps an empty vec the safe
    /// default — never a deserialization failure that would kill the poller.
    #[serde(default)]
    pub blocked: Vec<BlockedWorktree>,
}

/// One coord-deferred worktree: the path plus the gate reason that held it
/// back. `reason` carries coord's `DeferReason` snake_case token
/// (`dirty` | `not_landed` | `other_live_reference` | `serialize_claim_active`
/// | `grace_pending` | `not_a_candidate`), or `pinned` once Phase 2 lands.
#[derive(Debug, Clone, Deserialize)]
pub struct BlockedWorktree {
    pub worktree_path: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub reason: String,
    /// Phase-2 retention pin (`auto` | `pinned`). `None` on a pre-Phase-2
    /// coord — treated as unpinned.
    #[serde(default)]
    pub retention: Option<String>,
}

/// What coord wants done to one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReclaimAction {
    /// Unlink junctions, then remove the worktree.
    Remove,
    /// Re-create the junction(s) from the worktree's sink to main.
    Rejunction,
    /// Forward-compatibility: an action this runner build doesn't know.
    /// Treated as a no-op (logged), never destructive.
    Unknown(String),
}

impl<'de> Deserialize<'de> for ReclaimAction {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        Ok(match s.as_str() {
            "remove" => ReclaimAction::Remove,
            "rejunction" => ReclaimAction::Rejunction,
            other => ReclaimAction::Unknown(other.to_string()),
        })
    }
}

/// One reclaim instruction.
#[derive(Debug, Clone, Deserialize)]
pub struct ReclaimInstruction {
    pub worktree_path: String,
    pub repo: String,
    pub action: ReclaimAction,
    /// `worktree:orphan` | `worktree:unjunctioned` (free-form; logged).
    #[serde(default)]
    pub reason: String,
    /// coord's view of dirtiness. The runner ALSO re-checks via git and
    /// refuses to act on a dirty worktree regardless of this flag
    /// (defense in depth).
    #[serde(default)]
    pub is_dirty: bool,
    /// Junctioned sink dirs *relative to the worktree* (e.g.
    /// `["target", "node_modules"]`).
    #[serde(default)]
    pub junctioned_paths: Vec<String>,
    /// Phase-2 retention pin (`auto` | `pinned`), forward-compatible.
    /// `None` on a pre-Phase-2 coord — treated as unpinned. coord's own
    /// gate will withhold a pinned worktree, so this is belt-and-braces for
    /// the on-demand path.
    #[serde(default)]
    pub retention: Option<String>,
}

// ---------------------------------------------------------------------------
// The safe-path plan (pure decision logic — unit-tested without real FS).
// ---------------------------------------------------------------------------

/// One concrete step in the reclaim plan. The ORDER of these in the
/// returned `Vec` is the safety contract: every [`ReclaimStep::UnlinkJunction`]
/// precedes the [`ReclaimStep::RemoveWorktree`] (INV-W4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReclaimStep {
    /// Unlink the reparse point at this absolute path WITHOUT recursing.
    UnlinkJunction(PathBuf),
    /// Remove the worktree rooted at this absolute path (git worktree
    /// remove --force, falling back to a dir remove). Only emitted AFTER
    /// all `UnlinkJunction` steps.
    RemoveWorktree(PathBuf),
    /// (Re)create a junction at `link` pointing at `target`.
    CreateJunction { link: PathBuf, target: PathBuf },
    /// Explicitly do nothing (dirty / unknown action / dry-run). Carries a
    /// human-readable reason for the log.
    Skip(String),
}

/// Compute the ordered step plan for one instruction. PURE — no
/// filesystem mutation, no junction *verification* (that happens at
/// execution time, where we have the real disk). This is the function the
/// unit tests pin: it guarantees junction-unlinks come before the
/// worktree removal, that a dirty instruction yields only a `Skip`, that
/// an UNARMED action yields only `Skip`s, and that an instruction whose
/// worktree root is ABSENT on disk yields only a `Skip`.
///
/// Arming is per-action (Q1): a `Remove` is destructive only when
/// `remove_armed`; a `Rejunction` creates junctions only when
/// `rejunction_armed`. An unarmed action logs "would do X" exactly like the
/// old global dry-run path.
///
/// `canonical_path` is the repo's canonical checkout — the junction target
/// root for a `rejunction`. `None` when the runner couldn't resolve it
/// (then a rejunction degrades to a `Skip`).
///
/// `root_exists` is the caller's dispatch-time `worktree_path` existence
/// probe. `false` means coord's instruction is built on a stale census (the
/// worktree was deleted out-of-band): executing a `Rejunction` against it
/// would re-materialize the root as an empty husk (the 2026-06-12 incident),
/// so the ENTIRE instruction is skipped — for `Remove` too, uniformly. The
/// invariant this pins: a reclaim execution must never create a filesystem
/// path, except junction reparse points inside an already-existing worktree
/// root.
pub fn plan_reclaim(
    instr: &ReclaimInstruction,
    rejunction_armed: bool,
    remove_armed: bool,
    canonical_path: Option<&Path>,
    root_exists: bool,
) -> Vec<ReclaimStep> {
    // Absent-root guard — a stale instruction for a worktree that no longer
    // exists must do NOTHING (the next census is the ack that clears it).
    if !root_exists {
        return vec![ReclaimStep::Skip(format!(
            "skipping {} — absent on disk (stale instruction; next census is the ack)",
            instr.worktree_path
        ))];
    }

    // Defense in depth #1 — NEVER act on a dirty worktree.
    if instr.is_dirty {
        return vec![ReclaimStep::Skip(format!(
            "is_dirty worktree {} — refusing all destructive action",
            instr.worktree_path
        ))];
    }

    // Per-action arming — an unarmed action is advisory-only. We still emit
    // a Skip so the caller logs "would do X".
    let armed = match &instr.action {
        ReclaimAction::Remove => remove_armed,
        ReclaimAction::Rejunction => rejunction_armed,
        // Unknown actions are never "armed" — they always no-op below.
        ReclaimAction::Unknown(_) => true,
    };
    if !armed {
        return vec![ReclaimStep::Skip(format!(
            "unarmed: would {:?} worktree {} (reason={})",
            instr.action, instr.worktree_path, instr.reason
        ))];
    }

    let worktree = PathBuf::from(&instr.worktree_path);
    match &instr.action {
        ReclaimAction::Remove => {
            let mut steps: Vec<ReclaimStep> = Vec::new();
            // INV-W4: unlink EVERY junction first, in the order coord
            // listed them, BEFORE the recursive worktree removal.
            for rel in &instr.junctioned_paths {
                steps.push(ReclaimStep::UnlinkJunction(worktree.join(rel)));
            }
            // Only now the worktree removal.
            steps.push(ReclaimStep::RemoveWorktree(worktree));
            steps
        }
        ReclaimAction::Rejunction => {
            let Some(canonical) = canonical_path else {
                return vec![ReclaimStep::Skip(format!(
                    "rejunction {} — no canonical path resolved for repo {}",
                    instr.worktree_path, instr.repo
                ))];
            };
            if instr.junctioned_paths.is_empty() {
                return vec![ReclaimStep::Skip(format!(
                    "rejunction {} — no junctioned_paths to recreate",
                    instr.worktree_path
                ))];
            }
            instr
                .junctioned_paths
                .iter()
                .map(|rel| ReclaimStep::CreateJunction {
                    link: worktree.join(rel),
                    target: canonical.join(rel),
                })
                .collect()
        }
        ReclaimAction::Unknown(a) => vec![ReclaimStep::Skip(format!(
            "unknown reclaim action {a:?} for {} — ignoring",
            instr.worktree_path
        ))],
    }
}

// ---------------------------------------------------------------------------
// Execution (the side-effecting half).
// ---------------------------------------------------------------------------

/// Execute one planned step. Idempotent: a missing worktree / already-
/// unlinked junction is `Ok(())`, not an error.
pub(super) fn execute_step(step: &ReclaimStep) -> Result<(), String> {
    match step {
        ReclaimStep::Skip(reason) => {
            info!("worktree_reclaim: skip — {reason}");
            Ok(())
        }
        ReclaimStep::UnlinkJunction(path) => unlink_junction(path),
        ReclaimStep::RemoveWorktree(path) => remove_worktree(path),
        ReclaimStep::CreateJunction { link, target } => create_junction(link, target),
    }
}

/// Unlink a junction WITHOUT recursing into it (INV-W4). Verifies the path
/// is a reparse point BEFORE removing — a path that isn't a junction
/// (already unlinked, or a real dir we didn't expect) is left untouched
/// and reported as a success no-op. We use `remove_dir` (NOT
/// `remove_dir_all`): on Windows that removes only the reparse point, not
/// its target.
fn unlink_junction(path: &Path) -> Result<(), String> {
    if !path.exists() && !is_junction(path) {
        // Already gone (or never existed) — idempotent no-op.
        debug!(
            "worktree_reclaim: junction {} absent — nothing to unlink",
            path.display()
        );
        return Ok(());
    }
    if !is_junction(path) {
        // NOT a reparse point. NEVER recursively delete here — this is the
        // exact case that would gut the canonical tree. Leave it; coord
        // can re-evaluate.
        warn!(
            "worktree_reclaim: {} is NOT a junction (real dir?) — refusing to remove (INV-W4)",
            path.display()
        );
        return Ok(());
    }
    // Confirmed reparse point — remove ONLY the link.
    match std::fs::remove_dir(path) {
        Ok(()) => {
            info!("worktree_reclaim: unlinked junction {}", path.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("unlink junction {}: {e}", path.display())),
    }
}

/// Remove the worktree dir. Tries `git worktree remove --force` from the
/// worktree itself first (so git prunes its administrative metadata);
/// falls back to a plain `remove_dir_all`. Safe to call only AFTER every
/// junction has been unlinked (the caller's plan guarantees the ordering).
/// Idempotent: a missing dir is `Ok(())`.
///
/// ## INV-W5 — never recursively delete a real clone
///
/// The fallback is unconditional: it does not care WHY `git worktree remove`
/// failed. On a repo root that command always fails ("is a main working tree"),
/// so the `remove_dir_all` below would delete the whole clone, object store
/// included. Neither dirty guard catches it — a clean clone is genuinely clean.
///
/// So prove the structure before destroying it, exactly as [`unlink_junction`]
/// proves a path is a reparse point before unlinking: a linked worktree has
/// `.git` as a FILE, a clone has it as a DIRECTORY. This guard is the last mile
/// and covers every caller — coord-issued instructions, the coord-absent
/// backstop sweep, and anything added later — so a classifier defect upstream
/// can cost a wasted instruction but never a repository.
///
/// ### An independent last mile cannot rest on a two-state predicate
///
/// It used to read `path.join(".git").is_dir()`, which is not a proof of
/// anything: `is_dir()` TRAVERSES symlinks and folds `EACCES` / `ENOTDIR` /
/// `EIO` / `ELOOP` / a dead mount into `false` — and `false` here means "not a
/// clone", i.e. "delete it". A real clone whose `.git` is a symlink to a
/// volume that is no longer mounted read `false`, passed the guard, and had
/// its whole working tree removed by the `remove_dir_all` below. The guard
/// was independent of the upstream classifier but not of the filesystem's
/// error paths, and a last mile that folds errors into permission is not a
/// last mile.
///
/// It now reads [`census::probe_clone_rootness`], which is tri-state for
/// exactly the reason [`super::dirty::GitPresence`] is: only
/// [`CloneRootness::NotCloneRoot`] is a MEASUREMENT of "safe to remove
/// recursively", and it is the only value this gate lets through. A clone
/// refuses; a path whose clone-ness could not be established refuses too, with
/// its own message, because "we could not look" is not "there is nothing
/// there".
pub(super) fn remove_worktree(path: &Path) -> Result<(), String> {
    if !path.exists() {
        debug!(
            "worktree_reclaim: worktree {} already gone — no-op",
            path.display()
        );
        return Ok(());
    }
    // Err, not a silent Ok, on BOTH refusing arms: neither must be counted as a
    // removal, and both must be loud enough to diagnose what stopped it.
    match super::census::probe_clone_rootness(path) {
        CloneRootness::NotCloneRoot => {}
        CloneRootness::CloneRoot => {
            return Err(format!(
                "refusing to remove {} — `.git` is a DIRECTORY, so this is a real \
                 clone, not a linked worktree (INV-W5)",
                path.display()
            ));
        }
        CloneRootness::Undetermined => {
            return Err(format!(
                "refusing to remove {} — could not determine whether `.git` is a \
                 clone's object store: it is a symlink we will not traverse, or \
                 the lookup failed for a reason that is not NotFound (EACCES, \
                 ENOTDIR, EIO, ELOOP, a dead mount). An unmeasured path is not a \
                 measured absence, and this is the last mile before \
                 remove_dir_all (INV-W5)",
                path.display()
            ));
        }
    }
    let path_str = path.to_string_lossy().to_string();
    // `git -C <wt> worktree remove --force <wt>` prunes the registration.
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(["-C", &path_str, "worktree", "remove", "--force", &path_str]);
    let git_ok = matches!(
        run_probe(
            cmd,
            RECLAIM_CMD_TIMEOUT,
            "worktree_reclaim: git worktree remove"
        ),
        ProbeOutcome::Captured(_)
    );
    if git_ok {
        info!(
            "worktree_reclaim: git worktree remove --force {} ok",
            path.display()
        );
    }
    // Whether git removed it or not, ensure the dir is gone. At this point
    // all junctions are unlinked, so remove_dir_all can't recurse into a
    // canonical tree.
    if path.exists() {
        std::fs::remove_dir_all(path)
            .map_err(|e| format!("remove worktree dir {}: {e}", path.display()))?;
        info!("worktree_reclaim: removed worktree dir {}", path.display());
    }
    // A session dir we just emptied is garbage — prune it here so husks can
    // never accumulate 1:1 with successful reclaims.
    prune_empty_session_parent(path);
    Ok(())
}

/// Remove `worktree`'s parent when our removal just emptied it.
///
/// Session worktrees live at `<root>/<session-uuid>/<repo>`, so reclaiming the
/// `<repo>` checkout leaves the `<session-uuid>` dir behind as an empty husk.
/// Nothing else ever removed it on the coord-instructed path, so husks grew one
/// per successful reclaim — 1,554 of them observed on the operator's box
/// 2026-08-03 against only 28 populated dirs, which makes any directory-count
/// measurement of "how many worktrees are left" wildly wrong.
///
/// Two guards, mirroring [`backstop_sweep`]'s existing hygiene step (which
/// already did this for its own path — this brings the coord-instructed path
/// to parity):
///
///  1. **Non-recursive `remove_dir`** — never `remove_dir_all`. It fails
///     harmlessly when anything remains, so a sibling repo worktree for the
///     same session (concurrently allocated, or simply not yet reclaimed) is
///     never destroyed. That makes the operation inherently race-safe.
///  2. **UUID-named parents only** — the same scope test `backstop_sweep`
///     applies. The worktree root itself, `qontinui-root`, and operator
///     scratch dirs are not UUID-named, so this can never walk up out of the
///     session layer no matter what path a caller passes.
///
/// Best-effort by construction: every failure is expected traffic (non-empty,
/// already gone, or held), so nothing here can fail a reclaim.
fn prune_empty_session_parent(worktree: &Path) {
    let Some(parent) = worktree.parent() else {
        return;
    };
    if !is_session_uuid_dir(parent) {
        debug!(
            "worktree_reclaim: parent {} is not a session-uuid dir — not pruning",
            parent.display()
        );
        return;
    }
    match std::fs::remove_dir(parent) {
        Ok(()) => info!(
            "worktree_reclaim: pruned empty session dir {}",
            parent.display()
        ),
        Err(e) => debug!(
            "worktree_reclaim: session dir {} not pruned ({e}) — expected when \
             siblings remain",
            parent.display()
        ),
    }
}

/// Is `path`'s final segment a session UUID?
///
/// The scope test every session-dir prune shares. The worktree root itself,
/// `qontinui-root`, and operator scratch dirs are not UUID-named, so a prune
/// keyed on this can never walk up out of the session layer no matter what
/// path a caller passes.
fn is_session_uuid_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| Uuid::parse_str(n).is_ok())
}

/// Create (or recreate) a junction at `link` pointing at `target`
/// (`mklink /J` semantics). Idempotent: if `link` is already a junction
/// we leave it; if it's a stale dir we remove the link first.
#[cfg(windows)]
fn create_junction(link: &Path, target: &Path) -> Result<(), String> {
    if !target.exists() {
        return Err(format!(
            "rejunction: target {} does not exist",
            target.display()
        ));
    }
    // If a junction is already there, treat as done (idempotent).
    if is_junction(link) {
        debug!(
            "worktree_reclaim: junction {} already present — no-op",
            link.display()
        );
        return Ok(());
    }
    // A real dir/file in the way (not a junction) — refuse, to avoid
    // clobbering operator data.
    if link.exists() {
        return Err(format!(
            "rejunction: {} exists and is not a junction — refusing to clobber",
            link.display()
        ));
    }
    // A missing parent means the worktree root itself is gone (for the sink
    // links `<wt>/node_modules` / `<wt>/target`, `link.parent()` IS the
    // worktree root). The only legitimate parents are created by
    // `git worktree add`, never by reclaim — creating them here would
    // re-materialize a deleted worktree as an empty husk.
    if let Some(parent) = link.parent() {
        if !parent.exists() {
            return Err(format!(
                "rejunction: link parent {} missing — refusing to create directories",
                parent.display()
            ));
        }
    }
    // `cmd /C mklink /J <link> <target>` — /J = directory junction.
    let mut cmd = crate::process_helpers::no_window("cmd");
    cmd.args([
        "/C",
        "mklink",
        "/J",
        &link.to_string_lossy(),
        &target.to_string_lossy(),
    ]);
    // `output_with_timeout`, not `run_probe`: the failure arm below needs the
    // child's stderr verbatim, and a timeout surfaces here as an io error whose
    // Display already names the pid + budget.
    let out = crate::process_helpers::output_with_timeout(cmd, RECLAIM_CMD_TIMEOUT)
        .map_err(|e| format!("rejunction: spawn mklink: {e}"))?;
    if out.status.success() {
        info!(
            "worktree_reclaim: rejunctioned {} -> {}",
            link.display(),
            target.display()
        );
        Ok(())
    } else {
        Err(format!(
            "rejunction: mklink /J {} {} failed: {}",
            link.display(),
            target.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Non-Windows: junctions are a Windows concept; rejunction is a no-op
/// (the runner ships on Windows — this arm exists so the crate builds on
/// CI's other targets).
#[cfg(not(windows))]
fn create_junction(link: &Path, _target: &Path) -> Result<(), String> {
    debug!(
        "worktree_reclaim: rejunction {} skipped (non-windows)",
        link.display()
    );
    Ok(())
}

/// Re-verify dirtiness at execution time (defense in depth — the pull may
/// be stale). Reclaim-scoped: `git status --porcelain` MINUS the runner's own
/// untracked scaffolding, via [`super::dirty::porcelain_is_dirty`] — the same
/// predicate the census reports with, so the two can never disagree and
/// strand a worktree as "clean per coord, dirty per the runner" forever.
///
/// ## A tree we could not read is DIRTY, not clean
///
/// This used to map every probe failure to `false`, on the reasoning that a
/// failed `.output()` had always done so. That reasoning was only half right,
/// and the missing half is a data-loss path:
///
/// * A failed `.output()` DID answer `false` — but a HUNG `git status` never
///   returned at all, so [`execute_pull`]'s `Remove` guard could not be
///   reached with a fabricated "clean". The hang was a (terrible) safety
///   interlock.
/// * Bounding the child removed that interlock. A concurrent `index.lock` —
///   exactly the hang class the bound exists for — now returns
///   [`ProbeOutcome::Degraded`] at [`RECLAIM_CMD_TIMEOUT`], and a `false`
///   here lets the armed `Remove` through. [`remove_worktree`]'s own
///   `git worktree remove --force` is bounded by the same budget and degrades
///   to `std::fs::remove_dir_all` — so the uncommitted work is destroyed and
///   the only trace is a WARN naming a killed pid.
///
/// So an unreadable tree answers `true` (see
/// [`super::dirty::DirtyVerdict::as_conservative_bool`]). The one carve-out is
/// a directory with NO `.git` at all, which cannot be holding uncommitted work
/// — the same distinction [`backstop_dirty_verdict`] has always made.
pub(super) fn worktree_is_dirty(path: &Path) -> bool {
    super::dirty::probe_reclaim_dirty(
        path,
        RECLAIM_CMD_TIMEOUT,
        "worktree_reclaim: git status --porcelain",
    )
    .as_conservative_bool()
}

// ---------------------------------------------------------------------------
// G6 — in-flight-build guard (runner-only). Pure-ish + injectable for tests.
// ---------------------------------------------------------------------------

/// Resolve the G6 activity window (seconds) from the env, default 600s.
fn activity_window_secs() -> u64 {
    std::env::var("QONTINUI_WORKTREE_RECLAIM_ACTIVITY_WINDOW_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_ACTIVITY_WINDOW_SECS)
}

/// True iff `path` exists and its mtime is younger than `window`.
/// A missing path → `false` (not active). An unreadable mtime is treated
/// conservatively as active (`true`) — we'd rather skip than reclaim a tree
/// we can't reason about.
fn path_recently_touched(path: &Path, window: Duration) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        // Missing path is fine — nothing to be active.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        // Any other IO error → conservative: treat as active.
        Err(_) => return true,
    };
    match meta.modified() {
        Ok(mtime) => match mtime.elapsed() {
            // Younger than the window → recently touched.
            Ok(elapsed) => elapsed < window,
            // mtime in the future (clock skew) → treat as recently touched.
            Err(_) => true,
        },
        // Unreadable mtime → conservative: active.
        Err(_) => true,
    }
}

/// True iff a `.cargo-lock` under any profile dir of `target_dir` cannot be
/// opened with write share-mode — i.e. a live `cargo` invocation holds it.
///
/// We probe `target/debug`, `target/release`, and every `target/*/`
/// directory one level deep (covers custom profiles + per-triple dirs). A
/// `.cargo-lock` that opens cleanly (or is absent) → not building; a
/// sharing/permission failure → building.
fn cargo_lock_held(target_dir: &Path) -> bool {
    // Collect candidate profile dirs: the two well-known ones plus any
    // first-level subdir of `target/`.
    let mut profile_dirs: Vec<PathBuf> = vec![target_dir.join("debug"), target_dir.join("release")];
    if let Ok(entries) = std::fs::read_dir(target_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                profile_dirs.push(p);
            }
        }
    }

    for dir in profile_dirs {
        let lock = dir.join(".cargo-lock");
        match std::fs::symlink_metadata(&lock) {
            // No lock file here — this profile isn't building.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            // Can't even stat it (other error) → conservative: building.
            Err(_) => return true,
            Ok(_) => {}
        }
        // Lock present — try an exclusive-ish open. On Windows, while cargo
        // holds it, opening with write access fails with a sharing
        // violation (PermissionDenied). A clean open → cargo is NOT holding
        // it (stale lock from a crashed build).
        match std::fs::OpenOptions::new().write(true).open(&lock) {
            Ok(_) => continue, // openable → not held → not building (this dir)
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return true, // sharing/permission failure → building
        }
    }
    false
}

/// G6 probe with the env-resolved activity window — the shared entrypoint
/// for the executor AND the census shadow-mode report
/// ([`super::census`] pushes the result per worktree per tick as
/// `building`, so coord can gauge would-be G6 skips while arming is OFF).
pub(super) fn probe_building(worktree: &Path) -> bool {
    worktree_is_building(worktree, Duration::from_secs(activity_window_secs()))
}

/// Census-only build signal: a genuinely-held cargo build lock under
/// `<worktree>/target/`, and NOTHING else. Unlike [`probe_building`] (used by
/// the reclaim executor, which legitimately also skips a recently-touched
/// worktree) this DROPS the recent-activity heuristic — an editor save,
/// `git status`, or any write to the worktree root bumps mtime WITHOUT a build
/// in flight. Reporting that as `building` in the FLEET census over-reported
/// coord's build-concurrency gauge (`count_building` climbed across idle/edited
/// dev worktrees with no real builds), pinning `decide_isolation` in rule 3c so
/// every allocate `Wait`-ed and sessions fell back to the shared root. Phase 2
/// of `plans/2026-06-08-coord-build-slot-budget-saturation-fix.md`; coord's
/// building-freshness TTL is the backstop for any flag that still goes stale.
pub(super) fn probe_building_for_census(worktree: &Path) -> bool {
    cargo_lock_held(&worktree.join("target"))
}

/// G6: is this worktree currently being built? Either signal → skip.
/// Injectable (takes the worktree root) so tests drive it with tempdirs.
///
///   (a) a held `.cargo-lock` under `<worktree>/target/`, or
///   (b) `target/`, `node_modules/`, or the worktree root touched within
///       `window`.
fn worktree_is_building(worktree: &Path, window: Duration) -> bool {
    let target = worktree.join("target");
    if cargo_lock_held(&target) {
        return true;
    }
    // Recent-activity window across the sink top-levels + the root.
    path_recently_touched(&target, window)
        || path_recently_touched(&worktree.join("node_modules"), window)
        || path_recently_touched(worktree, window)
}

/// Execute all instructions in one pull.
fn execute_pull(pull: &ReclaimPull) {
    if pull.instructions.is_empty() {
        debug!("worktree_reclaim: no instructions");
        return;
    }
    info!(
        "worktree_reclaim: {} instruction(s), rejunction_armed={} remove_armed={}",
        pull.instructions.len(),
        pull.rejunction_armed,
        pull.remove_armed
    );
    let window = Duration::from_secs(activity_window_secs());
    for instr in &pull.instructions {
        let wt = PathBuf::from(&instr.worktree_path);

        // Absent-root probe — once per instruction, at dispatch time. A
        // worktree deleted out-of-band (operator cleanup, another machine)
        // leaves coord serving stale instructions until the next census
        // tick; executing one would re-create the root as an empty husk.
        let root_exists = wt.exists();
        if !root_exists {
            warn!(
                "worktree_reclaim: skipping {} — absent on disk (stale instruction; \
                 next census is the ack)",
                instr.worktree_path
            );
        }

        // Whether THIS instruction's action is armed (would actually do
        // something destructive/mutating this tick). Skip the execution-time
        // guards entirely for an unarmed (advisory) instruction so the
        // "would do X" Skip still logs.
        let armed = match &instr.action {
            ReclaimAction::Remove => pull.remove_armed,
            ReclaimAction::Rejunction => pull.rejunction_armed,
            ReclaimAction::Unknown(_) => false,
        };

        // G6 — evaluated for EVERY instruction, armed or not. Unarmed
        // ("shadow mode") logs the would-skip so the census-carried
        // `building` fact + these lines give the Q1 graduation its
        // prove-out data while arming is still OFF; armed skips for real.
        let building = worktree_is_building(&wt, window);
        if building && !armed {
            info!(
                "worktree_reclaim: {} building — WOULD skip (reason=building, shadow/unarmed)",
                instr.worktree_path
            );
            // fall through: the unarmed plan still logs its advisory
            // "would do X" Skip step below.
        }

        if armed {
            // G6 — never act on a worktree that is currently building.
            if building {
                info!(
                    "worktree_reclaim: {} building — skipping (reason=building)",
                    instr.worktree_path
                );
                continue;
            }

            // Execution-time re-check: even if coord said clean, refuse a
            // worktree that's dirty right now (only relevant for Remove).
            if instr.action == ReclaimAction::Remove && worktree_is_dirty(&wt) {
                warn!(
                    "worktree_reclaim: {} dirty at execution time — skipping remove (INV defense)",
                    instr.worktree_path
                );
                continue;
            }
        }

        let canonical = super::canonical_paths::default_canonical_path(&instr.repo).ok();
        let steps = plan_reclaim(
            instr,
            pull.rejunction_armed,
            pull.remove_armed,
            canonical.as_deref(),
            root_exists,
        );
        let removed = execute_steps(&instr.worktree_path, &steps).is_ok()
            && steps
                .iter()
                .any(|s| matches!(s, ReclaimStep::RemoveWorktree(_)));
        // Phase 3 registration hygiene: after a SUCCESSFUL removal, prune
        // the parent repo's `.git/worktrees/` registration. Best-effort —
        // a prune failure warns inside `prune_parent_repo`, never fails
        // the removal result.
        if removed {
            if let Some(c) = canonical.as_deref() {
                prune_parent_repo(c);
            } else {
                debug!(
                    "worktree_reclaim: removed {} but no canonical path for repo {} — \
                     registration prune deferred to the daily pass",
                    instr.worktree_path, instr.repo
                );
            }
        }
    }
}

/// Execute an ordered [`ReclaimStep`] plan, honoring the INV-W4 abort rule:
/// a FAILED `UnlinkJunction` stops the plan immediately, because the
/// remaining `RemoveWorktree` would then recurse through a still-live
/// reparse point and gut the canonical tree. Any other step failure is
/// logged and the plan continues (each step is idempotent, so a retry —
/// the next poll tick, or the operator pressing the button again — is
/// safe).
///
/// Returns `Ok(())` when every step succeeded, `Err(msg)` describing the
/// first failure otherwise. Shared by the background poller
/// ([`execute_pull`]) and the on-demand endpoint
/// ([`super::on_demand::reclaim_now`]) so there is exactly ONE place a
/// worktree removal can be carried out.
pub(super) fn execute_steps(worktree_path: &str, steps: &[ReclaimStep]) -> Result<(), String> {
    let mut first_err: Option<String> = None;
    for step in steps {
        if let Err(e) = execute_step(step) {
            warn!("worktree_reclaim: step {step:?} failed: {e}");
            if first_err.is_none() {
                first_err = Some(e);
            }
            if matches!(step, ReclaimStep::UnlinkJunction(_)) {
                warn!(
                    "worktree_reclaim: aborting remaining steps for {worktree_path} — a junction \
                     unlink failed, recursive removal would be unsafe (INV-W4)"
                );
                break;
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Phase 3 — registration hygiene (`git worktree prune`).
//
// `git worktree remove` prunes its own registration, but the `remove_dir_all`
// fallback and every out-of-band deletion (operator cleanup, another machine,
// the Phase 4 backstop below) leave a stale entry in the parent repo's
// `.git/worktrees/` forever — 1,064 had piled up in qontinui-coord by
// 2026-07-19 because nothing ever ran `git worktree prune`. Two surfaces fix
// that: a best-effort prune after every successful reclaim removal, and a
// low-frequency (daily) prune pass over every canonical checkout.
//
// Prune is registration-only: `git worktree prune` deletes ONLY
// `.git/worktrees/<id>` entries whose worktree dir is gone — it never
// creates or deletes a working directory, so it can never resurrect a husk
// (the husk-guard invariant: reclaim never creates filesystem paths).
// ---------------------------------------------------------------------------

/// Parse a seconds value from a raw env string; `None` / unparseable →
/// `default`. Pure (env reading lifted to callers) so tests never mutate
/// the process-global environment.
fn parse_secs_env(raw: Option<&str>, default: u64) -> u64 {
    raw.and_then(|s| s.trim().parse().ok()).unwrap_or(default)
}

/// Cadence of the daily local prune + backstop maintenance pass.
fn prune_interval_secs() -> u64 {
    parse_secs_env(
        std::env::var("QONTINUI_WORKTREE_PRUNE_INTERVAL_SECS")
            .ok()
            .as_deref(),
        DEFAULT_PRUNE_INTERVAL_SECS,
    )
}

/// Age ceiling for the Phase 4 backstop sweep.
fn backstop_max_age_secs() -> u64 {
    parse_secs_env(
        std::env::var("QONTINUI_WORKTREE_BACKSTOP_MAX_AGE_SECS")
            .ok()
            .as_deref(),
        DEFAULT_BACKSTOP_MAX_AGE_SECS,
    )
}

/// Age floor for the empty-session-dir sweep.
fn empty_session_min_age_secs() -> u64 {
    parse_secs_env(
        std::env::var("QONTINUI_WORKTREE_EMPTY_SESSION_MIN_AGE_SECS")
            .ok()
            .as_deref(),
        DEFAULT_EMPTY_SESSION_MIN_AGE_SECS,
    )
}

/// Prune session dirs that are ALREADY empty when the maintenance pass runs.
///
/// [`prune_empty_session_parent`] closes the husk trap going FORWARD, but only
/// for a dir *this process* just emptied. Two husk classes it cannot reach:
///
///  1. **Pre-existing** — emptied by a build older than that prune, or while
///     the runner was down. Their `<repo>` child is already gone, so
///     `remove_worktree` will never run for them again and the forward-only
///     prune can never see them. 10 such husks were on the operator's box
///     2026-08-03 against 20 populated dirs.
///  2. **Allocated, never populated** — [`super::allocate_and_materialize_with_claim`] creates
///     `<session-uuid>/` *before* the locality fetch and `git worktree add`,
///     so a failure in either leaves the empty parent behind. 494 of these
///     were measured 2026-07-27 against a 6,666-dir root; no removal path is
///     ever invoked for them at all.
///
/// Both classes had exactly one sweeper — [`backstop_sweep`]'s hygiene step —
/// and that sweep returns early whenever coord has ever been live, which, now
/// that reclaim is armed in production, is always. So the husk backlog was
/// permanent in the steady state the fleet actually runs in. This pass runs
/// UNCONDITIONALLY instead.
///
/// Unconditional is safe here because, unlike the backstop, this destroys
/// nothing: `remove_dir` is non-recursive, so a dir holding ANY entry — a live
/// worktree, a half-finished checkout, a stray file — fails harmlessly and is
/// left alone. That also makes it inherently race-safe against a concurrent
/// allocation rather than race-checked. No dirty guard is needed for the same
/// reason: an empty dir has no work to lose. Two guards remain:
///
///  1. **UUID-named only** ([`is_session_uuid_dir`]) — the shared scope test.
///  2. **An age floor** — allocation creates the parent and only then runs a
///     network fetch, so a legitimately empty session dir exists for as long
///     as that fetch takes. An unreadable mtime is never old enough
///     (conservative, matching [`dir_age_secs`]'s contract).
fn prune_empty_session_dirs() {
    let Some(root) = session_worktree_root() else {
        debug!("worktree_husk_prune: no session-worktree root resolved — skipping");
        return;
    };
    let pruned = prune_empty_session_dirs_in(&root, empty_session_min_age_secs());
    if pruned > 0 {
        info!(
            "worktree_husk_prune: pruned {pruned} empty session dir(s) under {}",
            root.display()
        );
    }
}

/// Pure-ish core of [`prune_empty_session_dirs`]: `root` and the age floor are
/// resolved by the caller so tests can drive a tempdir without mutating the
/// process-global environment (which would be flaky under parallel tests —
/// the same split [`super::canonical_paths`] uses). Returns the count pruned.
fn prune_empty_session_dirs_in(root: &Path, min_age_secs: u64) -> usize {
    let entries = match std::fs::read_dir(root) {
        Ok(r) => r,
        Err(_) => return 0, // root absent → nothing leaked
    };
    let mut pruned = 0usize;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || !is_session_uuid_dir(&dir) {
            continue;
        }
        // Unmeasurable age is never old enough.
        let Some(age) = dir_age_secs(&dir) else {
            continue;
        };
        if age < min_age_secs {
            continue;
        }
        // Non-recursive: succeeds ONLY if the dir is genuinely empty, so a
        // populated session is never touched.
        match std::fs::remove_dir(&dir) {
            Ok(()) => {
                debug!(
                    "worktree_husk_prune: pruned empty session dir {}",
                    dir.display()
                );
                pruned += 1;
            }
            Err(e) => debug!(
                "worktree_husk_prune: {} not pruned ({e}) — expected when populated",
                dir.display()
            ),
        }
    }
    pruned
}

/// The exact git argv (after the `git` binary) for a registration prune of
/// `repo_root`. Pure — unit tests pin the invocation shape.
fn prune_command_args(repo_root: &Path) -> Vec<String> {
    vec![
        "-C".to_string(),
        repo_root.to_string_lossy().to_string(),
        "worktree".to_string(),
        "prune".to_string(),
    ]
}

/// Best-effort `git worktree prune` in a parent repo's canonical checkout.
/// NEVER fails the caller: a missing dir / non-repo / git failure logs a
/// warning and returns. Creates nothing, deletes no working directory —
/// registration metadata only.
pub(super) fn prune_parent_repo(repo_root: &Path) {
    if !repo_root.is_dir() {
        debug!(
            "worktree_reclaim: prune skipped — {} is not a directory",
            repo_root.display()
        );
        return;
    }
    let args = prune_command_args(repo_root);
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(&args);
    match run_probe(
        cmd,
        RECLAIM_CMD_TIMEOUT,
        "worktree_reclaim: git worktree prune",
    ) {
        ProbeOutcome::Captured(_) => {
            debug!(
                "worktree_reclaim: git worktree prune ok in {}",
                repo_root.display()
            );
        }
        // One arm for all three failure modes (non-zero exit, spawn error,
        // timeout) — `run_probe` already WARNed the details, and prune is
        // best-effort either way.
        ProbeOutcome::Degraded(reason) => {
            warn!(
                "worktree_reclaim: git worktree prune in {} failed: {reason:?}",
                repo_root.display(),
            );
        }
    }
}

/// Enumerate every canonical repo checkout under the workspace root —
/// reuses the census's discovery contract exactly (`qontinui_root()` for the
/// root, [`super::census::is_canonical_repo_root`] for the repo-root test) so
/// there is no second repo-discovery notion to drift from.
fn enumerate_canonical_checkouts() -> Vec<PathBuf> {
    let Some(root) = super::census::qontinui_root() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && super::census::is_canonical_repo_root(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// The daily prune pass: `git worktree prune` in every canonical checkout.
/// Covers registrations orphaned by manual deletions that never went
/// through [`remove_worktree`].
fn prune_all_canonical_checkouts() {
    let repos = enumerate_canonical_checkouts();
    if repos.is_empty() {
        debug!("worktree_reclaim: prune pass — no canonical checkouts resolved");
        return;
    }
    info!(
        "worktree_reclaim: daily prune pass over {} canonical checkout(s)",
        repos.len()
    );
    for repo in repos {
        prune_parent_repo(&repo);
    }
}

// ---------------------------------------------------------------------------
// Phase 4 — coord-absent local backstop sweep (LAST RESORT).
//
// 2,873 leaked session-worktree dirs (~60 GB) filled the disk on 2026-07-19:
// coord's reclaim was unarmed + blind to the `<root>/<session-uuid>/<repo>`
// path shape, the leaked dirs had no coord ledger rows, and the coord census
// has a 24 h retention — so nothing anywhere would EVER delete them. This
// sweep is the machine-local defense-in-depth for exactly that state: it
// deletes a session-worktree dir only when BOTH
//   (a) its mtime age exceeds a generous ceiling (default 14 days), AND
//   (b) coord has been unreachable-or-unarmed for this poller's ENTIRE
//       session (a monotonic boolean that flips permanently to "coord was
//       live" on the first successful pull with any arming flag on).
// (b) guarantees the backstop can never race a coord-issued instruction:
// the moment coord's lifecycle is live, the backstop is suppressed for the
// rest of the process lifetime. Dirty trees are never touched, and deletion
// goes through the same INV-W4 plan machinery (junction-unlink-first via
// [`execute_steps`]) as coord-issued removals.
// ---------------------------------------------------------------------------

/// Pure eligibility for one backstop deletion. `true` only when the dir's
/// age STRICTLY exceeds the ceiling, coord was never seen live+armed this
/// session, and the tree is clean. This is the function the unit tests pin.
pub(super) fn backstop_eligible(
    age_secs: u64,
    ceiling_secs: u64,
    coord_ever_live: bool,
    is_dirty: bool,
) -> bool {
    !coord_ever_live && !is_dirty && age_secs > ceiling_secs
}

/// Tri-state dirtiness verdict for the backstop.
///
/// Historically this was the STRICTER of the two paths, because
/// [`worktree_is_dirty`] mapped every failure to "not dirty". It no longer is:
/// both now run on [`super::dirty::probe_reclaim_dirty`], so a degraded probe
/// refuses a delete on the coord-instructed path too. This enum remains
/// because the backstop's caller wants to LOG the three cases apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackstopDirty {
    /// `git status --porcelain` succeeded and reported no real work (empty,
    /// or only the runner's own untracked scaffolding — see
    /// [`super::dirty`]), OR the dir has no `.git` at all (plain leaked dir —
    /// nothing to be dirty).
    Clean,
    /// `git status --porcelain` succeeded and reported real work — WIP, never
    /// delete.
    Dirty,
    /// A `.git` file/dir is present but `git status` failed (corrupt or
    /// unreadable repo) — we can't prove it's clean, so skip and log.
    CorruptSkip,
}

/// Execution-time dirtiness check for one backstop candidate.
///
/// Now expressed on the SHARED tri-state probe
/// ([`super::dirty::probe_reclaim_dirty`]) rather than its own copy of the
/// match arms, so this path and [`worktree_is_dirty`] cannot drift: the
/// reclaim-scoped predicate, the `.git`-absent carve-out and the
/// "degrade is never clean" rule are decided in exactly one place.
fn backstop_dirty_verdict(path: &Path) -> BackstopDirty {
    match super::dirty::probe_reclaim_dirty(
        path,
        RECLAIM_CMD_TIMEOUT,
        "worktree_reclaim: backstop git status",
    ) {
        super::dirty::DirtyVerdict::Clean => BackstopDirty::Clean,
        super::dirty::DirtyVerdict::Dirty => BackstopDirty::Dirty,
        // A `.git` is present but the probe did not answer (timeout, spawn
        // failure, corrupt repo) — we cannot prove it is clean, so skip.
        super::dirty::DirtyVerdict::Unknown => BackstopDirty::CorruptSkip,
    }
}

/// mtime age of `path` in whole seconds. `None` when the metadata / mtime
/// is unreadable or the mtime is in the future (clock skew) — callers skip
/// (conservative: an unmeasurable age is never "old enough").
fn dir_age_secs(path: &Path) -> Option<u64> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    mtime.elapsed().ok().map(|d| d.as_secs())
}

/// The session-worktree root (`<workspace-parent>/qontinui-worktrees`, or
/// the env override) under which `<session-uuid>/<repo>` dirs materialize.
/// Resolved through [`super::canonical_paths::agent_worktree_root`] so the
/// sweep honors the same `QONTINUI_WORKTREE_ROOT` / `COORD_WORKTREE_ROOT`
/// override as materialization; only the canonical's PARENT feeds the
/// default, so any direct child of the workspace root anchors it.
fn session_worktree_root() -> Option<PathBuf> {
    let root = super::census::qontinui_root()?;
    Some(super::canonical_paths::agent_worktree_root(
        &root.join("qontinui-runner"),
    ))
}

/// One backstop sweep over `<session_root>/<session-uuid>/<repo>` dirs.
/// `coord_ever_live` is the poller's monotonic liveness boolean — `true`
/// suppresses the entire sweep (last-resort contract). Best-effort: every
/// failure logs and moves on; the sweep never propagates an error.
fn backstop_sweep(coord_ever_live: bool) {
    if coord_ever_live {
        debug!("worktree_backstop: coord seen live+armed this session — sweep suppressed");
        return;
    }
    let ceiling = backstop_max_age_secs();
    let Some(root) = session_worktree_root() else {
        debug!("worktree_backstop: no session-worktree root resolved — skipping");
        return;
    };
    let sessions = match std::fs::read_dir(&root) {
        Ok(r) => r,
        Err(_) => return, // root absent → nothing leaked
    };
    for session in sessions.flatten() {
        let session_dir = session.path();
        if !session_dir.is_dir() {
            continue;
        }
        // Only session-UUID dirs are governed — anything else under the
        // root (operator scratch, unrelated tooling) is out of scope.
        if !is_session_uuid_dir(&session_dir) {
            debug!(
                "worktree_backstop: {} is not a session-uuid dir — out of scope",
                session_dir.display()
            );
            continue;
        }
        let repos = match std::fs::read_dir(&session_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for repo_entry in repos.flatten() {
            let repo_dir = repo_entry.path();
            if !repo_dir.is_dir() {
                continue;
            }
            backstop_consider_one(&repo_dir, ceiling, coord_ever_live);
        }
        // Hygiene: a session dir left empty by the sweep (or by earlier
        // partial cleanup) is removed non-recursively — fails harmlessly
        // when anything remains inside.
        let _ = std::fs::remove_dir(&session_dir);
    }
}

/// Evaluate + (maybe) delete ONE session-worktree repo dir. Split out of
/// [`backstop_sweep`] so the per-dir guard ordering reads linearly.
fn backstop_consider_one(repo_dir: &Path, ceiling: u64, coord_ever_live: bool) {
    // Age first — unmeasurable age is never old enough.
    let Some(age) = dir_age_secs(repo_dir) else {
        debug!(
            "worktree_backstop: {} mtime unreadable — skipping",
            repo_dir.display()
        );
        return;
    };
    // Dirtiness (WIP is sacred; corrupt-but-present .git is skipped).
    let dirty = match backstop_dirty_verdict(repo_dir) {
        BackstopDirty::Clean => false,
        BackstopDirty::Dirty => true,
        BackstopDirty::CorruptSkip => {
            warn!(
                "worktree_backstop: {} has a .git but `git status` failed (corrupt?) — skipping",
                repo_dir.display()
            );
            return;
        }
    };
    if !backstop_eligible(age, ceiling, coord_ever_live, dirty) {
        return;
    }
    // G6 — never touch a tree that is currently building (belt-and-braces:
    // a >14-day-old mtime makes this near-impossible, but the probe is
    // cheap and the reclaim executor's contract is uniform).
    if probe_building(repo_dir) {
        info!(
            "worktree_backstop: {} building — skipping this pass",
            repo_dir.display()
        );
        return;
    }
    // INV-W4: unlink every top-level junction (plus the Tauri-layout
    // `src-tauri/target`) BEFORE the recursive removal, via the same step
    // machinery as coord-issued removals ([`execute_steps`] aborts the plan
    // on a failed unlink so the removal can never recurse through a live
    // reparse point).
    let mut steps: Vec<ReclaimStep> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(repo_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if is_junction(&p) {
                steps.push(ReclaimStep::UnlinkJunction(p));
            }
        }
    }
    let st_target = repo_dir.join("src-tauri").join("target");
    if is_junction(&st_target) {
        steps.push(ReclaimStep::UnlinkJunction(st_target));
    }
    steps.push(ReclaimStep::RemoveWorktree(repo_dir.to_path_buf()));

    let wt_str = repo_dir.to_string_lossy().to_string();
    match execute_steps(&wt_str, &steps) {
        Ok(()) => {
            warn!(
                "worktree_backstop: DELETED {} (age={}d > ceiling {}d; coord absent/unarmed \
                 for entire poller session — last-resort local backstop)",
                repo_dir.display(),
                age / 86_400,
                ceiling / 86_400
            );
            // Phase 3 hygiene: prune the parent repo's registration.
            if let Some(repo_name) = repo_dir.file_name().and_then(|n| n.to_str()) {
                if let Ok(canonical) = super::canonical_paths::default_canonical_path(repo_name) {
                    prune_parent_repo(&canonical);
                }
            }
        }
        Err(e) => {
            warn!(
                "worktree_backstop: failed to delete {}: {e}",
                repo_dir.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Pull + tick + spawn (mirrors census.rs identity/base resolution).
// ---------------------------------------------------------------------------

/// Fetch this device's pending reclaim decision from coord.
///
/// `Ok(None)` = cleanly not applicable (no `device_id` in
/// `~/.qontinui/machine.json`, or no coord base configured) — the caller
/// skips without treating it as an error. `Err` = a real transport / non-2xx
/// failure.
///
/// This is coord's DECISION surface and the single source of reap
/// eligibility for BOTH triggers: the background poller ([`tick_once`]) and
/// the on-demand endpoint ([`super::on_demand`]). Note the instruction list
/// is computed by coord's G1–G5 gate **regardless of arming** — the
/// `remove_armed` / `rejunction_armed` booleans only tell the *silent
/// background* path whether it may act. The consented on-demand path reads
/// the same cleared set without needing the silent path armed.
pub(super) async fn fetch_pull() -> Result<Option<(Uuid, ReclaimPull)>, String> {
    let device_id = match super::census::load_device_id_pub() {
        Some(id) => id,
        None => {
            debug!("worktree_reclaim: no device_id — skipping");
            return Ok(None);
        }
    };
    let base = match qontinui_runner_lib::profiles::connected_coord_base() {
        Some(b) => b,
        None => {
            debug!("worktree_reclaim: no coord_url configured — skipping");
            return Ok(None);
        }
    };

    let url = format!(
        "{}/coord/worktree-reclaim/{}",
        base.trim_end_matches('/'),
        device_id
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("build reclaim http client: {}", error_chain(&e)))?;
    let resp = crate::coord_http::coord_get(&client, &url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {}", error_chain(&e)))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(200).collect();
        return Err(format!("coord returned {status} for GET {url}: {excerpt}"));
    }
    let pull: ReclaimPull = resp
        .json()
        .await
        .map_err(|e| format!("decode reclaim pull: {e}"))?;
    Ok(Some((device_id, pull)))
}

/// Render an error together with its FULL source chain.
///
/// `reqwest`'s `Display` prints only `error sending request for url (…)` and
/// leaves the thing that actually explains the failure — the OS error, the DNS
/// or TLS fault — reachable only via [`std::error::Error::source`]. A bare
/// `format!("{e}")` therefore produces a line that names a symptom and no
/// cause, which is not a diagnosable log entry.
///
/// This is not hypothetical. Between 2026-07-28 and 2026-08-01 the reclaim
/// poller failed 100 % of its pulls for five days and emitted ~140 identical
/// WARN lines per day, every one of them cause-free; the box reached coord
/// fine over the same URL throughout, and the on-demand path calling THIS
/// function succeeded the whole time. The outage was undiagnosable from the
/// logs by construction.
fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    use std::fmt::Write as _;
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let _ = write!(out, ": {cause}");
        source = cause.source();
    }
    out
}

/// What one reclaim cycle actually did.
///
/// This used to be a bare `bool` meaning `live_armed`, which made a SUCCESSFUL
/// pull with arming off (the default posture) indistinguishable from "no
/// device_id configured — did nothing". Any health signal built on that bool
/// would report a perfectly healthy unarmed poller as never having succeeded.
/// The two cases are different facts, so they get different variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// No device_id / no coord_url — nothing was attempted. Not a success and
    /// not a failure.
    Skipped,
    /// coord answered. `live_armed` is true iff at least one arming flag was
    /// on — the signal that flips the poller's monotonic `coord_ever_live` and
    /// permanently suppresses the Phase 4 backstop.
    Pulled { live_armed: bool },
}

/// One reclaim cycle: pull + execute. `Err` only on a transport / non-2xx
/// failure.
pub async fn tick_once() -> Result<TickOutcome, String> {
    let Some((_device_id, pull)) = fetch_pull().await? else {
        return Ok(TickOutcome::Skipped);
    };
    let live_armed = pull.rejunction_armed || pull.remove_armed;

    // execute_pull runs synchronous git/cmd subprocesses and filesystem
    // removals — potentially long (junction unlinks + worktree deletes).
    // Run it on the blocking pool so the shared fleet-publishers runtime's
    // async worker isn't pinned for the duration (the starvation class
    // PR #391 isolated the heartbeat from).
    spawn_blocking_tracked(move || execute_pull(&pull))
        .await
        .map_err(|e| format!("reclaim execution panicked: {e}"))?;
    Ok(TickOutcome::Pulled { live_armed })
}

/// Spawn the periodic reclaim poller. Interval from
/// `QONTINUI_WORKTREE_RECLAIM_INTERVAL_SECS` (default 300s, floored 30s).
/// `MissedTickBehavior::Skip` + warn-and-retry, like
/// [`super::census::spawn_census`].
///
/// Census-before-reclaim boot ordering (R3, the stale-census husk-guard):
/// the FIRST pull of each boot waits for the census publisher to land ≥1
/// successful POST, so coord decides from this boot's disk truth rather
/// than the previous boot's last census (the window where a worktree
/// deleted while the runner was down draws husk-creating instructions).
/// Bounded by one reclaim interval so a census-disabled config never
/// deadlocks the poller.
pub fn spawn_reclaim() {
    let secs: u64 = std::env::var("QONTINUI_WORKTREE_RECLAIM_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_RECLAIM_INTERVAL_SECS)
        .max(30);

    info!(
        "worktree_reclaim: starting periodic reclaim poller, interval={}s",
        secs
    );

    tokio::spawn(async move {
        if super::census::wait_first_census_posted(Duration::from_secs(secs)).await {
            debug!("worktree_reclaim: census posted this boot — reclaim pulls may begin");
        } else {
            warn!(
                "worktree_reclaim: no census POST within {secs}s — proceeding without the \
                 boot-ordering gate (census disabled or slow; coord's staleness degrade is \
                 the backstop)"
            );
        }
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Phase 4: monotonic coord-liveness. Flips to `true` on the FIRST
        // successful pull with any arming flag on, and never back — from
        // that moment the local backstop sweep is suppressed for the rest
        // of the process lifetime (coord's lifecycle owns reclaim).
        let mut coord_ever_live = false;
        // Phase 3/4: the daily maintenance pass (registration prune +
        // backstop sweep). The FIRST pass is deliberately deferred one full
        // interval after boot: a runner that boots during a coord outage
        // must not mass-delete on its first tick, and with a 14-day ceiling
        // a one-day deferral is immaterial.
        let mut last_maintenance = Instant::now();
        loop {
            tick.tick().await;
            match tick_once().await {
                Ok(TickOutcome::Pulled { live_armed }) => {
                    // coord answered: a success for health purposes whether or
                    // not anything was armed.
                    note_poll_success();
                    if live_armed && !coord_ever_live {
                        info!(
                            "worktree_reclaim: coord live+armed — local backstop permanently \
                             suppressed for this poller session"
                        );
                    }
                    coord_ever_live |= live_armed;
                }
                // Nothing attempted (unconfigured). Not a success — it must not
                // reset a failure streak — and not a failure either.
                Ok(TickOutcome::Skipped) => {}
                Err(e) => note_poll_failure(&e),
            }

            let interval = Duration::from_secs(prune_interval_secs());
            if last_maintenance.elapsed() >= interval {
                last_maintenance = Instant::now();
                let ever_live = coord_ever_live;
                // Blocking pool: the pass runs git subprocesses + dir walks.
                if let Err(e) = spawn_blocking_tracked(move || {
                    prune_all_canonical_checkouts();
                    // Unconditional (unlike the backstop below): removing an
                    // ALREADY-empty dir destroys nothing, and the backstop —
                    // the only other sweeper — is suppressed whenever coord
                    // is live, i.e. always in the armed steady state.
                    prune_empty_session_dirs();
                    backstop_sweep(ever_live);
                })
                .await
                {
                    warn!("worktree_reclaim: maintenance pass panicked: {e}");
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests — the INV-W4 ordering is the load-bearing assertion.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The health signal must separate "idle" from "dead". This is the single
    /// test that touches the poller-health statics; keep it that way, they are
    /// process-global.
    #[test]
    fn poller_health_distinguishes_dead_from_healthy() {
        // Fresh process state: never succeeded, no failures.
        let h = poller_health();
        assert_eq!(h.consecutive_failures, 0);
        assert_eq!(h.last_success_unix, None);

        for _ in 0..RECLAIM_FAILURE_ESCALATION_STREAK {
            note_poll_failure("GET https://coord…: error sending request: os error 10054");
        }
        let h = poller_health();
        assert_eq!(h.consecutive_failures, RECLAIM_FAILURE_ESCALATION_STREAK);
        assert!(
            h.last_error.as_deref().is_some_and(|e| e.contains("10054")),
            "the cause chain must survive into the health signal"
        );
        // Still zero successes — the state the 5-day outage was actually in,
        // and the one `coord_reachable: true` could not express.
        assert_eq!(h.last_success_unix, None);

        note_poll_success();
        let h = poller_health();
        assert_eq!(h.consecutive_failures, 0, "a success clears the streak");
        assert!(h.last_success_unix.is_some());
        assert_eq!(h.last_error, None);
    }

    /// `reqwest`'s Display names a symptom; the cause lives in `source()`.
    #[test]
    fn error_chain_includes_causes() {
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "os error 10054")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer(Inner);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error sending request for url")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let rendered = error_chain(&Outer(Inner));
        assert_eq!(rendered, "error sending request for url: os error 10054");
    }

    /// INV-W5. A dir whose `.git` is a DIRECTORY is a real clone: refuse, and
    /// prove the tree is still on disk afterwards. `git worktree remove` fails
    /// on a repo root, and before this guard the unconditional `remove_dir_all`
    /// fallback deleted the whole clone.
    #[test]
    fn remove_worktree_refuses_a_real_clone() {
        let dir = tempfile::tempdir().unwrap();
        let clone = dir.path().join("qontinui-coord-wt-prcreate-fix");
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        std::fs::write(clone.join("src.rs"), b"work").unwrap();

        let err = remove_worktree(&clone).expect_err("a real clone must not be removable");
        assert!(err.contains("INV-W5"), "unexpected error: {err}");
        assert!(clone.join(".git").is_dir(), "object store must survive");
        assert!(clone.join("src.rs").exists(), "worktree files must survive");
    }

    /// The pre-fix predicate, reproduced verbatim so every case below can be
    /// shown to have gone the OTHER way under it. `true` here meant "a clone —
    /// refuse"; `false` meant "not a clone — fall through to `remove_dir_all`".
    fn legacy_is_dir_clone_root(path: &Path) -> bool {
        path.join(".git").is_dir()
    }

    /// INV-W5, the case the tri-state exists for. A REAL CLONE whose `.git` is
    /// a symlink to a gitdir that is gone (volume unmounted, store moved).
    ///
    /// NEUTER CHECK: `is_dir()` traverses, the target is absent, so it says
    /// `false` = "not a clone" = delete — and `remove_dir_all` then succeeds,
    /// because the *files* were always readable. The whole working tree goes.
    #[cfg(unix)]
    #[test]
    fn remove_worktree_refuses_a_dangling_git_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let clone = dir.path().join("qontinui-coord");
        std::fs::create_dir_all(&clone).unwrap();
        std::os::unix::fs::symlink("/nonexistent/moved-gitdir", clone.join(".git")).unwrap();
        std::fs::write(clone.join("src.rs"), b"work").unwrap();

        assert!(
            !legacy_is_dir_clone_root(&clone),
            "pre-fix predicate said NOT-a-clone, i.e. it authorized the delete"
        );
        assert_eq!(
            super::super::census::probe_clone_rootness(&clone),
            CloneRootness::Undetermined
        );

        let err = remove_worktree(&clone).expect_err("an unmeasurable path must not be removed");
        assert!(err.contains("INV-W5"), "unexpected error: {err}");
        assert!(clone.join("src.rs").exists(), "worktree files must survive");
    }

    /// A `.git` symlink that resolves to a real object store is refused for the
    /// same reason: we do not traverse, so it is `Undetermined`, not
    /// `CloneRoot`. Outcome-identical to the old predicate here (which said
    /// "clone" and refused) — recorded so the change of REASON is deliberate
    /// rather than accidental.
    #[cfg(unix)]
    #[test]
    fn remove_worktree_refuses_a_git_symlink_to_a_live_object_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("elsewhere.git");
        std::fs::create_dir_all(&store).unwrap();
        let clone = dir.path().join("qontinui-coord");
        std::fs::create_dir_all(&clone).unwrap();
        std::os::unix::fs::symlink(&store, clone.join(".git")).unwrap();

        assert!(legacy_is_dir_clone_root(&clone));
        assert_eq!(
            super::super::census::probe_clone_rootness(&clone),
            CloneRootness::Undetermined
        );
        let err = remove_worktree(&clone).expect_err("refuse");
        assert!(err.contains("INV-W5"), "unexpected error: {err}");
        assert!(clone.exists());
    }

    /// The `.git` lookup fails with `EACCES` because the candidate directory
    /// itself is mode 000.
    ///
    /// NEUTER CHECK: `is_dir()` maps EACCES to `false` exactly as it maps
    /// ENOENT — indistinguishable — so the old guard fell through to
    /// `remove_dir_all` on a directory it had never managed to look inside.
    #[cfg(unix)]
    #[test]
    fn remove_worktree_refuses_an_unreadable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("qontinui-coord-wt-x");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /x/.git/worktrees/y").unwrap();
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o000)).unwrap();

        let rootness = super::super::census::probe_clone_rootness(&wt);
        let legacy = legacy_is_dir_clone_root(&wt);
        let verdict = remove_worktree(&wt);
        // Restore BEFORE asserting so a failure still lets the tempdir clean up.
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o755)).unwrap();

        if rootness != CloneRootness::Undetermined {
            // root / CAP_DAC_OVERRIDE ignores mode bits: nothing to assert.
            eprintln!("skipping: this process can read a mode-000 directory (root?)");
            return;
        }
        assert!(
            !legacy,
            "precondition: is_dir() maps EACCES to false, exactly like ENOENT"
        );
        let err = verdict.expect_err("an unreadable candidate must not be removed");
        assert!(err.contains("INV-W5"), "unexpected error: {err}");
        assert!(wt.exists(), "the tree must survive");
    }

    /// `ENOTDIR`: the candidate is a regular FILE, so `<path>/.git` cannot even
    /// be looked up.
    ///
    /// NEUTER CHECK: `is_dir()` folds ENOTDIR into `false` too, so the old
    /// guard passed it to `remove_dir_all` — which fails with a raw io error
    /// rather than the named invariant.
    #[test]
    fn remove_worktree_refuses_a_path_that_is_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("qontinui-coord-wt-file");
        std::fs::write(&f, b"not a directory").unwrap();

        assert!(!legacy_is_dir_clone_root(&f));
        assert_eq!(
            super::super::census::probe_clone_rootness(&f),
            CloneRootness::Undetermined
        );
        let err = remove_worktree(&f).expect_err("refuse");
        assert!(err.contains("INV-W5"), "unexpected error: {err}");
        assert!(f.exists());
    }

    /// The guard must NOT become a blanket refusal. A directory MEASURED to
    /// have no `.git` at all is a plain dir, not a repository, and the backstop
    /// sweep's whole job is removing those.
    #[test]
    fn remove_worktree_still_removes_a_genuinely_non_clone_directory() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("qontinui-coord-wt-husk");
        std::fs::create_dir_all(plain.join("nested")).unwrap();
        std::fs::write(plain.join("nested").join("a.txt"), b"junk").unwrap();

        assert_eq!(
            super::super::census::probe_clone_rootness(&plain),
            CloneRootness::NotCloneRoot
        );
        remove_worktree(&plain).expect("a plain directory is still removable");
        assert!(!plain.exists());
    }

    /// The guard must not block the case reclaim actually exists for: a linked
    /// worktree, whose `.git` is a FILE.
    #[test]
    fn remove_worktree_still_removes_a_linked_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("qontinui-coord-wt-real");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /x/.git/worktrees/y").unwrap();

        // `git worktree remove` fails here (not a registered worktree); the
        // `remove_dir_all` fallback is what completes it — exactly the path
        // INV-W5 must leave intact.
        remove_worktree(&wt).expect("a linked worktree is still removable");
        assert!(!wt.exists());
    }

    /// Build `<root>/<session-uuid>/<repo>` with `.git` as a FILE (a linked
    /// worktree, the shape `remove_worktree` accepts).
    fn session_worktree(root: &Path, session: &str, repo: &str) -> PathBuf {
        let wt = root.join(session).join(repo);
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /x/.git/worktrees/y").unwrap();
        wt
    }

    /// The husk regression: reclaiming the only repo under a session dir must
    /// leave NO empty `<session-uuid>` dir behind. Before this, husks grew one
    /// per successful reclaim (1,554 observed vs 28 populated, 2026-08-03).
    #[test]
    fn removing_the_last_repo_prunes_the_empty_session_dir() {
        let dir = tempfile::tempdir().unwrap();
        let session = "019fbb21-e55d-7ef2-bbc1-5c371871e41b";
        let wt = session_worktree(dir.path(), session, "qontinui-coord");

        remove_worktree(&wt).expect("removable");

        assert!(!wt.exists(), "repo dir removed");
        assert!(
            !dir.path().join(session).exists(),
            "empty session dir must be pruned, not left as a husk"
        );
    }

    /// Race safety: a sibling repo worktree under the same session must never
    /// be destroyed. `remove_dir` is non-recursive, so a non-empty parent
    /// simply fails and is left alone.
    #[test]
    fn session_dir_with_a_surviving_sibling_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let session = "019fbb22-ee8c-7193-9fb5-dfe211e55579";
        let a = session_worktree(dir.path(), session, "qontinui-coord");
        let b = session_worktree(dir.path(), session, "qontinui-runner");

        remove_worktree(&a).expect("removable");

        assert!(!a.exists(), "target removed");
        assert!(b.exists(), "sibling worktree MUST survive");
        assert!(
            dir.path().join(session).exists(),
            "session dir with a surviving sibling must not be pruned"
        );
    }

    /// Scope guard: only UUID-named parents are governed, so the prune can
    /// never walk up into the worktree root, `qontinui-root`, or an operator
    /// scratch dir — no matter what path a caller passes.
    #[test]
    fn non_uuid_parent_is_never_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("qontinui-root");
        let wt = parent.join("qr-wt-p2b");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /x/.git/worktrees/y").unwrap();

        remove_worktree(&wt).expect("removable");

        assert!(!wt.exists(), "worktree removed");
        assert!(
            parent.exists(),
            "a non-UUID parent must never be pruned even when empty"
        );
    }

    /// Idempotent + harmless on the paths that are not session-shaped at all.
    #[test]
    fn prune_empty_session_parent_is_safe_on_odd_inputs() {
        let dir = tempfile::tempdir().unwrap();
        // Already-absent parent: no panic, no error.
        prune_empty_session_parent(&dir.path().join("019fbb26-3b3b-7361-bd98-f11c7d4131e0/gone"));
        // A path with no parent at all.
        prune_empty_session_parent(Path::new("x"));
        // Root-ish path.
        prune_empty_session_parent(Path::new("/"));
    }

    // -----------------------------------------------------------------------
    // The BACKLOG sweep — husks that already exist when the pass runs, which
    // the forward-only `prune_empty_session_parent` can never reach.
    // -----------------------------------------------------------------------

    /// The gap `prune_empty_session_parent` leaves: a husk whose `<repo>`
    /// child is ALREADY gone (emptied by an older build, by a removal that
    /// happened while the runner was down, or by an allocation that created
    /// the parent and never populated it). `remove_worktree` never runs for
    /// these again, and the only other sweeper — `backstop_sweep` — is
    /// suppressed whenever coord is live. So this pass must reach them.
    #[test]
    fn already_empty_session_dirs_are_pruned() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("019fbb27-1111-7111-8111-111111111111");
        let b = root.path().join("019fbb28-2222-7222-8222-222222222222");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        // floor 0 — every dir is old enough.
        assert_eq!(prune_empty_session_dirs_in(root.path(), 0), 2);
        assert!(!a.exists(), "husk pruned");
        assert!(!b.exists(), "husk pruned");
    }

    /// A populated session dir must survive untouched. `remove_dir` is
    /// non-recursive, so this holds no matter what the dir contains — the
    /// guard is the syscall itself, not a check that could go stale between
    /// look and leap.
    #[test]
    fn populated_session_dir_survives_the_sweep() {
        let root = tempfile::tempdir().unwrap();
        let live = session_worktree(
            root.path(),
            "019fbb29-3333-7333-8333-333333333333",
            "qontinui-runner",
        );
        let husk = root.path().join("019fbb2a-4444-7444-8444-444444444444");
        std::fs::create_dir_all(&husk).unwrap();

        assert_eq!(prune_empty_session_dirs_in(root.path(), 0), 1);
        assert!(live.exists(), "a live worktree MUST survive");
        assert!(!husk.exists(), "the husk beside it is still pruned");
    }

    /// Scope guard: a non-UUID child of the root is never touched, even when
    /// empty — operator scratch dirs live beside the session dirs.
    #[test]
    fn non_uuid_dirs_are_never_swept() {
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().join("qr-wt-p2b");
        std::fs::create_dir_all(&scratch).unwrap();

        assert_eq!(prune_empty_session_dirs_in(root.path(), 0), 0);
        assert!(scratch.exists(), "a non-UUID dir must never be swept");
    }

    /// Race guard: allocation creates `<session-uuid>/` and only THEN runs a
    /// network fetch + `git worktree add`, so a legitimately empty session dir
    /// exists for as long as that takes. The age floor keeps the sweep off it.
    #[test]
    fn freshly_allocated_empty_session_dir_is_below_the_age_floor() {
        let root = tempfile::tempdir().unwrap();
        let allocating = root.path().join("019fbb2b-5555-7555-8555-555555555555");
        std::fs::create_dir_all(&allocating).unwrap();

        // Just created, so far below the 1h default floor.
        assert_eq!(
            prune_empty_session_dirs_in(root.path(), DEFAULT_EMPTY_SESSION_MIN_AGE_SECS),
            0
        );
        assert!(
            allocating.exists(),
            "an in-flight allocation must not be swept out from under itself"
        );
    }

    /// An absent root is "nothing leaked", not an error.
    #[test]
    fn sweep_on_an_absent_root_is_a_no_op() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(prune_empty_session_dirs_in(&root.path().join("gone"), 0), 0);
    }

    fn instr(action: ReclaimAction, junctioned: &[&str], is_dirty: bool) -> ReclaimInstruction {
        ReclaimInstruction {
            worktree_path: "D:/qontinui-root/qontinui-runner-wt-foo".to_string(),
            repo: "qontinui-runner".to_string(),
            action,
            reason: "worktree:orphan".to_string(),
            is_dirty,
            junctioned_paths: junctioned.iter().map(|s| s.to_string()).collect(),
            retention: None,
        }
    }

    #[test]
    fn remove_unlinks_every_junction_before_removing_worktree() {
        let i = instr(ReclaimAction::Remove, &["target", "node_modules"], false);
        // remove_armed=true.
        let steps = plan_reclaim(&i, false, true, None, true);

        // Exactly: UnlinkJunction(target), UnlinkJunction(node_modules),
        // RemoveWorktree(path) — junctions first, in order.
        assert_eq!(
            steps,
            vec![
                ReclaimStep::UnlinkJunction(PathBuf::from(
                    "D:/qontinui-root/qontinui-runner-wt-foo/target"
                )),
                ReclaimStep::UnlinkJunction(PathBuf::from(
                    "D:/qontinui-root/qontinui-runner-wt-foo/node_modules"
                )),
                ReclaimStep::RemoveWorktree(PathBuf::from(
                    "D:/qontinui-root/qontinui-runner-wt-foo"
                )),
            ]
        );
    }

    #[test]
    fn every_unlink_precedes_the_removal() {
        // Stronger property: regardless of how many junctions, the
        // RemoveWorktree index is strictly after every UnlinkJunction.
        let i = instr(
            ReclaimAction::Remove,
            &["a", "b", "c", "node_modules", "target"],
            false,
        );
        let steps = plan_reclaim(&i, false, true, None, true);
        let remove_idx = steps
            .iter()
            .position(|s| matches!(s, ReclaimStep::RemoveWorktree(_)))
            .expect("must contain a RemoveWorktree step");
        let last_unlink_idx = steps
            .iter()
            .rposition(|s| matches!(s, ReclaimStep::UnlinkJunction(_)))
            .expect("must contain UnlinkJunction steps");
        assert!(
            last_unlink_idx < remove_idx,
            "INV-W4: every junction unlink must precede the worktree removal"
        );
        // Exactly one removal, and it's last.
        assert_eq!(remove_idx, steps.len() - 1);
    }

    #[test]
    fn remove_with_no_junctions_is_just_a_removal() {
        let i = instr(ReclaimAction::Remove, &[], false);
        let steps = plan_reclaim(&i, false, true, None, true);
        assert_eq!(
            steps,
            vec![ReclaimStep::RemoveWorktree(PathBuf::from(
                "D:/qontinui-root/qontinui-runner-wt-foo"
            ))]
        );
    }

    #[test]
    fn dirty_instruction_yields_no_destructive_steps() {
        let i = instr(ReclaimAction::Remove, &["target", "node_modules"], true);
        // Even fully armed, a dirty worktree yields only a Skip.
        let steps = plan_reclaim(&i, true, true, None, true);
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], ReclaimStep::Skip(_)));
        // No unlink / remove / create anywhere.
        assert!(!steps.iter().any(|s| matches!(
            s,
            ReclaimStep::UnlinkJunction(_)
                | ReclaimStep::RemoveWorktree(_)
                | ReclaimStep::CreateJunction { .. }
        )));
    }

    #[test]
    fn unarmed_remove_yields_no_destructive_steps() {
        // remove_armed=false → advisory-only Skip even though it's clean.
        let i = instr(ReclaimAction::Remove, &["target", "node_modules"], false);
        let steps = plan_reclaim(
            &i, /* rejunction */ false, /* remove */ false, None, true,
        );
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], ReclaimStep::Skip(_)));
        assert!(!steps.iter().any(|s| matches!(
            s,
            ReclaimStep::UnlinkJunction(_)
                | ReclaimStep::RemoveWorktree(_)
                | ReclaimStep::CreateJunction { .. }
        )));
    }

    #[test]
    fn unarmed_rejunction_skips_even_with_canonical() {
        // rejunction_armed=false → advisory-only Skip.
        let i = instr(ReclaimAction::Rejunction, &["target"], false);
        let canonical = PathBuf::from("D:/qontinui-root/qontinui-runner");
        let steps = plan_reclaim(&i, false, false, Some(&canonical), true);
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], ReclaimStep::Skip(_)));
    }

    #[test]
    fn rejunction_armed_but_remove_unarmed_skips_removes() {
        // The graduated state: rejunction default-on, remove still gated.
        // A Remove instruction is advisory-only...
        let rm = instr(ReclaimAction::Remove, &["target"], false);
        let steps = plan_reclaim(
            &rm, /* rejunction */ true, /* remove */ false, None, true,
        );
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], ReclaimStep::Skip(_)));
        assert!(!steps.iter().any(|s| matches!(
            s,
            ReclaimStep::UnlinkJunction(_) | ReclaimStep::RemoveWorktree(_)
        )));

        // ...while a Rejunction in the SAME tick actually executes.
        let rj = instr(ReclaimAction::Rejunction, &["target"], false);
        let canonical = PathBuf::from("D:/qontinui-root/qontinui-runner");
        let rj_steps = plan_reclaim(&rj, true, false, Some(&canonical), true);
        assert_eq!(
            rj_steps,
            vec![ReclaimStep::CreateJunction {
                link: PathBuf::from("D:/qontinui-root/qontinui-runner-wt-foo/target"),
                target: PathBuf::from("D:/qontinui-root/qontinui-runner/target"),
            }]
        );
    }

    #[test]
    fn both_unarmed_yields_nothing_destructive() {
        // Defaults-absent equivalent: neither action armed → only Skips,
        // nothing destructive, for either action kind.
        for action in [ReclaimAction::Remove, ReclaimAction::Rejunction] {
            let i = instr(action, &["target", "node_modules"], false);
            let canonical = PathBuf::from("D:/qontinui-root/qontinui-runner");
            let steps = plan_reclaim(&i, false, false, Some(&canonical), true);
            assert_eq!(steps.len(), 1);
            assert!(matches!(steps[0], ReclaimStep::Skip(_)));
            assert!(!steps.iter().any(|s| matches!(
                s,
                ReclaimStep::UnlinkJunction(_)
                    | ReclaimStep::RemoveWorktree(_)
                    | ReclaimStep::CreateJunction { .. }
            )));
        }
    }

    #[test]
    fn rejunction_creates_junctions_to_canonical() {
        let i = instr(
            ReclaimAction::Rejunction,
            &["target", "node_modules"],
            false,
        );
        let canonical = PathBuf::from("D:/qontinui-root/qontinui-runner");
        // rejunction_armed=true.
        let steps = plan_reclaim(&i, true, false, Some(&canonical), true);
        assert_eq!(
            steps,
            vec![
                ReclaimStep::CreateJunction {
                    link: PathBuf::from("D:/qontinui-root/qontinui-runner-wt-foo/target"),
                    target: PathBuf::from("D:/qontinui-root/qontinui-runner/target"),
                },
                ReclaimStep::CreateJunction {
                    link: PathBuf::from("D:/qontinui-root/qontinui-runner-wt-foo/node_modules"),
                    target: PathBuf::from("D:/qontinui-root/qontinui-runner/node_modules"),
                },
            ]
        );
    }

    #[test]
    fn rejunction_without_canonical_path_skips() {
        let i = instr(ReclaimAction::Rejunction, &["target"], false);
        // Armed, but no canonical path → degrades to Skip.
        let steps = plan_reclaim(&i, true, false, None, true);
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], ReclaimStep::Skip(_)));
    }

    #[test]
    fn unknown_action_is_a_skip_not_a_destructive_step() {
        let i = instr(
            ReclaimAction::Unknown("nuke".to_string()),
            &["target"],
            false,
        );
        // Even with both flags armed, an unknown action is a no-op Skip.
        let steps = plan_reclaim(&i, true, true, None, true);
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], ReclaimStep::Skip(_)));
    }

    #[test]
    fn absent_root_skips_entire_instruction_for_both_actions() {
        // The stale-census husk-guard (R1): a worktree missing on disk must
        // yield ONLY a Skip — for both actions, armed and unarmed — never a
        // step that could create a filesystem path.
        let canonical = PathBuf::from("D:/qontinui-root/qontinui-runner");
        for action in [ReclaimAction::Remove, ReclaimAction::Rejunction] {
            for (rj_armed, rm_armed) in [(false, false), (true, true)] {
                let i = instr(action.clone(), &["target", "node_modules"], false);
                let steps = plan_reclaim(
                    &i,
                    rj_armed,
                    rm_armed,
                    Some(&canonical),
                    /* root */ false,
                );
                assert_eq!(steps.len(), 1, "{action:?} armed=({rj_armed},{rm_armed})");
                assert!(
                    matches!(&steps[0], ReclaimStep::Skip(r) if r.contains("absent on disk")),
                    "absent root must be a Skip carrying the absent-on-disk reason"
                );
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn create_junction_refuses_to_create_missing_parent() {
        // R2: a rejunction whose link parent (the worktree root) is missing
        // must Err and create NOTHING — never re-materialize a husk.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("canonical-node_modules");
        std::fs::create_dir(&target).unwrap();
        let missing_parent = dir.path().join("gone-wt");
        let link = missing_parent.join("node_modules");

        let res = create_junction(&link, &target);
        assert!(res.is_err(), "missing parent must be an error");
        assert!(
            res.unwrap_err().contains("refusing to create directories"),
            "error must name the refusal"
        );
        assert!(
            !missing_parent.exists(),
            "the missing parent must NOT be created"
        );
    }

    #[test]
    fn unlink_junction_on_a_plain_dir_is_a_noop_not_a_delete() {
        // A real (non-reparse-point) dir must NOT be removed by
        // unlink_junction — this is the core INV-W4 guard.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("node_modules");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("keep.txt"), b"data").unwrap();

        // Not a junction → unlink is a no-op success and the dir survives.
        let res = unlink_junction(&real);
        assert!(res.is_ok());
        assert!(real.exists(), "a real dir must survive an unlink_junction");
        assert!(real.join("keep.txt").exists(), "contents must survive");
    }

    #[test]
    fn unlink_missing_junction_is_idempotent_success() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("node_modules");
        assert!(unlink_junction(&missing).is_ok());
    }

    #[test]
    fn remove_missing_worktree_is_idempotent_success() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone-wt");
        assert!(remove_worktree(&missing).is_ok());
    }

    #[test]
    fn reclaim_action_deserializes_known_and_unknown() {
        let j = r#"{"worktree_path":"p","repo":"r","action":"remove"}"#;
        let i: ReclaimInstruction = serde_json::from_str(j).unwrap();
        assert_eq!(i.action, ReclaimAction::Remove);
        assert!(!i.is_dirty);
        assert!(i.junctioned_paths.is_empty());

        let j2 = r#"{"worktree_path":"p","repo":"r","action":"rejunction"}"#;
        let i2: ReclaimInstruction = serde_json::from_str(j2).unwrap();
        assert_eq!(i2.action, ReclaimAction::Rejunction);

        let j3 = r#"{"worktree_path":"p","repo":"r","action":"frobnicate"}"#;
        let i3: ReclaimInstruction = serde_json::from_str(j3).unwrap();
        assert_eq!(i3.action, ReclaimAction::Unknown("frobnicate".to_string()));
    }

    #[test]
    fn pull_arming_defaults_off_when_absent() {
        // A pull body missing both arming flags must fail SAFE: neither
        // action armed (advisory-only). Also covers an OLD coord that only
        // ships the legacy `dry_run` field — unknown fields are ignored and
        // arming stays off.
        let j = r#"{"instructions":[]}"#;
        let p: ReclaimPull = serde_json::from_str(j).unwrap();
        assert!(
            !p.rejunction_armed,
            "missing rejunction_armed → false (safe)"
        );
        assert!(!p.remove_armed, "missing remove_armed → false (safe)");

        let legacy = r#"{"dry_run": false, "instructions": []}"#;
        let lp: ReclaimPull = serde_json::from_str(legacy).unwrap();
        assert!(!lp.rejunction_armed, "old-coord dry_run ignored → unarmed");
        assert!(!lp.remove_armed, "old-coord dry_run ignored → unarmed");
    }

    #[test]
    fn pull_parses_full_shape() {
        let j = r#"{
            "rejunction_armed": true,
            "remove_armed": false,
            "instructions": [
                {
                    "worktree_path": "D:/qontinui-root/qontinui-runner-wt-x",
                    "repo": "qontinui-runner",
                    "action": "remove",
                    "reason": "worktree:orphan",
                    "is_dirty": false,
                    "junctioned_paths": ["target", "node_modules"]
                }
            ]
        }"#;
        let p: ReclaimPull = serde_json::from_str(j).unwrap();
        assert!(p.rejunction_armed);
        assert!(!p.remove_armed);
        assert_eq!(p.instructions.len(), 1);
        assert_eq!(p.instructions[0].action, ReclaimAction::Remove);
        assert_eq!(
            p.instructions[0].junctioned_paths,
            vec!["target".to_string(), "node_modules".to_string()]
        );
    }

    // -----------------------------------------------------------------
    // G6 — in-flight-build guard.
    // -----------------------------------------------------------------

    #[test]
    fn g6_missing_paths_are_not_building() {
        // A worktree dir that doesn't exist (no target, no node_modules,
        // root absent) → not building.
        let dir = tempfile::tempdir().unwrap();
        let ghost = dir.path().join("does-not-exist");
        assert!(!worktree_is_building(&ghost, Duration::from_secs(600)));
    }

    #[test]
    fn g6_recent_root_mtime_is_building() {
        // A freshly-created worktree root (mtime = now) is within any
        // reasonable window → active.
        let dir = tempfile::tempdir().unwrap();
        assert!(
            worktree_is_building(dir.path(), Duration::from_secs(600)),
            "a just-touched root must read as building"
        );
    }

    #[test]
    fn g6_old_tree_with_no_lock_is_not_building() {
        // Zero-length window makes "recent" impossible; with no .cargo-lock
        // the tree reads as not building.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        assert!(
            !worktree_is_building(dir.path(), Duration::from_secs(0)),
            "no lock + window 0 → not building"
        );
    }

    #[cfg(windows)]
    #[test]
    fn g6_held_cargo_lock_is_building() {
        // Simulate cargo holding the lock: open the .cargo-lock with a
        // share mode that denies write (FILE_SHARE_READ only), mirroring
        // cargo's exclusive build lock, then probe.
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;

        let dir = tempfile::tempdir().unwrap();
        let debug = dir.path().join("target").join("debug");
        std::fs::create_dir_all(&debug).unwrap();
        let lock = debug.join(".cargo-lock");
        std::fs::write(&lock, b"").unwrap();

        // Hold it open with write-denied sharing.
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ)
            .open(&lock)
            .unwrap();

        // Window 0 isolates the lock signal from the mtime signal.
        assert!(
            worktree_is_building(dir.path(), Duration::from_secs(0)),
            "a held .cargo-lock must read as building"
        );
        drop(_held);
        // Once released, the lock opens cleanly → not building (window 0).
        assert!(!worktree_is_building(dir.path(), Duration::from_secs(0)));
    }

    #[test]
    fn g6_unheld_cargo_lock_is_not_building() {
        // A present-but-unheld .cargo-lock (crashed build) + window 0 →
        // not building (openable).
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("target").join("release");
        std::fs::create_dir_all(&release).unwrap();
        std::fs::write(release.join(".cargo-lock"), b"").unwrap();
        assert!(!worktree_is_building(dir.path(), Duration::from_secs(0)));
    }

    // -----------------------------------------------------------------
    // Phase 3 — registration hygiene (`git worktree prune`).
    // -----------------------------------------------------------------

    #[test]
    fn prune_invocation_shape_is_registration_only() {
        // The exact argv pins the invocation: `git -C <repo> worktree prune`
        // — a registration-metadata operation with no path-creating or
        // recursive-delete flags.
        let args = prune_command_args(Path::new("D:/qontinui-root/qontinui-runner"));
        assert_eq!(
            args,
            vec![
                "-C".to_string(),
                "D:/qontinui-root/qontinui-runner".to_string(),
                "worktree".to_string(),
                "prune".to_string(),
            ]
        );
    }

    #[test]
    fn prune_on_missing_dir_is_a_noop_that_creates_nothing() {
        // Husk-guard regression class: pruning must NEVER resurrect (or
        // create) a directory. A missing repo root stays missing.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone-repo");
        prune_parent_repo(&missing);
        assert!(!missing.exists(), "prune must not create the repo dir");
    }

    #[test]
    fn prune_on_non_repo_dir_creates_no_entries() {
        // Best-effort contract: a non-git dir warns (git fails) but the
        // dir's contents are untouched and nothing new appears — a pruned
        // registration can never re-materialize a worktree dir.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), b"data").unwrap();
        prune_parent_repo(dir.path());
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("keep.txt")]);
    }

    #[test]
    fn secs_env_parses_and_defaults() {
        // Good value → parsed; bad/missing → default.
        assert_eq!(parse_secs_env(Some("120"), 86_400), 120);
        assert_eq!(parse_secs_env(Some(" 120 "), 86_400), 120);
        assert_eq!(parse_secs_env(Some("not-a-number"), 86_400), 86_400);
        assert_eq!(parse_secs_env(Some(""), 86_400), 86_400);
        assert_eq!(parse_secs_env(Some("-5"), 86_400), 86_400);
        assert_eq!(parse_secs_env(None, 86_400), 86_400);
    }

    // -----------------------------------------------------------------
    // Phase 4 — coord-absent local backstop sweep.
    // -----------------------------------------------------------------

    #[test]
    fn backstop_ceiling_boundary() {
        let ceiling = DEFAULT_BACKSTOP_MAX_AGE_SECS;
        // Just-under and exactly-at the ceiling → NOT eligible (strictly
        // greater required).
        assert!(!backstop_eligible(ceiling - 1, ceiling, false, false));
        assert!(!backstop_eligible(ceiling, ceiling, false, false));
        // Just-over → eligible.
        assert!(backstop_eligible(ceiling + 1, ceiling, false, false));
    }

    #[test]
    fn backstop_suppressed_once_coord_seen_live() {
        // The monotonic boolean: coord seen live+armed ONCE suppresses the
        // backstop for the whole session, regardless of age.
        let ceiling = DEFAULT_BACKSTOP_MAX_AGE_SECS;
        assert!(!backstop_eligible(ceiling * 10, ceiling, true, false));
    }

    #[test]
    fn backstop_never_deletes_dirty_trees() {
        let ceiling = DEFAULT_BACKSTOP_MAX_AGE_SECS;
        assert!(!backstop_eligible(ceiling * 10, ceiling, false, true));
        // And dirty + coord-live together is doubly ineligible.
        assert!(!backstop_eligible(ceiling * 10, ceiling, true, true));
    }

    #[test]
    fn backstop_dirty_verdict_no_git_is_clean() {
        // A plain leaked dir with no `.git` at all: `git status` fails, but
        // there is nothing to be dirty → Clean (deletable).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("leftover.txt"), b"x").unwrap();
        assert_eq!(backstop_dirty_verdict(dir.path()), BackstopDirty::Clean);
    }

    #[test]
    fn backstop_dirty_verdict_corrupt_git_skips() {
        // A `.git` dir is PRESENT but empty/corrupt → `git status` fails →
        // CorruptSkip (we can't prove it's clean, so never delete).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(
            backstop_dirty_verdict(dir.path()),
            BackstopDirty::CorruptSkip
        );
    }

    #[test]
    fn backstop_env_override_bad_value_falls_back_to_default() {
        // The env plumbing shares parse_secs_env; pin the two defaults.
        assert_eq!(
            parse_secs_env(Some("garbage"), DEFAULT_BACKSTOP_MAX_AGE_SECS),
            14 * 86_400
        );
        assert_eq!(parse_secs_env(None, DEFAULT_PRUNE_INTERVAL_SECS), 86_400);
    }

    #[test]
    fn backstop_sweep_suppressed_when_coord_ever_live() {
        // With coord seen live, the sweep is a pure no-op — safe to call
        // directly in tests (it must return before touching any real root).
        backstop_sweep(true);
    }

    // ── CRITICAL-1 regression: an unreadable tree is never "clean" ──────────
    //
    // The data-loss path these pin: coord issues an armed `Remove`;
    // `execute_pull`'s execution-time re-check calls `worktree_is_dirty`; a
    // concurrent `index.lock` makes `git status` overrun `RECLAIM_CMD_TIMEOUT`
    // and get killed; the pre-review code mapped that to `false`, the `Remove`
    // was NOT skipped, `remove_worktree`'s own bounded `git worktree remove`
    // also degraded, and `std::fs::remove_dir_all` destroyed the WIP.

    /// A `.git`-bearing tree whose `git status` cannot answer must report
    /// DIRTY, so the `Remove` guard skips it.
    ///
    /// The `.git` file points at a directory that does not exist, so real git
    /// exits non-zero — the same `Degraded` arm a killed, timed-out child
    /// takes. Against the pre-review `ProbeOutcome::Degraded(_) => false`
    /// this assertion fails.
    #[test]
    fn an_unreadable_git_bearing_worktree_reads_as_dirty() {
        let dir = tempfile::tempdir().unwrap();
        // Linked-worktree shape: `.git` is a FILE naming an admin dir that
        // isn't there, so `git -C <dir> status` fails instead of walking up.
        std::fs::write(
            dir.path().join(".git"),
            "gitdir: /nonexistent/admin/dir/for/this/test\n",
        )
        .unwrap();

        assert!(
            worktree_is_dirty(dir.path()),
            "a worktree whose `git status` could not answer must NEVER read as \
             clean — that verdict is what authorizes `remove_dir_all`"
        );
    }

    /// The same input through the backstop sweep's verdict: `CorruptSkip`,
    /// never `Clean`.
    #[test]
    fn backstop_refuses_an_unreadable_git_bearing_candidate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".git"),
            "gitdir: /nonexistent/admin/dir/for/this/test\n",
        )
        .unwrap();

        assert_eq!(
            backstop_dirty_verdict(dir.path()),
            BackstopDirty::CorruptSkip
        );
    }

    /// The carve-out must survive the fix: a leaked plain directory with no
    /// `.git` has nothing to lose, so it stays removable. Without this the
    /// fail-closed change would strand every plain leaked dir forever.
    #[test]
    fn a_dir_with_no_git_is_still_removable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !worktree_is_dirty(dir.path()),
            "a directory with no `.git` cannot hold uncommitted work"
        );
        assert_eq!(backstop_dirty_verdict(dir.path()), BackstopDirty::Clean);
    }

    /// The predicate the `Remove` guard actually keys on, stated directly:
    /// only a proven-clean tree authorizes a destructive removal.
    #[test]
    fn only_a_proven_clean_verdict_permits_removal() {
        use super::super::dirty::DirtyVerdict;
        assert!(DirtyVerdict::Clean.permits_removal());
        assert!(!DirtyVerdict::Dirty.permits_removal());
        assert!(!DirtyVerdict::Unknown.permits_removal());
    }
}
