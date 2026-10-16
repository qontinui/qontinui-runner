//! What a run's observations mean: latencies, marker loss vs coalescing, and
//! the agreement of two capture sources.

use crate::codec::{marker_seq, Marker, MarkerKind};
use crate::records::{CrosscheckRecord, CrosscheckSummary, Ns};

use super::Emitted;

/// When and where a source first saw a marker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirstSeen {
    pub t_ns: u64,
    pub frame: u64,
    /// Frames the source coalesced into that frame (DXGI accumulation);
    /// `None` for a source that cannot tell.
    pub missed: Option<u32>,
}

/// The marker an emitted record refers to.
pub fn marker_of(e: &Emitted) -> Marker {
    Marker {
        kind: e.kind,
        session: e.session,
        seq: marker_seq(e.seq_full),
    }
}

/// Latencies (ms) from emission to first appearance, for every emitted marker
/// that appeared. A first appearance BEFORE the emission is impossible on a
/// shared timebase; such samples are counted as anomalies and excluded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Latencies {
    pub samples_ms: Vec<f64>,
    pub negative: u64,
}

pub fn latencies<'a>(
    emitted: impl IntoIterator<Item = &'a Emitted>,
    seen: impl Fn(Marker) -> Option<FirstSeen>,
) -> Latencies {
    let mut out = Latencies::default();
    for e in emitted {
        let Some(fs) = seen(marker_of(e)) else {
            continue;
        };
        if fs.t_ns < e.t_emit_ns {
            out.negative += 1;
            continue;
        }
        out.samples_ms.push((fs.t_ns - e.t_emit_ns) as f64 / 1e6);
    }
    out
}

/// The loss/coalescing classification of one session's sequences (catalog
/// `marker_loss` / `marker_skipped`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LossTally {
    /// Sequences current for >= 2 refresh intervals (the loss denominator).
    pub eligible: u64,
    /// Eligible sequences never observed, or observed out of order.
    pub lost: u64,
    pub out_of_order: u64,
    /// Sequences counted at all (the `marker_skipped` denominator).
    pub counted: u64,
    /// Superseded before 2 refresh intervals and never observed (coalescing).
    pub skipped: u64,
    /// Sequences whose lifetime overlapped an excluded interval.
    pub excluded: u64,
}

impl std::ops::AddAssign for LossTally {
    fn add_assign(&mut self, o: LossTally) {
        self.eligible += o.eligible;
        self.lost += o.lost;
        self.out_of_order += o.out_of_order;
        self.counted += o.counted;
        self.skipped += o.skipped;
        self.excluded += o.excluded;
    }
}

/// Classify one session's output sequences (sorted by emission). A sequence
/// is current from its emission until the next one's (the last until
/// `run_end`); `span_excluded(a, b)` says whether `[a, b]` touched an excluded
/// interval.
pub fn loss(
    session_seqs: &[Emitted],
    seen: impl Fn(Marker) -> Option<FirstSeen>,
    refresh_ns: u64,
    run_end: u64,
    span_excluded: impl Fn(u64, u64) -> bool,
) -> LossTally {
    let mut t = LossTally::default();
    let seen_at: Vec<Option<u64>> = session_seqs
        .iter()
        .map(|e| seen(marker_of(e)).map(|f| f.t_ns))
        .collect();
    // Out of order: a LATER sequence was seen before this one.
    let mut suffix_min = vec![u64::MAX; session_seqs.len() + 1];
    for j in (0..session_seqs.len()).rev() {
        suffix_min[j] = suffix_min[j + 1].min(seen_at[j].unwrap_or(u64::MAX));
    }
    for (j, e) in session_seqs.iter().enumerate() {
        let end = session_seqs.get(j + 1).map_or(run_end, |n| n.t_emit_ns);
        if span_excluded(e.t_emit_ns, end) {
            t.excluded += 1;
            continue;
        }
        t.counted += 1;
        let current = end.saturating_sub(e.t_emit_ns);
        let eligible = current >= 2 * refresh_ns;
        let ooo = seen_at[j].is_some_and(|s| suffix_min[j + 1] < s);
        if eligible {
            t.eligible += 1;
            if seen_at[j].is_none() || ooo {
                t.lost += 1;
            }
        } else if seen_at[j].is_none() {
            t.skipped += 1;
        }
        if ooo {
            t.out_of_order += 1;
        }
    }
    t
}

/// The per-marker band reaches this many FRAME intervals (see [`crosscheck`])
/// past either end of the declared offset band. Each source sees a marker first
/// in its own next frame, so a sound marker's gap is the declared offset plus a
/// sampling-phase difference of less than one frame either way.
pub const CROSSCHECK_MARKER_BAND_FRAMES: f64 = 1.0;
/// The median gap may lie at most this many frame intervals outside the
/// declared offset band. A quarter frame: wide enough for the median's own
/// sampling noise, narrow enough that a one-frame systematic lag of either
/// source leaves it wherever a frame is longer than the declared band is wide
/// divided by 0.75 (13.3 ms for the 10 ms WGC offset: any host at or below
/// 75 Hz).
pub const CROSSCHECK_MEDIAN_BAND_FRAMES: f64 = 0.25;
/// The largest share of compared markers that may lie outside the per-marker
/// band before the run disagrees.
pub const CROSSCHECK_MAX_OUTSIDE_SHARE: f64 = 0.05;
/// The fewest compared markers for which the tail rule decides. Below 20 one
/// marker is already more than 5%, so the share would let a single marker
/// void the run.
pub const CROSSCHECK_TAIL_MIN_COMPARED: u64 = 20;
/// The fewest compared markers for which the median rule decides: from three
/// the median is a middle marker, never an extreme one. Below it the
/// cross-check is `not_performed`.
pub const CROSSCHECK_MEDIAN_MIN_COMPARED: u64 = 3;
/// The frame interval the bands are sized in is the primary's measured
/// frame-spacing p50, clamped to `[1, 2]` refresh intervals. Never finer than
/// the display can present. Never coarser than every other refresh, the
/// slowest compositor cadence measured (the GPU-less hosted VM): WGC delivers
/// a frame only when the window changed, so a sparser spacing measures a
/// window that was idle, not the compositor, and must not open the bands up.
/// A host that truly composes more slowly gets bands that are too tight, so it
/// fails visibly (unknown(capture_disagreement)), never silently.
pub const CROSSCHECK_FRAME_MAX_REFRESHES: f64 = 2.0;

/// Compare two sources' first appearances (plan D2: two independent sources
/// for one timestamp; a capture defect must be visible). The two are never
/// averaged, and a disagreement makes the run's latencies
/// unknown(capture_disagreement).
///
/// Each marker both saw cleanly gives a gap, primary minus secondary. The
/// secondary's time is only an upper bound where it coalesced frames at that
/// marker, so such a marker is not compared when the bound is the later time.
///
/// Units. The bands are sized in FRAME intervals F, not refresh intervals:
/// F is `frame_spacing_ms` (the primary's measured frame-spacing p50) clamped
/// to `[R, 2 R]`, or R when it is unknown. On real hardware F is about R; on
/// the GPU-less hosted VM the compositor presents every other refresh, so F is
/// 2 R (CI run 38362046020: WGC frame spacing p50 31.25 ms at R = 15.625 ms).
///
/// The declared band: WGC's `SystemRelativeTime` stamps the compositor's
/// render of the capture frame, DDA's `LastPresentTime` the desktop
/// presentation, so WGC may be up to `expected_offset_ms` later by design
/// (`selftest::WGC_RENDER_OFFSET_MS`). The declared band is
/// `[0, expected_offset_ms]` (or `[expected_offset_ms, 0]` for a negative one).
///
/// What the rule catches — and what it does not:
/// - a SYSTEMATIC offset: the median gap (of an even count, the LOWER of the
///   two middle gaps) lies more than F/4 outside the declared band. That is a
///   time-base or unit bug, or either source lagging by a whole frame on every
///   marker — the latter only where 0.75 F exceeds the declared band's width
///   (F > 13.3 ms for 10 ms: up to 75 Hz). At 144 Hz a one-frame lag (6.9 ms)
///   is smaller than the declared WGC offset itself and is not caught by the
///   median; the tail rule may still catch it;
/// - the PRIMARY missing frames, or markers decoded from the wrong frame, on
///   more than 5% of markers: more than 5% of compared markers lie more than
///   one frame interval outside the declared band. Applied only from 20
///   compared markers ([`CROSSCHECK_TAIL_MIN_COMPARED`]); below that the
///   verdict rests on the median rule alone (a systematic defect moves every
///   marker, so the median still shows it), and the summary says the tail rule
///   was not applied. A secondary that coalesced frames is not a miss here: its
///   marker is skipped (above);
/// - below three compared markers neither rule decides: `not_performed`;
/// - it does NOT catch a primary that misses an occasional frame (fewer than
///   5% of markers), nor a misattribution confined to so few markers.
///
/// Every marker outside the per-marker band is a `crosscheck` record with
/// verdict `outside_band`, whatever the run's verdict.
///
/// Until CI run 38362046020 the rule was "any one marker more than one refresh
/// interval apart voids the run"; there 22-104 of 206-698 markers per watch
/// (11-15%) were more than one refresh interval apart — 359 of those 361 with
/// WGC later, and none of the 359 with a WGC frame between the two times —
/// while the median gap was 6.5-8.9 ms and 0.4-1.3% lay more than one frame
/// (31.25 ms) outside the declared band.
///
/// `expected_offset_ms` is the primary's declared stage offset minus the
/// secondary's.
#[allow(clippy::too_many_arguments)]
pub fn crosscheck(
    markers_primary: &[(Marker, FirstSeen)],
    secondary_seen: impl Fn(Marker) -> Option<FirstSeen>,
    secondary_markers: &[(Marker, FirstSeen)],
    primary_seen: impl Fn(Marker) -> Option<FirstSeen>,
    refresh_ns: u64,
    frame_spacing_ms: Option<f64>,
    expected_offset_ms: f64,
    secondary_configured: bool,
) -> (CrosscheckSummary, Vec<CrosscheckRecord>) {
    let mut s = CrosscheckSummary {
        verdict: "not_performed",
        expected_offset_ms,
        ..Default::default()
    };
    let mut records = Vec::new();
    if !secondary_configured {
        s.detail = "no secondary capture source in this run".into();
        return (s, records);
    }
    let refresh_ms = refresh_ns as f64 / 1e6;
    let frame_ms = frame_spacing_ms
        .filter(|f| f.is_finite() && *f > 0.0)
        .unwrap_or(refresh_ms)
        .clamp(refresh_ms, CROSSCHECK_FRAME_MAX_REFRESHES * refresh_ms);
    s.frame_ms = Some(frame_ms);
    let (lo, hi) = (expected_offset_ms.min(0.0), expected_offset_ms.max(0.0));
    let marker_band = (
        lo - CROSSCHECK_MARKER_BAND_FRAMES * frame_ms,
        hi + CROSSCHECK_MARKER_BAND_FRAMES * frame_ms,
    );
    let median_band = (
        lo - CROSSCHECK_MEDIAN_BAND_FRAMES * frame_ms,
        hi + CROSSCHECK_MEDIAN_BAND_FRAMES * frame_ms,
    );
    s.marker_band_ms = Some(marker_band);
    s.median_band_ms = Some(median_band);
    let mut gaps: Vec<f64> = Vec::new();
    for &(m, p) in markers_primary {
        if m.kind == MarkerKind::Calibration {
            continue;
        }
        let Some(q) = secondary_seen(m) else {
            s.secondary_unseen += 1;
            continue;
        };
        if q.missed.is_some_and(|m| m > 0) && q.t_ns > p.t_ns {
            s.secondary_coalesced += 1;
            continue;
        }
        s.compared += 1;
        // Records keep the secondary-minus-primary sign they always had.
        let delta = (q.t_ns as i128 - p.t_ns as i128) as f64 / 1e6;
        let gap = -delta;
        gaps.push(gap);
        s.max_abs_delta_ms = Some(
            s.max_abs_delta_ms
                .map_or(delta.abs(), |m| m.max(delta.abs())),
        );
        if (marker_band.0..=marker_band.1).contains(&gap) {
            s.agreed += 1;
        } else {
            s.disagreed += 1;
            records.push(CrosscheckRecord {
                kind: m.kind,
                session: m.session,
                seq: m.seq,
                t_primary_ns: Ns(p.t_ns),
                t_secondary_ns: Ns(q.t_ns),
                delta_ms: delta,
                refresh_ms,
                frame_ms,
                verdict: "outside_band",
            });
        }
    }
    s.primary_unseen = secondary_markers
        .iter()
        .filter(|(m, _)| m.kind != MarkerKind::Calibration && primary_seen(*m).is_none())
        .count() as u64;
    if gaps.is_empty() {
        s.detail = "the secondary source saw none of the primary's markers cleanly".into();
        return (s, records);
    }
    gaps.sort_by(f64::total_cmp);
    // Nearest rank: of an even count, the lower of the two middle gaps.
    let median = gaps[crate::stats::nearest_rank(50, gaps.len()) - 1];
    let outside_share = s.disagreed as f64 / s.compared as f64;
    s.median_gap_ms = Some(median);
    s.outside_share = Some(outside_share);
    s.tail_rule_applied = s.compared >= CROSSCHECK_TAIL_MIN_COMPARED;
    if s.compared < CROSSCHECK_MEDIAN_MIN_COMPARED {
        s.detail = format!(
            "only {} marker(s) seen cleanly by both sources; the median rule needs {CROSSCHECK_MEDIAN_MIN_COMPARED}, so neither rule decides",
            s.compared
        );
        return (s, records);
    }
    let systematic = !(median_band.0..=median_band.1).contains(&median);
    let missed = s.tail_rule_applied && outside_share > CROSSCHECK_MAX_OUTSIDE_SHARE;
    s.verdict = if systematic || missed {
        "disagree"
    } else {
        "agree"
    };
    let pct = 100.0 * outside_share;
    let max_pct = 100.0 * CROSSCHECK_MAX_OUTSIDE_SHARE;
    let median_text = format!(
        "the median primary-minus-secondary gap over {} markers is {median:.2} ms (band [{:.2}, {:.2}] ms: the declared {expected_offset_ms:.1} ms stage offset +/- a quarter frame, frame {frame_ms:.2} ms)",
        s.compared, median_band.0, median_band.1
    );
    let tail_text = if s.tail_rule_applied {
        format!(
            "{} of {} markers ({pct:.1}%) lie outside [{:.2}, {:.2}] ms (+/- one frame; at most {max_pct:.0}% may)",
            s.disagreed, s.compared, marker_band.0, marker_band.1
        )
    } else {
        format!(
            "{} of {} markers lie outside [{:.2}, {:.2}] ms (+/- one frame); the tail rule needs {CROSSCHECK_TAIL_MIN_COMPARED} markers and was not applied",
            s.disagreed, s.compared, marker_band.0, marker_band.1
        )
    };
    s.detail = match (systematic, missed) {
        (true, true) => {
            format!("systematic offset and a missed-frame tail: {median_text}; {tail_text}")
        }
        (true, false) => {
            format!("systematic offset between the sources: {median_text}; {tail_text}")
        }
        (false, true) => {
            format!(
                "the primary missed frames or misattributed markers: {tail_text}; {median_text}"
            )
        }
        (false, false) => format!("agreed: {median_text}; {tail_text}"),
    };
    (s, records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EmitSource;
    use std::collections::HashMap;

    const MS: u64 = 1_000_000;

    fn em(session: u16, seq: u64, t_ms: u64) -> Emitted {
        Emitted {
            session,
            kind: MarkerKind::Output,
            seq_full: seq,
            t_emit_ns: t_ms * MS,
            source: EmitSource::Generator,
        }
    }

    fn fs(t_ms: u64) -> FirstSeen {
        FirstSeen {
            t_ns: t_ms * MS,
            frame: 0,
            missed: None,
        }
    }

    #[test]
    fn latency_is_first_appearance_minus_emission_and_negatives_are_anomalies() {
        let emitted = [em(0, 0, 100), em(0, 1, 200), em(0, 2, 300)];
        let mut seen = HashMap::new();
        seen.insert(marker_of(&emitted[0]), fs(112));
        seen.insert(marker_of(&emitted[1]), fs(190)); // before emission
        let l = latencies(&emitted, |m| seen.get(&m).copied());
        assert_eq!(l.samples_ms, vec![12.0]);
        assert_eq!(l.negative, 1);
    }

    #[test]
    fn loss_skip_and_out_of_order_are_told_apart() {
        let refresh = 16 * MS;
        // seq 0 current 100 ms, seen; seq 1 current 10 ms, never seen (coalesced);
        // seq 2 current 100 ms, never seen (LOST); seq 3 seen AFTER seq 4 (out
        // of order); seq 4 seen.
        let seqs = [
            em(0, 0, 0),
            em(0, 1, 100),
            em(0, 2, 110),
            em(0, 3, 210),
            em(0, 4, 310),
        ];
        let mut seen = HashMap::new();
        seen.insert(marker_of(&seqs[0]), fs(10));
        seen.insert(marker_of(&seqs[3]), fs(330));
        seen.insert(marker_of(&seqs[4]), fs(320));
        let t = loss(
            &seqs,
            |m| seen.get(&m).copied(),
            refresh,
            500 * MS,
            |_, _| false,
        );
        assert_eq!(t.counted, 5);
        assert_eq!(t.eligible, 4);
        assert_eq!(t.skipped, 1);
        assert_eq!(t.out_of_order, 1);
        assert_eq!(t.lost, 2, "seq 2 never seen + seq 3 out of order");
    }

    #[test]
    fn excluded_spans_are_not_counted() {
        let seqs = [em(0, 0, 0), em(0, 1, 100)];
        let t = loss(&seqs, |_| None, 16 * MS, 200 * MS, |a, _| a >= 100 * MS);
        assert_eq!(t.excluded, 1);
        assert_eq!(t.counted, 1);
        assert_eq!(t.lost, 1);
    }

    const R64: u64 = 15_625_000;
    const R60: u64 = 16_666_667;
    const R60_MS: f64 = 16.666_667;

    /// Run `crosscheck` with one output marker per gap (primary minus
    /// secondary, ms): the primary at `1 s + 100 ms * i`, the secondary `gap`
    /// earlier. `frame_ms` is the primary's measured frame spacing.
    fn run(
        gaps: &[f64],
        refresh_ns: u64,
        frame_ms: Option<f64>,
        offset_ms: f64,
    ) -> (CrosscheckSummary, Vec<CrosscheckRecord>) {
        let m = |seq: usize| Marker::new(MarkerKind::Output, 1, seq as u32);
        let at = |i: usize| 1_000 * MS + 100 * MS * i as u64;
        let first = |t_ns: u64| FirstSeen {
            t_ns,
            frame: 0,
            missed: None,
        };
        let primary: Vec<(Marker, FirstSeen)> =
            (0..gaps.len()).map(|i| (m(i), first(at(i)))).collect();
        let sec: HashMap<Marker, FirstSeen> = gaps
            .iter()
            .enumerate()
            .map(|(i, g)| (m(i), first((at(i) as f64 - g * 1e6).round() as u64)))
            .collect();
        let sec_list: Vec<(Marker, FirstSeen)> = sec.iter().map(|(k, v)| (*k, *v)).collect();
        let primary_map: HashMap<Marker, FirstSeen> = primary.iter().copied().collect();
        crosscheck(
            &primary,
            |k| sec.get(&k).copied(),
            &sec_list,
            |k| primary_map.get(&k).copied(),
            refresh_ns,
            frame_ms,
            offset_ms,
            true,
        )
    }

    /// A sound host presenting every refresh at 60 Hz: the declared offset
    /// `offset` plus a sampling-phase difference of up to half a frame either
    /// way, spread evenly over 400 markers.
    fn healthy_60(offset: f64) -> Vec<f64> {
        (0..400)
            .map(|i| offset + ((i % 41) as f64 / 40.0 - 0.5) * R60_MS)
            .collect()
    }

    /// CI run 38362046020's focus soak, rebuilt from its replay histogram
    /// (WGC minus DDA, in 15.625 ms buckets): 16 gaps in (-15.6, 0], 393 in
    /// (0, 15.6], 49 in (15.6, 31.3], 4 in (31.3, 46.9], one each in
    /// (46.9, 62.5], (62.5, 78.1] and (109.4, 125.0] — each bucket's gaps
    /// spread evenly over it (each bucket's top included). Median 8.6 ms (the
    /// replay's: 8.86), 5 of 465 outside +/- one 31.25 ms frame (the replay's:
    /// 6).
    fn vm_focus_soak() -> Vec<f64> {
        let r = 15.625;
        let mut g = Vec::new();
        for (bucket, count) in [
            (-1i32, 16usize),
            (0, 393),
            (1, 49),
            (2, 4),
            (3, 1),
            (4, 1),
            (7, 1),
        ] {
            for k in 1..=count {
                g.push(r * (f64::from(bucket) + k as f64 / count as f64));
            }
        }
        g
    }

    #[test]
    fn crosscheck_never_averages_and_skips_a_coalesced_upper_bound() {
        let m = |seq| Marker::new(MarkerKind::Output, 1, seq);
        let primary = vec![
            (m(1), fs(100)),
            (m(2), fs(200)),
            (m(3), fs(300)),
            (m(4), fs(400)),
        ];
        let mut sec = HashMap::new();
        sec.insert(m(1), fs(95)); // gap +5 ms: inside
        sec.insert(m(2), fs(240)); // gap -40 ms: outside +/- one frame
        sec.insert(m(4), fs(393)); // gap +7 ms: inside
        sec.insert(
            m(3),
            FirstSeen {
                t_ns: 350 * MS,
                frame: 9,
                missed: Some(2),
            },
        ); // coalesced: not compared
        let sec_list: Vec<(Marker, FirstSeen)> = sec.iter().map(|(k, v)| (*k, *v)).collect();
        let primary_map: HashMap<Marker, FirstSeen> = primary.iter().copied().collect();
        let (s, recs) = crosscheck(
            &primary,
            |k| sec.get(&k).copied(),
            &sec_list,
            |k| primary_map.get(&k).copied(),
            R64,
            None,
            10.0,
            true,
        );
        assert_eq!(
            (s.compared, s.agreed, s.disagreed, s.secondary_coalesced),
            (3, 2, 1, 1)
        );
        // Three markers: the median (5 ms) decides; the tail rule does not.
        assert_eq!(s.verdict, "agree", "{}", s.detail);
        assert!(!s.tail_rule_applied);
        assert_eq!(recs.len(), 1, "the outside marker is still recorded");
        assert_eq!(recs[0].t_primary_ns, Ns(200 * MS));
        assert_eq!(recs[0].t_secondary_ns, Ns(240 * MS));
        assert_eq!(
            recs[0].verdict, "outside_band",
            "a record is a marker, not the run's verdict"
        );
    }

    /// The hosted VM's measured shape agrees once the bands are sized in its
    /// measured 31.25 ms frame spacing.
    #[test]
    fn the_hosted_vm_replay_agrees() {
        let g = vm_focus_soak();
        assert_eq!(g.len(), 465);
        let (s, recs) = run(&g, R64, Some(31.25), 10.0);
        assert_eq!(s.verdict, "agree", "{}", s.detail);
        assert_eq!(s.frame_ms, Some(31.25));
        assert_eq!(s.disagreed, 5);
        assert_eq!(recs.len(), 5);
        assert!(
            s.median_gap_ms.is_some_and(|m| (8.0..9.0).contains(&m)),
            "{s:?}"
        );
        // The same data judged in refresh intervals would be voided: the
        // frame spacing is what makes the VM's every-other-refresh honest.
        assert_eq!(run(&g, R64, None, 10.0).0.verdict, "disagree");
    }

    /// Sound hosts agree wherever the declared offset sits in its band.
    #[test]
    fn a_sound_60hz_host_agrees() {
        for offset in [0.0, 5.0, 10.0] {
            let (s, _) = run(&healthy_60(offset), R60, Some(R60_MS), 10.0);
            assert_eq!(s.verdict, "agree", "offset {offset}: {}", s.detail);
        }
    }

    /// Review S1: on a host presenting every refresh, the primary one frame
    /// late on EVERY marker disagrees, wherever the true offset sits.
    #[test]
    fn a_60hz_wgc_one_frame_late_disagrees() {
        for offset in [0.0, 5.0, 7.0, 10.0] {
            let g: Vec<f64> = healthy_60(offset).iter().map(|g| g + R60_MS).collect();
            let (s, _) = run(&g, R60, Some(R60_MS), 10.0);
            assert_eq!(s.verdict, "disagree", "offset {offset}: {}", s.detail);
            assert!(s.detail.starts_with("systematic offset"), "{}", s.detail);
        }
    }

    /// Review S1: the secondary one frame late on every marker disagrees too.
    #[test]
    fn a_60hz_dda_one_frame_late_disagrees() {
        for offset in [0.0, 5.0, 7.0, 10.0] {
            let g: Vec<f64> = healthy_60(offset).iter().map(|g| g - R60_MS).collect();
            let (s, _) = run(&g, R60, Some(R60_MS), 10.0);
            assert_eq!(s.verdict, "disagree", "offset {offset}: {}", s.detail);
        }
    }

    /// Review S1: 40% of markers one WGC frame late at 60 Hz.
    #[test]
    fn a_60hz_wgc_late_on_40_percent_of_markers_disagrees() {
        let g: Vec<f64> = healthy_60(7.0)
            .iter()
            .enumerate()
            .map(|(i, g)| if i % 5 < 2 { g + R60_MS } else { *g })
            .collect();
        let (s, _) = run(&g, R60, Some(R60_MS), 10.0);
        assert_eq!(s.verdict, "disagree", "{}", s.detail);
    }

    /// A time-base or unit bug moves every marker: the median leaves its band
    /// even though no single marker leaves the per-marker band.
    #[test]
    fn a_systematic_offset_disagrees() {
        // Secondary 15 ms later than the primary on every marker at 64 Hz:
        // inside the per-marker band [-15.63, 25.63], outside the median band
        // [-3.91, 13.91].
        let (s, _) = run(&[-15.0; 200], R64, Some(15.625), 10.0);
        assert_eq!(s.verdict, "disagree", "{}", s.detail);
        assert_eq!(s.disagreed, 0);
        assert!(s.detail.starts_with("systematic offset"), "{}", s.detail);
        // Primary 20 ms later than the declared offset allows.
        assert_eq!(
            run(&[20.0; 200], R64, Some(15.625), 10.0).0.verdict,
            "disagree"
        );
        // With no declared offset a constant 10 ms is systematic too.
        assert_eq!(
            run(&[10.0; 200], R64, Some(15.625), 0.0).0.verdict,
            "disagree"
        );
    }

    /// The primary missing frames sees some markers whole frames late: past 5%
    /// of markers outside the band the run disagrees, with a sound median.
    #[test]
    fn the_primary_missing_frames_disagrees() {
        let tail = |every: usize| -> Vec<f64> {
            (0..300)
                .map(|i| if i % every == 0 { 55.0 } else { 5.0 })
                .collect()
        };
        let (s, _) = run(&tail(10), R64, Some(15.625), 10.0);
        assert_eq!(s.verdict, "disagree", "{}", s.detail);
        assert_eq!(s.median_gap_ms, Some(5.0));
        assert!(
            s.detail.starts_with("the primary missed frames"),
            "{}",
            s.detail
        );
        // At 5% exactly it still agrees; the bound is "more than".
        assert_eq!(run(&tail(20), R64, Some(15.625), 10.0).0.verdict, "agree");
    }

    /// Review S3: below 20 compared markers the tail rule does not decide.
    #[test]
    fn the_tail_rule_needs_twenty_markers() {
        let with_two_out =
            |n: usize| -> Vec<f64> { (0..n).map(|i| if i < 2 { 60.0 } else { 5.0 }).collect() };
        let (s19, _) = run(&with_two_out(19), R64, Some(15.625), 10.0);
        assert_eq!(s19.verdict, "agree", "{}", s19.detail);
        assert!(!s19.tail_rule_applied);
        assert_eq!(s19.disagreed, 2, "still counted and recorded");
        assert!(s19.detail.contains("was not applied"), "{}", s19.detail);
        let (s20, _) = run(&with_two_out(20), R64, Some(15.625), 10.0);
        assert!(s20.tail_rule_applied);
        assert_eq!(s20.verdict, "disagree", "2 of 20 is 10%: {}", s20.detail);
        // Below 20 the median still catches a systematic offset.
        assert_eq!(
            run(&[30.0; 10], R64, Some(15.625), 10.0).0.verdict,
            "disagree"
        );
    }

    /// Below three markers neither rule decides.
    #[test]
    fn fewer_than_three_markers_is_not_performed() {
        let (s, recs) = run(&[60.0, 60.0], R64, Some(15.625), 10.0);
        assert_eq!(s.verdict, "not_performed", "{}", s.detail);
        assert_eq!(recs.len(), 2);
        assert_eq!(
            run(&[60.0; 3], R64, Some(15.625), 10.0).0.verdict,
            "disagree"
        );
    }

    /// Review N1: of an even count, the median is the lower middle gap.
    #[test]
    fn the_median_of_an_even_count_is_the_lower_middle() {
        let (s, _) = run(&[0.0, 5.0, 24.0, 40.0], R64, Some(15.625), 10.0);
        assert_eq!(s.median_gap_ms, Some(5.0));
        assert_eq!(s.verdict, "agree", "{}", s.detail);
    }

    /// The frame interval is clamped to one to two refresh intervals.
    #[test]
    fn the_frame_interval_is_clamped() {
        let g = [5.0; 30];
        assert_eq!(run(&g, R64, Some(1.0), 10.0).0.frame_ms, Some(15.625));
        assert_eq!(run(&g, R64, Some(500.0), 10.0).0.frame_ms, Some(31.25));
        assert_eq!(run(&g, R64, None, 10.0).0.frame_ms, Some(15.625));
        assert_eq!(run(&g, R64, Some(f64::NAN), 10.0).0.frame_ms, Some(15.625));
    }

    #[test]
    fn crosscheck_without_a_secondary_is_not_performed() {
        let (s, _) = crosscheck(&[], |_| None, &[], |_| None, 16 * MS, None, 0.0, false);
        assert_eq!(s.verdict, "not_performed");
    }

    #[test]
    fn a_marker_only_the_secondary_saw_is_primary_unseen() {
        let m = Marker::new(MarkerKind::Output, 2, 5);
        let sec = vec![(m, fs(50))];
        let (s, _) = crosscheck(&[], |_| None, &sec, |_| None, 16 * MS, None, 0.0, true);
        assert_eq!(s.primary_unseen, 1);
        assert_eq!(s.verdict, "not_performed");
    }
}
