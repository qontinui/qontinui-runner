//! Step Executor Module
//!
//! Provides unified execution of automation steps (workflows, actions, states,
//! screenshots, Playwright tests, AWAS). This is the core execution layer used by:
//! - Run page (single workflow execution)
//! - AI Builder (multi-step execution before AI session)
//! - MCP API (direct step execution)
//!
//! The design principle: multi-step execution is the foundation, and running
//! a single workflow is just a special case (one step of type "workflow").
//!
//! ## Module Structure
//!
//! - `executor` - Main executor implementation (StepExecutor, types, execute methods)
//! - `events` - Tree event emission helpers for action logging
//! - `handlers` - Step handler trait, registry, and all handler implementations
//!
//! ## Architecture
//!
//! Step execution uses a polymorphic handler dispatch system. All 24 step types
//! are implemented as individual handlers in the `handlers/` module:
//!
//! | Category | Handlers |
//! |----------|----------|
//! | **GUI** (7) | workflow, workflow_ref, state, action, gui_action, screenshot, macro |
//! | **Shell/Script** (4) | shell_command, shell, script, playwright |
//! | **Verification** (4) | log_watch, check, check_group, test |
//! | **API/MCP** (2) | api_request, mcp_call |
//! | **AWAS** (5) | awas_discover, awas_execute, awas_check_support, awas_list_actions, awas_extract_elements |
//! | **Other** (1) | prompt |
//!
//! See `handlers/mod.rs` for details on adding new step types.

// The original step_executor.rs implementation (moved here)
#[allow(clippy::module_inception)]
mod executor;

// New modular components (Phase 1-3 of refactoring)
pub mod dry_run;
pub mod events;
pub mod handlers;
pub mod step_cache;

// Re-export everything from the original executor module for backward compatibility
// This ensures existing code continues to work without changes
pub use executor::*;
