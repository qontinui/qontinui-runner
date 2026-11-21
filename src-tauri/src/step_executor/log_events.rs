//! Runner log events a `StepExecutor` run reads back, and which log sources
//! a workflow's steps make relevant.
//!
//! `parse_action_events` / `parse_image_recognition_events` parse the
//! runner's `runner-actions.jsonl` / `runner-image-recognition.jsonl`
//! captures into `ExecutionResult`'s GUI-automation event lists.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::info;

use crate::step_executor::ExecutionStepConfig;

/// An action execution event from runner-actions.jsonl
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEvent {
    pub timestamp: String,
    pub event_type: String,
    pub step_name: Option<String>,
    pub success: Option<bool>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    /// Raw JSON for additional details
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// An image recognition event from runner-image-recognition.jsonl
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRecognitionEvent {
    pub timestamp: String,
    pub template_name: String,
    pub matched: bool,
    pub confidence: f64,
    pub threshold: f64,
    pub location: Option<ImageLocation>,
    pub screenshot_path: Option<String>,
}

/// Location of a matched/expected image
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageLocation {
    pub x: i32,
    pub y: i32,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

// ============================================================================
// Log Relevance Filtering
// ============================================================================

/// Which log sources are relevant for this workflow
#[derive(Debug, Clone, Default)]
pub struct RelevantLogSources {
    /// Include runner GUI automation logs (actions + image recognition)
    pub gui_automation: bool,
    /// Include Playwright test logs
    pub playwright: bool,
    /// User-defined application logs (always true if configured)
    pub application: bool,
}

impl RelevantLogSources {
    /// Determine which logs are relevant based on the step types in the workflow
    pub fn from_steps(steps: &[ExecutionStepConfig]) -> Self {
        let has_gui_automation = steps
            .iter()
            .any(|s| matches!(s.step_type.as_str(), "workflow" | "state" | "action"));
        let has_playwright = steps.iter().any(|s| s.step_type == "playwright");

        Self {
            gui_automation: has_gui_automation,
            playwright: has_playwright,
            application: true, // Always include user-defined sources
        }
    }

    /// Log what's relevant for debugging
    pub fn log_relevance(&self) {
        info!(
            "Log relevance: GUI automation={}, Playwright={}, Application={}",
            self.gui_automation, self.playwright, self.application
        );
    }
}

/// Parse runner-actions.jsonl content into ActionEvent list
pub fn parse_action_events(content: &str) -> Vec<ActionEvent> {
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<ActionEvent>(line).ok())
        .collect()
}

/// Parse runner-image-recognition.jsonl content into ImageRecognitionEvent list
pub fn parse_image_recognition_events(content: &str) -> Vec<ImageRecognitionEvent> {
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<ImageRecognitionEvent>(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relevant_log_sources() {
        // No steps = no GUI automation
        let empty: Vec<ExecutionStepConfig> = vec![];
        let relevant = RelevantLogSources::from_steps(&empty);
        assert!(!relevant.gui_automation);
        assert!(!relevant.playwright);
        assert!(relevant.application);
    }
}
