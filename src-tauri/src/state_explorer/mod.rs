//! State Explorer module
//!
//! This module implements an AI-driven state exploration system that traverses
//! application states using the state machine structure and verifies that
//! reality matches the described expectations.
//!
//! # Architecture
//!
//! The state explorer:
//! 1. Loads a configuration file with states, transitions, and descriptions
//! 2. Uses exploration strategies to traverse the state machine
//! 3. At each state, captures screenshots and logs
//! 4. Compares actual state to expected descriptions
//! 5. Reports discrepancies for AI analysis
//!
//! # Exploration Strategies
//!
//! - **Exhaustive**: Visit every state and transition
//! - **SmokeTest**: Quick path through critical states
//! - **Regression**: Focus on previously-failed areas
//! - **RandomWalk**: Discover unexpected behaviors
//! - **Targeted**: Verify specific states/transitions

mod ai_context;
mod assertions;
mod baseline;
mod checkpoint;
mod dependency;
mod depth;
mod exploration;
mod exploration_task;
mod report;
mod suggestions;
mod types;

// Re-export types that are used by commands and other modules
pub use exploration::{ExplorationStrategy, StateExplorer, StateMachineGraph};
pub use exploration_task::ExplorationTask;
pub use types::{ExplorationConfig, ExplorationResult, ExplorationStatus};

// AI analysis context for description-based verification
pub use ai_context::{
    ExplorationAnalysisContext, ExplorationSummary, StateAnalysisContext, StateObservations,
    TimingInfo, TransitionAnalysisContext,
};

// Checkpoint system for interleaved exploration
pub use checkpoint::{
    CheckpointConfig, CheckpointManager, CheckpointStatus, CheckpointTrigger,
    ExplorationCheckpoint,
};

// Explicit assertions for states
pub use assertions::{AssertionResult, AssertionType, StateAssertion};

// Dependency graph for intelligent exploration ordering
pub use dependency::{DependencyEdge, DependencyGraph, StateNode};

// Exploration depth modes and critical path finding
pub use depth::{CriticalPathFinder, DepthConfig, ExplorationDepth, ScreenshotMode};

// Baseline comparison for diff-based exploration
pub use baseline::{
    BaselineDiff, BaselineManager, ChangeType, ExplorationBaseline, StateDiff, StateSignature,
    TransitionDiff, TransitionSignature,
};

// AI-suggested assertions
pub use suggestions::{
    suggestions_to_markdown, AssertionSuggester, SuggestedAssertion, SuggestionCategory,
    SuggestionConfig, SuggestionSet,
};

// These exports are available for external use
pub use exploration::{ExplorationPath, StateVisit};
pub use report::{
    DiscrepancyType, ExplorationReport, StateExplorationReport, TransitionExplorationReport,
};
pub use types::{
    ExplorationPriority, StateExplorationResult, TransitionExplorationResult, VerificationConfig,
};
