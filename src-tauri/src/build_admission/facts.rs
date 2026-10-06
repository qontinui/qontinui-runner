//! Host facts for the admission core (plan D3), each with its provenance.
//!
//! Pure parsers (`parse_*`, [`attribute`], [`NonBuildTracker`]) do the work
//! and are tested on fixtures; the `read_*` functions only fetch text. Linux
//! reads `/proc` and cgroupfs; elsewhere memory comes from the runner's own
//! commit-available accessor and PSI is `not_supported` (it drops its
//! conjunct, plan D3) — never `unknown`.
//!
//! Frozen state is read from a scope's `cgroup.events` (`frozen 1`), never from
//! `ps`: a task frozen through `cgroup.freeze` still shows state `S` (measured
//! on merytshost 2026-10-06, recorded in the plan's Progress block and coord
//! finding `bed01ee9`), so `ps` cannot tell a frozen build from a sleeping one.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use qontinui_types::build_admission::{estimate::nearest_rank, Fact, HostFacts};
use serde::Serialize;

use super::ci_reservation::{self, CiMeasure};

/// The comm names that are build work (plan D4: attributed build trees).
pub const BUILD_COMMS: &[&str] = &["rustc", "clippy-driver"];

/// Where the readers look, and whether they are Linux files. Injected so tests
/// never read the real host.
#[derive(Debug, Clone)]
pub struct Roots {
    pub proc: PathBuf,
    pub cgroup: PathBuf,
    /// `/proc` and cgroupfs exist (Linux). Off Linux memory comes from the
    /// runner's commit-available accessor and PSI is `not_supported`.
    pub linux: bool,
}

impl Roots {
    pub fn host() -> Self {
        Roots {
            proc: PathBuf::from("/proc"),
            cgroup: PathBuf::from("/sys/fs/cgroup"),
            linux: cfg!(target_os = "linux"),
        }
    }
}

/// `(MemTotal, MemAvailable)` in bytes from `/proc/meminfo` text.
pub fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let kb = |key: &str| {
        text.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|v| v * 1024)
    };
    (kb("MemTotal:"), kb("MemAvailable:"))
}

/// Memory PSI `full avg10` (percent) from `/proc/pressure/memory` text.
pub fn parse_psi_full_avg10(text: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with("full "))?;
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix("avg10="))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

/// Memory PSI `full avg300` (percent), published beside avg10.
pub fn parse_psi_full_avg300(text: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with("full "))?;
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix("avg300="))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

/// Whether a cgroup is frozen, from its `cgroup.events` text (`frozen 0|1`).
/// `None` when the key is absent (a kernel without the freezer).
pub fn parse_cgroup_frozen(events: &str) -> Option<bool> {
    events.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        (it.next() == Some("frozen")).then(|| it.next() == Some("1"))
    })
}

/// [`parse_cgroup_frozen`] for the cgroup at `dir` (a lease's scope).
pub fn read_frozen(dir: &Path) -> Option<bool> {
    std::fs::read_to_string(dir.join("cgroup.events"))
        .ok()
        .and_then(|t| parse_cgroup_frozen(&t))
}

/// One process, as far as attribution needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub ppid: u32,
    pub comm: String,
    pub anon_bytes: u64,
    /// Kernel start time (clock ticks since boot): pid + start is identity.
    pub start: u64,
}

/// `(comm, ppid, start)` from `/proc/<pid>/stat` text.
pub fn parse_stat(text: &str) -> Option<(String, u32, u64)> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let comm = text.get(open + 1..close)?.to_owned();
    let rest: Vec<&str> = text.get(close + 2..)?.split_whitespace().collect();
    Some((
        comm,
        rest.get(1)?.parse().ok()?,
        rest.get(19)?.parse().ok()?,
    ))
}

/// `(uid, RssAnon bytes)` from `/proc/<pid>/status` text. `RssAnon` is absent
/// for kernel threads and zombies, which are skipped, not counted as 0.
pub fn parse_status(text: &str) -> Option<(u32, u64)> {
    let mut uid = None;
    let mut anon = None;
    for l in text.lines() {
        if let Some(v) = l.strip_prefix("Uid:") {
            uid = v.split_whitespace().next().and_then(|u| u.parse().ok());
        } else if let Some(v) = l.strip_prefix("RssAnon:") {
            anon = v
                .split_whitespace()
                .next()
                .and_then(|k| k.parse::<u64>().ok())
                .map(|k| k * 1024);
        }
    }
    Some((uid?, anon?))
}

/// The calling uid's processes from a `/proc` tree.
pub fn read_proc_table(proc_root: &Path, uid: u32) -> HashMap<u32, Proc> {
    let mut out = HashMap::new();
    let Ok(rd) = std::fs::read_dir(proc_root) else {
        return out;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let base = e.path();
        let (Ok(stat), Ok(status)) = (
            std::fs::read_to_string(base.join("stat")),
            std::fs::read_to_string(base.join("status")),
        ) else {
            continue;
        };
        let (Some((comm, ppid, start)), Some((puid, anon))) =
            (parse_stat(&stat), parse_status(&status))
        else {
            continue;
        };
        if puid == uid {
            out.insert(
                pid,
                Proc {
                    ppid,
                    comm,
                    anon_bytes: anon,
                    start,
                },
            );
        }
    }
    out
}

/// Build memory split by whether a lease owns it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BuildRss {
    pub leased_bytes: u64,
    pub unleased_bytes: u64,
    pub build_procs: u32,
    /// Anonymous bytes under each leased wrapper pid (every process at every
    /// depth below it: cargo, rustc, linkers, build scripts).
    #[serde(skip)]
    pub per_lease: HashMap<u32, u64>,
}

/// Attribute memory to builds. A process with a leased wrapper pid among its
/// ancestors (or itself) belongs to that lease, whatever it is. Anything else
/// in a cargo tree (an ancestor-or-self named `cargo`), and any orphan
/// `rustc`/`clippy-driver`, is UNLEASED build memory (plan D4). Everything
/// else is not build memory at all.
pub fn attribute(table: &HashMap<u32, Proc>, lease_pids: &HashSet<u32>) -> BuildRss {
    let mut out = BuildRss::default();
    for (pid, p) in table {
        let mut seen = HashSet::new();
        let mut cur = Some(*pid);
        let mut lease = None;
        let mut in_cargo_tree = false;
        while let Some(c) = cur {
            if !seen.insert(c) {
                break;
            }
            if lease_pids.contains(&c) {
                lease = Some(c);
                break;
            }
            let Some(node) = table.get(&c) else { break };
            in_cargo_tree |= node.comm == "cargo";
            cur = Some(node.ppid);
        }
        if BUILD_COMMS.contains(&p.comm.as_str()) {
            out.build_procs += 1;
        }
        match lease {
            Some(l) => {
                out.leased_bytes += p.anon_bytes;
                *out.per_lease.entry(l).or_default() += p.anon_bytes;
            }
            None if in_cargo_tree || BUILD_COMMS.contains(&p.comm.as_str()) => {
                out.unleased_bytes += p.anon_bytes;
            }
            None => {}
        }
    }
    out
}

/// Non-build used memory over time (plan D3 `non_build_p95_24h`): one sample a
/// minute; `live_only` (the newest sample) until an hour of samples exists,
/// then `measured` p95 over the last 24 h.
#[derive(Debug, Default)]
pub struct NonBuildTracker {
    samples: VecDeque<(u64, u64)>,
}

const MINUTE: u64 = 60;
const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;

impl NonBuildTracker {
    pub fn push(&mut self, now_s: u64, bytes: u64) {
        if self.samples.back().is_some_and(|(t, _)| now_s < t + MINUTE) {
            // Keep the newest value within the minute for the live reading.
            if let Some(last) = self.samples.back_mut() {
                last.1 = bytes;
            }
            return;
        }
        self.samples.push_back((now_s, bytes));
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now_s.saturating_sub(*t) > DAY)
        {
            self.samples.pop_front();
        }
    }

    pub fn fact(&self) -> Fact<u64> {
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return Fact::Unknown;
        };
        if last.0.saturating_sub(first.0) < HOUR {
            return Fact::LiveOnly(last.1);
        }
        let vals: Vec<u64> = self.samples.iter().map(|(_, b)| *b).collect();
        nearest_rank(&vals, 0.95).map_or(Fact::Unknown, Fact::Measured)
    }
}

fn sysinfo_total_memory() -> Option<u64> {
    let mut s = sysinfo::System::new();
    s.refresh_memory();
    Some(s.total_memory()).filter(|t| *t > 0)
}

/// Everything the broker measured this tick, for the state endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct FactsDetail {
    pub facts: HostFacts,
    pub psi_mem_full_avg300: Option<f64>,
    pub ci: CiMeasure,
    pub build_rss: Option<BuildRss>,
    pub measured_at_s: u64,
}

/// Memory PSI from `/proc/pressure/memory`. A kernel without PSI (no
/// `pressure` directory, or a read refused with EOPNOTSUPP under `psi=0`)
/// cannot provide it: `not_supported`, which drops the conjunct. Any other
/// failure is `unknown`, which withholds.
fn read_psi(roots: &Roots) -> (Fact<f64>, Option<f64>) {
    if !roots.linux {
        return (Fact::NotSupported, None);
    }
    let dir = roots.proc.join("pressure");
    if !dir.exists() {
        return (Fact::NotSupported, None);
    }
    match std::fs::read_to_string(dir.join("memory")) {
        Ok(t) => (
            parse_psi_full_avg10(&t).map_or(Fact::Unknown, Fact::Measured),
            parse_psi_full_avg300(&t),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported || e.raw_os_error() == Some(95) => {
            (Fact::NotSupported, None)
        }
        Err(_) => (Fact::Unknown, None),
    }
}

/// Non-build used memory for one sample, or `None` when a term is unknown.
/// A term the platform cannot provide subtracts nothing (D3).
pub fn non_build_sample(
    mem_total: Option<u64>,
    mem_available: Option<u64>,
    build: Option<&BuildRss>,
    ci_usage: Fact<u64>,
) -> Option<u64> {
    let used = mem_total?.saturating_sub(mem_available?);
    let builds = build.map_or(0, |b| b.leased_bytes + b.unleased_bytes);
    let ci = match ci_usage.resolve() {
        qontinui_types::build_admission::Resolved::Value(v) => v,
        qontinui_types::build_admission::Resolved::Dropped => 0,
        qontinui_types::build_admission::Resolved::Unknown => return None,
    };
    Some(used.saturating_sub(builds).saturating_sub(ci))
}

/// Collect one tick's facts. `lease_pids` are the live wrapper pids.
pub fn collect(
    roots: &Roots,
    uid: Option<u32>,
    lease_pids: &HashSet<u32>,
    tracker: &mut NonBuildTracker,
    admissions_paused: bool,
    now_s: u64,
) -> FactsDetail {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let (mem_total, mem_available) = if roots.linux {
        std::fs::read_to_string(roots.proc.join("meminfo"))
            .ok()
            .map(|t| parse_meminfo(&t))
            .unwrap_or((None, None))
    } else {
        (
            sysinfo_total_memory(),
            crate::fleet::resource_sample::available_commit_bytes(),
        )
    };
    let (psi10, psi300) = read_psi(roots);
    let ci = if roots.linux {
        ci_reservation::measure(&roots.cgroup)
    } else {
        ci_reservation::measure(Path::new(""))
    };
    // Build-tree attribution needs /proc; off Linux it is not supported and
    // subtracts nothing from non-build use (counted as non-build: conservative).
    let build = match (roots.linux, uid) {
        (true, Some(u)) => Some(attribute(&read_proc_table(&roots.proc, u), lease_pids)),
        _ => None,
    };
    let unleased = match (&build, roots.linux) {
        (Some(b), _) => Fact::Measured(b.unleased_bytes),
        (None, true) => Fact::Unknown,
        (None, false) => Fact::NotSupported,
    };
    if let Some(nb) = non_build_sample(mem_total, mem_available, build.as_ref(), ci.usage) {
        tracker.push(now_s, nb);
    }
    FactsDetail {
        facts: HostFacts {
            mem_total_bytes: mem_total.unwrap_or(0),
            cpus,
            mem_available_bytes: mem_available.map_or(Fact::Unknown, Fact::Measured),
            psi_mem_full_avg10: psi10,
            non_build_p95_bytes: tracker.fact(),
            ci_reservation_bytes: ci.reservation,
            unleased_build_rss_bytes: unleased,
            admissions_paused,
        },
        psi_mem_full_avg300: psi300,
        ci,
        build_rss: build,
        measured_at_s: now_s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_meminfo_and_psi() {
        let m = "MemTotal: 1000 kB\nMemFree: 1 kB\nMemAvailable: 400 kB\n";
        assert_eq!(parse_meminfo(m), (Some(1_024_000), Some(409_600)));
        let p = "some avg10=1.00 avg60=2.00 avg300=3.00 total=1\nfull avg10=0.50 avg60=1.50 avg300=2.50 total=1\n";
        assert_eq!(parse_psi_full_avg10(p), Some(0.5));
        assert_eq!(parse_psi_full_avg300(p), Some(2.5));
        assert_eq!(parse_psi_full_avg10("some avg10=1.0\n"), None);
        assert_eq!(parse_psi_full_avg10("full avg10=nan\n"), None);
    }

    #[test]
    fn frozen_comes_from_cgroup_events_not_ps() {
        assert_eq!(parse_cgroup_frozen("populated 1\nfrozen 1\n"), Some(true));
        assert_eq!(parse_cgroup_frozen("populated 1\nfrozen 0\n"), Some(false));
        assert_eq!(parse_cgroup_frozen("populated 1\n"), None);
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        assert_eq!(read_frozen(t.path()), Some(true));
        assert_eq!(read_frozen(&t.path().join("gone")), None);
    }

    #[test]
    fn parses_stat_and_status() {
        let stat = "123 (clippy-driver) S 77 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 4242 0\n";
        assert_eq!(parse_stat(stat), Some(("clippy-driver".into(), 77, 4242)));
        assert_eq!(
            parse_status("Name:\tx\nUid:\t1000\t1000\t1000\t1000\nRssAnon:\t2048 kB\n"),
            Some((1000, 2048 * 1024))
        );
        assert_eq!(parse_status("Name:\tkthread\nUid:\t0\t0\t0\t0\n"), None);
    }

    fn p(ppid: u32, comm: &str, anon: u64) -> Proc {
        Proc {
            ppid,
            comm: comm.into(),
            anon_bytes: anon,
            start: 0,
        }
    }

    #[test]
    fn attributes_build_trees_at_every_depth() {
        let mut t = HashMap::new();
        t.insert(10, p(1, "bash", 1)); // a leased wrapper
        t.insert(11, p(10, "cargo", 5));
        t.insert(12, p(11, "clippy-driver", 100));
        t.insert(13, p(12, "rustc", 200)); // depth 3 below the wrapper
        t.insert(14, p(13, "ld", 50)); // a linker counts for its lease
        t.insert(20, p(1, "cargo", 5)); // an unleased build tree
        t.insert(21, p(20, "rustc", 30));
        t.insert(22, p(20, "cc", 4)); // its linker is build memory too
        t.insert(30, p(30, "rustc", 7)); // a self-parented orphan rustc
        t.insert(40, p(1, "node", 999)); // not build memory at all
        let lease = HashSet::from([10]);
        let b = attribute(&t, &lease);
        assert_eq!(
            (b.leased_bytes, b.unleased_bytes, b.build_procs),
            (1 + 5 + 100 + 200 + 50, 5 + 30 + 4 + 7, 4)
        );
        assert_eq!(b.per_lease[&10], 356);
    }

    #[test]
    fn reads_a_proc_tree_for_one_uid_only() {
        let d = tempfile::tempdir().unwrap();
        let mk = |pid: u32, ppid: u32, comm: &str, uid: u32, anon_kb: u64| {
            let dir = d.path().join(pid.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("stat"),
                format!("{pid} ({comm}) S {ppid} {} 9\n", "0 ".repeat(17)),
            )
            .unwrap();
            std::fs::write(
                dir.join("status"),
                format!(
                    "Name:\t{comm}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nRssAnon:\t{anon_kb} kB\n"
                ),
            )
            .unwrap();
        };
        mk(5, 1, "rustc", 1000, 3);
        mk(6, 1, "rustc", 1001, 3);
        let t = read_proc_table(d.path(), 1000);
        assert_eq!(t.len(), 1);
        assert_eq!(t[&5].anon_bytes, 3 * 1024);
        assert_eq!(t[&5].start, 9);
    }

    #[test]
    fn non_build_is_live_only_until_an_hour_then_a_measured_p95() {
        let mut tr = NonBuildTracker::default();
        assert_eq!(tr.fact(), Fact::Unknown);
        tr.push(0, 10);
        tr.push(30, 11); // same minute: replaces the live value
        assert_eq!(tr.fact(), Fact::LiveOnly(11));
        for m in 1..=60u64 {
            tr.push(m * 60, 100 + m);
        }
        // 61 samples: 11 and 101..=160; p95 = rank ceil(57.95) = 58 → 157.
        assert_eq!(tr.fact(), Fact::Measured(157));
        // Older than a day falls out.
        tr.push(2 * DAY, 5);
        assert_eq!(tr.fact(), Fact::LiveOnly(5));
    }

    #[test]
    fn non_build_subtracts_ci_usage_never_its_reservation() {
        const G: u64 = 1 << 30;
        let b = BuildRss {
            leased_bytes: 10 * G,
            unleased_bytes: 20 * G,
            ..Default::default()
        };
        // merytshost-like: 368 total, 150 available, CI holding 43 (it may
        // take 148.5 — that is not used memory).
        assert_eq!(
            non_build_sample(
                Some(368 * G),
                Some(150 * G),
                Some(&b),
                Fact::Measured(43 * G)
            ),
            Some((368 - 150 - 30 - 43) * G)
        );
        // No cgroup v2 and no attribution (Windows): terms drop, not withhold.
        assert_eq!(
            non_build_sample(Some(64 * G), Some(40 * G), None, Fact::NotSupported),
            Some(24 * G)
        );
        assert_eq!(
            non_build_sample(Some(64 * G), Some(40 * G), None, Fact::Unknown),
            None
        );
        assert_eq!(
            non_build_sample(None, Some(40 * G), None, Fact::NotSupported),
            None
        );
    }

    fn fixture_host() -> (tempfile::TempDir, Roots) {
        let d = tempfile::tempdir().unwrap();
        let proc = d.path().join("proc");
        let cg = d.path().join("cg");
        std::fs::create_dir_all(proc.join("pressure")).unwrap();
        std::fs::create_dir_all(&cg).unwrap();
        std::fs::write(
            proc.join("meminfo"),
            "MemTotal: 1048576 kB\nMemAvailable: 524288 kB\n",
        )
        .unwrap();
        std::fs::write(
            proc.join("pressure/memory"),
            "full avg10=1.50 avg60=0 avg300=0.25 total=0\n",
        )
        .unwrap();
        std::fs::write(cg.join("cgroup.controllers"), "memory\n").unwrap();
        let roots = Roots {
            proc,
            cgroup: cg,
            linux: true,
        };
        (d, roots)
    }

    #[test]
    fn collect_on_a_fixture_host() {
        let (_d, roots) = fixture_host();
        let mut tr = NonBuildTracker::default();
        let f = collect(&roots, Some(4242), &HashSet::new(), &mut tr, false, 100);
        assert_eq!(f.facts.mem_total_bytes, 1 << 30);
        assert_eq!(f.facts.mem_available_bytes, Fact::Measured(1 << 29));
        assert_eq!(f.facts.psi_mem_full_avg10, Fact::Measured(1.5));
        assert_eq!(f.psi_mem_full_avg300, Some(0.25));
        assert_eq!(
            f.ci.source,
            super::super::ci_reservation::CiSource::NoCiSlice
        );
        assert_eq!(f.facts.unleased_build_rss_bytes, Fact::Measured(0));
        assert_eq!(f.facts.non_build_p95_bytes, Fact::LiveOnly(1 << 29));
        // No pressure directory on Linux: the kernel lacks PSI.
        std::fs::remove_dir_all(roots.proc.join("pressure")).unwrap();
        let f = collect(&roots, Some(4242), &HashSet::new(), &mut tr, false, 200);
        assert_eq!(f.facts.psi_mem_full_avg10, Fact::NotSupported);
    }

    #[test]
    fn off_linux_psi_and_attribution_are_not_supported() {
        let (_d, mut roots) = fixture_host();
        roots.linux = false;
        let mut tr = NonBuildTracker::default();
        let f = collect(&roots, Some(4242), &HashSet::new(), &mut tr, false, 100);
        assert_eq!(f.facts.psi_mem_full_avg10, Fact::NotSupported);
        assert_eq!(f.facts.unleased_build_rss_bytes, Fact::NotSupported);
        assert_eq!(f.facts.ci_reservation_bytes, Fact::NotSupported);
        assert!(f.build_rss.is_none());
    }
}
