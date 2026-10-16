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
//! disappears mid-run is reported in `exited` (the catalog's `crashes`); so
//! is a generator, with the session its slot file names, because a generator
//! killed without writing its `exit` record must end its session's marker
//! schedule — otherwise its overdue markers read as a host hang.
//!
//! A read that fails is an exit only when the process is GONE: the reader
//! says so, or a fresh process-table read for that pid finds it absent (or
//! a different process under the pid). A process still in the table whose
//! read fails is an unreadable member and withholds the group's numbers. On
//! Linux that includes a ZOMBIE host member — exited but not yet reaped by
//! its parent: it stays in `/proc` while its memory reads fail, so it reads
//! as unreadable, not exited, until its parent reaps it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, ThreadKind, UpdateKind};

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

/// A generator's slot file: the generator's pid, its parent (the session
/// shell) and the session index it claimed (`slot`, which its markers carry).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub pid: u32,
    pub ppid: u32,
    pub session: Option<u16>,
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

/// Whether the live process `p` is the generator a slot file names. A slot
/// names its generator by PID, and a PID is not an identity: another process
/// can hold it (a reused PID; or, for tmux-wsl, a Windows process holding the
/// number of a generator that runs inside WSL, whose slot file carries a Linux
/// PID). So the process must carry this run's generator flag; or, where its
/// command line is unreadable, be a node process whose parent is the slot's
/// session shell.
fn slot_names(p: &ProcInfo, slot: &Slot, run_id: Option<&str>) -> bool {
    if is_generator(p, run_id) {
        return true;
    }
    let name = p.name.to_ascii_lowercase();
    p.cmd.is_empty() && (name == "node" || name == "node.exe") && p.parent == Some(slot.ppid)
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

/// Whether a sysinfo entry is a process rather than one of a process's
/// threads (Linux lists those too). A kernel thread is a process of its own.
fn is_process(kind: Option<ThreadKind>) -> bool {
    kind != Some(ThreadKind::Userland)
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
    let session = v
        .get("slot")
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok());
    Some(Slot { pid, ppid, session })
}

/// Every generator slot in `dir` for `run_id`. A slot whose record does not
/// carry its index takes it from the file name (`slot-<k>`).
pub fn read_slots(dir: &Path, run_id: Option<&str>) -> Vec<Slot> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let k: u16 = e
                .file_name()
                .to_str()?
                .strip_prefix("slot-")
                .filter(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))?
                .parse()
                .ok()?;
            let slot = parse_slot(&std::fs::read_to_string(e.path()).ok()?, run_id)?;
            Some(Slot {
                session: slot.session.or(Some(k)),
                ..slot
            })
        })
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
        let Some(p) = table.get(&s.pid) else { continue };
        if !slot_names(p, s, cfg.run_id.as_deref()) {
            continue;
        }
        candidates.push(s.pid);
        // The shell is the generator's own parent, never a PID the slot asserts.
        if p.parent == Some(s.ppid) && shell_ok(s.ppid) {
            candidates.push(s.ppid);
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

/// A process counted in some group at the previous sample.
#[derive(Clone, Debug)]
struct Member {
    group: &'static str,
    name: String,
    start_time: u64,
    /// The session whose generator it is, when a slot file named it.
    session: Option<u16>,
}

/// The sampler.
pub struct Sampler {
    cfg: SamplerConfig,
    sys: System,
    reader: Box<dyn ProcessReader>,
    prev_cpu: HashMap<u32, (u64, u64)>,
    members: HashMap<u32, Member>,
    workload_ids: HashSet<(u32, u64)>,
    /// Each slot file's generator, by (pid, start time): its session.
    gen_sessions: HashMap<(u32, u64), u16>,
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
            gen_sessions: HashMap::new(),
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
            // sysinfo lists every Linux THREAD (`/proc/<pid>/task/<tid>`) as a
            // process whose parent is its own process. Counted as members, a
            // host's threads multiplied its memory (each reads the whole
            // process's smaps_rollup: 109 GB for one runner in CI run
            // 38345475088), stretched a 500 ms sample to ~4 s with their reads,
            // and reported each exiting thread as a host crash. A process is
            // a process, not its threads.
            .filter(|(_, p)| is_process(p.thread_kind()))
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
        self.sample_table(&table, now_ns)
    }

    /// Whether `pid` (started at `start`) is gone, asked after a read of it
    /// failed: the reader's answer, else the process table re-read for it.
    fn has_exited(&mut self, pid: u32, start: u64) -> bool {
        if let Some(gone) = self.reader.gone(pid) {
            return gone;
        }
        let spid = sysinfo::Pid::from_u32(pid);
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[spid]),
            true,
            ProcessRefreshKind::new(),
        );
        self.sys
            .process(spid)
            .is_none_or(|p| p.start_time() != start)
    }

    /// One sample of an already-read process table.
    fn sample_table(&mut self, table: &BTreeMap<u32, ProcInfo>, now_ns: u64) -> SampleRecord {
        let slots = self
            .cfg
            .run_dir
            .as_deref()
            .map(|d| read_slots(d, self.cfg.run_id.as_deref()))
            .unwrap_or_default();
        for s in &slots {
            if let (Some(session), Some(p)) = (s.session, table.get(&s.pid)) {
                // Only the generator the slot names carries its session: an
                // unrelated process holding the PID never becomes "session k's
                // generator" in an exit record.
                if slot_names(p, s, self.cfg.run_id.as_deref()) {
                    self.gen_sessions.insert((s.pid, p.start_time), session);
                }
            }
        }
        let groups = assign_groups(table, &self.cfg, &slots, &mut self.workload_ids);
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
        let mut exited_now = Vec::new();
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
                let member = Member {
                    group: name,
                    name: table.get(&pid).map(|p| p.name.clone()).unwrap_or_default(),
                    start_time: start,
                    session: self.gen_sessions.get(&(pid, start)).copied(),
                };
                let cpu = self.reader.cpu_ns(pid);
                let mem = self.reader.memory(pid);
                if (cpu.is_none() || mem.is_none()) && self.has_exited(pid, start) {
                    // It exited between the table read and its own read:
                    // an exit, not an unreadable member — the group's sums
                    // are whole without it.
                    exited_now.push(ExitedProcess {
                        pid,
                        group: member.group,
                        name: member.name,
                        session: member.session,
                    });
                    self.prev_cpu.remove(&pid);
                    g.pids.retain(|&p| p != pid);
                    continue;
                }
                now_members.insert(pid, member);
                match cpu {
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
                match mem {
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
            if g.pids.is_empty() {
                // Every member exited mid-read.
                g.note = Some(NOTE_EMPTY);
            } else if g.unreadable > 0 {
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
            if gpu_by_pid.is_some() && !g.pids.is_empty() {
                g.gpu_pct = Some(g.gpu_by_engine.values().copied().fold(0.0, f64::max));
            }
            out_groups.insert(name, g);
        }

        let mut exited = exited_now;
        for (pid, m) in &self.members {
            let gone = table.get(pid).is_none_or(|p| p.start_time != m.start_time);
            if gone {
                exited.push(ExitedProcess {
                    pid: *pid,
                    group: m.group,
                    name: m.name.clone(),
                    session: m.session,
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
            session: Some(0),
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
                ppid: 102,
                session: Some(0),
            })
        );
        assert_eq!(parse_slot(slot, Some("run-8")), None, "another run's slot");
        assert_eq!(parse_slot("not json", None), None);
    }

    #[test]
    fn a_slot_pid_held_by_another_process_is_not_workload() {
        // tmux-wsl's slot files carry LINUX pids; on Windows those numbers can
        // belong to anything — here the host's own OpenConsole.exe (101) and a
        // stranger (500) with an unreadable command line under another parent.
        let mut t = table();
        t.insert(500, p(Some(9), "svchost.exe", &[]));
        let cfg = SamplerConfig {
            host_pids: vec![100],
            run_id: Some("run-7".into()),
            ..SamplerConfig::default()
        };
        let slots = [
            Slot {
                pid: 101,
                ppid: 100,
                session: Some(0),
            },
            Slot {
                pid: 500,
                ppid: 9,
                session: Some(1),
            },
        ];
        let g = assign_groups(&t, &cfg, &slots, &mut HashSet::new());
        assert!(
            g[GROUP_HOST].contains(&101),
            "the host's own OpenConsole stays host: {g:?}"
        );
        assert!(!g[GROUP_WORKLOAD].contains(&101), "{g:?}");
        assert!(!g[GROUP_WORKLOAD].contains(&500), "{g:?}");
        assert!(!g[GROUP_WORKLOAD].contains(&9), "{g:?}");
        // Another run's generator at the slot's PID is not this run's either.
        let mut other = table();
        if let Some(gen) = other.get_mut(&103) {
            gen.cmd[5] = "run-8".into();
        }
        let slots = [Slot {
            pid: 103,
            ppid: 102,
            session: Some(0),
        }];
        let g = assign_groups(&other, &cfg, &slots, &mut HashSet::new());
        assert!(g[GROUP_WORKLOAD].is_empty(), "{g:?}");
        // An unreadable command line is accepted only for node under the slot's shell.
        let mut unread = table();
        if let Some(gen) = unread.get_mut(&103) {
            gen.cmd.clear();
        }
        let wrong_shell = [Slot {
            pid: 103,
            ppid: 101,
            session: Some(0),
        }];
        let g = assign_groups(&unread, &cfg, &wrong_shell, &mut HashSet::new());
        assert!(g[GROUP_WORKLOAD].is_empty(), "{g:?}");
        // A slot naming the real generator but another "shell" (here the host's
        // OpenConsole) never moves that process out of the host.
        let g = assign_groups(&table(), &cfg, &wrong_shell, &mut HashSet::new());
        assert_eq!(
            g[GROUP_WORKLOAD],
            vec![102, 103],
            "the generator and its real parent"
        );
        assert!(g[GROUP_HOST].contains(&101), "{g:?}");
    }

    #[test]
    fn an_unrelated_process_at_a_slot_pid_never_names_a_session_when_it_exits() {
        let dir = std::env::temp_dir().join(format!(
            "termhost-probe-slots-stranger-{}-{}",
            std::process::id(),
            crate::clock::now_ns()
        ));
        std::fs::create_dir_all(&dir).expect("run dir");
        // A Linux pid from tmux-wsl's slot, held on this side by the host's OpenConsole.
        std::fs::write(
            dir.join("slot-0"),
            r#"{"schema":"termhost-bench/slot/v1","slot":0,"run_id":"run-7","pid":101,"ppid":100}"#,
        )
        .expect("slot-0");
        let mut s = Sampler::new(
            SamplerConfig {
                host_pids: vec![100],
                run_id: Some("run-7".into()),
                run_dir: Some(dir.clone()),
                ..SamplerConfig::default()
            },
            Box::new(FakeReader {
                cpu: HashMap::new(),
                unreadable: None,
                exited_mid_read: None,
            }),
        );
        let t = table();
        assert!(s.sample_table(&t, 0).exited.is_empty());
        let mut after = t.clone();
        after.remove(&101);
        let gone = s.sample_table(&after, 500_000_000).exited;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            gone.iter().all(|e| e.session.is_none()),
            "the OpenConsole's exit names no generator session: {gone:?}"
        );
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
        /// Exits between the table read and its own read.
        exited_mid_read: Option<u32>,
    }

    impl ProcessReader for FakeReader {
        fn gone(&mut self, pid: u32) -> Option<bool> {
            Some(self.exited_mid_read == Some(pid))
        }
        fn cpu_ns(&mut self, pid: u32) -> Option<u64> {
            if self.exited_mid_read == Some(pid) {
                return None;
            }
            // Any other live process in the tree (a node another test spawned
            // as our child) reads as idle, never as unreadable.
            (self.unreadable != Some(pid)).then(|| self.cpu.get(&pid).copied().unwrap_or(0))
        }
        fn memory(&mut self, pid: u32) -> Option<crate::platform::ProcMem> {
            (self.unreadable != Some(pid) && self.exited_mid_read != Some(pid)).then_some(
                crate::platform::ProcMem {
                    catalog_bytes: 1_048_576,
                    rss_bytes: 2_097_152,
                },
            )
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
                exited_mid_read: None,
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
            exited_mid_read: None,
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
                exited_mid_read: None,
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
    fn a_generator_seen_gone_names_its_session_and_a_shell_names_none() {
        let dir = std::env::temp_dir().join(format!(
            "termhost-probe-slots-{}-{}",
            std::process::id(),
            crate::clock::now_ns()
        ));
        std::fs::create_dir_all(&dir).expect("run dir");
        std::fs::write(
            dir.join("slot-0"),
            r#"{"schema":"termhost-bench/slot/v1","slot":0,"run_id":"run-7","pid":103,"ppid":102}"#,
        )
        .expect("slot-0");
        // A record without its index takes it from the file name.
        std::fs::write(
            dir.join("slot-5"),
            r#"{"schema":"termhost-bench/slot/v1","run_id":"run-7","pid":205,"ppid":204}"#,
        )
        .expect("slot-5");
        let sessions: BTreeSet<(u32, Option<u16>)> = read_slots(&dir, Some("run-7"))
            .into_iter()
            .map(|s| (s.pid, s.session))
            .collect();
        assert_eq!(sessions, BTreeSet::from([(103, Some(0)), (205, Some(5))]));

        let mut s = Sampler::new(
            SamplerConfig {
                host_pids: vec![100],
                run_id: Some("run-7".into()),
                run_dir: Some(dir.clone()),
                ..SamplerConfig::default()
            },
            Box::new(FakeReader {
                cpu: HashMap::new(),
                unreadable: None,
                exited_mid_read: None,
            }),
        );
        let t = table();
        assert!(s.sample_table(&t, 0).exited.is_empty());
        // The generator is killed; its shell lives on.
        let mut after = t.clone();
        after.remove(&103);
        let gone = s.sample_table(&after, 500_000_000).exited;
        assert_eq!(
            gone,
            vec![ExitedProcess {
                pid: 103,
                group: GROUP_WORKLOAD,
                name: "node.exe".into(),
                session: Some(0),
            }]
        );
        // The shell exits later: workload too, but no generator of a session.
        after.remove(&102);
        let gone = s.sample_table(&after, 1_000_000_000).exited;
        assert_eq!(gone.len(), 1);
        assert_eq!((gone[0].pid, gone[0].session), (102, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_member_that_exits_mid_read_is_an_exit_not_an_unreadable_member() {
        // OpenConsole (101) exits between the table read and its own read,
        // in what turns out to be the run's last sample: the host group's
        // numbers are whole without it, so its end memory is not lost.
        let mut s = Sampler::new(
            SamplerConfig {
                host_pids: vec![100],
                run_id: Some("run-7".into()),
                ..SamplerConfig::default()
            },
            Box::new(FakeReader {
                cpu: HashMap::new(),
                unreadable: None,
                exited_mid_read: Some(101),
            }),
        );
        let rec = s.sample_table(&table(), 0);
        let host = &rec.groups[GROUP_HOST];
        assert_eq!(host.note, None, "{host:?}");
        assert_eq!(host.unreadable, 0);
        assert_eq!(host.pids, vec![100, 104, 105]);
        assert_eq!(host.memory_mb, Some(3.0));
        assert_eq!(
            rec.exited
                .iter()
                .map(|e| (e.pid, e.group))
                .collect::<Vec<_>>(),
            vec![(101, GROUP_HOST)]
        );
        // Reported once: the next sample does not report it again.
        let mut after = table();
        after.remove(&101);
        assert!(s.sample_table(&after, 500_000_000).exited.is_empty());
    }

    /// A reader that answers `gone` with "don't know" — like the Windows
    /// and Linux readers — so the sampler's own process-table re-read
    /// decides, and whose reads of `failing` pids fail.
    struct TableDecides {
        failing: Vec<u32>,
    }

    impl ProcessReader for TableDecides {
        fn cpu_ns(&mut self, pid: u32) -> Option<u64> {
            (!self.failing.contains(&pid)).then_some(0)
        }
        fn memory(&mut self, pid: u32) -> Option<crate::platform::ProcMem> {
            (!self.failing.contains(&pid)).then_some(crate::platform::ProcMem {
                catalog_bytes: 1_048_576,
                rss_bytes: 1_048_576,
            })
        }
        fn gpu(&mut self) -> GpuReading {
            GpuReading::Unsupported("test".into())
        }
    }

    #[test]
    fn without_the_readers_word_the_process_table_decides_gone_or_unreadable() {
        // This test process is alive, under its real start time; the other
        // pid exists nowhere. Both reads fail.
        let me = std::process::id();
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let start = sys
            .process(sysinfo::Pid::from_u32(me))
            .expect("this process")
            .start_time();
        let absent = u32::MAX - 11;
        let mut t = BTreeMap::new();
        t.insert(
            absent,
            ProcInfo {
                start_time: 7,
                ..p(Some(me), "gone.exe", &[])
            },
        );
        let run = |failing: Vec<u32>| {
            let mut s = Sampler::new(
                SamplerConfig {
                    host_pids: vec![me, absent],
                    ..SamplerConfig::default()
                },
                Box::new(TableDecides { failing }),
            );
            let mut table = t.clone();
            table.insert(
                me,
                ProcInfo {
                    start_time: start,
                    ..p(None, "probe", &[])
                },
            );
            s.sample_table(&table, 0)
        };
        // A pid absent from a fresh table: exited, and the group is whole.
        let rec = run(vec![absent]);
        let host = &rec.groups[GROUP_HOST];
        assert_eq!(host.note, None, "{host:?}");
        assert_eq!(host.pids, vec![me]);
        assert_eq!(
            rec.exited.iter().map(|e| e.pid).collect::<Vec<_>>(),
            vec![absent]
        );
        // A pid still present whose read fails: unreadable, not exited.
        let rec = run(vec![me]);
        let host = &rec.groups[GROUP_HOST];
        assert_eq!(host.note, Some(NOTE_UNREADABLE), "{host:?}");
        assert!(rec.exited.is_empty(), "{:?}", rec.exited);
    }

    #[test]
    fn a_thread_is_not_a_process_member() {
        assert!(is_process(None));
        assert!(is_process(Some(ThreadKind::Kernel)));
        assert!(!is_process(Some(ThreadKind::Userland)));
    }

    /// On Linux the real table: this process's own threads (it has at least
    /// the one spawned here) must not appear as processes beside it.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_process_table_lists_no_threads() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let _ = rx.recv();
        });
        let me = std::process::id();
        let tids: Vec<u32> = std::fs::read_dir(format!("/proc/{me}/task"))
            .expect("/proc/self/task")
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .filter(|&t| t != me)
            .collect();
        assert!(!tids.is_empty(), "the spawned thread has a tid");
        let mut s = Sampler::new(
            SamplerConfig::default(),
            Box::new(FakeReader {
                cpu: HashMap::new(),
                unreadable: None,
                exited_mid_read: None,
            }),
        );
        let table = s.table();
        assert!(table.contains_key(&me), "this process is listed");
        for t in &tids {
            assert!(
                !table.contains_key(t),
                "thread {t} of {me} is listed as a process"
            );
        }
        let _ = tx.send(());
        worker.join().expect("worker");
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
                exited_mid_read: None,
            }),
        );
        let host = &s.sample(0).groups[GROUP_HOST];
        assert_eq!(host.note, Some(NOTE_UNREADABLE));
        assert_eq!(host.memory_mb, None);
        assert!(host.unreadable > 0);
    }
}
