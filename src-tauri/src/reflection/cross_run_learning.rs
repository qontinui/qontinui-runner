//! Cross-run learning shared types.
//!
//! The analysis implementation lives on `PgDb::post_run_analysis` in
//! `database/pg/reflection.rs`; this module only holds the shared
//! `ExtractedSkill` type consumed by event emitters.

/// A skill that was auto-extracted during cross-run analysis.
/// Returned to the caller so they can emit async events on the workflow event bus.
#[derive(Debug, Clone)]
pub struct ExtractedSkill {
    pub skill_id: String,
    pub skill_slug: String,
    pub source_fix_id: String,
    pub source_task_run_id: String,
    pub workflow_name: String,
}
