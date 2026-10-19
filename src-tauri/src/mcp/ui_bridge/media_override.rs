//! In-situ asset override — the runner's direct `/ui-bridge/control/media/override`
//! family (plan 2026-10-13-image-generation-and-visual-judgment, Phase 18).
//!
//! The SDK's `POST /control/media/override` swaps one image slot's source in
//! place (an `<img>`'s `src`/`srcset`, every `<source srcset>` of its
//! `<picture>`, or a CSS `background-image`), waits for the browser to decode
//! it and confirms `currentSrc`, and `DELETE` restores the originals and
//! verifies them. `GET` lists what is still overridden.
//!
//! Three layers, the F2 network-stub precedent (`stubs.rs`):
//!   1. this file — validation + the HTTP route, forwarding to the runner's
//!      OWN webview over `ui_bridge_request_sync`;
//!   2. `mcp/sdk_client.rs` — `/ui-bridge/sdk/control/media/override`, the same
//!      command over the active SDK app's transport (WS or HTTP);
//!   3. `src/hooks/ui-bridge-events/useMediaEvents.ts` — the frontend hook that
//!      runs the SDK engine in the runner's webview.
//!
//! It is a DEV CONTROL. The SDK engine answers `capability_unavailable_in_build`
//! unless the realm opted in (`@qontinui/ui-bridge` `core/dev-controls.ts`): the
//! injected runtime always does; the runner's webview opts in only when
//! `import.meta.env.DEV` is true, i.e. under a Vite dev server. Every runner —
//! the published one and every dev or temp runner alike — runs in exe mode, a
//! Vite BUILD with the frontend embedded, so this direct route ALWAYS answers
//! the typed refusal from the runner's own webview. It is mounted anyway
//! because the SDK manifest-parity test requires every SDK route to be exposed.
//! The `/sdk/` wrapper reaches the override only on an app that opted in itself.
//!
//! The gate is NOT a security boundary. The override adds no capability beyond
//! the runner's existing `page_evaluate` / eval surface, through which a caller
//! can already run script in the page and rewrite any `src` itself. The gate
//! keeps a page-altering control off by default in an app's production build;
//! it keeps nothing from a caller who can evaluate script.
//!
//! Validation mirrors the SDK's `validateMediaOverrideRequest` for an early
//! 400; the SDK copy is the authority. One rule is runner-only: the decode wait
//! must finish inside the IPC envelope's own budget, or the envelope times out
//! while the frontend is still waiting and the reply is lost. An absent
//! `decodeTimeoutMs` is filled with a value that fits; a caller's value that
//! does not fit is refused, naming the budget.
//!
//! Redaction: the request body carries candidate URLs (which can be presigned
//! asset-store URLs) and the response carries the page's original URLs. Neither
//! is logged here — the log lines name the target only.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use serde_json::Value;
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::{
    get_ui_bridge_timeout_ms, ui_bridge_request_sync, wrap_ipc_result_keeping_failure_data,
};

/// SDK bounds, mirrored (`control/media-override.ts`).
pub(crate) const MIN_TTL_MS: u64 = 1_000;
pub(crate) const MAX_TTL_MS: u64 = 900_000;
pub(crate) const MAX_DECODE_TIMEOUT_MS: u64 = 60_000;
pub(crate) const DEFAULT_DECODE_TIMEOUT_MS: u64 = 10_000;

/// The `/sdk/` wrapper's transport budget: both `AppDispatcher`'s HTTP arm and
/// `CommandRelay`'s WS arm wait 30 s (`app_dispatch.rs`, `command_relay.rs`).
pub(crate) const SDK_DISPATCH_BUDGET_MS: u64 = 30_000;

/// Headroom kept between the decode wait and the transport's own timeout, so
/// the frontend's typed `DECODE_TIMEOUT` (with its rollback) reaches the
/// caller instead of a bare transport timeout. After its decode wait the SDK
/// takes one more bounded step — measuring the candidate's pixels on success,
/// or verifying the rollback against the rendered page on failure — each
/// capped at `POST_DECODE_WAIT_MS` (1 s) in the SDK's
/// `control/media-override.ts`, so this must stay above 1 s.
pub(crate) const DECODE_HEADROOM_MS: u64 = 2_000;
const _: () = assert!(
    DECODE_HEADROOM_MS > 1_000,
    "headroom must cover the SDK's POST_DECODE_WAIT_MS"
);

fn non_empty_str(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.trim().is_empty())
}

fn bounded_int(body: &Value, field: &str, min: u64, max: u64) -> Result<(), String> {
    match body.get(field) {
        None => Ok(()),
        Some(v) => match v.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(()),
            _ => Err(format!(
                "{field} must be an integer in {min}..{max} (got {v})"
            )),
        },
    }
}

/// Validate a `POST /control/media/override` body. Mirrors the SDK rules.
pub(crate) fn validate_override_body(body: &Value) -> Result<(), String> {
    let obj = body.as_object().ok_or("body must be a JSON object")?;
    let has_slot = obj.contains_key("slotId");
    let has_element = obj.contains_key("elementId");
    if has_slot == has_element {
        return Err("exactly one of slotId or elementId is required".into());
    }
    if has_slot && !non_empty_str(&obj["slotId"]) {
        return Err("slotId must be a non-empty string".into());
    }
    if has_element && !non_empty_str(&obj["elementId"]) {
        return Err("elementId must be a non-empty string".into());
    }
    for f in ["src", "srcset", "sizes", "backgroundImage"] {
        if let Some(v) = obj.get(f) {
            if !non_empty_str(v) {
                return Err(format!("{f} must be a non-empty string"));
            }
        }
    }
    if let Some(sources) = obj.get("sources") {
        let arr = sources
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or("sources must be a non-empty array of {type?, srcset, sizes?}")?;
        for (i, s) in arr.iter().enumerate() {
            let o = s
                .as_object()
                .ok_or_else(|| format!("sources[{i}] must be an object"))?;
            if !o.get("srcset").is_some_and(non_empty_str) {
                return Err(format!("sources[{i}].srcset must be a non-empty string"));
            }
            for f in ["type", "sizes"] {
                if let Some(v) = o.get(f) {
                    if !non_empty_str(v) {
                        return Err(format!("sources[{i}].{f} must be a non-empty string"));
                    }
                }
            }
        }
    }
    let image =
        obj.contains_key("src") || obj.contains_key("srcset") || obj.contains_key("sources");
    let background = obj.contains_key("backgroundImage");
    if !image && !background {
        return Err("one of src, srcset, sources or backgroundImage is required".into());
    }
    if image && background {
        return Err(
            "backgroundImage overrides CSS; it cannot be combined with src, srcset or sources"
                .into(),
        );
    }
    if obj.contains_key("sizes") && !obj.contains_key("srcset") {
        return Err("sizes needs srcset".into());
    }
    if obj.contains_key("sources") && !obj.contains_key("src") && !obj.contains_key("srcset") {
        return Err(
            "sources replaces the <source> elements only; also give src or srcset for the <img> fallback"
                .into(),
        );
    }
    bounded_int(body, "ttlMs", MIN_TTL_MS, MAX_TTL_MS)?;
    bounded_int(body, "decodeTimeoutMs", 0, MAX_DECODE_TIMEOUT_MS)?;
    Ok(())
}

/// Validate a `DELETE /control/media/override` body (`{}` or `{overrideId}`).
pub(crate) fn validate_restore_body(body: &Value) -> Result<(), String> {
    let obj = body.as_object().ok_or("body must be a JSON object")?;
    if let Some(v) = obj.get("overrideId") {
        if !non_empty_str(v) {
            return Err("overrideId must be a non-empty string".into());
        }
    }
    Ok(())
}

/// Fit the decode wait inside a transport budget of `budget_ms`: fill an
/// absent `decodeTimeoutMs`, refuse one that would outlive the transport.
pub(crate) fn fit_decode_budget(body: &mut Value, budget_ms: u64) -> Result<(), String> {
    let ceiling = budget_ms.saturating_sub(DECODE_HEADROOM_MS).max(1);
    let obj = body.as_object_mut().ok_or("body must be a JSON object")?;
    match obj.get("decodeTimeoutMs").and_then(Value::as_u64) {
        Some(n) if n > ceiling => Err(format!(
            "decodeTimeoutMs {n} would outlive the transport's {budget_ms} ms budget (at most \
             {ceiling} ms leaves room for the typed DECODE_TIMEOUT and its rollback to reach \
             you); lower it, or raise the runner's UI Bridge IPC timeout"
        )),
        Some(_) => Ok(()),
        None => {
            obj.insert(
                "decodeTimeoutMs".into(),
                Value::from(ceiling.min(DEFAULT_DECODE_TIMEOUT_MS)),
            );
            Ok(())
        }
    }
}

/// Validate + budget-fit an override body. Shared with the `/sdk/` wrapper.
pub(crate) fn prepare_override_body(mut body: Value, budget_ms: u64) -> Result<Value, String> {
    validate_override_body(&body)?;
    fit_decode_budget(&mut body, budget_ms)?;
    Ok(body)
}

fn target_label(body: &Value) -> String {
    body.get("slotId")
        .and_then(Value::as_str)
        .map(|s| format!("slot {s}"))
        .or_else(|| {
            body.get("elementId")
                .and_then(Value::as_str)
                .map(|s| format!("element {s}"))
        })
        .unwrap_or_else(|| "<no target>".into())
}

type HandlerResult = Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<Value>>)>;

fn bad_request(msg: String) -> (StatusCode, Json<ApiResponse<Value>>) {
    let e = api_error(msg);
    (
        StatusCode::BAD_REQUEST,
        Json(ApiResponse {
            success: e.success,
            data: None,
            error: e.error,
            error_detail: e.error_detail,
            hint: e.hint,
            code: e.code,
            suggestions: e.suggestions,
        }),
    )
}

/// POST /ui-bridge/control/media/override — swap one image slot in the runner's webview.
pub async fn ui_bridge_media_override_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<Value>,
) -> HandlerResult {
    let body = prepare_override_body(body, get_ui_bridge_timeout_ms()).map_err(bad_request)?;
    info!("UI Bridge API: media override on {}", target_label(&body));
    wrap_ipc_result_keeping_failure_data(
        ui_bridge_request_sync(&state, "media_override_apply", body).await,
    )
}

/// DELETE /ui-bridge/control/media/override — restore one override or all, verified.
pub async fn ui_bridge_media_override_restore_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<Value>>,
) -> HandlerResult {
    let body = body
        .map(|Json(b)| b)
        .unwrap_or_else(|| serde_json::json!({}));
    validate_restore_body(&body).map_err(bad_request)?;
    info!("UI Bridge API: media override restore");
    wrap_ipc_result_keeping_failure_data(
        ui_bridge_request_sync(&state, "media_override_restore", body).await,
    )
}

/// GET /ui-bridge/control/media/override — the overrides still active.
pub async fn ui_bridge_media_override_list_handler(
    State(state): State<Arc<ApiState>>,
) -> HandlerResult {
    wrap_ipc_result_keeping_failure_data(
        ui_bridge_request_sync(&state, "media_override_list", serde_json::json!({})).await,
    )
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route(
        "/ui-bridge/control/media/override",
        post(ui_bridge_media_override_handler)
            .delete(ui_bridge_media_override_restore_handler)
            .get(ui_bridge_media_override_list_handler),
    )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/control/media/override"),
        ("DELETE", "/ui-bridge/control/media/override"),
        ("GET", "/ui-bridge/control/media/override"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_minimal_src_override_is_valid() {
        assert!(validate_override_body(&json!({"slotId": "hero", "src": "/c.png"})).is_ok());
        assert!(validate_override_body(&json!({"elementId": "img-1", "srcset": "/a 1x"})).is_ok());
        assert!(
            validate_override_body(&json!({"slotId": "bg", "backgroundImage": "/c.png"})).is_ok()
        );
        assert!(validate_override_body(&json!({
            "slotId": "p", "src": "/c.png",
            "sources": [{"type": "image/avif", "srcset": "/c.avif"}]
        }))
        .is_ok());
    }

    #[test]
    fn the_sdk_rules_are_mirrored() {
        let cases = [
            (json!([]), "JSON object"),
            (
                json!({"src": "/c.png"}),
                "exactly one of slotId or elementId",
            ),
            (
                json!({"slotId": "a", "elementId": "b", "src": "x"}),
                "exactly one",
            ),
            (json!({"slotId": ""}), "slotId must be a non-empty string"),
            (
                json!({"slotId": "a"}),
                "one of src, srcset, sources or backgroundImage",
            ),
            (
                json!({"slotId": "a", "src": "x", "backgroundImage": "y"}),
                "cannot be combined",
            ),
            (
                json!({"slotId": "a", "src": "x", "sizes": "50vw"}),
                "sizes needs srcset",
            ),
            (
                json!({"slotId": "a", "sources": [{"srcset": "x"}]}),
                "also give src or srcset",
            ),
            (
                json!({"slotId": "a", "src": "x", "sources": []}),
                "non-empty array",
            ),
            (
                json!({"slotId": "a", "src": "x", "sources": [{"type": "t"}]}),
                "srcset must be",
            ),
            (
                json!({"slotId": "a", "src": "x", "ttlMs": 10}),
                "ttlMs must be an integer",
            ),
            (
                json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 60001}),
                "decodeTimeoutMs",
            ),
            (
                json!({"slotId": "a", "src": 7}),
                "src must be a non-empty string",
            ),
        ];
        for (body, needle) in cases {
            let err = validate_override_body(&body).expect_err(&body.to_string());
            assert!(err.contains(needle), "{body}: {err} lacks {needle}");
        }
    }

    #[test]
    fn restore_takes_an_optional_override_id() {
        assert!(validate_restore_body(&json!({})).is_ok());
        assert!(validate_restore_body(&json!({"overrideId": "mo-1"})).is_ok());
        assert!(validate_restore_body(&json!({"overrideId": ""})).is_err());
        assert!(validate_restore_body(&json!("x")).is_err());
    }

    #[test]
    fn an_absent_decode_wait_is_fitted_inside_the_transport_budget() {
        let body = prepare_override_body(json!({"slotId": "a", "src": "x"}), 10_000).unwrap();
        assert_eq!(body["decodeTimeoutMs"], json!(8_000));
        // A roomy budget keeps the SDK's own default.
        let body =
            prepare_override_body(json!({"slotId": "a", "src": "x"}), SDK_DISPATCH_BUDGET_MS)
                .unwrap();
        assert_eq!(body["decodeTimeoutMs"], json!(DEFAULT_DECODE_TIMEOUT_MS));
        // A tiny budget still leaves a positive wait rather than zero.
        let body = prepare_override_body(json!({"slotId": "a", "src": "x"}), 1_500).unwrap();
        assert_eq!(body["decodeTimeoutMs"], json!(1));
    }

    #[test]
    fn a_decode_wait_that_would_outlive_the_transport_is_refused_naming_the_budget() {
        let err = prepare_override_body(
            json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 9_000}),
            10_000,
        )
        .unwrap_err();
        assert!(err.contains("10000 ms budget"), "{err}");
        assert!(prepare_override_body(
            json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 8_000}),
            10_000
        )
        .is_ok());
    }

    #[test]
    fn the_target_label_never_carries_a_url() {
        let label =
            target_label(&json!({"slotId": "hero", "src": "https://s3/x?X-Amz-Signature=s"}));
        assert_eq!(label, "slot hero");
    }

    #[test]
    fn route_entries_match_the_router() {
        let entries = route_entries();
        assert_eq!(entries.len(), 3);
        for method in ["POST", "DELETE", "GET"] {
            assert!(entries.contains(&(method, "/ui-bridge/control/media/override")));
        }
    }
}
