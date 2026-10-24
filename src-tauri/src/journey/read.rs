//! Bounded reads of the journey ledger — the rows the Phase 2 query routes and
//! the Phase 4 timing / timeline-ref reads are computed over.
//!
//! Plan `2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time`.
//! Every read here is:
//!
//! - **behind the same schema probe as the writer** — an absent table is
//!   [`LedgerLoad::SchemaAbsent`], never an empty result set;
//! - **bounded** — at most [`EDGE_READ_LIMIT`] edges (newest first) and
//!   [`FRONTIER_READ_LIMIT`] frontier rows, each fetched as `limit + 1` so a
//!   truncation is DETECTED and reported ([`LedgerRead::edges_truncated`]),
//!   never mistaken for the whole ledger; and under [`READ_TIMEOUT`];
//! - **typed at the boundary** — each row is assembled by `jsonb_build_object`
//!   into the camelCase wire shape and deserialized through the shared
//!   `qontinui_types::journey` structs, so the column→field mapping lives in
//!   one SQL constant. A row that does not deserialize is COUNTED
//!   ([`LedgerRead::rows_unparseable`]), never silently dropped.
//!
//! `since` IS pushed into SQL (`$3`), so `?since=` narrows the bounded read
//! itself — the way out of a truncated read. `build_graph` re-applies it (a
//! no-op there) and so cannot count the rows SQL never returned:
//! `edgesFilteredOut` counts only the `appVersion` / `runKind` filters, which
//! stay in Rust so their exclusions are counted.

use std::time::Duration;

use qontinui_types::journey::{FrontierEntry, JourneyEdgeObservation};
use qontinui_vision_core::ChangeTimeline;

use crate::database::pg::PgDb;

use super::capture::{journey_schema_supported, SchemaProbe};

/// Most edges one query reads (newest first). Phase 0 U5 measured
/// snapshot-equivalent volume in the tens per month; this is headroom by
/// three orders of magnitude while keeping one read well under a second.
pub(crate) const EDGE_READ_LIMIT: i64 = 20_000;

/// Most frontier rows one query reads. The primary key bounds the table, but
/// not to a small number on a large app.
pub(crate) const FRONTIER_READ_LIMIT: i64 = 20_000;

/// Bound on one read (pool checkout excluded — the pool has its own).
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// An RFC 3339 UTC rendering of a `timestamptz` column, microsecond
/// precision — the form the writer binds (`SecondsFormat::Micros`).
macro_rules! ts_sql {
    ($col:literal) => {
        concat!(
            "to_char(",
            $col,
            " AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"
        )
    };
}

macro_rules! edge_object_sql {
    () => {
        concat!(
            "jsonb_build_object(",
            "'id', e.id::text, ",
            "'observedAt', ",
            ts_sql!("e.observed_at"),
            ", ",
            "'appId', e.app_id, ",
            "'appVersion', e.app_version, ",
            "'runnerBuildId', e.runner_build_id, ",
            "'runnerInstance', e.runner_instance, ",
            "'runId', e.run_id, ",
            "'runKind', e.run_kind, ",
            "'fromNode', e.from_node, ",
            "'toNode', e.to_node, ",
            "'trigger', e.trigger, ",
            "'outcome', e.outcome, ",
            "'invalidatedAt', ",
            ts_sql!("e.invalidated_at"),
            ", ",
            "'invalidatedReason', e.invalidated_reason, ",
            "'invalidatedBy', e.invalidated_by::text, ",
            "'invalidationToken', e.invalidation_token::text)"
        )
    };
}

/// One app's edges, newest first, withdrawn rows INCLUDED (the graph counts
/// them as `edgesInvalidated`). `$2` is the bound plus one.
pub(crate) const EDGE_READ_SQL: &str = concat!(
    "SELECT ",
    edge_object_sql!(),
    "\n  FROM project.journey_edge_observations e\n",
    " WHERE e.app_id = $1\n",
    "   AND ($3::timestamptz IS NULL OR e.observed_at >= $3::timestamptz)\n",
    " ORDER BY e.observed_at DESC, e.id DESC\n",
    " LIMIT $2"
);

/// [`EDGE_READ_SQL`] plus the `timeline` column, for edge timing.
pub(crate) const EDGE_TIMELINE_READ_SQL: &str = concat!(
    "SELECT ",
    edge_object_sql!(),
    ", e.timeline\n  FROM project.journey_edge_observations e\n",
    " WHERE e.app_id = $1\n",
    "   AND ($3::timestamptz IS NULL OR e.observed_at >= $3::timestamptz)\n",
    " ORDER BY e.observed_at DESC, e.id DESC\n",
    " LIMIT $2"
);

/// One app's frontier rows in key order. `$2` is the bound plus one.
pub(crate) const FRONTIER_READ_SQL: &str = concat!(
    "SELECT jsonb_build_object(",
    "'appId', f.app_id, ",
    "'nodeKey', f.node_key, ",
    "'node', f.node, ",
    "'affordanceFingerprint', f.affordance_fingerprint, ",
    "'affordanceRole', f.affordance_role, ",
    "'declaredEffect', f.declared_effect, ",
    "'reason', f.reason, ",
    "'firstSeenAt', ",
    ts_sql!("f.first_seen_at"),
    ", ",
    "'lastSeenAt', ",
    ts_sql!("f.last_seen_at"),
    ", ",
    "'lastSeenRunId', f.last_seen_run_id)\n",
    "  FROM project.journey_frontier f\n",
    " WHERE f.app_id = $1\n",
    " ORDER BY f.node_key, f.affordance_fingerprint\n",
    " LIMIT $2"
);

/// One edge's timeline, by id, for `/vision/assert` `timeline_ref`.
pub(crate) const TIMELINE_BY_ID_SQL: &str = concat!(
    "SELECT e.timeline, ",
    ts_sql!("e.invalidated_at"),
    "\n  FROM project.journey_edge_observations e\n",
    " WHERE e.id = $1::text::uuid"
);

/// Why a read produced no rows to compute over. Never an empty result.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LedgerUnavailable {
    /// The probe found a journey table missing.
    SchemaAbsent,
    /// The database could not be asked (pool, probe failure, read error,
    /// timeout).
    Failed(String),
}

/// One edge with its parsed `timeline` column.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TimedEdge {
    pub edge: JourneyEdgeObservation,
    /// `None` = the column is NULL (no timeline recorded).
    pub timeline: Option<ChangeTimeline>,
    /// The column held a value that is not a `ChangeTimeline`.
    pub timeline_unparseable: bool,
}

/// What a bounded read returned, with every gap it knows about.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LedgerRead {
    pub edges: Vec<JourneyEdgeObservation>,
    pub frontier: Vec<FrontierEntry>,
    /// More edges exist than [`EDGE_READ_LIMIT`]; the oldest were not read.
    pub edges_truncated: bool,
    /// More frontier rows exist than [`FRONTIER_READ_LIMIT`].
    pub frontier_truncated: bool,
    /// Rows that did not deserialize into the contract structs.
    pub rows_unparseable: u64,
}

/// Split a `limit + 1` fetch into the kept rows and a truncation flag.
pub(crate) fn split_bound<T>(mut rows: Vec<T>, limit: i64) -> (Vec<T>, bool) {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    (rows, truncated)
}

/// Parse jsonb row objects into `T`, counting the ones that do not parse.
pub(crate) fn parse_rows<T: serde::de::DeserializeOwned>(
    rows: Vec<serde_json::Value>,
) -> (Vec<T>, u64) {
    let mut out = Vec::with_capacity(rows.len());
    let mut bad = 0u64;
    for row in rows {
        match serde_json::from_value::<T>(row) {
            Ok(v) => out.push(v),
            Err(e) => {
                tracing::warn!("journey read: a ledger row did not parse: {e}");
                bad += 1;
            }
        }
    }
    (out, bad)
}

/// A pooled connection whose probe found both tables, or why not.
async fn readable_connection(pg: &PgDb) -> Result<deadpool_postgres::Object, LedgerUnavailable> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| LedgerUnavailable::Failed(format!("PG pool error: {e}")))?;
    match journey_schema_supported(&conn).await {
        SchemaProbe::Present => Ok(conn),
        SchemaProbe::Absent { .. } => Err(LedgerUnavailable::SchemaAbsent),
        SchemaProbe::Failed { error } => Err(LedgerUnavailable::Failed(format!(
            "the journey schema probe failed: {error}"
        ))),
    }
}

async fn bounded<T>(
    what: &str,
    fut: impl std::future::Future<Output = Result<T, tokio_postgres::Error>>,
) -> Result<T, LedgerUnavailable> {
    match tokio::time::timeout(READ_TIMEOUT, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(LedgerUnavailable::Failed(crate::database::pg::pg_err(
            what, &e,
        ))),
        Err(_) => Err(LedgerUnavailable::Failed(format!(
            "{what} did not answer within {READ_TIMEOUT:?}"
        ))),
    }
}

/// One app's edges (at or after `since`, when set) and frontier, bounded.
pub(crate) async fn read_ledger(
    pg: &PgDb,
    app_id: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<LedgerRead, LedgerUnavailable> {
    let conn = readable_connection(pg).await?;
    let edge_bound = EDGE_READ_LIMIT + 1;
    let frontier_bound = FRONTIER_READ_LIMIT + 1;
    let edge_rows = bounded(
        "journey edge read",
        conn.query(EDGE_READ_SQL, &[&app_id, &edge_bound, &since]),
    )
    .await?;
    let frontier_rows = bounded(
        "journey frontier read",
        conn.query(FRONTIER_READ_SQL, &[&app_id, &frontier_bound]),
    )
    .await?;
    let first_col =
        |rows: Vec<tokio_postgres::Row>| -> Result<Vec<serde_json::Value>, LedgerUnavailable> {
            rows.iter()
                .map(|r| {
                    r.try_get::<_, serde_json::Value>(0).map_err(|e| {
                        LedgerUnavailable::Failed(crate::database::pg::pg_err(
                            "journey row decode",
                            &e,
                        ))
                    })
                })
                .collect()
        };
    let (edge_values, edges_truncated) = split_bound(first_col(edge_rows)?, EDGE_READ_LIMIT);
    let (frontier_values, frontier_truncated) =
        split_bound(first_col(frontier_rows)?, FRONTIER_READ_LIMIT);
    let (edges, bad_edges) = parse_rows::<JourneyEdgeObservation>(edge_values);
    let (frontier, bad_frontier) = parse_rows::<FrontierEntry>(frontier_values);
    Ok(LedgerRead {
        edges,
        frontier,
        edges_truncated,
        frontier_truncated,
        rows_unparseable: bad_edges + bad_frontier,
    })
}

/// Parse one row's `timeline` column.
pub(crate) fn parse_timeline(raw: Option<serde_json::Value>) -> (Option<ChangeTimeline>, bool) {
    match raw.filter(|v| !v.is_null()) {
        None => (None, false),
        Some(v) => match serde_json::from_value::<ChangeTimeline>(v) {
            Ok(t) => (Some(t), false),
            Err(e) => {
                tracing::warn!("journey read: a stored timeline did not parse: {e}");
                (None, true)
            }
        },
    }
}

/// One app's edges with their timelines, bounded. Returns the rows, whether
/// the bound truncated them, and the count of rows that did not parse.
pub(crate) async fn read_timed_edges(
    pg: &PgDb,
    app_id: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(Vec<TimedEdge>, bool, u64), LedgerUnavailable> {
    let conn = readable_connection(pg).await?;
    let bound = EDGE_READ_LIMIT + 1;
    let rows = bounded(
        "journey edge-timing read",
        conn.query(EDGE_TIMELINE_READ_SQL, &[&app_id, &bound, &since]),
    )
    .await?;
    let (rows, truncated) = split_bound(rows, EDGE_READ_LIMIT);
    let mut out = Vec::with_capacity(rows.len());
    let mut bad = 0u64;
    for row in rows {
        let edge = row
            .try_get::<_, serde_json::Value>(0)
            .ok()
            .and_then(|v| serde_json::from_value::<JourneyEdgeObservation>(v).ok());
        let Some(edge) = edge else {
            bad += 1;
            continue;
        };
        let raw = row
            .try_get::<_, Option<serde_json::Value>>(1)
            .unwrap_or(None);
        let (timeline, timeline_unparseable) = parse_timeline(raw);
        out.push(TimedEdge {
            edge,
            timeline,
            timeline_unparseable,
        });
    }
    Ok((out, truncated, bad))
}

/// How a `timeline_ref` resolved.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TimelineRefLookup {
    Found(Box<ChangeTimeline>),
    /// The ref is not a UUID, or no row has it.
    NotFound,
    /// The row exists and its `timeline` column is NULL.
    NoTimeline,
    /// The row was withdrawn (`invalidated_at` set) at this time.
    Withdrawn(String),
    /// The column holds a value that is not a `ChangeTimeline`.
    Unparseable,
    Unavailable(LedgerUnavailable),
}

/// Load one edge observation's timeline by id.
pub(crate) async fn read_timeline_by_id(pg: &PgDb, id: &str) -> TimelineRefLookup {
    if uuid::Uuid::parse_str(id).is_err() {
        return TimelineRefLookup::NotFound;
    }
    let conn = match readable_connection(pg).await {
        Ok(c) => c,
        Err(e) => return TimelineRefLookup::Unavailable(e),
    };
    let rows = match bounded(
        "journey timeline_ref read",
        conn.query(TIMELINE_BY_ID_SQL, &[&id]),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return TimelineRefLookup::Unavailable(e),
    };
    let Some(row) = rows.first() else {
        return TimelineRefLookup::NotFound;
    };
    if let Ok(Some(at)) = row.try_get::<_, Option<String>>(1) {
        return TimelineRefLookup::Withdrawn(at);
    }
    let raw = row
        .try_get::<_, Option<serde_json::Value>>(0)
        .unwrap_or(None);
    match parse_timeline(raw) {
        (Some(t), _) => TimelineRefLookup::Found(Box::new(t)),
        (None, true) => TimelineRefLookup::Unparseable,
        (None, false) => TimelineRefLookup::NoTimeline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_fetch_is_truncated_and_a_short_one_is_not() {
        let (rows, t) = split_bound(vec![1, 2, 3], 2);
        assert_eq!(rows, vec![1, 2]);
        assert!(t, "limit + 1 rows means the bound cut something");
        let (rows, t) = split_bound(vec![1, 2], 2);
        assert_eq!(rows, vec![1, 2]);
        assert!(!t);
    }

    #[test]
    fn reads_are_bounded_scoped_and_newest_first() {
        for sql in [EDGE_READ_SQL, EDGE_TIMELINE_READ_SQL] {
            assert!(sql.contains("WHERE e.app_id = $1"), "{sql}");
            assert!(
                sql.contains("AND ($3::timestamptz IS NULL OR e.observed_at >= $3::timestamptz)"),
                "?since= must narrow the bounded read itself: {sql}"
            );
            assert!(sql.contains("ORDER BY e.observed_at DESC, e.id DESC"));
            assert!(sql.ends_with("LIMIT $2"));
        }
        assert!(FRONTIER_READ_SQL.contains("WHERE f.app_id = $1"));
        assert!(FRONTIER_READ_SQL.ends_with("LIMIT $2"));
        assert!(EDGE_TIMELINE_READ_SQL.contains(", e.timeline\n"));
        assert!(
            !EDGE_READ_SQL.contains("e.timeline"),
            "the graph read never selects the timeline"
        );
    }

    /// The row objects carry exactly the contract's camelCase field names, so
    /// a row deserializes through the shared struct.
    #[test]
    fn row_objects_deserialize_through_the_contract_structs() {
        let edge = serde_json::json!({
            "id": "6f1c1f46-0f0e-4b8b-9a51-6c3c5d6f1d2a",
            "observedAt": "2026-10-01T10:00:00.000000Z",
            "appId": "qontinui-web",
            "appVersion": null,
            "runnerBuildId": "b1",
            "runnerInstance": "primary",
            "runId": null,
            "runKind": "agent_action",
            "fromNode": {"specId": "home", "stateIds": ["idle"], "modelled": true,
                         "pathnameTemplate": null, "pageLabel": null},
            "toNode": null,
            "trigger": {"actionType": "click", "targetFingerprint": null, "targetRole": null,
                        "declaredEffect": null, "navigationTrigger": "affordance",
                        "chokePoint": "element_action"},
            "outcome": "to_node_unobserved",
            "invalidatedAt": null,
            "invalidatedReason": null,
            "invalidatedBy": null,
            "invalidationToken": null,
        });
        for key in edge.as_object().unwrap().keys() {
            assert!(
                EDGE_READ_SQL.contains(&format!("'{key}'")),
                "the read SQL does not build {key}"
            );
        }
        let (parsed, bad) = parse_rows::<JourneyEdgeObservation>(vec![edge]);
        assert_eq!(bad, 0);
        assert_eq!(parsed.len(), 1);
        parsed[0].validate().expect("a valid row");

        let frontier = serde_json::json!({
            "appId": "qontinui-web",
            "nodeKey": "home#idle",
            "node": {"specId": "home", "stateIds": ["idle"], "modelled": true,
                     "pathnameTemplate": null, "pageLabel": null},
            "affordanceFingerprint": "fp-1",
            "affordanceRole": null,
            "declaredEffect": "read",
            "reason": "not_yet_activated",
            "firstSeenAt": "2026-10-01T10:00:00.000000Z",
            "lastSeenAt": "2026-10-01T10:00:00.000000Z",
            "lastSeenRunId": null,
        });
        for key in frontier.as_object().unwrap().keys() {
            assert!(FRONTIER_READ_SQL.contains(&format!("'{key}'")), "{key}");
        }
        let (parsed, bad) = parse_rows::<FrontierEntry>(vec![frontier, serde_json::json!({})]);
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            bad, 1,
            "an unparseable row is counted, never dropped silently"
        );
    }

    #[test]
    fn a_null_timeline_is_none_and_garbage_is_counted() {
        assert_eq!(parse_timeline(None), (None, false));
        assert_eq!(parse_timeline(Some(serde_json::Value::Null)), (None, false));
        let (t, bad) = parse_timeline(Some(serde_json::json!({"nope": 1})));
        assert!(t.is_none());
        assert!(bad);
    }
}
