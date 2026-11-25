//! Core types for the task orchestrator.
//!
//! The wire-format DTOs (`VerificationPlan`, `SuccessCriterion`,
//! `CriterionType`, `VerificationMethod`, `WorkerSignal`, `Finding`,
//! `CriterionOverride`, `OverrideCollection`, `StageTransition`, and
//! `WorkerDomain`) live in `qontinui_types::verification`; this
//! module re-exports the ones the runner uses, so the rest of the runner can
//! `use crate::orchestrator::types::*`. The list is explicit: the verification
//! result DTOs (`VerificationResult`, `DomainVerificationResult`,
//! `IterationVerificationResults`, `TaskCompletionResult`) were filled only by
//! the deleted orchestrator verifier and are not re-exported, and the
//! multi-worker `DomainAssignment` / `WorkerInstance` / `WorkerStatus` had no
//! runner caller.
//!
//! Runner-side behavior (constructors that stamp timestamps / generate UUIDs,
//! parsers that extract signals from AI output text, override collection)
//! stays here. Because of Rust's orphan rule we can't define inherent
//! `impl` blocks on types defined in another crate, so the methods are
//! exposed through extension traits (`WorkerSignalExt`, `CriterionOverrideExt`,
//! `OverrideCollectionExt`). The traits are re-exported along with the DTOs so
//! callers can keep using `CriterionOverride::new(...)`, etc. verbatim as long
//! as this module's contents are brought into scope.

#![allow(dead_code)]

pub use qontinui_types::verification::{
    Confidence, CriterionOverride, CriterionType, Finding, OverrideCollection, StageTransition,
    SuccessCriterion, VerificationMethod, VerificationPlan, WorkerDomain, WorkerSignal,
};

// ============================================================================
// WorkerSignal behavior (extension trait)
// ============================================================================

/// Runner-side behavior for [`WorkerSignal`].
pub trait WorkerSignalExt: Sized {
    /// Parse a worker signal from AI output text.
    fn parse_from_output(output: &str) -> Option<Self>;
}

impl WorkerSignalExt for WorkerSignal {
    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    fn parse_from_output(output: &str) -> Option<Self> {
        // Check for [WORK_COMPLETE] marker
        if output.contains("[WORK_COMPLETE]") {
            // Extract reason if present
            let reason = if let Some(start) = output.find("[WORK_COMPLETE]") {
                let after = &output[start + 15..];
                let reason_text = after.lines().next().unwrap_or("").trim();
                if reason_text.is_empty() {
                    None
                } else {
                    Some(reason_text.to_string())
                }
            } else {
                None
            };
            return Some(WorkerSignal::WorkComplete { reason });
        }

        // Check for [NEED_REPLAN] marker
        if output.contains("[NEED_REPLAN]") {
            // Extract reason
            let reason = if let Some(start) = output.find("[NEED_REPLAN]") {
                let after = &output[start + 13..];
                // Take everything until next marker or end
                let end = after.find('[').unwrap_or(after.len());
                after[..end].trim().to_string()
            } else {
                "Plan revision requested".to_string()
            };
            return Some(WorkerSignal::NeedReplan { reason });
        }

        // Check for [FINDING:...] marker
        if let Some(start) = output.find("[FINDING:") {
            if let Some(end) = output[start..].find(']') {
                let finding_type = &output[start + 9..start + end];
                // Extract description (next line or content after marker)
                let after = &output[start + end + 1..];
                let description = after.lines().next().unwrap_or("").trim().to_string();

                return Some(WorkerSignal::Finding(Finding {
                    id: uuid::Uuid::new_v4().to_string(),
                    finding_type: finding_type.to_string(),
                    description,
                    evidence: None,
                    confidence: Confidence::Medium,
                    related_files: vec![],
                }));
            }
        }

        // Default: continue working
        None // Return None to indicate no explicit signal (implies Continue)
    }
}

// ============================================================================
// CriterionOverride behavior (extension trait)
// ============================================================================

/// Runner-side behavior for [`CriterionOverride`].
pub trait CriterionOverrideExt: Sized {
    /// Create a new criterion override.
    fn new(criterion_id: &str, item: &str, justification: &str, iteration: u32) -> Self;
    /// Set the worker ID for this override.
    fn with_worker_id(self, worker_id: &str) -> Self;
    /// Parse criterion overrides from worker output text.
    ///
    /// Looks for markers in the format:
    /// ```text
    /// [CRITERION_OVERRIDE:criterion_id]
    /// Item: ClassName or file/path
    /// Justification: Why this is acceptable
    /// [/CRITERION_OVERRIDE]
    /// ```
    fn parse_from_output(output: &str, iteration: u32) -> Vec<Self>;
}

impl CriterionOverrideExt for CriterionOverride {
    fn new(criterion_id: &str, item: &str, justification: &str, iteration: u32) -> Self {
        Self {
            criterion_id: criterion_id.to_string(),
            item: item.to_string(),
            justification: justification.to_string(),
            iteration,
            worker_id: None,
            recorded_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    fn with_worker_id(mut self, worker_id: &str) -> Self {
        self.worker_id = Some(worker_id.to_string());
        self
    }

    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    fn parse_from_output(output: &str, iteration: u32) -> Vec<Self> {
        let mut overrides = Vec::new();
        let mut remaining = output;

        while let Some(start) = remaining.find("[CRITERION_OVERRIDE:") {
            // Find the criterion ID
            let after_start = &remaining[start + 20..];
            if let Some(id_end) = after_start.find(']') {
                let criterion_id = after_start[..id_end].trim();

                // Find the closing marker
                let content_start = start + 20 + id_end + 1;
                if let Some(end) = remaining[content_start..].find("[/CRITERION_OVERRIDE]") {
                    let content = &remaining[content_start..content_start + end];

                    // Parse Item and Justification from content
                    let item = extract_field(content, "Item")
                        .or_else(|| extract_field(content, "Class"))
                        .or_else(|| extract_field(content, "File"))
                        .unwrap_or_else(|| "unspecified".to_string());

                    let justification = extract_field(content, "Justification")
                        .or_else(|| extract_field(content, "Reason"))
                        .unwrap_or_else(|| content.trim().to_string());

                    overrides.push(<CriterionOverride as CriterionOverrideExt>::new(
                        criterion_id,
                        &item,
                        &justification,
                        iteration,
                    ));

                    remaining = &remaining[content_start + end + 21..];
                } else {
                    // No closing marker, skip this one
                    remaining = &remaining[start + 20..];
                }
            } else {
                // Malformed, skip
                remaining = &remaining[start + 20..];
            }
        }

        overrides
    }
}

/// Extract a field value from content (e.g., "Item: value").
///
/// Previously a private method on `CriterionOverride`; free function here
/// because the trait doesn't need to expose it.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn extract_field(content: &str, field_name: &str) -> Option<String> {
    let pattern = format!("{}:", field_name);
    if let Some(start) = content.find(&pattern) {
        let after = &content[start + pattern.len()..];
        // Take until newline or end
        let value = after.lines().next().unwrap_or("").trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

// ============================================================================
// OverrideCollection behavior (extension trait)
// ============================================================================

/// Runner-side behavior for [`OverrideCollection`].
pub trait OverrideCollectionExt: Sized {
    /// Create a new empty override collection.
    fn new() -> Self;
    /// Add an override to the collection.
    fn add(&mut self, override_: CriterionOverride);
    /// Add multiple overrides to the collection.
    fn add_all(&mut self, overrides: Vec<CriterionOverride>);
    /// Check if a criterion has any overrides.
    fn has_override(&self, criterion_id: &str) -> bool;
    /// Get all overrides for a specific criterion.
    fn get_overrides(&self, criterion_id: &str) -> Vec<&CriterionOverride>;
    /// Get the justification summary for a criterion (concatenated if multiple).
    fn get_justification_summary(&self, criterion_id: &str) -> Option<String>;
    /// Get all unique criterion IDs that have overrides.
    fn overridden_criteria(&self) -> Vec<String>;
}

impl OverrideCollectionExt for OverrideCollection {
    fn new() -> Self {
        Self::default()
    }

    fn add(&mut self, override_: CriterionOverride) {
        self.overrides.push(override_);
    }

    fn add_all(&mut self, overrides: Vec<CriterionOverride>) {
        self.overrides.extend(overrides);
    }

    fn has_override(&self, criterion_id: &str) -> bool {
        self.overrides
            .iter()
            .any(|o| o.criterion_id == criterion_id)
    }

    fn get_overrides(&self, criterion_id: &str) -> Vec<&CriterionOverride> {
        self.overrides
            .iter()
            .filter(|o| o.criterion_id == criterion_id)
            .collect()
    }

    fn get_justification_summary(&self, criterion_id: &str) -> Option<String> {
        let overrides = self.get_overrides(criterion_id);
        if overrides.is_empty() {
            return None;
        }

        let summaries: Vec<String> = overrides
            .iter()
            .map(|o| format!("- {}: {}", o.item, o.justification))
            .collect();

        Some(summaries.join("\n"))
    }

    fn overridden_criteria(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .overrides
            .iter()
            .map(|o| o.criterion_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_signal_parsing() {
        // Test WORK_COMPLETE
        let output = "I've fixed the bug. [WORK_COMPLETE] The validation now works.";
        let signal = WorkerSignal::parse_from_output(output);
        assert!(matches!(signal, Some(WorkerSignal::WorkComplete { .. })));

        // Test NEED_REPLAN
        let output = "[NEED_REPLAN] The issue is in a different service.";
        let signal = WorkerSignal::parse_from_output(output);
        assert!(matches!(signal, Some(WorkerSignal::NeedReplan { .. })));

        // Test FINDING
        let output = "[FINDING:bug] The login function doesn't validate email format";
        let signal = WorkerSignal::parse_from_output(output);
        assert!(matches!(signal, Some(WorkerSignal::Finding(_))));

        // Test no signal
        let output = "I'm still working on the problem...";
        let signal = WorkerSignal::parse_from_output(output);
        assert!(signal.is_none());
    }
}
