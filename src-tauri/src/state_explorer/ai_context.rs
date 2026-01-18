//! AI Analysis Context for State Explorer
//!
//! This module builds AI analysis context from state and transition descriptions,
//! enabling zero-config AI verification based on the semantic descriptions in
//! workflow configurations.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::types::{ElementCheck, StateExplorationResult, TransitionExplorationResult};
use crate::config::{StateDescription, TransitionDescription};

/// Timing information collected during exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingInfo {
    /// Time to detect state in milliseconds
    pub detection_ms: u64,
    /// Time to capture screenshot in milliseconds
    pub screenshot_ms: u64,
    /// Total exploration time for this state in milliseconds
    pub total_ms: u64,
}

/// Observations collected at a state during exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateObservations {
    /// Path to captured screenshot
    pub screenshot: Option<PathBuf>,
    /// Elements that were detected
    pub detected_elements: Vec<ElementCheck>,
    /// Elements that were expected (from description)
    pub expected_elements: Vec<String>,
    /// Elements that were expected but not found
    pub missing_elements: Vec<String>,
    /// Elements found that were not expected
    pub unexpected_elements: Vec<String>,
    /// Timing information
    pub timing: TimingInfo,
    /// Any console errors or warnings captured
    pub console_errors: Vec<String>,
    /// Detection confidence score (0.0 - 1.0)
    pub detection_confidence: f64,
}

/// Context for AI analysis of a single state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateAnalysisContext {
    /// State identifier
    pub state_id: String,
    /// State name for display
    pub state_name: String,
    /// Description from configuration (if available)
    pub description: Option<StateDescription>,
    /// Actual observations from exploration
    pub observations: StateObservations,
    /// Path taken to reach this state
    pub path_taken: Vec<String>,
    /// Issues found in previous states on this path
    pub previous_issues: Vec<String>,
}

impl StateAnalysisContext {
    /// Build context from exploration result and state info
    pub fn from_exploration(
        result: &StateExplorationResult,
        description: Option<StateDescription>,
        path_taken: Vec<String>,
        previous_issues: Vec<String>,
    ) -> Self {
        // Determine missing elements
        let expected = description
            .as_ref()
            .and_then(|d| d.expected_elements.clone())
            .unwrap_or_default();

        let found_elements: Vec<String> = result
            .expected_elements_found
            .iter()
            .filter(|e| e.found)
            .map(|e| e.element.clone())
            .collect();

        let missing: Vec<String> = expected
            .iter()
            .filter(|e| !found_elements.contains(e))
            .cloned()
            .collect();

        // Collect unexpected elements that were found (elements that should NOT be visible)
        let unexpected_found: Vec<String> = result
            .unexpected_elements_found
            .iter()
            .filter(|e| e.found)
            .map(|e| e.element.clone())
            .collect();

        Self {
            state_id: result.state_id.clone(),
            state_name: result.state_name.clone(),
            description,
            observations: StateObservations {
                screenshot: result.screenshot_path.as_ref().map(PathBuf::from),
                detected_elements: result.expected_elements_found.clone(),
                expected_elements: expected,
                missing_elements: missing,
                unexpected_elements: unexpected_found,
                timing: TimingInfo {
                    detection_ms: result.duration_ms / 2, // Estimate
                    screenshot_ms: result.duration_ms / 4,
                    total_ms: result.duration_ms,
                },
                console_errors: vec![],
                detection_confidence: result.detection_confidence,
            },
            path_taken,
            previous_issues,
        }
    }

    /// Generate an AI analysis prompt from the context
    pub fn to_analysis_prompt(&self) -> String {
        let mut prompt = String::new();

        // Header
        prompt.push_str(&format!(
            "# State Analysis: {} ({})\n\n",
            self.state_name, self.state_id
        ));

        // Description context (if available)
        if let Some(desc) = &self.description {
            prompt.push_str("## Expected State (from configuration)\n\n");
            prompt.push_str(&format!("**Summary:** {}\n\n", desc.summary));

            if let Some(user_goal) = &desc.user_goal {
                prompt.push_str(&format!("**User Goal:** {}\n\n", user_goal));
            }

            if let Some(elements) = &desc.expected_elements {
                if !elements.is_empty() {
                    prompt.push_str("**Expected Elements:**\n");
                    for elem in elements {
                        prompt.push_str(&format!("- {}\n", elem));
                    }
                    prompt.push('\n');
                }
            }

            if let Some(unexpected) = &desc.unexpected_elements {
                if !unexpected.is_empty() {
                    prompt.push_str("**Should NOT Be Visible:**\n");
                    for elem in unexpected {
                        prompt.push_str(&format!("- {}\n", elem));
                    }
                    prompt.push('\n');
                }
            }

            if let Some(verification) = &desc.verification_prompt {
                prompt.push_str(&format!("**Custom Verification:** {}\n\n", verification));
            }
        }

        // Actual observations
        prompt.push_str("## Actual Observations\n\n");
        prompt.push_str(&format!(
            "**Detection Confidence:** {:.1}%\n\n",
            self.observations.detection_confidence * 100.0
        ));

        // Found elements
        if !self.observations.detected_elements.is_empty() {
            prompt.push_str("**Detected Elements:**\n");
            for elem in &self.observations.detected_elements {
                let status = if elem.found { "✓" } else { "✗" };
                prompt.push_str(&format!(
                    "- {} {} (confidence: {:.1}%)\n",
                    status,
                    elem.element,
                    elem.confidence * 100.0
                ));
            }
            prompt.push('\n');
        }

        // Missing elements
        if !self.observations.missing_elements.is_empty() {
            prompt.push_str("**Missing Elements (expected but not found):**\n");
            for elem in &self.observations.missing_elements {
                prompt.push_str(&format!("- ✗ {}\n", elem));
            }
            prompt.push('\n');
        }

        // Unexpected elements found
        if !self.observations.unexpected_elements.is_empty() {
            prompt.push_str("**Unexpected Elements Found (should not be visible):**\n");
            for elem in &self.observations.unexpected_elements {
                prompt.push_str(&format!("- ⚠ {}\n", elem));
            }
            prompt.push('\n');
        }

        // Timing
        prompt.push_str(&format!(
            "**Timing:** {} ms total\n\n",
            self.observations.timing.total_ms
        ));

        // Path context
        if !self.path_taken.is_empty() {
            prompt.push_str("## Navigation Path\n\n");
            prompt.push_str(&format!("Reached via: {}\n\n", self.path_taken.join(" → ")));
        }

        // Previous issues
        if !self.previous_issues.is_empty() {
            prompt.push_str("## Previous Issues on Path\n\n");
            for issue in &self.previous_issues {
                prompt.push_str(&format!("- {}\n", issue));
            }
            prompt.push('\n');
        }

        // Analysis questions
        prompt.push_str("## Analysis Questions\n\n");
        prompt.push_str("1. Does the screenshot match the expected state description?\n");
        prompt.push_str("2. Are there any missing or unexpected elements?\n");
        prompt.push_str("3. Is this a valid application state or an error condition?\n");
        prompt.push_str("4. What actions might be needed to fix any discrepancies?\n");

        prompt
    }

    /// Check if this state has discrepancies that need analysis
    pub fn has_discrepancies(&self) -> bool {
        !self.observations.missing_elements.is_empty()
            || !self.observations.unexpected_elements.is_empty()
            || self.observations.detection_confidence < 0.5
    }
}

/// Context for AI analysis of a transition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionAnalysisContext {
    /// Transition identifier
    pub transition_id: String,
    /// Source state
    pub from_state: String,
    /// Target state
    pub to_state: String,
    /// Description from configuration (if available)
    pub description: Option<TransitionDescription>,
    /// Actual duration in milliseconds
    pub actual_duration_ms: u64,
    /// Whether duration exceeded expected
    pub duration_exceeded: bool,
    /// Screenshot before transition
    pub screenshot_before: Option<PathBuf>,
    /// Screenshot after transition
    pub screenshot_after: Option<PathBuf>,
    /// Error if transition failed
    pub error: Option<String>,
}

impl TransitionAnalysisContext {
    /// Build context from exploration result and transition info
    pub fn from_exploration(
        result: &TransitionExplorationResult,
        description: Option<TransitionDescription>,
    ) -> Self {
        Self {
            transition_id: result.transition_id.clone(),
            from_state: result.from_state_id.clone(),
            to_state: result.to_state_id.clone(),
            description,
            actual_duration_ms: result.duration_ms,
            duration_exceeded: result.duration_exceeded,
            screenshot_before: result.screenshot_before.as_ref().map(PathBuf::from),
            screenshot_after: result.screenshot_after.as_ref().map(PathBuf::from),
            error: result.error.clone(),
        }
    }

    /// Generate an AI analysis prompt for the transition
    pub fn to_analysis_prompt(&self) -> String {
        let mut prompt = String::new();

        // Header
        prompt.push_str(&format!(
            "# Transition Analysis: {} → {}\n\n",
            self.from_state, self.to_state
        ));
        prompt.push_str(&format!("**Transition ID:** {}\n\n", self.transition_id));

        // Description context
        if let Some(desc) = &self.description {
            prompt.push_str("## Expected Behavior (from configuration)\n\n");
            prompt.push_str(&format!("**Intent:** {}\n\n", desc.intent));

            if let Some(preconditions) = &desc.preconditions {
                if !preconditions.is_empty() {
                    prompt.push_str("**Preconditions:**\n");
                    for pre in preconditions {
                        prompt.push_str(&format!("- {}\n", pre));
                    }
                    prompt.push('\n');
                }
            }

            if let Some(postconditions) = &desc.postconditions {
                if !postconditions.is_empty() {
                    prompt.push_str("**Postconditions:**\n");
                    for post in postconditions {
                        prompt.push_str(&format!("- {}\n", post));
                    }
                    prompt.push('\n');
                }
            }

            if let Some(failures) = &desc.failure_modes {
                if !failures.is_empty() {
                    prompt.push_str("**Known Failure Modes:**\n");
                    for mode in failures {
                        prompt.push_str(&format!("- {}\n", mode));
                    }
                    prompt.push('\n');
                }
            }

            if let Some(expected_ms) = desc.expected_duration_ms {
                prompt.push_str(&format!("**Expected Duration:** {} ms\n\n", expected_ms));
            }
        }

        // Actual results
        prompt.push_str("## Actual Results\n\n");
        prompt.push_str(&format!(
            "**Actual Duration:** {} ms\n",
            self.actual_duration_ms
        ));

        if self.duration_exceeded {
            prompt.push_str("**⚠ Duration exceeded expected time**\n");
        }
        prompt.push('\n');

        if let Some(error) = &self.error {
            prompt.push_str(&format!("**Error:** {}\n\n", error));
        }

        // Screenshots
        if self.screenshot_before.is_some() || self.screenshot_after.is_some() {
            prompt.push_str("**Screenshots captured:** before and after transition\n\n");
        }

        // Analysis questions
        prompt.push_str("## Analysis Questions\n\n");
        prompt.push_str("1. Did the transition complete successfully?\n");
        prompt.push_str("2. Does the 'after' screenshot show the expected target state?\n");
        prompt.push_str("3. Was the transition duration acceptable?\n");
        prompt.push_str("4. Are there any signs of errors or unexpected behavior?\n");

        prompt
    }
}

/// Combined context for a full exploration run analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationAnalysisContext {
    /// Run identifier
    pub run_id: String,
    /// Strategy used
    pub strategy: String,
    /// States analyzed
    pub states: Vec<StateAnalysisContext>,
    /// Transitions analyzed
    pub transitions: Vec<TransitionAnalysisContext>,
    /// Overall summary statistics
    pub summary: ExplorationSummary,
}

/// Summary statistics for exploration analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationSummary {
    /// Total states visited
    pub states_visited: u32,
    /// States with discrepancies
    pub states_with_issues: u32,
    /// Total transitions executed
    pub transitions_executed: u32,
    /// Transitions with issues
    pub transitions_with_issues: u32,
    /// Overall pass/fail
    pub passed: bool,
}

impl ExplorationAnalysisContext {
    /// Generate a comprehensive AI analysis prompt
    pub fn to_analysis_prompt(&self) -> String {
        let mut prompt = String::new();

        // Header
        prompt.push_str(&format!("# State Exploration Analysis Report\n\n"));
        prompt.push_str(&format!("**Run ID:** {}\n", self.run_id));
        prompt.push_str(&format!("**Strategy:** {}\n", self.strategy));
        prompt.push_str(&format!(
            "**Overall Status:** {}\n\n",
            if self.summary.passed {
                "PASSED"
            } else {
                "FAILED"
            }
        ));

        // Summary
        prompt.push_str("## Summary\n\n");
        prompt.push_str(&format!(
            "- States: {}/{} with issues\n",
            self.summary.states_with_issues, self.summary.states_visited
        ));
        prompt.push_str(&format!(
            "- Transitions: {}/{} with issues\n\n",
            self.summary.transitions_with_issues, self.summary.transitions_executed
        ));

        // States with issues
        let states_with_issues: Vec<_> = self
            .states
            .iter()
            .filter(|s| s.has_discrepancies())
            .collect();

        if !states_with_issues.is_empty() {
            prompt.push_str("## States Requiring Attention\n\n");
            for state in states_with_issues {
                prompt.push_str(&format!("### {}\n\n", state.state_name));
                if !state.observations.missing_elements.is_empty() {
                    prompt.push_str(&format!(
                        "**Missing:** {}\n",
                        state.observations.missing_elements.join(", ")
                    ));
                }
                if !state.observations.unexpected_elements.is_empty() {
                    prompt.push_str(&format!(
                        "**Unexpected:** {}\n",
                        state.observations.unexpected_elements.join(", ")
                    ));
                }
                prompt.push_str(&format!(
                    "**Confidence:** {:.1}%\n\n",
                    state.observations.detection_confidence * 100.0
                ));
            }
        }

        // Transitions with issues
        let transitions_with_issues: Vec<_> = self
            .transitions
            .iter()
            .filter(|t| t.error.is_some() || t.duration_exceeded)
            .collect();

        if !transitions_with_issues.is_empty() {
            prompt.push_str("## Transitions Requiring Attention\n\n");
            for transition in transitions_with_issues {
                prompt.push_str(&format!(
                    "### {} → {}\n\n",
                    transition.from_state, transition.to_state
                ));
                if let Some(error) = &transition.error {
                    prompt.push_str(&format!("**Error:** {}\n", error));
                }
                if transition.duration_exceeded {
                    prompt.push_str(&format!(
                        "**Duration exceeded:** {} ms\n",
                        transition.actual_duration_ms
                    ));
                }
                prompt.push('\n');
            }
        }

        // Analysis questions
        prompt.push_str("## Overall Analysis Questions\n\n");
        prompt.push_str("1. What is the root cause of the identified issues?\n");
        prompt.push_str("2. Are any issues false positives (configuration vs. actual problems)?\n");
        prompt.push_str("3. What is the priority order for fixing these issues?\n");
        prompt.push_str("4. Are there patterns suggesting a common underlying problem?\n");
        prompt.push_str("5. What changes should be made to prevent these issues?\n");

        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_analysis_prompt_generation() {
        let context = StateAnalysisContext {
            state_id: "login".to_string(),
            state_name: "Login Page".to_string(),
            description: Some(StateDescription {
                summary: "The login page where users enter credentials".to_string(),
                expected_elements: Some(vec![
                    "Username field".to_string(),
                    "Password field".to_string(),
                    "Login button".to_string(),
                ]),
                unexpected_elements: Some(vec!["Error message".to_string()]),
                user_goal: Some("Enter credentials to access the system".to_string()),
                verification_prompt: None,
            }),
            observations: StateObservations {
                screenshot: Some(PathBuf::from("screenshot.png")),
                detected_elements: vec![
                    ElementCheck {
                        element: "Username field".to_string(),
                        found: true,
                        confidence: 0.95,
                        location: None,
                    },
                    ElementCheck {
                        element: "Password field".to_string(),
                        found: true,
                        confidence: 0.92,
                        location: None,
                    },
                ],
                expected_elements: vec![
                    "Username field".to_string(),
                    "Password field".to_string(),
                    "Login button".to_string(),
                ],
                missing_elements: vec!["Login button".to_string()],
                unexpected_elements: vec![],
                timing: TimingInfo {
                    detection_ms: 150,
                    screenshot_ms: 50,
                    total_ms: 200,
                },
                console_errors: vec![],
                detection_confidence: 0.85,
            },
            path_taken: vec!["initial".to_string()],
            previous_issues: vec![],
        };

        let prompt = context.to_analysis_prompt();

        assert!(prompt.contains("Login Page"));
        assert!(prompt.contains("Username field"));
        assert!(prompt.contains("Missing Elements"));
        assert!(prompt.contains("Login button"));
        assert!(prompt.contains("85.0%"));
    }

    #[test]
    fn test_has_discrepancies() {
        let mut context = StateAnalysisContext {
            state_id: "test".to_string(),
            state_name: "Test".to_string(),
            description: None,
            observations: StateObservations {
                screenshot: None,
                detected_elements: vec![],
                expected_elements: vec![],
                missing_elements: vec![],
                unexpected_elements: vec![],
                timing: TimingInfo {
                    detection_ms: 0,
                    screenshot_ms: 0,
                    total_ms: 0,
                },
                console_errors: vec![],
                detection_confidence: 0.9,
            },
            path_taken: vec![],
            previous_issues: vec![],
        };

        // No discrepancies
        assert!(!context.has_discrepancies());

        // Missing element
        context
            .observations
            .missing_elements
            .push("Button".to_string());
        assert!(context.has_discrepancies());

        // Reset and test low confidence
        context.observations.missing_elements.clear();
        context.observations.detection_confidence = 0.3;
        assert!(context.has_discrepancies());
    }
}
