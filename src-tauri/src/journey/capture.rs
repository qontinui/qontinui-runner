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
//! - **Every action opens a PENDING edge** (see [`super::cursor`]) whose
//!   `from_node` is the last node this runner resolved for the cursor; the
//!   next control/SDK snapshot of that cursor closes it. Execute-with-diff is
//!   no exception: its `SemanticSnapshot`s are not a shape the spec evaluator
//!   reads, so they would be a SECOND node-identity source. A diff contributes
//!   only an outcome hint (`error` / `verdict_unknown` / `settle_timeout`).
//! - A second action arriving first, or a pending edge older than
//!   [`super::cursor::PENDING_TTL`], closes as `to_node_unobserved` (D3).
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

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use qontinui_types::journey::{EdgeOutcome, JourneyEdgeObservation, JourneyNode, RunKind};
use qontinui_vision_core::ChangeTimeline;
use tokio::sync::mpsc;
use tracing::warn;

use crate::database::pg::PgDb;

use super::cursor::{
    verdict_hint, ActionSpec, CursorKey, Cursors, EdgeDraft, Observed, Provenance,
};
use crate::mcp::ui_bridge::request::ActionOutcome;
use crate::mcp::ui_bridge::types::UiBridgeErrorCode;
use super::frontier;
use super::health;
use super::node::{
    affordance_digest, build_node, extract_affordances, page_identity, present_state_ids,
    AffordanceIndex, SpecLookup,
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

/// The runner-owned tokens that say an action was ATTEMPTED and its own
/// verdict is UNKNOWN (plan
/// `2026-11-18-the-journey-ledger-never-records-an-unknown-action-verdict-as-a-success-or-a-failure`
/// D3). Read from a control route's `error_detail.context.code`, and from an
/// SDK route's top-level `code` ([`verdict_unknown_code`]); either records a
/// `verdict_unknown` edge, never `error` and never a success.
///
/// - `RECOVERY_VERDICT_MISSING` — `/ai/recovery/attempt`'s 500 for a reply
///   that stated no `attemptSucceeded`
///   ([`crate::mcp::ui_bridge::ai_analyze::RECOVERY_VERDICT_MISSING`]);
/// - `SDK_DISPATCH_OUTCOME_UNKNOWN` — an SDK route's dispatch that failed
///   AFTER the request reached the app (a timeout, a dropped socket, an
///   unreadable answer), so whether the app acted is unknown
///   ([`crate::mcp::sdk_client::SDK_DISPATCH_OUTCOME_UNKNOWN`]).
///
/// Add a token here, not a per-route table: the reader is typed, so every
/// route that stamps one is covered without a second edit.
pub(crate) const VERDICT_UNKNOWN_CODES: [&str; 2] = [
    crate::mcp::ui_bridge::ai_analyze::RECOVERY_VERDICT_MISSING,
    crate::mcp::sdk_client::SDK_DISPATCH_OUTCOME_UNKNOWN,
];

/// The verdict-unknown token a body carries, if any: its top-level `code`,
/// else its `error_detail.context.code`.
pub(crate) fn verdict_unknown_code(response: &serde_json::Value) -> Option<&'static str> {
    [
        response.get("code"),
        response.pointer("/error_detail/context/code"),
    ]
    .into_iter()
    .flatten()
    .filter_map(|v| v.as_str())
    .find_map(|c| VERDICT_UNKNOWN_CODES.iter().copied().find(|t| *t == c))
}

/// The prefix of [`INDETERMINATE_ACTION_ERROR`]: the distinguishable half of
/// that contract (see its doc).
const INDETERMINATE_PREFIX: &str = "INDETERMINATE:";

/// Is this an `INDETERMINATE:` error — the marker
/// [`crate::mcp::ui_bridge::request::normalize_action_result`] writes over a
/// result that carried no boolean `success`? Such a body says `success:
/// false` only so no permissive reader downstream takes it for a pass; the
/// action's verdict is UNKNOWN, not a failure.
fn indeterminate_marker(error: Option<&str>) -> bool {
    error.is_some_and(|e| e.starts_with(INDETERMINATE_PREFIX))
}

/// Did a control-route action act on the UI, and what was its verdict? Decided
/// from the response's TYPED fields, never from route strings (plan
/// `2026-11-18-the-journey-ledger-never-records-an-unknown-action-verdict-as-a-success-or-a-failure`
/// D3), so a new route is covered by stamping the right code:
///
/// - a 2xx envelope: `success: true` → [`ActionOutcome::Succeeded`];
///   `success: false` → [`ActionOutcome::Failed`], unless its `error` is the
///   `INDETERMINATE:` marker → [`ActionOutcome::Indeterminate`];
/// - an error whose `error_detail.context.code` is in
///   [`VERDICT_UNKNOWN_CODES`] (any status) → [`ActionOutcome::Indeterminate`]:
///   the action was attempted and its verdict is unknown;
/// - a 4xx whose `error_detail.code` is `ACTION_FAILED` AND whose context
///   carries a post-dispatch re-coder's stamp ([`post_dispatch_action_failure`])
///   → [`ActionOutcome::Failed`]: the action was DISPATCHED and failed
///   (`elements::as_action_failure`, `/ai/recovery/attempt`'s
///   `RECOVERY_FAILED`). The code ALONE is not evidence of dispatch:
///   `classify_transport_error` also codes any refusal whose prose merely
///   contains "Operation failed" / "action failed" as `ACTION_FAILED`, with no
///   context, and that refusal reached no UI;
/// - any other 4xx → `None`: the runner refused the request itself
///   (malformed body, unknown element, unknown action, an unstamped
///   `ACTION_FAILED`), no UI action occurred, so no edge;
/// - any other 5xx → [`ActionOutcome::Failed`] (a transport-level failure;
///   sorting dispatch-time timeouts from pre-dispatch refusals is a named
///   follow-up of that plan, not this reader's job).
pub(crate) fn control_action_verdict<T: serde::Serialize, E: serde::Serialize>(
    result: &Result<
        axum::Json<crate::mcp::types::ApiResponse<T>>,
        (
            axum::http::StatusCode,
            axum::Json<crate::mcp::types::ApiResponse<E>>,
        ),
    >,
) -> Option<ActionOutcome> {
    match result {
        Ok(body) if body.success => Some(ActionOutcome::Succeeded),
        Ok(body) if indeterminate_marker(body.error.as_deref()) => {
            Some(ActionOutcome::Indeterminate)
        }
        Ok(body) => Some(ActionOutcome::Failed(body.error.clone())),
        Err((status, body)) => {
            let detail = body.error_detail.as_ref();
            let context_code = detail
                .and_then(|d| d.context.as_ref())
                .and_then(|c| c.get("code"))
                .and_then(|c| c.as_str());
            if context_code.is_some_and(|c| VERDICT_UNKNOWN_CODES.contains(&c)) {
                return Some(ActionOutcome::Indeterminate);
            }
            if status.is_client_error() {
                return detail
                    .filter(|d| post_dispatch_action_failure(d))
                    .map(|_| ActionOutcome::Failed(body.error.clone()));
            }
            Some(ActionOutcome::Failed(body.error.clone()))
        }
    }
}

/// Is this typed error a GENUINELY post-dispatch action failure? Its code is
/// `ACTION_FAILED` and its context carries the stamp one of the two typed
/// re-coders writes — and the prose classifier never does:
///
/// - `elements::as_action_failure` stamps `context.requested_action` (with
///   `element_id` and the `recovery` trail) on every 400 it re-codes, after
///   `execute_action` dispatched;
/// - `ai_analyze::recovery_error_detail` stamps the recovery token into
///   `context.code` (`RECOVERY_FAILED` and any future token it maps to
///   `ACTION_FAILED`: recovery RAN and did not recover the element).
///
/// `classify_transport_error`'s `ACTION_FAILED` (prose containing "Operation
/// failed" / "action failed") carries no context at all, so it is not trusted:
/// the same reason `bookmarks::is_attempted_failure` reads a message prefix
/// rather than the code.
fn post_dispatch_action_failure(detail: &crate::mcp::ui_bridge::UiBridgeError) -> bool {
    if !matches!(detail.code, UiBridgeErrorCode::ActionFailed) {
        return false;
    }
    let Some(context) = detail.context.as_ref().and_then(|c| c.as_object()) else {
        return false;
    };
    context.get("requested_action").is_some_and(|v| v.is_string())
        || context.get("code").is_some_and(|v| v.is_string())
}

/// [`control_action_verdict`] for a handler that computed its success
/// envelope's verdict itself (a batch aggregating its steps' three-state
/// outcomes): `ok` stands for any 2xx, and an error is read the usual way.
pub(crate) fn control_action_verdict_with<T: serde::Serialize, E: serde::Serialize>(
    result: &Result<
        axum::Json<crate::mcp::types::ApiResponse<T>>,
        (
            axum::http::StatusCode,
            axum::Json<crate::mcp::types::ApiResponse<E>>,
        ),
    >,
    ok: ActionOutcome,
) -> Option<ActionOutcome> {
    match result {
        Ok(_) => Some(ok),
        Err(_) => control_action_verdict(result),
    }
}

/// Record a CONTROL-surface action from its handler's result (M3). The
/// control routes drive the runner's own webview, so the cursor is the
/// runner app's main window. A 4xx the runner answered itself records
/// nothing ([`control_action_verdict`]).
pub(crate) fn record_control_result<T: serde::Serialize, E: serde::Serialize>(
    state: &Arc<crate::mcp::types::ApiState>,
    result: &Result<
        axum::Json<crate::mcp::types::ApiResponse<T>>,
        (
            axum::http::StatusCode,
            axum::Json<crate::mcp::types::ApiResponse<E>>,
        ),
    >,
    action: ActionSpec,
) {
    record_control_verdict(state, control_action_verdict(result), action);
}

/// Record a CONTROL-surface action from a verdict already decided
/// ([`control_action_verdict`] / [`control_action_verdict_with`]). `None`
/// records nothing.
pub(crate) fn record_control_verdict(
    state: &Arc<crate::mcp::types::ApiState>,
    verdict: Option<ActionOutcome>,
    action: ActionSpec,
) {
    if let Some(outcome) = verdict {
        record_action(
            state.app_state.pg_db.clone(),
            CursorKey::new(crate::spec_api::storage::RUNNER_APP_ID, None),
            action,
            Provenance::default(),
            outcome,
        );
    }
}

/// Where an SDK-route action landed (N2, C1), or `None` when it landed
/// nowhere.
///
/// - a refusal ([`sdk_refusal`]) reached no UI → `None`;
/// - an active SDK app was connected → that app, with its reported version
///   and the request's relay `scope` (`tabId`);
/// - no SDK app was connected:
///   - on a route WITH an IPC fallback (`ipc_fallback`), the route acted on
///     the runner's own webview instead, so the action is the runner app's,
///     in its main-window cursor;
///   - on a route WITHOUT one, the dispatch reached nothing → `None`.
pub(crate) fn sdk_action_target(
    active_app: Option<(String, Option<String>)>,
    scope: Option<&str>,
    ipc_fallback: bool,
    response: &serde_json::Value,
) -> Option<(CursorKey, Option<String>)> {
    if sdk_refusal(response) {
        return None;
    }
    match active_app {
        Some((app_id, version)) => Some((CursorKey::new(app_id, scope), version)),
        None if ipc_fallback => Some((
            CursorKey::new(crate::spec_api::storage::RUNNER_APP_ID, None),
            None,
        )),
        None => None,
    }
}

/// What an SDK route's JSON answer says about the action's own verdict (plan
/// `2026-11-18-the-journey-ledger-never-records-an-unknown-action-verdict-as-a-success-or-a-failure`
/// D2), or `None` when it is a refusal that reached no UI ([`sdk_refusal`]):
///
/// - a verdict-unknown token ([`verdict_unknown_code`]) →
///   [`ActionOutcome::Indeterminate`];
/// - otherwise the strict reader [`crate::mcp::ui_bridge::request::action_outcome`]:
///   `success: true` → succeeded, `success: false` → failed, NO boolean
///   `success` → indeterminate (an absent verdict is never a pass);
/// - a `success: false` whose `error` is the `INDETERMINATE:` marker
///   (a body [`crate::mcp::ui_bridge::request::normalize_action_result`]
///   rewrote) → indeterminate, never a failure.
pub(crate) fn sdk_action_verdict(response: &serde_json::Value) -> Option<ActionOutcome> {
    if verdict_unknown_code(response).is_some() {
        return Some(ActionOutcome::Indeterminate);
    }
    if sdk_refusal(response) {
        return None;
    }
    Some(match crate::mcp::ui_bridge::request::action_outcome(response) {
        ActionOutcome::Failed(err) if indeterminate_marker(err.as_deref()) => {
            ActionOutcome::Indeterminate
        }
        other => other,
    })
}

/// The verdict of a `/ui-bridge/sdk/ai/recovery/attempt` answer for the
/// journey ledger, from the HTTP `status` the route answers and its body.
/// The attempt states its own verdict in `data.attemptSucceeded`, so the
/// envelope's `success` alone is not it (an older ui-bridge server answered
/// envelope `success: true` with `data.recovered: false` for an attempt that
/// recovered nothing):
///
/// - a verdict-unknown token → [`ActionOutcome::Indeterminate`];
/// - a refusal ([`sdk_refusal`], a request that never reached an app), or the
///   app's 400 [`SDK_RECOVERY_VALIDATION_ERROR`] for an invalid payload → `None`;
/// - `attemptSucceeded: true` → succeeded, unless the envelope (or `data`)
///   says `success: false` — a server that failed AFTER a strategy acted — which
///   is indeterminate;
/// - `attemptSucceeded: false` → failed;
/// - no boolean `attemptSucceeded`: an explicit `success: false` is a failure
///   below 500 and indeterminate at 500+ (not an attempt verdict); anything
///   else stated no verdict → indeterminate.
///
/// The same reading as the ui-bridge Python client's `read_recovery_attempt`
/// with ONE deliberate, more conservative difference: a 4xx stating
/// `attemptSucceeded: true` beside `success: false` reads as `False` (failed)
/// in the Python client, but as [`ActionOutcome::Indeterminate`] here — a
/// strategy acted and the server then failed, so the ledger records neither a
/// success nor a failure it cannot vouch for.
pub(crate) fn sdk_recovery_verdict(status: u16, body: &serde_json::Value) -> Option<ActionOutcome> {
    if verdict_unknown_code(body).is_some() {
        return Some(ActionOutcome::Indeterminate);
    }
    if sdk_refusal(body) || carries_code(body, &[SDK_RECOVERY_VALIDATION_ERROR]) {
        return None;
    }
    let data = body.get("data").filter(|d| d.is_object());
    let explicit_failure = [Some(body), data]
        .into_iter()
        .flatten()
        .any(|v| v.get("success").and_then(|s| s.as_bool()) == Some(false));
    let error = || {
        body.get("error")
            .and_then(|e| e.as_str())
            .map(str::to_string)
    };
    let stated = data
        .and_then(|d| d.get(crate::mcp::ui_bridge::ai_analyze::ATTEMPT_SUCCEEDED))
        .and_then(|v| v.as_bool());
    Some(match stated {
        Some(true) if explicit_failure => ActionOutcome::Indeterminate,
        Some(true) => ActionOutcome::Succeeded,
        Some(false) => ActionOutcome::Failed(error()),
        None if explicit_failure && status < 500 => ActionOutcome::Failed(error()),
        None => ActionOutcome::Indeterminate,
    })
}

/// Record an SDK-route action from its JSON answer (M3). `active_app` is the
/// active SDK connection read BEFORE dispatch, `scope` the request's `tabId`,
/// and `ipc_fallback` whether this route falls back to the runner's own
/// webview when no SDK app is connected; [`sdk_action_target`] decides where
/// it landed, and [`sdk_action_verdict`] what the answer says happened.
pub(crate) fn record_sdk_result(
    state: &Arc<crate::mcp::types::ApiState>,
    active_app: Option<(String, Option<String>)>,
    scope: Option<&str>,
    ipc_fallback: bool,
    response: &serde_json::Value,
    action: ActionSpec,
) {
    record_sdk_verdict(
        state,
        active_app,
        scope,
        ipc_fallback,
        response,
        sdk_action_verdict(response),
        action,
    );
}

/// [`record_sdk_result`] with the verdict decided by the caller — for a route
/// whose answer carries a verdict of its own beyond the envelope's `success`
/// (`/sdk/ai/recovery/attempt`'s `attemptSucceeded`). `None` records nothing.
pub(crate) fn record_sdk_verdict(
    state: &Arc<crate::mcp::types::ApiState>,
    active_app: Option<(String, Option<String>)>,
    scope: Option<&str>,
    ipc_fallback: bool,
    response: &serde_json::Value,
    verdict: Option<ActionOutcome>,
    action: ActionSpec,
) {
    let Some(outcome) = verdict else {
        return;
    };
    let Some((key, app_version)) = sdk_action_target(active_app, scope, ipc_fallback, response)
    else {
        return;
    };
    record_action(
        state.app_state.pg_db.clone(),
        key,
        action,
        Provenance {
            app_version,
            run_id: None,
        },
        outcome,
    );
}

/// `failure_origin` of a request the RUNNER rejected before touching any UI
/// on an SDK route's IPC fallback (e1).
pub(crate) const RUNNER_REJECTION_ORIGIN: &str = "runner";

/// Tag an IPC-fallback answer the runner produced with a 4xx as a runner
/// rejection (`failure_origin: "runner"`), so [`sdk_refusal`] records nothing
/// for it — exactly the control routes' 4xx rule. A 5xx (the action was
/// attempted and failed) and any 2xx are returned unchanged.
pub(crate) fn tag_runner_rejection(
    mut response: serde_json::Value,
    status: axum::http::StatusCode,
) -> serde_json::Value {
    if status.is_client_error() {
        if let Some(obj) = response.as_object_mut() {
            obj.insert(
                "failure_origin".to_string(),
                serde_json::Value::String(RUNNER_REJECTION_ORIGIN.to_string()),
            );
        }
    }
    response
}

/// The relay tab an SDK request is pinned to: `?tabId=` / `?targetTabId=`,
/// else the body's `tabId` / `targetTabId`.
pub(crate) fn sdk_request_scope(
    query: &std::collections::HashMap<String, String>,
    body: Option<&serde_json::Value>,
) -> Option<String> {
    query
        .get("tabId")
        .or_else(|| query.get("targetTabId"))
        .cloned()
        .or_else(|| {
            body.and_then(|b| b.get("tabId").or_else(|| b.get("targetTabId")))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .filter(|s| !s.trim().is_empty())
}

/// Is a snapshot request FILTERED (M2)? `visibleOnly`, `currentRouteOnly`,
/// `withDisabledOnly` (either spelling) set truthy, or any `recency`. A
/// filtered snapshot is a view of the page, not its configuration, so it
/// neither resolves a node nor closes a pending edge.
pub(crate) fn snapshot_query_is_filtered(
    query: &std::collections::HashMap<String, String>,
) -> bool {
    let truthy = |k: &str| {
        query.get(k).is_some_and(|v| {
            let s = v.trim();
            s == "1" || s.eq_ignore_ascii_case("true")
        })
    };
    [
        "visibleOnly",
        "currentRouteOnly",
        "withDisabledOnly",
        "with_disabled_only",
    ]
    .into_iter()
    .any(truthy)
        || query.contains_key("recency")
}

/// Refusal codes on EVERY SDK route: the answer came back without any UI being
/// reached — `TAB_NOT_FOUND` / `TAB_STALE`, the SDK relay could not route to a
/// tab.
pub(crate) const SDK_REFUSAL_CODES: [&str; 2] = ["TAB_NOT_FOUND", "TAB_STALE"];

/// `UB-VALIDATION-ERROR` — on `/sdk/ai/recovery/attempt` ONLY
/// ([`sdk_recovery_verdict`]), the app's own server refusing the request's
/// PAYLOAD before acting (ui-bridge `mapInternalErrorCode('VALIDATION_ERROR')`,
/// `attemptRecovery`'s 400 for an invalid `failure` / `instruction`): nothing
/// acted, exactly the control routes' 4xx rule. Not a refusal code for the
/// other SDK routes, whose validation-error answers are not established as
/// pre-action, so they keep their pre-existing reading.
pub(crate) const SDK_RECOVERY_VALIDATION_ERROR: &str = "UB-VALIDATION-ERROR";

/// Does `response` carry one of `codes` as `code` / `errorCode` at its top
/// level, under `error`, or under `data`?
fn carries_code(response: &serde_json::Value, codes: &[&str]) -> bool {
    let code_in = |v: Option<&serde_json::Value>| {
        v.into_iter().any(|v| {
            ["code", "errorCode"].iter().any(|k| {
                v.get(*k)
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| codes.contains(&c))
            })
        })
    };
    code_in(Some(response)) || code_in(response.get("error")) || code_in(response.get("data"))
}

/// Did an SDK route's answer come from a REFUSAL rather than an attempted
/// action (m3, N3)? Either the relay's `TAB_NOT_FOUND` / `TAB_STALE`
/// envelope — `code` or `errorCode` at the top level, under `error`, or under
/// `data` — or a TRANSPORT-origin dispatch refusal (`failure_origin:
/// "transport"`, from `ipc_fallback_refusal`): neither reached any UI, so,
/// like the control surface's 4xx rule, neither records. An APP-origin
/// failure (`failure_origin: "app"`) did reach the app and is an `error` edge.
/// A RUNNER-side request rejection on an IPC-fallback route
/// (`failure_origin: "runner"`, see [`tag_runner_rejection`]) never touched
/// the UI either and records nothing — the control routes' 4xx rule (e1).
pub(crate) fn sdk_refusal(response: &serde_json::Value) -> bool {
    // A verdict-unknown answer DID reach the app (the request was sent; only
    // its outcome is unknown), so it is never a refusal, whatever its origin.
    if verdict_unknown_code(response).is_some() {
        return false;
    }
    let refused_before_ui = matches!(
        response.get("failure_origin").and_then(|v| v.as_str()),
        Some(RUNNER_REJECTION_ORIGIN | "transport")
    );
    refused_before_ui || carries_code(response, &SDK_REFUSAL_CODES)
}

/// `task_run_id` (the run the action routes already attribute events to) as
/// the ledger's `run_id`.
pub(crate) fn run_id_from_task_run(task_run_id: Option<i64>) -> Option<String> {
    task_run_id.map(|id| id.to_string())
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

/// The edge INSERT and, in the same statement, the frontier change its
/// OUTCOME implies (M4) — one statement, so the two cannot disagree:
///
/// - `changed` / `no_change` / `settle_timeout`: the affordance was activated
///   from this node and something was observed after it, so it leaves the
///   frontier there (DELETE);
/// - `error`: the activation was attempted and failed — the row stays on the
///   frontier with `reason = activation_failed` (upserted if absent), never
///   deleted: a failed click has not explored anything;
/// - `to_node_unobserved`: where it led is unknown, so the frontier row is left
///   exactly as it is;
/// - `verdict_unknown`: whether the action ACTIVATED is the unknown, so the
///   frontier row is left exactly as it is too — neither cleared as explored
///   nor marked `activation_failed` (plan
///   `2026-11-18-the-journey-ledger-never-records-an-unknown-action-verdict-as-a-success-or-a-failure`
///   D1). Neither branch below names it, and a test pins that.
///
/// Casts: `$1::text::uuid` and `$2::text::timestamptz` so the row carries
/// exactly the id / close time the validated struct carries; the three JSONB
/// columns bind `serde_json::Value`s. `$10` (`to_node`) binds
/// `Option<serde_json::Value>`, and `None` is SQL NULL — NEVER a JSON `null`,
/// which the migration's `jsonb_typeof` CHECK rejects and which would satisfy
/// `NOT NULL`-style reasoning while naming nothing (see [`EdgeInsert`]).
/// Select-list binds carry explicit casts so Postgres never has to infer a
/// parameter type from an `INSERT … SELECT` target list.
///
/// Neither frontier branch matches anything when `$14` (the trigger's
/// fingerprint) is NULL.
///
/// `$17` (`timeline`, plan Phase 4) binds `Option<serde_json::Value>` with the
/// same SQL-NULL rule as `to_node`, and only ever a
/// `ChangeTimeline::for_ledger()` projection ([`edge_insert`]).
pub(crate) const EDGE_WRITE_SQL: &str = r#"WITH inserted AS (
    INSERT INTO project.journey_edge_observations
        (id, observed_at, app_id, app_version, runner_build_id, runner_instance,
         run_id, run_kind, from_node, to_node, trigger, outcome, timeline)
    VALUES ($1::text::uuid, $2::text::timestamptz, $3, $4, $5, $6,
            $7, $8, $9::jsonb, $10::jsonb, $11::jsonb, $12, $17::jsonb)
    RETURNING id
), cleared AS (
    DELETE FROM project.journey_frontier f
     WHERE f.app_id = $3
       AND f.node_key = $13
       AND f.affordance_fingerprint = $14
       AND $12 IN ('changed', 'no_change', 'settle_timeout')
       AND EXISTS (SELECT 1 FROM inserted)
    RETURNING 1
), failed AS (
    INSERT INTO project.journey_frontier
        (app_id, node_key, node, affordance_fingerprint, affordance_role,
         declared_effect, reason, last_seen_run_id)
    SELECT $3::text, $13::text, $9::jsonb, $14::text, $15::text,
           $16::text, 'activation_failed', $7::text
     WHERE $12 = 'error'
       AND $14 IS NOT NULL
       AND EXISTS (SELECT 1 FROM inserted)
    ON CONFLICT (app_id, node_key, affordance_fingerprint) DO UPDATE
       SET reason = 'activation_failed',
           last_seen_at = now(),
           last_seen_run_id = EXCLUDED.last_seen_run_id
    RETURNING 1
)
SELECT (SELECT count(*) FROM inserted) AS inserted,
       (SELECT count(*) FROM cleared) AS cleared,
       (SELECT count(*) FROM failed) AS failed"#;

/// Number of parameters [`EDGE_WRITE_SQL`] binds; sizes the call site's
/// params array so SQL/param arity drift is a compile error plus a test.
pub(crate) const EDGE_WRITE_BINDS: usize = 17;

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
    /// For the `error` branch's frontier upsert.
    pub target_role: Option<String>,
    /// For the `error` branch's frontier upsert (`read` / `write` /
    /// `destructive`; `None` = undeclared).
    pub declared_effect: Option<String>,
    /// The `timeline` column: `None` binds SQL NULL ("this action recorded no
    /// timeline"); `Some` is ALWAYS the [`ChangeTimeline::for_ledger`]
    /// projection, serialized through its typed mirror.
    pub timeline: Option<serde_json::Value>,
}

/// Validate a row against the contract and produce its binds.
///
/// Validation runs here, before every write: a failure is returned as the
/// contract error's text and counted as a write failure by the caller —
/// never a silent drop.
///
/// `timeline` is projected through [`ChangeTimeline::for_ledger`] HERE, at the
/// last step before the bind, whatever the caller passed — the projection is
/// idempotent, so the column can never receive a URL path, an event's prose
/// summary, or a source key outside `LEDGER_SOURCE_KEYS`.
pub(crate) fn edge_insert(
    obs: &JourneyEdgeObservation,
    timeline: Option<&ChangeTimeline>,
) -> Result<EdgeInsert, String> {
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
        target_role: obs.trigger.target_role.clone(),
        declared_effect: obs
            .trigger
            .declared_effect
            .map(|e| frontier::effect_str(e).to_string()),
        timeline: timeline
            .map(|t| serde_json::to_value(t.for_ledger()).map_err(|e| e.to_string()))
            .transpose()?,
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

/// The answer to "can the journey ledger be written here?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchemaProbe {
    /// Both tables exist and are readable.
    Present,
    /// At least one table does not exist. The names are the missing ones.
    Absent { missing: Vec<&'static str> },
    /// The probe itself failed (error or timeout). Writes are not attempted;
    /// health reports it as a failure, never as "absent" and never as
    /// "writing". NOT cached: the next call after [`PROBE_RETRY_BACKOFF`]
    /// probes again, so a transient database fault heals by itself.
    Failed { error: String },
}

/// How long a failed probe is reused before the next call probes again.
pub(crate) const PROBE_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(60);

/// What the process knows about the schema.
///
/// `Settled` holds only `Present` / `Absent` — the schema's shape, which does
/// not change under a running process without a migration. A probe FAILURE is
/// a fact about the database's health at one moment, so it is held only with
/// its time and expires after [`PROBE_RETRY_BACKOFF`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeCache {
    Unprobed,
    Settled(SchemaProbe),
    FailedAt {
        error: String,
        at: std::time::Instant,
    },
}

/// The answer the cache can give without probing, if any. Pure.
pub(crate) fn cached_answer(cache: &ProbeCache, now: std::time::Instant) -> Option<SchemaProbe> {
    match cache {
        ProbeCache::Unprobed => None,
        ProbeCache::Settled(answer) => Some(answer.clone()),
        ProbeCache::FailedAt { error, at } => (now.saturating_duration_since(*at)
            < PROBE_RETRY_BACKOFF)
            .then(|| SchemaProbe::Failed {
                error: error.clone(),
            }),
    }
}

/// What to remember after a probe answered `answer` at `now`. Pure.
pub(crate) fn cache_after(answer: &SchemaProbe, now: std::time::Instant) -> ProbeCache {
    match answer {
        SchemaProbe::Failed { error } => ProbeCache::FailedAt {
            error: error.clone(),
            at: now,
        },
        settled => ProbeCache::Settled(settled.clone()),
    }
}

/// Process-wide probe cache. Keyed on nothing, like
/// `OBSERVATION_APP_ID_SUPPORTED`: the runner constructs exactly one `PgDb`.
static JOURNEY_SCHEMA: std::sync::Mutex<ProbeCache> = std::sync::Mutex::new(ProbeCache::Unprobed);

/// Serializes probing, so concurrent callers wait for one probe instead of
/// each running their own.
static PROBE_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn probe_cache() -> std::sync::MutexGuard<'static, ProbeCache> {
    JOURNEY_SCHEMA.lock().unwrap_or_else(|p| p.into_inner())
}

/// The probe's most recent answer, without probing. `None` until a probe has
/// answered; a failure older than the backoff still reads as `Failed` here
/// (health reports it until a probe replaces it).
pub(crate) fn journey_schema_state() -> Option<SchemaProbe> {
    match &*probe_cache() {
        ProbeCache::Unprobed => None,
        ProbeCache::Settled(answer) => Some(answer.clone()),
        ProbeCache::FailedAt { error, .. } => Some(SchemaProbe::Failed {
            error: error.clone(),
        }),
    }
}

/// Resolve whether the journey tables are present: `Present` / `Absent` are
/// probed once per process; a `Failed` probe is retried after
/// [`PROBE_RETRY_BACKOFF`]. Warns on every probe that does not find the
/// tables — at most once per backoff window, since answers are cached.
pub(crate) async fn journey_schema_supported(client: &tokio_postgres::Client) -> SchemaProbe {
    // Each guard is a temporary of its own `let`, dropped before any await.
    let cached = cached_answer(&probe_cache(), std::time::Instant::now());
    if let Some(answer) = cached {
        return answer;
    }
    let _gate = PROBE_GATE.lock().await;
    let cached = cached_answer(&probe_cache(), std::time::Instant::now());
    if let Some(answer) = cached {
        return answer;
    }
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
            error: format!("journey schema probe did not answer within {JOURNEY_PROBE_TIMEOUT:?}"),
        },
    };
    *probe_cache() = cache_after(&answer, std::time::Instant::now());
    if answer != SchemaProbe::Present {
        warn!(
            "journey::capture: the journey ledger is NOT being written ({:?}). \
             GET /apps/<app_id>/journey/health reports the cause. A missing table needs \
             qontinui-web migration journey_01_edge_ledger (an existing embedded database never \
             receives new tables — plan Phase 0 decision 4); a failed probe is retried \
             automatically after {:?}.",
            answer, PROBE_RETRY_BACKOFF
        );
    }
    answer
}

// ---------------------------------------------------------------------------
// Events and the worker
// ---------------------------------------------------------------------------

/// One thing a handler saw. Carries only what the worker needs; a snapshot
/// is shared (`Arc`), never deep-copied, with the response and the
/// co-occurrence capture.
#[derive(Debug)]
pub(crate) enum JourneyEvent {
    /// An action (opens a pending edge). `hint` is the outcome it forces
    /// (`error` / `verdict_unknown` / `settle_timeout`), if any.
    Action {
        key: CursorKey,
        provenance: Provenance,
        action: ActionSpec,
        hint: Option<EdgeOutcome>,
        /// The action response's interaction timeline, already projected
        /// through [`ChangeTimeline::for_ledger`]; `None` = none recorded.
        timeline: Option<Box<ChangeTimeline>>,
    },
    /// A successful, UNFILTERED snapshot of the cursor's page (closes a
    /// pending edge). May be the SDK's `{success, data}` envelope; the worker
    /// reads through it ([`snapshot_view`]).
    Snapshot {
        key: CursorKey,
        app_version: Option<String>,
        snapshot: Arc<serde_json::Value>,
    },
    /// Close every pending edge older than the TTL (the retention tick).
    Sweep,
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

/// An action through a choke point, with its own three-state verdict:
/// [`verdict_hint`] maps a failure to the `error` hint and an unstated verdict
/// to `verdict_unknown` (plan D2).
pub(crate) fn record_action(
    pg_db: Arc<PgDb>,
    key: CursorKey,
    action: ActionSpec,
    provenance: Provenance,
    outcome: ActionOutcome,
) {
    enqueue_edge_observation(
        pg_db,
        JourneyEvent::Action {
            key,
            provenance,
            action,
            hint: verdict_hint(&outcome),
            timeline: None,
        },
    );
}

/// A successful, unfiltered snapshot.
pub(crate) fn record_snapshot(
    pg_db: Arc<PgDb>,
    key: CursorKey,
    app_version: Option<String>,
    snapshot: Arc<serde_json::Value>,
) {
    enqueue_edge_observation(
        pg_db,
        JourneyEvent::Snapshot {
            key,
            app_version,
            snapshot,
        },
    );
}

/// The outcome hint of an execute-with-diff response:
///
/// - `error` when the route failed (`failed`) or the diff result's
///   `actionSuccess` is present and not `true` (top level or under `data`);
/// - else `verdict_unknown` when `actionSuccess` is ABSENT (or `null`): the
///   route did not fail, but the diff never stated whether its action
///   succeeded, and an absent verdict is never a pass (plan
///   `2026-11-18-the-journey-ledger-never-records-an-unknown-action-verdict-as-a-success-or-a-failure`
///   D2);
/// - else `settle_timeout` when `settleTimedOut` is true;
/// - else none (the closing snapshot decides).
pub(crate) fn diff_outcome_hint(response: &serde_json::Value, failed: bool) -> Option<EdgeOutcome> {
    let field = |name: &str| {
        response
            .get(name)
            .or_else(|| response.get("data").and_then(|d| d.get(name)))
            .filter(|v| !v.is_null())
    };
    let action_success = field("actionSuccess");
    let action_failed = action_success.is_some_and(|v| v != &serde_json::Value::Bool(true));
    if failed || action_failed {
        return Some(EdgeOutcome::Error);
    }
    if action_success.is_none() {
        return Some(EdgeOutcome::VerdictUnknown);
    }
    (field("settleTimedOut").and_then(|v| v.as_bool()) == Some(true))
        .then_some(EdgeOutcome::SettleTimeout)
}

/// What an execute-with-diff response says about its interaction timeline.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DiffTimeline {
    /// No `timeline` key (the request sent `timeline: false`, or the producer
    /// predates the default-on timeline).
    Absent,
    /// A timeline, parsed and projected through
    /// [`ChangeTimeline::for_ledger`].
    Parsed(Box<ChangeTimeline>),
    /// A `timeline` key whose value is not a `ChangeTimeline` — counted on
    /// `/journey/health`, never stored.
    Unparseable(String),
}

/// The timeline of an execute-with-diff response, at the top level (the
/// relay body, an `ActionDiffResult`) or under `data` (the in-process
/// server's `success(result)` envelope, and the guarded failure shape). A
/// JSON `null` is [`DiffTimeline::Absent`].
pub(crate) fn diff_timeline(response: &serde_json::Value) -> DiffTimeline {
    let raw = response
        .get("timeline")
        .or_else(|| response.get("data").and_then(|d| d.get("timeline")))
        .filter(|v| !v.is_null());
    match raw {
        None => DiffTimeline::Absent,
        Some(v) => match serde_json::from_value::<ChangeTimeline>(v.clone()) {
            Ok(t) => DiffTimeline::Parsed(Box::new(t.for_ledger())),
            Err(e) => DiffTimeline::Unparseable(e.to_string()),
        },
    }
}

/// An execute-with-diff: a pending edge like every other action, carrying
/// the diff's outcome hint and its interaction timeline (plan Phase 4 — the
/// row this edge closes into stores `timeline.for_ledger()`).
pub(crate) fn record_diff(
    pg_db: Arc<PgDb>,
    key: CursorKey,
    request_body: &serde_json::Value,
    response: &serde_json::Value,
    provenance: Provenance,
    failed: bool,
) {
    let timeline = match diff_timeline(response) {
        DiffTimeline::Absent => None,
        DiffTimeline::Parsed(t) => Some(t),
        DiffTimeline::Unparseable(e) => {
            health::record_timeline_unparseable(e);
            None
        }
    };
    enqueue_edge_observation(
        pg_db,
        JourneyEvent::Action {
            key,
            provenance,
            action: ActionSpec::with_diff(request_body),
            hint: diff_outcome_hint(response, failed),
            timeline,
        },
    );
}

/// The snapshot inside a body: the SDK `{success, data}` envelope's `data`
/// when it is an object, else the body itself.
pub(crate) fn snapshot_view(body: &serde_json::Value) -> &serde_json::Value {
    match body.get("data") {
        Some(inner) if inner.is_object() => inner,
        _ => body,
    }
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
            hint,
            timeline,
        } => {
            if let Some(displaced) = cursors.open_with_timeline(
                &key,
                &action,
                provenance,
                hint,
                timeline,
                Instant::now(),
            ) {
                write_edge(pg, displaced).await;
            }
        }
        JourneyEvent::Snapshot {
            key,
            app_version,
            snapshot,
        } => {
            let node = resolve_node(pg, &key.app_id, app_version, Arc::clone(&snapshot)).await;
            let affordances = extract_affordances(snapshot_view(&snapshot));
            let observed = Observed {
                node: node.clone(),
                digest: affordance_digest(&affordances),
            };
            let closed = cursors.observe(&key, observed, affordances.clone(), Instant::now());
            let run_id = closed.as_ref().and_then(|d| d.provenance.run_id.clone());
            if let Some(draft) = closed {
                write_edge(pg, draft).await;
            }
            write_frontier(pg, &key.app_id, &node, &affordances, run_id).await;
        }
        JourneyEvent::Sweep => {
            for draft in cursors.sweep(Instant::now()) {
                write_edge(pg, draft).await;
            }
        }
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
/// spec matched to the snapshot's page against the snapshot in hand. Never
/// fetches a snapshot; runs only on the worker, and the CPU-bound parse and
/// evaluation run on a blocking thread over the shared snapshot (no copy).
async fn resolve_node(
    pg: &PgDb,
    app_id: &str,
    app_version: Option<String>,
    snapshot: Arc<serde_json::Value>,
) -> JourneyNode {
    let identity = page_identity(snapshot_view(&snapshot));
    let lookup = match identity.spec_lookup_label.clone() {
        None => SpecLookup::NoSpec,
        Some(page_id) => {
            match crate::spec_api::spec_check::load_page_spec(pg, app_id, &page_id).await {
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
                Ok(Some(loaded)) => {
                    let app_id = app_id.to_string();
                    let snap = Arc::clone(&snapshot);
                    let evaluated = tokio::task::spawn_blocking(move || {
                        evaluate_present_states(snapshot_view(&snap), &loaded, app_id, app_version)
                    })
                    .await;
                    match evaluated {
                        Ok(Some(present_state_ids)) => SpecLookup::Evaluated {
                            spec_id: page_id,
                            present_state_ids,
                        },
                        // Not a canonical control snapshot, or the blocking
                        // task failed: a spec exists but was not evaluated.
                        Ok(None) | Err(_) => SpecLookup::NotEvaluable { spec_id: page_id },
                    }
                }
            }
        }
    };
    build_node(&identity, &lookup)
}

/// Parse the snapshot (borrowing, not copying) and evaluate the loaded spec
/// against it. `None` when the snapshot is not the canonical control shape.
fn evaluate_present_states(
    snapshot: &serde_json::Value,
    loaded: &crate::spec_api::spec_check::LoadedPageSpec,
    app_id: String,
    app_version: Option<String>,
) -> Option<Vec<String>> {
    use serde::Deserialize;
    let parsed = qontinui_types::ui_bridge::UIBridgeSnapshot::deserialize(snapshot).ok()?;
    let fingerprint = qontinui_types::spec_check::BridgeFingerprint {
        app_id,
        app_version,
        route: None,
        bridge_version: None,
        snapshot_timestamp: String::new(),
        element_count: parsed.elements.len() as u32,
    };
    let result = crate::spec_api::spec_check::evaluate_loaded(&parsed, loaded, fingerprint);
    Some(present_state_ids(&result))
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

async fn write_edge(pg: &PgDb, mut draft: EdgeDraft) {
    let timeline = draft.timeline.take();
    let obs = finalize(draft);
    let binds = match edge_insert(&obs, timeline.as_deref()) {
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
        &binds.target_role,
        &binds.declared_effect,
        &binds.timeline,
    ];
    match conn.query_one(EDGE_WRITE_SQL, &params).await {
        Ok(row) => {
            let cleared = row.try_get::<_, i64>(1).unwrap_or(0);
            health::record_edge_written(cleared.max(0) as u64, binds.timeline.is_some());
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
        &batch.reasons,
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
    use crate::journey::cursor::resolve_trigger;
    use crate::journey::node::{Affordance, ElementAffordance};
    use qontinui_types::ir::IrEffect;
    use qontinui_types::journey::ChokePoint;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn key() -> CursorKey {
        CursorKey {
            app_id: "qontinui-web".into(),
            runner_instance: "primary".into(),
            scope: None,
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

    fn seen(spec: &str, states: &[&str]) -> Observed {
        Observed {
            node: node(spec, states),
            digest: "d".into(),
        }
    }

    fn click(id: &str) -> ActionSpec {
        ActionSpec::element(id, "click", ChokePoint::ElementAction)
    }

    fn affordances_with(id: &str, fp: &str) -> AffordanceIndex {
        let mut idx = AffordanceIndex::default();
        idx.elements.insert(
            id.to_string(),
            ElementAffordance {
                affordance: Affordance {
                    fingerprint: fp.to_string(),
                    role: Some("button".into()),
                    declared_effect: Some(IrEffect::Write),
                    navigation: false,
                },
                action_effects: BTreeMap::new(),
            },
        );
        idx
    }

    // ---- row construction -------------------------------------------------

    #[test]
    fn an_unobserved_edge_binds_sql_null_never_json_null() {
        let now = Instant::now();
        let mut c = Cursors::default();
        c.observe(&key(), seen("a", &["1"]), AffordanceIndex::default(), now);
        c.open(&key(), &click("x"), Provenance::default(), None, now);
        let displaced = c
            .open(&key(), &click("y"), Provenance::default(), None, now)
            .unwrap();
        let binds = edge_insert(&finalize(displaced), None).expect("a valid row");
        assert_eq!(binds.to_node, None, "to_node must bind SQL NULL");
        assert_ne!(binds.to_node, Some(serde_json::Value::Null));
        assert_eq!(binds.outcome, "to_node_unobserved");
    }

    #[test]
    fn an_observed_edge_binds_its_nodes_as_objects() {
        let now = Instant::now();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("a", &["1"]),
            affordances_with("x", "fp-x"),
            now,
        );
        c.open(&key(), &click("x"), Provenance::default(), None, now);
        let edge = c
            .observe(&key(), seen("b", &["2"]), AffordanceIndex::default(), now)
            .unwrap();
        let binds = edge_insert(&finalize(edge), None).unwrap();
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
            timeline: None,
        };
        let err = edge_insert(&finalize(draft), None).expect_err("validation must refuse it");
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
            "run_id, run_kind, from_node, to_node, trigger, outcome, timeline)",
            "$1::text::uuid",
            "$2::text::timestamptz",
            "$9::jsonb, $10::jsonb, $11::jsonb",
            "DELETE FROM project.journey_frontier",
            "f.affordance_fingerprint = $14",
            "$12 IN ('changed', 'no_change', 'settle_timeout')",
            "WHERE $12 = 'error'",
            "SET reason = 'activation_failed'",
        ] {
            assert!(EDGE_WRITE_SQL.contains(needle), "missing {needle:?}");
        }
        assert!(
            EDGE_WRITE_SQL.contains("$12, $17::jsonb)"),
            "the timeline column binds $17 as jsonb"
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
        assert_eq!(
            control_action_verdict(&failed),
            Some(ActionOutcome::Failed(Some("boom".to_string())))
        );
        let ok: Result<_, (StatusCode, Json<ApiResponse<()>>)> =
            Ok(Json(ApiResponse::success(serde_json::json!({}))));
        assert_eq!(control_action_verdict(&ok), Some(ActionOutcome::Succeeded));
    }

    // ---- plan 2026-11-18 D3: the control verdict is read from typed fields ---

    type ControlResult = Result<
        axum::Json<crate::mcp::types::ApiResponse<serde_json::Value>>,
        (
            axum::http::StatusCode,
            axum::Json<crate::mcp::types::ApiResponse<()>>,
        ),
    >;

    fn recovery_reply(data: serde_json::Value) -> ControlResult {
        crate::mcp::ui_bridge::ai_analyze::as_recovery_failure(Ok(axum::Json(
            crate::mcp::types::ApiResponse::success(data),
        )))
    }

    /// The 500 `/ai/recovery/attempt` answers for a reply that stated no
    /// verdict records `verdict_unknown`, never `error`.
    #[test]
    fn a_recovery_verdict_missing_500_is_verdict_unknown() {
        let result = recovery_reply(json!({ "strategiesAttempted": [] }));
        let (status, _) = result.as_ref().expect_err("a verdict-less reply is a 500");
        assert_eq!(*status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            control_action_verdict(&result),
            Some(ActionOutcome::Indeterminate)
        );
    }

    /// `RECOVERY_FAILED` — the attempt RAN and did not act — is a 400 whose
    /// `error_detail.code` is `ACTION_FAILED`: an `error` edge, not "the runner
    /// refused".
    #[test]
    fn a_recovery_failed_400_is_an_error_edge() {
        let result = recovery_reply(json!({ "attemptSucceeded": false }));
        let (status, _) = result.as_ref().expect_err("did-not-act is a 400");
        assert_eq!(*status, axum::http::StatusCode::BAD_REQUEST);
        assert!(matches!(
            control_action_verdict(&result),
            Some(ActionOutcome::Failed(_))
        ));
    }

    /// A recovery REFUSAL (`RECOVERY_UNSCOPED` → `INVALID_REQUEST`) ran
    /// nothing: no edge.
    #[test]
    fn a_recovery_refusal_400_records_no_edge() {
        let result = recovery_reply(json!({
            "success": false,
            "code": "RECOVERY_UNSCOPED",
            "error": "RECOVERY_UNSCOPED: ai_recovery_attempt requires params.elementId",
        }));
        assert!(result.is_err());
        assert_eq!(control_action_verdict(&result), None);
    }

    /// `execute_action`'s post-dispatch failures are re-coded `ACTION_FAILED`
    /// by `as_action_failure`: an `error` edge. Built through the real
    /// re-coder, so a change to the code it stamps reds here.
    #[test]
    fn an_action_failed_400_from_execute_action_is_an_error_edge() {
        use crate::mcp::types::ApiResponse;
        use axum::http::StatusCode;
        use axum::Json;
        let dispatched: ControlResult = Err((
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error("TERMINAL_EXITED: pane 3 exited")),
        ));
        let result = crate::mcp::ui_bridge::elements::as_action_failure(
            dispatched,
            "click",
            "btn",
            json!(null),
        );
        let (status, Json(body)) = result.as_ref().expect_err("still a 400");
        assert_eq!(*status, StatusCode::BAD_REQUEST);
        assert!(matches!(
            body.error_detail.as_ref().map(|d| &d.code),
            Some(UiBridgeErrorCode::ActionFailed)
        ));
        assert_eq!(
            control_action_verdict(&result),
            Some(ActionOutcome::Failed(Some(
                "TERMINAL_EXITED: pane 3 exited".to_string()
            )))
        );
    }

    /// An `ACTION_FAILED` the prose classifier assigned to a REFUSAL (its
    /// message merely contains "Operation failed") carries no re-coder stamp:
    /// no edge. The typed re-coders' `RECOVERY_FAILED` / `as_action_failure`
    /// 400s are covered above and still record `error`.
    #[test]
    fn an_unstamped_prose_action_failed_400_records_no_edge() {
        use crate::mcp::types::ApiResponse;
        use crate::mcp::ui_bridge::types::classify_transport_error;
        use axum::http::StatusCode;
        use axum::Json;
        let msg = "Operation failed: intent not registered";
        let detail = classify_transport_error(msg);
        assert!(
            matches!(detail.code, UiBridgeErrorCode::ActionFailed),
            "the classifier codes this prose ACTION_FAILED"
        );
        let refused: ControlResult = Err((
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error_detailed(msg.to_string(), detail)),
        ));
        assert_eq!(control_action_verdict(&refused), None);
        // The re-coded forms on the same status still record `error`.
        let recovery_failed = recovery_reply(json!({ "attemptSucceeded": false }));
        assert!(matches!(
            control_action_verdict(&recovery_failed),
            Some(ActionOutcome::Failed(_))
        ));
        let recoded = crate::mcp::ui_bridge::elements::as_action_failure(
            Err((
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::error_detailed(
                    msg.to_string(),
                    classify_transport_error(msg),
                )),
            )),
            "click",
            "btn",
            json!(null),
        );
        assert!(matches!(
            control_action_verdict(&recoded),
            Some(ActionOutcome::Failed(_))
        ));
    }

    /// The LIVE `/ai/recovery/attempt` path: `wrap_ipc_result` flattened the
    /// frontend's `RECOVERY_FAILED: <message>` into a 400 and the prose
    /// classifier read "action failed" in it as a context-less
    /// `ACTION_FAILED`. `as_recovery_failure`'s Err arm must still stamp the
    /// recovery token, so the attempt that RAN records an `error` edge.
    #[test]
    fn a_recovery_failed_400_on_the_err_arm_is_an_error_edge() {
        use crate::mcp::types::ApiResponse;
        use crate::mcp::ui_bridge::types::classify_transport_error;
        use axum::http::StatusCode;
        use axum::Json;
        let msg = "RECOVERY_FAILED: Click action failed";
        let detail = classify_transport_error(msg);
        assert!(matches!(detail.code, UiBridgeErrorCode::ActionFailed));
        assert!(detail.context.is_none());
        let result = crate::mcp::ui_bridge::ai_analyze::as_recovery_failure(Err((
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error_detailed(msg.to_string(), detail)),
        )));
        let (status, Json(body)) = result.as_ref().expect_err("still a 400");
        assert_eq!(*status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body.error_detail
                .as_ref()
                .and_then(|d| d.context.as_ref())
                .and_then(|c| c.get("code"))
                .and_then(|c| c.as_str()),
            Some("RECOVERY_FAILED")
        );
        assert!(matches!(
            control_action_verdict(&result),
            Some(ActionOutcome::Failed(_))
        ));
        // A recovery REFUSAL on the same arm still records nothing — including
        // an attempt whose executor dispatched nothing (its message carries
        // the executor's own "action failed" prose, which the classifier
        // matches).
        for refused in [
            "RECOVERY_UNSCOPED: requires elementId; Operation failed",
            "RECOVERY_NOT_DISPATCHED: UB-ACTION-FAILED: Click action failed",
            "RECOVERY_WRITE_REFUSED: recovery may not 'type'",
            // The executor's catch arm, a throw before any dispatch.
            "RECOVERY_NOT_DISPATCHED: network error",
        ] {
            let result = crate::mcp::ui_bridge::ai_analyze::as_recovery_failure(Err((
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::error_detailed(
                    refused.to_string(),
                    classify_transport_error(refused),
                )),
            )));
            assert_eq!(control_action_verdict(&result), None, "{refused}");
        }
    }

    /// Any other 4xx is still the runner refusing the request: no edge, even
    /// with a typed detail.
    #[test]
    fn another_typed_4xx_records_no_edge() {
        use crate::mcp::types::ApiResponse;
        use crate::mcp::ui_bridge::UiBridgeError;
        use axum::http::StatusCode;
        use axum::Json;
        let not_found: ControlResult = Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::error_detailed(
                "no such element".to_string(),
                UiBridgeError::element_not_found("btn"),
            )),
        ));
        assert_eq!(control_action_verdict(&not_found), None);
    }

    /// A verdict-unknown token decides wherever it appears in
    /// `error_detail.context.code`, whatever the status.
    #[test]
    fn a_verdict_unknown_token_wins_on_any_status() {
        use crate::mcp::types::ApiResponse;
        use crate::mcp::ui_bridge::UiBridgeError;
        use axum::http::StatusCode;
        use axum::Json;
        for status in [StatusCode::BAD_REQUEST, StatusCode::BAD_GATEWAY] {
            for token in VERDICT_UNKNOWN_CODES {
                let result: ControlResult = Err((
                    status,
                    Json(ApiResponse::<()>::error_detailed(
                        "x".to_string(),
                        UiBridgeError {
                            code: UiBridgeErrorCode::InternalError,
                            message: "x".to_string(),
                            recovery: None,
                            context: Some(json!({ "code": token })),
                        },
                    )),
                ));
                assert_eq!(
                    control_action_verdict(&result),
                    Some(ActionOutcome::Indeterminate),
                    "{status} {token}"
                );
            }
        }
    }

    /// A 2xx envelope whose `error` is the `INDETERMINATE:` marker reads as
    /// unknown, not as a failure; a batch handler's own verdict replaces the
    /// envelope on a 2xx and leaves errors to the usual reading.
    #[test]
    fn an_indeterminate_envelope_and_a_batch_verdict_are_verdict_unknown() {
        use crate::mcp::types::ApiResponse;
        use crate::mcp::ui_bridge::request::INDETERMINATE_ACTION_ERROR;
        use axum::http::StatusCode;
        use axum::Json;
        let mut body = ApiResponse::success(json!({}));
        body.success = false;
        body.error = Some(INDETERMINATE_ACTION_ERROR.to_string());
        let marker: ControlResult = Ok(Json(body));
        assert_eq!(
            control_action_verdict(&marker),
            Some(ActionOutcome::Indeterminate)
        );
        // The control batch answers `success: false` for a batch with an
        // indeterminate step; its folded verdict is what the ledger gets.
        let mut batch = ApiResponse::success(json!({ "results": [] }));
        batch.success = false;
        batch.error = Some("One or more batch steps failed".to_string());
        let batch: ControlResult = Ok(Json(batch));
        assert_eq!(
            control_action_verdict_with(&batch, ActionOutcome::Indeterminate),
            Some(ActionOutcome::Indeterminate)
        );
        let refused: ControlResult = Err((
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error("batch_size_exceeded")),
        ));
        assert_eq!(
            control_action_verdict_with(&refused, ActionOutcome::Succeeded),
            None
        );
    }

    // ---- plan 2026-11-18 D2: the SDK verdict --------------------------------

    #[test]
    fn a_verdictless_sdk_reply_is_verdict_unknown() {
        assert_eq!(
            sdk_action_verdict(&json!({ "data": { "elementId": "x" } })),
            Some(ActionOutcome::Indeterminate),
            "no `success` key is not a success"
        );
        assert_eq!(
            sdk_action_verdict(&json!({ "success": true })),
            Some(ActionOutcome::Succeeded)
        );
        assert_eq!(
            sdk_action_verdict(&json!({ "success": false, "error": "nope" })),
            Some(ActionOutcome::Failed(Some("nope".to_string())))
        );
        // A body normalise_action_result rewrote: its `false` is a marker.
        let (normalised, _) = crate::mcp::ui_bridge::request::normalize_action_result(
            json!({ "elementId": "x" }),
        );
        assert_eq!(
            sdk_action_verdict(&normalised),
            Some(ActionOutcome::Indeterminate)
        );
        // An SDK dispatch that failed after the request was sent.
        let after_send = json!({
            "success": false,
            "code": crate::mcp::sdk_client::SDK_DISPATCH_OUTCOME_UNKNOWN,
            "failure_origin": "transport",
        });
        assert_eq!(
            sdk_action_verdict(&after_send),
            Some(ActionOutcome::Indeterminate)
        );
        assert!(
            !sdk_refusal(&after_send),
            "a verdict-unknown answer reached the app: never a refusal"
        );
    }

    /// `UB-VALIDATION-ERROR` is a refusal on the recovery twin ONLY: there it
    /// records no edge; on every other SDK route it is read as before this
    /// rule existed — an app-origin `success: false`, an `error` edge.
    #[test]
    fn a_ub_validation_error_is_a_refusal_on_the_recovery_twin_only() {
        let refused = json!({
            "success": false,
            "code": "UB-VALIDATION-ERROR",
            "error": "failure must be an object",
            "failure_origin": "app",
        });
        assert_eq!(sdk_recovery_verdict(400, &refused), None);
        // Nested under `data` / `error` too, on the recovery twin.
        assert_eq!(
            sdk_recovery_verdict(
                400,
                &json!({ "success": false, "error": { "code": "UB-VALIDATION-ERROR" } })
            ),
            None
        );
        // Any other SDK route: not a refusal.
        assert!(!sdk_refusal(&refused));
        assert_eq!(
            sdk_action_verdict(&refused),
            Some(ActionOutcome::Failed(Some(
                "failure must be an object".to_string()
            )))
        );
        assert!(sdk_action_target(Some(("a".into(), None)), None, false, &refused).is_some());
        // The relay refusals still refuse everywhere.
        let tab = json!({ "success": false, "code": "TAB_STALE" });
        assert!(sdk_refusal(&tab));
        assert_eq!(sdk_action_verdict(&tab), None);
        assert_eq!(sdk_recovery_verdict(400, &tab), None);
    }

    /// The `INDETERMINATE:` reader matches the marker the normaliser writes.
    #[test]
    fn the_indeterminate_prefix_is_the_markers_prefix() {
        use crate::mcp::ui_bridge::request::INDETERMINATE_ACTION_ERROR;
        assert!(INDETERMINATE_ACTION_ERROR.starts_with(INDETERMINATE_PREFIX));
        assert!(indeterminate_marker(Some(INDETERMINATE_ACTION_ERROR)));
    }

    /// `/sdk/ai/recovery/attempt` reads the attempt's OWN verdict, the way the
    /// ui-bridge Python client's `read_recovery_attempt` does.
    #[test]
    fn the_sdk_recovery_verdict_is_the_attempts_own() {
        use ActionOutcome::*;
        // The TS server's success answer.
        assert_eq!(
            sdk_recovery_verdict(
                200,
                &json!({ "success": true, "data": { "attemptSucceeded": true } })
            ),
            Some(Succeeded)
        );
        // The TS server's not-recovered 400, data kept by the twin.
        assert!(matches!(
            sdk_recovery_verdict(
                400,
                &json!({ "success": false, "error": "RECOVERY_FAILED: exhausted",
                         "data": { "attemptSucceeded": false } })
            ),
            Some(Failed(_))
        ));
        // A pre-#426 server: envelope success, no verdict — never a success.
        assert_eq!(
            sdk_recovery_verdict(
                200,
                &json!({ "success": true, "data": { "recovered": false } })
            ),
            Some(Indeterminate)
        );
        // The TS server's fault arm after a strategy acted.
        assert_eq!(
            sdk_recovery_verdict(
                500,
                &json!({ "success": false, "data": { "attemptSucceeded": true } })
            ),
            Some(Indeterminate)
        );
        // A failure with no stated verdict: below 500 did-not-act, 5xx unknown.
        assert!(matches!(
            sdk_recovery_verdict(400, &json!({ "success": false, "error": "x" })),
            Some(Failed(_))
        ));
        assert_eq!(
            sdk_recovery_verdict(500, &json!({ "success": false, "error": "x" })),
            Some(Indeterminate)
        );
        // A dispatch error after the request was sent.
        assert_eq!(
            sdk_recovery_verdict(
                502,
                &json!({ "success": false,
                         "code": crate::mcp::sdk_client::SDK_DISPATCH_OUTCOME_UNKNOWN,
                         "failure_origin": "transport" })
            ),
            Some(Indeterminate)
        );
        // A request that never left the runner.
        assert_eq!(
            sdk_recovery_verdict(
                200,
                &json!({ "success": false, "code": "SDK_DISPATCH_FAILED",
                         "failure_origin": "transport" })
            ),
            None
        );
    }

    // ---- plan 2026-11-18 D1: the frontier is untouched by verdict_unknown ---

    /// A `verdict_unknown` edge neither clears its affordance as explored nor
    /// marks it `activation_failed`: whether it activated is the unknown. Both
    /// statements branch on outcomes by NAME, so naming `verdict_unknown` in
    /// either would change that.
    #[test]
    fn verdict_unknown_leaves_the_frontier_untouched() {
        assert!(
            !EDGE_WRITE_SQL.contains(EdgeOutcome::VerdictUnknown.as_str()),
            "EDGE_WRITE_SQL must not clear or fail an affordance on verdict_unknown"
        );
        assert!(
            !super::frontier::FRONTIER_UPSERT_SQL.contains(EdgeOutcome::VerdictUnknown.as_str()),
            "FRONTIER_UPSERT_SQL must not count verdict_unknown as explored"
        );
        assert_eq!(EdgeOutcome::VerdictUnknown.as_str(), "verdict_unknown");
        // The branches that DO act name their outcomes explicitly.
        assert!(EDGE_WRITE_SQL.contains("$12 IN ('changed', 'no_change', 'settle_timeout')"));
        assert!(EDGE_WRITE_SQL.contains("WHERE $12 = 'error'"));
    }

    // ---- N2 / N3: where an SDK action landed ---------------------------------

    #[test]
    fn with_no_app_connected_only_a_fallback_route_lands_on_the_runner() {
        let ok = json!({"success": true});
        let (key, version) = sdk_action_target(None, Some("t1"), true, &ok)
            .expect("the IPC fallback acted on the runner webview");
        assert_eq!(key.app_id, crate::spec_api::storage::RUNNER_APP_ID);
        assert_eq!(
            key.scope, None,
            "the runner's main window, not the relay tab"
        );
        assert_eq!(version, None);
        let failed = json!({"success": false, "error": "No active SDK app connection"});
        assert!(
            sdk_action_target(None, Some("t1"), false, &failed).is_none(),
            "a route with no fallback reached nothing: no edge"
        );
        assert!(sdk_action_target(None, None, false, &ok).is_none());
    }

    #[test]
    fn with_an_app_connected_the_fallback_flag_is_irrelevant() {
        for ipc_fallback in [true, false] {
            let (key, version) = sdk_action_target(
                Some(("qontinui-web".into(), Some("1.0".into()))),
                Some("t1"),
                ipc_fallback,
                &json!({"success": false, "error": "boom"}),
            )
            .unwrap();
            assert_eq!(
                (key.app_id.as_str(), key.scope.as_deref()),
                ("qontinui-web", Some("t1"))
            );
            assert_eq!(version.as_deref(), Some("1.0"));
        }
    }

    #[test]
    fn only_transport_and_tab_refusals_record_nothing() {
        let transport =
            json!({"success": false, "code": "SDK_DISPATCH_FAILED", "failure_origin": "transport"});
        let app = json!({"success": false, "code": "ELEMENT_DISABLED", "failure_origin": "app"});
        assert!(sdk_action_target(None, None, true, &transport).is_none());
        assert!(sdk_action_target(Some(("a".into(), None)), None, false, &transport).is_none());
        assert!(
            sdk_action_target(Some(("a".into(), None)), None, false, &app).is_some(),
            "an app-origin failure reached the app: an error edge"
        );
    }

    #[test]
    fn a_runner_rejection_on_a_fallback_route_records_nothing() {
        use axum::http::StatusCode;
        // e1, send-keys fallback: the keyboard handler's 4xx is a rejection…
        let rejected = tag_runner_rejection(
            json!({"success": false, "error": "unknown target"}),
            StatusCode::BAD_REQUEST,
        );
        assert!(sdk_refusal(&rejected));
        assert!(sdk_action_target(None, None, true, &rejected).is_none());
        // …while a 5xx was an attempt that failed: an error edge.
        let failed = tag_runner_rejection(
            json!({"success": false, "error": "ipc timeout"}),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
        assert!(!sdk_refusal(&failed));
        assert!(sdk_action_target(None, None, true, &failed).is_some());
        // e1, navigate fallback: a NotNavigable / UnroutedPage rejection
        // carries the runner origin, so it records nothing either.
        let not_navigable = json!({
            "success": false,
            "error": "not navigable",
            "failure_origin": RUNNER_REJECTION_ORIGIN,
            "error_detail": {"code": "INVALID_REQUEST"}
        });
        assert!(sdk_action_target(None, None, true, &not_navigable).is_none());
    }

    #[test]
    fn the_sdk_scope_comes_from_the_query_then_the_body() {
        let mut q = std::collections::HashMap::new();
        assert_eq!(
            sdk_request_scope(&q, Some(&json!({"targetTabId": "b"}))).as_deref(),
            Some("b")
        );
        q.insert("tabId".to_string(), "a".to_string());
        assert_eq!(
            sdk_request_scope(&q, Some(&json!({"tabId": "b"}))).as_deref(),
            Some("a")
        );
        assert_eq!(
            sdk_request_scope(&std::collections::HashMap::new(), None),
            None
        );
    }

    // ---- M2: filtered snapshots ----------------------------------------------

    #[test]
    fn a_filtered_snapshot_is_never_recorded() {
        let q = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<std::collections::HashMap<_, _>>()
        };
        assert!(!snapshot_query_is_filtered(&q(&[])));
        assert!(!snapshot_query_is_filtered(&q(&[
            ("visibleOnly", "false"),
            ("tabId", "t1")
        ])));
        assert!(snapshot_query_is_filtered(&q(&[("visibleOnly", "1")])));
        assert!(snapshot_query_is_filtered(&q(&[(
            "currentRouteOnly",
            "TRUE"
        )])));
        assert!(snapshot_query_is_filtered(&q(&[(
            "with_disabled_only",
            "true"
        )])));
        assert!(snapshot_query_is_filtered(&q(&[("recency", "5000")])));
    }

    // ---- m3: SDK refusals record nothing ------------------------------------

    #[test]
    fn a_relay_tab_refusal_is_not_an_action() {
        assert!(sdk_refusal(
            &json!({"success": false, "code": "TAB_NOT_FOUND"})
        ));
        assert!(sdk_refusal(
            &json!({"success": false, "error": {"code": "TAB_STALE"}})
        ));
        assert!(sdk_refusal(&json!({"data": {"errorCode": "TAB_STALE"}})));
        assert!(!sdk_refusal(
            &json!({"success": false, "error": "click missed"})
        ));
        assert!(!sdk_refusal(&json!({"success": true})));
    }

    // ---- diff outcome hint (M5, m4) -----------------------------------------

    #[test]
    fn diff_hint_follows_the_route_failure_rule() {
        let ok = json!({"actionSuccess": true, "settleTimedOut": false});
        assert_eq!(diff_outcome_hint(&ok, false), None);
        assert_eq!(
            diff_outcome_hint(&ok, true),
            Some(EdgeOutcome::Error),
            "success:false"
        );
        let action_failed = json!({"success": true, "data": {"actionSuccess": false}});
        assert_eq!(
            diff_outcome_hint(&action_failed, false),
            Some(EdgeOutcome::Error)
        );
        let settle = json!({"data": {"actionSuccess": true, "settleTimedOut": true}});
        assert_eq!(
            diff_outcome_hint(&settle, false),
            Some(EdgeOutcome::SettleTimeout)
        );
        let relay = json!({"actionResult": {}, "diff": {}});
        assert_eq!(
            diff_outcome_hint(&relay, false),
            Some(EdgeOutcome::VerdictUnknown),
            "an absent actionSuccess on a non-failed route is an unstated verdict, never a pass"
        );
        let null = json!({"data": {"actionSuccess": null, "settleTimedOut": true}});
        assert_eq!(
            diff_outcome_hint(&null, false),
            Some(EdgeOutcome::VerdictUnknown),
            "a null verdict is unstated too, and wins over the settle hint"
        );
        assert_eq!(
            diff_outcome_hint(&relay, true),
            Some(EdgeOutcome::Error),
            "a failed route stays an error"
        );
    }

    /// M5: ONE node-identity source. The same page, seen through the control
    /// snapshot route and through the SDK's `{success, data}` envelope,
    /// resolves to one key — and a diff-path action closes onto that key.
    #[test]
    fn the_same_page_resolves_to_one_key_regardless_of_path() {
        let page = json!({
            "activeTab": "settings",
            "page": {"pathname": "/", "route": {"pattern": "/settings"}},
            "elements": [{"id": "b", "label": "Save", "actions": ["click"]}]
        });
        let enveloped = json!({"success": true, "data": page.clone()});
        let control = build_node(&page_identity(snapshot_view(&page)), &SpecLookup::NoSpec);
        let sdk = build_node(
            &page_identity(snapshot_view(&enveloped)),
            &SpecLookup::NoSpec,
        );
        assert_eq!(control.key(), sdk.key());
        assert_eq!(
            affordance_digest(&extract_affordances(snapshot_view(&page))),
            affordance_digest(&extract_affordances(snapshot_view(&enveloped)))
        );

        let now = Instant::now();
        let mut c = Cursors::default();
        let observed = |n: &JourneyNode, v: &serde_json::Value| Observed {
            node: n.clone(),
            digest: affordance_digest(&extract_affordances(snapshot_view(v))),
        };
        c.observe(
            &key(),
            observed(&control, &page),
            extract_affordances(&page),
            now,
        );
        c.open(
            &key(),
            &ActionSpec::with_diff(&json!({"elementId": "b", "action": "click"})),
            Provenance::default(),
            diff_outcome_hint(&json!({"actionSuccess": true}), false),
            now,
        );
        let edge = c
            .observe(
                &key(),
                observed(&sdk, &enveloped),
                extract_affordances(snapshot_view(&enveloped)),
                now,
            )
            .unwrap();
        assert_eq!(edge.from_node.key(), edge.to_node.unwrap().key());
        assert_eq!(edge.outcome, EdgeOutcome::NoChange);
    }

    // ---- schema probe cache (M1) --------------------------------------------

    #[test]
    fn a_failed_probe_is_not_cached_past_the_backoff() {
        let t0 = std::time::Instant::now();
        let failed = SchemaProbe::Failed {
            error: "connection reset".into(),
        };
        let cache = cache_after(&failed, t0);
        assert_eq!(
            cached_answer(&cache, t0 + std::time::Duration::from_secs(1)),
            Some(failed.clone()),
            "inside the backoff the failure is reused (no probe storm)"
        );
        assert_eq!(
            cached_answer(&cache, t0 + PROBE_RETRY_BACKOFF),
            None,
            "after the backoff the next call probes again"
        );
    }

    #[test]
    fn present_and_absent_are_settled() {
        let t0 = std::time::Instant::now();
        let later = t0 + std::time::Duration::from_secs(86_400);
        for answer in [
            SchemaProbe::Present,
            SchemaProbe::Absent {
                missing: vec!["project.journey_frontier"],
            },
        ] {
            assert_eq!(
                cached_answer(&cache_after(&answer, t0), later),
                Some(answer)
            );
        }
        assert_eq!(cached_answer(&ProbeCache::Unprobed, t0), None);
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

        let now = Instant::now();
        let mut c = Cursors::default();
        let from = build_node(&page_identity(&before), &SpecLookup::NoSpec);
        let from_aff = extract_affordances(&before);
        c.observe(
            &key(),
            Observed {
                node: from.clone(),
                digest: affordance_digest(&from_aff),
            },
            from_aff.clone(),
            now,
        );
        c.open(&key(), &spec, Provenance::default(), None, now);
        let to = build_node(&page_identity(&after), &SpecLookup::NoSpec);
        let to_aff = extract_affordances(&after);
        let edge = c
            .observe(
                &key(),
                Observed {
                    node: to.clone(),
                    digest: affordance_digest(&to_aff),
                },
                to_aff.clone(),
                now,
            )
            .unwrap();

        let row = serde_json::to_string(&finalize(edge.clone())).unwrap();
        let binds = edge_insert(&finalize(edge), None).unwrap();
        let frontier_after = frontier::frontier_batch("qontinui-web", &to, &to_aff, None)
            .unwrap()
            .unwrap();
        let frontier_before = frontier::frontier_batch("qontinui-web", &from, &from_aff, None)
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

    // -- Phase 4: the stored timeline is the ledger projection --------------

    #[test]
    fn a_stored_timeline_is_the_ledger_projection_with_no_url_path_or_text() {
        use crate::journey::test_fixtures::{page_changed, timeline_json};
        let raw = timeline_json(
            vec![page_changed(
                40,
                "https://app.example/search/sentinel-7f3a?q=sentinel-7f3a",
            )],
            120,
        );
        // Both producers' shapes: the relay's top-level ActionDiffResult and
        // the in-process `{success, data}` envelope.
        for response in [
            serde_json::json!({ "actionSuccess": true, "timeline": raw.clone() }),
            serde_json::json!({ "success": true, "data": { "actionSuccess": true, "timeline": raw.clone() } }),
        ] {
            let DiffTimeline::Parsed(t) = diff_timeline(&response) else {
                panic!("a timeline must parse: {response}");
            };
            let mut c = Cursors::default();
            let now = Instant::now();
            c.open_with_timeline(
                &key(),
                &click("x"),
                Provenance::default(),
                None,
                Some(t),
                now,
            );
            let edge = c
                .observe(&key(), seen("b", &["2"]), AffordanceIndex::default(), now)
                .expect("closed");
            let timeline = edge.timeline.clone();
            let binds = edge_insert(&finalize(edge), timeline.as_deref()).unwrap();
            let stored = binds.timeline.expect("the column is written");
            let text = stored.to_string();
            assert!(
                !text.contains("sentinel-7f3a"),
                "a URL path or query leaked: {text}"
            );
            assert!(!text.contains("clicked Save"), "event prose leaked: {text}");
            assert!(!text.contains("/search"), "{text}");
            // It IS the projection, byte for byte.
            let parsed: qontinui_vision_core::ChangeTimeline =
                serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(stored, serde_json::to_value(parsed.for_ledger()).unwrap());
        }
    }

    #[test]
    fn an_absent_or_garbage_timeline_stores_null() {
        assert_eq!(
            diff_timeline(&serde_json::json!({"actionSuccess": true})),
            DiffTimeline::Absent
        );
        assert_eq!(
            diff_timeline(&serde_json::json!({"timeline": null})),
            DiffTimeline::Absent
        );
        assert!(matches!(
            diff_timeline(&serde_json::json!({"timeline": {"events": "nope"}})),
            DiffTimeline::Unparseable(_)
        ));
        let mut c = Cursors::default();
        let now = Instant::now();
        c.open(&key(), &click("x"), Provenance::default(), None, now);
        let edge = c
            .observe(&key(), seen("b", &["2"]), AffordanceIndex::default(), now)
            .expect("closed");
        assert!(edge.timeline.is_none());
        let binds = edge_insert(&finalize(edge), None).unwrap();
        assert!(binds.timeline.is_none(), "SQL NULL, never a JSON null");
    }
}
