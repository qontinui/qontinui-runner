//! Task Orchestrator Module
//!
//! Implements the agentic architecture state machine for autonomous development automation.
//! See docs/AGENTIC_ARCHITECTURE.md for full specification.
//!
//! Key principles:
//! - Separation of concerns between planning, execution, and verification
//! - System-enforced gates that agents cannot bypass
//! - Information isolation for verification agents
//! - Deterministic verification before AI verification
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
//!
//! ### Integration Layer
//! - `config`: Unified configuration for all 15 modules
//! - `runtime`: Integrated orchestrator runtime combining all modules
//!
//! ## Usage
//!
//! ```rust
//! use crate::orchestrator::{OrchestratorRuntime, EnhancedOrchestratorConfig, ConfigPresets};
//!
//! // Create runtime with default config
//! let runtime = OrchestratorRuntime::new(EnhancedOrchestratorConfig::default());
//!
//! // Or use presets
//! let runtime = OrchestratorRuntime::new(ConfigPresets::production());
//!
//! // Initialize and run task
//! runtime.initialize_task("task-1", "Fix all type errors")?;
//! runtime.start_iteration()?;
//! // ... process work ...
//! runtime.complete_task(true, None)?;
//! ```

pub mod agent_roles;
pub mod agent_tool;
pub mod checkpoint;
pub mod compression;
pub mod config;
pub mod context_propagation;
pub mod context_strategies;
pub mod event_bus;
pub mod flow;
pub mod flow_executor;
pub mod hooks;
pub mod integration;
pub mod interrupt;
pub mod knowledge;
pub mod layers;
pub mod learning;
pub mod ledger;
pub mod memory;
pub mod output;
pub mod parallel;
pub mod planning;
pub mod realtime_events;
pub mod retry;
pub mod runtime;
pub mod state;
pub mod status_events;
pub mod structured_output;
pub mod subflows;
pub mod typed_channels;
pub mod types;
pub mod verification;

// Re-exports for public API - some may be unused internally but needed for external access
#[allow(unused_imports)]
pub use agent_roles::*;
#[allow(unused_imports)]
pub use agent_tool::*;
#[allow(unused_imports)]
pub use checkpoint::*;
pub use compression::*;
#[allow(unused_imports)]
pub use config::*;
#[allow(unused_imports)]
pub use context_propagation::*;
#[allow(unused_imports)]
pub use context_strategies::*;
#[allow(unused_imports)]
pub use event_bus::*;
#[allow(unused_imports)]
pub use flow::*;
#[allow(unused_imports)]
pub use flow_executor::*;
#[allow(unused_imports)]
pub use hooks::*;
pub use integration::*;
#[allow(unused_imports)]
pub use interrupt::*;
#[allow(unused_imports)]
pub use knowledge::*;
#[allow(unused_imports)]
pub use layers::*;
#[allow(unused_imports)]
pub use learning::*;
#[allow(unused_imports)]
pub use ledger::*;
#[allow(unused_imports)]
pub use memory::*;
#[allow(unused_imports)]
pub use output::*;
#[allow(unused_imports)]
pub use parallel::*;
#[allow(unused_imports)]
pub use planning::*;
#[allow(unused_imports)]
pub use realtime_events::*;
pub use retry::*;
#[allow(unused_imports)]
pub use runtime::*;
#[allow(unused_imports)]
pub use state::*;
#[allow(unused_imports)]
pub use status_events::*;
#[allow(unused_imports)]
pub use structured_output::*;
#[allow(unused_imports)]
pub use subflows::*;
#[allow(unused_imports)]
pub use typed_channels::*;
pub use types::*;
pub use verification::*;
