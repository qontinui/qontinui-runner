//! Unified Workflow Executor
//!
//! This module provides a clean, modular implementation of the verification-agentic loop
//! for unified workflows. It follows the Single Responsibility Principle with clear
//! separation of concerns:
//!
//! - `types.rs`: Data structures for loop state and results
//! - `phases.rs`: Individual phase executors (setup, verification, agentic, completion)
//! - `loop_controller.rs`: The main loop coordination logic
//!
//! ## Key Design Principles
//!
//! 1. **Verification is the sole authority**: The AI cannot bypass verification by
//!    claiming [TASK_COMPLETE]. The loop only exits when verification passes.
//!
//! 2. **No orchestrator integration**: This module calls Claude directly without
//!    going through the session system or orchestrator. The unified workflow has
//!    a fixed architecture that doesn't need dynamic AI management.
//!
//! 3. **Clear completion criteria**: Completion phase ONLY runs if verification passed.
//!    There is no ambiguity about when a task is complete.
//!
//! 4. **Comprehensive logging**: Every decision is logged for debugging and audit.
//!
//! ## Usage
//!
//! ```ignore
//! let controller = LoopController::new(app_state, config_storage, app_handle);
//!
//! let config = LoopConfig {
//!     max_iterations: 5,
//!     timeout_seconds: 300,
//!     base_prompt: "Fix the issues".to_string(),
//!     workflow_name: "My Workflow".to_string(),
//!     workflow_id: "wf-123".to_string(),
//!     execution_id: "exec-456".to_string(),
//! };
//!
//! let result = controller.run(
//!     config,
//!     setup_steps,
//!     verification_steps,
//!     agentic_steps,
//!     completion_automation_steps,
//!     completion_prompt_steps,
//! ).await;
//!
//! if result.verification_passed {
//!     println!("Workflow completed successfully!");
//! } else {
//!     println!("Workflow failed: verification did not pass");
//! }
//! ```

mod loop_controller;
mod phases;
mod types;

pub use loop_controller::{LoopController, WorkflowResult};
pub use types::{AgenticOutcome, IterationResult, LoopConfig, LoopResult, WorkflowPhase};

// Re-export phase executors for testing or advanced usage
pub use phases::{AgenticExecutor, CompletionExecutor, SetupExecutor, VerificationExecutor};
