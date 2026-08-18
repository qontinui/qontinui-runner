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
//! ## Scope — FOUR classes, disjoint by construction ([`TargetClass`])
//!
//! **This module used to be out-of-tree-only.** It declared repo checkouts and
//! linked worktrees out of scope specifically so it *"stays out-of-tree-only so
//! it can never race those guards"* (the worktree reclaim engine's Pinned /
//! dirty / G6 guards). That scope statement is **superseded** by the
//! disk-monitoring plan
//! (`plans/2026-08-07-product-disk-monitoring-and-cleanup.md`, Phase 2 step 3),
//! whose Phase 0 census measured the population the old scope could not see:
//!
//! | Class | Roots | Bytes | measured 2026-08-16 |
//! |---|---:|---:|---|
//! | in-repo-canonical (`.git` **dir**) | 41 | 1,669.8 GB | 47 % — the LARGEST class |
//! | sibling-worktree, linked (`.git` **file**) | 110 | 910.5 GB | 26 % |
//! | container (`_wt`, `_targets`, …) | 85 | 859.4 GB | 24 % |
//! | sibling-nongit | 8 | 131.2 GB | 4 % |
//!
//! Two of those four classes were entirely invisible to the old enumerator, and
//! together they hold **73 % of the bytes**. The enumerator now walks all four
//! and tags every root with its [`TargetClass`]; the boundary is enforced in the
//! **classifier**, not by refusing to look.
//!
//! ### The boundary — who owns which path (the race that used to be avoided by blindness)
//!
//! Descending into repos and worktrees puts this reaper in territory the
//! worktree reclaim engine and the build system also touch. The two engines are
//! kept disjoint **by path, not by ignorance**:
//!
//! | Path (relative to the enclosing checkout/worktree) | Owner | Verdict here |
//! |---|---|---|
//! | basename `target` (`<wt>/target`, `<wt>/src-tauri/target`) | worktree reclaim ([`super::reclaim`]) | [`SkipReason::OwnedByWorktreeReclaim`] — never a candidate |
//! | anything under `target-pool/`, or basename `target-agent` | the build system / `cargo-guard` | [`SkipReason::OwnedByBuildPool`] — never a candidate |
//! | any root inside a **canonical checkout** (`.git` is a dir) | nobody yet — v1 defers the verb | [`SkipReason::ReportOnly`] — measured and reported, never removed |
//! | a **renamed** in-worktree build dir in a linked worktree (`target-<slug>`, `target-sessrestore`, …) | THIS reaper | subject to G-dirty + the five gates |
//!
//! and, layered on top for anything inside a linked worktree:
//!
//!   * **G-dirty** — a worktree with uncommitted work has an
//!     [`SkipReason::WorktreeDirty`] verdict for every build dir inside it. This
//!     takes worktree-reclaim's inviolable G1 authority where it applies, using
//!     the SAME predicate ([`super::dirty::porcelain_is_dirty`]) so the two
//!     engines can never disagree about whether a tree is dirty.
//!
//! So the old "can never race those guards" property is **preserved and now
//! stated positively**: this reaper's verb reaches only paths no other engine
//! claims, and the paths another engine claims are still *measured and reported*
//! — which is the whole point of the survey ([`super::disk_survey`]).
//!
//! A "cargo target root" is identified positively by a cargo marker — a
//! `CACHEDIR.TAG` (newer cargo) OR a `.rustc_info.json` (present in EVERY cargo
//! target root, including the majority on this machine that have no tag). Either
//! is authoritative; a dir with neither is never treated as a target.
//!
//! ## Report mode (INV-D1) — measurement is NEVER gated on the deletion gates
//!
//! [`classify_candidate`] is **pure over its injected [`ClassifyOptions`]**: no
//! env read, no arming check, no deletion, and — the property this exists for —
//! **no preflight abort**. The motivating defect is `cargo-sweep-all.ps1
//! -WhatIf`, which aborts (`exit 2`) in preflight when any cargo/rustc process
//! is running, making the read-only report unobtainable during exactly the disk
//! emergency it is needed for. A preview that refuses to render is
//! indistinguishable from "nothing to reclaim".
//!
//! Here, a root with a build in flight is reported as
//! `blocked: `[`SkipReason::Building`] — a per-candidate verdict, never a
//! global refusal. [`super::disk_survey`] calls THIS classifier, so the preview
//! cannot drift from what the verb would actually do.
//!
//! ## Safety gates (each candidate must clear ALL — see [`classify_candidate`])
//!
//! The first three are the **boundary gates** added when the enumerator grew
//! past out-of-tree-only; they run BEFORE the original five, so a path another
//! engine owns is refused before any disk probing happens.
//!   * **G-report-only** — a root inside a canonical checkout (`.git` is a
//!     directory) has no v1 verb. Reported with its bytes; never removed, not
//!     even when armed.
//!   * **G-owner** — a root another engine owns by path (basename `target`,
//!     `target-agent`, or anything under `target-pool/`) is never a candidate
//!     here, so the two engines cannot race on one path.
//!   * **G-dirty** — a root inside a linked worktree with uncommitted work is
//!     never a candidate: worktree-reclaim's inviolable G1 applies wherever a
//!     worktree exists, and it is evaluated with the same predicate.
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
//!   * **Pruning WITHIN a live in-repo target dir.** The in-repo-canonical class
//!     is the largest by bytes (1.67 TB, 47 %) and is enumerated and reported
//!     here, but it has **no verb in v1**: removing a canonical `<repo>/target`
//!     whole is not what is wanted; pruning stale artifacts *inside* one needs
//!     its own guard design. Tracked as the plan's first follow-up.
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

/// Container dirs whose descendants may themselves be target roots
/// (`_wt/<slug>/target*`, `_targets/<slug>`, …).
const CONTAINER_DIRS: &[&str] = &[
    "_cargo-targets",
    "cargo-targets",
    "_targets",
    ".wt-targets",
    "_wt",
];

/// How deep below the workspace root the enumerator walks. Covers every layout
/// this fleet produces — `_wt/<slug>/src-tauri/target-x` is the deepest at 4 —
/// while keeping a mistaken root (a home dir, `C:\`) from becoming an unbounded
/// walk. Pruning at every discovered target root keeps the real cost far below
/// the bound.
const MAX_WALK_DEPTH: u32 = 4;

/// Hard ceiling on directories visited in one enumeration. Hitting it sets
/// [`Enumeration::truncated`], which the survey renders as an explicit
/// "this list is incomplete" — an under-count is never reported silently.
const MAX_DIRS_VISITED: usize = 200_000;

/// Directories the walk never descends into. `.git` holds no build artifacts;
/// the rest are large populations with no cargo target roots inside them, so
/// descending only costs time.
const NEVER_DESCEND: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".pnpm-store",
    ".turbo",
];

/// Build directories inside a checkout/worktree that belong to the **worktree
/// reclaim engine** ([`super::reclaim`] removes `<worktree>/target` and
/// `<worktree>/node_modules`). Matched on the BASENAME, so `<wt>/target` and
/// `<wt>/src-tauri/target` are both covered.
const WORKTREE_RECLAIM_BASENAME: &str = "target";

/// Build directories inside a checkout/worktree that belong to the **build
/// system** (`cargo-guard`'s shared agent dir and the supervisor's build pool).
/// `target-agent` is matched on the basename; `target-pool` on any path
/// component, so `target-pool/slot-0` and `target-pool/lkg` are both covered.
const BUILD_POOL_BASENAME: &str = "target-agent";
const BUILD_POOL_COMPONENT: &str = "target-pool";

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

/// Where a cargo target root lives, which decides **who owns it**.
///
/// The distinction between the first two is made on the `.git` entry's TYPE and
/// nothing else: a canonical checkout carries `.git` as a **directory**, a
/// linked git worktree carries it as a **file**. A `is_dir()` test alone
/// mis-assigns every linked worktree (110 of them on this machine, 910 GB), so
/// both forms are always tested — see [`git_marker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetClass {
    /// Inside a canonical repo checkout (`.git` is a directory). **Report-only
    /// in v1** — the largest class by bytes and the one whose verb is deferred.
    InRepoCanonical,
    /// Inside a linked git worktree (`.git` is a FILE pointing at the main
    /// repo's `worktrees/<name>` dir).
    SiblingWorktree,
    /// Under one of the known container dirs (`_wt`, `_targets`,
    /// `_cargo-targets`, `cargo-targets`, `.wt-targets`).
    Container,
    /// A bare out-of-tree root with no enclosing git repo at all
    /// (`target-wt-<slug>`, `cargo-target-coord`, …).
    SiblingNonGit,
}

impl TargetClass {
    /// Wire token — stable, kebab-case, matching the plan's census table.
    pub fn as_str(self) -> &'static str {
        match self {
            TargetClass::InRepoCanonical => "in-repo-canonical",
            TargetClass::SiblingWorktree => "sibling-worktree",
            TargetClass::Container => "container",
            TargetClass::SiblingNonGit => "sibling-nongit",
        }
    }

    /// Every class, in the plan's byte-descending order — so a summary can list
    /// a class with **zero** roots explicitly instead of omitting it (an absent
    /// class and an empty class must not render the same).
    pub fn all() -> [TargetClass; 4] {
        [
            TargetClass::InRepoCanonical,
            TargetClass::SiblingWorktree,
            TargetClass::Container,
            TargetClass::SiblingNonGit,
        ]
    }

    /// Whether this reaper's verb reaches this class at all in v1.
    /// `in-repo-canonical` is **report-only**: measured, surfaced, never removed.
    pub fn has_verb(self) -> bool {
        !matches!(self, TargetClass::InRepoCanonical)
    }
}

/// One enumerated cargo target root.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub class: TargetClass,
    /// Root of the enclosing checkout / linked worktree, when there is one.
    /// `None` for [`TargetClass::Container`] and [`TargetClass::SiblingNonGit`].
    pub repo_root: Option<PathBuf>,
}

/// Result of one enumeration pass. Carries the walk's own limits so an
/// incomplete answer can SAY it is incomplete.
#[derive(Debug, Clone, Default)]
pub struct Enumeration {
    pub candidates: Vec<Candidate>,
    /// Directories visited (bounded by [`MAX_DIRS_VISITED`]).
    pub dirs_visited: usize,
    /// The visit ceiling was hit — the candidate list is a PREFIX of the real
    /// population, not the whole of it.
    pub truncated: bool,
    /// Directories whose `read_dir` failed, with the reason. A failed read is
    /// reported, never folded into "nothing there".
    pub read_errors: Vec<(PathBuf, String)>,
}

/// What kind of `.git` entry a directory carries — the ONLY discriminator
/// between a canonical checkout and a linked worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitMarker {
    /// `.git` is a directory ⇒ canonical checkout.
    Dir,
    /// `.git` is a FILE (`gitdir: …`) ⇒ linked worktree.
    File,
}

fn git_marker(dir: &Path) -> Option<GitMarker> {
    match std::fs::symlink_metadata(dir.join(".git")) {
        Ok(m) if m.is_dir() => Some(GitMarker::Dir),
        Ok(m) if m.is_file() => Some(GitMarker::File),
        _ => None,
    }
}

/// The descent context — what the walker is currently inside.
#[derive(Debug, Clone)]
enum WalkCtx {
    /// The workspace root, or a plain directory under it that is neither a
    /// container nor a git repo.
    Loose,
    /// Under a known container dir.
    Container,
    /// Inside a checkout (`InRepoCanonical`) or a linked worktree
    /// (`SiblingWorktree`), rooted at the given path.
    Repo { root: PathBuf, class: TargetClass },
}

impl WalkCtx {
    fn class(&self) -> TargetClass {
        match self {
            WalkCtx::Loose => TargetClass::SiblingNonGit,
            WalkCtx::Container => TargetClass::Container,
            WalkCtx::Repo { class, .. } => *class,
        }
    }

    fn repo_root(&self) -> Option<PathBuf> {
        match self {
            WalkCtx::Repo { root, .. } => Some(root.clone()),
            _ => None,
        }
    }
}

/// Enumerate EVERY cargo target root under `root`, tagged with its
/// [`TargetClass`].
///
/// Walks all four classes — the two out-of-tree ones this module always saw,
/// plus the in-repo and in-worktree populations that hold 73 % of the bytes.
/// The safety boundary is enforced downstream in [`classify_candidate`], not by
/// refusing to look: a root another engine owns is *measured and reported*, and
/// separately refused a verb.
///
/// Invariants held by the walk itself:
///   * **never descends INTO a target root** — a discovered root is pruned, so
///     `target/debug/deps` can never be enumerated as a nested candidate;
///   * **never follows a reparse point** (junction or symlink), for either
///     descent or classification (`symlink_metadata` throughout);
///   * bounded by [`MAX_WALK_DEPTH`] and [`MAX_DIRS_VISITED`], and says so via
///     [`Enumeration::truncated`] when the bound bites.
pub fn enumerate_all(root: &Path) -> Enumeration {
    let mut e = Enumeration::default();
    walk(root, 0, &WalkCtx::Loose, &mut e);
    e
}

/// Back-compat facade: just the candidate list.
pub fn enumerate_candidates(root: &Path) -> Vec<Candidate> {
    enumerate_all(root).candidates
}

fn walk(dir: &Path, depth: u32, ctx: &WalkCtx, e: &mut Enumeration) {
    if depth > MAX_WALK_DEPTH || e.truncated {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            // A read that FAILED is not a directory that is EMPTY. Record it so
            // the survey can say the list is missing a branch.
            e.read_errors.push((dir.to_path_buf(), err.to_string()));
            return;
        }
    };
    for entry in entries.flatten() {
        if e.dirs_visited >= MAX_DIRS_VISITED {
            e.truncated = true;
            return;
        }
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ft = meta.file_type();
        if !ft.is_dir() || ft.is_symlink() || is_junction(&path) {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if NEVER_DESCEND.contains(&name) {
            continue;
        }
        e.dirs_visited += 1;

        // A target root terminates the branch — we never walk inside one.
        if is_target_root(&path) {
            e.candidates.push(Candidate {
                path,
                class: ctx.class(),
                repo_root: ctx.repo_root(),
            });
            continue;
        }

        // Otherwise decide what the child IS, then descend with that context.
        let child_ctx = if CONTAINER_DIRS.contains(&name) {
            WalkCtx::Container
        } else {
            match git_marker(&path) {
                Some(GitMarker::Dir) => WalkCtx::Repo {
                    root: path.clone(),
                    class: TargetClass::InRepoCanonical,
                },
                Some(GitMarker::File) => WalkCtx::Repo {
                    root: path.clone(),
                    class: TargetClass::SiblingWorktree,
                },
                // Not a repo boundary — stay in the caller's context, so a
                // target root under `qontinui-worktrees/<slug>/…` inherits the
                // right class instead of being mis-tagged at every level.
                None => ctx.clone(),
            }
        };
        walk(&path, depth + 1, &child_ctx, e);
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
pub(super) fn newest_artifact_age(target_root: &Path) -> Option<Duration> {
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
    measure_dir_size(dir).map(|m| m.bytes).unwrap_or(0)
}

/// A size measurement plus how trustworthy it is.
///
/// The plain `-> u64` form above cannot distinguish "0 bytes" from "could not
/// read", and a fabricated zero on a disk-reclaim preview is exactly the
/// dishonesty this feature exists to remove. Callers that RENDER a size use
/// this; the reaper's own logging keeps the scalar form.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SizeMeasurement {
    pub bytes: u64,
    /// Sub-directories whose `read_dir` failed. `> 0` ⇒ `bytes` is a LOWER
    /// BOUND, and the caller must say so rather than present it as exact.
    pub unreadable_dirs: usize,
}

/// Measure `dir` recursively, junction-safe. `None` iff `dir` itself could not
/// be read at all — an UNKNOWN size, never a zero.
pub(super) fn measure_dir_size(dir: &Path) -> Option<SizeMeasurement> {
    let read = std::fs::read_dir(dir).ok()?;
    let mut m = SizeMeasurement::default();
    for entry in read.flatten() {
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => {
                m.unreadable_dirs += 1;
                continue;
            }
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if is_junction(&path) {
                continue;
            }
            match measure_dir_size(&path) {
                Some(child) => {
                    m.bytes = m.bytes.saturating_add(child.bytes);
                    m.unreadable_dirs += child.unreadable_dirs;
                }
                None => m.unreadable_dirs += 1,
            }
        } else if ft.is_file() {
            m.bytes = m.bytes.saturating_add(meta.len());
        }
    }
    Some(m)
}

/// The reason a candidate was NOT reaped this tick (for shadow-mode logging and
/// for the report-mode survey's `blocked` items). Every refusal carries exactly
/// one — a candidate is never dropped silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// **G-report-only** — inside a canonical checkout. v1 measures this class
    /// (the largest by bytes) and deliberately gives it no verb.
    ReportOnly,
    /// **G-owner** — the worktree reclaim engine owns this path
    /// (`<worktree>/target`, `<worktree>/src-tauri/target`).
    OwnedByWorktreeReclaim,
    /// **G-owner** — the build system owns this path (`target-agent`, or
    /// anything under `target-pool/`).
    OwnedByBuildPool,
    /// **G-dirty** — the enclosing worktree has uncommitted work, so
    /// worktree-reclaim's inviolable G1 applies to everything inside it.
    WorktreeDirty,
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

impl SkipReason {
    /// Stable kebab-case wire token.
    pub fn token(self) -> &'static str {
        match self {
            SkipReason::ReportOnly => "report-only",
            SkipReason::OwnedByWorktreeReclaim => "owned-by-worktree-reclaim",
            SkipReason::OwnedByBuildPool => "owned-by-build-pool",
            SkipReason::WorktreeDirty => "worktree-dirty",
            SkipReason::Reparse => "reparse",
            SkipReason::Building => "building",
            SkipReason::GracePending => "grace-pending",
            SkipReason::Kept => "kept",
        }
    }

    /// One operator-facing sentence. Rendered next to the token so a refusal is
    /// legible without reading this source.
    pub fn detail(self) -> &'static str {
        match self {
            SkipReason::ReportOnly => {
                "Inside a canonical repo checkout. Measured and reported, but v1 has no cleanup \
                 verb for this class — removing a live `<repo>/target` whole is not wanted, and \
                 pruning inside one needs its own guard design."
            }
            SkipReason::OwnedByWorktreeReclaim => {
                "The worktree reclaim engine owns this path — it removes `<worktree>/target` and \
                 `<worktree>/node_modules` under its own Pinned/dirty/G6 guards. Reported here so \
                 the bytes are visible; never removed here, so the two engines cannot race."
            }
            SkipReason::OwnedByBuildPool => {
                "The build system owns this path (`target-agent` / `target-pool/<slot>`) — it is \
                 the shared agent build dir or a supervisor pool slot, not garbage."
            }
            SkipReason::WorktreeDirty => {
                "The enclosing worktree has uncommitted work. Worktree-reclaim's G1 is inviolable \
                 and applies to every build dir inside a dirty tree."
            }
            SkipReason::Reparse => {
                "A junction/symlink. Only the link would ever be unlinked, never the tree behind \
                 it, so it is not treated as reclaimable space."
            }
            SkipReason::Building => {
                "A build is in flight here (a held `.cargo-lock`, or an unreadable/unrecognised \
                 layout treated conservatively as live)."
            }
            SkipReason::GracePending => {
                "Idle, but the no-activity grace window has not elapsed yet."
            }
            SkipReason::Kept => {
                "Operator-pinned — a `.reap-keep` sentinel at its top level, or its name in \
                 `COORD_ORPHAN_TARGET_KEEP`. A pin is honoured even when armed."
            }
        }
    }
}

/// Everything [`classify_candidate`] needs, injected rather than read from the
/// ambient process. Constructing this is the ONLY place env is consulted, which
/// is what makes the classifier itself pure — and therefore callable in report
/// mode from an HTTP handler with no arming check and no preflight abort
/// (INV-D1).
#[derive(Clone)]
pub struct ClassifyOptions {
    /// Idle window a root must clear.
    pub grace: Duration,
    /// Basenames pinned by the operator (the `COORD_ORPHAN_TARGET_KEEP` list).
    pub keep_names: Vec<String>,
    /// Dirtiness probe for the enclosing worktree. Injectable so the boundary
    /// gate is unit-testable without a git repo; the production value is
    /// [`worktree_is_dirty`], the SAME predicate worktree-reclaim uses.
    pub worktree_is_dirty: fn(&Path) -> bool,
}

impl ClassifyOptions {
    /// Ambient configuration: env grace + env keep-list + the real git probe.
    pub fn from_env() -> Self {
        Self::new(grace())
    }

    /// Explicit grace, env keep-list, real git probe.
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            keep_names: keep_names_from_env(),
            worktree_is_dirty,
        }
    }
}

/// Parse `COORD_ORPHAN_TARGET_KEEP` into basenames. Absent ⇒ empty list.
fn keep_names_from_env() -> Vec<String> {
    std::env::var(KEEP_ENV)
        .ok()
        .map(|list| {
            list.split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// True iff `path` is a git worktree with reclaim-scoped uncommitted work.
/// Delegates to [`super::dirty::porcelain_is_dirty`] — the same predicate the
/// census and [`super::reclaim`] use, so the engines can never disagree.
///
/// **Fail-safe direction:** a git invocation that fails to RUN reads as dirty
/// (do not touch a tree we cannot reason about). Note this is the opposite of
/// [`super::reclaim::worktree_is_dirty`]'s `unwrap_or(false)`: there, a failed
/// probe leaves coord's own dirty verdict in charge; here there is no second
/// opinion, so the refusal has to be the default.
pub fn worktree_is_dirty(path: &Path) -> bool {
    let Some(path_str) = path.to_str() else {
        return true;
    };
    match crate::process_helpers::no_window("git")
        .args(["-C", path_str, "status", "--porcelain"])
        .output()
    {
        Ok(o) if o.status.success() => {
            super::dirty::porcelain_is_dirty(&String::from_utf8_lossy(&o.stdout))
        }
        _ => true,
    }
}

/// True iff this target root is operator-pinned: a `.reap-keep` sentinel at its
/// top level, or its basename in `keep_names`. Honored even when armed — a pin
/// is never overridden by arming.
fn is_kept(path: &Path, keep_names: &[String]) -> bool {
    if std::fs::symlink_metadata(path.join(KEEP_SENTINEL)).is_ok() {
        return true;
    }
    match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => keep_names.iter().any(|kept| kept == name),
        None => false,
    }
}

/// The **boundary gates** (G-report-only / G-owner / G-dirty). Pure over the
/// candidate's class and its path relative to the enclosing checkout, plus the
/// injected dirtiness probe.
///
/// Split out from the five disk gates because these are the ones that make
/// walking into repos and worktrees safe at all: they answer "does another
/// engine own this path?" without touching disk beyond the enclosing tree's
/// `git status`.
fn boundary_verdict(c: &Candidate, opts: &ClassifyOptions) -> Result<(), SkipReason> {
    let basename = c
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    if let Some(repo_root) = c.repo_root.as_deref() {
        // Inside a checkout or a linked worktree: another engine may own the
        // path outright. Checked BEFORE the class verdict so a `<repo>/target`
        // is attributed to its real owner rather than to "v1 has no verb".
        if basename == WORKTREE_RECLAIM_BASENAME {
            return Err(SkipReason::OwnedByWorktreeReclaim);
        }
        if basename == BUILD_POOL_BASENAME
            || c.path
                .strip_prefix(repo_root)
                .ok()
                .into_iter()
                .flat_map(|rel| rel.components())
                .any(|comp| comp.as_os_str() == BUILD_POOL_COMPONENT)
        {
            return Err(SkipReason::OwnedByBuildPool);
        }
    }

    // v1 gives the in-repo-canonical class no verb at all.
    if !c.class.has_verb() {
        return Err(SkipReason::ReportOnly);
    }

    // Worktree-reclaim's inviolable G1, applied wherever a worktree exists.
    if c.class == TargetClass::SiblingWorktree {
        if let Some(repo_root) = c.repo_root.as_deref() {
            if (opts.worktree_is_dirty)(repo_root) {
                return Err(SkipReason::WorktreeDirty);
            }
        }
    }
    Ok(())
}

/// Classify an enumerated candidate: `Ok(())` = reapable, `Err(reason)` = skip.
///
/// **Pure over `opts`** — no env read, no arming check, no deletion, and no
/// preflight abort. That is INV-D1 in code: [`super::disk_survey`] calls this
/// exact function in report mode, so the preview cannot drift from what the
/// verb would do, and a build in flight yields a per-candidate
/// [`SkipReason::Building`] rather than a refusal to answer.
///
/// Gate order is the safety contract: boundary gates (cheapest, no disk probing
/// beyond `git status`) first, then pin, then reparse, then the positive
/// artifact check, then the live-build and grace probes.
pub fn classify_candidate(c: &Candidate, opts: &ClassifyOptions) -> Result<(), SkipReason> {
    boundary_verdict(c, opts)?;
    classify_path(&c.path, opts)
}

/// The five original per-directory gates, for a path whose ownership boundary
/// has already been cleared (or that has none, being a bare out-of-tree root).
pub fn classify_path(path: &Path, opts: &ClassifyOptions) -> Result<(), SkipReason> {
    // Pin wins over everything, including arming.
    if is_kept(path, &opts.keep_names) {
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
        Some(age) if age < opts.grace => Err(SkipReason::GracePending),
        Some(_) => Ok(()),
    }
}

/// Convenience facade over [`classify_path`] for a bare out-of-tree root, with
/// the keep-list read from the ambient env.
pub fn classify(path: &Path, grace: Duration) -> Result<(), SkipReason> {
    classify_path(path, &ClassifyOptions::new(grace))
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

/// Bucket a skip reason into [`ReapSummary`]'s counters.
fn count_skip(summary: &mut ReapSummary, reason: SkipReason) {
    match reason {
        SkipReason::Kept => summary.skipped_kept += 1,
        SkipReason::Reparse => summary.skipped_reparse += 1,
        SkipReason::Building => summary.skipped_live += 1,
        SkipReason::GracePending => summary.skipped_grace += 1,
        SkipReason::ReportOnly => summary.skipped_report_only += 1,
        SkipReason::OwnedByWorktreeReclaim | SkipReason::OwnedByBuildPool => {
            summary.skipped_other_owner += 1
        }
        SkipReason::WorktreeDirty => summary.skipped_worktree_dirty += 1,
    }
}

/// One reaper cycle over `root` with explicit `armed`/`grace` (the testable
/// seam). Enumerates out-of-tree target roots, classifies each, and — only when
/// `armed` — removes the reapable ones junction-safely. When `!armed` it merely
/// logs "would reap" and removes NOTHING. Returns a summary.
pub fn run_cycle(root: &Path, armed: bool, grace: Duration) -> ReapSummary {
    let opts = ClassifyOptions::new(grace);
    let enumeration = enumerate_all(root);
    let mut summary = ReapSummary {
        scanned: enumeration.candidates.len(),
        truncated: enumeration.truncated,
        ..Default::default()
    };
    for c in enumeration.candidates {
        match classify_candidate(&c, &opts) {
            Err(reason) => count_skip(&mut summary, reason),
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
        "orphan_target_reaper: cycle armed={armed} scanned={} truncated={} candidates={} \
         candidate_gb={:.2} reaped={} reaped_gb={:.2} \
         skipped(live={},grace={},reparse={},kept={},report_only={},other_owner={},wt_dirty={}) \
         errors={}",
        summary.scanned,
        summary.truncated,
        summary.candidates,
        summary.candidate_bytes as f64 / 1_073_741_824.0,
        summary.reaped,
        summary.reaped_bytes as f64 / 1_073_741_824.0,
        summary.skipped_live,
        summary.skipped_grace,
        summary.skipped_reparse,
        summary.skipped_kept,
        summary.skipped_report_only,
        summary.skipped_other_owner,
        summary.skipped_worktree_dirty,
        summary.errors,
    );
    summary
}

/// Per-cycle telemetry.
#[derive(Debug, Default, Clone)]
pub struct ReapSummary {
    pub scanned: usize,
    /// The enumeration hit its visit ceiling — `scanned` is a lower bound.
    pub truncated: bool,
    pub candidates: usize,
    pub candidate_bytes: u64,
    pub reaped: usize,
    pub reaped_bytes: u64,
    pub skipped_live: usize,
    pub skipped_grace: usize,
    pub skipped_reparse: usize,
    pub skipped_kept: usize,
    /// In-repo-canonical roots: enumerated and measured, no v1 verb.
    pub skipped_report_only: usize,
    /// Owned by the worktree reclaim engine or the build pool.
    pub skipped_other_owner: usize,
    /// Inside a worktree with uncommitted work (G-dirty).
    pub skipped_worktree_dirty: usize,
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

    /// A cargo target root that has NO `CACHEDIR.TAG` — only a
    /// `.rustc_info.json` plus a `debug/` layout. This is the MAJORITY
    /// population on the real machine (`_cargo-targets/<slug>`,
    /// `_targets/<slug>`, `.wt-targets/<slug>`, …).
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
    fn enumerate_finds_every_class_and_tags_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Bare out-of-tree target root (direct child).
        mk_target_root(&root.join("target-wt-saf3"));
        // Container dir with a child root.
        mk_target_root(&root.join("_cargo-targets/blastfix"));
        // Nested container (_wt/<slug>/target).
        mk_target_root(&root.join("_wt/coord-x/target"));
        // A canonical repo checkout (`.git` is a DIRECTORY) with targets
        // inside. These used to be invisible; they are now enumerated and
        // tagged `in-repo-canonical` — the class the classifier then refuses.
        let repo = root.join("qontinui-runner");
        fs::create_dir_all(repo.join(".git/refs")).unwrap();
        mk_target_root(&repo.join("target"));
        mk_target_root(&repo.join("src-tauri/target"));

        let cands = enumerate_candidates(root);
        let by_name = |name: &str| {
            cands
                .iter()
                .find(|c| c.path.file_name().unwrap().to_string_lossy() == name)
                .unwrap_or_else(|| panic!("no candidate named {name}"))
        };
        assert_eq!(by_name("target-wt-saf3").class, TargetClass::SiblingNonGit);
        assert_eq!(by_name("blastfix").class, TargetClass::Container);
        // `_wt/coord-x/target` — a container child, not a git worktree.
        let wt_target: Vec<_> = cands
            .iter()
            .filter(|c| c.path.components().any(|comp| comp.as_os_str() == "_wt"))
            .collect();
        assert_eq!(wt_target.len(), 1, "_wt/coord-x/target must be found");
        assert_eq!(wt_target[0].class, TargetClass::Container);
        // Both in-repo targets are now FOUND, tagged, and attributed to their
        // checkout — the visibility Phase 0 measured at 1.67 TB.
        let in_repo: Vec<_> = cands
            .iter()
            .filter(|c| c.class == TargetClass::InRepoCanonical)
            .collect();
        assert_eq!(
            in_repo.len(),
            2,
            "<repo>/target and <repo>/src-tauri/target"
        );
        for c in in_repo {
            assert_eq!(c.repo_root.as_deref(), Some(repo.as_path()));
        }
    }

    #[test]
    fn a_linked_worktree_is_detected_by_its_dot_git_file() {
        // The `.git`-as-FILE form is the ONLY discriminator between a linked
        // worktree and a canonical checkout. A `is_dir()` test alone
        // mis-assigns all 110 linked worktrees Phase 0 measured (910 GB).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let linked = root.join("qontinui-runner-wt-a");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join(".git"), b"gitdir: /repo/.git/worktrees/a\n").unwrap();
        mk_target_root(&linked.join("target-a"));

        let cands = enumerate_candidates(root);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].class, TargetClass::SiblingWorktree);
        assert_eq!(cands[0].repo_root.as_deref(), Some(linked.as_path()));
    }

    #[test]
    fn in_repo_canonical_is_report_only_even_at_zero_grace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let repo = root.join("some-repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        mk_target_root(&repo.join("target-renamed"));
        let opts = ClassifyOptions {
            grace: Duration::ZERO,
            keep_names: Vec::new(),
            worktree_is_dirty: |_| false,
        };
        let cands = enumerate_candidates(root);
        assert_eq!(cands.len(), 1);
        assert_eq!(
            classify_candidate(&cands[0], &opts),
            Err(SkipReason::ReportOnly),
            "v1 gives the largest class no verb — not even when armed"
        );
    }

    #[test]
    fn boundary_gates_refuse_paths_other_engines_own() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root.join("repo-wt-x");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), b"gitdir: /repo/.git/worktrees/x\n").unwrap();
        mk_target_root(&wt.join("target"));
        mk_target_root(&wt.join("target-agent"));
        mk_target_root(&wt.join("target-pool/slot-0"));
        mk_target_root(&wt.join("target-renamed"));

        let opts = ClassifyOptions {
            grace: Duration::ZERO,
            keep_names: Vec::new(),
            worktree_is_dirty: |_| false,
        };
        let verdict = |name: &str| {
            let c = enumerate_candidates(root)
                .into_iter()
                .find(|c| c.path.ends_with(name))
                .unwrap_or_else(|| panic!("no candidate {name}"));
            classify_candidate(&c, &opts)
        };
        assert_eq!(
            verdict("target"),
            Err(SkipReason::OwnedByWorktreeReclaim),
            "<worktree>/target belongs to the worktree reclaim engine"
        );
        assert_eq!(verdict("target-agent"), Err(SkipReason::OwnedByBuildPool));
        assert_eq!(verdict("slot-0"), Err(SkipReason::OwnedByBuildPool));
        // The renamed in-worktree build dir IS this reaper's territory — the
        // population the module doc used to declare out of scope.
        assert_eq!(verdict("target-renamed"), Ok(()));
    }

    #[test]
    fn a_dirty_worktree_blocks_its_renamed_build_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root.join("repo-wt-dirty");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), b"gitdir: /repo/.git/worktrees/d\n").unwrap();
        mk_target_root(&wt.join("target-renamed"));
        let c = enumerate_candidates(root).into_iter().next().unwrap();

        let clean = ClassifyOptions {
            grace: Duration::ZERO,
            keep_names: Vec::new(),
            worktree_is_dirty: |_| false,
        };
        let dirty = ClassifyOptions {
            worktree_is_dirty: |_| true,
            ..clean.clone()
        };
        assert_eq!(classify_candidate(&c, &clean), Ok(()));
        assert_eq!(
            classify_candidate(&c, &dirty),
            Err(SkipReason::WorktreeDirty),
            "worktree-reclaim's G1 is inviolable wherever a worktree exists"
        );
    }

    #[test]
    fn skip_reasons_have_distinct_tokens_and_details() {
        let all = [
            SkipReason::ReportOnly,
            SkipReason::OwnedByWorktreeReclaim,
            SkipReason::OwnedByBuildPool,
            SkipReason::WorktreeDirty,
            SkipReason::Reparse,
            SkipReason::Building,
            SkipReason::GracePending,
            SkipReason::Kept,
        ];
        let tokens: std::collections::BTreeSet<&str> = all.iter().map(|r| r.token()).collect();
        assert_eq!(tokens.len(), all.len(), "every reason needs its own token");
        for r in all {
            assert!(!r.detail().is_empty(), "{:?} needs an operator sentence", r);
        }
    }

    #[test]
    fn walk_never_descends_into_a_target_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outer = root.join("target-outer");
        mk_target_root(&outer);
        // A marker dir INSIDE a discovered root must never become a second
        // candidate — the walk prunes at the root.
        mk_target_root(&outer.join("debug/inner"));
        let cands = enumerate_candidates(root);
        assert_eq!(cands.len(), 1);
        assert!(cands[0].path.ends_with("target-outer"));
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
