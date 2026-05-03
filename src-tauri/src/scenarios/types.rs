//! Wire types for scenario projection. Mirrors the TS interfaces in
//! `ui-bridge-auto/src/state/scenario-projection.ts`.
//!
//! These shapes are stable JSON contracts consumed by the Phase B3 MCP
//! tools (Python). Field names use camelCase via
//! `serde(rename_all = "camelCase")` to match the TS output byte-for-byte.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Static (deterministic) projection
// ---------------------------------------------------------------------------

/// One projected transition emanating from a state. Mirror of
/// `ProjectedTransition` (TS). Action *content* is intentionally omitted —
/// clients that need the action body should resolve it against the IR.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTransition {
    pub transition_id: String,
    /// Only emitted when distinct from `transition_id` — the IR convention
    /// is to default `name = id` when no human-readable label exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `IrTransition.activate_states`, sorted ascending.
    pub target_state_ids: Vec<String>,
    /// `IrTransition.actions.len()`.
    pub action_count: usize,
}

/// One projected state. Mirror of `ProjectedState` (TS).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedState {
    pub state_id: String,
    /// Only emitted when distinct from `state_id` — see [`ProjectedTransition::label`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub required_element_count: usize,
    /// Outbound transitions, sorted by `transition_id` ascending.
    pub outbound_transitions: Vec<ProjectedTransition>,
}

/// Static, deterministic projection. Same `IrDocument` input → byte-identical
/// JSON output via the canonical sort discipline. Mirror of `ScenarioProjection` (TS).
///
/// The `deterministic: true` field is a load-bearing marker — Phase B3 tools
/// can persist this artifact safely. Compare with [`CurrentScenarioProjection`]
/// which carries `deterministic: false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioProjection {
    /// All states, sorted by `state_id` ascending.
    pub states: Vec<ProjectedState>,
    /// Always `true`. Serialized as the literal `true` (not a string).
    pub deterministic: bool,
}

// ---------------------------------------------------------------------------
// Runtime-aware projection (wire-shape only — payload comes from the webview)
// ---------------------------------------------------------------------------

/// One transition the runtime engine could fire right now. Mirror of
/// `AvailableTransition` (TS).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AvailableTransition {
    pub transition_id: String,
    pub from_state_id: String,
    /// `IrTransition.activate_states`, sorted ascending.
    pub target_state_ids: Vec<String>,
}

/// One transition the runtime engine cannot fire right now. Mirror of
/// `BlockedTransition` (TS).
///
/// `cause` is one of `"no-match"`, `"ambiguous"`, or `"predicate-failed"`.
/// The Rust side keeps it as a `String` so we don't have to maintain a
/// parallel enum in lockstep with the TS source — the wire contract is
/// the only thing that matters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BlockedTransition {
    pub transition_id: String,
    pub from_state_id: String,
    pub cause: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Runtime-aware projection. Mirror of `CurrentScenarioProjection` (TS).
///
/// Non-deterministic by design — the output depends on the live registry's
/// `getAllElements()` snapshot. The `deterministic: false` marker is the
/// load-bearing distinguisher from [`ScenarioProjection`].
///
/// We keep the Rust mirror to validate the IPC response shape; the actual
/// payload returned to HTTP callers passes through as
/// `serde_json::Value` to avoid a round-trip parse.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CurrentScenarioProjection {
    pub states: Vec<ProjectedState>,
    /// State ids whose required elements resolve in the live registry,
    /// sorted ascending.
    pub current_state_ids: Vec<String>,
    pub available_transitions: Vec<AvailableTransition>,
    pub blocked_transitions: Vec<BlockedTransition>,
    /// Always `false`.
    pub deterministic: bool,
}
