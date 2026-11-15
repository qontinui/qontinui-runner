//! `#[ignore]`d round-trips of the reflection-fix lifecycle against a
//! DISPOSABLE Postgres carrying the project schema, addressed by
//! `DATABASE_URL` (the `database/pg/*` convention, `token_usage.rs`; plan
//! 2026-11-06-a-product-workflow-verification-with-nothing-to-check-never-reports-passed,
//! Phase 4). Every arm WRITES, so the database is never guessed:
//! [`disposable_pg`] refuses an unset `DATABASE_URL`, and one naming
//! `qontinui_db` (the canonical-dev fallback's database, and production's).
//! Each arm's fixtures live under a workflow name unique to the run, so the
//! evaluator's workflow-scoped re-queue touches only rows the arm created,
//! and each arm deletes its runs (cascading to findings, fixes and fix
//! applications) when it passes.
//!
//! Recipe: start a throwaway cluster
//! (`qontinui-claude-config/scripts/ephemeral-db.sh start pgvector` prints
//! a DSN), load `src-tauri/schema.pg.sql.generated` into a database other
//! than `qontinui_db` the way `embedded_pg::apply_canonical_schema` does
//! (`CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public` first,
//! every `CREATE SCHEMA` made `IF NOT EXISTS`, and on a cluster without
//! pgvector the vector columns as `bytea` with their ivfflat/hnsw indexes
//! dropped), export that DSN as `DATABASE_URL`, then from the runner
//! checkout run `cargo-guard.sh test -- --ignored database::pg::reflection`
//! (`database` is a BIN module, so `--lib` would match nothing).
use super::*;
use std::collections::HashMap;

/// A connection to the disposable database. `PgDb::new` runs the boot
/// self-provisioning DDL, which two arms racing on a fresh database can
/// collide on (`CREATE … IF NOT EXISTS` is not race-free), so connections
/// open one at a time.
async fn disposable_pg() -> PgDb {
    static PROVISION: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        panic!(
            "refusing to run: DATABASE_URL is unset. These round-trips WRITE, so they run \
             only against a disposable database carrying the project schema"
        )
    });
    let config: tokio_postgres::Config = url
        .parse()
        .expect("DATABASE_URL parses as a Postgres connection string");
    let database = config
        .get_dbname()
        .or(config.get_user())
        .unwrap_or_default();
    assert_ne!(
        database, "qontinui_db",
        "refusing to run against qontinui_db, the canonical dev database and production's: \
         point DATABASE_URL at a disposable database"
    );
    let _one_at_a_time = PROVISION
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    PgDb::new(&url)
        .await
        .expect("PgDb::new against the disposable database")
}

/// A workflow name no other run of these arms shares.
fn unique_workflow(arm: &str) -> String {
    format!("wfv-p4-{arm}-{}", uuid::Uuid::new_v4())
}

/// A run of `workflow`, completed at the literal RFC 3339 instant
/// `completed_at`, or still running when that is `None`.
async fn insert_run(pg: &PgDb, id: &str, workflow: &str, completed_at: Option<&str>) {
    let status = if completed_at.is_some() {
        "complete"
    } else {
        "running"
    };
    let conn = pg.pool.get().await.expect("PG pool");
    conn.execute(
        "INSERT INTO task_runs (id, task_name, workflow_name, status, completed_at) \
         VALUES ($1, 'wfv-p4 fixture run', $2, $3, $4::TEXT::timestamptz)",
        &[&id, &workflow, &status, &completed_at],
    )
    .await
    .expect("insert task_runs fixture");
}

/// The reflection run every fixture fix names (`reflection_task_run_id` is
/// a foreign key). Named as production names it, so it is never a run of
/// the reflected workflow.
async fn insert_reflection_run(pg: &PgDb, workflow: &str) -> String {
    let id = format!("{workflow}-reflection");
    insert_run(pg, &id, &format!("Reflection: {workflow}"), None).await;
    id
}

async fn insert_finding(pg: &PgDb, id: &str, task_run_id: &str, signature: Option<&str>) {
    let conn = pg.pool.get().await.expect("PG pool");
    conn.execute(
        "INSERT INTO task_run_findings \
             (id, task_run_id, category, severity, signature_hash, title, description, \
              detected_in_session) \
         VALUES ($1, $2, 'test_failure', 'medium', $3, 'wfv-p4 fixture finding', \
                 'fixture', 1)",
        &[&id, &task_run_id, &signature],
    )
    .await
    .expect("insert task_run_findings fixture");
}

/// One fixture fix. `applied_at` (and `created_at`) is a literal RFC 3339
/// instant, cast in SQL; `effectiveness` / `evidence` let an arm plant a
/// verdict an earlier evaluator (or the hand-set door) wrote.
struct FixRow<'a> {
    id: &'a str,
    source_run: &'a str,
    reflection_run: &'a str,
    finding: Option<&'a str>,
    applied_at: &'a str,
    effectiveness: Option<&'a str>,
    evidence: Option<&'a str>,
}

async fn insert_fix(pg: &PgDb, fix: FixRow<'_>) {
    let description = format!("wfv-p4 fixture fix {}", fix.id);
    let conn = pg.pool.get().await.expect("PG pool");
    conn.execute(
        "INSERT INTO reflection_fixes \
             (id, source_task_run_id, reflection_task_run_id, source_finding_id, fix_type, \
              fix_description, status, effectiveness, effectiveness_evidence, applied_at, \
              created_at) \
         VALUES ($1, $2, $3, $4, 'context_addition', $5, 'applied', $6, $7, \
                 $8::TEXT::timestamptz, $8::TEXT::timestamptz)",
        &[
            &fix.id,
            &fix.source_run,
            &fix.reflection_run,
            &fix.finding,
            &description,
            &fix.effectiveness,
            &fix.evidence,
            &fix.applied_at,
        ],
    )
    .await
    .expect("insert reflection_fixes fixture");
}

/// The fix as stored now.
async fn stored(pg: &PgDb, id: &str) -> ReflectionFix {
    pg.get_reflection_fix(id)
        .await
        .expect("read the fixture fix")
        .expect("the fixture fix exists")
}

/// The counts a written verdict rests on, parsed.
fn evidence_of(fix: &ReflectionFix) -> serde_json::Value {
    let raw = fix
        .effectiveness_evidence
        .as_deref()
        .unwrap_or_else(|| panic!("{} carries no effectiveness_evidence", fix.id));
    serde_json::from_str(raw).expect("effectiveness_evidence is JSON")
}

/// Delete `workflow`'s fixture runs; findings, fixes and fix applications
/// go with them (`ON DELETE CASCADE`).
async fn cleanup(pg: &PgDb, workflow: &str) {
    let reflection = format!("Reflection: {workflow}");
    let conn = pg.pool.get().await.expect("PG pool");
    conn.execute(
        "DELETE FROM task_runs WHERE workflow_name = $1 OR workflow_name = $2",
        &[&workflow, &reflection],
    )
    .await
    .expect("delete the fixture runs");
}

/// The count the evaluator before Phase 4 ran for each fix, VERBATIM. It
/// bound the workflow name, the fix's `created_at` as the `String` that
/// `SELECT_ALL_COLUMNS` reads it as (`created_at::TEXT`), and the fix's
/// `source_finding_id`.
const PRE_PHASE_4_COUNT_QUERY: &str = r#"SELECT COUNT(*)
                   FROM task_run_findings trf
                   INNER JOIN task_runs tr ON tr.id = trf.task_run_id
                   WHERE tr.workflow_name = $1
                     AND tr.completed_at > $2
                     AND trf.signature_hash = (
                         SELECT trf2.signature_hash
                         FROM task_run_findings trf2
                         WHERE trf2.id = $3
                     )"#;

/// Phase 4's re-measure, recorded: today's (pre-Phase-4) evaluator, run as
/// written. Its pending-fix read is rejected by the server
/// (`column reference "id" is ambiguous`), so its count was never reached;
/// and the count itself, bound as written, is refused by the driver on its
/// parameter TYPE (a `String` bound to `timestamptz`). So no `effective`
/// stamp was ever earned, and the re-queue withdraws every `effective` stamp
/// that carries no evidence.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_todays_count_query_as_written_errors_on_its_parameter_type() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("todays-query");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-11T00:00:00Z")).await;
    insert_finding(&pg, &id("f"), &id("r0"), Some(&id("sig"))).await;
    insert_fix(
        &pg,
        FixRow {
            id: &id("fix"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: Some(&id("f")),
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;
    let conn = pg.pool.get().await.expect("PG pool");

    // (1) Its pending-fix read, as written: the bare column list through a
    // join to task_runs.
    let pending_read = format!(
        "SELECT {} FROM reflection_fixes rf \
         INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id \
         WHERE tr.workflow_name = $1 \
           AND rf.status = 'applied' \
           AND rf.effectiveness IS NULL \
         ORDER BY rf.created_at DESC \
         LIMIT 100",
        SELECT_ALL_COLUMNS
    );
    let read_err = conn
        .query(&pending_read, &[&wf])
        .await
        .expect_err("the pre-Phase-4 pending-fix read cannot run");
    eprintln!("re-measure (1), the pre-Phase-4 pending-fix read: {read_err}");
    assert_eq!(
        read_err.as_db_error().map(|db| db.code()),
        Some(&tokio_postgres::error::SqlState::AMBIGUOUS_COLUMN),
        "{read_err}"
    );

    // (2) Its count, with the fix read the way that code read it.
    let fix = stored(&pg, &id("fix")).await;
    let count_err = conn
        .query_one(
            PRE_PHASE_4_COUNT_QUERY,
            &[&wf, &fix.created_at, &fix.source_finding_id],
        )
        .await
        .expect_err("the pre-Phase-4 count cannot run");
    eprintln!("re-measure (2), the pre-Phase-4 count bound as written: {count_err}");
    let wrong_type = std::error::Error::source(&count_err)
        .and_then(|cause| cause.downcast_ref::<tokio_postgres::types::WrongType>());
    assert!(
        wrong_type.is_some(),
        "the count must fail on its parameter TYPE, a String bound to timestamptz: {count_err}"
    );
    assert!(
        count_err
            .to_string()
            .contains("error serializing parameter 1"),
        "{count_err}"
    );
    cleanup(&pg, &wf).await;
}

/// The arming arm: `POST /reflection/evaluate`'s handler routes to
/// `evaluate_pending_fixes`, and counts the rows it returns. One workflow,
/// six pending fixes, one call: only the five exposed fixes are written, each
/// returned with the verdict written; the unexposed one is neither written
/// nor returned.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_evaluate_judges_only_exposed_fixes_and_returns_only_the_rows_it_wrote() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("evaluate");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    // The source run, then five later runs of the workflow.
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-11T00:00:00Z")).await;
    insert_run(&pg, &id("r2"), &wf, Some("2026-01-12T00:00:00Z")).await;
    insert_run(&pg, &id("r3"), &wf, Some("2026-01-13T00:00:00Z")).await;
    insert_run(&pg, &id("r4"), &wf, Some("2026-01-14T00:00:00Z")).await;
    insert_run(&pg, &id("r5"), &wf, Some("2026-01-15T00:00:00Z")).await;
    // The source run's findings; a signature first seen in r1 and r2; and
    // the source run's `rec` signature again in r4.
    insert_finding(&pg, &id("f-ok"), &id("r0"), Some(&id("sig-ok"))).await;
    insert_finding(&pg, &id("f-rec"), &id("r0"), Some(&id("sig-rec"))).await;
    insert_finding(&pg, &id("f-nosig"), &id("r0"), None).await;
    insert_finding(&pg, &id("f-new-r1"), &id("r1"), Some(&id("sig-new"))).await;
    insert_finding(&pg, &id("f-new-r2"), &id("r2"), Some(&id("sig-new"))).await;
    insert_finding(&pg, &id("f-rec-r4"), &id("r4"), Some(&id("sig-rec"))).await;

    let unexposed = id("fix-unexposed");
    let no_finding = id("fix-no-finding");
    let recurs = id("fix-recurs");
    let regression = id("fix-regression");
    let effective = id("fix-effective");
    let no_signature = id("fix-no-signature");
    for (fix, finding, applied_at) in [
        // Later runs r4 and r5 only.
        (&unexposed, Some(id("f-ok")), "2026-01-13T12:00:00Z"),
        (&no_finding, None, "2026-01-10T00:00:00Z"),
        // Later runs r3..r5; `rec` is back in r4.
        (&recurs, Some(id("f-rec")), "2026-01-12T12:00:00Z"),
        // Later runs r1..r5; `new`, unseen by 2026-01-10, is in r1 and r2.
        (&regression, Some(id("f-ok")), "2026-01-10T00:00:00Z"),
        // Later runs r3..r5; `ok` never recurs, `new` and `rec` were seen before.
        (&effective, Some(id("f-ok")), "2026-01-12T12:00:00Z"),
        (&no_signature, Some(id("f-nosig")), "2026-01-10T00:00:00Z"),
    ] {
        insert_fix(
            &pg,
            FixRow {
                id: fix,
                source_run: &id("r0"),
                reflection_run: &reflection,
                finding: finding.as_deref(),
                applied_at,
                effectiveness: None,
                evidence: None,
            },
        )
        .await;
    }

    let written = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("evaluate_pending_fixes");

    let verdicts: HashMap<String, Option<String>> = written
        .iter()
        .map(|fix| (fix.id.clone(), fix.effectiveness.clone()))
        .collect();
    let expected: HashMap<String, Option<String>> = [
        (no_finding.clone(), "inconclusive"),
        (recurs.clone(), "ineffective"),
        (regression.clone(), "caused_regression"),
        (effective.clone(), "effective"),
        (no_signature.clone(), "inconclusive"),
    ]
    .into_iter()
    .map(|(fix, verdict)| (fix, Some(verdict.to_string())))
    .collect();
    assert_eq!(
        verdicts, expected,
        "the call returns exactly the rows it wrote, each with the verdict it wrote"
    );
    assert_eq!(
        written.len(),
        5,
        "six fixes were pending; five were exposed"
    );
    for fix in &written {
        assert!(
            fix.evaluated_at.is_some(),
            "{} carries evaluated_at",
            fix.id
        );
        assert!(
            fix.effectiveness_evidence.is_some(),
            "{} carries the counts its verdict rests on",
            fix.id
        );
        assert_eq!(
            stored(&pg, &fix.id).await.effectiveness,
            fix.effectiveness,
            "the database holds what the call reported for {}",
            fix.id
        );
    }

    // The unexposed fix was neither written nor reported: still unevaluated.
    let left = stored(&pg, &unexposed).await;
    assert_eq!(
        (
            left.effectiveness,
            left.effectiveness_evidence,
            left.evaluated_at
        ),
        (None, None, None)
    );

    // A second pass writes nothing: no verdict of the first is withdrawn,
    // and the unexposed fix is still unexposed.
    let again = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("a second evaluate_pending_fixes");
    assert!(
        again.is_empty(),
        "a second pass wrote {:?}",
        again.iter().map(|fix| &fix.id).collect::<Vec<_>>()
    );
    assert_eq!(
        stored(&pg, &effective).await.effectiveness.as_deref(),
        Some("effective")
    );
    cleanup(&pg, &wf).await;
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_fewer_than_three_later_runs_leaves_the_fix_unevaluated() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("unexposed");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-11T00:00:00Z")).await;
    insert_run(&pg, &id("r2"), &wf, Some("2026-01-12T00:00:00Z")).await;
    // A run that has not completed is no exposure.
    insert_run(&pg, &id("r3"), &wf, None).await;
    insert_finding(&pg, &id("f"), &id("r0"), Some(&id("sig"))).await;
    insert_fix(
        &pg,
        FixRow {
            id: &id("fix"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: Some(&id("f")),
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;

    let written = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("evaluate_pending_fixes");
    assert!(
        written.is_empty(),
        "two later runs judge nothing: {written:?}"
    );
    let fix = stored(&pg, &id("fix")).await;
    assert_eq!(
        (
            fix.effectiveness,
            fix.effectiveness_evidence,
            fix.evaluated_at
        ),
        (None, None, None)
    );
    cleanup(&pg, &wf).await;
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_a_recurring_finding_reads_ineffective() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("recurs");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-11T00:00:00Z")).await;
    insert_run(&pg, &id("r2"), &wf, Some("2026-01-12T00:00:00Z")).await;
    insert_run(&pg, &id("r3"), &wf, Some("2026-01-13T00:00:00Z")).await;
    insert_finding(&pg, &id("f"), &id("r0"), Some(&id("sig"))).await;
    insert_finding(&pg, &id("f-again"), &id("r2"), Some(&id("sig"))).await;
    insert_fix(
        &pg,
        FixRow {
            id: &id("fix"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: Some(&id("f")),
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;

    let written = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("evaluate_pending_fixes");
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(written[0].effectiveness.as_deref(), Some("ineffective"));
    let evidence = evidence_of(&written[0]);
    assert_eq!(evidence["later_runs"], 3);
    assert_eq!(evidence["recurrences"], 1);
    cleanup(&pg, &wf).await;
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_a_new_signature_in_two_later_runs_reads_caused_regression() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("regression");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-11T00:00:00Z")).await;
    insert_run(&pg, &id("r2"), &wf, Some("2026-01-12T00:00:00Z")).await;
    insert_run(&pg, &id("r3"), &wf, Some("2026-01-13T00:00:00Z")).await;
    insert_finding(&pg, &id("f"), &id("r0"), Some(&id("sig"))).await;
    insert_finding(&pg, &id("new-r1"), &id("r1"), Some(&id("sig-new"))).await;
    insert_finding(&pg, &id("new-r3"), &id("r3"), Some(&id("sig-new"))).await;
    insert_fix(
        &pg,
        FixRow {
            id: &id("fix"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: Some(&id("f")),
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;

    let written = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("evaluate_pending_fixes");
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(
        written[0].effectiveness.as_deref(),
        Some("caused_regression")
    );
    let evidence = evidence_of(&written[0]);
    assert_eq!(evidence["recurrences"], 0);
    assert_eq!(evidence["new_signature_peak_runs"], 2);
    cleanup(&pg, &wf).await;
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_an_exposed_fix_without_recurrence_reads_effective_with_its_counts() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("effective");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-11T00:00:00Z")).await;
    insert_run(&pg, &id("r2"), &wf, Some("2026-01-12T00:00:00Z")).await;
    insert_run(&pg, &id("r3"), &wf, Some("2026-01-13T00:00:00Z")).await;
    insert_finding(&pg, &id("f"), &id("r0"), Some(&id("sig"))).await;
    insert_finding(&pg, &id("old"), &id("r0"), Some(&id("sig-old"))).await;
    // A new signature in ONE later run, and an old one again: neither is a regression.
    insert_finding(&pg, &id("new-r2"), &id("r2"), Some(&id("sig-new"))).await;
    insert_finding(&pg, &id("old-r3"), &id("r3"), Some(&id("sig-old"))).await;
    insert_fix(
        &pg,
        FixRow {
            id: &id("fix"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: Some(&id("f")),
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;

    let written = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("evaluate_pending_fixes");
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(written[0].effectiveness.as_deref(), Some("effective"));
    let evidence = evidence_of(&written[0]);
    assert_eq!(evidence["verdict"], "effective");
    assert_eq!(evidence["later_runs"], 3);
    assert_eq!(evidence["recurrences"], 0);
    assert_eq!(evidence["new_signature_peak_runs"], 1);
    cleanup(&pg, &wf).await;
}

/// Both re-queue scopes, and what the re-queue never touches.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_requeue_withdraws_unjudged_and_unexposed_effective_stamps_of_this_workflow_only() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("requeue");
    let other = unique_workflow("requeue-other");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    let other_reflection = insert_reflection_run(&pg, &other).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-11T00:00:00Z")).await;
    insert_run(&pg, &id("r2"), &wf, Some("2026-01-12T00:00:00Z")).await;
    insert_run(&pg, &id("r3"), &wf, Some("2026-01-13T00:00:00Z")).await;
    insert_finding(&pg, &id("f-a"), &id("r0"), Some(&id("sig-a"))).await;
    insert_finding(&pg, &id("f-b"), &id("r0"), Some(&id("sig-b"))).await;
    insert_finding(&pg, &id("f-b-again"), &id("r2"), Some(&id("sig-b"))).await;
    let other_r0 = format!("{other}-r0");
    insert_run(&pg, &other_r0, &other, Some("2026-01-01T00:00:00Z")).await;

    let unjudged = id("fix-unjudged");
    let hand_no_finding = id("fix-hand-no-finding");
    let hand_unexposed = id("fix-hand-unexposed");
    let judged = id("fix-judged");
    let ineffective = id("fix-ineffective");
    let hand_evidence = r#"{"set_by":"operator"}"#;
    let judged_evidence = r#"{"verdict":"effective","later_runs":3}"#;
    for (fix, finding, applied_at, effectiveness, evidence) in [
        // The re-measured scope: `effective` with no evidence, though
        // exposed (three later runs, a signature) — nothing judged it.
        (
            &unjudged,
            Some(id("f-b")),
            "2026-01-10T00:00:00Z",
            "effective",
            None,
        ),
        // A2-D3's scope: set by hand, no finding to observe.
        (
            &hand_no_finding,
            None,
            "2026-01-10T00:00:00Z",
            "effective",
            Some(hand_evidence),
        ),
        // A2-D3's scope: set by hand after one later run.
        (
            &hand_unexposed,
            Some(id("f-a")),
            "2026-01-12T12:00:00Z",
            "effective",
            Some(hand_evidence),
        ),
        // A verdict that rests on evidence and exposure stands.
        (
            &judged,
            Some(id("f-a")),
            "2026-01-10T00:00:00Z",
            "effective",
            Some(judged_evidence),
        ),
        // Only `effective` is withdrawn.
        (
            &ineffective,
            Some(id("f-a")),
            "2026-01-10T00:00:00Z",
            "ineffective",
            None,
        ),
    ] {
        insert_fix(
            &pg,
            FixRow {
                id: fix,
                source_run: &id("r0"),
                reflection_run: &reflection,
                finding: finding.as_deref(),
                applied_at,
                effectiveness: Some(effectiveness),
                evidence,
            },
        )
        .await;
    }
    // Another workflow's unjudged stamp: outside the evaluated workflow.
    let other_fix = format!("{other}-fix-unjudged");
    insert_fix(
        &pg,
        FixRow {
            id: &other_fix,
            source_run: &other_r0,
            reflection_run: &other_reflection,
            finding: None,
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: Some("effective"),
            evidence: None,
        },
    )
    .await;

    let written = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("evaluate_pending_fixes");
    let verdicts: HashMap<String, Option<String>> = written
        .iter()
        .map(|fix| (fix.id.clone(), fix.effectiveness.clone()))
        .collect();
    let expected: HashMap<String, Option<String>> = [
        // Re-queued and judged on its exposure: `sig-b` came back in r2.
        (unjudged.clone(), Some("ineffective".to_string())),
        (hand_no_finding.clone(), Some("inconclusive".to_string())),
    ]
    .into_iter()
    .collect();
    assert_eq!(verdicts, expected);

    // Re-queued, then left unevaluated; its evidence went with its verdict.
    let left = stored(&pg, &hand_unexposed).await;
    assert_eq!(
        (
            left.effectiveness,
            left.effectiveness_evidence,
            left.evaluated_at
        ),
        (None, None, None)
    );
    let kept = stored(&pg, &judged).await;
    assert_eq!(kept.effectiveness.as_deref(), Some("effective"));
    assert_eq!(
        kept.effectiveness_evidence.as_deref(),
        Some(judged_evidence)
    );
    assert_eq!(
        stored(&pg, &ineffective).await.effectiveness.as_deref(),
        Some("ineffective")
    );
    let untouched = stored(&pg, &other_fix).await;
    assert_eq!(
        (
            untouched.effectiveness.as_deref(),
            untouched.effectiveness_evidence.as_deref()
        ),
        (Some("effective"), None)
    );
    cleanup(&pg, &wf).await;
    cleanup(&pg, &other).await;
}

/// The source run is not its own exposure. A fix applied before its source
/// run completed (a manual trigger) sees that run land after `applied_at`;
/// counted, it is a third later run and its own finding a recurrence.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_the_source_run_is_not_its_own_exposure() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("own-run");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    // r0, the source run, completes AFTER the fixes are applied.
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-12T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-13T00:00:00Z")).await;
    insert_run(&pg, &id("r2"), &wf, Some("2026-01-14T00:00:00Z")).await;
    insert_finding(&pg, &id("f"), &id("r0"), Some(&id("sig"))).await;
    // A pending fix: two later runs, so it stays unevaluated.
    insert_fix(
        &pg,
        FixRow {
            id: &id("pending"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: Some(&id("f")),
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;
    // An `effective` stamp WITH evidence and an observable finding: only the
    // later-run count can re-queue it, and two later runs do.
    insert_fix(
        &pg,
        FixRow {
            id: &id("stamped"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: Some(&id("f")),
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: Some("effective"),
            evidence: Some("{\"later_runs\": 3}"),
        },
    )
    .await;

    let written = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("evaluate_pending_fixes");
    assert!(
        written.is_empty(),
        "two later runs judge nothing: {written:?}"
    );
    for name in ["pending", "stamped"] {
        let fix = stored(&pg, &id(name)).await;
        assert_eq!(
            (
                fix.effectiveness,
                fix.effectiveness_evidence,
                fix.evaluated_at
            ),
            (None, None, None),
            "{name}: the source run is not a later run"
        );
    }
    cleanup(&pg, &wf).await;
}

/// A signature the fix's own source run already carried is not "new since
/// `applied_at`", even when that run completed after the fix was applied
/// (a manual trigger): the source run is prior, never later.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_a_signature_its_source_run_carried_is_not_new_after_the_fix() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("source-signature");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    // r0, the source run, completes AFTER the fix is applied, carrying the
    // fixed finding (sig) and a second one (other).
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-12T00:00:00Z")).await;
    insert_finding(&pg, &id("f"), &id("r0"), Some(&id("sig"))).await;
    insert_finding(&pg, &id("g"), &id("r0"), Some(&id("other"))).await;
    // Three later runs; two of them show `other` again, none shows `sig`.
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-13T00:00:00Z")).await;
    insert_run(&pg, &id("r2"), &wf, Some("2026-01-14T00:00:00Z")).await;
    insert_run(&pg, &id("r3"), &wf, Some("2026-01-15T00:00:00Z")).await;
    insert_finding(&pg, &id("h1"), &id("r1"), Some(&id("other"))).await;
    insert_finding(&pg, &id("h2"), &id("r2"), Some(&id("other"))).await;
    insert_fix(
        &pg,
        FixRow {
            id: &id("fix"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: Some(&id("f")),
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;

    let written = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("evaluate_pending_fixes");
    let verdicts: Vec<_> = written
        .iter()
        .map(|fix| (fix.id.as_str(), fix.effectiveness.as_deref()))
        .collect();
    let fix_id = id("fix");
    assert_eq!(
        verdicts,
        vec![(fix_id.as_str(), Some("effective"))],
        "`other` predates the fix (its source run carried it), so two later runs of it are no regression"
    );
    cleanup(&pg, &wf).await;
}

/// The trend reads bind their cutoff as text and cast it in SQL (a Rust
/// `String` bound to a `timestamptz` comparison is refused by
/// postgres-types), and the two snapshot reads select the columns their
/// tables have and read `snapshot_at` as text. One row inside a 7-day
/// cutoff and one outside it, per read.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_the_trend_reads_apply_their_cutoff() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("trends");
    let id = |name: &str| format!("{wf}-{name}");
    let component = crate::reflection::architecture::normalize_component_path("src/wfv_p4.rs");
    let conn = pg.pool.get().await.expect("PG pool");
    for (name, age, score, rate, fixes) in [
        ("old", "30 days", 0.25_f64, 0.5_f64, 4_i32),
        ("new", "1 hour", 0.75_f64, 0.9_f64, 7_i32),
    ] {
        conn.execute(
            "INSERT INTO convergence_snapshots \
                 (id, workflow_name, convergence_score, consecutive_clean_runs, novelty_score, \
                  effective_fix_rate, change_velocity, total_fixes, effective_fixes, snapshot_at) \
             VALUES ($1, $2, $3, 0, 0.0, $4, 0.0, $5, 0, NOW() - $6::TEXT::interval)",
            &[&id(name), &wf, &score, &rate, &fixes, &age],
        )
        .await
        .expect("insert convergence_snapshots fixture");
        conn.execute(
            "INSERT INTO component_health_snapshots \
                 (id, workflow_name, component_path, health_score, fix_count, snapshot_at) \
             VALUES ($1, $2, $3, $4, $5, NOW() - $6::TEXT::interval)",
            &[&id(name), &wf, &component, &score, &fixes, &age],
        )
        .await
        .expect("insert component_health_snapshots fixture");
    }

    let trends = pg
        .get_workflow_trends(&wf, Some("7d"))
        .await
        .expect("get_workflow_trends with a cutoff");
    assert_eq!(trends.snapshot_count, 1, "only the snapshot inside 7 days");
    assert_eq!(trends.convergence[0].value, 0.75);
    assert_eq!(trends.fix_rate[0].value, 0.9);
    assert_eq!(trends.total_fixes[0].count, Some(7));
    assert!(!trends.convergence[0].timestamp.is_empty());
    let all = pg
        .get_workflow_trends(&wf, None)
        .await
        .expect("get_workflow_trends without a cutoff");
    assert_eq!(all.snapshot_count, 2);

    let health = pg
        .get_component_trend(&wf, "src/wfv_p4.rs", Some("7d"))
        .await
        .expect("get_component_trend with a cutoff");
    assert_eq!(
        health.health_scores.len(),
        1,
        "only the snapshot inside 7 days"
    );
    assert_eq!(health.health_scores[0].value, 0.75);
    assert_eq!(health.fix_counts[0].count, Some(7));

    // Two fixes of the workflow: one created an hour ago, one in January.
    let reflection = insert_reflection_run(&pg, &wf).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    let recent = (chrono::Utc::now() - chrono::Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    for (name, created) in [
        ("old-fix", "2026-01-10T00:00:00Z"),
        ("new-fix", recent.as_str()),
    ] {
        insert_fix(
            &pg,
            FixRow {
                id: &id(name),
                source_run: &id("r0"),
                reflection_run: &reflection,
                finding: None,
                applied_at: created,
                effectiveness: Some("effective"),
                evidence: None,
            },
        )
        .await;
    }
    let over_time = pg
        .get_effectiveness_over_time(&wf, "day", Some("7d"))
        .await
        .expect("get_effectiveness_over_time with a cutoff");
    let totals: u32 = over_time.buckets.iter().map(|bucket| bucket.total).sum();
    assert_eq!(totals, 1, "only the fix created inside 7 days");

    conn.execute(
        "DELETE FROM convergence_snapshots WHERE workflow_name = $1",
        &[&wf],
    )
    .await
    .expect("delete the convergence fixtures");
    conn.execute(
        "DELETE FROM component_health_snapshots WHERE workflow_name = $1",
        &[&wf],
    )
    .await
    .expect("delete the component fixtures");
    cleanup(&pg, &wf).await;
}

/// `store_convergence_snapshot`, the one writer of `convergence_snapshots`,
/// stores a row `get_workflow_trends` reads back inside a 7-day cutoff.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_a_stored_convergence_snapshot_reads_back_in_the_trends() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("snapshot");
    let metrics = crate::reflection::prediction::ConvergenceMetrics {
        score: 0.625,
        consecutive_clean_runs: 3,
        novelty_score: 0.125,
        effective_fix_rate: 0.5,
        change_velocity: 0.25,
        total_fixes: 8,
        effective_fixes: 4,
    };
    // `project`, not the column's default `workflow`, so the bind is what is read back.
    pg.store_convergence_snapshot(&wf, None, "project", &metrics)
        .await
        .expect("store_convergence_snapshot");

    let trends = pg
        .get_workflow_trends(&wf, Some("7d"))
        .await
        .expect("get_workflow_trends with a cutoff");
    assert_eq!(
        trends.snapshot_count, 1,
        "the stored snapshot is inside 7 days"
    );
    assert_eq!(trends.convergence[0].value, 0.625);
    assert_eq!(trends.fix_rate[0].value, 0.5);
    assert_eq!(trends.velocity[0].value, 0.25);
    assert_eq!(trends.total_fixes[0].count, Some(8));

    let conn = pg.pool.get().await.expect("PG pool");
    let row = conn
        .query_one(
            "SELECT consecutive_clean_runs, effective_fixes, novelty_score, scope \
               FROM convergence_snapshots WHERE workflow_name = $1",
            &[&wf],
        )
        .await
        .expect("read the stored snapshot");
    let stored: (i32, i32, f64, String) = (
        row.try_get(0).expect("consecutive_clean_runs"),
        row.try_get(1).expect("effective_fixes"),
        row.try_get(2).expect("novelty_score"),
        row.try_get(3).expect("scope"),
    );
    assert_eq!(stored, (3, 4, 0.125, "project".to_string()));
    conn.execute(
        "DELETE FROM convergence_snapshots WHERE workflow_name = $1",
        &[&wf],
    )
    .await
    .expect("delete the stored snapshot");
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_the_hand_set_door_stamps_evaluated_at_in_sql() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("hand-set");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_fix(
        &pg,
        FixRow {
            id: &id("fix"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: None,
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;

    pg.update_fix_effectiveness(&id("fix"), "inconclusive", Some(r#"{"set_by":"operator"}"#))
        .await
        .expect("the hand-set door writes");
    let fix = stored(&pg, &id("fix")).await;
    assert_eq!(fix.effectiveness.as_deref(), Some("inconclusive"));
    assert_eq!(
        fix.effectiveness_evidence.as_deref(),
        Some(r#"{"set_by":"operator"}"#)
    );
    let conn = pg.pool.get().await.expect("PG pool");
    let fresh: bool = conn
        .query_one(
            "SELECT evaluated_at > NOW() - INTERVAL '5 minutes' \
             FROM reflection_fixes WHERE id = $1",
            &[&id("fix")],
        )
        .await
        .expect("read evaluated_at")
        .try_get(0)
        .expect("a stamped evaluated_at compares to a bool");
    assert!(fresh, "evaluated_at is the database's own NOW()");

    let missing = pg
        .update_fix_effectiveness(&id("no-such-fix"), "inconclusive", None)
        .await
        .expect_err("an unknown fix is an error");
    assert!(missing.contains("not found"), "{missing}");
    cleanup(&pg, &wf).await;
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_save_reflection_fix_stamps_applied_and_created_at_in_sql() {
    // `save_reflection_fix` mirrors a stored fix into tenant memory; a
    // round-trip must not send one off the box.
    let _no_egress = crate::egress::test_support::pin(
        crate::egress::Flow::TranscriptSync,
        crate::egress::Level::Off,
    );
    let pg = disposable_pg().await;
    let wf = unique_workflow("save-fix");
    let reflection = insert_reflection_run(&pg, &wf).await;
    let source = format!("{wf}-r0");
    insert_run(&pg, &source, &wf, Some("2026-01-01T00:00:00Z")).await;
    let input = CreateReflectionFixInput {
        source_task_run_id: source.clone(),
        reflection_task_run_id: reflection.clone(),
        source_finding_id: None,
        source_knowledge_id: None,
        fix_type: "context_addition".to_string(),
        // Unique to the run: the content-hash dedup spans workflows.
        fix_description: format!("wfv-p4 fixture fix for {wf}"),
        file_changed: None,
        old_value: None,
        new_value: None,
        confidence: "medium".to_string(),
        source_agent: None,
        reasoning: None,
        alternatives_considered: None,
    };

    let saved = pg
        .save_reflection_fix(&input)
        .await
        .expect("save_reflection_fix stores the fix");
    assert_eq!(saved.status, "applied");
    assert_eq!(saved.effectiveness, None);
    assert_eq!(
        saved.applied_at, saved.created_at,
        "both are the same statement's NOW()"
    );
    let conn = pg.pool.get().await.expect("PG pool");
    let fresh: bool = conn
        .query_one(
            "SELECT applied_at > NOW() - INTERVAL '5 minutes' \
             FROM reflection_fixes WHERE id = $1",
            &[&saved.id],
        )
        .await
        .expect("read applied_at")
        .try_get(0)
        .expect("applied_at compares to a bool");
    assert!(fresh, "applied_at is the database's own NOW()");

    let again = pg
        .save_reflection_fix(&input)
        .await
        .expect("a duplicate answers the stored fix");
    assert_eq!(again.id, saved.id, "the content-hash dedup still holds");
    cleanup(&pg, &wf).await;
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_save_fix_application_stamps_applied_at_in_sql() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("fix-application");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, None).await;
    insert_fix(
        &pg,
        FixRow {
            id: &id("fix"),
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: None,
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;

    let application = pg
        .save_fix_application(&id("fix"), &id("r1"), Some(&id("sig")), "shown")
        .await
        .expect("save_fix_application stores the application");
    let conn = pg.pool.get().await.expect("PG pool");
    let row = conn
        .query_one(
            "SELECT outcome, applied_at > NOW() - INTERVAL '5 minutes' AS fresh \
             FROM fix_applications WHERE id = $1",
            &[&application],
        )
        .await
        .expect("read the application back");
    let outcome: Option<String> = row.try_get("outcome").expect("outcome is text");
    let fresh: bool = row.try_get("fresh").expect("applied_at compares to a bool");
    assert_eq!(outcome.as_deref(), Some("shown"));
    assert!(fresh, "applied_at is the database's own NOW()");
    let reuse_count: Option<i32> = conn
        .query_one(
            "SELECT reuse_count FROM reflection_fixes WHERE id = $1",
            &[&id("fix")],
        )
        .await
        .expect("read reuse_count")
        .try_get(0)
        .expect("reuse_count is an integer");
    assert_eq!(reuse_count, Some(1));
    cleanup(&pg, &wf).await;
}

/// An unexposed fix now STAYS pending, so a hundred of them must not crowd
/// an older, exposed fix out of the evaluator's 100-row read.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_a_hundred_unexposed_fixes_do_not_starve_an_exposed_one() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("starvation");
    let id = |name: &str| format!("{wf}-{name}");
    let reflection = insert_reflection_run(&pg, &wf).await;
    insert_run(&pg, &id("r0"), &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_run(&pg, &id("r1"), &wf, Some("2026-01-11T00:00:00Z")).await;
    insert_run(&pg, &id("r2"), &wf, Some("2026-01-12T00:00:00Z")).await;
    insert_run(&pg, &id("r3"), &wf, Some("2026-01-13T00:00:00Z")).await;
    insert_finding(&pg, &id("f"), &id("r0"), Some(&id("sig"))).await;
    let exposed = id("fix-exposed");
    insert_fix(
        &pg,
        FixRow {
            id: &exposed,
            source_run: &id("r0"),
            reflection_run: &reflection,
            finding: Some(&id("f")),
            applied_at: "2026-01-10T00:00:00Z",
            effectiveness: None,
            evidence: None,
        },
    )
    .await;
    // A hundred fixes applied after the workflow's last run.
    for n in 0..100 {
        insert_fix(
            &pg,
            FixRow {
                id: &id(&format!("fix-unexposed-{n:03}")),
                source_run: &id("r0"),
                reflection_run: &reflection,
                finding: Some(&id("f")),
                applied_at: "2026-01-14T00:00:00Z",
                effectiveness: None,
                evidence: None,
            },
        )
        .await;
    }

    let written = pg
        .evaluate_pending_fixes(&wf)
        .await
        .expect("evaluate_pending_fixes");
    assert_eq!(
        written
            .iter()
            .map(|fix| fix.id.as_str())
            .collect::<Vec<_>>(),
        vec![exposed.as_str()],
        "the exposed fix is judged; none of the unexposed hundred is"
    );
    assert_eq!(written[0].effectiveness.as_deref(), Some("effective"));
    cleanup(&pg, &wf).await;
}
