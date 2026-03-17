//! Step Executor Module
//!
//! Provides unified execution of automation steps (workflows, actions, states,
//! screenshots, Playwright tests, AWAS). This is the core execution layer used by:
//! - Run page (single workflow execution)
//! - AI Builder (multi-step execution before AI session)
//! - MCP API (direct step execution)
//!
//! The design principle: multi-step execution is the foundation, and running
//! a single workflow is just a special case (one step of type "workflow").
//!
//! ## Architecture
//!
//! Step execution uses a polymorphic handler dispatch system:
//!
//! ```text
//! StepExecutor.execute_single_step()
//!     └── HandlerRegistry.get_handler(step_type)
//!             └── handler.execute(step, context)
//! ```
//!
//! All step types are implemented as separate handlers in the `handlers/` module.
//! The `HandlerRegistry` maps step type strings to handler implementations.
//!
//! ## Core Step Types (3 handlers)
//!
//! - **Command**: command (unified: shell command, check, check group, test)
//! - **UI Bridge**: ui_bridge
//! - **AI**: prompt

#![allow(dead_code)]

use regex::Regex;

use crate::action_service::UnifiedActionService;
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::database::CreateTaskRunEventInput;
use crate::display::RawEvent;
use crate::executor::file_logger::FileLogger;
use crate::iteration_bundle::{
    parse_action_events, parse_image_recognition_events, ActionEvent, ImageRecognitionEvent,
    RelevantLogSources,
};
use crate::orchestrator::context_propagation::{
    ExpressionEvaluator, RuntimeContext, SharedVariableStore,
};
use crate::str_utils::truncate_str;
use crate::unified_workflow_executor::get_parent_task_id;

// Handler system imports
use super::handlers::{HandlerContext, HandlerRegistry, StepHandler};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

/// Workflow phase for step execution.
///
/// Steps can explicitly declare which phase they belong to, eliminating
/// the need for heuristic-based phase detection from step names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepPhase {
    /// Setup phase - runs once at the start
    Setup,
    /// Verification phase - runs tests/checks each iteration
    Verification,
    /// Agentic phase - AI execution
    Agentic,
    /// Completion phase - runs once at the end
    Completion,
}

impl StepPhase {
    /// Convert to string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            StepPhase::Setup => "setup",
            StepPhase::Verification => "verification",
            StepPhase::Agentic => "agentic",
            StepPhase::Completion => "completion",
        }
    }

    /// Parse from string, returning None for invalid values.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "setup" => Some(StepPhase::Setup),
            "verification" => Some(StepPhase::Verification),
            "agentic" => Some(StepPhase::Agentic),
            "completion" => Some(StepPhase::Completion),
            _ => None,
        }
    }
}

impl std::fmt::Display for StepPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// DAG Execution Engine
// ============================================================================

/// Compute execution layers from step dependencies using topological sort (Kahn's algorithm).
///
/// Steps are organized into layers where all steps in a layer can execute in parallel.
/// Dependencies come from two sources:
/// 1. `inputs` — referenced step IDs are implicit dependencies
/// 2. `depends_on` — explicit ordering constraints
///
/// Returns `Vec<Vec<usize>>` where each inner vec is a layer of step indices that
/// can run concurrently. Returns Err if a cycle is detected.
pub fn compute_execution_layers(steps: &[ExecutionStepConfig]) -> Result<Vec<Vec<usize>>, String> {
    let n = steps.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    // Build a map from step ID to index
    let mut id_to_index: HashMap<String, usize> = HashMap::new();
    for (i, step) in steps.iter().enumerate() {
        if let Some(ref id) = step.id {
            id_to_index.insert(id.clone(), i);
        }
    }

    // Build adjacency list and in-degree counts
    let mut in_degree = vec![0usize; n];
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, step) in steps.iter().enumerate() {
        // Collect dependencies from `inputs` (extract referenced step IDs)
        if let Some(ref inputs) = step.inputs {
            for reference in inputs.values() {
                // Parse "step-id.property[.path]" — extract the step ID (first segment)
                let step_id = reference.split('.').next().unwrap_or("");
                if let Some(&dep_index) = id_to_index.get(step_id) {
                    if dep_index != i {
                        adjacency[dep_index].push(i);
                        in_degree[i] += 1;
                    }
                }
            }
        }

        // Collect dependencies from `depends_on` (explicit ordering)
        if let Some(ref deps) = step.depends_on {
            for dep_id in deps {
                if let Some(&dep_index) = id_to_index.get(dep_id) {
                    if dep_index != i {
                        adjacency[dep_index].push(i);
                        in_degree[i] += 1;
                    }
                }
            }
        }
    }

    // Kahn's algorithm: BFS topological sort, grouping into layers
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (i, &deg) in in_degree.iter().enumerate().take(n) {
        if deg == 0 {
            queue.push_back(i);
        }
    }

    let mut layers: Vec<Vec<usize>> = Vec::new();
    let mut processed = 0;

    while !queue.is_empty() {
        let layer_size = queue.len();
        let mut layer = Vec::with_capacity(layer_size);

        for _ in 0..layer_size {
            let node = queue.pop_front().unwrap();
            layer.push(node);
            processed += 1;

            for &neighbor in &adjacency[node] {
                in_degree[neighbor] -= 1;
                if in_degree[neighbor] == 0 {
                    queue.push_back(neighbor);
                }
            }
        }

        layers.push(layer);
    }

    if processed != n {
        return Err(format!(
            "Circular dependency detected: {} of {} steps could not be ordered",
            n - processed,
            n
        ));
    }

    Ok(layers)
}

/// Extract step IDs referenced in an `inputs` map.
///
/// Each input value has format "step-id.property[.path]".
/// Returns the unique set of referenced step IDs.
pub fn extract_input_dependencies(inputs: &HashMap<String, String>) -> HashSet<String> {
    inputs
        .values()
        .filter_map(|reference| {
            let step_id = reference.split('.').next()?;
            if step_id.is_empty() {
                None
            } else {
                Some(step_id.to_string())
            }
        })
        .collect()
}

// ============================================================================
// Step Configuration
// ============================================================================

/// Configuration for a single execution step.
///
/// Supports 3 core step types: command, ui_bridge, prompt.
/// ("test" is dispatched through command handler when test_id/test_type fields are set)
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ExecutionStepConfig {
    /// Step type: "command", "ui_bridge", "prompt" (legacy "test" maps to "command")
    #[serde(rename = "type")]
    pub step_type: String,

    /// Explicit command mode: "shell", "check", "check_group", or "test".
    /// When set, the command handler uses this directly instead of inferring
    /// from which optional fields are populated.
    #[serde(alias = "mode", default)]
    pub command_mode: Option<String>,

    /// Step ID from the workflow definition (UUID)
    #[serde(default)]
    pub id: Option<String>,

    /// Step name (description)
    #[serde(default)]
    pub name: Option<String>,

    /// Workflow phase: "setup", "verification", "agentic", or "completion"
    #[serde(default)]
    pub phase: Option<String>,

    /// Timeout for this step in seconds
    #[serde(rename = "timeoutSeconds", default)]
    pub timeout_seconds: Option<u64>,

    /// Whether to run this step on subsequent iterations (after the first).
    /// Default: true (all steps run on each iteration for fresh data)
    #[serde(rename = "runOnSubsequentIterations", default)]
    pub run_on_subsequent_iterations: Option<bool>,

    /// Optional sub-step identifier for granular progress tracking.
    #[serde(rename = "subStepId", alias = "sub_step_id")]
    pub sub_step_id: Option<String>,

    /// Whether this step should only run in dev mode
    #[serde(alias = "devModeOnly", alias = "dev_mode_only", default)]
    pub dev_mode_only: Option<bool>,

    // ========================================================================
    // Data Flow (NEW - replaces gates)
    // ========================================================================
    /// Input mappings: name -> "step-id.field" or "step-id.output.json.path"
    /// References are resolved just-in-time before step execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<HashMap<String, String>>,

    /// Extract named values from this step's output using JSON paths.
    /// name -> JSON path into step output
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract: Option<HashMap<String, String>>,

    /// Explicit ordering dependencies beyond those implied by inputs.
    /// Step IDs that must complete before this step runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,

    /// Whether this step is required for verification to pass.
    /// Default: true. Set to false for informational-only steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,

    /// Number of retry attempts on failure (0 = no retries)
    #[serde(alias = "retryCount", alias = "retry_count", default)]
    pub retry_count: Option<u32>,

    /// Delay in milliseconds between retry attempts (default: 2000)
    #[serde(alias = "retryDelayMs", alias = "retry_delay_ms", default)]
    pub retry_delay_ms: Option<u64>,

    // ========================================================================
    // Test Step Fields
    // ========================================================================
    /// Test ID for verification test steps
    #[serde(rename = "testId", alias = "test_id")]
    pub test_id: Option<String>,

    /// Test type for verification test steps
    #[serde(alias = "testType", alias = "test_type")]
    pub test_type: Option<String>,

    /// Whether test failure should fail the workflow
    #[serde(
        alias = "testIsCritical",
        alias = "test_is_critical",
        alias = "is_critical",
        alias = "is_blocking",
        default
    )]
    pub test_is_critical: Option<bool>,

    // ========================================================================
    // Shell Command Step Fields (used by command type)
    // ========================================================================
    /// Shell Command: The command to execute
    #[serde(alias = "shellCommand", alias = "command")]
    pub shell_command: Option<String>,

    /// Shell Command: Reference to a saved shell command ID
    #[serde(alias = "shellCommandId", alias = "shell_command_id")]
    pub shell_command_id: Option<String>,

    /// Shell Command: Working directory for command execution
    #[serde(alias = "shellCommandWorkingDirectory", alias = "working_directory")]
    pub shell_command_working_directory: Option<String>,

    /// Shell Command: Whether to fail the workflow if command returns non-zero
    #[serde(alias = "shellCommandFailOnError", alias = "fail_on_error", default)]
    pub shell_command_fail_on_error: Option<bool>,

    // ========================================================================
    // Check Step Fields (used by command type with check_type)
    // ========================================================================
    /// Check: Type of check (lint, format, typecheck, analyze, security, custom_command)
    #[serde(alias = "checkType", alias = "check_type")]
    pub check_type: Option<String>,

    /// Check: Command to run
    #[serde(alias = "checkCommand")]
    pub check_command: Option<String>,

    /// Check: Working directory
    #[serde(alias = "checkWorkingDirectory")]
    pub check_working_directory: Option<String>,

    /// Check: Whether to run auto-fix
    #[serde(alias = "checkAutoFix", alias = "auto_fix", default)]
    pub check_auto_fix: Option<bool>,

    /// Check: URL to check for http_status check type
    #[serde(alias = "checkUrl")]
    pub check_url: Option<String>,

    /// Check: Expected HTTP status code (default: 200)
    #[serde(alias = "expectedStatus")]
    pub expected_status: Option<u16>,

    /// Check (ai_review): Prompt instructions for the AI reviewer
    #[serde(alias = "aiReviewPrompt", alias = "ai_review_prompt")]
    pub ai_review_prompt: Option<String>,

    /// Check (ai_review): Path to file to review
    #[serde(alias = "aiReviewInputPath", alias = "ai_review_input_path")]
    pub ai_review_input_path: Option<String>,

    /// Check (ai_review): Also validate input as a workflow JSON before AI review
    #[serde(
        alias = "aiReviewValidateAsWorkflow",
        alias = "ai_review_validate_as_workflow",
        default
    )]
    pub ai_review_validate_as_workflow: Option<bool>,

    /// Check (ai_review): Path to acceptance criteria JSON for cross-validation
    #[serde(
        alias = "aiReviewCriteriaPath",
        alias = "ai_review_criteria_path",
        default
    )]
    pub ai_review_criteria_path: Option<String>,

    /// Check (ci_cd): GitHub repository in owner/repo format
    #[serde(alias = "repository", alias = "ciCdRepository")]
    pub ci_cd_repository: Option<String>,

    /// Check (ci_cd): GitHub Actions workflow name filter
    #[serde(
        alias = "workflow_name",
        alias = "workflowName",
        alias = "ciCdWorkflowName"
    )]
    pub ci_cd_workflow_name: Option<String>,

    /// Check (ci_cd): Branch filter
    #[serde(alias = "branch", alias = "ciCdBranch")]
    pub ci_cd_branch: Option<String>,

    /// Check (ci_cd): Wait for in-progress runs to complete
    #[serde(
        alias = "wait_for_completion",
        alias = "waitForCompletion",
        alias = "ciCdWait",
        default
    )]
    pub ci_cd_wait: Option<bool>,

    // ========================================================================
    // Prompt Step Fields
    // ========================================================================
    /// Prompt content (for prompt steps - not executed, passed to AI)
    #[serde(rename = "promptContent", alias = "content")]
    pub prompt_content: Option<String>,

    /// Prompt execution mode: "session" (default) or "response" (simple prompt→response)
    #[serde(alias = "promptMode", alias = "prompt_mode")]
    pub prompt_mode: Option<String>,

    /// Path to write AI response output
    #[serde(alias = "outputPath", alias = "output_path")]
    pub output_path: Option<String>,

    /// Path to read input from (content appended to prompt)
    #[serde(alias = "inputPath", alias = "input_path")]
    pub input_path: Option<String>,

    /// Per-step model override (takes precedence over phase-level override).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Per-step provider override (takes precedence over phase-level override).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    // ========================================================================
    // Check Group Step Fields
    // ========================================================================
    /// Check Group: ID of the check group to execute
    #[serde(alias = "checkGroupId", alias = "check_group_id")]
    pub check_group_id: Option<String>,

    // ========================================================================
    // UI Bridge Step Fields
    // ========================================================================
    /// UI Bridge: Action to perform ("navigate", "execute", "assert", "snapshot")
    #[serde(alias = "uiBridgeAction", alias = "ui_bridge_action")]
    pub ui_bridge_action: Option<String>,

    /// UI Bridge: URL to navigate to or connect to
    #[serde(alias = "uiBridgeUrl", alias = "ui_bridge_url")]
    pub ui_bridge_url: Option<String>,

    /// UI Bridge: Natural language instruction for execute action
    #[serde(alias = "uiBridgeInstruction", alias = "ui_bridge_instruction")]
    pub ui_bridge_instruction: Option<String>,

    /// UI Bridge: Target element selector or description for assert action
    #[serde(alias = "uiBridgeTarget", alias = "ui_bridge_target")]
    pub ui_bridge_target: Option<String>,

    /// UI Bridge: Assertion type ("exists", "text_equals", "contains", "visible", "enabled")
    #[serde(alias = "uiBridgeAssertType", alias = "ui_bridge_assert_type")]
    pub ui_bridge_assert_type: Option<String>,

    /// UI Bridge: Expected value for assertions
    #[serde(alias = "uiBridgeExpected", alias = "ui_bridge_expected")]
    pub ui_bridge_expected: Option<String>,

    /// UI Bridge: Timeout in milliseconds for UI Bridge operations
    #[serde(alias = "uiBridgeTimeoutMs", alias = "ui_bridge_timeout_ms")]
    pub ui_bridge_timeout_ms: Option<u64>,

    /// UI Bridge: Comparison mode ("structural", "visual", "both") for compare action
    #[serde(alias = "uiBridgeCompareMode", alias = "ui_bridge_compare_mode")]
    pub ui_bridge_compare_mode: Option<String>,

    /// UI Bridge: Reference snapshot data (JSON) for compare action
    #[serde(
        alias = "uiBridgeReferenceSnapshot",
        alias = "ui_bridge_reference_snapshot"
    )]
    pub ui_bridge_reference_snapshot: Option<serde_json::Value>,

    /// UI Bridge: Reference snapshot ID for compare action (loads from saved snapshots)
    #[serde(
        alias = "uiBridgeReferenceSnapshotId",
        alias = "ui_bridge_reference_snapshot_id"
    )]
    pub ui_bridge_reference_snapshot_id: Option<String>,

    /// UI Bridge: Severity threshold for compare action ("critical", "major", "minor", "info")
    #[serde(
        alias = "uiBridgeSeverityThreshold",
        alias = "ui_bridge_severity_threshold"
    )]
    pub ui_bridge_severity_threshold: Option<String>,

    /// UI Bridge: Snapshot target — "control" (default), "sdk", or "proxy:PORT"
    #[serde(alias = "uiBridgeSnapshotTarget", alias = "ui_bridge_snapshot_target")]
    pub ui_bridge_snapshot_target: Option<String>,

    // ========================================================================
    // Artifact Step Fields
    // ========================================================================
    /// Path to a workflow JSON file to save (used by save_workflow_artifact)
    #[serde(alias = "artifactInputPath", alias = "artifact_input_path")]
    pub artifact_input_path: Option<String>,

    /// Enable PipelineArtifact capture in save_workflow_artifact.
    /// When true, reads investigation.md, criteria.json, and prompt data from
    /// the artifact directory and creates a PipelineArtifact for training data.
    #[serde(
        alias = "artifactCapturePrompts",
        alias = "artifact_capture_prompts",
        default
    )]
    pub artifact_capture_prompts: Option<bool>,

    // ========================================================================
    // Workflow Fixup Step Fields
    // ========================================================================
    /// Path to workflow JSON file for fixup operations (supports {{artifact_dir}})
    #[serde(alias = "fixupInputPath", alias = "fixup_input_path")]
    pub fixup_input_path: Option<String>,

    /// Fixup mode: "autofix", "harden", or "validate_criteria"
    #[serde(alias = "fixupMode", alias = "fixup_mode")]
    pub fixup_mode: Option<String>,

    /// Path to criteria JSON file (used by validate_criteria mode)
    #[serde(alias = "fixupCriteriaPath", alias = "fixup_criteria_path")]
    pub fixup_criteria_path: Option<String>,

    // ========================================================================
    // Workflow Step Fields (for "workflow" step type — run a saved workflow inline)
    // ========================================================================
    /// ID of the referenced workflow to execute inline.
    #[serde(alias = "workflowId", alias = "workflow_id")]
    pub ref_workflow_id: Option<String>,

    /// Cached display name of the referenced workflow.
    /// Note: `workflow_name` alias already claimed by `ci_cd_workflow_name`,
    /// so the handler reads the name from the loaded workflow instead.
    #[serde(alias = "refWorkflowName")]
    pub ref_workflow_name: Option<String>,

    /// Input variables for workflow_ref steps.
    /// Keys are variable names; values are substituted into the child workflow's
    /// prompt using `{{key}}` template syntax.
    #[serde(
        alias = "refWorkflowInputs",
        alias = "ref_workflow_inputs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub ref_workflow_inputs: Option<HashMap<String, String>>,

    /// Whether to inherit model overrides from the parent workflow context.
    /// Only used by workflow_ref steps. Default: true.
    #[serde(
        alias = "refInheritModelOverrides",
        alias = "ref_inherit_model_overrides",
        default
    )]
    pub ref_inherit_model_overrides: Option<bool>,

    // ========================================================================
    // Restart Process Step Fields
    // ========================================================================
    /// Process config ID to restart
    #[serde(alias = "restartProcessId", alias = "restart_process_id")]
    pub restart_process_id: Option<String>,

    /// Process name to restart (resolved to ID at runtime)
    #[serde(alias = "restartProcessName", alias = "restart_process_name")]
    pub restart_process_name: Option<String>,

    /// Whether to wait for health port after restart (default: true)
    #[serde(
        alias = "restartWaitForHealth",
        alias = "restart_wait_for_health",
        default
    )]
    pub restart_wait_for_health: Option<bool>,

    // ========================================================================
    // Console Error Handling
    // ========================================================================
    /// If true, step fails when console errors are captured during execution
    /// (even if the step itself passes). Default: false (console errors are informational).
    #[serde(
        default,
        rename = "failOnConsoleErrors",
        alias = "fail_on_console_errors"
    )]
    pub fail_on_console_errors: bool,
}

impl ExecutionStepConfig {
    /// Get the typed phase if set, parsing from string if needed.
    pub fn get_phase(&self) -> Option<StepPhase> {
        self.phase.as_ref().and_then(|p| StepPhase::from_str_opt(p))
    }

    /// Set the phase explicitly.
    pub fn with_phase(mut self, phase: StepPhase) -> Self {
        self.phase = Some(phase.as_str().to_string());
        self
    }

    /// Set the phase on a mutable reference.
    pub fn set_phase(&mut self, phase: StepPhase) {
        self.phase = Some(phase.as_str().to_string());
    }

    /// Create the default pre-flight environment check step.
    ///
    /// This creates a step that runs inline commands to verify:
    /// - Disk space (minimum 5GB free)
    /// - Node.js availability
    /// - Git availability
    ///
    /// The check is platform-aware and uses inline commands (no external script required).
    /// Exit codes: 0=all passed, non-zero=check failed
    pub fn default_preflight_check() -> Self {
        // Use inline commands instead of external script for portability
        // Windows: Raw PowerShell script (check handler detects PowerShell syntax and runs directly)
        // Unix: Use bash with && chained commands
        let command = if cfg!(target_os = "windows") {
            // Windows: Raw PowerShell script - DO NOT wrap with "powershell -Command"
            // The check handler will detect PowerShell syntax (Get-, $var) and run via PowerShell
            r#"$freeGB = 0; try { $d = Get-PSDrive -Name ((Get-Location).Drive.Name) -ErrorAction Stop; $freeGB = [math]::Round($d.Free / 1GB, 1) } catch { try { $disk = Get-CimInstance -ClassName Win32_LogicalDisk -Filter "DeviceID='C:'" -ErrorAction Stop; $freeGB = [math]::Round($disk.FreeSpace / 1GB, 1) } catch { $freeGB = -1 } }; if ($freeGB -lt 0) { Write-Host 'Disk: unknown (could not query)' } else { Write-Host "Disk: $freeGB GB free"; if ($freeGB -lt 5) { Write-Host '[FAIL] Low disk space'; exit 1 } }; $nodeVer = node --version 2>$null; if ($nodeVer) { Write-Host "[OK] Node.js: $nodeVer" } else { Write-Host '[WARN] Node.js not found' }; $gitVer = git --version 2>$null; if ($gitVer) { Write-Host "[OK] $gitVer" } else { Write-Host '[WARN] Git not found' }; exit 0"#
        } else {
            // Unix: bash commands
            r#"FREE_GB=$(($(df -k . | tail -1 | awk '{print $4}') / 1024 / 1024)); echo "Disk: ${FREE_GB}GB free"; if [ "$FREE_GB" -lt 5 ]; then echo "[FAIL] Low disk space"; exit 1; fi; node --version 2>/dev/null && echo "[OK] Node.js found" || echo "[WARN] Node.js not found"; git --version 2>/dev/null && echo "[OK] Git found" || echo "[WARN] Git not found"; exit 0"#
        };

        Self {
            step_type: "command".to_string(),
            command_mode: Some("check".to_string()),
            name: Some("Pre-flight Environment Check".to_string()),
            phase: Some("setup".to_string()),
            check_type: Some("custom_command".to_string()),
            check_command: Some(command.to_string()),
            // Non-critical: environment warnings should not block workflow
            // Only critical failures (exit 1) like low disk space will cause the step to fail
            test_is_critical: Some(false),
            // Only run on first iteration - environment doesn't change during workflow
            run_on_subsequent_iterations: Some(false),
            ..Default::default()
        }
    }

    /// Create a health check step for verifying server availability.
    ///
    /// This step makes an HTTP request to a URL and checks for the expected status code.
    /// Used by the automatic health check feature when `health_check_enabled` is true.
    pub fn health_check(
        name: &str,
        url: &str,
        expected_status: u16,
        timeout_seconds: u64,
        is_critical: bool,
    ) -> Self {
        Self {
            step_type: "command".to_string(),
            command_mode: Some("check".to_string()),
            name: Some(name.to_string()),
            phase: Some("verification".to_string()),
            check_type: Some("http_status".to_string()),
            check_url: Some(url.to_string()),
            expected_status: Some(expected_status),
            timeout_seconds: Some(timeout_seconds),
            test_is_critical: Some(is_critical),
            run_on_subsequent_iterations: Some(true),
            ..Default::default()
        }
    }

    /// Check if this step should run based on the current iteration number
    /// Returns true if the step should be executed, false if it should be skipped
    pub fn should_run_on_iteration(&self, iteration: u32) -> bool {
        // First iteration always runs all steps
        if iteration <= 1 {
            return true;
        }

        // For subsequent iterations, check if the step is configured to run
        // Default: all steps run on each iteration for fresh data
        // Users can explicitly set run_on_subsequent_iterations: false to skip on subsequent iterations
        self.run_on_subsequent_iterations.unwrap_or(true)
    }
}

/// Result of executing a single step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    /// Step index (0-based)
    pub step_index: usize,
    /// Step type that was executed
    pub step_type: String,
    /// Step name for display
    pub step_name: String,
    /// Step ID from the workflow definition (UUID)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// Whether the step succeeded
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Path to screenshot if captured
    pub screenshot_path: Option<String>,
    /// When this step started (ISO 8601 timestamp)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When this step ended (ISO 8601 timestamp)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Step configuration (for AI visibility)
    pub config: StepExecutionConfig,
    /// Verification-specific fields (for test/check steps in verification phase)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_details: Option<VerificationStepDetails>,
    /// Additional output data from the step handler
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_data: Option<serde_json::Value>,
    /// Whether this step is required for verification pass/fail (from step config)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Resolved input values (for debugging data flow)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_inputs: Option<HashMap<String, serde_json::Value>>,
    /// Extracted values from step output (for debugging data flow)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_values: Option<HashMap<String, serde_json::Value>>,
    /// Auto-detected failure category based on output patterns.
    /// Categories: "infrastructure", "setup_issue", "test_failure", "unknown"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_category: Option<String>,
    /// Whether this step was interrupted (runner restart detected)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<bool>,
}

/// Verification-specific details for test and check steps
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerificationStepDetails {
    /// Step ID from the workflow
    pub step_id: String,
    /// Phase this step belongs to
    pub phase: String,
    /// Standard output from the step
    pub stdout: Option<String>,
    /// Standard error from the step
    pub stderr: Option<String>,
    /// For test steps: number of assertions passed
    pub assertions_passed: Option<u32>,
    /// For test steps: total number of assertions
    pub assertions_total: Option<u32>,
    /// For test steps: console output from browser/runtime
    pub console_output: Option<String>,
    /// For Playwright tests: page snapshot (YAML accessibility tree)
    pub page_snapshot: Option<String>,
    /// Exit code from command execution
    pub exit_code: Option<i32>,
    /// For check_group steps: individual check results with details
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_results: Option<Vec<IndividualCheckResult>>,
    /// Console errors captured during this step's execution (from UI Bridge ConsoleCapture)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_errors: Option<Vec<serde_json::Value>>,
}

/// Individual check result within a check group
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndividualCheckResult {
    /// Check name
    pub name: String,
    /// Status: "passed", "failed", "skipped"
    pub status: String,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Number of issues found
    pub issues_found: u32,
    /// Number of issues fixed (if auto-fix is enabled)
    pub issues_fixed: u32,
    /// Number of files checked
    pub files_checked: u32,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Raw output from the check tool
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Individual issues found (limited to avoid huge payloads)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<CheckIssueDetail>,
}

/// Details of an individual issue found by a check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckIssueDetail {
    /// File path where the issue was found
    pub file: String,
    /// Line number (1-based)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column number (1-based)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Rule code (e.g., "E501", "no-unused-vars")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Issue message
    pub message: String,
    /// Severity level: "error", "warning", "info"
    pub severity: String,
    /// Whether this issue is fixable
    #[serde(default)]
    pub fixable: bool,
}

/// Step configuration captured for AI visibility
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepExecutionConfig {
    /// Timeout for this step in seconds
    pub timeout_seconds: Option<u64>,
    /// For check steps: "lint", "format", "typecheck", "analyze", "security", "custom_command"
    pub check_type: Option<String>,
    /// Shell command or check command
    pub command: Option<String>,
    /// Test ID for verification test steps
    pub test_id: Option<String>,
    /// Test type for test steps: "repository", "playwright", etc.
    pub test_type: Option<String>,
    /// Working directory for shell commands and checks
    pub working_directory: Option<String>,
    /// UI Bridge action type
    pub ui_bridge_action: Option<String>,
}

/// Result of executing all steps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Whether all steps completed successfully
    pub success: bool,
    /// Total number of steps
    pub total_steps: usize,
    /// Number of successful steps
    pub successful_steps: usize,
    /// Number of failed steps
    pub failed_steps: usize,
    /// Total execution time in milliseconds
    pub total_duration_ms: u64,
    /// Individual step results
    pub steps: Vec<StepExecutionResult>,
    /// Logs captured during execution (from .dev-logs/)
    #[serde(default)]
    pub captured_logs: Option<CapturedLogs>,
    /// Runner logs captured during execution (GUI automation events)
    #[serde(default)]
    pub captured_runner_logs: Option<CapturedRunnerLogs>,
    /// Whether verification passed (for unified workflows)
    #[serde(default)]
    pub verification_passed: Option<bool>,
    /// Loop/iteration details (for unified workflows)
    #[serde(default)]
    pub loop_result: Option<crate::unified_workflow_executor::LoopResult>,
    /// Task summary (AI-generated)
    #[serde(default)]
    pub task_summary: Option<String>,
}

/// Result of running all verification_steps in a unified workflow
///
/// This is returned by execute_verification_steps and used to:
/// 1. Determine if the agentic phase should run (any failures)
/// 2. Build context for the AI about what failed
/// 3. Store results in the database for the Recap page
///
/// Verification pass/fail is determined by `required` steps:
/// all_passed = all steps with required=true (default) succeeded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationPhaseResult {
    /// Iteration number (1-indexed)
    pub iteration: u32,
    /// Whether all required verification steps passed
    pub all_passed: bool,
    /// Total number of verification steps
    pub total_steps: usize,
    /// Number of steps that passed
    pub passed_steps: usize,
    /// Number of steps that failed
    pub failed_steps: usize,
    /// Number of steps that were skipped
    pub skipped_steps: usize,
    /// Total execution time in milliseconds
    pub total_duration_ms: u64,
    /// Individual step results
    pub step_results: Vec<StepExecutionResult>,
    /// Whether an unrecoverable failure occurred (e.g., connectivity failure)
    /// that makes agentic retry pointless
    pub critical_failure: bool,
    /// Console errors captured during the entire verification phase
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_errors: Option<Vec<serde_json::Value>>,
    /// Health status from the SDK app's UI Bridge (score, status, breakdown)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_health: Option<serde_json::Value>,
    /// Deduplicated browser events captured during verification (HMR, React errors, network, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_events: Option<Vec<serde_json::Value>>,
    /// Failed network requests captured during verification
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_failures: Option<Vec<serde_json::Value>>,
}

/// Extract a text representation from a handler's output_data for AI context.
///
/// Extract Unix-style env var prefixes (KEY=VALUE) from a command string.
/// cmd.exe doesn't support "KEY=VALUE command" syntax, so we parse out
/// env vars to pass them via Command::env() instead.
/// Example: "SKIP_WEB_SERVER=1 npx test" -> ([("SKIP_WEB_SERVER", "1")], "npx test")
fn extract_env_prefix_for_cmd(command: &str) -> (Vec<(String, String)>, String) {
    let mut envs = Vec::new();
    let mut remaining = command.trim();

    while let Some(eq_pos) = remaining.find('=') {
        let prefix = &remaining[..eq_pos];
        if prefix.is_empty()
            || prefix.contains(' ')
            || !prefix.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            break;
        }
        let after_eq = &remaining[eq_pos + 1..];
        let value_end = after_eq.find(' ').unwrap_or(after_eq.len());
        let value = &after_eq[..value_end];
        envs.push((prefix.to_string(), value.to_string()));
        remaining = after_eq[value_end..].trim_start();
    }

    (envs, remaining.to_string())
}

/// Handlers store their output in different shapes inside `output_data`.
/// This function tries common patterns to extract human-readable text:
/// 1. `output_data.output` — combined stdout+stderr (used by check handler)
/// 2. `output_data.summary` — text summary (used by check_group handler)
/// 3. `output_data` as a top-level string
/// 4. Fallback: pretty-printed JSON (truncated)
fn extract_text_from_output_data(output_data: &Option<serde_json::Value>) -> Option<String> {
    let data = output_data.as_ref()?;

    // 1. Direct string field "output" (check handler puts combined stdout+stderr here)
    if let Some(output) = data.get("output").and_then(|v| v.as_str()) {
        if !output.is_empty() {
            return Some(output.to_string());
        }
    }

    // 2. "summary" field (check_group handler, etc.)
    if let Some(summary) = data.get("summary").and_then(|v| v.as_str()) {
        if !summary.is_empty() {
            return Some(summary.to_string());
        }
    }

    // 3. Top-level string value
    if let Some(s) = data.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }

    // 4. Render as pretty JSON for any other structured output
    // Skip trivial values that wouldn't be useful to the AI
    if data.is_null() {
        return None;
    }
    if let Some(obj) = data.as_object() {
        if obj.is_empty() {
            return None;
        }
        // Skip if only contains "skipped": true (disabled steps)
        if obj.len() == 2 && obj.contains_key("skipped") {
            return None;
        }
    }

    let json_str = serde_json::to_string_pretty(data).ok()?;
    if json_str.len() > 4000 {
        Some(format!(
            "{}...\n[truncated, {} more chars]",
            truncate_str(&json_str, 4000),
            json_str.len() - 4000
        ))
    } else {
        Some(json_str)
    }
}

/// Categorize a verification step failure based on output patterns.
///
/// Returns a category string that helps the AI understand the nature of the failure:
/// - "infrastructure" — connectivity, timeout, or service availability issues
/// - "setup_issue" — missing files, modules, or configuration
/// - "test_failure" — assertion failures or test expectation mismatches
/// - "unknown" — no recognized pattern
pub fn categorize_failure(output: &str) -> &'static str {
    let lower = output.to_lowercase();

    // Infrastructure issues: connectivity, timeouts, service unavailability
    if lower.contains("connection refused")
        || lower.contains("econnrefused")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("econnreset")
        || lower.contains("enotfound")
        || lower.contains("ehostunreach")
        || lower.contains("network error")
        || lower.contains("socket hang up")
    {
        return "infrastructure";
    }

    // Setup issues: missing files, modules, configuration
    if lower.contains("no such file")
        || lower.contains("not found")
        || lower.contains("missing module")
        || lower.contains("module not found")
        || lower.contains("cannot find module")
        || lower.contains("modulenotfounderror")
        || lower.contains("importerror")
        || lower.contains("command not found")
        || lower.contains("is not recognized")
        || lower.contains("no such command")
    {
        return "setup_issue";
    }

    // Test failures: assertions, expectations, test framework errors
    if lower.contains("assertionerror")
        || lower.contains("assert_eq")
        || lower.contains("assert_ne")
        || lower.contains("expected")
        || lower.contains("assertion failed")
        || lower.contains("test failed")
        || lower.contains("expect(")
        || lower.contains("tobetruthy")
        || lower.contains("toequal")
        || lower.contains("tomatch")
    {
        return "test_failure";
    }

    "unknown"
}

impl VerificationPhaseResult {
    /// Build a failure context string for the agentic phase
    ///
    /// This summarizes what failed so the AI knows what to work on.
    /// Includes detailed per-step output, command info, and failure categorization.
    pub fn build_failure_context(&self) -> String {
        if self.all_passed {
            return String::new();
        }

        let mut context = String::new();
        context.push_str("## Verification Results\n\n");
        context.push_str(&format!(
            "**Status:** {} of {} verification steps passed\n\n",
            self.passed_steps, self.total_steps
        ));

        // App health status from UI Bridge (if available)
        if let Some(ref health) = self.app_health {
            let status = health
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            // Only include health info when the app is unhealthy — don't add noise for healthy apps
            if status == "degraded" || status == "broken" {
                let score = health.get("score").and_then(|v| v.as_u64()).unwrap_or(0);
                context.push_str(&format!(
                    "**App Health:** {} (score: {}/100)\n",
                    status.to_uppercase(),
                    score
                ));
                if let Some(breakdown) = health.get("breakdown") {
                    let crashes = breakdown
                        .get("crashes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let errors = breakdown
                        .get("errors")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let warnings = breakdown
                        .get("warnings")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    context.push_str(&format!(
                        "  Crashes: {}, Errors: {}, Warnings: {}\n",
                        crashes, errors, warnings
                    ));
                }
                if let Some(top_issue) = health.get("topIssue") {
                    if let Some(msg) = top_issue.get("message").and_then(|v| v.as_str()) {
                        let severity = top_issue
                            .get("severity")
                            .and_then(|v| v.as_str())
                            .unwrap_or("error");
                        context.push_str(&format!("  Top issue: [{}] {}\n", severity, msg));
                    }
                }
                context.push('\n');
            }
        }

        // List failed steps with details including command and failure category
        context.push_str("### Failed Steps\n\n");
        for result in &self.step_results {
            if !result.success {
                // Include failure category prefix if available
                let category_prefix = if let Some(ref cat) = result.failure_category {
                    if cat != "unknown" {
                        format!("[{}] ", cat)
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // Header: step name, type, and check subtype if present
                if let Some(ref check_type) = result.config.check_type {
                    context.push_str(&format!(
                        "#### {}{} ({}, {})\n",
                        category_prefix, result.step_name, result.step_type, check_type
                    ));
                } else {
                    context.push_str(&format!(
                        "#### {}{} ({})\n",
                        category_prefix, result.step_name, result.step_type
                    ));
                }

                // Include the command that was run (if available)
                if let Some(ref cmd) = result.config.command {
                    context.push_str(&format!("**Command:** `{}`\n", cmd));
                }

                // Include the working directory where the command ran (if set)
                if let Some(ref wd) = result.config.working_directory {
                    context.push_str(&format!("**Working Directory:** `{}`\n", wd));
                }

                if let Some(error) = &result.error {
                    context.push_str(&format!("**Error:** {}\n", error));
                }

                if let Some(details) = &result.verification_details {
                    if let Some(stdout) = &details.stdout {
                        if !stdout.is_empty() {
                            // Truncate long output
                            let truncated = if stdout.len() > 2000 {
                                format!(
                                    "{}...\n[truncated, {} more chars]",
                                    truncate_str(stdout, 2000),
                                    stdout.len() - 2000
                                )
                            } else {
                                stdout.clone()
                            };
                            context.push_str(&format!("**Output:**\n```\n{}\n```\n", truncated));
                        }
                    }
                    if let Some(stderr) = &details.stderr {
                        if !stderr.is_empty() {
                            let truncated = if stderr.len() > 1000 {
                                format!("{}...\n[truncated]", truncate_str(stderr, 1000))
                            } else {
                                stderr.clone()
                            };
                            context.push_str(&format!("**Stderr:**\n```\n{}\n```\n", truncated));
                        }
                    }
                    if let Some(passed) = details.assertions_passed {
                        if let Some(total) = details.assertions_total {
                            context.push_str(&format!(
                                "**Assertions:** {}/{} passed\n",
                                passed, total
                            ));
                        }
                    }
                    if let Some(ref console_errors) = details.console_errors {
                        if !console_errors.is_empty() {
                            context.push_str(&format!(
                                "**Console Errors ({}):**\n",
                                console_errors.len()
                            ));
                            for err in console_errors.iter().take(10) {
                                let msg = err
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Unknown error");
                                let err_type =
                                    err.get("type").and_then(|v| v.as_str()).unwrap_or("error");
                                context.push_str(&format!("- [{}] {}\n", err_type, msg));
                            }
                            if console_errors.len() > 10 {
                                context.push_str(&format!(
                                    "  ... and {} more console errors\n",
                                    console_errors.len() - 10
                                ));
                            }
                        }
                    }
                }

                // === Generic structured data extraction ===
                // These render structured details based on data presence, not step type.
                // Any step that produces check_results or assertion results gets them rendered.

                // Individual check results (e.g., from check_group steps)
                if let Some(ref details) = result.verification_details {
                    if let Some(ref checks) = details.check_results {
                        for check in checks {
                            if check.status == "failed" {
                                context.push_str(&format!("**Check: {} [FAILED]**\n", check.name));
                                if let Some(ref err) = check.error_message {
                                    context.push_str(&format!("Error: {}\n", err));
                                }
                                if let Some(ref output) = check.output {
                                    if !output.is_empty() {
                                        let truncated = if output.len() > 3000 {
                                            format!(
                                                "{}...\n[truncated, {} more chars]",
                                                truncate_str(output, 3000),
                                                output.len() - 3000
                                            )
                                        } else {
                                            output.clone()
                                        };
                                        context.push_str(&format!("```\n{}\n```\n", truncated));
                                    }
                                }
                                if !check.issues.is_empty() {
                                    context.push_str("Issues:\n");
                                    for issue in check.issues.iter().take(30) {
                                        context.push_str(&format!("- {}", issue.file));
                                        if let Some(line) = issue.line {
                                            context.push_str(&format!(":{}", line));
                                            if let Some(col) = issue.column {
                                                context.push_str(&format!(":{}", col));
                                            }
                                        }
                                        if let Some(ref code) = issue.code {
                                            context.push_str(&format!(" [{}]", code));
                                        }
                                        context.push_str(&format!(" {}\n", issue.message));
                                    }
                                    if check.issues.len() > 30 {
                                        context.push_str(&format!(
                                            "  ... and {} more issues\n",
                                            check.issues.len() - 30
                                        ));
                                    }
                                }
                                context.push('\n');
                            }
                        }
                    }
                }

                // Spec assertion results (from output_data, any step producing spec_result)
                if let Some(ref output_data) = result.output_data {
                    if let Some(assertion_results) = output_data
                        .get("spec_result")
                        .and_then(|sr| sr.get("assertionResults"))
                        .and_then(|ar| ar.as_array())
                    {
                        context.push_str("**Assertion Details:**\n");
                        for ar in assertion_results {
                            let passed =
                                ar.get("passed").and_then(|v| v.as_bool()).unwrap_or(false);
                            let status = if passed { "PASSED" } else { "FAILED" };
                            let target = ar.get("target").and_then(|v| v.as_str()).unwrap_or("?");
                            let target_desc = ar
                                .get("targetDescription")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            context.push_str(&format!("- [{}] {}", status, target));
                            if !target_desc.is_empty() && target_desc != target {
                                context.push_str(&format!(" ({})", target_desc));
                            }
                            context.push('\n');

                            if let Some(search) = ar.get("searchDetails") {
                                let confidence = search
                                    .get("confidence")
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(0.0);
                                let reasons = search
                                    .get("matchReasons")
                                    .and_then(|v| v.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|r| r.as_str())
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    })
                                    .unwrap_or_default();
                                let candidates = search
                                    .get("candidateCount")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                context.push_str(&format!(
                                    "  Element found: yes (confidence: {:.2}, match: \"{}\", candidates: {})\n",
                                    confidence, reasons, candidates
                                ));
                            } else if !passed
                                && ar.get("failureReason").and_then(|v| v.as_str())
                                    == Some("Element could not be found")
                            {
                                context.push_str("  Element found: no\n");
                            }

                            if !passed {
                                let expected = ar
                                    .get("expected")
                                    .map(|v| format!("{}", v))
                                    .unwrap_or_default();
                                let actual = ar
                                    .get("actual")
                                    .map(|v| format!("{}", v))
                                    .unwrap_or_default();
                                if !expected.is_empty() || !actual.is_empty() {
                                    context.push_str(&format!(
                                        "  Expected: {}, Actual: {}\n",
                                        expected, actual
                                    ));
                                }
                                if let Some(reason) =
                                    ar.get("failureReason").and_then(|v| v.as_str())
                                {
                                    context.push_str(&format!("  Reason: {}\n", reason));
                                }
                                if let Some(suggestion) =
                                    ar.get("suggestion").and_then(|v| v.as_str())
                                {
                                    context.push_str(&format!("  Suggestion: {}\n", suggestion));
                                }
                            }
                        }
                    }
                }

                context.push('\n');
            }
        }

        // List passed steps briefly
        let passed: Vec<_> = self.step_results.iter().filter(|r| r.success).collect();
        if !passed.is_empty() {
            context.push_str("### Passed Steps\n\n");
            for result in passed {
                context.push_str(&format!(
                    "- ✓ {} ({}ms)\n",
                    result.step_name, result.duration_ms
                ));
            }
        }

        // Phase-level console errors (captured between steps, not during any specific step)
        if let Some(ref console_errors) = self.console_errors {
            if !console_errors.is_empty() {
                context.push_str("\n### Console Errors During Verification\n\n");
                context.push_str(&format!(
                    "{} console error(s) captured during the verification phase:\n\n",
                    console_errors.len()
                ));
                for err in console_errors.iter().take(15) {
                    let msg = err
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown error");
                    let err_level = err.get("level").and_then(|v| v.as_str()).unwrap_or("error");
                    context.push_str(&format!("- [{}] {}\n", err_level, msg));
                }
                if console_errors.len() > 15 {
                    context.push_str(&format!("  ... and {} more\n", console_errors.len() - 15));
                }
                context.push('\n');
            }
        }

        // Browser events — richer than console errors, includes HMR failures,
        // React error boundaries, resource load errors, network errors
        if let Some(ref events) = self.browser_events {
            if !events.is_empty() {
                context.push_str("### Browser Events During Verification\n\n");
                for event in events.iter().take(15) {
                    // FingerprintedEvent shape: { fingerprint, event: AnyCapturedEvent, count, firstSeen, lastSeen }
                    // AnyCapturedEvent has: type, level, message, stack, timestamp, url
                    let inner = event.get("event");
                    let msg = inner
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        // Fallback: raw (non-fingerprinted) event with top-level message
                        .or_else(|| event.get("message").and_then(|v| v.as_str()));

                    if let Some(msg) = msg {
                        let severity = inner
                            .and_then(|e| e.get("level"))
                            .and_then(|v| v.as_str())
                            .or_else(|| inner.and_then(|e| e.get("type")).and_then(|v| v.as_str()))
                            .unwrap_or("error");
                        let count = event.get("count").and_then(|v| v.as_u64()).unwrap_or(1);
                        if count > 1 {
                            context.push_str(&format!("- [{}] {} (x{})\n", severity, msg, count));
                        } else {
                            context.push_str(&format!("- [{}] {}\n", severity, msg));
                        }
                    }
                }
                if events.len() > 15 {
                    context.push_str(&format!("  ... and {} more events\n", events.len() - 15));
                }
                context.push('\n');
            }
        }

        // Network failures — failed HTTP requests from the SDK app
        if let Some(ref failures) = self.network_failures {
            if !failures.is_empty() {
                context.push_str("### Failed Network Requests\n\n");
                for failure in failures.iter().take(10) {
                    let request = failure.get("request");
                    let response = failure.get("response");
                    let error = failure.get("error").and_then(|v| v.as_str());

                    let method = request
                        .and_then(|r| r.get("method"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let url = request
                        .and_then(|r| r.get("url"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let status = response
                        .and_then(|r| r.get("statusCode"))
                        .and_then(|v| v.as_u64());

                    if let Some(status_code) = status {
                        let status_text = response
                            .and_then(|r| r.get("statusText"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        context.push_str(&format!(
                            "- {} {} → {} {}\n",
                            method, url, status_code, status_text
                        ));
                    } else if let Some(err_msg) = error {
                        context.push_str(&format!("- {} {} → {}\n", method, url, err_msg));
                    } else {
                        context.push_str(&format!("- {} {} → failed\n", method, url));
                    }
                }
                if failures.len() > 10 {
                    context.push_str(&format!(
                        "  ... and {} more failed requests\n",
                        failures.len() - 10
                    ));
                }
                context.push('\n');
            }
        }

        context
    }

    /// Build a brief summary for logging
    pub fn summary(&self) -> String {
        if self.all_passed {
            format!(
                "Verification PASSED: {}/{} steps in {}ms",
                self.passed_steps, self.total_steps, self.total_duration_ms
            )
        } else {
            format!(
                "Verification FAILED: {}/{} steps passed, {} failed in {}ms{}",
                self.passed_steps,
                self.total_steps,
                self.failed_steps,
                self.total_duration_ms,
                if self.critical_failure {
                    " (CRITICAL)"
                } else {
                    ""
                }
            )
        }
    }
}

/// A log source configuration (passed from frontend)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSourceConfig {
    /// Unique identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Absolute path to the log file
    pub path: String,
    /// Whether this source is enabled
    pub enabled: bool,
}

/// Logs captured from application log files during automation
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapturedLogs {
    /// Log entries per source (keyed by source name)
    pub sources: HashMap<String, String>,
}

/// Runner logs captured during automation (GUI automation + Playwright)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapturedRunnerLogs {
    /// Action/workflow execution events (from runner-actions.jsonl)
    pub actions: Vec<ActionEvent>,
    /// Image recognition events (from runner-image-recognition.jsonl)
    pub image_recognition: Vec<ImageRecognitionEvent>,
}

// ============================================================================
// Log Watch Types
// ============================================================================

/// An error detected in a log file during log_watch step execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogError {
    /// Source log file name (e.g., "backend.log")
    pub source: String,
    /// Line number in the log file (1-indexed)
    pub line_number: usize,
    /// Timestamp extracted from the log line (if available)
    pub timestamp: Option<String>,
    /// The error message/line
    pub message: String,
    /// Context lines before the error (typically 2-3 lines)
    pub context_before: Vec<String>,
    /// Context lines after the error (typically 2-3 lines)
    pub context_after: Vec<String>,
    /// Type of error: "error", "exception", "traceback", "warning", "fatal", "panic"
    pub error_type: String,
}

/// Default error patterns used for log_watch if none specified
pub(crate) const DEFAULT_ERROR_PATTERNS: &[&str] = &[
    "ERROR",
    "Error:",
    "error:",
    "Exception",
    "exception",
    "Traceback",
    "traceback",
    "TypeError",
    "SyntaxError",
    "ReferenceError",
    "ValueError",
    "KeyError",
    "AttributeError",
    "ImportError",
    "RuntimeError",
    "FATAL",
    "fatal",
    "panic",
    "PANIC",
    "FAILED",
    "Failed:",
];

/// Get default log source filenames from global settings.
/// Falls back to ["backend.log", "frontend.log"] if no sources are configured.
pub(crate) fn get_default_log_source_names() -> Vec<String> {
    let settings = crate::settings::get_global_log_source_settings();
    let names: Vec<String> = settings
        .sources
        .iter()
        .filter(|s| s.enabled)
        .map(|s| {
            // If path is absolute, extract the filename; otherwise use as-is
            let path = std::path::Path::new(&s.path);
            if path.is_absolute() {
                path.file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| s.path.clone())
            } else {
                s.path.clone()
            }
        })
        .collect();
    if names.is_empty() {
        vec!["backend.log".to_string(), "frontend.log".to_string()]
    } else {
        names
    }
}

/// Default time window in seconds
pub(crate) const DEFAULT_TIME_WINDOW_SECONDS: u64 = 60;

/// Number of context lines before/after an error
pub(crate) const CONTEXT_LINES: usize = 3;

impl ExecutionResult {
    /// Generate a markdown summary of the execution results
    pub fn to_markdown_summary(&self) -> String {
        if self.steps.is_empty() {
            return String::new();
        }

        let mut summary = String::new();
        summary.push_str("\n## Pre-Execution Results\n\n");
        summary.push_str("The following steps were executed deterministically by the runner:\n\n");

        for result in &self.steps {
            summary.push_str(&format!(
                "{}. **{}** ({}): {} in {}ms\n",
                result.step_index + 1,
                result.step_name,
                result.step_type,
                if result.success {
                    "Success".to_string()
                } else {
                    format!(
                        "Failed - {}",
                        result.error.as_deref().unwrap_or("unknown error")
                    )
                },
                result.duration_ms
            ));

            if let Some(ref path) = result.screenshot_path {
                summary.push_str(&format!("   Screenshot: `{}`\n", path));
            }
        }

        summary.push_str(&format!(
            "\n**Summary:** {} of {} steps completed successfully.\n",
            self.successful_steps, self.total_steps
        ));

        if self.failed_steps > 0 {
            summary.push_str("\n**Note:** Some steps failed. Please analyze the errors above.\n");
        }

        // Include captured logs if any
        if let Some(ref logs) = self.captured_logs {
            if !logs.sources.is_empty() {
                summary.push_str("\n## Application Logs (Captured During Automation)\n\n");

                for (name, content) in &logs.sources {
                    if !content.trim().is_empty() {
                        summary.push_str(&format!("### {} Logs\n\n```\n", name));
                        // Limit to last 100 lines to avoid overwhelming the AI
                        let lines: Vec<&str> = content.lines().collect();
                        let start = if lines.len() > 100 {
                            lines.len() - 100
                        } else {
                            0
                        };
                        for line in &lines[start..] {
                            summary.push_str(line);
                            summary.push('\n');
                        }
                        summary.push_str("```\n\n");
                    }
                }
            }
        }

        summary
    }
}

/// Step Executor - executes automation steps using UnifiedActionService
pub struct StepExecutor {
    action_service: UnifiedActionService,
    app_state: Arc<AppState>,
    /// Configuration storage for loading saved configs
    config_storage: Arc<TokioMutex<ConfigStorage>>,
    /// Optional app handle for emitting events to the Tauri frontend
    app_handle: Option<tauri::AppHandle>,
    /// Optional task run ID for database logging (AWAS steps, etc.)
    task_run_id: Option<String>,
    /// Runtime context for variable expansion in commands
    runtime_context: RuntimeContext,
    /// Shared variable store for API request chaining (thread-safe, clone-friendly)
    shared_variables: SharedVariableStore,
    /// Registry of step handlers for polymorphic dispatch
    handler_registry: HandlerRegistry,
    /// PID tracker for AI process management (passed to WorkflowStepHandler)
    pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
}

impl StepExecutor {
    /// Create a new StepExecutor
    pub fn new(app_state: Arc<AppState>, config_storage: Arc<TokioMutex<ConfigStorage>>) -> Self {
        Self {
            action_service: UnifiedActionService::new(app_state.clone(), config_storage.clone()),
            app_state,
            config_storage,
            app_handle: None,
            task_run_id: None,
            runtime_context: RuntimeContext::new(),
            shared_variables: SharedVariableStore::new(),
            handler_registry: HandlerRegistry::with_standard_handlers(),
            pid_tracker: None,
        }
    }

    /// Create a new StepExecutor with an app handle for frontend event emission
    pub fn with_app_handle(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            action_service: UnifiedActionService::new(app_state.clone(), config_storage.clone()),
            app_state,
            config_storage,
            app_handle: Some(app_handle),
            task_run_id: None,
            runtime_context: RuntimeContext::new(),
            shared_variables: SharedVariableStore::new(),
            handler_registry: HandlerRegistry::with_standard_handlers(),
            pid_tracker: None,
        }
    }

    /// Set the task run ID for database logging (builder pattern).
    ///
    /// When set, AWAS step results will be saved to the database.
    pub fn with_task_run_id(mut self, task_run_id: String) -> Self {
        self.runtime_context = RuntimeContext::with_task_run_id(&task_run_id);
        self.task_run_id = Some(task_run_id);
        self
    }

    /// Set the task run ID for database logging (mutable setter).
    ///
    /// Same as `with_task_run_id` but takes `&mut self` for use after construction.
    pub fn set_task_run_id(&mut self, task_run_id: String) {
        self.runtime_context = RuntimeContext::with_task_run_id(&task_run_id);
        self.task_run_id = Some(task_run_id);
    }

    /// Set a variable in the runtime context for variable expansion in commands.
    ///
    /// Variables can be referenced in shell commands using `{{variable_name}}` syntax.
    pub fn set_context_variable(&mut self, name: &str, value: serde_json::Value) {
        self.runtime_context.set_variable(name, value);
    }

    /// Get the runtime context (for advanced use cases).
    pub fn runtime_context(&self) -> &RuntimeContext {
        &self.runtime_context
    }

    /// Get a mutable reference to the runtime context (for advanced use cases).
    pub fn runtime_context_mut(&mut self) -> &mut RuntimeContext {
        &mut self.runtime_context
    }

    /// Get the shared variable store.
    pub fn shared_variables(&self) -> &SharedVariableStore {
        &self.shared_variables
    }

    /// Create a HandlerContext for executing steps via the handler system.
    ///
    /// This shares the executor's state (runtime_context, shared_variables)
    /// with the handlers to maintain consistency during step execution.
    fn create_handler_context(&self) -> HandlerContext {
        HandlerContext::with_shared_state(
            self.app_state.clone(),
            self.config_storage.clone(),
            self.app_handle.clone(),
            self.runtime_context.clone(),
            self.shared_variables.clone(),
            self.task_run_id.clone(),
            self.pid_tracker.clone(),
        )
    }

    /// Expand shared variables in a string.
    ///
    /// Replaces `{{variable_name}}` patterns with values from the shared variable store.
    /// This is used for API request chaining where response data from one request
    /// can be referenced in subsequent requests.
    fn expand_with_shared_vars(&self, text: &str) -> String {
        use once_cell::sync::Lazy;
        static VAR_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\{\{([^}]+)\}\}").unwrap());

        let mut result = text.to_string();
        for cap in VAR_PATTERN.captures_iter(text) {
            let var_name = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if let Some(value) = self.shared_variables.get(var_name) {
                result = result.replace(&cap[0], &value);
            }
        }
        result
    }

    /// Persist runtime context (variables) to the database so the Context Tab can display them.
    ///
    /// Merges variables from both RuntimeContext and SharedVariableStore into a single JSON blob
    /// stored in `task_runs.runtime_context_json`.
    fn persist_runtime_context(&self, execution_id: &str) {
        let shared_vars = self.shared_variables.get_all();
        let ctx_vars = &self.runtime_context.variables;

        // Skip if there are no variables to persist
        if shared_vars.is_empty() && ctx_vars.is_empty() {
            return;
        }

        // Build a combined context: merge RuntimeContext variables and SharedVariableStore
        let mut variables = serde_json::Map::new();
        for (name, value) in ctx_vars {
            variables.insert(name.clone(), json!({ "value": value, "source": "system" }));
        }
        for (name, value) in &shared_vars {
            variables.insert(name.clone(), json!({ "value": value, "source": "step" }));
        }

        let context_json = json!({
            "variables": variables,
            "iteration": self.runtime_context.iteration,
        });

        // Remap to parent ID for workflow sequence children
        let task_run_id = get_parent_task_id(execution_id);

        if let Err(e) = self
            .app_state
            .checkpoint_db
            .update_task_run_runtime_context(&task_run_id, &context_json.to_string())
        {
            warn!("Failed to persist runtime context: {}", e);
        }
    }

    /// Log a step execution event to the database
    ///
    /// This logs step start, complete, and error events to the task_run_events table.
    ///
    /// Note: For composed run children (e.g., composed-run-X-workflow-N),
    /// the task_run_id is automatically remapped to the parent task ID because
    /// only parent IDs exist in task_runs (required by foreign key constraint).
    fn log_step_event(
        &self,
        task_run_id: &str,
        step: &ExecutionStepConfig,
        step_index: usize,
        event_subtype: &str,
        message: &str,
        duration_ms: Option<i64>,
        error: Option<&str>,
        exit_code: Option<i32>,
        stdout: Option<&str>,
        stderr: Option<&str>,
    ) {
        // For workflow sequence children, remap to parent ID to satisfy FK constraint
        let parent_id = get_parent_task_id(task_run_id);
        let step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());

        // Generate action_id for consistent event aggregation
        // Format matches StepEventBuilder: {phase}-{step_type}-{task_run_id}-{step_index}
        // This ensures start/complete events for the same step are merged in the Timeline
        let phase = step.phase.as_deref().unwrap_or("setup");
        let action_id = format!("{}-{}-{}-{}", phase, step.step_type, parent_id, step_index);

        // Build data JSON with step details (include original task_run_id for context)
        let iteration = self.runtime_context.iteration;
        let data = json!({
            "step_index": step_index,
            "step_type": step.step_type,
            "step_name": step_name,
            "phase": step.phase,
            "iteration": iteration,
            "original_task_run_id": task_run_id,  // Keep original ID for debugging
            "command": step.shell_command.as_ref().or(step.check_command.as_ref()),
            "working_directory": step.shell_command_working_directory.as_ref().or(step.check_working_directory.as_ref()),
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "error": error,
        });

        let event_input = CreateTaskRunEventInput {
            task_run_id: parent_id, // Use parent ID for FK constraint
            event_type: "step_execution".to_string(),
            event_subtype: Some(event_subtype.to_string()),
            message: message.to_string(),
            data: Some(serde_json::to_string(&data).unwrap_or_default()),
            workflow_name: None,
            state_name: None,
            action_id: Some(action_id),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms,
        };

        if let Err(e) = self
            .app_state
            .checkpoint_db
            .create_task_run_event(&event_input)
        {
            warn!("Failed to log step event: {}", e);
        }
    }

    /// Emit a tree event to the Tauri frontend (if app_handle is available)
    fn emit_tree_event(
        &self,
        event_type: &str,
        node: &serde_json::Value,
        timestamp: f64,
        sequence: u32,
    ) {
        use tauri::Emitter;
        if let Some(ref app_handle) = self.app_handle {
            let tree_event = json!({
                "type": "tree_event",
                "event_type": event_type,
                "node": node,
                "path": [],
                "timestamp": timestamp,
                "sequence": sequence,
            });
            if let Err(e) = app_handle.emit("executor-event", &tree_event) {
                warn!("Failed to emit tree event to frontend: {}", e);
            }
        }
    }

    /// Record a screenshot capture event to the RunRecordingHandler.
    ///
    /// This ensures screenshots captured directly by the step executor
    /// (not through Python) are still recorded in the automation logs.
    async fn record_screenshot_event(
        &self,
        screenshot_type: &str,
        file_path: &str,
        monitor: Option<i32>,
        delay_seconds: Option<u32>,
        success: bool,
        associated_action: Option<String>,
        error: Option<String>,
    ) {
        let monitor_str = monitor.map(|m| m.to_string());
        self.app_state
            .run_recording_handler
            .on_screenshot_captured(
                screenshot_type,
                file_path,
                monitor_str,
                delay_seconds,
                success,
                associated_action,
                error,
            )
            .await;
    }

    /// Execute a list of steps and return results
    ///
    /// This is the core execution function used by all consumers.
    /// Steps are executed in order, and execution continues even if a step fails
    /// (so the caller can see all results and decide how to proceed).
    pub async fn execute_steps(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
    ) -> ExecutionResult {
        self.execute_steps_with_log_sources(steps, execution_id, &[])
            .await
    }

    /// Execute steps for a specific iteration
    ///
    /// For iterations > 1, filters out setup steps that aren't marked to run on
    /// subsequent iterations. This is the iteration-aware version of execute_steps.
    ///
    /// For Playwright steps, all Playwright steps are combined (since Playwright
    /// closes the browser after each run). Setup Playwright scripts are run first,
    /// followed by verification Playwright scripts.
    pub async fn execute_steps_for_iteration(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
        iteration: u32,
    ) -> ExecutionResult {
        // Preprocess steps for iteration:
        // 1. Filter out setup steps that shouldn't run on subsequent iterations
        // 2. Combine Playwright steps for efficiency (setup + verification)
        let processed_steps = Self::preprocess_steps_for_iteration(steps, iteration);

        if processed_steps.len() != steps.len() {
            info!(
                "Iteration {}: Preprocessed {} steps to {} (filtered/combined)",
                iteration,
                steps.len(),
                processed_steps.len(),
            );
        }

        self.execute_steps_with_log_sources(&processed_steps, execution_id, log_sources)
            .await
    }

    /// Preprocess steps for a specific iteration
    ///
    /// This handles:
    /// 1. Filtering out setup steps that shouldn't run on subsequent iterations
    /// 2. For Playwright steps: combining multiple scripts into a single script
    ///    (setup scripts first, then verification scripts) since Playwright closes
    ///    the browser after each run
    fn preprocess_steps_for_iteration(
        steps: &[ExecutionStepConfig],
        iteration: u32,
    ) -> Vec<ExecutionStepConfig> {
        // For first iteration, return all steps as-is
        if iteration <= 1 {
            return steps.to_vec();
        }

        // For subsequent iterations, filter out steps that shouldn't run
        steps
            .iter()
            .filter(|step| {
                let should_run = step.should_run_on_iteration(iteration);
                if !should_run {
                    info!(
                        "Iteration {}: Skipping step '{}' (type: {})",
                        iteration,
                        step.name.as_deref().unwrap_or("unnamed"),
                        step.step_type
                    );
                }
                should_run
            })
            .cloned()
            .collect()
    }

    /// Execute steps with log source configuration for log capture
    #[tracing::instrument(
        name = "workflow.steps.execute",
        skip(self, steps, log_sources),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id,
            log_source_count = %log_sources.len()
        )
    )]
    pub async fn execute_steps_with_log_sources(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> ExecutionResult {
        let mut results = Vec::new();
        let total_start = std::time::Instant::now();

        if steps.is_empty() {
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: results,
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            };
        }

        // Determine which logs are relevant based on step types
        let relevant_logs = RelevantLogSources::from_steps(steps);
        relevant_logs.log_relevance();

        // Record log file positions before execution (only for enabled sources)
        let log_positions = Self::capture_log_positions(log_sources);

        // Record runner log positions (only if GUI automation is relevant)
        let runner_log_positions = if relevant_logs.gui_automation {
            Self::capture_runner_log_positions()
        } else {
            HashMap::new()
        };

        info!(
            "Executing {} steps for execution {}",
            steps.len(),
            execution_id
        );

        // Get the task run ID for event logging (prefer self.task_run_id, fall back to execution_id)
        let log_task_run_id = self
            .task_run_id
            .clone()
            .unwrap_or_else(|| execution_id.to_string());

        for (index, step) in steps.iter().enumerate() {
            let step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());
            let start_time = std::time::Instant::now();
            let started_at = chrono::Utc::now().to_rfc3339();

            info!(
                "Executing step {}/{}: {} ({})",
                index + 1,
                steps.len(),
                step_name,
                step.step_type
            );

            // Log step start event
            self.log_step_event(
                &log_task_run_id,
                step,
                index,
                "start",
                &format!(
                    "Starting step {}/{}: {} ({})",
                    index + 1,
                    steps.len(),
                    step_name,
                    step.step_type
                ),
                None,
                None,
                None,
                None,
                None,
            );

            let (success, error, screenshot_path, _output_data) =
                self.execute_single_step(step).await;

            let final_screenshot = screenshot_path;

            let duration_ms = start_time.elapsed().as_millis() as u64;

            if success {
                info!(
                    "Step {}/{} completed successfully in {}ms",
                    index + 1,
                    steps.len(),
                    duration_ms
                );
                // Log step completion event
                self.log_step_event(
                    &log_task_run_id,
                    step,
                    index,
                    "complete",
                    &format!(
                        "Step {}/{} completed successfully in {}ms",
                        index + 1,
                        steps.len(),
                        duration_ms
                    ),
                    Some(duration_ms as i64),
                    None,
                    None,
                    None,
                    None,
                );
            } else {
                warn!("Step {}/{} failed: {:?}", index + 1, steps.len(), error);
                // Log step error event
                self.log_step_event(
                    &log_task_run_id,
                    step,
                    index,
                    "error",
                    &format!("Step {}/{} failed: {:?}", index + 1, steps.len(), error),
                    Some(duration_ms as i64),
                    error.as_deref(),
                    None,
                    None,
                    None,
                );
            }

            let ended_at = chrono::Utc::now().to_rfc3339();

            results.push(StepExecutionResult {
                step_index: index,
                step_type: step.step_type.clone(),
                step_name,
                step_id: step.id.clone(),
                success,
                error,
                screenshot_path: final_screenshot,
                started_at: Some(started_at),
                ended_at: Some(ended_at),
                duration_ms,
                config: StepExecutionConfig {
                    timeout_seconds: step.timeout_seconds,
                    check_type: step.check_type.clone(),
                    command: step
                        .check_command
                        .clone()
                        .or_else(|| step.shell_command.clone()),
                    test_id: step.test_id.clone(),
                    test_type: step.test_type.clone(),
                    working_directory: step
                        .check_working_directory
                        .clone()
                        .or_else(|| step.shell_command_working_directory.clone()),
                    ui_bridge_action: step.ui_bridge_action.clone(),
                },
                verification_details: None,
                output_data: None,
                required: step.required,
                resolved_inputs: None,
                extracted_values: None,
                failure_category: None,
                interrupted: None,
            });
        }

        let successful_steps = results.iter().filter(|r| r.success).count();
        let failed_steps = results.len() - successful_steps;

        info!(
            "Completed {} steps: {} succeeded, {} failed",
            results.len(),
            successful_steps,
            failed_steps
        );

        // Capture logs that were written during execution
        let captured_logs = Self::capture_logs_since(log_sources, log_positions);

        // Capture runner logs (only if GUI automation was relevant)
        let captured_runner_logs = if relevant_logs.gui_automation {
            Self::capture_runner_logs_since(runner_log_positions)
        } else {
            None
        };

        // Persist runtime context (variables + shared variables) to the database
        // so the Context Tab can display them after completion.
        self.persist_runtime_context(execution_id);

        ExecutionResult {
            success: failed_steps == 0,
            total_steps: results.len(),
            successful_steps,
            failed_steps,
            total_duration_ms: total_start.elapsed().as_millis() as u64,
            steps: results,
            captured_logs,
            captured_runner_logs,
            verification_passed: None,
            loop_result: None,
            task_summary: None,
        }
    }

    // ========================================================================
    // Phase-Based Execution Methods
    // ========================================================================

    /// Filter steps by phase
    pub fn filter_steps_by_phase(
        steps: &[ExecutionStepConfig],
        phase: &str,
    ) -> Vec<ExecutionStepConfig> {
        steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some(phase))
            .cloned()
            .collect()
    }

    /// Check if any steps exist in the given phase
    pub fn has_steps_in_phase(steps: &[ExecutionStepConfig], phase: &str) -> bool {
        steps.iter().any(|s| s.phase.as_deref() == Some(phase))
    }

    /// Count steps in each phase
    pub fn count_steps_by_phase(
        steps: &[ExecutionStepConfig],
    ) -> std::collections::HashMap<String, usize> {
        let mut counts = std::collections::HashMap::new();
        for step in steps {
            if let Some(ref phase) = step.phase {
                *counts.entry(phase.clone()).or_insert(0) += 1;
            } else {
                // Steps without explicit phase are considered "unknown"
                *counts.entry("unknown".to_string()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Execute only setup phase steps.
    ///
    /// This runs setup steps (shell commands, workflows, etc.) that prepare the
    /// environment before the verification loop begins. Setup steps run ONCE
    /// at the start of the workflow.
    ///
    /// Returns the execution result and whether setup completed successfully.
    pub async fn execute_setup_phase(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> (ExecutionResult, bool) {
        let setup_steps = Self::filter_steps_by_phase(steps, "setup");

        if setup_steps.is_empty() {
            info!("No setup steps to execute, setup phase complete by default");
            return (
                ExecutionResult {
                    success: true,
                    total_steps: 0,
                    successful_steps: 0,
                    failed_steps: 0,
                    total_duration_ms: 0,
                    steps: vec![],
                    captured_logs: None,
                    captured_runner_logs: None,
                    verification_passed: None,
                    loop_result: None,
                    task_summary: None,
                },
                true, // Setup phase complete
            );
        }

        info!(
            "Executing {} setup phase steps for {}",
            setup_steps.len(),
            execution_id
        );

        let result = self
            .execute_steps_with_log_sources(&setup_steps, execution_id, log_sources)
            .await;

        let setup_complete = result.success;

        info!(
            "Setup phase {}: {} of {} steps succeeded",
            if setup_complete { "complete" } else { "failed" },
            result.successful_steps,
            result.total_steps
        );

        (result, setup_complete)
    }

    /// Execute only completion phase steps.
    ///
    /// This runs completion steps (cleanup, reports, notifications) that run
    /// ONCE after the verification loop exits (success or max iterations).
    ///
    /// Returns the execution result.
    pub async fn execute_completion_phase(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> ExecutionResult {
        let completion_steps = Self::filter_steps_by_phase(steps, "completion");

        if completion_steps.is_empty() {
            info!("No completion steps to execute");
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: vec![],
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            };
        }

        info!(
            "Executing {} completion phase steps for {}",
            completion_steps.len(),
            execution_id
        );

        let result = self
            .execute_steps_with_log_sources(&completion_steps, execution_id, log_sources)
            .await;

        info!(
            "Completion phase done: {} of {} steps succeeded",
            result.successful_steps, result.total_steps
        );

        result
    }

    /// Execute only verification/agentic phase steps (for iterations).
    ///
    /// This runs verification and agentic steps that may run on each iteration.
    /// On iteration > 1, setup steps are filtered out (unless marked to run on
    /// subsequent iterations).
    ///
    /// Completion steps are always excluded from this method.
    pub async fn execute_verification_phase(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
        iteration: u32,
    ) -> ExecutionResult {
        // Filter out setup and completion steps, keep only verification/agentic
        let mut verification_steps: Vec<ExecutionStepConfig> = steps
            .iter()
            .filter(|s| {
                let phase = s.phase.as_deref().unwrap_or("unknown");
                // Include verification and agentic phase steps
                phase == "verification" || phase == "agentic"
            })
            .cloned()
            .collect();

        // For iteration > 1, also filter based on run_on_subsequent_iterations
        if iteration > 1 {
            verification_steps.retain(|step| step.should_run_on_iteration(iteration));
        }

        if verification_steps.is_empty() {
            info!(
                "No verification/agentic steps to execute for iteration {}",
                iteration
            );
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: vec![],
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            };
        }

        info!(
            "Executing {} verification/agentic phase steps for iteration {}",
            verification_steps.len(),
            iteration
        );

        self.execute_steps_with_log_sources(&verification_steps, execution_id, log_sources)
            .await
    }

    /// Get current file positions for configured log sources
    fn capture_log_positions(
        log_sources: &[LogSourceConfig],
    ) -> std::collections::HashMap<String, u64> {
        use std::io::{Seek, SeekFrom};

        let mut positions = std::collections::HashMap::new();

        for source in log_sources {
            if !source.enabled {
                continue;
            }

            let path = std::path::Path::new(&source.path);
            if let Ok(mut file) = std::fs::File::open(path) {
                if let Ok(pos) = file.seek(SeekFrom::End(0)) {
                    positions.insert(source.id.clone(), pos);
                }
            }
        }

        positions
    }

    /// Read log content that was written since the given positions
    fn capture_logs_since(
        log_sources: &[LogSourceConfig],
        positions: std::collections::HashMap<String, u64>,
    ) -> Option<CapturedLogs> {
        use std::io::{Read, Seek, SeekFrom};

        let mut sources = std::collections::HashMap::new();

        for source in log_sources {
            if !source.enabled {
                continue;
            }

            let start_pos = positions.get(&source.id).copied().unwrap_or(0);
            let path = std::path::Path::new(&source.path);

            if let Ok(mut file) = std::fs::File::open(path) {
                if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                    let mut content = String::new();
                    if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                        sources.insert(source.name.clone(), content);
                    }
                }
            }
        }

        if sources.is_empty() {
            None
        } else {
            Some(CapturedLogs { sources })
        }
    }

    /// Get the .dev-logs directory path
    fn get_dev_logs_dir() -> PathBuf {
        crate::paths::get_dev_logs_dir()
    }

    /// Get current file positions for runner log files (actions + image recognition)
    fn capture_runner_log_positions() -> HashMap<String, u64> {
        use std::io::{Seek, SeekFrom};

        let mut positions = HashMap::new();
        let dev_logs = Self::get_dev_logs_dir();

        // Track positions for runner-actions.jsonl and runner-image-recognition.jsonl
        for filename in &["runner-actions.jsonl", "runner-image-recognition.jsonl"] {
            let path = dev_logs.join(filename);
            if let Ok(mut file) = std::fs::File::open(&path) {
                if let Ok(pos) = file.seek(SeekFrom::End(0)) {
                    positions.insert(filename.to_string(), pos);
                    info!(
                        "Captured runner log position for {}: {} bytes",
                        filename, pos
                    );
                }
            }
        }

        positions
    }

    /// Read runner logs that were written since the given positions
    fn capture_runner_logs_since(positions: HashMap<String, u64>) -> Option<CapturedRunnerLogs> {
        use std::io::{Read, Seek, SeekFrom};

        let dev_logs = Self::get_dev_logs_dir();
        let mut actions = Vec::new();
        let mut image_recognition = Vec::new();

        // Read runner-actions.jsonl
        let actions_path = dev_logs.join("runner-actions.jsonl");
        let start_pos = positions.get("runner-actions.jsonl").copied().unwrap_or(0);
        if let Ok(mut file) = std::fs::File::open(&actions_path) {
            if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                let mut content = String::new();
                if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                    actions = parse_action_events(&content);
                    info!("Captured {} action events from runner log", actions.len());
                }
            }
        }

        // Read runner-image-recognition.jsonl
        let ir_path = dev_logs.join("runner-image-recognition.jsonl");
        let start_pos = positions
            .get("runner-image-recognition.jsonl")
            .copied()
            .unwrap_or(0);
        if let Ok(mut file) = std::fs::File::open(&ir_path) {
            if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                let mut content = String::new();
                if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                    image_recognition = parse_image_recognition_events(&content);
                    info!(
                        "Captured {} image recognition events from runner log",
                        image_recognition.len()
                    );
                }
            }
        }

        if actions.is_empty() && image_recognition.is_empty() {
            None
        } else {
            Some(CapturedRunnerLogs {
                actions,
                image_recognition,
            })
        }
    }

    /// Execute a single step and return (success, error, screenshot_path, output_data)
    async fn execute_single_step(
        &self,
        step: &ExecutionStepConfig,
    ) -> (
        bool,
        Option<String>,
        Option<String>,
        Option<serde_json::Value>,
    ) {
        // Try to use the handler registry for polymorphic dispatch.
        // This is the new modular approach - handlers are self-contained and testable.
        // If no handler is registered, fall back to the legacy match statement below.
        // Normalize "test" → "command" for backward compatibility with saved workflows
        let lookup_type = if step.step_type == "test" {
            "command"
        } else {
            &step.step_type
        };
        if let Some(handler) = self.handler_registry.get(lookup_type) {
            let context = self.create_handler_context();
            let result = handler.execute(step, &context).await;
            return (
                result.success,
                result.error,
                result.screenshot_path,
                result.output_data,
            );
        }

        // Fallback match statement for step types without registered handlers.
        // Most step types are handled by the handler registry dispatch above.

        // Timeouts are disabled by default - only apply if explicitly specified
        let timeout = step.timeout_seconds;

        match step.step_type.as_str() {
            // NOTE: "test" is no longer a separate step type — it's dispatched through CommandHandler
            // via the lookup_type normalization above. This legacy arm is kept only as a safety net.
            "test" => {
                // Execute verification test with tree event emission
                use std::sync::atomic::{AtomicU32, Ordering};
                static TEST_SEQUENCE: AtomicU32 = AtomicU32::new(1);
                let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::SeqCst);
                let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let action_id = format!("test-{}", sequence);
                let step_name = step
                    .name
                    .clone()
                    .unwrap_or_else(|| "Verification Test".to_string());
                let test_id_display = step
                    .test_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let is_critical = step.test_is_critical.unwrap_or(false);

                // Build action node for tree events
                let action_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("TEST: {}", step_name),
                    "timestamp": timestamp,
                    "status": "pending",
                    "metadata": {
                        "test_id": &test_id_display,
                        "is_critical": is_critical,
                    }
                });

                // Emit action_started tree event to file log
                FileLogger::log_tree_event(
                    "action_started",
                    &action_node,
                    &[],
                    timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_started".to_string(),
                        timestamp,
                        data: json!({ "node": action_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_started", &action_node, timestamp, sequence);

                // Execute the test
                let result = if let Some(ref test_id) = step.test_id {
                    // Execute stored verification test by ID
                    match self.execute_verification_test(test_id, is_critical).await {
                        Ok((success, error)) => (success, error, None, None),
                        Err(e) => (
                            false,
                            Some(format!("Test execution error: {}", e)),
                            None,
                            None,
                        ),
                    }
                } else if step.test_type.as_deref() == Some("repository") {
                    // Repository test: run a command in the working directory
                    let command = step
                        .check_command
                        .clone()
                        .or_else(|| step.shell_command.clone())
                        .unwrap_or_else(|| "pytest".to_string());
                    let working_dir = step
                        .check_working_directory
                        .clone()
                        .or_else(|| step.shell_command_working_directory.clone())
                        .unwrap_or_else(|| {
                            std::env::current_dir()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|_| ".".to_string())
                        });
                    // Resolve relative paths to absolute
                    let working_dir = {
                        let p = std::path::Path::new(&working_dir);
                        if p.is_relative() {
                            std::env::current_dir()
                                .ok()
                                .map(|cwd| {
                                    let resolved = cwd.join(p);
                                    resolved.canonicalize().unwrap_or(resolved).to_string_lossy().to_string()
                                })
                                .unwrap_or(working_dir)
                        } else {
                            working_dir
                        }
                    };

                    info!("Executing repository test: {} in {}", command, working_dir);

                    // Create a temporary step config for the shell command execution
                    let temp_step = ExecutionStepConfig {
                        shell_command: Some(command.clone()),
                        shell_command_working_directory: Some(working_dir),
                        ..Default::default()
                    };
                    // Timeouts are disabled by default
                    let timeout = step.timeout_seconds;
                    let (s, e, p) = self.execute_shell_command_step(&temp_step, timeout).await;
                    (s, e, p, None)
                } else {
                    (
                        false,
                        Some("No test ID specified and test_type is not 'repository'".to_string()),
                        None,
                        None,
                    )
                };

                let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let duration = end_timestamp - timestamp;
                let (success, ref error_opt, _, _) = result;

                // Build completed/failed node
                let completed_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("TEST: {}", step_name),
                    "timestamp": end_timestamp,
                    "status": if success { "success" } else { "failed" },
                    "duration": duration,
                    "error": error_opt.clone(),
                    "metadata": {
                        "test_id": &test_id_display,
                        "is_critical": is_critical,
                    }
                });

                let event_type = if success {
                    "action_completed"
                } else {
                    "action_failed"
                };

                // Emit tree event to file log
                FileLogger::log_tree_event(
                    event_type,
                    &completed_node,
                    &[],
                    end_timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: event_type.to_string(),
                        timestamp: end_timestamp,
                        data: json!({ "node": completed_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

                result
            }
            "prompt" => {
                // Prompt steps are text for the AI, not executed here - emit tree events for UI visibility
                use std::sync::atomic::{AtomicU32, Ordering};
                static PROMPT_SEQUENCE: AtomicU32 = AtomicU32::new(1);
                let sequence = PROMPT_SEQUENCE.fetch_add(1, Ordering::SeqCst);
                let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let action_id = format!("prompt-{}", sequence);
                let step_name = step.name.clone().unwrap_or_else(|| "AI Prompt".to_string());
                let prompt_text = step.prompt_content.clone().unwrap_or_default();
                let prompt_preview = if prompt_text.len() > 100 {
                    format!("{}...", truncate_str(&prompt_text, 100))
                } else {
                    prompt_text.clone()
                };

                // Build action node for tree events
                let action_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("PROMPT: {}", step_name),
                    "timestamp": timestamp,
                    "status": "pending",
                    "metadata": {
                        "prompt_preview": prompt_preview,
                        "type": "ai_prompt",
                    }
                });

                // Emit action_started tree event to file log
                FileLogger::log_tree_event(
                    "action_started",
                    &action_node,
                    &[],
                    timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_started".to_string(),
                        timestamp,
                        data: json!({ "node": action_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_started", &action_node, timestamp, sequence);

                // Prompt steps complete immediately (text is passed to AI, not executed here)
                let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;

                // Build completed node
                let completed_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("PROMPT: {}", step_name),
                    "timestamp": end_timestamp,
                    "status": "success",
                    "duration": end_timestamp - timestamp,
                    "metadata": {
                        "prompt_preview": prompt_preview,
                        "type": "ai_prompt",
                        "note": "Prompt text passed to AI for processing",
                    }
                });

                // Emit action_completed tree event to file log
                FileLogger::log_tree_event(
                    "action_completed",
                    &completed_node,
                    &[],
                    end_timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_completed".to_string(),
                        timestamp: end_timestamp,
                        data: json!({ "node": completed_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_completed", &completed_node, end_timestamp, sequence);

                (true, None, None, None)
            }
            // ================================================================
            // Shell Command Step Type
            // ================================================================
            "shell_command" => {
                let (s, e, p) = self.execute_shell_command_step(step, timeout).await;
                (s, e, p, None)
            }
            // ================================================================
            // Check Step Type (code quality checks)
            // ================================================================
            "check" => {
                let (s, e, p) = self.execute_check_step(step, timeout).await;
                (s, e, p, None)
            }
            // ================================================================
            // Check Group Step Type (run all checks in a group)
            // ================================================================
            "check_group" => {
                let (success, error, summary, _check_results) =
                    self.execute_check_group_step(step, timeout).await;
                (success, error, summary, None)
            }
            // ================================================================
            // Shell Step Type (execute shell command)
            // ================================================================
            "shell" => {
                // Timeouts are disabled by default
                let timeout = step.timeout_seconds;
                let (success, error, output) = self.execute_shell_command_step(step, timeout).await;
                // Return output as the third element for potential logging
                (success, error, output, None)
            }
            // ================================================================
            // Log Watch Step Type (scan dev logs for errors)
            // ================================================================
            "log_watch" => {
                let (success, error, output) = self.execute_log_watch_step(step).await;
                (success, error, output, None)
            }
            // ================================================================
            // Gate Step Type (aggregate verification results)
            // ================================================================
            "gate" => {
                // The gate step is a semantic aggregation marker used by workflow
                // generation. Actual pass/fail aggregation is handled by
                // execute_verification_steps_with_events which checks all required
                // steps. The gate step itself always succeeds at execution time.
                info!(
                    "Gate step '{}' executed (aggregation handled by verification executor)",
                    step.name.as_deref().unwrap_or("unnamed")
                );
                (true, None, None, None)
            }
            _ => {
                // Delegate to handler registry for any unrecognized step type
                warn!("Unknown step type: {}", step.step_type);
                (
                    false,
                    Some(format!("Unknown step type: {}", step.step_type)),
                    None,
                    None,
                )
            }
        }
    }

    /// Run a Playwright test script via HTTP API
    #[tracing::instrument(
        name = "playwright.test.script",
        skip(self),
        fields(
            test_name = %script_id
        )
    )]
    async fn run_playwright_script(
        &self,
        script_id: &str,
    ) -> (bool, Option<String>, Option<String>) {
        let client = reqwest::Client::new();
        let base_url = crate::mcp::types::get_self_base_url_from_env();
        let url = format!("{}/playwright/tests/{}/run", base_url, script_id);

        match client
            .post(&url)
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
        {
            Ok(response) => {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    let success = json
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let error = if !success {
                        json.get("error")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    };
                    (success, error, None)
                } else {
                    (
                        false,
                        Some("Failed to parse Playwright response".to_string()),
                        None,
                    )
                }
            }
            Err(e) => (
                false,
                Some(format!("Playwright request error: {}", e)),
                None,
            ),
        }
    }

    /// Run inline Playwright script content (for combined scripts)
    ///
    /// This runs script content directly without needing a script ID.
    /// Used for combined setup+verification scripts.
    #[tracing::instrument(
        name = "playwright.test.inline",
        skip(self, content),
        fields(
            test_name = %script_name,
            content_length = %content.len(),
            target_url = ?target_url
        )
    )]
    async fn run_playwright_inline(
        &self,
        content: &str,
        target_url: Option<&str>,
        script_name: &str,
    ) -> (bool, Option<String>, Option<String>) {
        info!(
            "Running inline Playwright script: {} ({} chars)",
            script_name,
            content.len()
        );

        // Run the inline script using the playwright executor
        match crate::playwright::run_script_inline(content, target_url, script_name) {
            Ok(result) => {
                let error = if !result.passed {
                    result.error.clone()
                } else {
                    None
                };
                (result.passed, error, None)
            }
            Err(e) => (false, Some(format!("Inline Playwright error: {}", e)), None),
        }
    }

    /// Execute a verification test by ID and return simplified (success, error) tuple
    ///
    /// This is the legacy interface used by execute_single_step.
    async fn execute_verification_test(
        &self,
        test_id: &str,
        is_critical: bool,
    ) -> Result<(bool, Option<String>), String> {
        use crate::test_executor::TestStatus;

        let result = self.execute_verification_test_with_details(test_id).await?;

        // Log the result
        if result.status == TestStatus::Passed {
            info!(
                "Test '{}' passed in {}ms ({}/{} assertions)",
                result.test_name,
                result.duration_ms,
                result.assertions_passed,
                result.assertions_passed + result.assertions_failed
            );
            Ok((true, None))
        } else {
            let error_msg = format!(
                "Test '{}' {}: {} ({}/{} assertions passed)",
                result.test_name,
                match result.status {
                    TestStatus::Failed => "failed",
                    TestStatus::Error => "errored",
                    TestStatus::Timeout => "timed out",
                    _ => "did not pass",
                },
                result.error.as_deref().unwrap_or("Unknown error"),
                result.assertions_passed,
                result.assertions_passed + result.assertions_failed
            );

            warn!("{}", error_msg);

            // If critical, report as step failure; otherwise, log but succeed
            if is_critical {
                Ok((false, Some(error_msg)))
            } else {
                info!("Non-critical test failure - step continues");
                Ok((true, Some(format!("(Non-critical) {}", error_msg))))
            }
        }
    }

    /// Execute a verification test by ID and return the full TestExecutionResult
    ///
    /// This provides rich details for verification phase context building.
    async fn execute_verification_test_with_details(
        &self,
        test_id: &str,
    ) -> Result<crate::test_executor::TestExecutionResult, String> {
        use crate::database::TestType as DbTestType;
        use crate::test_executor::{self, TestCategory, TestDefinition, TestType, VisionConfig};

        info!("Executing verification test with details: {}", test_id);

        // Get the test from database
        let verification_test = self
            .app_state
            .checkpoint_db
            .get_verification_test(test_id)?
            .ok_or_else(|| format!("Verification test not found: {}", test_id))?;

        // Convert database TestType to test_executor TestType
        let test_type = match verification_test.test_type {
            DbTestType::PlaywrightCdp => TestType::PlaywrightCdp,
            DbTestType::QontinuiVision => TestType::QontinuiVision,
            DbTestType::PythonScript => TestType::PythonScript,
            DbTestType::RepositoryTest => TestType::RepositoryTest,
        };

        // Parse vision config if present
        let vision_config: Option<VisionConfig> = verification_test
            .vision_config
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Parse repo test config if present
        let repo_test_config = verification_test
            .repo_test_config
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Convert to TestDefinition
        let test_def = TestDefinition {
            id: verification_test.id.clone(),
            name: verification_test.name.clone(),
            test_type,
            category: TestCategory::Custom, // Default to Custom
            playwright_code: verification_test.playwright_code.clone(),
            vision_config,
            python_code: verification_test.python_code.clone(),
            repo_test_config,
            timeout_seconds: verification_test.timeout_seconds.unwrap_or(60),
            is_critical: verification_test.is_critical,
            config: verification_test.config.clone(),
        };

        // Execute the test (synchronous)
        let result = test_executor::execute_test(&test_def);

        Ok(result)
    }

    /// Execute all verification steps and return a VerificationPhaseResult
    ///
    /// This is the main entry point for the verification phase in the
    /// verification-agentic loop. It:
    /// 1. Executes each verification step in order
    /// 2. Captures detailed results for each step
    /// 3. Stops on critical step failure
    /// 4. Returns a summary that can be used to build AI context
    #[tracing::instrument(
        name = "workflow.verification.execute",
        skip(self, steps),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id,
            iteration = %iteration
        )
    )]
    pub async fn execute_verification_steps(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
    ) -> VerificationPhaseResult {
        self.execute_verification_steps_with_events(steps, execution_id, iteration, None)
            .await
    }

    /// Run verification phase steps with optional event emission.
    ///
    /// This version emits completion events as each step finishes, allowing
    /// the UI to show real-time progress instead of waiting until all steps complete.
    #[tracing::instrument(
        name = "workflow.verification.with_events",
        skip(self, steps),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id,
            iteration = %iteration,
            workflow_name = ?workflow_name
        )
    )]
    pub async fn execute_verification_steps_with_events(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
        workflow_name: Option<&str>,
    ) -> VerificationPhaseResult {
        use crate::step_event_builder::StepEventBuilder;
        use crate::step_metadata::{StepDetails, StepMetadata};
        use crate::step_types::StepType;
        use crate::test_executor::TestStatus;
        use crate::workflow_state::{CheckpointManager, StepCheckpoint};
        use std::time::Instant;

        // For workflow sequence children, use parent ID for event logging (FK constraint)
        let event_execution_id = get_parent_task_id(execution_id);
        let start = Instant::now();
        let mut step_results = Vec::new();
        let mut passed_steps = 0;
        let mut failed_steps = 0;
        let mut skipped_steps = 0;
        let critical_failure = false;

        // Filter to only verification phase steps
        let verification_steps: Vec<_> = steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("verification"))
            .collect();

        info!(
            "Executing {} verification steps for iteration {}",
            verification_steps.len(),
            iteration
        );

        // Track whether a navigation step has been seen, so we can auto-inject
        // retries for subsequent SDK steps (WebSocket reconnection takes ~15s).
        let mut after_navigation = false;

        for (index, step) in verification_steps.iter().enumerate() {
            // Skip remaining steps if we had a critical failure
            if critical_failure {
                let skipped_at = chrono::Utc::now().to_rfc3339();
                let result = StepExecutionResult {
                    step_index: index,
                    step_name: step
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("Step {}", index + 1)),
                    step_type: step.step_type.clone(),
                    step_id: step.id.clone(),
                    success: false,
                    error: Some("Skipped due to critical failure".to_string()),
                    screenshot_path: None,
                    started_at: Some(skipped_at.clone()),
                    ended_at: Some(skipped_at),
                    duration_ms: 0,
                    config: StepExecutionConfig {
                        timeout_seconds: None,
                        check_type: None,
                        command: None,
                        test_id: step.test_id.clone(),
                        test_type: step.test_type.clone(),
                        working_directory: None,
                        ui_bridge_action: None,
                    },
                    verification_details: None,
                    output_data: None,
                    required: step.required,
                    resolved_inputs: None,
                    extracted_values: None,
                    failure_category: None,
                    interrupted: None,
                };
                step_results.push(result);
                skipped_steps += 1;
                continue;
            }

            let step_start = Instant::now();
            let step_started_at = chrono::Utc::now().to_rfc3339();
            let step_name = step
                .name
                .clone()
                .unwrap_or_else(|| format!("Step {}", index + 1));

            // Runtime command sanitization: replace jq with python (jq unavailable on Windows MSYS)
            // This is a safety net in case the hardener didn't process the workflow at generation time.
            let step = if step.step_type == "command" || step.step_type == "shell" {
                if let Some(ref cmd) = step.shell_command {
                    if cmd.contains("| jq ") {
                        let sanitized = super::handlers::shell_command::ShellCommandHandler::replace_jq_with_python_static(cmd);
                        if sanitized != *cmd {
                            info!("Verification executor: jq→python replacement applied for step '{}'", step_name);
                            let mut patched = (*step).clone();
                            patched.shell_command = Some(sanitized);
                            std::borrow::Cow::Owned(patched)
                        } else {
                            std::borrow::Cow::Borrowed(*step)
                        }
                    } else {
                        std::borrow::Cow::Borrowed(*step)
                    }
                } else {
                    std::borrow::Cow::Borrowed(*step)
                }
            } else {
                std::borrow::Cow::Borrowed(*step)
            };
            let step = step.as_ref();

            // Track navigation steps for auto-retry injection
            let cmd_str = step.shell_command.as_deref().unwrap_or("");
            if cmd_str.contains("sdk/page/navigate") {
                after_navigation = true;
            }

            // Determine retry configuration: explicit from step config, or auto-inject
            // for SDK steps that follow a navigation step (WebSocket reconnection delay).
            let (max_retries, retry_delay) = if step.retry_count.is_some() {
                // Explicit retry config takes precedence
                (
                    step.retry_count.unwrap_or(0),
                    step.retry_delay_ms.unwrap_or(2000),
                )
            } else if after_navigation
                && cmd_str.contains("ui-bridge/sdk/")
                && !cmd_str.contains("sdk/page/navigate")
            {
                // Auto-inject retries for SDK verification steps after page navigation.
                // After navigation, the WebSocket connection needs time to reconnect (~15s).
                info!(
                    "Auto-injecting retries for SDK step '{}' after navigation",
                    step_name
                );
                (3_u32, 3000_u64)
            } else {
                (0, 2000)
            };

            // Stop auto-retry injection after hitting a non-SDK step
            if after_navigation
                && !cmd_str.is_empty()
                && !cmd_str.contains("ui-bridge/sdk/")
                && !cmd_str.contains("sdk/page/navigate")
            {
                after_navigation = false;
            }

            // Execute with retry loop
            let (mut success, mut error, mut verification_details, step_output_data) = {
                let mut last_result = (false, Some("not executed".to_string()), None, None);
                for attempt in 0..=max_retries {
                    if attempt > 0 {
                        info!(
                            "Retrying verification step '{}' (attempt {}/{}, delay {}ms)",
                            step_name,
                            attempt + 1,
                            max_retries + 1,
                            retry_delay
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(retry_delay)).await;
                    }

                    last_result = match step.step_type.as_str() {
                        "test" => {
                            if let Some(ref test_id) = step.test_id {
                                match self.execute_verification_test_with_details(test_id).await {
                                    Ok(test_result) => {
                                        let passed = test_result.status == TestStatus::Passed;
                                        let details = VerificationStepDetails {
                                            step_id: step
                                                .name
                                                .clone()
                                                .unwrap_or_else(|| format!("step-{}", index)),
                                            phase: "verification".to_string(),
                                            stdout: Some(test_result.output.clone()),
                                            stderr: None,
                                            assertions_passed: Some(test_result.assertions_passed),
                                            assertions_total: Some(
                                                test_result.assertions_passed
                                                    + test_result.assertions_failed,
                                            ),
                                            console_output: test_result
                                                .structured_output
                                                .as_ref()
                                                .and_then(|v| v.get("console_output"))
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string()),
                                            page_snapshot: test_result
                                                .structured_output
                                                .as_ref()
                                                .and_then(|v| v.get("page_snapshot"))
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string()),
                                            exit_code: test_result.exit_code,
                                            check_results: None,
                                            console_errors: None,
                                        };
                                        (
                                            passed,
                                            if passed {
                                                None
                                            } else {
                                                test_result.error.clone()
                                            },
                                            Some(details),
                                            None,
                                        )
                                    }
                                    Err(e) => (
                                        false,
                                        Some(format!("Test execution error: {}", e)),
                                        Some(VerificationStepDetails {
                                            step_id: step
                                                .name
                                                .clone()
                                                .unwrap_or_else(|| format!("step-{}", index)),
                                            phase: "verification".to_string(),
                                            stderr: Some(e),
                                            ..Default::default()
                                        }),
                                        None,
                                    ),
                                }
                            } else {
                                // No test_id — delegate to handler system which supports
                                // repository tests, inline commands (check_command/shell_command),
                                // and auto-detection fallbacks.
                                let (success, handler_error, _screenshot, handler_output_data) =
                                    self.execute_single_step(step).await;
                                let details = VerificationStepDetails {
                                    step_id: step
                                        .name
                                        .clone()
                                        .unwrap_or_else(|| format!("step-{}", index)),
                                    phase: "verification".to_string(),
                                    ..Default::default()
                                };
                                (success, handler_error, Some(details), handler_output_data)
                            }
                        }
                        "check" => {
                            // Execute check step (shell command for checks like lint, typecheck, etc.)
                            // Output is extracted by post-match normalization from handler_output_data.
                            let (success, error, _screenshot, handler_output_data) =
                                self.execute_single_step(step).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                // stdout filled by post-match normalization from handler_output_data
                                ..Default::default()
                            };
                            (success, error, Some(details), handler_output_data)
                        }
                        "shell" => {
                            // Execute shell command step
                            // Timeouts are disabled by default
                            let timeout = step.timeout_seconds;
                            let (success, error, output) =
                                self.execute_shell_command_step(step, timeout).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                stdout: output, // Capture output for AI context
                                ..Default::default()
                            };
                            (success, error, Some(details), None)
                        }
                        "check_group" => {
                            // Execute check group - runs all checks in the group
                            // Timeouts are disabled by default
                            let timeout = step.timeout_seconds;
                            let (success, error, summary, check_results) =
                                self.execute_check_group_step(step, timeout).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                // Capture the detailed summary with all check results for AI context
                                stdout: summary,
                                // Include structured check results for UI display
                                check_results,
                                ..Default::default()
                            };
                            (success, error, Some(details), None)
                        }
                        "log_watch" => {
                            // Execute log watch step (scans dev logs for errors)
                            let (success, error, output) = self.execute_log_watch_step(step).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                stdout: output,
                                ..Default::default()
                            };
                            (success, error, Some(details), None)
                        }
                        "gate" => {
                            // Gate step is a semantic aggregation marker. Actual pass/fail
                            // aggregation is handled by the verification phase result logic.
                            // The gate step itself always succeeds at execution time.
                            info!(
                        "Gate step '{}' executed (aggregation handled by verification executor)",
                        step.name.as_deref().unwrap_or("unnamed")
                    );
                            (true, None, None, None)
                        }
                        "prompt" => {
                            // AI Review verification step — dispatch via handler with iteration context
                            let mut handler_ctx = self.create_handler_context();
                            handler_ctx.iteration = Some(iteration);
                            let handler = super::handlers::PromptStepHandler;
                            let result = handler.execute(step, &handler_ctx).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                stdout: result
                                    .output_data
                                    .as_ref()
                                    .and_then(|d| d.get("reasoning"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string()),
                                ..Default::default()
                            };
                            (
                                result.success,
                                result.error,
                                Some(details),
                                result.output_data,
                            )
                        }
                        _ => {
                            // Generic handler for all other step types in verification.
                            // Output is captured by the post-match normalization block below.
                            let (success, error, screenshot, handler_output_data) =
                                self.execute_single_step(step).await;
                            let details = screenshot.map(|s| VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                stdout: Some(s),
                                ..Default::default()
                            });
                            (success, error, details, handler_output_data)
                        }
                    };

                    // If step succeeded or no retries left, break out
                    if last_result.0 || attempt >= max_retries {
                        break;
                    }
                }
                last_result
            };

            // === Post-match normalization ===
            // Ensure every verification step has VerificationStepDetails with stdout
            // populated. This makes output available to the agentic phase regardless
            // of step type. Handlers put their output in different places (stdout,
            // output_data, check_results), so we normalize here.
            if verification_details.is_none() {
                // No verification_details at all — extract text from output_data
                let extracted = extract_text_from_output_data(&step_output_data);
                if extracted.is_some() || !success {
                    verification_details = Some(VerificationStepDetails {
                        step_id: step
                            .name
                            .clone()
                            .unwrap_or_else(|| format!("step-{}", index)),
                        phase: "verification".to_string(),
                        stdout: extracted,
                        ..Default::default()
                    });
                }
            } else if let Some(ref mut details) = verification_details {
                // verification_details exists but stdout is None — fill from output_data
                if details.stdout.is_none() {
                    details.stdout = extract_text_from_output_data(&step_output_data);
                }
            }

            // Extract consoleErrors from output_data and attach to verification_details
            if let Some(ref mut details) = verification_details {
                if details.console_errors.is_none() {
                    if let Some(ref output) = step_output_data {
                        // consoleErrors may be at top level (action response) or nested in spec_result
                        let errors = output
                            .get("consoleErrors")
                            .or_else(|| {
                                output
                                    .get("spec_result")
                                    .and_then(|sr| sr.get("consoleErrors"))
                            })
                            .and_then(|v| v.as_array())
                            .cloned();
                        if errors.as_ref().is_some_and(|e| !e.is_empty()) {
                            details.console_errors = errors;
                        }
                    }
                }
            }

            // Auto-fail: if fail_on_console_errors is set and console errors were captured,
            // flip a passing step to failed
            if success && step.fail_on_console_errors {
                if let Some(ref details) = verification_details {
                    if details
                        .console_errors
                        .as_ref()
                        .is_some_and(|e| !e.is_empty())
                    {
                        let count = details.console_errors.as_ref().map_or(0, |e| e.len());
                        warn!(
                            "Verification step '{}' passed but has {} console error(s) — failing due to fail_on_console_errors",
                            step_name, count
                        );
                        success = false;
                        error = Some(format!(
                            "Step passed but {} console error(s) detected (fail_on_console_errors=true)",
                            count
                        ));
                    }
                }
            }

            let duration_ms = step_start.elapsed().as_millis() as u64;

            if success {
                passed_steps += 1;
                info!(
                    "Verification step '{}' passed in {}ms",
                    step_name, duration_ms
                );
            } else {
                failed_steps += 1;
                warn!(
                    "Verification step '{}' failed: {:?}",
                    step_name,
                    error.as_deref().unwrap_or("unknown error")
                );

                // Note: critical_failure is set by connectivity or infrastructure failures
            }

            let step_ended_at = chrono::Utc::now().to_rfc3339();

            // Auto-detect failure category from step output
            let failure_category = if !success {
                let output_text = verification_details
                    .as_ref()
                    .and_then(|d| d.stdout.as_deref())
                    .unwrap_or("")
                    .to_string()
                    + verification_details
                        .as_ref()
                        .and_then(|d| d.stderr.as_deref())
                        .unwrap_or("")
                    + error.as_deref().unwrap_or("");
                Some(categorize_failure(&output_text).to_string())
            } else {
                None
            };

            let result = StepExecutionResult {
                step_index: index,
                step_name,
                step_type: step.step_type.clone(),
                step_id: step.id.clone(),
                success,
                error,
                screenshot_path: None,
                started_at: Some(step_started_at),
                ended_at: Some(step_ended_at),
                duration_ms,
                config: StepExecutionConfig {
                    timeout_seconds: step.timeout_seconds,
                    check_type: step.check_type.clone(),
                    command: step
                        .check_command
                        .clone()
                        .or_else(|| step.shell_command.clone()),
                    test_id: step.test_id.clone(),
                    test_type: step.test_type.clone(),
                    working_directory: step
                        .check_working_directory
                        .clone()
                        .or_else(|| step.shell_command_working_directory.clone()),
                    ui_bridge_action: step.ui_bridge_action.clone(),
                },
                verification_details,
                output_data: step_output_data,
                required: step.required,
                resolved_inputs: None,
                extracted_values: None,
                failure_category,
                interrupted: None,
            };

            // Emit completion event for this step (real-time UI update)
            // This allows the frontend to show progress as each step finishes
            if workflow_name.is_some() {
                let step_type_enum =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
                let metadata = StepMetadata::verification(
                    &event_execution_id, // Use parent ID for FK constraint
                    step_type_enum,
                    &result.step_name,
                    index,
                    iteration,
                );

                let details = if result.success {
                    StepDetails::default().with_duration(duration_ms as i64)
                } else {
                    StepDetails::default()
                        .with_duration(duration_ms as i64)
                        .with_error(result.error.clone().unwrap_or_default())
                };

                let builder = StepEventBuilder::new(&event_execution_id, metadata) // Use parent ID
                    .with_details(details)
                    .with_workflow_name(workflow_name.unwrap_or_default());

                let event = if result.success {
                    builder.build_complete(duration_ms as i64)
                } else {
                    builder.build_error(duration_ms as i64, result.error.as_deref())
                };

                if let Err(e) = self.app_state.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to emit verification step completion event: {}", e);
                }
            }

            // Update step checkpoint to reflect completion progressively
            // (enables real-time UI updates via /task-runs/{id}/full-state)
            {
                let checkpoint_mgr =
                    CheckpointManager::new(self.app_state.checkpoint_db.clone(), "unified");
                let cp_step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "verification",
                    Some(iteration),
                    index,
                    cp_step_type.as_str(),
                )
                .with_step_name(&result.step_name)
                .with_stage_index(None);

                let result_json_str = serde_json::to_string(&result).ok();
                if result.success {
                    checkpoint.mark_success(result_json_str, duration_ms as i64);
                } else {
                    checkpoint.mark_failed(
                        result.error.as_deref().unwrap_or("Unknown error"),
                        duration_ms as i64,
                    );
                    // Also store result_json for failed steps so resume can access details
                    checkpoint.result_json = result_json_str;
                }

                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!("Failed to update verification step checkpoint: {}", e);
                }

                // Broadcast step-progress to WebSocket clients so the web dashboard refetches
                if let Some(ref app_handle) = self.app_handle {
                    let status = if result.success { "success" } else { "failed" };
                    crate::event_system::broadcast_ws_notification(
                        app_handle,
                        "step-progress",
                        &serde_json::json!({
                            "task_run_id": execution_id,
                            "step_index": index,
                            "step_name": result.step_name,
                            "status": status,
                        }),
                    );
                }
            }

            step_results.push(result);
        }

        let total_duration_ms = start.elapsed().as_millis() as u64;

        // Determine all_passed: all required steps must succeed.
        // Steps with required=false are informational only — their failure
        // doesn't trigger the agentic loop.
        let all_passed = step_results.iter().all(|r| {
            let is_required = r.required.unwrap_or(true); // default: required
            r.success || !is_required
        });

        let result = VerificationPhaseResult {
            iteration,
            all_passed,
            total_steps: verification_steps.len(),
            passed_steps,
            failed_steps,
            skipped_steps,
            total_duration_ms,
            step_results,
            critical_failure,
            console_errors: None, // Populated by phases.rs after verification completes
            app_health: None,     // Populated by phases.rs after verification completes
            browser_events: None, // Populated by phases.rs after verification completes
            network_failures: None, // Populated by phases.rs after verification completes
        };

        info!("{}", result.summary());
        result
    }

    // =========================================================================
    // Log Watch Step Execution
    // =========================================================================

    /// Execute a log_watch step: scan .dev-logs/ for error patterns.
    ///
    /// Reads configured log sources and scans the tail of each file for
    /// error patterns (ERROR, Exception, Traceback, etc.). Returns success
    /// with any detected errors in the output string. The log_watch step is
    /// typically non-critical (required=false), so errors are informational.
    async fn execute_log_watch_step(
        &self,
        _step: &ExecutionStepConfig,
    ) -> (bool, Option<String>, Option<String>) {
        use std::io::{BufRead, BufReader};

        let dev_logs = Self::get_dev_logs_dir();
        let source_names = get_default_log_source_names();

        let mut all_errors: Vec<LogError> = Vec::new();
        let mut scanned_sources = 0;

        for source_name in &source_names {
            let log_path = dev_logs.join(source_name);
            if !log_path.exists() {
                continue;
            }

            let file = match std::fs::File::open(&log_path) {
                Ok(f) => f,
                Err(e) => {
                    warn!("log_watch: Could not open {}: {}", source_name, e);
                    continue;
                }
            };

            scanned_sources += 1;

            // Keep only the last 500 + CONTEXT_LINES lines in a ring buffer
            // to avoid reading entire large log files into memory.
            let reader = BufReader::new(file);
            let window_size = 500 + CONTEXT_LINES;
            let mut ring: VecDeque<String> = VecDeque::with_capacity(window_size + 1);
            let mut total_lines: usize = 0;

            for line in reader.lines().map_while(Result::ok) {
                if ring.len() >= window_size {
                    ring.pop_front();
                }
                ring.push_back(line);
                total_lines += 1;
            }

            // The ring now holds the last `window_size` lines of the file.
            // We scan only the last 500 of those (skipping the CONTEXT_LINES prefix
            // which exist solely to provide context_before for the first matches).
            let ring_len = ring.len();
            let scan_start = ring_len.saturating_sub(500);
            // Offset to convert ring index to original file line number
            let ring_offset = total_lines.saturating_sub(ring_len);

            for ring_idx in scan_start..ring_len {
                let line = &ring[ring_idx];
                for pattern in DEFAULT_ERROR_PATTERNS {
                    if line.contains(pattern) {
                        // Collect context lines from the ring buffer
                        let ctx_start = ring_idx.saturating_sub(CONTEXT_LINES);
                        let ctx_end = (ring_idx + CONTEXT_LINES + 1).min(ring_len);

                        let context_before: Vec<String> =
                            ring.range(ctx_start..ring_idx).cloned().collect();
                        let context_after: Vec<String> = if ring_idx + 1 < ctx_end {
                            ring.range(ring_idx + 1..ctx_end).cloned().collect()
                        } else {
                            Vec::new()
                        };

                        all_errors.push(LogError {
                            source: source_name.clone(),
                            line_number: ring_offset + ring_idx + 1,
                            timestamp: None,
                            message: line.clone(),
                            context_before,
                            context_after,
                            error_type: pattern.to_string(),
                        });
                        break; // Only match first pattern per line
                    }
                }
            }
        }

        let output = if all_errors.is_empty() {
            format!(
                "Log watch: scanned {} source(s), no errors detected.",
                scanned_sources
            )
        } else {
            // Deduplicate and limit output
            let error_count = all_errors.len();
            let display_limit = 10;
            let mut summary = format!(
                "Log watch: scanned {} source(s), {} error(s) detected.\n",
                scanned_sources, error_count
            );
            for (i, err) in all_errors.iter().take(display_limit).enumerate() {
                summary.push_str(&format!(
                    "\n[{}] {}:{} — {}\n  {}\n",
                    i + 1,
                    err.source,
                    err.line_number,
                    err.error_type,
                    // Truncate long lines
                    if err.message.len() > 200 {
                        format!("{}...", &err.message[..200])
                    } else {
                        err.message.clone()
                    }
                ));
            }
            if error_count > display_limit {
                summary.push_str(&format!(
                    "\n... and {} more error(s)\n",
                    error_count - display_limit
                ));
            }
            summary
        };

        info!(
            "log_watch: {}",
            if all_errors.is_empty() {
                "clean"
            } else {
                "errors found"
            }
        );

        // log_watch always returns success — it's informational.
        // The step is typically marked required=false so it won't fail the workflow.
        (true, None, Some(output))
    }

    // =========================================================================
    // Shell Command Step Execution
    // =========================================================================

    /// Execute a shell command step
    ///
    /// Check if a command uses bash/Unix syntax that cmd.exe cannot handle.
    /// Delegates to `ShellCommandHandler::is_bash_command()`.
    fn is_bash_command(command: &str) -> bool {
        super::handlers::shell_command::ShellCommandHandler::is_bash_command(command)
    }

    /// Supports variable expansion using `{{variable_name}}` syntax in the command.
    /// Variables are resolved from the runtime context.
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_shell_command_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        use std::process::Stdio;
        use tokio::time::{timeout, Duration};

        // Get the command template - either directly or by looking up shell_command_id from database
        let template_command = match &step.shell_command {
            Some(cmd) => cmd.clone(),
            None => {
                // If no direct command, check for shell_command_id
                if let Some(id) = &step.shell_command_id {
                    match self.app_state.checkpoint_db.get_shell_command(id) {
                        Ok(Some(cmd)) => cmd.command,
                        Ok(None) => {
                            return (
                                false,
                                Some(format!("Shell command not found: {}", id)),
                                None,
                            );
                        }
                        Err(e) => {
                            return (
                                false,
                                Some(format!("Failed to look up shell command {}: {}", id, e)),
                                None,
                            );
                        }
                    }
                } else {
                    return (false, Some("No shell command specified".to_string()), None);
                }
            }
        };

        // Expand variables in the command using runtime context
        let evaluator = ExpressionEvaluator::new();
        let has_variables = evaluator.has_expressions(&template_command);
        let command = evaluator.evaluate(&template_command, &self.runtime_context);

        // Track which variables were resolved (for UI display)
        let resolved_variables: Option<HashMap<String, String>> = if has_variables {
            let expressions = evaluator.find_expressions(&template_command);
            let mut vars = HashMap::new();
            for expr in expressions {
                // Try to resolve the expression to get the value
                let resolved =
                    evaluator.evaluate(&format!("{{{{{}}}}}", expr), &self.runtime_context);
                // Only include if it was actually resolved (doesn't still contain braces)
                if !resolved.contains("{{") {
                    vars.insert(expr, resolved);
                }
            }
            if vars.is_empty() {
                None
            } else {
                Some(vars)
            }
        } else {
            None
        };

        // Log variable expansion if applicable
        if has_variables {
            info!(
                "Shell command variables expanded: template='{}' -> resolved='{}'",
                template_command, command
            );
            if let Some(ref vars) = resolved_variables {
                info!("Resolved variables: {:?}", vars);
            }
        }

        // Runtime sanitization: replace jq with python since jq is unavailable on Windows MSYS
        let command = if command.contains("| jq ") {
            let sanitized =
                super::handlers::shell_command::ShellCommandHandler::replace_jq_with_python_static(
                    &command,
                );
            if sanitized != command {
                info!(
                    "Legacy executor: jq→python replacement applied: {}",
                    &sanitized[..sanitized.len().min(100)]
                );
            }
            sanitized
        } else {
            command
        };

        // Runtime sanitization: on Windows, Python outputs \r\n line endings which
        // corrupt URLs when piped through xargs (curl rejects \r in URL paths).
        // Insert `tr -d '\r'` before xargs to strip carriage returns.
        let command = if cfg!(target_os = "windows") && command.contains("| xargs") {
            let sanitized = command.replace("| xargs", "| tr -d '\\r' | xargs");
            if sanitized != command {
                info!("Windows CR sanitization: inserted tr -d '\\r' before xargs");
            }
            sanitized
        } else {
            command
        };

        let step_name = step.name.as_deref().unwrap_or("Shell Command");
        // Resolve relative paths to absolute so child processes get the correct CWD
        let working_directory = step.shell_command_working_directory.clone().map(|wd| {
            let p = std::path::Path::new(&wd);
            if p.is_relative() {
                match std::env::current_dir() {
                    Ok(cwd) => {
                        let resolved = cwd.join(p);
                        match resolved.canonicalize() {
                            Ok(abs) => abs.to_string_lossy().to_string(),
                            Err(_) => resolved.to_string_lossy().to_string(),
                        }
                    }
                    Err(_) => wd,
                }
            } else {
                wd
            }
        });
        let fail_on_error = step.shell_command_fail_on_error.unwrap_or(true);

        // Detect if command uses PowerShell syntax
        let is_powershell = command.contains("Get-")
            || command.contains("Set-")
            || command.contains("New-")
            || command.contains("Remove-")
            || command.contains("Invoke-")
            || command.contains("ForEach-Object")
            || command.contains("Where-Object")
            || command.contains("Select-Object")
            || command.contains("$_")
            || command.contains("$env:")
            || command.contains("-ErrorAction")
            || command.contains("| %")
            || command.contains("| ?");

        // Detect bash/Unix commands that cmd.exe cannot handle
        let is_bash = !is_powershell && Self::is_bash_command(&command);

        let shell_type = if cfg!(target_os = "windows") && is_powershell {
            "powershell"
        } else if cfg!(target_os = "windows") && is_bash {
            "bash"
        } else if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };

        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Executing shell command '{}': {} (shell: {}, timeout: {}, working_dir: {:?})",
            step_name, command, shell_type, timeout_str, working_directory
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static SHELL_COMMAND_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = SHELL_COMMAND_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("shell-command-{}", sequence);

        // Truncate command for display (first 50 chars)
        let command_display = if command.len() > 50 {
            format!("{}...", truncate_str(&command, 50))
        } else {
            command.clone()
        };

        // Build action node for tree events
        let action_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("SHELL: {}", step_name),
            "timestamp": timestamp,
            "status": "pending",
            "metadata": {
                "command": &command_display,
                "shell_type": shell_type,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "timeout_seconds": timeout_secs,
            }
        });

        // Emit action_started tree event to file log
        FileLogger::log_tree_event("action_started", &action_node, &[], timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "action_started".to_string(),
                timestamp,
                data: json!({ "node": action_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event("action_started", &action_node, timestamp, sequence);

        // NOTE: Database event logging is handled by the unified workflow executor
        // (execute_steps_with_log_sources -> log_step_event) to avoid duplicates.
        // Tree events above are still emitted for the Session/Actions page.

        // Build the command - use PowerShell for PowerShell syntax, bash for
        // Unix-style commands, and cmd.exe as default on Windows
        let mut cmd = if cfg!(target_os = "windows") {
            if is_powershell {
                let mut c = crate::process_helpers::tokio_no_window("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
                c
            } else if is_bash {
                // Use Git Bash for Unix-style commands on Windows.
                // Resolve the full path to avoid accidentally picking up WSL's
                // bash.exe which can fail when no WSL distro is installed.
                let bash_path =
                    super::handlers::shell_command::ShellCommandHandler::find_git_bash()
                        .unwrap_or_else(|| "bash".to_string());
                let mut c = crate::process_helpers::tokio_no_window(&bash_path);
                // Ensure MSYS2 /usr/bin is on PATH so tools like cat, grep, sed
                // are available even when bash is invoked non-interactively.
                if let Some(usr_bin) = std::path::Path::new(&bash_path).parent() {
                    let usr_bin_str = usr_bin.to_string_lossy();
                    let current_path = std::env::var("PATH").unwrap_or_default();
                    if !current_path.contains(&*usr_bin_str) {
                        c.env("PATH", format!("{};{}", usr_bin_str, current_path));
                    }
                }
                c.args(["-c", &command]);
                c
            } else {
                // cmd.exe doesn't understand single quotes — strip them.
                // Also extract Unix-style KEY=VALUE env var prefixes since
                // cmd.exe doesn't support "KEY=VALUE command" syntax.
                let stripped = command.replace('\'', "");
                let (extra_envs, actual_cmd) = extract_env_prefix_for_cmd(&stripped);
                let mut c = crate::process_helpers::tokio_cmd_no_window();
                c.args(["/C", &actual_cmd]);
                for (key, value) in extra_envs {
                    c.env(key, value);
                }
                c
            }
        } else {
            let mut c = crate::process_helpers::tokio_no_window("sh");
            c.args(["-c", &command]);
            c
        };

        // Set working directory if specified
        if let Some(ref wd) = working_directory {
            cmd.current_dir(wd);
        }

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Execute with optional timeout
        let start = std::time::Instant::now();

        // Process the result - execute with or without timeout depending on setting
        let (success, exit_code, stdout, stderr) = if let Some(timeout_secs_val) = timeout_secs {
            // Execute with timeout
            let timeout_duration = Duration::from_secs(timeout_secs_val);
            let output_result = timeout(timeout_duration, cmd.output()).await;

            match output_result {
                Ok(Ok(output)) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Ok(Err(e)) => {
                    warn!("Failed to execute shell command '{}': {}", step_name, e);
                    (
                        false,
                        None,
                        String::new(),
                        format!("Failed to execute command: {}", e),
                    )
                }
                Err(_) => {
                    warn!(
                        "Shell command '{}' timed out after {}s",
                        step_name, timeout_secs_val
                    );
                    (
                        false,
                        None,
                        String::new(),
                        format!("Command timed out after {} seconds", timeout_secs_val),
                    )
                }
            }
        } else {
            // No timeout - execute without timeout wrapper
            match cmd.output().await {
                Ok(output) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Err(e) => {
                    warn!("Failed to execute shell command '{}': {}", step_name, e);
                    (
                        false,
                        None,
                        String::new(),
                        format!("Failed to execute command: {}", e),
                    )
                }
            }
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        info!(
            "Shell command '{}' completed: success={}, exit_code={:?}, duration={}ms",
            step_name, success, exit_code, duration_ms
        );

        // Log output if present
        if !stdout.is_empty() {
            info!("Shell command stdout:\n{}", stdout.trim());
        }
        if !stderr.is_empty() {
            if success {
                info!("Shell command stderr:\n{}", stderr.trim());
            } else {
                warn!("Shell command stderr:\n{}", stderr.trim());
            }
        }

        // Determine overall success based on fail_on_error setting
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;

        // Truncate stdout/stderr for display
        let stdout_display = if stdout.len() > 200 {
            format!("{}...", truncate_str(&stdout, 200))
        } else {
            stdout.clone()
        };
        let stderr_display = if stderr.len() > 200 {
            format!("{}...", truncate_str(&stderr, 200))
        } else {
            stderr.clone()
        };

        let (final_success, error_msg, output_data) = if success {
            // Return stdout in the screenshot_path field (repurposed for output data)
            let output_data = if stdout.is_empty() {
                None
            } else {
                Some(stdout.clone())
            };
            (true, None, output_data)
        } else if fail_on_error {
            let error_msg = if !stderr.is_empty() {
                format!(
                    "Command failed (exit code {:?}): {}",
                    exit_code,
                    stderr.trim()
                )
            } else {
                format!("Command failed with exit code {:?}", exit_code)
            };
            (false, Some(error_msg), None)
        } else {
            // Return success but include the error message
            info!(
                "Shell command '{}' failed but fail_on_error=false, continuing",
                step_name
            );
            let error_msg = if !stderr.is_empty() {
                format!("(ignored) Command failed: {}", stderr.trim())
            } else {
                format!("(ignored) Command failed with exit code {:?}", exit_code)
            };
            (true, Some(error_msg), Some(stdout.clone()))
        };

        // Build completed action node
        let completed_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("SHELL: {}", step_name),
            "timestamp": end_timestamp,
            "status": if final_success { "success" } else { "failed" },
            "duration": duration,
            "metadata": {
                "command": &command_display,
                "shell_type": shell_type,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "exit_code": exit_code,
                "stdout": &stdout_display,
                "stderr": &stderr_display,
                "duration_ms": duration_ms,
            }
        });

        let event_type = if final_success {
            "action_completed"
        } else {
            "action_failed"
        };

        // Emit completion tree event to file log
        FileLogger::log_tree_event(event_type, &completed_node, &[], end_timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp: end_timestamp,
                data: json!({ "node": completed_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

        // NOTE: Database event logging is handled by the unified workflow executor
        // (execute_steps_with_log_sources -> log_step_event) to avoid duplicates.
        // Tree events above are still emitted for the Session/Actions page.

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // Check Step Execution
    // =========================================================================

    /// Execute a code quality check step
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_check_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        use std::process::Stdio;
        use tokio::time::{timeout, Duration};

        // Debug logging to trace check_type values
        info!(
            "execute_check_step: step_name={:?}, check_type={:?}, check_command={:?}, working_dir={:?}",
            step.name, step.check_type, step.check_command, step.check_working_directory
        );

        let check_type = step.check_type.as_deref().unwrap_or("custom_command");
        let step_name = step.name.as_deref().unwrap_or("Check");
        // Note: Due to serde alias conflict, "working_directory" goes to shell_command_working_directory
        // So we check both fields for backwards compatibility.
        // Resolve relative paths to absolute so child processes get the correct CWD
        // regardless of the runner process's own working directory.
        let working_directory = step
            .check_working_directory
            .clone()
            .or_else(|| step.shell_command_working_directory.clone())
            .map(|wd| {
                let p = std::path::Path::new(&wd);
                if p.is_relative() {
                    match std::env::current_dir() {
                        Ok(cwd) => {
                            let resolved = cwd.join(p);
                            match resolved.canonicalize() {
                                Ok(abs) => abs.to_string_lossy().to_string(),
                                Err(_) => resolved.to_string_lossy().to_string(),
                            }
                        }
                        Err(_) => wd,
                    }
                } else {
                    wd
                }
            });

        // Handle http_status check type separately (doesn't need language detection)
        if check_type == "http_status" {
            return self
                .execute_http_status_check(step, step_name, timeout_secs)
                .await;
        }

        // Detect project type from working directory to auto-select appropriate tools
        let detected_language = {
            let work_dir = working_directory.as_deref().unwrap_or(".");
            let path = std::path::Path::new(work_dir);

            if path.join("Cargo.toml").exists() {
                "rust"
            } else if path.join("pyproject.toml").exists()
                || path.join("setup.py").exists()
                || path.join("requirements.txt").exists()
            {
                "python"
            } else if path.join("go.mod").exists() {
                "go"
            } else if path.join("tsconfig.json").exists() {
                "typescript"
            } else if path.join("package.json").exists() {
                "javascript"
            } else if path.join("CMakeLists.txt").exists()
                || path.join("Makefile").exists()
                || path.join("configure.ac").exists()
            {
                "c_cpp"
            } else if path.join("build.gradle").exists()
                || path.join("build.gradle.kts").exists()
                || path.join("pom.xml").exists()
            {
                "java"
            } else if path.join("mix.exs").exists() {
                "elixir"
            } else if path.join("Gemfile").exists() {
                "ruby"
            } else if path.join("composer.json").exists() {
                "php"
            } else if path.join("Package.swift").exists() {
                "swift"
            } else if path.join("*.csproj").exists() || path.join("*.sln").exists() {
                // Note: glob patterns don't work with exists(), but we'll check for common .NET files
                "dotnet"
            } else {
                "unknown"
            }
        };

        // Additional check for .NET projects (need to actually scan directory)
        let detected_language = if detected_language == "unknown" {
            let work_dir = working_directory.as_deref().unwrap_or(".");
            let path = std::path::Path::new(work_dir);
            if let Ok(entries) = std::fs::read_dir(path) {
                let has_dotnet = entries.filter_map(|e| e.ok()).any(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    name.ends_with(".csproj") || name.ends_with(".sln") || name.ends_with(".fsproj")
                });
                if has_dotnet {
                    "dotnet"
                } else {
                    "unknown"
                }
            } else {
                "unknown"
            }
        } else {
            detected_language
        };

        info!(
            "Check step '{}': detected language = {}",
            step_name, detected_language
        );

        // Get the command to run - auto-detect based on language if not specified
        // Note: Due to serde alias conflict, "command" in JSON goes to shell_command, not check_command
        // So we check both fields for backwards compatibility with frontend using "command" field
        let explicit_command = step
            .check_command
            .as_ref()
            .filter(|s| !s.is_empty())
            .or_else(|| step.shell_command.as_ref().filter(|s| !s.is_empty()));

        let command = match explicit_command {
            Some(cmd) => Some(cmd.clone()),
            None => {
                // Auto-select commands based on detected language and check type
                match (check_type, detected_language) {
                    // Python checks
                    ("lint", "python") => Some("ruff check .".to_string()),
                    ("format", "python") => Some("black --check .".to_string()),
                    ("typecheck", "python") => Some("mypy .".to_string()),
                    ("analyze", "python") => Some("ruff check . --statistics".to_string()),
                    ("security", "python") => Some("pip-audit".to_string()),

                    // Rust checks
                    ("lint", "rust") => Some("cargo clippy -- -D warnings".to_string()),
                    ("format", "rust") => Some("cargo fmt --check".to_string()),
                    ("typecheck", "rust") => Some("cargo check".to_string()),
                    ("analyze", "rust") => Some("cargo clippy --all-targets --all-features".to_string()),
                    ("security", "rust") => Some("cargo audit".to_string()),

                    // Go checks
                    ("lint", "go") => Some("golangci-lint run".to_string()),
                    ("format", "go") => Some("gofmt -l .".to_string()),
                    ("typecheck", "go") => Some("go vet ./...".to_string()),
                    ("analyze", "go") => Some("go vet ./... && staticcheck ./...".to_string()),
                    ("security", "go") => Some("gosec ./...".to_string()),

                    // TypeScript checks
                    ("lint", "typescript") => Some("npx eslint . --ext .ts,.tsx".to_string()),
                    ("format", "typescript") => Some("npx prettier --check .".to_string()),
                    ("typecheck", "typescript") => Some("npx tsc --noEmit".to_string()),
                    ("analyze", "typescript") => Some("npx eslint . --ext .ts,.tsx --format json".to_string()),
                    ("security", "typescript") => Some("npm audit".to_string()),

                    // JavaScript checks
                    ("lint", "javascript") => Some("npx eslint .".to_string()),
                    ("format", "javascript") => Some("npx prettier --check .".to_string()),
                    ("typecheck", "javascript") => None, // No typecheck for plain JS
                    ("analyze", "javascript") => Some("npx eslint . --format json".to_string()),
                    ("security", "javascript") => Some("npm audit".to_string()),

                    // C/C++ checks (using common tools)
                    ("lint", "c_cpp") => Some("cppcheck --enable=all .".to_string()),
                    ("format", "c_cpp") => Some("clang-format --dry-run -Werror **/*.cpp **/*.c **/*.h".to_string()),
                    ("typecheck", "c_cpp") => Some("make -n".to_string()), // Dry-run make
                    ("analyze", "c_cpp") => Some("cppcheck --enable=all --xml .".to_string()),
                    ("security", "c_cpp") => Some("flawfinder .".to_string()),

                    // Java checks
                    ("lint", "java") => Some("./gradlew checkstyleMain || mvn checkstyle:check".to_string()),
                    ("format", "java") => Some("./gradlew spotlessCheck || mvn spotless:check".to_string()),
                    ("typecheck", "java") => Some("./gradlew compileJava || mvn compile".to_string()),
                    ("analyze", "java") => Some("./gradlew pmd || mvn pmd:check".to_string()),
                    ("security", "java") => Some("./gradlew dependencyCheckAnalyze || mvn org.owasp:dependency-check-maven:check".to_string()),

                    // Ruby checks
                    ("lint", "ruby") => Some("bundle exec rubocop".to_string()),
                    ("format", "ruby") => Some("bundle exec rubocop --format offenses".to_string()),
                    ("typecheck", "ruby") => Some("bundle exec srb tc".to_string()), // Sorbet
                    ("analyze", "ruby") => Some("bundle exec rubocop --format json".to_string()),
                    ("security", "ruby") => Some("bundle exec bundler-audit check".to_string()),

                    // PHP checks
                    ("lint", "php") => Some("./vendor/bin/phpcs".to_string()),
                    ("format", "php") => Some("./vendor/bin/php-cs-fixer fix --dry-run --diff".to_string()),
                    ("typecheck", "php") => Some("./vendor/bin/phpstan analyse".to_string()),
                    ("analyze", "php") => Some("./vendor/bin/phpmd . text cleancode,codesize,controversial".to_string()),
                    ("security", "php") => Some("composer audit".to_string()),

                    // Elixir checks
                    ("lint", "elixir") => Some("mix credo".to_string()),
                    ("format", "elixir") => Some("mix format --check-formatted".to_string()),
                    ("typecheck", "elixir") => Some("mix dialyzer".to_string()),
                    ("analyze", "elixir") => Some("mix credo --format json".to_string()),
                    ("security", "elixir") => Some("mix deps.audit".to_string()),

                    // Swift checks
                    ("lint", "swift") => Some("swiftlint".to_string()),
                    ("format", "swift") => Some("swiftformat --lint .".to_string()),
                    ("typecheck", "swift") => Some("swift build".to_string()),
                    ("analyze", "swift") => Some("swiftlint --reporter json".to_string()),
                    ("security", "swift") => None, // No standard security tool

                    // .NET checks
                    ("lint", "dotnet") => Some("dotnet format --verify-no-changes".to_string()),
                    ("format", "dotnet") => Some("dotnet format --verify-no-changes".to_string()),
                    ("typecheck", "dotnet") => Some("dotnet build --no-restore".to_string()),
                    ("analyze", "dotnet") => Some("dotnet build /p:TreatWarningsAsErrors=true".to_string()),
                    ("security", "dotnet") => Some("dotnet list package --vulnerable".to_string()),

                    // Unknown language - skip gracefully
                    (check_type_val, "unknown") => {
                        warn!(
                            "Check step '{}': No language detected, skipping {} check. \
                            Specify a command explicitly or ensure project has recognizable marker files.",
                            step_name, check_type_val
                        );
                        None
                    }

                    // Catch-all for unrecognized check types on known languages
                    _ => {
                        warn!(
                            "Check step '{}': Unsupported check type '{}' for language '{}', skipping.",
                            step_name, check_type, detected_language
                        );
                        None
                    }
                }
            }
        };

        // Handle the case where no command could be determined (skip gracefully)
        let command = match command {
            Some(cmd) => cmd,
            None => {
                info!(
                    "Check step '{}' skipped: no applicable check for type '{}' and language '{}'",
                    step_name, check_type, detected_language
                );
                // Return success with a warning message
                return (
                    true,
                    Some(format!(
                        "Skipped: No {} check available for {} projects. Specify a command explicitly if needed.",
                        check_type, detected_language
                    )),
                    None,
                );
            }
        };
        let auto_fix = step.check_auto_fix.unwrap_or(false);

        // Modify command for auto-fix if enabled (language-aware)
        let final_command = if auto_fix {
            match (check_type, detected_language) {
                // Python auto-fix
                ("lint", "python") => command.replace("ruff check", "ruff check --fix"),
                ("format", "python") => command.replace("--check", ""),

                // Rust auto-fix
                ("lint", "rust") => command.replace("cargo clippy", "cargo clippy --fix"),
                ("format", "rust") => command.replace("--check", ""),

                // Go auto-fix
                ("lint", "go") => command.replace("golangci-lint run", "golangci-lint run --fix"),
                ("format", "go") => command.replace("gofmt -l", "gofmt -w"),

                // TypeScript/JavaScript auto-fix
                ("lint", "typescript") | ("lint", "javascript") => {
                    if command.contains("eslint") {
                        format!("{} --fix", command)
                    } else {
                        command.replace("lint", "lint:fix")
                    }
                }
                ("format", "typescript") | ("format", "javascript") => {
                    if command.contains("prettier") {
                        command.replace("--check", "--write")
                    } else {
                        command
                            .replace("format:check", "format")
                            .replace("--check", "")
                    }
                }

                // C/C++ auto-fix
                ("format", "c_cpp") => command.replace("--dry-run -Werror", "-i"),

                // Ruby auto-fix
                ("lint", "ruby") | ("format", "ruby") => format!("{} --autocorrect", command),

                // PHP auto-fix
                ("lint", "php") => command.replace("phpcs", "phpcbf"),
                ("format", "php") => command.replace("--dry-run --diff", ""),

                // Elixir auto-fix
                ("format", "elixir") => command.replace("--check-formatted", ""),

                // Swift auto-fix
                ("lint", "swift") => format!("{} --fix", command),
                ("format", "swift") => command.replace("--lint", ""),

                // .NET auto-fix
                ("lint", "dotnet") | ("format", "dotnet") => {
                    command.replace("--verify-no-changes", "")
                }

                // For languages without auto-fix, just return the command as-is
                _ => command,
            }
        } else {
            command
        };

        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Executing check '{}' ({}): {} (timeout: {}, working_dir: {:?})",
            step_name, check_type, final_command, timeout_str, working_directory
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static CHECK_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = CHECK_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("check-{}", sequence);

        // Truncate command for display
        let command_display = if final_command.len() > 50 {
            format!("{}...", truncate_str(&final_command, 50))
        } else {
            final_command.clone()
        };

        // Build action node for tree events
        let action_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("CHECK: {}", step_name),
            "timestamp": timestamp,
            "status": "pending",
            "metadata": {
                "check_type": check_type,
                "command": &command_display,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "auto_fix": auto_fix,
                "timeout_seconds": timeout_secs,
            }
        });

        // Emit action_started tree event to file log
        FileLogger::log_tree_event("action_started", &action_node, &[], timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "action_started".to_string(),
                timestamp,
                data: json!({ "node": action_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event("action_started", &action_node, timestamp, sequence);

        // Detect if command uses PowerShell syntax (same logic as shell_command_step)
        let is_powershell = final_command.contains("Get-")
            || final_command.contains("Set-")
            || final_command.contains("New-")
            || final_command.contains("Remove-")
            || final_command.contains("Invoke-")
            || final_command.contains("ForEach-Object")
            || final_command.contains("Where-Object")
            || final_command.contains("Select-Object")
            || final_command.contains("$_")
            || final_command.contains("$env:")
            || final_command.contains("-ErrorAction")
            || final_command.contains("| %")
            || final_command.contains("| ?");

        // Detect bash/Unix commands that cmd.exe cannot handle
        let is_bash = !is_powershell && Self::is_bash_command(&final_command);

        // Build the command
        let mut cmd = if cfg!(target_os = "windows") {
            if is_powershell {
                let mut c = crate::process_helpers::tokio_no_window("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command", &final_command]);
                c
            } else if is_bash {
                // Use Git Bash for Unix-style commands on Windows.
                let bash_path =
                    super::handlers::shell_command::ShellCommandHandler::find_git_bash()
                        .unwrap_or_else(|| "bash".to_string());
                let mut c = crate::process_helpers::tokio_no_window(&bash_path);
                // Ensure MSYS2 /usr/bin is on PATH for cat, grep, etc.
                if let Some(usr_bin) = std::path::Path::new(&bash_path).parent() {
                    let usr_bin_str = usr_bin.to_string_lossy();
                    let current_path = std::env::var("PATH").unwrap_or_default();
                    if !current_path.contains(&*usr_bin_str) {
                        c.env("PATH", format!("{};{}", usr_bin_str, current_path));
                    }
                }
                c.args(["-c", &final_command]);
                c
            } else {
                // cmd.exe doesn't understand single quotes — strip them.
                // Also extract Unix-style KEY=VALUE env var prefixes since
                // cmd.exe doesn't support "KEY=VALUE command" syntax.
                let stripped = final_command.replace('\'', "");
                let (extra_envs, actual_cmd) = extract_env_prefix_for_cmd(&stripped);
                let mut c = crate::process_helpers::tokio_cmd_no_window();
                c.args(["/C", &actual_cmd]);
                for (key, value) in extra_envs {
                    c.env(key, value);
                }
                c
            }
        } else {
            let mut c = crate::process_helpers::tokio_no_window("sh");
            c.args(["-c", &final_command]);
            c
        };

        // Set working directory if specified
        if let Some(ref wd) = working_directory {
            cmd.current_dir(wd);
        }

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Execute with optional timeout
        let start = std::time::Instant::now();

        // Helper to process command output
        let process_output = |output: std::process::Output,
                              duration_ms: u64|
         -> (bool, Option<String>, Option<String>) {
            let exit_code = output.status.code();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let success = output.status.success();

            info!(
                "Check '{}' completed: success={}, exit_code={:?}, duration={}ms",
                step_name, success, exit_code, duration_ms
            );

            if success {
                let output_data = if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                };
                (true, None, output_data)
            } else {
                // IMPORTANT: Capture BOTH stdout and stderr for failed checks
                // so the AI can see the full error context for fixing
                let mut combined_output = String::new();
                if !stdout.is_empty() {
                    combined_output.push_str("=== STDOUT ===\n");
                    combined_output.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !combined_output.is_empty() {
                        combined_output.push_str("\n\n");
                    }
                    combined_output.push_str("=== STDERR ===\n");
                    combined_output.push_str(&stderr);
                }
                let error_summary = if !stderr.is_empty() {
                    stderr.lines().take(5).collect::<Vec<_>>().join("\n")
                } else {
                    stdout.lines().take(5).collect::<Vec<_>>().join("\n")
                };
                (
                    false,
                    Some(format!(
                        "Check failed (exit code {:?}): {}",
                        exit_code,
                        error_summary.trim()
                    )),
                    Some(combined_output), // Return full output for AI context
                )
            }
        };

        // Process the result - execute with or without timeout depending on setting
        let (final_success, error_msg, output_data) = if let Some(timeout_secs_val) = timeout_secs {
            // Execute with timeout
            let timeout_duration = Duration::from_secs(timeout_secs_val);
            let output_result = timeout(timeout_duration, cmd.output()).await;
            let duration_ms = start.elapsed().as_millis() as u64;

            match output_result {
                Ok(Ok(output)) => process_output(output, duration_ms),
                Ok(Err(e)) => {
                    warn!("Failed to execute check '{}': {}", step_name, e);
                    (false, Some(format!("Failed to execute check: {}", e)), None)
                }
                Err(_) => {
                    warn!(
                        "Check '{}' timed out after {}s",
                        step_name, timeout_secs_val
                    );
                    (
                        false,
                        Some(format!(
                            "Check timed out after {} seconds",
                            timeout_secs_val
                        )),
                        None,
                    )
                }
            }
        } else {
            // No timeout - execute without timeout wrapper
            let duration_ms = start.elapsed().as_millis() as u64;
            match cmd.output().await {
                Ok(output) => process_output(output, duration_ms),
                Err(e) => {
                    warn!("Failed to execute check '{}': {}", step_name, e);
                    (false, Some(format!("Failed to execute check: {}", e)), None)
                }
            }
        };

        // Emit completion event
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;
        let total_duration_ms = start.elapsed().as_millis() as u64;

        let completed_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("CHECK: {}", step_name),
            "timestamp": end_timestamp,
            "status": if final_success { "success" } else { "failed" },
            "duration": duration,
            "metadata": {
                "check_type": check_type,
                "command": &command_display,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "duration_ms": total_duration_ms,
                "error": error_msg.as_deref().unwrap_or(""),
            }
        });

        let event_type = if final_success {
            "action_completed"
        } else {
            "action_failed"
        };

        // Emit completion tree event to file log
        FileLogger::log_tree_event(event_type, &completed_node, &[], end_timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp: end_timestamp,
                data: json!({ "node": completed_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // HTTP Status Check Execution
    // =========================================================================

    /// Execute an HTTP status check
    ///
    /// Makes an HTTP GET request to the specified URL and verifies the status code
    /// matches the expected value. Useful for health checks before running tests.
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_http_status_check(
        &self,
        step: &ExecutionStepConfig,
        step_name: &str,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        use std::time::Duration;

        // Get the URL to check
        let url = match &step.check_url {
            Some(u) => u.clone(),
            None => {
                return (
                    false,
                    Some("check_url is required for http_status check".to_string()),
                    None,
                );
            }
        };

        let expected_status = step.expected_status.unwrap_or(200);
        // Cap at 5 minutes if specified, otherwise use a large default for the HTTP client
        let timeout = timeout_secs
            .map(|t| Duration::from_secs(t.min(300)))
            .unwrap_or(Duration::from_secs(300)); // 5 min default for HTTP checks
        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());

        info!(
            "Executing HTTP status check '{}': url={}, expected_status={}, timeout={}",
            step_name, url, expected_status, timeout_str
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static HTTP_CHECK_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = HTTP_CHECK_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("http-check-{}", sequence);

        // Build action node for tree events
        let action_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("HTTP CHECK: {}", step_name),
            "timestamp": timestamp,
            "status": "pending",
            "metadata": {
                "check_type": "http_status",
                "url": &url,
                "expected_status": expected_status,
                "timeout_seconds": timeout.as_secs(),
            }
        });

        // Emit action_started tree event to file log
        FileLogger::log_tree_event("action_started", &action_node, &[], timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "action_started".to_string(),
                timestamp,
                data: json!({ "node": action_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event("action_started", &action_node, timestamp, sequence);

        // Make the HTTP request
        let start = std::time::Instant::now();
        let client = match reqwest::Client::builder().timeout(timeout).build() {
            Ok(c) => c,
            Err(e) => {
                let error_msg = format!("Failed to create HTTP client: {}", e);
                warn!("{}", error_msg);
                return (false, Some(error_msg), None);
            }
        };

        let result = client.get(&url).send().await;
        let duration_ms = start.elapsed().as_millis() as u64;

        // Process the result
        let (final_success, error_msg, output_data) = match result {
            Ok(response) => {
                let actual_status = response.status().as_u16();
                info!(
                    "HTTP check '{}' completed: actual_status={}, expected={}, duration={}ms",
                    step_name, actual_status, expected_status, duration_ms
                );

                if actual_status == expected_status {
                    (
                        true,
                        None,
                        Some(
                            json!({
                                "status": actual_status,
                                "url": url,
                                "duration_ms": duration_ms
                            })
                            .to_string(),
                        ),
                    )
                } else {
                    (
                        false,
                        Some(format!(
                            "Expected status {} but got {} from {}",
                            expected_status, actual_status, url
                        )),
                        Some(
                            json!({
                                "status": actual_status,
                                "expected": expected_status,
                                "url": url,
                                "duration_ms": duration_ms
                            })
                            .to_string(),
                        ),
                    )
                }
            }
            Err(e) => {
                // Categorize error for better AI understanding
                let error_msg = if e.is_connect() {
                    format!(
                        "Server not running at {} - Connection refused. Make sure the service is started.",
                        url
                    )
                } else if e.is_timeout() {
                    format!(
                        "Server at {} not responding - Request timed out after {}s. The service may be overloaded or not running.",
                        url, timeout.as_secs()
                    )
                } else if e.is_request() {
                    format!("Invalid request to {}: {}", url, e)
                } else {
                    format!("Failed to reach {}: {}", url, e)
                };

                warn!("HTTP check '{}' failed: {}", step_name, error_msg);
                (
                    false,
                    Some(error_msg.clone()),
                    Some(
                        json!({
                            "error": error_msg,
                            "url": url,
                            "duration_ms": duration_ms
                        })
                        .to_string(),
                    ),
                )
            }
        };

        // Emit completion event
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;

        let completed_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("HTTP CHECK: {}", step_name),
            "timestamp": end_timestamp,
            "status": if final_success { "success" } else { "failed" },
            "duration": duration,
            "metadata": {
                "check_type": "http_status",
                "url": &url,
                "expected_status": expected_status,
                "duration_ms": duration_ms,
                "error": error_msg.as_deref().unwrap_or(""),
            }
        });

        let event_type = if final_success {
            "action_completed"
        } else {
            "action_failed"
        };

        // Emit completion tree event to file log
        FileLogger::log_tree_event(event_type, &completed_node, &[], end_timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp: end_timestamp,
                data: json!({ "node": completed_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // Check Group Step Execution
    // =========================================================================

    /// Execute all checks in a check group
    /// Returns: (success, error_message, summary_text, individual_check_results)
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_check_group_step(
        &self,
        step: &ExecutionStepConfig,
        _timeout_secs: Option<u64>,
    ) -> (
        bool,
        Option<String>,
        Option<String>,
        Option<Vec<IndividualCheckResult>>,
    ) {
        let step_name = step.name.as_deref().unwrap_or("Check Group");
        let group_id = match &step.check_group_id {
            Some(id) => id.clone(),
            None => {
                return (
                    false,
                    Some("No check group ID specified for check_group step".to_string()),
                    None,
                    None,
                );
            }
        };

        info!(
            "execute_check_group_step: step_name={:?}, group_id={:?}",
            step_name, group_id
        );

        // Get the checkpoint_db from app_state
        let db = &self.app_state.checkpoint_db;

        // Get the group
        let group = match db.get_check_group(&group_id) {
            Ok(Some(g)) => g,
            Ok(None) => {
                return (
                    false,
                    Some(format!("Check group not found: {}", group_id)),
                    None,
                    None,
                );
            }
            Err(e) => {
                return (
                    false,
                    Some(format!("Failed to get check group: {}", e)),
                    None,
                    None,
                );
            }
        };

        if !group.enabled {
            info!("Check group '{}' is disabled, skipping", group.name);
            return (
                true,
                None,
                Some(format!("Check group '{}' is disabled", group.name)),
                None,
            );
        }

        // Get checks in the group
        let checks = match db.get_checks_in_group(&group_id) {
            Ok(c) => c,
            Err(e) => {
                return (
                    false,
                    Some(format!("Failed to get checks in group: {}", e)),
                    None,
                    None,
                );
            }
        };

        if checks.is_empty() {
            return (
                true,
                None,
                Some(format!("No checks in group '{}'", group.name)),
                None,
            );
        }

        info!(
            "Executing check group '{}' with {} checks (stop_on_failure: {})",
            group.name,
            checks.len(),
            group.stop_on_failure
        );

        // Execute each check
        use crate::check_executor::{execute_check, CheckDefinition, CheckTool, CheckType};
        use std::time::Instant;

        let start_time = Instant::now();
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        let mut results_output = Vec::new();
        let mut check_results: Vec<IndividualCheckResult> = Vec::new();

        for check in &checks {
            if !check.enabled {
                results_output.push(format!("  [SKIPPED] {} (disabled)", check.name));
                check_results.push(IndividualCheckResult {
                    name: check.name.clone(),
                    status: "skipped".to_string(),
                    duration_ms: 0,
                    issues_found: 0,
                    issues_fixed: 0,
                    files_checked: 0,
                    error_message: Some("Check is disabled".to_string()),
                    output: None,
                    issues: Vec::new(),
                });
                skipped += 1;
                continue;
            }

            let check_def = CheckDefinition {
                id: check.id.clone(),
                name: check.name.clone(),
                check_type: serde_json::from_str(&format!("\"{}\"", check.check_type))
                    .unwrap_or(CheckType::Lint),
                tool: serde_json::from_str(&format!("\"{}\"", check.tool))
                    .unwrap_or(CheckTool::Custom),
                command: check.command.clone(),
                working_directory: check.working_directory.clone(),
                config_path: check.config_path.clone(),
                auto_fix: check.auto_fix,
                fail_on_warning: check.fail_on_warning,
                timeout_seconds: check.timeout_seconds,
                is_critical: check.is_critical,
            };

            let result = execute_check(&check_def);
            let is_success = result.is_success();

            // Extract issues from structured output (limit to 50 to avoid huge payloads)
            let issues: Vec<CheckIssueDetail> = result
                .structured_output
                .as_ref()
                .map(|so| {
                    so.issues
                        .iter()
                        .take(50) // Limit to 50 issues per check
                        .map(|issue| CheckIssueDetail {
                            file: issue.file.clone(),
                            line: issue.line,
                            column: issue.column,
                            code: issue.code.clone(),
                            message: issue.message.clone(),
                            severity: format!("{:?}", issue.severity).to_lowercase(),
                            fixable: issue.fixable,
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Build individual check result
            let check_result = IndividualCheckResult {
                name: check.name.clone(),
                status: if is_success { "passed" } else { "failed" }.to_string(),
                duration_ms: result.duration_ms,
                issues_found: result.issues_found,
                issues_fixed: result.issues_fixed,
                files_checked: result.files_checked,
                error_message: result.error.clone(),
                output: if result.output.len() > 2000 {
                    Some(format!("{}... (truncated)", &result.output[..2000]))
                } else if !result.output.is_empty() {
                    Some(result.output.clone())
                } else {
                    None
                },
                issues,
            };
            check_results.push(check_result);

            if is_success {
                passed += 1;
                results_output.push(format!(
                    "  [PASSED] {} ({}ms, {} issues found, {} fixed)",
                    check.name, result.duration_ms, result.issues_found, result.issues_fixed
                ));
            } else {
                failed += 1;
                results_output.push(format!(
                    "  [FAILED] {} ({}ms): {}",
                    check.name,
                    result.duration_ms,
                    result.error.as_deref().unwrap_or(&result.output)
                ));

                if group.stop_on_failure {
                    results_output.push("  Stopping due to stop_on_failure setting".to_string());
                    break;
                }
            }
        }

        let duration_ms = start_time.elapsed().as_millis();
        let total = passed + failed;
        let success = failed == 0;

        let summary = format!(
            "Check group '{}': {}/{} passed ({}ms total)\n{}",
            group.name,
            passed,
            total,
            duration_ms,
            results_output.join("\n")
        );

        info!(
            "Check group '{}' completed: {}/{} passed, {} skipped ({}ms)",
            group.name, passed, total, skipped, duration_ms
        );

        if success {
            (true, None, Some(summary), Some(check_results))
        } else {
            (
                false,
                Some(format!(
                    "Check group '{}' failed: {}/{} passed",
                    group.name, passed, total
                )),
                Some(summary),
                Some(check_results),
            )
        }
    }
}

// ============================================================================
// Log Watch Helper Functions (outside impl block for reusability)
// ============================================================================

/// Collect recent errors from log files
///
/// Reads the tail of each log file, parses timestamps, and extracts
/// error lines within the specified time window.
pub(crate) async fn collect_recent_log_errors(
    log_sources: &[String],
    time_window_seconds: u64,
    custom_patterns: Option<&[String]>,
) -> Vec<LogError> {
    use chrono::Utc;
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let dev_logs_dir = crate::paths::get_dev_logs_dir();
    let cutoff_time = Utc::now() - chrono::Duration::seconds(time_window_seconds as i64);
    let mut all_errors = Vec::new();

    // Build pattern list: defaults + custom
    let mut patterns: Vec<&str> = DEFAULT_ERROR_PATTERNS.to_vec();
    if let Some(custom) = custom_patterns {
        for p in custom {
            patterns.push(p.as_str());
        }
    }

    for source_name in log_sources {
        // If the source name is an absolute path, use it directly;
        // otherwise join with dev_logs_dir (backward compat for workflow configs)
        let source_path = std::path::Path::new(source_name);
        let log_path = if source_path.is_absolute() {
            source_path.to_path_buf()
        } else {
            dev_logs_dir.join(source_name)
        };

        if !log_path.exists() {
            // Log file doesn't exist - this is OK, just skip it
            info!("Log file not found, skipping: {:?}", log_path);
            continue;
        }

        // Read the file
        let file = match File::open(&log_path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Failed to open log file {:?}: {}", log_path, e);
                continue;
            }
        };

        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().map_while(|l| l.ok()).collect();
        let total_lines = lines.len();

        // Process lines, looking for errors
        for (line_idx, line) in lines.iter().enumerate() {
            // Check if this line matches any error pattern
            let error_type = find_error_type(line, &patterns);
            if error_type.is_none() {
                continue;
            }
            let error_type = error_type.unwrap();

            // Try to parse timestamp from the line
            let timestamp = extract_timestamp(line);

            // If we have a timestamp, check if it's within the time window
            // If no timestamp can be parsed, skip the error to avoid including stale entries
            match &timestamp {
                Some(ts) => {
                    if let Some(parsed) = parse_log_timestamp(ts) {
                        if parsed < cutoff_time {
                            // This error is older than our time window, skip it
                            continue;
                        }
                    } else {
                        // Timestamp found but couldn't be parsed - skip to avoid stale errors
                        continue;
                    }
                }
                None => {
                    // No timestamp in the line - skip to avoid including errors of unknown age
                    // This prevents old errors from files that weren't cleared from being included
                    continue;
                }
            }

            // Collect context lines
            let context_before: Vec<String> =
                lines[line_idx.saturating_sub(CONTEXT_LINES)..line_idx].to_vec();

            let context_after: Vec<String> = lines
                [(line_idx + 1).min(total_lines)..(line_idx + 1 + CONTEXT_LINES).min(total_lines)]
                .to_vec();

            all_errors.push(LogError {
                source: source_name.clone(),
                line_number: line_idx + 1, // 1-indexed
                timestamp,
                message: line.clone(),
                context_before,
                context_after,
                error_type,
            });
        }
    }

    // Limit to avoid overwhelming output (keep most recent 50 errors)
    if all_errors.len() > 50 {
        all_errors = all_errors.into_iter().rev().take(50).rev().collect();
    }

    all_errors
}

/// Find what type of error a line represents, if any
pub(crate) fn find_error_type(line: &str, patterns: &[&str]) -> Option<String> {
    let line_lower = line.to_lowercase();

    // Check each pattern
    for pattern in patterns {
        let pattern_lower = pattern.to_lowercase();
        if line_lower.contains(&pattern_lower) {
            // Categorize the error type
            if pattern_lower.contains("traceback") {
                return Some("traceback".to_string());
            } else if pattern_lower.contains("exception") {
                return Some("exception".to_string());
            } else if pattern_lower.contains("panic") {
                return Some("panic".to_string());
            } else if pattern_lower.contains("fatal") {
                return Some("fatal".to_string());
            } else if pattern_lower.contains("error") {
                return Some("error".to_string());
            } else if pattern_lower.contains("failed") {
                return Some("failed".to_string());
            } else {
                return Some("error".to_string());
            }
        }
    }

    None
}

/// Extract timestamp from a log line (handles multiple formats)
pub(crate) fn extract_timestamp(line: &str) -> Option<String> {
    use once_cell::sync::Lazy;
    use regex::Regex;

    // Common timestamp patterns
    // Pattern 1: 2026-01-26 10:30:45 or 2026-01-26T10:30:45
    // Pattern 2: [2026-01-26T10:30:45Z] or [2026-01-26 10:30:45]
    // Pattern 3: ISO 8601 with milliseconds

    // Try to match ISO 8601 format
    static ISO_TIMESTAMP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)")
            .unwrap()
    });
    static BRACKETED_TIMESTAMP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"\[(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)\]")
            .unwrap()
    });

    // Try bracketed format first
    if let Some(caps) = BRACKETED_TIMESTAMP.captures(line) {
        return caps.get(1).map(|m| m.as_str().to_string());
    }

    // Try ISO format
    if let Some(caps) = ISO_TIMESTAMP.captures(line) {
        return caps.get(1).map(|m| m.as_str().to_string());
    }

    None
}

/// Parse a timestamp string into a DateTime
pub(crate) fn parse_log_timestamp(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

    // Try RFC3339 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try common formats
    let formats = [
        "%Y-%m-%dT%H:%M:%S%.fZ",
        "%Y-%m-%dT%H:%M:%SZ",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ];

    for fmt in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(ts, fmt) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }

    None
}

/// Format log errors into a markdown report for AI consumption
pub(crate) fn format_log_errors_for_ai(errors: &[LogError]) -> String {
    let mut report = String::new();

    report.push_str("## Log Errors Detected\n\n");
    report.push_str(&format!("**Total errors found:** {}\n\n", errors.len()));

    // Group errors by source
    let mut by_source: std::collections::HashMap<String, Vec<&LogError>> =
        std::collections::HashMap::new();
    for error in errors {
        by_source
            .entry(error.source.clone())
            .or_default()
            .push(error);
    }

    for (source, source_errors) in by_source {
        report.push_str(&format!(
            "### {} ({} errors)\n\n",
            source,
            source_errors.len()
        ));

        for error in source_errors {
            report.push_str(&format!(
                "#### Line {} ({})\n",
                error.line_number, error.error_type
            ));

            if let Some(ref ts) = error.timestamp {
                report.push_str(&format!("**Timestamp:** {}\n", ts));
            }

            report.push_str("\n**Context:**\n```\n");

            // Context before
            for line in &error.context_before {
                report.push_str(&format!("  {}\n", line));
            }

            // Error line (highlighted)
            report.push_str(&format!("> {}\n", error.message));

            // Context after
            for line in &error.context_after {
                report.push_str(&format!("  {}\n", line));
            }

            report.push_str("```\n\n");
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_result_empty_summary() {
        let result = ExecutionResult {
            success: true,
            total_steps: 0,
            successful_steps: 0,
            failed_steps: 0,
            total_duration_ms: 0,
            steps: vec![],
            captured_logs: None,
            captured_runner_logs: None,
            verification_passed: None,
            loop_result: None,
            task_summary: None,
        };
        assert_eq!(result.to_markdown_summary(), "");
    }

    #[test]
    fn test_execution_result_summary() {
        let result = ExecutionResult {
            success: true,
            total_steps: 2,
            successful_steps: 2,
            failed_steps: 0,
            total_duration_ms: 1500,
            steps: vec![
                StepExecutionResult {
                    step_index: 0,
                    step_type: "workflow".to_string(),
                    step_name: "Login".to_string(),
                    step_id: None,
                    success: true,
                    error: None,
                    screenshot_path: Some("screenshot1.png".to_string()),
                    started_at: None,
                    ended_at: None,
                    duration_ms: 1000,
                    config: StepExecutionConfig::default(),
                    verification_details: None,
                    output_data: None,
                    required: None,
                    resolved_inputs: None,
                    extracted_values: None,
                    failure_category: None,
                    interrupted: None,
                },
                StepExecutionResult {
                    step_index: 1,
                    step_type: "screenshot".to_string(),
                    step_name: "Capture".to_string(),
                    step_id: None,
                    success: true,
                    error: None,
                    screenshot_path: Some("screenshot2.png".to_string()),
                    started_at: None,
                    ended_at: None,
                    duration_ms: 500,
                    config: StepExecutionConfig::default(),
                    verification_details: None,
                    output_data: None,
                    required: None,
                    resolved_inputs: None,
                    extracted_values: None,
                    failure_category: None,
                    interrupted: None,
                },
            ],
            captured_logs: None,
            captured_runner_logs: None,
            verification_passed: None,
            loop_result: None,
            task_summary: None,
        };
        let summary = result.to_markdown_summary();
        assert!(summary.contains("Pre-Execution Results"));
        assert!(summary.contains("Login"));
        assert!(summary.contains("2 of 2 steps completed successfully"));
    }

    // ========================================================================
    // compute_execution_layers tests
    // ========================================================================

    fn make_step(id: &str, step_type: &str) -> ExecutionStepConfig {
        ExecutionStepConfig {
            id: Some(id.to_string()),
            step_type: step_type.to_string(),
            name: Some(id.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_dag_no_dependencies() {
        // Three independent steps should form one layer
        let steps = vec![
            make_step("a", "check"),
            make_step("b", "check"),
            make_step("c", "check"),
        ];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].len(), 3);
    }

    #[test]
    fn test_dag_linear_chain() {
        // a -> b -> c (each depends on the previous)
        let a = make_step("a", "shell_command");
        let mut b = make_step("b", "shell_command");
        b.depends_on = Some(vec!["a".to_string()]);
        let mut c = make_step("c", "shell_command");
        c.depends_on = Some(vec!["b".to_string()]);

        let steps = vec![a, b, c];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec![0]); // a
        assert_eq!(layers[1], vec![1]); // b
        assert_eq!(layers[2], vec![2]); // c
    }

    #[test]
    fn test_dag_diamond() {
        // a -> b, a -> c, b -> d, c -> d
        let a = make_step("a", "api_request");
        let mut b = make_step("b", "check");
        b.depends_on = Some(vec!["a".to_string()]);
        let mut c = make_step("c", "check");
        c.depends_on = Some(vec!["a".to_string()]);
        let mut d = make_step("d", "prompt");
        d.depends_on = Some(vec!["b".to_string(), "c".to_string()]);

        let steps = vec![a, b, c, d];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec![0]); // a
        assert!(layers[1].contains(&1)); // b and c in parallel
        assert!(layers[1].contains(&2));
        assert_eq!(layers[2], vec![3]); // d
    }

    #[test]
    fn test_dag_input_dependencies() {
        // b reads from a's output via inputs
        let a = make_step("a", "api_request");
        let mut b = make_step("b", "check");
        let mut inputs = std::collections::HashMap::new();
        inputs.insert("response".to_string(), "a.output.body".to_string());
        b.inputs = Some(inputs);

        let steps = vec![a, b];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0], vec![0]); // a first
        assert_eq!(layers[1], vec![1]); // then b
    }

    #[test]
    fn test_dag_cycle_detection() {
        // a -> b -> a (cycle)
        let mut a = make_step("a", "check");
        a.depends_on = Some(vec!["b".to_string()]);
        let mut b = make_step("b", "check");
        b.depends_on = Some(vec!["a".to_string()]);

        let steps = vec![a, b];
        let result = compute_execution_layers(&steps);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Circular"));
    }

    #[test]
    fn test_dag_empty_steps() {
        let steps: Vec<ExecutionStepConfig> = vec![];
        let layers = compute_execution_layers(&steps).unwrap();
        assert!(layers.is_empty());
    }

    #[test]
    fn test_dag_single_step() {
        let steps = vec![make_step("a", "shell_command")];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0], vec![0]);
    }

    #[test]
    fn test_dag_unknown_dependency_ignored() {
        // Step references a non-existent dependency - should be ignored
        let mut a = make_step("a", "check");
        a.depends_on = Some(vec!["nonexistent".to_string()]);

        let steps = vec![a];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0], vec![0]);
    }
}
