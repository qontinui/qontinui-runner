//! The three `since` SQL constants — `EDGE_READ_SINCE_SQL`,
//! `EDGE_TIMELINE_READ_SINCE_SQL` and `OLDER_EDGE_GROUPS_SQL` — executed on
//! PostgreSQL through their production callers, [`read_ledger_since`] and
//! [`read_timed_edges`] (plan
//! `2026-11-09-runner-postgres-backed-tests-never-run-so-hand-verified-sql-is-unretained`,
//! Phase 2).
//!
//! qontinui-runner#2759 checked these constants by hand on an ephemeral
//! Postgres and kept the result only in its PR body; CI pinned them by their
//! text alone (`tests::a_since_read_partitions_the_ledger_and_hides_no_row`).
//! These tests are that check, retained. Each runs on a fresh database of its
//! own from the test harness (`PgDb::new_for_test`), which carries the full
//! canonical schema, seeds the same ledger ([`seeded`]) and asserts one
//! property, lettered as the plan letters them:
//!
//! - (a) the partition is complete: the rows read in full plus the rows the
//!   older groups stand for are the app's rows, each where the partition puts
//!   it;
//! - (b) the row exactly at `since` is read in full and is in no group;
//! - (c) no row of another app reaches either read;
//! - (d) two group reads are identical, ordered by `min(id::text)`;
//! - (e) a live and a withdrawn copy of one transition are two groups;
//! - (f) every graph answer over the since read reaches the verdict it
//!   reaches over every row, and the three corrected row totals are equal;
//! - (g) Postgres accepts every edge-case bind [`since_bind`] makes.
//!
//! Each assertion the plan names a mutation for was seen to fail against it,
//! with its own message: (b) when `<` became `<=` in `OLDER_EDGE_GROUPS_SQL`'s
//! partition, (c) when `e.app_id = $1` became `$1::text IS NOT NULL` in the
//! three since queries, and (e) when `(e.invalidated_at IS NOT NULL)` left that
//! query's `GROUP BY`. (a), (f) and (g) went red under all three; (d), which
//! asserts only stability and order, under none.
//!
//! (f)'s verdict (`test_fixtures::verdict`) reads each conclusion by identity,
//! and was seen to need to. When the full since read handed every affordance
//! row back under another `targetFingerprint`, (f) failed on the graph answer's
//! edges alone; the verdict it replaced, which compared the graph's nodes by
//! their number and read no edge, passed it. An older group never adds a node
//! or an edge — the filter excludes it first — so a wrong representative shows
//! in (f) only through the doubt it leaves: a representative naming its
//! `fromNode` as its `toNode` failed (f) on the graph answer's unknown
//! destinations, but one doing so only for `changed` groups left (f) green
//! (every prior-knowledge candidate here is unsettled already), and (a), (b),
//! (e) and (g) caught it. The verdict now also reads each graph node's open
//! frontier entries and unconfirmed destination and each unexplored node's two
//! flags, and (f) is still green under that last mutation.
//!
//! Every mutation above was a temporary edit to `read.rs`, reverted after its
//! run, so its runs are kept with the pull request that added this module, not
//! in the tree. Each ran before this branch was rebased onto `main` and again
//! on the rebased branch, against the verdict as it stands now, with the same
//! result; only the runs before the rebase also tried the replaced verdict.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use qontinui_types::journey::{build_graph, JourneyEdgeObservation, JourneyFilter, JourneyGraph};
use qontinui_vision_core::ChangeTimeline;

use super::{read_ledger, read_ledger_since, read_timed_edges, since_bind, LedgerRead, TimedRead};
use crate::database::pg::PgDb;
use crate::journey::capture::RUNNER_BUILD_ID;
use crate::journey::test_fixtures::{assert_same_conclusions, edge, key, timeline, writing};

// -- The seeded ledger --------------------------------------------------------

/// The app every read here is for.
const APP: &str = "journey-pg-app";

/// An app sharing the ledger, none of whose rows may reach a read for [`APP`].
const OTHER_APP: &str = "journey-pg-other-app";

/// The instant every read but (g)'s is partitioned at, as the writer renders
/// it (`SecondsFormat::Micros`).
const SINCE: &str = "2026-10-01T00:00:00.000000Z";

/// The one row stored with a timeline.
const TIMED_ROW: u32 = 4;

fn instant(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .unwrap_or_else(|e| panic!("fixture instant {rfc3339}: {e}"))
        .with_timezone(&Utc)
}

fn since() -> DateTime<Utc> {
    instant(SINCE)
}

/// Row `n`'s id. Lowercase, as `id::text` renders a uuid.
fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

/// `template` as row `n` of `app`, observed at `at`, stamped with the build id
/// the writer stamps (`capture::finalize`).
fn row(app: &str, n: u32, at: &str, template: &JourneyEdgeObservation) -> JourneyEdgeObservation {
    let mut e = template.clone();
    e.id = id(n);
    e.app_id = app.to_string();
    e.observed_at = at.to_string();
    e.runner_build_id = RUNNER_BUILD_ID.to_string();
    e
}

/// `e`, withdrawn as a bulk invalidation stamps a row: when, why, by whom and
/// under which token (the same four columns `state_discovery::invalidate`
/// sets on `co_occurrence_observations`).
fn withdrawn(mut e: JourneyEdgeObservation) -> JourneyEdgeObservation {
    e.invalidated_at = Some("2026-09-20T08:30:00.000000Z".into());
    e.invalidated_reason = Some("recorded against a broken build".into());
    e.invalidated_by = Some("operator".into());
    e.invalidation_token = Some("5b0c3f8e-6a3d-4f0e-9d7b-2c1a4e8f9b60".into());
    e
}

/// The app's 23 rows: 5 at or after [`SINCE`], 18 before it in 9 transitions.
///
/// The older rows' ids are chosen so that the groups' smallest ids run
/// OPPOSITE to the order their transitions sort in (A's group has the largest,
/// Z's the smallest), so only the SQL's `ORDER BY min(e.id::text)` puts them in
/// id order.
fn subject_rows() -> Vec<JourneyEdgeObservation> {
    let a_b = edge(0, "a", Some("b"), "affordance", "changed", "fp-a1");
    let a_d = edge(0, "a", Some("d"), "affordance", "changed", "fp-a2");
    let a_end = edge(0, "a", Some("end"), "affordance", "changed", "fp-a3");
    let a_u_by_url = edge(0, "a", Some("u"), "push", "changed", "fp-a4");
    let a_z = edge(0, "a", Some("z"), "affordance", "changed", "fp-a5");
    let b_c = edge(0, "b", Some("c"), "affordance", "changed", "fp-b");
    let f_g_by_url = edge(0, "f", Some("g"), "push", "changed", "fp-f");
    // `no_change` across two nodes fails validate(): an untrusted row.
    let j_k_untrusted = edge(0, "j", Some("k"), "affordance", "no_change", "fp-j");
    let p_unobserved = edge(0, "p", None, "affordance", "to_node_unobserved", "fp-p");
    let v_w_untrusted = edge(0, "v", Some("w"), "affordance", "no_change", "fp-v");
    let x_u = edge(0, "x", Some("u"), "affordance", "changed", "fp-x");
    let z_dead_click = edge(0, "z", Some("z"), "affordance", "no_change", "fp-z");
    vec![
        // At or after `since`: read in full.
        // (b) Exactly at `since`.
        row(APP, 1, SINCE, &a_b),
        // (g) Exactly at the floor of a nanosecond `since`.
        row(APP, 2, "2026-10-01T10:00:00.123456Z", &a_d),
        // (g) Just before a leap second, and one second after it.
        row(APP, 3, "2026-10-01T10:00:59.000000Z", &a_end),
        row(APP, TIMED_ROW, "2026-10-01T10:01:00.000000Z", &a_u_by_url),
        row(APP, 5, "2026-10-02T00:00:00.000000Z", &a_z),
        // Before `since`: read as groups.
        // (b) The transition of the row at `since`, the last of these one
        // microsecond before it.
        row(APP, 109, "2026-09-15T12:00:00.000000Z", &a_b),
        row(APP, 111, "2026-09-30T23:59:59.999999Z", &a_b),
        // (e) B's only exit: three live copies and two withdrawn ones.
        row(APP, 108, "2026-09-01T00:01:00.000000Z", &b_c),
        row(APP, 112, "2026-09-01T00:02:00.000000Z", &b_c),
        row(APP, 119, "2026-09-01T00:03:00.000000Z", &b_c),
        withdrawn(row(APP, 107, "2026-09-02T00:01:00.000000Z", &b_c)),
        withdrawn(row(APP, 113, "2026-09-02T00:02:00.000000Z", &b_c)),
        // A URL arrival.
        row(APP, 106, "2026-09-03T00:00:00.000000Z", &f_g_by_url),
        row(APP, 114, "2026-09-03T01:00:00.000000Z", &f_g_by_url),
        // Untrusted rows, live and withdrawn.
        row(APP, 105, "2026-09-04T00:00:00.000000Z", &j_k_untrusted),
        row(APP, 115, "2026-09-04T01:00:00.000000Z", &j_k_untrusted),
        withdrawn(row(APP, 103, "2026-09-05T00:00:00.000000Z", &v_w_untrusted)),
        withdrawn(row(APP, 116, "2026-09-05T01:00:00.000000Z", &v_w_untrusted)),
        // A close whose destination is unknown: a group of one.
        row(APP, 104, "2026-09-06T00:00:00.000000Z", &p_unobserved),
        // An older click into U, which the kept rows reach only by URL.
        row(APP, 102, "2026-09-07T00:00:00.000000Z", &x_u),
        row(APP, 117, "2026-09-07T01:00:00.000000Z", &x_u),
        // A dead click: a genuine negative.
        row(APP, 101, "2026-09-08T00:00:00.000000Z", &z_dead_click),
        row(APP, 118, "2026-09-08T01:00:00.000000Z", &z_dead_click),
    ]
}

/// The other app's 5 rows: copies of the subject's transitions on both sides
/// of `since`, one of them exactly at it.
fn other_rows() -> Vec<JourneyEdgeObservation> {
    let a_b = edge(0, "a", Some("b"), "affordance", "changed", "fp-a1");
    let b_c = edge(0, "b", Some("c"), "affordance", "changed", "fp-b");
    let j_k_untrusted = edge(0, "j", Some("k"), "affordance", "no_change", "fp-j");
    vec![
        row(OTHER_APP, 201, SINCE, &a_b),
        row(OTHER_APP, 202, "2026-10-01T12:00:00.000000Z", &b_c),
        row(OTHER_APP, 203, "2026-09-01T00:05:00.000000Z", &b_c),
        row(OTHER_APP, 204, "2026-09-01T00:06:00.000000Z", &b_c),
        row(
            OTHER_APP,
            205,
            "2026-09-04T02:00:00.000000Z",
            &j_k_untrusted,
        ),
    ]
}

/// The timeline stored with [`TIMED_ROW`]: what the writer stores, the
/// `for_ledger` projection.
fn stored_timeline() -> ChangeTimeline {
    timeline(vec![], 120).for_ledger()
}

/// The timeline row `e` was stored with, if any.
fn timeline_of(e: &JourneyEdgeObservation) -> Option<ChangeTimeline> {
    (e.id == id(TIMED_ROW)).then(stored_timeline)
}

/// Every column of `project.journey_edge_observations`
/// (`schema.pg.sql.generated`), bound with the writer's casts
/// (`capture::EDGE_WRITE_SQL`). Not the writer itself: it validates each row,
/// so it refuses the untrusted ones, and it never writes a withdrawal.
const INSERT_EDGE_SQL: &str = "INSERT INTO project.journey_edge_observations \
     (id, observed_at, app_id, app_version, runner_build_id, runner_instance, \
      run_id, run_kind, from_node, to_node, trigger, outcome, timeline, \
      invalidated_at, invalidated_reason, invalidated_by, invalidation_token) \
     VALUES ($1::text::uuid, $2::text::timestamptz, $3, $4, $5, $6, \
             $7, $8, $9::jsonb, $10::jsonb, $11::jsonb, $12, $13::jsonb, \
             $14::text::timestamptz, $15, $16, $17)";

async fn insert(conn: &deadpool_postgres::Object, e: &JourneyEdgeObservation) {
    let from_node = serde_json::to_value(&e.from_node).expect("a node serializes");
    // SQL NULL for an unobserved destination, never a JSON `null`.
    let to_node = e
        .to_node
        .as_ref()
        .map(|n| serde_json::to_value(n).expect("a node serializes"));
    let trigger = serde_json::to_value(&e.trigger).expect("a trigger serializes");
    let timeline_json =
        timeline_of(e).map(|t| serde_json::to_value(t).expect("a timeline serializes"));
    let run_kind = e.run_kind.as_str();
    let outcome = e.outcome.as_str();
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 17] = [
        &e.id,
        &e.observed_at,
        &e.app_id,
        &e.app_version,
        &e.runner_build_id,
        &e.runner_instance,
        &e.run_id,
        &run_kind,
        &from_node,
        &to_node,
        &trigger,
        &outcome,
        &timeline_json,
        &e.invalidated_at,
        &e.invalidated_reason,
        &e.invalidated_by,
        &e.invalidation_token,
    ];
    if let Err(err) = conn.execute(INSERT_EDGE_SQL, &params).await {
        panic!(
            "{}",
            crate::database::pg::pg_err(&format!("seed {}", label(e)), &err)
        );
    }
}

/// A fresh database holding [`subject_rows`] and [`other_rows`].
async fn seeded() -> Arc<PgDb> {
    let pg = PgDb::new_for_test().await;
    let conn = pg
        .pool()
        .get()
        .await
        .unwrap_or_else(|e| panic!("a connection to the test database: {e}"));
    let rows: Vec<JourneyEdgeObservation> =
        subject_rows().into_iter().chain(other_rows()).collect();
    for e in &rows {
        insert(&conn, e).await;
    }
    pg
}

/// How many of `app`'s rows the ledger holds, by SQL rather than by the seed.
async fn rows_in_ledger(pg: &PgDb, app: &str) -> u64 {
    let conn = pg
        .pool()
        .get()
        .await
        .unwrap_or_else(|e| panic!("a connection to the test database: {e}"));
    let n = conn
        .query_one(
            "SELECT count(*) FROM project.journey_edge_observations WHERE app_id = $1",
            &[&app],
        )
        .await
        .and_then(|row| row.try_get::<_, i64>(0))
        .unwrap_or_else(|e| panic!("count {app}'s rows: {e}"));
    u64::try_from(n).expect("a count is never negative")
}

async fn read_since(pg: &PgDb, since: DateTime<Utc>) -> LedgerRead {
    read_ledger_since(pg, APP, since)
        .await
        .unwrap_or_else(|why| panic!("read_ledger_since({APP}, {since:?}) could not read: {why:?}"))
}

async fn read_timed_since(pg: &PgDb, since: DateTime<Utc>) -> TimedRead {
    read_timed_edges(pg, APP, Some(since))
        .await
        .unwrap_or_else(|why| panic!("read_timed_edges({APP}, {since:?}) could not read: {why:?}"))
}

// -- What a read must return ---------------------------------------------------

/// What `OLDER_EDGE_GROUPS_SQL` groups by — from, to, trigger, outcome and
/// whether the row is withdrawn — written readably. Every row here is built by
/// [`edge`], whose node and trigger JSON are fixed by the spec ids, the
/// navigation kind and the fingerprint named here, so two rows share this text
/// exactly when they share the SQL's group key.
fn transition(e: &JourneyEdgeObservation) -> String {
    let to = e
        .to_node
        .as_ref()
        .map_or_else(|| "(unobserved)".to_string(), |n| n.key());
    let fingerprint = e.trigger.target_fingerprint.as_deref().unwrap_or("-");
    let withdrawn = if e.invalidated_at.is_some() {
        ", withdrawn"
    } else {
        ""
    };
    format!(
        "{} -> {to} by {} {fingerprint}, {}{withdrawn}",
        e.from_node.key(),
        e.trigger.navigation_trigger.as_str(),
        e.outcome.as_str(),
    )
}

/// A row, or a group's representative, as a failure names it.
fn label(e: &JourneyEdgeObservation) -> String {
    let n =
        e.id.rsplit('-')
            .next()
            .unwrap_or(e.id.as_str())
            .trim_start_matches('0');
    format!("{}#{n} {} @ {}", e.app_id, transition(e), e.observed_at)
}

fn labels<'a>(rows: impl IntoIterator<Item = &'a JourneyEdgeObservation>) -> Vec<String> {
    rows.into_iter().map(label).collect()
}

/// One older group as the SQL makes it: its representative's id (the smallest
/// id among its rows), its `observedAt` (the newest among them) and how many
/// rows it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupFacts {
    id: String,
    observed_at: String,
    rows: u64,
}

/// `groups` as `(transition, facts)`, sorted by transition, so two sets of
/// groups compare whatever order they were read in.
fn by_transition<'a>(
    groups: impl IntoIterator<Item = (&'a JourneyEdgeObservation, u64)>,
) -> Vec<(String, GroupFacts)> {
    let mut out: Vec<(String, GroupFacts)> = groups
        .into_iter()
        .map(|(e, rows)| {
            (
                transition(e),
                GroupFacts {
                    id: e.id.clone(),
                    observed_at: e.observed_at.clone(),
                    rows,
                },
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Where a `since` read must put each of the app's rows: in full, newest first,
/// the rows `full` names; every other row in the group of its transition.
struct Expected {
    full: Vec<JourneyEdgeObservation>,
    groups: Vec<(String, GroupFacts)>,
}

fn expected(full: &[u32]) -> Expected {
    let rows = subject_rows();
    let full: Vec<JourneyEdgeObservation> = full
        .iter()
        .copied()
        .map(|n| {
            rows.iter()
                .find(|e| e.id == id(n))
                .cloned()
                .unwrap_or_else(|| panic!("row {n} is not seeded"))
        })
        .collect();
    let mut groups: BTreeMap<String, GroupFacts> = BTreeMap::new();
    for e in rows.iter().filter(|e| !full.contains(e)) {
        let g = groups.entry(transition(e)).or_insert_with(|| GroupFacts {
            id: e.id.clone(),
            observed_at: e.observed_at.clone(),
            rows: 0,
        });
        if e.id < g.id {
            g.id.clone_from(&e.id);
        }
        if e.observed_at > g.observed_at {
            g.observed_at.clone_from(&e.observed_at);
        }
        g.rows += 1;
    }
    Expected {
        full,
        groups: groups.into_iter().collect(),
    }
}

/// The exact form of (a), for both callers: `read` reads in full exactly the
/// rows `expected` names, newest first and unchanged, and groups every other
/// row with its transition; `timed` places every row where `read` does, and
/// returns each full row's stored timeline.
fn assert_places_every_row(case: &str, read: &LedgerRead, timed: &TimedRead, expected: &Expected) {
    assert!(
        !read.edges_truncated && !read.older_groups_truncated,
        "{case}: a read of 23 rows reports truncation"
    );
    assert_eq!(
        labels(&read.edges),
        labels(&expected.full),
        "{case}: the full read is not exactly the app's rows at or after the bind, newest first"
    );
    assert_eq!(
        read.edges, expected.full,
        "{case}: a row read in full does not come back as it was written"
    );
    assert_eq!(
        by_transition(read.older_groups.iter().map(|g| (&g.edge, g.rows))),
        expected.groups,
        "{case}: the older groups are not the app's other rows, one group per transition"
    );

    let ledger: Vec<(String, u64)> = read
        .edges
        .iter()
        .map(|e| (label(e), 1))
        .chain(read.older_groups.iter().map(|g| (label(&g.edge), g.rows)))
        .collect();
    let placed: Vec<(String, u64)> = timed
        .rows
        .iter()
        .map(|t| (label(&t.edge), t.rows))
        .collect();
    assert_eq!(
        placed, ledger,
        "{case}: the timed read places rows differently from the ledger read"
    );
    assert!(
        !timed.edges_truncated && !timed.older_groups_truncated,
        "{case}: a timed read of 23 rows reports truncation"
    );
    for (i, t) in timed.rows.iter().enumerate() {
        let full = read.edges.get(i);
        assert_eq!(
            Some(&t.edge),
            full.or_else(|| read.older_groups.get(i - read.edges.len()).map(|g| &g.edge)),
            "{case}: the timed read's entry {i} is not the ledger read's"
        );
        assert!(
            !t.timeline_unparseable,
            "{case}: the timed read could not parse the timeline of {}",
            label(&t.edge)
        );
        // A group's representative is no row, so it carries no timeline.
        let stored = full.and_then(timeline_of);
        assert_eq!(
            t.timeline,
            stored,
            "{case}: the timed read did not return the timeline stored with {}",
            label(&t.edge)
        );
    }
}

/// The graph over every row of the app — its unpartitioned read, checked to be
/// exactly the seeded rows — and the graph over its read partitioned at
/// `since`, both under the filter `since`.
async fn graphs(pg: &PgDb, since: DateTime<Utc>) -> (JourneyGraph, JourneyGraph) {
    let every = read_ledger(pg, APP)
        .await
        .unwrap_or_else(|why| panic!("read_ledger({APP}) could not read: {why:?}"));
    assert!(
        !every.edges_truncated && every.rows_unparseable == 0,
        "the unpartitioned read of 23 rows is truncated or has unparseable rows"
    );
    let mut every_row = every.edges.clone();
    every_row.sort_by(|a, b| a.id.cmp(&b.id));
    let mut seeded = subject_rows();
    seeded.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(
        every_row, seeded,
        "the unpartitioned read is not exactly the seeded rows"
    );
    let read = read_since(pg, since).await;
    assert!(
        read.older_rows() > read.older_groups.len() as u64,
        "the read partitioned at {since:?} summarized no rows, so it cannot show \
         that summarizing changes nothing"
    );
    let filter = JourneyFilter {
        since: Some(since),
        ..JourneyFilter::default()
    };
    (
        build_graph(&every.edges, &every.frontier, &writing(), &filter),
        read.build_graph(&writing(), &filter),
    )
}

/// The three row totals `LedgerRead::build_graph` corrects: `edgesFilteredOut`,
/// `edgesInvalidated` and `unplacedRejectedRows`.
fn totals(g: &JourneyGraph) -> (u64, u64, u64) {
    (
        g.coverage.edges_filtered_out,
        g.coverage.edges_invalidated,
        g.unplaced_rejected_rows,
    )
}

// -- The seven properties -------------------------------------------------------

/// (a) Every row of the app is read exactly once: in full, or inside the older
/// group of its transition. Here 5 rows in full and 18 rows in 9 groups.
#[tokio::test]
async fn a_since_read_on_postgres_partitions_every_row() {
    let pg = seeded().await;
    let count = rows_in_ledger(&pg, APP).await;
    assert_eq!(count, 23, "the seed did not write the app's 23 rows");

    let read = read_since(&pg, since()).await;
    assert_eq!(
        read.edges.len() as u64 + read.older_rows(),
        count,
        "(a) the since read does not partition the app's {count} rows: {} read in full \
         + {} in {} older groups, {} set aside as unparseable",
        read.edges.len(),
        read.older_rows(),
        read.older_groups.len(),
        read.rows_unparseable
    );
    assert_eq!(
        read.rows_unparseable, 0,
        "(a) the since read set rows aside as unparseable"
    );
    let timed = read_timed_since(&pg, since()).await;
    let timed_rows: u64 = timed.rows.iter().map(|t| t.rows).sum();
    assert_eq!(
        timed_rows, count,
        "(a) the timed since read stands for {timed_rows} rows, not the app's {count} \
         ({} set aside as unparseable)",
        timed.rows_unparseable
    );
    assert_eq!(
        timed.rows_unparseable, 0,
        "(a) the timed since read set rows aside as unparseable"
    );

    assert_eq!(
        (read.edges.len(), read.older_rows(), read.older_groups.len()),
        (5, 18, 9),
        "(a) this ledger's read at `since` is 5 rows in full and 18 rows in 9 older groups \
         (rows in full, rows grouped, groups)"
    );
    assert_places_every_row("(a)", &read, &timed, &expected(&[5, 4, 3, 2, 1]));
}

/// (b) The row observed exactly at `since` is read in full — the full reads'
/// `>=` — and stands in no group — the groups' `<`. Its transition has two
/// older rows, so a group that took it in would stand for three and render at
/// `since`, which `parse_older_groups` refuses: that shows as the group missing
/// and its rows set aside as unparseable.
#[tokio::test]
async fn the_row_at_since_is_read_in_full_and_in_no_group() {
    let pg = seeded().await;
    let at_since = subject_rows()
        .into_iter()
        .find(|e| e.observed_at == SINCE)
        .expect("a seeded row at `since`");
    let its_transition = transition(&at_since);
    let read = read_since(&pg, since()).await;
    let timed = read_timed_since(&pg, since()).await;

    assert!(
        read.edges.contains(&at_since),
        "(b) the row at `since` ({}) is not in the full read: {:?}",
        label(&at_since),
        labels(&read.edges)
    );
    assert!(
        timed.rows.iter().any(|t| t.edge == at_since && t.rows == 1),
        "(b) the row at `since` ({}) is not in the timed full read",
        label(&at_since)
    );

    let grouped: Vec<u64> = read
        .older_groups
        .iter()
        .filter(|g| transition(&g.edge) == its_transition)
        .map(|g| g.rows)
        .collect();
    assert_eq!(
        grouped,
        vec![2],
        "(b) the row at `since` ({}) went into an older group: the group of `{its_transition}` must \
         stand for its 2 rows before `since` and nothing else, but the read's groups of \
         that transition stand for {grouped:?} rows, and {} rows were set aside as \
         unparseable (a group holding the row at `since` renders at `since`, and is set \
         aside so)",
        label(&at_since),
        read.rows_unparseable
    );
    let timed_grouped: Vec<u64> = timed
        .rows
        .iter()
        .filter(|t| transition(&t.edge) == its_transition && t.edge.observed_at.as_str() < SINCE)
        .map(|t| t.rows)
        .collect();
    assert_eq!(
        timed_grouped,
        vec![2],
        "(b) the row at `since` ({}) went into an older group of the timed read: its \
         groups of `{its_transition}` stand for {timed_grouped:?} rows, {} set aside as unparseable",
        label(&at_since),
        timed.rows_unparseable
    );
    assert_eq!(
        (read.rows_unparseable, timed.rows_unparseable),
        (0, 0),
        "(b) rows were set aside as unparseable (ledger read, timed read)"
    );
}

/// (c) No row of another app reaches either read, on either side of `since` —
/// and the other app's rows are there to leak: read for that app, they are.
#[tokio::test]
async fn no_row_of_another_app_reaches_either_since_read() {
    let pg = seeded().await;
    let read = read_since(&pg, since()).await;
    let leaked = labels(
        read.edges
            .iter()
            .chain(read.older_groups.iter().map(|g| &g.edge))
            .filter(|e| e.app_id != APP),
    );
    assert!(
        leaked.is_empty(),
        "(c) rows of another app reached the since read of {APP}: {leaked:?}"
    );
    let timed = read_timed_since(&pg, since()).await;
    let leaked = labels(
        timed
            .rows
            .iter()
            .map(|t| &t.edge)
            .filter(|e| e.app_id != APP),
    );
    assert!(
        leaked.is_empty(),
        "(c) rows of another app reached the timed since read of {APP}: {leaked:?}"
    );

    let other = read_ledger_since(&pg, OTHER_APP, since())
        .await
        .unwrap_or_else(|why| panic!("read_ledger_since({OTHER_APP}) could not read: {why:?}"));
    assert_eq!(
        (other.edges.len(), other.older_rows()),
        (2, 3),
        "the other app's 5 rows are not in the ledger on both sides of `since`, so (c) \
         would hold vacuously"
    );
}

/// (d) The older groups are a function of the ledger: two reads return them
/// identically, ordered by their representative's id — the smallest id among
/// each group's rows, which (a) checks — however the rows were written or
/// their transitions sort. Which groups there are is (a)'s concern, so a
/// changed set of groups does not red this test.
#[tokio::test]
async fn older_groups_read_the_same_twice_in_min_id_order() {
    let pg = seeded().await;
    let first = read_since(&pg, since()).await;
    let second = read_since(&pg, since()).await;
    assert_eq!(
        first.older_groups, second.older_groups,
        "(d) two reads of one ledger returned different older groups"
    );

    let ids: Vec<&str> = first
        .older_groups
        .iter()
        .map(|g| g.edge.id.as_str())
        .collect();
    let mut ascending = ids.clone();
    ascending.sort_unstable();
    ascending.dedup();
    assert_eq!(
        ids,
        ascending,
        "(d) the older groups are not in strictly ascending min(id::text) order: {:?}",
        labels(first.older_groups.iter().map(|g| &g.edge))
    );
    // Not vacuous: sorted by transition — the group key, which is the order a
    // sort-based aggregate with no `ORDER BY` returns — the same groups are in
    // another order.
    let mut keyed: Vec<(String, &str)> = first
        .older_groups
        .iter()
        .map(|g| (transition(&g.edge), g.edge.id.as_str()))
        .collect();
    keyed.sort_unstable();
    let ids_by_transition: Vec<&str> = keyed.into_iter().map(|(_, group)| group).collect();
    assert_ne!(
        ids_by_transition, ids,
        "the seed's groups are in min(id::text) order when sorted by transition too, so \
         (d) would hold with no ORDER BY"
    );

    let timed = read_timed_since(&pg, since()).await;
    let timed_groups: Vec<(&JourneyEdgeObservation, u64)> = timed
        .rows
        .get(first.edges.len()..)
        .unwrap_or_default()
        .iter()
        .map(|t| (&t.edge, t.rows))
        .collect();
    let groups: Vec<(&JourneyEdgeObservation, u64)> = first
        .older_groups
        .iter()
        .map(|g| (&g.edge, g.rows))
        .collect();
    assert_eq!(
        timed_groups, groups,
        "(d) the timed read's older groups differ from the ledger read's"
    );
}

/// (e) Withdrawal is part of the group key: B's 3 live and 2 withdrawn rows
/// before `since` are two groups, so no live row is counted withdrawn and no
/// withdrawn row live. Only the app's own rows are looked at; another app's
/// copies are (c)'s concern.
#[tokio::test]
async fn a_live_and_a_withdrawn_copy_of_one_transition_are_two_groups() {
    let pg = seeded().await;
    let b_c = |e: &JourneyEdgeObservation| {
        e.app_id == APP
            && e.from_node.key() == key("b")
            && e.to_node.as_ref().map(|n| n.key()) == Some(key("c"))
    };

    let read = read_since(&pg, since()).await;
    let mut groups: Vec<(bool, u64)> = read
        .older_groups
        .iter()
        .filter(|g| b_c(&g.edge))
        .map(|g| (g.edge.invalidated_at.is_some(), g.rows))
        .collect();
    groups.sort_unstable();
    assert_eq!(
        groups,
        vec![(false, 3), (true, 2)],
        "(e) B -> C's rows before `since` are not two groups, a live one of 3 and a \
         withdrawn one of 2; (withdrawn, rows) per group: {groups:?}"
    );

    let timed = read_timed_since(&pg, since()).await;
    let mut groups: Vec<(bool, u64)> = timed
        .rows
        .iter()
        .filter(|t| b_c(&t.edge) && t.edge.observed_at.as_str() < SINCE)
        .map(|t| (t.edge.invalidated_at.is_some(), t.rows))
        .collect();
    groups.sort_unstable();
    assert_eq!(
        groups,
        vec![(false, 3), (true, 2)],
        "(e) the timed read does not keep B -> C's live and withdrawn rows before \
         `since` apart; (withdrawn, rows) per group: {groups:?}"
    );
}

/// (f) Summarizing rows changes no conclusion: every graph answer over the
/// since read (`LedgerRead::build_graph`) reaches the verdict it reaches over
/// every row (`build_graph` over the unpartitioned read), and the three row
/// totals the grouped graph corrects are equal. Verdicts, not whole graphs:
/// every other count of excluded rows counts a group once.
#[tokio::test]
async fn a_since_read_concludes_what_every_row_would() {
    let pg = seeded().await;
    let (whole, grouped) = graphs(&pg, since()).await;
    assert_same_conclusions(
        "(f) the read partitioned at `since`",
        &whole,
        &grouped,
        &key("a"),
        &key("b"),
        &key("c"),
    );
    assert_eq!(
        totals(&grouped),
        totals(&whole),
        "(f) (edgesFilteredOut, edgesInvalidated, unplacedRejectedRows) of the since read \
         differ from every row's"
    );
    // 12 live trusted rows before `since`, 4 withdrawn, 4 untrusted.
    assert_eq!(
        totals(&grouped),
        (12, 4, 4),
        "(f) (edgesFilteredOut, edgesInvalidated, unplacedRejectedRows)"
    );
}

/// (g) Postgres accepts every bind `since_bind` makes of an edge-case `since`,
/// reads it as `since_bind` means it, and the read still places every row and
/// concludes what every row would:
///
/// - a leap second binds as the last microsecond of its own second, so the row
///   one second on — which the filter keeps — is read in full (Postgres reads
///   `:60.5` as half a second INTO the next minute, and would group it);
/// - an instant past year 9999 in UTC binds without chrono's leading `+`, and
///   every row is older than it;
/// - nanoseconds floor to the column's microseconds, so the row exactly at the
///   floor — which the filter excludes — is read in full and excluded by the
///   graph instead.
#[tokio::test]
async fn postgres_accepts_every_edge_case_since_bind() {
    let pg = seeded().await;
    // (what, since, the bind it makes, the rows it reads in full, newest first)
    let cases: [(&str, &str, &str, &[u32]); 3] = [
        (
            "a leap second",
            "2026-10-01T10:00:60.5Z",
            "2026-10-01T10:00:59.999999Z",
            &[5, 4],
        ),
        (
            "an instant past year 9999 in UTC",
            "9999-12-31T23:59:59-23:59",
            "10000-01-01T23:58:59.000000Z",
            &[],
        ),
        (
            "a nanosecond since",
            "2026-10-01T10:00:00.123456789Z",
            "2026-10-01T10:00:00.123456Z",
            &[5, 4, 3, 2],
        ),
    ];
    for (what, given, bind, full) in cases {
        let since = instant(given);
        assert_eq!(
            since_bind(since).as_deref(),
            Some(bind),
            "(g) {what}: since_bind no longer binds {given} as this case expects, so the \
             case may not exercise the edge it names"
        );
        let read = read_ledger_since(&pg, APP, since)
            .await
            .unwrap_or_else(|why| {
                panic!("(g) {what}: Postgres did not take the bind {bind}: {why:?}")
            });
        let timed = read_timed_edges(&pg, APP, Some(since))
            .await
            .unwrap_or_else(|why| {
                panic!(
                    "(g) {what}: Postgres did not take the bind {bind} in the timed read: {why:?}"
                )
            });
        let case = format!("(g) {what}, bound as {bind}");
        assert_eq!(
            (read.rows_unparseable, timed.rows_unparseable),
            (0, 0),
            "{case}: rows were set aside as unparseable (ledger read, timed read)"
        );
        assert_places_every_row(&case, &read, &timed, &expected(full));

        let (whole, grouped) = graphs(&pg, since).await;
        assert_same_conclusions(&case, &whole, &grouped, &key("a"), &key("b"), &key("c"));
        assert_eq!(
            totals(&grouped),
            totals(&whole),
            "{case}: (edgesFilteredOut, edgesInvalidated, unplacedRejectedRows) differ from \
             every row's"
        );
    }
}
