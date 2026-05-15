//! AI-Powered Workflow Generation Module
//!
//! This module provides functionality to generate UnifiedWorkflows from
//! natural language descriptions using AI.

pub mod benchmark;
pub mod code_graph;
pub mod complexity;
pub mod consistency;
pub mod constitution;
pub mod decomposition;
pub mod dependency_analysis;
pub mod discovery_tools;
pub mod domain_generators;
pub mod domain_routing;
pub mod evaluation;
pub mod explorer;
pub mod few_shot_curator;
pub mod generator;
pub mod gepa_optimizer;
pub mod hardener;
pub mod investigator;
pub mod meta_workflow;
pub mod pipeline_artifacts;
pub mod prm_export;
pub mod prompt_analysis;
pub mod reflector;
pub mod relevance_filter;
pub mod revision;
pub mod rules;
pub mod schema_context;
pub mod self_improve;
pub mod similar_workflows;
// Stream E (Flywheel) — coverage-growth loop. Gated behind the
// `spec-authoring` Cargo feature so default builds never compile it.
#[cfg(feature = "spec-authoring")]
pub mod spec_authoring;
pub mod spec_synthesis;
pub mod specification;
pub mod step_type_knowledge;
pub mod step_type_metadata;
pub mod structured_output;
pub mod template_library;
pub mod template_lifecycle;
pub mod template_promotion;
pub mod training_data;
pub mod validation;
pub mod verification_templates;
pub mod wrapper_manifest;

pub use generator::{
    extract_json_from_response, generate_workflow, GenerateWorkflowRequest,
    GenerateWorkflowResponse,
};
