//! Types for the orchestration loop — runner-side shim.
//!
//! The DTO shapes (`OrchestrationLoopConfig`, sub-configs, enums,
//! iteration/status/multi-loop shapes) live in
//! `qontinui-types::orchestration_config` and are re-exported here via
//! `pub use`. Runner-specific behavior that used to live in inherent `impl`
//! blocks on `OrchestrationLoopConfig` is exposed through the
//! [`OrchestrationLoopConfigExt`] extension trait because Rust's orphan rule
//! forbids inherent methods on foreign types. See `scheduler.rs` and
//! `unified_workflows.rs` for the same pattern applied to earlier extractions.
//!
//! Runtime-only types that never cross the wire (`LoopMetadata`,
//! `LoopMetadataMap`) remain runner-local — they have no
//! `Serialize`/`Deserialize` derives and are used purely for in-memory
//! bookkeeping by the multi-loop manager.

pub use qontinui_types::orchestration_config::*;

use std::collections::HashMap;

// ============================================================================
// Extension trait (orphan rule prevents inherent `impl` blocks on foreign
// types)
// ============================================================================

/// Runner-side behavior for [`OrchestrationLoopConfig`].
///
/// Exposed as a trait because `OrchestrationLoopConfig` is defined in
/// `qontinui-types` and we cannot add inherent methods to it here. Import
/// this trait (or `crate::orchestration_loop::types::*`) to call these
/// methods exactly as if they were inherent.
pub trait OrchestrationLoopConfigExt {
    /// Resolve the iteration cap for loop bounds: `None` means unlimited,
    /// which we represent internally as `u32::MAX` so loops still have a
    /// concrete bound (2^32 iterations is effectively unbounded for any
    /// real workflow — the loop will always exit on success, failure, or
    /// external stop well before approaching this value).
    fn iter_cap(&self) -> u32;

    /// Human-readable iteration cap for logs and UI copy (`"∞"` for
    /// unlimited).
    fn iter_cap_display(&self) -> String;
}

impl OrchestrationLoopConfigExt for OrchestrationLoopConfig {
    fn iter_cap(&self) -> u32 {
        self.max_iterations.unwrap_or(u32::MAX)
    }

    fn iter_cap_display(&self) -> String {
        self.max_iterations
            .map(|n| n.to_string())
            .unwrap_or_else(|| "∞".to_string())
    }
}

// ============================================================================
// Runner-local runtime types (not on the wire)
// ============================================================================

/// Metadata stored alongside each loop's state.
///
/// Pure runner-side bookkeeping — no `Serialize`/`Deserialize`, never crosses
/// the wire. Used by the multi-loop manager to attach per-loop labels and the
/// shared "abort all on first error" policy to the in-memory running state.
#[derive(Debug, Clone, Default)]
pub struct LoopMetadata {
    /// Human label for this loop (mirrors `MultiLoopEntry.label`).
    pub label: Option<String>,
    /// Abort-all-on-first-error policy (mirrors
    /// `MultiLoopConfig.stop_all_on_error`).
    pub stop_all_on_error: bool,
}

/// Maps loop IDs to per-loop metadata.
pub type LoopMetadataMap = HashMap<LoopId, LoopMetadata>;
