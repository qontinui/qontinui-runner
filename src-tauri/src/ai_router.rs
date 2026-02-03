//! AI Task Router Module
//!
//! Implements intelligent task routing to select the appropriate AI model
//! based on task complexity. This optimizes cost and latency by using:
//! - Fast/cheap models (Haiku) for simple tasks
//! - Balanced models (Sonnet) for medium complexity
//! - Powerful models (Opus) for complex tasks
//!
//! Inspired by agenticSeek's task routing pattern.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for intelligent task routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// Whether routing is enabled (if false, use default model)
    #[serde(default)]
    pub enabled: bool,

    /// Model to use for simple tasks (fast, cheap)
    #[serde(default = "default_simple_model")]
    pub simple_model: String,

    /// Model to use for medium complexity tasks (balanced)
    #[serde(default = "default_medium_model")]
    pub medium_model: String,

    /// Model to use for complex tasks (powerful)
    #[serde(default = "default_complex_model")]
    pub complex_model: String,

    /// File count thresholds: (simple_max, medium_max)
    /// - 0 to simple_max files = Simple
    /// - simple_max+1 to medium_max files = Medium
    /// - medium_max+ files = Complex
    #[serde(default = "default_file_thresholds")]
    pub file_count_thresholds: (usize, usize),

    /// Prompt length thresholds: (simple_max, medium_max)
    #[serde(default = "default_prompt_thresholds")]
    pub prompt_length_thresholds: (usize, usize),

    /// Keywords that indicate complex tasks
    #[serde(default = "default_complex_keywords")]
    pub complex_keywords: Vec<String>,

    /// Keywords that indicate simple tasks
    #[serde(default = "default_simple_keywords")]
    pub simple_keywords: Vec<String>,
}

fn default_simple_model() -> String {
    "claude-3-5-haiku-20241022".to_string()
}

fn default_medium_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

fn default_complex_model() -> String {
    "claude-opus-4-20250514".to_string()
}

fn default_file_thresholds() -> (usize, usize) {
    (3, 10) // 1-3 simple, 4-10 medium, 11+ complex
}

fn default_prompt_thresholds() -> (usize, usize) {
    (500, 2000) // <500 simple, 500-2000 medium, >2000 complex
}

fn default_complex_keywords() -> Vec<String> {
    vec![
        "refactor".to_string(),
        "architecture".to_string(),
        "redesign".to_string(),
        "migrate".to_string(),
        "rewrite".to_string(),
        "optimize".to_string(),
        "security audit".to_string(),
        "performance".to_string(),
        "scalability".to_string(),
    ]
}

fn default_simple_keywords() -> Vec<String> {
    vec![
        "fix typo".to_string(),
        "add comment".to_string(),
        "rename".to_string(),
        "format".to_string(),
        "lint".to_string(),
        "simple".to_string(),
        "quick".to_string(),
        "small change".to_string(),
    ]
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default - opt-in feature
            simple_model: default_simple_model(),
            medium_model: default_medium_model(),
            complex_model: default_complex_model(),
            file_count_thresholds: default_file_thresholds(),
            prompt_length_thresholds: default_prompt_thresholds(),
            complex_keywords: default_complex_keywords(),
            simple_keywords: default_simple_keywords(),
        }
    }
}

// ============================================================================
// Task Complexity
// ============================================================================

/// Assessed complexity level of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    /// Simple tasks: quick fixes, small changes, formatting
    Simple,
    /// Medium tasks: feature additions, bug fixes, moderate refactoring
    Medium,
    /// Complex tasks: architecture changes, major refactoring, security audits
    Complex,
}

impl TaskComplexity {
    /// Get a human-readable name for this complexity level.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Simple => "Simple",
            Self::Medium => "Medium",
            Self::Complex => "Complex",
        }
    }
}

// ============================================================================
// Complexity Assessment
// ============================================================================

/// Context for assessing task complexity.
#[derive(Debug, Default)]
pub struct TaskContext {
    /// The task prompt/description
    pub prompt: String,
    /// Number of files involved (if known)
    pub file_count: Option<usize>,
    /// Number of verification criteria (if known)
    pub criteria_count: Option<usize>,
    /// File paths involved (for pattern analysis)
    pub file_paths: Vec<String>,
}

impl TaskContext {
    /// Create a new task context with just a prompt.
    pub fn from_prompt(prompt: &str) -> Self {
        Self {
            prompt: prompt.to_string(),
            ..Default::default()
        }
    }

    /// Add file count information.
    pub fn with_file_count(mut self, count: usize) -> Self {
        self.file_count = Some(count);
        self
    }

    /// Add criteria count information.
    pub fn with_criteria_count(mut self, count: usize) -> Self {
        self.criteria_count = Some(count);
        self
    }

    /// Add file paths for analysis.
    pub fn with_file_paths(mut self, paths: Vec<String>) -> Self {
        self.file_paths = paths;
        self
    }
}

/// Result of complexity assessment with reasoning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityAssessment {
    /// The assessed complexity level
    pub complexity: TaskComplexity,
    /// Confidence in the assessment (0.0 to 1.0)
    pub confidence: f32,
    /// Factors that contributed to this assessment
    pub factors: Vec<String>,
    /// The model selected based on this complexity
    pub selected_model: String,
}

// ============================================================================
// Task Router
// ============================================================================

/// Service for routing tasks to appropriate AI models.
pub struct TaskRouter {
    config: RoutingConfig,
}

impl TaskRouter {
    /// Create a new task router with the given configuration.
    pub fn new(config: RoutingConfig) -> Self {
        Self { config }
    }

    /// Create a router with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(RoutingConfig::default())
    }

    /// Check if routing is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Assess the complexity of a task and select an appropriate model.
    ///
    /// Returns the assessment including the selected model.
    pub fn assess_and_route(&self, context: &TaskContext) -> ComplexityAssessment {
        let mut factors = Vec::new();
        let mut scores: Vec<(TaskComplexity, f32)> = Vec::new();

        // Factor 1: Keyword analysis
        let prompt_lower = context.prompt.to_lowercase();

        // Check for complex keywords
        let complex_keyword_matches: Vec<_> = self
            .config
            .complex_keywords
            .iter()
            .filter(|kw| prompt_lower.contains(&kw.to_lowercase()))
            .collect();

        if !complex_keyword_matches.is_empty() {
            factors.push(format!(
                "Complex keywords found: {}",
                complex_keyword_matches
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            scores.push((TaskComplexity::Complex, 0.4));
        }

        // Check for simple keywords
        let simple_keyword_matches: Vec<_> = self
            .config
            .simple_keywords
            .iter()
            .filter(|kw| prompt_lower.contains(&kw.to_lowercase()))
            .collect();

        if !simple_keyword_matches.is_empty() {
            factors.push(format!(
                "Simple keywords found: {}",
                simple_keyword_matches
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            scores.push((TaskComplexity::Simple, 0.3));
        }

        // Factor 2: Prompt length
        let prompt_len = context.prompt.len();
        let (simple_max, medium_max) = self.config.prompt_length_thresholds;

        if prompt_len <= simple_max {
            factors.push(format!("Short prompt ({} chars)", prompt_len));
            scores.push((TaskComplexity::Simple, 0.2));
        } else if prompt_len <= medium_max {
            factors.push(format!("Medium prompt ({} chars)", prompt_len));
            scores.push((TaskComplexity::Medium, 0.2));
        } else {
            factors.push(format!("Long prompt ({} chars)", prompt_len));
            scores.push((TaskComplexity::Complex, 0.2));
        }

        // Factor 3: File count (if provided)
        if let Some(file_count) = context.file_count {
            let (simple_max, medium_max) = self.config.file_count_thresholds;

            if file_count <= simple_max {
                factors.push(format!("Few files ({} files)", file_count));
                scores.push((TaskComplexity::Simple, 0.25));
            } else if file_count <= medium_max {
                factors.push(format!("Moderate files ({} files)", file_count));
                scores.push((TaskComplexity::Medium, 0.25));
            } else {
                factors.push(format!("Many files ({} files)", file_count));
                scores.push((TaskComplexity::Complex, 0.25));
            }
        }

        // Factor 4: Criteria count (if provided)
        if let Some(criteria_count) = context.criteria_count {
            if criteria_count <= 2 {
                factors.push(format!("Few criteria ({} criteria)", criteria_count));
                scores.push((TaskComplexity::Simple, 0.15));
            } else if criteria_count <= 5 {
                factors.push(format!("Moderate criteria ({} criteria)", criteria_count));
                scores.push((TaskComplexity::Medium, 0.15));
            } else {
                factors.push(format!("Many criteria ({} criteria)", criteria_count));
                scores.push((TaskComplexity::Complex, 0.15));
            }
        }

        // Calculate weighted scores for each complexity level
        let mut simple_score = 0.0f32;
        let mut medium_score = 0.0f32;
        let mut complex_score = 0.0f32;

        for (complexity, weight) in &scores {
            match complexity {
                TaskComplexity::Simple => simple_score += weight,
                TaskComplexity::Medium => medium_score += weight,
                TaskComplexity::Complex => complex_score += weight,
            }
        }

        // Determine final complexity
        let (complexity, confidence) =
            if complex_score >= simple_score && complex_score >= medium_score {
                (TaskComplexity::Complex, complex_score)
            } else if simple_score >= medium_score {
                (TaskComplexity::Simple, simple_score)
            } else {
                (TaskComplexity::Medium, medium_score)
            };

        // Normalize confidence to 0.0-1.0 range
        let total_score = simple_score + medium_score + complex_score;
        let confidence = if total_score > 0.0 {
            confidence / total_score
        } else {
            0.5 // Default confidence if no factors
        };

        // Select model based on complexity
        let selected_model = self.get_model_for_complexity(complexity);

        debug!(
            "Task complexity assessment: {:?} (confidence: {:.2}), model: {}",
            complexity, confidence, selected_model
        );

        ComplexityAssessment {
            complexity,
            confidence,
            factors,
            selected_model,
        }
    }

    /// Get the model for a given complexity level.
    pub fn get_model_for_complexity(&self, complexity: TaskComplexity) -> String {
        match complexity {
            TaskComplexity::Simple => self.config.simple_model.clone(),
            TaskComplexity::Medium => self.config.medium_model.clone(),
            TaskComplexity::Complex => self.config.complex_model.clone(),
        }
    }

    /// Convenience method to assess and return just the model name.
    pub fn route_task(&self, context: &TaskContext) -> String {
        if !self.config.enabled {
            // Return medium model as default when routing is disabled
            return self.config.medium_model.clone();
        }

        let assessment = self.assess_and_route(context);
        info!(
            "Routed task to {} ({:?}, confidence: {:.2})",
            assessment.selected_model, assessment.complexity, assessment.confidence
        );
        assessment.selected_model
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = RoutingConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.file_count_thresholds, (3, 10));
    }

    #[test]
    fn test_simple_task_detection() {
        let config = RoutingConfig {
            enabled: true,
            ..Default::default()
        };
        let router = TaskRouter::new(config);

        let context = TaskContext::from_prompt("Fix typo in README");
        let assessment = router.assess_and_route(&context);

        assert_eq!(assessment.complexity, TaskComplexity::Simple);
    }

    #[test]
    fn test_complex_task_detection() {
        let config = RoutingConfig {
            enabled: true,
            ..Default::default()
        };
        let router = TaskRouter::new(config);

        let context = TaskContext::from_prompt(
            "Refactor the entire authentication system to use OAuth2 and migrate the database schema",
        )
        .with_file_count(15)
        .with_criteria_count(8);

        let assessment = router.assess_and_route(&context);

        assert_eq!(assessment.complexity, TaskComplexity::Complex);
    }

    #[test]
    fn test_medium_task_detection() {
        let config = RoutingConfig {
            enabled: true,
            ..Default::default()
        };
        let router = TaskRouter::new(config);

        let context = TaskContext::from_prompt("Add a new API endpoint for user preferences")
            .with_file_count(5);

        let assessment = router.assess_and_route(&context);

        // Should be medium (moderate file count, no strong keywords)
        assert!(
            assessment.complexity == TaskComplexity::Medium
                || assessment.complexity == TaskComplexity::Simple
        );
    }

    #[test]
    fn test_routing_disabled() {
        let config = RoutingConfig {
            enabled: false,
            ..Default::default()
        };
        let router = TaskRouter::new(config);

        let context = TaskContext::from_prompt("Refactor everything");
        let model = router.route_task(&context);

        // Should return medium model (default) when disabled
        assert_eq!(model, default_medium_model());
    }
}
