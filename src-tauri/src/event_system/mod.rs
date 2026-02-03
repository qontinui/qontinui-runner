//! Centralized event emission system for qontinui-runner.
//!
//! This module provides a unified interface for emitting events to the Tauri frontend
//! and WebSocket clients. It replaces the scattered event emission patterns found
//! throughout the codebase with a consistent, type-safe approach.
//!
//! # Architecture
//!
//! The event system consists of:
//!
//! - **`AppEvent`** - A unified enum representing all event types that can be emitted
//! - **`EventEmitter`** - A facade for emitting events with consistent error handling
//!
//! # Usage
//!
//! ```ignore
//! use crate::event_system::{EventEmitter, AppEvent};
//!
//! // Create an emitter from an AppHandle
//! let emitter = EventEmitter::new(app_handle);
//!
//! // Use convenience methods for common events
//! emitter.executor_event("workflow_started", json!({"name": "Test"}));
//! emitter.rag_progress("project-1", "processing", "Loading...", Some(50));
//!
//! // Or create events directly
//! emitter.emit_or_warn(AppEvent::executor_error("Something went wrong"));
//! ```
//!
//! # Migration Guide
//!
//! To migrate existing code to use the event system:
//!
//! 1. Replace `app_handle.emit("event-name", &payload)` with appropriate `EventEmitter` method
//! 2. Use typed `AppEvent` variants instead of raw JSON payloads
//! 3. Choose the appropriate error handling method:
//!    - `emit()` - Returns Result, caller handles errors
//!    - `emit_or_warn()` - Logs warning on failure (recommended for most cases)
//!    - `emit_or_error()` - Logs error on failure (for critical events)
//!    - `emit_silent()` - Ignores errors (use sparingly)
//!
//! # TODO: Files to Migrate
//!
//! The following files currently emit events directly and should be migrated to use EventEmitter:
//!
//! ## High Priority (Core Event Emission)
//! - `executor/events.rs` - EventForwarder (executor-event, executor-error, executor-response)
//! - `executor/output.rs` - Output handling (executor-error, extraction-error)
//! - `executor/python_bridge.rs` - Python bridge events (executor-event)
//!
//! ## Medium Priority (Feature-Specific Events)
//! - `commands/rag.rs` - RAG events (rag-progress, rag-completion)
//! - `commands/flow.rs` - Flow events (flow-event)
//! - `executor/extraction_executor.rs` - Extraction events (extraction-event)
//!
//! ## Lower Priority (Scattered Events)
//! - `mcp_api.rs` - Various events (executor-event, finding_detected, finding_resolved, test-navigation, ui-bridge-request, ai-output)
//! - `step_executor.rs` - Tree events (executor-event)
//! - `commands/ui_bridge.rs` - UI bridge events
//! - `commands/execution/python_executor.rs` - Python executor events
//! - `commands/execution/system_ops.rs` - Error events
//! - `orchestrator/status_events.rs` - Orchestrator status events
//! - `orchestrator/realtime_events.rs` - Learning/checkpoint events
//!
//! # Event Channel Reference
//!
//! | Event Name | Purpose | Used By |
//! |------------|---------|---------|
//! | `executor-event` | Workflow execution updates | EventForwarder, step_executor |
//! | `executor-error` | Execution errors | EventForwarder, output handlers |
//! | `executor-response` | Command responses | EventForwarder |
//! | `extraction-event` | Web extraction updates | ExtractionExecutor |
//! | `extraction-error` | Extraction errors | Output handlers |
//! | `extraction-response` | Extraction responses | EventForwarder |
//! | `rag-progress` | RAG processing progress | commands/rag |
//! | `rag-completion` | RAG processing complete | commands/rag |
//! | `flow-event` | Flow execution events | commands/flow, FlowExecutor |
//! | `ai-output` | AI session output | mcp_api |
//! | `finding_detected` | Finding detected | mcp_api |
//! | `finding_resolved` | Finding resolved | mcp_api |
//! | `test-navigation` | Test navigation | mcp_api |
//! | `ui-bridge-request` | UI automation | mcp_api |
//! | `error` | Generic errors | Various |

mod broadcaster;
mod emitter;
mod types;

// Public API exports for external consumers (e.g., tests, downstream crates)
#[allow(unused_imports)]
pub use broadcaster::{shared_broadcaster, EventBroadcaster, SharedEventBroadcaster};
pub use emitter::EventEmitter;
#[allow(unused_imports)]
pub use types::AppEvent;
