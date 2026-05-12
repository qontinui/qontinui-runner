pub mod auto_yield_policy;
pub mod bridge_helpers;
pub mod bridge_manager;
pub mod context;
pub mod event_handler;
pub mod events;
pub mod execution_helpers;
pub mod executor_traits;
pub mod extraction_executor;
pub mod file_logger;
pub mod file_registry;
pub mod flakiness;
pub mod gui_lock;
pub mod health;
pub mod lifecycle;
pub mod output;
pub mod process;
pub mod protocol;
pub mod python_bridge;
pub mod results;
pub mod state;
pub mod step_adapter;
pub mod traits;
pub mod unified_execution;
pub mod upcoming_file_registry;
pub mod url_lock;

pub use bridge_helpers::{
    get_or_create_default_bridge, get_or_create_default_bridge_compartment,
    is_default_bridge_running, is_default_bridge_running_compartment, require_running_bridge,
    require_running_bridge_compartment, stop_default_bridge, stop_default_bridge_compartment,
    with_default_bridge, with_default_bridge_compartment,
};
pub use bridge_manager::{BridgeInfo, BridgeManager, BridgeMode, CreateBridgeResult};
pub use execution_helpers::{prompt_builder, timeout_helper};
pub use extraction_executor::ExtractionExecutor;
pub use file_logger::FileLogger;
pub use gui_lock::GuiLockInfo;
pub use python_bridge::PythonBridge;
pub use results::{ExecutionOutcome, IntoOutcome};
pub use state::ExecutorState;

pub use file_registry::{
    FileLockInfo, FileLockInfoWithWaiters, FileLockManager, FileLockWaiter, FileRegistryManager,
};
#[allow(unused_imports)]
pub use url_lock::UrlLockInfo;
pub use url_lock::UrlLockManager;
// Re-export unified execution utilities (primary integration point for executor traits)
// Note: These are public API exports used by external consumers (e.g., MCP API)
#[allow(unused_imports)]
pub use unified_execution::{
    aggregate_step_results, build_outcome, execute_with_lifecycle, UnifiedStepRunner,
};
