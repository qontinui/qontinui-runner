//! The pure measurement engine: timestamped observations in, records and a
//! summary out. Everything that decides a number or a verdict lives here, with
//! no threads, clocks or platform calls, so it is tested event by event.
//!
//! Inputs: frames (with decoded markers) from capture sources, window-state
//! samples, real-input observations, the markers the workload emitted (from
//! the generators' `gen-<k>.jsonl`, or the selftest's own schedule), and
//! process samples. Outputs: `frame` / `first_seen` / `interval` / `hang` /
//! `crosscheck` records as they become final, and a [`WatchSummary`].

pub mod analysis;
pub mod intervals;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::codec::{marker_seq, Marker, MarkerKind};
use crate::records::{
    FirstSeenRecord, FrameRecord, HangRecord, MarkerObs, Ns, OccluderInfo, ProcessSummary, Record,
    SampleRecord, SourceId, WatchSummary,
};
use crate::stats::{self, KpiValue};

pub use analysis::FirstSeen;
pub use intervals::{Intervals, MonitorPresence, CLASS_OCCLUDED, CLASS_OPERATOR};

/// One captured frame, already scanned.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameObs {
    pub source: SourceId,
    pub index: u64,
    pub t_ns: u64,
    pub width: u32,
    pub height: u32,
    pub markers: Vec<MarkerObs>,
    pub rejected: u32,
    pub missed: u32,
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
    pub interval_ns: u64,
    /// How long after an interval ends it stays open for late samples.
    pub interval_grace_ns: u64,
    /// No update for this long with markers pending is a hang (2 s).
    pub hang_after_ns: u64,
    pub min_valid_intervals: u64,
    pub expect_visible: Option<BTreeSet<u16>>,
    pub frames: FramesMode,
    pub monitor: MonitorPresence,
}

impl ModelConfig {
    pub fn new(primary: SourceId, secondary: Option<SourceId>, refresh_ns: u64) -> Self {
        ModelConfig {
            primary,
            secondary,
            refresh_ns: refresh_ns.max(1),
            interval_ns: 500_000_000,
            interval_grace_ns: 150_000_000,
            hang_after_ns: 2_000_000_000,
            min_valid_intervals: stats::HANGS.min_n,
            expect_visible: None,
            frames: FramesMode::All,
            monitor: MonitorPresence::Configured,
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
}

#[derive(Clone, Debug, Default)]
struct GroupAcc {
    cpu: Vec<f64>,
    gpu: Vec<f64>,
    memory_end: Option<f64>,
    samples: u64,
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
    v_samples: Vec<u32>,
    sessions_seen: BTreeSet<u16>,
    emitted: BTreeMap<(EmitSource, u16, MarkerKind), Vec<Emitted>>,
    intervals: Intervals,
    active_hang: Option<Episode>,
    hangs: Vec<(Episode, bool)>,
    groups: BTreeMap<&'static str, GroupAcc>,
    crashes: u64,
    finished: bool,
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
            v_samples: Vec::new(),
            sessions_seen: BTreeSet::new(),
            emitted: BTreeMap::new(),
            intervals,
            active_hang: None,
            hangs: Vec::new(),
            groups: BTreeMap::new(),
            crashes: 0,
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

    /// Hang episodes so far: (start, end, kinds, valid).
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
        let distinct: BTreeSet<u16> = f
            .markers
            .iter()
            .filter(|m| m.kind == MarkerKind::Output)
            .map(|m| m.session)
            .collect();
        if f.source == self.cfg.primary {
            let at = self.primary_frames.partition_point(|&t| t <= f.t_ns);
            self.primary_frames.insert(at, f.t_ns);
            self.intervals.note_frame(f.t_ns);
            self.v_samples.push(distinct.len() as u32);
            self.sessions_seen.extend(distinct.iter().copied());
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
                    v: distinct.len() as u32,
                    markers: f.markers,
                    rejected: f.rejected,
                }),
            );
        }
        out
    }

    pub fn on_window_sample(&mut self, s: &WindowSample) {
        self.intervals.add_sample(s);
    }

    pub fn on_input(&mut self, e: &InputObs) {
        self.intervals.add_input(e);
    }

    pub fn on_monitor_health(&mut self, t_ns: u64, healthy: bool) {
        self.intervals.note_monitor(t_ns, healthy);
    }

    pub fn on_emitted(&mut self, e: Emitted) {
        let list = self
            .emitted
            .entry((e.source, e.session, e.kind))
            .or_default();
        let at = list.partition_point(|x| (x.t_emit_ns, x.seq_full) <= (e.t_emit_ns, e.seq_full));
        list.insert(at, e);
    }

    /// A process sample was taken (the record is written by the caller).
    pub fn on_process_sample(&mut self, s: &SampleRecord) {
        for (&name, g) in &s.groups {
            let acc = self.groups.entry(name).or_default();
            acc.samples += 1;
            if let Some(c) = g.cpu_pct {
                acc.cpu.push(c);
            }
            if let Some(gp) = g.gpu_pct {
                acc.gpu.push(gp);
            }
            if g.memory_mb.is_some() {
                acc.memory_end = g.memory_mb;
            }
        }
        self.crashes += s.exited.iter().filter(|e| e.group == "host").count() as u64;
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

    /// The earliest emission among visible sessions' output markers that have
    /// been waiting at least `hang_after` at time `t` with nothing at least as
    /// new shown. `None` = no session had content pending that long.
    fn pending_since(&self, t: u64) -> Option<u64> {
        let cutoff = t.checked_sub(self.cfg.hang_after_ns)?;
        let mut earliest: Option<u64> = None;
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
                    let since = tail[0].t_emit_ns;
                    earliest = Some(earliest.map_or(since, |x| x.min(since)));
                }
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
        let last = self.last_frame_at_or_before(t_end);
        let updated_recently = last.is_some_and(|f| f + self.cfg.hang_after_ns > t_end);
        let stall_since = if updated_recently {
            None
        } else {
            self.pending_since(t_end)
                .map(|p| p.max(last.unwrap_or(self.t_start)))
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

    fn complete_hang(&mut self, ep: Episode) -> Record {
        let valid = self.intervals.span_valid(ep.start_ns, ep.end_ns);
        self.hangs.push((ep, valid));
        let mut kinds = Vec::new();
        if ep.no_update {
            kinds.push("no_update_with_pending");
        }
        if ep.os_hung {
            kinds.push("os_not_responding");
        }
        Record::Hang(HangRecord {
            t_start_ns: Ns(ep.start_ns),
            t_end_ns: Ns(ep.end_ns),
            duration_ms: (ep.end_ns.saturating_sub(ep.start_ns)) as f64 / 1e6,
            kinds,
            valid,
        })
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

    fn latency_kpi(
        &self,
        spec: stats::KpiSpec,
        kind: MarkerKind,
        sources: &[EmitSource],
        disagreement: Option<&str>,
    ) -> (KpiValue, u64) {
        let visible = self.visible_sessions();
        let emitted = self
            .emitted
            .iter()
            .filter(|((src, session, k), _)| {
                *k == kind
                    && sources.contains(src)
                    && (kind == MarkerKind::Echo || visible.contains(session))
            })
            .flat_map(|(_, list)| list.iter());
        let l = analysis::latencies(emitted, |m| self.first_seen(self.cfg.primary, m));
        let kpi = match disagreement {
            Some(detail) if !l.samples_ms.is_empty() => KpiValue::unknown(
                spec,
                "capture_disagreement",
                l.samples_ms.len() as u64,
                detail.to_string(),
            ),
            _ => KpiValue::percentiles(spec, &l.samples_ms, &[50, 95, 99]),
        };
        (kpi, l.negative)
    }

    /// Why too few valid units remain, when exclusion caused it.
    fn exclusion_reason(&self, excluded_units: u64, valid_units: u64, min_n: u64) -> &'static str {
        if valid_units + excluded_units < min_n {
            return "insufficient_samples";
        }
        let (_, occluded, operator) = self.intervals.tally();
        if operator > occluded {
            CLASS_OPERATOR
        } else {
            CLASS_OCCLUDED
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
                ..ep
            }));
        }

        let refresh_ns = self.cfg.refresh_ns;
        let deltas: Vec<f64> = self
            .primary_frames
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64 / 1e6)
            .collect();
        let frame_interval_ms = KpiValue::percentiles(stats::FRAME_INTERVAL, &deltas, &[50, 95]);
        let jank_hits = deltas
            .iter()
            .filter(|&&d| d > 2.0 * refresh_ns as f64 / 1e6)
            .count() as u64;
        let jank_share = KpiValue::share(stats::JANK_SHARE, jank_hits, deltas.len() as u64);

        let (valid, occluded, operator) = self.intervals.tally();
        let total = valid + occluded + operator;
        let min_valid = self.cfg.min_valid_intervals;
        let valid_hangs = self.hangs.iter().filter(|(_, v)| *v).count() as u64;
        let excluded_hangs = self.hangs.len() as u64 - valid_hangs;
        let hangs = if valid >= min_valid {
            KpiValue::measured_value(stats::HANGS, valid_hangs as f64, valid)
        } else {
            let reason = self.exclusion_reason(total - valid, valid, min_valid);
            KpiValue::unknown(
                stats::HANGS,
                reason,
                valid,
                format!("{valid} valid of {total} intervals (need {min_valid}); excluded: {occluded} occluded-class, {operator} operator-activity"),
            )
        };

        // Cross-check before latencies: a disagreement makes them unknown.
        let primary_list = self
            .seen_order
            .get(&self.cfg.primary)
            .cloned()
            .unwrap_or_default();
        let (crosscheck, cc_records) = match self.cfg.secondary {
            Some(sec) => {
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
            None => analysis::crosscheck(&primary_list, |_| None, &[], |_| None, refresh_ns, false),
        };
        out.extend(cc_records.into_iter().map(Record::Crosscheck));
        let disagreement = (crosscheck.verdict == "disagree").then(|| crosscheck.detail.clone());
        let (output_latency_ms, neg_out) = self.latency_kpi(
            stats::OUTPUT_LATENCY,
            MarkerKind::Output,
            &[EmitSource::Generator, EmitSource::Schedule],
            disagreement.as_deref(),
        );
        let (echo_latency_ms, neg_echo) = self.latency_kpi(
            stats::OUTPUT_LATENCY,
            MarkerKind::Echo,
            &[EmitSource::Generator],
            disagreement.as_deref(),
        );
        let (input_latency_ms, neg_in) = self.latency_kpi(
            stats::INPUT_LATENCY,
            MarkerKind::Echo,
            &[EmitSource::Injector],
            disagreement.as_deref(),
        );

        let mut loss = analysis::LossTally::default();
        for session in self.visible_sessions() {
            for src in [EmitSource::Generator, EmitSource::Schedule] {
                if let Some(list) = self.emitted.get(&(src, session, MarkerKind::Output)) {
                    loss += analysis::loss(
                        list,
                        |m| self.first_seen(self.cfg.primary, m),
                        refresh_ns,
                        t_end,
                        |a, b| !self.intervals.span_valid(a, b),
                    );
                }
            }
        }
        let marker_loss = if loss.eligible >= stats::MARKER_LOSS.min_n {
            KpiValue::share(stats::MARKER_LOSS, loss.lost, loss.eligible)
        } else {
            KpiValue::unknown(
                stats::MARKER_LOSS,
                self.exclusion_reason(loss.excluded, loss.eligible, stats::MARKER_LOSS.min_n),
                loss.eligible,
                format!(
                    "{} eligible sequences, {} excluded by invalid intervals",
                    loss.eligible, loss.excluded
                ),
            )
        };
        let marker_skipped = if loss.counted >= stats::MARKER_SKIPPED.min_n {
            KpiValue::share(stats::MARKER_SKIPPED, loss.skipped, loss.counted)
        } else {
            KpiValue::unknown(
                stats::MARKER_SKIPPED,
                self.exclusion_reason(loss.excluded, loss.counted, stats::MARKER_SKIPPED.min_n),
                loss.counted,
                None,
            )
        };

        let mut v_sorted = self.v_samples.clone();
        v_sorted.sort_unstable();
        let v_actual_p50 =
            (!v_sorted.is_empty()).then(|| v_sorted[stats::nearest_rank(50, v_sorted.len()) - 1]);
        let processes = self
            .groups
            .iter()
            .map(|(&name, g)| {
                (
                    name,
                    ProcessSummary {
                        cpu_pct: KpiValue::mean_p95(stats::CPU_PCT, &g.cpu),
                        gpu_pct: if g.gpu.is_empty() {
                            KpiValue::unknown(stats::GPU_PCT, "gpu_counters_unavailable", 0, None)
                        } else {
                            KpiValue::mean_p95(stats::GPU_PCT, &g.gpu)
                        },
                        memory_mb_end: g.memory_end,
                        samples: g.samples,
                    },
                )
            })
            .collect();

        let summary = WatchSummary {
            t_start_ns: Ns(self.t_start),
            t_end_ns: Ns(t_end),
            primary: self.cfg.primary,
            secondary: self.cfg.secondary,
            refresh_ms: refresh_ns as f64 / 1e6,
            frames: self
                .frame_counts
                .iter()
                .map(|(s, n)| (s.as_str(), *n))
                .collect(),
            frame_interval_ms,
            jank_share,
            v_actual_max: v_sorted.last().copied().unwrap_or(0),
            v_actual_p50,
            sessions_seen: self.sessions_seen.iter().copied().collect(),
            intervals_total: total,
            intervals_valid: valid,
            intervals_excluded_occluded: occluded,
            intervals_excluded_operator_activity: operator,
            hangs,
            hang_episodes_excluded: excluded_hangs,
            emitted_markers: self.emitted.values().map(|l| l.len() as u64).sum(),
            output_latency_ms,
            echo_latency_ms,
            input_latency_ms,
            marker_loss,
            marker_skipped,
            crosscheck,
            negative_latencies: neg_out + neg_echo + neg_in,
            crashes: self.crashes,
            processes,
        };
        (out, summary)
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
