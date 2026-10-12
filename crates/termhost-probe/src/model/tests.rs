//! The model, event by event: every verdict the probe can reach, reached from
//! a hand-built timeline.

use super::*;
use crate::raster::Rect;
use crate::records::{GroupSample, Record};
use crate::stats::KpiState;

const MS: u64 = 1_000_000;
const REFRESH: u64 = 16_666_667;

fn cfg() -> ModelConfig {
    let mut c = ModelConfig::new(SourceId::Wgc, None, REFRESH);
    c.min_valid_intervals = 4;
    c
}

fn obs(kind: MarkerKind, session: u16, seq: u32) -> MarkerObs {
    MarkerObs {
        kind,
        session,
        seq,
        rect: Rect {
            x: 0,
            y: 0,
            w: 240,
            h: 24,
        },
    }
}

const WINDOW: Rect = Rect {
    x: 0,
    y: 0,
    w: 800,
    h: 600,
};

fn frame(source: SourceId, index: u64, t_ms: u64, markers: Vec<MarkerObs>) -> FrameObs {
    FrameObs {
        source,
        index,
        t_ns: t_ms * MS,
        width: 800,
        height: 600,
        region: WINDOW,
        markers,
        rejected: 0,
        missed: None,
        scan_ns: 0,
        lag_ns: Some(0),
    }
}

fn good_sample(t_ns: u64) -> WindowSample {
    WindowSample {
        t_ns,
        foreground: Some(true),
        visible: Some(true),
        cloaked: Some(false),
        minimized: Some(false),
        occluded_fraction: Some(0.0),
        occluders: vec![],
        hung: Some(false),
    }
}

fn emitted(session: u16, seq: u64, t_ms: u64) -> Emitted {
    Emitted {
        session,
        kind: MarkerKind::Output,
        seq_full: seq,
        t_emit_ns: t_ms * MS,
        source: EmitSource::Generator,
    }
}

/// How a timeline is driven.
struct Timeline<F: Fn(u64) -> bool, S: Fn(u64) -> WindowSample> {
    /// A generator on session 0 emits at 10 Hz while this holds.
    emit: fn(u64) -> bool,
    /// The window shows each marker 12 ms after it is emitted unless frozen.
    frozen: F,
    sample: S,
    /// How late each frame reaches the probe; `None` = the source cannot
    /// say when its frames were presented.
    lag_ms: Option<u64>,
}

/// Feed `[from_ms, to_ms)` of a timeline into `m`: emissions, frames, good
/// window samples every 100 ms (or `sample(t)`), and ticks.
fn drive<F: Fn(u64) -> bool, S: Fn(u64) -> WindowSample>(
    m: &mut Model,
    from_ms: u64,
    to_ms: u64,
    tl: &Timeline<F, S>,
) -> Vec<Record> {
    let mut out = Vec::new();
    for t_ms in from_ms..to_ms {
        let seq = t_ms / 100;
        if t_ms % 100 == 0 && (tl.emit)(t_ms) {
            m.on_emitted(emitted(0, seq, t_ms));
        }
        if t_ms % 100 == 12 && (tl.emit)(t_ms - 12) && !(tl.frozen)(t_ms) {
            let mut f = frame(
                SourceId::Wgc,
                seq + 1,
                t_ms,
                vec![obs(MarkerKind::Output, 0, seq as u32)],
            );
            f.lag_ns = tl.lag_ms.map(|l| l * MS);
            out.extend(m.on_frame(f));
        }
        if t_ms % 100 == 50 {
            m.on_window_sample(&(tl.sample)(t_ms * MS));
        }
        if t_ms % 50 == 0 {
            out.extend(m.tick(t_ms * MS));
        }
    }
    out
}

fn always(_: u64) -> bool {
    true
}

/// A generator on session 0 emitting at 10 Hz from 0 to `end_ms`, the window
/// showing each marker 12 ms later unless `frozen(t)`, then finish.
fn run(
    end_ms: u64,
    frozen: impl Fn(u64) -> bool,
    sample: impl Fn(u64) -> WindowSample,
    cfg: ModelConfig,
) -> (Model, Vec<Record>, WatchSummary) {
    let mut m = Model::new(cfg, 0);
    let tl = Timeline {
        emit: always,
        frozen,
        sample,
        lag_ms: Some(0),
    };
    let mut out = drive(&mut m, 0, end_ms, &tl);
    let (rest, summary) = m.finish(end_ms * MS);
    out.extend(rest);
    (m, out, summary)
}

fn hang_records(out: &[Record]) -> Vec<&crate::records::HangRecord> {
    out.iter()
        .filter_map(|r| match r {
            Record::Hang(h) => Some(h),
            _ => None,
        })
        .collect()
}

#[test]
fn first_appearance_is_recorded_once_per_source() {
    let mut m = Model::new(cfg(), 0);
    let a = m.on_frame(frame(
        SourceId::Wgc,
        1,
        10,
        vec![obs(MarkerKind::Output, 3, 7)],
    ));
    let b = m.on_frame(frame(
        SourceId::Wgc,
        2,
        20,
        vec![obs(MarkerKind::Output, 3, 7)],
    ));
    let c = m.on_frame(frame(
        SourceId::Dda,
        1,
        25,
        vec![obs(MarkerKind::Output, 3, 7)],
    ));
    let count = |rs: &[Record]| {
        rs.iter()
            .filter(|r| matches!(r, Record::FirstSeen(_)))
            .count()
    };
    assert_eq!((count(&a), count(&b), count(&c)), (1, 0, 1));
    let marker = Marker::new(MarkerKind::Output, 3, 7);
    assert_eq!(
        m.first_seen(SourceId::Wgc, marker).map(|f| f.t_ns),
        Some(10 * MS)
    );
    assert_eq!(
        m.first_seen(SourceId::Dda, marker).map(|f| f.t_ns),
        Some(25 * MS)
    );
}

#[test]
fn frames_mode_changes_writes_only_changed_frames() {
    let mut c = cfg();
    c.frames = FramesMode::Changes;
    let mut m = Model::new(c, 0);
    let f = |i, t, seq| frame(SourceId::Wgc, i, t, vec![obs(MarkerKind::Output, 0, seq)]);
    let frames = |rs: Vec<Record>| rs.iter().filter(|r| matches!(r, Record::Frame(_))).count();
    assert_eq!(frames(m.on_frame(f(1, 10, 1))), 1);
    assert_eq!(frames(m.on_frame(f(2, 20, 1))), 0);
    assert_eq!(frames(m.on_frame(f(3, 30, 2))), 1);
}

#[test]
fn a_steady_run_has_no_hangs_and_measures_latency() {
    let (_, out, s) = run(20_000, |_| false, good_sample, cfg());
    assert!(hang_records(&out).is_empty(), "{:?}", hang_records(&out));
    assert_eq!(s.hangs.state, KpiState::Measured);
    assert_eq!(s.hangs.value, Some(0.0));
    assert_eq!(s.output_latency_ms.state, KpiState::Measured);
    assert_eq!(s.output_latency_ms.stat("p50"), Some(12.0));
    assert_eq!(s.marker_loss.value, Some(0.0));
    assert_eq!(s.v_actual_max, Some(1));
    assert_eq!(s.sessions_seen, vec![0]);
    assert_eq!(s.probe.behind, Some(false));
}

#[test]
fn no_update_for_two_seconds_with_markers_pending_is_one_hang() {
    // Frozen from 5.0 s to 8.0 s: markers keep coming, nothing is shown.
    let (m, out, s) = run(20_000, |t| (5_000..8_000).contains(&t), good_sample, cfg());
    let hangs = hang_records(&out);
    assert_eq!(hangs.len(), 1, "{hangs:?}");
    let h = hangs[0];
    assert_eq!(h.kinds, vec!["no_update_with_pending"]);
    assert!(h.valid);
    // From the first emission nothing showed (5.0 s; the window was current
    // until then — its last update was 4.912 s) to the first update after
    // (8.012 s).
    assert_eq!(h.t_start_ns, Ns(5_000 * MS));
    assert_eq!(h.t_end_ns, Ns(8_012 * MS));
    assert_eq!(s.hangs.value, Some(1.0));
    assert_eq!(m.hang_episodes().len(), 1);
    // The frozen sequences were current for 100 ms and never shown: lost.
    assert!(s.marker_loss.value.expect("measured") > 0.0);
}

#[test]
fn a_wedge_that_never_recovers_is_a_counted_hang() {
    // The host wedges at 5 s and never recovers; the window stays
    // foreground and unoccluded. Ending on an interval boundary and inside
    // one must both count it.
    for end_ms in [20_000, 20_250] {
        let (_, out, s) = run(end_ms, |t| t >= 5_000, good_sample, cfg());
        let hangs = hang_records(&out);
        assert_eq!(hangs.len(), 1, "{end_ms}: {hangs:?}");
        assert!(hangs[0].open_at_end, "{end_ms}: {:?}", hangs[0]);
        assert!(hangs[0].valid, "{end_ms}: {:?}", hangs[0]);
        assert_eq!(s.hangs.value, Some(1.0), "{end_ms}: {:?}", s.hangs);
        assert_eq!(s.hang_episodes_excluded, 0, "{end_ms}");
    }
}

#[test]
fn a_shorter_freeze_is_not_a_hang() {
    let (_, out, _) = run(20_000, |t| (5_000..6_500).contains(&t), good_sample, cfg());
    assert!(hang_records(&out).is_empty());
}

#[test]
fn a_generator_blocked_inside_its_write_is_seen_as_pending() {
    // The host stops draining the PTY at 5 s: the generator blocks inside a
    // write, so it logs nothing more, and the window stops updating. Only its
    // schedule (10 Hz from t0) shows a marker overdue.
    let mut m = Model::new(cfg(), 0);
    m.on_gen_start(GenSchedule {
        session: 0,
        t0_ns: 0,
        marker_hz: 10.0,
        load: true,
    });
    let tl = Timeline {
        emit: |t| t < 5_000,
        frozen: |_| false,
        sample: good_sample,
        lag_ms: Some(0),
    };
    let mut out = drive(&mut m, 0, 20_000, &tl);
    let (rest, s) = m.finish(20_000 * MS);
    out.extend(rest);
    let hangs = hang_records(&out);
    assert_eq!(hangs.len(), 1, "{hangs:?}");
    assert_eq!(hangs[0].kinds, vec!["no_update_with_pending"]);
    // Sequence 50 was due at 5.0 s and never written.
    assert_eq!(hangs[0].t_start_ns, Ns(5_000 * MS));
    assert_eq!(s.hangs.value, Some(1.0));
}

#[test]
fn an_exited_generator_has_nothing_overdue() {
    let mut m = Model::new(cfg(), 0);
    m.on_gen_start(GenSchedule {
        session: 0,
        t0_ns: 0,
        marker_hz: 10.0,
        load: true,
    });
    m.on_gen_exit(0, 4_950 * MS);
    // The sampler later sees its process gone too: the earlier end stands.
    m.on_gen_exit(0, 9_000 * MS);
    let tl = Timeline {
        emit: |t| t < 5_000,
        frozen: |_| false,
        sample: good_sample,
        lag_ms: Some(0),
    };
    let mut out = drive(&mut m, 0, 20_000, &tl);
    out.extend(m.finish(20_000 * MS).0);
    assert!(hang_records(&out).is_empty(), "{:?}", hang_records(&out));
}

#[test]
fn coalesced_markers_do_not_cause_a_hang_at_the_end_of_the_run() {
    // One sequence superseded before it was ever shown, then the generator
    // stops and the window stops updating: nothing new is pending.
    let mut m = Model::new(cfg(), 0);
    let mut out = Vec::new();
    for (seq, t) in [(0u64, 0u64), (1, 100), (2, 105)] {
        m.on_emitted(emitted(0, seq, t));
    }
    out.extend(m.on_frame(frame(
        SourceId::Wgc,
        1,
        20,
        vec![obs(MarkerKind::Output, 0, 0)],
    )));
    out.extend(m.on_frame(frame(
        SourceId::Wgc,
        2,
        120,
        vec![obs(MarkerKind::Output, 0, 2)],
    )));
    for t in (0..6_000).step_by(100) {
        m.on_window_sample(&good_sample(t * MS));
        out.extend(m.tick(t * MS));
    }
    let (rest, _) = m.finish(6_000 * MS);
    out.extend(rest);
    assert!(hang_records(&out).is_empty(), "{:?}", hang_records(&out));
}

#[test]
fn os_not_responding_is_a_hang_episode() {
    let sample = |t: u64| WindowSample {
        hung: Some((6_000 * MS..7_000 * MS).contains(&t)),
        ..good_sample(t)
    };
    let (_, out, s) = run(12_000, |_| false, sample, cfg());
    let hangs = hang_records(&out);
    assert_eq!(hangs.len(), 1);
    assert_eq!(hangs[0].kinds, vec!["os_not_responding"]);
    assert_eq!(s.hangs.value, Some(1.0));
}

#[test]
fn an_occluded_run_reports_unknown_occluded_instead_of_counts() {
    let mut c = cfg();
    c.min_valid_intervals = 30;
    let sample = |t: u64| WindowSample {
        occluded_fraction: Some(if t > 3_000 * MS { 0.6 } else { 0.0 }),
        ..good_sample(t)
    };
    let (_, _, s) = run(20_000, |_| false, sample, c);
    assert_eq!(s.hangs.state, KpiState::Unknown);
    assert_eq!(s.hangs.reason, Some("occluded"));
    assert!(s.hangs.value.is_none());
    assert!(s.intervals_excluded_occluded > 30);
}

#[test]
fn operator_activity_reports_unknown_operator_activity() {
    let mut c = cfg();
    c.min_valid_intervals = 30;
    let mut m = Model::new(c, 0);
    for t in (0..20_000u64).step_by(100) {
        m.on_window_sample(&good_sample(t * MS));
        if t > 2_000 {
            m.on_input(&InputObs {
                t_ns: t * MS + 10 * MS,
                device: InputDevice::Pointer,
                origin: InputOrigin::Real,
            });
        }
        m.tick(t * MS);
    }
    let (_, s) = m.finish(20_000 * MS);
    assert_eq!(s.hangs.reason, Some("operator_activity"));
}

#[test]
fn a_listener_that_never_started_is_a_harness_error_not_the_operator() {
    let mut c = cfg();
    c.min_valid_intervals = 30;
    c.monitor = MonitorPresence::Absent;
    let (_, _, s) = run(20_000, |_| false, good_sample, c);
    assert_eq!(s.hangs.reason, Some("harness_error"), "{:?}", s.hangs);
    assert!(s.intervals_excluded_harness_error > 30);
    assert_eq!(s.intervals_excluded_operator_activity, 0);
}

#[test]
fn no_window_samples_is_a_harness_error_not_occlusion() {
    // No inspector: nothing was ever sampled.
    let mut c = cfg();
    c.min_valid_intervals = 30;
    let mut m = Model::new(c, 0);
    for t in (0..20_000u64).step_by(50) {
        m.tick(t * MS);
    }
    let (_, s) = m.finish(20_000 * MS);
    assert_eq!(s.hangs.reason, Some("harness_error"), "{:?}", s.hangs);
}

#[test]
fn a_short_clean_run_is_insufficient_not_occluded() {
    let mut c = cfg();
    c.min_valid_intervals = 60;
    let (_, _, s) = run(5_000, |_| false, good_sample, c);
    assert_eq!(s.hangs.reason, Some("insufficient_samples"));
}

#[test]
fn sequences_outside_closed_intervals_are_not_blamed_on_occlusion() {
    // A clean run ending mid-interval: the last sequences overlap the
    // trailing partial interval, so they cannot be judged — but no interval
    // was excluded, so the reason is too few samples, not occlusion.
    let (_, _, s) = run(10_250, |_| false, good_sample, cfg());
    assert_eq!(
        s.marker_loss.state,
        KpiState::Unknown,
        "{:?}",
        s.marker_loss
    );
    assert_eq!(s.marker_loss.reason, Some("insufficient_samples"));
}

#[test]
fn markers_emitted_before_the_probe_was_watching_are_not_measured() {
    // The generator started 5 s before `watch`, which starts at 5.05 s. The
    // marker on screen then (emitted at 5.0 s) appears in the first frame at
    // 5.062 s: 62 ms of "latency" that is really the probe starting up.
    let mut m = Model::new(cfg(), 5_050 * MS);
    for seq in 0..200u64 {
        m.on_emitted(emitted(0, seq, seq * 100));
    }
    m.on_frame(frame(
        SourceId::Wgc,
        1,
        5_062,
        vec![obs(MarkerKind::Output, 0, 50)],
    ));
    for seq in 51..200u64 {
        m.on_frame(frame(
            SourceId::Wgc,
            seq,
            seq * 100 + 12,
            vec![obs(MarkerKind::Output, 0, seq as u32)],
        ));
    }
    let samples = m
        .latency_samples(MarkerKind::Output, &[EmitSource::Generator])
        .samples_ms;
    assert_eq!(samples.len(), 149, "seq 51..199");
    assert!(samples.iter().all(|&l| l == 12.0), "{samples:?}");
}

#[test]
fn hangs_inside_excluded_intervals_are_not_counted() {
    let sample = |t: u64| WindowSample {
        foreground: Some(!(4_000 * MS..9_000 * MS).contains(&t)),
        ..good_sample(t)
    };
    let (_, out, s) = run(20_000, |t| (5_000..8_000).contains(&t), sample, cfg());
    let hangs = hang_records(&out);
    assert_eq!(hangs.len(), 1);
    assert!(!hangs[0].valid);
    assert_eq!(s.hangs.value, Some(0.0));
    assert_eq!(s.hang_episodes_excluded, 1);
}

#[test]
fn late_real_input_amends_the_interval_and_the_hang_it_let_count() {
    let mut m = Model::new(cfg(), 0);
    let tl = Timeline {
        emit: always,
        frozen: |t| (5_000..8_000).contains(&t),
        sample: good_sample,
        lag_ms: Some(0),
    };
    let out = drive(&mut m, 0, 10_000, &tl);
    let hangs = hang_records(&out);
    assert_eq!(hangs.len(), 1);
    assert!(hangs[0].valid, "valid when it completed");
    // Real input at 6.0 s, delivered after its interval had closed.
    let amended = m.on_input(&InputObs {
        t_ns: 6_000 * MS,
        device: InputDevice::Keyboard,
        origin: InputOrigin::Real,
    });
    let interval = amended
        .iter()
        .find_map(|r| match r {
            Record::Interval(i) => Some(i),
            _ => None,
        })
        .expect("the interval is re-emitted");
    assert!(interval.amended && !interval.valid && interval.index == 12);
    let hang = amended
        .iter()
        .find_map(|r| match r {
            Record::Hang(h) => Some(h),
            _ => None,
        })
        .expect("the hang is re-emitted");
    assert!(hang.amended && !hang.valid);
    let (_, s) = m.finish(10_000 * MS);
    assert_eq!(s.hangs.value, Some(0.0));
    assert_eq!(s.hang_episodes_excluded, 1);
}

#[test]
fn a_capture_disagreement_makes_latency_unknown() {
    let mut c = cfg();
    c.secondary = Some(SourceId::Dda);
    let mut m = Model::new(c, 0);
    for seq in 0..150u32 {
        let t = u64::from(seq) * 100;
        m.on_emitted(emitted(0, u64::from(seq), t));
        m.on_frame(frame(
            SourceId::Wgc,
            u64::from(seq),
            t + 10,
            vec![obs(MarkerKind::Output, 0, seq)],
        ));
        // DDA agrees except for one marker it reports 60 ms later.
        let late = if seq == 77 { 60 } else { 5 };
        m.on_frame(frame(
            SourceId::Dda,
            u64::from(seq),
            t + 10 + late,
            vec![obs(MarkerKind::Output, 0, seq)],
        ));
    }
    let (recs, s) = m.finish(15_100 * MS);
    assert_eq!(s.crosscheck.verdict, "disagree");
    assert_eq!(s.crosscheck.disagreed, 1);
    assert_eq!(s.output_latency_ms.state, KpiState::Unknown);
    assert_eq!(s.output_latency_ms.reason, Some("capture_disagreement"));
    assert!(recs.iter().any(|r| matches!(r, Record::Crosscheck(_))));
}

fn agreeing_run(c: ModelConfig) -> WatchSummary {
    let mut m = Model::new(c, 0);
    for seq in 0..150u32 {
        let t = u64::from(seq) * 100;
        m.on_emitted(emitted(0, u64::from(seq), t));
        m.on_frame(frame(
            SourceId::Wgc,
            u64::from(seq),
            t + 10,
            vec![obs(MarkerKind::Output, 0, seq)],
        ));
        m.on_frame(frame(
            SourceId::Dda,
            u64::from(seq),
            t + 18,
            vec![obs(MarkerKind::Output, 0, seq)],
        ));
    }
    m.finish(15_100 * MS).1
}

#[test]
fn agreeing_sources_keep_the_primary_latency() {
    let mut c = cfg();
    c.secondary = Some(SourceId::Dda);
    let s = agreeing_run(c);
    assert_eq!(s.crosscheck.verdict, "agree");
    // The primary's 10 ms, not an average with the secondary's 18 ms.
    assert_eq!(s.output_latency_ms.stat("p50"), Some(10.0));
}

#[test]
fn an_assumed_refresh_rate_makes_refresh_counted_kpis_unknown() {
    let mut c = cfg();
    c.secondary = Some(SourceId::Dda);
    c.refresh_assumed = true;
    let s = agreeing_run(c);
    assert!(s.refresh_assumed);
    for k in [&s.jank_share, &s.marker_loss, &s.marker_skipped] {
        assert_eq!(k.state, KpiState::Unknown, "{k:?}");
        assert_eq!(k.reason, Some("refresh_rate_unknown"), "{k:?}");
    }
    assert_eq!(s.crosscheck.verdict, "not_performed");
    assert!(
        s.crosscheck.detail.contains("refresh"),
        "{}",
        s.crosscheck.detail
    );
    // What is not counted in refresh intervals stays measured.
    assert_eq!(s.output_latency_ms.state, KpiState::Measured);
    assert_eq!(s.frame_interval_ms.state, KpiState::Measured);
}

#[test]
fn without_a_primary_source_capture_kpis_are_unavailable() {
    let mut c = cfg();
    c.capture_available = false;
    let (_, out, s) = run(20_000, |t| t >= 5_000, good_sample, c);
    assert!(hang_records(&out).is_empty(), "no frames to miss: no hangs");
    for k in [
        &s.output_latency_ms,
        &s.frame_interval_ms,
        &s.jank_share,
        &s.marker_loss,
        &s.marker_skipped,
        &s.hangs,
    ] {
        assert_eq!(k.state, KpiState::Unknown, "{k:?}");
        assert_eq!(k.reason, Some("capture_unavailable"), "{k:?}");
    }
}

#[test]
fn a_probe_that_falls_behind_reports_harness_error() {
    for (lag_ms, hangs_unknown) in [(400, false), (2_000, true)] {
        let mut m = Model::new(cfg(), 0);
        let tl = Timeline {
            emit: always,
            frozen: |_| false,
            sample: good_sample,
            lag_ms: Some(lag_ms),
        };
        drive(&mut m, 0, 20_000, &tl);
        let (_, s) = m.finish(20_000 * MS);
        assert_eq!(s.probe.behind, Some(true), "{lag_ms}: {:?}", s.probe);
        for k in [&s.output_latency_ms, &s.frame_interval_ms, &s.marker_loss] {
            assert_eq!(k.reason, Some("harness_error"), "{lag_ms}: {k:?}");
        }
        assert_eq!(
            s.hangs.reason == Some("harness_error"),
            hangs_unknown,
            "{lag_ms}: {:?}",
            s.hangs
        );
    }
}

#[test]
fn input_latency_is_not_instrumented_in_watch() {
    let (_, _, s) = run(2_000, |_| false, good_sample, cfg());
    assert_eq!(s.input_latency_ms.reason, Some("not_instrumented"));
    let mut c = cfg();
    c.input_instrumented = true;
    let (_, _, s) = run(2_000, |_| false, good_sample, c);
    assert_eq!(s.input_latency_ms.reason, Some("insufficient_samples"));
}

#[test]
fn v_counts_sessions_displayed_across_partial_reads() {
    let mut m = Model::new(cfg(), 0);
    let at = |session: u16, seq: u32| MarkerObs {
        kind: MarkerKind::Output,
        session,
        seq,
        rect: Rect {
            x: 0,
            y: i32::from(session) * 100,
            w: 240,
            h: 24,
        },
    };
    let v_of = |rs: &[Record]| {
        rs.iter().find_map(|r| match r {
            Record::Frame(f) => Some(f.v),
            _ => None,
        })
    };
    // A full read shows four panes' markers.
    let mut f = frame(SourceId::X11, 1, 10, (0..4).map(|s| at(s, 1)).collect());
    assert_eq!(v_of(&m.on_frame(f.clone())), Some(4));
    // A damage band over pane 1 only: its marker repainted, the others still
    // on screen although outside the band.
    f = frame(SourceId::X11, 2, 20, vec![at(1, 2)]);
    f.region = Rect {
        x: 0,
        y: 36,
        w: 800,
        h: 152,
    };
    assert_eq!(v_of(&m.on_frame(f.clone())), Some(4));
    // A band over pane 2 whose marker is gone.
    f = frame(SourceId::X11, 3, 30, vec![]);
    f.region = Rect {
        x: 0,
        y: 136,
        w: 800,
        h: 152,
    };
    assert_eq!(v_of(&m.on_frame(f)), Some(3));
}

#[test]
fn v_actual_counts_distinct_visible_sessions_per_frame() {
    let mut m = Model::new(cfg(), 0);
    m.on_frame(frame(
        SourceId::Wgc,
        1,
        10,
        vec![
            obs(MarkerKind::Output, 0, 1),
            obs(MarkerKind::Output, 1, 1),
            obs(MarkerKind::Echo, 1, 0),
            obs(MarkerKind::Output, 2, 1),
        ],
    ));
    m.on_frame(frame(
        SourceId::Wgc,
        2,
        20,
        vec![obs(MarkerKind::Output, 0, 2)],
    ));
    let (_, s) = m.finish(1_000 * MS);
    assert_eq!(s.v_actual_max, Some(3));
    assert_eq!(s.sessions_seen, vec![0, 1, 2]);
}

#[test]
fn what_was_never_measured_is_null_not_zero() {
    let m = Model::new(cfg(), 0);
    let mut m = m;
    let (_, s) = m.finish(1_000 * MS);
    assert_eq!(s.v_actual_max, None, "no frames: no V");
    assert_eq!(s.v_actual_p50, None);
    assert_eq!(s.crashes, None, "no host group was sampled");
}

fn whole_host() -> GroupSample {
    GroupSample {
        pids: vec![4],
        cpu_pct: Some(10.0),
        memory_mb: Some(50.0),
        ..GroupSample::default()
    }
}

fn empty_host() -> GroupSample {
    GroupSample {
        note: Some(crate::sampler::NOTE_EMPTY),
        ..GroupSample::default()
    }
}

fn unreadable_host() -> GroupSample {
    GroupSample {
        pids: vec![4],
        unreadable: 1,
        note: Some(crate::sampler::NOTE_UNREADABLE),
        ..GroupSample::default()
    }
}

fn sample_at(
    t_ms: u64,
    host: Option<GroupSample>,
    exited: Vec<crate::records::ExitedProcess>,
) -> crate::records::SampleRecord {
    let mut groups = std::collections::BTreeMap::new();
    if let Some(h) = host {
        groups.insert(crate::sampler::GROUP_HOST, h);
    }
    crate::records::SampleRecord {
        t_ns: Ns(t_ms * MS),
        dt_ms: Some(500.0),
        groups,
        gpu: "measured".into(),
        exited,
    }
}

fn host_sample(
    exited: Vec<crate::records::ExitedProcess>,
    empty: bool,
) -> crate::records::SampleRecord {
    sample_at(
        0,
        Some(if empty { empty_host() } else { whole_host() }),
        exited,
    )
}

fn exited(pid: u32, group: &'static str, session: Option<u16>) -> crate::records::ExitedProcess {
    crate::records::ExitedProcess {
        pid,
        group,
        name: format!("pid-{pid}"),
        session,
    }
}

#[test]
fn host_process_exits_count_as_crashes() {
    let mut m = Model::new(cfg(), 0);
    m.on_process_sample(&host_sample(
        vec![exited(4, "host", None), exited(5, "workload", None)],
        false,
    ));
    let (_, s) = m.finish(1_000 * MS);
    assert_eq!(s.crashes, Some(1));
}

#[test]
fn memory_at_the_end_is_null_unless_the_last_sample_was_whole() {
    let end = |seq: Vec<GroupSample>| {
        let mut m = Model::new(cfg(), 0);
        for (i, g) in seq.into_iter().enumerate() {
            m.on_process_sample(&sample_at(i as u64 * 500, Some(g), vec![]));
        }
        let (_, s) = m.finish(200_000 * MS);
        s.processes["host"].memory_mb_end
    };
    // The orchestrator passed a launcher's PID (wt.exe hands off and exits):
    // one whole sample of the launcher, then nothing.
    let mut launcher = vec![whole_host()];
    launcher.extend((0..29).map(|_| empty_host()));
    assert_eq!(
        end(launcher),
        None,
        "the launcher's few MB are not the host's end-of-soak memory"
    );
    // A host that crashed at minute 1 of 2.
    let mut crashed: Vec<GroupSample> = (0..120).map(|_| whole_host()).collect();
    crashed.extend((0..120).map(|_| empty_host()));
    assert_eq!(
        end(crashed),
        None,
        "a pre-crash reading is not the end value"
    );
    // The last sample could not be read whole.
    assert_eq!(
        end(vec![whole_host(), whole_host(), unreadable_host()]),
        None
    );
    // A whole last sample is the end value, whatever came before it.
    assert_eq!(
        end(vec![empty_host(), unreadable_host(), whole_host()]),
        Some(50.0)
    );
}

#[test]
fn a_generator_killed_without_an_exit_record_is_not_a_host_hang() {
    // The generator is killed at 5 s and writes no `exit` record, so the
    // window (rightly) stops updating. Its schedule alone would keep marker
    // 50 overdue to the end of the run — a "hang" — until the sampler sees
    // its process gone (5.2 s) and names its session from the slot file.
    let mut m = Model::new(cfg(), 0);
    m.on_gen_start(GenSchedule {
        session: 0,
        t0_ns: 0,
        marker_hz: 10.0,
        load: true,
    });
    let tl = Timeline {
        emit: |t| t < 5_000,
        frozen: |_| false,
        sample: good_sample,
        lag_ms: Some(0),
    };
    let mut out = drive(&mut m, 0, 5_200, &tl);
    m.on_process_sample(&sample_at(
        5_200,
        None,
        vec![exited(103, "workload", Some(0))],
    ));
    out.extend(drive(&mut m, 5_200, 20_000, &tl));
    let (rest, s) = m.finish(20_000 * MS);
    out.extend(rest);
    assert!(hang_records(&out).is_empty(), "{:?}", hang_records(&out));
    assert_eq!(s.hangs.value, Some(0.0), "{:?}", s.hangs);
    // Another workload process exiting (a shell) ends no schedule.
    let mut m = Model::new(cfg(), 0);
    m.on_gen_start(GenSchedule {
        session: 0,
        t0_ns: 0,
        marker_hz: 10.0,
        load: true,
    });
    let mut out = drive(&mut m, 0, 5_200, &tl);
    m.on_process_sample(&sample_at(5_200, None, vec![exited(102, "workload", None)]));
    out.extend(drive(&mut m, 5_200, 20_000, &tl));
    out.extend(m.finish(20_000 * MS).0);
    assert_eq!(hang_records(&out).len(), 1, "the generator is still due");
}

#[test]
fn a_generator_that_exits_behind_schedule_leaves_nothing_pending() {
    // A slow host: the generator's writes ran late, and when the stop file
    // came (5 s) it had written only through marker 40 (due 4.0 s) and
    // exited. The host showed everything it was given; markers 41.. were
    // never written, so nothing is pending after the exit.
    let mut m = Model::new(cfg(), 0);
    m.on_gen_start(GenSchedule {
        session: 0,
        t0_ns: 0,
        marker_hz: 10.0,
        load: true,
    });
    m.on_gen_exit(0, 5_000 * MS);
    let tl = Timeline {
        emit: |t| t <= 4_000,
        frozen: |_| false,
        sample: good_sample,
        lag_ms: Some(0),
    };
    let mut out = drive(&mut m, 0, 20_000, &tl);
    out.extend(m.finish(20_000 * MS).0);
    assert!(hang_records(&out).is_empty(), "{:?}", hang_records(&out));
}

#[test]
fn a_probe_that_cannot_place_its_frames_in_time_has_not_shown_it_kept_up() {
    // Every frame's presentation time unknown — an X server clock the probe
    // could not establish.
    let mut m = Model::new(cfg(), 0);
    let tl = Timeline {
        emit: always,
        frozen: |_| false,
        sample: good_sample,
        lag_ms: None,
    };
    let out = drive(&mut m, 0, 20_000, &tl);
    let (_, s) = m.finish(20_000 * MS);
    assert_eq!(s.probe.behind, None, "{:?}", s.probe);
    assert!(s.probe.frames > 100, "{:?}", s.probe);
    assert_eq!(s.probe.frames_lag_unknown, s.probe.frames);
    assert_eq!(s.probe.lag_ms_max, None, "unknown, not 0");
    for k in [
        &s.output_latency_ms,
        &s.frame_interval_ms,
        &s.marker_loss,
        &s.hangs,
    ] {
        assert_eq!(k.state, KpiState::Unknown, "{k:?}");
        assert_eq!(k.reason, Some("harness_error"), "{k:?}");
    }
    let frame = out
        .iter()
        .find(|r| matches!(r, Record::Frame(_)))
        .expect("a frame record");
    assert!(
        crate::records::to_line(frame).contains("\"lag_ms\":null"),
        "{}",
        crate::records::to_line(frame)
    );
}

#[test]
fn lag_is_judged_over_the_frames_whose_presentation_is_known() {
    // X11's first frame has no damage behind it; every later one does.
    let mut m = Model::new(cfg(), 0);
    let mut first = frame(SourceId::Wgc, 0, 1, vec![]);
    first.lag_ns = None;
    m.on_frame(first);
    let tl = Timeline {
        emit: always,
        frozen: |_| false,
        sample: good_sample,
        lag_ms: Some(5),
    };
    drive(&mut m, 0, 20_000, &tl);
    let (_, s) = m.finish(20_000 * MS);
    assert_eq!(s.probe.frames_lag_unknown, 1);
    assert_eq!(s.probe.behind, Some(false), "{:?}", s.probe);
    assert_eq!(s.probe.lag_ms_max, Some(5.0));
    assert_eq!(s.output_latency_ms.state, KpiState::Measured);
}

#[test]
fn an_empty_host_group_is_unknown_not_zero() {
    let mut m = Model::new(cfg(), 0);
    for _ in 0..30 {
        m.on_process_sample(&host_sample(vec![], true));
    }
    let (_, s) = m.finish(1_000 * MS);
    let host = &s.processes["host"];
    assert_eq!(host.cpu_pct.state, KpiState::Unknown);
    assert_eq!(
        host.cpu_pct.reason,
        Some("harness_error"),
        "{:?}",
        host.cpu_pct
    );
    assert_eq!(host.memory_mb_end, None);
    assert_eq!(host.samples_empty, 30);
}

/// A model fed `lags_ms` as its primary frames' lags, finished.
fn health_of(lags_ms: &[u64]) -> crate::records::ProbeHealth {
    let mut m = Model::new(cfg(), 0);
    for (i, &lag) in lags_ms.iter().enumerate() {
        let mut f = frame(SourceId::Wgc, i as u64, 10 + i as u64 * 17, vec![]);
        f.lag_ns = Some(lag * MS);
        m.on_frame(f);
    }
    m.finish(10_000 * MS).1.probe
}

#[test]
fn one_late_frame_does_not_make_the_probe_behind() {
    // A selftest's ~100 frames, one of them 300 ms late (a scheduler hiccup,
    // a slow staging copy): judged on the p95, the probe kept up.
    let mut lags = vec![4u64; 99];
    lags.push(300);
    let h = health_of(&lags);
    assert_eq!(h.behind, Some(false), "{h:?}");
    assert_eq!(h.lag_statistic, Some("p95"));
    assert_eq!((h.frames_over_bound, h.lag_ms_max), (1, Some(300.0)));
    // Six late frames in a hundred are past the p95: behind.
    let mut lags = vec![4u64; 94];
    lags.extend([300; 6]);
    let h = health_of(&lags);
    assert_eq!(h.behind, Some(true), "{h:?}");
    assert_eq!(h.frames_over_bound, 6);
}

#[test]
fn from_ten_to_twenty_frames_one_outlier_is_tolerated_and_below_ten_there_is_no_verdict() {
    let mut lags = vec![4u64; 9];
    lags.push(300);
    let h = health_of(&lags);
    assert_eq!(h.lag_statistic, Some("second_largest"));
    assert_eq!(h.behind, Some(false), "{h:?}");
    lags[0] = 200;
    let h = health_of(&lags);
    assert_eq!(h.behind, Some(true), "{h:?}");
    assert_eq!(h.lag_ms_judged, Some(200.0));
    // Below ten frames with a known lag there is no verdict at all: one
    // outlier in two would be half the frames late.
    for lags in [&[4u64, 300][..], &[80], &[4; 9]] {
        let h = health_of(lags);
        assert_eq!((h.lag_statistic, h.behind), (None, None), "{lags:?}: {h:?}");
    }
}
