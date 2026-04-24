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

// Make submodules public so tauri::generate_handler! can access macro-generated items
pub mod bridge_execution;
pub mod executor_status;
pub mod python_executor;
pub mod system_ops;
pub mod workflow_execution;

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};

/// Build the Tauri plugin that registers this module's command handlers.
///
/// Non-generic because several sub-module handlers accept concrete
/// `tauri::AppHandle`.
pub fn plugin() -> TauriPlugin<tauri::Wry> {
    PluginBuilder::<tauri::Wry>::new("qontinui_execution")
        .invoke_handler(tauri::generate_handler![
            python_executor::start_python_executor,
            python_executor::stop_python_executor,
            python_executor::update_capture_settings,
            workflow_execution::start_execution,
            workflow_execution::stop_execution,
            workflow_execution::pause_execution,
            workflow_execution::resume_execution,
            workflow_execution::get_resolved_initial_states,
            workflow_execution::get_workflow_required_screens,
            bridge_execution::run_workflow_on_bridge,
            bridge_execution::transfer_gui_lock,
            bridge_execution::list_bridges,
            bridge_execution::get_bridge_info,
            executor_status::get_executor_status,
            executor_status::get_monitors,
            executor_status::set_input_capture_enabled,
            executor_status::get_input_validation_status,
            system_ops::handle_error,
            system_ops::check_for_updates,
            system_ops::install_update,
            system_ops::open_folder,
        ])
        .build()
}
