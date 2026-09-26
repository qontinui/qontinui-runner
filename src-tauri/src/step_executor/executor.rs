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
use crate::iteration_bundle::{
    parse_action_events, parse_image_recognition_events, RelevantLogSources,
};
use crate::orchestrator::context_propagation::{RuntimeContext, SharedVariableStore};
use crate::unified_workflow_executor::get_parent_task_id;

// Handler system imports
use super::breakpoint::{BreakpointManager, StalenessCheck};
use super::handlers::{HandlerContext, HandlerRegistry};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

// Types extracted to executor_types.rs (re-exported via mod.rs)
use super::executor_types::*;

// ============================================================================
// Typed dispatch helpers — Session 2a
// ============================================================================

/// Build a [`FullRunnerStep`] variant from an `ExecutionStepConfig`.
///
/// Two construction paths:
///
/// 1. **Direct Rust-field constructor** (preferred): for step types whose
///    inner `qontinui-types` struct uses bare field names that collide with
///    other variants on the fat `ExecutionStepConfig` struct (e.g. `action`,
///    `target`, `workflow_name`). Reading typed Rust fields sidesteps the
///    JSON field-name ambiguity and always succeeds for well-formed configs.
///
/// 2. **JSON round-trip fallback**: everything else uses
///    `serde_json::to_value` + `from_value`. `ExecutionStepConfig` serialises
///    `step_type` as `"type"` (via `#[serde(rename = "type")]`), which is
///    the tag field `FullRunnerStep`'s `#[serde(tag = "type")]` expects, so
///    the intermediate `Value` has the right shape for `from_value`.
///
/// Add a new arm to the match below whenever a new step type is introduced
/// that the JSON round-trip can't parse cleanly — don't paper over it with
/// looser serde aliases on `ExecutionStepConfig`. `spec_check`,
/// `wrapper_action` and `effect_check` need no arm: their typed structs accept
/// the `ExecutionStepConfig` field names as aliases, which
/// `typed_dispatch_corpus_tests` proves on every live producer's shapes.
fn to_full_runner_step(
    step: &ExecutionStepConfig,
) -> Result<qontinui_types::workflow_step::FullRunnerStep, String> {
    use qontinui_types::workflow_step::{
        FullRunnerStep, UiBridgeAction, UiBridgeAssertType, UiBridgeComparisonMode,
        UiBridgeSeverity, UiBridgeStep, UiBridgeStepPhase, WorkflowStep, WorkflowStepPhase,
    };

    // Direct constructors for variants that don't round-trip cleanly.
    match step.step_type.as_str() {
        "ui_bridge" => {
            let action = match step.ui_bridge_action.as_deref() {
                Some(s) => parse_snake_enum::<UiBridgeAction>(s)
                    .map_err(|e| format!("ui_bridge.action: {e}"))?,
                // What the handler runs for an action-less step
                // (`DEFAULT_UI_BRIDGE_ACTION`), and what both editors show.
                None => UiBridgeAction::Snapshot,
            };
            let assert_type = step
                .ui_bridge_assert_type
                .as_deref()
                .map(|s| {
                    parse_snake_enum::<UiBridgeAssertType>(s)
                        .map_err(|e| format!("ui_bridge.assert_type: {e}"))
                })
                .transpose()?;
            let comparison_mode = step
                .ui_bridge_compare_mode
                .as_deref()
                .map(|s| {
                    parse_snake_enum::<UiBridgeComparisonMode>(s)
                        .map_err(|e| format!("ui_bridge.comparison_mode: {e}"))
                })
                .transpose()?;
            let severity_threshold = step
                .ui_bridge_severity_threshold
                .as_deref()
                .map(|s| {
                    parse_snake_enum::<UiBridgeSeverity>(s)
                        .map_err(|e| format!("ui_bridge.severity_threshold: {e}"))
                })
                .transpose()?;
            let phase = parse_phase_or_default::<UiBridgeStepPhase>(step.phase.as_deref(), || {
                UiBridgeStepPhase::default()
            });
            return Ok(FullRunnerStep::UiBridge(UiBridgeStep {
                base: base_from_step(step),
                phase,
                action,
                url: step.ui_bridge_url.clone(),
                instruction: step.ui_bridge_instruction.clone(),
                target: step.ui_bridge_target.clone(),
                assert_type,
                expected: step.ui_bridge_expected.clone(),
                timeout_ms: step.ui_bridge_timeout_ms,
                comparison_mode,
                reference_snapshot_id: step.ui_bridge_reference_snapshot_id.clone(),
                severity_threshold,
                ui_bridge_snapshot_target: step.ui_bridge_snapshot_target.clone(),
                action_plan: step.ui_bridge_action_plan.clone(),
            }));
        }
        "workflow" => {
            let workflow_id = step.ref_workflow_id.clone().ok_or_else(|| {
                "workflow step missing required field ref_workflow_id / workflowId".to_string()
            })?;
            let workflow_name = step
                .ref_workflow_name
                .clone()
                .or_else(|| step.name.clone())
                .unwrap_or_default();
            let phase = parse_phase_or_default::<WorkflowStepPhase>(step.phase.as_deref(), || {
                WorkflowStepPhase::default()
            });
            return Ok(FullRunnerStep::Workflow(WorkflowStep {
                base: base_from_step(step),
                phase,
                workflow_id,
                workflow_name,
            }));
        }
        _ => {}
    }

    let value = serde_json::to_value(step)
        .map_err(|e| format!("failed to serialize ExecutionStepConfig: {e}"))?;
    serde_json::from_value(value).map_err(|e| {
        format!(
            "failed to parse step as FullRunnerStep (type={:?}): {e}",
            step.step_type
        )
    })
}

/// Parse a snake_case enum variant name by wrapping in a JSON string and
/// routing through serde.  Works for any enum with
/// `#[serde(rename_all = "snake_case")]`.
fn parse_snake_enum<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_value::<T>(serde_json::Value::String(s.to_string()))
        .map_err(|e| format!("unknown variant {:?}: {e}", s))
}

/// Parse a `phase` string into a variant-specific phase enum, falling back
/// to the type's Default when the string is missing or unrecognised.
///
/// Variant phase enums are subsets of the global phase set (e.g. `UiBridgeStepPhase`
/// excludes `agentic`); an invalid value here is usually upstream data carrying a
/// phase the variant can't legally be in. Falling back to Default keeps dispatch
/// unblocked — the handler still reads `step.phase` from `ExecutionStepConfig`
/// when it needs the exact string.
fn parse_phase_or_default<T: serde::de::DeserializeOwned>(
    s: Option<&str>,
    default: impl FnOnce() -> T,
) -> T {
    s.and_then(|s| parse_snake_enum::<T>(s).ok())
        .unwrap_or_else(default)
}

/// Copy the `BaseStepFields` surface from an `ExecutionStepConfig`.
///
/// `ExecutionStepConfig` only carries `id` and `name` — the rest of the
/// `BaseStepFields` surface (inputs, extract, depends_on, retry, …) lives
/// on other dispatch paths and isn't needed for handler lookup.
fn base_from_step(step: &ExecutionStepConfig) -> qontinui_types::workflow_step::BaseStepFields {
    qontinui_types::workflow_step::BaseStepFields {
        id: step.id.clone().unwrap_or_default(),
        name: step.name.clone().unwrap_or_default(),
        ..Default::default()
    }
}

/// Exhaustive map from `FullRunnerStep` variant → handler-registry lookup key.
///
/// Adding a new `FullRunnerStep` variant **without** updating this match
/// produces a compile error — that is the entire point of this function.
/// No wildcard arm is allowed here.
fn handler_lookup_key(step: &qontinui_types::workflow_step::FullRunnerStep) -> &'static str {
    use qontinui_types::workflow_step::FullRunnerStep;
    match step {
        FullRunnerStep::Command(_) => "command",
        FullRunnerStep::Prompt(_) => "prompt",
        FullRunnerStep::UiBridge(_) => "ui_bridge",
        FullRunnerStep::Workflow(_) => "workflow",
        FullRunnerStep::CodeExecution(_) => "code_execution",
        FullRunnerStep::ExecutePlaybook(_) => "execute_playbook",
        FullRunnerStep::NativeAccessibility(_) => "native_accessibility",
        FullRunnerStep::RestartProcess(_) => "restart_process",
        FullRunnerStep::SaveWorkflowArtifact(_) => "save_workflow_artifact",
        FullRunnerStep::WorkflowFixup(_) => "workflow_fixup",
        FullRunnerStep::UiBridgeDesignAudit(_) => "ui_bridge_design_audit",
        FullRunnerStep::UiBridgeVisualAssertion(_) => "ui_bridge_visual_assertion",
        FullRunnerStep::WorkflowRef(_) => "workflow_ref",
        FullRunnerStep::DagCancel(_) => "dag_cancel",
        FullRunnerStep::DagApproval(_) => "dag_approval",
        FullRunnerStep::DagLoop(_) => "dag_loop",
        FullRunnerStep::VgaAutomate(_) => "vga_automate",
        FullRunnerStep::SpecCheck(_) => "spec_check",
        FullRunnerStep::WrapperAction(_) => "wrapper_action",
        FullRunnerStep::EffectCheck(_) => "effect_check",
    }
}

/// One arm of the legacy string `match` in `execute_single_step`, which serves
/// the step types that have no registered handler. None has a
/// `FullRunnerStep` variant, so their typed parse always fails; being listed
/// here is what routes that failure to the legacy `match` silently.
///
/// The `match` is exhaustive over this enum, so every listed type has an arm
/// by construction. The unregistered handler modules `check.rs`,
/// `check_group.rs` and `shell_command.rs` declare the same step types, but
/// they are internal to `CommandHandler` — the registry never serves them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LegacyStep {
    ShellCommand,
    Check,
    CheckGroup,
    Shell,
    LogWatch,
    Gate,
}

impl LegacyStep {
    /// Every legacy arm — the ONE authoritative list; `LEGACY_STRING_DISPATCH`
    /// and [`LegacyStep::from_type`] are derived from it.
    pub(super) const ALL: [LegacyStep; 6] = [
        Self::ShellCommand,
        Self::Check,
        Self::CheckGroup,
        Self::Shell,
        Self::LogWatch,
        Self::Gate,
    ];

    /// The step type string this arm serves.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ShellCommand => "shell_command",
            Self::Check => "check",
            Self::CheckGroup => "check_group",
            Self::Shell => "shell",
            Self::LogWatch => "log_watch",
            Self::Gate => "gate",
        }
    }

    /// The legacy arm serving `step_type`, if any.
    pub(super) fn from_type(step_type: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == step_type)
    }
}

const LEGACY_STRING_DISPATCH_ARRAY: [&str; LegacyStep::ALL.len()] = {
    let mut out = [""; LegacyStep::ALL.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = LegacyStep::ALL[i].as_str();
        i += 1;
    }
    out
};

/// Step types served by the legacy string `match` rather than a registered
/// handler, derived from [`LegacyStep::ALL`] so the two cannot disagree.
pub(super) const LEGACY_STRING_DISPATCH: &[&str] = &LEGACY_STRING_DISPATCH_ARRAY;

/// Where `execute_single_step` sends a step.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DispatchRoute {
    /// The typed parse succeeded; dispatch to the handler under this key.
    Registry(&'static str),
    /// A type the registry serves whose typed parse FAILED. It still runs, on
    /// the handler registered under the raw step type: handlers tolerate
    /// shapes the schema rejects (e.g. a command step stamped
    /// `phase: "agentic"` by `refetch_unified_workflow_steps`), and a step
    /// that worked must not start failing. Logged at `warn!` — after the
    /// typed parse was made total for live shapes, this fires only on a
    /// genuine schema/handler disagreement.
    RegistryFallback { key: String, parse_error: String },
    /// A [`LEGACY_STRING_DISPATCH`] type; served by the legacy `match`.
    Legacy(LegacyStep),
    /// Neither typed, registered, nor legacy.
    Unknown,
    /// A conversion seam could not parse the step
    /// ([`ExecutionStepConfig::conversion_error`]); the step fails with this
    /// message.
    ConversionFailed(String),
}

/// What `execute_single_step` returns for [`DispatchRoute::ConversionFailed`]:
/// a failed step carrying the conversion seam's message, and nothing run.
/// Split out so the outcome is testable without a `StepExecutor`.
pub(super) fn conversion_failure_outcome(
    error: String,
) -> (
    bool,
    Option<String>,
    Option<String>,
    Option<serde_json::Value>,
) {
    warn!("Step failed at conversion: {}", error);
    (false, Some(error), None, None)
}

/// Decide where a step goes. Pure, so the routing is testable without a
/// `StepExecutor` (which needs a live `AppState`).
pub(super) fn resolve_dispatch(
    step: &ExecutionStepConfig,
    registry: &HandlerRegistry,
) -> DispatchRoute {
    if let Some(error) = &step.conversion_error {
        return DispatchRoute::ConversionFailed(error.clone());
    }
    // "test" is a backward-compat alias with no `FullRunnerStep` variant; it
    // is served by the command handler.
    if step.step_type == "test" {
        return DispatchRoute::Registry("command");
    }
    match to_full_runner_step(step) {
        Ok(typed) => DispatchRoute::Registry(handler_lookup_key(&typed)),
        Err(parse_error) => {
            if let Some(legacy) = LegacyStep::from_type(&step.step_type) {
                tracing::debug!(
                    step_type = %step.step_type,
                    "legacy string-dispatched step type"
                );
                DispatchRoute::Legacy(legacy)
            } else if registry.has_handler(&step.step_type) {
                DispatchRoute::RegistryFallback {
                    key: step.step_type.clone(),
                    parse_error,
                }
            } else {
                DispatchRoute::Unknown
            }
        }
    }
}

// Imports from extracted modules

// Legacy step handlers extracted to legacy_steps.rs
// Verification execution extracted to verification_execution.rs

pub struct StepExecutor {
    pub(crate) action_service: UnifiedActionService,
    pub(crate) app_state: Arc<AppState>,
    /// Configuration storage for loading saved configs
    pub(crate) config_storage: Arc<TokioMutex<ConfigStorage>>,
    /// Optional app handle for emitting events to the Tauri frontend
    pub(crate) app_handle: Option<tauri::AppHandle>,
    /// Optional task run ID for database logging (AWAS steps, etc.)
    pub(crate) task_run_id: Option<String>,
    /// Runtime context for variable expansion in commands
    pub(crate) runtime_context: RuntimeContext,
    /// Shared variable store for API request chaining (thread-safe, clone-friendly)
    pub(crate) shared_variables: SharedVariableStore,
    /// Registry of step handlers for polymorphic dispatch
    pub(crate) handler_registry: HandlerRegistry,
    /// PID tracker for AI process management (passed to WorkflowStepHandler)
    pub(crate) pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
    /// Path scope policy for working directory resolution boundary enforcement.
    pub(crate) path_scope_policy: crate::paths::PathScopePolicy,
    /// Per-workflow security profile override (e.g., "standard", "strict").
    /// When set, overrides the default profile from settings.
    pub(crate) workflow_security_profile: Option<String>,
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
            path_scope_policy: crate::paths::PathScopePolicy::default(),
            workflow_security_profile: None,
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
            path_scope_policy: crate::paths::PathScopePolicy::default(),
            workflow_security_profile: None,
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

    /// Set the path scope policy for working directory boundary enforcement.
    pub fn set_path_scope_policy(&mut self, policy: crate::paths::PathScopePolicy) {
        self.path_scope_policy = policy;
    }

    /// Set the per-workflow security profile override.
    pub fn set_workflow_security_profile(&mut self, profile: Option<String>) {
        self.workflow_security_profile = profile;
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
    pub(crate) async fn create_handler_context(&self) -> HandlerContext {
        // Resolve the security policy: workflow profile override > settings default
        let mut security_settings = crate::settings::get_security_settings();
        if let Some(ref wf_profile) = self.workflow_security_profile {
            security_settings.default_profile = wf_profile.clone();
        }
        let security_policy = crate::security::PolicyEngine::resolve(None, &security_settings);
        let audit_logger =
            crate::security::audit::AuditLogger::new(security_settings.audit_enabled);

        let mut ctx = HandlerContext::with_shared_state(
            self.app_state.clone(),
            self.config_storage.clone(),
            self.app_handle.clone(),
            self.runtime_context.clone(),
            self.shared_variables.clone(),
            self.task_run_id.clone(),
            self.pid_tracker.clone(),
        )
        .with_path_scope_policy(self.path_scope_policy.clone())
        .with_security_policy(security_policy.clone())
        .with_audit_logger(audit_logger.clone());

        // Set up credential proxy when proxy mode is active
        let credential_proxy = if security_settings.credential_proxy_enabled
            && security_policy.credentials.mode == crate::security::policy::CredentialMode::Proxy
        {
            let cred_names: Vec<&str> =
                if security_policy.credentials.allowed_credentials.is_empty() {
                    vec!["claude_api", "openai", "gemini"]
                } else {
                    security_policy
                        .credentials
                        .allowed_credentials
                        .iter()
                        .map(|s| s.as_str())
                        .collect()
                };
            let cred_proxy = crate::security::credential_proxy::CredentialProxy::new(&cred_names);
            ctx = ctx.with_credential_placeholders(cred_proxy.placeholder_env_vars());
            Some(cred_proxy)
        } else {
            None
        };

        // Start network mediation proxy when enabled.
        // The mediator is stored on the HandlerContext so it lives for the step's duration.
        if security_settings.network_proxy_enabled {
            match crate::security::network_proxy::NetworkMediator::start(
                security_policy.clone(),
                audit_logger,
                credential_proxy,
            )
            .await
            {
                Ok(mediator) => {
                    let proxy_url = mediator.proxy_url_for_container();
                    info!("Network mediation proxy started at {}", proxy_url);
                    ctx = ctx
                        .with_network_proxy_url(proxy_url)
                        .with_network_mediator(mediator);
                }
                Err(e) => {
                    warn!("Failed to start network mediation proxy: {}", e);
                }
            }
        }

        ctx
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

        // Runtime context persistence removed — all persistence now via PgDb.
        let _ = task_run_id;
        let _ = context_json;
    }

    /// Log a step execution event to the database
    ///
    /// This logs step start, complete, and error events to the task_run_events table.
    ///
    /// Note: For composed run children (e.g., composed-run-X-workflow-N),
    /// the task_run_id is automatically remapped to the parent task ID because
    /// only parent IDs exist in task_runs (required by foreign key constraint).
    pub(crate) fn log_step_event(
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
            "node_id": step.id,
            "node_kind": step.effective_node_kind().as_str(),
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

        // PG-primary: fire-and-forget async write to PostgreSQL
        {
            let pg = self.app_state.pg_db.clone();
            let event_clone = event_input.clone();
            tokio::spawn(async move {
                if let Err(e) = pg.create_task_run_event(&event_clone).await {
                    tracing::warn!("PG event write failed: {}", e);
                }
            });
        }
        // SQLite step event logging removed — all persistence now via PgDb.
    }

    /// Emit a tree event to the Tauri frontend (if app_handle is available)
    pub(crate) fn emit_tree_event(
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
    pub(crate) async fn record_screenshot_event(
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

            // Link any findings detected during this step's execution window
            if let Some(ref task_run_id) = self.task_run_id {
                let sn_c = step_name.clone();
                let idx_c = index as i32;
                match self
                    .app_state
                    .pg_db
                    .link_findings_to_steps_by_timestamp(
                        task_run_id,
                        &sn_c,
                        idx_c,
                        &started_at,
                        &ended_at,
                    )
                    .await
                {
                    Ok(count) if count > 0 => {
                        info!(
                            "Linked {} findings to step '{}' (index {})",
                            count, sn_c, idx_c
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("Failed to link findings to step '{}': {}", sn_c, e);
                    }
                }
            }

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

            // ================================================================
            // Breakpoint: pause execution after this step if annotated
            // ================================================================
            if step.breakpoint.unwrap_or(false) && success {
                let bp_step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());
                info!(
                    "Breakpoint hit after step {}/{}: {} — pausing execution",
                    index + 1,
                    steps.len(),
                    bp_step_name
                );

                // Serialize runtime context and remaining steps for the snapshot
                let variables_json = serde_json::to_string(&json!({
                    "runtime_context": serde_json::to_value(&self.runtime_context).unwrap_or(json!(null)),
                    "shared_variables": self.shared_variables.get_all(),
                }))
                .unwrap_or_else(|_| "{}".to_string());

                let pending_steps_json = if index + 1 < steps.len() {
                    serde_json::to_string(&steps[index + 1..]).unwrap_or_else(|_| "[]".to_string())
                } else {
                    "[]".to_string()
                };

                let snapshot = BreakpointManager::build_snapshot(
                    execution_id,
                    index,
                    step.name.clone(),
                    step.phase.clone(),
                    None, // iteration set by caller if needed
                    variables_json,
                    results.last().and_then(|r| r.screenshot_path.clone()),
                    pending_steps_json,
                );

                let bp_manager = BreakpointManager::new(self.app_state.clone());

                // Log breakpoint event
                self.log_step_event(
                    &log_task_run_id,
                    step,
                    index,
                    "breakpoint_hit",
                    &format!(
                        "Breakpoint hit after step {}/{}: {} — waiting for resume",
                        index + 1,
                        steps.len(),
                        bp_step_name
                    ),
                    None,
                    None,
                    None,
                    None,
                    None,
                );

                // Save snapshot and wait for resume
                match bp_manager.save_snapshot(&snapshot).await {
                    Ok(_) => {
                        info!(
                            "Breakpoint snapshot {} saved, waiting for resume",
                            snapshot.id
                        );

                        // Set task status to paused
                        let parent_id = get_parent_task_id(execution_id);
                        let _ = self
                            .app_state
                            .pg_db
                            .update_task_run_status(&parent_id, "paused")
                            .await;

                        // Block until resumed
                        if let Err(e) = bp_manager.wait_for_resume(&snapshot.id, execution_id).await
                        {
                            warn!("Breakpoint wait error: {} — continuing execution", e);
                        }

                        // Check staleness and warn
                        match bp_manager.check_staleness(&snapshot) {
                            StalenessCheck::Stale(age) => {
                                let stale_msg = format!(
                                    "Breakpoint snapshot is stale ({} minutes old) — state may have changed",
                                    age.num_minutes()
                                );
                                warn!("{}", stale_msg);
                                self.log_step_event(
                                    &log_task_run_id,
                                    step,
                                    index,
                                    "breakpoint_stale",
                                    &stale_msg,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                );
                            }
                            StalenessCheck::Fresh => {}
                        }

                        // Restore task status to running
                        let _ = self
                            .app_state
                            .pg_db
                            .update_task_run_status(&parent_id, "running")
                            .await;

                        info!("Breakpoint {} resumed — continuing execution", snapshot.id);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to save breakpoint snapshot: {} — continuing without pause",
                            e
                        );
                    }
                }
            }
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
    pub(crate) fn get_dev_logs_dir() -> PathBuf {
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

    /// Run the handler registered under `key`.
    async fn run_registered_handler(
        &self,
        key: &str,
        step: &ExecutionStepConfig,
    ) -> (
        bool,
        Option<String>,
        Option<String>,
        Option<serde_json::Value>,
    ) {
        let Some(handler) = self.handler_registry.get(key) else {
            // Unreachable while the parity test holds: every route to here
            // carries a key the registry serves.
            return (
                false,
                Some(format!(
                    "No handler registered for step type {key:?} (from {:?})",
                    step.step_type
                )),
                None,
                None,
            );
        };
        let context = self.create_handler_context().await;
        let result = handler.execute(step, &context).await;
        (
            result.success,
            result.error,
            result.screenshot_path,
            result.output_data,
        )
    }

    /// Execute a single step and return (success, error, screenshot_path, output_data)
    pub(crate) async fn execute_single_step(
        &self,
        step: &ExecutionStepConfig,
    ) -> (
        bool,
        Option<String>,
        Option<String>,
        Option<serde_json::Value>,
    ) {
        // ── Blueprint deterministic guarantee (Phase 2) ──────────────────────────
        // execute_single_step routes every step through the handler registry /
        // deterministic fallback match; it NEVER constructs an AiSessionConfig
        // (prompt steps are no-ops here — the AI session is spawned by the
        // unified workflow executor, not this boundary). Make that an explicit,
        // observable invariant rather than an accident.
        let node_kind = step.effective_node_kind();
        tracing::debug!(
            node_id = ?step.id,
            step_type = %step.step_type,
            ?node_kind,
            "execute_single_step dispatch (deterministic boundary — no AI session is created here)"
        );
        debug_assert!(
            step.step_type != "prompt"
                || node_kind == crate::workflow::dag_schema::NodeKind::Agentic,
            "a step classified Deterministic reached execute_single_step with step_type=prompt \
             (node_id={:?}); the deterministic guarantee would be violated if this path ever \
             spawned an AI session",
            step.id
        );

        // Typed dispatch boundary. `resolve_dispatch` decides the route: the
        // registry for a typed step (or, with a warning, for a registered
        // type whose typed parse failed), the legacy `match` below for a
        // `LEGACY_STRING_DISPATCH` type, and a step failure for a type nothing
        // serves.
        let legacy = match resolve_dispatch(step, &self.handler_registry) {
            DispatchRoute::Registry(key) => return self.run_registered_handler(key, step).await,
            DispatchRoute::RegistryFallback { key, parse_error } => {
                warn!(
                    "Step type {:?} is registry-served but failed the typed parse ({}); \
                     dispatching to its handler by the raw step type",
                    key, parse_error
                );
                return self.run_registered_handler(&key, step).await;
            }
            DispatchRoute::Unknown => {
                warn!("Unknown step type: {}", step.step_type);
                return (
                    false,
                    Some(format!("Unknown step type: {}", step.step_type)),
                    None,
                    None,
                );
            }
            DispatchRoute::ConversionFailed(error) => return conversion_failure_outcome(error),
            DispatchRoute::Legacy(legacy) => legacy,
        };

        // Legacy string-dispatched step types (no registered handler).

        // Timeouts are disabled by default - only apply if explicitly specified
        let timeout = step.timeout_seconds;

        match legacy {
            // ================================================================
            // Shell Command Step Type
            // ================================================================
            LegacyStep::ShellCommand => {
                let (s, e, p) = self.execute_shell_command_step(step, timeout).await;
                (s, e, p, None)
            }
            // ================================================================
            // Check Step Type (code quality checks)
            // ================================================================
            LegacyStep::Check => {
                let (s, e, p) = self.execute_check_step(step, timeout).await;
                (s, e, p, None)
            }
            // ================================================================
            // Check Group Step Type (run all checks in a group)
            // ================================================================
            LegacyStep::CheckGroup => {
                let (success, error, summary, _check_results) =
                    self.execute_check_group_step(step, timeout).await;
                (success, error, summary, None)
            }
            // ================================================================
            // Shell Step Type (execute shell command)
            // ================================================================
            LegacyStep::Shell => {
                // Timeouts are disabled by default
                let timeout = step.timeout_seconds;
                let (success, error, output) = self.execute_shell_command_step(step, timeout).await;
                // Return output as the third element for potential logging
                (success, error, output, None)
            }
            // ================================================================
            // Log Watch Step Type (scan dev logs for errors)
            // ================================================================
            LegacyStep::LogWatch => {
                let (success, error, output) = self.execute_log_watch_step(step).await;
                (success, error, output, None)
            }
            // ================================================================
            // Gate Step Type (aggregate verification results)
            // ================================================================
            LegacyStep::Gate => {
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_executor::dag::compute_execution_layers;

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

    // ========================================================================
    // Session 2a: typed dispatch boundary tests
    // ========================================================================

    /// One default instance of every `FullRunnerStep` variant, with the
    /// handler-registry key it must map to.
    fn all_variant_cases() -> Vec<(&'static str, qontinui_types::workflow_step::FullRunnerStep)> {
        use qontinui_types::workflow_step::*;
        vec![
            ("command", FullRunnerStep::Command(CommandStep::default())),
            ("prompt", FullRunnerStep::Prompt(PromptStep::default())),
            (
                "ui_bridge",
                FullRunnerStep::UiBridge(UiBridgeStep::default()),
            ),
            (
                "workflow",
                FullRunnerStep::Workflow(WorkflowStep::default()),
            ),
            (
                "code_execution",
                FullRunnerStep::CodeExecution(CodeExecutionStep::default()),
            ),
            (
                "execute_playbook",
                FullRunnerStep::ExecutePlaybook(ExecutePlaybookStep::default()),
            ),
            (
                "native_accessibility",
                FullRunnerStep::NativeAccessibility(NativeAccessibilityStep::default()),
            ),
            (
                "restart_process",
                FullRunnerStep::RestartProcess(RestartProcessStep::default()),
            ),
            (
                "save_workflow_artifact",
                FullRunnerStep::SaveWorkflowArtifact(SaveWorkflowArtifactStep::default()),
            ),
            (
                "workflow_fixup",
                FullRunnerStep::WorkflowFixup(WorkflowFixupStep::default()),
            ),
            (
                "ui_bridge_design_audit",
                FullRunnerStep::UiBridgeDesignAudit(UiBridgeDesignAuditStep::default()),
            ),
            (
                "ui_bridge_visual_assertion",
                FullRunnerStep::UiBridgeVisualAssertion(UiBridgeVisualAssertionStep::default()),
            ),
            (
                "workflow_ref",
                FullRunnerStep::WorkflowRef(WorkflowRefStep::default()),
            ),
            (
                "dag_cancel",
                FullRunnerStep::DagCancel(DagCancelStep::default()),
            ),
            (
                "dag_approval",
                FullRunnerStep::DagApproval(DagApprovalStep::default()),
            ),
            ("dag_loop", FullRunnerStep::DagLoop(DagLoopStep::default())),
            (
                "vga_automate",
                FullRunnerStep::VgaAutomate(VgaAutomateStep::default()),
            ),
            (
                "spec_check",
                FullRunnerStep::SpecCheck(SpecCheckStep::default()),
            ),
            (
                "wrapper_action",
                FullRunnerStep::WrapperAction(WrapperActionStep::default()),
            ),
            (
                "effect_check",
                FullRunnerStep::EffectCheck(EffectCheckStep::default()),
            ),
        ]
    }

    /// `handler_lookup_key` must return the right string for every one of the
    /// 20 `FullRunnerStep` variants.
    ///
    /// This slice is a RUNTIME check of each arm's string. The compile-time
    /// coverage check is the wildcard-free `match` in `handler_lookup_key`
    /// itself: a new variant without an arm fails to compile there. The slice
    /// has to be extended by hand, which is why the count is asserted.
    #[test]
    fn test_handler_lookup_key_all_variants() {
        let cases = all_variant_cases();

        // Exactly 20 variants — make sure we haven't accidentally skipped one.
        assert_eq!(
            cases.len(),
            20,
            "expected exactly 20 FullRunnerStep variants"
        );
        // ...and every one of them is a distinct key the registry serves.
        let registry = crate::step_executor::handlers::HandlerRegistry::with_standard_handlers();
        let keys: std::collections::BTreeSet<&str> = cases.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys.len(), cases.len(), "duplicate lookup key in cases");
        for key in &keys {
            assert!(
                registry.get(key).is_some(),
                "handler_lookup_key yields {key:?}, which no registered handler serves"
            );
        }

        for (expected_key, variant) in &cases {
            let actual = handler_lookup_key(variant);
            assert_eq!(
                actual, *expected_key,
                "handler_lookup_key returned {:?} for variant that should map to {:?}",
                actual, expected_key
            );
        }
    }

    /// Round-trip: `ExecutionStepConfig` → `to_full_runner_step` → `handler_lookup_key`
    /// must yield the expected handler string for a representative set of types.
    ///
    /// The test builds `ExecutionStepConfig` values by deserializing from
    /// carefully crafted JSON.  Only step types whose field names align between
    /// `ExecutionStepConfig` and the target `FullRunnerStep` inner struct are
    /// included here.
    ///
    /// `ui_bridge` and `workflow` also round-trip because Session 2c added
    /// direct Rust-field constructors that bypass JSON field-name conflicts
    /// on the fat `ExecutionStepConfig` struct (`action` / `target` /
    /// `workflow_name` are shared across variants).
    ///
    /// Variants not covered below are tested indirectly via
    /// `test_handler_lookup_key_all_variants`; add an explicit round-trip
    /// case here only when a variant has non-trivial field mapping to
    /// verify.
    #[test]
    fn test_to_full_runner_step_round_trip() {
        // Each entry: (label, minimal JSON, expected handler key).
        let cases: &[(&str, serde_json::Value, &str)] = &[
            (
                "command",
                json!({"type": "command", "id": "s1", "name": "build", "phase": "setup"}),
                "command",
            ),
            (
                "prompt",
                json!({"type": "prompt", "id": "s2", "name": "ask", "phase": "agentic", "content": "do the thing"}),
                "prompt",
            ),
            (
                "ui_bridge",
                json!({
                    "type": "ui_bridge",
                    "id": "s5",
                    "name": "click",
                    "phase": "verification",
                    "ui_bridge_action": "execute",
                    "ui_bridge_target": "#submit",
                    "ui_bridge_instruction": "click submit"
                }),
                "ui_bridge",
            ),
            (
                "workflow",
                json!({
                    "type": "workflow",
                    "id": "s6",
                    "name": "run child",
                    "phase": "setup",
                    "ref_workflow_id": "wf-123"
                }),
                "workflow",
            ),
            (
                "dag_cancel",
                json!({"type": "dag_cancel", "id": "s3", "name": "stop"}),
                "dag_cancel",
            ),
            (
                "dag_approval",
                json!({"type": "dag_approval", "id": "s4", "name": "wait"}),
                "dag_approval",
            ),
        ];

        for (label, raw_json, expected_key) in cases {
            let step: ExecutionStepConfig = serde_json::from_value(raw_json.clone())
                .unwrap_or_else(|e| {
                    panic!(
                        "failed to deserialize ExecutionStepConfig for {:?}: {}",
                        label, e
                    )
                });
            let typed = to_full_runner_step(&step)
                .unwrap_or_else(|e| panic!("to_full_runner_step failed for {:?}: {}", label, e));
            let key = handler_lookup_key(&typed);
            assert_eq!(
                key, *expected_key,
                "round-trip key mismatch for step type {:?}",
                label
            );
        }
    }

    /// Unknown step types must return an `Err` from `to_full_runner_step`;
    /// `resolve_dispatch` then decides between the legacy match, a refusal and
    /// `Unknown step type`.
    #[test]
    fn test_to_full_runner_step_unknown_returns_err() {
        let step = ExecutionStepConfig {
            step_type: "totally_unknown_custom_type".to_string(),
            ..Default::default()
        };
        assert!(
            to_full_runner_step(&step).is_err(),
            "expected Err for unknown step type, but got Ok"
        );
    }

    /// The "test" step type must NOT be passed to `to_full_runner_step`; the
    /// dispatch code normalizes it to "command" at the string layer before any
    /// typed parse attempt.  Verify that "test" on its own does fail the typed
    /// parse (confirming it has no FullRunnerStep variant).
    #[test]
    fn test_legacy_test_type_not_in_full_runner_step() {
        let step = ExecutionStepConfig {
            step_type: "test".to_string(),
            ..Default::default()
        };
        // "test" is intentionally absent from FullRunnerStep; the dispatch
        // code handles it before calling to_full_runner_step.
        assert!(
            to_full_runner_step(&step).is_err(),
            "\"test\" should not be a FullRunnerStep variant; it is handled at the string layer"
        );
    }

    // ========================================================================
    // Phase 3: explicit routes and registry <-> enum parity
    // ========================================================================

    use crate::step_executor::handlers::HandlerRegistry;

    fn minimal_step(step_type: &str) -> ExecutionStepConfig {
        ExecutionStepConfig {
            step_type: step_type.to_string(),
            id: Some("a".to_string()),
            name: Some("b".to_string()),
            ..Default::default()
        }
    }

    /// Every type the handler registry serves is produced by
    /// `handler_lookup_key` for some `FullRunnerStep` variant, and vice versa.
    /// A handler registered without a variant can only be reached through the
    /// untyped fallback — exactly the gap this pins shut.
    ///
    /// Explicitly excluded: the handler modules `check.rs`, `check_group.rs`,
    /// `shell_command.rs` and `test.rs`. They implement `StepHandler` and
    /// declare a `step_type()`, but `with_standard_handlers` does NOT register
    /// them — they are internal to `CommandHandler`, and their step types are
    /// served by the legacy `match` (or, for `test`, normalised to `command`).
    /// So they have no variant on purpose; the assertion below keeps them out
    /// of the registry, where they would need one.
    #[test]
    fn registry_and_full_runner_step_are_in_parity() {
        const UNREGISTERED_INTERNAL_HANDLERS: &[&str] =
            &["check", "check_group", "shell_command", "test"];

        let registry = HandlerRegistry::with_standard_handlers();
        let variant_keys: std::collections::BTreeSet<&str> = all_variant_cases()
            .iter()
            .map(|(_, v)| handler_lookup_key(v))
            .collect();
        let registered: std::collections::BTreeSet<&str> =
            registry.step_types().into_iter().collect();

        let without_variant: Vec<_> = registered.difference(&variant_keys).collect();
        assert!(
            without_variant.is_empty(),
            "registered handler(s) with no FullRunnerStep variant: {without_variant:?}"
        );
        let without_handler: Vec<_> = variant_keys.difference(&registered).collect();
        assert!(
            without_handler.is_empty(),
            "FullRunnerStep variant key(s) with no registered handler: {without_handler:?}"
        );
        for ty in UNREGISTERED_INTERNAL_HANDLERS {
            assert!(
                registry.get(ty).is_none(),
                "{ty:?} is an internal CommandHandler module; registering it needs a variant"
            );
        }
    }

    /// Every `LEGACY_STRING_DISPATCH` entry has an arm in the legacy match: a
    /// minimal step of each routes to `Legacy`, never to the `Unknown step
    /// type` failure. (`StepExecutor` needs a live `AppState`, so this drives
    /// `resolve_dispatch`, whose `Legacy` payload the legacy match is
    /// exhaustive over.)
    #[test]
    fn every_legacy_dispatch_entry_has_a_legacy_arm() {
        let registry = HandlerRegistry::with_standard_handlers();
        for ty in LEGACY_STRING_DISPATCH {
            let route = resolve_dispatch(&minimal_step(ty), &registry);
            assert_ne!(route, DispatchRoute::Unknown, "{ty:?}: Unknown step type");
            assert_eq!(
                route,
                DispatchRoute::Legacy(LegacyStep::from_type(ty).unwrap()),
                "{ty:?}"
            );
            assert!(
                registry.get(ty).is_none(),
                "{ty:?} is registered; it should not also be legacy"
            );
        }
    }

    /// The legacy types, spelled independently of `LEGACY_STRING_DISPATCH` so
    /// that dropping an entry from the const fails here rather than silently
    /// shrinking the check.
    #[test]
    fn legacy_types_route_to_the_legacy_match() {
        const EXPECTED: &[&str] = &[
            "shell_command",
            "check",
            "check_group",
            "shell",
            "log_watch",
            "gate",
        ];
        let registry = HandlerRegistry::with_standard_handlers();
        for ty in EXPECTED {
            assert!(
                matches!(
                    resolve_dispatch(&minimal_step(ty), &registry),
                    DispatchRoute::Legacy(_)
                ),
                "{ty:?} must route to the legacy match"
            );
        }
        assert_eq!(LEGACY_STRING_DISPATCH.len(), EXPECTED.len());
    }

    /// Assert `step` takes the registry fallback to `key`, and return the
    /// carried parse error.
    fn assert_fallback(step: &ExecutionStepConfig, key: &str) -> String {
        let registry = HandlerRegistry::with_standard_handlers();
        match resolve_dispatch(step, &registry) {
            DispatchRoute::RegistryFallback {
                key: got,
                parse_error,
            } => {
                assert_eq!(got, key);
                assert!(
                    parse_error.contains("failed to parse step as FullRunnerStep"),
                    "{parse_error}"
                );
                parse_error
            }
            other => panic!("expected RegistryFallback to {key:?}, got {other:?}"),
        }
    }

    /// A type the registry serves whose typed parse fails still runs on its
    /// handler (by the raw step type), carrying the serde message for the
    /// warning — never the legacy match, never `Unknown`.
    #[test]
    fn registered_type_failing_the_parse_falls_back_to_its_handler_with_the_serde_message() {
        let step: ExecutionStepConfig = serde_json::from_value(json!({
            "type": "workflow_fixup", "id": "a", "name": "b", "fixupMode": "zzz"
        }))
        .unwrap();
        let msg = assert_fallback(&step, "workflow_fixup");
        assert!(
            msg.contains("zzz"),
            "serde message names the bad value: {msg}"
        );
    }

    /// The phase stamping `refetch_unified_workflow_steps` applies
    /// (mcp/unified_workflows.rs:204-231): every step is converted with
    /// `serde_json::from_value::<ExecutionStepConfig>` (:105) and then gets
    /// `phase` set from the array it sits in, so a command step placed in
    /// `agentic_steps` is stamped `"agentic"` (:219-223). The function itself
    /// is not callable here (it reads the workflow from Postgres), so this
    /// replicates the loop over the real `normalize_to_stages`.
    fn refetch_stamped_steps(
        workflow: &crate::unified_workflows::UnifiedWorkflow,
    ) -> Vec<ExecutionStepConfig> {
        use crate::unified_workflows::UnifiedWorkflowExt;
        let mut out = Vec::new();
        for stage in &workflow.normalize_to_stages() {
            for (phase, steps) in [
                ("setup", &stage.setup_steps),
                ("verification", &stage.verification_steps),
                ("agentic", &stage.agentic_steps),
                ("completion", &stage.completion_steps),
            ] {
                for step in steps {
                    let mut config: ExecutionStepConfig =
                        serde_json::from_value(step.clone()).unwrap();
                    config.phase = Some(phase.to_string());
                    out.push(config);
                }
            }
        }
        out
    }

    /// A command step in `agentic_steps` is stamped `phase: "agentic"`, which
    /// `CommandStepPhase` has no variant for. The verification path executes
    /// agentic-phase steps (`execute_verification_steps_with_events` keeps
    /// `verification` and `agentic`), and `CommandHandler` ran them before the
    /// typed dispatch existed — so the route must be the command handler.
    #[test]
    fn refetch_agentic_stamped_command_step_falls_back_to_the_command_handler() {
        let workflow: crate::unified_workflows::UnifiedWorkflow = serde_json::from_value(json!({
            "id": "w",
            "name": "w",
            "agenticSteps": [
                {"type": "command", "id": "c1", "name": "run tests", "command": "cargo test"}
            ]
        }))
        .unwrap();
        let steps = refetch_stamped_steps(&workflow);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].phase.as_deref(), Some("agentic"));
        let msg = assert_fallback(&steps[0], "command");
        assert!(msg.contains("agentic"), "{msg}");
    }

    /// An out-of-enum `check_type` on a command step fails the typed parse
    /// (`CheckType` is closed) but `CommandHandler` reads the raw string.
    #[test]
    fn command_step_with_out_of_enum_check_type_falls_back_to_the_command_handler() {
        let step: ExecutionStepConfig = serde_json::from_value(json!({
            "type": "command", "id": "a", "name": "b", "phase": "verification",
            "check_type": "not_a_real_check_type", "command": "true"
        }))
        .unwrap();
        let msg = assert_fallback(&step, "command");
        assert!(msg.contains("not_a_real_check_type"), "{msg}");
    }

    #[test]
    fn unknown_type_routes_to_unknown_and_test_to_command() {
        let registry = HandlerRegistry::with_standard_handlers();
        assert_eq!(
            resolve_dispatch(&minimal_step("totally_unknown_custom_type"), &registry),
            DispatchRoute::Unknown
        );
        assert_eq!(
            resolve_dispatch(&minimal_step("test"), &registry),
            DispatchRoute::Registry("command")
        );
        let mut prompt = minimal_step("prompt");
        prompt.prompt_content = Some(String::new());
        assert_eq!(
            resolve_dispatch(&prompt, &registry),
            DispatchRoute::Registry("prompt")
        );
    }

    /// A prompt step with NO content (not even `""`) fails the typed parse —
    /// `PromptStep.content` is a required `String` — but `PromptStepHandler`
    /// passes an absent body through as success, so the step still runs.
    #[test]
    fn contentless_prompt_step_falls_back_to_the_prompt_handler() {
        assert_fallback(&minimal_step("prompt"), "prompt");
    }
}

#[cfg(test)]
#[path = "typed_dispatch_corpus_tests.rs"]
mod typed_dispatch_corpus_tests;
