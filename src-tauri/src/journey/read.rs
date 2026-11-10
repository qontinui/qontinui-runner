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
//! No filter (`since` / `appVersion` / `runKind`) ever HIDES a row from the
//! shared `build_graph`, which applies the filter and turns every row it
//! excludes into doubt (filtered departures, arrival doubt, unplaced rejected
//! rows); a row SQL never returned would be no doubt at all, so a dead end
//! whose only exit predates `since` would read as a complete finding.
//!
//! `since` alone is pushed into SQL, as a PARTITION rather than a filter, so
//! it can narrow a read of a ledger larger than [`EDGE_READ_LIMIT`]:
//!
//! - rows at or after `since` are read in full, under the bound;
//! - rows before it are read as GROUPS ([`OLDER_EDGE_GROUPS_SQL`]): one row per
//!   distinct `(fromNode, toNode, trigger, outcome, withdrawn)`, with its row
//!   count. Those columns are every input of `validate()` and of how
//!   `build_graph` places an excluded row, so a group's representative gets the
//!   verdict each of its rows would. The other columns of a representative are
//!   per-column aggregates (it is not a row that exists), which is harmless:
//!   since every group predates `since`, the filter excludes it before they
//!   are read. A group whose rendered time does not predate `since` (only a BC
//!   row, which the writer cannot produce) is counted unparseable instead.
//!
//! [`LedgerRead::build_graph`] puts each group's row count back into the three
//! row totals. Every other count of excluded rows counts a group once, so it can
//! undercount; each is only ever read as zero or non-zero, which a group decides
//! exactly as its rows would. The answer discloses this as
//! `provenance.coverage.edgesSummarized`.

use std::time::Duration;

use chrono::{DateTime, Datelike, SecondsFormat, Timelike, Utc};
use qontinui_types::journey::{
    build_graph, trusted_edge_row, FrontierEntry, JourneyEdgeObservation, JourneyFilter,
    JourneyGraph, JourneyLedgerHealth, JourneyRejectedRowKind,
};
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
    " ORDER BY e.observed_at DESC, e.id DESC\n",
    " LIMIT $2"
);

/// [`EDGE_READ_SQL`] plus the `timeline` column, for edge timing.
pub(crate) const EDGE_TIMELINE_READ_SQL: &str = concat!(
    "SELECT ",
    edge_object_sql!(),
    ", e.timeline\n  FROM project.journey_edge_observations e\n",
    " WHERE e.app_id = $1\n",
    " ORDER BY e.observed_at DESC, e.id DESC\n",
    " LIMIT $2"
);

/// [`EDGE_READ_SQL`] narrowed to the rows at or after `$3`. Always read beside
/// [`OLDER_EDGE_GROUPS_SQL`], whose predicate is this one's exact complement.
pub(crate) const EDGE_READ_SINCE_SQL: &str = concat!(
    "SELECT ",
    edge_object_sql!(),
    "\n  FROM project.journey_edge_observations e\n",
    " WHERE e.app_id = $1\n",
    "   AND e.observed_at >= $3::text::timestamptz\n",
    " ORDER BY e.observed_at DESC, e.id DESC\n",
    " LIMIT $2"
);

/// [`EDGE_TIMELINE_READ_SQL`] narrowed to the rows at or after `$3`.
pub(crate) const EDGE_TIMELINE_READ_SINCE_SQL: &str = concat!(
    "SELECT ",
    edge_object_sql!(),
    ", e.timeline\n  FROM project.journey_edge_observations e\n",
    " WHERE e.app_id = $1\n",
    "   AND e.observed_at >= $3::text::timestamptz\n",
    " ORDER BY e.observed_at DESC, e.id DESC\n",
    " LIMIT $2"
);

/// The rows before `$3`, one per distinct transition, each with its row count
/// (see the module doc). `$2` is the bound plus one, in groups. Ordered by the
/// representative's id, so the same ledger always gives the same groups.
pub(crate) const OLDER_EDGE_GROUPS_SQL: &str = concat!(
    "SELECT jsonb_build_object(",
    "'id', min(e.id::text), ",
    "'observedAt', ",
    ts_sql!("max(e.observed_at)"),
    ", ",
    "'appId', e.app_id, ",
    "'appVersion', min(e.app_version), ",
    "'runnerBuildId', min(e.runner_build_id), ",
    "'runnerInstance', min(e.runner_instance), ",
    "'runId', min(e.run_id), ",
    "'runKind', min(e.run_kind), ",
    "'fromNode', e.from_node, ",
    "'toNode', e.to_node, ",
    "'trigger', e.trigger, ",
    "'outcome', e.outcome, ",
    "'invalidatedAt', ",
    ts_sql!("max(e.invalidated_at)"),
    ", ",
    "'invalidatedReason', min(e.invalidated_reason), ",
    "'invalidatedBy', min(e.invalidated_by::text), ",
    "'invalidationToken', min(e.invalidation_token::text)), ",
    "count(*)::bigint\n",
    "  FROM project.journey_edge_observations e\n",
    " WHERE e.app_id = $1\n",
    "   AND e.observed_at < $3::text::timestamptz\n",
    " GROUP BY e.app_id, e.from_node, e.to_node, e.trigger, e.outcome,",
    " (e.invalidated_at IS NOT NULL)\n",
    " ORDER BY min(e.id::text)\n",
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

/// One edge's timeline, by id AND app, for `/vision/assert` `timeline_ref`:
/// a ref naming another app's observation resolves to no row (unknown), so a
/// timeline is never evaluated against the wrong app.
pub(crate) const TIMELINE_BY_ID_SQL: &str = concat!(
    "SELECT e.timeline, ",
    ts_sql!("e.invalidated_at"),
    "\n  FROM project.journey_edge_observations e\n",
    " WHERE e.id = $1::text::uuid\n",
    "   AND e.app_id = $2"
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
    /// How many ledger rows this stands for: 1, or a group's row count when
    /// the row is the representative of an [`OlderEdgeGroup`].
    pub rows: u64,
}

/// The rows before `since` with one `(fromNode, toNode, trigger, outcome,
/// withdrawn)`: a representative and how many rows it stands for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OlderEdgeGroup {
    pub edge: JourneyEdgeObservation,
    pub rows: u64,
}

/// What a bounded read returned, with every gap it knows about.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LedgerRead {
    pub edges: Vec<JourneyEdgeObservation>,
    /// The rows before `since`, grouped; empty on a read with no `since`.
    pub older_groups: Vec<OlderEdgeGroup>,
    pub frontier: Vec<FrontierEntry>,
    /// More edges at or after `since` (or at all, with no `since`) than
    /// [`EDGE_READ_LIMIT`]; the oldest were not read.
    pub edges_truncated: bool,
    /// More older groups than [`EDGE_READ_LIMIT`]. Which went unread says
    /// nothing about their age: groups are ordered by id.
    pub older_groups_truncated: bool,
    /// More frontier rows exist than [`FRONTIER_READ_LIMIT`].
    pub frontier_truncated: bool,
    /// Rows that did not deserialize into the contract structs.
    pub rows_unparseable: u64,
}

impl LedgerRead {
    /// The shared graph over everything this read returned, with each older
    /// group's row count back in the totals.
    ///
    /// `build_graph` sees one row per group, so its row totals would otherwise
    /// count groups. Its doubt is unchanged by that: a representative is judged
    /// and placed exactly as each of its rows would be, and doubt reads "any".
    /// Only `edgesFilteredOut`, `edgesInvalidated` and `unplacedRejectedRows`
    /// are corrected; see the module doc for the other counts.
    pub(crate) fn build_graph(
        &self,
        ledger: &JourneyLedgerHealth,
        filter: &JourneyFilter,
    ) -> JourneyGraph {
        if self.older_groups.is_empty() {
            return build_graph(&self.edges, &self.frontier, ledger, filter);
        }
        let edges: Vec<JourneyEdgeObservation> = self
            .edges
            .iter()
            .chain(self.older_groups.iter().map(|g| &g.edge))
            .cloned()
            .collect();
        let mut graph = build_graph(&edges, &self.frontier, ledger, filter);
        for group in &self.older_groups {
            let extra = group.rows.saturating_sub(1);
            let withdrawn = group.edge.invalidated_at.is_some();
            if withdrawn {
                graph.coverage.edges_invalidated += extra;
            }
            match trusted_edge_row(&group.edge) {
                Ok(_) if !withdrawn => graph.coverage.edges_filtered_out += extra,
                Ok(_) => {}
                Err(reason) => {
                    graph.unplaced_rejected_rows += extra;
                    // The edge entries' `rows` sum to `unplaced_rejected_rows`.
                    if let Some(entry) = graph.rejected.iter_mut().find(|r| {
                        r.kind == JourneyRejectedRowKind::Edge
                            && r.row == group.edge.id
                            && r.reason == reason
                    }) {
                        entry.rows += extra;
                    }
                }
            }
        }
        graph
    }

    /// How many ledger rows the older groups stand for.
    pub(crate) fn older_rows(&self) -> u64 {
        self.older_groups.iter().map(|g| g.rows).sum()
    }
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

/// Parse `(row object, row count)` group rows read before `since` (the bind),
/// counting every ledger row of a group that does not parse.
///
/// A group whose rendered `observedAt` is NOT before `since` contradicts the
/// partition that selected it, and its aggregate columns would reach the
/// filter as if they were a row. That happens only for a row the runner's
/// writer cannot produce (a BC timestamp renders without its era), so such a
/// group is counted as unparseable rows: a read gap, never an edge.
pub(crate) fn parse_older_groups(
    groups: Vec<(serde_json::Value, i64)>,
    since: &str,
) -> (Vec<OlderEdgeGroup>, u64) {
    let since = DateTime::parse_from_rfc3339(since).ok();
    let mut out = Vec::with_capacity(groups.len());
    let mut bad = 0u64;
    for (row, rows) in groups {
        let rows = u64::try_from(rows).unwrap_or(0).max(1);
        match serde_json::from_value::<JourneyEdgeObservation>(row) {
            Ok(edge) => {
                let after = since.zip(DateTime::parse_from_rfc3339(&edge.observed_at).ok());
                if after.is_some_and(|(since, at)| at >= since) {
                    tracing::warn!(
                        "journey read: a ledger row group before `since` renders at {} \
                         (not before it); counted as unparseable",
                        edge.observed_at
                    );
                    bad += rows;
                } else {
                    out.push(OlderEdgeGroup { edge, rows });
                }
            }
            Err(e) => {
                tracing::warn!("journey read: a ledger row group did not parse: {e}");
                bad += rows;
            }
        }
    }
    (out, bad)
}

/// The `$3` bind of a `since` read, or `None` when `since` cannot partition
/// the ledger.
///
/// Floored to the column's microsecond precision. Flooring can only place a
/// row in the full read that the filter then excludes (which `build_graph`
/// counts as usual), never a row the filter keeps among the older groups.
///
/// A leap second (`:60.5`) would break that if rendered as given: chrono orders
/// it before the next second, while Postgres reads it as INTO the next second.
/// So it binds as the last microsecond of its own second, which is below every
/// row the filter keeps.
///
/// RFC 3339 admits year 0 and Postgres has no such year. No row is that old, so
/// such a `since` reads like no `since`, rather than failing the read.
///
/// An offset can carry year 9999 into year 10000, which chrono renders with a
/// leading `+` and Postgres refuses as a time zone. Without the `+` it reads it.
pub(crate) fn since_bind(since: DateTime<Utc>) -> Option<String> {
    if since.year() < 1 {
        return None;
    }
    let since = if since.nanosecond() >= 1_000_000_000 {
        since.with_nanosecond(999_999_000)?
    } else {
        since
    };
    let bind = since.to_rfc3339_opts(SecondsFormat::Micros, true);
    Some(bind.trim_start_matches('+').to_string())
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

/// The older groups of a `since` read: the parsed groups, whether the bound
/// truncated them, and how many ledger rows did not parse.
async fn read_older_groups(
    conn: &deadpool_postgres::Object,
    app_id: &str,
    since: &str,
) -> Result<(Vec<OlderEdgeGroup>, bool, u64), LedgerUnavailable> {
    let bound = EDGE_READ_LIMIT + 1;
    let rows = bounded(
        "journey older-edge read",
        conn.query(OLDER_EDGE_GROUPS_SQL, &[&app_id, &bound, &since]),
    )
    .await?;
    let (rows, truncated) = split_bound(rows, EDGE_READ_LIMIT);
    let groups = rows
        .iter()
        .map(|r| {
            Ok((
                r.try_get::<_, serde_json::Value>(0)?,
                r.try_get::<_, i64>(1)?,
            ))
        })
        .collect::<Result<Vec<_>, tokio_postgres::Error>>()
        .map_err(|e| {
            LedgerUnavailable::Failed(crate::database::pg::pg_err("journey row decode", &e))
        })?;
    let (groups, bad) = parse_older_groups(groups, since);
    Ok((groups, truncated, bad))
}

/// One app's edges and frontier, bounded, with no narrowing at all.
pub(crate) async fn read_ledger(pg: &PgDb, app_id: &str) -> Result<LedgerRead, LedgerUnavailable> {
    read_ledger_at(pg, app_id, None).await
}

/// One app's edges and frontier, bounded, partitioned at `since` (see the
/// module doc). Every row still reaches `build_graph`.
pub(crate) async fn read_ledger_since(
    pg: &PgDb,
    app_id: &str,
    since: DateTime<Utc>,
) -> Result<LedgerRead, LedgerUnavailable> {
    read_ledger_at(pg, app_id, since_bind(since)).await
}

async fn read_ledger_at(
    pg: &PgDb,
    app_id: &str,
    since: Option<String>,
) -> Result<LedgerRead, LedgerUnavailable> {
    let conn = readable_connection(pg).await?;
    let edge_bound = EDGE_READ_LIMIT + 1;
    let frontier_bound = FRONTIER_READ_LIMIT + 1;
    let edge_rows = match &since {
        Some(s) => {
            bounded(
                "journey edge read",
                conn.query(EDGE_READ_SINCE_SQL, &[&app_id, &edge_bound, s]),
            )
            .await?
        }
        None => {
            bounded(
                "journey edge read",
                conn.query(EDGE_READ_SQL, &[&app_id, &edge_bound]),
            )
            .await?
        }
    };
    let (older_groups, older_groups_truncated, bad_older) = match &since {
        Some(s) => read_older_groups(&conn, app_id, s).await?,
        None => (Vec::new(), false, 0),
    };
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
        older_groups,
        frontier,
        edges_truncated,
        older_groups_truncated,
        frontier_truncated,
        rows_unparseable: bad_edges + bad_frontier + bad_older,
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

/// What [`read_timed_edges`] returned, with every gap it knows about.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TimedRead {
    pub rows: Vec<TimedEdge>,
    pub edges_truncated: bool,
    pub older_groups_truncated: bool,
    pub rows_unparseable: u64,
}

/// One app's edges with their timelines, bounded, partitioned at `since` when
/// it is given (a row before it needs no timeline: the filter excludes it, and
/// it is only counted).
pub(crate) async fn read_timed_edges(
    pg: &PgDb,
    app_id: &str,
    since: Option<DateTime<Utc>>,
) -> Result<TimedRead, LedgerUnavailable> {
    let conn = readable_connection(pg).await?;
    let bound = EDGE_READ_LIMIT + 1;
    let since = since.and_then(since_bind);
    let rows = match &since {
        Some(s) => {
            bounded(
                "journey edge-timing read",
                conn.query(EDGE_TIMELINE_READ_SINCE_SQL, &[&app_id, &bound, s]),
            )
            .await?
        }
        None => {
            bounded(
                "journey edge-timing read",
                conn.query(EDGE_TIMELINE_READ_SQL, &[&app_id, &bound]),
            )
            .await?
        }
    };
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
        let (timeline, timeline_unparseable) = timeline_column(
            row.try_get::<_, Option<serde_json::Value>>(1)
                .map_err(|e| e.to_string()),
        );
        out.push(TimedEdge {
            edge,
            timeline,
            timeline_unparseable,
            rows: 1,
        });
    }
    let (older, older_groups_truncated, bad_older) = match &since {
        Some(s) => read_older_groups(&conn, app_id, s).await?,
        None => (Vec::new(), false, 0),
    };
    out.extend(older.into_iter().map(|g| TimedEdge {
        edge: g.edge,
        timeline: None,
        timeline_unparseable: false,
        rows: g.rows,
    }));
    Ok(TimedRead {
        rows: out,
        edges_truncated: truncated,
        older_groups_truncated,
        rows_unparseable: bad + bad_older,
    })
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

/// The `timeline` column as decoded: a DECODE error is an unparseable
/// timeline (counted, `producer_failed`), never "no timeline".
pub(crate) fn timeline_column(
    decoded: Result<Option<serde_json::Value>, String>,
) -> (Option<ChangeTimeline>, bool) {
    match decoded {
        Ok(raw) => parse_timeline(raw),
        Err(e) => {
            tracing::warn!("journey read: the timeline column did not decode: {e}");
            (None, true)
        }
    }
}

/// Resolve one `timeline_ref` row from its two decoded columns. A decode
/// error on `invalidated_at` is UNAVAILABLE (whether the row is live is not
/// known — never "live"); one on `timeline` is UNPARSEABLE (never "no
/// timeline").
pub(crate) fn resolve_ref_row(
    timeline: Result<Option<serde_json::Value>, String>,
    invalidated_at: Result<Option<String>, String>,
) -> TimelineRefLookup {
    match invalidated_at {
        Err(e) => {
            return TimelineRefLookup::Unavailable(LedgerUnavailable::Failed(format!(
                "the edge's invalidated_at did not decode: {e}"
            )))
        }
        Ok(Some(at)) => return TimelineRefLookup::Withdrawn(at),
        Ok(None) => {}
    }
    match timeline_column(timeline) {
        (Some(t), _) => TimelineRefLookup::Found(Box::new(t)),
        (None, true) => TimelineRefLookup::Unparseable,
        (None, false) => TimelineRefLookup::NoTimeline,
    }
}

/// Load one edge observation's timeline by id, scoped to `app_id`.
pub(crate) async fn read_timeline_by_id(pg: &PgDb, app_id: &str, id: &str) -> TimelineRefLookup {
    if uuid::Uuid::parse_str(id).is_err() {
        return TimelineRefLookup::NotFound;
    }
    let conn = match readable_connection(pg).await {
        Ok(c) => c,
        Err(e) => return TimelineRefLookup::Unavailable(e),
    };
    let rows = match bounded(
        "journey timeline_ref read",
        conn.query(TIMELINE_BY_ID_SQL, &[&id, &app_id]),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return TimelineRefLookup::Unavailable(e),
    };
    let Some(row) = rows.first() else {
        return TimelineRefLookup::NotFound;
    };
    resolve_ref_row(
        row.try_get::<_, Option<serde_json::Value>>(0)
            .map_err(|e| e.to_string()),
        row.try_get::<_, Option<String>>(1)
            .map_err(|e| e.to_string()),
    )
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

    /// The text between the outer `FROM` and `ORDER BY`, whitespace-trimmed
    /// per line — the table and its WHERE clause, nothing else.
    fn from_to_order(sql: &str) -> String {
        let from = sql.rfind("FROM ").expect("a FROM") + "FROM ".len();
        let order = sql.find("ORDER BY").expect("an ORDER BY");
        sql.get(from..order)
            .expect("FROM precedes ORDER BY")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn reads_are_bounded_scoped_and_newest_first() {
        // With no `since`, read_ledger reads EDGE_READ_SQL and read_timed_edges
        // reads EDGE_TIMELINE_READ_SQL. Each is scoped to the app and NOTHING
        // else: a filter pushed into SQL hides the rows build_graph turns into
        // doubt.
        for sql in [EDGE_READ_SQL, EDGE_TIMELINE_READ_SQL] {
            assert_eq!(
                from_to_order(sql),
                "project.journey_edge_observations e\nWHERE e.app_id = $1",
                "the WHERE clause must be exactly the app scope: {sql}"
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

    /// A `since` read is a partition: the full reads and the older groups have
    /// complementary predicates, so no row falls in neither, and no other filter
    /// is pushed. Reading the full rows alone is the measured-over-unseen-data
    /// bug, which is why each `since` read is pinned beside the groups.
    #[test]
    fn a_since_read_partitions_the_ledger_and_hides_no_row() {
        for sql in [EDGE_READ_SINCE_SQL, EDGE_TIMELINE_READ_SINCE_SQL] {
            assert_eq!(
                from_to_order(sql),
                "project.journey_edge_observations e\n\
                 WHERE e.app_id = $1\n\
                 AND e.observed_at >= $3::text::timestamptz",
                "{sql}"
            );
            assert!(sql.contains("ORDER BY e.observed_at DESC, e.id DESC"));
            assert!(sql.ends_with("LIMIT $2"));
        }
        assert_eq!(
            from_to_order(OLDER_EDGE_GROUPS_SQL),
            "project.journey_edge_observations e\n\
             WHERE e.app_id = $1\n\
             AND e.observed_at < $3::text::timestamptz\n\
             GROUP BY e.app_id, e.from_node, e.to_node, e.trigger, e.outcome, \
             (e.invalidated_at IS NOT NULL)",
            "the key must hold every input of validate() and of how build_graph \
             places an excluded row"
        );
        assert!(OLDER_EDGE_GROUPS_SQL.ends_with("LIMIT $2"));
        assert!(EDGE_TIMELINE_READ_SINCE_SQL.contains(", e.timeline\n"));
        assert!(!EDGE_READ_SINCE_SQL.contains("e.timeline"));
        assert!(!OLDER_EDGE_GROUPS_SQL.contains("e.timeline"));
    }

    /// A group's representative deserializes through the same struct as a
    /// row, so its object must carry exactly a row object's keys.
    #[test]
    fn a_group_row_object_has_the_keys_of_a_row_object() {
        let keys = |sql: &str| -> Vec<String> {
            let mut keys: Vec<String> = sql
                .split('\'')
                .skip(1)
                .step_by(2)
                .filter(|k| k.chars().all(|c| c.is_ascii_alphabetic()) && *k != "UTC")
                .map(str::to_string)
                .collect();
            keys.sort();
            keys
        };
        let row = keys(EDGE_READ_SQL);
        assert!(row.contains(&"invalidationToken".to_string()), "{row:?}");
        assert_eq!(row, keys(OLDER_EDGE_GROUPS_SQL));
    }

    #[test]
    fn since_binds_floor_to_the_column_and_year_zero_does_not_partition() {
        let parse = |s: &str| {
            DateTime::parse_from_rfc3339(s)
                .expect("a fixture instant")
                .with_timezone(&Utc)
        };
        let bind = since_bind(parse("2026-10-01T10:00:00.123456789Z")).expect("a bind");
        assert_eq!(bind, "2026-10-01T10:00:00.123456Z");
        // Floored, the older groups can never hold a row the filter keeps.
        assert!(parse(&bind) <= parse("2026-10-01T10:00:00.123456789Z"));
        // A leap second binds below every row of the next second, which chrono
        // orders after it and the filter keeps.
        let leap = parse("2026-10-01T10:00:60.5Z");
        let bind = since_bind(leap).expect("a bind");
        assert_eq!(bind, "2026-10-01T10:00:59.999999Z");
        assert!(parse(&bind) < leap);
        // Past year 9999 in UTC: no `+`, which Postgres would read as a zone.
        let far = since_bind(parse("9999-12-31T23:59:59-23:59")).expect("a bind");
        assert!(far.starts_with("10000-"), "{far}");
        assert_eq!(since_bind(parse("0000-06-01T00:00:00Z")), None);
        // An offset can carry a year-1 instant back into year 0 in UTC.
        assert_eq!(since_bind(parse("0001-01-01T00:00:00+14:00")), None);
        assert!(since_bind(parse("0001-01-01T00:00:00Z")).is_some());
    }

    /// A `since` bind after every fixture row above.
    const SINCE: &str = "2026-10-02T00:00:00.000000Z";

    /// One group exactly as Postgres emitted it from [`OLDER_EDGE_GROUPS_SQL`]
    /// over a seeded table (aggregate columns, a withdrawn row, null `by`).
    #[test]
    fn a_group_row_as_postgres_emits_it_parses() {
        let row = serde_json::json!({
            "id": "0004b358-3041-4f58-b464-66e3f8af0aa2", "appId": "app", "runId": "r0",
            "toNode": { "specId": "e", "modelled": true, "stateIds": ["s"],
                        "pageLabel": null, "pathnameTemplate": null },
            "outcome": "changed", "runKind": "agent_action",
            "trigger": { "actionType": "click", "chokePoint": "element_action",
                         "targetRole": "button", "declaredEffect": null,
                         "navigationTrigger": "affordance", "targetFingerprint": "fp1" },
            "fromNode": { "specId": "d", "modelled": true, "stateIds": ["s"],
                          "pageLabel": null, "pathnameTemplate": null },
            "appVersion": "1.0", "observedAt": "2026-09-01T08:19:57.000000Z",
            "invalidatedAt": "2026-09-20T00:00:00.000000Z", "invalidatedBy": null,
            "runnerBuildId": "b", "runnerInstance": "primary",
            "invalidatedReason": "bad run", "invalidationToken": "tok"
        });
        let (groups, bad) = parse_older_groups(vec![(row, 7500)], SINCE);
        assert_eq!(bad, 0);
        assert_eq!(groups[0].rows, 7500);
        assert!(trusted_edge_row(&groups[0].edge).is_ok());
        assert!(groups[0].edge.invalidated_at.is_some());
    }

    /// A group that does not parse counts every ledger row it stands for.
    #[test]
    fn an_unparseable_group_counts_all_its_rows() {
        let good = crate::journey::test_fixtures::edge(1, "a", Some("b"), "push", "changed", "x");
        let (groups, bad) = parse_older_groups(
            vec![
                (serde_json::to_value(&good).expect("edge serializes"), 7),
                (serde_json::json!({ "not": "an edge" }), 5),
            ],
            SINCE,
        );
        assert_eq!(bad, 5);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].rows, 7);
        assert_eq!(groups[0].edge, good);
    }

    /// A group that renders at or after `since` (a BC row, whose era the
    /// rendering drops) is not believed: its rows are a gap, not an edge.
    #[test]
    fn a_group_rendered_at_or_after_since_is_unparseable_not_an_edge() {
        let mut later =
            crate::journey::test_fixtures::edge(1, "a", Some("b"), "push", "changed", "x");
        later.observed_at = SINCE.into();
        let earlier =
            crate::journey::test_fixtures::edge(2, "c", Some("d"), "push", "changed", "y");
        let (groups, bad) = parse_older_groups(
            vec![
                (serde_json::to_value(&later).expect("edge serializes"), 4),
                (serde_json::to_value(&earlier).expect("edge serializes"), 3),
            ],
            SINCE,
        );
        assert_eq!(bad, 4);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].edge, earlier);
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

    // -- Review fixes: decode errors are never "no timeline" or "live" ------

    #[test]
    fn a_decode_error_is_unparseable_or_unavailable_never_absent_or_live() {
        assert_eq!(timeline_column(Err("bad type".into())), (None, true));
        assert!(matches!(
            resolve_ref_row(Ok(None), Err("bad type".into())),
            TimelineRefLookup::Unavailable(LedgerUnavailable::Failed(_))
        ));
        assert_eq!(
            resolve_ref_row(Err("bad type".into()), Ok(None)),
            TimelineRefLookup::Unparseable
        );
        assert_eq!(
            resolve_ref_row(Ok(None), Ok(None)),
            TimelineRefLookup::NoTimeline
        );
        assert_eq!(
            resolve_ref_row(Ok(None), Ok(Some("2026-10-01T00:00:00Z".into()))),
            TimelineRefLookup::Withdrawn("2026-10-01T00:00:00Z".into())
        );
    }

    #[test]
    fn a_timeline_ref_is_scoped_to_its_app() {
        assert!(
            TIMELINE_BY_ID_SQL.contains("AND e.app_id = $2"),
            "a ref from another app must resolve to no row: {TIMELINE_BY_ID_SQL}"
        );
    }
}
