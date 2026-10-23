//! Workload class census (plan
//! `2026-10-01-fleet-capacity-page-finds-the-bottleneck-and-the-idle-hardware`,
//! Decision 2 and Phase 2).
//!
//! The resource sample says how much CPU and memory a machine is spending; this
//! says **on what**. Every process on the lane is put into exactly one class of
//! a small, closed, versioned vocabulary ([`WorkloadClass`]), and the sample
//! carries one aggregate per non-empty class — `{class, cpu_pct, rss_bytes,
//! gpu_mem_bytes, proc_count, root_count}`, the shape coord stores in
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
//! bug this module can have.
//!
//! The guarantee is about what is READ, so it is kept per platform by what
//! each collector calls:
//!
//! * **Linux (host lane and the WSL guest)** reads `/proc/<pid>/stat` (whose
//!   only string is the kernel's `comm`), `/proc/<pid>/cgroup`, `/proc/stat`
//!   and the `cpu.stat` of GitHub Actions runner unit cgroups — never
//!   `cmdline` or `environ`.
//! * **macOS** asks `libproc` for each process's `PROC_PIDTBSDINFO` (pid,
//!   parent, start time and the kernel's executable name) and
//!   `PROC_PIDTASKINFO` (CPU times and resident size) — two fixed-size kernel
//!   structs with no argument in them. It never refreshes a `sysinfo` process
//!   table: `sysinfo` reads every process's `KERN_PROCARGS2` (argv AND
//!   environment) on each refresh whatever refresh kind it is given.
//! * **Windows** uses one `sysinfo` table refreshed with CPU and memory only;
//!   there `sysinfo` takes names from the system process snapshot, and `cmd`,
//!   `environ`, `cwd` and the image path are never loaded.
//! * **Any other host** takes no census: there is no collector this module
//!   trusts to leave argv unread.
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
//! * `root_count` is `null` for every class that has no notion of a root.
//! * A census that is switched off, or could not be taken whole, is no census:
//!   the sample publishes no `workloads` at all (coord stores NULL), never an
//!   empty array, which would read as "measured, and nothing is running".
//!
//! ## What a CPU share means
//!
//! `cpu_pct` is the class's share of the **whole lane's** CPU capacity over
//! the tick, 0–100 — the classes of one sample therefore sum to at most 100
//! and compare directly with the sample's own `cpu_busy_permille / 10`.
//!
//! On Linux and macOS a share is a difference of CUMULATIVE CPU time, so
//! nothing a process did inside the tick is lost to sampling: a process seen
//! for the first time started inside the interval, and ALL of its CPU time is
//! counted (a 2-second `rustc` between two 30-second ticks is attributed in
//! full). The one blind spot of per-process accounting — a process that
//! started AND exited between two ticks — is closed for GitHub Actions runner
//! units (Linux) by their cgroup's `cpu.stat` `usage_usec`, which keeps the
//! time of exited children; the unit's class gets the larger of the two
//! figures. A runner unit's surplus is a job's when the unit had a
//! `Runner.Worker` on the previous tick (the job ended inside the interval) or
//! when it exceeds what an idle listener could account for.
//!
//! A machine whose clock of record restarts — the WSL VM rebooting under a
//! running runner — is detected by `/proc/stat`: a `btime` that moved by more
//! than [`BOOT_TIME_TOLERANCE_SECS`] (it is derived from the wall clock, so an
//! NTP step nudges it by a second or so without any reboot) or a jiffy total
//! that went backwards. Every baseline is then dropped and the next tick is a
//! first tick again, never a share computed across two boots.
//!
//! Windows takes `sysinfo`'s per-process rate, which reads zero for a
//! process's first observation, so short-lived processes there go
//! unattributed. The gap between the classes and `cpu_busy_permille / 10` is
//! that unattributed load; `other` is always reported when any process falls
//! into it, so load that IS observed but unexplained is never hidden.
//! `sysinfo` also reads zero — not a measurement — for a process it could not
//! open (its CPU times are left unset), so the census retries that open and
//! reports such a process's CPU as UNKNOWN, making its class's `cpu_pct`
//! `null` exactly as an unreadable macOS process does (below).
//!
//! On macOS a non-root runner may not read another user's task info (root
//! daemons included): such a process is still identified and counted, but its
//! CPU time and resident size are UNKNOWN. A class holding even one such
//! process publishes `cpu_pct: null` and `rss_bytes: null` — never a figure
//! that silently covers only the processes the runner happened to be allowed
//! to read. An unreadable process also leaves no CPU baseline, so it is never
//! later credited its whole lifetime as if it had just started. An unreaped
//! zombie is not unreadable: xnu refuses its task info only because it has no
//! task left, so it counts as `0` CPU and `0` resident bytes.
//!
//! `rss_bytes` sums resident set sizes, so pages shared between processes of a
//! class (or across classes) are counted once per process. It is a pressure
//! indicator, not an exact partition of physical memory. It is `null` where
//! the class could not be measured whole — a member whose memory is unreadable
//! (macOS, above), or the `claude_session` placeholder below, which is also
//! the one case where `proc_count` is `null`.
//!
//! ## What a count means
//!
//! `proc_count` counts the class's user-space processes (Linux kernel threads
//! are excluded; their CPU time still counts toward `other`). `root_count` is
//! what reconciles a class against a session or job ledger: one per Claude
//! Code process with no Claude Code ancestor (`claude_session`), and one per
//! `Runner.Worker` (`gh_actions_job`).
//!
//! A Claude Code process is recognised by its executable name, `claude`. On
//! Linux the npm install renames itself (Node sets the kernel's `comm` from
//! `process.title`), but on Windows and macOS it stays the system `node`, and
//! nothing short of its argv says it is Claude Code — so those lanes
//! recognise only the native `claude` binary. A Windows or macOS census that
//! saw no native root therefore publishes `claude_session` with a `null`
//! `root_count` and `cpu_pct` (never an absent class, which would read as no
//! session); an npm session's load lands in its parent's class, and the
//! session ledger is the authority on how many run there.

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

/// The host census's cost budget: p95 over [`DURATION_WINDOW`] ticks.
const DURATION_BUDGET: Duration = Duration::from_millis(50);

/// Least time between two over-budget warnings while the p95 stays over.
const OVER_BUDGET_WARN_EVERY: Duration = Duration::from_secs(30 * 60);

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

    /// Whether the class has roots to count ([`WorkloadEntry::root_count`]).
    fn has_roots(self) -> bool {
        matches!(self, WorkloadClass::ClaudeSession | WorkloadClass::GhActionsJob)
    }
}

/// One class's aggregate in a sample. Field names are coord's storage
/// interface (`coord.device_resource_samples.workloads`) — do not rename.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct WorkloadEntry {
    pub(crate) class: &'static str,
    /// Share of the lane's total CPU capacity, 0–100. `null` on a tick with no
    /// previous observation to diff against, or when one of the class's
    /// processes had no readable CPU time over the interval.
    pub(crate) cpu_pct: Option<f64>,
    /// Resident set sizes summed over the class. `null` = not measured: the
    /// census could not tell which processes belong to the class, or could
    /// not read the memory of one of them.
    pub(crate) rss_bytes: Option<u64>,
    /// Per-process VRAM summed over the class. `null` = not measured.
    pub(crate) gpu_mem_bytes: Option<u64>,
    /// User-space processes in the class (kernel threads excluded), readable
    /// or not. `null` only on the `claude_session` placeholder a Windows or
    /// macOS census publishes when it could not count sessions at all.
    pub(crate) proc_count: Option<u32>,
    /// The class's roots — one per Claude Code process with no Claude Code
    /// ancestor (`claude_session`), one per `Runner.Worker` (`gh_actions_job`)
    /// — so the class reconciles against the session and CI-job ledgers.
    /// `null` for every other class: it has no root to count.
    pub(crate) root_count: Option<u32>,
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
const CLAUDE: &str = "claude";

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
        CLAUDE => ClaudeSession,
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
///    load, which is the question the capacity page asks of it. That includes
///    a container whose `containerd-shim` runs under the job or in its unit
///    (rootless Docker, Podman). A container the system `dockerd` started on a
///    job's behalf has a shim parented to `containerd` and sits in its own
///    `docker-<id>.scope`: nothing short of the Docker API or argv ties it to
///    the job, so it stays `docker`.
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

/// Whether a process is one of its class's roots ([`WorkloadEntry::root_count`]).
fn is_root(class: WorkloadClass, p: &ProcView<'_>) -> bool {
    match class {
        WorkloadClass::ClaudeSession => p.exe == CLAUDE && !p.parent_chain.contains(&CLAUDE),
        WorkloadClass::GhActionsJob => p.exe == ACTIONS_WORKER,
        _ => false,
    }
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
    /// This process's share of the lane's CPU capacity over the tick, 0–100;
    /// `None` when it could not be read. Ignored when the census is told the
    /// tick has no CPU delta.
    pub(crate) cpu_pct: Option<f64>,
    /// `None` when the process's resident size could not be read.
    pub(crate) rss_bytes: Option<u64>,
    /// A Linux kernel thread: its CPU counts, but it is no workload process.
    pub(crate) kernel_thread: bool,
}

/// The most CPU, as a share of the lane, an idle runner unit's exited children
/// are taken to explain. An idle `Runner.Listener` long-polls and spawns
/// almost nothing; a unit surplus above this ran a job that exited inside the
/// interval, even when no `Runner.Worker` was alive at either end of it.
const LISTENER_OVERHEAD_PCT: f64 = 0.5;

/// What runner unit cgroups add to one tick (Linux only; empty elsewhere).
#[derive(Debug, Default)]
pub(crate) struct UnitCpu {
    /// Runner unit → its cgroup's own CPU share over the tick, 0–100.
    pub(crate) share: HashMap<String, f64>,
    /// Runner units that had a `Runner.Worker` on the PREVIOUS tick.
    pub(crate) busy_before: HashSet<String>,
}

/// The runner units that have a `Runner.Worker` in this table.
fn busy_runner_units(records: &[ProcRecord]) -> HashSet<String> {
    records
        .iter()
        .filter(|r| r.name == ACTIONS_WORKER)
        .filter_map(|r| r.unit.clone().filter(|u| is_actions_runner_unit(u)))
        .collect()
}

/// One class's running totals. A member whose CPU or memory could not be read
/// makes that figure UNKNOWN for the whole class: a sum over the readable
/// members only would publish a partial figure as if it were the class's.
#[derive(Debug, Default)]
struct ClassTotal {
    cpu: f64,
    cpu_unknown: bool,
    rss: u64,
    rss_unknown: bool,
    vram: u64,
    procs: u32,
    roots: u32,
}

impl ClassTotal {
    /// Add one member's CPU share and resident size; `None` is unreadable.
    fn add(&mut self, cpu_pct: Option<f64>, rss_bytes: Option<u64>) {
        match cpu_pct {
            Some(c) => self.cpu += c,
            None => self.cpu_unknown = true,
        }
        match rss_bytes {
            Some(r) => self.rss = self.rss.saturating_add(r),
            None => self.rss_unknown = true,
        }
    }

    /// The class's share, rounded to hundredths — `None` on a tick with no CPU
    /// delta, or when any member's CPU was unreadable.
    fn cpu_pct(&self, cpu_known: bool) -> Option<f64> {
        (cpu_known && !self.cpu_unknown).then(|| (self.cpu.clamp(0.0, 100.0) * 100.0).round() / 100.0)
    }

    /// The class's resident total — `None` when any member's was unreadable.
    fn rss_bytes(&self) -> Option<u64> {
        (!self.rss_unknown).then_some(self.rss)
    }
}

/// Aggregate a process table into one entry per non-empty class. PURE.
///
/// `cpu_known = false` publishes `cpu_pct: null` on every entry (first tick).
/// A class with a member whose CPU (memory) is unreadable publishes
/// `cpu_pct` (`rss_bytes`) `null` — see [`ClassTotal`].
/// `vram_by_pid` is a complete per-process VRAM measurement, or `None` when
/// none was taken — then every entry's `gpu_mem_bytes` is `null`.
/// `unit_cpu` carries each runner unit cgroup's own share over the tick (it
/// includes children that exited inside the tick) — see [`UnitCpu`].
/// A unit whose live processes all fall in one class raises that class by
/// whatever the unit's figure exceeds its processes' sum; a unit whose
/// processes span classes, or one of whose processes has no readable CPU, is
/// ignored, since its surplus cannot be placed or measured.
pub(crate) fn census(
    records: &[ProcRecord],
    cpu_known: bool,
    vram_by_pid: Option<&HashMap<u32, u64>>,
    unit_cpu: &UnitCpu,
) -> Vec<WorkloadEntry> {
    let by_pid: HashMap<u32, &ProcRecord> = records.iter().map(|r| (r.pid, r)).collect();

    // Which runners have a job: by unit, and by listener (the worker is the
    // listener's child), so both the Linux cgroup path and the name-only path
    // (Windows, macOS, a runner started by hand) agree.
    let busy_units = busy_runner_units(records);
    let mut busy_listeners: HashSet<u32> = HashSet::new();
    for r in records.iter().filter(|r| r.name == ACTIONS_WORKER) {
        if let Some(parent) = r.ppid.and_then(|pp| by_pid.get(&pp)) {
            if parent.name == ACTIONS_LISTENER {
                busy_listeners.insert(parent.pid);
            }
        }
    }

    let mut acc: HashMap<WorkloadClass, ClassTotal> = HashMap::new();
    /// A unit's live processes: their CPU sum, and their one class — `None`
    /// once two classes, or a process with unreadable CPU, have been seen.
    struct UnitAcc {
        cpu: f64,
        class: Option<WorkloadClass>,
    }
    let mut units: HashMap<&str, UnitAcc> = HashMap::new();

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
        let class = classify(&view);
        let a = acc.entry(class).or_default();
        a.add(r.cpu_pct, r.rss_bytes);
        if !r.kernel_thread {
            a.procs = a.procs.saturating_add(1);
        }
        if is_root(class, &view) {
            a.roots = a.roots.saturating_add(1);
        }
        if let Some(v) = vram_by_pid.and_then(|m| m.get(&r.pid)) {
            a.vram = a.vram.saturating_add(*v);
        }
        if let Some(u) = unit.filter(|u| unit_cpu.share.contains_key(*u)) {
            let ua = units.entry(u).or_insert(UnitAcc {
                cpu: 0.0,
                class: Some(class),
            });
            ua.cpu += r.cpu_pct.unwrap_or(0.0);
            if ua.class != Some(class) || r.cpu_pct.is_none() {
                ua.class = None;
            }
        }
    }

    for (unit, ua) in &units {
        let (Some(class), Some(share)) = (ua.class, unit_cpu.share.get(*unit)) else {
            continue;
        };
        let surplus = (share - ua.cpu).max(0.0);
        if surplus <= 0.0 {
            continue;
        }
        // The live processes say "idle" at the END of the interval; a worker
        // that ran on the previous tick, or more CPU than a listener spends,
        // says a job ran inside it and exited before this reading.
        let class = if class == WorkloadClass::GhActionsRunnerIdle
            && (unit_cpu.busy_before.contains(*unit) || surplus > LISTENER_OVERHEAD_PCT)
        {
            WorkloadClass::GhActionsJob
        } else {
            class
        };
        acc.entry(class).or_default().cpu += surplus;
    }

    WorkloadClass::ALL
        .iter()
        .filter_map(|class| {
            let a = acc.get(class)?;
            Some(WorkloadEntry {
                class: class.as_str(),
                cpu_pct: a.cpu_pct(cpu_known),
                rss_bytes: a.rss_bytes(),
                gpu_mem_bytes: vram_by_pid.map(|_| a.vram),
                proc_count: Some(a.procs),
                root_count: class.has_roots().then_some(a.roots),
            })
        })
        .collect()
}

/// Where a `/proc/<pid>/cgroup` body places a process: the innermost
/// `*.service` / `*.scope` segment of the unified (v2) hierarchy, or of the v1
/// `name=systemd` one, plus — v2 only — that unit's cgroup path (the prefix of
/// the process's path ending at the unit), where its `cpu.stat` lives. `None`
/// when neither hierarchy names a unit.
pub(crate) fn unit_placement(text: &str) -> Option<(String, Option<String>)> {
    let (path, v2) = match text.lines().find_map(|l| l.strip_prefix("0::")) {
        Some(p) => (p, true),
        None => (
            text.lines()
                .find_map(|l| l.split_once(":name=systemd:").map(|(_, p)| p))?,
            false,
        ),
    };
    let segs: Vec<&str> = path.trim().split('/').collect();
    let idx = segs
        .iter()
        .rposition(|seg| seg.ends_with(".service") || seg.ends_with(".scope"))?;
    let unit = segs.get(idx)?.to_string();
    let unit_path = v2
        .then(|| segs.get(..=idx).map(|s| s.join("/")))
        .flatten();
    Some((unit, unit_path))
}

/// The systemd unit a `/proc/<pid>/cgroup` body places a process in
/// ([`unit_placement`] without the path).
pub(crate) fn unit_from_cgroup(text: &str) -> Option<String> {
    unit_placement(text).map(|(unit, _)| unit)
}

/// `usage_usec` from a cgroup v2 `cpu.stat` body.
fn parse_usage_usec(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("usage_usec "))
        .and_then(|v| v.trim().parse().ok())
}

// ---------------------------------------------------------------------------
// Cumulative CPU-time accounting — the Linux and macOS host lanes and the WSL guest
// ---------------------------------------------------------------------------

/// `PF_KTHREAD` in `/proc/<pid>/stat`'s `flags` field.
const PF_KTHREAD: u64 = 0x0020_0000;

/// One process as `/proc/<pid>/stat` describes it (macOS fills the same
/// fields from `libproc`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcStat {
    pid: u32,
    ppid: u32,
    /// The kernel's executable name — never argv.
    comm: String,
    /// With `pid`, what tells a reused pid from the process before it. `None`
    /// when the process could be identified but not dated (macOS, another
    /// user's process): it then has no CPU accounting at all, since nothing
    /// tells its next reading from a reused pid's.
    start_time: Option<u64>,
    /// User + system CPU time, cumulative over the process's life (its own
    /// threads; not its reaped children): jiffies on Linux, microseconds on
    /// macOS. `None` when unreadable.
    cpu_ticks: Option<u64>,
    /// `None` when unreadable.
    rss_pages: Option<u64>,
    kernel_thread: bool,
    /// An unreaped zombie (macOS `SZOMB`): it has no task left, so its CPU
    /// time can be neither read nor added to — over any interval it is `0`,
    /// never UNKNOWN. Always `false` on Linux, where a zombie's `stat` still
    /// carries its CPU time.
    zombie: bool,
}

/// Parse one `/proc/<pid>/stat` line. `comm` may contain spaces and
/// parentheses, so it is the text between the FIRST `(` and the LAST `)`.
fn parse_proc_stat(line: &str) -> Option<ProcStat> {
    let (pid, rest) = line.split_once(" (")?;
    let (comm, fields) = rest.rsplit_once(") ")?;
    let f: Vec<&str> = fields.split_whitespace().collect();
    // proc(5) field numbers minus 3: state is field 3, ppid 4, flags 9,
    // utime 14, stime 15, starttime 22, rss 24.
    let num = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok());
    Some(ProcStat {
        pid: pid.trim().parse().ok()?,
        ppid: u32::try_from(num(1)?).ok()?,
        comm: comm.to_string(),
        cpu_ticks: Some(num(11)?.saturating_add(num(12)?)),
        start_time: Some(num(19)?),
        rss_pages: Some(num(21)?),
        kernel_thread: num(6).is_some_and(|flags| flags & PF_KTHREAD != 0),
        zombie: false,
    })
}

/// xnu's `p_stat` for a zombie (`bsd/sys/proc.h`, `#define SZOMB 5`). Spelled
/// here rather than taken from `libc` so [`macos_task_reading`] is testable
/// off macOS; `macos_proc_stat` pins it to `libc::SZOMB` at compile time.
const SZOMB: u32 = 5;

/// What a macOS process's `PROC_PIDTASKINFO` reading makes of its CPU time
/// and resident size, as `(cpu_ticks, rss_bytes, zombie)`. PURE.
///
/// `status` is the process's `p_stat` (both BSD flavors carry it); `task` is
/// `(cumulative CPU microseconds, resident bytes)` when the task flavor
/// answered. xnu answers the BSD flavors for an unreaped zombie but refuses
/// its task flavor — it has no task left — so a zombie is NOT unreadable: it
/// holds no memory and accrues no CPU. Treating it as unreadable would null
/// its whole class's figures every tick until its parent reaps it.
fn macos_task_reading(status: u32, task: Option<(u64, u64)>) -> (Option<u64>, Option<u64>, bool) {
    if status == SZOMB {
        return (None, Some(0), true);
    }
    match task {
        Some((cpu_us, rss)) => (Some(cpu_us), Some(rss), false),
        // Another user's process: UNKNOWN, never zero.
        None => (None, None, false),
    }
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

/// `btime` (boot time, seconds since the epoch) from a `/proc/stat` line.
fn parse_btime(line: &str) -> Option<u64> {
    line.trim().strip_prefix("btime ")?.trim().parse().ok()
}

/// How far `btime` may move between two readings of one boot. The kernel
/// derives it from the wall clock minus uptime, so a clock step (NTP, a
/// resumed VM resyncing) shifts it by a second or so with no reboot; a reboot
/// moves it by the whole uptime the previous boot had.
const BOOT_TIME_TOLERANCE_SECS: u64 = 5;

/// Whether the counters restarted between two readings: a `btime` that
/// appeared, vanished or moved by more than [`BOOT_TIME_TOLERANCE_SECS`], or a
/// jiffy total that went backwards (a reboot soon after the previous boot
/// moves `btime` little, but always restarts the total). PURE.
fn counters_restarted(prev_boot: Option<u64>, prev_total: u64, boot: Option<u64>, total: u64) -> bool {
    let boot_moved = match (prev_boot, boot) {
        (Some(a), Some(b)) => a.abs_diff(b) > BOOT_TIME_TOLERANCE_SECS,
        (None, None) => false,
        _ => true,
    };
    boot_moved || total < prev_total
}

/// One reading of a lane's process table in cumulative CPU time: procfs on
/// Linux and in the WSL guest, `libproc` on macOS.
#[derive(Debug, Clone, Default)]
struct CpuTimeTick {
    /// Bytes per unit of [`ProcStat::rss_pages`].
    page_size: u64,
    /// `USER_HZ`, which converts a cgroup's microseconds into jiffies. `None`
    /// (unreadable) forgoes the unit-cgroup figures, never the census.
    clk_tck: Option<u64>,
    /// The machine's boot time. A change beyond [`BOOT_TIME_TOLERANCE_SECS`]
    /// between two readings means the counters restarted under us (the WSL VM
    /// rebooted), so no baseline carries over — see [`counters_restarted`].
    boot_time: Option<u64>,
    /// The lane's whole CPU capacity so far, in the unit of
    /// [`ProcStat::cpu_ticks`] — jiffies across all CPUs on Linux.
    total_jiffies: u64,
    stats: Vec<ProcStat>,
    /// pid → systemd unit.
    units: HashMap<u32, String>,
    /// GitHub Actions runner unit → its cgroup's cumulative `usage_usec`.
    unit_usage_usec: HashMap<String, u64>,
}

/// The previous reading a tick is diffed against.
#[derive(Debug)]
struct PrevTick {
    boot_time: Option<u64>,
    total_jiffies: u64,
    /// `(pid, start time)` → cumulative CPU ticks, `None` for a process whose
    /// CPU time was unreadable: it was alive, so it is NOT new next tick, and
    /// it has no baseline to diff against.
    ticks: HashMap<(u32, u64), Option<u64>>,
    /// Pids of processes identified but not dated ([`ProcStat::start_time`]
    /// `None`). A dated process under one of them next tick may be that same
    /// process, so it is not taken as new.
    undated: HashSet<u32>,
    unit_usage_usec: HashMap<String, u64>,
    /// Runner units that had a `Runner.Worker` in that reading.
    busy_units: HashSet<String>,
}

/// One process's CPU ticks over the interval since `prev`. PURE.
///
/// A process absent from the previous reading started inside the interval, so
/// ALL of its CPU time belongs to it. `None` — UNKNOWN, never zero and never a
/// lifetime — when its CPU time is unreadable now, it has no start time to key
/// a baseline by, or it was alive last time without a readable baseline. A
/// zombie accrued nothing: `0`, whatever its baseline.
fn interval_cpu_ticks(prev: &PrevTick, s: &ProcStat) -> Option<u64> {
    if s.zombie {
        return Some(0);
    }
    let now = s.cpu_ticks?;
    let start = s.start_time?;
    match prev.ticks.get(&(s.pid, start)) {
        Some(before) => before.map(|b| now.saturating_sub(b)),
        None if prev.undated.contains(&s.pid) => None,
        None => Some(now),
    }
}

/// The baseline a dated process leaves for the next tick: its cumulative CPU
/// time, or `None` (alive, no baseline) when that is unreadable. A zombie's CPU
/// time is unreadable but frozen, so it carries forward the baseline it had —
/// and none when it had none, so a later reading of that key is UNKNOWN rather
/// than a whole lifetime poured into one interval. PURE.
fn next_baseline(prev: Option<&PrevTick>, key: (u32, u64), s: &ProcStat) -> Option<u64> {
    if s.zombie {
        prev.and_then(|p| p.ticks.get(&key).copied().flatten())
    } else {
        s.cpu_ticks
    }
}

/// Cumulative-time accounting over successive [`CpuTimeTick`]s.
#[derive(Debug, Default)]
struct CpuTimeCensus {
    prev: Option<PrevTick>,
}

impl CpuTimeCensus {
    /// Aggregate one reading. `None` when it holds no process — the lane then
    /// publishes no census, never a fabricated one.
    fn take(
        &mut self,
        tick: CpuTimeTick,
        vram_by_pid: Option<&HashMap<u32, u64>>,
    ) -> Option<Vec<WorkloadEntry>> {
        if tick.stats.is_empty() {
            return None;
        }
        // A different boot is a different machine as far as every counter is
        // concerned: pids, start times, cgroup usage and the jiffy total all
        // restarted, and a restarted total can exceed the old one.
        if self.prev.as_ref().is_some_and(|prev| {
            counters_restarted(prev.boot_time, prev.total_jiffies, tick.boot_time, tick.total_jiffies)
        }) {
            self.prev = None;
        }
        // The interval in jiffies of the whole lane, and the reading it starts at.
        let interval = self.prev.as_ref().and_then(|prev| {
            let span = tick.total_jiffies.checked_sub(prev.total_jiffies).filter(|s| *s > 0)?;
            Some((span as f64, prev))
        });
        let cpu_known = interval.is_some();
        let records: Vec<ProcRecord> = tick
            .stats
            .iter()
            .map(|s| ProcRecord {
                pid: s.pid,
                ppid: (s.ppid != 0).then_some(s.ppid),
                name: normalize_exe_name(&s.comm),
                unit: tick.units.get(&s.pid).cloned(),
                // Without an interval the census ignores CPU altogether.
                cpu_pct: match interval {
                    Some((span, prev)) => interval_cpu_ticks(prev, s).map(|t| t as f64 / span * 100.0),
                    None => Some(0.0),
                },
                rss_bytes: s.rss_pages.map(|p| p.saturating_mul(tick.page_size)),
                kernel_thread: s.kernel_thread,
            })
            .collect();
        // A unit first seen this tick has no baseline: its counter spans its
        // whole life, so it is left out rather than poured into one interval.
        let unit_cpu = match (interval, tick.clk_tck) {
            (Some((span, prev)), Some(hz)) => UnitCpu {
                share: tick
                    .unit_usage_usec
                    .iter()
                    .filter_map(|(unit, usage)| {
                        let delta = usage.checked_sub(*prev.unit_usage_usec.get(unit)?)?;
                        let jiffies = delta as f64 * hz as f64 / 1_000_000.0;
                        Some((unit.clone(), jiffies / span * 100.0))
                    })
                    .collect(),
                busy_before: prev.busy_units.clone(),
            },
            _ => UnitCpu::default(),
        };
        let busy_units = busy_runner_units(&records);
        // An unreadable CPU time is kept as `None`, never as a baseline.
        let ticks = tick
            .stats
            .iter()
            .filter_map(|s| {
                let key = (s.pid, s.start_time?);
                Some((key, next_baseline(self.prev.as_ref(), key, s)))
            })
            .collect();
        self.prev = Some(PrevTick {
            boot_time: tick.boot_time,
            total_jiffies: tick.total_jiffies,
            ticks,
            undated: tick
                .stats
                .iter()
                .filter(|s| s.start_time.is_none())
                .map(|s| s.pid)
                .collect(),
            unit_usage_usec: tick.unit_usage_usec,
            busy_units,
        });
        Some(census(&records, cpu_known, vram_by_pid, &unit_cpu))
    }
}

// ---------------------------------------------------------------------------
// Host lane
// ---------------------------------------------------------------------------

/// Recent host-census durations, and the over-budget warning's state.
struct CensusTimer {
    durations: VecDeque<Duration>,
    last_warned: Option<std::time::Instant>,
}

impl CensusTimer {
    fn new() -> Self {
        Self {
            durations: VecDeque::with_capacity(DURATION_WINDOW),
            last_warned: None,
        }
    }

    /// Keep this tick's duration and log it with the window p95. A p95 over
    /// [`DURATION_BUDGET`] is a warning, repeated at most every
    /// [`OVER_BUDGET_WARN_EVERY`] while it lasts.
    fn record(&mut self, took: Duration, procs: usize) {
        if self.durations.len() == DURATION_WINDOW {
            self.durations.pop_front();
        }
        self.durations.push_back(took);
        let p95 = p95(&self.durations);
        if p95 > DURATION_BUDGET {
            let due = self
                .last_warned
                .is_none_or(|t| t.elapsed() >= OVER_BUDGET_WARN_EVERY);
            if due {
                self.last_warned = Some(std::time::Instant::now());
                tracing::warn!(
                    "fleet::workload_census: host census p95 {} ms over {} ticks exceeds its {} ms \
                     budget (this tick: {procs} processes in {} ms)",
                    p95.as_millis(),
                    self.durations.len(),
                    DURATION_BUDGET.as_millis(),
                    took.as_millis()
                );
                return;
            }
        } else {
            self.last_warned = None;
        }
        tracing::debug!(
            "fleet::workload_census: host census of {procs} processes took {} ms (p95 {} ms over {} ticks)",
            took.as_millis(),
            p95.as_millis(),
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

/// The host lane's census state on Linux: procfs read directly, so CPU is
/// accounted in cumulative time (see the module docs).
#[cfg(target_os = "linux")]
pub(crate) struct HostCensus {
    cumulative: CpuTimeCensus,
    /// Each live process's unit placement, keyed by pid and start time so a
    /// reused pid is re-read. A placement does not change under a process
    /// that is not moved, so each `/proc/<pid>/cgroup` is read once — once
    /// it has been read successfully.
    placements: HashMap<u32, (Option<u64>, Option<(String, Option<String>)>)>,
    page_size: u64,
    clk_tck: Option<u64>,
    timer: CensusTimer,
}

#[cfg(target_os = "linux")]
impl HostCensus {
    pub(crate) fn new() -> Self {
        // SAFETY: `sysconf` takes an integer selector, touches no caller memory
        // and has no preconditions; every return value is handled below.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        // SAFETY: as above.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Self {
            cumulative: CpuTimeCensus::default(),
            placements: HashMap::new(),
            // `_SC_PAGESIZE` never fails on Linux; the constant only keeps a
            // reading from turning into a zero.
            page_size: u64::try_from(page).ok().filter(|p| *p > 0).unwrap_or(4096),
            clk_tck: u64::try_from(hz).ok().filter(|h| *h > 0),
            timer: CensusTimer::new(),
        }
    }

    /// Read the process table and aggregate it. `vram_by_pid` is the seam for
    /// a per-process VRAM measurement; `None` publishes `gpu_mem_bytes: null`.
    /// `None` when procfs could not be read.
    pub(crate) fn take(&mut self, vram_by_pid: Option<&HashMap<u32, u64>>) -> Option<Vec<WorkloadEntry>> {
        let started = std::time::Instant::now();
        let tick = self.read_tick()?;
        let procs = tick.stats.len();
        let entries = self.cumulative.take(tick, vram_by_pid);
        self.timer.record(started.elapsed(), procs);
        entries
    }

    fn read_tick(&mut self) -> Option<CpuTimeTick> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let total_jiffies = parse_total_jiffies(stat.lines().next()?)?;
        let boot_time = stat.lines().find_map(parse_btime);
        let stats: Vec<ProcStat> = std::fs::read_dir("/proc")
            .ok()?
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            // A process can exit between the listing and the read.
            .filter_map(|pid| std::fs::read_to_string(format!("/proc/{pid}/stat")).ok())
            .filter_map(|line| parse_proc_stat(line.trim_end()))
            .collect();

        let live: HashSet<u32> = stats.iter().map(|s| s.pid).collect();
        self.placements.retain(|pid, _| live.contains(pid));
        let mut units = HashMap::new();
        let mut runner_unit_paths: HashMap<String, String> = HashMap::new();
        for s in &stats {
            let cached = self
                .placements
                .get(&s.pid)
                .filter(|(start, _)| *start == s.start_time)
                .map(|(_, p)| p.clone());
            // Only a read that succeeded is cached: a failed one (a racing
            // exit, a transient error) is retried next tick, never pinned.
            let placement = match cached {
                Some(p) => p,
                None => match std::fs::read_to_string(format!("/proc/{}/cgroup", s.pid)) {
                    Ok(text) => {
                        let p = unit_placement(&text);
                        self.placements.insert(s.pid, (s.start_time, p.clone()));
                        p
                    }
                    Err(_) => None,
                },
            };
            if let Some((unit, path)) = placement {
                if let Some(path) = path.filter(|_| is_actions_runner_unit(&unit)) {
                    runner_unit_paths.insert(unit.clone(), path);
                }
                units.insert(s.pid, unit);
            }
        }
        // The unified hierarchy is at the root on a pure-v2 host and under
        // `unified/` on a hybrid one; a v1-only host has no `usage_usec`.
        let unit_usage_usec = runner_unit_paths
            .into_iter()
            .filter_map(|(unit, path)| {
                ["/sys/fs/cgroup", "/sys/fs/cgroup/unified"]
                    .iter()
                    .find_map(|root| {
                        let body = std::fs::read_to_string(format!("{root}{path}/cpu.stat")).ok()?;
                        parse_usage_usec(&body)
                    })
                    .map(|usage| (unit, usage))
            })
            .collect();

        Some(CpuTimeTick {
            page_size: self.page_size,
            clk_tck: self.clk_tck,
            boot_time,
            total_jiffies,
            stats,
            units,
            unit_usage_usec,
        })
    }
}

/// The host lane's census state on macOS: `libproc` read directly, so CPU is
/// accounted in cumulative time as on Linux.
///
/// Never a `sysinfo` process refresh: on macOS that reads every process's
/// `KERN_PROCARGS2` — argv and environment — into the runner on every call,
/// whatever `ProcessRefreshKind` asks for. The three `proc_pidinfo` flavors
/// used here return fixed-size kernel structs with no argument in them.
#[cfg(target_os = "macos")]
pub(crate) struct HostCensus {
    cumulative: CpuTimeCensus,
    /// The origin of the lane's capacity clock ([`CpuTimeTick::total_jiffies`]
    /// is CPU-microseconds since this instant).
    epoch: std::time::Instant,
    /// Online CPUs; `None` when `sysconf` would not say, and then there is no
    /// census — a guessed count would scale every share by the wrong factor.
    cpu_count: Option<u64>,
    /// Mach absolute-time units → nanoseconds, as `(numer, denom)`; `None`
    /// when the kernel would not say, and then there is no census.
    timebase: Option<(u64, u64)>,
    timer: CensusTimer,
}

#[cfg(target_os = "macos")]
impl HostCensus {
    pub(crate) fn new() -> Self {
        // SAFETY: `sysconf` takes an integer selector, touches no caller memory
        // and has no preconditions; every return value is handled below.
        let ncpu = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        Self {
            cumulative: CpuTimeCensus::default(),
            epoch: std::time::Instant::now(),
            cpu_count: u64::try_from(ncpu).ok().filter(|n| *n > 0),
            timebase: mach_timebase(),
            timer: CensusTimer::new(),
        }
    }

    /// Read the process table and aggregate it. `vram_by_pid` is the seam for
    /// a per-process VRAM measurement; `None` publishes `gpu_mem_bytes: null`.
    /// `None` when the table could not be read whole.
    pub(crate) fn take(&mut self, vram_by_pid: Option<&HashMap<u32, u64>>) -> Option<Vec<WorkloadEntry>> {
        let started = std::time::Instant::now();
        let tick = self.read_tick()?;
        let procs = tick.stats.len();
        let mut entries = self.cumulative.take(tick, vram_by_pid);
        if let Some(entries) = entries.as_mut() {
            claude_sessions_unknown_without_native_root(entries);
        }
        self.timer.record(started.elapsed(), procs);
        entries
    }

    fn read_tick(&self) -> Option<CpuTimeTick> {
        let (numer, denom) = self.timebase?;
        // The lane's capacity so far, in the unit of a process's CPU time.
        let elapsed_us = u64::try_from(self.epoch.elapsed().as_micros()).ok()?;
        let total = elapsed_us.checked_mul(self.cpu_count?)?;
        let stats: Vec<ProcStat> = macos_all_pids()?
            .into_iter()
            .filter_map(|pid| macos_proc_stat(pid, numer, denom))
            .collect();
        Some(CpuTimeTick {
            // `rss_pages` carries bytes here.
            page_size: 1,
            clk_tck: None,
            // The capacity clock is this process's own monotonic clock; it
            // cannot restart under the census.
            boot_time: None,
            total_jiffies: total,
            stats,
            units: HashMap::new(),
            unit_usage_usec: HashMap::new(),
        })
    }
}

/// The Mach timebase, `(numer, denom)`: `pti_total_user`/`pti_total_system`
/// are in Mach absolute-time units, which are nanoseconds only on Intel.
#[cfg(target_os = "macos")]
#[allow(deprecated)] // libc points at the `mach2` crate, which this crate does not use.
fn mach_timebase() -> Option<(u64, u64)> {
    // SAFETY: `mach_timebase_info` is two `u32`s; all-zero is a valid value.
    let mut tb: libc::mach_timebase_info = unsafe { std::mem::zeroed() };
    // SAFETY: writes one struct this frame owns, and nothing else.
    let rc = unsafe { libc::mach_timebase_info(&mut tb) };
    (rc == 0 && tb.numer > 0 && tb.denom > 0).then(|| (u64::from(tb.numer), u64::from(tb.denom)))
}

/// Every pid on the host. `None` when the list cannot be read whole — a
/// truncated table would be a silent undercount.
#[cfg(target_os = "macos")]
fn macos_all_pids() -> Option<Vec<libc::pid_t>> {
    // SAFETY: a null buffer of size 0 only asks for the current count.
    let estimate = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let mut capacity = usize::try_from(estimate).ok().filter(|n| *n > 0)?;
    // A list that fills its buffer may have been cut short by processes
    // started since the estimate: grow and ask again.
    for _ in 0..3 {
        capacity = capacity.saturating_add(capacity / 4).saturating_add(64);
        let mut pids: Vec<libc::pid_t> = vec![0; capacity];
        let bytes = libc::c_int::try_from(capacity.checked_mul(std::mem::size_of::<libc::pid_t>())?).ok()?;
        // SAFETY: `pids` is writable for `bytes` bytes and outlives the call;
        // the return value is the number of pids written.
        let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let got = usize::try_from(got).ok().filter(|n| *n > 0)?;
        if got < capacity {
            pids.truncate(got);
            return Some(pids);
        }
    }
    None
}

/// One process from `libproc`: `PROC_PIDTBSDINFO` for its identity and start
/// time, `PROC_PIDTASKINFO` for its CPU time and resident size. `None` only
/// when the process has exited or cannot even be identified.
///
/// Both of those flavors are refused for another user's process unless the
/// runner is root; `PROC_PIDT_SHORTBSDINFO` is exempt from that check (xnu
/// `bsd/kern/proc_info.c`, `proc_pidinfo`, `NO_CHECK_SAME_USER`), so such a
/// process is still identified — pid, parent and a 16-byte name — and
/// counted, with no start time and its CPU time and memory UNKNOWN (`None`),
/// never zero. A zombie is the exception the other way: see
/// [`macos_task_reading`].
#[cfg(target_os = "macos")]
fn macos_proc_stat(pid: libc::pid_t, numer: u64, denom: u64) -> Option<ProcStat> {
    const _: () = assert!(SZOMB == libc::SZOMB, "xnu's SZOMB as libc spells it");
    // SAFETY: `proc_bsdinfo` is a plain C struct; all-zero is a valid value.
    let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let bsd_size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: the kernel writes at most `bsd_size` bytes into `bsd`, which
    // this frame owns; the return value is the byte count written.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::addr_of_mut!(bsd).cast(),
            bsd_size,
        )
    };
    let (own_pid, ppid, comm, start_time, status) = if got == bsd_size {
        // `pbi_name` is the executable's name up to 32 bytes, `pbi_comm` the
        // same cut at 16; neither is ever argv.
        let name = Some(c_chars_to_string(&bsd.pbi_name))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| c_chars_to_string(&bsd.pbi_comm));
        let start = bsd
            .pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(bsd.pbi_start_tvusec);
        (bsd.pbi_pid, bsd.pbi_ppid, name, Some(start), bsd.pbi_status)
    } else {
        // SAFETY: `proc_bsdshortinfo` is a plain C struct; all-zero is valid.
        let mut short: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let short_size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdshortinfo>()).ok()?;
        // SAFETY: as above, for `proc_bsdshortinfo`.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                std::ptr::addr_of_mut!(short).cast(),
                short_size,
            )
        };
        if got != short_size {
            return None;
        }
        // `pbsi_comm` is the executable's name cut at 16 bytes, never argv.
        (short.pbsi_pid, short.pbsi_ppid, c_chars_to_string(&short.pbsi_comm), None, short.pbsi_status)
    };

    // SAFETY: as above, for `proc_taskinfo`.
    let mut task: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let task_size = libc::c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: as above.
    let task_got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            std::ptr::addr_of_mut!(task).cast(),
            task_size,
        )
    };
    let reading = (task_got == task_size).then(|| {
        let mach = u128::from(task.pti_total_user.saturating_add(task.pti_total_system));
        let ns = mach * u128::from(numer) / u128::from(denom);
        (u64::try_from(ns / 1_000).unwrap_or(u64::MAX), task.pti_resident_size)
    });
    let (cpu_ticks, rss_pages, zombie) = macos_task_reading(status, reading);

    Some(ProcStat {
        pid: own_pid,
        ppid,
        comm,
        start_time,
        cpu_ticks,
        // `rss_pages` carries bytes here.
        rss_pages,
        kernel_thread: false,
        zombie,
    })
}

/// A NUL-terminated fixed-size C name → `String` (lossy).
#[cfg(target_os = "macos")]
fn c_chars_to_string(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| c.to_ne_bytes()[0])
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The host lane's census state on Windows: ONE `sysinfo::System` kept across
/// ticks, so the sample interval itself is the CPU diff window.
#[cfg(windows)]
pub(crate) struct HostCensus {
    sys: sysinfo::System,
    /// Whether `sys` has been refreshed before — the first refresh has no CPU
    /// delta.
    primed: bool,
    /// Logical CPUs; `None` when `sysinfo` listed none, and then there is no
    /// census — a guessed count would scale every share by the wrong factor.
    cpu_count: Option<usize>,
    timer: CensusTimer,
}

#[cfg(windows)]
impl HostCensus {
    pub(crate) fn new() -> Self {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_list(sysinfo::CpuRefreshKind::new());
        let cpu_count = Some(sys.cpus().len()).filter(|n| *n > 0);
        Self {
            sys,
            primed: false,
            cpu_count,
            timer: CensusTimer::new(),
        }
    }

    /// Refresh the table and aggregate it. `vram_by_pid` is the seam for a
    /// per-process VRAM measurement; `None` publishes `gpu_mem_bytes: null`.
    /// `None` when the CPU count is unknown.
    pub(crate) fn take(&mut self, vram_by_pid: Option<&HashMap<u32, u64>>) -> Option<Vec<WorkloadEntry>> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};

        let cpu_count = self.cpu_count? as f64;
        let started = std::time::Instant::now();
        // CPU and memory only; `cmd`, `environ`, `cwd` and the image path stay
        // at `UpdateKind::Never`, so they are never even read into the table.
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_cpu().with_memory(),
        );
        let cpu_known = self.primed;
        self.primed = true;

        let records: Vec<ProcRecord> = self
            .sys
            .processes()
            .values()
            .map(|p| ProcRecord {
                pid: p.pid().as_u32(),
                ppid: p.parent().map(|pp| pp.as_u32()),
                name: normalize_exe_name(&p.name().to_string_lossy()),
                unit: None,
                cpu_pct: windows_cpu_readable(p.pid().as_u32()).then(|| f64::from(p.cpu_usage()) / cpu_count),
                rss_bytes: Some(p.memory()),
                kernel_thread: false,
            })
            .collect();

        let mut entries = census(&records, cpu_known, vram_by_pid, &UnitCpu::default());
        claude_sessions_unknown_without_native_root(&mut entries);
        self.timer.record(started.elapsed(), records.len());
        Some(entries)
    }
}

/// Whether `sysinfo` can have read `pid`'s CPU times this tick.
///
/// sysinfo 0.32 opens each process with `PROCESS_QUERY_INFORMATION |
/// PROCESS_VM_READ`, falling back to `PROCESS_QUERY_LIMITED_INFORMATION`; when
/// both are refused (a protected or another session's process), its
/// `compute_cpu_usage` skips `GetProcessTimes`, leaves the times zero and
/// reports 0 % — a default, not a reading. It exposes no "no handle" flag, so
/// the weaker of its two opens is repeated here: refused means UNKNOWN.
/// Resident size is unaffected: sysinfo takes it from the system snapshot.
///
/// Pid 0, the System Idle Process, is never opened by sysinfo either, but its
/// "CPU" is the machine's idle time, which is no load: it stays a measured 0.
#[cfg(windows)]
fn windows_cpu_readable(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    if pid == 0 {
        return true;
    }
    // SAFETY: `OpenProcess` reads no caller memory and returns null on failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: a non-null handle from `OpenProcess` is ours, closed exactly once.
    unsafe { CloseHandle(handle) };
    true
}

/// A Windows or macOS census that saw no native `claude` root cannot count
/// Claude Code sessions: the npm install runs as `node` there, and only its
/// argv — never read — says what it is. So instead of an absent class, which reads
/// as "measured, no session", it publishes a `claude_session` entry whose
/// `root_count`, `cpu_pct`, `proc_count` and `rss_bytes` are all `null`. With a native root seen, the class
/// is measured as everywhere else. PURE.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn claude_sessions_unknown_without_native_root(entries: &mut Vec<WorkloadEntry>) {
    let class = WorkloadClass::ClaudeSession.as_str();
    if entries.iter().any(|e| e.class == class && e.root_count.is_some_and(|n| n > 0)) {
        return;
    }
    let unknown = WorkloadEntry {
        class,
        cpu_pct: None,
        rss_bytes: None,
        gpu_mem_bytes: None,
        proc_count: None,
        root_count: None,
    };
    match entries.iter_mut().find(|e| e.class == class) {
        Some(e) => {
            e.cpu_pct = None;
            e.root_count = None;
        }
        // `claude_session` is the first class of the vocabulary, so the
        // entries stay in vocabulary order.
        None => entries.insert(0, unknown),
    }
}

/// Any other host takes no census: no collector here is trusted to leave
/// argv unread on it, and no census is UNKNOWN, never an empty one.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) struct HostCensus;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl HostCensus {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn take(&mut self, _vram_by_pid: Option<&HashMap<u32, u64>>) -> Option<Vec<WorkloadEntry>> {
        None
    }
}

/// The host census, or `None` when it is switched off or could not be taken.
/// Holds the long-lived state; a census switched off drops it, so switching
/// back on starts with a `null`-CPU tick rather than a CPU figure averaged
/// over the dark period.
pub(crate) fn host_workloads() -> Option<Vec<WorkloadEntry>> {
    static STATE: std::sync::Mutex<Option<HostCensus>> = std::sync::Mutex::new(None);
    let mut slot = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if !census_enabled() {
        *slot = None;
        return None;
    }
    slot.get_or_insert_with(HostCensus::new).take(None)
}

// ---------------------------------------------------------------------------
// WSL guest — rides the lane's single `wsl.exe` probe
// ---------------------------------------------------------------------------

// The section's literals are macros so the one script below can be spelled
// with `concat!`, and each still has one home.
macro_rules! guest_marker {
    () => {
        "@@qontinui-workload-census@@"
    };
}
macro_rules! guest_cgroup_marker {
    () => {
        "@@qontinui-workload-census-cgroup@@"
    };
}
macro_rules! guest_unit_marker {
    () => {
        "@@qontinui-workload-census-units@@"
    };
}
macro_rules! guest_rc_marker {
    () => {
        "@@qontinui-workload-census-rc@@"
    };
}
macro_rules! guest_stat_secs {
    () => {
        "2"
    };
}
macro_rules! guest_cgroup_secs {
    () => {
        "2"
    };
}
macro_rules! guest_unit_secs {
    () => {
        "1"
    };
}
macro_rules! guest_section_secs {
    () => {
        "6"
    };
}

/// Separates the probe's original procfs output from the census section.
const GUEST_MARKER: &str = guest_marker!();
/// Separates the census's `stat` lines from its `cgroup` files.
const GUEST_CGROUP_MARKER: &str = guest_cgroup_marker!();
/// Separates the `cgroup` files from the runner units' `cpu.stat` files.
const GUEST_UNIT_MARKER: &str = guest_unit_marker!();
/// Prefixes the exit status of each bounded step.
const GUEST_RC_MARKER: &str = guest_rc_marker!();

/// In-guest bound on reading every `/proc/<pid>/stat`, seconds.
const GUEST_STAT_SECS: &str = guest_stat_secs!();
/// In-guest bound on reading every `/proc/<pid>/cgroup`, seconds.
const GUEST_CGROUP_SECS: &str = guest_cgroup_secs!();
/// In-guest bound on finding and reading the runner units' `cpu.stat`, seconds.
const GUEST_UNIT_SECS: &str = guest_unit_secs!();
/// Allowance for the section's unbounded steps — starting `sh`, the two
/// `getconf`s, `head`, `grep` and the `echo`s — which read a handful of small
/// files and exit; seconds.
const GUEST_UNBOUNDED_SLACK_SECS: u64 = 1;
/// In-guest bound on the WHOLE section (`timeout -s KILL` around its `sh`):
/// the sum of the bounded steps plus [`GUEST_UNBOUNDED_SLACK_SECS`], so the
/// unbounded steps are bounded too, seconds.
const GUEST_SECTION_SECS: &str = guest_section_secs!();
/// The longest single bounded step, seconds. `timeout` runs its command in a
/// process group of its own, so a bounded read still running when the whole
/// section is killed survives that kill — holding the probe's stdout open —
/// until its own bound ends it.
const GUEST_LONGEST_STEP_SECS: u64 = 2;

/// The most the census section can add to the probe: the whole-section bound
/// ([`GUEST_SECTION_SECS`]) plus [`GUEST_LONGEST_STEP_SECS`] for a bounded
/// read that outlives it. The probe's own timeout grows by exactly this while
/// the census is on, so a census that runs long is killed in the guest — and
/// published as no census — before it can time out the probe and cost the
/// lane its memory and saturation readings. (A guest with no `timeout` runs
/// no section at all: no marker, no census.)
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const GUEST_CENSUS_BUDGET: Duration =
    Duration::from_secs(2 + 2 + 1 + GUEST_UNBOUNDED_SLACK_SECS + GUEST_LONGEST_STEP_SECS);

/// The census section, as ONE `sh -c` script.
///
/// `wsl.exe -d <distro> -- …` hands the rest of its command line to the
/// distro user's LOGIN shell, which may be fish or zsh, where `$?` and an
/// unmatched glob do not mean what they mean in `sh`. So the outer shell sees
/// only `; timeout -s KILL <secs> sh -c "<script>"`: Rust's Windows argument
/// quoting wraps this
/// space-carrying argument in double quotes, which every one of those shells
/// reads the same way — no glob, `;` or `'` inside them is touched — and the
/// one character they all expand there, `$`, is written `\$`, which all of
/// them turn back into `$`. Inside, POSIX `sh` alone interprets the script.
/// The script itself therefore contains no `"` and no other backslash.
///
/// It emits the page size, `USER_HZ`, the aggregate `cpu` line and the
/// `btime` line of `/proc/stat`, every `/proc/<pid>/stat` (each line starts
/// with its own pid, and its only string is the kernel's `comm`), every
/// `/proc/<pid>/cgroup` behind a `tail` header naming its pid, and the
/// `cpu.stat` of every GitHub Actions runner unit cgroup whose directory is up
/// to eight levels below `/sys/fs/cgroup` (`-maxdepth 9` counts the file), behind
/// a header naming its path. Never `cmdline` and never `environ`.
///
/// Each read is bounded in the guest by `timeout -s KILL`, and followed by its
/// exit status; a killed read (124 / 137), or one whose status never arrived
/// because the whole section was killed first, makes the section unusable
/// rather than a silent undercount. On a guest where this section does not
/// run, its marker is missing and the lane publishes no census — UNKNOWN,
/// never empty.
const GUEST_CENSUS_SCRIPT: &str = concat!(
    "echo ", guest_marker!(), "; ",
    "getconf PAGESIZE; getconf CLK_TCK; head -n 1 /proc/stat; grep btime /proc/stat; ",
    "timeout -s KILL ", guest_stat_secs!(), " cat /proc/[0-9]*/stat 2>/dev/null; ",
    "echo ", guest_rc_marker!(), " \\$?; ",
    "echo ", guest_cgroup_marker!(), "; ",
    "timeout -s KILL ", guest_cgroup_secs!(), " tail -v -n +1 /proc/[0-9]*/cgroup 2>/dev/null; ",
    "echo ", guest_rc_marker!(), " \\$?; ",
    "echo ", guest_unit_marker!(), "; ",
    "timeout -s KILL ", guest_unit_secs!(),
    " find /sys/fs/cgroup -maxdepth 9 -name cpu.stat -path '*/actions.runner.*.service/cpu.stat'",
    " -exec tail -v -n +1 {} + 2>/dev/null; ",
    "echo ", guest_rc_marker!(), " \\$?"
);

/// Shell words appended to the `wsl` lane's one probe when the census is on:
/// the census section ([`GUEST_CENSUS_SCRIPT`]) as one `sh -c` under the
/// whole-section bound ([`GUEST_SECTION_SECS`]), after the probe's own `cat`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const GUEST_PROBE_WORDS: &[&str] = &[
    ";",
    "timeout",
    "-s",
    "KILL",
    GUEST_SECTION_SECS,
    "sh",
    "-c",
    GUEST_CENSUS_SCRIPT,
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

/// Whether a bounded step of the section was killed by its `timeout` (124 on
/// TERM, 137 on KILL) — its output is then partial.
fn guest_step_killed(part: &str) -> bool {
    part.lines()
        .filter_map(|l| l.trim().strip_prefix(GUEST_RC_MARKER))
        .any(|rc| matches!(rc.trim(), "124" | "137"))
}

/// Whether a bounded step of the section ran to its end: its exit status
/// arrived (the whole section was not killed first) and its own `timeout`
/// did not kill it.
fn guest_step_complete(part: &str) -> bool {
    part.lines().any(|l| l.trim().starts_with(GUEST_RC_MARKER)) && !guest_step_killed(part)
}

/// `tail -v -n +1` output → (path inside the header, body) per file.
fn tail_blocks(text: &str) -> Vec<(&str, String)> {
    let mut blocks: Vec<(&str, String)> = Vec::new();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("==> ").and_then(|r| r.strip_suffix(" <==")) {
            blocks.push((header, String::new()));
        } else if let Some((_, body)) = blocks.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    blocks
}

/// The `cgroup` part → pid → unit. Each file follows a
/// `==> /proc/<pid>/cgroup <==` header.
fn parse_guest_cgroups(text: &str) -> HashMap<u32, String> {
    tail_blocks(text)
        .into_iter()
        .filter_map(|(path, body)| {
            let pid = path
                .strip_prefix("/proc/")?
                .strip_suffix("/cgroup")?
                .parse()
                .ok()?;
            Some((pid, unit_from_cgroup(&body)?))
        })
        .collect()
}

/// The units part → runner unit → `usage_usec`. Each file follows a
/// `==> /sys/fs/cgroup/…/<unit>/cpu.stat <==` header; a hybrid host lists a
/// unit under more than one hierarchy, and only the unified one carries
/// `usage_usec`.
fn parse_guest_unit_usage(text: &str) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    for (path, body) in tail_blocks(text) {
        let Some(unit) = path
            .strip_suffix("/cpu.stat")
            .and_then(|dir| dir.rsplit('/').next())
            .filter(|u| is_actions_runner_unit(u))
        else {
            continue;
        };
        if let Some(usage) = parse_usage_usec(&body) {
            out.entry(unit.to_string()).or_insert(usage);
        }
    }
    out
}

/// Parse one census section into a reading. `None` when the section is
/// unusable: no page size, no `cpu` line, or a process-table step that did
/// not complete. An incomplete unit step only forgoes the unit figures.
fn parse_guest_section(section: &str) -> Option<CpuTimeTick> {
    let (stats_part, rest) = section.split_once(GUEST_CGROUP_MARKER)?;
    let (cgroup_part, unit_part) = rest.split_once(GUEST_UNIT_MARKER).unwrap_or((rest, ""));
    if !guest_step_complete(stats_part) || !guest_step_complete(cgroup_part) {
        return None;
    }
    let mut lines = stats_part.lines().map(str::trim).filter(|l| !l.is_empty());
    let page_size: u64 = lines.next()?.parse().ok()?;
    // `USER_HZ` is optional: a guest whose `getconf` lacks it goes straight to
    // the `cpu` line.
    let next = lines.next()?;
    let (clk_tck, cpu_line) = match next.parse::<u64>() {
        Ok(hz) => (Some(hz).filter(|h| *h > 0), lines.next()?),
        Err(_) => (None, next),
    };
    let total_jiffies = parse_total_jiffies(cpu_line)?;
    let boot_time = stats_part.lines().find_map(parse_btime);
    let stats: Vec<ProcStat> = lines.filter_map(parse_proc_stat).collect();
    let unit_usage_usec = if guest_step_complete(unit_part) {
        parse_guest_unit_usage(unit_part)
    } else {
        HashMap::new()
    };
    Some(CpuTimeTick {
        page_size,
        clk_tck,
        boot_time,
        total_jiffies,
        stats,
        units: parse_guest_cgroups(cgroup_part),
        unit_usage_usec,
    })
}

/// The `wsl` lane's census state.
#[derive(Debug, Default)]
pub(crate) struct GuestCensus {
    cumulative: CpuTimeCensus,
}

impl GuestCensus {
    /// Aggregate one census section. `None` when the section is unusable — the
    /// lane then publishes no census, never a fabricated one — and the
    /// previous reading stays the baseline, so the next interval simply spans
    /// two ticks.
    pub(crate) fn take(&mut self, section: &str) -> Option<Vec<WorkloadEntry>> {
        self.cumulative.take(parse_guest_section(section)?, None)
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
            cpu_pct: Some(cpu),
            rss_bytes: Some(rss),
            kernel_thread: false,
        }
    }

    fn no_units() -> UnitCpu {
        UnitCpu::default()
    }

    fn unit_share(unit: &str, share: f64) -> UnitCpu {
        UnitCpu {
            share: HashMap::from([(unit.to_string(), share)]),
            busy_before: HashSet::new(),
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

    /// A container a job starts is the job's load wherever the table can tell
    /// — its shim under the worker, or in the busy runner's unit — and stays
    /// `docker` when only the system daemon's tree holds it.
    #[test]
    fn containers_a_job_runs_are_the_jobs() {
        use WorkloadClass::*;
        assert_eq!(
            classify(&view("postgres", &["containerd-shim", "podman", "bash", "runner.worker"], None)),
            GhActionsJob
        );
        let unit = Some("actions.runner.o-r.host-1.service");
        let mut in_unit = view("postgres", &["containerd-shim", "systemd"], unit);
        in_unit.ci_worker_present = true;
        assert_eq!(classify(&in_unit), GhActionsJob);
        assert_eq!(
            classify(&view("postgres", &["containerd-shim", "systemd"], Some("docker-abc.scope"))),
            Docker,
            "a system dockerd container has nothing argv-free tying it to the job"
        );
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
        let out = census(&table, true, None, &no_units());
        let get = |c: &str| out.iter().find(|e| e.class == c).cloned();
        let job = get("gh_actions_job").expect("job class");
        assert_eq!(job.proc_count, Some(6), "busy unit's 4 + the busy Windows pair");
        assert_eq!(job.root_count, Some(2), "one per Runner.Worker");
        let idle = get("gh_actions_runner_idle").expect("idle class");
        assert_eq!(idle.proc_count, Some(3));
        assert_eq!(idle.root_count, None, "an idle runner has no root to count");
        assert_eq!(get("other").expect("systemd").proc_count, Some(1));
    }

    /// `root_count` reconciles against the session ledger: one per Claude
    /// Code process with no Claude Code ancestor, however many processes each
    /// session runs.
    #[test]
    fn claude_roots_count_sessions_not_processes() {
        let table = vec![
            rec(1, 0, "systemd", None, 0.0, 1),
            rec(2, 1, "qontinui-runner", None, 0.0, 1),
            rec(3, 2, "claude", None, 1.0, 1),
            rec(4, 3, "bash", None, 1.0, 1),
            rec(5, 4, "node", None, 1.0, 1),
            // A nested `claude -p` a session runs is that session's, not a root.
            rec(6, 4, "claude", None, 1.0, 1),
            rec(7, 2, "claude", None, 1.0, 1),
        ];
        let out = census(&table, true, None, &no_units());
        let claude = out.iter().find(|e| e.class == "claude_session").expect("claude");
        assert_eq!(claude.proc_count, Some(5));
        assert_eq!(claude.root_count, Some(2));
        let runner = out.iter().find(|e| e.class == "qontinui_runner").expect("runner");
        assert_eq!(runner.root_count, None);
    }

    #[test]
    fn kernel_threads_count_cpu_but_not_processes() {
        let mut kworker = rec(3, 2, "kworker/0:1", None, 4.0, 0);
        kworker.kernel_thread = true;
        let mut kthreadd = rec(2, 0, "kthreadd", None, 0.0, 0);
        kthreadd.kernel_thread = true;
        let table = vec![kthreadd, kworker, rec(10, 1, "python3", None, 1.0, 5)];
        let out = census(&table, true, None, &no_units());
        let other = out.iter().find(|e| e.class == "other").expect("other");
        assert_eq!(other.proc_count, Some(1), "kernel threads are no workload process");
        assert_eq!(other.cpu_pct, Some(5.0), "but their CPU time is still load");
    }

    /// A runner unit's cgroup figure covers children that exited inside the
    /// tick; the class gets the larger of it and its live processes' sum.
    #[test]
    fn a_runner_units_cgroup_figure_covers_exited_children() {
        let unit = "actions.runner.o-r.host-1.service";
        let table = vec![
            rec(11, 1, "Runner.Listener", Some(unit), 1.0, 1),
            rec(12, 11, "Runner.Worker", Some(unit), 9.0, 1),
        ];
        let out = census(&table, true, None, &unit_share(unit, 60.0));
        let job = out.iter().find(|e| e.class == "gh_actions_job").expect("job");
        assert_eq!(job.cpu_pct, Some(60.0));
        // Never below what the processes themselves measured.
        let out = census(&table, true, None, &unit_share(unit, 4.0));
        assert_eq!(out.iter().find(|e| e.class == "gh_actions_job").and_then(|e| e.cpu_pct), Some(10.0));
        // A unit whose processes span classes cannot place its surplus.
        let mixed_unit = "qontinui-runner.service";
        let mixed = vec![
            rec(2, 1, "qontinui-runner", Some(mixed_unit), 1.0, 1),
            rec(3, 2, "claude", Some(mixed_unit), 1.0, 1),
        ];
        let out = census(&mixed, true, None, &unit_share(mixed_unit, 50.0));
        let total: f64 = out.iter().filter_map(|e| e.cpu_pct).sum();
        assert_eq!(total, 2.0);
    }

    /// A job that ended inside the interval leaves an idle listener behind;
    /// its exited children's CPU is still the job's, never idle overhead.
    #[test]
    fn an_idle_units_surplus_is_a_jobs_when_a_job_ran_in_the_interval() {
        let unit = "actions.runner.o-r.host-1.service";
        let idle = vec![rec(11, 1, "Runner.Listener", Some(unit), 0.1, 1)];
        let class_cpu = |out: &[WorkloadEntry], c: &str| {
            out.iter().find(|e| e.class == c).and_then(|e| e.cpu_pct)
        };

        // The unit had a worker on the previous tick: even a small surplus is
        // the job's tail.
        let mut after_job = unit_share(unit, 0.4);
        after_job.busy_before.insert(unit.to_string());
        let out = census(&idle, true, None, &after_job);
        assert_eq!(class_cpu(&out, "gh_actions_job"), Some(0.3));
        assert_eq!(class_cpu(&out, "gh_actions_runner_idle"), Some(0.1));
        let job = out.iter().find(|e| e.class == "gh_actions_job").expect("job");
        assert_eq!(job.proc_count, Some(0), "no job process is alive at the reading");

        // No worker seen at either end, but more than a listener spends: a
        // job started and finished inside the interval.
        let out = census(&idle, true, None, &unit_share(unit, 25.1));
        assert_eq!(class_cpu(&out, "gh_actions_job"), Some(25.0));
        assert_eq!(class_cpu(&out, "gh_actions_runner_idle"), Some(0.1));

        // Listener overhead stays idle.
        let out = census(&idle, true, None, &unit_share(unit, 0.3));
        assert_eq!(class_cpu(&out, "gh_actions_job"), None);
        assert_eq!(class_cpu(&out, "gh_actions_runner_idle"), Some(0.3));
    }

    /// The previous tick's busyness is carried by the accounting itself.
    #[test]
    fn the_guest_census_remembers_which_units_ran_a_job() {
        let unit = "actions.runner.o-r.spaceship-1.service";
        let cg = |pids: &[u32]| {
            pids.iter()
                .map(|p| format!("==> /proc/{p}/cgroup <==\n0::/system.slice/{unit}\n\n"))
                .collect::<String>()
        };
        let usage = |usec: u64| {
            format!("==> /sys/fs/cgroup/system.slice/{unit}/cpu.stat <==\nusage_usec {usec}\n")
        };
        let mut g = GuestCensus::default();
        g.take(&guest_section(
            [0, 0, 0, 1000, 0, 0, 0, 0],
            &[(1, 0, "init", 0, 1), (2, 1, "Runner.Listener", 10, 2), (3, 2, "Runner.Worker", 0, 3)],
            &cg(&[2, 3]),
            &usage(1_000_000),
        ))
        .expect("first tick");
        g.take(&guest_section(
            [100, 0, 0, 1900, 0, 0, 0, 0],
            &[(1, 0, "init", 0, 1), (2, 1, "Runner.Listener", 10, 2), (3, 2, "Runner.Worker", 100, 3)],
            &cg(&[2, 3]),
            &usage(2_000_000),
        ))
        .expect("busy tick");
        // The worker exited inside this interval; the unit's cgroup kept the
        // 3 jiffies it burned before going.
        let third = g
            .take(&guest_section(
                [103, 0, 0, 2897, 0, 0, 0, 0],
                &[(1, 0, "init", 0, 1), (2, 1, "Runner.Listener", 10, 2)],
                &cg(&[2]),
                &usage(2_030_000),
            ))
            .expect("third tick");
        let job = third.iter().find(|e| e.class == "gh_actions_job").expect("job tail");
        assert_eq!(job.cpu_pct, Some(0.3));
        let idle = third.iter().find(|e| e.class == "gh_actions_runner_idle").expect("idle");
        assert_eq!(idle.cpu_pct, Some(0.0));
    }

    /// A rebooted guest restarts every counter — its new jiffy total can even
    /// exceed the old one — so a changed `btime` drops every baseline.
    #[test]
    fn a_rebooted_guest_starts_over_with_unknown_cpu() {
        let procs: &[(u32, u32, &str, u64, u64)] = &[(1, 0, "init", 0, 1), (50, 1, "rustc", 10, 100)];
        let mut g = GuestCensus::default();
        g.take(&guest_section([0, 0, 0, 1000, 0, 0, 0, 0], procs, "", ""))
            .expect("first tick");
        let rebooted = guest_section(
            [500, 0, 0, 5000, 0, 0, 0, 0],
            &[(1, 0, "init", 0, 1), (50, 1, "rustc", 400, 100)],
            "",
            "",
        )
        .replacen("btime 1700000000", "btime 1700009999", 1);
        let after = g.take(&rebooted).expect("tick after reboot");
        assert!(after.iter().all(|e| e.cpu_pct.is_none()), "{after:?}");
        // The reading after that diffs against the new boot only.
        let next = g
            .take(
                &guest_section(
                    [600, 0, 0, 5900, 0, 0, 0, 0],
                    &[(1, 0, "init", 0, 1), (50, 1, "rustc", 500, 100)],
                    "",
                    "",
                )
                .replacen("btime 1700000000", "btime 1700009999", 1),
            )
            .expect("second tick of the new boot");
        let build = next.iter().find(|e| e.class == "cargo_build").expect("build");
        assert_eq!(build.cpu_pct, Some(10.0));
    }

    #[test]
    fn btime_parses_from_its_own_line_only() {
        assert_eq!(parse_btime("btime 1700000000"), Some(1_700_000_000));
        assert_eq!(parse_btime("cpu  1 2 3"), None);
        assert_eq!(parse_btime("btime x"), None);
    }

    /// Where an npm Claude Code runs as `node`, a census with no native root
    /// says "unknown", never "no session"; a native root is counted as ever.
    #[test]
    fn sessions_are_unknown_without_a_native_claude_root() {
        // An npm session: `node` with a shell under it. Nothing says Claude.
        let npm = vec![rec(10, 1, "node.exe", None, 3.0, 100), rec(11, 10, "bash.exe", None, 1.0, 10)];
        let mut out = census(&npm, true, None, &no_units());
        claude_sessions_unknown_without_native_root(&mut out);
        let claude = &out[0];
        assert_eq!(claude.class, "claude_session", "kept in vocabulary order: {out:?}");
        assert_eq!(claude.root_count, None, "never 0 while a session may be running");
        assert_eq!(claude.cpu_pct, None);
        assert_eq!(claude.proc_count, None, "unmeasured, never 0");
        assert_eq!(claude.rss_bytes, None, "unmeasured, never 0");
        assert_eq!(out.iter().filter(|e| e.class == "claude_session").count(), 1);
        let v = serde_json::to_value(&out).expect("serializes");
        for field in ["cpu_pct", "root_count", "proc_count", "rss_bytes"] {
            assert!(v[0][field].is_null(), "{field} is UNKNOWN: null on the wire, never 0");
        }
        // The measured `other` beside it still sends integers.
        let other = v.as_array().expect("array").iter().find(|e| e["class"] == "other").expect("other");
        assert_eq!(other["proc_count"], serde_json::json!(2));
        assert_eq!(other["rss_bytes"], serde_json::json!(110));

        // A native root is measured as everywhere else.
        let native = vec![rec(20, 1, "claude.exe", None, 2.0, 50), rec(21, 20, "git.exe", None, 1.0, 5)];
        let mut out = census(&native, true, None, &no_units());
        let before = out.clone();
        claude_sessions_unknown_without_native_root(&mut out);
        assert_eq!(out, before);
        assert_eq!(out[0].root_count, Some(1));
        assert_eq!(out[0].cpu_pct, Some(3.0));
    }

    #[test]
    fn other_is_always_reported_with_its_share() {
        let table = vec![
            rec(1, 0, "init", None, 2.0, 100),
            rec(2, 1, "claude", None, 10.0, 1000),
            rec(3, 1, "mystery-daemon", None, 40.0, 5000),
        ];
        let out = census(&table, true, None, &no_units());
        let other = out.iter().find(|e| e.class == "other").expect("other");
        assert_eq!(other.proc_count, Some(2));
        assert_eq!(other.cpu_pct, Some(42.0));
        assert_eq!(other.rss_bytes, Some(5100));
        let claude = out.iter().find(|e| e.class == "claude_session").expect("claude");
        assert_eq!(claude.cpu_pct, Some(10.0));
        // Empty classes are not listed at all.
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn the_first_tick_is_unknown_cpu_not_zero() {
        let table = vec![rec(1, 0, "claude", None, 0.0, 10)];
        let out = census(&table, false, None, &no_units());
        assert_eq!(out[0].cpu_pct, None);
        let v = serde_json::to_value(&out).expect("serializes");
        assert!(v[0]["cpu_pct"].is_null(), "UNKNOWN is null on the wire, never 0");
    }

    #[test]
    fn the_wire_shape_is_coords() {
        let out = census(&[rec(1, 0, "cargo", None, 12.346, 7)], true, None, &no_units());
        let v = serde_json::to_value(&out).expect("serializes");
        let obj = v[0].as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["class", "cpu_pct", "gpu_mem_bytes", "proc_count", "root_count", "rss_bytes"]
        );
        assert_eq!(v[0]["class"], "cargo_build");
        assert_eq!(v[0]["cpu_pct"], 12.35);
        assert!(v[0]["gpu_mem_bytes"].is_null(), "no VRAM measurement is UNKNOWN");
        assert!(v[0]["root_count"].is_null(), "a build has no root to count");
        assert!(v[0]["proc_count"].is_u64(), "a measured count is a JSON integer");
        assert_eq!(v[0]["proc_count"], 1);
        assert!(v[0]["rss_bytes"].is_u64(), "measured memory is a JSON integer");
        assert_eq!(v[0]["rss_bytes"], 7);
    }

    #[test]
    fn a_vram_measurement_fills_every_class_including_zero() {
        let table = vec![
            rec(1, 0, "llama-server", None, 1.0, 1),
            rec(2, 0, "claude", None, 1.0, 1),
        ];
        let vram = HashMap::from([(1, 8_000_000_000u64)]);
        let out = census(&table, true, Some(&vram), &no_units());
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
        let out = census(&table, true, None, &no_units());
        assert!(out.len() <= MAX_WORKLOAD_ENTRIES);
        assert_eq!(out.len(), WorkloadClass::ALL.len());
        let total: u32 = out.iter().map(|e| e.proc_count.expect("measured")).sum();
        assert_eq!(total, 2000, "every process lands in exactly one class");
    }

    /// The census input has nowhere to put an argument, and the published JSON
    /// carries only class names and numbers. Even argv-shaped text arriving as
    /// a process NAME — the one string field — and a token-shaped `comm` and
    /// cgroup path on the guest path never reach the wire.
    #[test]
    fn no_argument_or_token_reaches_the_wire() {
        const TOKEN: &str = "ghp_FAKEtoken0123456789abcdef";
        let argv = format!("curl -H 'Authorization: Bearer {TOKEN}' https://x");
        let table = vec![
            rec(1, 0, &argv, None, 5.0, 10),
            rec(2, 1, "claude", Some("qontinui-runner.service"), 5.0, 10),
            rec(3, 2, "bash", Some(format!("{TOKEN}.scope").as_str()), 5.0, 10),
        ];
        let out = census(&table, true, None, &no_units());
        assert_eq!(out.len(), 2, "the argv-named process is `other`, nothing more");
        let json = serde_json::to_string(&out).expect("serializes");
        assert!(!json.contains(TOKEN), "{json}");
        assert!(!json.contains("curl"), "process names never leave the host either");
        assert!(!json.contains("Bearer"), "{json}");

        // Guest path: a stat line whose comm is token-shaped.
        let section = format!(
            "\n4096\n100\ncpu  100 0 100 800 0 0 0 0 0 0\n\
             7 ({TOKEN}) S 1 7 7 0 -1 0 0 0 0 0 5 5 0 0 20 0 1 0 100 1000 10\n\
             {GUEST_RC_MARKER} 1\n\
             {GUEST_CGROUP_MARKER}\n==> /proc/7/cgroup <==\n0::/user.slice/{TOKEN}.scope\n\
             {GUEST_RC_MARKER} 0\n{GUEST_UNIT_MARKER}\n{GUEST_RC_MARKER} 1\n"
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

    /// The unit's cgroup path ends AT the unit, even for a process in a
    /// sub-cgroup of it, and only the unified hierarchy has one to read.
    #[test]
    fn a_units_cgroup_path_is_the_v2_prefix_ending_at_it() {
        assert_eq!(
            unit_placement("0::/system.slice/actions.runner.o-r.h-1.service/job/inner\n"),
            Some((
                "actions.runner.o-r.h-1.service".to_string(),
                Some("/system.slice/actions.runner.o-r.h-1.service".to_string())
            ))
        );
        assert_eq!(
            unit_placement("1:name=systemd:/system.slice/docker.service\n"),
            Some(("docker.service".to_string(), None))
        );
        assert_eq!(parse_usage_usec("usage_usec 1234\nuser_usec 1000\n"), Some(1234));
        assert_eq!(parse_usage_usec("nr_periods 0\n"), None);
    }

    #[test]
    fn stat_lines_parse_comm_with_spaces_and_parens() {
        let s = parse_proc_stat("42 (tmux: server) (x)) S 1 42 42 0 -1 0 0 0 0 0 300 200 0 0 20 0 1 0 5555 100000 250")
            .expect("parses");
        assert_eq!(s.pid, 42);
        assert_eq!(s.ppid, 1);
        assert_eq!(s.comm, "tmux: server) (x)");
        assert_eq!(s.cpu_ticks, Some(500));
        assert_eq!(s.start_time, Some(5555));
        assert_eq!(s.rss_pages, Some(250));
        assert!(!s.kernel_thread);
        assert!(parse_proc_stat("garbage").is_none());
        // flags carries PF_KTHREAD (0x00208040 = 2129984).
        let k = parse_proc_stat("9 (kworker/0:1) I 2 0 0 0 -1 2129984 0 0 0 0 0 7 0 0 20 0 1 0 30 0 0")
            .expect("parses");
        assert!(k.kernel_thread);
    }

    fn pstat(pid: u32, comm: &str, start: Option<u64>, cpu: Option<u64>, rss: Option<u64>) -> ProcStat {
        ProcStat {
            pid,
            ppid: 1,
            comm: comm.to_string(),
            start_time: start,
            cpu_ticks: cpu,
            rss_pages: rss,
            kernel_thread: false,
            zombie: false,
        }
    }

    fn cpu_tick(total: u64, stats: Vec<ProcStat>) -> CpuTimeTick {
        CpuTimeTick {
            page_size: 1,
            total_jiffies: total,
            stats,
            ..CpuTimeTick::default()
        }
    }

    /// One unreadable member makes its class's figure UNKNOWN — never a sum
    /// over the readable members only — and leaves other classes alone.
    #[test]
    fn an_unreadable_member_makes_its_classs_figure_unknown() {
        let mut t = ClassTotal::default();
        t.add(Some(10.0), Some(100));
        assert_eq!((t.cpu_pct(true), t.rss_bytes()), (Some(10.0), Some(100)));
        assert_eq!(t.cpu_pct(false), None, "no interval, no share");
        t.add(None, Some(50));
        assert_eq!((t.cpu_pct(true), t.rss_bytes()), (None, Some(150)));
        t.add(Some(1.0), None);
        assert_eq!((t.cpu_pct(true), t.rss_bytes()), (None, None));

        let mut root_daemon = rec(10, 1, "launchd-helper", None, 0.0, 0);
        root_daemon.cpu_pct = None;
        root_daemon.rss_bytes = None;
        let out = census(
            &[rec(1, 0, "init", None, 2.0, 10), root_daemon, rec(20, 1, "rustc", None, 30.0, 500)],
            true,
            None,
            &no_units(),
        );
        let other = out.iter().find(|e| e.class == "other").expect("other");
        assert_eq!((other.cpu_pct, other.rss_bytes), (None, None));
        assert_eq!(other.proc_count, Some(2), "an unreadable process is still counted");
        let build = out.iter().find(|e| e.class == "cargo_build").expect("build");
        assert_eq!((build.cpu_pct, build.rss_bytes), (Some(30.0), Some(500)));
    }

    /// A unit holding a process with no readable CPU has no measurable
    /// surplus: none is poured into the class.
    #[test]
    fn a_unit_with_an_unreadable_member_adds_no_surplus() {
        let unit = "actions.runner.o-r.x.service";
        let mut worker = rec(3, 2, "Runner.Worker", Some(unit), 0.0, 1);
        worker.cpu_pct = None;
        let out = census(
            &[rec(2, 1, "Runner.Listener", Some(unit), 1.0, 1), worker],
            true,
            None,
            &unit_share(unit, 40.0),
        );
        let job = out.iter().find(|e| e.class == "gh_actions_job").expect("job");
        assert_eq!(job.cpu_pct, None);
        assert!(!out.iter().any(|e| e.cpu_pct.is_some_and(|c| c > 1.0)), "{out:?}");
    }

    /// A process alive but unreadable leaves no baseline, so when it becomes
    /// readable it is NOT credited its whole lifetime as if it had just
    /// started; one interval later it is measured normally.
    #[test]
    fn an_unread_cpu_time_is_never_a_baseline() {
        let mut c = CpuTimeCensus::default();
        c.take(cpu_tick(1000, vec![pstat(1, "init", Some(1), Some(0), Some(1)), pstat(7, "rustc", Some(5), None, None)]), None)
            .expect("first tick");
        let second = c
            .take(
                cpu_tick(2000, vec![pstat(1, "init", Some(1), Some(0), Some(1)), pstat(7, "rustc", Some(5), Some(900), Some(4))]),
                None,
            )
            .expect("second tick");
        let build = second.iter().find(|e| e.class == "cargo_build").expect("build");
        assert_eq!(build.cpu_pct, None, "no baseline: UNKNOWN, never a 90% lifetime spike");
        assert_eq!(build.rss_bytes, Some(4));
        let third = c
            .take(
                cpu_tick(3000, vec![pstat(1, "init", Some(1), Some(0), Some(1)), pstat(7, "rustc", Some(5), Some(1000), Some(4))]),
                None,
            )
            .expect("third tick");
        let build = third.iter().find(|e| e.class == "cargo_build").expect("build");
        assert_eq!(build.cpu_pct, Some(10.0));
    }

    /// A process identified but not dated (macOS short info) has no CPU
    /// accounting, and a dated process under its pid next tick may be the
    /// same one, so it is not taken as new either.
    #[test]
    fn an_undated_process_is_counted_without_cpu() {
        let mut c = CpuTimeCensus::default();
        c.take(cpu_tick(1000, vec![pstat(1, "init", Some(1), Some(0), Some(1)), pstat(9, "rustc", None, None, None)]), None)
            .expect("first tick");
        let second = c
            .take(cpu_tick(2000, vec![pstat(1, "init", Some(1), Some(0), Some(1)), pstat(9, "rustc", None, None, None)]), None)
            .expect("second tick");
        let build = second.iter().find(|e| e.class == "cargo_build").expect("build");
        assert_eq!((build.cpu_pct, build.rss_bytes, build.proc_count), (None, None, Some(1)));
        let other = second.iter().find(|e| e.class == "other").expect("init");
        assert_eq!(other.cpu_pct, Some(0.0), "readable classes are unaffected");
        let third = c
            .take(cpu_tick(3000, vec![pstat(1, "init", Some(1), Some(0), Some(1)), pstat(9, "rustc", Some(8), Some(800), Some(2))]), None)
            .expect("third tick");
        let build = third.iter().find(|e| e.class == "cargo_build").expect("build");
        assert_eq!(build.cpu_pct, None, "possibly the undated process: no lifetime credit");
    }

    /// xnu refuses a zombie's task info only because it has no task left: it
    /// is `0` CPU and `0` bytes, never UNKNOWN; a live process's refusal is
    /// UNKNOWN; and zombie status wins over whatever the task flavor said.
    #[test]
    fn a_zombie_reads_as_zero_not_unknown() {
        const SRUN: u32 = 2;
        assert_eq!(macos_task_reading(SZOMB, None), (None, Some(0), true));
        assert_eq!(macos_task_reading(SZOMB, Some((900, 4096))), (None, Some(0), true));
        assert_eq!(macos_task_reading(SRUN, Some((900, 4096))), (Some(900), Some(4096), false));
        assert_eq!(macos_task_reading(SRUN, None), (None, None, false));
    }

    fn zombie(pid: u32, comm: &str, start: Option<u64>) -> ProcStat {
        let (cpu, rss, zombie) = macos_task_reading(SZOMB, None);
        ProcStat {
            zombie,
            ..pstat(pid, comm, start, cpu, rss)
        }
    }

    /// An unreaped zombie leaves its class measured: `0` over every interval,
    /// with its baseline carried forward (so nothing it did before dying is
    /// lost or re-counted) — and, with no baseline to carry, none invented, so
    /// it can never be credited a lifetime.
    #[test]
    fn a_zombie_keeps_its_class_measured_and_never_spikes() {
        let mut c = CpuTimeCensus::default();
        let init = || pstat(1, "init", Some(1), Some(0), Some(1));
        c.take(cpu_tick(1000, vec![init(), pstat(7, "rustc", Some(5), Some(500), Some(4))]), None)
            .expect("first tick");
        for total in [2000, 3000] {
            let out = c.take(cpu_tick(total, vec![init(), zombie(7, "rustc", Some(5))]), None).expect("zombie tick");
            let build = out.iter().find(|e| e.class == "cargo_build").expect("build");
            assert_eq!((build.cpu_pct, build.rss_bytes, build.proc_count), (Some(0.0), Some(0), Some(1)));
        }
        let key = (7, 5);
        assert_eq!(c.prev.as_ref().and_then(|p| p.ticks.get(&key).copied()), Some(Some(500)), "baseline carried");

        // A zombie first seen with no baseline: 0 now, no baseline after.
        let first = zombie(8, "rustc", Some(6));
        assert_eq!(interval_cpu_ticks(c.prev.as_ref().expect("prev"), &first), Some(0));
        c.take(cpu_tick(4000, vec![init(), first]), None).expect("tick");
        assert_eq!(c.prev.as_ref().and_then(|p| p.ticks.get(&(8, 6)).copied()), Some(None), "alive, no baseline");
        let reread = pstat(8, "rustc", Some(6), Some(900), Some(4));
        assert_eq!(interval_cpu_ticks(c.prev.as_ref().expect("prev"), &reread), None, "never a lifetime");

        // An undated zombie (another user's) is still 0, not UNKNOWN.
        let undated = zombie(9, "rustc", None);
        assert_eq!(interval_cpu_ticks(c.prev.as_ref().expect("prev"), &undated), Some(0));
    }

    #[test]
    fn only_a_real_restart_drops_the_baselines() {
        let b = 1_700_000_000;
        assert!(!counters_restarted(Some(b), 100, Some(b), 200));
        assert!(!counters_restarted(Some(b), 100, Some(b + 1), 200), "a clock step is no reboot");
        assert!(!counters_restarted(Some(b), 100, Some(b - 5), 200));
        assert!(counters_restarted(Some(b), 100, Some(b + 6), 200));
        assert!(counters_restarted(Some(b), 100, Some(b), 50), "the jiffy total went backwards");
        assert!(counters_restarted(Some(b), 100, None, 200));
        assert!(counters_restarted(None, 100, Some(b), 200));
        assert!(!counters_restarted(None, 100, None, 200));
    }

    /// A section killed whole mid-step lacks that step's exit status: a
    /// truncated process table or cgroup list is no census, a truncated unit
    /// list only forgoes the unit figures.
    #[test]
    fn a_step_whose_status_never_arrived_is_incomplete() {
        let truncated_cgroups = format!(
            "\n4096\n100\ncpu  1 2 3 4 5 6 7 8\n1 (init) S 0 1 1 0 -1 0 0 0 0 0 5 5 0 0 20 0 1 0 1 1000 10\n\
             {GUEST_RC_MARKER} 0\n{GUEST_CGROUP_MARKER}\n==> /proc/1/cgroup <==\n0::/init.scope\n"
        );
        assert!(parse_guest_section(&truncated_cgroups).is_none());
        let unit = "actions.runner.o-r.x.service";
        let truncated_units = format!(
            "\n4096\n100\ncpu  1 2 3 4 5 6 7 8\n1 (init) S 0 1 1 0 -1 0 0 0 0 0 5 5 0 0 20 0 1 0 1 1000 10\n\
             {GUEST_RC_MARKER} 0\n{GUEST_CGROUP_MARKER}\n{GUEST_RC_MARKER} 0\n{GUEST_UNIT_MARKER}\n\
             ==> /sys/fs/cgroup/system.slice/{unit}/cpu.stat <==\nusage_usec 5\n"
        );
        let tick = parse_guest_section(&truncated_units).expect("tick");
        assert!(tick.unit_usage_usec.is_empty());
        let complete = format!("{truncated_units}{GUEST_RC_MARKER} 0\n");
        let tick = parse_guest_section(&complete).expect("tick");
        assert_eq!(tick.unit_usage_usec.get(unit), Some(&5));
    }

    fn guest_section(
        total: [u64; 8],
        procs: &[(u32, u32, &str, u64, u64)],
        cgroups: &str,
        units: &str,
    ) -> String {
        let cpu = total.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
        let mut s = format!("\n4096\n100\ncpu  {cpu} 0 0\nbtime 1700000000\n");
        for (pid, ppid, comm, ticks, start) in procs {
            s.push_str(&format!(
                "{pid} ({comm}) S {ppid} 0 0 0 -1 0 0 0 0 0 {ticks} 0 0 0 20 0 1 0 {start} 0 10\n"
            ));
        }
        s.push_str(&format!("{GUEST_RC_MARKER} 1\n{GUEST_CGROUP_MARKER}\n{cgroups}"));
        s.push_str(&format!("{GUEST_RC_MARKER} 0\n{GUEST_UNIT_MARKER}\n{units}{GUEST_RC_MARKER} 0\n"));
        s
    }

    #[test]
    fn the_guest_census_is_unknown_cpu_then_a_measured_share() {
        let unit = "actions.runner.o-r.spaceship-1.service";
        let cg = format!(
            "==> /proc/2/cgroup <==\n0::/system.slice/{unit}\n\n==> /proc/3/cgroup <==\n0::/system.slice/{unit}\n\n==> /proc/4/cgroup <==\n0::/system.slice/{unit}\n"
        );
        let usage = |usec: u64| {
            format!("==> /sys/fs/cgroup/system.slice/{unit}/cpu.stat <==\nusage_usec {usec}\nuser_usec 0\n")
        };
        let mut g = GuestCensus::default();
        let first = g
            .take(&guest_section(
                [0, 0, 0, 1000, 0, 0, 0, 0],
                &[(1, 0, "init", 0, 1), (2, 1, "Runner.Listener", 10, 2), (3, 2, "Runner.Worker", 0, 3)],
                &cg,
                &usage(1_000_000),
            ))
            .expect("first tick");
        assert!(first.iter().all(|e| e.cpu_pct.is_none()), "{first:?}");

        // 1000 jiffies later: the worker and a new rustc (started this
        // interval, so all its ticks count) burn 400 + 100 of them; the unit's
        // cgroup saw 6 s = 600 jiffies, 100 of them in a child already gone.
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
                &usage(7_000_000),
            ))
            .expect("second tick");
        let job = second.iter().find(|e| e.class == "gh_actions_job").expect("job");
        assert_eq!(job.cpu_pct, Some(60.0), "the unit's figure, exited child included");
        assert_eq!(job.proc_count, Some(3), "listener, worker and the rustc under it");
        assert_eq!(job.root_count, Some(1));
        assert_eq!(job.rss_bytes, Some(3 * 10 * 4096));
        let other = second.iter().find(|e| e.class == "other").expect("init");
        assert_eq!(other.cpu_pct, Some(0.0));
    }

    /// A short-lived process seen once is counted in full: everything it did
    /// happened inside the interval it first appeared in.
    #[test]
    fn a_process_first_seen_is_counted_whole() {
        let mut g = GuestCensus::default();
        g.take(&guest_section([0, 0, 0, 1000, 0, 0, 0, 0], &[(1, 0, "init", 0, 1)], "", ""))
            .expect("first tick");
        let second = g
            .take(&guest_section(
                [300, 0, 0, 1700, 0, 0, 0, 0],
                &[(1, 0, "init", 0, 1), (50, 1, "rustc", 250, 1200)],
                "",
                "",
            ))
            .expect("second tick");
        let build = second.iter().find(|e| e.class == "cargo_build").expect("build");
        assert_eq!(build.cpu_pct, Some(25.0));
    }

    #[test]
    fn a_missing_killed_or_unusable_guest_section_is_no_census() {
        let (head, census) = split_guest_output("MemTotal: 1 kB\n");
        assert_eq!(head, "MemTotal: 1 kB\n");
        assert!(census.is_none());

        let probe = format!(
            "MemTotal: 1 kB\n{GUEST_MARKER}\n4096\n100\ncpu  1 2 3 4 5 6 7 8\n{GUEST_RC_MARKER} 0\n\
             {GUEST_CGROUP_MARKER}\n{GUEST_RC_MARKER} 0\n{GUEST_UNIT_MARKER}\n"
        );
        let (head, census) = split_guest_output(&probe);
        assert_eq!(head, "MemTotal: 1 kB\n");
        assert!(
            GuestCensus::default().take(census.expect("section")).is_none(),
            "a section with no process is UNKNOWN, never an empty census"
        );

        // A process-table read killed by its in-guest bound is partial: no census.
        let killed = guest_section([0, 0, 0, 1000, 0, 0, 0, 0], &[(1, 0, "init", 0, 1)], "", "")
            .replacen(&format!("{GUEST_RC_MARKER} 1"), &format!("{GUEST_RC_MARKER} 137"), 1);
        assert!(GuestCensus::default().take(&killed).is_none());

        // A killed unit read only forgoes the unit figures.
        let units_killed = guest_section([0, 0, 0, 1000, 0, 0, 0, 0], &[(1, 0, "init", 0, 1)], "", "")
            .replacen(&format!("{GUEST_UNIT_MARKER}\n{GUEST_RC_MARKER} 0"), &format!("{GUEST_UNIT_MARKER}\n{GUEST_RC_MARKER} 124"), 1);
        assert!(GuestCensus::default().take(&units_killed).is_some());
    }

    #[test]
    fn a_guest_without_user_hz_still_takes_a_census() {
        let section = format!(
            "\n4096\ncpu  1 2 3 4 5 6 7 8\n1 (init) S 0 1 1 0 -1 0 0 0 0 0 5 5 0 0 20 0 1 0 1 1000 10\n\
             {GUEST_RC_MARKER} 0\n{GUEST_CGROUP_MARKER}\n{GUEST_RC_MARKER} 0\n{GUEST_UNIT_MARKER}\n"
        );
        let tick = parse_guest_section(&section).expect("tick");
        assert_eq!(tick.clk_tck, None);
        assert_eq!(tick.stats.len(), 1);
    }

    #[test]
    fn guest_unit_usage_prefers_the_unified_hierarchy() {
        let text = "==> /sys/fs/cgroup/cpu,cpuacct/system.slice/actions.runner.a.b.service/cpu.stat <==\n\
                    nr_periods 0\n\n\
                    ==> /sys/fs/cgroup/unified/system.slice/actions.runner.a.b.service/cpu.stat <==\n\
                    usage_usec 42\n\n\
                    ==> /sys/fs/cgroup/system.slice/not-a-runner.service/cpu.stat <==\nusage_usec 7\n";
        let u = parse_guest_unit_usage(text);
        assert_eq!(u.get("actions.runner.a.b.service"), Some(&42));
        assert_eq!(u.len(), 1);
    }

    #[test]
    fn the_guest_probe_never_reads_argv_or_environment() {
        for w in GUEST_PROBE_WORDS {
            assert!(!w.contains("cmdline") && !w.contains("environ") && !w.contains("cwd"), "{w}");
        }
    }

    /// The whole section is killed at its steps' bounds plus the slack for its
    /// unbounded steps; the probe's timeout grows by that plus the longest
    /// step (which can outlive the kill), and every bounded step is followed
    /// by its exit status.
    #[test]
    fn the_guest_census_budget_is_the_sum_of_its_bounds_and_slack() {
        let bounds = [GUEST_STAT_SECS, GUEST_CGROUP_SECS, GUEST_UNIT_SECS];
        let parsed: Vec<u64> = bounds
            .iter()
            .map(|s| s.parse::<u64>().expect("whole seconds"))
            .collect();
        let secs: u64 = parsed.iter().sum();
        let section: u64 = GUEST_SECTION_SECS.parse().expect("whole seconds");
        assert_eq!(section, secs + GUEST_UNBOUNDED_SLACK_SECS);
        assert_eq!(Some(&GUEST_LONGEST_STEP_SECS), parsed.iter().max());
        assert_eq!(GUEST_CENSUS_BUDGET, Duration::from_secs(section + GUEST_LONGEST_STEP_SECS));
        assert_eq!(
            GUEST_PROBE_WORDS[..7],
            [";", "timeout", "-s", "KILL", GUEST_SECTION_SECS, "sh", "-c"],
            "the whole section runs under its own bound"
        );
        assert_eq!(GUEST_CENSUS_SCRIPT.matches("timeout -s KILL ").count(), bounds.len());
        for b in bounds {
            assert!(GUEST_CENSUS_SCRIPT.contains(&format!("timeout -s KILL {b} ")));
        }
        assert_eq!(
            GUEST_CENSUS_SCRIPT.matches(&format!("{GUEST_RC_MARKER} \\$?")).count(),
            bounds.len()
        );
    }

    /// The outer (login) shell must hand the script to `sh` untouched: no
    /// `"` (Rust would escape it), no backslash but the `\$` every outer
    /// shell turns into `$`, and spaces in it so it is quoted at all.
    #[test]
    fn the_guest_script_survives_any_login_shell() {
        let (script, words) = GUEST_PROBE_WORDS.split_last().expect("words");
        assert_eq!(*script, GUEST_CENSUS_SCRIPT, "the script is the last word");
        for w in words {
            assert!(!w.contains([' ', '\t', '"', '$', '\\', '\'', '*']), "{w}: quoted or expanded");
        }
        assert!(!GUEST_CENSUS_SCRIPT.contains('"'));
        assert_eq!(
            GUEST_CENSUS_SCRIPT.matches('\\').count(),
            GUEST_CENSUS_SCRIPT.matches("\\$").count(),
            "every backslash escapes a `$`"
        );
        assert!(!GUEST_CENSUS_SCRIPT.ends_with('\\'));
        assert!(GUEST_CENSUS_SCRIPT.contains(' '), "Rust quotes only an argument with a space");
        assert!(
            !GUEST_CENSUS_SCRIPT.replace("\\$?", "").contains('$'),
            "no unescaped `$` for the login shell to expand"
        );
    }

    /// The command line `wsl.exe` forwards, as Rust's Windows quoting builds
    /// it for these words: an argument with a space or tab is wrapped in
    /// double quotes (the script holds no `"` and ends in no backslash, so
    /// nothing inside needs escaping).
    #[cfg(target_os = "linux")]
    fn windows_command_line(words: &[&str]) -> String {
        words
            .iter()
            .map(|w| {
                assert!(!w.contains('"'));
                if w.is_empty() || w.contains([' ', '\t']) {
                    format!("\"{w}\"")
                } else {
                    (*w).to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Live: the probe exactly as `wsl.exe` hands it to a login shell, run
    /// through every login shell this machine has, yields the meminfo head
    /// AND a census section that parses — page size, `cpu` and `btime`, and
    /// the process table.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_guest_probe_runs_the_same_under_every_login_shell() {
        let mut words = vec!["cat", "/proc/meminfo"];
        words.extend_from_slice(GUEST_PROBE_WORDS);
        let line = windows_command_line(&words);
        let mut ran = 0;
        for shell in ["sh", "bash", "zsh", "fish"] {
            let out = match std::process::Command::new(shell).arg("-c").arg(&line).output() {
                Ok(out) => out,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => panic!("{shell}: {e}"),
            };
            ran += 1;
            let text = String::from_utf8_lossy(&out.stdout);
            let (head, section) = split_guest_output(&text);
            assert!(head.contains("MemTotal:"), "{shell}: {head}");
            let section = section.unwrap_or_else(|| panic!("{shell}: no census section"));
            let tick = parse_guest_section(section).unwrap_or_else(|| panic!("{shell}: {section}"));
            assert!(tick.boot_time.is_some(), "{shell}: btime");
            assert!(tick.stats.len() > 1, "{shell}: process table");
            assert!(!guest_step_killed(section), "{shell}: a bounded step was killed");
        }
        assert!(ran >= 1, "no POSIX shell to run the probe under");
    }

    /// The module must never load argv, environment or cwd. Structural: the
    /// production half of this file names none of the accessors, no refresh
    /// that would load them wholesale, and — on macOS, where any `sysinfo`
    /// process refresh reads `KERN_PROCARGS2` — no `sysinfo` at all.
    #[test]
    fn production_code_never_touches_argv_env_or_cwd() {
        const SRC: &str = include_str!("workload_census.rs");
        // Code only: the docs above name what is avoided, and why.
        let prod: String = SRC
            .split_once("\n#[cfg(test)]")
            .map_or(SRC, |(a, _)| a)
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            ".cmd()",
            ".environ()",
            ".cwd()",
            ".exe()",
            "with_cmd",
            "with_environ",
            "with_cwd",
            "with_exe",
            "ProcessRefreshKind::everything",
            "UpdateKind::Always",
            "UpdateKind::OnlyIfNotSet",
            "/cmdline",
            "/environ",
            "KERN_PROCARGS",
            "proc_pidpath",
        ] {
            assert!(!prod.contains(forbidden), "production code must not use {forbidden}");
        }
        // Every macOS-only item, from its `cfg` to its closing brace.
        let macos: Vec<&str> = prod
            .split("#[cfg(target_os = \"macos\")]")
            .skip(1)
            .map(|item| item.split("\n}\n").next().unwrap_or(item))
            .collect();
        assert!(macos.len() >= 4, "the macOS host lane must exist");
        for item in macos {
            assert!(!item.contains("sysinfo::"), "the macOS lane must not use sysinfo: {item}");
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

    #[test]
    fn an_over_budget_p95_warns_once_per_period() {
        let mut t = CensusTimer::new();
        t.record(Duration::from_millis(10), 1);
        assert!(t.last_warned.is_none());
        t.record(Duration::from_millis(500), 1);
        let first = t.last_warned.expect("p95 over budget warns");
        t.record(Duration::from_millis(500), 1);
        assert_eq!(t.last_warned, Some(first), "not again inside the period");
    }

    /// Live: a census of THIS machine — two ticks, as the sampler takes them.
    /// Asserts only the invariants that hold on any box; the timing is printed
    /// (`--nocapture`) as the budget measurement.
    #[test]
    fn a_live_host_census_holds_its_invariants() {
        let mut h = HostCensus::new();
        let t0 = std::time::Instant::now();
        let first = h.take(None).expect("first tick");
        let d1 = t0.elapsed();
        assert!(first.iter().all(|e| e.cpu_pct.is_none()));
        assert!(!first.is_empty());
        std::thread::sleep(Duration::from_millis(250));
        let t1 = std::time::Instant::now();
        let second = h.take(None).expect("second tick");
        let d2 = t1.elapsed();
        // Only Linux reads every process's CPU time; on macOS and Windows a
        // process the runner may not open nulls its class's share.
        #[cfg(target_os = "linux")]
        assert!(second.iter().all(|e| e.cpu_pct.is_some()));
        assert!(second.len() <= MAX_WORKLOAD_ENTRIES);
        let total: f64 = second.iter().filter_map(|e| e.cpu_pct).sum();
        assert!(total <= 100.0 + 0.1 * second.len() as f64, "{total}");
        // `filter_map`: the macOS/Windows `claude_session` placeholder has none.
        let procs: u32 = second.iter().filter_map(|e| e.proc_count).sum();
        println!("host census: {procs} processes, first tick {d1:?}, second tick {d2:?}; {second:?}");
    }
}
