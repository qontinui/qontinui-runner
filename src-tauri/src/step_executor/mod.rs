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
//! - `events` - Tree event emission helpers for action logging (NEW)
//! - `handlers` - Step handler trait and registry (NEW)
//!
//! ## Refactoring Status
//!
//! This module has been fully refactored:
//! - Phase 1-3 (DONE): Added events.rs and handlers/ for new patterns
//! - Phase 4-5 (DONE): Migrated initial step handlers to handlers/
//! - Phase 6 (DONE): Wired handlers into execute_single_step via HandlerRegistry
//! - Phase 7-9 (DONE): Migrated most handlers (19 total)
//! - Phase 10 (DONE): Migrated AWAS handlers (5 total)
//!
//! All 24 handlers are now active:
//!   - GUI: workflow, workflow_ref, state, action, gui_action, screenshot, macro (7)
//!   - Shell/Script: shell_command, shell, script, playwright (4)
//!   - Verification: log_watch, check, check_group, test (4)
//!   - API/MCP: api_request, mcp_call (2)
//!   - AWAS: awas_discover, awas_execute, awas_check_support, awas_list_actions, awas_extract_elements (5)
//!   - Other: prompt (1)
//!
//! ## Step Categories
//!
//! - **GUI Automation**: workflow, state, action
//! - **Web Automation**: playwright
//! - **AWAS Automation**: awas_discover, awas_execute, awas_check_support, etc.
//! - **Shell**: shell_command
//! - **API**: api_request
//! - **Checks**: check, check_group
//! - **Other**: mcp_call, macro, log_watch, prompt

// The original step_executor.rs implementation (moved here)
#[allow(clippy::module_inception)]
mod executor;

// New modular components (Phase 1-3 of refactoring)
pub mod events;
pub mod handlers;

// Re-export everything from the original executor module for backward compatibility
// This ensures existing code continues to work without changes
pub use executor::*;
