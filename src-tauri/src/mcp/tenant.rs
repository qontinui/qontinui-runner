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
/// A write failure after the checks passed is 500 `MACHINE_JSON_WRITE_FAILED`.
///
/// On success `data.takes_effect` is `"mixed"`: `data.surfaces` lists each pin
/// consumer with `timing` `live` or `next_start` (the dual-write gate and the
/// coord-mcp nonce restore read the pin once at startup).
/// `data.existing_sessions` counts running coord-mcp bindings: those pinned at
/// creation keep their tenant, and those that were unpinned follow the new pin
/// on their next request.
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
        // The effect-timing field is present; its shape is pinned below.
        assert_eq!(data["takes_effect"], "mixed");

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

    /// The per-surface effect report: every surface names a timing of `live`
    /// or `next_start` and a non-empty evidence locator, both timings occur,
    /// the two startup readers are the ones classified `next_start`, and the
    /// running-session census carries both counts.
    #[tokio::test]
    async fn put_reports_per_surface_effect_timing_and_a_session_census() {
        let (_amb, _machine) = fixture();
        let (status, body) = call(put_req(TENANT_B)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let data = &body["data"];
        assert_eq!(data["takes_effect"], "mixed");

        let surfaces = data["surfaces"].as_array().expect("surfaces is a list");
        assert!(!surfaces.is_empty());
        let mut next_start = Vec::new();
        let mut live = 0;
        for s in surfaces {
            let name = s["surface"].as_str().expect("surface name");
            assert!(
                s["evidence"].as_str().is_some_and(|e| !e.trim().is_empty()),
                "{name}: evidence must be named"
            );
            assert!(s["detail"].as_str().is_some(), "{name}: detail");
            match s["timing"].as_str() {
                Some("live") => live += 1,
                Some("next_start") => next_start.push(name.to_string()),
                other => panic!("{name}: timing must be live|next_start, got {other:?}"),
            }
        }
        assert!(live > 0, "some surfaces switch live");
        next_start.sort();
        assert_eq!(
            next_start,
            [
                "coord_mcp_nonce_restore",
                "session_coordination_dual_write_gate"
            ],
            "exactly the startup readers are next_start"
        );

        let existing = &data["existing_sessions"];
        for group in ["pinned_at_creation", "follows_machine_pin"] {
            assert!(existing[group]["count"].is_u64(), "{group}.count");
            assert!(existing[group]["effect"].is_string(), "{group}.effect");
        }
        assert!(existing["scope"].is_string());
    }

    /// A write the checks allowed that then fails on I/O is a 500 with
    /// `MACHINE_JSON_WRITE_FAILED`, not a 4xx: the caller did nothing wrong.
    /// Forced by making the fixture directory read-only: the checks pass (both
    /// files stay readable), then the atomic write cannot create its temp file.
    #[cfg(unix)]
    #[tokio::test]
    async fn put_maps_a_write_failure_to_500() {
        use std::os::unix::fs::PermissionsExt;
        let (amb, machine) = fixture();
        let dir = amb.dir().to_path_buf();
        let before = std::fs::read(&machine).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Running as root ignores directory modes; the lever does not exist
        // there, so say so rather than pass vacuously.
        let probe = dir.join(".write-probe");
        if std::fs::write(&probe, b"x").is_ok() {
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("put_maps_a_write_failure_to_500: directory modes not enforced; skipped");
            return;
        }
        let (status, body) = call(put_req(TENANT_B)).await;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(body["code"], "MACHINE_JSON_WRITE_FAILED");
        assert_eq!(std::fs::read(&machine).unwrap(), before);
    }

    #[test]
    fn status_mapping_covers_every_refusal() {
        assert_eq!(
            status_for(&SetActiveTenantError::WriteFailed("x".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status_for(&SetActiveTenantError::NoHome),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status_for(&SetActiveTenantError::WriteRefused("x".into())),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status_for(&SetActiveTenantError::NotBound {
                tenant: "t".into(),
                bound: vec![]
            }),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_for(&SetActiveTenantError::Empty),
            StatusCode::BAD_REQUEST
        );
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
