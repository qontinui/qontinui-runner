//! AI analysis + semantic diff routes.
//!
//! Macro-generated handlers that proxy AI-side operations (analysis of
//! tabular/region/structured data, cross-app comparison, recovery attempts,
//! semantic search, semantic diff, pixel-accurate image diff)
//! to the webview SDK via `ui_bridge_request_sync`.
//!
//! Namespace note: most handlers sit under `/ai/*`; `image-diff` also has a
//! `/control/ai/image-diff` alias retained for backwards compatibility with
//! the `compareVisualRegression` caller path.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use tracing::warn;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;
use super::types::{recovery_hint_for, UiBridgeError, UiBridgeErrorCode};
use super::{ipc_handler_get, ipc_handler_post};

// Semantic search & diff
ipc_handler_post!(ui_bridge_ai_semantic_search_handler, "ai_semantic_search");
ipc_handler_get!(ui_bridge_ai_diff_handler, "ai_diff");

// AI analysis
ipc_handler_post!(ui_bridge_ai_analyze_data_handler, "ai_analyze_data");
ipc_handler_post!(ui_bridge_ai_analyze_regions_handler, "ai_analyze_regions");
ipc_handler_post!(
    ui_bridge_ai_analyze_structured_handler,
    "ai_analyze_structured_data"
);
ipc_handler_post!(
    ui_bridge_ai_analyze_cross_app_handler,
    "ai_analyze_cross_app"
);

/// Result shape shared by every UI Bridge HTTP handler in this module.
type BridgeResult =
    Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)>;

/// Default code for a recovery that ran and did not recover, when the payload
/// carried no machine-readable one of its own. Mirrors `RECOVERY_FAILED` in
/// `src/hooks/ui-bridge-events/recoveryScope.ts`.
const RECOVERY_FAILED: &str = "RECOVERY_FAILED";

/// Code for a 2xx `ai_recovery_attempt` reply that states NO verdict: its
/// [`ATTEMPT_SUCCEEDED`] is absent, `null` or not a boolean
/// ([`AttemptVerdict::Unstated`]). The runner mints it; no frontend arm sends
/// it, because every arm writes the field. Seeing it means the producer did not
/// answer the question (an older bundle, an early return, a future arm, or a
/// success envelope with no `data`), so whether the scoped attempt acted is
/// unknown. See [`verdict_missing_failure`].
///
/// `pub(crate)` because the journey ledger reads it: it is a member of
/// [`crate::journey::capture::VERDICT_UNKNOWN_CODES`], so this 500 records a
/// `verdict_unknown` edge — never `error`, which would assert a failure the
/// runner cannot prove.
pub(crate) const RECOVERY_VERDICT_MISSING: &str = "RECOVERY_VERDICT_MISSING";

/// Read the leading `CODE:` token out of a typed refusal message.
///
/// The frontend's refusals are prefixed with their machine-readable code
/// (`"RECOVERY_UNSCOPED: ai_recovery_attempt requires params.elementId — …"`),
/// which is the one part of the sentence a client can branch on. Returns
/// `None` for anything that is not a SCREAMING_SNAKE_CASE token followed by a
/// colon, so ordinary prose is never mistaken for a code.
pub(crate) fn recovery_code_from_message(message: &str) -> Option<String> {
    let head = message.split(':').next()?.trim();
    if head.is_empty() || head.len() > 64 {
        return None;
    }
    if head
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        Some(head.to_string())
    } else {
        None
    }
}

/// Map a `RECOVERY_*` token onto the envelope's TYPED error taxonomy.
///
/// The token is the finest-grained answer and is preserved verbatim — in
/// `code` at the top level and in `error_detail.context.code`. This is the
/// coarse classification a client that only understands [`UiBridgeErrorCode`]
/// branches on. Without it every refusal read as `INTERNAL_ERROR`: the
/// classifier's fallthrough ([`super::types::classify_transport_error`]) has
/// no pattern for these messages, so it asserted a RUNNER DEFECT for what are
/// ordinary, expected, caller-caused refusals.
fn recovery_error_code(code: &str) -> UiBridgeErrorCode {
    match code {
        // Request-shape refusals — the caller asked recovery to do something
        // it is not allowed to do (guess a target, leave the addressed
        // element, write into input state). Nothing is wrong with the runner.
        "RECOVERY_UNSCOPED" | "RECOVERY_OUT_OF_SCOPE" | "RECOVERY_WRITE_REFUSED" => {
            UiBridgeErrorCode::InvalidRequest
        }
        // The addressed element is not in the tree — the very condition the
        // element routes already report as `ELEMENT_NOT_FOUND`.
        "RECOVERY_TARGET_MISSING" => UiBridgeErrorCode::ElementNotFound,
        // The reply stated no verdict, so whether the attempt acted is
        // UNKNOWN. This arm keeps the token out of the fallthrough below,
        // which says the attempt RAN. Under envelope rule 2a this code becomes
        // the wire's top-level `code`, so without the arm the token would reach
        // a caller as `ACTION_FAILED`, the same answer as `RECOVERY_FAILED`.
        // `INTERNAL_ERROR` is the true class (the reply came from the runner's
        // own frontend bundle), and its `UNRECOVERABLE` hint tells a caller not
        // to re-run an attempt whose effect is unknown.
        RECOVERY_VERDICT_MISSING => UiBridgeErrorCode::InternalError,
        // `RECOVERY_FAILED` and any future token: recovery RAN against the
        // addressed element and did not recover it. That is an action failure.
        _ => UiBridgeErrorCode::ActionFailed,
    }
}

/// Build the typed `error_detail` for a recovery refusal, preserving whatever
/// context the upstream detail carried and stamping the token into
/// `context.code` so nothing is lost by the coarse mapping.
fn recovery_error_detail(
    code: &str,
    message: String,
    prior_context: Option<serde_json::Value>,
) -> UiBridgeError {
    let mapped = recovery_error_code(code);
    let recovery = Some(recovery_hint_for(&mapped));
    let mut obj = match prior_context {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "code".to_string(),
        serde_json::Value::String(code.to_string()),
    );
    UiBridgeError {
        code: mapped,
        message,
        recovery,
        context: Some(serde_json::Value::Object(obj)),
    }
}

/// The frontend's scoped-attempt verdict key in `ai_recovery_attempt`'s `data`
/// (`RecoveryVerdict.attemptSucceeded` in
/// `src/hooks/ui-bridge-events/recoveryScope.ts`). Distinct from the runner's
/// outer `RecoveryOutcome.recovered` on purpose — see [`as_recovery_failure`].
///
/// The field has two readers: [`as_recovery_failure`] here, which decides the
/// HTTP answer of `/ai/recovery/attempt`, and
/// `recovery_executor::scoped_attempt_verdict`, which decides whether the LLM
/// fallback retries the original action. Both read it strictly, through the
/// one function [`attempt_verdict`], so they cannot disagree about a missing
/// value. Only `true` counts as success. An absent, `null` or non-boolean value
/// is [`AttemptVerdict::Unstated`], never a pass.
pub(crate) const ATTEMPT_SUCCEEDED: &str = "attemptSucceeded";

/// What a reply to the scoped recovery attempt (`ai_recovery_attempt`) says
/// about whether the attempt acted on the addressed element. Three states, not
/// two, because a missing verdict is not a `true` one; the same pattern as
/// [`super::request::ActionOutcome`] for an action's `success`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptVerdict {
    /// [`ATTEMPT_SUCCEEDED`] is `true`: the scoped executor acted on the
    /// addressed element. This says nothing about whether the ORIGINAL action
    /// now succeeds; the retry answers that.
    Acted,
    /// [`ATTEMPT_SUCCEEDED`] is `false`, or the reply carries an explicit
    /// `success: false` (a refusal the dispatcher stamped onto `data`),
    /// whatever else it says.
    DidNotAct,
    /// The reply states no verdict: [`ATTEMPT_SUCCEEDED`] is absent, `null` or
    /// not a boolean. Whether the attempt acted is unknown, so this is never a
    /// success.
    Unstated,
}

/// Read a scoped-attempt reply's verdict. This is the one reader of
/// [`ATTEMPT_SUCCEEDED`]; both of its consumers call it (see that constant).
///
/// An explicit `success: false` is [`AttemptVerdict::DidNotAct`] whatever else
/// the reply says. Otherwise `attemptSucceeded: true` is
/// [`AttemptVerdict::Acted`], `false` is [`AttemptVerdict::DidNotAct`], and
/// anything else is [`AttemptVerdict::Unstated`]: a missing key, `null`, a
/// string, a number, or a reply that is not an object at all.
///
/// Deliberately NOT [`super::request::action_outcome`]. The frontend's success
/// reply carries no `success` key, because the dispatcher forwards the bare
/// `data`. Reading `success` strictly would call every successful attempt
/// indeterminate.
pub(crate) fn attempt_verdict(reply: &serde_json::Value) -> AttemptVerdict {
    if reply.get("success").and_then(|v| v.as_bool()) == Some(false) {
        return AttemptVerdict::DidNotAct;
    }
    match reply.get(ATTEMPT_SUCCEEDED).and_then(|v| v.as_bool()) {
        Some(true) => AttemptVerdict::Acted,
        Some(false) => AttemptVerdict::DidNotAct,
        None => AttemptVerdict::Unstated,
    }
}

/// The maximum number of a verdict-less reply's top-level key names that
/// [`verdict_missing_failure`] reports. Enough to identify the producer.
const PAYLOAD_KEYS_LIMIT: usize = 32;

/// The maximum length, in characters, of each reported key name. With
/// [`PAYLOAD_KEYS_LIMIT`] this bounds the reported names to 2,048 characters
/// (at most 8 KiB of UTF-8), so an arbitrary payload cannot inflate the error
/// body or the log line (and cannot push the body past the envelope
/// middleware's JSON pass, which would change the wire shape).
const PAYLOAD_KEY_CHARS_LIMIT: usize = 64;

/// The HTTP 500 for a 2xx reply that stated no verdict
/// ([`AttemptVerdict::Unstated`]). `payload` is the reply's `data`, or `None`
/// when the envelope carried none.
///
/// The handler sets `code: RECOVERY_VERDICT_MISSING` and `error_detail.code:
/// INTERNAL_ERROR` (through [`recovery_error_code`]), with the token in
/// `error_detail.context.code`. The handler's two codes disagree, so the
/// envelope middleware's rule 2a (`crate::mcp::envelope`) rewrites the
/// top-level `code` to `INTERNAL_ERROR`: on the wire both codes read
/// `INTERNAL_ERROR`, and the token stays in `error_detail.context.code` and
/// `error_detail.context.displaced_top_level_code`. A caller therefore keys on
/// `error_detail.context.code`. The context also describes the reply's shape,
/// because a reply that does not answer the question is identified by what it
/// did carry:
///
/// - `payload_type`: its JSON type, or `absent`;
/// - `payload_keys`: up to [`PAYLOAD_KEYS_LIMIT`] of its top-level key names,
///   each cut to [`PAYLOAD_KEY_CHARS_LIMIT`] characters;
/// - `payload_key_count`: how many keys it had, so a cut list shows the cut.
///
/// Only key names are reported, never values.
fn verdict_missing_failure(
    payload: Option<&serde_json::Value>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    let payload_type = match payload {
        None => "absent",
        Some(serde_json::Value::Null) => "null",
        Some(serde_json::Value::Bool(_)) => "boolean",
        Some(serde_json::Value::Number(_)) => "number",
        Some(serde_json::Value::String(_)) => "string",
        Some(serde_json::Value::Array(_)) => "array",
        Some(serde_json::Value::Object(_)) => "object",
    };
    let keys: Vec<&str> = payload
        .and_then(serde_json::Value::as_object)
        .map(|obj| obj.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let shown: Vec<String> = keys
        .iter()
        .take(PAYLOAD_KEYS_LIMIT)
        .map(|k| k.chars().take(PAYLOAD_KEY_CHARS_LIMIT).collect())
        .collect();
    let message = format!(
        "{RECOVERY_VERDICT_MISSING}: the frontend's ai_recovery_attempt reply carried no \
         boolean `{ATTEMPT_SUCCEEDED}`; whether the scoped attempt acted is unknown"
    );
    warn!(
        payload_type,
        payload_key_count = keys.len(),
        payload_keys = ?shown,
        "ui_bridge /ai/recovery/attempt: {RECOVERY_VERDICT_MISSING}: a 2xx reply carried \
         no boolean {ATTEMPT_SUCCEEDED}; answering HTTP 500, not success"
    );
    let context = serde_json::json!({
        "payload_type": payload_type,
        "payload_keys": shown,
        "payload_key_count": keys.len(),
    });
    let mut body = api_error(message.clone());
    body.error_detail = Some(recovery_error_detail(
        RECOVERY_VERDICT_MISSING,
        message,
        Some(context),
    ));
    body.code = Some(RECOVERY_VERDICT_MISSING.to_string());
    (StatusCode::INTERNAL_SERVER_ERROR, Json(body))
}

/// Stop `/ai/recovery/attempt` from laundering a typed refusal into a success.
///
/// THE DEFECT: the frontend answered `RECOVERY_UNSCOPED` /
/// `RECOVERY_TARGET_MISSING` with `{success: false, error: "<CODE>: …",
/// data: {recovered: false}}` (the field is `attemptSucceeded` today), but the
/// response dispatcher (`request::handle_ui_bridge_response`) forwarded ONLY
/// `response.data` when the handler supplied one — so `wrap_ipc_result`
/// received a bare `{recovered: false}` with no `success: false` to flatten and
/// answered
/// **HTTP 200 `{"success":true,"recovered":false}`**. A caller that refused to
/// guess a target was indistinguishable from one that recovered.
///
/// That dispatcher hole is now closed at the seam itself:
/// [`super::request::extract_response_data`] carries the envelope's
/// `success: false` (and `error`/`code`/`hint`) across the extraction for every
/// handler, so `recoveryFailureData` no longer has to mirror the refusal into
/// `data` by hand and no longer takes a `message` to mirror.
///
/// This function is still load-bearing, for the cases the seam cannot see. It
/// reads a 2xx reply's verdict through [`attempt_verdict`], the same reader
/// `recovery_executor::scoped_attempt_verdict` uses, and handles every arm:
///
/// - [`AttemptVerdict::Acted`] (`attemptSucceeded: true`) passes through
///   untouched as the 200.
/// - [`AttemptVerdict::DidNotAct`] becomes an HTTP 400 carrying the payload's
///   own `error` text and a machine-readable `code`. The case is a frontend
///   that answers `{success: true, data: {attemptSucceeded: false}}`, an HONEST
///   envelope reporting that the scoped attempt ran and did not act on the
///   addressed element. It is defensive, a case the seam cannot see: today's
///   bundle sends every did-not-act reply with envelope `success: false`, which
///   `wrap_ipc_result` turns into a 400 before this arm runs. Turning that verdict into a 400 is this
///   boundary's own job, exactly as [`super::elements::as_action_failure`] does
///   for `execute_action`.
/// - [`AttemptVerdict::Unstated`] becomes an HTTP 500 whose
///   `error_detail.code` is `INTERNAL_ERROR` and whose
///   `error_detail.context.code` is `RECOVERY_VERDICT_MISSING`
///   ([`verdict_missing_failure`]; on the wire the top-level `code` is
///   `INTERNAL_ERROR` too, by envelope rule 2a). This is a 2xx whose `attemptSucceeded` is
///   absent, `null` or not a boolean, or that carries no `data` at all: the
///   producer did not say whether the attempt acted. Passing it through would
///   read an absence as a pass, which is what this function did until plan
///   `2026-11-10-the-recovery-attempt-route-never-reads-a-missing-verdict-as-success`.
///   `RECOVERY_FAILED` would be wrong too, because it says the attempt ran.
///   The status is a 5xx because the caller's request was fine. The journey
///   ledger records a `verdict_unknown` edge for it (the token is in
///   [`crate::journey::capture::VERDICT_UNKNOWN_CODES`], plan
///   `2026-11-18-the-journey-ledger-never-records-an-unknown-action-verdict-as-a-success-or-a-failure`
///   D3): neither the `error` it used to record, which asserted a failure the
///   runner cannot prove, nor no edge, which would erase an attempt that may
///   have acted. It is 500 rather than 502 because the code is
///   `INTERNAL_ERROR`, and the status and the code must name one event.
///
/// On the error side, a 400 that arrived without a `code` gets one derived from
/// its message. Transport-level failures (500 / 503: the frontend never
/// answered) pass through untouched, because they are not attempt verdicts.
///
/// **The HTTP status of `/ai/recovery/attempt` therefore reflects the SCOPED
/// ATTEMPT, not the ORIGINAL action.** A 200 means the scoped executor acted on
/// the addressed element; it does NOT mean the action that originally failed
/// now succeeds. That stricter question is
/// [`super::recovery_executor::RecoveryOutcome::recovered`], answered by an
/// independent retry of the original action — and the two legitimately
/// disagree (the right element was clicked, the original action is still
/// broken: 200 here, `recovered: false` there). The frontend's field was named
/// `recovered` until plan `2026-08-23-single-source-derived-facts` item 8,
/// which is exactly how one word came to answer two questions.
///
/// Every failure arm also builds a TYPED `error_detail` ([`recovery_error_code`]).
/// Stamping only the top-level `code` left the token laundered one layer down:
/// `api_error` leaves `error_detail: None`, and `classify_transport_error` has
/// no pattern for `RECOVERY_*: …`, so the structured half of the very same body
/// said `INTERNAL_ERROR` — a runner defect — for an ordinary caller-caused
/// refusal.
pub(crate) fn as_recovery_failure(result: BridgeResult) -> BridgeResult {
    let code_of = |payload: &serde_json::Value, fallback: Option<&str>| -> String {
        payload
            .get("code")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                payload
                    .get("error")
                    .and_then(|v| v.as_str())
                    .and_then(recovery_code_from_message)
            })
            .or_else(|| fallback.and_then(recovery_code_from_message))
            .unwrap_or_else(|| RECOVERY_FAILED.to_string())
    };

    match result {
        Ok(Json(resp)) => {
            // A success envelope with no `data` at all states no verdict
            // either, so it is read as a `null` reply.
            let verdict = attempt_verdict(resp.data.as_ref().unwrap_or(&serde_json::Value::Null));
            match verdict {
                AttemptVerdict::Acted => return Ok(Json(resp)),
                AttemptVerdict::Unstated => {
                    return Err(verdict_missing_failure(resp.data.as_ref()));
                }
                // The attempt ran and did not act: the 400 below.
                AttemptVerdict::DidNotAct => {}
            }
            let payload = resp.data.clone().unwrap_or(serde_json::Value::Null);
            let code = code_of(&payload, None);
            let message = payload
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    format!("{code}: recovery did not recover the addressed element")
                });
            let mut body = api_error(message.clone());
            // `api_error` leaves `error_detail: None`, which is exactly how the
            // typed token used to get laundered one layer down: a later
            // classifier filled the empty slot with its `InternalError`
            // fallthrough, so the body said `RECOVERY_UNSCOPED` at the top and
            // `INTERNAL_ERROR` inside. Build the detail HERE, the way
            // `elements::as_action_failure` does.
            body.error_detail = Some(recovery_error_detail(&code, message, None));
            body.code = Some(code);
            Err((StatusCode::BAD_REQUEST, Json(body)))
        }
        Err((status, Json(mut body))) => {
            if status == StatusCode::BAD_REQUEST {
                let code = body
                    .code
                    .clone()
                    .unwrap_or_else(|| code_of(&serde_json::Value::Null, body.error.as_deref()));
                // The live path: `wrap_ipc_result` already flattened the
                // frontend's `success:false` payload into a 400 and ran
                // `classify_transport_error` over the refusal prose, which has
                // no pattern for `RECOVERY_*: …` and so produced
                // `InternalError`. Re-code ONLY that unclassified case — the
                // same rule `as_action_failure` applies — because a detail the
                // classifier genuinely recognised is a better answer than the
                // token's coarse mapping.
                let unclassified = match &body.error_detail {
                    None => true,
                    Some(d) => matches!(d.code, UiBridgeErrorCode::InternalError),
                };
                if unclassified {
                    let prior = body.error_detail.take();
                    let message = prior
                        .as_ref()
                        .map(|d| d.message.clone())
                        .or_else(|| body.error.clone())
                        .unwrap_or_else(|| {
                            format!("{code}: recovery did not recover the addressed element")
                        });
                    body.error_detail = Some(recovery_error_detail(
                        &code,
                        message,
                        prior.and_then(|d| d.context),
                    ));
                }
                body.code = Some(code);
            }
            Err((status, Json(body)))
        }
    }
}

/// The answer of `/ai/recovery/attempt`: its failure body may carry `data`
/// (the stated verdict and the original-action facts, see
/// [`recovery_route_answer`]).
type RecoveryRouteResult = Result<
    Json<ApiResponse<serde_json::Value>>,
    (StatusCode, Json<ApiResponse<serde_json::Value>>),
>;

/// The executor's statements about the ORIGINAL action, in its reply's data:
/// whether the attempt ran the original instruction, and whether that run
/// reported success. The wire names the ui-bridge TS SDK server's
/// `attemptRecovery` uses, so a client reads both servers the same way.
const ORIGINAL_ACTION_FACTS: [&str; 2] = ["originalActionAttempted", "originalActionPerformed"];

/// The original-action facts a reply's payload STATES: each one only when it
/// is a boolean. An absent or non-boolean fact is left out, which a reader
/// takes as UNKNOWN (the ui-bridge Python client never re-executes the
/// original on an unknown) — never defaulted to `false`, which would license a
/// second execution nothing established.
fn original_action_facts(
    payload: Option<&serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    ORIGINAL_ACTION_FACTS
        .iter()
        .filter_map(|key| {
            payload
                .and_then(|p| p.get(*key))
                .filter(|v| v.is_boolean())
                .map(|v| (key.to_string(), v.clone()))
        })
        .collect()
}

/// Put [`as_recovery_failure`]'s answer on the wire with the scoped
/// executor's original-action facts kept (plan
/// `2026-11-18-the-journey-ledger-never-records-an-unknown-action-verdict-as-a-success-or-a-failure`,
/// Phase 3).
///
/// `result` is the frontend reply through
/// [`super::request::wrap_ipc_result_keeping_failure_data`], so a failure
/// still carries its payload. [`as_recovery_failure`] decides the status and
/// the typed codes exactly as before. Then:
///
/// - a 200 passes through whole: its `data` is the executor's own, facts
///   included;
/// - a 4xx (the attempt ran and did not act, or was refused) gets `data:
///   {attemptSucceeded: false, ...facts}`. That is the TS server's
///   `recoveryNotSucceeded` shape, so a client that branches on
///   `originalActionAttempted` (the original ran and failed, so it must not be
///   re-run) reads it from either server;
/// - a 5xx stays without `data`: its verdict is unknown, and a fact stated
///   beside an unknown verdict would read as a verdict.
fn recovery_route_answer(
    result: Result<
        Json<ApiResponse<serde_json::Value>>,
        (StatusCode, Json<ApiResponse<serde_json::Value>>),
    >,
) -> RecoveryRouteResult {
    let facts = match &result {
        Ok(Json(resp)) => original_action_facts(resp.data.as_ref()),
        Err((_, Json(body))) => original_action_facts(body.data.as_ref()),
    };
    let flat: BridgeResult = result.map_err(|(status, Json(body))| {
        (
            status,
            Json(ApiResponse {
                success: body.success,
                data: None,
                error: body.error,
                error_detail: body.error_detail,
                hint: body.hint,
                code: body.code,
                suggestions: body.suggestions,
            }),
        )
    });
    as_recovery_failure(flat).map_err(|(status, Json(body))| {
        let data = status.is_client_error().then(|| {
            let mut data = facts;
            data.insert(
                ATTEMPT_SUCCEEDED.to_string(),
                serde_json::Value::Bool(false),
            );
            serde_json::Value::Object(data)
        });
        (
            status,
            Json(ApiResponse {
                success: body.success,
                data,
                error: body.error,
                error_detail: body.error_detail,
                hint: body.hint,
                code: body.code,
                suggestions: body.suggestions,
            }),
        )
    })
}

/// Journey ledger choke point (plan
/// 2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time, M3):
/// `/ai/recovery/attempt` re-attempts a failed action: ONE `batch_action` (`recovery_attempt`).
/// The verdict is [`crate::journey::capture::control_action_verdict`]'s: a 200
/// is a success edge, the 400 `RECOVERY_FAILED` (`ACTION_FAILED`) an `error`
/// edge, the 500 [`RECOVERY_VERDICT_MISSING`] a `verdict_unknown` edge, and a
/// refusal 400 no edge.
pub async fn ui_bridge_ai_recovery_attempt_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> RecoveryRouteResult {
    let action = crate::journey::cursor::ActionSpec::batch_kind("recovery_attempt", None);
    let result =
        ui_bridge_ai_recovery_attempt_handler_dispatch(State(Arc::clone(&state)), Json(body)).await;
    crate::journey::capture::record_control_result(&state, &result, action);
    result
}

/// `POST /ui-bridge/ai/recovery/attempt` — hand-written rather than
/// macro-generated so the response passes through [`as_recovery_failure`]
/// (via [`recovery_route_answer`]).
async fn ui_bridge_ai_recovery_attempt_handler_dispatch(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> RecoveryRouteResult {
    let payload = serde_json::json!({ "params": body });
    recovery_route_answer(
        crate::mcp::ui_bridge::request::wrap_ipc_result_keeping_failure_data(
            ui_bridge_request_sync(&state, "ai_recovery_attempt", payload).await,
        ),
    )
}

// Pixel-accurate image diff (compareVisualRegression alias)
ipc_handler_post!(ui_bridge_image_diff_handler, "image_diff");

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // AI semantic search & diff
        .route(
            "/ui-bridge/ai/semantic-search",
            post(ui_bridge_ai_semantic_search_handler),
        )
        .route("/ui-bridge/ai/diff", get(ui_bridge_ai_diff_handler))
        // AI analysis
        .route(
            "/ui-bridge/ai/analyze/data",
            post(ui_bridge_ai_analyze_data_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/regions",
            post(ui_bridge_ai_analyze_regions_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/structured-data",
            post(ui_bridge_ai_analyze_structured_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/cross-app-compare",
            post(ui_bridge_ai_analyze_cross_app_handler),
        )
        .route(
            "/ui-bridge/ai/recovery/attempt",
            post(ui_bridge_ai_recovery_attempt_handler),
        )
        // Pixel-accurate image diff (canonical visual regression)
        .route(
            "/ui-bridge/ai/image-diff",
            post(ui_bridge_image_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/image-diff",
            post(ui_bridge_image_diff_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/ai/semantic-search"),
        ("GET", "/ui-bridge/ai/diff"),
        ("POST", "/ui-bridge/ai/analyze/data"),
        ("POST", "/ui-bridge/ai/analyze/regions"),
        ("POST", "/ui-bridge/ai/analyze/structured-data"),
        ("POST", "/ui-bridge/ai/analyze/cross-app-compare"),
        ("POST", "/ui-bridge/ai/recovery/attempt"),
        ("POST", "/ui-bridge/ai/image-diff"),
        ("POST", "/ui-bridge/control/ai/image-diff"),
    ]
}

#[cfg(test)]
mod recovery_failure_tests {
    //! Manual-test-loop iteration 10, item 4 — `/ai/recovery/attempt` laundered
    //! typed refusals.
    //!
    //! The route answered `RECOVERY_UNSCOPED` / `RECOVERY_TARGET_MISSING` with
    //! HTTP 200 `{"success":true,"recovered":false}`: the frontend's
    //! envelope-level `success:false` + `error` are dropped by the response
    //! dispatcher whenever a handler supplies `data`, so `wrap_ipc_result` saw
    //! a bare `{recovered:false}` and had nothing to flatten. These pin the
    //! boundary that closes it — and that it does NOT 400 a genuine recovery.
    //!
    //! They also pin the strict verdict read (plan
    //! `2026-11-10-the-recovery-attempt-route-never-reads-a-missing-verdict-as-success`):
    //! a 2xx reply that states no `attemptSucceeded` is an HTTP 500
    //! `RECOVERY_VERDICT_MISSING`, not a success.

    use super::*;
    use serde_json::json;

    fn ok(data: serde_json::Value) -> BridgeResult {
        Ok(Json(ApiResponse::success(data)))
    }

    #[test]
    fn a_leading_screaming_snake_token_is_read_as_the_code() {
        assert_eq!(
            recovery_code_from_message(
                "RECOVERY_UNSCOPED: ai_recovery_attempt requires params.elementId"
            ),
            Some("RECOVERY_UNSCOPED".to_string())
        );
        assert_eq!(
            recovery_code_from_message("RECOVERY_TARGET_MISSING: element 'x' is not in the tree"),
            Some("RECOVERY_TARGET_MISSING".to_string())
        );
    }

    #[test]
    fn ordinary_prose_is_never_mistaken_for_a_code() {
        assert_eq!(recovery_code_from_message("recovery did not succeed"), None);
        assert_eq!(recovery_code_from_message("Note: something happened"), None);
        assert_eq!(recovery_code_from_message(""), None);
    }

    /// THE DEFECT, pinned: an unscoped refusal must be an HTTP 400 whose `code`
    /// carries `RECOVERY_UNSCOPED` — not a 200 with `success: true`.
    #[test]
    fn an_unscoped_refusal_becomes_http_400_with_its_code() {
        let message = "RECOVERY_UNSCOPED: ai_recovery_attempt requires params.elementId — recovery is limited to the element the failing action addressed.";
        let out = as_recovery_failure(ok(json!({
            "success": false,
            "error": message,
            "code": "RECOVERY_UNSCOPED",
            "attemptSucceeded": false,
        })));
        let (status, Json(body)) = out.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!body.success);
        assert_eq!(body.code.as_deref(), Some("RECOVERY_UNSCOPED"));
        assert_eq!(body.error.as_deref(), Some(message));
    }

    #[test]
    fn a_missing_target_refusal_keeps_its_own_code() {
        let out = as_recovery_failure(ok(json!({
            "success": false,
            "error": "RECOVERY_TARGET_MISSING: element 'ghost' is not in the current tree; nothing to recover.",
            "code": "RECOVERY_TARGET_MISSING",
            "attemptSucceeded": false,
            "elementId": "ghost",
        })));
        let (status, Json(body)) = out.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.code.as_deref(), Some("RECOVERY_TARGET_MISSING"));
    }

    /// Defence in depth: even with no `code` field the leading token of the
    /// message is enough, so a frontend that only prefixes its error still
    /// produces a machine-readable answer.
    #[test]
    fn the_code_is_recovered_from_the_message_when_the_field_is_absent() {
        let out = as_recovery_failure(ok(json!({
            "success": false,
            "error": "RECOVERY_UNSCOPED: no elementId",
            "attemptSucceeded": false,
        })));
        let (_, Json(body)) = out.unwrap_err();
        assert_eq!(body.code.as_deref(), Some("RECOVERY_UNSCOPED"));
    }

    /// And the pre-fix payload shape itself — a bare failed verdict with
    /// nothing else — must still not read as a success.
    #[test]
    fn the_bare_laundered_payload_is_no_longer_a_success() {
        let out = as_recovery_failure(ok(json!({ "attemptSucceeded": false })));
        let (status, Json(body)) = out.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.code.as_deref(), Some("RECOVERY_FAILED"));
    }

    /// The other half of the contract: a route that 400s on EVERYTHING is a
    /// different bug. A genuine recovery stays HTTP 200.
    #[test]
    fn a_genuine_recovery_still_answers_200() {
        let payload = json!({ "attemptSucceeded": true, "elementId": "btn-1" });
        let out = as_recovery_failure(ok(payload.clone()));
        let Json(body) = out.unwrap();
        assert!(body.success);
        assert_eq!(body.data, Some(payload));
    }

    /// A 2xx reply that states NO verdict (`attemptSucceeded` absent, `null`
    /// or not a boolean) is not a success. It used to pass through as
    /// `{"success": true}`, and the test this replaces pinned that: an absence
    /// read as a pass. It now answers HTTP 500 `RECOVERY_VERDICT_MISSING`,
    /// typed `INTERNAL_ERROR`, naming the reply's own keys.
    #[test]
    fn a_reply_that_states_no_verdict_is_a_500_not_a_success() {
        for reply in [
            json!({}),
            json!({ "something": "else" }),
            json!({ "attemptSucceeded": "yes" }),
            json!({ "attemptSucceeded": null }),
        ] {
            let (status, Json(body)) = as_recovery_failure(ok(reply.clone()))
                .expect_err("a reply that states no verdict is not a success");
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{reply}");
            assert!(!body.success, "{reply}");
            assert_eq!(
                body.code.as_deref(),
                Some("RECOVERY_VERDICT_MISSING"),
                "{reply}"
            );
            assert_eq!(detail_code(&body), "INTERNAL_ERROR", "{reply}");
            assert_eq!(
                detail_context_code(&body),
                "RECOVERY_VERDICT_MISSING",
                "{reply}"
            );
            let mut expected_keys: Vec<String> = reply
                .as_object()
                .expect("an object reply")
                .keys()
                .cloned()
                .collect();
            expected_keys.sort();
            assert_eq!(context_payload_keys(&body), expected_keys, "{reply}");
        }
    }

    /// THE LIVE SHAPE, through the real seam. A success envelope with no
    /// `data` is forwarded WHOLE by the dispatcher (`extract_response_data`'s
    /// no-`data` arm), so `wrap_ipc_result` reads `success: true` and answers
    /// 200, yet nothing in it says whether the attempt acted. It must not
    /// leave this boundary as a success.
    #[test]
    fn a_success_envelope_with_no_data_through_the_seam_is_a_500() {
        use super::super::request::{extract_response_data, wrap_ipc_result};

        let envelope = json!({
            "requestId": "r",
            "type": "ai_recovery_attempt",
            "success": true,
            "timestamp": 1,
        });
        let wrapped = wrap_ipc_result(Ok(extract_response_data(&envelope)));
        assert!(
            wrapped.is_ok(),
            "precondition: the seam forwards a data-less success envelope as a 200"
        );
        let (status, Json(body)) = as_recovery_failure(wrapped)
            .expect_err("an envelope that states no verdict is not a success");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.code.as_deref(), Some("RECOVERY_VERDICT_MISSING"));
        assert_eq!(detail_code(&body), "INTERNAL_ERROR");
        assert_eq!(detail_context_code(&body), "RECOVERY_VERDICT_MISSING");
        assert_eq!(
            context_payload_keys(&body),
            ["requestId", "success", "timestamp", "type"]
        );
    }

    /// An `ApiResponse` success that carries no `data` at all states no
    /// verdict either.
    #[test]
    fn a_success_with_no_data_at_all_is_a_500() {
        let mut resp = ApiResponse::success(serde_json::Value::Null);
        resp.data = None;
        let (status, Json(body)) =
            as_recovery_failure(Ok(Json(resp))).expect_err("no data is no verdict");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(detail_code(&body), "INTERNAL_ERROR");
        assert_eq!(detail_context_code(&body), "RECOVERY_VERDICT_MISSING");
        let wire = serde_json::to_value(&body).unwrap();
        assert_eq!(
            wire["error_detail"]["context"]["payload_type"],
            json!("absent")
        );
        assert_eq!(
            wire["error_detail"]["context"]["payload_key_count"],
            json!(0)
        );
    }

    /// Every non-object `data` is Unstated too, and the context names its JSON
    /// type, so each `payload_type` arm is pinned.
    #[test]
    fn a_non_object_reply_names_its_json_type() {
        for (reply, expected) in [
            (json!(null), "null"),
            (json!(true), "boolean"),
            (json!(1), "number"),
            (json!("x"), "string"),
            (json!([1, 2]), "array"),
            (json!({}), "object"),
        ] {
            let (status, Json(body)) = as_recovery_failure(ok(reply.clone()))
                .expect_err("a non-object reply states no verdict");
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{reply}");
            let wire = serde_json::to_value(&body).unwrap();
            let context = &wire["error_detail"]["context"];
            assert_eq!(context["payload_type"], json!(expected), "{reply}");
            assert_eq!(
                context["code"],
                json!("RECOVERY_VERDICT_MISSING"),
                "{reply}"
            );
        }
    }

    /// Each reported key name is cut to 64 characters, so a long key cannot
    /// inflate the body.
    #[test]
    fn each_reported_key_name_is_bounded_in_length() {
        let long = "k".repeat(10_000);
        let mut obj = serde_json::Map::new();
        obj.insert(long, json!(1));
        let (_, Json(body)) = as_recovery_failure(ok(serde_json::Value::Object(obj))).unwrap_err();
        let wire = serde_json::to_value(&body).unwrap();
        let name = wire["error_detail"]["context"]["payload_keys"][0]
            .as_str()
            .expect("the cut key name is reported");
        // A literal, not `PAYLOAD_KEY_CHARS_LIMIT`, so changing the bound reddens it.
        assert_eq!(name.chars().count(), 64);

        // A multi-byte key: byte 64 falls inside a character, so a byte slice
        // would panic here; the cut must count characters.
        let mut obj = serde_json::Map::new();
        obj.insert("€".repeat(100), json!(1));
        let (_, Json(body)) = as_recovery_failure(ok(serde_json::Value::Object(obj))).unwrap_err();
        let wire = serde_json::to_value(&body).unwrap();
        assert_eq!(
            wire["error_detail"]["context"]["payload_keys"][0],
            json!("€".repeat(64))
        );
    }

    /// The reported key names are bounded, and the count shows the cut.
    #[test]
    fn the_reported_payload_keys_are_bounded() {
        let wide: serde_json::Map<String, serde_json::Value> =
            (0..100).map(|i| (format!("k{i:03}"), json!(i))).collect();
        let (_, Json(body)) = as_recovery_failure(ok(serde_json::Value::Object(wide))).unwrap_err();
        let wire = serde_json::to_value(&body).unwrap();
        let context = &wire["error_detail"]["context"];
        assert_eq!(context["payload_type"], json!("object"));
        // A literal, not `PAYLOAD_KEYS_LIMIT`, so changing the bound reddens it.
        assert_eq!(context["payload_keys"].as_array().map(Vec::len), Some(32));
        assert_eq!(context["payload_key_count"], json!(100));
    }

    // ------------------------------------------------------------------
    // `attempt_verdict`, the one reader of `attemptSucceeded`, arm by arm.
    // ------------------------------------------------------------------

    #[test]
    fn attempt_verdict_reads_true_as_acted() {
        for reply in [
            json!({ "attemptSucceeded": true }),
            json!({ "attemptSucceeded": true, "success": true, "elementId": "btn-1" }),
        ] {
            assert_eq!(attempt_verdict(&reply), AttemptVerdict::Acted, "{reply}");
        }
    }

    /// `attemptSucceeded: false` did not act, and an explicit
    /// `success: false` overrides everything else in the reply.
    #[test]
    fn attempt_verdict_reads_false_or_an_explicit_refusal_as_did_not_act() {
        for reply in [
            json!({ "attemptSucceeded": false }),
            json!({ "success": false }),
            json!({ "success": false, "attemptSucceeded": true }),
            json!({
                "success": false,
                "error": "RECOVERY_UNSCOPED: no elementId",
                "attemptSucceeded": false,
            }),
        ] {
            assert_eq!(
                attempt_verdict(&reply),
                AttemptVerdict::DidNotAct,
                "{reply}"
            );
        }
    }

    /// Anything that is not a boolean `attemptSucceeded` states no verdict,
    /// including a reply whose envelope `success: true` survived the seam.
    #[test]
    fn attempt_verdict_reads_anything_else_as_unstated() {
        for reply in [
            json!({}),
            json!({ "something": "else" }),
            json!({ "success": true }),
            json!({ "attemptSucceeded": null }),
            json!({ "attemptSucceeded": "yes" }),
            json!({ "attemptSucceeded": "true" }),
            json!({ "attemptSucceeded": 1 }),
            json!(null),
            json!("attemptSucceeded"),
            json!([true]),
        ] {
            assert_eq!(attempt_verdict(&reply), AttemptVerdict::Unstated, "{reply}");
        }
    }

    /// Transport-level failures (the frontend never answered) are not recovery
    /// verdicts and must keep their status and body.
    #[test]
    fn transport_failures_are_left_alone() {
        let failure: BridgeResult = Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("frontend not ready")),
        ));
        let (status, Json(body)) = as_recovery_failure(failure).unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.code, None);
    }

    // ------------------------------------------------------------------
    // Iteration 11, item 2 — the typed token was laundered ONE LAYER DOWN.
    //
    // `code` at the top level said `RECOVERY_UNSCOPED`; `error_detail.code`
    // said `INTERNAL_ERROR`, because `api_error` leaves the detail empty and a
    // downstream classifier filled it with its fallthrough. A client reading
    // the structured half was told the runner had a defect.
    // ------------------------------------------------------------------

    /// The detail's code for each mapped token — read off the SERIALIZED
    /// envelope, which is what a caller actually sees.
    fn detail_code(body: &ApiResponse<()>) -> String {
        serde_json::to_value(body).unwrap()["error_detail"]["code"]
            .as_str()
            .expect("error_detail.code must be present")
            .to_string()
    }

    fn detail_context_code(body: &ApiResponse<()>) -> String {
        serde_json::to_value(body).unwrap()["error_detail"]["context"]["code"]
            .as_str()
            .expect("error_detail.context.code must be present")
            .to_string()
    }

    /// `error_detail.context.payload_keys`, sorted, so the assertion does not
    /// depend on the JSON map's key order.
    fn context_payload_keys(body: &ApiResponse<()>) -> Vec<String> {
        let mut keys: Vec<String> = serde_json::to_value(body).unwrap()["error_detail"]["context"]
            ["payload_keys"]
            .as_array()
            .expect("error_detail.context.payload_keys must be present")
            .iter()
            .map(|k| k.as_str().expect("a key name").to_string())
            .collect();
        keys.sort();
        keys
    }

    /// Every token [`recovery_error_code`] names, each on the arm that produces
    /// it. The three refusal tokens come from the 200-with-
    /// `attemptSucceeded:false` arm at HTTP 400, and are never
    /// `INTERNAL_ERROR`: a caller-caused refusal is not a runner defect.
    /// `RECOVERY_VERDICT_MISSING` comes from a 2xx reply that states no verdict,
    /// at HTTP 500, and must be `INTERNAL_ERROR`. The `ACTION_FAILED`
    /// fallthrough would claim the attempt ran.
    #[test]
    fn each_recovery_token_gets_its_own_typed_inner_code() {
        for (token, expected, expected_status) in [
            (
                "RECOVERY_UNSCOPED",
                "INVALID_REQUEST",
                StatusCode::BAD_REQUEST,
            ),
            (
                "RECOVERY_TARGET_MISSING",
                "ELEMENT_NOT_FOUND",
                StatusCode::BAD_REQUEST,
            ),
            ("RECOVERY_FAILED", "ACTION_FAILED", StatusCode::BAD_REQUEST),
            (
                "RECOVERY_VERDICT_MISSING",
                "INTERNAL_ERROR",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            let reply = if expected_status == StatusCode::BAD_REQUEST {
                json!({
                    "success": false,
                    "error": format!("{token}: refused"),
                    "code": token,
                    "attemptSucceeded": false,
                })
            } else {
                // The runner mints this token itself. The reply names no
                // code; it just states no verdict.
                json!({ "elementId": "btn-1" })
            };
            let (status, Json(body)) = as_recovery_failure(ok(reply)).unwrap_err();
            assert_eq!(status, expected_status, "{token}");
            assert_eq!(body.code.as_deref(), Some(token), "{token}");
            if expected_status == StatusCode::BAD_REQUEST {
                assert_ne!(detail_code(&body), "INTERNAL_ERROR", "{token}");
            }
            assert_eq!(detail_code(&body), expected, "{token}");
            // The token itself survives the coarse mapping.
            assert_eq!(detail_context_code(&body), token, "{token}");
        }
    }

    /// THE LIVE PATH: `wrap_ipc_result` has already flattened the frontend's
    /// `success:false` payload into a 400 whose `error_detail` was classified
    /// `INTERNAL_ERROR` (no transport pattern matches `RECOVERY_*: …`). That
    /// unclassified detail must be RE-CODED, not left alone.
    #[test]
    fn a_pre_classified_internal_error_detail_is_recoded() {
        for (token, expected) in [
            ("RECOVERY_UNSCOPED", "INVALID_REQUEST"),
            ("RECOVERY_TARGET_MISSING", "ELEMENT_NOT_FOUND"),
            ("RECOVERY_FAILED", "ACTION_FAILED"),
        ] {
            let message = format!("{token}: refused");
            let pre = crate::mcp::types::api_error_detailed(
                message.clone(),
                super::super::types::classify_transport_error(&message),
            );
            assert_eq!(
                detail_code(&pre),
                "INTERNAL_ERROR",
                "precondition: the classifier must fall through for {token}"
            );
            let failure: BridgeResult = Err((StatusCode::BAD_REQUEST, Json(pre)));
            let (_, Json(body)) = as_recovery_failure(failure).unwrap_err();
            assert_eq!(body.code.as_deref(), Some(token), "{token}");
            assert_eq!(detail_code(&body), expected, "{token}");
            assert_eq!(detail_context_code(&body), token, "{token}");
        }
    }

    /// A detail the transport classifier genuinely RECOGNISED is a better
    /// answer than the token's coarse mapping, so it is kept.
    #[test]
    fn an_already_classified_detail_is_not_reclassified() {
        let message = "No element found for 'ghost'";
        let pre = crate::mcp::types::api_error_detailed(
            message,
            super::super::types::classify_transport_error(message),
        );
        assert_eq!(detail_code(&pre), "ELEMENT_NOT_FOUND");
        let failure: BridgeResult = Err((StatusCode::BAD_REQUEST, Json(pre)));
        let (_, Json(body)) = as_recovery_failure(failure).unwrap_err();
        assert_eq!(detail_code(&body), "ELEMENT_NOT_FOUND");
    }

    /// The other half of the contract, structurally: a genuine recovery is a
    /// 200 and carries no `error_detail` at all.
    #[test]
    fn a_genuine_recovery_carries_no_error_detail() {
        let Json(body) = as_recovery_failure(ok(json!({ "attemptSucceeded": true }))).unwrap();
        assert!(body.success);
        assert!(body.error_detail.is_none());
    }

    /// A 400 that already carries a code keeps it — this only ever adds.
    #[test]
    fn an_existing_code_is_not_overwritten() {
        let mut pre = api_error("RECOVERY_WRITE_REFUSED: recovery may not 'type'");
        pre.code = Some("RECOVERY_WRITE_REFUSED".to_string());
        let failure: BridgeResult = Err((StatusCode::BAD_REQUEST, Json(pre)));
        let (_, Json(body)) = as_recovery_failure(failure).unwrap_err();
        assert_eq!(body.code.as_deref(), Some("RECOVERY_WRITE_REFUSED"));
    }
}

#[cfg(test)]
mod recovery_route_answer_tests {
    //! Plan `2026-11-18-the-journey-ledger-never-records-an-unknown-action-verdict-as-a-success-or-a-failure`,
    //! Phase 3: `/ai/recovery/attempt` states what the scoped executor did to
    //! the ORIGINAL action (`originalActionAttempted` /
    //! `originalActionPerformed`), on its 200 and on its 4xx, like the ui-bridge
    //! TS server — and states nothing beside an unknown verdict.
    //!
    //! Every reply here goes through the real seam the route uses:
    //! `extract_response_data` (the frontend envelope → the dispatched value),
    //! then `wrap_ipc_result_keeping_failure_data`, then
    //! [`recovery_route_answer`].
    use super::*;
    use crate::mcp::ui_bridge::request::{
        extract_response_data, wrap_ipc_result_keeping_failure_data,
    };
    use serde_json::json;

    fn route(frontend_envelope: serde_json::Value) -> RecoveryRouteResult {
        recovery_route_answer(wrap_ipc_result_keeping_failure_data(Ok(
            extract_response_data(&frontend_envelope),
        )))
    }

    #[test]
    fn a_success_states_the_original_ran_and_was_performed() {
        let Json(resp) = route(json!({
            "success": true,
            "data": {
                "attemptSucceeded": true,
                "elementId": "btn",
                "originalActionAttempted": true,
                "originalActionPerformed": true,
            },
        }))
        .expect("an acted attempt is a 200");
        let data = resp.data.expect("data");
        assert_eq!(data["attemptSucceeded"], json!(true));
        assert_eq!(data["originalActionAttempted"], json!(true));
        assert_eq!(data["originalActionPerformed"], json!(true));
    }

    #[test]
    fn a_recovery_failed_400_keeps_the_verdict_and_the_facts() {
        let (status, Json(body)) = route(json!({
            "success": false,
            "error": "RECOVERY_FAILED: element is disabled",
            "data": {
                "code": "RECOVERY_FAILED",
                "attemptSucceeded": false,
                "elementId": "btn",
                "originalActionAttempted": true,
                "originalActionPerformed": false,
            },
        }))
        .expect_err("ran and did not act is a 400");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.code.as_deref(), Some("RECOVERY_FAILED"));
        assert!(matches!(
            body.error_detail.as_ref().map(|d| &d.code),
            Some(UiBridgeErrorCode::ActionFailed)
        ));
        assert_eq!(
            body.data,
            Some(json!({
                "attemptSucceeded": false,
                "originalActionAttempted": true,
                "originalActionPerformed": false,
            })),
            "only the verdict and the original-action facts ride the 400"
        );
    }

    #[test]
    fn a_refusal_400_states_nothing_ran() {
        let (status, Json(body)) = route(json!({
            "success": false,
            "error": "RECOVERY_UNSCOPED: ai_recovery_attempt requires params.elementId",
            "code": "RECOVERY_UNSCOPED",
            "data": {
                "code": "RECOVERY_UNSCOPED",
                "attemptSucceeded": false,
                "originalActionAttempted": false,
                "originalActionPerformed": false,
            },
        }))
        .expect_err("a refusal is a 400");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let data = body.data.expect("data");
        assert_eq!(data["originalActionAttempted"], json!(false));
        assert_eq!(data["attemptSucceeded"], json!(false));
    }

    /// An older bundle states no facts: the 400 carries the verdict alone,
    /// and the absent facts stay absent (UNKNOWN), never `false`.
    #[test]
    fn absent_or_garbage_facts_stay_absent() {
        let (_, Json(body)) = route(json!({
            "success": false,
            "error": "RECOVERY_FAILED: nope",
            "data": {
                "code": "RECOVERY_FAILED",
                "attemptSucceeded": false,
                "originalActionAttempted": "yes",
            },
        }))
        .expect_err("a 400");
        assert_eq!(body.data, Some(json!({ "attemptSucceeded": false })));
    }

    /// A verdict-less reply is the 500 `RECOVERY_VERDICT_MISSING`, and it
    /// carries no `data`: a fact beside an unknown verdict would read as one.
    #[test]
    fn a_verdict_missing_500_carries_no_data() {
        let (status, Json(body)) = route(json!({
            "success": true,
            "data": { "originalActionAttempted": true, "elementId": "btn" },
        }))
        .expect_err("no verdict is a 500");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.data, None);
        let wire = serde_json::to_value(&body).unwrap();
        assert_eq!(
            wire["error_detail"]["context"]["code"],
            json!(RECOVERY_VERDICT_MISSING)
        );
    }
}
