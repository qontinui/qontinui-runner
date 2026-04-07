//! Shared types for reflection storage.
//!
//! The actual storage operations live in `database/pg/reflection.rs`. This
//! module retains the public summary type used across the API surface.

/// Summary of a reflection run for history display.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReflectionRunSummary {
    pub task_run_id: String,
    pub source_task_run_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub fix_count: u32,
}
