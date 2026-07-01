//! Spec-Check (B) v1 — pure-Rust matcher for `IrPageSpec × UIBridgeSnapshot`.
//!
//! Design context: `qontinui-dev-notes/spec-check-v1/00-design-context.md`.
//! Plan:           `qontinui-dev-notes/spec-check-v1/02-crate-core.md`.
//!
//! No Tauri, no HTTP, no MCP, no async runtime — sync API, rayon-internal.

pub mod confidence;
pub mod criteria;
pub mod derive;
pub mod diff;
pub mod evaluator;
pub mod fetch;
pub mod policy;
pub mod result_migration;
pub mod role_categories;
pub mod snapshot;

#[cfg(test)]
pub mod proptest_strategies;

pub use evaluator::{
    evaluate, evaluate_batch, evaluate_with_identity, evaluate_with_identity_and_validation,
};
pub use fetch::{wrap_snapshot, SnapshotFetchError};
pub use policy::apply_policy;
pub use result_migration::deserialize_result_with_migration;
pub use snapshot::IndexedSnapshot;

// Re-export wire types so downstream crates don't need two deps. Names match
// the qontinui-schemas registrations at qontinui-runner/src-tauri/src/
// schema_export.rs:642-673.
pub use qontinui_types::ir::{
    IrAssertion, IrAssertionTarget, IrElementCriteria, IrPageSpec, IrState,
};
pub use qontinui_types::spec_check::{
    AssertionMiss, AssertionOutcome, AssertionResult, AssertionScope, AssertionSeverityCounts,
    BridgeFingerprint, CandidateMiss, ClassificationStatus, Confidence, ConjunctEvaluation,
    ConjunctRule, FieldDiff, MatchOutcome, MatchedElement, MissReason, PolicyConjunct,
    PolicyEvaluation, PolicyStatus, RecommendedState, SpecCheckPolicy, SpecCheckResult,
    SpecCheckStepConfig, SpecCheckSummary, SpecValidation, StateMatchResult, ThresholdConfig,
};
pub use qontinui_types::ui_bridge::{UIBridgeElement, UIBridgeSnapshot};
