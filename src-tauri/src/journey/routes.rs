//! The journey query door — `GET /apps/{app_id}/journey/*` (plan
//! `2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time`,
//! Phase 2 runner half, plus Phase 4's `edge-timing`).
//!
//! | route | query | answer value |
//! |---|---|---|
//! | `health` | — | [`super::health::JourneyHealthResponse`] (Phase 1; not an envelope) |
//! | `graph` | filter | `JourneyGraph` |
//! | `reachable` | `from` + filter | `JourneyReachable` |
//! | `steps` | `from`, `to` + filter | `JourneySteps` |
//! | `dead-ends` | filter | `JourneyDeadEnds` |
//! | `prior-knowledge` | filter | `JourneyPriorKnowledge` |
//! | `frontier` | `node`? + filter | `JourneyFrontier` |
//! | `edge-timing` | `from`, `to` + filter | [`JourneyEdgeTimingAnswer`] |
//!
//! "filter" is `?since=<RFC 3339>&app_version=<v>&run_kind=<agent_action|explorer|passive_session>`;
//! any other parameter is a 400, so a misspelt filter can never be silently
//! dropped. Every applied filter is echoed (`filterEcho`).
//!
//! ## The answer is the shared `Observation<T>` envelope (plan D6)
//!
//! - a POSITIVE answer (a path found, a node listed) is `measured`, its value
//!   the shared query result; the gaps it was computed over ride in
//!   `provenance.coverage.unmeasured` (a DEGRADED measurement);
//! - a NEGATIVE answer is `unknown` whenever
//!   `JourneySearchFrontier::unsettled_because` names a code (ledger not
//!   writing, subject never observed, open frontier, unobserved departures)
//!   or the read itself has a gap (a truncated or unparseable read); only
//!   otherwise is it `absent`, with
//!   `JourneySearchFrontier::unmeasured(&coverage, subject_observed)` in
//!   `provenance.coverage.unmeasured` — so the shared constructor itself
//!   refuses `absent` over an open frontier. Settledness is never re-derived
//!   here;
//! - a non-measured answer carries the full query result in
//!   `provenance.source.result`, so the frontier entries that would settle it
//!   are named in the answer itself;
//! - the shared `JourneyCoverage` block rides INSIDE `provenance.coverage`,
//!   beside `considered / measured / unmeasured` (D6), so the coverage line is
//!   present whatever the status.
//!
//! A read that could not be made (schema absent, pool / probe / read failure,
//! timeout) is `unknown` — `ledger_schema_absent` or `producer_failed` —
//! NEVER an empty graph. Read failures carry no coverage counts at all (they
//! are unknown, not zero).
//!
//! The pure answer builders ([`answer_graph`] … [`answer_edge_timing`]) take
//! the rows and `evaluated_at`, so the same ledger and filter give
//! byte-identical JSON (acceptance 5).
//!
//! No origin-guard roster entry: these routes are called by ui-bridge-mcp and
//! agents (non-browser), which the default `RoutePolicy::EnforceDoors` admits
//! (plan Phase 2, resolved vet note).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, SecondsFormat, Utc};
use qontinui_types::journey::{
    build_graph, dead_ends, frontier, reachable_from, requires_prior_knowledge, steps_to,
    JourneyCoverage, JourneyEdgeObservation, JourneyFilter, JourneyGraph, JourneyLedgerHealth,
    JourneySearchFrontier, RunKind,
};
use qontinui_vision_core::{
    edge_timing, EdgeTimingSample, Observation, ObservationCoverage, Producer, Provenance,
    UnknownCode, UnmeasuredDimension,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::mcp::types::ApiState;

use super::health;
use super::read::{
    read_ledger, read_timed_edges, LedgerRead, LedgerUnavailable, TimedEdge, EDGE_READ_LIMIT,
    FRONTIER_READ_LIMIT,
};

// The route-local answer value lives in the LIB crate
// (`qontinui_runner_lib::journey_wire`) so `schema_export` can register it.
pub use qontinui_runner_lib::journey_wire::JourneyEdgeTimingAnswer;

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

/// Every query parameter any journey route takes. Unknown keys are refused
/// (400) so `?sinse=` cannot pose as an unfiltered read; each route also
/// refuses a known key it does not use ([`JourneyParams::only`]).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JourneyParams {
    pub since: Option<String>,
    pub app_version: Option<String>,
    pub run_kind: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub node: Option<String>,
}

/// A request the route refuses before reading anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BadRequest(pub String);

impl IntoResponse for BadRequest {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "bad_request", "detail": self.0 })),
        )
            .into_response()
    }
}

impl JourneyParams {
    /// Refuse a parameter this route does not read.
    fn only(&self, allowed: &[&str]) -> Result<(), BadRequest> {
        let present = [
            ("from", self.from.is_some()),
            ("to", self.to.is_some()),
            ("node", self.node.is_some()),
        ];
        for (name, is_set) in present {
            if is_set && !allowed.contains(&name) {
                return Err(BadRequest(format!(
                    "`{name}` is not a parameter of this route (accepted: {}, since, app_version, run_kind)",
                    if allowed.is_empty() { "none other".to_string() } else { allowed.join(", ") }
                )));
            }
        }
        Ok(())
    }

    fn required<'a>(&self, name: &str, v: &'a Option<String>) -> Result<&'a str, BadRequest> {
        match v.as_deref() {
            Some(s) if !s.is_empty() => Ok(s),
            _ => Err(BadRequest(format!(
                "`{name}` is required (a JourneyNode key)"
            ))),
        }
    }

    /// The shared filter the parameters spell.
    pub(crate) fn filter(&self) -> Result<JourneyFilter, BadRequest> {
        let since = self
            .since
            .as_deref()
            .map(|s| {
                DateTime::parse_from_rfc3339(s)
                    .map(|d| d.with_timezone(&Utc))
                    .map_err(|e| BadRequest(format!("`since` is not RFC 3339 ({e}): {s}")))
            })
            .transpose()?;
        let run_kind = self
            .run_kind
            .as_deref()
            .map(|k| {
                serde_json::from_value::<RunKind>(Value::String(k.to_string())).map_err(|_| {
                    BadRequest(format!(
                        "`run_kind` must be agent_action, explorer or passive_session, got {k:?}"
                    ))
                })
            })
            .transpose()?;
        Ok(JourneyFilter {
            since,
            app_version: self.app_version.clone(),
            run_kind,
        })
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// Gaps of the read itself, independent of the graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ReadGaps {
    pub edges_truncated: bool,
    pub frontier_truncated: bool,
    pub rows_unparseable: u64,
}

impl ReadGaps {
    fn of(read: &LedgerRead) -> Self {
        Self {
            edges_truncated: read.edges_truncated,
            frontier_truncated: read.frontier_truncated,
            rows_unparseable: read.rows_unparseable,
        }
    }

    /// The read's gaps as unmeasured dimensions. A truncated frontier read
    /// left frontier rows unread, and every frontier row is an OPEN entry, so
    /// its code is `frontier_open`.
    fn dimensions(&self) -> Vec<UnmeasuredDimension> {
        let mut out = Vec::new();
        if self.edges_truncated {
            out.push(UnmeasuredDimension::new(
                format!("edges_beyond_read_bound_{EDGE_READ_LIMIT}"),
                1,
                UnknownCode::InputMissing,
            ));
        }
        if self.frontier_truncated {
            out.push(UnmeasuredDimension::new(
                format!("frontier_beyond_read_bound_{FRONTIER_READ_LIMIT}"),
                1,
                UnknownCode::FrontierOpen,
            ));
        }
        if self.rows_unparseable > 0 {
            out.push(UnmeasuredDimension::new(
                "rows_unparseable",
                self.rows_unparseable,
                UnknownCode::ProducerFailed,
            ));
        }
        out
    }
}

/// The closed `UnknownCode` for a wire string the shared query crate returns.
/// Every string `qontinui_types::journey` can return is a member (pinned in
/// qontinui-schemas against the registry); an unmatched one is a contract
/// break, reported as `producer_failed` rather than guessed.
pub(crate) fn unknown_code(code: &str) -> UnknownCode {
    serde_json::from_value(Value::String(code.to_string())).unwrap_or(UnknownCode::ProducerFailed)
}

fn producer(query: &str) -> Producer {
    Producer::new(format!("runner/journey/{query}"), env!("CARGO_PKG_VERSION"))
}

fn parse_ts(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// Serialize an envelope and merge the journey coverage block INTO
/// `provenance.coverage` (D6). The two key sets are disjoint
/// (`considered / measured / unmeasured` vs the `JourneyCoverage` fields).
fn finish<T: Serialize>(obs: Observation<T>, coverage: Option<&JourneyCoverage>) -> Value {
    let mut v = serde_json::to_value(&obs).unwrap_or_else(|e| {
        serde_json::json!({ "status": "unknown", "unknown": {
            "code": "producer_failed", "detail": format!("envelope serialization failed: {e}") } })
    });
    if let (Some(cov), Some(slot)) = (
        coverage,
        v.pointer_mut("/provenance/coverage")
            .and_then(Value::as_object_mut),
    ) {
        if let Ok(Value::Object(extra)) = serde_json::to_value(cov) {
            for (k, val) in extra {
                slot.entry(k).or_insert(val);
            }
        }
    }
    v
}

fn source_of(query: &str, app_id: &str, result: Option<Value>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("query".into(), Value::String(query.to_string()));
    m.insert("appId".into(), Value::String(app_id.to_string()));
    if let Some(r) = result {
        m.insert("result".into(), r);
    }
    m
}

fn detail_for(code: UnknownCode, query: &str, frontier: &JourneySearchFrontier) -> String {
    match code {
        UnknownCode::FrontierOpen => format!(
            "{query}: the searched part of the graph has {} open frontier entr{} (affordances \
             seen and never activated); the negative cannot be settled until they are explored — \
             they are listed in provenance.source.result.frontier",
            frontier.open_entries,
            if frontier.open_entries == 1 {
                "y"
            } else {
                "ies"
            }
        ),
        UnknownCode::ToNodeUnobserved => format!(
            "{query}: {} departure(s) in the searched part never had their destination observed",
            frontier.unobserved_departures
        ),
        UnknownCode::NoObservations => format!(
            "{query}: the ledger holds no observation this question can be answered from (an \
             empty ledger under this filter, or a node it has never seen)"
        ),
        UnknownCode::LedgerSchemaAbsent => {
            format!("{query}: the journey tables do not exist in this database")
        }
        UnknownCode::LedgerWriteFailing => format!(
            "{query}: the journey ledger is not being written, so a negative over it could \
             describe a stale graph as a finished one (see provenance.coverage.ledger)"
        ),
        UnknownCode::InputMissing => format!(
            "{query}: the read bound was reached ({EDGE_READ_LIMIT} edges); older edges were \
             not read, so a negative cannot be settled — narrow with ?since="
        ),
        other => format!("{query}: not settled ({other})"),
    }
}

/// Inputs every graph-backed answer shares.
pub(crate) struct AnswerCtx<'a> {
    pub query: &'static str,
    pub app_id: &'a str,
    pub gaps: ReadGaps,
    pub evaluated_at: DateTime<Utc>,
}

/// The one rule (module doc): positive → measured; negative → unknown when
/// `unsettled_because` or a read gap names a code, else absent.
fn answer<T: Serialize>(
    ctx: &AnswerCtx<'_>,
    result: &T,
    positive: bool,
    frontier: &JourneySearchFrontier,
    coverage: &JourneyCoverage,
    subject_observed: bool,
) -> Value {
    // The shared crate names every gap of the searched part — the ledger
    // state, an empty ledger, open frontier, unmeasured departures — so the
    // envelope's own `absent` guard refuses a negative over any of them.
    let mut unmeasured: Vec<UnmeasuredDimension> = frontier
        .unmeasured(coverage, subject_observed)
        .into_iter()
        .map(|(dim, count, code)| UnmeasuredDimension::new(dim, count, unknown_code(code)))
        .collect();
    unmeasured.extend(ctx.gaps.dimensions());
    let considered = coverage.nodes_observed;
    let unsettled_nodes = frontier.nodes.len() as u64;
    let obs_coverage = ObservationCoverage {
        considered,
        measured: if unmeasured.is_empty() {
            considered
        } else {
            considered.saturating_sub(unsettled_nodes.max(1))
        },
        unmeasured,
    };
    let mut prov = Provenance::new(producer(ctx.query), ctx.evaluated_at, obs_coverage);
    if let Some(at) = parse_ts(coverage.newest_edge_at.as_deref()) {
        prov = prov.with_observed_at(at);
    }
    let result_value = serde_json::to_value(result).unwrap_or(Value::Null);

    if positive {
        let prov = prov.with_source(source_of(ctx.query, ctx.app_id, None));
        return finish(Observation::measured(result_value, prov), Some(coverage));
    }
    let prov = prov.with_source(source_of(ctx.query, ctx.app_id, Some(result_value)));
    let code = frontier
        .unsettled_because(coverage, subject_observed)
        .map(unknown_code)
        .or_else(|| prov.coverage.unmeasured.first().map(|d| d.code));
    if let Some(code) = code {
        let detail = detail_for(code, ctx.query, frontier);
        return finish(
            Observation::<Value>::unknown(code, detail, prov),
            Some(coverage),
        );
    }
    match Observation::<Value>::absent(prov.clone()) {
        Ok(obs) => finish(obs, Some(coverage)),
        // Considered nothing: never absence.
        Err(e) => finish(
            Observation::<Value>::unknown(
                UnknownCode::NoObservations,
                format!("{}: {e}", ctx.query),
                prov,
            ),
            Some(coverage),
        ),
    }
}

/// The answer to a read that could not be made. Schema absence keeps a
/// coverage block (the tables do not exist, so there are no rows and the
/// ledger state says why); a read failure carries NO counts.
pub(crate) fn answer_unavailable(
    query: &'static str,
    app_id: &str,
    filter: &JourneyFilter,
    ledger: &JourneyLedgerHealth,
    why: &LedgerUnavailable,
    evaluated_at: DateTime<Utc>,
) -> Value {
    let mut source = source_of(query, app_id, None);
    source.insert(
        "filterEcho".into(),
        serde_json::to_value(filter).unwrap_or(Value::Null),
    );
    source.insert(
        "ledger".into(),
        serde_json::to_value(ledger).unwrap_or(Value::Null),
    );
    let prov = Provenance::new(
        producer(query),
        evaluated_at,
        ObservationCoverage::default(),
    )
    .with_source(source);
    match why {
        LedgerUnavailable::SchemaAbsent => {
            let graph = build_graph(&[], &[], ledger, filter);
            finish(
                Observation::<Value>::unknown(
                    UnknownCode::LedgerSchemaAbsent,
                    format!("{query}: {}", ledger.detail),
                    prov,
                ),
                Some(&graph.coverage),
            )
        }
        LedgerUnavailable::Failed(e) => finish(
            Observation::<Value>::unknown(
                UnknownCode::ProducerFailed,
                format!("{query}: the journey ledger could not be read: {e}"),
                prov,
            ),
            None,
        ),
    }
}

/// `graph`: measured iff any edge was observed under the filter. The whole-
/// graph frontier (from `dead_ends`) is what a negative is settled against.
pub(crate) fn answer_graph(ctx: &AnswerCtx<'_>, graph: &JourneyGraph) -> Value {
    let whole = frontier(graph, None).frontier;
    let positive = graph.coverage.edges_observed > 0;
    answer(ctx, graph, positive, &whole, &graph.coverage, true)
}

/// `reachable?from=`: measured iff the start reaches some OTHER node.
pub(crate) fn answer_reachable(ctx: &AnswerCtx<'_>, graph: &JourneyGraph, from: &str) -> Value {
    let r = reachable_from(graph, from);
    let positive = r.reachable.iter().any(|n| n.node_key != r.from_key);
    answer(ctx, &r, positive, &r.frontier, &r.coverage, r.from_observed)
}

/// `steps?from=&to=`: measured iff a path was observed.
pub(crate) fn answer_steps(
    ctx: &AnswerCtx<'_>,
    graph: &JourneyGraph,
    from: &str,
    to: &str,
) -> Value {
    let s = steps_to(graph, from, to);
    let positive = s.steps.is_some();
    let subject = s.from_observed && s.to_observed;
    answer(ctx, &s, positive, &s.frontier, &s.coverage, subject)
}

/// `dead-ends`: measured iff at least one dead end was found.
pub(crate) fn answer_dead_ends(ctx: &AnswerCtx<'_>, graph: &JourneyGraph) -> Value {
    let d = dead_ends(graph);
    let positive = !d.dead_ends.is_empty();
    answer(ctx, &d, positive, &d.frontier, &d.coverage, true)
}

/// `prior-knowledge`: measured iff at least one node needs prior knowledge.
pub(crate) fn answer_prior_knowledge(ctx: &AnswerCtx<'_>, graph: &JourneyGraph) -> Value {
    let p = requires_prior_knowledge(graph);
    let positive = !p.nodes.is_empty();
    answer(ctx, &p, positive, &p.frontier, &p.coverage, true)
}

/// `frontier?node=`: the shared `frontier()` read — measured iff an open,
/// listable entry exists; an empty list is `absent` only when its search
/// frontier is settled (rejected frontier rows, unmeasured departures and a
/// never-observed node all keep it `unknown`).
pub(crate) fn answer_frontier(
    ctx: &AnswerCtx<'_>,
    graph: &JourneyGraph,
    node: Option<&str>,
) -> Value {
    let f = frontier(graph, node);
    let positive = !f.entries.is_empty();
    answer(ctx, &f, positive, &f.frontier, &f.coverage, f.node_observed)
}

fn keeps(filter: &JourneyFilter, edge: &JourneyEdgeObservation) -> bool {
    if let Some(since) = filter.since {
        match parse_ts(Some(&edge.observed_at)) {
            Some(at) if at >= since => {}
            _ => return false,
        }
    }
    if let Some(v) = &filter.app_version {
        if edge.app_version.as_deref() != Some(v.as_str()) {
            return false;
        }
    }
    if filter.run_kind.is_some_and(|k| edge.run_kind != k) {
        return false;
    }
    true
}

/// `edge-timing?from=&to=`: the distribution of one edge's timelines. Edge
/// identity is the node keys (`JourneyNode::key()`), never jsonb equality.
/// Measured iff at least one live observation of the edge passed the filter;
/// rows without a timeline count toward `n` and are reported as unmeasured.
pub(crate) fn answer_edge_timing(
    ctx: &AnswerCtx<'_>,
    rows: &[TimedEdge],
    from: &str,
    to: &str,
    filter: &JourneyFilter,
    ledger: &JourneyLedgerHealth,
) -> Value {
    let mut samples = Vec::new();
    let mut filtered_out = 0u64;
    let mut invalidated = 0u64;
    let mut unparseable = 0u64;
    for r in rows {
        let e = &r.edge;
        let same_edge =
            e.from_node.key() == from && e.to_node.as_ref().is_some_and(|n| n.key() == to);
        if !same_edge {
            continue;
        }
        if e.invalidated_at.is_some() {
            invalidated += 1;
            continue;
        }
        if !keeps(filter, e) {
            filtered_out += 1;
            continue;
        }
        let Some(observed_at) = parse_ts(Some(&e.observed_at)) else {
            unparseable += 1;
            continue;
        };
        if r.timeline_unparseable {
            unparseable += 1;
        }
        samples.push(EdgeTimingSample {
            observation_id: e.id.clone(),
            observed_at,
            app_version: e.app_version.clone(),
            timeline: r.timeline.clone(),
        });
    }
    let timing = edge_timing(&samples);
    let n = timing.n;
    let without = timing.without_timeline;
    let newest = timing.newest_observed_at.clone();
    let answer_value = JourneyEdgeTimingAnswer {
        from_key: from.to_string(),
        to_key: to.to_string(),
        filter_echo: filter.clone(),
        edges_filtered_out: filtered_out,
        edges_invalidated: invalidated,
        timelines_unparseable: unparseable,
        timing,
        ledger: ledger.clone(),
    };
    let mut unmeasured = ctx.gaps.dimensions();
    if without > 0 {
        unmeasured.push(UnmeasuredDimension::new(
            "timeline",
            without,
            UnknownCode::InputMissing,
        ));
    }
    if unparseable > 0 {
        unmeasured.push(UnmeasuredDimension::new(
            "timeline_unparseable",
            unparseable,
            UnknownCode::ProducerFailed,
        ));
    }
    let ledger_code = match ledger.state {
        qontinui_types::journey::LedgerState::Writing => None,
        qontinui_types::journey::LedgerState::SchemaAbsent => Some(UnknownCode::LedgerSchemaAbsent),
        qontinui_types::journey::LedgerState::WriteFailing => Some(UnknownCode::LedgerWriteFailing),
    };
    if let Some(code) = ledger_code {
        unmeasured.push(UnmeasuredDimension::new("ledger", 1, code));
    }
    let measured = n.saturating_sub(without);
    let obs_cov = ObservationCoverage {
        considered: n,
        measured: if unmeasured.is_empty() {
            n
        } else {
            measured.min(n)
        },
        unmeasured,
    };
    let mut prov = Provenance::new(producer(ctx.query), ctx.evaluated_at, obs_cov);
    if let Some(at) = parse_ts(newest.as_deref()) {
        prov = prov.with_observed_at(at);
    }
    let value = serde_json::to_value(&answer_value).unwrap_or(Value::Null);
    if n > 0 {
        let prov = prov.with_source(source_of(ctx.query, ctx.app_id, None));
        return finish(Observation::measured(value, prov), None);
    }
    let prov = prov.with_source(source_of(ctx.query, ctx.app_id, Some(value)));
    let code = ledger_code
        .or_else(|| ctx.gaps.dimensions().first().map(|d| d.code))
        .unwrap_or(UnknownCode::NoObservations);
    let detail = format!(
        "edge-timing: no live observation of {from:?} -> {to:?} under this filter \
         ({filtered_out} filtered out, {invalidated} withdrawn)"
    );
    finish(Observation::<Value>::unknown(code, detail, prov), None)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// The live ledger state, probing first when no settled answer is cached
/// (the same evidence `/journey/health` answers from).
async fn live_ledger(state: &Arc<ApiState>, app_id: &str) -> JourneyLedgerHealth {
    health::get_journey_health(State(Arc::clone(state)), Path(app_id.to_string()))
        .await
        .0
        .ledger
}

fn now() -> DateTime<Utc> {
    // Truncated to millis so the wire form is the same width every time.
    let n = Utc::now();
    DateTime::parse_from_rfc3339(&n.to_rfc3339_opts(SecondsFormat::Millis, true))
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or(n)
}

/// Validate, read, build the graph, and hand it to `f`; or answer why not.
/// `validate` runs before the database is touched.
async fn graph_route(
    state: Arc<ApiState>,
    app_id: String,
    params: JourneyParams,
    query: &'static str,
    allowed: &[&str],
    validate: impl FnOnce(&JourneyParams) -> Result<(), BadRequest>,
    f: impl FnOnce(&AnswerCtx<'_>, &JourneyGraph, &JourneyParams) -> Value,
) -> Response {
    let filter = match params
        .only(allowed)
        .and_then(|_| validate(&params))
        .and_then(|_| params.filter())
    {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let ledger = live_ledger(&state, &app_id).await;
    let body = match read_ledger(&state.app_state.pg_db, &app_id).await {
        Err(why) => answer_unavailable(query, &app_id, &filter, &ledger, &why, now()),
        Ok(read) => {
            let graph = build_graph(&read.edges, &read.frontier, &ledger, &filter);
            let ctx = AnswerCtx {
                query,
                app_id: &app_id,
                gaps: ReadGaps::of(&read),
                evaluated_at: now(),
            };
            f(&ctx, &graph, &params)
        }
    };
    Json(body).into_response()
}

fn no_validation(_: &JourneyParams) -> Result<(), BadRequest> {
    Ok(())
}

/// `GET /apps/{app_id}/journey/graph`.
pub async fn get_journey_graph(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    Query(params): Query<JourneyParams>,
) -> Response {
    graph_route(
        state,
        app_id,
        params,
        "graph",
        &[],
        no_validation,
        |ctx, g, _| answer_graph(ctx, g),
    )
    .await
}

/// `GET /apps/{app_id}/journey/reachable?from=`.
pub async fn get_journey_reachable(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    Query(params): Query<JourneyParams>,
) -> Response {
    graph_route(
        state,
        app_id,
        params,
        "reachable",
        &["from"],
        |p| p.required("from", &p.from).map(|_| ()),
        |ctx, g, p| answer_reachable(ctx, g, p.from.as_deref().unwrap_or_default()),
    )
    .await
}

/// `GET /apps/{app_id}/journey/steps?from=&to=`.
pub async fn get_journey_steps(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    Query(params): Query<JourneyParams>,
) -> Response {
    graph_route(
        state,
        app_id,
        params,
        "steps",
        &["from", "to"],
        |p| {
            p.required("from", &p.from)?;
            p.required("to", &p.to).map(|_| ())
        },
        |ctx, g, p| {
            answer_steps(
                ctx,
                g,
                p.from.as_deref().unwrap_or_default(),
                p.to.as_deref().unwrap_or_default(),
            )
        },
    )
    .await
}

/// `GET /apps/{app_id}/journey/dead-ends`.
pub async fn get_journey_dead_ends(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    Query(params): Query<JourneyParams>,
) -> Response {
    graph_route(
        state,
        app_id,
        params,
        "dead-ends",
        &[],
        no_validation,
        |ctx, g, _| answer_dead_ends(ctx, g),
    )
    .await
}

/// `GET /apps/{app_id}/journey/prior-knowledge`.
pub async fn get_journey_prior_knowledge(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    Query(params): Query<JourneyParams>,
) -> Response {
    graph_route(
        state,
        app_id,
        params,
        "prior-knowledge",
        &[],
        no_validation,
        |ctx, g, _| answer_prior_knowledge(ctx, g),
    )
    .await
}

/// `GET /apps/{app_id}/journey/frontier[?node=]`.
pub async fn get_journey_frontier(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    Query(params): Query<JourneyParams>,
) -> Response {
    graph_route(
        state,
        app_id,
        params,
        "frontier",
        &["node"],
        no_validation,
        |ctx, g, p| answer_frontier(ctx, g, p.node.as_deref()),
    )
    .await
}

/// `GET /apps/{app_id}/journey/edge-timing?from=&to=` (plan Phase 4).
pub async fn get_journey_edge_timing(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    Query(params): Query<JourneyParams>,
) -> Response {
    let checked = params.only(&["from", "to"]).and_then(|_| {
        let filter = params.filter()?;
        let from = params.required("from", &params.from)?.to_string();
        let to = params.required("to", &params.to)?.to_string();
        Ok((filter, from, to))
    });
    let (filter, from, to) = match checked {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let ledger = live_ledger(&state, &app_id).await;
    let body = match read_timed_edges(&state.app_state.pg_db, &app_id).await {
        Err(why) => answer_unavailable("edge-timing", &app_id, &filter, &ledger, &why, now()),
        Ok((rows, truncated, bad)) => {
            let ctx = AnswerCtx {
                query: "edge-timing",
                app_id: &app_id,
                gaps: ReadGaps {
                    edges_truncated: truncated,
                    frontier_truncated: false,
                    rows_unparseable: bad,
                },
                evaluated_at: now(),
            };
            answer_edge_timing(&ctx, &rows, &from, &to, &filter, &ledger)
        }
    };
    Json(body).into_response()
}

/// Every journey route, merged into the main router beside
/// `spec_api::routes()`.
pub fn router() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/apps/{app_id}/journey/health",
            get(health::get_journey_health),
        )
        .route("/apps/{app_id}/journey/graph", get(get_journey_graph))
        .route(
            "/apps/{app_id}/journey/reachable",
            get(get_journey_reachable),
        )
        .route("/apps/{app_id}/journey/steps", get(get_journey_steps))
        .route(
            "/apps/{app_id}/journey/dead-ends",
            get(get_journey_dead_ends),
        )
        .route(
            "/apps/{app_id}/journey/prior-knowledge",
            get(get_journey_prior_knowledge),
        )
        .route("/apps/{app_id}/journey/frontier", get(get_journey_frontier))
        .route(
            "/apps/{app_id}/journey/edge-timing",
            get(get_journey_edge_timing),
        )
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
