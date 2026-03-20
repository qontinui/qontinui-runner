//! Meta-Optimizer System
//!
//! Three focused optimizer agents that analyze historical run data and produce
//! recommendations to improve distinct layers of the system:
//!
//! - **Pipeline Prompt Optimizer** — Rewrites system prompts for pipeline agents
//! - **Architecture Optimizer** — Compares workflow architectures and tunes parameters
//! - **Generation Template Optimizer** — Improves workflow generation rules
//!
//! All outputs are recommendations (never auto-applied). Human applies from UI.
//!
//! ## Module structure
//!
//! - `types` — Shared types (OptimizerType, Recommendation, etc.)
//! - `trigger` — Shared trigger coordinator (threshold check, guard logic, launch)
//! - `prompt_registry` — CRUD for prompt_registry table
//! - `recommendations` — CRUD for meta_optimizer_recommendations table
//! - `pipeline_prompt_optimizer` — Workflow builder for Agent 1
//! - `architecture_optimizer` — Workflow builder for Agent 2
//! - `generation_template_optimizer` — Workflow builder for Agent 3

pub mod architecture_optimizer;
pub mod canary;
pub mod failure_analysis;
pub mod generation_template_optimizer;
pub mod parser;
pub mod pipeline_prompt_optimizer;
pub mod prompt_registry;
pub mod recommendations;
pub mod snapshots;
pub mod trigger;
pub mod types;
