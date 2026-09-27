//! Orchestration Loop — runner-side workflow loop engine.
//!
//! Runs iterative workflow loops that can target any runner (self or another).
//! The loop executes workflows, evaluates exit conditions, and — for the
//! restart between-iteration modes — restarts the TARGET runner through a
//! path resolved once, before the loop starts.
//!
//! # Architecture
//!
//! ```text
//! Orchestrating Runner (this runner)
//!   │
//!   ├─ Pre-start: resolve the restart path (refuse "unsupported here: …")
//!   ├─ Start workflow on Target Runner (HTTP API)
//!   ├─ Poll for completion
//!   ├─ Trigger reflection on Target Runner
//!   ├─ Evaluate exit condition (0 fixes = done)
//!   ├─ Restart Target Runner via the resolved path
//!   │    ├─ InstanceManager — in-process stop + relaunch of a child this
//!   │    │                    runner spawned (the path a published install uses)
//!   │    └─ DevSupervisor   — `rebuild: true` only, on a dev box whose
//!   │                         supervisor can compile from a source checkout
//!   └─ Repeat
//! ```
//!
//! Restarts of runner-managed targets always go in-process through
//! `InstanceManager`, on dev boxes too, so the dev box exercises the same path
//! users get. The dev supervisor is used for exactly one thing — rebuilding
//! from source — and is never a silent fallback for a plain restart.
//!
//! The orchestrating runner is never restarted — only the target runner is. A
//! restart mode aimed at the orchestrating runner itself (the default target)
//! is refused before the loop starts (`target_is_orchestrator`), as is a
//! target this runner did not spawn (`target_not_runner_managed`) and a
//! rebuild with no reachable dev supervisor (`rebuild_needs_dev_supervisor`).
//! `orchestration_loop_restart_capability` reports the same verdict to the UI
//! without starting anything.

pub mod ai_session_executor;
pub mod commands;
pub mod conductor;
pub mod context_summarizer;
pub mod coord_gate;
pub mod diagnostician;
pub mod fix_agent;
pub mod intervention;
pub mod ledger;
pub mod loop_engine;
pub mod org_chart;
pub mod remote_client;
pub mod restart_path;
pub mod stall_detector;
pub mod subtask_executor;
pub mod task_decomposer;
pub mod types;
