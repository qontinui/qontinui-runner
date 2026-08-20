//! Document/window-level key dispatch.
//!
//! Covers:
//!   - `POST /ui-bridge/control/key` — the runner's own literal-key form.
//!   - `POST /ui-bridge/control/page/send-keys` — the SDK-declared
//!     `sendKeysToPage` contract (combo-string grammar, `document` default,
//!     `delay`, per-key `outcomes[]`). Added because ui-bridge 912a3e301
//!     (2026-08-20) wired the SDK side under this path rather than promoting
//!     `/control/key`; both go through the same `dispatch_key` IPC.
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

/// Default dispatch target for `POST /ui-bridge/control/page/send-keys`.
///
/// Deliberately **not** `DEFAULT_KEY_TARGET`. The SDK's `sendKeysToPage`
/// defaults to `document` (ui-bridge
/// `packages/ui-bridge/src/server/page-primitives.ts`), and for an
/// SDK-declared route the SDK contract is the source of truth. A
/// `document`-dispatched event still reaches `window` listeners by bubbling,
/// so the runner's global shortcuts remain reachable through this route too.
pub const DEFAULT_PAGE_KEY_TARGET: &str = "document";

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
    parse_target_with_default(body, DEFAULT_KEY_TARGET)
}

fn parse_target_with_default(
    body: &serde_json::Map<String, serde_json::Value>,
    default: &str,
) -> Result<String, String> {
    match body.get("target") {
        None | Some(serde_json::Value::Null) => Ok(default.to_string()),
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

// ============================================================================
// SDK combo-string grammar (`"ctrl+Enter"`)
//
// Mirrors ui-bridge `packages/ui-bridge/src/core/key-events.ts`. It backs the
// SDK-declared `POST /ui-bridge/control/page/send-keys` route ONLY —
// `/control/key`'s bare-string form stays a literal key name so no existing
// caller changes meaning.
//
// Validation is strict on purpose, exactly as the SDK is: a misspelled key
// dispatched verbatim "succeeds" while matching no listener, which is the
// silently-wrong answer this route exists to rule out. The explicit descriptor
// form (`[{"key":"…"}]`) still bypasses vocabulary validation so an exotic key
// stays reachable.
// ============================================================================

/// Modifier token → canonical flag name. Mirrors the SDK's `MODIFIER_TOKENS`.
fn modifier_token(token: &str) -> Option<&'static str> {
    match token.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "ctl" => Some("ctrl"),
        "shift" => Some("shift"),
        "alt" | "option" => Some("alt"),
        "meta" | "cmd" | "command" | "super" | "win" => Some("meta"),
        _ => None,
    }
}

/// Every modifier token the grammar accepts, for error messages.
const MODIFIER_TOKEN_LIST: &str =
    "ctrl, control, ctl, shift, alt, option, meta, cmd, command, super, win";

/// Friendly aliases for keys whose DOM value is awkward to type in JSON.
/// Mirrors the SDK's `KEY_ALIASES`.
fn key_alias(key: &str) -> Option<&'static str> {
    match key.to_ascii_lowercase().as_str() {
        "space" | "spacebar" => Some(" "),
        "esc" => Some("Escape"),
        "del" => Some("Delete"),
        "return" => Some("Enter"),
        _ => None,
    }
}

/// Named `KeyboardEvent.key` values the string grammar accepts. Mirrors the
/// SDK's `KNOWN_KEY_NAMES` (= `NON_PRINTABLE_KEYS` plus the editing/system
/// names). Single characters and `F1`–`F24` are accepted separately.
const KNOWN_KEY_NAMES: &[&str] = &[
    "Enter",
    "Tab",
    "Escape",
    "Backspace",
    "Delete",
    "Insert",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Control",
    "Shift",
    "Alt",
    "Meta",
    "CapsLock",
    "NumLock",
    "ScrollLock",
    "ContextMenu",
    "Clear",
    "Pause",
    "PrintScreen",
    "Help",
    "AltGraph",
    "Cancel",
    "Undo",
    "Redo",
    "Copy",
    "Cut",
    "Paste",
    "Select",
    "Fn",
    "Symbol",
];

/// `true` for a key name the string grammar will dispatch: one character, a
/// known name, or `F1`–`F24`.
fn is_known_key_name(key: &str) -> bool {
    if key.chars().count() == 1 {
        return true;
    }
    if KNOWN_KEY_NAMES.contains(&key) {
        return true;
    }
    // F1–F24
    match key.strip_prefix('F') {
        Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
            matches!(n.parse::<u32>(), Ok(v) if (1..=24).contains(&v))
        }
        _ => false,
    }
}

/// Parse one combo token: optional `+`-joined modifier prefixes followed by a
/// key name (`"Escape"`, `"ctrl+Enter"`, `"a"`, `"+"`).
fn parse_key_combo(token: &str) -> Result<KeyStroke, String> {
    let raw = token.trim();
    if raw.is_empty() {
        return Err("empty key token".to_string());
    }
    // `"+"` (and any all-separator token) is the literal plus key, not a combo.
    if raw.chars().all(|c| c == '+') {
        return Ok(KeyStroke {
            key: "+".to_string(),
            modifiers: KeyModifiers::default(),
        });
    }

    let parts: Vec<&str> = raw.split('+').collect();
    let mut modifiers = KeyModifiers::default();
    // `split` on a non-empty string always yields at least one part.
    let key_part = parts[parts.len() - 1];
    for part in &parts[..parts.len() - 1] {
        match modifier_token(part) {
            Some("ctrl") => modifiers.ctrl = true,
            Some("shift") => modifiers.shift = true,
            Some("alt") => modifiers.alt = true,
            Some("meta") => modifiers.meta = true,
            _ => {
                return Err(format!(
                "unknown modifier \"{part}\" in key combo \"{raw}\" (valid: {MODIFIER_TOKEN_LIST})"
            ))
            }
        }
    }

    let key = key_alias(key_part)
        .map(|a| a.to_string())
        .unwrap_or_else(|| key_part.to_string());
    if !is_known_key_name(&key) {
        return Err(format!(
            "unknown key name \"{key}\" in \"{raw}\". Use a DOM KeyboardEvent.key value \
             (e.g. \"Escape\", \"Enter\", \"Tab\", \"ArrowDown\", \"F5\", or a single character), \
             or pass the explicit descriptor form [{{\"key\":\"{key}\"}}] to bypass this check."
        ));
    }
    Ok(KeyStroke { key, modifiers })
}

/// Normalized `POST /ui-bridge/control/page/send-keys` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendKeysToPageRequest {
    pub keys: Vec<KeyStroke>,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<u64>,
}

const SEND_KEYS_REQUIRED_MESSAGE: &str =
    "`keys` is required and must be a string (\"Escape\", \"ctrl+Enter\"), an array of such \
     strings, or an array of {\"key\":\"…\",\"modifiers\":{…}} descriptors";

/// Normalize an incoming `POST /ui-bridge/control/page/send-keys` body against
/// the SDK contract: `keys` accepts the combo-string grammar, `target` defaults
/// to `document`, and an unrecognized `target` is an error rather than a silent
/// fallback.
///
/// A bare string is deliberately ONE key press, never a character sequence —
/// re-reading `"Escape"` as six characters is the misinterpretation this route
/// exists to avoid. Type text with the element-scoped actions instead.
pub fn parse_send_keys_to_page_request(
    body: &serde_json::Value,
) -> Result<SendKeysToPageRequest, String> {
    let map = body.as_object().ok_or_else(|| {
        format!("request body must be a JSON object — {SEND_KEYS_REQUIRED_MESSAGE}")
    })?;

    let target = parse_target_with_default(map, DEFAULT_PAGE_KEY_TARGET)?;

    let delay = match map.get("delay") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(ms) => Some(ms),
            None => return Err("`delay` must be a non-negative whole number of ms".to_string()),
        },
        Some(_) => return Err("`delay` must be a number (ms between keys)".to_string()),
    };

    let keys = match map.get("keys") {
        Some(serde_json::Value::String(s)) => vec![parse_key_combo(s)?],
        Some(serde_json::Value::Array(items)) => {
            if items.is_empty() {
                return Err("`keys` array must not be empty".to_string());
            }
            items
                .iter()
                .map(|entry| match entry {
                    // Strings go through the validated combo grammar; the
                    // explicit descriptor form stays unvalidated by design.
                    serde_json::Value::String(s) => parse_key_combo(s),
                    other => parse_key_entry(other),
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        _ => return Err(SEND_KEYS_REQUIRED_MESSAGE.to_string()),
    };

    Ok(SendKeysToPageRequest {
        keys,
        target,
        delay,
    })
}

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

/// POST /ui-bridge/control/page/send-keys
///
/// The SDK-declared document-level key dispatch (`sendKeysToPage`, ui-bridge
/// `UI_BRIDGE_ROUTES`). Same underlying dispatch as `/control/key` — the
/// differences are the ones the SDK contract specifies:
///
///   * `keys` accepts the combo-string grammar (`"Escape"`, `"ctrl+Enter"`,
///     `["Escape","Tab"]`) with strict key-name validation, alongside the
///     explicit `[{"key":…,"modifiers":{…}}]` descriptor form.
///   * `target` defaults to `document` (not `/control/key`'s `window`), and an
///     unrecognized value is a 400 rather than a silent fallback.
///   * `delay` (ms) paces the sequence.
///   * The response carries per-key `outcomes[].defaultPrevented`, so a caller
///     can prove a listener consumed the key.
///
/// ⚠ `target: "activeElement"` can type into whatever is focused — see the
/// module docs. It is opt-in only here too.
pub async fn ui_bridge_send_keys_to_page_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let request = match parse_send_keys_to_page_request(&body) {
        Ok(r) => r,
        Err(msg) => return Err((StatusCode::BAD_REQUEST, Json(api_error(msg)))),
    };

    info!(
        "UI Bridge API: send_keys_to_page ({} key(s) → {})",
        request.keys.len(),
        request.target
    );

    let window_label = body.get("windowLabel").and_then(|v| v.as_str());
    let mut params = serde_json::json!({
        "keys": request.keys,
        "target": request.target,
    });
    if let Some(delay) = request.delay {
        params["delay"] = serde_json::json!(delay);
    }
    let payload = target_window_payload(serde_json::json!({ "params": params }), window_label);

    wrap_ipc_result(ui_bridge_request_sync(&state, "dispatch_key", payload).await)
}

// ============================================================================
// Routes + manifest
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new()
        .route(
            "/ui-bridge/control/key",
            post(ui_bridge_dispatch_key_handler),
        )
        .route(
            "/ui-bridge/control/page/send-keys",
            post(ui_bridge_send_keys_to_page_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/control/key"),
        ("POST", "/ui-bridge/control/page/send-keys"),
    ]
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

    // ── POST /ui-bridge/control/page/send-keys (SDK contract) ───────────

    #[test]
    fn send_keys_bare_string_is_one_key_not_a_character_sequence() {
        let req = parse_send_keys_to_page_request(&json!({ "keys": "Escape" })).unwrap();
        assert_eq!(req.keys.len(), 1, "\"Escape\" must be ONE key, not six");
        assert_eq!(req.keys[0].key, "Escape");
    }

    #[test]
    fn send_keys_parses_the_combo_string_grammar() {
        let req = parse_send_keys_to_page_request(&json!({ "keys": "ctrl+shift+Enter" })).unwrap();
        assert_eq!(req.keys[0].key, "Enter");
        assert_eq!(
            req.keys[0].modifiers,
            KeyModifiers {
                ctrl: true,
                shift: true,
                alt: false,
                meta: false
            }
        );
    }

    #[test]
    fn send_keys_accepts_modifier_aliases() {
        for (combo, expected) in [
            (
                "control+a",
                KeyModifiers {
                    ctrl: true,
                    ..Default::default()
                },
            ),
            (
                "cmd+a",
                KeyModifiers {
                    meta: true,
                    ..Default::default()
                },
            ),
            (
                "option+a",
                KeyModifiers {
                    alt: true,
                    ..Default::default()
                },
            ),
            (
                "win+a",
                KeyModifiers {
                    meta: true,
                    ..Default::default()
                },
            ),
        ] {
            let req = parse_send_keys_to_page_request(&json!({ "keys": combo })).unwrap();
            assert_eq!(req.keys[0].modifiers, expected, "combo {combo}");
        }
    }

    #[test]
    fn send_keys_resolves_key_aliases() {
        for (input, expected) in [
            ("esc", "Escape"),
            ("del", "Delete"),
            ("return", "Enter"),
            ("space", " "),
        ] {
            let req = parse_send_keys_to_page_request(&json!({ "keys": input })).unwrap();
            assert_eq!(req.keys[0].key, expected, "alias {input}");
        }
    }

    #[test]
    fn send_keys_treats_a_bare_plus_as_the_literal_plus_key() {
        let req = parse_send_keys_to_page_request(&json!({ "keys": "+" })).unwrap();
        assert_eq!(req.keys[0].key, "+");
        assert_eq!(req.keys[0].modifiers, KeyModifiers::default());
    }

    #[test]
    fn send_keys_accepts_function_keys_up_to_f24() {
        for k in ["F1", "F5", "F12", "F24"] {
            parse_send_keys_to_page_request(&json!({ "keys": k }))
                .unwrap_or_else(|e| panic!("{k} should parse: {e}"));
        }
        let err = parse_send_keys_to_page_request(&json!({ "keys": "F25" })).unwrap_err();
        assert!(err.contains("unknown key name"), "got: {err}");
    }

    #[test]
    fn send_keys_rejects_a_misspelled_key_name() {
        // The whole point of the strict grammar: dispatching "Excape" would
        // "succeed" while matching no listener.
        let err = parse_send_keys_to_page_request(&json!({ "keys": "Excape" })).unwrap_err();
        assert!(err.contains("unknown key name"), "got: {err}");
        assert!(
            err.contains("descriptor form"),
            "should name the escape hatch: {err}"
        );
    }

    #[test]
    fn send_keys_rejects_an_unknown_modifier() {
        let err = parse_send_keys_to_page_request(&json!({ "keys": "hyper+Enter" })).unwrap_err();
        assert!(err.contains("unknown modifier"), "got: {err}");
    }

    #[test]
    fn send_keys_descriptor_form_bypasses_vocabulary_validation() {
        // An exotic key must stay reachable through the explicit form.
        let req =
            parse_send_keys_to_page_request(&json!({ "keys": [{ "key": "MediaPlayPause" }] }))
                .unwrap();
        assert_eq!(req.keys[0].key, "MediaPlayPause");
    }

    #[test]
    fn send_keys_accepts_a_mixed_array() {
        let req = parse_send_keys_to_page_request(
            &json!({ "keys": ["Escape", { "key": "Tab", "modifiers": { "shift": true } }] }),
        )
        .unwrap();
        assert_eq!(req.keys.len(), 2);
        assert_eq!(req.keys[0].key, "Escape");
        assert!(req.keys[1].modifiers.shift);
    }

    #[test]
    fn send_keys_defaults_target_to_document_not_window() {
        // The SDK contract's default, deliberately different from
        // /control/key's `window`.
        let req = parse_send_keys_to_page_request(&json!({ "keys": "Escape" })).unwrap();
        assert_eq!(req.target, DEFAULT_PAGE_KEY_TARGET);
        assert_eq!(req.target, "document");
        assert_ne!(req.target, DEFAULT_KEY_TARGET);
    }

    #[test]
    fn send_keys_rejects_an_unknown_target_rather_than_falling_back() {
        let err = parse_send_keys_to_page_request(&json!({ "keys": "Escape", "target": "iframe" }))
            .unwrap_err();
        assert!(err.contains("unknown `target`"), "got: {err}");
    }

    #[test]
    fn send_keys_carries_delay_through() {
        let req =
            parse_send_keys_to_page_request(&json!({ "keys": ["a", "b"], "delay": 25 })).unwrap();
        assert_eq!(req.delay, Some(25));
        let err =
            parse_send_keys_to_page_request(&json!({ "keys": "a", "delay": "fast" })).unwrap_err();
        assert!(err.contains("`delay`"), "got: {err}");
    }

    #[test]
    fn send_keys_rejects_missing_and_empty_keys() {
        let err = parse_send_keys_to_page_request(&json!({ "target": "document" })).unwrap_err();
        assert!(err.contains("`keys` is required"), "got: {err}");
        let err = parse_send_keys_to_page_request(&json!({ "keys": [] })).unwrap_err();
        assert!(err.contains("must not be empty"), "got: {err}");
    }

    #[test]
    fn send_keys_route_is_registered_in_the_manifest() {
        // The SDK↔runner manifest drift test reads route_entries(); keep the
        // two in lockstep here so a dropped .route() call is caught locally.
        assert!(route_entries().contains(&("POST", "/ui-bridge/control/page/send-keys")));
    }

    #[test]
    fn control_key_bare_string_stays_a_literal_key_name() {
        // /control/key must NOT gain the combo grammar — an existing caller
        // sending a literal key name must keep its meaning.
        let req = parse_dispatch_key_request(&json!({ "keys": ["ctrl+Enter"] })).unwrap();
        assert_eq!(req.keys[0].key, "ctrl+Enter");
        assert_eq!(req.keys[0].modifiers, KeyModifiers::default());
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
