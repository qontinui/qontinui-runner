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
    /// `'runner' | 'supervisor' | 'ci-step'`. Distinguishes this row from the
    /// supervisor's host-lane row for the same device.
    pub(crate) source: String,
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
            source: "runner".to_string(),
        }
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
        let job = crate::job_object::ScopedKillOnCloseJob::create(None);
        if let (Some(j), Some(handle)) = (job.as_ref(), child.raw_handle()) {
            j.assign(handle as _);
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

/// Collect the `wsl` lane by reading the VM's own `/proc/meminfo`.
///
/// `None` when there is no WSL, the probe times out, or the output does not
/// parse — a missing lane is honest; a zeroed one is not.
#[cfg(windows)]
async fn collect_wsl_lane() -> Option<ResourceSample> {
    let distro = wsl_distro().await?;
    let out = wsl_probe(&["-d", &distro, "--", "cat", "/proc/meminfo"]).await?;
    if !out.status.success() {
        return None;
    }
    // `/proc/meminfo` is plain ASCII even though `wsl --list` is UTF-16.
    let text = String::from_utf8_lossy(&out.stdout);
    parse_meminfo(&text, distro)
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
    let host = tokio::task::spawn_blocking(collect_host_lane).await.ok();
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
