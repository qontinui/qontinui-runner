//! Goal verification for AI sessions
//!
//! Provides configurable goal verification strategies including:
//! - PromptBased: Trust [TASK_COMPLETE] marker in AI output
//! - VisualVerification: Use image recognition to verify goal state
//! - MaxIterationsOnly: Stop after max iterations (legacy)

use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::info;

/// How to verify that the goal has been achieved
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalVerificationType {
    /// Trust [TASK_COMPLETE] marker in Claude output (default)
    #[default]
    PromptBased,
    /// Use visual element verification
    VisualVerification,
    /// Only use max iterations, no goal checking
    MaxIterationsOnly,
}

/// Configuration for visual verification
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VisualVerificationConfig {
    /// Image ID to look for to verify goal
    pub target_image_id: Option<String>,
    /// Target state to navigate to and verify
    pub target_state_id: Option<String>,
    /// Whether to take screenshot for verification
    #[serde(default = "default_take_screenshot")]
    pub take_screenshot: bool,
    /// Monitor index for visual verification
    #[serde(default)]
    pub monitor_index: Option<i32>,
}

fn default_take_screenshot() -> bool {
    true
}

/// Goal configuration for an AI session
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GoalConfig {
    /// Type of goal verification to use
    #[serde(default)]
    pub verification_type: GoalVerificationType,
    /// Visual verification config (if using VisualVerification)
    #[serde(default)]
    pub visual_config: Option<VisualVerificationConfig>,
    /// Description of the goal for the AI
    #[serde(default)]
    pub goal_description: Option<String>,
}

/// Check if AI output contains goal completion markers
/// Returns true if any marker indicates the goal has been achieved
#[allow(dead_code)]
pub fn check_goal_completion_markers(output: &str) -> bool {
    // List of markers that indicate goal completion
    // Claude should output one of these when the goal is achieved
    // NOTE: [TASK_COMPLETE] is the canonical marker used by TaskMonitor and prompts
    let completion_markers = [
        "[TASK_COMPLETE]", // Primary marker - used by TaskMonitor and prompts
        "[GOAL_COMPLETE]",
        "[GOAL_ACHIEVED]",
        "[STOP_SESSION]",
        "[SESSION_COMPLETE]",
    ];

    // Check for explicit markers (case-insensitive)
    let output_upper = output.to_uppercase();
    for marker in &completion_markers {
        if output_upper.contains(marker) {
            info!("Goal completion marker detected: {}", marker);
            return true;
        }
    }

    // Also check for common completion patterns in structured output
    // These patterns appear in Claude's session summaries
    let completion_patterns = [
        r#""goal_achieved":\s*true"#,
        r#""goal_achieved": true"#,
        r#"goal_achieved:\s*true"#,
        r#""status":\s*"complete""#,
        r#"status:\s*"complete""#,
    ];

    for pattern in &completion_patterns {
        if let Ok(re) = Regex::new(pattern) {
            if re.is_match(output) {
                info!("Goal completion pattern detected: {}", pattern);
                return true;
            }
        }
    }

    false
}
