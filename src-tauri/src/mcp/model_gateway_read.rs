//! `GET /model-gateway` — this install's model-gateway state over HTTP.
//!
//! [`crate::model_gateway::state`] is the authority (`NotDeclared | Declared |
//! Unknown`); until this route it was readable only through the Tauri command
//! `get_model_gateway`. The consumer is qontinui-studio's gateway read
//! (`src/qontinui_studio/gateway.py`), a local CLI, which refuses every image
//! call unless the answer is `not_declared` — so the read is fail-closed on
//! the studio side, and this route only has to tell the truth.
//!
//! # Shape
//!
//! ```json
//! {"state": "not_declared" | "declared" | "unknown", "reason": "<text>", "routes": {}}
//! ```
//!
//! Served bare (studio accepts it bare or inside `{"data": ...}`).
//!
//! # `routes` is always `{}`
//!
//! Studio's contract lets `routes` carry `images` (an OpenAI-images-wire base
//! URL) and `images_credential_env`. The runner's gateway fronts the
//! Anthropic **Messages** API only ([`crate::model_gateway::ModelGatewaySettings`]
//! has a single `base_url` the runner appends `/v1/messages` to); it declares
//! no images route. Emitting the Messages base URL under `images` would point
//! studio's image adapters at an endpoint that does not speak their wire, so
//! nothing is emitted, and a declared gateway therefore refuses image calls in
//! studio — the intended fail-closed reading.
//!
//! # No secret-adjacent material
//!
//! The route is unauthenticated (see the origin-guard note on
//! [`routes`]), so it never emits the declaration's `base_url`, `headers`,
//! `api_key_helper` (a command line that prints the key), `network_auth` or
//! TTL. [`gateway_state_json`] takes the state by reference and reads only its
//! variant (plus the `Unknown` reason text, which `classify_state` composes
//! from fixed sentences and the settings read error); the tests pin that a
//! fully populated declaration contributes nothing to the body.

use std::sync::Arc;

use axum::{response::Json, routing::get};
use serde_json::{json, Value};

use crate::mcp::types::ApiState;
use crate::model_gateway::GatewayState;

/// The fixed `reason` for a declared gateway: no declaration field is echoed.
pub const DECLARED_REASON: &str = "a model gateway is declared; it carries no images route";

/// The fixed `reason` when no gateway is declared.
pub const NOT_DECLARED_REASON: &str = "no model gateway is declared on this install";

/// Map a gateway state to the served JSON body. Pure, so every state is
/// unit-testable without touching the live config dir.
pub fn gateway_state_json(state: &GatewayState) -> Value {
    let (state, reason) = match state {
        GatewayState::NotDeclared => ("not_declared", NOT_DECLARED_REASON.to_string()),
        // The declaration is deliberately unbound: nothing in it is served.
        GatewayState::Declared(_) => ("declared", DECLARED_REASON.to_string()),
        GatewayState::Unknown(reason) => ("unknown", reason.clone()),
    };
    json!({
        "state": state,
        "reason": reason,
        // Always empty: the runner gateway is Messages-shaped and declares no
        // images route (module doc).
        "routes": {},
    })
}

/// `GET /model-gateway`.
///
/// `state()` reads `settings.json` and the gateway marker from disk, so it runs
/// on the blocking pool. A failed join is reported as `unknown`, never as
/// `not_declared`: an answer that could not be computed must not read as "no
/// gateway".
pub async fn get_model_gateway(
    axum::extract::State(_state): axum::extract::State<Arc<ApiState>>,
) -> Json<Value> {
    let state = tokio::task::spawn_blocking(crate::model_gateway::state)
        .await
        .unwrap_or_else(|e| {
            GatewayState::Unknown(format!(
                "the model gateway state could not be read ({e}); failing closed"
            ))
        });
    Json(gateway_state_json(&state))
}

/// Routes for this module.
///
/// Origin guard: `/model-gateway` is on no route list, deliberately. It is not
/// a door (it returns no secret, names no path or URL, spawns nothing), so it
/// is not on `CREDENTIAL_DOORS`; its caller is a local CLI, which classifies
/// `NonBrowser` and reaches every route under every route policy, so it needs
/// no browser allowlist. A browser origin is admitted-and-counted under the
/// default `EnforceDoors` and refused under `enforce` — no browser needs it.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    axum::Router::new().route("/model-gateway", get(get_model_gateway))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_gateway::ModelGatewaySettings;
    use std::collections::BTreeMap;

    fn keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        k.sort();
        k
    }

    #[test]
    fn not_declared_shape() {
        let v = gateway_state_json(&GatewayState::NotDeclared);
        assert_eq!(keys(&v), ["reason", "routes", "state"]);
        assert_eq!(v["state"], "not_declared");
        assert_eq!(v["reason"], NOT_DECLARED_REASON);
        assert_eq!(v["routes"], json!({}));
    }

    #[test]
    fn unknown_carries_its_reason() {
        let v = gateway_state_json(&GatewayState::Unknown("settings.json unreadable".into()));
        assert_eq!(keys(&v), ["reason", "routes", "state"]);
        assert_eq!(v["state"], "unknown");
        assert_eq!(v["reason"], "settings.json unreadable");
        assert_eq!(v["routes"], json!({}));
    }

    #[test]
    fn declared_emits_no_declaration_field() {
        let mut headers = BTreeMap::new();
        headers.insert(
            "X-Tenant-Tag".to_string(),
            "tenant-header-value-7f3a".to_string(),
        );
        let settings = ModelGatewaySettings {
            base_url: Some("https://gw-base-url-marker.example.com/anthropic".to_string()),
            headers,
            api_key_helper: Some("print-key-helper-marker --secret".to_string()),
            network_auth: true,
            api_key_helper_ttl_secs: Some(4242),
        };
        let v = gateway_state_json(&GatewayState::Declared(settings));
        assert_eq!(keys(&v), ["reason", "routes", "state"]);
        assert_eq!(v["state"], "declared");
        assert_eq!(v["reason"], DECLARED_REASON);
        assert_eq!(v["routes"], json!({}));

        let body = v.to_string();
        for marker in [
            "gw-base-url-marker",
            "example.com",
            "X-Tenant-Tag",
            "tenant-header-value-7f3a",
            "print-key-helper-marker",
            "--secret",
            "4242",
            "network_auth",
            "api_key_helper",
            "base_url",
            "headers",
        ] {
            assert!(!body.contains(marker), "body leaked {marker:?}: {body}");
        }
    }

    #[test]
    fn route_is_registered_and_not_a_door() {
        let registered = crate::mcp::relay_path_policy::tests::registered_routes();
        assert!(
            registered.contains(&("GET".to_string(), "/model-gateway".to_string())),
            "GET /model-gateway is not registered"
        );
        assert!(!crate::mcp::origin_guard::is_credential_door(
            "GET",
            "/model-gateway"
        ));
    }
}
