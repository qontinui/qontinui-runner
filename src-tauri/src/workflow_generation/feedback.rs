//! Workflow generation feedback recording.
//!
//! Replaces info!-only logging with structured DB inserts when users
//! edit, delete, or rate AI-generated workflows. This data feeds the
//! self-improvement analyzer in `self_improve.rs`.

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

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
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let (feedback_type, edited_field, old_value, new_value, delete_reason, rating, rating_comment) =
        match feedback {
            FeedbackType::Edit {
                field,
                old_value,
                new_value,
            } => (
                "edit",
                Some(field.as_str()),
                old_value.as_deref(),
                new_value.as_deref(),
                None,
                None,
                None,
            ),
            FeedbackType::Delete { reason } => {
                ("delete", None, None, None, reason.as_deref(), None, None)
            }
            FeedbackType::Rating { rating, comment } => (
                "rating",
                None,
                None,
                None,
                None,
                Some(*rating),
                comment.as_deref(),
            ),
            FeedbackType::Fork => ("fork", None, None, None, None, None, None),
        };

    conn.execute(
        r#"INSERT INTO workflow_generation_feedback
            (id, workflow_id, task_run_id, feedback_type, edited_field, old_value, new_value,
             delete_reason, rating, rating_comment, workflow_category, workflow_description, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
        params![
            id,
            workflow_id,
            task_run_id,
            feedback_type,
            edited_field,
            old_value,
            new_value,
            delete_reason,
            rating,
            rating_comment,
            workflow_category,
            workflow_description,
            now,
        ],
    )
    .map_err(|e| format!("Failed to record workflow feedback: {}", e))?;

    info!(
        "Recorded {} feedback for workflow {} (id={})",
        feedback_type, workflow_id, id,
    );

    Ok(id)
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
    let (where_clause, param) = match category {
        Some(cat) => ("WHERE workflow_category = ?1", Some(cat.to_string())),
        None => ("", None),
    };

    // Count by feedback type
    let sql = format!(
        "SELECT feedback_type, COUNT(*) FROM workflow_generation_feedback {} GROUP BY feedback_type",
        where_clause
    );

    let conn_ref = conn;
    let mut stmt = conn_ref
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare feedback summary query: {}", e))?;

    let mapper = |row: &rusqlite::Row| -> rusqlite::Result<(String, i64)> {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    };

    let rows = if let Some(ref p) = param {
        stmt.query_map(params![p], mapper)
    } else {
        stmt.query_map([], mapper)
    }
    .map_err(|e| format!("Failed to query feedback summary: {}", e))?;

    let mut total_edits = 0i64;
    let mut total_deletes = 0i64;
    let mut total_ratings = 0i64;
    let mut total_forks = 0i64;

    for (ft, count) in rows.flatten() {
        match ft.as_str() {
            "edit" => total_edits = count,
            "delete" => total_deletes = count,
            "rating" => total_ratings = count,
            "fork" => total_forks = count,
            _ => {}
        }
    }

    // Average rating
    let avg_sql = format!(
        "SELECT AVG(rating) FROM workflow_generation_feedback WHERE feedback_type = 'rating' AND rating IS NOT NULL {}",
        if category.is_some() { "AND workflow_category = ?1" } else { "" }
    );

    let avg_rating: Option<f64> = if let Some(ref p) = param {
        conn.query_row(&avg_sql, params![p], |row| row.get(0)).ok()
    } else {
        conn.query_row(&avg_sql, [], |row| row.get(0)).ok()
    }
    .flatten();

    // Most edited fields
    let field_sql = format!(
        "SELECT edited_field, COUNT(*) as cnt FROM workflow_generation_feedback \
         WHERE feedback_type = 'edit' AND edited_field IS NOT NULL {} \
         GROUP BY edited_field ORDER BY cnt DESC LIMIT 5",
        if category.is_some() {
            "AND workflow_category = ?1"
        } else {
            ""
        }
    );

    let mut field_stmt = conn
        .prepare(&field_sql)
        .map_err(|e| format!("Failed to prepare field summary: {}", e))?;

    let field_mapper = |row: &rusqlite::Row| -> rusqlite::Result<(String, i64)> {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    };

    let field_rows = if let Some(ref p) = param {
        field_stmt.query_map(params![p], field_mapper)
    } else {
        field_stmt.query_map([], field_mapper)
    }
    .map_err(|e| format!("Failed to query field summary: {}", e))?;

    let most_edited_fields: Vec<(String, i64)> = field_rows.filter_map(|r| r.ok()).collect();

    Ok(FeedbackSummary {
        total_edits,
        total_deletes,
        total_ratings,
        total_forks,
        avg_rating,
        most_edited_fields,
    })
}
