use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::iteration_bundle::{ActionEvent, ImageRecognitionEvent};

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

/// Configuration for a single execution step.
///
/// Supports 3 core step types: command, ui_bridge, prompt.
/// ("test" is dispatched through command handler when test_id/test_type fields are set)
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ExecutionStepConfig {
    /// Step type: "command", "ui_bridge", "prompt" (legacy "test" maps to "command").
    /// The AI workflow generator (structured_output.rs) emits `"step_type"` while
    /// the execution system expects `"type"`. The alias ensures both deserialize.
    #[serde(rename = "type", alias = "step_type")]
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

    /// Pause execution AFTER this step completes, capture a snapshot, and wait for resume.
    /// When true, the executor captures workflow state and suspends until an explicit
    /// resume signal is received via the API.
    #[serde(alias = "breakpoint", default)]
    pub breakpoint: Option<bool>,

    // ========================================================================
    // Code Execution Step Fields
    // ========================================================================
    /// Inline Python code to execute in the sandbox
    #[serde(alias = "code", default)]
    pub code: Option<String>,

    /// Path to a Python file to execute in the sandbox
    #[serde(alias = "code_file", alias = "codeFile", default)]
    pub code_file: Option<String>,

    /// Sandbox mode: "enforce" (block on violation) or "warn" (log and continue)
    #[serde(alias = "sandbox_mode", alias = "sandboxMode", default)]
    pub sandbox_mode: Option<String>,

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
    /// Prompt content (for prompt steps - not executed, passed to AI).
    ///
    /// Canonical wire name is `content` (matches `PromptStep::content` in
    /// qontinui-types). Legacy aliases `promptContent` / `prompt_content`
    /// are accepted on deserialize for older stored workflows and the
    /// current SchedulerTaskForm request shape.
    #[serde(rename = "content", alias = "promptContent", alias = "prompt_content")]
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
    // NOTE on canonical-name renames: `ExecutionStepConfig` is a fat struct
    // shared by all step types, so bare canonical names like `"action"` and
    // `"target"` are ambiguous — `native_accessibility` and `ui_bridge` both
    // use them.  Keeping prefixed primary names here preserves correct routing
    // when ExecutionStepConfig is deserialized from mixed-type JSON.  The
    // typed-dispatch boundary in `executor.rs::to_full_runner_step` uses
    // per-variant Rust-field constructors (Session 2c) to produce the typed
    // `FullRunnerStep` view without round-tripping through JSON, so these
    // prefixed names do not block the typed invariant.
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

    /// UI Bridge: Structured action plan (JSON array of typed actions).
    /// Used with ui_bridge_action = "action_plan" to execute a sequence of
    /// pre-planned UI actions without a second LLM interpretation call.
    #[serde(alias = "uiBridgeActionPlan", alias = "ui_bridge_action_plan")]
    pub ui_bridge_action_plan: Option<serde_json::Value>,

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
    // Native Accessibility Step Fields
    // ========================================================================
    /// Accessibility action: "capture", "click", "type", "focus", "query", "ai_context"
    #[serde(alias = "a11yAction", alias = "action")]
    pub a11y_action: Option<String>,

    /// Connection target: "Desktop", window title, or "pid:1234"
    #[serde(alias = "a11yTarget", alias = "target")]
    pub a11y_target: Option<String>,

    /// Element ref ID for click/type/focus (e.g. "@e3")
    #[serde(alias = "a11yRefId", alias = "ref_id")]
    pub a11y_ref_id: Option<String>,

    /// Text to type (for "type" action)
    #[serde(alias = "a11yText", alias = "text")]
    pub a11y_text: Option<String>,

    /// Whether to clear existing text before typing (default: false)
    #[serde(alias = "a11yClearFirst", alias = "clear_first", default)]
    pub a11y_clear_first: Option<bool>,

    /// Role filter for "query" action (e.g. "button", "textbox")
    #[serde(alias = "a11yQueryRole", alias = "query_role")]
    pub a11y_query_role: Option<String>,

    /// Label filter for "query" action
    #[serde(alias = "a11yQueryLabel", alias = "query_label")]
    pub a11y_query_label: Option<String>,

    /// Only include interactive elements (for "query" and "ai_context")
    #[serde(alias = "a11yInteractiveOnly", alias = "interactive_only", default)]
    pub a11y_interactive_only: Option<bool>,

    /// Maximum elements for "ai_context" action (default: 50)
    #[serde(alias = "a11yMaxElements", alias = "max_elements", default)]
    pub a11y_max_elements: Option<u32>,

    /// Include hidden elements in "capture" (default: false)
    #[serde(alias = "a11yIncludeHidden", alias = "include_hidden", default)]
    pub a11y_include_hidden: Option<bool>,

    /// Maximum tree depth for "capture" (None = unlimited)
    #[serde(alias = "a11yMaxDepth", alias = "max_depth", default)]
    pub a11y_max_depth: Option<u32>,

    // ========================================================================
    // Visual Assertion
    // ========================================================================
    /// Visual assertion type: "text", "screenshot", or "highlight".
    #[serde(
        alias = "visualAssertionType",
        alias = "visual_assertion_type",
        default
    )]
    pub visual_assertion_type: Option<String>,

    /// Element query (JSON) for text assertions.
    #[serde(
        alias = "visualAssertionQuery",
        alias = "visual_assertion_query",
        default
    )]
    pub visual_assertion_query: Option<serde_json::Value>,

    /// Expected text (for text assertion) or element ID (for screenshot/highlight).
    #[serde(
        alias = "visualAssertionExpected",
        alias = "visual_assertion_expected",
        default
    )]
    pub visual_assertion_expected: Option<String>,

    /// Options JSON for the assertion (TextAssertionOptions or ScreenshotAssertionOptions).
    #[serde(
        alias = "visualAssertionOptions",
        alias = "visual_assertion_options",
        default
    )]
    pub visual_assertion_options: Option<serde_json::Value>,

    // ========================================================================
    // VGA (Visual GUI Automation) Step Fields
    // ========================================================================
    /// VGA: UUID referencing `runner.vga_state_machines.id` — the persisted
    /// state machine defining the elements the step may click/type/wait for.
    ///
    /// VGA state machine id. See `qontinui-schemas::workflow_step`
    /// for the canonical spelling (`vgaStateMachineId`) — the aliases
    /// here exist only for historical JSON compatibility.
    #[serde(alias = "stateMachineId", alias = "state_machine_id", default)]
    pub vga_state_machine_id: Option<String>,

    /// VGA: Target process / window name (e.g. "notepad++.exe") — used by the
    /// HAL to focus the correct top-level window before each action.
    #[serde(alias = "targetProcess", alias = "target_process", default)]
    pub vga_target_process: Option<String>,

    /// VGA: Ordered sequence of VGA actions (internally-tagged by "kind").
    /// Passed through verbatim to the Python worker.
    #[serde(alias = "actionSequence", alias = "action_sequence", default)]
    pub vga_action_sequence: Option<serde_json::Value>,

    /// VGA: Overall step timeout in milliseconds. Defaults to 300000 (5 min).
    #[serde(
        alias = "vgaTimeoutMs",
        alias = "vga_timeout_ms",
        alias = "timeoutMs",
        alias = "timeout_ms",
        default
    )]
    pub vga_timeout_ms: Option<u64>,

    /// VGA: Reserved for future async mode. Currently the handler rejects
    /// `true` until async mode is implemented.
    #[serde(alias = "vgaAsync", alias = "vga_async", alias = "async", default)]
    pub vga_async: Option<bool>,

    // ========================================================================
    // Wrapper Action Step Fields (Phase 3)
    // ========================================================================
    /// Wrapper Action: ID of the installed wrapper to dispatch through.
    #[serde(alias = "wrapperId", alias = "wrapper_id", default)]
    pub wrapper_id: Option<String>,

    /// Wrapper Action: ID of the action exposed by the wrapper.
    #[serde(alias = "actionId", alias = "action_id", default)]
    pub wrapper_action_id: Option<String>,

    /// Wrapper Action: JSON object of params to pass to the action.
    /// Values may contain `{{ variable }}` template references that are
    /// resolved against the workflow's runtime context before dispatch.
    #[serde(alias = "wrapperParams", alias = "wrapper_params", default)]
    pub wrapper_params: Option<serde_json::Value>,

    /// Wrapper Action: Optional name of a workflow variable to write the
    /// dispatch result to. Empty / `None` means "don't store the result".
    #[serde(alias = "resultVariable", alias = "result_variable", default)]
    pub wrapper_result_variable: Option<String>,

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
