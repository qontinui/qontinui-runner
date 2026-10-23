//! Workload class census (plan
//! `2026-10-01-fleet-capacity-page-finds-the-bottleneck-and-the-idle-hardware`,
//! Decision 2 and Phase 2).
//!
//! The resource sample says how much CPU and memory a machine is spending; this
//! says **on what**. Every process on the lane is put into exactly one class of
//! a small, closed, versioned vocabulary ([`WorkloadClass`]), and the sample
//! carries one aggregate per non-empty class — `{class, cpu_pct, rss_bytes,
//! gpu_mem_bytes, proc_count}`, the shape coord stores in
//! `coord.device_resource_samples.workloads`.
//!
//! ## What never leaves the host
//!
//! Command lines on this fleet carry tokens — a staged bearer header was once
//! published from a tempfile path — so **argv, environment and cwd are never
//! read by this module at all**, not merely filtered out of the output. The
//! classifier's input is [`ProcView`]: an executable name, the executable names
//! of its ancestors, and (Linux) the systemd unit it runs in. That type has no
//! field an argument could be put into, so "argv leaked into a class" is not a
//! bug this module can have; the `sysinfo` refresh is built from
//! `ProcessRefreshKind::new()` plus CPU and memory only, so `cmd`, `environ`,
//! `cwd` and `exe` are never loaded into the process table either. The WSL guest
//! census reads `/proc/<pid>/stat` (whose only string is the kernel's `comm`)
//! and `/proc/<pid>/cgroup`, never `cmdline` or `environ`.
//!
//! The only strings that leave the host are the class names below.
//!
//! ## UNKNOWN is a value
//!
//! * A CPU share needs two observations. The first tick after start (or after
//!   the census is switched back on) has none, so every entry's `cpu_pct` is
//!   `null` for that tick — never `0`, which would render a saturated box idle.
//! * `gpu_mem_bytes` is `null` unless the caller hands in a per-process VRAM
//!   measurement; no measurement is UNKNOWN, not zero VRAM.
//! * A census that is switched off is no census: the sample publishes no
//!   `workloads` at all (coord stores NULL), never an empty array, which would
//!   read as "measured, and nothing is running".
//!
//! ## What a CPU share means
//!
//! `cpu_pct` is the class's share of the **whole lane's** CPU capacity over
//! the tick, 0–100 — the classes of one sample therefore sum to at most 100
//! and compare directly with the sample's own `cpu_busy_permille / 10`. The
//! gap between the two is load the census could not attribute: processes that
//! started AND exited between two ticks, and (host lane) the first interval of a
//! process `sysinfo` saw for the first time, which it reports as zero. `other`
//! is always reported when any process falls into it, so load that IS observed
//! but unexplained is never hidden.
//!
//! `rss_bytes` sums resident set sizes, so pages shared between processes of a
//! class (or across classes) are counted once per process. It is a pressure
//! indicator, not an exact partition of physical memory.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use serde::Serialize;

/// Version of the [`WorkloadClass`] vocabulary. Bump it whenever a class is
/// added, removed or renamed — coord drops classes it does not know, so a
/// consumer comparing two windows needs to know which vocabulary each used.
pub(crate) const WORKLOAD_CLASS_VOCAB_VERSION: u32 = 1;

/// Upper bound on entries in one lane's census — coord truncates anything
/// longer. Holds by construction (one entry per class); the assertion below
/// turns a vocabulary that outgrows it into a compile error.
pub(crate) const MAX_WORKLOAD_ENTRIES: usize = 16;

const _: () = assert!(WorkloadClass::ALL.len() <= MAX_WORKLOAD_ENTRIES);

/// Kill switch: `QONTINUI_WORKLOAD_CENSUS=0` stops the census on this machine.
const KILL_SWITCH_ENV: &str = "QONTINUI_WORKLOAD_CENSUS";

/// Deepest ancestor chain walked for one process. Real trees are a dozen deep;
/// the bound exists so a `ppid` cycle in a racy snapshot cannot loop forever.
const MAX_CHAIN_DEPTH: usize = 64;

/// How many recent host-census durations the p95 is computed over.
const DURATION_WINDOW: usize = 64;

/// The closed workload vocabulary, version [`WORKLOAD_CLASS_VOCAB_VERSION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum WorkloadClass {
    /// A Claude Code session and everything it runs that has no class of its
    /// own (shells, node tooling, test runners).
    ClaudeSession,
    /// A GitHub Actions runner that is executing a job, and everything the job
    /// runs — a `cargo` under a job is CI load, not a local build.
    GhActionsJob,
    /// A GitHub Actions runner service with no job: listener overhead only.
    GhActionsRunnerIdle,
    /// `cargo`, `rustc` and everything a build spawns (linkers, build scripts).
    CargoBuild,
    /// The Qontinui runner process and its unclassified children.
    QontinuiRunner,
    /// The dev supervisor.
    Supervisor,
    /// Docker / containerd daemons, shims and container processes.
    Docker,
    /// The WSL VM as the Windows host sees it (`vmmem`, `wsl*.exe`). The `wsl`
    /// lane breaks the same load down from inside; never sum the two.
    WslVm,
    /// Local model servers (llama.cpp, ollama), identified by executable name.
    GpuInference,
    /// Web browsers, including automation-driven headless ones.
    Browser,
    /// Everything else. Always reported when non-empty.
    Other,
}

impl WorkloadClass {
    /// Every class, in the order a census lists them.
    pub(crate) const ALL: [WorkloadClass; 11] = [
        WorkloadClass::ClaudeSession,
        WorkloadClass::GhActionsJob,
        WorkloadClass::GhActionsRunnerIdle,
        WorkloadClass::CargoBuild,
        WorkloadClass::QontinuiRunner,
        WorkloadClass::Supervisor,
        WorkloadClass::Docker,
        WorkloadClass::WslVm,
        WorkloadClass::GpuInference,
        WorkloadClass::Browser,
        WorkloadClass::Other,
    ];

    /// The wire name — the one home of the vocabulary's spelling.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            WorkloadClass::ClaudeSession => "claude_session",
            WorkloadClass::GhActionsJob => "gh_actions_job",
            WorkloadClass::GhActionsRunnerIdle => "gh_actions_runner_idle",
            WorkloadClass::CargoBuild => "cargo_build",
            WorkloadClass::QontinuiRunner => "qontinui_runner",
            WorkloadClass::Supervisor => "supervisor",
            WorkloadClass::Docker => "docker",
            WorkloadClass::WslVm => "wsl_vm",
            WorkloadClass::GpuInference => "gpu_inference",
            WorkloadClass::Browser => "browser",
            WorkloadClass::Other => "other",
        }
    }
}

/// One class's aggregate in a sample. Field names are coord's storage
/// interface (`coord.device_resource_samples.workloads`) — do not rename.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct WorkloadEntry {
    pub(crate) class: &'static str,
    /// Share of the lane's total CPU capacity, 0–100. `null` on a tick with no
    /// previous observation to diff against.
    pub(crate) cpu_pct: Option<f64>,
    pub(crate) rss_bytes: u64,
    /// Per-process VRAM summed over the class. `null` = not measured.
    pub(crate) gpu_mem_bytes: Option<u64>,
    pub(crate) proc_count: u32,
}

/// Whether this machine should take a census this tick: the local kill switch
/// is not set AND the tenant's `workload_census_enabled` control is not `false`.
pub(crate) fn census_enabled() -> bool {
    !kill_switch_engaged(std::env::var(KILL_SWITCH_ENV).ok().as_deref())
        && crate::mcp::fleet_policy_poller::fleet_workload_census_enabled()
}

/// Exactly `0` (whitespace-trimmed) engages the kill switch; unset or any other
/// value leaves the census on — it ships enabled.
fn kill_switch_engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "0")
}

// ---------------------------------------------------------------------------
// Classifier — pure
// ---------------------------------------------------------------------------

/// The classifier's whole input for one process. There is deliberately no
/// field for arguments, environment or working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcView<'a> {
    /// Normalized executable name ([`normalize_exe_name`]).
    pub(crate) exe: &'a str,
    /// Normalized executable names of the ancestors, nearest first.
    pub(crate) parent_chain: Vec<&'a str>,
    /// The systemd unit (`*.service` / `*.scope`) the process runs in, Linux only.
    pub(crate) unit: Option<&'a str>,
    /// Whether the GitHub Actions runner this process belongs to — by unit, or
    /// by its nearest `Runner.Listener` ancestor — currently has a
    /// `Runner.Worker`, i.e. is running a job. Derived from the same process
    /// table by [`census`]; meaningless (and ignored) outside a runner.
    pub(crate) ci_worker_present: bool,
}

const ACTIONS_WORKER: &str = "runner.worker";
const ACTIONS_LISTENER: &str = "runner.listener";

/// Lowercase, without a trailing `.exe` — so `Runner.Worker.exe` on Windows and
/// `Runner.Worker` on Linux are one name.
pub(crate) fn normalize_exe_name(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.strip_suffix(".exe") {
        Some(stem) => stem.to_string(),
        None => lower,
    }
}

/// A GitHub Actions runner's systemd unit: `actions.runner.<scope>.<name>.service`.
fn is_actions_runner_unit(unit: &str) -> bool {
    unit.starts_with("actions.runner.") && unit.ends_with(".service")
}

/// The class an executable name identifies on its own, if any.
///
/// Linux reports the kernel's `comm`, truncated to 15 bytes — hence prefixes
/// such as `qontinui-superv` and `containerd-shim`.
fn class_of_exe(exe: &str) -> Option<WorkloadClass> {
    use WorkloadClass::*;
    let class = match exe {
        "claude" => ClaudeSession,
        "cargo" | "rustc" | "rustdoc" | "clippy-driver" | "sccache" => CargoBuild,
        e if e.starts_with("cargo-") => CargoBuild,
        "qontinui-runner" => QontinuiRunner,
        e if e.starts_with("qontinui-superv") => Supervisor,
        "dockerd" | "docker" | "rootlesskit" | "slirp4netns" | "vpnkit" => Docker,
        e if e.starts_with("containerd")
            || e.starts_with("com.docker.")
            || e.starts_with("docker-")
            || e.starts_with("docker desktop") =>
        {
            Docker
        }
        "vmmem" | "vmmemwsl" | "wsl" | "wslhost" | "wslservice" | "wslrelay" => WslVm,
        "llama-server" | "llama-swap" | "llama-cli" => GpuInference,
        e if e.starts_with("ollama") => GpuInference,
        "chrome" | "chromium" | "chromium-browser" | "google-chrome" | "google chrome"
        | "headless_shell" | "firefox" | "firefox-bin" | "firefox-esr" | "msedge" | "brave"
        | "brave browser" | "safari" | "opera" | "vivaldi" => Browser,
        e if e.starts_with("chrome_") => Browser,
        _ => return None,
    };
    Some(class)
}

/// The class a systemd unit implies for a process no name in its chain
/// classified — typically a daemon re-parented to `init` inside its service.
fn class_of_unit(unit: &str) -> Option<WorkloadClass> {
    use WorkloadClass::*;
    if unit == "docker.service"
        || unit == "containerd.service"
        || (unit.starts_with("docker-") && unit.ends_with(".scope"))
    {
        return Some(Docker);
    }
    if unit == "qontinui-runner.service" {
        return Some(QontinuiRunner);
    }
    if unit.starts_with("qontinui-supervisor") {
        return Some(Supervisor);
    }
    None
}

/// Classify one process. PURE.
///
/// Precedence, and why:
/// 1. **CI owns everything inside it.** A process in an
///    `actions.runner.*.service` unit, or under a `Runner.Listener` /
///    `Runner.Worker`, is `gh_actions_job` when that runner has a worker and
///    `gh_actions_runner_idle` when it does not — the build a job runs is CI
///    load, which is the question the capacity page asks of it.
/// 2. **A name that identifies itself** (`claude`, `cargo`, `dockerd`, …).
/// 3. **The nearest classified ancestor** — a shell a Claude session runs is
///    that session's load; a linker under `cargo` is the build's.
/// 4. **The systemd unit**, for processes whose chain names nothing.
/// 5. `other`.
pub(crate) fn classify(p: &ProcView<'_>) -> WorkloadClass {
    let lineage = || std::iter::once(p.exe).chain(p.parent_chain.iter().copied());
    let in_runner_unit = p.unit.is_some_and(is_actions_runner_unit);
    let under_worker = lineage().any(|n| n == ACTIONS_WORKER);
    let under_listener = lineage().any(|n| n == ACTIONS_LISTENER);
    if under_worker || ((in_runner_unit || under_listener) && p.ci_worker_present) {
        return WorkloadClass::GhActionsJob;
    }
    if in_runner_unit || under_listener {
        return WorkloadClass::GhActionsRunnerIdle;
    }
    lineage()
        .find_map(class_of_exe)
        .or_else(|| p.unit.and_then(class_of_unit))
        .unwrap_or(WorkloadClass::Other)
}

// ---------------------------------------------------------------------------
// Census — pure aggregation over a process table
// ---------------------------------------------------------------------------

/// One row of a process table, as either lane's collector reads it. Same rule
/// as [`ProcView`]: nothing here can hold an argument.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProcRecord {
    pub(crate) pid: u32,
    pub(crate) ppid: Option<u32>,
    /// Normalized executable name ([`normalize_exe_name`]).
    pub(crate) name: String,
    pub(crate) unit: Option<String>,
    /// This process's share of the lane's CPU capacity over the tick, 0–100.
    /// Ignored when the census is told the tick has no CPU delta.
    pub(crate) cpu_pct: f64,
    pub(crate) rss_bytes: u64,
}

/// Aggregate a process table into one entry per non-empty class. PURE.
///
/// `cpu_known = false` publishes `cpu_pct: null` on every entry (first tick).
/// `vram_by_pid` is a complete per-process VRAM measurement, or `None` when
/// none was taken — then every entry's `gpu_mem_bytes` is `null`.
pub(crate) fn census(
    records: &[ProcRecord],
    cpu_known: bool,
    vram_by_pid: Option<&HashMap<u32, u64>>,
) -> Vec<WorkloadEntry> {
    let by_pid: HashMap<u32, &ProcRecord> = records.iter().map(|r| (r.pid, r)).collect();

    // Which runners have a job: by unit, and by listener (the worker is the
    // listener's child), so both the Linux cgroup path and the name-only path
    // (Windows, macOS, a runner started by hand) agree.
    let mut busy_units: HashSet<&str> = HashSet::new();
    let mut busy_listeners: HashSet<u32> = HashSet::new();
    for r in records.iter().filter(|r| r.name == ACTIONS_WORKER) {
        if let Some(unit) = r.unit.as_deref().filter(|u| is_actions_runner_unit(u)) {
            busy_units.insert(unit);
        }
        if let Some(parent) = r.ppid.and_then(|pp| by_pid.get(&pp)) {
            if parent.name == ACTIONS_LISTENER {
                busy_listeners.insert(parent.pid);
            }
        }
    }

    #[derive(Default)]
    struct Acc {
        cpu: f64,
        rss: u64,
        vram: u64,
        procs: u32,
    }
    let mut acc: HashMap<WorkloadClass, Acc> = HashMap::new();

    for r in records {
        let mut chain: Vec<&str> = Vec::new();
        let mut listener: Option<u32> = (r.name == ACTIONS_LISTENER).then_some(r.pid);
        let mut seen: HashSet<u32> = HashSet::from([r.pid]);
        let mut cursor = r.ppid;
        while let Some(pp) = cursor {
            if chain.len() >= MAX_CHAIN_DEPTH || !seen.insert(pp) {
                break;
            }
            let Some(parent) = by_pid.get(&pp) else {
                break;
            };
            if listener.is_none() && parent.name == ACTIONS_LISTENER {
                listener = Some(parent.pid);
            }
            chain.push(parent.name.as_str());
            cursor = parent.ppid;
        }
        let unit = r.unit.as_deref();
        let view = ProcView {
            exe: r.name.as_str(),
            parent_chain: chain,
            unit,
            ci_worker_present: unit.is_some_and(|u| busy_units.contains(u))
                || listener.is_some_and(|l| busy_listeners.contains(&l)),
        };
        let a = acc.entry(classify(&view)).or_default();
        a.cpu += r.cpu_pct;
        a.rss = a.rss.saturating_add(r.rss_bytes);
        a.procs = a.procs.saturating_add(1);
        if let Some(v) = vram_by_pid.and_then(|m| m.get(&r.pid)) {
            a.vram = a.vram.saturating_add(*v);
        }
    }

    WorkloadClass::ALL
        .iter()
        .filter_map(|class| {
            let a = acc.get(class)?;
            Some(WorkloadEntry {
                class: class.as_str(),
                cpu_pct: cpu_known.then(|| (a.cpu.clamp(0.0, 100.0) * 100.0).round() / 100.0),
                rss_bytes: a.rss,
                gpu_mem_bytes: vram_by_pid.map(|_| a.vram),
                proc_count: a.procs,
            })
        })
        .collect()
}

/// The systemd unit a `/proc/<pid>/cgroup` body places a process in: the
/// innermost `*.service` / `*.scope` segment of the unified (v2) hierarchy, or
/// of the v1 `name=systemd` one. `None` when neither names a unit.
pub(crate) fn unit_from_cgroup(text: &str) -> Option<String> {
    let path = text
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .or_else(|| {
            text.lines()
                .find_map(|l| l.split_once(":name=systemd:").map(|(_, p)| p))
        })?;
    path.trim()
        .rsplit('/')
        .find(|seg| seg.ends_with(".service") || seg.ends_with(".scope"))
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Host lane — one long-lived sysinfo table
// ---------------------------------------------------------------------------

/// The host lane's census state: ONE `sysinfo::System` kept across ticks, so
/// the 30 s sample interval itself is the CPU diff window.
pub(crate) struct HostCensus {
    sys: sysinfo::System,
    /// Whether `sys` has been refreshed before — the first refresh has no CPU
    /// delta.
    primed: bool,
    cpu_count: usize,
    /// Linux: the unit each live process runs in, keyed by pid and start time
    /// so a reused pid is re-read. Units do not change under a process, so
    /// each `/proc/<pid>/cgroup` is read once rather than every tick.
    #[cfg(target_os = "linux")]
    units: HashMap<u32, (u64, Option<String>)>,
    durations: VecDeque<Duration>,
}

impl HostCensus {
    pub(crate) fn new() -> Self {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_list(sysinfo::CpuRefreshKind::new());
        let cpu_count = sys.cpus().len().max(1);
        Self {
            sys,
            primed: false,
            cpu_count,
            #[cfg(target_os = "linux")]
            units: HashMap::new(),
            durations: VecDeque::with_capacity(DURATION_WINDOW),
        }
    }

    /// Refresh the table and aggregate it. `vram_by_pid` is the seam for a
    /// per-process VRAM measurement; `None` publishes `gpu_mem_bytes: null`.
    pub(crate) fn take(&mut self, vram_by_pid: Option<&HashMap<u32, u64>>) -> Vec<WorkloadEntry> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, ThreadKind};

        let started = std::time::Instant::now();
        // CPU and memory only: `cmd`, `environ`, `cwd` and `exe` stay at
        // `UpdateKind::Never`, so they are never even read into the table.
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_cpu().with_memory(),
        );
        let cpu_known = self.primed;
        self.primed = true;

        let cpu_count = self.cpu_count as f64;
        let rows: Vec<(ProcRecord, u64)> = self
            .sys
            .processes()
            .values()
            // Linux lists every thread as a pseudo-process; the thread-group
            // leader's counters already include them.
            .filter(|p| p.thread_kind() != Some(ThreadKind::Userland))
            .map(|p| {
                let record = ProcRecord {
                    pid: p.pid().as_u32(),
                    ppid: p.parent().map(|pp| pp.as_u32()),
                    name: normalize_exe_name(&p.name().to_string_lossy()),
                    unit: None,
                    cpu_pct: f64::from(p.cpu_usage()) / cpu_count,
                    rss_bytes: p.memory(),
                };
                (record, p.start_time())
            })
            .collect();
        let records: Vec<ProcRecord> = rows
            .into_iter()
            .map(|(mut r, start_time)| {
                r.unit = self.unit_of(r.pid, start_time);
                r
            })
            .collect();
        #[cfg(target_os = "linux")]
        {
            let live: HashSet<u32> = records.iter().map(|r| r.pid).collect();
            self.units.retain(|pid, _| live.contains(pid));
        }

        let entries = census(&records, cpu_known, vram_by_pid);
        self.record_duration(started.elapsed(), records.len());
        entries
    }

    #[cfg(target_os = "linux")]
    fn unit_of(&mut self, pid: u32, start_time: u64) -> Option<String> {
        if let Some((start, unit)) = self.units.get(&pid) {
            if *start == start_time {
                return unit.clone();
            }
        }
        let unit = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .ok()
            .and_then(|t| unit_from_cgroup(&t));
        self.units.insert(pid, (start_time, unit.clone()));
        unit
    }

    #[cfg(not(target_os = "linux"))]
    fn unit_of(&mut self, _pid: u32, _start_time: u64) -> Option<String> {
        None
    }

    /// Keep the recent census durations and log this tick's with the window
    /// p95 — the census's budget is ≤ 50 ms p95.
    fn record_duration(&mut self, took: Duration, procs: usize) {
        if self.durations.len() == DURATION_WINDOW {
            self.durations.pop_front();
        }
        self.durations.push_back(took);
        tracing::debug!(
            "fleet::workload_census: host census of {procs} processes took {} ms (p95 {} ms over {} ticks)",
            took.as_millis(),
            p95(&self.durations).as_millis(),
            self.durations.len()
        );
    }
}

fn p95(window: &VecDeque<Duration>) -> Duration {
    let mut v: Vec<Duration> = window.iter().copied().collect();
    v.sort_unstable();
    let idx = (v.len() * 95).div_ceil(100).saturating_sub(1);
    v.get(idx).copied().unwrap_or_default()
}

/// The host census, or `None` when it is switched off. Holds the long-lived
/// state; a census switched off drops it, so switching back on starts with a
/// `null`-CPU tick rather than a CPU figure averaged over the dark period.
pub(crate) fn host_workloads() -> Option<Vec<WorkloadEntry>> {
    static STATE: std::sync::Mutex<Option<HostCensus>> = std::sync::Mutex::new(None);
    let mut slot = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if !census_enabled() {
        *slot = None;
        return None;
    }
    Some(slot.get_or_insert_with(HostCensus::new).take(None))
}

// ---------------------------------------------------------------------------
// WSL guest — rides the lane's single `wsl.exe` probe
// ---------------------------------------------------------------------------

/// Separates the probe's original procfs output from the census section.
const GUEST_MARKER: &str = "@@qontinui-workload-census@@";
/// Separates the census's `stat` lines from its `cgroup` files.
const GUEST_CGROUP_MARKER: &str = "@@qontinui-workload-census-cgroup@@";

/// Shell words appended to the `wsl` lane's one probe when the census is on.
///
/// `wsl.exe -d <distro> -- …` runs its arguments through the distro's default
/// shell, so `;`, the globs and the redirections are interpreted in the guest.
/// The section emits the page size, the aggregate `cpu` line of `/proc/stat`,
/// every `/proc/<pid>/stat` (each line starts with its own pid, and its only
/// string is the kernel's `comm`) and every `/proc/<pid>/cgroup` behind a
/// `tail` header naming its pid. Never `cmdline` and never `environ`. On a
/// guest where this section does not run, its marker is missing and the lane
/// publishes no census — UNKNOWN, never empty.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const GUEST_PROBE_WORDS: &[&str] = &[
    ";",
    "echo",
    GUEST_MARKER,
    ";",
    "getconf",
    "PAGESIZE",
    ";",
    "head",
    "-n",
    "1",
    "/proc/stat",
    ";",
    "cat",
    "/proc/[0-9]*/stat",
    "2>/dev/null",
    ";",
    "echo",
    GUEST_CGROUP_MARKER,
    ";",
    "tail",
    "-n",
    "+1",
    "/proc/[0-9]*/cgroup",
    "2>/dev/null",
];

/// Split the probe's stdout into the original procfs part (which the lane's
/// memory and saturation parsers read exactly as before) and the census
/// section, if one was emitted.
pub(crate) fn split_guest_output(text: &str) -> (&str, Option<&str>) {
    match text.split_once(GUEST_MARKER) {
        Some((head, census)) => (head, Some(census)),
        None => (text, None),
    }
}

/// One guest process as `/proc/<pid>/stat` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GuestStat {
    pid: u32,
    ppid: u32,
    comm: String,
    start_time: u64,
    cpu_ticks: u64,
    rss_pages: u64,
}

/// Parse one `/proc/<pid>/stat` line. `comm` may contain spaces and
/// parentheses, so it is the text between the FIRST `(` and the LAST `)`.
fn parse_guest_stat(line: &str) -> Option<GuestStat> {
    let (pid, rest) = line.split_once(" (")?;
    let (comm, fields) = rest.rsplit_once(") ")?;
    let f: Vec<&str> = fields.split_whitespace().collect();
    // proc(5) field numbers minus 3: state is field 3, ppid 4, utime 14,
    // stime 15, starttime 22, rss 24.
    let num = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok());
    Some(GuestStat {
        pid: pid.trim().parse().ok()?,
        ppid: u32::try_from(num(1)?).ok()?,
        comm: comm.to_string(),
        cpu_ticks: num(11)?.saturating_add(num(12)?),
        start_time: num(19)?,
        rss_pages: num(21)?,
    })
}

/// Total jiffies across all CPUs from `/proc/stat`'s aggregate `cpu` line
/// (user through steal; guest time is already inside user).
fn parse_total_jiffies(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("cpu ")?;
    let v: Vec<u64> = rest
        .split_whitespace()
        .take(8)
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    (v.len() == 8).then(|| v.iter().sum())
}

/// `tail -n +1` output → pid → unit. Each file follows a
/// `==> /proc/<pid>/cgroup <==` header.
fn parse_guest_cgroups(text: &str) -> HashMap<u32, String> {
    let mut blocks: Vec<(Option<u32>, String)> = Vec::new();
    for line in text.lines() {
        if let Some(header) = line
            .strip_prefix("==> /proc/")
            .and_then(|r| r.strip_suffix("/cgroup <=="))
        {
            blocks.push((header.parse().ok(), String::new()));
        } else if let Some((_, body)) = blocks.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    blocks
        .into_iter()
        .filter_map(|(pid, body)| Some((pid?, unit_from_cgroup(&body)?)))
        .collect()
}

/// The `wsl` lane's census state: the previous tick's per-process CPU ticks,
/// keyed by pid and start time, and the guest's total jiffies.
#[derive(Debug, Default)]
pub(crate) struct GuestCensus {
    prev: Option<(u64, HashMap<(u32, u64), u64>)>,
}

impl GuestCensus {
    /// Aggregate one census section. `None` when the section is unusable (no
    /// page size, no `cpu` line, no process) — the lane then publishes no
    /// census, never a fabricated one.
    pub(crate) fn take(&mut self, section: &str) -> Option<Vec<WorkloadEntry>> {
        let (stats_part, cgroup_part) = section
            .split_once(GUEST_CGROUP_MARKER)
            .unwrap_or((section, ""));
        let mut lines = stats_part.lines().map(str::trim).filter(|l| !l.is_empty());
        let page_size: u64 = lines.next()?.parse().ok()?;
        let total = parse_total_jiffies(lines.next()?)?;
        let stats: Vec<GuestStat> = lines.filter_map(parse_guest_stat).collect();
        if stats.is_empty() {
            return None;
        }
        let units = parse_guest_cgroups(cgroup_part);

        let ticks: HashMap<(u32, u64), u64> = stats
            .iter()
            .map(|s| ((s.pid, s.start_time), s.cpu_ticks))
            .collect();
        // A process absent from the previous tick started inside this interval,
        // so all of its ticks belong to it.
        let interval = self
            .prev
            .as_ref()
            .and_then(|(prev_total, prev)| {
                let span = total.checked_sub(*prev_total).filter(|s| *s > 0)?;
                Some((span as f64, prev))
            });
        let cpu_known = interval.is_some();
        let records: Vec<ProcRecord> = stats
            .iter()
            .map(|s| ProcRecord {
                pid: s.pid,
                ppid: (s.ppid != 0).then_some(s.ppid),
                name: normalize_exe_name(&s.comm),
                unit: units.get(&s.pid).cloned(),
                cpu_pct: interval.map_or(0.0, |(span, prev)| {
                    let before = prev.get(&(s.pid, s.start_time)).copied().unwrap_or(0);
                    s.cpu_ticks.saturating_sub(before) as f64 / span * 100.0
                }),
                rss_bytes: s.rss_pages.saturating_mul(page_size),
            })
            .collect();
        self.prev = Some((total, ticks));
        Some(census(&records, cpu_known, None))
    }
}

/// The `wsl` lane's census from its probe's census section, or `None` when the
/// census is off or the section is missing / unusable.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn guest_workloads(section: Option<&str>) -> Option<Vec<WorkloadEntry>> {
    static STATE: std::sync::Mutex<Option<GuestCensus>> = std::sync::Mutex::new(None);
    let mut slot = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(section) = section.filter(|_| census_enabled()) else {
        *slot = None;
        return None;
    };
    slot.get_or_insert_with(GuestCensus::default).take(section)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(exe: &'a str, chain: &[&'a str], unit: Option<&'a str>) -> ProcView<'a> {
        ProcView {
            exe,
            parent_chain: chain.to_vec(),
            unit,
            ci_worker_present: false,
        }
    }

    fn rec(pid: u32, ppid: u32, name: &str, unit: Option<&str>, cpu: f64, rss: u64) -> ProcRecord {
        ProcRecord {
            pid,
            ppid: (ppid != 0).then_some(ppid),
            name: normalize_exe_name(name),
            unit: unit.map(str::to_string),
            cpu_pct: cpu,
            rss_bytes: rss,
        }
    }

    #[test]
    fn the_vocabulary_is_exactly_coords() {
        let names: Vec<&str> = WorkloadClass::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "claude_session",
                "gh_actions_job",
                "gh_actions_runner_idle",
                "cargo_build",
                "qontinui_runner",
                "supervisor",
                "docker",
                "wsl_vm",
                "gpu_inference",
                "browser",
                "other",
            ]
        );
        assert_eq!(WORKLOAD_CLASS_VOCAB_VERSION, 1);
    }

    #[test]
    fn names_normalize_across_platforms() {
        assert_eq!(normalize_exe_name("Runner.Worker.exe"), "runner.worker");
        assert_eq!(normalize_exe_name("Runner.Worker"), "runner.worker");
        assert_eq!(normalize_exe_name("claude.exe"), "claude");
        assert_eq!(normalize_exe_name("Google Chrome"), "google chrome");
    }

    #[test]
    fn classifier_table() {
        use WorkloadClass::*;
        let cases: &[(&str, &[&str], Option<&str>, WorkloadClass)] = &[
            ("claude", &["qontinui-runner", "systemd"], None, ClaudeSession),
            ("bash", &["claude", "qontinui-runner"], None, ClaudeSession),
            ("cargo", &["bash", "claude"], None, CargoBuild),
            ("rustc", &["cargo", "bash"], None, CargoBuild),
            ("cc", &["build-script-bu", "cargo"], None, CargoBuild),
            ("ld", &["cc", "rustc", "cargo"], None, CargoBuild),
            ("cargo-nextest", &["bash"], None, CargoBuild),
            ("qontinui-runner", &["systemd"], None, QontinuiRunner),
            ("bash", &["qontinui-runner"], None, QontinuiRunner),
            ("qontinui-superv", &["systemd"], None, Supervisor),
            ("qontinui-runner", &["qontinui-superv"], None, QontinuiRunner),
            ("dockerd", &["systemd"], None, Docker),
            ("containerd-shim", &["systemd"], None, Docker),
            ("com.docker.backend", &["explorer"], None, Docker),
            ("postgres", &["containerd-shim"], None, Docker),
            ("vmmemwsl", &["system"], None, WslVm),
            ("wslhost", &["explorer"], None, WslVm),
            ("llama-server", &["containerd-shim"], None, GpuInference),
            ("ollama", &["systemd"], None, GpuInference),
            ("chrome", &["chrome", "explorer"], None, Browser),
            ("msedge", &["explorer"], None, Browser),
            ("headless_shell", &["node", "claude"], None, Browser),
            ("firefox", &["systemd"], None, Browser),
            ("python3", &["systemd"], None, Other),
            ("sshd", &["systemd"], Some("ssh.service"), Other),
            // Unit fallback: a daemon re-parented to init inside its service.
            ("sh", &["systemd"], Some("docker.service"), Docker),
            ("sh", &["systemd"], Some("qontinui-runner.service"), QontinuiRunner),
            // The chain wins over the unit: Claude sessions live in the
            // runner's service cgroup.
            ("bash", &["claude", "qontinui-runner"], Some("qontinui-runner.service"), ClaudeSession),
        ];
        for (exe, chain, unit, want) in cases {
            assert_eq!(
                classify(&view(exe, chain, *unit)),
                *want,
                "{exe} under {chain:?} in {unit:?}"
            );
        }
    }

    #[test]
    fn ci_owns_everything_inside_it() {
        use WorkloadClass::*;
        let unit = Some("actions.runner.qontinui-qontinui-coord.merytshost-1.service");
        // A worker anywhere in the lineage is a job.
        assert_eq!(classify(&view("runner.worker", &["runner.listener"], unit)), GhActionsJob);
        assert_eq!(
            classify(&view("cargo", &["bash", "runner.worker", "runner.listener"], None)),
            GhActionsJob,
            "a build a job runs is CI load, not a local build"
        );
        // Listener alone: idle, by unit or by name.
        assert_eq!(classify(&view("runner.listener", &["runsvc.sh"], unit)), GhActionsRunnerIdle);
        assert_eq!(classify(&view("runsvc.sh", &["systemd"], unit)), GhActionsRunnerIdle);
        assert_eq!(classify(&view("runner.listener", &["explorer"], None)), GhActionsRunnerIdle);
        // Same runner with a worker present: the whole unit is busy.
        let mut busy = view("runner.listener", &["runsvc.sh"], unit);
        busy.ci_worker_present = true;
        assert_eq!(classify(&busy), GhActionsJob);
        let mut reparented = view("cargo", &["systemd"], unit);
        reparented.ci_worker_present = true;
        assert_eq!(classify(&reparented), GhActionsJob);
    }

    #[test]
    fn census_derives_runner_busyness_from_the_table() {
        let busy_unit = "actions.runner.o-r.host-1.service";
        let idle_unit = "actions.runner.o-r.host-2.service";
        let table = vec![
            rec(1, 0, "systemd", None, 0.0, 1),
            rec(10, 1, "runsvc.sh", Some(busy_unit), 0.0, 1),
            rec(11, 10, "Runner.Listener", Some(busy_unit), 1.0, 1),
            rec(12, 11, "Runner.Worker", Some(busy_unit), 1.0, 1),
            rec(13, 12, "cargo", Some(busy_unit), 30.0, 1),
            rec(20, 1, "runsvc.sh", Some(idle_unit), 0.0, 1),
            rec(21, 20, "Runner.Listener", Some(idle_unit), 0.5, 1),
            // Windows/macOS shape: no unit, the listener's child decides.
            rec(30, 1, "Runner.Listener.exe", None, 0.5, 1),
            rec(31, 30, "Runner.Worker.exe", None, 2.0, 1),
            rec(40, 1, "Runner.Listener.exe", None, 0.5, 1),
        ];
        let out = census(&table, true, None);
        let get = |c: &str| out.iter().find(|e| e.class == c).cloned();
        let job = get("gh_actions_job").expect("job class");
        assert_eq!(job.proc_count, 6, "busy unit's 4 + the busy Windows pair");
        let idle = get("gh_actions_runner_idle").expect("idle class");
        assert_eq!(idle.proc_count, 3);
        assert_eq!(get("other").expect("systemd").proc_count, 1);
    }

    #[test]
    fn other_is_always_reported_with_its_share() {
        let table = vec![
            rec(1, 0, "init", None, 2.0, 100),
            rec(2, 1, "claude", None, 10.0, 1000),
            rec(3, 1, "mystery-daemon", None, 40.0, 5000),
        ];
        let out = census(&table, true, None);
        let other = out.iter().find(|e| e.class == "other").expect("other");
        assert_eq!(other.proc_count, 2);
        assert_eq!(other.cpu_pct, Some(42.0));
        assert_eq!(other.rss_bytes, 5100);
        let claude = out.iter().find(|e| e.class == "claude_session").expect("claude");
        assert_eq!(claude.cpu_pct, Some(10.0));
        // Empty classes are not listed at all.
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn the_first_tick_is_unknown_cpu_not_zero() {
        let table = vec![rec(1, 0, "claude", None, 0.0, 10)];
        let out = census(&table, false, None);
        assert_eq!(out[0].cpu_pct, None);
        let v = serde_json::to_value(&out).expect("serializes");
        assert!(v[0]["cpu_pct"].is_null(), "UNKNOWN is null on the wire, never 0");
    }

    #[test]
    fn the_wire_shape_is_coords() {
        let out = census(&[rec(1, 0, "cargo", None, 12.346, 7)], true, None);
        let v = serde_json::to_value(&out).expect("serializes");
        let obj = v[0].as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["class", "cpu_pct", "gpu_mem_bytes", "proc_count", "rss_bytes"]);
        assert_eq!(v[0]["class"], "cargo_build");
        assert_eq!(v[0]["cpu_pct"], 12.35);
        assert!(v[0]["gpu_mem_bytes"].is_null(), "no VRAM measurement is UNKNOWN");
    }

    #[test]
    fn a_vram_measurement_fills_every_class_including_zero() {
        let table = vec![
            rec(1, 0, "llama-server", None, 1.0, 1),
            rec(2, 0, "claude", None, 1.0, 1),
        ];
        let vram = HashMap::from([(1, 8_000_000_000u64)]);
        let out = census(&table, true, Some(&vram));
        let by = |c: &str| out.iter().find(|e| e.class == c).and_then(|e| e.gpu_mem_bytes);
        assert_eq!(by("gpu_inference"), Some(8_000_000_000));
        assert_eq!(by("claude_session"), Some(0), "measured, and none");
    }

    #[test]
    fn the_census_never_exceeds_sixteen_entries() {
        let names = [
            "claude", "Runner.Worker", "Runner.Listener", "cargo", "qontinui-runner",
            "qontinui-supervisor", "dockerd", "vmmem", "llama-server", "chrome",
            "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10",
        ];
        let table: Vec<ProcRecord> = (0..2000u32)
            .map(|i| rec(i + 1, 0, names[i as usize % names.len()], None, 0.01, 1))
            .collect();
        let out = census(&table, true, None);
        assert!(out.len() <= MAX_WORKLOAD_ENTRIES);
        assert_eq!(out.len(), WorkloadClass::ALL.len());
        let total: u32 = out.iter().map(|e| e.proc_count).sum();
        assert_eq!(total, 2000, "every process lands in exactly one class");
    }

    /// The census input has nowhere to put an argument, and the published JSON
    /// carries only class names and numbers. A guest probe whose `comm` and a
    /// would-be argv both carry a fake token proves neither reaches the wire.
    #[test]
    fn no_argument_or_token_reaches_the_wire() {
        const TOKEN: &str = "ghp_FAKEtoken0123456789abcdef";
        // What `ps -o args` WOULD show for these processes — the census has no
        // input that can carry it, so the only way to "pass" it is not at all.
        let _would_be_argv = format!("curl -H 'Authorization: Bearer {TOKEN}' https://x");
        let table = vec![
            rec(1, 0, "curl", None, 5.0, 10),
            rec(2, 1, "claude", Some("qontinui-runner.service"), 5.0, 10),
        ];
        let json = serde_json::to_string(&census(&table, true, None)).expect("serializes");
        assert!(!json.contains(TOKEN));
        assert!(!json.contains("curl"), "process names never leave the host either");

        // Guest path: a stat line whose comm is token-shaped.
        let section = format!(
            "\n4096\ncpu  100 0 100 800 0 0 0 0 0 0\n\
             7 ({TOKEN}) S 1 7 7 0 -1 0 0 0 0 0 5 5 0 0 20 0 1 0 100 1000 10\n\
             {GUEST_CGROUP_MARKER}\n==> /proc/7/cgroup <==\n0::/user.slice/{TOKEN}.scope\n"
        );
        let mut g = GuestCensus::default();
        let out = g.take(&section).expect("census");
        let json = serde_json::to_string(&out).expect("serializes");
        assert!(!json.contains(TOKEN), "{json}");
        assert!(!json.contains("ghp_"));
    }

    #[test]
    fn units_come_from_the_innermost_service_or_scope() {
        assert_eq!(
            unit_from_cgroup(
                "0::/ci.slice/ci-runners.slice/actions.runner.qontinui-qontinui-claude-config.merytshost-ccfg-4.service\n"
            )
            .as_deref(),
            Some("actions.runner.qontinui-qontinui-claude-config.merytshost-ccfg-4.service")
        );
        assert_eq!(
            unit_from_cgroup(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/qontinui-runner.service\n"
            )
            .as_deref(),
            Some("qontinui-runner.service")
        );
        assert_eq!(
            unit_from_cgroup("12:pids:/\n1:name=systemd:/system.slice/docker.service\n").as_deref(),
            Some("docker.service")
        );
        assert_eq!(unit_from_cgroup("0::/\n"), None);
    }

    #[test]
    fn guest_stat_lines_parse_comm_with_spaces_and_parens() {
        let s = parse_guest_stat("42 (tmux: server) (x)) S 1 42 42 0 -1 0 0 0 0 0 300 200 0 0 20 0 1 0 5555 100000 250")
            .expect("parses");
        assert_eq!(s.pid, 42);
        assert_eq!(s.ppid, 1);
        assert_eq!(s.comm, "tmux: server) (x)");
        assert_eq!(s.cpu_ticks, 500);
        assert_eq!(s.start_time, 5555);
        assert_eq!(s.rss_pages, 250);
        assert!(parse_guest_stat("garbage").is_none());
    }

    fn guest_section(total: [u64; 8], procs: &[(u32, u32, &str, u64, u64)], cgroups: &str) -> String {
        let cpu = total.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
        let mut s = format!("\n4096\ncpu  {cpu} 0 0\n");
        for (pid, ppid, comm, ticks, start) in procs {
            s.push_str(&format!(
                "{pid} ({comm}) S {ppid} 0 0 0 -1 0 0 0 0 0 {ticks} 0 0 0 20 0 1 0 {start} 0 10\n"
            ));
        }
        s.push_str(GUEST_CGROUP_MARKER);
        s.push('\n');
        s.push_str(cgroups);
        s
    }

    #[test]
    fn the_guest_census_is_unknown_cpu_then_a_measured_share() {
        let unit = "actions.runner.o-r.spaceship-1.service";
        let cg = format!(
            "==> /proc/2/cgroup <==\n0::/system.slice/{unit}\n\n==> /proc/3/cgroup <==\n0::/system.slice/{unit}\n\n==> /proc/4/cgroup <==\n0::/system.slice/{unit}\n"
        );
        let mut g = GuestCensus::default();
        let first = g
            .take(&guest_section(
                [0, 0, 0, 1000, 0, 0, 0, 0],
                &[(1, 0, "init", 0, 1), (2, 1, "Runner.Listener", 10, 2), (3, 2, "Runner.Worker", 0, 3)],
                &cg,
            ))
            .expect("first tick");
        assert!(first.iter().all(|e| e.cpu_pct.is_none()), "{first:?}");

        // 1000 jiffies later: the worker and a new rustc (started this
        // interval, so all its ticks count) burn 400 + 100 of them.
        let second = g
            .take(&guest_section(
                [500, 0, 0, 1500, 0, 0, 0, 0],
                &[
                    (1, 0, "init", 0, 1),
                    (2, 1, "Runner.Listener", 10, 2),
                    (3, 2, "Runner.Worker", 400, 3),
                    (4, 3, "rustc", 100, 900),
                ],
                &cg,
            ))
            .expect("second tick");
        let job = second.iter().find(|e| e.class == "gh_actions_job").expect("job");
        assert_eq!(job.cpu_pct, Some(50.0));
        assert_eq!(job.proc_count, 3, "listener, worker and the rustc under it");
        assert_eq!(job.rss_bytes, 3 * 10 * 4096);
        let other = second.iter().find(|e| e.class == "other").expect("init");
        assert_eq!(other.cpu_pct, Some(0.0));
    }

    #[test]
    fn a_missing_or_unusable_guest_section_is_no_census() {
        let (head, census) = split_guest_output("MemTotal: 1 kB\n");
        assert_eq!(head, "MemTotal: 1 kB\n");
        assert!(census.is_none());

        let probe = format!("MemTotal: 1 kB\n{GUEST_MARKER}\n4096\ncpu  1 2 3 4 5 6 7 8\n");
        let (head, census) = split_guest_output(&probe);
        assert_eq!(head, "MemTotal: 1 kB\n");
        assert!(
            GuestCensus::default().take(census.expect("section")).is_none(),
            "a section with no process is UNKNOWN, never an empty census"
        );
    }

    #[test]
    fn the_guest_probe_never_reads_argv_or_environment() {
        for w in GUEST_PROBE_WORDS {
            assert!(!w.contains("cmdline") && !w.contains("environ") && !w.contains("cwd"), "{w}");
        }
    }

    /// The module must never load argv, environment or cwd. Structural: the
    /// production half of this file names none of the accessors.
    #[test]
    fn production_code_never_touches_argv_env_or_cwd() {
        const SRC: &str = include_str!("workload_census.rs");
        let prod = SRC.split_once("\n#[cfg(test)]").map_or(SRC, |(a, _)| a);
        for forbidden in [".cmd()", ".environ()", ".cwd()", ".exe()", "with_cmd", "with_environ", "with_cwd", "with_exe"] {
            assert!(!prod.contains(forbidden), "production code must not call {forbidden}");
        }
    }

    #[test]
    fn only_an_explicit_zero_engages_the_kill_switch() {
        assert!(kill_switch_engaged(Some("0")));
        assert!(kill_switch_engaged(Some(" 0\n")));
        assert!(!kill_switch_engaged(None));
        assert!(!kill_switch_engaged(Some("1")));
        assert!(!kill_switch_engaged(Some("false")));
        assert!(!kill_switch_engaged(Some("")));
    }

    #[test]
    fn p95_of_a_window() {
        let w: VecDeque<Duration> = (1..=100).map(Duration::from_millis).collect();
        assert_eq!(p95(&w), Duration::from_millis(95));
        assert_eq!(p95(&VecDeque::new()), Duration::ZERO);
    }

    /// Live: a census of THIS machine — two ticks, as the sampler takes them.
    /// Asserts only the invariants that hold on any box; the timing is printed
    /// (`--nocapture`) as the budget measurement.
    #[test]
    fn a_live_host_census_holds_its_invariants() {
        let mut h = HostCensus::new();
        let t0 = std::time::Instant::now();
        let first = h.take(None);
        let d1 = t0.elapsed();
        assert!(first.iter().all(|e| e.cpu_pct.is_none()));
        assert!(!first.is_empty());
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let t1 = std::time::Instant::now();
        let second = h.take(None);
        let d2 = t1.elapsed();
        assert!(second.iter().all(|e| e.cpu_pct.is_some()));
        assert!(second.len() <= MAX_WORKLOAD_ENTRIES);
        let total: f64 = second.iter().filter_map(|e| e.cpu_pct).sum();
        assert!(total <= 100.0 + 0.1 * second.len() as f64, "{total}");
        let procs: u32 = second.iter().map(|e| e.proc_count).sum();
        println!("host census: {procs} processes, first tick {d1:?}, second tick {d2:?}; {second:?}");
    }
}
