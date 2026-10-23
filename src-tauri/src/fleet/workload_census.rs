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
//! * **Linux (host lane and the WSL guest)** reads `/proc/<pid>/stat` (whose
//!   only string is the kernel's `comm`), `/proc/<pid>/cgroup`, `/proc/stat`
//!   and the `cpu.stat` of GitHub Actions runner unit cgroups — never
//!   `cmdline` or `environ`.
//! * **Other hosts** use one `sysinfo` table refreshed with CPU, memory and the
//!   executable PATH only (loaded once per process). The path is reduced to one
//!   bit on the spot — "this `node` is Claude Code" — and never enters the
//!   census input; `cmd`, `environ` and `cwd` are never loaded.
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
//! On Linux a share is a difference of CUMULATIVE CPU time, so nothing a
//! process did inside the tick is lost to sampling: a process seen for the
//! first time started inside the interval, and ALL of its CPU time is counted
//! (a 2-second `rustc` between two 30-second ticks is attributed in full). The
//! one blind spot of per-process accounting — a process that started AND
//! exited between two ticks — is closed for GitHub Actions runner units by
//! their cgroup's `cpu.stat` `usage_usec`, which keeps the time of exited
//! children; the unit's class gets the larger of the two figures.
//!
//! Hosts without procfs (Windows, macOS) take `sysinfo`'s per-process rate,
//! which reads zero for a process's first observation, so short-lived
//! processes there go unattributed. The gap between the classes and
//! `cpu_busy_permille / 10` is that unattributed load; `other` is always
//! reported when any process falls into it, so load that IS observed but
//! unexplained is never hidden.
//!
//! `rss_bytes` sums resident set sizes, so pages shared between processes of a
//! class (or across classes) are counted once per process. It is a pressure
//! indicator, not an exact partition of physical memory.
//!
//! ## What a count means
//!
//! `proc_count` counts the class's user-space processes (Linux kernel threads
//! are excluded; their CPU time still counts toward `other`). `root_count` is
//! what reconciles a class against a session or job ledger: one per Claude
//! Code process with no Claude Code ancestor (`claude_session`), and one per
//! `Runner.Worker` (`gh_actions_job`).

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
    /// previous observation to diff against.
    pub(crate) cpu_pct: Option<f64>,
    pub(crate) rss_bytes: u64,
    /// Per-process VRAM summed over the class. `null` = not measured.
    pub(crate) gpu_mem_bytes: Option<u64>,
    /// User-space processes in the class (kernel threads excluded).
    pub(crate) proc_count: u32,
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

/// Whether an executable's image path is Claude Code's own: a component named
/// `@anthropic-ai` or containing `claude-code` (an npm package dir, or a
/// runtime vendored inside one). PURE; the path never leaves this function.
pub(crate) fn image_identifies_claude_code(image: &std::path::Path) -> bool {
    image.components().any(|c| {
        let part = c.as_os_str().to_string_lossy().to_ascii_lowercase();
        part == "@anthropic-ai" || part.contains("claude-code")
    })
}

/// The name a process is classified under. A Node runtime whose image is
/// Claude Code's (the Windows npm install runs as `node.exe`) is `claude`; an
/// ordinary `node.exe` stays `node` — its image says nothing, and the argv
/// that would is never read.
pub(crate) fn identity_name(name: String, image: Option<&std::path::Path>) -> String {
    if name == "node" && image.is_some_and(image_identifies_claude_code) {
        CLAUDE.to_string()
    } else {
        name
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
    /// Normalized executable name ([`normalize_exe_name`] / [`identity_name`]).
    pub(crate) name: String,
    pub(crate) unit: Option<String>,
    /// This process's share of the lane's CPU capacity over the tick, 0–100.
    /// Ignored when the census is told the tick has no CPU delta.
    pub(crate) cpu_pct: f64,
    pub(crate) rss_bytes: u64,
    /// A Linux kernel thread: its CPU counts, but it is no workload process.
    pub(crate) kernel_thread: bool,
}

/// Aggregate a process table into one entry per non-empty class. PURE.
///
/// `cpu_known = false` publishes `cpu_pct: null` on every entry (first tick).
/// `vram_by_pid` is a complete per-process VRAM measurement, or `None` when
/// none was taken — then every entry's `gpu_mem_bytes` is `null`.
/// `unit_cpu` is a unit cgroup's own share over the tick (it includes children
/// that exited inside the tick). A unit whose live processes all fall in one
/// class raises that class by whatever the unit's figure exceeds its
/// processes' sum; a unit whose processes span classes is ignored, since its
/// surplus cannot be placed.
pub(crate) fn census(
    records: &[ProcRecord],
    cpu_known: bool,
    vram_by_pid: Option<&HashMap<u32, u64>>,
    unit_cpu: &HashMap<String, f64>,
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
        roots: u32,
    }
    let mut acc: HashMap<WorkloadClass, Acc> = HashMap::new();
    /// A unit's live processes: their CPU sum, and their one class — `None`
    /// once two classes have been seen.
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
        a.cpu += r.cpu_pct;
        a.rss = a.rss.saturating_add(r.rss_bytes);
        if !r.kernel_thread {
            a.procs = a.procs.saturating_add(1);
        }
        if is_root(class, &view) {
            a.roots = a.roots.saturating_add(1);
        }
        if let Some(v) = vram_by_pid.and_then(|m| m.get(&r.pid)) {
            a.vram = a.vram.saturating_add(*v);
        }
        if let Some(u) = unit.filter(|u| unit_cpu.contains_key(*u)) {
            let ua = units.entry(u).or_insert(UnitAcc {
                cpu: 0.0,
                class: Some(class),
            });
            ua.cpu += r.cpu_pct;
            if ua.class != Some(class) {
                ua.class = None;
            }
        }
    }

    for (unit, ua) in &units {
        let (Some(class), Some(share)) = (ua.class, unit_cpu.get(*unit)) else {
            continue;
        };
        if let Some(a) = acc.get_mut(&class) {
            a.cpu += (share - ua.cpu).max(0.0);
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
// procfs accounting — shared by the Linux host lane and the WSL guest
// ---------------------------------------------------------------------------

/// `PF_KTHREAD` in `/proc/<pid>/stat`'s `flags` field.
const PF_KTHREAD: u64 = 0x0020_0000;

/// One process as `/proc/<pid>/stat` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcStat {
    pid: u32,
    ppid: u32,
    comm: String,
    start_time: u64,
    /// `utime + stime`, cumulative over the process's life (its own threads;
    /// not its reaped children).
    cpu_ticks: u64,
    rss_pages: u64,
    kernel_thread: bool,
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
        cpu_ticks: num(11)?.saturating_add(num(12)?),
        start_time: num(19)?,
        rss_pages: num(21)?,
        kernel_thread: num(6).is_some_and(|flags| flags & PF_KTHREAD != 0),
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

/// One procfs reading of a lane.
#[derive(Debug, Clone, Default)]
struct ProcfsTick {
    page_size: u64,
    /// `USER_HZ`, which converts a cgroup's microseconds into jiffies. `None`
    /// (unreadable) forgoes the unit-cgroup figures, never the census.
    clk_tck: Option<u64>,
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
    total_jiffies: u64,
    ticks: HashMap<(u32, u64), u64>,
    unit_usage_usec: HashMap<String, u64>,
}

/// Cumulative-time accounting over successive [`ProcfsTick`]s.
#[derive(Debug, Default)]
struct ProcfsCensus {
    prev: Option<PrevTick>,
}

impl ProcfsCensus {
    /// Aggregate one reading. `None` when it holds no process — the lane then
    /// publishes no census, never a fabricated one.
    fn take(
        &mut self,
        tick: ProcfsTick,
        vram_by_pid: Option<&HashMap<u32, u64>>,
    ) -> Option<Vec<WorkloadEntry>> {
        if tick.stats.is_empty() {
            return None;
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
                // A process absent from the previous reading started inside
                // this interval, so ALL of its CPU time belongs to it.
                cpu_pct: interval.map_or(0.0, |(span, prev)| {
                    let before = prev.ticks.get(&(s.pid, s.start_time)).copied().unwrap_or(0);
                    s.cpu_ticks.saturating_sub(before) as f64 / span * 100.0
                }),
                rss_bytes: s.rss_pages.saturating_mul(tick.page_size),
                kernel_thread: s.kernel_thread,
            })
            .collect();
        // A unit first seen this tick has no baseline: its counter spans its
        // whole life, so it is left out rather than poured into one interval.
        let unit_cpu: HashMap<String, f64> = match (interval, tick.clk_tck) {
            (Some((span, prev)), Some(hz)) => tick
                .unit_usage_usec
                .iter()
                .filter_map(|(unit, usage)| {
                    let delta = usage.checked_sub(*prev.unit_usage_usec.get(unit)?)?;
                    let jiffies = delta as f64 * hz as f64 / 1_000_000.0;
                    Some((unit.clone(), jiffies / span * 100.0))
                })
                .collect(),
            _ => HashMap::new(),
        };
        self.prev = Some(PrevTick {
            total_jiffies: tick.total_jiffies,
            ticks: tick
                .stats
                .iter()
                .map(|s| ((s.pid, s.start_time), s.cpu_ticks))
                .collect(),
            unit_usage_usec: tick.unit_usage_usec,
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
    procfs: ProcfsCensus,
    /// Each live process's unit placement, keyed by pid and start time so a
    /// reused pid is re-read. A placement does not change under a process
    /// that is not moved, so each `/proc/<pid>/cgroup` is read once.
    placements: HashMap<u32, (u64, Option<(String, Option<String>)>)>,
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
            procfs: ProcfsCensus::default(),
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
        let entries = self.procfs.take(tick, vram_by_pid);
        self.timer.record(started.elapsed(), procs);
        entries
    }

    fn read_tick(&mut self) -> Option<ProcfsTick> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let total_jiffies = parse_total_jiffies(stat.lines().next()?)?;
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
            let placement = cached.unwrap_or_else(|| {
                let p = std::fs::read_to_string(format!("/proc/{}/cgroup", s.pid))
                    .ok()
                    .and_then(|t| unit_placement(&t));
                self.placements.insert(s.pid, (s.start_time, p.clone()));
                p
            });
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

        Some(ProcfsTick {
            page_size: self.page_size,
            clk_tck: self.clk_tck,
            total_jiffies,
            stats,
            units,
            unit_usage_usec,
        })
    }
}

/// The host lane's census state off Linux: ONE `sysinfo::System` kept across
/// ticks, so the sample interval itself is the CPU diff window.
#[cfg(not(target_os = "linux"))]
pub(crate) struct HostCensus {
    sys: sysinfo::System,
    /// Whether `sys` has been refreshed before — the first refresh has no CPU
    /// delta.
    primed: bool,
    cpu_count: usize,
    timer: CensusTimer,
}

#[cfg(not(target_os = "linux"))]
impl HostCensus {
    pub(crate) fn new() -> Self {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_list(sysinfo::CpuRefreshKind::new());
        let cpu_count = sys.cpus().len().max(1);
        Self {
            sys,
            primed: false,
            cpu_count,
            timer: CensusTimer::new(),
        }
    }

    /// Refresh the table and aggregate it. `vram_by_pid` is the seam for a
    /// per-process VRAM measurement; `None` publishes `gpu_mem_bytes: null`.
    pub(crate) fn take(&mut self, vram_by_pid: Option<&HashMap<u32, u64>>) -> Option<Vec<WorkloadEntry>> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};

        let started = std::time::Instant::now();
        // CPU, memory, and the image path once per process (it identifies a
        // Claude Code `node.exe`); `cmd`, `environ` and `cwd` stay at
        // `UpdateKind::Never`, so they are never even read into the table.
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new()
                .with_cpu()
                .with_memory()
                .with_exe(UpdateKind::OnlyIfNotSet),
        );
        let cpu_known = self.primed;
        self.primed = true;

        let cpu_count = self.cpu_count as f64;
        let records: Vec<ProcRecord> = self
            .sys
            .processes()
            .values()
            .map(|p| ProcRecord {
                pid: p.pid().as_u32(),
                ppid: p.parent().map(|pp| pp.as_u32()),
                name: identity_name(normalize_exe_name(&p.name().to_string_lossy()), p.exe()),
                unit: None,
                cpu_pct: f64::from(p.cpu_usage()) / cpu_count,
                rss_bytes: p.memory(),
                kernel_thread: false,
            })
            .collect();

        let entries = census(&records, cpu_known, vram_by_pid, &HashMap::new());
        self.timer.record(started.elapsed(), records.len());
        Some(entries)
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

/// Separates the probe's original procfs output from the census section.
const GUEST_MARKER: &str = "@@qontinui-workload-census@@";
/// Separates the census's `stat` lines from its `cgroup` files.
const GUEST_CGROUP_MARKER: &str = "@@qontinui-workload-census-cgroup@@";
/// Separates the `cgroup` files from the runner units' `cpu.stat` files.
const GUEST_UNIT_MARKER: &str = "@@qontinui-workload-census-units@@";
/// Prefixes the exit status of each bounded step.
const GUEST_RC_MARKER: &str = "@@qontinui-workload-census-rc@@";

/// In-guest bound on reading every `/proc/<pid>/stat`, seconds.
const GUEST_STAT_SECS: &str = "2";
/// In-guest bound on reading every `/proc/<pid>/cgroup`, seconds.
const GUEST_CGROUP_SECS: &str = "2";
/// In-guest bound on reading the runner units' `cpu.stat`, seconds.
const GUEST_UNIT_SECS: &str = "1";

/// The most the census section can add to the probe: the sum of its bounded
/// steps. The probe's own timeout grows by exactly this while the census is
/// on, so the census can never cost the lane its memory and saturation
/// readings — a slow census is killed in the guest and published as no census.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const GUEST_CENSUS_BUDGET: Duration = Duration::from_secs(5);

/// Shell words appended to the `wsl` lane's one probe when the census is on.
///
/// `wsl.exe -d <distro> -- …` runs its arguments through the distro's default
/// shell, so `;`, `$?`, the globs and the redirections are interpreted in the
/// guest. The section emits the page size, `USER_HZ`, the aggregate `cpu` line
/// of `/proc/stat`, every `/proc/<pid>/stat` (each line starts with its own
/// pid, and its only string is the kernel's `comm`), every `/proc/<pid>/cgroup`
/// behind a `tail` header naming its pid, and the `cpu.stat` of every GitHub
/// Actions runner unit cgroup (one to three slices deep) behind a header naming
/// its path. Never `cmdline` and never `environ`.
///
/// Each read is bounded in the guest by `timeout -s KILL`, and followed by its
/// exit status; a killed read (124 / 137) makes the section unusable rather
/// than a silent undercount. On a guest where this section does not run, its
/// marker is missing and the lane publishes no census — UNKNOWN, never empty.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const GUEST_PROBE_WORDS: &[&str] = &[
    ";",
    "echo",
    GUEST_MARKER,
    ";",
    "getconf",
    "PAGESIZE",
    ";",
    "getconf",
    "CLK_TCK",
    ";",
    "head",
    "-n",
    "1",
    "/proc/stat",
    ";",
    "timeout",
    "-s",
    "KILL",
    GUEST_STAT_SECS,
    "cat",
    "/proc/[0-9]*/stat",
    "2>/dev/null",
    ";",
    "echo",
    GUEST_RC_MARKER,
    "$?",
    ";",
    "echo",
    GUEST_CGROUP_MARKER,
    ";",
    "timeout",
    "-s",
    "KILL",
    GUEST_CGROUP_SECS,
    "tail",
    "-v",
    "-n",
    "+1",
    "/proc/[0-9]*/cgroup",
    "2>/dev/null",
    ";",
    "echo",
    GUEST_RC_MARKER,
    "$?",
    ";",
    "echo",
    GUEST_UNIT_MARKER,
    ";",
    "timeout",
    "-s",
    "KILL",
    GUEST_UNIT_SECS,
    "tail",
    "-v",
    "-n",
    "+1",
    "/sys/fs/cgroup/*/actions.runner.*.service/cpu.stat",
    "/sys/fs/cgroup/*/*/actions.runner.*.service/cpu.stat",
    "/sys/fs/cgroup/*/*/*/actions.runner.*.service/cpu.stat",
    "2>/dev/null",
    ";",
    "echo",
    GUEST_RC_MARKER,
    "$?",
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
/// unusable: no page size, no `cpu` line, or a process-table step that was
/// killed. A killed unit step only forgoes the unit figures.
fn parse_guest_section(section: &str) -> Option<ProcfsTick> {
    let (stats_part, rest) = section.split_once(GUEST_CGROUP_MARKER)?;
    let (cgroup_part, unit_part) = rest.split_once(GUEST_UNIT_MARKER).unwrap_or((rest, ""));
    if guest_step_killed(stats_part) || guest_step_killed(cgroup_part) {
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
    let stats: Vec<ProcStat> = lines.filter_map(parse_proc_stat).collect();
    let unit_usage_usec = if guest_step_killed(unit_part) {
        HashMap::new()
    } else {
        parse_guest_unit_usage(unit_part)
    };
    Some(ProcfsTick {
        page_size,
        clk_tck,
        total_jiffies,
        stats,
        units: parse_guest_cgroups(cgroup_part),
        unit_usage_usec,
    })
}

/// The `wsl` lane's census state.
#[derive(Debug, Default)]
pub(crate) struct GuestCensus {
    procfs: ProcfsCensus,
}

impl GuestCensus {
    /// Aggregate one census section. `None` when the section is unusable — the
    /// lane then publishes no census, never a fabricated one — and the
    /// previous reading stays the baseline, so the next interval simply spans
    /// two ticks.
    pub(crate) fn take(&mut self, section: &str) -> Option<Vec<WorkloadEntry>> {
        self.procfs.take(parse_guest_section(section)?, None)
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
            kernel_thread: false,
        }
    }

    fn no_units() -> HashMap<String, f64> {
        HashMap::new()
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

    /// The Windows npm install runs Claude Code as `node.exe`: only the image
    /// path, never argv, makes it a session — and an ordinary `node` stays put.
    #[test]
    fn a_claude_code_node_is_known_by_its_image_alone() {
        use std::path::Path;
        let npm = Path::new("/usr/lib/node_modules/@anthropic-ai/claude-code/vendor/node");
        assert!(image_identifies_claude_code(npm));
        assert_eq!(identity_name("node".to_string(), Some(npm)), "claude");
        let plain = Path::new("/usr/bin/node");
        assert!(!image_identifies_claude_code(plain));
        assert_eq!(identity_name("node".to_string(), Some(plain)), "node");
        assert_eq!(identity_name("node".to_string(), None), "node");
        // Only a Node runtime is renamed; a helper inside the package is not.
        assert_eq!(identity_name("rg".to_string(), Some(npm)), "rg");
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
        assert_eq!(job.proc_count, 6, "busy unit's 4 + the busy Windows pair");
        assert_eq!(job.root_count, Some(2), "one per Runner.Worker");
        let idle = get("gh_actions_runner_idle").expect("idle class");
        assert_eq!(idle.proc_count, 3);
        assert_eq!(idle.root_count, None, "an idle runner has no root to count");
        assert_eq!(get("other").expect("systemd").proc_count, 1);
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
        assert_eq!(claude.proc_count, 5);
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
        assert_eq!(other.proc_count, 1, "kernel threads are no workload process");
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
        let units = HashMap::from([(unit.to_string(), 60.0)]);
        let out = census(&table, true, None, &units);
        let job = out.iter().find(|e| e.class == "gh_actions_job").expect("job");
        assert_eq!(job.cpu_pct, Some(60.0));
        // Never below what the processes themselves measured.
        let low = HashMap::from([(unit.to_string(), 4.0)]);
        let out = census(&table, true, None, &low);
        assert_eq!(out.iter().find(|e| e.class == "gh_actions_job").and_then(|e| e.cpu_pct), Some(10.0));
        // A unit whose processes span classes cannot place its surplus.
        let mixed_unit = "qontinui-runner.service";
        let mixed = vec![
            rec(2, 1, "qontinui-runner", Some(mixed_unit), 1.0, 1),
            rec(3, 2, "claude", Some(mixed_unit), 1.0, 1),
        ];
        let units = HashMap::from([(mixed_unit.to_string(), 50.0)]);
        let out = census(&mixed, true, None, &units);
        let total: f64 = out.iter().filter_map(|e| e.cpu_pct).sum();
        assert_eq!(total, 2.0);
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
        let total: u32 = out.iter().map(|e| e.proc_count).sum();
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
        assert_eq!(s.cpu_ticks, 500);
        assert_eq!(s.start_time, 5555);
        assert_eq!(s.rss_pages, 250);
        assert!(!s.kernel_thread);
        assert!(parse_proc_stat("garbage").is_none());
        // flags carries PF_KTHREAD (0x00208040 = 2129984).
        let k = parse_proc_stat("9 (kworker/0:1) I 2 0 0 0 -1 2129984 0 0 0 0 0 7 0 0 20 0 1 0 30 0 0")
            .expect("parses");
        assert!(k.kernel_thread);
    }

    fn guest_section(
        total: [u64; 8],
        procs: &[(u32, u32, &str, u64, u64)],
        cgroups: &str,
        units: &str,
    ) -> String {
        let cpu = total.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
        let mut s = format!("\n4096\n100\ncpu  {cpu} 0 0\n");
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
        assert_eq!(job.proc_count, 3, "listener, worker and the rustc under it");
        assert_eq!(job.root_count, Some(1));
        assert_eq!(job.rss_bytes, 3 * 10 * 4096);
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

    /// The probe's timeout grows by exactly the census's in-guest bounds, and
    /// every bounded step is followed by its exit status.
    #[test]
    fn the_guest_census_budget_is_the_sum_of_its_bounds() {
        let secs: u64 = [GUEST_STAT_SECS, GUEST_CGROUP_SECS, GUEST_UNIT_SECS]
            .iter()
            .map(|s| s.parse::<u64>().expect("whole seconds"))
            .sum();
        assert_eq!(GUEST_CENSUS_BUDGET, Duration::from_secs(secs));
        let timeouts = GUEST_PROBE_WORDS.iter().filter(|w| **w == "timeout").count();
        let statuses = GUEST_PROBE_WORDS.iter().filter(|w| **w == GUEST_RC_MARKER).count();
        assert_eq!(timeouts, 3);
        assert_eq!(statuses, timeouts);
    }

    /// The module must never load argv, environment or cwd. Structural: the
    /// production half of this file names none of the accessors, and no
    /// refresh that would load them wholesale.
    #[test]
    fn production_code_never_touches_argv_env_or_cwd() {
        const SRC: &str = include_str!("workload_census.rs");
        let prod = SRC.split_once("\n#[cfg(test)]").map_or(SRC, |(a, _)| a);
        for forbidden in [
            ".cmd()",
            ".environ()",
            ".cwd()",
            "with_cmd",
            "with_environ",
            "with_cwd",
            "ProcessRefreshKind::everything",
            "UpdateKind::Always",
            "/cmdline",
            "/environ",
        ] {
            assert!(!prod.contains(forbidden), "production code must not use {forbidden}");
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
        assert!(second.iter().all(|e| e.cpu_pct.is_some()));
        assert!(second.len() <= MAX_WORKLOAD_ENTRIES);
        let total: f64 = second.iter().filter_map(|e| e.cpu_pct).sum();
        assert!(total <= 100.0 + 0.1 * second.len() as f64, "{total}");
        let procs: u32 = second.iter().map(|e| e.proc_count).sum();
        println!("host census: {procs} processes, first tick {d1:?}, second tick {d2:?}; {second:?}");
    }
}
