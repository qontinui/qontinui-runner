//! The session-lifecycle kernel (plan `2026-10-28-one-session-lifecycle-authority`,
//! Phase 37): pure evaluators a close path decides from.
//!
//! This first slice carries one evaluator, [`classify_child`], delivered by plan
//! `2026-11-04-runner-finished-close-closes-nothing-on-any-box` Phase 2a. It is
//! SHADOW ONLY in the runner: the `finished_close` census logs what it would
//! admit or refuse and acts on nothing. Arming it is that plan's Phase 2b.
//!
//! Nothing here performs I/O. The caller reads the process table, the command
//! lines and a second, fresh table, and passes all three in.

pub mod classify_child;

pub use classify_child::{
    classify_child, classify_session, ChildClass, ChildVerdict, Platform, ProcRow, ProcTable,
    SessionVerdict,
};
