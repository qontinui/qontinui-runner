//! Per-machine resource samples (plan
//! `2026-08-02-fleet-resource-telemetry-and-ci-allocation.md` §A1/§A2, runner
//! half).
//!
//! POSTs a typed, **lane-separated** snapshot of this machine's capacity to
//! `POST /coord/devices/{device_id}/resource-sample` so the fleet's headroom
//! stops being a number an agent has to shell out for, one `Get-CimInstance`
//! at a time.
//!
//! ## Why this has no timer of its own
//!
//! It is driven from [`crate::fleet::spawn_budget_republisher`], which already
//! runs a periodic loop and — load-bearingly — already returns early on a
//! secondary instance. Every runner on a box shares one
//! `~/.qontinui/machine.json` and therefore one `device_id`, so a `test-*`
//! runner's samples would be indistinguishable from the primary's while
//! describing a different workload. On 2026-07-28 exactly that shape (a temp
//! runner writing the shared device row) left coord's shadow lane electing no
//! device for six days; the write-up is on `fleet::budget_publish_allowed`.
//! Hanging the sampler off the already-gated loop inherits the gate rather than
//! restating it — a rule keyed in one place is the whole point of that helper.
//!
//! ## Lanes are not summable
//!
//! `.wslconfig` caps WSL at a ceiling well below physical RAM, so `host` and
//! `wsl` measure different pools. They are also **coupled**, not disjoint:
//! `pageReporting=true` means WSL returns idle pages to Windows, so the host
//! lane's free-commit figure already nets out WSL's live usage and the WSL
//! lane's spendable headroom is `min(ceiling - used, host_free)`. A row without
//! a lane is uninterpretable, and a UI that adds the two is confidently wrong.
//! We publish both, labelled; we never publish a single "machine RAM" number.
//!
//! ## Best-effort, always
//!
//! A coord outage must never affect the runner. Failures are logged at DEBUG
//! (WARN on the first few) and the sample is DROPPED — never buffered, never
//! retried. The next tick re-observes the machine, and a sample is only
//! interesting while it is fresh.

use std::path::PathBuf;
use std::time::Duration;

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use serde::Serialize;
use tracing::{debug, warn};

/// Default sample cadence in seconds.
///
/// Deliberately much faster than [`crate::fleet::BUDGET_REPUBLISH_DEFAULT_SECS`]
/// (600s): the budget writes an **authoritative column** that coord's
/// dispatchers read as this device's declared capacity, while a sample is an
/// observation that ages out. Re-asserting an authoritative value fast buys
/// nothing and multiplies the blast radius of a wrong one; observing fast is
/// the only way headroom-based admission sees a spike at all.
const SAMPLE_DEFAULT_SECS: u64 = 30;

/// Floor for the sample cadence. Below this the WSL probe (a `wsl.exe`
/// subprocess) starts costing more than the signal is worth.
const SAMPLE_MIN_SECS: u64 = 10;

/// Bound on the `wsl.exe` probe. A wedged or starting-up WSL VM must not stall
/// the sampler loop — it just costs that tick's `wsl` lane.
#[cfg(windows)]
const WSL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// HTTP timeout for one sample POST. Short on purpose: a sample that arrives
/// after the next one was taken is worse than no sample.
const POST_TIMEOUT: Duration = Duration::from_secs(8);

/// Resolve the sample cadence, applying the [`SAMPLE_MIN_SECS`] floor.
/// Overridable via `COORD_RESOURCE_SAMPLE_SECS`.
pub(crate) fn sample_interval_secs() -> u64 {
    std::env::var("COORD_RESOURCE_SAMPLE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SAMPLE_DEFAULT_SECS)
        .max(SAMPLE_MIN_SECS)
}

/// One tick's sleep: the cadence with symmetric ±20% jitter, so a fleet of
/// machines rebooted together does not converge onto one POST instant. Reuses
/// [`crate::agent_pusher::jittered_interval`] rather than rolling a second
/// jitter helper.
pub(crate) fn jittered_sleep(interval_secs: u64) -> Duration {
    let jitter = (interval_secs / 5).max(1);
    Duration::from_secs(crate::agent_pusher::jittered_interval(
        interval_secs,
        jitter,
    ))
}

/// Which resource pool a sample describes. `lane` is mandatory and
/// load-bearing — see the module docs on why lanes must never be summed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lane {
    Host,
    Wsl,
}

impl Lane {
    /// The wire name, and the name that lands in [`ResourceSample::lane`].
    ///
    /// `pub(crate)` so the lane vocabulary has exactly ONE home: the spawn
    /// gate's fleet-floor cache (`mcp::fleet_policy_poller`) selects a lane's
    /// floors by this string, and a second literal `"host"` somewhere else is
    /// how a renamed lane turns into a silently empty lookup rather than a
    /// compile error.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Lane::Host => "host",
            Lane::Wsl => "wsl",
        }
    }
}

/// Which instrument produced a lane's saturation counts.
///
/// **Not bookkeeping — without it the number changes meaning silently.** coord
/// already carries exactly this field, as `process_health.rs:163`'s
/// `pids_source`, and `:158`-`:160` says why in one line: `"cgroup"` counts
/// **tasks (threads)** and `"proc"` counts **thread-group leaders**, and *"they
/// are different quantities."* A publisher that probes the cgroup, fails, and
/// falls back to the OS-wide table therefore emits a number that can differ
/// from the previous tick by an order of magnitude with nothing in the row
/// saying so — and coord's saturation ratio would divide it by a ceiling
/// measured on the *other* quantity.
///
/// Read together with [`Saturation`]'s doc for the rule that decides WHICH
/// instrument this names when a row carries counts from two of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaturationSource {
    /// The pids cgroup controller — tasks (threads) inside one cgroup.
    ///
    /// coord's own `container`-lane arm. The runner publishes the `host` and
    /// `wsl` lanes, neither of which has a root-cgroup pids ceiling to read, so
    /// nothing here emits it; the variant exists so the vocabulary has one home
    /// rather than two.
    Cgroup,
    /// The kernel's own task table, read through `/proc`: this module takes
    /// `/proc/loadavg`'s fourth field (`runnable/total`, proc(5)'s "kernel
    /// scheduling entities") against `/proc/sys/kernel/threads-max`, which
    /// bounds exactly that quantity.
    ///
    /// There is no Windows arm of this variant, and that is deliberate:
    /// `GetPerformanceInfo` reports a live `ThreadCount` and **no ceiling for
    /// it**, so a `"proc"` reading on the Windows host lane could only ever be
    /// half a pair. See [`Saturation`].
    Proc,
    /// A Windows **job object**'s process accounting — `ActiveProcesses`
    /// against `ActiveProcessLimit`. NEW with this plan — coord's
    /// `process_health.rs` emits `"cgroup"` and `"proc"` only, so do not go
    /// looking for a shipped publisher of this string and conclude the
    /// vocabulary is broken.
    JobObject,
}

impl SaturationSource {
    /// The wire string. The vocabulary is
    /// `"cgroup" | "proc" | "job_object" | NULL` and the column is deliberately
    /// free text (no CHECK) — see `fleet_res_tel_04_saturation_columns.py`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SaturationSource::Cgroup => "cgroup",
            SaturationSource::Proc => "proc",
            SaturationSource::JobObject => "job_object",
        }
    }
}

/// One `coord.device_resource_samples` row (§A1 field names).
///
/// Every numeric field is `Option`: a metric no probe carried is **UNKNOWN,
/// never 0**. A lone zero in an otherwise populated row is the more dangerous
/// reading of the two — it looks like a measurement. `resource-sampler.sh`
/// learned this the hard way and its awk reducer says the same thing.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ResourceSample {
    /// `'host' | 'wsl'`.
    pub(crate) lane: String,
    /// Which publisher within the lane. `None` = "the only publisher for this
    /// lane", which is the runner's host lane; the WSL lane names its distro.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lane_instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cpu_cores: Option<i32>,
    /// 1-minute load average. `None` on the Windows host lane — Windows has no
    /// equivalent, and a fabricated 0.0 would read as "idle".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) load_1m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mem_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mem_available_bytes: Option<u64>,
    /// Windows commit limit / free commit. `None` off Windows, where the
    /// concept does not exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) commit_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) commit_available_bytes: Option<u64>,
    /// Swap leads the analysis, not the memory fields — see
    /// [`crate::ci_node::admission::Headroom`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) swap_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) swap_used_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) disk_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) disk_free_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) disk_mount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) build_slots_total: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) build_slots_busy: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) build_queue_depth: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ci_jobs_running: Option<i32>,
    /// The kernel task ceiling for this lane — `/proc/sys/kernel/threads-max`
    /// on Linux. `i64`, not `u64`, because coord reads the column as
    /// `Option<i64>` and a number it cannot deserialize is a silently dropped
    /// row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) threads_max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) threads_used: Option<i64>,
    /// A cgroup or Windows job-object PID ceiling, where one applies and binds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pids_max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pids_used: Option<i64>,
    /// Which instrument produced the pair above — see [`Saturation`]. Never
    /// written without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) saturation_source: Option<String>,
    /// `'runner' | 'supervisor' | 'ci-step'`. Distinguishes this row from the
    /// supervisor's host-lane row for the same device.
    pub(crate) source: String,
}

/// One lane's saturation reading — a **complete** count/ceiling pair plus the
/// instrument that produced both. The axis the 2026-08-27 fork-exhaustion
/// incident sat at 99.3% of while every memory gauge on the box read ≤ 21%.
///
/// Plan `2026-08-27-fleet-telemetry-has-no-saturation-dimension-but-memory`,
/// Phase 3. The storage is `coord.device_resource_samples`'s `threads_max` /
/// `threads_used` / `pids_max` / `pids_used` / `saturation_source`
/// (qontinui-web migration `fleet_res_tel_04`).
///
/// ## Why this type cannot hold half a pair
///
/// That is the whole reason it is a type and not four `Option` fields, and it
/// is not tidiness. coord grades the saturation axis as soon as a row carries
/// **any** of the four columns (`SaturationInputs::is_unmeasured`), and a count
/// without its ceiling — or a ceiling without its count — is a *real gap*
/// there, so it grades `Unknown`. `Unknown` outranks `Warn` and `Ok` in coord's
/// worst-of `headroom` composition, so publishing half a pair would strip the
/// row of its perfectly good memory and disk verdicts, permanently, for the
/// sake of a number nothing can divide. **Publishing nothing is strictly better
/// than publishing half** — and a publisher that carries no saturation
/// instrument at all (every runner in the fleet until its next start) is
/// skipped by coord rather than graded, which is exactly the shape a lane with
/// no readable ceiling must present.
///
/// The ratio's numerator and denominator are therefore always the same
/// quantity, from the same instrument, in the same read.
///
/// ## NULL, never 0
///
/// A fabricated `0` here does not merely misreport — it **inverts** the
/// reading: `threads_used = 0` renders as perfectly idle on the one axis built
/// to catch a box at 99.3%, and coord's `NULLS LAST` ranking would then promote
/// the blind machine to the front of the dispatch queue. A `0` **ceiling** is
/// worse still: coord's ratio divides by it. Both are rejected in
/// [`Self::new`], which is why every constructor returns `Option`.
///
/// An **unbounded** ceiling — cgroup v2's literal `max`, or a Windows job
/// object with no `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` — is likewise nothing at
/// all, never a sentinel: any non-NULL stand-in would render an unbounded scope
/// as saturated, and "nothing bounds this scope" is a real and important fact
/// (it is precisely what `docker inspect`'s `PidsLimit=<nil>` said during the
/// incident).
///
/// ## One pair per lane, and which one
///
/// A narrower ceiling that actually **bounds** the lane is the binding
/// constraint and wins; where nothing narrower is bounded, the kernel thread
/// table is the binding one. That is exactly the (deliberate, and correct)
/// cross-scope comparison the incident was diagnosed by — 190,840 cgroup tasks
/// against a host-wide `threads-max` of 192,146, correct *because* nothing
/// bounded the cgroup.
///
/// **Exactly one pair is ever written**, whichever arm answered — see
/// [`ResourceSample::set_saturation`], which takes one reading and not four
/// columns. That is what keeps the publisher and the consumer in agreement
/// without them having to share a preference order: coord's `lane_saturation`
/// reads the threads pair first and falls back to pids, so a row carrying
/// *both* would let coord divide by the wider ceiling while the publisher had
/// already decided the narrower one binds. Emitting one pair makes that
/// disagreement unrepresentable rather than merely unlikely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Saturation {
    counter: SaturationCounter,
    used: i64,
    max: i64,
    source: SaturationSource,
}

/// Which pair of columns a [`Saturation`] fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaturationCounter {
    /// `threads_used` / `threads_max` — the kernel task table.
    Threads,
    /// `pids_used` / `pids_max` — a cgroup or job-object ceiling.
    Pids,
}

impl Saturation {
    /// A reading, or `None` if it is not a complete and usable pair.
    ///
    /// Rejects a missing half, a non-positive ceiling (coord's ratio divides by
    /// it, and a `0` ceiling would render an unbounded lane as saturated) and a
    /// negative count. Every rejection publishes NOTHING for the lane rather
    /// than a partial row — see the type docs.
    fn new(
        counter: SaturationCounter,
        used: Option<i64>,
        max: Option<i64>,
        source: SaturationSource,
    ) -> Option<Self> {
        let (used, max) = (used?, max?);
        (max > 0 && used >= 0).then_some(Self {
            counter,
            used,
            max,
            source,
        })
    }

    /// The kernel task table: `threads_used` / `threads_max`.
    pub(crate) fn threads(
        used: Option<i64>,
        max: Option<i64>,
        source: SaturationSource,
    ) -> Option<Self> {
        Self::new(SaturationCounter::Threads, used, max, source)
    }

    /// A cgroup / job-object PID ceiling: `pids_used` / `pids_max`.
    pub(crate) fn pids(
        used: Option<i64>,
        max: Option<i64>,
        source: SaturationSource,
    ) -> Option<Self> {
        Self::new(SaturationCounter::Pids, used, max, source)
    }

    /// This reading's saturation ratio, `used / max`.
    ///
    /// **Total, not `Option`** — deliberately, and it is the payoff of the
    /// complete-pair invariant above: [`Self::new`] has already rejected a
    /// missing half and a non-positive ceiling, so a reading that exists is
    /// always divisible and no caller has to re-guard the divisor. The
    /// `Option` lives one level up, on whether the lane produced a reading at
    /// all — which is the same place coord puts it (`NULLIF(threads_max, 0)`
    /// guards a *stored* row this publisher never writes).
    pub(crate) fn ratio(self) -> f64 {
        self.used as f64 / self.max as f64
    }
}

impl ResourceSample {
    fn empty(lane: Lane, lane_instance: Option<String>) -> Self {
        Self {
            lane: lane.as_str().to_string(),
            lane_instance,
            cpu_cores: None,
            load_1m: None,
            mem_total_bytes: None,
            mem_available_bytes: None,
            commit_total_bytes: None,
            commit_available_bytes: None,
            swap_total_bytes: None,
            swap_used_bytes: None,
            disk_total_bytes: None,
            disk_free_bytes: None,
            disk_mount: None,
            build_slots_total: None,
            build_slots_busy: None,
            build_queue_depth: None,
            ci_jobs_running: None,
            threads_max: None,
            threads_used: None,
            pids_max: None,
            pids_used: None,
            saturation_source: None,
            source: "runner".to_string(),
        }
    }

    /// Write a saturation reading onto this sample — **the only way the five
    /// saturation fields are ever set.**
    ///
    /// `None` leaves all five NULL, which is the correct publish for a lane
    /// with no readable ceiling and is what coord's `is_unmeasured` skip is
    /// built for. There is deliberately no setter that takes the columns
    /// individually: [`Saturation`] documents why half a pair is worse than
    /// silence, and a per-column setter is exactly how that invariant would be
    /// lost at some future call site.
    fn set_saturation(&mut self, reading: Option<Saturation>) {
        let Some(r) = reading else {
            return;
        };
        match r.counter {
            SaturationCounter::Threads => {
                self.threads_used = Some(r.used);
                self.threads_max = Some(r.max);
            }
            SaturationCounter::Pids => {
                self.pids_used = Some(r.used);
                self.pids_max = Some(r.max);
            }
        }
        self.saturation_source = Some(r.source.as_str().to_string());
    }
}

/// Wire shape of `POST /coord/devices/{device_id}/resource-sample`.
///
/// Batched because one tick emits every lane the machine has, and two POSTs
/// for one observation instant would let a lane land without its sibling — the
/// exact "9 GB free WSL beside a 900 MB host" reading §C3 forbids. Mirrors
/// `agent_worktree::census::WorktreeCensusReq`'s envelope (`device_id` +
/// optional `tenant_id` + rows) rather than inventing a second push shape.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ResourceSampleReq {
    pub(crate) device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tenant_id: Option<String>,
    pub(crate) sampled_at: String,
    pub(crate) samples: Vec<ResourceSample>,
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

/// Available **commit** in bytes, or `None` when it cannot be determined.
///
/// This is the one memory quantity the runner's guards and the A1 snapshot
/// agree on (plan §A3). On Windows it reports free COMMIT (`ullAvailPageFile`),
/// NOT free physical RAM: the binding constraint for a big rustc here is the
/// commit limit, and builds have died at ~90% commit while free-physical still
/// looked healthy. `qontinui-supervisor::build_monitor::available_commit_bytes`
/// and `cargo-guard.sh` read the same underlying number by design — before this
/// plan `ci_node` was the **only** lane on a different quantity
/// (`sysinfo::System::available_memory()`, which on Windows is
/// physical-available), so its 4 GiB floor and the supervisor's 5 GiB floor were
/// not comparable at all. They are now.
///
/// Off Windows this falls back to sysinfo's `available_memory()` (MemAvailable
/// on Linux), which is the closest honest equivalent.
pub(crate) fn available_commit_bytes() -> Option<u64> {
    commit_status().map(|(_total, avail)| avail)
}

/// `(commit_total, commit_available)` in bytes. `None` off Windows or when the
/// call fails — callers fail OPEN.
fn commit_status() -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: `status` is a correctly-sized, zeroed MEMORYSTATUSEX with
        // `dwLength` set as the API requires; the call only writes into it.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        if ok == 0 {
            return None;
        }
        Some((status.ullTotalPageFile, status.ullAvailPageFile))
    }
    #[cfg(not(windows))]
    {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let avail = sys.available_memory();
        (avail > 0).then_some((sys.total_memory(), avail))
    }
}

/// Collect the `host` lane. Blocking (sysinfo refresh + a disk enumeration), so
/// callers run it on a blocking pool.
///
/// **Not for the spawn gate.** The disk enumeration walks every volume the OS
/// reports — including disconnected network and removable drives, which block
/// until the OS gives up — and this also reads the `ci_node` settings and
/// computes build occupancy. That is the right cost for a 30 s publish and the
/// wrong cost immediately before a PTY opens on a tokio worker, so the gate
/// takes [`spawn_gate_reading`] instead (plan
/// `2026-08-07-runner-resource-guard-and-session-protection.md` §Part A).
fn collect_host_lane() -> ResourceSample {
    use sysinfo::System;

    let mut s = ResourceSample::empty(Lane::Host, None);

    s.cpu_cores = std::thread::available_parallelism()
        .ok()
        .map(|n| n.get().min(i32::MAX as usize) as i32);

    let mut sys = System::new();
    sys.refresh_memory();
    let total = sys.total_memory();
    if total > 0 {
        s.mem_total_bytes = Some(total);
        s.mem_available_bytes = Some(sys.available_memory());
    }
    // Swap: published ONLY where it is a real, independently-measured pagefile
    // figure. On Windows sysinfo derives it from the commit charge, so
    // publishing it as `swap_used_bytes` would hand coord's §B1 ranking the
    // commit reading a second time under a Linux name — and the ranking leads
    // on swap precisely because it is meant to be the *other* signal. The
    // Windows host lane carries `commit_*` instead, which is the honest one;
    // the `wsl` lane carries real swap from `/proc/meminfo`. Same rule the
    // local guard follows in `ci_node::admission::probe_headroom`.
    #[cfg(not(windows))]
    {
        let swap_total = sys.total_swap();
        if swap_total > 0 {
            s.swap_total_bytes = Some(swap_total);
            s.swap_used_bytes = Some(sys.used_swap());
        }
    }

    if let Some((commit_total, commit_avail)) = commit_status() {
        s.commit_total_bytes = Some(commit_total);
        s.commit_available_bytes = Some(commit_avail);
    }

    // Windows has no load average; sysinfo returns zeros there, and a
    // fabricated 0.0 would render as "idle" on a saturated box.
    #[cfg(not(windows))]
    {
        s.load_1m = Some(System::load_average().one);
    }

    // Same volume probe the disk floor gates on, so the dashboard's disk figure
    // and the gate's are literally one reading.
    if let Some(root) = crate::agent_runtime::qontinui_root_dir() {
        if let Some((mount, total, free)) = crate::ci_node::admission::probe_volume_for(&root) {
            s.disk_mount = Some(mount.to_string_lossy().to_string());
            s.disk_total_bytes = Some(total);
            s.disk_free_bytes = Some(free);
        }
    }

    // The runner's build lane IS its `ci_node` executor, so slots and CI jobs
    // are the same occupancy read two ways. The supervisor publishes the
    // Windows build pool's slots under `source='supervisor'`.
    let ci = crate::settings::get_ci_node_settings();
    let (running, queued) = crate::ci_node::admission::occupancy();
    if ci.enabled {
        s.build_slots_total = Some(ci.max_concurrent_builds.max(1).min(i32::MAX as u32) as i32);
    }
    s.build_slots_busy = Some(running.min(i32::MAX as usize) as i32);
    s.build_queue_depth = Some(queued.min(i32::MAX as usize) as i32);
    s.ci_jobs_running = Some(running.min(i32::MAX as usize) as i32);

    // The saturation axis — the third one, and the one the 2026-08-27 incident
    // sat at 99.3% of while every field above it read healthy.
    s.set_saturation(host_saturation());

    s
}

/// The `(lane, free commit)` pair the spawn gate decides on — the whole reading,
/// nothing else.
///
/// Plan `2026-08-07-runner-resource-guard-and-session-protection.md` §Part A.
/// The terminal-spawn gate needs a verdict before it opens a PTY: the ~30 s
/// publish cadence plus a coord round-trip is far too slow for that decision,
/// and the gate has to keep working when coord is unreachable — which is
/// exactly when the box is most likely to be under load.
///
/// ## Why not [`collect_host_lane`]'s whole sample
///
/// [`crate::resource_guard::evaluate`] consults exactly two things: which lane
/// the reading is from, and how much commit is free. The publisher's sample
/// additionally refreshes sysinfo, enumerates EVERY volume on the box (network
/// and removable drives included — a disconnected mount blocks until the OS
/// gives up), reads the `ci_node` settings and computes build occupancy. None of
/// that reaches the verdict, and `TerminalSession::spawn` is called
/// SYNCHRONOUSLY on a tokio worker from every unattended spawn seam, so a stalled
/// mount would park a runtime worker on the way to opening a PTY. This is one
/// [`available_commit_bytes`] call: microseconds, no allocation, no volume probe,
/// no settings read — the "a few milliseconds, no I/O" the plan's §Part D step 1
/// actually asks for.
///
/// The gate and the fleet dashboard still agree on the QUANTITY: both read free
/// commit through this same [`available_commit_bytes`], plan §A3's converged
/// number. They are two instants of one metric rather than one instant shared,
/// which is all a spawn-time verdict can honestly claim anyway — the published
/// row is up to [`SAMPLE_DEFAULT_SECS`] old by the time a PTY opens.
///
/// ## Host lane ONLY, deliberately
///
/// No WSL lane here, and adding one would be a regression. The WSL probe forks
/// `wsl.exe` under [`WSL_PROBE_TIMEOUT`] (5 s); a pre-PTY gate that can stall
/// five seconds on a wedged or cold-starting WSL VM is a worse user-facing
/// failure than the one it prevents. The host lane is also the *correct* lane
/// for the question: `pageReporting=true` means the host free-commit figure
/// already nets out WSL's live usage (see this module's "Lanes are not
/// summable" docs above), so a host reading is not blind to `vmmemWSL` — it is
/// precisely the quantity that collapsed to 7.25 GB during the 2026-08-06→07
/// incident. The WSL figure still reaches coord in the published sample, where
/// cross-machine ranking can afford the subprocess.
pub(crate) fn spawn_gate_reading() -> (&'static str, Option<u64>) {
    (Lane::Host.as_str(), available_commit_bytes())
}

/// Run one bounded `wsl.exe` probe, reaping the child **and its tree** on
/// expiry.
///
/// `tokio::time::timeout` only drops the output-future, and tokio does *not*
/// kill on drop by default — so a bare `timeout(Command::output())` orphans a
/// live `wsl.exe` on every expiry, and that orphan holds its VM session open
/// for the runner's whole lifetime. `kill_on_drop` alone is not enough either:
/// `wsl.exe` spawns a second `wsl.exe` to carry the session, and the grandchild
/// outlives a killed parent. Same defect, and the same two-part fix, as the
/// `git push` bound in [`crate::agent_pusher`] — see its `kill_on_drop`
/// comment for the incident that established the pattern.
///
/// Measured 2026-08-27 on the operator box: a WSL VM that could not `fork()`
/// made every probe time out, and the sampler accumulated **512** stuck
/// `wsl.exe` (98k handles, 23% of all system handles) over ~3h — jamming the
/// very distro it was trying to read, and outliving the fault that started it.
#[cfg(windows)]
async fn wsl_probe(args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = crate::process_helpers::tokio_no_window("wsl.exe");
    cmd.args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().ok()?;

    // Held until this function returns; dropping it closes the job and reaps
    // the whole tree. Best-effort — on failure we still have `kill_on_drop`
    // plus the global job, i.e. strictly better than before.
    let _tree_job = {
        let job = qontinui_runner_win32::ScopedKillOnCloseJob::create(None);
        if let (Some(j), Some(handle)) = (job.as_ref(), child.raw_handle()) {
            // SAFETY: `handle` came from the live `child` this scope owns.
            unsafe { j.assign(handle as _) };
        }
        job
    };

    tokio::time::timeout(WSL_PROBE_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()
}

/// Which WSL distro to probe: `QONTINUI_WSL_DISTRO` if set, else the first
/// entry `wsl.exe --list --quiet` reports. `None` = no WSL on this box, which
/// is the honest "this machine has one lane" answer, not an error.
///
/// Resolved once per process and cached — including the negative answer. The
/// installed distro set does not change under a running runner, and a 30s
/// sampler that re-forked `wsl.exe` to re-derive it would double this
/// module's process cost for a constant.
#[cfg(windows)]
async fn wsl_distro() -> Option<String> {
    static CACHE: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();
    CACHE.get_or_init(resolve_wsl_distro).await.clone()
}

#[cfg(windows)]
async fn resolve_wsl_distro() -> Option<String> {
    if let Ok(d) = std::env::var("QONTINUI_WSL_DISTRO") {
        let d = d.trim().to_string();
        if !d.is_empty() {
            return Some(d);
        }
    }
    let out = wsl_probe(&["--list", "--quiet"]).await?;
    if !out.status.success() {
        return None;
    }
    first_distro(&decode_utf16le(&out.stdout))
}

/// `wsl.exe --list` writes UTF-16LE, not UTF-8 — decoding it as UTF-8 yields
/// NUL-interleaved garbage that parses as a distro name. Same class as the
/// UTF-16LE Tauri log encoding this repo already tripped over once.
#[cfg(windows)]
fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// First non-empty line of a `wsl --list --quiet` listing, with the BOM and
/// CR stripped. Pure for tests.
#[cfg_attr(not(windows), allow(dead_code))]
fn first_distro(listing: &str) -> Option<String> {
    listing
        .lines()
        .map(|l| l.trim_matches(|c: char| c == '\u{feff}' || c.is_whitespace() || c == '\0'))
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

/// The procfs files the `wsl` lane reads, in one `cat`, in one fork.
///
/// Order is the order they appear in the probe's stdout, but nothing parses by
/// position: [`parse_meminfo`] and [`parse_proc_saturation`] each recognise their
/// own lines by SHAPE, so a file that is missing on some kernel costs its own
/// reading and nothing else.
#[cfg_attr(not(windows), allow(dead_code))]
const WSL_PROC_FILES: &[&str] = &[
    // Memory, the lane's original reason for existing.
    "/proc/meminfo",
    // The kernel task ceiling — a bare integer. The exact file
    // `process_health.rs:506` reads, and the 192,146 the 2026-08-27 incident
    // ran into.
    "/proc/sys/kernel/threads-max",
    // Current tasks, as the denominator of field 4 (`runnable/total`). Same
    // quantity `threads-max` bounds — proc(5): "kernel scheduling entities",
    // i.e. threads, not thread-group leaders.
    "/proc/loadavg",
];

/// Collect the `wsl` lane by reading the VM's own procfs.
///
/// `None` when there is no WSL, the probe times out, or the output does not
/// parse — a missing lane is honest; a zeroed one is not.
///
/// ## One fork, and that is load-bearing
///
/// Every file in [`WSL_PROC_FILES`] rides the single `cat` this function
/// already ran for `/proc/meminfo`. Adding a second `wsl.exe` probe to publish
/// a fork-exhaustion metric would make this module a contributor to the next
/// incident instead of its detector: the 2026-08-27 event accumulated **512**
/// stuck `wsl.exe` (98k handles, 23% of all system handles) from the
/// *monitoring* path alone, and [`wsl_probe`]'s own doc records it. `:390` and
/// `:625` of this file already warn against re-forking to re-derive a value.
///
/// ## Why the exit status is no longer a gate
///
/// `cat` exits non-zero if **any** operand is missing while still writing every
/// operand that was not — so with three files a kernel lacking one of them
/// would have cost the whole lane, memory included, under the old check. What
/// decides the lane's existence is `MemTotal` parsing, which is the honest test
/// and the one that was always doing the work.
#[cfg(windows)]
async fn collect_wsl_lane() -> Option<ResourceSample> {
    let distro = wsl_distro().await?;
    // Scoped so the `&distro` borrow ends before the name is moved below.
    let out = {
        let mut args = vec!["-d", distro.as_str(), "--", "cat"];
        args.extend_from_slice(WSL_PROC_FILES);
        wsl_probe(&args).await?
    };
    // procfs is plain ASCII even though `wsl --list` is UTF-16.
    let text = String::from_utf8_lossy(&out.stdout);
    let mut sample = parse_meminfo(&text, distro)?;
    sample.set_saturation(parse_proc_saturation(&text));
    Some(sample)
}

#[cfg(not(windows))]
async fn collect_wsl_lane() -> Option<ResourceSample> {
    None
}

/// Parse `/proc/meminfo` into a `wsl`-lane sample. Values are kB.
///
/// Swap is reported as total + **used** (`SwapTotal - SwapFree`) because a bare
/// swap byte count cannot be read as pressure — the ceiling differs per host,
/// so pressure only means something against it.
fn parse_meminfo(text: &str, lane_instance: String) -> Option<ResourceSample> {
    let kb = |key: &str| -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.trim().strip_suffix("kB"))
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|v| v.saturating_mul(1024))
    };

    let mem_total = kb("MemTotal:")?;
    let mut s = ResourceSample::empty(Lane::Wsl, Some(lane_instance));
    s.mem_total_bytes = Some(mem_total);
    s.mem_available_bytes = kb("MemAvailable:");
    if let Some(swap_total) = kb("SwapTotal:") {
        // A zero ceiling is a real configuration, not a missing reading — but
        // "used" against it is meaningless, so only publish the pair.
        s.swap_total_bytes = Some(swap_total);
        s.swap_used_bytes = kb("SwapFree:").map(|free| swap_total.saturating_sub(free));
    }
    Some(s)
}

/// A `/proc`-derived saturation reading, out of `threads-max` and `loadavg`
/// text — the same concatenated probe output [`parse_meminfo`] reads on the
/// `wsl` lane, and two direct file reads on a Linux `host` lane.
///
/// `threads_used / threads_max` — both whole-kernel, both from `/proc`, and
/// both the *same quantity*: `/proc/loadavg`'s fourth field counts kernel
/// scheduling entities (threads), which is exactly what `kernel.threads-max`
/// bounds. That agreement is the property `saturation_source` exists to record;
/// the label is `"proc"` because the instrument is procfs, and the column NAME
/// (`threads_used`, not `pids_used`) is what fixes the quantity as threads —
/// coord's `cgroup`-vs-`proc` note is about which *pids* count a publisher
/// took, and this publisher takes none.
///
/// This is the WIDER of the two Linux arms and therefore the fallback, not the
/// preference: see [`parse_cgroup_saturation`] for the narrower one and
/// [`Saturation`] for why a ceiling that actually bounds the lane wins.
///
/// Parsing is by shape, not by position — see [`WSL_PROC_FILES`]. The files are
/// unambiguous against each other: every `meminfo` line carries a `Key:`,
/// `threads-max` is a lone integer, and `loadavg` is the one line whose fourth
/// whitespace-separated field is `a/b`.
fn parse_proc_saturation(text: &str) -> Option<Saturation> {
    let threads_max = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|l| l.parse::<i64>().ok());

    let threads_used = text.lines().find_map(|l| {
        let fields: Vec<&str> = l.split_whitespace().collect();
        if fields.len() < 5 {
            return None;
        }
        let (_runnable, total) = fields[3].split_once('/')?;
        total.parse::<i64>().ok()
    });

    Saturation::threads(threads_used, threads_max, SaturationSource::Proc)
}

/// One pids-controller value: a bare integer, or cgroup **v2**'s literal `max`.
///
/// `None` — never 0, never a sentinel — for:
///
/// * **`max`**, which is how cgroup v2 spells UNBOUNDED. This is not an edge
///   case: it is exactly what `docker inspect` reported as `PidsLimit=<nil>`
///   during the 2026-08-27 incident, when nothing bounded the container that
///   consumed the whole kernel task table. A sentinel here would render an
///   unbounded cgroup as *saturated*, and a 0 would make the consumer's ratio
///   divide by zero. "Nothing bounds this scope" is a real and important fact,
///   and NULL is how the schema spells it.
/// * an absent, empty, non-numeric or negative read — the file did not answer,
///   which is UNKNOWN and not a measurement.
#[cfg_attr(windows, allow(dead_code))]
fn parse_cgroup_value(text: &str) -> Option<i64> {
    let token = text.split_whitespace().next()?;
    if token == "max" {
        return None;
    }
    token.parse::<i64>().ok().filter(|v| *v >= 0)
}

/// The `"cgroup"` arm: a pids-controller count against the ceiling that bounds
/// it, over the cgroup **v1** paths with the **v2** paths as fallback.
///
/// The two paths, in this order, are the ones `process_health.rs:510` already
/// reads (`/sys/fs/cgroup/pids/pids.max`, then `/sys/fs/cgroup/pids.max`), and
/// the count is their siblings — matching that module's vocabulary and its
/// semantics rather than minting a second one.
///
/// **The fallback is keyed on whether a path ANSWERED, not on whether its value
/// parsed.** A v1 `pids.max` reading `max` means unbounded *on the hierarchy
/// this lane is actually in*; falling through to v2 on that would publish a
/// ceiling from a hierarchy that does not bound this lane at all — a number
/// that looks like a measurement and is about somewhere else. An unbounded
/// hierarchy publishes nothing, which is the honest answer and is what coord's
/// `is_unmeasured` skip is built for.
///
/// Each argument is the file's CONTENT, or `None` when the read failed, so the
/// whole decision is pure and testable without a cgroup mount.
#[cfg_attr(windows, allow(dead_code))]
fn parse_cgroup_saturation(
    pids_max_v1: Option<&str>,
    pids_max_v2: Option<&str>,
    pids_current_v1: Option<&str>,
    pids_current_v2: Option<&str>,
) -> Option<Saturation> {
    fn answered(text: Option<&str>) -> Option<&str> {
        text.filter(|t| !t.trim().is_empty())
    }
    let max = answered(pids_max_v1)
        .or_else(|| answered(pids_max_v2))
        .and_then(parse_cgroup_value);
    let used = answered(pids_current_v1)
        .or_else(|| answered(pids_current_v2))
        .and_then(parse_cgroup_value);
    Saturation::pids(used, max, SaturationSource::Cgroup)
}

/// [`parse_cgroup_saturation`] over the live filesystem.
#[cfg(not(windows))]
fn cgroup_saturation() -> Option<Saturation> {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let (max_v1, max_v2) = (
        read("/sys/fs/cgroup/pids/pids.max"),
        read("/sys/fs/cgroup/pids.max"),
    );
    let (used_v1, used_v2) = (
        read("/sys/fs/cgroup/pids/pids.current"),
        read("/sys/fs/cgroup/pids.current"),
    );
    parse_cgroup_saturation(
        max_v1.as_deref(),
        max_v2.as_deref(),
        used_v1.as_deref(),
        used_v2.as_deref(),
    )
}

/// The `host` lane's saturation reading on **Windows**, or `None`.
///
/// `None` on this fleet today, and that is the correct answer rather than a gap
/// to paper over: **Windows exposes no system-wide thread or handle ceiling.**
/// `GetPerformanceInfo` reports live `ThreadCount` / `HandleCount` /
/// `ProcessCount` with no bound for any of them, and the per-process
/// handle-table maximum (2^24) is not a system quantity. A job object's
/// `ActiveProcessLimit` is the one readable, *enforced* bound in this family,
/// so it is what the `"job_object"` arm publishes — and where no job sets one,
/// the lane publishes nothing at all and coord's `is_unmeasured` skip leaves
/// the row's memory and disk verdicts intact.
///
/// Publishing a live thread count against a NULL ceiling instead would pin the
/// host row to `unknown` forever; see [`Saturation`]. **Do not "fix" this by
/// inventing a ceiling** — a fabricated denominator is the one failure mode
/// worse than the blind spot this plan closed.
///
/// `pub(crate)` because `ci_node::admission::probe_headroom` reads the
/// SAME function for its local defer decision. The node's threshold and coord's
/// dashboard must be two instants of one instrument, never two instruments —
/// §C1 of plan `2026-08-02-fleet-resource-telemetry-and-ci-allocation` is the
/// recorded cost of getting that wrong (a correctly-shared ratio with an
/// unshared threshold, and a strip that disagreed with the dispatcher anyway).
#[cfg(windows)]
pub(crate) fn host_saturation() -> Option<Saturation> {
    let (used, max) = qontinui_runner_win32::current_job_pid_saturation()?;
    Saturation::pids(Some(used), Some(max), SaturationSource::JobObject)
}

/// The `host` lane's saturation reading on **Linux**, straight off the
/// filesystem with no subprocess at all.
///
/// **The narrower ceiling first.** A pids cgroup that declares a limit is the
/// constraint that actually binds this process tree — a containerised runner at
/// 95 of a 100-PID cgroup limit is out of PIDs while `kernel.threads-max` still
/// reads 0.05% used — so the `"cgroup"` arm leads and the whole-kernel
/// `"proc"` pair is the fallback for the ordinary case where nothing narrower
/// bounds the lane. That is the same judgement, in the same direction, as the
/// 2026-08-27 diagnosis: the *binding* ceiling is the one to divide by, and
/// which one that is has to be read, never assumed.
///
/// Exactly one pair is ever published, so coord's threads-first preference is
/// satisfied trivially — see [`Saturation`]'s "one pair per lane" note.
///
/// The `/proc` half is read into the same text shape [`parse_proc_saturation`]
/// parses on the `wsl` lane, so the two lanes cannot drift into two definitions
/// of one ratio.
///
/// `pub(crate)` for the same reason as the Windows arm above.
#[cfg(not(windows))]
pub(crate) fn host_saturation() -> Option<Saturation> {
    if let Some(bounded) = cgroup_saturation() {
        return Some(bounded);
    }
    let mut text = std::fs::read_to_string("/proc/sys/kernel/threads-max").unwrap_or_default();
    text.push('\n');
    text.push_str(&std::fs::read_to_string("/proc/loadavg").unwrap_or_default());
    parse_proc_saturation(&text)
}

// ---------------------------------------------------------------------------
// Publish
// ---------------------------------------------------------------------------

/// Read `active_tenant_id` from `~/.qontinui/machine.json` — the DEVICE-LEVEL
/// DEFAULT binding, which is the right attribution for a machine-scoped
/// observation. `None` for single-tenant operators; coord attributes the
/// sample to the device's resolved tenant regardless.
fn resolve_tenant_id() -> Option<String> {
    let path: PathBuf = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value.get("active_tenant_id").and_then(|v| v.as_str())?;
    let raw = raw.trim();
    (!raw.is_empty()).then(|| raw.to_string())
}

/// Collect every lane this machine has, as one observation instant.
pub(crate) async fn collect() -> Vec<ResourceSample> {
    let host = spawn_blocking_tracked(collect_host_lane).await.ok();
    let wsl = collect_wsl_lane().await;
    host.into_iter().chain(wsl).collect()
}

/// Take one observation and POST it. Best-effort end to end: every failure
/// path returns quietly, so a coord outage costs the sample and nothing else.
///
/// **Not gated here.** The caller — [`crate::fleet::spawn_budget_republisher`]
/// — is already gated on shared-machine-state ownership, and this function is
/// deliberately reachable only from inside it. Re-checking would restate the
/// rule in a second place; the whole lesson of the 2026-07-28 outage is that a
/// rule keyed in one place is the one that survives the next mechanism.
pub(crate) async fn publish_once() {
    let Some(device) = crate::fleet::load_device_file() else {
        debug!("fleet::resource_sample: no ~/.qontinui/machine.json — skipping");
        return;
    };
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        debug!("fleet::resource_sample: no coord base configured — skipping");
        return;
    };

    // Bearer BEFORE the probes: the door is device-keyed (coord binds the path
    // `:device_id` to the device principal, so the allocator's inputs are not a
    // wider attack surface than the allocator itself), and there is no
    // anonymous fallback. On an unpaired or auth-wedged device this returns
    // every tick, so resolving it first is what stops the sampler forking
    // `wsl.exe` twice a minute forever for a POST that can never be sent.
    let Some(bearer) = crate::coord_mcp::read_usable_device_jwt().await else {
        debug!("fleet::resource_sample: no usable device JWT — skipping this tick");
        return;
    };

    let samples = collect().await;
    if samples.is_empty() {
        debug!("fleet::resource_sample: no lane produced a sample this tick");
        return;
    }

    let body = ResourceSampleReq {
        device_id: device.device_id.clone(),
        tenant_id: resolve_tenant_id(),
        sampled_at: chrono::Utc::now().to_rfc3339(),
        samples,
    };

    let url = format!(
        "{}/coord/devices/{}/resource-sample",
        base.trim_end_matches('/'),
        device.device_id
    );

    let client = match reqwest::Client::builder().timeout(POST_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            debug!("fleet::resource_sample: http client build failed: {e}");
            return;
        }
    };

    // coord-auth-exempt(device-jwt-required): resolves through
    // `coord_mcp::read_usable_device_jwt` and returns early without one, so a
    // sample is skipped rather than published anonymously.
    match client
        .post(&url)
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            debug!(
                "fleet::resource_sample: published {} lane(s)",
                body.samples.len()
            );
        }
        Ok(resp) => {
            let status = resp.status();
            warn_once(format!("POST {url} -> HTTP {status}"));
        }
        Err(e) => warn_once(format!("POST {url} failed: {e}")),
    }
}

/// Log a publish failure loudly the first few times, then quietly.
///
/// A 30s sampler against a coord that is down for an hour would otherwise emit
/// 120 identical WARNs and bury whatever the operator is actually reading. The
/// first ones are the diagnosis; the rest are noise.
fn warn_once(msg: String) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEEN: AtomicU32 = AtomicU32::new(0);
    if SEEN.fetch_add(1, Ordering::Relaxed) < 3 {
        warn!("fleet::resource_sample: {msg} (best-effort — sample dropped)");
    } else {
        debug!("fleet::resource_sample: {msg} (best-effort — sample dropped)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_cadence_is_floored_and_much_faster_than_the_budget() {
        // The floor is what stops a hand-edit turning a 30s observation into a
        // `wsl.exe` fork storm.
        assert!(SAMPLE_MIN_SECS >= 10);
        assert!(
            SAMPLE_DEFAULT_SECS < crate::fleet::BUDGET_REPUBLISH_DEFAULT_SECS,
            "the sample is an observation and must out-pace the authoritative \
             budget re-assert, not match it"
        );
    }

    #[test]
    fn jitter_stays_within_the_band() {
        for _ in 0..200 {
            let d = jittered_sleep(30).as_secs();
            assert!((24..=36).contains(&d), "jittered 30s tick out of band: {d}");
        }
        // The floor case must not underflow to zero.
        for _ in 0..50 {
            let d = jittered_sleep(10).as_secs();
            assert!((8..=12).contains(&d), "jittered 10s tick out of band: {d}");
        }
    }

    #[test]
    fn the_spawn_gate_reading_is_the_host_lane_and_only_the_host_lane() {
        // The gate looks its fleet floors up by this exact string
        // (`mcp::fleet_policy_poller::SessionFloorsByLane::for_lane`), and an
        // unrecognised lane there yields NO floors — a renamed lane must break
        // here rather than silently disable the fleet term. It must also never
        // be the `wsl` lane: a pre-PTY gate cannot afford the `wsl.exe` fork
        // behind WSL_PROBE_TIMEOUT.
        let (lane, _) = spawn_gate_reading();
        assert_eq!(lane, "host");
        assert_eq!(lane, Lane::Host.as_str());
        // Same lane the publisher labels its host row with, so the gate and the
        // dashboard are talking about the same pool.
        assert_eq!(lane, collect_host_lane().lane);
    }

    #[cfg(windows)]
    #[test]
    fn the_spawn_gate_reading_carries_the_commit_figure_the_gate_reads() {
        // On Windows the guard ladder's one memory quantity is free COMMIT
        // (`available_commit_bytes`, plan §A3). If the reading did not carry
        // it, the gate would silently have nothing to compare against — and
        // because the gate fails OPEN on `None`, that failure would be invisible
        // rather than loud.
        let (_, commit) = spawn_gate_reading();
        assert!(
            commit.is_some(),
            "GlobalMemoryStatusEx must populate the free-commit figure on Windows"
        );
        // It is the SAME probe the publisher's sample carries, which is the
        // property that keeps the gate's number and the dashboard's comparable.
        assert_eq!(commit.is_some(), available_commit_bytes().is_some());
        assert!(collect_host_lane().commit_total_bytes.unwrap_or(0) > 0);
    }

    #[test]
    fn meminfo_parses_totals_and_derives_swap_used() {
        let text = "\
MemTotal:       16375488 kB
MemFree:        15122944 kB
MemAvailable:   15335424 kB
SwapTotal:       8388608 kB
SwapFree:        8036352 kB
";
        let s = parse_meminfo(text, "Ubuntu-24.04".to_string()).expect("parses");
        assert_eq!(s.lane, "wsl");
        assert_eq!(s.lane_instance.as_deref(), Some("Ubuntu-24.04"));
        assert_eq!(s.mem_total_bytes, Some(16_375_488 * 1024));
        assert_eq!(s.mem_available_bytes, Some(15_335_424 * 1024));
        assert_eq!(s.swap_total_bytes, Some(8_388_608 * 1024));
        // used = total - free, so the ratio is meaningful against the ceiling.
        assert_eq!(s.swap_used_bytes, Some((8_388_608 - 8_036_352) * 1024));
        // No commit concept inside the VM, and the runner cannot see the
        // Actions runners' occupancy from outside it.
        assert_eq!(s.commit_available_bytes, None);
        assert_eq!(s.ci_jobs_running, None);
    }

    #[test]
    fn meminfo_missing_keys_are_unknown_never_zero() {
        // MemAvailable absent (an old kernel, or a truncated read): the field
        // must stay None. A 0 here would render as "no memory left".
        let s = parse_meminfo("MemTotal:  1024 kB\n", "d".to_string()).expect("parses");
        assert_eq!(s.mem_available_bytes, None);
        assert_eq!(s.swap_total_bytes, None);
        assert_eq!(s.swap_used_bytes, None);
        // Without MemTotal there is no lane at all.
        assert!(parse_meminfo("SwapTotal: 10 kB\n", "d".to_string()).is_none());
    }

    #[test]
    fn unknown_fields_are_omitted_from_the_wire_not_sent_as_null_zero() {
        let s = ResourceSample::empty(Lane::Host, None);
        let v = serde_json::to_value(&s).expect("serializes");
        let obj = v.as_object().expect("object");
        assert_eq!(obj.get("lane").and_then(|v| v.as_str()), Some("host"));
        assert_eq!(obj.get("source").and_then(|v| v.as_str()), Some("runner"));
        assert!(
            !obj.contains_key("mem_available_bytes"),
            "an unmeasured field must be absent, never 0 — a zero looks like a \
             measurement"
        );
        assert!(!obj.contains_key("lane_instance"));
    }

    #[test]
    fn distro_listing_survives_the_bom_and_blank_lines() {
        assert_eq!(
            first_distro("\u{feff}Ubuntu-24.04\r\ndocker-desktop\r\n"),
            Some("Ubuntu-24.04".to_string())
        );
        assert_eq!(first_distro("\r\n\r\n"), None);
        assert_eq!(first_distro(""), None);
    }

    #[test]
    fn batch_envelope_carries_the_device_and_every_lane_together() {
        let body = ResourceSampleReq {
            device_id: "dev-1".to_string(),
            tenant_id: None,
            sampled_at: "2026-08-06T00:00:00Z".to_string(),
            samples: vec![
                ResourceSample::empty(Lane::Host, None),
                ResourceSample::empty(Lane::Wsl, Some("Ubuntu-24.04".to_string())),
            ],
        };
        let v = serde_json::to_value(&body).expect("serializes");
        assert_eq!(v["device_id"], "dev-1");
        assert!(v.get("tenant_id").is_none(), "absent, not null");
        let lanes: Vec<&str> = v["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["lane"].as_str().unwrap())
            .collect();
        assert_eq!(
            lanes,
            vec!["host", "wsl"],
            "both lanes ride one POST so a lane can never land without its \
             sibling — the '9 GB free WSL beside a 900 MB host' reading"
        );
    }

    /// The whole point of Phase 3, in one assertion: the row the 2026-08-27
    /// incident would have produced.
    ///
    /// 190,840 tasks against a `threads-max` of 192,146 is 99.3% — while the
    /// same box reported 73.3 GB free commit and `vmmemWSL` at ~21% of its
    /// ceiling, and `/admin/coord/devops` was green on every lane. The
    /// publisher's half of "would the dashboard have shown it?" is that both
    /// numbers reach coord, in one row, correctly labelled.
    #[test]
    fn the_incident_row_reaches_coord_with_a_ratio_of_zero_point_nine_nine() {
        let text = "\
MemTotal:       25165824 kB
MemAvailable:   19922944 kB
192146
1.20 1.31 1.44 91/190840 3312551
";
        let mut s = parse_meminfo(text, "Ubuntu-24.04".to_string()).expect("parses");
        s.set_saturation(parse_proc_saturation(text));

        assert_eq!(s.threads_used, Some(190_840));
        assert_eq!(s.threads_max, Some(192_146));
        assert_eq!(s.saturation_source.as_deref(), Some("proc"));
        // The pids pair stays NULL: this lane has no cgroup ceiling, and coord
        // reads the threads pair first anyway.
        assert_eq!(s.pids_used, None);
        assert_eq!(s.pids_max, None);

        let ratio = s.threads_used.unwrap() as f64 / s.threads_max.unwrap() as f64;
        assert!(
            ratio > 0.99,
            "the incident reading must land above coord's 0.80 saturation floor, \
             not below it: {ratio}"
        );
        // …while every memory field on the same row reads healthy, which is
        // precisely why an independent axis was needed.
        assert!(s.mem_available_bytes.unwrap() * 100 / s.mem_total_bytes.unwrap() > 50);
    }

    /// Half a pair must publish NOTHING — the single most important property of
    /// this publisher, and the one that is invisible until it deploys.
    ///
    /// coord grades the saturation axis the moment a row carries any of the
    /// four columns, and an ungradeable pair renders `Unknown`, which outranks
    /// `Warn` and `Ok` in its worst-of `headroom`. A publisher that emitted a
    /// count with no ceiling would therefore pin EVERY row it wrote to
    /// `unknown` — destroying memory and disk verdicts that were perfectly
    /// good — in exchange for a number nothing can divide.
    #[test]
    fn half_a_pair_publishes_nothing_at_all() {
        let meminfo = "MemTotal: 1024 kB\n";

        // A count with no ceiling (no `/proc/sys/kernel/threads-max` on this
        // kernel, or `cat` could not read it).
        let count_only = format!("{meminfo}0.10 0.20 0.30 2/4242 999\n");
        assert_eq!(parse_proc_saturation(&count_only), None);

        // A ceiling with no count (`/proc/loadavg` missing or truncated).
        let ceiling_only = format!("{meminfo}192146\n");
        assert_eq!(parse_proc_saturation(&ceiling_only), None);

        // And neither — the ordinary pre-Phase-3 shape, which coord's
        // `is_unmeasured` skip is built for.
        assert_eq!(parse_proc_saturation(meminfo), None);

        // Every rejection leaves all five wire fields absent, never zeroed.
        for text in [&count_only, &ceiling_only, &meminfo.to_string()] {
            let mut s = parse_meminfo(text, "d".to_string()).expect("parses");
            s.set_saturation(parse_proc_saturation(text));
            assert_eq!(s.threads_used, None);
            assert_eq!(s.threads_max, None);
            assert_eq!(s.pids_used, None);
            assert_eq!(s.pids_max, None);
            assert_eq!(s.saturation_source, None);
        }
    }

    /// A zero ceiling is not a ceiling: coord's ratio divides by it.
    #[test]
    fn a_zero_ceiling_is_rejected_rather_than_divided_by() {
        let text = "MemTotal: 1024 kB\n0\n0.10 0.20 0.30 2/4242 999\n";
        assert_eq!(parse_proc_saturation(text), None);
        assert_eq!(
            Saturation::threads(Some(10), Some(0), SaturationSource::Proc),
            None
        );
        assert_eq!(
            Saturation::pids(Some(10), Some(0), SaturationSource::JobObject),
            None
        );
    }

    /// cgroup **v2** spells "no limit" as the literal string `max`, and that
    /// must publish NOTHING.
    ///
    /// This is the incident's own reading one level down: `docker inspect`
    /// showed `PidsLimit=<nil>` while the container consumed the whole kernel
    /// task table. A sentinel would render an unbounded cgroup as *saturated*
    /// and a 0 would divide by zero — "nothing bounds this scope" is a real
    /// fact and NULL is how the schema spells it.
    #[test]
    fn cgroup_v2_unbounded_max_publishes_nothing() {
        assert_eq!(parse_cgroup_value("max\n"), None);
        assert_eq!(
            parse_cgroup_saturation(None, Some("max\n"), None, Some("57\n")),
            None,
            "an unbounded ceiling is not a ceiling, however good the count is"
        );
        // …and it is not mistaken for a parse failure that could fall through
        // to the OTHER hierarchy: v1 says unbounded, so v2's 100 is about a
        // hierarchy that does not bound this lane.
        assert_eq!(
            parse_cgroup_saturation(Some("max\n"), Some("100\n"), Some("57\n"), Some("57\n")),
            None,
            "the v1→v2 fallback is keyed on whether the path ANSWERED, not on \
             whether its value parsed"
        );
    }

    /// The cgroup **v1 → v2 path** fallback, in both directions, and the label
    /// it publishes.
    #[test]
    fn the_cgroup_arm_falls_back_from_the_v1_paths_to_the_v2_paths() {
        // v1 absent (a pure cgroup-v2 host): v2 answers.
        let v2_only = parse_cgroup_saturation(None, Some("4096\n"), None, Some("57\n"))
            .expect("a bounded v2 hierarchy is a complete pair");
        assert_eq!(v2_only.ratio(), 57.0 / 4096.0);
        let mut s = ResourceSample::empty(Lane::Host, None);
        s.set_saturation(Some(v2_only));
        assert_eq!(s.pids_used, Some(57));
        assert_eq!(s.pids_max, Some(4096));
        assert_eq!(s.saturation_source.as_deref(), Some("cgroup"));
        assert_eq!(s.threads_used, None, "one pair per lane, never both");

        // Both present (a hybrid mount): v1 leads, the same order
        // `process_health.rs:510` reads them in.
        let v1_wins =
            parse_cgroup_saturation(Some("200\n"), Some("4096\n"), Some("100\n"), Some("57\n"))
                .expect("complete pair");
        assert_eq!(v1_wins.ratio(), 0.5);

        // An empty read is "did not answer", not "answered with nothing".
        let blank_v1 = parse_cgroup_saturation(Some("\n"), Some("4096\n"), Some(""), Some("57\n"))
            .expect("complete pair");
        assert_eq!(blank_v1.ratio(), 57.0 / 4096.0);
    }

    /// A missing or unreadable cgroup file is NULL, never 0 — and half a pair
    /// still publishes nothing.
    #[test]
    fn an_unreadable_cgroup_file_is_null_never_zero() {
        assert_eq!(parse_cgroup_saturation(None, None, None, None), None);
        assert_eq!(
            parse_cgroup_saturation(None, None, Some("57\n"), None),
            None,
            "a count with no ceiling is half a pair"
        );
        assert_eq!(
            parse_cgroup_saturation(Some("4096\n"), None, None, None),
            None,
            "a ceiling with no count is half a pair"
        );
        // Garbage in a file that exists is still UNKNOWN, not a measurement.
        assert_eq!(parse_cgroup_value("not-a-number\n"), None);
        assert_eq!(parse_cgroup_value(""), None);
        assert_eq!(parse_cgroup_value("-1\n"), None);
        // A zero ceiling is rejected here too — coord's ratio divides by it.
        assert_eq!(
            parse_cgroup_saturation(Some("0\n"), None, Some("0\n"), None),
            None
        );
    }

    /// The parser reads by SHAPE, so a file missing from the middle of the
    /// `cat` costs its own reading and nothing else — which is why the probe's
    /// exit status is no longer a gate on the whole lane.
    #[test]
    fn the_lane_survives_a_missing_file_in_the_middle_of_the_probe() {
        // meminfo + loadavg, with `threads-max` absent: memory still lands.
        let text = "\
MemTotal:       16375488 kB
MemAvailable:   15335424 kB
0.52 0.58 0.59 3/1234 5678
";
        let mut s = parse_meminfo(text, "Ubuntu".to_string()).expect("the lane survives");
        s.set_saturation(parse_proc_saturation(text));
        assert_eq!(s.mem_total_bytes, Some(16_375_488 * 1024));
        assert_eq!(s.threads_used, None, "and the axis alone goes dark");
    }

    /// The `job_object` arm fills the PID pair, not the thread pair, and says
    /// so on the wire.
    #[test]
    fn the_job_object_arm_fills_the_pid_pair_and_labels_itself() {
        let mut s = ResourceSample::empty(Lane::Host, None);
        s.set_saturation(Saturation::pids(
            Some(37),
            Some(512),
            SaturationSource::JobObject,
        ));
        assert_eq!(s.pids_used, Some(37));
        assert_eq!(s.pids_max, Some(512));
        assert_eq!(s.threads_used, None, "Windows publishes no thread ceiling");
        assert_eq!(s.threads_max, None);
        assert_eq!(s.saturation_source.as_deref(), Some("job_object"));
    }

    /// The wire vocabulary must stay inside coord's `KNOWN_SATURATION_SOURCES`.
    ///
    /// The column is free TEXT with no CHECK — an unrecognised value is
    /// *stored*, never rejected — so nothing but this test stands between a
    /// renamed variant and a provenance label no consumer can interpret.
    #[test]
    fn saturation_sources_stay_inside_coords_vocabulary() {
        for source in [
            SaturationSource::Cgroup,
            SaturationSource::Proc,
            SaturationSource::JobObject,
        ] {
            assert!(
                ["cgroup", "proc", "job_object"].contains(&source.as_str()),
                "{source:?} emits {:?}, which coord's KNOWN_SATURATION_SOURCES \
                 does not list",
                source.as_str()
            );
        }
    }

    /// Unmeasured saturation must be ABSENT from the wire, not sent as 0.
    #[test]
    fn unmeasured_saturation_is_omitted_from_the_wire() {
        let s = ResourceSample::empty(Lane::Wsl, Some("Ubuntu".to_string()));
        let v = serde_json::to_value(&s).expect("serializes");
        let obj = v.as_object().expect("object");
        for key in [
            "threads_max",
            "threads_used",
            "pids_max",
            "pids_used",
            "saturation_source",
        ] {
            assert!(
                !obj.contains_key(key),
                "{key} must be absent when unmeasured — a 0 here renders as \
                 perfectly idle on the axis built to catch a box at 99.3%"
            );
        }
    }

    /// **One fork.** The saturation reading rides the `cat` the lane already
    /// ran; a second `wsl.exe` probe to publish a fork-exhaustion metric would
    /// make this module a contributor to the next incident rather than its
    /// detector — the 2026-08-27 event accumulated 512 stuck `wsl.exe` (98k
    /// handles, 23% of all system handles) from the monitoring path alone.
    ///
    /// Structural, because the behaviour needs a live WSL VM to exercise.
    #[test]
    fn the_wsl_lane_still_takes_exactly_one_fork() {
        const SRC: &str = include_str!("resource_sample.rs");
        let prod = SRC
            .split_once("\n#[cfg(test)]")
            .map(|(a, _)| a)
            .unwrap_or(SRC);
        let start = prod
            .find("async fn collect_wsl_lane()")
            .expect("the WSL lane collector must exist");
        let body = &prod[start..];
        let end = body[1..]
            .find("\n#[cfg(")
            .map(|i| i + 1)
            .unwrap_or(body.len());
        let body = &body[..end];
        assert_eq!(
            body.matches("wsl_probe(").count(),
            1,
            "collect_wsl_lane must fork `wsl.exe` exactly once — every file it \
             reads rides WSL_PROC_FILES on that one `cat`"
        );
        assert!(
            body.contains("WSL_PROC_FILES"),
            "the file list must be the shared constant, not a second literal"
        );
    }

    /// Production source of `fleet.rs`, with its test module split off so a
    /// call site written inside a test cannot satisfy a pin that production
    /// code fails.
    fn fleet_prod_src() -> &'static str {
        const SRC: &str = include_str!("../fleet.rs");
        SRC.split_once("\n#[cfg(test)]")
            .map(|(before, _)| before)
            .unwrap_or(SRC)
    }

    /// §A2, the load-bearing one: **a secondary must publish no samples.**
    ///
    /// Every runner instance on a box shares one `~/.qontinui/machine.json`,
    /// hence one `device_id`, so a `test-*` runner's samples are
    /// indistinguishable from the primary's — and coord's allocator reads them.
    /// The 2026-07-28 outage is the same shape one column over: a temp runner
    /// wrote `max_concurrent_builds = 0` over the primary's row and coord's
    /// shadow lane elected no device for six days.
    ///
    /// The sampler therefore has **no gate of its own**. It is driven from
    /// `spawn_budget_republisher`, which returns before spawning anything when
    /// this instance does not own shared machine-wide state. This test pins that
    /// structurally — the ownership check must come *before* the sampler in
    /// that function — because the behaviour cannot be exercised directly: the
    /// predicate reads `QONTINUI_PORT`, which other harness threads mutate, so
    /// an env-driven assertion here would flake (as
    /// `instance::primary_keeps_the_unscoped_path` records).
    ///
    /// Keyed on `owns_shared_root_state()`, the ownership helper, rather than
    /// on the name of the wrapper predicate — the wrapper is being renamed by
    /// PR #951 as it grows to cover the heartbeat, the tree publisher and the
    /// worktree census, and a pin that breaks on a rename pins the wrong thing.
    #[test]
    fn a_secondary_never_reaches_the_sampler() {
        let src = fleet_prod_src();
        let fn_start = src
            .find("pub fn spawn_budget_republisher(")
            .expect("the sampler's host function must exist");
        let body = &src[fn_start..];
        // Bound the search to this function: the next top-level item.
        let end = body[1..]
            .find("\npub ")
            .map(|i| i + 1)
            .unwrap_or(body.len());
        let body = &body[..end];

        let gate = body
            .find("owns_shared_root_state()")
            .expect("spawn_budget_republisher must consult shared-state ownership");
        let sampler = body
            .find("resource_sample::publish_once()")
            .expect("the sampler must be driven from the already-gated republisher");
        assert!(
            gate < sampler,
            "the ownership gate must precede the sampler, or a secondary would \
             publish samples over the primary's device row"
        );

        assert_eq!(
            src.matches("resource_sample::publish_once()").count(),
            1,
            "exactly one production call site — a second one would need its own \
             gate, and the lesson of 2026-07-28 is that a rule keyed in one \
             place is the one that survives"
        );
    }

    /// The sampler must not have grown a timer of its own (plan §A2 corrected:
    /// the periodic publisher already exists; adding a second one is the defect).
    #[test]
    fn the_sampler_has_no_timer_of_its_own() {
        const SRC: &str = include_str!("resource_sample.rs");
        let prod = SRC
            .split_once("\n#[cfg(test)]")
            .map(|(a, _)| a)
            .unwrap_or(SRC);
        assert!(
            !prod.contains("tokio::spawn"),
            "the sampler rides `spawn_budget_republisher`'s loop — a task of its \
             own would escape that function's secondary-instance gate"
        );
    }

    /// §A3: the exported floor probe and the snapshot's
    /// `commit_available_bytes` are the same reading, taken once.
    ///
    /// Asserted on the *shape* rather than by comparing two calls: free commit
    /// moves between any two reads, so a value comparison would be a flake
    /// generator. What must hold is that the pair is coherent — either the
    /// machine reports both numbers or neither, and used can never exceed the
    /// ceiling. A `commit_available > commit_total` row would make every
    /// downstream ratio nonsense.
    #[test]
    fn commit_probe_reports_a_coherent_pair_or_nothing() {
        match commit_status() {
            Some((total, avail)) => {
                assert!(total > 0, "a commit ceiling of 0 is not a reading");
                assert!(
                    avail <= total,
                    "free commit {avail} exceeds the commit limit {total}"
                );
                assert!(available_commit_bytes().is_some());
            }
            None => {
                assert!(
                    available_commit_bytes().is_none(),
                    "the floor probe must go dark exactly when the snapshot does \
                     — a lane that fails open while the dashboard shows a number \
                     is the divergence §A3 exists to end"
                );
            }
        }
    }
}
