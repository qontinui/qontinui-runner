//! Per-slice cgroup v2 memory and PSI telemetry for the 30 s resource sample
//! (plan `2026-10-19-ci-nodes-run-the-tauri-runner-always-on-as-ci-node-role`,
//! Phase 0b).
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
//!   the whole system.
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
//! Mirrors the `slice_tel_01` migration's column comments in qontinui-web:
//!
//! ```text
//! cgroup_slices = {"ci-runners.slice": {"memory_current": N,
//!                  "memory_high": N | "max", "memory_max": N | "max",
//!                  "events": {"high": N, "max": N, "oom_kill": N}}}
//! psi           = {"system": {"memory": {"some": {"avg10": f, "avg60": f},
//!                                        "full": {...}}, "cpu": ..., "io": ...},
//!                  "slices": {"ci-runners.slice": {"memory": ..., ...}}}
//! ```
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
    let mut bases = vec![cgroup_root.to_path_buf(), cgroup_root.join("system.slice")];
    if let Ok(rd) = fs::read_dir(cgroup_root.join("user.slice")) {
        let mut users: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        users.sort();
        for u in users {
            if let Ok(inner) = fs::read_dir(&u) {
                let mut managers: Vec<PathBuf> = inner
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("user@") && n.ends_with(".service"))
                    })
                    .collect();
                managers.sort();
                bases.extend(managers);
            }
        }
    }
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

/// Read every watched slice under `cgroup_root`. Returns the `cgroup_slices`
/// map and the per-slice PSI map. A slice with no readable file is omitted.
pub(crate) fn read_slices(cgroup_root: &Path) -> (Map<String, Value>, Map<String, Value>) {
    let (mut mem, mut psi) = (Map::new(), Map::new());
    for slice in WATCHED_SLICES {
        let Some((dir, memory)) = resolve_slice(cgroup_root, slice) else {
            continue;
        };
        mem.insert(slice.to_string(), memory);
        if let Some(p) = read_slice_psi(&dir) {
            psi.insert(slice.to_string(), p);
        }
    }
    (mem, psi)
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
/// (unmeasured); an EMPTY slices map is a real answer: the unified hierarchy
/// was read and none of the watched slices exists here.
pub(crate) fn collect_from(cgroup_root: &Path, proc_pressure: &Path) -> SliceTelemetry {
    // The unified hierarchy announces itself with `cgroup.controllers` at its
    // root. Without it (cgroup v1 or hybrid) this host cannot be measured.
    if !cgroup_root.join("cgroup.controllers").is_file() {
        return SliceTelemetry::not_supported();
    }
    let (mem, slice_psi) = read_slices(cgroup_root);
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
        let (mem, psi) = read_slices(d.path());
        assert_eq!(mem.len(), 2, "agent-builds.slice is absent and omitted");
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
        let (mem, _) = read_slices(d.path());
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
}
