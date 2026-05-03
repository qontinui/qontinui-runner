//! Workflow Fixup Handler
//!
//! Runs deterministic Rust fixups on a workflow JSON file during meta-workflow
//! execution. Used in verification and completion phases to apply autofix,
//! hardening, and criteria validation without AI involvement.

use async_trait::async_trait;
use tracing::info;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::ExecutionStepConfig;

pub struct WorkflowFixupHandler;

#[async_trait]
impl StepHandler for WorkflowFixupHandler {
    fn step_type(&self) -> &'static str {
        "workflow_fixup"
    }

    fn display_name(&self) -> &'static str {
        "Workflow Fixup"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        // Read fields from step config
        let input_path = match &step.fixup_input_path {
            Some(p) => p.clone(),
            None => return StepHandlerResult::failure("workflow_fixup: missing fixup_input_path"),
        };
        let mode = step.fixup_mode.as_deref().unwrap_or("autofix");

        info!("Running workflow fixup (mode={}): {}", mode, input_path);

        match mode {
            "autofix" => Self::run_autofix(&input_path).await,
            "harden" => Self::run_harden(&input_path).await,
            "validate_criteria" => {
                let criteria_path = step.fixup_criteria_path.as_deref();
                Self::run_validate_criteria(&input_path, criteria_path).await
            }
            other => StepHandlerResult::failure(format!("Unknown fixup mode: {}", other)),
        }
    }
}

impl WorkflowFixupHandler {
    /// Autofix mode: parse workflow JSON, apply fix_workflow + sanitize_commands, write back.
    async fn run_autofix(input_path: &str) -> StepHandlerResult {
        // Read file
        let json_content = match tokio::fs::read_to_string(input_path).await {
            Ok(c) => c,
            Err(e) => {
                return StepHandlerResult::failure(format!("Failed to read {}: {}", input_path, e))
            }
        };

        // Parse as UnifiedWorkflow
        let mut workflow: crate::unified_workflows::UnifiedWorkflow =
            match serde_json::from_str(&json_content) {
                Ok(w) => w,
                Err(e) => {
                    return StepHandlerResult::failure(format!(
                        "Failed to parse workflow JSON: {}",
                        e
                    ))
                }
            };

        // Apply fix_workflow (UUID generation, phase assignment, command mode inference)
        crate::workflow_generation::validation::fix_workflow(&mut workflow);

        // Apply command sanitization
        use crate::workflow_generation::hardener::sanitize_commands_in_steps;
        let mut sanitize_count = 0;
        sanitize_count += sanitize_commands_in_steps(&mut workflow.setup_steps);
        sanitize_count += sanitize_commands_in_steps(&mut workflow.verification_steps);
        sanitize_count += sanitize_commands_in_steps(&mut workflow.completion_steps);
        for stage in workflow.stages.iter_mut() {
            sanitize_count += sanitize_commands_in_steps(&mut stage.setup_steps);
            sanitize_count += sanitize_commands_in_steps(&mut stage.verification_steps);
            sanitize_count += sanitize_commands_in_steps(&mut stage.completion_steps);
        }

        info!(
            "Autofix applied structural fixes, sanitized {} commands",
            sanitize_count
        );

        // Write back
        let output_json = match serde_json::to_string_pretty(&workflow) {
            Ok(j) => j,
            Err(e) => {
                return StepHandlerResult::failure(format!("Failed to serialize workflow: {}", e))
            }
        };
        if let Err(e) = tokio::fs::write(input_path, &output_json).await {
            return StepHandlerResult::failure(format!("Failed to write {}: {}", input_path, e));
        }

        StepHandlerResult::success_with_data(serde_json::json!({
            "mode": "autofix",
            "sanitize_count": sanitize_count,
        }))
    }

    /// Harden mode: apply fix_sdk_urls + sanitize_commands + fix_workflow, write back.
    async fn run_harden(input_path: &str) -> StepHandlerResult {
        let json_content = match tokio::fs::read_to_string(input_path).await {
            Ok(c) => c,
            Err(e) => {
                return StepHandlerResult::failure(format!("Failed to read {}: {}", input_path, e))
            }
        };

        let mut workflow: crate::unified_workflows::UnifiedWorkflow =
            match serde_json::from_str(&json_content) {
                Ok(w) => w,
                Err(e) => {
                    return StepHandlerResult::failure(format!(
                        "Failed to parse workflow JSON: {}",
                        e
                    ))
                }
            };

        // Apply SDK URL fixes (returns a new workflow since fix_sdk_urls takes &self).
        // Uses the deprecated `fix_sdk_urls` shim for now; Phase F will migrate
        // this caller to `normalize_ui_bridge_urls` with an explicit
        // target_family + runner_port. For runner-self workflow JSON files
        // the shim's Control default is the right behavior anyway.
        #[allow(deprecated)]
        let workflow_after_sdk = crate::workflow_generation::hardener::fix_sdk_urls(&workflow);
        workflow = workflow_after_sdk;

        // Apply command sanitization
        use crate::workflow_generation::hardener::sanitize_commands_in_steps;
        let mut sanitize_count = 0;
        sanitize_count += sanitize_commands_in_steps(&mut workflow.setup_steps);
        sanitize_count += sanitize_commands_in_steps(&mut workflow.verification_steps);
        sanitize_count += sanitize_commands_in_steps(&mut workflow.completion_steps);
        for stage in workflow.stages.iter_mut() {
            sanitize_count += sanitize_commands_in_steps(&mut stage.setup_steps);
            sanitize_count += sanitize_commands_in_steps(&mut stage.verification_steps);
            sanitize_count += sanitize_commands_in_steps(&mut stage.completion_steps);
        }

        // Apply fix_workflow
        crate::workflow_generation::validation::fix_workflow(&mut workflow);

        info!(
            "Harden applied SDK fixes, {} sanitizations, structural fixes",
            sanitize_count
        );

        let output_json = match serde_json::to_string_pretty(&workflow) {
            Ok(j) => j,
            Err(e) => {
                return StepHandlerResult::failure(format!("Failed to serialize workflow: {}", e))
            }
        };
        if let Err(e) = tokio::fs::write(input_path, &output_json).await {
            return StepHandlerResult::failure(format!("Failed to write {}: {}", input_path, e));
        }

        StepHandlerResult::success_with_data(serde_json::json!({
            "mode": "harden",
            "sanitize_count": sanitize_count,
        }))
    }

    /// Validate criteria mode: read criteria JSON, check coverage against workflow.
    async fn run_validate_criteria(
        input_path: &str,
        criteria_path: Option<&str>,
    ) -> StepHandlerResult {
        let criteria_path = match criteria_path {
            Some(p) => p,
            None => {
                return StepHandlerResult::failure("validate_criteria requires fixup_criteria_path")
            }
        };

        // Read workflow
        let workflow_json = match tokio::fs::read_to_string(input_path).await {
            Ok(c) => c,
            Err(e) => return StepHandlerResult::failure(format!("Failed to read workflow: {}", e)),
        };
        let workflow: crate::unified_workflows::UnifiedWorkflow =
            match serde_json::from_str(&workflow_json) {
                Ok(w) => w,
                Err(e) => {
                    return StepHandlerResult::failure(format!(
                        "Failed to parse workflow JSON: {}",
                        e
                    ))
                }
            };

        // Read criteria
        let criteria_json = match tokio::fs::read_to_string(criteria_path).await {
            Ok(c) => c,
            Err(e) => return StepHandlerResult::failure(format!("Failed to read criteria: {}", e)),
        };
        let criteria: crate::workflow_generation::specification::AcceptanceCriteria =
            match serde_json::from_str(&criteria_json) {
                Ok(c) => c,
                Err(e) => {
                    return StepHandlerResult::failure(format!(
                        "Failed to parse criteria JSON: {}",
                        e
                    ))
                }
            };

        // Check that each automatable criterion has a matching verification step with criterion_id
        let mut missing_criteria = Vec::new();
        let mut covered = 0;

        // Collect criterion_ids from verification steps
        let mut step_criterion_ids: Vec<String> = Vec::new();
        for step_value in &workflow.verification_steps {
            if let Some(cid) = step_value.get("criterion_id").and_then(|v| v.as_str()) {
                step_criterion_ids.push(cid.to_string());
            }
        }
        // Also check stage verification steps
        for stage in &workflow.stages {
            for step_value in &stage.verification_steps {
                if let Some(cid) = step_value.get("criterion_id").and_then(|v| v.as_str()) {
                    step_criterion_ids.push(cid.to_string());
                }
            }
        }

        use crate::workflow_generation::specification::VerificationMethod;
        for criterion in &criteria.criteria {
            if criterion.method == VerificationMethod::Manual {
                continue; // Skip manual criteria
            }
            if step_criterion_ids.contains(&criterion.id) {
                covered += 1;
            } else {
                missing_criteria.push(format!(
                    "{} ({:?}): {}",
                    criterion.id, criterion.priority, criterion.description
                ));
            }
        }

        let automatable_count = criteria
            .criteria
            .iter()
            .filter(|c| c.method != VerificationMethod::Manual)
            .count();

        let coverage_pct = (covered * 100).checked_div(automatable_count).unwrap_or(100);

        if missing_criteria.is_empty() {
            StepHandlerResult::success_with_data(serde_json::json!({
                "mode": "validate_criteria",
                "covered": covered,
                "total_automatable": automatable_count,
                "coverage_pct": coverage_pct,
            }))
        } else {
            // Don't hard-fail -- just report. The AI review will catch this too.
            StepHandlerResult::success_with_data(serde_json::json!({
                "mode": "validate_criteria",
                "covered": covered,
                "total_automatable": automatable_count,
                "missing_criteria": missing_criteria,
                "coverage_pct": coverage_pct,
            }))
        }
    }
}
