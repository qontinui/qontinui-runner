//! Unified Orchestrator Configuration
//!
//! This module provides a comprehensive configuration system for the orchestrator,
//! including all 15 enhancement modules. Each module can be individually enabled
//! or disabled with its own configuration.
//!
//! # Configuration Hierarchy
//!
//! ```text
//! EnhancedOrchestratorConfig
//! ├── base: OrchestratorConfig          // Existing base config
//! ├── context: ContextConfig            // Context window management (#3)
//! ├── ledger: LedgerConfig              // Task ledger pattern (#2)
//! ├── layers: LayersConfig              // Middleware layers (#4)
//! ├── interrupt: InterruptConfig        // Human-in-the-loop (#5)
//! ├── memory: MemoryConfig              // Multi-tier memory (#6)
//! ├── roles: RolesConfig                // Agent roles (#7)
//! ├── channels: ChannelsConfig          // Typed state channels (#8)
//! ├── events: EventBusConfig            // Event bus (#9)
//! ├── subflows: SubflowsConfig          // Nested workflows (#10)

#![allow(dead_code)]
//! ├── parallel: ParallelConfig          // Parallel execution (#11)
//! ├── agent_tools: AgentToolsConfig     // Agent-as-tool (#12)
//! ├── checkpoint: CheckpointConfig      // Time-travel debugging (#13)
//! ├── flow: FlowConfig                  // Flow orchestration (#14)
//! └── learning: LearningConfig          // Learning loop (#15)
//! ```
//!
//! # Example
//!
//! ```rust
//! use crate::orchestrator::config::EnhancedOrchestratorConfig;
//!
//! // Default config with all features enabled
//! let config = EnhancedOrchestratorConfig::default();
//!
//! // Minimal config for simple tasks
//! let minimal = EnhancedOrchestratorConfig::minimal();
//!
//! // Production config with stability-focused settings
//! let production = EnhancedOrchestratorConfig::production();
//!
//! // Custom config via builder
//! let custom = EnhancedOrchestratorConfig::builder()
//!     .enable_parallel(true)
//!     .enable_checkpointing(true)
//!     .enable_learning(false)
//!     .build();
//! ```

use serde::{Deserialize, Serialize};

use super::checkpoint::AutoCheckpointSettings;
use super::context_strategies::ContextStrategy;
use super::learning::AnalysisConfig;
use super::parallel::MergeStrategy;

/// Unified configuration for the enhanced orchestrator.
///
/// This combines all feature configurations into a single structure
/// that can be serialized, stored, and loaded.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnhancedOrchestratorConfig {
    /// Base orchestrator configuration
    pub base: BaseConfig,
    /// Context window management configuration
    pub context: ContextConfig,
    /// Task ledger configuration
    pub ledger: LedgerConfig,
    /// Middleware layers configuration
    pub layers: LayersConfig,
    /// Interrupt/resume configuration
    pub interrupt: InterruptConfig,
    /// Multi-tier memory configuration
    pub memory: MemoryConfig,
    /// Agent roles configuration
    pub roles: RolesConfig,
    /// Typed state channels configuration
    pub channels: ChannelsConfig,
    /// Event bus configuration
    pub events: EventBusConfig,
    /// Subflows configuration
    pub subflows: SubflowsConfig,
    /// Parallel execution configuration
    pub parallel: ParallelConfig,
    /// Agent-as-tool configuration
    pub agent_tools: AgentToolsConfig,
    /// Checkpointing configuration
    pub checkpoint: CheckpointConfig,
    /// Flow orchestration configuration
    pub flow: FlowConfig,
    /// Learning loop configuration
    pub learning: LearningSystemConfig,
}

impl EnhancedOrchestratorConfig {
    /// Create a minimal configuration with only essential features.
    ///
    /// This is suitable for simple, single-iteration tasks where
    /// advanced features would add unnecessary overhead.
    pub fn minimal() -> Self {
        Self {
            base: BaseConfig::default(),
            context: ContextConfig {
                enabled: true,
                strategy: ContextStrategy::Unbounded,
                ..Default::default()
            },
            ledger: LedgerConfig {
                enabled: false,
                ..Default::default()
            },
            layers: LayersConfig {
                enabled: false,
                ..Default::default()
            },
            interrupt: InterruptConfig {
                enabled: false,
                ..Default::default()
            },
            memory: MemoryConfig {
                enabled: false,
                ..Default::default()
            },
            roles: RolesConfig {
                enabled: false,
                ..Default::default()
            },
            channels: ChannelsConfig {
                enabled: false,
                ..Default::default()
            },
            events: EventBusConfig {
                enabled: false,
                ..Default::default()
            },
            subflows: SubflowsConfig {
                enabled: false,
                ..Default::default()
            },
            parallel: ParallelConfig {
                enabled: false,
                ..Default::default()
            },
            agent_tools: AgentToolsConfig {
                enabled: false,
                ..Default::default()
            },
            checkpoint: CheckpointConfig {
                enabled: false,
                ..Default::default()
            },
            flow: FlowConfig {
                enabled: false,
                ..Default::default()
            },
            learning: LearningSystemConfig {
                enabled: false,
                ..Default::default()
            },
        }
    }

    /// Create a production-ready configuration with stability-focused settings.
    ///
    /// This enables observability, checkpointing, and error recovery
    /// while disabling experimental features like learning loops.
    pub fn production() -> Self {
        Self {
            base: BaseConfig {
                max_iterations: 15,
                ai_timeout_seconds: 600,
                ..Default::default()
            },
            context: ContextConfig {
                enabled: true,
                strategy: ContextStrategy::token_limited(100_000),
                ..Default::default()
            },
            ledger: LedgerConfig {
                enabled: true,
                stall_threshold_iterations: 5,
                ..Default::default()
            },
            layers: LayersConfig {
                enabled: true,
                enable_persistence: true,
                enable_observability: true,
                enable_limits: true,
                enable_debug: false, // Disabled in production
            },
            interrupt: InterruptConfig {
                enabled: true,
                require_approval_for_critical: true,
                ..Default::default()
            },
            memory: MemoryConfig {
                enabled: true,
                short_term_max_items: 100,
                long_term_enabled: true,
                entity_memory_enabled: true,
            },
            roles: RolesConfig {
                enabled: true,
                use_predefined_roles: true,
                ..Default::default()
            },
            channels: ChannelsConfig {
                enabled: true,
                use_standard_channels: true,
                ..Default::default()
            },
            events: EventBusConfig {
                enabled: true,
                enable_telemetry: true,
                enable_audit_log: true,
                ..Default::default()
            },
            subflows: SubflowsConfig {
                enabled: true,
                max_nesting_depth: 3,
                ..Default::default()
            },
            parallel: ParallelConfig {
                enabled: true,
                max_concurrent_branches: 4,
                default_merge_strategy: MergeStrategy::WaitAll,
                ..Default::default()
            },
            agent_tools: AgentToolsConfig {
                enabled: true,
                register_defaults: true,
                ..Default::default()
            },
            checkpoint: CheckpointConfig {
                enabled: true,
                auto_checkpoint: true,
                max_checkpoints: 50,
                checkpoint_on_verification: true,
                ..Default::default()
            },
            flow: FlowConfig {
                enabled: true,
                default_timeout_secs: 3600,
                ..Default::default()
            },
            learning: LearningSystemConfig {
                enabled: false, // Disabled in production until stabilized
                ..Default::default()
            },
        }
    }

    /// Create a builder for custom configuration.
    pub fn builder() -> EnhancedOrchestratorConfigBuilder {
        EnhancedOrchestratorConfigBuilder::new()
    }

    /// Check if any parallel features are enabled.
    pub fn is_parallel_enabled(&self) -> bool {
        self.parallel.enabled
    }

    /// Check if checkpointing is enabled.
    pub fn is_checkpointing_enabled(&self) -> bool {
        self.checkpoint.enabled
    }

    /// Check if learning is enabled.
    pub fn is_learning_enabled(&self) -> bool {
        self.learning.enabled
    }

    /// Check if flow mode is available.
    pub fn is_flow_mode_available(&self) -> bool {
        self.flow.enabled
    }

    /// Get the context strategy.
    pub fn context_strategy(&self) -> &ContextStrategy {
        &self.context.strategy
    }
}

// ============================================================================
// Base Configuration
// ============================================================================

/// Base orchestrator configuration (mirrors existing OrchestratorConfig).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseConfig {
    /// Maximum iterations before stopping
    pub max_iterations: u32,
    /// Timeout for AI calls in seconds
    pub ai_timeout_seconds: u64,
    /// Working directory for commands
    pub working_directory: String,
    /// Enable planning phase
    pub enable_planning: bool,
    /// Enable AI verification
    pub enable_ai_verification: bool,
    /// Run verification before first iteration
    pub run_initial_verification: bool,
}

impl Default for BaseConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            ai_timeout_seconds: 300,
            working_directory: ".".to_string(),
            enable_planning: true,
            enable_ai_verification: true,
            run_initial_verification: false,
        }
    }
}

// ============================================================================
// Feature Configurations
// ============================================================================

/// Context window management configuration (#3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Enable context management
    pub enabled: bool,
    /// Context strategy to use
    pub strategy: ContextStrategy,
    /// Maximum tokens (for token-limited strategy)
    pub max_tokens: usize,
    /// Enable semantic pruning
    pub enable_semantic_pruning: bool,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strategy: ContextStrategy::token_limited(50_000),
            max_tokens: 50_000,
            enable_semantic_pruning: false,
        }
    }
}

/// Task ledger configuration (#2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerConfig {
    /// Enable ledger pattern
    pub enabled: bool,
    /// Iterations before stall detection triggers
    pub stall_threshold_iterations: u32,
    /// Enable automatic replanning on stall
    pub auto_replan_on_stall: bool,
    /// Track discovered facts
    pub track_facts: bool,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stall_threshold_iterations: 3,
            auto_replan_on_stall: true,
            track_facts: true,
        }
    }
}

/// Middleware layers configuration (#4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayersConfig {
    /// Enable layers system
    pub enabled: bool,
    /// Enable persistence layer
    pub enable_persistence: bool,
    /// Enable observability layer
    pub enable_observability: bool,
    /// Enable execution limits layer
    pub enable_limits: bool,
    /// Enable debug logging layer
    pub enable_debug: bool,
}

impl Default for LayersConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enable_persistence: true,
            enable_observability: true,
            enable_limits: true,
            enable_debug: false,
        }
    }
}

/// Interrupt/resume configuration (#5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptConfig {
    /// Enable interrupt/resume protocol
    pub enabled: bool,
    /// Require approval for critical decisions
    pub require_approval_for_critical: bool,
    /// Timeout for human response in seconds
    pub human_response_timeout_secs: Option<u64>,
    /// Auto-resume with default on timeout
    pub auto_resume_on_timeout: bool,
}

impl Default for InterruptConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_approval_for_critical: true,
            human_response_timeout_secs: None,
            auto_resume_on_timeout: false,
        }
    }
}

/// Multi-tier memory configuration (#6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Enable memory system
    pub enabled: bool,
    /// Maximum items in short-term memory
    pub short_term_max_items: usize,
    /// Enable long-term memory persistence
    pub long_term_enabled: bool,
    /// Enable entity memory
    pub entity_memory_enabled: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            short_term_max_items: 50,
            long_term_enabled: true,
            entity_memory_enabled: true,
        }
    }
}

/// Agent roles configuration (#7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolesConfig {
    /// Enable agent roles
    pub enabled: bool,
    /// Use predefined roles
    pub use_predefined_roles: bool,
    /// Custom role definitions
    pub custom_roles: Vec<String>,
}

impl Default for RolesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            use_predefined_roles: true,
            custom_roles: Vec::new(),
        }
    }
}

/// Typed state channels configuration (#8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelsConfig {
    /// Enable typed channels
    pub enabled: bool,
    /// Use standard channels (findings, errors, etc.)
    pub use_standard_channels: bool,
    /// Enable version tracking
    pub enable_versioning: bool,
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            use_standard_channels: true,
            enable_versioning: true,
        }
    }
}

/// Event bus configuration (#9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBusConfig {
    /// Enable event bus
    pub enabled: bool,
    /// Enable telemetry handler
    pub enable_telemetry: bool,
    /// Enable audit log handler
    pub enable_audit_log: bool,
    /// Enable webhook handler
    pub enable_webhooks: bool,
    /// Webhook URLs
    pub webhook_urls: Vec<String>,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enable_telemetry: true,
            enable_audit_log: false,
            enable_webhooks: false,
            webhook_urls: Vec::new(),
        }
    }
}

/// Subflows configuration (#10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubflowsConfig {
    /// Enable subflows
    pub enabled: bool,
    /// Maximum nesting depth
    pub max_nesting_depth: u32,
    /// Enable subflow library
    pub enable_library: bool,
}

impl Default for SubflowsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_nesting_depth: 5,
            enable_library: true,
        }
    }
}

/// Parallel execution configuration (#11).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelConfig {
    /// Enable parallel execution
    pub enabled: bool,
    /// Maximum concurrent branches
    pub max_concurrent_branches: usize,
    /// Default merge strategy
    pub default_merge_strategy: MergeStrategy,
    /// Timeout for parallel execution in seconds
    pub timeout_secs: Option<u64>,
}

impl Default for ParallelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_concurrent_branches: 4,
            default_merge_strategy: MergeStrategy::WaitAll,
            timeout_secs: None,
        }
    }
}

/// Agent-as-tool configuration (#12).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolsConfig {
    /// Enable agent-as-tool pattern
    pub enabled: bool,
    /// Register default tools
    pub register_defaults: bool,
    /// Maximum agent tool depth
    pub max_depth: u32,
}

impl Default for AgentToolsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            register_defaults: true,
            max_depth: 3,
        }
    }
}

/// Checkpointing configuration (#13).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointConfig {
    /// Enable checkpointing
    pub enabled: bool,
    /// Auto-checkpoint settings
    pub auto_checkpoint: bool,
    /// Maximum checkpoints to retain
    pub max_checkpoints: usize,
    /// Checkpoint on verification boundaries
    pub checkpoint_on_verification: bool,
    /// Checkpoint every N iterations
    pub checkpoint_every_n_iterations: Option<u32>,
    /// Enable replay/time-travel
    pub enable_replay: bool,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_checkpoint: true,
            max_checkpoints: 20,
            checkpoint_on_verification: true,
            checkpoint_every_n_iterations: Some(5),
            enable_replay: true,
        }
    }
}

impl From<CheckpointConfig> for AutoCheckpointSettings {
    fn from(config: CheckpointConfig) -> Self {
        AutoCheckpointSettings {
            every_n_iterations: config.checkpoint_every_n_iterations,
            on_state_change: true,
            before_verification: config.checkpoint_on_verification,
            on_failure: true,
        }
    }
}

/// Flow orchestration configuration (#14).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowConfig {
    /// Enable flow orchestration mode
    pub enabled: bool,
    /// Default timeout for flows in seconds
    pub default_timeout_secs: u64,
    /// Enable flow library
    pub enable_library: bool,
}

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_timeout_secs: 3600,
            enable_library: true,
        }
    }
}

/// Learning loop configuration (#15).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningSystemConfig {
    /// Enable learning loop
    pub enabled: bool,
    /// Minimum pattern confidence
    pub min_pattern_confidence: f64,
    /// Minimum occurrences for pattern recognition
    pub min_pattern_occurrences: u32,
    /// Enable post-task analysis
    pub enable_post_task_analysis: bool,
    /// Apply learnings automatically
    pub auto_apply_learnings: bool,
}

impl Default for LearningSystemConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_pattern_confidence: 0.3,
            min_pattern_occurrences: 3,
            enable_post_task_analysis: true,
            auto_apply_learnings: false,
        }
    }
}

impl From<LearningSystemConfig> for AnalysisConfig {
    fn from(config: LearningSystemConfig) -> Self {
        AnalysisConfig {
            min_pattern_confidence: config.min_pattern_confidence,
            min_pattern_occurrences: config.min_pattern_occurrences,
            ..Default::default()
        }
    }
}

// ============================================================================
// Builder
// ============================================================================

/// Builder for EnhancedOrchestratorConfig.
#[derive(Debug, Clone)]
pub struct EnhancedOrchestratorConfigBuilder {
    config: EnhancedOrchestratorConfig,
}

impl EnhancedOrchestratorConfigBuilder {
    pub fn new() -> Self {
        Self {
            config: EnhancedOrchestratorConfig::default(),
        }
    }

    /// Set base configuration.
    pub fn base(mut self, base: BaseConfig) -> Self {
        self.config.base = base;
        self
    }

    /// Set maximum iterations.
    pub fn max_iterations(mut self, max: u32) -> Self {
        self.config.base.max_iterations = max;
        self
    }

    /// Set AI timeout.
    pub fn ai_timeout(mut self, secs: u64) -> Self {
        self.config.base.ai_timeout_seconds = secs;
        self
    }

    /// Set working directory.
    pub fn working_directory(mut self, dir: impl Into<String>) -> Self {
        self.config.base.working_directory = dir.into();
        self
    }

    /// Enable or disable planning.
    pub fn enable_planning(mut self, enable: bool) -> Self {
        self.config.base.enable_planning = enable;
        self
    }

    /// Enable or disable AI verification.
    pub fn enable_ai_verification(mut self, enable: bool) -> Self {
        self.config.base.enable_ai_verification = enable;
        self
    }

    /// Set context strategy.
    pub fn context_strategy(mut self, strategy: ContextStrategy) -> Self {
        self.config.context.strategy = strategy;
        self
    }

    /// Enable or disable ledger.
    pub fn enable_ledger(mut self, enable: bool) -> Self {
        self.config.ledger.enabled = enable;
        self
    }

    /// Enable or disable layers.
    pub fn enable_layers(mut self, enable: bool) -> Self {
        self.config.layers.enabled = enable;
        self
    }

    /// Enable or disable interrupt protocol.
    pub fn enable_interrupt(mut self, enable: bool) -> Self {
        self.config.interrupt.enabled = enable;
        self
    }

    /// Enable or disable memory system.
    pub fn enable_memory(mut self, enable: bool) -> Self {
        self.config.memory.enabled = enable;
        self
    }

    /// Enable or disable agent roles.
    pub fn enable_roles(mut self, enable: bool) -> Self {
        self.config.roles.enabled = enable;
        self
    }

    /// Enable or disable typed channels.
    pub fn enable_channels(mut self, enable: bool) -> Self {
        self.config.channels.enabled = enable;
        self
    }

    /// Enable or disable event bus.
    pub fn enable_events(mut self, enable: bool) -> Self {
        self.config.events.enabled = enable;
        self
    }

    /// Enable or disable subflows.
    pub fn enable_subflows(mut self, enable: bool) -> Self {
        self.config.subflows.enabled = enable;
        self
    }

    /// Enable or disable parallel execution.
    pub fn enable_parallel(mut self, enable: bool) -> Self {
        self.config.parallel.enabled = enable;
        self
    }

    /// Set maximum concurrent branches.
    pub fn max_concurrent_branches(mut self, max: usize) -> Self {
        self.config.parallel.max_concurrent_branches = max;
        self
    }

    /// Enable or disable agent tools.
    pub fn enable_agent_tools(mut self, enable: bool) -> Self {
        self.config.agent_tools.enabled = enable;
        self
    }

    /// Enable or disable checkpointing.
    pub fn enable_checkpointing(mut self, enable: bool) -> Self {
        self.config.checkpoint.enabled = enable;
        self
    }

    /// Enable or disable flow mode.
    pub fn enable_flow(mut self, enable: bool) -> Self {
        self.config.flow.enabled = enable;
        self
    }

    /// Enable or disable learning.
    pub fn enable_learning(mut self, enable: bool) -> Self {
        self.config.learning.enabled = enable;
        self
    }

    /// Build the configuration.
    pub fn build(self) -> EnhancedOrchestratorConfig {
        self.config
    }
}

impl Default for EnhancedOrchestratorConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Presets
// ============================================================================

/// Configuration presets for common use cases.
pub struct ConfigPresets;

impl ConfigPresets {
    /// Configuration for debugging/development.
    pub fn development() -> EnhancedOrchestratorConfig {
        EnhancedOrchestratorConfig::builder()
            .enable_checkpointing(true)
            .enable_layers(true)
            .enable_learning(true)
            .build()
    }

    /// Configuration for fast, simple tasks.
    pub fn fast() -> EnhancedOrchestratorConfig {
        EnhancedOrchestratorConfig::minimal()
    }

    /// Configuration for complex, multi-agent tasks.
    pub fn multi_agent() -> EnhancedOrchestratorConfig {
        EnhancedOrchestratorConfig::builder()
            .enable_parallel(true)
            .max_concurrent_branches(8)
            .enable_roles(true)
            .enable_agent_tools(true)
            .enable_channels(true)
            .enable_events(true)
            .build()
    }

    /// Configuration for long-running autonomous tasks.
    pub fn autonomous() -> EnhancedOrchestratorConfig {
        EnhancedOrchestratorConfig::builder()
            .max_iterations(50)
            .enable_checkpointing(true)
            .enable_learning(true)
            .enable_ledger(true)
            .enable_memory(true)
            .context_strategy(ContextStrategy::token_limited(100_000))
            .build()
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
        let config = EnhancedOrchestratorConfig::default();
        assert!(config.context.enabled);
        assert!(config.ledger.enabled);
        assert!(config.layers.enabled);
        assert_eq!(config.base.max_iterations, 10);
    }

    #[test]
    fn test_minimal_config() {
        let config = EnhancedOrchestratorConfig::minimal();
        assert!(config.context.enabled);
        assert!(!config.ledger.enabled);
        assert!(!config.parallel.enabled);
        assert!(!config.learning.enabled);
    }

    #[test]
    fn test_production_config() {
        let config = EnhancedOrchestratorConfig::production();
        assert!(config.checkpoint.enabled);
        assert!(config.events.enable_audit_log);
        assert!(!config.layers.enable_debug);
        assert!(!config.learning.enabled);
    }

    #[test]
    fn test_builder() {
        let config = EnhancedOrchestratorConfig::builder()
            .max_iterations(20)
            .enable_parallel(true)
            .max_concurrent_branches(8)
            .enable_learning(false)
            .build();

        assert_eq!(config.base.max_iterations, 20);
        assert!(config.parallel.enabled);
        assert_eq!(config.parallel.max_concurrent_branches, 8);
        assert!(!config.learning.enabled);
    }

    #[test]
    fn test_presets() {
        let dev = ConfigPresets::development();
        assert!(dev.checkpoint.enabled);
        assert!(dev.learning.enabled);

        let fast = ConfigPresets::fast();
        assert!(!fast.parallel.enabled);

        let multi = ConfigPresets::multi_agent();
        assert!(multi.parallel.enabled);
        assert_eq!(multi.parallel.max_concurrent_branches, 8);

        let auto = ConfigPresets::autonomous();
        assert_eq!(auto.base.max_iterations, 50);
        assert!(auto.learning.enabled);
    }

    #[test]
    fn test_helper_methods() {
        let config = EnhancedOrchestratorConfig::default();
        assert!(config.is_parallel_enabled());
        assert!(config.is_checkpointing_enabled());
        assert!(config.is_learning_enabled());
        assert!(config.is_flow_mode_available());
    }

    #[test]
    fn test_config_serialization() {
        let config = EnhancedOrchestratorConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let restored: EnhancedOrchestratorConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(config.base.max_iterations, restored.base.max_iterations);
        assert_eq!(
            config.parallel.max_concurrent_branches,
            restored.parallel.max_concurrent_branches
        );
    }

    #[test]
    fn test_checkpoint_config_conversion() {
        let config = CheckpointConfig {
            enabled: true,
            auto_checkpoint: true,
            max_checkpoints: 30,
            checkpoint_on_verification: true,
            checkpoint_every_n_iterations: Some(3),
            enable_replay: true,
        };

        let auto_settings: AutoCheckpointSettings = config.into();
        assert_eq!(auto_settings.every_n_iterations, Some(3));
        assert!(auto_settings.before_verification);
        assert!(auto_settings.on_failure);
    }
}
