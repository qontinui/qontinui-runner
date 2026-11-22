//! Tests of the knowledge ranker and change velocity (plan
//! 2026-11-09-reflection-reports-only-what-it-judged-and-the-dead-verifier-leaves-no-residue,
//! Phase 3). The pure arms run everywhere. The `pg_*` arms are `#[ignore]`d
//! round-trips against the disposable database `tests::disposable_pg`
//! describes; run them with
//! `cargo-guard.sh test -- --ignored database::pg::reflection::ranker_tests`.
use super::knowledge_ranker::{
    effectiveness_weight, recency_weight, require_window, velocity_factor_for,
    velocity_from_count, NORMALIZED_FILE_CHANGED_SQL, SCORED_KNOWLEDGE_SQL,
};
use super::tests::{cleanup, disposable_pg, insert_reflection_run, insert_run, unique_workflow};
use super::*;
use crate::database::pg::schema_oracle::{aliased_identifiers, declared_columns};
use crate::reflection::architecture::normalize_component_path;
use crate::reflection::prediction::ScoredKnowledge;
use chrono::{Duration, TimeZone, Utc};

// ---------------------------------------------------------------------------
// Pure arms
// ---------------------------------------------------------------------------

#[test]
fn weight_by_linked_fix_verdict() {
    // Unlinked: neither vouched for nor against.
    assert_eq!(effectiveness_weight(false, None), Ok(0.7));
    // A verdict on an unlinked entry cannot exist; the link decides first.
    assert_eq!(effectiveness_weight(false, Some("effective")), Ok(0.7));
    // Linked, by verdict.
    assert_eq!(effectiveness_weight(true, Some("effective")), Ok(1.0));
    assert_eq!(effectiveness_weight(true, Some("ineffective")), Ok(0.0));
    assert_eq!(effectiveness_weight(true, Some("caused_regression")), Ok(0.0));
    assert_eq!(effectiveness_weight(true, Some("inconclusive")), Ok(0.5));
    // Linked and not yet judged.
    assert_eq!(effectiveness_weight(true, None), Ok(0.5));
}

#[test]
fn a_linked_fix_with_an_unrecognised_verdict_is_an_error_not_a_default() {
    let err = effectiveness_weight(true, Some("mostly_fine"))
        .expect_err("an unrecognised verdict must not map to a weight");
    assert!(err.contains("mostly_fine"), "the error names the verdict: {err}");
}

#[test]
fn recency_halves_after_ten_days_and_never_exceeds_one() {
    let now = Utc.with_ymd_and_hms(2026, 11, 9, 12, 0, 0).unwrap();
    assert_eq!(recency_weight(now, now), 1.0);
    assert_eq!(recency_weight(now, now - Duration::days(10)), 0.5);
    assert_eq!(recency_weight(now, now - Duration::days(90)), 0.1);
    // Stamped ahead of the clock: counts as new instead of dividing by zero
    // (a 10-day lead made the old denominator 0).
    assert_eq!(recency_weight(now, now + Duration::days(10)), 1.0);
    assert_eq!(recency_weight(now, now + Duration::days(30)), 1.0);
}

#[test]
fn velocity_factor_is_the_reciprocal_of_one_plus_velocity() {
    assert_eq!(velocity_factor_for(0.0), 1.0);
    assert_eq!(velocity_factor_for(1.0), 0.5);
    assert_eq!(velocity_factor_for(4.0), 0.2);
}

#[test]
fn velocity_over_zero_one_and_many_fixes_and_a_window_of_zero() {
    assert_eq!(velocity_from_count(0, 5), Ok(0.0));
    assert_eq!(velocity_from_count(1, 5), Ok(0.2));
    assert_eq!(velocity_from_count(7, 5), Ok(1.4));
    assert_eq!(velocity_from_count(3, 1), Ok(3.0));
    let err = velocity_from_count(3, 0).expect_err("a window of 0 has no rate");
    assert!(err.contains("window"), "{err}");
    assert!(velocity_from_count(0, 0).is_err(), "zero fixes does not excuse a zero window");
    assert!(require_window(0).is_err());
    assert!(require_window(1).is_ok());
}

/// The gate the ranker's query never had: every `tk.`, `tr.` and `rf.` column
/// it names is one the generated schema declares. It derives the names from
/// the query text, so a column added back (`tk.last_validated_at` was the one
/// that made the ranker fail on every database) turns it red.
#[test]
fn ranker_query_names_only_real_columns() {
    for (alias, table) in [
        ("tk", "project.task_knowledge"),
        ("tr", "project.task_runs"),
        ("rf", "project.reflection_fixes"),
    ] {
        let declared = declared_columns(table);
        let referenced = aliased_identifiers(SCORED_KNOWLEDGE_SQL, alias);
        assert!(
            !referenced.is_empty(),
            "the ranker query references no `{alias}.` column: the alias changed or the columns \
             stopped being qualified, and this test can no longer see them"
        );
        for column in referenced {
            assert!(
                declared.contains(&column),
                "the ranker query names `{alias}.{column}`, which {table} does not have. \
                 Declared columns: {declared:?}"
            );
        }
    }
}

/// The link the ranker joins is spelled exactly this way (Done-when block 1
/// searches for it), and the two columns it used to select are gone.
#[test]
fn ranker_query_joins_the_fix_through_source_knowledge_id() {
    assert!(SCORED_KNOWLEDGE_SQL.contains("rf.source_knowledge_id = tk.id"));
    assert!(!SCORED_KNOWLEDGE_SQL.contains("last_validated_at"));
    assert!(!SCORED_KNOWLEDGE_SQL.contains("reflection_fix_id"));
    assert!(!SCORED_KNOWLEDGE_SQL.contains("target_component"));
}

// ---------------------------------------------------------------------------
// Round-trips against the disposable database
// ---------------------------------------------------------------------------

async fn insert_knowledge(pg: &PgDb, id: &str, run: &str, created_at: chrono::DateTime<Utc>) {
    let conn = pg.pool.get().await.expect("PG pool");
    conn.execute(
        "INSERT INTO task_knowledge (id, task_run_id, category, content, created_at) \
         VALUES ($1, $2, 'finding', $3, $4)",
        &[&id, &run, &format!("wfv-p3 knowledge {id}"), &created_at],
    )
    .await
    .expect("insert task_knowledge fixture");
}

/// One fix. `knowledge` links it to an entry; `file` is `file_changed`.
struct Fix<'a> {
    id: &'a str,
    source_run: &'a str,
    reflection_run: &'a str,
    knowledge: Option<&'a str>,
    file: Option<&'a str>,
    effectiveness: Option<&'a str>,
    applied_at: chrono::DateTime<Utc>,
}

async fn insert_fix(pg: &PgDb, fix: Fix<'_>) {
    let conn = pg.pool.get().await.expect("PG pool");
    conn.execute(
        "INSERT INTO reflection_fixes \
             (id, source_task_run_id, reflection_task_run_id, source_knowledge_id, fix_type, \
              fix_description, file_changed, status, effectiveness, applied_at, created_at) \
         VALUES ($1, $2, $3, $4, 'context_addition', 'wfv-p3 fixture fix', $5, 'applied', $6, $7, $7)",
        &[
            &fix.id,
            &fix.source_run,
            &fix.reflection_run,
            &fix.knowledge,
            &fix.file,
            &fix.effectiveness,
            &fix.applied_at,
        ],
    )
    .await
    .expect("insert reflection_fixes fixture");
}

/// Remove what an arm created. `task_knowledge` has no foreign key to the
/// run, so it goes explicitly; the fixes cascade from their runs.
async fn clean(pg: &PgDb, workflow: &str) {
    let conn = pg.pool.get().await.expect("PG pool");
    conn.execute(
        "DELETE FROM task_knowledge WHERE task_run_id IN \
             (SELECT id FROM task_runs WHERE workflow_name = $1)",
        &[&workflow],
    )
    .await
    .expect("delete the fixture knowledge");
    cleanup(pg, workflow).await;
}

fn entry<'a>(scored: &'a [ScoredKnowledge], id: &str) -> &'a ScoredKnowledge {
    scored
        .iter()
        .find(|s| s.knowledge_id == id)
        .unwrap_or_else(|| panic!("{id} is not among the ranked entries"))
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_sql_normalization_agrees_with_normalize_component_path() {
    let pg = disposable_pg().await;
    let conn = pg.pool.get().await.expect("PG pool");
    let query = format!(
        "SELECT {NORMALIZED_FILE_CHANGED_SQL} FROM (SELECT $1::text AS file_changed) t"
    );
    // (input, what both normalizers must produce)
    let literals = [
        ("src\\Main\\Lib.RS", "src/main/lib.rs"), // backslashes, mixed case
        ("C:\\Src\\Main.RS", "src/main.rs"),      // drive letter
        ("d:/Work/A.py", "work/a.py"),            // lowercase drive, slashes
        ("./Src/A.rs", "src/a.rs"),               // leading ./
        (".\\Src\\A.rs", "src/a.rs"),             // leading .\
        ("C:\\.\\a.rs", "a.rs"),                  // drive, then ./
        ("src/main.rs", "src/main.rs"),           // already normal
        ("é:/x", "é:/x"),                         // a multibyte first character is no drive
        ("a/./b.rs", "a/./b.rs"),                 // only a LEADING ./ goes
    ];
    for (raw, expected) in literals {
        assert_eq!(
            normalize_component_path(raw),
            expected,
            "normalize_component_path({raw:?})"
        );
        let row = conn
            .query_one(query.as_str(), &[&raw])
            .await
            .expect("normalize in SQL");
        let in_sql: String = row.try_get(0).expect("normalized text");
        assert_eq!(in_sql, expected, "the SQL expression on {raw:?}");
    }
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_ranker_returns_no_rows_for_an_empty_workflow() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("rank-empty");
    let scored = pg
        .score_knowledge_relevance(&wf, 10)
        .await
        .expect("the ranker runs against the real schema");
    assert!(scored.is_empty(), "{scored:?}");
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_ranker_scores_an_unlinked_entry_at_point_seven() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("rank-unlinked");
    let run = format!("{wf}-run");
    insert_run(&pg, &run, &wf, Some("2026-01-01T00:00:00Z")).await;
    let k = format!("{wf}-k");
    insert_knowledge(&pg, &k, &run, Utc::now()).await;

    let scored = pg.score_knowledge_relevance(&wf, 10).await;
    clean(&pg, &wf).await;
    let scored = scored.expect("the ranker runs");

    assert_eq!(scored.len(), 1, "{scored:?}");
    let only = &scored[0];
    assert_eq!(only.knowledge_id, k);
    assert_eq!(only.effectiveness_weight, 0.7);
    assert_eq!(only.velocity_factor, 1.0);
    assert!(
        (only.relevance_score - 0.7 * only.recency_weight).abs() < 1e-12,
        "{only:?}"
    );
    assert!(only.recency_weight > 0.99, "a fresh entry: {only:?}");
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_ranker_weights_one_linked_entry_per_verdict() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("rank-verdicts");
    let run = format!("{wf}-run");
    insert_run(&pg, &run, &wf, Some("2026-01-01T00:00:00Z")).await;
    let reflection = insert_reflection_run(&pg, &wf).await;
    let now = Utc::now();

    // (verdict, expected effectiveness weight)
    let arms: [(Option<&str>, f64); 5] = [
        (Some("effective"), 1.0),
        (Some("ineffective"), 0.0),
        (Some("caused_regression"), 0.0),
        (Some("inconclusive"), 0.5),
        (None, 0.5),
    ];
    let mut ids = Vec::new();
    for (n, (verdict, _)) in arms.iter().enumerate() {
        let k = format!("{wf}-k{n}");
        insert_knowledge(&pg, &k, &run, now).await;
        insert_fix(
            &pg,
            Fix {
                id: &format!("{wf}-fix{n}"),
                source_run: &run,
                reflection_run: &reflection,
                knowledge: Some(&k),
                // One fix per component, so velocity is 1 / window 5 = 0.2.
                file: Some(&format!("src/{wf}/component-{n}.rs")),
                effectiveness: *verdict,
                applied_at: now,
            },
        )
        .await;
        ids.push(k);
    }

    let scored = pg.score_knowledge_relevance(&wf, 10).await;
    clean(&pg, &wf).await;
    let scored = scored.expect("the ranker runs");

    assert_eq!(scored.len(), 5, "{scored:?}");
    for (k, (verdict, weight)) in ids.iter().zip(arms) {
        let s = entry(&scored, k);
        assert_eq!(s.effectiveness_weight, weight, "verdict {verdict:?}: {s:?}");
        assert!(
            (s.velocity_factor - 1.0 / 1.2).abs() < 1e-12,
            "one fix over a window of 5 is velocity 0.2: {s:?}"
        );
    }
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_ranker_ranks_newer_entries_first() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("rank-newer");
    let run = format!("{wf}-run");
    insert_run(&pg, &run, &wf, Some("2026-01-01T00:00:00Z")).await;
    let now = Utc::now();
    let (old, mid, new) = (
        format!("{wf}-old"),
        format!("{wf}-mid"),
        format!("{wf}-new"),
    );
    // Inserted oldest-first so insertion order cannot explain the result.
    insert_knowledge(&pg, &old, &run, now - Duration::days(30)).await;
    insert_knowledge(&pg, &mid, &run, now - Duration::days(10)).await;
    insert_knowledge(&pg, &new, &run, now - Duration::hours(1)).await;

    let scored = pg.score_knowledge_relevance(&wf, 10).await;
    // `limit` truncates after ranking.
    let top = pg.score_knowledge_relevance(&wf, 1).await;
    clean(&pg, &wf).await;
    let scored = scored.expect("the ranker runs");

    let order: Vec<&str> = scored.iter().map(|s| s.knowledge_id.as_str()).collect();
    assert_eq!(order, [new.as_str(), mid.as_str(), old.as_str()]);
    // 10 days old: 1 / (1 + 1.0); 30 days: 1 / (1 + 3.0); each unlinked, so x 0.7.
    assert!((entry(&scored, &mid).relevance_score - 0.35).abs() < 1e-9);
    assert!((entry(&scored, &old).relevance_score - 0.175).abs() < 1e-9);
    assert_eq!(top.expect("the ranker runs")[0].knowledge_id, new);
}

#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_velocity_over_zero_one_and_many_fixes_and_a_zero_window() {
    let pg = disposable_pg().await;
    let wf = unique_workflow("velocity");
    let run = format!("{wf}-run");
    insert_run(&pg, &run, &wf, Some("2026-01-01T00:00:00Z")).await;
    let reflection = insert_reflection_run(&pg, &wf).await;
    let component = format!("src/{wf}/main.rs");
    let t0 = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();

    // Zero fixes: a measured 0.
    let zero = pg.compute_change_velocity(&component, 5).await;
    assert_eq!(zero, Ok(0.0));

    // One fix, stored under a different spelling of the same component.
    insert_fix(
        &pg,
        Fix {
            id: &format!("{wf}-f0"),
            source_run: &run,
            reflection_run: &reflection,
            knowledge: None,
            file: Some(&format!("C:\\SRC\\{}\\Main.RS", wf.to_uppercase())),
            effectiveness: None,
            applied_at: t0,
        },
    )
    .await;
    let one = pg.compute_change_velocity(&component, 5).await;

    // Two more, at later instants. Window 2 covers the last two instants.
    for n in 1..=2 {
        insert_fix(
            &pg,
            Fix {
                id: &format!("{wf}-f{n}"),
                source_run: &run,
                reflection_run: &reflection,
                knowledge: None,
                file: Some(&component),
                effectiveness: None,
                applied_at: t0 + Duration::days(n),
            },
        )
        .await;
    }
    let many_wide = pg.compute_change_velocity(&component, 5).await;
    let many_narrow = pg.compute_change_velocity(&component, 2).await;
    // A different component reads none of them.
    let other = pg
        .compute_change_velocity(&format!("src/{wf}/other.rs"), 5)
        .await;
    // A window of 0 is refused even though fixes exist.
    let refused = pg.compute_change_velocity(&component, 0).await;
    clean(&pg, &wf).await;

    assert_eq!(one, Ok(0.2));
    assert_eq!(many_wide, Ok(0.6));
    assert_eq!(many_narrow, Ok(1.0));
    assert_eq!(other, Ok(0.0));
    assert!(refused.is_err(), "{refused:?}");
}

/// The ranker's and velocity's reads fail when the role cannot read
/// `reflection_fixes`. Before Phase 3 velocity returned a 0 and the ranker's
/// failure was swallowed by its caller. A control with the grant in place
/// shows the `Err` comes from the missing grant and from nothing else.
#[tokio::test]
#[ignore = "needs a disposable Postgres (DATABASE_URL, never qontinui_db) carrying the project schema"]
async fn pg_both_reads_err_without_select_on_reflection_fixes() {
    let admin = disposable_pg().await;
    let wf = unique_workflow("no-select");
    let run = format!("{wf}-run");
    insert_run(&admin, &run, &wf, Some("2026-01-01T00:00:00Z")).await;
    insert_knowledge(&admin, &format!("{wf}-k"), &run, Utc::now()).await;

    let role = format!("rt_p3_{}", uuid::Uuid::new_v4().simple());
    {
        let conn = admin.pool.get().await.expect("PG pool");
        for statement in [
            format!("CREATE ROLE {role}"),
            format!("GRANT USAGE ON SCHEMA project, public TO {role}"),
            format!("GRANT SELECT ON project.task_knowledge, project.task_runs TO {role}"),
        ] {
            conn.batch_execute(&statement).await.expect("set up the role");
        }
    }

    // The same database, with every connection acting as the role.
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL (disposable_pg checked it)");
    let separator = if url.contains('?') { '&' } else { '?' };
    let as_role = format!("{url}{separator}options=-c%20role%3D{role}");
    let limited = PgDb {
        pool: PgDb::build_pool(&as_role).expect("a pool that connects as the role"),
    };

    let denied_rank = limited.score_knowledge_relevance(&wf, 10).await;
    let denied_velocity = limited.compute_change_velocity("src/x.rs", 5).await;

    {
        let conn = admin.pool.get().await.expect("PG pool");
        conn.batch_execute(&format!(
            "GRANT SELECT ON project.reflection_fixes TO {role}"
        ))
        .await
        .expect("grant the control");
    }
    let control_rank = limited.score_knowledge_relevance(&wf, 10).await;
    let control_velocity = limited.compute_change_velocity("src/x.rs", 5).await;

    // The role's connections hold privileges: close them before dropping it.
    drop(limited);
    {
        let conn = admin.pool.get().await.expect("PG pool");
        conn.batch_execute(&format!("DROP OWNED BY {role}; DROP ROLE {role}"))
            .await
            .expect("drop the role");
    }
    clean(&admin, &wf).await;

    let rank_err = denied_rank.expect_err("the ranker must not read past a denied table");
    assert!(rank_err.contains("permission denied"), "{rank_err}");
    let velocity_err = denied_velocity.expect_err("velocity must not read a denied table as 0");
    assert!(velocity_err.contains("permission denied"), "{velocity_err}");
    assert_eq!(control_rank.expect("with the grant the ranker runs").len(), 1);
    assert_eq!(control_velocity, Ok(0.0));
}
