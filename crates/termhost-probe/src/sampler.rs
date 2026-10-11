//! The 500 ms process sampler: CPU-time deltas, the catalog's memory number
//! (private bytes on Windows, PSS on Linux), working set / RSS and per-PID GPU
//! engine utilisation, for these groups (KPI catalog attribution):
//!
//! - `host`: the setup's process tree (the given roots and every descendant)
//!   MINUS the workload and the probe. The PTY host (conhost.exe /
//!   OpenConsole.exe) stays, because the host chooses it. There is no host
//!   group at all when no root was given.
//! - `workload`: every generator — named by its `<run-dir>/slot-<k>` file or
//!   by its `--termhost-bench-generator <run-id>` command line — plus its
//!   session shell (D13 makes the shell identical for every setup, so it is
//!   workload, not host). Membership is fixed for the whole run, keyed by
//!   (pid, start time): a shell stays workload after its generator exits, and
//!   a workload process is never moved into the host group.
//! - `compositor`: dwm.exe / the X server — shared by everything on the
//!   machine and attributed to no setup.
//! - `probe`: this process (and anything it spawns), so the instrument's own
//!   cost is on record and never counted as the host's.
//!
//! A group with no live process, or with a member that could not be read,
//! reports `null` values and a `note`: a partial sum is not the group's
//! number, and an empty group's is unknown, not 0. A host process that
//! disappears mid-run is reported in `exited` (the catalog's `crashes`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::platform::{GpuReading, ProcessReader};
use crate::records::{ExitedProcess, GroupSample, Ns, SampleRecord};

pub const GROUP_HOST: &str = "host";
pub const GROUP_WORKLOAD: &str = "workload";
pub const GROUP_COMPOSITOR: &str = "compositor";
pub const GROUP_PROBE: &str = "probe";
/// The generator flag D13's command line carries.
pub const GENERATOR_FLAG: &str = "--termhost-bench-generator";
/// `GroupSample::note`: the group had no live process.
pub const NOTE_EMPTY: &str = "no_live_process";
/// `GroupSample::note`: a member's CPU or memory could not be read.
pub const NOTE_UNREADABLE: &str = "unreadable_members";

/// What to sample.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SamplerConfig {
    pub host_pids: Vec<u32>,
    /// Generators are matched by this run id; `None` matches any generator.
    pub run_id: Option<String>,
    pub compositor: bool,
    /// The X display whose server is the compositor (Linux).
    pub display: Option<String>,
    /// The run directory whose `slot-<k>` files name each generator's pid and
    /// parent (session shell) pid.
    pub run_dir: Option<PathBuf>,
    /// The probe's own pid: the `probe` group, never part of the host.
    pub probe_pid: Option<u32>,
}

/// One row of the process table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcInfo {
    pub parent: Option<u32>,
    pub name: String,
    pub cmd: Vec<String>,
    pub start_time: u64,
}

/// A generator's slot file: (generator pid, its parent — the session shell).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub pid: u32,
    pub ppid: u32,
}

fn is_generator(p: &ProcInfo, run_id: Option<&str>) -> bool {
    p.cmd
        .iter()
        .position(|a| a == GENERATOR_FLAG)
        .is_some_and(|i| match run_id {
            Some(id) => p.cmd.get(i + 1).is_some_and(|a| a == id),
            None => true,
        })
}

fn is_compositor(p: &ProcInfo, display: Option<&str>) -> bool {
    let name = p.name.to_ascii_lowercase();
    if name == "dwm.exe" || name == "dwm" {
        return true;
    }
    let x_server = matches!(
        name.as_str(),
        "xvfb" | "xorg" | "x" | "xwayland" | "xorg.bin"
    );
    x_server
        && match display {
            Some(d) => p.cmd.iter().any(|a| a == d),
            None => true,
        }
}

/// Parse one `slot-<k>` file (Phase 1's `termhost-bench/slot/v1`); `None` for
/// another run's slot or an unreadable one.
pub fn parse_slot(text: &str, run_id: Option<&str>) -> Option<Slot> {
    let v: Value = serde_json::from_str(text.lines().next()?).ok()?;
    if let (Some(want), Some(got)) = (run_id, v.get("run_id").and_then(Value::as_str)) {
        if want != got {
            return None;
        }
    }
    let pid = u32::try_from(v.get("pid")?.as_u64()?).ok()?;
    let ppid = u32::try_from(v.get("ppid")?.as_u64()?).ok()?;
    Some(Slot { pid, ppid })
}

/// Every generator slot in `dir` for `run_id`.
pub fn read_slots(dir: &Path, run_id: Option<&str>) -> Vec<Slot> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_prefix("slot-"))
                .is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| parse_slot(&t, run_id))
        .collect()
}

/// `roots` and every descendant, not descending into `skip`.
fn tree(
    roots: impl IntoIterator<Item = u32>,
    children: &HashMap<u32, Vec<u32>>,
    skip: &BTreeSet<u32>,
) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    let mut queue: VecDeque<u32> = roots.into_iter().collect();
    while let Some(pid) = queue.pop_front() {
        if skip.contains(&pid) || !out.insert(pid) {
            continue;
        }
        if let Some(kids) = children.get(&pid) {
            queue.extend(kids.iter().copied());
        }
    }
    out
}

/// Assign PIDs to groups. `workload_ids` holds every (pid, start time) ever
/// classified workload in this run: this sample's generators (by slot file or
/// command line) and their session shells are added to it, and every live
/// process in it is workload — so membership never lapses within a run.
pub fn assign_groups(
    table: &BTreeMap<u32, ProcInfo>,
    cfg: &SamplerConfig,
    slots: &[Slot],
    workload_ids: &mut HashSet<(u32, u64)>,
) -> BTreeMap<&'static str, Vec<u32>> {
    let roots: BTreeSet<u32> = cfg
        .host_pids
        .iter()
        .copied()
        .filter(|p| table.contains_key(p))
        .collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, p) in table {
        if let Some(parent) = p.parent {
            if parent != pid {
                children.entry(parent).or_default().push(pid);
            }
        }
    }
    let probe = cfg
        .probe_pid
        .filter(|p| table.contains_key(p))
        .map(|p| tree([p], &children, &BTreeSet::new()))
        .unwrap_or_default();

    // The session shell is never a host root (a host that spawns the
    // generator directly keeps its own accounting) and never the probe.
    let shell_ok = |p: u32| table.contains_key(&p) && !roots.contains(&p) && !probe.contains(&p);
    let mut candidates: Vec<u32> = Vec::new();
    for (&pid, p) in table {
        if is_generator(p, cfg.run_id.as_deref()) {
            candidates.push(pid);
            if let Some(parent) = p.parent.filter(|&pp| shell_ok(pp)) {
                candidates.push(parent);
            }
        }
    }
    for s in slots {
        if table.contains_key(&s.pid) {
            candidates.push(s.pid);
            if shell_ok(s.ppid) {
                candidates.push(s.ppid);
            }
        }
    }
    for pid in candidates {
        if let Some(p) = table.get(&pid) {
            workload_ids.insert((pid, p.start_time));
        }
    }
    let workload: BTreeSet<u32> = table
        .iter()
        .filter(|(pid, p)| workload_ids.contains(&(**pid, p.start_time)))
        .map(|(&pid, _)| pid)
        .collect();

    let mut groups = BTreeMap::new();
    if !cfg.host_pids.is_empty() {
        let skip: BTreeSet<u32> = workload.union(&probe).copied().collect();
        let host = tree(roots.iter().copied(), &children, &skip);
        groups.insert(GROUP_HOST, host.into_iter().collect());
    }
    groups.insert(GROUP_WORKLOAD, workload.into_iter().collect());
    if cfg.compositor {
        let comp: Vec<u32> = table
            .iter()
            .filter(|(_, p)| is_compositor(p, cfg.display.as_deref()))
            .map(|(&pid, _)| pid)
            .collect();
        groups.insert(GROUP_COMPOSITOR, comp);
    }
    if cfg.probe_pid.is_some() {
        groups.insert(GROUP_PROBE, probe.into_iter().collect());
    }
    groups
}

/// The sampler.
pub struct Sampler {
    cfg: SamplerConfig,
    sys: System,
    reader: Box<dyn ProcessReader>,
    prev_cpu: HashMap<u32, (u64, u64)>,
    members: HashMap<u32, (&'static str, String, u64)>,
    workload_ids: HashSet<(u32, u64)>,
    last_t: Option<u64>,
}

impl Sampler {
    pub fn new(cfg: SamplerConfig, reader: Box<dyn ProcessReader>) -> Sampler {
        Sampler {
            cfg,
            sys: System::new(),
            reader,
            prev_cpu: HashMap::new(),
            members: HashMap::new(),
            workload_ids: HashSet::new(),
            last_t: None,
        }
    }

    fn table(&mut self) -> BTreeMap<u32, ProcInfo> {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_cmd(UpdateKind::OnlyIfNotSet),
        );
        self.sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                (
                    pid.as_u32(),
                    ProcInfo {
                        parent: p.parent().map(|x| x.as_u32()),
                        name: p.name().to_string_lossy().into_owned(),
                        cmd: p
                            .cmd()
                            .iter()
                            .map(|a| a.to_string_lossy().into_owned())
                            .collect(),
                        start_time: p.start_time(),
                    },
                )
            })
            .collect()
    }

    /// Take one sample at `now_ns`.
    pub fn sample(&mut self, now_ns: u64) -> SampleRecord {
        let table = self.table();
        let slots = self
            .cfg
            .run_dir
            .as_deref()
            .map(|d| read_slots(d, self.cfg.run_id.as_deref()))
            .unwrap_or_default();
        let groups = assign_groups(&table, &self.cfg, &slots, &mut self.workload_ids);
        let dt_ns = self.last_t.map(|t| now_ns.saturating_sub(t));
        self.last_t = Some(now_ns);

        let gpu = self.reader.gpu();
        let (gpu_by_pid, gpu_status) = match gpu {
            GpuReading::Measured(m) => (Some(m), "measured".to_string()),
            GpuReading::Unsupported(why) => (None, format!("unsupported: {why}")),
            GpuReading::Failed(why) => (None, format!("failed: {why}")),
        };

        let mut out_groups = BTreeMap::new();
        let mut now_members = HashMap::new();
        for (&name, pids) in &groups {
            let mut g = GroupSample {
                pids: pids.clone(),
                ..GroupSample::default()
            };
            if pids.is_empty() {
                g.note = Some(NOTE_EMPTY);
                out_groups.insert(name, g);
                continue;
            }
            let mut cpu_delta: u64 = 0;
            let mut any_delta = false;
            let mut mem_sum: u64 = 0;
            let mut rss_sum: u64 = 0;
            for &pid in pids {
                let start = table.get(&pid).map_or(0, |p| p.start_time);
                now_members.insert(
                    pid,
                    (
                        name,
                        table.get(&pid).map(|p| p.name.clone()).unwrap_or_default(),
                        start,
                    ),
                );
                match self.reader.cpu_ns(pid) {
                    Some(c) => {
                        if let Some(&(prev_start, prev_cpu)) = self.prev_cpu.get(&pid) {
                            if prev_start == start && c >= prev_cpu {
                                cpu_delta += c - prev_cpu;
                                any_delta = true;
                            }
                        }
                        self.prev_cpu.insert(pid, (start, c));
                    }
                    None => g.unreadable += 1,
                }
                match self.reader.memory(pid) {
                    Some(m) => {
                        mem_sum += m.catalog_bytes;
                        rss_sum += m.rss_bytes;
                    }
                    None => g.unreadable += 1,
                }
                if let Some(by_pid) = &gpu_by_pid {
                    if let Some(engines) = by_pid.get(&pid) {
                        for (engine, pct) in engines {
                            *g.gpu_by_engine.entry(engine.clone()).or_default() += pct;
                        }
                    }
                }
            }
            if g.unreadable > 0 {
                g.note = Some(NOTE_UNREADABLE);
            } else {
                if let Some(dt) = dt_ns.filter(|&d| d > 0) {
                    if any_delta {
                        g.cpu_pct = Some(cpu_delta as f64 / dt as f64 * 100.0);
                        g.cpu_ms = Some(cpu_delta as f64 / 1e6);
                    }
                }
                g.memory_mb = Some(mem_sum as f64 / 1_048_576.0);
                g.rss_mb = Some(rss_sum as f64 / 1_048_576.0);
            }
            if gpu_by_pid.is_some() {
                g.gpu_pct = Some(g.gpu_by_engine.values().copied().fold(0.0, f64::max));
            }
            out_groups.insert(name, g);
        }

        let mut exited = Vec::new();
        for (pid, (group, name, start)) in &self.members {
            let gone = table.get(pid).is_none_or(|p| p.start_time != *start);
            if gone {
                exited.push(ExitedProcess {
                    pid: *pid,
                    group,
                    name: name.clone(),
                });
                self.prev_cpu.remove(pid);
            }
        }
        exited.sort_by_key(|e| e.pid);
        self.members = now_members;

        SampleRecord {
            t_ns: Ns(now_ns),
            dt_ms: dt_ns.map(|d| d as f64 / 1e6),
            groups: out_groups,
            gpu: gpu_status,
            exited,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(parent: Option<u32>, name: &str, cmd: &[&str]) -> ProcInfo {
        ProcInfo {
            parent,
            name: name.into(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            start_time: 1,
        }
    }

    fn table() -> BTreeMap<u32, ProcInfo> {
        let mut t = BTreeMap::new();
        t.insert(100, p(None, "WindowsTerminal.exe", &["wt.exe"]));
        t.insert(101, p(Some(100), "OpenConsole.exe", &[]));
        t.insert(
            102,
            p(
                Some(100),
                "powershell.exe",
                &["powershell.exe", "-NoLogo", "-NoExit"],
            ),
        );
        t.insert(
            103,
            p(
                Some(102),
                "node.exe",
                &[
                    "node",
                    "ai-session.mjs",
                    "--run-dir",
                    "d",
                    GENERATOR_FLAG,
                    "run-7",
                ],
            ),
        );
        t.insert(104, p(Some(100), "powershell.exe", &[]));
        t.insert(
            105,
            p(
                Some(104),
                "node.exe",
                &["node", "ai-session.mjs", GENERATOR_FLAG, "run-8"],
            ),
        );
        t.insert(200, p(None, "dwm.exe", &[]));
        t.insert(300, p(None, "explorer.exe", &[]));
        t
    }

    fn assign(
        t: &BTreeMap<u32, ProcInfo>,
        cfg: &SamplerConfig,
    ) -> BTreeMap<&'static str, Vec<u32>> {
        assign_groups(t, cfg, &[], &mut HashSet::new())
    }

    #[test]
    fn generators_and_their_shells_are_workload_not_host() {
        let cfg = SamplerConfig {
            host_pids: vec![100],
            run_id: Some("run-7".into()),
            compositor: true,
            ..SamplerConfig::default()
        };
        let g = assign(&table(), &cfg);
        assert_eq!(g[GROUP_WORKLOAD], vec![102, 103]);
        // run-8's generator belongs to another run: it stays in the host tree.
        assert_eq!(g[GROUP_HOST], vec![100, 101, 104, 105]);
        assert_eq!(g[GROUP_COMPOSITOR], vec![200]);
    }

    #[test]
    fn without_a_run_id_every_generator_is_workload() {
        let cfg = SamplerConfig {
            host_pids: vec![100],
            ..SamplerConfig::default()
        };
        let g = assign(&table(), &cfg);
        assert_eq!(g[GROUP_WORKLOAD], vec![102, 103, 104, 105]);
        assert_eq!(g[GROUP_HOST], vec![100, 101]);
        assert!(!g.contains_key(GROUP_COMPOSITOR));
    }

    #[test]
    fn a_generator_spawned_directly_by_the_host_never_removes_the_host() {
        let mut t = BTreeMap::new();
        t.insert(1, p(None, "tmux", &[]));
        t.insert(2, p(Some(1), "node", &["node", GENERATOR_FLAG, "r"]));
        let cfg = SamplerConfig {
            host_pids: vec![1],
            run_id: Some("r".into()),
            ..SamplerConfig::default()
        };
        let g = assign(&t, &cfg);
        assert_eq!(g[GROUP_HOST], vec![1]);
        assert_eq!(g[GROUP_WORKLOAD], vec![2]);
    }

    #[test]
    fn a_session_shell_stays_workload_after_its_generator_exits() {
        let cfg = SamplerConfig {
            host_pids: vec![100],
            run_id: Some("run-7".into()),
            ..SamplerConfig::default()
        };
        let mut ids = HashSet::new();
        let first = assign_groups(&table(), &cfg, &[], &mut ids);
        assert_eq!(first[GROUP_WORKLOAD], vec![102, 103]);
        // The stop file: the generator exits, its shell lives on.
        let mut after = table();
        after.remove(&103);
        let second = assign_groups(&after, &cfg, &[], &mut ids);
        assert_eq!(
            second[GROUP_WORKLOAD],
            vec![102],
            "the shell is still workload"
        );
        assert!(!second[GROUP_HOST].contains(&102), "{second:?}");
        // A NEW process reusing the pid is not the old shell.
        let mut reused = after.clone();
        if let Some(shell) = reused.get_mut(&102) {
            shell.start_time = 99;
        }
        let third = assign_groups(&reused, &cfg, &[], &mut ids);
        assert!(third[GROUP_HOST].contains(&102), "{third:?}");
    }

    #[test]
    fn slot_files_name_generators_whose_command_line_is_unreadable() {
        let mut t = table();
        if let Some(gen) = t.get_mut(&103) {
            gen.cmd.clear();
        }
        let cfg = SamplerConfig {
            host_pids: vec![100],
            run_id: Some("run-7".into()),
            ..SamplerConfig::default()
        };
        let slots = [Slot {
            pid: 103,
            ppid: 102,
        }];
        let g = assign_groups(&t, &cfg, &slots, &mut HashSet::new());
        assert_eq!(g[GROUP_WORKLOAD], vec![102, 103]);
        assert!(!g[GROUP_HOST].contains(&102));
        let slot =
            r#"{"schema":"termhost-bench/slot/v1","slot":0,"run_id":"run-7","pid":103,"ppid":102}"#;
        assert_eq!(
            parse_slot(slot, Some("run-7")),
            Some(Slot {
                pid: 103,
                ppid: 102
            })
        );
        assert_eq!(parse_slot(slot, Some("run-8")), None, "another run's slot");
        assert_eq!(parse_slot("not json", None), None);
    }

    #[test]
    fn no_roots_means_no_host_group() {
        let cfg = SamplerConfig {
            run_id: Some("run-7".into()),
            compositor: true,
            ..SamplerConfig::default()
        };
        let g = assign(&table(), &cfg);
        assert!(!g.contains_key(GROUP_HOST), "{g:?}");
        assert_eq!(g[GROUP_WORKLOAD], vec![102, 103]);
    }

    #[test]
    fn the_probe_is_its_own_group_and_never_the_hosts() {
        let mut t = table();
        t.insert(400, p(Some(100), "termhost-probe.exe", &[]));
        let cfg = SamplerConfig {
            host_pids: vec![100],
            probe_pid: Some(400),
            ..SamplerConfig::default()
        };
        let g = assign(&t, &cfg);
        assert_eq!(g[GROUP_PROBE], vec![400]);
        assert!(!g[GROUP_HOST].contains(&400));
    }

    #[test]
    fn the_x_server_for_our_display_is_the_compositor() {
        let mut t = BTreeMap::new();
        t.insert(
            10,
            p(
                None,
                "Xvfb",
                &["Xvfb", ":99", "-screen", "0", "1280x1024x24"],
            ),
        );
        t.insert(11, p(None, "Xvfb", &["Xvfb", ":5"]));
        let cfg = SamplerConfig {
            compositor: true,
            display: Some(":99".into()),
            ..SamplerConfig::default()
        };
        assert_eq!(assign(&t, &cfg)[GROUP_COMPOSITOR], vec![10]);
    }

    struct FakeReader {
        cpu: HashMap<u32, u64>,
        unreadable: Option<u32>,
    }

    impl ProcessReader for FakeReader {
        fn cpu_ns(&mut self, pid: u32) -> Option<u64> {
            // Any other live process in the tree (a node another test spawned
            // as our child) reads as idle, never as unreadable.
            (self.unreadable != Some(pid)).then(|| self.cpu.get(&pid).copied().unwrap_or(0))
        }
        fn memory(&mut self, pid: u32) -> Option<crate::platform::ProcMem> {
            (self.unreadable != Some(pid)).then_some(crate::platform::ProcMem {
                catalog_bytes: 1_048_576,
                rss_bytes: 2_097_152,
            })
        }
        fn gpu(&mut self) -> GpuReading {
            let mut m = HashMap::new();
            m.insert(
                std::process::id(),
                BTreeMap::from([("3D".to_string(), 12.5)]),
            );
            GpuReading::Measured(m)
        }
    }

    #[test]
    fn own_process_sampling_produces_cpu_memory_and_gpu() {
        let me = std::process::id();
        let mut s = Sampler::new(
            SamplerConfig {
                host_pids: vec![me],
                ..SamplerConfig::default()
            },
            Box::new(FakeReader {
                cpu: HashMap::from([(me, 1_000_000_000)]),
                unreadable: None,
            }),
        );
        let first = s.sample(0);
        assert_eq!(first.dt_ms, None, "no interval before the first sample");
        assert_eq!(
            first.groups[GROUP_HOST].cpu_pct, None,
            "no delta on the first sample"
        );
        assert!(first.groups[GROUP_HOST].pids.contains(&me));
        s.reader = Box::new(FakeReader {
            cpu: HashMap::from([(me, 1_250_000_000)]),
            unreadable: None,
        });
        let second = s.sample(500_000_000);
        assert_eq!(second.dt_ms, Some(500.0));
        let host = &second.groups[GROUP_HOST];
        let own_share = host.cpu_pct.expect("cpu");
        assert!((own_share - 50.0).abs() < 1e-6, "{own_share}");
        assert_eq!(host.memory_mb, Some(host.pids.len() as f64));
        assert_eq!(host.gpu_pct, Some(12.5));
        assert_eq!(second.gpu, "measured");
    }

    #[test]
    fn an_empty_host_group_reports_no_numbers() {
        // A root that is not (or no longer) alive: a crashed host, or a
        // launcher that handed off and exited.
        let mut s = Sampler::new(
            SamplerConfig {
                host_pids: vec![u32::MAX - 7],
                ..SamplerConfig::default()
            },
            Box::new(FakeReader {
                cpu: HashMap::new(),
                unreadable: None,
            }),
        );
        for t in [0, 500_000_000] {
            let host = &s.sample(t).groups[GROUP_HOST];
            assert!(host.pids.is_empty());
            assert_eq!(host.note, Some(NOTE_EMPTY));
            assert_eq!(
                (host.cpu_pct, host.memory_mb, host.gpu_pct),
                (None, None, None),
                "an empty group is unknown, not 0"
            );
        }
    }

    #[test]
    fn a_partly_unreadable_group_withholds_its_sums() {
        let me = std::process::id();
        let mut s = Sampler::new(
            SamplerConfig {
                host_pids: vec![me],
                ..SamplerConfig::default()
            },
            Box::new(FakeReader {
                cpu: HashMap::from([(me, 1)]),
                unreadable: Some(me),
            }),
        );
        let host = &s.sample(0).groups[GROUP_HOST];
        assert_eq!(host.note, Some(NOTE_UNREADABLE));
        assert_eq!(host.memory_mb, None);
        assert!(host.unreadable > 0);
    }
}
