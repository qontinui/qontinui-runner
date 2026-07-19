//! Document/window-level key dispatch.
//!
//! Covers:
//!   - `POST /ui-bridge/control/key`
//!
//! # Why this family exists
//!
//! Some runner UI is reachable ONLY behind a keyboard shortcut. The
//! session-manager sidebar (`SessionManagerPanel`, which hosts the Worktrees
//! panel) renders only when `workflowGen.showSidebar` is true; that flag
//! defaults `false` and is toggled by `Ctrl+Shift+B`, whose listener is
//! attached to **`window`** (`src/hooks/terminal/useKeyboardShortcuts.ts`).
//!
//! Before this route there was no way for UI Bridge automation to fire such a
//! shortcut:
//!   - `GET /ui-bridge/control/keyboard-shortcuts` is a read-only *registry* —
//!     it lists shortcuts, it cannot fire one.
//!   - `/ui-bridge/commands` + `/ui-bridge/invoke/{cmd}` expose **Tauri
//!     backend** commands, not frontend shortcuts.
//!   - The SDK's `keyboard` element action dispatches modifier keydowns but is
//!     **element-scoped**, so the per-element action gate in
//!     `useControlEvents.ts` rejects it (`Action 'keyboard' is not allowed for
//!     element '<id>'`) unless the element itself advertises `keyboard`.
//!
//! Keyboard shortcuts are inherently document-level, so this route is
//! deliberately **element-free** rather than being modelled as a `keyboard`
//! action on a synthetic `document`/`body` pseudo-element — inventing such a
//! pseudo-element would weaken the per-element action gate for no benefit.
//!
//! # Request shape
//!
//! The body mirrors the SDK's existing `keyboard`/`sendKeys` param shape, so a
//! later promotion into the SDK contract is a rename and not a redesign:
//!
//! ```jsonc
//! {
//!   "keys": [ { "key": "B", "modifiers": { "ctrl": true, "shift": true } } ],
//!   "target": "window"   // optional; default "window"
//! }
//! ```
//!
//! A single-object shorthand (`{"key":"B","modifiers":{…}}`) and bare strings
//! inside `keys` (`{"keys":["Escape"]}`) are accepted and coerced to the array
//! form. A missing or empty `keys` is a 400.
//!
//! # ⚠ `target: "activeElement"` — text-injection hazard
//!
//! `window` is the DEFAULT and this matters for safety: a `window`-targeted
//! `KeyboardEvent` reaches the runner's shortcut listeners but **cannot** land
//! text in a focused input.
//!
//! `activeElement` is the ONE target that CAN type into a focused field, and on
//! a runner the focused field is frequently a terminal input bound to a **live
//! Claude / PowerShell session** — an unintended dispatch there injects text
//! into someone's live work. It is therefore **opt-in only** and must never
//! become the default. Prefer `window` unless you specifically need
//! focus-scoped typing, in which case use the element-scoped `keyboard` action
//! on a known element instead whenever possible.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::{target_window_payload, ui_bridge_request_sync, wrap_ipc_result};

/// Default dispatch target. `window` is the safe default: the runner's global
/// shortcut listeners live on `window`, and a `window`-dispatched event cannot
/// land text in a focused input.
pub const DEFAULT_KEY_TARGET: &str = "window";

/// Dispatch targets accepted by `POST /ui-bridge/control/key`, in canonical
/// spelling. See the module docs for the `activeElement` hazard.
pub const ALLOWED_KEY_TARGETS: [&str; 4] = ["window", "document", "body", "activeElement"];

/// Modifier flags for a single keystroke. All default `false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyModifiers {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub meta: bool,
}

/// One keystroke: a `KeyboardEvent.key` value plus its modifier flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyStroke {
    pub key: String,
    #[serde(default)]
    pub modifiers: KeyModifiers,
}

/// Normalized `POST /ui-bridge/control/key` request — always the array form
/// with a canonical, validated `target`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DispatchKeyRequest {
    pub keys: Vec<KeyStroke>,
    pub target: String,
}

fn parse_key_entry(entry: &serde_json::Value) -> Result<KeyStroke, String> {
    match entry {
        serde_json::Value::String(s) if !s.is_empty() => Ok(KeyStroke {
            key: s.clone(),
            modifiers: KeyModifiers::default(),
        }),
        serde_json::Value::String(_) => {
            Err("each key entry must be a non-empty string".to_string())
        }
        serde_json::Value::Object(map) => {
            let key = map
                .get("key")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    "each key entry needs a non-empty `key` (e.g. {\"key\":\"B\"})".to_string()
                })?;
            let modifiers = match map.get("modifiers") {
                None | Some(serde_json::Value::Null) => KeyModifiers::default(),
                Some(v) => serde_json::from_value::<KeyModifiers>(v.clone())
                    .map_err(|e| format!("invalid `modifiers`: {e}"))?,
            };
            Ok(KeyStroke {
                key: key.to_string(),
                modifiers,
            })
        }
        _ => Err(
            "each key entry must be a string or an object like {\"key\":\"B\",\"modifiers\":{…}}"
                .to_string(),
        ),
    }
}

fn parse_target(body: &serde_json::Map<String, serde_json::Value>) -> Result<String, String> {
    match body.get("target") {
        None | Some(serde_json::Value::Null) => Ok(DEFAULT_KEY_TARGET.to_string()),
        Some(serde_json::Value::String(raw)) => ALLOWED_KEY_TARGETS
            .iter()
            .find(|c| c.eq_ignore_ascii_case(raw))
            .map(|c| (*c).to_string())
            .ok_or_else(|| {
                format!(
                    "unknown `target` '{raw}' — expected one of {}",
                    ALLOWED_KEY_TARGETS.join(", ")
                )
            }),
        Some(_) => Err("`target` must be a string".to_string()),
    }
}

/// Normalize an incoming `POST /ui-bridge/control/key` body.
///
/// Accepts the array form (`{"keys":[…]}`), the single-object shorthand
/// (`{"key":"B","modifiers":{…}}`), a single object under `keys`, and bare
/// strings inside `keys`. Returns a human-readable message on rejection (the
/// caller turns it into a 400).
pub fn parse_dispatch_key_request(body: &serde_json::Value) -> Result<DispatchKeyRequest, String> {
    let map = body.as_object().ok_or_else(|| {
        "request body must be a JSON object like {\"keys\":[{\"key\":\"B\",\"modifiers\":{\"ctrl\":true,\"shift\":true}}]}"
            .to_string()
    })?;

    let target = parse_target(map)?;

    let raw_keys = match map.get("keys") {
        Some(serde_json::Value::Array(items)) => {
            if items.is_empty() {
                return Err(MISSING_KEYS_MESSAGE.to_string());
            }
            items
                .iter()
                .map(parse_key_entry)
                .collect::<Result<Vec<_>, _>>()?
        }
        // Single-object / single-string shorthand nested under `keys`.
        Some(v @ serde_json::Value::Object(_)) | Some(v @ serde_json::Value::String(_)) => {
            vec![parse_key_entry(v)?]
        }
        Some(serde_json::Value::Null) | None => {
            // Top-level single-object shorthand: {"key":"B","modifiers":{…}}
            if map.contains_key("key") {
                vec![parse_key_entry(body)?]
            } else {
                return Err(MISSING_KEYS_MESSAGE.to_string());
            }
        }
        Some(_) => return Err(MISSING_KEYS_MESSAGE.to_string()),
    };

    Ok(DispatchKeyRequest {
        keys: raw_keys,
        target,
    })
}

const MISSING_KEYS_MESSAGE: &str = "`keys` is required and must be a non-empty array — e.g. \
     {\"keys\":[{\"key\":\"B\",\"modifiers\":{\"ctrl\":true,\"shift\":true}}]}. \
     The single-object shorthand {\"key\":\"B\",\"modifiers\":{…}} is also accepted.";

/// POST /ui-bridge/control/key
///
/// Dispatch a document/window-level `KeyboardEvent` sequence (`keydown`,
/// `keypress` for printable keys only, `keyup`) in the runner webview. This is
/// the only way UI Bridge automation can fire a global keyboard shortcut such
/// as `Ctrl+Shift+B` (session-manager sidebar) — element actions can't, because
/// the shortcut listeners are on `window`, not on any registered element.
///
/// Body: `{"keys":[{"key":"B","modifiers":{"ctrl":true,"shift":true}}],"target":"window"}`.
/// `target` is optional and defaults to `"window"`.
///
/// ⚠ **`target: "activeElement"` can type into whatever is focused** — on a
/// runner that is often a terminal bound to a live Claude/PowerShell session,
/// so an unintended dispatch injects text into real work. It is opt-in only;
/// `window` (the default) cannot do this. See the module docs.
///
/// Returns `{"dispatched": <n>, "target": "<canonical>", "defaultPrevented": <bool>}`,
/// where `defaultPrevented` reflects the LAST `keydown` and tells the caller
/// whether a handler actually consumed the shortcut.
pub async fn ui_bridge_dispatch_key_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let request = match parse_dispatch_key_request(&body) {
        Ok(r) => r,
        Err(msg) => return Err((StatusCode::BAD_REQUEST, Json(api_error(msg)))),
    };

    info!(
        "UI Bridge API: dispatch_key ({} key(s) → {})",
        request.keys.len(),
        request.target
    );

    let window_label = body.get("windowLabel").and_then(|v| v.as_str());
    let payload = target_window_payload(
        serde_json::json!({
            "params": {
                "keys": request.keys,
                "target": request.target,
            }
        }),
        window_label,
    );

    wrap_ipc_result(ui_bridge_request_sync(&state, "dispatch_key", payload).await)
}

// ============================================================================
// Routes + manifest
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route(
        "/ui-bridge/control/key",
        post(ui_bridge_dispatch_key_handler),
    )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[("POST", "/ui-bridge/control/key")]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_array_form_with_modifiers() {
        let req = parse_dispatch_key_request(&json!({
            "keys": [{ "key": "B", "modifiers": { "ctrl": true, "shift": true } }]
        }))
        .expect("array form should parse");
        assert_eq!(req.target, "window");
        assert_eq!(
            req.keys,
            vec![KeyStroke {
                key: "B".to_string(),
                modifiers: KeyModifiers {
                    ctrl: true,
                    shift: true,
                    alt: false,
                    meta: false
                },
            }]
        );
    }

    #[test]
    fn default_target_is_window() {
        let req = parse_dispatch_key_request(&json!({ "keys": [{ "key": "Escape" }] })).unwrap();
        assert_eq!(req.target, DEFAULT_KEY_TARGET);
        assert_eq!(req.target, "window");
        assert_eq!(req.keys[0].modifiers, KeyModifiers::default());
    }

    #[test]
    fn accepts_single_object_shorthand() {
        let req = parse_dispatch_key_request(&json!({
            "key": "B",
            "modifiers": { "ctrl": true, "shift": true }
        }))
        .expect("single-object shorthand should parse");
        assert_eq!(req.keys.len(), 1);
        assert_eq!(req.keys[0].key, "B");
        assert!(req.keys[0].modifiers.ctrl && req.keys[0].modifiers.shift);
        assert_eq!(req.target, "window");
    }

    #[test]
    fn accepts_single_object_nested_under_keys() {
        let req = parse_dispatch_key_request(&json!({ "keys": { "key": "Enter" } })).unwrap();
        assert_eq!(req.keys.len(), 1);
        assert_eq!(req.keys[0].key, "Enter");
    }

    #[test]
    fn accepts_bare_strings_in_keys_array() {
        let req = parse_dispatch_key_request(&json!({ "keys": ["Escape", "Enter"] })).unwrap();
        assert_eq!(req.keys.len(), 2);
        assert_eq!(req.keys[1].key, "Enter");
    }

    #[test]
    fn rejects_missing_keys() {
        let err = parse_dispatch_key_request(&json!({ "target": "window" })).unwrap_err();
        assert!(err.contains("`keys` is required"), "got: {err}");
    }

    #[test]
    fn rejects_empty_keys_array() {
        let err = parse_dispatch_key_request(&json!({ "keys": [] })).unwrap_err();
        assert!(err.contains("`keys` is required"), "got: {err}");
    }

    #[test]
    fn rejects_entry_without_key_field() {
        let err =
            parse_dispatch_key_request(&json!({ "keys": [{ "modifiers": { "ctrl": true } }] }))
                .unwrap_err();
        assert!(err.contains("non-empty `key`"), "got: {err}");
    }

    #[test]
    fn rejects_unknown_target() {
        let err =
            parse_dispatch_key_request(&json!({ "keys": [{ "key": "B" }], "target": "iframe" }))
                .unwrap_err();
        assert!(err.contains("unknown `target`"), "got: {err}");
        assert!(
            err.contains("activeElement"),
            "should list valid targets: {err}"
        );
    }

    #[test]
    fn accepts_every_documented_target_case_insensitively() {
        for target in ALLOWED_KEY_TARGETS {
            let req = parse_dispatch_key_request(&json!({
                "keys": [{ "key": "B" }],
                "target": target.to_lowercase()
            }))
            .unwrap_or_else(|e| panic!("target {target} should parse: {e}"));
            // Canonical spelling is restored (e.g. "activeelement" → "activeElement").
            assert_eq!(req.target, target);
        }
    }

    #[test]
    fn rejects_non_object_body() {
        let err = parse_dispatch_key_request(&json!(["B"])).unwrap_err();
        assert!(err.contains("must be a JSON object"), "got: {err}");
    }

    #[test]
    fn serialized_keystroke_matches_sdk_param_shape() {
        // The IPC payload must mirror the SDK's `sendKeys` param shape so a
        // later promotion into the SDK contract is a rename, not a redesign.
        let req = parse_dispatch_key_request(&json!({
            "keys": [{ "key": "B", "modifiers": { "ctrl": true, "shift": true } }]
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&req.keys).unwrap(),
            json!([{
                "key": "B",
                "modifiers": { "ctrl": true, "shift": true, "alt": false, "meta": false }
            }])
        );
    }
}
