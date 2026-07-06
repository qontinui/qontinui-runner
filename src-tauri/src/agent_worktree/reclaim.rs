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
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::census::is_junction;

/// Default reclaim poll cadence — 300s (5 min), matching the census.
/// Override via `QONTINUI_WORKTREE_RECLAIM_INTERVAL_SECS`.
const DEFAULT_RECLAIM_INTERVAL_SECS: u64 = 300;

/// Default G6 "recently active" window — 600s. A worktree whose `target/`,
/// `node_modules/`, or root was touched within this window is treated as
/// actively building and skipped. Override via
/// `QONTINUI_WORKTREE_RECLAIM_ACTIVITY_WINDOW_SECS`.
const DEFAULT_ACTIVITY_WINDOW_SECS: u64 = 600;

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
fn execute_step(step: &ReclaimStep) -> Result<(), String> {
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
fn remove_worktree(path: &Path) -> Result<(), String> {
    if !path.exists() {
        debug!(
            "worktree_reclaim: worktree {} already gone — no-op",
            path.display()
        );
        return Ok(());
    }
    let path_str = path.to_string_lossy().to_string();
    // `git -C <wt> worktree remove --force <wt>` prunes the registration.
    let git_ok = crate::process_helpers::no_window("git")
        .args(["-C", &path_str, "worktree", "remove", "--force", &path_str])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
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
    Ok(())
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
    let out = crate::process_helpers::no_window("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &link.to_string_lossy(),
            &target.to_string_lossy(),
        ])
        .output()
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
/// be stale). `true` when `git status --porcelain` is non-empty.
fn worktree_is_dirty(path: &Path) -> bool {
    let path_str = match path.to_str() {
        Some(s) => s,
        None => return false,
    };
    crate::process_helpers::no_window("git")
        .args(["-C", path_str, "status", "--porcelain"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
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
        for step in &steps {
            if let Err(e) = execute_step(step) {
                warn!("worktree_reclaim: step {step:?} failed: {e}");
                // Continue — idempotent retry next tick. We do NOT abort
                // the remaining steps blindly, but if a junction unlink
                // failed we MUST NOT proceed to the worktree removal.
                if matches!(step, ReclaimStep::UnlinkJunction(_)) {
                    warn!(
                        "worktree_reclaim: aborting remaining steps for {} — a junction unlink \
                         failed, recursive removal would be unsafe (INV-W4)",
                        instr.worktree_path
                    );
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pull + tick + spawn (mirrors census.rs identity/base resolution).
// ---------------------------------------------------------------------------

/// One reclaim cycle: pull + execute. Returns `Ok(())` on a clean skip or
/// a successful pull; `Err` only on a transport / non-2xx failure.
pub async fn tick_once() -> Result<(), String> {
    let device_id = match super::census::load_device_id_pub() {
        Some(id) => id,
        None => {
            debug!("worktree_reclaim: no device_id — skipping");
            return Ok(());
        }
    };
    let base = match super::census::coord_http_base_pub() {
        Some(b) => b,
        None => {
            debug!("worktree_reclaim: no coord_url configured — skipping");
            return Ok(());
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
        .map_err(|e| format!("build reclaim http client: {e}"))?;
    let resp = crate::coord_http::coord_get(&client, &url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
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

    // execute_pull runs synchronous git/cmd subprocesses and filesystem
    // removals — potentially long (junction unlinks + worktree deletes).
    // Run it on the blocking pool so the shared fleet-publishers runtime's
    // async worker isn't pinned for the duration (the starvation class
    // PR #391 isolated the heartbeat from).
    tokio::task::spawn_blocking(move || execute_pull(&pull))
        .await
        .map_err(|e| format!("reclaim execution panicked: {e}"))?;
    Ok(())
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
        loop {
            tick.tick().await;
            if let Err(e) = tick_once().await {
                warn!("worktree_reclaim: {e}");
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

    fn instr(action: ReclaimAction, junctioned: &[&str], is_dirty: bool) -> ReclaimInstruction {
        ReclaimInstruction {
            worktree_path: "D:/qontinui-root/qontinui-runner-wt-foo".to_string(),
            repo: "qontinui-runner".to_string(),
            action,
            reason: "worktree:orphan".to_string(),
            is_dirty,
            junctioned_paths: junctioned.iter().map(|s| s.to_string()).collect(),
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
}
