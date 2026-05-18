//! Wire-format types for runner-to-backend WebSocket relay envelopes and
//! backend-originated runner-status events.
//!
//! ## Why this module exists
//!
//! `mcp/backend_relay.rs::handle_outbound` previously rewrapped Tauri-side
//! `AppEvent`s into ad-hoc `serde_json::json!({...})` literals before
//! shipping them over the WebSocket relay. The web + mobile consumers then
//! hand-typed matching `WsMessage` interfaces — a pattern that recently
//! caused 3 silently-broken WS handlers (audited under
//! `qontinui-web@ba7f38d2`).
//!
//! This module is the single source of truth for those WS envelope shapes.
//! Bindings generated from these structs flow to the frontend via
//! `qontinui-schemas/ts/src/generated/` so handlers in the React/RN layers
//! can `as RunnerRelayMessage` (a discriminated union) instead of writing
//! their own.
//!
//! ## Two families of types live here
//!
//! ### 1. Runner-originated relay envelopes (Part A)
//!
//! `RunnerRelayMessage` — the discriminated union of every envelope shape
//! `handle_outbound` ships to the backend. Variants use snake_case
//! (`#[serde(rename_all = "snake_case")]`) because the existing `json!`
//! literals all use snake_case keys (`terminal_id`, `exit_code`, etc.).
//!
//! ### 2. Backend-originated runner-status events (Part B)
//!
//! `RunnerStatusEvent` — the discriminated union of events the Python
//! backend (`qontinui-web/backend/app/services/runner/event_publisher.py`)
//! pushes to clients on the per-user Redis pub/sub channel. Python is the
//! source of truth here; we mirror the wire format in Rust so TS bindings
//! exist without coupling the web frontend to Pydantic. The Python emitter
//! references this module by name in its docstring as the contract.
//!
//! ## Adding a new envelope
//!
//! 1. Add a variant to `RunnerRelayMessage` (or `RunnerStatusEvent`).
//! 2. Register the union in `schema_export.rs`.
//! 3. Run `npm run gen-events` from `qontinui-runner/` to regenerate TS.
//! 4. If you're adding a backend-originated event, also update the matching
//!    publish_* method in `event_publisher.py` (Python contract, mirrored).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ===========================================================================
// Part A — Runner → backend relay envelopes
// ===========================================================================

/// Discriminated union of every WS envelope `mcp/backend_relay.rs::handle_outbound`
/// ships to the backend after rewrapping a Tauri-side `AppEvent`.
///
/// Internally tagged on `"type"` with snake_case variant names because that's
/// what the prior `serde_json::json!` literals emitted on the wire.
///
/// Wire keys are snake_case (NOT camelCase) — verified against the literal
/// strings in `handle_outbound` lines ~605-638. Consumers that expect
/// camelCase (e.g. some web hooks) are wrong; the relay never emitted
/// camelCase here.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerRelayMessage {
    /// Phase completion result. Emitted from the `phase-result` broadcast
    /// channel; `data` carries the original `AppEvent` payload
    /// (`{ execution_id, result }`).
    PhaseCompleted {
        #[serde(default)]
        data: Value,
    },

    /// UI error reported by the runner. Emitted from the `ui-error` broadcast
    /// channel; `data` carries the original `AppEvent` payload (matches
    /// `qontinui_types::device::DeviceUiError`'s wire shape — renamed from
    /// `RunnerUiError` in the Unified Devices Registry Phase 7 schemas bump).
    /// Always present (serializes as `null` when absent) to match the legacy
    /// `json!` literal.
    UiError {
        #[serde(default)]
        data: Value,
    },

    /// Most-recent crash dump metadata. Emitted from the `recent-crash`
    /// broadcast channel; `data` carries the original `AppEvent` payload
    /// (matches `qontinui_types::device::DeviceCrash`'s wire shape — renamed
    /// from `RunnerCrash` in the Unified Devices Registry Phase 7 schemas
    /// bump).
    RecentCrash {
        #[serde(default)]
        data: Value,
    },

    /// AI session output forwarded from the `ai-output` broadcast channel
    /// (mobile chat sessions). `data` carries the original `AiOutput`
    /// `AppEvent` payload.
    ChatResponse {
        #[serde(default)]
        data: Value,
    },

    /// AI session lifecycle state from the `session-state` broadcast channel.
    /// `data` carries the original `AppEvent` payload (state transitions for
    /// the mobile chat UI).
    ChatSessionState {
        #[serde(default)]
        data: Value,
    },

    /// PTY output from the `terminal-output` broadcast channel. The relay
    /// pulls the inner `terminal_id` and `data` out of the AppEvent payload
    /// and ships them at the envelope's top level (NOT under a `data` key).
    /// `data` here is the base64-encoded PTY bytes — same shape as
    /// `qontinui_types::terminal::TerminalOutputEvent.data`. Always present
    /// (serialize as `null` when absent) to match legacy literal output.
    TerminalOutput {
        #[serde(default)]
        terminal_id: Value,
        #[serde(default)]
        data: Value,
    },

    /// PTY exit signal from the `terminal-exit` broadcast channel. As above,
    /// the relay flattens `terminal_id` + `exit_code` to the envelope's top
    /// level rather than nesting under a `data` field.
    TerminalExit {
        #[serde(default)]
        terminal_id: Value,
        #[serde(default)]
        exit_code: Value,
    },
}

// ===========================================================================
// Part B — Backend (Python) → client runner-status events
// ===========================================================================
//
// These mirror the JSON payloads published by
// `qontinui-web/backend/app/services/runner/event_publisher.py` on the
// per-user Redis channel `runner:status:updates:{user_id}`, plus the
// `initial_state` snapshot pushed by `runner_status_ws.py` on connect.
//
// Python is the source of truth for what hits the wire — we mirror it here
// so TS bindings exist. The Python module's docstring references this Rust
// type by name as the wire contract; if you change one, change the other.

/// Wire-format runner shape carried inside `runner_connected.connection`.
///
/// This is intentionally a partial Device payload (snake_case, only the
/// fields the Python emitter actually populates in
/// `RunnerEventPublisher.publish_runner_connected`). It is NOT
/// `qontinui_types::device::Device` — that shape requires `derived_status`,
/// `capabilities`, `created_at` etc., which the connection payload doesn't
/// include. Consumers reading this should refetch the canonical Device row
/// via REST after seeing `runner_connected` (the legacy `runner_*` event
/// names are preserved on the wire; the Phase 7 schemas rename renamed the
/// DTO type but not the event identifiers).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "RunnerConnectedConnection")]
pub struct RunnerConnectedConnection {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_at: Option<String>,
    /// Always present as `null` on the wire when the runner is still
    /// connected. Python ships this field unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disconnected_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_port: Option<u16>,
    #[serde(default)]
    pub ws_connected: bool,
}

/// Discriminated union of every event the Python backend pushes to the web
/// client over the `/api/v1/runners/status` WebSocket.
///
/// Internally tagged on `"type"` matching the Python wire format. Variant
/// renames (`runner.woke` keeps the dot rather than going to snake_case)
/// preserve the literal strings the emitter sends.
///
/// Source of truth: the `publish_*` methods in
/// `qontinui-web/backend/app/services/runner/event_publisher.py` and the
/// `initial_state` send in `runner_status_ws.py`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum RunnerStatusEvent {
    /// Snapshot pushed on WS connect. Each entry is a full canonical Device
    /// (as serialized by `qontinui_schemas.device.Device` Pydantic — renamed
    /// from `Runner` in the Unified Devices Registry Phase 7 schemas bump).
    /// Carried here as `Value` because the canonical `Device` lives in
    /// `qontinui_types::device::Device`; the TS binding consumer should
    /// narrow `runners[i] as Device` after import. The wire field name
    /// `runners` is preserved for back-compat with the Python emitter.
    #[serde(rename = "initial_state")]
    InitialState { runners: Vec<Value> },

    /// A runner WS handshake completed. `connection` is the partial
    /// connection payload; refetch via REST to hydrate to full Runner.
    #[serde(rename = "runner_connected")]
    RunnerConnected {
        connection: RunnerConnectedConnection,
        timestamp: String,
    },

    /// A runner WS dropped.
    #[serde(rename = "runner_disconnected")]
    RunnerDisconnected {
        runner_id: String,
        timestamp: String,
    },

    /// Runner sent updated `runner_name` after handshake.
    #[serde(rename = "runner_name_updated")]
    RunnerNameUpdated {
        runner_id: String,
        runner_name: String,
        timestamp: String,
    },

    /// Runner sent updated `runner_port` after handshake.
    #[serde(rename = "runner_port_updated")]
    RunnerPortUpdated {
        runner_id: String,
        runner_port: u16,
        timestamp: String,
    },

    /// Wake-intent fulfilled — the WakeRunnerModal listens for this to
    /// transition out of "waking" state.
    #[serde(rename = "runner.woke")]
    RunnerWoke {
        runner_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        timestamp: String,
    },

    /// Server-side error emitted by the WS endpoint (e.g. failed to load
    /// initial state). Treated as a soft warning by clients; the connection
    /// stays open.
    #[serde(rename = "error")]
    Error { error: String },
}

// ===========================================================================
// Wire-format golden tests
// ===========================================================================
//
// These tests pin the exact JSON the typed structs emit so the
// schematization in Part A doesn't accidentally diverge from the wire
// shapes the previous `serde_json::json!` literals produced.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn relay_phase_completed_matches_legacy_literal() {
        let typed = RunnerRelayMessage::PhaseCompleted {
            data: json!({ "execution_id": "e1", "result": "ok" }),
        };
        let v = serde_json::to_value(&typed).unwrap();
        assert_eq!(
            v,
            json!({
                "type": "phase_completed",
                "data": { "execution_id": "e1", "result": "ok" }
            })
        );
    }

    #[test]
    fn relay_terminal_output_flattens_terminal_id_and_data() {
        let typed = RunnerRelayMessage::TerminalOutput {
            terminal_id: json!("t-1"),
            data: json!("base64=="),
        };
        let v = serde_json::to_value(&typed).unwrap();
        assert_eq!(
            v,
            json!({
                "type": "terminal_output",
                "terminal_id": "t-1",
                "data": "base64=="
            })
        );
    }

    #[test]
    fn relay_terminal_exit_includes_exit_code() {
        let typed = RunnerRelayMessage::TerminalExit {
            terminal_id: json!("t-2"),
            exit_code: json!(0),
        };
        let v = serde_json::to_value(&typed).unwrap();
        assert_eq!(
            v,
            json!({
                "type": "terminal_exit",
                "terminal_id": "t-2",
                "exit_code": 0
            })
        );
    }

    #[test]
    fn relay_ui_error_carries_data_payload() {
        let typed = RunnerRelayMessage::UiError {
            data: json!({ "kind": "build_failed", "message": "rustc failed" }),
        };
        let v = serde_json::to_value(&typed).unwrap();
        assert_eq!(
            v,
            json!({
                "type": "ui_error",
                "data": { "kind": "build_failed", "message": "rustc failed" }
            })
        );
    }

    #[test]
    fn relay_ui_error_emits_null_data_when_absent() {
        let typed = RunnerRelayMessage::UiError { data: Value::Null };
        let v = serde_json::to_value(&typed).unwrap();
        // Wire-compat with the legacy `serde_json::json!` literal which
        // emitted `"data": null` (NOT a missing key) when the broadcast
        // payload was absent. Web + mobile consumers may rely on the key
        // being present for `message.data ?? fallback` patterns.
        assert_eq!(v, json!({ "type": "ui_error", "data": null }));
    }

    #[test]
    fn status_event_runner_connected_uses_runner_underscore_id_not_dot() {
        let typed = RunnerStatusEvent::RunnerConnected {
            connection: RunnerConnectedConnection {
                id: "r-1".into(),
                runner_name: Some("Desktop".into()),
                connected_at: Some("2026-05-03T12:00:00Z".into()),
                disconnected_at: None,
                duration_seconds: None,
                ip_address: None,
                project_id: None,
                runner_port: Some(7860),
                ws_connected: true,
            },
            timestamp: "2026-05-03T12:00:00Z".into(),
        };
        let v = serde_json::to_value(&typed).unwrap();
        assert_eq!(v["type"], "runner_connected");
        assert_eq!(v["connection"]["id"], "r-1");
        assert_eq!(v["connection"]["runner_port"], 7860);
        assert_eq!(v["connection"]["ws_connected"], true);
    }

    #[test]
    fn status_event_runner_woke_keeps_dot_in_type() {
        let typed = RunnerStatusEvent::RunnerWoke {
            runner_id: "r-1".into(),
            intent_id: Some("i-1".into()),
            task_id: None,
            reason: Some("boot".into()),
            timestamp: "2026-05-03T12:00:00Z".into(),
        };
        let v = serde_json::to_value(&typed).unwrap();
        assert_eq!(v["type"], "runner.woke");
        assert_eq!(v["runner_id"], "r-1");
        assert_eq!(v["intent_id"], "i-1");
        assert!(v.get("task_id").is_none(), "None task_id must be omitted");
        assert_eq!(v["reason"], "boot");
    }

    #[test]
    fn status_event_initial_state_carries_runners_array() {
        let typed = RunnerStatusEvent::InitialState {
            runners: vec![json!({ "id": "r-1" }), json!({ "id": "r-2" })],
        };
        let v = serde_json::to_value(&typed).unwrap();
        assert_eq!(v["type"], "initial_state");
        assert_eq!(v["runners"][0]["id"], "r-1");
        assert_eq!(v["runners"][1]["id"], "r-2");
    }
}
