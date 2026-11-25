//! The session-lifecycle kernel (plan `2026-10-28-one-session-lifecycle-authority`,
//! Phase 37): pure evaluators a close path decides from.
//!
//! This first slice carries one evaluator, [`classify_child`], delivered by plan
//! `2026-11-04-runner-finished-close-closes-nothing-on-any-box` Phase 2a, and
//! armed by its Phase 2b for the `finished_close` arm only: that arm's census
//! reads a finished session's children through it, and its `graceful_exit`
//! call leaves a proven shim out of the any-child refusal.
//!
//! Nothing here performs I/O. The caller reads the process table, the command
//! lines (in each platform's [`Argv`] form) and a second, fresh table, and
//! passes all three in.

pub mod classify_child;
pub mod state;

pub use classify_child::{
    classify_child, classify_session, Argv, ChildClass, ChildVerdict, Platform, ProcRow, ProcTable,
    SessionVerdict,
};

pub use state::*;
