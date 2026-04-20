//! Saga compensation for Restate durable workflows.
//!
//! Implements a LIFO compensation stack that records undo actions at phase
//! boundaries and executes them in reverse order on workflow failure.
//!
//! The concrete types and executor functions live in
//! [`crate::unified_workflow_executor`] so they can be shared between the
//! Restate service and the legacy desktop loop. This module re-exports the
//! surface that `restate::service` consumes.

pub use crate::unified_workflow_executor::compensation::{
    execute_all_compensations, execute_compensation, execute_custom_command, execute_file_cleanup,
    execute_git_reset, execute_process_kill, execute_worktree_remove,
};
pub use crate::unified_workflow_executor::types::{
    CompensationAction, CompensationResult, CompensationType,
};
