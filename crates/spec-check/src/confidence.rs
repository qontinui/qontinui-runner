//! Per-state match-rate → Confidence band (Step 6 of Plan 02, §5.4).
//!
//! Thresholds are `pub const` so downstream code (Stream G observability,
//! Stream C HTTP serialization) shares the same boundaries.
//!
//! **Wire enum** (`spec_check.rs:155`): `Confidence = High | Medium | Low`.
//! There is no `None` variant — sub-threshold rates are surfaced by
//! returning `Option<Confidence>` (`None` = below the floor).

use crate::{Confidence, RecommendedState, StateMatchResult};

/// Match-rate boundary at or above which the band is `Confidence::High`.
pub const CONFIDENCE_HIGH_THRESHOLD: f32 = 0.95;

/// Match-rate boundary at or above which the band is `Confidence::Medium`.
pub const CONFIDENCE_MEDIUM_THRESHOLD: f32 = 0.70;

/// Match-rate boundary at or above which the band is `Confidence::Low`.
/// Sub-floor rates produce `None`.
pub const CONFIDENCE_LOW_THRESHOLD: f32 = 0.30;

/// Map a per-state match rate to a confidence band.
///
/// Returns `None` when `rate < CONFIDENCE_LOW_THRESHOLD`. Callers that need
/// a non-Option for the wire (e.g. `RecommendedState.confidence:
/// Confidence`) must guard their construction on `Some(_)` and skip
/// emitting the recommendation otherwise.
///
/// NaN inputs return `None` — comparisons against NaN are always false, so
/// every threshold check fails through to the floor branch.
pub fn band_for_rate(rate: f32) -> Option<Confidence> {
    if rate >= CONFIDENCE_HIGH_THRESHOLD {
        Some(Confidence::High)
    } else if rate >= CONFIDENCE_MEDIUM_THRESHOLD {
        Some(Confidence::Medium)
    } else if rate >= CONFIDENCE_LOW_THRESHOLD {
        Some(Confidence::Low)
    } else {
        None
    }
}

/// Choose the recommended state from a set of evaluated states.
///
/// Selection rule:
///   1. Highest `match_rate` wins.
///   2. Tiebreak: state with `is_initial == Some(true)` wins.
///   3. Final tiebreak: `state_id` ascending (lexical) for determinism.
///
/// Returns `None` when all candidate states have `match_rate <
/// CONFIDENCE_LOW_THRESHOLD` (no band assignable) or when `states` is
/// empty. The returned `RecommendedState.reason` is always a non-empty,
/// human-readable explanation.
///
/// `is_initial` is a caller-supplied closure resolving a `state_id` to
/// `IrState.is_initial` (the spec IR isn't available in this crate's
/// public API surface — callers carry the lookup).
pub fn recommend_state(
    states: &[StateMatchResult],
    is_initial: impl Fn(&str) -> bool,
) -> Option<RecommendedState> {
    // Filter to states with an assignable band — this enforces the floor.
    let candidates: Vec<&StateMatchResult> = states
        .iter()
        .filter(|s| band_for_rate(s.match_rate).is_some())
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Find the top match rate. NaN cannot survive the floor filter (NaN
    // fails every `>=`), so partial_cmp on the survivors is well-defined.
    let top_rate = candidates
        .iter()
        .map(|s| s.match_rate)
        .fold(f32::NEG_INFINITY, f32::max);

    // Collect all states tied at the top rate. Use a strict bit-equal
    // check rather than `==` to avoid spurious ties from float drift —
    // every `match_rate` in `candidates` was compared against the same
    // `top_rate` via `f32::max`, so the winners share its bit pattern.
    let top_bits = top_rate.to_bits();
    let tied: Vec<&StateMatchResult> = candidates
        .iter()
        .copied()
        .filter(|s| s.match_rate.to_bits() == top_bits)
        .collect();

    // Safe: candidates was non-empty and top_rate came from it, so at
    // least one state ties with itself.
    debug_assert!(!tied.is_empty());

    let band = band_for_rate(top_rate).expect("survived the floor filter");

    if tied.len() == 1 {
        let winner = tied[0];
        let reason = format!(
            "highest match rate ({:.2}) among {} states",
            winner.match_rate,
            states.len()
        );
        return Some(RecommendedState {
            state_id: winner.state_id.clone(),
            confidence: band,
            reason,
        });
    }

    // Tiebreak 1: prefer is_initial.
    let initials: Vec<&StateMatchResult> = tied
        .iter()
        .copied()
        .filter(|s| is_initial(&s.state_id))
        .collect();

    if initials.len() == 1 {
        let winner = initials[0];
        // Name another contender for the evidence string — pick the
        // lexically smallest non-winner among the ties.
        let other = tied
            .iter()
            .copied()
            .filter(|s| s.state_id != winner.state_id)
            .min_by(|a, b| a.state_id.cmp(&b.state_id))
            .map(|s| s.state_id.clone())
            .unwrap_or_default();
        let reason = format!(
            "highest match rate ({:.2}), tied with {}, won on is_initial",
            winner.match_rate, other
        );
        return Some(RecommendedState {
            state_id: winner.state_id.clone(),
            confidence: band,
            reason,
        });
    }

    // Either zero or multiple initials — narrow the pool then fall through
    // to the lexical tiebreak. If multiple states are flagged initial, we
    // tiebreak among them; otherwise we tiebreak among all ties.
    let pool: Vec<&StateMatchResult> = if initials.len() > 1 { initials } else { tied };

    // Tiebreak 2: lexically smallest state_id.
    let winner = pool
        .iter()
        .copied()
        .min_by(|a, b| a.state_id.cmp(&b.state_id))
        .expect("pool is non-empty");

    let other = pool
        .iter()
        .copied()
        .filter(|s| s.state_id != winner.state_id)
        .min_by(|a, b| a.state_id.cmp(&b.state_id))
        .map(|s| s.state_id.clone())
        .unwrap_or_default();

    let reason = format!(
        "highest match rate ({:.2}), tied with {}, won on state_id ordering",
        winner.match_rate, other
    );

    Some(RecommendedState {
        state_id: winner.state_id.clone(),
        confidence: band,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(id: &str, rate: f32) -> StateMatchResult {
        StateMatchResult {
            state_id: id.to_string(),
            state_name: id.to_string(),
            match_rate: rate,
            classification: crate::ThresholdConfig::default().classify_match_rate(rate),
            assertions: vec![],
        }
    }

    // ------------- band_for_rate -------------

    #[test]
    fn band_for_rate_high_boundary() {
        assert_eq!(band_for_rate(0.95), Some(Confidence::High));
        assert_eq!(band_for_rate(1.0), Some(Confidence::High));
        assert_eq!(band_for_rate(0.999), Some(Confidence::High));
    }

    #[test]
    fn band_for_rate_medium_just_below_high() {
        assert_eq!(band_for_rate(0.949), Some(Confidence::Medium));
        assert_eq!(band_for_rate(0.70), Some(Confidence::Medium));
    }

    #[test]
    fn band_for_rate_low_band() {
        assert_eq!(band_for_rate(0.30), Some(Confidence::Low));
        assert_eq!(band_for_rate(0.699), Some(Confidence::Low));
    }

    #[test]
    fn band_for_rate_below_floor_returns_none() {
        assert_eq!(band_for_rate(0.29), None);
        assert_eq!(band_for_rate(0.0), None);
    }

    #[test]
    fn band_for_rate_nan_returns_none() {
        assert_eq!(band_for_rate(f32::NAN), None);
    }

    // ------------- recommend_state -------------

    #[test]
    fn recommend_state_empty_returns_none() {
        let states: Vec<StateMatchResult> = vec![];
        assert!(recommend_state(&states, |_| false).is_none());
    }

    #[test]
    fn recommend_state_all_sub_floor_returns_none() {
        let states = vec![state("a", 0.0), state("b", 0.29)];
        assert!(recommend_state(&states, |_| false).is_none());
    }

    #[test]
    fn recommend_state_single_winner() {
        let states = vec![state("a", 0.40), state("b", 0.85), state("c", 0.70)];
        let rec = recommend_state(&states, |_| false).expect("a winner");
        assert_eq!(rec.state_id, "b");
        assert_eq!(rec.confidence, Confidence::Medium);
        assert!(!rec.reason.is_empty());
        assert!(rec.reason.contains("0.85"));
    }

    #[test]
    fn recommend_state_is_initial_tiebreak() {
        let states = vec![state("a", 0.60), state("b", 0.60)];
        // Only "b" is initial.
        let rec = recommend_state(&states, |id| id == "b").expect("a winner");
        assert_eq!(rec.state_id, "b");
        assert_eq!(rec.confidence, Confidence::Low);
        assert!(rec.reason.contains("is_initial"));
    }

    #[test]
    fn recommend_state_state_id_tiebreak_no_initial() {
        // Repeat to guard against accidental nondeterminism (the plan
        // calls out 100 runs).
        for _ in 0..100 {
            let states = vec![
                state("zebra", 0.60),
                state("alpha", 0.60),
                state("mango", 0.60),
            ];
            let rec = recommend_state(&states, |_| false).expect("a winner");
            assert_eq!(rec.state_id, "alpha");
            assert!(rec.reason.contains("state_id ordering"));
        }
    }

    #[test]
    fn recommend_state_state_id_tiebreak_multiple_initials() {
        // Two initials tied — tiebreak by state_id within the initials pool.
        let states = vec![
            state("zebra", 0.80),
            state("alpha", 0.80),
            state("mango", 0.80),
        ];
        let rec = recommend_state(&states, |id| id == "zebra" || id == "mango").expect("a winner");
        // Both zebra and mango are initial; lex-smallest is mango.
        assert_eq!(rec.state_id, "mango");
    }

    #[test]
    fn recommend_state_reason_is_non_empty() {
        let states = vec![state("only", 0.95)];
        let rec = recommend_state(&states, |_| false).expect("a winner");
        assert!(!rec.reason.is_empty());
        assert_eq!(rec.confidence, Confidence::High);
    }

    #[test]
    fn recommend_state_skips_sub_floor_when_some_qualify() {
        let states = vec![state("low", 0.10), state("mid", 0.75)];
        let rec = recommend_state(&states, |_| false).expect("a winner");
        assert_eq!(rec.state_id, "mid");
    }
}
