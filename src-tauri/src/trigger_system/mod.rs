//! Event-driven workflow trigger system.
//!
//! Provides proactive automation: workflows fire automatically in response to
//! external events (webhooks, file changes, git events, workflow completions,
//! health check failures).
//!
//! ## Architecture
//!
//! ```text
//! External Event (webhook, file change, git push, workflow completion, health check)
//!     |
//!     v
//! TriggerService receives event via tokio::mpsc channel
//!     |
//!     v
//! Debounce (coalesce rapid events within configurable window)
//!     |
//!     v
//! Throttle check (cooldown_seconds, max_concurrent)
//!     |
//!     v
//! Condition evaluation (require_idle, branch_filter, custom conditions)
//!     |
//!     v
//! Execute: load workflow -> create task_run -> build LoopConfig -> spawn
//!     |
//!     v
//! Record in trigger_history
//! ```

pub mod evaluator;
pub mod executor;
pub mod github_api;
pub mod pr_shepherd;
pub mod service;
pub mod types;
pub mod watchers;

pub use service::{get_trigger_service, start_trigger_service, stop_trigger_service};
