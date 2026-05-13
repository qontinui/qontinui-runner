//! Productivity-stack Coordinator support modules.
//!
//! The HTTP wire surface lives in `crate::mcp::coordinator` (kept there
//! because it owns the `CoordinatorAction` discriminated-union wire type
//! and the `/coordinator/*` Axum routes). This module hosts the
//! intra-crate side-effect dispatcher (Phase 1a) and the in-process Rust
//! scheduler (Phase 1b) so both can drive auto-act actions through the
//! same code path.

pub mod act;
pub mod config;
mod decide;
pub mod deconflicter;
mod llm_decide;
mod observe;
pub mod scheduler;

pub use scheduler::CoordinatorSchedulerHandle;
