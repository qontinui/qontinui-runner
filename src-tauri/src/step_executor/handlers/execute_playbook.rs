//! Execute Playbook Step Handler
//!
//! Executes machine-generated playbook steps by parsing the markdown format
//! produced by `playbook_generator.py` and driving the state machine through
//! the specified transitions.
//!
//! Playbook format:
//! ```markdown
//! ### Step 1: Click
//! - **Action**: click on "Sign In"
//! - **Transition**: LoginForm → Dashboard
//! - **Variable**: `{{emailAddress}}`
//! ```

use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use tracing::{error, info, warn};

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

// ---------------------------------------------------------------------------
// Parsed playbook structures
// ---------------------------------------------------------------------------

/// A single parsed step from the playbook markdown.
#[derive(Debug)]
struct PlaybookStep {
    /// Step number from the header
    number: u32,
    /// Step label (e.g. "Click", "Type", "Navigate")
    label: String,
    /// Action description (from **Action** line)
    action: Option<String>,
    /// Transition: (source_state, target_state) from **Transition** line
    transition: Option<(String, String)>,
    /// Variable name from **Variable** line
    variable: Option<String>,
}

// ---------------------------------------------------------------------------
// Playbook parser
// ---------------------------------------------------------------------------

/// Parse a playbook markdown string into a sequence of steps.
fn parse_playbook(content: &str) -> Vec<PlaybookStep> {
    let step_header_re = Regex::new(r"###\s+Step\s+(\d+):\s*(.+)").unwrap();
    let action_re = Regex::new(r"-\s+\*\*Action\*\*:\s*(.+)").unwrap();
    let transition_re = Regex::new(r"-\s+\*\*Transition\*\*:\s*(.+?)\s*→\s*(.+)").unwrap();
    let variable_re = Regex::new(r"-\s+\*\*Variable\*\*:\s*`?\{\{(\w+)\}\}`?").unwrap();

    let mut steps: Vec<PlaybookStep> = Vec::new();
    let mut current: Option<PlaybookStep> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(caps) = step_header_re.captures(trimmed) {
            // Flush previous step
            if let Some(step) = current.take() {
                steps.push(step);
            }

            let number: u32 = caps[1].parse().unwrap_or(0);
            let label = caps[2].trim().to_string();

            current = Some(PlaybookStep {
                number,
                label,
                action: None,
                transition: None,
                variable: None,
            });
            continue;
        }

        if let Some(ref mut step) = current {
            if let Some(caps) = action_re.captures(trimmed) {
                step.action = Some(caps[1].trim().to_string());
            } else if let Some(caps) = transition_re.captures(trimmed) {
                let source = caps[1].trim().to_string();
                let target = caps[2].trim().to_string();
                step.transition = Some((source, target));
            } else if let Some(caps) = variable_re.captures(trimmed) {
                step.variable = Some(caps[1].to_string());
            }
        }
    }

    // Flush last step
    if let Some(step) = current.take() {
        steps.push(step);
    }

    steps
}

/// Resolve `{{variableName}}` placeholders in a string using shared variables.
fn resolve_variables(text: &str, context: &HandlerContext) -> String {
    let var_re = Regex::new(r"\{\{(\w+)\}\}").unwrap();
    var_re
        .replace_all(text, |caps: &regex::Captures| {
            let var_name = &caps[1];
            // SharedVariableStore.get() returns Option<String>
            context
                .shared_variables
                .get(var_name)
                .unwrap_or_else(|| format!("{{{{{}}}}}", var_name))
        })
        .into_owned()
}

// ---------------------------------------------------------------------------
// Action description parser
// ---------------------------------------------------------------------------

/// Parse a natural-language action description into (action_type, target).
///
/// Examples:
///   `click on "Sign In"` → ("CLICK", "Sign In")
///   `type "hello" into email field` → ("TYPE", "email field")
///   `select "Option A"` → ("SELECT", "Option A")
fn parse_action_description(desc: &str) -> (String, String) {
    let desc_lower = desc.to_lowercase();

    // Try to extract quoted target first
    let quote_re = Regex::new(r#""([^"]+)""#).unwrap();
    let quoted: Vec<String> = quote_re
        .captures_iter(desc)
        .map(|c| c[1].to_string())
        .collect();

    if desc_lower.starts_with("click") {
        let target = quoted.first().cloned().unwrap_or_default();
        return ("CLICK".to_string(), target);
    }

    if desc_lower.starts_with("type") || desc_lower.starts_with("enter") {
        // For "type X into Y", the target is Y (after "into"/"in")
        if let Some(pos) = desc_lower
            .find(" into ")
            .or_else(|| desc_lower.find(" in "))
        {
            let after = &desc[pos..]
                .trim_start_matches(" into ")
                .trim_start_matches(" in ");
            let target = after.trim().trim_matches('"').to_string();
            return ("TYPE".to_string(), target);
        }
        let target = quoted.last().cloned().unwrap_or_default();
        return ("TYPE".to_string(), target);
    }

    if desc_lower.starts_with("select") {
        let target = quoted.first().cloned().unwrap_or_default();
        return ("SELECT".to_string(), target);
    }

    // Fallback: use the label as action type and first quoted string as target
    let action_type = desc_lower
        .split_whitespace()
        .next()
        .unwrap_or("click")
        .to_uppercase();
    let target = quoted.first().cloned().unwrap_or_default();
    (action_type, target)
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub struct ExecutePlaybookHandler;

#[async_trait]
impl StepHandler for ExecutePlaybookHandler {
    fn step_type(&self) -> &'static str {
        "execute_playbook"
    }

    fn display_name(&self) -> &'static str {
        "Execute Playbook"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Get playbook content from prompt_content (the primary text field)
        let playbook_content = match &step.prompt_content {
            Some(content) if !content.is_empty() => content.clone(),
            _ => {
                return StepHandlerResult::failure(
                    "execute_playbook: missing playbook content in prompt_content",
                );
            }
        };

        let step_name = step.name.as_deref().unwrap_or("Execute Playbook");
        info!("Executing playbook: {}", step_name);

        // Parse playbook markdown
        let playbook_steps = parse_playbook(&playbook_content);
        if playbook_steps.is_empty() {
            return StepHandlerResult::failure(
                "execute_playbook: no steps found in playbook content",
            );
        }

        info!(
            "Parsed {} playbook steps from '{}'",
            playbook_steps.len(),
            step_name,
        );

        let timeout_per_step = step.timeout_seconds.unwrap_or(30);
        let mut completed_steps: Vec<serde_json::Value> = Vec::new();
        let mut failed = false;
        let mut failure_message = String::new();

        for pb_step in &playbook_steps {
            info!(
                "Playbook step {}: {} (transition: {:?})",
                pb_step.number, pb_step.label, pb_step.transition
            );

            // If the step has a variable reference, resolve it
            let resolved_action = pb_step
                .action
                .as_ref()
                .map(|a| resolve_variables(a, context));

            // Execute transition if present
            if let Some((ref _source, ref target)) = pb_step.transition {
                let resolved_target = resolve_variables(target, context);

                match context
                    .action_service
                    .go_to_state(&resolved_target, None, None, Some(timeout_per_step))
                    .await
                {
                    Ok(result) => {
                        if result.success {
                            info!(
                                "Playbook step {} transition to '{}' succeeded",
                                pb_step.number, resolved_target,
                            );
                            completed_steps.push(json!({
                                "step": pb_step.number,
                                "label": pb_step.label,
                                "transition": resolved_target,
                                "success": true,
                            }));
                        } else {
                            let msg = format!(
                                "Playbook step {} transition to '{}' failed: {}",
                                pb_step.number,
                                resolved_target,
                                result.error.as_deref().unwrap_or("unknown"),
                            );
                            warn!("{}", msg);
                            failed = true;
                            failure_message = msg;
                            completed_steps.push(json!({
                                "step": pb_step.number,
                                "label": pb_step.label,
                                "transition": resolved_target,
                                "success": false,
                                "error": result.error,
                            }));
                            break;
                        }
                    }
                    Err(e) => {
                        let msg = format!(
                            "Playbook step {} transition to '{}' error: {}",
                            pb_step.number, resolved_target, e,
                        );
                        error!("{}", msg);
                        failed = true;
                        failure_message = msg;
                        completed_steps.push(json!({
                            "step": pb_step.number,
                            "label": pb_step.label,
                            "transition": resolved_target,
                            "success": false,
                            "error": e.to_string(),
                        }));
                        break;
                    }
                }
            } else if let Some(ref action_desc) = resolved_action {
                // Action without transition — try to execute via the action service.
                // NOTE: execute_action expects an image/element ID, not a human label.
                // If the target is a natural-language description (e.g., "Sign In"),
                // the action service may not be able to resolve it. In that case,
                // we log a warning and skip rather than failing the entire playbook.
                info!("Playbook step {} action: {}", pb_step.number, action_desc,);

                let (action_type, target) = parse_action_description(action_desc);

                // Check if target looks like a natural-language label vs an element ID
                let is_nl_target = target.contains(' ') || target.starts_with('"');
                if is_nl_target {
                    warn!(
                        "Playbook step {} has natural-language target '{}' — \
                         action-without-transition steps require state machine context. \
                         Skipping action execution.",
                        pb_step.number, target,
                    );
                    completed_steps.push(json!({
                        "step": pb_step.number,
                        "label": pb_step.label,
                        "action": action_desc,
                        "success": true,
                        "skipped": true,
                        "reason": "natural-language target requires state machine context",
                    }));
                    continue;
                }

                match context
                    .action_service
                    .execute_action(&action_type, &target, None, None)
                    .await
                {
                    Ok(result) => {
                        completed_steps.push(json!({
                            "step": pb_step.number,
                            "label": pb_step.label,
                            "action": action_desc,
                            "success": result.success,
                        }));
                        if !result.success {
                            let msg = format!(
                                "Playbook step {} action '{}' failed: {}",
                                pb_step.number,
                                action_desc,
                                result.message.as_deref().unwrap_or("unknown"),
                            );
                            warn!("{}", msg);
                            failed = true;
                            failure_message = msg;
                            break;
                        }
                    }
                    Err(e) => {
                        let msg = format!("Playbook step {} action error: {}", pb_step.number, e,);
                        error!("{}", msg);
                        failed = true;
                        failure_message = msg;
                        completed_steps.push(json!({
                            "step": pb_step.number,
                            "label": pb_step.label,
                            "action": action_desc,
                            "success": false,
                            "error": e.to_string(),
                        }));
                        break;
                    }
                }
            } else {
                // Step with no transition and no action — skip
                info!(
                    "Playbook step {} has no transition or action, skipping",
                    pb_step.number,
                );
                completed_steps.push(json!({
                    "step": pb_step.number,
                    "label": pb_step.label,
                    "skipped": true,
                }));
            }
        }

        let output = json!({
            "totalSteps": playbook_steps.len(),
            "completedSteps": completed_steps.len(),
            "steps": completed_steps,
        });

        if failed {
            StepHandlerResult::failure_with_data(failure_message, output)
        } else {
            StepHandlerResult::success_with_data(output)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_playbook_basic() {
        let content = r#"
## Playbook: Login Flow

### Step 1: Click
- **Action**: click on "Sign In"
- **Transition**: Home → LoginForm

### Step 2: Type
- **Action**: type "user@example.com" into email field
- **Variable**: `{{emailAddress}}`

### Step 3: Click
- **Action**: click on "Submit"
- **Transition**: LoginForm → Dashboard
"#;

        let steps = parse_playbook(content);
        assert_eq!(steps.len(), 3);

        assert_eq!(steps[0].number, 1);
        assert_eq!(steps[0].label, "Click");
        assert_eq!(steps[0].action.as_deref(), Some(r#"click on "Sign In""#));
        assert_eq!(
            steps[0].transition,
            Some(("Home".to_string(), "LoginForm".to_string()))
        );
        assert!(steps[0].variable.is_none());

        assert_eq!(steps[1].number, 2);
        assert_eq!(steps[1].variable.as_deref(), Some("emailAddress"));
        assert!(steps[1].transition.is_none());

        assert_eq!(steps[2].number, 3);
        assert_eq!(
            steps[2].transition,
            Some(("LoginForm".to_string(), "Dashboard".to_string()))
        );
    }

    #[test]
    fn test_parse_playbook_empty() {
        let steps = parse_playbook("# Just a title\nNo steps here.");
        assert!(steps.is_empty());
    }

    #[test]
    fn test_parse_playbook_arrow_with_spaces() {
        let content = "### Step 1: Navigate\n- **Transition**: State A  →  State B\n";
        let steps = parse_playbook(content);
        assert_eq!(steps.len(), 1);
        assert_eq!(
            steps[0].transition,
            Some(("State A".to_string(), "State B".to_string()))
        );
    }
}
