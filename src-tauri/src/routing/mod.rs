//! Adaptive routing infrastructure.
//!
//! - `q_router` — Q-learning over WorkflowArchitecture selection, keyed by
//!   `TaskState = (Domain × ComplexityTier × has_ui)`. The taxonomy is used by
//!   `ai_router`; `QRouter::compute_reward` is used by `orchestrator::
//!   learning_recorder::update_q_routing_table_pg` on every workflow outcome.
//! - `model_q_router` — `PhaseModelRouter` Q-table for per-phase model-tier
//!   selection, referenced by `ai_router`.
//! - `model_profiles` — model performance profiles (pass rate, cost efficiency,
//!   cache stats) persisted in the `model_profiles` PG table, read/written by
//!   the meta-optimizer commands.

pub mod model_profiles;
pub mod model_q_router;
pub mod q_router;
