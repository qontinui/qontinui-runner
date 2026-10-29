//! `POST /apps/{app_id}/journey/explore` — build the journey graph on
//! purpose, within a stated budget (plan
//! `2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time`,
//! Phase 3, runner half).
//!
//! A deterministic, frontier-first BFS with NO model call anywhere:
//!
//! 1. take an unfiltered snapshot through the same snapshot route agents use
//!    and ask the journey worker which node it resolved ([`super::capture::barrier`])
//!    — every iteration starts by contacting the app, so an app that died
//!    mid-run stops the run `app_unreachable`, never `frontier_exhausted`;
//! 2. pick the OLDEST `not_yet_activated` frontier entry that plan D4's closed
//!    predicate allows ([`select_next`]), within the depth budget, whose
//!    route fits the action budget;
//! 3. get to its node by replaying the shortest observed path (`steps_to`)
//!    over the D4-SAFE edges only — an observed edge an agent took through a
//!    `write` button is never replayed;
//! 4. re-check D4 against the LIVE element ([`live_activation`]) and activate
//!    it through the SAME SDK action handler agents use, inside
//!    [`super::capture::as_explorer`] — so the Phase 1 producer writes the edge
//!    with `run_kind = explorer`. There is no private write path.
//!
//! It repeats until a budget is spent or no activatable entry is left, and
//! returns — and persists to `project.journey_explorations` (qontinui-web
//! migrations `journey_02_explorations`,
//! `journey_03_explorations_stop_unsettled` and
//! `journey_04_explorations_stop_detail_any_stop`) — the run's budget, spend,
//! stop reason (with the [`Unsettled`] breakdown as `stop_detail` whenever it
//! records anything — see [`stop_detail_column`]) and accounting.
//! `frontier_exhausted` is the only stop reason under which a later
//! `not_reachable` could be claimed for the explored component.
//!
//! ## D4, restated (not `isNavigationElement`)
//!
//! A declared `write` / `destructive` effect is never activated; a declared
//! `read` is; with no declaration, only an explicit navigation role
//! (`link` / `menuitem` / `tab`) or type (`link` / `menuitem`) is; anything
//! else is `effect_undeclared`. That is [`super::frontier::d4_reason`] over
//! [`super::node::is_navigation_affordance`] — the predicate the frontier
//! already stores — and NOT ui-bridge's `isNavigationElement`, which also
//! counts a `sidebar` / `nav` parent context and would click a "Delete" button
//! rendered in a sidebar.
//!
//! ## Accounting — nothing vanishes
//!
//! Every frontier entry the run SAW (at any node it stood on) ends in exactly
//! one bucket: activated (`affordancesActivated` — the app acted on it) or
//! remaining, by reason (`frontierRemainingByReason`, all six reasons, zeros
//! included; an activation the app failed is `activation_failed`). So
//! `affordancesActivated + Σ frontierRemainingByReason = affordancesSeen`, by
//! construction ([`Accounting`]). An activatable entry only a BUDGET kept
//! from the run is reported `budget_exhausted` in the answer; unroutable and
//! component entries keep their reason. The frontier row itself is left
//! `not_yet_activated`, because the upsert never moves a row out of
//! `budget_exhausted` and the next run would otherwise never pick it again.
//!
//! `edgesAdded` is a different count: the run's action events the producer's
//! queue accepted (activations, replays and navigations), each of which
//! closes into one ledger row with this `runId` — see
//! [`ExplorationResult::edges_added`] for when the two can differ.
//!
//! ## `frontier_exhausted` is earned
//!
//! It is claimed only when a FRESH confirming snapshot answers after the
//! selection found nothing, the selection re-run on the ledger as that
//! confirming look left it still finds nothing, every entry the run activated
//! has left the ledger's open frontier, every ledger read was complete,
//! parseable and from a `writing` ledger, no journey write failed while the
//! run went, no activatable entry was skipped as unroutable, undrivable or
//! failed (this run's or an earlier one's `activation_failed` row), no D4-safe
//! component action was left undriven, every action event the run took
//! reached the producer, and every answer the app gave could be read.
//! Otherwise the run stops `frontier_unsettled` — an unknown with its own
//! name — and an [`Unsettled`] block saying why ([`settle_stop`]). An app
//! that ANSWERED with a failure, or whose answer could not be read, is never
//! `app_unreachable`: mid-run that, too, is `frontier_unsettled`.
//!
//! ## Auth wall
//!
//! The explorer drives whatever session the connected app already has; it
//! mints nothing and asks for nothing. A snapshot carries no HTML input type,
//! so a password field is not structurally visible; the closed predicate is
//! [`is_auth_wall`]: an SDK answer carrying an authentication refusal code, or
//! a node whose route template / page label is a login page.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use qontinui_types::ir::IrEffect;
use qontinui_types::journey::{
    build_graph, steps_to, FrontierEntry, FrontierReason, JourneyEdgeObservation, JourneyFilter,
    JourneyGraph, JourneyGraphEdge, JourneyLedgerHealth, JourneyNode, LedgerState, RunKind,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::mcp::types::ApiState;

use super::capture::{
    self, snapshot_view, BarrierMiss, ExplorerRun, NodeReading, BARRIER_TIMEOUT, RUNNER_BUILD_ID,
};
use super::cursor::CursorKey;
use super::frontier::d4_reason;
use super::node::{extract_affordances, is_navigation_affordance, unknown_node, AffordanceIndex};

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// Where the run starts: a URL the explorer navigates to first, or a node key
/// the app must already be at or have an observed safe path to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExploreStart {
    Url(String),
    Node(String),
}

impl ExploreStart {
    /// The `start` column / answer field: `{"url": …}` or `{"node": …}`.
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::Url(u) => json!({ "url": u }),
            Self::Node(k) => json!({ "node": k }),
        }
    }
}

/// The three budget dials. All REQUIRED — there is no default, so no run is
/// ever unbounded by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExploreBudget {
    pub max_actions: u32,
    pub max_depth: u32,
    pub max_wall_clock_s: u32,
}

/// A validated `POST /journey/explore` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExploreRequest {
    pub start: ExploreStart,
    pub budget: ExploreBudget,
}

/// The longest wall-clock budget a run may ask for: one hour. The route
/// answers when the run ends, so a caller's HTTP request is held that long.
pub(crate) const MAX_WALL_CLOCK_S: u32 = 3600;

/// The body keys the route accepts; any other key is a 422.
const REQUEST_KEYS: [&str; 4] = ["start", "max_actions", "max_depth", "max_wall_clock_s"];

/// A refused request: the status and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal(pub StatusCode, pub String);

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn unprocessable(msg: impl Into<String>) -> Refusal {
    Refusal(StatusCode::UNPROCESSABLE_ENTITY, msg.into())
}

/// One budget field: present, an integer, at least `min`, and within the
/// column's `INTEGER`.
fn budget_field(
    body: &serde_json::Map<String, Value>,
    name: &str,
    min: u32,
) -> Result<u32, Refusal> {
    let v = body.get(name).ok_or_else(|| {
        unprocessable(format!(
            "{name} is required: the explorer has no default budget (max_actions, max_depth and \
             max_wall_clock_s must all be given)"
        ))
    })?;
    let n = v
        .as_u64()
        .ok_or_else(|| unprocessable(format!("{name} must be a non-negative integer")))?;
    if n < u64::from(min) || n > i32::MAX as u64 {
        return Err(unprocessable(format!(
            "{name} must be between {min} and {}",
            i32::MAX
        )));
    }
    Ok(n as u32)
}

/// Parse and validate the body. Not JSON → 400; anything else wrong — a
/// missing budget field above all — → 422 naming it.
pub(crate) fn parse_explore_request(body: &[u8]) -> Result<ExploreRequest, Refusal> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| Refusal(StatusCode::BAD_REQUEST, format!("body is not JSON: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| unprocessable("body must be a JSON object"))?;
    if let Some(unknown) = obj.keys().find(|k| !REQUEST_KEYS.contains(&k.as_str())) {
        return Err(unprocessable(format!(
            "unknown field `{unknown}`; accepted: {}",
            REQUEST_KEYS.join(", ")
        )));
    }
    let start = obj.get("start").ok_or_else(|| {
        unprocessable("start is required: {\"url\": …} or {\"node\": <node key>}")
    })?;
    let start = parse_start(start)?;
    let budget = ExploreBudget {
        max_actions: budget_field(obj, "max_actions", 1)?,
        max_depth: budget_field(obj, "max_depth", 0)?,
        max_wall_clock_s: budget_field(obj, "max_wall_clock_s", 1)?,
    };
    if budget.max_wall_clock_s > MAX_WALL_CLOCK_S {
        return Err(unprocessable(format!(
            "max_wall_clock_s must be at most {MAX_WALL_CLOCK_S} (one hour): the route holds its \
             request open for the whole run"
        )));
    }
    Ok(ExploreRequest { start, budget })
}

fn parse_start(v: &Value) -> Result<ExploreStart, Refusal> {
    let bad =
        || unprocessable("start must be {\"url\": <non-empty string>} or {\"node\": <node key>}");
    let obj = v.as_object().ok_or_else(bad)?;
    if obj.len() != 1 {
        return Err(bad());
    }
    let text = |k: &str| {
        obj.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    match (text("url"), text("node")) {
        (Some(u), None) => Ok(ExploreStart::Url(u)),
        (None, Some(n)) => Ok(ExploreStart::Node(n)),
        _ => Err(bad()),
    }
}

// ---------------------------------------------------------------------------
// D4 against a live element
// ---------------------------------------------------------------------------

/// Is an observed edge one the explorer may REPLAY? The same D4 predicate as
/// activation ([`live_activation`]), read off the record.
///
/// D4's navigation arm is role OR type, and a trigger records the role but
/// not the element type. So the record answers it two ways:
///
/// - an edge the EXPLORER took (`run_kind = explorer`) passed D4 on the live
///   element — type arm included — when it was taken: an `<a onClick>` typed
///   `link` with no role, activated by one run, is routable for the next;
/// - any other edge passes only on what it recorded — a declared `read`, or
///   an explicit navigation role. A type-only link an agent clicked stays
///   unroutable (fail-closed: the record cannot show its type), and an entry
///   behind it is counted `unroutable`, never silently dropped.
///
/// A declared `write` / `destructive` effect is never replayed, whoever took
/// it, and every replay re-checks D4 on the LIVE element before acting.
pub(crate) fn replayable(edge: &JourneyEdgeObservation) -> bool {
    let trigger = &edge.trigger;
    let passed_live = edge.run_kind == RunKind::Explorer;
    trigger.target_fingerprint.is_some()
        && d4_reason(
            trigger.declared_effect,
            passed_live || is_navigation_affordance(trigger.target_role.as_deref(), None),
        ) == FrontierReason::NotYetActivated
}

/// What to fire on a live element, by D4 — or why it must not be activated.
///
/// - declared `read` → the smallest custom action declared `read` (a
///   read-declared element exposes no undeclared standard action — see
///   `node::element_declared_effect`);
/// - no declaration, explicit navigation role/type → `click`, when the
///   element offers it ([`super::node::ElementAffordance::offers_click`], read
///   off the same snapshot layer the index was built from);
/// - anything else → the frontier reason it stays under.
///
/// Two refusals here are the explorer's own, not the ledger's: a navigation
/// element that offers no `click`, and a read-declared element with no read
/// action. Both answer `effect_undeclared` (nothing safe to fire), while the
/// ledger row stays `not_yet_activated` ([`ledger_reason`]) — so the caller
/// counts them as [`Unsettled::undrivable`], never as a classification.
pub(crate) fn live_activation(
    affordances: &AffordanceIndex,
    element_id: &str,
) -> Result<String, FrontierReason> {
    let Some(el) = affordances.elements.get(element_id) else {
        return Err(FrontierReason::NotYetActivated);
    };
    let reason = d4_reason(el.affordance.declared_effect, el.affordance.navigation);
    if reason != FrontierReason::NotYetActivated {
        return Err(reason);
    }
    match el.affordance.declared_effect {
        Some(IrEffect::Read) => el
            .action_effects
            .iter()
            .find(|(_, e)| **e == Some(IrEffect::Read))
            .map(|(id, _)| id.clone())
            .ok_or(FrontierReason::EffectUndeclared),
        _ if el.offers_click => Ok("click".to_string()),
        _ => Err(FrontierReason::EffectUndeclared),
    }
}

/// The reason the producer writes for a live element's frontier row (plan D4
/// over its affordance): what the ledger says after the snapshot that showed
/// it. `None` when the element is not on the page.
pub(crate) fn ledger_reason(
    affordances: &AffordanceIndex,
    element_id: &str,
) -> Option<FrontierReason> {
    affordances
        .elements
        .get(element_id)
        .map(|el| d4_reason(el.affordance.declared_effect, el.affordance.navigation))
}

/// The element carrying `fingerprint` in a snapshot's affordances — the
/// smallest id when several fingerprint identically (deterministic).
pub(crate) fn element_with_fingerprint(
    index: &AffordanceIndex,
    fingerprint: &str,
) -> Option<String> {
    index
        .elements
        .iter()
        .filter(|(_, e)| e.affordance.fingerprint == fingerprint)
        .map(|(id, _)| id.clone())
        .min()
}

// ---------------------------------------------------------------------------
// Auth wall
// ---------------------------------------------------------------------------

/// Authentication-refusal codes an SDK answer may carry (top level, under
/// `error`, `data` or `error_detail`).
pub(crate) const AUTH_REFUSAL_CODES: [&str; 3] =
    ["UNAUTHORIZED", "UNAUTHENTICATED", "AUTH_REQUIRED"];

/// Route templates of a login page, compared lower-cased without a trailing
/// `/`. Closed: a page that is not one of these is not an auth wall.
pub(crate) const LOGIN_TEMPLATES: [&str; 5] = [
    "/login",
    "/signin",
    "/sign-in",
    "/auth/login",
    "/auth/signin",
];

/// App-declared page labels of a login page (labels are slugs).
pub(crate) const LOGIN_LABELS: [&str; 3] = ["login", "signin", "sign-in"];

/// Does an SDK answer carry an authentication refusal?
pub(crate) fn answer_is_auth_refusal(answer: &Value) -> bool {
    let code_in = |v: Option<&Value>| {
        v.into_iter().any(|v| {
            ["code", "errorCode"].iter().any(|k| {
                v.get(*k)
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| AUTH_REFUSAL_CODES.contains(&c))
            })
        })
    };
    code_in(Some(answer))
        || code_in(answer.get("error"))
        || code_in(answer.get("data"))
        || code_in(answer.get("error_detail"))
}

/// Is this node a login page? See [`LOGIN_TEMPLATES`] / [`LOGIN_LABELS`].
pub(crate) fn is_auth_wall(node: &JourneyNode) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('/').to_ascii_lowercase();
    node.pathname_template
        .as_deref()
        .is_some_and(|t| LOGIN_TEMPLATES.contains(&norm(t).as_str()))
        || node
            .page_label
            .as_deref()
            .is_some_and(|l| LOGIN_LABELS.contains(&norm(l).as_str()))
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Why a run stopped — the closed set `journey_explorations.stopped_because`
/// CHECKs (qontinui-web `journey_02_explorations`, widened by
/// `journey_03_explorations_stop_unsettled`).
///
/// Only [`Self::FrontierExhausted`] licenses a later `not_reachable` for the
/// explored component; every other word is a run that stopped while
/// activatable frontier remained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    BudgetActions,
    BudgetDepth,
    BudgetTime,
    FrontierExhausted,
    /// No entry was left to pick, yet the frontier is NOT known to be
    /// exhausted: the run ended with activatable frontier it could not read,
    /// route or activate. An UNKNOWN, named as one — the answer's
    /// [`Unsettled`] block and `stopExplanation` say which.
    FrontierUnsettled,
    AppUnreachable,
    AuthWall,
}

impl StopReason {
    #[cfg(test)]
    pub const ALL: [StopReason; 7] = [
        Self::BudgetActions,
        Self::BudgetDepth,
        Self::BudgetTime,
        Self::FrontierExhausted,
        Self::FrontierUnsettled,
        Self::AppUnreachable,
        Self::AuthWall,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::BudgetActions => "budget_actions",
            Self::BudgetDepth => "budget_depth",
            Self::BudgetTime => "budget_time",
            Self::FrontierExhausted => "frontier_exhausted",
            Self::FrontierUnsettled => "frontier_unsettled",
            Self::AppUnreachable => "app_unreachable",
            Self::AuthWall => "auth_wall",
        }
    }

    fn is_budget(self) -> bool {
        matches!(
            self,
            Self::BudgetActions | Self::BudgetDepth | Self::BudgetTime
        )
    }
}

/// How to get from the current node to an entry's node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Route {
    /// Navigate to the start URL first (the entry's node is not reachable
    /// from where the app is, but is from the start).
    pub via_start_url: bool,
    /// The observed, D4-safe edges to replay, in order.
    pub path: Vec<JourneyGraphEdge>,
}

impl Route {
    /// Actions the route plus the activation itself spends.
    pub(crate) fn cost(&self) -> u32 {
        u32::from(self.via_start_url) + self.path.len() as u32 + 1
    }
}

/// Entries the selection passed over, by why (reported, never hidden).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotActivated {
    /// Activatable, but at a node deeper than `max_depth` from the start.
    pub beyond_depth: u32,
    /// Activatable and within depth, but with no observed D4-safe route from
    /// the app's node (nor via the start URL).
    pub unroutable: u32,
    /// D4-safe component actions (`not_yet_activated`) in the start's
    /// component or at a node the run stood on: never activated by this
    /// explorer (it drives elements through the element-action handler only).
    pub component_actions: u32,
}

/// Why an empty selection is NOT an exhausted frontier: activatable work the
/// run could not see or could not do. Any non-zero field stops the run
/// `frontier_unsettled` instead of `frontier_exhausted` (see [`settle_stop`]).
/// The fields are measured whatever stopped the run; they decide only
/// whether an exhaustion may be claimed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unsettled {
    /// Activatable entries with no observed safe route (last selection) —
    /// those in the start's component, and those at a node the run stood on
    /// that its routing graph cannot reach from the start.
    pub unroutable: u32,
    /// D4-safe component actions the explorer does not drive (last
    /// selection): activatable frontier it left by construction.
    pub component_actions: u32,
    /// Entries this run attempted that did not become an edge it could
    /// account for: the replayed path did not hold, the element was not on
    /// the live page, or the app failed the activation.
    pub attempts_failed: u32,
    /// Entries this run reached whose ledger row is still `not_yet_activated`
    /// but whose live element offers nothing D4 lets it fire: a navigation
    /// element with no `click`, or a read-declared element with no read
    /// action ([`live_activation`]). The ledger keeps them open, so the run
    /// left activatable frontier it could not drive.
    pub undrivable: u32,
    /// `activation_failed` rows (last selection) in the start's component or
    /// at a node the run stood on — this run's failures and any earlier run's
    /// or agent's. Such a row is retried within budget ([`select_next`]), but
    /// while one stays open the frontier is not exhausted: an observation
    /// never moves a row out of `activation_failed`, only an edge from it does.
    pub activation_failed_open: u32,
    /// A ledger read hit its row bound: the run did not see the whole
    /// frontier.
    pub ledger_read_truncated: bool,
    /// Ledger rows that did not parse, or frontier rows that failed their
    /// contract — each could be an open entry anywhere.
    pub rows_unreadable: u64,
    /// Whether an exhausted frontier failed its confirmation: `Some(true)`
    /// when the final snapshot could not be tied to a fresh node (runner-side:
    /// a dropped event or a timeout), the app answered it with a failure or
    /// an unreadable answer, or the ledger could not be re-read after it;
    /// `Some(false)` when it was confirmed. `None` (JSON `null`): no
    /// exhaustion was being confirmed, so this was not measured.
    pub exhaustion_unconfirmed: Option<bool>,
    /// Entries still open when the ledger was re-read after the confirming
    /// look: activatable ones the re-run selection found (routable, beyond
    /// depth, or unroutable), plus every entry this run ACTIVATED whose row
    /// still reads `not_yet_activated` / `activation_failed` (its edge never
    /// reached the ledger). `None` (JSON `null`): the re-run selection did
    /// not run, so this was not measured.
    pub open_at_confirmation: Option<u32>,
    /// Whether the look taken AFTER the stop — the closing snapshot, the
    /// barrier tying it to a fresh node, and the ledger re-read — failed to
    /// complete, whatever stopped the run: `Some(true)` when any part of it
    /// failed (a runner-side miss — a dropped snapshot event, a barrier
    /// timeout, the producer gone —, an app that answered it with a failure,
    /// whose answer could not be read, that could not be reached or was
    /// behind a login, a ledger re-read that failed, or the post-stop grace
    /// running out); `Some(false)` when it completed. `None` (JSON `null`):
    /// no closing look was taken, because the run had ALREADY stopped
    /// `app_unreachable` / `auth_wall` before it. A run whose stop word
    /// became `app_unreachable` / `auth_wall` AT the closing look — an
    /// exhausted frontier whose confirming snapshot found the app gone or
    /// behind a login — took the look and reads `Some(true)`, beside
    /// `exhaustionUnconfirmed: true`. Under a BUDGET stop this is the one
    /// place such a failure is recorded: the stop word stays the budget, and
    /// the reasons of what was seen were not refreshed after the last action.
    pub closing_look_failed: Option<bool>,
    /// A ledger read during the run reported the ledger NOT `writing`: what
    /// the run read may lack its own writes.
    pub ledger_not_writing: bool,
    /// Journey writes that failed or were not attempted, process-wide, while
    /// the run was going (the health counters at its end minus at its
    /// start). Any of them may be one of this run's edges or frontier rows;
    /// another app's failure counts too — fail-closed.
    pub ledger_write_failures: u64,
    /// The app's answer to an ACTION could not be read (a timeout, an
    /// unreadable frame, a non-JSON or non-2xx reply): it may have acted, so
    /// where it went is unknown.
    pub action_outcome_unknown: bool,
    /// The app's answer to a SNAPSHOT could not be read the same way, so
    /// where it was and what it offered were not read.
    pub snapshot_outcome_unknown: bool,
    /// The run's action events the producer's queue could not hold: actions
    /// the app saw whose edge (and frontier move) the ledger will never show.
    pub edge_events_dropped: u64,
    /// The app ANSWERED a snapshot with a failure, so where it was and what
    /// it offered were not read. It is reachable — never `app_unreachable`.
    pub snapshot_refused_by_app: bool,
}

impl Unsettled {
    pub(crate) fn any(&self) -> bool {
        self.unroutable > 0
            || self.component_actions > 0
            || self.attempts_failed > 0
            || self.undrivable > 0
            || self.activation_failed_open > 0
            || self.ledger_read_truncated
            || self.rows_unreadable > 0
            || self.exhaustion_unconfirmed == Some(true)
            || self.open_at_confirmation.is_some_and(|n| n > 0)
            || self.closing_look_failed == Some(true)
            || self.edge_events_dropped > 0
            || self.snapshot_refused_by_app
            || self.ledger_not_writing
            || self.ledger_write_failures > 0
            || self.action_outcome_unknown
            || self.snapshot_outcome_unknown
    }
}

/// An optional measure for a message: the value, or `not measured`.
fn measured<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "not measured".to_string(), |v| v.to_string())
}

/// The selection's answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Choice {
    Next {
        entry: Box<FrontierEntry>,
        depth: u32,
        route: Route,
    },
    /// No candidate the budget allows: `budget_actions` (one is routable but
    /// does not fit), `budget_depth` (only deeper ones are left), or
    /// `frontier_exhausted` — which [`settle_stop`] still checks.
    Stop(StopReason),
}

/// A selection and what it saw.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Selection {
    pub choice: Choice,
    pub skipped: NotActivated,
    /// Activatable entries only the BUDGET kept from this run: routable ones
    /// within depth (the chosen one included) and ones beyond `max_depth`.
    /// Exactly these — no unroutable or component entry — are reported
    /// `budget_exhausted` when a budget stops the run.
    pub budget_left: BTreeSet<EntryKey>,
    /// `activation_failed` rows in the start's component or at a node the run
    /// stood on, attempted this run or not ([`Unsettled::activation_failed_open`]).
    pub failed_open: u32,
}

/// `(node_key, affordance_fingerprint)` — a frontier row's identity.
pub(crate) type EntryKey = (String, String);

fn entry_key(e: &FrontierEntry) -> EntryKey {
    (e.node_key.clone(), e.affordance_fingerprint.clone())
}

/// The routing graph: the ledger's edges whose trigger is D4-replayable
/// ([`replayable`]), with every frontier row. Depths and routes are computed
/// over it only, so no route ever replays an unsafe activation.
pub(crate) fn routing_graph(
    edges: &[JourneyEdgeObservation],
    frontier: &[FrontierEntry],
    ledger: &JourneyLedgerHealth,
) -> JourneyGraph {
    let safe: Vec<JourneyEdgeObservation> =
        edges.iter().filter(|e| replayable(e)).cloned().collect();
    build_graph(&safe, frontier, ledger, &JourneyFilter::default())
}

/// Hop count from `from` to `to` over the graph's navigation edges, with the
/// path; `Some((0, []))` when they are the same node.
fn hops(graph: &JourneyGraph, from: &str, to: &str) -> Option<(u32, Vec<JourneyGraphEdge>)> {
    if from == to {
        return Some((0, Vec::new()));
    }
    let s = steps_to(graph, from, to);
    s.steps.map(|n| (n as u32, s.path))
}

/// Pick the next entry to activate — deterministic, no clock, no I/O.
///
/// Candidates are frontier rows `not_yet_activated` — or `activation_failed`,
/// a RETRY — not yet attempted this run, not declared `write` /
/// `destructive` (fail-closed even if a row's reason disagrees), not a
/// component action, at a node in the start's component within `max_depth`
/// hops. Fresh entries come before retries; within each, OLDEST first
/// (`firstSeenAt`, then node key, then fingerprint); the first whose route
/// fits the remaining action budget is chosen. A retry is an ordinary
/// activation: it costs budget like any other and is re-checked against D4
/// on the live element.
///
/// Every `activation_failed` row in the start's component or at a `visited`
/// node is also COUNTED ([`Selection::failed_open`]), retried or not: an
/// observation never moves a row out of `activation_failed`, so while one
/// stays, the frontier is not exhausted.
///
/// What is passed over is COUNTED, never dropped: a component action in the
/// start's component or at a `visited` node (`component_actions`), and an
/// entry with no route — in the component but unreachable from where the app
/// is, or at a `visited` node the routing graph cannot reach from the start
/// (`unroutable`). An entry at a node outside the start's component that the
/// run never stood on is not counted: no observed D4-safe path leads there
/// from the start, so it is not this run's frontier, and `frontier_exhausted`
/// claims nothing about it — the claim, and any later `not_reachable`, is
/// about the explored component only.
///
/// No candidate fits → `budget_actions`; none routable but some too deep →
/// `budget_depth`; otherwise `frontier_exhausted` (unsettled by
/// [`settle_stop`] when anything was skipped or unseen).
#[allow(clippy::too_many_arguments)]
pub(crate) fn select_next(
    graph: &JourneyGraph,
    start_key: &str,
    start_url: bool,
    current_key: &str,
    max_depth: u32,
    actions_left: u32,
    attempted: &BTreeSet<EntryKey>,
    visited: &BTreeSet<String>,
) -> Selection {
    let mut skipped = NotActivated::default();
    let mut budget_left = BTreeSet::new();
    let not_unsafe = |e: &&FrontierEntry| {
        !matches!(
            e.declared_effect,
            Some(IrEffect::Write | IrEffect::Destructive)
        )
    };
    let failed_open = graph
        .nodes
        .iter()
        .flat_map(|n| n.frontier.iter())
        .filter(|e| e.reason == FrontierReason::ActivationFailed)
        .filter(not_unsafe)
        .filter(|e| visited.contains(&e.node_key) || hops(graph, start_key, &e.node_key).is_some())
        .count() as u32;
    let mut candidates: Vec<&FrontierEntry> = graph
        .nodes
        .iter()
        .flat_map(|n| n.frontier.iter())
        .filter(|e| {
            matches!(
                e.reason,
                FrontierReason::NotYetActivated | FrontierReason::ActivationFailed
            )
        })
        .filter(|e| !attempted.contains(&entry_key(e)))
        .filter(not_unsafe)
        .collect();
    let retry = |e: &FrontierEntry| e.reason == FrontierReason::ActivationFailed;
    candidates.sort_by(|a, b| {
        (
            retry(a),
            &a.first_seen_at,
            &a.node_key,
            &a.affordance_fingerprint,
        )
            .cmp(&(
                retry(b),
                &b.first_seen_at,
                &b.node_key,
                &b.affordance_fingerprint,
            ))
    });

    let mut chosen: Option<(Box<FrontierEntry>, u32, Route)> = None;
    let mut routable_but_too_costly = false;
    for entry in candidates {
        let from_start = hops(graph, start_key, &entry.node_key);
        let stood_there = visited.contains(&entry.node_key);
        if entry.affordance_fingerprint.starts_with("component:") {
            if from_start.is_some() || stood_there {
                skipped.component_actions += 1;
            }
            continue;
        }
        let Some((depth, _)) = from_start else {
            if stood_there {
                // The run stood here, yet its routing graph has no safe path
                // from the start (it arrived some way the ledger cannot
                // replay): activatable work it cannot do.
                skipped.unroutable += 1;
            }
            continue;
        };
        if depth > max_depth {
            skipped.beyond_depth += 1;
            budget_left.insert(entry_key(entry));
            continue;
        }
        let route = match hops(graph, current_key, &entry.node_key) {
            Some((_, path)) => Route {
                via_start_url: false,
                path,
            },
            None if start_url => Route {
                via_start_url: true,
                path: hops(graph, start_key, &entry.node_key)
                    .map(|(_, p)| p)
                    .unwrap_or_default(),
            },
            None => {
                skipped.unroutable += 1;
                continue;
            }
        };
        budget_left.insert(entry_key(entry));
        if chosen.is_some() {
            continue;
        }
        if route.cost() > actions_left {
            routable_but_too_costly = true;
            continue;
        }
        chosen = Some((Box::new(entry.clone()), depth, route));
    }
    let choice = match chosen {
        Some((entry, depth, route)) => Choice::Next {
            entry,
            depth,
            route,
        },
        None if routable_but_too_costly => Choice::Stop(StopReason::BudgetActions),
        None if skipped.beyond_depth > 0 => Choice::Stop(StopReason::BudgetDepth),
        None => Choice::Stop(StopReason::FrontierExhausted),
    };
    Selection {
        choice,
        skipped,
        budget_left,
        failed_open,
    }
}

/// The honest stop for an empty selection.
///
/// `frontier_exhausted` is the ONE reason that licenses a later
/// `not_reachable` for the component, so it is claimed only when nothing
/// activatable was skipped, failed or unseen. Otherwise the run stops
/// `frontier_unsettled`, with [`Unsettled`] naming why — its own word, because
/// every measured word would assert something false of it (`budget_actions`
/// a spent budget that was not spent; `app_unreachable` an app that
/// answered). Unknown is first-class: it is never dressed as a measured
/// reason.
pub(crate) fn settle_stop(
    reason: StopReason,
    unsettled: &Unsettled,
) -> (StopReason, Option<String>) {
    if reason != StopReason::FrontierExhausted || !unsettled.any() {
        return (reason, None);
    }
    let u = unsettled;
    (
        StopReason::FrontierUnsettled,
        Some(format!(
            "frontier NOT exhausted — activatable work this run could not do or could not see: \
             {} unroutable, {} component action(s) not driven, {} failed attempt(s), {} \
             undrivable entr(ies), {} activation_failed row(s) open, ledger read truncated: {}, \
             {} unreadable row(s), exhaustion unconfirmed: {}, entr(ies) open at the \
             confirming read: {}, closing look failed: {}, {} action event(s) dropped, snapshot \
             refused by the app: {}, ledger not writing: {}, {} journey write(s) failed, action \
             outcome unknown: {}, snapshot outcome unknown: {}",
            u.unroutable,
            u.component_actions,
            u.attempts_failed,
            u.undrivable,
            u.activation_failed_open,
            u.ledger_read_truncated,
            u.rows_unreadable,
            measured(u.exhaustion_unconfirmed),
            measured(u.open_at_confirmation),
            measured(u.closing_look_failed),
            u.edge_events_dropped,
            u.snapshot_refused_by_app,
            u.ledger_not_writing,
            u.ledger_write_failures,
            u.action_outcome_unknown,
            u.snapshot_outcome_unknown
        )),
    )
}

/// Entries this run ACTIVATED (the app acted) whose frontier row the ledger
/// still holds open (`not_yet_activated` / `activation_failed`): the edge
/// the activation should have written — which deletes that row — never
/// reached the ledger. The re-run selection cannot see them (they are in
/// `attempted`), so they are counted here.
pub(crate) fn activated_still_open(accounting: &Accounting, graph: &JourneyGraph) -> u32 {
    let open: BTreeSet<EntryKey> = graph
        .nodes
        .iter()
        .flat_map(|n| n.frontier.iter())
        .filter(|e| {
            matches!(
                e.reason,
                FrontierReason::NotYetActivated | FrontierReason::ActivationFailed
            )
        })
        .map(entry_key)
        .collect();
    accounting
        .seen
        .iter()
        .filter(|(k, d)| **d == Disposition::Activated && open.contains(*k))
        .count() as u32
}

// ---------------------------------------------------------------------------
// Accounting
// ---------------------------------------------------------------------------

/// What became of one seen entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// The run activated it through the action handler and the app acted.
    Activated,
    /// Still on the frontier, for this reason (`activation_failed` when the
    /// app failed the run's activation).
    Remaining(FrontierReason),
}

/// Every frontier entry the run saw, and what became of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Accounting {
    pub seen: BTreeMap<EntryKey, Disposition>,
}

impl Accounting {
    /// Record the frontier at the nodes the run stood on. An activated entry
    /// stays activated; a remaining one takes the latest reason.
    pub(crate) fn see(&mut self, graph: &JourneyGraph, visited: &BTreeSet<String>) {
        for node in graph.nodes.iter().filter(|n| visited.contains(&n.key)) {
            for e in &node.frontier {
                let d = self
                    .seen
                    .entry(entry_key(e))
                    .or_insert(Disposition::Remaining(e.reason));
                if let Disposition::Remaining(r) = d {
                    *r = e.reason;
                }
            }
        }
    }

    pub(crate) fn activated(&mut self, key: EntryKey) {
        self.seen.insert(key, Disposition::Activated);
    }

    pub(crate) fn remaining(&mut self, key: EntryKey, reason: FrontierReason) {
        match self.seen.get(&key) {
            Some(Disposition::Activated) => {}
            _ => {
                self.seen.insert(key, Disposition::Remaining(reason));
            }
        }
    }

    /// Seen entries the run activated.
    pub(crate) fn activated_count(&self) -> u32 {
        self.seen
            .values()
            .filter(|d| **d == Disposition::Activated)
            .count() as u32
    }

    /// Remaining entries by reason, every reason present (zeros included).
    /// Under a BUDGET stop, an entry still `not_yet_activated` that is in
    /// `budget_left` — one only the budget kept from the run — is reported
    /// `budget_exhausted`; every other leftover keeps its reason. An entry in
    /// `attempts_finished` is never relabelled: the run's attempt at it
    /// FINISHED — by any outcome: acted, failed, unroutable, not on the page,
    /// undrivable, reclassified — so the budget is not what kept it from the
    /// run. The one entry whose attempt a halt cut short (the run's
    /// `in_flight` entry) is not in that set, and under a budget stop is
    /// `budget_exhausted`.
    pub(crate) fn remaining_by_reason(
        &self,
        stopped: StopReason,
        budget_left: &BTreeSet<EntryKey>,
        attempts_finished: &BTreeSet<EntryKey>,
    ) -> BTreeMap<String, u32> {
        let mut out: BTreeMap<String, u32> = FrontierReason::ALL
            .iter()
            .map(|r| (r.as_str().to_string(), 0))
            .collect();
        for (key, d) in &self.seen {
            if let Disposition::Remaining(r) = d {
                let r = if stopped.is_budget()
                    && *r == FrontierReason::NotYetActivated
                    && budget_left.contains(key)
                    && !attempts_finished.contains(key)
                {
                    FrontierReason::BudgetExhausted
                } else {
                    *r
                };
                *out.entry(r.as_str().to_string()).or_default() += 1;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Why contacting the app ended the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DriveFailure {
    /// No answer, or a transport/relay refusal: no UI was reached.
    Unreachable(String),
    /// The app answered with an authentication refusal.
    AuthWall(String),
    /// The app ANSWERED a snapshot, with a failure: it is reachable (never
    /// `app_unreachable`), but where it is and what it offers were not read.
    AppAnswered(String),
    /// The request may have reached the app, but its answer could not be
    /// read (`failure_origin: "unknown"` — a timeout, an unreadable frame, a
    /// non-JSON or non-2xx reply). Never `app_unreachable`: it may have acted.
    OutcomeUnknown(String),
    /// The RUNNER failed on its own side (its own route's answer could not be
    /// read): nothing is known about the app — a runner-side failure with no
    /// row, never `app_unreachable`.
    Runner(String),
}

/// What an attempted action did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActResult {
    /// The handler acted (the producer opens an edge for it).
    Acted,
    /// The app answered that it attempted it and failed (the producer opens
    /// an `error` edge).
    Failed(String),
}

/// What the ledger holds for the app, and how completely it was read.
pub(crate) struct LedgerRows {
    pub edges: Vec<JourneyEdgeObservation>,
    pub frontier: Vec<FrontierEntry>,
    pub ledger: JourneyLedgerHealth,
    /// Either read hit its row bound.
    pub truncated: bool,
    /// Rows that did not parse.
    pub rows_unparseable: u64,
}

/// The run's action events as the producer's queue took them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EdgeEvents {
    pub enqueued: u64,
    pub dropped: u64,
}

/// Everything the loop does to the outside world. The live driver
/// ([`LiveDriver`]) calls the SAME route handlers agents call; tests drive a
/// simulated app.
#[async_trait::async_trait]
pub(crate) trait ExploreDriver: Send {
    /// An unfiltered snapshot of the app, through the snapshot route (which
    /// also feeds it to the journey producer).
    async fn snapshot(&mut self) -> Result<Value, DriveFailure>;
    /// Fire `action` on element `element_id` through the element-action route.
    async fn activate(&mut self, element_id: &str, action: &str)
        -> Result<ActResult, DriveFailure>;
    /// Navigate to `url` through the page-navigate route.
    async fn navigate(&mut self, url: &str) -> Result<ActResult, DriveFailure>;
    /// Wait (at most `deadline`, capped at [`BARRIER_TIMEOUT`]) for the
    /// producer to process everything so far; what it last resolved for the
    /// explorer's cursor on this app, or why it could not be asked.
    async fn current_node(&mut self, deadline: Duration) -> Result<NodeReading, BarrierMiss>;
    /// The ledger rows for this app.
    async fn ledger_rows(&mut self) -> Result<LedgerRows, String>;
    /// Tell the producer the explorer's cursor no longer knows where the app
    /// is (its last node is a previous run's), waiting until it has.
    async fn forget_position(&mut self) -> Result<(), BarrierMiss>;
    /// The run's action events, as enqueued to (or dropped by) the producer.
    fn edge_events(&self) -> EdgeEvents;
    /// Journey writes that failed or were not attempted so far, process-wide
    /// (monotonic; the run compares its end with its start).
    fn ledger_write_failures(&self) -> u64;
    /// Time since the run started.
    fn elapsed(&self) -> Duration;
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// How long the work AFTER a stop (the confirming snapshot, its barrier and
/// one ledger read) may take, in total — whatever stopped the run, the time
/// budget included.
pub(crate) const POST_STOP_GRACE: Duration = Duration::from_secs(5);

/// What a finished loop reports.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LoopOutcome {
    pub stopped_because: StopReason,
    pub stop_explanation: String,
    pub stop_node: Option<JourneyNode>,
    pub spent_actions: u32,
    pub spent_depth: u32,
    pub accounting: Accounting,
    pub not_activated: NotActivated,
    pub unsettled: Unsettled,
    pub budget_left: BTreeSet<EntryKey>,
    /// Entries whose attempt FINISHED, by any outcome (every attempted entry
    /// but the one a halt cut short): never relabelled `budget_exhausted`.
    pub attempts_finished: BTreeSet<EntryKey>,
    pub edge_events: EdgeEvents,
}

/// Why a run ends WITHOUT a row: none of the seven stop reasons would be true.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NoRowKind {
    /// The runner itself failed (the producer or the ledger read).
    Internal,
    /// The app answered the start-URL navigation, or the run's first
    /// snapshot, with a failure: it is reachable, so `app_unreachable` would
    /// be false, and no exploring began, so no budget or frontier word is
    /// true either. A 422.
    StartRefused,
    /// The app's answer to the start-URL navigation, or to the run's first
    /// snapshot, could not be read: it may have acted, and nothing was
    /// explored, so no stop word is true. A 502.
    StartUnknown,
    /// A node start [`check_node_start`] refused, with its status (a 422 for
    /// a start that cannot be reached or is behind an auth wall, a 502 / 503
    /// for an answer it could not read or an app or producer it could not
    /// ask). Nothing was acted on.
    StartCheck(StatusCode),
}

/// A run that ended without a row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NoRow {
    pub kind: NoRowKind,
    pub detail: String,
    pub spent_actions: u32,
    pub edge_events: EdgeEvents,
}

struct Run<'a, D: ExploreDriver> {
    d: &'a mut D,
    budget: ExploreBudget,
    /// The start node's key, once the run has one.
    start_key: Option<String>,
    spent_actions: u32,
    spent_depth: u32,
    accounting: Accounting,
    attempted: BTreeSet<EntryKey>,
    /// The entry whose attempt is under way: set when it is chosen, cleared
    /// at the top of every loop iteration — so every `continue` and every
    /// finished action clears it, whatever the outcome — and left set only
    /// when the run RETURNS mid-attempt (a halt cut the attempt short).
    in_flight: Option<EntryKey>,
    visited: BTreeSet<String>,
    node: Option<JourneyNode>,
    affordances: AffordanceIndex,
    /// The last look is newer than the last action: the node, snapshot and
    /// affordances describe the app as it is now.
    fresh: bool,
    /// The cursor's resolved-snapshot count at the last look.
    snapshots_seen: Option<u64>,
    unsettled: Unsettled,
    budget_left: BTreeSet<EntryKey>,
    /// [`ExploreDriver::ledger_write_failures`] when the run started.
    write_failures_at_start: u64,
}

enum Halt {
    Stop(StopReason, String),
    NoRow(NoRowKind, String),
    /// Mid-run only: the app refused the navigation back to the start URL.
    /// The loop counts it as a failed attempt at the entry it was routing to
    /// and carries on; it never ends a run.
    DetourRefused,
}

fn internal(detail: impl Into<String>) -> Halt {
    Halt::NoRow(NoRowKind::Internal, detail.into())
}

/// One look's deadline. Every wait inside the look — the baseline barrier,
/// the snapshot, the barrier after it — gets only what is LEFT of it, so a
/// look never overruns its deadline by a second full wait.
#[derive(Debug, Clone, Copy)]
struct LookDeadline {
    until: tokio::time::Instant,
    /// The deadline is what the TIME BUDGET has left: a wait it cuts short
    /// is the budget running out (`budget_time`), not a runner failure.
    budget_bound: bool,
}

impl LookDeadline {
    fn new(within: Duration, budget_bound: bool) -> Self {
        Self {
            until: tokio::time::Instant::now() + within,
            budget_bound,
        }
    }

    fn left(&self) -> Duration {
        self.until
            .saturating_duration_since(tokio::time::Instant::now())
    }
}

impl<D: ExploreDriver> Run<'_, D> {
    fn wall_clock(&self) -> Duration {
        Duration::from_secs(u64::from(self.budget.max_wall_clock_s))
    }

    /// Time the budget has left (zero when spent).
    fn remaining(&self) -> Duration {
        self.wall_clock().saturating_sub(self.d.elapsed())
    }

    fn time_up(&self) -> Halt {
        Halt::Stop(
            StopReason::BudgetTime,
            format!("{} s spent", self.budget.max_wall_clock_s),
        )
    }

    fn check_time(&self) -> Result<(), Halt> {
        if self.remaining().is_zero() {
            return Err(self.time_up());
        }
        Ok(())
    }

    /// A wait inside a look ran out of its deadline. When the deadline was
    /// the time budget's remainder, the budget is what ran out.
    fn wait_ran_out(&self, dl: LookDeadline, what: &str) -> Halt {
        if dl.budget_bound {
            self.time_up()
        } else {
            internal(format!("{what} did not answer within the post-stop grace"))
        }
    }

    /// Ask the producer where the app is, within what is left of `dl`.
    ///
    /// A barrier that times out on a wait the BUDGET shortened (less than
    /// [`BARRIER_TIMEOUT`] was left of it) is the time budget running out —
    /// `budget_time`, with a row. One that times out on its full
    /// [`BARRIER_TIMEOUT`], or whose worker is gone, is a runner-side failure:
    /// no stop word is true of it, so no row.
    async fn reading(&mut self, dl: LookDeadline) -> Result<NodeReading, Halt> {
        let wait = dl.left();
        match self.d.current_node(wait).await {
            Ok(r) => Ok(r),
            Err(BarrierMiss::TimedOut) if dl.budget_bound && wait < BARRIER_TIMEOUT => {
                Err(self.time_up())
            }
            Err(BarrierMiss::TimedOut) => Err(internal(format!(
                "the journey producer did not answer where the app is within {} s",
                wait.min(BARRIER_TIMEOUT).as_secs()
            ))),
            Err(BarrierMiss::WorkerGone) => Err(internal(
                "the journey producer is not running, so where the app is is unknown",
            )),
        }
    }

    /// How a failed ACTION (an activation, a replay, a navigation) ends the
    /// run. Every `frontier_unsettled` arm sets the [`Unsettled`] flag that
    /// says why — never an unsettled run with nothing named.
    fn drive_failed(&mut self, f: DriveFailure) -> Halt {
        match f {
            DriveFailure::Unreachable(d) => Halt::Stop(StopReason::AppUnreachable, d),
            DriveFailure::AuthWall(d) => Halt::Stop(StopReason::AuthWall, d),
            // [`classify_answer`] reads an app's refused action as a failed
            // activation, so this arm is not reached by the live driver; if a
            // driver does answer it, it is a failed attempt, never
            // `app_unreachable`.
            DriveFailure::AppAnswered(d) => {
                self.unsettled.attempts_failed += 1;
                Halt::Stop(
                    StopReason::FrontierUnsettled,
                    format!("the app answered an action with a failure: {d}"),
                )
            }
            DriveFailure::Runner(d) => internal(d),
            DriveFailure::OutcomeUnknown(d) => {
                self.unsettled.action_outcome_unknown = true;
                Halt::Stop(
                    StopReason::FrontierUnsettled,
                    format!(
                        "the app's answer to an action could not be read, so whether it acted \
                         (and where it went) is unknown: {d}"
                    ),
                )
            }
        }
    }

    /// The app's answer to a snapshot could not be read. As
    /// [`Self::snapshot_refused`]: no row before the run stood on any node,
    /// `frontier_unsettled` after — never `app_unreachable`.
    fn snapshot_unknown(&mut self, why: String) -> Halt {
        self.unsettled.snapshot_outcome_unknown = true;
        if self.visited.is_empty() {
            return Halt::NoRow(
                NoRowKind::StartUnknown,
                format!("the app's answer to the first snapshot could not be read: {why}"),
            );
        }
        Halt::Stop(
            StopReason::FrontierUnsettled,
            format!(
                "the app's answer to a snapshot could not be read, so where it is and what it \
                 offers were not read: {why}"
            ),
        )
    }

    /// The app answered a snapshot with a failure. Before the run stood on
    /// any node nothing was explored: no stop word is true, so no row (the
    /// start was refused). After, the run ends with frontier it could not
    /// read — `frontier_unsettled` — and never `app_unreachable`.
    fn snapshot_refused(&mut self, why: String) -> Halt {
        self.unsettled.snapshot_refused_by_app = true;
        if self.visited.is_empty() {
            return Halt::NoRow(
                NoRowKind::StartRefused,
                format!("the app answered the first snapshot with a failure: {why}"),
            );
        }
        Halt::Stop(
            StopReason::FrontierUnsettled,
            format!(
                "the app answered a snapshot with a failure, so where it is and what it offers \
                 were not read: {why}"
            ),
        )
    }

    /// Snapshot, then learn the node the producer resolved for it. An auth
    /// refusal or a login page stops the run here. Every wait shares the one
    /// deadline `dl`.
    async fn look_within(&mut self, dl: LookDeadline) -> Result<JourneyNode, Halt> {
        if self.snapshots_seen.is_none() {
            // Baseline, so the first look can tell a fresh node from a stale one.
            self.snapshots_seen = Some(self.reading(dl).await?.snapshots);
        }
        let snapshot = match tokio::time::timeout(dl.left(), self.d.snapshot()).await {
            Err(_) => return Err(self.wait_ran_out(dl, "the snapshot")),
            Ok(Err(DriveFailure::AppAnswered(why))) => return Err(self.snapshot_refused(why)),
            Ok(Err(DriveFailure::OutcomeUnknown(why))) => return Err(self.snapshot_unknown(why)),
            Ok(Err(f)) => return Err(self.drive_failed(f)),
            Ok(Ok(s)) => s,
        };
        if answer_is_auth_refusal(&snapshot) {
            return Err(Halt::Stop(
                StopReason::AuthWall,
                "the snapshot answer carried an authentication refusal".into(),
            ));
        }
        let reading = self.reading(dl).await?;
        // A snapshot the producer never received (a dropped event) leaves the
        // count unmoved: the node it reports is STALE, so where the app is is
        // unknown — never taken as a match.
        if Some(reading.snapshots) <= self.snapshots_seen {
            return Err(internal(
                "the journey producer did not receive the snapshot just taken (its node would be \
                 stale), so where the app is is unknown",
            ));
        }
        self.snapshots_seen = Some(reading.snapshots);
        let Some(node) = reading.node else {
            return Err(internal(
                "the journey producer resolved no node for the snapshot just taken",
            ));
        };
        self.affordances = extract_affordances(snapshot_view(&snapshot));
        self.visited.insert(node.key());
        self.node = Some(node.clone());
        self.fresh = true;
        if is_auth_wall(&node) {
            return Err(Halt::Stop(
                StopReason::AuthWall,
                format!("node {} is a login page", node.key()),
            ));
        }
        Ok(node)
    }

    async fn look(&mut self) -> Result<JourneyNode, Halt> {
        self.check_time()?;
        let dl = LookDeadline::new(self.remaining(), true);
        self.look_within(dl).await
    }

    /// Spend one action. Never called past the budget: the selection only
    /// returns a route that fits.
    fn spend(&mut self) {
        debug_assert!(self.spent_actions < self.budget.max_actions);
        self.spent_actions += 1;
        self.fresh = false;
    }

    /// An action is never cut off mid-flight (the time budget may overrun by
    /// the one in-flight action, as the table allows); only waits are bounded.
    async fn act(&mut self, element_id: &str, action: &str) -> Result<ActResult, Halt> {
        self.spend();
        match self.d.activate(element_id, action).await {
            Ok(r) => Ok(r),
            Err(f) => Err(self.drive_failed(f)),
        }
    }

    async fn rows_within(&mut self, deadline: Duration) -> Result<JourneyGraph, Halt> {
        let rows = tokio::time::timeout(deadline, self.d.ledger_rows())
            .await
            .map_err(|_| self.time_up())?
            .map_err(internal)?;
        self.unsettled.ledger_read_truncated |= rows.truncated;
        // A ledger that is not `writing` may lack this run's own edges and
        // frontier rows: what was read is not the frontier the app has.
        self.unsettled.ledger_not_writing |= rows.ledger.state != LedgerState::Writing;
        let graph = routing_graph(&rows.edges, &rows.frontier, &rows.ledger);
        // Unreadable rows: unparsed ones, and rows that failed their contract
        // (an untrusted frontier row may be an open entry anywhere; an
        // untrusted edge, a route the run cannot see). Latest read wins.
        self.unsettled.rows_unreadable = rows.rows_unparseable + graph.coverage.rows_rejected;
        Ok(graph)
    }

    async fn rows(&mut self) -> Result<JourneyGraph, Halt> {
        self.check_time()?;
        let deadline = self.remaining();
        self.rows_within(deadline).await
    }

    /// Replay one D4-safe edge from the current node. `false` = the app did
    /// not follow the observed path (the element is gone, no longer safe, or
    /// it led elsewhere).
    async fn replay(&mut self, edge: &JourneyGraphEdge) -> Result<bool, Halt> {
        let Some(fp) = edge.trigger.target_fingerprint.as_deref() else {
            return Ok(false);
        };
        let Some(id) = element_with_fingerprint(&self.affordances, fp) else {
            return Ok(false);
        };
        let Ok(action) = live_activation(&self.affordances, &id) else {
            return Ok(false);
        };
        self.check_time()?;
        if let ActResult::Failed(_) = self.act(&id, &action).await? {
            return Ok(false);
        }
        let node = self.look().await?;
        Ok(node.key() == edge.to_key)
    }

    async fn go_to_start_url(&mut self, url: &str, first: bool) -> Result<JourneyNode, Halt> {
        self.check_time()?;
        self.spend();
        let answered = match self.d.navigate(url).await {
            Ok(r) => r,
            // Nothing explored yet and the outcome unknown: no stop word is
            // true — no row (502).
            Err(DriveFailure::OutcomeUnknown(why)) if first => {
                self.unsettled.action_outcome_unknown = true;
                return Err(Halt::NoRow(
                    NoRowKind::StartUnknown,
                    format!(
                        "the app's answer to the start-URL navigation could not be read: {why}"
                    ),
                ));
            }
            Err(f) => return Err(self.drive_failed(f)),
        };
        match answered {
            ActResult::Acted => {}
            // The app ANSWERED: it is reachable. At the run's start nothing
            // has been explored, so no stop word is true — no row (422).
            ActResult::Failed(why) if first => {
                return Err(Halt::NoRow(
                    NoRowKind::StartRefused,
                    format!("the app refused navigation to the start URL: {why}"),
                ))
            }
            // Mid-run, a refused return to the start is a failed attempt at
            // the entry it was routing to; the loop carries on.
            ActResult::Failed(_) => return Err(Halt::DetourRefused),
        }
        self.look().await
    }
}

/// Run the exploration.
pub(crate) async fn explore<D: ExploreDriver>(
    d: &mut D,
    request: &ExploreRequest,
) -> Result<LoopOutcome, NoRow> {
    let mut run = Run {
        d,
        budget: request.budget,
        start_key: None,
        spent_actions: 0,
        spent_depth: 0,
        accounting: Accounting::default(),
        attempted: BTreeSet::new(),
        in_flight: None,
        visited: BTreeSet::new(),
        node: None,
        affordances: AffordanceIndex::default(),
        fresh: false,
        snapshots_seen: None,
        unsettled: Unsettled::default(),
        budget_left: BTreeSet::new(),
        write_failures_at_start: 0,
    };
    run.write_failures_at_start = run.d.ledger_write_failures();
    // The explorer's cursor outlives a run: its last node is the previous
    // run's. Forget it first — ONCE, for every start: a URL start acts BEFORE
    // any look, so its start navigation's edge must run from the unknown
    // node, never from where another run left off; a node start's check
    // snapshots the app before anything else, and that snapshot must not
    // close a previous run's pending edge with where the app is now.
    if run.d.forget_position().await.is_err() {
        return Err(NoRow {
            kind: NoRowKind::Internal,
            detail: "the journey producer could not be told the run starts from an unknown \
                     position; nothing was acted on"
                .into(),
            spent_actions: 0,
            edge_events: run.d.edge_events(),
        });
    }
    // A node start must be where the app is, or reachable from it, before
    // any action is taken ([`check_node_start`], which relies on the forget
    // above and does not forget again).
    if let ExploreStart::Node(k) = &request.start {
        if let Err(Refusal(status, detail)) = check_node_start(&mut *run.d, k).await {
            return Err(NoRow {
                kind: NoRowKind::StartCheck(status),
                detail,
                spent_actions: 0,
                edge_events: run.d.edge_events(),
            });
        }
    }
    let mut not_activated = NotActivated::default();
    let start_url = match &request.start {
        ExploreStart::Url(u) => Some(u.clone()),
        ExploreStart::Node(_) => None,
    };
    let halted = explore_inner(&mut run, request, start_url.as_deref(), &mut not_activated).await;
    let (mut stopped_because, mut stop_explanation) = match halted {
        Halt::Stop(reason, detail) => (reason, detail),
        Halt::DetourRefused => {
            return Err(NoRow {
                kind: NoRowKind::Internal,
                detail: "a refused detour escaped the loop".into(),
                spent_actions: run.spent_actions,
                edge_events: run.d.edge_events(),
            })
        }
        Halt::NoRow(kind, detail) => {
            return Err(NoRow {
                kind,
                detail,
                spent_actions: run.spent_actions,
                edge_events: run.d.edge_events(),
            })
        }
    };
    // Close the last action's pending edge and refresh the reasons of what
    // was seen — when the app can still be reached — all within
    // POST_STOP_GRACE. This look also CONFIRMS an exhausted frontier: it is
    // claimed only when the app answered, fresh, after the selection found
    // nothing, AND the selection re-run on the ledger as that look left it
    // still finds nothing activatable. An app that stopped answering is
    // `app_unreachable`; one now behind a login page is `auth_wall`; a
    // runner-side failure to tie the snapshot to a node, an app that answered
    // it with a failure, or a ledger that could not be re-read leaves the
    // exhaustion unconfirmed (settled below). A budget stop stays a budget
    // stop either way — but a closing look that did not complete is recorded
    // (`closing_look_failed`), never dropped: the unknown is kept in the
    // answer and, through `stop_detail`, in the row.
    if !matches!(
        stopped_because,
        StopReason::AppUnreachable | StopReason::AuthWall
    ) {
        let exhausted = stopped_because == StopReason::FrontierExhausted;
        let confirmed = tokio::time::timeout(POST_STOP_GRACE, async {
            let looked = run
                .look_within(LookDeadline::new(POST_STOP_GRACE, false))
                .await;
            let graph = match &looked {
                Ok(_) => run.rows_within(POST_STOP_GRACE).await.ok(),
                Err(_) => None,
            };
            if let Some(g) = &graph {
                run.accounting.see(g, &run.visited);
            }
            (looked, graph)
        })
        .await;
        let closing_failure = match &confirmed {
            Ok((Ok(_), Some(_))) => None,
            Ok((Ok(_), None)) => Some("the ledger could not be re-read after it".to_string()),
            Ok((Err(Halt::Stop(_, d) | Halt::NoRow(_, d)), _)) => Some(d.clone()),
            Ok((Err(Halt::DetourRefused), _)) => Some("a refused detour".to_string()),
            Err(_) => Some(format!(
                "it did not finish within the post-stop grace ({} s)",
                POST_STOP_GRACE.as_secs()
            )),
        };
        run.unsettled.closing_look_failed = Some(closing_failure.is_some());
        if let (Some(why), true) = (&closing_failure, stopped_because.is_budget()) {
            stop_explanation = format!(
                "{stop_explanation}; the closing look after the stop did not complete, so where \
                 the app ended and the last action's edge are unknown: {why}"
            );
        }
        match confirmed {
            Ok((Ok(here), Some(graph))) if exhausted => match run.start_key.clone() {
                Some(start_key) => {
                    let again = select_next(
                        &graph,
                        &start_key,
                        start_url.is_some(),
                        &here.key(),
                        run.budget.max_depth,
                        run.budget.max_actions - run.spent_actions,
                        &run.attempted,
                        &run.visited,
                    );
                    run.unsettled.unroutable = again.skipped.unroutable;
                    run.unsettled.component_actions = again.skipped.component_actions;
                    run.unsettled.activation_failed_open = again.failed_open;
                    run.unsettled.open_at_confirmation = Some(
                        again.budget_left.len() as u32
                            + again.skipped.unroutable
                            + activated_still_open(&run.accounting, &graph),
                    );
                    run.unsettled.exhaustion_unconfirmed = Some(false);
                }
                None => run.unsettled.exhaustion_unconfirmed = Some(true),
            },
            Ok((
                Err(Halt::Stop(reason @ (StopReason::AppUnreachable | StopReason::AuthWall), d)),
                _,
            )) if exhausted => {
                stopped_because = reason;
                stop_explanation = format!(
                    "the frontier looked exhausted, but the confirming snapshot said otherwise: {d}"
                );
            }
            _ if exhausted => run.unsettled.exhaustion_unconfirmed = Some(true),
            _ => {}
        }
    }
    // A journey write that failed while the run was going may be one of its
    // own edges or frontier rows: the ledger it read is not the app's
    // frontier. Read after the confirming look, whose barrier waited for the
    // producer to process everything the run did.
    run.unsettled.ledger_write_failures = run
        .d
        .ledger_write_failures()
        .saturating_sub(run.write_failures_at_start);
    // An action event the producer never received is an edge (and a frontier
    // move) the ledger will never show: the frontier the run read was not
    // the frontier the app has.
    let edge_events = run.d.edge_events();
    run.unsettled.edge_events_dropped = edge_events.dropped;
    if let (reason, Some(why)) = settle_stop(stopped_because, &run.unsettled) {
        stopped_because = reason;
        stop_explanation = why;
    }
    let stop_node = match (stopped_because, run.node.clone()) {
        // The CHECK requires the node an auth wall was met at; when the
        // refusal came before any node was resolved, the node is unknown —
        // said as the unknown node, never invented.
        (StopReason::AuthWall, None) => Some(unknown_node()),
        (_, n) => n,
    };
    Ok(LoopOutcome {
        stopped_because,
        stop_explanation,
        stop_node,
        spent_actions: run.spent_actions,
        spent_depth: run.spent_depth,
        accounting: run.accounting,
        not_activated,
        unsettled: run.unsettled,
        budget_left: run.budget_left,
        attempts_finished: run
            .attempted
            .iter()
            .filter(|k| run.in_flight.as_ref() != Some(*k))
            .cloned()
            .collect(),
        edge_events,
    })
}

async fn explore_inner<D: ExploreDriver>(
    run: &mut Run<'_, D>,
    request: &ExploreRequest,
    start_url: Option<&str>,
    not_activated: &mut NotActivated,
) -> Halt {
    let start_key = match &request.start {
        ExploreStart::Url(u) => match run.go_to_start_url(u, true).await {
            Ok(n) => n.key(),
            Err(h) => return h,
        },
        ExploreStart::Node(k) => k.clone(),
    };
    run.start_key = Some(start_key.clone());
    loop {
        // Whatever brought the loop back here — a `continue` out of an
        // attempt or a finished action — no attempt is in flight any more.
        run.in_flight = None;
        // An iteration after an action contacts the app first: the snapshot
        // is the liveness proof, taken BEFORE the frontier is consulted (and
        // the final look in `explore` confirms an exhausted frontier), so a
        // dead app can never read as an exhausted one.
        let here = if run.fresh {
            match run.node.clone() {
                Some(n) => n,
                None => return internal("a fresh look with no node"),
            }
        } else {
            match run.look().await {
                Ok(n) => n,
                Err(h) => return h,
            }
        };
        let graph = match run.rows().await {
            Ok(g) => g,
            Err(h) => return h,
        };
        run.accounting.see(&graph, &run.visited);
        let selection = select_next(
            &graph,
            &start_key,
            start_url.is_some(),
            &here.key(),
            run.budget.max_depth,
            run.budget.max_actions - run.spent_actions,
            &run.attempted,
            &run.visited,
        );
        run.budget_left = selection.budget_left;
        *not_activated = selection.skipped;
        run.unsettled.unroutable = selection.skipped.unroutable;
        run.unsettled.component_actions = selection.skipped.component_actions;
        run.unsettled.activation_failed_open = selection.failed_open;
        if let Err(h) = run.check_time() {
            return h;
        }
        let (entry, depth, route) = match selection.choice {
            Choice::Next {
                entry,
                depth,
                route,
            } => (entry, depth, route),
            Choice::Stop(reason) => {
                let skipped = selection.skipped;
                let detail = match reason {
                    StopReason::BudgetActions => format!(
                        "{} of {} actions spent; no activatable entry's route fits the rest",
                        run.spent_actions, run.budget.max_actions
                    ),
                    StopReason::BudgetDepth => format!(
                        "{} activatable entr(ies) lie deeper than max_depth {}",
                        skipped.beyond_depth, run.budget.max_depth
                    ),
                    _ => format!(
                        "no activatable frontier entry is left in the start's component \
                         ({} component action(s) not driven, {} unroutable)",
                        skipped.component_actions, skipped.unroutable
                    ),
                };
                return Halt::Stop(reason, detail);
            }
        };
        let key = entry_key(&entry);
        run.attempted.insert(key.clone());
        run.in_flight = Some(key.clone());

        let mut arrived = true;
        if route.via_start_url {
            if let Some(url) = start_url {
                match run.go_to_start_url(url, false).await {
                    Ok(_) => {}
                    Err(Halt::DetourRefused) => arrived = false,
                    Err(h) => return h,
                }
            }
        }
        if arrived {
            for edge in &route.path {
                match run.replay(edge).await {
                    Ok(true) => {}
                    Ok(false) => {
                        arrived = false;
                        break;
                    }
                    Err(h) => return h,
                }
            }
        }
        if !arrived || run.node.as_ref().map(|n| n.key()) != Some(entry.node_key.clone()) {
            // The observed path did not hold; the entry stays where it is,
            // is not retried this run, and unsettles any exhaustion claim.
            run.unsettled.attempts_failed += 1;
            continue;
        }

        let Some(element_id) =
            element_with_fingerprint(&run.affordances, &entry.affordance_fingerprint)
        else {
            // Not on the live page: still on the frontier, under the reason
            // its row already had (unactivated, or a failed retry).
            run.accounting.remaining(key, entry.reason);
            run.unsettled.attempts_failed += 1;
            continue;
        };
        let action = match live_activation(&run.affordances, &element_id) {
            Ok(a) => a,
            Err(_)
                if ledger_reason(&run.affordances, &element_id)
                    == Some(FrontierReason::NotYetActivated) =>
            {
                // D4 allows it — the ledger row stays `not_yet_activated` —
                // but the live element offers nothing safe to fire (a
                // navigation element with no `click`, a read-declared one with
                // no read action). Activatable frontier this run cannot do: it
                // keeps the ledger's reason and unsettles any exhaustion claim.
                run.accounting.remaining(key, entry.reason);
                run.unsettled.undrivable += 1;
                continue;
            }
            Err(reason) => {
                // The live element fails D4 (the row was stale): it is NOT
                // activated, and the snapshot just taken has already moved
                // the row to this reason. Not a failure — a classification.
                // An `activation_failed` row is the exception: an observation
                // never moves it, so it keeps that reason (and stays counted
                // in `activation_failed_open`).
                let kept = match entry.reason {
                    FrontierReason::ActivationFailed => entry.reason,
                    _ => reason,
                };
                run.accounting.remaining(key, kept);
                continue;
            }
        };
        if let Err(h) = run.check_time() {
            return h;
        }
        match run.act(&element_id, &action).await {
            Ok(ActResult::Acted) => {
                run.accounting.activated(key);
                run.spent_depth = run.spent_depth.max(depth);
            }
            Ok(ActResult::Failed(_)) => {
                // The producer writes an `error` edge and keeps the row as
                // `activation_failed`.
                run.accounting
                    .remaining(key, FrontierReason::ActivationFailed);
                run.unsettled.attempts_failed += 1;
                run.spent_depth = run.spent_depth.max(depth);
            }
            Err(h) => return h,
        }
        // The action finished (whatever its outcome): nothing is in flight.
        run.in_flight = None;
    }
}

// ---------------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------------

/// What the run spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExploreSpent {
    pub actions: u32,
    pub depth: u32,
    pub wall_clock_ms: u64,
}

/// Whether the run's row reached `project.journey_explorations`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceState {
    Written,
    /// The row was written WITHOUT its `stop_detail`: the run is not
    /// `frontier_unsettled` and its [`Unsettled`] block records something,
    /// but this database still has qontinui-web `journey_03`'s CHECK, which
    /// allows `stop_detail` only on a `frontier_unsettled` row
    /// (`journey_04_explorations_stop_detail_any_stop` is not applied). The
    /// insert never fails on it; the block is in this answer only, and the
    /// `detail` says so.
    WrittenWithoutStopDetail,
    /// The table does not exist in this database (an embedded database
    /// predating it never receives it — plan Phase 0 decision 4).
    SchemaAbsent,
    /// The probe, the validation or the INSERT failed.
    WriteFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Persistence {
    pub state: PersistenceState,
    pub detail: String,
}

/// The answer of `POST /journey/explore`, and the row persisted from it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationResult {
    pub run_id: String,
    pub app_id: String,
    pub budget: ExploreBudget,
    pub spent: ExploreSpent,
    pub stopped_because: StopReason,
    /// Why the run stopped, in prose, for a reader. Not stored: the stored,
    /// machine-readable breakdown is [`Self::unsettled`], persisted as the
    /// `stop_detail` column ([`stop_detail_column`]).
    pub stop_explanation: String,
    pub stop_node: Option<JourneyNode>,
    /// The run's ACTION EVENTS the journey producer's queue accepted —
    /// activations, route replays and start-URL navigations alike. Each
    /// closes into exactly one `journey_edge_observations` row with this
    /// `runId` (outcome `to_node_unobserved` included), so this equals that
    /// row count UNLESS a row's write then failed (counted on
    /// `/journey/health`) or the row has since aged out of the 90-day
    /// window. It is not "new graph edges": a replay re-observes an existing
    /// edge. Persisted as `edges_added`.
    pub edges_added: u32,
    /// Distinct frontier entries the run saw at the nodes it stood on.
    pub affordances_seen: u32,
    /// Of those, the ones it activated and the app acted on.
    /// `affordancesActivated + Σ frontierRemainingByReason = affordancesSeen`.
    pub affordances_activated: u32,
    /// Every entry the run saw and did not activate, by reason (all six
    /// reasons, zeros included). `budget_exhausted` = an activatable entry
    /// only a budget kept from this run.
    pub frontier_remaining_by_reason: BTreeMap<String, u32>,
    /// Entries the selection passed over, by why.
    pub not_activated: NotActivated,
    /// What kept (or would have kept) the run from claiming an exhausted
    /// frontier. All zero / false under `frontier_exhausted`; under
    /// `frontier_unsettled` it says why. Under any other stop it is what was
    /// measured on the way — unknowns the run carried, not why it stopped.
    /// Persisted as `stop_detail` whenever it records anything
    /// ([`stop_detail_column`]).
    ///
    /// The run's dropped action events are reported HERE, as
    /// `unsettled.edgeEventsDropped`, and nowhere else in this answer: this
    /// block is what the row keeps, so the count survives the answer, and
    /// one field cannot disagree with a copy of itself. (The no-row answer,
    /// which has no `unsettled` block, carries `edgeEventsDropped` at its
    /// top level beside `edgesAdded`.)
    pub unsettled: Unsettled,
    pub start: Value,
    pub started_at: String,
    pub finished_at: String,
    pub runner_build_id: String,
    pub runner_instance: String,
    pub persistence: Persistence,
}

/// The CHECKs of `project.journey_explorations` (qontinui-web migrations
/// `journey_02_explorations`, `journey_03_explorations_stop_unsettled` and
/// `journey_04_explorations_stop_detail_any_stop`) that a value can violate,
/// checked HERE so a row
/// the database would refuse is refused with the constraint named before it
/// is sent. Three are not re-checked because the types cannot violate them:
/// `stopped_because` is a [`StopReason`] (its seven words, pinned against the
/// vendored schema by a test), `stop_node` is a [`JourneyNode`] (always a JSON
/// object, validated here), and `frontier_remaining_by_reason` is a map (always
/// an object).
pub(crate) fn check_record(r: &ExplorationResult) -> Result<(), String> {
    let b = &r.budget;
    let s = &r.spent;
    let fits_int = |n: u64| n <= i32::MAX as u64;
    let positive = b.max_actions > 0 && b.max_wall_clock_s > 0;
    let in_range = [b.max_actions, b.max_depth, b.max_wall_clock_s]
        .iter()
        .all(|n| fits_int(u64::from(*n)));
    if !(positive && in_range) {
        return Err("ck_journey_explorations_budget_positive".into());
    }
    if !(fits_int(u64::from(s.actions))
        && fits_int(u64::from(s.depth))
        && s.wall_clock_ms <= i64::MAX as u64)
    {
        return Err("ck_journey_explorations_spent_non_negative".into());
    }
    if s.actions > b.max_actions || s.depth > b.max_depth {
        return Err("ck_journey_explorations_spent_within_budget".into());
    }
    if r.edges_added > s.actions {
        return Err("ck_journey_explorations_edges_added".into());
    }
    let started = chrono::DateTime::parse_from_rfc3339(&r.started_at)
        .map_err(|e| format!("started_at is not RFC 3339: {e}"))?;
    let finished = chrono::DateTime::parse_from_rfc3339(&r.finished_at)
        .map_err(|e| format!("finished_at is not RFC 3339: {e}"))?;
    if finished < started {
        return Err("ck_journey_explorations_finished_after_started".into());
    }
    if r.stopped_because == StopReason::AuthWall && r.stop_node.is_none() {
        return Err("ck_journey_explorations_auth_wall_names_node".into());
    }
    if !r.start.is_object() {
        return Err("ck_journey_explorations_start_object".into());
    }
    if let Some(n) = &r.stop_node {
        n.validate()
            .map_err(|e| format!("stop_node contract validation: {e}"))?;
    }
    // journey_04's CHECK: any stop may carry it, as a JSON object. (On a
    // database still at journey_03, `persist` withholds it from a row that is
    // not frontier_unsettled instead — see [`stop_detail_bind`].)
    let detail = stop_detail_column(r);
    let unsettled = r.stopped_because == StopReason::FrontierUnsettled;
    if detail.as_ref().is_some_and(|d| !d.is_object()) {
        return Err("ck_journey_explorations_stop_detail".into());
    }
    if unsettled && !r.unsettled.any() {
        return Err(
            "frontier_unsettled names no reason: its unsettled block has no flag set".into(),
        );
    }
    if unsettled && detail.is_none() {
        return Err(
            "stop_detail: a frontier_unsettled row must carry its unsettled breakdown".into(),
        );
    }
    Ok(())
}

/// The `stop_detail` column (qontinui-web `journey_03_explorations_stop_unsettled`,
/// widened to every stop by `journey_04_explorations_stop_detail_any_stop`):
/// the [`Unsettled`] block — the answer's `unsettled`, camelCase — exactly
/// when it RECORDS something, i.e. [`Unsettled::any`]: a count above zero, a
/// flag set, or a confirmation measure that failed (`exhaustionUnconfirmed`
/// / `closingLookFailed` `true`, `openAtConfirmation` above zero). A
/// confirmation that passed (`false` / `0`) or was not taken (`null`) records
/// nothing on its own.
///
/// - `frontier_unsettled`: always non-NULL ([`check_record`] refuses one whose
///   block records nothing) — it says WHY the frontier was not exhausted.
/// - any other stop: non-NULL when the run carried an unknown or an unsettling
///   count on the way (dropped action events, failed journey writes, a closing
///   look that did not complete, …) — so the stored row keeps it; the stop
///   word stays the reason. NULL when the block records nothing.
pub(crate) fn stop_detail_column(r: &ExplorationResult) -> Option<Value> {
    if !r.unsettled.any() {
        return None;
    }
    serde_json::to_value(r.unsettled).ok()
}

/// What the database's CHECK on `stop_detail` allows, as the probe read it
/// ([`EXPLORATION_SCHEMA_PROBE_SQL`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopDetailScope {
    /// qontinui-web `journey_04`: on any stop, as a JSON object.
    AnyStop,
    /// qontinui-web `journey_03` (or `journey_04`'s downgrade): only on a
    /// `frontier_unsettled` row. Also what an unreadable probe column reads
    /// as — fail-closed, so the insert never fails on it.
    UnsettledOnly,
}

/// The `stop_detail` value to bind for `r` under `scope`, and whether a
/// block that [`stop_detail_column`] would store was WITHHELD (a database at
/// `journey_03` and a row that is not `frontier_unsettled`).
pub(crate) fn stop_detail_bind(
    r: &ExplorationResult,
    scope: StopDetailScope,
) -> (Option<Value>, bool) {
    let detail = stop_detail_column(r);
    let allowed =
        scope == StopDetailScope::AnyStop || r.stopped_because == StopReason::FrontierUnsettled;
    match detail {
        Some(d) if allowed => (Some(d), false),
        Some(_) => (None, true),
        None => (None, false),
    }
}

/// Does a CHECK definition (as `pg_get_constraintdef` renders it) tie
/// `stop_detail` to `frontier_unsettled`? The predicate
/// [`EXPLORATION_SCHEMA_PROBE_SQL`]'s second column evaluates in SQL, kept
/// here so a test holds it against both migrations' renderings.
///
/// Fail-safe, with a known false positive: ANY CHECK on the table whose
/// definition names both words reads as restricting — journey_03's, but also
/// a future one that, say, REQUIRES the detail on an unsettled row without
/// restricting other stops. While such a CHECK stands, every non-unsettled
/// row's detail is withheld (`written_without_stop_detail`, said in the
/// answer), never sent where it might be refused; a migration that adds one
/// must revisit this predicate, or the withholding never ends.
///
/// Run against a real database: the probe SQL was executed on an ephemeral
/// PostgreSQL 16.15 (`ephemeral-db.sh start pgvector`, 2026-10-29) with the
/// web alembic chain at `journey_02` (`false, false`), `journey_03`
/// (`true, true`), `journey_04` (`true, false`) and downgraded back to
/// `journey_03` (`true, true`). No runner test drives a database.
pub(crate) fn check_restricts_stop_detail(definition: &str) -> bool {
    definition.contains("stop_detail") && definition.contains("frontier_unsettled")
}

/// `INSERT` of one run. `$5` / `$6` cast from the RFC 3339 strings the answer
/// carries, so the row holds exactly the times the caller was told;
/// `finished_at` is always sent (the `now()` default is a fallback only).
/// `$15` (`stop_node`) and `$18` (`stop_detail`) bind `Option<Value>`: `None`
/// is SQL NULL, never JSON `null`.
pub(crate) const EXPLORATION_INSERT_SQL: &str = r#"INSERT INTO project.journey_explorations
    (run_id, app_id, runner_build_id, runner_instance, started_at, finished_at, start,
     budget_max_actions, budget_max_depth, budget_max_wall_clock_s,
     spent_actions, spent_depth, spent_wall_clock_ms, stopped_because, stop_node,
     edges_added, frontier_remaining_by_reason, stop_detail)
VALUES ($1, $2, $3, $4, $5::text::timestamptz, $6::text::timestamptz, $7::jsonb,
        $8, $9, $10, $11, $12, $13, $14, $15::jsonb, $16, $17::jsonb, $18::jsonb)"#;

/// Parameters [`EXPLORATION_INSERT_SQL`] binds.
pub(crate) const EXPLORATION_INSERT_BINDS: usize = 18;

/// One bound value of [`EXPLORATION_INSERT_SQL`].
pub(crate) type InsertParam = Box<dyn tokio_postgres::types::ToSql + Sync + Send>;

/// The INSERT's binds as `(column, value)`, in the SQL's column order — one
/// list, so a test can hold each column to its own value. `stop_detail` is
/// bound under `scope` ([`stop_detail_bind`]).
pub(crate) fn exploration_insert_params(
    r: &ExplorationResult,
    scope: StopDetailScope,
) -> Vec<(&'static str, InsertParam)> {
    let to_i32 = |n: u32| n as i32;
    let stop_node = r
        .stop_node
        .as_ref()
        .map(|n| serde_json::to_value(n).unwrap_or(Value::Null))
        .filter(|v| v.is_object());
    let remaining = serde_json::to_value(&r.frontier_remaining_by_reason).unwrap_or(json!({}));
    vec![
        ("run_id", Box::new(r.run_id.clone())),
        ("app_id", Box::new(r.app_id.clone())),
        ("runner_build_id", Box::new(r.runner_build_id.clone())),
        ("runner_instance", Box::new(r.runner_instance.clone())),
        ("started_at", Box::new(r.started_at.clone())),
        ("finished_at", Box::new(r.finished_at.clone())),
        ("start", Box::new(r.start.clone())),
        ("budget_max_actions", Box::new(to_i32(r.budget.max_actions))),
        ("budget_max_depth", Box::new(to_i32(r.budget.max_depth))),
        (
            "budget_max_wall_clock_s",
            Box::new(to_i32(r.budget.max_wall_clock_s)),
        ),
        ("spent_actions", Box::new(to_i32(r.spent.actions))),
        ("spent_depth", Box::new(to_i32(r.spent.depth))),
        (
            "spent_wall_clock_ms",
            Box::new(r.spent.wall_clock_ms as i64),
        ),
        ("stopped_because", Box::new(r.stopped_because.as_str())),
        ("stop_node", Box::new(stop_node)),
        ("edges_added", Box::new(to_i32(r.edges_added))),
        ("frontier_remaining_by_reason", Box::new(remaining)),
        ("stop_detail", Box::new(stop_detail_bind(r, scope).0)),
    ]
}

/// Two columns.
///
/// 1. Does the explorations table exist? Schema-qualified, like the ledger
///    probe. It also requires the `stop_detail` column (web
///    `journey_03_explorations_stop_unsettled`): a database at `journey_02`
///    has the table but would refuse every INSERT, so it reads
///    `schema_absent`.
/// 2. Does a CHECK on it still tie `stop_detail` to `frontier_unsettled`
///    (web `journey_03`'s, before `journey_04_explorations_stop_detail_any_stop`
///    relaxed it)? The SQL form of [`check_restricts_stop_detail`]. `true` →
///    [`StopDetailScope::UnsettledOnly`].
pub(crate) const EXPLORATION_SCHEMA_PROBE_SQL: &str =
    "SELECT to_regclass('project.journey_explorations') IS NOT NULL \
     AND EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = 'project' \
     AND table_name = 'journey_explorations' AND column_name = 'stop_detail'), \
     EXISTS (SELECT 1 FROM pg_constraint c \
     WHERE c.conrelid = to_regclass('project.journey_explorations') AND c.contype = 'c' \
     AND pg_get_constraintdef(c.oid) LIKE '%stop_detail%' \
     AND pg_get_constraintdef(c.oid) LIKE '%frontier_unsettled%')";

async fn persist(pg: &crate::database::pg::PgDb, r: &ExplorationResult) -> Persistence {
    if let Err(e) = check_record(r) {
        return Persistence {
            state: PersistenceState::WriteFailed,
            detail: format!("the row violates {e}; not sent"),
        };
    }
    let failed = |detail: String| Persistence {
        state: PersistenceState::WriteFailed,
        detail,
    };
    let conn = match pg.pool().get().await {
        Ok(c) => c,
        Err(e) => return failed(format!("PG pool error: {e}")),
    };
    let scope = match conn.query_one(EXPLORATION_SCHEMA_PROBE_SQL, &[]).await {
        Ok(row) if row.try_get::<_, bool>(0).unwrap_or(false) => {
            // Unreadable → restricting: withholding a value never fails the
            // insert, sending one the CHECK refuses would.
            if row.try_get::<_, bool>(1).unwrap_or(true) {
                StopDetailScope::UnsettledOnly
            } else {
                StopDetailScope::AnyStop
            }
        }
        Ok(_) => {
            return Persistence {
                state: PersistenceState::SchemaAbsent,
                detail: "project.journey_explorations, or its stop_detail column, is missing — \
                         apply qontinui-web migrations journey_02_explorations and \
                         journey_03_explorations_stop_unsettled (an existing embedded DB never \
                         receives new tables; see plan Phase 0 decision 4). The run's EDGES were \
                         written."
                    .into(),
            }
        }
        Err(e) => {
            return failed(crate::database::pg::pg_err(
                "journey explorations probe",
                &e,
            ))
        }
    };
    let withheld = stop_detail_bind(r, scope).1;
    let owned = exploration_insert_params(r, scope);
    if owned.len() != EXPLORATION_INSERT_BINDS {
        return failed(format!(
            "{} binds for {EXPLORATION_INSERT_BINDS} INSERT parameters; not sent",
            owned.len()
        ));
    }
    let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = owned
        .iter()
        .map(|(_, v)| &**v as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    match conn.execute(EXPLORATION_INSERT_SQL, &params).await {
        Ok(_) if withheld => Persistence {
            state: PersistenceState::WrittenWithoutStopDetail,
            detail: format!(
                "one row in project.journey_explorations, WITHOUT its stop_detail: this run \
                 stopped {} and its unsettled block records unknowns, but this database's CHECK \
                 still allows stop_detail only on a frontier_unsettled row — apply qontinui-web \
                 migration journey_04_explorations_stop_detail_any_stop. The block is in this \
                 answer only.",
                r.stopped_because.as_str()
            ),
        },
        Ok(_) => Persistence {
            state: PersistenceState::Written,
            detail: "one row in project.journey_explorations".into(),
        },
        Err(e) => failed(crate::database::pg::pg_err(
            "journey explorations insert",
            &e,
        )),
    }
}

fn clamp_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Assemble the answer. `finished_at` is `started_at` plus the MONOTONIC
/// elapsed time — the same clock, so it is never before `started_at` even if
/// the wall clock steps back mid-run.
pub(crate) fn assemble(
    run_id: String,
    app_id: String,
    request: &ExploreRequest,
    started_at: chrono::DateTime<chrono::Utc>,
    elapsed: Duration,
    outcome: LoopOutcome,
) -> ExplorationResult {
    let fmt =
        |t: chrono::DateTime<chrono::Utc>| t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let finished_at = started_at + chrono::Duration::from_std(elapsed).unwrap_or_default();
    ExplorationResult {
        run_id,
        app_id,
        budget: request.budget,
        spent: ExploreSpent {
            actions: outcome.spent_actions,
            depth: outcome.spent_depth,
            wall_clock_ms: elapsed.as_millis() as u64,
        },
        stopped_because: outcome.stopped_because,
        stop_explanation: outcome.stop_explanation,
        stop_node: outcome.stop_node,
        edges_added: clamp_u32(outcome.edge_events.enqueued),
        affordances_seen: outcome.accounting.seen.len() as u32,
        affordances_activated: outcome.accounting.activated_count(),
        frontier_remaining_by_reason: outcome.accounting.remaining_by_reason(
            outcome.stopped_because,
            &outcome.budget_left,
            &outcome.attempts_finished,
        ),
        not_activated: outcome.not_activated,
        unsettled: outcome.unsettled,
        start: request.start.to_json(),
        started_at: fmt(started_at),
        finished_at: fmt(finished_at),
        runner_build_id: RUNNER_BUILD_ID.to_string(),
        runner_instance: capture::runner_instance(),
        persistence: Persistence {
            state: PersistenceState::WriteFailed,
            detail: "unknown: not yet persisted".into(),
        },
    }
}

// ---------------------------------------------------------------------------
// The live driver
// ---------------------------------------------------------------------------

/// An answer the RUNNER produced by rejecting the request before touching any
/// UI (`failure_origin: "runner"`, [`capture::RUNNER_REJECTION_ORIGIN`]).
/// Checked FIRST by both [`classify_answer`] and [`classify_snapshot`],
/// whatever the HTTP status: `page/navigate`'s rejection, for one, is sent
/// with a 200 and `success: false`. Nothing about the app was learned, so it
/// is [`DriveFailure::Runner`] — a runner-side failure with no row (503) —
/// never `app_unreachable`, never an app's answer.
fn runner_rejection(body: &Value, what: &str) -> Option<DriveFailure> {
    (body.get("failure_origin").and_then(|v| v.as_str()) == Some(capture::RUNNER_REJECTION_ORIGIN))
        .then(|| {
            DriveFailure::Runner(format!(
                "the runner rejected the {what} before touching any UI: {}",
                body.get("error").map(|e| e.to_string()).unwrap_or_default()
            ))
        })
}

/// What an SDK action route's answer means for the run, in order: a RUNNER
/// rejection ([`runner_rejection`]) is [`DriveFailure::Runner`], whatever
/// else the answer says; an auth refusal is an auth wall;
/// `failure_origin: "unknown"` (the request may have
/// reached the app but its answer could not be read) is
/// [`DriveFailure::OutcomeUnknown`] — this function maps only that LABEL: an
/// answer with no boolean `success` reads as unknown because the named-app
/// routes add the label to it, and one with neither `success` nor a label
/// falls through to acted; a refusal that reached no UI (a transport refusal
/// — a named app that could not be reached, or a refused webview fallback —
/// or a tab refusal) is `app_unreachable`; an app-answered `success: false`
/// is a failed activation; anything else acted.
pub(crate) fn classify_answer(answer: Value) -> Result<ActResult, DriveFailure> {
    if let Some(f) = runner_rejection(&answer, "action") {
        return Err(f);
    }
    if answer_is_auth_refusal(&answer) {
        return Err(DriveFailure::AuthWall(
            "the action answer carried an authentication refusal".into(),
        ));
    }
    let origin = answer.get("failure_origin").and_then(|v| v.as_str());
    if origin == Some("unknown") {
        return Err(DriveFailure::OutcomeUnknown(format!(
            "the action's answer could not be read: {}",
            answer
                .get("error")
                .map(|e| e.to_string())
                .unwrap_or_default()
        )));
    }
    if capture::sdk_refusal(&answer) {
        return Err(DriveFailure::Unreachable(format!(
            "the action reached no UI: {}",
            answer
                .get("error")
                .map(|e| e.to_string())
                .unwrap_or_default()
        )));
    }
    if answer.get("success") == Some(&Value::Bool(false)) {
        return Ok(ActResult::Failed(
            answer
                .get("error")
                .map(|e| e.to_string())
                .unwrap_or_default(),
        ));
    }
    Ok(ActResult::Acted)
}

/// What the snapshot route's answer means for the run, in order.
///
/// - an answer tagged `failure_origin: "runner"` (the runner rejected the
///   request before touching any UI), WHATEVER its status — a 2xx
///   `success: false` included → a runner-side failure
///   ([`DriveFailure::Runner`]: no row), never `app_unreachable` and never the
///   app's answer ([`runner_rejection`], as [`classify_answer`] reads it);
/// - an authentication refusal → an auth wall;
/// - a 2xx answer that is not `success: false` → the snapshot;
/// - a 2xx `success: false`, or a failure whose `failure_origin` is `"app"`
///   → the APP answered, with a failure: reachable, so never
///   `app_unreachable` ([`DriveFailure::AppAnswered`]);
/// - a failure whose `failure_origin` is `"unknown"`, or that names no
///   origin → whether it reached the app is not known
///   ([`DriveFailure::OutcomeUnknown`]), never `app_unreachable`;
/// - only a `failure_origin: "transport"` failure (never delivered) → no UI
///   was reached → `app_unreachable`.
pub(crate) fn classify_snapshot(status: StatusCode, body: Value) -> Result<Value, DriveFailure> {
    if let Some(f) = runner_rejection(&body, "snapshot") {
        return Err(f);
    }
    if answer_is_auth_refusal(&body) {
        return Err(DriveFailure::AuthWall(
            "the snapshot answer carried an authentication refusal".into(),
        ));
    }
    let refused = body.get("success") == Some(&Value::Bool(false));
    if status.is_success() && !refused {
        return Ok(body);
    }
    let why = format!(
        "snapshot failed ({status}): {}",
        body.get("error").map(|e| e.to_string()).unwrap_or_default()
    );
    let origin = body.get("failure_origin").and_then(|v| v.as_str());
    let app_answered = (status.is_success() && refused) || origin == Some("app");
    if app_answered {
        Err(DriveFailure::AppAnswered(why))
    } else if origin == Some("transport") {
        Err(DriveFailure::Unreachable(why))
    } else {
        // `unknown`, or a failure that names no origin (or one this reader
        // does not know): whether it reached the app is not said, so it is
        // never read as unreachable.
        Err(DriveFailure::OutcomeUnknown(why))
    }
}

/// Drives the app through the runner's own SDK route handlers — the ones an
/// agent's `POST /ui-bridge/sdk/...` reaches — inside
/// [`capture::as_explorer`], so every action they record is the explorer's.
///
/// Every call NAMES the app (`?app_id=`): the SDK routes then dispatch to that
/// registered app by id and never fall back to the runner's own webview, so a
/// connection that drops or switches mid-run can never redirect a click onto
/// another UI (an unreachable named app is a transport refusal →
/// `app_unreachable`). The routes ALSO refuse their webview fallback inside
/// any [`capture::as_explorer`] scope, as a second line.
///
/// Snapshots run inside the scope too, so the explorer's actions and
/// snapshots land on its OWN cursor ([`CursorKey::for_explorer`]): a human's
/// relayed click or an agent's unscoped snapshot of the same app cannot close
/// or displace the explorer's pending edge.
pub(crate) struct LiveDriver {
    state: Arc<ApiState>,
    app_id: String,
    run: ExplorerRun,
    started: Instant,
}

impl LiveDriver {
    fn named(&self) -> HashMap<String, String> {
        HashMap::from([("app_id".to_string(), self.app_id.clone())])
    }
}

#[async_trait::async_trait]
impl ExploreDriver for LiveDriver {
    async fn snapshot(&mut self) -> Result<Value, DriveFailure> {
        let query = self.named();
        let resp = capture::as_explorer(
            self.run.clone(),
            crate::mcp::sdk_client::handle_snapshot(State(Arc::clone(&self.state)), Query(query)),
        )
        .await
        .into_response();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .map_err(|e| {
                DriveFailure::Runner(format!("the snapshot route's body was unreadable: {e}"))
            })?;
        let body: Value = serde_json::from_slice(&bytes).map_err(|e| {
            DriveFailure::Runner(format!("the snapshot route's body is not JSON: {e}"))
        })?;
        classify_snapshot(status, body)
    }

    async fn activate(
        &mut self,
        element_id: &str,
        action: &str,
    ) -> Result<ActResult, DriveFailure> {
        let answer = capture::as_explorer(
            self.run.clone(),
            crate::mcp::sdk_client::handle_element_action(
                State(Arc::clone(&self.state)),
                Path(element_id.to_string()),
                Query(crate::mcp::sdk_client::SdkActionQueryParams {
                    task_run_id: None,
                    tab_id: None,
                    app_id: Some(self.app_id.clone()),
                }),
                Json(json!({ "action": action })),
            ),
        )
        .await
        .0;
        classify_answer(answer)
    }

    async fn navigate(&mut self, url: &str) -> Result<ActResult, DriveFailure> {
        let answer = capture::as_explorer(
            self.run.clone(),
            crate::mcp::sdk_client::handle_page_navigate(
                State(Arc::clone(&self.state)),
                Query(self.named()),
                Json(json!({ "url": url })),
            ),
        )
        .await
        .0;
        classify_answer(answer)
    }

    async fn forget_position(&mut self) -> Result<(), BarrierMiss> {
        capture::forget_position(
            self.state.app_state.pg_db.clone(),
            CursorKey::for_explorer(self.app_id.clone()),
            BARRIER_TIMEOUT,
        )
        .await
    }

    async fn current_node(&mut self, deadline: Duration) -> Result<NodeReading, BarrierMiss> {
        capture::barrier(
            self.state.app_state.pg_db.clone(),
            CursorKey::for_explorer(self.app_id.clone()),
            deadline,
        )
        .await
    }

    async fn ledger_rows(&mut self) -> Result<LedgerRows, String> {
        let ledger = super::health::get_journey_health(
            State(Arc::clone(&self.state)),
            Path(self.app_id.clone()),
        )
        .await
        .0
        .ledger;
        let read = super::read::read_ledger(&self.state.app_state.pg_db, &self.app_id)
            .await
            .map_err(|e| format!("journey ledger read failed: {e:?}"))?;
        Ok(LedgerRows {
            truncated: read.edges_truncated || read.frontier_truncated,
            rows_unparseable: read.rows_unparseable,
            edges: read.edges,
            frontier: read.frontier,
            ledger,
        })
    }

    fn edge_events(&self) -> EdgeEvents {
        EdgeEvents {
            enqueued: self.run.enqueued(),
            dropped: self.run.dropped(),
        }
    }

    fn ledger_write_failures(&self) -> u64 {
        super::health::write_failures_total()
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

// ---------------------------------------------------------------------------
// The route
// ---------------------------------------------------------------------------

/// Apps with a run in flight: two explorers driving one app would close each
/// other's pending edges.
fn running() -> &'static Mutex<BTreeSet<String>> {
    static RUNNING: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(BTreeSet::new()))
}

pub(crate) struct RunGuard(String);

impl RunGuard {
    pub(crate) fn claim(app_id: &str) -> Option<Self> {
        let mut set = running().lock().unwrap_or_else(|p| p.into_inner());
        set.insert(app_id.to_string())
            .then(|| RunGuard(app_id.to_string()))
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        running()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}

/// A node start must be where the app is, or reachable from it over observed
/// D4-safe edges; checked before any action is taken.
///
/// - It is run by [`explore`] right AFTER the run's one
///   [`ExploreDriver::forget_position`], and relies on it: the producer has
///   forgotten the explorer cursor's position, so this check's snapshot
///   cannot close a previous run's pending edge. It does not forget again.
/// - The node "where the app is" must be FRESH: the producer's resolved
///   snapshot count must have moved past what it was before this check's
///   snapshot (the freshness check [`Run`]'s looks use), else a stale node —
///   one from a previous run — would be named (503).
/// - An authentication refusal is an auth wall, said as one (422): the
///   explorer drives the session the app already has and mints none.
/// - An answer that could not be read is a 502; an app that answered with a
///   failure a 422; one that could not be reached a 503.
pub(crate) async fn check_node_start<D: ExploreDriver>(
    d: &mut D,
    start_key: &str,
) -> Result<(), Refusal> {
    let unavailable = |m: String| Refusal(StatusCode::SERVICE_UNAVAILABLE, m);
    let no_producer = || unavailable("the journey producer could not say where the app is".into());
    let before = d
        .current_node(BARRIER_TIMEOUT)
        .await
        .map_err(|_| no_producer())?
        .snapshots;
    d.snapshot().await.map_err(|f| match f {
        DriveFailure::AuthWall(why) => unprocessable(format!(
            "the app is behind an authentication wall ({why}): the explorer drives the session \
             the app already has and mints none; sign the app in, then start again"
        )),
        DriveFailure::AppAnswered(why) => unprocessable(format!(
            "the app answered the snapshot taken to check the start node with a failure: {why}"
        )),
        DriveFailure::OutcomeUnknown(why) => Refusal(
            StatusCode::BAD_GATEWAY,
            format!(
                "the app's answer to the snapshot taken to check the start node could not be \
                 read: {why}"
            ),
        ),
        DriveFailure::Unreachable(why) => unavailable(format!(
            "the app could not be reached to check the start node: {why}"
        )),
        DriveFailure::Runner(why) => {
            unavailable(format!("the runner failed checking the start node: {why}"))
        }
    })?;
    let reading = d
        .current_node(BARRIER_TIMEOUT)
        .await
        .map_err(|_| no_producer())?;
    if reading.snapshots <= before {
        return Err(unavailable(
            "the journey producer did not receive the snapshot just taken (its node would be \
             stale), so where the app is is unknown"
                .into(),
        ));
    }
    let here = match reading.node {
        Some(n) => n.key(),
        None => return Err(no_producer()),
    };
    let rows = d.ledger_rows().await.map_err(unavailable)?;
    let graph = routing_graph(&rows.edges, &rows.frontier, &rows.ledger);
    if graph.node(start_key).is_none() {
        return Err(unprocessable(format!(
            "start node {start_key} has never been observed for this app"
        )));
    }
    if hops(&graph, &here, start_key).is_none() {
        return Err(unprocessable(format!(
            "the app is at {here}, and no observed D4-safe path leads from there to start node \
             {start_key}; start from a URL instead"
        )));
    }
    Ok(())
}

/// The app id the explorer drives, or why it refuses (a 422 before anything
/// runs).
///
/// - It must be an identifier ([`super::observe::is_identifier`]) EXACTLY as
///   the path gave it — the same check, untrimmed, the observe route holds an
///   app id to ([`super::observe::observe_app_id`]). A padded id (a decoded
///   `%20`) is not an identifier and is refused, never silently rewritten:
///   no journey route normalizes the app id, so a trimmed one would be
///   written under a spelling the read routes would not find.
/// - The runner's OWN webview (compared case-insensitively): D4 rule 3
///   activates any undeclared `menuitem` / `link` / `tab`, and the runner's
///   UI has `role="menuitem"` buttons with no declared effect that act — the
///   wrapper card menu's "Stop" and "Uninstall"
///   (`src/components/wrappers/WrapperCard.tsx`, `MenuItem`). If one is in a
///   snapshot, D4 would fire it, so exploring the runner is not D4-safe. The
///   plan's acceptance target is qontinui-web; this fails closed.
pub(crate) fn explore_app_id(app_id: &str) -> Result<String, Refusal> {
    if !super::observe::is_identifier(app_id) {
        return Err(unprocessable(
            "app id must be an identifier ([A-Za-z0-9._:+-], ≤128)",
        ));
    }
    if app_id.eq_ignore_ascii_case(crate::spec_api::storage::RUNNER_APP_ID) {
        return Err(unprocessable(format!(
            "the explorer does not drive {app_id}: the runner's own webview has menu items with \
             no declared effect that act (e.g. a wrapper's \"Uninstall\"), which D4's \
             navigation rule would activate"
        )));
    }
    Ok(app_id.to_string())
}

/// `POST /apps/{app_id}/journey/explore`.
///
/// - 400: not JSON — a body serde cannot parse as JSON at all (a syntax
///   error or a truncated body), the same status axum's `Json` extractor
///   answers on the runner's other JSON routes and the observe route
///   answers; 422: a JSON body of the wrong shape — a missing budget field,
///   an unknown field, a bad `start` —, a node start that cannot be reached,
///   an app id that is not an identifier or names the runner's own app
///   ([`explore_app_id`]), or a start the app refused — its start URL, or its
///   first snapshot (no row: no stop word is true of a run that never began)
///   — or a node start behind an authentication wall;
/// - 502: the app's answer to the start URL, the first snapshot, or the node
///   start check's snapshot could not be read (no row);
/// - 409: a run is already exploring this app;
/// - 503: the journey ledger is not `writing` (the explorer has no private
///   write path, so a run would add nothing), the runner itself failed
///   mid-run (no row is written: none of the seven stop reasons is true), or
///   the node start check could not reach the app or ask the producer;
/// - 200: the run's answer, with `persistence` saying whether its row was
///   written.
pub async fn post_explore(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let app_id = match explore_app_id(&app_id) {
        Ok(id) => id,
        Err(r) => return r.into_response(),
    };
    let request = match parse_explore_request(&body) {
        Ok(r) => r,
        Err(r) => return r.into_response(),
    };
    let ledger = super::health::get_journey_health(State(Arc::clone(&state)), Path(app_id.clone()))
        .await
        .0
        .ledger;
    if ledger.state != LedgerState::Writing {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "the journey ledger is not being written, so an exploration would add \
                          no edges; see GET /apps/{app_id}/journey/health",
                "ledger": ledger,
            })),
        )
            .into_response();
    }
    let Some(_guard) = RunGuard::claim(&app_id) else {
        return Refusal(
            StatusCode::CONFLICT,
            format!("an exploration of {app_id} is already running"),
        )
        .into_response();
    };
    let run_id = uuid::Uuid::new_v4().to_string();
    let started_at = chrono::Utc::now();
    let mut driver = LiveDriver {
        state: Arc::clone(&state),
        app_id: app_id.clone(),
        run: ExplorerRun::new(run_id.clone()),
        started: Instant::now(),
    };
    let outcome = match explore(&mut driver, &request).await {
        Ok(o) => o,
        Err(no_row) => {
            let (status, what) = match no_row.kind {
                NoRowKind::Internal => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the exploration stopped on a runner-side failure",
                ),
                NoRowKind::StartRefused => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "the exploration could not start",
                ),
                NoRowKind::StartUnknown => {
                    (StatusCode::BAD_GATEWAY, "the exploration could not start")
                }
                NoRowKind::StartCheck(status) => (status, "the node start was refused"),
            };
            return (
                status,
                Json(json!({
                    "error": format!("{what}: {}", no_row.detail),
                    "runId": run_id,
                    "spentActions": no_row.spent_actions,
                    "edgesAdded": no_row.edge_events.enqueued,
                    "edgeEventsDropped": no_row.edge_events.dropped,
                    "persisted": false,
                })),
            )
                .into_response();
        }
    };
    let mut result = assemble(
        run_id,
        app_id,
        &request,
        started_at,
        driver.elapsed(),
        outcome,
    );
    result.persistence = persist(&state.app_state.pg_db, &result).await;
    Json(result).into_response()
}

#[cfg(test)]
#[path = "explorer_tests.rs"]
mod tests;
