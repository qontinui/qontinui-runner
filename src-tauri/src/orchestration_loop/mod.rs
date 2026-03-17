//! Orchestration Loop — runner-side workflow loop engine.
//!
//! Runs iterative workflow loops that can target any runner (self or another).
//! The loop executes workflows, evaluates exit conditions, and coordinates
//! runner restarts via the supervisor API.
//!
//! # Architecture
//!
//! ```text
//! Orchestrating Runner (this runner)
//!   │
//!   ├─ Start workflow on Target Runner (HTTP API)
//!   ├─ Poll for completion
//!   ├─ Trigger reflection on Target Runner
//!   ├─ Evaluate exit condition (0 fixes = done)
//!   ├─ Ask Supervisor to restart Target Runner (HTTP API)
//!   └─ Repeat
//! ```
//!
//! The orchestrating runner is never restarted — only the target runner is.
//! This cleanly separates the orchestrator from the execution target.

pub mod commands;
pub mod fix_agent;
pub mod loop_engine;
pub mod remote_client;
pub mod types;
