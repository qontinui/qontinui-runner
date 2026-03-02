//! Unified AI Session Executor Module
//!
//! Consolidates the duplicated AI session handling logic from phases.rs into a single,
//! reusable executor. This module handles:
//!
//! - Context building with AiSessionContext builder chain
//! - FindingContext creation with phase-specific session_num
//! - Prompt transformation (autonomous context, marker stripping, finding instructions)
//! - Workspace root retrieval
//! - Claude session execution with retry support
//! - Duration tracking and event logging
//!
//! ## Usage
//!
//! ```ignore
//! let executor = UnifiedAiSessionExecutor::new(app_state, app_handle, pid_tracker);
//!
//! // The session runs until completion or manual stop.
//! // Health monitoring is handled by the Doctor service.
//! let config = AiSessionConfig::setup("exec-123", "My Workflow", "Setup AI Task");
//!
//! let result = executor.execute(&config, "Fix the code", logger).await;
//! ```

#![allow(dead_code)]

use std::sync::Arc;
use tauri::Emitter;
use tracing::{error, info, instrument, warn};

use crate::database::CreateTaskRunEventInput;
use crate::doctor::DoctorHandle;
use crate::execution_context::AiSessionContext;
use crate::mcp::shared::{
    FindingContext, ProgressContext, ReflectionFixContext, FINDING_INSTRUCTIONS,
};
use crate::runtime_env::{
    get_available_mcp_tools, AiSessionContextExt, AiSessionContextToolsExt, ExecutionContextExt,
};
use crate::settings::get_ai_settings;
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::step_registry::{StepEventKind, StepEventLogger};
use crate::step_types::StepType;
use crate::unified_workflow_executor::{get_parent_task_id, WorkflowPhase};
use crate::AppState;

// =============================================================================
// Configuration Types
// =============================================================================

/// Configuration for an AI session execution.
#[derive(Debug, Clone)]
pub struct AiSessionConfig {
    /// The task run ID this session belongs to.
    pub task_run_id: String,
    /// Name of the workflow (for session naming).
    pub workflow_name: String,
    /// Which phase this AI session is part of.
    pub phase: WorkflowPhase,
    /// Iteration number (required for Verification/Agentic phases).
    pub iteration: Option<u32>,
    /// Human-readable name of the step (for logging).
    pub step_name: String,
    /// Whether to prepend autonomous execution context to the prompt.
    pub add_autonomous_context: bool,
    /// Whether to append finding instructions to the prompt.
    pub append_finding_instructions: bool,
    /// Whether to strip [TASK_COMPLETE] marker instructions from the prompt.
    pub strip_completion_markers: bool,
    /// Optional checkpoint ID for progress tracking.
    /// If provided, progress markers detected in AI output will be saved to the database.
    pub checkpoint_id: Option<String>,
    /// Metadata for sub-steps when using consolidated prompts.
    /// Used for granular progress tracking with [STEP_COMPLETE:id] markers.
    pub sub_step_metadata: Option<Vec<crate::executor::prompt_builder::SubStepMetadata>>,
    /// Optional context for reflection fix marker detection.
    /// When set, the AI session will parse [REFLECTION_FIX:...] markers and store them in the database.
    pub reflection_fix_ctx: Option<ReflectionFixContext>,
    /// Optional context for step injection marker detection.
    /// When set, the AI session will parse [INJECT_STEP]...[/INJECT_STEP] markers
    /// and collect injected verification steps.
    pub step_injection_ctx: Option<crate::step_injection::types::StepInjectionContext>,
    /// Optional model override from stage config (e.g., "claude-sonnet-4-5-20250514").
    /// When set, the AI CLI will be invoked with --model <model>.
    pub model_override: Option<String>,
    /// Optional temperature override for API providers (0.0–1.0).
    pub temperature_override: Option<f32>,
    /// Optional max output tokens override for API providers.
    pub max_tokens_override: Option<u32>,
}

impl AiSessionConfig {
    /// Create a configuration for a setup phase AI session.
    pub fn setup(
        task_run_id: impl Into<String>,
        workflow_name: impl Into<String>,
        step_name: impl Into<String>,
    ) -> Self {
        Self {
            task_run_id: task_run_id.into(),
            workflow_name: workflow_name.into(),
            phase: WorkflowPhase::Setup,
            iteration: None,
            step_name: step_name.into(),
            add_autonomous_context: true,
            append_finding_instructions: false,
            strip_completion_markers: true,
            checkpoint_id: None,
            sub_step_metadata: None,
            reflection_fix_ctx: None,
            step_injection_ctx: None,
            model_override: None,
            temperature_override: None,
            max_tokens_override: None,
        }
    }

    /// Create a configuration for an agentic phase AI session.
    pub fn agentic(
        task_run_id: impl Into<String>,
        workflow_name: impl Into<String>,
        iteration: u32,
    ) -> Self {
        Self {
            task_run_id: task_run_id.into(),
            workflow_name: workflow_name.into(),
            phase: WorkflowPhase::Agentic,
            iteration: Some(iteration),
            step_name: format!("Fix issues (iteration {})", iteration),
            add_autonomous_context: true,
            append_finding_instructions: true,
            strip_completion_markers: true,
            checkpoint_id: None,
            sub_step_metadata: None,
            reflection_fix_ctx: None,
            step_injection_ctx: None,
            model_override: None,
            temperature_override: None,
            max_tokens_override: None,
        }
    }

    /// Create a configuration for a completion phase AI session.
    pub fn completion(
        task_run_id: impl Into<String>,
        workflow_name: impl Into<String>,
        step_name: impl Into<String>,
        iterations_run: u32,
    ) -> Self {
        Self {
            task_run_id: task_run_id.into(),
            workflow_name: workflow_name.into(),
            phase: WorkflowPhase::Completion,
            iteration: Some(iterations_run), // Used for turn count calculation
            step_name: step_name.into(),
            add_autonomous_context: false,
            append_finding_instructions: true,
            strip_completion_markers: true,
            checkpoint_id: None,
            sub_step_metadata: None,
            reflection_fix_ctx: None,
            step_injection_ctx: None,
            model_override: None,
            temperature_override: None,
            max_tokens_override: None,
        }
    }

    /// Set the checkpoint ID for progress tracking.
    ///
    /// If set, progress markers detected in AI output will be saved to the database
    /// and associated with this checkpoint.
    pub fn with_checkpoint_id(mut self, checkpoint_id: impl Into<String>) -> Self {
        self.checkpoint_id = Some(checkpoint_id.into());
        self
    }

    /// Set sub-step metadata for granular progress tracking.
    ///
    /// When using consolidated prompts with multiple sub-steps, this metadata
    /// allows tracking completion of individual sub-steps via [STEP_COMPLETE:id] markers.
    pub fn with_sub_step_metadata(
        mut self,
        metadata: Vec<crate::executor::prompt_builder::SubStepMetadata>,
    ) -> Self {
        self.sub_step_metadata = Some(metadata);
        self
    }

    /// Set the reflection fix context for parsing [REFLECTION_FIX:...] markers.
    ///
    /// When set, the AI session will detect and store reflection fixes emitted by the AI.
    /// This is only used during reflection workflows.
    pub fn with_reflection_fix_ctx(mut self, ctx: ReflectionFixContext) -> Self {
        self.reflection_fix_ctx = Some(ctx);
        self
    }

    /// Set the step injection context for parsing [INJECT_STEP]...[/INJECT_STEP] markers.
    ///
    /// When set, the AI session will collect injected verification steps from the AI output.
    pub fn with_step_injection_ctx(
        mut self,
        ctx: crate::step_injection::types::StepInjectionContext,
    ) -> Self {
        self.step_injection_ctx = Some(ctx);
        self
    }

    /// Set a model override for the AI CLI invocation.
    /// When set, the Claude CLI will be invoked with `--model <model>`.
    pub fn with_model_override(mut self, model: Option<String>) -> Self {
        self.model_override = model;
        self
    }

    /// Set a temperature override for API providers.
    pub fn with_temperature(mut self, temperature: Option<f32>) -> Self {
        self.temperature_override = temperature;
        self
    }

    /// Set a max_tokens override for API providers.
    pub fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens_override = max_tokens;
        self
    }
}

/// Result of an AI session execution.
#[derive(Debug, Clone)]
pub struct AiSessionResult {
    /// Whether the AI session succeeded.
    pub success: bool,
    /// The AI's output text.
    pub output: String,
    /// Duration of the session in milliseconds.
    pub duration_ms: i64,
    /// Dynamically injected verification steps parsed from AI output.
    /// These are collected from `[INJECT_STEP]...[/INJECT_STEP]` markers.
    pub injected_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Error message when the session failed (e.g., CLI spawn failure, timeout).
    /// Empty string if no error occurred.
    pub error: String,
    /// Input tokens consumed (available for API providers only, None for CLI).
    pub input_tokens: Option<u64>,
    /// Output tokens generated (available for API providers only, None for CLI).
    pub output_tokens: Option<u64>,
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Build context explaining that the AI is running autonomously without user interaction.
///
/// This context is prepended to agentic phase prompts to ensure the AI:
/// 1. Does NOT ask questions expecting user responses
/// 2. Uses [FINDING:needs_review] to flag things requiring user attention
/// 3. Makes reasonable decisions and documents them
fn build_autonomous_execution_context() -> &'static str {
    r#"## AUTONOMOUS EXECUTION MODE

You are running **autonomously** as part of an automated workflow. There is NO user monitoring this session in real-time.

### Critical Rules:

1. **DO NOT ask questions expecting a response.** No one is watching to answer.
   - "Should I delete this file?"
   - "Which approach do you prefer?"
   - "Is it okay if I...?"

2. **Make reasonable decisions and document them using the [FINDING:...] format** (see below).

3. **Flag anything requiring human judgment:**
   - Security-sensitive decisions -> use `[FINDING:todo:medium:needs_input]`
   - Ambiguous requirements -> use `[FINDING:todo:medium:needs_input]`
   - Files that might need manual cleanup -> use `[FINDING:warning:low]`
   - Unexpected findings that don't block the task -> use `[FINDING:warning:info]`

The user will review all findings after the workflow completes. Focus on completing your task and documenting anything noteworthy.

---

"#
}

/// Build trigger API documentation for the AI agent.
///
/// This documents the trigger management API so the agent can create
/// event-driven workflow automation during execution.
fn build_trigger_api_docs() -> &'static str {
    r##"## Workflow Triggers
Create event-driven triggers that automatically run workflows:

```bash
# Create a file-watch trigger (runs workflow when files change)
curl -s -X POST http://localhost:9876/triggers \
  -H "Content-Type: application/json" \
  -d '{"name":"On src change","trigger_config":{"type":"file_watch","paths":["/path/to/src"],"patterns":["*.ts","*.rs"],"ignore_patterns":["node_modules/**"],"recursive":true},"workflow_id":"WORKFLOW_UUID"}'
```

Trigger types: webhook, file_watch, workflow_chain, git_event, health_check, schedule

```bash
# List triggers
curl -s http://localhost:9876/triggers
# Test a trigger (dry run)
curl -s -X POST http://localhost:9876/triggers/TRIGGER_ID/test
# Delete a trigger
curl -s -X DELETE http://localhost:9876/triggers/TRIGGER_ID
```
"##
}

/// Build canvas API documentation for the AI agent.
///
/// This documents the A2UI canvas panel API so the agent can render
/// rich visual content in the user's dashboard during execution.
fn build_canvas_api_docs() -> &'static str {
    r##"## Canvas Panels

Use canvas panels to show structured data visually in the user's dashboard instead of dumping raw text into the conversation. Panels persist across messages and can be updated in-place.

### API Reference

**Create or update** (POST — reuse panel_id to update in-place):
```bash
curl -s -X POST http://localhost:9876/canvas/panels \
  -H "Content-Type: application/json" \
  -d '{"panel_id":"my-panel","component":"Table","title":"Results","data":{...}}'
```

Optional fields: `"priority":10` (lower=first, default 50), `"size":"compact"|"normal"|"large"`, `"group":"Analysis"` (section header).

**Delete:** `curl -s -X DELETE http://localhost:9876/canvas/panels/my-panel`

### Component Schemas

- **Markdown** — `{"content":"# Heading\nParagraph with **bold**"}` — Rich text, summaries, explanations
- **Table** — `{"columns":["File","Issue"],"rows":[["app.ts","Null check"]],"sortable":true}` — Comparisons, data grids
- **CodeDiff** — `{"file_path":"src/app.ts","language":"typescript","unified_diff":"@@ -1,3 +1,3 @@\n-old\n+new"}` — Code changes, before/after
- **FileTree** — `{"root":"src/","entries":[{"path":"app.ts","type":"file","status":"modified"}]}` — File listings with status
- **KeyValue** — `{"pairs":[{"key":"Status","value":"OK","style":"success"}]}` — Config, metadata (styles: default/success/warning/error)
- **Terminal** — `{"lines":["$ npm test","PASS 42 tests"]}` — Command output, logs
- **Alert** — `{"severity":"warning","message":"Deprecated API","details":"Use v2 endpoint"}` — Notices (info/success/warning/error)
- **Timeline** — `{"events":[{"title":"Build","status":"success"},{"title":"Deploy","status":"running"}]}` — Step progress (pending/running/success/failed)
- **ProgressChart** — `{"segments":[{"label":"Pass","value":42,"color":"green"},{"label":"Fail","value":3,"color":"red"}]}` — Metric breakdowns
- **FindingList** — `{"findings":[{"title":"SQL injection","severity":"high","location":"auth.ts:42"}]}` — Issues, audit results (info/low/medium/high/critical)
- **Checklist** — `{"items":[{"id":"1","label":"Fix imports","checked":true},{"id":"2","label":"Add tests","checked":false}]}` — Task tracking

### Best Practices

- Prefer **updating** an existing panel (same panel_id) over creating many similar panels
- Use **Table** for comparisons, **CodeDiff** for changes, **Checklist** for progress tracking
- Use **group** to organize related panels under section headers (e.g., group:"Analysis", group:"Results")
- Use **priority** to control display order — important panels first (priority:10), supplementary last (priority:90)
"##
}

/// Strip [TASK_COMPLETE] and similar completion marker instructions from prompts.
///
/// In unified workflows, verification determines completion, not the AI.
/// This removes any instructions telling the AI to output completion markers.
fn strip_completion_marker_instructions(prompt: &str) -> String {
    let patterns_to_remove = [
        "When you complete the task, include a summary line starting with [TASK_COMPLETE] followed by a brief summary.",
        "When complete, print [TASK_COMPLETE].",
        "When the goal is VERIFIED achieved, print [TASK_COMPLETE].",
        "Continue the task. When complete, print [TASK_COMPLETE].",
        "Continue the task. When the goal is VERIFIED achieved, print [TASK_COMPLETE].",
        "Continue the task from where you left off. When complete, print [TASK_COMPLETE].",
        "print [TASK_COMPLETE]",
        "output [TASK_COMPLETE]",
        "[TASK_COMPLETE]",
    ];

    let mut result = prompt.to_string();
    for pattern in patterns_to_remove {
        result = result.replace(pattern, "");
    }

    // Clean up any resulting double newlines
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}

// =============================================================================
// Unified AI Session Executor
// =============================================================================

/// Unified executor for AI sessions across all workflow phases.
///
/// This consolidates the duplicated AI session handling logic from SetupExecutor,
/// AgenticExecutor, and CompletionExecutor into a single reusable component.
pub struct UnifiedAiSessionExecutor {
    app_state: Arc<AppState>,
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    /// Optional session manager for interactive bidirectional sessions.
    /// When present, sessions use the stream-json protocol for multi-turn interaction.
    pub(crate) session_manager: Option<Arc<crate::claude_session::SessionManager>>,
}

impl UnifiedAiSessionExecutor {
    /// Create a new unified AI session executor.
    pub fn new(
        app_state: Arc<AppState>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            app_state,
            app_handle,
            pid_tracker,
            session_manager: None,
        }
    }

    /// Set the session manager for interactive mode.
    pub fn with_session_manager(
        mut self,
        session_manager: Arc<crate::claude_session::SessionManager>,
    ) -> Self {
        self.session_manager = Some(session_manager);
        self
    }

    /// Execute an AI session with the given configuration and prompt.
    ///
    /// This method:
    /// 1. Builds the session context based on phase
    /// 2. Builds the finding context based on phase
    /// 3. Transforms the prompt (autonomous context, marker stripping, finding instructions)
    /// 4. Calls run_claude_session_with_retry
    /// 5. Logs events via StepEventLogger
    /// 6. Returns AiSessionResult
    #[instrument(
        name = "ai.session",
        skip(self, prompt, logger),
        fields(
            execution_id = %config.task_run_id,
            workflow_name = %config.workflow_name,
            phase = ?config.phase,
            iteration = config.iteration,
            step_name = %config.step_name
        )
    )]
    pub async fn execute(
        &self,
        config: &AiSessionConfig,
        prompt: &str,
        logger: &StepEventLogger,
    ) -> AiSessionResult {
        let session_id = self.build_session_id(config);
        let start_time = std::time::Instant::now();

        // Get workspace root
        let workspace_root = crate::mcp::shared::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // Clone necessary values for the blocking task
        let pid_tracker = self.pid_tracker.clone();
        let retry_config = get_ai_settings().retry;
        let app_handle = self.app_handle.clone();

        // Extract DoctorHandle for health monitoring registration
        let doctor_handle: Option<DoctorHandle> = {
            let lock = self.app_state.doctor_handle.lock().await;
            lock.clone()
        };

        // Get available MCP tools before creating session context
        let available_tools = get_available_mcp_tools(&self.app_state).await;

        // Build session context based on phase
        let session_ctx = self.build_session_context(config, available_tools);

        // Build finding context based on phase
        let finding_ctx = self.build_finding_context(config);

        // Build progress context if checkpoint_id is provided
        let progress_ctx = self.build_progress_context(config);

        // Get event kinds for this phase
        let (start_kind, complete_kind, error_kind) = self.get_event_kinds(config.phase);

        // Create metadata for consistent events
        let metadata = self.build_step_metadata(config);
        let details = StepDetails::ai_session(session_id.clone());

        info!(
            "UNIFIED-AI-SESSION: Running {} AI (session: {}, phase: {:?}, checkpoint: {:?})",
            config.phase.as_str(),
            session_id,
            config.phase,
            config.checkpoint_id
        );

        // Log start event
        if let Err(e) = logger.log_start(start_kind, metadata.clone(), details.clone()) {
            warn!("Failed to log AI start event: {}", e);
        }

        // Emit sub_step_started events for all sub-steps in the batch
        if let Some(ref sub_steps) = config.sub_step_metadata {
            let total_sub_steps = sub_steps.len();
            for (index, sub_step) in sub_steps.iter().enumerate() {
                let sub_step_event = serde_json::json!({
                    "checkpoint_id": config.checkpoint_id.clone().unwrap_or_default(),
                    "task_run_id": config.task_run_id.clone(),
                    "sub_step_id": sub_step.sub_step_id.clone(),
                    "sub_step_index": index,
                    "total_sub_steps": total_sub_steps,
                    "step_name": sub_step.step_name.clone(),
                    "phase": config.phase.as_str(),
                    "timestamp": chrono::Utc::now().timestamp_millis(),
                });
                if let Err(e) = self.app_handle.emit("sub_step_started", &sub_step_event) {
                    warn!("Failed to emit sub_step_started event: {}", e);
                }
            }
            info!(
                "Emitted sub_step_started events for {} sub-steps in {} phase",
                total_sub_steps,
                config.phase.as_str()
            );
        }

        // Transform prompt based on config flags
        let transformed_prompt = self.transform_prompt(config, prompt);

        // Determine whether to use interactive mode.
        // Interactive requires BOTH a session manager AND the setting to be enabled.
        let interactive_enabled = get_ai_settings().interactive_sessions_enabled;
        let use_interactive = self.session_manager.is_some() && interactive_enabled;

        if !use_interactive && self.session_manager.is_some() {
            info!(
                "UNIFIED-AI-SESSION: Interactive sessions disabled by setting, falling back to inline mode"
            );
        }

        // Run Claude session (interactive if session manager is available AND setting enabled, otherwise inline)
        let workspace_for_claude = workspace_root;
        let sid_for_claude = session_id.clone();
        let session_manager = if use_interactive {
            self.session_manager.clone()
        } else {
            None
        };
        let task_run_id_for_claude = config.task_run_id.clone();
        let reflection_fix_ctx = config.reflection_fix_ctx.clone();
        let step_injection_ctx = config.step_injection_ctx.clone();
        let model_override = config.model_override.clone();

        let result = tokio::task::spawn_blocking(move || {
            let doctor_ref = doctor_handle.as_ref();
            if let Some(ref sm) = session_manager {
                // Interactive mode: bidirectional stream-json session
                crate::claude_session::runner::run_claude_session_interactive_with_retry(
                    &workspace_for_claude,
                    &transformed_prompt,
                    &sid_for_claude,
                    &app_handle,
                    session_ctx,
                    finding_ctx,
                    progress_ctx,
                    Some(pid_tracker),
                    Some(&retry_config),
                    sm,
                    &task_run_id_for_claude,
                    doctor_ref,
                    reflection_fix_ctx,
                    step_injection_ctx,
                    model_override.as_deref(),
                )
            } else {
                // Inline mode: one-shot session (either no session manager or interactive disabled)
                info!(
                    "UNIFIED-AI-SESSION: Using inline (non-interactive) mode for session {}",
                    sid_for_claude
                );
                crate::claude_session::runner::run_claude_session_with_retry(
                    &workspace_for_claude,
                    &transformed_prompt,
                    &sid_for_claude,
                    &app_handle,
                    session_ctx,
                    finding_ctx,
                    progress_ctx,
                    Some(pid_tracker),
                    Some(&retry_config),
                    doctor_ref,
                    reflection_fix_ctx,
                    step_injection_ctx,
                    model_override.as_deref(),
                )
            }
        })
        .await;

        let duration_ms = start_time.elapsed().as_millis() as i64;

        // Rebuild metadata for completion event
        let metadata = self.build_step_metadata(config);

        // Handle result and log appropriate events
        match result {
            Ok(Ok((success, output, _, injected_steps))) => {
                info!(
                    "UNIFIED-AI-SESSION: {} completed (success={}, output={} chars, duration={}ms, injected_steps={})",
                    config.phase.as_str(),
                    success,
                    output.len(),
                    duration_ms,
                    injected_steps.len()
                );

                let details =
                    StepDetails::ai_session_complete(session_id.clone(), output.len(), duration_ms);

                let kind = if success { complete_kind } else { error_kind };
                let error_msg = if success {
                    None
                } else {
                    Some("AI reported failure")
                };

                if let Err(e) =
                    logger.log_end(kind, metadata, details, duration_ms, success, error_msg)
                {
                    warn!("Failed to log AI complete event: {}", e);
                }

                // Save AI output as a task_run_event for the AI Data viewer
                // This ensures completed runs show AI output from all phases
                if !output.is_empty() {
                    self.save_ai_output_event(config, &session_id, &output, duration_ms);
                }

                // Persist AI output to task_run_output_chunks for the /output endpoint.
                // The agentic phase is already persisted by the loop_controller with
                // iteration headers, so only persist setup and completion here.
                if !output.is_empty()
                    && matches!(
                        config.phase,
                        WorkflowPhase::Setup | WorkflowPhase::Completion
                    )
                {
                    let phase_label = config.phase.as_str();
                    let formatted = format!(
                        "\n--- AI {} Output ({}) ---\n{}\n",
                        phase_label, config.step_name, output
                    );
                    let task_run_id = get_parent_task_id(&config.task_run_id);
                    if let Err(e) = self.app_state.checkpoint_db.append_task_output_ex(
                        &task_run_id,
                        &formatted,
                        false,
                        false,
                    ) {
                        warn!(
                            "Failed to persist {} AI output to chunks: {}",
                            phase_label, e
                        );
                    }
                }

                AiSessionResult {
                    success,
                    output,
                    duration_ms,
                    injected_steps,
                    error: String::new(),
                    input_tokens: None,
                    output_tokens: None,
                }
            }
            Ok(Err(e)) => {
                error!(
                    "UNIFIED-AI-SESSION: {} failed: {}",
                    config.phase.as_str(),
                    e
                );

                let details = StepDetails::ai_session(session_id.clone()).with_error(e.to_string());

                if let Err(log_err) =
                    logger.log_error(error_kind, metadata, details, duration_ms, Some(&e))
                {
                    warn!("Failed to log AI error event: {}", log_err);
                }

                AiSessionResult {
                    success: false,
                    output: String::new(),
                    duration_ms,
                    injected_steps: Vec::new(),
                    error: e.to_string(),
                    input_tokens: None,
                    output_tokens: None,
                }
            }
            Err(e) => {
                error!(
                    "UNIFIED-AI-SESSION: Task join error for {}: {}",
                    config.phase.as_str(),
                    e
                );

                let details = StepDetails::ai_session(session_id.clone()).with_error(e.to_string());
                let error_msg = format!("Task join error: {}", e);

                if let Err(log_err) =
                    logger.log_error(error_kind, metadata, details, duration_ms, Some(&error_msg))
                {
                    warn!("Failed to log AI error event: {}", log_err);
                }

                AiSessionResult {
                    success: false,
                    output: String::new(),
                    duration_ms,
                    injected_steps: Vec::new(),
                    error: error_msg,
                    input_tokens: None,
                    output_tokens: None,
                }
            }
        }
    }

    // =========================================================================
    // Helper Methods
    // =========================================================================

    /// Save AI output as a task_run_event so the AI Data viewer shows it for completed runs.
    fn save_ai_output_event(
        &self,
        config: &AiSessionConfig,
        session_id: &str,
        output: &str,
        duration_ms: i64,
    ) {
        let phase_label = config.phase.as_str();
        let iteration_label = config
            .iteration
            .map(|i| format!(" (iteration {})", i))
            .unwrap_or_default();
        let message = format!(
            "AI {} session{}: {}",
            phase_label, iteration_label, &config.step_name
        );

        let data = serde_json::json!({
            "session_id": session_id,
            "phase": phase_label,
            "iteration": config.iteration,
            "step_name": config.step_name,
            "output": output,
            "output_length": output.len(),
            "source": "claude",
        });

        // For composed run children (e.g., composed-run-X-workflow-N),
        // remap to the parent task ID since only the parent has a task_runs record.
        let task_run_id = get_parent_task_id(&config.task_run_id);

        let event = CreateTaskRunEventInput {
            task_run_id,
            event_type: "ai_output".to_string(),
            event_subtype: Some(phase_label.to_string()),
            message,
            data: Some(data.to_string()),
            workflow_name: Some(config.workflow_name.clone()),
            state_name: None,
            action_id: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: Some(duration_ms),
        };

        if let Err(e) = self.app_state.checkpoint_db.create_task_run_event(&event) {
            warn!("Failed to save AI output event to database: {}", e);
        }
    }

    /// Build a session ID based on the configuration.
    fn build_session_id(&self, config: &AiSessionConfig) -> String {
        match config.phase {
            WorkflowPhase::Setup => format!("{}-setup", config.task_run_id),
            WorkflowPhase::Verification => {
                let iter = config.iteration.unwrap_or(1);
                format!("{}-verification-{}", config.task_run_id, iter)
            }
            WorkflowPhase::Agentic => {
                let iter = config.iteration.unwrap_or(1);
                format!("{}-agentic-{}", config.task_run_id, iter)
            }
            WorkflowPhase::Completion => format!("{}-completion", config.task_run_id),
        }
    }

    /// Build the AiSessionContext based on the phase.
    fn build_session_context(
        &self,
        config: &AiSessionConfig,
        available_tools: Vec<String>,
    ) -> Option<AiSessionContext> {
        let ctx = match config.phase {
            WorkflowPhase::Setup => {
                AiSessionContext::setup(&config.task_run_id, &config.workflow_name)
                    .with_runtime_env()
                    .with_new_trace()
                    .with_ai_settings()
                    .with_available_tools(available_tools)
                    .with_turn_count(1) // Setup is always turn 1
            }
            WorkflowPhase::Verification => {
                let iteration = config.iteration.unwrap_or(1);
                AiSessionContext::verification(
                    &config.task_run_id,
                    &config.workflow_name,
                    iteration,
                )
                .with_runtime_env()
                .with_new_trace()
                .with_ai_settings()
                .with_available_tools(available_tools)
                .with_turn_count(iteration)
            }
            WorkflowPhase::Agentic => {
                let iteration = config.iteration.unwrap_or(1);
                AiSessionContext::agentic(&config.task_run_id, &config.workflow_name, iteration)
                    .with_runtime_env()
                    .with_new_trace()
                    .with_ai_settings()
                    .with_available_tools(available_tools)
                    .with_turn_count(iteration) // Iteration IS the turn number for agentic phase
            }
            WorkflowPhase::Completion => {
                // Calculate turn count based on workflow phases:
                // - Setup phase = turn 1
                // - Agentic phases = turns 2, 3, 4... (one per iteration)
                // - Completion phase = iterations + 2
                let iterations_run = config.iteration.unwrap_or(0);
                let completion_turn = if iterations_run > 0 {
                    iterations_run + 2
                } else {
                    2
                };

                AiSessionContext::completion(&config.task_run_id, &config.workflow_name)
                    .with_runtime_env()
                    .with_new_trace()
                    .with_ai_settings()
                    .with_available_tools(available_tools)
                    .with_turn_count(completion_turn)
            }
        };

        Some(ctx)
    }

    /// Build the FindingContext based on the phase.
    fn build_finding_context(&self, config: &AiSessionConfig) -> Option<FindingContext> {
        let session_num = match config.phase {
            WorkflowPhase::Setup => 0,
            WorkflowPhase::Verification | WorkflowPhase::Agentic => config.iteration.unwrap_or(1),
            WorkflowPhase::Completion => 999, // Special marker for completion phase
        };

        Some(FindingContext {
            task_run_id: config.task_run_id.clone(),
            session_num,
        })
    }

    /// Build the ProgressContext if checkpoint_id is provided.
    fn build_progress_context(&self, config: &AiSessionConfig) -> Option<ProgressContext> {
        config
            .checkpoint_id
            .as_ref()
            .map(|checkpoint_id| ProgressContext {
                checkpoint_id: checkpoint_id.clone(),
                task_run_id: config.task_run_id.clone(),
            })
    }

    /// Transform the prompt based on configuration flags.
    fn transform_prompt(&self, config: &AiSessionConfig, prompt: &str) -> String {
        let mut result = prompt.to_string();

        // Strip completion markers if configured
        if config.strip_completion_markers {
            result = strip_completion_marker_instructions(&result);
        }

        // Prepend autonomous context if configured
        if config.add_autonomous_context {
            let autonomous_context = build_autonomous_execution_context();
            result = format!("{}{}", autonomous_context, result);
        }

        // Append canvas and trigger API documentation for agentic phases
        if config.add_autonomous_context {
            result = format!(
                "{}\n\n{}\n\n{}",
                result,
                build_canvas_api_docs(),
                build_trigger_api_docs()
            );
        }

        // Append finding instructions if configured
        if config.append_finding_instructions {
            result = format!("{}{}", result, FINDING_INSTRUCTIONS);
        }

        result
    }

    /// Get the start, complete, and error event kinds for a phase.
    fn get_event_kinds(
        &self,
        phase: WorkflowPhase,
    ) -> (StepEventKind, StepEventKind, StepEventKind) {
        match phase {
            WorkflowPhase::Setup => (
                StepEventKind::SetupAiStart,
                StepEventKind::SetupAiComplete,
                StepEventKind::SetupAiError,
            ),
            WorkflowPhase::Verification => (
                StepEventKind::VerificationStepStart,
                StepEventKind::VerificationStepComplete,
                StepEventKind::VerificationStepError,
            ),
            WorkflowPhase::Agentic => (
                StepEventKind::AgenticAiStart,
                StepEventKind::AgenticAiComplete,
                StepEventKind::AgenticAiError,
            ),
            WorkflowPhase::Completion => (
                StepEventKind::CompletionAiStart,
                StepEventKind::CompletionAiComplete,
                StepEventKind::CompletionAiError,
            ),
        }
    }

    /// Build step metadata for the AI session.
    fn build_step_metadata(&self, config: &AiSessionConfig) -> StepMetadata {
        match config.phase {
            WorkflowPhase::Setup => {
                StepMetadata::setup(&config.task_run_id, StepType::Prompt, &config.step_name, 0)
            }
            WorkflowPhase::Verification => {
                let iteration = config.iteration.unwrap_or(1);
                StepMetadata::verification(
                    &config.task_run_id,
                    StepType::Prompt,
                    &config.step_name,
                    0,
                    iteration,
                )
            }
            WorkflowPhase::Agentic => {
                let iteration = config.iteration.unwrap_or(1);
                StepMetadata::agentic(
                    &config.task_run_id,
                    StepType::AiSession,
                    &config.step_name,
                    0, // Fixed: agentic is a single step, not indexed
                    iteration,
                )
            }
            WorkflowPhase::Completion => StepMetadata::completion(
                &config.task_run_id,
                StepType::Prompt,
                &config.step_name,
                0,
            ),
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_completion_marker_instructions() {
        let prompt = "Fix the code. When complete, print [TASK_COMPLETE]. Make sure tests pass.";
        let result = strip_completion_marker_instructions(prompt);
        assert_eq!(result, "Fix the code.  Make sure tests pass.");
        assert!(!result.contains("[TASK_COMPLETE]"));
    }

    #[test]
    fn test_strip_completion_marker_instructions_multiple() {
        let prompt = "Fix the code. print [TASK_COMPLETE] And output [TASK_COMPLETE] at the end.";
        let result = strip_completion_marker_instructions(prompt);
        assert!(!result.contains("[TASK_COMPLETE]"));
    }

    #[test]
    fn test_ai_session_config_setup() {
        let config = AiSessionConfig::setup("task-123", "My Workflow", "Setup Step");
        assert_eq!(config.phase, WorkflowPhase::Setup);
        assert!(config.iteration.is_none());
        assert!(config.add_autonomous_context);
        assert!(!config.append_finding_instructions);
        assert!(config.strip_completion_markers);
        assert!(config.checkpoint_id.is_none());
    }

    #[test]
    fn test_ai_session_config_with_checkpoint_id() {
        let config = AiSessionConfig::agentic("task-123", "My Workflow", 1)
            .with_checkpoint_id("checkpoint-abc-123");
        assert_eq!(config.checkpoint_id, Some("checkpoint-abc-123".to_string()));
    }

    #[test]
    fn test_ai_session_config_agentic() {
        let config = AiSessionConfig::agentic("task-123", "My Workflow", 3);
        assert_eq!(config.phase, WorkflowPhase::Agentic);
        assert_eq!(config.iteration, Some(3));
        assert!(config.add_autonomous_context);
        assert!(config.append_finding_instructions);
        assert!(config.strip_completion_markers);
    }

    #[test]
    fn test_ai_session_config_completion() {
        let config = AiSessionConfig::completion("task-123", "My Workflow", "Completion Step", 5);
        assert_eq!(config.phase, WorkflowPhase::Completion);
        assert_eq!(config.iteration, Some(5)); // iterations_run for turn count
        assert!(!config.add_autonomous_context);
        assert!(config.append_finding_instructions);
        assert!(config.strip_completion_markers);
    }

    #[test]
    fn test_build_autonomous_execution_context() {
        let context = build_autonomous_execution_context();
        assert!(context.contains("AUTONOMOUS EXECUTION MODE"));
        assert!(context.contains("DO NOT ask questions"));
        assert!(context.contains("[FINDING:"));
    }
}
