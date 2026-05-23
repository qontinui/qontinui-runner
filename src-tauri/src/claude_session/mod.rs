//! Interactive Claude CLI session management.
//!
//! This module provides bidirectional communication with Claude CLI using the
//! stream-json protocol. It replaces the one-shot subprocess model with an
//! interactive session that supports:
//!
//! - Multiple turns within a single CLI process
//! - User messages sent during execution
//! - Message queuing when the AI is processing
//! - Interrupt support for stopping mid-turn
//! - Autonomous-to-interactive context switching

pub mod dispatcher;
pub mod federation;
pub mod manager;
pub mod resume;
pub mod runner;
pub mod session;
pub mod state;
pub mod worker_session;
pub mod writer;

pub use manager::SessionManager;
pub use session::ClaudeSession;
pub use state::SessionState;
pub use worker_session::WorkerSession;
