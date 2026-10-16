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

/// Per-marker band, in refresh intervals either side of the two sources'
/// declared offset: a marker whose primary-minus-secondary gap lies outside it
/// is counted as disagreeing (and gets a `crosscheck` record).
pub const CROSSCHECK_MARKER_BAND_REFRESHES: f64 = 2.0;
/// The run's median gap must lie within this many refresh intervals of the
/// declared offset band; outside it the two sources are systematically apart.
pub const CROSSCHECK_MEDIAN_BAND_REFRESHES: f64 = 1.0;
/// The largest share of compared markers that may lie outside the per-marker
/// band before the run disagrees.
pub const CROSSCHECK_MAX_OUTSIDE_SHARE: f64 = 0.05;

/// Compare two sources' first appearances (plan D2: two independent sources
/// for one timestamp; a capture defect must be visible). The two are never
/// averaged, and a disagreement makes the run's latencies
/// unknown(capture_disagreement).
///
/// Each marker both saw cleanly gives a gap, primary minus secondary. The
/// secondary's time is only an upper bound where it coalesced frames at that
/// marker, so such a marker is not compared when the bound is the later time.
///
/// What the gaps are held to — the RULE, not any one marker:
/// - the two timestamps stamp different stages: WGC's `SystemRelativeTime` is
///   the compositor's render of the capture frame, DDA's `LastPresentTime` the
///   desktop presentation, so WGC may be up to `expected_offset_ms` later by
///   design (`selftest::WGC_RENDER_OFFSET_MS`). The declared band is
///   `[0, expected_offset_ms]` (or `[expected_offset_ms, 0]` for a negative one);
/// - each source sees a marker first in ITS next frame, so a single gap also
///   carries up to one frame interval of sampling phase. On a GPU-less hosted
///   VM the compositor presents every other refresh (CI run 38362046020: WGC
///   frame spacing median 31.25 ms = 2 x 15.625 ms), so single gaps of one to
///   two refresh intervals are a property of the machine, not a defect;
/// - SYSTEMATIC disagreement (a time-base, unit or attribution bug): the median
///   gap lies more than one refresh interval outside the declared band;
/// - a source MISSING FRAMES (or a marker decoded from the wrong frame): more
///   than 5% of compared markers lie more than two refresh intervals outside
///   the declared band.
///
/// Until CI run 38362046020 the rule was "any one marker more than one refresh
/// interval apart voids the run"; there 22-104 of 206-698 markers per watch
/// (11-15%) were more than one refresh interval apart — 359 of those 361 with
/// WGC later, and none of the 359 with a WGC frame between the two times — while
/// the median gap was 6.5-8.9 ms and 0.4-1.3% lay outside the per-marker
/// band.
///
/// `expected_offset_ms` is the primary's declared stage offset minus the
/// secondary's.
pub fn crosscheck(
    markers_primary: &[(Marker, FirstSeen)],
    secondary_seen: impl Fn(Marker) -> Option<FirstSeen>,
    secondary_markers: &[(Marker, FirstSeen)],
    primary_seen: impl Fn(Marker) -> Option<FirstSeen>,
    refresh_ns: u64,
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
    let (lo, hi) = (expected_offset_ms.min(0.0), expected_offset_ms.max(0.0));
    let marker_band = (
        lo - CROSSCHECK_MARKER_BAND_REFRESHES * refresh_ms,
        hi + CROSSCHECK_MARKER_BAND_REFRESHES * refresh_ms,
    );
    let median_band = (
        lo - CROSSCHECK_MEDIAN_BAND_REFRESHES * refresh_ms,
        hi + CROSSCHECK_MEDIAN_BAND_REFRESHES * refresh_ms,
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
                verdict: "disagree",
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
    let median = gaps[crate::stats::nearest_rank(50, gaps.len()) - 1];
    let outside_share = s.disagreed as f64 / s.compared as f64;
    s.median_gap_ms = Some(median);
    s.outside_share = Some(outside_share);
    let systematic = !(median_band.0..=median_band.1).contains(&median);
    let missed = outside_share > CROSSCHECK_MAX_OUTSIDE_SHARE;
    s.verdict = if systematic || missed {
        "disagree"
    } else {
        "agree"
    };
    let pct = 100.0 * outside_share;
    let max_pct = 100.0 * CROSSCHECK_MAX_OUTSIDE_SHARE;
    let median_text = format!(
        "the median primary-minus-secondary gap over {} markers is {median:.2} ms (band [{:.2}, {:.2}] ms: the declared {expected_offset_ms:.1} ms stage offset +/- one refresh interval, {refresh_ms:.2} ms)",
        s.compared, median_band.0, median_band.1
    );
    let tail_text = format!(
        "{} of {} markers ({pct:.1}%) lie outside [{:.2}, {:.2}] ms (+/- two refresh intervals; at most {max_pct:.0}% may)",
        s.disagreed, s.compared, marker_band.0, marker_band.1
    );
    s.detail = match (systematic, missed) {
        (true, true) => format!("systematic offset and missed frames: {median_text}; {tail_text}"),
        (true, false) => {
            format!("systematic offset between the sources: {median_text}; {tail_text}")
        }
        (false, true) => {
            format!("a source missed frames or misattributed markers: {tail_text}; {median_text}")
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

    /// Run `crosscheck` over output markers 0..n with the primary at
    /// `100 * seq` ms and the secondary at `primary - gap(seq)` ms.
    fn run_gaps(
        n: u32,
        offset_ms: f64,
        gap: impl Fn(u32) -> i64,
    ) -> (CrosscheckSummary, Vec<CrosscheckRecord>) {
        let m = |seq| Marker::new(MarkerKind::Output, 1, seq);
        let primary: Vec<(Marker, FirstSeen)> = (0..n)
            .map(|i| (m(i), fs(1000 + 100 * u64::from(i))))
            .collect();
        let sec: HashMap<Marker, FirstSeen> = (0..n)
            .map(|i| {
                let t = (1000 + 100 * i64::from(i) - gap(i)) as u64;
                (m(i), fs(t))
            })
            .collect();
        let sec_list: Vec<(Marker, FirstSeen)> = sec.iter().map(|(k, v)| (*k, *v)).collect();
        let primary_map: HashMap<Marker, FirstSeen> = primary.iter().copied().collect();
        crosscheck(
            &primary,
            |k| sec.get(&k).copied(),
            &sec_list,
            |k| primary_map.get(&k).copied(),
            R64,
            offset_ms,
            true,
        )
    }

    #[test]
    fn crosscheck_never_averages_and_skips_a_coalesced_upper_bound() {
        let m = |seq| Marker::new(MarkerKind::Output, 1, seq);
        let primary = vec![(m(1), fs(100)), (m(2), fs(200)), (m(3), fs(300))];
        let mut sec = HashMap::new();
        sec.insert(m(1), fs(95)); // gap +5 ms: inside
        sec.insert(m(2), fs(240)); // gap -40 ms: outside +/- two intervals
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
            10.0,
            true,
        );
        assert_eq!(
            (s.compared, s.agreed, s.disagreed, s.secondary_coalesced),
            (2, 1, 1, 1)
        );
        // Half the compared markers outside the band is far over 5%.
        assert_eq!(s.verdict, "disagree");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].t_primary_ns, Ns(200 * MS));
        assert_eq!(recs[0].t_secondary_ns, Ns(240 * MS));
    }

    /// CI run 38362046020's shape (windows-latest, 64 Hz, WARP): WGC later than
    /// DDA by 0-31 ms, median ~8 ms, ~12% of markers more than one refresh
    /// interval apart, ~1% more than two. The old any-marker rule voided this;
    /// the statistic rule measures it.
    #[test]
    fn a_hosted_vm_spread_of_one_to_two_intervals_agrees() {
        let (s, recs) = run_gaps(400, 10.0, |i| match i % 100 {
            0 => 45,                        // 1%: beyond two intervals of the band
            1..=12 => 16 + (i % 15) as i64, // 12%: one to two intervals
            _ => (i % 12) as i64,           // the bulk: 0-11 ms
        });
        assert_eq!(s.verdict, "agree", "{}", s.detail);
        assert_eq!(s.disagreed, 4);
        assert_eq!(recs.len(), 4, "each marker outside the band is recorded");
        assert!(s.median_gap_ms.is_some_and(|m| m < 15.0));
    }

    /// A time-base, unit or attribution bug moves every marker: the median
    /// leaves its band even though no single marker leaves the wide one.
    #[test]
    fn a_systematic_offset_disagrees() {
        // The secondary 20 ms LATER than the primary on every marker (gap -20):
        // inside the per-marker band [-31.25, 41.25], outside the median band
        // [-15.625, 25.625].
        let (s, _) = run_gaps(200, 10.0, |_| -20);
        assert_eq!(s.verdict, "disagree", "{}", s.detail);
        assert_eq!(s.disagreed, 0);
        assert!(s.detail.starts_with("systematic offset"), "{}", s.detail);
        // ...and the primary 30 ms later than the declared offset allows.
        let (s, _) = run_gaps(200, 10.0, |_| 30);
        assert_eq!(s.verdict, "disagree", "{}", s.detail);
        // With no declared offset the same 20 ms is systematic too.
        let (s, _) = run_gaps(200, 0.0, |_| 20);
        assert_eq!(s.verdict, "disagree", "{}", s.detail);
    }

    /// A source that misses frames sees some markers whole frames late: past
    /// 5% of markers outside the band, the run disagrees even with a sound
    /// median.
    #[test]
    fn a_source_missing_frames_disagrees() {
        // Every tenth marker 50 ms late in the primary (10%).
        let (s, _) = run_gaps(300, 10.0, |i| if i % 10 == 0 { 55 } else { 5 });
        assert_eq!(s.verdict, "disagree", "{}", s.detail);
        assert!(s.median_gap_ms.is_some_and(|m| (m - 5.0).abs() < 1e-9));
        assert!(
            s.detail.starts_with("a source missed frames"),
            "{}",
            s.detail
        );
        // At 5% exactly it still agrees; the bound is "more than".
        let (s, _) = run_gaps(300, 10.0, |i| if i % 20 == 0 { 55 } else { 5 });
        assert_eq!(s.verdict, "agree", "{}", s.detail);
    }

    #[test]
    fn crosscheck_without_a_secondary_is_not_performed() {
        let (s, _) = crosscheck(&[], |_| None, &[], |_| None, 16 * MS, 0.0, false);
        assert_eq!(s.verdict, "not_performed");
    }

    #[test]
    fn a_marker_only_the_secondary_saw_is_primary_unseen() {
        let m = Marker::new(MarkerKind::Output, 2, 5);
        let sec = vec![(m, fs(50))];
        let (s, _) = crosscheck(&[], |_| None, &sec, |_| None, 16 * MS, 0.0, true);
        assert_eq!(s.primary_unseen, 1);
        assert_eq!(s.verdict, "not_performed");
    }
}
