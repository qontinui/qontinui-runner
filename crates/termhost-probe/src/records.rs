//! The probe's output contract: every JSON-lines record it writes.
//!
//! One object per line, `{"schema": "termhost-probe/v1", "type": <kind>, ...}`.
//! Absolute timestamps (`*_ns`) are DECIMAL STRINGS — they exceed 2^53, and the
//! generator's `gen-<k>.jsonl` uses the same convention — on the probe clock
//! ([`crate::clock`]), the timebase of Node's `process.hrtime`. Durations are
//! numbers in milliseconds (`*_ms`). Raw frames are never written anywhere:
//! a frame record carries only its time, size and the markers decoded from it.

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
    /// DXGI Desktop Duplication (the Windows cross-check).
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
}

#[derive(Clone, Debug, Serialize)]
pub struct FrameRecord {
    pub source: SourceId,
    pub index: u64,
    pub t_ns: Ns,
    pub w: u32,
    pub h: u32,
    /// Frames the source coalesced into this one (DXGI `AccumulatedFrames - 1`).
    pub missed: u32,
    /// Distinct sessions with an output marker in this frame (V, measured).
    pub v: u32,
    pub markers: Vec<MarkerObs>,
    /// Red-cyan starts that did not decode.
    pub rejected: u32,
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
    /// `occluded` or `operator_activity` — the catalog reason this interval
    /// counts toward.
    pub exclusion_class: Option<&'static str>,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExitedProcess {
    pub pid: u32,
    pub group: &'static str,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SampleRecord {
    pub t_ns: Ns,
    pub dt_ms: f64,
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
    /// Counted only when every interval it touches is valid.
    pub valid: bool,
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

#[derive(Clone, Debug, Serialize)]
pub struct IncidentRecord {
    /// A results-schema incident kind (`capture_lost`, `operator_input`,
    /// `host_crashed`, `harness_error`).
    pub kind: &'static str,
    pub at: String,
    pub detail: String,
    pub stops_run: bool,
    pub source: Option<SourceId>,
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
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckRecord {
    pub check: String,
    /// Gated: every mechanism it depends on is OPERATIVE (or required), so a
    /// failure fails the selftest.
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

/// The summary `watch` writes last.
#[derive(Clone, Debug, Serialize)]
pub struct WatchSummary {
    pub t_start_ns: Ns,
    pub t_end_ns: Ns,
    pub primary: SourceId,
    pub secondary: Option<SourceId>,
    pub refresh_ms: f64,
    pub frames: BTreeMap<&'static str, u64>,
    pub frame_interval_ms: KpiValue,
    pub jank_share: KpiValue,
    pub v_actual_max: u32,
    pub v_actual_p50: Option<u32>,
    pub sessions_seen: Vec<u16>,
    pub intervals_total: u64,
    pub intervals_valid: u64,
    pub intervals_excluded_occluded: u64,
    pub intervals_excluded_operator_activity: u64,
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
    pub crashes: u64,
    pub processes: BTreeMap<&'static str, ProcessSummary>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CrosscheckSummary {
    /// `agree`, `disagree` (latency KPIs become unknown(capture_disagreement)),
    /// or `not_performed`.
    pub verdict: &'static str,
    pub compared: u64,
    pub agreed: u64,
    pub disagreed: u64,
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
    pub memory_mb_end: Option<f64>,
    pub samples: u64,
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

/// Where records go: a file, stdout, and/or memory (tests).
pub struct Sink {
    file: Option<BufWriter<File>>,
    stdout: bool,
    memory: Option<Arc<Mutex<Vec<String>>>>,
    last_flush: Instant,
    lines: u64,
}

impl Sink {
    /// A sink appending to `path` (created if absent) and optionally echoing
    /// to stdout.
    pub fn to_file(path: &Path, stdout: bool) -> std::io::Result<Sink> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
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
}
