//! Pipeline artifact capture for generator evaluation.
//!
//! Captures intermediate data from each stage of the workflow generation
//! pipeline (discovery, builder, autofix, verification/fixer loop, hardener)
//! for later analysis and debugging.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Complete artifact record for one generation pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineArtifact {
    pub id: String,
    pub workflow_id: Option<String>,
    pub task_run_id: Option<String>,
    pub description: String,
    pub category: Option<String>,
    pub created_at: String,

    // Timing (milliseconds)
    pub discovery_duration_ms: Option<u64>,
    pub builder_duration_ms: Option<u64>,
    pub autofix_duration_ms: Option<u64>,
    pub verification_duration_ms: Option<u64>,
    pub hardener_duration_ms: Option<u64>,
    pub total_duration_ms: Option<u64>,

    // Intermediate snapshots
    pub discovery_calls: Option<serde_json::Value>,
    pub builder_raw_output: Option<String>,
    pub builder_parsed_json: Option<serde_json::Value>,
    pub autofix_diff: Option<serde_json::Value>,
    pub verification_iterations: Option<serde_json::Value>,
    pub fixer_snapshots: Option<Vec<serde_json::Value>>,
    pub hardening_summary: Option<serde_json::Value>,
    pub hardened_json: Option<serde_json::Value>,
    pub final_json: Option<serde_json::Value>,
    pub validation_errors: Option<serde_json::Value>,

    // Outcome
    pub success: bool,
    pub error_message: Option<String>,
    pub model_used: Option<String>,
}

/// Summary view of an artifact (for list endpoints).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineArtifactSummary {
    pub id: String,
    pub workflow_id: Option<String>,
    pub description: String,
    pub category: Option<String>,
    pub created_at: String,
    pub total_duration_ms: Option<u64>,
    pub success: bool,
    pub model_used: Option<String>,
    pub verification_iteration_count: u32,
    pub hardener_converted_count: u32,
}

/// Builder for collecting pipeline data during generation.
///
/// Used inside `generate_workflow()` to accumulate timing and snapshot data,
/// then finalized into a `PipelineArtifact`.
#[derive(Debug, Default)]
pub struct PipelineArtifactBuilder {
    pub description: String,
    pub category: Option<String>,

    pub discovery_duration_ms: Option<u64>,
    pub builder_duration_ms: Option<u64>,
    pub autofix_duration_ms: Option<u64>,
    pub verification_duration_ms: Option<u64>,
    pub hardener_duration_ms: Option<u64>,

    pub discovery_calls: Option<serde_json::Value>,
    pub builder_raw_output: Option<String>,
    pub builder_parsed_json: Option<serde_json::Value>,
    pub autofix_diff: Option<serde_json::Value>,
    pub verification_iterations: Option<serde_json::Value>,
    pub fixer_snapshots: Vec<serde_json::Value>,
    pub hardening_summary: Option<serde_json::Value>,
    pub hardened_json: Option<serde_json::Value>,
    pub final_json: Option<serde_json::Value>,
    pub validation_errors: Option<serde_json::Value>,

    pub success: bool,
    pub error_message: Option<String>,
    pub model_used: Option<String>,
}

impl PipelineArtifactBuilder {
    pub fn new(description: &str, category: Option<&str>) -> Self {
        Self {
            description: description.to_string(),
            category: category.map(|s| s.to_string()),
            success: true,
            ..Default::default()
        }
    }

    /// Finalize into a PipelineArtifact with computed total duration.
    pub fn build(self, total_duration_ms: u64) -> PipelineArtifact {
        PipelineArtifact {
            id: Uuid::new_v4().to_string(),
            workflow_id: None,
            task_run_id: None,
            description: self.description,
            category: self.category,
            created_at: Utc::now().to_rfc3339(),

            discovery_duration_ms: self.discovery_duration_ms,
            builder_duration_ms: self.builder_duration_ms,
            autofix_duration_ms: self.autofix_duration_ms,
            verification_duration_ms: self.verification_duration_ms,
            hardener_duration_ms: self.hardener_duration_ms,
            total_duration_ms: Some(total_duration_ms),

            discovery_calls: self.discovery_calls,
            builder_raw_output: self.builder_raw_output,
            builder_parsed_json: self.builder_parsed_json,
            autofix_diff: self.autofix_diff,
            verification_iterations: self.verification_iterations,
            fixer_snapshots: if self.fixer_snapshots.is_empty() {
                None
            } else {
                Some(self.fixer_snapshots)
            },
            hardening_summary: self.hardening_summary,
            hardened_json: self.hardened_json,
            final_json: self.final_json,
            validation_errors: self.validation_errors,

            success: self.success,
            error_message: self.error_message,
            model_used: self.model_used,
        }
    }
}

/// Compute a JSON diff between two workflow JSON values.
///
/// Returns a JSON object with changed fields: `{field: {before: ..., after: ...}}`.
/// Only includes top-level fields that differ.
pub fn compute_json_diff(
    before: &serde_json::Value,
    after: &serde_json::Value,
) -> serde_json::Value {
    let mut diff = serde_json::Map::new();

    if let (Some(before_obj), Some(after_obj)) = (before.as_object(), after.as_object()) {
        // Check fields in before
        for (key, before_val) in before_obj {
            let after_val = after_obj.get(key).unwrap_or(&serde_json::Value::Null);
            if before_val != after_val {
                diff.insert(
                    key.clone(),
                    serde_json::json!({
                        "before": before_val,
                        "after": after_val,
                    }),
                );
            }
        }
        // Check fields only in after
        for (key, after_val) in after_obj {
            if !before_obj.contains_key(key) {
                diff.insert(
                    key.clone(),
                    serde_json::json!({
                        "before": null,
                        "after": after_val,
                    }),
                );
            }
        }
    }

    serde_json::Value::Object(diff)
}
