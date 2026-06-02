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
//! ## Arming
//!
//! coord ships `dry_run = true` by default — every instruction is LOGGED
//! ("would do X") and nothing destructive happens. The operator flips
//! `COORD_WORKTREE_RECLAIM_ENABLED` server-side to set `dry_run = false`,
//! which arms execution. Defense in depth: even when armed, the runner
//! NEVER acts on an `is_dirty` worktree (coord also filters these).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::census::is_junction;

/// Default reclaim poll cadence — 300s (5 min), matching the census.
/// Override via `QONTINUI_WORKTREE_RECLAIM_INTERVAL_SECS`.
const DEFAULT_RECLAIM_INTERVAL_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// Wire types — coord serializes these.
// ---------------------------------------------------------------------------

/// The pull-endpoint body: `GET {coord}/coord/worktree-reclaim/{device_id}`.
#[derive(Debug, Clone, Deserialize)]
pub struct ReclaimPull {
    /// When `true` (coord's default), every instruction is LOGGED but no
    /// destructive action runs. The operator arms execution server-side
    /// via `COORD_WORKTREE_RECLAIM_ENABLED`.
    #[serde(default = "default_dry_run")]
    pub dry_run: bool,
    #[serde(default)]
    pub instructions: Vec<ReclaimInstruction>,
}

/// `dry_run` defaults to `true` so a missing/older-coord field fails
/// SAFE — never destructive.
fn default_dry_run() -> bool {
    true
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
/// worktree removal, that a dirty instruction yields only a `Skip`, and
/// that a dry-run yields only `Skip`s.
///
/// `canonical_path` is the repo's canonical checkout — the junction target
/// root for a `rejunction`. `None` when the runner couldn't resolve it
/// (then a rejunction degrades to a `Skip`).
pub fn plan_reclaim(
    instr: &ReclaimInstruction,
    dry_run: bool,
    canonical_path: Option<&Path>,
) -> Vec<ReclaimStep> {
    // Defense in depth #1 — NEVER act on a dirty worktree.
    if instr.is_dirty {
        return vec![ReclaimStep::Skip(format!(
            "is_dirty worktree {} — refusing all destructive action",
            instr.worktree_path
        ))];
    }
    // Defense in depth #2 — a dry-run does nothing destructive. We still
    // emit a Skip per instruction so the caller logs "would do X".
    if dry_run {
        return vec![ReclaimStep::Skip(format!(
            "dry_run: would {:?} worktree {} (reason={})",
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
    let git_ok = std::process::Command::new("git")
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
    if let Some(parent) = link.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("rejunction: mkdir {} : {e}", parent.display()))?;
        }
    }
    // `cmd /C mklink /J <link> <target>` — /J = directory junction.
    let out = std::process::Command::new("cmd")
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
    std::process::Command::new("git")
        .args(["-C", path_str, "status", "--porcelain"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Execute all instructions in one pull.
fn execute_pull(pull: &ReclaimPull) {
    if pull.instructions.is_empty() {
        debug!("worktree_reclaim: no instructions");
        return;
    }
    info!(
        "worktree_reclaim: {} instruction(s), dry_run={}",
        pull.instructions.len(),
        pull.dry_run
    );
    for instr in &pull.instructions {
        // Execution-time re-check: even if coord said clean, refuse a
        // worktree that's dirty right now.
        let wt = PathBuf::from(&instr.worktree_path);
        let dirty_now = !pull.dry_run && instr.action == ReclaimAction::Remove && {
            let d = worktree_is_dirty(&wt);
            if d {
                warn!(
                    "worktree_reclaim: {} dirty at execution time — skipping remove (INV defense)",
                    instr.worktree_path
                );
            }
            d
        };
        if dirty_now {
            continue;
        }

        let canonical = super::canonical_paths::default_canonical_path(&instr.repo).ok();
        let steps = plan_reclaim(instr, pull.dry_run, canonical.as_deref());
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
    let resp = client
        .get(&url)
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

    execute_pull(&pull);
    Ok(())
}

/// Spawn the periodic reclaim poller. Interval from
/// `QONTINUI_WORKTREE_RECLAIM_INTERVAL_SECS` (default 300s, floored 30s).
/// `MissedTickBehavior::Skip` + warn-and-retry, like
/// [`super::census::spawn_census`].
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
        let steps = plan_reclaim(&i, false, None);

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
        let steps = plan_reclaim(&i, false, None);
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
        let steps = plan_reclaim(&i, false, None);
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
        let steps = plan_reclaim(&i, false, None);
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
    fn dry_run_yields_no_destructive_steps() {
        let i = instr(ReclaimAction::Remove, &["target", "node_modules"], false);
        let steps = plan_reclaim(&i, /* dry_run */ true, None);
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
    fn dry_run_skips_even_a_rejunction() {
        let i = instr(ReclaimAction::Rejunction, &["target"], false);
        let canonical = PathBuf::from("D:/qontinui-root/qontinui-runner");
        let steps = plan_reclaim(&i, true, Some(&canonical));
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], ReclaimStep::Skip(_)));
    }

    #[test]
    fn rejunction_creates_junctions_to_canonical() {
        let i = instr(
            ReclaimAction::Rejunction,
            &["target", "node_modules"],
            false,
        );
        let canonical = PathBuf::from("D:/qontinui-root/qontinui-runner");
        let steps = plan_reclaim(&i, false, Some(&canonical));
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
        let steps = plan_reclaim(&i, false, None);
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
        let steps = plan_reclaim(&i, false, None);
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], ReclaimStep::Skip(_)));
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
    fn pull_dry_run_defaults_true_when_absent() {
        // A pull body missing `dry_run` must fail SAFE (dry_run=true).
        let j = r#"{"instructions":[]}"#;
        let p: ReclaimPull = serde_json::from_str(j).unwrap();
        assert!(p.dry_run, "missing dry_run must default to true (safe)");
    }

    #[test]
    fn pull_parses_full_shape() {
        let j = r#"{
            "dry_run": false,
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
        assert!(!p.dry_run);
        assert_eq!(p.instructions.len(), 1);
        assert_eq!(p.instructions[0].action, ReclaimAction::Remove);
        assert_eq!(
            p.instructions[0].junctioned_paths,
            vec!["target".to_string(), "node_modules".to_string()]
        );
    }
}
