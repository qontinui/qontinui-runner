//! AI-Powered Workflow Generation Module
//!
//! This module provides functionality to generate UnifiedWorkflows from
//! natural language descriptions using AI.

pub mod benchmark;
pub mod decomposition;
pub mod dependency_analysis;
pub mod discovery_tools;
pub mod example_workflows;
pub mod feedback;
pub mod generator;
pub mod hardener;
pub mod investigator;
pub mod meta_workflow;
pub mod pattern_mining;
pub mod pipeline_artifacts;
pub mod prompt_analysis;
pub mod relevance_filter;
pub mod revision;
pub mod rules;
pub mod schema_context;
pub mod self_improve;
pub mod similar_workflows;
pub mod spec_synthesis;
pub mod specification;
pub mod step_type_knowledge;
pub mod step_type_metadata;
pub mod template_promotion;
pub mod training_data;
pub mod validation;
pub mod verification_templates;

pub use generator::{
    extract_json_from_response, generate_workflow, GenerateWorkflowRequest,
    GenerateWorkflowResponse,
};
