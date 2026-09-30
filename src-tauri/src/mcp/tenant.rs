//! Headless door to this device's tenant pin — `GET`/`PUT /tenant/active`.
//!
//! Plan `2026-09-23-remote-create-residuals-after-coord-registration-confirm`
//! Phase 4. The pin (`~/.qontinui/machine.json::active_tenant_id`, read by
//! `tenant_pin::resolve_tenant_pin`) used to have exactly one writer, the
//! `set_active_tenant` Tauri command — reachable from the desktop UI only, so
//! a headless runner could be pinned only by hand-editing `machine.json`.
//!
//! These handlers are thin: both call the SAME functions the Tauri commands
//! call (`commands::tenant::{active_tenant_view, apply_active_tenant}`), so
//! the validation (UUID, device must hold a binding for the tenant) and the
//! identity-preserving atomic write cannot drift between the two doors.
//!
//! ## Origin policy
//!
//! `/tenant/active` (every method) is on
//! `mcp::origin_guard::CREDENTIAL_DOORS`: a `PUT` re-points which tenant this
//! device's NEW sessions and device-level surfaces write into, which is a
//! credential-selection change. So no browser origin other than the runner's
//! own webview reaches it, under every route policy, and it is NOT on
//! `TRUSTED_DOOR_GRACE` — the qontinui-web dev frontend is refused too. A
//! loopback agent, script or curl (`OriginClass::NonBrowser`) reaches it
//! unchanged. It is also absent from `relay_path_policy::RELAY_ALLOWED`, so
//! the backend relay's `http_request` arm cannot reach it from off-box.

use axum::{http::StatusCode, response::Json, routing::get, Router};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::commands::tenant::{
    active_tenant_view, applied_payload, apply_active_tenant, current_machine_pin,
    ActiveTenantView, SetActiveTenantError,
};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

type HandlerError = (StatusCode, Json<ApiResponse<()>>);

/// `PUT /tenant/active` body.
#[derive(Debug, Deserialize)]
struct PutActiveTenant {
    tenant_id: String,
}

/// GET /tenant/active
///
/// `{ active_tenant_id, source, pin, candidates }` — the same view the
/// `get_active_tenant` Tauri command returns.
async fn get_active_tenant() -> Result<Json<ApiResponse<ActiveTenantView>>, HandlerError> {
    let view = spawn_blocking_tracked(active_tenant_view)
        .await
        .map_err(|e| {
            error!("tenant: GET /tenant/active task failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {e}"))),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    Ok(Json(ApiResponse::success(view)))
}

/// The HTTP status for each refusal. Caller mistakes are 4xx; a write the
/// caller was entitled to that then failed on I/O is a 500.
fn status_for(err: &SetActiveTenantError) -> StatusCode {
    match err {
        SetActiveTenantError::Empty | SetActiveTenantError::Malformed(_) => StatusCode::BAD_REQUEST,
        SetActiveTenantError::NotBound { .. } => StatusCode::FORBIDDEN,
        // Device state, not the request, prevents the write.
        SetActiveTenantError::NoBindings | SetActiveTenantError::WriteRefused(_) => {
            StatusCode::CONFLICT
        }
        SetActiveTenantError::NoHome | SetActiveTenantError::WriteFailed(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// PUT /tenant/active  `{"tenant_id": "<uuid>"}`
///
/// Pins this device's default tenant. Refuses — `machine.json` unchanged —
/// a blank or non-UUID id (400 `INVALID_TENANT_ID`), a tenant this device
/// holds no binding for (403 `TENANT_NOT_BOUND`), a device with no binding at
/// all (409 `NO_TENANT_BINDINGS`), and a `machine.json` with no identity to
/// preserve (409 `MACHINE_JSON_WRITE_REFUSED`).
///
/// On success `data.takes_effect` is `"live"` and `data.takes_effect_detail`
/// says what that covers: new sessions and device-level surfaces on their next
/// read; sessions that already exist keep their recorded tenant
/// (`data.existing_sessions: "unchanged"`).
async fn put_active_tenant(
    Json(body): Json<PutActiveTenant>,
) -> Result<Json<ApiResponse<serde_json::Value>>, HandlerError> {
    let result = spawn_blocking_tracked(move || {
        let previous = current_machine_pin();
        apply_active_tenant(&body.tenant_id).map(|written| (written, previous))
    })
    .await
    .map_err(|e| {
        error!("tenant: PUT /tenant/active task failed: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {e}"))),
        )
    })?;

    match result {
        Ok((written, previous)) => {
            info!(
                active_tenant_id = %written,
                previous = ?previous,
                "tenant: active tenant pinned via PUT /tenant/active"
            );
            Ok(Json(ApiResponse::success(applied_payload(
                &written, previous,
            ))))
        }
        Err(e) => {
            warn!(code = e.code(), "tenant: PUT /tenant/active refused: {e}");
            Err((
                status_for(&e),
                Json(ApiResponse::error_with_code(e.to_string(), e.code())),
            ))
        }
    }
}

/// Generic over the router state so tests can serve it without an
/// [`ApiState`]; the handlers take no state.
fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route(
        "/tenant/active",
        get(get_active_tenant).put(put_active_tenant),
    )
}

pub fn routes() -> Router<Arc<ApiState>> {
    router()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use qontinui_runner_lib::ambient::test_support::IsolatedAmbient;
    use tower::ServiceExt;

    const TENANT_A: &str = "aaaaaaaa-0000-4000-8000-00000000000a";
    const TENANT_B: &str = "bbbbbbbb-0000-4000-8000-00000000000b";
    const UNBOUND: &str = "cccccccc-0000-4000-8000-00000000000c";

    /// A fixture machine with an identity, pinned to A, bound to A and B.
    fn fixture() -> (IsolatedAmbient, std::path::PathBuf) {
        let amb = IsolatedAmbient::new();
        let machine = amb.write_machine_json(&format!(
            r#"{{"device_id":"11111111-0000-4000-8000-000000000001","hostname":"box","active_tenant_id":"{TENANT_A}"}}"#
        ));
        // `IsolatedAmbient` points QONTINUI_SECURE_STORAGE_DIR at its dir,
        // which is where `pair::paired_user_path` reads.
        std::fs::write(
            amb.dir().join("paired_user.json"),
            format!(
                r#"{{"user_id":"u","tenant_id":"{TENANT_A}","default_tenant_id":"{TENANT_A}",
                    "bindings":[{{"tenant_id":"{TENANT_A}","user_id":"u"}},
                                {{"tenant_id":"{TENANT_B}","user_id":"u"}}]}}"#
            ),
        )
        .unwrap();
        (amb, machine)
    }

    async fn call(req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let resp = router::<()>().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    fn get_req() -> Request<Body> {
        Request::builder()
            .uri("/tenant/active")
            .body(Body::empty())
            .unwrap()
    }

    fn put_req(tenant: &str) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri("/tenant/active")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"tenant_id":"{tenant}"}}"#)))
            .unwrap()
    }

    #[tokio::test]
    async fn get_reads_the_pin_and_the_bound_candidates() {
        let (_amb, _machine) = fixture();
        let (status, body) = call(get_req()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let data = &body["data"];
        assert_eq!(data["active_tenant_id"], TENANT_A);
        assert_eq!(data["source"], "machine.json");
        assert_eq!(data["pin"], "pinned");
        assert_eq!(data["candidates"], serde_json::json!([TENANT_A, TENANT_B]));
    }

    #[tokio::test]
    async fn put_sets_the_pin_then_get_reads_it_back() {
        let (_amb, machine) = fixture();
        let (status, body) = call(put_req(TENANT_B)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let data = &body["data"];
        assert_eq!(data["active_tenant_id"], TENANT_B);
        assert_eq!(data["previous_active_tenant_id"], TENANT_A);
        // The effect-timing field is present and says what it covers.
        assert_eq!(data["takes_effect"], "live");
        assert_eq!(data["existing_sessions"], "unchanged");
        assert!(data["takes_effect_detail"]
            .as_str()
            .is_some_and(|d| d.contains("NEW sessions")));

        // The write landed in the fixture's machine.json, identity preserved.
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&machine).unwrap()).unwrap();
        assert_eq!(on_disk["active_tenant_id"], TENANT_B);
        assert_eq!(on_disk["device_id"], "11111111-0000-4000-8000-000000000001");
        assert_eq!(on_disk["hostname"], "box");

        // And the live resolver every consumer uses sees it with no restart.
        assert_eq!(
            qontinui_runner_lib::tenant_pin::resolve_tenant_pin(),
            qontinui_runner_lib::tenant_pin::TenantPin::Pinned(
                uuid::Uuid::parse_str(TENANT_B).unwrap()
            )
        );

        let (_, body) = call(get_req()).await;
        assert_eq!(body["data"]["active_tenant_id"], TENANT_B);
    }

    #[tokio::test]
    async fn put_normalizes_case_to_the_canonical_uuid() {
        let (_amb, _machine) = fixture();
        let (status, body) = call(put_req(&TENANT_B.to_uppercase())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["active_tenant_id"], TENANT_B);
    }

    #[tokio::test]
    async fn put_refuses_an_unbound_tenant_and_leaves_machine_json_byte_identical() {
        let (_amb, machine) = fixture();
        let before = std::fs::read(&machine).unwrap();
        let (status, body) = call(put_req(UNBOUND)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "TENANT_NOT_BOUND");
        assert_eq!(std::fs::read(&machine).unwrap(), before);
    }

    #[tokio::test]
    async fn put_refuses_a_malformed_id_and_leaves_machine_json_byte_identical() {
        let (_amb, machine) = fixture();
        let before = std::fs::read(&machine).unwrap();
        for bad in ["not-a-uuid", "   "] {
            let (status, body) = call(put_req(bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}: {body}");
            assert_eq!(body["code"], "INVALID_TENANT_ID");
        }
        assert_eq!(std::fs::read(&machine).unwrap(), before);
    }

    #[tokio::test]
    async fn put_refuses_when_the_device_holds_no_binding() {
        let (amb, machine) = fixture();
        std::fs::remove_file(amb.dir().join("paired_user.json")).unwrap();
        let before = std::fs::read(&machine).unwrap();
        let (status, body) = call(put_req(TENANT_A)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "NO_TENANT_BINDINGS");
        assert_eq!(std::fs::read(&machine).unwrap(), before);
    }

    #[tokio::test]
    async fn put_refuses_a_machine_json_with_no_identity() {
        let (amb, _machine) = fixture();
        let machine = amb.write_machine_json(r#"{"hostname":"box"}"#);
        let before = std::fs::read(&machine).unwrap();
        let (status, body) = call(put_req(TENANT_B)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "MACHINE_JSON_WRITE_REFUSED");
        assert_eq!(std::fs::read(&machine).unwrap(), before);
    }

    /// The route is a credential door: no browser origin but the runner's own
    /// webview reaches either method, whatever the route policy.
    #[test]
    fn tenant_active_is_a_credential_door_for_every_method() {
        for method in ["GET", "PUT"] {
            assert!(
                crate::mcp::origin_guard::is_credential_door(method, "/tenant/active"),
                "{method} /tenant/active must be on CREDENTIAL_DOORS"
            );
        }
    }
}
