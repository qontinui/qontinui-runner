//! Rust mirrors of the TypeScript IR types from
//! `qontinui-schemas/ts/src/ui-bridge-ir/`. We don't auto-generate these
//! because the IR module sits intentionally on the TS side (Rust
//! `qontinui-schemas/rust/` houses the runtime/db wire types — see ADR-001
//! decision #6 for the boundary). The Spec API needs to (de)serialize IR
//! documents over HTTP, so we keep a small, hand-mirrored Rust copy here.
//!
//! The mirror is structural only — no behavior. The projection lives in
//! `spec_api::projection`. Field-renaming follows the TS `camelCase`
//! convention via `serde(rename_all = "camelCase")`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Element criteria
// ---------------------------------------------------------------------------

/// Mirror of `IRElementCriteria`
/// (`qontinui-schemas/ts/src/ui-bridge-ir/element-criteria.ts`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IrElementCriteria {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aria_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessible_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// HTML attributes to check (exact string match). Stored as a `BTreeMap`
    /// so JSON serialization is deterministic (matters for byte-stable
    /// projection output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<BTreeMap<String, String>>,
}

// ---------------------------------------------------------------------------
// Primitives — IR-only fields (provenance, metadata, effect, cross-refs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IrProvenance {
    /// "hand-authored" | "build-plugin" | "ai-generated" | "migrated"
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IrMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_elements: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IrCrossRef {
    pub doc: String,
    /// `ref` is reserved in Rust — serialize as `ref` for the wire while
    /// using `r#ref` as the Rust field name.
    #[serde(rename = "ref")]
    pub r#ref: String,
}

// ---------------------------------------------------------------------------
// Wait spec / transition action
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IrWaitSpec {
    /// "idle" | "element" | "state" | "time" | "condition" | "vanish" | "change" | "stable"
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<IrElementCriteria>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_period_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IrTransitionAction {
    #[serde(rename = "type")]
    pub kind: String,
    pub target: IrElementCriteria,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_after: Option<IrWaitSpec>,
}

// ---------------------------------------------------------------------------
// State / transition / document
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IrStateCondition {
    pub element: IrElementCriteria,
    /// "visible" | "enabled" | "checked" | "expanded" | "selected" | "text" | "value"
    pub property: String,
    pub expected: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparator: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IrState {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    pub required_elements: Vec<IrElementCriteria>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_elements: Option<Vec<IrElementCriteria>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<IrStateCondition>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_initial: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_terminal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incoming_transitions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IrMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<IrProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_refs: Option<Vec<IrCrossRef>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IrTransition {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    pub from_states: Vec<String>,
    pub activate_states: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_states: Option<Vec<String>>,

    pub actions: Vec<IrTransitionAction>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bidirectional: Option<bool>,

    /// "read" | "write" | "destructive"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IrMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<IrProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_refs: Option<Vec<IrCrossRef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IrDocument {
    /// Schema version. Currently always `"1.0"`.
    pub version: String,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IrMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<IrProvenance>,

    pub states: Vec<IrState>,
    pub transitions: Vec<IrTransition>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_state: Option<String>,
}

// ---------------------------------------------------------------------------
// Legacy spec shapes — output of the projection (mirrors `LegacySpec` in
// `qontinui-schemas/ts/src/ui-bridge-ir/projection.ts`).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LegacyAssertionTarget {
    /// Always `"search"` for the projection — point/region targets aren't
    /// expressible in the IR.
    #[serde(rename = "type")]
    pub kind: String,
    /// Free-form criteria object — kept as `Value` so we don't lose any
    /// inverse-projection round-trip information.
    pub criteria: serde_json::Value,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LegacyAssertion {
    pub id: String,
    pub description: String,
    pub category: String,
    pub severity: String,
    pub assertion_type: String,
    pub target: LegacyAssertionTarget,
    pub source: String,
    pub reviewed: bool,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LegacyGroup {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub assertions: Vec<LegacyAssertion>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LegacyProcessStep {
    pub action: String,
    pub target: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_after: Option<IrWaitSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LegacyTransition {
    pub id: String,
    pub name: String,
    pub activate_states: Vec<String>,
    pub deactivate_states: Vec<String>,
    pub stays_visible: bool,
    pub process: Vec<LegacyProcessStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LegacyStateMachineState {
    pub id: String,
    pub name: String,
    pub description: String,
    pub elements: Vec<serde_json::Value>,
    pub is_initial: bool,
    pub transitions: Vec<LegacyTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LegacyStateMachine {
    pub states: Vec<LegacyStateMachineState>,
}

/// Top-level legacy spec output. Serialized via a `serde_json::Value` step
/// so the projection can apply lexicographic key sorting at the end (matches
/// the TS projection's `sortKeys` pass).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LegacySpec {
    pub version: String,
    pub description: String,
    pub groups: Vec<LegacyGroup>,
    pub state_machine: LegacyStateMachine,
    pub metadata: serde_json::Value,
}
