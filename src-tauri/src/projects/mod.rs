//! Projects — the dashboard's server-side view over data the runner already
//! holds.
//!
//! The registry itself (`SavedProject`, persisted in `settings.json`) and its
//! Tauri commands live in [`crate::commands::saved_projects`]. This module
//! owns the *join*: [`snapshot`] folds managed processes, live terminal
//! sessions, AI sessions, git state, pending questions, health and spend into
//! one [`qontinui_types::projects::ProjectSnapshot`], keyed on the project
//! root path.
//!
//! See `plans/2026-07-24-runner-projects-dashboard.md` §6.

pub mod snapshot;
