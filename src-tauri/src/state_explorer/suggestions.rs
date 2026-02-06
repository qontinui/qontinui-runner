//! AI-Suggested Assertions for State Explorer
//!
//! This module analyzes exploration results and suggests useful assertions
//! that can be added to the configuration to improve future explorations.
//!
//! # How It Works
//!
//! 1. After exploring states, the system analyzes patterns
//! 2. For consistently detected elements, it suggests visibility assertions
//! 3. For text elements, it suggests content assertions
//! 4. For timing patterns, it suggests performance assertions
//! 5. Users can accept/reject suggestions
//! 6. Accepted suggestions become part of the config for future runs
//!
//! # Usage
//!
//! ```ignore
//! let suggester = AssertionSuggester::new();
//! let suggestions = suggester.analyze_result(&exploration_result);

#![allow(dead_code)]
//!
//! for suggestion in suggestions {
//!     if user_accepts(&suggestion) {
//!         config.add_assertion(suggestion.to_assertion());
//!     }
//! }
//! ```

use serde::{Deserialize, Serialize};

use super::assertions::{AssertionType, StateAssertion};
use super::types::{ExplorationResult, ExplorationStatus, StateExplorationResult};

/// A suggested assertion generated from exploration analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedAssertion {
    /// Unique ID for this suggestion
    pub id: String,
    /// State ID this suggestion applies to
    pub state_id: String,
    /// State name for display
    pub state_name: String,
    /// The suggested assertion type
    pub assertion_type: AssertionType,
    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,
    /// Human-readable reason for the suggestion
    pub reason: String,
    /// Category of suggestion
    pub category: SuggestionCategory,
    /// Whether this should be a critical assertion
    pub suggested_critical: bool,
    /// Evidence supporting this suggestion
    pub evidence: Vec<String>,
    /// Whether this suggestion has been accepted
    #[serde(default)]
    pub accepted: bool,
    /// Whether this suggestion has been dismissed
    #[serde(default)]
    pub dismissed: bool,
}

/// Categories of assertion suggestions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionCategory {
    /// Element visibility assertions
    ElementVisibility,
    /// Text content assertions
    TextContent,
    /// Element state (enabled/disabled) assertions
    ElementState,
    /// Timing/performance assertions
    Timing,
    /// Element count assertions
    ElementCount,
    /// Custom pattern-based assertions
    Pattern,
}

impl SuggestionCategory {
    /// Get display name for category
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::ElementVisibility => "Element Visibility",
            Self::TextContent => "Text Content",
            Self::ElementState => "Element State",
            Self::Timing => "Performance",
            Self::ElementCount => "Element Count",
            Self::Pattern => "Pattern Match",
        }
    }

    /// Get description of what this category suggests
    pub fn description(&self) -> &'static str {
        match self {
            Self::ElementVisibility => {
                "Assertions about whether elements should be visible or hidden"
            }
            Self::TextContent => "Assertions about text content within elements",
            Self::ElementState => "Assertions about element enabled/disabled state",
            Self::Timing => "Assertions about timing and performance thresholds",
            Self::ElementCount => "Assertions about the number of matching elements",
            Self::Pattern => "Assertions based on detected patterns in the UI",
        }
    }
}

impl SuggestedAssertion {
    /// Create a new suggestion
    pub fn new(
        state_id: impl Into<String>,
        state_name: impl Into<String>,
        assertion_type: AssertionType,
        confidence: f64,
        reason: impl Into<String>,
        category: SuggestionCategory,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            state_id: state_id.into(),
            state_name: state_name.into(),
            assertion_type,
            confidence,
            reason: reason.into(),
            category,
            suggested_critical: false,
            evidence: Vec::new(),
            accepted: false,
            dismissed: false,
        }
    }

    /// Mark this suggestion as critical
    pub fn critical(mut self) -> Self {
        self.suggested_critical = true;
        self
    }

    /// Add evidence for this suggestion
    pub fn with_evidence(mut self, evidence: Vec<String>) -> Self {
        self.evidence = evidence;
        self
    }

    /// Add a single piece of evidence
    pub fn add_evidence(&mut self, evidence: impl Into<String>) {
        self.evidence.push(evidence.into());
    }

    /// Convert to an actual assertion
    pub fn to_assertion(&self) -> StateAssertion {
        let mut assertion = StateAssertion::new(self.assertion_type.clone());
        if self.suggested_critical {
            assertion = assertion.critical();
        }
        assertion = assertion.with_message(format!(
            "AI-suggested: {} (confidence: {:.0}%)",
            self.reason,
            self.confidence * 100.0
        ));
        assertion
    }

    /// Accept this suggestion
    pub fn accept(&mut self) {
        self.accepted = true;
        self.dismissed = false;
    }

    /// Dismiss this suggestion
    pub fn dismiss(&mut self) {
        self.dismissed = true;
        self.accepted = false;
    }

    /// Check if this suggestion is pending (not accepted or dismissed)
    pub fn is_pending(&self) -> bool {
        !self.accepted && !self.dismissed
    }
}

/// Collection of suggestions for a single exploration run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionSet {
    /// Run ID these suggestions are for
    pub run_id: String,
    /// When suggestions were generated
    pub generated_at: String,
    /// All suggestions
    pub suggestions: Vec<SuggestedAssertion>,
    /// Configuration used for suggestion generation
    pub config: SuggestionConfig,
}

impl SuggestionSet {
    /// Create a new suggestion set
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            suggestions: Vec::new(),
            config: SuggestionConfig::default(),
        }
    }

    /// Add a suggestion
    pub fn add(&mut self, suggestion: SuggestedAssertion) {
        self.suggestions.push(suggestion);
    }

    /// Get pending suggestions
    pub fn pending(&self) -> Vec<&SuggestedAssertion> {
        self.suggestions.iter().filter(|s| s.is_pending()).collect()
    }

    /// Get accepted suggestions
    pub fn accepted(&self) -> Vec<&SuggestedAssertion> {
        self.suggestions.iter().filter(|s| s.accepted).collect()
    }

    /// Get suggestions for a specific state
    pub fn for_state(&self, state_id: &str) -> Vec<&SuggestedAssertion> {
        self.suggestions
            .iter()
            .filter(|s| s.state_id == state_id)
            .collect()
    }

    /// Get suggestions by category
    pub fn by_category(&self, category: SuggestionCategory) -> Vec<&SuggestedAssertion> {
        self.suggestions
            .iter()
            .filter(|s| s.category == category)
            .collect()
    }

    /// Get high-confidence suggestions (>= threshold)
    pub fn high_confidence(&self, threshold: f64) -> Vec<&SuggestedAssertion> {
        self.suggestions
            .iter()
            .filter(|s| s.confidence >= threshold)
            .collect()
    }

    /// Accept suggestion by ID
    pub fn accept_by_id(&mut self, id: &str) -> bool {
        if let Some(s) = self.suggestions.iter_mut().find(|s| s.id == id) {
            s.accept();
            true
        } else {
            false
        }
    }

    /// Dismiss suggestion by ID
    pub fn dismiss_by_id(&mut self, id: &str) -> bool {
        if let Some(s) = self.suggestions.iter_mut().find(|s| s.id == id) {
            s.dismiss();
            true
        } else {
            false
        }
    }

    /// Generate summary of suggestions
    pub fn summary(&self) -> String {
        let total = self.suggestions.len();
        let pending = self.pending().len();
        let accepted = self.accepted().len();
        let dismissed = total - pending - accepted;

        format!(
            "{} suggestions: {} pending, {} accepted, {} dismissed",
            total, pending, accepted, dismissed
        )
    }

    /// Save suggestion set to disk
    pub fn save(&self, output_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
        let suggestions_dir = output_dir.join("suggestions");
        std::fs::create_dir_all(&suggestions_dir)
            .map_err(|e| format!("Failed to create suggestions directory: {}", e))?;

        let filename = format!("suggestions-{}.json", self.run_id);
        let path = suggestions_dir.join(&filename);

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize suggestions: {}", e))?;

        std::fs::write(&path, json)
            .map_err(|e| format!("Failed to write suggestions file: {}", e))?;

        Ok(path)
    }

    /// Load suggestion set from disk
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read suggestions file: {}", e))?;

        serde_json::from_str(&json).map_err(|e| format!("Failed to parse suggestions: {}", e))
    }
}

/// Configuration for suggestion generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionConfig {
    /// Minimum confidence threshold for suggestions
    pub min_confidence: f64,
    /// Whether to suggest element visibility assertions
    pub suggest_visibility: bool,
    /// Whether to suggest text content assertions
    pub suggest_text: bool,
    /// Whether to suggest element state assertions
    pub suggest_state: bool,
    /// Whether to suggest timing assertions
    pub suggest_timing: bool,
    /// Whether to suggest element count assertions
    pub suggest_count: bool,
    /// Timing threshold percentage for suggestions
    pub timing_threshold_percent: f64,
    /// Whether to only suggest for passed states
    pub only_passed_states: bool,
}

impl Default for SuggestionConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.7,
            suggest_visibility: true,
            suggest_text: true,
            suggest_state: true,
            suggest_timing: true,
            suggest_count: true,
            timing_threshold_percent: 50.0,
            only_passed_states: false,
        }
    }
}

/// Analyzes exploration results and generates assertion suggestions
pub struct AssertionSuggester {
    config: SuggestionConfig,
}

impl AssertionSuggester {
    /// Create a new assertion suggester with default config
    pub fn new() -> Self {
        Self {
            config: SuggestionConfig::default(),
        }
    }

    /// Create with custom config
    pub fn with_config(config: SuggestionConfig) -> Self {
        Self { config }
    }

    /// Analyze an exploration result and generate suggestions
    pub fn analyze(&self, result: &ExplorationResult) -> SuggestionSet {
        let mut suggestions = SuggestionSet::new(&result.run_id);
        suggestions.config = self.config.clone();

        for state_result in &result.state_explorations {
            // Skip failed states if configured
            if self.config.only_passed_states && state_result.status != ExplorationStatus::Passed {
                continue;
            }

            // Generate visibility suggestions
            if self.config.suggest_visibility {
                suggestions
                    .suggestions
                    .extend(self.suggest_visibility_assertions(state_result));
            }

            // Generate timing suggestions
            if self.config.suggest_timing {
                suggestions
                    .suggestions
                    .extend(self.suggest_timing_assertions(state_result));
            }

            // Generate element count suggestions
            if self.config.suggest_count {
                suggestions
                    .suggestions
                    .extend(self.suggest_count_assertions(state_result));
            }
        }

        suggestions
    }

    /// Suggest visibility assertions based on detected elements
    fn suggest_visibility_assertions(
        &self,
        state_result: &StateExplorationResult,
    ) -> Vec<SuggestedAssertion> {
        let mut suggestions = Vec::new();

        // Suggest assertions for high-confidence found elements
        for element_check in &state_result.expected_elements_found {
            if element_check.found && element_check.confidence >= self.config.min_confidence {
                let mut suggestion = SuggestedAssertion::new(
                    &state_result.state_id,
                    &state_result.state_name,
                    AssertionType::ElementVisible {
                        element_id: element_check.element.clone(),
                    },
                    element_check.confidence,
                    format!(
                        "Element '{}' consistently detected with {:.0}% confidence",
                        element_check.element,
                        element_check.confidence * 100.0
                    ),
                    SuggestionCategory::ElementVisibility,
                );

                suggestion.add_evidence(format!(
                    "Detected in state '{}' with confidence {:.2}",
                    state_result.state_name, element_check.confidence
                ));

                if let Some((x, y, w, h)) = element_check.location {
                    suggestion
                        .add_evidence(format!("Located at ({}, {}) with size {}x{}", x, y, w, h));
                }

                suggestions.push(suggestion);
            }
        }

        // Suggest assertions for unexpected elements that should NOT be visible
        for element_check in &state_result.unexpected_elements_found {
            if element_check.found && element_check.confidence >= self.config.min_confidence {
                let suggestion = SuggestedAssertion::new(
                    &state_result.state_id,
                    &state_result.state_name,
                    AssertionType::ElementNotVisible {
                        element_id: element_check.element.clone(),
                    },
                    element_check.confidence,
                    format!(
                        "Unexpected element '{}' was found - consider adding assertion to catch this",
                        element_check.element
                    ),
                    SuggestionCategory::ElementVisibility,
                )
                .critical()
                .with_evidence(vec![format!(
                    "Element found unexpectedly with confidence {:.2}",
                    element_check.confidence
                )]);

                suggestions.push(suggestion);
            }
        }

        suggestions
    }

    /// Suggest timing assertions based on observed performance
    fn suggest_timing_assertions(
        &self,
        state_result: &StateExplorationResult,
    ) -> Vec<SuggestedAssertion> {
        let mut suggestions = Vec::new();

        // Suggest max duration assertion if timing is consistent
        if state_result.duration_ms > 0 {
            // Suggest a max timing with some buffer
            let suggested_max = (state_result.duration_ms as f64 * 1.5) as u64;
            let confidence = if state_result.status == ExplorationStatus::Passed {
                0.8
            } else {
                0.5
            };

            if confidence >= self.config.min_confidence {
                let suggestion = SuggestedAssertion::new(
                    &state_result.state_id,
                    &state_result.state_name,
                    AssertionType::Custom {
                        expression: format!("state_duration_ms <= {}", suggested_max),
                    },
                    confidence,
                    format!(
                        "State exploration took {}ms - suggest max threshold of {}ms",
                        state_result.duration_ms, suggested_max
                    ),
                    SuggestionCategory::Timing,
                )
                .with_evidence(vec![
                    format!("Observed duration: {}ms", state_result.duration_ms),
                    format!("Suggested max: {}ms (150% of observed)", suggested_max),
                ]);

                suggestions.push(suggestion);
            }
        }

        suggestions
    }

    /// Suggest element count assertions
    fn suggest_count_assertions(
        &self,
        state_result: &StateExplorationResult,
    ) -> Vec<SuggestedAssertion> {
        let mut suggestions = Vec::new();

        let found_count = state_result
            .expected_elements_found
            .iter()
            .filter(|e| e.found)
            .count();

        // Suggest assertion if we have multiple expected elements
        if found_count >= 2 {
            let avg_confidence = state_result
                .expected_elements_found
                .iter()
                .filter(|e| e.found)
                .map(|e| e.confidence)
                .sum::<f64>()
                / found_count as f64;

            if avg_confidence >= self.config.min_confidence {
                let suggestion = SuggestedAssertion::new(
                    &state_result.state_id,
                    &state_result.state_name,
                    AssertionType::ElementCount {
                        pattern: "expected_elements".to_string(),
                        expected_count: found_count,
                    },
                    avg_confidence,
                    format!(
                        "Consistently found {} expected elements - consider asserting count",
                        found_count
                    ),
                    SuggestionCategory::ElementCount,
                )
                .with_evidence(vec![
                    format!("Found {} of expected elements", found_count),
                    format!("Average confidence: {:.0}%", avg_confidence * 100.0),
                ]);

                suggestions.push(suggestion);
            }
        }

        suggestions
    }

    /// Suggest assertions based on comparison with previous runs
    pub fn suggest_from_comparison(
        &self,
        current: &ExplorationResult,
        baseline: &super::baseline::ExplorationBaseline,
    ) -> SuggestionSet {
        let mut suggestions = SuggestionSet::new(&current.run_id);
        suggestions.config = self.config.clone();

        // Compare and suggest assertions for stable elements
        for (state_id, baseline_sig) in &baseline.state_signatures {
            if let Some(current_state) = current
                .state_explorations
                .iter()
                .find(|s| &s.state_id == state_id)
            {
                // Find elements that are stable across runs
                let current_elements: std::collections::HashSet<_> = current_state
                    .expected_elements_found
                    .iter()
                    .filter(|e| e.found)
                    .map(|e| e.element.clone())
                    .collect();

                let baseline_elements: std::collections::HashSet<_> =
                    baseline_sig.elements_found.iter().cloned().collect();

                // Elements present in both runs are good candidates for assertions
                let stable_elements: Vec<_> =
                    current_elements.intersection(&baseline_elements).collect();

                for element in stable_elements {
                    let current_check = current_state
                        .expected_elements_found
                        .iter()
                        .find(|e| &e.element == element);

                    let confidence = current_check.map(|e| e.confidence).unwrap_or(0.9);

                    if confidence >= self.config.min_confidence {
                        let suggestion = SuggestedAssertion::new(
                            state_id,
                            &current_state.state_name,
                            AssertionType::ElementVisible {
                                element_id: element.clone(),
                            },
                            (confidence + 0.9) / 2.0, // Boost confidence for stable elements
                            format!(
                                "Element '{}' stable across baseline and current run",
                                element
                            ),
                            SuggestionCategory::ElementVisibility,
                        )
                        .with_evidence(vec![
                            "Present in baseline run".to_string(),
                            "Present in current run".to_string(),
                            format!("Detection confidence: {:.0}%", confidence * 100.0),
                        ]);

                        suggestions.add(suggestion);
                    }
                }
            }
        }

        suggestions
    }
}

impl Default for AssertionSuggester {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate markdown report of suggestions
pub fn suggestions_to_markdown(suggestions: &SuggestionSet) -> String {
    let mut md = String::new();

    md.push_str("# Assertion Suggestions\n\n");
    md.push_str(&format!("**Run ID:** {}\n", suggestions.run_id));
    md.push_str(&format!("**Generated:** {}\n\n", suggestions.generated_at));
    md.push_str(&format!("## Summary\n\n{}\n\n", suggestions.summary()));

    // Group by category
    for category in [
        SuggestionCategory::ElementVisibility,
        SuggestionCategory::TextContent,
        SuggestionCategory::ElementState,
        SuggestionCategory::Timing,
        SuggestionCategory::ElementCount,
        SuggestionCategory::Pattern,
    ] {
        let cat_suggestions = suggestions.by_category(category);
        if !cat_suggestions.is_empty() {
            md.push_str(&format!("## {}\n\n", category.display_name()));
            md.push_str(&format!("*{}*\n\n", category.description()));

            for suggestion in cat_suggestions {
                let status = if suggestion.accepted {
                    "✅ Accepted"
                } else if suggestion.dismissed {
                    "❌ Dismissed"
                } else {
                    "⏳ Pending"
                };

                md.push_str(&format!(
                    "### {} - {} ({:.0}% confidence) {}\n\n",
                    suggestion.state_name,
                    suggestion.assertion_type.description(),
                    suggestion.confidence * 100.0,
                    status
                ));
                md.push_str(&format!("**Reason:** {}\n\n", suggestion.reason));

                if !suggestion.evidence.is_empty() {
                    md.push_str("**Evidence:**\n");
                    for evidence in &suggestion.evidence {
                        md.push_str(&format!("- {}\n", evidence));
                    }
                    md.push('\n');
                }

                if suggestion.suggested_critical {
                    md.push_str("⚠️ **Suggested as critical assertion**\n\n");
                }
            }
        }
    }

    // High confidence suggestions summary
    let high_conf = suggestions.high_confidence(0.9);
    if !high_conf.is_empty() {
        md.push_str("## High Confidence Suggestions (≥90%)\n\n");
        md.push_str(
            "These suggestions have very high confidence and are recommended for adoption:\n\n",
        );
        for suggestion in high_conf {
            md.push_str(&format!(
                "- **{}**: {} ({:.0}%)\n",
                suggestion.state_name,
                suggestion.reason,
                suggestion.confidence * 100.0
            ));
        }
        md.push('\n');
    }

    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_explorer::types::ElementCheck;

    fn create_test_state_result() -> StateExplorationResult {
        StateExplorationResult {
            state_id: "test_state".to_string(),
            state_name: "Test State".to_string(),
            status: ExplorationStatus::Passed,
            screenshot_path: None,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            completed_at: Some("2024-01-01T00:00:01Z".to_string()),
            duration_ms: 1000,
            expected_elements_found: vec![
                ElementCheck {
                    element: "button_1".to_string(),
                    found: true,
                    confidence: 0.95,
                    location: Some((100, 100, 50, 30)),
                },
                ElementCheck {
                    element: "input_1".to_string(),
                    found: true,
                    confidence: 0.88,
                    location: None,
                },
            ],
            unexpected_elements_found: vec![],
            detection_confidence: 0.9,
            error: None,
            ai_notes: None,
        }
    }

    #[test]
    fn test_suggestion_creation() {
        let suggestion = SuggestedAssertion::new(
            "state_1",
            "State One",
            AssertionType::ElementVisible {
                element_id: "button".to_string(),
            },
            0.95,
            "Test reason",
            SuggestionCategory::ElementVisibility,
        );

        assert_eq!(suggestion.state_id, "state_1");
        assert_eq!(suggestion.confidence, 0.95);
        assert!(!suggestion.accepted);
        assert!(!suggestion.dismissed);
        assert!(suggestion.is_pending());
    }

    #[test]
    fn test_suggestion_accept_dismiss() {
        let mut suggestion = SuggestedAssertion::new(
            "state_1",
            "State One",
            AssertionType::ElementVisible {
                element_id: "button".to_string(),
            },
            0.95,
            "Test",
            SuggestionCategory::ElementVisibility,
        );

        suggestion.accept();
        assert!(suggestion.accepted);
        assert!(!suggestion.dismissed);
        assert!(!suggestion.is_pending());

        suggestion.dismiss();
        assert!(!suggestion.accepted);
        assert!(suggestion.dismissed);
        assert!(!suggestion.is_pending());
    }

    #[test]
    fn test_suggestion_to_assertion() {
        let suggestion = SuggestedAssertion::new(
            "state_1",
            "State One",
            AssertionType::ElementVisible {
                element_id: "button".to_string(),
            },
            0.95,
            "Test reason",
            SuggestionCategory::ElementVisibility,
        )
        .critical();

        let assertion = suggestion.to_assertion();
        assert!(assertion.is_critical);
    }

    #[test]
    fn test_suggester_visibility() {
        let suggester = AssertionSuggester::new();
        let state_result = create_test_state_result();
        let suggestions = suggester.suggest_visibility_assertions(&state_result);

        assert!(!suggestions.is_empty());
        // Should have suggestions for high-confidence elements
        assert!(suggestions
            .iter()
            .any(|s| matches!(&s.assertion_type, AssertionType::ElementVisible { element_id } if element_id == "button_1")));
    }

    #[test]
    fn test_suggestion_set_operations() {
        let mut set = SuggestionSet::new("run_1");

        set.add(SuggestedAssertion::new(
            "state_1",
            "State One",
            AssertionType::ElementVisible {
                element_id: "a".to_string(),
            },
            0.95,
            "Test 1",
            SuggestionCategory::ElementVisibility,
        ));

        set.add(SuggestedAssertion::new(
            "state_1",
            "State One",
            AssertionType::ElementVisible {
                element_id: "b".to_string(),
            },
            0.5,
            "Test 2",
            SuggestionCategory::ElementVisibility,
        ));

        assert_eq!(set.pending().len(), 2);
        assert_eq!(set.high_confidence(0.9).len(), 1);
        assert_eq!(set.for_state("state_1").len(), 2);
        assert_eq!(
            set.by_category(SuggestionCategory::ElementVisibility).len(),
            2
        );
    }

    #[test]
    fn test_category_display() {
        assert_eq!(
            SuggestionCategory::ElementVisibility.display_name(),
            "Element Visibility"
        );
        assert!(!SuggestionCategory::Timing.description().is_empty());
    }
}
