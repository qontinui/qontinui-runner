//! Wrapper App Manifest Builder
//!
//! Phase B1 of the wrapper-Builder fix plan: builds a Markdown "Registered
//! Wrapper Apps" section that gets injected into the Builder's recognition
//! prompt at generation time. Without this, the Builder is blind to live
//! WebSocket-registered wrapper apps and either invents wrapperIds (which
//! 404 at runtime) or omits wrapper steps entirely.
//!
//! ## Design split
//!
//! The module is intentionally split into two layers so tests can target
//! the formatter directly without spinning up a full `ApiState`:
//!
//! - [`format_manifest`] — pure synchronous formatter. Takes the
//!   already-fetched `(app, components)` pairs and produces the Markdown
//!   block. Easy to unit-test with canned data.
//! - [`build_manifest_from_state`] — async fetcher. Takes an `AppRegistry`
//!   and `AppDispatcher`, lists live WS-transport apps, dispatches
//!   `getComponents` to each, defensively unwraps the response shapes
//!   wrappers may return, and feeds the result to `format_manifest`.
//! - [`build_manifest`] — convenience entry-point that takes a
//!   `commands::AppState` and threads through to the fetcher. The
//!   recognition-prompt call sites use this layer so they don't need
//!   direct awareness of the registry/dispatcher types.
//!
//! ## Caching
//!
//! `build_manifest_from_state` consults a 5-second wall-clock TTL cache
//! shared across calls so back-to-back Builder runs (e.g. an HTTP request
//! quickly retried after a transient failure) don't re-dispatch
//! `getComponents` to every live wrapper. The cache key is implicit (whole
//! manifest); 5s is short enough that wrappers entering or leaving the
//! registry surface to the next Builder run within one prompt-assembly
//! window. See `proj_wrapper_builder_orchestration_gaps.md` for context.
//!
//! ## Output stability
//!
//! The Markdown output is deterministic for a given (sorted) set of apps
//! and actions: apps are emitted alphabetically by `appId`, and actions
//! within each app are emitted in the order the wrapper returned them
//! (most wrappers' implementations preserve a stable declaration order).
//! Tests assert specific strings appear in the output rather than exact
//! whole-string equality so cosmetic formatting tweaks don't churn them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::warn;

use crate::mcp::app_dispatch::AppDispatcher;
use crate::mcp::app_registry::{AppRegistry, AppTransport, RegisteredApp};

/// 5-second TTL cache. The worst-case staleness is 5 s of an apparently-
/// live wrapper appearing absent (or vice-versa) in a freshly-generated
/// workflow's prompt. Acceptable given that any miss only re-dispatches
/// `getComponents` to live wrappers, which is itself sub-second.
const CACHE_TTL: Duration = Duration::from_secs(5);

/// Process-wide cache. The cache stores the final formatted Markdown
/// keyed by `runner_api_port`, so different runner instances (primary +
/// temp runners) don't trip over each other's cached output.
static CACHE: once_cell::sync::Lazy<Mutex<Option<CachedManifest>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

#[derive(Clone)]
struct CachedManifest {
    runner_api_port: u16,
    fetched_at: Instant,
    markdown: String,
}

/// One entry in the formatted manifest: a wrapper app plus the components
/// it reports via `getComponents`. The `components` field is the raw JSON
/// the wrapper returned (already unwrapped from `{data: …}` /
/// `{components: …}` envelopes); each element is expected to follow the
/// shape `{ id, name?, actions?: [{ id, name?, paramSchema? }] }` but the
/// formatter is defensive about missing fields.
#[derive(Clone)]
pub struct ManifestEntry {
    pub app: RegisteredApp,
    pub components: Vec<Value>,
}

/// Public convenience entry-point used by the Builder recognition-prompt
/// call sites. Takes a `commands::AppState` for symmetry with the existing
/// `runner_api_port` resolution path.
///
/// `AppState` carries `OnceCell` handles to the shared `AppRegistry` and
/// `AppDispatcher` that `mcp_api::start_server` populates immediately
/// after construction. Reads here are guaranteed to succeed once the HTTP
/// server has started; before that (e.g. unit tests that construct an
/// `AppState` without bringing up the server, or rare bootstrap-phase
/// callers), this function returns an empty string and the prompt's
/// append-when-non-empty branch skips the "Registered Wrapper Apps"
/// section entirely.
pub async fn build_manifest(app_state: &crate::commands::AppState, runner_api_port: u16) -> String {
    let registry = match app_state.app_registry.get() {
        Some(r) => r,
        None => return String::new(),
    };
    let dispatcher = match app_state.app_dispatcher.get() {
        Some(d) => d,
        None => return String::new(),
    };
    build_manifest_from_state(registry, dispatcher, runner_api_port).await
}

/// Build the manifest by fetching live components from every WS-transport
/// app registered in `registry`, then handing the result to
/// [`format_manifest`]. Subject to a 5-second TTL cache shared across
/// callers.
///
/// Returns an empty string if no WS-transport apps are live.
pub async fn build_manifest_from_state(
    registry: &Arc<AppRegistry>,
    dispatcher: &Arc<AppDispatcher>,
    runner_api_port: u16,
) -> String {
    // Fast-path: warm cache for the same runner port.
    {
        let cached = CACHE.lock().await;
        if let Some(c) = cached.as_ref() {
            if c.runner_api_port == runner_api_port && c.fetched_at.elapsed() < CACHE_TTL {
                return c.markdown.clone();
            }
        }
    }

    let entries = collect_live_ws_components(registry, dispatcher).await;
    let markdown = format_manifest(&entries, runner_api_port);

    let mut cached = CACHE.lock().await;
    *cached = Some(CachedManifest {
        runner_api_port,
        fetched_at: Instant::now(),
        markdown: markdown.clone(),
    });
    markdown
}

/// For each live WS-transport app in `registry`, dispatch `getComponents`
/// over the WS connection and collect the response. The response shapes
/// wrappers may return are defensively unwrapped to mirror the behavior
/// of `mcp::ui_bridge::elements::ws_collect_components` (Phase A1).
///
/// Per-wrapper failures during the fan-out are logged and skipped rather
/// than aborting — a single flaky wrapper must not deny the entire Builder
/// the rest of the manifest.
async fn collect_live_ws_components(
    registry: &Arc<AppRegistry>,
    dispatcher: &Arc<AppDispatcher>,
) -> Vec<ManifestEntry> {
    let live = registry.list_live().await;
    let mut out = Vec::new();
    for entry in live {
        if entry.transport != AppTransport::Websocket {
            continue;
        }
        let app_id = entry.app.app_id.clone();
        let components = match dispatcher
            .dispatch(
                &app_id,
                "getComponents",
                Method::GET,
                "/ui-bridge/control/components",
                serde_json::json!({}),
            )
            .await
        {
            Ok(value) => unwrap_components_payload(value),
            Err(e) => {
                warn!(
                    "[wrapper-manifest] getComponents WS dispatch to '{}' failed: {}",
                    app_id, e
                );
                continue;
            }
        };
        out.push(ManifestEntry {
            app: entry,
            components,
        });
    }
    // Sort by appId for deterministic output across runs (the registry
    // returns HashMap iteration order, which is unspecified).
    out.sort_by(|a, b| a.app.app.app_id.cmp(&b.app.app.app_id));
    out
}

/// Defensively unwrap whatever shape a wrapper's `getComponents` handler
/// returns: a bare array, `{data: [...]}`, `{components: [...]}`,
/// `{data: {components: [...]}}`, or a single-component object. Mirrors
/// the unwrapping logic in `ws_collect_components` so the wire contract
/// stays consistent across the read-merge endpoint and the manifest
/// builder.
fn unwrap_components_payload(value: Value) -> Vec<Value> {
    if let Some(arr) = value.as_array() {
        return arr.clone();
    }
    if let Some(arr) = value.get("data").and_then(|v| v.as_array()) {
        return arr.clone();
    }
    if let Some(arr) = value.get("components").and_then(|v| v.as_array()) {
        return arr.clone();
    }
    if let Some(arr) = value
        .get("data")
        .and_then(|d| d.get("components"))
        .and_then(|v| v.as_array())
    {
        return arr.clone();
    }
    if value.is_object() {
        return vec![value];
    }
    Vec::new()
}

/// Pure synchronous formatter. Takes pre-fetched `(app, components)`
/// pairs and produces the Markdown block.
///
/// Returns an empty string when `entries` is empty so the prompt's
/// append-when-non-empty branch can simply check `!s.is_empty()`.
pub fn format_manifest(entries: &[ManifestEntry], runner_api_port: u16) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str("## Registered Wrapper Apps\n\n");
    out.push_str(
        "The following apps are currently registered via WebSocket transport. \
         To call one of their actions, emit a `command` step:\n\n",
    );
    out.push_str(&format!(
        "`curl -sS -X POST http://localhost:{port}/ui-bridge/control/component/<wrapperId>/action/<actionId> -H \"Content-Type: application/json\" -d '{{ ...params... }}'`\n\n",
        port = runner_api_port
    ));
    out.push_str("Available wrappers and actions:\n\n");

    for entry in entries {
        let app_id = &entry.app.app.app_id;
        let app_name = entry.app.app.app_name.as_str();
        out.push_str(&format!(
            "- **wrapperId: `{app_id}`** (\"{app_name}\")\n",
            app_id = app_id,
            app_name = app_name
        ));

        // Each component may itself carry a list of actions, OR be a
        // single addressable action. Wrappers vary; we emit one bullet
        // per action found, falling back to one bullet per component
        // when the component shape doesn't expose nested actions.
        let action_lines = format_component_actions(&entry.components);
        if action_lines.is_empty() {
            out.push_str("  - _(no actions reported)_\n");
        } else {
            for line in action_lines {
                out.push_str(&format!("  - {}\n", line));
            }
        }
    }

    out.push_str(
        "\nUse the wrapper's exact `wrapperId` and `actionId` strings — \
         they are case-sensitive and the runtime will reject mismatches with HTTP 404. \
         If a wrapper is no longer registered when the workflow runs, \
         the step will fail with a structured error.\n",
    );
    out
}

/// Walk a component list and produce `"actionId — params: <schema>"`
/// bullets. Handles the two common wrapper shapes:
///
/// 1. Component carries `actions: [{ id, paramSchema? }, ...]` — emit
///    one bullet per action.
/// 2. Component itself looks like an action (has `id` at top level
///    and no nested `actions`) — emit one bullet for it.
///
/// Components without an `id` field at any level are skipped (we have no
/// addressable handle to emit anyway).
fn format_component_actions(components: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for c in components {
        // Shape 1: nested actions array.
        if let Some(actions) = c.get("actions").and_then(|v| v.as_array()) {
            for a in actions {
                if let Some(line) = format_action_line(a) {
                    out.push(line);
                }
            }
            continue;
        }
        // Shape 2: component is itself the action.
        if let Some(line) = format_action_line(c) {
            out.push(line);
        }
    }
    out
}

fn format_action_line(action: &Value) -> Option<String> {
    let id = action.get("id").and_then(|v| v.as_str())?;
    let params = action.get("paramSchema").or_else(|| action.get("params"));
    let params_str = match params {
        Some(p) => serde_json::to_string(p).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    };
    Some(format!(
        "`{id}` — params: `{params}`",
        id = id,
        params = params_str
    ))
}

// ============================================================================
// Tests
// ============================================================================
//
// The tests target the pure formatter (`format_manifest`) and the live
// fetcher (`build_manifest_from_state`) directly. We deliberately avoid
// constructing a full `commands::AppState` for tests; that struct pulls in
// dozens of services and would dwarf the unit-test surface.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::app_discovery::DiscoveredApp;
    use crate::mcp::command_relay::{CommandRelay, CommandResponse};
    use crate::mcp::ws_relay::WsConnectionManager;
    use serde_json::json;
    use std::time::Duration;

    fn sample_app(app_id: &str, app_name: &str) -> DiscoveredApp {
        DiscoveredApp {
            app_id: app_id.to_string(),
            app_name: app_name.to_string(),
            app_type: "web".into(),
            framework: None,
            url: "http://unused".into(),
            port: 0,
            base_path: "".into(),
            version: None,
            capabilities: vec![],
            element_count: None,
            component_count: None,
            discovered_at: 0,
        }
    }

    fn registered(app_id: &str, app_name: &str, transport: AppTransport) -> RegisteredApp {
        RegisteredApp {
            app: sample_app(app_id, app_name),
            origin: None,
            last_seen_ms: chrono::Utc::now().timestamp_millis(),
            transport,
            websocket_conn_id: if transport == AppTransport::Websocket {
                Some(1)
            } else {
                None
            },
            keep_alive_ms: None,
        }
    }

    /// Test 1 — Empty registry: `format_manifest` returns "" so the
    /// recognition prompt skips the section header entirely.
    #[test]
    fn empty_entries_produce_empty_string() {
        let s = format_manifest(&[], 9876);
        assert_eq!(s, "");
    }

    /// Test 2 — Single wrapper, two actions: assert id, name, both
    /// action ids, and a paramSchema fragment all appear in the
    /// formatted output.
    #[test]
    fn single_wrapper_with_two_actions_formats_correctly() {
        let entry = ManifestEntry {
            app: registered("test-wrapper", "Test Wrapper", AppTransport::Websocket),
            components: vec![json!({
                "id": "test-wrapper",
                "actions": [
                    {
                        "id": "echo",
                        "paramSchema": { "input": "string" }
                    },
                    {
                        "id": "reverse",
                        "paramSchema": { "text": "string", "limit": "integer" }
                    }
                ]
            })],
        };
        let s = format_manifest(&[entry], 9877);
        // Section header appears.
        assert!(
            s.contains("## Registered Wrapper Apps"),
            "section header missing"
        );
        // Curl template uses the runner port we passed in.
        assert!(
            s.contains(
                "http://localhost:9877/ui-bridge/control/component/<wrapperId>/action/<actionId>"
            ),
            "curl template missing or has wrong port"
        );
        // Wrapper id & display name surface.
        assert!(s.contains("`test-wrapper`"), "wrapperId missing");
        assert!(s.contains("\"Test Wrapper\""), "app_name missing");
        // Both action ids surface.
        assert!(s.contains("`echo`"), "action 'echo' missing");
        assert!(s.contains("`reverse`"), "action 'reverse' missing");
        // paramSchema fragment surfaces.
        assert!(
            s.contains("\"input\":\"string\""),
            "paramSchema fragment for echo missing"
        );
        // Failure-mode caveat documented in the trailing prose.
        assert!(s.contains("HTTP 404"), "404 caveat missing");
    }

    /// Test 3 — HTTP-transport app must NOT appear in the output of
    /// `build_manifest_from_state`: the WS-only filter is applied before
    /// dispatch.
    #[tokio::test]
    async fn http_transport_app_is_skipped() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let (_conn_id, mut outbound_rx) = ws.test_register("ws-app").await;
        // One WS app — should appear.
        registry
            .upsert(
                sample_app("ws-app", "WS App"),
                None,
                AppTransport::Websocket,
                Some(1),
                None,
            )
            .await;
        // One HTTP app — must be filtered out.
        registry
            .upsert(
                sample_app("http-app", "HTTP App"),
                None,
                AppTransport::Http,
                None,
                None,
            )
            .await;

        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));
        let dispatcher = AppDispatcher::new(registry.clone(), relay.clone());

        // Spawn the manifest build so we can play the wrapper on the other end.
        let registry_h = registry.clone();
        let dispatcher_h = dispatcher.clone();
        let build_fut = tokio::spawn(async move {
            // Bypass the cache to keep this test independent of cache
            // state from prior tests in the same process.
            collect_live_ws_components(&registry_h, &dispatcher_h).await
        });

        // Pull the outbound frame the dispatcher pushed to the WS app
        // and resolve it. If the HTTP app were not filtered, we'd see a
        // dispatch attempt to it as well (which would fail with a
        // network error, since `sample_app` uses "http://unused").
        let frame = outbound_rx
            .recv()
            .await
            .expect("ws-app must receive a frame");
        let v: Value = serde_json::from_str(&frame).unwrap();
        let cmd_id = v["commandId"].as_str().unwrap().to_string();
        relay
            .resolve(CommandResponse {
                command_id: cmd_id,
                success: true,
                result: Some(json!([{ "id": "ws-app", "actions": [{"id": "noop"}] }])),
                error: None,
            })
            .await;

        let entries = build_fut.await.unwrap();
        let app_ids: Vec<String> = entries.iter().map(|e| e.app.app.app_id.clone()).collect();
        assert_eq!(
            app_ids,
            vec!["ws-app".to_string()],
            "HTTP-transport apps must not appear in the manifest"
        );

        // The formatted output for those entries also must not mention
        // the HTTP app's id.
        let md = format_manifest(&entries, 9876);
        assert!(md.contains("`ws-app`"), "ws-app should appear");
        assert!(
            !md.contains("`http-app`"),
            "http-app must NOT appear in the manifest"
        );
    }

    /// Test 4 — `unwrap_components_payload` accepts each of the four
    /// shapes wrappers commonly return, plus a single-object fallback.
    #[test]
    fn unwrap_components_payload_handles_all_shapes() {
        // Bare array.
        let v = json!([{"id": "a"}, {"id": "b"}]);
        assert_eq!(unwrap_components_payload(v).len(), 2);

        // {data: [...]}.
        let v = json!({"data": [{"id": "a"}]});
        assert_eq!(unwrap_components_payload(v).len(), 1);

        // {components: [...]}.
        let v = json!({"components": [{"id": "a"}, {"id": "b"}, {"id": "c"}]});
        assert_eq!(unwrap_components_payload(v).len(), 3);

        // {data: {components: [...]}}.
        let v = json!({"data": {"components": [{"id": "a"}]}});
        assert_eq!(unwrap_components_payload(v).len(), 1);

        // Single object, no envelope.
        let v = json!({"id": "lone"});
        let out = unwrap_components_payload(v);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get("id").and_then(|v| v.as_str()), Some("lone"));

        // Non-object, non-array: empty.
        let v = json!("string");
        assert_eq!(unwrap_components_payload(v).len(), 0);
    }
}
