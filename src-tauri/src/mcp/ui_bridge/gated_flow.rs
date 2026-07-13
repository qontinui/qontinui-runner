//! Gated-flow UI Bridge family — identity introspection (P3) + the observe
//! tier for credential-adjacent commands (P2).
//!
//! Two routes, both self-contained in this submodule so the `mod.rs`
//! `manifest_matches_route_calls` drift test sees the `.route(` calls here and
//! matches them against [`route_entries`]:
//!
//! - `GET  /ui-bridge/session` — read-only identity facts (NO token material).
//!   Composed entirely from Rust-side auth accessors; no Tauri round-trip.
//! - `POST /ui-bridge/observe/{command}` — observe-only tier. Performs the SAME
//!   HTTP→Tauri round-trip as the invoke proxy, then returns ONLY the command's
//!   registered `observe_projection` of the result — never the raw payload.
//!   Orthogonal to the invoke allowlist: a command can be observe-allowed while
//!   invoke-denied (e.g. `github_list_repos`). See
//!   `crate::ui_bridge_invoke::{observe_projection_for, UI_BRIDGE_OBSERVE_COMMANDS}`.
//!
//! Both are runner-only (no SDK `UI_BRIDGE_ROUTES` analog), so they are
//! allow-listed in `runner_only_baseline` of
//! `sdk_manifest_routes_are_exposed_by_runner` in `mod.rs`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::Value;
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Format an absolute unix-seconds timestamp as an RFC3339 string, or `None`
/// if it is out of range. Used to render bearer expiries without ever touching
/// the token itself.
fn unix_to_rfc3339(secs: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.to_rfc3339())
}

/// `GET /ui-bridge/session`
///
/// Read-only identity introspection. Returns ONLY non-secret identity facts —
/// NEVER token material:
///
/// ```json
/// {
///   "signed_in": bool,
///   "tenant_id": string | null,
///   "cognito_client_id": string | null,
///   "bearer_kind": "access" | "id" | "device" | null,
///   "bearer_expires_at": string | null,
///   "api_base_url": string
/// }
/// ```
///
/// Field provenance:
/// - `signed_in` — `crate::commands::auth::check_auth_status().authenticated`
///   (authoritative local-session check + best-effort `users/me` enrichment).
/// - `tenant_id` — `crate::session::dual_write::resolve_active_tenant_id()`
///   (a UUID read from `machine.json`; non-secret).
/// - `cognito_client_id` — the runner's fixed, non-secret Cognito app client id
///   (`qontinui_runner_lib::cognito::COGNITO_CLIENT_ID`), reported only when a
///   Cognito session is the active bearer source; `null` otherwise.
/// - `bearer_kind` — which bearer the runner presents: `"access"` when a
///   Cognito (oauth) session is present (the runner presents the Cognito ACCESS
///   token to the web backend — see `web_backend_user_bearer`), else
///   `"device"` when a valid coord device-JWT is present, else `null`. `"id"`
///   is a valid discriminant but is never selected: the Cognito id token is
///   stored but never used as a request bearer.
/// - `bearer_expires_at` — RFC3339 expiry of the selected bearer, derived from
///   stored expiries (`oauth_expires_at` / the device-JWT's unverified `exp`);
///   never exposes the token.
/// - `api_base_url` — `crate::api_config::get_api_base_url()`.
///
/// Presence checks read only expiry/refresh metadata and never a decrypted
/// token, and deliberately do NOT trigger a token refresh (no side effects).
pub async fn ui_bridge_session_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Value>> {
    info!("UI Bridge API: session (identity introspection)");

    let auth_manager = crate::auth::AuthManager::new();

    // signed_in — authoritative via check_auth_status. Any error (which the
    // command only returns on a transient validation failure) is treated as
    // not-signed-in for this read-only surface.
    let signed_in = match crate::commands::auth::check_auth_status().await {
        Ok(status) => status.authenticated,
        Err(_) => false,
    };

    // tenant_id — non-secret UUID from machine.json (or null).
    let tenant_id = crate::session::dual_write::resolve_active_tenant_id().map(|t| t.to_string());

    // Credential presence — derived WITHOUT reading token material or
    // triggering a refresh. A non-empty stored oauth refresh token is the same
    // signal `refresh_cognito_bearer` keys on for "a Cognito session exists".
    let has_cognito_session = auth_manager
        .get_oauth_refresh_token()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false);
    // A valid (unexpired) coord device-JWT sits in the access_token slot when
    // `device_jwt_needs_refresh()` reports `Ok(false)`.
    let has_device_jwt = matches!(auth_manager.device_jwt_needs_refresh(), Ok(false));

    let bearer_kind: Option<&str> = if has_cognito_session {
        Some("access")
    } else if has_device_jwt {
        Some("device")
    } else {
        None
    };

    let bearer_expires_at = match bearer_kind {
        Some("access") => auth_manager
            .get_oauth_expires_at()
            .and_then(unix_to_rfc3339),
        Some("device") => auth_manager.access_token_exp().and_then(unix_to_rfc3339),
        _ => None,
    };

    // The client id is a compile-time constant identifier, not a credential.
    // Reported only when a Cognito session is the active bearer source.
    let cognito_client_id = if has_cognito_session {
        Some(qontinui_runner_lib::cognito::COGNITO_CLIENT_ID)
    } else {
        None
    };

    let api_base_url = crate::api_config::get_api_base_url();

    Json(ApiResponse::success(serde_json::json!({
        "signed_in": signed_in,
        "tenant_id": tenant_id,
        "cognito_client_id": cognito_client_id,
        "bearer_kind": bearer_kind,
        "bearer_expires_at": bearer_expires_at,
        "api_base_url": api_base_url,
    })))
}

/// `POST /ui-bridge/observe/{command}`
///
/// Observe-only tier for credential-adjacent commands. The gate is orthogonal
/// to the invoke allowlist: this is allowed iff the command has a registered
/// `observe_projection` (see `crate::ui_bridge_invoke::observe_projection_for`)
/// — it does NOT consult `is_allowlisted`, and it never widens the invoke
/// surface (`github_list_repos` stays invoke-denied).
///
/// Performs the SAME HTTP→Tauri round-trip as
/// `ui_bridge_invoke_handler`, then applies the command's projection to the raw
/// result and returns ONLY the projection in the `ApiResponse::success`
/// envelope. On invoke error, returns the same error envelope the invoke
/// handler returns (the raw payload is never leaked).
pub async fn ui_bridge_observe_handler(
    State(state): State<Arc<ApiState>>,
    Path(command): Path<String>,
    Query(q): Query<crate::mcp::ui_bridge_invoke_handlers::InvokeTimeoutQuery>,
    Json(body): Json<crate::mcp::ui_bridge_invoke_handlers::InvokeRequestBody>,
) -> Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Observe-tier gate — orthogonal to `is_allowlisted`.
    let Some(projection) = crate::ui_bridge_invoke::observe_projection_for(&command) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "command '{}' is not observe-allowed — no observe projection is registered for it",
                command
            ))),
        ));
    };

    let timeout_ms = q
        .timeout_ms
        .unwrap_or(crate::mcp::ui_bridge_invoke_handlers::DEFAULT_INVOKE_TIMEOUT_MS);

    // P5 — snapshot the trace sequence BEFORE the round-trip so we only collect
    // outbound calls this invocation caused (there is no request id threaded
    // across the IPC boundary to correlate on).
    let trace_seq = crate::outbound_trace::current_seq();

    // Same round-trip as invoke; error envelope is returned verbatim (`?`).
    let raw = crate::mcp::ui_bridge_invoke_handlers::perform_invoke_round_trip(
        &state, &command, &body.args, timeout_ms,
    )
    .await?;

    // Return ONLY the projection — the raw command payload never leaves here.
    let mut observed = projection(&raw);

    // P5 — append the redacted outbound trace, opt-in per command. Additive:
    // the projection's own keys are unchanged, so the P2 contract still holds.
    if crate::ui_bridge_invoke::traces_outbound(&command) {
        let traces = crate::outbound_trace::drain_since(&command, trace_seq);
        if let Some(obj) = observed.as_object_mut() {
            obj.insert(
                "outbound_trace".to_string(),
                serde_json::to_value(&traces).unwrap_or(Value::Null),
            );
        }
    }

    Ok(Json(ApiResponse::success(observed)))
}

/// Gated-flow routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/ui-bridge/session", get(ui_bridge_session_handler))
        .route(
            "/ui-bridge/observe/{command}",
            post(ui_bridge_observe_handler),
        )
}

/// Static (method, path) tuples mirroring [`routes`]. Concatenated into
/// `route_manifest()` in `mod.rs`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/session"),
        ("POST", "/ui-bridge/observe/{command}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_entries_match_route_count() {
        // Keep in lockstep with the `.route(` calls in `routes()`.
        assert_eq!(route_entries().len(), 2);
        for (method, path) in route_entries() {
            assert!(path.starts_with("/ui-bridge/"));
            assert!(matches!(*method, "GET" | "POST"));
        }
    }

    #[test]
    fn unix_to_rfc3339_formats_and_rejects_out_of_range() {
        // 2021-01-01T00:00:00Z
        let s = unix_to_rfc3339(1_609_459_200).expect("in-range timestamp formats");
        assert!(s.starts_with("2021-01-01T00:00:00"));
        // i64::MAX seconds is out of chrono's representable range → None.
        assert!(unix_to_rfc3339(i64::MAX).is_none());
    }

    #[test]
    fn observe_gate_uses_projection_registry_not_invoke_allowlist() {
        // github_list_repos is observe-allowed (Some projection) and
        // invoke-denied (not in the invoke allowlist) — the observe handler
        // gates on the former.
        assert!(crate::ui_bridge_invoke::observe_projection_for("github_list_repos").is_some());
        assert!(!crate::ui_bridge_invoke::is_allowlisted(
            "github_list_repos"
        ));
    }
}
