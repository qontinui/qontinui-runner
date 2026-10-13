//! `selftest`: the instrument shown to measure what it claims — and shown
//! FAILING on known-bad input — before any reading is trusted (plan Phase 2;
//! policy testing/a-test-must-be-able-to-fail, shape 3).
//!
//! On a window of its own, with the same engine `watch` runs:
//!
//! 1. the doctor's verdicts. A check is GATED — its failure, or its failure
//!    to run, fails the selftest — when every mechanism it depends on gates
//!    ([`doctor::gates`]): OPERATIVE and DEGRADED ones do; an
//!    INOPERATIVE-ON-THIS-MACHINE one never does; an UNKNOWN one only when
//!    it is a capture source that opened in an OPERATIVE interactive session
//!    and then produced no frame — so a probe regression that breaks the
//!    doctor's own capture proof turns the selftest red instead of
//!    un-gating the checks that would have caught it, while an UNKNOWN the
//!    environment decides (a source that never opened, a GPU counter query
//!    that errored) is recorded with its reason and does not fail the job;
//! 2. clock alignment with Node's `process.hrtime` (a pipe round trip);
//! 3. the process sampler on the probe's own process;
//! 4. each operative capture source sees a calibration marker;
//! 5. output latency: markers painted at scheduled times behind an artificial
//!    delay D (0, 50, 200 ms) must be reported as D within one refresh
//!    interval — gated for DDA and X11. WGC stamps a later stage (the
//!    compositor's render of the capture frame), measured on one machine
//!    class only, so its absolute check is reported but not gated; WGC is
//!    gated instead against DDA (`wgc_render_offset.d*`: median minus DDA's
//!    within the declared offset) or, without DDA, against itself
//!    (`output_latency_slope.wgc`: median minus D steady across delays);
//! 6. WGC and DDA agree on first appearance: the median (p95 from 20
//!    markers) of their per-marker gaps' deviation from the expected render
//!    offset is within one refresh interval;
//! 7. a 3 s paint freeze with markers pending is reported as a hang;
//! 8. injected keys round-trip as echo markers, recognised as the probe's own;
//! 9. ONE untagged injection is classified as not-ours and aborts the arm;
//! 10. occluding the window mid-run turns `hangs` into unknown(occluded);
//! 11. (Windows) a window that stops pumping messages is reported "not
//!     responding";
//! 12. the probe kept up with its primary source over the whole run (frame
//!     lag measured against presentation, within the bound), so the latency
//!     the checks above read was not the probe's own backlog.
//!
//! When the display reports no refresh rate, the one-refresh-interval
//! tolerance of checks 5 and 6 is the assumed 60 Hz interval, and each says
//! so: a bound on the instrument's error against a schedule the selftest
//! controls, not a KPI.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use crate::clock::{now_ns, sleep_until_ns};
use crate::codec::{marker_seq, Marker, MarkerKind};
use crate::doctor::{self, ClockFacts, Facts, State};
use crate::engine::{Engine, EngineSetup};
use crate::feed::SharedFeed;
use crate::inject::{letters, run_arm, ArmConfig, ArmOutcome};
use crate::model::{EmitSource, Emitted, FramesMode, ModelConfig, MonitorPresence};
use crate::platform::{
    Host, InputMonitor, KeyInjector, PaintSlot, Tagging, TestWindow, TestWindowOptions,
};
use crate::records::{CheckRecord, DoctorRecord, FileMode, ProbeHealth, Record, Sink, SourceId};
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

/// How much later WGC's frame times are than Desktop Duplication's, by
/// design. WGC stamps `SystemRelativeTime`, documented by Microsoft as "the
/// QPC time at which the compositor rendered the frame" — the render of the
/// CAPTURE frame, a later stage than the desktop presentation DDA's
/// `LastPresentTime` stamps. Declared from the one measurement there is: on
/// the first hosted run (PR #2177, run 38315983341, windows-latest, Basic
/// Render Driver) WGC trailed DDA at every delay by +9.3, +7.0 and +7.9 ms
/// (medians d0 22.33 vs 13.03 ms, d50 68.42 vs 61.42, d200 219.41 vs
/// 211.53). That one run on one machine class is why WGC's ABSOLUTE latency
/// is reported but not gated, and WGC is instead held to DDA by this offset
/// (and, without DDA, to itself across delays) — the offset band gating
/// only on displays inside the measured class (`WGC_OFFSET_CLASS_MS`).
pub const WGC_RENDER_OFFSET_MS: f64 = 10.0;
/// The slack around the offset: the measured offsets spread 2.3 ms. It is
/// capped at half the refresh interval (`wgc_offset_margin_ms`), so a
/// regression that adds one frame always lands outside the band.
pub const WGC_OFFSET_MARGIN_MS: f64 = 4.0;
/// The refresh intervals the offset was measured on (that run: 15.625 ms,
/// 64 Hz). The band's milliseconds are only known to mean "the same render
/// stage" there: at 144 Hz a one-frame regression could fit inside it, and a
/// real-GPU 60 Hz box may phase-lock to a larger offset. Elsewhere — or with
/// an assumed rate — the offset check is reported, not gated.
pub const WGC_OFFSET_CLASS_MS: (f64, f64) = (14.0, 17.5);

/// Whether the display's refresh interval lies in the measured class.
pub fn refresh_in_offset_class(refresh_ms: f64, assumed: bool) -> bool {
    !assumed && (WGC_OFFSET_CLASS_MS.0..=WGC_OFFSET_CLASS_MS.1).contains(&refresh_ms)
}

/// The band's margin at this refresh interval: `min(4 ms, refresh / 2)`.
/// Inside the measured class (14-17.5 ms) refresh / 2 is at least 7 ms, so
/// the margin there is always 4 ms, and the one-frame guarantee — a whole
/// added interval lands outside offset + margin — comes from the class
/// bounds, not from the cap. The cap only matters for a band evaluated
/// outside the class, where the check is reported, not gated.
pub fn wgc_offset_margin_ms(refresh_ms: f64) -> f64 {
    WGC_OFFSET_MARGIN_MS.min(refresh_ms / 2.0)
}

/// The render-stage offset a source's frame times carry relative to DDA's.
pub fn render_offset_ms(kind: SourceId) -> f64 {
    match kind {
        SourceId::Wgc => WGC_RENDER_OFFSET_MS,
        SourceId::Dda | SourceId::X11 | SourceId::Synthetic => 0.0,
    }
}

/// Whether, at one delay, WGC's median latency minus DDA's lies from
/// `margin` below zero up to the declared render offset plus `margin`.
pub fn wgc_offset_ok(wgc_median_ms: f64, dda_median_ms: f64, margin_ms: f64) -> bool {
    let d = wgc_median_ms - dda_median_ms;
    (-margin_ms..=WGC_RENDER_OFFSET_MS + margin_ms).contains(&d)
}

/// The slope invariant: how far `median(d) - d` moves across the delays. A
/// fixed pipeline stage moves it by nothing; a source that adds latency as
/// the delay grows (or only some of the time) moves it. A CONSTANT added
/// latency moves it by nothing either, so without DDA such a regression is
/// only reported (by the ungated absolute checks), never gated. `None` with
/// fewer than two delays measured.
pub fn latency_spread_ms(medians: &[(u64, f64)]) -> Option<f64> {
    if medians.len() < 2 {
        return None;
    }
    let excess = medians.iter().map(|&(d, m)| m - d as f64);
    let (lo, hi) = excess.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), e| {
        (lo.min(e), hi.max(e))
    });
    Some(hi - lo)
}

/// The cross-check's statistic over per-marker deviations from the expected
/// offset: the median below 20 markers, the nearest-rank p95 from 20 — so a
/// single jittered marker cannot decide it. With its name; `None` with none.
pub fn crosscheck_statistic(deviations_ms: &mut [f64]) -> Option<(f64, &'static str)> {
    if deviations_ms.is_empty() {
        return None;
    }
    deviations_ms.sort_by(f64::total_cmp);
    let n = deviations_ms.len();
    Some(if n >= 20 {
        (deviations_ms[stats::nearest_rank(95, n) - 1], "p95")
    } else {
        (deviations_ms[stats::nearest_rank(50, n) - 1], "median")
    })
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
    facts: Facts,
    doctor: DoctorRecord,
    checks: Vec<CheckRecord>,
}

impl Selftest<'_> {
    /// Not ruled out on this machine: OPERATIVE, DEGRADED or UNKNOWN — worth
    /// attempting. Only an INOPERATIVE-ON-THIS-MACHINE mechanism (a hard
    /// dependency absent) is skipped.
    fn usable(&self, id: &str) -> bool {
        doctor::state_of(&self.doctor, id) != State::Inoperative
    }

    /// Whether checks on `id` gate the selftest ([`doctor::gates`]).
    fn gating(&self, id: &str) -> bool {
        doctor::gates(&self.facts, &self.doctor, id)
    }

    fn deps_reason(&self, deps: &[&'static str]) -> String {
        deps.iter()
            .filter(|d| !self.gating(d))
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
        let gated = deps.iter().all(|d| self.gating(d));
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

    /// A check recorded with its verdict but never gated, and why.
    fn report_only(
        &mut self,
        name: impl Into<String>,
        deps: &[&'static str],
        passed: Option<bool>,
        why: &str,
        detail: impl Into<String>,
        data: Value,
    ) {
        let rec = CheckRecord {
            check: name.into(),
            gated: false,
            passed,
            deps: deps.to_vec(),
            detail: format!("not gated — {why}. {}", detail.into()),
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
            let medians = self.latency_phases(
                &mut engine,
                &win,
                &feed,
                &kinds,
                &refresh,
                &mut seq,
                &mut latency_markers,
            );
            self.wgc_relative_checks(&kinds, &medians, &refresh);
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
        let (summary, _, _) = engine.finish(now_ns());
        if let Some(p) = primary {
            self.probe_kept_up(p, &summary.probe);
        }
    }

    /// The probe's own health over the run: every latency the checks above
    /// read assumes the probe saw each frame promptly. A frame it could not
    /// place against its presentation time proves nothing either way, so a
    /// run with no such placement could not show it kept up.
    fn probe_kept_up(&mut self, primary: SourceId, probe: &ProbeHealth) {
        let source = primary.as_str();
        // Which statistic was judged, over how many frames, and how many
        // were late: one late frame never decides it (see ProbeHealth).
        let judged = format!(
            "{} lag {:.1} ms over {} frame(s) with a known lag; {} over the bound; max {:.1} ms",
            probe.lag_statistic.unwrap_or("-"),
            probe.lag_ms_judged.unwrap_or_default(),
            probe.frames - probe.frames_lag_unknown,
            probe.frames_over_bound,
            probe.lag_ms_max.unwrap_or_default()
        );
        let detail = match probe.behind {
            Some(false) => format!(
                "{source}: the probe kept within the {:.0} ms lag bound ({judged}; {} frame(s) had no presentation time)",
                probe.lag_bound_ms, probe.frames_lag_unknown
            ),
            Some(true) => format!(
                "the probe fell behind {source}: past the {:.0} ms lag bound ({judged}), so the latencies it measured include its own backlog",
                probe.lag_bound_ms
            ),
            None => format!(
                "not performed: {} of {source}'s {} frames could be placed against their presentation time, and a verdict needs {} — whether the probe kept up was never established",
                probe.frames - probe.frames_lag_unknown,
                probe.frames,
                crate::model::LAG_VERDICT_MIN_FRAMES
            ),
        };
        self.check(
            "probe_kept_up",
            &[capture_mechanism(primary)],
            probe.behind.map(|behind| !behind),
            detail,
            serde_json::to_value(probe).unwrap_or(Value::Null),
        );
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
    ) -> Vec<(SourceId, u64, f64)> {
        // The tolerance is one refresh interval. When the display reports no
        // rate it is the assumed 60 Hz interval, and the check says so: a
        // selftest tolerance, not a KPI. WGC's absolute check is reported but
        // not gated (see WGC_RENDER_OFFSET_MS); `wgc_relative_checks` gates
        // WGC against DDA or across delays instead.
        let refresh_ms = refresh.ns as f64 / 1e6;
        let tolerance = if refresh.assumed {
            format!("{refresh_ms:.2} ms, ASSUMED: the display reports no refresh rate")
        } else {
            format!("one refresh interval, {refresh_ms:.2} ms")
        };
        let mut medians = Vec::new();
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
                if let Some(m) = med.filter(|_| observed == k) {
                    medians.push((kind, d, m));
                }
                let ok = observed == k
                    && paint_errors == 0
                    && med.is_some_and(|m| (m - d_ms).abs() <= refresh_ms)
                    && min >= d_ms - 1.0;
                let name = format!("output_latency.{}.d{d}", kind.as_str());
                let detail = format!(
                        "delay {d} ms: observed {observed}/{k}, median {} ms, min {} ms, max {} ms; must be within {tolerance} of {d} ms and never before the paint",
                        med.map_or("-".into(), |m| format!("{m:.2}")),
                        if observed > 0 { format!("{min:.2}") } else { "-".into() },
                        if observed > 0 { format!("{max:.2}") } else { "-".into() },
                    );
                let data = json!({
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
                });
                if kind == SourceId::Wgc {
                    self.report_only(
                        name,
                        &[capture_mechanism(kind)],
                        Some(ok),
                        "WGC stamps the compositor's render of the capture frame, a stage after presentation measured on one machine class only (run 38315983341: +7..9 ms behind DDA); WGC is gated against DDA (wgc_render_offset.*) or across delays (output_latency_slope.wgc) instead",
                        detail,
                        data,
                    );
                } else {
                    self.check(name, &[capture_mechanism(kind)], Some(ok), detail, data);
                }
            }
        }
        medians
    }

    /// WGC's gated latency checks. With DDA open and gating: per delay, WGC's
    /// median minus DDA's stays within the declared render offset. Without
    /// it: WGC's own `median(d) - d` moves by at most one refresh interval
    /// across the delays.
    fn wgc_relative_checks(
        &mut self,
        kinds: &[SourceId],
        medians: &[(SourceId, u64, f64)],
        refresh: &crate::platform::Refresh,
    ) {
        let refresh_ms = refresh.ns as f64 / 1e6;
        if !kinds.contains(&SourceId::Wgc) {
            return;
        }
        let of = |kind: SourceId, d: u64| {
            medians
                .iter()
                .find(|&&(k, dd, _)| k == kind && dd == d)
                .map(|&(_, _, m)| m)
        };
        if kinds.contains(&SourceId::Dda) && self.gating(doctor::CAPTURE_DDA) {
            let margin = wgc_offset_margin_ms(refresh_ms);
            let in_class = refresh_in_offset_class(refresh_ms, refresh.assumed);
            for &d in &self.cfg.delays_ms.clone() {
                let (w, q) = (of(SourceId::Wgc, d), of(SourceId::Dda, d));
                let passed = w.zip(q).map(|(w, q)| wgc_offset_ok(w, q, margin));
                let name = format!("wgc_render_offset.d{d}");
                let deps = [doctor::CAPTURE_WGC, doctor::CAPTURE_DDA];
                let detail = match w.zip(q) {
                    Some((w, q)) => format!(
                        "delay {d} ms: WGC median {w:.2} ms minus DDA median {q:.2} ms = {:.2} ms; must lie from {margin:.1} ms below zero up to {:.1} ms (the declared {WGC_RENDER_OFFSET_MS:.0} ms render offset plus a {margin:.1} ms margin)",
                        w - q,
                        WGC_RENDER_OFFSET_MS + margin
                    ),
                    None => format!("delay {d} ms: WGC or DDA did not see every marker, so their medians cannot be compared"),
                };
                let data = json!({ "delay_ms": d, "wgc_median_ms": w, "dda_median_ms": q, "offset_ms": WGC_RENDER_OFFSET_MS, "margin_ms": margin, "refresh_ms": refresh_ms, "in_measured_class": in_class });
                if in_class {
                    self.check(name, &deps, passed, detail, data);
                } else {
                    let why = format!(
                        "the display's refresh interval ({refresh_ms:.2} ms{}) is outside the class the render offset was measured on ({:.1}-{:.1} ms, run 38315983341), where its milliseconds are not known to mean the same stage",
                        if refresh.assumed { ", ASSUMED" } else { "" },
                        WGC_OFFSET_CLASS_MS.0,
                        WGC_OFFSET_CLASS_MS.1
                    );
                    self.report_only(name, &deps, passed, &why, detail, data);
                }
            }
        } else {
            let wgc: Vec<(u64, f64)> = medians
                .iter()
                .filter(|&&(k, _, _)| k == SourceId::Wgc)
                .map(|&(_, d, m)| (d, m))
                .collect();
            let spread = latency_spread_ms(&wgc);
            self.check(
                "output_latency_slope.wgc",
                &[doctor::CAPTURE_WGC],
                spread.map(|s| wgc.len() == self.cfg.delays_ms.len() && s <= refresh_ms),
                match spread {
                    Some(s) => format!(
                        "no DDA to hold WGC to: WGC's median latency minus the delay moved by {s:.2} ms across delays {:?} ms; must stay within one refresh interval ({refresh_ms:.2} ms) — a fixed render stage moves it by nothing, and so does a CONSTANT added latency, which without DDA is reported (output_latency.wgc.*), not gated",
                        wgc.iter().map(|&(d, _)| d).collect::<Vec<_>>()
                    ),
                    None => "no DDA to hold WGC to, and fewer than two delays had every marker seen".to_string(),
                },
                json!({ "medians": wgc.iter().map(|&(d, m)| json!({"delay_ms": d, "median_ms": m})).collect::<Vec<_>>(), "spread_ms": spread, "refresh_ms": refresh_ms }),
            );
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
        // The latency checks' tolerance, labelled the same way when the
        // display reports no rate: a bound on the instrument's error, not a
        // KPI (which is why `watch` reports its own cross-check as not
        // performed then).
        let refresh_ms = refresh.ns as f64 / 1e6;
        let tolerance = if refresh.assumed {
            format!("{refresh_ms:.2} ms, ASSUMED: the display reports no refresh rate")
        } else {
            format!("one refresh interval, {refresh_ms:.2} ms")
        };
        // Each marker's gap, primary minus secondary, against the gap the two
        // timestamps' stages put there by design (WGC renders after DDA's
        // presentation); judged on a statistic one jittered marker cannot
        // decide.
        let expected = render_offset_ms(primary) - render_offset_ms(secondary);
        let mut deviations: Vec<f64> = Vec::new();
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
            let gap = (p.t_ns as f64 - q.t_ns as f64) / 1e6;
            let deviation = (gap - expected).abs();
            max_abs = max_abs.max(deviation);
            if deviation > refresh_ms {
                disagreed += 1;
            }
            deviations.push(deviation);
        }
        let stat = crosscheck_statistic(&mut deviations);
        self.check(
            "capture_crosscheck",
            &[capture_mechanism(primary), capture_mechanism(secondary)],
            stat.map(|(v, _)| v <= refresh_ms),
            match stat {
                Some((v, name)) => format!(
                    "{compared} markers seen cleanly by both {} and {}: the {name} of their first-appearance gaps' deviation from the expected {expected:.1} ms is {v:.2} ms; must be within {tolerance} ({disagreed} single markers beyond it, max {max_abs:.2} ms; {coalesced} coalesced by the secondary)",
                    primary.as_str(),
                    secondary.as_str()
                ),
                None => format!(
                    "no marker was seen cleanly by both {} and {} ({coalesced} coalesced by the secondary)",
                    primary.as_str(),
                    secondary.as_str()
                ),
            },
            json!({
                "compared": compared,
                "disagreed": disagreed,
                "coalesced": coalesced,
                "expected_gap_ms": expected,
                "statistic": stat.map(|(_, n)| n),
                "statistic_ms": stat.map(|(v, _)| v),
                "max_abs_deviation_ms": max_abs,
                "refresh_ms": refresh_ms,
                "refresh_assumed": refresh.assumed,
            }),
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
        facts: facts.clone(),
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
        "### {}\n\n**{}** — {} gated check(s), {} failed; {} not gated (a mechanism they depend on is INOPERATIVE-ON-THIS-MACHINE, or UNKNOWN for a reason the environment decides).\n\n",
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
        "\n{} gated check(s), {} failed — exit {}\n(lower-case = not gated: a mechanism it depends on is INOPERATIVE-ON-THIS-MACHINE, or UNKNOWN for a reason the environment decides)\n",
        o.summary["gated"], o.summary["failed"], o.exit_code
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgc_is_held_to_dda_by_the_declared_render_offset() {
        let m = wgc_offset_margin_ms(15.625);
        assert_eq!(m, 4.0);
        // The measured offsets (run 38315983341) pass.
        for (w, q) in [(22.33, 13.03), (68.42, 61.42), (219.41, 211.53)] {
            assert!(wgc_offset_ok(w, q, m), "{w} - {q}");
        }
        // One more frame (an interval, ~16 ms) behind DDA fails, and so does
        // WGC running well ahead of DDA.
        assert!(!wgc_offset_ok(38.0, 13.0, m));
        assert!(wgc_offset_ok(27.0, 13.0, m), "the 14 ms edge holds");
        assert!(!wgc_offset_ok(27.1, 13.0, m));
        assert!(!wgc_offset_ok(8.0, 13.0, m));
    }

    #[test]
    fn the_offset_band_gates_only_inside_the_measured_refresh_class() {
        assert!(refresh_in_offset_class(15.625, false), "the measured 64 Hz");
        assert!(refresh_in_offset_class(16.667, false), "60 Hz");
        assert!(!refresh_in_offset_class(16.667, true), "an assumed rate");
        assert!(!refresh_in_offset_class(6.944, false), "144 Hz");
        assert!(!refresh_in_offset_class(33.333, false), "30 Hz");
        // The margin never lets one frame inside the band: at 144 Hz it is
        // half an interval, so offset + one frame lies outside.
        let m = wgc_offset_margin_ms(6.944);
        assert!((m - 3.472).abs() < 1e-9, "{m}");
        assert!(!wgc_offset_ok(13.0 + WGC_RENDER_OFFSET_MS + 6.944, 13.0, m));
    }

    #[test]
    fn a_fixed_stage_keeps_the_slope_and_a_growing_one_breaks_it() {
        assert_eq!(latency_spread_ms(&[(0, 22.0)]), None);
        let fixed = latency_spread_ms(&[(0, 22.3), (50, 68.4), (200, 219.4)]).expect("three");
        assert!(fixed < 5.0, "{fixed}");
        let growing = latency_spread_ms(&[(0, 22.0), (50, 90.0), (200, 260.0)]).expect("three");
        // Excesses 22, 40 and 60 ms: a 38 ms spread.
        assert!((growing - 38.0).abs() < 1e-9, "{growing}");
    }

    #[test]
    fn one_jittered_marker_does_not_decide_the_crosscheck() {
        // Ten markers, one 30 ms off: the median is the typical deviation.
        let mut ten = vec![2.0; 9];
        ten.push(30.0);
        assert_eq!(crosscheck_statistic(&mut ten), Some((2.0, "median")));
        // Sixty markers, two spikes: the p95 stays typical; four spikes
        // (past 5 %) reach it.
        let mut sixty = vec![1.5; 58];
        sixty.extend([30.0, 40.0]);
        assert_eq!(crosscheck_statistic(&mut sixty), Some((1.5, "p95")));
        let mut spiky = vec![1.5; 56];
        spiky.extend([30.0; 4]);
        assert_eq!(crosscheck_statistic(&mut spiky), Some((30.0, "p95")));
        assert_eq!(crosscheck_statistic(&mut []), None);
    }
}
