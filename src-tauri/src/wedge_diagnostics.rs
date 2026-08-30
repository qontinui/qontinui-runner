//! Runtime-independent diagnostic capture for a wedged runner.
//!
//! Plan `2026-08-30-runner-blocking-pool-exhaustion-and-wedge-diagnostics`,
//! Phase 4.
//!
//! **What this is for.** When the runner wedges, the process is alive, the HTTP
//! surface is dead, the thread count is climbing and there is no panic. The
//! 2026-08-30 incident was root-caused only by a multi-agent live-forensics
//! session that ran a manual thread census (`Get-CimInstance`) against a
//! process that was minutes from being restarted out from under the
//! investigation. This module automates that census so the *next* incident
//! arrives with its own evidence already on disk.
//!
//! **Three rules, all load-bearing, all inherited from
//! [`crate::health_monitor`]'s watchdog** (the module doc there explains why):
//!
//! 1. **Never `tracing`.** The tracing subscriber is a shared-fate path — a
//!    writer behind a lock, a bounded channel, a file handle, any of which can
//!    be held by a task the starved runtime will never schedule again. Every
//!    failure here is swallowed. There is not one `tracing` call in this file
//!    and there must never be one.
//! 2. **Never the async runtime.** The capture is driven from the
//!    `WedgeDetector`'s own dedicated OS thread. A `tokio` task would be parked
//!    by exactly the condition being reported.
//! 3. **Never WMI / `Get-CimInstance` / PowerShell.** WMI degradation is part
//!    of the failure being diagnosed; a diagnostic that depends on the broken
//!    subsystem is worthless precisely when it is needed. The thread census is
//!    a native kernel enumeration (`NtQuerySystemInformation` on Windows,
//!    `/proc/self/task` on Linux) and the child census is `sysinfo`, which is
//!    itself native on every platform.
//!
//! **And one more that is easy to get wrong: the capture must be BOUNDED.** It
//! runs on the one thread whose entire job is to keep ticking through a fully
//! parked runtime. A native enumeration with no timeout would risk wedging the
//! detector itself. [`crate::off_runtime::deadline`] is an *async* future and
//! is therefore unusable here, so this module implements the synchronous
//! equivalent: each capture step runs on a long-lived worker thread and is
//! awaited with `recv_timeout`. On expiry the field records `"timed_out"` and
//! the caller continues.
//!
//! Note the thread-leak trap [`crate::health_monitor`] documents for
//! `LivezProber` and which [`BoundedCapturer`] copies verbatim: **one** worker
//! thread for the process, never one per capture. A capture every ~5 minutes
//! through a multi-hour wedge would otherwise leave a pile of stuck threads
//! behind — a thread leak triggered by the very condition being reported.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::time::Duration;

// ─────────────────────────────────────────────────────────────────────────────
// Blocking-pool saturation counter
// ─────────────────────────────────────────────────────────────────────────────

/// Bodies currently EXECUTING on tokio's blocking pool, counted by
/// [`spawn_blocking_tracked`].
///
/// Queued-but-not-yet-started tasks are deliberately not counted: a blocking
/// pool slot is consumed by a running body, and saturation is exactly the state
/// where every slot is running. Counting the queue would report a number the
/// 512 ceiling cannot be compared against.
static BLOCKING_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Tokio's **documented default** `max_blocking_threads`.
///
/// This is the default, NOT a measured value. Nothing in this crate builds the
/// application runtime with an explicit `max_blocking_threads` — the only two
/// call sites are the deliberately-tiny fixtures in `health_monitor`'s own
/// tests — so the app runtime runs on tokio's default of 512. If a
/// `max_blocking_threads(..)` is ever added to the real runtime builder, this
/// constant must be read from the same place instead of restated here.
pub const BLOCKING_POOL_DEFAULT_CAPACITY: usize = 512;

/// RAII counter for one in-flight blocking body.
///
/// A guard rather than a manual decrement so a body that panics still gives its
/// slot back during unwind — a leaked count would make the saturation figure
/// climb monotonically and report a wedge that is not there.
pub struct BlockingSlot {
    _private: (),
}

impl BlockingSlot {
    /// Take a slot. Held until dropped.
    pub fn enter() -> Self {
        BLOCKING_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        Self { _private: () }
    }
}

impl Drop for BlockingSlot {
    fn drop(&mut self) {
        // `fetch_update` rather than `fetch_sub`: a saturating floor at 0 means
        // a hypothetical unbalanced drop can never wrap to `usize::MAX` and
        // publish a nonsense "18446744073709551615/512 slots in use".
        let _ = BLOCKING_IN_FLIGHT.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
            Some(n.saturating_sub(1))
        });
    }
}

/// Drop-in replacement for [`tokio::task::spawn_blocking`] that counts the
/// body while it runs.
///
/// Tokio exposes blocking-pool depth only through `RuntimeMetrics`, which needs
/// the `tokio_unstable` cfg — not enabled here, and (per the plan's Phase 5
/// non-goal for the shipped build) not something to enable for a metric. This
/// wrapper is the lightweight substitute: it makes
/// `"N/512 blocking-pool slots in use"` a fact the watchdog can state directly
/// instead of a number the reader has to infer from a total thread count.
pub fn spawn_blocking_tracked<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let _slot = BlockingSlot::enter();
        f()
    })
}

/// Blocking bodies executing right now. Never blocks; a plain atomic load.
pub fn blocking_pool_in_flight() -> usize {
    BLOCKING_IN_FLIGHT.load(Ordering::SeqCst)
}

/// The blocking pool's slot ceiling. See [`BLOCKING_POOL_DEFAULT_CAPACITY`].
pub fn blocking_pool_capacity() -> usize {
    BLOCKING_POOL_DEFAULT_CAPACITY
}

// ─────────────────────────────────────────────────────────────────────────────
// The sampled record — a pure data structure, serialized by a pure function
// ─────────────────────────────────────────────────────────────────────────────

/// A field that may not have been obtainable.
///
/// Serialized untagged, so a successful capture is the value itself and a
/// failed one is a bare string (`"timed_out"`, `"unsupported_platform"`,
/// `"enumeration_failed"`, `"capture_thread_unavailable"`). One field shape,
/// two readings, and a reader can never mistake "we could not look" for "there
/// was nothing there" — the same absence-is-not-zero discipline the rest of the
/// fleet's evidence surfaces follow.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Captured<T> {
    Value(T),
    Unavailable(&'static str),
}

impl<T> Captured<T> {
    /// The reason string, or `None` when the field actually holds a value.
    pub fn unavailable_reason(&self) -> Option<&'static str> {
        match self {
            Captured::Value(_) => None,
            Captured::Unavailable(r) => Some(r),
        }
    }
}

/// Per-thread wait-reason tally for THIS process.
///
/// `by_wait_reason` keys are the kernel's own `KWAIT_REASON` names on Windows
/// (`UserRequest`, `Executive`, `WrQueue`, …) and the `/proc` state letters
/// rendered long-hand on Linux (`Running`, `Sleeping`, `UninterruptibleDisk`,
/// …). Both come from a native enumeration, never WMI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadCensus {
    /// Plain total thread count, emitted alongside the tally so the headline
    /// number does not have to be summed out of the histogram.
    pub total: usize,
    pub by_wait_reason: BTreeMap<String, usize>,
}

/// One direct child process.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChildProcess {
    pub pid: u32,
    pub name: String,
    /// Seconds since the child started.
    pub elapsed_secs: u64,
    /// Approximate CPU share. Derived from a single `sysinfo` refresh, so it is
    /// coarse by construction — a shape indicator, not a measurement.
    pub cpu_percent: f32,
}

/// Census of the process's DIRECT children.
///
/// Capped on purpose. 324 direct children were observed live during the
/// 2026-08-30 incident; a record that inlined all of them would be a single
/// unreadable line, so the full population survives as `total` plus a
/// name-frequency tally, and only the oldest few are listed individually.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChildCensus {
    pub total: usize,
    /// Name → count, truncated to [`MAX_CHILD_NAMES`] most frequent.
    pub by_name: BTreeMap<String, usize>,
    /// The [`MAX_CHILDREN_LISTED`] longest-lived children. Oldest first —
    /// a leaked child is old, and age is what distinguishes it from the churn.
    pub oldest: Vec<ChildProcess>,
}

/// Blocking-pool occupancy at the moment of the escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BlockingPoolSnapshot {
    pub in_flight: usize,
    pub capacity: usize,
}

impl BlockingPoolSnapshot {
    /// Read the live counters. Two atomic loads — cannot block, so it needs no
    /// deadline of its own.
    pub fn sample() -> Self {
        Self {
            in_flight: blocking_pool_in_flight(),
            capacity: blocking_pool_capacity(),
        }
    }
}

/// One line of `wedge-diagnostics.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WedgeDiagnosticRecord {
    /// RFC3339, wall clock. For a human reading the file next to other logs —
    /// never for elapsed arithmetic (see `unresponsive_for_secs`).
    pub ts: String,
    pub pid: u32,
    /// `"escalate"` or `"re-escalate"` — the escalation that triggered this
    /// capture.
    pub event: &'static str,
    pub consecutive_failures: u32,
    /// Phase 3's REAL elapsed since the first failed probe of this incident,
    /// measured on a monotonic clock. Not `failures × nominal interval`.
    pub unresponsive_for_secs: u64,
    pub unresponsive_for_ms: u64,
    pub blocking_pool: BlockingPoolSnapshot,
    pub threads: Captured<ThreadCensus>,
    pub children: Captured<ChildCensus>,
}

/// Serialize one record as a single JSONL line (trailing `\n` included).
///
/// Pure: a function of the record alone, no clock and no threads, which is what
/// makes the wire shape testable. Cannot fail — a serialization error (there is
/// no reachable one, but the process is by assumption sick) degrades to a
/// minimal hand-built line rather than losing the record or panicking.
pub fn render_record(record: &WedgeDiagnosticRecord) -> String {
    match serde_json::to_string(record) {
        Ok(s) => {
            let mut s = s;
            s.push('\n');
            s
        }
        Err(_) => format!(
            "{{\"ts\":\"{}\",\"pid\":{},\"event\":\"{}\",\"consecutive_failures\":{},\
             \"unresponsive_for_secs\":{},\"render\":\"serialization_failed\"}}\n",
            record.ts.replace('"', "'"),
            record.pid,
            record.event,
            record.consecutive_failures,
            record.unresponsive_for_secs
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// File naming, rolling and retention — pure policy, testable off a fake listing
// ─────────────────────────────────────────────────────────────────────────────

/// Basename prefix of the diagnostics sink.
///
/// Daily-rolled as `wedge-diagnostics.jsonl.<YYYY-MM-DD>`, deliberately the
/// same shape as the runner's own `qontinui-runner.log.<date>` sink so
/// `crate::paths::resolve_newest_rolling_file` and every existing log consumer
/// can find it without a second convention.
pub const DIAGNOSTICS_PREFIX: &str = "wedge-diagnostics.jsonl";

/// Daily files retained, matching `logging.rs`'s `max_log_files(14)` — a
/// fortnight of history, which covers any post-mortem, and a hard cap on disk.
pub const DIAGNOSTICS_MAX_FILES: usize = 14;

/// Individually-listed children per record.
pub const MAX_CHILDREN_LISTED: usize = 10;

/// Distinct child names kept in the frequency tally per record.
pub const MAX_CHILD_NAMES: usize = 12;

/// Is `name` one of our rolled diagnostics files?
///
/// Matches `prefix` exactly and the `prefix.<date>` rotation form (`prefix.`
/// followed by a digit) — never arbitrary siblings like `prefix.bak`, so
/// retention can't delete something it did not write. Same predicate as
/// `crate::paths::resolve_newest_rolling_file`.
pub fn is_diagnostics_file(name: &str) -> bool {
    if name == DIAGNOSTICS_PREFIX {
        return true;
    }
    name.strip_prefix(&format!("{DIAGNOSTICS_PREFIX}."))
        .and_then(|suffix| suffix.chars().next())
        .is_some_and(|c| c.is_ascii_digit())
}

/// The diagnostics file for one UTC day.
pub fn diagnostics_file_name(day: &str) -> String {
    format!("{DIAGNOSTICS_PREFIX}.{day}")
}

/// Full path of the diagnostics file for one UTC day.
pub fn diagnostics_path(dir: &Path, day: &str) -> PathBuf {
    dir.join(diagnostics_file_name(day))
}

/// Retention policy, as a pure function of a directory listing.
///
/// Returns the names to delete: everything matching the sink prefix beyond the
/// newest `keep`. `YYYY-MM-DD` sorts lexicographically as it sorts
/// chronologically, so the newest-`keep` set needs no `stat` — which matters,
/// because the caller runs on the watchdog thread and every avoidable syscall
/// there is a syscall that cannot stall.
pub fn files_to_prune(names: &[String], keep: usize) -> Vec<String> {
    let mut ours: Vec<&String> = names
        .iter()
        .filter(|n| is_diagnostics_file(n.as_str()))
        .collect();
    ours.sort();
    if ours.len() <= keep {
        return Vec::new();
    }
    let cut = ours.len() - keep;
    ours[..cut].iter().map(|n| (*n).clone()).collect()
}

/// Keep only the `keep` most frequent entries of a tally (ties broken by name,
/// so the output is deterministic and diffable across records).
pub fn cap_tally(tally: BTreeMap<String, usize>, keep: usize) -> BTreeMap<String, usize> {
    if tally.len() <= keep {
        return tally;
    }
    let mut v: Vec<(String, usize)> = tally.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(keep);
    v.into_iter().collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Wait-reason naming — platform-independent, so it compiles and is tested
// everywhere even though only Windows produces these codes
// ─────────────────────────────────────────────────────────────────────────────

/// Render a Windows `KWAIT_REASON` code as its kernel name.
///
/// These are the kernel's own names, not WMI's `Win32_Thread.ThreadWaitReason`
/// spellings — the two differ (WMI says `EventPairLow`, the kernel says
/// `WrEventPair`), and this module reads the kernel directly on purpose.
pub fn wait_reason_name(code: u32) -> &'static str {
    match code {
        0 => "Executive",
        1 => "FreePage",
        2 => "PageIn",
        3 => "PoolAllocation",
        4 => "DelayExecution",
        5 => "Suspended",
        6 => "UserRequest",
        7 => "WrExecutive",
        8 => "WrFreePage",
        9 => "WrPageIn",
        10 => "WrPoolAllocation",
        11 => "WrDelayExecution",
        12 => "WrSuspended",
        13 => "WrUserRequest",
        14 => "WrEventPair",
        15 => "WrQueue",
        16 => "WrLpcReceive",
        17 => "WrLpcReply",
        18 => "WrVirtualMemory",
        19 => "WrPageOut",
        20 => "WrRendezvous",
        21 => "WrKeyedEvent",
        22 => "WrTerminated",
        23 => "WrProcessInSwap",
        24 => "WrCpuRateControl",
        25 => "WrCalloutStack",
        26 => "WrKernel",
        27 => "WrResource",
        28 => "WrPushLock",
        29 => "WrMutex",
        30 => "WrQuantumEnd",
        31 => "WrDispatchInt",
        32 => "WrPreempted",
        33 => "WrYieldExecution",
        34 => "WrFastMutex",
        35 => "WrGuardedMutex",
        36 => "WrRundown",
        37 => "WrAlertByThreadId",
        38 => "WrDeferredPreempt",
        39 => "WrPhysicalFault",
        40 => "WrIoRing",
        41 => "WrMdlCache",
        42 => "WrRcu",
        43 => "MaximumWaitReason",
        _ => "UnknownWaitReason",
    }
}

/// Render a Linux `/proc/<pid>/stat` state letter long-hand.
///
/// The Linux analogue of a Windows wait reason: it is what a per-thread census
/// can actually say on this platform, and it is what makes the capture path
/// exercised and testable off Windows instead of dead code.
pub fn linux_state_name(state: char) -> &'static str {
    match state {
        'R' => "Running",
        'S' => "Sleeping",
        'D' => "UninterruptibleDisk",
        'Z' => "Zombie",
        'T' => "Stopped",
        't' => "TracingStop",
        'W' => "Paging",
        'X' | 'x' => "Dead",
        'K' => "WakeKill",
        'P' => "Parked",
        'I' => "Idle",
        _ => "UnknownState",
    }
}

/// Extract the state letter (field 3) from a `/proc/<pid>/stat` line.
///
/// Field 2 is `comm`, which is parenthesised and may itself contain spaces and
/// parentheses (`(tokio (weird) name)`), so the only correct parse splits at
/// the LAST `)` rather than tokenising from the left. Pure, and tested against
/// exactly that hostile case.
pub fn parse_proc_stat_state(stat: &str) -> Option<char> {
    let after_comm = &stat[stat.rfind(')')? + 1..];
    after_comm.split_whitespace().next()?.chars().next()
}

/// Extract the parent pid (field 4) from a `/proc/<pid>/stat` line.
pub fn parse_proc_stat_ppid(stat: &str) -> Option<u32> {
    let after_comm = &stat[stat.rfind(')')? + 1..];
    after_comm.split_whitespace().nth(1)?.parse().ok()
}

// ─────────────────────────────────────────────────────────────────────────────
// The native capture steps
// ─────────────────────────────────────────────────────────────────────────────

/// Which capture to run. One step per deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStep {
    Threads,
    Children,
}

/// A capture step's answer. `None` means the platform (or the kernel call)
/// could not produce one — reported as `"unsupported_platform"` /
/// `"enumeration_failed"`, never as an empty census.
#[derive(Debug, Clone, PartialEq)]
pub enum StepOutput {
    Threads(Option<ThreadCensus>),
    Children(Option<ChildCensus>),
}

impl StepOutput {
    fn matches(&self, step: CaptureStep) -> bool {
        matches!(
            (self, step),
            (StepOutput::Threads(_), CaptureStep::Threads)
                | (StepOutput::Children(_), CaptureStep::Children)
        )
    }
}

/// Run one capture step natively. Called ONLY on the capture worker thread —
/// never inline on the watchdog thread, because neither enumeration is bounded
/// on its own.
pub fn run_capture_step(step: CaptureStep) -> StepOutput {
    match step {
        CaptureStep::Threads => StepOutput::Threads(capture_thread_census()),
        CaptureStep::Children => StepOutput::Children(capture_child_census()),
    }
}

/// Per-thread wait-reason census for this process.
#[cfg(target_os = "linux")]
pub fn capture_thread_census() -> Option<ThreadCensus> {
    let entries = std::fs::read_dir("/proc/self/task").ok()?;
    let mut total = 0usize;
    let mut by_wait_reason: BTreeMap<String, usize> = BTreeMap::new();
    for entry in entries.flatten() {
        total += 1;
        let name = std::fs::read_to_string(entry.path().join("stat"))
            .ok()
            .and_then(|s| parse_proc_stat_state(&s))
            .map(linux_state_name)
            .unwrap_or("UnknownState");
        *by_wait_reason.entry(name.to_string()).or_insert(0) += 1;
    }
    if total == 0 {
        return None;
    }
    Some(ThreadCensus {
        total,
        by_wait_reason,
    })
}

#[cfg(target_os = "windows")]
pub fn capture_thread_census() -> Option<ThreadCensus> {
    windows_thread_census::census()
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn capture_thread_census() -> Option<ThreadCensus> {
    // macOS has no per-thread enumeration through `sysinfo` and no `/proc`.
    // Reporting `unsupported_platform` is the honest answer; inventing an empty
    // census would read as "no threads", which is the one thing it cannot mean.
    None
}

/// Direct-children census, via `sysinfo`'s native enumeration.
///
/// `sysinfo` is used rather than a second hand-rolled `CreateToolhelp32Snapshot`
/// walk because it is already a dependency, is native (Toolhelp /
/// `NtQuerySystemInformation` on Windows, `/proc` on Linux — never WMI), and
/// gives name, start time and CPU in one pass on every platform. The Windows
/// thread census below cannot use it only because `sysinfo` has no per-thread
/// API at all.
pub fn capture_child_census() -> Option<ChildCensus> {
    use sysinfo::{ProcessesToUpdate, System};

    let me = sysinfo::Pid::from_u32(std::process::id());
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let mut kids: Vec<ChildProcess> = sys
        .processes()
        .values()
        .filter(|p| p.parent() == Some(me))
        .map(|p| ChildProcess {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().to_string(),
            elapsed_secs: p.run_time(),
            cpu_percent: p.cpu_usage(),
        })
        .collect();

    let total = kids.len();
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for k in &kids {
        *by_name.entry(k.name.clone()).or_insert(0) += 1;
    }
    // Oldest first: a leaked child is an old child, and age is what separates
    // it from ordinary spawn churn.
    kids.sort_by(|a, b| {
        b.elapsed_secs
            .cmp(&a.elapsed_secs)
            .then_with(|| a.pid.cmp(&b.pid))
    });
    kids.truncate(MAX_CHILDREN_LISTED);

    Some(ChildCensus {
        total,
        by_name: cap_tally(by_name, MAX_CHILD_NAMES),
        oldest: kids,
    })
}

/// Native Windows per-thread wait-reason enumeration.
///
/// **Why hand-declared FFI.** The API that carries `WaitReason` is
/// `NtQuerySystemInformation(SystemProcessInformation)`, which `windows-sys`
/// exposes under `Wdk_System_SystemInformation` — a feature this crate does not
/// enable, and `Cargo.toml` is out of bounds for this change. Toolhelp's
/// `TH32CS_SNAPTHREAD` (already used by `health_monitor::get_thread_count`)
/// yields a count but no wait reason, and `sysinfo` has no per-thread API on
/// any platform. `ntdll` is always linkable, so the signature is declared here
/// instead. The layouts are the documented x64 ones and are pinned by a
/// compile-time size assertion, which is the cheapest guard against a
/// mis-transcribed field.
#[cfg(target_os = "windows")]
#[allow(dead_code, non_snake_case)]
mod windows_thread_census {
    use super::{wait_reason_name, ThreadCensus};
    use std::collections::BTreeMap;

    const SYSTEM_PROCESS_INFORMATION: u32 = 5;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;

    /// Upper bound on the grow-and-retry loop. The process table can grow
    /// between the sizing call and the read; six doublings from 1 MB is far
    /// past any real machine, and a bounded loop is what keeps a hostile or
    /// pathological table from spinning the capture thread.
    const MAX_ATTEMPTS: usize = 6;

    /// Sanity ceiling on the process-list walk, so a corrupt `NextEntryOffset`
    /// cannot loop forever inside the capture thread.
    const MAX_PROCESSES_WALKED: usize = 100_000;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ClientId {
        unique_process: isize,
        unique_thread: isize,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SystemThreadInformation {
        kernel_time: i64,
        user_time: i64,
        create_time: i64,
        wait_time: u32,
        start_address: *mut core::ffi::c_void,
        client_id: ClientId,
        priority: i32,
        base_priority: i32,
        context_switches: u32,
        thread_state: u32,
        wait_reason: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SystemProcessInformation {
        next_entry_offset: u32,
        number_of_threads: u32,
        working_set_private_size: i64,
        hard_fault_count: u32,
        number_of_threads_high_watermark: u32,
        cycle_time: u64,
        create_time: i64,
        user_time: i64,
        kernel_time: i64,
        image_name: UnicodeString,
        base_priority: i32,
        unique_process_id: isize,
        inherited_from_unique_process_id: isize,
        handle_count: u32,
        session_id: u32,
        unique_process_key: usize,
        peak_virtual_size: usize,
        virtual_size: usize,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_page_count: usize,
        read_operation_count: i64,
        write_operation_count: i64,
        other_operation_count: i64,
        read_transfer_count: i64,
        write_transfer_count: i64,
        other_transfer_count: i64,
    }

    // The documented x64 layouts. If a field is ever mis-transcribed, the walk
    // would silently read garbage wait reasons; these turn that into a build
    // failure instead.
    #[cfg(target_pointer_width = "64")]
    const _: () = assert!(core::mem::size_of::<SystemProcessInformation>() == 0x100);
    #[cfg(target_pointer_width = "64")]
    const _: () = assert!(core::mem::size_of::<SystemThreadInformation>() == 0x50);

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQuerySystemInformation(
            system_information_class: u32,
            system_information: *mut core::ffi::c_void,
            system_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    pub(super) fn census() -> Option<ThreadCensus> {
        let me = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() } as usize
            as isize;

        // `Vec<u64>`, not `Vec<u8>`: the buffer is reinterpreted as a
        // `SYSTEM_PROCESS_INFORMATION` chain, which needs 8-byte alignment. A
        // `Vec<u8>` is 1-byte aligned and the cast would be UB.
        let mut words: usize = 128 * 1024; // 1 MiB
        for _ in 0..MAX_ATTEMPTS {
            let mut buf: Vec<u64> = vec![0u64; words];
            let bytes = words * 8;
            let mut needed: u32 = 0;
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_INFORMATION,
                    buf.as_mut_ptr().cast(),
                    bytes as u32,
                    &mut needed,
                )
            };
            if status == STATUS_INFO_LENGTH_MISMATCH {
                let want = (needed as usize).div_ceil(8) + 8192;
                words = want.max(words * 2);
                continue;
            }
            if status < 0 {
                return None;
            }
            return unsafe { tally(buf.as_ptr().cast::<u8>(), bytes, me) };
        }
        None
    }

    /// Walk the process chain, find our own entry, tally its threads.
    ///
    /// # Safety
    /// `base` must point at `len` readable bytes holding the chain returned by
    /// `NtQuerySystemInformation(SystemProcessInformation)`.
    unsafe fn tally(base: *const u8, len: usize, me: isize) -> Option<ThreadCensus> {
        let proc_size = core::mem::size_of::<SystemProcessInformation>();
        let thread_size = core::mem::size_of::<SystemThreadInformation>();
        let mut offset: usize = 0;

        for _ in 0..MAX_PROCESSES_WALKED {
            if offset.checked_add(proc_size)? > len {
                return None;
            }
            let spi = &*(base.add(offset) as *const SystemProcessInformation);

            if spi.unique_process_id == me {
                let count = spi.number_of_threads as usize;
                let threads_at = offset.checked_add(proc_size)?;
                let span = count.checked_mul(thread_size)?;
                if threads_at.checked_add(span)? > len {
                    return None;
                }
                let threads = base.add(threads_at) as *const SystemThreadInformation;
                let mut by_wait_reason: BTreeMap<String, usize> = BTreeMap::new();
                for i in 0..count {
                    let t = &*threads.add(i);
                    *by_wait_reason
                        .entry(wait_reason_name(t.wait_reason).to_string())
                        .or_insert(0) += 1;
                }
                return Some(ThreadCensus {
                    total: count,
                    by_wait_reason,
                });
            }

            let next = spi.next_entry_offset as usize;
            if next == 0 {
                // Walked the whole table without finding ourselves. That is a
                // failed enumeration, not an empty process.
                return None;
            }
            offset = offset.checked_add(next)?;
        }
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Bounded capture: one long-lived worker thread, one deadline per step
// ─────────────────────────────────────────────────────────────────────────────

/// Default per-step budget. Short enough that both steps together stay far
/// inside `health_monitor`'s 180s monitor-stall window (so a slow capture can
/// never present as a dead detector), long enough that a healthy enumeration of
/// a few hundred processes finishes comfortably.
pub const CAPTURE_STEP_DEADLINE_SECS: u64 = 3;

/// Runs capture steps on ONE long-lived worker thread and never waits longer
/// than its deadline for an answer.
///
/// This is [`crate::health_monitor`]'s `LivezProber` shape, copied deliberately
/// rather than re-derived, including the part that is easy to miss: once a step
/// overruns, the worker still owes an answer, so the capturer stops queueing
/// work onto it and reports `"timed_out"` immediately on every later call until
/// the late answer is collected and discarded. Without that, each escalation
/// would pay the full deadline again, and each would leave another abandoned
/// thread behind if the worker were per-capture.
pub struct BoundedCapturer {
    req_tx: SyncSender<CaptureStep>,
    res_rx: Receiver<StepOutput>,
    /// True while the worker still owes an answer we already gave up on.
    outstanding: bool,
    alive: bool,
    deadline: Duration,
}

impl BoundedCapturer {
    /// Production capturer: native steps, default deadline.
    pub fn new() -> Self {
        Self::with_runner(
            Duration::from_secs(CAPTURE_STEP_DEADLINE_SECS),
            run_capture_step,
        )
    }

    /// The seam the tests drive: any step implementation, any deadline.
    pub fn with_runner<F>(deadline: Duration, runner: F) -> Self
    where
        F: Fn(CaptureStep) -> StepOutput + Send + 'static,
    {
        let (req_tx, req_rx) = std::sync::mpsc::sync_channel::<CaptureStep>(1);
        let (res_tx, res_rx) = std::sync::mpsc::channel::<StepOutput>();
        let alive = std::thread::Builder::new()
            .name("wedge-capture".to_string())
            .spawn(move || {
                while let Ok(step) = req_rx.recv() {
                    if res_tx.send(runner(step)).is_err() {
                        break;
                    }
                }
            })
            .is_ok();
        Self {
            req_tx,
            res_rx,
            outstanding: false,
            alive,
            deadline,
        }
    }

    /// Run one step under its own deadline.
    ///
    /// `Err` carries the reason string that lands in the record.
    pub fn capture(&mut self, step: CaptureStep) -> Result<StepOutput, &'static str> {
        if !self.alive {
            return Err("capture_thread_unavailable");
        }

        if self.outstanding {
            match self.res_rx.try_recv() {
                // A late answer to a capture we already reported on. Discard it
                // and reuse the thread from here on.
                Ok(_late) => self.outstanding = false,
                Err(TryRecvError::Empty) => return Err("timed_out"),
                Err(TryRecvError::Disconnected) => {
                    self.alive = false;
                    return Err("capture_thread_unavailable");
                }
            }
        }

        if self.req_tx.try_send(step).is_err() {
            return Err("timed_out");
        }
        match self.res_rx.recv_timeout(self.deadline) {
            Ok(out) if out.matches(step) => Ok(out),
            // A stale answer from a previous step arriving in this step's
            // window. Not this step's evidence, so it is not reported as it.
            Ok(_mismatched) => Err("timed_out"),
            Err(RecvTimeoutError::Timeout) => {
                self.outstanding = true;
                Err("timed_out")
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.alive = false;
                Err("capture_thread_unavailable")
            }
        }
    }
}

impl Default for BoundedCapturer {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The rig the watchdog thread owns
// ─────────────────────────────────────────────────────────────────────────────

/// Captures a diagnostic record on escalation and appends it to the rolled
/// JSONL sink.
///
/// Owned by the `WedgeDetector`, driven from its dedicated OS thread. Every
/// method is best-effort and infallible by contract: the process is already
/// sick, so a failure to capture must never make it worse and must never panic.
pub struct WedgeDiagnostics {
    dir: PathBuf,
    capturer: BoundedCapturer,
    /// UTC day of the last retention sweep, so the sweep costs one `read_dir`
    /// per day rather than one per escalation.
    last_prune_day: Option<String>,
}

impl WedgeDiagnostics {
    /// Resolve the sink directory ONCE, on whatever thread constructs this —
    /// never inside the capture path, where a stall in path resolution would
    /// silence the whole record.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            capturer: BoundedCapturer::new(),
            last_prune_day: None,
        }
    }

    /// Test seam: an injected capturer.
    pub fn with_capturer(dir: PathBuf, capturer: BoundedCapturer) -> Self {
        Self {
            dir,
            capturer,
            last_prune_day: None,
        }
    }

    /// Build one record. Bounded: each census step gets its own deadline and a
    /// `"timed_out"` field on expiry.
    pub fn sample(
        &mut self,
        event: &'static str,
        consecutive_failures: u32,
        unresponsive_for: Duration,
    ) -> WedgeDiagnosticRecord {
        // Read the pool counters FIRST. Two atomic loads that cannot block, so
        // the saturation figure describes the moment of the escalation rather
        // than the moment the last enumeration happened to finish.
        let blocking_pool = BlockingPoolSnapshot::sample();

        let threads = match self.capturer.capture(CaptureStep::Threads) {
            Ok(StepOutput::Threads(Some(c))) => Captured::Value(c),
            Ok(StepOutput::Threads(None)) => Captured::Unavailable(unsupported_or_failed()),
            Ok(_) => Captured::Unavailable("timed_out"),
            Err(reason) => Captured::Unavailable(reason),
        };
        let children = match self.capturer.capture(CaptureStep::Children) {
            Ok(StepOutput::Children(Some(c))) => Captured::Value(c),
            Ok(StepOutput::Children(None)) => Captured::Unavailable("enumeration_failed"),
            Ok(_) => Captured::Unavailable("timed_out"),
            Err(reason) => Captured::Unavailable(reason),
        };

        WedgeDiagnosticRecord {
            ts: chrono::Utc::now().to_rfc3339(),
            pid: std::process::id(),
            event,
            consecutive_failures,
            unresponsive_for_secs: unresponsive_for.as_secs(),
            unresponsive_for_ms: unresponsive_for.as_millis().min(u64::MAX as u128) as u64,
            blocking_pool,
            threads,
            children,
        }
    }

    /// Append one rendered line to today's file, rolling and pruning as needed.
    ///
    /// Raw blocking `std::fs`, exactly like
    /// `health_monitor::write_wedge_breadcrumb` and for the same reason: the
    /// durable record must not sit behind the tracing subscriber, which shares
    /// fate with the subsystem being reported.
    pub fn append(&mut self, day: &str, line: &str) {
        use std::io::Write;
        let path = diagnostics_path(&self.dir, day);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
        // Retention runs on a day change only. It is a `read_dir` plus a few
        // unlinks on the same thread that must keep ticking through a wedge, so
        // it is paid once per rolled file rather than once per escalation.
        if self.last_prune_day.as_deref() != Some(day) {
            self.last_prune_day = Some(day.to_string());
            self.prune();
        }
    }

    /// Apply [`files_to_prune`] to the live directory. Best-effort throughout.
    fn prune(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect();
        for name in files_to_prune(&names, DIAGNOSTICS_MAX_FILES) {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
    }

    /// Capture and write one record. The single entry point the detector calls.
    pub fn capture_and_append(
        &mut self,
        event: &'static str,
        consecutive_failures: u32,
        unresponsive_for: Duration,
    ) {
        let record = self.sample(event, consecutive_failures, unresponsive_for);
        // The UTC day comes from the record's own timestamp, so the line and
        // the file it lands in can never disagree about which day it is.
        let day = record.ts.get(..10).unwrap_or("unknown").to_string();
        let line = render_record(&record);
        self.append(&day, &line);
    }
}

/// Why a `None` thread census happened, as far as this build can tell.
fn unsupported_or_failed() -> &'static str {
    if cfg!(any(target_os = "linux", target_os = "windows")) {
        "enumeration_failed"
    } else {
        "unsupported_platform"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- blocking-pool counter ----

    /// `BLOCKING_IN_FLIGHT` is a PROCESS-GLOBAL static, so two of these tests
    /// running in parallel observe each other's slots and the before/after
    /// assertions race. Same discipline (and same reason) as
    /// `health_monitor`'s `SERIAL` guard around its global atomics.
    /// Poison-recovering, so a panicking test — and one of these panics ON
    /// PURPOSE — cannot cascade-fail the rest.
    static POOL_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_tracked_blocking_body_increments_and_decrements() {
        let _g = POOL_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let before = blocking_pool_in_flight();
        {
            let _slot = BlockingSlot::enter();
            assert_eq!(blocking_pool_in_flight(), before + 1);
        }
        assert_eq!(
            blocking_pool_in_flight(),
            before,
            "the slot was not returned on drop"
        );
    }

    /// A panicking body must still give its slot back. Without the RAII guard
    /// the count would climb monotonically and eventually report a saturated
    /// pool that is not saturated — a false diagnosis of the exact condition
    /// this counter exists to report.
    #[test]
    fn a_panicking_blocking_body_still_returns_its_slot() {
        let _g = POOL_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let before = blocking_pool_in_flight();
        let r = std::panic::catch_unwind(|| {
            let _slot = BlockingSlot::enter();
            panic!("boom");
        });
        assert!(r.is_err(), "the fixture must actually panic");
        assert_eq!(
            blocking_pool_in_flight(),
            before,
            "a panicking body leaked its blocking-pool slot"
        );
    }

    #[test]
    fn spawn_blocking_tracked_counts_the_running_body() {
        let _g = POOL_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let before = blocking_pool_in_flight();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<usize>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        // `rt.enter()` rather than `block_on(async { .. })`: an async block whose
        // value is itself a `JoinHandle` is the `async_yields_async` lint, and
        // the handle here is deliberately NOT awaited yet — the point is to
        // observe the counter while the body is still running.
        let handle = {
            let _guard = rt.enter();
            spawn_blocking_tracked(move || {
                let _ = entered_tx.send(blocking_pool_in_flight());
                let _ = release_rx.recv();
            })
        };
        let seen = entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the body must run");
        assert_eq!(
            seen,
            before + 1,
            "the body did not hold a slot while it was executing"
        );
        let _ = release_tx.send(());
        rt.block_on(handle).expect("join");
        assert_eq!(blocking_pool_in_flight(), before);
    }

    #[test]
    fn the_capacity_is_tokios_documented_default() {
        assert_eq!(blocking_pool_capacity(), 512);
    }

    // ---- the record's wire shape (pure: no threads, no clock) ----

    fn fixture_record() -> WedgeDiagnosticRecord {
        WedgeDiagnosticRecord {
            ts: "2026-08-30T12:00:00+00:00".to_string(),
            pid: 4242,
            event: "escalate",
            consecutive_failures: 3,
            unresponsive_for_secs: 4093,
            unresponsive_for_ms: 4_093_500,
            blocking_pool: BlockingPoolSnapshot {
                in_flight: 512,
                capacity: 512,
            },
            threads: Captured::Value(ThreadCensus {
                total: 540,
                by_wait_reason: BTreeMap::from([
                    ("Executive".to_string(), 12),
                    ("UserRequest".to_string(), 528),
                ]),
            }),
            children: Captured::Value(ChildCensus {
                total: 324,
                by_name: BTreeMap::from([("git.exe".to_string(), 324)]),
                oldest: vec![ChildProcess {
                    pid: 9001,
                    name: "git.exe".to_string(),
                    elapsed_secs: 4000,
                    cpu_percent: 0.0,
                }],
            }),
        }
    }

    #[test]
    fn the_record_is_one_json_object_per_line() {
        let line = render_record(&fixture_record());
        assert!(
            line.ends_with('\n'),
            "JSONL lines must be newline-terminated"
        );
        assert_eq!(line.matches('\n').count(), 1, "no embedded newlines");
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).expect("valid JSON");
        assert_eq!(v["pid"], 4242);
        assert_eq!(v["event"], "escalate");
        assert_eq!(v["consecutive_failures"], 3);
        assert_eq!(v["unresponsive_for_secs"], 4093);
        assert_eq!(v["blocking_pool"]["in_flight"], 512);
        assert_eq!(v["blocking_pool"]["capacity"], 512);
        assert_eq!(v["threads"]["total"], 540);
        assert_eq!(v["threads"]["by_wait_reason"]["UserRequest"], 528);
        assert_eq!(v["children"]["total"], 324);
        assert_eq!(v["children"]["oldest"][0]["pid"], 9001);
    }

    /// The exact wire bytes. This file is an append-only evidence sink that
    /// outlives the process and will be read by tools and by humans years after
    /// the incident, so the field names and their order are a contract, not an
    /// implementation detail — pinned here rather than left to whatever the
    /// struct definition happens to say next month.
    #[test]
    fn the_wire_format_is_pinned() {
        assert_eq!(
            render_record(&fixture_record()),
            concat!(
                r#"{"ts":"2026-08-30T12:00:00+00:00","pid":4242,"event":"escalate","#,
                r#""consecutive_failures":3,"unresponsive_for_secs":4093,"#,
                r#""unresponsive_for_ms":4093500,"#,
                r#""blocking_pool":{"in_flight":512,"capacity":512},"#,
                r#""threads":{"total":540,"by_wait_reason":{"Executive":12,"UserRequest":528}},"#,
                r#""children":{"total":324,"by_name":{"git.exe":324},"#,
                r#""oldest":[{"pid":9001,"name":"git.exe","elapsed_secs":4000,"#,
                r#""cpu_percent":0.0}]}}"#,
                "\n"
            )
        );
    }

    /// An unavailable field must read as a bare reason string, never as an
    /// empty census — "we could not look" and "there was nothing there" are
    /// different findings and the record must not conflate them.
    #[test]
    fn an_unavailable_field_serializes_as_its_reason_string() {
        let mut r = fixture_record();
        r.threads = Captured::Unavailable("timed_out");
        r.children = Captured::Unavailable("unsupported_platform");
        let v: serde_json::Value =
            serde_json::from_str(render_record(&r).trim_end()).expect("valid JSON");
        assert_eq!(v["threads"], "timed_out");
        assert_eq!(v["children"], "unsupported_platform");
    }

    /// A record with a 300+ child population must still be one readable line.
    #[test]
    fn a_huge_child_population_stays_a_bounded_line() {
        let mut by_name = BTreeMap::new();
        let mut oldest = Vec::new();
        for i in 0..400u32 {
            by_name.insert(format!("child-{i}.exe"), 1);
            oldest.push(ChildProcess {
                pid: i,
                name: format!("child-{i}.exe"),
                elapsed_secs: i as u64,
                cpu_percent: 0.0,
            });
        }
        let by_name = cap_tally(by_name, MAX_CHILD_NAMES);
        oldest.truncate(MAX_CHILDREN_LISTED);
        let mut r = fixture_record();
        r.children = Captured::Value(ChildCensus {
            total: 400,
            by_name,
            oldest,
        });
        let line = render_record(&r);
        assert!(
            line.len() < 4096,
            "a 400-child record rendered {} bytes — the caps are not holding",
            line.len()
        );
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).expect("valid JSON");
        assert_eq!(
            v["children"]["total"], 400,
            "the full population must survive as a total even when the list is capped"
        );
    }

    #[test]
    fn cap_tally_keeps_the_most_frequent_deterministically() {
        let t = BTreeMap::from([
            ("a".to_string(), 1),
            ("b".to_string(), 9),
            ("c".to_string(), 5),
        ]);
        assert_eq!(
            cap_tally(t, 2),
            BTreeMap::from([("b".to_string(), 9), ("c".to_string(), 5)])
        );
    }

    // ---- retention policy (pure: a fake listing) ----

    #[test]
    fn retention_keeps_the_newest_fourteen_and_nothing_else() {
        let mut names: Vec<String> = (1..=20)
            .map(|d| diagnostics_file_name(&format!("2026-08-{d:02}")))
            .collect();
        // Unrelated siblings in the same dev-logs dir must never be touched.
        names.push("qontinui-runner.log.2026-08-30".to_string());
        names.push("wedge-incidents.log".to_string());
        names.push("wedge-diagnostics.jsonl.bak".to_string());

        let pruned = files_to_prune(&names, DIAGNOSTICS_MAX_FILES);
        assert_eq!(pruned.len(), 6, "20 dated files minus a 14-file window");
        assert_eq!(pruned[0], diagnostics_file_name("2026-08-01"));
        assert_eq!(pruned[5], diagnostics_file_name("2026-08-06"));
        for p in &pruned {
            assert!(
                p.starts_with(DIAGNOSTICS_PREFIX),
                "retention reached outside its own sink: {p}"
            );
        }
        assert!(!pruned.iter().any(|p| p.ends_with(".bak")));
    }

    #[test]
    fn retention_is_a_no_op_below_the_window() {
        let names: Vec<String> = (1..=14)
            .map(|d| diagnostics_file_name(&format!("2026-08-{d:02}")))
            .collect();
        assert!(files_to_prune(&names, DIAGNOSTICS_MAX_FILES).is_empty());
    }

    #[test]
    fn only_our_own_rolled_files_are_recognised() {
        assert!(is_diagnostics_file("wedge-diagnostics.jsonl"));
        assert!(is_diagnostics_file("wedge-diagnostics.jsonl.2026-08-30"));
        assert!(!is_diagnostics_file("wedge-diagnostics.jsonl.bak"));
        assert!(!is_diagnostics_file("wedge-diagnostics.jsonl.old"));
        assert!(!is_diagnostics_file("qontinui-runner.log.2026-08-30"));
    }

    #[test]
    fn retention_actually_unlinks_on_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for d in 1..=20 {
            std::fs::write(
                tmp.path()
                    .join(diagnostics_file_name(&format!("2026-08-{d:02}"))),
                b"{}\n",
            )
            .expect("write");
        }
        std::fs::write(tmp.path().join("qontinui-runner.log.2026-08-01"), b"x").expect("write");

        let mut rig = WedgeDiagnostics::with_capturer(
            tmp.path().to_path_buf(),
            BoundedCapturer::with_runner(Duration::from_millis(50), |s| match s {
                CaptureStep::Threads => StepOutput::Threads(None),
                CaptureStep::Children => StepOutput::Children(None),
            }),
        );
        rig.append("2026-08-21", "{\"x\":1}\n");

        let remaining: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .filter(|n| is_diagnostics_file(n))
            .collect();
        assert_eq!(
            remaining.len(),
            DIAGNOSTICS_MAX_FILES,
            "retention did not hold: {remaining:?}"
        );
        assert!(
            tmp.path().join("qontinui-runner.log.2026-08-01").exists(),
            "retention deleted a file it did not write"
        );
        assert!(tmp
            .path()
            .join(diagnostics_file_name("2026-08-21"))
            .exists());
    }

    // ---- bounded capture ----

    /// The load-bearing bound. A census that hangs must yield `"timed_out"`
    /// within budget rather than parking the one thread whose job is to survive
    /// a parked runtime.
    ///
    /// Neuter check: replace `recv_timeout` with `recv` in
    /// `BoundedCapturer::capture` and this test hangs instead of failing —
    /// which is precisely the production failure it is guarding against.
    #[test]
    fn a_hanging_capture_step_times_out_within_budget() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let mut cap = BoundedCapturer::with_runner(Duration::from_millis(200), move |s| {
            let _ = release_rx.recv();
            match s {
                CaptureStep::Threads => StepOutput::Threads(None),
                CaptureStep::Children => StepOutput::Children(None),
            }
        });

        let started = std::time::Instant::now();
        assert_eq!(cap.capture(CaptureStep::Threads), Err("timed_out"));
        // The SECOND call must be instant: the worker still owes an answer, so
        // re-queueing onto it would cost the deadline again on every
        // escalation, and (if the worker were per-capture) leak a thread each
        // time. This is the `LivezProber` trap, copied.
        assert_eq!(cap.capture(CaptureStep::Children), Err("timed_out"));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(900),
            "two capture steps against a hung worker took {elapsed:?} — the second re-waited \
             the full deadline instead of reporting immediately"
        );
        let _ = release_tx.send(());
    }

    /// A record built against a hung capturer is still a valid, complete record
    /// — every field present, the unavailable ones naming why.
    #[test]
    fn a_timed_out_capture_still_produces_a_complete_record() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut rig = WedgeDiagnostics::with_capturer(
            tmp.path().to_path_buf(),
            BoundedCapturer::with_runner(Duration::from_millis(150), move |s| {
                let _ = release_rx.recv();
                match s {
                    CaptureStep::Threads => StepOutput::Threads(None),
                    CaptureStep::Children => StepOutput::Children(None),
                }
            }),
        );
        let started = std::time::Instant::now();
        rig.capture_and_append("escalate", 63, Duration::from_secs(4093));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the capture path blocked the caller for {:?}",
            started.elapsed()
        );

        let day = chrono::Utc::now().to_rfc3339()[..10].to_string();
        let body = std::fs::read_to_string(diagnostics_path(tmp.path(), &day)).expect("record");
        let v: serde_json::Value =
            serde_json::from_str(body.trim_end()).expect("a valid JSONL record");
        assert_eq!(v["threads"], "timed_out");
        assert_eq!(v["children"], "timed_out");
        assert_eq!(v["consecutive_failures"], 63);
        assert_eq!(
            v["unresponsive_for_secs"], 4093,
            "the Phase-3 elapsed must survive into the diagnostic record"
        );
        let _ = release_tx.send(());
    }

    /// A capturer whose worker answers normally produces values, and the record
    /// lands in the day-stamped file the record's own timestamp names.
    #[test]
    fn a_healthy_capture_lands_a_value_record_in_the_days_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut rig = WedgeDiagnostics::with_capturer(
            tmp.path().to_path_buf(),
            BoundedCapturer::with_runner(Duration::from_secs(2), |s| match s {
                CaptureStep::Threads => StepOutput::Threads(Some(ThreadCensus {
                    total: 7,
                    by_wait_reason: BTreeMap::from([("UserRequest".to_string(), 7)]),
                })),
                CaptureStep::Children => StepOutput::Children(Some(ChildCensus {
                    total: 2,
                    by_name: BTreeMap::from([("git".to_string(), 2)]),
                    oldest: vec![],
                })),
            }),
        );
        rig.capture_and_append("re-escalate", 123, Duration::from_secs(600));

        let day = chrono::Utc::now().to_rfc3339()[..10].to_string();
        let body = std::fs::read_to_string(diagnostics_path(tmp.path(), &day)).expect("record");
        let v: serde_json::Value = serde_json::from_str(body.trim_end()).expect("valid JSONL");
        assert_eq!(v["event"], "re-escalate");
        assert_eq!(v["threads"]["total"], 7);
        assert_eq!(v["children"]["total"], 2);

        // A second escalation appends rather than truncating.
        rig.capture_and_append("re-escalate", 183, Duration::from_secs(900));
        let body = std::fs::read_to_string(diagnostics_path(tmp.path(), &day)).expect("record");
        assert_eq!(body.lines().count(), 2, "the sink must be append-only");
    }

    /// A healthy round trip returns the step's OWN output, and a `None` census
    /// comes back as a value the caller can name a reason for — never as a
    /// timeout, which would be a different finding.
    #[test]
    fn a_round_trip_returns_the_steps_own_output() {
        let mut cap = BoundedCapturer::with_runner(Duration::from_secs(30), |s| match s {
            CaptureStep::Threads => StepOutput::Threads(None),
            CaptureStep::Children => StepOutput::Children(Some(ChildCensus {
                total: 0,
                by_name: BTreeMap::new(),
                oldest: vec![],
            })),
        });
        assert_eq!(
            cap.capture(CaptureStep::Threads),
            Ok(StepOutput::Threads(None))
        );
        assert!(matches!(
            cap.capture(CaptureStep::Children),
            Ok(StepOutput::Children(Some(_)))
        ));
    }

    // ---- native census: it must produce plausible values for THIS process ----

    #[test]
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "windows")),
        ignore = "no per-thread enumeration on this platform"
    )]
    fn the_thread_census_sees_this_test_process() {
        let c = capture_thread_census().expect("a native thread census");
        assert!(c.total >= 1, "a live process has at least one thread");
        assert!(
            !c.by_wait_reason.is_empty(),
            "the census produced no wait-reason buckets"
        );
        let summed: usize = c.by_wait_reason.values().sum();
        assert_eq!(
            summed, c.total,
            "the histogram must account for every thread it counted"
        );
    }

    #[test]
    fn the_child_census_runs_and_counts_a_real_child() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sleep" })
            .args(if cfg!(windows) {
                vec!["/C", "ping -n 6 127.0.0.1 >NUL"]
            } else {
                vec!["5"]
            })
            .spawn()
            .expect("spawn a child");

        let c = capture_child_census().expect("a native child census");
        assert!(
            c.total >= 1,
            "the census missed a child this test just spawned"
        );
        assert!(
            c.oldest.len() <= MAX_CHILDREN_LISTED,
            "the listed-children cap is not holding"
        );
        assert!(
            c.by_name.len() <= MAX_CHILD_NAMES,
            "the name-tally cap is not holding"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    // ---- pure parsers ----

    /// `comm` is parenthesised and may contain spaces AND parentheses, so a
    /// left-to-right tokenise reads the wrong field. Pinning the hostile case.
    #[test]
    fn proc_stat_is_parsed_from_the_last_paren() {
        let line = "1234 (tokio (odd) name) D 991 1234 0 0 -1 4194304 100 0";
        assert_eq!(parse_proc_stat_state(line), Some('D'));
        assert_eq!(parse_proc_stat_ppid(line), Some(991));
        assert_eq!(parse_proc_stat_state("garbage"), None);
    }

    #[test]
    fn wait_reasons_render_the_kernels_names() {
        assert_eq!(wait_reason_name(0), "Executive");
        assert_eq!(wait_reason_name(6), "UserRequest");
        assert_eq!(wait_reason_name(14), "WrEventPair");
        assert_eq!(wait_reason_name(15), "WrQueue");
        assert_eq!(wait_reason_name(9_999), "UnknownWaitReason");
    }

    #[test]
    fn linux_states_render_long_hand() {
        assert_eq!(linux_state_name('D'), "UninterruptibleDisk");
        assert_eq!(linux_state_name('S'), "Sleeping");
        assert_eq!(linux_state_name('?'), "UnknownState");
    }
}
