//! AI-Powered Workflow Generation Module
//!
//! This module provides functionality to generate UnifiedWorkflows from
//! natural language descriptions using AI.

pub mod generator;
pub mod schema_context;
pub mod validation;

pub use generator::{generate_workflow, GenerateWorkflowRequest, GenerateWorkflowResponse};
pub use validation::validate_workflow;
