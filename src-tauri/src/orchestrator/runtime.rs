//! Integrated Orchestrator Runtime
//!
//! This module provides the main runtime that integrates all 15 orchestrator
//! enhancement modules into a cohesive execution environment.
//!
//! # Architecture
//!
//! The runtime coordinates all modules:
//! - Tier 1: Structured output, ledger, context strategies, layers, interrupt
//! - Tier 2: Memory, roles, channels, event bus, subflows
//! - Tier 3: Parallel execution, agent tools, checkpointing, flow mode, learning

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::agent_roles::{AgentRole, RoleRegistry};
use super::agent_tool::{AgentTool, ToolRegistry};
use super::checkpoint::{Checkpoint, CheckpointManager, StateSnapshot, VerificationSnapshot};
use super::config::EnhancedOrchestratorConfig;
use super::context_strategies::ContextStrategy;
use super::event_bus::EventBus;
use super::flow::{Flow, FlowState};
use super::interrupt::InterruptManager;
use super::learning::{LearningSystem, TaskOutcome};
use super::ledger::TaskLedger;
use super::memory::MemorySystem;
use super::parallel::{MergeStrategy, ParallelExecution};
use super::subflows::{SubflowDef, SubflowLibrary};
use super::typed_channels::SharedChannelState;

/// The integrated orchestrator runtime.
///
/// This brings together all 15 enhancement modules into a single
/// coordinated runtime environment.
pub struct OrchestratorRuntime {
    /// Configuration
    pub config: EnhancedOrchestratorConfig,
    /// Runtime state
    state: RuntimeState,
    /// Module instances
    modules: RuntimeModules,
}

/// Runtime state tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeState {
    /// Current task ID
    pub task_id: Option<String>,
    /// Current iteration
    pub iteration: u32,
    /// Whether currently in flow mode
    pub in_flow_mode: bool,
    /// Current flow state (if in flow mode)
    pub flow_state: Option<FlowState>,
    /// Active interrupts
    pub active_interrupts: Vec<String>,
    /// Running parallel executions
    pub parallel_executions: HashMap<String, ParallelExecutionState>,
    /// Current status
    pub status: RuntimeStatus,
    /// Timestamp when runtime was created
    pub created_at: String,
    /// Timestamp of last activity
    pub last_activity_at: String,
}

impl Default for RuntimeState {
    fn default() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            task_id: None,
            iteration: 0,
            in_flow_mode: false,
            flow_state: None,
            active_interrupts: Vec::new(),
            parallel_executions: HashMap::new(),
            status: RuntimeStatus::Idle,
            created_at: now.clone(),
            last_activity_at: now,
        }
    }
}

/// Parallel execution state tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelExecutionState {
    pub execution_id: String,
    pub total_branches: usize,
    pub completed_branches: usize,
    pub failed_branches: usize,
    pub merge_strategy: MergeStrategy,
}

/// Runtime status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RuntimeStatus {
    #[default]
    Idle,
    Initializing,
    Running,
    Paused,
    WaitingForInput,
    InParallelExecution,
    Checkpointing,
    Completed,
    Failed,
}

/// Container for all module instances.
pub struct RuntimeModules {
    /// Task ledger (#2)
    pub ledger: Option<TaskLedger>,
    /// Memory system (#6)
    pub memory: MemorySystem,
    /// Event bus (#9)
    pub event_bus: EventBus,
    /// Typed channels (#8)
    pub channels: SharedChannelState,
    /// Role registry (#7)
    pub roles: RoleRegistry,
    /// Tool registry (#12)
    pub tools: ToolRegistry,
    /// Checkpoint manager (#13)
    pub checkpoints: CheckpointManager,
    /// Interrupt manager (#5)
    pub interrupts: InterruptManager,
    /// Learning system (#15)
    pub learning: LearningSystem,
    /// Subflow library (#10)
    pub subflows: SubflowLibrary,
    /// Flow library (#14)
    pub flows: HashMap<String, Flow>,
}

impl Default for RuntimeModules {
    fn default() -> Self {
        Self {
            ledger: None,
            memory: MemorySystem::new(),
            event_bus: EventBus::new(),
            channels: SharedChannelState::new(),
            roles: RoleRegistry::with_defaults(),
            tools: ToolRegistry::with_defaults(),
            checkpoints: CheckpointManager::new(),
            interrupts: InterruptManager::new(),
            learning: LearningSystem::new(),
            subflows: SubflowLibrary::new(),
            flows: HashMap::new(),
        }
    }
}

impl OrchestratorRuntime {
    /// Create a new runtime with the given configuration.
    pub fn new(config: EnhancedOrchestratorConfig) -> Self {
        let mut modules = RuntimeModules::default();

        // Configure modules based on config
        if config.checkpoint.enabled {
            modules.checkpoints =
                CheckpointManager::new().with_max_per_task(config.checkpoint.max_checkpoints);
        }

        if config.learning.enabled {
            modules.learning = LearningSystem::with_config(config.learning.clone().into());
        }

        if config.roles.use_predefined_roles {
            modules.roles = RoleRegistry::with_defaults();
        }

        if config.agent_tools.register_defaults {
            modules.tools = ToolRegistry::with_defaults();
        }

        Self {
            config,
            state: RuntimeState::default(),
            modules,
        }
    }

    /// Create a runtime with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(EnhancedOrchestratorConfig::default())
    }

    /// Create a runtime optimized for production.
    pub fn production() -> Self {
        Self::new(EnhancedOrchestratorConfig::production())
    }

    /// Create a minimal runtime for simple tasks.
    pub fn minimal() -> Self {
        Self::new(EnhancedOrchestratorConfig::minimal())
    }

    // ========================================================================
    // Task Lifecycle
    // ========================================================================

    /// Initialize a new task.
    pub fn initialize_task(&mut self, task_id: &str, goal: &str) -> Result<(), String> {
        self.state.task_id = Some(task_id.to_string());
        self.state.iteration = 0;
        self.state.status = RuntimeStatus::Initializing;
        self.update_activity();

        // Initialize ledger if enabled
        if self.config.ledger.enabled {
            self.modules.ledger = Some(TaskLedger::new(goal));
        }

        // Create initial checkpoint if enabled
        if self.config.checkpoint.enabled && self.config.checkpoint.auto_checkpoint {
            let _ = self.checkpoint();
        }

        self.state.status = RuntimeStatus::Running;
        Ok(())
    }

    /// Start a new iteration.
    pub fn start_iteration(&mut self) -> Result<u32, String> {
        self.state.iteration += 1;
        self.update_activity();

        // Auto-checkpoint based on iteration count
        if self.config.checkpoint.enabled {
            if let Some(every_n) = self.config.checkpoint.checkpoint_every_n_iterations {
                if self.state.iteration % every_n == 0 {
                    let _ = self.checkpoint();
                }
            }
        }

        Ok(self.state.iteration)
    }

    /// Complete the current task.
    pub fn complete_task(&mut self, success: bool, reason: Option<&str>) -> Result<(), String> {
        self.update_activity();

        // Record outcome for learning
        if self.config.learning.enabled {
            let task_id = self.state.task_id.clone().unwrap_or_default();
            let outcome = if success {
                TaskOutcome::success(&task_id).with_iterations(self.state.iteration)
            } else {
                TaskOutcome::failure(&task_id)
                    .with_iterations(self.state.iteration)
                    .with_error(reason.unwrap_or("Unknown error"))
            };
            self.modules.learning.record_outcome(outcome);

            // Run post-task analysis if enabled
            if self.config.learning.enable_post_task_analysis {
                let _ = self.modules.learning.analyze_patterns();
            }
        }

        // Final checkpoint
        if self.config.checkpoint.enabled {
            let _ = self.checkpoint();
        }

        self.state.status = if success {
            RuntimeStatus::Completed
        } else {
            RuntimeStatus::Failed
        };

        Ok(())
    }

    // ========================================================================
    // Memory Operations (#6)
    // ========================================================================

    /// Store an observation in memory.
    pub fn remember_observation(&mut self, content: &str) {
        if self.config.memory.enabled {
            let item = super::memory::MemoryItem::observation(content);
            self.modules.memory.short_term.add(item);
        }
    }

    /// Store a decision in memory.
    pub fn remember_decision(&mut self, content: &str) {
        if self.config.memory.enabled {
            let item = super::memory::MemoryItem::decision(content);
            self.modules.memory.short_term.add(item);
        }
    }

    /// Get recent memory context.
    pub fn get_memory_context(&self, limit: usize) -> Vec<String> {
        if self.config.memory.enabled {
            self.modules
                .memory
                .short_term
                .get_recent(limit)
                .iter()
                .map(|i| i.content.clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Build full memory context for prompts.
    pub fn build_memory_context(&self) -> String {
        if self.config.memory.enabled {
            self.modules.memory.build_context(20)
        } else {
            String::new()
        }
    }

    // ========================================================================
    // Role Operations (#7)
    // ========================================================================

    /// Get a role by ID.
    pub fn get_role(&self, role_id: &str) -> Option<&AgentRole> {
        if self.config.roles.enabled {
            self.modules.roles.get(role_id)
        } else {
            None
        }
    }

    /// Register a custom role.
    pub fn register_role(&mut self, role: AgentRole) {
        if self.config.roles.enabled {
            self.modules.roles.register(role);
        }
    }

    /// Build a prompt with role context.
    pub fn build_role_prompt(&self, role_id: &str, base_prompt: &str) -> String {
        if self.config.roles.enabled {
            if let Some(role) = self.modules.roles.get(role_id) {
                format!(
                    "## Your Role\n\n**Role:** {}\n**Goal:** {}\n**Backstory:** {}\n\n---\n\n{}",
                    role.role, role.goal, role.backstory, base_prompt
                )
            } else {
                base_prompt.to_string()
            }
        } else {
            base_prompt.to_string()
        }
    }

    // ========================================================================
    // Tool Operations (#12)
    // ========================================================================

    /// Get a tool by ID.
    pub fn get_tool(&self, tool_id: &str) -> Option<&AgentTool> {
        if self.config.agent_tools.enabled {
            self.modules.tools.get(tool_id)
        } else {
            None
        }
    }

    /// Register a custom tool.
    pub fn register_tool(&mut self, tool: AgentTool) {
        if self.config.agent_tools.enabled {
            self.modules.tools.register(tool);
        }
    }

    // ========================================================================
    // Checkpoint Operations (#13)
    // ========================================================================

    /// Create a checkpoint.
    pub fn checkpoint(&mut self) -> Result<String, String> {
        if !self.config.checkpoint.enabled {
            return Err("Checkpointing is disabled".to_string());
        }

        self.state.status = RuntimeStatus::Checkpointing;

        // Create state snapshot
        let snapshot = StateSnapshot {
            state: format!("{:?}", self.state.status),
            iteration: self.state.iteration,
            channels: HashMap::new(),
            knowledge: Vec::new(),
            verification: VerificationSnapshot {
                criteria_results: HashMap::new(),
                overall_passed: false,
            },
            findings: Vec::new(),
            files_modified: Vec::new(),
            custom_data: HashMap::new(),
        };

        let checkpoint =
            Checkpoint::new(self.state.task_id.as_deref().unwrap_or("unknown"), snapshot);

        let checkpoint_id = self.modules.checkpoints.save(checkpoint);

        self.state.status = RuntimeStatus::Running;
        Ok(checkpoint_id)
    }

    /// List checkpoints for the current task.
    pub fn list_checkpoints(&self) -> Vec<super::checkpoint::CheckpointSummary> {
        if self.config.checkpoint.enabled {
            if let Some(task_id) = &self.state.task_id {
                self.modules.checkpoints.list_for_task(task_id)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }

    /// Restore from a checkpoint.
    pub fn restore_checkpoint(&mut self, checkpoint_id: &str) -> Result<(), String> {
        if !self.config.checkpoint.enabled {
            return Err("Checkpointing is disabled".to_string());
        }

        let checkpoint = self
            .modules
            .checkpoints
            .load(checkpoint_id)
            .ok_or_else(|| format!("Checkpoint '{}' not found", checkpoint_id))?;

        self.state.task_id = Some(checkpoint.task_id.clone());
        self.state.iteration = checkpoint.state.iteration;

        Ok(())
    }

    // ========================================================================
    // Parallel Execution Operations (#11)
    // ========================================================================

    /// Start a parallel execution.
    pub fn start_parallel_execution(
        &mut self,
        execution: ParallelExecution,
    ) -> Result<String, String> {
        if !self.config.parallel.enabled {
            return Err("Parallel execution is disabled".to_string());
        }

        let execution_id = execution.id.clone();
        let state = ParallelExecutionState {
            execution_id: execution_id.clone(),
            total_branches: execution.branches.len(),
            completed_branches: 0,
            failed_branches: 0,
            merge_strategy: execution.merge_strategy.clone(),
        };

        self.state
            .parallel_executions
            .insert(execution_id.clone(), state);
        self.state.status = RuntimeStatus::InParallelExecution;

        Ok(execution_id)
    }

    /// Complete a parallel execution.
    pub fn complete_parallel_execution(&mut self, execution_id: &str) -> Result<(), String> {
        self.state.parallel_executions.remove(execution_id);
        if self.state.parallel_executions.is_empty() {
            self.state.status = RuntimeStatus::Running;
        }
        Ok(())
    }

    // ========================================================================
    // Flow Operations (#14)
    // ========================================================================

    /// Register a flow.
    pub fn register_flow(&mut self, flow: Flow) {
        if self.config.flow.enabled {
            self.modules.flows.insert(flow.id.clone(), flow);
        }
    }

    /// Start a flow execution.
    pub fn start_flow(&mut self, flow_id: &str) -> Result<FlowState, String> {
        if !self.config.flow.enabled {
            return Err("Flow mode is disabled".to_string());
        }

        let flow = self
            .modules
            .flows
            .get(flow_id)
            .ok_or_else(|| format!("Flow '{}' not found", flow_id))?
            .clone();

        let flow_state = FlowState::new(&flow);
        self.state.in_flow_mode = true;
        self.state.flow_state = Some(flow_state.clone());

        Ok(flow_state)
    }

    /// Check if in flow mode.
    pub fn is_in_flow_mode(&self) -> bool {
        self.state.in_flow_mode
    }

    // ========================================================================
    // Learning Operations (#15)
    // ========================================================================

    /// Record a task outcome for learning.
    pub fn record_learning_outcome(&mut self, outcome: TaskOutcome) {
        if self.config.learning.enabled {
            self.modules.learning.record_outcome(outcome);
        }
    }

    /// Analyze patterns from completed tasks.
    pub fn analyze_patterns(&mut self) -> Option<super::learning::AnalysisResult> {
        if self.config.learning.enabled {
            Some(self.modules.learning.analyze_patterns())
        } else {
            None
        }
    }

    /// Get the best strategy based on learning.
    pub fn get_best_strategy(&self) -> Option<(String, f64)> {
        if self.config.learning.enabled {
            self.modules.learning.get_best_strategy()
        } else {
            None
        }
    }

    // ========================================================================
    // Subflow Operations (#10)
    // ========================================================================

    /// Register a subflow.
    pub fn register_subflow(&mut self, subflow: SubflowDef) {
        if self.config.subflows.enabled {
            self.modules.subflows.register(subflow);
        }
    }

    /// Get a subflow by ID.
    pub fn get_subflow(&self, subflow_id: &str) -> Option<&SubflowDef> {
        if self.config.subflows.enabled {
            self.modules.subflows.get(subflow_id)
        } else {
            None
        }
    }

    // ========================================================================
    // Context Operations (#3)
    // ========================================================================

    /// Apply context strategy to items.
    pub fn apply_context_strategy(
        &self,
        items: &[super::context_strategies::ContextItem],
    ) -> super::context_strategies::StrategyResult {
        self.config.context.strategy.apply(items)
    }

    /// Get current context strategy.
    pub fn context_strategy(&self) -> &ContextStrategy {
        &self.config.context.strategy
    }

    /// Check if context strategy is enabled.
    pub fn is_context_enabled(&self) -> bool {
        self.config.context.enabled
    }

    // ========================================================================
    // State Access
    // ========================================================================

    /// Get current runtime state.
    pub fn state(&self) -> &RuntimeState {
        &self.state
    }

    /// Get current task ID.
    pub fn task_id(&self) -> Option<&str> {
        self.state.task_id.as_deref()
    }

    /// Get current iteration.
    pub fn iteration(&self) -> u32 {
        self.state.iteration
    }

    /// Get current status.
    pub fn status(&self) -> RuntimeStatus {
        self.state.status
    }

    /// Check if runtime is active.
    pub fn is_active(&self) -> bool {
        matches!(
            self.state.status,
            RuntimeStatus::Running | RuntimeStatus::InParallelExecution
        )
    }

    fn update_activity(&mut self) {
        self.state.last_activity_at = chrono::Utc::now().to_rfc3339();
    }

    /// Get runtime summary for debugging.
    pub fn summary(&self) -> RuntimeSummary {
        RuntimeSummary {
            task_id: self.state.task_id.clone(),
            iteration: self.state.iteration,
            status: self.state.status,
            features_enabled: self.enabled_features(),
            active_interrupts: self.state.active_interrupts.len(),
            parallel_executions: self.state.parallel_executions.len(),
            learning_outcomes: self.modules.learning.outcomes.len(),
        }
    }

    /// List enabled features.
    pub fn enabled_features(&self) -> Vec<String> {
        let mut features = Vec::new();

        if self.config.ledger.enabled {
            features.push("ledger".to_string());
        }
        if self.config.context.enabled {
            features.push("context".to_string());
        }
        if self.config.layers.enabled {
            features.push("layers".to_string());
        }
        if self.config.interrupt.enabled {
            features.push("interrupt".to_string());
        }
        if self.config.memory.enabled {
            features.push("memory".to_string());
        }
        if self.config.roles.enabled {
            features.push("roles".to_string());
        }
        if self.config.channels.enabled {
            features.push("channels".to_string());
        }
        if self.config.events.enabled {
            features.push("events".to_string());
        }
        if self.config.subflows.enabled {
            features.push("subflows".to_string());
        }
        if self.config.parallel.enabled {
            features.push("parallel".to_string());
        }
        if self.config.agent_tools.enabled {
            features.push("agent_tools".to_string());
        }
        if self.config.checkpoint.enabled {
            features.push("checkpoint".to_string());
        }
        if self.config.flow.enabled {
            features.push("flow".to_string());
        }
        if self.config.learning.enabled {
            features.push("learning".to_string());
        }

        features
    }
}

/// Summary of runtime state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSummary {
    pub task_id: Option<String>,
    pub iteration: u32,
    pub status: RuntimeStatus,
    pub features_enabled: Vec<String>,
    pub active_interrupts: usize,
    pub parallel_executions: usize,
    pub learning_outcomes: usize,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let runtime = OrchestratorRuntime::with_defaults();
        assert_eq!(runtime.status(), RuntimeStatus::Idle);
        assert!(runtime.task_id().is_none());
    }

    #[test]
    fn test_runtime_minimal() {
        let runtime = OrchestratorRuntime::minimal();
        let features = runtime.enabled_features();
        assert!(features.contains(&"context".to_string()));
        assert!(!features.contains(&"parallel".to_string()));
    }

    #[test]
    fn test_task_lifecycle() {
        let mut runtime = OrchestratorRuntime::with_defaults();

        runtime.initialize_task("task-1", "Test goal").unwrap();
        assert_eq!(runtime.task_id(), Some("task-1"));
        assert_eq!(runtime.status(), RuntimeStatus::Running);

        runtime.start_iteration().unwrap();
        assert_eq!(runtime.iteration(), 1);

        runtime.complete_task(true, None).unwrap();
        assert_eq!(runtime.status(), RuntimeStatus::Completed);
    }

    #[test]
    fn test_memory_operations() {
        let mut runtime = OrchestratorRuntime::with_defaults();
        runtime.initialize_task("task-1", "Test").unwrap();

        runtime.remember_observation("Test fact");

        let context = runtime.get_memory_context(10);
        assert!(!context.is_empty());
    }

    #[test]
    fn test_checkpoint_operations() {
        let mut runtime = OrchestratorRuntime::with_defaults();
        runtime.initialize_task("task-1", "Test").unwrap();

        let checkpoint_id = runtime.checkpoint().unwrap();
        assert!(!checkpoint_id.is_empty());

        let checkpoints = runtime.list_checkpoints();
        assert!(!checkpoints.is_empty());
    }

    #[test]
    fn test_role_operations() {
        let runtime = OrchestratorRuntime::with_defaults();

        // Default roles should be available (debugger is one of the defaults)
        let debugger = runtime.get_role("debugger");
        assert!(debugger.is_some());

        let prompt = runtime.build_role_prompt("debugger", "Fix the bug");
        assert!(prompt.contains("Your Role"));
    }

    #[test]
    fn test_tool_operations() {
        let runtime = OrchestratorRuntime::with_defaults();

        // Default tools should be available
        let tool = runtime.get_tool("security_analyst");
        assert!(tool.is_some());
    }

    #[test]
    fn test_learning_operations() {
        let mut runtime = OrchestratorRuntime::with_defaults();

        let outcome = TaskOutcome::success("task-1")
            .with_iterations(5)
            .with_strategy("incremental");

        runtime.record_learning_outcome(outcome);

        let analysis = runtime.analyze_patterns();
        assert!(analysis.is_some());
    }

    #[test]
    fn test_summary() {
        let runtime = OrchestratorRuntime::with_defaults();
        let summary = runtime.summary();

        assert!(summary.task_id.is_none());
        assert_eq!(summary.iteration, 0);
        assert!(!summary.features_enabled.is_empty());
    }

    #[test]
    fn test_disabled_features() {
        let mut runtime = OrchestratorRuntime::minimal();

        // Checkpointing should be disabled
        let result = runtime.checkpoint();
        assert!(result.is_err());

        // Parallel execution should be disabled
        let parallel = ParallelExecution::new();
        let result = runtime.start_parallel_execution(parallel);
        assert!(result.is_err());
    }

    #[test]
    fn test_context_strategy() {
        use crate::orchestrator::context_strategies::{ContextItem, ItemCategory};

        let runtime = OrchestratorRuntime::with_defaults();

        // Create test context items
        let items: Vec<ContextItem> = (0..100)
            .map(|i| {
                ContextItem::new(
                    format!("item-{}", i),
                    "a".repeat(1000),
                    ItemCategory::WorkOutput,
                )
            })
            .collect();

        let result = runtime.apply_context_strategy(&items);

        // Strategy should process items and produce a result
        assert!(!result.strategy_used.is_empty());
    }
}
