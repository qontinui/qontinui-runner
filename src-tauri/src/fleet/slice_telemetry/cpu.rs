//! The CPU half of the slice telemetry: per-slice `cpu.stat` deltas, and the
//! split of the host's CPU into session work, CI and the rest (plan
//! `2026-11-05-one-free-resource-model-places-sessions-builds-and-ci`, Phase 2;
//! its D3 names the classes).
//!
//! # What is read
//!
//! cgroup v2 `cpu.stat` `usage_usec`: cumulative CPU time in microseconds. It
//! is hierarchical (a cgroup's counter includes every descendant, and the time
//! of a child that has exited stays in its parent), and the file exists in
//! every cgroup whether or not the `cpu` controller is enabled there, so no
//! delegation is needed. Each sample reads it for:
//!
//! * each watched slice ([`WATCHED_SLICES`]), from the directory its memory
//!   entry was read from ([`read_slices`](super::read_slices)), else from its
//!   first candidate that is a cgroup;
//! * the runner's own unit cgroup, from the `0::` line of `/proc/self/cgroup`
//!   (today `app.slice/qontinui-runner.service` under a user manager);
//! * each `qontinui-pty-holder-*.scope` directly under the runner's user
//!   manager or in one of its direct `*.slice` children (`app.slice`, where
//!   they sit today). The manager is the one on the runner's cgroup path, else
//!   the one of the user the runner runs as (the `Uid:` line of
//!   `/proc/self/status`), never another user's. With neither, no holder is
//!   looked for, and neither the holder keys nor the session total is
//!   published. Each holder pane, where `claude` runs, gets its own
//!   `systemd-run --user --scope` with no `--slice`, so it sits beside the
//!   runner's unit, not in it;
//! * each CI dispatch cgroup (`dispatch-*`) that `ci_node` made to contain a
//!   CI job: in the runner's own cgroup or in its unit, wherever
//!   [`candidate_parents`] would have put it, and in the directory an operator
//!   named in `QONTINUI_CI_CGROUP_ROOT`, which `ci_node` uses instead, when
//!   that directory is under the cgroup root and named without `.` or `..`;
//! * `ci.slice` and each `ci-*.slice` child of it (`ci-runners.slice`, and
//!   `ci-winvm.slice` on nawa);
//! * `user.slice`, `system.slice`, every other direct child of the root, and
//!   the root cgroup.
//!
//! # Classes, each cgroup counted once
//!
//! Session work is the runner unit, the holder scopes, and `agent-builds.slice`
//! and `qontinui-runner.slice` wherever they are installed outside every user
//! manager or under the runner's own. One under another user's manager is that
//! user's work, even when it is the one the slice's memory entry describes. The
//! runner unit is the last `.service` or `.scope` on the runner's cgroup path,
//! so a runner started from a login shell charges its whole login
//! `session-N.scope` to session work. A read that finds no unit for the runner
//! publishes no session total. CI is `ci.slice`, `ci-runners.slice` wherever it
//! resolves, and the dispatch cgroups. Each named cgroup is charged its own
//! time minus that of the differently-classed named cgroups nested in it, so a
//! dispatch cgroup inside the runner's unit is CI and not session work, and a
//! cgroup inside a same-class one (the runner unit inside
//! `qontinui-runner.slice`, `ci-runners.slice` inside `ci.slice`) is not added
//! twice. A remainder is its slice's time minus each outermost named cgroup
//! inside it, each subtracted exactly once; `unattributed` is the host's time
//! minus every outermost named cgroup.
//!
//! A CI job that `ci_node` could not contain (the unit has no delegated
//! cgroup, as with `Delegate=no`) runs in the runner's own cgroup, where no
//! cgroup separates it from session work. So does each step of a contained job
//! that `ci_node` could not move into the job's cgroup (the kernel refuses the
//! move, as it can when `QONTINUI_CI_CGROUP_ROOT` names a directory outside the
//! runner's user manager), and such a job counts from its first such step on.
//! So does what `ci_node` runs itself for a dispatch, contained or not: the
//! checkout, the canonical-configuration convergence and the provisioning
//! before it settles the job's containment (and all of a dispatch refused or
//! failed before then), and the report and the cleanup after the job's steps.
//! While any of these runs, and for the read after it ends, the session and CI
//! totals are absent (`ci_node` counts them: [`DispatchCounts`]); on a unit
//! with `Delegate=no` that is every dispatch from its start to its end. What a
//! step used between its start and `ci_node` finding it unplaced, a moment
//! later, can be published as session work. So is the runner's own handling
//! of a contained job's steps while they run (reading their output and
//! posting it). A process left running by a job that was not contained, or
//! by a step that was not placed, stays in the runner's cgroup after the
//! dispatch ends, and is session work from then on.
//!
//! A dispatch's service containers run where their container runtime puts
//! them, outside `ci_node`'s cgroups, so their time is counted where that is,
//! not as the dispatch's: under docker's defaults (`system.slice`, or a
//! `docker` cgroup under the root, by its cgroup driver) it is in the
//! `system.slice` remainder or in `unattributed_usec`, not in the CI total.
//!
//! A dispatch cgroup this read could not list is withheld like one no read
//! listed (below). When `QONTINUI_CI_CGROUP_ROOT`, where `ci_node` makes every
//! dispatch cgroup when it is set, names a directory outside the cgroup root or
//! names it through `..`, or, with no such directory named, the runner's own
//! cgroup, beside which `ci_node` makes them, is unknown, one could be
//! anywhere: no class total, remainder or `unattributed_usec` is published.
//! When a directory they are made in could not be listed, what a dispatch
//! cgroup there would be part of is absent.
//!
//! # A delta, or nothing
//!
//! The first read of a root publishes no CPU at all: there is nothing to
//! subtract from, and a `0` would read as idle. After that, a cgroup's delta
//! exists only when it was read both times as the same directory (its inode)
//! and its counter did not go backwards. A cgroup that appeared, was recreated
//! or was reset therefore has no delta that read, never a negative one and
//! never its whole lifetime, and a value that needs it is absent rather than
//! partial. Holder scopes are the exception, because panes come and go: a new
//! one is left out for that read, and its time falls into the remainder of the
//! slice it runs in, which is where it was spent. So does the last stretch of
//! one that ended since the previous read, and `pty_holder_scopes_ended`
//! counts those, as `pty_holder_scopes_measured` below
//! `pty_holder_scopes_seen` counts the new ones. Any other named cgroup that
//! disappeared since the previous read withholds every value it was part of,
//! for that read, unless it sat inside a named cgroup of its own class, whose
//! time still holds it. So a dispatch cgroup that starts or ends leaves the
//! session and CI totals absent for that read. A dispatch that started and
//! ended between two reads is in neither listing, and neither is one made
//! after a read listed the cgroups but before it read their counters.
//! `ci_node` counts the dispatches it starts ([`DispatchCounts`]); each read
//! takes that count before it lists the cgroups and again after it reads the
//! counters, and a read whose second count differs from the previous read's
//! first withholds what such a dispatch's time would be part of, in each place
//! it could have been made.
//!
//! A directory that could not be listed is not taken for an empty one (one
//! that does not exist is): the cgroups it would hold are unplaced, and every
//! value that needs them is absent. That includes `user.slice` and its
//! `user-N.slice`s, the way to every user manager a CI slice can be looked for
//! under: when one of them could not be listed and a CI slice was not found
//! elsewhere, the CI slice may be in a manager there, and the CI total, the
//! `user.slice` remainder and `unattributed_usec` are withheld.
//!
//! A read more than [`MAX_INTERVAL_USEC`] after the previous one (four 30 s
//! publish periods, the age at which the plan's D2 calls a sample stale)
//! publishes no CPU either, and becomes the next read's baseline. A delta over
//! such a gap, after a stretch in which the publisher did not run, would be an
//! average over time the sample's PSI readings do not cover.
//!
//! # Wire shape
//!
//! Added to `cgroup_slices` beside the memory keys, which keep their names:
//!
//! ```text
//! "<watched slice>": {..., "cpu": {"usage_delta_usec": N, "interval_usec": N}}
//! "cpu_attribution": {"interval_usec": N, "host_usec": N,
//!     "session_usec": N, "ci_usec": N, "unattributed_usec": N,
//!     "user_slice_remainder_usec": N, "system_slice_remainder_usec": N,
//!     "runner_unit": "<cgroup path>", "runner_unit_usec": N,
//!     "pty_holder_scopes_usec": N, "pty_holder_scopes_measured": n,
//!     "pty_holder_scopes_seen": n, "pty_holder_scopes_ended": n,
//!     "ci_children_usec": {"ci-runners.slice": N, ...},
//!     "ci_children_unmeasured": n, "ci_children_omitted": n,
//!     "ci_children_unlisted": true, "withheld": ["<reason>", ...]}
//! ```
//!
//! `interval_usec` is the time between this read and the previous one, in
//! microseconds by std's `Instant`. On Linux std reads `CLOCK_MONOTONIC` for
//! it today, which does not count time the system is suspended
//! (clock_gettime(2)), so neither does the interval, as none of the CPU values
//! does; std's documentation does not promise that clock.
//! Every other `*_usec` value is CPU time used in that interval,
//! in microseconds summed over CPUs: two cores busy for 30 s is 60,000,000. A
//! value that could not be measured is absent. A time less the times inside it
//! is published as 0 when read skew puts it below 0 by no more than the larger
//! of 2% of that time and 0.1 CPU-second ([`exclusive`]). A class with no
//! cgroup on the machine publishes no total. `runner_unit_usec` is the unit's
//! own counter, with any dispatch cgroup in it and any CI work run in it
//! (above) included. It is not session work, least of all while `withheld`
//! names `dispatch_uncontained`: session work is `session_usec`.
//! `pty_holder_scopes_usec` sums the holders measured this read, and is absent
//! when holders were seen but none could be measured. `pty_holder_scopes_seen`
//! and `_measured` are published whenever the holders were all found, 0
//! included, and `pty_holder_scopes_ended`, the holders seen at the previous
//! read that are gone, whenever both reads found them. Only `ci-*.slice`
//! children of `ci.slice` are named: the time of any other cgroup in it, and
//! of a process in it directly, is in `ci_usec` beyond the children's sum.
//! Read skew can also put that sum a little above `ci_usec`, since a read
//! takes each child's counter a moment before its parent's.
//! `ci_children_unmeasured` counts the named children with no delta this read
//! (one that just appeared), so a reader can tell that `ci_children_usec` is
//! partial; `ci_children_omitted` counts the `ci-*.slice` children not named.
//! Both are published whenever `ci.slice` was listed, 0 included. A `ci.slice`
//! that could not be listed publishes `ci_children_unlisted` instead: its own
//! time is still the CI total.
//!
//! `withheld` lists why this read withheld a class total, a remainder or
//! `unattributed_usec`, each reason once and in this order: `cgroup_gone`, a
//! named cgroup gone since the previous read; `user_slice_unlisted`, a CI
//! slice not found while `user.slice`, or a `user-N.slice` in it, could not be
//! listed (a CI slice the previous read named there is under this reason, not
//! `cgroup_gone`); `dispatch_started`, a dispatch
//! `ci_node` counted since the previous read counted; `dispatch_uncontained`,
//! CI work counted in the runner's own cgroup (above) during the interval, or
//! some that may have been; `dispatch_dir_unlisted`, a directory dispatch
//! cgroups are made in that could not be listed (a dispatch cgroup the
//! previous read listed there is under this reason, not `cgroup_gone`);
//! `ci_cgroup_root_not_followed`, a `QONTINUI_CI_CGROUP_ROOT` that is not
//! followed; `own_cgroup_unknown`, the runner's own cgroup unknown. It is
//! published only when one of them withheld something. Those values can also
//! be absent with no reason listed: one that needs a cgroup with no delta this
//! read, such as one that just appeared or was recreated; `session_usec` when
//! the runner's unit or the holder scopes were not all found, which
//! `runner_unit` and the holder keys show; one taken as a time less the times
//! inside it when those exceed it by more than read skew allows
//! ([`exclusive`], which logs it at debug level); and `unattributed_usec` when
//! the root could not be listed in full ([`Placement::host_time`]).
//!
//! `host_usec` is the root cgroup's counter. The kernel derives it from its
//! per-CPU time accounting (`root_cgroup_cputime()` in `kernel/cgroup/rstat.c`,
//! which at v6.12 sums user, nice, system, irq, softirq and steal time), and
//! every other cgroup's counter from the run time of the tasks in it. So the
//! two disagree, by an amount that varies from read to read.
//! Nothing here depends on its size: the host's time is the larger of the two.
//! `unattributed_usec` is the host's time less every outermost named cgroup,
//! the host's time being `host_usec` or, where the root's direct children add
//! up to more, their sum. That keeps it from reading below what the root's
//! children hold outside the named cgroups (both remainders among them), and
//! it also holds whatever the root's counter shows beyond its children, such
//! as the steal time it sums (above) and the time of kernel threads in the
//! root cgroup itself. Holder scopes that were not looked for (no user manager
//! known, or one that could not be listed) are in the `user.slice` remainder
//! and in `unattributed_usec`, which is where their time was spent.
//! `cpu_attribution` names no slice and carries no memory field, so a reader
//! grading slice memory sees an unmeasured entry.
//!
//! Every name is published only when its JSON form, quotes and escapes
//! included, fits [`MAX_PUBLISHED_NAME_BYTES`], and at most
//! [`MAX_CI_CHILDREN`] children are named. That bounds the whole
//! `cgroup_slices` document at 4 top-level keys, about 11 KiB and depth 4 (a
//! scalar counting 1, as coord counts), with no NUL (a cgroup name cannot hold
//! one), inside the caps that coord#3567 gives the column (open, caps as of
//! b461649cb: 16 keys, 16 KiB, depth 8) whatever the machine's cgroups are
//! called.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use serde_json::{json, Map, Value};
use tracing::{debug, warn};

use super::{nested_parent, read, slice_bases, slice_dirs_in, SliceBases, WATCHED_SLICES};
use crate::ci_node::cgroup::{
    candidate_parents, parse_unified_cgroup, DispatchCounts, CGROUP_ROOT_ENV, DISPATCH_DIR_PREFIX,
};

/// The `cgroup_slices` key holding the class split.
pub(super) const ATTRIBUTION_KEY: &str = "cpu_attribution";
/// The key of the CPU object inside each watched slice's entry.
pub(super) const SLICE_CPU_KEY: &str = "cpu";
/// The slices a remainder is published for, with the key it is published under.
const REMAINDERS: [(&str, &str); 2] = [
    ("user.slice", "user_slice_remainder_usec"),
    ("system.slice", "system_slice_remainder_usec"),
];
/// Most `ci-*.slice` children published by name. The rest, and any whose name
/// is too long to publish, are counted in `ci_children_omitted`.
const MAX_CI_CHILDREN: usize = 16;
/// Longest cgroup name or path published as a string, measured as written into
/// the JSON ([`published_len`]): a raw name can grow up to six times when
/// escaped, so its raw length bounds nothing.
const MAX_PUBLISHED_NAME_BYTES: usize = 512;
/// The longest interval a CPU delta is published over: four 30 s publish
/// periods, the age at which the plan's D2 calls a sample stale.
pub(super) const MAX_INTERVAL_USEC: u64 = 120_000_000;
/// The least read skew allowed for, 0.1 CPU-second; see [`exclusive`].
const SKEW_FLOOR_USEC: u64 = 100_000;
/// Roots whose previous read is kept. Production reads one root,
/// `/sys/fs/cgroup`; the bound only stops a caller that cycles through roots
/// from growing the map.
const MAX_ROOTS: usize = 16;

/// The class a named cgroup's CPU is charged to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Class {
    Session,
    Ci,
}

impl Class {
    /// The `cpu_attribution` key of the class total.
    fn key(self) -> &'static str {
        match self {
            Class::Session => "session_usec",
            Class::Ci => "ci_usec",
        }
    }
}

/// Why a read withheld values, published in `withheld` in this order
/// ([`Withheld`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Why {
    /// A named cgroup a value needed is gone since the previous read.
    CgroupGone,
    /// A CI slice was not found, and `user.slice`, or a `user-N.slice` in it,
    /// could not be listed: the slice may be in a user manager there
    /// ([`Placement::unlisted_ci_homes`]). A CI slice the previous read named
    /// there is under this reason, not [`Why::CgroupGone`].
    UserSliceUnlisted,
    /// `ci_node` counted a dispatch since the previous read counted.
    DispatchStarted,
    /// CI work `ci_node` counts as an uncontained run ([`DispatchCounts`])
    /// was in the runner's own cgroup during the interval, or may have been.
    DispatchUncontained,
    /// A directory dispatch cgroups are made in could not be listed. A
    /// dispatch cgroup the previous read listed there is under this reason,
    /// not [`Why::CgroupGone`].
    DispatchDirUnlisted,
    /// `QONTINUI_CI_CGROUP_ROOT` names a directory that is not [`followed`].
    CiCgroupRootNotFollowed,
    /// The runner's own cgroup is unknown.
    OwnCgroupUnknown,
}

impl Why {
    /// How `withheld` names it.
    fn key(self) -> &'static str {
        match self {
            Why::CgroupGone => "cgroup_gone",
            Why::UserSliceUnlisted => "user_slice_unlisted",
            Why::DispatchStarted => "dispatch_started",
            Why::DispatchUncontained => "dispatch_uncontained",
            Why::DispatchDirUnlisted => "dispatch_dir_unlisted",
            Why::CiCgroupRootNotFollowed => "ci_cgroup_root_not_followed",
            Why::OwnCgroupUnknown => "own_cgroup_unknown",
        }
    }

    /// Every reason, in the order `withheld` lists them. Kept by hand: a
    /// variant added to [`Why`] is added here too, as to the match above.
    #[cfg(test)]
    const ALL: [Why; 7] = [
        Why::CgroupGone,
        Why::UserSliceUnlisted,
        Why::DispatchStarted,
        Why::DispatchUncontained,
        Why::DispatchDirUnlisted,
        Why::CiCgroupRootNotFollowed,
        Why::OwnCgroupUnknown,
    ];
}

/// The class each watched slice is charged to; `None` leaves a slice
/// unattributed. Every entry of [`WATCHED_SLICES`] has one, which a test pins.
pub(super) fn watched_class(slice: &str) -> Option<Class> {
    match slice {
        "agent-builds.slice" | "qontinui-runner.slice" => Some(Class::Session),
        "ci-runners.slice" => Some(Class::Ci),
        _ => None,
    }
}

/// One cgroup at one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Reading {
    /// The directory's inode (the kernfs node id): a cgroup removed and created
    /// again under the same name is a different directory with a fresh counter.
    id: u64,
    usage_usec: u64,
}

/// The previous read of one cgroup root.
#[derive(Debug, Clone)]
pub(super) struct Snapshot {
    taken_at: Instant,
    /// Every cgroup read, by path relative to the root (`""` is the root).
    readings: BTreeMap<String, Reading>,
    /// The named cgroups other than holder scopes, with their class. One gone
    /// by the next read withholds the values it was part of.
    strict: BTreeMap<String, Class>,
    /// The holder scopes found, so the next read can count the ones that
    /// ended; `None` when they were not all found.
    holders: Option<BTreeSet<String>>,
    /// What `ci_node` had counted before this read listed the cgroups.
    dispatches: DispatchCounts,
}

/// What changed between two reads of a root.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct CpuReport {
    pub(super) interval_usec: u64,
    /// Each watched slice's own delta, by slice name.
    pub(super) slices: BTreeMap<&'static str, u64>,
    /// The `cpu_attribution` object; empty when nothing in it was measured.
    pub(super) attribution: Map<String, Value>,
}

/// `usage_usec` from a `cpu.stat` body; `None` when the line is missing or does
/// not parse.
pub(super) fn parse_usage_usec(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix("usage_usec "))
        .and_then(|v| v.trim().parse().ok())
}

/// A sum of CPU times. It saturates rather than overflow: no real counter
/// comes near `u64::MAX`, and a panic would drop the whole resource sample.
fn sum_usec(values: impl IntoIterator<Item = u64>) -> u64 {
    values.into_iter().fold(0, u64::saturating_add)
}

/// [`sum_usec`] over values that may be unmeasured: `None` when one is.
fn sum_measured(values: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
    values
        .into_iter()
        .try_fold(0_u64, |total, v| v.map(|v| total.saturating_add(v)))
}

/// CPU time between two readings of one cgroup. `None` unless both are of the
/// same directory and the counter did not go backwards, so a recreated cgroup
/// or a reset counter gives no delta: never a negative one, and never a whole
/// lifetime's.
pub(super) fn delta(prev: Option<Reading>, cur: Option<Reading>) -> Option<u64> {
    let (p, c) = (prev?, cur?);
    if p.id != c.id {
        return None;
    }
    c.usage_usec.checked_sub(p.usage_usec)
}

#[cfg(unix)]
fn dir_identity(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

/// cgroup v2 exists only on Linux. Off unix the fixture tests read no identity,
/// so there only a counter going backwards marks a new cgroup.
#[cfg(not(unix))]
fn dir_identity(_meta: &fs::Metadata) -> u64 {
    0
}

/// One cgroup's reading. The directory's identity is taken on both sides of
/// the counter, so a cgroup removed and made again under the same name in
/// between is no reading, rather than the old cgroup's identity paired with
/// the new one's counter. No test can remake a cgroup between the two, so
/// that is argued here, not exercised.
fn read_reading(dir: &Path) -> Option<Reading> {
    let meta = fs::metadata(dir).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let usage_usec = read(&dir.join("cpu.stat")).and_then(|t| parse_usage_usec(&t))?;
    let id = dir_identity(&meta);
    (dir_identity(&fs::metadata(dir).ok()?) == id).then_some(Reading { id, usage_usec })
}

fn is_cgroup(dir: &Path) -> bool {
    dir.join("cpu.stat").is_file()
}

fn is_user_manager(name: &str) -> bool {
    name.starts_with("user@") && name.ends_with(".service")
}

fn is_holder_scope(name: &str) -> bool {
    name.starts_with(qontinui_pty_holder::spawn::HOLDER_UNIT_PREFIX) && name.ends_with(".scope")
}

/// The runner's own unit cgroup from a `/proc/<pid>/cgroup` body, relative to
/// the cgroup root: the `0::` path, as [`parse_unified_cgroup`] reads it for
/// `ci_node`, cut back to its last `.service` or `.scope` component, since a
/// unit may run its processes in a sub-cgroup of itself.
/// `None` for the root, for a path that names no unit, and for a unit that is a
/// whole user manager or an `init.scope`, either of which would charge
/// everything under it to sessions. A login `session-N.scope` is a unit here:
/// a runner started from a login shell is charged that whole login session.
pub(super) fn parse_runner_unit(text: &str) -> Option<String> {
    let path = parse_unified_cgroup(text)?;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|p| *p == "." || *p == "..") {
        return None;
    }
    let i = parts
        .iter()
        .rposition(|p| p.ends_with(".service") || p.ends_with(".scope"))?;
    if is_user_manager(parts[i]) || parts[i] == "init.scope" {
        return None;
    }
    Some(parts[..=i].join("/"))
}

/// The user manager whose `--user` scopes hold the runner's panes: the
/// `user@N.service` on the runner's own cgroup path, else the one belonging to
/// the `user-N.slice` on it (a runner started from a login session). `None`
/// when the path names neither, as for a system service.
pub(super) fn runner_manager(unit_rel: &str) -> Option<String> {
    let parts: Vec<&str> = unit_rel.split('/').collect();
    if let Some(i) = parts.iter().position(|p| is_user_manager(p)) {
        return Some(parts[..=i].join("/"));
    }
    let i = parts
        .iter()
        .position(|p| p.starts_with("user-") && p.ends_with(".slice"))?;
    let uid = parts[i].strip_prefix("user-")?.strip_suffix(".slice")?;
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{}/user@{uid}.service", parts[..=i].join("/")))
}

/// The user manager of the user the runner runs as, from a `/proc/<pid>/status`
/// body: `user@<uid>.service` under `user-<uid>.slice`, for the real uid on the
/// `Uid:` line. The fallback when the runner's cgroup path names no user
/// manager (a system service, say), so holder scopes are still looked for
/// under this runner's own user and never under another user's.
pub(super) fn uid_manager(status: &str) -> Option<String> {
    let uid = status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?;
    if !uid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("user.slice/user-{uid}.slice/user@{uid}.service"))
}

/// The bytes `name` takes in the published JSON, quotes and escapes included.
pub(super) fn published_len(name: &str) -> usize {
    serde_json::to_string(name).map_or(usize::MAX, |s| s.len())
}

/// The subdirectories of `dir`, sorted by name: none when `dir` does not exist,
/// and `None` when it could not be listed in full, since what a failed listing
/// would have found is unknown rather than nothing. A subdirectory whose name
/// is not UTF-8 cannot be named in a path here, so it leaves the listing
/// unknown too.
fn child_dirs(dir: &Path) -> Option<Vec<(String, PathBuf)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Some(Vec::new()),
        Err(e) => return unlisted(dir, &e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => return unlisted(dir, &e),
        };
        match entry.file_type() {
            Ok(t) if t.is_dir() => {}
            Ok(_) => continue,
            // Removed since the listing: it is not there.
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return unlisted(dir, &e),
        }
        match entry.file_name().into_string() {
            Ok(name) => out.push((name, entry.path())),
            Err(name) => return unlisted(dir, &format!("a name that is not UTF-8: {name:?}")),
        }
    }
    out.sort();
    Some(out)
}

fn unlisted<T>(dir: &Path, why: &dyn std::fmt::Display) -> Option<T> {
    debug!(
        dir = %dir.display(),
        error = %why,
        "slice cpu: a cgroup directory could not be listed; what it holds is not measured this read"
    );
    None
}

/// Every holder scope directly under `manager`, or under one of its direct
/// `*.slice` children (`app.slice` is where `systemd-run --user --scope` puts a
/// scope given no `--slice`). `None` when one of those directories could not
/// be listed.
pub(super) fn holder_scopes(manager: &Path) -> Option<Vec<PathBuf>> {
    let mut out = Vec::new();
    for (name, dir) in child_dirs(manager)? {
        if is_holder_scope(&name) {
            out.push(dir);
        } else if name.ends_with(".slice") {
            out.extend(
                child_dirs(&dir)?
                    .into_iter()
                    .filter(|(name, _)| is_holder_scope(name))
                    .map(|(_, path)| path),
            );
        }
    }
    out.sort();
    Some(out)
}

/// Does `rel` step through `.` or `..`? Such a path need not be under the
/// root it is joined to, so it is not followed.
fn has_dot_component(rel: &str) -> bool {
    rel.split('/').any(|p| p == "." || p == "..")
}

/// `dir` relative to `root` when it is followed: under `root`, and named
/// without `.` or `..`.
fn followed(root: &Path, dir: &Path) -> Option<String> {
    rel_of(root, dir).filter(|rel| !has_dot_component(rel))
}

/// The directories `ci_node` makes CI dispatch cgroups in, relative to `root`:
/// `ci_cgroup_root`, the directory an operator named in
/// `QONTINUI_CI_CGROUP_ROOT` (which `ci_node` uses instead of discovery), when
/// it is [`followed`], and each parent [`candidate_parents`] would have tried
/// for a runner whose own cgroup is `own` (`/`-rooted, as `/proc/self/cgroup`
/// gives it). A directory named through `..` is left out.
fn dispatch_parents(root: &Path, own: Option<&str>, ci_cgroup_root: Option<&Path>) -> Vec<String> {
    let mut parents: Vec<String> = ci_cgroup_root
        .and_then(|dir| followed(root, dir))
        .into_iter()
        .collect();
    // Discovery makes no dispatch cgroup in the root cgroup; an operator can
    // still name the root.
    parents.extend(
        own.map(candidate_parents)
            .unwrap_or_default()
            .iter()
            .map(|parent| parent.trim_start_matches('/').to_string())
            .filter(|rel| !rel.is_empty()),
    );
    parents.retain(|rel| !has_dot_component(rel));
    parents.sort();
    parents.dedup();
    parents
}

/// The `dispatch-*` cgroups in `parents` ([`dispatch_parents`]), relative to
/// `root`. `None` when one of those directories could not be listed.
fn dispatch_cgroups(root: &Path, parents: &[String]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for rel in parents {
        for (name, dir) in child_dirs(&dir_of(root, rel))? {
            if name.starts_with(DISPATCH_DIR_PREFIX) && is_cgroup(&dir) {
                out.extend(rel_of(root, &dir));
            }
        }
    }
    out.sort();
    out.dedup();
    Some(out)
}

/// `dir` relative to `root`, `/`-separated. `None` outside the root or for a
/// name that is not UTF-8.
fn rel_of(root: &Path, dir: &Path) -> Option<String> {
    let rest = dir.strip_prefix(root).ok()?;
    let parts: Vec<&str> = rest
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<_>>()?;
    Some(parts.join("/"))
}

fn dir_of(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// The path of `name` inside `parent`, both relative to the root.
fn child_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn depth(rel: &str) -> usize {
    if rel.is_empty() {
        0
    } else {
        rel.split('/').count()
    }
}

/// Is `rel` strictly inside `ancestor`? Both relative to the root; `""` is the
/// root itself.
fn is_under(rel: &str, ancestor: &str) -> bool {
    if ancestor.is_empty() {
        return !rel.is_empty();
    }
    rel.len() > ancestor.len()
        && rel.starts_with(ancestor)
        && rel.as_bytes()[ancestor.len()] == b'/'
}

/// `whole - parts`, where `parts` are cgroups inside `whole` that were read
/// just before it. Children are read first, so a parent's delta can trail the
/// sum of theirs only by what they used while the previous read was under way.
/// That shortfall is small and clamps to 0. A larger one means the parts were
/// not all inside the whole, and publishes nothing rather than a wrong number.
/// The root's own counter, which can read below its children, never stands for
/// the whole here: [`Placement::host_time`] is at least their sum.
fn exclusive(what: &str, whole: u64, parts: u64) -> Option<u64> {
    if let Some(v) = whole.checked_sub(parts) {
        return Some(v);
    }
    if parts - whole <= (whole / 50).max(SKEW_FLOOR_USEC) {
        return Some(0);
    }
    debug!(
        cgroup = what,
        whole_usec = whole,
        parts_usec = parts,
        "slice cpu: nested cgroups used more than the cgroup holding them; publishing no value"
    );
    None
}

/// A cgroup whose CPU is charged to a class.
#[derive(Debug, Clone)]
struct Named {
    rel: String,
    class: Class,
    /// Holder scopes come and go with their panes: one without a delta this
    /// read is left out, and its time stays in the remainder holding it.
    churns: bool,
}

/// Where, this read, each cgroup the attribution needs lives under the root.
#[derive(Debug)]
struct Placement {
    runner_unit: Option<String>,
    /// The user manager the holder scopes were looked for under. `None` when
    /// neither the runner's cgroup path nor its uid names one.
    manager: Option<String>,
    /// The holder scopes under `manager`. `None` when no manager is known or a
    /// directory they live in could not be listed: then they were not all
    /// found, and nothing that needs them is published.
    holders: Option<Vec<String>>,
    /// The runner's own cgroup (`""` is the root), where an uncontained CI
    /// dispatch runs, and a step `ci_node` could not place in its dispatch's
    /// cgroup. `None` when `/proc/self/cgroup` names none.
    own: Option<String>,
    /// Where `ci_node` makes dispatch cgroups ([`dispatch_parents`]).
    dispatch_parents: Vec<String>,
    /// Why `ci_node` could have made a dispatch cgroup in a directory
    /// `dispatch_parents` does not hold; `None` when it could not. With
    /// `QONTINUI_CI_CGROUP_ROOT` set it makes them only there, so that
    /// directory must be [`followed`]; otherwise beside the runner's own
    /// cgroup, which must then be known. When not, a dispatch cgroup could be
    /// anywhere ([`Withheld::find`]).
    dispatch_parents_unknown: Option<Why>,
    /// The CI dispatch cgroups ([`dispatch_cgroups`]). `None` when a directory
    /// they live in could not be listed, which withholds what one there would
    /// be part of ([`Withheld::find`]): a dispatch cgroup is CI work inside
    /// the runner's unit.
    dispatches: Option<Vec<String>>,
    /// Each watched slice's directory ([`find_watched`]); its `cpu` object is
    /// of this directory.
    watched: Vec<(&'static str, String)>,
    /// `user.slice`, and each `user-N.slice` in it, that could not be listed
    /// for the bases the watched slices and `ci.slice` are looked for under
    /// ([`slice_bases`]).
    users_unlisted: Vec<String>,
    /// The session-class watched slices charged to sessions
    /// ([`Self::find_session_slices`]).
    session_slices: Vec<String>,
    ci_slice: Option<String>,
    /// `ci.slice`'s named children, by name ([`find_ci`]). `None` when
    /// `ci.slice` could not be listed.
    ci_children: Option<Vec<(String, String)>>,
    ci_children_omitted: usize,
    /// The root's direct child cgroups; `None` when the root could not be
    /// listed.
    root_children: Option<Vec<String>>,
}

/// The holder scopes under `manager`, relative to `root`, less the runner's own
/// unit (a runner started from a pane runs in that pane's scope). `None` when a
/// directory they live in could not be listed.
fn find_holders(root: &Path, manager: &str, runner_unit: Option<&str>) -> Option<Vec<String>> {
    let mut rels: Vec<String> = holder_scopes(&root.join(manager))?
        .iter()
        .filter_map(|d| rel_of(root, d))
        .filter(|rel| runner_unit != Some(rel.as_str()))
        .collect();
    rels.sort();
    rels.dedup();
    Some(rels)
}

/// Each watched slice's directory: the one its memory entry was read from
/// (`memory_dirs`, from [`read_slices`](super::read_slices)), which is not
/// looked for again, so its `cpu` object is of the same cgroup; for a slice
/// with no memory entry, its first candidate under `bases` ([`slice_bases`])
/// that is a cgroup.
fn find_watched(
    root: &Path,
    bases: &[PathBuf],
    memory_dirs: &[(&'static str, PathBuf)],
) -> Vec<(&'static str, String)> {
    WATCHED_SLICES
        .iter()
        .filter_map(|&slice| {
            let dir = match memory_dirs.iter().find(|(s, _)| *s == slice) {
                Some((_, dir)) => dir.clone(),
                None => slice_dirs_in(bases, slice)
                    .into_iter()
                    .find(|d| is_cgroup(d))?,
            };
            Some((slice, rel_of(root, &dir)?))
        })
        .collect()
}

/// `ci.slice`, its named children (name, path) and how many were not named.
type CiPlacement = (Option<String>, Option<Vec<(String, String)>>, usize);

/// `ci.slice` under `bases` ([`slice_bases`]), its `ci-*.slice` children by
/// name, and how many of those were not named: at most [`MAX_CI_CHILDREN`]
/// are, each only when its name fits [`MAX_PUBLISHED_NAME_BYTES`]. The
/// children are `None` when `ci.slice` could not be listed; its own time is
/// still the CI total then.
fn find_ci(root: &Path, bases: &[PathBuf]) -> CiPlacement {
    let Some(dir) = slice_dirs_in(bases, "ci.slice")
        .into_iter()
        .find(|d| is_cgroup(d))
    else {
        return (None, Some(Vec::new()), 0);
    };
    let ci_slice = rel_of(root, &dir);
    let Some(children) = child_dirs(&dir) else {
        return (ci_slice, None, 0);
    };
    let (mut named, mut omitted) = (Vec::new(), 0);
    for (name, child) in children {
        if nested_parent(&name).as_deref() != Some("ci.slice") {
            continue;
        }
        match rel_of(root, &child) {
            Some(rel)
                if named.len() < MAX_CI_CHILDREN
                    && published_len(&name) <= MAX_PUBLISHED_NAME_BYTES =>
            {
                named.push((name, rel));
            }
            _ => omitted += 1,
        }
    }
    (ci_slice, Some(named), omitted)
}

/// The root's direct child cgroups; `None` when the root could not be listed.
fn find_root_children(root: &Path) -> Option<Vec<String>> {
    child_dirs(root).map(|children| {
        children
            .into_iter()
            .filter(|(_, dir)| is_cgroup(dir))
            .filter_map(|(_, dir)| rel_of(root, &dir))
            .collect()
    })
}

impl Placement {
    /// `procfs` names the runner's own cgroup and user, and `memory_dirs`
    /// where the watched slices' memory was read ([`measure`]);
    /// `ci_cgroup_root` is `QONTINUI_CI_CGROUP_ROOT` ([`dispatch_cgroups`]).
    fn find(
        root: &Path,
        procfs: Option<&Path>,
        ci_cgroup_root: Option<&Path>,
        memory_dirs: &[(&'static str, PathBuf)],
    ) -> Self {
        let own_cgroup = procfs.and_then(|p| read(&p.join("self").join("cgroup")));
        let runner_unit = own_cgroup
            .as_deref()
            .and_then(parse_runner_unit)
            .filter(|rel| is_cgroup(&root.join(rel)));
        let manager = runner_unit.as_deref().and_then(runner_manager).or_else(|| {
            procfs
                .and_then(|p| read(&p.join("self").join("status")))
                .and_then(|t| uid_manager(&t))
        });
        let holders = manager
            .as_deref()
            .and_then(|m| find_holders(root, m, runner_unit.as_deref()));
        let own_path = own_cgroup.as_deref().and_then(parse_unified_cgroup);
        let dispatch_parents = dispatch_parents(root, own_path.as_deref(), ci_cgroup_root);
        let dispatches = dispatch_cgroups(root, &dispatch_parents);
        let own = own_path
            .map(|o| o.trim_matches('/').to_string())
            .filter(|rel| !has_dot_component(rel));
        let dispatch_parents_unknown = match ci_cgroup_root {
            Some(dir) => followed(root, dir)
                .is_none()
                .then_some(Why::CiCgroupRootNotFollowed),
            None => own.is_none().then_some(Why::OwnCgroupUnknown),
        };
        let SliceBases { bases, unlisted } = slice_bases(root);
        // Each directory `slice_bases` could not list is `user.slice` or in it.
        let users_unlisted = unlisted
            .iter()
            .map(|dir| rel_of(root, dir).unwrap_or_else(|| "user.slice".to_string()))
            .collect();
        let (ci_slice, ci_children, ci_children_omitted) = find_ci(root, &bases);
        let mut placement = Self {
            runner_unit,
            manager,
            holders,
            own,
            dispatch_parents,
            dispatch_parents_unknown,
            dispatches,
            watched: find_watched(root, &bases, memory_dirs),
            users_unlisted,
            session_slices: Vec::new(),
            ci_slice,
            ci_children,
            ci_children_omitted,
            root_children: find_root_children(root),
        };
        placement.session_slices = placement.find_session_slices(root);
        placement
    }

    /// Every directory of a session-class watched slice that is a cgroup under
    /// the root, `system.slice` or the runner's own user manager, the bases
    /// [`slice_dirs`] tries less every other user's manager. They are named
    /// rather than listed, so a `user.slice` that could not be listed hides
    /// none of them. The memory entry describes a slice's first candidate,
    /// which can be another user's; the runner's own is still found.
    fn find_session_slices(&self, root: &Path) -> Vec<String> {
        let mut bases = vec![root.to_path_buf(), root.join("system.slice")];
        bases.extend(self.manager.as_deref().map(|m| root.join(m)));
        let mut out: Vec<String> = WATCHED_SLICES
            .iter()
            .filter(|slice| watched_class(slice) == Some(Class::Session))
            .flat_map(|slice| slice_dirs_in(&bases, slice))
            .filter(|dir| is_cgroup(dir))
            .filter_map(|dir| rel_of(root, &dir))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Were all of `class`'s cgroups found this read? Session work needs the
    /// runner's own unit and the holder scopes. The dispatch cgroups, which
    /// both classes need, are [`Withheld::find`]'s: one this read could not
    /// list is withheld like one no read listed.
    fn placed(&self, class: Class) -> bool {
        class != Class::Session || (self.runner_unit.is_some() && self.holders.is_some())
    }

    /// The directories a CI slice this read did not find could be in, in a
    /// user manager no listing reached: `user.slice`, and each `user-N.slice`
    /// in it, that could not be listed ([`slice_bases`]), when `ci.slice` or a
    /// CI-class watched slice was not found under the bases that were; none
    /// otherwise. A CI slice is not looked for by name the way the session
    /// slices are ([`Self::find_session_slices`]): it can be in any user's
    /// manager, not only the runner's.
    fn unlisted_ci_homes(&self) -> &[String] {
        let watched_missing = WATCHED_SLICES
            .iter()
            .filter(|slice| watched_class(slice) == Some(Class::Ci))
            .any(|slice| !self.watched.iter().any(|(w, _)| w == slice));
        if self.ci_slice.is_none() || watched_missing {
            &self.users_unlisted
        } else {
            &[]
        }
    }

    /// Could `rel` be a CI slice this read did not name because the user
    /// manager it is in could not be listed? That is when `rel` is `ci.slice`
    /// or a CI-class watched slice in one of [`Self::unlisted_ci_homes`].
    /// [`Withheld::find`] withholds what one there is part of as unlisted, not
    /// as gone. A dispatch cgroup is not a slice: one gone from the runner's
    /// unit is gone, whatever could be listed.
    fn unlisted_ci_slice_at(&self, rel: &str) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let ci_slice = name == "ci.slice"
            || WATCHED_SLICES
                .iter()
                .any(|slice| *slice == name && watched_class(slice) == Some(Class::Ci));
        ci_slice
            && self
                .unlisted_ci_homes()
                .iter()
                .any(|dir| is_under(rel, dir))
    }

    /// Could `rel` be a dispatch cgroup this read did not list because it
    /// could not? That is when a directory dispatch cgroups are made in could
    /// not be listed and `rel` is a `dispatch-*` cgroup directly in one of
    /// them. [`Withheld::find`] withholds what one there is part of as
    /// unlisted, not as gone.
    fn unlisted_dispatch_at(&self, rel: &str) -> bool {
        self.dispatches.is_none()
            && self.dispatch_parents.iter().any(|home| {
                rel.strip_prefix(child_rel(home, DISPATCH_DIR_PREFIX).as_str())
                    .is_some_and(|rest| !rest.contains('/'))
            })
    }

    /// The host's CPU time this read: the root's own delta, or the summed
    /// deltas of its direct children where they add up to more (the module
    /// docs say why the two differ). `None` without the root's delta, and
    /// without any child's (or the root's listing): a partial sum would leave
    /// the root's own counter, which can read below them, standing for the
    /// whole.
    fn host_time(&self, host: Option<u64>, delta_of: impl Fn(&str) -> Option<u64>) -> Option<u64> {
        let children = sum_measured(self.root_children.as_ref()?.iter().map(|rel| delta_of(rel)))?;
        host.map(|h| h.max(children))
    }

    /// The named cgroups, each once: a path claimed twice keeps its first role,
    /// in the order runner unit, session-class slices, the CI watched slice,
    /// `ci.slice`, dispatch cgroups, holder scopes.
    fn named(&self) -> Vec<Named> {
        let ci_watched = self
            .watched
            .iter()
            .filter(|(slice, _)| watched_class(slice) == Some(Class::Ci))
            .map(|(_, rel)| rel);
        let candidates = self
            .runner_unit
            .iter()
            .map(|r| (r, Class::Session, false))
            .chain(
                self.session_slices
                    .iter()
                    .map(|r| (r, Class::Session, false)),
            )
            .chain(ci_watched.map(|r| (r, Class::Ci, false)))
            .chain(self.ci_slice.iter().map(|r| (r, Class::Ci, false)))
            .chain(
                self.dispatches
                    .iter()
                    .flatten()
                    .map(|r| (r, Class::Ci, false)),
            )
            .chain(
                self.holders
                    .iter()
                    .flatten()
                    .map(|r| (r, Class::Session, true)),
            );
        let mut out: Vec<Named> = Vec::new();
        for (rel, class, churns) in candidates {
            if !out.iter().any(|n| &n.rel == rel) {
                out.push(Named {
                    rel: rel.clone(),
                    class,
                    churns,
                });
            }
        }
        out
    }
}

/// Read every cgroup the attribution needs, children before parents: a parent
/// read after its children includes all they had used by then.
fn read_all(root: &Path, placement: &Placement, named: &[Named]) -> BTreeMap<String, Reading> {
    let mut rels: BTreeSet<&str> = named.iter().map(|n| n.rel.as_str()).collect();
    // Every watched slice, named or not, for its own `cpu` object.
    rels.extend(placement.watched.iter().map(|(_, rel)| rel.as_str()));
    rels.extend(
        placement
            .ci_children
            .iter()
            .flatten()
            .map(|(_, rel)| rel.as_str()),
    );
    rels.extend(placement.root_children.iter().flatten().map(String::as_str));
    rels.extend(REMAINDERS.iter().map(|(slice, _)| *slice));
    rels.insert("");
    let mut order: Vec<&str> = rels.into_iter().collect();
    order.sort_by_key(|rel| std::cmp::Reverse(depth(rel)));
    order
        .into_iter()
        .filter_map(|rel| Some((rel.to_string(), read_reading(&dir_of(root, rel))?)))
        .collect()
}

/// The named cgroups this read can stand on (a holder with no delta is left
/// out), each with its delta and its nearest named ancestor.
struct Forest<'a> {
    nodes: Vec<(&'a Named, Option<u64>)>,
    parent: Vec<Option<usize>>,
    /// The first node of the same-class run each node belongs to.
    run_head: Vec<usize>,
}

impl<'a> Forest<'a> {
    fn new(named: &'a [Named], delta_of: impl Fn(&str) -> Option<u64>) -> Self {
        let nodes: Vec<(&Named, Option<u64>)> = named
            .iter()
            .map(|n| (n, delta_of(&n.rel)))
            .filter(|(n, d)| !n.churns || d.is_some())
            .collect();
        let parent: Vec<Option<usize>> = nodes
            .iter()
            .map(|(n, _)| {
                nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, (m, _))| is_under(&n.rel, &m.rel))
                    .max_by_key(|(_, (m, _))| m.rel.len())
                    .map(|(j, _)| j)
            })
            .collect();
        let heads = |i: usize| parent[i].is_none_or(|p| nodes[p].0.class != nodes[i].0.class);
        let run_head = (0..nodes.len())
            .map(|mut i| {
                while let Some(p) = parent[i].filter(|_| !heads(i)) {
                    i = p;
                }
                i
            })
            .collect();
        Self {
            nodes,
            parent,
            run_head,
        }
    }

    /// Per class with a cgroup here: the time of each of its runs, minus the
    /// differently-classed runs directly inside it. `None` when a delta it needs
    /// is missing.
    fn class_totals(&self) -> BTreeMap<Class, Option<u64>> {
        let n = self.nodes.len();
        let mut out: BTreeMap<Class, Option<u64>> = BTreeMap::new();
        for h in (0..n).filter(|&i| self.run_head[i] == i) {
            let inner = sum_measured(
                (0..n)
                    .filter(|&b| self.run_head[b] == b && b != h)
                    .filter(|&b| self.parent[b].map(|p| self.run_head[p]) == Some(h))
                    .map(|b| self.nodes[b].1),
            );
            let (named, whole) = self.nodes[h];
            let own = whole
                .zip(inner)
                .and_then(|(w, s)| exclusive(&named.rel, w, s));
            let total = out.entry(named.class).or_insert(Some(0));
            *total = total.zip(own).map(|(t, o)| t.saturating_add(o));
        }
        out
    }

    /// The summed delta of the outermost named cgroups inside `top` (`""` is
    /// the root); `None` when one of them has no delta.
    fn outermost_inside(&self, top: &str) -> Option<u64> {
        sum_measured(
            (0..self.nodes.len())
                .filter(|&i| self.parent[i].is_none() && is_under(&self.nodes[i].0.rel, top))
                .map(|i| self.nodes[i].1),
        )
    }

    /// The nearest node strictly above `rel`.
    fn nearest_above(&self, rel: &str) -> Option<&Named> {
        self.nodes
            .iter()
            .map(|(m, _)| *m)
            .filter(|m| is_under(rel, &m.rel))
            .max_by_key(|m| m.rel.len())
    }
}

/// What this read cannot measure: the values that need the time of a named
/// cgroup that disappeared since the previous read, or of a CI dispatch that
/// no read listed. That time is in the cgroups that held it, and in no named
/// one of its class. `reasons` says why, for `withheld`.
#[derive(Debug, Default)]
struct Withheld {
    classes: BTreeSet<Class>,
    remainders: BTreeSet<&'static str>,
    unattributed: bool,
    reasons: BTreeSet<Why>,
}

impl Withheld {
    /// `after` is what `ci_node` had counted once this read's counters were
    /// read, and the previous read kept what it had counted before it listed
    /// the cgroups. A dispatch counted in between may have run where neither
    /// listing saw it. An uncontained run alive then, or begun in between and
    /// perhaps over already, ran in the runner's own cgroup in this interval,
    /// or may have. A dispatch cgroup in a directory this read did not look in,
    /// or could not list, was not listed either, however long it has run. One
    /// the previous read listed is then missing from this read's for that
    /// reason, whether or not it is gone, so it is withheld as unlisted
    /// rather than reported gone. A CI slice in a user manager this read could
    /// not list was not named at all ([`Placement::unlisted_ci_homes`]).
    fn find(
        p: &Placement,
        prev: &Snapshot,
        after: DispatchCounts,
        named: &[Named],
        forest: &Forest<'_>,
    ) -> Self {
        let mut w = Self::default();
        for (rel, &class) in &prev.strict {
            if named.iter().any(|n| &n.rel == rel)
                || p.unlisted_dispatch_at(rel)
                || p.unlisted_ci_slice_at(rel)
            {
                continue;
            }
            if w.lost(rel, class, forest) {
                w.reasons.insert(Why::CgroupGone);
            }
        }
        let mut unlisted_ci = false;
        for dir in p.unlisted_ci_homes() {
            // The slice would be in this directory, in no named cgroup:
            // `ci.slice` here stands for its place.
            unlisted_ci |= w.lost(&child_rel(dir, "ci.slice"), Class::Ci, forest);
        }
        if unlisted_ci {
            w.reasons.insert(Why::UserSliceUnlisted);
        }
        let mut why: BTreeSet<Why> = p.dispatch_parents_unknown.into_iter().collect();
        if p.dispatches.is_none() {
            why.insert(Why::DispatchDirUnlisted);
        }
        if after.started != prev.dispatches.started {
            why.insert(Why::DispatchStarted);
        }
        if prev.dispatches.uncontained_running > 0
            || after.uncontained_begun != prev.dispatches.uncontained_begun
        {
            why.insert(Why::DispatchUncontained);
        }
        if !why.is_empty() && w.unlisted_dispatch(p, forest) {
            // With the runner's own cgroup unknown, `unlisted_dispatch` could
            // not tell where such a run was, and withheld everything.
            if p.own.is_none() {
                why.insert(Why::OwnCgroupUnknown);
            }
            w.reasons.extend(why);
        }
        w
    }

    /// A cgroup of `class` at `rel` whose time this read cannot see. True when
    /// that withholds anything, as it does unless the cgroup sat inside a
    /// named cgroup of its own class.
    fn lost(&mut self, rel: &str, class: Class, forest: &Forest<'_>) -> bool {
        match forest.nearest_above(rel) {
            // Inside a cgroup of its own class: that one's time still holds it.
            Some(holder) if holder.class == class => false,
            Some(holder) => {
                self.classes.insert(class);
                self.classes.insert(holder.class);
                true
            }
            None => {
                self.classes.insert(class);
                self.unattributed = true;
                self.remainders.extend(
                    REMAINDERS
                        .iter()
                        .map(|(slice, _)| *slice)
                        .filter(|slice| is_under(rel, slice)),
                );
                true
            }
        }
    }

    /// A dispatch that may have run since the previous read where no listed
    /// dispatch cgroup holds its time ([`Self::find`]): in a dispatch cgroup no
    /// read listed, which is in a dispatch parent or the runner's own cgroup,
    /// or in the runner's own cgroup itself. With that cgroup unknown, or a
    /// `QONTINUI_CI_CGROUP_ROOT` that is not followed, it could be anywhere.
    /// True when that withholds anything.
    fn unlisted_dispatch(&mut self, p: &Placement, forest: &Forest<'_>) -> bool {
        let Some(own) = p
            .own
            .as_ref()
            .filter(|_| p.dispatch_parents_unknown.is_none())
        else {
            self.classes.extend([Class::Session, Class::Ci]);
            self.unattributed = true;
            self.remainders
                .extend(REMAINDERS.iter().map(|(slice, _)| *slice));
            return true;
        };
        let mut any = false;
        for home in p.dispatch_parents.iter().chain([own]) {
            any |= self.lost(&child_rel(home, DISPATCH_DIR_PREFIX), Class::Ci, forest);
        }
        any
    }
}

/// Said once per process at `warn`, then at `debug`: a read past the stale line
/// publishes no CPU. With `COORD_RESOURCE_SAMPLE_SECS` set past about 100 s
/// some reads are past it (the cadence jitters by a fifth, and the publish loop
/// spends time of its own between sleeps), and past about 150 s all are.
fn note_stale(interval_usec: u64) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if SAID.swap(true, Ordering::Relaxed) {
        debug!(
            interval_usec,
            "slice cpu: a read past the stale line publishes no CPU"
        );
    } else {
        warn!(
            interval_usec,
            max_interval_usec = MAX_INTERVAL_USEC,
            "slice cpu: this read came longer after the previous one than the stale line, so it \
             publishes no CPU; with COORD_RESOURCE_SAMPLE_SECS past about 100 s some reads will \
             not either, and past about 150 s none will publish CPU (said once)"
        );
    }
}

/// The `cpu_attribution` object being built. A value goes in only when it was
/// measured, and `measured` says whether one did: an object holding only
/// counts and names is not published.
#[derive(Default)]
struct Attribution {
    map: Map<String, Value>,
    measured: bool,
}

impl Attribution {
    /// A CPU time, when it was measured.
    fn put(&mut self, key: &str, usec: Option<u64>) {
        if let Some(v) = usec {
            self.map.insert(key.to_string(), json!(v));
            self.measured = true;
        }
    }

    /// A count or a name: what a measurement covered, not a measurement.
    fn note(&mut self, key: &str, v: Value) {
        self.map.insert(key.to_string(), v);
    }
}

/// The host's time and its split: the class totals, `unattributed_usec` and
/// the remainders, each only where nothing it needs was withheld, and why
/// anything was.
fn class_keys(
    p: &Placement,
    forest: &Forest<'_>,
    withheld: &Withheld,
    delta_of: impl Fn(&str) -> Option<u64> + Copy,
    a: &mut Attribution,
) {
    let host = delta_of("");
    a.put("host_usec", host);
    for (class, total) in forest.class_totals() {
        if p.placed(class) && !withheld.classes.contains(&class) {
            a.put(class.key(), total);
        }
    }
    if !withheld.unattributed {
        let unattributed = p
            .host_time(host, delta_of)
            .zip(forest.outermost_inside(""))
            .and_then(|(w, s)| exclusive("/", w, s));
        a.put("unattributed_usec", unattributed);
    }
    for (slice, key) in REMAINDERS {
        if !withheld.remainders.contains(slice) {
            let remainder = delta_of(slice)
                .zip(forest.outermost_inside(slice))
                .and_then(|(w, s)| exclusive(slice, w, s));
            a.put(key, remainder);
        }
    }
    if !withheld.reasons.is_empty() {
        let reasons: Vec<&str> = withheld.reasons.iter().map(|w| w.key()).collect();
        a.note("withheld", json!(reasons));
    }
}

/// The runner unit's own time and path, and the holder keys: how many holder
/// scopes were seen, how many of them were measured this read, their summed
/// time, and how many of those seen at the previous read are gone. No holder
/// key when the holders were not all found, and no count of ended ones unless
/// the previous read found them all too.
fn runner_keys(
    p: &Placement,
    prev: &Snapshot,
    delta_of: impl Fn(&str) -> Option<u64>,
    a: &mut Attribution,
) {
    if let Some(unit) = &p.runner_unit {
        a.put("runner_unit_usec", delta_of(unit));
        if published_len(unit) <= MAX_PUBLISHED_NAME_BYTES {
            a.note("runner_unit", json!(unit));
        }
    }
    let Some(holders) = &p.holders else {
        return;
    };
    let deltas: Vec<u64> = holders.iter().filter_map(|h| delta_of(h)).collect();
    if holders.is_empty() || !deltas.is_empty() {
        a.put(
            "pty_holder_scopes_usec",
            Some(sum_usec(deltas.iter().copied())),
        );
    }
    a.note("pty_holder_scopes_measured", json!(deltas.len()));
    a.note("pty_holder_scopes_seen", json!(holders.len()));
    if let Some(before) = &prev.holders {
        let ended = before
            .iter()
            .filter(|h| holders.binary_search(h).is_err())
            .count();
        a.note("pty_holder_scopes_ended", json!(ended));
    }
}

/// The `ci.slice` children keys, when there is a `ci.slice`: each named
/// child's delta, how many named children had none this read, and how many
/// were not named; or, when `ci.slice` could not be listed, a mark saying so.
fn ci_children_keys(p: &Placement, delta_of: impl Fn(&str) -> Option<u64>, a: &mut Attribution) {
    if p.ci_slice.is_none() {
        return;
    }
    let Some(named) = &p.ci_children else {
        a.note("ci_children_unlisted", json!(true));
        return;
    };
    let mut children = Map::new();
    let mut unmeasured = 0_usize;
    for (name, rel) in named {
        match delta_of(rel) {
            Some(d) => {
                children.insert(name.clone(), json!(d));
            }
            None => unmeasured += 1,
        }
    }
    if !children.is_empty() {
        a.note("ci_children_usec", Value::Object(children));
        a.measured = true;
    }
    a.note("ci_children_unmeasured", json!(unmeasured));
    a.note("ci_children_omitted", json!(p.ci_children_omitted));
}

/// What changed between `prev` and `cur`, two reads of one root, `after` being
/// what `ci_node` had counted once `cur`'s counters were read. `None` when no
/// time, or more than [`MAX_INTERVAL_USEC`], has passed between them.
fn report(
    p: &Placement,
    named: &[Named],
    prev: &Snapshot,
    cur: &Snapshot,
    after: DispatchCounts,
) -> Option<CpuReport> {
    let elapsed = cur.taken_at.checked_duration_since(prev.taken_at)?;
    let interval_usec = u64::try_from(elapsed.as_micros()).ok().filter(|&i| i > 0)?;
    if interval_usec > MAX_INTERVAL_USEC {
        note_stale(interval_usec);
        return None;
    }
    let delta_of = |rel: &str| {
        delta(
            prev.readings.get(rel).copied(),
            cur.readings.get(rel).copied(),
        )
    };
    let slices = p
        .watched
        .iter()
        .filter_map(|(slice, rel)| Some((*slice, delta_of(rel)?)))
        .collect();

    let forest = Forest::new(named, delta_of);
    let withheld = Withheld::find(p, prev, after, named, &forest);
    let mut a = Attribution::default();
    class_keys(p, &forest, &withheld, delta_of, &mut a);
    runner_keys(p, prev, delta_of, &mut a);
    ci_children_keys(p, delta_of, &mut a);
    let attribution = if a.measured {
        a.note("interval_usec", json!(interval_usec));
        a.map
    } else {
        Map::new()
    };
    Some(CpuReport {
        interval_usec,
        slices,
        attribution,
    })
}

/// One read of `cgroup_root`: the snapshot to keep for the next read and, when
/// `prev` is an earlier read of the same root, what changed since it.
/// `procfs` is the `/proc` whose `self/cgroup` names the runner's own cgroup
/// and whose `self/status` names the user it runs as; `None` finds no runner
/// unit, holder scope or own cgroup, and so publishes no session total, and no
/// CI total, remainder or `unattributed_usec` unless `ci_cgroup_root` is
/// followed. `ci_cgroup_root` is `QONTINUI_CI_CGROUP_ROOT`. `memory_dirs` is
/// the directory [`read_slices`](super::read_slices) read each watched slice's
/// memory from, which is where that slice's CPU is read ([`find_watched`]).
/// `dispatches` says what `ci_node` has counted; it is called before the
/// cgroups are listed and again after their counters are read
/// ([`Withheld::find`]). `now` is called once the cgroups are placed and just
/// before the counters are read, so listing the directories does not count in
/// the interval.
pub(super) fn measure(
    cgroup_root: &Path,
    procfs: Option<&Path>,
    ci_cgroup_root: Option<&Path>,
    memory_dirs: &[(&'static str, PathBuf)],
    dispatches: impl Fn() -> DispatchCounts,
    prev: Option<&Snapshot>,
    now: impl FnOnce() -> Instant,
) -> (Snapshot, Option<CpuReport>) {
    // Before the listing: a dispatch counted after this could have been made
    // after it too, since `ci_node` counts one only once its cgroup exists.
    let before = dispatches();
    let placement = Placement::find(cgroup_root, procfs, ci_cgroup_root, memory_dirs);
    let named = placement.named();
    let taken_at = now();
    let readings = read_all(cgroup_root, &placement, &named);
    // After the counters: `ci_node` starts a dispatch's steps only once it is
    // counted, so a dispatch whose steps these counters hold time of is
    // counted by now.
    let after = dispatches();
    let strict = named
        .iter()
        .filter(|n| !n.churns)
        .map(|n| (n.rel.clone(), n.class))
        .collect();
    let snapshot = Snapshot {
        taken_at,
        readings,
        strict,
        holders: placement
            .holders
            .as_ref()
            .map(|h| h.iter().cloned().collect()),
        dispatches: before,
    };
    let report = prev.and_then(|p| report(&placement, &named, p, &snapshot, after));
    (snapshot, report)
}

/// Add a report to `slices`, the `cgroup_slices` map holding the memory
/// entries: a `cpu` object inside each watched slice's existing entry (never a
/// new entry), and the attribution under [`ATTRIBUTION_KEY`] when anything in
/// it was measured.
pub(super) fn apply(report: &CpuReport, slices: &mut Map<String, Value>) {
    for (slice, usec) in &report.slices {
        if let Some(Value::Object(entry)) = slices.get_mut(*slice) {
            entry.insert(
                SLICE_CPU_KEY.into(),
                json!({"usage_delta_usec": usec, "interval_usec": report.interval_usec}),
            );
        }
    }
    if !report.attribution.is_empty() {
        slices.insert(
            ATTRIBUTION_KEY.into(),
            Value::Object(report.attribution.clone()),
        );
    }
}

/// The previous read of each root, between calls.
static PREVIOUS: Mutex<BTreeMap<PathBuf, Snapshot>> = Mutex::new(BTreeMap::new());

fn previous() -> MutexGuard<'static, BTreeMap<PathBuf, Snapshot>> {
    PREVIOUS.lock().unwrap_or_else(|e| e.into_inner())
}

fn remember(root: &Path, snapshot: Snapshot) {
    keep(&mut previous(), root, snapshot);
}

/// Keep `snapshot` in `map` as the previous read of `root`. A root new to a
/// map holding [`MAX_ROOTS`] takes the place of the one read longest ago.
fn keep(map: &mut BTreeMap<PathBuf, Snapshot>, root: &Path, snapshot: Snapshot) {
    if !map.contains_key(root) && map.len() >= MAX_ROOTS {
        let oldest = map
            .iter()
            .min_by_key(|(_, s)| s.taken_at)
            .map(|(k, _)| k.clone());
        if let Some(k) = oldest {
            map.remove(&k);
        }
    }
    map.insert(root.to_path_buf(), snapshot);
}

/// Measure `cgroup_root` against its previous read and add the CPU keys to
/// `slices`, the `cgroup_slices` map already holding the memory entries. The
/// first read of a root only remembers it. `proc_pressure` is
/// `<procfs>/pressure`, whose parent names the runner's own cgroup and user.
/// `memory_dirs` is where each slice in `slices` was read from, and
/// `dispatches` says what `ci_node` has counted ([`measure`]).
pub(super) fn attach(
    cgroup_root: &Path,
    proc_pressure: &Path,
    memory_dirs: &[(&'static str, PathBuf)],
    dispatches: impl Fn() -> DispatchCounts,
    slices: &mut Map<String, Value>,
) {
    // Read as `ci_node` reads it when it places a dispatch.
    let ci_cgroup_root = std::env::var_os(CGROUP_ROOT_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    // Taken out rather than held: the lock is never kept across the reads.
    let prev = previous().remove(cgroup_root);
    let (snapshot, report) = measure(
        cgroup_root,
        proc_pressure.parent(),
        ci_cgroup_root.as_deref(),
        memory_dirs,
        dispatches,
        prev.as_ref(),
        Instant::now,
    );
    remember(cgroup_root, snapshot);
    if let Some(r) = report {
        apply(&r, slices);
    }
}

#[cfg(test)]
mod tests;
