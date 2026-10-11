//! `selftest`: the instrument shown to measure what it claims — and shown
//! FAILING on known-bad input — before any reading is trusted (plan Phase 2;
//! policy testing/a-test-must-be-able-to-fail, shape 3).
//!
//! On a window of its own, with the same engine `watch` runs:
//!
//! 1. the doctor's verdicts. Every check below is GATED — its failure, or
//!    its failure to run, fails the selftest — unless a mechanism it depends
//!    on is INOPERATIVE-ON-THIS-MACHINE (a hard dependency is absent). An
//!    UNKNOWN or DEGRADED mechanism gates: a probe regression that makes the
//!    doctor's own capture proof fail must turn the selftest red, not quietly
//!    un-gate the checks that would have caught it;
//! 2. clock alignment with Node's `process.hrtime` (a pipe round trip);
//! 3. the process sampler on the probe's own process;
//! 4. each operative capture source sees a calibration marker;
//! 5. output latency: markers painted at scheduled times behind an artificial
//!    delay D (0, 50, 200 ms) must be reported as D within one refresh
//!    interval;
//! 6. WGC and DDA agree on first appearance within one refresh interval;
//! 7. a 3 s paint freeze with markers pending is reported as a hang;
//! 8. injected keys round-trip as echo markers, recognised as the probe's own;
//! 9. ONE untagged injection is classified as not-ours and aborts the arm;
//! 10. occluding the window mid-run turns `hangs` into unknown(occluded);
//! 11. (Windows) a window that stops pumping messages is reported "not
//!     responding".

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use crate::clock::{now_ns, sleep_until_ns};
use crate::codec::{marker_seq, Marker, MarkerKind};
use crate::doctor::{self, ClockFacts, State};
use crate::engine::{Engine, EngineSetup};
use crate::feed::SharedFeed;
use crate::inject::{letters, run_arm, ArmConfig, ArmOutcome};
use crate::model::{EmitSource, Emitted, FramesMode, ModelConfig, MonitorPresence};
use crate::platform::{
    Host, InputMonitor, KeyInjector, PaintSlot, Tagging, TestWindow, TestWindowOptions,
};
use crate::records::{CheckRecord, DoctorRecord, FileMode, Record, Sink, SourceId};
use crate::sampler::{Sampler, SamplerConfig, GROUP_HOST};
use crate::stats;

const MS: u64 = 1_000_000;
/// The session id the selftest's markers carry.
pub const SESSION: u16 = 7;

#[derive(Clone, Debug)]
pub struct SelftestConfig {
    pub delays_ms: Vec<u64>,
    pub markers_per_delay: usize,
    pub marker_period_ms: u64,
    pub keys: usize,
    pub key_interval_ms: u64,
    pub hang_freeze_ms: u64,
    pub occlusion_ms: u64,
    pub os_hung_freeze_ms: u64,
    /// Mechanisms that must be OPERATIVE (else the selftest fails).
    pub require: Vec<String>,
    pub node: String,
    /// Where the internal engines' records go (`selftest-probe.jsonl`,
    /// `selftest-inject.jsonl`); discarded when unset.
    pub run_dir: Option<PathBuf>,
    pub window: TestWindowOptions,
}

impl Default for SelftestConfig {
    fn default() -> Self {
        SelftestConfig {
            delays_ms: vec![0, 50, 200],
            markers_per_delay: 20,
            marker_period_ms: 100,
            keys: 20,
            key_interval_ms: 100,
            hang_freeze_ms: 3000,
            occlusion_ms: 5000,
            os_hung_freeze_ms: 5800,
            require: Vec::new(),
            node: "node".into(),
            run_dir: None,
            window: TestWindowOptions {
                title: "termhost-probe selftest".into(),
                x: 80,
                y: 80,
                client_w: 640,
                client_h: 200,
                activate: true,
                topmost: true,
                cell_w: 12,
                cell_h: 24,
                echo_session: SESSION,
            },
        }
    }
}

/// The selftest's result.
#[derive(Clone, Debug)]
pub struct SelftestOutcome {
    pub checks: Vec<CheckRecord>,
    pub exit_code: i32,
    pub summary: Value,
    /// The doctor's verdicts the checks were gated on.
    pub doctor: DoctorRecord,
}

/// The mechanism a capture source depends on.
pub fn capture_mechanism(kind: SourceId) -> &'static str {
    match kind {
        SourceId::Wgc => doctor::CAPTURE_WGC,
        SourceId::Dda => doctor::CAPTURE_DDA,
        SourceId::X11 | SourceId::Synthetic => doctor::CAPTURE_X11,
    }
}

fn paint_schedule(
    win: Arc<dyn TestWindow>,
    items: Vec<(u64, PaintSlot, Option<Marker>)>,
) -> JoinHandle<Vec<Result<u64, String>>> {
    std::thread::spawn(move || {
        items
            .into_iter()
            .map(|(t, slot, m)| {
                sleep_until_ns(t);
                win.paint(slot, m)
            })
            .collect()
    })
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[stats::nearest_rank(50, v.len()) - 1])
}

struct Selftest<'a> {
    host: &'a mut dyn Host,
    cfg: &'a SelftestConfig,
    out: &'a mut Sink,
    doctor: DoctorRecord,
    checks: Vec<CheckRecord>,
}

impl Selftest<'_> {
    /// Not ruled out on this machine: OPERATIVE, DEGRADED or UNKNOWN. Only an
    /// INOPERATIVE-ON-THIS-MACHINE mechanism (a hard dependency absent) is
    /// skipped and un-gates the checks that depend on it.
    fn usable(&self, id: &str) -> bool {
        doctor::state_of(&self.doctor, id) != State::Inoperative
    }

    fn deps_reason(&self, deps: &[&'static str]) -> String {
        deps.iter()
            .filter(|d| !self.usable(d))
            .map(|d| {
                let m = self.doctor.mechanisms.iter().find(|m| m.id == *d);
                format!(
                    "{d} is {} ({})",
                    m.map_or("UNKNOWN", |m| m.state.as_str()),
                    m.map_or("not reported", |m| m.reason.as_str())
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn check(
        &mut self,
        name: impl Into<String>,
        deps: &[&'static str],
        passed: Option<bool>,
        detail: impl Into<String>,
        data: Value,
    ) {
        let gated = deps.iter().all(|d| self.usable(d));
        let mut detail = detail.into();
        if !gated {
            detail = format!("not gated — {}. {detail}", self.deps_reason(deps));
        }
        let rec = CheckRecord {
            check: name.into(),
            gated,
            passed,
            deps: deps.to_vec(),
            detail,
            data,
        };
        self.out.emit(&Record::Check(rec.clone()));
        self.checks.push(rec);
    }

    fn sink_for(&self, file: &str) -> Sink {
        match &self.cfg.run_dir {
            Some(dir) => Sink::create(&dir.join(file), false, FileMode::Truncate)
                .unwrap_or_else(|_| Sink::discard()),
            None => Sink::discard(),
        }
    }

    fn required(&mut self) {
        for id in self.cfg.require.clone() {
            let state = doctor::state_of(&self.doctor, &id);
            let ok = state == State::Operative;
            let reason = self
                .doctor
                .mechanisms
                .iter()
                .find(|m| m.id == id)
                .map_or_else(
                    || "not a mechanism the doctor reports".to_string(),
                    |m| m.reason.clone(),
                );
            self.check(
                format!("required.{id}"),
                &[],
                Some(ok),
                format!("required mechanism {id} is {}: {reason}", state.as_str()),
                json!({ "state": state.as_str() }),
            );
        }
    }

    fn clock(&mut self, facts: &ClockFacts) {
        match facts {
            ClockFacts::Measured(a) => self.check(
                "clock_aligned",
                &[doctor::CLOCK_ALIGNED],
                Some(a.aligned),
                format!(
                    "node process.hrtime vs the probe clock: offset {:.3} ms, best round trip {:.3} ms",
                    a.offset_ns as f64 / 1e6,
                    a.rtt_ns as f64 / 1e6
                ),
                json!({ "offset_ms": a.offset_ns as f64 / 1e6, "rtt_ms": a.rtt_ns as f64 / 1e6, "samples": a.samples }),
            ),
            ClockFacts::NoNode(why) | ClockFacts::Failed(why) => self.check(
                "clock_aligned",
                &[doctor::CLOCK_ALIGNED],
                None,
                format!("could not compare clocks: {why}"),
                Value::Null,
            ),
        }
    }

    fn sampler(&mut self) {
        let mut s = Sampler::new(
            SamplerConfig {
                host_pids: vec![std::process::id()],
                ..SamplerConfig::default()
            },
            self.host.process_reader(),
        );
        let mut last = None;
        for i in 0..3 {
            if i > 0 {
                // Burn a little CPU so the delta is not zero by construction.
                let until = now_ns() + 150 * MS;
                let mut x = 0u64;
                while now_ns() < until {
                    x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                }
                std::hint::black_box(x);
                std::thread::sleep(Duration::from_millis(100));
            }
            last = Some(s.sample(now_ns()));
        }
        let rec = last.expect("three samples");
        let host = rec.groups.get(GROUP_HOST).cloned().unwrap_or_default();
        let ok = host.pids.contains(&std::process::id())
            && host.cpu_pct.is_some_and(|c| c > 0.0)
            && host.memory_mb.is_some_and(|m| m > 0.0);
        self.check(
            "process_sampler",
            &[],
            Some(ok),
            "CPU-time delta and memory of the probe's own process over 3 samples",
            json!({ "cpu_pct": host.cpu_pct, "memory_mb": host.memory_mb, "rss_mb": host.rss_mb, "gpu": rec.gpu }),
        );
        self.check(
            "gpu_counters_sampled",
            &[doctor::COUNTERS_GPU],
            Some(rec.gpu == "measured"),
            format!("PDH GPU engine counters read during sampling: {}", rec.gpu),
            json!({ "gpu": rec.gpu, "gpu_pct": host.gpu_pct }),
        );
    }

    fn run_window_phases(&mut self) {
        let opts = self.cfg.window.clone();
        let win = match self.host.create_test_window(&opts) {
            Ok(w) => w,
            Err(why) => {
                self.check(
                    "selftest_window",
                    &[doctor::SESSION_INTERACTIVE],
                    Some(false),
                    format!("could not create the selftest window: {why}"),
                    Value::Null,
                );
                return;
            }
        };
        let fg = win.activate();
        std::thread::sleep(Duration::from_millis(300));
        let fg_ok = matches!(fg, Ok(true));
        self.check(
            "selftest_window_foreground",
            &[doctor::SESSION_INTERACTIVE],
            Some(fg_ok),
            match &fg {
                Ok(true) => "the selftest window is foreground".to_string(),
                Ok(false) => "the selftest window could not be made foreground; the measured window must be foreground, so the input and occlusion proofs cannot run".to_string(),
                Err(e) => format!("activation failed: {e}"),
            },
            Value::Null,
        );
        self.window_phases(win.clone(), fg_ok);
        win.close();
    }

    fn window_phases(&mut self, win: Arc<dyn TestWindow>, foreground: bool) {
        let handle = win.handle();
        let refresh = self.host.refresh(handle);
        let refresh_ms = refresh.ns as f64 / 1e6;
        let mut sources = Vec::new();
        for kind in self.host.source_kinds() {
            if !self.usable(capture_mechanism(kind)) {
                continue;
            }
            match self.host.open_source(kind, handle) {
                Ok(s) => sources.push(s),
                Err(why) => self.check(
                    format!("capture_open.{}", kind.as_str()),
                    &[capture_mechanism(kind)],
                    Some(false),
                    format!(
                        "could not open {} on the selftest window: {why}",
                        kind.as_str()
                    ),
                    Value::Null,
                ),
            }
        }
        let kinds: Vec<SourceId> = sources.iter().map(|s| s.id()).collect();
        let (primary, secondary) = crate::watch::choose_sources(&kinds);

        let tagging = Tagging::new();
        let monitor: Result<InputMonitor, String> = self.host.input_monitor(&tagging);
        let inspector = self.host.inspector(handle);
        if let Err(why) = &inspector {
            self.check(
                "window_inspector",
                &[doctor::SESSION_INTERACTIVE],
                Some(false),
                why.clone(),
                Value::Null,
            );
        }
        let feed = SharedFeed::new();
        let mut mc = ModelConfig::new(
            primary.unwrap_or(SourceId::Synthetic),
            secondary,
            refresh.ns,
        );
        mc.refresh_assumed = refresh.assumed;
        mc.capture_available = primary.is_some();
        mc.input_instrumented = true;
        mc.frames = FramesMode::Changes;
        mc.monitor = if monitor.is_ok() {
            MonitorPresence::Configured
        } else {
            MonitorPresence::Absent
        };
        let mut setup = EngineSetup::new(mc);
        setup.sources = sources;
        setup.inspector = inspector.ok();
        setup.input = monitor.as_ref().ok().map(|m| m.bus.subscribe());
        setup.feeds = vec![Box::new(feed.clone())];
        let mut engine = Engine::start(setup, self.sink_for("selftest-probe.jsonl"), now_ns());

        // Calibration: every operative source sees the window.
        engine.pump_until(now_ns() + 300 * MS);
        let calib = Marker::new(MarkerKind::Calibration, SESSION, 0);
        let _ = win.paint(PaintSlot::Output, None);
        let _ = win.paint(PaintSlot::Output, Some(calib));
        engine.pump_while(now_ns() + 3000 * MS, |m| {
            kinds.iter().any(|&k| m.first_seen(k, calib).is_none())
        });
        for &k in &kinds {
            let seen = engine.model().first_seen(k, calib);
            self.check(
                format!("capture_visible.{}", k.as_str()),
                &[capture_mechanism(k)],
                Some(seen.is_some()),
                if seen.is_some() {
                    format!(
                        "{} captured the selftest window and decoded its calibration marker",
                        k.as_str()
                    )
                } else {
                    format!(
                        "{} never showed the selftest window's calibration marker within 3 s",
                        k.as_str()
                    )
                },
                json!({ "refresh_ms": refresh_ms, "refresh_source": refresh.source, "refresh_assumed": refresh.assumed }),
            );
        }

        let mut seq: u64 = 1;
        let mut latency_markers: Vec<Marker> = Vec::new();
        if let Some(primary) = primary {
            self.latency_phases(
                &mut engine,
                &win,
                &feed,
                &kinds,
                &refresh,
                &mut seq,
                &mut latency_markers,
            );
            if let Some(sec) = secondary {
                self.crosscheck(&engine, primary, sec, &latency_markers, &refresh);
            }
            self.hang_phase(&mut engine, &win, &feed, primary, &mut seq);
        } else {
            self.check(
                "output_latency",
                &[doctor::CAPTURE_WGC, doctor::CAPTURE_X11],
                None,
                "no capture source is operative on this machine, so latency cannot be measured",
                Value::Null,
            );
        }

        if let Ok(mon) = &monitor {
            if self.usable(doctor::INPUT_SAFE) && foreground {
                self.input_phases(&mut engine, &win, &feed, mon, &tagging, primary);
            } else {
                self.check(
                    "input_round_trip",
                    &[doctor::INPUT_SAFE],
                    if foreground { None } else { Some(false) },
                    if foreground {
                        "input.safe is INOPERATIVE-ON-THIS-MACHINE".to_string()
                    } else {
                        "the selftest window is not foreground, so D9 forbids injecting any key"
                            .to_string()
                    },
                    Value::Null,
                );
            }
        } else if let Err(why) = &monitor {
            self.check(
                "input_listener",
                &[doctor::INPUT_SAFE],
                Some(false),
                format!("the real-input listener could not start: {why}"),
                Value::Null,
            );
        }

        self.occlusion_phase(&win, &monitor, primary, refresh.ns, foreground);
        if self.host.reports_hung_windows() {
            self.os_hung_phase(&mut engine, &win);
        }
        engine.finish(now_ns());
    }

    #[allow(clippy::too_many_arguments)]
    fn latency_phases(
        &mut self,
        engine: &mut Engine,
        win: &Arc<dyn TestWindow>,
        feed: &SharedFeed,
        kinds: &[SourceId],
        refresh: &crate::platform::Refresh,
        seq: &mut u64,
        all: &mut Vec<Marker>,
    ) {
        // The tolerance is one refresh interval. When the display reports no
        // rate it is the assumed 60 Hz interval, and the check says so: a
        // selftest tolerance, not a KPI.
        let refresh_ms = refresh.ns as f64 / 1e6;
        let tolerance = if refresh.assumed {
            format!("{refresh_ms:.2} ms, ASSUMED: the display reports no refresh rate")
        } else {
            format!("one refresh interval, {refresh_ms:.2} ms")
        };
        let period = self.cfg.marker_period_ms * MS;
        let k = self.cfg.markers_per_delay;
        for &d in &self.cfg.delays_ms.clone() {
            let t0 = now_ns() + 300 * MS;
            let mut items = Vec::new();
            let mut sched = Vec::new();
            for i in 0..k {
                let t_sched = t0 + i as u64 * period;
                let m = Marker::new(MarkerKind::Output, SESSION, marker_seq(*seq));
                feed.push(Emitted {
                    session: SESSION,
                    kind: MarkerKind::Output,
                    seq_full: *seq,
                    t_emit_ns: t_sched,
                    source: EmitSource::Schedule,
                });
                items.push((t_sched + d * MS, PaintSlot::Output, Some(m)));
                sched.push((m, t_sched));
                all.push(m);
                *seq += 1;
            }
            let last_paint = t0 + (k as u64 - 1) * period + d * MS;
            let painter = paint_schedule(win.clone(), items);
            engine.pump_until(last_paint + 600 * MS);
            let painted = painter.join().unwrap_or_default();
            let lateness: Vec<f64> = painted
                .iter()
                .zip(&sched)
                .filter_map(|(p, (_, t))| {
                    p.as_ref()
                        .ok()
                        .map(|tp| (*tp as f64 - (*t + d * MS) as f64) / 1e6)
                })
                .collect();
            let paint_errors = painted.iter().filter(|p| p.is_err()).count();
            for &kind in kinds {
                let mut lat: Vec<f64> = sched
                    .iter()
                    .filter_map(|(m, t)| {
                        engine
                            .model()
                            .first_seen(kind, *m)
                            .map(|fs| (fs.t_ns as f64 - *t as f64) / 1e6)
                    })
                    .collect();
                let observed = lat.len();
                let min = lat.iter().copied().fold(f64::INFINITY, f64::min);
                let max = lat.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let med = median(&mut lat);
                let d_ms = d as f64;
                let ok = observed == k
                    && paint_errors == 0
                    && med.is_some_and(|m| (m - d_ms).abs() <= refresh_ms)
                    && min >= d_ms - 1.0;
                self.check(
                    format!("output_latency.{}.d{d}", kind.as_str()),
                    &[capture_mechanism(kind)],
                    Some(ok),
                    format!(
                        "delay {d} ms: observed {observed}/{k}, median {} ms, min {} ms, max {} ms; must be within {tolerance} of {d} ms and never before the paint",
                        med.map_or("-".into(), |m| format!("{m:.2}")),
                        if observed > 0 { format!("{min:.2}") } else { "-".into() },
                        if observed > 0 { format!("{max:.2}") } else { "-".into() },
                    ),
                    json!({
                        "delay_ms": d,
                        "expected": k,
                        "observed": observed,
                        "median_ms": med,
                        "min_ms": (observed > 0).then_some(min),
                        "max_ms": (observed > 0).then_some(max),
                        "refresh_ms": refresh_ms,
                        "refresh_assumed": refresh.assumed,
                        "painter_late_max_ms": lateness.iter().copied().fold(0.0, f64::max),
                        "paint_errors": paint_errors,
                    }),
                );
            }
        }
    }

    fn crosscheck(
        &mut self,
        engine: &Engine,
        primary: SourceId,
        secondary: SourceId,
        markers: &[Marker],
        refresh: &crate::platform::Refresh,
    ) {
        let refresh_ms = refresh.ns as f64 / 1e6;
        if refresh.assumed {
            self.check(
                "capture_crosscheck",
                &[capture_mechanism(primary), capture_mechanism(secondary)],
                None,
                "not performed: the display reports no refresh rate, so agreement within one refresh interval cannot be judged",
                json!({ "refresh_assumed": true }),
            );
            return;
        }
        let (mut compared, mut disagreed, mut coalesced) = (0u32, 0u32, 0u32);
        let mut max_abs: f64 = 0.0;
        for &m in markers {
            let (Some(p), Some(q)) = (
                engine.model().first_seen(primary, m),
                engine.model().first_seen(secondary, m),
            ) else {
                continue;
            };
            if q.missed.is_some_and(|m| m > 0) && q.t_ns > p.t_ns {
                coalesced += 1;
                continue;
            }
            compared += 1;
            let delta = (q.t_ns as f64 - p.t_ns as f64) / 1e6;
            max_abs = max_abs.max(delta.abs());
            if delta.abs() > refresh_ms {
                disagreed += 1;
            }
        }
        self.check(
            "capture_crosscheck",
            &[capture_mechanism(primary), capture_mechanism(secondary)],
            Some(compared > 0 && disagreed == 0),
            format!(
                "{compared} markers seen cleanly by both {} and {}: {disagreed} first appeared more than one refresh interval ({refresh_ms:.2} ms) apart (max |delta| {max_abs:.2} ms; {coalesced} coalesced by the secondary)",
                primary.as_str(),
                secondary.as_str()
            ),
            json!({ "compared": compared, "disagreed": disagreed, "coalesced": coalesced, "max_abs_delta_ms": max_abs }),
        );
    }

    fn hang_phase(
        &mut self,
        engine: &mut Engine,
        win: &Arc<dyn TestWindow>,
        feed: &SharedFeed,
        primary: SourceId,
        seq: &mut u64,
    ) {
        let period = 100 * MS;
        let freeze = self.cfg.hang_freeze_ms * MS;
        let t0 = now_ns() + 300 * MS;
        let freeze_start = t0 + 500 * MS;
        let freeze_end = freeze_start + freeze;
        let end = freeze_end + 1000 * MS;
        let mut items = Vec::new();
        let mut t = t0;
        while t < end {
            let m = Marker::new(MarkerKind::Output, SESSION, marker_seq(*seq));
            feed.push(Emitted {
                session: SESSION,
                kind: MarkerKind::Output,
                seq_full: *seq,
                t_emit_ns: t,
                source: EmitSource::Schedule,
            });
            if !(freeze_start..freeze_end).contains(&t) {
                items.push((t, PaintSlot::Output, Some(m)));
            }
            *seq += 1;
            t += period;
        }
        let before = engine.model().hang_episodes().len();
        let painter = paint_schedule(win.clone(), items);
        engine.pump_until(end + 1500 * MS);
        let _ = painter.join();
        let episodes: Vec<_> = engine
            .model()
            .hang_episodes()
            .into_iter()
            .skip(before)
            .collect();
        let found = episodes
            .iter()
            .find(|e| e.2 && e.0 < freeze_end && e.1 > freeze_start)
            .copied();
        let spurious = episodes
            .iter()
            .filter(|e| e.2 && (e.1 <= freeze_start || e.0 >= freeze_end))
            .count();
        self.check(
            "hang_detected",
            &[capture_mechanism(primary)],
            Some(found.is_some() && spurious == 0),
            match found {
                Some((s, e, _, _, valid)) => format!(
                    "a {:.0} ms paint freeze with markers pending was reported as a hang from +{:.0} ms to +{:.0} ms (valid: {valid}); {spurious} spurious episodes",
                    freeze as f64 / 1e6,
                    (s as f64 - freeze_start as f64) / 1e6,
                    (e as f64 - freeze_start as f64) / 1e6
                ),
                None => format!(
                    "a {:.0} ms paint freeze with markers pending was NOT reported as a hang ({} episodes in the phase)",
                    freeze as f64 / 1e6,
                    episodes.len()
                ),
            },
            json!({ "freeze_ms": self.cfg.hang_freeze_ms, "episodes": episodes.len(), "spurious": spurious }),
        );
    }

    fn input_phases(
        &mut self,
        engine: &mut Engine,
        win: &Arc<dyn TestWindow>,
        feed: &SharedFeed,
        monitor: &InputMonitor,
        tagging: &Tagging,
        primary: Option<SourceId>,
    ) {
        let mut inj: Box<dyn KeyInjector> = match self.host.injector(win.handle(), tagging) {
            Ok(i) => i,
            Err(why) => {
                self.check(
                    "input_round_trip",
                    &[doctor::INPUT_SAFE],
                    Some(false),
                    format!("no injector: {why}"),
                    Value::Null,
                );
                return;
            }
        };
        let _ = win.activate();
        let target = win.handle().to_string();
        let keys = self.cfg.keys;
        let echo_base = win.echoes().len() as u64;
        let recognised_before = tagging
            .recognised
            .load(std::sync::atomic::Ordering::Relaxed);
        let mut sub = monitor.bus.subscribe();
        let arm = ArmConfig {
            keys: letters(keys),
            interval: Duration::from_millis(self.cfg.key_interval_ms),
            start_delay: Duration::from_millis(200),
            settle: Duration::from_millis(300),
            target: target.clone(),
        };
        let feed2 = feed.clone();
        let mut sink = self.sink_for("selftest-inject.jsonl");
        let handle = std::thread::spawn(move || {
            let mut t_injects = Vec::new();
            let outcome = run_arm(&arm, inj.as_mut(), &mut sub, &mut sink, |i, t, _| {
                feed2.push(Emitted {
                    session: SESSION,
                    kind: MarkerKind::Echo,
                    seq_full: echo_base + u64::from(i),
                    t_emit_ns: t,
                    source: EmitSource::Injector,
                });
                t_injects.push(t);
            });
            (outcome, t_injects, inj, sub, sink)
        });
        engine.pump_while(
            now_ns() + (keys as u64 * self.cfg.key_interval_ms + 5000) * MS,
            |_| !handle.is_finished(),
        );
        engine.pump_until(now_ns() + 800 * MS);
        let Ok((outcome, t_injects, mut inj, mut sub, mut sink)) = handle.join() else {
            self.check(
                "input_round_trip",
                &[doctor::INPUT_SAFE],
                Some(false),
                "the input arm panicked",
                Value::Null,
            );
            return;
        };
        let recognised = tagging
            .recognised
            .load(std::sync::atomic::Ordering::Relaxed)
            - recognised_before;
        let mut echo_lat = Vec::new();
        if let Some(p) = primary {
            for (i, t) in t_injects.iter().enumerate() {
                let m = Marker::new(MarkerKind::Echo, SESSION, marker_seq(echo_base + i as u64));
                if let Some(fs) = engine.model().first_seen(p, m) {
                    if fs.t_ns >= *t && fs.t_ns - t < 1000 * MS {
                        echo_lat.push((fs.t_ns - t) as f64 / 1e6);
                    }
                }
            }
        }
        let sent = outcome.sent();
        let completed = matches!(
            outcome,
            ArmOutcome::Completed {
                late_real_input: 0,
                ..
            }
        );
        let all_echoed = echo_lat.len() == keys;
        let ok = completed && sent as usize == keys && all_echoed && recognised >= 2 * sent as u64;
        let mut lat = echo_lat.clone();
        let deps: Vec<&'static str> = match primary {
            Some(p) => vec![doctor::INPUT_SAFE, capture_mechanism(p)],
            None => vec![doctor::INPUT_SAFE],
        };
        self.check(
            "input_round_trip",
            &deps,
            Some(ok),
            format!(
                "{sent}/{keys} keys injected with the foreground checked before each; {}/{keys} echo markers seen; the listener recognised {recognised} of the probe's {} key events as its own; outcome {outcome:?}",
                echo_lat.len(),
                2 * sent
            ),
            json!({
                "sent": sent,
                "echoed": echo_lat.len(),
                "recognised": recognised,
                "input_latency_p50_ms": median(&mut lat),
                "input_latency_max_ms": echo_lat.iter().copied().fold(0.0, f64::max),
                "tag": format!("0x{:08x}", tagging.tag),
            }),
        );

        // The must-abort proof: one UNTAGGED key mid-arm.
        let _ = win.activate();
        let arm = ArmConfig {
            keys: letters(10),
            interval: Duration::from_millis(150),
            start_delay: Duration::from_millis(200),
            settle: Duration::from_millis(100),
            target,
        };
        let handle = std::thread::spawn(move || {
            run_arm(&arm, inj.as_mut(), &mut sub, &mut sink, |i, _, inj| {
                if i == 1 {
                    let _ = inj.send_char('z', false);
                }
            })
        });
        engine.pump_while(now_ns() + 10_000 * MS, |_| !handle.is_finished());
        let outcome = handle.join();
        let ok = matches!(
            &outcome,
            Ok(ArmOutcome::Aborted { reason, sent: 2, index: 2, .. }) if reason.is_operator_activity()
        );
        self.check(
            "untagged_injection_aborts",
            &[doctor::INPUT_SAFE],
            Some(ok),
            format!("after 2 tagged keys one untagged key was injected; the arm must classify it as not the probe's and abort before key 2: {outcome:?}"),
            Value::Null,
        );
    }

    fn occlusion_phase(
        &mut self,
        win: &Arc<dyn TestWindow>,
        monitor: &Result<InputMonitor, String>,
        primary: Option<SourceId>,
        refresh_ns: u64,
        foreground: bool,
    ) {
        let deps = [doctor::SESSION_INTERACTIVE, doctor::INPUT_SAFE];
        let inspector = match self.host.inspector(win.handle()) {
            Ok(i) => i,
            Err(why) => {
                self.check(
                    "occlusion_reported",
                    &deps,
                    Some(false),
                    format!("no window inspector: {why}"),
                    Value::Null,
                );
                return;
            }
        };
        let _ = win.activate();
        let mut mc = ModelConfig::new(primary.unwrap_or(SourceId::Synthetic), None, refresh_ns);
        mc.min_valid_intervals = 8;
        mc.frames = FramesMode::None;
        mc.monitor = if monitor.is_ok() {
            MonitorPresence::Configured
        } else {
            MonitorPresence::Absent
        };
        let mut setup = EngineSetup::new(mc);
        setup.inspector = Some(inspector);
        setup.input = monitor.as_ref().ok().map(|m| m.bus.subscribe());
        let t0 = now_ns();
        let mut engine = Engine::start(setup, self.sink_for("selftest-occlusion.jsonl"), t0);
        engine.pump_until(t0 + 1250 * MS);
        let covered = win.set_occluded(true);
        engine.pump_until(t0 + (1250 + self.cfg.occlusion_ms) * MS);
        let _ = win.set_occluded(false);
        let (summary, model, _) = engine.finish(t0 + (1500 + self.cfg.occlusion_ms) * MS);
        let records = model.intervals();
        let valid_before = records.iter().filter(|r| r.index < 2 && r.valid).count();
        let occluded_while_foreground = records
            .iter()
            .filter(|r| r.occluded > 0 && r.not_foreground == 0)
            .count();
        let reason = summary.hangs.reason;
        let ok = covered.is_ok()
            && reason == Some("occluded")
            && valid_before >= 1
            && occluded_while_foreground >= 6;
        self.check(
            "occlusion_reported",
            &deps,
            Some(ok),
            format!(
                "window covered for {} ms by an opaque window of ours: hangs reported {:?}({}); {valid_before} interval(s) valid before the cover (proves validity was reachable{}), {occluded_while_foreground} interval(s) occluded while still foreground",
                self.cfg.occlusion_ms,
                summary.hangs.state,
                reason.unwrap_or("-"),
                if foreground { "" } else { "; the window was not foreground" }
            ),
            json!({
                "hangs_state": summary.hangs.state,
                "hangs_reason": reason,
                "intervals": records.len(),
                "valid_before": valid_before,
                "occluded_while_foreground": occluded_while_foreground,
                "occluder": covered.err(),
            }),
        );
    }

    fn os_hung_phase(&mut self, engine: &mut Engine, win: &Arc<dyn TestWindow>) {
        let before = engine.model().hang_episodes().len();
        let t0 = now_ns();
        let froze = win.freeze(Duration::from_millis(self.cfg.os_hung_freeze_ms));
        engine.pump_until(t0 + (self.cfg.os_hung_freeze_ms + 1800) * MS);
        let hung_intervals = engine
            .model()
            .intervals()
            .iter()
            .filter(|r| r.t_start_ns.0 + 500 * MS >= t0 && r.hung > 0)
            .count();
        let episode = engine
            .model()
            .hang_episodes()
            .into_iter()
            .skip(before)
            .any(|e| e.3);
        self.check(
            "os_not_responding_detected",
            &[doctor::SESSION_INTERACTIVE],
            Some(froze.is_ok() && hung_intervals >= 1 && episode),
            format!(
                "the selftest window stopped servicing messages for {} ms: {hung_intervals} interval(s) sampled the OS 'not responding' state, os_not_responding episode reported: {episode}{}",
                self.cfg.os_hung_freeze_ms,
                froze.err().map_or(String::new(), |e| format!(" (freeze failed: {e})"))
            ),
            Value::Null,
        );
    }
}

/// Run the selftest. Returns its outcome; the caller exits with
/// `exit_code` (0 = every gated check passed, 1 = otherwise).
pub fn run(host: &mut dyn Host, cfg: &SelftestConfig, out: &mut Sink) -> SelftestOutcome {
    let facts = host.doctor_facts(&cfg.node);
    let report = doctor::report(&facts);
    out.emit(&Record::Doctor(report.clone()));
    let mut st = Selftest {
        host,
        cfg,
        out,
        doctor: report,
        checks: Vec::new(),
    };
    st.required();
    st.clock(&facts.clock);
    st.sampler();
    let any_capture = [
        doctor::CAPTURE_WGC,
        doctor::CAPTURE_DDA,
        doctor::CAPTURE_X11,
    ]
    .iter()
    .any(|m| st.usable(m));
    if st.usable(doctor::SESSION_INTERACTIVE) || any_capture {
        st.run_window_phases();
    } else {
        let why = st.deps_reason(&[doctor::SESSION_INTERACTIVE]);
        st.check(
            "window_phases",
            &[doctor::SESSION_INTERACTIVE],
            None,
            format!("skipped: no interactive session on this machine ({why})"),
            Value::Null,
        );
    }

    let gated: Vec<&CheckRecord> = st.checks.iter().filter(|c| c.gated).collect();
    let failed: Vec<String> = gated
        .iter()
        .filter(|c| c.passed != Some(true))
        .map(|c| c.check.clone())
        .collect();
    let not_gated: Vec<String> = st
        .checks
        .iter()
        .filter(|c| !c.gated)
        .map(|c| c.check.clone())
        .collect();
    let passed = gated.len() - failed.len();
    let exit_code = i32::from(!failed.is_empty());
    let mechanisms: serde_json::Map<String, Value> = st
        .doctor
        .mechanisms
        .iter()
        .map(|m| (m.id.to_string(), json!(m.state.as_str())))
        .collect();
    let summary = json!({
        "checks": st.checks.len(),
        "gated": gated.len(),
        "passed": passed,
        "failed": failed.len(),
        "not_gated": st.checks.len() - gated.len(),
        "failed_checks": failed,
        "not_gated_checks": not_gated,
        "exit_code": exit_code,
        "mechanisms": mechanisms,
        "codec": crate::spec::spec(),
    });
    st.out.emit(&Record::SelftestSummary(summary.clone()));
    SelftestOutcome {
        checks: st.checks,
        exit_code,
        summary,
        doctor: st.doctor,
    }
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// A Markdown rendering for a CI job summary: the verdict, every check
/// (gated or not, with why), and the mechanisms the doctor found.
pub fn render_markdown(o: &SelftestOutcome, title: &str) -> String {
    let mut s = format!(
        "### {}\n\n**{}** — {} gated check(s), {} failed; {} not gated (a mechanism they depend on is INOPERATIVE-ON-THIS-MACHINE).\n\n",
        md_cell(title),
        if o.exit_code == 0 { "PASS" } else { "FAIL" },
        o.summary["gated"],
        o.summary["failed"],
        o.summary["not_gated"]
    );
    s.push_str("| check | gated | result | detail |\n|---|---|---|---|\n");
    for c in &o.checks {
        let result = match c.passed {
            Some(true) => "pass",
            Some(false) => "FAIL",
            None => "could not run",
        };
        s.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            md_cell(&c.check),
            if c.gated { "gated" } else { "not gated" },
            result,
            md_cell(&c.detail)
        ));
    }
    s.push_str("\n| mechanism | state | reason |\n|---|---|---|\n");
    for m in &o.doctor.mechanisms {
        s.push_str(&format!(
            "| {} | {} | {} |\n",
            m.id,
            m.state.as_str(),
            md_cell(&m.reason)
        ));
    }
    s
}

/// A human rendering of the checks (without `--json`).
pub fn render_human(o: &SelftestOutcome) -> String {
    let mut s = String::from("termhost-probe selftest\n\n");
    for c in &o.checks {
        let tag = match (c.gated, c.passed) {
            (true, Some(true)) => "PASS",
            (true, _) => "FAIL",
            (false, Some(true)) => "pass",
            (false, Some(false)) => "fail",
            (false, None) => "skip",
        };
        s.push_str(&format!("  {tag}  {:<34} {}\n", c.check, c.detail));
    }
    s.push_str(&format!(
        "\n{} gated check(s), {} failed — exit {}\n(lower-case = not gated: a mechanism it depends on is INOPERATIVE-ON-THIS-MACHINE)\n",
        o.summary["gated"], o.summary["failed"], o.exit_code
    ));
    s
}
