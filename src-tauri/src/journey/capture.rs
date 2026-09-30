//! Capture of journey edges at the runner's UI action choke points.
//!
//! Plan `2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time`
//! D3: every agent UI action already passes through the runner, so the edge
//! ledger is fed at the action handlers — all of them, or edges silently go
//! missing for whichever transport a skill happens to use. Modelled on
//! `state_discovery::capture::enqueue_observation`: fire-and-forget, a
//! once-per-process schema probe, and never an error to the caller.
//!
//! ## Shape
//!
//! Handlers call one of [`record_action`], [`record_snapshot`] or
//! [`record_diff`] AFTER their response is assembled. Each is a synchronous,
//! non-blocking `try_send` of a [`JourneyEvent`] onto one bounded channel;
//! ONE worker task (spawned on first use) owns all journey state and does all
//! node resolution and every database write. That single consumer is what
//! keeps an action and the snapshot that closes it in order — two independent
//! `tokio::spawn`s could run in either order and close an edge with the wrong
//! destination.
//!
//! - **Diff paths write immediately**: the execute-with-diff response carries
//!   `beforeSnapshot` / `afterSnapshot`, so from/to nodes resolve from those.
//!   A diff response without them (the WebSocket relay's `{actionResult, diff}`
//!   shape) degrades to the no-snapshot rule below rather than guessing.
//! - **No-snapshot paths open a PENDING edge** per `(app_id, runner_instance)`
//!   whose `from_node` is the last node this runner resolved for the app. The
//!   next snapshot of that app closes it; a second action arriving first
//!   closes it with `to_node: None`, `outcome: to_node_unobserved` (D3).
//!
//! Node resolution (Phase 0 decision 1) never fetches a snapshot and never
//! runs on a request path: it evaluates the ONE page spec matched to the
//! snapshot in hand, on the worker.
//!
//! ## Degrade loudly
//!
//! Unlike the co-occurrence precedent (which only `warn!`s), every outcome —
//! schema absent, a validation failure, a refused INSERT, a full queue — is
//! counted into [`super::health`], which `GET /apps/{app_id}/journey/health`
//! reports. A ledger that stopped growing must never read as a finished one.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use qontinui_types::journey::{
    ChokePoint, EdgeOutcome, JourneyEdgeObservation, JourneyNode, JourneyTrigger,
    NavigationTriggerKind, RunKind,
};
use tokio::sync::{mpsc, OnceCell};
use tracing::warn;

use crate::database::pg::PgDb;

use super::frontier;
use super::health;
use super::node::{
    build_node, component_action_fingerprint, extract_affordances, page_identity,
    present_state_ids, unknown_node, AffordanceIndex, SpecLookup,
};

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// This binary's build id — the value `/health` reports as `buildId`.
pub(crate) const RUNNER_BUILD_ID: &str = env!("RUNNER_BUILD_ID");

/// The runner instance label, with the same meaning as
/// `co_occurrence_observations.runner_instance`: `QONTINUI_RUNNER_ROLE`, else
/// `primary` — byte-for-byte the expression the snapshot handler always used,
/// now shared so the two ledgers cannot label one runner differently.
pub(crate) fn runner_instance() -> String {
    std::env::var("QONTINUI_RUNNER_ROLE")
        .ok()
        .unwrap_or_else(|| "primary".to_string())
}

/// One pending-edge slot per app per runner instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CursorKey {
    pub app_id: String,
    pub runner_instance: String,
}

impl CursorKey {
    pub(crate) fn new(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            runner_instance: runner_instance(),
        }
    }
}

/// Who acted, as far as the request says. Nothing here is invented: an absent
/// value is `None` and lands as SQL NULL ("not reported").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Provenance {
    /// `SdkAppInfo.version` of the app acted on; `None` = not reported (U4).
    pub app_version: Option<String>,
    /// The caller's run: the `task_run_id` query parameter the action routes
    /// already read for `ui_bridge_events` persistence; `None` when absent.
    pub run_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

/// What an action targeted, by the identifiers the request itself names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TriggerTarget {
    /// An element, by its registry id (resolved to a fingerprint against the
    /// snapshot the action was taken from).
    Element(String),
    /// A component action.
    Component {
        component_id: String,
        action_id: String,
    },
    /// The request named no resolvable target (e.g. a natural-language
    /// `instruction` on execute-with-diff).
    Unresolved,
}

/// An action as a handler describes it. CLOSED over structure: there is no
/// field for a typed value, and the constructors below read only the action
/// NAME and the TARGET id out of a request body — never `params`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionSpec {
    /// Wire `actionType`: the action verb, a component action id, or
    /// `batch:<n>`.
    pub action_type: String,
    /// The action whose declared effect the trigger carries (a custom action
    /// id on an element). `None` when no single action applies (a batch).
    pub effect_action: Option<String>,
    pub target: TriggerTarget,
    pub choke_point: ChokePoint,
}

impl ActionSpec {
    /// An element action (`/control/element/{id}/action`, the SDK twin).
    pub(crate) fn element(element_id: &str, action: &str, choke_point: ChokePoint) -> Self {
        Self {
            action_type: action.to_string(),
            effect_action: Some(action.to_string()),
            target: TriggerTarget::Element(element_id.to_string()),
            choke_point,
        }
    }

    /// A batch of element actions is ONE trigger: `actionType` is
    /// `batch:<n>` and the target is the FIRST step's element. `None` for an
    /// empty batch — nothing acted.
    pub(crate) fn batch(steps: &[serde_json::Value]) -> Option<Self> {
        let first = steps.first()?;
        let target = first
            .get("elementId")
            .or_else(|| first.get("element_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|id| TriggerTarget::Element(id.to_string()))
            .unwrap_or(TriggerTarget::Unresolved);
        Some(Self {
            action_type: format!("batch:{}", steps.len()),
            effect_action: None,
            target,
            choke_point: ChokePoint::BatchAction,
        })
    }

    /// A component action.
    pub(crate) fn component(component_id: &str, action_id: &str) -> Self {
        Self {
            action_type: action_id.to_string(),
            effect_action: None,
            target: TriggerTarget::Component {
                component_id: component_id.to_string(),
                action_id: action_id.to_string(),
            },
            choke_point: ChokePoint::ComponentAction,
        }
    }

    /// An execute-with-diff request body, in either spelling the routes
    /// accept (`elementAction: {elementId, action}` or the flat
    /// `elementId` + `operation`/`action`). A body carrying only an
    /// `instruction` has no structural target.
    pub(crate) fn with_diff(body: &serde_json::Value) -> Self {
        let envelope = body.get("elementAction");
        let element_id = envelope
            .and_then(|e| e.get("elementId"))
            .or_else(|| body.get("elementId"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let action = envelope
            .and_then(|e| e.get("action"))
            .or_else(|| body.get("operation"))
            .or_else(|| body.get("action"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let action_type = match (action, body.get("instruction").is_some()) {
            (Some(a), _) => a.to_string(),
            (None, true) => "instruction".to_string(),
            (None, false) => "unknown".to_string(),
        };
        Self {
            effect_action: action.map(String::from),
            action_type,
            target: element_id
                .map(|id| TriggerTarget::Element(id.to_string()))
                .unwrap_or(TriggerTarget::Unresolved),
            choke_point: ChokePoint::ExecuteWithDiff,
        }
    }
}

/// Build the wire trigger, resolving the target against the affordances of
/// the snapshot the action was taken FROM.
///
/// An element id absent from that snapshot yields `targetFingerprint: None`
/// ("not resolved") rather than a fingerprint of something else. A component
/// action's fingerprint is derived from its ids, so it resolves without a
/// snapshot; its declared effect still needs one.
pub(crate) fn resolve_trigger(action: &ActionSpec, from: &AffordanceIndex) -> JourneyTrigger {
    let (target_fingerprint, target_role, declared_effect) = match &action.target {
        TriggerTarget::Element(id) => match from.elements.get(id) {
            Some(el) => (
                Some(el.affordance.fingerprint.clone()),
                el.affordance.role.clone(),
                action
                    .effect_action
                    .as_ref()
                    .and_then(|a| el.action_effects.get(a).copied().flatten()),
            ),
            None => (None, None, None),
        },
        TriggerTarget::Component {
            component_id,
            action_id,
        } => (
            Some(component_action_fingerprint(component_id, action_id)),
            None,
            from.component_actions
                .get(&(component_id.clone(), action_id.clone()))
                .and_then(|a| a.declared_effect),
        ),
        TriggerTarget::Unresolved => (None, None, None),
    };
    JourneyTrigger {
        action_type: action.action_type.clone(),
        target_fingerprint,
        target_role,
        declared_effect,
        navigation_trigger: NavigationTriggerKind::Affordance,
        choke_point: action.choke_point,
    }
}

/// Did a control-route action act on the UI, and did it fail?
///
/// - `None` — the runner refused the request itself (a 4xx: malformed body,
///   unknown element, unknown action) — no UI action occurred, so no edge;
/// - `Some(true)` — the action was attempted and failed (a 5xx, or a 200 whose
///   envelope says `success: false`) — an `error` edge;
/// - `Some(false)` — the action succeeded.
pub(crate) fn control_action_verdict<T: serde::Serialize>(
    result: &Result<
        axum::Json<crate::mcp::types::ApiResponse<T>>,
        (
            axum::http::StatusCode,
            axum::Json<crate::mcp::types::ApiResponse<()>>,
        ),
    >,
) -> Option<bool> {
    match result {
        Ok(body) => Some(!body.success),
        Err((status, _)) if status.is_client_error() => None,
        Err(_) => Some(true),
    }
}

/// `task_run_id` (the run the action routes already attribute events to) as
/// the ledger's `run_id`.
pub(crate) fn run_id_from_task_run(task_run_id: Option<i64>) -> Option<String> {
    task_run_id.map(|id| id.to_string())
}

// ---------------------------------------------------------------------------
// Pending-edge state machine (pure)
// ---------------------------------------------------------------------------

/// How an edge closed. `failed` wins over a settle timeout, which wins over
/// the key comparison: an errored action's destination is data, but its
/// outcome is the error.
pub(crate) fn outcome_of(
    from: &JourneyNode,
    to: Option<&JourneyNode>,
    failed: bool,
    settle_timed_out: bool,
) -> EdgeOutcome {
    match to {
        None => EdgeOutcome::ToNodeUnobserved,
        Some(_) if failed => EdgeOutcome::Error,
        Some(_) if settle_timed_out => EdgeOutcome::SettleTimeout,
        Some(to) if to.key() == from.key() => EdgeOutcome::NoChange,
        Some(_) => EdgeOutcome::Changed,
    }
}

/// An edge ready to be finalized into a row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgeDraft {
    pub key: CursorKey,
    pub provenance: Provenance,
    pub from_node: JourneyNode,
    pub to_node: Option<JourneyNode>,
    pub trigger: JourneyTrigger,
    pub outcome: EdgeOutcome,
}

#[derive(Debug, Clone, PartialEq)]
struct PendingEdge {
    from_node: JourneyNode,
    trigger: JourneyTrigger,
    failed: bool,
    provenance: Provenance,
}

impl PendingEdge {
    fn close(self, key: &CursorKey, to: Option<JourneyNode>) -> EdgeDraft {
        let outcome = outcome_of(&self.from_node, to.as_ref(), self.failed, false);
        EdgeDraft {
            key: key.clone(),
            provenance: self.provenance,
            from_node: self.from_node,
            to_node: to,
            trigger: self.trigger,
            outcome,
        }
    }
}

#[derive(Debug, Default)]
struct Cursor {
    last_node: Option<JourneyNode>,
    last_affordances: AffordanceIndex,
    pending: Option<PendingEdge>,
}

/// Per-`(app_id, runner_instance)` journey state: the last node resolved, its
/// affordances, and at most ONE pending edge.
#[derive(Debug, Default)]
pub(crate) struct Cursors {
    map: HashMap<CursorKey, Cursor>,
}

impl Cursors {
    /// An action with no snapshot in hand. Opens a pending edge from the last
    /// node this runner resolved for the app (an unmodelled unknown node when
    /// none is known yet — the edge is still recorded, never skipped).
    ///
    /// Returns the edge this action DISPLACED: a still-pending previous action
    /// closes with an unobserved destination.
    pub(crate) fn open(
        &mut self,
        key: &CursorKey,
        action: &ActionSpec,
        provenance: Provenance,
        failed: bool,
    ) -> Option<EdgeDraft> {
        let cursor = self.map.entry(key.clone()).or_default();
        let displaced = cursor.pending.take().map(|p| p.close(key, None));
        cursor.pending = Some(PendingEdge {
            from_node: cursor.last_node.clone().unwrap_or_else(unknown_node),
            trigger: resolve_trigger(action, &cursor.last_affordances),
            failed,
            provenance,
        });
        displaced
    }

    /// A snapshot resolved into `node`. Closes the pending edge (if any) with
    /// `node` as its destination, and makes `node` the from-node of whatever
    /// comes next.
    pub(crate) fn observe(
        &mut self,
        key: &CursorKey,
        node: JourneyNode,
        affordances: AffordanceIndex,
    ) -> Option<EdgeDraft> {
        let cursor = self.map.entry(key.clone()).or_default();
        let closed = cursor
            .pending
            .take()
            .map(|p| p.close(key, Some(node.clone())));
        cursor.last_node = Some(node);
        cursor.last_affordances = affordances;
        closed
    }

    /// An execute-with-diff with BOTH snapshots in hand. The before-snapshot is
    /// itself an observation (it closes any pending edge); the diff edge is
    /// written immediately; the after-snapshot becomes the last node.
    ///
    /// Returns the drafts in write order: the closed pending edge (if any),
    /// then the diff edge.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_immediate(
        &mut self,
        key: &CursorKey,
        from: JourneyNode,
        from_affordances: AffordanceIndex,
        to: JourneyNode,
        to_affordances: AffordanceIndex,
        action: &ActionSpec,
        provenance: Provenance,
        failed: bool,
        settle_timed_out: bool,
    ) -> Vec<EdgeDraft> {
        let trigger = resolve_trigger(action, &from_affordances);
        let mut drafts: Vec<EdgeDraft> = self
            .observe(key, from.clone(), from_affordances)
            .into_iter()
            .collect();
        let outcome = outcome_of(&from, Some(&to), failed, settle_timed_out);
        drafts.push(EdgeDraft {
            key: key.clone(),
            provenance,
            from_node: from,
            to_node: Some(to.clone()),
            trigger,
            outcome,
        });
        let cursor = self.map.entry(key.clone()).or_default();
        cursor.last_node = Some(to);
        cursor.last_affordances = to_affordances;
        drafts
    }

    /// Pending edges currently open, across every app.
    pub(crate) fn pending_count(&self) -> usize {
        self.map.values().filter(|c| c.pending.is_some()).count()
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// Turn a draft into the contract row: a fresh id, the close time, and this
/// runner's own provenance.
pub(crate) fn finalize(draft: EdgeDraft) -> JourneyEdgeObservation {
    JourneyEdgeObservation {
        id: uuid::Uuid::new_v4().to_string(),
        observed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        app_id: draft.key.app_id,
        app_version: draft.provenance.app_version,
        runner_build_id: RUNNER_BUILD_ID.to_string(),
        runner_instance: draft.key.runner_instance,
        run_id: draft.provenance.run_id,
        run_kind: RunKind::AgentAction,
        from_node: draft.from_node,
        to_node: draft.to_node,
        trigger: draft.trigger,
        outcome: draft.outcome,
        invalidated_at: None,
        invalidated_reason: None,
        invalidated_by: None,
        invalidation_token: None,
    }
}

/// The edge INSERT and, in the same statement, the frontier DELETE it
/// implies: once an affordance has been the trigger of an edge from a node it
/// is no longer frontier there. One statement, so the two cannot disagree.
///
/// Casts: `$1::text::uuid` and `$2::text::timestamptz` so the row carries
/// exactly the id / close time the validated struct carries; the three JSONB
/// columns bind `serde_json::Value`s. `$10` (`to_node`) binds
/// `Option<serde_json::Value>`, and `None` is SQL NULL — NEVER a JSON `null`,
/// which the migration's `jsonb_typeof` CHECK rejects and which would satisfy
/// `NOT NULL`-style reasoning while naming nothing (see [`EdgeInsert`]).
///
/// The DELETE matches nothing when `$14` (the trigger's fingerprint) is NULL.
pub(crate) const EDGE_WRITE_SQL: &str = r#"WITH inserted AS (
    INSERT INTO project.journey_edge_observations
        (id, observed_at, app_id, app_version, runner_build_id, runner_instance,
         run_id, run_kind, from_node, to_node, trigger, outcome)
    VALUES ($1::text::uuid, $2::text::timestamptz, $3, $4, $5, $6,
            $7, $8, $9::jsonb, $10::jsonb, $11::jsonb, $12)
    RETURNING id
), cleared AS (
    DELETE FROM project.journey_frontier f
     WHERE f.app_id = $3
       AND f.node_key = $13
       AND f.affordance_fingerprint = $14
       AND EXISTS (SELECT 1 FROM inserted)
    RETURNING 1
)
SELECT (SELECT count(*) FROM inserted) AS inserted,
       (SELECT count(*) FROM cleared) AS cleared"#;

/// Number of parameters [`EDGE_WRITE_SQL`] binds; sizes the call site's
/// params array so SQL/param arity drift is a compile error plus a test.
pub(crate) const EDGE_WRITE_BINDS: usize = 14;

/// The bind values of [`EDGE_WRITE_SQL`], built by a pure function so the
/// null-handling of `to_node` is testable without a database.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgeInsert {
    pub id: String,
    pub observed_at: String,
    pub app_id: String,
    pub app_version: Option<String>,
    pub runner_build_id: String,
    pub runner_instance: String,
    pub run_id: Option<String>,
    pub run_kind: String,
    pub from_node: serde_json::Value,
    /// `None` binds SQL NULL. Never `Some(Value::Null)` — see [`edge_insert`].
    pub to_node: Option<serde_json::Value>,
    pub trigger: serde_json::Value,
    pub outcome: String,
    pub from_node_key: String,
    pub target_fingerprint: Option<String>,
}

/// Validate a row against the contract and produce its binds.
///
/// Validation runs here, before every write: a failure is returned as the
/// contract error's text and counted as a write failure by the caller —
/// never a silent drop.
pub(crate) fn edge_insert(obs: &JourneyEdgeObservation) -> Result<EdgeInsert, String> {
    obs.validate()
        .map_err(|e| format!("contract validation: {e}"))?;
    let to_json = |n: &JourneyNode| serde_json::to_value(n).map_err(|e| e.to_string());
    let to_node = match &obs.to_node {
        // SQL NULL. `serde_json::to_value(&None::<JourneyNode>)` would be a
        // JSON `null`, which is exactly what must never reach the column.
        None => None,
        Some(n) => Some(to_json(n)?),
    };
    Ok(EdgeInsert {
        id: obs.id.clone(),
        observed_at: obs.observed_at.clone(),
        app_id: obs.app_id.clone(),
        app_version: obs.app_version.clone(),
        runner_build_id: obs.runner_build_id.clone(),
        runner_instance: obs.runner_instance.clone(),
        run_id: obs.run_id.clone(),
        run_kind: obs.run_kind.as_str().to_string(),
        from_node: to_json(&obs.from_node)?,
        to_node,
        trigger: serde_json::to_value(&obs.trigger).map_err(|e| e.to_string())?,
        outcome: obs.outcome.as_str().to_string(),
        from_node_key: obs.from_node.key(),
        target_fingerprint: obs.trigger.target_fingerprint.clone(),
    })
}

// ---------------------------------------------------------------------------
// Schema probe
// ---------------------------------------------------------------------------

/// Do both journey tables exist? Schema-qualified `to_regclass`, so the answer
/// does not depend on the pool's `search_path`; `pg_catalog`-backed, so a
/// grant quirk cannot read as "absent" (same reasoning as
/// `state_discovery::capture::OBSERVATION_SCHEMA_PROBE_SQL`).
pub(crate) const JOURNEY_SCHEMA_PROBE_SQL: &str = r#"SELECT to_regclass('project.journey_edge_observations') IS NOT NULL,
                      to_regclass('project.journey_frontier') IS NOT NULL"#;

/// "Exists" is not "readable": read at most one row from each table so a
/// privilege failure surfaces as a probe error instead of every write failing.
pub(crate) const JOURNEY_READ_PROBE_SQL: &str = r#"SELECT (SELECT e.id FROM project.journey_edge_observations e LIMIT 1),
                      (SELECT f.node_key FROM project.journey_frontier f LIMIT 1)"#;

/// Bound on the once-per-process probe — the same 5 s the pool uses for
/// itself, for the same stall reason as the co-occurrence probe.
pub(crate) const JOURNEY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The once-per-process answer to "can the journey ledger be written here?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchemaProbe {
    /// Both tables exist and are readable.
    Present,
    /// At least one table does not exist. The names are the missing ones.
    Absent { missing: Vec<&'static str> },
    /// The probe itself failed (error or timeout). Writes are not attempted
    /// for the life of the process; health reports it as a failure, never as
    /// "absent" and never as "writing".
    Failed { error: String },
}

/// Process-wide cache of the probe. Keyed on nothing, like
/// `OBSERVATION_APP_ID_SUPPORTED`: the runner constructs exactly one `PgDb`,
/// and a migration is followed by a restart, which re-probes.
static JOURNEY_SCHEMA: OnceCell<SchemaProbe> = OnceCell::const_new();

/// The probe's answer if it has completed, without running it.
pub(crate) fn journey_schema_state() -> Option<&'static SchemaProbe> {
    JOURNEY_SCHEMA.get()
}

/// Resolve — once per process — whether the journey tables are present,
/// warning once when they are not.
pub(crate) async fn journey_schema_supported(
    client: &tokio_postgres::Client,
) -> &'static SchemaProbe {
    JOURNEY_SCHEMA
        .get_or_init(|| async {
            let probed = tokio::time::timeout(JOURNEY_PROBE_TIMEOUT, async {
                let row = client.query_one(JOURNEY_SCHEMA_PROBE_SQL, &[]).await?;
                let has_edges = row.try_get::<_, bool>(0).unwrap_or(false);
                let has_frontier = row.try_get::<_, bool>(1).unwrap_or(false);
                if has_edges && has_frontier {
                    client.query_one(JOURNEY_READ_PROBE_SQL, &[]).await?;
                }
                Ok::<_, tokio_postgres::Error>((has_edges, has_frontier))
            })
            .await;
            let answer = match probed {
                Ok(Ok((true, true))) => SchemaProbe::Present,
                Ok(Ok((has_edges, has_frontier))) => {
                    let mut missing = Vec::new();
                    if !has_edges {
                        missing.push("project.journey_edge_observations");
                    }
                    if !has_frontier {
                        missing.push("project.journey_frontier");
                    }
                    SchemaProbe::Absent { missing }
                }
                Ok(Err(e)) => SchemaProbe::Failed {
                    error: crate::database::pg::pg_err("journey schema probe", &e),
                },
                Err(_) => SchemaProbe::Failed {
                    error: format!(
                        "journey schema probe did not answer within {JOURNEY_PROBE_TIMEOUT:?}"
                    ),
                },
            };
            if answer != SchemaProbe::Present {
                warn!(
                    "journey::capture: the journey ledger is NOT being written for this process \
                     ({:?}). GET /apps/<app_id>/journey/health reports the cause. Remedy: apply \
                     qontinui-web migration journey_01_edge_ledger (an existing embedded database \
                     never receives new tables — plan Phase 0 decision 4), then restart the runner \
                     to re-probe.",
                    answer
                );
            }
            answer
        })
        .await
}

// ---------------------------------------------------------------------------
// Events and the worker
// ---------------------------------------------------------------------------

/// One thing a handler saw. Carries only what the worker needs; a snapshot
/// is shared, not copied, with the co-occurrence capture.
#[derive(Debug)]
pub(crate) enum JourneyEvent {
    /// A no-snapshot action (opens a pending edge).
    Action {
        key: CursorKey,
        provenance: Provenance,
        action: ActionSpec,
        failed: bool,
    },
    /// A successful snapshot of the app (closes a pending edge).
    Snapshot {
        key: CursorKey,
        app_version: Option<String>,
        snapshot: Arc<serde_json::Value>,
    },
    /// An execute-with-diff. `before` / `after` are `None` when the response
    /// carried no snapshots; the edge then takes the pending-edge rule.
    Diff {
        key: CursorKey,
        provenance: Provenance,
        action: ActionSpec,
        failed: bool,
        settle_timed_out: bool,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    },
}

/// Events buffered between the handlers and the worker. A snapshot can be
/// megabytes, so the queue is bounded; an event that does not fit is COUNTED
/// as a failed write (never dropped silently).
const EVENT_QUEUE_CAPACITY: usize = 64;

static EVENT_TX: OnceLock<mpsc::Sender<JourneyEvent>> = OnceLock::new();

/// Hand one event to the journey worker, spawning it on first use.
/// Fire-and-forget: never blocks, never fails the caller.
pub(crate) fn enqueue_edge_observation(pg_db: Arc<PgDb>, event: JourneyEvent) {
    let tx = EVENT_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
        tokio::spawn(run_worker(pg_db, rx));
        tx
    });
    if let Err(e) = tx.try_send(event) {
        let reason = match e {
            mpsc::error::TrySendError::Full(_) => "journey event queue full — event dropped",
            mpsc::error::TrySendError::Closed(_) => "journey worker is not running — event dropped",
        };
        health::record_write_failure(reason.to_string());
    }
}

/// A no-snapshot action through a choke point.
pub(crate) fn record_action(
    pg_db: Arc<PgDb>,
    app_id: &str,
    action: ActionSpec,
    provenance: Provenance,
    failed: bool,
) {
    enqueue_edge_observation(
        pg_db,
        JourneyEvent::Action {
            key: CursorKey::new(app_id),
            provenance,
            action,
            failed,
        },
    );
}

/// A successful snapshot of an app.
pub(crate) fn record_snapshot(
    pg_db: Arc<PgDb>,
    app_id: &str,
    app_version: Option<String>,
    snapshot: Arc<serde_json::Value>,
) {
    enqueue_edge_observation(
        pg_db,
        JourneyEvent::Snapshot {
            key: CursorKey::new(app_id),
            app_version,
            snapshot,
        },
    );
}

/// An execute-with-diff response. Reads `beforeSnapshot`, `afterSnapshot`,
/// `settleTimedOut` at the top level or under `data` (the in-process SDK
/// server's `success(result)` envelope).
pub(crate) fn record_diff(
    pg_db: Arc<PgDb>,
    app_id: &str,
    request_body: &serde_json::Value,
    response: &serde_json::Value,
    provenance: Provenance,
    failed: bool,
) {
    let field = |name: &str| {
        response
            .get(name)
            .or_else(|| response.get("data").and_then(|d| d.get(name)))
            .filter(|v| v.is_object())
            .cloned()
    };
    let settle_timed_out = response
        .get("settleTimedOut")
        .or_else(|| response.get("data").and_then(|d| d.get("settleTimedOut")))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    enqueue_edge_observation(
        pg_db,
        JourneyEvent::Diff {
            key: CursorKey::new(app_id),
            provenance,
            action: ActionSpec::with_diff(request_body),
            failed,
            settle_timed_out,
            before: field("beforeSnapshot"),
            after: field("afterSnapshot"),
        },
    );
}

async fn run_worker(pg_db: Arc<PgDb>, mut rx: mpsc::Receiver<JourneyEvent>) {
    let mut cursors = Cursors::default();
    while let Some(event) = rx.recv().await {
        process_event(&pg_db, &mut cursors, event).await;
        health::set_pending_open(cursors.pending_count() as u64);
    }
}

async fn process_event(pg: &PgDb, cursors: &mut Cursors, event: JourneyEvent) {
    match event {
        JourneyEvent::Action {
            key,
            provenance,
            action,
            failed,
        } => {
            if let Some(displaced) = cursors.open(&key, &action, provenance, failed) {
                write_edge(pg, displaced).await;
            }
        }
        JourneyEvent::Snapshot {
            key,
            app_version,
            snapshot,
        } => {
            let node = resolve_node(pg, &key.app_id, app_version, &snapshot).await;
            let affordances = extract_affordances(&snapshot);
            let closed = cursors.observe(&key, node.clone(), affordances.clone());
            let run_id = closed.as_ref().and_then(|d| d.provenance.run_id.clone());
            if let Some(draft) = closed {
                write_edge(pg, draft).await;
            }
            write_frontier(pg, &key.app_id, &node, &affordances, run_id).await;
        }
        JourneyEvent::Diff {
            key,
            provenance,
            action,
            failed,
            settle_timed_out,
            before,
            after,
        } => match (before, after) {
            (Some(before), Some(after)) => {
                let app_version = provenance.app_version.clone();
                let run_id = provenance.run_id.clone();
                let from = resolve_node(pg, &key.app_id, app_version.clone(), &before).await;
                let to = resolve_node(pg, &key.app_id, app_version, &after).await;
                let from_aff = extract_affordances(&before);
                let to_aff = extract_affordances(&after);
                let drafts = cursors.record_immediate(
                    &key,
                    from.clone(),
                    from_aff.clone(),
                    to.clone(),
                    to_aff.clone(),
                    &action,
                    provenance,
                    failed,
                    settle_timed_out,
                );
                let mut drafts = drafts.into_iter();
                let (closed_pending, diff_edge) = match (drafts.next(), drafts.next()) {
                    (Some(a), Some(b)) => (Some(a), Some(b)),
                    (Some(only), None) => (None, Some(only)),
                    _ => (None, None),
                };
                if let Some(d) = closed_pending {
                    write_edge(pg, d).await;
                }
                // The from-node's frontier first, so the diff edge's own
                // frontier DELETE clears the affordance it fired.
                write_frontier(pg, &key.app_id, &from, &from_aff, run_id.clone()).await;
                if let Some(d) = diff_edge {
                    write_edge(pg, d).await;
                }
                write_frontier(pg, &key.app_id, &to, &to_aff, run_id).await;
            }
            // No snapshots in the response (the relay shape): the diff route
            // is still a choke point, so the action opens a pending edge.
            _ => {
                if let Some(displaced) = cursors.open(&key, &action, provenance, failed) {
                    write_edge(pg, displaced).await;
                }
            }
        },
    }
}

/// One-shot gate for the spec-lookup warning (a configuration fact that would
/// otherwise repeat on every snapshot).
fn should_warn_spec_lookup_once() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    WARNED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Resolve a snapshot to its node (Phase 0 decision 1): evaluate the ONE page
/// spec matched to the snapshot's page, in process, against the snapshot in
/// hand. Never fetches a snapshot; runs only on the worker.
async fn resolve_node(
    pg: &PgDb,
    app_id: &str,
    app_version: Option<String>,
    snapshot: &serde_json::Value,
) -> JourneyNode {
    let identity = page_identity(snapshot);
    let lookup = match identity.spec_lookup_label.clone() {
        None => SpecLookup::NoSpec,
        Some(page_id) => {
            match crate::spec_api::spec_check::parse_supplied_snapshot(snapshot.clone()) {
                Ok(parsed) => {
                    let fingerprint = qontinui_types::spec_check::BridgeFingerprint {
                        app_id: app_id.to_string(),
                        app_version,
                        route: None,
                        bridge_version: None,
                        snapshot_timestamp: String::new(),
                        element_count: parsed.elements.len() as u32,
                    };
                    match crate::spec_api::spec_check::evaluate_page_in_process(
                        pg,
                        app_id,
                        &page_id,
                        &parsed,
                        fingerprint,
                    )
                    .await
                    {
                        Ok(Some(result)) => SpecLookup::Evaluated {
                            spec_id: page_id,
                            present_state_ids: present_state_ids(&result),
                        },
                        Ok(None) => SpecLookup::NoSpec,
                        Err(e) => {
                            if should_warn_spec_lookup_once() {
                                warn!(
                                    "journey::capture: spec lookup for app {} page {} failed ({}); \
                                     the node is recorded unmodelled. Warned once per process.",
                                    app_id, page_id, e
                                );
                            }
                            SpecLookup::NoSpec
                        }
                    }
                }
                // Not a control snapshot (a `SemanticSnapshot` from a diff):
                // the evaluator cannot read it, so the node is unmodelled — but
                // it still names the spec when one exists.
                Err(_) => {
                    if spec_exists(pg, app_id, &page_id).await {
                        SpecLookup::NotEvaluable { spec_id: page_id }
                    } else {
                        SpecLookup::NoSpec
                    }
                }
            }
        }
    };
    build_node(&identity, &lookup)
}

async fn spec_exists(pg: &PgDb, app_id: &str, page_id: &str) -> bool {
    if page_id.is_empty()
        || page_id.contains("..")
        || page_id.contains('/')
        || page_id.contains('\\')
    {
        return false;
    }
    match crate::spec_api::storage::resolve_specs_root(pg, app_id).await {
        Ok(root) => matches!(
            crate::spec_api::storage::read_ir(&root, app_id, page_id),
            Ok(Some(_))
        ),
        Err(_) => false,
    }
}

/// Get a pooled connection and the probe's answer, or record why not.
async fn writable_connection(pg: &PgDb) -> Option<deadpool_postgres::Object> {
    let conn = match pg.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            health::record_write_failure(format!("PG pool error: {e}"));
            return None;
        }
    };
    let probe = journey_schema_supported(&conn).await;
    match probe {
        SchemaProbe::Present => Some(conn),
        SchemaProbe::Absent { .. } | SchemaProbe::Failed { .. } => {
            health::record_not_written();
            None
        }
    }
}

async fn write_edge(pg: &PgDb, draft: EdgeDraft) {
    let obs = finalize(draft);
    let binds = match edge_insert(&obs) {
        Ok(b) => b,
        Err(e) => {
            health::record_write_failure(e);
            return;
        }
    };
    let Some(conn) = writable_connection(pg).await else {
        return;
    };
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); EDGE_WRITE_BINDS] = [
        &binds.id,
        &binds.observed_at,
        &binds.app_id,
        &binds.app_version,
        &binds.runner_build_id,
        &binds.runner_instance,
        &binds.run_id,
        &binds.run_kind,
        &binds.from_node,
        &binds.to_node,
        &binds.trigger,
        &binds.outcome,
        &binds.from_node_key,
        &binds.target_fingerprint,
    ];
    match conn.query_one(EDGE_WRITE_SQL, &params).await {
        Ok(row) => {
            let cleared = row.try_get::<_, i64>(1).unwrap_or(0);
            health::record_edge_written(cleared.max(0) as u64);
        }
        Err(e) => {
            health::record_write_failure(crate::database::pg::pg_err("journey edge insert", &e));
        }
    }
}

async fn write_frontier(
    pg: &PgDb,
    app_id: &str,
    node: &JourneyNode,
    affordances: &AffordanceIndex,
    run_id: Option<String>,
) {
    let batch = match frontier::frontier_batch(app_id, node, affordances, run_id) {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(e) => {
            health::record_write_failure(e);
            return;
        }
    };
    let Some(conn) = writable_connection(pg).await else {
        return;
    };
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); frontier::FRONTIER_UPSERT_BINDS] = [
        &batch.app_id,
        &batch.node_key,
        &batch.node,
        &batch.run_id,
        &batch.fingerprints,
        &batch.roles,
        &batch.effects,
    ];
    match conn.execute(frontier::FRONTIER_UPSERT_SQL, &params).await {
        Ok(n) => health::record_frontier_upserted(n),
        Err(e) => {
            health::record_write_failure(crate::database::pg::pg_err("journey frontier upsert", &e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journey::node::{Affordance, ElementAffordance};
    use qontinui_types::ir::IrEffect;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn key() -> CursorKey {
        CursorKey {
            app_id: "qontinui-web".into(),
            runner_instance: "primary".into(),
        }
    }

    fn node(spec: &str, states: &[&str]) -> JourneyNode {
        JourneyNode::new(
            Some(spec.to_string()),
            states.iter().map(|s| s.to_string()),
            None,
            None,
        )
    }

    fn click(id: &str) -> ActionSpec {
        ActionSpec::element(id, "click", ChokePoint::ElementAction)
    }

    fn affordances_with(id: &str, fp: &str) -> AffordanceIndex {
        let mut idx = AffordanceIndex::default();
        let mut action_effects = BTreeMap::new();
        action_effects.insert("wipe".to_string(), Some(IrEffect::Destructive));
        idx.elements.insert(
            id.to_string(),
            ElementAffordance {
                affordance: Affordance {
                    fingerprint: fp.to_string(),
                    role: Some("button".into()),
                    declared_effect: Some(IrEffect::Destructive),
                },
                action_effects,
            },
        );
        idx
    }

    // ---- pending-edge state machine -------------------------------------

    #[test]
    fn an_action_then_a_snapshot_closes_one_changed_edge() {
        let mut c = Cursors::default();
        assert!(c
            .observe(
                &key(),
                node("home", &["idle"]),
                affordances_with("b", "fp-b")
            )
            .is_none());
        assert!(c
            .open(&key(), &click("b"), Provenance::default(), false)
            .is_none());
        assert_eq!(c.pending_count(), 1);
        let edge = c
            .observe(
                &key(),
                node("detail", &["open"]),
                AffordanceIndex::default(),
            )
            .expect("the snapshot closes the pending edge");
        assert_eq!(c.pending_count(), 0);
        assert_eq!(edge.outcome, EdgeOutcome::Changed);
        assert_eq!(edge.from_node.key(), "home#idle");
        assert_eq!(
            edge.to_node.as_ref().map(|n| n.key()).as_deref(),
            Some("detail#open")
        );
        assert_eq!(edge.trigger.target_fingerprint.as_deref(), Some("fp-b"));
        assert_eq!(edge.trigger.target_role.as_deref(), Some("button"));
        assert_eq!(edge.trigger.choke_point, ChokePoint::ElementAction);
    }

    #[test]
    fn a_dead_click_is_a_no_change_row_not_an_absent_one() {
        let mut c = Cursors::default();
        c.observe(&key(), node("home", &["idle"]), AffordanceIndex::default());
        c.open(&key(), &click("b"), Provenance::default(), false);
        let edge = c
            .observe(&key(), node("home", &["idle"]), AffordanceIndex::default())
            .unwrap();
        assert_eq!(edge.outcome, EdgeOutcome::NoChange);
        assert!(finalize(edge).validate().is_ok());
    }

    #[test]
    fn a_second_action_overwrites_the_first_as_unobserved() {
        let mut c = Cursors::default();
        c.observe(&key(), node("home", &["idle"]), AffordanceIndex::default());
        assert!(c
            .open(&key(), &click("a"), Provenance::default(), false)
            .is_none());
        let displaced = c
            .open(&key(), &click("b"), Provenance::default(), false)
            .expect("the first pending edge is displaced");
        assert_eq!(displaced.outcome, EdgeOutcome::ToNodeUnobserved);
        assert!(displaced.to_node.is_none());
        assert_eq!(
            c.pending_count(),
            1,
            "the second action is now the pending one"
        );
        // The second edge still starts at the last OBSERVED node: nothing was
        // seen between the two actions.
        let second = c
            .observe(&key(), node("x", &["y"]), AffordanceIndex::default())
            .unwrap();
        assert_eq!(second.from_node.key(), "home#idle");
        assert_eq!(second.trigger.action_type, "click");
    }

    #[test]
    fn an_action_before_any_snapshot_starts_at_an_unknown_node() {
        let mut c = Cursors::default();
        c.open(&key(), &click("b"), Provenance::default(), false);
        let edge = c
            .observe(&key(), node("home", &["idle"]), AffordanceIndex::default())
            .unwrap();
        assert_eq!(edge.from_node.key(), "unmodelled:unknown");
        assert_eq!(
            edge.trigger.target_fingerprint, None,
            "no snapshot → unresolved target"
        );
        assert!(finalize(edge).validate().is_ok());
    }

    #[test]
    fn a_failed_action_closes_as_error() {
        let mut c = Cursors::default();
        c.observe(&key(), node("home", &["idle"]), AffordanceIndex::default());
        c.open(&key(), &click("b"), Provenance::default(), true);
        let edge = c
            .observe(&key(), node("home", &["idle"]), AffordanceIndex::default())
            .unwrap();
        assert_eq!(edge.outcome, EdgeOutcome::Error);
    }

    #[test]
    fn apps_have_independent_pending_edges() {
        let mut c = Cursors::default();
        let other = CursorKey {
            app_id: "qontinui-runner".into(),
            runner_instance: "primary".into(),
        };
        c.open(&key(), &click("a"), Provenance::default(), false);
        assert!(c
            .open(&other, &click("b"), Provenance::default(), false)
            .is_none());
        assert_eq!(c.pending_count(), 2);
    }

    // ---- diff path -------------------------------------------------------

    #[test]
    fn a_diff_writes_one_edge_and_moves_the_cursor() {
        let mut c = Cursors::default();
        let action = ActionSpec::with_diff(&json!({"elementId": "b", "operation": "wipe"}));
        let drafts = c.record_immediate(
            &key(),
            node("home", &["idle"]),
            affordances_with("b", "fp-b"),
            node("home", &["empty"]),
            AffordanceIndex::default(),
            &action,
            Provenance {
                app_version: Some("1.2.3".into()),
                run_id: Some("42".into()),
            },
            false,
            false,
        );
        assert_eq!(drafts.len(), 1);
        let edge = &drafts[0];
        assert_eq!(edge.outcome, EdgeOutcome::Changed);
        assert_eq!(edge.trigger.choke_point, ChokePoint::ExecuteWithDiff);
        assert_eq!(edge.trigger.action_type, "wipe");
        assert_eq!(edge.trigger.declared_effect, Some(IrEffect::Destructive));
        assert_eq!(edge.provenance.run_id.as_deref(), Some("42"));
        // The after-node is the from-node of the next action.
        c.open(&key(), &click("z"), Provenance::default(), false);
        let next = c
            .observe(&key(), node("q", &["r"]), AffordanceIndex::default())
            .unwrap();
        assert_eq!(next.from_node.key(), "home#empty");
    }

    #[test]
    fn a_diff_before_snapshot_closes_a_pending_edge_first() {
        let mut c = Cursors::default();
        c.observe(&key(), node("a", &["1"]), AffordanceIndex::default());
        c.open(&key(), &click("x"), Provenance::default(), false);
        let drafts = c.record_immediate(
            &key(),
            node("b", &["2"]),
            AffordanceIndex::default(),
            node("b", &["2"]),
            AffordanceIndex::default(),
            &ActionSpec::with_diff(&json!({"elementId": "y", "action": "click"})),
            Provenance::default(),
            false,
            true,
        );
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts[0].to_node.as_ref().unwrap().key(), "b#2");
        assert_eq!(drafts[0].outcome, EdgeOutcome::Changed);
        assert_eq!(drafts[1].outcome, EdgeOutcome::SettleTimeout);
    }

    #[test]
    fn outcome_precedence() {
        let a = node("a", &["1"]);
        let b = node("b", &["1"]);
        assert_eq!(
            outcome_of(&a, None, true, true),
            EdgeOutcome::ToNodeUnobserved
        );
        assert_eq!(outcome_of(&a, Some(&b), true, true), EdgeOutcome::Error);
        assert_eq!(
            outcome_of(&a, Some(&b), false, true),
            EdgeOutcome::SettleTimeout
        );
        assert_eq!(
            outcome_of(&a, Some(&a), false, false),
            EdgeOutcome::NoChange
        );
        assert_eq!(outcome_of(&a, Some(&b), false, false), EdgeOutcome::Changed);
    }

    // ---- row construction -------------------------------------------------

    #[test]
    fn an_unobserved_edge_binds_sql_null_never_json_null() {
        let mut c = Cursors::default();
        c.observe(&key(), node("a", &["1"]), AffordanceIndex::default());
        c.open(&key(), &click("x"), Provenance::default(), false);
        let displaced = c
            .open(&key(), &click("y"), Provenance::default(), false)
            .unwrap();
        let binds = edge_insert(&finalize(displaced)).expect("a valid row");
        assert_eq!(binds.to_node, None, "to_node must bind SQL NULL");
        assert_ne!(binds.to_node, Some(serde_json::Value::Null));
        assert_eq!(binds.outcome, "to_node_unobserved");
    }

    #[test]
    fn an_observed_edge_binds_its_nodes_as_objects() {
        let mut c = Cursors::default();
        c.observe(&key(), node("a", &["1"]), affordances_with("x", "fp-x"));
        c.open(&key(), &click("x"), Provenance::default(), false);
        let edge = c
            .observe(&key(), node("b", &["2"]), AffordanceIndex::default())
            .unwrap();
        let binds = edge_insert(&finalize(edge)).unwrap();
        assert!(binds.from_node.is_object());
        assert!(binds.to_node.as_ref().is_some_and(|v| v.is_object()));
        assert!(binds.trigger.is_object());
        assert_eq!(binds.from_node_key, "a#1");
        assert_eq!(binds.target_fingerprint.as_deref(), Some("fp-x"));
        assert_eq!(binds.run_kind, "agent_action");
        assert_eq!(binds.runner_build_id, RUNNER_BUILD_ID);
        assert!(chrono::DateTime::parse_from_rfc3339(&binds.observed_at).is_ok());
        assert!(uuid::Uuid::parse_str(&binds.id).is_ok());
    }

    #[test]
    fn a_contract_violation_is_an_error_not_a_row() {
        // A pageLabel carrying a '/' is the schemas crate's tripwire for a
        // leaked path; the producer must refuse the row, and the caller
        // counts the refusal.
        let bad = JourneyNode::new(
            None,
            Vec::<String>::new(),
            None,
            Some("search/secret".into()),
        );
        let draft = EdgeDraft {
            key: key(),
            provenance: Provenance::default(),
            from_node: bad,
            to_node: Some(node("a", &["1"])),
            trigger: resolve_trigger(&click("x"), &AffordanceIndex::default()),
            outcome: EdgeOutcome::Changed,
        };
        let err = edge_insert(&finalize(draft)).expect_err("validation must refuse it");
        assert!(err.contains("contract validation"), "{err}");
    }

    #[test]
    fn edge_write_binds_match_the_sql() {
        let max = (1..=40)
            .rev()
            .find(|n| EDGE_WRITE_SQL.contains(&format!("${n}")))
            .unwrap_or(0);
        assert_eq!(max, EDGE_WRITE_BINDS);
        for n in 1..=EDGE_WRITE_BINDS {
            assert!(EDGE_WRITE_SQL.contains(&format!("${n}")), "${n} unused");
        }
    }

    #[test]
    fn edge_write_sql_names_the_contract_columns_and_casts() {
        for needle in [
            "INSERT INTO project.journey_edge_observations",
            "(id, observed_at, app_id, app_version, runner_build_id, runner_instance,",
            "run_id, run_kind, from_node, to_node, trigger, outcome)",
            "$1::text::uuid",
            "$2::text::timestamptz",
            "$9::jsonb, $10::jsonb, $11::jsonb",
            "DELETE FROM project.journey_frontier",
            "f.affordance_fingerprint = $14",
        ] {
            assert!(EDGE_WRITE_SQL.contains(needle), "missing {needle:?}");
        }
        assert!(
            !EDGE_WRITE_SQL.contains("timeline"),
            "the timeline column is Phase 4's; this producer never writes it"
        );
    }

    #[test]
    fn schema_probe_is_schema_qualified_and_reads() {
        assert!(
            JOURNEY_SCHEMA_PROBE_SQL.contains("to_regclass('project.journey_edge_observations')")
        );
        assert!(JOURNEY_SCHEMA_PROBE_SQL.contains("to_regclass('project.journey_frontier')"));
        assert!(!JOURNEY_SCHEMA_PROBE_SQL.contains("information_schema"));
        assert_eq!(JOURNEY_READ_PROBE_SQL.matches("LIMIT 1").count(), 2);
    }

    #[test]
    fn a_client_error_opens_no_edge() {
        use crate::mcp::types::ApiResponse;
        use axum::http::StatusCode;
        use axum::Json;
        let refused: Result<Json<ApiResponse<serde_json::Value>>, _> = Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::error("nope".to_string())),
        ));
        assert_eq!(control_action_verdict(&refused), None);
        let failed: Result<Json<ApiResponse<serde_json::Value>>, _> = Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::error("boom".to_string())),
        ));
        assert_eq!(control_action_verdict(&failed), Some(true));
        let ok: Result<_, (StatusCode, Json<ApiResponse<()>>)> =
            Ok(Json(ApiResponse::success(serde_json::json!({}))));
        assert_eq!(control_action_verdict(&ok), Some(false));
    }

    // ---- choke-point coverage -----------------------------------------------

    /// The body of the top-level item `fn <name>` in `source`: from its
    /// signature to the next top-level item. Text-level on purpose — the
    /// handlers need a live `ApiState` to run, and what this pins is the
    /// WIRING, which is otherwise invisible: a transport whose handler never
    /// calls the ledger writes no row and fails nothing.
    fn item_body<'a>(source: &'a str, name: &str) -> &'a str {
        let start = [
            format!("\npub async fn {name}("),
            format!("\nasync fn {name}("),
            format!("\nfn {name}("),
            format!("\npub(crate) fn {name}("),
        ]
        .iter()
        .find_map(|sig| source.find(sig.as_str()))
        .unwrap_or_else(|| panic!("handler `{name}` not found"));
        let rest = source.get(start + 1..).unwrap_or_default();
        let end = rest
            .match_indices("\n}\n")
            .next()
            .map(|(i, _)| i + 3)
            .unwrap_or(rest.len());
        rest.get(..end).unwrap_or(rest)
    }

    /// Every choke point — the five D3 paths plus the SDK component and batch
    /// routes recorded under the same action kinds — and both snapshot
    /// routes that close pending edges, must call the ledger. A missing
    /// transport would otherwise be invisible: edges for whichever transport a
    /// skill uses would simply never appear.
    #[test]
    fn every_choke_point_calls_the_ledger() {
        // cargo runs tests with CWD = crate root (src-tauri).
        let read = |p: &str| std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"));
        let elements = read("src/mcp/ui_bridge/elements.rs");
        let bookmarks = read("src/mcp/ui_bridge/bookmarks.rs");
        let sdk = read("src/mcp/sdk_client.rs");

        let cases: [(&str, &str, &str); 14] = [
            // (1) control element action
            (
                "elements",
                "ui_bridge_execute_action_handler",
                "record_action(",
            ),
            // (2) control batch
            (
                "elements",
                "ui_bridge_batch_actions_handler",
                "record_action(",
            ),
            // (3) control component action
            (
                "elements",
                "ui_bridge_execute_component_action_handler",
                "record_action(",
            ),
            // (4) SDK element action
            ("sdk", "handle_element_action", "record_action("),
            // (5) execute-with-diff: two runner routes + the SDK twin
            (
                "bookmarks",
                "ui_bridge_execute_with_diff_handler",
                "record_diff_result(",
            ),
            (
                "bookmarks",
                "ui_bridge_with_diff_handler",
                "record_diff_result(",
            ),
            ("bookmarks", "record_diff_result", "record_diff("),
            ("sdk", "handle_ct_execute_with_diff", "record_diff("),
            // SDK component action — recorded as `component_action`
            (
                "sdk",
                "handle_component_action",
                "record_sdk_component_action(",
            ),
            (
                "sdk",
                "record_sdk_component_action",
                "ActionSpec::component(",
            ),
            // SDK batch routes — recorded as `batch_action`
            ("sdk", "handle_execute_batch_action", "record_sdk_batch("),
            ("sdk", "handle_control_batch", "record_sdk_batch("),
            // Snapshots that close pending edges
            (
                "elements",
                "ui_bridge_get_snapshot_handler",
                "record_snapshot(",
            ),
            ("sdk", "handle_snapshot", "record_journey_snapshot("),
        ];
        for (file, handler, call) in cases {
            let source = match file {
                "elements" => &elements,
                "bookmarks" => &bookmarks,
                _ => &sdk,
            };
            assert!(
                item_body(source, handler).contains(call),
                "{file}::{handler} must call `{call}` — without it this transport writes no \
                 journey row"
            );
        }
        assert!(item_body(&sdk, "record_journey_snapshot").contains("record_snapshot("));
        assert!(item_body(&sdk, "record_sdk_batch").contains("ActionSpec::batch("));
    }

    // ---- request parsing ---------------------------------------------------

    #[test]
    fn a_batch_is_one_trigger_on_its_first_target() {
        let steps = vec![
            json!({"elementId": "first", "action": "type", "params": {"text": "x"}}),
            json!({"elementId": "second", "action": "click"}),
        ];
        let spec = ActionSpec::batch(&steps).unwrap();
        assert_eq!(spec.action_type, "batch:2");
        assert_eq!(spec.target, TriggerTarget::Element("first".into()));
        assert_eq!(spec.choke_point, ChokePoint::BatchAction);
        assert!(
            ActionSpec::batch(&[]).is_none(),
            "an empty batch acted on nothing"
        );
    }

    #[test]
    fn a_component_trigger_fingerprint_matches_the_frontier_key() {
        let t = resolve_trigger(
            &ActionSpec::component("grid", "purge"),
            &AffordanceIndex::default(),
        );
        assert_eq!(
            t.target_fingerprint.as_deref(),
            Some("component:grid:purge")
        );
        assert_eq!(t.choke_point, ChokePoint::ComponentAction);
    }

    #[test]
    fn with_diff_reads_both_spellings() {
        let a =
            ActionSpec::with_diff(&json!({"elementAction": {"elementId": "e", "action": "click"}}));
        assert_eq!(a.target, TriggerTarget::Element("e".into()));
        assert_eq!(a.action_type, "click");
        let b = ActionSpec::with_diff(&json!({"instruction": "open the settings"}));
        assert_eq!(b.target, TriggerTarget::Unresolved);
        assert_eq!(b.action_type, "instruction");
    }

    // ---- privacy -----------------------------------------------------------

    /// Typed text never reaches a row: the sentinel is typed through an action
    /// request, sits in an input's value in both snapshots, AND is the URL
    /// path's last segment — and appears in no serialized edge or frontier
    /// row.
    #[test]
    fn a_typed_sentinel_appears_in_no_row() {
        const SENTINEL: &str = "zz-sentinel-8f3a91";
        let request = json!({"action": "type", "params": {"text": SENTINEL}});
        let action_name = request["action"].as_str().unwrap();
        let spec = ActionSpec::element("search-box", action_name, ChokePoint::ElementAction);

        let snapshot = |value: &str| {
            json!({
                "page": {
                    "pathname": format!("/search/{SENTINEL}"),
                    "url": format!("http://app.local/search/{SENTINEL}")
                },
                "elements": [{
                    "id": "search-box", "type": "input", "role": "searchbox",
                    "label": "Search", "value": value, "text": value,
                    "actions": ["type", "click"]
                }]
            })
        };
        let before = snapshot("");
        let after = snapshot(SENTINEL);

        let mut c = Cursors::default();
        let from_identity = page_identity(&before);
        let from = build_node(&from_identity, &SpecLookup::NoSpec);
        c.observe(&key(), from.clone(), extract_affordances(&before));
        c.open(&key(), &spec, Provenance::default(), false);
        let to_identity = page_identity(&after);
        let to = build_node(&to_identity, &SpecLookup::NoSpec);
        let edge = c
            .observe(&key(), to.clone(), extract_affordances(&after))
            .unwrap();

        let row = serde_json::to_string(&finalize(edge.clone())).unwrap();
        let binds = edge_insert(&finalize(edge)).unwrap();
        let frontier_after =
            frontier::frontier_batch("qontinui-web", &to, &extract_affordances(&after), None)
                .unwrap()
                .unwrap();
        let frontier_before =
            frontier::frontier_batch("qontinui-web", &from, &extract_affordances(&before), None)
                .unwrap()
                .unwrap();

        for (what, text) in [
            ("edge row", row),
            ("edge binds", format!("{binds:?}")),
            ("frontier (after)", format!("{frontier_after:?}")),
            ("frontier (before)", format!("{frontier_before:?}")),
        ] {
            assert!(
                !text.contains(SENTINEL),
                "{what} leaked the sentinel: {text}"
            );
        }
        assert_eq!(
            from.page_label, None,
            "a pathname-only page has no pageLabel"
        );
    }
}
