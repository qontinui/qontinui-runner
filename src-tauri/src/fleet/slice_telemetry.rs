//! Per-slice cgroup v2 memory, CPU and PSI telemetry for the 30 s resource
//! sample (plan `2026-10-19-ci-nodes-run-the-tauri-runner-always-on-as-ci-node-role`,
//! Phase 0b; the CPU half is plan
//! `2026-11-05-one-free-resource-model-places-sessions-builds-and-ci`, Phase 2).
//!
//! # Why this exists
//!
//! The msi incident was a *slice* starving: `ci-runners.slice` sat at its
//! `memory.high` while every host gauge read healthy. The sample carried host
//! load/CPU/memory/swap and only the pids cgroup controller, so nothing showed
//! it. This module adds, for each watched slice ([`WATCHED_SLICES`]):
//!
//! * `memory.current`, `memory.high`, `memory.max`;
//! * the `memory.events` `high` / `max` / `oom_kill` counters;
//! * PSI `memory` / `cpu` / `io` `some` / `full` `avg10` / `avg60` from the
//!   slice's own `*.pressure` files, and the same from `/proc/pressure/*` for
//!   the whole system;
//! * the CPU the slice used since the previous sample, from `cpu.stat`, and
//!   the host's CPU split into session work, CI and the unattributed rest
//!   (`cpu`, which documents what each class is made of).
//!
//! # Not supported is not zero
//!
//! Off Linux, or on a Linux host without the unified cgroup v2 hierarchy, the
//! publisher says [`SOURCE_NOT_SUPPORTED`] and publishes NO slice or PSI
//! values. A `0` would read as "this slice is idle", which is the wrong answer
//! on exactly the hosts that cannot be measured (`machine-resources.md` §7).
//! The same discipline holds inside a Linux read: a file that is missing or
//! does not parse leaves its key ABSENT from the JSON, and cgroup v2's literal
//! `max` (no limit) is kept as the string `"max"` rather than being turned
//! into a number.
//!
//! # Shape on the wire
//!
//! The memory keys mirror the `slice_tel_01` migration's column comments in
//! qontinui-web, which describe `cgroup_slices` as keyed by slice. The CPU
//! keys extend that contract: a `cpu` object inside each watched slice's
//! entry, and `cpu_attribution`, the one top-level key that is not a slice:
//!
//! ```text
//! cgroup_slices = {"ci-runners.slice": {"memory_current": N,
//!                  "memory_high": N | "max", "memory_max": N | "max",
//!                  "events": {"high": N, "max": N, "oom_kill": N},
//!                  "cpu": {"usage_delta_usec": N, "interval_usec": N}},
//!                  "cpu_attribution": {"session_usec": N, "ci_usec": N, ...}}
//! psi           = {"system": {"memory": {"some": {"avg10": f, "avg60": f},
//!                                        "full": {...}}, "cpu": ..., "io": ...},
//!                  "slices": {"ci-runners.slice": {"memory": ..., ...}}}
//! ```
//!
//! The CPU keys are deltas over the interval since the previous read of the
//! same root, so the first sample after a start carries none; the full
//! `cpu_attribution` key set is in the `cpu` module.
//!
//! # Testability
//!
//! The parsers and [`read_slices`] / [`read_psi`] are plain `std::fs` and take
//! their roots as arguments, so the fixture-directory tests run on every host.
//! Only [`collect`] picks the real `/sys/fs/cgroup` and `/proc/pressure` roots,
//! and only under `cfg(target_os = "linux")`.

// The readers are exercised by tests on every OS but called in production only
// on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Map, Value};

mod cpu;

/// The slices watched, in publish order. A slice that does not exist on the
/// machine is absent from the published map.
pub(crate) const WATCHED_SLICES: [&str; 3] = [
    "ci-runners.slice",
    "agent-builds.slice",
    "qontinui-runner.slice",
];

/// `slice_source` when the cgroup v2 tree was read.
pub(crate) const SOURCE_LINUX_CGROUP_V2: &str = "linux_cgroup_v2";
/// `slice_source` when this host cannot be measured (non-Linux, or no unified
/// cgroup hierarchy). The explicit form of "unmeasured".
pub(crate) const SOURCE_NOT_SUPPORTED: &str = "not_supported";

/// The three telemetry fields a sample carries.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SliceTelemetry {
    pub(crate) cgroup_slices: Option<Value>,
    pub(crate) psi: Option<Value>,
    pub(crate) slice_source: Option<String>,
}

impl SliceTelemetry {
    fn not_supported() -> Self {
        Self {
            cgroup_slices: None,
            psi: None,
            slice_source: Some(SOURCE_NOT_SUPPORTED.to_string()),
        }
    }
}

/// A cgroup v2 limit: a byte count, or the literal `max` (unbounded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CgLimit {
    Bytes(u64),
    Unlimited,
}

impl Serialize for CgLimit {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CgLimit::Bytes(n) => s.serialize_u64(*n),
            CgLimit::Unlimited => s.serialize_str("max"),
        }
    }
}

/// `memory.high` / `memory.max`: a decimal byte count or `max`. Anything else
/// (empty, negative, garbage) is `None`, never a default.
pub(crate) fn parse_limit(text: &str) -> Option<CgLimit> {
    let t = text.trim();
    if t == "max" {
        return Some(CgLimit::Unlimited);
    }
    t.parse::<u64>().ok().map(CgLimit::Bytes)
}

/// `memory.current`: a decimal byte count. `max` is not a valid reading here.
pub(crate) fn parse_bytes(text: &str) -> Option<u64> {
    text.trim().parse::<u64>().ok()
}

/// The `memory.events` counters the alert reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct MemoryEvents {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) high: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) oom_kill: Option<u64>,
}

/// `memory.events` is `key value` lines (`low`, `high`, `max`, `oom`,
/// `oom_kill`, ...). Returns `None` when none of the three wanted keys parse,
/// so an unreadable file never publishes an all-zero counter set.
pub(crate) fn parse_memory_events(text: &str) -> Option<MemoryEvents> {
    let mut ev = MemoryEvents::default();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(key), Some(val)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(n) = val.parse::<u64>() else {
            continue;
        };
        match key {
            "high" => ev.high = Some(n),
            "max" => ev.max = Some(n),
            "oom_kill" => ev.oom_kill = Some(n),
            _ => {}
        }
    }
    (ev != MemoryEvents::default()).then_some(ev)
}

/// One PSI line's averages, in percent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct PsiAvg {
    pub(crate) avg10: f64,
    pub(crate) avg60: f64,
}

/// One resource's PSI: the `some` and `full` lines. `full` is absent where the
/// kernel does not define it (system-scope `cpu` on older kernels).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub(crate) struct PsiResource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) some: Option<PsiAvg>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) full: Option<PsiAvg>,
}

fn parse_psi_line(rest: &str) -> Option<PsiAvg> {
    let (mut a10, mut a60) = (None, None);
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix("avg10=") {
            a10 = v.parse::<f64>().ok();
        } else if let Some(v) = tok.strip_prefix("avg60=") {
            a60 = v.parse::<f64>().ok();
        }
    }
    let (avg10, avg60) = (a10?, a60?);
    (avg10.is_finite() && avg60.is_finite()).then_some(PsiAvg { avg10, avg60 })
}

/// Parse a `/proc/pressure/*` or `*.pressure` file:
/// `some avg10=0.00 avg60=0.00 avg300=0.00 total=0`. `None` when neither line
/// parses (a PSI-disabled kernel returns an empty or unreadable file).
pub(crate) fn parse_pressure(text: &str) -> Option<PsiResource> {
    let mut res = PsiResource::default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("some ") {
            res.some = parse_psi_line(rest);
        } else if let Some(rest) = line.strip_prefix("full ") {
            res.full = parse_psi_line(rest);
        }
    }
    (res.some.is_some() || res.full.is_some()).then_some(res)
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// One slice directory's memory reading, or `None` when no memory file in it
/// was readable (the controller is not enabled, or the slice is not there).
fn read_slice_memory(dir: &Path) -> Option<Value> {
    let mut m = Map::new();
    if let Some(v) = read(&dir.join("memory.current")).and_then(|t| parse_bytes(&t)) {
        m.insert("memory_current".into(), json!(v));
    }
    if let Some(v) = read(&dir.join("memory.high")).and_then(|t| parse_limit(&t)) {
        m.insert("memory_high".into(), json!(v));
    }
    if let Some(v) = read(&dir.join("memory.max")).and_then(|t| parse_limit(&t)) {
        m.insert("memory_max".into(), json!(v));
    }
    if let Some(ev) = read(&dir.join("memory.events")).and_then(|t| parse_memory_events(&t)) {
        m.insert("events".into(), json!(ev));
    }
    (!m.is_empty()).then_some(Value::Object(m))
}

/// Memory, cpu and io PSI from a directory holding `memory.pressure`,
/// `cpu.pressure` and `io.pressure` (a cgroup dir).
fn read_slice_psi(dir: &Path) -> Option<Value> {
    let mut m = Map::new();
    for (key, file) in [
        ("memory", "memory.pressure"),
        ("cpu", "cpu.pressure"),
        ("io", "io.pressure"),
    ] {
        if let Some(p) = read(&dir.join(file)).and_then(|t| parse_pressure(&t)) {
            m.insert(key.into(), json!(p));
        }
    }
    (!m.is_empty()).then_some(Value::Object(m))
}

/// systemd nests `a-b.slice` under `a.slice` unless the unit overrides its
/// parent with `Slice=`, so `ci-runners.slice` lives at
/// `ci.slice/ci-runners.slice`. `None` for a slice name with no dash.
fn nested_parent(slice: &str) -> Option<String> {
    let stem = slice.strip_suffix(".slice")?;
    let (head, _) = stem.split_once('-')?;
    Some(format!("{head}.slice"))
}

/// Candidate directories for a slice, in resolution order. Under each base --
/// the cgroup root (system units), `system.slice/`, then every user manager
/// (`user.slice/user-N.slice/user@N.service/`, where the runner's own slice
/// lives on a systemd user service) -- the dash-nested path
/// (`ci.slice/ci-runners.slice`) is tried before the flat one.
pub(crate) fn slice_dirs(cgroup_root: &Path, slice: &str) -> Vec<PathBuf> {
    slice_dirs_in(&slice_bases(cgroup_root).bases, slice)
}

/// The bases [`slice_dirs`] resolves a slice under ([`slice_bases`]).
pub(crate) struct SliceBases {
    /// In resolution order.
    pub(crate) bases: Vec<PathBuf>,
    /// `user.slice`, and each `user-N.slice` in it, that exists but could not
    /// be listed in full: a user manager in it may be missing from `bases`.
    pub(crate) unlisted: Vec<PathBuf>,
}

/// The bases a slice is looked for under, in [`slice_dirs`]' order, and the
/// user directories that could not be listed for them.
pub(crate) fn slice_bases(cgroup_root: &Path) -> SliceBases {
    let mut bases = vec![cgroup_root.to_path_buf(), cgroup_root.join("system.slice")];
    let mut unlisted = Vec::new();
    let mut users = list_dir(&cgroup_root.join("user.slice"), &mut unlisted, |p| {
        p.is_dir()
    });
    users.sort();
    for u in users {
        let mut managers = list_dir(&u, &mut unlisted, |p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("user@") && n.ends_with(".service"))
        });
        managers.sort();
        bases.extend(managers);
    }
    SliceBases { bases, unlisted }
}

/// The entries of `dir` that `keep` accepts, as many as could be read. `dir`
/// goes on `unlisted` when it exists but could not be listed, in whole or in
/// part; one that does not exist has no entries.
fn list_dir(dir: &Path, unlisted: &mut Vec<PathBuf>, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                unlisted.push(dir.to_path_buf());
            }
            return Vec::new();
        }
    };
    let (mut out, mut whole) = (Vec::new(), true);
    for entry in rd {
        match entry {
            Ok(e) => {
                let p = e.path();
                if keep(&p) {
                    out.push(p);
                }
            }
            Err(_) => whole = false,
        }
    }
    if !whole {
        unlisted.push(dir.to_path_buf());
    }
    out
}

/// `slice`'s candidate directories under each of `bases` in turn, the
/// dash-nested path ([`nested_parent`]) before the flat one.
pub(crate) fn slice_dirs_in(bases: &[PathBuf], slice: &str) -> Vec<PathBuf> {
    let parent = nested_parent(slice);
    let mut out = Vec::new();
    for base in bases {
        if let Some(p) = &parent {
            out.push(base.join(p).join(slice));
        }
        out.push(base.join(slice));
    }
    out
}

/// The first candidate directory that exists and yields a memory reading.
fn resolve_slice(cgroup_root: &Path, slice: &str) -> Option<(PathBuf, Value)> {
    slice_dirs(cgroup_root, slice)
        .into_iter()
        .find_map(|d| read_slice_memory(&d).map(|v| (d, v)))
}

/// What [`read_slices`] read.
pub(crate) struct SliceReads {
    /// The `cgroup_slices` map. A slice with no readable file is omitted.
    pub(crate) memory: Map<String, Value>,
    /// The per-slice PSI map.
    pub(crate) psi: Map<String, Value>,
    /// The directory each slice in `memory` was read from, in
    /// [`WATCHED_SLICES`] order, so that its `cpu` object is read from the
    /// same cgroup ([`cpu::attach`]).
    pub(crate) dirs: Vec<(&'static str, PathBuf)>,
}

/// Read every watched slice under `cgroup_root`.
pub(crate) fn read_slices(cgroup_root: &Path) -> SliceReads {
    let mut reads = SliceReads {
        memory: Map::new(),
        psi: Map::new(),
        dirs: Vec::new(),
    };
    for slice in WATCHED_SLICES {
        let Some((dir, memory)) = resolve_slice(cgroup_root, slice) else {
            continue;
        };
        reads.memory.insert(slice.to_string(), memory);
        if let Some(p) = read_slice_psi(&dir) {
            reads.psi.insert(slice.to_string(), p);
        }
        reads.dirs.push((slice, dir));
    }
    reads
}

/// System-wide PSI from a `/proc/pressure`-shaped directory.
pub(crate) fn read_psi(proc_pressure: &Path) -> Map<String, Value> {
    let mut m = Map::new();
    for key in ["memory", "cpu", "io"] {
        if let Some(p) = read(&proc_pressure.join(key)).and_then(|t| parse_pressure(&t)) {
            m.insert(key.to_string(), json!(p));
        }
    }
    m
}

/// Assemble the three fields from the two roots. `None` fields stay `None`
/// (unmeasured); a slices map with no watched slice in it is a real answer:
/// the unified hierarchy was read and none of the watched slices exists here.
/// From the second call on, such a map can still hold `cpu_attribution`.
///
/// The CPU keys of `cgroup_slices` are deltas since the previous call for the
/// same `cgroup_root`, which this keeps between calls, so a first call carries
/// none. Each watched slice's CPU is read from the directory [`read_slices`]
/// read its memory from. `proc_pressure` is `<procfs>/pressure`: the runner's
/// own cgroup and user are read from `<procfs>/self/cgroup` and
/// `<procfs>/self/status` beside it. What `ci_node` has counted of its
/// dispatches ([`crate::ci_node::cgroup::dispatch_counts`]) is read before the
/// cgroups are listed and again after their counters are read: the session
/// and CI split is withheld for a read across which a CI dispatch may have run
/// where no read listed it.
pub(crate) fn collect_from(cgroup_root: &Path, proc_pressure: &Path) -> SliceTelemetry {
    // The unified hierarchy announces itself with `cgroup.controllers` at its
    // root. Without it (cgroup v1 or hybrid) this host cannot be measured.
    if !cgroup_root.join("cgroup.controllers").is_file() {
        return SliceTelemetry::not_supported();
    }
    let SliceReads {
        memory: mut mem,
        psi: slice_psi,
        dirs,
    } = read_slices(cgroup_root);
    cpu::attach(
        cgroup_root,
        proc_pressure,
        &dirs,
        crate::ci_node::cgroup::dispatch_counts,
        &mut mem,
    );
    let system = read_psi(proc_pressure);

    let psi = if system.is_empty() && slice_psi.is_empty() {
        None
    } else {
        let mut p = Map::new();
        if !system.is_empty() {
            p.insert("system".into(), Value::Object(system));
        }
        if !slice_psi.is_empty() {
            p.insert("slices".into(), Value::Object(slice_psi));
        }
        Some(Value::Object(p))
    };
    SliceTelemetry {
        cgroup_slices: Some(Value::Object(mem)),
        psi,
        slice_source: Some(SOURCE_LINUX_CGROUP_V2.to_string()),
    }
}

/// The live reading. Linux reads the real roots; every other OS reports
/// `not_supported` with no values.
#[cfg(target_os = "linux")]
pub(crate) fn collect() -> SliceTelemetry {
    collect_from(Path::new("/sys/fs/cgroup"), Path::new("/proc/pressure"))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn collect() -> SliceTelemetry {
    SliceTelemetry::not_supported()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PSI_MEM: &str = "some avg10=1.50 avg60=2.25 avg300=0.10 total=123456\n\
                           full avg10=0.50 avg60=0.75 avg300=0.00 total=654\n";
    const PSI_CPU_SOME_ONLY: &str = "some avg10=3.00 avg60=4.00 avg300=5.00 total=9\n";
    const EVENTS: &str = "low 0\nhigh 17\nmax 3\noom 1\noom_kill 2\noom_group_kill 0\n";

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn limit_keeps_max_as_unlimited_and_rejects_garbage() {
        assert_eq!(parse_limit("max\n"), Some(CgLimit::Unlimited));
        assert_eq!(
            parse_limit("1073741824\n"),
            Some(CgLimit::Bytes(1073741824))
        );
        assert_eq!(parse_limit("0\n"), Some(CgLimit::Bytes(0)));
        assert_eq!(parse_limit(""), None);
        assert_eq!(parse_limit("-1"), None);
        assert_eq!(parse_limit("lots"), None);
        // `max` is no reading for memory.current.
        assert_eq!(parse_bytes("max"), None);
        assert_eq!(parse_bytes("42\n"), Some(42));
    }

    #[test]
    fn memory_events_parse_the_three_counters_and_ignore_the_rest() {
        let ev = parse_memory_events(EVENTS).unwrap();
        assert_eq!(
            ev,
            MemoryEvents {
                high: Some(17),
                max: Some(3),
                oom_kill: Some(2)
            }
        );
        // An all-zero file is a real reading and stays distinct from none.
        let zero = parse_memory_events("high 0\nmax 0\noom_kill 0\n").unwrap();
        assert_eq!(zero.high, Some(0));
        // Nothing parseable is None, never a zero struct.
        assert_eq!(parse_memory_events(""), None);
        assert_eq!(parse_memory_events("low 0\noom 1\n"), None);
        assert_eq!(parse_memory_events("high notanumber\n"), None);
    }

    #[test]
    fn pressure_parses_some_and_full_and_tolerates_a_missing_full() {
        let m = parse_pressure(PSI_MEM).unwrap();
        assert_eq!(
            m.some,
            Some(PsiAvg {
                avg10: 1.5,
                avg60: 2.25
            })
        );
        assert_eq!(
            m.full,
            Some(PsiAvg {
                avg10: 0.5,
                avg60: 0.75
            })
        );
        let c = parse_pressure(PSI_CPU_SOME_ONLY).unwrap();
        assert!(c.some.is_some());
        assert_eq!(c.full, None, "an undefined full line stays absent");
        assert_eq!(parse_pressure(""), None);
        assert_eq!(parse_pressure("some avg10=x avg60=1\n"), None);
        assert_eq!(
            parse_pressure("some avg10=1.0\n"),
            None,
            "half a line is no line"
        );
    }

    fn fixture_tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(r, "cgroup.controllers", "cpuset cpu io memory pids\n");
        // System-level slice with limits and a counter history.
        write(r, "ci.slice/ci-runners.slice/memory.current", "900\n");
        write(r, "ci.slice/ci-runners.slice/memory.high", "1000\n");
        write(r, "ci.slice/ci-runners.slice/memory.max", "max\n");
        write(r, "ci.slice/ci-runners.slice/memory.events", EVENTS);
        write(r, "ci.slice/ci-runners.slice/memory.pressure", PSI_MEM);
        write(
            r,
            "ci.slice/ci-runners.slice/cpu.pressure",
            PSI_CPU_SOME_ONLY,
        );
        // The runner's slice lives under a user manager.
        write(
            r,
            "user.slice/user-1000.slice/user@1000.service/qontinui.slice/qontinui-runner.slice/memory.current",
            "5\n",
        );
        // agent-builds.slice is absent on this machine.
        d
    }

    #[test]
    fn slices_resolve_across_system_and_user_manager_paths() {
        let d = fixture_tree();
        let SliceReads {
            memory: mem,
            psi,
            dirs,
        } = read_slices(d.path());
        assert_eq!(mem.len(), 2, "agent-builds.slice is absent and omitted");
        assert_eq!(
            dirs,
            vec![
                ("ci-runners.slice", d.path().join("ci.slice/ci-runners.slice")),
                (
                    "qontinui-runner.slice",
                    d.path().join(
                        "user.slice/user-1000.slice/user@1000.service/qontinui.slice/qontinui-runner.slice"
                    )
                ),
            ],
            "each slice's directory, for its cpu object"
        );
        let ci = &mem["ci-runners.slice"];
        assert_eq!(ci["memory_current"], json!(900));
        assert_eq!(ci["memory_high"], json!(1000));
        assert_eq!(
            ci["memory_max"],
            json!("max"),
            "no limit stays the string max"
        );
        assert_eq!(ci["events"], json!({"high": 17, "max": 3, "oom_kill": 2}));
        let rn = &mem["qontinui-runner.slice"];
        assert_eq!(rn["memory_current"], json!(5));
        assert!(
            rn.get("memory_high").is_none(),
            "unreadable stays absent, not 0"
        );
        assert!(rn.get("events").is_none());
        assert!(!mem.contains_key("agent-builds.slice"));
        // PSI only for the slice that has *.pressure files.
        assert_eq!(psi.len(), 1);
        assert_eq!(
            psi["ci-runners.slice"]["memory"]["some"]["avg10"],
            json!(1.5)
        );
        assert!(psi["ci-runners.slice"]["cpu"].get("full").is_none());
        assert!(psi["ci-runners.slice"].get("io").is_none());
    }

    #[test]
    fn a_flat_slice_directory_still_resolves() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "agent-builds.slice/memory.current",
            "7
",
        );
        let mem = read_slices(d.path()).memory;
        assert_eq!(mem["agent-builds.slice"]["memory_current"], json!(7));
        assert_eq!(
            nested_parent("ci-runners.slice").as_deref(),
            Some("ci.slice")
        );
        assert_eq!(nested_parent("plain.slice"), None);
    }

    #[test]
    fn collect_from_assembles_the_three_fields() {
        let d = fixture_tree();
        let p = tempfile::tempdir().unwrap();
        write(p.path(), "memory", PSI_MEM);
        write(p.path(), "cpu", PSI_CPU_SOME_ONLY);
        // io absent: PSI partially available.
        let t = collect_from(d.path(), p.path());
        assert_eq!(t.slice_source.as_deref(), Some(SOURCE_LINUX_CGROUP_V2));
        assert!(t.cgroup_slices.is_some());
        let psi = t.psi.unwrap();
        assert_eq!(psi["system"]["memory"]["full"]["avg60"], json!(0.75));
        assert!(psi["system"].get("io").is_none());
        assert_eq!(
            psi["slices"]["ci-runners.slice"]["memory"]["some"]["avg60"],
            json!(2.25)
        );
    }

    #[test]
    fn no_unified_hierarchy_is_not_supported_not_zero() {
        let d = tempfile::tempdir().unwrap(); // no cgroup.controllers
        let t = collect_from(d.path(), d.path());
        assert_eq!(t.slice_source.as_deref(), Some(SOURCE_NOT_SUPPORTED));
        assert_eq!(t.cgroup_slices, None);
        assert_eq!(t.psi, None);
    }

    #[test]
    fn an_empty_unified_tree_is_a_measured_empty_map_with_no_psi() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "cgroup.controllers", "memory\n");
        let t = collect_from(d.path(), &d.path().join("no-proc-pressure"));
        assert_eq!(t.cgroup_slices, Some(json!({})));
        assert_eq!(t.psi, None, "PSI disabled is unmeasured, not zeros");
        assert_eq!(t.slice_source.as_deref(), Some(SOURCE_LINUX_CGROUP_V2));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn off_linux_the_live_reading_is_not_supported() {
        let t = collect();
        assert_eq!(t, SliceTelemetry::not_supported());
    }

    /// The payload builder the 30 s publish reads (`publish_once` ->
    /// `resource_sample::collect` -> `collect_host_lane` ->
    /// `slice_telemetry::collect` -> `collect_from`) carries each watched
    /// slice's CPU, and the session / CI split, from its second read of a root.
    /// The first read has no previous one and carries no CPU at all.
    #[test]
    fn collect_from_carries_slice_cpu_deltas_from_its_second_read() {
        // `collect_from` reads `ci_node`'s dispatch counts and its
        // `QONTINUI_CI_CGROUP_ROOT`: every test that moves either holds this
        // lock, and an override left in the environment would name a
        // directory outside this fixture.
        let _g = crate::test_env::env_lock();
        let ci_root = crate::ci_node::cgroup::CGROUP_ROOT_ENV;
        let _restore = crate::test_env::EnvVarRestore::capture(&[ci_root]);
        std::env::remove_var(ci_root);
        let d = tempfile::tempdir().unwrap();
        let (root, procfs) = (d.path().join("cgroup"), d.path().join("proc"));
        let mgr = "user.slice/user-1000.slice/user@1000.service";
        let unit = format!("{mgr}/app.slice/qontinui-runner.service");
        let pane = format!("{mgr}/app.slice/qontinui-pty-holder-p1-16f2c-18e4ca879535bc14.scope");
        let builds = format!("{mgr}/agent.slice/agent-builds.slice");
        write(&root, "cgroup.controllers", "cpuset cpu io memory pids\n");
        write(&root, "ci.slice/ci-runners.slice/memory.current", "900\n");
        write(&root, &format!("{builds}/memory.current"), "800\n");
        write(&procfs, "self/cgroup", &format!("0::/{unit}\n"));
        let usage = |k: u64| {
            for (rel, usec) in [
                ("user.slice", 40_000_000),
                ("system.slice", 5_000_000),
                (unit.as_str(), 4_000_000),
                (pane.as_str(), 6_000_000),
                (builds.as_str(), 10_000_000),
                ("ci.slice", 30_000_000),
                ("ci.slice/ci-runners.slice", 20_000_000),
                ("ci.slice/ci-winvm.slice", 10_000_000),
            ] {
                write(
                    &root,
                    &format!("{rel}/cpu.stat"),
                    &format!("usage_usec {}\n", usec * k),
                );
            }
            write(
                &root,
                "cpu.stat",
                &format!("usage_usec {}\n", 100_000_000 * k),
            );
        };
        let pressure = procfs.join("pressure");

        usage(1);
        let first = collect_from(&root, &pressure).cgroup_slices.unwrap();
        assert_eq!(first["ci-runners.slice"]["memory_current"], json!(900));
        assert!(
            first["ci-runners.slice"].get("cpu").is_none(),
            "no previous read: no delta, not 0"
        );
        assert!(first.get("cpu_attribution").is_none());

        std::thread::sleep(std::time::Duration::from_millis(5));
        usage(2);
        let second = collect_from(&root, &pressure).cgroup_slices.unwrap();
        let runners = &second["ci-runners.slice"];
        assert_eq!(
            runners["memory_current"],
            json!(900),
            "memory keys unchanged"
        );
        assert_eq!(runners["cpu"]["usage_delta_usec"], json!(20_000_000));
        assert!(runners["cpu"]["interval_usec"].as_u64().unwrap() > 0);
        assert_eq!(
            second["agent-builds.slice"]["cpu"]["usage_delta_usec"],
            json!(10_000_000)
        );
        let split = &second["cpu_attribution"];
        assert_eq!(split["host_usec"], json!(100_000_000));
        assert_eq!(
            split["session_usec"],
            json!(20_000_000),
            "unit + pane + builds"
        );
        assert_eq!(
            split["ci_usec"],
            json!(30_000_000),
            "ci.slice, winvm included"
        );
        assert_eq!(split["user_slice_remainder_usec"], json!(20_000_000));
        assert_eq!(split["system_slice_remainder_usec"], json!(5_000_000));
        assert_eq!(split["unattributed_usec"], json!(50_000_000));
        assert_eq!(
            split["ci_children_usec"],
            json!({"ci-runners.slice": 20_000_000, "ci-winvm.slice": 10_000_000})
        );
        assert_eq!(split["pty_holder_scopes_seen"], json!(1));
    }

    /// `collect_from` hands `cpu::attach` the directory each watched slice's
    /// memory was read from, so the slice's `cpu` object is of that cgroup and
    /// not of a candidate tried before it that has no memory files: here
    /// `ci.slice/ci-runners.slice`, tried before the flat `ci-runners.slice`.
    #[test]
    fn collect_from_reads_each_slices_cpu_where_it_read_its_memory() {
        // As in the test above: `collect_from` reads `ci_node`'s counts and
        // `QONTINUI_CI_CGROUP_ROOT`.
        let _g = crate::test_env::env_lock();
        let ci_root = crate::ci_node::cgroup::CGROUP_ROOT_ENV;
        let _restore = crate::test_env::EnvVarRestore::capture(&[ci_root]);
        std::env::remove_var(ci_root);
        let d = tempfile::tempdir().unwrap();
        let (root, procfs) = (d.path().join("cgroup"), d.path().join("proc"));
        write(&root, "cgroup.controllers", "cpuset cpu io memory pids\n");
        write(&root, "ci-runners.slice/memory.current", "900\n");
        let usage = |k: u64| {
            for (rel, usec) in [
                ("ci.slice/ci-runners.slice", 7_000_000),
                ("ci-runners.slice", 11_000_000),
            ] {
                write(
                    &root,
                    &format!("{rel}/cpu.stat"),
                    &format!("usage_usec {}\n", usec * k),
                );
            }
            write(
                &root,
                "cpu.stat",
                &format!("usage_usec {}\n", 100_000_000 * k),
            );
        };
        let pressure = procfs.join("pressure");

        usage(1);
        collect_from(&root, &pressure);
        std::thread::sleep(std::time::Duration::from_millis(5));
        usage(2);
        let second = collect_from(&root, &pressure).cgroup_slices.unwrap();
        let runners = &second["ci-runners.slice"];
        assert_eq!(runners["memory_current"], json!(900));
        assert_eq!(
            runners["cpu"]["usage_delta_usec"],
            json!(11_000_000),
            "the cgroup the memory entry describes, not the nested candidate"
        );
    }

    /// The live reading takes `ci_node`'s own dispatch counts: a CI job it
    /// could not contain runs in the runner's own cgroup, beside session work,
    /// so the split is withheld from the first read after the job started
    /// through the first read after it ended, and published again from the
    /// next.
    #[test]
    fn collect_from_withholds_the_split_while_ci_node_runs_a_job_it_could_not_contain() {
        use crate::ci_node::cgroup::{Containment, CGROUP_ROOT_ENV};
        // Every test that makes an uncontained run, through a `Containment`
        // or a step it could not place, holds this lock, so only this one
        // moves the counts while it is held.
        let _g = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[CGROUP_ROOT_ENV]);
        std::env::remove_var(CGROUP_ROOT_ENV);
        let d = tempfile::tempdir().unwrap();
        let (root, procfs) = (d.path().join("cgroup"), d.path().join("proc"));
        let unit = "user.slice/user-1000.slice/user@1000.service/app.slice/qontinui-runner.service";
        write(&root, "cgroup.controllers", "cpu memory pids\n");
        write(&procfs, "self/cgroup", &format!("0::/{unit}\n"));
        let pressure = procfs.join("pressure");
        let unit_stat = format!("{unit}/cpu.stat");
        let mut reads: u64 = 0;
        let mut read = || {
            for (file, usec) in [
                ("cpu.stat", 50),
                ("user.slice/cpu.stat", 30),
                (unit_stat.as_str(), 10),
                ("ci.slice/cpu.stat", 20),
            ] {
                write(&root, file, &format!("usage_usec {}\n", usec * reads));
            }
            reads += 1;
            std::thread::sleep(std::time::Duration::from_millis(2));
            collect_from(&root, &pressure).cgroup_slices.unwrap()
        };
        let split = |slices: Value| {
            let a = &slices["cpu_attribution"];
            assert_eq!(a["host_usec"], json!(50), "a read that measured");
            (a.get("session_usec").cloned(), a.get("ci_usec").cloned())
        };
        let published = (Some(json!(10)), Some(json!(20)));

        assert!(read().get("cpu_attribution").is_none(), "a first read");
        assert_eq!(split(read()), published, "no dispatch");

        std::env::set_var(CGROUP_ROOT_ENV, d.path().join("no-cgroup-here"));
        let job = Containment::create("withheld", 1 << 20);
        std::env::remove_var(CGROUP_ROOT_ENV);
        assert!(matches!(job, Containment::Uncontained(_)));
        assert_eq!(
            split(read()),
            (None, None),
            "the first read after it started"
        );
        assert_eq!(split(read()), (None, None), "a read while it runs");
        drop(job);
        assert_eq!(split(read()), (None, None), "the read after it ended");
        assert_eq!(split(read()), published, "the job is over");
    }
}
