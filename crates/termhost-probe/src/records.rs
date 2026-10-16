//! The probe's output contract: every JSON-lines record it writes.
//!
//! One object per line, `{"schema": "termhost-probe/v1", "type": <kind>, ...}`.
//! Absolute timestamps (`*_ns`) are DECIMAL STRINGS — they exceed 2^53, and the
//! generator's `gen-<k>.jsonl` uses the same convention — on the probe clock
//! ([`crate::clock`]), the timebase of Node's `process.hrtime`. Durations are
//! numbers in milliseconds (`*_ms`). Raw frames are never written anywhere:
//! a frame record carries only its time, size and the markers decoded from it.
//!
//! # What an orchestrator must know (Phase 3 builds against this)
//!
//! - **A value the probe cannot measure is `null` or an unknown KPI, never 0.**
//!   `frame.missed` is `null` for sources that cannot detect skipped frames
//!   (WGC, X11) and a count only for Desktop Duplication; `sample.dt_ms` is
//!   `null` on the first sample; `summary.v_actual_max` is `null` with no
//!   frames; `summary.crashes` is `null` when no host group was sampled; an
//!   empty or partly unreadable process group reports `null` values and a
//!   `note`.
//! - **Files are per run.** `watch` refuses a run directory whose `probe.jsonl`
//!   already holds records, and `inject` one whose `inject.jsonl` does, so two
//!   runs can never interleave in one file: one `watch` and at most one
//!   `inject` per run directory.
//! - **Late input amends earlier records.** Real input that arrives after its
//!   interval closed re-emits that `interval` record with `amended: true` (and
//!   any `hang` it invalidates); the LAST record per interval `index` / per
//!   hang start is the verdict.
//! - **Workload membership is fixed for the whole run.** `watch --run-dir`
//!   reads the generators' `<run-dir>/slot-<k>` files (Phase 1's
//!   `termhost-bench/slot/v1`: `pid`, `ppid`, `run_id`) as well as generator
//!   command lines; a generator and its session shell are workload from the
//!   first sample that sees them, keyed by (pid, start time), and never move
//!   into the host group — not after the generator exits either.
//! - **`inject` announces each key before sending it.** An `inject_intent`
//!   record (index, key, and on X11 the XTEST ledger ids) is written and
//!   flushed to `inject.jsonl` immediately before the key is sent; `watch`
//!   reads `<run-dir>/inject.jsonl` to recognise those keys as the probe's
//!   own, so run `watch` and `inject` with the same `--run-dir` (and, on
//!   Windows, the same `--tag`).
//! - **`watch` does not pair keys with echoes.** Its `input_latency_ms` is
//!   `unknown(not_instrumented)`; the orchestrator pairs `inject.jsonl`
//!   `t_inject_ns` with the echo markers' `first_seen` records.
//! - **Incidents follow the results schema's incident shape** (`kind`, `at`,
//!   `detail`, `stops_run`, optional `data`); the capture source an incident
//!   is about is `data.source`.
//! - **The frame `v`** is the number of sessions whose output marker is
//!   currently displayed, accumulated across partial reads (X11 delivers only
//!   the damaged band of a window), not the count inside one band.
//! - **The probe's own health is recorded**: each frame carries `scan_ms` (time
//!   to scan it) and `lag_ms` (how long after its presentation the probe
//!   received it); the summary's `probe` block says whether the probe fell
//!   behind, in which case the KPIs it could have inflated are
//!   `unknown(harness_error)`; the sampler has a `probe` group. On X11 a
//!   frame's presentation time is its oldest DAMAGE event's server timestamp
//!   mapped onto the probe clock; `lag_ms` is `null` for a frame whose
//!   presentation time is not known (X11's first frame, or a server clock the
//!   probe could not establish), and `probe.behind` is `null` when no frame's
//!   was — the KPIs it guards are then `unknown(harness_error)` too, because
//!   a probe that cannot show it kept up has not.
//! - **What a frame time means depends on the source.** WGC (the Windows
//!   primary) stamps `SystemRelativeTime`, documented as the time the
//!   compositor RENDERED the capture frame; Desktop Duplication stamps the
//!   desktop's presentation. On the first hosted run WGC trailed DDA by a
//!   near-constant 7-9 ms at every delay, so Windows output latencies carry
//!   that stage: compare setups measured by the same source, never a WGC
//!   number with a DDA or X11 one.
//! - **A group's `memory_mb_end` is its memory in the LAST sample**, `null`
//!   unless that sample was whole: a host that crashed, or a launcher that
//!   handed off and exited, has no end-of-soak value.
//! - **A generator's process exit names its session** (`sample.exited[]`
//!   `session`, from its slot file): that session's marker schedule ends
//!   there, so a generator killed without an `exit` record is a workload
//!   death, not a host hang.
//! - **An assumed refresh rate is marked** (`capture.refresh_assumed`,
//!   `summary.refresh_assumed`), and every KPI counted in refresh intervals is
//!   then `unknown(refresh_rate_unknown)`.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Serialize, Serializer};

use crate::codec::{Marker, MarkerKind};
use crate::raster::Rect;
use crate::spec::CodecSpec;
use crate::stats::KpiValue;

/// The schema id on every line.
pub const SCHEMA: &str = "termhost-probe/v1";

/// An absolute probe-clock timestamp, serialized as a decimal string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ns(pub u64);

impl Serialize for Ns {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

/// A capture source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    /// Windows.Graphics.Capture, one window (the primary Windows source).
    Wgc,
    /// DXGI Desktop Duplication (the Windows cross-check; never primary).
    Dda,
    /// X11 damage + shm read of the damaged rows.
    X11,
    /// An in-memory source (tests and the hermetic pipeline check).
    Synthetic,
}

impl SourceId {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceId::Wgc => "wgc",
            SourceId::Dda => "dda",
            SourceId::X11 => "x11",
            SourceId::Synthetic => "synthetic",
        }
    }

    /// Whether this source can be the primary one. Desktop Duplication
    /// delivers a frame for ANY change on its output (a clock, another window),
    /// so its cadence is the desktop's, not the window's: it is the
    /// cross-check only.
    pub fn can_be_primary(self) -> bool {
        !matches!(self, SourceId::Dda)
    }
}

/// One decoded marker in a frame record.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct MarkerObs {
    pub kind: MarkerKind,
    pub session: u16,
    pub seq: u32,
    /// Window-relative pixels.
    pub rect: Rect,
}

impl MarkerObs {
    pub fn marker(&self) -> Marker {
        Marker {
            kind: self.kind,
            session: self.session,
            seq: self.seq,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StartRecord {
    pub tool: &'static str,
    pub version: &'static str,
    pub subcommand: &'static str,
    pub pid: u32,
    pub platform: &'static str,
    pub clock: &'static str,
    pub t0_ns: Ns,
    pub at: String,
    pub codec: CodecSpec,
    pub args: Vec<String>,
    /// The injection tag in effect (`inject`, or `watch --tag`), as hex.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaptureRecord {
    pub source: SourceId,
    /// `started` or `failed`.
    pub state: &'static str,
    pub target: String,
    pub detail: String,
    pub refresh_hz: f64,
    pub refresh_source: String,
    /// The display reported no refresh rate and 60 Hz was assumed: every KPI
    /// counted in refresh intervals is then unknown(refresh_rate_unknown).
    pub refresh_assumed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct FrameRecord {
    pub source: SourceId,
    pub index: u64,
    pub t_ns: Ns,
    pub w: u32,
    pub h: u32,
    /// Frames the source coalesced into this one (DXGI `AccumulatedFrames - 1`);
    /// `null` for a source that cannot detect skipped frames (WGC, X11).
    pub missed: Option<u32>,
    /// Sessions whose output marker is currently displayed — accumulated over
    /// partial reads (an X11 frame is only the damaged band), measured.
    pub v: u32,
    pub markers: Vec<MarkerObs>,
    /// Red-cyan starts that did not decode.
    pub rejected: u32,
    /// How long the probe took to scan this frame.
    pub scan_ms: f64,
    /// How long after the frame's oldest content was presented the probe
    /// received it; `null` when that presentation time is not known.
    pub lag_ms: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FirstSeenRecord {
    pub source: SourceId,
    pub kind: MarkerKind,
    pub session: u16,
    pub seq: u32,
    pub t_ns: Ns,
    pub frame: u64,
    pub rect: Rect,
}

/// Real (non-probe) input in an interval, by device. Counts only — which key
/// or where the pointer went is never recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct InputCounts {
    pub keyboard: u32,
    pub pointer: u32,
    pub other: u32,
}

impl InputCounts {
    pub fn total(&self) -> u32 {
        self.keyboard + self.pointer + self.other
    }
}

/// A window above the measured one that overlapped it (class name and pid;
/// never a title, which can be sensitive).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct OccluderInfo {
    pub class: String,
    pub pid: u32,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Debug, Serialize)]
pub struct IntervalRecord {
    pub index: u64,
    pub t_start_ns: Ns,
    pub t_end_ns: Ns,
    /// A trailing interval shorter than the full length: recorded, never
    /// counted.
    pub partial: bool,
    pub samples: u32,
    pub not_foreground: u32,
    pub not_visible: u32,
    pub cloaked: u32,
    pub minimized: u32,
    pub occluded: u32,
    pub state_unknown: u32,
    pub max_occluded_fraction: Option<f64>,
    pub occluders: Vec<OccluderInfo>,
    /// Samples in which the OS reported the window not responding.
    pub hung: u32,
    pub real_input: InputCounts,
    /// Injected input that did NOT carry this run's tag (another injector).
    pub foreign_injected: u32,
    pub probe_input: u32,
    /// `ok`, `down` (the monitor failed) or `absent` (none configured).
    pub monitor: &'static str,
    pub frames: u32,
    pub valid: bool,
    pub excluded: Vec<&'static str>,
    /// `occluded`, `operator_activity` or `harness_error` — the catalog
    /// reason this interval counts toward.
    pub exclusion_class: Option<&'static str>,
    /// Re-emitted because input that arrived after the interval closed
    /// changed its verdict; the last record per `index` wins.
    #[serde(skip_serializing_if = "is_false")]
    pub amended: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GroupSample {
    pub pids: Vec<u32>,
    pub cpu_pct: Option<f64>,
    pub cpu_ms: Option<f64>,
    /// The catalog's memory number: private bytes (Windows) / PSS (Linux).
    pub memory_mb: Option<f64>,
    pub rss_mb: Option<f64>,
    /// GPU engine utilisation summed over the group's PIDs, per engine type.
    pub gpu_by_engine: BTreeMap<String, f64>,
    /// The busiest engine type's summed utilisation.
    pub gpu_pct: Option<f64>,
    /// PIDs whose CPU or memory could not be read this sample.
    pub unreadable: u32,
    /// Why the values are `null`: `no_live_process` (the group is empty) or
    /// `unreadable_members` (a partial sum is not the group's number).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExitedProcess {
    pub pid: u32,
    pub group: &'static str,
    pub name: String,
    /// The session whose generator this was (its slot file named the pid):
    /// that session's marker schedule ends here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<u16>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SampleRecord {
    pub t_ns: Ns,
    /// Since the previous sample; `null` on the first.
    pub dt_ms: Option<f64>,
    pub groups: BTreeMap<&'static str, GroupSample>,
    /// `measured`, or why GPU counters are absent this sample.
    pub gpu: String,
    pub exited: Vec<ExitedProcess>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HangRecord {
    pub t_start_ns: Ns,
    pub t_end_ns: Ns,
    pub duration_ms: f64,
    /// `no_update_with_pending` and/or `os_not_responding`.
    pub kinds: Vec<&'static str>,
    /// Counted only when every full interval it touches is valid.
    pub valid: bool,
    /// The episode was still open when the run ended (judged over the full
    /// intervals before the end).
    #[serde(skip_serializing_if = "is_false")]
    pub open_at_end: bool,
    /// Re-emitted because late input invalidated an interval it touches.
    #[serde(skip_serializing_if = "is_false")]
    pub amended: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CrosscheckRecord {
    pub kind: MarkerKind,
    pub session: u16,
    pub seq: u32,
    pub t_primary_ns: Ns,
    pub t_secondary_ns: Ns,
    pub delta_ms: f64,
    pub refresh_ms: f64,
    pub verdict: &'static str,
}

/// The results schema's incident shape: `kind`, `at`, `detail`, `stops_run`
/// and an optional free-form `data`.
#[derive(Clone, Debug, Serialize)]
pub struct IncidentRecord {
    /// A results-schema incident kind (`capture_lost`, `operator_input`,
    /// `host_crashed`, `harness_error`).
    pub kind: &'static str,
    pub at: String,
    pub detail: String,
    pub stops_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// When it happened on the probe clock (the frames' clock), where known:
    /// `at` is wall time, stamped when the engine got to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t_ns: Option<Ns>,
}

impl IncidentRecord {
    pub fn new(kind: &'static str, detail: impl Into<String>, stops_run: bool) -> Self {
        IncidentRecord {
            kind,
            at: crate::clock::wall_rfc3339(),
            detail: detail.into(),
            stops_run,
            data: None,
            t_ns: None,
        }
    }

    /// It happened at `t_ns` on the probe clock.
    pub fn at_ns(mut self, t_ns: u64) -> Self {
        self.t_ns = Some(Ns(t_ns));
        self
    }

    /// The incident concerns this capture source (`data.source`).
    pub fn about_source(mut self, source: SourceId) -> Self {
        self.data = Some(serde_json::json!({ "source": source.as_str() }));
        self
    }
}

/// Written and flushed immediately BEFORE a key is sent, so another process
/// (`watch`) can recognise the key as the probe's own when it arrives.
#[derive(Clone, Debug, Serialize)]
pub struct InjectIntentRecord {
    pub index: u32,
    pub key: String,
    /// Platform event ids a listener matches the key's events against (X11:
    /// keycode, plus 0x100 for the release); empty where a tag in the event
    /// itself identifies it (Windows `dwExtraInfo`).
    pub ledger: Vec<u32>,
    pub t_ns: Ns,
}

#[derive(Clone, Debug, Serialize)]
pub struct InjectRecord {
    pub index: u32,
    pub key: String,
    /// Taken immediately before the key-down event was sent.
    pub t_inject_ns: Ns,
    /// When the foreground check before this key passed.
    pub t_check_ns: Ns,
    pub tagged: bool,
    pub target: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct InjectAbortRecord {
    pub index: u32,
    pub reason: &'static str,
    pub detail: String,
    pub keys_sent: u32,
}

/// One mechanism's verdict, in the vocabulary of qontinui-claude-config
/// `capability-doctor.sh`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MechanismVerdict {
    pub id: &'static str,
    pub state: crate::doctor::State,
    pub reason: String,
    pub checked_at: String,
    pub depends_on: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DoctorSummary {
    pub inoperative: u32,
    pub degraded: u32,
    pub operative: u32,
    pub unknown: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorRecord {
    pub host: String,
    pub os: String,
    pub checked_at: String,
    pub mode: &'static str,
    pub codec: CodecSpec,
    pub mechanisms: Vec<MechanismVerdict>,
    pub summary: DoctorSummary,
    /// Safety rules the doctor's own probes were seen to break (a D9
    /// foreground change around its probe window). Recorded, not repaired.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<String>,
    /// Observations that are not violations (the operator switched windows
    /// while the doctor's probe was up).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckRecord {
    pub check: String,
    /// Gated: every mechanism it depends on gates (`doctor::gates`: OPERATIVE
    /// or DEGRADED, or UNKNOWN only for a capture source that opened in an
    /// operative session and produced no frame) — or it is a `--require`
    /// check — so a failure, or a check that could not run, fails the
    /// selftest.
    pub gated: bool,
    /// `None` when the check could not run.
    pub passed: Option<bool>,
    pub deps: Vec<&'static str>,
    pub detail: String,
    pub data: serde_json::Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct EndRecord {
    pub t_ns: Ns,
    pub exit_code: i32,
    pub reason: String,
}

/// The probe's own health over a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProbeHealth {
    /// Primary frames processed.
    pub frames: u64,
    /// Primary frames whose presentation time was not known, so their lag
    /// could not be measured (X11's first frame; every X11 frame while the
    /// server clock is not established). The lag figures below cover the
    /// others.
    pub frames_lag_unknown: u64,
    pub scan_ms_p50: Option<f64>,
    pub scan_ms_max: Option<f64>,
    pub lag_ms_p50: Option<f64>,
    /// The lag `behind` judges: the p95 from 20 frames with a known lag,
    /// the second-largest from 10 — never the maximum, so one late frame
    /// cannot decide it. `null` below 10 such frames: no verdict.
    pub lag_ms_judged: Option<f64>,
    /// Which statistic `lag_ms_judged` is: `p95` or `second_largest`;
    /// `null` with fewer than 10 known lags.
    pub lag_statistic: Option<&'static str>,
    /// Frames whose lag exceeded the bound.
    pub frames_over_bound: u64,
    pub lag_ms_max: Option<f64>,
    /// Above this the probe was not keeping up with the source.
    pub lag_bound_ms: f64,
    /// The probe fell behind: latency, loss and cadence are
    /// unknown(harness_error). `null` when fewer than 10 primary frames had a
    /// known lag —
    /// whether the probe kept up was never established, and those KPIs are
    /// unknown(harness_error) as well.
    pub behind: Option<bool>,
}

/// The summary `watch` writes last.
#[derive(Clone, Debug, Serialize)]
pub struct WatchSummary {
    pub t_start_ns: Ns,
    pub t_end_ns: Ns,
    pub primary: SourceId,
    pub secondary: Option<SourceId>,
    pub refresh_ms: f64,
    pub refresh_assumed: bool,
    pub frames: BTreeMap<&'static str, u64>,
    pub frame_interval_ms: KpiValue,
    pub jank_share: KpiValue,
    pub v_actual_max: Option<u32>,
    pub v_actual_p50: Option<u32>,
    pub sessions_seen: Vec<u16>,
    pub intervals_total: u64,
    pub intervals_valid: u64,
    pub intervals_excluded_occluded: u64,
    pub intervals_excluded_operator_activity: u64,
    pub intervals_excluded_harness_error: u64,
    pub hangs: KpiValue,
    pub hang_episodes_excluded: u64,
    pub emitted_markers: u64,
    pub output_latency_ms: KpiValue,
    pub echo_latency_ms: KpiValue,
    pub input_latency_ms: KpiValue,
    pub marker_loss: KpiValue,
    pub marker_skipped: KpiValue,
    pub crosscheck: CrosscheckSummary,
    pub negative_latencies: u64,
    /// Host processes that exited during the run; `null` when no host group
    /// was sampled.
    pub crashes: Option<u64>,
    pub processes: BTreeMap<&'static str, ProcessSummary>,
    pub probe: ProbeHealth,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CrosscheckSummary {
    /// `agree`, `disagree` (latency KPIs become unknown(capture_disagreement)),
    /// or `not_performed`. Judged on the statistics below, never on one
    /// marker (`model::analysis::crosscheck` states the rule).
    pub verdict: &'static str,
    pub compared: u64,
    /// Markers whose gap lies inside the per-marker band.
    pub agreed: u64,
    /// Markers whose gap lies outside the per-marker band (each has a
    /// `crosscheck` record).
    pub disagreed: u64,
    /// The primary's declared stage offset minus the secondary's (ms).
    pub expected_offset_ms: f64,
    /// The per-marker band on the primary-minus-secondary gap (ms).
    pub marker_band_ms: Option<(f64, f64)>,
    /// The band the median gap must lie in (ms).
    pub median_band_ms: Option<(f64, f64)>,
    /// The median primary-minus-secondary gap (ms).
    pub median_gap_ms: Option<f64>,
    /// `disagreed / compared`; the run disagrees above 5%.
    pub outside_share: Option<f64>,
    /// The secondary source coalesced frames where it first saw the marker, so
    /// its time is only an upper bound.
    pub secondary_coalesced: u64,
    pub secondary_unseen: u64,
    pub primary_unseen: u64,
    pub max_abs_delta_ms: Option<f64>,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProcessSummary {
    pub cpu_pct: KpiValue,
    pub gpu_pct: KpiValue,
    /// The group's memory in its LAST sample; `null` unless that sample was
    /// whole. A host that crashed mid-run, or a launcher PID that handed off
    /// and exited, has no end-of-soak value — an earlier reading is not one.
    pub memory_mb_end: Option<f64>,
    pub samples: u64,
    /// Samples in which the group had no live process.
    pub samples_empty: u64,
    /// Samples in which a member could not be read (values withheld).
    pub samples_unreadable: u64,
}

/// Every record type.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Start(StartRecord),
    Capture(CaptureRecord),
    Frame(FrameRecord),
    FirstSeen(FirstSeenRecord),
    Interval(IntervalRecord),
    Sample(SampleRecord),
    Hang(HangRecord),
    Crosscheck(CrosscheckRecord),
    Incident(IncidentRecord),
    InjectIntent(InjectIntentRecord),
    Inject(InjectRecord),
    InjectAbort(InjectAbortRecord),
    Doctor(DoctorRecord),
    Check(CheckRecord),
    Summary(Box<WatchSummary>),
    SelftestSummary(serde_json::Value),
    End(EndRecord),
}

/// Serialize one record as one line (no trailing newline), `schema` first.
pub fn to_line(r: &Record) -> String {
    let body = serde_json::to_string(r).expect("records always serialize");
    // Every record is an internally tagged object, so the body is `{"type":...}`.
    let rest = body.strip_prefix('{').unwrap_or(&body);
    format!("{{\"schema\":\"{SCHEMA}\",{rest}")
}

/// How [`Sink::create`] treats an existing file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileMode {
    /// Refuse a file that already holds anything: a run's records never mix
    /// with another run's (`watch`, `inject`).
    New,
    /// Start the file afresh (the selftest's own diagnostic files).
    Truncate,
}

/// Where records go: a file, stdout, and/or memory (tests).
pub struct Sink {
    file: Option<BufWriter<File>>,
    stdout: bool,
    memory: Option<Arc<Mutex<Vec<String>>>>,
    last_flush: Instant,
    lines: u64,
}

impl Sink {
    /// A sink writing to `path` and optionally echoing to stdout.
    pub fn create(path: &Path, stdout: bool, mode: FileMode) -> std::io::Result<Sink> {
        let file = match mode {
            FileMode::Truncate => File::create(path)?,
            FileMode::New => {
                if std::fs::metadata(path).is_ok_and(|m| m.len() > 0) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!(
                            "{} already holds records from another run; use a fresh run directory",
                            path.display()
                        ),
                    ));
                }
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(path)?
            }
        };
        Ok(Sink {
            file: Some(BufWriter::new(file)),
            stdout,
            memory: None,
            last_flush: Instant::now(),
            lines: 0,
        })
    }

    pub fn stdout_only() -> Sink {
        Sink {
            file: None,
            stdout: true,
            memory: None,
            last_flush: Instant::now(),
            lines: 0,
        }
    }

    /// A sink that keeps every line in memory (shared with the caller).
    pub fn memory() -> (Sink, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        (
            Sink {
                file: None,
                stdout: false,
                memory: Some(lines.clone()),
                last_flush: Instant::now(),
                lines: 0,
            },
            lines,
        )
    }

    /// A sink that drops everything (the doctor's internal probes).
    pub fn discard() -> Sink {
        Sink {
            file: None,
            stdout: false,
            memory: None,
            last_flush: Instant::now(),
            lines: 0,
        }
    }

    pub fn emit(&mut self, r: &Record) {
        let line = to_line(r);
        self.lines += 1;
        if let Some(f) = self.file.as_mut() {
            // A full disk must not kill a measurement mid-run; the summary on
            // stdout still reports it.
            let _ = writeln!(f, "{line}");
        }
        if self.stdout {
            let out = std::io::stdout();
            let mut lock = out.lock();
            let _ = writeln!(lock, "{line}");
            let _ = lock.flush();
        }
        if let Some(m) = &self.memory {
            m.lock().expect("sink memory").push(line);
        }
        // Flushed at least every 250 ms so a tailing orchestrator sees records
        // promptly.
        if self.last_flush.elapsed() >= Duration::from_millis(250) {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        if let Some(f) = self.file.as_mut() {
            let _ = f.flush();
        }
        self.last_flush = Instant::now();
    }

    pub fn lines(&self) -> u64 {
        self.lines
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_schema_then_type_and_string_timestamps() {
        let r = Record::End(EndRecord {
            t_ns: Ns(18_446_744_073_709_551_615),
            exit_code: 0,
            reason: "done".into(),
        });
        let line = to_line(&r);
        assert!(
            line.starts_with("{\"schema\":\"termhost-probe/v1\",\"type\":\"end\","),
            "{line}"
        );
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(v["t_ns"], "18446744073709551615");
    }

    #[test]
    fn memory_sink_collects_lines() {
        let (mut sink, lines) = Sink::memory();
        sink.emit(&Record::End(EndRecord {
            t_ns: Ns(1),
            exit_code: 3,
            reason: "x".into(),
        }));
        assert_eq!(lines.lock().expect("lock").len(), 1);
        assert_eq!(sink.lines(), 1);
    }

    #[test]
    fn incidents_have_only_the_results_schema_fields() {
        let plain = to_line(&Record::Incident(IncidentRecord::new(
            "harness_error",
            "x",
            false,
        )));
        let v: serde_json::Value = serde_json::from_str(&plain).expect("JSON");
        let keys: Vec<&str> = v
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        let mut keys = keys;
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["at", "detail", "kind", "schema", "stops_run", "type"]
        );
        let about = to_line(&Record::Incident(
            IncidentRecord::new("capture_lost", "y", true).about_source(SourceId::Wgc),
        ));
        let v: serde_json::Value = serde_json::from_str(&about).expect("JSON");
        assert!(v.get("source").is_none(), "{about}");
        assert_eq!(v["data"]["source"], "wgc");
        // The probe-clock time rides beside the schema fields only when set.
        assert!(v.get("t_ns").is_none(), "{about}");
        let timed = to_line(&Record::Incident(
            IncidentRecord::new("capture_lost", "y", false)
                .about_source(SourceId::X11)
                .at_ns(42),
        ));
        let v: serde_json::Value = serde_json::from_str(&timed).expect("JSON");
        assert_eq!(v["t_ns"], "42");
    }

    #[test]
    fn a_run_file_is_never_appended_to() {
        let dir = std::env::temp_dir().join(format!(
            "termhost-probe-sink-{}-{}",
            std::process::id(),
            crate::clock::now_ns()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("probe.jsonl");
        {
            let mut s = Sink::create(&path, false, FileMode::New).expect("fresh file");
            s.emit(&Record::End(EndRecord {
                t_ns: Ns(1),
                exit_code: 0,
                reason: "first run".into(),
            }));
        }
        let again = Sink::create(&path, false, FileMode::New);
        assert!(again.is_err(), "a second run must not append to the first");
        // The selftest's own files start afresh instead.
        drop(Sink::create(&path, false, FileMode::Truncate).expect("truncate"));
        assert_eq!(std::fs::metadata(&path).expect("meta").len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
