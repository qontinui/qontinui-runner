//! AI analysis + semantic diff routes.
//!
//! Macro-generated handlers that proxy AI-side operations (analysis of
//! tabular/region/structured data, cross-app comparison, recovery attempts,
//! semantic search, semantic diff, media compare, pixel-accurate image diff)
//! to the webview SDK via `ui_bridge_request_sync`.
//!
//! Namespace note: most handlers sit under `/ai/*`; `image-diff` also has a
//! `/control/ai/image-diff` alias retained for backwards compatibility with
//! the `compareVisualRegression` caller path.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;
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

/// Stop `/ai/recovery/attempt` from laundering a typed refusal into a success.
///
/// THE DEFECT: the frontend answered `RECOVERY_UNSCOPED` /
/// `RECOVERY_TARGET_MISSING` with `{success: false, error: "<CODE>: …",
/// data: {recovered: false}}`, but the response dispatcher
/// (`request::handle_ui_bridge_response`) forwards ONLY `response.data` when
/// the handler supplied one — so `wrap_ipc_result` received a bare
/// `{recovered: false}` with no `success: false` to flatten and answered
/// **HTTP 200 `{"success":true,"recovered":false}`**. A caller that refused to
/// guess a target was indistinguishable from one that recovered.
///
/// The frontend half now mirrors the refusal into `data` (see
/// `recoveryFailureData`); this half is the boundary that turns it into an
/// honest HTTP answer, exactly as [`super::elements::as_action_failure`] does
/// for `execute_action`:
///
/// - a 200 whose payload says `recovered: false` becomes an HTTP 400 carrying
///   the payload's own `error` text and a machine-readable `code`;
/// - a 400 that arrived without a `code` gets one derived from its message.
///
/// `recovered: true` and transport-level failures (500 / 503 — the frontend
/// never answered) pass through untouched: those are not recovery verdicts.
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
            let recovered_false = resp
                .data
                .as_ref()
                .and_then(|d| d.get("recovered"))
                .and_then(|v| v.as_bool())
                == Some(false);
            if !recovered_false {
                return Ok(Json(resp));
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
            let mut body = api_error(message);
            body.code = Some(code);
            Err((StatusCode::BAD_REQUEST, Json(body)))
        }
        Err((status, Json(mut body))) => {
            if status == StatusCode::BAD_REQUEST && body.code.is_none() {
                body.code = Some(code_of(&serde_json::Value::Null, body.error.as_deref()));
            }
            Err((status, Json(body)))
        }
    }
}

/// `POST /ui-bridge/ai/recovery/attempt` — hand-written rather than
/// macro-generated so the response passes through [`as_recovery_failure`].
pub async fn ui_bridge_ai_recovery_attempt_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> BridgeResult {
    let payload = serde_json::json!({ "params": body });
    as_recovery_failure(crate::mcp::ui_bridge::request::wrap_ipc_result(
        ui_bridge_request_sync(&state, "ai_recovery_attempt", payload).await,
    ))
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
            "recovered": false,
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
            "recovered": false,
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
            "recovered": false,
        })));
        let (_, Json(body)) = out.unwrap_err();
        assert_eq!(body.code.as_deref(), Some("RECOVERY_UNSCOPED"));
    }

    /// And the pre-fix payload shape itself — a bare `{recovered:false}` with
    /// nothing else — must still not read as a success.
    #[test]
    fn the_bare_laundered_payload_is_no_longer_a_success() {
        let out = as_recovery_failure(ok(json!({ "recovered": false })));
        let (status, Json(body)) = out.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.code.as_deref(), Some("RECOVERY_FAILED"));
    }

    /// The other half of the contract: a route that 400s on EVERYTHING is a
    /// different bug. A genuine recovery stays HTTP 200.
    #[test]
    fn a_genuine_recovery_still_answers_200() {
        let payload = json!({ "recovered": true, "elementId": "btn-1" });
        let out = as_recovery_failure(ok(payload.clone()));
        let Json(body) = out.unwrap();
        assert!(body.success);
        assert_eq!(body.data, Some(payload));
    }

    /// A response with no recovery verdict at all is not this boundary's
    /// business — pass it through untouched.
    #[test]
    fn an_unrelated_payload_passes_through() {
        let out = as_recovery_failure(ok(json!({ "something": "else" })));
        assert!(out.is_ok());
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
