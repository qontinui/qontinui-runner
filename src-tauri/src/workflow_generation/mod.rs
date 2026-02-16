//! AI-Powered Workflow Generation Module
//!
//! This module provides functionality to generate UnifiedWorkflows from
//! natural language descriptions using AI.

pub mod example_workflows;
pub mod feedback;
pub mod generator;
pub mod hardener;
pub mod meta_workflow;
pub mod relevance_filter;
pub mod rules;
pub mod schema_context;
pub mod self_improve;
pub mod similar_workflows;
pub mod step_type_metadata;
pub mod validation;

pub use generator::{
    extract_json_from_response, generate_workflow, GenerateWorkflowRequest,
    GenerateWorkflowResponse,
};
