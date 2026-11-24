//! `#[ignore]`d round-trips of the readers that fold fix verdicts through
//! `reflection::fix_verdicts` (plan
//! 2026-11-09-reflection-reports-only-what-it-judged-and-the-dead-verifier-leaves-no-residue,
//! Phase 4). Run with
//! `cargo-guard.sh test -- --ignored database::pg::reflection::verdict_tests`
//! against the disposable database `tests::disposable_pg` describes.
use super::tests::{cleanup, disposable_pg, insert_reflection_run, insert_run, unique_workflow};
use super::*;

/// A fix of `source_run` carrying `effectiveness` (`None` = unjudged),
/// created at the literal instant `created`, tagged with `agent` so a
/// global reader can find this arm's rows among peers'.
async fn plant_fix(
    pg: &PgDb,
    id: &str,
    source_run: &str,
    reflection_run: &str,
    effectiveness: Option<&str>,
    created: &str,
    agent: &str,
) {
    let conn = pg.pool.get().await.expect("PG pool");
    conn.execute(
        "INSERT INTO reflection_fixes \
             (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, \
              status, effectiveness, source_agent, applied_at, created_at) \
         VALUES ($1, $2, $3, 'context_addition', 'wfv-p4 verdict fixture', 'applied', $4, $5, \
                 $6::TEXT::timestamptz, $6::TEXT::timestamptz)",
        &[
            &id,
            &source_run,
            &reflection_run,
            &effectiveness,
            &agent,
            &created,
        ],
    )
    .await
    .expect("insert reflection_fixes fixture");
}

/// A workflow whose fixes nobody judged has no rate, counts nothing as
/// ineffective or regression, and reports every fix as unjudged.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_an_only_unjudged_workflow_has_no_rate_and_zero_ineffective() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("unjudged");
    let reflection = insert_reflection_run(&pg, &wf).await;
    let run = format!("{wf}-r0");
    insert_run(&pg, &run, &wf, Some("2026-01-01T00:00:00Z")).await;
    let created = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    for n in 0..3 {
        plant_fix(
            &pg,
            &format!("{wf}-f{n}"),
            &run,
            &reflection,
            None,
            &created,
            &wf,
        )
        .await;
    }

    let report = pg
        .get_effectiveness_report(&wf)
        .await
        .expect("get_effectiveness_report");
    assert_eq!(report.total_fixes, 3);
    assert_eq!(report.unevaluated_count, 3);
    assert_eq!(report.effective_count, 0);
    assert_eq!(report.ineffective_count, 0);
    assert_eq!(report.regression_count, 0);
    assert_eq!(report.effectiveness_rate, None);

    let over_time = pg
        .get_effectiveness_over_time(&wf, "day", None)
        .await
        .expect("get_effectiveness_over_time");
    assert_eq!(over_time.buckets.len(), 1);
    let bucket = &over_time.buckets[0];
    assert_eq!(bucket.total, 3);
    assert_eq!(bucket.unjudged, 3);
    assert_eq!(bucket.ineffective, 0, "an unjudged fix is not ineffective");
    assert_eq!(bucket.regression, 0);
    assert_eq!(bucket.effectiveness_rate, None);
    cleanup(&pg, &wf).await;
}

/// One fix of each verdict plus an unjudged one: `caused_regression` lands
/// in `regression`, the rate is over the judged three, and the buckets add
/// up to the total.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_each_verdict_lands_in_its_own_bucket_and_the_rate_is_over_judged_fixes() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("verdicts");
    let reflection = insert_reflection_run(&pg, &wf).await;
    let run = format!("{wf}-r0");
    insert_run(&pg, &run, &wf, Some("2026-01-01T00:00:00Z")).await;
    let created = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    for (name, verdict) in [
        ("e", Some("effective")),
        ("i", Some("ineffective")),
        ("r", Some("caused_regression")),
        ("c", Some("inconclusive")),
        ("u", None),
    ] {
        plant_fix(
            &pg,
            &format!("{wf}-{name}"),
            &run,
            &reflection,
            verdict,
            &created,
            &wf,
        )
        .await;
    }

    let report = pg
        .get_effectiveness_report(&wf)
        .await
        .expect("get_effectiveness_report");
    assert_eq!(report.total_fixes, 5);
    assert_eq!(report.effective_count, 1);
    assert_eq!(report.ineffective_count, 1);
    assert_eq!(report.regression_count, 1);
    assert_eq!(report.inconclusive_count, 1);
    assert_eq!(report.unevaluated_count, 1);
    assert_eq!(report.effectiveness_rate, Some(1.0 / 3.0));

    let over_time = pg
        .get_effectiveness_over_time(&wf, "day", None)
        .await
        .expect("get_effectiveness_over_time");
    assert_eq!(over_time.buckets.len(), 1);
    let bucket = &over_time.buckets[0];
    assert_eq!(
        (
            bucket.total,
            bucket.effective,
            bucket.ineffective,
            bucket.regression,
            bucket.inconclusive,
            bucket.unjudged
        ),
        (5, 1, 1, 1, 1, 1)
    );
    assert_eq!(bucket.effectiveness_rate, Some(1.0 / 3.0));
    cleanup(&pg, &wf).await;
}

/// The failure analysis counts a NULL as unjudged, never effective, and its
/// rate is a percentage over judged fixes only.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_failure_analysis_never_counts_a_null_as_effective() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("fa");
    let reflection = insert_reflection_run(&pg, &wf).await;
    let run = format!("{wf}-r0");
    insert_run(&pg, &run, &wf, Some("2026-01-01T00:00:00Z")).await;
    let created = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    // The agent tag is this arm's own group in the global analysis.
    let only_null = format!("{wf}-null");
    let mixed = format!("{wf}-mixed");
    for n in 0..2 {
        plant_fix(
            &pg,
            &format!("{wf}-n{n}"),
            &run,
            &reflection,
            None,
            &created,
            &only_null,
        )
        .await;
    }
    for (name, verdict) in [
        ("me", Some("effective")),
        ("mi", Some("ineffective")),
        ("mr", Some("caused_regression")),
        ("mr2", Some("caused_regression")),
        ("mc", Some("inconclusive")),
        ("mu", None),
    ] {
        plant_fix(
            &pg,
            &format!("{wf}-{name}"),
            &run,
            &reflection,
            verdict,
            &created,
            &mixed,
        )
        .await;
    }

    let analysis = pg
        .get_failure_analysis(
            "2020-01-01T00:00:00Z",
            &crate::meta_optimizer::types::WorkflowCategory::Main,
        )
        .await
        .expect("get_failure_analysis");
    let record = |agent: &str| {
        analysis
            .reflection_fix_effectiveness
            .iter()
            .find(|r| r.source_agent.as_deref() == Some(agent))
            .unwrap_or_else(|| panic!("no fix-effectiveness row for {agent}"))
    };

    let only_null = record(&only_null);
    assert_eq!(only_null.total, 2);
    assert_eq!(only_null.unjudged, 2);
    assert_eq!(only_null.effective, 0, "a NULL is never effective");
    assert_eq!(only_null.effectiveness_rate, None);

    let mixed = record(&mixed);
    assert_eq!(
        (
            mixed.total,
            mixed.effective,
            mixed.ineffective,
            mixed.caused_regression,
            mixed.inconclusive,
            mixed.unjudged
        ),
        (6, 1, 1, 2, 1, 1)
    );
    assert_eq!(mixed.effectiveness_rate, Some(25.0));
    cleanup(&pg, &wf).await;
}
