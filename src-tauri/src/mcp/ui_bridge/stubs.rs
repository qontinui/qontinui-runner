//! F2 — Network-stub registry HTTP handlers.
//!
//! Routes live under `/ui-bridge/control/network/stubs*`. Rust does body
//! validation here, then forwards to the React SDK via
//! `ui_bridge_request_sync` (actions: `register_network_stub`,
//! `list_network_stubs`, `delete_network_stub`, `clear_network_stubs`).
//!
//! Keep this file focused on HTTP/IPC plumbing + validation. The actual
//! match semantics (FIFO, `times` accounting) live SDK-side in
//! `packages/ui-bridge/src/network/stubs.ts`.
//!
//! Design alignment: the validator mirrors
//! `validateStubRequest` on the SDK side so a request rejected by the Rust
//! handler would have been rejected by the SDK anyway. Both layers share
//! the same contract; the Rust copy exists for early 400s + unit tests.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;

// ============================================================================
// Request types
// ============================================================================

/// Method filter for a stub. Accepts the HTTP verbs we support plus the
/// wildcard `"*"`. Matches the SDK's `StubMethod` exactly.
fn is_valid_method(m: &str) -> bool {
    matches!(m, "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "*")
}

/// Response half of the stub body.
///
/// Exactly one of `body` / `body_json` must be set — enforced by
/// `StubRequest::validate` rather than serde so we emit a nicer error.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StubResponse {
    #[serde(default)]
    pub status: Option<i64>,
    #[serde(default)]
    pub headers: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub body_json: Option<serde_json::Value>,
}

/// Top-level stub registration request.
///
/// Deserializes permissively (any string in `method`) so we can return a
/// pointed 400 with the actual offending value instead of a raw serde
/// rejection. The SDK-side validator is the source of truth; this copy
/// catches obvious errors before we pay the IPC round-trip.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StubRequest {
    pub url_pattern: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
    pub response: Option<StubResponse>,
    #[serde(default)]
    pub times: Option<serde_json::Value>,
}

impl StubRequest {
    /// Validate the payload. Returns `Ok(())` if valid, else a human-readable
    /// error string suitable for a 400 body.
    pub fn validate(&self) -> Result<(), String> {
        let pattern = self
            .url_pattern
            .as_deref()
            .ok_or("urlPattern is required")?;
        if pattern.is_empty() {
            return Err("urlPattern must be non-empty".into());
        }

        if let Some(m) = self.method.as_deref() {
            if !is_valid_method(m) {
                return Err(format!(
                    "method must be one of GET|POST|PUT|DELETE|PATCH|*, got \"{}\"",
                    m
                ));
            }
        }

        let response = self.response.as_ref().ok_or("response is required")?;

        if let Some(status) = response.status {
            if !(100..=599).contains(&status) {
                return Err(format!("status must be in 100-599, got {}", status));
            }
        }

        let has_body = response.body.is_some();
        let has_body_json = response.body_json.is_some();
        match (has_body, has_body_json) {
            (false, false) => {
                return Err("exactly one of response.body or response.bodyJson is required".into())
            }
            (true, true) => {
                return Err("response.body and response.bodyJson are mutually exclusive".into())
            }
            _ => {}
        }

        if let Some(times) = &self.times {
            let ok = match times {
                serde_json::Value::String(s) => s == "always",
                serde_json::Value::Number(n) => n.as_i64().is_some_and(|i| i >= 1),
                _ => false,
            };
            if !ok {
                return Err(format!(
                    "times must be \"always\" or a positive integer, got {}",
                    times
                ));
            }
        }

        Ok(())
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// POST /control/network/stubs — register a new stub.
pub async fn ui_bridge_register_network_stub_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Register network stub");

    // Deserialize with the tolerant struct so we can emit a structured 400.
    let req: StubRequest = match serde_json::from_value(body.clone()) {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("invalid stub body: {}", e))),
            ));
        }
    };
    if let Err(msg) = req.validate() {
        return Err((StatusCode::BAD_REQUEST, Json(api_error(msg))));
    }

    match ui_bridge_request_sync(&state, "register_network_stub", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// GET /control/network/stubs — list registered stubs.
pub async fn ui_bridge_list_network_stubs_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: List network stubs");

    match ui_bridge_request_sync(&state, "list_network_stubs", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// DELETE /control/network/stubs/:id — delete a specific stub.
pub async fn ui_bridge_delete_network_stub_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Delete network stub {}", id);

    let payload = serde_json::json!({ "id": id });
    match ui_bridge_request_sync(&state, "delete_network_stub", payload).await {
        Ok(data) => {
            // The SDK-side handler returns { success, code? } — preserve the
            // NOT_FOUND signal by surfacing 404 when appropriate.
            if let Some(code) = data.get("code").and_then(|c| c.as_str()) {
                if code == "NOT_FOUND" {
                    return Err((
                        StatusCode::NOT_FOUND,
                        Json(api_error(format!("stub {} not found", id))),
                    ));
                }
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// DELETE /control/network/stubs — clear every stub.
pub async fn ui_bridge_clear_network_stubs_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Clear all network stubs");

    match ui_bridge_request_sync(&state, "clear_network_stubs", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// N3 — Non-consuming stub verification
// ============================================================================
//
// Separate request type from `StubRequest` — this endpoint does NOT register
// anything, it just probes the existing registry. The wire payload is a
// trimmed `{urlPattern, method?}` pair with the same camelCase convention as
// the register side.

/// Request body for `POST /control/network/verify-stub`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyStubRequest {
    pub url_pattern: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
}

impl VerifyStubRequest {
    /// Validate. Mirrors `StubRequest::validate` but is much narrower — the
    /// response shape doesn't exist on verify and `times` doesn't apply.
    pub fn validate(&self) -> Result<(), String> {
        let pattern = self
            .url_pattern
            .as_deref()
            .ok_or("urlPattern is required")?;
        if pattern.is_empty() {
            return Err("urlPattern must be non-empty".into());
        }
        if let Some(m) = self.method.as_deref() {
            if !is_valid_method(m) {
                return Err(format!(
                    "method must be one of GET|POST|PUT|DELETE|PATCH|*, got \"{}\"",
                    m
                ));
            }
        }
        Ok(())
    }
}

/// POST /control/network/verify-stub — non-consuming probe.
///
/// Asks the SDK registry "would this URL+method get stubbed right now?"
/// WITHOUT decrementing `times: 1` entries or bumping `hitCount`. Returns
/// the hypothetical response (status/headers/body) + the matched stub's
/// registry entry snapshot on hit; on miss returns `{matched: false, ...}`
/// with null fields.
pub async fn ui_bridge_verify_network_stub_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Verify network stub");

    let req: VerifyStubRequest = match serde_json::from_value(body.clone()) {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("invalid verify-stub body: {}", e))),
            ));
        }
    };
    if let Err(msg) = req.validate() {
        return Err((StatusCode::BAD_REQUEST, Json(api_error(msg))));
    }

    match ui_bridge_request_sync(&state, "verify_network_stub", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Route registration
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/network/stubs",
            post(ui_bridge_register_network_stub_handler)
                .get(ui_bridge_list_network_stubs_handler)
                .delete(ui_bridge_clear_network_stubs_handler),
        )
        .route(
            "/ui-bridge/control/network/stubs/{id}",
            delete(ui_bridge_delete_network_stub_handler),
        )
        .route(
            "/ui-bridge/control/network/verify-stub",
            post(ui_bridge_verify_network_stub_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/control/network/stubs"),
        ("GET", "/ui-bridge/control/network/stubs"),
        ("DELETE", "/ui-bridge/control/network/stubs"),
        ("DELETE", "/ui-bridge/control/network/stubs/{id}"),
        ("POST", "/ui-bridge/control/network/verify-stub"),
    ]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod stub_request_tests {
    //! Deserialization + validation tests for `StubRequest` / `StubResponse`.
    //! Follows the pattern established by `page_navigate_mode_tests`.

    use super::{StubRequest, StubResponse};

    fn parse(body: &str) -> StubRequest {
        serde_json::from_str(body).expect("StubRequest must deserialize")
    }

    #[test]
    fn valid_minimal_body_passes() {
        let req = parse(r#"{"urlPattern":"/foo","response":{"body":"hi"}}"#);
        req.validate().expect("minimal body is valid");
        assert_eq!(req.url_pattern.as_deref(), Some("/foo"));
    }

    #[test]
    fn body_json_snake_case_deserialization() {
        // Because of `rename_all = "camelCase"`, the wire key is `bodyJson`.
        let req = parse(r#"{"urlPattern":"/foo","response":{"bodyJson":{"a":1}}}"#);
        req.validate().expect("bodyJson body is valid");
        let resp: &StubResponse = req.response.as_ref().expect("response");
        assert!(resp.body_json.is_some());
        assert!(resp.body.is_none());
    }

    #[test]
    fn missing_url_pattern_is_rejected() {
        let req = parse(r#"{"response":{"body":"hi"}}"#);
        assert!(req.validate().unwrap_err().contains("urlPattern"));
    }

    #[test]
    fn empty_url_pattern_is_rejected() {
        let req = parse(r#"{"urlPattern":"","response":{"body":"hi"}}"#);
        assert!(req.validate().unwrap_err().contains("non-empty"));
    }

    #[test]
    fn unknown_method_is_rejected() {
        let req = parse(r#"{"urlPattern":"/f","method":"HEAD","response":{"body":"hi"}}"#);
        assert!(req.validate().unwrap_err().contains("method"));
    }

    #[test]
    fn wildcard_method_is_accepted() {
        let req = parse(r#"{"urlPattern":"/f","method":"*","response":{"body":"hi"}}"#);
        req.validate().expect("wildcard method valid");
    }

    #[test]
    fn missing_body_and_body_json_is_rejected() {
        let req = parse(r#"{"urlPattern":"/f","response":{}}"#);
        assert!(req.validate().unwrap_err().contains("bodyJson"));
    }

    #[test]
    fn both_body_and_body_json_is_rejected() {
        let req = parse(r#"{"urlPattern":"/f","response":{"body":"x","bodyJson":{"a":1}}}"#);
        assert!(req.validate().unwrap_err().contains("mutually exclusive"));
    }

    #[test]
    fn status_out_of_range_is_rejected() {
        let req = parse(r#"{"urlPattern":"/f","response":{"body":"x","status":99}}"#);
        assert!(req.validate().unwrap_err().contains("100-599"));
        let req = parse(r#"{"urlPattern":"/f","response":{"body":"x","status":600}}"#);
        assert!(req.validate().unwrap_err().contains("100-599"));
    }

    #[test]
    fn times_always_is_accepted() {
        let req = parse(r#"{"urlPattern":"/f","response":{"body":"x"},"times":"always"}"#);
        req.validate().expect("times always valid");
    }

    #[test]
    fn times_positive_int_is_accepted() {
        let req = parse(r#"{"urlPattern":"/f","response":{"body":"x"},"times":3}"#);
        req.validate().expect("times 3 valid");
    }

    #[test]
    fn times_zero_is_rejected() {
        let req = parse(r#"{"urlPattern":"/f","response":{"body":"x"},"times":0}"#);
        assert!(req.validate().unwrap_err().contains("positive integer"));
    }

    #[test]
    fn times_negative_is_rejected() {
        let req = parse(r#"{"urlPattern":"/f","response":{"body":"x"},"times":-5}"#);
        assert!(req.validate().unwrap_err().contains("positive integer"));
    }

    #[test]
    fn times_unknown_string_is_rejected() {
        let req = parse(r#"{"urlPattern":"/f","response":{"body":"x"},"times":"once"}"#);
        assert!(req.validate().unwrap_err().contains("always"));
    }

    #[test]
    fn response_headers_deserialize_as_map() {
        let req =
            parse(r#"{"urlPattern":"/f","response":{"body":"x","headers":{"x-a":"1","x-b":"2"}}}"#);
        req.validate().expect("valid");
        let headers = req.response.unwrap().headers.unwrap();
        assert_eq!(headers.get("x-a").unwrap().as_str(), Some("1"));
        assert_eq!(headers.get("x-b").unwrap().as_str(), Some("2"));
    }
}

#[cfg(test)]
mod verify_stub_request_tests {
    //! N3: serde + validation tests for `VerifyStubRequest`.

    use super::VerifyStubRequest;

    fn parse(body: &str) -> VerifyStubRequest {
        serde_json::from_str(body).expect("VerifyStubRequest must deserialize")
    }

    #[test]
    fn minimal_valid_body() {
        let req = parse(r#"{"urlPattern":"/foo"}"#);
        req.validate().expect("valid");
        assert_eq!(req.url_pattern.as_deref(), Some("/foo"));
        assert!(req.method.is_none());
    }

    #[test]
    fn camel_case_wire_key() {
        // `url_pattern` in Rust is `urlPattern` on the wire.
        let req = parse(r#"{"urlPattern":"/foo","method":"POST"}"#);
        assert_eq!(req.url_pattern.as_deref(), Some("/foo"));
        assert_eq!(req.method.as_deref(), Some("POST"));
    }

    #[test]
    fn missing_url_pattern_is_rejected() {
        let req = parse(r#"{"method":"GET"}"#);
        assert!(req.validate().unwrap_err().contains("urlPattern"));
    }

    #[test]
    fn empty_url_pattern_is_rejected() {
        let req = parse(r#"{"urlPattern":""}"#);
        assert!(req.validate().unwrap_err().contains("non-empty"));
    }

    #[test]
    fn unknown_method_is_rejected() {
        let req = parse(r#"{"urlPattern":"/f","method":"HEAD"}"#);
        assert!(req.validate().unwrap_err().contains("method"));
    }

    #[test]
    fn wildcard_method_is_accepted() {
        let req = parse(r#"{"urlPattern":"/f","method":"*"}"#);
        req.validate().expect("wildcard valid");
    }

    #[test]
    fn absent_method_is_accepted() {
        let req = parse(r#"{"urlPattern":"/f"}"#);
        req.validate().expect("absent method valid");
    }
}
