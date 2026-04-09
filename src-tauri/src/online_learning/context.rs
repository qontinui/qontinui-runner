//! Context encoder trait and implementations.
//!
//! A context encoder maps rich input data to a discrete state key string.
//! The bandit framework uses this to index into its Q-table.

/// Trait for encoding rich context into a discrete state key.
pub trait ContextEncoder: Send + Sync {
    /// The input type to encode.
    type Input;

    /// Encode the input into a discrete state key string.
    fn encode(&self, input: &Self::Input) -> String;
}

/// Identity encoder: the input is already a string key.
/// Useful for testing and simple cases.
pub struct IdentityEncoder;

impl ContextEncoder for IdentityEncoder {
    type Input = String;

    fn encode(&self, input: &String) -> String {
        input.clone()
    }
}

/// Model routing context for the bandit model router.
#[derive(Debug, Clone)]
pub struct ModelRoutingContext {
    /// Length of the task prompt in characters.
    pub prompt_length: usize,
    /// Number of files involved.
    pub file_count: usize,
    /// Complexity tier (from learning_outcomes enrichment).
    pub complexity_tier: String,
    /// Primary domain tag.
    pub domain: String,
    /// Whether the task involves UI components.
    pub has_ui: bool,
    /// Step type (for per-step model routing, optional).
    pub step_type: Option<String>,
    /// Word count of the prompt (signal enrichment).
    pub word_count: usize,
    /// Number of code blocks in the prompt.
    pub code_block_count: usize,
    /// Number of distinct file paths mentioned.
    pub cross_file_dep_count: usize,
    /// Whether the prompt contains error traces.
    pub has_error_context: bool,
    /// Current iteration number (higher = harder problem).
    pub iteration_number: u32,
    /// Whether the previous fix attempt failed.
    pub previous_fix_failed: bool,
}

/// Encodes model routing context into a discrete state key.
///
/// State space: 3 prompt_buckets × 3 file_buckets × 5 complexity × 5 domain × 2 has_ui = 450 states.
/// With optional step_type, the space grows but remains manageable.
pub struct ModelRoutingEncoder;

impl ContextEncoder for ModelRoutingEncoder {
    type Input = ModelRoutingContext;

    fn encode(&self, input: &ModelRoutingContext) -> String {
        let prompt_bucket = match input.prompt_length {
            0..=500 => "short",
            501..=2000 => "medium",
            _ => "long",
        };
        let file_bucket = match input.file_count {
            0..=3 => "few",
            4..=10 => "moderate",
            _ => "many",
        };
        let domain = normalize_domain(&input.domain);
        let complexity = normalize_complexity(&input.complexity_tier);
        let ui = if input.has_ui { "ui" } else { "no_ui" };

        // Signal enrichment: effort bucket based on code blocks + file deps
        let effort = if input.code_block_count <= 1 && input.cross_file_dep_count <= 1 {
            "simple"
        } else if input.cross_file_dep_count <= 3 {
            "moderate"
        } else {
            "complex"
        };
        let retry = if input.previous_fix_failed {
            "retry"
        } else {
            "fresh"
        };

        format!(
            "{}:{}:{}:{}:{}:{}:{}",
            prompt_bucket, file_bucket, complexity, domain, ui, effort, retry
        )
    }
}

/// Task context encoder for strategy selection.
/// Simplified version using only domain + complexity + has_ui (matching Q-Router).
pub struct TaskContextEncoder;

impl ContextEncoder for TaskContextEncoder {
    type Input = TaskContext;

    fn encode(&self, input: &TaskContext) -> String {
        let domain = normalize_domain(&input.domain);
        let complexity = normalize_complexity(&input.complexity_tier);
        let ui = if input.has_ui { "ui" } else { "no_ui" };
        format!("{}:{}:{}", domain, complexity, ui)
    }
}

/// Simplified task context for strategy selection.
#[derive(Debug, Clone)]
pub struct TaskContext {
    pub domain: String,
    pub complexity_tier: String,
    pub has_ui: bool,
}

/// Normalize domain tags to the canonical set.
fn normalize_domain(domain: &str) -> &'static str {
    let lower = domain.to_lowercase();
    if lower.contains("frontend") || lower.contains("ui-bridge") {
        "frontend"
    } else if lower.contains("database") {
        "database"
    } else if lower.contains("testing") {
        "testing"
    } else if lower.contains("infra")
        || lower.contains("orchestration")
        || lower.contains("meta-optimizer")
    {
        "infra"
    } else if lower.contains("backend") || lower.contains("api") {
        "backend"
    } else {
        "backend" // default
    }
}

/// Normalize complexity tier to canonical values.
fn normalize_complexity(tier: &str) -> &'static str {
    match tier.to_lowercase().as_str() {
        "trivial" => "trivial",
        "simple" => "simple",
        "moderate" => "moderate",
        "complex" => "complex",
        "highly_complex" => "highly_complex",
        _ => "moderate", // fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_routing_encoder() {
        let encoder = ModelRoutingEncoder;
        let ctx = ModelRoutingContext {
            prompt_length: 300,
            file_count: 2,
            complexity_tier: "moderate".to_string(),
            domain: "backend".to_string(),
            has_ui: false,
            step_type: None,
            word_count: 0,
            code_block_count: 0,
            cross_file_dep_count: 0,
            has_error_context: false,
            iteration_number: 0,
            previous_fix_failed: false,
        };
        assert_eq!(
            encoder.encode(&ctx),
            "short:few:moderate:backend:no_ui:simple:fresh"
        );
    }

    #[test]
    fn test_model_routing_encoder_complex() {
        let encoder = ModelRoutingEncoder;
        let ctx = ModelRoutingContext {
            prompt_length: 5000,
            file_count: 15,
            complexity_tier: "highly_complex".to_string(),
            domain: "frontend".to_string(),
            has_ui: true,
            step_type: None,
            word_count: 0,
            code_block_count: 3,
            cross_file_dep_count: 5,
            has_error_context: true,
            iteration_number: 4,
            previous_fix_failed: true,
        };
        assert_eq!(
            encoder.encode(&ctx),
            "long:many:highly_complex:frontend:ui:complex:retry"
        );
    }

    #[test]
    fn test_task_context_encoder() {
        let encoder = TaskContextEncoder;
        let ctx = TaskContext {
            domain: "database".to_string(),
            complexity_tier: "complex".to_string(),
            has_ui: false,
        };
        assert_eq!(encoder.encode(&ctx), "database:complex:no_ui");
    }
}
