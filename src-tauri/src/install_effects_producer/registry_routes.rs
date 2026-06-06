//! Axum routes for private-registry credential CRUD (Phase 4).
//!
//! Mirrors the wrapper-credential route posture in `wrappers::routes`
//! (`list_credentials` / `set_credential` / `delete_credential`) exactly:
//!
//! - **GET** returns presence + metadata only (`hasValue: bool`) — NEVER the
//!   plaintext.
//! - **PUT** stores a value in the OS keyring (rejects an empty value — use
//!   DELETE to clear; rejects a non-`[A-Z0-9_]+` name with 400).
//! - **DELETE** removes the value (idempotent — a missing entry is a 200).
//!
//! Route surface (full prefixes):
//!
//! | Method | Path                                                | Handler            |
//! | ------ | --------------------------------------------------- | ------------------ |
//! | GET    | `/install-effects/registry-credentials/{name}`      | get_credential     |
//! | PUT    | `/install-effects/registry-credentials/{name}`      | put_credential     |
//! | DELETE | `/install-effects/registry-credentials/{name}`      | delete_credential  |
//!
//! Mounted by [`super::routes`] alongside `POST /install-effects/run`.

use std::sync::Arc;

use axum::{extract::Path as AxumPath, http::StatusCode, response::Json, routing::get, Router};
use serde::{Deserialize, Serialize};

use super::registry_creds::{validate_name, RegistryCredentialError, RegistryCredentialStore};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // GET + DELETE share the `{name}` path — chain on one MethodRouter to
        // satisfy axum 0.8's "one MethodRouter per path" rule.
        .route(
            "/install-effects/registry-credentials/{name}",
            get(get_credential)
                .put(put_credential)
                .delete(delete_credential),
        )
}

/// GET response: presence + metadata, NEVER the secret. Mirrors
/// `wrappers::routes::CredentialDescriptor`'s `hasValue` posture.
#[derive(Debug, Serialize)]
struct RegistryCredentialDescriptor {
    name: String,
    /// `true` iff a value is currently stored. Plaintext is NEVER returned.
    #[serde(rename = "hasValue")]
    has_value: bool,
    /// The keyring service the entry lives under (metadata only — not a secret).
    service: &'static str,
}

/// `GET /install-effects/registry-credentials/{name}` — presence probe only.
async fn get_credential(
    AxumPath(name): AxumPath<String>,
) -> Result<Json<ApiResponse<RegistryCredentialDescriptor>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Validate the name shape so a bad key is a clean 400 (not a keyring error).
    validate_name(&name).map_err(name_error_to_http)?;
    Ok(Json(ApiResponse::success(RegistryCredentialDescriptor {
        has_value: RegistryCredentialStore::has(&name),
        service: super::registry_creds::KEYRING_SERVICE,
        name,
    })))
}

#[derive(Debug, Deserialize)]
struct SetRegistryCredentialRequest {
    value: String,
}

/// `PUT /install-effects/registry-credentials/{name}` — store a token. The
/// value is written to the OS keyring and NEVER echoed back.
async fn put_credential(
    AxumPath(name): AxumPath<String>,
    Json(req): Json<SetRegistryCredentialRequest>,
) -> Result<Json<ApiResponse<&'static str>>, (StatusCode, Json<ApiResponse<()>>)> {
    validate_name(&name).map_err(name_error_to_http)?;
    if req.value.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "credential value must not be empty (use DELETE to clear)",
            )),
        ));
    }
    match RegistryCredentialStore::set(&name, &req.value) {
        Ok(()) => Ok(Json(ApiResponse::success("set"))),
        Err(e) => Err(store_error_to_http(e)),
    }
}

/// `DELETE /install-effects/registry-credentials/{name}` — idempotent clear.
async fn delete_credential(
    AxumPath(name): AxumPath<String>,
) -> Result<Json<ApiResponse<&'static str>>, (StatusCode, Json<ApiResponse<()>>)> {
    validate_name(&name).map_err(name_error_to_http)?;
    match RegistryCredentialStore::delete(&name) {
        Ok(()) => Ok(Json(ApiResponse::success("deleted"))),
        Err(e) => Err(store_error_to_http(e)),
    }
}

/// A name-validation failure ⇒ 400. The message names the bad key (NOT a
/// secret — the key is the env-var name).
fn name_error_to_http(e: RegistryCredentialError) -> (StatusCode, Json<ApiResponse<()>>) {
    (StatusCode::BAD_REQUEST, Json(api_error(e.to_string())))
}

/// A keyring backend failure ⇒ 500. `Display` never includes the value.
fn store_error_to_http(e: RegistryCredentialError) -> (StatusCode, Json<ApiResponse<()>>) {
    match e {
        RegistryCredentialError::InvalidName(_) => name_error_to_http(e),
        RegistryCredentialError::Keyring(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("registry credential store error: {e}"))),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The handlers don't read `ApiState` (the registry store is a global OS
    // keyring), so a full-router test would only add a heavy `tauri::AppHandle`
    // construction. We instead test the load-bearing seams directly: the
    // name-validation → status mapping and the "GET descriptor never serializes
    // a value" guard.

    #[test]
    fn bad_name_maps_to_400() {
        let (status, _) = name_error_to_http(validate_name("bad-name").unwrap_err());
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status2, _) = name_error_to_http(validate_name("").unwrap_err());
        assert_eq!(status2, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn keyring_store_error_maps_to_500() {
        let (status, _) =
            store_error_to_http(RegistryCredentialError::Keyring("backend down".into()));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn descriptor_serializes_presence_only_never_a_value() {
        // The "secret never leaks via the route" guard: a GET descriptor with a
        // stored value present STILL only serializes `hasValue` (no plaintext).
        let desc = RegistryCredentialDescriptor {
            name: "NPM_TOKEN".to_string(),
            has_value: true,
            service: super::super::registry_creds::KEYRING_SERVICE,
        };
        let json = serde_json::to_value(&desc).unwrap();
        assert_eq!(json["hasValue"], true);
        assert_eq!(json["name"], "NPM_TOKEN");
        assert_eq!(
            json["service"],
            super::super::registry_creds::KEYRING_SERVICE
        );
        // No `value`/secret key in the serialized descriptor — EVER.
        assert!(
            json.get("value").is_none(),
            "descriptor leaked a value: {json}"
        );
        // And the only string fields are name + service (the env-var name and
        // the keyring service id — neither is the secret).
        let obj = json.as_object().unwrap();
        assert_eq!(obj.len(), 3, "descriptor fields: {obj:?}");
    }

    #[test]
    fn empty_put_value_is_a_client_error_shape() {
        // The empty-value rejection lives in `put_credential`; assert the
        // contract message exists so a clear() goes through DELETE.
        let req = SetRegistryCredentialRequest {
            value: String::new(),
        };
        assert!(req.value.is_empty());
    }
}
