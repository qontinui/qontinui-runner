//! Effects endpoints — D3 effect-calculus recent-effect ledger.
//!
//! Read-only proxy over the SDK's `GET /effects/recent` route (handler
//! `getRecentEffects`). The SDK owns the effect ledger (each entry is an
//! `EffectRecordEntry { requestId?, action, elementId?, outcome, cause,
//! verification, timestamp }`); the runner exposes it through the
//! `/ui-bridge/effects/recent` HTTP surface so agents / dashboards driving the
//! runner can read the recent predicted-vs-observed outcomes without reaching
//! the SDK directly.
//!
//! This is a "runner direct" family route per `CONTRACT.md`: the handler lives
//! here and is registered in this family's `routes()` + `route_entries()`. It
//! forwards to the SDK's HTTP surface via `crate::mcp::sdk_client::sdk_request`
//! (the same passthrough helper used by the SDK-proxy GETs in `sdk_client.rs`),
//! returning the SDK's `APIResponse<EffectRecordEntry[]>` JSON verbatim. The
//! `?limit=N` query param is forwarded to the SDK for server-side truncation.
//!
//! ## `POST /ui-bridge/control/component/{id}/action/{action_id}/predict`
//!
//! Phase 6 of plan
//! `2026-09-04-effect-calculus-joins-the-component-action-registry` adds the
//! second route in this family: ask the effect twin what invoking a component
//! action WOULD do, **without invoking it**. Same wiring as the ledger route
//! above — a verbatim proxy to the SDK, which owns the registry, the effect
//! signatures and the snapshot pipeline that produce the answer.
//!
//! **It lives beside the ledger, not beside the invocation route in
//! `elements.rs`.** Both of this family's routes are effect-calculus READS
//! that change nothing; the invocation route dispatches a handler that does.
//! Filing a predict endpoint next to the code that executes actions is how a
//! future edit ends up sharing a code path with the one thing this endpoint
//! must never do.
//!
//! The body is forwarded verbatim rather than parsed into a typed request: the
//! SDK already owns the wrapped-vs-bare param normalisation
//! (`server/handlers.ts` `normalizePredictBody`), and a second copy here would
//! fork from it the moment either side gained a request-level field. An absent
//! or unparseable body is forwarded as an empty object — predicting with no
//! params is a legitimate question, not a malformed request.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    response::Json,
};
use reqwest::Method;
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::sdk_client::sdk_request;
use crate::mcp::types::ApiState;

/// Query parameters for `GET /ui-bridge/effects/recent`.
#[derive(Debug, Deserialize)]
pub struct RecentEffectsQuery {
    /// Maximum number of recent effect records to return. Forwarded to the SDK
    /// verbatim; the SDK applies the default/cap when omitted.
    pub limit: Option<i64>,
}

/// GET /ui-bridge/effects/recent — proxy to the SDK's `getRecentEffects`.
///
/// Forwards the `limit` query param and relays the SDK's
/// `APIResponse<EffectRecordEntry[]>` JSON. On transport failure returns a
/// `{ success: false, error }` envelope (HTTP 200) mirroring the other
/// `sdk_request`-backed proxies in `sdk_client.rs`.
pub async fn ui_bridge_effects_recent_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RecentEffectsQuery>,
) -> Json<serde_json::Value> {
    let path = build_sdk_path(query.limit);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// Build the SDK-side path, forwarding `limit` as a query param when present.
fn build_sdk_path(limit: Option<i64>) -> String {
    match limit {
        Some(limit) => format!("/effects/recent?limit={}", limit),
        None => "/effects/recent".to_string(),
    }
}

/// POST /ui-bridge/control/component/{id}/action/{action_id}/predict —
/// proxy to the SDK's `predictComponentAction`.
///
/// **Asks; never acts.** The SDK handler this forwards to resolves the
/// action's effect signature, captures a pre-snapshot and evaluates the
/// prediction without calling the action's handler. Nothing on this side may
/// ever fall back to the invocation route: a "prediction" that executed the
/// action is the single worst failure this endpoint can have.
///
/// Relays the SDK's `APIResponse<ComponentActionPredictResponse>` verbatim. On
/// transport failure returns a `{ success: false, error }` envelope (HTTP 200)
/// mirroring the ledger route above — note that such an envelope means the
/// question was never asked, NOT that the action is unclassified and NOT that
/// it is safe.
pub async fn ui_bridge_predict_component_action_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, action_id)): Path<(String, String)>,
    body: Bytes,
) -> Json<serde_json::Value> {
    let path = build_predict_sdk_path(&id, &action_id);
    let payload = parse_predict_body(&body);
    match sdk_request(&state, Method::POST, &path, Some(payload)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// Build the SDK-side predict path for a component action.
fn build_predict_sdk_path(component_id: &str, action_id: &str) -> String {
    format!(
        "/control/component/{}/action/{}/predict",
        component_id, action_id
    )
}

/// Parse a predict body into the object the SDK expects.
///
/// Anything that is not a JSON object — absent, unparseable, `null`, or a
/// scalar — becomes `{}`. Rejecting those instead would turn "predict this
/// action with no params" into a 400, and forwarding a non-object would make
/// the SDK's normaliser guess.
fn parse_predict_body(body: &Bytes) -> serde_json::Value {
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(value @ serde_json::Value::Object(_)) => value,
        _ => serde_json::json!({}),
    }
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/effects/recent",
            get(ui_bridge_effects_recent_handler),
        )
        .route(
            "/ui-bridge/control/component/{id}/action/{action_id}/predict",
            post(ui_bridge_predict_component_action_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/effects/recent"),
        (
            "POST",
            "/ui-bridge/control/component/{id}/action/{action_id}/predict",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_entries_lists_recent_effects() {
        let entries = route_entries();
        assert!(entries.contains(&("GET", "/ui-bridge/effects/recent")));
    }

    #[test]
    fn route_entries_lists_the_predict_route() {
        // Registering in `routes()` without mirroring the tuple here is the
        // exact drift `manifest_matches_route_calls` exists to catch; this
        // pins the entry at the family level so the failure names THIS file.
        let entries = route_entries();
        assert!(entries.contains(&(
            "POST",
            "/ui-bridge/control/component/{id}/action/{action_id}/predict"
        )));
    }

    #[test]
    fn predict_sdk_path_is_the_invocation_path_plus_predict() {
        // The SDK declares `/control/component/:id/action/:actionId/predict`.
        // Forwarding to anything else is a 404 the caller sees as "no
        // prediction available", which is exactly the reading Phase 6 forbids.
        assert_eq!(
            build_predict_sdk_path("invoice-row", "delete"),
            "/control/component/invoice-row/action/delete/predict"
        );
    }

    #[test]
    fn an_absent_or_broken_body_forwards_as_an_empty_object() {
        // Predicting with no params is a legitimate question. It must not turn
        // into a 400, and it must not forward `null` — the SDK normalises an
        // object.
        assert_eq!(parse_predict_body(&Bytes::new()), serde_json::json!({}));
        assert_eq!(
            parse_predict_body(&Bytes::from_static(b"not json")),
            serde_json::json!({})
        );
        assert_eq!(
            parse_predict_body(&Bytes::from_static(b"null")),
            serde_json::json!({})
        );
        // A JSON scalar is not a param bag either.
        assert_eq!(
            parse_predict_body(&Bytes::from_static(b"7")),
            serde_json::json!({})
        );
    }

    #[test]
    fn a_real_body_is_forwarded_verbatim() {
        // Verbatim, NOT re-shaped: the SDK owns the wrapped-vs-bare merge
        // (`normalizePredictBody`), and a second copy here would fork from it.
        assert_eq!(
            parse_predict_body(&Bytes::from_static(br#"{"layoutId":"split"}"#)),
            serde_json::json!({ "layoutId": "split" })
        );
        assert_eq!(
            parse_predict_body(&Bytes::from_static(br#"{"params":{"a":1},"requestId":"r"}"#)),
            serde_json::json!({ "params": { "a": 1 }, "requestId": "r" })
        );
    }

    #[test]
    fn build_sdk_path_forwards_limit() {
        assert_eq!(build_sdk_path(Some(25)), "/effects/recent?limit=25");
        assert_eq!(build_sdk_path(None), "/effects/recent");
    }
}
