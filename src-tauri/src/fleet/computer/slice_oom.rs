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
//! * Whatever the slice delta has left after the watched units and B is ONE
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
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::events::{event_id, ComputerEvent, ComputerObs, ObsInput};
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

/// Read every watched slice's `oom_kill` counter and its kept children's
/// under `root` (a `/sys/fs/cgroup`-shaped tree). `tracked` is each slice's
/// child set from the previous tick, `watched` the watched unit names (never
/// listed as children). Plain `std::fs`, so the fixture tests run anywhere.
pub(crate) fn read_from(
    root: &Path,
    tracked: &BTreeMap<String, BTreeSet<String>>,
    watched: &BTreeSet<String>,
    read_at: DateTime<Utc>,
) -> SliceOomRead {
    if !root.join("cgroup.controllers").is_file() {
        return SliceOomRead {
            source: SliceSource::NotSupported,
            read_at,
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
        slices,
    }
}

/// The live read of `/sys/fs/cgroup`. Blocking file IO: run it in a tracked
/// blocking body.
#[cfg(target_os = "linux")]
fn read_live(
    tracked: &BTreeMap<String, BTreeSet<String>>,
    watched: &BTreeSet<String>,
) -> SliceOomRead {
    read_from(Path::new("/sys/fs/cgroup"), tracked, watched, Utc::now())
}

/// This tick's OOM-placement reads for the host, against `prev` (its last
/// observation) and `input` (this tick's, without any reads yet). Linux only;
/// elsewhere nothing is read. Best-effort: a failed read is `unreadable`,
/// never a stalled tick.
pub(crate) async fn collect(prev: Option<&ComputerObs>, input: &ObsInput<'_>) -> OomReads {
    #[cfg(target_os = "linux")]
    {
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
        let slices = spawn_blocking_tracked(move || read_live(&tracked, &watched))
            .await
            .unwrap_or_else(|_| SliceOomRead {
                source: SliceSource::Unreadable,
                read_at: Utc::now(),
                slices: Vec::new(),
            });
        OomReads {
            slices: Some(slices),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (prev, input);
        OomReads::default()
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
    /// The child's counter now — the event id's key.
    counter: u64,
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
    /// The next tick's baseline.
    next: SliceObs,
}

/// Plan one slice. `prev` is its persisted baseline. PURE.
fn plan_slice<'a>(
    prev: Option<&SliceObs>,
    s: &'a SliceRead,
    kills: &[UnitKill],
    rebooted: bool,
    read_at: DateTime<Utc>,
) -> SlicePlan<'a> {
    // (baseline, the children's baselines — none after a reboot).
    let consume: Option<(u64, Option<&BTreeMap<String, u64>>)> = match prev {
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
    let next = SliceObs {
        cgroup: s.cgroup.clone(),
        oom_kill: s.oom_kill,
        observed_at: read_at,
        children: next_children,
    };
    let Some((base, children_base)) = consume else {
        return SlicePlan {
            read: s,
            base: None,
            left: 0,
            children: Vec::new(),
            clamped: false,
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
                    counter: now,
                });
            }
        }
    }
    // The children were read after the slice: a kill between the two reads
    // is in a child's counter and not yet in the slice's. Trimmed here, it is
    // the next tick's slice delta.
    let excess = children
        .iter()
        .map(|c| c.count)
        .sum::<u64>()
        .saturating_sub(left);
    let clamped = trim_children(&mut children, excess);
    SlicePlan {
        read: s,
        base: Some(base),
        left,
        children,
        clamped,
        next,
    }
}

/// Remove `excess` from the children's counts, the last child first; `true`
/// when anything was removed. PURE.
fn trim_children(children: &mut Vec<ChildKill>, mut excess: u64) -> bool {
    let fired = excess > 0;
    while excess > 0 {
        let Some(last) = children.last_mut() else {
            break;
        };
        let cut = last.count.min(excess);
        last.count -= cut;
        excess -= cut;
        if last.count == 0 {
            children.pop();
        }
    }
    fired
}

/// Everything the slice stage of `events::derive` needs.
pub(crate) struct SliceInput<'a> {
    pub(crate) prev: &'a BTreeMap<String, SliceObs>,
    pub(crate) read: Option<&'a SliceOomRead>,
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

/// The slice stage of `events::derive`: this tick's slice events (Layers A
/// and B), the kills they carry and the next baselines. PURE.
pub(crate) fn attribute(a: &SliceInput<'_>) -> SliceOutcome {
    let status = a
        .read
        .map_or(SLICES_NOT_SUPPORTED, |r| r.source.as_str());
    let read = a.read.filter(|r| r.source == SliceSource::Read);
    let cgroups: Vec<(String, String)> = match read {
        Some(r) => r
            .slices
            .iter()
            .map(|s| (s.name.clone(), s.cgroup.clone()))
            .collect(),
        None => a
            .prev
            .iter()
            .map(|(n, s)| (n.clone(), s.cgroup.clone()))
            .collect(),
    };
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
    let plans: Vec<SlicePlan<'_>> = read
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

    // Per slice: B, then the remainder.
    let mut drafts: Vec<Draft> = plans
        .iter()
        .map(|p| {
            let named: u64 = p.children.iter().map(|c| c.count).sum();
            Draft {
                children: p.children.clone(),
                remainder: p.left.saturating_sub(named),
                clamped: p.clamped,
            }
        })
        .collect();
    // Read skew against vmstat: never more than `V - U` in all.
    let total: u64 = drafts.iter().map(Draft::total).sum();
    let tick_clamped = total > budget;
    if tick_clamped {
        let mut excess = total - budget;
        for d in drafts.iter_mut() {
            let cut = d.remainder.min(excess);
            d.remainder -= cut;
            excess -= cut;
        }
        for d in drafts.iter_mut() {
            let before: u64 = d.children.iter().map(|c| c.count).sum();
            trim_children(&mut d.children, excess.min(before));
            excess -= excess.min(before);
        }
    }

    let mut events = Vec::new();
    for (p, d) in plans.iter().zip(&drafts) {
        let s = p.read;
        let clamped = d.clamped || tick_clamped;
        let detail = |unit: Option<&str>, cgroup: &str, count: u64, how: &str| {
            json!({
                "victim_unit": unit,
                "victim_slice": s.name,
                "victim_cgroup": cgroup,
                "count": count,
                "attribution": how,
                "slices": status,
                "children_truncated": s.children_truncated,
                "count_clamped": clamped,
                "oom_kill_total": a.oom_kill_total,
                "slice_oom_kill_total": s.oom_kill,
                "previous_slice_oom_kill_total": p.base,
            })
        };
        for c in &d.children {
            let how = "cgroup_memory_events";
            events.push(ComputerEvent {
                client_event_id: event_id(
                    "oom_kill",
                    &[a.boot, &c.child, how, &c.counter.to_string()],
                ),
                kind: "oom_kill".into(),
                observed_at: a.now_s.to_string(),
                detail: detail(
                    Some(&c.child),
                    &format!("{}/{}", s.cgroup, c.child),
                    c.count,
                    how,
                ),
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
                    &[
                        a.boot,
                        &s.cgroup,
                        "slice_memory_events",
                        &s.oom_kill.to_string(),
                    ],
                ),
                kind: "oom_kill".into(),
                observed_at: a.now_s.to_string(),
                detail: detail(None, &s.cgroup, d.remainder, how),
            });
        }
    }
    let attributed = drafts.iter().map(Draft::total).sum();
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

/// One slice's events before they are built.
struct Draft {
    children: Vec<ChildKill>,
    remainder: u64,
    clamped: bool,
}

impl Draft {
    fn total(&self) -> u64 {
        self.children.iter().map(|c| c.count).sum::<u64>() + self.remainder
    }
}

#[cfg(test)]
mod tests {
    use super::super::events::{self, derive, ComputerEvent, ComputerObs, ObsInput};
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
        assert_eq!(d["children_truncated"], false);
        assert_eq!(d["count_clamped"], false);
        assert_eq!(d["slice_oom_kill_total"], 289);
        assert_eq!(d["previous_slice_oom_kill_total"], 288);
        assert_eq!(o[0].observed_at, "2026-11-27T00:15:00Z");
        assert_eq!(
            o[0].client_event_id,
            event_id(
                "oom_kill",
                &[BOOT, "agent-build-a.scope", "cgroup_memory_events", "1"]
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
                &[BOOT, "/ci.slice/ci-runners.slice", "slice_memory_events", "5"]
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
                    "2"
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
        let r0 = read_from(d.path(), &BTreeMap::new(), &BTreeSet::new(), t("2026-11-27T00:14:30Z"));
        agent_tree(d.path(), 290, 65);
        let r1 = read_from(d.path(), &BTreeMap::new(), &BTreeSet::new(), t("2026-11-27T00:15:00Z"));
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
        let r = read_from(d.path(), &BTreeMap::new(), &BTreeSet::new(), t("2026-11-27T00:15:00Z"));
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
        let r = read_from(root, &BTreeMap::new(), &watched_names, t("2026-11-27T00:15:00Z"));
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
}
