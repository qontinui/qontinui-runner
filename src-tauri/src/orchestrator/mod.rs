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
pub mod learning_recorder;
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

// Re-exports for public API (only items actually used by other modules)
pub use compression::*;
pub use retry::*;
pub use types::*;
pub use verification::*;
