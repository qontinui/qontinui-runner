//! Task Orchestrator Module
//!
//! Building blocks for autonomous development automation: planning, knowledge,
//! retry, compression, context propagation and the modules listed below.
//!
//! The orchestrator's own task state machine (`TaskOrchestrator`) and its
//! deterministic + AI verifier (`VerificationOrchestrator`) were deleted: no
//! production path constructed either. Product workflow verification is the
//! unified workflow executor's (`crate::unified_workflow_executor`). The
//! `OrchestratorRuntime` / `EnhancedOrchestratorConfig` integration layer was
//! deleted for the same reason: nothing outside this module constructed it.
//!
//! ## New Modules (2026 Enhancements)
//!
//! ### Tier 1 (Implemented)
//! - `structured_output`: Type-safe JSON output format for AI workers
//! - `ledger`: Task ledger pattern for progress tracking and stall detection
//! - `context_strategies`: Context window management strategies (unbounded, buffered, token-limited, etc.)
//! - `layers`: Layered middleware architecture for observability, limits, and debugging
//! - `interrupt`: Human-in-the-loop interrupt/resume protocol
//!
//! ### Tier 2 (Implemented)
//! - `agent_roles`: Role/goal/backstory pattern for specialized agents
//! - `event_bus`: Decoupled event handling with subscribers
//! - `typed_channels`: Type-safe state channels with reducers
//! - `memory`: Multi-tier memory system (short-term, long-term, entity)
//! - `subflows`: Nested workflow composition
//!
//! ### Tier 3 (Implemented)
//! - `parallel`: Parallel execution with merge strategies
//! - `agent_tool`: Agent-as-tool pattern for LLM function calling
//! - `checkpoint`: Checkpointing with time-travel debugging
//! - `flow`: Flow orchestration mode for deterministic workflows
//! - `learning`: Learning loop with post-task analysis

pub mod agent_roles;
pub mod agent_tool;
pub mod brain_actor;
pub mod checkpoint;
pub mod compression;
pub mod context_propagation;
pub mod context_strategies;
pub mod contract_validator;
pub mod contracts;
pub mod event_bus;
pub mod flow;
pub mod flow_executor;
pub mod hooks;
pub mod interrupt;
pub mod knowledge;
pub mod layers;
pub mod learning;
pub mod learning_recorder;
pub mod ledger;
pub mod memory;
pub mod parallel;
pub mod planning;
pub mod realtime_events;
pub mod retry;
pub mod role_specializations;
pub mod state;
pub mod status_events;
pub mod structured_output;
pub mod subflows;
pub mod tool_guard;
pub mod typed_channels;
pub mod types;

// Re-exports for public API (only items actually used by other modules)
pub use compression::*;
pub use retry::*;
pub use types::*;
