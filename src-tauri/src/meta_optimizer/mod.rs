//! Meta-Optimizer System
//!
//! Four focused optimizer agents that analyze historical run data and produce
//! recommendations to improve distinct layers of the system:
//!
//! - **Pipeline Prompt Optimizer** — Rewrites system prompts for pipeline agents
//! - **Architecture Optimizer** — Compares workflow architectures and tunes parameters
//! - **Generation Template Optimizer** — Improves workflow generation rules
//! - **Meta-Prompt Optimizer** — LLM-based iterative prompt rewriting with canary A/B testing
//!
//! All outputs are recommendations (never auto-applied). Human applies from UI.
//!
//! ## Module structure
//!
//! - `types` — Shared types (OptimizerType, Recommendation, etc.)
//! - `trigger` — Shared trigger coordinator (threshold check, guard logic, launch)
//! - `prompt_registry` — CRUD for prompt_registry table
//! - `prompt_extractor` — Extract prompt samples and compute group metrics
//! - `prompt_evolution` — Track prompt rewrite history and canary verdicts
//! - `recommendations` — CRUD for meta_optimizer_recommendations table
//! - `pipeline_prompt_optimizer` — Workflow builder for Agent 1
//! - `architecture_optimizer` — Workflow builder for Agent 2
//! - `generation_template_optimizer` — Workflow builder for Agent 3
//! - `meta_prompt_optimizer` — Workflow builder for Agent 4
//! - `agentic_metrics` — DeepEval-inspired 0.0–1.0 metric scoring for runs

pub mod agentic_metrics;
pub mod architecture_optimizer;
pub mod canary;
pub mod comparison_bridge;
pub mod cost_optimizer;
pub mod drift_detection;
pub mod eval_runner;
pub mod eval_spec;
pub mod failure_analysis;
pub mod generation_template_optimizer;
pub mod golden_dataset;
pub mod llm_judge_metrics;
pub mod meta_prompt_optimizer;
pub mod parser;
pub mod pipeline_prompt_optimizer;
pub mod prompt_evolution;
pub mod prompt_extractor;
pub mod prompt_registry;
pub mod recommendations;
pub mod robustness;
pub mod snapshots;
pub mod trigger;
pub mod types;

#[cfg(test)]
mod scout_integration_tests;
