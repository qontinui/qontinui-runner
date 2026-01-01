//! Core types for the AI Verification Agent
//!
//! These types define the configuration and results of verification tasks.

use serde::{Deserialize, Serialize};

/// Priority level for verification - determines order of state exploration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationPriority {
    /// Critical states - must be verified first
    Critical,
    /// High priority states - verified after critical
    High,
    /// Normal priority - default
    Normal,
    /// Low priority - verified last if time permits
    Low,
}

impl Default for VerificationPriority {
    fn default() -> Self {
        Self::Normal
    }
}

/// Status of a verification check
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Not yet verified
    Pending,
    /// Currently being verified
    InProgress,
    /// Verification passed - reality matches expectation
    Passed,
    /// Verification failed - discrepancy detected
    Failed,
    /// Verification was skipped (e.g., state not reachable)
    Skipped,
    /// Error occurred during verification
    Error,
}

impl Default for VerificationStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// Configuration for a verification task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationTaskConfig {
    /// Path to the qontinui config file
    pub config_path: String,

    /// Exploration strategy to use
    #[serde(default)]
    pub strategy: String, // "exhaustive", "smoke_test", "regression", "random_walk", "targeted"

    /// Maximum number of states to visit (0 = unlimited)
    #[serde(default)]
    pub max_states: u32,

    /// Maximum time in seconds for the verification run (0 = unlimited)
    #[serde(default)]
    pub max_duration_seconds: u64,

    /// Specific state IDs to verify (for targeted strategy)
    #[serde(default)]
    pub target_state_ids: Vec<String>,

    /// Specific transition IDs to verify (for targeted strategy)
    #[serde(default)]
    pub target_transition_ids: Vec<String>,

    /// Monitor index to use for verification
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

    /// Directory to store verification artifacts
    #[serde(default)]
    pub output_directory: Option<String>,

    /// Whether to stop on first failure
    #[serde(default)]
    pub stop_on_first_failure: bool,

    /// Random seed for reproducible random walks
    #[serde(default)]
    pub random_seed: Option<u64>,
}

fn default_true() -> bool {
    true
}

fn default_state_delay() -> u64 {
    500 // 500ms between states
}

impl Default for VerificationTaskConfig {
    fn default() -> Self {
        Self {
            config_path: String::new(),
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
        }
    }
}

/// Configuration for verifying a specific state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationConfig {
    /// State ID being verified
    pub state_id: String,

    /// Priority of this verification
    #[serde(default)]
    pub priority: VerificationPriority,

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
            priority: VerificationPriority::Normal,
            verification_hints: Vec::new(),
            expected_elements: Vec::new(),
            unexpected_elements: Vec::new(),
            detect_timeout_ms: default_detect_timeout(),
        }
    }
}

/// Result of verifying a single state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateVerification {
    /// State ID that was verified
    pub state_id: String,

    /// State name (for display)
    pub state_name: String,

    /// Verification status
    pub status: VerificationStatus,

    /// Path to the screenshot taken at this state
    pub screenshot_path: Option<String>,

    /// Time when verification started
    pub started_at: String,

    /// Time when verification completed
    pub completed_at: Option<String>,

    /// Duration of verification in milliseconds
    pub duration_ms: u64,

    /// Whether all expected elements were found
    pub expected_elements_found: Vec<ElementCheck>,

    /// Whether any unexpected elements were found
    pub unexpected_elements_found: Vec<ElementCheck>,

    /// Confidence score from state detection (0.0 - 1.0)
    pub detection_confidence: f64,

    /// Error message if verification failed
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

/// Result of verifying a transition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionVerification {
    /// Transition ID
    pub transition_id: String,

    /// Source state ID
    pub from_state_id: String,

    /// Target state ID
    pub to_state_id: String,

    /// Verification status
    pub status: VerificationStatus,

    /// Screenshot before transition
    pub screenshot_before: Option<String>,

    /// Screenshot after transition
    pub screenshot_after: Option<String>,

    /// Time when verification started
    pub started_at: String,

    /// Time when verification completed
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

/// Overall result of a verification run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Unique ID for this verification run
    pub run_id: String,

    /// Config path that was verified
    pub config_path: String,

    /// Strategy used for exploration
    pub strategy: String,

    /// When the verification started
    pub started_at: String,

    /// When the verification completed
    pub completed_at: Option<String>,

    /// Total duration in milliseconds
    pub total_duration_ms: u64,

    /// Total number of states in the config
    pub total_states: u32,

    /// Number of states visited
    pub states_visited: u32,

    /// Number of states that passed verification
    pub states_passed: u32,

    /// Number of states that failed verification
    pub states_failed: u32,

    /// Number of states skipped
    pub states_skipped: u32,

    /// Total number of transitions in the config
    pub total_transitions: u32,

    /// Number of transitions verified
    pub transitions_verified: u32,

    /// Number of transitions that passed
    pub transitions_passed: u32,

    /// Number of transitions that failed
    pub transitions_failed: u32,

    /// List of state verifications
    pub state_verifications: Vec<StateVerification>,

    /// List of transition verifications
    pub transition_verifications: Vec<TransitionVerification>,

    /// Overall status
    pub overall_status: VerificationStatus,

    /// Summary of findings
    pub summary: String,

    /// Path to the full report
    pub report_path: Option<String>,
}

impl VerificationResult {
    /// Create a new verification result
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
            transitions_verified: 0,
            transitions_passed: 0,
            transitions_failed: 0,
            state_verifications: Vec::new(),
            transition_verifications: Vec::new(),
            overall_status: VerificationStatus::Pending,
            summary: String::new(),
            report_path: None,
        }
    }

    /// Mark the verification as complete and calculate final stats
    pub fn complete(&mut self) {
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());

        // Calculate overall status
        if self.states_failed > 0 || self.transitions_failed > 0 {
            self.overall_status = VerificationStatus::Failed;
        } else if self.states_visited == 0 {
            self.overall_status = VerificationStatus::Error;
        } else if self.states_passed == self.states_visited
            && self.transitions_passed == self.transitions_verified
        {
            self.overall_status = VerificationStatus::Passed;
        } else {
            self.overall_status = VerificationStatus::Passed; // Partial success
        }

        // Generate summary
        self.summary = format!(
            "Verified {}/{} states ({} passed, {} failed, {} skipped), {}/{} transitions ({} passed, {} failed)",
            self.states_visited,
            self.total_states,
            self.states_passed,
            self.states_failed,
            self.states_skipped,
            self.transitions_verified,
            self.total_transitions,
            self.transitions_passed,
            self.transitions_failed
        );
    }

    /// Add a state verification result
    pub fn add_state_verification(&mut self, verification: StateVerification) {
        match verification.status {
            VerificationStatus::Passed => self.states_passed += 1,
            VerificationStatus::Failed => self.states_failed += 1,
            VerificationStatus::Skipped => self.states_skipped += 1,
            _ => {}
        }
        self.states_visited += 1;
        self.state_verifications.push(verification);
    }

    /// Add a transition verification result
    pub fn add_transition_verification(&mut self, verification: TransitionVerification) {
        match verification.status {
            VerificationStatus::Passed => self.transitions_passed += 1,
            VerificationStatus::Failed => self.transitions_failed += 1,
            _ => {}
        }
        self.transitions_verified += 1;
        self.transition_verifications.push(verification);
    }
}
