//! Workflow generation feedback recording.
//!
//! Replaces info!-only logging with structured DB inserts when users
//! edit, delete, or rate AI-generated workflows. This data feeds the
//! self-improvement analyzer in `self_improve.rs`.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;
use crate::database::Connection;

/// Type of feedback on a generated workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FeedbackType {
    /// User edited a field of the generated workflow.
    Edit {
        field: String,
        old_value: Option<String>,
        new_value: Option<String>,
    },
    /// User deleted the generated workflow.
    Delete { reason: Option<String> },
    /// User rated the generated workflow.
    Rating {
        rating: i32,
        comment: Option<String>,
    },
    /// User forked the generated workflow.
    Fork,
}

/// Record feedback on a generated workflow.
///
/// Only records if the workflow has a `generated_by_task_run_id` (i.e., was AI-generated).
pub fn record_workflow_feedback(
    conn: &Connection,
    workflow_id: &str,
    task_run_id: Option<&str>,
    workflow_category: Option<&str>,
    workflow_description: Option<&str>,
    feedback: &FeedbackType,
) -> Result<String, String> {
    Err("SQLite removed".to_string())
}

/// Summary of feedback for a workflow or category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackSummary {
    pub total_edits: i64,
    pub total_deletes: i64,
    pub total_ratings: i64,
    pub total_forks: i64,
    pub avg_rating: Option<f64>,
    pub most_edited_fields: Vec<(String, i64)>,
}

/// Get aggregated feedback summary for a category or all workflows.
pub fn get_workflow_feedback_summary(
    conn: &Connection,
    category: Option<&str>,
) -> Result<FeedbackSummary, String> {
    Err("SQLite removed".to_string())
}
