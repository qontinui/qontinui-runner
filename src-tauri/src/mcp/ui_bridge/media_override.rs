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
//! 400; the SDK copy is the authority. One rule is runner-only: the page-side
//! wait must finish inside the transport's own budget, or the transport times
//! out while the page is still waiting and the reply is lost. That holds for
//! both waits — an apply's `decodeTimeoutMs` and a restore's `verifyTimeoutMs`.
//! A caller's value that does not fit the runner's budget is refused, naming
//! the budget, on EVERY arm.
//!
//! An ABSENT wait is filled only where the body goes straight to the SDK engine
//! ([`WaitFill`]): the direct route (the runner's own webview over IPC) and the
//! `/sdk/` wrapper's WebSocket arm. The `/sdk/` HTTP arm goes to the app's own
//! ui-bridge server, whose relay fits the wait to ITS command budget (8 s by
//! default, a 6 s ceiling — `relay-handlers.ts` `relayCommandBudgetMs`) and
//! REFUSES a larger value; a runner fill sized to the runner's 30 s would be
//! refused there, so that arm leaves an absent wait absent for the relay to
//! fill. The relay fills at most `DEFAULT_DECODE_TIMEOUT_MS` (10 s), so its
//! fill also fits inside the runner's own 30 s `/sdk/` budget.
//!
//! The restore fit matters on its own: an override can be APPLIED with a longer
//! decode wait than the transport allows (the injected runtime accepts up to
//! 60 s), and the SDK's restore waits up to that override's own
//! `decodeTimeoutMs` unless the request bounds it with `verifyTimeoutMs`.
//!
//! Redaction: the request body carries candidate URLs (which can be presigned
//! asset-store URLs) and the response carries the page's original URLs. Neither
//! is logged here — the log lines name the target only.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use serde_json::Value;
use tracing::info;

use crate::mcp::app_dispatch::ArmPayloads;
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

/// A JSON number the SDK's `Number.isInteger` accepts, as a `u64`: `5000` and
/// `5000.0` alike (JavaScript has one number type, so a client may well send
/// the float spelling). A fraction, a negative or a non-number is `None`.
fn whole_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.fract() == 0.0 && (0.0..=u64::MAX as f64).contains(f))
            .map(|f| f as u64)
    })
}

fn bounded_int(body: &Value, field: &str, min: u64, max: u64) -> Result<(), String> {
    match body.get(field) {
        None => Ok(()),
        Some(v) => match whole_u64(v) {
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

/// Validate a `DELETE /control/media/override` body (`{}` or `{overrideId}`,
/// each with an optional `verifyTimeoutMs`). Mirrors the SDK rules.
pub(crate) fn validate_restore_body(body: &Value) -> Result<(), String> {
    let obj = body.as_object().ok_or("body must be a JSON object")?;
    if let Some(v) = obj.get("overrideId") {
        if !non_empty_str(v) {
            return Err("overrideId must be a non-empty string".into());
        }
    }
    bounded_int(body, "verifyTimeoutMs", 0, MAX_DECODE_TIMEOUT_MS)?;
    Ok(())
}

/// The longest page-side wait that still leaves [`DECODE_HEADROOM_MS`] inside a
/// transport budget of `budget_ms` — never zero.
fn wait_ceiling(budget_ms: u64) -> u64 {
    budget_ms.saturating_sub(DECODE_HEADROOM_MS).max(1)
}

/// The transport budget a page-side wait is fitted to, and what a caller can
/// do about a wait that does not fit it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitBudget {
    /// The direct route: the runner's own IPC envelope, sized by the
    /// `QONTINUI_TIMEOUT_UI_BRIDGE_IPC` setting.
    UiBridgeIpc(u64),
    /// The `/sdk/` wrapper: a fixed [`SDK_DISPATCH_BUDGET_MS`] that no setting
    /// changes, so its refusal must not point at one.
    SdkDispatch,
}

impl WaitBudget {
    fn ms(self) -> u64 {
        match self {
            Self::UiBridgeIpc(ms) => ms,
            Self::SdkDispatch => SDK_DISPATCH_BUDGET_MS,
        }
    }

    fn remedy(self) -> &'static str {
        match self {
            Self::UiBridgeIpc(_) => {
                "lower it, or raise the runner's UI Bridge IPC timeout \
                 (QONTINUI_TIMEOUT_UI_BRIDGE_IPC)"
            }
            Self::SdkDispatch => "lower it; the /sdk/ transport's budget is fixed",
        }
    }
}

/// What a transport arm does with an ABSENT page-side wait. See the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitFill {
    /// The body reaches the SDK engine directly (the runner's webview over IPC,
    /// or an SDK app over WebSocket): fill the wait to fit this transport.
    EngineDirect,
    /// The body reaches the app's ui-bridge server (the `/sdk/` HTTP arm),
    /// whose relay fits the wait to its own, smaller budget: leave it absent.
    AppServerFits,
}

/// Refuse a caller's `field` wait that would outlive the transport, and fill an
/// absent one with `fill_ms` when there is one. `result` names what the
/// headroom lets reach the caller. A present non-integer is left for
/// validation (run first) to answer.
fn fit_wait(
    body: &mut Value,
    field: &str,
    budget: WaitBudget,
    fill_ms: Option<u64>,
    result: &str,
) -> Result<(), String> {
    let budget_ms = budget.ms();
    let ceiling = wait_ceiling(budget_ms);
    let obj = body.as_object_mut().ok_or("body must be a JSON object")?;
    match obj.get(field).map(whole_u64) {
        Some(Some(n)) if n > ceiling => Err(format!(
            "{field} {n} would outlive the transport's {budget_ms} ms budget (at most \
             {ceiling} ms leaves room for {result} to reach you); {}",
            budget.remedy()
        )),
        Some(_) => Ok(()),
        None => {
            if let Some(fill) = fill_ms {
                obj.insert(field.into(), Value::from(fill));
            }
            Ok(())
        }
    }
}

/// Fit the decode wait inside a transport `budget`: refuse a
/// `decodeTimeoutMs` that would outlive the transport, and fill an absent one
/// when the arm is [`WaitFill::EngineDirect`].
pub(crate) fn fit_decode_budget(
    body: &mut Value,
    budget: WaitBudget,
    fill: WaitFill,
) -> Result<(), String> {
    let fill_ms = (fill == WaitFill::EngineDirect)
        .then(|| wait_ceiling(budget.ms()).min(DEFAULT_DECODE_TIMEOUT_MS));
    fit_wait(
        body,
        "decodeTimeoutMs",
        budget,
        fill_ms,
        "the typed DECODE_TIMEOUT and its rollback",
    )
}

/// Fit a restore's verify wait inside a transport `budget`: refuse
/// a `verifyTimeoutMs` that would outlive the transport, and fill an absent one
/// with the full ceiling when the arm is [`WaitFill::EngineDirect`].
///
/// The fill differs from the SDK relay's `min(10 s, ceiling)`, and that is safe
/// ONLY on an engine-direct arm, where nothing downstream holds the body to a
/// smaller budget: the engine waits `min(override's decodeTimeoutMs,
/// verifyTimeoutMs)`, so the ceiling fill only SHORTENS a wait that would not
/// fit and leaves every override applied with a wait that does fit exactly as
/// it was applied. Sent to an app's ui-bridge server instead it would exceed
/// that relay's ceiling and be refused — which is why the `/sdk/` HTTP arm does
/// not fill. It is capped at the SDK's own [`MAX_DECODE_TIMEOUT_MS`], which the
/// SDK refuses above: a raised IPC timeout on the direct route must not turn
/// every restore into an `INVALID_REQUEST`.
pub(crate) fn fit_verify_budget(
    body: &mut Value,
    budget: WaitBudget,
    fill: WaitFill,
) -> Result<(), String> {
    let fill_ms = (fill == WaitFill::EngineDirect)
        .then(|| wait_ceiling(budget.ms()).min(MAX_DECODE_TIMEOUT_MS));
    fit_wait(
        body,
        "verifyTimeoutMs",
        budget,
        fill_ms,
        "the restore's verdict",
    )
}

/// Validate + budget-fit an override body. Shared with the `/sdk/` wrapper.
pub(crate) fn prepare_override_body(
    mut body: Value,
    budget: WaitBudget,
    fill: WaitFill,
) -> Result<Value, String> {
    validate_override_body(&body)?;
    fit_decode_budget(&mut body, budget, fill)?;
    Ok(body)
}

/// Validate + budget-fit a restore body. Shared with the `/sdk/` wrapper.
pub(crate) fn prepare_restore_body(
    mut body: Value,
    budget: WaitBudget,
    fill: WaitFill,
) -> Result<Value, String> {
    validate_restore_body(&body)?;
    fit_verify_budget(&mut body, budget, fill)?;
    Ok(body)
}

type FitWait = fn(&mut Value, WaitBudget, WaitFill) -> Result<(), String>;

/// The `/sdk/` wrapper's per-arm bodies, as one [`ArmPayloads`] the dispatcher
/// selects from on the same registry read that picks the arm. Validated once;
/// the ceiling is checked on the body both arms share, and only the WebSocket
/// copy gains the fill (its second fit cannot refuse: a present value has
/// already passed the same ceiling).
fn sdk_arm_payloads(
    body: Value,
    validate: fn(&Value) -> Result<(), String>,
    fit: FitWait,
) -> Result<ArmPayloads, String> {
    validate(&body)?;
    let mut http = body;
    fit(&mut http, WaitBudget::SdkDispatch, WaitFill::AppServerFits)?;
    let mut websocket = http.clone();
    fit(
        &mut websocket,
        WaitBudget::SdkDispatch,
        WaitFill::EngineDirect,
    )?;
    Ok(ArmPayloads::per_arm(websocket, http))
}

/// `/sdk/` POST bodies: validated, a caller's `decodeTimeoutMs` held to the
/// runner's 30 s budget on both arms, an absent one filled on the WS arm only.
pub(crate) fn sdk_override_bodies(body: Value) -> Result<ArmPayloads, String> {
    sdk_arm_payloads(body, validate_override_body, fit_decode_budget)
}

/// `/sdk/` DELETE bodies: as [`sdk_override_bodies`], for `verifyTimeoutMs`.
pub(crate) fn sdk_restore_bodies(body: Value) -> Result<ArmPayloads, String> {
    sdk_arm_payloads(body, validate_restore_body, fit_verify_budget)
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
    let body = prepare_override_body(
        body,
        WaitBudget::UiBridgeIpc(get_ui_bridge_timeout_ms()),
        WaitFill::EngineDirect,
    )
    .map_err(bad_request)?;
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
    let body = prepare_restore_body(
        body,
        WaitBudget::UiBridgeIpc(get_ui_bridge_timeout_ms()),
        WaitFill::EngineDirect,
    )
    .map_err(bad_request)?;
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
    use crate::mcp::app_registry::AppTransport;
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
        let body = prepare_override_body(
            json!({"slotId": "a", "src": "x"}),
            WaitBudget::UiBridgeIpc(10_000),
            WaitFill::EngineDirect,
        )
        .unwrap();
        assert_eq!(body["decodeTimeoutMs"], json!(8_000));
        // A roomy budget keeps the SDK's own default.
        let body = prepare_override_body(
            json!({"slotId": "a", "src": "x"}),
            WaitBudget::SdkDispatch,
            WaitFill::EngineDirect,
        )
        .unwrap();
        assert_eq!(body["decodeTimeoutMs"], json!(DEFAULT_DECODE_TIMEOUT_MS));
        // A tiny budget still leaves a positive wait rather than zero.
        let body = prepare_override_body(
            json!({"slotId": "a", "src": "x"}),
            WaitBudget::UiBridgeIpc(1_500),
            WaitFill::EngineDirect,
        )
        .unwrap();
        assert_eq!(body["decodeTimeoutMs"], json!(1));
    }

    #[test]
    fn a_decode_wait_that_would_outlive_the_transport_is_refused_naming_the_budget() {
        let err = prepare_override_body(
            json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 9_000}),
            WaitBudget::UiBridgeIpc(10_000),
            WaitFill::EngineDirect,
        )
        .unwrap_err();
        assert!(err.contains("10000 ms budget"), "{err}");
        assert!(prepare_override_body(
            json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 8_000}),
            WaitBudget::UiBridgeIpc(10_000),
            WaitFill::EngineDirect
        )
        .is_ok());
    }

    #[test]
    fn preparing_an_override_body_validates_it_first() {
        // Every case is a JSON OBJECT the budget fit alone would accept, so
        // only the validation step inside `prepare_override_body` refuses it.
        let cases = [
            (
                json!({"slotId": "a"}),
                "one of src, srcset, sources or backgroundImage",
            ),
            (
                json!({"src": "/c.png"}),
                "exactly one of slotId or elementId",
            ),
            (
                json!({"slotId": "a", "src": "x", "decodeTimeoutMs": -5}),
                "decodeTimeoutMs must be an integer",
            ),
        ];
        for (body, needle) in cases {
            let err = prepare_override_body(
                body.clone(),
                WaitBudget::SdkDispatch,
                WaitFill::EngineDirect,
            )
            .expect_err(&body.to_string());
            assert!(err.contains(needle), "{body}: {err} lacks {needle}");
        }
    }

    #[test]
    fn restore_bounds_its_verify_wait_like_the_sdk() {
        assert!(validate_restore_body(&json!({"verifyTimeoutMs": 0})).is_ok());
        assert!(validate_restore_body(&json!({"verifyTimeoutMs": 60_000})).is_ok());
        for bad in [json!(60_001), json!(-1), json!("5000"), json!(1.5)] {
            let err = validate_restore_body(&json!({ "verifyTimeoutMs": bad })).unwrap_err();
            assert!(err.contains("verifyTimeoutMs must be an integer"), "{err}");
        }
    }

    #[test]
    fn an_absent_verify_wait_is_fitted_inside_the_sdk_dispatch_budget() {
        // The /sdk/ DELETE: an override applied with a 60 s decode wait (the
        // injected runtime allows it) would otherwise be restored with a 60 s
        // verify wait over a 30 s transport.
        let body = prepare_restore_body(json!({}), WaitBudget::SdkDispatch, WaitFill::EngineDirect)
            .unwrap();
        assert_eq!(
            body["verifyTimeoutMs"],
            json!(SDK_DISPATCH_BUDGET_MS - DECODE_HEADROOM_MS)
        );
        assert!(body["verifyTimeoutMs"].as_u64().unwrap() < SDK_DISPATCH_BUDGET_MS);
        let body = prepare_restore_body(
            json!({"overrideId": "mo-1"}),
            WaitBudget::SdkDispatch,
            WaitFill::EngineDirect,
        )
        .unwrap();
        assert_eq!(body["overrideId"], json!("mo-1"));
        assert_eq!(body["verifyTimeoutMs"], json!(28_000));
        // A tiny budget still leaves a positive wait rather than zero.
        let body = prepare_restore_body(
            json!({}),
            WaitBudget::UiBridgeIpc(1_500),
            WaitFill::EngineDirect,
        )
        .unwrap();
        assert_eq!(body["verifyTimeoutMs"], json!(1));
        // A raised IPC timeout never fills past the SDK's own bound, which the
        // SDK refuses — and the filled body still passes the runner's mirror.
        let body = prepare_restore_body(
            json!({}),
            WaitBudget::UiBridgeIpc(120_000),
            WaitFill::EngineDirect,
        )
        .unwrap();
        assert_eq!(body["verifyTimeoutMs"], json!(MAX_DECODE_TIMEOUT_MS));
        assert!(validate_restore_body(&body).is_ok());
    }

    #[test]
    fn a_verify_wait_that_would_outlive_the_transport_is_refused_naming_the_budget() {
        let err = prepare_restore_body(
            json!({"verifyTimeoutMs": 28_001}),
            WaitBudget::SdkDispatch,
            WaitFill::EngineDirect,
        )
        .unwrap_err();
        assert!(err.contains("verifyTimeoutMs 28001"), "{err}");
        assert!(err.contains("30000 ms budget"), "{err}");
        // A caller's wait that fits is kept, not replaced by the fill.
        let body = prepare_restore_body(
            json!({"verifyTimeoutMs": 5_000}),
            WaitBudget::SdkDispatch,
            WaitFill::EngineDirect,
        )
        .unwrap();
        assert_eq!(body["verifyTimeoutMs"], json!(5_000));
        // Validation runs before the fit: a malformed body is refused, not filled.
        assert!(prepare_restore_body(
            json!({"overrideId": ""}),
            WaitBudget::SdkDispatch,
            WaitFill::EngineDirect
        )
        .unwrap_err()
        .contains("overrideId"));
    }

    /// The body each transport is sent, read the way the dispatcher reads it
    /// ([`ArmPayloads::for_transport`]) — so a swap of the two arms' bodies
    /// anywhere between here and the dispatcher reds the tests below.
    fn sent_to(payloads: &ArmPayloads, transport: AppTransport) -> Value {
        payloads.clone().for_transport(transport)
    }

    #[test]
    fn an_http_registered_app_is_sent_no_wait_for_its_server_to_fit() {
        // The app's ui-bridge server fits these to ITS relay budget (a 6 s
        // ceiling by default) and refuses more — so a runner fill sized to
        // the runner's 30 s would refuse every request on an HTTP app.
        let apply = sdk_override_bodies(json!({"slotId": "a", "src": "x"})).unwrap();
        let http = sent_to(&apply, AppTransport::Http);
        assert_eq!(http, json!({"slotId": "a", "src": "x"}));
        assert!(http.get("decodeTimeoutMs").is_none());
        let restore = sdk_restore_bodies(json!({"overrideId": "mo-1"})).unwrap();
        let http = sent_to(&restore, AppTransport::Http);
        assert_eq!(http, json!({"overrideId": "mo-1"}));
        assert!(http.get("verifyTimeoutMs").is_none());
        let restore_all = sdk_restore_bodies(json!({})).unwrap();
        assert_eq!(sent_to(&restore_all, AppTransport::Http), json!({}));
    }

    #[test]
    fn a_websocket_registered_app_is_sent_the_filled_wait_engine_direct() {
        let apply = sdk_override_bodies(json!({"slotId": "a", "src": "x"})).unwrap();
        assert_eq!(
            sent_to(&apply, AppTransport::Websocket)["decodeTimeoutMs"],
            json!(DEFAULT_DECODE_TIMEOUT_MS)
        );
        let restore = sdk_restore_bodies(json!({})).unwrap();
        assert_eq!(
            sent_to(&restore, AppTransport::Websocket)["verifyTimeoutMs"],
            json!(SDK_DISPATCH_BUDGET_MS - DECODE_HEADROOM_MS)
        );
    }

    #[test]
    fn a_refusal_names_a_remedy_that_works_on_its_transport() {
        // /sdk/: the budget is fixed, so the refusal must not send the caller
        // to an IPC setting that has no effect there.
        let err = sdk_restore_bodies(json!({"verifyTimeoutMs": 28_001})).unwrap_err();
        assert!(err.contains("budget is fixed"), "{err}");
        assert!(!err.contains("IPC"), "{err}");
        // The direct route: the IPC timeout is the budget, so raising it helps.
        let err = prepare_override_body(
            json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 9_000}),
            WaitBudget::UiBridgeIpc(10_000),
            WaitFill::EngineDirect,
        )
        .unwrap_err();
        assert!(err.contains("QONTINUI_TIMEOUT_UI_BRIDGE_IPC"), "{err}");
    }

    #[test]
    fn the_app_server_arm_refuses_an_over_budget_wait_on_its_own() {
        // Pin the HTTP arm's refusal directly, not only through
        // `sdk_arm_payloads`: leaving a wait absent must not also stop
        // refusing an oversized one.
        let err = prepare_override_body(
            json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 28_001}),
            WaitBudget::SdkDispatch,
            WaitFill::AppServerFits,
        )
        .unwrap_err();
        assert!(err.contains("decodeTimeoutMs 28001"), "{err}");
        let err = prepare_restore_body(
            json!({"verifyTimeoutMs": 28_001.0}),
            WaitBudget::SdkDispatch,
            WaitFill::AppServerFits,
        )
        .unwrap_err();
        assert!(err.contains("verifyTimeoutMs 28001"), "{err}");
        // And it validates: a malformed body is refused, not passed through.
        assert!(prepare_restore_body(
            json!({"overrideId": ""}),
            WaitBudget::SdkDispatch,
            WaitFill::AppServerFits
        )
        .is_err());
    }

    #[test]
    fn the_sdk_arms_both_refuse_and_both_keep_a_callers_wait() {
        // Over the runner's own ceiling: refused before either arm is built.
        let err =
            sdk_override_bodies(json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 28_001}))
                .unwrap_err();
        assert!(err.contains("30000 ms budget"), "{err}");
        let err = sdk_restore_bodies(json!({"verifyTimeoutMs": 28_001})).unwrap_err();
        assert!(err.contains("30000 ms budget"), "{err}");
        // Invalid: refused on both arms too.
        assert!(sdk_override_bodies(json!({"slotId": "a"})).is_err());
        assert!(sdk_restore_bodies(json!({"overrideId": ""})).is_err());
        // A caller's wait that fits is passed through unchanged on both arms.
        let b = sdk_override_bodies(json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 4_000}))
            .unwrap();
        for transport in [AppTransport::Websocket, AppTransport::Http] {
            assert_eq!(sent_to(&b, transport)["decodeTimeoutMs"], json!(4_000));
        }
        let b = sdk_restore_bodies(json!({"verifyTimeoutMs": 4_000})).unwrap();
        for transport in [AppTransport::Websocket, AppTransport::Http] {
            assert_eq!(sent_to(&b, transport)["verifyTimeoutMs"], json!(4_000));
        }
    }

    #[test]
    fn a_whole_number_float_is_an_integer_as_in_the_sdk() {
        // The SDK checks `Number.isInteger`, which accepts 5000.0.
        let body = json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 5000.0, "ttlMs": 60000.0});
        assert!(validate_override_body(&body).is_ok());
        assert!(validate_restore_body(&json!({"verifyTimeoutMs": 5000.0})).is_ok());
        assert!(validate_restore_body(&json!({"verifyTimeoutMs": 0.0})).is_ok());
        for bad in [json!(5000.5), json!(-1.0), json!(60_001.0)] {
            let err = validate_restore_body(&json!({ "verifyTimeoutMs": bad })).unwrap_err();
            assert!(err.contains("must be an integer"), "{err}");
        }
        // The budget check sees the float spelling too, rather than waving it through.
        let err = sdk_restore_bodies(json!({"verifyTimeoutMs": 29_000.0})).unwrap_err();
        assert!(err.contains("30000 ms budget"), "{err}");
        let err =
            sdk_override_bodies(json!({"slotId": "a", "src": "x", "decodeTimeoutMs": 29_000.0}))
                .unwrap_err();
        assert!(err.contains("30000 ms budget"), "{err}");
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
