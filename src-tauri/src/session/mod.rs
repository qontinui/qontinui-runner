//! Unified Session Manager
//!
//! Manages all AI sessions with a unified checkpoint format and shared execution
//! infrastructure.
//!
//! Key design decisions:
//! - All sessions use checkpoint-based continuation (no independent process spawning)
//! - Sessions run as child processes of the runner
//! - Multiple non-GUI sessions can run concurrently
//! - Only one GUI automation session can run at a time (gui_lock)
//! - Checkpoint persistence enables resume after runner restart
//!
//! # Module Structure
//!
//! - `types` - Core data structures (Session, SessionConfig, SessionStatus, etc.)
//! - `checkpoints` - Checkpoint persistence and management
//! - `locking` - GUI automation lock for exclusive access
//! - `lifecycle` - Session lifecycle management (SessionManager)

mod checkpoints;
mod lifecycle;
mod locking;
mod types;

// Re-export all public types for backward compatibility
// Some may not be used currently but are part of the public API
#[allow(unused_imports)]
pub use checkpoints::Checkpoint;
pub use lifecycle::SessionManager;
#[allow(unused_imports)]
pub use locking::GuiLock;
#[allow(unused_imports)]
pub use types::SessionEvent;
pub use types::{Session, SessionConfig, SessionStatus};
