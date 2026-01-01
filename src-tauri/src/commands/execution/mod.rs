//! Execution control commands
//!
//! This module handles all Python executor lifecycle and workflow execution operations:
//! - Starting and stopping the Python executor
//! - Starting and stopping workflow execution
//! - Querying executor status
//! - Monitor detection
//! - System operations (updates, folder opening)
//!
//! # Module Organization
//!
//! - `python_executor` - Python bridge lifecycle (start, stop, capture settings)
//! - `executor_status` - Status queries and input validation
//! - `workflow_execution` - Workflow start/stop and initial states resolution
//! - `system_ops` - System-level operations (updates, folder opening, error handling)

// Make submodules public so tauri::generate_handler! can access macro-generated items
pub mod executor_status;
pub mod python_executor;
pub mod system_ops;
pub mod workflow_execution;
