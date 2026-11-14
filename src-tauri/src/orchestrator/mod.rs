//! Task Orchestrator Module
//!
//! Building blocks for autonomous development automation: knowledge, retry,
//! compression, context propagation, checkpointing, learning, flow execution
//! and agent roles.
//!
//! The orchestrator's own task state machine (`TaskOrchestrator`) and its
//! deterministic + AI verifier (`VerificationOrchestrator`) were deleted: no
//! production path constructed either. Product workflow verification is the
//! unified workflow executor's (`crate::unified_workflow_executor`). The
//! `OrchestratorRuntime` / `EnhancedOrchestratorConfig` integration layer, and
//! the submodules only it would have driven (`agent_tool`,
//! `context_strategies`, `event_bus`, `interrupt`, `layers`, `ledger`,
//! `parallel`, `planning`, `subflows`, `typed_channels`), were deleted for the
//! same reason: nothing outside this module used them.

pub mod agent_roles;
pub mod brain_actor;
pub mod checkpoint;
pub mod compression;
pub mod context_propagation;
pub mod contract_validator;
pub mod contracts;
pub mod flow;
pub mod flow_executor;
pub mod hooks;
pub mod knowledge;
pub mod learning;
pub mod learning_recorder;
pub mod memory;
pub mod realtime_events;
pub mod retry;
pub mod role_specializations;
pub mod state;
pub mod status_events;
pub mod structured_output;
pub mod tool_guard;
pub mod types;

// Re-exports for public API (only items actually used by other modules)
pub use compression::*;
pub use retry::*;
pub use types::*;
