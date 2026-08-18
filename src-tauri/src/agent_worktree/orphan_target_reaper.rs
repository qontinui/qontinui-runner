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
//! | Path | Owner | Verdict here |
//! |---|---|---|
//! | basename `target` at ANY depth inside a checkout/worktree (`<wt>/target`, `<wt>/src-tauri/target`, `_wt/<slug>/target`) | worktree reclaim ([`super::reclaim`]) | [`SkipReason::OwnedByWorktreeReclaim`] — never a candidate |
//! | basename `target` with NO enclosing checkout at all (`D:/scratch/target`) | nobody — `super::reclaim` only ever removes `<worktree>/target` for a worktree in its census | falls through to the ordinary gates; a genuine orphan keeps a verb |
//! | anything under `target-pool/`, or basename `target-agent`, ANYWHERE | the build system / `cargo-guard` | [`SkipReason::OwnedByBuildPool`] — never a candidate |
//! | any root inside a **canonical checkout** (`.git` is a dir) | nobody yet — v1 defers the verb | [`SkipReason::ReportOnly`] — measured and reported, never removed |
//! | a root whose enclosing dir's `.git` could not be READ | UNKNOWN | [`SkipReason::OwnershipUnknown`] — never a candidate |
//! | a **renamed** in-worktree build dir in a linked worktree (`target-<slug>`, `target-sessrestore`, …) | THIS reaper | subject to G-dirty + the five gates |
//!
//! The build-pool row is matched **path-globally** — on the basename, or on any
//! component of the absolute path — and NOT on the path relative to a resolved
//! enclosing checkout, so no read failure anywhere can dissolve it. The
//! worktree-reclaim row is matched on the basename at any DEPTH (never on a
//! path relative to the checkout root), which is the property that matters:
//! `<wt>/src-tauri/target` matches as surely as `<wt>/target`. It is scoped to
//! roots that HAVE an enclosing checkout, because that is the only shape
//! `super::reclaim` ever acts on — a `target` with no repo around it is nobody's,
//! and claiming an owner for it is a fabricated observation of exactly the kind
//! this module refuses everywhere else. A root whose enclosing `.git` could not
//! be read never reaches that gate: it fails **closed** one step earlier
//! ([`SkipReason::OwnershipUnknown`]): `Ok(absent)` and `Err(unreadable)` are
//! distinguished by [`git_probe`], and only the first means "not a repo".
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
//!   * **G-owner** — a root another engine owns by path (basename `target`
//!     inside a checkout/worktree, `target-agent`, or anything under
//!     `target-pool/`) is never a candidate here, so the two engines cannot race
//!     on one path. Matched on the basename at any DEPTH — never on a path
//!     relative to a resolved checkout root, which a failed `.git` read would
//!     take away.
//!   * **G-ownership-unknown** — a root whose enclosing directory carries a
//!     `.git` entry that could not be READ. Whether another engine owns it is
//!     unknown, so it is refused ([`SkipReason::OwnershipUnknown`]).
//!   * **G-dirty** — a root inside a linked worktree with uncommitted work is
//!     never a candidate: worktree-reclaim's inviolable G1 applies wherever a
//!     worktree exists, and it is evaluated with the same predicate. A dirty
//!     probe that could not COMPLETE is refused too, under its own distinct
//!     reason ([`SkipReason::DirtyUnknown`]) — a refusal on a missing reading
//!     must never render as an observation of uncommitted work.
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
//!     a container never qualifies. Its refusal is
//!     [`SkipReason::UnrecognizedLayout`], NOT "a build is in flight".
//!   * **G-live** — not currently building: no held `.cargo-lock` under any
//!     profile dir, AND deepest build-artifact mtime older than the grace window.
//!     An unreadable mtime is refused as [`SkipReason::ActivityUnknown`], and a
//!     `.cargo-lock` probe that could not complete as
//!     [`SkipReason::LockStateUnknown`] — same fail-safe direction, but neither
//!     claims to have seen a build.
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
//! ### Shadow-window prerequisites BEFORE that flag is ever flipped
//!
//! `COORD_ORPHAN_TARGET_REAP_ENABLED` is a **pre-existing** flag whose blast
//! radius GREW when the enumerator stopped being out-of-tree-only. The same
//! unchanged flag now also reaches renamed build dirs inside linked worktrees
//! and under container dirs. Two gaps must be closed, or consciously accepted in
//! writing, before it is armed:
//!
//!   1. **coord's retention PIN is not honoured here.** [`super::reclaim`]
//!      refuses a worktree whose coord ledger row says `retention='pinned'`
//!      (`reclaim.rs`, the `Pinned` guard). This module has no ledger and no
//!      coord round-trip, so the only pins it can see are the ledger-less ones:
//!      the `.reap-keep` sentinel and `COORD_ORPHAN_TARGET_KEEP`. Concretely: a
//!      coord-PINNED worktree's `target-<slug>` **is reapable here**. The module
//!      doc above takes reclaim's G1 (dirty) but is deliberately silent on its
//!      Pinned guard, and that asymmetry is a gap, not a design. Close it by
//!      consulting the pinned set (the census already knows it) or by requiring
//!      the operator to drop `.reap-keep` into every pinned worktree first.
//!   2. **The shadow window must be reviewed from a summary that cannot fake a
//!      zero.** [`run_cycle`]'s operator-facing "would reap" lines report an
//!      unreadable root as `size UNKNOWN`, never `0.00 GB` — the review is only
//!      as good as that line, so it must never under-state what arming would do.
//!
//! ## Posture
//!
//! Machine-wide, best-effort periodic poller (default 900s, env
//! `QONTINUI_ORPHAN_TARGET_INTERVAL_SECS`, floored 60s). No coord round-trip and
//! no identity needed — purely local disk hygiene. A failing tick `warn!`s and
//! retries; the loop never panics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
/// both forms are always tested — see [`git_probe`].
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
    /// The enclosing directory carries a `.git` entry that could not be READ, so
    /// `class` / `repo_root` are a GUESS. Every gate that keys on either must
    /// fail closed — see [`SkipReason::OwnershipUnknown`].
    pub enclosing_git_unreadable: bool,
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
    /// Reads that FAILED, with the reason: a directory whose `read_dir` could
    /// not be opened, or a `.git` entry whose stat failed (the probe that
    /// decides ownership). A failed read is reported, never folded into
    /// "nothing there".
    pub read_errors: Vec<(PathBuf, String)>,
    /// Directory ENTRIES that errored mid-iteration, after their `read_dir`
    /// opened successfully. Distinct from [`Self::read_errors`], which records
    /// only open failures: a directory can open and then fail per entry, and
    /// `entries.flatten()` would drop those silently. `> 0` ⇒ this walk saw
    /// less than the directory holds.
    pub entry_errors: usize,
    /// Directories NOT descended into because [`MAX_WALK_DEPTH`] was reached. A
    /// target root below the bound is simply absent from `candidates`, so the
    /// bound has to be counted or its effect is invisible.
    pub depth_limited_dirs: usize,
    /// Reparse points (junctions / symlinks) the walk refused to follow. They
    /// appear neither as candidates nor as errors — by design, since following
    /// one would double-count the tree behind it — so they are counted here
    /// rather than vanishing.
    pub reparse_dirs_skipped: usize,
}

/// What a directory's `.git` entry says — the ONLY discriminator between a
/// canonical checkout and a linked worktree, plus the two NON-answers that must
/// never be collapsed into each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitProbe {
    /// `.git` is a directory ⇒ canonical checkout.
    Dir,
    /// `.git` is a FILE (`gitdir: …`) ⇒ linked worktree.
    File,
    /// There is no `.git` here — a POSITIVE "not a repo boundary".
    Absent,
    /// A `.git` entry exists (or the stat failed for a reason other than
    /// not-found) but its kind could not be established. **Not the same as
    /// absent**: the old code mapped every error to `None`, so a permissions
    /// blip or a worktree mid-`git worktree add`/`remove` silently demoted a
    /// linked worktree to a plain directory — and with it dropped both owner
    /// gates and G-dirty.
    Unreadable,
}

fn git_probe(dir: &Path) -> GitProbe {
    match std::fs::symlink_metadata(dir.join(".git")) {
        Ok(m) if m.is_dir() => GitProbe::Dir,
        Ok(m) if m.is_file() => GitProbe::File,
        // An entry that is neither (a symlinked `.git`, a device node): it
        // exists, so this is not "no repo here" — we simply cannot say which.
        Ok(_) => GitProbe::Unreadable,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => GitProbe::Absent,
        Err(_) => GitProbe::Unreadable,
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
    /// Inside a directory whose `.git` entry could not be read. We do not know
    /// whether it is a canonical checkout, a linked worktree, or not a repo at
    /// all. `class` is the parent's — a guess kept only so the item still
    /// renders — and everything inside fails closed.
    OpaqueRepo { root: PathBuf, class: TargetClass },
}

impl WalkCtx {
    fn class(&self) -> TargetClass {
        match self {
            WalkCtx::Loose => TargetClass::SiblingNonGit,
            WalkCtx::Container => TargetClass::Container,
            WalkCtx::Repo { class, .. } | WalkCtx::OpaqueRepo { class, .. } => *class,
        }
    }

    fn repo_root(&self) -> Option<PathBuf> {
        match self {
            WalkCtx::Repo { root, .. } | WalkCtx::OpaqueRepo { root, .. } => Some(root.clone()),
            _ => None,
        }
    }

    /// True inside a directory whose `.git` could not be read.
    fn git_unreadable(&self) -> bool {
        matches!(self, WalkCtx::OpaqueRepo { .. })
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
    if e.truncated {
        return;
    }
    if depth > MAX_WALK_DEPTH {
        // The bound is a design decision, not a failure — but a target root
        // below it is simply ABSENT from the answer, so it is counted rather
        // than dropped silently.
        e.depth_limited_dirs += 1;
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
    for entry in entries {
        if e.dirs_visited >= MAX_DIRS_VISITED {
            e.truncated = true;
            return;
        }
        // A directory that OPENED can still error per entry. `flatten()` used to
        // discard those, so the walk under-reported with no marker at all.
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                e.entry_errors += 1;
                continue;
            }
        };
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                e.entry_errors += 1;
                continue;
            }
        };
        let ft = meta.file_type();
        if ft.is_symlink() || is_junction(&path) {
            // Never followed — a junctioned target root would double-count the
            // tree behind it — but never silent either. Counted only when it
            // resolves to a DIRECTORY (`is_dir` follows the link, unlike the
            // `symlink_metadata` above), i.e. only when it is a branch the walk
            // would otherwise have descended into.
            if path.is_dir() {
                e.reparse_dirs_skipped += 1;
            }
            continue;
        }
        if !ft.is_dir() {
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
                enclosing_git_unreadable: ctx.git_unreadable(),
            });
            continue;
        }

        // Otherwise decide what the child IS, then descend with that context.
        let child_ctx = if CONTAINER_DIRS.contains(&name) {
            match ctx {
                // Opacity SURVIVES the container transition. A plain
                // `WalkCtx::Container` here would drop `git_unreadable`, so a
                // target root directly under `<opaque-repo>/_wt/` would lose the
                // unknown flag and be classified as if ownership had been
                // established — the fail-closed boundary dissolved by a rename
                // one level up. No such layout exists on this machine today;
                // preserving it costs nothing and removes the trap.
                WalkCtx::OpaqueRepo { root, .. } => WalkCtx::OpaqueRepo {
                    root: root.clone(),
                    class: TargetClass::Container,
                },
                _ => WalkCtx::Container,
            }
        } else {
            match git_probe(&path) {
                GitProbe::Dir => WalkCtx::Repo {
                    root: path.clone(),
                    class: TargetClass::InRepoCanonical,
                },
                GitProbe::File => WalkCtx::Repo {
                    root: path.clone(),
                    class: TargetClass::SiblingWorktree,
                },
                // Not a repo boundary — stay in the caller's context, so a
                // target root under `qontinui-worktrees/<slug>/…` inherits the
                // right class instead of being mis-tagged at every level.
                GitProbe::Absent => ctx.clone(),
                // A `.git` we could not read is NOT an absent one. Descend with
                // an opaque context so everything inside fails closed, and
                // record the failed read so the survey can say so.
                GitProbe::Unreadable => {
                    e.read_errors.push((
                        path.join(".git"),
                        "the `.git` entry could not be read, so this directory's ownership \
                         (canonical checkout / linked worktree / not a repo) is UNKNOWN"
                            .to_string(),
                    ));
                    WalkCtx::OpaqueRepo {
                        root: path.clone(),
                        class: ctx.class(),
                    }
                }
            }
        };
        walk(&path, depth + 1, &child_ctx, e);
    }
}

/// What the `.cargo-lock` probe managed to establish. A tri-state for the same
/// reason [`DirtyProbe`] is one: "a build is in flight" is an OBSERVATION, and a
/// probe that could not read the disk has not made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockState {
    /// A `.cargo-lock` is held — a write-open failed the way a live cargo's
    /// sharing violation does.
    Held,
    /// Every profile dir was enumerated and no lock is held.
    Free,
    /// A read the probe depends on FAILED, so whether a lock is held is unknown.
    Unknown,
}

/// Whether a `.cargo-lock` under any profile dir of `target_root` is held by a
/// live cargo invocation (Windows: a write-open fails with a sharing violation).
/// Mirrors [`super::reclaim`]'s `cargo_lock_held`, but tri-state.
///
/// **Fail-closed, without fabricating an observation.** Every failed READ
/// answers [`LockState::Unknown`], which the caller refuses exactly like `Held`
/// — the refusal is identical, the CLAIM is not. Returning `Held` from a failed
/// `read_dir`/stat is what the first round-1 fix did, and it filed a permissions
/// blip under `skipped_live` ("a build is in flight here") on the very line the
/// arming decision is reviewed from.
///
/// The write-open failure stays `Held` on purpose: it is not an incidental read
/// error, it IS the sharing-violation probe. Windows reports the violation with
/// the same `ErrorKind` a permissions denial would, so the two are not
/// separable here — and a lock file that exists and cannot be opened for write
/// is the positive signal this function is built around.
fn cargo_lock_held(target_root: &Path) -> LockState {
    let mut profile_dirs: Vec<PathBuf> =
        vec![target_root.join("debug"), target_root.join("release")];
    // A root we cannot enumerate may hold a renamed profile dir with a live
    // lock, so the probe is incomplete — UNKNOWN, not "no lock here", and not a
    // build we watched start.
    let Ok(entries) = std::fs::read_dir(target_root) else {
        return LockState::Unknown;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return LockState::Unknown;
        };
        let p = entry.path();
        if p.is_dir() {
            profile_dirs.push(p);
        }
    }
    for dir in profile_dirs {
        let lock = dir.join(".cargo-lock");
        match std::fs::symlink_metadata(&lock) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            // The lock file's existence itself could not be established.
            Err(_) => return LockState::Unknown,
            Ok(_) => {}
        }
        match std::fs::OpenOptions::new().write(true).open(&lock) {
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return LockState::Held,
        }
    }
    LockState::Free
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
        for entry in entries {
            // An entry that errors mid-iteration could be the NEWEST artifact.
            // Dropping it would age the root and could reap a live build, so an
            // unreadable entry makes the whole answer UNKNOWN.
            let Ok(entry) = entry else {
                return None;
            };
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
    for entry in read {
        // An entry that errors after a successful open would otherwise be
        // dropped, silently shrinking the total. Mark it instead.
        let Ok(entry) = entry else {
            m.unreadable_dirs += 1;
            continue;
        };
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
    /// **G-ownership-unknown** — the enclosing directory's `.git` entry could
    /// not be read, so which engine (if any) owns this path is UNKNOWN. Refused,
    /// because a boundary you cannot see is not a boundary you may cross.
    OwnershipUnknown,
    /// **G-dirty** — the enclosing worktree has uncommitted work, so
    /// worktree-reclaim's inviolable G1 applies to everything inside it.
    WorktreeDirty,
    /// **G-dirty, failed closed** — the `git status` probe could not be
    /// completed, so whether the enclosing worktree is dirty is UNKNOWN. The
    /// refusal is the same; the CLAIM is not. Kept distinct from
    /// [`Self::WorktreeDirty`] because "there is uncommitted work here" is a
    /// statement about the operator's tree that a failed probe cannot make.
    DirtyUnknown,
    /// The root (or a nested dir being deleted) is a reparse point — never
    /// followed; only the link is ever unlinked, never recursed into.
    Reparse,
    /// A live build owns it: a `.cargo-lock` is held under one of the profile
    /// dirs. An OBSERVATION — a probe that could not read the disk reports
    /// [`Self::LockStateUnknown`] instead.
    Building,
    /// **G-live, failed closed** — a read the `.cargo-lock` probe depends on
    /// failed (`read_dir` of the root, a directory entry, or the lock's own
    /// stat), so whether a build holds this root is UNKNOWN. Refused exactly
    /// like [`Self::Building`]; kept distinct because "a build is in flight
    /// here" is a statement about the operator's machine that a failed read
    /// cannot make — and because folding it into the `skipped_live` counter
    /// would let a permissions blip inflate the live-build tally the shadow
    /// window's arming decision is read from.
    LockStateUnknown,
    /// Not positively recognisable as a rebuildable cargo target — a cargo
    /// marker but no `debug`/`release` profile layout. Never removed, and
    /// deliberately NOT reported as a build in flight.
    UnrecognizedLayout,
    /// The newest build-artifact mtime could not be read, so how long this root
    /// has been idle is UNKNOWN. Treated as live and refused — but it does not
    /// claim a build was observed.
    ActivityUnknown,
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
            SkipReason::OwnershipUnknown => "ownership-unknown",
            SkipReason::WorktreeDirty => "worktree-dirty",
            SkipReason::DirtyUnknown => "worktree-dirty-unknown",
            SkipReason::Reparse => "reparse",
            SkipReason::Building => "building",
            SkipReason::LockStateUnknown => "lock-state-unknown",
            SkipReason::UnrecognizedLayout => "unrecognized-layout",
            SkipReason::ActivityUnknown => "activity-unknown",
            SkipReason::GracePending => "grace-pending",
            SkipReason::Kept => "kept",
        }
    }

    /// True when this refusal is a statement of IGNORANCE — a read that failed —
    /// rather than an observed fact about the path. The distinction is the whole
    /// honesty contract of this feature: a probe that could not run must never
    /// render as an observation.
    pub fn is_unknown(self) -> bool {
        matches!(
            self,
            SkipReason::OwnershipUnknown
                | SkipReason::DirtyUnknown
                | SkipReason::ActivityUnknown
                | SkipReason::LockStateUnknown
        )
    }

    /// True when the refusal is "another engine owns this path" (or "we cannot
    /// tell which engine does"). These are the reasons that must strip this
    /// reaper's verb from an item — the verb field names WHO would act, so
    /// claiming a path we are refused is claiming another engine's territory.
    pub fn owned_elsewhere(self) -> bool {
        matches!(
            self,
            SkipReason::OwnedByWorktreeReclaim
                | SkipReason::OwnedByBuildPool
                | SkipReason::OwnershipUnknown
                | SkipReason::ReportOnly
        )
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
                "`target` inside a checkout or linked worktree is the canonical build-dir name the \
                 worktree reclaim engine owns — it removes `<worktree>/target` and \
                 `<worktree>/node_modules` under its own Pinned/dirty/G6 guards. This reaper's \
                 territory is RENAMED build dirs, so it never claims a dir named `target` at any \
                 depth inside a repo. Reported here so the bytes are visible; never removed here, \
                 so the two engines cannot race."
            }
            SkipReason::OwnedByBuildPool => {
                "The build system owns this name (`target-agent` / anything under \
                 `target-pool/`) — the shared agent build dir or a supervisor pool slot, not \
                 garbage. Matched on the path alone, so the pool is protected by what it IS \
                 rather than by where it currently happens to live."
            }
            SkipReason::OwnershipUnknown => {
                "The enclosing directory carries a `.git` entry that could NOT be read, so whether \
                 this path belongs to a linked worktree (and therefore to worktree-reclaim's \
                 guards) is unknown. Refused — this is a missing reading, not a finding that the \
                 path is free."
            }
            SkipReason::WorktreeDirty => {
                "The enclosing worktree has uncommitted work. Worktree-reclaim's G1 is inviolable \
                 and applies to every build dir inside a dirty tree."
            }
            SkipReason::DirtyUnknown => {
                "The enclosing worktree's `git status` probe could NOT be completed (it failed to \
                 run, timed out, or exited non-zero), so whether the tree has uncommitted work is \
                 unknown. Treated as dirty and refused — a refusal on a missing reading, NOT an \
                 observation of uncommitted work."
            }
            SkipReason::Reparse => {
                "A junction/symlink. Only the link would ever be unlinked, never the tree behind \
                 it, so it is not treated as reclaimable space."
            }
            SkipReason::Building => {
                "A build is in flight here — a `.cargo-lock` is held under one of the profile \
                 dirs."
            }
            SkipReason::LockStateUnknown => {
                "The `.cargo-lock` probe could NOT complete (the root or one of its entries could \
                 not be listed, or the lock file's own stat failed), so whether a build holds \
                 this root is unknown. Treated as live and refused — a refusal on a missing \
                 reading, NOT an observed build."
            }
            SkipReason::UnrecognizedLayout => {
                "Not positively recognisable as a rebuildable cargo target: it carries a cargo \
                 marker but no `debug`/`release` profile layout. Deletion here is contingent on a \
                 dir LOOKING like a built target, never on its name — so this is refused. Nothing \
                 here says a build is running."
            }
            SkipReason::ActivityUnknown => {
                "The newest build-artifact mtime under this root could not be read, so how long it \
                 has been idle is unknown. Treated as live and refused — a refusal on a missing \
                 reading, NOT an observed build."
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
    pub worktree_is_dirty: fn(&Path) -> DirtyProbe,
    /// Per-`ClassifyOptions` memo of the dirty probe, keyed by repo root.
    ///
    /// Load-bearing, not an optimisation. [`boundary_verdict`] runs per
    /// CANDIDATE, and this fleet has ~110 linked worktrees each holding several
    /// build dirs — without a memo one survey walk (and one 900 s reaper cycle)
    /// spawns hundreds of `git status` processes, and a single slow repo is paid
    /// for once per candidate inside it. Since a walk publishes one snapshot, a
    /// worktree's dirtiness is read ONCE per walk by construction: the
    /// alternative — re-probing mid-walk — would also make the snapshot
    /// internally inconsistent.
    dirty_memo: Arc<Mutex<HashMap<PathBuf, DirtyProbe>>>,
}

/// The three answers a dirtiness probe can give. A tri-state because the
/// two-state version could not distinguish "this tree has uncommitted work" from
/// "the probe did not complete" — and [`SkipReason::WorktreeDirty`] rendered the
/// second as the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyProbe {
    Clean,
    Dirty,
    /// The probe failed to run, timed out, or exited non-zero.
    Unknown,
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
            dirty_memo: Arc::default(),
        }
    }

    /// Explicit grace and an injected dirty probe, with an empty keep-list — the
    /// seam the boundary-gate tests drive, so they never need a real git repo.
    pub fn with_probe(grace: Duration, worktree_is_dirty: fn(&Path) -> DirtyProbe) -> Self {
        Self {
            grace,
            keep_names: Vec::new(),
            worktree_is_dirty,
            dirty_memo: Arc::default(),
        }
    }

    /// The memoised dirty probe. A poisoned lock degrades to an un-memoised
    /// probe rather than to an answer — never to a fabricated `Clean`.
    fn dirty(&self, repo_root: &Path) -> DirtyProbe {
        if let Ok(memo) = self.dirty_memo.lock() {
            if let Some(v) = memo.get(repo_root) {
                return *v;
            }
        }
        let verdict = (self.worktree_is_dirty)(repo_root);
        if let Ok(mut memo) = self.dirty_memo.lock() {
            memo.insert(repo_root.to_path_buf(), verdict);
        }
        verdict
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

/// How long the `git status` dirtiness probe may run before it is killed and
/// answered [`DirtyProbe::Unknown`].
///
/// An unbounded `.output()` is what this replaces. The survey walk and the
/// reaper cycle both run inside a task that must finish: a single hung `git`
/// would pin `disk_survey::BUILD_ACTIVE` forever, which makes `spawn_rebuild` a
/// permanent no-op, freezes the snapshot, and leaves `census_refreshing: true` —
/// INV-D1's failure mode reached through a different door.
const DIRTY_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Poll cadence while waiting for the probe.
const DIRTY_PROBE_POLL: Duration = Duration::from_millis(25);

/// Whether `path` is a git worktree with reclaim-scoped uncommitted work.
/// Delegates to [`super::dirty::porcelain_is_dirty`] — the same predicate the
/// census and [`super::reclaim`] use, so the engines can never disagree.
///
/// **Fail-safe direction:** anything short of a clean, successful, in-time run
/// answers [`DirtyProbe::Unknown`], which [`boundary_verdict`] refuses exactly
/// like `Dirty` — do not touch a tree we cannot reason about. Note this is the
/// opposite of [`super::reclaim::worktree_is_dirty`]'s `unwrap_or(false)`:
/// there, a failed probe leaves coord's own dirty verdict in charge; here there
/// is no second opinion, so the refusal has to be the default. The tri-state is
/// what keeps that refusal from CLAIMING uncommitted work it never saw.
///
/// Bounded by [`DIRTY_PROBE_TIMEOUT`].
///
/// **stdout is drained on its own thread**, and that is not cosmetic. This used
/// to poll `try_wait` with nobody reading the pipe, so a tree with more than the
/// ~64 KB pipe buffer of porcelain output blocked git on its own `write` — it
/// could never exit, the deadline always fired, and the answer cost the full
/// 15 s. It resolved fail-closed (`Unknown` ⇒ refused, and a tree with that much
/// porcelain is emphatically not clean), so it was never *wrong*; it was
/// `N × 15 s` across ~110 worktrees for an answer already available. With the
/// drain running, git finishes and the verdict is real.
///
/// **Every exit path reaps the child.** `std::process::Child` does not kill or
/// wait on drop, so an early `return` without `kill()` + `wait()` leaks a
/// running `git` plus (on POSIX) a zombie.
pub fn worktree_is_dirty(path: &Path) -> DirtyProbe {
    let Some(path_str) = path.to_str() else {
        return DirtyProbe::Unknown;
    };
    let mut child = match crate::process_helpers::no_window("git")
        .args(["-C", path_str, "status", "--porcelain"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return DirtyProbe::Unknown,
    };
    // Reap-and-answer helper: never return from below without it.
    fn give_up(child: &mut std::process::Child) -> DirtyProbe {
        let _ = child.kill();
        let _ = child.wait();
        DirtyProbe::Unknown
    }
    let Some(mut stdout) = child.stdout.take() else {
        return give_up(&mut child);
    };
    let drain = std::thread::spawn(move || {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut buf)
            .ok()
            .map(|_| buf)
    });
    let deadline = std::time::Instant::now() + DIRTY_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    // The child is already reaped by `try_wait`; the drain thread
                    // ends on EOF now that the write end is closed.
                    let _ = drain.join();
                    return DirtyProbe::Unknown;
                }
                // The process has exited, so the pipe is at EOF and this join
                // returns immediately.
                let Ok(Some(out)) = drain.join() else {
                    return DirtyProbe::Unknown;
                };
                return if super::dirty::porcelain_is_dirty(&out) {
                    DirtyProbe::Dirty
                } else {
                    DirtyProbe::Clean
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    warn!(
                        "orphan_target_reaper: `git status` in {} exceeded {:?} — killed, \
                         dirtiness UNKNOWN (refused)",
                        path.display(),
                        DIRTY_PROBE_TIMEOUT
                    );
                    return give_up(&mut child);
                }
                std::thread::sleep(DIRTY_PROBE_POLL);
            }
            // `try_wait` itself failed. The child is still running and NOT
            // reaped — the one path that used to leak it.
            Err(_) => return give_up(&mut child),
        }
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

/// The **boundary gates** (G-owner / G-ownership-unknown / G-report-only /
/// G-dirty). Pure over the candidate's path and class plus the injected
/// dirtiness probe.
///
/// Split out from the five disk gates because these are the ones that make
/// walking into repos and worktrees safe at all: they answer "does another
/// engine own this path?" without touching disk beyond the enclosing tree's
/// `git status`.
///
/// ## The owner gates are PATH-GLOBAL, and that is the point
///
/// They used to sit inside `if let Some(repo_root) = …`, which made the
/// ownership boundary POSITIONAL and fail-OPEN. `repo_root` is `Some` only when
/// the `.git` probe resolved, so ANY failure stat'ing `<wt>/.git` — a worktree
/// mid-`git worktree add`/`remove`, a permissions blip, a `.git` file already
/// removed — demoted a linked worktree to `Container`/`SiblingNonGit` and, with
/// it, silently handed `<wt>/target` a verb on a path the reclaim engine owns:
/// exactly the "two engines disagree" case this module's doc calls impossible by
/// construction. Matching the basename and the path components directly makes
/// the boundary independent of a read that can fail, and of where the build pool
/// happens to be laid out today.
fn boundary_verdict(c: &Candidate, opts: &ClassifyOptions) -> Result<(), SkipReason> {
    let basename = c
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    // G-owner (build pool), evaluated on the PATH alone — the pool is protected
    // by what it IS, so no read failure anywhere can dissolve this boundary.
    if basename == BUILD_POOL_BASENAME
        || c.path
            .components()
            .any(|comp| comp.as_os_str() == BUILD_POOL_COMPONENT)
    {
        return Err(SkipReason::OwnedByBuildPool);
    }

    // G-ownership-unknown: the enclosing `.git` could not be read, so `class`
    // and `repo_root` are guesses and every gate keyed on them is unreliable.
    //
    // Evaluated BEFORE the worktree-reclaim gate below (it used to run after) so
    // that a `target` under an unreadable `.git` is refused for the reason we
    // can actually establish — "we cannot see who owns this" — instead of being
    // told the reclaim engine owns it, which is precisely the fabricated owner
    // this round is removing. The refusal is identical either way; only the
    // claim changes, and nothing can route around it: both arms are `Err`.
    if c.enclosing_git_unreadable {
        return Err(SkipReason::OwnershipUnknown);
    }

    // G-owner (worktree reclaim). Path-global in the sense that matters —
    // `<wt>/target`, `<wt>/src-tauri/target` and `_wt/<slug>/target` all match
    // regardless of depth — but SCOPED to a root with an enclosing checkout,
    // because that is the only shape `super::reclaim` acts on: it removes
    // `<worktree>/target` for worktrees it knows from the census, and nothing
    // else. A bare `D:/scratch/target` with no enclosing repo at all has no
    // owner; refusing it under this reason asserted one that does not exist and
    // left a genuinely-orphaned dir permanently verbless.
    //
    // Checked before G-report-only so a `<repo>/target` is attributed to its
    // real owner rather than to "v1 has no verb".
    if basename == WORKTREE_RECLAIM_BASENAME && c.repo_root.is_some() {
        return Err(SkipReason::OwnedByWorktreeReclaim);
    }

    // v1 gives the in-repo-canonical class no verb at all.
    if !c.class.has_verb() {
        return Err(SkipReason::ReportOnly);
    }

    // Worktree-reclaim's inviolable G1, applied wherever a worktree exists.
    if c.class == TargetClass::SiblingWorktree {
        if let Some(repo_root) = c.repo_root.as_deref() {
            match opts.dirty(repo_root) {
                DirtyProbe::Dirty => return Err(SkipReason::WorktreeDirty),
                DirtyProbe::Unknown => return Err(SkipReason::DirtyUnknown),
                DirtyProbe::Clean => {}
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
///
/// **Deliberately private.** It bypasses [`boundary_verdict`], so a caller that
/// reached it directly would evaluate the disk gates on a path another engine
/// owns. Every production path goes through [`classify_candidate`]; nothing
/// outside this module has ever called this.
fn classify_path(path: &Path, opts: &ClassifyOptions) -> Result<(), SkipReason> {
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
    // never by name alone. Its own reason: reporting "a build is in flight" for
    // a dir with no profile layout is simply false.
    if !looks_like_cargo_artifact(path) {
        return Err(SkipReason::UnrecognizedLayout);
    }
    match cargo_lock_held(path) {
        LockState::Held => return Err(SkipReason::Building),
        // Same refusal as `Held`, different CLAIM — see `LockStateUnknown`.
        LockState::Unknown => return Err(SkipReason::LockStateUnknown),
        LockState::Free => {}
    }
    match newest_artifact_age(path) {
        // Unreadable age → fail-safe (do not reap), but say WHY: the reading is
        // missing, which is not the same as having seen a build.
        None => Err(SkipReason::ActivityUnknown),
        Some(age) if age < opts.grace => Err(SkipReason::GracePending),
        Some(_) => Ok(()),
    }
}

/// Convenience facade over [`classify_path`] for a bare out-of-tree root, with
/// the keep-list read from the ambient env. Test-only for the same reason
/// [`classify_path`] is private — it skips the ownership boundary.
#[cfg(test)]
fn classify(path: &Path, grace: Duration) -> Result<(), SkipReason> {
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

/// Render a [`measure_dir_size`] result for the operator-facing cycle log.
/// `None` says UNKNOWN in words — never `0.00 GB`, which on a reclaim preview
/// reads as "there is nothing here" and is the exact dishonesty this feature
/// exists to remove.
fn render_size(measured: Option<SizeMeasurement>) -> String {
    match measured {
        None => "size UNKNOWN — the root could not be read".to_string(),
        Some(m) if m.unreadable_dirs > 0 => format!(
            "at least {:.2} GB ({} subdir(s) unreadable, so this is a LOWER BOUND)",
            m.bytes as f64 / 1_073_741_824.0,
            m.unreadable_dirs
        ),
        Some(m) => format!("{:.2} GB", m.bytes as f64 / 1_073_741_824.0),
    }
}

/// Bucket a skip reason into [`ReapSummary`]'s counters.
fn count_skip(summary: &mut ReapSummary, reason: SkipReason) {
    // The "we could not read it" refusals share a counter, kept OUT of the
    // observation counters below: a shadow-window review must be able to see
    // how much of a cycle's verdict rests on missing readings rather than on
    // findings.
    if reason.is_unknown() {
        summary.skipped_unknown += 1;
        return;
    }
    match reason {
        SkipReason::Kept => summary.skipped_kept += 1,
        SkipReason::Reparse => summary.skipped_reparse += 1,
        SkipReason::Building => summary.skipped_live += 1,
        SkipReason::UnrecognizedLayout => summary.skipped_unrecognized += 1,
        SkipReason::GracePending => summary.skipped_grace += 1,
        SkipReason::ReportOnly => summary.skipped_report_only += 1,
        SkipReason::OwnedByWorktreeReclaim | SkipReason::OwnedByBuildPool => {
            summary.skipped_other_owner += 1
        }
        SkipReason::WorktreeDirty => summary.skipped_worktree_dirty += 1,
        // Counted above; listed only for exhaustiveness, so a NEW reason cannot
        // be added without deciding which bucket it belongs in.
        SkipReason::OwnershipUnknown
        | SkipReason::DirtyUnknown
        | SkipReason::ActivityUnknown
        | SkipReason::LockStateUnknown => {}
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
        read_errors: enumeration.read_errors.len(),
        entry_errors: enumeration.entry_errors,
        depth_limited_dirs: enumeration.depth_limited_dirs,
        reparse_dirs_skipped: enumeration.reparse_dirs_skipped,
        ..Default::default()
    };
    for c in enumeration.candidates {
        match classify_candidate(&c, &opts) {
            Err(reason) => count_skip(&mut summary, reason),
            Ok(()) => {
                // `measure_dir_size(..).unwrap_or(0)` used to render an
                // UNREADABLE root as "would reap 0.00 GB" in the very line the
                // shadow window is reviewed from — a fabricated zero on the
                // operator-facing summary. The size is now an Option all the way
                // to the log line.
                let measured = measure_dir_size(&c.path);
                let bytes = measured.map_or(0, |m| m.bytes);
                summary.candidates += 1;
                summary.candidate_bytes = summary.candidate_bytes.saturating_add(bytes);
                match measured {
                    None => summary.candidates_with_unknown_bytes += 1,
                    Some(m) if m.unreadable_dirs > 0 => {
                        summary.candidates_with_partial_bytes += 1;
                    }
                    Some(_) => {}
                }
                let size = render_size(measured);
                if !armed {
                    info!(
                        "orphan_target_reaper: [dry-run] would reap {} ({size})",
                        c.path.display(),
                    );
                    continue;
                }
                match remove_junction_safe(&c.path) {
                    Ok(()) => {
                        summary.reaped += 1;
                        summary.reaped_bytes = summary.reaped_bytes.saturating_add(bytes);
                        info!("orphan_target_reaper: reaped {} ({size})", c.path.display(),);
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
         candidate_gb={:.2}{} reaped={} reaped_gb={:.2} \
         skipped(live={},unrecognized={},grace={},reparse={},kept={},report_only={},\
         other_owner={},wt_dirty={},unknown={}) \
         walk(read_errors={},entry_errors={},depth_limited={},reparse_skipped={}) errors={}",
        summary.scanned,
        summary.truncated,
        summary.candidates,
        summary.candidate_bytes as f64 / 1_073_741_824.0,
        // The GB above is a lower bound whenever a candidate could not be
        // sized; say so rather than letting the number stand alone.
        if summary.candidates_with_unknown_bytes > 0 || summary.candidates_with_partial_bytes > 0 {
            format!(
                "(LOWER BOUND: {} unsized, {} partial)",
                summary.candidates_with_unknown_bytes, summary.candidates_with_partial_bytes
            )
        } else {
            String::new()
        },
        summary.reaped,
        summary.reaped_bytes as f64 / 1_073_741_824.0,
        summary.skipped_live,
        summary.skipped_unrecognized,
        summary.skipped_grace,
        summary.skipped_reparse,
        summary.skipped_kept,
        summary.skipped_report_only,
        summary.skipped_other_owner,
        summary.skipped_worktree_dirty,
        summary.skipped_unknown,
        summary.read_errors,
        summary.entry_errors,
        summary.depth_limited_dirs,
        summary.reparse_dirs_skipped,
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
    /// Summed over the candidates that COULD be sized. A LOWER BOUND whenever
    /// either counter below is non-zero.
    pub candidate_bytes: u64,
    /// Candidates whose size could not be read at all — excluded from
    /// `candidate_bytes` rather than counted as zero.
    pub candidates_with_unknown_bytes: usize,
    /// Candidates sized with at least one unreadable subtree.
    pub candidates_with_partial_bytes: usize,
    pub reaped: usize,
    pub reaped_bytes: u64,
    pub skipped_live: usize,
    /// Carries a cargo marker but no profile layout — refused, and NOT counted
    /// as a build in flight.
    pub skipped_unrecognized: usize,
    pub skipped_grace: usize,
    pub skipped_reparse: usize,
    pub skipped_kept: usize,
    /// In-repo-canonical roots: enumerated and measured, no v1 verb.
    pub skipped_report_only: usize,
    /// Owned by the worktree reclaim engine or the build pool.
    pub skipped_other_owner: usize,
    /// Inside a worktree with uncommitted work (G-dirty).
    pub skipped_worktree_dirty: usize,
    /// Refused because a READING was missing (ownership, dirtiness, or idle
    /// age), not because anything was observed.
    pub skipped_unknown: usize,
    /// Directories whose read failed during the walk (`read_dir` open, or the
    /// `.git` ownership probe).
    pub read_errors: usize,
    /// Directory entries that errored mid-iteration.
    pub entry_errors: usize,
    /// Directories not descended into because of [`MAX_WALK_DEPTH`].
    pub depth_limited_dirs: usize,
    /// Reparse points the walk refused to follow.
    pub reparse_dirs_skipped: usize,
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
        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
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
        mk_target_root(&wt.join("target-renamed"));
        // The build pool actually lives OUTSIDE any worktree (a canonical
        // checkout's sibling). Placing it here is what the old positional gate
        // relied on: the gate must hold on the path alone.
        mk_target_root(&root.join("target-pool/slot-0"));
        mk_target_root(&root.join("target-agent"));

        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
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
        assert_eq!(
            verdict("slot-0"),
            Err(SkipReason::OwnedByBuildPool),
            "the pool gate must hold with NO enclosing checkout to strip a prefix against"
        );
        // The renamed in-worktree build dir IS this reaper's territory — the
        // population the module doc used to declare out of scope.
        assert_eq!(verdict("target-renamed"), Ok(()));
    }

    /// **R2 regression — the owner boundary must not be positional.**
    ///
    /// Both owner gates used to sit inside `if let Some(repo_root) = …`, and
    /// `repo_root` is `Some` only when the `.git` probe resolved. So any failure
    /// stat'ing `<wt>/.git` demoted the worktree and handed `<wt>/target` — a
    /// path the reclaim engine owns — this reaper's verb.
    ///
    /// The build-pool gate is matched on the path ALONE and must fire on the
    /// fully demoted shape (`repo_root: None`, `.git` readable-and-absent). The
    /// worktree-reclaim gate is scoped to a root that HAS an enclosing checkout
    /// (see R-3 below), so the shape that exercises it is the one the failing
    /// `.git` stat actually produces: `enclosing_git_unreadable: true`, which
    /// fails closed one gate earlier under the reason we can establish.
    #[test]
    fn owner_gates_hold_without_a_resolved_repo_root() {
        let demoted = |path: &Path| Candidate {
            path: path.to_path_buf(),
            // The demotion's own symptom: a linked worktree read as a bare
            // out-of-tree root.
            class: TargetClass::SiblingNonGit,
            repo_root: None,
            enclosing_git_unreadable: false,
        };
        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
        assert_eq!(
            classify_candidate(&demoted(Path::new("/ws/repo-wt-x/target-agent")), &opts),
            Err(SkipReason::OwnedByBuildPool)
        );
        assert_eq!(
            classify_candidate(&demoted(Path::new("/ws/target-pool/slot-0")), &opts),
            Err(SkipReason::OwnedByBuildPool)
        );
        // A `<wt>/target` whose `.git` stat FAILED — the R2 scenario — is still
        // refused, and now under the reason the probe can actually support.
        let unreadable = Candidate {
            enclosing_git_unreadable: true,
            ..demoted(Path::new("/ws/repo-wt-x/target"))
        };
        assert_eq!(
            classify_candidate(&unreadable, &opts),
            Err(SkipReason::OwnershipUnknown),
            "a failed `.git` read must not hand another engine's path a verb"
        );
        // And a `<wt>/target` whose worktree DID resolve is the reclaim
        // engine's, at any depth inside it.
        for p in ["/ws/repo-wt-x/target", "/ws/repo-wt-x/src-tauri/target"] {
            let owned = Candidate {
                repo_root: Some(PathBuf::from("/ws/repo-wt-x")),
                class: TargetClass::SiblingWorktree,
                ..demoted(Path::new(p))
            };
            assert_eq!(
                classify_candidate(&owned, &opts),
                Err(SkipReason::OwnedByWorktreeReclaim),
                "{p}"
            );
        }
    }

    /// **R-3 — the path-global `target` gate must not fabricate an owner.**
    ///
    /// Making `basename == "target"` path-global was right for
    /// `_wt/<slug>/target`, but it also fired on a `target` with NO enclosing
    /// worktree at all. `super::reclaim` only ever removes `<worktree>/target`
    /// for a worktree it knows from the census, so a bare `D:/scratch/target`
    /// orphan was refused forever under a `reason_detail` asserting "The
    /// worktree reclaim engine owns this path" — an owner that does not exist.
    /// Nothing owned it, and nothing would ever clean it up.
    #[test]
    fn a_bare_target_with_no_enclosing_checkout_is_nobodys_and_keeps_its_verb() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A bare orphan: `target` directly under the walk root, no repo anywhere
        // above it.
        mk_target_root(&root.join("target"));
        // …and the control, one directory over: the same basename INSIDE a
        // linked worktree, which really is the reclaim engine's.
        let wt = root.join("repo-wt-x");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), b"gitdir: /repo/.git/worktrees/x\n").unwrap();
        mk_target_root(&wt.join("target"));

        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
        let candidates = enumerate_candidates(root);
        let bare = candidates
            .iter()
            .find(|c| c.repo_root.is_none() && c.path.ends_with("target"))
            .expect("the bare orphan was enumerated");
        let owned = candidates
            .iter()
            .find(|c| c.repo_root.is_some() && c.path.ends_with("target"))
            .expect("the in-worktree one was enumerated");

        assert_eq!(
            classify_candidate(bare, &opts),
            Ok(()),
            "nothing owns a bare `target`, so refusing it as owned strands it forever"
        );
        assert_eq!(
            classify_candidate(owned, &opts),
            Err(SkipReason::OwnedByWorktreeReclaim),
            "…while the in-worktree one is still another engine's, which is what \
             makes the assertion above a narrowing rather than a hole"
        );
    }

    /// An unreadable `.git` is not an ABSENT one: the candidate inside it fails
    /// closed with its own reason, instead of being demoted to a class whose
    /// gates no longer apply.
    #[test]
    fn an_unreadable_dot_git_fails_closed() {
        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
        let opaque = Candidate {
            path: PathBuf::from("/ws/repo-wt-x/target-renamed"),
            class: TargetClass::SiblingNonGit,
            repo_root: Some(PathBuf::from("/ws/repo-wt-x")),
            enclosing_git_unreadable: true,
        };
        assert_eq!(
            classify_candidate(&opaque, &opts),
            Err(SkipReason::OwnershipUnknown),
            "ownership we could not read is refused, not assumed free"
        );
        // The SAME path with a readable `.git` is this reaper's territory —
        // which is what makes the assertion above non-vacuous.
        let known = Candidate {
            enclosing_git_unreadable: false,
            class: TargetClass::SiblingWorktree,
            ..opaque
        };
        assert_ne!(
            classify_candidate(&known, &opts),
            Err(SkipReason::OwnershipUnknown)
        );
    }

    /// [`git_probe`] must distinguish all four answers. The old `git_marker`
    /// collapsed every error into `None` — indistinguishable from "no `.git`
    /// here", which is what made the demotion silent.
    #[test]
    fn git_probe_distinguishes_absent_from_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("canonical");
        fs::create_dir_all(canonical.join(".git/refs")).unwrap();
        assert_eq!(git_probe(&canonical), GitProbe::Dir);

        let linked = tmp.path().join("linked");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join(".git"), b"gitdir: /r/.git/worktrees/l\n").unwrap();
        assert_eq!(git_probe(&linked), GitProbe::File);

        let plain = tmp.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        assert_eq!(
            git_probe(&plain),
            GitProbe::Absent,
            "no `.git` is a POSITIVE answer"
        );
    }

    /// The walk-level half of the same regression, on the one `.git` shape that
    /// is reproducible without special privileges: a `.git` that exists but is
    /// neither a dir nor a file. It must NOT read as absent.
    #[cfg(unix)]
    #[test]
    fn an_opaque_dot_git_is_recorded_and_fails_closed_end_to_end() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root.join("repo-wt-opaque");
        fs::create_dir_all(&wt).unwrap();
        symlink("/nonexistent/worktrees/opaque", wt.join(".git")).unwrap();
        mk_target_root(&wt.join("target-renamed"));

        let e = enumerate_all(root);
        assert_eq!(e.candidates.len(), 1);
        assert!(
            e.candidates[0].enclosing_git_unreadable,
            "an unreadable `.git` must be carried on the candidate"
        );
        assert!(
            e.read_errors.iter().any(|(p, _)| p.ends_with(".git")),
            "and reported as a failed read, not folded into 'nothing there'"
        );
        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
        assert_eq!(
            classify_candidate(&e.candidates[0], &opts),
            Err(SkipReason::OwnershipUnknown)
        );
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

        let clean = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
        let dirty = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Dirty);
        let unknown = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Unknown);
        assert_eq!(classify_candidate(&c, &clean), Ok(()));
        assert_eq!(
            classify_candidate(&c, &dirty),
            Err(SkipReason::WorktreeDirty),
            "worktree-reclaim's G1 is inviolable wherever a worktree exists"
        );
        // A probe that could not COMPLETE refuses just as hard, but under its
        // own reason: "the enclosing worktree has uncommitted work" is a claim a
        // failed probe has no standing to make.
        assert_eq!(
            classify_candidate(&c, &unknown),
            Err(SkipReason::DirtyUnknown)
        );
        assert!(SkipReason::DirtyUnknown.is_unknown());
        assert!(!SkipReason::WorktreeDirty.is_unknown());
        // The prose must REFUSE without asserting the finding. `WorktreeDirty`
        // states it flatly; `DirtyUnknown` may only say the question is open.
        assert!(SkipReason::WorktreeDirty
            .detail()
            .starts_with("The enclosing worktree has uncommitted work."));
        assert!(
            !SkipReason::DirtyUnknown
                .detail()
                .contains("The enclosing worktree has uncommitted work"),
            "the detail must not assert a fact the probe never established"
        );
        assert!(SkipReason::DirtyUnknown
            .detail()
            .contains("could NOT be completed"));
    }

    /// **R3** — the dirty probe is memoised per repo root for the life of one
    /// [`ClassifyOptions`]. ~110 linked worktrees each holding several build
    /// dirs meant ~one `git status` spawn PER CANDIDATE, on both the survey walk
    /// and every 900 s reaper cycle.
    #[test]
    fn the_dirty_probe_is_memoised_per_repo_root() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        CALLS.store(0, Ordering::SeqCst);

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root.join("repo-wt-memo");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), b"gitdir: /repo/.git/worktrees/m\n").unwrap();
        for name in ["target-a", "target-b", "target-c"] {
            mk_target_root(&wt.join(name));
        }
        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            DirtyProbe::Clean
        });
        let cands = enumerate_candidates(root);
        assert_eq!(cands.len(), 3);
        for c in &cands {
            assert_eq!(classify_candidate(c, &opts), Ok(()));
        }
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            1,
            "one `git status` per repo root per walk, not one per candidate"
        );
    }

    #[test]
    fn skip_reasons_have_distinct_tokens_and_details() {
        let all = [
            SkipReason::ReportOnly,
            SkipReason::OwnedByWorktreeReclaim,
            SkipReason::OwnedByBuildPool,
            SkipReason::OwnershipUnknown,
            SkipReason::WorktreeDirty,
            SkipReason::DirtyUnknown,
            SkipReason::Reparse,
            SkipReason::Building,
            SkipReason::LockStateUnknown,
            SkipReason::UnrecognizedLayout,
            SkipReason::ActivityUnknown,
            SkipReason::GracePending,
            SkipReason::Kept,
        ];
        let tokens: std::collections::BTreeSet<&str> = all.iter().map(|r| r.token()).collect();
        assert_eq!(tokens.len(), all.len(), "every reason needs its own token");
        let details: std::collections::BTreeSet<&str> = all.iter().map(|r| r.detail()).collect();
        assert_eq!(
            details.len(),
            all.len(),
            "two reasons sharing a sentence is how a refusal on a MISSING READING ends up \
             rendering as an observed fact — the defect this split exists to fix"
        );
        for r in all {
            assert!(!r.detail().is_empty(), "{:?} needs an operator sentence", r);
        }
        // Exactly the ignorance refusals, and no observation among them. The
        // list is spelled out rather than counted so that adding a reason forces
        // a decision about which side of the line it falls on — `building` and
        // `lock-state-unknown` sitting next to each other here IS the R-2 fix.
        let unknown: Vec<&str> = all
            .iter()
            .filter(|r| r.is_unknown())
            .map(|r| r.token())
            .collect();
        assert_eq!(
            unknown,
            vec![
                "ownership-unknown",
                "worktree-dirty-unknown",
                "lock-state-unknown",
                "activity-unknown"
            ]
        );
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
        // …and the refusal says what it actually is. It used to report
        // `Building`, whose headline — "a build is in flight here" — is flatly
        // false for a dir with no profile layout at all.
        assert_eq!(
            classify(&fake, Duration::ZERO),
            Err(SkipReason::UnrecognizedLayout)
        );
        assert!(!SkipReason::UnrecognizedLayout
            .detail()
            .contains("A build is in flight"));
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

    /// The PRODUCTION dirty probe fails closed to `Unknown`, never to `Clean`.
    /// A directory that is not a git worktree makes `git status` exit non-zero
    /// (and a box with no `git` on PATH makes it fail to spawn) — both must land
    /// on the tri-state's refusing arm.
    #[test]
    fn the_production_dirty_probe_fails_closed_to_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            worktree_is_dirty(tmp.path()),
            DirtyProbe::Unknown,
            "a probe that cannot answer must not answer CLEAN — that would hand a verb to a \
             tree we know nothing about"
        );
    }

    /// **R3** — the probe must be bounded. An unbounded `.output()` on the
    /// preview path lets ONE hung `git` pin `disk_survey::BUILD_ACTIVE` forever:
    /// `spawn_rebuild` becomes a permanent no-op, the snapshot freezes, and
    /// `census_refreshing` stays `true`. Source-level because a hanging `git` is
    /// not something a unit test can conjure portably.
    #[test]
    fn the_dirty_probe_is_bounded_by_a_deadline() {
        const SRC: &str = include_str!("orphan_target_reaper.rs");
        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        let f = prod
            .split_once("pub fn worktree_is_dirty(")
            .map(|(_, after)| after)
            .expect("worktree_is_dirty must exist; move this pin if it is renamed");
        let body = f.split_once("\n}\n").map(|(b, _)| b).unwrap_or(f);
        assert!(
            !body.contains(".output()"),
            "`.output()` blocks with no timeout — the whole survey walk is hostage to one git"
        );
        assert!(
            body.contains("deadline") && body.contains("kill()"),
            "the probe must have a deadline AND kill the child when it passes"
        );
        // **R-5** — `std::process::Child` does NOT reap on drop, so every early
        // return must kill+wait. The timeout arm always did; the `try_wait`
        // error arm returned bare, leaking a running `git` plus a POSIX zombie.
        // Pinned as a source shape because provoking a `try_wait` failure is not
        // portable: no `return DirtyProbe::Unknown;` may appear un-reaped once
        // the child exists.
        let after_spawn = body
            .split_once("Err(_) => return DirtyProbe::Unknown,\n    };")
            .map(|(_, after)| after)
            .expect("the spawn arm must stay the last bare Unknown return");
        assert!(
            !after_spawn.contains("=> return DirtyProbe::Unknown"),
            "an arm returns Unknown without reaping the child — use `give_up(&mut child)`"
        );
        // **R-5 (second half)** — stdout must be drained concurrently. With
        // nobody reading, a `git status` bigger than the pipe buffer blocks on
        // its own write, can never exit, and burns the entire 15 s deadline for
        // an answer that was already available.
        assert!(
            body.contains("std::thread::spawn"),
            "stdout must be drained off-thread or a chatty git eats the whole deadline"
        );
    }

    /// A walk whose ROOT could not be read reports the failure and NO
    /// candidates. The two must never look alike downstream — see
    /// `disk_survey::a_failed_root_read_is_unknown_not_a_measured_zero`.
    #[test]
    fn a_failed_root_read_is_recorded_not_reported_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("never-created");
        let e = enumerate_all(&missing);
        assert!(e.candidates.is_empty());
        assert_eq!(e.read_errors.len(), 1, "the failed read is RECORDED");
        assert_eq!(e.read_errors[0].0, missing);
        assert!(!e.read_errors[0].1.is_empty(), "with the OS reason");
    }

    /// [`MAX_WALK_DEPTH`] and the reparse skip are both silent omissions unless
    /// they are counted: a target root below the bound, or behind a junction,
    /// appears neither as an item nor as an error.
    #[cfg(unix)]
    #[test]
    fn the_walk_counts_its_depth_bound_and_its_skipped_reparse_points() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Depth 5 — one past the bound, so this root is INVISIBLE.
        mk_target_root(&root.join("a/b/c/d/e/target-too-deep"));
        let real = root.join("real-target");
        mk_target_root(&real);
        symlink(&real, root.join("linked-target")).unwrap();

        let e = enumerate_all(root);
        assert!(
            !e.candidates
                .iter()
                .any(|c| c.path.ends_with("target-too-deep")),
            "the bound really does hide it — otherwise the counter proves nothing"
        );
        assert!(
            e.depth_limited_dirs > 0,
            "so the bound must SAY it bit: {e:?}"
        );
        assert!(
            !e.candidates
                .iter()
                .any(|c| c.path.ends_with("linked-target")),
            "a reparse point is never followed"
        );
        assert_eq!(e.reparse_dirs_skipped, 1, "…but it is counted");
    }

    /// The operator-facing "would reap" line is the whole shadow-window review.
    /// An unreadable root used to print `0.00 GB` there — a fabricated zero on
    /// the one surface the arming decision is made from.
    #[test]
    fn an_unsized_candidate_renders_as_unknown_never_as_zero_gb() {
        let unknown = render_size(None);
        assert!(unknown.contains("UNKNOWN"), "{unknown}");
        assert!(!unknown.contains("0.00 GB"), "{unknown}");
        let partial = render_size(Some(SizeMeasurement {
            bytes: 1_073_741_824,
            unreadable_dirs: 3,
        }));
        assert!(partial.contains("LOWER BOUND"), "{partial}");
        let exact = render_size(Some(SizeMeasurement {
            bytes: 2_147_483_648,
            unreadable_dirs: 0,
        }));
        assert_eq!(exact, "2.00 GB");
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
        assert_eq!(cargo_lock_held(&tgt), LockState::Free);
        // Open with share_mode=0 (exclusive), as a live cargo effectively does;
        // our probe's write-open then fails with a sharing violation → building.
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&lock)
            .unwrap();
        assert_eq!(
            cargo_lock_held(&tgt),
            LockState::Held,
            "an exclusively-held lock reads as building"
        );
    }

    /// **R-2 — a `.cargo-lock` probe that could not READ must not report a build.**
    ///
    /// The round-1 fix turned `entries.flatten()` into `let Ok(entry) = entry
    /// else { return true }`, i.e. a directory entry that could not be read
    /// became [`SkipReason::Building`] — whose operator sentence asserts "A build
    /// is in flight here", something never observed. Worse, `count_skip` filed it
    /// under `skipped_live`, the OBSERVATION counter the shadow-window arming
    /// decision is read from, so a permissions blip inflated the live-build
    /// tally on the very line the decision rests on.
    ///
    /// Fail-closed is preserved: the refusal is identical to `Building`. Only the
    /// claim, and the counter, change.
    #[test]
    fn an_unreadable_lock_probe_is_unknown_never_a_build_in_flight() {
        let tmp = tempfile::tempdir().unwrap();
        // A root whose `read_dir` fails exactly as a permissions blip does: it
        // is not a directory at all, so the enumeration the probe depends on
        // cannot complete.
        let not_a_dir = tmp.path().join("not-a-dir");
        fs::write(&not_a_dir, b"").unwrap();
        assert_eq!(
            cargo_lock_held(&not_a_dir),
            LockState::Unknown,
            "a failed read is not a finding that no lock is held — nor that one is"
        );
        // Non-vacuous control: a readable root with no lock is a REAL `Free`.
        let ok = tmp.path().join("t");
        mk_target_root(&ok);
        assert_eq!(cargo_lock_held(&ok), LockState::Free);

        // The reason it maps to is a statement of ignorance, so it lands in
        // `skipped_unknown` and NEVER in `skipped_live`.
        assert!(SkipReason::LockStateUnknown.is_unknown());
        assert!(!SkipReason::Building.is_unknown());
        let count = |reason| {
            let mut s = ReapSummary::default();
            count_skip(&mut s, reason);
            s
        };
        let unknown = count(SkipReason::LockStateUnknown);
        assert_eq!(unknown.skipped_unknown, 1);
        assert_eq!(
            unknown.skipped_live, 0,
            "a permissions blip must not inflate the live-build tally the arming \
             decision is reviewed from"
        );
        let building = count(SkipReason::Building);
        assert_eq!(building.skipped_live, 1);
        assert_eq!(building.skipped_unknown, 0);
        // And the sentences must not be interchangeable.
        assert!(SkipReason::Building
            .detail()
            .contains("A build is in flight"));
        assert!(
            !SkipReason::LockStateUnknown
                .detail()
                .contains("A build is in flight"),
            "a probe that did not run must not describe what it would have seen"
        );
    }
}
