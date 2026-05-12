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
//!
//! [`backfill_tasks`] is a one-shot maintenance helper for the
//! `coord-task-status-hygiene` plan (Phase 3): it cross-references stuck
//! `coord.tasks` rows against `git log` on `main` and flips matches to
//! `done`. Dry-run by default.

pub mod backfill_tasks;
pub mod decompose;
pub mod review;
pub mod rewind;
pub mod summarize;
pub mod workers;
