//! A2-D3's decision, one literal fixture per arm, DB-free (plan
//! 2026-11-06-a-product-workflow-verification-with-nothing-to-check-never-reports-passed,
//! Phase 4). The `#[ignore]`d round-trips in `tests` drive the same arms
//! through `evaluate_pending_fixes` against real `timestamptz` rows.
use super::*;

/// A fix exposed to `later_runs` later runs, whose source finding resolves
/// to a signature that never recurred, with no new signature seen.
fn exposed(later_runs: i64) -> FixExposure {
    FixExposure {
        later_runs,
        has_source_finding: true,
        source_signature_known: true,
        recurrences: 0,
        new_signature_peak_runs: 0,
    }
}

#[test]
fn a_fix_with_fewer_than_three_later_runs_stays_unevaluated() {
    assert_eq!(decide_fix_effectiveness(&exposed(0)), None);
    assert_eq!(decide_fix_effectiveness(&exposed(1)), None);
    assert_eq!(decide_fix_effectiveness(&exposed(2)), None);
    // Exposure is tested first: neither a recurrence nor a missing finding
    // is judged before the fix was exposed.
    assert_eq!(
        decide_fix_effectiveness(&FixExposure {
            recurrences: 2,
            ..exposed(2)
        }),
        None
    );
    assert_eq!(
        decide_fix_effectiveness(&FixExposure {
            has_source_finding: false,
            source_signature_known: false,
            ..exposed(1)
        }),
        None
    );
    // The boundary: the third later run is exposure enough.
    assert_eq!(
        decide_fix_effectiveness(&exposed(3)),
        Some(FixEffectiveness::Effective)
    );
}

#[test]
fn a_fix_with_no_source_finding_reads_inconclusive() {
    for later_runs in [3, 12] {
        let exposure = FixExposure {
            has_source_finding: false,
            source_signature_known: false,
            ..exposed(later_runs)
        };
        assert_eq!(
            decide_fix_effectiveness(&exposure),
            Some(FixEffectiveness::Inconclusive),
            "{later_runs} later runs"
        );
    }
}

#[test]
fn a_fix_whose_finding_has_no_signature_reads_inconclusive() {
    let exposure = FixExposure {
        source_signature_known: false,
        ..exposed(5)
    };
    assert_eq!(
        decide_fix_effectiveness(&exposure),
        Some(FixEffectiveness::Inconclusive)
    );
}

#[test]
fn a_fix_whose_finding_recurs_reads_ineffective() {
    for recurrences in [1, 4] {
        let exposure = FixExposure {
            recurrences,
            ..exposed(5)
        };
        assert_eq!(
            decide_fix_effectiveness(&exposure),
            Some(FixEffectiveness::Ineffective),
            "{recurrences} recurrences"
        );
    }
    // A recurrence outranks a new signature: the fix did not fix its own finding.
    assert_eq!(
        decide_fix_effectiveness(&FixExposure {
            recurrences: 1,
            new_signature_peak_runs: 3,
            ..exposed(6)
        }),
        Some(FixEffectiveness::Ineffective)
    );
}

#[test]
fn a_new_signature_in_two_later_runs_reads_caused_regression() {
    for peak in [2, 5] {
        let exposure = FixExposure {
            new_signature_peak_runs: peak,
            ..exposed(5)
        };
        assert_eq!(
            decide_fix_effectiveness(&exposure),
            Some(FixEffectiveness::CausedRegression),
            "a new signature in {peak} later runs"
        );
    }
    // In one later run it is not a regression yet.
    assert_eq!(
        decide_fix_effectiveness(&FixExposure {
            new_signature_peak_runs: 1,
            ..exposed(4)
        }),
        Some(FixEffectiveness::Effective)
    );
}

#[test]
fn an_exposed_fix_whose_finding_never_recurred_reads_effective() {
    for later_runs in [3, 40] {
        assert_eq!(
            decide_fix_effectiveness(&exposed(later_runs)),
            Some(FixEffectiveness::Effective),
            "{later_runs} later runs"
        );
    }
}

#[test]
fn an_unreadable_exposure_writes_no_verdict() {
    // A failed read is NO verdict, never the zero count `.unwrap_or(0)`
    // used to turn into `effective`.
    assert_eq!(
        judge_fix_exposure_read(
            "fix-1",
            Err("PG fix exposure: [42703] column does not exist".to_string())
        ),
        None
    );
}

#[test]
fn a_readable_exposure_carries_its_verdict_and_the_counts_it_rests_on() {
    let judged = judge_fix_exposure_read(
        "fix-1",
        Ok(FixExposure {
            new_signature_peak_runs: 1,
            ..exposed(4)
        }),
    );
    let (verdict, evidence) = judged.expect("an exposed fix is judged");
    assert_eq!(verdict, FixEffectiveness::Effective);
    let evidence: serde_json::Value =
        serde_json::from_str(&evidence).expect("the evidence is JSON");
    assert_eq!(evidence["verdict"], "effective");
    assert_eq!(evidence["later_runs"], 4);
    assert_eq!(evidence["min_later_runs"], 3);
    assert_eq!(evidence["source_finding"], true);
    assert_eq!(evidence["source_signature_known"], true);
    assert_eq!(evidence["recurrences"], 0);
    assert_eq!(evidence["new_signature_peak_runs"], 1);
    assert_eq!(evidence["min_later_runs_for_regression"], 2);
    // An unexposed fix gets no verdict, so nothing is written for it.
    assert_eq!(judge_fix_exposure_read("fix-2", Ok(exposed(2))), None);
}

#[test]
fn prefixed_select_columns_qualifies_every_selected_column() {
    let columns = prefixed_select_columns("rf");
    let parts: Vec<&str> = columns.split(", ").collect();
    assert_eq!(parts.len(), 24, "{columns}");
    assert!(parts.iter().all(|c| c.starts_with("rf.")), "{columns}");
    assert_eq!(parts[0], "rf.id");
    assert!(parts.contains(&"rf.applied_at::TEXT"), "{columns}");
    assert!(parts.contains(&"rf.applicability_context"), "{columns}");
}
