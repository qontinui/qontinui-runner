//! Step conversion utilities for transforming JSON step definitions into ExecutionStepConfig.
//!
//! This module contains standalone functions for:
//! - Variable substitution in step templates (`{{artifact_dir}}`, `{{execution_id}}`, `{{iteration}}`)
//! - Converting JSON Value arrays to typed `ExecutionStepConfig` vectors
//! - Extracting prompt-type steps separately from automation steps
//! - Phase assignment for steps originating from different workflow arrays

use crate::step_executor::{ExecutionStepConfig, StepPhase};

/// Variables available for substitution in step fields.
pub struct SubstitutionVars {
    pub artifact_dir: Option<String>,
    pub execution_id: String,
    pub iteration: u32,
}

/// Apply variable substitution to a JSON step value.
///
/// Replaces template variables in all string values within the JSON:
/// - `{{artifact_dir}}` -> artifact directory path (forward slashes)
/// - `{{execution_id}}` -> the task run ID
/// - `{{iteration}}` -> current iteration number
pub fn apply_variable_substitution(
    step: &serde_json::Value,
    vars: &SubstitutionVars,
) -> serde_json::Value {
    let mut json_str = serde_json::to_string(step).unwrap_or_default();

    if let Some(ref artifact_dir) = vars.artifact_dir {
        // Use forward slashes on all platforms for consistency
        let normalized = artifact_dir.replace('\\', "/");
        json_str = json_str.replace("{{artifact_dir}}", &normalized);
    }
    json_str = json_str.replace("{{execution_id}}", &vars.execution_id);
    json_str = json_str.replace("{{iteration}}", &vars.iteration.to_string());

    serde_json::from_str(&json_str).unwrap_or_else(|_| step.clone())
}

/// Apply variable substitution to a slice of JSON step values.
pub fn apply_substitution_to_steps(
    steps: &[serde_json::Value],
    vars: &SubstitutionVars,
) -> Vec<serde_json::Value> {
    steps
        .iter()
        .map(|s| apply_variable_substitution(s, vars))
        .collect()
}

/// Apply variable substitution to an ExecutionStepConfig's string fields.
///
/// Replaces `{{artifact_dir}}` and `{{execution_id}}` in all relevant
/// Option<String> fields. This is called after the artifact directory
/// is created but before steps are executed.
pub fn substitute_step_vars(
    step: &mut ExecutionStepConfig,
    artifact_dir: &str,
    execution_id: &str,
) {
    let sub = |s: &mut Option<String>| {
        if let Some(val) = s {
            if val.contains("{{artifact_dir}}") || val.contains("{{execution_id}}") {
                *val = val
                    .replace("{{artifact_dir}}", artifact_dir)
                    .replace("{{execution_id}}", execution_id);
            }
        }
    };

    sub(&mut step.output_path);
    sub(&mut step.input_path);
    sub(&mut step.ai_review_input_path);
    sub(&mut step.shell_command);
    sub(&mut step.shell_command_working_directory);
    sub(&mut step.check_command);
    sub(&mut step.check_working_directory);
    sub(&mut step.artifact_input_path);
    sub(&mut step.fixup_input_path);
    sub(&mut step.fixup_criteria_path);

    // Also substitute in prompt content (may reference artifact paths)
    if let Some(ref mut content) = step.prompt_content {
        if content.contains("{{artifact_dir}}") || content.contains("{{execution_id}}") {
            *content = content
                .replace("{{artifact_dir}}", artifact_dir)
                .replace("{{execution_id}}", execution_id);
        }
    }
}

pub fn convert_json_steps_to_execution_steps(
    steps: &[serde_json::Value],
    monitor: i32,
) -> Vec<ExecutionStepConfig> {
    convert_json_steps_with_phase(steps, monitor, None)
}

/// Convert JSON Value steps to ExecutionStepConfig with explicit phase.
///
/// Sets the explicit phase on all steps that don't already have one.
/// This is the preferred function for unified workflow execution.
pub fn convert_json_steps_with_phase(
    steps: &[serde_json::Value],
    _monitor: i32,
    explicit_phase: Option<&str>,
) -> Vec<ExecutionStepConfig> {
    steps
        .iter()
        // Filter out prompt steps - they're handled separately to avoid duplicate logging
        .filter(|step| {
            !matches!(
                value_step_type(step),
                Some("prompt" | "ai_session" | "ai_prompt" | "run_prompt_sequence")
            )
        })
        .filter_map(|step| convert_step_value(step, explicit_phase))
        .collect()
}

/// Convert ALL JSON steps (including prompt-type) to ExecutionStepConfig with explicit phase.
///
/// Unlike `convert_json_steps_with_phase` which filters out prompt steps,
/// this function preserves all step types in their original order.
/// This is needed for the verification phase where prompt-type steps
/// (AI-evaluated checks) must be included alongside automation steps.
pub fn convert_all_json_steps_with_phase(
    steps: &[serde_json::Value],
    _monitor: i32,
    explicit_phase: Option<&str>,
) -> Vec<ExecutionStepConfig> {
    steps
        .iter()
        .filter_map(|step| convert_step_value(step, explicit_phase))
        .collect()
}

/// One step of [`convert_json_steps_with_phase`] /
/// [`convert_all_json_steps_with_phase`]: normalize, parse, and on a parse
/// failure fall back to a hand-built step — except for `ui_bridge`
/// ([`fallback_refused`]), which becomes a [`failing_step`] so the run fails
/// on it visibly instead of losing it.
fn convert_step_value(
    step: &serde_json::Value,
    explicit_phase: Option<&str>,
) -> Option<ExecutionStepConfig> {
    let mut config = match parse_step_value(step) {
        Ok(config) => config,
        Err(e) if fallback_refused(step) => {
            let err = StepConversionError::new(step, &e);
            tracing::error!("{err}; the step will fail at execution");
            failing_step(step, &err)
        }
        Err(_) => {
            // Fall back to manual field extraction — preserve command, working directory,
            // and other key fields so that check/test steps with inline commands still work
            let step_type = value_step_type(step)?;
            let get = |key: &str| step.get(key).and_then(|v| v.as_str()).map(str::to_string);
            ExecutionStepConfig {
                step_type: step_type.to_string(),
                name: get("name"),
                id: get("id"),
                shell_command: get("command"),
                shell_command_working_directory: get("working_directory"),
                check_type: get("check_type"),
                test_type: get("test_type"),
                test_id: get("test_id"),
                ..Default::default()
            }
        }
    };

    // Set explicit phase if not already set
    if config.phase.is_none() {
        if let Some(phase_str) = explicit_phase {
            if let Some(phase) = StepPhase::from_str_opt(phase_str) {
                config.set_phase(phase);
            }
        }
    }

    Some(config)
}

// ============================================================================
// Canonical-key normalization
// ============================================================================

/// The step type a JSON step declares (`type`, or the generator's `step_type`).
pub(crate) fn value_step_type(step: &serde_json::Value) -> Option<&str> {
    step.get("type")
        .or_else(|| step.get("step_type"))
        .and_then(|t| t.as_str())
}

/// Canonical `UiBridgeStep` keys (qontinui-schemas `workflow_step.rs`), in
/// every spelling a producer writes, and the `ExecutionStepConfig` field each
/// one feeds. `(canonical spellings, field, prefixed spellings that win)`.
///
/// The bare keys are the shared contract: both step editors, the Builder
/// skill templates and the typed `UiBridgeStep` use them. `ExecutionStepConfig`
/// reads `ui_bridge_*` instead, and aliases bare `action` / `target` to the
/// `native_accessibility` fields and `timeoutMs` to `vga_timeout_ms`, so an
/// un-normalized Builder step reaches the handler with no action at all.
const UI_BRIDGE_CANONICAL_KEYS: &[(&[&str], &str, &[&str])] = &[
    (&["action"], "ui_bridge_action", &["uiBridgeAction"]),
    (&["url"], "ui_bridge_url", &["uiBridgeUrl"]),
    (&["target"], "ui_bridge_target", &["uiBridgeTarget"]),
    (
        &["instruction"],
        "ui_bridge_instruction",
        &["uiBridgeInstruction"],
    ),
    (
        &["assert_type", "assertType"],
        "ui_bridge_assert_type",
        &["uiBridgeAssertType"],
    ),
    (&["expected"], "ui_bridge_expected", &["uiBridgeExpected"]),
    (
        &["comparison_mode", "comparisonMode"],
        "ui_bridge_compare_mode",
        &["uiBridgeCompareMode"],
    ),
    (
        &["reference_snapshot_id", "referenceSnapshotId"],
        "ui_bridge_reference_snapshot_id",
        &["uiBridgeReferenceSnapshotId"],
    ),
    (
        &["severity_threshold", "severityThreshold"],
        "ui_bridge_severity_threshold",
        &["uiBridgeSeverityThreshold"],
    ),
    (
        &["timeout_ms", "timeoutMs"],
        "ui_bridge_timeout_ms",
        &["uiBridgeTimeoutMs"],
    ),
    (
        &["action_plan", "actionPlan"],
        "ui_bridge_action_plan",
        &["uiBridgeActionPlan"],
    ),
];

/// `ExecutionStepConfig` fields typed `Option<String>`, in both prefixed
/// spellings (snake field name and camelCase alias), whose value
/// may arrive as structured JSON (a criteria object in `target`, a number in
/// `expected`); those are carried as their JSON text, which is what the
/// handler parses back.
const UI_BRIDGE_STRING_FIELDS: &[&str] = &[
    "ui_bridge_action",
    "uiBridgeAction",
    "ui_bridge_url",
    "uiBridgeUrl",
    "ui_bridge_target",
    "uiBridgeTarget",
    "ui_bridge_instruction",
    "uiBridgeInstruction",
    "ui_bridge_assert_type",
    "uiBridgeAssertType",
    "ui_bridge_expected",
    "uiBridgeExpected",
    "ui_bridge_compare_mode",
    "uiBridgeCompareMode",
    "ui_bridge_reference_snapshot_id",
    "uiBridgeReferenceSnapshotId",
    "ui_bridge_severity_threshold",
    "uiBridgeSeverityThreshold",
    "ui_bridge_snapshot_target",
    "uiBridgeSnapshotTarget",
];

/// Rewrite a step's canonical keys onto the `ExecutionStepConfig` field names,
/// in place. Runs before EVERY `from_value::<ExecutionStepConfig>` — use
/// [`parse_step_value`] / [`parse_steps_json`] rather than calling serde
/// directly.
///
/// Only `ui_bridge` steps are rewritten. Each canonical key (snake or camel)
/// is moved onto its `ui_bridge_*` field when that field is absent or `null`
/// in both its snake and camel spellings — a prefixed key already present
/// wins — and the canonical key is REMOVED either way, so bare `action` /
/// `target` cannot also populate `a11y_*` and `timeoutMs` cannot populate
/// `vga_timeout_ms`. Idempotent: a serialized `ExecutionStepConfig` carries no
/// canonical keys, so it passes through unchanged.
pub fn normalize_step_value(step: &mut serde_json::Value) {
    if value_step_type(step) != Some("ui_bridge") {
        return;
    }
    let Some(obj) = step.as_object_mut() else {
        return;
    };
    let present = |obj: &serde_json::Map<String, serde_json::Value>, k: &str| {
        obj.get(k).is_some_and(|v| !v.is_null())
    };
    for (canonical, field, prefixed) in UI_BRIDGE_CANONICAL_KEYS {
        // First non-null canonical spelling wins; every spelling is removed.
        let mut value = None;
        for key in *canonical {
            if let Some(v) = obj.remove(*key) {
                if value.is_none() && !v.is_null() {
                    value = Some(v);
                }
            }
        }
        let Some(value) = value else { continue };
        let already = present(obj, field) || prefixed.iter().any(|k| present(obj, k));
        if !already {
            // A `null` prefixed spelling would otherwise sit beside the
            // inserted field and serde would refuse the pair as a duplicate.
            obj.remove(*field);
            for k in *prefixed {
                obj.remove(*k);
            }
            obj.insert((*field).to_string(), value);
        }
    }
    for field in UI_BRIDGE_STRING_FIELDS {
        if let Some(v) = obj.get_mut(*field) {
            if !(v.is_string() || v.is_null()) {
                *v = serde_json::Value::String(v.to_string());
            }
        }
    }
}

/// Normalize a copy of `step` ([`normalize_step_value`]) and parse it.
pub fn parse_step_value(
    step: &serde_json::Value,
) -> Result<ExecutionStepConfig, serde_json::Error> {
    let mut step = step.clone();
    normalize_step_value(&mut step);
    serde_json::from_value(step)
}

/// Why [`parse_steps_json`] refused a steps array.
#[derive(Debug)]
pub enum StepsJsonError {
    /// Not a JSON array of steps, or a step whose type has a hand-built
    /// fallback failed the parse (the pre-normalization behavior).
    Malformed(serde_json::Error),
    /// A step with no faithful fallback ([`fallback_refused`]) failed the
    /// parse. Callers surface this as the run's failure.
    UnparseableStep(StepConversionError),
}

impl std::fmt::Display for StepsJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "{e}"),
            Self::UnparseableStep(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StepsJsonError {}

/// Parse a JSON array of steps (`execution_steps_json`, a durable batch),
/// normalizing each ([`normalize_step_value`]). A `ui_bridge` step that
/// still fails is reported as [`StepsJsonError::UnparseableStep`], naming it.
///
/// The whole array is scanned: an unparseable `ui_bridge` step OUTRANKS a
/// [`StepsJsonError::Malformed`] step anywhere in it, so a malformed step
/// earlier in the array (which callers may treat as "fall back") cannot hide
/// it.
pub fn parse_steps_json(json: &str) -> Result<Vec<ExecutionStepConfig>, StepsJsonError> {
    let steps: Vec<serde_json::Value> =
        serde_json::from_str(json).map_err(StepsJsonError::Malformed)?;
    let mut parsed = Vec::with_capacity(steps.len());
    let mut malformed = None;
    for step in &steps {
        match parse_step_value(step) {
            Ok(config) => parsed.push(config),
            Err(e) if fallback_refused(step) => {
                return Err(StepsJsonError::UnparseableStep(StepConversionError::new(
                    step, &e,
                )));
            }
            Err(e) => {
                malformed.get_or_insert(e);
            }
        }
    }
    match malformed {
        Some(e) => Err(StepsJsonError::Malformed(e)),
        None => Ok(parsed),
    }
}

/// A step that failed [`parse_step_value`] must not be rebuilt by a
/// hand-built fallback extractor when that would drop what the step does.
/// For `ui_bridge` the fallback cannot carry the action faithfully (and an
/// action-less step silently runs `snapshot`), so the seam surfaces a
/// [`StepConversionError`] instead — never a dropped or rebuilt step.
pub fn fallback_refused(step: &serde_json::Value) -> bool {
    value_step_type(step) == Some("ui_bridge")
}

/// A step that does not parse, named by its type, name and id, with the
/// serde error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepConversionError {
    pub step_type: String,
    pub id: Option<String>,
    pub name: Option<String>,
    pub error: String,
}

impl StepConversionError {
    pub fn new(step: &serde_json::Value, error: &serde_json::Error) -> Self {
        let get = |k: &str| step.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Self {
            step_type: value_step_type(step).unwrap_or_default().to_string(),
            id: get("id"),
            name: get("name"),
            error: error.to_string(),
        }
    }
}

impl std::fmt::Display for StepConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} step", self.step_type)?;
        match (&self.name, &self.id) {
            (Some(n), Some(i)) => write!(f, " '{n}' (id {i})")?,
            (Some(n), None) => write!(f, " '{n}'")?,
            (None, Some(i)) => write!(f, " (id {i})")?,
            (None, None) => write!(f, " (unnamed, no id)")?,
        }
        write!(f, " could not be parsed: {}", self.error)
    }
}

impl std::error::Error for StepConversionError {}

/// The step a conversion seam emits for a [`StepConversionError`] when it
/// cannot propagate the error: same type, id, name and phase, and
/// `conversion_error` set, so it FAILS at execution with that message
/// (`DispatchRoute::ConversionFailed`) rather than running as anything else.
pub fn failing_step(step: &serde_json::Value, err: &StepConversionError) -> ExecutionStepConfig {
    ExecutionStepConfig {
        step_type: err.step_type.clone(),
        id: err.id.clone(),
        name: err.name.clone(),
        phase: step
            .get("phase")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        conversion_error: Some(err.to_string()),
        ..Default::default()
    }
}

/// Extract prompt steps from JSON Value array
///
/// If `explicit_phase` is provided, it will be set on all steps that don't
/// already have a phase specified.
pub fn extract_prompt_steps_from_json(steps: &[serde_json::Value]) -> Vec<ExecutionStepConfig> {
    extract_prompt_steps_with_phase(steps, None)
}

/// Extract prompt steps with explicit phase.
pub fn extract_prompt_steps_with_phase(
    steps: &[serde_json::Value],
    explicit_phase: Option<&str>,
) -> Vec<ExecutionStepConfig> {
    steps
        .iter()
        .filter(|step| {
            step.get("type")
                .or_else(|| step.get("step_type"))
                .and_then(|t| t.as_str())
                .map(|t| matches!(t, "prompt" | "ai_prompt" | "run_prompt_sequence"))
                .unwrap_or(false)
        })
        .filter_map(|step| {
            let mut config = parse_step_value(step).ok()?;

            // Set explicit phase if not already set
            if config.phase.is_none() {
                if let Some(phase_str) = explicit_phase {
                    if let Some(phase) = StepPhase::from_str_opt(phase_str) {
                        config.set_phase(phase);
                    }
                }
            }

            Some(config)
        })
        .collect()
}
