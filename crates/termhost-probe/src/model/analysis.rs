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

/// Compare two sources' first appearances. A marker both saw is AGREED when
/// the times differ by at most one refresh interval and DISAGREED otherwise —
/// except where the secondary coalesced frames at that marker, which makes its
/// time an upper bound only (not compared). The two are never averaged (plan
/// D2): any disagreement makes the run's latencies unknown(capture_disagreement).
pub fn crosscheck(
    markers_primary: &[(Marker, FirstSeen)],
    secondary_seen: impl Fn(Marker) -> Option<FirstSeen>,
    secondary_markers: &[(Marker, FirstSeen)],
    primary_seen: impl Fn(Marker) -> Option<FirstSeen>,
    refresh_ns: u64,
    secondary_configured: bool,
) -> (CrosscheckSummary, Vec<CrosscheckRecord>) {
    let mut s = CrosscheckSummary {
        verdict: "not_performed",
        ..Default::default()
    };
    let mut records = Vec::new();
    if !secondary_configured {
        s.detail = "no secondary capture source in this run".into();
        return (s, records);
    }
    let refresh_ms = refresh_ns as f64 / 1e6;
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
        let delta = (q.t_ns as i128 - p.t_ns as i128) as f64 / 1e6;
        s.max_abs_delta_ms = Some(
            s.max_abs_delta_ms
                .map_or(delta.abs(), |m| m.max(delta.abs())),
        );
        let agree = delta.abs() <= refresh_ms;
        if agree {
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
    s.verdict = if s.disagreed > 0 {
        "disagree"
    } else if s.compared == 0 {
        "not_performed"
    } else {
        "agree"
    };
    s.detail = match s.verdict {
        "disagree" => format!(
            "{} of {} markers seen by both sources first appeared more than one refresh interval ({refresh_ms:.2} ms) apart",
            s.disagreed, s.compared
        ),
        "not_performed" => "the secondary source saw none of the primary's markers cleanly".into(),
        _ => format!("{} markers agreed within one refresh interval ({refresh_ms:.2} ms)", s.agreed),
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

    #[test]
    fn crosscheck_agrees_within_one_refresh_and_never_averages() {
        let refresh = 16_666_667;
        let m = |seq| Marker::new(MarkerKind::Output, 1, seq);
        let primary = vec![(m(1), fs(100)), (m(2), fs(200)), (m(3), fs(300))];
        let mut sec = HashMap::new();
        sec.insert(m(1), fs(110)); // agree (10 ms)
        sec.insert(m(2), fs(240)); // disagree (40 ms)
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
            refresh,
            true,
        );
        assert_eq!(
            (s.compared, s.agreed, s.disagreed, s.secondary_coalesced),
            (2, 1, 1, 1)
        );
        assert_eq!(s.verdict, "disagree");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].t_primary_ns, Ns(200 * MS));
        assert_eq!(recs[0].t_secondary_ns, Ns(240 * MS));
    }

    #[test]
    fn crosscheck_without_a_secondary_is_not_performed() {
        let (s, _) = crosscheck(&[], |_| None, &[], |_| None, 16 * MS, false);
        assert_eq!(s.verdict, "not_performed");
    }

    #[test]
    fn a_marker_only_the_secondary_saw_is_primary_unseen() {
        let m = Marker::new(MarkerKind::Output, 2, 5);
        let sec = vec![(m, fs(50))];
        let (s, _) = crosscheck(&[], |_| None, &sec, |_| None, 16 * MS, true);
        assert_eq!(s.primary_unseen, 1);
    }
}
