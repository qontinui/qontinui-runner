//! The model, event by event: every verdict the probe can reach, reached from
//! a hand-built timeline.

use super::*;
use crate::raster::Rect;
use crate::records::Record;
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

fn frame(source: SourceId, index: u64, t_ms: u64, markers: Vec<MarkerObs>) -> FrameObs {
    FrameObs {
        source,
        index,
        t_ns: t_ms * MS,
        width: 800,
        height: 600,
        markers,
        rejected: 0,
        missed: 0,
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

/// Drive a timeline: a generator on session 0 emitting at 10 Hz from 0 to
/// `end_ms`, the window showing each marker 12 ms later unless `frozen(t)`,
/// good window samples every 100 ms (or `sample(t)`), and ticks.
fn run(
    end_ms: u64,
    frozen: impl Fn(u64) -> bool,
    sample: impl Fn(u64) -> WindowSample,
    cfg: ModelConfig,
) -> (Model, Vec<Record>, WatchSummary) {
    let mut m = Model::new(cfg, 0);
    let mut out = Vec::new();
    let mut index = 0;
    let mut current: Option<u32> = None;
    for t_ms in 0..end_ms {
        if t_ms % 100 == 0 {
            m.on_emitted(emitted(0, t_ms / 100, t_ms));
            current = Some((t_ms / 100) as u32);
        }
        if t_ms % 100 == 12 && !frozen(t_ms) {
            if let Some(seq) = current {
                index += 1;
                out.extend(m.on_frame(frame(
                    SourceId::Wgc,
                    index,
                    t_ms,
                    vec![obs(MarkerKind::Output, 0, seq)],
                )));
            }
        }
        if t_ms % 100 == 50 {
            m.on_window_sample(&sample(t_ms * MS));
        }
        if t_ms % 50 == 0 {
            out.extend(m.tick(t_ms * MS));
        }
    }
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
    assert_eq!(s.v_actual_max, 1);
    assert_eq!(s.sessions_seen, vec![0]);
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
fn a_shorter_freeze_is_not_a_hang() {
    let (_, out, _) = run(20_000, |t| (5_000..6_500).contains(&t), good_sample, cfg());
    assert!(hang_records(&out).is_empty());
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
fn a_short_clean_run_is_insufficient_not_occluded() {
    let mut c = cfg();
    c.min_valid_intervals = 60;
    let (_, _, s) = run(5_000, |_| false, good_sample, c);
    assert_eq!(s.hangs.reason, Some("insufficient_samples"));
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

#[test]
fn agreeing_sources_keep_the_primary_latency() {
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
        m.on_frame(frame(
            SourceId::Dda,
            u64::from(seq),
            t + 18,
            vec![obs(MarkerKind::Output, 0, seq)],
        ));
    }
    let (_, s) = m.finish(15_100 * MS);
    assert_eq!(s.crosscheck.verdict, "agree");
    // The primary's 10 ms, not an average with the secondary's 18 ms.
    assert_eq!(s.output_latency_ms.stat("p50"), Some(10.0));
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
    assert_eq!(s.v_actual_max, 3);
    assert_eq!(s.sessions_seen, vec![0, 1, 2]);
}

#[test]
fn host_process_exits_count_as_crashes() {
    let mut m = Model::new(cfg(), 0);
    m.on_process_sample(&crate::records::SampleRecord {
        t_ns: Ns(0),
        dt_ms: 500.0,
        groups: Default::default(),
        gpu: "measured".into(),
        exited: vec![
            crate::records::ExitedProcess {
                pid: 4,
                group: "host",
                name: "host.exe".into(),
            },
            crate::records::ExitedProcess {
                pid: 5,
                group: "workload",
                name: "node.exe".into(),
            },
        ],
    });
    let (_, s) = m.finish(1_000 * MS);
    assert_eq!(s.crashes, 1);
}
