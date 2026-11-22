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
//! The route is unauthenticated for a local, non-browser caller (see the
//! origin-guard note on [`routes`]), so it never emits the declaration's
//! `base_url`, `headers`, `api_key_helper` (a command line that prints the
//! key), `network_auth` or TTL. [`gateway_state_json`] takes the state by
//! reference and reads only its variant (plus the `Unknown` reason text, which
//! is passed through — next section); the tests pin that a fully populated
//! declaration contributes nothing to the body.
//!
//! # Which reasons are fixed
//!
//! Only two: `declared` serves [`DECLARED_REASON`] and `not_declared` serves
//! [`NOT_DECLARED_REASON`]. An `unknown` reason is passed through as composed,
//! so it is not fixed: `classify_state`'s text (fixed sentences plus the
//! settings read error), or the handler's own text carrying the join error when
//! the blocking read could not finish.

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

/// `GET /model-gateway`: [`serve_with`] over the live
/// [`crate::model_gateway::state`]. The tests drive [`serve_with`]; this
/// wrapper's one job, passing the LIVE reader, is not under test (proving it
/// would mean writing a declared gateway into the process-global config dir).
/// Never pass it a constant: an answer of `not_declared` that is not the
/// reader's lets studio call vendors directly.
pub async fn get_model_gateway(
    axum::extract::State(_state): axum::extract::State<Arc<ApiState>>,
) -> Json<Value> {
    serve_with(crate::model_gateway::state).await
}

/// The handler body with its state reader injected, so the tests can prove the
/// served answer is the reader's and not a constant.
///
/// The live reader reads `settings.json` and the gateway marker from disk, so
/// it runs on the blocking pool. A failed join (the reader panicked, or the
/// task was cancelled) is reported as `unknown` carrying the join error, never
/// as `not_declared`: an answer that could not be computed must not read as
/// "no gateway", which studio would take as leave to call vendors directly.
async fn serve_with<F>(read: F) -> Json<Value>
where
    F: FnOnce() -> GatewayState + Send + 'static,
{
    let state = tokio::task::spawn_blocking(read).await.unwrap_or_else(|e| {
        GatewayState::Unknown(format!(
            "the model gateway state could not be read ({e}); failing closed"
        ))
    });
    Json(gateway_state_json(&state))
}

/// Routes for this module.
///
/// Origin guard: `GET /model-gateway` is a `CREDENTIAL_DOORS` entry. It
/// returns no secret, but under the default `EnforceDoors` route policy a
/// route on no list is only SHADOW-refused to a browser: a Foreign origin is
/// admitted (and counted) and CORS echoes it, so a visited web page could read
/// this install's gateway state wherever the browser does not enforce a
/// private-network preflight. The door entry refuses every browser class
/// (Trusted, Extension, Foreign) under every route policy. The caller,
/// qontinui-studio's local CLI, sends no `Origin` and no cross-site
/// `Sec-Fetch-Site`, so it classifies `NonBrowser` and is admitted before the
/// door check, as is the webview (`FirstParty`).
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

    /// A settings value that is unmistakably a declaration.
    fn declared_settings() -> ModelGatewaySettings {
        ModelGatewaySettings {
            base_url: Some("https://gw.example.com/anthropic".to_string()),
            headers: BTreeMap::new(),
            api_key_helper: None,
            network_auth: false,
            api_key_helper_ttl_secs: None,
        }
    }

    /// The handler serves what its reader returned. A handler that ignored the
    /// reader and always answered `not_declared` would make studio fail OPEN
    /// (direct vendor image calls); this is the test that goes red for it.
    #[tokio::test]
    async fn the_handler_serves_a_declared_readers_state() {
        let Json(v) = serve_with(|| GatewayState::Declared(declared_settings())).await;
        assert_eq!(v["state"], "declared");
        assert_eq!(v["reason"], DECLARED_REASON);
    }

    #[tokio::test]
    async fn the_handler_serves_a_not_declared_readers_state() {
        let Json(v) = serve_with(|| GatewayState::NotDeclared).await;
        assert_eq!(v["state"], "not_declared");
        assert_eq!(v["reason"], NOT_DECLARED_REASON);
    }

    /// A reader that panics on the blocking pool is a failed join, and a
    /// failed join is `unknown` carrying the join error — never `not_declared`.
    #[tokio::test]
    async fn a_panicking_reader_is_unknown_with_the_join_error() {
        let Json(v) = serve_with(|| -> GatewayState {
            panic!("gateway-reader-panic-marker");
        })
        .await;
        assert_eq!(keys(&v), ["reason", "routes", "state"]);
        assert_eq!(v["state"], "unknown");
        let reason = v["reason"].as_str().expect("reason is a string");
        assert!(
            reason.starts_with("the model gateway state could not be read (")
                && reason.ends_with("); failing closed"),
            "reason: {reason}"
        );
        assert!(
            reason.contains("panicked") && reason.contains("gateway-reader-panic-marker"),
            "the join error is carried: {reason}"
        );
        assert_eq!(v["routes"], json!({}));
    }

    /// The route is registered, and it is MOUNTED: `create_router` in
    /// `mcp_api.rs` merges this module's router. `registered_routes()` alone
    /// cannot pin the mount — it scans every source file, this one included,
    /// so it reads the `.route("/model-gateway", …)` above whether or not
    /// anything merges it. The mount is read from `create_router`'s own body
    /// with comment lines dropped, so a commented-out merge does not count.
    /// (`origin_guard::tests::no_route_registered_after_apply` separately pins
    /// that every merge there sits before the guard is applied.)
    #[test]
    fn route_is_registered_and_mounted_in_create_router() {
        let registered = crate::mcp::relay_path_policy::tests::registered_routes();
        assert!(
            registered.contains(&("GET".to_string(), "/model-gateway".to_string())),
            "GET /model-gateway is not registered"
        );
        let mounted = crate::mcp_api::mcp_api_item_source("fn create_router(")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .any(|l| l.trim() == ".merge(crate::mcp::model_gateway_read::routes())");
        assert!(
            mounted,
            "create_router must merge crate::mcp::model_gateway_read::routes()"
        );
    }

    /// The route is a credential door (see [`routes`]), and the entry is
    /// route-exact: it widens nothing else.
    #[test]
    fn route_is_a_credential_door() {
        assert!(crate::mcp::origin_guard::is_credential_door(
            "GET",
            "/model-gateway"
        ));
        assert!(!crate::mcp::origin_guard::is_credential_door(
            "POST",
            "/model-gateway"
        ));
        assert!(!crate::mcp::origin_guard::is_credential_door(
            "GET",
            "/model-gateway/x"
        ));
    }
}
