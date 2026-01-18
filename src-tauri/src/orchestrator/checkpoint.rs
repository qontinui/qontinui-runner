//! Checkpointing with Time-Travel Debugging Module
//!
//! Implements checkpointing and replay capabilities inspired by LangGraph.
//! Enables saving execution state at key points and replaying from any checkpoint.
//!
//! ## Key Concepts
//!
//! - **Checkpoint**: A snapshot of execution state at a point in time
//! - **CheckpointManager**: Manages saving, loading, and listing checkpoints
//! - **ReplaySession**: Replays execution from a checkpoint
//!
//! ## Example
//!
//! ```rust
//! use qontinui_runner::orchestrator::checkpoint::*;
//!
//! let mut manager = CheckpointManager::new();
//!
//! // Save a checkpoint
//! let cp_id = manager.save(Checkpoint::new("task-1", state_snapshot));
//!
//! // Later, restore from checkpoint
//! let restored = manager.load(&cp_id);
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// State Snapshot
// ============================================================================

/// A snapshot of execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Current orchestrator state name.
    pub state: String,
    /// Current iteration.
    pub iteration: u32,
    /// Channel values with versions.
    pub channels: HashMap<String, ChannelSnapshot>,
    /// Knowledge base entries.
    pub knowledge: Vec<KnowledgeEntry>,
    /// Verification status.
    pub verification: VerificationSnapshot,
    /// Active findings.
    pub findings: Vec<FindingSnapshot>,
    /// Files modified.
    pub files_modified: Vec<String>,
    /// Custom state data.
    pub custom_data: HashMap<String, serde_json::Value>,
}

/// Snapshot of a channel's state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSnapshot {
    pub name: String,
    pub value: serde_json::Value,
    pub version: u64,
}

/// Snapshot of a knowledge entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    pub id: String,
    pub category: String,
    pub content: String,
    pub iteration: u32,
}

/// Snapshot of verification state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationSnapshot {
    pub criteria_results: HashMap<String, CriterionResult>,
    pub overall_passed: bool,
}

/// Result of a verification criterion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionResult {
    pub criterion_id: String,
    pub passed: bool,
    pub reason: Option<String>,
    pub verified_at: String,
}

/// Snapshot of a finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingSnapshot {
    pub id: String,
    pub category: String,
    pub severity: String,
    pub description: String,
    pub resolved: bool,
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            state: "unknown".to_string(),
            iteration: 0,
            channels: HashMap::new(),
            knowledge: Vec::new(),
            verification: VerificationSnapshot {
                criteria_results: HashMap::new(),
                overall_passed: false,
            },
            findings: Vec::new(),
            files_modified: Vec::new(),
            custom_data: HashMap::new(),
        }
    }
}

impl StateSnapshot {
    /// Create a new state snapshot.
    pub fn new(state: impl Into<String>, iteration: u32) -> Self {
        Self {
            state: state.into(),
            iteration,
            ..Default::default()
        }
    }

    /// Add a channel snapshot.
    pub fn with_channel(
        mut self,
        name: impl Into<String>,
        value: serde_json::Value,
        version: u64,
    ) -> Self {
        let name = name.into();
        self.channels.insert(
            name.clone(),
            ChannelSnapshot {
                name,
                value,
                version,
            },
        );
        self
    }

    /// Add custom data.
    pub fn with_data(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.custom_data.insert(key.into(), value);
        self
    }
}

// ============================================================================
// Checkpoint
// ============================================================================

/// A checkpoint of execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Unique checkpoint ID.
    pub id: String,
    /// Task ID this checkpoint belongs to.
    pub task_id: String,
    /// Human-readable name.
    pub name: Option<String>,
    /// Description of why this checkpoint was created.
    pub description: Option<String>,
    /// The state snapshot.
    pub state: StateSnapshot,
    /// When this checkpoint was created.
    pub created_at: String,
    /// Event that triggered this checkpoint.
    pub trigger: CheckpointTrigger,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Parent checkpoint ID (for branching).
    pub parent_id: Option<String>,
    /// Checkpoint metadata.
    pub metadata: HashMap<String, serde_json::Value>,
}

/// What triggered the checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CheckpointTrigger {
    /// Automatic (periodic or state change).
    Automatic { reason: String },
    /// Manual user request.
    Manual,
    /// Before a significant operation.
    BeforeOperation { operation: String },
    /// After a successful operation.
    AfterSuccess { operation: String },
    /// After a failure.
    AfterFailure { error: String },
    /// Verification boundary.
    VerificationBoundary,
    /// Iteration boundary.
    IterationBoundary { iteration: u32 },
    /// Custom trigger.
    Custom { trigger: String },
}

impl Checkpoint {
    /// Create a new checkpoint.
    pub fn new(task_id: impl Into<String>, state: StateSnapshot) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            name: None,
            description: None,
            state,
            created_at: chrono::Utc::now().to_rfc3339(),
            trigger: CheckpointTrigger::Manual,
            tags: Vec::new(),
            parent_id: None,
            metadata: HashMap::new(),
        }
    }

    /// Set the name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the trigger.
    pub fn with_trigger(mut self, trigger: CheckpointTrigger) -> Self {
        self.trigger = trigger;
        self
    }

    /// Add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Set parent checkpoint.
    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Get a summary for listing.
    pub fn summary(&self) -> CheckpointSummary {
        CheckpointSummary {
            id: self.id.clone(),
            task_id: self.task_id.clone(),
            name: self.name.clone(),
            state: self.state.state.clone(),
            iteration: self.state.iteration,
            created_at: self.created_at.clone(),
            trigger_type: self.trigger_type_name(),
            tags: self.tags.clone(),
        }
    }

    /// Get the trigger type name.
    fn trigger_type_name(&self) -> String {
        match &self.trigger {
            CheckpointTrigger::Automatic { .. } => "automatic".to_string(),
            CheckpointTrigger::Manual => "manual".to_string(),
            CheckpointTrigger::BeforeOperation { .. } => "before_operation".to_string(),
            CheckpointTrigger::AfterSuccess { .. } => "after_success".to_string(),
            CheckpointTrigger::AfterFailure { .. } => "after_failure".to_string(),
            CheckpointTrigger::VerificationBoundary => "verification".to_string(),
            CheckpointTrigger::IterationBoundary { .. } => "iteration".to_string(),
            CheckpointTrigger::Custom { .. } => "custom".to_string(),
        }
    }
}

/// Summary of a checkpoint for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointSummary {
    pub id: String,
    pub task_id: String,
    pub name: Option<String>,
    pub state: String,
    pub iteration: u32,
    pub created_at: String,
    pub trigger_type: String,
    pub tags: Vec<String>,
}

// ============================================================================
// Checkpoint Manager
// ============================================================================

/// Manages checkpoints for tasks.
#[derive(Debug, Default)]
pub struct CheckpointManager {
    /// Checkpoints by ID.
    checkpoints: HashMap<String, Checkpoint>,
    /// Index by task ID.
    task_index: HashMap<String, Vec<String>>,
    /// Maximum checkpoints per task.
    max_per_task: usize,
    /// Automatic checkpoint settings.
    auto_checkpoint: AutoCheckpointSettings,
}

/// Settings for automatic checkpointing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCheckpointSettings {
    /// Checkpoint every N iterations.
    pub every_n_iterations: Option<u32>,
    /// Checkpoint on state changes.
    pub on_state_change: bool,
    /// Checkpoint before verification.
    pub before_verification: bool,
    /// Checkpoint on failures.
    pub on_failure: bool,
}

impl Default for AutoCheckpointSettings {
    fn default() -> Self {
        Self {
            every_n_iterations: Some(5),
            on_state_change: true,
            before_verification: true,
            on_failure: true,
        }
    }
}

impl CheckpointManager {
    /// Create a new checkpoint manager.
    pub fn new() -> Self {
        Self {
            max_per_task: 50,
            ..Default::default()
        }
    }

    /// Set max checkpoints per task.
    pub fn with_max_per_task(mut self, max: usize) -> Self {
        self.max_per_task = max;
        self
    }

    /// Set auto checkpoint settings.
    pub fn with_auto_settings(mut self, settings: AutoCheckpointSettings) -> Self {
        self.auto_checkpoint = settings;
        self
    }

    /// Save a checkpoint.
    pub fn save(&mut self, checkpoint: Checkpoint) -> String {
        let id = checkpoint.id.clone();
        let task_id = checkpoint.task_id.clone();

        // Add to index
        self.task_index
            .entry(task_id.clone())
            .or_insert_with(Vec::new)
            .push(id.clone());

        // Enforce limit
        if let Some(task_cps) = self.task_index.get(&task_id) {
            if task_cps.len() > self.max_per_task {
                // Remove oldest
                if let Some(oldest_id) = task_cps.first().cloned() {
                    self.delete(&oldest_id);
                }
            }
        }

        // Store checkpoint
        self.checkpoints.insert(id.clone(), checkpoint);

        id
    }

    /// Load a checkpoint by ID.
    pub fn load(&self, id: &str) -> Option<&Checkpoint> {
        self.checkpoints.get(id)
    }

    /// Delete a checkpoint.
    pub fn delete(&mut self, id: &str) -> bool {
        if let Some(cp) = self.checkpoints.remove(id) {
            if let Some(task_cps) = self.task_index.get_mut(&cp.task_id) {
                task_cps.retain(|i| i != id);
            }
            true
        } else {
            false
        }
    }

    /// List checkpoints for a task.
    pub fn list_for_task(&self, task_id: &str) -> Vec<CheckpointSummary> {
        self.task_index
            .get(task_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.checkpoints.get(id))
                    .map(|cp| cp.summary())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// List all checkpoints across all tasks.
    pub fn list_all(&self) -> Vec<CheckpointSummary> {
        self.checkpoints.values().map(|cp| cp.summary()).collect()
    }

    /// Get all unique task IDs that have checkpoints.
    pub fn task_ids(&self) -> Vec<String> {
        self.task_index.keys().cloned().collect()
    }

    /// Find checkpoints by tag.
    pub fn find_by_tag(&self, tag: &str) -> Vec<CheckpointSummary> {
        self.checkpoints
            .values()
            .filter(|cp| cp.tags.iter().any(|t| t == tag))
            .map(|cp| cp.summary())
            .collect()
    }

    /// Get the latest checkpoint for a task.
    pub fn latest_for_task(&self, task_id: &str) -> Option<&Checkpoint> {
        self.task_index
            .get(task_id)
            .and_then(|ids| ids.last())
            .and_then(|id| self.checkpoints.get(id))
    }

    /// Check if automatic checkpoint should be created.
    pub fn should_auto_checkpoint(
        &self,
        current_iteration: u32,
        last_checkpoint_iteration: Option<u32>,
        state_changed: bool,
        is_verification_boundary: bool,
        is_failure: bool,
    ) -> bool {
        if let Some(interval) = self.auto_checkpoint.every_n_iterations {
            let last = last_checkpoint_iteration.unwrap_or(0);
            if current_iteration >= last + interval {
                return true;
            }
        }

        if self.auto_checkpoint.on_state_change && state_changed {
            return true;
        }

        if self.auto_checkpoint.before_verification && is_verification_boundary {
            return true;
        }

        if self.auto_checkpoint.on_failure && is_failure {
            return true;
        }

        false
    }

    /// Get total checkpoint count.
    pub fn total_count(&self) -> usize {
        self.checkpoints.len()
    }

    /// Get checkpoint count for a task.
    pub fn count_for_task(&self, task_id: &str) -> usize {
        self.task_index
            .get(task_id)
            .map(|ids| ids.len())
            .unwrap_or(0)
    }

    /// Clear all checkpoints for a task.
    pub fn clear_task(&mut self, task_id: &str) {
        if let Some(ids) = self.task_index.remove(task_id) {
            for id in ids {
                self.checkpoints.remove(&id);
            }
        }
    }
}

// ============================================================================
// Replay Session
// ============================================================================

/// A session for replaying from a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplaySession {
    /// Session ID.
    pub id: String,
    /// Original task ID.
    pub original_task_id: String,
    /// New task ID for this replay.
    pub replay_task_id: String,
    /// Starting checkpoint ID.
    pub checkpoint_id: String,
    /// Current state.
    pub state: StateSnapshot,
    /// Modifications made during replay.
    pub modifications: Vec<ReplayModification>,
    /// When the replay started.
    pub started_at: String,
    /// Replay status.
    pub status: ReplayStatus,
}

/// Status of a replay session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// A modification made during replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayModification {
    /// Type of modification.
    pub modification_type: ModificationType,
    /// Description.
    pub description: String,
    /// When applied.
    pub applied_at: String,
}

/// Types of replay modifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModificationType {
    /// Changed a channel value.
    ChannelChange {
        channel: String,
        old_value: serde_json::Value,
        new_value: serde_json::Value,
    },
    /// Changed state.
    StateChange {
        old_state: String,
        new_state: String,
    },
    /// Added knowledge.
    KnowledgeAdd { entry_id: String },
    /// Removed knowledge.
    KnowledgeRemove { entry_id: String },
    /// Custom modification.
    Custom { data: serde_json::Value },
}

impl ReplaySession {
    /// Create a new replay session from a checkpoint.
    pub fn from_checkpoint(checkpoint: &Checkpoint) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            original_task_id: checkpoint.task_id.clone(),
            replay_task_id: format!("{}-replay-{}", checkpoint.task_id, uuid::Uuid::new_v4()),
            checkpoint_id: checkpoint.id.clone(),
            state: checkpoint.state.clone(),
            modifications: Vec::new(),
            started_at: chrono::Utc::now().to_rfc3339(),
            status: ReplayStatus::Pending,
        }
    }

    /// Start the replay.
    pub fn start(&mut self) {
        self.status = ReplayStatus::Running;
    }

    /// Record a modification.
    pub fn record_modification(
        &mut self,
        mod_type: ModificationType,
        description: impl Into<String>,
    ) {
        self.modifications.push(ReplayModification {
            modification_type: mod_type,
            description: description.into(),
            applied_at: chrono::Utc::now().to_rfc3339(),
        });
    }

    /// Complete the replay.
    pub fn complete(&mut self) {
        self.status = ReplayStatus::Completed;
    }

    /// Fail the replay.
    pub fn fail(&mut self) {
        self.status = ReplayStatus::Failed;
    }

    /// Get modification count.
    pub fn modification_count(&self) -> usize {
        self.modifications.len()
    }
}

// ============================================================================
// Checkpoint Diff
// ============================================================================

/// Difference between two checkpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointDiff {
    /// From checkpoint ID.
    pub from_id: String,
    /// To checkpoint ID.
    pub to_id: String,
    /// State difference.
    pub state_change: Option<(String, String)>,
    /// Iteration difference.
    pub iteration_diff: i32,
    /// Changed channels.
    pub channel_changes: Vec<ChannelChange>,
    /// Added findings.
    pub added_findings: Vec<String>,
    /// Resolved findings.
    pub resolved_findings: Vec<String>,
}

/// A change to a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelChange {
    pub channel: String,
    pub from_version: u64,
    pub to_version: u64,
}

impl CheckpointDiff {
    /// Compute diff between two checkpoints.
    pub fn compute(from: &Checkpoint, to: &Checkpoint) -> Self {
        let state_change = if from.state.state != to.state.state {
            Some((from.state.state.clone(), to.state.state.clone()))
        } else {
            None
        };

        let iteration_diff = to.state.iteration as i32 - from.state.iteration as i32;

        let mut channel_changes = Vec::new();
        for (name, to_channel) in &to.state.channels {
            if let Some(from_channel) = from.state.channels.get(name) {
                if from_channel.version != to_channel.version {
                    channel_changes.push(ChannelChange {
                        channel: name.clone(),
                        from_version: from_channel.version,
                        to_version: to_channel.version,
                    });
                }
            } else {
                channel_changes.push(ChannelChange {
                    channel: name.clone(),
                    from_version: 0,
                    to_version: to_channel.version,
                });
            }
        }

        let from_finding_ids: std::collections::HashSet<_> =
            from.state.findings.iter().map(|f| &f.id).collect();
        let to_finding_ids: std::collections::HashSet<_> =
            to.state.findings.iter().map(|f| &f.id).collect();

        let added_findings = to_finding_ids
            .difference(&from_finding_ids)
            .map(|s| (*s).clone())
            .collect();

        let resolved_findings = to
            .state
            .findings
            .iter()
            .filter(|f| {
                f.resolved
                    && from
                        .state
                        .findings
                        .iter()
                        .any(|of| of.id == f.id && !of.resolved)
            })
            .map(|f| f.id.clone())
            .collect();

        Self {
            from_id: from.id.clone(),
            to_id: to.id.clone(),
            state_change,
            iteration_diff,
            channel_changes,
            added_findings,
            resolved_findings,
        }
    }
}

// ============================================================================
// Replay Manager
// ============================================================================

/// Manages replay sessions and tracks lineage between original tasks and replays.
#[derive(Debug, Default)]
pub struct ReplayManager {
    /// Active replay sessions by ID.
    active_sessions: HashMap<String, ReplaySession>,
    /// Mapping from replay task ID to session ID.
    task_to_session: HashMap<String, String>,
    /// Lineage tracking: child_task_id -> LineageInfo.
    lineage: HashMap<String, LineageInfo>,
}

/// Information about a task's lineage (replay history).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageInfo {
    /// The task run ID.
    pub task_run_id: String,
    /// The checkpoint this task was replayed from (if any).
    pub source_checkpoint_id: Option<String>,
    /// The original task ID (root of the lineage tree).
    pub original_task_id: String,
    /// Parent task ID (immediate parent, may be same as original or an intermediate replay).
    pub parent_task_id: Option<String>,
    /// Depth in the replay tree (0 = original, 1 = first replay, etc.).
    pub replay_depth: u32,
    /// When this lineage entry was created.
    pub created_at: String,
    /// IDs of tasks replayed from this task.
    pub child_task_ids: Vec<String>,
}

impl LineageInfo {
    /// Create lineage for an original (non-replayed) task.
    pub fn original(task_run_id: impl Into<String>) -> Self {
        let task_id = task_run_id.into();
        Self {
            task_run_id: task_id.clone(),
            source_checkpoint_id: None,
            original_task_id: task_id,
            parent_task_id: None,
            replay_depth: 0,
            created_at: chrono::Utc::now().to_rfc3339(),
            child_task_ids: Vec::new(),
        }
    }

    /// Create lineage for a replayed task.
    pub fn replayed(
        task_run_id: impl Into<String>,
        checkpoint_id: impl Into<String>,
        original_task_id: impl Into<String>,
        parent_task_id: impl Into<String>,
        replay_depth: u32,
    ) -> Self {
        Self {
            task_run_id: task_run_id.into(),
            source_checkpoint_id: Some(checkpoint_id.into()),
            original_task_id: original_task_id.into(),
            parent_task_id: Some(parent_task_id.into()),
            replay_depth,
            created_at: chrono::Utc::now().to_rfc3339(),
            child_task_ids: Vec::new(),
        }
    }

    /// Check if this is a replayed task.
    pub fn is_replay(&self) -> bool {
        self.source_checkpoint_id.is_some()
    }
}

/// Result of starting a replay from a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResult {
    /// The replay session.
    pub session: ReplaySession,
    /// Lineage information for the new task.
    pub lineage: LineageInfo,
    /// The restored state snapshot.
    pub restored_state: StateSnapshot,
}

impl ReplayManager {
    /// Create a new replay manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a replay session from a checkpoint.
    ///
    /// Creates a new task run ID, replay session, and lineage entry.
    /// Returns the ReplayResult which includes the session and lineage info.
    pub fn start_replay(
        &mut self,
        checkpoint: &Checkpoint,
        _checkpoints: &CheckpointManager,
    ) -> ReplayResult {
        // Create the replay session
        let session = ReplaySession::from_checkpoint(checkpoint);

        // Determine lineage
        let original_task_id = checkpoint.task_id.clone();

        // Check if the checkpoint's task was itself a replay
        let (root_task_id, parent_depth) =
            if let Some(lineage) = self.lineage.get(&checkpoint.task_id) {
                (lineage.original_task_id.clone(), lineage.replay_depth)
            } else {
                (checkpoint.task_id.clone(), 0)
            };

        let lineage = LineageInfo::replayed(
            &session.replay_task_id,
            &checkpoint.id,
            &root_task_id,
            &original_task_id,
            parent_depth + 1,
        );

        // Update parent's child list if tracked
        if let Some(parent_lineage) = self.lineage.get_mut(&original_task_id) {
            parent_lineage
                .child_task_ids
                .push(session.replay_task_id.clone());
        }

        // Store session and lineage
        let session_id = session.id.clone();
        let replay_task_id = session.replay_task_id.clone();
        self.active_sessions
            .insert(session_id.clone(), session.clone());
        self.task_to_session
            .insert(replay_task_id.clone(), session_id);
        self.lineage.insert(replay_task_id, lineage.clone());

        ReplayResult {
            session,
            lineage,
            restored_state: checkpoint.state.clone(),
        }
    }

    /// Get a replay session by session ID.
    pub fn get_session(&self, session_id: &str) -> Option<&ReplaySession> {
        self.active_sessions.get(session_id)
    }

    /// Get a mutable replay session by session ID.
    pub fn get_session_mut(&mut self, session_id: &str) -> Option<&mut ReplaySession> {
        self.active_sessions.get_mut(session_id)
    }

    /// Get the session for a replay task ID.
    pub fn get_session_for_task(&self, task_id: &str) -> Option<&ReplaySession> {
        self.task_to_session
            .get(task_id)
            .and_then(|sid| self.active_sessions.get(sid))
    }

    /// Mark a replay session as started (running).
    pub fn start_session(&mut self, session_id: &str) -> Result<(), String> {
        let session = self
            .active_sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("Session '{}' not found", session_id))?;
        session.start();
        Ok(())
    }

    /// Complete a replay session.
    pub fn complete_session(&mut self, session_id: &str) -> Result<(), String> {
        let session = self
            .active_sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("Session '{}' not found", session_id))?;
        session.complete();
        Ok(())
    }

    /// Fail a replay session.
    pub fn fail_session(&mut self, session_id: &str) -> Result<(), String> {
        let session = self
            .active_sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("Session '{}' not found", session_id))?;
        session.fail();
        Ok(())
    }

    /// Get lineage for a task.
    pub fn get_lineage(&self, task_id: &str) -> Option<&LineageInfo> {
        self.lineage.get(task_id)
    }

    /// Register lineage for an original (non-replayed) task.
    pub fn register_original_task(&mut self, task_id: impl Into<String>) {
        let task_id = task_id.into();
        if !self.lineage.contains_key(&task_id) {
            self.lineage
                .insert(task_id.clone(), LineageInfo::original(task_id));
        }
    }

    /// Get the full lineage tree for a task (all ancestors and descendants).
    pub fn get_lineage_tree(&self, task_id: &str) -> LineageTree {
        let mut tree = LineageTree {
            root_task_id: String::new(),
            nodes: Vec::new(),
        };

        // Find the lineage for this task
        let lineage = match self.lineage.get(task_id) {
            Some(l) => l,
            None => return tree,
        };

        // Find root
        tree.root_task_id = lineage.original_task_id.clone();

        // Collect all nodes in the lineage
        let mut to_visit = vec![tree.root_task_id.clone()];
        let mut visited = std::collections::HashSet::new();

        while let Some(current_id) = to_visit.pop() {
            if visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id.clone());

            if let Some(node_lineage) = self.lineage.get(&current_id) {
                tree.nodes.push(LineageNode {
                    task_id: current_id.clone(),
                    checkpoint_id: node_lineage.source_checkpoint_id.clone(),
                    parent_task_id: node_lineage.parent_task_id.clone(),
                    depth: node_lineage.replay_depth,
                    created_at: node_lineage.created_at.clone(),
                    is_current: current_id == task_id,
                });

                // Add children to visit
                for child_id in &node_lineage.child_task_ids {
                    to_visit.push(child_id.clone());
                }
            }
        }

        // Sort by depth and creation time
        tree.nodes.sort_by(|a, b| {
            a.depth
                .cmp(&b.depth)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });

        tree
    }

    /// List all active replay sessions.
    pub fn list_active_sessions(&self) -> Vec<&ReplaySession> {
        self.active_sessions
            .values()
            .filter(|s| s.status == ReplayStatus::Running || s.status == ReplayStatus::Pending)
            .collect()
    }

    /// Get count of active sessions.
    pub fn active_session_count(&self) -> usize {
        self.active_sessions
            .values()
            .filter(|s| s.status == ReplayStatus::Running || s.status == ReplayStatus::Pending)
            .count()
    }

    /// Clean up completed/failed sessions older than a threshold.
    pub fn cleanup_old_sessions(&mut self, max_age_seconds: u64) {
        let now = chrono::Utc::now();
        let max_age = chrono::Duration::seconds(max_age_seconds as i64);

        let to_remove: Vec<String> = self
            .active_sessions
            .iter()
            .filter(|(_, session)| {
                if session.status == ReplayStatus::Running
                    || session.status == ReplayStatus::Pending
                {
                    return false;
                }
                if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&session.started_at) {
                    now.signed_duration_since(started.with_timezone(&chrono::Utc)) > max_age
                } else {
                    false
                }
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in to_remove {
            if let Some(session) = self.active_sessions.remove(&id) {
                self.task_to_session.remove(&session.replay_task_id);
            }
        }
    }
}

/// A tree representation of task lineage (for visualization).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageTree {
    /// The root (original) task ID.
    pub root_task_id: String,
    /// All nodes in the tree.
    pub nodes: Vec<LineageNode>,
}

/// A node in the lineage tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageNode {
    /// Task run ID.
    pub task_id: String,
    /// Checkpoint ID this was replayed from (None for original).
    pub checkpoint_id: Option<String>,
    /// Parent task ID (None for root).
    pub parent_task_id: Option<String>,
    /// Depth in the tree (0 = root).
    pub depth: u32,
    /// When this task was created.
    pub created_at: String,
    /// Whether this is the current/queried task.
    pub is_current: bool,
}

// ============================================================================
// Orchestrator State Restoration
// ============================================================================

/// Configuration for restoring orchestrator state from a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateRestorationConfig {
    /// Whether to restore channel values.
    pub restore_channels: bool,
    /// Whether to restore knowledge entries.
    pub restore_knowledge: bool,
    /// Whether to restore findings.
    pub restore_findings: bool,
    /// Whether to restore verification state.
    pub restore_verification: bool,
    /// Whether to restore custom data.
    pub restore_custom_data: bool,
    /// Starting iteration for the replay (defaults to checkpoint iteration).
    pub starting_iteration: Option<u32>,
}

impl Default for StateRestorationConfig {
    fn default() -> Self {
        Self {
            restore_channels: true,
            restore_knowledge: true,
            restore_findings: true,
            restore_verification: true,
            restore_custom_data: true,
            starting_iteration: None,
        }
    }
}

impl StateRestorationConfig {
    /// Create a minimal restoration config (just state and iteration).
    pub fn minimal() -> Self {
        Self {
            restore_channels: false,
            restore_knowledge: false,
            restore_findings: false,
            restore_verification: false,
            restore_custom_data: false,
            starting_iteration: None,
        }
    }

    /// Create a full restoration config.
    pub fn full() -> Self {
        Self::default()
    }
}

/// Instructions for restoring orchestrator state.
/// Returned by prepare_restoration() for the caller to apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestorationInstructions {
    /// The target state to transition to.
    pub target_state: String,
    /// The iteration to resume from.
    pub resume_iteration: u32,
    /// Channel values to restore.
    pub channels: HashMap<String, serde_json::Value>,
    /// Knowledge entries to restore.
    pub knowledge: Vec<KnowledgeEntry>,
    /// Findings to restore.
    pub findings: Vec<FindingSnapshot>,
    /// Verification state to restore.
    pub verification: VerificationSnapshot,
    /// Custom data to restore.
    pub custom_data: HashMap<String, serde_json::Value>,
    /// Files that were modified (for reference).
    pub files_modified: Vec<String>,
    /// The checkpoint this restoration is from.
    pub source_checkpoint_id: String,
    /// The original task ID.
    pub original_task_id: String,
}

impl RestorationInstructions {
    /// Create restoration instructions from a checkpoint.
    pub fn from_checkpoint(checkpoint: &Checkpoint, config: &StateRestorationConfig) -> Self {
        let snapshot = &checkpoint.state;

        Self {
            target_state: snapshot.state.clone(),
            resume_iteration: config.starting_iteration.unwrap_or(snapshot.iteration),
            channels: if config.restore_channels {
                snapshot
                    .channels
                    .iter()
                    .map(|(k, v)| (k.clone(), v.value.clone()))
                    .collect()
            } else {
                HashMap::new()
            },
            knowledge: if config.restore_knowledge {
                snapshot.knowledge.clone()
            } else {
                Vec::new()
            },
            findings: if config.restore_findings {
                snapshot.findings.clone()
            } else {
                Vec::new()
            },
            verification: if config.restore_verification {
                snapshot.verification.clone()
            } else {
                VerificationSnapshot {
                    criteria_results: HashMap::new(),
                    overall_passed: false,
                }
            },
            custom_data: if config.restore_custom_data {
                snapshot.custom_data.clone()
            } else {
                HashMap::new()
            },
            files_modified: snapshot.files_modified.clone(),
            source_checkpoint_id: checkpoint.id.clone(),
            original_task_id: checkpoint.task_id.clone(),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_snapshot_creation() {
        let snapshot = StateSnapshot::new("planning", 5)
            .with_channel("status", serde_json::json!("active"), 3)
            .with_data("custom", serde_json::json!({"key": "value"}));

        assert_eq!(snapshot.state, "planning");
        assert_eq!(snapshot.iteration, 5);
        assert!(snapshot.channels.contains_key("status"));
    }

    #[test]
    fn test_checkpoint_creation() {
        let state = StateSnapshot::new("executing", 3);
        let cp = Checkpoint::new("task-1", state)
            .with_name("Before verification")
            .with_trigger(CheckpointTrigger::VerificationBoundary)
            .with_tag("important");

        assert_eq!(cp.task_id, "task-1");
        assert!(cp.name.is_some());
        assert!(cp.tags.contains(&"important".to_string()));
    }

    #[test]
    fn test_checkpoint_manager() {
        let mut manager = CheckpointManager::new();

        let state = StateSnapshot::new("state1", 1);
        let cp = Checkpoint::new("task-1", state);
        let id = manager.save(cp);

        assert!(manager.load(&id).is_some());
        assert_eq!(manager.count_for_task("task-1"), 1);
    }

    #[test]
    fn test_checkpoint_listing() {
        let mut manager = CheckpointManager::new();

        for i in 0..3 {
            let state = StateSnapshot::new("state", i);
            let cp = Checkpoint::new("task-1", state);
            manager.save(cp);
        }

        let list = manager.list_for_task("task-1");
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn test_auto_checkpoint_logic() {
        let manager = CheckpointManager::new();

        // Should checkpoint due to interval
        assert!(manager.should_auto_checkpoint(10, Some(0), false, false, false));

        // Should checkpoint due to state change
        assert!(manager.should_auto_checkpoint(1, Some(0), true, false, false));

        // Should checkpoint before verification
        assert!(manager.should_auto_checkpoint(1, Some(0), false, true, false));

        // Should not checkpoint (no conditions met)
        assert!(!manager.should_auto_checkpoint(2, Some(0), false, false, false));
    }

    #[test]
    fn test_replay_session() {
        let state = StateSnapshot::new("executing", 5);
        let cp = Checkpoint::new("task-1", state);

        let mut session = ReplaySession::from_checkpoint(&cp);
        assert_eq!(session.status, ReplayStatus::Pending);

        session.start();
        assert_eq!(session.status, ReplayStatus::Running);

        session.record_modification(
            ModificationType::StateChange {
                old_state: "executing".to_string(),
                new_state: "planning".to_string(),
            },
            "Changed state for testing",
        );

        assert_eq!(session.modification_count(), 1);
    }

    #[test]
    fn test_checkpoint_diff() {
        let state1 = StateSnapshot::new("planning", 1).with_channel(
            "status",
            serde_json::json!("pending"),
            1,
        );
        let cp1 = Checkpoint::new("task-1", state1);

        let state2 = StateSnapshot::new("executing", 3).with_channel(
            "status",
            serde_json::json!("running"),
            5,
        );
        let cp2 = Checkpoint::new("task-1", state2);

        let diff = CheckpointDiff::compute(&cp1, &cp2);

        assert!(diff.state_change.is_some());
        assert_eq!(diff.iteration_diff, 2);
        assert!(!diff.channel_changes.is_empty());
    }

    #[test]
    fn test_max_checkpoints_per_task() {
        let mut manager = CheckpointManager::new().with_max_per_task(3);

        for i in 0..5 {
            let state = StateSnapshot::new("state", i);
            let cp = Checkpoint::new("task-1", state);
            manager.save(cp);
        }

        // Should only keep 3 (most recent)
        assert_eq!(manager.count_for_task("task-1"), 3);
    }

    #[test]
    fn test_replay_manager_start_replay() {
        let mut cp_manager = CheckpointManager::new();
        let mut replay_manager = ReplayManager::new();

        // Create a checkpoint
        let state = StateSnapshot::new("executing", 5);
        let cp = Checkpoint::new("task-1", state);
        let cp_id = cp_manager.save(cp.clone());

        // Start a replay
        let result = replay_manager.start_replay(&cp, &cp_manager);

        assert_eq!(result.session.original_task_id, "task-1");
        assert_eq!(result.session.checkpoint_id, cp.id);
        assert_eq!(result.session.status, ReplayStatus::Pending);
        assert_eq!(result.lineage.replay_depth, 1);
        assert_eq!(result.lineage.original_task_id, "task-1");
        assert!(result.lineage.source_checkpoint_id.is_some());
    }

    #[test]
    fn test_replay_lineage_tracking() {
        let mut cp_manager = CheckpointManager::new();
        let mut replay_manager = ReplayManager::new();

        // Register original task
        replay_manager.register_original_task("task-1");

        // Create checkpoint for original task
        let state = StateSnapshot::new("executing", 5);
        let cp1 = Checkpoint::new("task-1", state);
        cp_manager.save(cp1.clone());

        // First replay
        let result1 = replay_manager.start_replay(&cp1, &cp_manager);
        let replay1_task_id = result1.session.replay_task_id.clone();

        // Create checkpoint for first replay
        let state2 = StateSnapshot::new("executing", 7);
        let mut cp2 = Checkpoint::new(&replay1_task_id, state2);
        cp_manager.save(cp2.clone());

        // Second replay (from first replay's checkpoint)
        let result2 = replay_manager.start_replay(&cp2, &cp_manager);

        // Check lineage depths
        assert_eq!(result1.lineage.replay_depth, 1);
        assert_eq!(result2.lineage.replay_depth, 2);
        assert_eq!(result2.lineage.original_task_id, "task-1");
        assert_eq!(result2.lineage.parent_task_id, Some(replay1_task_id));
    }

    #[test]
    fn test_lineage_tree() {
        let mut cp_manager = CheckpointManager::new();
        let mut replay_manager = ReplayManager::new();

        // Register original
        replay_manager.register_original_task("task-1");

        // Create checkpoint
        let state = StateSnapshot::new("executing", 5);
        let cp = Checkpoint::new("task-1", state);
        cp_manager.save(cp.clone());

        // Start replay
        let result = replay_manager.start_replay(&cp, &cp_manager);

        // Get lineage tree
        let tree = replay_manager.get_lineage_tree(&result.session.replay_task_id);

        assert_eq!(tree.root_task_id, "task-1");
        assert_eq!(tree.nodes.len(), 2); // Original + replay
    }

    #[test]
    fn test_restoration_instructions() {
        let state = StateSnapshot::new("executing", 5)
            .with_channel("status", serde_json::json!("active"), 3)
            .with_data("custom_key", serde_json::json!({"nested": true}));

        let cp = Checkpoint::new("task-1", state);

        // Full restoration
        let full_config = StateRestorationConfig::full();
        let full_instructions = RestorationInstructions::from_checkpoint(&cp, &full_config);

        assert_eq!(full_instructions.target_state, "executing");
        assert_eq!(full_instructions.resume_iteration, 5);
        assert!(!full_instructions.channels.is_empty());
        assert!(!full_instructions.custom_data.is_empty());

        // Minimal restoration
        let minimal_config = StateRestorationConfig::minimal();
        let minimal_instructions = RestorationInstructions::from_checkpoint(&cp, &minimal_config);

        assert_eq!(minimal_instructions.target_state, "executing");
        assert!(minimal_instructions.channels.is_empty());
        assert!(minimal_instructions.custom_data.is_empty());
    }

    #[test]
    fn test_session_lifecycle() {
        let mut cp_manager = CheckpointManager::new();
        let mut replay_manager = ReplayManager::new();

        let state = StateSnapshot::new("executing", 5);
        let cp = Checkpoint::new("task-1", state);
        cp_manager.save(cp.clone());

        let result = replay_manager.start_replay(&cp, &cp_manager);
        let session_id = result.session.id.clone();

        // Start session
        replay_manager.start_session(&session_id).unwrap();
        assert_eq!(
            replay_manager.get_session(&session_id).unwrap().status,
            ReplayStatus::Running
        );

        // Complete session
        replay_manager.complete_session(&session_id).unwrap();
        assert_eq!(
            replay_manager.get_session(&session_id).unwrap().status,
            ReplayStatus::Completed
        );
    }
}
