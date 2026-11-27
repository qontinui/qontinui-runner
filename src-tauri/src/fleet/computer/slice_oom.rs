//! OOM kills inside the watched slices (plan
//! `2026-11-27-devops-oom-kills-inside-agent-build-slices-carry-no-victim`).
//!
//! Before this module an OOM kill that no WATCHED unit accounted for reached
//! coord as one `oom_kill` event with `victim_unit: null` — including a kill
//! inside `agent-builds.slice`, which is where the fleet caps builds. This
//! names the narrowest container the collector can PROVE, from evidence an
//! unprivileged process can read. A level that is not proven is `null`; a
//! plausible guess is never a value.
//!
//! ## The layers
//!
//! * **A — slice counters (the count authority).** Each tick reads every
//!   [`slice_telemetry::WATCHED_SLICES`] entry's `memory.events` `oom_kill`
//!   through the existing slice reader ([`slice_telemetry::read_slice_reads`]).
//!   cgroup v2's `memory.events` is hierarchical, so a slice's delta counts the
//!   kills in every descendant. Every `oom_kill` event inside a watched slice
//!   carries `victim_slice`, a watched unit's own event included.
//! * **B — live children (exact, file-only).** Every resolved slice's direct
//!   child cgroups (`*.scope`, `*.service`, nested `*.slice`) are listed and
//!   their own `oom_kill` counters read. A child whose counter moved is the
//!   victim (`attribution: "cgroup_memory_events"`, the watched-unit arm's
//!   word). Watched units are skipped (they have their own event) and do not
//!   count toward the [`CHILD_CAP`] cap.
//! * **C — the user journal** (`journal_oom`), for a child that died before
//!   the tick could list it: only for a slice under THIS user's manager, only
//!   for a unit last tick's child listing proves was inside the slice, at
//!   most as many entries as the counters left unexplained
//!   (`attribution: "user_journal_unit_oom"`, `count: 1` each). The read is
//!   decided by [`journal_window`] — pure, over the previous state — before
//!   the derive, and forked only then. Every slice event says what Layer C
//!   could do for it in `journal`: `read` (consulted), `unreadable` (needed,
//!   but no readable journal holds the slice's messages), `not_needed`
//!   (nothing it could add — the counters named the kill) or `no_proof` (after
//!   a reboot, where no listing from this boot proves a placement).
//! * **Kernel records** (Phase 3, `journal_oom`), where the kernel log is
//!   readable: a record whose kernel-written `task_memcg` lies under the slice
//!   places what would be the remainder (`attribution: "kernel_oom_record"`,
//!   `victim_unit` = the path component under the slice), and an exact
//!   `task_memcg` match names any slice event's `victim_comm`
//!   (`victim_comm_source`: `kernel_log` / `not_found` / `unreadable`).
//! * Whatever the slice delta has left after the watched units, B, C and the
//!   kernel records is ONE
//!   slice-level event with `victim_unit: null` — "inside this slice, child
//!   not placed".
//!
//! ## Arithmetic
//!
//! Let `V` be the `/proc/vmstat` delta and `U` the watched units' kills. A
//! watched unit inside a slice is subtracted from that slice's delta, by its
//! `ControlGroup=` (this tick's, else the last non-empty one persisted), so a
//! kill is never counted twice. The slice files and `/proc/vmstat` are read
//! at different instants, so the slice events are clamped to `V - U`
//! (`count_clamped: true`), trimming the remainder first and B last, never a
//! watched-unit event; a child total over its slice's own delta is clamped the
//! same way. Slice and child deltas are consumed only on a tick whose `V` is
//! computable — otherwise their baselines are kept, so a kill is not counted
//! by a slice now and by the residual next tick. A counter that went down (the
//! cgroup was recreated) or a slice found at a different path rebaselines and
//! emits nothing; first sight is a baseline; after a reboot a slice's baseline
//! is 0, its children are cleared, and the attribution is
//! `slice_memory_events_since_boot`.
//!
//! ## Linux only
//!
//! The read is `/sys/fs/cgroup`. Elsewhere [`collect`] reads nothing and every
//! `oom_kill` event says `slices: "not_supported"`. The read runs in one
//! tracked blocking body, from the reporter's own tick, before the derive.

// The readers are exercised by tests on every OS but called in production only
// on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::events::{self, event_id, rfc3339, ComputerEvent, ComputerObs, ObsInput};
use super::journal_oom::{
    self, boot_key, JournalRead, Journalctl, KernelOomRecord, KernelRead, UnitOomEntry,
};
use crate::fleet::slice_telemetry;

/// Children tracked per slice. Past it a child is untracked, its kills fall
/// to the slice remainder, and the event says `children_truncated: true`. It
/// keeps the persisted observer state bounded.
pub(crate) const CHILD_CAP: usize = 64;

/// The persisted observation of one watched slice (`ComputerObs.slices`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct SliceObs {
    /// The slice's cgroup path relative to `/sys/fs/cgroup` (leading `/`). A
    /// slice found at a different path rebaselines rather than producing a
    /// delta between two different cgroups.
    pub(crate) cgroup: String,
    /// Its `memory.events` `oom_kill` baseline.
    pub(crate) oom_kill: u64,
    /// When that baseline was read.
    pub(crate) observed_at: DateTime<Utc>,
    /// Each tracked direct child's `oom_kill` baseline, by name.
    #[serde(default)]
    pub(crate) children: BTreeMap<String, u64>,
    /// The units Layer C or a kernel record named on the tick that took this
    /// baseline, with the time of the entry that named each (µs). A kill is
    /// seen by BOTH sources — the kernel's line, then systemd's a moment
    /// later — and the next window starts at this read, so without this the
    /// other source's line for the same kill could name a second one.
    #[serde(default)]
    pub(crate) named: BTreeMap<String, u64>,
    /// What the vmstat clamp kept back in this baseline (`oom_kill` below
    /// the read). Carried ONE tick only: still uncovered by vmstat the next
    /// tick, it is dropped rather than carried again (an excess no vmstat
    /// lag explains, such as a watched unit with no known cgroup, would
    /// otherwise stay forever and be counted against a later kill).
    #[serde(default)]
    pub(crate) carried: u64,
}

/// How far past the counter read a journal entry or kernel record may be
/// stamped and still name a kill that read counted: systemd logs a kill a
/// moment after the kernel counts it. The reporter WAITS until then before
/// it forks (`collect_from`), so the window's end is real.
pub(crate) const JOURNAL_LOOKAHEAD_SECS: i64 = 3;

/// Two lines naming one unit this close together are the same kill, seen by
/// the kernel and by systemd.
const SAME_KILL_SECS: u64 = 5;

/// The upper bound of a tick's journal window: its counter read plus
/// [`JOURNAL_LOOKAHEAD_SECS`]. PURE.
fn window_end(read_at: DateTime<Utc>) -> DateTime<Utc> {
    read_at + chrono::Duration::seconds(JOURNAL_LOOKAHEAD_SECS)
}

/// How long to wait so `lookahead` has passed since the counter read, both
/// taken on the MONOTONIC clock (a wall-clock step cannot move them): never
/// more than `lookahead` itself, and nothing once it has passed. PURE.
pub(crate) fn lookahead_wait(
    read_at: std::time::Instant,
    now: std::time::Instant,
    lookahead: Duration,
) -> Duration {
    (read_at + lookahead)
        .saturating_duration_since(now)
        .min(lookahead)
}

/// Whether last tick already named `unit` for the kill a line at `at` (µs)
/// reports, from either source. PURE.
fn named_before(plan: &SlicePlan<'_>, unit: &str, at: u64) -> bool {
    plan.named_before
        .and_then(|n| n.get(unit))
        .is_some_and(|was| was.abs_diff(at) <= SAME_KILL_SECS * 1_000_000)
}

/// Whether this tick read the watched slices — the `slices` detail field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SliceSource {
    /// The unified cgroup v2 hierarchy was read.
    Read,
    /// The read did not finish (its blocking body failed).
    #[default]
    Unreadable,
    /// No unified cgroup v2 hierarchy here.
    NotSupported,
}

impl SliceSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SliceSource::Read => "read",
            SliceSource::Unreadable => "unreadable",
            SliceSource::NotSupported => "not_supported",
        }
    }
}

/// `slices` on an event of a lane that never reads slices.
const SLICES_NOT_SUPPORTED: &str = "not_supported";

/// One tick's read of the watched slices.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct SliceOomRead {
    pub(crate) source: SliceSource,
    /// Taken immediately BEFORE the counters were read, so no kill the next
    /// tick's delta counts can be older than the next baseline's time.
    pub(crate) read_at: DateTime<Utc>,
    /// The runner's effective uid: `journalctl --user` reads only its own
    /// user manager's journal.
    pub(crate) euid: Option<u32>,
    /// Every watched slice whose `oom_kill` counter was read, in
    /// [`slice_telemetry::WATCHED_SLICES`] order.
    pub(crate) slices: Vec<SliceRead>,
}

/// One watched slice as read this tick.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SliceRead {
    pub(crate) name: String,
    /// Relative to `/sys/fs/cgroup`, with a leading `/`.
    pub(crate) cgroup: String,
    pub(crate) oom_kill: u64,
    /// The kept direct children ([`select_children`]), in name order.
    pub(crate) children: Vec<ChildRead>,
    /// The listing had more (non-watched) children than [`CHILD_CAP`].
    pub(crate) children_truncated: bool,
}

/// One kept direct child of a watched slice.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChildRead {
    pub(crate) name: String,
    /// `None` when its `memory.events` could not be read (it went away
    /// between the listing and the read).
    pub(crate) oom_kill: Option<u64>,
}

/// The host's OOM-placement reads for one tick, taken before the derive.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct OomReads {
    /// `None` = not read on this lane (non-Linux).
    pub(crate) slices: Option<SliceOomRead>,
    /// Layer C's user-journal read; `None` = no window needed one.
    pub(crate) journal: Option<JournalRead>,
    /// Phase 3's kernel-log read; `None` = no window needed one.
    pub(crate) kernel: Option<KernelRead>,
}

/// A watched unit's OOM kill this tick, as the slice arithmetic subtracts it
/// from the slice (and child) that contains it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UnitKill {
    pub(crate) unit: String,
    pub(crate) count: u64,
    /// This tick's `ControlGroup=`, else the last non-empty one persisted.
    pub(crate) control_group: Option<String>,
}

// ---------------------------------------------------------------------------
// The read
// ---------------------------------------------------------------------------

/// `path` lies strictly inside the cgroup `ancestor`. PURE.
pub(crate) fn is_under(path: &str, ancestor: &str) -> bool {
    path.len() > ancestor.len()
        && path.starts_with(ancestor)
        && path.as_bytes().get(ancestor.len()) == Some(&b'/')
}

/// The kept children of one slice: watched units are dropped (and do not
/// count toward the cap); of the rest, the ones `tracked` last tick come first
/// and the others fill in NAME order up to `cap`, so an unspecified readdir
/// order cannot change the kept set from tick to tick. Returns the kept names
/// in name order, and whether anything was left out. PURE.
pub(crate) fn select_children(
    listed: impl IntoIterator<Item = String>,
    tracked: &BTreeSet<String>,
    watched: &BTreeSet<String>,
    cap: usize,
) -> (Vec<String>, bool) {
    let candidates: BTreeSet<String> = listed
        .into_iter()
        .filter(|n| !watched.contains(n))
        .collect();
    let (old, new): (Vec<&String>, Vec<&String>) =
        candidates.iter().partition(|n| tracked.contains(*n));
    let kept: BTreeSet<String> = old
        .into_iter()
        .chain(new)
        .take(cap)
        .cloned()
        .collect();
    let truncated = candidates.len() > kept.len();
    (kept.into_iter().collect(), truncated)
}

/// The direct child cgroups of `dir` that are units: `*.scope`, `*.service`,
/// `*.slice`. An unlistable directory has none.
fn child_names(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| [".scope", ".service", ".slice"].iter().any(|s| n.ends_with(s)))
        .collect()
}

/// `dir` as a cgroup path relative to `root`, with a leading `/` (the shape
/// of systemd's `ControlGroup=` and the kernel's `task_memcg`). `None` when
/// it is not under `root` or not UTF-8.
fn cgroup_rel(root: &Path, dir: &Path) -> Option<String> {
    let rel = dir.strip_prefix(root).ok()?;
    let mut out = String::new();
    for c in rel.components() {
        out.push('/');
        out.push_str(c.as_os_str().to_str()?);
    }
    (!out.is_empty()).then_some(out)
}

/// The uid of the user manager (`user@<uid>.service`) a cgroup lies under;
/// `None` under the system root. PURE.
pub(crate) fn manager_uid(cgroup: &str) -> Option<u32> {
    cgroup.split('/').find_map(|c| {
        c.strip_prefix("user@")
            .and_then(|r| r.strip_suffix(".service"))
            .and_then(|n| n.parse().ok())
    })
}

/// Read every watched slice's `oom_kill` counter and its kept children's
/// under `root` (a `/sys/fs/cgroup`-shaped tree). `tracked` is each slice's
/// child set from the previous tick, `watched` the watched unit names (never
/// listed as children). Plain `std::fs`, so the fixture tests run anywhere.
pub(crate) fn read_from(
    root: &Path,
    tracked: &BTreeMap<String, BTreeSet<String>>,
    watched: &BTreeSet<String>,
    euid: Option<u32>,
    read_at: DateTime<Utc>,
) -> SliceOomRead {
    if !root.join("cgroup.controllers").is_file() {
        return SliceOomRead {
            source: SliceSource::NotSupported,
            read_at,
            euid,
            slices: Vec::new(),
        };
    }
    let reads = slice_telemetry::read_slice_reads(root);
    let none = BTreeSet::new();
    let mut slices = Vec::new();
    for (name, dir) in &reads.dirs {
        let Some(oom_kill) = reads
            .memory
            .get(*name)
            .and_then(|m| m.get("events"))
            .and_then(|e| e.get("oom_kill"))
            .and_then(serde_json::Value::as_u64)
        else {
            continue;
        };
        let Some(cgroup) = cgroup_rel(root, dir) else {
            continue;
        };
        let (kept, children_truncated) = select_children(
            child_names(dir),
            tracked.get(*name).unwrap_or(&none),
            watched,
            CHILD_CAP,
        );
        let children = kept
            .into_iter()
            .map(|n| ChildRead {
                oom_kill: std::fs::read_to_string(dir.join(&n).join("memory.events"))
                    .ok()
                    .and_then(|t| crate::fleet::host_axes::parse_memory_events_oom_kill(&t)),
                name: n,
            })
            .collect();
        slices.push(SliceRead {
            name: name.to_string(),
            cgroup,
            oom_kill,
            children,
            children_truncated,
        });
    }
    SliceOomRead {
        source: SliceSource::Read,
        read_at,
        euid,
        slices,
    }
}

/// Where one tick's OOM-placement reads come from. Production reads the live
/// tree and forks the real `journalctl` ([`Sources::live`]); a test passes a
/// fixture tree and a fake binary, so the whole path from the window to the
/// derive input runs in a test.
#[derive(Debug, Clone)]
pub(crate) struct Sources {
    /// A `/sys/fs/cgroup`-shaped tree.
    pub(crate) cgroup_root: PathBuf,
    /// The runner's effective uid.
    pub(crate) euid: Option<u32>,
    pub(crate) journalctl: Journalctl,
    /// One deadline for every journal read of the tick, together.
    pub(crate) deadline: Duration,
    /// How long after the counter read to wait before forking, so the
    /// window's end ([`JOURNAL_LOOKAHEAD_SECS`]) has happened.
    pub(crate) lookahead: Duration,
}

impl Sources {
    /// The live sources: `/sys/fs/cgroup`, this process's euid, `journalctl`
    /// from `PATH`, [`journal_oom::READ_TIMEOUT`].
    #[cfg(target_os = "linux")]
    pub(crate) fn live() -> Self {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        Self {
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
            euid: Some(euid),
            journalctl: Journalctl::live(),
            deadline: journal_oom::READ_TIMEOUT,
            lookahead: Duration::from_secs(JOURNAL_LOOKAHEAD_SECS.unsigned_abs()),
        }
    }
}

/// This tick's OOM-placement reads for the host, against `prev` (its last
/// observation) and `input` (this tick's, without any reads yet). Linux only;
/// elsewhere nothing is read. Best-effort: a failed read is `unreadable`,
/// never a stalled tick.
pub(crate) async fn collect(prev: Option<&ComputerObs>, input: &ObsInput<'_>) -> OomReads {
    #[cfg(target_os = "linux")]
    {
        collect_from(prev, input, &Sources::live()).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (prev, input);
        OomReads::default()
    }
}

/// [`collect`] against explicit [`Sources`]: the slice read in one tracked
/// blocking body, then — only when [`journal_window`] asks, i.e. on a tick
/// with a slice kill to place — a wait until the read plus the lookahead
/// ([`lookahead_wait`]), then the journal reads, concurrently, under the
/// sources' one deadline.
///
/// `Reporter::tick` is awaited INLINE in `spawn_budget_republisher`'s loop, so
/// a kill tick adds at most 3 s + 5 s (the lookahead plus the journal
/// deadline) to that loop — to the CI-node capacity refresh and the budget
/// re-assert after it. The loop's cadence is the resource-sample interval,
/// 30 s ± 20 % by default and never under 10 s, and kill ticks are rare, so
/// the wait stays inline rather than taking a task of its own (which the
/// reporter's gate forbids). The sleep is async: it holds no thread.
pub(crate) async fn collect_from(
    prev: Option<&ComputerObs>,
    input: &ObsInput<'_>,
    src: &Sources,
) -> OomReads {
    use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
    let tracked: BTreeMap<String, BTreeSet<String>> = prev
        .map(|p| {
            p.slices
                .iter()
                .map(|(n, s)| (n.clone(), s.children.keys().cloned().collect()))
                .collect()
        })
        .unwrap_or_default();
    let watched: BTreeSet<String> = input
        .services
        .unwrap_or(&[])
        .iter()
        .map(|w| w.row.unit.clone())
        .collect();
    let (root, euid) = (src.cgroup_root.clone(), src.euid);
    // The read's monotonic instant, taken beside its wall-clock `read_at`:
    // the wait below is measured on it.
    let (read_mono, slices) = spawn_blocking_tracked(move || {
        let at = std::time::Instant::now();
        (at, read_from(&root, &tracked, &watched, euid, Utc::now()))
    })
    .await
    .unwrap_or_else(|_| {
        (
            std::time::Instant::now(),
            SliceOomRead {
                source: SliceSource::Unreadable,
                read_at: Utc::now(),
                euid,
                slices: Vec::new(),
            },
        )
    });
    let (journal, kernel) = match journal_window(prev, input, &slices) {
        Some(w) => {
            let wait = lookahead_wait(read_mono, std::time::Instant::now(), src.lookahead);
            tokio::time::sleep_until(tokio::time::Instant::now() + wait).await;
            journal_oom::read_all(
                &src.journalctl,
                w.since,
                w.until,
                w.user,
                w.kernel,
                src.deadline,
            )
            .await
        }
        None => (None, None),
    };
    OomReads {
        slices: Some(slices),
        journal,
        kernel,
    }
}

// ---------------------------------------------------------------------------
// The arithmetic
// ---------------------------------------------------------------------------

/// A live child whose own counter moved (Layer B).
#[derive(Debug, Clone, PartialEq)]
struct ChildKill {
    child: String,
    count: u64,
    /// The child's baseline the delta was taken from.
    was: u64,
    /// The child's counter now.
    counter: u64,
}

impl ChildKill {
    /// The event id's key: the counter RANGE, so a part carried to the next
    /// tick (its baseline kept back by a clamp) is a different event there.
    fn key(&self) -> String {
        format!("{}..{}", self.was, self.counter)
    }
}

/// One slice's attribution for this tick, before the vmstat clamp.
struct SlicePlan<'a> {
    read: &'a SliceRead,
    /// `Some(baseline)` when this tick consumes a delta; `None` for a first
    /// sight, a moved path or a counter that went down (a rebaseline).
    base: Option<u64>,
    /// What the slice delta has left after the watched units inside it.
    left: u64,
    /// Layer B, already clamped to `left`.
    children: Vec<ChildKill>,
    /// That clamp fired.
    clamped: bool,
    /// The consumed baseline's time — where Layer C's window starts.
    since: Option<DateTime<Utc>>,
    /// Last tick's child listing, the proof of placement Layer C needs;
    /// `None` after a reboot (no proof within this boot) or a rebaseline.
    listing: Option<&'a BTreeMap<String, u64>>,
    /// What last tick's entries stamped past its read already named.
    named_before: Option<&'a BTreeMap<String, u64>>,
    /// What the vmstat clamp kept back in the previous baseline.
    carried_before: u64,
    /// The next tick's baseline.
    next: SliceObs,
}

impl SlicePlan<'_> {
    /// What the counters left unexplained after the watched units and B.
    fn unnamed(&self) -> u64 {
        self.left
            .saturating_sub(self.children.iter().map(|c| c.count).sum())
    }
}

/// Where Layer C may look for a slice's victims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Door {
    /// This user's own manager: `journalctl --user` holds its messages.
    Own,
    /// The system root or another user's manager: not readable here.
    Foreign,
    /// After a reboot: no listing from this boot proves a placement.
    NoProof,
}

fn door(s: &SliceRead, euid: Option<u32>, rebooted: bool) -> Door {
    if rebooted {
        return Door::NoProof;
    }
    match (manager_uid(&s.cgroup), euid) {
        (Some(m), Some(e)) if m == e => Door::Own,
        _ => Door::Foreign,
    }
}

/// Plan one slice. `prev` is its persisted baseline. PURE.
fn plan_slice<'a>(
    prev: Option<&'a SliceObs>,
    s: &'a SliceRead,
    kills: &[UnitKill],
    rebooted: bool,
    read_at: DateTime<Utc>,
) -> SlicePlan<'a> {
    // (baseline, the children's baselines — none after a reboot).
    let consume: Option<(u64, Option<&'a BTreeMap<String, u64>>)> = match prev {
        None => None,
        Some(p) if p.cgroup != s.cgroup => None,
        Some(_) if rebooted => Some((0, None)),
        Some(p) if s.oom_kill < p.oom_kill => None,
        Some(p) => Some((p.oom_kill, Some(&p.children))),
    };
    let prev_children = consume.and_then(|(_, c)| c);
    // A kept child whose counter could not be read keeps last tick's
    // baseline, so its listing still proves its placement next tick.
    let next_children: BTreeMap<String, u64> = s
        .children
        .iter()
        .filter_map(|c| {
            c.oom_kill
                .or_else(|| prev_children.and_then(|m| m.get(&c.name).copied()))
                .map(|v| (c.name.clone(), v))
        })
        .collect();
    let mut next = SliceObs {
        cgroup: s.cgroup.clone(),
        oom_kill: s.oom_kill,
        observed_at: read_at,
        children: next_children,
        named: BTreeMap::new(),
        carried: 0,
    };
    let Some((base, children_base)) = consume else {
        return SlicePlan {
            read: s,
            base: None,
            left: 0,
            children: Vec::new(),
            clamped: false,
            since: None,
            listing: None,
            named_before: None,
            carried_before: 0,
            next,
        };
    };
    let inside = |path: &str| -> u64 {
        kills
            .iter()
            .filter(|k| k.control_group.as_deref().is_some_and(|cg| is_under(cg, path)))
            .map(|k| k.count)
            .sum()
    };
    let left = (s.oom_kill - base).saturating_sub(inside(&s.cgroup));
    let mut children: Vec<ChildKill> = Vec::new();
    if let Some(cb) = children_base {
        for c in &s.children {
            let (Some(now), Some(&was)) = (c.oom_kill, cb.get(&c.name)) else {
                continue;
            };
            if now <= was {
                continue;
            }
            // A nested slice holding a watched unit: that unit's own kill is
            // not this child's.
            let count = (now - was).saturating_sub(inside(&format!("{}/{}", s.cgroup, c.name)));
            if count > 0 {
                children.push(ChildKill {
                    child: c.name.clone(),
                    count,
                    was,
                    counter: now,
                });
            }
        }
    }
    // The children were read after the slice: a kill between the two reads
    // is in a child's counter and not yet in the slice's. Trimmed here, it is
    // the next tick's slice delta — and the child keeps that much of its old
    // baseline, so the next tick's B still names it.
    let excess = children
        .iter()
        .map(|c| c.count)
        .sum::<u64>()
        .saturating_sub(left);
    let cuts = trim_children(&mut children, excess);
    for (child, cut) in &cuts {
        if let Some(v) = next.children.get_mut(child) {
            *v = v.saturating_sub(*cut);
        }
    }
    let clamped = !cuts.is_empty();
    SlicePlan {
        read: s,
        base: Some(base),
        left,
        children,
        clamped,
        since: prev.map(|p| p.observed_at),
        listing: children_base,
        named_before: prev.map(|p| &p.named),
        // A reboot restarts the counter at 0: nothing is carried across it.
        carried_before: if rebooted { 0 } else { prev.map_or(0, |p| p.carried) },
        next,
    }
}

/// Layer C's journal window for this tick: from the earliest consumed
/// baseline among the user-manager slices whose kill no counter will explain,
/// to this tick's read plus [`JOURNAL_LOOKAHEAD_SECS`]. Evaluated before the derive, over the PREVIOUS state
/// with the derive's own arithmetic — so the fork happens only when the
/// derive can use it. The window starts at the slice baseline's
/// `observed_at`, never at the loop's `last_tick_at` (which advances on ticks
/// that consume nothing). `None` = no read needed. PURE.
pub(crate) fn journal_window(
    prev: Option<&ComputerObs>,
    input: &ObsInput<'_>,
    read: &SliceOomRead,
) -> Option<JournalWindow> {
    let prev = prev?;
    if read.source != SliceSource::Read {
        return None;
    }
    let (by_id, by_at) = events::reboot_detected(prev, input);
    let rebooted = by_id || by_at;
    let (a, b) = events::vmstat_window(Some(prev), rebooted, input)?;
    let kills = events::unit_kills(prev, input, rebooted);
    let budget = b
        .saturating_sub(a)
        .saturating_sub(kills.iter().map(|k| k.count).sum());
    if budget == 0 {
        return None;
    }
    let plans: Vec<SlicePlan<'_>> = read
        .slices
        .iter()
        .map(|s| plan_slice(prev.slices.get(&s.name), s, &kills, rebooted, read.read_at))
        .collect();
    // Layer C: a user-manager slice of this uid with a kill no counter names.
    let user = plans
        .iter()
        .filter(|p| p.unnamed() > 0 && door(p.read, read.euid, rebooted) == Door::Own)
        .filter_map(|p| p.since)
        .min();
    // The kernel arm: any slice event at all (its `victim_comm`, and the
    // placement of a remainder). A reboot does not stop it: `task_memcg` is
    // its own proof.
    let kernel = plans
        .iter()
        .filter(|p| p.left > 0)
        .filter_map(|p| p.since)
        .min();
    let since = user.into_iter().chain(kernel).min()?;
    (since < read.read_at).then_some(JournalWindow {
        since,
        until: window_end(read.read_at),
        user: user.is_some(),
        kernel: kernel.is_some(),
    })
}

/// `[since, until)` for this tick's journal reads, and which of them to fork.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct JournalWindow {
    pub(crate) since: DateTime<Utc>,
    pub(crate) until: DateTime<Utc>,
    /// Layer C's `journalctl --user` read.
    pub(crate) user: bool,
    /// Phase 3's `journalctl -k` read.
    pub(crate) kernel: bool,
}

/// The kernel records of this boot inside a plan's `[since, until)`. PURE.
fn records_in<'k>(
    records: &'k [KernelOomRecord],
    plan: &SlicePlan<'_>,
    until: DateTime<Utc>,
    boot: &str,
) -> Vec<&'k KernelOomRecord> {
    let Some(since) = plan.since else {
        return Vec::new();
    };
    let boot = boot_key(boot);
    let (from, to) = (micros(since), micros(until));
    records
        .iter()
        .filter(|r| {
            !boot.is_empty()
                && boot_key(&r.boot_id) == boot
                && r.realtime_usec >= from
                && r.realtime_usec < to
        })
        .collect()
}

/// Kernel records placing what is left of a slice's delta: one kill each,
/// for a unit directly under the slice that no watched unit, B or C already
/// accounts for, at most `need`. PURE.
fn place_kernel(
    records: &[&KernelOomRecord],
    plan: &SlicePlan<'_>,
    journal: &[UnitOomEntry],
    watched: &[&str],
    kills: &[UnitKill],
    need: u64,
) -> Vec<(String, KernelOomRecord)> {
    let slice = &plan.read.cgroup;
    records
        .iter()
        .filter(|r| is_under(&r.task_memcg, slice))
        .filter(|r| {
            !kills.iter().any(|k| {
                k.control_group
                    .as_deref()
                    .is_some_and(|cg| r.task_memcg == cg || is_under(&r.task_memcg, cg))
            })
        })
        .filter_map(|r| {
            let child = r.task_memcg.get(slice.len() + 1..)?.split('/').next()?;
            let unit = [".scope", ".service", ".slice"]
                .iter()
                .any(|x| child.ends_with(x))
                .then_some(child)?;
            (!watched.contains(&unit)
                && !plan.children.iter().any(|c| c.child == unit)
                && !journal.iter().any(|e| e.unit == unit)
                && !named_before(plan, unit, r.realtime_usec))
            .then(|| (unit.to_string(), (*r).clone()))
        })
        .take(usize::try_from(need).unwrap_or(usize::MAX))
        .collect()
}

/// `(victim_comm, victim_comm_source)` for a slice event whose narrowest
/// proven cgroup is `cgroup`: only an EXACT `task_memcg` match names a comm.
/// `None` = the kernel log was not asked (the fields are left out). PURE.
fn comm_for(
    kernel: Option<&KernelRead>,
    records: &[&KernelOomRecord],
    cgroup: &str,
) -> Option<(Option<String>, &'static str)> {
    match kernel? {
        KernelRead::Unreadable => Some((None, "unreadable")),
        KernelRead::Read(_) => Some(
            records
                .iter()
                .find(|r| r.task_memcg == cgroup)
                .map_or((None, "not_found"), |r| (Some(r.comm.clone()), "kernel_log")),
        ),
    }
}

/// Microseconds since the epoch (0 before it). PURE.
fn micros(t: DateTime<Utc>) -> u64 {
    u64::try_from(t.timestamp_micros()).unwrap_or(0)
}

/// Layer C's accepted entries for one slice: this boot, inside
/// `[since, until)`, a unit last tick's listing proves was in the slice, not
/// a watched unit, not a child B already named, not a child listed THIS tick
/// with a readable counter that did not move (whatever it logged, it was not
/// killed since the baseline — a late entry for an earlier kill), at most
/// `need`. PURE.
fn accept_journal(
    entries: &[UnitOomEntry],
    plan: &SlicePlan<'_>,
    until: DateTime<Utc>,
    boot: &str,
    watched: &[&str],
    need: u64,
) -> Vec<UnitOomEntry> {
    let (Some(listing), Some(since)) = (plan.listing, plan.since) else {
        return Vec::new();
    };
    let boot = boot_key(boot);
    if boot.is_empty() {
        return Vec::new();
    }
    let (from, to) = (micros(since), micros(until));
    entries
        .iter()
        .filter(|e| {
            boot_key(&e.boot_id) == boot
                && e.realtime_usec >= from
                && e.realtime_usec < to
                && listing.contains_key(&e.unit)
                && !watched.contains(&e.unit.as_str())
                && !plan.children.iter().any(|c| c.child == e.unit)
                && !plan.read.children.iter().any(|c| {
                    c.name == e.unit
                        && c.oom_kill.is_some()
                        && c.oom_kill == listing.get(&e.unit).copied()
                })
                && !named_before(plan, &e.unit, e.realtime_usec)
        })
        .take(usize::try_from(need).unwrap_or(usize::MAX))
        .cloned()
        .collect()
}

/// Remove `excess` from the children's counts, the last child first.
/// Returns what was cut from each child (empty = nothing). PURE.
fn trim_children(children: &mut Vec<ChildKill>, mut excess: u64) -> Vec<(String, u64)> {
    let mut cuts = Vec::new();
    while excess > 0 {
        let Some(last) = children.last_mut() else {
            break;
        };
        let cut = last.count.min(excess);
        last.count -= cut;
        excess -= cut;
        cuts.push((last.child.clone(), cut));
        if last.count == 0 {
            children.pop();
        }
    }
    cuts
}

/// Everything the slice stage of `events::derive` needs.
pub(crate) struct SliceInput<'a> {
    pub(crate) prev: &'a BTreeMap<String, SliceObs>,
    pub(crate) read: Option<&'a SliceOomRead>,
    /// Layer C's read, when [`journal_window`] asked for one.
    pub(crate) journal: Option<&'a JournalRead>,
    /// Phase 3's kernel-log read, when [`journal_window`] asked for one.
    pub(crate) kernel: Option<&'a KernelRead>,
    pub(crate) unit_kills: &'a [UnitKill],
    /// The watched unit names this tick.
    pub(crate) watched: &'a [&'a str],
    pub(crate) rebooted: bool,
    /// `V - U`: what the slice events may add up to this tick. `None` = the
    /// vmstat delta is not computable, so nothing is consumed.
    pub(crate) budget: Option<u64>,
    pub(crate) boot: &'a str,
    pub(crate) now_s: &'a str,
    pub(crate) oom_kill_total: Option<u64>,
}

/// What the slice stage produced.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SliceOutcome {
    pub(crate) events: Vec<ComputerEvent>,
    /// The kills those events carry.
    pub(crate) attributed: u64,
    /// The next tick's slice baselines.
    pub(crate) next: BTreeMap<String, SliceObs>,
    /// The `slices` detail field for every `oom_kill` event this tick.
    pub(crate) status: &'static str,
    /// `(slice, cgroup)` for every slice whose path is known, to place the
    /// watched units' own events.
    cgroups: Vec<(String, String)>,
}

impl SliceOutcome {
    /// The watched slice containing `control_group`, if any. PURE.
    pub(crate) fn slice_of(&self, control_group: Option<&str>) -> Option<&str> {
        let cg = control_group?;
        self.cgroups
            .iter()
            .find(|(_, path)| is_under(cg, path))
            .map(|(name, _)| name.as_str())
    }
}

/// The slice stage of `events::derive`: this tick's slice events (Layers A,
/// B, C and the kernel records), the kills they carry and the next
/// baselines. PURE.
pub(crate) fn attribute(a: &SliceInput<'_>) -> SliceOutcome {
    let status = a
        .read
        .map_or(SLICES_NOT_SUPPORTED, |r| r.source.as_str());
    let read = a.read.filter(|r| r.source == SliceSource::Read);
    let cgroups = slice_cgroups(a.prev, read);
    let unchanged = |next: BTreeMap<String, SliceObs>| SliceOutcome {
        events: Vec::new(),
        attributed: 0,
        next,
        status,
        cgroups: cgroups.clone(),
    };
    let Some(read) = read else {
        // Not read this tick: keep every baseline.
        return unchanged(a.prev.clone());
    };
    let mut plans: Vec<SlicePlan<'_>> = read
        .slices
        .iter()
        .map(|s| plan_slice(a.prev.get(&s.name), s, a.unit_kills, a.rebooted, read.read_at))
        .collect();
    let Some(budget) = a.budget else {
        // vmstat not computable: nothing is consumed, so every baseline is
        // kept; a slice seen for the first time still gets one.
        let mut next = a.prev.clone();
        for p in plans {
            next.entry(p.read.name.clone()).or_insert(p.next);
        }
        return unchanged(next);
    };
    let mut drafts: Vec<Draft<'_>> = plans.iter().map(|p| draft_slice(a, read, p)).collect();
    let tick_clamped = clamp_to_budget(&mut drafts, budget);
    let events = emit_slice_events(a, status, &plans, &drafts, tick_clamped);
    let attributed = drafts.iter().map(Draft::total).sum();
    carry_into_next(&mut plans, &drafts, read.read_at);
    // A slice not read this tick (gone, or its counter unreadable) loses its
    // baseline: its kills meanwhile were the residual's, and it rebaselines
    // when it is back.
    let next = plans
        .into_iter()
        .map(|p| (p.read.name.clone(), p.next))
        .collect();
    SliceOutcome {
        events,
        attributed,
        next,
        status,
        cgroups,
    }
}

/// `(slice, cgroup)` for every slice whose path is known this tick: from the
/// read, else from the persisted baselines. PURE.
fn slice_cgroups(
    prev: &BTreeMap<String, SliceObs>,
    read: Option<&SliceOomRead>,
) -> Vec<(String, String)> {
    match read {
        Some(r) => r
            .slices
            .iter()
            .map(|s| (s.name.clone(), s.cgroup.clone()))
            .collect(),
        None => prev
            .iter()
            .map(|(n, s)| (n.clone(), s.cgroup.clone()))
            .collect(),
    }
}

/// One slice's events before the vmstat clamp: B, then C, then the kernel
/// records, then the remainder. PURE.
fn draft_slice<'k>(a: &SliceInput<'k>, read: &SliceOomRead, p: &SlicePlan<'_>) -> Draft<'k> {
    let need = p.unnamed();
    let d = door(p.read, read.euid, a.rebooted);
    let journal = match (d, a.journal) {
        (Door::Own, Some(JournalRead::Read(entries))) if need > 0 => {
            accept_journal(entries, p, window_end(read.read_at), a.boot, a.watched, need)
        }
        _ => Vec::new(),
    };
    // `journal` on the events C did not produce: whether Layer C could have
    // named them.
    let journal_word = match (need, d, a.journal) {
        (0, _, _) => "not_needed",
        (_, Door::NoProof, _) => "no_proof",
        (_, Door::Own, Some(JournalRead::Read(_))) => "read",
        _ => "unreadable",
    };
    let records = match a.kernel {
        Some(KernelRead::Read(r)) => records_in(r, p, window_end(read.read_at), a.boot),
        _ => Vec::new(),
    };
    let left = need - journal.len() as u64;
    let kernel = place_kernel(&records, p, &journal, a.watched, a.unit_kills, left);
    Draft {
        children: p.children.clone(),
        remainder: left - kernel.len() as u64,
        journal,
        kernel,
        records,
        journal_word,
        clamped: p.clamped,
        cut_slice: 0,
        cut_children: Vec::new(),
    }
}

/// Read skew against vmstat: never more than `budget` (`V - U`) in all.
/// Trims the remainders first, then C's entries, then the kernel records,
/// then B, recording each cut on its draft ([`Draft::cut_slice`],
/// [`Draft::cut_children`]); `true` when it fired. PURE.
fn clamp_to_budget(drafts: &mut [Draft<'_>], budget: u64) -> bool {
    let total: u64 = drafts.iter().map(Draft::total).sum();
    if total <= budget {
        return false;
    }
    let mut excess = total - budget;
    for d in drafts.iter_mut() {
        let cut = d.remainder.min(excess);
        d.remainder -= cut;
        d.cut_slice += cut;
        excess -= cut;
    }
    for d in drafts.iter_mut() {
        while excess > 0 && d.journal.pop().is_some() {
            d.cut_slice += 1;
            excess -= 1;
        }
    }
    for d in drafts.iter_mut() {
        while excess > 0 && d.kernel.pop().is_some() {
            d.cut_slice += 1;
            excess -= 1;
        }
    }
    for d in drafts.iter_mut() {
        let before: u64 = d.children.iter().map(|c| c.count).sum();
        let cuts = trim_children(&mut d.children, excess.min(before));
        excess -= excess.min(before);
        for (_, cut) in &cuts {
            d.cut_slice += cut;
        }
        d.cut_children.extend(cuts);
    }
    true
}

/// Carry into the next baselines what the clamp cut, and record every unit
/// this tick named (B at the read, C and the kernel records at their lines'
/// times). PURE.
///
/// A cut is carried at most as far as THIS tick's own new kills: on a tick
/// after a carry, a re-cut of the carried excess (one vmstat never covers)
/// is dropped, while a fresh lag cut of this tick's new kills is still
/// carried. A B cut keeps its child's placement only when the whole cut is
/// carried.
fn carry_into_next(plans: &mut [SlicePlan<'_>], drafts: &[Draft<'_>], read_at: DateTime<Utc>) {
    let read_us = micros(read_at);
    for (p, d) in plans.iter_mut().zip(drafts) {
        // This tick's own new kills: what the slice delta left after the
        // watched units inside it, minus the part carried in.
        let fresh = p.left.saturating_sub(p.carried_before);
        let carry = if p.carried_before == 0 {
            d.cut_slice
        } else {
            d.cut_slice.min(fresh)
        };
        if carry > 0 {
            p.next.oom_kill = p.next.oom_kill.saturating_sub(carry);
            p.next.carried = carry;
            if carry == d.cut_slice {
                for (child, cut) in &d.cut_children {
                    if let Some(v) = p.next.children.get_mut(child) {
                        *v = v.saturating_sub(*cut);
                    }
                }
            }
        }
        let named = d
            .children
            .iter()
            .map(|c| (c.child.clone(), read_us))
            .chain(d.journal.iter().map(|e| (e.unit.clone(), e.realtime_usec)))
            .chain(d.kernel.iter().map(|(u, r)| (u.clone(), r.realtime_usec)));
        p.next.named.extend(named);
    }
}

/// `DateTime` from journald's microseconds, as an event time; `fallback`
/// when out of range. PURE.
fn event_time(realtime_usec: u64, fallback: &str) -> String {
    i64::try_from(realtime_usec)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_micros)
        .map_or_else(|| fallback.to_string(), rfc3339)
}

/// Build every slice's events. Ids carry the slice's cgroup, so units of the
/// same name under two slices (the system root and a user manager) never
/// share one. PURE.
fn emit_slice_events(
    a: &SliceInput<'_>,
    status: &str,
    plans: &[SlicePlan<'_>],
    drafts: &[Draft<'_>],
    tick_clamped: bool,
) -> Vec<ComputerEvent> {
    let mut events = Vec::new();
    for (p, d) in plans.iter().zip(drafts) {
        let s = p.read;
        let clamped = d.clamped || tick_clamped;
        let detail = |unit: Option<&str>, cgroup: &str, count: u64, how: &str, journal: &str| {
            let mut v = json!({
                "victim_unit": unit,
                "victim_slice": s.name,
                "victim_cgroup": cgroup,
                "count": count,
                "attribution": how,
                "slices": status,
                "journal": journal,
                "children_truncated": s.children_truncated,
                "count_clamped": clamped,
                "oom_kill_total": a.oom_kill_total,
                "slice_oom_kill_total": s.oom_kill,
                "previous_slice_oom_kill_total": p.base,
            });
            if let Some((comm, source)) = comm_for(a.kernel, &d.records, cgroup) {
                v["victim_comm"] = json!(comm);
                v["victim_comm_source"] = json!(source);
            }
            v
        };
        for c in &d.children {
            let how = "cgroup_memory_events";
            events.push(ComputerEvent {
                client_event_id: event_id(
                    "oom_kill",
                    &[a.boot, &s.cgroup, &c.child, how, &c.key()],
                ),
                kind: "oom_kill".into(),
                observed_at: a.now_s.to_string(),
                detail: detail(
                    Some(&c.child),
                    &format!("{}/{}", s.cgroup, c.child),
                    c.count,
                    how,
                    "not_needed",
                ),
            });
        }
        for e in &d.journal {
            let how = "user_journal_unit_oom";
            events.push(ComputerEvent {
                client_event_id: event_id(
                    "oom_kill",
                    &[a.boot, &s.cgroup, &e.unit, how, &e.realtime_usec.to_string()],
                ),
                kind: "oom_kill".into(),
                observed_at: event_time(e.realtime_usec, a.now_s),
                detail: detail(
                    Some(&e.unit),
                    &format!("{}/{}", s.cgroup, e.unit),
                    1,
                    how,
                    "read",
                ),
            });
        }
        for (unit, r) in &d.kernel {
            let how = "kernel_oom_record";
            let mut v = detail(Some(unit), &r.task_memcg, 1, how, d.journal_word);
            v["victim_comm"] = json!(r.comm);
            v["victim_comm_source"] = json!("kernel_log");
            events.push(ComputerEvent {
                client_event_id: event_id(
                    "oom_kill",
                    &[a.boot, &s.cgroup, unit, how, &r.realtime_usec.to_string()],
                ),
                kind: "oom_kill".into(),
                observed_at: event_time(r.realtime_usec, a.now_s),
                detail: v,
            });
        }
        if d.remainder > 0 {
            let how = if a.rebooted {
                "slice_memory_events_since_boot"
            } else {
                "slice_memory_events"
            };
            events.push(ComputerEvent {
                client_event_id: event_id(
                    "oom_kill",
                    // The counter RANGE, as a B child's: a remainder carried
                    // to the next tick at an unchanged counter is a distinct
                    // event there, not a duplicate coord drops.
                    &[
                        a.boot,
                        &s.cgroup,
                        "slice_memory_events",
                        &format!("{}..{}", p.base.unwrap_or(0), s.oom_kill),
                    ],
                ),
                kind: "oom_kill".into(),
                observed_at: a.now_s.to_string(),
                detail: detail(None, &s.cgroup, d.remainder, how, d.journal_word),
            });
        }
    }
    events
}

/// One slice's events before they are built.
struct Draft<'k> {
    children: Vec<ChildKill>,
    /// Layer C's accepted entries, one kill each.
    journal: Vec<UnitOomEntry>,
    /// Kernel records placing a kill each, by the unit under the slice.
    kernel: Vec<(String, KernelOomRecord)>,
    /// This slice's kernel records in its window, for `victim_comm`.
    records: Vec<&'k KernelOomRecord>,
    /// `journal` on this slice's remainder event.
    journal_word: &'static str,
    remainder: u64,
    clamped: bool,
    /// What the vmstat clamp cut from this slice's kills, kept back in its
    /// next baseline (for one tick) so they are counted when vmstat catches
    /// up. Only a B cut keeps its placement (its child's baseline is kept
    /// back too); the others come back as an unplaced slice remainder.
    cut_slice: u64,
    /// The B part of it, per child.
    cut_children: Vec<(String, u64)>,
}

impl Draft<'_> {
    fn total(&self) -> u64 {
        self.children.iter().map(|c| c.count).sum::<u64>()
            + self.journal.len() as u64
            + self.kernel.len() as u64
            + self.remainder
    }
}

#[cfg(test)]
mod tests {
    use super::super::events::{self, derive, ComputerEvent, ComputerObs, ObsInput};
    use super::super::journal_oom;
    use super::super::services::{parse_systemctl_show, row_from_props, WatchedUnit};
    use super::*;

    const BOOT: &str = "27cd9877-3ff3-47f0-8388-9880a31d5902";
    const AGENT: &str =
        "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice";
    const CI: &str = "/ci.slice/ci-runners.slice";
    const RUNNER_SLICE: &str =
        "/user.slice/user-1000.slice/user@1000.service/qontinui.slice/qontinui-runner.slice";

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn slice(name: &str, cgroup: &str, oom: u64, children: &[(&str, Option<u64>)]) -> SliceRead {
        SliceRead {
            name: name.into(),
            cgroup: cgroup.into(),
            oom_kill: oom,
            children: children
                .iter()
                .map(|(n, c)| ChildRead {
                    name: (*n).into(),
                    oom_kill: *c,
                })
                .collect(),
            children_truncated: false,
        }
    }

    fn read(at: &str, slices: Vec<SliceRead>) -> SliceOomRead {
        SliceOomRead {
            source: SliceSource::Read,
            read_at: t(at),
            euid: Some(1000),
            slices,
        }
    }

    fn watched(show: &str, cgroup_oom: Option<u64>) -> Vec<WatchedUnit> {
        parse_systemctl_show(show)
            .iter()
            .map(|p| WatchedUnit {
                row: row_from_props(p, None),
                cgroup_oom_kill: cgroup_oom,
                control_group: p.control_group.clone(),
            })
            .collect()
    }

    fn input<'a>(
        units: &'a [WatchedUnit],
        oom: Option<u64>,
        slices: Option<&'a SliceOomRead>,
    ) -> ObsInput<'a> {
        ObsInput {
            boot_id: Some(BOOT),
            booted_at: Some("2026-11-20T08:00:00Z"),
            oom_kill_total: oom,
            services: Some(units),
            complete_kinds: &["gh_actions_runner", "qontinui_runner"],
            carry_oom_baseline: true,
            slices,
            journal: None,
            kernel: None,
        }
    }

    fn ooms(ev: &[ComputerEvent]) -> Vec<&ComputerEvent> {
        ev.iter().filter(|e| e.kind == "oom_kill").collect()
    }

    /// Two ticks: a baseline from `before`, then `after`.
    fn two_ticks(
        units: &[WatchedUnit],
        before: (u64, &SliceOomRead),
        after: (Option<u64>, &SliceOomRead),
    ) -> (Vec<ComputerEvent>, ComputerObs) {
        let (ev0, obs0) = derive(
            None,
            &input(units, Some(before.0), Some(before.1)),
            t("2026-11-27T00:14:30Z"),
        );
        assert!(ev0.is_empty(), "the first observation is a baseline: {ev0:?}");
        derive(
            Some(&obs0),
            &input(units, after.0, Some(after.1)),
            t("2026-11-27T00:15:00Z"),
        )
    }

    const RUNNER_SERVICE: &str = "\
Id=qontinui-runner.service
ActiveState=active
SubState=running
Result=success
OOMPolicy=continue
NRestarts=0
StateChangeTimestamp=Wed 2026-11-25 09:00:00 UTC
ControlGroup=/user.slice/user-1000.slice/user@1000.service/qontinui.slice/qontinui-runner.slice/qontinui-runner.service
";

    const CI_RUNNER_ACTIVE: &str = "\
Id=actions.runner.example-org-example-repo.fleetbox.service
ActiveState=active
SubState=running
Result=success
Restart=no
OOMPolicy=stop
NRestarts=0
StateChangeTimestamp=Tue 2026-11-24 09:12:40 UTC
ControlGroup=/ci.slice/ci-runners.slice/actions.runner.example-org-example-repo.fleetbox.service
";

    const CI_RUNNER_OOM: &str = "\
Id=actions.runner.example-org-example-repo.fleetbox.service
ActiveState=failed
SubState=failed
Result=oom-kill
Restart=no
OOMPolicy=stop
NRestarts=0
StateChangeTimestamp=Fri 2026-11-27 00:14:50 UTC
ControlGroup=
";

    /// Plan test 2: a live child whose own counter moved is the victim.
    #[test]
    fn a_live_child_whose_counter_moved_is_the_victim() {
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![slice(
                "agent-builds.slice",
                AGENT,
                288,
                &[("agent-build-a.scope", Some(0)), ("agent-build-b.scope", Some(2))],
            )],
        );
        let r1 = read(
            "2026-11-27T00:15:00Z",
            vec![slice(
                "agent-builds.slice",
                AGENT,
                289,
                &[("agent-build-a.scope", Some(1)), ("agent-build-b.scope", Some(2))],
            )],
        );
        let (ev, next) = two_ticks(&[], (34078, &r0), (Some(34079), &r1));
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        let d = &o[0].detail;
        assert_eq!(d["victim_unit"], "agent-build-a.scope");
        assert_eq!(d["victim_slice"], "agent-builds.slice");
        assert_eq!(
            d["victim_cgroup"],
            "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice/agent-build-a.scope"
        );
        assert_eq!(d["attribution"], "cgroup_memory_events");
        assert_eq!(d["count"], 1);
        assert_eq!(d["slices"], "read");
        assert_eq!(d["journal"], "not_needed");
        assert_eq!(d["children_truncated"], false);
        assert_eq!(d["count_clamped"], false);
        assert_eq!(d["slice_oom_kill_total"], 289);
        assert_eq!(d["previous_slice_oom_kill_total"], 288);
        assert_eq!(o[0].observed_at, "2026-11-27T00:15:00Z");
        assert_eq!(
            o[0].client_event_id,
            event_id(
                "oom_kill",
                &[
                    BOOT,
                    "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice",
                    "agent-build-a.scope",
                    "cgroup_memory_events",
                    "0..1"
                ]
            )
        );
        let s = &next.slices["agent-builds.slice"];
        assert_eq!(s.oom_kill, 289);
        assert_eq!(s.observed_at, t("2026-11-27T00:15:00Z"));
        assert_eq!(
            s.children,
            [("agent-build-a.scope".to_string(), 1), ("agent-build-b.scope".to_string(), 2)]
                .into_iter()
                .collect()
        );
    }

    /// Plan test 6: a watched unit inside a watched slice is killed. Only the
    /// unit's own event, carrying the slice; no slice remainder beside it.
    #[test]
    fn a_watched_unit_inside_a_watched_slice_is_counted_once() {
        let before = watched(RUNNER_SERVICE, Some(0));
        let after = watched(RUNNER_SERVICE, Some(1));
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![slice("qontinui-runner.slice", RUNNER_SLICE, 4, &[])],
        );
        let r1 = read(
            "2026-11-27T00:15:00Z",
            vec![slice("qontinui-runner.slice", RUNNER_SLICE, 5, &[])],
        );
        let (_, o0) = derive(
            None,
            &input(&before, Some(10), Some(&r0)),
            t("2026-11-27T00:14:30Z"),
        );
        let (ev, _) = derive(
            Some(&o0),
            &input(&after, Some(11), Some(&r1)),
            t("2026-11-27T00:15:00Z"),
        );
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(o[0].detail["victim_unit"], "qontinui-runner.service");
        assert_eq!(o[0].detail["attribution"], "cgroup_memory_events");
        assert_eq!(o[0].detail["victim_slice"], "qontinui-runner.slice");
        assert_eq!(
            o[0].detail["victim_cgroup"],
            "/user.slice/user-1000.slice/user@1000.service/qontinui.slice/qontinui-runner.slice/qontinui-runner.service"
        );
        assert_eq!(o[0].detail["slices"], "read");
    }

    /// Plan test 17: a GitHub runner service under `ci-runners.slice` dies
    /// on the `unit_result` arm — its `ControlGroup=` is empty this tick, so
    /// it is placed by the one persisted last tick. The same on the
    /// `cgroup_memory_events` arm.
    #[test]
    fn a_watched_unit_is_placed_by_its_persisted_cgroup_on_the_unit_result_arm() {
        let before = watched(CI_RUNNER_ACTIVE, Some(0));
        let after = watched(CI_RUNNER_OOM, None);
        assert_eq!(after[0].control_group, None, "the dead unit's cgroup is gone");
        let r0 = read("2026-11-27T00:14:30Z", vec![slice("ci-runners.slice", CI, 3, &[])]);
        let r1 = read("2026-11-27T00:15:00Z", vec![slice("ci-runners.slice", CI, 4, &[])]);
        let (_, o0) = derive(
            None,
            &input(&before, Some(20), Some(&r0)),
            t("2026-11-27T00:14:30Z"),
        );
        assert_eq!(
            o0.units["actions.runner.example-org-example-repo.fleetbox.service"]
                .control_group
                .as_deref(),
            Some("/ci.slice/ci-runners.slice/actions.runner.example-org-example-repo.fleetbox.service")
        );
        let (ev, o1) = derive(
            Some(&o0),
            &input(&after, Some(21), Some(&r1)),
            t("2026-11-27T00:15:00Z"),
        );
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "only the unit's own event: {ev:?}");
        assert_eq!(o[0].detail["attribution"], "unit_result");
        assert_eq!(o[0].detail["victim_slice"], "ci-runners.slice");
        assert_eq!(
            o[0].detail["victim_cgroup"],
            "/ci.slice/ci-runners.slice/actions.runner.example-org-example-repo.fleetbox.service"
        );
        // The last non-empty value survives the empty one.
        assert_eq!(
            o1.units["actions.runner.example-org-example-repo.fleetbox.service"]
                .control_group
                .as_deref(),
            Some("/ci.slice/ci-runners.slice/actions.runner.example-org-example-repo.fleetbox.service")
        );

        // The cgroup arm: the unit survives (OOMPolicy=continue), its own
        // counter moved.
        let live = watched(CI_RUNNER_ACTIVE, Some(1));
        let (ev, _) = derive(
            Some(&o0),
            &input(&live, Some(21), Some(&r1)),
            t("2026-11-27T00:15:00Z"),
        );
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(o[0].detail["attribution"], "cgroup_memory_events");
        assert_eq!(o[0].detail["victim_slice"], "ci-runners.slice");
    }

    /// Plan test 7: two slices move in one tick — two events, and the
    /// residual is computed over both.
    #[test]
    fn two_slices_moving_in_one_tick_are_two_events_and_the_residual_spans_both() {
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![
                slice("ci-runners.slice", CI, 3, &[]),
                slice("agent-builds.slice", AGENT, 10, &[("agent-build-a.scope", Some(0))]),
            ],
        );
        let r1 = read(
            "2026-11-27T00:15:00Z",
            vec![
                slice("ci-runners.slice", CI, 5, &[]),
                slice("agent-builds.slice", AGENT, 11, &[("agent-build-a.scope", Some(1))]),
            ],
        );
        let (ev, _) = two_ticks(&[], (100, &r0), (Some(104), &r1));
        let o = ooms(&ev);
        assert_eq!(o.len(), 3, "{ev:?}");
        assert_eq!(o[0].detail["victim_slice"], "ci-runners.slice");
        assert_eq!(o[0].detail["attribution"], "slice_memory_events");
        assert_eq!(o[0].detail["victim_unit"], serde_json::Value::Null);
        assert_eq!(o[0].detail["victim_cgroup"], "/ci.slice/ci-runners.slice");
        assert_eq!(o[0].detail["count"], 2);
        assert_eq!(
            o[0].client_event_id,
            event_id(
                "oom_kill",
                &[BOOT, "/ci.slice/ci-runners.slice", "slice_memory_events", "3..5"]
            )
        );
        assert_eq!(o[1].detail["victim_unit"], "agent-build-a.scope");
        assert_eq!(o[1].detail["count"], 1);
        // 104 - 100 = 4 kills; 3 were inside the slices.
        assert_eq!(o[2].detail["attribution"], "vmstat_delta");
        assert_eq!(o[2].detail["count"], 1);
        assert_eq!(o[2].detail["victim_slice"], serde_json::Value::Null);
        assert_eq!(o[2].detail["slices"], "read");
    }

    /// Plan test 8: slice delta 3, vmstat delta 2 → 2 in all, clamped.
    #[test]
    fn read_skew_is_clamped_to_the_vmstat_delta() {
        let r0 = read("2026-11-27T00:14:30Z", vec![slice("agent-builds.slice", AGENT, 10, &[])]);
        let r1 = read("2026-11-27T00:15:00Z", vec![slice("agent-builds.slice", AGENT, 13, &[])]);
        let (ev, _) = two_ticks(&[], (50, &r0), (Some(52), &r1));
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "no residual beside it: {ev:?}");
        assert_eq!(o[0].detail["count"], 2);
        assert_eq!(o[0].detail["count_clamped"], true);
    }

    /// A child total over its slice's own delta (the children are read after
    /// the slice) is clamped to the slice delta.
    #[test]
    fn children_are_clamped_to_their_slice_delta() {
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![slice(
                "agent-builds.slice",
                AGENT,
                10,
                &[("agent-build-a.scope", Some(0)), ("agent-build-b.scope", Some(0))],
            )],
        );
        let r1 = read(
            "2026-11-27T00:15:00Z",
            vec![slice(
                "agent-builds.slice",
                AGENT,
                11,
                &[("agent-build-a.scope", Some(1)), ("agent-build-b.scope", Some(1))],
            )],
        );
        let (ev, _) = two_ticks(&[], (50, &r0), (Some(52), &r1));
        let o = ooms(&ev);
        assert_eq!(o.len(), 2, "{ev:?}");
        assert_eq!(o[0].detail["victim_unit"], "agent-build-a.scope");
        assert_eq!(o[0].detail["count"], 1);
        assert_eq!(o[0].detail["count_clamped"], true);
        // The other kill (52 - 50 = 2) is outside the slice's own count.
        assert_eq!(o[1].detail["attribution"], "vmstat_delta");
        assert_eq!(o[1].detail["count"], 1);
    }

    /// Plan test 9: a slice counter that went down (the cgroup was recreated)
    /// rebaselines; the kill is the vmstat residual's.
    #[test]
    fn a_slice_counter_that_went_down_rebaselines() {
        let r0 = read("2026-11-27T00:14:30Z", vec![slice("agent-builds.slice", AGENT, 289, &[])]);
        let r1 = read("2026-11-27T00:15:00Z", vec![slice("agent-builds.slice", AGENT, 1, &[])]);
        let (ev, next) = two_ticks(&[], (34078, &r0), (Some(34079), &r1));
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(o[0].detail["attribution"], "vmstat_delta");
        assert_eq!(o[0].detail["victim_unit"], serde_json::Value::Null);
        assert_eq!(next.slices["agent-builds.slice"].oom_kill, 1);
    }

    /// Plan test 10: the slice resolved at a different path rebaselines.
    #[test]
    fn a_moved_slice_path_rebaselines() {
        let r0 = read("2026-11-27T00:14:30Z", vec![slice("agent-builds.slice", AGENT, 289, &[])]);
        let moved = "/user.slice/user-1001.slice/user@1001.service/agent.slice/agent-builds.slice";
        let r1 = read("2026-11-27T00:15:00Z", vec![slice("agent-builds.slice", moved, 300, &[])]);
        let (ev, next) = two_ticks(&[], (34078, &r0), (Some(34078), &r1));
        assert!(ooms(&ev).is_empty(), "{ev:?}");
        assert_eq!(next.slices["agent-builds.slice"].cgroup, moved);
        assert_eq!(next.slices["agent-builds.slice"].oom_kill, 300);
    }

    /// Plan tests 11 and 24 (reboot): a slice baseline is 0 after a reboot,
    /// its children are cleared (no child is placed), and the attribution is
    /// `slice_memory_events_since_boot`; a first-ever sight is a baseline.
    #[test]
    fn after_a_reboot_a_slice_counts_from_zero_and_first_sight_is_a_baseline() {
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![slice("agent-builds.slice", AGENT, 289, &[("agent-build-a.scope", Some(0))])],
        );
        let (ev0, o0) = derive(None, &input(&[], Some(34078), Some(&r0)), t("2026-11-27T00:14:30Z"));
        assert!(ev0.is_empty(), "first-ever sight: {ev0:?}");
        assert_eq!(o0.slices["agent-builds.slice"].oom_kill, 289);

        let new_boot = "11111111-2222-4333-8444-555555555555";
        let r1 = read(
            "2026-11-27T03:01:00Z",
            vec![slice("agent-builds.slice", AGENT, 2, &[("agent-build-b.scope", Some(1))])],
        );
        let inp = ObsInput {
            boot_id: Some(new_boot),
            booted_at: Some("2026-11-27T03:00:00Z"),
            ..input(&[], Some(2), Some(&r1))
        };
        let (ev, o1) = derive(Some(&o0), &inp, t("2026-11-27T03:01:00Z"));
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["reboot", "oom_kill"], "{ev:?}");
        let d = &ev[1].detail;
        assert_eq!(d["attribution"], "slice_memory_events_since_boot");
        assert_eq!(d["victim_unit"], serde_json::Value::Null);
        assert_eq!(d["count"], 2);
        assert_eq!(d["previous_slice_oom_kill_total"], 0);
        assert_eq!(
            ev[1].client_event_id,
            event_id(
                "oom_kill",
                &[
                    new_boot,
                    "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice",
                    "slice_memory_events",
                    "0..2"
                ]
            )
        );
        assert_eq!(
            o1.slices["agent-builds.slice"].children,
            [("agent-build-b.scope".to_string(), 1)].into_iter().collect()
        );

        // A slice seen for the first time on a later tick: a baseline only,
        // and the kill is the residual's.
        let r2 = read(
            "2026-11-27T03:01:30Z",
            vec![
                slice("agent-builds.slice", AGENT, 2, &[("agent-build-b.scope", Some(1))]),
                slice("ci-runners.slice", CI, 40, &[]),
            ],
        );
        let inp = ObsInput {
            boot_id: Some(new_boot),
            booted_at: Some("2026-11-27T03:00:00Z"),
            ..input(&[], Some(3), Some(&r2))
        };
        let (ev, o2) = derive(Some(&o1), &inp, t("2026-11-27T03:01:30Z"));
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(o[0].detail["attribution"], "vmstat_delta");
        assert_eq!(o2.slices["ci-runners.slice"].oom_kill, 40);
    }

    /// Plan test 24: a tick whose vmstat read failed keeps every slice and
    /// child baseline and emits no slice event; the next readable tick
    /// attributes the kill once.
    #[test]
    fn an_unreadable_vmstat_keeps_the_slice_baselines() {
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![slice("agent-builds.slice", AGENT, 10, &[("agent-build-a.scope", Some(0))])],
        );
        let r1 = read(
            "2026-11-27T00:15:00Z",
            vec![slice("agent-builds.slice", AGENT, 11, &[("agent-build-a.scope", Some(1))])],
        );
        let (ev, o1) = two_ticks(&[], (50, &r0), (None, &r1));
        assert!(ooms(&ev).is_empty(), "{ev:?}");
        assert_eq!(o1.slices["agent-builds.slice"].oom_kill, 10);
        assert_eq!(o1.slices["agent-builds.slice"].observed_at, t("2026-11-27T00:14:30Z"));
        assert_eq!(o1.slices["agent-builds.slice"].children["agent-build-a.scope"], 0);
        let (ev, _) = derive(
            Some(&o1),
            &input(&[], Some(51), Some(&r1)),
            t("2026-11-27T00:15:30Z"),
        );
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(o[0].detail["victim_unit"], "agent-build-a.scope");
        assert_eq!(o[0].detail["count"], 1);
    }

    /// Plan test 25: the clamp trims the slice remainder before the children,
    /// and never a watched unit's own event.
    #[test]
    fn the_clamp_trims_the_remainder_before_the_children_and_never_a_watched_unit() {
        // qontinui-runner.slice: the runner service (watched, 1 kill), a
        // child scope (1 kill) and 2 unplaced kills — a delta of 4.
        let before = watched(RUNNER_SERVICE, Some(0));
        let after = watched(RUNNER_SERVICE, Some(1));
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![slice("qontinui-runner.slice", RUNNER_SLICE, 10, &[("build-x.scope", Some(0))])],
        );
        let r1 = read(
            "2026-11-27T00:15:00Z",
            vec![slice("qontinui-runner.slice", RUNNER_SLICE, 14, &[("build-x.scope", Some(1))])],
        );
        let (_, o0) = derive(None, &input(&before, Some(100), Some(&r0)), t("2026-11-27T00:14:30Z"));
        let run = |v: u64| {
            let (ev, _) = derive(
                Some(&o0),
                &input(&after, Some(v), Some(&r1)),
                t("2026-11-27T00:15:00Z"),
            );
            ooms(&ev)
                .iter()
                .map(|e| {
                    (
                        e.detail["attribution"].as_str().unwrap().to_string(),
                        e.detail["count"].as_u64().unwrap(),
                        e.detail.get("count_clamped").cloned(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let f = Some(serde_json::Value::Bool(false));
        let tr = Some(serde_json::Value::Bool(true));
        // Unclamped: V delta 4 = unit 1 + child 1 + remainder 2.
        assert_eq!(
            run(104),
            vec![
                ("cgroup_memory_events".to_string(), 1, None),
                ("cgroup_memory_events".to_string(), 1, f.clone()),
                ("slice_memory_events".to_string(), 2, f.clone()),
            ]
        );
        // V delta 3: the remainder loses one.
        assert_eq!(
            run(103),
            vec![
                ("cgroup_memory_events".to_string(), 1, None),
                ("cgroup_memory_events".to_string(), 1, tr.clone()),
                ("slice_memory_events".to_string(), 1, tr.clone()),
            ]
        );
        // V delta 2: the remainder is gone, the child stays.
        assert_eq!(
            run(102),
            vec![
                ("cgroup_memory_events".to_string(), 1, None),
                ("cgroup_memory_events".to_string(), 1, tr.clone()),
            ]
        );
        // V delta 1: only the watched unit's own event, never trimmed.
        assert_eq!(run(101), vec![("cgroup_memory_events".to_string(), 1, None)]);
        assert_eq!(run(100), vec![("cgroup_memory_events".to_string(), 1, None)]);
    }

    /// A nested slice child holding a watched unit: that unit's own kill is
    /// subtracted from the child's delta, so it is not counted twice.
    #[test]
    fn a_nested_child_does_not_recount_the_watched_unit_inside_it() {
        const NESTED: &str = "\
Id=actions.runner.example-org-example-repo.fleetbox.service
ActiveState=active
SubState=running
Result=success
OOMPolicy=continue
NRestarts=0
StateChangeTimestamp=Tue 2026-11-24 09:12:40 UTC
ControlGroup=/ci.slice/ci-runners.slice/ci-runners-gpu.slice/actions.runner.example-org-example-repo.fleetbox.service
";
        let before = watched(NESTED, Some(0));
        let after = watched(NESTED, Some(1));
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![slice("ci-runners.slice", CI, 3, &[("ci-runners-gpu.slice", Some(0))])],
        );
        let r1 = read(
            "2026-11-27T00:15:00Z",
            vec![slice("ci-runners.slice", CI, 4, &[("ci-runners-gpu.slice", Some(1))])],
        );
        let (_, o0) = derive(None, &input(&before, Some(20), Some(&r0)), t("2026-11-27T00:14:30Z"));
        let (ev, _) = derive(
            Some(&o0),
            &input(&after, Some(21), Some(&r1)),
            t("2026-11-27T00:15:00Z"),
        );
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(
            o[0].detail["victim_unit"],
            "actions.runner.example-org-example-repo.fleetbox.service"
        );
        assert_eq!(o[0].detail["victim_slice"], "ci-runners.slice");
    }

    /// Plan test 13: the same tick derived twice from the same prior state (a
    /// dropped state save) mints identical ids.
    #[test]
    fn a_tick_derived_twice_mints_identical_ids() {
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![slice("agent-builds.slice", AGENT, 10, &[("agent-build-a.scope", Some(0))])],
        );
        let r1 = read(
            "2026-11-27T00:15:00Z",
            vec![slice("agent-builds.slice", AGENT, 13, &[("agent-build-a.scope", Some(1))])],
        );
        let (_, o0) = derive(None, &input(&[], Some(50), Some(&r0)), t("2026-11-27T00:14:30Z"));
        let ids = |at: &str| {
            let (ev, _) = derive(Some(&o0), &input(&[], Some(54), Some(&r1)), t(at));
            ev.into_iter().map(|e| e.client_event_id).collect::<Vec<_>>()
        };
        let first = ids("2026-11-27T00:15:00Z");
        assert_eq!(first.len(), 3, "{first:?}");
        assert_eq!(first, ids("2026-11-27T00:15:30Z"));
    }

    /// Plan test 23 (Phase 1 part): every `oom_kill` event says whether the
    /// slices were read.
    #[test]
    fn every_oom_kill_event_carries_slices() {
        let units = watched(RUNNER_SERVICE, Some(0));
        let killed = watched(RUNNER_SERVICE, Some(1));
        let r0 = read(
            "2026-11-27T00:14:30Z",
            vec![slice("qontinui-runner.slice", RUNNER_SLICE, 4, &[])],
        );
        let (_, o0) = derive(None, &input(&units, Some(10), Some(&r0)), t("2026-11-27T00:14:30Z"));
        let unread = SliceOomRead {
            source: SliceSource::Unreadable,
            read_at: t("2026-11-27T00:15:00Z"),
            euid: Some(1000),
            slices: Vec::new(),
        };
        let cases: [(Option<&SliceOomRead>, &str); 3] = [
            (Some(&r0), "read"),
            (Some(&unread), "unreadable"),
            (None, "not_supported"),
        ];
        for (slices, want) in cases {
            let (ev, next) = derive(
                Some(&o0),
                &input(&killed, Some(12), slices),
                t("2026-11-27T00:15:00Z"),
            );
            let o = ooms(&ev);
            assert_eq!(o.len(), 2, "{want}: {ev:?}");
            for e in &o {
                assert_eq!(e.detail["slices"], want, "{want}: {e:?}");
            }
            assert_eq!(o[1].detail["attribution"], "vmstat_delta");
            // The watched unit is placed by the slice path it knows, read or
            // not; nothing read keeps every slice baseline.
            assert_eq!(o[0].detail["victim_slice"], "qontinui-runner.slice");
            if want != "read" {
                assert_eq!(next.slices, o0.slices, "{want}");
            }
        }
    }

    /// Plan test 14: an observer-state file written before `slices` and
    /// `control_group` existed loads (serde default).
    #[test]
    fn an_old_state_file_without_slices_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("computer-observer.json");
        std::fs::write(
            &path,
            r#"{"computers":{"h":{"boot_id":"27cd9877-3ff3-47f0-8388-9880a31d5902","oom_kill_total":34078,"units":{"qontinui-runner.service":{"active_state":"active","result":"success","state_changed_at":null,"n_restarts":0,"cgroup_oom_kill":0}}}},"pending":{},"last_tick_at":"2026-11-27T00:14:30Z","last_tick_boot_id":null}"#,
        )
        .unwrap();
        let loaded = events::load(&path);
        assert!(!dir.path().join("computer-observer.json.corrupt").exists());
        let h = &loaded.computers["h"];
        assert_eq!(h.oom_kill_total, Some(34078));
        assert!(h.slices.is_empty());
        assert_eq!(h.units["qontinui-runner.service"].control_group, None);

        // And the new fields round-trip.
        let mut st = loaded.clone();
        let r = read(
            "2026-11-27T00:15:00.123456Z",
            vec![slice("agent-builds.slice", AGENT, 289, &[("agent-build-a.scope", Some(0))])],
        );
        let (_, next) = derive(Some(h), &input(&[], Some(34078), Some(&r)), t("2026-11-27T00:15:00Z"));
        st.computers.insert("h".into(), next);
        events::save(&path, &st).unwrap();
        assert_eq!(events::load(&path), st);
        assert_eq!(
            events::load(&path).computers["h"].slices["agent-builds.slice"].observed_at,
            t("2026-11-27T00:15:00.123456Z")
        );
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// A cgroup tree with `agent-builds.slice` under the user manager and
    /// `n` live `agent-build-NN.scope` children.
    fn agent_tree(root: &Path, slice_oom: u64, n: usize) {
        write(root, "cgroup.controllers", "cpuset cpu io memory pids\n");
        let dir = "user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice";
        write(root, &format!("{dir}/memory.current"), "1024\n");
        write(
            root,
            &format!("{dir}/memory.events"),
            &format!("low 0\nhigh 7\nmax 2\noom 3\noom_kill {slice_oom}\noom_group_kill 0\n"),
        );
        for i in 0..n {
            write(
                root,
                &format!("{dir}/agent-build-{i:02}.scope/memory.events"),
                "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\n",
            );
        }
        // Not cgroups of units: ignored.
        write(root, &format!("{dir}/cgroup.procs"), "");
    }

    /// Plan test 12: zero children, one, 64 and 65 — the last is truncated to
    /// a persisted child map of 64.
    #[test]
    fn the_child_listing_is_capped_at_64() {
        for (n, kept, truncated) in [(0, 0, false), (1, 1, false), (64, 64, false), (65, 64, true)] {
            let d = tempfile::tempdir().unwrap();
            agent_tree(d.path(), 289, n);
            let r = read_from(
                d.path(),
                &BTreeMap::new(),
                &BTreeSet::new(),
                Some(1000),
                t("2026-11-27T00:15:00Z"),
            );
            assert_eq!(r.source, SliceSource::Read);
            assert_eq!(r.slices.len(), 1, "{n}");
            let s = &r.slices[0];
            assert_eq!(s.name, "agent-builds.slice");
            assert_eq!(
                s.cgroup,
                "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice"
            );
            assert_eq!(s.oom_kill, 289);
            assert_eq!(s.children.len(), kept, "{n}");
            assert_eq!(s.children_truncated, truncated, "{n}");
            let (_, obs) = derive(None, &input(&[], Some(1), Some(&r)), t("2026-11-27T00:15:00Z"));
            assert_eq!(obs.slices["agent-builds.slice"].children.len(), kept, "{n}");
        }
        // A truncated listing says so on the slice's events.
        let d = tempfile::tempdir().unwrap();
        agent_tree(d.path(), 289, 65);
        let r0 = read_from(d.path(), &BTreeMap::new(), &BTreeSet::new(), Some(1000), t("2026-11-27T00:14:30Z"));
        agent_tree(d.path(), 290, 65);
        let r1 = read_from(d.path(), &BTreeMap::new(), &BTreeSet::new(), Some(1000), t("2026-11-27T00:15:00Z"));
        let (ev, _) = two_ticks(&[], (5, &r0), (Some(6), &r1));
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(o[0].detail["children_truncated"], true);
        assert_eq!(o[0].detail["attribution"], "slice_memory_events");
    }

    /// No unified hierarchy: `not_supported`, never an empty `read`.
    #[test]
    fn a_tree_without_cgroup_v2_is_not_supported() {
        let d = tempfile::tempdir().unwrap();
        let r = read_from(d.path(), &BTreeMap::new(), &BTreeSet::new(), Some(1000), t("2026-11-27T00:15:00Z"));
        assert_eq!(r.source, SliceSource::NotSupported);
        assert_eq!(r.source.as_str(), "not_supported");
        assert!(r.slices.is_empty());
    }

    /// Plan test 22: a capped child set is the same set whatever order the
    /// directory is listed in, and children tracked last tick are kept first.
    #[test]
    fn a_capped_child_set_is_stable_under_shuffled_listings() {
        let names: Vec<String> = (0..70).map(|i| format!("c{i:02}.scope")).collect();
        let mut reversed = names.clone();
        reversed.reverse();
        let mut interleaved: Vec<String> = names.iter().step_by(2).cloned().collect();
        interleaved.extend(names.iter().skip(1).step_by(2).cloned());
        let none = BTreeSet::new();
        let (a, ta) = select_children(names.clone(), &none, &none, 64);
        let (b, _) = select_children(reversed, &none, &none, 64);
        let (c, _) = select_children(interleaved, &none, &none, 64);
        assert!(ta);
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a.first().map(String::as_str), Some("c00.scope"));
        assert_eq!(a.last().map(String::as_str), Some("c63.scope"));

        // Next tick: c00..c09 are gone and twenty names that sort BEFORE the
        // tracked ones appear. The 54 tracked survivors stay; 10 of the new
        // ones fill in by name.
        let tracked: BTreeSet<String> = a.iter().cloned().collect();
        let mut next: Vec<String> = (10..70).map(|i| format!("c{i:02}.scope")).collect();
        next.extend((0..20).map(|i| format!("a{i:02}.scope")));
        let (k1, t1) = select_children(next.clone(), &tracked, &none, 64);
        next.reverse();
        let (k2, _) = select_children(next, &tracked, &none, 64);
        assert!(t1);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 64);
        assert!((10..64).all(|i| k1.contains(&format!("c{i:02}.scope"))));
        assert!((0..10).all(|i| k1.contains(&format!("a{i:02}.scope"))));
        assert!(!k1.contains(&"a10.scope".to_string()));
        assert!(!k1.contains(&"c64.scope".to_string()));
    }

    /// Plan test 18 (Phase 1 part): a watched unit listed as a slice child is
    /// excluded from Layer B and does not count toward the cap.
    #[test]
    fn watched_unit_children_are_excluded_and_uncapped() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(root, "cgroup.controllers", "memory pids\n");
        write(root, "ci.slice/ci-runners.slice/memory.events", "oom_kill 3\n");
        let mut watched_names = BTreeSet::new();
        for i in 0..70 {
            let unit = format!("actions.runner.example-org-r{i:02}.fleetbox.service");
            write(
                root,
                &format!("ci.slice/ci-runners.slice/{unit}/memory.events"),
                "oom_kill 1\n",
            );
            watched_names.insert(unit);
        }
        write(root, "ci.slice/ci-runners.slice/run-x.scope/memory.events", "oom_kill 0\n");
        let r = read_from(root, &BTreeMap::new(), &watched_names, Some(1000), t("2026-11-27T00:15:00Z"));
        let s = &r.slices[0];
        assert_eq!(s.name, "ci-runners.slice");
        assert_eq!(s.cgroup, "/ci.slice/ci-runners.slice");
        assert_eq!(
            s.children,
            vec![ChildRead {
                name: "run-x.scope".into(),
                oom_kill: Some(0)
            }]
        );
        assert!(!s.children_truncated, "watched units do not count toward the cap");
    }

    #[test]
    fn cgroup_paths_nest_only_at_a_component_boundary() {
        assert!(is_under("/a.slice/b.scope", "/a.slice"));
        assert!(!is_under("/a.slice", "/a.slice"));
        assert!(!is_under("/a.slice-x/b.scope", "/a.slice"));
        assert!(!is_under("/b.slice", "/a.slice"));
    }

    // ---- Layer C (Phase 2) ----------------------------------------------

    const SCOPE: &str = "agent-build-1795734446-543417-858931110.scope";

    /// One `journalctl --user -o json --output-fields=USER_UNIT,…` line.
    fn jline(unit: &str, realtime: u64, boot: &str) -> String {
        format!(
            r#"{{"__CURSOR":"s=f9;i=1","__REALTIME_TIMESTAMP":"{realtime}","__MONOTONIC_TIMESTAMP":"3131979535372","_BOOT_ID":"{boot}","USER_UNIT":"{unit}","_SOURCE_REALTIME_TIMESTAMP":"{realtime}"}}"#
        )
    }

    fn journal(lines: &[String]) -> JournalRead {
        journal_oom::parse_user_journal(&lines.join("\n"))
    }

    fn with_journal<'a>(base: ObsInput<'a>, j: &'a JournalRead) -> ObsInput<'a> {
        ObsInput {
            journal: Some(j),
            ..base
        }
    }

    /// The baseline tick for the Layer C tests: `agent-builds.slice` at 288
    /// with `children` listed.
    fn baseline(children: &[(&str, Option<u64>)]) -> ComputerObs {
        let r0 = read(
            "2026-11-27T00:14:40Z",
            vec![slice("agent-builds.slice", AGENT, 288, children)],
        );
        derive(None, &input(&[], Some(34078), Some(&r0)), t("2026-11-27T00:14:40Z")).1
    }

    /// Plan test 1 — the incident. Last tick listed the build scope under
    /// `agent-builds.slice`; the kill ended it, so this tick it is gone; the
    /// slice counter moved 288 → 289 and vmstat 34078 → 34079; the user
    /// journal names the scope (and, as systemd does, its ancestor slices).
    /// Exactly one event, naming the scope; no `vmstat_delta` event.
    #[test]
    fn the_incident_names_the_dead_build_scope() {
        let d = tempfile::tempdir().unwrap();
        agent_tree(d.path(), 288, 0);
        let dir = "user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice";
        write(
            d.path(),
            &format!("{dir}/{SCOPE}/memory.events"),
            "low 0\nhigh 900\nmax 40\noom 1\noom_kill 0\n",
        );
        let r0 = read_from(d.path(), &BTreeMap::new(), &BTreeSet::new(), Some(1000), t("2026-11-27T00:14:40Z"));
        assert_eq!(r0.slices[0].children.len(), 1);
        let (ev0, o0) = derive(None, &input(&[], Some(34078), Some(&r0)), t("2026-11-27T00:14:40Z"));
        assert!(ev0.is_empty());

        std::fs::remove_dir_all(d.path().join(dir).join(SCOPE)).unwrap();
        agent_tree(d.path(), 289, 0);
        let tracked: BTreeMap<String, BTreeSet<String>> = o0
            .slices
            .iter()
            .map(|(n, s)| (n.clone(), s.children.keys().cloned().collect()))
            .collect();
        let r1 = read_from(d.path(), &tracked, &BTreeSet::new(), Some(1000), t("2026-11-27T00:15:10Z"));
        assert!(r1.slices[0].children.is_empty(), "the scope is gone");

        // The window is decided before the derive, from the previous state.
        let w = journal_window(Some(&o0), &input(&[], Some(34079), Some(&r1)), &r1).unwrap();
        assert_eq!(w.since, t("2026-11-27T00:14:40Z"));
        // The read plus the 3 s lookahead.
        assert_eq!(w.until, t("2026-11-27T00:15:13Z"));
        assert!(w.user && w.kernel);

        let boot = "27cd98773ff347f083889880a31d5902";
        let j = journal(&[
            jline("-.slice", 1795738498276000, boot),
            jline("agent.slice", 1795738498276500, boot),
            jline("agent-builds.slice", 1795738498276900, boot),
            jline(SCOPE, 1795738498277337, boot),
        ]);
        let (ev, next) = derive(
            Some(&o0),
            &with_journal(input(&[], Some(34079), Some(&r1)), &j),
            t("2026-11-27T00:15:10Z"),
        );
        assert_eq!(ev.len(), 1, "{ev:?}");
        let e = &ev[0];
        assert_eq!(e.kind, "oom_kill");
        assert_eq!(e.detail["victim_unit"], "agent-build-1795734446-543417-858931110.scope");
        assert_eq!(e.detail["victim_slice"], "agent-builds.slice");
        assert_eq!(
            e.detail["victim_cgroup"],
            "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice/agent-build-1795734446-543417-858931110.scope"
        );
        assert_eq!(e.detail["attribution"], "user_journal_unit_oom");
        assert_eq!(e.detail["count"], 1);
        assert_eq!(e.detail["journal"], "read");
        assert_eq!(e.detail["slices"], "read");
        assert_eq!(e.observed_at, "2026-11-27T00:14:58Z");
        assert_eq!(
            e.client_event_id,
            event_id(
                "oom_kill",
                &[
                    "27cd9877-3ff3-47f0-8388-9880a31d5902",
                    "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice",
                    "agent-build-1795734446-543417-858931110.scope",
                    "user_journal_unit_oom",
                    "1795738498277337"
                ]
            )
        );
        assert_eq!(next.slices["agent-builds.slice"].oom_kill, 289);
    }

    /// Plan test 2 (Layer C part): B explains the whole delta, so no journal
    /// read is asked for.
    #[test]
    fn a_kill_b_explains_needs_no_journal_read() {
        let o0 = baseline(&[("agent-build-a.scope", Some(0))]);
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![slice("agent-builds.slice", AGENT, 289, &[("agent-build-a.scope", Some(1))])],
        );
        let w = journal_window(Some(&o0), &input(&[], Some(34079), Some(&r1)), &r1).unwrap();
        assert!(!w.user, "no user-journal read: {w:?}");
        assert!(w.kernel, "the kernel arm still names the child's comm");
    }

    /// Plan test 3: a child born and killed inside one tick was never listed,
    /// so its journal entry proves nothing: the remainder, unplaced.
    #[test]
    fn a_child_never_listed_is_not_placed_by_the_journal() {
        let o0 = baseline(&[]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 289, &[])]);
        let j = journal(&[jline("agent-build-brief.scope", 1795738498277337, BOOT)]);
        let (ev, _) = derive(
            Some(&o0),
            &with_journal(input(&[], Some(34079), Some(&r1)), &j),
            t("2026-11-27T00:15:10Z"),
        );
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].detail["victim_unit"], serde_json::Value::Null);
        assert_eq!(ev[0].detail["victim_slice"], "agent-builds.slice");
        assert_eq!(ev[0].detail["attribution"], "slice_memory_events");
        assert_eq!(ev[0].detail["journal"], "read");
        assert_eq!(ev[0].detail["count"], 1);
    }

    /// Plan test 4 (the derive side): an unreadable journal leaves the
    /// remainder standing with its count, saying `journal: "unreadable"`. The
    /// four ways a read fails are pinned in `journal_oom`'s tests.
    #[test]
    fn an_unreadable_journal_keeps_the_remainder_and_its_count() {
        let o0 = baseline(&[(SCOPE, Some(0))]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 291, &[])]);
        for j in [Some(JournalRead::Unreadable), None] {
            let inp = ObsInput {
                journal: j.as_ref(),
                ..input(&[], Some(34081), Some(&r1))
            };
            let (ev, _) = derive(Some(&o0), &inp, t("2026-11-27T00:15:10Z"));
            assert_eq!(ev.len(), 1, "{ev:?}");
            assert_eq!(ev[0].detail["journal"], "unreadable");
            assert_eq!(ev[0].detail["count"], 3);
            assert_eq!(ev[0].detail["victim_unit"], serde_json::Value::Null);
        }
    }

    /// Plan test 5: an entry naming a unit listed under a DIFFERENT slice is
    /// not accepted for this one.
    #[test]
    fn an_entry_listed_under_another_slice_is_not_accepted() {
        let r0 = read(
            "2026-11-27T00:14:40Z",
            vec![
                slice("agent-builds.slice", AGENT, 288, &[]),
                slice("qontinui-runner.slice", RUNNER_SLICE, 7, &[("other-x.scope", Some(0))]),
            ],
        );
        let (_, o0) = derive(None, &input(&[], Some(34078), Some(&r0)), t("2026-11-27T00:14:40Z"));
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![
                slice("agent-builds.slice", AGENT, 289, &[]),
                slice("qontinui-runner.slice", RUNNER_SLICE, 7, &[]),
            ],
        );
        let j = journal(&[jline("other-x.scope", 1795738498277337, BOOT)]);
        let (ev, _) = derive(
            Some(&o0),
            &with_journal(input(&[], Some(34079), Some(&r1)), &j),
            t("2026-11-27T00:15:10Z"),
        );
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].detail["victim_slice"], "agent-builds.slice");
        assert_eq!(ev[0].detail["victim_unit"], serde_json::Value::Null);
    }

    /// Plan test 16: neither a unit `Description` (argv) nor the journal
    /// `MESSAGE` ever reaches an event, even when the read returns them.
    #[test]
    fn no_description_or_message_reaches_an_event() {
        let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
        let line = format!(
            r#"{{"__REALTIME_TIMESTAMP":"1795738498277337","_BOOT_ID":"27cd98773ff347f083889880a31d5902","USER_UNIT":"{SCOPE}","MESSAGE":"{SCOPE}: A process of this unit has been killed by the OOM killer. {token}","Description":"[systemd-run] cargo test --token {token}","UNIT_DESCRIPTION":"{token}"}}"#
        );
        let j = journal_oom::parse_user_journal(&line);
        let o0 = baseline(&[(SCOPE, Some(0))]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 289, &[])]);
        let (ev, _) = derive(
            Some(&o0),
            &with_journal(input(&[], Some(34079), Some(&r1)), &j),
            t("2026-11-27T00:15:10Z"),
        );
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].detail["attribution"], "user_journal_unit_oom");
        let wire = serde_json::to_string(&ev).unwrap();
        assert!(!wire.contains(token), "{wire}");
        assert!(!wire.contains("killed by the OOM killer"), "{wire}");
        assert!(!wire.contains("systemd-run"), "{wire}");
        assert!(!format!("{j:?}").contains(token), "the parser kept only the three fields");
    }

    /// Plan test 18 (Layer C part): a watched unit is never placed by the
    /// journal, even if an older listing held its name.
    #[test]
    fn the_journal_never_names_a_watched_unit() {
        let units = watched(RUNNER_SERVICE, Some(0));
        let r0 = read(
            "2026-11-27T00:14:40Z",
            vec![slice(
                "qontinui-runner.slice",
                RUNNER_SLICE,
                4,
                &[("qontinui-runner.service", Some(0))],
            )],
        );
        let (_, o0) = derive(None, &input(&units, Some(10), Some(&r0)), t("2026-11-27T00:14:40Z"));
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("qontinui-runner.slice", RUNNER_SLICE, 5, &[])]);
        let j = journal(&[jline("qontinui-runner.service", 1795738498277337, BOOT)]);
        let (ev, _) = derive(
            Some(&o0),
            &with_journal(input(&units, Some(11), Some(&r1)), &j),
            t("2026-11-27T00:15:10Z"),
        );
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(o[0].detail["attribution"], "slice_memory_events");
    }

    /// Plan test 19: an entry older than the baseline, entries beyond the
    /// unexplained count, and an entry from another boot are not accepted.
    #[test]
    fn journal_entries_are_bounded_by_window_count_and_boot() {
        let o0 = baseline(&[
            ("agent-build-a.scope", Some(0)),
            ("agent-build-b.scope", Some(0)),
            ("agent-build-c.scope", Some(0)),
            ("agent-build-d.scope", Some(0)),
        ]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 290, &[])]);
        // 00:14:40Z is 1795738480 s.
        let j = journal(&[
            jline("agent-build-a.scope", 1795738479999999, BOOT),
            jline("agent-build-b.scope", 1795738485000000, "0000000000000000000000000000beef"),
            jline("agent-build-c.scope", 1795738490000000, BOOT),
            jline("agent-build-d.scope", 1795738495000000, BOOT),
            jline("agent-build-a.scope", 1795738509000000, BOOT),
            // At the read itself (00:15:10Z): inside the lookahead, but the
            // count of 2 is already named by the earlier entries.
            jline("agent-build-b.scope", 1795738510000000, BOOT),
        ]);
        let (ev, _) = derive(
            Some(&o0),
            &with_journal(input(&[], Some(34080), Some(&r1)), &j),
            t("2026-11-27T00:15:10Z"),
        );
        let named: Vec<&str> = ev.iter().filter_map(|e| e.detail["victim_unit"].as_str()).collect();
        assert_eq!(named, vec!["agent-build-c.scope", "agent-build-d.scope"], "{ev:?}");
        assert_eq!(ev.len(), 2, "the delta of 2 is fully named: {ev:?}");
    }

    /// Plan test 20: a tick whose early return advanced the loop's
    /// `last_tick_at` past the slice baselines consumed nothing, so the next
    /// window still starts at the baseline and the kill in between is named.
    #[test]
    fn the_window_starts_at_the_slice_baseline_not_the_last_tick() {
        let o0 = baseline(&[(SCOPE, Some(0))]);
        let mut state = events::ObserverState::default();
        state.computers.insert("h".into(), o0.clone());
        // Two ticks returned early (no device file): the loop clock moved on.
        state.last_tick_at = Some("2026-11-27T00:15:40Z".into());
        let r = read("2026-11-27T00:16:10Z", vec![slice("agent-builds.slice", AGENT, 289, &[])]);
        let prev = state.computers.get("h");
        let w = journal_window(prev, &input(&[], Some(34079), Some(&r)), &r).unwrap();
        assert_eq!(w.since, t("2026-11-27T00:14:40Z"));
        let j = journal(&[jline(SCOPE, 1795738520000000, BOOT)]); // 00:15:20Z
        let (ev, _) = derive(prev, &with_journal(input(&[], Some(34079), Some(&r)), &j), t("2026-11-27T00:16:10Z"));
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].detail["victim_unit"], SCOPE);
    }

    /// Plan test 23 (Layer C part): a system-root slice's messages are in the
    /// system journal, which this user cannot read — no window, and its
    /// remainder says `journal: "unreadable"`. So does another user's slice.
    #[test]
    fn a_slice_outside_this_users_manager_forks_no_journal_read() {
        for cgroup in [
            CI,
            "/user.slice/user-1001.slice/user@1001.service/agent.slice/agent-builds.slice",
        ] {
            let r0 = read("2026-11-27T00:14:40Z", vec![slice("agent-builds.slice", cgroup, 3, &[("x.scope", Some(0))])]);
            let (_, o0) = derive(None, &input(&[], Some(20), Some(&r0)), t("2026-11-27T00:14:40Z"));
            let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", cgroup, 4, &[])]);
            let w = journal_window(Some(&o0), &input(&[], Some(21), Some(&r1)), &r1).unwrap();
            assert!(!w.user, "no user-journal fork for {cgroup}");
            let j = journal(&[jline("x.scope", 1795738500000000, BOOT)]);
            let (ev, _) = derive(Some(&o0), &with_journal(input(&[], Some(21), Some(&r1)), &j), t("2026-11-27T00:15:10Z"));
            assert_eq!(ev.len(), 1, "{ev:?}");
            assert_eq!(ev[0].detail["journal"], "unreadable", "{cgroup}");
            assert_eq!(ev[0].detail["victim_unit"], serde_json::Value::Null);
        }
        assert_eq!(manager_uid(AGENT), Some(1000));
        assert_eq!(manager_uid(CI), None);
    }

    /// Plan test 24 (reboot part): after a reboot Layer C is skipped — no
    /// listing from this boot proves a placement — and no window is asked for.
    #[test]
    fn after_a_reboot_the_journal_places_nothing() {
        let o0 = baseline(&[(SCOPE, Some(0))]);
        let new_boot = "11111111-2222-4333-8444-555555555555";
        let r1 = read("2026-11-27T03:01:00Z", vec![slice("agent-builds.slice", AGENT, 1, &[])]);
        let base = ObsInput {
            boot_id: Some(new_boot),
            booted_at: Some("2026-11-27T03:00:00Z"),
            ..input(&[], Some(1), Some(&r1))
        };
        let w = journal_window(Some(&o0), &base, &r1).unwrap();
        assert!(!w.user, "no user-journal fork after a reboot");
        let j = journal(&[jline(SCOPE, 1795748410000000, "11111111222243338444555555555555")]);
        let (ev, _) = derive(Some(&o0), &with_journal(base, &j), t("2026-11-27T03:01:00Z"));
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(o[0].detail["attribution"], "slice_memory_events_since_boot");
        assert_eq!(o[0].detail["victim_unit"], serde_json::Value::Null);
        assert_eq!(o[0].detail["journal"], "no_proof");
    }

    /// Plan test 25 (Layer C part): the clamp trims the remainder, then C's
    /// entries, before B.
    #[test]
    fn the_clamp_trims_journal_names_before_children() {
        let o0 = baseline(&[("agent-build-a.scope", Some(0)), (SCOPE, Some(0))]);
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![slice("agent-builds.slice", AGENT, 291, &[("agent-build-a.scope", Some(1))])],
        );
        let j = journal(&[jline(SCOPE, 1795738498277337, BOOT)]);
        let run = |v: u64| {
            let (ev, _) = derive(Some(&o0), &with_journal(input(&[], Some(v), Some(&r1)), &j), t("2026-11-27T00:15:10Z"));
            ev.iter()
                .map(|e| (e.detail["attribution"].as_str().unwrap().to_string(), e.detail["count"].as_u64().unwrap()))
                .collect::<Vec<_>>()
        };
        let s = |a: &str, n: u64| (a.to_string(), n);
        assert_eq!(run(34081), vec![s("cgroup_memory_events", 1), s("user_journal_unit_oom", 1), s("slice_memory_events", 1)]);
        assert_eq!(run(34080), vec![s("cgroup_memory_events", 1), s("user_journal_unit_oom", 1)]);
        assert_eq!(run(34079), vec![s("cgroup_memory_events", 1)]);
    }

    // ---- kernel records (Phase 3) ---------------------------------------

    fn krec(memcg: &str, comm: &str, realtime: u64, boot: &str) -> KernelOomRecord {
        KernelOomRecord {
            task_memcg: memcg.into(),
            comm: comm.into(),
            realtime_usec: realtime,
            boot_id: boot.into(),
        }
    }

    fn with_kernel<'a>(base: ObsInput<'a>, k: &'a KernelRead) -> ObsInput<'a> {
        ObsInput {
            kernel: Some(k),
            ..base
        }
    }

    /// Plan test 15: an exact `task_memcg` match names `victim_comm`; a
    /// record elsewhere names nothing (`not_found`); an unreadable kernel log
    /// says so; a log never asked leaves both fields out.
    #[test]
    fn victim_comm_comes_only_from_an_exact_task_memcg_match() {
        let o0 = baseline(&[("agent-build-a.scope", Some(0))]);
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![slice("agent-builds.slice", AGENT, 289, &[("agent-build-a.scope", Some(1))])],
        );
        let at = 1795738498277337; // 00:14:58Z
        let exact = KernelRead::Read(vec![krec(
            "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice/agent-build-a.scope",
            "rustc",
            at,
            "27cd98773ff347f083889880a31d5902",
        )]);
        let elsewhere = KernelRead::Read(vec![krec(
            "/system.slice/postgres.service",
            "postgres",
            at,
            "27cd98773ff347f083889880a31d5902",
        )]);
        let unreadable = KernelRead::Unreadable;
        let one = |k: Option<&KernelRead>| {
            let inp = ObsInput {
                kernel: k,
                ..input(&[], Some(34079), Some(&r1))
            };
            let (ev, _) = derive(Some(&o0), &inp, t("2026-11-27T00:15:10Z"));
            assert_eq!(ev.len(), 1, "{ev:?}");
            assert_eq!(ev[0].detail["attribution"], "cgroup_memory_events");
            ev[0].detail.clone()
        };
        let d = one(Some(&exact));
        assert_eq!(d["victim_comm"], "rustc");
        assert_eq!(d["victim_comm_source"], "kernel_log");
        let d = one(Some(&elsewhere));
        assert_eq!(d["victim_comm"], serde_json::Value::Null);
        assert_eq!(d["victim_comm_source"], "not_found");
        let d = one(Some(&unreadable));
        assert_eq!(d["victim_comm"], serde_json::Value::Null);
        assert_eq!(d["victim_comm_source"], "unreadable");
        let d = one(None);
        assert!(d.get("victim_comm").is_none() && d.get("victim_comm_source").is_none(), "{d}");
    }

    /// Plan test 26: a readable record whose `task_memcg` is under the slice
    /// places what would have been the remainder — the unit directly under
    /// the slice — with its comm.
    #[test]
    fn a_kernel_record_places_the_remainder() {
        let o0 = baseline(&[]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 290, &[])]);
        let boot = "27cd98773ff347f083889880a31d5902";
        let k = KernelRead::Read(vec![
            krec(
                "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice/agent-build-x.scope",
                "rustc",
                1795738498277337,
                boot,
            ),
            // Before the baseline (00:14:40Z): not this tick's kill.
            krec(
                "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice/agent-build-old.scope",
                "cc1plus",
                1795738470000000,
                boot,
            ),
        ]);
        let (ev, _) = derive(
            Some(&o0),
            &with_kernel(input(&[], Some(34080), Some(&r1)), &k),
            t("2026-11-27T00:15:10Z"),
        );
        assert_eq!(ev.len(), 2, "{ev:?}");
        let d = &ev[0].detail;
        assert_eq!(d["victim_unit"], "agent-build-x.scope");
        assert_eq!(d["attribution"], "kernel_oom_record");
        assert_eq!(d["count"], 1);
        assert_eq!(d["victim_comm"], "rustc");
        assert_eq!(d["victim_comm_source"], "kernel_log");
        assert_eq!(d["victim_slice"], "agent-builds.slice");
        assert_eq!(
            d["victim_cgroup"],
            "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice/agent-build-x.scope"
        );
        assert_eq!(ev[0].observed_at, "2026-11-27T00:14:58Z");
        assert_eq!(
            ev[0].client_event_id,
            event_id(
                "oom_kill",
                &[
                    "27cd9877-3ff3-47f0-8388-9880a31d5902",
                    "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice",
                    "agent-build-x.scope",
                    "kernel_oom_record",
                    "1795738498277337"
                ]
            )
        );
        // The second kill stays the remainder; its slice path matches no
        // record exactly.
        assert_eq!(ev[1].detail["attribution"], "slice_memory_events");
        assert_eq!(ev[1].detail["count"], 1);
        assert_eq!(ev[1].detail["victim_comm_source"], "not_found");
    }

    /// Plan test 23 (Phase 3 part): readable with no match is `not_found`,
    /// unreadable is `unreadable`, and the residual outside every slice was
    /// never asked, so it carries no `victim_comm_source`.
    #[test]
    fn the_comm_states_stay_distinguishable() {
        let o0 = baseline(&[]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 289, &[])]);
        for (k, want) in [
            (KernelRead::Read(vec![]), "not_found"),
            (KernelRead::Unreadable, "unreadable"),
        ] {
            let (ev, _) = derive(
                Some(&o0),
                &with_kernel(input(&[], Some(34080), Some(&r1)), &k),
                t("2026-11-27T00:15:10Z"),
            );
            assert_eq!(ev.len(), 2, "{ev:?}");
            assert_eq!(ev[0].detail["attribution"], "slice_memory_events");
            assert_eq!(ev[0].detail["victim_comm_source"], want);
            assert_eq!(ev[1].detail["attribution"], "vmstat_delta");
            assert!(ev[1].detail.get("victim_comm_source").is_none());
        }
    }

    /// A kernel record never re-places a kill a watched unit, B or C already
    /// accounts for, and never one from another boot.
    #[test]
    fn a_kernel_record_places_only_what_nothing_else_named() {
        let units = watched(RUNNER_SERVICE, Some(0));
        let killed = watched(RUNNER_SERVICE, Some(1));
        let r0 = read(
            "2026-11-27T00:14:40Z",
            vec![slice("qontinui-runner.slice", RUNNER_SLICE, 4, &[("b.scope", Some(0)), ("c.scope", Some(0))])],
        );
        let (_, o0) = derive(None, &input(&units, Some(10), Some(&r0)), t("2026-11-27T00:14:40Z"));
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![slice("qontinui-runner.slice", RUNNER_SLICE, 8, &[("b.scope", Some(1))])],
        );
        let boot = "27cd98773ff347f083889880a31d5902";
        let at = 1795738498000000;
        let k = KernelRead::Read(vec![
            krec(&format!("{RUNNER_SLICE}/qontinui-runner.service"), "qontinui-runner", at, boot),
            krec(&format!("{RUNNER_SLICE}/b.scope"), "rustc", at + 1, boot),
            krec(&format!("{RUNNER_SLICE}/c.scope"), "ld", at + 2, boot),
            krec(&format!("{RUNNER_SLICE}/d.scope"), "cc", at + 3, "0000000000000000000000000000beef"),
        ]);
        let j = journal(&[jline("c.scope", at + 2, boot)]);
        let inp = ObsInput {
            kernel: Some(&k),
            journal: Some(&j),
            ..input(&killed, Some(14), Some(&r1))
        };
        let (ev, _) = derive(Some(&o0), &inp, t("2026-11-27T00:15:10Z"));
        let got: Vec<(String, String)> = ooms(&ev)
            .iter()
            .map(|e| {
                (
                    e.detail["attribution"].as_str().unwrap().to_string(),
                    e.detail["victim_unit"].as_str().unwrap_or("-").to_string(),
                )
            })
            .collect();
        // Delta 4 = the watched unit 1 + B (b) 1 + C (c) 1 + one unplaced.
        assert_eq!(
            got,
            vec![
                ("cgroup_memory_events".to_string(), "qontinui-runner.service".to_string()),
                ("cgroup_memory_events".to_string(), "b.scope".to_string()),
                ("user_journal_unit_oom".to_string(), "c.scope".to_string()),
                ("slice_memory_events".to_string(), "-".to_string()),
            ],
            "{ev:?}"
        );
        let b = &ooms(&ev)[1].detail;
        assert_eq!(b["victim_comm"], "rustc");
        let c = &ooms(&ev)[2].detail;
        assert_eq!(c["victim_comm"], "ld");
    }

    /// After a reboot the kernel record still places a kill: `task_memcg` is
    /// its own proof, unlike a listing from the previous boot.
    #[test]
    fn after_a_reboot_a_kernel_record_still_places() {
        let o0 = baseline(&[]);
        let new_boot = "11111111-2222-4333-8444-555555555555";
        let r1 = read("2026-11-27T03:01:00Z", vec![slice("agent-builds.slice", AGENT, 1, &[])]);
        let base = ObsInput {
            boot_id: Some(new_boot),
            booted_at: Some("2026-11-27T03:00:00Z"),
            ..input(&[], Some(1), Some(&r1))
        };
        let w = journal_window(Some(&o0), &base, &r1).unwrap();
        assert!(w.kernel && !w.user, "{w:?}");
        let k = KernelRead::Read(vec![krec(
            &format!("{AGENT}/agent-build-y.scope"),
            "rustc",
            1795748410000000,
            "11111111222243338444555555555555",
        )]);
        let (ev, _) = derive(Some(&o0), &with_kernel(base, &k), t("2026-11-27T03:01:00Z"));
        let o = ooms(&ev);
        assert_eq!(o.len(), 1, "{ev:?}");
        assert_eq!(o[0].detail["attribution"], "kernel_oom_record");
        assert_eq!(o[0].detail["victim_unit"], "agent-build-y.scope");
    }

    // ---- pre-PR review fixes --------------------------------------------

    /// Review A1: a late journal entry for last tick's B-named kill (logged
    /// after that tick's read, so inside this window) does not name this
    /// tick's unexplained kill: the unit is listed now with a readable
    /// counter that did not move.
    #[test]
    fn a_late_entry_for_an_earlier_kill_names_nothing_now() {
        let o0 = baseline(&[("agent-build-a.scope", Some(0))]);
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![slice("agent-builds.slice", AGENT, 289, &[("agent-build-a.scope", Some(1))])],
        );
        let (ev1, o1) = derive(Some(&o0), &input(&[], Some(34079), Some(&r1)), t("2026-11-27T00:15:10Z"));
        assert_eq!(ev1[0].detail["victim_unit"], "agent-build-a.scope");
        let r2 = read(
            "2026-11-27T00:15:40Z",
            vec![slice("agent-builds.slice", AGENT, 290, &[("agent-build-a.scope", Some(1))])],
        );
        // 00:15:10.2Z: systemd logged the first kill a moment after the read.
        let j = journal(&[jline("agent-build-a.scope", 1795738510200000, BOOT)]);
        let (ev, _) = derive(
            Some(&o1),
            &with_journal(input(&[], Some(34080), Some(&r2)), &j),
            t("2026-11-27T00:15:40Z"),
        );
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].detail["attribution"], "slice_memory_events");
        assert_eq!(ev[0].detail["victim_unit"], serde_json::Value::Null);
    }

    /// Review A2: same-named units under two slices (the system root and a
    /// user manager) never share an event id.
    #[test]
    fn same_named_children_of_two_slices_get_distinct_ids() {
        let r0 = read(
            "2026-11-27T00:14:40Z",
            vec![
                slice("ci-runners.slice", CI, 3, &[("build-1.scope", Some(0))]),
                slice("agent-builds.slice", AGENT, 10, &[("build-1.scope", Some(0))]),
            ],
        );
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![
                slice("ci-runners.slice", CI, 4, &[("build-1.scope", Some(1))]),
                slice("agent-builds.slice", AGENT, 11, &[("build-1.scope", Some(1))]),
            ],
        );
        let (ev, _) = two_ticks(&[], (100, &r0), (Some(102), &r1));
        let o = ooms(&ev);
        assert_eq!(o.len(), 2, "{ev:?}");
        assert_ne!(o[0].client_event_id, o[1].client_event_id);
        assert_eq!(
            o[0].client_event_id,
            event_id(
                "oom_kill",
                &[BOOT, "/ci.slice/ci-runners.slice", "build-1.scope", "cgroup_memory_events", "0..1"]
            )
        );
        assert_eq!(
            o[1].client_event_id,
            event_id(
                "oom_kill",
                &[
                    BOOT,
                    "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice",
                    "build-1.scope",
                    "cgroup_memory_events",
                    "0..1"
                ]
            )
        );
    }

    /// Review A4: a child trimmed because it was read after its slice keeps
    /// that much of its old baseline, so the next tick names it.
    #[test]
    fn a_clamp_trimmed_child_is_named_next_tick() {
        let o0 = baseline(&[("agent-build-a.scope", Some(0))]);
        // The kill landed between the slice read and the child read.
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![slice("agent-builds.slice", AGENT, 288, &[("agent-build-a.scope", Some(1))])],
        );
        let (ev1, o1) = derive(Some(&o0), &input(&[], Some(34078), Some(&r1)), t("2026-11-27T00:15:10Z"));
        assert!(ooms(&ev1).is_empty(), "{ev1:?}");
        assert_eq!(o1.slices["agent-builds.slice"].children["agent-build-a.scope"], 0);
        let r2 = read(
            "2026-11-27T00:15:40Z",
            vec![slice("agent-builds.slice", AGENT, 289, &[("agent-build-a.scope", Some(1))])],
        );
        let (ev2, o2) = derive(Some(&o1), &input(&[], Some(34079), Some(&r2)), t("2026-11-27T00:15:40Z"));
        let o = ooms(&ev2);
        assert_eq!(o.len(), 1, "{ev2:?}");
        assert_eq!(o[0].detail["victim_unit"], "agent-build-a.scope");
        assert_eq!(o[0].detail["attribution"], "cgroup_memory_events");
        assert_eq!(o2.slices["agent-builds.slice"].children["agent-build-a.scope"], 1);
    }

    /// Review A5: a watched unit whose ControlGroup was never known says
    /// `victim_cgroup: null`, not nothing.
    #[test]
    fn an_unplaced_watched_unit_says_victim_cgroup_null() {
        let before = watched(&CI_RUNNER_ACTIVE.replace(
            "ControlGroup=/ci.slice/ci-runners.slice/actions.runner.example-org-example-repo.fleetbox.service",
            "ControlGroup=",
        ), Some(0));
        let after = watched(CI_RUNNER_OOM, None);
        let (_, o0) = derive(None, &input(&before, Some(20), None), t("2026-11-27T00:14:40Z"));
        let (ev, _) = derive(Some(&o0), &input(&after, Some(21), None), t("2026-11-27T00:15:10Z"));
        let o = ooms(&ev);
        assert_eq!(o[0].detail["attribution"], "unit_result");
        assert_eq!(o[0].detail.get("victim_cgroup"), Some(&serde_json::Value::Null));
    }

    /// Review C2: `collect_from` end to end — a fixture cgroup tree, a fake
    /// `journalctl`, the pre-derive window, the forks, the parse, and the
    /// derive input they feed. The scope dies between two ticks; the user
    /// journal (probed readable) names it; the kernel log is unreadable.
    #[cfg(unix)]
    #[tokio::test]
    async fn collect_from_drives_the_window_the_forks_and_the_derive() {
        let d = tempfile::tempdir().unwrap();
        let fake = tempfile::tempdir().unwrap();
        let dir = "user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice";
        agent_tree(d.path(), 288, 0);
        write(
            d.path(),
            &format!("{dir}/{SCOPE}/memory.events"),
            "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\n",
        );
        let src = Sources {
            cgroup_root: d.path().to_path_buf(),
            euid: Some(1000),
            journalctl: journal_oom::Journalctl {
                program: "sh".into(),
                leading: vec![fake.path().join("journalctl.sh").to_string_lossy().into_owned()],
            },
            deadline: Duration::from_secs(5),
            lookahead: Duration::from_millis(1200),
        };
        // No previous state: no window, so no wait.
        let started = std::time::Instant::now();
        let r0 = collect_from(None, &input(&[], Some(34078), None), &src).await;
        assert!(started.elapsed() < Duration::from_millis(1000), "{:?}", started.elapsed());
        assert_eq!(r0.journal, None, "no previous state, no window");
        let (_, o0) = derive(None, &input(&[], Some(34078), r0.slices.as_ref()), t("2026-11-27T00:14:40Z"));

        // The kill: after the baseline was read, before the next one.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let killed_at = u64::try_from(Utc::now().timestamp_micros()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::remove_dir_all(d.path().join(dir).join(SCOPE)).unwrap();
        agent_tree(d.path(), 289, 0);
        let script = format!(
            r#"case "$*" in
  "--user -n 1"*) echo '{{"__CURSOR":"s=1","__REALTIME_TIMESTAMP":"1","_BOOT_ID":"27cd98773ff347f083889880a31d5902"}}' ;;
  "--user -o json --output-fields=USER_UNIT "*) echo '{{"__REALTIME_TIMESTAMP":"{killed_at}","_BOOT_ID":"27cd98773ff347f083889880a31d5902","USER_UNIT":"{SCOPE}"}}' ;;
  "-k "*) ;;
  *) exit 2 ;;
esac
"#
        );
        std::fs::write(fake.path().join("journalctl.sh"), script).unwrap();

        let base = input(&[], Some(34079), None);
        // Round-3 A4: a tick with a kill to place waits out the lookahead
        // after its counter read before it forks.
        let started = std::time::Instant::now();
        let r1 = collect_from(Some(&o0), &base, &src).await;
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(1200), "{took:?}");
        // Round-4 A5: and no longer than the lookahead plus the deadline.
        assert!(took < Duration::from_millis(1200 + 5000), "{took:?}");
        assert!(matches!(r1.journal, Some(JournalRead::Read(ref e)) if e.len() == 1), "{r1:?}");
        assert_eq!(r1.kernel, Some(KernelRead::Unreadable));
        let inp = ObsInput {
            slices: r1.slices.as_ref(),
            journal: r1.journal.as_ref(),
            kernel: r1.kernel.as_ref(),
            ..base
        };
        let (ev, _) = derive(Some(&o0), &inp, t("2026-11-27T00:15:10Z"));
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].detail["victim_unit"], SCOPE);
        assert_eq!(ev[0].detail["attribution"], "user_journal_unit_oom");
        assert_eq!(ev[0].detail["victim_comm_source"], "unreadable");
    }

    // ---- round-2 review fixes -------------------------------------------

    /// Round-2 A1: a child cut partly by its slice clamp carries the rest to
    /// the next tick, and that event gets its own id (keyed on the counter
    /// range), so coord does not dedupe it away.
    #[test]
    fn a_carried_child_remainder_is_a_distinct_event_next_tick() {
        let o0 = baseline(&[("agent-build-a.scope", Some(0))]);
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![slice("agent-builds.slice", AGENT, 289, &[("agent-build-a.scope", Some(2))])],
        );
        let (ev1, o1) = derive(Some(&o0), &input(&[], Some(34080), Some(&r1)), t("2026-11-27T00:15:10Z"));
        let b1: Vec<_> = ooms(&ev1).into_iter().filter(|e| e.detail["attribution"] == "cgroup_memory_events").collect();
        assert_eq!(b1.len(), 1, "{ev1:?}");
        assert_eq!(b1[0].detail["count"], 1);
        assert_eq!(o1.slices["agent-builds.slice"].children["agent-build-a.scope"], 1);
        let r2 = read(
            "2026-11-27T00:15:40Z",
            vec![slice("agent-builds.slice", AGENT, 290, &[("agent-build-a.scope", Some(2))])],
        );
        let (ev2, _) = derive(Some(&o1), &input(&[], Some(34081), Some(&r2)), t("2026-11-27T00:15:40Z"));
        let b2: Vec<_> = ooms(&ev2).into_iter().filter(|e| e.detail["attribution"] == "cgroup_memory_events").collect();
        assert_eq!(b2.len(), 1, "{ev2:?}");
        assert_eq!(b2[0].detail["victim_unit"], "agent-build-a.scope");
        assert_ne!(b1[0].client_event_id, b2[0].client_event_id);
        assert_eq!(
            b2[0].client_event_id,
            event_id(
                "oom_kill",
                &[
                    BOOT,
                    "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice",
                    "agent-build-a.scope",
                    "cgroup_memory_events",
                    "1..2"
                ]
            )
        );
    }

    /// Round-2 A2: what the vmstat clamp cuts is kept back in the next
    /// baselines, so those kills keep their slice (and child) placement when
    /// vmstat catches up next tick, instead of going to the residual.
    #[test]
    fn the_vmstat_clamp_carries_its_cut_into_the_next_baselines() {
        // A remainder cut: slice 288 -> 291, vmstat only +2.
        let o0 = baseline(&[]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 291, &[])]);
        let (ev1, o1) = derive(Some(&o0), &input(&[], Some(34080), Some(&r1)), t("2026-11-27T00:15:10Z"));
        assert_eq!(ooms(&ev1)[0].detail["count"], 2);
        assert_eq!(o1.slices["agent-builds.slice"].oom_kill, 290);
        let (ev2, _) = derive(Some(&o1), &input(&[], Some(34081), Some(&r1)), t("2026-11-27T00:15:40Z"));
        let o = ooms(&ev2);
        assert_eq!(o.len(), 1, "{ev2:?}");
        assert_eq!(o[0].detail["attribution"], "slice_memory_events");
        assert_eq!(o[0].detail["victim_slice"], "agent-builds.slice");

        // A child cut: the child's kill is counted by the slice and the child
        // but not yet by vmstat.
        let o0 = baseline(&[("agent-build-a.scope", Some(0))]);
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![slice("agent-builds.slice", AGENT, 289, &[("agent-build-a.scope", Some(1))])],
        );
        let (ev1, o1) = derive(Some(&o0), &input(&[], Some(34078), Some(&r1)), t("2026-11-27T00:15:10Z"));
        assert!(ooms(&ev1).is_empty(), "{ev1:?}");
        assert_eq!(o1.slices["agent-builds.slice"].oom_kill, 288);
        assert_eq!(o1.slices["agent-builds.slice"].children["agent-build-a.scope"], 0);
        let (ev2, _) = derive(Some(&o1), &input(&[], Some(34079), Some(&r1)), t("2026-11-27T00:15:40Z"));
        let o = ooms(&ev2);
        assert_eq!(o.len(), 1, "{ev2:?}");
        assert_eq!(o[0].detail["victim_unit"], "agent-build-a.scope");
    }

    /// Round-2 A5: an entry stamped a moment AFTER the counter read (systemd
    /// logs the kill after the kernel counts it) still names the kill that
    /// read counted — and does not name another one next tick.
    #[test]
    fn an_entry_just_after_the_read_names_its_kill_once() {
        let o0 = baseline(&[(SCOPE, Some(0)), ("agent-build-b.scope", Some(0))]);
        let r1 = read(
            "2026-11-27T00:15:10Z",
            vec![slice("agent-builds.slice", AGENT, 289, &[("agent-build-b.scope", Some(0))])],
        );
        // 00:15:10.5Z: half a second after the read.
        let j = journal(&[jline(SCOPE, 1795738510500000, BOOT)]);
        let w = journal_window(Some(&o0), &input(&[], Some(34079), Some(&r1)), &r1).unwrap();
        assert!(w.until > t("2026-11-27T00:15:10.5Z"), "{w:?}");
        let (ev1, o1) = derive(
            Some(&o0),
            &with_journal(input(&[], Some(34079), Some(&r1)), &j),
            t("2026-11-27T00:15:10Z"),
        );
        assert_eq!(ev1.len(), 1, "{ev1:?}");
        assert_eq!(ev1[0].detail["victim_unit"], SCOPE);
        assert_eq!(o1.slices["agent-builds.slice"].named.get(SCOPE), Some(&1795738510500000));
        // Next tick: another kill, and the same entry is still in its window
        // (it starts at the previous read). It must not name this one.
        let o1 = ComputerObs {
            slices: o1
                .slices
                .iter()
                .map(|(n, s)| {
                    let mut s = s.clone();
                    s.children.insert(SCOPE.into(), 0);
                    (n.clone(), s)
                })
                .collect(),
            ..o1
        };
        let r2 = read("2026-11-27T00:15:40Z", vec![slice("agent-builds.slice", AGENT, 290, &[])]);
        let (ev2, _) = derive(
            Some(&o1),
            &with_journal(input(&[], Some(34080), Some(&r2)), &j),
            t("2026-11-27T00:15:40Z"),
        );
        assert_eq!(ev2.len(), 1, "{ev2:?}");
        assert_eq!(ev2[0].detail["attribution"], "slice_memory_events");
        assert_ne!(ev1[0].client_event_id, ev2[0].client_event_id);
    }

    /// Round-2 A7: `journal_window` subtracts the watched units' kills (its
    /// own `unit_kills`), so a kill the runner service's own event explains
    /// forks nothing.
    #[test]
    fn the_window_subtracts_watched_unit_kills() {
        let units = watched(RUNNER_SERVICE, Some(0));
        let killed = watched(RUNNER_SERVICE, Some(1));
        let r0 = read("2026-11-27T00:14:40Z", vec![slice("qontinui-runner.slice", RUNNER_SLICE, 4, &[])]);
        let (_, o0) = derive(None, &input(&units, Some(10), Some(&r0)), t("2026-11-27T00:14:40Z"));
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("qontinui-runner.slice", RUNNER_SLICE, 5, &[])]);
        // vmstat +2: one kill elsewhere, so the budget is not what stops it.
        assert_eq!(journal_window(Some(&o0), &input(&killed, Some(12), Some(&r1)), &r1), None);
        // The same tick with the unit's counter unchanged: the slice kill is
        // unexplained, and both reads are asked for.
        let w = journal_window(Some(&o0), &input(&units, Some(12), Some(&r1)), &r1).unwrap();
        assert!(w.user && w.kernel, "{w:?}");
    }

    // ---- round-3 review fixes -------------------------------------------

    /// Round-3 A1: a slice remainder the vmstat clamp carried is re-emitted
    /// next tick at an UNCHANGED counter; its id is keyed on the counter range,
    /// so it is not deduped away while already counted as attributed.
    #[test]
    fn a_carried_remainder_is_a_distinct_event_next_tick() {
        let o0 = baseline(&[]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 291, &[])]);
        let (ev1, o1) = derive(Some(&o0), &input(&[], Some(34080), Some(&r1)), t("2026-11-27T00:15:10Z"));
        let (ev2, _) = derive(Some(&o1), &input(&[], Some(34081), Some(&r1)), t("2026-11-27T00:15:40Z"));
        let rem = |ev: &[ComputerEvent]| {
            ev.iter()
                .find(|e| e.detail["attribution"] == "slice_memory_events")
                .map(|e| e.client_event_id.clone())
                .unwrap()
        };
        assert_ne!(rem(&ev1), rem(&ev2));
        assert_eq!(
            rem(&ev2),
            event_id(
                "oom_kill",
                &[
                    BOOT,
                    "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice",
                    "slice_memory_events",
                    "290..291"
                ]
            )
        );
    }

    /// Round-3 A2: a clamp cut is carried ONE tick. A watched unit with no
    /// known cgroup inside a slice is counted by U but never subtracted from
    /// the slice, so vmstat never covers that excess; carried forever it
    /// would be counted against a later kill elsewhere.
    #[test]
    fn a_clamp_cut_vmstat_never_covers_is_dropped_after_one_tick() {
        let no_cg = RUNNER_SERVICE.replace(
            "ControlGroup=/user.slice/user-1000.slice/user@1000.service/qontinui.slice/qontinui-runner.slice/qontinui-runner.service",
            "ControlGroup=",
        );
        let before = watched(&no_cg, Some(0));
        let after = watched(&no_cg, Some(1));
        let r0 = read("2026-11-27T00:14:40Z", vec![slice("qontinui-runner.slice", RUNNER_SLICE, 4, &[])]);
        let (_, o0) = derive(None, &input(&before, Some(10), Some(&r0)), t("2026-11-27T00:14:40Z"));
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("qontinui-runner.slice", RUNNER_SLICE, 5, &[])]);
        let (_, o1) = derive(Some(&o0), &input(&after, Some(11), Some(&r1)), t("2026-11-27T00:15:10Z"));
        assert_eq!(o1.slices["qontinui-runner.slice"].oom_kill, 4, "carried once");
        assert_eq!(o1.slices["qontinui-runner.slice"].carried, 1);
        let (_, o2) = derive(Some(&o1), &input(&after, Some(11), Some(&r1)), t("2026-11-27T00:15:40Z"));
        assert_eq!(o2.slices["qontinui-runner.slice"].oom_kill, 5, "not carried again");
        assert_eq!(o2.slices["qontinui-runner.slice"].carried, 0);
        // A later tick: one kill in the slice, one elsewhere.
        let r3 = read("2026-11-27T00:16:10Z", vec![slice("qontinui-runner.slice", RUNNER_SLICE, 6, &[])]);
        let (ev, _) = derive(Some(&o2), &input(&after, Some(13), Some(&r3)), t("2026-11-27T00:16:10Z"));
        let got: Vec<(String, u64)> = ooms(&ev)
            .iter()
            .map(|e| (e.detail["attribution"].as_str().unwrap().to_string(), e.detail["count"].as_u64().unwrap()))
            .collect();
        assert_eq!(
            got,
            vec![("slice_memory_events".to_string(), 1), ("vmstat_delta".to_string(), 1)],
            "{ev:?}"
        );
    }

    /// Round-3 A5: a kill a kernel record named is not named again next tick
    /// by systemd's later line for the same unit, as if it were another kill.
    #[test]
    fn a_unit_named_by_one_source_is_not_renamed_by_the_other() {
        let o0 = baseline(&[(SCOPE, Some(0))]);
        // The scope is still listed at the read, but its counter could not
        // be read (it was going away): B cannot name it; the kernel can.
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 289, &[(SCOPE, None)])]);
        let k = KernelRead::Read(vec![krec(
            &format!("{AGENT}/{SCOPE}"),
            "rustc",
            1795738509500000, // 00:15:09.5Z
            "27cd98773ff347f083889880a31d5902",
        )]);
        let (ev1, o1) = derive(Some(&o0), &with_kernel(input(&[], Some(34079), Some(&r1)), &k), t("2026-11-27T00:15:10Z"));
        assert_eq!(ev1.len(), 1, "{ev1:?}");
        assert_eq!(ev1[0].detail["attribution"], "kernel_oom_record");
        assert_eq!(o1.slices["agent-builds.slice"].named.get(SCOPE), Some(&1795738509500000));
        // Next tick another, unplaced kill; systemd's line for the FIRST kill
        // lands in this window (00:15:13.5Z, past the last lookahead).
        let r2 = read("2026-11-27T00:15:40Z", vec![slice("agent-builds.slice", AGENT, 290, &[])]);
        let j = journal(&[jline(SCOPE, 1795738513500000, BOOT)]);
        let (ev2, _) = derive(Some(&o1), &with_journal(input(&[], Some(34080), Some(&r2)), &j), t("2026-11-27T00:15:40Z"));
        assert_eq!(ev2.len(), 1, "{ev2:?}");
        assert_eq!(ev2[0].detail["attribution"], "slice_memory_events");
        assert_eq!(ev2[0].detail["victim_unit"], serde_json::Value::Null);
    }

    /// Round-3 A6: a slice baseline saved before `named` and `carried`
    /// existed loads with neither.
    #[test]
    fn an_old_slice_baseline_loads_without_the_new_fields() {
        let old: SliceObs = serde_json::from_str(
            r#"{"cgroup":"/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice","oom_kill":289,"observed_at":"2026-11-27T00:14:40Z","children":{"agent-build-a.scope":0},"named_ahead":["x@1"]}"#,
        )
        .unwrap();
        assert_eq!(old.oom_kill, 289);
        assert!(old.named.is_empty());
        assert_eq!(old.carried, 0);
        let bare: SliceObs =
            serde_json::from_str(r#"{"cgroup":"/a.slice","oom_kill":1,"observed_at":"2026-11-27T00:14:40Z"}"#).unwrap();
        assert!(bare.children.is_empty() && bare.named.is_empty());
    }

    // ---- round-4 review fixes -------------------------------------------

    /// Round-4 A1 / round-5 A3: the wait is measured on the monotonic clock,
    /// never longer than the lookahead, and nothing once it has passed.
    #[test]
    fn the_lookahead_wait_is_bounded_both_ways() {
        let read = std::time::Instant::now();
        let three = Duration::from_secs(3);
        assert_eq!(lookahead_wait(read, read, three), three);
        assert_eq!(lookahead_wait(read, read + Duration::from_secs(1), three), Duration::from_secs(2));
        // The read is long past: no wait.
        assert_eq!(lookahead_wait(read, read + Duration::from_secs(50), three), Duration::ZERO);
        // A read that looks later than now: still at most the lookahead.
        assert_eq!(lookahead_wait(read + Duration::from_secs(3600), read, three), three);
        assert_eq!(lookahead_wait(read, read, Duration::ZERO), Duration::ZERO);
    }

    /// Round-4 A3: back-to-back lagging kills. A fresh lag cut on the tick
    /// after a carry is carried too, so both kills keep their slice; only a
    /// re-cut of the carried excess would be dropped.
    #[test]
    fn back_to_back_lagging_kills_are_both_counted_in_the_slice() {
        let o0 = baseline(&[]);
        // Tick 1: a kill in the slice, vmstat has not counted it yet.
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 289, &[])]);
        let (ev1, o1) = derive(Some(&o0), &input(&[], Some(34078), Some(&r1)), t("2026-11-27T00:15:10Z"));
        assert!(ooms(&ev1).is_empty(), "{ev1:?}");
        assert_eq!(o1.slices["agent-builds.slice"].carried, 1);
        // Tick 2: vmstat counts kill 1; kill 2 lands in the slice, again
        // ahead of vmstat.
        let r2 = read("2026-11-27T00:15:40Z", vec![slice("agent-builds.slice", AGENT, 290, &[])]);
        let (ev2, o2) = derive(Some(&o1), &input(&[], Some(34079), Some(&r2)), t("2026-11-27T00:15:40Z"));
        assert_eq!(ooms(&ev2).len(), 1, "{ev2:?}");
        assert_eq!(ooms(&ev2)[0].detail["attribution"], "slice_memory_events");
        assert_eq!(o2.slices["agent-builds.slice"].oom_kill, 289, "the fresh cut is carried");
        // Tick 3: vmstat counts kill 2 — inside the slice, not the residual.
        let (ev3, _) = derive(Some(&o2), &input(&[], Some(34080), Some(&r2)), t("2026-11-27T00:16:10Z"));
        let kinds: Vec<&str> = ooms(&ev3).iter().map(|e| e.detail["attribution"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["slice_memory_events"], "{ev3:?}");
    }

    /// Round-4 A4: a unit Layer B placed is recorded as named, so systemd's
    /// late line for that kill does not name a different kill next tick.
    #[test]
    fn a_b_placed_unit_is_not_renamed_by_its_late_journal_line() {
        let o0 = baseline(&[(SCOPE, Some(0))]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 289, &[(SCOPE, Some(1))])]);
        let (ev1, o1) = derive(Some(&o0), &input(&[], Some(34079), Some(&r1)), t("2026-11-27T00:15:10Z"));
        assert_eq!(ev1[0].detail["attribution"], "cgroup_memory_events");
        assert_eq!(o1.slices["agent-builds.slice"].named.get(SCOPE), Some(&1795738510000000));
        // The scope is gone; another, unplaced kill; systemd's line for the
        // first kill lands now (00:15:13.5Z).
        let r2 = read("2026-11-27T00:15:40Z", vec![slice("agent-builds.slice", AGENT, 290, &[])]);
        let j = journal(&[jline(SCOPE, 1795738513500000, BOOT)]);
        let (ev2, _) = derive(Some(&o1), &with_journal(input(&[], Some(34080), Some(&r2)), &j), t("2026-11-27T00:15:40Z"));
        assert_eq!(ev2.len(), 1, "{ev2:?}");
        assert_eq!(ev2[0].detail["attribution"], "slice_memory_events");
        assert_eq!(ev2[0].detail["victim_unit"], serde_json::Value::Null);
    }

    // ---- round-5 review fixes -------------------------------------------

    /// Round-5 A1: a carried excess vmstat never covers is dropped after one
    /// tick even when a watched unit in the same slice dies on that tick —
    /// the unit's own kill is not one of the slice's new kills.
    #[test]
    fn a_watched_unit_kill_does_not_keep_a_dead_carry_alive() {
        let units = watched(RUNNER_SERVICE, Some(0));
        let killed = watched(RUNNER_SERVICE, Some(1));
        let r0 = read("2026-11-27T00:14:40Z", vec![slice("qontinui-runner.slice", RUNNER_SLICE, 4, &[])]);
        let (_, o0) = derive(None, &input(&units, Some(10), Some(&r0)), t("2026-11-27T00:14:40Z"));
        // Tick 1: a slice kill vmstat does not count — carried once.
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("qontinui-runner.slice", RUNNER_SLICE, 5, &[])]);
        let (_, o1) = derive(Some(&o0), &input(&units, Some(10), Some(&r1)), t("2026-11-27T00:15:10Z"));
        assert_eq!(o1.slices["qontinui-runner.slice"].carried, 1);
        // Tick 2: the runner service dies in the slice (vmstat +1 for it);
        // the carried excess is still uncovered.
        let r2 = read("2026-11-27T00:15:40Z", vec![slice("qontinui-runner.slice", RUNNER_SLICE, 6, &[])]);
        let (ev2, o2) = derive(Some(&o1), &input(&killed, Some(11), Some(&r2)), t("2026-11-27T00:15:40Z"));
        let o = ooms(&ev2);
        assert_eq!(o.len(), 1, "{ev2:?}");
        assert_eq!(o[0].detail["victim_unit"], "qontinui-runner.service");
        assert_eq!(o2.slices["qontinui-runner.slice"].carried, 0, "dropped, not carried again");
        assert_eq!(o2.slices["qontinui-runner.slice"].oom_kill, 6);
    }

    /// Round-5 A2: a carry does not cross a reboot. The new boot's lag cut
    /// is carried as a first carry, not dropped as a re-cut of the old one.
    #[test]
    fn a_reboot_resets_the_carry() {
        let o0 = baseline(&[]);
        let r1 = read("2026-11-27T00:15:10Z", vec![slice("agent-builds.slice", AGENT, 289, &[])]);
        let (_, o1) = derive(Some(&o0), &input(&[], Some(34078), Some(&r1)), t("2026-11-27T00:15:10Z"));
        assert_eq!(o1.slices["agent-builds.slice"].carried, 1);
        let new_boot = "11111111-2222-4333-8444-555555555555";
        // One kill since boot in the slice, not yet in vmstat.
        let r2 = read("2026-11-27T03:01:00Z", vec![slice("agent-builds.slice", AGENT, 1, &[])]);
        let inp = ObsInput {
            boot_id: Some(new_boot),
            booted_at: Some("2026-11-27T03:00:00Z"),
            ..input(&[], Some(0), Some(&r2))
        };
        let (_, o2) = derive(Some(&o1), &inp, t("2026-11-27T03:01:00Z"));
        assert_eq!(o2.slices["agent-builds.slice"].carried, 1, "carried in the new boot");
        assert_eq!(o2.slices["agent-builds.slice"].oom_kill, 0);
    }
}
