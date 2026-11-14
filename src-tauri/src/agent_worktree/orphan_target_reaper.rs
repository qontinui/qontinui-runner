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
//! | anything under `target-pool/`, or basename `target-agent` / `target-arming`, ANYWHERE | the build system / `cargo-guard` | [`SkipReason::OwnedByBuildPool`] — never a candidate |
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
//!     inside a checkout/worktree, `target-agent`, `target-arming`, or anything under
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
//! ## Enumeration roots — TWO of them, and an ABSENT one is not an EMPTY one
//!
//! The walk used to start at exactly one place: [`super::census::qontinui_root`]
//! (= `workspace_paths::workspace_root()`). Everything above is written as if
//! that were the whole world, and for the out-of-tree cargo caches it is.
//!
//! It is not the whole world for the **harness scratchpad tree**. Agent sessions
//! are handed a per-session scratchpad at
//! `<tmp>/claude-<uid>/<project>/<session>/scratchpad/`, and agents build there:
//! a `CARGO_TARGET_DIR` dropped in a scratchpad is a cargo target root exactly
//! like `target-wt-<slug>` is, with the same rebuildable-cache economics. That
//! tree is **not under the workspace root at any depth**, so those roots were
//! not "missed by a heuristic" — they were *structurally invisible*. No gate
//! refused them; the enumerator never looked.
//!
//! So enumeration is now over a LIST of roots ([`reap_roots`]):
//!
//! | # | Root | Resolution | Behaviour |
//! |---|---|---|---|
//! | 1 | the workspace root | [`super::census::qontinui_root`] | **unchanged** |
//! | 2… | the harness scratchpad FAMILY | [`reap_roots_from`]: `<tmp>/claude-<uid>`, then every `<qontinui_dir>/scratch/<account>/claude-<uid>` and `…/<account>/<lineage>/claude-<uid>` ([`scratch_family_under`]) | one [`ReapRoot`] per tree |
//!
//! The scratch roots are **discovered, never hardcoded**: `<tmp>` from the
//! platform temp dir, `<uid>` from `geteuid(2)` and `<qontinui_dir>` from
//! `ambient::qontinui_dir`. A literal `/tmp/claude-1000` would be correct on
//! precisely one box and silently wrong everywhere else, which is the same
//! defect class as a fabricated zero.
//!
//! ### Why the scratch root is a FAMILY — the tree it used to read was empty
//!
//! The account shortcuts launch sessions with `TMPDIR=<qontinui_dir>/scratch/
//! <account>`, so their scratchpads are under
//! `<qontinui_dir>/scratch/<account>/claude-<uid>/<project>/<session>/…`, not
//! under `<tmp>`. On merytshost 2026-10-31 the reaper's one scratch root was
//! `/tmp/claude-1000` and reported `scanned=0 census=complete` — a complete
//! census of an empty directory, reading as coverage — while the on-disk
//! family held hundreds of GB (417 GB when re-measured 2026-11-02). Each family member is now its own root with its own
//! visit budget and its own line on the cycle (`path=` names it), a family that
//! could not be established is a root of its own that says so, and every root
//! also reports how many bytes its walk READ (`walked_gb=`, `dirs=`) beside
//! `scanned`, so `scanned=0` over a populated tree no longer looks like
//! `scanned=0` over an empty one.
//!
//! ### A scratch candidate takes two more gates than a workspace one
//!
//! A scratch root holds other sessions' live state, so a candidate there must
//! (a) sit under `<project>/<session-uuid>/`, (b) belong to a session that is
//! idle on EVERY account — the newest `<account>/projects/<project>/<uuid>.jsonl`
//! across all of them, because a session can move accounts and leave a frozen
//! copy behind — with no live process naming its uuid or holding its directory,
//! and (c) not be held: no granted flock on its `.cargo-lock` files
//! (`/proc/locks`, through the census's own parser) and no process whose cwd,
//! open fd or `CARGO_TARGET_DIR` is in it. The reaper's own `.cargo-lock` probe
//! is a write-open, which an `flock` does not block, so on Linux the old G-live
//! was an mtime alone. Anything those reads could not establish — no account
//! home, no transcript, an unlistable process table — refuses the candidate;
//! a rule that needed a positive DEAD would never fire, and one that read an
//! absence as DEAD would delete a live session's build. See
//! [`classify_candidate`] and [`scratch_liveness_verdict`].
//!
//! **An absent root is ABSENT, not empty.** Every root carries a
//! [`RootState`] into [`ReapSummary::roots`], and a root that is `Absent`,
//! `Unresolved`, `Unreadable` or `AlreadyWalked` contributes NOTHING and SAYS
//! so. A missing tree that rendered as `scanned=0` would read as "looked, found
//! nothing" — the identical silent-empty-as-NO error [`SkipReason::OwnershipUnknown`]
//! exists to refuse.
//!
//! ### The verb stays TARGET-DIRECTORIES-ONLY — load-bearing, not incidental
//!
//! A scratchpad holds far more than build caches. Measured on this box
//! 2026-09-01: **23 git worktrees live inside `/tmp/claude-1000` scratchpads
//! whose HEAD is reachable from no remote ref**, one of them dirty. tmpfs has no
//! undelete and no recycle bin. Widening the verb from "a cargo target root" to
//! "a scratchpad" would put every one of them in the population.
//!
//! It is not widened, and the property is structural rather than a rule someone
//! must remember: a candidate exists only where [`is_target_root`] finds a cargo
//! marker, and it keeps a verb only where [`looks_like_cargo_artifact`] also
//! finds a `debug`/`release` profile layout. A worktree, a `notes/` dir, a
//! transcript, a scratch `.sql` file — none carries a `CACHEDIR.TAG` or a
//! `.rustc_info.json`, so none is ever enumerated as a candidate at all. A test
//! pins exactly this.
//!
//! ### The depth bound belongs to the ROOT, not to the module
//!
//! The scratchpad tree is DEEPER than the workspace one, so a single global
//! bound was necessarily wrong for one of them. The first shadow cycle over
//! `/tmp/claude-1000` (2026-09-01) reported 5 target roots — 4 reapable,
//! 30.9 GB — while counting **5,226** directories it never descended into.
//!
//! Each [`ReapRoot`] now carries its own bound ([`ReapRootKind::max_walk_depth`]):
//! the workspace root keeps 4 ([`WORKSPACE_MAX_WALK_DEPTH`] — flat
//! `<root>/target-wt-<slug>` layout, behaviour unchanged), the scratchpad root
//! gets 6 ([`SCRATCHPAD_MAX_WALK_DEPTH`], which reaches a target root at depth
//! 7 — the depth this fleet's own `…/scratchpad/<subdir>/<checkout>/src-tauri/target`
//! layout produces). [`MAX_DIRS_VISITED`] is untouched and remains the ABSOLUTE
//! cost ceiling, so a deeper bound can widen what is reachable without ever
//! becoming unbounded work.
//!
//! **A bound of N reaches depth N + 1**, because a target root is recognised
//! while its PARENT is being iterated. The full measured depth table, the
//! before/after of the raise, and the correction they forced to the reading
//! that motivated this change are on [`SCRATCHPAD_MAX_WALK_DEPTH`]. The short
//! version: the four candidates that first cycle found were ALREADY reachable
//! at 4, raising to 6 moved `scanned` 5 → 12 and `candidates` not at all, and
//! it was the *reporting*, not the bound, that let 4 read as a total.
//!
//! ### A truncated walk SAYS it is truncated — in the type, not just a counter
//!
//! The counters alone were not enough. `candidates=4` came off a depth-limited
//! walk in exactly the shape a complete census would have — a truncated result
//! presented as a total, which is the same silent-empty-as-UNKNOWN failure
//! [`SkipReason::OwnershipUnknown`] exists to refuse. A side counter does not
//! fix that: nothing forces a consumer to read it.
//!
//! So every walked root carries a [`Completeness`] into
//! [`RootReport::completeness`], and the cycle rolls them up
//! ([`ReapSummary::completeness`]). [`Completeness::Truncated`] names both
//! causes — the depth bound and [`MAX_DIRS_VISITED`] — and renders into the
//! cycle summary line as an explicit `INCOMPLETE CENSUS … LOWER BOUND` marker
//! sitting directly beside the number it qualifies. A consumer that reads
//! `candidates` cannot avoid seeing that the number is a floor.
//!
//! A target root nested deeper than its root's bound is still ABSENT from the
//! answer rather than refused — but the answer now says it may be.
//!
//! ### Cross-user safety — new with root 2, because root 2 lives in a shared dir
//!
//! The workspace root is single-owner, so this module never needed to ask who
//! owns a path. `<tmp>` is not: it is sticky (`drwxrwxrwt`) and holds other
//! accounts' trees — on this box 1,827 of the direct children of `/tmp` are
//! owned by `runner`, not by the invoking account. A reaper that walks anything
//! rooted in a shared directory must therefore have an **account boundary**, and
//! it is the OUTERMOST one: a path another uid owns is not this reaper's to
//! reason about at all, whichever engine's naming convention it happens to
//! match. [`SkipReason::ForeignOwner`], evaluated first in [`boundary_verdict`],
//! from a uid captured during the walk at no extra syscall. Reported (its bytes
//! are real and visible); never acted on.
//!
//! ## The pressure signal has TWO axes, and they are OR-ed, never AND-ed
//!
//! Every threshold in this module was byte-denominated — the motivating incident
//! at the top of this file is "filled `D:` to 1.3 % free". On 2026-09-01 `/tmp`
//! on the operator box hit its hard `nr_inodes=1048576` cap and **every Claude
//! Code session lost its Bash tool at once** (`ENOSPC` on every spawn), while
//! `df -h /tmp` read a healthy 27 % used. A byte-only alarm reads GREEN through
//! the whole of that outage.
//!
//! So [`FsCapacity`] carries both axes for the mount each root lives on, read
//! with `statvfs(3)`. `statvfs` is the ONE filesystem read in this module that
//! is not a walk, and it is exempt from the no-walking discipline for a
//! structural reason: it reads the superblock, so it is O(1) regardless of how
//! many files the filesystem holds.
//!
//! Two rules make the signal honest:
//!
//!   * **OR, never AND.** [`FsCapacity::pressured`] fires when EITHER axis is at
//!     or above the threshold. ANDing them reproduces the 2026-09-01 outage
//!     exactly: inodes at 100 %, bytes at 27 %, verdict "healthy". Bytes are not
//!     demoted — they remain a first-class axis with their own verdict.
//!   * **`f_files == 0` is UNKNOWN, not 0 %.** A tmpfs mounted `nr_inodes=0`,
//!     and btrfs / xfs / zfs generally, report no inode cap at all. Computing
//!     "0 used of 0" yields the *healthiest possible reading* on exactly the
//!     filesystems where nothing is measurable — a fabricated zero of the same
//!     family as `measure_dir_size(..).unwrap_or(0)`. The axis reports
//!     [`PressureAxis::Unknown`] with the reason [`INODE_LIMIT_NOT_REPORTED`],
//!     and the BYTE half — a genuine measurement — passes through unaffected.
//!     Nulling both would throw away a reading that was actually taken.
//!
//! The signal is **surfaced, not armed**: it is measured and logged beside every
//! root so a shadow window can be reviewed against it. It gates no removal, and
//! this module ships DRY-RUN exactly as before.
//!
//! Named gap, stated rather than hidden: `statvfs(3)` is POSIX, so on Windows
//! both axes report [`STATVFS_UNAVAILABLE`]. NTFS has no inode cap to report,
//! and the byte half there wants `GetDiskFreeSpaceExW`, which is a separate
//! change on a platform this branch cannot compile-check. Reported as UNKNOWN
//! with a reason; never as healthy.
//!
//! ## Arming — TWO populations, TWO switches
//!
//! **Workspace roots: DRY-RUN by default**; armed only by
//! `COORD_ORPHAN_TARGET_REAP_ENABLED` truthy — same posture as
//! [`super::reclaim`]. While dark it merely LOGS what it would reap (+ bytes),
//! so the operator reviews a shadow cycle before arming.
//!
//! **Scratch roots: ARMED by default**, disabled by
//! `COORD_ORPHAN_TARGET_SCRATCH_REAP_ENABLED` being exactly `0`
//! ([`SCRATCH_REAP_ENABLED_ENV`]). The two are independent and each root carries
//! its own ([`ReapRoot::armed`], rendered as `armed=` in the root's token): a
//! reclaim nobody switched on is a reclaim the product does not have, and the
//! workspace flag's named prerequisite — coord's retention pin is not honoured,
//! so a pinned worktree's `target-<slug>` is reapable — is a property of
//! WORKSPACE worktrees. A scratch target has no coord worktree row to pin. What
//! guards the scratch arm instead is per candidate: the session and holder
//! gates above, and a RE-READ of all of them immediately before each removal
//! ([`reread_scratch_candidate`]) — a candidate list is a snapshot and never a
//! delete list. A new default reaches a box only on that box's next runner
//! start.
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

use super::census::{is_junction, qontinui_root, remove_link};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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

/// Env override pointing the harness-scratchpad enumeration root somewhere
/// else. Unset — the normal case — the roots are DISCOVERED from the platform
/// temp dir, the invoking uid and the on-disk scratch family
/// ([`scratch_family_under`]), never hardcoded. This exists so the root is an
/// injectable seam for tests and for an operator whose harness lays
/// scratchpads out elsewhere. It overrides the WHOLE family outright: set, the
/// one named directory is the only scratch root walked.
pub const SCRATCH_ROOT_ENV: &str = "COORD_ORPHAN_TARGET_SCRATCH_ROOT";
/// Env switch for the SCRATCH population's destructive arm — session scratch
/// under the harness-scratchpad roots, which is a different population from the
/// workspace's with a different safety argument (plan
/// `2026-11-01-session-scratch-on-disk-has-no-reclaim-arm-so-the-largest-idle-bytes-hold-every-build-behind-the-reserve`,
/// D6 and Phase 5). **Default ON; exactly `0` disables.** Any other value —
/// unset, empty, `false`, `1` — leaves the scratch arm on. Independent of
/// [`REAP_ENABLED_ENV`]: neither switch arms or disarms the other population.
pub const SCRATCH_REAP_ENABLED_ENV: &str = "COORD_ORPHAN_TARGET_SCRATCH_REAP_ENABLED";
/// Env override for the percentage-used at or above which a capacity axis is
/// reported PRESSURED. Default 90 %. Applies to BOTH axes independently.
pub const PRESSURE_PCT_ENV: &str = "COORD_ORPHAN_TARGET_PRESSURE_PCT";

const DEFAULT_GRACE_SECS: u64 = 86_400;
const DEFAULT_PRESSURE_PCT: f64 = 90.0;
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

/// How deep below the WORKSPACE root the enumerator walks. Covers every layout
/// this fleet produces — `_wt/<slug>/src-tauri/target-x` is the deepest at 4 —
/// while keeping a mistaken root (a home dir, `C:\`) from becoming an unbounded
/// walk. Pruning at every discovered target root keeps the real cost far below
/// the bound.
///
/// This is the workspace root's bound ONLY. The depth bound is a property of
/// the root being walked, not of the module — see
/// [`ReapRootKind::max_walk_depth`].
const WORKSPACE_MAX_WALK_DEPTH: u32 = 4;

/// How deep below the HARNESS SCRATCHPAD root the enumerator walks.
///
/// ## What a bound of N actually reaches — measured, because it is off by one
///
/// [`walk`] returns at `depth > max_depth`, and a target root is recognised
/// while iterating its PARENT. So a bound of N reaches target roots at
/// root-relative depth **N + 1**, and 4 was never the bound that hid the
/// scratchpad population at depth 5.
///
/// This was checked against the real tree rather than assumed, and it corrects
/// the reading that motivated this change. Enumerating `/tmp/claude-1000` on
/// 2026-09-01 found every cargo marker directory it holds:
///
/// | depth | dir | profile layout |
/// |---|---|---|
/// | 1 | `coord-target-<hash>` | yes → candidate |
/// | 5 | `<project>/<session>/scratchpad/{bs,f,verify}/target` | yes → candidate |
/// | 5 | `…/scratchpad/wt-p1-migration/.ruff_cache` | no |
/// | 6 | `…/scratchpad/<wt>/backend/.{ruff,mypy,pytest}_cache` (7 of them) | no |
///
/// The four candidates that cycle reported were ALREADY reachable at 4. What 4
/// hid is the depth-6 row — seven directories, none of them cargo today.
///
/// ## Why the bound is still raised
///
/// Because the scratchpad tree demonstrably nests to depth 6 and beyond, and
/// this fleet's own Rust layout lands a target there. A repo checked out inside
/// a scratchpad subdir puts its build dir at
/// `<project>/<session>/scratchpad/<subdir>/<checkout>/src-tauri/target` —
/// **depth 7**. The depth-6 `backend/` rows above are that same shape, one
/// language over. A bound of 6 reaches depth 7 and so covers it; 4 reaches only
/// 5 and cannot.
///
/// It is deliberately not larger. Past that the tree stops having a known
/// shape, and an unbounded walk over a shared temp filesystem is exactly what
/// the bound exists to prevent.
///
/// ## What the raise cost, and what it did NOT buy
///
/// The same tree walked twice, DRY-RUN, 2026-09-01:
///
/// | | 4 | 6 |
/// |---|---|---|
/// | `scanned` | 5 | 12 |
/// | `candidates` | 4 | **4** |
/// | `candidate_gb` | 30.92 | **30.92** |
/// | `dirs_visited` | 6,744 | 18,628 |
/// | `depth_limited` | 5,237 | 6,393 |
/// | wall clock | 0.22 s | 0.95 s |
///
/// The candidate set did not move. The seven directories the raise newly
/// reaches are `.ruff_cache` / `.mypy_cache` / `.pytest_cache` — they carry a
/// `CACHEDIR.TAG` so they enumerate, and they carry no `debug`/`release` layout
/// so [`looks_like_cargo_artifact`] refuses them a verb
/// (`skipped(unrecognized)` 1 → 8). The raise buys REACH for a layout this
/// fleet produces, not bytes that were already there; anyone reading it as a
/// reclaim win is reading it wrong.
///
/// 18,628 of [`MAX_DIRS_VISITED`]'s 200,000 is the cost, at one cycle per
/// 900 s. The ceiling is untouched and still absolute.
///
/// **`depth_limited` went UP, 5,237 → 6,393, and that is not a regression.**
/// Descending two more levels exposes a new frontier of directories at the new
/// bound. The census is a LOWER BOUND at 6 exactly as it was at 4 — which is
/// the whole reason [`Completeness`] exists rather than a deeper constant: no
/// bound makes this tree complete, so the answer has to SAY it is a floor.
///
/// The COST ceiling is unchanged and remains [`MAX_DIRS_VISITED`], which is
/// absolute: a deeper bound can enumerate more directories but can never become
/// unbounded work, and hitting the ceiling is reported as
/// [`Completeness::Truncated`] rather than as a smaller answer.
const SCRATCHPAD_MAX_WALK_DEPTH: u32 = 6;

/// Hard ceiling on directories visited in one enumeration, whatever root is
/// being walked. Hitting it sets [`Enumeration::truncated`], which the survey
/// renders as an explicit "this list is incomplete" — an under-count is never
/// reported silently.
///
/// This is the ABSOLUTE cost bound. Per-root depth bounds
/// ([`ReapRootKind::max_walk_depth`]) shape *which* subtrees are reachable; this
/// one caps how much work any of them may cost.
const MAX_DIRS_VISITED: usize = 200_000;

/// Ceiling on the failed reads the enumeration RECORDS. The count
/// ([`Enumeration::read_errors_total`]) is never capped — only the per-path
/// detail list is, because it is held in memory for the whole walk and
/// serialized verbatim into every `GET /disk/reclaimable` response. Bounded by
/// [`MAX_DIRS_VISITED`] alone, one locked subtree could put six figures of
/// paths on the wire.
///
/// The number is a diagnosis budget, not a measurement: past a few dozen
/// samples the next unreadable path tells an operator nothing the count does
/// not already say.
pub(crate) const MAX_RECORDED_READ_ERRORS: usize = 100;

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
/// system**: `cargo-guard`'s shared agent dir (`target-agent`), the arming
/// prove's persistent per-repo target (`target-arming`, qontinui-claude-config
/// `scripts/lib/arming_runs.py`; plan
/// `2026-11-06-arming-prove-shared-target-lock-starvation` D1 keeps it WARM on
/// purpose, so reaping it would turn every prove back into a cold build) and
/// the supervisor's build pool. The named dirs are matched on the basename;
/// `target-pool` on any path component, so `target-pool/slot-0` and
/// `target-pool/lkg` are both covered.
const BUILD_POOL_BASENAMES: &[&str] = &["target-agent", "target-arming"];
const BUILD_POOL_COMPONENT: &str = "target-pool";

/// Ceiling on the directory entries one family discovery pass reads
/// ([`scratch_family_under`]). Discovery lists two levels of the scratch tree,
/// and every account directory there holds thousands of loose files beside its
/// `claude-<uid>` tree (measured on merytshost 2026-11-11: 3,601 entries in
/// `scratch/` and 117,974 more across its children, 10,734 in the largest), so
/// the ceiling is sized for that with room to grow — a first cut at 20,000 was
/// hit on the first real read and reported the family incomplete. Reading a
/// directory entry is a `readdir` record, not a stat, so a pass at this scale
/// costs a fraction of a second. It is still a ceiling, and it says so when it
/// bites ([`ScratchFamily::truncated`]) rather than returning a quiet prefix.
const MAX_FAMILY_SCAN_ENTRIES: usize = 1_000_000;

/// True iff `COORD_ORPHAN_TARGET_REAP_ENABLED` is truthy. Default OFF (dry-run).
///
/// Arms the WORKSPACE population only. The scratch population has its own
/// switch ([`scratch_reap_armed`]); see [`root_armed`].
pub fn reap_armed() -> bool {
    workspace_reap_armed_from(std::env::var(REAP_ENABLED_ENV).ok().as_deref())
}

/// [`reap_armed`] over an injected value: truthy is exactly `1`, `true` or
/// `TRUE`. Anything else — unset included — is dry-run.
fn workspace_reap_armed_from(raw: Option<&str>) -> bool {
    matches!(raw, Some("1") | Some("true") | Some("TRUE"))
}

/// True unless `COORD_ORPHAN_TARGET_SCRATCH_REAP_ENABLED` is exactly `0`.
/// Default ON — a reclaim nobody switched on is a reclaim the product does not
/// have (served policy `engineering-priorities` `capability-ships-enabled`).
///
/// The workspace population stays default-OFF behind [`reap_armed`] because its
/// arming prerequisite — coord's retention pin is not honoured here, so a
/// pinned worktree's `target-<slug>` is reapable — is a property of WORKSPACE
/// worktrees. A scratch target has no coord worktree row to pin, so the two
/// populations do not share that gap, and the scratch arm is further guarded
/// per candidate by the session-liveness and holder gates
/// ([`classify_candidate`]).
pub fn scratch_reap_armed() -> bool {
    scratch_reap_armed_from(std::env::var(SCRATCH_REAP_ENABLED_ENV).ok().as_deref())
}

/// [`scratch_reap_armed`] over an injected value. Only the exact string `0`
/// disables; a mistyped `false` or ` 0` does NOT silently disarm a default-on
/// capability, which is the same strictness the switch's documentation states.
fn scratch_reap_armed_from(raw: Option<&str>) -> bool {
    raw != Some("0")
}

/// A warning for a [`SCRATCH_REAP_ENABLED_ENV`] value that reads like an attempt
/// to switch the arm off but is not exactly `0`. The arm is default ON, so an
/// operator who typed `false` or `off` to stop deletion would otherwise get
/// deletion, silently.
fn scratch_switch_warning(raw: Option<&str>) -> Option<String> {
    match raw {
        None | Some("0") | Some("1") => None,
        Some(other) => Some(format!(
            "{SCRATCH_REAP_ENABLED_ENV}={other:?} is not exactly `0`, so the scratch reclaim \
             stays ON — only the exact value `0` disables it"
        )),
    }
}

/// Whether the destructive arm is open for a root of `kind`, from the two
/// switches' raw values. **Each population reads only its own switch** — the
/// workspace flag never arms scratch and the scratch flag never arms the
/// workspace — and a test pins exactly that.
fn root_armed(kind: ReapRootKind, workspace_raw: Option<&str>, scratch_raw: Option<&str>) -> bool {
    match kind {
        ReapRootKind::Workspace => workspace_reap_armed_from(workspace_raw),
        ReapRootKind::HarnessScratchpad => scratch_reap_armed_from(scratch_raw),
    }
}

/// The pressure threshold, in percent-used, from [`PRESSURE_PCT_ENV`].
/// A non-finite or out-of-range value falls back to the default rather than
/// producing an axis that can never fire (or always fires).
fn pressure_pct() -> f64 {
    std::env::var(PRESSURE_PCT_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 100.0)
        .unwrap_or(DEFAULT_PRESSURE_PCT)
}

/// Who owns an enumerated path, relative to the account this process runs as.
///
/// Captured during the walk from the directory record it already reads, so it
/// costs no extra syscall. It exists because enumeration root 2 lives under
/// a **sticky, world-writable** `<tmp>`: unlike the workspace root, a path found
/// there may belong to another account entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerProbe {
    /// Owned by the uid this process runs as.
    Invoking,
    /// Owned by a DIFFERENT uid. Reported (the bytes are real), never acted on.
    Foreign { uid: u32 },
    /// This platform has no POSIX uid ownership, so there is no account
    /// boundary for the gate to decide. **Not** a failed reading: it is a
    /// positive statement that the question does not apply here, in the same
    /// way [`GitProbe::Absent`] is a positive "not a repo boundary". Every root
    /// this module enumerates on such a platform is reachable by the invoking
    /// account by construction.
    NotApplicable,
}

/// The effective uid of this process. `geteuid(2)` — the EFFECTIVE uid, because
/// that is the identity the kernel actually checks a write against.
#[cfg(unix)]
pub fn invoking_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, touches no caller memory, and is
    // documented never to fail.
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn owner_of(meta: &std::fs::Metadata) -> OwnerProbe {
    use std::os::unix::fs::MetadataExt;
    let uid = meta.uid();
    if uid == invoking_uid() {
        OwnerProbe::Invoking
    } else {
        OwnerProbe::Foreign { uid }
    }
}

#[cfg(not(unix))]
fn owner_of(_meta: &std::fs::Metadata) -> OwnerProbe {
    OwnerProbe::NotApplicable
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
/// follow into a junctioned dir. The walk tests the reparse bit from the entry's
/// directory record and calls [`has_cargo_marker`] directly, so only tests use
/// this whole-path form.
#[cfg(test)]
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
    /// Which account owns this path, captured during the walk. Anything but
    /// [`OwnerProbe::Invoking`] / [`OwnerProbe::NotApplicable`] loses the verb —
    /// see [`SkipReason::ForeignOwner`].
    pub owner: OwnerProbe,
    /// The harness-scratchpad root this candidate was found under, or `None`
    /// for a workspace candidate. `Some` is what puts a candidate through the
    /// SCRATCH gates in [`classify_candidate`]: it must sit under a
    /// `<project>/<session-uuid>/` directory ([`SkipReason::NotSessionScoped`]),
    /// that session must be idle on every account's transcript
    /// ([`SkipReason::SessionLive`] / [`SkipReason::SessionUnknown`]), and no
    /// live process may hold the target ([`SkipReason::Building`] /
    /// [`SkipReason::HeldByProcess`] / [`SkipReason::LockStateUnknown`]).
    pub family_root: Option<PathBuf>,
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
    ///
    /// **A bounded SAMPLE, not the whole set** — see
    /// [`MAX_RECORDED_READ_ERRORS`]. [`Self::read_errors_total`] carries how
    /// many there actually were, so a cap can never shrink the reported count.
    pub read_errors: Vec<(PathBuf, String)>,
    /// How many reads failed, counted without a bound. `> read_errors.len()`
    /// means the list above is a sample.
    ///
    /// The walk descends to its root's depth bound
    /// ([`ReapRootKind::max_walk_depth`]) over up to [`MAX_DIRS_VISITED`]
    /// directories and pushes one entry per unreadable `.git`, so an
    /// antivirus-locked or permission-locked subtree can produce errors by the
    /// thousand — each one a path plus a sentence, held in memory here and
    /// serialized whole into every `GET /disk/reclaimable` response. The count
    /// is what the honesty contract needs; the list is a sample for diagnosis.
    pub read_errors_total: usize,
    /// Directory ENTRIES that errored mid-iteration, after their `read_dir`
    /// opened successfully. Distinct from [`Self::read_errors`], which records
    /// only open failures: a directory can open and then fail per entry, and
    /// `entries.flatten()` would drop those silently. `> 0` ⇒ this walk saw
    /// less than the directory holds.
    pub entry_errors: usize,
    /// Directories NOT descended into because the walk's depth bound
    /// ([`ReapRootKind::max_walk_depth`]) was reached. A target root below the
    /// bound is simply absent from `candidates`, so the bound has to be counted
    /// or its effect is invisible.
    ///
    /// `> 0` ⇒ the candidate list is a LOWER BOUND. Counting it is necessary
    /// but not sufficient — see [`Completeness`], which puts the same fact in
    /// the TYPE so a consumer cannot read past it.
    pub depth_limited_dirs: usize,
    /// Reparse points (junctions / symlinks) the walk refused to follow. They
    /// appear neither as candidates nor as errors — by design, since following
    /// one would double-count the tree behind it — so they are counted here
    /// rather than vanishing.
    pub reparse_dirs_skipped: usize,
    /// Bytes of the regular files directly inside the directories this walk
    /// READ — read from the directory record the walk already holds for every
    /// entry, so counting them costs no syscall.
    ///
    /// **Not the size of the tree**, and rendered as such. A target root, a
    /// [`NEVER_DESCEND`] directory, a link and anything past the depth bound
    /// are pruned and contribute nothing, so this is a floor under what the
    /// root holds. It exists so a root that scanned no target roots still
    /// states what it DID read: `scanned=0` beside `walked_gb=60` is a tree
    /// with no cargo targets in it, while `scanned=0` beside `walked_gb=0.00`
    /// is an empty one, and the cycle line used to render both identically.
    pub walked_bytes: u64,
    /// Set by [`enumerate_family_within`]: stamped onto every candidate as
    /// [`Candidate::family_root`] so the classifier can tell a scratch
    /// candidate from a workspace one.
    family_root: Option<PathBuf>,
}

impl Enumeration {
    /// Record one failed read. The COUNT always moves; the per-path detail is
    /// kept only while under [`MAX_RECORDED_READ_ERRORS`], so a locked subtree
    /// cannot put six figures of paths in memory and on the wire. Callers must
    /// go through here rather than pushing directly — a direct push would move
    /// the list without the count and silently un-cap it.
    fn record_read_error(&mut self, path: PathBuf, error: String) {
        self.read_errors_total += 1;
        if self.read_errors.len() < MAX_RECORDED_READ_ERRORS {
            self.read_errors.push((path, error));
        }
    }

    /// How many reads failed, as every honesty branch must count them.
    ///
    /// The SAME fail-closed reader
    /// [`super::disk_survey::ScanStats::read_errors_seen`] applies to the copy
    /// of these two fields that goes on the wire — and the source they are
    /// copied from had none, which is the half that matters more: the survey's
    /// copy is healed at the `build_snapshot` boundary, but
    /// [`run_cycle`] read `read_errors_total` RAW, so a `read_errors` moved
    /// without its counter under-reported in [`ReapSummary`] and in the cycle
    /// log with nothing to catch it.
    ///
    /// Taking the larger fails **closed**: a counter someone forgot to
    /// increment can never shrink the count below the errors actually
    /// recorded, so the failure mode of the split is an over-report of
    /// incompleteness rather than a fabricated measured zero — the one answer
    /// this module exists to make impossible.
    pub fn read_errors_seen(&self) -> usize {
        self.read_errors_total.max(self.read_errors.len())
    }
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
///   * bounded by a caller-supplied depth bound and by [`MAX_DIRS_VISITED`],
///     and says so via [`Enumeration::truncated`] /
///     [`Enumeration::depth_limited_dirs`] when either bound bites.
///
/// Walks to [`WORKSPACE_MAX_WALK_DEPTH`]. Use [`enumerate_all_within`] for any
/// root whose tree is shaped differently — the depth bound belongs to the ROOT,
/// not to this module.
pub fn enumerate_all(root: &Path) -> Enumeration {
    enumerate_all_within(root, WORKSPACE_MAX_WALK_DEPTH)
}

/// [`enumerate_all`] with the depth bound named by the caller, because the two
/// enumeration roots this module walks have genuinely different shapes: the
/// workspace root is flat (`<root>/target-wt-<slug>`), the harness scratchpad
/// tree nests one or two levels deeper. One global constant is necessarily
/// wrong for one of them — see [`ReapRootKind::max_walk_depth`].
pub fn enumerate_all_within(root: &Path, max_depth: u32) -> Enumeration {
    let mut e = Enumeration::default();
    walk(root, 0, max_depth, &WalkCtx::Loose, &mut e);
    e
}

/// [`enumerate_all_within`] for a harness-scratchpad (family) root: every
/// candidate it returns carries [`Candidate::family_root`], so the classifier
/// applies the session-liveness and holder gates to it. The walk itself is the
/// same one — it never follows a link and never descends into a target root —
/// which is the property that keeps a symlink in a family root, pointing at a
/// live checkout's build output, out of the candidate list.
pub fn enumerate_family_within(root: &Path, max_depth: u32) -> Enumeration {
    let mut e = Enumeration {
        family_root: Some(root.to_path_buf()),
        ..Enumeration::default()
    };
    walk(root, 0, max_depth, &WalkCtx::Loose, &mut e);
    e
}

/// Back-compat facade: just the candidate list.
pub fn enumerate_candidates(root: &Path) -> Vec<Candidate> {
    enumerate_all(root).candidates
}

fn walk(dir: &Path, depth: u32, max_depth: u32, ctx: &WalkCtx, e: &mut Enumeration) {
    if e.truncated {
        return;
    }
    if depth > max_depth {
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
            e.record_read_error(dir.to_path_buf(), err.to_string());
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
        // The directory RECORD, not the file: `DirEntry::metadata` never
        // follows a link (it is `lstat` on Unix) and on Windows is read from
        // the `FindNextFile` record with no further system call. The
        // `symlink_metadata(path)` + `is_junction(path)` it replaces opened
        // every entry of a ~180k-directory walk twice, every 15 minutes (plan
        // `2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-fetch-into-empty-dirs` Phase 3).
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => {
                e.entry_errors += 1;
                continue;
            }
        };
        let ft = meta.file_type();
        if ft.is_symlink() || super::census::metadata_is_reparse_point(&meta) {
            // Never followed — a junctioned target root would double-count the
            // tree behind it — but never silent either. Counted only when it
            // resolves to a DIRECTORY (`is_dir` follows the link, unlike the
            // directory record above), i.e. only when it is a branch the walk
            // would otherwise have descended into.
            if path.is_dir() {
                e.reparse_dirs_skipped += 1;
            }
            continue;
        }
        if !ft.is_dir() {
            if ft.is_file() {
                // The record is already in hand, so what the walk READ is free
                // to count — see [`Enumeration::walked_bytes`].
                e.walked_bytes = e.walked_bytes.saturating_add(meta.len());
            }
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
        // The entry is already proven not to be a reparse point (above), so
        // only the cargo marker is left to read: `is_target_root` would
        // re-open the entry to ask `is_junction` again.
        if has_cargo_marker(&path) {
            e.candidates.push(Candidate {
                path,
                class: ctx.class(),
                repo_root: ctx.repo_root(),
                enclosing_git_unreadable: ctx.git_unreadable(),
                // Free: `meta` is the directory record this loop already read.
                owner: owner_of(&meta),
                family_root: e.family_root.clone(),
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
                    e.record_read_error(
                        path.join(".git"),
                        "the `.git` entry could not be read, so this directory's ownership \
                         (canonical checkout / linked worktree / not a repo) is UNKNOWN"
                            .to_string(),
                    );
                    WalkCtx::OpaqueRepo {
                        root: path.clone(),
                        class: ctx.class(),
                    }
                }
            }
        };
        walk(&path, depth + 1, max_depth, &child_ctx, e);
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
        // The directory record, not the file (see `walk`): this sizes every
        // file of every candidate target and used to open each one. On Windows
        // a record's size can lag a file that is open for writing (NTFS updates
        // the directory entry lazily), so a target mid-build can read a little
        // low. Sizes here feed reports, never a reap decision.
        let meta = match entry.metadata() {
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
            // Only a DIRECTORY is tested for the reparse bit, as before: a
            // regular file can carry a non-link reparse tag (WOF compression,
            // dedup, a cloud placeholder) and its bytes still count.
            if super::census::metadata_is_reparse_point(&meta) {
                continue;
            }
            let path = entry.path();
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
    /// **G-foreign-owner** — the path is owned by a uid other than the one this
    /// process runs as. The OUTERMOST boundary: enumeration root 2 lives under a
    /// sticky, world-writable `<tmp>` shared with other accounts, so a path
    /// found there may simply not be ours. Measured and reported; never acted on.
    ForeignOwner,
    /// **G-report-only** — inside a canonical checkout. v1 measures this class
    /// (the largest by bytes) and deliberately gives it no verb.
    ReportOnly,
    /// **G-owner** — the worktree reclaim engine owns this path
    /// (`<worktree>/target`, `<worktree>/src-tauri/target`).
    OwnedByWorktreeReclaim,
    /// **G-owner** — the build system owns this path (`target-agent`,
    /// `target-arming`, or anything under `target-pool/`).
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
    /// **G-session-scope** (scratch roots only) — the target does not sit under
    /// a `<project>/<session-uuid>/` directory of its scratch root, so no
    /// session owns it and there is no liveness to read. An observation about
    /// the path's shape, not a missing reading. Never a candidate, whatever its
    /// size: the steward's census calls the same population `NON_SESSION`.
    NotSessionScoped,
    /// **G-session-live** (scratch roots only) — the owning session is not
    /// idle: its newest transcript, across EVERY account, is younger than the
    /// grace window, or a live process names its uuid or holds its directory.
    SessionLive,
    /// **G-session-live, failed closed** (scratch roots only) — the session's
    /// liveness could not be READ: no account home resolved, or no account
    /// holds a transcript for it, or one could not be stat'ed. Refused, because
    /// a rule that needed a positive DEAD would never fire and a rule that
    /// treated an absent reading as one would delete a live session's build.
    SessionUnknown,
    /// **G-holder** (scratch roots only) — a live process holds the target
    /// itself: its cwd or an open file is under the target, or its
    /// `CARGO_TARGET_DIR` names it. An OBSERVATION of a holder, kept apart from
    /// [`Self::Building`] (a held `.cargo-lock`), because "a build is in flight
    /// here" is a claim about cargo and this one is about any process.
    HeldByProcess,
}

impl SkipReason {
    /// Stable kebab-case wire token.
    pub fn token(self) -> &'static str {
        match self {
            SkipReason::ForeignOwner => "foreign-owner",
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
            SkipReason::NotSessionScoped => "non-session",
            SkipReason::SessionLive => "session-live",
            SkipReason::SessionUnknown => "session-unknown",
            SkipReason::HeldByProcess => "held-by-process",
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
                | SkipReason::SessionUnknown
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
                // Another ACCOUNT owns it. Same consequence as another engine
                // owning it — the verb field names who would act, and this
                // reaper would not.
                | SkipReason::ForeignOwner
        )
    }

    /// One operator-facing sentence. Rendered next to the token so a refusal is
    /// legible without reading this source.
    pub fn detail(self) -> &'static str {
        match self {
            SkipReason::ForeignOwner => {
                "Owned by a different uid than this process runs as. Enumeration reaches into a \
                 sticky, world-writable temp tree that other accounts share, so a path found \
                 there is not necessarily ours. Measured and reported so the bytes are visible; \
                 never removed — acting on another account's files is outside this reaper's \
                 authority, whatever its name or layout suggests."
            }
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
                "The build system owns this name (`target-agent` / `target-arming` / anything \
                 under `target-pool/`) — the shared agent build dir, the arming prove's warm \
                 target or a supervisor pool slot, not \
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
            SkipReason::NotSessionScoped => {
                "Under a scratch root but not under a `<project>/<session-uuid>/` directory of \
                 it, so no session owns this target and there is no liveness to read. Never a \
                 candidate, whatever its size — an unattributable target is not this reaper's \
                 to reason about."
            }
            SkipReason::SessionLive => {
                "The owning session is not idle: its newest transcript on ANY account is \
                 younger than the grace window, or a live process names its uuid or holds its \
                 directory. A session can move accounts, so the newest transcript across every \
                 account decides — never the one in the account whose tree this is."
            }
            SkipReason::SessionUnknown => {
                "The owning session's liveness could NOT be read (no account home resolved, no \
                 account holds a transcript for it, or one could not be stat'ed). Refused — a \
                 refusal on a missing reading, NOT an observation that the session is live."
            }
            SkipReason::HeldByProcess => {
                "A live process holds this target: its working directory or an open file is \
                 under it, or its `CARGO_TARGET_DIR` names it. A dry-run list is a snapshot and \
                 never a delete list, so a held target stays however idle its session looks."
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
    /// What the SCRATCH gates read: the process table, `/proc/locks`, and the
    /// account homes whose transcripts say whether a session is idle. Only a
    /// candidate carrying [`Candidate::family_root`] ever touches it, and its
    /// reads are lazy and memoised per options value, so a cycle with no
    /// scratch candidate past the cheap gates reads none of it.
    scratch: ScratchEnv,
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

    /// Explicit grace, env keep-list, real git probe, the real process table.
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            keep_names: keep_names_from_env(),
            worktree_is_dirty,
            dirty_memo: Arc::default(),
            scratch: ScratchEnv::ambient(),
        }
    }

    /// Explicit grace and an injected dirty probe, with an empty keep-list — the
    /// seam the boundary-gate tests drive, so they never need a real git repo.
    ///
    /// The scratch environment is [`ScratchEnv::fail_closed`]: with no process
    /// table and no account home, every scratch candidate is refused as
    /// UNKNOWN. A test that wants a scratch candidate to clear the scratch gates
    /// must say so with [`Self::with_scratch_env`], so the permissive reading is
    /// always a deliberate one.
    pub fn with_probe(grace: Duration, worktree_is_dirty: fn(&Path) -> DirtyProbe) -> Self {
        Self {
            grace,
            keep_names: Vec::new(),
            worktree_is_dirty,
            dirty_memo: Arc::default(),
            scratch: ScratchEnv::fail_closed(),
        }
    }

    /// Replace the scratch environment — the seam that points the scratch gates
    /// at a fixture process table and fixture account homes.
    pub fn with_scratch_env(mut self, scratch: ScratchEnv) -> Self {
        self.scratch = scratch;
        self
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

    // G-foreign-owner, FIRST — the account boundary is outside every engine
    // boundary below it. The gates that follow all answer "which of OUR engines
    // owns this path?", a question that presupposes the path is ours at all.
    // Enumeration root 2 lives under a sticky, world-writable `<tmp>` shared
    // with other accounts (1,827 of `/tmp`'s direct children belong to `runner`
    // on this box), so that presupposition is no longer free. Ordered ahead of
    // the build-pool gate deliberately: a `target-agent` belonging to ANOTHER
    // account is refused for the reason we can actually establish about it,
    // rather than by claiming our build system owns someone else's directory.
    if let OwnerProbe::Foreign { .. } = c.owner {
        return Err(SkipReason::ForeignOwner);
    }

    // G-owner (build pool), evaluated on the PATH alone — the pool is protected
    // by what it IS, so no read failure anywhere can dissolve this boundary.
    if BUILD_POOL_BASENAMES.contains(&basename)
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
///
/// ## A candidate under a scratch root takes two more gates
///
/// One carrying [`Candidate::family_root`] must also (1) sit under a
/// `<project>/<session-uuid>/` directory of that root, checked right after the
/// boundary gates because it is pure and decisive, and (2), once the disk gates
/// below have cleared it, belong to a session that is idle on every account and
/// whose target no live process holds ([`scratch_liveness_verdict`]). Both are
/// refusals on the same footing as the rest: a missing reading refuses.
pub fn classify_candidate(c: &Candidate, opts: &ClassifyOptions) -> Result<(), SkipReason> {
    boundary_verdict(c, opts)?;
    let session = match c.family_root.as_deref() {
        Some(family) => {
            Some(scratch_session_of(family, &c.path).ok_or(SkipReason::NotSessionScoped)?)
        }
        None => None,
    };
    classify_path(&c.path, opts)?;
    match session {
        Some(s) => scratch_liveness_verdict(&c.path, &s, opts),
        None => Ok(()),
    }
}

/// The checks that must hold AGAIN immediately before an armed scratch removal:
/// the disk gates (pin, reparse, layout, held lock, idle age) and the scratch
/// gates, over a process table and transcript read taken NOW rather than at
/// classification time. Cheaper than [`classify_candidate`] on purpose — the
/// boundary gates (ownership, `git status`) are properties of where the target
/// sits, which a session resuming cannot change; what a resume changes is
/// exactly what this re-reads.
fn reread_scratch_candidate(c: &Candidate, opts: &ClassifyOptions) -> Result<(), SkipReason> {
    let Some(family) = c.family_root.as_deref() else {
        return Ok(());
    };
    let session = scratch_session_of(family, &c.path).ok_or(SkipReason::NotSessionScoped)?;
    // The shape and the transcript lookup above are LEXICAL, and the holder
    // reads below are on resolved paths. If a component between the family root
    // and the target has become a link since the walk, those two describe
    // different directories, and the removal that follows would follow the
    // link. The target must still resolve to exactly where the walk found it.
    let unchanged = match (
        std::fs::canonicalize(family),
        std::fs::canonicalize(&c.path),
    ) {
        (Ok(canon_family), Ok(canon_target)) => c
            .path
            .strip_prefix(family)
            .is_ok_and(|rel| canon_target == canon_family.join(rel)),
        _ => false,
    };
    if !unchanged {
        return Err(SkipReason::Reparse);
    }
    let fresh = opts.clone().with_scratch_env(opts.scratch.fresh());
    classify_path(&c.path, &fresh)?;
    scratch_liveness_verdict(&c.path, &session, &fresh)
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

// ---------------------------------------------------------------------------
// The on-disk scratch family and the gates a scratch candidate must clear.
//
// Plan `2026-11-01-session-scratch-on-disk-has-no-reclaim-arm-so-the-largest-
// idle-bytes-hold-every-build-behind-the-reserve`, D1 / D3 / D4 and Phase 4.
// ---------------------------------------------------------------------------

/// Everything [`scratch_family_under`] found, and everything it could not.
///
/// A discovery that read nothing because a directory was unreadable must not
/// render as one that read everything and found no member, so the problems are
/// carried beside the members rather than dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScratchFamily {
    /// Every `<scratch>/<account>/claude-<uid>` and
    /// `<scratch>/<account>/<lineage>/claude-<uid>` that is a REAL directory,
    /// sorted. Depth 2 is a family member too — the one-session-lifecycle plan
    /// (Phase 126) puts `claude-<uid>` one level further down.
    pub members: Vec<PathBuf>,
    /// A symlink to a directory where a member, an account directory or a
    /// lineage directory would be. Never a member and never descended into: a
    /// link here is how a scratch tree comes to hold a live checkout, and a walk
    /// that followed it would count one tree twice and delete a build dir it
    /// does not own.
    pub links_skipped: usize,
    /// Directories that could not be listed or stat'ed. `> 0` ⇒ the member list
    /// may be missing a tree.
    pub unreadable: usize,
    /// [`MAX_FAMILY_SCAN_ENTRIES`] was reached. The member list is a prefix.
    pub truncated: bool,
}

impl ScratchFamily {
    /// The member list may be missing a tree.
    pub fn incomplete(&self) -> bool {
        self.unreadable > 0 || self.truncated
    }
}

/// The real (non-link) child directories of `dir`, sorted. Spends from
/// `budget`, and records a missing read on `fam` — except a `dir` that does not
/// exist, which is simply absent and has no children. A link to a directory is
/// counted in [`ScratchFamily::links_skipped`] unless it is named `member_leaf`,
/// which [`probe_family_member`] counts itself.
fn real_child_dirs_budgeted(
    dir: &Path,
    member_leaf: &str,
    budget: &mut usize,
    fam: &mut ScratchFamily,
) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => {
            fam.unreadable += 1;
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        if *budget == 0 {
            fam.truncated = true;
            break;
        }
        *budget -= 1;
        let Ok(entry) = entry else {
            fam.unreadable += 1;
            continue;
        };
        // `DirEntry::file_type` never follows a link, so a symlink to a
        // directory is `is_symlink`, not `is_dir`.
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => out.push(entry.path()),
            // A link to a directory where an account or lineage directory would
            // be: never followed, but counted — a `scratch/<account>` that is a
            // link onto a bigger disk would otherwise read as an empty family
            // with nothing incomplete, the shape this discovery exists to end.
            Ok(ft)
                if ft.is_symlink() && entry.file_name() != member_leaf && entry.path().is_dir() =>
            {
                fam.links_skipped += 1
            }
            Ok(_) => {}
            Err(_) => fam.unreadable += 1,
        }
    }
    out.sort();
    out
}

/// Probe one candidate family member. A real directory joins the family, a
/// link is counted and refused, a missing path is nothing, and any other
/// failure is a missing reading.
fn probe_family_member(path: PathBuf, fam: &mut ScratchFamily) {
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_symlink() => fam.links_skipped += 1,
        Ok(m) if m.is_dir() => fam.members.push(path),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => fam.unreadable += 1,
    }
}

/// Every `claude-<uid>` tree under `<qontinui_dir>/scratch`, which is where the
/// account shortcuts point `TMPDIR` — so where the harness's per-session
/// scratchpads (`<project>/<session>/scratchpad/…`) actually go.
///
/// ## Why this exists
///
/// The reaper's scratch root used to be `<tmp>/claude-<uid>` alone. On a box
/// whose sessions are launched with `TMPDIR=<qontinui_dir>/scratch/<account>`
/// that tree is empty, and the cycle said so as
/// `harness-scratchpad:present(scanned=0,candidates=0,census=complete)` — a
/// complete census of the wrong directory, reading as coverage of the right
/// one. On merytshost the right one held 417 GB.
///
/// ## Shape
///
/// `<scratch>/<account>/claude-<uid>` (depth 1) and
/// `<scratch>/<account>/<lineage>/claude-<uid>` (depth 2). **Pure over its
/// arguments** — no env, no ambient home — so a fixture `qontinui_dir` drives
/// every arm. Reads at most [`MAX_FAMILY_SCAN_ENTRIES`] directory entries, and
/// says when that bit; never follows a link.
pub fn scratch_family_under(qontinui_dir: &Path, uid: u32) -> ScratchFamily {
    scratch_family_with_budget(qontinui_dir, uid, MAX_FAMILY_SCAN_ENTRIES)
}

/// [`scratch_family_under`] with the entry ceiling named by the caller — the
/// seam that lets a test hit the ceiling without creating twenty thousand
/// directories.
fn scratch_family_with_budget(qontinui_dir: &Path, uid: u32, budget: usize) -> ScratchFamily {
    let mut fam = ScratchFamily::default();
    let leaf = format!("claude-{uid}");
    let mut budget = budget;
    let accounts =
        real_child_dirs_budgeted(&qontinui_dir.join("scratch"), &leaf, &mut budget, &mut fam);
    for account in accounts {
        probe_family_member(account.join(&leaf), &mut fam);
        for lineage in real_child_dirs_budgeted(&account, &leaf, &mut budget, &mut fam) {
            // `<account>/claude-<uid>` is the depth-1 member itself, never a
            // lineage directory to look inside.
            if lineage.file_name().is_some_and(|n| n == leaf.as_str()) {
                continue;
            }
            probe_family_member(lineage.join(&leaf), &mut fam);
        }
    }
    fam.members.sort();
    fam
}

/// A scratch session directory is named by its uuid, and anything else under a
/// project directory is not a session (the steward's `NON_SESSION`): 8-4-4-4-12
/// hex, either case.
fn is_session_uuid(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Which session a scratch target belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScratchSession {
    /// The encoded-cwd directory the session lives under — also the name of
    /// the directory its transcript lives under in every account home.
    project: String,
    uuid: String,
    /// `<family>/<project>/<uuid>`.
    dir: PathBuf,
}

/// The session `target` belongs to, or `None` when it is not under a
/// `<project>/<uuid>/…` directory of `family_root` with at least one more
/// level — the target itself.
///
/// A target directly under the family root (`coord-target-<hash>`), directly
/// under a project directory, or under a non-uuid directory has no owner whose
/// liveness could be read, and so is never a candidate.
fn scratch_session_of(family_root: &Path, target: &Path) -> Option<ScratchSession> {
    use std::path::Component;
    let rel = target.strip_prefix(family_root).ok()?;
    let mut names: Vec<&str> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(n) => names.push(n.to_str()?),
            _ => return None,
        }
    }
    let [project, uuid, _, ..] = names.as_slice() else {
        return None;
    };
    if !is_session_uuid(uuid) {
        return None;
    }
    Some(ScratchSession {
        project: (*project).to_string(),
        uuid: (*uuid).to_string(),
        dir: family_root.join(project).join(uuid),
    })
}

/// The `projects/` subdirectory every account home keeps its transcripts under.
const TRANSCRIPT_PROJECTS_DIR: &str = "projects";

/// What the transcripts say about how long a session has been idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptRead {
    /// Time since the NEWEST copy of the session's transcript, across every
    /// account home that holds one, was written.
    Idle(Duration),
    /// No account holds a copy, or one could not be read. Not an age.
    Unknown,
}

/// The newest `<home>/projects/<project>/<uuid>.jsonl` across ALL `homes`.
///
/// **Across all of them, not the account whose scratch tree this is.** A
/// session can move accounts: `session_carry::copy_transcript` leaves a FROZEN
/// copy behind in the old one, so the old account's transcript reads idle for a
/// session that is running right now under the new one. The measured idle
/// population in the plan (66 of 68 GB on one account) was binned by exactly
/// that single-account mtime.
///
/// A missing copy in one home is normal; a copy that could not be read is not —
/// it could be the newer one — and a session with no copy anywhere is UNKNOWN,
/// never old.
fn newest_transcript_age(homes: &[PathBuf], project: &str, uuid: &str) -> TranscriptRead {
    let file = format!("{uuid}.jsonl");
    let mut newest: Option<std::time::SystemTime> = None;
    for home in homes {
        let path = home.join(TRANSCRIPT_PROJECTS_DIR).join(project).join(&file);
        match std::fs::metadata(&path) {
            Ok(meta) => match meta.modified() {
                Ok(t) => newest = Some(newest.map_or(t, |cur| cur.max(t))),
                Err(_) => return TranscriptRead::Unknown,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return TranscriptRead::Unknown,
        }
    }
    match newest {
        // An mtime in the future (`elapsed` errors) is the freshest possible
        // reading, not a missing one.
        Some(t) => TranscriptRead::Idle(t.elapsed().unwrap_or(Duration::ZERO)),
        None => TranscriptRead::Unknown,
    }
}

/// Where the account homes come from.
#[derive(Clone)]
enum HomesSource {
    /// The runner's own discovery — `CLAUDE_CONFIG_DIR`, the settings roster,
    /// then every `~/.claude-*`: the same rule the transcript readers use.
    /// Resolved on first use, not at construction, so building a
    /// [`ClassifyOptions`] reads no settings.
    Ambient,
    /// Injected. `None` is "the accounts root could not be resolved".
    Fixed(Option<Vec<PathBuf>>),
}

#[derive(Default)]
struct ScratchMemo {
    homes: std::sync::OnceLock<Option<Vec<PathBuf>>>,
    procs: Mutex<Option<Arc<ProcSnapshot>>>,
}

/// What the scratch gates read, behind seams.
///
/// Production reads `/proc` and the discovered account homes; a test hands it a
/// fixture `/proc`-shaped directory and a fixture home, which is how every arm —
/// a live session, a held target, an unreadable table — is driven without a
/// process or a session to hold.
#[derive(Clone)]
pub struct ScratchEnv {
    proc_root: PathBuf,
    homes: HomesSource,
    memo: Arc<ScratchMemo>,
}

impl ScratchEnv {
    /// The real `/proc` and the discovered account homes.
    pub fn ambient() -> Self {
        Self {
            proc_root: PathBuf::from("/proc"),
            homes: HomesSource::Ambient,
            memo: Arc::default(),
        }
    }

    /// An injected process table and account homes. `homes: None` is an
    /// unresolved accounts root.
    pub fn fixed(proc_root: PathBuf, homes: Option<Vec<PathBuf>>) -> Self {
        Self {
            proc_root,
            homes: HomesSource::Fixed(homes),
            memo: Arc::default(),
        }
    }

    /// No process table and no account home: every scratch candidate that
    /// reaches the liveness gates is refused as UNKNOWN.
    pub fn fail_closed() -> Self {
        Self::fixed(PathBuf::new(), None)
    }

    /// The same inputs with EMPTY memos, so the next read goes to the machine.
    fn fresh(&self) -> Self {
        Self {
            proc_root: self.proc_root.clone(),
            homes: self.homes.clone(),
            memo: Arc::default(),
        }
    }

    /// The account homes, resolved once per options value. `None` ⇒ unresolved.
    fn homes(&self) -> Option<&[PathBuf]> {
        self.memo
            .homes
            .get_or_init(|| match &self.homes {
                HomesSource::Fixed(h) => h.clone(),
                HomesSource::Ambient => {
                    let found =
                        qontinui_runner_lib::session_archive::discovery::discover_account_homes_from_env(
                            &crate::settings::get_claude_config_dirs(),
                        );
                    if found.is_empty() {
                        None
                    } else {
                        Some(found.into_iter().map(|h| h.config_dir).collect())
                    }
                }
            })
            .as_deref()
    }

    /// The process table, read at most once per memo. A poisoned lock degrades
    /// to an un-memoised read — never to an answer.
    fn procs(&self) -> Arc<ProcSnapshot> {
        let Ok(mut slot) = self.memo.procs.lock() else {
            return Arc::new(ProcSnapshot::read(&self.proc_root));
        };
        slot.get_or_insert_with(|| Arc::new(ProcSnapshot::read(&self.proc_root)))
            .clone()
    }

    /// Own-uid processes whose `/proc` entries were denied, in the table read so
    /// far. 0 when none has been read.
    pub fn opaque_procs_seen(&self) -> usize {
        self.memo
            .procs
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|p| p.opaque))
            .unwrap_or(0)
    }
}

/// One own-uid process, reduced to what the gates ask about it.
#[derive(Debug, Default)]
struct ProcEntry {
    /// `/proc/<pid>/cmdline`, ASCII-lowercased, NULs kept: a uuid named
    /// anywhere in argv is found by substring.
    cmdline: Vec<u8>,
    /// `CLAUDE_CODE_SESSION_ID` from its environment.
    session_env: Option<String>,
    /// `CARGO_TARGET_DIR` from its environment, made absolute against its cwd
    /// and canonicalised where it exists.
    cargo_target_dir: Option<PathBuf>,
    cwd: Option<PathBuf>,
    /// The binary it is executing (`/proc/<pid>/exe`). A process running a
    /// binary built into a target holds nothing else in it — no fd, no flock, a
    /// cwd elsewhere, a relative argv[0] — and is the holder most likely to be
    /// missed without this read.
    exe: Option<PathBuf>,
    /// Absolute link targets of its open fds. Pipes, sockets and anon inodes
    /// are not paths and are not kept.
    fds: Vec<PathBuf>,
}

/// One read of the process table, shared by every candidate of a cycle.
#[derive(Debug, Default)]
struct ProcSnapshot {
    /// The table could be listed at all. `false` ⇒ every question about
    /// holders and sessions is UNKNOWN.
    listed: bool,
    procs: Vec<ProcEntry>,
    /// Own-uid pids this uid was denied — see
    /// [`ReapSummary::scratch_opaque_procs`].
    opaque: usize,
    /// `/proc/locks`, parsed by the census's own parser. `None` ⇒ unreadable.
    locks: Option<HashMap<super::cargo_locks::LockKey, super::cargo_locks::LockEntry>>,
}

/// What the process table says about a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    /// A held flock on one of the target's `.cargo-lock` files.
    Locked,
    /// A process's cwd, open fd or `CARGO_TARGET_DIR` is under (or names) it.
    Held,
    /// Nothing holds it.
    Free,
    /// A read the answer depends on failed.
    Unknown,
}

/// Parse the two variables the gates need out of a `/proc/<pid>/environ` blob.
fn read_environ(raw: &[u8], entry: &mut ProcEntry) {
    for var in raw.split(|b| *b == 0) {
        if let Some(v) = var.strip_prefix(b"CLAUDE_CODE_SESSION_ID=") {
            entry.session_env = Some(String::from_utf8_lossy(v).to_ascii_lowercase());
        } else if let Some(v) = var.strip_prefix(b"CARGO_TARGET_DIR=") {
            let v = String::from_utf8_lossy(v);
            if !v.is_empty() {
                entry.cargo_target_dir = Some(PathBuf::from(v.into_owned()));
            }
        }
    }
}

/// Ceiling on the open-fd links one process-table read follows. A table with
/// more is UNKNOWN rather than a quiet prefix: the census's own open-file scan
/// stops at 100,000 and reports a null owner, and a gate that guards a deletion
/// must not read a truncated table as a complete one.
const MAX_PROC_FDS: usize = 500_000;

/// How a failed `/proc/<pid>/…` read is to be understood.
enum ProcFailure {
    /// The process exited between the listing and the read: it holds nothing.
    Vanished,
    /// The kernel refused this uid. See [`ProcSnapshot::read`].
    Denied,
    /// Anything else (`EMFILE`, `EIO`, `ENOMEM`, …). A reading that failed for a
    /// reason nobody has accounted for is a missing reading, never an absence.
    Other,
}

fn proc_failure(e: &std::io::Error) -> ProcFailure {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return ProcFailure::Denied;
    }
    if e.kind() == std::io::ErrorKind::NotFound {
        return ProcFailure::Vanished;
    }
    // `ESRCH` is what `/proc/<pid>/environ` answers for a process that is
    // exiting, and `std` does not map it to `NotFound`.
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ESRCH) {
        return ProcFailure::Vanished;
    }
    ProcFailure::Other
}

/// What reading one process came to.
enum ProcRead {
    Entry {
        entry: ProcEntry,
        denied: bool,
    },
    /// It vanished mid-read.
    Gone,
    /// A read failed for a reason that is neither a vanished process nor a
    /// refusal: the table as a whole can no longer be called read.
    Unknown,
}

/// Read one `/proc/<pid>` directory. `exe` is the one field whose absence is
/// not a vanished process (a zombie has none while `cwd` already says so).
fn read_one_proc(dir: &Path, fd_budget: &mut usize) -> ProcRead {
    let mut pe = ProcEntry::default();
    let mut denied = false;
    macro_rules! field {
        ($read:expr, |$v:ident| $assign:expr) => {
            match $read {
                Ok($v) => $assign,
                Err(e) => match proc_failure(&e) {
                    ProcFailure::Denied => denied = true,
                    ProcFailure::Vanished => return ProcRead::Gone,
                    ProcFailure::Other => return ProcRead::Unknown,
                },
            }
        };
    }
    field!(std::fs::read(dir.join("cmdline")), |b| pe.cmdline =
        b.to_ascii_lowercase());
    field!(std::fs::read(dir.join("environ")), |b| read_environ(
        &b, &mut pe
    ));
    field!(std::fs::read_link(dir.join("cwd")), |p| pe.cwd = Some(p));
    match std::fs::read_link(dir.join("exe")) {
        Ok(p) => pe.exe = Some(p),
        Err(e) => match proc_failure(&e) {
            ProcFailure::Denied => denied = true,
            ProcFailure::Vanished => {}
            ProcFailure::Other => return ProcRead::Unknown,
        },
    }
    match std::fs::read_dir(dir.join("fd")) {
        Ok(fds) => {
            for fd in fds {
                let target = match fd {
                    Ok(fd) => std::fs::read_link(fd.path()),
                    Err(e) => Err(e),
                };
                match target {
                    Ok(t) => {
                        if *fd_budget == 0 {
                            return ProcRead::Unknown;
                        }
                        *fd_budget -= 1;
                        if t.is_absolute() {
                            pe.fds.push(t);
                        }
                    }
                    Err(e) => match proc_failure(&e) {
                        ProcFailure::Denied => denied = true,
                        // An fd closed between the listing and the link read.
                        ProcFailure::Vanished => {}
                        ProcFailure::Other => return ProcRead::Unknown,
                    },
                }
            }
        }
        Err(e) => match proc_failure(&e) {
            ProcFailure::Denied => denied = true,
            ProcFailure::Vanished => return ProcRead::Gone,
            ProcFailure::Other => return ProcRead::Unknown,
        },
    }
    if let Some(raw) = pe.cargo_target_dir.take() {
        // Relative to the holder's cwd as it is NOW, not as it was at exec: a
        // process that has since changed directory can resolve elsewhere. The
        // flock read backstops that, because cargo holds its lock either way.
        let abs = if raw.is_absolute() {
            Some(raw)
        } else {
            pe.cwd.as_ref().map(|cwd| cwd.join(raw))
        };
        pe.cargo_target_dir = abs.map(|p| std::fs::canonicalize(&p).unwrap_or(p));
    }
    ProcRead::Entry { entry: pe, denied }
}

impl ProcSnapshot {
    /// Read the own-uid process table under `proc_root`.
    ///
    /// **What counts as unreadable.** A table that cannot be LISTED is
    /// [`ProcSnapshot::listed`]` == false` and answers UNKNOWN for everything,
    /// and so is one in which any read failed for a reason other than the two
    /// below, or whose open-fd links exceed [`MAX_PROC_FDS`]. Per process: one
    /// that vanished mid-read held nothing, and is skipped. A permission
    /// denial on an own-uid process is the kernel saying this uid may not
    /// inspect it — `systemd --user`, `ssh-agent`, an `sshd-session` all answer
    /// so on this box — and is TOLERATED but COUNTED. Refusing on any of them
    /// would leave the gates inert on every box that runs a user manager; a
    /// live cargo build is caught regardless of its `/proc` entry by the
    /// `/proc/locks` read, which lists every process's flocks.
    ///
    /// The count is a LOWER BOUND: a process that is not dumpable has a
    /// root-owned `/proc/<pid>` directory, so it is dropped by the own-uid
    /// filter before any read is attempted and is not counted.
    fn read(proc_root: &Path) -> Self {
        let mut snap = ProcSnapshot::default();
        let Ok(listing) = std::fs::read_dir(proc_root) else {
            return snap;
        };
        snap.listed = true;
        let mut fd_budget = MAX_PROC_FDS;
        for entry in listing {
            // A listing that errors mid-way could have dropped any pid.
            let Ok(entry) = entry else {
                return ProcSnapshot::default();
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            // Own-uid only: another account's processes are not this reaper's
            // to reason about, and its `/proc/<pid>/fd` is unreadable anyway.
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(e) => match proc_failure(&e) {
                    ProcFailure::Vanished => continue,
                    _ => return ProcSnapshot::default(),
                },
            };
            if matches!(owner_of(&meta), OwnerProbe::Foreign { .. }) {
                continue;
            }
            match read_one_proc(&entry.path(), &mut fd_budget) {
                ProcRead::Entry { entry, denied } => {
                    if denied {
                        snap.opaque += 1;
                    }
                    snap.procs.push(entry);
                }
                ProcRead::Gone => {}
                ProcRead::Unknown => return ProcSnapshot::default(),
            }
        }
        snap.locks = std::fs::read_to_string(proc_root.join("locks"))
            .ok()
            .map(|text| super::cargo_locks::parse_proc_locks(&text));
        snap
    }

    /// Does a live process name this session — its uuid in argv or in
    /// `CLAUDE_CODE_SESSION_ID`, or a cwd or open fd under its directory?
    fn names_session(&self, session: &ScratchSession, canon_dir: &Path) -> bool {
        let needle = session.uuid.to_ascii_lowercase();
        self.procs.iter().any(|p| {
            contains_bytes(&p.cmdline, needle.as_bytes())
                || p.session_env.as_deref() == Some(needle.as_str())
                || p.cwd.as_deref().is_some_and(|c| c.starts_with(canon_dir))
                || p.exe.as_deref().is_some_and(|x| x.starts_with(canon_dir))
                || p.fds.iter().any(|f| f.starts_with(canon_dir))
        })
    }

    /// Does anything hold `canon_target`?
    ///
    /// Two independent reads, because each is blind where the other is not. A
    /// held flock on any of the target's `.cargo-lock` files is the proper
    /// G-live read on Linux — the reaper's own `cargo_lock_held` is a
    /// write-open, which an `flock` does not block — and `/proc/locks` lists
    /// every process's, readable or not. A process whose cwd, executable,
    /// open fd or `CARGO_TARGET_DIR` is under the target covers what holds the
    /// directory without holding a lock: a shell parked in it, an editor, a
    /// binary built into it and still running, a `cargo` that has not taken its
    /// lock yet.
    fn holds_target(&self, canon_target: &Path) -> Holder {
        use super::cargo_locks::lookup_lock;
        // The flock read can fail in three places — `/proc/locks`, listing the
        // target for its lock files, statting one. A failure is remembered
        // rather than returned at once, because what a process is OBSERVED to
        // hold below outranks a reading that could not be made.
        let mut unknown = false;
        match (&self.locks, cargo_lock_files(canon_target)) {
            (Some(locks), Some(lock_files)) => {
                for file in lock_files {
                    match lock_key_of_path(&file) {
                        // An inode-only match (an anonymous device) is a
                        // candidate, not a proof — and refusing on a candidate
                        // is the safe direction here, unlike a report that must
                        // not name a false owner.
                        Ok(Some(key))
                            if lookup_lock(locks, key)
                                .is_some_and(|(_, e, _)| !e.holders.is_empty()) =>
                        {
                            return Holder::Locked;
                        }
                        // A lock that is free, or not there at all.
                        Ok(_) => {}
                        Err(()) => unknown = true,
                    }
                }
            }
            _ => unknown = true,
        }
        let held = self.procs.iter().any(|p| {
            p.cwd
                .as_deref()
                .is_some_and(|c| c.starts_with(canon_target))
                || p.exe
                    .as_deref()
                    .is_some_and(|x| x.starts_with(canon_target))
                || p.fds.iter().any(|f| f.starts_with(canon_target))
                || p.cargo_target_dir
                    .as_deref()
                    .is_some_and(|d| d.starts_with(canon_target) || canon_target.starts_with(d))
        });
        if held {
            Holder::Held
        } else if unknown {
            Holder::Unknown
        } else {
            Holder::Free
        }
    }
}

/// Substring search over bytes. An empty needle matches nothing.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// The real (non-link) child directories of `dir`, or `None` when the listing
/// itself failed for a reason other than the directory not existing.
fn real_child_dirs(dir: &Path) -> Option<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.ok()?;
        if entry.file_type().ok()?.is_dir() {
            out.push(entry.path());
        }
    }
    Some(out)
}

/// Every place cargo can keep a `.cargo-lock` under a target root: the root,
/// `<root>/<profile>/` and `<root>/<triple>/<profile>/`. The same two levels
/// the census's own enumeration reads, and no deeper. Existence is not
/// required — the caller stats each — so a profile with no lock costs one
/// failed stat. `None` when a listing failed.
fn cargo_lock_files(target: &Path) -> Option<Vec<PathBuf>> {
    let mut out = vec![target.join(".cargo-lock")];
    for l1 in real_child_dirs(target)? {
        out.push(l1.join(".cargo-lock"));
        for l2 in real_child_dirs(&l1)? {
            out.push(l2.join(".cargo-lock"));
        }
    }
    Some(out)
}

/// `/proc/locks`' key for a file's inode, from its stat.
#[cfg(unix)]
fn lock_key_of(meta: &std::fs::Metadata) -> Option<super::cargo_locks::LockKey> {
    use std::os::unix::fs::MetadataExt;
    let (major, minor) = super::cargo_locks::dev_major_minor(meta.dev());
    Some(super::cargo_locks::LockKey {
        major,
        minor,
        ino: meta.ino(),
    })
}

#[cfg(not(unix))]
fn lock_key_of(_meta: &std::fs::Metadata) -> Option<super::cargo_locks::LockKey> {
    None
}

/// `Ok(Some(key))` for a lock file that exists, `Ok(None)` for one that does
/// not, `Err(())` when the stat failed or this platform has no inode key.
fn lock_key_of_path(path: &Path) -> Result<Option<super::cargo_locks::LockKey>, ()> {
    match std::fs::metadata(path) {
        Ok(meta) => lock_key_of(&meta).map(Some).ok_or(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// The SCRATCH gates for a candidate that has already cleared the boundary and
/// disk gates: is its session idle, and does anything hold it?
///
/// Read at classify time, in this order, cheapest first:
///
///  1. **Transcript.** The newest `<home>/projects/<project>/<uuid>.jsonl`
///     across ALL account homes must be older than the grace window
///     ([`SkipReason::SessionLive`]); no copy anywhere, or no resolved account
///     home, is [`SkipReason::SessionUnknown`].
///  2. **Holder.** An unlistable process table is
///     [`SkipReason::LockStateUnknown`] — the plan's name for a holder read
///     that could not be made. A held `.cargo-lock` is
///     [`SkipReason::Building`]; a process in, or aimed at, the target is
///     [`SkipReason::HeldByProcess`].
///  3. **Session.** A live process naming the session's uuid or holding its
///     directory is [`SkipReason::SessionLive`]. Only then is a holder read
///     that could not be completed reported, as `LockStateUnknown`: what was
///     observed outranks what could not be read.
///
/// The verdict is the SNAPSHOT's, and a snapshot is never a delete list: the
/// armed removal path re-reads all of it immediately before each removal
/// ([`reread_scratch_candidate`]).
fn scratch_liveness_verdict(
    target: &Path,
    session: &ScratchSession,
    opts: &ClassifyOptions,
) -> Result<(), SkipReason> {
    let env = &opts.scratch;
    let Some(homes) = env.homes() else {
        return Err(SkipReason::SessionUnknown);
    };
    match newest_transcript_age(homes, &session.project, &session.uuid) {
        TranscriptRead::Unknown => return Err(SkipReason::SessionUnknown),
        TranscriptRead::Idle(age) if age < opts.grace => return Err(SkipReason::SessionLive),
        TranscriptRead::Idle(_) => {}
    }
    let procs = env.procs();
    if !procs.listed {
        return Err(SkipReason::LockStateUnknown);
    }
    // `/proc` reports resolved paths, so compare against resolved paths. A path
    // that cannot be resolved cannot be compared, which is a missing reading.
    let (Ok(canon_target), Ok(canon_dir)) = (
        std::fs::canonicalize(target),
        std::fs::canonicalize(&session.dir),
    ) else {
        return Err(SkipReason::LockStateUnknown);
    };
    // What a process is OBSERVED to hold outranks a reading that could not be
    // made, and the specific observation outranks the general one: a build's
    // open fds are under the session directory too, so asking "does a process
    // name this session" first would report every held target as a live
    // session and bury the held lock that is the better evidence.
    let holder = procs.holds_target(&canon_target);
    match holder {
        Holder::Locked => return Err(SkipReason::Building),
        Holder::Held => return Err(SkipReason::HeldByProcess),
        Holder::Free | Holder::Unknown => {}
    }
    if procs.names_session(session, &canon_dir) {
        return Err(SkipReason::SessionLive);
    }
    match holder {
        Holder::Unknown => Err(SkipReason::LockStateUnknown),
        _ => Ok(()),
    }
}

/// Junction-safe recursive removal. Removes any link — `dir` itself or a
/// nested one — as a link only, never recursing into its target, before
/// deleting real content, then removes `dir` itself. Every link arm goes
/// through [`remove_link`], which picks the platform's and the link shape's
/// own removal call. Mirrors [`super::reclaim`] INV-W4.
///
/// ## Why `is_symlink` is tested BEFORE `is_dir`
///
/// Measured on `x86_64-pc-windows-msvc`, rustc 1.95.0 (the pinned
/// toolchain), under `symlink_metadata`: a directory junction (`mklink /J`),
/// a directory symlink (`mklink /D`) and a file symlink all report
/// `is_symlink() == true` and `is_dir() == false`. So a nested link on
/// Windows lands in the `is_symlink` arm, exactly as one on Linux does — it
/// never reaches the `is_dir` arm below, whatever its target is.
///
/// This is what the previous shape got wrong. It tested `is_dir()` first and
/// handled a link only inside that arm, so every nested Windows junction
/// fell through to a bare `let _ = std::fs::remove_file(&path)` —
/// `PermissionDenied` (os error 5) on a junction, MEASURED, with the error
/// discarded. The junction survived, the closing `remove_dir(dir)` then
/// failed as "directory not empty", and the reap of any target tree holding
/// a nested junction could never complete. Nothing was ever deleted through
/// the link, so the defect was a permanent no-op, not a data loss.
fn remove_junction_safe(dir: &Path) -> std::io::Result<()> {
    // `dir` itself must not be a reparse point — the caller guarantees it, but
    // defend anyway: unlink the link only.
    if is_junction(dir) {
        return remove_link(dir);
    }
    let entries = std::fs::read_dir(dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            // A link — a Unix symlink, or a Windows junction / directory
            // symlink / file symlink. Unlink it; never recurse in.
            remove_nested_link(&path)?;
        } else if ft.is_dir() {
            if is_junction(&path) {
                // A directory reparse point that is NOT a name surrogate, so
                // `is_symlink` does not report it and it reads as a plain
                // directory: a cloud placeholder, a dedup stub, an app-exec
                // link. `is_junction` tests the raw reparse-point attribute
                // and does see it. Unlink it rather than walking through it.
                remove_nested_link(&path)?;
            } else {
                remove_junction_safe(&path)?;
            }
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    std::fs::remove_dir(dir)
}

/// Unlink one nested link found by [`remove_junction_safe`] — link only,
/// never its target.
///
/// A link that vanished under us (a concurrent reaper pass, the agent's own
/// cleanup) is an idempotent `Ok`. Every other error is LOGGED against the
/// link's own path and then propagated, which fails the reap.
///
/// Propagating is what the old `let _ =` did not do, and it costs nothing:
/// a link left in place makes the caller's closing `remove_dir(dir)` fail
/// anyway, but as "directory not empty", which names neither the link nor
/// the reason it survived. The reaper's caller reports the error against the
/// candidate ROOT, so the `warn!` here is what names the entry.
fn remove_nested_link(path: &Path) -> std::io::Result<()> {
    match remove_link(path) {
        Ok(()) => Ok(()),
        // Already gone — a concurrent removal, not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            warn!(
                "orphan_target_reaper: cannot unlink nested link {}: {e} (the link survives; its target is untouched)",
                path.display()
            );
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Filesystem capacity — BOTH axes, read from the superblock (never a walk).
// ---------------------------------------------------------------------------

/// The filesystem reports no inode cap (`f_files == 0`): a tmpfs mounted
/// `nr_inodes=0`, or btrfs / xfs / zfs, which allocate inodes dynamically.
/// "0 used of 0" would compute as 0 % — the healthiest possible reading on
/// exactly the filesystems where nothing is measurable.
pub const INODE_LIMIT_NOT_REPORTED: &str = "inode_limit_not_reported";
/// The filesystem reports no block count (`f_blocks == 0`) — the byte-axis
/// twin of [`INODE_LIMIT_NOT_REPORTED`].
pub const BLOCK_COUNT_NOT_REPORTED: &str = "block_count_not_reported";
/// `statvfs(3)` is POSIX and this is not a POSIX platform. A stated gap, and
/// deliberately not a green reading — see the module doc's "Named gap".
pub const STATVFS_UNAVAILABLE: &str = "statvfs_unavailable_on_this_platform";
/// The `statvfs(3)` call itself failed (the path vanished, an interior NUL,
/// a permission fault). A MISSING READING, never a measurement of zero.
pub const STATVFS_FAILED: &str = "statvfs_call_failed";

/// Capacity of the mount a path lives on, on **both** axes.
///
/// Every field pair is `total` / `free` plus an `Option<f64>` percentage that is
/// `None` exactly when the axis is UNKNOWN, alongside the reason it is unknown.
/// The two axes are independent all the way through: one being unmeasurable
/// never nulls the other, because throwing away a reading that WAS taken is as
/// dishonest as fabricating one that was not.
///
/// **`bytes_free` is `f_bavail`, not `f_bfree`.** `f_bfree` counts the blocks the
/// filesystem reserves for root as free; `f_bavail` is what an unprivileged
/// process can actually allocate, and it is the reading coord's allocation
/// budget uses (`free_bytes` against `reserve_bytes`). The pressure axis used to
/// read `f_bfree`, so on merytshost (101.96e9 root-reserved bytes) it showed
/// 86.3 % used while `df` showed 92 to 93 % — two readers of one disk
/// disagreeing across the 90 % line. Percent used is therefore
/// `(total - avail) / total`: it crosses 90 % exactly where `avail` drops to 10 %
/// of the disk, which is where coord's reserve starts.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FsCapacity {
    pub bytes_total: u64,
    /// Bytes available to an unprivileged process (`f_bavail`).
    pub bytes_free: u64,
    /// `None` ⇒ UNKNOWN, with [`Self::bytes_unknown_reason`] saying why.
    pub bytes_pct_used: Option<f64>,
    pub bytes_unknown_reason: Option<&'static str>,
    pub inodes_total: u64,
    pub inodes_free: u64,
    /// `None` ⇒ UNKNOWN, with [`Self::inodes_unknown_reason`] saying why. This
    /// is the axis the 2026-09-01 `/tmp` exhaustion was invisible on.
    pub inodes_pct_used: Option<f64>,
    pub inodes_unknown_reason: Option<&'static str>,
}

/// One axis's verdict. A tri-state for the same reason [`DirtyProbe`] is one:
/// "this filesystem is not under pressure" is an OBSERVATION, and an axis that
/// reports no limit has not made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureAxis {
    Ok,
    Pressured,
    Unknown,
}

impl PressureAxis {
    pub fn as_str(self) -> &'static str {
        match self {
            PressureAxis::Ok => "OK",
            PressureAxis::Pressured => "PRESSURED",
            PressureAxis::Unknown => "UNKNOWN",
        }
    }
}

/// Percentage used from a `total` / `free` pair. `None` iff `total == 0` — the
/// UNKNOWN arm, never 0 %.
fn pct_used(total: u64, free: u64) -> Option<f64> {
    if total == 0 {
        return None;
    }
    // A filesystem can report `free > total` (reserved blocks accounting);
    // clamping keeps the percentage in range without inventing a reading.
    let free = free.min(total);
    Some(((total - free) as f64 / total as f64) * 100.0)
}

/// The pure arithmetic half of the capacity read, over the raw `statvfs(3)`
/// fields. Split out from [`fs_capacity`] so the percentages and — the part
/// that matters — the UNKNOWN arm are unit-testable on every platform, with no
/// mount and no privileges.
///
/// The byte axis takes **`f_bavail`** — see [`FsCapacity`] for why it is not
/// `f_bfree`. The inode axis keeps `f_ffree`: a filesystem has no
/// root-reserved inode count to disagree about.
pub fn capacity_from_statvfs(
    block_size: u64,
    f_blocks: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
) -> FsCapacity {
    // The percentages are computed on the COUNTS, not on the byte products, so
    // a large filesystem cannot overflow its way to a wrong ratio.
    let (bytes_pct_used, bytes_unknown_reason) = match pct_used(f_blocks, f_bavail) {
        Some(p) => (Some(p), None),
        None => (None, Some(BLOCK_COUNT_NOT_REPORTED)),
    };
    let (inodes_pct_used, inodes_unknown_reason) = match pct_used(f_files, f_ffree) {
        Some(p) => (Some(p), None),
        None => (None, Some(INODE_LIMIT_NOT_REPORTED)),
    };
    FsCapacity {
        bytes_total: f_blocks.saturating_mul(block_size),
        bytes_free: f_bavail.saturating_mul(block_size),
        bytes_pct_used,
        bytes_unknown_reason,
        inodes_total: f_files,
        inodes_free: f_ffree,
        inodes_pct_used,
        inodes_unknown_reason,
    }
}

/// A capacity that could not be read at all, on either axis, for one stated
/// reason. Both percentages are `None` — never zeroes.
fn capacity_unknown(reason: &'static str) -> FsCapacity {
    FsCapacity {
        bytes_unknown_reason: Some(reason),
        inodes_unknown_reason: Some(reason),
        ..FsCapacity::default()
    }
}

/// Read both capacity axes for the mount `path` lives on.
///
/// `statvfs(3)` reads the **superblock**, so this is O(1) regardless of how many
/// files the filesystem holds. That is why it is exempt from this module's
/// otherwise-strict no-walking discipline: the inode count that mattered on
/// 2026-09-01 was 987,519 files, and counting them by walking would have cost
/// more than the outage.
#[cfg(unix)]
pub fn fs_capacity(path: &Path) -> FsCapacity {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return capacity_unknown(STATVFS_FAILED);
    };
    let mut buf = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is a NUL-terminated path valid for the call, and `buf` is
    // a correctly-sized, suitably-aligned `statvfs` the kernel fills in full on
    // success. The return code is checked before `assume_init`.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), buf.as_mut_ptr()) };
    if rc != 0 {
        return capacity_unknown(STATVFS_FAILED);
    }
    // SAFETY: `statvfs` returned 0, so `buf` is initialised.
    let s = unsafe { buf.assume_init() };
    capacity_from_statvfs_struct(&s)
}

/// The raw `statvfs(3)` struct to a [`FsCapacity`], split out of
/// [`fs_capacity`] so the one line that chooses WHICH free-block field feeds the
/// byte axis is testable on a hand-built struct, on a filesystem that reserves
/// nothing for root.
#[cfg(unix)]
fn capacity_from_statvfs_struct(s: &libc::statvfs) -> FsCapacity {
    // `f_frsize` is the fragment size the block counts are denominated in;
    // `f_bsize` is the preferred I/O size and only a fallback for the
    // filesystems that leave `f_frsize` zero.
    let block_size = if s.f_frsize != 0 {
        s.f_frsize as u64
    } else {
        s.f_bsize as u64
    };
    capacity_from_statvfs(
        block_size,
        s.f_blocks as u64,
        // `f_bavail`, not `f_bfree`: what an unprivileged process can allocate,
        // which is the reading coord's budget uses.
        s.f_bavail as u64,
        s.f_files as u64,
        s.f_ffree as u64,
    )
}

#[cfg(not(unix))]
pub fn fs_capacity(_path: &Path) -> FsCapacity {
    capacity_unknown(STATVFS_UNAVAILABLE)
}

impl FsCapacity {
    /// The byte axis's verdict at `threshold` percent used.
    pub fn bytes_axis(&self, threshold: f64) -> PressureAxis {
        axis_verdict(self.bytes_pct_used, threshold)
    }

    /// The inode axis's verdict at `threshold` percent used.
    pub fn inodes_axis(&self, threshold: f64) -> PressureAxis {
        axis_verdict(self.inodes_pct_used, threshold)
    }

    /// **OR, never AND.** Either axis at or above the threshold is pressure.
    ///
    /// ANDing the two reproduces the 2026-09-01 outage exactly: `/tmp` at 100 %
    /// inodes and 27 % bytes, verdict "healthy", every session's Bash tool gone.
    /// An axis that could not be measured contributes NEITHER a fire nor a
    /// clear — it is simply absent from this predicate, and
    /// [`Self::render`] states it separately so a false quiet is visible.
    pub fn pressured(&self, threshold: f64) -> bool {
        matches!(self.bytes_axis(threshold), PressureAxis::Pressured)
            || matches!(self.inodes_axis(threshold), PressureAxis::Pressured)
    }

    /// True iff NEITHER axis could be measured — the one case where "no pressure
    /// observed" would be a fabricated all-clear.
    pub fn wholly_unknown(&self) -> bool {
        self.bytes_pct_used.is_none() && self.inodes_pct_used.is_none()
    }

    fn render_bytes(&self) -> String {
        match (self.bytes_pct_used, self.bytes_unknown_reason) {
            (Some(p), _) => format!(
                "{p:.1}% used ({:.2} GB total, {:.2} GB free)",
                self.bytes_total as f64 / 1_073_741_824.0,
                self.bytes_free as f64 / 1_073_741_824.0
            ),
            (None, Some(r)) => format!("UNKNOWN ({r})"),
            (None, None) => "UNKNOWN (no reason recorded)".to_string(),
        }
    }

    fn render_inodes(&self) -> String {
        match (self.inodes_pct_used, self.inodes_unknown_reason) {
            (Some(p), _) => format!(
                "{p:.1}% used ({} total, {} free)",
                self.inodes_total, self.inodes_free
            ),
            (None, Some(r)) => format!("UNKNOWN ({r})"),
            (None, None) => "UNKNOWN (no reason recorded)".to_string(),
        }
    }

    /// The operator-facing capacity line. Both axes always appear, each with its
    /// own verdict, so a divergence between them is legible at a glance — that
    /// divergence IS the 2026-09-01 finding.
    pub fn render(&self, threshold: f64) -> String {
        let verdict = if self.pressured(threshold) {
            "PRESSURED"
        } else if self.wholly_unknown() {
            "UNKNOWN — neither axis could be measured, which is NOT an all-clear"
        } else {
            "no pressure on any axis that could be measured"
        };
        format!(
            "bytes {} | inodes {} | pressure(>= {threshold:.1}% used): bytes={} inodes={} => {}",
            self.render_bytes(),
            self.render_inodes(),
            self.bytes_axis(threshold).as_str(),
            self.inodes_axis(threshold).as_str(),
            verdict,
        )
    }
}

fn axis_verdict(pct: Option<f64>, threshold: f64) -> PressureAxis {
    match pct {
        None => PressureAxis::Unknown,
        Some(p) if p >= threshold => PressureAxis::Pressured,
        Some(_) => PressureAxis::Ok,
    }
}

// ---------------------------------------------------------------------------
// Enumeration roots.
// ---------------------------------------------------------------------------

/// Which tree an enumeration root is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapRootKind {
    /// [`super::census::qontinui_root`] — the original and only root, behaviour
    /// unchanged.
    Workspace,
    /// `<tmp>/claude-<uid>` — the harness's per-session scratchpad tree, which
    /// is not under the workspace root at any depth.
    HarnessScratchpad,
}

impl ReapRootKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReapRootKind::Workspace => "workspace",
            ReapRootKind::HarnessScratchpad => "harness-scratchpad",
        }
    }

    /// How deep below THIS root the walk descends.
    ///
    /// The bound is a property of the tree, not of the module: the workspace
    /// root is flat (`<root>/target-wt-<slug>`, deepest real layout 4) while the
    /// harness scratchpad tree puts its targets at depth 5
    /// (`<project>/<session>/scratchpad/<subdir>/<target>`). A single constant
    /// is necessarily wrong for one of them, and it was — 4 hid the *typical*
    /// scratchpad target, 5,226 depth-limited dirs in one measured cycle.
    ///
    /// Derived from the kind rather than stored free-form so the two can never
    /// drift apart; [`MAX_DIRS_VISITED`] still caps the cost of either.
    pub fn max_walk_depth(self) -> u32 {
        match self {
            ReapRootKind::Workspace => WORKSPACE_MAX_WALK_DEPTH,
            ReapRootKind::HarnessScratchpad => SCRATCHPAD_MAX_WALK_DEPTH,
        }
    }
}

/// Whether a walk saw the WHOLE tree under its root, or stopped short of it.
///
/// This exists because a counter was not enough. `depth_limited_dirs` and
/// [`Enumeration::truncated`] were both already recorded, and a cycle still
/// reported `candidates=4` off a walk that never descended into 5,226
/// directories — in exactly the shape a complete census reports. A number that
/// is a FLOOR, rendered indistinguishably from a number that is a TOTAL, is the
/// same silent-empty-as-UNKNOWN failure [`SkipReason::OwnershipUnknown`] exists
/// to refuse; the fix is to put the fact in the type the consumer must destructure,
/// not in a field it may ignore.
///
/// Phase 5b's cleanup steward consumes exactly this output, and its own
/// load-bearing rule is that an incomplete read is UNKNOWN, never a low number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    /// The walk reached every directory under the root: neither the depth bound
    /// nor the visit ceiling bit. `scanned` / `candidates` are a census.
    ///
    /// Note what this does NOT claim: unreadable directories
    /// ([`Enumeration::read_errors`]) and refused reparse points
    /// ([`Enumeration::reparse_dirs_skipped`]) are separate omissions with
    /// their own counters. `Complete` means the WALK was not cut short by its
    /// own bounds, not that the filesystem answered everything.
    Complete,
    /// The walk stopped short. `scanned` / `candidates` are a LOWER BOUND, and
    /// a target root that exists below the cut is simply absent from the answer.
    Truncated {
        /// Directories not descended into because the depth bound was reached.
        depth_limited_dirs: usize,
        /// Directories actually visited before stopping.
        dirs_visited: usize,
        /// The [`MAX_DIRS_VISITED`] ceiling was hit — the other, harder cause.
        /// Kept distinct from the depth bound because they are fixed
        /// differently: one by a bound that matches the tree's shape, one by
        /// accepting that the tree is too large to enumerate whole.
        visit_ceiling_hit: bool,
    },
}

impl Completeness {
    /// What one finished walk established about its own coverage.
    pub fn from_walk(e: &Enumeration) -> Self {
        if e.depth_limited_dirs == 0 && !e.truncated {
            return Completeness::Complete;
        }
        Completeness::Truncated {
            depth_limited_dirs: e.depth_limited_dirs,
            dirs_visited: e.dirs_visited,
            visit_ceiling_hit: e.truncated,
        }
    }

    /// `true` only when the candidate set is a census. Every consumer that
    /// treats a count as a total must gate on this.
    pub fn is_complete(&self) -> bool {
        matches!(self, Completeness::Complete)
    }

    /// The counts are a floor, not a total.
    pub fn is_lower_bound(&self) -> bool {
        !self.is_complete()
    }

    /// Compact token for the per-root position in the cycle summary line.
    pub fn token(&self) -> String {
        match self {
            Completeness::Complete => "complete".to_string(),
            Completeness::Truncated {
                depth_limited_dirs,
                dirs_visited,
                visit_ceiling_hit,
            } => format!(
                "INCOMPLETE(depth_limited={depth_limited_dirs},dirs_visited={dirs_visited},\
                 visit_ceiling={visit_ceiling_hit})"
            ),
        }
    }

    /// The loud, operator-facing marker that sits directly beside the number it
    /// qualifies. `None` for a complete census — an unconditional marker would
    /// train a reader to skip it.
    pub fn marker(&self) -> Option<String> {
        match self {
            Completeness::Complete => None,
            Completeness::Truncated { .. } => Some(format!(
                " [INCOMPLETE CENSUS — scanned/candidates are a LOWER BOUND, not a total: {}]",
                self.token()
            )),
        }
    }

    /// Roll two roots' coverage into one cycle-level answer. Incompleteness
    /// WINS: a cycle is a census only if every walked root was one, and the
    /// counts add, so the aggregate can only ever over-report incompleteness.
    pub fn merged(self, other: Self) -> Self {
        match (self, other) {
            (Completeness::Complete, Completeness::Complete) => Completeness::Complete,
            (Completeness::Complete, t) | (t, Completeness::Complete) => t,
            (
                Completeness::Truncated {
                    depth_limited_dirs: a_depth,
                    dirs_visited: a_visited,
                    visit_ceiling_hit: a_ceiling,
                },
                Completeness::Truncated {
                    depth_limited_dirs: b_depth,
                    dirs_visited: b_visited,
                    visit_ceiling_hit: b_ceiling,
                },
            ) => Completeness::Truncated {
                depth_limited_dirs: a_depth + b_depth,
                dirs_visited: a_visited + b_visited,
                visit_ceiling_hit: a_ceiling || b_ceiling,
            },
        }
    }
}

/// What a cycle established about one root BEFORE walking it. The reason this
/// exists at all: a root that is not there must not render as a root that is
/// there and empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootState {
    /// A real directory. Walked.
    Present,
    /// The path does not exist. **Absent, not empty** — nothing was enumerated
    /// under it and nothing may be concluded about what it holds.
    Absent,
    /// The path exists but could not be established as a directory. A MISSING
    /// READING, kept distinct from [`Self::Absent`] the same way
    /// [`GitProbe::Unreadable`] is kept distinct from [`GitProbe::Absent`].
    Unreadable(String),
    /// The root could not be RESOLVED at all (no workspace root; no POSIX uid
    /// for the scratchpad layout). Distinct again: nothing was even looked at.
    Unresolved(&'static str),
    /// This root resolves to a tree an earlier root in the same cycle already
    /// walked. Skipped so its candidates are not counted twice — and named, so
    /// the skip is not mistaken for an empty tree.
    AlreadyWalked,
}

impl RootState {
    pub fn token(&self) -> &'static str {
        match self {
            RootState::Present => "present",
            RootState::Absent => "absent",
            RootState::Unreadable(_) => "unreadable",
            RootState::Unresolved(_) => "unresolved",
            RootState::AlreadyWalked => "already-walked",
        }
    }

    /// Whether the walk should descend from here. `Unreadable` is walkable on
    /// purpose: [`enumerate_all`] records the failed read faithfully, which is a
    /// better answer than this function inventing one.
    pub fn is_walkable(&self) -> bool {
        matches!(self, RootState::Present | RootState::Unreadable(_))
    }

    /// One operator-facing sentence, for the cycle log.
    pub fn describe(&self) -> String {
        match self {
            RootState::Present => "the root is a directory and was walked.".to_string(),
            RootState::Absent => "the tree is ABSENT. That is NOT the same as present-and-empty: \
                 nothing was enumerated under it, and nothing may be concluded about what it \
                 holds."
                .to_string(),
            RootState::Unreadable(e) => {
                format!("the root could not be read ({e}) — a MISSING READING, not an empty tree.")
            }
            RootState::Unresolved(why) => {
                format!("the root could not be resolved: {why}")
            }
            RootState::AlreadyWalked => {
                "already walked as an earlier root in this same cycle — skipped to avoid \
                 double-counting, NOT because it is empty."
                    .to_string()
            }
        }
    }
}

/// One enumeration root, resolved and probed.
#[derive(Debug, Clone)]
pub struct ReapRoot {
    pub kind: ReapRootKind,
    /// `None` iff `state` is [`RootState::Unresolved`].
    pub path: Option<PathBuf>,
    pub state: RootState,
    /// How deep below this root the walk descends — a property of THIS root's
    /// tree shape, from [`ReapRootKind::max_walk_depth`], because one global
    /// constant is necessarily wrong for one of the two trees.
    pub max_walk_depth: u32,
    /// Whether the destructive arm is open for THIS root. Arming is a property
    /// of the root's population, not of the cycle: the workspace root is armed
    /// by [`REAP_ENABLED_ENV`] and the scratch roots by
    /// [`SCRATCH_REAP_ENABLED_ENV`], independently ([`root_armed`]). A root
    /// built by [`ReapRoot::at`] is DRY-RUN until [`Self::with_armed`] says
    /// otherwise — the fail-safe default.
    pub armed: bool,
}

impl ReapRoot {
    /// Probe a resolved path. `Present` only for a real directory; a missing
    /// path is `Absent`, and a stat that failed for any other reason is
    /// `Unreadable` — the three are never collapsed. DRY-RUN until
    /// [`Self::with_armed`].
    pub fn at(kind: ReapRootKind, path: &Path) -> Self {
        let state = match std::fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => RootState::Present,
            Ok(_) => RootState::Unreadable(
                "the path exists but is not a directory (a file, a symlink, a device node)"
                    .to_string(),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RootState::Absent,
            Err(e) => RootState::Unreadable(e.to_string()),
        };
        Self {
            kind,
            path: Some(path.to_path_buf()),
            state,
            max_walk_depth: kind.max_walk_depth(),
            armed: false,
        }
    }

    pub fn unresolved(kind: ReapRootKind, why: &'static str) -> Self {
        Self {
            kind,
            path: None,
            state: RootState::Unresolved(why),
            max_walk_depth: kind.max_walk_depth(),
            armed: false,
        }
    }

    /// An UNRESOLVED root that still knows which path it could not resolve — a
    /// family discovery that failed names the directory it was reading, so the
    /// cycle line does not have to guess.
    fn unresolved_at(kind: ReapRootKind, path: &Path, why: &'static str) -> Self {
        Self {
            path: Some(path.to_path_buf()),
            ..Self::unresolved(kind, why)
        }
    }

    /// Open or close the destructive arm for this root.
    pub fn with_armed(mut self, armed: bool) -> Self {
        self.armed = armed;
        self
    }
}

/// Every input [`reap_roots_from`] resolves the enumeration roots from, as
/// values: no env read, no ambient home. Production fills it from the process
/// ([`reap_roots`]); a test fills it from a fixture, which is how the family
/// and the two arming switches are driven without touching either.
#[derive(Debug, Clone, Default)]
pub struct ReapRootInputs {
    /// The workspace root, or `None` when it did not resolve.
    pub workspace_root: Option<PathBuf>,
    /// [`SCRATCH_ROOT_ENV`], when set. Non-blank, it replaces the whole family.
    pub scratch_root_override: Option<String>,
    /// The platform temp dir.
    pub tmp: PathBuf,
    /// The `~/.qontinui` directory the on-disk scratch family hangs under.
    pub qontinui_dir: Option<PathBuf>,
    /// The invoking uid, or `None` on a platform with no POSIX uid.
    pub uid: Option<u32>,
    /// Raw [`REAP_ENABLED_ENV`].
    pub workspace_reap_env: Option<String>,
    /// Raw [`SCRATCH_REAP_ENABLED_ENV`].
    pub scratch_reap_env: Option<String>,
}

impl ReapRootInputs {
    /// Read every input from the running process.
    pub fn ambient() -> Self {
        Self {
            workspace_root: qontinui_root(),
            scratch_root_override: std::env::var(SCRATCH_ROOT_ENV).ok(),
            tmp: std::env::temp_dir(),
            qontinui_dir: qontinui_runner_lib::ambient::qontinui_dir(),
            #[cfg(unix)]
            uid: Some(invoking_uid()),
            #[cfg(not(unix))]
            uid: None,
            workspace_reap_env: std::env::var(REAP_ENABLED_ENV).ok(),
            scratch_reap_env: std::env::var(SCRATCH_REAP_ENABLED_ENV).ok(),
        }
    }
}

/// The scratch roots for `inputs`: `<tmp>/claude-<uid>` first, then every
/// member of the on-disk family ([`scratch_family_under`]), each its own
/// [`ReapRoot`] with its own visit budget and its own state on the cycle line.
///
/// **A family that could not be established says so as a root of its own.** An
/// unresolved `~/.qontinui`, an unreadable or truncated discovery, and a scratch
/// tree that holds no `claude-<uid>` at all each become a non-walkable root
/// naming the directory, so a cycle can never read as having covered a family
/// it did not find — the failure this resolution exists to close.
fn scratch_roots_from(inputs: &ReapRootInputs) -> Vec<ReapRoot> {
    const KIND: ReapRootKind = ReapRootKind::HarnessScratchpad;
    if let Some(explicit) = inputs.scratch_root_override.as_deref().map(str::trim) {
        if !explicit.is_empty() {
            return vec![ReapRoot::at(KIND, Path::new(explicit))];
        }
    }
    let Some(uid) = inputs.uid else {
        return vec![ReapRoot::unresolved(
            KIND,
            "this platform has no POSIX uid, so the `claude-<uid>` harness scratchpad layout \
             does not exist here — a named gap, never an empty tree",
        )];
    };
    let mut roots = vec![ReapRoot::at(
        KIND,
        &inputs.tmp.join(format!("claude-{uid}")),
    )];
    let Some(qdir) = inputs.qontinui_dir.as_deref() else {
        roots.push(ReapRoot::unresolved(
            KIND,
            "no `~/.qontinui` directory resolved, so the on-disk scratch family under it is \
             unknown — a named gap, never an empty tree",
        ));
        return roots;
    };
    let scratch = qdir.join("scratch");
    let family = scratch_family_under(qdir, uid);
    if family.incomplete() {
        warn!(
            "orphan_target_reaper: scratch family discovery under {} was INCOMPLETE \
             (unreadable={}, truncated={}) — the members below may be missing a tree",
            scratch.display(),
            family.unreadable,
            family.truncated,
        );
        roots.push(ReapRoot::unresolved_at(
            KIND,
            &scratch,
            "scratch family discovery was incomplete (an unreadable directory or the entry \
             ceiling) — the family roots listed may be missing a tree",
        ));
    }
    if family.links_skipped > 0 {
        warn!(
            "orphan_target_reaper: {} symlink(s) to directories under {} were refused as family \
             roots or account directories — a link is never followed",
            family.links_skipped,
            scratch.display(),
        );
    }
    if family.members.is_empty() && !family.incomplete() {
        roots.push(match std::fs::symlink_metadata(&scratch) {
            Ok(_) => ReapRoot::unresolved_at(
                KIND,
                &scratch,
                "the scratch directory exists but holds no `claude-<uid>` tree under any \
                 account — no on-disk scratch family to walk",
            ),
            // Absent, not empty: nothing lives under a directory that is not there.
            Err(_) => ReapRoot::at(KIND, &scratch),
        });
    }
    roots.extend(family.members.iter().map(|m| ReapRoot::at(KIND, m)));
    roots
}

/// Resolve every enumeration root from `inputs`, in order: the workspace root
/// (unchanged), then the scratch family ([`scratch_roots_from`]). Every root is
/// returned WITH its state and its arming, so a caller can never silently lose
/// one and a population can never inherit another's switch ([`root_armed`]).
pub fn reap_roots_from(inputs: &ReapRootInputs) -> Vec<ReapRoot> {
    let arm = |root: ReapRoot| {
        let armed = root_armed(
            root.kind,
            inputs.workspace_reap_env.as_deref(),
            inputs.scratch_reap_env.as_deref(),
        );
        root.with_armed(armed)
    };
    let mut roots = vec![arm(match inputs.workspace_root.as_deref() {
        Some(r) => ReapRoot::at(ReapRootKind::Workspace, r),
        None => ReapRoot::unresolved(
            ReapRootKind::Workspace,
            "`workspace_paths::workspace_root()` returned None, so the workspace root is \
             unknown on this machine",
        ),
    })];
    roots.extend(scratch_roots_from(inputs).into_iter().map(arm));
    roots
}

/// The ambient enumeration roots ([`reap_roots_from`] over the running
/// process). Every root is returned WITH its state and its arming.
pub fn reap_roots() -> Vec<ReapRoot> {
    reap_roots_from(&ReapRootInputs::ambient())
}

/// One reaper cycle against the ambient config ([`reap_roots`] + env arming +
/// env grace). Thin wrapper over [`run_cycle_over_roots`] so the loop stays
/// trivial.
///
/// It no longer short-circuits to an empty summary when a root fails to resolve:
/// that returned `scanned=0` for a cycle that had not looked at anything, which
/// is indistinguishable from a cycle that looked and found nothing. The
/// unresolved root is reported as such in [`ReapSummary::roots`] instead.
pub fn tick_once() -> ReapSummary {
    run_cycle_over_roots(&reap_roots(), grace())
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
        SkipReason::ForeignOwner => summary.skipped_foreign_owner += 1,
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
        SkipReason::NotSessionScoped => summary.skipped_non_session += 1,
        SkipReason::SessionLive => summary.skipped_session_live += 1,
        SkipReason::HeldByProcess => summary.skipped_held += 1,
        // Counted above; listed only for exhaustiveness, so a NEW reason cannot
        // be added without deciding which bucket it belongs in.
        SkipReason::OwnershipUnknown
        | SkipReason::DirtyUnknown
        | SkipReason::ActivityUnknown
        | SkipReason::LockStateUnknown
        | SkipReason::SessionUnknown => {}
    }
}

/// ` session=<uuid>` for a scratch candidate, empty for a workspace one — so
/// the would-reap and reaped lines name the session a reviewer must check the
/// liveness reads against.
fn session_tag(c: &Candidate) -> String {
    c.family_root
        .as_deref()
        .and_then(|family| scratch_session_of(family, &c.path))
        .map(|s| format!(" session={}", s.uuid))
        .unwrap_or_default()
}

/// One reaper cycle over a SINGLE root with explicit `armed`/`grace` — the
/// original testable seam, kept verbatim in behaviour. Delegates to
/// [`run_cycle_over_roots`] so there is exactly one cycle implementation.
pub fn run_cycle(root: &Path, armed: bool, grace: Duration) -> ReapSummary {
    run_cycle_over_roots(
        &[ReapRoot::at(ReapRootKind::Workspace, root).with_armed(armed)],
        grace,
    )
}

/// One reaper cycle over a LIST of roots with an explicit `grace`. Arming is
/// each root's own ([`ReapRoot::armed`]), not the cycle's.
/// Enumerates target roots under each walkable root, classifies each, and —
/// only when THAT root is armed — removes the reapable ones junction-safely.
/// A root that is not armed merely logs "would reap" and removes NOTHING.
///
/// Every root is accounted for in [`ReapSummary::roots`] whether or not it was
/// walked, because "the tree is not there" and "the tree is there and holds
/// nothing" are different answers and only one of them was observed.
///
/// The per-root capacity read ([`fs_capacity`]) happens here rather than inside
/// the walk: it is a superblock read, so it is O(1) and cannot be confused with
/// the enumeration's cost.
pub fn run_cycle_over_roots(roots: &[ReapRoot], grace: Duration) -> ReapSummary {
    run_cycle_over_roots_with(roots, &ClassifyOptions::new(grace))
}

/// [`run_cycle_over_roots`] with the classifier's options injected — the seam
/// that points the scratch gates at a fixture process table and fixture
/// account homes, and the dirty probe at a fixture.
pub fn run_cycle_over_roots_with(roots: &[ReapRoot], opts: &ClassifyOptions) -> ReapSummary {
    let threshold = pressure_pct();
    let mut summary = ReapSummary::default();
    // Canonical paths already walked this cycle, so two roots resolving to the
    // same tree cannot double-count its candidates (or, once armed, race each
    // other over the same paths).
    let mut walked: Vec<PathBuf> = Vec::new();

    for r in roots {
        let mut report = RootReport {
            kind: r.kind,
            path: r.path.clone(),
            state: r.state.clone(),
            scanned: 0,
            candidates: 0,
            capacity: None,
            max_walk_depth: r.max_walk_depth,
            completeness: None,
            walked_bytes: 0,
            dirs_visited: 0,
            armed: r.armed,
        };
        // This root's switch, not the cycle's: the workspace and the scratch
        // populations are armed independently ([`root_armed`]).
        let armed = r.armed;
        let rendered_path = r
            .path
            .as_ref()
            .map_or_else(|| "<unresolved>".to_string(), |p| p.display().to_string());

        let Some(path) = r.path.as_deref().filter(|_| r.state.is_walkable()) else {
            info!(
                "orphan_target_reaper: root[{}] {rendered_path} state={} — {}",
                r.kind.as_str(),
                report.state.token(),
                report.state.describe(),
            );
            summary.roots.push(report);
            continue;
        };

        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if walked.contains(&key) {
            report.state = RootState::AlreadyWalked;
            info!(
                "orphan_target_reaper: root[{}] {rendered_path} state={} — {}",
                r.kind.as_str(),
                report.state.token(),
                report.state.describe(),
            );
            summary.roots.push(report);
            continue;
        }
        walked.push(key);

        // BOTH capacity axes for the mount this root lives on, before the walk.
        // Logged per root so an inode/byte divergence is visible on the very
        // line a shadow window is reviewed from — the 2026-09-01 `/tmp`
        // exhaustion was invisible to a byte-only reading.
        let capacity = fs_capacity(path);
        info!(
            "orphan_target_reaper: root[{}] {rendered_path} state={} armed={armed} | {}",
            r.kind.as_str(),
            report.state.token(),
            capacity.render(threshold),
        );
        report.capacity = Some(capacity);

        // THIS root's depth bound, not a module-wide one — the workspace tree is
        // flat and the scratchpad tree is not. A scratch root is walked as a
        // FAMILY root, so every candidate carries the root it came from and
        // goes through the session-liveness and holder gates.
        let enumeration = match r.kind {
            ReapRootKind::Workspace => enumerate_all_within(path, r.max_walk_depth),
            ReapRootKind::HarnessScratchpad => enumerate_family_within(path, r.max_walk_depth),
        };
        report.scanned = enumeration.candidates.len();
        report.walked_bytes = enumeration.walked_bytes;
        report.dirs_visited = enumeration.dirs_visited;
        // Recorded per root, BEFORE the candidate loop, so a truncated walk
        // cannot reach a consumer as a bare count. `scanned`/`candidates` on
        // this report are a floor whenever this is `Truncated`.
        report.completeness = Some(Completeness::from_walk(&enumeration));
        summary.scanned += enumeration.candidates.len();
        summary.truncated |= enumeration.truncated;
        // The TOTAL, not `read_errors.len()` — the detail list is capped at
        // `MAX_RECORDED_READ_ERRORS` and reading its length here would make the
        // cycle log under-report a locked subtree as exactly 100 failures.
        // Through `read_errors_seen` rather than the raw counter, so the two
        // fields going out of step can only ever OVER-report incompleteness.
        summary.read_errors += enumeration.read_errors_seen();
        summary.entry_errors += enumeration.entry_errors;
        summary.depth_limited_dirs += enumeration.depth_limited_dirs;
        summary.reparse_dirs_skipped += enumeration.reparse_dirs_skipped;

        for c in enumeration.candidates {
            match classify_candidate(&c, opts) {
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
                    report.candidates += 1;
                    summary.candidate_bytes = summary.candidate_bytes.saturating_add(bytes);
                    match measured {
                        None => summary.candidates_with_unknown_bytes += 1,
                        Some(m) if m.unreadable_dirs > 0 => {
                            summary.candidates_with_partial_bytes += 1;
                        }
                        Some(_) => {}
                    }
                    let size = render_size(measured);
                    let session = session_tag(&c);
                    if !armed {
                        info!(
                            "orphan_target_reaper: [dry-run] would reap {} [root={} class={}{session}] \
                             ({size})",
                            c.path.display(),
                            r.kind.as_str(),
                            c.class.as_str(),
                        );
                        continue;
                    }
                    // A candidate list is a snapshot and never a delete list: a
                    // session can resume, and a build can start, between the
                    // classification above and this removal (and sizing a large
                    // target takes real time). A scratch candidate is therefore
                    // RE-READ — disk gates, transcripts, process table, locks —
                    // immediately before it is removed, and a disagreement
                    // between the two reads removes nothing.
                    if let Err(reason) = reread_scratch_candidate(&c, opts) {
                        summary.reread_refused += 1;
                        warn!(
                            "orphan_target_reaper: re-read refused {} [root={}{session}]: {} — \
                             cleared at classification, NOT removed",
                            c.path.display(),
                            r.kind.as_str(),
                            reason.token(),
                        );
                        continue;
                    }
                    match remove_junction_safe(&c.path) {
                        Ok(()) => {
                            summary.reaped += 1;
                            summary.reaped_bytes = summary.reaped_bytes.saturating_add(bytes);
                            info!(
                                "orphan_target_reaper: reaped {} [root={} class={}{session}] ({size})",
                                c.path.display(),
                                r.kind.as_str(),
                                c.class.as_str(),
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
        summary.roots.push(report);
    }
    summary.scratch_opaque_procs = opts.scratch.opaque_procs_seen();

    info!("{}", render_cycle_line(&summary));
    if summary.population_unknown() {
        warn!(
            "orphan_target_reaper: cycle enumerated NO target roots under any configured \
             root ({}) and its walk reported a shortfall ({}). The zeros above are an \
             UNKNOWN population, not a measured one — this cycle is not evidence that \
             there is nothing to reap, and must not be read as a clean preview toward \
             arming.",
            summary
                .roots
                .iter()
                .map(RootReport::render)
                .collect::<Vec<_>>()
                .join("; "),
            // `shortfall_causes` reports WALK counters and is empty by
            // construction when no walk ran, so the unwalked case needs its own
            // sentence or the warning would name no cause at all.
            if summary.no_root_walked() {
                "no configured root could be walked".to_string()
            } else {
                summary.shortfall_causes()
            },
        );
    }
    summary
}

/// The operator-facing cycle summary line — the one surface a shadow window is
/// reviewed from, and (Phase 5b) the one the cleanup steward consumes.
///
/// Split out of [`run_cycle_over_roots`] so the honesty markers on it are
/// testable as a STRING rather than only as struct fields: what has to hold is
/// that a reader of this line cannot mistake a truncated walk for a census, and
/// that is a property of the rendered text.
fn render_cycle_line(summary: &ReapSummary) -> String {
    let completeness = summary.completeness();
    // The cycle-level `armed=` is true iff ANY walked root is armed. Arming is a
    // property of each root's population ([`root_armed`]), so the per-root token
    // inside `roots=[…]` is the answer to "was THIS root armed"; this one only
    // says whether the cycle could have removed anything at all.
    let armed = summary.any_root_armed();
    // The cycle line is the surface a shadow-window review reads, and an
    // enumeration that saw NOTHING renders on it bit-for-bit like a clean
    // machine: `scanned=0 candidates=0 candidate_gb=0.00 reaped=0`. Its one
    // honesty annotation — the `(LOWER BOUND: …)` suffix — keys on
    // `candidates_with_unknown_bytes` / `candidates_with_partial_bytes`, both
    // necessarily `0` when there are no candidates at all, so it is silent in
    // exactly the state that needs it. `population=` carries the statement in a
    // field of its own and the `warn!` below carries it in prose, because a
    // claim that lives only in prose is not made to a machine reader at all.
    // This is `disk_survey`'s rule — an UNKNOWN population must not publish
    // the shape of a measurement — applied to the sibling surface of the SAME
    // walk: both read `enumerate_all`, and only one of them refused the zero.
    // THREE values, not two. `population_unknown()` requires an empty
    // enumeration, so a binary token would print the affirmative word
    // "measured" for every cycle that found a root — including the ordinary
    // one, since with the workspace root's bound at 4 it bites on essentially any
    // deep tree, and the steady-state line would read `population=measured`
    // beside `depth_limited=118`. That is this commit's own defect pointed at
    // its own new field. `partial` names the middle state — roots WERE found,
    // and the walk still did not reach everything — and leaves `measured` to
    // mean what it says. The counters in `walk(...)` below say which shortfall,
    // so the middle arm needs no prose; only the UNKNOWN arm gets the `warn!`.
    let population = if summary.population_unknown() {
        "UNKNOWN"
    } else if summary.walk_fell_short() {
        "partial"
    } else {
        "measured"
    };
    format!(
        "orphan_target_reaper: cycle armed={armed} population={population} roots=[{}] scanned={} truncated={} \
         candidates={}{} candidate_gb={:.2}{} reaped={} reaped_gb={:.2} \
         skipped(live={},unrecognized={},grace={},reparse={},kept={},report_only={},\
         other_owner={},foreign_owner={},wt_dirty={},non_session={},session_live={},held={},\
         unknown={}) reread_refused={} scratch_opaque_procs={} \
         walk(read_errors={},entry_errors={},depth_limited={},reparse_skipped={}) errors={}",
        // Every configured root, WITH its state — so a cycle that walked one of
        // two roots can never read as a cycle that walked the machine.
        summary
            .roots
            .iter()
            .map(RootReport::render)
            .collect::<Vec<_>>()
            .join("; "),
        summary.scanned,
        summary.truncated,
        summary.candidates,
        // Sits directly beside the number it qualifies, because a consumer that
        // reads `candidates` must not be able to miss that the walk stopped
        // short. Empty for a genuine census — an unconditional marker trains a
        // reader to skip it.
        completeness.marker().unwrap_or_default(),
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
        summary.skipped_foreign_owner,
        summary.skipped_worktree_dirty,
        summary.skipped_non_session,
        summary.skipped_session_live,
        summary.skipped_held,
        summary.skipped_unknown,
        summary.reread_refused,
        summary.scratch_opaque_procs,
        summary.read_errors,
        summary.entry_errors,
        summary.depth_limited_dirs,
        summary.reparse_dirs_skipped,
        summary.errors,
    )
}

/// What one enumeration root contributed to a cycle — including the case where
/// it contributed nothing BECAUSE IT WAS NOT THERE.
#[derive(Debug, Clone)]
pub struct RootReport {
    pub kind: ReapRootKind,
    /// `None` iff the root could not be resolved at all.
    pub path: Option<PathBuf>,
    pub state: RootState,
    /// Target roots enumerated. Meaningful ONLY when `state.is_walkable()`; for
    /// every other state it stays 0 and the STATE is the answer, so an absent
    /// tree can never be read as "walked, found nothing".
    pub scanned: usize,
    /// Of those, the ones that cleared every gate.
    pub candidates: usize,
    /// Both capacity axes for the mount this root lives on. `None` iff the root
    /// was not walked — nothing to stat.
    pub capacity: Option<FsCapacity>,
    /// The depth bound this root was walked with
    /// ([`ReapRootKind::max_walk_depth`]), carried into the report so the
    /// operator-facing line says which bound produced the numbers beside it.
    pub max_walk_depth: u32,
    /// Whether `scanned` / `candidates` are a census or a FLOOR. `None` iff the
    /// root was not walked — the same `None`-means-not-walked reading as
    /// `capacity`, and NOT [`Completeness::Complete`], which would claim a
    /// coverage nobody measured. For an unwalked root the answer is `state`.
    pub completeness: Option<Completeness>,
    /// Bytes of the regular files directly inside the directories this walk
    /// read ([`Enumeration::walked_bytes`]) — a FLOOR under what the root
    /// holds, printed beside `scanned` so a root that found no target roots
    /// states how much it read instead of looking empty. 0 for a root that was
    /// not walked.
    pub walked_bytes: u64,
    /// Directories the walk visited. With `walked_bytes`, what separates "a
    /// tree with no cargo target in it" from "an empty tree".
    pub dirs_visited: usize,
    /// Whether the destructive arm was open for this root this cycle.
    pub armed: bool,
}

impl RootReport {
    /// Compact per-root token for the cycle summary line.
    ///
    /// A harness-scratchpad root also names its PATH, last inside the
    /// parentheses: the family has one root per account tree, so a token that
    /// did not say which tree it read would be a census of nothing in
    /// particular. The existing fields keep their order and spelling and the
    /// new ones follow them.
    fn render(&self) -> String {
        let path = match (self.kind, &self.path) {
            (ReapRootKind::HarnessScratchpad, Some(p)) => Some(p.display().to_string()),
            _ => None,
        };
        match &self.state {
            st if st.is_walkable() => format!(
                "{}:{}(scanned={},candidates={},depth<={},census={},walked_gb={:.2},dirs={},\
                 armed={}{})",
                self.kind.as_str(),
                st.token(),
                self.scanned,
                self.candidates,
                self.max_walk_depth,
                self.completeness
                    .as_ref()
                    // A walkable root that reached the walk always has one; the
                    // fallback names the gap rather than inventing "complete".
                    .map_or_else(|| "UNKNOWN".to_string(), Completeness::token),
                self.walked_bytes as f64 / 1_073_741_824.0,
                self.dirs_visited,
                self.armed,
                path.map_or_else(String::new, |p| format!(",path={p}")),
            ),
            // NOT `scanned=0`: an unwalked root reports its state alone, so a
            // reader cannot mistake "not there" for "there and empty".
            st => format!(
                "{}:{}{}",
                self.kind.as_str(),
                st.token(),
                path.map_or_else(String::new, |p| format!("(path={p})")),
            ),
        }
    }
}

/// Per-cycle telemetry.
#[derive(Debug, Default, Clone)]
pub struct ReapSummary {
    /// Every configured enumeration root, with the state that decided whether
    /// it was walked. A root that was not walked is present here with its
    /// reason — never merely missing, and never as `scanned = 0`.
    pub roots: Vec<RootReport>,
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
    /// Owned by another ACCOUNT (a uid other than this process's). Kept out of
    /// `skipped_other_owner` because "another engine of ours owns this" and
    /// "this is not ours at all" are different findings, and a shadow window
    /// reviewing a shared-temp root needs to see the second one on its own.
    pub skipped_foreign_owner: usize,
    /// Inside a worktree with uncommitted work (G-dirty).
    pub skipped_worktree_dirty: usize,
    /// Scratch-root targets with no `<project>/<session-uuid>/` owner
    /// ([`SkipReason::NotSessionScoped`]).
    pub skipped_non_session: usize,
    /// Scratch-root targets whose session is not idle ([`SkipReason::SessionLive`]).
    pub skipped_session_live: usize,
    /// Scratch-root targets a live process holds ([`SkipReason::HeldByProcess`]).
    pub skipped_held: usize,
    /// Armed scratch candidates the classifier cleared that the re-read taken
    /// immediately before removal then REFUSED. The candidate list is a
    /// snapshot, and a session may resume between the snapshot and the delete;
    /// each of these is a removal that did not happen because the second read
    /// disagreed with the first.
    pub reread_refused: usize,
    /// Own-uid processes whose `/proc` entries this uid was denied (the kernel
    /// has declared them un-inspectable by us — `systemd --user`, `ssh-agent`).
    /// They are tolerated, because refusing on any of them would leave the
    /// scratch gates inert on every systemd box, but they are COUNTED here so
    /// the tolerance is visible on the line a shadow window is read from.
    pub scratch_opaque_procs: usize,
    /// Refused because a READING was missing (ownership, dirtiness, or idle
    /// age), not because anything was observed.
    pub skipped_unknown: usize,
    /// Directories whose read failed during the walk (`read_dir` open, or the
    /// `.git` ownership probe). The UNCAPPED count — read through
    /// [`Enumeration::read_errors_seen`], never from the recorded sample, whose
    /// length stops at [`MAX_RECORDED_READ_ERRORS`].
    pub read_errors: usize,
    /// Directory entries that errored mid-iteration.
    pub entry_errors: usize,
    /// Directories not descended into because a root's depth bound
    /// ([`ReapRootKind::max_walk_depth`]) was reached.
    pub depth_limited_dirs: usize,
    /// Reparse points the walk refused to follow.
    pub reparse_dirs_skipped: usize,
    pub errors: usize,
}

impl ReapSummary {
    /// **The cycle enumerated NOTHING, and the walk that produced that
    /// emptiness fell short of the tree** — so "no target roots" is an UNKNOWN
    /// population here, not a measured zero.
    ///
    /// This is [`super::disk_survey`]'s `population_unknown` predicate, on the
    /// sibling surface of the *same* walk: [`run_cycle`] and
    /// `super::disk_survey::build_snapshot` both consume [`enumerate_all`],
    /// and the survey's `items` is one rendered row per enumerated candidate,
    /// so [`Self::scanned`] `== 0` is exactly the survey's `items.is_empty()`.
    /// The survey learned to refuse the zero over two rounds of honesty fixes;
    /// this surface was left certifying one, and it is the surface a
    /// shadow-window review reads.
    ///
    /// The depth bound is one of [`Self::walk_fell_short`]'s five conjuncts for
    /// the same reason it is there — and it only reaches this predicate under
    /// the empty-enumeration guard. With [`WORKSPACE_MAX_WALK_DEPTH`] at 4 the bound bites on
    /// essentially any deep tree, so a cycle that FOUND roots must never be
    /// flagged by it or the flag would be permanently true and signal nothing.
    /// It costs nothing here because this predicate already requires an
    /// enumeration that found no roots at all.
    ///
    /// The reachable case is the ordinary misconfiguration: a `root` one level
    /// too high (or newly unreadable — a detached drive, a permissions change)
    /// finds no target root at or above the bound while *nothing fails*, which
    /// renders as a clean cycle.
    pub fn population_unknown(&self) -> bool {
        self.scanned == 0 && (self.walk_fell_short() || self.no_root_walked())
    }

    /// No configured root ever REACHED the walk — every one was absent,
    /// unresolvable, unreadable before listing, or a duplicate of a root
    /// already walked this cycle.
    ///
    /// This disjunct exists because the predicate above was written for a
    /// SINGLE-root cycle, where a missing root meant `read_dir` failed and
    /// `read_errors` carried the shortfall. Over a LIST of roots the state probe
    /// short-circuits before the walk, so `read_errors` stays 0 and every walk
    /// counter reads clean — and the cycle line would publish the affirmative
    /// `population=measured` over a cycle that looked at nothing at all. That is
    /// the exact claim [`Self::population_unknown`] exists to refuse, so the
    /// multi-root arrangement had to be taught the same refusal rather than
    /// inheriting a predicate whose reach had quietly narrowed to "the root
    /// existed but could not be listed".
    ///
    /// The discriminator is [`RootReport::completeness`], which is `None`
    /// exactly for an unwalked root — its own doc says so — rather than a second
    /// state enum that could drift from it.
    pub fn no_root_walked(&self) -> bool {
        !self.roots.iter().any(|r| r.completeness.is_some())
    }

    /// Whether any root that was actually WALKED had its destructive arm open.
    /// A root that was not walked removed nothing whatever its switch said, so
    /// it does not make the cycle armed.
    pub fn any_root_armed(&self) -> bool {
        self.roots
            .iter()
            .any(|r| r.armed && r.completeness.is_some())
    }

    /// The walk did not reach everything, whatever it found on the way.
    ///
    /// Split out from [`Self::population_unknown`] because the two questions are
    /// different and the cycle line needs both: this one is true on essentially
    /// every real cycle (the depth bound sees to that), while the other requires
    /// an empty enumeration and is the one that must never be mistaken for a
    /// clean preview. Reading only this would pin the line permanently short;
    /// reading only the other would print an affirmative "measured" over a walk
    /// that skipped a hundred directories.
    ///
    /// The `read_errors` conjunct is the loosest of the five: `record_read_error`
    /// is called both for a `read_dir` that failed — a directory genuinely
    /// unseen — and for a `.git` ownership probe that could not be read, after
    /// which the walk still descends into that directory. So a non-zero count
    /// means "a read failed", not always "a subtree is missing", which is why
    /// the cycle warning says the walk reported a SHORTFALL rather than that it
    /// did not see the whole tree. Failing closed over the looser reading is
    /// correct here; asserting the stronger cause would not be.
    pub fn walk_fell_short(&self) -> bool {
        self.truncated
            || self.read_errors > 0
            || self.entry_errors > 0
            || self.reparse_dirs_skipped > 0
            || self.depth_limited_dirs > 0
    }

    /// Every way this walk fell short of the tree, named — never just the first
    /// one that matched. `disk_survey`'s `census_note` joins its gap fragments
    /// the same way and for the same reason: omitting a cause the counters carry
    /// is the same defect as asserting one they do not, pointed the other way.
    ///
    /// Empty exactly when [`Self::walk_fell_short`] is false — which is also
    /// what makes it a usable assertion for "this cycle saw the whole tree".
    pub fn shortfall_causes(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.truncated {
            parts.push("it hit its visit ceiling".to_string());
        }
        if self.read_errors > 0 {
            parts.push(format!(
                "{} director{} could not be read",
                self.read_errors,
                if self.read_errors == 1 { "y" } else { "ies" }
            ));
        }
        if self.entry_errors > 0 {
            parts.push(format!(
                "{} director{} errored mid-listing",
                self.entry_errors,
                if self.entry_errors == 1 {
                    "y entry"
                } else {
                    "y entries"
                }
            ));
        }
        if self.reparse_dirs_skipped > 0 {
            parts.push(format!(
                "{} junction{} not followed",
                self.reparse_dirs_skipped,
                if self.reparse_dirs_skipped == 1 {
                    " was"
                } else {
                    "s were"
                }
            ));
        }
        if self.depth_limited_dirs > 0 {
            parts.push(format!(
                "{} director{} not descended into at the walk's depth bound",
                self.depth_limited_dirs,
                if self.depth_limited_dirs == 1 {
                    "y was"
                } else {
                    "ies were"
                }
            ));
        }
        parts.join("; ")
    }

    /// Whether this cycle's `scanned` / `candidates` are a CENSUS or a FLOOR,
    /// rolled up over every root that was actually walked.
    ///
    /// Computed rather than stored, so it cannot go stale against the
    /// [`RootReport`]s it is derived from. Incompleteness wins: one truncated
    /// root makes the whole cycle a lower bound, because the cycle-level
    /// numbers are sums over the roots.
    ///
    /// Roots that were NOT walked contribute nothing here — an `Absent` or
    /// `Unresolved` root is not "completely walked", and this method is not
    /// where that is reported. [`RootState`] on each [`RootReport`] is, and a
    /// cycle where no root was walkable says so there rather than by claiming a
    /// census of nothing.
    pub fn completeness(&self) -> Completeness {
        self.roots
            .iter()
            .filter_map(|r| r.completeness.clone())
            .fold(Completeness::Complete, Completeness::merged)
    }
}

/// Spawn the periodic reaper on the ambient tokio runtime. Interval from
/// `QONTINUI_ORPHAN_TARGET_INTERVAL_SECS` (default 900 s, floored 60 s).
/// `MissedTickBehavior::Skip`; failures never panic. The blocking disk work
/// runs on `spawn_blocking` so it never stalls the async runtime.
///
/// ## Instance-gated, exactly like [`super::disk_survey::spawn_disk_surveyor`]
///
/// [`run_cycle`] walks the WHOLE MACHINE: [`enumerate_all`] descends to
/// each root's depth bound across up to [`MAX_DIRS_VISITED`] directories, spawns one
/// `git status` per repo root it meets, and sizes every candidate it finds. That
/// is the same walk the reclaim preview publishes hourly — and the preview's
/// copy is instance-gated precisely because it is machine-wide, so every
/// secondary and every supervisor-spawned temp runner would otherwise start its
/// own full-machine sizing walk over identical data, on the very disk-pressured
/// box this feature exists for.
///
/// The gate stopped at the preview when it was added. This loop runs the same
/// walk **four times as often** (900 s against the survey's 3600 s), so leaving
/// it ungated left the louder of the two identical walks running on every
/// instance. It is also the DESTRUCTIVE engine: once the arming flag is flipped
/// (plan `2026-08-07-product-disk-monitoring-and-cleanup`, Phase 4), N ungated
/// instances would race each other over the same paths, each one's
/// `remove_junction_safe` racing the others' `classify_candidate` readings.
///
/// The predicate is the bare [`crate::instance::owns_shared_root_state`] the
/// survey uses, not `fleet::machine_state_publish_allowed`: that wrapper guards
/// what a machine PUBLISHES to coord, and this loop publishes nothing and holds
/// no machine identity. What is being arbitrated here is disk work, so the
/// disk-work predicate is the right one.
///
/// ## And it yields to the preview's walk on the instance that keeps both
///
/// The instance gate deduplicates the two identical walks ACROSS processes; it
/// does nothing about them inside one. On the owning instance both loops
/// survive, running the same `enumerate_all` and the same sizing pass over the
/// same disk, with cadences that collide every hour. Each tick therefore skips
/// while [`super::disk_survey::build_active`] holds — one-directional, because
/// the preview must never yield (INV-D1: it answers mid-build, mid-emergency
/// and coord-unreachable) and this loop is the one that ticks four times as
/// often and is dry-run by default.
pub fn spawn_orphan_reaper() {
    if !crate::instance::owns_shared_root_state() {
        info!(
            "orphan_target_reaper: not the shared-root-owning instance — skipping the periodic \
             reaper (the walk is machine-wide and the owning instance covers this disk). \
             `GET /disk/reclaimable` still answers here and still refreshes on demand."
        );
        return;
    }
    let secs: u64 = std::env::var(INTERVAL_SECS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(60);
    if let Some(warning) =
        scratch_switch_warning(std::env::var(SCRATCH_REAP_ENABLED_ENV).ok().as_deref())
    {
        warn!("orphan_target_reaper: {warning}");
    }
    info!(
        "orphan_target_reaper: starting periodic reaper, interval={}s, armed(workspace)={}, \
         armed(scratch)={} — the two populations are armed independently",
        secs,
        reap_armed(),
        scratch_reap_armed()
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            // Yield the tick while the reclaim preview is mid-walk. Gating the
            // reaper per INSTANCE left the two identical walks still racing
            // each other ON the owning one: both run `enumerate_all` and size
            // every candidate over the whole machine, and their cadences line
            // up every hour, so the box this feature exists for was paying for
            // two concurrent full-machine sizing walks over the same disk.
            //
            // ONE-DIRECTIONAL, and deliberately so. The preview never yields to
            // the reaper (INV-D1: it must answer mid-build, mid-emergency and
            // coord-unreachable), and it is the reaper that can afford to wait
            // — it ticks four times as often, so a skipped cycle costs 900 s of
            // latency on hygiene that is dry-run by default. Skipping, never
            // queueing, matches the `MissedTickBehavior::Skip` already on this
            // interval.
            //
            // Correctness, not only cost: once the arming flag is flipped, a
            // reap running underneath a survey walk leaves the published
            // snapshot advertising reclaimable bytes for paths already removed
            // — drift between the preview and the verb, which this module's
            // whole design (one classifier, both engines) exists to prevent.
            //
            // Advisory rather than a lock: the survey can still start just
            // after this reads false. That is the correct strength for a cost
            // and consistency guard whose failure mode is one redundant walk.
            if super::disk_survey::build_active() {
                info!(
                    "orphan_target_reaper: a reclaim-preview walk is in flight — skipping this \
                     cycle rather than running the same machine-wide walk beside it. The next \
                     tick proceeds normally."
                );
                continue;
            }
            if let Err(e) = spawn_blocking_tracked(tick_once).await {
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

    /// What [`owner_of`] must report for a path this test process just created.
    /// On a POSIX platform that is [`OwnerProbe::Invoking`]; elsewhere the
    /// question does not apply.
    #[cfg(unix)]
    fn expected_local_owner() -> OwnerProbe {
        OwnerProbe::Invoking
    }
    #[cfg(not(unix))]
    fn expected_local_owner() -> OwnerProbe {
        OwnerProbe::NotApplicable
    }

    /// The session every scratch fixture lives under: scratch directories are
    /// named by their session's uuid, and a target under anything else is not a
    /// candidate ([`SkipReason::NotSessionScoped`]).
    const SESS: &str = "0a1b2c3d-1111-4222-8333-444455556666";
    /// The encoded-cwd directory the fixture session lives under, here and in
    /// the fixture account home's `projects/`.
    const PROJ: &str = "-home-x-Projects-proj";

    /// A harness-shaped scratchpad tree: `<root>/claude-4242/<project>/<SESS>/scratchpad`.
    /// Returns `(family_root, scratchpad)`.
    fn mk_scratchpad_tree(base: &Path) -> (PathBuf, PathBuf) {
        let family = base.join("claude-4242");
        let scratch = family.join(format!("{PROJ}/{SESS}/scratchpad"));
        fs::create_dir_all(&scratch).unwrap();
        (family, scratch)
    }

    /// Set `path`'s mtime `secs` seconds into the past.
    fn age_path(path: &Path, secs: u64) {
        let t = std::time::SystemTime::now() - Duration::from_secs(secs);
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(t)).unwrap();
    }

    /// Age every artifact the idle probe reads, so a [`mk_target_root`] target
    /// has been idle for `secs`.
    fn age_target(root: &Path, secs: u64) {
        for rel in [
            "CACHEDIR.TAG",
            ".rustc_info.json",
            "debug/.cargo-lock",
            "debug",
            "debug/deps",
        ] {
            let p = root.join(rel);
            if p.exists() {
                age_path(&p, secs);
            }
        }
    }

    /// A fixture account home holding a transcript for `session` last written
    /// `secs` ago, under [`PROJ`].
    fn mk_account_home(base: &Path, name: &str, session: &str, secs: u64) -> PathBuf {
        let home = base.join(name);
        let dir = home.join(format!("projects/{PROJ}"));
        fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join(format!("{session}.jsonl"));
        fs::write(&transcript, b"{}").unwrap();
        age_path(&transcript, secs);
        home
    }

    /// A listable, EMPTY fixture process table named `name` under `base`: no
    /// processes, no locks.
    fn mk_proc_root(base: &Path, name: &str) -> PathBuf {
        let root = base.join(name);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("locks"), b"").unwrap();
        root
    }

    /// The scratch environment in which [`SESS`] is IDLE: one account home whose
    /// transcript is three days old, and an empty process table.
    fn idle_env(base: &Path) -> ScratchEnv {
        let home = mk_account_home(base, "acct-a", SESS, 3 * 86_400);
        ScratchEnv::fixed(mk_proc_root(base, "proc"), Some(vec![home]))
    }

    /// Classifier options over [`idle_env`], the dirty probe stubbed clean.
    fn idle_opts(base: &Path, grace: Duration) -> ClassifyOptions {
        ClassifyOptions::with_probe(grace, |_| DirtyProbe::Clean).with_scratch_env(idle_env(base))
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
        mk_target_root(&wt.join("target-arming"));

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
            verdict("target-arming"),
            Err(SkipReason::OwnedByBuildPool),
            "the arming prove's warm target is the build system's, not a renamed orphan"
        );
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
            owner: OwnerProbe::Invoking,
            family_root: None,
        };
        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
        assert_eq!(
            classify_candidate(&demoted(Path::new("/ws/repo-wt-x/target-agent")), &opts),
            Err(SkipReason::OwnedByBuildPool)
        );
        assert_eq!(
            classify_candidate(&demoted(Path::new("/ws/repo-wt-x/target-arming")), &opts),
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
            owner: OwnerProbe::Invoking,
            family_root: None,
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
            SkipReason::NotSessionScoped,
            SkipReason::SessionLive,
            SkipReason::SessionUnknown,
            SkipReason::HeldByProcess,
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
                "activity-unknown",
                "session-unknown"
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
        // The env is process-global, so hold the ONE shared lock every
        // env-touching test in this binary takes — the set/assert/restore
        // dance this replaces excluded nobody. The restore is declared AFTER
        // the lock so it runs while the lock is still held, and it survives
        // the panic path, which the manual restore did not.
        // Plan `2026-08-25-runner-test-suite-env-isolation`.
        let _g = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[KEEP_ENV]);
        let tmp = tempfile::tempdir().unwrap();
        let tgt = tmp.path().join("target-wt-pinned");
        mk_target_root(&tgt);
        std::env::set_var(KEEP_ENV, "foo, target-wt-pinned ,bar");
        assert_eq!(classify(&tgt, Duration::ZERO), Err(SkipReason::Kept));
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

    /// A symlinked `dir` is unlinked as a link: on Linux the old
    /// `remove_dir` answered ENOTDIR, and nothing may ever follow the link
    /// into the tree it points at.
    #[cfg(unix)]
    #[test]
    fn remove_junction_safe_on_a_symlink_removes_only_the_link() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("keep.txt"), b"canonical").unwrap();
        let link = tmp.path().join("link");
        symlink(&real, &link).unwrap();

        remove_junction_safe(&link).expect("a symlink must be unlinkable");

        assert!(
            fs::symlink_metadata(&link).is_err(),
            "the link itself must be gone"
        );
        assert!(real.is_dir(), "the link's target must survive");
        assert_eq!(
            fs::read(real.join("keep.txt")).unwrap(),
            b"canonical",
            "the target's contents must survive"
        );
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

    /// **A cycle that enumerated nothing is not evidence that there is nothing
    /// to reap** — the same refusal `disk_survey` learned, on the sibling
    /// surface of the same walk.
    ///
    /// `run_cycle` and `disk_survey::build_snapshot` both consume
    /// `enumerate_all`. The survey refuses to certify a zero over an empty walk
    /// that fell short of the tree; this summary went on publishing
    /// `scanned=0 candidates=0 candidate_gb=0.00 reaped=0` — bit-for-bit what a
    /// clean machine emits — and its one honesty annotation, the
    /// `(LOWER BOUND: …)` suffix, keys on counters that are necessarily `0`
    /// when there are no candidates at all, so it is silent in exactly this
    /// state. That matters because this line is the surface the shadow-window
    /// review reads, and Phase 4's arming rule counts clean previews off it.
    #[test]
    fn a_cycle_that_enumerated_nothing_is_unknown_not_a_measured_zero() {
        // A root that does not exist: a detached drive, or a
        // `paths.workspace_root` pointed somewhere gone.
        //
        // ⚠️ THE MECHANISM CHANGED UNDER THIS TEST, and the assertions moved
        // with it while the CLAIM did not. When the cycle walked a single root,
        // a missing one meant `read_dir` failed and `read_errors > 0` carried
        // the shortfall — so that is what this test asserted. Over a LIST of
        // roots the state probe classifies the root `Absent` and short-circuits
        // BEFORE the walk, so no walk counter moves and `read_errors` is
        // legitimately 0 (`an_absent_root_is_reported_absent_never_as_an_empty_walk`
        // asserts exactly that, and asserting the opposite here is how the two
        // tests contradicted each other after the rebase). What must still hold
        // is the claim in this test's NAME: the cycle enumerated nothing, and
        // that nothing is UNKNOWN rather than a measured zero.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("detached-volume");
        let s = run_cycle(&missing, /* armed */ false, Duration::ZERO);
        assert_eq!(s.scanned, 0, "the fixture: the walk enumerated nothing");
        assert!(
            s.no_root_walked(),
            "the root was never walked, so no walk counter can carry the shortfall"
        );
        assert!(
            s.population_unknown(),
            "an empty enumeration from a cycle that walked NOTHING is UNKNOWN, not zero"
        );
        assert!(
            !s.walk_fell_short(),
            "and it is UNKNOWN for the unwalked reason, not because a counter moved — \
             pinning the disjunct that the multi-root arrangement made load-bearing"
        );
        // …and it must SAY so on the line an operator reads. The predicate being
        // right is not the property that matters; the rendered token is, and it
        // had no coverage at all until this assertion — which is precisely how a
        // rebase disarmed the field while every test stayed green.
        let line = render_cycle_line(&s);
        assert!(
            line.contains("population=UNKNOWN"),
            "a cycle that walked nothing must not publish an affirmative token: {line}"
        );

        // The depth bound is the second door, and it is the one that bites
        // without anything failing at all.
        let bound_only = ReapSummary {
            scanned: 0,
            depth_limited_dirs: 7,
            ..Default::default()
        };
        assert!(bound_only.population_unknown());
        assert!(
            bound_only.shortfall_causes().contains("depth bound"),
            "{}",
            bound_only.shortfall_causes()
        );

        // NON-VACUOUS, and the property the bound's exclusion elsewhere exists
        // to protect: a cycle that FOUND roots under the same bitten bound is a
        // measurement. With the workspace root's bound at 4 it bites on any deep
        // tree, so flagging that case would pin the predicate true forever.
        let found = ReapSummary {
            scanned: 3,
            depth_limited_dirs: 7,
            ..Default::default()
        };
        assert!(!found.population_unknown());
        // …but it is NOT a certified measurement either, and the cycle line
        // must not call it one. This is the middle arm: roots were found, and
        // the walk still did not reach everything.
        assert!(
            found.walk_fell_short(),
            "a bitten bound is a shortfall even when roots were found"
        );

        // Second control: a walk that enumerated nothing and saw the WHOLE tree
        // is a real zero, and must stay one.
        let clean_root = tempfile::tempdir().unwrap();
        let clean = run_cycle(clean_root.path(), /* armed */ false, Duration::ZERO);
        assert_eq!(clean.scanned, 0);
        assert!(
            !clean.population_unknown(),
            "nothing fell short, so the empty result is measured: {}",
            clean.shortfall_causes()
        );
        assert!(!clean.walk_fell_short());
        assert_eq!(clean.shortfall_causes(), "");
    }

    /// Every cause the counters carry is named, never just the first that
    /// matched — the failure mode `census_note` avoids by joining its fragments.
    #[test]
    fn shortfall_causes_names_every_cause_not_the_first() {
        let s = ReapSummary {
            scanned: 0,
            truncated: true,
            read_errors: 4137,
            entry_errors: 1,
            reparse_dirs_skipped: 2,
            depth_limited_dirs: 118,
            ..Default::default()
        };
        let causes = s.shortfall_causes();
        for needle in [
            "visit ceiling",
            "4137 directories could not be read",
            "1 directory entry errored",
            "2 junctions were not followed",
            "118 directories were not descended into at the walk's depth bound",
        ] {
            assert!(causes.contains(needle), "missing {needle:?} in {causes:?}");
        }
        assert_eq!(causes.matches("; ").count(), 4, "five causes, four joins");

        // The plural branch of every fragment, which the singular fixture above
        // cannot reach. `entry_errors` is the one that read "5 directories
        // entries errored" until this follow-up.
        let plural = ReapSummary {
            scanned: 0,
            entry_errors: 5,
            reparse_dirs_skipped: 1,
            depth_limited_dirs: 1,
            read_errors: 1,
            ..Default::default()
        }
        .shortfall_causes();
        for needle in [
            "1 directory could not be read",
            "5 directory entries errored mid-listing",
            "1 junction was not followed",
            "1 directory was not descended into at the walk's depth bound",
        ] {
            assert!(plural.contains(needle), "missing {needle:?} in {plural:?}");
        }
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

    /// The recorded read-error list is capped; the COUNT never is.
    ///
    /// The list is held for the whole walk and serialized verbatim into every
    /// `GET /disk/reclaimable` response, and the walk records one entry per
    /// unreadable `.git` across up to [`MAX_DIRS_VISITED`] directories — so one
    /// antivirus- or permission-locked subtree could put six figures of paths on
    /// the wire. Capping the list is safe only while the count stays exact:
    /// [`Enumeration::read_errors_total`] is what every honesty branch reads, so
    /// a cap can never become a silent under-report.
    #[test]
    fn recorded_read_errors_are_capped_but_the_count_is_not() {
        let mut e = Enumeration::default();
        let n = MAX_RECORDED_READ_ERRORS * 3 + 7;
        for i in 0..n {
            e.record_read_error(PathBuf::from(format!("/ws/dir-{i}")), "denied".to_string());
        }
        assert_eq!(e.read_errors_total, n, "the count is exact");
        assert_eq!(
            e.read_errors.len(),
            MAX_RECORDED_READ_ERRORS,
            "the detail list is bounded"
        );
        // The sample is the FIRST failures, not the last: the earliest ones sit
        // nearest the walk's root and say the most about what was missed.
        assert_eq!(e.read_errors[0].0, PathBuf::from("/ws/dir-0"));

        // Non-vacuous: under the cap the list is complete, so an ordinary walk
        // loses nothing.
        let mut small = Enumeration::default();
        small.record_read_error(PathBuf::from("/ws/one"), "denied".to_string());
        assert_eq!(small.read_errors_total, 1);
        assert_eq!(small.read_errors.len(), 1);
    }

    /// The cap-and-count split fails CLOSED at its SOURCE, not only at the copy.
    ///
    /// `ScanStats::read_errors_seen` heals a desynced pair on the way to the
    /// wire, so the survey was covered. The reaper was not: `run_cycle` read
    /// `read_errors_total` raw, so a `read_errors` moved without its counter
    /// under-reported in `ReapSummary` and in the cycle log with nothing to
    /// catch it. Both readers now go through the same fail-closed accessor, so
    /// the failure mode of the split is an OVER-report of incompleteness — the
    /// direction that cannot fabricate a clean walk.
    #[test]
    fn a_desynced_enumeration_over_reports_rather_than_under_reports() {
        let mut e = Enumeration::default();
        e.read_errors.push((
            PathBuf::from("/ws/pushed-past-the-seam"),
            "denied".to_string(),
        ));
        assert_eq!(e.read_errors_total, 0, "the counter was never moved");
        assert_eq!(
            e.read_errors_seen(),
            1,
            "and the reader must not certify a walk clean while an error sits in the list"
        );

        // Non-vacuous, and the property this must not break: the cap still
        // wins whenever the counter is the larger of the two, which is the
        // whole point of keeping an uncapped total.
        let mut capped = Enumeration::default();
        for i in 0..(MAX_RECORDED_READ_ERRORS * 2) {
            capped.record_read_error(PathBuf::from(format!("/ws/d-{i}")), "denied".to_string());
        }
        assert_eq!(capped.read_errors.len(), MAX_RECORDED_READ_ERRORS);
        assert_eq!(capped.read_errors_seen(), MAX_RECORDED_READ_ERRORS * 2);
    }

    /// Nothing pushes a read error past the `record_read_error` seam.
    ///
    /// The cap is only as good as the single door it is enforced at, and both
    /// `Enumeration` fields are `pub` — a direct `read_errors.push` compiles
    /// fine, moves the list without the count, and silently un-caps the wire.
    /// The seam was DOCUMENTED as the only way in when it was added; this is
    /// what makes that true. Source-level because the property is "no such call
    /// exists", which no runtime assertion can observe.
    #[test]
    fn read_errors_are_only_ever_appended_through_the_recording_seam() {
        const SRC: &str = include_str!("orphan_target_reaper.rs");
        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        // The one legitimate push is inside `record_read_error` itself.
        let seam = prod
            .split_once("fn record_read_error(")
            .map(|(_, after)| after)
            .expect("the recording seam must exist; move this pin if it is renamed");
        let seam_body = seam.split_once("\n    }\n").map(|(b, _)| b).unwrap_or(seam);
        assert_eq!(
            seam_body.matches("read_errors.push(").count(),
            1,
            "the seam itself appends exactly once"
        );
        assert_eq!(
            prod.matches("read_errors.push(").count(),
            1,
            "a read error was appended outside `record_read_error`, which moves the list without \
             `read_errors_total` and un-caps what goes into every `GET /disk/reclaimable` response"
        );
    }

    /// The periodic cycle yields while a reclaim-preview walk is in flight.
    ///
    /// Source-level for the same reason the instance gate's pin is: the
    /// predicate reads process-global state parallel test threads mutate.
    ///
    /// Gating per INSTANCE left the two identical machine-wide walks still
    /// racing each other ON the owning one — same `enumerate_all`, same sizing,
    /// cadences colliding every hour. One-directional by design: the preview
    /// never yields (INV-D1), and the reaper is the one that ticks four times
    /// as often and is dry-run by default.
    #[test]
    fn the_periodic_cycle_yields_to_an_in_flight_preview_walk() {
        const SRC: &str = include_str!("orphan_target_reaper.rs");
        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        let spawn = prod
            .split_once("pub fn spawn_orphan_reaper() {")
            .map(|(_, after)| after)
            .expect("spawn_orphan_reaper must exist; move this pin if it is renamed");
        let body = spawn.split_once("\n}\n").map(|(b, _)| b).unwrap_or(spawn);
        assert!(
            body.contains("disk_survey::build_active()"),
            "the 900s full-machine walk is spawned without consulting the preview's walk, so both \
             run the same sizing pass over the same disk at once on the owning instance"
        );
        assert!(
            body.contains("continue"),
            "and it must SKIP the tick rather than block on it — queueing behind a multi-minute \
             walk would defeat `MissedTickBehavior::Skip` on this interval"
        );
    }

    /// The periodic reaper is instance-gated, exactly like the reclaim preview.
    ///
    /// Source-level for the same reason
    /// `disk_survey::the_periodic_surveyor_is_instance_gated` is: the predicate
    /// reads process-global state that parallel test threads mutate.
    ///
    /// Phase 2 replaced this cycle's shallow container scan with the SAME
    /// machine-wide walk the preview publishes — and gated only the preview,
    /// leaving the copy that runs four times as often (900 s against 3600 s)
    /// starting on every secondary and every supervisor-spawned temp runner.
    /// This is also the destructive engine, so once the arming flag is flipped
    /// the ungated version would have N instances racing over the same paths.
    #[test]
    fn the_periodic_reaper_is_instance_gated() {
        const SRC: &str = include_str!("orphan_target_reaper.rs");
        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        let spawn = prod
            .split_once("pub fn spawn_orphan_reaper() {")
            .map(|(_, after)| after)
            .expect("spawn_orphan_reaper must exist; move this pin if it is renamed");
        let body = spawn.split_once("\n}\n").map(|(b, _)| b).unwrap_or(spawn);
        assert!(
            body.contains("owns_shared_root_state()"),
            "the 900s full-machine walk is spawned with no instance guard, so every secondary \
             and every supervisor-spawned temp runner runs its own copy of it — and would race \
             them all over the same paths once the arming flag is flipped."
        );
    }

    /// The depth bound and the reparse skip are both silent omissions unless
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

    /// A NESTED junction, inside the tree being reaped — the case the
    /// top-level test above does not reach.
    ///
    /// Before the fix the loop tested `is_dir()` first and handled a link
    /// only within that arm. A junction reports `is_symlink() == true` and
    /// `is_dir() == false` (asserted below, on the pinned toolchain), so it
    /// fell through to a bare `let _ = std::fs::remove_file(&path)`, which is
    /// `PermissionDenied` (os error 5) on a junction. The error was
    /// discarded, the junction survived, the closing `remove_dir` failed as
    /// "directory not empty", and the reap could never complete. With the old
    /// loop this test fails on the `expect` below.
    #[cfg(windows)]
    #[test]
    fn nested_windows_junction_is_unlinked_and_the_reap_completes() {
        let tmp = tempfile::tempdir().unwrap();
        // The canonical tree the nested junction points at, OUTSIDE the tree
        // being reaped — following the link would be real data loss.
        let outside = tmp.path().join("canonical");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious.rlib"), b"do-not-delete").unwrap();

        // The tree to reap: real content, a real subdirectory, and a junction.
        let tree = tmp.path().join("target-to-reap");
        mk_target_root(&tree);
        fs::write(tree.join("debug/deps/obj.o"), b"garbage").unwrap();
        // `join("debug/linked")` would keep the forward slash verbatim, and
        // `cmd` then reads `/linked` as a switch ("Invalid switch") — so
        // `mklink_junction` fails and this test SKIPS instead of testing
        // anything. Build the path one segment at a time.
        let nested = tree.join("debug").join("linked");
        if !mklink_junction(&nested, &outside) {
            eprintln!("skipping: mklink /J unavailable in this environment");
            return;
        }

        // The measurement this fix rests on, pinned as an assertion.
        assert!(is_junction(&nested), "is_junction must see the nested link");
        let ft = fs::symlink_metadata(&nested).unwrap().file_type();
        assert!(ft.is_symlink(), "a junction must report is_symlink()");
        assert!(
            !ft.is_dir(),
            "a junction must NOT report is_dir(), so a nested link can only reach the is_symlink arm"
        );

        remove_junction_safe(&tree).expect("the reap must complete");

        assert!(!tree.exists(), "the whole reaped tree must be gone");
        assert!(
            outside.join("precious.rlib").exists(),
            "content behind the nested junction must NOT be followed/deleted"
        );
    }

    /// [`remove_nested_link`]'s contract, on every platform: a link that is
    /// already gone is an idempotent `Ok`, and any OTHER error propagates.
    ///
    /// The old loop discarded both with `let _ =`. A non-empty plain
    /// directory is the portable failure injection — `remove_link` reaches
    /// `remove_dir` on Windows and `remove_file` on Unix, and both refuse it.
    /// Nothing may be deleted on the failing path.
    #[test]
    fn remove_nested_link_tolerates_a_vanished_link_and_propagates_anything_else() {
        let tmp = tempfile::tempdir().unwrap();

        // Already gone — a concurrent reaper pass, not a failure.
        let missing = tmp.path().join("never-existed");
        assert!(
            remove_nested_link(&missing).is_ok(),
            "a link that vanished under us must be an idempotent Ok"
        );

        // A real error must propagate, and must delete nothing.
        let real = tmp.path().join("not-a-link");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("keep.txt"), b"data").unwrap();
        assert!(
            remove_nested_link(&real).is_err(),
            "a removal that failed must propagate, never be swallowed"
        );
        assert!(
            real.join("keep.txt").exists(),
            "the failing path must have deleted nothing"
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

    // =====================================================================
    // Phase 3 — the SECOND enumeration root (the harness scratchpad tree).
    // =====================================================================

    /// The scratchpad tree's targets are enumerated, and they land in the class
    /// this module ALREADY has for "a bare out-of-tree root with no enclosing
    /// git repo". Verified against the real classifier rather than assumed — no
    /// fifth [`TargetClass`] was needed.
    #[test]
    fn the_scratchpad_root_enumerates_its_targets_as_sibling_nongit() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, scratch) = mk_scratchpad_tree(tmp.path());
        mk_target_root(&scratch.join("cargo-target"));

        let cands = enumerate_candidates(&family);
        assert_eq!(cands.len(), 1, "the scratchpad target root is enumerated");
        assert!(cands[0].path.ends_with("cargo-target"));
        assert_eq!(
            cands[0].class,
            TargetClass::SiblingNonGit,
            "a scratchpad target has no enclosing checkout, so it lands in the EXISTING \
             out-of-tree class"
        );
        assert_eq!(cands[0].repo_root, None);
        assert_eq!(
            cands[0].owner,
            expected_local_owner(),
            "the enumerator must RECORD ownership, not leave it at a default"
        );
        // And it keeps a verb at zero grace — this is the population the second
        // root exists to reach, so the test above must not be vacuous.
        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
        assert_eq!(classify_candidate(&cands[0], &opts), Ok(()));
    }

    /// **The verb stays TARGET-DIRECTORIES-ONLY.** 23 git worktrees whose HEAD
    /// is reachable from no remote ref live inside `/tmp/claude-1000`
    /// scratchpads on this box (one dirty), and tmpfs has no undelete. Keeping
    /// the verb on cargo target roots puts every one of them outside the
    /// population BY CONSTRUCTION rather than by a rule someone must remember.
    #[test]
    fn a_non_target_directory_under_the_scratchpad_root_is_never_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, scratch) = mk_scratchpad_tree(tmp.path());

        // A git worktree holding work that exists nowhere else.
        let wt = scratch.join("feat-unpushed");
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::write(wt.join(".git"), b"gitdir: /repo/.git/worktrees/feat\n").unwrap();
        fs::write(wt.join("src/lib.rs"), b"fn main() {}").unwrap();
        // A plain notes dir.
        let notes = scratch.join("notes");
        fs::create_dir_all(&notes).unwrap();
        fs::write(notes.join("findings.md"), b"# notes").unwrap();
        // A dir NAMED like a build dir but carrying no cargo marker at all.
        let lookalike = scratch.join("target-lookalike");
        fs::create_dir_all(lookalike.join("debug")).unwrap();
        // One genuine target root, so the assertion below is non-vacuous.
        mk_target_root(&scratch.join("cargo-target"));

        let names: Vec<String> = enumerate_candidates(&family)
            .iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["cargo-target".to_string()],
            "only a cargo target root is ever a CANDIDATE; got {names:?}"
        );

        // Pinned a second time at the classifier, so even a future enumerator
        // change could not hand one of these a verb.
        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
        for p in [wt.clone(), notes.clone(), lookalike.clone()] {
            let c = Candidate {
                path: p.clone(),
                class: TargetClass::SiblingNonGit,
                repo_root: None,
                enclosing_git_unreadable: false,
                owner: OwnerProbe::Invoking,
                family_root: None,
            };
            assert_eq!(
                classify_candidate(&c, &opts),
                Err(SkipReason::UnrecognizedLayout),
                "{}",
                p.display()
            );
        }
        assert!(wt.exists() && notes.exists() && lookalike.exists());
    }

    /// An entry owned by ANOTHER uid is skipped with its own typed reason. The
    /// gate is the OUTERMOST one: every engine-ownership gate below it
    /// presupposes the path is ours to reason about at all, and under a sticky
    /// `<tmp>` that presupposition is no longer free.
    #[test]
    fn an_entry_owned_by_a_different_uid_is_skipped_with_the_typed_reason() {
        let opts = ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean);
        let foreign = |path: &str| Candidate {
            path: PathBuf::from(path),
            class: TargetClass::SiblingNonGit,
            repo_root: None,
            enclosing_git_unreadable: false,
            owner: OwnerProbe::Foreign { uid: 999 },
            family_root: None,
        };
        assert_eq!(
            classify_candidate(&foreign("/tmp/claude-999/s/scratchpad/cargo-target"), &opts),
            Err(SkipReason::ForeignOwner)
        );
        // It wins even over a name OUR build system would otherwise claim, so we
        // never assert ownership of another account's directory.
        assert_eq!(
            classify_candidate(&foreign("/tmp/other/target-agent"), &opts),
            Err(SkipReason::ForeignOwner)
        );
        // It strips the verb, exactly like the engine-ownership reasons...
        assert!(SkipReason::ForeignOwner.owned_elsewhere());
        // ...but it is an OBSERVATION, not a missing reading, so it must not
        // inflate the "we could not read it" tally the arming decision reads.
        assert!(!SkipReason::ForeignOwner.is_unknown());
        assert_eq!(SkipReason::ForeignOwner.token(), "foreign-owner");
        let mut s = ReapSummary::default();
        count_skip(&mut s, SkipReason::ForeignOwner);
        assert_eq!(s.skipped_foreign_owner, 1);
        assert_eq!(
            s.skipped_other_owner, 0,
            "kept out of the engine-owner bucket"
        );
        assert_eq!(s.skipped_unknown, 0);
        // The same path owned by US is this reaper's territory — so the
        // assertions above are non-vacuous.
        let ours = Candidate {
            owner: OwnerProbe::Invoking,
            ..foreign("/tmp/claude-999/s/scratchpad/cargo-target")
        };
        assert_ne!(
            classify_candidate(&ours, &opts),
            Err(SkipReason::ForeignOwner)
        );
    }

    #[cfg(unix)]
    #[test]
    fn owner_of_reads_the_real_uid_and_compares_against_geteuid() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let meta = fs::symlink_metadata(tmp.path()).unwrap();
        assert_eq!(meta.uid(), invoking_uid());
        assert_eq!(owner_of(&meta), OwnerProbe::Invoking);
    }

    /// The tmp root is DISCOVERED — platform temp dir plus the effective uid,
    /// both read at runtime. A hardcoded `/tmp/claude-1000` is right on exactly
    /// one box.
    #[cfg(unix)]
    #[test]
    fn the_scratchpad_root_is_discovered_from_the_platform_never_hardcoded() {
        let inputs = ReapRootInputs {
            // The override replaces the whole family; this test is about the
            // DISCOVERED root, whatever the environment it runs in sets.
            scratch_root_override: None,
            ..ReapRootInputs::ambient()
        };
        assert_eq!(inputs.tmp, std::env::temp_dir());
        assert_eq!(inputs.uid, Some(invoking_uid()));
        let first = reap_roots_from(&inputs)
            .into_iter()
            .find(|r| r.kind == ReapRootKind::HarnessScratchpad)
            .and_then(|r| r.path)
            .expect("unix always resolves a scratch root");
        assert!(
            first.starts_with(std::env::temp_dir()),
            "{}",
            first.display()
        );
        assert_eq!(
            first.file_name().unwrap().to_string_lossy(),
            format!("claude-{}", invoking_uid())
        );
    }

    /// **Absent is ABSENT, not empty.** A missing root must not render as a
    /// walked root that found nothing — the same silent-empty-as-NO error the
    /// `OwnershipUnknown` reasoning refuses everywhere else.
    #[test]
    fn an_absent_root_is_reported_absent_never_as_an_empty_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("claude-4242"); // deliberately never created
        let s = run_cycle_over_roots(
            &[ReapRoot::at(ReapRootKind::HarnessScratchpad, &missing)],
            Duration::ZERO,
        );
        assert_eq!(s.roots.len(), 1, "a root is never silently dropped");
        assert_eq!(s.roots[0].state, RootState::Absent);
        assert_eq!(s.scanned, 0);
        assert_eq!(
            s.read_errors, 0,
            "an absent root is not a failed read either"
        );
        assert!(
            s.roots[0].capacity.is_none(),
            "nothing was stat'd, so no capacity is claimed"
        );
        let rendered = s.roots[0].render();
        assert_eq!(
            rendered,
            format!("harness-scratchpad:absent(path={})", missing.display()),
            "a scratch root names the tree it could not find"
        );
        assert!(
            !rendered.contains("scanned"),
            "an unwalked root must not carry a scanned=0 that reads as 'walked, found nothing': \
             {rendered}"
        );
        assert!(s.roots[0].state.describe().contains("ABSENT"));
    }

    /// Root 1 keeps its exact behaviour, and root 2 adds the population that was
    /// structurally invisible before.
    #[test]
    fn both_roots_are_walked_and_root_one_behaves_exactly_as_before() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let (family, scratch) = mk_scratchpad_tree(tmp.path());
        mk_target_root(&ws.join("target-wt-a"));
        mk_target_root(&scratch.join("cargo-target"));

        let one = run_cycle(&ws, /* armed */ false, Duration::ZERO);
        assert_eq!((one.scanned, one.candidates), (1, 1));
        assert_eq!(one.roots.len(), 1);
        assert_eq!(one.roots[0].kind, ReapRootKind::Workspace);

        let both = run_cycle_over_roots_with(
            &[
                ReapRoot::at(ReapRootKind::Workspace, &ws),
                ReapRoot::at(ReapRootKind::HarnessScratchpad, &family),
            ],
            &idle_opts(tmp.path(), Duration::ZERO),
        );
        assert_eq!(both.roots.len(), 2);
        assert_eq!(both.roots[0].scanned, 1, "root 1 unchanged");
        assert_eq!(both.roots[1].scanned, 1, "root 2 newly visible");
        assert_eq!(both.candidates, 2);
        assert_eq!(both.reaped, 0, "DRY-RUN by default removes nothing");
    }

    /// Two roots resolving to the same tree are walked once, and the second says
    /// WHY it contributed nothing rather than reporting an empty walk.
    #[test]
    fn a_repeated_root_is_marked_already_walked_not_counted_twice() {
        let tmp = tempfile::tempdir().unwrap();
        mk_target_root(&tmp.path().join("target-wt-a"));
        let s = run_cycle_over_roots(
            &[
                ReapRoot::at(ReapRootKind::Workspace, tmp.path()),
                ReapRoot::at(ReapRootKind::HarnessScratchpad, tmp.path()),
            ],
            Duration::ZERO,
        );
        assert_eq!(s.scanned, 1, "the shared tree is walked exactly once");
        assert_eq!(s.roots[1].state, RootState::AlreadyWalked);
        assert_eq!(
            s.roots[1].render(),
            format!(
                "harness-scratchpad:already-walked(path={})",
                tmp.path().display()
            )
        );
    }

    /// Every pre-existing gate still applies on the new root: a pin is honoured,
    /// and DRY-RUN removes nothing.
    #[test]
    fn a_pin_inside_the_scratchpad_root_is_honoured_and_dry_run_removes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, scratch) = mk_scratchpad_tree(tmp.path());
        let pinned = scratch.join("pinned-target");
        mk_target_root(&pinned);
        fs::write(pinned.join(KEEP_SENTINEL), b"").unwrap();
        let free = scratch.join("free-target");
        mk_target_root(&free);

        let s = run_cycle_over_roots_with(
            &[ReapRoot::at(ReapRootKind::HarnessScratchpad, &family)],
            &idle_opts(tmp.path(), Duration::ZERO),
        );
        assert_eq!(s.scanned, 2);
        assert_eq!(s.skipped_kept, 1, "the pin is honoured on the new root too");
        assert_eq!(s.candidates, 1);
        assert_eq!(s.reaped, 0);
        assert!(pinned.exists() && free.exists(), "DRY-RUN removes nothing");
    }

    /// G-dirty reaches into the new root too: a renamed build dir inside a
    /// scratchpad git worktree is refused while the tree is dirty, and refused
    /// under the DISTINCT reason when the probe could not answer.
    #[test]
    fn g_dirty_still_applies_to_a_worktree_inside_the_scratchpad_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, scratch) = mk_scratchpad_tree(tmp.path());
        let wt = scratch.join("wt-a");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), b"gitdir: /repo/.git/worktrees/a\n").unwrap();
        mk_target_root(&wt.join("target-renamed"));

        let cands = enumerate_candidates(&family);
        let c = cands
            .iter()
            .find(|c| c.path.ends_with("target-renamed"))
            .expect("the renamed build dir is enumerated");
        assert_eq!(c.class, TargetClass::SiblingWorktree);
        assert_eq!(
            classify_candidate(
                c,
                &ClassifyOptions::with_probe(Duration::ZERO, |_| { DirtyProbe::Dirty })
            ),
            Err(SkipReason::WorktreeDirty)
        );
        assert_eq!(
            classify_candidate(
                c,
                &ClassifyOptions::with_probe(Duration::ZERO, |_| { DirtyProbe::Unknown })
            ),
            Err(SkipReason::DirtyUnknown)
        );
    }

    // =====================================================================
    // Phase 5a — the depth bound is per-root, and truncation is legible.
    // =====================================================================

    /// The bound is a property of the ROOT. The workspace root's behaviour must
    /// not change (4 — its layout is flat), and the scratchpad root must reach
    /// the measured population (6 — targets sit at root-relative depth 5, plus
    /// one level of headroom).
    #[test]
    fn the_depth_bound_belongs_to_the_root_workspace_four_scratchpad_six() {
        assert_eq!(
            ReapRootKind::Workspace.max_walk_depth(),
            4,
            "the workspace root's behaviour is unchanged by this phase"
        );
        assert_eq!(
            ReapRootKind::HarnessScratchpad.max_walk_depth(),
            6,
            "<project>/<session>/scratchpad/<subdir>/<target> is depth 5; 6 is that plus one \
             level of headroom"
        );
        assert_eq!(WORKSPACE_MAX_WALK_DEPTH, 4);
        assert_eq!(SCRATCHPAD_MAX_WALK_DEPTH, 6);

        // …and every constructed root carries its kind's bound, so the two can
        // never drift apart.
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            ReapRoot::at(ReapRootKind::Workspace, tmp.path()).max_walk_depth,
            4
        );
        assert_eq!(
            ReapRoot::at(ReapRootKind::HarnessScratchpad, tmp.path()).max_walk_depth,
            6
        );
        assert_eq!(
            ReapRoot::unresolved(ReapRootKind::HarnessScratchpad, "why").max_walk_depth,
            6,
            "an unresolved root still knows the bound it WOULD have used"
        );
        let inputs = ReapRootInputs {
            workspace_root: Some(tmp.path().to_path_buf()),
            tmp: tmp.path().to_path_buf(),
            uid: Some(4242),
            ..ReapRootInputs::default()
        };
        let resolved = reap_roots_from(&inputs);
        assert_eq!(
            resolved.first().map(|r| (r.kind, r.max_walk_depth)),
            Some((ReapRootKind::Workspace, 4))
        );
        assert!(
            resolved.len() >= 2
                && resolved[1..]
                    .iter()
                    .all(|r| (r.kind, r.max_walk_depth) == (ReapRootKind::HarnessScratchpad, 6)),
            "every resolved scratch root carries the scratch bound, not a shared constant: \
             {resolved:?}"
        );
    }

    /// A bound of N reaches a target root at depth N + 1 (the root is
    /// recognised while its PARENT is iterated), so 4 reaches depth 5 and 6
    /// reaches depth 7. Pinned because the off-by-one is what decides whether a
    /// real layout is visible — and because the reading that motivated this
    /// phase got it wrong in the other direction.
    #[test]
    fn a_bound_of_n_reaches_a_target_root_at_depth_n_plus_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk_target_root(&root.join("a/b/c/d/at-depth-5"));
        mk_target_root(&root.join("a/b/c/d/e/at-depth-6"));

        let at_four = enumerate_all_within(root, 4);
        let names = |e: &Enumeration| {
            let mut v: Vec<String> = e
                .candidates
                .iter()
                .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        assert_eq!(names(&at_four), vec!["at-depth-5".to_string()]);
        assert!(
            at_four.depth_limited_dirs > 0,
            "and the level it did NOT reach is counted"
        );

        let at_five = enumerate_all_within(root, 5);
        assert_eq!(
            names(&at_five),
            vec!["at-depth-5".to_string(), "at-depth-6".to_string()]
        );
        assert_eq!(at_five.depth_limited_dirs, 0);
    }

    /// The layout the raise is FOR: a repo checked out inside a scratchpad
    /// subdir puts its Rust build dir at
    /// `<project>/<session>/scratchpad/<subdir>/<checkout>/src-tauri/target` —
    /// depth 7, which the workspace bound cannot reach and the scratchpad bound
    /// can. Under the old single constant it was INVISIBLE, and the cycle said
    /// `scanned=0` in the same shape a real census would.
    #[test]
    fn the_scratchpad_bound_reaches_the_nested_checkout_target_the_workspace_bound_hid() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, scratch) = mk_scratchpad_tree(tmp.path());
        // family/<project>/<session>/scratchpad = depth 3; +<subdir>/<checkout>/
        // src-tauri/<target> = depth 7.
        let deep = scratch.join("dn-wt/qontinui-runner/src-tauri/target");
        mk_target_root(&deep);

        let shallow = run_cycle_over_roots(
            &[ReapRoot::at(ReapRootKind::Workspace, &family)],
            Duration::ZERO,
        );
        assert_eq!(
            shallow.scanned, 0,
            "the workspace bound reaches depth 5 and this target is at 7"
        );
        assert!(
            shallow.completeness().is_lower_bound(),
            "…and it must SAY that 0 is a floor: {:?}",
            shallow.completeness()
        );

        let deep_enough = run_cycle_over_roots_with(
            &[ReapRoot::at(ReapRootKind::HarnessScratchpad, &family)],
            &idle_opts(tmp.path(), Duration::ZERO),
        );
        assert_eq!(
            (deep_enough.scanned, deep_enough.candidates),
            (1, 1),
            "the scratchpad bound reaches the measured population"
        );
        assert_eq!(
            deep_enough.completeness(),
            Completeness::Complete,
            "and having reached it, the walk was not cut short at all"
        );
        assert_eq!(deep_enough.depth_limited_dirs, 0);
        assert_eq!(deep_enough.reaped, 0, "DRY-RUN removes nothing");
    }

    /// **A truncated candidate set may never be readable as a census.** The
    /// counter alone was not enough: `candidates=4` came off a depth-limited
    /// walk in exactly the shape a complete answer has. The fact now lives in
    /// the type AND in the operator-facing line.
    #[test]
    fn a_truncated_walk_says_so_in_the_type_and_in_the_summary_line() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        mk_target_root(&root.join("target-wt-shallow"));
        // Depth 6 under a bound of 4 (which reaches 5) — present on disk,
        // absent from the answer.
        mk_target_root(&root.join("a/b/c/d/e/target-wt-hidden"));

        let s = run_cycle_over_roots(
            &[ReapRoot::at(ReapRootKind::Workspace, &root)],
            Duration::ZERO,
        );
        assert_eq!(s.scanned, 1, "the deep root really is hidden");

        // 1. In the type, per root.
        let report = &s.roots[0];
        let per_root = report
            .completeness
            .clone()
            .expect("a walked root always records its coverage");
        assert!(
            !per_root.is_complete() && per_root.is_lower_bound(),
            "{per_root:?}"
        );
        match per_root {
            Completeness::Truncated {
                depth_limited_dirs,
                visit_ceiling_hit,
                ..
            } => {
                assert!(depth_limited_dirs > 0);
                assert!(!visit_ceiling_hit, "this one was the DEPTH bound");
            }
            Completeness::Complete => unreachable!(),
        }

        // 2. In the type, per cycle.
        assert!(s.completeness().is_lower_bound());

        // 3. In the per-root token, beside the counts it qualifies.
        let rendered = report.render();
        assert!(
            rendered.contains("census=INCOMPLETE") && rendered.contains("depth<=4"),
            "{rendered}"
        );

        // 4. In the operator-facing cycle line — beside `candidates=`, which is
        //    the number a consumer actually reads.
        let line = render_cycle_line(&s);
        assert!(line.contains("INCOMPLETE CENSUS"), "{line}");
        // The cycle-level verdict beside the per-root census marker: this walk
        // DID find roots, so it is `partial` — never the affirmative `measured`,
        // and never `UNKNOWN`, which is reserved for an empty enumeration.
        assert!(line.contains("population=partial"), "{line}");
        assert!(line.contains("LOWER BOUND, not a total"), "{line}");
        // The cycle-level field, not the per-root token inside `roots=[…]`,
        // which carries its own `candidates=` — hence the leading space.
        let after_candidates = line
            .split_once(" candidates=")
            .expect("the line names candidates")
            .1;
        assert!(
            after_candidates.starts_with("1 [INCOMPLETE CENSUS"),
            "the marker must sit directly beside the number it qualifies, where a reader cannot \
             miss it: {line}"
        );

        // …and the honest opposite: a complete walk carries no marker, so the
        // marker never becomes noise a reader learns to skip.
        let shallow_only = tempfile::tempdir().unwrap();
        mk_target_root(&shallow_only.path().join("target-wt-a"));
        let clean = run_cycle_over_roots(
            &[ReapRoot::at(ReapRootKind::Workspace, shallow_only.path())],
            Duration::ZERO,
        );
        assert_eq!(clean.completeness(), Completeness::Complete);
        let clean_line = render_cycle_line(&clean);
        assert!(!clean_line.contains("INCOMPLETE"), "{clean_line}");
        assert!(clean_line.contains("census=complete"), "{clean_line}");
        assert!(
            clean_line.contains("population=measured"),
            "a complete walk that found roots earns the affirmative token: {clean_line}"
        );
    }

    /// Rolling two roots up may only ever OVER-report incompleteness: one
    /// truncated root makes the cycle-level sums a floor.
    #[test]
    fn completeness_rolls_up_with_incompleteness_winning() {
        let truncated = Completeness::Truncated {
            depth_limited_dirs: 5_226,
            dirs_visited: 40_000,
            visit_ceiling_hit: false,
        };
        assert_eq!(
            Completeness::Complete.merged(truncated.clone()),
            truncated,
            "a complete root cannot launder a truncated one"
        );
        assert_eq!(
            truncated.clone().merged(Completeness::Complete),
            truncated,
            "…in either order"
        );
        assert_eq!(
            truncated.clone().merged(Completeness::Truncated {
                depth_limited_dirs: 4,
                dirs_visited: 100,
                visit_ceiling_hit: true,
            }),
            Completeness::Truncated {
                depth_limited_dirs: 5_230,
                dirs_visited: 40_100,
                visit_ceiling_hit: true,
            },
            "counts add and the harder cause survives"
        );
        assert_eq!(
            Completeness::Complete.merged(Completeness::Complete),
            Completeness::Complete
        );

        // The visit ceiling is the OTHER truncation cause and reaches the same
        // verdict, so a cycle stopped by cost is not readable as a census either.
        let mut e = Enumeration {
            dirs_visited: MAX_DIRS_VISITED,
            truncated: true,
            ..Default::default()
        };
        assert!(Completeness::from_walk(&e).is_lower_bound());
        e.truncated = false;
        assert_eq!(Completeness::from_walk(&e), Completeness::Complete);
    }

    // =====================================================================
    // Phase 4 — the inode axis beside the byte one.
    // =====================================================================

    #[test]
    fn capacity_arithmetic_is_percent_used_on_both_axes() {
        // 100 blocks of 4 KiB with 25 free ⇒ 75 % used; 400 inodes, 100 free ⇒ 75 %.
        let c = capacity_from_statvfs(4096, 100, 25, 400, 100);
        assert_eq!(c.bytes_total, 409_600);
        assert_eq!(c.bytes_free, 102_400);
        assert_eq!(c.inodes_total, 400);
        assert_eq!(c.inodes_free, 100);
        assert!((c.bytes_pct_used.unwrap() - 75.0).abs() < 1e-9);
        assert!((c.inodes_pct_used.unwrap() - 75.0).abs() < 1e-9);
        assert_eq!(c.bytes_unknown_reason, None);
        assert_eq!(c.inodes_unknown_reason, None);
        assert_eq!(c.bytes_axis(90.0), PressureAxis::Ok);
        assert_eq!(c.inodes_axis(70.0), PressureAxis::Pressured);
    }

    /// **`f_files == 0` is UNKNOWN, not 0 %.** A tmpfs mounted `nr_inodes=0`,
    /// and btrfs/xfs/zfs generally, report no inode cap — so "0 used of 0"
    /// computes as the HEALTHIEST possible reading on exactly the filesystems
    /// where nothing is measurable. And the byte half, which IS a genuine
    /// measurement, must survive: nulling both throws away a reading that was
    /// actually taken.
    #[test]
    fn a_filesystem_reporting_no_inode_cap_is_unknown_and_the_byte_half_survives() {
        let c = capacity_from_statvfs(4096, 100, 25, 0, 0);
        assert_eq!(c.inodes_pct_used, None);
        assert_eq!(c.inodes_unknown_reason, Some(INODE_LIMIT_NOT_REPORTED));
        assert_eq!(c.inodes_axis(90.0), PressureAxis::Unknown);
        assert!((c.bytes_pct_used.unwrap() - 75.0).abs() < 1e-9);
        assert_eq!(c.bytes_axis(90.0), PressureAxis::Ok);
        assert_eq!(c.bytes_unknown_reason, None);
        assert!(!c.wholly_unknown());
        assert!(c.render(90.0).contains(INODE_LIMIT_NOT_REPORTED));
        assert!(c.render(90.0).contains("75.0% used"));

        // The mirror case: no block count, a real inode reading.
        let n = capacity_from_statvfs(4096, 0, 0, 400, 100);
        assert_eq!(n.bytes_pct_used, None);
        assert_eq!(n.bytes_unknown_reason, Some(BLOCK_COUNT_NOT_REPORTED));
        assert!((n.inodes_pct_used.unwrap() - 75.0).abs() < 1e-9);
    }

    /// The 2026-09-01 invariant as ARITHMETIC: inode pressure fires while the
    /// byte axis reads healthy. ANDing the two reproduces the outage exactly —
    /// `/tmp` at 100 % inodes, 27 % bytes, and every session's Bash tool gone.
    #[test]
    fn inode_pressure_fires_while_the_byte_axis_reads_healthy() {
        // The measured shape of `/tmp` on the operator box during the incident.
        let c = capacity_from_statvfs(4096, 48_344_193, 35_549_375, 1_048_576, 0);
        assert_eq!(c.inodes_pct_used, Some(100.0));
        assert!(
            c.bytes_pct_used.unwrap() < 30.0,
            "the byte axis read healthy throughout: {:?}",
            c.bytes_pct_used
        );
        assert_eq!(c.inodes_axis(90.0), PressureAxis::Pressured);
        assert_eq!(c.bytes_axis(90.0), PressureAxis::Ok);
        assert!(
            c.pressured(90.0),
            "OR, never AND — an AND here IS the outage"
        );
        assert!(c.render(90.0).contains("=> PRESSURED"));
    }

    /// Both axes unmeasurable must render UNKNOWN, never an all-clear.
    #[test]
    fn neither_axis_measurable_renders_unknown_not_an_all_clear() {
        let c = capacity_from_statvfs(4096, 0, 0, 0, 0);
        assert!(c.wholly_unknown());
        assert!(!c.pressured(90.0));
        assert!(c.render(90.0).contains("NOT an all-clear"));
    }

    /// The env threshold clamps to a usable range rather than producing an axis
    /// that can never fire (or always fires).
    #[test]
    fn the_pressure_threshold_falls_back_on_a_nonsense_value() {
        assert_eq!(axis_verdict(Some(90.0), 90.0), PressureAxis::Pressured);
        assert_eq!(axis_verdict(Some(89.9), 90.0), PressureAxis::Ok);
        assert_eq!(axis_verdict(None, 90.0), PressureAxis::Unknown);
    }

    /// A live capacity read of the process's own temp dir must PARSE — the axes
    /// are either measured or UNKNOWN-with-a-reason, never a silent zero.
    #[test]
    fn a_live_capacity_read_is_measured_or_unknown_with_a_reason() {
        let c = fs_capacity(&std::env::temp_dir());
        assert_eq!(
            c.bytes_pct_used.is_none(),
            c.bytes_unknown_reason.is_some(),
            "an unknown axis always carries its reason, and a known one never does"
        );
        assert_eq!(
            c.inodes_pct_used.is_none(),
            c.inodes_unknown_reason.is_some()
        );
    }

    const INODE_FIXTURE_ENV: &str = "QONTINUI_REAPER_INODE_FIXTURE_DIR";
    const INNER_PROBE: &str =
        "agent_worktree::orphan_target_reaper::tests::capped_mount_inner_probe";

    /// The inner half of [`a_capped_scratch_mount_diverges_on_the_two_axes`],
    /// run by that test INSIDE a user+mount namespace where the capped tmpfs is
    /// visible. `#[ignore]` because it is meaningless outside that namespace; a
    /// plain `cargo test` never runs it, and the outer test invokes it by exact
    /// name.
    #[test]
    #[ignore = "driven by a_capped_scratch_mount_diverges_on_the_two_axes, inside a mount namespace"]
    fn capped_mount_inner_probe() {
        let dir = std::env::var(INODE_FIXTURE_ENV).expect("the outer test sets the fixture dir");
        let cap = fs_capacity(Path::new(&dir));
        let inodes = cap
            .inodes_pct_used
            .expect("a tmpfs mounted with nr_inodes reports f_files");
        let bytes = cap.bytes_pct_used.expect("a tmpfs reports f_blocks");
        assert!(inodes >= 99.0, "inodes_pct_used = {inodes}");
        assert!(bytes < 1.0, "bytes_pct_used = {bytes}");
        assert_eq!(cap.inodes_axis(90.0), PressureAxis::Pressured);
        assert_eq!(cap.bytes_axis(90.0), PressureAxis::Ok);
        assert!(cap.pressured(90.0));
    }

    /// **Parity with the shell probe.** `qontinui-claude-config`'s
    /// `scripts/tmpfs-inode-probe.sh` asserts the same divergence invariant on a
    /// scratch mount; this asserts it through the RUST reader, so the two cannot
    /// drift on the property that matters: `inodes_pct_used >= 99` while
    /// `bytes_pct_used == 0` on a capped mount.
    ///
    /// The mount is created in a user+mount namespace, so it needs no
    /// privileges — but it does need unprivileged userns to be enabled. Where it
    /// is not, this test SKIPS LOUDLY rather than reading green, and the pure
    /// arithmetic above carries the invariant instead.
    #[test]
    fn a_capped_scratch_mount_diverges_on_the_two_axes() {
        const SKIP: &str = "SKIPPED (LOUDLY) a_capped_scratch_mount_diverges_on_the_two_axes";
        if !cfg!(target_os = "linux") {
            eprintln!(
                "{SKIP}: not Linux — no tmpfs `nr_inodes` fixture is constructible here. \
                       The invariant is still asserted as arithmetic by \
                       `inode_pressure_fires_while_the_byte_axis_reads_healthy`."
            );
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            eprintln!("{SKIP}: could not locate the test binary to re-enter.");
            return;
        };
        let holder = tempfile::tempdir().unwrap();
        let mp = holder.path().join("capped");
        fs::create_dir_all(&mp).unwrap();
        let script = format!(
            "set -e\n\
             mount -t tmpfs -o nr_inodes=200,size=64M tmpfs '{mp}'\n\
             i=0; while [ $i -lt 400 ]; do : > '{mp}'/f$i 2>/dev/null || true; i=$((i+1)); done\n\
             exec '{exe}' --ignored --exact {INNER_PROBE} --nocapture\n",
            mp = mp.display(),
            exe = exe.display(),
        );
        let out = match std::process::Command::new("unshare")
            .args(["-Urm", "bash", "-c", &script])
            .env(INODE_FIXTURE_ENV, &mp)
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                eprintln!("{SKIP}: `unshare` could not be run ({e}).");
                return;
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stdout.contains("test result: ok.") {
            return; // the invariant held against a real capped mount
        }
        // Distinguish "the invariant FAILED" from "the fixture could not be
        // built here". Only the first may fail the suite; the second must be
        // loud, because a test that cannot fire must never read green.
        if stdout.contains("test result: FAILED") || stdout.contains("panicked at") {
            panic!(
                "the capped-mount divergence invariant FAILED:\n--- stdout ---\n{stdout}\n\
                 --- stderr ---\n{stderr}"
            );
        }
        eprintln!(
            "{SKIP}: the capped tmpfs fixture could not be created in this environment \
             (unprivileged user namespaces or `mount` unavailable). The divergence invariant \
             is still asserted as arithmetic by \
             `inode_pressure_fires_while_the_byte_axis_reads_healthy`.\n\
             --- fixture stderr ---\n{stderr}"
        );
    }

    // -----------------------------------------------------------------
    // Plan 2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-
    // fetch-into-empty-dirs, Phase 3: the walkers read the directory record.
    // The candidates are the same by construction and the bytes differ only
    // where a directory record lags the file, so a behaviour test cannot fail
    // on a revert — these pin the SOURCE instead (mutation: restore a per-entry
    // `symlink_metadata(` or `is_junction(` call and they go red).
    // -----------------------------------------------------------------

    /// The CODE of `fn <name>(` in this file's production half, with `//`
    /// comments stripped so prose naming the old calls neither trips nor
    /// satisfies a pin. Only `//` comments: a `/* */` comment, or a `//`
    /// inside a string literal, is not handled — neither occurs in these
    /// bodies today.
    fn prod_fn_code(name: &str) -> String {
        const SRC: &str = include_str!("orphan_target_reaper.rs");
        let prod = SRC
            .split_once("\n#[cfg(test)]\nmod tests {")
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        let start = prod
            .split_once(&format!("fn {name}("))
            .map(|(_, after)| after)
            .unwrap_or_else(|| panic!("fn {name} must exist; move this pin if it is renamed"));
        let body = start.split_once("\n}\n").map(|(b, _)| b).unwrap_or(start);
        body.lines()
            .map(|l| l.split("//").next().unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_enumeration_walk_reads_directory_records_not_files() {
        let code = prod_fn_code("walk");
        assert!(
            code.contains("entry.metadata()"),
            "the walk must read each entry's directory record"
        );
        for opened in ["symlink_metadata(", "is_junction(", "is_target_root("] {
            assert!(
                !code.contains(opened),
                "`{opened}` re-opens every entry of a ~180k-directory walk; read the record instead"
            );
        }
    }

    #[test]
    fn the_size_walk_reads_directory_records_not_files() {
        let code = prod_fn_code("measure_dir_size");
        assert!(code.contains("entry.metadata()"));
        for opened in ["symlink_metadata(", "is_junction("] {
            assert!(
                !code.contains(opened),
                "`{opened}` opens every file of every candidate target it sizes"
            );
        }
    }

    #[test]
    fn sizing_counts_files_and_skips_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::create_dir_all(root.join("sub").join("deep")).unwrap();
        std::fs::write(root.join("sub").join("b.bin"), vec![0u8; 250]).unwrap();
        std::fs::write(root.join("sub").join("deep").join("c.bin"), vec![0u8; 7]).unwrap();
        // A link to a sizeable tree outside: must contribute 0.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("big.bin"), vec![0u8; 5000]).unwrap();
        let link = root.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        #[cfg(windows)]
        if !mklink_junction(&link, outside.path()) {
            eprintln!("skipping: mklink /J unavailable on this host");
            return;
        }
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "the link must exist, or the test proves nothing about skipping it"
        );
        let m = measure_dir_size(root).expect("root is readable");
        assert_eq!(
            m.bytes, 357,
            "100 + 250 + 7; the linked tree is never counted"
        );
        assert_eq!(m.unreadable_dirs, 0);
    }

    /// The reparse test the walkers now apply to a directory RECORD answers
    /// true for a junction read through `read_dir`, and false for a plain dir.
    #[cfg(windows)]
    #[test]
    fn a_junctions_directory_record_carries_the_reparse_bit() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target-dir");
        std::fs::create_dir(&target).unwrap();
        let link = dir.path().join("jn");
        if !mklink_junction(&link, &target) {
            eprintln!("skipping: mklink /J unavailable on this host");
            return;
        }
        let mut seen = std::collections::HashMap::new();
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            seen.insert(
                entry.file_name().to_string_lossy().into_owned(),
                super::super::census::metadata_is_reparse_point(&meta),
            );
        }
        assert_eq!(seen.get("jn"), Some(&true));
        assert_eq!(seen.get("target-dir"), Some(&false));
    }

    // =====================================================================
    // Plan `2026-11-01-session-scratch-on-disk-has-no-reclaim-arm-…`,
    // Phases 4 and 5 — the on-disk scratch family, its gates, its arming.
    // =====================================================================

    /// `canonicalize`, for planting paths a `/proc` fixture reports: the kernel
    /// reports RESOLVED paths, and so does the gate it is compared against.
    fn canon(p: &Path) -> PathBuf {
        fs::canonicalize(p).unwrap()
    }

    /// `(tmp, family, target)`: `claude-4242/<PROJ>/<SESS>/scratchpad/tgt`, a
    /// cargo target idle for three days.
    fn idle_scratch_target() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let (family, scratch) = mk_scratchpad_tree(tmp.path());
        let target = scratch.join("tgt");
        mk_target_root(&target);
        fs::write(target.join("debug/.cargo-lock"), b"").unwrap();
        age_target(&target, 3 * 86_400);
        (tmp, family, target)
    }

    /// The one candidate a family walk finds under `family`.
    fn only_candidate(family: &Path) -> Candidate {
        let mut found = enumerate_family_within(family, SCRATCHPAD_MAX_WALK_DEPTH).candidates;
        assert_eq!(
            found.len(),
            1,
            "exactly one target root expected: {found:?}"
        );
        found.remove(0)
    }

    /// One day: long enough that a three-day-old target and transcript clear
    /// it, and that a minute-old transcript does not.
    const DAY: Duration = Duration::from_secs(86_400);

    fn scratch_opts(env: ScratchEnv) -> ClassifyOptions {
        ClassifyOptions::with_probe(DAY, |_| DirtyProbe::Clean).with_scratch_env(env)
    }

    /// Plant one own-uid process in a fixture `/proc`: argv, environment, cwd
    /// and the paths it holds open.
    #[cfg(unix)]
    fn plant_proc(
        proc_root: &Path,
        pid: u32,
        argv: &[&str],
        env: &[(&str, &str)],
        cwd: &Path,
        fds: &[&Path],
    ) -> PathBuf {
        let dir = proc_root.join(pid.to_string());
        fs::create_dir_all(dir.join("fd")).unwrap();
        fs::write(dir.join("cmdline"), argv.join("\0")).unwrap();
        let environ: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        fs::write(dir.join("environ"), environ.join("\0")).unwrap();
        std::os::unix::fs::symlink(cwd, dir.join("cwd")).unwrap();
        std::os::unix::fs::symlink("/usr/bin/fixture-exe", dir.join("exe")).unwrap();
        for (i, f) in fds.iter().enumerate() {
            std::os::unix::fs::symlink(f, dir.join("fd").join(i.to_string())).unwrap();
        }
        dir
    }

    /// D1 — the family is every `claude-<uid>` tree under the on-disk scratch
    /// dir, at depth 1 AND depth 2, and a link is never a member.
    #[cfg(unix)]
    #[test]
    fn the_family_is_every_account_tree_at_depth_one_and_two_and_never_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = tmp.path().join("scratch");
        let mk = |rel: &str| fs::create_dir_all(scratch.join(rel)).unwrap();
        mk(".claude-a/claude-4242");
        mk(".claude-b/claude-4242");
        mk(".claude-c/lineage-1/claude-4242");
        mk(".claude-e/not-a-family-member");
        mk(".claude-f/claude-9999"); // another uid's tree is not ours
        fs::write(scratch.join("loose-file.txt"), b"x").unwrap();
        // A link named like a member: how a scratch tree comes to hold a live
        // checkout. Refused, and counted.
        let elsewhere = tmp.path().join("a-live-checkout");
        fs::create_dir_all(&elsewhere).unwrap();
        mk(".claude-d");
        std::os::unix::fs::symlink(&elsewhere, scratch.join(".claude-d/claude-4242")).unwrap();

        let fam = scratch_family_under(tmp.path(), 4242);
        assert_eq!(
            fam.members,
            vec![
                scratch.join(".claude-a/claude-4242"),
                scratch.join(".claude-b/claude-4242"),
                scratch.join(".claude-c/lineage-1/claude-4242"),
            ],
            "depth 1 and depth 2 are both members; a link, another uid and a bare account are not"
        );
        assert_eq!(fam.links_skipped, 1);
        assert!(!fam.incomplete(), "{fam:?}");

        // A missing scratch dir is an empty family, not a failed read.
        let none = scratch_family_under(&tmp.path().join("no-such-dir"), 4242);
        assert!(none.members.is_empty() && !none.incomplete(), "{none:?}");
    }

    /// The discovery pass is bounded and SAYS so: a prefix of the family is not
    /// the family.
    #[cfg(unix)]
    #[test]
    fn family_discovery_is_bounded_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fs::create_dir_all(tmp.path().join(format!("scratch/.claude-{i}/claude-4242")))
                .unwrap();
        }
        let cut = scratch_family_with_budget(tmp.path(), 4242, 3);
        assert!(cut.truncated && cut.incomplete(), "{cut:?}");
        let whole = scratch_family_with_budget(tmp.path(), 4242, 1_000);
        assert!(!whole.truncated, "{whole:?}");
        assert_eq!(whole.members.len(), 5);
    }

    /// Phase 4, test 1 — a fixture `qontinui_dir` with two account trees and an
    /// EMPTY tmp root produces candidates from both trees. Today's code walks
    /// `<tmp>/claude-<uid>` alone, so it finds neither: the cycle read
    /// `scanned=0 census=complete` over the wrong directory while the right one
    /// held 417 GB.
    #[cfg(unix)]
    #[test]
    fn a_qontinui_dir_with_two_account_trees_and_an_empty_tmp_yields_candidates_from_both() {
        let tmp = tempfile::tempdir().unwrap();
        let uid = invoking_uid();
        let qdir = tmp.path().join("qdir");
        let sess_b = "9f8e7d6c-1111-4222-8333-444455556666";
        for (account, sess) in [(".claude-a", SESS), (".claude-b", sess_b)] {
            let tgt = qdir.join(format!(
                "scratch/{account}/claude-{uid}/{PROJ}/{sess}/scratchpad/tgt"
            ));
            mk_target_root(&tgt);
            age_target(&tgt, 3 * 86_400);
        }
        // The tmp root exists and holds nothing — the shape of the incident.
        fs::create_dir_all(tmp.path().join(format!("empty-tmp/claude-{uid}"))).unwrap();
        let inputs = ReapRootInputs {
            tmp: tmp.path().join("empty-tmp"),
            qontinui_dir: Some(qdir.clone()),
            uid: Some(uid),
            // Dry-run: this test counts candidates, it does not reap them.
            scratch_reap_env: Some("0".to_string()),
            ..ReapRootInputs::default()
        };
        let roots = reap_roots_from(&inputs);
        let scratch_roots: Vec<_> = roots
            .iter()
            .filter(|r| r.kind == ReapRootKind::HarnessScratchpad)
            .collect();
        assert_eq!(
            scratch_roots.len(),
            3,
            "the tmp root and one root PER account tree: {scratch_roots:?}"
        );

        let home_a = mk_account_home(tmp.path(), "home-a", SESS, 3 * 86_400);
        let home_b = mk_account_home(tmp.path(), "home-b", sess_b, 3 * 86_400);
        let env = ScratchEnv::fixed(mk_proc_root(tmp.path(), "proc"), Some(vec![home_a, home_b]));
        let s = run_cycle_over_roots_with(&roots, &scratch_opts(env));
        assert_eq!(
            s.candidates,
            2,
            "a candidate from EACH account tree: {}",
            render_cycle_line(&s)
        );
        let line = render_cycle_line(&s);
        for account in [".claude-a", ".claude-b"] {
            let named = format!("scratch/{account}/claude-{uid}");
            assert!(
                line.contains(&named),
                "the line names every tree it read ({named}): {line}"
            );
        }
        // Per root, not just in total: each account tree scanned its own target.
        let per_root: Vec<usize> = s
            .roots
            .iter()
            .filter(|r| r.path.as_deref().is_some_and(|p| p.starts_with(&qdir)))
            .map(|r| r.scanned)
            .collect();
        assert_eq!(per_root, vec![1, 1], "{line}");
    }

    /// A family that could not be established is a root of its own that says so
    /// — never a cycle that reads as having covered a family it did not find.
    #[cfg(unix)]
    #[test]
    fn a_family_that_could_not_be_established_says_so_as_a_root() {
        let tmp = tempfile::tempdir().unwrap();
        let base = ReapRootInputs {
            tmp: tmp.path().join("t"),
            uid: Some(invoking_uid()),
            ..ReapRootInputs::default()
        };
        let scratch_states = |inputs: &ReapRootInputs| -> Vec<(&'static str, bool)> {
            reap_roots_from(inputs)
                .into_iter()
                .filter(|r| r.kind == ReapRootKind::HarnessScratchpad)
                .skip(1) // the tmp root, always first
                .map(|r| (r.state.token(), r.path.is_some()))
                .collect()
        };
        // No `~/.qontinui` at all.
        assert_eq!(scratch_states(&base), vec![("unresolved", false)]);
        // A scratch dir that is not there: ABSENT, naming the path.
        let absent = ReapRootInputs {
            qontinui_dir: Some(tmp.path().join("q1")),
            ..base.clone()
        };
        assert_eq!(scratch_states(&absent), vec![("absent", true)]);
        // A scratch dir that exists and holds no `claude-<uid>` anywhere.
        fs::create_dir_all(tmp.path().join("q2/scratch/.claude-a")).unwrap();
        let empty = ReapRootInputs {
            qontinui_dir: Some(tmp.path().join("q2")),
            ..base.clone()
        };
        assert_eq!(scratch_states(&empty), vec![("unresolved", true)]);
        // And none of them lets the cycle claim a measured population.
        let s = run_cycle_over_roots(&reap_roots_from(&empty), Duration::ZERO);
        assert!(
            !render_cycle_line(&s).contains("population=measured"),
            "{}",
            render_cycle_line(&s)
        );
        // The override still replaces the whole family outright.
        let overridden = ReapRootInputs {
            scratch_root_override: Some(format!("{}", tmp.path().join("elsewhere").display())),
            qontinui_dir: Some(tmp.path().join("q2")),
            ..base
        };
        assert_eq!(
            reap_roots_from(&overridden)
                .iter()
                .filter(|r| r.kind == ReapRootKind::HarnessScratchpad)
                .count(),
            1
        );
    }

    /// D2 — a root that scanned nothing says how much it READ, so `scanned=0`
    /// over a populated tree is distinguishable from `scanned=0` over an empty
    /// one. The cycle line rendered both identically.
    #[test]
    fn a_root_that_scanned_nothing_states_how_much_it_read() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let populated = tmp.path().join("populated");
        fs::create_dir_all(populated.join("sub")).unwrap();
        fs::write(populated.join("sub/big.bin"), vec![0u8; 8 << 20]).unwrap();
        let s = run_cycle_over_roots(
            &[
                ReapRoot::at(ReapRootKind::HarnessScratchpad, &empty),
                ReapRoot::at(ReapRootKind::HarnessScratchpad, &populated),
            ],
            Duration::ZERO,
        );
        let [e, p] = [s.roots[0].render(), s.roots[1].render()];
        assert!(
            e.contains("scanned=0") && e.contains("walked_gb=0.00") && e.contains("dirs=0"),
            "{e}"
        );
        assert!(
            p.contains("scanned=0") && p.contains("walked_gb=0.01") && p.contains("dirs=1"),
            "{p}"
        );
    }

    #[test]
    fn a_scratch_session_is_project_uuid_and_a_level_below() {
        let fam = Path::new("/f/claude-1");
        let s = scratch_session_of(fam, &fam.join(format!("{PROJ}/{SESS}/scratchpad/tgt")))
            .expect("project/uuid/…/target is a session target");
        assert_eq!((s.project.as_str(), s.uuid.as_str()), (PROJ, SESS));
        assert_eq!(s.dir, fam.join(PROJ).join(SESS));
        // The shapes that are NOT: directly under the family root, directly under
        // a project, under a non-uuid directory, and the session dir itself.
        for rel in [
            "coord-target-abc".to_string(),
            format!("{PROJ}/tgt"),
            format!("{PROJ}/sess-abc/scratchpad/tgt"),
            format!("{PROJ}/{SESS}"),
        ] {
            assert_eq!(scratch_session_of(fam, &fam.join(&rel)), None, "{rel}");
        }
        assert_eq!(
            scratch_session_of(fam, Path::new("/elsewhere/p/u/x")),
            None,
            "a path outside the family root belongs to no session of it"
        );
        assert!(is_session_uuid("0A1B2C3D-1111-4222-8333-444455556666"));
        assert!(!is_session_uuid("0a1b2c3d-1111-4222-8333-44445555666")); // 35
        assert!(!is_session_uuid("0a1b2c3d_1111_4222_8333_444455556666"));
        assert!(!is_session_uuid("0a1b2c3d-1111-4222-8333-44445555666g"));
    }

    /// G-session-scope — a target with no `<project>/<uuid>/` owner is never a
    /// candidate on a family root, whatever its size or age (the steward's
    /// `NON_SESSION`). Unix-only: its control target carries a `.cargo-lock`,
    /// whose inode key exists only where there is an inode to read.
    #[cfg(unix)]
    #[test]
    fn a_target_with_no_session_directory_is_never_a_candidate_on_a_family_root() {
        let tmp = tempfile::tempdir().unwrap();
        let family = tmp.path().join("claude-4242");
        for rel in [
            "coord-target-abc",               // directly under the family root
            &format!("{PROJ}/shared-target"), // directly under a project dir
            &format!("{PROJ}/not-a-uuid/scratchpad/tgt"),
        ] {
            mk_target_root(&family.join(rel));
            age_target(&family.join(rel), 3 * 86_400);
        }
        let opts = idle_opts(tmp.path(), DAY);
        let found = enumerate_family_within(&family, SCRATCHPAD_MAX_WALK_DEPTH).candidates;
        assert_eq!(found.len(), 3);
        for c in &found {
            assert_eq!(
                classify_candidate(c, &opts),
                Err(SkipReason::NotSessionScoped),
                "{}",
                c.path.display()
            );
        }
        // …and it is counted as what it is, not as an unknown reading.
        assert!(!SkipReason::NotSessionScoped.is_unknown());
        let s = run_cycle_over_roots_with(
            &[ReapRoot::at(ReapRootKind::HarnessScratchpad, &family).with_armed(true)],
            &opts,
        );
        assert_eq!((s.scanned, s.candidates, s.reaped), (3, 0, 0));
        assert_eq!(s.skipped_non_session, 3);
        assert_eq!(s.skipped_unknown, 0);
        // Control: the same target under a uuid session IS reapable.
        let (tmp2, family2, _t) = idle_scratch_target();
        assert_eq!(
            classify_candidate(&only_candidate(&family2), &idle_opts(tmp2.path(), DAY)),
            Ok(())
        );
    }

    /// Phase 4, test 2a — a target whose session reads LIVE stays out: a fresh
    /// transcript, a process naming the uuid in argv or in its environment, or a
    /// cwd or open fd under the session's directory.
    #[cfg(unix)]
    #[test]
    fn a_target_whose_session_reads_live_stays_out() {
        let (tmp, family, _target) = idle_scratch_target();
        let c = only_candidate(&family);
        let sess_dir = canon(&family.join(PROJ).join(SESS));
        let upper = SESS.to_ascii_uppercase();
        let home_old = mk_account_home(tmp.path(), "home-old", SESS, 3 * 86_400);
        let verdict = |proc_name: &str, homes: Vec<PathBuf>, plant: &dyn Fn(&Path)| {
            let proc_root = mk_proc_root(tmp.path(), proc_name);
            plant(&proc_root);
            classify_candidate(&c, &scratch_opts(ScratchEnv::fixed(proc_root, Some(homes))))
        };
        let nothing: &dyn Fn(&Path) = &|_| {};

        // Control: idle transcript, empty process table — the target IS reapable,
        // so every refusal below is attributable to the one thing it changes.
        assert_eq!(verdict("p0", vec![home_old.clone()], nothing), Ok(()));

        let fresh = mk_account_home(tmp.path(), "home-fresh", SESS, 60);
        assert_eq!(
            verdict("p1", vec![fresh], nothing),
            Err(SkipReason::SessionLive),
            "a transcript written a minute ago"
        );
        assert_eq!(
            verdict("p2", vec![home_old.clone()], &|root| {
                plant_proc(
                    root,
                    101,
                    &["claude", "--resume", upper.as_str()],
                    &[],
                    Path::new("/"),
                    &[],
                );
            }),
            Err(SkipReason::SessionLive),
            "the uuid in argv, in either case"
        );
        assert_eq!(
            verdict("p3", vec![home_old.clone()], &|root| {
                plant_proc(
                    root,
                    102,
                    &["bash"],
                    &[("CLAUDE_CODE_SESSION_ID", SESS)],
                    Path::new("/"),
                    &[],
                );
            }),
            Err(SkipReason::SessionLive),
            "the uuid in CLAUDE_CODE_SESSION_ID"
        );
        let in_session = sess_dir.join("scratchpad");
        assert_eq!(
            verdict("p4", vec![home_old.clone()], &|root| {
                plant_proc(root, 103, &["vim"], &[], &in_session, &[]);
            }),
            Err(SkipReason::SessionLive),
            "a cwd under the session directory"
        );
        assert_eq!(
            verdict("p5", vec![home_old.clone()], &|root| {
                plant_proc(
                    root,
                    104,
                    &["tail"],
                    &[],
                    Path::new("/"),
                    &[&in_session.join("notes.log")],
                );
            }),
            Err(SkipReason::SessionLive),
            "an open fd under the session directory"
        );
        // A process that names ANOTHER session is nothing to do with this one.
        assert_eq!(
            verdict("p6", vec![home_old], &|root| {
                plant_proc(
                    root,
                    105,
                    &["claude", "--resume", "ffffffff-1111-4222-8333-444455556666"],
                    &[],
                    Path::new("/"),
                    &[],
                );
            }),
            Ok(())
        );
    }

    /// Phase 4, test 2b — a target whose liveness CANNOT BE READ stays out, and
    /// each way of not reading it refuses under a reason that says so.
    #[cfg(unix)]
    #[test]
    fn a_target_whose_liveness_cannot_be_read_stays_out() {
        let (tmp, family, _target) = idle_scratch_target();
        let c = only_candidate(&family);
        let proc_root = mk_proc_root(tmp.path(), "proc");
        let home = mk_account_home(tmp.path(), "home", SESS, 3 * 86_400);
        let v = |env: ScratchEnv| classify_candidate(&c, &scratch_opts(env));
        assert_eq!(
            v(ScratchEnv::fixed(
                proc_root.clone(),
                Some(vec![home.clone()])
            )),
            Ok(()),
            "control"
        );
        // The accounts root did not resolve.
        assert_eq!(
            v(ScratchEnv::fixed(proc_root.clone(), None)),
            Err(SkipReason::SessionUnknown)
        );
        // Resolved, but no account holds a transcript for the session: a session
        // with no copy anywhere is UNKNOWN, never old.
        let empty_home = tmp.path().join("empty-home");
        fs::create_dir_all(empty_home.join("projects")).unwrap();
        assert_eq!(
            v(ScratchEnv::fixed(proc_root.clone(), Some(vec![empty_home]))),
            Err(SkipReason::SessionUnknown)
        );
        // The process table cannot be listed. Two independent layers refuse this —
        // the snapshot is not `listed`, and with no table there is no `locks` read
        // either — so the first is also asserted on its own, or removing it would
        // leave the verdict unchanged and the test green.
        assert!(!ProcSnapshot::read(&tmp.path().join("no-such-proc")).listed);
        assert!(ProcSnapshot::read(&proc_root).listed);
        assert_eq!(
            v(ScratchEnv::fixed(
                tmp.path().join("no-such-proc"),
                Some(vec![home.clone()])
            )),
            Err(SkipReason::LockStateUnknown)
        );
        // Listable, but `/proc/locks` is not there: the holder read cannot be made.
        let no_locks = mk_proc_root(tmp.path(), "no-locks");
        fs::remove_file(no_locks.join("locks")).unwrap();
        assert_eq!(
            v(ScratchEnv::fixed(no_locks, Some(vec![home]))),
            Err(SkipReason::LockStateUnknown)
        );
        // The same three, as the buckets the shadow window reads.
        assert!(SkipReason::SessionUnknown.is_unknown());
        assert!(SkipReason::LockStateUnknown.is_unknown());
        assert!(!SkipReason::SessionLive.is_unknown());
    }

    /// Phase 4, test 3 — a session idle under THIS account's transcript but
    /// fresh under another account's stays out: sessions move accounts and leave
    /// a frozen copy behind.
    #[cfg(unix)]
    #[test]
    fn a_session_idle_here_but_fresh_under_another_account_stays_out() {
        let (tmp, family, _target) = idle_scratch_target();
        let c = only_candidate(&family);
        let proc_root = mk_proc_root(tmp.path(), "proc");
        let frozen = mk_account_home(tmp.path(), "home-old-account", SESS, 5 * 86_400);
        let live = mk_account_home(tmp.path(), "home-new-account", SESS, 30);
        let verdict = |homes: Vec<PathBuf>| {
            classify_candidate(
                &c,
                &scratch_opts(ScratchEnv::fixed(proc_root.clone(), Some(homes))),
            )
        };
        assert_eq!(
            verdict(vec![frozen.clone()]),
            Ok(()),
            "control: the frozen copy alone reads idle — which is the trap"
        );
        for order in [
            vec![frozen.clone(), live.clone()],
            vec![live.clone(), frozen.clone()],
        ] {
            assert_eq!(
                verdict(order),
                Err(SkipReason::SessionLive),
                "the NEWEST copy across every account decides, in either order"
            );
        }
    }

    /// Phase 4, test 4 — a target with a live holder stays out: a process whose
    /// `CARGO_TARGET_DIR` names it (absolute or relative to its cwd), whose cwd
    /// or open fd is inside it, or a held flock on its `.cargo-lock`.
    #[cfg(unix)]
    #[test]
    fn a_target_with_a_live_holder_stays_out() {
        let (tmp, family, target) = idle_scratch_target();
        let c = only_candidate(&family);
        let home = mk_account_home(tmp.path(), "home", SESS, 3 * 86_400);
        let canon_target = canon(&target);
        let scratchpad = canon_target.parent().unwrap().to_path_buf();
        let verdict = |name: &str, plant: &dyn Fn(&Path)| {
            let root = mk_proc_root(tmp.path(), name);
            plant(&root);
            classify_candidate(
                &c,
                &scratch_opts(ScratchEnv::fixed(root, Some(vec![home.clone()]))),
            )
        };
        let tgt = canon_target.to_str().unwrap().to_string();

        assert_eq!(verdict("h0", &|_| {}), Ok(()), "control");
        assert_eq!(
            verdict("h1", &|r| {
                plant_proc(
                    r,
                    201,
                    &["cargo", "build"],
                    &[("CARGO_TARGET_DIR", tgt.as_str())],
                    Path::new("/"),
                    &[],
                );
            }),
            Err(SkipReason::HeldByProcess),
            "a live CARGO_TARGET_DIR holder"
        );
        assert_eq!(
            verdict("h2", &|r| {
                plant_proc(
                    r,
                    202,
                    &["cargo"],
                    &[("CARGO_TARGET_DIR", "tgt")],
                    &scratchpad,
                    &[],
                );
            }),
            Err(SkipReason::HeldByProcess),
            "a RELATIVE CARGO_TARGET_DIR resolves against the holder's cwd"
        );
        assert_eq!(
            verdict("h3", &|r| {
                plant_proc(
                    r,
                    203,
                    &["bash"],
                    &[],
                    &canon_target.join("debug/deps"),
                    &[],
                );
            }),
            Err(SkipReason::HeldByProcess),
            "a shell parked inside it"
        );
        assert_eq!(
            verdict("h4", &|r| {
                plant_proc(
                    r,
                    204,
                    &["rustc"],
                    &[],
                    Path::new("/"),
                    &[&canon_target.join("debug/deps/x.rlib")],
                );
            }),
            Err(SkipReason::HeldByProcess),
            "an open file inside it"
        );
        assert_eq!(
            verdict("h5", &|r| {
                plant_proc(
                    r,
                    205,
                    &["cargo"],
                    &[("CARGO_TARGET_DIR", "/somewhere/else")],
                    Path::new("/"),
                    &[],
                );
            }),
            Ok(()),
            "a CARGO_TARGET_DIR that names some OTHER directory holds nothing here"
        );
        // A granted flock row on the target's `.cargo-lock`, keyed by the file's
        // real device and inode.
        let lock = target.join("debug/.cargo-lock");
        let meta = fs::metadata(&lock).unwrap();
        let (major, minor) = {
            use std::os::unix::fs::MetadataExt;
            super::super::cargo_locks::dev_major_minor(meta.dev())
        };
        let ino = {
            use std::os::unix::fs::MetadataExt;
            meta.ino()
        };
        assert_eq!(
            verdict("h6", &|r| {
                fs::write(
                    r.join("locks"),
                    format!("1: FLOCK  ADVISORY  WRITE 4242 {major:02x}:{minor:02x}:{ino} 0 EOF\n"),
                )
                .unwrap();
            }),
            Err(SkipReason::Building),
            "a held flock reads as a build in flight even when no process entry names the target"
        );
        // …and a row on SOME OTHER inode does not.
        assert_eq!(
            verdict("h7", &|r| {
                fs::write(
                    r.join("locks"),
                    format!(
                        "1: FLOCK  ADVISORY  WRITE 4242 {major:02x}:{minor:02x}:{} 0 EOF\n",
                        ino + 1
                    ),
                )
                .unwrap();
            }),
            Ok(())
        );
    }

    /// Against the REAL `/proc`: a flock this process holds on the target's
    /// `.cargo-lock` keeps it, and releasing it frees it. The fixture tests above
    /// drive the parser and the key match; this one drives the kernel.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_flock_held_on_the_real_proc_keeps_the_target() {
        use std::os::unix::io::AsRawFd;
        let (tmp, family, target) = idle_scratch_target();
        let c = only_candidate(&family);
        let home = mk_account_home(tmp.path(), "home", SESS, 3 * 86_400);
        let opts = scratch_opts(ScratchEnv::fixed(PathBuf::from("/proc"), Some(vec![home])));
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(target.join("debug/.cargo-lock"))
            .unwrap();
        // SAFETY: a valid fd owned by `lock` for the duration of the call.
        let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "taking the flock failed");
        assert_eq!(
            classify_candidate(&c, &opts),
            Err(SkipReason::Building),
            "a flock held on the real kernel table keeps the target"
        );
        drop(lock);
        // A fresh options value: the process table is memoised per value.
        let after = scratch_opts(ScratchEnv::fixed(
            PathBuf::from("/proc"),
            Some(vec![tmp.path().join("home")]),
        ));
        assert_eq!(
            classify_candidate(&c, &after),
            Ok(()),
            "released, nothing holds it — so the refusal above was the flock"
        );
    }

    /// A denial on an own-uid process entry is TOLERATED but COUNTED. Refusing on
    /// every one would leave the gates inert on any box with a user manager
    /// (`systemd --user` denies its cwd to its own uid), and a live build is
    /// caught by `/proc/locks` regardless of its entry.
    #[cfg(unix)]
    #[test]
    fn an_own_uid_process_the_kernel_denies_us_is_counted_not_a_refusal() {
        use std::os::unix::fs::PermissionsExt;
        let (tmp, family, _target) = idle_scratch_target();
        let c = only_candidate(&family);
        let home = mk_account_home(tmp.path(), "home", SESS, 3 * 86_400);
        let proc_root = mk_proc_root(tmp.path(), "proc");
        let dir = plant_proc(
            &proc_root,
            301,
            &["systemd", "--user"],
            &[],
            Path::new("/"),
            &[],
        );
        fs::set_permissions(dir.join("environ"), fs::Permissions::from_mode(0o000)).unwrap();
        // Skip on the CONDITION, not on uid 0: root and `CAP_DAC_OVERRIDE` both
        // read a mode-000 file, and then no denial can be planted at all.
        if fs::read(dir.join("environ")).is_ok() {
            eprintln!(
                "SKIPPED (LOUDLY): this uid reads a mode-000 file, so no denial can be planted"
            );
            return;
        }
        let env = ScratchEnv::fixed(proc_root, Some(vec![home]));
        let opts = scratch_opts(env);
        assert_eq!(
            classify_candidate(&c, &opts),
            Ok(()),
            "a process this uid may not inspect does not refuse the target"
        );
        assert_eq!(opts.scratch.opaque_procs_seen(), 1, "…but it is counted");
        let s = run_cycle_over_roots_with(
            &[ReapRoot::at(ReapRootKind::HarnessScratchpad, &family)],
            &opts,
        );
        assert_eq!(s.scratch_opaque_procs, 1);
        assert!(
            render_cycle_line(&s).contains("scratch_opaque_procs=1"),
            "{}",
            render_cycle_line(&s)
        );
        // A process that VANISHED mid-read (no cwd) held nothing, and is skipped
        // without being counted as a denial.
        let gone = mk_proc_root(tmp.path(), "gone");
        let d = plant_proc(&gone, 302, &["x"], &[], Path::new("/"), &[]);
        fs::remove_file(d.join("cwd")).unwrap();
        let snap = ProcSnapshot::read(&gone);
        assert!(
            snap.listed && snap.procs.is_empty() && snap.opaque == 0,
            "{snap:?}"
        );
    }

    /// Phase 4, test 5 — a non-cargo directory in scratch is never a candidate,
    /// at the enumerator AND at the classifier, on a family root.
    #[test]
    fn a_non_cargo_directory_in_scratch_is_never_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, scratch) = mk_scratchpad_tree(tmp.path());
        let wt = scratch.join("feat-unpushed");
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::write(wt.join(".git"), b"gitdir: /repo/.git/worktrees/feat\n").unwrap();
        let notes = scratch.join("notes");
        fs::create_dir_all(&notes).unwrap();
        let lookalike = scratch.join("target-lookalike");
        fs::create_dir_all(lookalike.join("debug")).unwrap();
        mk_target_root(&scratch.join("cargo-target"));

        let found = enumerate_family_within(&family, SCRATCHPAD_MAX_WALK_DEPTH).candidates;
        let names: Vec<String> = found
            .iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["cargo-target".to_string()], "{names:?}");
        assert_eq!(found[0].family_root.as_deref(), Some(family.as_path()));

        let opts = idle_opts(tmp.path(), Duration::ZERO);
        for p in [&wt, &notes, &lookalike] {
            let c = Candidate {
                path: p.clone(),
                class: TargetClass::SiblingNonGit,
                repo_root: None,
                enclosing_git_unreadable: false,
                owner: expected_local_owner(),
                family_root: Some(family.clone()),
            };
            assert_eq!(
                classify_candidate(&c, &opts),
                Err(SkipReason::UnrecognizedLayout),
                "{}",
                p.display()
            );
        }
        assert!(wt.exists() && notes.exists() && lookalike.exists());
    }

    /// Phase 4, test 6 — a symlink in a family root pointing at a checkout's
    /// target is never a candidate, and that target SURVIVES an armed reap. The
    /// reaper's walk already refuses links; this pins it for the new roots, whose
    /// trees are known to hold such links (`scratch/fbsroot/qontinui-runner` is
    /// one into the primary checkout, 89.6 GB).
    #[cfg(unix)]
    #[test]
    fn a_symlink_in_a_family_root_to_a_checkouts_target_is_never_reaped() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, scratch) = mk_scratchpad_tree(tmp.path());
        // A live checkout, elsewhere, with a real cargo target of its own.
        let checkout = tmp.path().join("live-checkout");
        let live_target = checkout.join("src-tauri/target-live");
        mk_target_root(&live_target);
        fs::write(
            live_target.join("debug/deps/precious.rlib"),
            b"build output",
        )
        .unwrap();
        age_target(&live_target, 3 * 86_400);
        // Links to it from inside the session scratch: to the checkout, and
        // directly to the target.
        std::os::unix::fs::symlink(&checkout, scratch.join("checkout-link")).unwrap();
        std::os::unix::fs::symlink(&live_target, scratch.join("target-link")).unwrap();
        // A genuine target beside them, so the armed run below is not vacuous.
        let mine = scratch.join("tgt");
        mk_target_root(&mine);
        age_target(&mine, 3 * 86_400);

        let found = enumerate_family_within(&family, SCRATCHPAD_MAX_WALK_DEPTH);
        assert_eq!(
            found.candidates.len(),
            1,
            "only the real target: {:?}",
            found.candidates
        );
        assert!(
            found.reparse_dirs_skipped >= 2,
            "the links were counted, not followed"
        );

        let s = run_cycle_over_roots_with(
            &[ReapRoot::at(ReapRootKind::HarnessScratchpad, &family).with_armed(true)],
            &idle_opts(tmp.path(), DAY),
        );
        assert_eq!(
            (s.candidates, s.reaped),
            (1, 1),
            "{}",
            render_cycle_line(&s)
        );
        assert!(!mine.exists(), "the genuine target was reaped");
        assert!(
            live_target.join("debug/deps/precious.rlib").exists(),
            "the live checkout's build output is untouched"
        );
        assert!(
            fs::symlink_metadata(scratch.join("target-link")).is_ok()
                && fs::symlink_metadata(scratch.join("checkout-link")).is_ok(),
            "the links themselves were not removed either"
        );
    }

    /// A candidate list is a snapshot and never a delete list: the armed path
    /// RE-READS before removing, so a session that resumed after the
    /// classification keeps its target.
    #[cfg(unix)]
    #[test]
    fn the_reread_before_removal_refuses_what_changed_since_classification() {
        let (tmp, family, target) = idle_scratch_target();
        let c = only_candidate(&family);
        let home = mk_account_home(tmp.path(), "home", SESS, 3 * 86_400);
        let proc_root = mk_proc_root(tmp.path(), "proc");
        let opts = scratch_opts(ScratchEnv::fixed(proc_root.clone(), Some(vec![home])));

        // Classified while idle: the process table is read once and memoised.
        assert_eq!(classify_candidate(&c, &opts), Ok(()));
        // The session RESUMES: a process now names it.
        let resumed = plant_proc(
            &proc_root,
            777,
            &["claude", "--resume", SESS],
            &[],
            Path::new("/"),
            &[],
        );
        assert_eq!(
            classify_candidate(&c, &opts),
            Ok(()),
            "the memo still holds the PAST — which is exactly why a removal may not trust it"
        );
        assert_eq!(
            reread_scratch_candidate(&c, &opts),
            Err(SkipReason::SessionLive),
            "the re-read goes to the machine, not to the memo"
        );

        // An armed cycle over the stale memo removes nothing and says why.
        let roots = [ReapRoot::at(ReapRootKind::HarnessScratchpad, &family).with_armed(true)];
        let s = run_cycle_over_roots_with(&roots, &opts);
        assert_eq!((s.candidates, s.reaped, s.reread_refused), (1, 0, 1));
        assert!(target.exists(), "the resumed session's target survived");
        assert!(
            render_cycle_line(&s).contains("reread_refused=1"),
            "{}",
            render_cycle_line(&s)
        );

        // Control: the session ends, and the same armed cycle reaps — so the
        // refusal above was the re-read and nothing else.
        fs::remove_dir_all(resumed).unwrap();
        let s = run_cycle_over_roots_with(&roots, &opts);
        assert_eq!((s.candidates, s.reaped, s.reread_refused), (1, 1, 0));
        assert!(!target.exists());
    }

    /// Phase 4, test 7 — the pressure read agrees with `f_bavail`, which is what
    /// coord's budget uses. With root-reserved blocks, the old `f_bfree` reading
    /// sits UNDER the 90 % line while the disk is already over it.
    #[test]
    fn the_pressure_read_over_root_reserved_blocks_agrees_with_f_bavail() {
        // 1000 blocks of 4 KiB; 150 free by `f_bfree`, of which 100 are reserved
        // for root, so only 50 are available to a process like this one.
        let (blocks, bfree, bavail) = (1_000u64, 150u64, 50u64);
        let c = capacity_from_statvfs(4096, blocks, bavail, 400, 100);
        assert_eq!(c.bytes_free, bavail * 4096, "free is what can be allocated");
        assert!((c.bytes_pct_used.unwrap() - 95.0).abs() < 1e-9);
        assert_eq!(c.bytes_axis(90.0), PressureAxis::Pressured);
        assert!(c.pressured(90.0));
        // Non-vacuous: the SAME disk read by `f_bfree` is the divergence this
        // fixes — 85 % used, under the line, "healthy".
        assert_eq!(
            capacity_from_statvfs(4096, blocks, bfree, 400, 100).bytes_axis(90.0),
            PressureAxis::Ok
        );
        // The line is coord's reserve: pressure begins where `avail` falls to 10 %.
        assert_eq!(
            capacity_from_statvfs(4096, 1_000, 100, 400, 100).bytes_axis(90.0),
            PressureAxis::Pressured
        );
        assert_eq!(
            capacity_from_statvfs(4096, 1_000, 101, 400, 100).bytes_axis(90.0),
            PressureAxis::Ok
        );
    }

    /// The wiring, not just the arithmetic: `fs_capacity` hands `capacity_from_statvfs`
    /// the real `f_bavail`. Effective wherever the filesystem reserves blocks for
    /// root (merytshost: ~100 GB); on one that reserves none `f_bfree` and
    /// `f_bavail` coincide and this asserts only what is true of both.
    #[cfg(unix)]
    #[test]
    fn the_live_capacity_read_takes_f_bavail_not_f_bfree() {
        let dir = tempfile::tempdir().unwrap();
        let mut raw = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        let path = std::ffi::CString::new(dir.path().to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path and a correctly-sized out-buffer; the
        // result is read only after the call reports success.
        let rc = unsafe { libc::statvfs(path.as_ptr(), raw.as_mut_ptr()) };
        assert_eq!(rc, 0);
        // SAFETY: `statvfs` returned 0, so the buffer is initialised.
        let raw = unsafe { raw.assume_init() };
        let frsize = if raw.f_frsize != 0 {
            raw.f_frsize as u64
        } else {
            raw.f_bsize as u64
        };
        let avail = raw.f_bavail as u64 * frsize;
        let bfree = raw.f_bfree as u64 * frsize;
        let got = fs_capacity(dir.path()).bytes_free;
        // Free space moves while a test runs (concurrent builds), so compare with
        // slack — a hundred times smaller than the root-reserve it must tell apart.
        let slack = 256 << 20;
        assert!(
            got.abs_diff(avail) < slack,
            "bytes_free {got} must be f_bavail {avail}, not f_bfree {bfree}"
        );
        if bfree.saturating_sub(avail) > 4 * slack {
            assert!(
                got.abs_diff(bfree) > slack,
                "bytes_free {got} agrees with f_bfree {bfree} across {} reserved bytes",
                bfree - avail
            );
        }
    }

    /// One workspace target and one scratch target, both idle, both reapable.
    /// Returns `(tmp, inputs, workspace_target, scratch_target)`.
    #[cfg(unix)]
    fn two_population_fixture() -> (tempfile::TempDir, ReapRootInputs, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let uid = invoking_uid();
        let ws = tmp.path().join("workspace");
        let ws_target = ws.join("target-wt-a");
        mk_target_root(&ws_target);
        age_target(&ws_target, 3 * 86_400);
        let qdir = tmp.path().join("qdir");
        let scratch_target = qdir.join(format!(
            "scratch/.claude-a/claude-{uid}/{PROJ}/{SESS}/scratchpad/tgt"
        ));
        mk_target_root(&scratch_target);
        age_target(&scratch_target, 3 * 86_400);
        let inputs = ReapRootInputs {
            workspace_root: Some(ws),
            tmp: tmp.path().join("empty-tmp"),
            qontinui_dir: Some(qdir),
            uid: Some(uid),
            ..ReapRootInputs::default()
        };
        (tmp, inputs, ws_target, scratch_target)
    }

    /// Phase 5 gate — the two switches are PINNED APART: a scratch candidate
    /// reaps with the workspace flag unset, and a workspace candidate does not.
    #[cfg(unix)]
    #[test]
    fn the_two_arming_switches_are_pinned_apart() {
        // Both switches unset: scratch ships ON, the workspace ships DARK.
        let (tmp, inputs, ws_target, scratch_target) = two_population_fixture();
        assert_eq!(inputs.workspace_reap_env, None);
        assert_eq!(inputs.scratch_reap_env, None);
        let roots = reap_roots_from(&inputs);
        let armed: Vec<(ReapRootKind, bool)> = roots.iter().map(|r| (r.kind, r.armed)).collect();
        assert_eq!(armed.first(), Some(&(ReapRootKind::Workspace, false)));
        assert!(
            armed[1..]
                .iter()
                .all(|(k, a)| *k == ReapRootKind::HarnessScratchpad && *a),
            "every scratch root is armed by default: {armed:?}"
        );
        let s = run_cycle_over_roots_with(&roots, &idle_opts_zero(tmp.path()));
        assert_eq!(s.reaped, 1, "{}", render_cycle_line(&s));
        assert!(!scratch_target.exists(), "the scratch candidate reaped");
        assert!(ws_target.exists(), "the workspace candidate did NOT");
        assert_eq!(s.candidates, 2, "both were candidates; only one was armed");

        // The reverse: workspace flag set, scratch switch `0`.
        let (tmp, mut inputs, ws_target, scratch_target) = two_population_fixture();
        inputs.workspace_reap_env = Some("1".to_string());
        inputs.scratch_reap_env = Some("0".to_string());
        let s = run_cycle_over_roots_with(&reap_roots_from(&inputs), &idle_opts_zero(tmp.path()));
        assert_eq!(s.reaped, 1);
        assert!(
            !ws_target.exists(),
            "the workspace flag armed the workspace"
        );
        assert!(
            scratch_target.exists(),
            "exactly `0` disarmed the scratch arm"
        );

        // Neither flag arms the other's population, and both together arm both.
        let (tmp, mut inputs, ws_target, scratch_target) = two_population_fixture();
        inputs.workspace_reap_env = Some("true".to_string());
        let s = run_cycle_over_roots_with(&reap_roots_from(&inputs), &idle_opts_zero(tmp.path()));
        assert_eq!(s.reaped, 2);
        assert!(!ws_target.exists() && !scratch_target.exists());
    }

    /// Phase 5 — exactly `0` disables the scratch arm, and nothing else does.
    /// A mistyped switch must not silently disarm a default-on capability.
    #[test]
    fn exactly_zero_disables_the_scratch_arm_and_nothing_else_does() {
        assert!(!scratch_reap_armed_from(Some("0")));
        for raw in [
            None,
            Some(""),
            Some("1"),
            Some("true"),
            Some("false"),
            Some("off"),
            Some(" 0"),
            Some("00"),
        ] {
            assert!(
                scratch_reap_armed_from(raw),
                "{raw:?} must leave the scratch arm ON"
            );
        }
        // The workspace switch is unchanged: dark unless truthy.
        for raw in [None, Some(""), Some("0"), Some("yes"), Some("True")] {
            assert!(!workspace_reap_armed_from(raw), "{raw:?}");
        }
        for raw in [Some("1"), Some("true"), Some("TRUE")] {
            assert!(workspace_reap_armed_from(raw), "{raw:?}");
        }
        // Each kind reads only its own.
        assert!(root_armed(ReapRootKind::HarnessScratchpad, Some("1"), None));
        assert!(!root_armed(
            ReapRootKind::HarnessScratchpad,
            Some("1"),
            Some("0")
        ));
        assert!(!root_armed(ReapRootKind::Workspace, None, None));
        assert!(!root_armed(ReapRootKind::Workspace, None, Some("1")));
        assert_eq!(
            SCRATCH_REAP_ENABLED_ENV,
            "COORD_ORPHAN_TARGET_SCRATCH_REAP_ENABLED"
        );
    }

    /// Each root's token carries ITS arming, and the cycle-level `armed=` is the
    /// OR over walked roots — so the line a shadow window is read from says which
    /// population could have removed anything.
    #[cfg(unix)]
    #[test]
    fn each_roots_state_token_carries_its_arming() {
        let (tmp, inputs, _ws, _scratch) = two_population_fixture();
        let s = run_cycle_over_roots_with(&reap_roots_from(&inputs), &idle_opts_zero(tmp.path()));
        let line = render_cycle_line(&s);
        let ws = s
            .roots
            .iter()
            .find(|r| r.kind == ReapRootKind::Workspace)
            .unwrap();
        assert!(ws.render().contains("armed=false"), "{}", ws.render());
        let armed_scratch: Vec<String> = s
            .roots
            .iter()
            .filter(|r| r.kind == ReapRootKind::HarnessScratchpad && r.completeness.is_some())
            .map(RootReport::render)
            .collect();
        assert!(!armed_scratch.is_empty());
        assert!(
            armed_scratch.iter().all(|t| t.contains("armed=true")),
            "{armed_scratch:?}"
        );
        assert!(line.contains(" armed=true "), "cycle-level OR: {line}");
        // A cycle over only the dark workspace root is not armed.
        let dark = run_cycle_over_roots(
            &[ReapRoot::at(
                ReapRootKind::Workspace,
                inputs.workspace_root.as_deref().unwrap(),
            )],
            Duration::ZERO,
        );
        assert!(render_cycle_line(&dark).contains(" armed=false "));
    }

    /// Zero-grace idle options over [`idle_env`], for the cycle tests whose
    /// workspace candidate is brand new.
    #[cfg(unix)]
    fn idle_opts_zero(base: &Path) -> ClassifyOptions {
        idle_opts(base, Duration::ZERO)
    }

    /// OPERATOR PROBE — `#[ignore]`d, and **DRY-RUN BY CONSTRUCTION**: it reads
    /// this machine's real scratch family and prints what a cycle WOULD reap,
    /// beside the transcript, process and lock reads that cleared each target. It
    /// forces every root unarmed and never calls a removal, whatever the
    /// environment says.
    ///
    /// It exists because the periodic reaper is gated to the shared-root-owning
    /// instance, so a supervisor temp runner never runs a cycle and cannot
    /// produce the shadow-window evidence plan Phase 5 asks for. Run it with
    /// `cargo-guard.sh test -- --ignored --nocapture real_scratch_family_dry_run_probe`;
    /// `QONTINUI_REAPER_PROBE_CYCLES` sets the cycle count (default 3).
    #[test]
    #[ignore = "reads this machine's real scratch family; run on purpose, dry-run only"]
    fn real_scratch_family_dry_run_probe() {
        let cycles: usize = std::env::var("QONTINUI_REAPER_PROBE_CYCLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        // `ambient::qontinui_dir()` deflects to an empty directory inside a test
        // binary (the ambient canary), which would make this probe a census of
        // nothing — so resolve the real `~/.qontinui` by the pure rule it wraps.
        let inputs = ReapRootInputs {
            qontinui_dir: qontinui_runner_lib::ambient::qontinui_dir_from(
                std::env::var_os(qontinui_runner_lib::ambient::QONTINUI_HOME_ENV),
                dirs::home_dir(),
            ),
            ..ReapRootInputs::ambient()
        };
        let roots: Vec<ReapRoot> = reap_roots_from(&inputs)
            .into_iter()
            .filter(|r| r.kind == ReapRootKind::HarnessScratchpad)
            .map(|r| r.with_armed(false)) // the one line that makes this a probe
            .collect();
        assert!(roots.iter().all(|r| !r.armed));
        eprintln!("PROBE roots: {}", roots.len());
        for cycle in 1..=cycles {
            let opts = ClassifyOptions::from_env();
            let started = std::time::Instant::now();
            let summary = run_cycle_over_roots_with(&roots, &opts);
            eprintln!(
                "PROBE cycle {cycle}/{cycles} ({:?}): {}",
                started.elapsed(),
                render_cycle_line(&summary)
            );
            let mut would_reap: Vec<(PathBuf, String)> = Vec::new();
            let mut by_reason: std::collections::BTreeMap<&'static str, usize> = Default::default();
            for r in roots.iter().filter(|r| r.state.is_walkable()) {
                let Some(path) = r.path.as_deref() else {
                    continue;
                };
                for c in enumerate_family_within(path, r.max_walk_depth).candidates {
                    match classify_candidate(&c, &opts) {
                        Ok(()) => {
                            let s = scratch_session_of(path, &c.path)
                                .expect("a cleared scratch candidate has a session");
                            let homes = opts.scratch.homes().unwrap_or(&[]);
                            let age = match newest_transcript_age(homes, &s.project, &s.uuid) {
                                TranscriptRead::Idle(a) => format!("{}h", a.as_secs() / 3600),
                                TranscriptRead::Unknown => "UNKNOWN".to_string(),
                            };
                            let procs = opts.scratch.procs();
                            let holder = std::fs::canonicalize(&c.path)
                                .map(|t| format!("{:?}", procs.holds_target(&t)))
                                .unwrap_or_else(|e| format!("canonicalize failed: {e}"));
                            let named = std::fs::canonicalize(&s.dir)
                                .map(|d| procs.names_session(&s, &d))
                                .unwrap_or(true);
                            would_reap.push((
                                c.path.clone(),
                                format!("session={} transcript_idle={age} holder={holder} session_named_by_a_process={named}", s.uuid),
                            ));
                        }
                        Err(reason) => *by_reason.entry(reason.token()).or_default() += 1,
                    }
                }
            }
            eprintln!("PROBE cycle {cycle} refusals by reason: {by_reason:?}");
            eprintln!("PROBE cycle {cycle} would-reap set: {}", would_reap.len());
            for (p, why) in &would_reap {
                eprintln!("PROBE   would-reap {} [{why}]", p.display());
            }
            if cycle < cycles {
                std::thread::sleep(Duration::from_secs(
                    std::env::var("QONTINUI_REAPER_PROBE_GAP_SECS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1),
                ));
            }
        }
    }

    // ---- Review follow-ups: the process-table read, the re-read, the struct reader.

    /// The wiring, on a hand-built struct, so it is checked on a filesystem that
    /// reserves nothing for root too: the byte axis is `f_bavail`.
    #[cfg(unix)]
    #[test]
    fn the_statvfs_struct_reader_feeds_the_byte_axis_f_bavail() {
        // SAFETY: `libc::statvfs` is plain old data; all-zero is a valid value.
        let mut raw: libc::statvfs = unsafe { std::mem::zeroed() };
        raw.f_frsize = 4096;
        raw.f_bsize = 512;
        raw.f_blocks = 1000;
        raw.f_bfree = 150; // includes 100 reserved for root
        raw.f_bavail = 50;
        raw.f_files = 400;
        raw.f_ffree = 100;
        let c = capacity_from_statvfs_struct(&raw);
        assert_eq!(c.bytes_total, 1000 * 4096);
        assert_eq!(c.bytes_free, 50 * 4096, "available, not merely free");
        assert!((c.bytes_pct_used.unwrap() - 95.0).abs() < 1e-9);
        assert_eq!(c.bytes_axis(90.0), PressureAxis::Pressured);
        // `f_frsize` of zero falls back to `f_bsize`.
        raw.f_frsize = 0;
        assert_eq!(capacity_from_statvfs_struct(&raw).bytes_free, 50 * 512);
    }

    #[test]
    fn a_scratch_switch_that_reads_like_an_off_but_is_not_zero_is_warned_about() {
        assert_eq!(scratch_switch_warning(None), None);
        assert_eq!(scratch_switch_warning(Some("0")), None);
        assert_eq!(scratch_switch_warning(Some("1")), None);
        for raw in ["false", "off", "no", "0 ", ""] {
            let w = scratch_switch_warning(Some(raw)).expect(raw);
            assert!(
                w.contains(SCRATCH_REAP_ENABLED_ENV) && w.contains("stays ON"),
                "{w}"
            );
        }
    }

    /// A link to a directory where an account directory should be is counted,
    /// not skipped silently: a `scratch/<account>` that is a link onto a bigger
    /// disk must not read as an empty family with nothing incomplete.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_account_directory_is_counted_not_silently_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("bigger-disk/.claude-x");
        fs::create_dir_all(elsewhere.join("claude-4242")).unwrap();
        fs::create_dir_all(tmp.path().join("scratch")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, tmp.path().join("scratch/.claude-x")).unwrap();
        let fam = scratch_family_under(tmp.path(), 4242);
        assert!(
            fam.members.is_empty(),
            "a link is never descended into: {fam:?}"
        );
        assert_eq!(fam.links_skipped, 1, "…but it is counted: {fam:?}");
    }

    /// A process running a binary built into the target holds nothing else in
    /// it, and one running a binary from the session's scratch is the session.
    #[cfg(unix)]
    #[test]
    fn a_process_running_a_binary_from_the_target_or_the_session_stays_out() {
        let (tmp, family, target) = idle_scratch_target();
        let c = only_candidate(&family);
        let home = mk_account_home(tmp.path(), "home", SESS, 3 * 86_400);
        let sess_dir = canon(&family.join(PROJ).join(SESS));
        let verdict = |name: &str, exe: Option<&Path>| {
            let root = mk_proc_root(tmp.path(), name);
            let dir = plant_proc(&root, 401, &["./app"], &[], Path::new("/"), &[]);
            if let Some(exe) = exe {
                fs::remove_file(dir.join("exe")).unwrap();
                std::os::unix::fs::symlink(exe, dir.join("exe")).unwrap();
            }
            classify_candidate(
                &c,
                &scratch_opts(ScratchEnv::fixed(root, Some(vec![home.clone()]))),
            )
        };
        assert_eq!(
            verdict("e0", None),
            Ok(()),
            "control: a binary from elsewhere"
        );
        assert_eq!(
            verdict("e1", Some(&canon(&target).join("debug/app"))),
            Err(SkipReason::HeldByProcess),
            "a test binary or dev server built into the target"
        );
        assert_eq!(
            verdict("e2", Some(&sess_dir.join("scratchpad/lane3/bin/d-run"))),
            Err(SkipReason::SessionLive),
            "a binary running out of the session's own scratch"
        );
    }

    /// A read that failed for an UNEXPECTED reason is a missing reading, not a
    /// process that held nothing — and a process that vanished is still skipped.
    #[cfg(unix)]
    #[test]
    fn an_unexpected_read_error_makes_the_table_unknown_and_a_vanished_process_does_not() {
        let (tmp, family, _target) = idle_scratch_target();
        let c = only_candidate(&family);
        let home = mk_account_home(tmp.path(), "home", SESS, 3 * 86_400);
        let root = mk_proc_root(tmp.path(), "proc");
        let dir = plant_proc(&root, 501, &["x"], &[], Path::new("/"), &[]);
        let opts = |env: ScratchEnv| scratch_opts(env);
        assert_eq!(
            classify_candidate(
                &c,
                &opts(ScratchEnv::fixed(root.clone(), Some(vec![home.clone()])))
            ),
            Ok(()),
            "control"
        );
        // `cwd` that is a regular file makes `readlink` fail with EINVAL: not a
        // vanished process and not a refusal.
        fs::remove_file(dir.join("cwd")).unwrap();
        fs::write(dir.join("cwd"), b"not a link").unwrap();
        assert!(!ProcSnapshot::read(&root).listed);
        assert_eq!(
            classify_candidate(&c, &opts(ScratchEnv::fixed(root, Some(vec![home])))),
            Err(SkipReason::LockStateUnknown)
        );
        // The open-fd ceiling is a ceiling on the READ, not a quiet prefix.
        let fdroot = mk_proc_root(tmp.path(), "fds");
        let fdproc = plant_proc(
            &fdroot,
            502,
            &["x"],
            &[],
            Path::new("/"),
            &[Path::new("/a"), Path::new("/b")],
        );
        assert!(matches!(read_one_proc(&fdproc, &mut 1), ProcRead::Unknown));
        assert!(matches!(
            read_one_proc(&fdproc, &mut 2),
            ProcRead::Entry { .. }
        ));
    }

    /// The re-read checks that the target still resolves to where the walk found
    /// it: a component swapped for a link since then would make the removal
    /// follow the link.
    #[cfg(unix)]
    #[test]
    fn a_link_swapped_into_the_path_after_the_walk_refuses_the_removal() {
        let (tmp, family, _target) = idle_scratch_target();
        let c = only_candidate(&family);
        let opts = idle_opts(tmp.path(), DAY);
        assert_eq!(reread_scratch_candidate(&c, &opts), Ok(()), "control");
        let scratchpad = family.join(PROJ).join(SESS).join("scratchpad");
        let moved = scratchpad.with_file_name("scratchpad-real");
        fs::rename(&scratchpad, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &scratchpad).unwrap();
        assert_eq!(
            reread_scratch_candidate(&c, &opts),
            Err(SkipReason::Reparse),
            "the same lexical path now resolves somewhere else"
        );
    }
}
