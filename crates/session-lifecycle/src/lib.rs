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

//!
//! **Out of scope here.** The interim-host narrowing conjuncts I2 (composer
//! witness), I3 (continuation-permit pacing), I4 (root-cwd move), I5 (close rate
//! limit) and I7 (tenant proof) belong to Phases 52 and 54, where the interim
//! host exists; this kernel carries only I1, I6 and I8 (see [`guard`]).

pub mod classify_child;
pub mod decide;
pub mod evaluate;
pub mod guard;
pub mod model;
pub mod state;
pub mod stray;
#[cfg(test)]
mod testkit;

pub use classify_child::{
    classify_child, classify_child_with_strays, classify_session, classify_session_with_strays,
    Argv, ChildClass, ChildVerdict, Platform, ProcRow, ProcTable, SessionVerdict,
};
pub use decide::{
    closed_by_creator, decide, item_facts, operator_provenance, owed_facts, relayed_exit,
    resume_would_restore_schedule, Owed, BLOCKS_PER_HOUR, CONTINUE_WINDOW, MIGRATION_CAP,
    RESUME_ATTEMPTS_CAP,
};
pub use evaluate::*;
pub use guard::{close_action, evaluate as evaluate_guard, GuardReport};
pub use model::*;
pub use state::*;
pub use stray::{
    stray_verdict, FileFacts, NotStray, StrayClass, StrayContext, StrayEvidence, StrayFacts,
    StrayProc, StrayVerdict,
};
