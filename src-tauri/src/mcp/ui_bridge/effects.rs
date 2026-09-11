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

use super::elements::try_ws_dispatch_for_app;
use super::request::ui_bridge_request_sync;

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
/// **Dispatch mirrors the invocation route's, not the ledger route's above.**
/// `sdk_request` only reaches an EXTERNAL app that registered over
/// `POST /ui-bridge/sdk/connect` — nothing in this runner's own embedded
/// frontend ever calls that route, so `sdk_request` alone left this endpoint
/// permanently unreachable for the runner's own UI (coord finding
/// `0b8ebfff-d740-4fd4-acb8-294bc807ed5a`: predict returned the identical
/// `"No active SDK app connection"` for a real action id and a bogus one
/// alike, proving it never even reached id/action resolution). The sibling
/// `ui_bridge_execute_component_action_handler` (`elements.rs`) reaches the
/// embedded frontend by trying a WS-registered wrapper first
/// (`try_ws_dispatch_for_app`) and falling through to a direct Tauri IPC call
/// when none is registered — this handler now does the same, dispatching to
/// the SDK's `predictComponentAction` IPC/WS command (added by ui-bridge PR
/// #202, `react/commandHandlers.ts` `case 'predictComponentAction'`) instead
/// of reinventing the resolution.
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
    let raw_body = parse_predict_body(&body);
    let ipc_request = normalize_predict_body(&raw_body);
    let ipc_payload = serde_json::json!({
        "componentId": id,
        "actionId": action_id,
        "request": ipc_request,
    });

    let ws_path = build_predict_sdk_path(&id, &action_id);
    if let Some(ws_outcome) = try_ws_dispatch_for_app(
        &state.app_registry,
        &state.app_dispatcher,
        &id,
        "predictComponentAction",
        Method::POST,
        &ws_path,
        ipc_payload.clone(),
    )
    .await
    {
        return match ws_outcome {
            Ok(value) => Json(value),
            Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
        };
    }

    match ui_bridge_request_sync(&state, "predict_component_action", ipc_payload).await {
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

/// Normalize a parsed predict body into `{ params?, requestId? }` for the
/// IPC/WS transport, mirroring the SDK's own `normalizePredictBody`
/// (`ui-bridge` `server/handlers.ts`) so the two transports answer the same
/// question the same way. The HTTP transport (`sdk_request`, above) forwards
/// the raw body verbatim and leaves this normalisation to the SDK's HTTP
/// handler; the IPC/WS transport has no such handler in front of it, so this
/// function does the SDK's own normalisation here rather than sending an
/// unnormalised bag the frontend would have to reinterpret.
///
/// An explicit `params` object wins over flat top-level keys on collision —
/// the caller that spelled it out meant it. An empty result omits `params`
/// entirely (not `{}`), matching `ActionParams.params` being optional:
/// `resolveActionEffect`/signature code that tests `params === undefined`
/// must see the same shape a caller passing nothing would produce.
fn normalize_predict_body(body: &serde_json::Value) -> serde_json::Value {
    let obj = match body.as_object() {
        Some(o) => o,
        None => return serde_json::json!({}),
    };
    let mut flat = serde_json::Map::new();
    for (k, v) in obj {
        if k != "params" && k != "requestId" {
            flat.insert(k.clone(), v.clone());
        }
    }
    if let Some(serde_json::Value::Object(explicit)) = obj.get("params") {
        for (k, v) in explicit {
            flat.insert(k.clone(), v.clone());
        }
    }

    let mut out = serde_json::Map::new();
    if !flat.is_empty() {
        out.insert("params".to_string(), serde_json::Value::Object(flat));
    }
    if let Some(serde_json::Value::String(request_id)) = obj.get("requestId") {
        out.insert(
            "requestId".to_string(),
            serde_json::Value::String(request_id.clone()),
        );
    }
    serde_json::Value::Object(out)
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
            parse_predict_body(&Bytes::from_static(
                br#"{"params":{"a":1},"requestId":"r"}"#
            )),
            serde_json::json!({ "params": { "a": 1 }, "requestId": "r" })
        );
    }

    #[test]
    fn normalize_predict_body_wraps_a_bare_bag_into_params() {
        assert_eq!(
            normalize_predict_body(&serde_json::json!({ "tabId": "ai" })),
            serde_json::json!({ "params": { "tabId": "ai" } })
        );
    }

    #[test]
    fn normalize_predict_body_passes_an_already_wrapped_body_through() {
        assert_eq!(
            normalize_predict_body(&serde_json::json!({
                "params": { "a": 1 },
                "requestId": "r"
            })),
            serde_json::json!({ "params": { "a": 1 }, "requestId": "r" })
        );
    }

    #[test]
    fn normalize_predict_body_prefers_explicit_params_on_key_collision() {
        // A flat top-level key that collides with something inside the
        // explicit `params` object loses — the caller who spelled `params`
        // out meant it.
        assert_eq!(
            normalize_predict_body(&serde_json::json!({
                "a": "flat-loses",
                "params": { "a": "explicit-wins" }
            })),
            serde_json::json!({ "params": { "a": "explicit-wins" } })
        );
    }

    #[test]
    fn normalize_predict_body_omits_params_entirely_when_empty() {
        // Not `{"params": {}}` — an absent key, matching `params: undefined`
        // on the TypeScript side, so a signature testing `params === undefined`
        // sees the same shape a no-params caller produces.
        let out = normalize_predict_body(&serde_json::json!({}));
        assert!(!out.as_object().unwrap().contains_key("params"));
        assert_eq!(out, serde_json::json!({}));
    }

    #[test]
    fn normalize_predict_body_drops_a_non_string_request_id() {
        let out = normalize_predict_body(&serde_json::json!({ "requestId": 7 }));
        assert!(!out.as_object().unwrap().contains_key("requestId"));
    }

    #[test]
    fn normalize_predict_body_on_a_non_object_input_is_empty() {
        assert_eq!(
            normalize_predict_body(&serde_json::json!(null)),
            serde_json::json!({})
        );
        assert_eq!(
            normalize_predict_body(&serde_json::json!(7)),
            serde_json::json!({})
        );
    }

    #[test]
    fn build_sdk_path_forwards_limit() {
        assert_eq!(build_sdk_path(Some(25)), "/effects/recent?limit=25");
        assert_eq!(build_sdk_path(None), "/effects/recent");
    }
}
