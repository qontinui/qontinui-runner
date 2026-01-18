//! Baseline system for diff-based exploration
//!
//! This module enables regression detection by:
//! 1. Saving exploration runs as baselines
//! 2. Comparing new runs against baselines
//! 3. Detecting changes in state structure, timing, and appearance
//!
//! # Usage
//!
//! ```ignore
//! // Save a baseline
//! let baseline = ExplorationBaseline::from_result(&result, "v1.0 release");
//! baseline.save(&output_dir)?;
//!
//! // Compare against baseline
//! let diff = baseline.compare(&new_result);
//! if diff.has_regressions() {
//!     println!("Regressions found: {:?}", diff.regressions);
//! }
//! ```

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A saved baseline from a successful exploration run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationBaseline {
    /// Unique baseline ID
    pub id: String,
    /// Human-readable name/description
    pub name: String,
    /// Config file path this baseline is for
    pub config_path: String,
    /// Hash of the config file (to detect config changes)
    pub config_hash: String,
    /// When the baseline was created
    pub created_at: String,
    /// Strategy used for the original exploration
    pub strategy: String,
    /// Signatures for each explored state
    pub state_signatures: HashMap<String, StateSignature>,
    /// Signatures for each explored transition
    pub transition_signatures: HashMap<String, TransitionSignature>,
    /// Overall timing baseline
    pub total_duration_ms: u64,
    /// Number of states in the baseline
    pub state_count: usize,
    /// Number of transitions in the baseline
    pub transition_count: usize,
    /// Tags for organizing baselines
    pub tags: Vec<String>,
}

/// Signature of a state for comparison
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSignature {
    /// State ID
    pub state_id: String,
    /// State name
    pub state_name: String,
    /// Hash of detected elements (sorted, joined)
    pub element_hash: String,
    /// List of elements found
    pub elements_found: Vec<String>,
    /// Perceptual hash of screenshot (if available)
    pub screenshot_hash: Option<String>,
    /// Screenshot file path (relative to baseline)
    pub screenshot_path: Option<String>,
    /// Average detection confidence
    pub detection_confidence: f64,
    /// Exploration duration for this state
    pub duration_ms: u64,
    /// Whether exploration passed
    pub passed: bool,
}

/// Signature of a transition for comparison
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionSignature {
    /// Transition ID
    pub transition_id: String,
    /// Source state
    pub from_state_id: String,
    /// Target state
    pub to_state_id: String,
    /// Duration of transition
    pub duration_ms: u64,
    /// Expected duration (if specified)
    pub expected_duration_ms: Option<u64>,
    /// Whether transition passed
    pub passed: bool,
}

/// Difference between two exploration runs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineDiff {
    /// Baseline being compared against
    pub baseline_id: String,
    /// Run being compared
    pub run_id: String,
    /// When comparison was made
    pub compared_at: String,
    /// States that are new (not in baseline)
    pub new_states: Vec<String>,
    /// States that are missing (in baseline but not in run)
    pub missing_states: Vec<String>,
    /// States with differences
    pub changed_states: Vec<StateDiff>,
    /// Transitions that are new
    pub new_transitions: Vec<String>,
    /// Transitions that are missing
    pub missing_transitions: Vec<String>,
    /// Transitions with differences
    pub changed_transitions: Vec<TransitionDiff>,
    /// Overall timing difference (percentage)
    pub timing_diff_percent: f64,
    /// Summary of changes
    pub summary: String,
}

/// Difference in a single state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateDiff {
    /// State ID
    pub state_id: String,
    /// State name
    pub state_name: String,
    /// Type of change
    pub change_type: ChangeType,
    /// Elements that were added
    pub elements_added: Vec<String>,
    /// Elements that were removed
    pub elements_removed: Vec<String>,
    /// Whether screenshot changed significantly
    pub screenshot_changed: bool,
    /// Confidence difference
    pub confidence_diff: f64,
    /// Duration difference (ms)
    pub duration_diff_ms: i64,
    /// Pass status changed
    pub status_changed: bool,
    /// Baseline passed, current failed
    pub is_regression: bool,
}

/// Difference in a single transition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionDiff {
    /// Transition ID
    pub transition_id: String,
    /// Duration difference (ms)
    pub duration_diff_ms: i64,
    /// Duration difference percentage
    pub duration_diff_percent: f64,
    /// Status changed
    pub status_changed: bool,
    /// Baseline passed, current failed
    pub is_regression: bool,
}

/// Type of change detected
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// No significant change
    None,
    /// Elements changed
    ElementsChanged,
    /// Screenshot changed significantly
    ScreenshotChanged,
    /// Timing changed significantly
    TimingChanged,
    /// Status changed (pass/fail)
    StatusChanged,
    /// Multiple types of changes
    Multiple,
}

impl ExplorationBaseline {
    /// Create a baseline from an exploration result
    pub fn from_result(result: &super::types::ExplorationResult, name: impl Into<String>) -> Self {
        let mut state_signatures = HashMap::new();
        let mut transition_signatures = HashMap::new();

        // Build state signatures
        for state_result in &result.state_explorations {
            let elements: Vec<String> = state_result
                .expected_elements_found
                .iter()
                .filter(|e| e.found)
                .map(|e| e.element.clone())
                .collect();

            let element_hash = compute_element_hash(&elements);

            state_signatures.insert(
                state_result.state_id.clone(),
                StateSignature {
                    state_id: state_result.state_id.clone(),
                    state_name: state_result.state_name.clone(),
                    element_hash,
                    elements_found: elements,
                    screenshot_hash: state_result
                        .screenshot_path
                        .as_ref()
                        .map(|p| compute_file_hash(p)),
                    screenshot_path: state_result.screenshot_path.clone(),
                    detection_confidence: state_result.detection_confidence,
                    duration_ms: state_result.duration_ms,
                    passed: state_result.status == super::types::ExplorationStatus::Passed,
                },
            );
        }

        // Build transition signatures
        for trans_result in &result.transition_explorations {
            transition_signatures.insert(
                trans_result.transition_id.clone(),
                TransitionSignature {
                    transition_id: trans_result.transition_id.clone(),
                    from_state_id: trans_result.from_state_id.clone(),
                    to_state_id: trans_result.to_state_id.clone(),
                    duration_ms: trans_result.duration_ms,
                    expected_duration_ms: trans_result.expected_duration_ms,
                    passed: trans_result.status == super::types::ExplorationStatus::Passed,
                },
            );
        }

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            config_path: result.config_path.clone(),
            config_hash: compute_config_hash(&result.config_path),
            created_at: Utc::now().to_rfc3339(),
            strategy: result.strategy.clone(),
            state_signatures,
            transition_signatures,
            total_duration_ms: result.total_duration_ms,
            state_count: result.state_explorations.len(),
            transition_count: result.transition_explorations.len(),
            tags: Vec::new(),
        }
    }

    /// Add tags to the baseline
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Compare this baseline against a new exploration result
    pub fn compare(&self, result: &super::types::ExplorationResult) -> BaselineDiff {
        let mut new_states = Vec::new();
        let mut missing_states = Vec::new();
        let mut changed_states = Vec::new();
        let mut new_transitions = Vec::new();
        let mut missing_transitions = Vec::new();
        let mut changed_transitions = Vec::new();

        // Find new and changed states
        for state_result in &result.state_explorations {
            if let Some(baseline_sig) = self.state_signatures.get(&state_result.state_id) {
                // State exists in baseline - check for changes
                let diff = self.compare_state(baseline_sig, state_result);
                if diff.change_type != ChangeType::None {
                    changed_states.push(diff);
                }
            } else {
                // New state not in baseline
                new_states.push(state_result.state_id.clone());
            }
        }

        // Find missing states
        let current_state_ids: std::collections::HashSet<_> = result
            .state_explorations
            .iter()
            .map(|s| s.state_id.clone())
            .collect();

        for baseline_state_id in self.state_signatures.keys() {
            if !current_state_ids.contains(baseline_state_id) {
                missing_states.push(baseline_state_id.clone());
            }
        }

        // Find new and changed transitions
        for trans_result in &result.transition_explorations {
            if let Some(baseline_sig) = self.transition_signatures.get(&trans_result.transition_id)
            {
                let diff = self.compare_transition(baseline_sig, trans_result);
                if diff.duration_diff_percent.abs() > 20.0 || diff.status_changed {
                    changed_transitions.push(diff);
                }
            } else {
                new_transitions.push(trans_result.transition_id.clone());
            }
        }

        // Find missing transitions
        let current_trans_ids: std::collections::HashSet<_> = result
            .transition_explorations
            .iter()
            .map(|t| t.transition_id.clone())
            .collect();

        for baseline_trans_id in self.transition_signatures.keys() {
            if !current_trans_ids.contains(baseline_trans_id) {
                missing_transitions.push(baseline_trans_id.clone());
            }
        }

        // Calculate overall timing difference
        let timing_diff_percent = if self.total_duration_ms > 0 {
            ((result.total_duration_ms as f64 - self.total_duration_ms as f64)
                / self.total_duration_ms as f64)
                * 100.0
        } else {
            0.0
        };

        // Build summary
        let regression_count = changed_states.iter().filter(|d| d.is_regression).count()
            + changed_transitions
                .iter()
                .filter(|d| d.is_regression)
                .count();

        let summary = format!(
            "{} new states, {} missing states, {} changed states, {} regressions, {:.1}% timing change",
            new_states.len(),
            missing_states.len(),
            changed_states.len(),
            regression_count,
            timing_diff_percent
        );

        BaselineDiff {
            baseline_id: self.id.clone(),
            run_id: result.run_id.clone(),
            compared_at: Utc::now().to_rfc3339(),
            new_states,
            missing_states,
            changed_states,
            new_transitions,
            missing_transitions,
            changed_transitions,
            timing_diff_percent,
            summary,
        }
    }

    /// Compare a single state against baseline
    fn compare_state(
        &self,
        baseline: &StateSignature,
        current: &super::types::StateExplorationResult,
    ) -> StateDiff {
        let current_elements: Vec<String> = current
            .expected_elements_found
            .iter()
            .filter(|e| e.found)
            .map(|e| e.element.clone())
            .collect();

        let current_hash = compute_element_hash(&current_elements);
        let elements_changed = current_hash != baseline.element_hash;

        // Find added/removed elements
        let baseline_set: std::collections::HashSet<_> =
            baseline.elements_found.iter().cloned().collect();
        let current_set: std::collections::HashSet<_> = current_elements.iter().cloned().collect();

        let elements_added: Vec<_> = current_set.difference(&baseline_set).cloned().collect();
        let elements_removed: Vec<_> = baseline_set.difference(&current_set).cloned().collect();

        // Check screenshot change (if hashes available)
        let screenshot_changed = match (&baseline.screenshot_hash, &current.screenshot_path) {
            (Some(baseline_hash), Some(current_path)) => {
                let current_hash = compute_file_hash(current_path);
                current_hash != *baseline_hash
            }
            _ => false,
        };

        let confidence_diff = current.detection_confidence - baseline.detection_confidence;
        let duration_diff_ms = current.duration_ms as i64 - baseline.duration_ms as i64;

        let current_passed = current.status == super::types::ExplorationStatus::Passed;
        let status_changed = current_passed != baseline.passed;
        let is_regression = baseline.passed && !current_passed;

        // Determine change type
        let change_type = if is_regression || status_changed {
            ChangeType::StatusChanged
        } else if elements_changed && screenshot_changed {
            ChangeType::Multiple
        } else if elements_changed {
            ChangeType::ElementsChanged
        } else if screenshot_changed {
            ChangeType::ScreenshotChanged
        } else if duration_diff_ms.abs() > (baseline.duration_ms as i64 / 2) {
            ChangeType::TimingChanged
        } else {
            ChangeType::None
        };

        StateDiff {
            state_id: baseline.state_id.clone(),
            state_name: baseline.state_name.clone(),
            change_type,
            elements_added,
            elements_removed,
            screenshot_changed,
            confidence_diff,
            duration_diff_ms,
            status_changed,
            is_regression,
        }
    }

    /// Compare a single transition against baseline
    fn compare_transition(
        &self,
        baseline: &TransitionSignature,
        current: &super::types::TransitionExplorationResult,
    ) -> TransitionDiff {
        let duration_diff_ms = current.duration_ms as i64 - baseline.duration_ms as i64;
        let duration_diff_percent = if baseline.duration_ms > 0 {
            (duration_diff_ms as f64 / baseline.duration_ms as f64) * 100.0
        } else {
            0.0
        };

        let current_passed = current.status == super::types::ExplorationStatus::Passed;
        let status_changed = current_passed != baseline.passed;
        let is_regression = baseline.passed && !current_passed;

        TransitionDiff {
            transition_id: baseline.transition_id.clone(),
            duration_diff_ms,
            duration_diff_percent,
            status_changed,
            is_regression,
        }
    }

    /// Save baseline to disk
    pub fn save(&self, output_dir: &PathBuf) -> Result<PathBuf, String> {
        let baseline_dir = output_dir.join("baselines");
        std::fs::create_dir_all(&baseline_dir)
            .map_err(|e| format!("Failed to create baselines directory: {}", e))?;

        let filename = format!("baseline-{}.json", self.id);
        let path = baseline_dir.join(&filename);

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize baseline: {}", e))?;

        std::fs::write(&path, json).map_err(|e| format!("Failed to write baseline file: {}", e))?;

        Ok(path)
    }

    /// Load baseline from disk
    pub fn load(path: &PathBuf) -> Result<Self, String> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read baseline file: {}", e))?;

        serde_json::from_str(&json).map_err(|e| format!("Failed to parse baseline: {}", e))
    }
}

impl BaselineDiff {
    /// Check if there are any regressions
    pub fn has_regressions(&self) -> bool {
        self.changed_states.iter().any(|d| d.is_regression)
            || self.changed_transitions.iter().any(|d| d.is_regression)
    }

    /// Get count of regressions
    pub fn regression_count(&self) -> usize {
        self.changed_states
            .iter()
            .filter(|d| d.is_regression)
            .count()
            + self
                .changed_transitions
                .iter()
                .filter(|d| d.is_regression)
                .count()
    }

    /// Check if there are any changes
    pub fn has_changes(&self) -> bool {
        !self.new_states.is_empty()
            || !self.missing_states.is_empty()
            || !self.changed_states.is_empty()
            || !self.new_transitions.is_empty()
            || !self.missing_transitions.is_empty()
            || !self.changed_transitions.is_empty()
    }

    /// Generate markdown report of differences
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str("# Baseline Comparison Report\n\n");
        md.push_str(&format!("**Baseline ID:** {}\n", self.baseline_id));
        md.push_str(&format!("**Run ID:** {}\n", self.run_id));
        md.push_str(&format!("**Compared At:** {}\n\n", self.compared_at));
        md.push_str(&format!("## Summary\n\n{}\n\n", self.summary));

        if self.has_regressions() {
            md.push_str("## ⚠️ Regressions Detected\n\n");
            for state in self.changed_states.iter().filter(|d| d.is_regression) {
                md.push_str(&format!(
                    "- **{}** ({}): State that previously passed is now failing\n",
                    state.state_name, state.state_id
                ));
            }
            for trans in self.changed_transitions.iter().filter(|d| d.is_regression) {
                md.push_str(&format!(
                    "- **Transition {}**: Previously passed, now failing\n",
                    trans.transition_id
                ));
            }
            md.push('\n');
        }

        if !self.new_states.is_empty() {
            md.push_str("## New States\n\n");
            for state in &self.new_states {
                md.push_str(&format!("- {}\n", state));
            }
            md.push('\n');
        }

        if !self.missing_states.is_empty() {
            md.push_str("## Missing States\n\n");
            for state in &self.missing_states {
                md.push_str(&format!("- {}\n", state));
            }
            md.push('\n');
        }

        if !self.changed_states.is_empty() {
            md.push_str("## Changed States\n\n");
            for diff in &self.changed_states {
                md.push_str(&format!("### {} ({})\n\n", diff.state_name, diff.state_id));
                md.push_str(&format!("- **Change Type:** {:?}\n", diff.change_type));
                if !diff.elements_added.is_empty() {
                    md.push_str(&format!(
                        "- **Elements Added:** {:?}\n",
                        diff.elements_added
                    ));
                }
                if !diff.elements_removed.is_empty() {
                    md.push_str(&format!(
                        "- **Elements Removed:** {:?}\n",
                        diff.elements_removed
                    ));
                }
                if diff.screenshot_changed {
                    md.push_str("- **Screenshot Changed:** Yes\n");
                }
                md.push_str(&format!(
                    "- **Duration Change:** {}ms\n",
                    diff.duration_diff_ms
                ));
                md.push('\n');
            }
        }

        if self.timing_diff_percent.abs() > 10.0 {
            md.push_str("## Timing Changes\n\n");
            md.push_str(&format!(
                "Overall timing changed by {:.1}%\n\n",
                self.timing_diff_percent
            ));
        }

        md
    }

    /// Save diff report to disk
    pub fn save(&self, output_dir: &PathBuf) -> Result<PathBuf, String> {
        let diff_dir = output_dir.join("diffs");
        std::fs::create_dir_all(&diff_dir)
            .map_err(|e| format!("Failed to create diffs directory: {}", e))?;

        let filename = format!("diff-{}-vs-{}.json", self.baseline_id, self.run_id);
        let path = diff_dir.join(&filename);

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize diff: {}", e))?;

        std::fs::write(&path, json).map_err(|e| format!("Failed to write diff file: {}", e))?;

        // Also save markdown version
        let md_path = diff_dir.join(format!("diff-{}-vs-{}.md", self.baseline_id, self.run_id));
        std::fs::write(&md_path, self.to_markdown())
            .map_err(|e| format!("Failed to write diff markdown: {}", e))?;

        Ok(path)
    }
}

/// Manager for baseline operations
pub struct BaselineManager {
    /// Directory where baselines are stored
    baselines_dir: PathBuf,
    /// Cached baselines
    baselines: HashMap<String, ExplorationBaseline>,
}

impl BaselineManager {
    /// Create a new baseline manager
    pub fn new(output_dir: PathBuf) -> Self {
        let baselines_dir = output_dir.join("baselines");
        Self {
            baselines_dir,
            baselines: HashMap::new(),
        }
    }

    /// Load all baselines from disk
    pub fn load_all(&mut self) -> Result<(), String> {
        if !self.baselines_dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(&self.baselines_dir)
            .map_err(|e| format!("Failed to read baselines directory: {}", e))?
        {
            let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
            let path = entry.path();

            if path.extension().map(|e| e == "json").unwrap_or(false) {
                match ExplorationBaseline::load(&path) {
                    Ok(baseline) => {
                        self.baselines.insert(baseline.id.clone(), baseline);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load baseline {:?}: {}", path, e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Get baseline by ID
    pub fn get(&self, id: &str) -> Option<&ExplorationBaseline> {
        self.baselines.get(id)
    }

    /// Get latest baseline for a config path
    pub fn get_latest_for_config(&self, config_path: &str) -> Option<&ExplorationBaseline> {
        self.baselines
            .values()
            .filter(|b| b.config_path == config_path)
            .max_by_key(|b| &b.created_at)
    }

    /// List all baselines
    pub fn list(&self) -> Vec<&ExplorationBaseline> {
        self.baselines.values().collect()
    }

    /// List baselines by tag
    pub fn list_by_tag(&self, tag: &str) -> Vec<&ExplorationBaseline> {
        self.baselines
            .values()
            .filter(|b| b.tags.contains(&tag.to_string()))
            .collect()
    }

    /// Save a new baseline
    pub fn save(&mut self, baseline: ExplorationBaseline) -> Result<PathBuf, String> {
        let output_dir = self
            .baselines_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.baselines_dir.clone());
        let path = baseline.save(&output_dir)?;
        self.baselines.insert(baseline.id.clone(), baseline);
        Ok(path)
    }

    /// Delete a baseline
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        if let Some(baseline) = self.baselines.remove(id) {
            let path = self
                .baselines_dir
                .join(format!("baseline-{}.json", baseline.id));
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete baseline file: {}", e))?;
            }
        }
        Ok(())
    }
}

/// Compute a simple hash of element list for comparison
fn compute_element_hash(elements: &[String]) -> String {
    let mut sorted = elements.to_vec();
    sorted.sort();
    let joined = sorted.join("|");

    // Simple hash using std hash
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    joined.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Compute hash of a file (for screenshot comparison)
fn compute_file_hash(path: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // For now, use file size + modification time as a simple hash
    // In production, could use actual image perceptual hashing
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let mut hasher = DefaultHasher::new();
            metadata.len().hash(&mut hasher);
            if let Ok(modified) = metadata.modified() {
                modified.hash(&mut hasher);
            }
            format!("{:016x}", hasher.finish())
        }
        Err(_) => "unknown".to_string(),
    }
}

/// Compute hash of config file
fn compute_config_hash(config_path: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    match std::fs::read_to_string(config_path) {
        Ok(content) => {
            let mut hasher = DefaultHasher::new();
            content.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        }
        Err(_) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_element_hash() {
        let elements1 = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let elements2 = vec!["c".to_string(), "a".to_string(), "b".to_string()];
        let elements3 = vec!["a".to_string(), "b".to_string()];

        // Same elements in different order should have same hash
        assert_eq!(
            compute_element_hash(&elements1),
            compute_element_hash(&elements2)
        );

        // Different elements should have different hash
        assert_ne!(
            compute_element_hash(&elements1),
            compute_element_hash(&elements3)
        );
    }

    #[test]
    fn test_change_type() {
        assert_eq!(ChangeType::None, ChangeType::None);
        assert_ne!(ChangeType::ElementsChanged, ChangeType::ScreenshotChanged);
    }

    #[test]
    fn test_baseline_diff_has_regressions() {
        let diff = BaselineDiff {
            baseline_id: "b1".to_string(),
            run_id: "r1".to_string(),
            compared_at: "2024-01-01".to_string(),
            new_states: vec![],
            missing_states: vec![],
            changed_states: vec![StateDiff {
                state_id: "s1".to_string(),
                state_name: "State 1".to_string(),
                change_type: ChangeType::StatusChanged,
                elements_added: vec![],
                elements_removed: vec![],
                screenshot_changed: false,
                confidence_diff: 0.0,
                duration_diff_ms: 0,
                status_changed: true,
                is_regression: true,
            }],
            new_transitions: vec![],
            missing_transitions: vec![],
            changed_transitions: vec![],
            timing_diff_percent: 0.0,
            summary: "Test".to_string(),
        };

        assert!(diff.has_regressions());
        assert_eq!(diff.regression_count(), 1);
    }
}
