//! The memory half of the class split: how much memory the session class holds
//! at one read, so coord can charge a session its measured memory rather than
//! a seed (plan `2026-11-05-one-free-resource-model-places-sessions-builds-and-ci`,
//! D3; the item its Phase 4 left owed: "the runner publishes no per-session
//! memory, so session memory is not measured").
//!
//! # What is read
//!
//! cgroup v2 `memory.current`, in bytes, of every cgroup the CPU split names
//! ([`Placement::named`]): the runner's own unit, each
//! `qontinui-pty-holder-*.scope`, the session-class watched slices
//! (`agent-builds.slice`, and `qontinui-runner.slice` where it is installed),
//! `ci.slice` with its CI watched slice, and the CI dispatch cgroups.
//! `memory.current` counts page cache as well as anonymous memory, which is
//! what Phase 0's F2 sampler read and what coord's session memory seed was
//! measured as.
//!
//! # The session total
//!
//! `memory.current` is hierarchical, as `usage_usec` is: a cgroup's charge
//! includes every descendant's. So the session total is the class total the
//! CPU split's [`Forest`] forms, with memory in place of CPU time: each
//! outermost session cgroup's reading, less the outermost CI cgroups directly
//! inside it (a dispatch cgroup in the runner's unit is CI, not session work),
//! each cgroup counted once (the runner's unit inside `qontinui-runner.slice`
//! is not added twice). A builds slice is taken only under the root,
//! `system.slice` or the runner's own user manager, on every box. On a box
//! without `qontinui-runner.slice` the cgroups are those of Phase 0's `b_full`
//! session memory — the runner's unit, every holder scope and
//! `agent-builds.slice`, the basis its F3 found stable (p90/p50 1.22 on nawa,
//! 1.66 on nilan) — taken as D3 defines the class: the runner's own unit, and
//! every holder scope and builds slice under its user manager, whichever
//! runner under that uid started them (a second runner under the same uid
//! adds its panes and builds, while coord divides by this runner's sessions).
//! Against `b_full` that is less the dispatch cgroups, which `b_full` did not
//! subtract, and less what `b_full` summed under other user managers and in
//! `system.slice` (other runners' units, holder scopes and builds slices). Where `qontinui-runner.slice` is installed and the
//! runner's unit sits inside it, the whole slice is taken in the unit's place;
//! with the slice installed and the unit elsewhere, both are summed. coord
//! divides it by the sample's `active_terminal_sessions`.
//!
//! Each cgroup is read after every named cgroup inside it ([`read_named`]), so
//! a parent's reading holds at least what its children held when they were
//! read, less what they freed in between (a child that grew in between leaves
//! that growth in the parent's own share). A
//! child that frees memory between the two reads can still put the children a
//! little above the parent; up to the larger of 2% of the parent and
//! [`SKEW_FLOOR_BYTES`] that is read skew and the parent's own share is 0, and
//! past it no session total is published ([`exclusive_bytes`]).
//!
//! # A level, not a delta
//!
//! Memory needs no previous read, so every read publishes what it measured,
//! the first one included. A holder scope that ended between the listing and
//! its read is left out: what it still holds (page cache charged to the dying
//! cgroup) is in no named session cgroup. One created after the listing is
//! not read, so its memory is missing from the total while coord's session
//! count may already hold its session: an undercount of one pane at most per
//! new scope, for one read. One still there whose
//! `memory.current` could not be read leaves no session total (below).
//!
//! # Withheld
//!
//! `session_bytes` is absent, and `withheld` names why, when `ci_node` counted
//! CI work of this process in the runner's own cgroup, or a dispatch cgroup it
//! made may not have been listed, while this read was taken:
//!
//! * `dispatch_uncontained` — `ci_node` had an uncontained run alive, or began
//!   one, while this read was taken: CI work in the runner's own cgroup (its
//!   own work for every dispatch, the checkout among it, counts as one);
//! * `dispatch_started` — `ci_node` counted a dispatch while this read was
//!   taken, so its cgroup may postdate the listing;
//! * `dispatch_dir_unlisted`, `ci_cgroup_root_not_followed` — a dispatch
//!   cgroup could be where this read did not look.
//!
//! Every one of them withholds the total, wherever `ci_node` makes its
//! dispatch cgroups (`QONTINUI_CI_CGROUP_ROOT` included): the runner's own
//! cgroup, where `ci_node` runs its own work for every dispatch, is inside the
//! runner's unit. The counts are this process's own (`DispatchCounts`), so CI
//! work another runner process runs uncontained in this unit, or in a
//! dispatch cgroup no read listed, is session memory with nothing withheld, as
//! it is session CPU (a dispatch cgroup of its that a read lists is still
//! subtracted as CI).
//!
//! Neither of those covers all CI memory in a session cgroup, and the rest is
//! in `session_bytes`, as the CPU split's docs say of CI time:
//!
//! * a CI step that builds through `cargo-guard`: the build lands in
//!   `agent-builds.slice`, which is session work;
//! * a process left running by a job that was not contained, or by a step
//!   that was not placed: it stays in the runner's cgroup after the dispatch
//!   ends, uncounted;
//! * memory CI work left charged to a session cgroup after it ended, until the
//!   kernel reclaims it: page cache `ci_node`'s own work charged to the
//!   runner's cgroup (a checkout into the warm primary checkout), and page
//!   cache a dispatch cgroup inside the runner's unit charged before it was
//!   removed, which the kernel keeps counting in the unit.
//!
//! Phase 0's `b_full`, the basis of coord's session memory seed, subtracted no
//! CI memory at all.
//!
//! `session_bytes` is also absent, with no reason listed, when the runner's
//! unit or the holder scopes were not all found (`runner_unit_bytes` and the
//! holder keys show which), or when a reading it needs could not be taken: the
//! unit's or a session slice's, that of a holder scope still there
//! ([`holders_unread`]), or that of a CI cgroup inside one of them (a dispatch
//! cgroup removed between the listing and its read).
//!
//! # Wire shape
//!
//! A top-level `cgroup_slices` key beside `cpu_attribution`, published only
//! when something in it was measured:
//!
//! ```text
//! "memory_attribution": {"session_bytes": N, "runner_unit_bytes": N,
//!     "pty_holder_scopes_bytes": N, "pty_holder_scopes_measured": n,
//!     "pty_holder_scopes_seen": n, "withheld": ["<reason>", ...]}
//! ```
//!
//! `runner_unit_bytes` is the unit's own reading, any dispatch cgroup in it
//! and any CI work run in it included, so it is not session memory:
//! `session_bytes` is. `pty_holder_scopes_bytes` sums the holder scopes read,
//! and is absent when holders were seen but none could be read;
//! `pty_holder_scopes_seen` and `_measured` are published whenever the holders
//! were all found, 0 included. The key names no slice, so every reader that
//! grades slice memory skips it as it skips `cpu_attribution` (coord's
//! `resource_headroom_slice::is_slice_key`, qontinui-web's `isSliceKey`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use tracing::debug;

use super::{depth, dir_of, is_cgroup, Class, Forest, Named, Placement, Why};
use crate::ci_node::cgroup::{DispatchCounts, CGROUP_ROOT_ENV};

/// The `cgroup_slices` key holding the memory split.
const MEMORY_ATTRIBUTION_KEY: &str = "memory_attribution";

/// The least read skew allowed for between a cgroup and the cgroups inside
/// it, 16 MiB; see [`exclusive_bytes`].
const SKEW_FLOOR_BYTES: u64 = 16 * 1024 * 1024;

/// `memory.current` of `dir`; `None` when it is missing or does not parse
/// (the memory controller is not enabled there, or the cgroup is gone).
fn read_bytes(dir: &Path) -> Option<u64> {
    super::super::read(&dir.join("memory.current")).and_then(|t| super::super::parse_bytes(&t))
}

/// `whole - parts` for memory: a cgroup's reading less the readings of the
/// named cgroups inside it, which were read just before it. A child that
/// freed memory in between can leave the parts a little above the whole; up to
/// the larger of 2% of the whole and [`SKEW_FLOOR_BYTES`] that is read skew
/// and the whole's own share is 0. More is a larger free in that window (a
/// large process exiting between the two reads), and nothing is published
/// rather than a wrong number.
fn exclusive_bytes(what: &str, whole: u64, parts: u64) -> Option<u64> {
    if let Some(v) = whole.checked_sub(parts) {
        return Some(v);
    }
    if parts - whole <= (whole / 50).max(SKEW_FLOOR_BYTES) {
        return Some(0);
    }
    debug!(
        cgroup = what,
        whole_bytes = whole,
        parts_bytes = parts,
        "slice memory: nested cgroups hold more than the cgroup holding them; this class's \
         memory total is not published"
    );
    None
}

/// Every named cgroup's reading by `read` (`memory.current`, in [`measure`]),
/// deepest first, so each is read after every named cgroup inside it (one
/// inside another is deeper than it).
fn read_named(named: &[Named], mut read: impl FnMut(&str) -> Option<u64>) -> BTreeMap<String, u64> {
    let mut rels: Vec<&str> = named.iter().map(|n| n.rel.as_str()).collect();
    rels.sort_by_key(|rel| std::cmp::Reverse(depth(rel)));
    rels.into_iter()
        .filter_map(|rel| Some((rel.to_string(), read(rel)?)))
        .collect()
}

/// Is a holder scope this read listed still there although its
/// `memory.current` could not be read? Then the session total would miss its
/// memory, and none is published. One that ended since the listing is gone
/// from the tree, so the total stands without it: what it still holds (page
/// cache charged to the dying cgroup) is in no named session cgroup.
fn holders_unread(root: &Path, p: &Placement, readings: &BTreeMap<String, u64>) -> bool {
    p.holders
        .iter()
        .flatten()
        .any(|h| !readings.contains_key(h) && is_cgroup(&dir_of(root, h)))
}

/// Why this read cannot publish a session total: CI work may have run, while
/// it was taken, in a session cgroup where this read does not subtract it.
/// `before` is what `ci_node` had counted before the cgroups were listed,
/// `after` what it had counted once they were read. Every reason withholds the
/// total, since the runner's own cgroup is inside the runner's unit (the
/// module docs say why); it is asked only for a read that placed the session
/// class, whose own cgroup is therefore known.
fn withheld(p: &Placement, before: DispatchCounts, after: DispatchCounts) -> BTreeSet<Why> {
    let mut why: BTreeSet<Why> = p.dispatch_parents_unknown.into_iter().collect();
    if p.dispatches.is_none() {
        why.insert(Why::DispatchDirUnlisted);
    }
    if after.started != before.started {
        why.insert(Why::DispatchStarted);
    }
    if before.uncontained_running > 0
        || after.uncontained_running > 0
        || after.uncontained_begun != before.uncontained_begun
    {
        why.insert(Why::DispatchUncontained);
    }
    why
}

/// The `memory_attribution` object for one read; empty when nothing in it was
/// measured. `holder_unread` is [`holders_unread`]'s answer for this read.
fn report(
    p: &Placement,
    named: &[Named],
    readings: &BTreeMap<String, u64>,
    before: DispatchCounts,
    after: DispatchCounts,
    holder_unread: bool,
) -> Map<String, Value> {
    let bytes_of = |rel: &str| readings.get(rel).copied();
    let forest = Forest::new(named, bytes_of);
    let mut out = Map::new();
    let mut measured = false;
    // Reasons are for a total this read placed and then withheld; an unplaced
    // class has none, and the unit and holder keys say what was not found.
    let placed = p.placed(Class::Session);
    let reasons = if placed {
        withheld(p, before, after)
    } else {
        BTreeSet::new()
    };
    if placed && reasons.is_empty() && !holder_unread {
        if let Some(&Some(total)) = forest.class_totals_by(exclusive_bytes).get(&Class::Session) {
            out.insert("session_bytes".into(), json!(total));
            measured = true;
        }
    }
    if let Some(bytes) = p.runner_unit.as_deref().and_then(bytes_of) {
        out.insert("runner_unit_bytes".into(), json!(bytes));
        measured = true;
    }
    if let Some(holders) = &p.holders {
        let read: Vec<u64> = holders.iter().filter_map(|h| bytes_of(h)).collect();
        if holders.is_empty() || !read.is_empty() {
            let sum = read.iter().fold(0_u64, |t, b| t.saturating_add(*b));
            out.insert("pty_holder_scopes_bytes".into(), json!(sum));
        }
        // No pane is a true 0, but not a reading: alone it publishes nothing.
        measured |= !read.is_empty();
        out.insert("pty_holder_scopes_measured".into(), json!(read.len()));
        out.insert("pty_holder_scopes_seen".into(), json!(holders.len()));
    }
    if !reasons.is_empty() {
        let keys: Vec<&str> = reasons.iter().map(|w| w.key()).collect();
        out.insert("withheld".into(), json!(keys));
    }
    if measured {
        out
    } else {
        Map::new()
    }
}

/// One read of `cgroup_root`'s session memory. `procfs`, `ci_cgroup_root`,
/// `memory_dirs` and `dispatches` are what the CPU split's `measure` takes;
/// `dispatches` is called before the cgroups are listed and again after their
/// `memory.current` is read.
fn measure(
    cgroup_root: &Path,
    procfs: Option<&Path>,
    ci_cgroup_root: Option<&Path>,
    memory_dirs: &[(&'static str, PathBuf)],
    dispatches: impl Fn() -> DispatchCounts,
) -> Map<String, Value> {
    let before = dispatches();
    let placement = Placement::find(cgroup_root, procfs, ci_cgroup_root, memory_dirs);
    read_and_report(cgroup_root, &placement, before, dispatches)
}

/// The rest of [`measure`] once `placement` is listed: read its cgroups and
/// report them. `before` is what `dispatches` counted before the listing.
fn read_and_report(
    cgroup_root: &Path,
    placement: &Placement,
    before: DispatchCounts,
    dispatches: impl Fn() -> DispatchCounts,
) -> Map<String, Value> {
    let named = placement.named();
    let readings = read_named(&named, |rel| read_bytes(&dir_of(cgroup_root, rel)));
    let after = dispatches();
    let holder_unread = holders_unread(cgroup_root, placement, &readings);
    report(placement, &named, &readings, before, after, holder_unread)
}

/// Add this read's `memory_attribution` to `slices` when anything in it was
/// measured. The arguments are the CPU split's `attach`'s, which this mirrors:
/// `proc_pressure` is `<procfs>/pressure`, and `QONTINUI_CI_CGROUP_ROOT` is
/// read as `ci_node` reads it.
pub(in crate::fleet::slice_telemetry) fn attach(
    cgroup_root: &Path,
    proc_pressure: &Path,
    memory_dirs: &[(&'static str, PathBuf)],
    dispatches: impl Fn() -> DispatchCounts,
    slices: &mut Map<String, Value>,
) {
    let ci_cgroup_root = std::env::var_os(CGROUP_ROOT_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let split = measure(
        cgroup_root,
        proc_pressure.parent(),
        ci_cgroup_root.as_deref(),
        memory_dirs,
        dispatches,
    );
    if !split.is_empty() {
        slices.insert(MEMORY_ATTRIBUTION_KEY.into(), Value::Object(split));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_node::cgroup::DISPATCH_DIR_PREFIX;
    use std::fs;

    const MGR: &str = "user.slice/user-1000.slice/user@1000.service";
    const GIB: u64 = 1024 * 1024 * 1024;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn unit() -> String {
        format!("{MGR}/app.slice/qontinui-runner.service")
    }

    fn holder(pane: &str) -> String {
        format!("{MGR}/app.slice/qontinui-pty-holder-{pane}-16f2c-18e4ca879535bc14.scope")
    }

    fn builds() -> String {
        format!("{MGR}/agent.slice/agent-builds.slice")
    }

    /// A fixture machine: a cgroup v2 root, and a `/proc` naming the runner's
    /// cgroup and, as uid 1000, its user.
    struct Machine {
        _dir: tempfile::TempDir,
        root: PathBuf,
        procfs: PathBuf,
    }

    impl Machine {
        fn new(runner_cgroup: Option<&str>) -> Self {
            Self::with_status(
                runner_cgroup,
                "Name:\tqontinui-runner\nUid:\t1000\t1000\t1000\t1000\n",
            )
        }

        /// As [`Machine::new`], with `status` as `/proc/self/status`.
        fn with_status(runner_cgroup: Option<&str>, status: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("cgroup");
            let procfs = dir.path().join("proc");
            write(&root, "cgroup.controllers", "cpuset cpu io memory pids\n");
            if let Some(c) = runner_cgroup {
                write(&procfs, "self/cgroup", &format!("0::/{c}\n"));
            }
            write(&procfs, "self/status", status);
            Self {
                _dir: dir,
                root,
                procfs,
            }
        }

        /// A cgroup at `rel` (it has a `cpu.stat`, which is how the placement
        /// tells a cgroup), holding `bytes` when that is given.
        fn cgroup(&self, rel: &str, bytes: Option<u64>) {
            write(&self.root, &format!("{rel}/cpu.stat"), "usage_usec 0\n");
            if let Some(b) = bytes {
                write(
                    &self.root,
                    &format!("{rel}/memory.current"),
                    &format!("{b}\n"),
                );
            }
        }

        fn measure_with(&self, dispatches: impl Fn() -> DispatchCounts) -> Map<String, Value> {
            self.measure_in(None, dispatches)
        }

        /// A read with `QONTINUI_CI_CGROUP_ROOT` at `ci_cgroup_root` (under
        /// the fixture's root).
        fn measure_in(
            &self,
            ci_cgroup_root: Option<&str>,
            dispatches: impl Fn() -> DispatchCounts,
        ) -> Map<String, Value> {
            let dirs = crate::fleet::slice_telemetry::read_slice_reads(&self.root).dirs;
            let ci_root = ci_cgroup_root.map(|rel| self.root.join(rel));
            measure(
                &self.root,
                Some(&self.procfs),
                ci_root.as_deref(),
                &dirs,
                dispatches,
            )
        }

        fn placement(&self) -> Placement {
            let dirs = crate::fleet::slice_telemetry::read_slice_reads(&self.root).dirs;
            Placement::find(&self.root, Some(&self.procfs), None, &dirs)
        }

        fn measure(&self) -> Map<String, Value> {
            self.measure_with(DispatchCounts::default)
        }
    }

    /// The runner's unit, two panes and the builds slice: Phase 0's `b_full`.
    fn sessions_machine() -> Machine {
        let m = Machine::new(Some(&unit()));
        m.cgroup(&unit(), Some(2 * GIB));
        m.cgroup(&holder("a"), Some(GIB / 2));
        m.cgroup(&holder("b"), Some(GIB / 4));
        m.cgroup(&builds(), Some(6 * GIB));
        m
    }

    /// The session total is the runner's unit, every holder scope and
    /// `agent-builds.slice`, from the first read on (a level needs no
    /// previous read).
    #[test]
    fn session_memory_is_the_unit_the_holders_and_the_builds_slice() {
        let split = sessions_machine().measure();
        assert_eq!(
            split["session_bytes"],
            json!(2 * GIB + GIB / 2 + GIB / 4 + 6 * GIB)
        );
        assert_eq!(split["runner_unit_bytes"], json!(2 * GIB));
        assert_eq!(split["pty_holder_scopes_bytes"], json!(GIB / 2 + GIB / 4));
        assert_eq!(split["pty_holder_scopes_seen"], json!(2));
        assert_eq!(split["pty_holder_scopes_measured"], json!(2));
        assert!(split.get("withheld").is_none(), "{split:?}");
    }

    /// The runner's unit inside `qontinui-runner.slice` is counted once: the
    /// slice's reading already holds it. Fails when a nested session cgroup
    /// is added on top of the one holding it.
    #[test]
    fn the_runner_unit_inside_its_slice_is_not_counted_twice() {
        let slice = format!("{MGR}/qontinui.slice/qontinui-runner.slice");
        let unit_in_slice = format!("{slice}/qontinui-runner.service");
        let m = Machine::new(Some(&unit_in_slice));
        m.cgroup(&slice, Some(3 * GIB));
        m.cgroup(&unit_in_slice, Some(2 * GIB));
        m.cgroup(&holder("a"), Some(GIB));
        let split = m.measure();
        assert_eq!(
            split["session_bytes"],
            json!(3 * GIB + GIB),
            "the slice (unit included) and the pane, not slice + unit + pane"
        );
        assert_eq!(split["runner_unit_bytes"], json!(2 * GIB));
    }

    /// A CI dispatch cgroup inside the runner's unit is CI memory: it is
    /// subtracted from the unit, and a CI slice adds nothing to sessions.
    #[test]
    fn a_dispatch_cgroup_in_the_unit_and_the_ci_slice_are_not_session_memory() {
        let m = sessions_machine();
        let dispatch = format!("{}/{}job-7", unit(), DISPATCH_DIR_PREFIX);
        m.cgroup(&dispatch, Some(GIB / 2));
        m.cgroup("ci.slice", Some(40 * GIB));
        m.cgroup("ci.slice/ci-runners.slice", Some(30 * GIB));
        let split = m.measure();
        assert_eq!(
            split["session_bytes"],
            json!((2 * GIB - GIB / 2) + GIB / 2 + GIB / 4 + 6 * GIB)
        );
        assert_eq!(
            split["runner_unit_bytes"],
            json!(2 * GIB),
            "the unit's own reading, its dispatch cgroup included"
        );
    }

    /// While `ci_node` runs CI work in the runner's own cgroup, or counts a
    /// dispatch during the read, the session total could hold CI memory: it is
    /// withheld and the reason named, and the unit's own reading still stands.
    #[test]
    fn ci_work_in_the_runners_cgroup_withholds_the_session_total() {
        let m = sessions_machine();
        let running = || DispatchCounts {
            uncontained_running: 1,
            ..DispatchCounts::default()
        };
        let split = m.measure_with(running);
        assert!(split.get("session_bytes").is_none(), "{split:?}");
        assert_eq!(split["withheld"], json!(["dispatch_uncontained"]));
        assert_eq!(split["runner_unit_bytes"], json!(2 * GIB));

        // A dispatch counted between the listing and the reads.
        let calls = std::cell::Cell::new(0_u64);
        let started = || {
            calls.set(calls.get() + 1);
            DispatchCounts {
                started: calls.get(),
                ..DispatchCounts::default()
            }
        };
        let split = m.measure_with(started);
        assert!(split.get("session_bytes").is_none(), "{split:?}");
        assert_eq!(split["withheld"], json!(["dispatch_started"]));
    }

    /// `dispatches` brackets the whole read: its first count is taken before
    /// the cgroups are listed and its second after their `memory.current` is
    /// read, so a dispatch that began anywhere in between is seen. Fails when
    /// the first count moves after the listing (the pane made by the first
    /// call would be counted) or the second before the reads (the unit would
    /// publish the reading the second call wrote).
    #[test]
    fn the_dispatch_counts_bracket_the_listing_and_the_reads() {
        let m = sessions_machine();
        let calls = std::cell::Cell::new(0_u32);
        let split = m.measure_with(|| {
            calls.set(calls.get() + 1);
            match calls.get() {
                1 => m.cgroup(&holder("c"), Some(GIB / 8)),
                2 => write(
                    &m.root,
                    &format!("{}/memory.current", unit()),
                    &format!("{}\n", 9 * GIB),
                ),
                n => panic!("dispatches called {n} times"),
            }
            DispatchCounts::default()
        });
        assert_eq!(calls.get(), 2);
        assert_eq!(split["pty_holder_scopes_seen"], json!(3), "{split:?}");
        assert_eq!(split["pty_holder_scopes_measured"], json!(3), "{split:?}");
        assert_eq!(split["runner_unit_bytes"], json!(2 * GIB), "{split:?}");
        assert_eq!(
            split["session_bytes"],
            json!(2 * GIB + GIB / 2 + GIB / 4 + GIB / 8 + 6 * GIB),
            "{split:?}"
        );
    }

    /// With `QONTINUI_CI_CGROUP_ROOT` naming a directory under a CI slice a
    /// counted dispatch still withholds the total: `ci_node` runs its own work
    /// for every dispatch in the runner's own cgroup, which is inside the unit.
    /// And with nothing counted, the total stands, the CI root's dispatch
    /// cgroup left out of it.
    #[test]
    fn a_ci_cgroup_root_under_a_ci_slice_still_withholds_on_a_counted_dispatch() {
        let m = sessions_machine();
        m.cgroup("ci.slice", Some(5 * GIB));
        let ci_root = "ci.slice/ci-dispatch.slice";
        m.cgroup(ci_root, Some(4 * GIB));
        m.cgroup(
            &format!("{ci_root}/{DISPATCH_DIR_PREFIX}job-9"),
            Some(4 * GIB),
        );
        let calls = std::cell::Cell::new(0_u64);
        let started = || {
            calls.set(calls.get() + 1);
            DispatchCounts {
                started: calls.get(),
                ..DispatchCounts::default()
            }
        };
        let split = m.measure_in(Some(ci_root), started);
        assert!(split.get("session_bytes").is_none(), "{split:?}");
        assert_eq!(split["withheld"], json!(["dispatch_started"]));

        let split = m.measure_in(Some(ci_root), DispatchCounts::default);
        assert_eq!(
            split["session_bytes"],
            json!(2 * GIB + GIB / 2 + GIB / 4 + 6 * GIB),
            "{split:?}"
        );
        assert!(split.get("withheld").is_none(), "{split:?}");
    }

    /// Every other reason withholds the total too, each named alone: a
    /// `QONTINUI_CI_CGROUP_ROOT` that is not followed, an uncontained run that
    /// began and ended within the read (no run alive at either count), and a
    /// dispatch parent that could not be listed. The unit's own reading
    /// stands in each.
    #[cfg(unix)]
    #[test]
    fn every_other_reason_withholds_the_session_total() {
        let m = sessions_machine();
        let only = |split: &Map<String, Value>, why: &str| {
            assert!(split.get("session_bytes").is_none(), "{why}: {split:?}");
            assert_eq!(split["withheld"], json!([why]), "{split:?}");
            assert_eq!(split["runner_unit_bytes"], json!(2 * GIB), "{why}");
        };
        // A CI cgroup root named through `..` is not followed.
        only(
            &m.measure_in(Some("../elsewhere"), DispatchCounts::default),
            "ci_cgroup_root_not_followed",
        );

        // A run begun after the first count and over by the second.
        let calls = std::cell::Cell::new(0_u64);
        let begun = || {
            calls.set(calls.get() + 1);
            DispatchCounts {
                uncontained_begun: calls.get() - 1,
                ..DispatchCounts::default()
            }
        };
        only(&m.measure_with(begun), "dispatch_uncontained");

        // The runner's unit, where `ci_node` makes its dispatch cgroups, could
        // not be listed; its own files are still read by name.
        let Some(_restore) = super::super::tests::unlistable(&m.root.join(unit())) else {
            return;
        };
        only(&m.measure(), "dispatch_dir_unlisted");
    }

    /// Without the runner's unit, or without the holder scopes, there is no
    /// session total (and no reason: the keys show what was not found).
    #[test]
    fn without_the_unit_or_the_holders_there_is_no_session_total() {
        // No `/proc/self/cgroup`: no unit, and no session total.
        let m = Machine::new(None);
        m.cgroup(&unit(), Some(2 * GIB));
        m.cgroup(&holder("a"), Some(GIB));
        let split = m.measure();
        assert!(split.get("session_bytes").is_none(), "{split:?}");
        assert!(split.get("runner_unit_bytes").is_none());
        assert!(split.get("withheld").is_none());
        // The holders are still found through the uid's manager and summed.
        assert_eq!(split["pty_holder_scopes_bytes"], json!(GIB));

        // A unit whose memory is unreadable (no memory controller there).
        let m = Machine::new(Some(&unit()));
        m.cgroup(&unit(), None);
        m.cgroup(&holder("a"), Some(GIB));
        let split = m.measure();
        assert!(split.get("session_bytes").is_none(), "{split:?}");
        assert!(split.get("runner_unit_bytes").is_none());

        // A system service with no `Uid:` line: the unit is found, but no
        // user manager is known, so the holders are not, and the unit alone
        // is not a session total.
        let system_unit = "system.slice/qontinui-runner.service";
        let m = Machine::with_status(Some(system_unit), "Name:\tqontinui-runner\n");
        m.cgroup(system_unit, Some(2 * GIB));
        m.cgroup(&holder("a"), Some(GIB));
        let split = m.measure();
        assert!(split.get("session_bytes").is_none(), "{split:?}");
        assert_eq!(split["runner_unit_bytes"], json!(2 * GIB));
        assert!(split.get("pty_holder_scopes_seen").is_none(), "{split:?}");
        assert!(split.get("withheld").is_none(), "{split:?}");
    }

    /// A holder scope still there whose memory could not be read leaves no
    /// session total, since the total would miss it; one that ended between
    /// the listing and the reads is left out, and the total stands.
    #[test]
    fn an_unread_holder_still_there_withholds_and_an_ended_one_is_left_out() {
        let m = sessions_machine();
        m.cgroup(&holder("c"), None);
        let split = m.measure();
        assert!(split.get("session_bytes").is_none(), "{split:?}");
        assert!(split.get("withheld").is_none(), "{split:?}");
        assert_eq!(split["pty_holder_scopes_seen"], json!(3));
        assert_eq!(split["pty_holder_scopes_measured"], json!(2));
        assert_eq!(split["pty_holder_scopes_bytes"], json!(GIB / 2 + GIB / 4));

        // The same scope ending after the listing: it is gone from the tree,
        // and the total stands without it.
        let placement = m.placement();
        assert_eq!(placement.holders.as_ref().map(Vec::len), Some(3));
        fs::remove_dir_all(m.root.join(holder("c"))).unwrap();
        let split = read_and_report(
            &m.root,
            &placement,
            DispatchCounts::default(),
            DispatchCounts::default,
        );
        assert_eq!(
            split["session_bytes"],
            json!(2 * GIB + GIB / 2 + GIB / 4 + 6 * GIB),
            "{split:?}"
        );
        assert_eq!(split["pty_holder_scopes_seen"], json!(3));
        assert_eq!(split["pty_holder_scopes_measured"], json!(2));
    }

    /// A read that measured nothing adds no key at all: never an object of
    /// zeros, never an empty one.
    #[test]
    fn a_read_that_measured_nothing_publishes_nothing() {
        let m = Machine::new(None);
        let mut slices = Map::new();
        let pressure = m.procfs.join("pressure");
        // `attach` reads `QONTINUI_CI_CGROUP_ROOT`; hold the lock every test
        // that moves it holds, with it unset.
        let _g = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[CGROUP_ROOT_ENV]);
        std::env::remove_var(CGROUP_ROOT_ENV);
        attach(
            &m.root,
            &pressure,
            &[],
            DispatchCounts::default,
            &mut slices,
        );
        assert!(slices.is_empty(), "{slices:?}");

        // And with something to measure, the key is there, a non-slice key.
        let m = sessions_machine();
        let pressure = m.procfs.join("pressure");
        attach(
            &m.root,
            &pressure,
            &[],
            DispatchCounts::default,
            &mut slices,
        );
        assert!(slices[MEMORY_ATTRIBUTION_KEY]["session_bytes"].is_u64());
        assert!(!MEMORY_ATTRIBUTION_KEY.ends_with(".slice"));
    }

    /// Children are read before the cgroup holding them, so a child that
    /// shrank in between can read above its parent: within the skew bound the
    /// parent's own share is 0, past it nothing is published.
    #[test]
    fn read_skew_is_bounded_and_past_the_bound_nothing_is_published() {
        assert_eq!(exclusive_bytes("u", 10 * GIB, 4 * GIB), Some(6 * GIB));
        assert_eq!(exclusive_bytes("u", 100, 100 + SKEW_FLOOR_BYTES), Some(0));
        assert_eq!(exclusive_bytes("u", 100, 101 + SKEW_FLOOR_BYTES), None);
        // 2% of a large whole is the bound there.
        assert_eq!(exclusive_bytes("u", 50 * GIB, 51 * GIB), Some(0));
        assert_eq!(exclusive_bytes("u", 50 * GIB, 52 * GIB), None);
    }

    /// The bound as a read sees it: a dispatch cgroup reading a little above
    /// the unit holding it leaves the unit's own share 0 and the total
    /// published; one reading far above it publishes no session total, while
    /// the unit's own reading still stands.
    #[test]
    fn a_dispatch_read_past_the_skew_bound_publishes_no_session_total() {
        let m = sessions_machine();
        let dispatch = format!("{}/{}job-7", unit(), DISPATCH_DIR_PREFIX);
        // 2% of the unit's 2 GiB is about 41 MiB; 30 MiB above is skew.
        m.cgroup(&dispatch, Some(2 * GIB + 30 * 1024 * 1024));
        let split = m.measure();
        assert_eq!(
            split["session_bytes"],
            json!(GIB / 2 + GIB / 4 + 6 * GIB),
            "{split:?}"
        );

        m.cgroup(&dispatch, Some(3 * GIB));
        let split = m.measure();
        assert!(split.get("session_bytes").is_none(), "{split:?}");
        assert!(split.get("withheld").is_none(), "{split:?}");
        assert_eq!(split["runner_unit_bytes"], json!(2 * GIB));
    }

    /// Every named cgroup is read after the named cgroups inside it, whatever
    /// order the placement names them in: a parent's reading then holds what
    /// its children held when they were read.
    #[test]
    fn every_cgroup_is_read_after_the_cgroups_inside_it() {
        let named = |rel: &str, class: Class| Named {
            rel: rel.to_string(),
            class,
            churns: false,
        };
        // Parents first, the worst order to be handed.
        let slice = format!("{MGR}/qontinui.slice/qontinui-runner.slice");
        let unit_in_slice = format!("{slice}/qontinui-runner.service");
        let dispatch = format!("{unit_in_slice}/{DISPATCH_DIR_PREFIX}job-1");
        let pane = holder("a");
        let given = vec![
            named(&slice, Class::Session),
            named(&unit_in_slice, Class::Session),
            named(&dispatch, Class::Ci),
            named(&pane, Class::Session),
        ];
        let mut asked: Vec<String> = Vec::new();
        let readings = read_named(&given, |rel| {
            asked.push(rel.to_string());
            Some(1)
        });
        assert_eq!(asked, vec![dispatch, unit_in_slice, slice, pane]);
        assert_eq!(readings.len(), 4, "each cgroup read once");
    }
}
