//! Schema Validation Module
//!
//! Provides runtime JSON Schema validation for LLM-generated structured output,
//! with structured error messages suitable for both human debugging and LLM retry feedback.
//!
//! # Architecture
//!
//! - [`validator`]: Core `SchemaValidator` wrapping the `jsonschema` crate
//! - [`feedback`]: Converts validation errors into LLM-consumable retry prompts
//! - [`coercion`]: Best-effort type coercion to reduce unnecessary validation retries

pub mod coercion;
pub mod constrained_decoding;
pub mod feedback;
pub mod retry_with_validation;
pub mod validator;

pub use coercion::CoercionPolicy;
pub use constrained_decoding::{
    prepare_schema_for_backend, prepare_raw_schema_for_backend, schema_to_gbnf,
    ConstrainedDecodingPayload,
};
pub use feedback::validation_feedback_prompt;
pub use retry_with_validation::{
    build_validation_retry_prompt, extract_json_from_output,
    process_ai_response_for_structured_output, validate_and_parse, StructuredOutputConfig,
    StructuredOutputError, StructuredOutputResult,
};
pub use validator::{SchemaValidator, ValidationError, ValidationResult};
