//! Execution control commands
//!
//! This module handles all Python executor lifecycle and workflow execution operations:
//! - Starting and stopping the Python executor
//! - Starting and stopping workflow execution
//! - Querying executor status
//! - Monitor detection
//! - System operations (updates, folder opening)
//! - Bridge-specific workflow execution and GUI lock management
//!
//! # Module Organization
//!
//! - `python_executor` - Python bridge lifecycle (start, stop, capture settings)
//! - `executor_status` - Status queries and input validation
//! - `workflow_execution` - Workflow start/stop and initial states resolution
//! - `system_ops` - System-level operations (updates, folder opening, error handling)
//! - `bridge_execution` - Bridge-specific workflow execution and GUI lock transfer

// Submodules are public so `ipc_registry::GROUPS` can name each one's
// `IPC_GROUP` (`crate::commands::execution::<sub>::IPC_GROUP`).
pub mod bridge_execution;
pub mod executor_status;
pub mod python_executor;
pub mod system_ops;
pub mod workflow_execution;
