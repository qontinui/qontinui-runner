//! Parallel Execution with Merge Module
//!
//! Implements true parallel work distribution inspired by AutoGen, Dify, and LangGraph.
//! Enables multiple workers to execute concurrently with flexible synchronization strategies.
//!
//! ## Key Concepts
//!
//! - **Branch**: An independent unit of work that can run in parallel
//! - **MergeStrategy**: How to synchronize parallel branches
//! - **ParallelExecution**: Orchestrates parallel branch execution
//!
//! ## Example
//!
//! ```rust
//! use qontinui_runner::orchestrator::parallel::*;
//!
//! let execution = ParallelExecution::new()
//!     .with_branch(Branch::new("analyze_code", "code_analysis"))
//!     .with_branch(Branch::new("run_tests", "testing"))
//!     .with_merge_strategy(MergeStrategy::WaitAll)
//!     .with_timeout(Duration::from_secs(300));
//! ```

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

// ============================================================================
// Branch Definition
// ============================================================================

/// A branch of parallel execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    /// Unique identifier for this branch.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Domain assignment for this branch.
    pub domain: String,
    /// Worker prompt or configuration.
    pub worker_config: WorkerConfig,
    /// Current status.
    pub status: BranchStatus,
    /// Dependencies on other branches (must complete first).
    pub depends_on: Vec<String>,
    /// Priority (higher = more important).
    pub priority: u32,
    /// Branch-specific timeout.
    pub timeout_secs: Option<u64>,
    /// Whether failure of this branch should abort the entire parallel execution.
    pub critical: bool,
}

/// Configuration for a worker in a branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// The prompt or task description.
    pub prompt: String,
    /// Agent role ID (if using agent roles).
    pub agent_role: Option<String>,
    /// Maximum iterations for this worker.
    pub max_iterations: Option<u32>,
    /// Input mappings from other branches or parent context.
    pub inputs: HashMap<String, String>,
}

/// Status of a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BranchStatus {
    /// Not yet started.
    #[default]
    Pending,
    /// Waiting for dependencies.
    Blocked,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Completed,
    /// Failed with error.
    Failed,
    /// Cancelled (due to timeout or other branch failure).
    Cancelled,
    /// Skipped (condition not met).
    Skipped,
}

impl Branch {
    /// Create a new branch.
    pub fn new(id: impl Into<String>, domain: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            domain: domain.into(),
            worker_config: WorkerConfig {
                prompt: String::new(),
                agent_role: None,
                max_iterations: None,
                inputs: HashMap::new(),
            },
            status: BranchStatus::Pending,
            depends_on: Vec::new(),
            priority: 50,
            timeout_secs: None,
            critical: false,
        }
    }

    /// Set the name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the worker prompt.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.worker_config.prompt = prompt.into();
        self
    }

    /// Set the agent role.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.worker_config.agent_role = Some(role.into());
        self
    }

    /// Add a dependency on another branch.
    pub fn depends_on(mut self, branch_id: impl Into<String>) -> Self {
        self.depends_on.push(branch_id.into());
        self
    }

    /// Set priority.
    pub fn with_priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// Mark as critical.
    pub fn critical(mut self) -> Self {
        self.critical = true;
        self
    }

    /// Add an input mapping.
    pub fn with_input(mut self, name: impl Into<String>, source: impl Into<String>) -> Self {
        self.worker_config.inputs.insert(name.into(), source.into());
        self
    }

    /// Check if this branch can start (all dependencies completed).
    pub fn can_start(&self, completed_branches: &[&str]) -> bool {
        self.depends_on
            .iter()
            .all(|dep| completed_branches.contains(&dep.as_str()))
    }

    /// Check if branch is finished (completed, failed, cancelled, or skipped).
    pub fn is_finished(&self) -> bool {
        matches!(
            self.status,
            BranchStatus::Completed
                | BranchStatus::Failed
                | BranchStatus::Cancelled
                | BranchStatus::Skipped
        )
    }
}

// ============================================================================
// Branch Result
// ============================================================================

/// Result of a branch execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchResult {
    /// Branch ID.
    pub branch_id: String,
    /// Whether the branch succeeded.
    pub success: bool,
    /// Output data.
    pub outputs: HashMap<String, serde_json::Value>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Number of iterations used.
    pub iterations: u32,
    /// Findings from this branch.
    pub findings: Vec<BranchFinding>,
    /// Files modified.
    pub files_modified: Vec<String>,
}

/// A finding from a branch execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchFinding {
    pub category: String,
    pub severity: String,
    pub description: String,
    pub location: Option<String>,
}

impl BranchResult {
    /// Create a successful result.
    pub fn success(branch_id: impl Into<String>) -> Self {
        Self {
            branch_id: branch_id.into(),
            success: true,
            outputs: HashMap::new(),
            error: None,
            duration_ms: 0,
            iterations: 0,
            findings: Vec::new(),
            files_modified: Vec::new(),
        }
    }

    /// Create a failed result.
    pub fn failure(branch_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            branch_id: branch_id.into(),
            success: false,
            outputs: HashMap::new(),
            error: Some(error.into()),
            duration_ms: 0,
            iterations: 0,
            findings: Vec::new(),
            files_modified: Vec::new(),
        }
    }

    /// Set duration.
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    /// Set iterations.
    pub fn with_iterations(mut self, iterations: u32) -> Self {
        self.iterations = iterations;
        self
    }

    /// Add output.
    pub fn with_output(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.outputs.insert(key.into(), value);
        self
    }
}

// ============================================================================
// Merge Strategy
// ============================================================================

/// Strategy for merging parallel branch results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum MergeStrategy {
    /// Wait for all branches to complete.
    #[default]
    WaitAll,
    /// Continue when any branch completes successfully.
    WaitAny,
    /// Wait for N branches to complete.
    WaitN { count: usize },
    /// Wait for a percentage of branches to agree.
    Quorum { threshold: f32 },
    /// Wait for specific branches by ID.
    WaitFor { branch_ids: Vec<String> },
    /// First successful result wins, cancel others.
    Race,
    /// Custom merge logic.
    Custom { handler: String },
}


impl MergeStrategy {
    /// Check if merge condition is satisfied.
    pub fn is_satisfied(&self, results: &[&BranchResult], total_branches: usize) -> bool {
        let completed = results.len();
        let successful = results.iter().filter(|r| r.success).count();

        match self {
            Self::WaitAll => completed == total_branches,
            Self::WaitAny => successful >= 1,
            Self::WaitN { count } => completed >= *count,
            Self::Quorum { threshold } => {
                let ratio = successful as f32 / total_branches as f32;
                ratio >= *threshold
            }
            Self::WaitFor { branch_ids } => branch_ids
                .iter()
                .all(|id| results.iter().any(|r| r.branch_id == *id)),
            Self::Race => successful >= 1,
            Self::Custom { .. } => false, // Custom handlers not evaluated here
        }
    }

    /// Describe this strategy.
    pub fn description(&self) -> String {
        match self {
            Self::WaitAll => "Wait for all branches to complete".to_string(),
            Self::WaitAny => "Continue when any branch succeeds".to_string(),
            Self::WaitN { count } => format!("Wait for {} branches to complete", count),
            Self::Quorum { threshold } => {
                format!("Wait for {}% of branches to succeed", threshold * 100.0)
            }
            Self::WaitFor { branch_ids } => {
                format!("Wait for branches: {}", branch_ids.join(", "))
            }
            Self::Race => "First successful branch wins".to_string(),
            Self::Custom { handler } => format!("Custom handler: {}", handler),
        }
    }
}

// ============================================================================
// Parallel Execution
// ============================================================================

/// Configuration and state for parallel execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelExecution {
    /// Unique identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Branches to execute.
    pub branches: Vec<Branch>,
    /// Merge strategy.
    pub merge_strategy: MergeStrategy,
    /// Overall timeout.
    pub timeout_secs: u64,
    /// Maximum concurrent branches.
    pub max_concurrent: Option<usize>,
    /// Whether to fail fast on any critical branch failure.
    pub fail_fast: bool,
    /// Current status.
    pub status: ParallelStatus,
    /// Branch results.
    pub results: HashMap<String, BranchResult>,
    /// When execution started.
    pub started_at: Option<String>,
    /// When execution completed.
    pub completed_at: Option<String>,
}

/// Status of the overall parallel execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ParallelStatus {
    /// Not yet started.
    #[default]
    Pending,
    /// Currently executing.
    Running,
    /// All required branches completed successfully.
    Completed,
    /// Execution failed.
    Failed,
    /// Execution timed out.
    TimedOut,
    /// Execution cancelled.
    Cancelled,
}

impl Default for ParallelExecution {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Parallel Execution".to_string(),
            branches: Vec::new(),
            merge_strategy: MergeStrategy::default(),
            timeout_secs: 600,
            max_concurrent: None,
            fail_fast: true,
            status: ParallelStatus::default(),
            results: HashMap::new(),
            started_at: None,
            completed_at: None,
        }
    }
}

impl ParallelExecution {
    /// Create a new parallel execution.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Add a branch.
    pub fn with_branch(mut self, branch: Branch) -> Self {
        self.branches.push(branch);
        self
    }

    /// Set merge strategy.
    pub fn with_merge_strategy(mut self, strategy: MergeStrategy) -> Self {
        self.merge_strategy = strategy;
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, duration: Duration) -> Self {
        self.timeout_secs = duration.as_secs();
        self
    }

    /// Set max concurrent.
    pub fn with_max_concurrent(mut self, max: usize) -> Self {
        self.max_concurrent = Some(max);
        self
    }

    /// Disable fail-fast.
    pub fn no_fail_fast(mut self) -> Self {
        self.fail_fast = false;
        self
    }

    /// Start execution.
    pub fn start(&mut self) {
        self.status = ParallelStatus::Running;
        self.started_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Record a branch result.
    pub fn record_result(&mut self, result: BranchResult) {
        let branch_id = result.branch_id.clone();

        // Update branch status
        if let Some(branch) = self.branches.iter_mut().find(|b| b.id == branch_id) {
            branch.status = if result.success {
                BranchStatus::Completed
            } else {
                BranchStatus::Failed
            };
        }

        self.results.insert(branch_id, result);
    }

    /// Check if execution should continue.
    pub fn should_continue(&self) -> bool {
        if self.status != ParallelStatus::Running {
            return false;
        }

        // Check fail-fast condition
        if self.fail_fast {
            let has_critical_failure = self
                .branches
                .iter()
                .any(|b| b.critical && b.status == BranchStatus::Failed);
            if has_critical_failure {
                return false;
            }
        }

        // Check if merge condition is satisfied
        let results: Vec<_> = self.results.values().collect();
        if self
            .merge_strategy
            .is_satisfied(&results, self.branches.len())
        {
            return false;
        }

        // Check if all branches are finished
        let all_finished = self.branches.iter().all(|b| b.is_finished());
        !all_finished
    }

    /// Get branches that can start.
    pub fn get_ready_branches(&self) -> Vec<&Branch> {
        let completed: Vec<_> = self
            .branches
            .iter()
            .filter(|b| b.status == BranchStatus::Completed)
            .map(|b| b.id.as_str())
            .collect();

        let running_count = self
            .branches
            .iter()
            .filter(|b| b.status == BranchStatus::Running)
            .count();

        let max_concurrent = self.max_concurrent.unwrap_or(usize::MAX);
        let available_slots = max_concurrent.saturating_sub(running_count);

        self.branches
            .iter()
            .filter(|b| b.status == BranchStatus::Pending && b.can_start(&completed))
            .take(available_slots)
            .collect()
    }

    /// Complete the execution.
    pub fn complete(&mut self, status: ParallelStatus) {
        self.status = status;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Get a summary of results.
    pub fn summary(&self) -> ParallelSummary {
        let successful = self.results.values().filter(|r| r.success).count();
        let failed = self.results.values().filter(|r| !r.success).count();
        let total_duration: u64 = self.results.values().map(|r| r.duration_ms).sum();
        let total_findings: usize = self.results.values().map(|r| r.findings.len()).sum();

        ParallelSummary {
            total_branches: self.branches.len(),
            completed: self.results.len(),
            successful,
            failed,
            pending: self
                .branches
                .iter()
                .filter(|b| b.status == BranchStatus::Pending)
                .count(),
            running: self
                .branches
                .iter()
                .filter(|b| b.status == BranchStatus::Running)
                .count(),
            total_duration_ms: total_duration,
            total_findings,
        }
    }

    /// Merge all branch outputs into a single map.
    pub fn merge_outputs(&self) -> HashMap<String, serde_json::Value> {
        let mut merged = HashMap::new();
        for (branch_id, result) in &self.results {
            for (key, value) in &result.outputs {
                merged.insert(format!("{}.{}", branch_id, key), value.clone());
            }
        }
        merged
    }
}

/// Summary of parallel execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelSummary {
    pub total_branches: usize,
    pub completed: usize,
    pub successful: usize,
    pub failed: usize,
    pub pending: usize,
    pub running: usize,
    pub total_duration_ms: u64,
    pub total_findings: usize,
}

// ============================================================================
// Parallel Execution Builder
// ============================================================================

/// Builder for common parallel execution patterns.
pub struct ParallelBuilder;

impl ParallelBuilder {
    /// Create a fan-out pattern (one input, multiple parallel outputs).
    pub fn fan_out(name: &str, branches: Vec<(&str, &str)>) -> ParallelExecution {
        let mut exec = ParallelExecution::new().with_name(name);
        for (id, domain) in branches {
            exec = exec.with_branch(Branch::new(id, domain));
        }
        exec
    }

    /// Create a pipeline pattern (branches execute in sequence).
    pub fn pipeline(name: &str, stages: Vec<(&str, &str)>) -> ParallelExecution {
        let mut exec = ParallelExecution::new()
            .with_name(name)
            .with_max_concurrent(1);

        let mut prev_id: Option<String> = None;
        for (id, domain) in stages {
            let mut branch = Branch::new(id, domain);
            if let Some(prev) = &prev_id {
                branch = branch.depends_on(prev.clone());
            }
            prev_id = Some(id.to_string());
            exec = exec.with_branch(branch);
        }
        exec
    }

    /// Create a race pattern (first success wins).
    pub fn race(name: &str, branches: Vec<(&str, &str)>) -> ParallelExecution {
        let mut exec = ParallelExecution::new()
            .with_name(name)
            .with_merge_strategy(MergeStrategy::Race);

        for (id, domain) in branches {
            exec = exec.with_branch(Branch::new(id, domain));
        }
        exec
    }

    /// Create a quorum pattern (need N successes).
    pub fn quorum(name: &str, branches: Vec<(&str, &str)>, threshold: f32) -> ParallelExecution {
        let mut exec = ParallelExecution::new()
            .with_name(name)
            .with_merge_strategy(MergeStrategy::Quorum { threshold });

        for (id, domain) in branches {
            exec = exec.with_branch(Branch::new(id, domain));
        }
        exec
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_branch_creation() {
        let branch = Branch::new("test", "analysis")
            .with_name("Test Branch")
            .with_prompt("Analyze the code")
            .with_priority(100)
            .critical();

        assert_eq!(branch.id, "test");
        assert_eq!(branch.name, "Test Branch");
        assert_eq!(branch.domain, "analysis");
        assert_eq!(branch.priority, 100);
        assert!(branch.critical);
    }

    #[test]
    fn test_branch_dependencies() {
        let branch = Branch::new("b", "test").depends_on("a");

        assert!(!branch.can_start(&[]));
        assert!(branch.can_start(&["a"]));
    }

    #[test]
    fn test_parallel_execution_creation() {
        let exec = ParallelExecution::new()
            .with_name("Test Execution")
            .with_branch(Branch::new("a", "domain_a"))
            .with_branch(Branch::new("b", "domain_b"))
            .with_merge_strategy(MergeStrategy::WaitAll);

        assert_eq!(exec.branches.len(), 2);
        assert!(matches!(exec.merge_strategy, MergeStrategy::WaitAll));
    }

    #[test]
    fn test_merge_strategy_wait_all() {
        let strategy = MergeStrategy::WaitAll;
        let r1 = BranchResult::success("a");
        let r2 = BranchResult::success("b");

        assert!(!strategy.is_satisfied(&[&r1], 2));
        assert!(strategy.is_satisfied(&[&r1, &r2], 2));
    }

    #[test]
    fn test_merge_strategy_wait_any() {
        let strategy = MergeStrategy::WaitAny;
        let success = BranchResult::success("a");
        let failure = BranchResult::failure("b", "error");

        assert!(strategy.is_satisfied(&[&success], 2));
        assert!(!strategy.is_satisfied(&[&failure], 2));
    }

    #[test]
    fn test_merge_strategy_quorum() {
        let strategy = MergeStrategy::Quorum { threshold: 0.5 };
        let r1 = BranchResult::success("a");
        let r2 = BranchResult::success("b");
        let r3 = BranchResult::failure("c", "error");

        assert!(!strategy.is_satisfied(&[&r1], 4));
        assert!(strategy.is_satisfied(&[&r1, &r2], 4));
        assert!(strategy.is_satisfied(&[&r1, &r2, &r3], 4));
    }

    #[test]
    fn test_get_ready_branches() {
        let mut exec = ParallelExecution::new()
            .with_branch(Branch::new("a", "d"))
            .with_branch(Branch::new("b", "d").depends_on("a"));

        exec.start();

        let ready = exec.get_ready_branches();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "a");
    }

    #[test]
    fn test_parallel_summary() {
        let mut exec = ParallelExecution::new()
            .with_branch(Branch::new("a", "d"))
            .with_branch(Branch::new("b", "d"));

        exec.start();
        exec.record_result(BranchResult::success("a").with_duration(1000));
        exec.record_result(BranchResult::failure("b", "error").with_duration(500));

        let summary = exec.summary();
        assert_eq!(summary.total_branches, 2);
        assert_eq!(summary.completed, 2);
        assert_eq!(summary.successful, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.total_duration_ms, 1500);
    }

    #[test]
    fn test_fan_out_pattern() {
        let exec = ParallelBuilder::fan_out("test", vec![("a", "d1"), ("b", "d2"), ("c", "d3")]);

        assert_eq!(exec.branches.len(), 3);
        // All branches should be independent
        assert!(exec.branches.iter().all(|b| b.depends_on.is_empty()));
    }

    #[test]
    fn test_pipeline_pattern() {
        let exec = ParallelBuilder::pipeline("test", vec![("a", "d1"), ("b", "d2"), ("c", "d3")]);

        assert_eq!(exec.branches.len(), 3);
        assert!(exec.branches[0].depends_on.is_empty());
        assert_eq!(exec.branches[1].depends_on, vec!["a"]);
        assert_eq!(exec.branches[2].depends_on, vec!["b"]);
    }

    #[test]
    fn test_max_concurrent() {
        let mut exec = ParallelExecution::new()
            .with_branch(Branch::new("a", "d"))
            .with_branch(Branch::new("b", "d"))
            .with_branch(Branch::new("c", "d"))
            .with_max_concurrent(2);

        exec.start();

        let ready = exec.get_ready_branches();
        assert_eq!(ready.len(), 2); // Should only get 2 due to max_concurrent
    }
}
