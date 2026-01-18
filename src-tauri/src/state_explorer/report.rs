//! Exploration report generation
//!
//! This module handles generating detailed reports of exploration results
//! that can be analyzed by AI or humans.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use super::types::{
    ExplorationResult, ExplorationStatus, StateExplorationResult, TransitionExplorationResult,
};

/// Type of discrepancy found during exploration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscrepancyType {
    /// Expected element was not found
    MissingElement,
    /// Unexpected element was found
    UnexpectedElement,
    /// State was not detected (visual pattern not matched)
    StateNotDetected,
    /// State detection confidence was low
    LowConfidence,
    /// Transition failed to execute
    TransitionFailed,
    /// Transition took longer than expected
    SlowTransition,
    /// Transition went to wrong state
    WrongTargetState,
    /// Screenshot mismatch
    VisualMismatch,
    /// Error during exploration
    ExplorationError,
}

/// A single discrepancy found during exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Discrepancy {
    /// Type of discrepancy
    pub discrepancy_type: DiscrepancyType,
    /// State ID where discrepancy was found
    pub state_id: String,
    /// State name for display
    pub state_name: String,
    /// Optional transition ID if this is a transition discrepancy
    pub transition_id: Option<String>,
    /// What was expected
    pub expected: String,
    /// What was actually found
    pub actual: String,
    /// Severity: "critical", "warning", "info"
    pub severity: String,
    /// Path to screenshot showing the issue
    pub screenshot_path: Option<String>,
    /// Additional context for AI analysis
    pub context: String,
    /// Suggested action
    pub suggested_action: Option<String>,
}

/// Report for a single state's exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateExplorationReport {
    /// State ID
    pub state_id: String,
    /// State name
    pub state_name: String,
    /// Whether exploration passed
    pub passed: bool,
    /// State description summary (if available)
    pub description_summary: Option<String>,
    /// Expected elements from description
    pub expected_elements: Vec<String>,
    /// Which expected elements were found
    pub found_elements: Vec<String>,
    /// Which expected elements were missing
    pub missing_elements: Vec<String>,
    /// Unexpected elements found
    pub unexpected_elements: Vec<String>,
    /// Detection confidence
    pub confidence: f64,
    /// Screenshot path
    pub screenshot_path: Option<String>,
    /// Time taken to explore
    pub duration_ms: u64,
    /// List of discrepancies
    pub discrepancies: Vec<Discrepancy>,
    /// AI notes
    pub ai_notes: Option<String>,
}

impl From<&StateExplorationResult> for StateExplorationReport {
    fn from(v: &StateExplorationResult) -> Self {
        let passed = matches!(v.status, ExplorationStatus::Passed);

        let found_elements: Vec<String> = v
            .expected_elements_found
            .iter()
            .filter(|e| e.found)
            .map(|e| e.element.clone())
            .collect();

        let missing_elements: Vec<String> = v
            .expected_elements_found
            .iter()
            .filter(|e| !e.found)
            .map(|e| e.element.clone())
            .collect();

        let unexpected_elements: Vec<String> = v
            .unexpected_elements_found
            .iter()
            .filter(|e| e.found)
            .map(|e| e.element.clone())
            .collect();

        let mut discrepancies = Vec::new();

        // Create discrepancies for missing elements
        for element in &missing_elements {
            discrepancies.push(Discrepancy {
                discrepancy_type: DiscrepancyType::MissingElement,
                state_id: v.state_id.clone(),
                state_name: v.state_name.clone(),
                transition_id: None,
                expected: element.clone(),
                actual: "Not found".to_string(),
                severity: "critical".to_string(),
                screenshot_path: v.screenshot_path.clone(),
                context: format!("Expected element '{}' was not visible in state", element),
                suggested_action: Some(format!("Check if '{}' is rendered correctly", element)),
            });
        }

        // Create discrepancies for unexpected elements
        for element in &unexpected_elements {
            discrepancies.push(Discrepancy {
                discrepancy_type: DiscrepancyType::UnexpectedElement,
                state_id: v.state_id.clone(),
                state_name: v.state_name.clone(),
                transition_id: None,
                expected: "Not present".to_string(),
                actual: element.clone(),
                severity: "warning".to_string(),
                screenshot_path: v.screenshot_path.clone(),
                context: format!("Unexpected element '{}' was found in state", element),
                suggested_action: Some("Verify this element should not appear here".to_string()),
            });
        }

        // Add low confidence discrepancy
        if v.detection_confidence < 0.8 {
            discrepancies.push(Discrepancy {
                discrepancy_type: DiscrepancyType::LowConfidence,
                state_id: v.state_id.clone(),
                state_name: v.state_name.clone(),
                transition_id: None,
                expected: "Confidence >= 0.8".to_string(),
                actual: format!("{:.2}", v.detection_confidence),
                severity: "warning".to_string(),
                screenshot_path: v.screenshot_path.clone(),
                context: "State detection confidence is below threshold".to_string(),
                suggested_action: Some(
                    "Review state images or update detection patterns".to_string(),
                ),
            });
        }

        Self {
            state_id: v.state_id.clone(),
            state_name: v.state_name.clone(),
            passed,
            description_summary: None,
            expected_elements: v
                .expected_elements_found
                .iter()
                .map(|e| e.element.clone())
                .collect(),
            found_elements,
            missing_elements,
            unexpected_elements,
            confidence: v.detection_confidence,
            screenshot_path: v.screenshot_path.clone(),
            duration_ms: v.duration_ms,
            discrepancies,
            ai_notes: v.ai_notes.clone(),
        }
    }
}

/// Report for a transition's exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionExplorationReport {
    /// Transition ID
    pub transition_id: String,
    /// Source state
    pub from_state: String,
    /// Target state
    pub to_state: String,
    /// Whether exploration passed
    pub passed: bool,
    /// Screenshot before transition
    pub screenshot_before: Option<String>,
    /// Screenshot after transition
    pub screenshot_after: Option<String>,
    /// Actual duration in ms
    pub duration_ms: u64,
    /// Expected duration from description
    pub expected_duration_ms: Option<u64>,
    /// Whether transition was slower than expected
    pub was_slow: bool,
    /// List of discrepancies
    pub discrepancies: Vec<Discrepancy>,
    /// Notes
    pub notes: Option<String>,
}

impl From<&TransitionExplorationResult> for TransitionExplorationReport {
    fn from(v: &TransitionExplorationResult) -> Self {
        let passed = matches!(v.status, ExplorationStatus::Passed);

        let mut discrepancies = Vec::new();

        if v.duration_exceeded {
            discrepancies.push(Discrepancy {
                discrepancy_type: DiscrepancyType::SlowTransition,
                state_id: v.from_state_id.clone(),
                state_name: v.from_state_id.clone(),
                transition_id: Some(v.transition_id.clone()),
                expected: format!("{}ms", v.expected_duration_ms.unwrap_or(0)),
                actual: format!("{}ms", v.duration_ms),
                severity: "warning".to_string(),
                screenshot_path: v.screenshot_after.clone(),
                context: "Transition took longer than expected".to_string(),
                suggested_action: Some("Investigate performance issues".to_string()),
            });
        }

        if let Some(error) = &v.error {
            discrepancies.push(Discrepancy {
                discrepancy_type: DiscrepancyType::TransitionFailed,
                state_id: v.from_state_id.clone(),
                state_name: v.from_state_id.clone(),
                transition_id: Some(v.transition_id.clone()),
                expected: format!("Navigate to {}", v.to_state_id),
                actual: error.clone(),
                severity: "critical".to_string(),
                screenshot_path: v.screenshot_after.clone(),
                context: "Transition failed to complete".to_string(),
                suggested_action: Some("Fix the transition action or target state".to_string()),
            });
        }

        Self {
            transition_id: v.transition_id.clone(),
            from_state: v.from_state_id.clone(),
            to_state: v.to_state_id.clone(),
            passed,
            screenshot_before: v.screenshot_before.clone(),
            screenshot_after: v.screenshot_after.clone(),
            duration_ms: v.duration_ms,
            expected_duration_ms: v.expected_duration_ms,
            was_slow: v.duration_exceeded,
            discrepancies,
            notes: v.notes.clone(),
        }
    }
}

/// Complete exploration report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationReport {
    /// Report metadata
    pub run_id: String,
    pub config_path: String,
    pub config_name: String,
    pub strategy: String,
    pub started_at: String,
    pub completed_at: String,
    pub total_duration_ms: u64,

    /// Summary statistics
    pub summary: ReportSummary,

    /// Detailed state reports
    pub state_reports: Vec<StateExplorationReport>,

    /// Detailed transition reports
    pub transition_reports: Vec<TransitionExplorationReport>,

    /// All discrepancies found
    pub all_discrepancies: Vec<Discrepancy>,

    /// Discrepancies grouped by severity
    pub discrepancies_by_severity: HashMap<String, Vec<Discrepancy>>,

    /// AI analysis prompt (pre-built prompt for AI to analyze results)
    pub ai_analysis_prompt: String,
}

/// Summary statistics for the report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSummary {
    pub total_states: u32,
    pub states_explored: u32,
    pub states_passed: u32,
    pub states_failed: u32,
    pub states_skipped: u32,

    pub total_transitions: u32,
    pub transitions_explored: u32,
    pub transitions_passed: u32,
    pub transitions_failed: u32,

    pub total_discrepancies: u32,
    pub critical_discrepancies: u32,
    pub warning_discrepancies: u32,
    pub info_discrepancies: u32,

    pub overall_pass_rate: f64,
    pub overall_status: String,
}

impl ExplorationReport {
    /// Create a report from exploration result
    pub fn from_result(result: &ExplorationResult, config_name: String) -> Self {
        let state_reports: Vec<StateExplorationReport> = result
            .state_explorations
            .iter()
            .map(StateExplorationReport::from)
            .collect();

        let transition_reports: Vec<TransitionExplorationReport> = result
            .transition_explorations
            .iter()
            .map(TransitionExplorationReport::from)
            .collect();

        // Collect all discrepancies
        let mut all_discrepancies: Vec<Discrepancy> = Vec::new();
        for sr in &state_reports {
            all_discrepancies.extend(sr.discrepancies.clone());
        }
        for tr in &transition_reports {
            all_discrepancies.extend(tr.discrepancies.clone());
        }

        // Group by severity
        let mut by_severity: HashMap<String, Vec<Discrepancy>> = HashMap::new();
        for d in &all_discrepancies {
            by_severity
                .entry(d.severity.clone())
                .or_default()
                .push(d.clone());
        }

        let critical_count = by_severity.get("critical").map(|v| v.len()).unwrap_or(0) as u32;
        let warning_count = by_severity.get("warning").map(|v| v.len()).unwrap_or(0) as u32;
        let info_count = by_severity.get("info").map(|v| v.len()).unwrap_or(0) as u32;

        let pass_rate = if result.states_visited > 0 {
            result.states_passed as f64 / result.states_visited as f64
        } else {
            0.0
        };

        let summary = ReportSummary {
            total_states: result.total_states,
            states_explored: result.states_visited,
            states_passed: result.states_passed,
            states_failed: result.states_failed,
            states_skipped: result.states_skipped,
            total_transitions: result.total_transitions,
            transitions_explored: result.transitions_explored,
            transitions_passed: result.transitions_passed,
            transitions_failed: result.transitions_failed,
            total_discrepancies: all_discrepancies.len() as u32,
            critical_discrepancies: critical_count,
            warning_discrepancies: warning_count,
            info_discrepancies: info_count,
            overall_pass_rate: pass_rate,
            overall_status: format!("{:?}", result.overall_status),
        };

        // Build AI analysis prompt
        let ai_prompt = Self::build_ai_analysis_prompt(&summary, &all_discrepancies);

        Self {
            run_id: result.run_id.clone(),
            config_path: result.config_path.clone(),
            config_name,
            strategy: result.strategy.clone(),
            started_at: result.started_at.clone(),
            completed_at: result.completed_at.clone().unwrap_or_default(),
            total_duration_ms: result.total_duration_ms,
            summary,
            state_reports,
            transition_reports,
            all_discrepancies,
            discrepancies_by_severity: by_severity,
            ai_analysis_prompt: ai_prompt,
        }
    }

    /// Build a prompt for AI analysis of the exploration results
    fn build_ai_analysis_prompt(summary: &ReportSummary, discrepancies: &[Discrepancy]) -> String {
        let mut prompt = String::new();

        prompt.push_str("# Exploration Results Analysis\n\n");
        prompt.push_str(
            "Please analyze the following exploration results and provide recommendations.\n\n",
        );

        prompt.push_str("## Summary\n");
        prompt.push_str(&format!(
            "- States explored: {}/{} ({:.1}% pass rate)\n",
            summary.states_passed,
            summary.states_explored,
            summary.overall_pass_rate * 100.0
        ));
        prompt.push_str(&format!(
            "- Transitions explored: {}/{} ({} passed, {} failed)\n",
            summary.transitions_explored,
            summary.total_transitions,
            summary.transitions_passed,
            summary.transitions_failed
        ));
        prompt.push_str(&format!(
            "- Discrepancies found: {} ({} critical, {} warnings)\n\n",
            summary.total_discrepancies,
            summary.critical_discrepancies,
            summary.warning_discrepancies
        ));

        if !discrepancies.is_empty() {
            prompt.push_str("## Discrepancies to Analyze\n\n");

            for (i, d) in discrepancies.iter().enumerate() {
                prompt.push_str(&format!(
                    "### {}. {} ({})\n",
                    i + 1,
                    d.state_name,
                    d.severity
                ));
                prompt.push_str(&format!("**Type:** {:?}\n", d.discrepancy_type));
                prompt.push_str(&format!("**Expected:** {}\n", d.expected));
                prompt.push_str(&format!("**Actual:** {}\n", d.actual));
                prompt.push_str(&format!("**Context:** {}\n", d.context));
                if let Some(action) = &d.suggested_action {
                    prompt.push_str(&format!("**Suggested Action:** {}\n", action));
                }
                if let Some(screenshot) = &d.screenshot_path {
                    prompt.push_str(&format!("**Screenshot:** {}\n", screenshot));
                }
                prompt.push('\n');
            }

            prompt.push_str("## Questions for Analysis\n\n");
            prompt.push_str("1. What are the root causes of these discrepancies?\n");
            prompt.push_str("2. Are any of these false positives (expected behavior changes)?\n");
            prompt.push_str("3. What is the priority order for fixing these issues?\n");
            prompt.push_str("4. Are there any patterns that suggest systemic problems?\n");
            prompt.push_str("5. What additional tests should be added to prevent regression?\n");
        } else {
            prompt.push_str("## Status\n\n");
            prompt.push_str("All exploration checks passed. No discrepancies found.\n");
        }

        prompt
    }

    /// Save report to file (JSON format)
    pub fn save_json(&self, output_dir: &PathBuf) -> Result<PathBuf, String> {
        fs::create_dir_all(output_dir)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;

        let filename = format!("exploration-report-{}.json", self.run_id);
        let path = output_dir.join(&filename);

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize: {}", e))?;

        fs::write(&path, json).map_err(|e| format!("Failed to write report: {}", e))?;

        Ok(path)
    }

    /// Save report as markdown (for human reading)
    pub fn save_markdown(&self, output_dir: &PathBuf) -> Result<PathBuf, String> {
        fs::create_dir_all(output_dir)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;

        let filename = format!("exploration-report-{}.md", self.run_id);
        let path = output_dir.join(&filename);

        let mut md = String::new();

        // Header
        md.push_str(&format!("# Exploration Report: {}\n\n", self.config_name));
        md.push_str(&format!("**Run ID:** {}\n", self.run_id));
        md.push_str(&format!("**Strategy:** {}\n", self.strategy));
        md.push_str(&format!("**Started:** {}\n", self.started_at));
        md.push_str(&format!("**Duration:** {}ms\n\n", self.total_duration_ms));

        // Summary
        md.push_str("## Summary\n\n");
        md.push_str("| Metric | Value |\n|--------|-------|\n");
        md.push_str(&format!(
            "| States Explored | {}/{} |\n",
            self.summary.states_explored, self.summary.total_states
        ));
        md.push_str(&format!(
            "| States Passed | {} |\n",
            self.summary.states_passed
        ));
        md.push_str(&format!(
            "| States Failed | {} |\n",
            self.summary.states_failed
        ));
        md.push_str(&format!(
            "| Pass Rate | {:.1}% |\n",
            self.summary.overall_pass_rate * 100.0
        ));
        md.push_str(&format!(
            "| Critical Issues | {} |\n",
            self.summary.critical_discrepancies
        ));
        md.push_str(&format!(
            "| Warnings | {} |\n\n",
            self.summary.warning_discrepancies
        ));

        // Discrepancies
        if !self.all_discrepancies.is_empty() {
            md.push_str("## Issues Found\n\n");

            for d in &self.all_discrepancies {
                let emoji = match d.severity.as_str() {
                    "critical" => "!!!",
                    "warning" => "!",
                    _ => "",
                };
                md.push_str(&format!(
                    "### {} [{:?}] {} - {}\n",
                    emoji, d.discrepancy_type, d.state_name, d.severity
                ));
                md.push_str(&format!("- **Expected:** {}\n", d.expected));
                md.push_str(&format!("- **Actual:** {}\n", d.actual));
                md.push_str(&format!("- **Context:** {}\n", d.context));
                if let Some(action) = &d.suggested_action {
                    md.push_str(&format!("- **Suggested:** {}\n", action));
                }
                md.push('\n');
            }
        }

        // State details
        md.push_str("## State Exploration Details\n\n");
        for sr in &self.state_reports {
            let status = if sr.passed { "PASS" } else { "FAIL" };
            md.push_str(&format!(
                "### {} - {} (confidence: {:.2})\n",
                sr.state_name, status, sr.confidence
            ));
            if !sr.found_elements.is_empty() {
                md.push_str(&format!("- Found: {}\n", sr.found_elements.join(", ")));
            }
            if !sr.missing_elements.is_empty() {
                md.push_str(&format!("- Missing: {}\n", sr.missing_elements.join(", ")));
            }
            md.push('\n');
        }

        fs::write(&path, md).map_err(|e| format!("Failed to write report: {}", e))?;

        Ok(path)
    }
}
