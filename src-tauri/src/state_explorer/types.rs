//! Core types for the State Explorer
//!
//! These types define the configuration and results of exploration tasks.

use serde::{Deserialize, Serialize};

/// Priority level for exploration - determines order of state exploration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExplorationPriority {
    /// Critical states - must be explored first
    Critical,
    /// High priority states - explored after critical
    High,
    /// Normal priority - default
    #[default]
    Normal,
    /// Low priority - explored last if time permits
    Low,
}


/// Status of an exploration check
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExplorationStatus {
    /// Not yet explored
    #[default]
    Pending,
    /// Currently being explored
    InProgress,
    /// Exploration passed - reality matches expectation
    Passed,
    /// Exploration failed - discrepancy detected
    Failed,
    /// Exploration was skipped (e.g., state not reachable)
    Skipped,
    /// Error occurred during exploration
    Error,
    /// Exploration paused at checkpoint for agentic work
    Paused,
}


/// Configuration for an exploration task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationConfig {
    /// Path to the qontinui config file
    pub config_path: String,

    /// Exploration depth preset (quick_scan, standard, deep, exhaustive)
    /// When set, overrides max_states, stop_on_first_failure, and screenshot settings
    #[serde(default)]
    pub depth: Option<String>,

    /// Exploration strategy to use
    #[serde(default)]
    pub strategy: String, // "exhaustive", "smoke_test", "regression", "random_walk", "targeted"

    /// Maximum number of states to visit (0 = unlimited)
    /// Overridden by depth preset if set
    #[serde(default)]
    pub max_states: u32,

    /// Maximum time in seconds for the exploration run (0 = unlimited)
    #[serde(default)]
    pub max_duration_seconds: u64,

    /// Specific state IDs to explore (for targeted strategy)
    #[serde(default)]
    pub target_state_ids: Vec<String>,

    /// Specific transition IDs to explore (for targeted strategy)
    #[serde(default)]
    pub target_transition_ids: Vec<String>,

    /// Monitor index to use for exploration
    #[serde(default)]
    pub monitor_index: Option<i32>,

    /// Whether to capture screenshots at each state
    #[serde(default = "default_true")]
    pub capture_screenshots: bool,

    /// Whether to capture screenshots during transitions
    #[serde(default)]
    pub capture_transition_screenshots: bool,

    /// Delay in milliseconds between state visits
    #[serde(default = "default_state_delay")]
    pub state_delay_ms: u64,

    /// Directory to store exploration artifacts
    #[serde(default)]
    pub output_directory: Option<String>,

    /// Whether to stop on first failure
    #[serde(default)]
    pub stop_on_first_failure: bool,

    /// Random seed for reproducible random walks
    #[serde(default)]
    pub random_seed: Option<u64>,

    // Checkpoint configuration for interleaved exploration
    /// Number of states to explore before creating a checkpoint
    #[serde(default = "default_checkpoint_batch_size")]
    pub checkpoint_batch_size: usize,

    /// Number of issues to accumulate before pausing for agentic work
    #[serde(default = "default_checkpoint_issue_threshold")]
    pub checkpoint_issue_threshold: usize,

    /// Whether to pause on critical failures
    #[serde(default = "default_true")]
    pub checkpoint_on_critical: bool,

    /// Whether to interleave exploration with agentic work
    #[serde(default)]
    pub interleave_with_agentic: bool,
}

fn default_checkpoint_batch_size() -> usize {
    10
}

fn default_checkpoint_issue_threshold() -> usize {
    5
}

fn default_true() -> bool {
    true
}

fn default_state_delay() -> u64 {
    500 // 500ms between states
}

impl Default for ExplorationConfig {
    fn default() -> Self {
        Self {
            config_path: String::new(),
            depth: None,
            strategy: "exhaustive".to_string(),
            max_states: 0,
            max_duration_seconds: 0,
            target_state_ids: Vec::new(),
            target_transition_ids: Vec::new(),
            monitor_index: None,
            capture_screenshots: true,
            capture_transition_screenshots: false,
            state_delay_ms: default_state_delay(),
            output_directory: None,
            stop_on_first_failure: false,
            random_seed: None,
            checkpoint_batch_size: default_checkpoint_batch_size(),
            checkpoint_issue_threshold: default_checkpoint_issue_threshold(),
            checkpoint_on_critical: true,
            interleave_with_agentic: false,
        }
    }
}

impl ExplorationConfig {
    /// Apply depth preset to configuration
    /// This overrides relevant settings based on the depth level
    pub fn apply_depth_preset(&mut self) {
        if let Some(ref depth_str) = self.depth {
            let depth = super::depth::ExplorationDepth::from_str(depth_str);
            let preset = super::depth::DepthConfig::from(depth);

            self.max_states = preset.max_states;
            self.stop_on_first_failure = preset.stop_on_first_failure;
            self.capture_screenshots = preset.capture_screenshots;
            self.capture_transition_screenshots = preset.capture_transition_screenshots;
        }
    }

    /// Create config with depth preset applied
    pub fn with_depth(mut self, depth: &str) -> Self {
        self.depth = Some(depth.to_string());
        self.apply_depth_preset();
        self
    }
}

/// Configuration for verifying a specific state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationConfig {
    /// State ID being verified
    pub state_id: String,

    /// Priority of this verification
    #[serde(default)]
    pub priority: ExplorationPriority,

    /// Custom verification hints from the state description
    #[serde(default)]
    pub verification_hints: Vec<String>,

    /// Expected elements that should be visible
    #[serde(default)]
    pub expected_elements: Vec<String>,

    /// Elements that should NOT be visible
    #[serde(default)]
    pub unexpected_elements: Vec<String>,

    /// Maximum time to wait for state to be detected (ms)
    #[serde(default = "default_detect_timeout")]
    pub detect_timeout_ms: u64,
}

fn default_detect_timeout() -> u64 {
    10000 // 10 seconds
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            state_id: String::new(),
            priority: ExplorationPriority::Normal,
            verification_hints: Vec::new(),
            expected_elements: Vec::new(),
            unexpected_elements: Vec::new(),
            detect_timeout_ms: default_detect_timeout(),
        }
    }
}

/// Result of exploring a single state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateExplorationResult {
    /// State ID that was explored
    pub state_id: String,

    /// State name (for display)
    pub state_name: String,

    /// Exploration status
    pub status: ExplorationStatus,

    /// Path to the screenshot taken at this state
    pub screenshot_path: Option<String>,

    /// Time when exploration started
    pub started_at: String,

    /// Time when exploration completed
    pub completed_at: Option<String>,

    /// Duration of exploration in milliseconds
    pub duration_ms: u64,

    /// Whether all expected elements were found
    pub expected_elements_found: Vec<ElementCheck>,

    /// Whether any unexpected elements were found
    pub unexpected_elements_found: Vec<ElementCheck>,

    /// Confidence score from state detection (0.0 - 1.0)
    pub detection_confidence: f64,

    /// Error message if exploration failed
    pub error: Option<String>,

    /// Additional notes from AI analysis
    pub ai_notes: Option<String>,
}

/// Check result for a single element
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementCheck {
    /// Element description
    pub element: String,
    /// Whether the element was found
    pub found: bool,
    /// Confidence of the check
    pub confidence: f64,
    /// Location if found (x, y, width, height)
    pub location: Option<(i32, i32, i32, i32)>,
}

/// Result of exploring a transition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionExplorationResult {
    /// Transition ID
    pub transition_id: String,

    /// Source state ID
    pub from_state_id: String,

    /// Target state ID
    pub to_state_id: String,

    /// Exploration status
    pub status: ExplorationStatus,

    /// Screenshot before transition
    pub screenshot_before: Option<String>,

    /// Screenshot after transition
    pub screenshot_after: Option<String>,

    /// Time when exploration started
    pub started_at: String,

    /// Time when exploration completed
    pub completed_at: Option<String>,

    /// Duration of transition in milliseconds
    pub duration_ms: u64,

    /// Expected duration from description
    pub expected_duration_ms: Option<u64>,

    /// Whether transition took longer than expected
    pub duration_exceeded: bool,

    /// Error message if transition failed
    pub error: Option<String>,

    /// Additional notes
    pub notes: Option<String>,
}

/// Overall result of an exploration run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationResult {
    /// Unique ID for this exploration run
    pub run_id: String,

    /// Config path that was explored
    pub config_path: String,

    /// Strategy used for exploration
    pub strategy: String,

    /// When the exploration started
    pub started_at: String,

    /// When the exploration completed
    pub completed_at: Option<String>,

    /// Total duration in milliseconds
    pub total_duration_ms: u64,

    /// Total number of states in the config
    pub total_states: u32,

    /// Number of states visited
    pub states_visited: u32,

    /// Number of states that passed exploration
    pub states_passed: u32,

    /// Number of states that failed exploration
    pub states_failed: u32,

    /// Number of states skipped
    pub states_skipped: u32,

    /// Total number of transitions in the config
    pub total_transitions: u32,

    /// Number of transitions explored
    pub transitions_explored: u32,

    /// Number of transitions that passed
    pub transitions_passed: u32,

    /// Number of transitions that failed
    pub transitions_failed: u32,

    /// List of state explorations
    pub state_explorations: Vec<StateExplorationResult>,

    /// List of transition explorations
    pub transition_explorations: Vec<TransitionExplorationResult>,

    /// Overall status
    pub overall_status: ExplorationStatus,

    /// Summary of findings
    pub summary: String,

    /// Path to the full report
    pub report_path: Option<String>,
}

impl ExplorationResult {
    /// Create a new exploration result
    pub fn new(run_id: String, config_path: String, strategy: String) -> Self {
        Self {
            run_id,
            config_path,
            strategy,
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            total_duration_ms: 0,
            total_states: 0,
            states_visited: 0,
            states_passed: 0,
            states_failed: 0,
            states_skipped: 0,
            total_transitions: 0,
            transitions_explored: 0,
            transitions_passed: 0,
            transitions_failed: 0,
            state_explorations: Vec::new(),
            transition_explorations: Vec::new(),
            overall_status: ExplorationStatus::Pending,
            summary: String::new(),
            report_path: None,
        }
    }

    /// Mark the exploration as complete and calculate final stats
    pub fn complete(&mut self) {
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());

        // Calculate overall status
        if self.states_failed > 0 || self.transitions_failed > 0 {
            self.overall_status = ExplorationStatus::Failed;
        } else if self.states_visited == 0 {
            self.overall_status = ExplorationStatus::Error;
        } else if self.states_passed == self.states_visited
            && self.transitions_passed == self.transitions_explored
        {
            self.overall_status = ExplorationStatus::Passed;
        } else {
            self.overall_status = ExplorationStatus::Passed; // Partial success
        }

        // Generate summary
        self.summary = format!(
            "Explored {}/{} states ({} passed, {} failed, {} skipped), {}/{} transitions ({} passed, {} failed)",
            self.states_visited,
            self.total_states,
            self.states_passed,
            self.states_failed,
            self.states_skipped,
            self.transitions_explored,
            self.total_transitions,
            self.transitions_passed,
            self.transitions_failed
        );
    }

    /// Add a state exploration result
    pub fn add_state_exploration(&mut self, exploration: StateExplorationResult) {
        match exploration.status {
            ExplorationStatus::Passed => self.states_passed += 1,
            ExplorationStatus::Failed => self.states_failed += 1,
            ExplorationStatus::Skipped => self.states_skipped += 1,
            _ => {}
        }
        self.states_visited += 1;
        self.state_explorations.push(exploration);
    }

    /// Add a transition exploration result
    pub fn add_transition_exploration(&mut self, exploration: TransitionExplorationResult) {
        match exploration.status {
            ExplorationStatus::Passed => self.transitions_passed += 1,
            ExplorationStatus::Failed => self.transitions_failed += 1,
            _ => {}
        }
        self.transitions_explored += 1;
        self.transition_explorations.push(exploration);
    }
}
