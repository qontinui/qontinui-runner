//! Ξ_Orphan cargo-target reaper (runner side).
//!
//! ## Why this exists — the reclaim blind spot
//!
//! The shipped worktree-reclaim system ([`super::reclaim`] + [`super::census`])
//! is **worktree-scoped and canonical-name-scoped**: the census enumerates git
//! worktrees and measures only the dir literally named `target`
//! (`src-tauri/target` for the Tauri runner) / `node_modules` inside each, and
//! the reaper only ever removes `<worktree>/target` / `<worktree>/node_modules`.
//!
//! But agents deliberately avoid the canonical `<worktree>/target` to dodge
//! build-lock contention with the always-running primary runner. `cargo-guard`
//! routes agent builds to `target-agent/` and honors any caller-set
//! `CARGO_TARGET_DIR` (e.g. `../target-pool/slot-0`). The result on disk is a
//! large population of **out-of-tree cargo target roots** — direct children of
//! [`super::census::qontinui_root`] that are NOT worktrees and have no `.git`:
//! `target-wt-<slug>`, `_cargo-targets/<slug>`, `cargo-targets/<slug>`,
//! `.cargo-target-<slug>`, `_targets/<slug>`, `.wt-targets/<slug>`,
//! `target-sessrestore`, `cargo-target-coord`, `_wt/<slug>/target*`, … Each is
//! 15–35 GB. These match no worktree pattern, so the census never walks them and
//! the reaper never reaps them. On 2026-07-23 they filled `D:` to 1.3 % free and
//! ~1.35 TB was recovered by hand. This module reaps that population
//! automatically, complementing (never replacing) the worktree reaper.
//!
//! ## Scope — out-of-tree roots ONLY
//!
//! To stay cleanly disjoint from the worktree reaper AND the build system, this
//! reaper NEVER descends into a repo checkout or a worktree, so it can never
//! touch a canonical `<repo>/target`, `<repo>/src-tauri/target`, `target-agent`,
//! or `target-pool` (those live INSIDE repo dirs and belong to the build
//! system / worktree reaper). It only considers:
//!   * a direct child of `qontinui_root()` that is ITSELF a cargo target root, and
//!   * children (one level, `_wt` two) of the known container dirs
//!     (`_cargo-targets`, `cargo-targets`, `_targets`, `.wt-targets`, `_wt`).
//!
//! A "cargo target root" is identified positively by a cargo marker — a
//! `CACHEDIR.TAG` (newer cargo) OR a `.rustc_info.json` (present in EVERY cargo
//! target root, including the majority on this machine that have no tag). Either
//! is authoritative; a dir with neither is never treated as a target.
//!
//! ## Safety gates (each candidate must clear ALL — see [`classify`])
//!   * **G-kept (pin)** — never a dir carrying a `.reap-keep` sentinel or named
//!     in `COORD_ORPHAN_TARGET_KEEP`. The ledger-less equivalent of the worktree
//!     reaper's `retention='pinned'`; a pin wins even when armed. A pinned
//!     worktree's own INTERNAL `target` is inherently safe (this reaper never
//!     descends into a worktree). BUT if that pinned worktree builds into an
//!     OUT-OF-TREE `CARGO_TARGET_DIR` cache, that cache is ledger-less and the
//!     worktree's coord pin does NOT extend to it — it is reaped like any other
//!     out-of-tree root once idle >24 h. That is acceptable (the cache is
//!     rebuildable, not source), but to hard-protect a specific out-of-tree
//!     cache the operator drops a `.reap-keep` in it or lists it in
//!     `COORD_ORPHAN_TARGET_KEEP`. (A future enhancement could map coord's
//!     pinned set to out-of-tree caches; not done here.)
//!   * **G-reparse** — never a reparse point; removal unlinks the *link* only,
//!     never recurses into a junction target (the ui-bridge junction-follow
//!     incident, [`super::reclaim`] INV-W4).
//!   * **G-classify** — POSITIVELY a rebuildable cargo target: a cargo marker
//!     (`CACHEDIR.TAG` OR `.rustc_info.json`, the enumerator gate) AND a
//!     `debug`/`release` profile layout ([`looks_like_cargo_artifact`]). Never
//!     deletes by name alone — a mis-placed source dir that happened to sit under
//!     a container never qualifies.
//!   * **G-live** — not currently building: no held `.cargo-lock` under any
//!     profile dir, AND deepest build-artifact mtime older than the grace window.
//!     The **deepest** mtime is load-bearing: a target root's own dir mtime lies
//!     (observed a root whose top mtime was 10 days old but whose `debug/` was
//!     written 15 min prior — an active build). Reaping on root mtime alone would
//!     kill a live build.
//!   * **G-grace** — deepest artifact mtime ≥ `COORD_ORPHAN_TARGET_GRACE_SECS`
//!     (default 86400 = 24 h, matching the worktree remove-grace).
//!
//! ## Deliberately out of scope (named follow-ups, not silent gaps)
//!   * **Renamed IN-worktree targets** (`<repo>-wt-<slug>/target-<slug>`) — these
//!     live inside enumerated worktrees; extending the worktree census/reaper to
//!     recognize non-`target` build-dir names is the correct home for them (keeps
//!     the Pinned/dirty/G6 worktree guards authoritative). This reaper stays
//!     out-of-tree-only so it can never race those guards.
//!   * **Orphan `node_modules`** — a distinct population (no `CACHEDIR.TAG`); not
//!     reaped here.
//!
//! ## Arming (fail-safe)
//!
//! DRY-RUN by default; armed only by `COORD_ORPHAN_TARGET_REAP_ENABLED` truthy —
//! same posture as [`super::reclaim`]. While dark it merely LOGS what it would
//! reap (+ bytes), so the operator reviews a shadow cycle before arming.
//!
//! ## Posture
//!
//! Machine-wide, best-effort periodic poller (default 900s, env
//! `QONTINUI_ORPHAN_TARGET_INTERVAL_SECS`, floored 60s). No coord round-trip and
//! no identity needed — purely local disk hygiene. A failing tick `warn!`s and
//! retries; the loop never panics.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{info, warn};

use super::census::{is_junction, qontinui_root};

/// Env flag arming destructive removal. Unset/false → dry-run (log only).
pub const REAP_ENABLED_ENV: &str = "COORD_ORPHAN_TARGET_REAP_ENABLED";
/// Env override for the idle grace (seconds) a root must clear. Default 24 h.
pub const GRACE_SECS_ENV: &str = "COORD_ORPHAN_TARGET_GRACE_SECS";
/// Env override for the poll interval (seconds). Default 900 s, floored 60 s.
pub const INTERVAL_SECS_ENV: &str = "QONTINUI_ORPHAN_TARGET_INTERVAL_SECS";
/// Env comma-separated allowlist of target-root basenames to NEVER reap — the
/// operator's out-of-tree pin (the worktree reaper's `retention='pinned'` is
/// ledger-scoped and does not reach ledger-less orphan target dirs).
pub const KEEP_ENV: &str = "COORD_ORPHAN_TARGET_KEEP";
/// A sentinel file placed at a target root's top level marks it kept forever —
/// the per-dir equivalent of a retention pin, honored even when armed.
pub const KEEP_SENTINEL: &str = ".reap-keep";

const DEFAULT_GRACE_SECS: u64 = 86_400;
const DEFAULT_INTERVAL_SECS: u64 = 900;

/// Container dirs whose immediate children may themselves be target roots.
/// `_wt` gets one extra level (`_wt/<slug>/target*`).
const CONTAINER_DIRS: &[&str] = &["_cargo-targets", "cargo-targets", "_targets", ".wt-targets"];
const NESTED_CONTAINER_DIRS: &[&str] = &["_wt"];

/// True iff `COORD_ORPHAN_TARGET_REAP_ENABLED` is truthy. Default OFF (dry-run).
pub fn reap_armed() -> bool {
    matches!(
        std::env::var(REAP_ENABLED_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

fn grace() -> Duration {
    let secs = std::env::var(GRACE_SECS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_GRACE_SECS);
    Duration::from_secs(secs)
}

/// True iff `dir` carries a co-authoritative cargo target marker: a
/// `CACHEDIR.TAG` (newer cargo) OR a `.rustc_info.json` (present in every cargo
/// target root, including the many on this machine that predate / lack the tag —
/// e.g. `_cargo-targets/<slug>`, `_targets/<slug>`, `.wt-targets/<slug>`,
/// `cargo-target-coord-wt-<slug>` all have `.rustc_info.json + debug/` but NO
/// tag). Either marker positively identifies a cargo build dir; a dir with
/// neither (a real source/data dir) is never a target root.
fn has_cargo_marker(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join("CACHEDIR.TAG")).is_ok()
        || std::fs::symlink_metadata(dir.join(".rustc_info.json")).is_ok()
}

/// A cargo target root is a non-junction dir carrying a cargo marker
/// ([`has_cargo_marker`]). `symlink_metadata` (via `is_junction`) so we never
/// follow into a junctioned dir.
fn is_target_root(dir: &Path) -> bool {
    if is_junction(dir) {
        return false;
    }
    has_cargo_marker(dir)
}

/// One reap candidate: an out-of-tree cargo target root.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
}

/// Enumerate out-of-tree cargo target roots under `root`. Never descends into a
/// repo checkout or worktree — only direct children that are themselves target
/// roots, plus the shallow contents of the known container dirs.
pub fn enumerate_candidates(root: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.file_type().is_dir() || meta.file_type().is_symlink() || is_junction(&path) {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Case 1: the direct child is itself a bare target root.
        if is_target_root(&path) {
            out.push(Candidate { path });
            continue;
        }
        // Case 2: a known container dir — its immediate children may be roots.
        if CONTAINER_DIRS.contains(&name) {
            collect_roots_shallow(&path, 1, &mut out);
            continue;
        }
        // Case 3: `_wt` — target roots live one level deeper (`_wt/<slug>/target*`).
        if NESTED_CONTAINER_DIRS.contains(&name) {
            collect_roots_shallow(&path, 2, &mut out);
            continue;
        }
        // Otherwise: a repo checkout / worktree / data dir — never descend.
    }
    out
}

/// Collect target roots up to `depth` levels below `dir` (depth 1 = direct
/// children). Skips reparse points; stops at the first target root on a path.
fn collect_roots_shallow(dir: &Path, depth: u32, out: &mut Vec<Candidate>) {
    if depth == 0 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.file_type().is_dir() || meta.file_type().is_symlink() || is_junction(&path) {
            continue;
        }
        if is_target_root(&path) {
            out.push(Candidate { path });
        } else {
            collect_roots_shallow(&path, depth - 1, out);
        }
    }
}

/// True iff a `.cargo-lock` under any profile dir of `target_root` is held by a
/// live cargo invocation (Windows: a write-open fails with a sharing violation).
/// Mirrors [`super::reclaim`]'s `cargo_lock_held`; conservative (treats stat
/// errors as "held").
fn cargo_lock_held(target_root: &Path) -> bool {
    let mut profile_dirs: Vec<PathBuf> =
        vec![target_root.join("debug"), target_root.join("release")];
    if let Ok(entries) = std::fs::read_dir(target_root) {
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return true,
            Ok(_) => {}
        }
        match std::fs::OpenOptions::new().write(true).open(&lock) {
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return true,
        }
    }
    false
}

/// Deepest (newest) build-artifact mtime under `target_root`, probing the dirs
/// where a live build writes: `debug/{deps,.fingerprint,incremental,build}` and
/// their `release` twins, plus the root. Returns the elapsed time since that
/// newest mtime. `None` if nothing is readable (caller treats `None` as active,
/// fail-safe). Bounded (one `read_dir` per probe dir) — not a full recursive walk.
///
/// NOTE: a cross-compile `cargo build --target <triple>` writes under
/// `<root>/<triple>/debug/…` rather than `<root>/debug/…`, which these
/// top-level probes do not reach. cargo-guard does not pass `--target` today, so
/// no orphan root here is a cross-compile dir; if that changes, add the
/// `<triple>/debug` probes (and the matching lock dirs in `cargo_lock_held`).
/// The 24 h grace still backstops this: a cross-compile root idle >24 h is safe.
fn newest_artifact_age(target_root: &Path) -> Option<Duration> {
    let probes = [
        "debug/deps",
        "debug/.fingerprint",
        "debug/incremental",
        "debug/build",
        "debug",
        "release/deps",
        "release/.fingerprint",
        "release",
        ".",
    ];
    let mut newest: Option<std::time::SystemTime> = None;
    for rel in probes {
        let p = target_root.join(rel);
        let entries = match std::fs::read_dir(&p) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    newest = Some(match newest {
                        Some(cur) if cur >= mtime => cur,
                        _ => mtime,
                    });
                }
            }
        }
    }
    newest.map(|t| t.elapsed().unwrap_or(Duration::ZERO))
}

/// Recursive byte size of `dir`, summing real file sizes and SKIPPING any
/// reparse point (never traversed). For pre-removal telemetry only.
fn dir_size_skipping_junctions(dir: &Path) -> u64 {
    let mut total: u64 = 0;
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    for entry in read.flatten() {
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if is_junction(&path) {
                continue;
            }
            total = total.saturating_add(dir_size_skipping_junctions(&path));
        } else if ft.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// The reason a candidate was NOT reaped this tick (for shadow-mode logging).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The root (or a nested dir being deleted) is a reparse point — never
    /// followed; only the link is ever unlinked, never recursed into.
    Reparse,
    /// A live build owns it (held `.cargo-lock` or fresh artifact mtime).
    Building,
    /// Idle, but the no-activity grace has not yet elapsed.
    GracePending,
    /// Operator-pinned via the `.reap-keep` sentinel or the `COORD_ORPHAN_TARGET_KEEP`
    /// allowlist — the ledger-less equivalent of a worktree retention pin.
    Kept,
}

/// True iff this target root is operator-pinned: a `.reap-keep` sentinel at its
/// top level, or its basename in the `COORD_ORPHAN_TARGET_KEEP` comma-list.
/// Honored even when armed — a pin is never overridden by arming.
fn is_kept(path: &Path) -> bool {
    if std::fs::symlink_metadata(path.join(KEEP_SENTINEL)).is_ok() {
        return true;
    }
    if let Ok(list) = std::env::var(KEEP_ENV) {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            return list
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .any(|kept| kept == name);
        }
    }
    false
}

/// Classify a candidate: `Ok(())` = reapable, `Err(reason)` = skip. Pure over
/// the injected `grace` so it is unit-testable without env/disk timing. The
/// checks are ordered cheapest-and-safest first so a pinned/junction/live dir
/// short-circuits before any deletion is ever contemplated.
pub fn classify(path: &Path, grace: Duration) -> Result<(), SkipReason> {
    // Pin wins over everything, including arming.
    if is_kept(path) {
        return Err(SkipReason::Kept);
    }
    // Never touch a reparse point — unlink the link only, never recurse in.
    if is_junction(path) {
        return Err(SkipReason::Reparse);
    }
    // Positive artifact classification: a real cargo target root carries a
    // CACHEDIR.TAG (checked by the caller/enumerator) AND either a
    // `.rustc_info.json` or a `debug`/`release` profile layout. Refuse to reap
    // anything that does not positively look like a rebuildable build dir —
    // never by name alone.
    if !looks_like_cargo_artifact(path) {
        // Not a recognizable build dir → treat as building (never delete).
        return Err(SkipReason::Building);
    }
    if cargo_lock_held(path) {
        return Err(SkipReason::Building);
    }
    match newest_artifact_age(path) {
        // Unreadable age → fail-safe: treat as building (do not reap).
        None => Err(SkipReason::Building),
        Some(age) if age < grace => Err(SkipReason::GracePending),
        Some(_) => Ok(()),
    }
}

/// Positive rebuildable-artifact check (defense in depth on top of the
/// enumerator's marker gate): the dir must carry a cargo marker
/// ([`has_cargo_marker`] — `CACHEDIR.TAG` OR `.rustc_info.json`) AND a
/// `debug`/`release` profile subdir. This makes deletion contingent on the dir
/// *looking like* a built cargo target, never on its name — a mis-placed source
/// dir, or a bare-marker dir with no built profiles, never qualifies. Verified
/// against the real population: `_wt-target` (no marker) is rejected here and by
/// the enumerator; a `CACHEDIR.TAG`-only dir with no profiles is rejected here.
fn looks_like_cargo_artifact(path: &Path) -> bool {
    let has_profile = path.join("debug").is_dir() || path.join("release").is_dir();
    has_cargo_marker(path) && has_profile
}

/// Junction-safe recursive removal. Unlinks any nested reparse point with
/// `remove_dir` (link only, never recursing into its target) before deleting
/// real content, then removes `dir` itself. Mirrors [`super::reclaim`] INV-W4.
fn remove_junction_safe(dir: &Path) -> std::io::Result<()> {
    // `dir` itself must not be a reparse point — the caller guarantees it, but
    // defend anyway: unlink the link only.
    if is_junction(dir) {
        return std::fs::remove_dir(dir);
    }
    let entries = std::fs::read_dir(dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_dir() {
            if is_junction(&path) || ft.is_symlink() {
                // Reparse point — unlink the link, never recurse in.
                let _ = std::fs::remove_dir(&path);
            } else {
                remove_junction_safe(&path)?;
            }
        } else if ft.is_symlink() {
            let _ = std::fs::remove_file(&path);
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    std::fs::remove_dir(dir)
}

/// One reaper cycle against the ambient config (`qontinui_root()` + env arming +
/// env grace). Thin wrapper over [`run_cycle`] so the loop stays trivial.
pub fn tick_once() -> ReapSummary {
    let root = match qontinui_root() {
        Some(r) => r,
        None => return ReapSummary::default(),
    };
    run_cycle(&root, reap_armed(), grace())
}

/// One reaper cycle over `root` with explicit `armed`/`grace` (the testable
/// seam). Enumerates out-of-tree target roots, classifies each, and — only when
/// `armed` — removes the reapable ones junction-safely. When `!armed` it merely
/// logs "would reap" and removes NOTHING. Returns a summary.
pub fn run_cycle(root: &Path, armed: bool, grace: Duration) -> ReapSummary {
    let candidates = enumerate_candidates(root);
    let mut summary = ReapSummary {
        scanned: candidates.len(),
        ..Default::default()
    };
    for c in candidates {
        match classify(&c.path, grace) {
            Err(SkipReason::Kept) => summary.skipped_kept += 1,
            Err(SkipReason::Reparse) => summary.skipped_reparse += 1,
            Err(SkipReason::Building) => summary.skipped_live += 1,
            Err(SkipReason::GracePending) => summary.skipped_grace += 1,
            Ok(()) => {
                let bytes = dir_size_skipping_junctions(&c.path);
                summary.candidates += 1;
                summary.candidate_bytes = summary.candidate_bytes.saturating_add(bytes);
                if !armed {
                    info!(
                        "orphan_target_reaper: [dry-run] would reap {} ({:.2} GB)",
                        c.path.display(),
                        bytes as f64 / 1_073_741_824.0
                    );
                    continue;
                }
                match remove_junction_safe(&c.path) {
                    Ok(()) => {
                        summary.reaped += 1;
                        summary.reaped_bytes = summary.reaped_bytes.saturating_add(bytes);
                        info!(
                            "orphan_target_reaper: reaped {} ({:.2} GB)",
                            c.path.display(),
                            bytes as f64 / 1_073_741_824.0
                        );
                    }
                    Err(e) => {
                        summary.errors += 1;
                        warn!(
                            "orphan_target_reaper: failed to remove {}: {e}",
                            c.path.display()
                        );
                    }
                }
            }
        }
    }
    info!(
        "orphan_target_reaper: cycle armed={armed} scanned={} candidates={} \
         candidate_gb={:.2} reaped={} reaped_gb={:.2} \
         skipped(live={},grace={},reparse={},kept={}) errors={}",
        summary.scanned,
        summary.candidates,
        summary.candidate_bytes as f64 / 1_073_741_824.0,
        summary.reaped,
        summary.reaped_bytes as f64 / 1_073_741_824.0,
        summary.skipped_live,
        summary.skipped_grace,
        summary.skipped_reparse,
        summary.skipped_kept,
        summary.errors,
    );
    summary
}

/// Per-cycle telemetry.
#[derive(Debug, Default, Clone)]
pub struct ReapSummary {
    pub scanned: usize,
    pub candidates: usize,
    pub candidate_bytes: u64,
    pub reaped: usize,
    pub reaped_bytes: u64,
    pub skipped_live: usize,
    pub skipped_grace: usize,
    pub skipped_reparse: usize,
    pub skipped_kept: usize,
    pub errors: usize,
}

/// Spawn the periodic reaper on the ambient tokio runtime. Interval from
/// `QONTINUI_ORPHAN_TARGET_INTERVAL_SECS` (default 900 s, floored 60 s).
/// `MissedTickBehavior::Skip`; failures never panic. The blocking disk work
/// runs on `spawn_blocking` so it never stalls the async runtime.
pub fn spawn_orphan_reaper() {
    let secs: u64 = std::env::var(INTERVAL_SECS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(60);
    info!(
        "orphan_target_reaper: starting periodic reaper, interval={}s, armed={}",
        secs,
        reap_armed()
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = tokio::task::spawn_blocking(tick_once).await {
                warn!("orphan_target_reaper: cycle task panicked/cancelled: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A cargo target root with a `CACHEDIR.TAG` (newer cargo).
    fn mk_target_root(dir: &Path) {
        fs::create_dir_all(dir.join("debug/deps")).unwrap();
        fs::write(dir.join("CACHEDIR.TAG"), b"Signature: 8a477f597d28d172").unwrap();
    }

    /// A cargo target root that has NO `CACHEDIR.TAG` — only `.rustc_info.json`
    /// + a `debug/` layout. This is the MAJORITY population on the real machine
    /// (`_cargo-targets/<slug>`, `_targets/<slug>`, `.wt-targets/<slug>`, …).
    fn mk_target_root_no_tag(dir: &Path) {
        fs::create_dir_all(dir.join("debug/deps")).unwrap();
        fs::write(dir.join(".rustc_info.json"), b"{}").unwrap();
    }

    #[test]
    fn is_target_root_requires_a_cargo_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        assert!(!is_target_root(&plain));
        let tgt = tmp.path().join("tgt");
        mk_target_root(&tgt);
        assert!(is_target_root(&tgt));
    }

    #[test]
    fn no_tag_rustc_info_dir_is_a_target_and_classifies_reapable() {
        // GAP 1 regression: the CACHEDIR.TAG-less majority must be recognized.
        let tmp = tempfile::tempdir().unwrap();
        let tgt = tmp.path().join("coord-rebase701");
        mk_target_root_no_tag(&tgt);
        assert!(
            is_target_root(&tgt),
            ".rustc_info.json alone marks a target root"
        );
        // Enumerated as a bare out-of-tree root.
        let cands = enumerate_candidates(tmp.path());
        assert!(cands.iter().any(|c| c.path.ends_with("coord-rebase701")));
        // And classified reapable (positive artifact: marker + debug/) at 0 grace.
        assert_eq!(classify(&tgt, Duration::ZERO), Ok(()));
    }

    #[test]
    fn enumerate_finds_bare_and_container_roots_but_not_repo_checkouts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Bare out-of-tree target root (direct child).
        mk_target_root(&root.join("target-wt-saf3"));
        // Container dir with a child root.
        mk_target_root(&root.join("_cargo-targets/blastfix"));
        // Nested container (_wt/<slug>/target).
        mk_target_root(&root.join("_wt/coord-x/target"));
        // A repo checkout with a canonical target INSIDE — must NOT be found
        // (we never descend into a non-container, non-root child).
        let repo = root.join("qontinui-runner");
        fs::create_dir_all(&repo).unwrap();
        mk_target_root(&repo.join("target"));
        mk_target_root(&repo.join("src-tauri/target"));

        let cands = enumerate_candidates(root);
        let names: Vec<String> = cands
            .iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"target-wt-saf3".to_string()));
        assert!(names.contains(&"blastfix".to_string()));
        // The only `target` in the set is the _wt one, never the two inside the
        // repo checkout — we must never descend into a repo checkout / worktree.
        let wt_target = cands
            .iter()
            .filter(|c| c.path.components().any(|comp| comp.as_os_str() == "_wt"))
            .count();
        assert_eq!(wt_target, 1, "_wt/coord-x/target must be found");
        let repo_leak = cands.iter().any(|c| {
            c.path
                .components()
                .any(|comp| comp.as_os_str() == "qontinui-runner")
        });
        assert!(!repo_leak, "must never descend into a repo checkout");
    }

    #[test]
    fn classify_grace_pending_when_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let tgt = tmp.path().join("t");
        mk_target_root(&tgt);
        fs::write(tgt.join("debug/deps/libfoo.rlib"), b"x").unwrap();
        // Just-written → younger than a 24h grace → GracePending.
        assert_eq!(
            classify(&tgt, Duration::from_secs(86_400)),
            Err(SkipReason::GracePending)
        );
        // Zero grace → reapable (nothing is younger than 0).
        assert_eq!(classify(&tgt, Duration::ZERO), Ok(()));
    }

    #[test]
    fn classify_skips_pinned_via_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        let tgt = tmp.path().join("t");
        mk_target_root(&tgt);
        // Old enough to otherwise be reapable...
        // (zero grace makes age always >= grace)
        assert_eq!(classify(&tgt, Duration::ZERO), Ok(()));
        // ...but a .reap-keep sentinel pins it, even at zero grace.
        fs::write(tgt.join(KEEP_SENTINEL), b"").unwrap();
        assert_eq!(classify(&tgt, Duration::ZERO), Err(SkipReason::Kept));
    }

    #[test]
    fn classify_refuses_dir_without_positive_artifact_markers() {
        let tmp = tempfile::tempdir().unwrap();
        // A dir with a CACHEDIR.TAG but NO profile layout / rustc_info — must
        // NOT be classified reapable (positive classification, never by name).
        let fake = tmp.path().join("target-lookalike");
        fs::create_dir_all(&fake).unwrap();
        fs::write(fake.join("CACHEDIR.TAG"), b"Signature: 8a477f597d28d172").unwrap();
        assert_eq!(classify(&fake, Duration::ZERO), Err(SkipReason::Building));
    }

    #[cfg(unix)]
    #[test]
    fn classify_skips_reparse_point() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        mk_target_root(&real);
        let link = tmp.path().join("link");
        symlink(&real, &link).unwrap();
        // A symlink (the cross-platform stand-in for a Windows junction) is a
        // reparse point → never reaped, unlinked-as-link only.
        assert_eq!(classify(&link, Duration::ZERO), Err(SkipReason::Reparse));
        // And the enumerator refuses to even consider a symlinked child.
        let cands = enumerate_candidates(tmp.path());
        assert!(
            !cands.iter().any(|c| c.path.ends_with("link")),
            "enumerator must skip reparse points"
        );
    }

    #[cfg(unix)]
    #[test]
    fn enumerate_skips_symlinked_container_child() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("elsewhere");
        mk_target_root(&real);
        let cont = tmp.path().join("_cargo-targets");
        fs::create_dir_all(&cont).unwrap();
        symlink(&real, cont.join("linked")).unwrap();
        let cands = enumerate_candidates(tmp.path());
        assert!(
            !cands.iter().any(|c| c.path.ends_with("linked")),
            "container-child reparse points must be skipped"
        );
    }

    #[test]
    fn classify_skips_live_build_window() {
        // A freshly-written artifact (live-build stand-in) under a real grace
        // window is skipped as GracePending — the mtime-based live guard.
        let tmp = tempfile::tempdir().unwrap();
        let tgt = tmp.path().join("t");
        mk_target_root(&tgt);
        fs::write(tgt.join("debug/deps/live.rlib"), b"x").unwrap();
        assert_eq!(
            classify(&tgt, Duration::from_secs(86_400)),
            Err(SkipReason::GracePending)
        );
    }

    #[test]
    fn classify_skips_via_keep_env_allowlist() {
        let tmp = tempfile::tempdir().unwrap();
        let tgt = tmp.path().join("target-wt-pinned");
        mk_target_root(&tgt);
        // Guard the process-global env: set, assert, restore.
        let prev = std::env::var(KEEP_ENV).ok();
        std::env::set_var(KEEP_ENV, "foo, target-wt-pinned ,bar");
        let verdict = classify(&tgt, Duration::ZERO);
        match prev {
            Some(v) => std::env::set_var(KEEP_ENV, v),
            None => std::env::remove_var(KEEP_ENV),
        }
        assert_eq!(verdict, Err(SkipReason::Kept));
    }

    #[test]
    fn remove_junction_safe_deletes_real_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let tgt = tmp.path().join("t");
        mk_target_root(&tgt);
        fs::write(tgt.join("debug/deps/a.o"), b"data").unwrap();
        assert!(tgt.exists());
        remove_junction_safe(&tgt).unwrap();
        assert!(!tgt.exists());
    }

    #[test]
    fn run_cycle_dry_run_removes_nothing() {
        // GAP 4: armed=false must perform ZERO removals — only count candidates.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = root.join("target-wt-old");
        let b = root.join("_cargo-targets/coord-old");
        mk_target_root(&a);
        mk_target_root_no_tag(&b);
        // Zero grace so both are otherwise reapable.
        let s = run_cycle(root, /* armed */ false, Duration::ZERO);
        assert_eq!(s.candidates, 2, "both roots are reap candidates");
        assert_eq!(s.reaped, 0, "dry-run reaps nothing");
        assert_eq!(s.reaped_bytes, 0);
        assert!(a.exists() && b.exists(), "dirs untouched in dry-run");
    }

    #[test]
    fn run_cycle_armed_removes_reapable_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = root.join("target-wt-old");
        mk_target_root(&a);
        fs::write(a.join("debug/deps/x.rlib"), b"data").unwrap();
        let s = run_cycle(root, /* armed */ true, Duration::ZERO);
        assert_eq!(s.candidates, 1);
        assert_eq!(s.reaped, 1);
        assert!(!a.exists(), "armed cycle removes the reapable root");
    }

    #[test]
    fn true_non_target_dir_is_rejected() {
        // The real `_wt-target` husk is a plain dir: no marker → never a target.
        let tmp = tempfile::tempdir().unwrap();
        let husk = tmp.path().join("_wt-target");
        fs::create_dir_all(husk.join("src")).unwrap();
        assert!(!is_target_root(&husk));
        assert!(!enumerate_candidates(tmp.path())
            .iter()
            .any(|c| c.path.ends_with("_wt-target")));
    }

    // ---- Windows-only: real junction (mklink /J) safety (GAP 3) ----
    #[cfg(windows)]
    fn mklink_junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &target.to_string_lossy(),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(windows)]
    #[test]
    fn windows_junction_is_never_reaped_and_unlinked_link_only() {
        let tmp = tempfile::tempdir().unwrap();
        // Real content behind a junction — the canonical tree a naive recursive
        // delete would destroy by following the link.
        let real = tmp.path().join("real-canonical");
        mk_target_root(&real);
        fs::write(real.join("debug/deps/precious.rlib"), b"do-not-delete").unwrap();
        let link = tmp.path().join("target-wt-junctioned");
        if !mklink_junction(&link, &real) {
            // Junction creation can require privilege; skip rather than fail if
            // the environment forbids it (developer symlink privilege off).
            eprintln!("skipping: mklink /J unavailable in this environment");
            return;
        }
        // is_junction must see it; classify must refuse it; enumerate must skip it.
        assert!(is_junction(&link));
        assert_eq!(classify(&link, Duration::ZERO), Err(SkipReason::Reparse));
        assert!(
            !enumerate_candidates(tmp.path())
                .iter()
                .any(|c| c.path.ends_with("target-wt-junctioned")),
            "enumerator must skip a real junction"
        );
        // remove_junction_safe on the junction unlinks the LINK only — the real
        // canonical tree behind it survives intact.
        remove_junction_safe(&link).unwrap();
        assert!(!link.exists(), "the junction link is removed");
        assert!(
            real.join("debug/deps/precious.rlib").exists(),
            "content behind the junction must NOT be followed/deleted"
        );
    }

    // ---- Windows-only: live-build lock guard positive half (GAP 4) ----
    #[cfg(windows)]
    #[test]
    fn cargo_lock_held_detects_exclusive_lock() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let tgt = tmp.path().join("t");
        mk_target_root(&tgt);
        let lock = tgt.join("debug/.cargo-lock");
        fs::write(&lock, b"").unwrap();
        // No holder → openable → not building.
        assert!(!cargo_lock_held(&tgt));
        // Open with share_mode=0 (exclusive), as a live cargo effectively does;
        // our probe's write-open then fails with a sharing violation → building.
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&lock)
            .unwrap();
        assert!(
            cargo_lock_held(&tgt),
            "an exclusively-held lock reads as building"
        );
    }
}
