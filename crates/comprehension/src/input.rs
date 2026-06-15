//! Input substrate the comprehension worker consumes — a UI Bridge **snapshot**
//! and a discovery **StateDiscoveryResult**.
//!
//! These are *lossy mirrors* of the runner's live `ui_bridge::get_snapshot`
//! element registry (`mcp/ui_bridge/elements.rs`) and the Python discovery
//! `StateDiscoveryResult` (`discovery/state_discovery/base.py`). We mirror only
//! the fields comprehension reads, deserialized permissively (`#[serde(default)]`
//! everywhere, no `deny_unknown_fields`) so a fuller live document still parses.
//!
//! Authoring note: these types exist so Phase 1 can run the *deterministic*
//! mapping against a committed fixture without a live page. The live capture
//! path (driving UI Bridge + discovery against `app.qontinui.io/connect-runner`)
//! is runtime-deferred — see `worker::capture_inputs`.

use serde::{Deserialize, Serialize};

/// A UI Bridge snapshot: a flat element registry plus the page URL. Mirrors the
/// shape `ui_bridge_get_snapshot_handler` returns (`role` / `accessibleName` /
/// `tagName` / `text` / `state` / `actions` / `bounds` / `parent` / `page`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Page URL the snapshot was captured from.
    #[serde(default)]
    pub page: String,
    /// Flat element registry.
    #[serde(default)]
    pub elements: Vec<SnapshotElement>,
    /// Forms surfaced by `get_forms` — field names/types/validation observed in
    /// the live DOM. Optional; absent on pages with no forms.
    #[serde(default)]
    pub forms: Vec<SnapshotForm>,
}

/// One element of a UI Bridge snapshot. Only the comprehension-relevant fields
/// are mirrored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotElement {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub tag_name: Option<String>,
    #[serde(default)]
    pub accessible_name: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    /// Action verbs the element supports (`click`, `type`, `submit`, …).
    #[serde(default)]
    pub actions: Vec<String>,
}

/// A form observed on the page via `get_forms`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotForm {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub fields: Vec<SnapshotFormField>,
}

/// A field of a snapshot form.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFormField {
    #[serde(default)]
    pub name: String,
    /// Coarse type the DOM reveals (`text`, `email`, `url`, `hidden`, …).
    #[serde(rename = "type", default)]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    /// Client-side validation rule observed firing, when any.
    #[serde(default)]
    pub validation: Option<String>,
}

/// A discovery `StateDiscoveryResult` (subset). Mirrors
/// `discovery/state_discovery/base.py::StateDiscoveryResult.to_dict()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryResult {
    #[serde(default)]
    pub states: Vec<DiscoveredState>,
    #[serde(default)]
    pub transitions: Vec<DiscoveredTransition>,
}

/// A discovered (fingerprint-clustered) UI state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredState {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_initial: bool,
    /// Observable element/text checks the comprehender turns into
    /// `IrAssertion`s. Authored here from the clustered evidence; in the live
    /// path these come from the discovery clusters' representative elements.
    #[serde(default)]
    pub observable_checks: Vec<ObservableCheck>,
}

/// One observable element/text check derived from a discovered state's
/// clustered elements → an `IrAssertion`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservableCheck {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// `element-presence` | `text-content`.
    #[serde(default)]
    pub category: String,
    /// `critical` | `warning`.
    #[serde(default)]
    pub severity: String,
    /// Free-form search criteria (role / text / text_contains). Passed through
    /// verbatim into the assertion target.
    #[serde(default)]
    pub criteria: serde_json::Value,
    #[serde(default)]
    pub label: String,
}

/// A discovered transition between states.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredTransition {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub from_state_ids: Vec<String>,
    #[serde(default)]
    pub to_state_ids: Vec<String>,
    /// The trigger element's search criteria (role / text_contains).
    #[serde(default)]
    pub trigger: serde_json::Value,
    /// Action verb (`click`, `type`, …).
    #[serde(default)]
    pub action_type: String,
}
