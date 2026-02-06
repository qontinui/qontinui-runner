//! Checkpoint system for State Explorer
//!
//! This module implements checkpointing for exploration tasks, enabling:
//! - Pausing exploration at strategic points
//! - Saving state for resumption
//! - Interleaving with agentic fix work
//!
//! # Checkpoint Triggers
//!
//! Exploration can pause at checkpoints when:
//! - A critical assertion fails
//! - A batch of states has been explored
//! - The issue threshold is reached
//!
//! # Resumption
//!
//! Checkpoints are persisted and can be resumed:
//! - After AI fixes are applied
//! - After runner restart
//! - For debugging purposes

#![allow(dead_code)]

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::report::Discrepancy;
use super::types::ExplorationConfig;

/// Triggers that can cause exploration to pause at a checkpoint
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointTrigger {
    /// A critical assertion failed - must fix before continuing
    CriticalFailure {
        state_id: String,
        failure_reason: String,
    },
    /// Completed a batch of states
    BatchComplete {
        batch_size: usize,
        batch_number: usize,
    },
    /// Accumulated too many issues
    IssueThreshold {
        issue_count: usize,
        threshold: usize,
    },
    /// Manual pause requested
    ManualPause,
    /// Timeout reached but not complete
    Timeout {
        elapsed_seconds: u64,
        max_seconds: u64,
    },
}

/// State of an exploration checkpoint
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    /// Checkpoint is active and can be resumed
    Active,
    /// Checkpoint was resumed and exploration continued
    Resumed,
    /// Checkpoint was abandoned (e.g., config changed)
    Abandoned,
    /// Exploration completed from this checkpoint
    Completed,
}

/// A checkpoint in the exploration process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationCheckpoint {
    /// Unique checkpoint ID
    pub id: String,
    /// Run ID this checkpoint belongs to
    pub run_id: String,
    /// Configuration used for exploration
    pub config: ExplorationConfig,
    /// States that have been explored
    pub completed_states: Vec<String>,
    /// States remaining to explore
    pub pending_states: Vec<String>,
    /// Current position in exploration path
    pub current_position: usize,
    /// Issues accumulated so far
    pub accumulated_issues: Vec<Discrepancy>,
    /// What triggered this checkpoint
    pub trigger: CheckpointTrigger,
    /// Status of this checkpoint
    pub status: CheckpointStatus,
    /// When the checkpoint was created (ISO 8601)
    pub created_at: String,
    /// When the checkpoint was last updated (ISO 8601)
    pub updated_at: String,
    /// Path to checkpoint file on disk
    pub checkpoint_path: Option<PathBuf>,
    /// Total exploration time so far (milliseconds)
    pub elapsed_ms: u64,
}

impl ExplorationCheckpoint {
    /// Create a new checkpoint
    pub fn new(
        run_id: String,
        config: ExplorationConfig,
        completed_states: Vec<String>,
        pending_states: Vec<String>,
        current_position: usize,
        accumulated_issues: Vec<Discrepancy>,
        trigger: CheckpointTrigger,
        elapsed_ms: u64,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            run_id,
            config,
            completed_states,
            pending_states,
            current_position,
            accumulated_issues,
            trigger,
            status: CheckpointStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            checkpoint_path: None,
            elapsed_ms,
        }
    }

    /// Mark checkpoint as resumed
    pub fn mark_resumed(&mut self) {
        self.status = CheckpointStatus::Resumed;
        self.updated_at = Utc::now().to_rfc3339();
    }

    /// Mark checkpoint as abandoned
    pub fn mark_abandoned(&mut self) {
        self.status = CheckpointStatus::Abandoned;
        self.updated_at = Utc::now().to_rfc3339();
    }

    /// Mark checkpoint as completed
    pub fn mark_completed(&mut self) {
        self.status = CheckpointStatus::Completed;
        self.updated_at = Utc::now().to_rfc3339();
    }

    /// Save checkpoint to disk
    pub fn save(&mut self, output_dir: &Path) -> Result<PathBuf, String> {
        let checkpoint_dir = output_dir.join("checkpoints");
        std::fs::create_dir_all(&checkpoint_dir)
            .map_err(|e| format!("Failed to create checkpoint directory: {}", e))?;

        let filename = format!("checkpoint-{}-{}.json", self.run_id, self.id);
        let path = checkpoint_dir.join(&filename);

        self.checkpoint_path = Some(path.clone());
        self.updated_at = Utc::now().to_rfc3339();

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize checkpoint: {}", e))?;

        std::fs::write(&path, json)
            .map_err(|e| format!("Failed to write checkpoint file: {}", e))?;

        Ok(path)
    }

    /// Load checkpoint from disk
    pub fn load(path: &PathBuf) -> Result<Self, String> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read checkpoint file: {}", e))?;

        serde_json::from_str(&json).map_err(|e| format!("Failed to parse checkpoint: {}", e))
    }

    /// Get a summary of issues for AI analysis
    pub fn get_issue_summary(&self) -> String {
        if self.accumulated_issues.is_empty() {
            return "No issues accumulated.".to_string();
        }

        let mut summary = format!(
            "# Exploration Checkpoint Summary\n\n\
             **Trigger:** {:?}\n\
             **States Completed:** {}\n\
             **States Remaining:** {}\n\
             **Issues Found:** {}\n\n\
             ## Issues\n\n",
            self.trigger,
            self.completed_states.len(),
            self.pending_states.len(),
            self.accumulated_issues.len()
        );

        for (i, issue) in self.accumulated_issues.iter().enumerate() {
            summary.push_str(&format!(
                "{}. **{:?}** in {} ({})\n   - Expected: {}\n   - Actual: {}\n",
                i + 1,
                issue.discrepancy_type,
                issue.state_name,
                issue.severity,
                issue.expected,
                issue.actual
            ));
            if !issue.context.is_empty() {
                summary.push_str(&format!("   - Context: {}\n", issue.context));
            }
            if let Some(suggested) = &issue.suggested_action {
                summary.push_str(&format!("   - Suggested: {}\n", suggested));
            }
            summary.push('\n');
        }

        summary
    }

    /// Check if this checkpoint should trigger agentic work
    pub fn should_trigger_agentic(&self) -> bool {
        match &self.trigger {
            CheckpointTrigger::CriticalFailure { .. } => true,
            CheckpointTrigger::IssueThreshold {
                issue_count,
                threshold,
            } => *issue_count >= *threshold,
            CheckpointTrigger::BatchComplete { .. } => {
                // Only trigger if there are issues to fix
                !self.accumulated_issues.is_empty()
            }
            CheckpointTrigger::ManualPause => false,
            CheckpointTrigger::Timeout { .. } => false,
        }
    }
}

/// Configuration for checkpoint behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointConfig {
    /// Number of states to explore before creating a checkpoint
    pub batch_size: usize,
    /// Number of issues to accumulate before pausing
    pub issue_threshold: usize,
    /// Whether to pause on critical failures
    pub pause_on_critical: bool,
    /// Whether to interleave with agentic work
    pub interleave_enabled: bool,
    /// Directory for checkpoint files
    pub checkpoint_dir: Option<PathBuf>,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            batch_size: 10,
            issue_threshold: 5,
            pause_on_critical: true,
            interleave_enabled: false,
            checkpoint_dir: None,
        }
    }
}

impl CheckpointConfig {
    /// Create config for quick exploration (larger batches, higher threshold)
    pub fn quick() -> Self {
        Self {
            batch_size: 20,
            issue_threshold: 10,
            pause_on_critical: true,
            interleave_enabled: false,
            checkpoint_dir: None,
        }
    }

    /// Create config for thorough exploration (smaller batches, lower threshold)
    pub fn thorough() -> Self {
        Self {
            batch_size: 5,
            issue_threshold: 3,
            pause_on_critical: true,
            interleave_enabled: true,
            checkpoint_dir: None,
        }
    }

    /// Create config with interleaving enabled
    pub fn with_interleaving(mut self) -> Self {
        self.interleave_enabled = true;
        self
    }
}

/// Manager for exploration checkpoints
pub struct CheckpointManager {
    /// Active checkpoints by run ID
    checkpoints: std::collections::HashMap<String, Vec<ExplorationCheckpoint>>,
    /// Configuration
    config: CheckpointConfig,
    /// Output directory for checkpoint files
    output_dir: PathBuf,
}

impl CheckpointManager {
    /// Create a new checkpoint manager
    pub fn new(config: CheckpointConfig, output_dir: PathBuf) -> Self {
        Self {
            checkpoints: std::collections::HashMap::new(),
            config,
            output_dir,
        }
    }

    /// Check if a checkpoint should be created
    pub fn should_checkpoint(
        &self,
        states_since_checkpoint: usize,
        issue_count: usize,
        has_critical_failure: bool,
    ) -> Option<CheckpointTrigger> {
        if has_critical_failure && self.config.pause_on_critical {
            return Some(CheckpointTrigger::CriticalFailure {
                state_id: "unknown".to_string(),
                failure_reason: "Critical assertion failed".to_string(),
            });
        }

        if issue_count >= self.config.issue_threshold {
            return Some(CheckpointTrigger::IssueThreshold {
                issue_count,
                threshold: self.config.issue_threshold,
            });
        }

        if states_since_checkpoint >= self.config.batch_size {
            return Some(CheckpointTrigger::BatchComplete {
                batch_size: self.config.batch_size,
                batch_number: states_since_checkpoint / self.config.batch_size,
            });
        }

        None
    }

    /// Create and store a checkpoint
    pub fn create_checkpoint(
        &mut self,
        run_id: String,
        config: ExplorationConfig,
        completed_states: Vec<String>,
        pending_states: Vec<String>,
        current_position: usize,
        accumulated_issues: Vec<Discrepancy>,
        trigger: CheckpointTrigger,
        elapsed_ms: u64,
    ) -> Result<ExplorationCheckpoint, String> {
        let mut checkpoint = ExplorationCheckpoint::new(
            run_id.clone(),
            config,
            completed_states,
            pending_states,
            current_position,
            accumulated_issues,
            trigger,
            elapsed_ms,
        );

        // Save to disk
        checkpoint.save(&self.output_dir)?;

        // Store in memory
        self.checkpoints
            .entry(run_id)
            .or_default()
            .push(checkpoint.clone());

        Ok(checkpoint)
    }

    /// Get the latest checkpoint for a run
    pub fn get_latest_checkpoint(&self, run_id: &str) -> Option<&ExplorationCheckpoint> {
        self.checkpoints.get(run_id).and_then(|checkpoints| {
            checkpoints
                .iter()
                .filter(|c| c.status == CheckpointStatus::Active)
                .next_back()
        })
    }

    /// Load checkpoints from disk for a run
    pub fn load_checkpoints(&mut self, run_id: &str) -> Result<Vec<ExplorationCheckpoint>, String> {
        let checkpoint_dir = self.output_dir.join("checkpoints");
        if !checkpoint_dir.exists() {
            return Ok(vec![]);
        }

        let _pattern = format!("checkpoint-{}-*.json", run_id);
        let mut loaded = vec![];

        for entry in std::fs::read_dir(&checkpoint_dir)
            .map_err(|e| format!("Failed to read checkpoint directory: {}", e))?
        {
            let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
            let path = entry.path();

            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if filename.starts_with(&format!("checkpoint-{}-", run_id))
                    && filename.ends_with(".json")
                {
                    match ExplorationCheckpoint::load(&path) {
                        Ok(checkpoint) => loaded.push(checkpoint),
                        Err(e) => {
                            tracing::warn!("Failed to load checkpoint {:?}: {}", path, e);
                        }
                    }
                }
            }
        }

        // Store loaded checkpoints
        if !loaded.is_empty() {
            self.checkpoints.insert(run_id.to_string(), loaded.clone());
        }

        Ok(loaded)
    }

    /// Check if interleaving is enabled
    pub fn is_interleaving_enabled(&self) -> bool {
        self.config.interleave_enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkpoint_creation() {
        let checkpoint = ExplorationCheckpoint::new(
            "run-123".to_string(),
            ExplorationConfig::default(),
            vec!["state1".to_string(), "state2".to_string()],
            vec!["state3".to_string()],
            2,
            vec![],
            CheckpointTrigger::BatchComplete {
                batch_size: 2,
                batch_number: 1,
            },
            5000,
        );

        assert_eq!(checkpoint.run_id, "run-123");
        assert_eq!(checkpoint.completed_states.len(), 2);
        assert_eq!(checkpoint.pending_states.len(), 1);
        assert_eq!(checkpoint.status, CheckpointStatus::Active);
    }

    #[test]
    fn test_should_checkpoint() {
        let config = CheckpointConfig {
            batch_size: 5,
            issue_threshold: 3,
            pause_on_critical: true,
            interleave_enabled: false,
            checkpoint_dir: None,
        };

        let manager = CheckpointManager::new(config, PathBuf::from("."));

        // No checkpoint needed
        assert!(manager.should_checkpoint(2, 1, false).is_none());

        // Batch complete
        let trigger = manager.should_checkpoint(5, 1, false);
        assert!(matches!(
            trigger,
            Some(CheckpointTrigger::BatchComplete { .. })
        ));

        // Issue threshold
        let trigger = manager.should_checkpoint(2, 3, false);
        assert!(matches!(
            trigger,
            Some(CheckpointTrigger::IssueThreshold { .. })
        ));

        // Critical failure
        let trigger = manager.should_checkpoint(1, 0, true);
        assert!(matches!(
            trigger,
            Some(CheckpointTrigger::CriticalFailure { .. })
        ));
    }

    #[test]
    fn test_issue_summary() {
        use super::super::report::{Discrepancy, DiscrepancyType};

        let checkpoint = ExplorationCheckpoint::new(
            "run-123".to_string(),
            ExplorationConfig::default(),
            vec!["state1".to_string()],
            vec!["state2".to_string()],
            1,
            vec![Discrepancy {
                discrepancy_type: DiscrepancyType::MissingElement,
                state_id: "state1".to_string(),
                state_name: "Login Page".to_string(),
                transition_id: None,
                expected: "Submit Button".to_string(),
                actual: "Not found".to_string(),
                severity: "warning".to_string(),
                screenshot_path: None,
                context: "Expected button was not found on the login page".to_string(),
                suggested_action: Some("Check if element ID changed".to_string()),
            }],
            CheckpointTrigger::IssueThreshold {
                issue_count: 1,
                threshold: 1,
            },
            3000,
        );

        let summary = checkpoint.get_issue_summary();
        assert!(summary.contains("MissingElement"));
        assert!(summary.contains("Login Page"));
        assert!(summary.contains("Check if element ID changed"));
    }
}
