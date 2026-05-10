//! In-product productivity-stack features (Phase 3+).
//!
//! Promotes the personal `qontinui-claude-config` slash commands into Rust
//! functions that any qontinui user can run from the UI without depending
//! on the Claude CLI or the personal config repo.
//!
//! See `plans/productivity-stack-product-readiness.md` for the multi-phase
//! design.
//! - Phase 3 ships [`decompose`].
//! - Phase 4 ships [`review`].
//! - Phase 5 ships [`rewind`] + [`summarize`].

pub mod decompose;
pub mod review;
pub mod rewind;
pub mod summarize;
pub mod workers;
