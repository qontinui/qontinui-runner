//! The pure measurement engine: timestamped observations in, records and a
//! summary out. Everything that decides a number or a verdict lives here, with
//! no threads, clocks or platform calls, so it is tested event by event.
//!
//! Inputs: frames (with decoded markers) from capture sources, window-state
//! samples, real-input observations, the markers the workload emitted (from
//! the generators' `gen-<k>.jsonl`, or the selftest's own schedule) and the
//! generators' schedules, and process samples. Outputs: `frame` / `first_seen`
//! / `interval` / `hang` / `crosscheck` records as they become final (and
//! amended records when late input changes a verdict), and a [`WatchSummary`].

pub mod analysis;
pub mod intervals;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::codec::{marker_seq, Marker, MarkerKind};
use crate::raster::Rect;
use crate::records::{
    FirstSeenRecord, FrameRecord, HangRecord, MarkerObs, Ns, OccluderInfo, ProbeHealth,
    ProcessSummary, Record, SampleRecord, SourceId, WatchSummary,
};
use crate::sampler::{GROUP_HOST, NOTE_EMPTY, NOTE_UNREADABLE};
use crate::stats::{self, KpiValue};

pub use analysis::FirstSeen;
pub use intervals::{
    Intervals, MonitorPresence, Tally, CLASS_HARNESS, CLASS_OCCLUDED, CLASS_OPERATOR,
};

/// One captured frame, already scanned.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameObs {
    pub source: SourceId,
    pub index: u64,
    pub t_ns: u64,
    pub width: u32,
    pub height: u32,
    /// The part of the window this frame shows, in window pixels: the whole
    /// window for WGC and Desktop Duplication, the damaged band for X11.
    pub region: Rect,
    pub markers: Vec<MarkerObs>,
    pub rejected: u32,
    /// Frames the source coalesced into this one; `None` for a source that
    /// cannot tell.
    pub missed: Option<u32>,
    /// How long the probe took to scan the frame.
    pub scan_ns: u64,
    /// How long after the frame's oldest content was presented the probe
    /// received it; `None` when the source could not say when that was.
    pub lag_ns: Option<u64>,
}

/// The measured window's state at one instant. `None` = could not be
/// established (which makes the interval invalid).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowSample {
    pub t_ns: u64,
    pub foreground: Option<bool>,
    pub visible: Option<bool>,
    pub cloaked: Option<bool>,
    pub minimized: Option<bool>,
    /// The share of the window's area covered by windows above it.
    pub occluded_fraction: Option<f64>,
    pub occluders: Vec<OccluderInfo>,
    /// The OS reports the window not responding (`IsHungAppWindow`).
    pub hung: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDevice {
    Keyboard,
    Pointer,
    Other,
}

/// Whose input an event was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputOrigin {
    /// Injected by this probe run (tagged).
    Probe,
    /// A physical device.
    Real,
    /// Injected, but not with this run's tag — another program. Treated as
    /// operator activity: it is not ours.
    ForeignInjected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputObs {
    pub t_ns: u64,
    pub device: InputDevice,
    pub origin: InputOrigin,
}

/// Who emitted a marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EmitSource {
    /// An AI-session generator (`gen-<k>.jsonl`): output and echo markers.
    Generator,
    /// The selftest's own scheduled output markers.
    Schedule,
    /// An injected key whose echo marker is expected (`t_emit` = `t_inject`):
    /// the input-latency start.
    Injector,
}

/// A marker the workload emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Emitted {
    pub session: u16,
    pub kind: MarkerKind,
    /// The full sequence counter (the marker carries it mod 117649).
    pub seq_full: u64,
    pub t_emit_ns: u64,
    pub source: EmitSource,
}

/// A generator's marker schedule, from its `start` record: marker tick k is
/// due at `t0 + floor(k * 1e6 / marker_hz)` us (Phase 1's `schedule.mjs`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GenSchedule {
    pub session: u16,
    pub t0_ns: u64,
    pub marker_hz: f64,
    /// A load run (markers tick); a calibration run paints one static marker.
    pub load: bool,
}

impl GenSchedule {
    /// When marker tick `k` is due.
    pub fn due_ns(&self, k: u64) -> Option<u64> {
        if !self.load || self.marker_hz.is_nan() || self.marker_hz <= 0.0 {
            return None;
        }
        let us = (k as f64 * 1e6 / self.marker_hz).floor();
        (us.is_finite() && us >= 0.0).then(|| self.t0_ns.saturating_add(us as u64 * 1000))
    }
}

/// Which frames get a `frame` record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramesMode {
    All,
    /// Only frames whose decoded marker set differs from the source's previous
    /// frame.
    Changes,
    None,
}

#[derive(Clone, Debug)]
pub struct ModelConfig {
    pub primary: SourceId,
    pub secondary: Option<SourceId>,
    pub refresh_ns: u64,
    /// The display reported no refresh rate; `refresh_ns` is an assumption.
    pub refresh_assumed: bool,
    /// A source able to be primary was opened; without one every capture KPI
    /// is unknown(capture_unavailable).
    pub capture_available: bool,
    /// The run receives injection times (the selftest); `watch` does not, so
    /// its `input_latency_ms` is unknown(not_instrumented).
    pub input_instrumented: bool,
    pub interval_ns: u64,
    /// How long after an interval ends it stays open for late samples.
    pub interval_grace_ns: u64,
    /// No update for this long with markers pending is a hang (2 s).
    pub hang_after_ns: u64,
    pub min_valid_intervals: u64,
    pub expect_visible: Option<BTreeSet<u16>>,
    pub frames: FramesMode,
    pub monitor: MonitorPresence,
    /// A primary frame received later than this after its time means the
    /// probe was not keeping up (WGC's three-buffer pool is full after about
    /// three refresh intervals, and then drops frames).
    pub lag_bound_ns: u64,
}

impl ModelConfig {
    pub fn new(primary: SourceId, secondary: Option<SourceId>, refresh_ns: u64) -> Self {
        let refresh_ns = refresh_ns.max(1);
        ModelConfig {
            primary,
            secondary,
            refresh_ns,
            refresh_assumed: false,
            capture_available: true,
            input_instrumented: false,
            interval_ns: 500_000_000,
            interval_grace_ns: 150_000_000,
            hang_after_ns: 2_000_000_000,
            min_valid_intervals: stats::HANGS.min_n,
            expect_visible: None,
            frames: FramesMode::All,
            monitor: MonitorPresence::Configured,
            lag_bound_ns: (3 * refresh_ns).max(50_000_000),
        }
    }
}

/// A hang episode in progress or finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Episode {
    start_ns: u64,
    end_ns: u64,
    last_stalled_ns: u64,
    no_update: bool,
    os_hung: bool,
    /// Still in progress when the run ended.
    open_at_end: bool,
}

#[derive(Clone, Debug, Default)]
struct GroupAcc {
    cpu: Vec<f64>,
    gpu: Vec<f64>,
    memory_end: Option<f64>,
    samples: u64,
    empty: u64,
    unreadable: u64,
}

/// The engine state.
pub struct Model {
    cfg: ModelConfig,
    t_start: u64,
    first_seen: HashMap<(SourceId, Marker), FirstSeen>,
    seen_order: BTreeMap<SourceId, Vec<(Marker, FirstSeen)>>,
    frame_counts: BTreeMap<SourceId, u64>,
    primary_frames: Vec<u64>,
    last_markers: HashMap<SourceId, Vec<Marker>>,
    /// Per source: each session's displayed output marker rectangle.
    displayed: HashMap<SourceId, BTreeMap<u16, Rect>>,
    v_samples: Vec<u32>,
    sessions_seen: BTreeSet<u16>,
    emitted: BTreeMap<(EmitSource, u16, MarkerKind), Vec<Emitted>>,
    schedules: BTreeMap<u16, (GenSchedule, Option<u64>)>,
    /// The highest output sequence each generator session has logged.
    last_logged: BTreeMap<u16, u64>,
    intervals: Intervals,
    active_hang: Option<Episode>,
    hangs: Vec<(Episode, bool)>,
    groups: BTreeMap<&'static str, GroupAcc>,
    host_sampled: bool,
    crashes: u64,
    lags: Vec<u64>,
    /// Primary frames whose lag could not be measured.
    lag_unknown: u64,
    scans: Vec<u64>,
    finished: bool,
}

/// Below this many frames with a known lag, `behind` gives no verdict.
pub const LAG_VERDICT_MIN_FRAMES: usize = 10;

/// From this many frames with a known lag, `behind` judges their p95.
pub const LAG_P95_MIN_FRAMES: usize = 20;

/// `inner` lies wholly inside `outer`.
fn contains(outer: &Rect, inner: &Rect) -> bool {
    outer.intersect(inner).as_ref() == Some(inner)
}

/// A nearest-rank percentile of nanosecond samples, in milliseconds.
fn pct_ms(sorted: &[u64], p: u32) -> Option<f64> {
    (!sorted.is_empty()).then(|| sorted[stats::nearest_rank(p, sorted.len()) - 1] as f64 / 1e6)
}

impl Model {
    pub fn new(cfg: ModelConfig, t_start: u64) -> Self {
        let intervals = Intervals::new(t_start, cfg.interval_ns, cfg.monitor);
        Model {
            cfg,
            t_start,
            first_seen: HashMap::new(),
            seen_order: BTreeMap::new(),
            frame_counts: BTreeMap::new(),
            primary_frames: Vec::new(),
            last_markers: HashMap::new(),
            displayed: HashMap::new(),
            v_samples: Vec::new(),
            sessions_seen: BTreeSet::new(),
            emitted: BTreeMap::new(),
            schedules: BTreeMap::new(),
            last_logged: BTreeMap::new(),
            intervals,
            active_hang: None,
            hangs: Vec::new(),
            groups: BTreeMap::new(),
            host_sampled: false,
            crashes: 0,
            lags: Vec::new(),
            lag_unknown: 0,
            scans: Vec::new(),
            finished: false,
        }
    }

    pub fn config(&self) -> &ModelConfig {
        &self.cfg
    }

    pub fn t_start(&self) -> u64 {
        self.t_start
    }

    /// When `source` first saw `marker`.
    pub fn first_seen(&self, source: SourceId, marker: Marker) -> Option<FirstSeen> {
        self.first_seen.get(&(source, marker)).copied()
    }

    pub fn frames_of(&self, source: SourceId) -> u64 {
        self.frame_counts.get(&source).copied().unwrap_or(0)
    }

    pub fn intervals(&self) -> &[crate::records::IntervalRecord] {
        self.intervals.closed()
    }

    pub fn sessions_seen(&self) -> &BTreeSet<u16> {
        &self.sessions_seen
    }

    /// Hang episodes so far: (start, end, no_update, os_hung, valid).
    pub fn hang_episodes(&self) -> Vec<(u64, u64, bool, bool, bool)> {
        self.hangs
            .iter()
            .map(|(e, valid)| (e.start_ns, e.end_ns, e.no_update, e.os_hung, *valid))
            .collect()
    }

    /// A scanned frame arrived.
    pub fn on_frame(&mut self, f: FrameObs) -> Vec<Record> {
        let mut out = Vec::new();
        *self.frame_counts.entry(f.source).or_default() += 1;
        // V: sessions displayed now. A frame re-reads its region, so markers
        // known inside it are replaced by what it shows; markers elsewhere
        // (outside an X11 damage band) are still on screen.
        let shown = self.displayed.entry(f.source).or_default();
        shown.retain(|_, r| !contains(&f.region, r));
        for m in f.markers.iter().filter(|m| m.kind == MarkerKind::Output) {
            shown.insert(m.session, m.rect);
        }
        let v = shown.len() as u32;
        if f.source == self.cfg.primary && self.cfg.capture_available {
            let at = self.primary_frames.partition_point(|&t| t <= f.t_ns);
            self.primary_frames.insert(at, f.t_ns);
            self.intervals.note_frame(f.t_ns);
            self.v_samples.push(v);
            self.sessions_seen.extend(
                f.markers
                    .iter()
                    .filter(|m| m.kind == MarkerKind::Output)
                    .map(|m| m.session),
            );
            match f.lag_ns {
                Some(lag) => self.lags.push(lag),
                None => self.lag_unknown += 1,
            }
            self.scans.push(f.scan_ns);
        }
        for m in &f.markers {
            let key = (f.source, m.marker());
            if self.first_seen.contains_key(&key) {
                continue;
            }
            let fs = FirstSeen {
                t_ns: f.t_ns,
                frame: f.index,
                missed: f.missed,
            };
            self.first_seen.insert(key, fs);
            self.seen_order
                .entry(f.source)
                .or_default()
                .push((m.marker(), fs));
            out.push(Record::FirstSeen(FirstSeenRecord {
                source: f.source,
                kind: m.kind,
                session: m.session,
                seq: m.seq,
                t_ns: Ns(f.t_ns),
                frame: f.index,
                rect: m.rect,
            }));
        }
        let emit_frame = match self.cfg.frames {
            FramesMode::All => true,
            FramesMode::None => false,
            FramesMode::Changes => {
                let mut now: Vec<Marker> = f.markers.iter().map(|m| m.marker()).collect();
                now.sort();
                let changed = self.last_markers.get(&f.source) != Some(&now);
                self.last_markers.insert(f.source, now);
                changed
            }
        };
        if emit_frame {
            out.insert(
                0,
                Record::Frame(FrameRecord {
                    source: f.source,
                    index: f.index,
                    t_ns: Ns(f.t_ns),
                    w: f.width,
                    h: f.height,
                    missed: f.missed,
                    v,
                    markers: f.markers,
                    rejected: f.rejected,
                    scan_ms: f.scan_ns as f64 / 1e6,
                    lag_ms: f.lag_ns.map(|l| l as f64 / 1e6),
                }),
            );
        }
        out
    }

    pub fn on_window_sample(&mut self, s: &WindowSample) {
        self.intervals.add_sample(s);
    }

    /// An input event. Returns the records it amends: an interval that closed
    /// before the event arrived and is now invalid, and any hang episode that
    /// interval had let count.
    pub fn on_input(&mut self, e: &InputObs) -> Vec<Record> {
        let Some(amended) = self.intervals.add_input(e) else {
            return Vec::new();
        };
        let (a, b) = (amended.t_start_ns.0, amended.t_end_ns.0);
        let mut out = vec![Record::Interval(amended)];
        for i in 0..self.hangs.len() {
            let (ep, valid) = self.hangs[i];
            if valid && ep.start_ns < b && ep.end_ns >= a && !self.judge(&ep) {
                self.hangs[i].1 = false;
                out.push(Record::Hang(Self::hang_record(&ep, false, true)));
            }
        }
        out
    }

    pub fn on_monitor_health(&mut self, t_ns: u64, healthy: bool) {
        self.intervals.note_monitor(t_ns, healthy);
    }

    pub fn on_emitted(&mut self, e: Emitted) {
        if e.source == EmitSource::Generator && e.kind == MarkerKind::Output {
            let last = self.last_logged.entry(e.session).or_insert(e.seq_full);
            *last = (*last).max(e.seq_full);
        }
        let list = self
            .emitted
            .entry((e.source, e.session, e.kind))
            .or_default();
        let at = list.partition_point(|x| (x.t_emit_ns, x.seq_full) <= (e.t_emit_ns, e.seq_full));
        list.insert(at, e);
    }

    /// A generator started (its `start` record).
    pub fn on_gen_start(&mut self, s: GenSchedule) {
        self.schedules.insert(s.session, (s, None));
    }

    /// A generator ended at `t_ns`: its `exit` record, or its process seen
    /// gone. The earliest report stands.
    pub fn on_gen_exit(&mut self, session: u16, t_ns: u64) {
        if let Some((_, exit)) = self.schedules.get_mut(&session) {
            *exit = Some(exit.map_or(t_ns, |x| x.min(t_ns)));
        }
    }

    /// A process sample was taken (the record is written by the caller).
    pub fn on_process_sample(&mut self, s: &SampleRecord) {
        for (&name, g) in &s.groups {
            if name == GROUP_HOST {
                self.host_sampled = true;
            }
            let acc = self.groups.entry(name).or_default();
            acc.samples += 1;
            match g.note {
                Some(NOTE_EMPTY) => acc.empty += 1,
                Some(NOTE_UNREADABLE) => acc.unreadable += 1,
                _ => {}
            }
            if let Some(c) = g.cpu_pct {
                acc.cpu.push(c);
            }
            if let Some(gp) = g.gpu_pct {
                acc.gpu.push(gp);
            }
            // The END value is the last sample's, whole or not: after a host
            // crashed, or a launcher handed off and exited, an earlier
            // reading is not the end-of-soak memory.
            acc.memory_end = g.memory_mb;
        }
        self.crashes += s.exited.iter().filter(|e| e.group == GROUP_HOST).count() as u64;
        // A generator killed without writing its `exit` record (its slot
        // file named the pid): its schedule ends where its process was seen
        // gone, so nothing it was due to write is pending after that — a
        // dead generator is a workload death, never a host hang.
        for e in &s.exited {
            if let Some(session) = e.session {
                self.on_gen_exit(session, s.t_ns.0);
            }
        }
    }

    /// Visible sessions: the declared set, else every session the primary
    /// source has shown at least once.
    fn visible_sessions(&self) -> BTreeSet<u16> {
        self.cfg
            .expect_visible
            .clone()
            .unwrap_or_else(|| self.sessions_seen.clone())
    }

    fn primary_seen_by(&self, m: Marker, t: u64) -> bool {
        self.first_seen
            .get(&(self.cfg.primary, m))
            .is_some_and(|fs| fs.t_ns <= t)
    }

    /// When the marker a generator's schedule says is next became due, if
    /// that was at or before `cutoff` and the generator still existed at `t`
    /// (the evaluation time). A generator blocked INSIDE its PTY write (a
    /// host that stopped draining) logs nothing at all, so only its schedule
    /// shows it pending. Once the generator has ended — its `exit` record, or
    /// its process seen gone — nothing of its schedule is pending any more:
    /// it will never write that marker, so waiting for it is not a hang.
    fn overdue_marker(&self, session: u16, cutoff: u64, t: u64) -> Option<u64> {
        let (sched, exit) = self.schedules.get(&session)?;
        let next = self.last_logged.get(&session).map_or(0, |s| s + 1);
        let due = sched.due_ns(next)?;
        (due <= cutoff && exit.is_none_or(|x| x > t)).then_some(due)
    }

    /// The earliest emission among visible sessions' output markers that have
    /// been waiting at least `hang_after` at time `t` with nothing at least as
    /// new shown — including a marker a generator's schedule says is overdue.
    /// `None` = no session had content pending that long.
    fn pending_since(&self, t: u64) -> Option<u64> {
        let cutoff = t.checked_sub(self.cfg.hang_after_ns)?;
        let mut earliest: Option<u64> = None;
        let mut note = |since: u64| earliest = Some(earliest.map_or(since, |x| x.min(since)));
        for session in self.visible_sessions() {
            for src in [EmitSource::Generator, EmitSource::Schedule] {
                let Some(list) = self.emitted.get(&(src, session, MarkerKind::Output)) else {
                    continue;
                };
                let n = list.partition_point(|e| e.t_emit_ns <= cutoff);
                if n == 0 {
                    continue;
                }
                let tail = &list[n - 1..];
                let shown = tail
                    .iter()
                    .any(|e| self.primary_seen_by(analysis::marker_of(e), t));
                if !shown {
                    note(tail[0].t_emit_ns);
                }
            }
            if let Some(due) = self.overdue_marker(session, cutoff, t) {
                note(due);
            }
        }
        earliest
    }

    fn last_frame_at_or_before(&self, t: u64) -> Option<u64> {
        let n = self.primary_frames.partition_point(|&f| f <= t);
        (n > 0).then(|| self.primary_frames[n - 1])
    }

    fn first_frame_after(&self, t: u64) -> Option<u64> {
        let n = self.primary_frames.partition_point(|&f| f <= t);
        self.primary_frames.get(n).copied()
    }

    /// Evaluate the hang rule at the end of a closed interval.
    fn evaluate_hang(&mut self, t_start: u64, t_end: u64, os_hung: bool) -> Option<Record> {
        // Without a primary source there are no frames to miss: the rule
        // would call every interval a hang.
        let stall_since = if self.cfg.capture_available {
            let last = self.last_frame_at_or_before(t_end);
            let updated_recently = last.is_some_and(|f| f + self.cfg.hang_after_ns > t_end);
            if updated_recently {
                None
            } else {
                self.pending_since(t_end)
                    .map(|p| p.max(last.unwrap_or(self.t_start)))
            }
        } else {
            None
        };
        if stall_since.is_some() || os_hung {
            match self.active_hang.as_mut() {
                Some(ep) => {
                    ep.end_ns = t_end;
                    ep.last_stalled_ns = t_end;
                    ep.no_update |= stall_since.is_some();
                    ep.os_hung |= os_hung;
                }
                None => {
                    self.active_hang = Some(Episode {
                        start_ns: stall_since.unwrap_or(t_start),
                        end_ns: t_end,
                        last_stalled_ns: t_end,
                        no_update: stall_since.is_some(),
                        os_hung,
                        open_at_end: false,
                    });
                }
            }
            None
        } else {
            let ep = self.active_hang.take()?;
            let end = self
                .first_frame_after(ep.last_stalled_ns)
                .unwrap_or(t_start)
                .max(ep.last_stalled_ns);
            Some(self.complete_hang(Episode { end_ns: end, ..ep }))
        }
    }

    /// An episode counts when every closed FULL interval it touches is valid.
    /// The span is clipped to the end of the last full interval: an episode
    /// still open when the run ends (a wedge that never recovered) reaches
    /// into the trailing partial interval, which is never valid, and would
    /// otherwise always be excluded.
    fn judge(&self, ep: &Episode) -> bool {
        match self.intervals.last_full_end() {
            Some(e) if ep.start_ns < e => self
                .intervals
                .span_valid(ep.start_ns, ep.end_ns.min(e.saturating_sub(1))),
            _ => false,
        }
    }

    fn hang_record(ep: &Episode, valid: bool, amended: bool) -> HangRecord {
        let mut kinds = Vec::new();
        if ep.no_update {
            kinds.push("no_update_with_pending");
        }
        if ep.os_hung {
            kinds.push("os_not_responding");
        }
        HangRecord {
            t_start_ns: Ns(ep.start_ns),
            t_end_ns: Ns(ep.end_ns),
            duration_ms: (ep.end_ns.saturating_sub(ep.start_ns)) as f64 / 1e6,
            kinds,
            valid,
            open_at_end: ep.open_at_end,
            amended,
        }
    }

    fn complete_hang(&mut self, ep: Episode) -> Record {
        let valid = self.judge(&ep);
        self.hangs.push((ep, valid));
        Record::Hang(Self::hang_record(&ep, valid, false))
    }

    /// Close intervals that are final at `now` and evaluate hangs on them.
    pub fn tick(&mut self, now: u64) -> Vec<Record> {
        let closed = self
            .intervals
            .close_until(now.saturating_sub(self.cfg.interval_grace_ns));
        self.after_close(closed)
    }

    fn after_close(&mut self, closed: Vec<crate::records::IntervalRecord>) -> Vec<Record> {
        let mut out = Vec::new();
        for rec in closed {
            let (a, b, hung, partial) =
                (rec.t_start_ns.0, rec.t_end_ns.0, rec.hung > 0, rec.partial);
            out.push(Record::Interval(rec));
            if !partial {
                if let Some(h) = self.evaluate_hang(a, b, hung) {
                    out.push(h);
                }
            }
        }
        out
    }

    /// Emissions the run could observe: from the first primary frame on.
    /// Before it the probe was not watching, so an earlier marker's "latency"
    /// would include the probe's own start-up.
    fn observable(&self, list: &[Emitted]) -> Vec<Emitted> {
        match self.primary_frames.first() {
            Some(&first) => list
                .iter()
                .filter(|e| e.t_emit_ns >= first)
                .copied()
                .collect(),
            None => Vec::new(),
        }
    }

    fn latency_samples(&self, kind: MarkerKind, sources: &[EmitSource]) -> analysis::Latencies {
        let visible = self.visible_sessions();
        let emitted: Vec<Emitted> = self
            .emitted
            .iter()
            .filter(|((src, session, k), _)| {
                *k == kind
                    && sources.contains(src)
                    && (kind == MarkerKind::Echo || visible.contains(session))
            })
            .flat_map(|(_, list)| self.observable(list))
            .collect();
        analysis::latencies(&emitted, |m| self.first_seen(self.cfg.primary, m))
    }

    /// Why too few valid units remain: insufficient samples, unless an
    /// interval was actually excluded — then the class most exclusions count
    /// toward.
    fn exclusion_reason(&self, excluded_units: u64, valid_units: u64, min_n: u64) -> &'static str {
        if excluded_units == 0 || valid_units + excluded_units < min_n {
            return "insufficient_samples";
        }
        self.intervals
            .tally()
            .dominant_class()
            .unwrap_or("insufficient_samples")
    }

    fn probe_health(&self) -> ProbeHealth {
        let mut lags = self.lags.clone();
        lags.sort_unstable();
        let mut scans = self.scans.clone();
        scans.sort_unstable();
        // Judged on a statistic one late frame cannot decide: a scheduler
        // hiccup or one slow staging copy is not a probe that fell behind.
        // p95 from 20 frames (it tolerates one late frame in 20); from 10,
        // the second-largest lag (one outlier in 10 or more). Below 10 frames
        // with a known lag no verdict is given: tolerating one outlier there
        // would excuse a large share of late frames (half of them at 2).
        let (judged, statistic) = match lags.len() {
            n if n < LAG_VERDICT_MIN_FRAMES => (None, None),
            n if n < LAG_P95_MIN_FRAMES => (Some(lags[n - 2] as f64 / 1e6), Some("second_largest")),
            _ => (pct_ms(&lags, 95), Some("p95")),
        };
        let bound_ms = self.cfg.lag_bound_ns as f64 / 1e6;
        ProbeHealth {
            lag_statistic: statistic,
            frames_over_bound: lags.iter().filter(|&&l| l > self.cfg.lag_bound_ns).count() as u64,
            frames: lags.len() as u64 + self.lag_unknown,
            frames_lag_unknown: self.lag_unknown,
            scan_ms_p50: pct_ms(&scans, 50),
            scan_ms_max: scans.last().map(|&s| s as f64 / 1e6),
            lag_ms_p50: pct_ms(&lags, 50),
            lag_ms_judged: judged,
            lag_ms_max: lags.last().map(|&l| l as f64 / 1e6),
            lag_bound_ms: bound_ms,
            // Judged only over frames whose lag is known; with too few of
            // them, whether the probe kept up was never established.
            behind: judged.map(|j| j > bound_ms),
        }
    }

    /// Close everything and build the summary.
    pub fn finish(&mut self, t_end: u64) -> (Vec<Record>, WatchSummary) {
        assert!(!self.finished, "Model::finish called twice");
        self.finished = true;
        let closed = self.intervals.finish(t_end);
        let mut out = self.after_close(closed);
        if let Some(ep) = self.active_hang.take() {
            out.push(self.complete_hang(Episode {
                end_ns: t_end.max(ep.last_stalled_ns),
                open_at_end: true,
                ..ep
            }));
        }

        let refresh_ns = self.cfg.refresh_ns;
        let probe = self.probe_health();
        // Frames arrived, but not one could be placed against its
        // presentation time: the probe cannot show it kept up.
        let lag_unknown = probe.frames > 0 && probe.behind.is_none();
        let lag_unknown_detail = || {
            format!(
                "only {} of {} primary frames had a known presentation time (a verdict needs {LAG_VERDICT_MIN_FRAMES}), so the probe cannot show it kept up with them",
                probe.frames - probe.frames_lag_unknown,
                probe.frames
            )
        };
        // What makes capture-derived KPIs unknown, in precedence order.
        let blocked: Option<(&'static str, String)> = if !self.cfg.capture_available {
            Some((
                "capture_unavailable",
                "no capture source able to be primary was opened (Desktop Duplication is the cross-check only)".into(),
            ))
        } else if probe.behind == Some(true) {
            Some((
                "harness_error",
                format!(
                    "the probe fell behind: its {} frame lag was {:.1} ms ({} of {} frames over the {:.1} ms bound), so markers may have appeared only in later frames",
                    probe.lag_statistic.unwrap_or("-"),
                    probe.lag_ms_judged.unwrap_or_default(),
                    probe.frames_over_bound,
                    probe.frames - probe.frames_lag_unknown,
                    probe.lag_bound_ms
                ),
            ))
        } else if lag_unknown {
            Some(("harness_error", lag_unknown_detail()))
        } else {
            None
        };
        let refresh_unknown = self.cfg.refresh_assumed.then(|| {
            format!(
                "the display reported no refresh rate ({:.2} ms assumed), so intervals of it cannot be counted",
                refresh_ns as f64 / 1e6
            )
        });
        let gate = |spec: stats::KpiSpec, refresh_dependent: bool, kpi: KpiValue| -> KpiValue {
            if let Some((reason, detail)) = &blocked {
                return KpiValue::unknown(spec, reason, kpi.n, detail.clone());
            }
            match &refresh_unknown {
                Some(detail) if refresh_dependent => {
                    KpiValue::unknown(spec, "refresh_rate_unknown", kpi.n, detail.clone())
                }
                _ => kpi,
            }
        };

        let deltas: Vec<f64> = self
            .primary_frames
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64 / 1e6)
            .collect();
        let frame_interval_ms = gate(
            stats::FRAME_INTERVAL,
            false,
            KpiValue::percentiles(stats::FRAME_INTERVAL, &deltas, &[50, 95]),
        );
        let jank_hits = deltas
            .iter()
            .filter(|&&d| d > 2.0 * refresh_ns as f64 / 1e6)
            .count() as u64;
        let jank_share = gate(
            stats::JANK_SHARE,
            true,
            KpiValue::share(stats::JANK_SHARE, jank_hits, deltas.len() as u64),
        );

        let tally = self.intervals.tally();
        let min_valid = self.cfg.min_valid_intervals;
        let valid_hangs = self.hangs.iter().filter(|(_, v)| *v).count() as u64;
        let excluded_hangs = self.hangs.len() as u64 - valid_hangs;
        let hang_lag_limit = self
            .cfg
            .hang_after_ns
            .saturating_sub(self.cfg.interval_grace_ns) as f64
            / 1e6;
        let hangs = if !self.cfg.capture_available {
            KpiValue::unknown(
                stats::HANGS,
                "capture_unavailable",
                tally.valid,
                "no primary capture source, so a window that stopped updating cannot be seen"
                    .to_string(),
            )
        } else if lag_unknown {
            KpiValue::unknown(
                stats::HANGS,
                "harness_error",
                tally.valid,
                lag_unknown_detail(),
            )
        } else if probe.lag_ms_max.is_some_and(|m| m > hang_lag_limit) {
            KpiValue::unknown(
                stats::HANGS,
                "harness_error",
                tally.valid,
                format!(
                    "a frame reached the probe {:.0} ms late: past {hang_lag_limit:.0} ms an interval can close before its frames arrive, which fabricates hangs",
                    probe.lag_ms_max.unwrap_or_default()
                ),
            )
        } else if tally.valid >= min_valid {
            KpiValue::measured_value(stats::HANGS, valid_hangs as f64, tally.valid)
        } else {
            let reason = self.exclusion_reason(tally.excluded(), tally.valid, min_valid);
            KpiValue::unknown(
                stats::HANGS,
                reason,
                tally.valid,
                format!(
                    "{} valid of {} intervals (need {min_valid}); excluded: {} occluded-class, {} operator-activity, {} harness-error",
                    tally.valid,
                    tally.total(),
                    tally.occluded,
                    tally.operator,
                    tally.harness
                ),
            )
        };

        // Cross-check before latencies: a disagreement makes them unknown.
        let primary_list = self
            .seen_order
            .get(&self.cfg.primary)
            .cloned()
            .unwrap_or_default();
        let (crosscheck, cc_records) = match (self.cfg.secondary, &refresh_unknown) {
            (Some(_), Some(detail)) => {
                let (mut s, _) =
                    analysis::crosscheck(&primary_list, |_| None, &[], |_| None, refresh_ns, false);
                s.detail = format!("not performed: {detail}");
                (s, Vec::new())
            }
            (Some(sec), None) => {
                let sec_list = self.seen_order.get(&sec).cloned().unwrap_or_default();
                analysis::crosscheck(
                    &primary_list,
                    |m| self.first_seen(sec, m),
                    &sec_list,
                    |m| self.first_seen(self.cfg.primary, m),
                    refresh_ns,
                    true,
                )
            }
            (None, _) => {
                analysis::crosscheck(&primary_list, |_| None, &[], |_| None, refresh_ns, false)
            }
        };
        out.extend(cc_records.into_iter().map(Record::Crosscheck));
        let disagreement = (crosscheck.verdict == "disagree").then(|| crosscheck.detail.clone());
        let latency = |spec: stats::KpiSpec, l: &analysis::Latencies| -> KpiValue {
            let kpi = match &disagreement {
                Some(detail) if !l.samples_ms.is_empty() => KpiValue::unknown(
                    spec,
                    "capture_disagreement",
                    l.samples_ms.len() as u64,
                    detail.clone(),
                ),
                _ => KpiValue::percentiles(spec, &l.samples_ms, &[50, 95, 99]),
            };
            gate(spec, false, kpi)
        };
        let out_l = self.latency_samples(
            MarkerKind::Output,
            &[EmitSource::Generator, EmitSource::Schedule],
        );
        let echo_l = self.latency_samples(MarkerKind::Echo, &[EmitSource::Generator]);
        let in_l = self.latency_samples(MarkerKind::Echo, &[EmitSource::Injector]);
        let output_latency_ms = latency(stats::OUTPUT_LATENCY, &out_l);
        let echo_latency_ms = latency(stats::OUTPUT_LATENCY, &echo_l);
        let input_latency_ms = if self.cfg.input_instrumented {
            latency(stats::INPUT_LATENCY, &in_l)
        } else {
            KpiValue::unknown(
                stats::INPUT_LATENCY,
                "not_instrumented",
                0,
                "watch receives no injection times; pair inject.jsonl t_inject_ns with the echo markers' first_seen records".to_string(),
            )
        };

        let mut loss = analysis::LossTally::default();
        for session in self.visible_sessions() {
            for src in [EmitSource::Generator, EmitSource::Schedule] {
                if let Some(list) = self.emitted.get(&(src, session, MarkerKind::Output)) {
                    loss += analysis::loss(
                        &self.observable(list),
                        |m| self.first_seen(self.cfg.primary, m),
                        refresh_ns,
                        t_end,
                        |a, b| !self.intervals.span_valid(a, b),
                    );
                }
            }
        }
        let marker_loss = gate(
            stats::MARKER_LOSS,
            true,
            if loss.eligible >= stats::MARKER_LOSS.min_n {
                KpiValue::share(stats::MARKER_LOSS, loss.lost, loss.eligible)
            } else {
                KpiValue::unknown(
                    stats::MARKER_LOSS,
                    self.exclusion_reason(loss.excluded, loss.eligible, stats::MARKER_LOSS.min_n),
                    loss.eligible,
                    format!(
                        "{} eligible sequences, {} outside valid full intervals",
                        loss.eligible, loss.excluded
                    ),
                )
            },
        );
        let marker_skipped = gate(
            stats::MARKER_SKIPPED,
            true,
            if loss.counted >= stats::MARKER_SKIPPED.min_n {
                KpiValue::share(stats::MARKER_SKIPPED, loss.skipped, loss.counted)
            } else {
                KpiValue::unknown(
                    stats::MARKER_SKIPPED,
                    self.exclusion_reason(loss.excluded, loss.counted, stats::MARKER_SKIPPED.min_n),
                    loss.counted,
                    None,
                )
            },
        );

        let mut v_sorted = self.v_samples.clone();
        v_sorted.sort_unstable();
        let v_actual_p50 =
            (!v_sorted.is_empty()).then(|| v_sorted[stats::nearest_rank(50, v_sorted.len()) - 1]);
        let processes = self
            .groups
            .iter()
            .map(|(&name, g)| (name, Self::process_summary(g)))
            .collect();

        let summary = WatchSummary {
            t_start_ns: Ns(self.t_start),
            t_end_ns: Ns(t_end),
            primary: self.cfg.primary,
            secondary: self.cfg.secondary,
            refresh_ms: refresh_ns as f64 / 1e6,
            refresh_assumed: self.cfg.refresh_assumed,
            frames: self
                .frame_counts
                .iter()
                .map(|(s, n)| (s.as_str(), *n))
                .collect(),
            frame_interval_ms,
            jank_share,
            v_actual_max: v_sorted.last().copied(),
            v_actual_p50,
            sessions_seen: self.sessions_seen.iter().copied().collect(),
            intervals_total: tally.total(),
            intervals_valid: tally.valid,
            intervals_excluded_occluded: tally.occluded,
            intervals_excluded_operator_activity: tally.operator,
            intervals_excluded_harness_error: tally.harness,
            hangs,
            hang_episodes_excluded: excluded_hangs,
            emitted_markers: self.emitted.values().map(|l| l.len() as u64).sum(),
            output_latency_ms,
            echo_latency_ms,
            input_latency_ms,
            marker_loss,
            marker_skipped,
            crosscheck,
            negative_latencies: out_l.negative + echo_l.negative + in_l.negative,
            crashes: self.host_sampled.then_some(self.crashes),
            processes,
            probe,
        };
        (out, summary)
    }

    fn process_summary(g: &GroupAcc) -> ProcessSummary {
        // No sample was readable whole: the group's number was never known.
        let none_whole = g.samples > 0 && g.empty + g.unreadable == g.samples;
        let withheld = |spec: stats::KpiSpec| -> Option<KpiValue> {
            none_whole.then(|| {
                let detail = if g.empty == g.samples {
                    format!(
                        "the group had no live process in any of {} samples",
                        g.samples
                    )
                } else {
                    format!(
                        "no sample of {} was readable whole ({} empty, {} with unreadable members)",
                        g.samples, g.empty, g.unreadable
                    )
                };
                KpiValue::unknown(spec, "harness_error", 0, detail)
            })
        };
        ProcessSummary {
            cpu_pct: if g.cpu.is_empty() {
                withheld(stats::CPU_PCT)
                    .unwrap_or_else(|| KpiValue::mean_p95(stats::CPU_PCT, &g.cpu))
            } else {
                KpiValue::mean_p95(stats::CPU_PCT, &g.cpu)
            },
            gpu_pct: if g.gpu.is_empty() {
                withheld(stats::GPU_PCT).unwrap_or_else(|| {
                    KpiValue::unknown(stats::GPU_PCT, "gpu_counters_unavailable", 0, None)
                })
            } else {
                KpiValue::mean_p95(stats::GPU_PCT, &g.gpu)
            },
            memory_mb_end: g.memory_end,
            samples: g.samples,
            samples_empty: g.empty,
            samples_unreadable: g.unreadable,
        }
    }
}

/// The marker an emitted record refers to (re-exported for callers).
pub fn marker_for(kind: MarkerKind, session: u16, seq_full: u64) -> Marker {
    Marker {
        kind,
        session,
        seq: marker_seq(seq_full),
    }
}

#[cfg(test)]
mod tests;
