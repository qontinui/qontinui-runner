//! The 500 ms process sampler: CPU-time deltas, the catalog's memory number
//! (private bytes on Windows, PSS on Linux), working set / RSS and per-PID GPU
//! engine utilisation, for three groups (KPI catalog attribution):
//!
//! - `host`: the setup's process tree (the given roots and every descendant)
//!   MINUS the workload. The PTY host (conhost.exe / OpenConsole.exe) stays,
//!   because the host chooses it.
//! - `workload`: every generator carrying `--termhost-bench-generator <run-id>`
//!   plus its parent session shell (D13 makes the shell identical for every
//!   setup, so it is workload, not host).
//! - `compositor`: dwm.exe / the X server — shared by everything on the
//!   machine and attributed to no setup.
//!
//! A host process that disappears mid-run is reported in `exited` (the
//! catalog's `crashes`).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::platform::{GpuReading, ProcessReader};
use crate::records::{ExitedProcess, GroupSample, Ns, SampleRecord};

pub const GROUP_HOST: &str = "host";
pub const GROUP_WORKLOAD: &str = "workload";
pub const GROUP_COMPOSITOR: &str = "compositor";
/// The generator flag D13's command line carries.
pub const GENERATOR_FLAG: &str = "--termhost-bench-generator";

/// What to sample.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SamplerConfig {
    pub host_pids: Vec<u32>,
    /// Generators are matched by this run id; `None` matches any generator.
    pub run_id: Option<String>,
    pub compositor: bool,
    /// The X display whose server is the compositor (Linux).
    pub display: Option<String>,
}

/// One row of the process table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcInfo {
    pub parent: Option<u32>,
    pub name: String,
    pub cmd: Vec<String>,
    pub start_time: u64,
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

/// Assign PIDs to groups (pure).
pub fn assign_groups(
    table: &BTreeMap<u32, ProcInfo>,
    cfg: &SamplerConfig,
) -> BTreeMap<&'static str, Vec<u32>> {
    let roots: BTreeSet<u32> = cfg
        .host_pids
        .iter()
        .copied()
        .filter(|p| table.contains_key(p))
        .collect();
    let mut workload = BTreeSet::new();
    for (&pid, p) in table {
        if is_generator(p, cfg.run_id.as_deref()) {
            workload.insert(pid);
            if let Some(parent) = p.parent {
                // The session shell — never a host root (a host that spawns
                // the generator directly keeps its own accounting).
                if !roots.contains(&parent) && table.contains_key(&parent) {
                    workload.insert(parent);
                }
            }
        }
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, p) in table {
        if let Some(parent) = p.parent {
            if parent != pid {
                children.entry(parent).or_default().push(pid);
            }
        }
    }
    let mut host = BTreeSet::new();
    let mut queue: VecDeque<u32> = roots.iter().copied().collect();
    while let Some(pid) = queue.pop_front() {
        if workload.contains(&pid) || !host.insert(pid) {
            continue;
        }
        if let Some(kids) = children.get(&pid) {
            queue.extend(kids.iter().copied());
        }
    }
    let mut groups = BTreeMap::new();
    groups.insert(GROUP_HOST, host.into_iter().collect());
    groups.insert(GROUP_WORKLOAD, workload.into_iter().collect());
    if cfg.compositor {
        let comp: Vec<u32> = table
            .iter()
            .filter(|(_, p)| is_compositor(p, cfg.display.as_deref()))
            .map(|(&pid, _)| pid)
            .collect();
        groups.insert(GROUP_COMPOSITOR, comp);
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
        let groups = assign_groups(&table, &self.cfg);
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
            let mut cpu_delta: u64 = 0;
            let mut any_delta = false;
            let mut mem_sum: u64 = 0;
            let mut rss_sum: u64 = 0;
            let mut mem_any = false;
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
                        mem_any = true;
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
            if let Some(dt) = dt_ns.filter(|&d| d > 0) {
                if any_delta || pids.is_empty() {
                    g.cpu_pct = Some(cpu_delta as f64 / dt as f64 * 100.0);
                    g.cpu_ms = Some(cpu_delta as f64 / 1e6);
                }
            }
            if mem_any || pids.is_empty() {
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
            dt_ms: dt_ns.map_or(0.0, |d| d as f64 / 1e6),
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

    #[test]
    fn generators_and_their_shells_are_workload_not_host() {
        let cfg = SamplerConfig {
            host_pids: vec![100],
            run_id: Some("run-7".into()),
            compositor: true,
            display: None,
        };
        let g = assign_groups(&table(), &cfg);
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
        let g = assign_groups(&table(), &cfg);
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
        let g = assign_groups(&t, &cfg);
        assert_eq!(g[GROUP_HOST], vec![1]);
        assert_eq!(g[GROUP_WORKLOAD], vec![2]);
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
        assert_eq!(assign_groups(&t, &cfg)[GROUP_COMPOSITOR], vec![10]);
    }

    struct FakeReader {
        cpu: HashMap<u32, u64>,
    }

    impl ProcessReader for FakeReader {
        fn cpu_ns(&mut self, pid: u32) -> Option<u64> {
            self.cpu.get(&pid).copied()
        }
        fn memory(&mut self, _pid: u32) -> Option<crate::platform::ProcMem> {
            Some(crate::platform::ProcMem {
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
            }),
        );
        let first = s.sample(0);
        assert_eq!(
            first.groups[GROUP_HOST].cpu_pct, None,
            "no delta on the first sample"
        );
        assert!(first.groups[GROUP_HOST].pids.contains(&me));
        s.reader = Box::new(FakeReader {
            cpu: HashMap::from([(me, 1_250_000_000)]),
        });
        let second = s.sample(500_000_000);
        let host = &second.groups[GROUP_HOST];
        let own_share = host.cpu_pct.expect("cpu");
        assert!((own_share - 50.0).abs() < 1e-6, "{own_share}");
        assert_eq!(host.memory_mb, Some(host.pids.len() as f64));
        assert_eq!(host.gpu_pct, Some(12.5));
        assert_eq!(second.gpu, "measured");
    }
}
