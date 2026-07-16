//! Tier-0 "Looping Agent" primitive — pure core (plan
//! `merge-shepherd-fixer-PLAN.md`, Phase 1).
//!
//! A looping agent is a **visible, never-expiring, self-healing** runner
//! terminal session that runs a playbook on a loop. The key simplification
//! (see `qontinui-dev-notes/playbooks/tier0-looping-agent-scope.md`): a
//! well-designed looping agent is **stateless per iteration** — its state
//! lives in a durable journal + live sources (GitHub, coord), so conversation
//! continuity is disposable. Context-low and crash recovery are both "kill
//! the session, spawn a fresh `--session-id` claude, re-read the journal" —
//! NO `--resume`, NO context summarization.
//!
//! This lib module holds everything PURE so `cargo test --lib` actually
//! type-checks and tests it (the runner's bin-only module tree is silently
//! skipped by `--lib` runs — see the `commands`-in-main.rs trap):
//!
//! - [`registry`] — the durable runner-local registry of looping-agent
//!   definitions + runtime state (JSON store, atomic writes, tolerant load),
//!   seeded with the built-in Merge Shepherd (DISABLED by default).
//! - [`idle`] — the rendered-VT-grid idle / context-low predicates. Grid
//!   scanning (not raw-byte scanning) is the fleet's proven approach: Claude
//!   Code paints whole frames as synchronized-output updates that hide text
//!   from rolling byte windows.
//! - [`policy`] — the supervisor's pure per-tick decision core: cadence
//!   gating, idle-nudge, context-low / K-cycle relaunch, death-respawn,
//!   spawn-failure backoff.
//! - [`playbook`] — the bundled built-in playbook(s) + the spawn/relaunch/
//!   nudge prompt builders.
//!
//! The impure glue (spawning visible tabs, submitting prompts, reading live
//! grids) lives in the runner bin at `src/looping_agent_supervisor.rs` and is
//! a thin composition of these cores over the existing
//! `run_continuation_terminal` spawn recipe.

pub mod idle;
pub mod playbook;
pub mod policy;
pub mod registry;
