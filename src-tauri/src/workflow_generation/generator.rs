//! Workflow Generator
//!
//! Generates UnifiedWorkflows from natural language descriptions using a
//! 3-agent agentic pipeline:
//!
//! 1. **Builder Agent** — generates the initial workflow JSON from the user's
//!    natural-language description + schema context.
//! 2. **Verification Agent** — reviews every deterministic step for semantic
//!    correctness *without running them*: command syntax, URL validity,
//!    check_type / command consistency, prompt quality, cross-step references,
//!    logical phase flow, etc.
//! 3. **Fixer Agent** — takes the verification report and the current workflow
//!    JSON, then produces a corrected version.
//!
//! Steps 2–3 loop until the verification agent reports zero issues or
//! `max_fix_iterations` is reached.

use crate::ai_provider::AiResponse;
use crate::ai_router::TaskContext;
use crate::commands::logging::AiOutputEntry;
use crate::context;
use crate::doctor::DoctorHandle;
use crate::skills::SkillRegistry;
use crate::unified_workflows::UnifiedWorkflow;
use crate::workflow_generation::dependency_analysis;
use crate::workflow_generation::hardener::{self, HardeningSummary};
use crate::workflow_generation::investigator;
use crate::workflow_generation::pipeline_artifacts::{
    compute_json_diff, PipelineArtifact, PipelineArtifactBuilder,
};
use crate::workflow_generation::prompt_analysis;
use crate::workflow_generation::revision;
use crate::workflow_generation::rules;
use crate::workflow_generation::schema_context::{
    build_schema_context, build_schema_context_full, format_skills_for_generator,
};
use crate::workflow_generation::self_improve;
use crate::workflow_generation::specification;
use crate::workflow_generation::validation::{fix_workflow, validate_workflow};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{debug, error, info, warn};

// ============================================================================
// Public types
// ============================================================================

/// Request to generate a workflow from natural language
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateWorkflowRequest {
    /// Natural language description of what the workflow should do
    pub description: String,
    /// Optional category for the generated workflow
    pub category: Option<String>,
    /// Optional tags for the generated workflow
    pub tags: Option<Vec<String>>,

    // === Workflow Configuration Options ===
    /// Maximum iterations for agentic phase (default: 10)
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// AI provider override (claude_cli, anthropic_api, openai_api, gemini_api)
    #[serde(default)]
    pub provider: Option<String>,
    /// Model override (depends on provider)
    #[serde(default)]
    pub model: Option<String>,
    /// Per-phase model overrides
    #[serde(default)]
    pub model_overrides:
        Option<std::collections::HashMap<String, crate::unified_workflows::ModelOverrideConfig>>,
    /// Skip AI summary generation at the end (default: false)
    #[serde(default)]
    pub skip_ai_summary: Option<bool>,
    /// Log source selection mode: "default", "ai", "all", or a profile_id
    #[serde(default)]
    pub log_source_selection: Option<String>,
    /// Custom developer prompt template for the workflow
    #[serde(default)]
    pub prompt_template: Option<String>,
    /// Whether to auto-include contexts based on task mentions (default: true)
    #[serde(default)]
    pub auto_include_contexts: Option<bool>,

    /// Context IDs to resolve and inject into the generation prompt
    #[serde(default)]
    pub context_ids: Option<Vec<String>>,
    /// Inline context text to inject directly (e.g., pasted CLAUDE.md content)
    #[serde(default)]
    pub inline_context: Option<String>,

    /// Maximum verification→fix iterations (default: 3, 0 = skip verification)
    #[serde(default)]
    pub max_fix_iterations: Option<u32>,

    /// Discovery mode: "auto" (default), "enabled" (always), "disabled" (never)
    #[serde(default)]
    pub discovery_mode: Option<String>,

    /// Whether to include UI Bridge SDK integration instructions in the builder prompt (default: true)
    #[serde(default = "default_true")]
    pub include_ui_bridge_instructions: Option<bool>,

    /// Whether to enable reflection mode for agentic iterations (default: true)
    #[serde(default = "default_true")]
    pub reflection_mode: Option<bool>,

    /// Whether to run an AI investigation step before the builder agent (default: true)
    #[serde(default = "default_true")]
    pub investigate_codebase: Option<bool>,

    /// Whether to include frontend design quality guidance in the builder prompt (default: false)
    #[serde(default)]
    pub include_design_guidance: Option<bool>,

    /// Whether to automatically run the generated workflow after the meta-workflow completes.
    /// When true, the backend will spawn the generated workflow without relying on frontend polling.
    #[serde(default)]
    pub auto_run: Option<bool>,

    /// Whether to run a specification agent before the builder (default: true).
    /// Defines acceptance criteria that guide verification step generation.
    #[serde(default = "default_true")]
    pub generate_specification: Option<bool>,

    /// Verification depth level: "smoke", "standard" (default), "thorough", "regression"
    /// Controls how many verification steps are generated and whether known issues are checked.
    #[serde(default)]
    pub verification_depth: Option<String>,
}

fn default_true() -> Option<bool> {
    Some(true)
}

impl Default for GenerateWorkflowRequest {
    fn default() -> Self {
        Self {
            description: String::new(),
            category: None,
            tags: None,
            max_iterations: None,
            provider: None,
            model: None,
            model_overrides: None,
            skip_ai_summary: None,
            log_source_selection: None,
            prompt_template: None,
            auto_include_contexts: None,
            context_ids: None,
            inline_context: None,
            max_fix_iterations: None,
            discovery_mode: None,
            include_ui_bridge_instructions: Some(true),
            reflection_mode: Some(true),
            investigate_codebase: Some(true),
            include_design_guidance: None,
            auto_run: None,
            generate_specification: Some(true),
            verification_depth: None,
        }
    }
}

/// One pass of the verification→fix loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationIteration {
    /// 1-based iteration number
    pub iteration: u32,
    /// Issues found by the verification agent
    pub issues: Vec<String>,
    /// Whether the fixer was invoked
    pub fix_applied: bool,
    /// Error message if the fixer agent failed (e.g., produced invalid JSON)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_error: Option<String>,
}

/// Response from workflow generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateWorkflowResponse {
    /// The generated workflow (if successful)
    pub workflow: Option<UnifiedWorkflow>,
    /// Structural validation errors (from deterministic validator)
    pub validation_errors: Vec<super::validation::ValidationError>,
    /// Whether the generation was successful
    pub success: bool,
    /// Error message if generation failed
    pub error: Option<String>,
    /// The model that was used for generation (not available for CLI)
    pub model_used: Option<String>,
    /// Details of each verification→fix iteration (empty when skipped)
    #[serde(default)]
    pub verification_iterations: Vec<VerificationIteration>,
    /// Summary of verification hardening (prompt → deterministic conversions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardening_summary: Option<HardeningSummary>,
    /// Discovery tool calls made during generation
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovery_calls: Vec<super::discovery_tools::DiscoveryCall>,
    /// Acceptance criteria generated by the specification phase (if run)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<specification::AcceptanceCriteria>,
    /// Quality report from the revision phase
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_report: Option<revision::QualityReport>,
    /// Confidence score (0.0–1.0) reflecting overall generation quality
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_score: Option<f32>,
}

// ============================================================================
// Discovery Feedback Helpers
// ============================================================================

/// Scan workflow steps for references (commands, URLs, file paths) that were
/// not mentioned in the original discovery context. Returns a deduplicated
/// list of undiscovered references.
fn find_undiscovered_references(
    workflow: &UnifiedWorkflow,
    discovery_context: &str,
) -> Vec<String> {
    let dc_lower = discovery_context.to_lowercase();
    let mut refs: Vec<String> = Vec::new();

    // Collect all step JSON values from the workflow
    let all_steps = workflow
        .setup_steps
        .iter()
        .chain(workflow.verification_steps.iter())
        .chain(workflow.agentic_steps.iter())
        .chain(workflow.completion_steps.iter())
        .chain(workflow.stages.iter().flat_map(|s| {
            s.setup_steps
                .iter()
                .chain(s.verification_steps.iter())
                .chain(s.agentic_steps.iter())
                .chain(s.completion_steps.iter())
        }));

    for step in all_steps {
        // Extract command field
        if let Some(cmd) = step.get("command").and_then(|v| v.as_str()) {
            // Extract the base command/tool name (first word)
            if let Some(base_cmd) = cmd.split_whitespace().next() {
                let clean = base_cmd.trim_start_matches("./");
                if !clean.is_empty() && clean.len() > 2 && !dc_lower.contains(&clean.to_lowercase())
                {
                    refs.push(clean.to_string());
                }
            }

            // Extract file paths from command (tokens starting with / or ./)
            for token in cmd.split_whitespace() {
                if (token.starts_with('/') || token.starts_with("./"))
                    && token.len() > 3
                    && !dc_lower.contains(&token.to_lowercase())
                {
                    refs.push(token.to_string());
                }
            }
        }

        // Extract URL fields
        for field in &["url", "health_check_url"] {
            if let Some(url) = step.get(*field).and_then(|v| v.as_str()) {
                if !url.is_empty() && !dc_lower.contains(&url.to_lowercase()) {
                    refs.push(url.to_string());
                }
            }
        }
    }

    // Deduplicate
    refs.sort();
    refs.dedup();
    refs
}

// ============================================================================
// Known Issue Injection
// ============================================================================

/// Inject deterministic verification steps for known issues that have
/// a `verification_step_template`. These bypass AI generation entirely.
fn inject_known_issue_steps(
    workflow: &mut UnifiedWorkflow,
    issues: &[crate::known_issues::KnownIssue],
    conn: Option<&Connection>,
) -> usize {
    let mut injected = 0;

    for issue in issues {
        // Only inject if the issue has a step template
        let step_template = match &issue.verification_step_template {
            Some(tmpl) if !tmpl.is_null() => tmpl.clone(),
            _ => {
                // Try to instantiate from the pattern template
                if let (Some(template_id), Some(c)) = (&issue.pattern_template_id, conn) {
                    match crate::known_issues::storage::get_pattern_template(c, template_id) {
                        Ok(Some(template)) => {
                            if let Some(ref step_tmpl) = template.step_template {
                                instantiate_template(step_tmpl, &issue.detection_config)
                            } else {
                                continue;
                            }
                        }
                        _ => continue,
                    }
                } else {
                    continue;
                }
            }
        };

        // Build the verification step from the template
        if let Some(step_obj) = step_template.as_object() {
            let mut step = step_obj.clone();
            // Ensure required fields
            if !step.contains_key("id") {
                step.insert(
                    "id".to_string(),
                    serde_json::json!(format!("regression-{}", &issue.id)),
                );
            }
            if !step.contains_key("name") {
                let name = format!("[Regression] {}", issue.title);
                step.insert("name".to_string(), serde_json::json!(name));
            }
            // Ensure phase is set to verification
            step.insert("phase".to_string(), serde_json::json!("verification"));
            // Tag as regression step
            step.insert(
                "regression_issue_id".to_string(),
                serde_json::json!(issue.id),
            );

            workflow
                .verification_steps
                .push(serde_json::Value::Object(step));
            injected += 1;
        }
    }

    injected
}

/// Fill template variables ({{key}}) with values from detection_config.
fn instantiate_template(
    template: &serde_json::Value,
    config: &serde_json::Value,
) -> serde_json::Value {
    let template_str = serde_json::to_string(template).unwrap_or_default();
    let mut result = template_str;

    if let Some(obj) = config.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{{{}}}}}", key);
            let replacement = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }

    serde_json::from_str(&result).unwrap_or_else(|_| template.clone())
}

// ============================================================================
// Main entry point
// ============================================================================

/// Generate a workflow from a natural language description using the
/// builder → verification → fixer agentic pipeline.
///
/// When `conn` and optionally `query_embedding` are provided, uses filtered
/// schema context with RAG examples for improved generation quality.
pub fn generate_workflow(
    request: GenerateWorkflowRequest,
    doctor_handle: Option<&DoctorHandle>,
    conn: Option<&Connection>,
    query_embedding: Option<&[f32]>,
) -> (GenerateWorkflowResponse, PipelineArtifact) {
    info!(
        "Generating workflow from description: {}",
        &request.description[..request.description.len().min(100)]
    );

    let pipeline_start = Instant::now();
    let mut artifact_builder =
        PipelineArtifactBuilder::new(&request.description, request.category.as_deref());

    let max_fix_iters = request.max_fix_iterations.unwrap_or(3);

    // ── Discovery Phase ──────────────────────────────────────────────────
    let discovery_start = Instant::now();
    let discovery_mode = request.discovery_mode.as_deref().unwrap_or("auto");
    let (mut discovery_context, discovery_calls) = if discovery_mode != "disabled" {
        let config = super::discovery_tools::DiscoveryConfig::default();
        let result =
            super::discovery_tools::run_discovery(&request.description, &config, discovery_mode);
        if !result.calls.is_empty() {
            info!(
                "Discovery: {} tools ran, {} succeeded",
                result.calls.len(),
                result.calls.iter().filter(|c| c.success).count()
            );
        }
        (result.context, result.calls)
    } else {
        debug!("Discovery disabled by request");
        (String::new(), vec![])
    };
    artifact_builder.discovery_duration_ms = Some(discovery_start.elapsed().as_millis() as u64);
    artifact_builder.discovery_calls = serde_json::to_value(&discovery_calls).ok();

    // ── Resolve per-phase model overrides for generation pipeline ────────
    let generation_model: Option<&str> = request
        .model_overrides
        .as_ref()
        .and_then(|m| m.get("generation"))
        .and_then(|c| c.model.as_deref())
        .or(request.model.as_deref());
    let generation_provider: Option<&str> = request
        .model_overrides
        .as_ref()
        .and_then(|m| m.get("generation"))
        .and_then(|c| c.provider.as_deref())
        .or(request.provider.as_deref());
    let investigation_model: Option<&str> = request
        .model_overrides
        .as_ref()
        .and_then(|m| m.get("investigation"))
        .and_then(|c| c.model.as_deref())
        .or(generation_model);
    let investigation_provider: Option<&str> = request
        .model_overrides
        .as_ref()
        .and_then(|m| m.get("investigation"))
        .and_then(|c| c.provider.as_deref())
        .or(generation_provider);
    let verification_model: Option<&str> = request
        .model_overrides
        .as_ref()
        .and_then(|m| m.get("verification"))
        .and_then(|c| c.model.as_deref())
        .or(generation_model);
    let verification_provider: Option<&str> = request
        .model_overrides
        .as_ref()
        .and_then(|m| m.get("verification"))
        .and_then(|c| c.provider.as_deref())
        .or(generation_provider);
    let specification_model: Option<&str> = request
        .model_overrides
        .as_ref()
        .and_then(|m| m.get("specification"))
        .and_then(|c| c.model.as_deref())
        .or(generation_model);
    let specification_provider: Option<&str> = request
        .model_overrides
        .as_ref()
        .and_then(|m| m.get("specification"))
        .and_then(|c| c.provider.as_deref())
        .or(generation_provider);

    // ── Investigation Phase ────────────────────────────────────────────────
    // Resolve contexts once (shared between investigation and builder).
    let resolved_contexts = {
        let mut ctx = String::new();
        if let Some(ref ids) = request.context_ids {
            if !ids.is_empty() {
                let resolved = context::resolve_contexts(ids, false, "", &[], &[]);
                if let Some(formatted) = context::format_contexts_for_prompt(&resolved) {
                    ctx.push_str(&formatted);
                }
            }
        }
        if let Some(ref inline) = request.inline_context {
            if !inline.is_empty() {
                ctx.push_str(&format!(
                    "<context name=\"User-Provided Context\">\n{}\n</context>\n\n",
                    inline
                ));
            }
        }
        ctx
    };

    let effective_request =
        if request.investigate_codebase.unwrap_or(true) && !discovery_context.is_empty() {
            info!("Running pre-generation investigation step...");
            let investigation = investigator::run_investigation(
                &request.description,
                &discovery_context,
                &resolved_contexts,
                doctor_handle,
                investigation_model,
                investigation_provider,
            );
            artifact_builder.investigation_duration_ms = Some(investigation.duration_ms);

            // Persist investigation output to ai-output.jsonl for debugging visibility
            {
                let status = if investigation.success {
                    "success"
                } else {
                    "failed"
                };
                let log_line = format!(
                    "[Investigation {} in {}ms] {}",
                    status,
                    investigation.duration_ms,
                    if investigation.success {
                        &investigation.enriched_description
                    } else {
                        "(fell back to original description)"
                    }
                );
                let entry = AiOutputEntry {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now().timestamp_millis(),
                    line: log_line,
                    source: "workflow-generator".to_string(),
                    action_id: None,
                    task_run_id: None,
                    session_id: None,
                    session_name: Some(format!(
                        "Generate: {}",
                        &request.description[..request.description.len().min(60)]
                    )),
                    phase: Some("investigation".to_string()),
                    phase_iteration: None,
                    screenshot_path: None,
                    screenshot_width: None,
                    screenshot_height: None,
                };
                let log_response = crate::commands::logging::append_ai_output_log(entry);
                if !log_response.success {
                    warn!(
                        "Failed to persist investigation AI output log: {}",
                        log_response.message.unwrap_or_default()
                    );
                }
            }

            if investigation.success {
                artifact_builder.investigation_enriched_description =
                    Some(investigation.enriched_description.clone());
                let mut enriched_request = request.clone();
                enriched_request.description = investigation.enriched_description;
                enriched_request
            } else {
                info!("Investigation failed, falling back to original description");
                request.clone()
            }
        } else {
            if request.investigate_codebase.unwrap_or(true) {
                debug!("Investigation skipped: no discovery context available");
            } else {
                debug!("Investigation disabled by request");
            }
            request.clone()
        };

    // ── Load self-improvement insights for prompt injection ─────────────
    let self_improve_ctx = conn.and_then(|c| match self_improve::analyze_generation_patterns(c) {
        Ok(ctx) if !ctx.is_empty() => Some(ctx),
        Ok(_) => None,
        Err(e) => {
            warn!("Failed to load self-improvement context: {}", e);
            None
        }
    });

    // ── Query Known Issues for Regression/Thorough ─────────────────────
    let verification_depth = request.verification_depth.as_deref().unwrap_or("standard");
    let relevant_issues: Vec<crate::known_issues::KnownIssue> = if matches!(
        verification_depth,
        "thorough" | "regression"
    ) {
        if let Some(c) = conn {
            match crate::known_issues::storage::find_relevant_issues_for_generation_with_context(
                c,
                verification_depth,
                &request.description,
            ) {
                Ok(issues) => {
                    if !issues.is_empty() {
                        info!(
                            "Found {} known issues for {} verification depth",
                            issues.len(),
                            verification_depth
                        );
                    }
                    issues
                }
                Err(e) => {
                    warn!("Failed to query known issues: {}", e);
                    vec![]
                }
            }
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    // ── Specification Phase ─────────────────────────────────────────────
    let acceptance_criteria = if request.generate_specification.unwrap_or(true) {
        info!("Running specification agent...");
        let spec_insights = self_improve_ctx
            .as_ref()
            .map(self_improve::format_specification_insights);
        let spec_result = specification::run_specification_agent(
            &effective_request.description,
            &discovery_context,
            &resolved_contexts,
            doctor_handle,
            specification_model,
            specification_provider,
            spec_insights.as_deref(),
            verification_depth,
            &relevant_issues,
        );

        artifact_builder.specification_duration_ms = Some(spec_result.duration_ms);
        artifact_builder.specification_criteria = serde_json::to_value(&spec_result.criteria).ok();
        artifact_builder.specification_prompt = Some(spec_result.prompt);

        // Persist specification output to ai-output.jsonl
        {
            let status = if spec_result.success {
                "success"
            } else {
                "failed"
            };
            let criteria_summary = if spec_result.success {
                format!(
                    "{} criteria ({})",
                    spec_result.criteria.criteria.len(),
                    spec_result.criteria.goal_summary
                )
            } else {
                "(no criteria produced)".to_string()
            };
            let log_line = format!(
                "[Specification {} in {}ms] {}",
                status, spec_result.duration_ms, criteria_summary
            );
            let entry = AiOutputEntry {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().timestamp_millis(),
                line: log_line,
                source: "workflow-generator".to_string(),
                action_id: None,
                task_run_id: None,
                session_id: None,
                session_name: Some(format!(
                    "Generate: {}",
                    &request.description[..request.description.len().min(60)]
                )),
                phase: Some("specification".to_string()),
                phase_iteration: None,
                screenshot_path: None,
                screenshot_width: None,
                screenshot_height: None,
            };
            let log_response = crate::commands::logging::append_ai_output_log(entry);
            if !log_response.success {
                warn!(
                    "Failed to persist specification AI output log: {}",
                    log_response.message.unwrap_or_default()
                );
            }
        }

        if spec_result.success && !spec_result.criteria.criteria.is_empty() {
            // Write back verification_step_template to matched known issues
            if let Some(c) = conn {
                write_back_verification_templates(c, &relevant_issues, &spec_result.criteria);
            }
            Some(spec_result.criteria)
        } else {
            None // Graceful fallback — builder runs without criteria
        }
    } else {
        debug!("Specification disabled by request");
        None
    };

    // ── Step 1: Builder Agent ──────────────────────────────────────────────
    let builder_start = Instant::now();
    let builder_insights_section = self_improve_ctx
        .as_ref()
        .map(self_improve::format_builder_insights);
    let mut workflow = match run_builder_agent(
        &effective_request,
        &discovery_context,
        acceptance_criteria.as_ref(),
        doctor_handle,
        conn,
        query_embedding,
        generation_model,
        generation_provider,
        builder_insights_section.as_deref(),
    ) {
        Ok((w, prompt)) => {
            artifact_builder.builder_duration_ms = Some(builder_start.elapsed().as_millis() as u64);
            artifact_builder.builder_parsed_json = serde_json::to_value(&w).ok();
            artifact_builder.builder_prompt = Some(prompt);
            w
        }
        Err(resp) => {
            artifact_builder.builder_duration_ms = Some(builder_start.elapsed().as_millis() as u64);
            artifact_builder.success = false;
            artifact_builder.error_message = resp.error.clone();
            let artifact = artifact_builder.build(pipeline_start.elapsed().as_millis() as u64);
            return (*resp, artifact);
        }
    };

    // ── Discovery Feedback Pass ──────────────────────────────────────────
    if discovery_mode != "disabled" {
        let undiscovered = find_undiscovered_references(&workflow, &discovery_context);
        if !undiscovered.is_empty() {
            info!(
                "Found {} references not in discovery context, running targeted re-discovery",
                undiscovered.len()
            );
            let feedback_description =
                format!("Gather information about: {}", undiscovered.join(", "));
            let config = super::discovery_tools::DiscoveryConfig::default();
            let feedback_result =
                super::discovery_tools::run_discovery(&feedback_description, &config, "enabled");
            if !feedback_result.context.is_empty() {
                discovery_context.push_str("\n\n## Re-discovery (feedback pass)\n");
                discovery_context.push_str(&feedback_result.context);
                info!(
                    "Re-discovery added {} chars of context ({} tool calls)",
                    feedback_result.context.len(),
                    feedback_result.calls.len()
                );
            }
        }
    }

    // Apply request overrides
    apply_request_options(&mut workflow, &request);

    // Deterministic auto-fix (UUIDs, timestamps, phase mismatches)
    let autofix_start = Instant::now();
    let before_autofix = serde_json::to_value(&workflow).ok();
    fix_workflow(&mut workflow);
    let after_autofix = serde_json::to_value(&workflow).ok();
    artifact_builder.autofix_duration_ms = Some(autofix_start.elapsed().as_millis() as u64);

    // Compute autofix diff
    if let (Some(ref before), Some(ref after)) = (&before_autofix, &after_autofix) {
        let diff = compute_json_diff(before, after);
        if diff.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            artifact_builder.autofix_diff = Some(diff);
        }
    }

    // ── Load skill registry once (built-in + user skills from DB) ───────
    let skill_registry = SkillRegistry::with_db(conn);

    // ── Step 2–3: Verification ↔ Fixer loop ────────────────────────────────
    let verification_start = Instant::now();
    let mut iterations: Vec<VerificationIteration> = Vec::new();

    if max_fix_iters > 0 {
        let mut previous_issue_count: Option<usize> = None;

        for iter_num in 1..=max_fix_iters {
            info!("Verification iteration {}/{}", iter_num, max_fix_iters);

            // Run verification agent
            let verification_blind_spots = self_improve_ctx
                .as_ref()
                .map(self_improve::format_verification_insights);
            let (issues, verification_prompt) = run_verification_agent(
                &workflow,
                &request.description,
                &discovery_context,
                acceptance_criteria.as_ref(),
                doctor_handle,
                conn,
                verification_model,
                verification_provider,
                &skill_registry,
                verification_blind_spots.as_deref(),
            );
            if !verification_prompt.is_empty() {
                artifact_builder
                    .verification_prompts
                    .push(verification_prompt);
            }

            let issue_count = issues.len();
            info!("Verification found {} issues", issue_count);

            if issue_count == 0 {
                iterations.push(VerificationIteration {
                    iteration: iter_num,
                    issues: vec![],
                    fix_applied: false,
                    fix_error: None,
                });
                info!("Workflow passed verification on iteration {}", iter_num);
                break;
            }

            // Convergence detection: compare to previous iteration
            if let Some(prev) = previous_issue_count {
                if issue_count >= prev {
                    info!(
                        "Fix convergence: {} -> {} issues (stopping — not improving)",
                        prev, issue_count
                    );
                    warn!(
                        "Fix loop not converging: {} -> {} issues",
                        prev, issue_count
                    );
                    iterations.push(VerificationIteration {
                        iteration: iter_num,
                        issues,
                        fix_applied: false,
                        fix_error: None,
                    });
                    break;
                } else {
                    info!(
                        "Fix convergence: {} -> {} issues (continuing)",
                        prev, issue_count
                    );
                }
            }
            previous_issue_count = Some(issue_count);

            // Log issues
            for issue in &issues {
                warn!("  verification: {}", issue);
            }

            // Last iteration — record issues but don't fix (no point, nothing will verify again)
            if iter_num == max_fix_iters {
                iterations.push(VerificationIteration {
                    iteration: iter_num,
                    issues,
                    fix_applied: false,
                    fix_error: None,
                });
                warn!(
                    "Max fix iterations reached with {} remaining issues",
                    issue_count
                );
                break;
            }

            // Run fixer agent
            match run_fixer_agent(
                &workflow,
                &issues,
                &request.description,
                doctor_handle,
                generation_model,
                generation_provider,
            ) {
                Ok(fixed) => {
                    iterations.push(VerificationIteration {
                        iteration: iter_num,
                        issues,
                        fix_applied: true,
                        fix_error: None,
                    });
                    workflow = fixed;
                    // Re-apply deterministic fixes on the corrected version
                    fix_workflow(&mut workflow);
                    // Capture fixer snapshot
                    if let Ok(snapshot) = serde_json::to_value(&workflow) {
                        artifact_builder.fixer_snapshots.push(snapshot);
                    }
                }
                Err(e) => {
                    warn!("Fixer agent failed: {}", e);
                    iterations.push(VerificationIteration {
                        iteration: iter_num,
                        issues,
                        fix_applied: false,
                        fix_error: Some(e),
                    });
                    break;
                }
            }
        }
    }
    artifact_builder.verification_duration_ms =
        Some(verification_start.elapsed().as_millis() as u64);
    artifact_builder.verification_iterations = serde_json::to_value(&iterations).ok();

    // ── Hardener Agent ───────────────────────────────────────────────────
    let hardener_start = Instant::now();
    let hardener_insights = self_improve_ctx
        .as_ref()
        .map(self_improve::format_hardener_insights);
    let (mut workflow, hardening_summary, hardener_prompt) = hardener::run_hardener_agent(
        &workflow,
        &request.description,
        doctor_handle,
        conn,
        generation_model,
        generation_provider,
        &skill_registry,
        hardener_insights.as_deref(),
    );
    artifact_builder.hardener_duration_ms = Some(hardener_start.elapsed().as_millis() as u64);
    artifact_builder.hardening_summary = serde_json::to_value(&hardening_summary).ok();
    artifact_builder.hardened_json = serde_json::to_value(&workflow).ok();
    artifact_builder.hardener_prompt = hardener_prompt;

    // Re-apply deterministic fixes after hardener (e.g. strip check-group setup/completion)
    fix_workflow(&mut workflow);

    // ── Enforce required-flag discipline (Feature 7) ──────────────────────
    hardener::enforce_required_flag_discipline(&mut workflow, acceptance_criteria.as_ref());

    // ── Revision Phase (Feature 1) ───────────────────────────────────────
    let revision_start = Instant::now();
    let max_revision_cycles: u32 = 2;
    let (quality_report, revision_cycles) = {
        let mut current_cycle = 0u32;
        let final_report = loop {
            current_cycle += 1;
            let report = revision::run_quality_analysis(
                &workflow,
                acceptance_criteria.as_ref(),
                doctor_handle,
                generation_model,
                generation_provider,
            );
            if report.pass || current_cycle >= max_revision_cycles {
                break report;
            }
            match revision::run_revision_agent(
                &workflow,
                &report,
                &request.description,
                doctor_handle,
                generation_model,
                generation_provider,
            ) {
                Ok(revised) => {
                    workflow = revised;
                    fix_workflow(&mut workflow);
                    info!(
                        "Revision cycle {}: applied fixes, re-analyzing...",
                        current_cycle
                    );
                }
                Err(e) => {
                    warn!("Revision cycle {} failed: {}", current_cycle, e);
                    break report;
                }
            }
        };
        (final_report, current_cycle)
    };
    artifact_builder.revision_cycles = Some(revision_cycles);
    artifact_builder.revision_duration_ms = Some(revision_start.elapsed().as_millis() as u64);
    artifact_builder.quality_report = serde_json::to_value(&quality_report).ok();
    workflow.quality_report = serde_json::to_value(&quality_report).ok();

    // ── Quality Gate: Confidence Scoring ────────────────────────────────
    let confidence_score = {
        let mut score: f32 = 1.0;

        // Factor 1: Remaining validation errors reduce confidence
        let current_validation_errors = validate_workflow(&workflow);
        let error_count = current_validation_errors.len();
        if error_count > 0 {
            // Each error reduces score; 5+ errors drops this factor to 0
            score *= (1.0 - (error_count as f32 / 5.0).min(1.0)).max(0.0);
        }

        // Factor 2: Quality report pass/fail and finding counts
        if !quality_report.pass {
            score *= 0.6;
        }
        let critical_findings = quality_report
            .findings
            .iter()
            .filter(|f| f.severity == revision::FindingSeverity::Critical)
            .count();
        let warning_findings = quality_report
            .findings
            .iter()
            .filter(|f| f.severity == revision::FindingSeverity::Warning)
            .count();
        if critical_findings > 0 {
            score *= (1.0 - (critical_findings as f32 * 0.15).min(0.6)).max(0.0);
        }
        if warning_findings > 0 {
            score *= (1.0 - (warning_findings as f32 * 0.05).min(0.3)).max(0.0);
        }

        // Factor 3: Verification iterations used vs max (more needed = lower confidence)
        if max_fix_iters > 0 {
            let iters_used = iterations.len() as f32;
            let ratio = iters_used / max_fix_iters as f32;
            // Using all iterations means something was hard to fix
            score *= 1.0 - (ratio * 0.3);
        }

        // Factor 4: Hardening conversion count (more conversions = builder produced more prompt steps)
        if let Some(ref summary) = hardening_summary {
            if summary.converted_count > 0 {
                let hardening_penalty = (summary.converted_count as f32 * 0.03).min(0.25);
                score *= 1.0 - hardening_penalty;
            }
        }

        score.clamp(0.0, 1.0)
    };

    info!("Quality gate confidence score: {:.3}", confidence_score);
    artifact_builder.confidence_score = Some(confidence_score);

    if confidence_score < 0.3 {
        let issues_summary = quality_report
            .findings
            .iter()
            .map(|f| format!("[{:?}] {}", f.severity, f.description))
            .collect::<Vec<_>>()
            .join("; ");
        let error_msg = format!(
            "Low confidence score ({:.2}): quality gate rejected workflow. Issues: {}",
            confidence_score, issues_summary
        );
        warn!("{}", error_msg);
        artifact_builder.success = false;
        artifact_builder.error_message = Some(error_msg.clone());
        let artifact = artifact_builder.build(pipeline_start.elapsed().as_millis() as u64);
        let response = GenerateWorkflowResponse {
            workflow: None,
            validation_errors: validate_workflow(&workflow),
            success: false,
            error: Some(error_msg),
            model_used: None,
            verification_iterations: iterations,
            hardening_summary,
            discovery_calls,
            acceptance_criteria,
            quality_report: Some(quality_report),
            confidence_score: Some(confidence_score),
        };
        return (response, artifact);
    } else if confidence_score < 0.6 {
        warn!(
            "Moderate confidence score ({:.2}): proceeding with caution. {} findings remain.",
            confidence_score,
            quality_report.findings.len()
        );
    }

    // ── Inject deterministic known-issue regression steps (post-hardener) ──
    if !relevant_issues.is_empty() {
        let injected = inject_known_issue_steps(&mut workflow, &relevant_issues, conn);
        if injected > 0 {
            info!(
                "Injected {} deterministic known-issue regression steps",
                injected
            );
        }
    }

    // ── Dependency Analysis & Cost Annotations (Feature 4) ──────────────
    let dep_graph = dependency_analysis::build_dependency_graph(&workflow);
    let cost_annotations = dependency_analysis::compute_cost_annotations(&workflow);
    workflow.dependency_graph = serde_json::to_value(&dep_graph).ok();
    workflow.cost_annotations = serde_json::to_value(&cost_annotations).ok();

    // ── Apply Retry Policies (Feature 6) ──────────────────────────────────
    hardener::apply_retry_policies(&mut workflow);

    // ── Post-generation stage verification quality warnings ───────────────
    for (i, stage) in workflow.stages.iter().enumerate() {
        let has_deterministic = stage.verification_steps.iter().any(|s| {
            let step_type = s.get("type").and_then(|v| v.as_str()).unwrap_or("");
            step_type != "prompt"
        });
        if !has_deterministic && !stage.verification_steps.is_empty() {
            warn!(
                "Stage {} has no deterministic verification steps — all are prompt-based",
                i
            );
        }

        // Check if stage targets web app but lacks SDK verification
        let stage_json = serde_json::to_string(stage).unwrap_or_default();
        let targets_web = stage_json.contains("localhost:3001")
            || stage_json.contains("localhost:1420")
            || stage_json.contains("ui-bridge");
        let has_sdk_check = stage.verification_steps.iter().any(|s| {
            let cmd = s.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let step_type = s.get("type").and_then(|v| v.as_str()).unwrap_or("");
            cmd.contains("ui-bridge/sdk") || step_type == "ui_bridge"
        });
        if targets_web && !has_sdk_check {
            warn!(
                "Stage {} targets web app but has no SDK/UI Bridge verification step",
                i
            );
        }
    }

    // ── Annotate steps with skill_origin where they match known skills ────
    crate::skills::annotate_skill_origins(&mut workflow, &skill_registry);

    // ── Final structural validation ────────────────────────────────────────
    let mut validation_errors = validate_workflow(&workflow);

    // Cross-validate acceptance criteria coverage
    if let Some(ref criteria) = acceptance_criteria {
        let coverage_errors = super::validation::validate_criteria_coverage(&workflow, criteria);
        if !coverage_errors.is_empty() {
            info!(
                "Criteria coverage validation found {} issues",
                coverage_errors.len()
            );
        }
        validation_errors.extend(coverage_errors);
    }

    artifact_builder.validation_errors = serde_json::to_value(&validation_errors).ok();
    artifact_builder.final_json = serde_json::to_value(&workflow).ok();

    if !validation_errors.is_empty() {
        warn!(
            "Generated workflow has {} structural validation errors",
            validation_errors.len()
        );
    }

    let stage_step_count: usize = workflow
        .stages
        .iter()
        .map(|s| {
            s.setup_steps.len()
                + s.verification_steps.len()
                + s.agentic_steps.len()
                + s.completion_steps.len()
        })
        .sum();
    let top_level_count = workflow.setup_steps.len()
        + workflow.verification_steps.len()
        + workflow.agentic_steps.len()
        + workflow.completion_steps.len();

    info!(
        "Successfully generated workflow: {} ({} top-level steps, {} stages with {} steps, {} verification iterations)",
        workflow.name,
        top_level_count,
        workflow.stages.len(),
        stage_step_count,
        iterations.len(),
    );

    // ── Auto-promote high-confidence insights to generation rules ─────
    if let Some(c) = conn {
        match prompt_analysis::analyze_all(c) {
            Ok(insights) if !insights.is_empty() => {
                match rules::promote_insights_to_rules(c, &insights) {
                    Ok(count) if count > 0 => {
                        info!("Promoted {} insights to auto-generated rules", count);
                    }
                    Ok(_) => {}
                    Err(e) => warn!("Failed to promote insights to rules: {}", e),
                }
            }
            Ok(_) => {}
            Err(e) => warn!("Failed to analyze insights for rule promotion: {}", e),
        }

        // Record which generation rules were applied to this workflow
        let active_rules = rules::list_rules(
            c,
            &rules::ListRulesQuery {
                agent: None,
                section: None,
                status: Some("active".to_string()),
                provenance: None,
            },
        );
        if let Ok(active) = active_rules {
            if !active.is_empty() {
                rules::record_rule_applications(c, &active, Some(&workflow.id), None);
            }
        }
    }

    let artifact = artifact_builder.build(pipeline_start.elapsed().as_millis() as u64);

    let response = GenerateWorkflowResponse {
        workflow: Some(workflow),
        validation_errors,
        success: true,
        error: None,
        model_used: None,
        verification_iterations: iterations,
        hardening_summary,
        discovery_calls,
        acceptance_criteria,
        quality_report: Some(quality_report),
        confidence_score: Some(confidence_score),
    };

    (response, artifact)
}

// ============================================================================
// Builder Agent
// ============================================================================

/// Run the builder agent to generate the initial workflow JSON.
///
/// Returns `(workflow, builder_prompt)` on success.
fn run_builder_agent(
    request: &GenerateWorkflowRequest,
    discovery_context: &str,
    acceptance_criteria: Option<&specification::AcceptanceCriteria>,
    doctor_handle: Option<&DoctorHandle>,
    conn: Option<&Connection>,
    query_embedding: Option<&[f32]>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    builder_insights: Option<&str>,
) -> Result<(UnifiedWorkflow, String), Box<GenerateWorkflowResponse>> {
    let schema_context = if conn.is_some() || query_embedding.is_some() {
        build_schema_context_full(&request.description, conn, query_embedding)
    } else {
        build_schema_context()
    };

    // Resolve saved + inline context
    let mut context_section = String::new();
    if let Some(ref ids) = request.context_ids {
        if !ids.is_empty() {
            let resolved = context::resolve_contexts(ids, false, "", &[], &[]);
            if let Some(formatted) = context::format_contexts_for_prompt(&resolved) {
                context_section.push_str(&formatted);
            }
        }
    }
    if let Some(ref inline) = request.inline_context {
        if !inline.is_empty() {
            context_section.push_str(&format!(
                "<context name=\"User-Provided Context\">\n{}\n</context>\n\n",
                inline
            ));
        }
    }

    let user_prompt = format!(
        r#"## User's Request
{description}

{category_hint}

Generate a complete UnifiedWorkflow JSON that accomplishes this task.

### Quality checklist — ensure your output meets ALL of these:
- Every `command` step (plain shell) has a real, syntactically-valid `command` (no placeholders).
- Every `command` step with `check_type` has a `command` that matches its check (e.g. lint → eslint/ruff, typecheck → tsc/mypy, format → prettier/black).
- Every `command` step with `test_type` has a valid `test_type` and either a `command` or `code` field.
- Only 3 step types exist: `command`, `ui_bridge`, and `prompt`. Do NOT use `shell_command`, `api_request`, `check`, `test`, `gate`, or `spec`.
- Every `prompt` in the agentic phase has substantive, multi-sentence instructions that reference the verification results and explain exactly what to fix.
- If verification steps exist there MUST be at least one agentic `prompt` step.
- Step names are descriptive (not "Step 1", "Test", etc.).
- `working_directory` paths look like real absolute or project-relative paths (no placeholders like "/path/to/project").
- If the workflow targets a web application (localhost:3001, localhost:1420, or similar), include a setup step to connect via UI Bridge SDK (POST /ui-bridge/sdk/connect). Use SDK endpoints for element inspection and state checking instead of Playwright when possible.
- Prompt steps that need to inspect or interact with web UI should reference SDK tools (sdk_elements, sdk_snapshot, sdk_ai_execute, sdk_ai_search) rather than Playwright for registered-element interactions.
- To verify page text content (metrics, statuses, headings), use SDK content discovery (sdk_elements with contentOnly/contentTypes filters, or sdk_snapshot) instead of screenshots. Use sdk_page_refresh/sdk_page_navigate for page navigation.
- When the task involves a style refactor, UX redesign, or layout improvement, ALL information and functionality from the original page MUST be preserved. Better presentation — not removal of content. If the original has N tabs, M metrics, or a table with K columns, the redesigned version must include all of them.

Remember: Return ONLY valid JSON, no markdown code blocks or explanations."#,
        description = request.description,
        category_hint = request
            .category
            .as_ref()
            .map(|c| format!("Use category: {}", c))
            .unwrap_or_default()
    );

    // Combine all context sections: schema + discovery + user contexts + prompt
    let mut sections = vec![schema_context];
    if !discovery_context.is_empty() {
        sections.push(discovery_context.to_string());
    }
    if !context_section.is_empty() {
        sections.push(context_section.clone());
    }
    if request.include_ui_bridge_instructions.unwrap_or(true) {
        sections.push(UI_BRIDGE_INSTRUCTIONS.to_string());
    }
    if request.include_design_guidance.unwrap_or(false) {
        sections.push(FRONTEND_DESIGN_INSTRUCTIONS.to_string());
    }

    // Inject skill catalog context (built-in + user skills from DB)
    let skill_registry = SkillRegistry::with_db(conn);
    let skills_section = format_skills_for_generator(&skill_registry);
    if !skills_section.is_empty() {
        sections.push(skills_section);
    }

    // Inject builder insights from self-improvement analysis
    if let Some(insights) = builder_insights {
        if !insights.is_empty() {
            sections.push(insights.to_string());
        }
    }

    // Inject acceptance criteria section (before user prompt so builder sees it)
    if let Some(criteria) = acceptance_criteria {
        sections.push(specification::format_criteria_for_builder(criteria));
    }

    // Append criteria-specific quality rules when criteria are present
    let mut final_prompt = user_prompt.clone();
    if acceptance_criteria.is_some() {
        final_prompt.push_str(
            "\n- CRITICAL: Every acceptance criterion has a verification step with matching `criterion_id`.\n\
             - Agentic prompt steps reference which criteria they address.\n",
        );
    }

    sections.push(final_prompt);
    let full_prompt = sections.join("\n\n");

    let task_context = TaskContext::from_prompt(&full_prompt);
    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
        &full_prompt,
        &task_context,
        doctor_handle,
        model_override,
        provider_override,
        None,
        None,
        None,
        None,
    );

    if !ai_result.success {
        error!(
            "Builder agent error: {}",
            ai_result.error.as_deref().unwrap_or("Unknown error")
        );
        return Err(Box::new(GenerateWorkflowResponse {
            workflow: None,
            validation_errors: vec![],
            success: false,
            error: Some(format!(
                "AI provider error: {}",
                ai_result
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string())
            )),
            model_used: None,
            verification_iterations: vec![],
            hardening_summary: None,
            discovery_calls: vec![],
            acceptance_criteria: None,
            quality_report: None,
            confidence_score: None,
        }));
    }

    debug!("Builder agent response received, parsing JSON...");
    let json_text = extract_json_from_response(&ai_result.output);

    serde_json::from_str::<UnifiedWorkflow>(&json_text)
        .map(|w| (w, full_prompt))
        .map_err(|e| {
            error!("Failed to parse builder agent JSON: {}", e);
            warn!("Response text: {}", &json_text[..json_text.len().min(500)]);
            Box::new(GenerateWorkflowResponse {
                workflow: None,
                validation_errors: vec![],
                success: false,
                error: Some(format!(
                    "Failed to parse generated workflow: {}. The AI may have returned invalid JSON.",
                    e
                )),
                model_used: None,
                verification_iterations: vec![],
                hardening_summary: None,
                discovery_calls: vec![],
                acceptance_criteria: None,
                quality_report: None,
                confidence_score: None,
            })
        })
}

// ============================================================================
// Verification Agent
// ============================================================================

/// Run the verification agent to review a workflow for semantic issues.
///
/// Returns `(issues, prompt)` — the list of human-readable issue descriptions
/// and the prompt that was sent to the AI. An empty issues list means
/// the workflow passed all checks.
fn run_verification_agent(
    workflow: &UnifiedWorkflow,
    user_description: &str,
    discovery_context: &str,
    acceptance_criteria: Option<&specification::AcceptanceCriteria>,
    doctor_handle: Option<&DoctorHandle>,
    conn: Option<&Connection>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    skill_registry: &SkillRegistry,
    blind_spots_section: Option<&str>,
) -> (Vec<String>, String) {
    let workflow_json = match serde_json::to_string_pretty(workflow) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to serialize workflow for verification: {}", e);
            return (
                vec![format!(
                    "Internal error: could not serialize workflow: {}",
                    e
                )],
                String::new(),
            );
        }
    };

    let prompt = build_verification_prompt(
        &workflow_json,
        user_description,
        discovery_context,
        acceptance_criteria,
        conn,
        skill_registry,
        blind_spots_section,
    );
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &task_context,
        doctor_handle,
        model_override,
        provider_override,
        None,
        None,
        None,
        None,
    );

    if !ai_result.success {
        warn!(
            "Verification agent failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown")
        );
        // Treat AI failure as "no issues found" — don't block the pipeline
        return (vec![], prompt);
    }

    (parse_verification_response(&ai_result.output), prompt)
}

/// Build the prompt for the verification agent.
fn build_verification_prompt(
    workflow_json: &str,
    user_description: &str,
    discovery_context: &str,
    acceptance_criteria: Option<&specification::AcceptanceCriteria>,
    conn: Option<&Connection>,
    skill_registry: &SkillRegistry,
    blind_spots_section: Option<&str>,
) -> String {
    // Load check rules from DB if available
    let check_rules_section = if let Some(conn) = conn {
        let check_rules = rules::load_rules(conn, "verification", "check_rules");
        if !check_rules.is_empty() {
            let mut s = String::from("## What to check\n\nFor EVERY step in setup_steps, verification_steps, and completion_steps, verify:\n\n");
            for rule in &check_rules {
                s.push_str(&format!("### {}\n{}\n\n", rule.title, rule.content));
            }
            Some(s)
        } else {
            None
        }
    } else {
        None
    };

    let checks = check_rules_section.unwrap_or_else(|| FALLBACK_VERIFICATION_CHECKS.to_string());

    let discovery_section = if discovery_context.is_empty() {
        String::new()
    } else {
        format!(
            "\n## System Discovery Context\n\nUse this real system information to verify step accuracy:\n\n{}\n",
            discovery_context
        )
    };

    let skills_section = format_skills_for_generator(skill_registry);
    let skills_context = if skills_section.is_empty() {
        String::new()
    } else {
        format!(
            "\n{}\nWhen verifying steps, check that step configurations are consistent with the skill catalog above. Flag steps that claim to use a skill but have incompatible parameters or phase assignments.\n",
            skills_section
        )
    };

    let criteria_section = acceptance_criteria
        .map(specification::format_criteria_for_verifier)
        .unwrap_or_default();

    let blind_spots = blind_spots_section.unwrap_or("");

    format!(
        r#"You are a workflow verification agent for Qontinui Runner.
Your job is to review a generated UnifiedWorkflow JSON and find semantic errors in the deterministic steps — WITHOUT running anything.

{checks}

Steps match the user's original request: "{user_description}"
{discovery_section}{skills_context}{criteria_section}{blind_spots}
## Output format

If you find issues, return a JSON array of strings, one per issue. Each issue should identify the step by name/index and describe the problem clearly.

If everything looks correct, return an empty array: []

Return ONLY the JSON array. No explanations, no markdown, just the array.

Examples:
["setup_steps[0] 'Install Deps': working_directory '/path/to/project' is a placeholder, not a real path", "agentic_steps[0] 'Fix Issues': prompt content is too vague — needs specific instructions referencing verification results"]
[]

## Workflow JSON to verify

{workflow_json}"#,
        checks = checks,
        user_description = user_description,
        discovery_section = discovery_section,
        skills_context = skills_context,
        criteria_section = criteria_section,
        workflow_json = workflow_json,
    )
}

/// Comprehensive UI Bridge SDK integration instructions injected into the builder prompt
/// when `include_ui_bridge_instructions` is true (the default).
const UI_BRIDGE_INSTRUCTIONS: &str = r#"## UI Bridge SDK Integration

When the workflow's agentic prompts instruct AI to create or modify React frontend code, include these UI Bridge SDK integration instructions in the agentic prompt content so the AI agent adds proper SDK instrumentation to the code it writes.

### AutoRegisterProvider (Automatic Element Discovery)
The SDK's AutoRegisterProvider automatically discovers interactive elements and assigns stable semantic IDs at runtime.
No manual data attributes are needed — the AutoRegisterProvider discovers and registers elements in the bridge registry automatically.
IDs follow the pattern: `{type}-{label-slug}[-{context}][-{index}]` (e.g., `button-save`, `input-email-settings-form`).

### useUIElement Hook
Register interactive elements (buttons, inputs, links, toggles) with useUIElement for custom programmatic actions and state:
```tsx
import { useUIElement } from '@/hooks/useUIBridge';

const { ref } = useUIElement({
  id: 'feature-component-element',
  type: 'button', // 'button' | 'input' | 'link' | 'toggle' | 'select' | 'display'
  label: 'Human-Readable Label',
});
// Attach ref to the element: <button ref={ref}>...</button>
```

### useUIComponent Hook
Register component-level actions for automation and group child elements:
```tsx
import { useUIComponent } from '@/hooks/useUIBridge';

useUIComponent({
  id: 'feature-component',
  name: 'Component Name',
  description: 'What this component does',
  actions: [
    { id: 'submit', label: 'Submit Form', handler: handleSubmit },
    { id: 'reset', label: 'Reset Form', handler: handleReset },
  ],
  elementIds: ['feature-component-name-input', 'feature-component-submit-btn'],
});
```

### useUIState Hook
Register conditional UI states for status tracking:
```tsx
import { useUIState } from '@/hooks/useUIBridge';

useUIState({
  id: 'feature-component-loading',
  value: isLoading,
  label: 'Loading State',
});
```

### Page Spec Files (.spec.uibridge.json)
Create a `.spec.uibridge.json` file alongside each new page/route with grouped assertions covering multiple categories:
```json
{
  "version": "1.0.0",
  "description": "Specs for the feature page",
  "groups": [
    {
      "id": "feature-page-structure",
      "name": "Page Structure",
      "description": "Core layout elements of the feature page",
      "category": "element-presence",
      "assertions": [
        {
          "id": "feature-heading",
          "description": "Page heading is present",
          "category": "element-presence",
          "severity": "critical",
          "target": { "type": "search", "criteria": { "role": "heading", "textContent": "Feature" } },
          "assertionType": "exists",
          "source": "manual",
          "reviewed": true,
          "enabled": true
        }
      ],
      "source": "manual"
    },
    {
      "id": "feature-form-state",
      "name": "Form State",
      "description": "Interactive state of form elements",
      "category": "state-consistency",
      "assertions": [
        {
          "id": "feature-submit-enabled",
          "description": "Submit button is enabled when form is valid",
          "category": "state-consistency",
          "severity": "critical",
          "target": { "type": "search", "criteria": { "role": "button", "textContent": "Submit" } },
          "assertionType": "enabled",
          "condition": { "type": "exists", "target": { "type": "search", "criteria": { "idPattern": "feature-form-valid" } } },
          "source": "manual",
          "reviewed": true,
          "enabled": true
        },
        {
          "id": "feature-submit-disabled-empty",
          "description": "Submit button is disabled when form is empty",
          "category": "form-validation",
          "severity": "critical",
          "target": { "type": "search", "criteria": { "role": "button", "textContent": "Submit" } },
          "assertionType": "disabled",
          "source": "manual",
          "reviewed": true,
          "enabled": true
        },
        {
          "id": "feature-input-value",
          "description": "Name input starts empty",
          "category": "form-validation",
          "severity": "info",
          "target": { "type": "search", "criteria": { "idPattern": "feature-name-input" } },
          "assertionType": "hasValue",
          "expected": "",
          "source": "manual",
          "reviewed": true,
          "enabled": true
        }
      ],
      "source": "manual"
    }
  ],
  "metadata": {
    "component": "FeaturePage",
    "pageUrl": "/feature",
    "elementSource": "sdk"
  }
}
```

### Assertion Types & Severity

Available assertion types: exists, notExists, visible, hidden, enabled, disabled, focused, checked, unchecked, hasText, containsText, hasValue, count, attribute, hasClass, cssProperty.

Severity levels: "critical" (core functionality), "warning" (important features), "info" (nice-to-have).

Use diverse assertion types — not just "exists". Verify element states (enabled/disabled), text content (hasText/containsText), input values (hasValue), and element counts (count). Use conditions for state-dependent assertions (e.g., button disabled when form is empty).

### UIBridgeProvider
Ensure the app wraps its root with `<UIBridgeProvider>` (usually already done in the app layout).

### Workflow Verification Steps
For verification steps that check the frontend, prefer UI Bridge SDK endpoints over Playwright:
- Use `POST /ui-bridge/sdk/connect` in setup to connect to the target app
- Use `sdk_elements`, `sdk_snapshot`, `sdk_ai_search` for element discovery
- Use `sdk_ai_execute` for UI interactions
- Use `sdk_page_navigate` / `sdk_page_refresh` for navigation

These instructions enable the runner to discover, inspect, and control the frontend programmatically for automated testing and verification."#;

/// Frontend design quality guidance injected into the builder prompt
/// when `include_design_guidance` is true (opt-in, default false).
const FRONTEND_DESIGN_INSTRUCTIONS: &str = r#"## Frontend Design Quality Guidance

When generating agentic prompts that create or modify frontend UI, include these design principles to ensure high-quality, distinctive results.

### Anti-AI-Slop Rules
Avoid these hallmarks of generic AI-generated UIs:
- Default system fonts with no typographic personality
- Purple/blue gradient backgrounds used decoratively
- Scattered micro-interactions with no purpose
- Perfectly symmetric grid layouts with equal-weight cards
- Generic hero sections with stock-style placeholder text
- Overuse of rounded corners and soft shadows on everything

### Typography
- Choose distinctive fonts that match the project's personality — not just Inter/system-ui
- Pair fonts strategically: one for headings, one for body (max two families)
- Use extreme weight variation (e.g., 900 for headings, 300 for body) to create hierarchy
- Size hierarchy should be meaningful: if everything is 14-16px, nothing stands out

### Color
- Build a cohesive palette (3-5 colors max) with intentional relationships
- Define colors as CSS custom properties for consistency
- Consider emotional tone: warm palettes feel approachable, cool palettes feel professional
- Backgrounds should be intentional — not just white/gray/dark. Subtle tints create atmosphere

### Motion & Transitions
- Orchestrate transitions so elements enter in a logical sequence, not all at once
- Use staggered reveals for lists and grids (50-100ms delay between items)
- Animation should communicate state changes, not just decorate
- Prefer CSS transitions over JavaScript animation libraries for simple effects

### Spatial Composition
- Break out of symmetric grids — use asymmetric layouts for visual interest
- Negative space is a design tool, not wasted space. Let content breathe
- Think in terms of composition (focal point, flow, balance) not just constraint grids
- Vary content density across sections to create rhythm

### Visual Atmosphere
- Layer multiple shadow values for realistic depth (not just `shadow-lg`)
- Subtle textures, noise, or gradients add warmth and reduce flatness
- Consider backdrop-filter (blur, saturate) for glass-morphism effects where appropriate
- Depth should serve hierarchy: elevated elements are more important

### Aesthetic Directions (pick one that fits the project)
- **Brutalist**: Raw, bold, intentionally rough. Monospace fonts, harsh contrasts, visible structure
- **Minimalist**: Maximum restraint. Generous whitespace, single accent color, typography-driven
- **Retro-futuristic**: Neon accents, dark backgrounds, tech-inspired typography, scan lines
- **Editorial**: Magazine-like layouts, dramatic type scale, asymmetric image placement
- **Organic**: Soft curves, natural color palettes, hand-drawn elements, flowing layouts
- **Industrial**: Exposed structure, monochrome with accent, dense information display

### Verification Integration
Pair design implementation with `sdk_design_evaluate` verification steps to measure spacing consistency, color usage, typography hierarchy, and overall design quality scores.

### Preservation Constraint
When redesigning or restyling existing pages, ALL original information, features, data fields, and functionality MUST be retained. Design improvements mean better presentation of the same content — never removal of content."#;

/// Hardcoded fallback verification checks, used when no DB connection is available.
const FALLBACK_VERIFICATION_CHECKS: &str = r#"## What to check

Only 3 step types are valid: `command`, `ui_bridge`, and `prompt`. If any step uses a different type (e.g. `shell_command`, `api_request`, `check`, `test`, `gate`, `spec`), flag it immediately.

For EVERY step in setup_steps, verification_steps, and completion_steps, verify:

### command step validation (plain shell mode — no check_type, no test_type)
`command` is a real, syntactically valid shell command (not a placeholder like "echo TODO" or "/path/to/script"). `working_directory`, if present, looks like a real path. `timeout_seconds` is reasonable. `fail_on_error` is appropriate.

### command step validation (check mode — check_type is set)
`check_type` and `command` are consistent: "lint" → linter, "typecheck" → type checker, "format" → formatter check, "analyze" → static analysis, "security" → security scanner, "custom_command" → any command. `command` is non-empty and syntactically valid. Step type MUST be `command` (not `check`).

### command step validation (test mode — test_type is set)
Has either `command` (for repository/custom_command) or `code` (for playwright/python). `test_type` is one of: playwright, qontinui_vision, python, repository, custom_command. The command/code looks substantive (not a placeholder). Step type MUST be `command` (not `test`).

### ui_bridge step validation
`action` is one of: navigate, execute, assert, snapshot. Required fields vary by action: navigate needs `url`, execute needs `instruction`, assert needs `target` and `assert_type`. `timeout_ms` is reasonable if set.

### prompt step quality
Content is substantive — at least 2 sentences with specific instructions. Agentic prompts reference verification results and describe what to fix. Not a generic placeholder like "Fix the errors" or "Do the task".

### UI Bridge SDK usage
If the workflow targets a web app but does NOT include a setup step to connect via UI Bridge SDK (POST to /ui-bridge/sdk/connect), flag it. If the workflow uses Playwright for simple element inspection when SDK endpoints could be used instead, flag it. If agentic prompt steps mention web UI interaction but don't reference SDK tools, flag it.

### Agentic-verification correspondence
For EACH prompt step in agentic_steps, there MUST be at least one corresponding deterministic verification step that can detect whether that agentic step's work succeeded. Tab/section existence checks do NOT count as adequate verification for the tab's CONTENT or FUNCTIONALITY.
Note: A file-existence check (e.g., `test ! -f`) alone is NOT adequate verification for removing a web page/route — the built/deployed application may still serve the route even after source file deletion. For removal tasks targeting web apps, at least one verification step must be a runtime check (UI Bridge navigate + assertion, or HTTP request to the removed URL).

### Cross-step and structural checks
If there are verification steps, there should be at least one agentic prompt step. Setup steps should logically prepare for what verification checks. Step names are descriptive (not "Step 1", "Test", "Check"). No duplicate step IDs.

### Multi-stage workflow validation (if `stages` array is present)
- Every stage MUST have a unique, valid UUID v4 `id`
- Every stage MUST have at least one deterministic verification step (same rules as top-level verification)
- When `stages` is non-empty, top-level step arrays (setup_steps, verification_steps, agentic_steps, completion_steps) should be empty
- Per-stage `provider` and `model`, if set, must be valid values (e.g., claude_cli, claude_api, gemini_cli, gemini_api)
- Stages should only be used when the task genuinely has 2+ distinct phases with different verification criteria — don't use stages for single-phase tasks
- Each stage's agentic-verification correspondence must hold independently"#;

/// Parse the verification agent's response into a list of issue strings.
fn parse_verification_response(response: &str) -> Vec<String> {
    let json_text = extract_json_array_from_response(response);

    match serde_json::from_str::<Vec<String>>(&json_text) {
        Ok(issues) => issues,
        Err(e) => {
            debug!(
                "Could not parse verification response as JSON array: {} — treating as text",
                e
            );
            // Fall back: if the AI returned free-text instead of JSON, split by lines
            // and filter out empty / boilerplate lines
            let lines: Vec<String> = response
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| {
                    !l.is_empty()
                        && !l.starts_with("```")
                        && l != "[]"
                        && !l.starts_with("No issues")
                        && !l.to_lowercase().starts_with("everything looks")
                })
                .collect();
            if lines.is_empty() {
                vec![] // treat as "no issues"
            } else {
                lines
            }
        }
    }
}

/// Extract a JSON array from a response that might be wrapped in markdown.
fn extract_json_array_from_response(response: &str) -> String {
    let trimmed = response.trim();

    // Try markdown code block
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            return trimmed[start + 7..start + 7 + end].trim().to_string();
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        let json_start = after.find('\n').map(|p| p + 1).unwrap_or(0);
        if let Some(end) = after[json_start..].find("```") {
            return after[json_start..json_start + end].trim().to_string();
        }
    }

    // Try to find a JSON array directly
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            if end > start {
                return trimmed[start..=end].to_string();
            }
        }
    }

    trimmed.to_string()
}

// ============================================================================
// Fixer Agent
// ============================================================================

/// Run the fixer agent, returning a corrected workflow or an error message.
fn run_fixer_agent(
    workflow: &UnifiedWorkflow,
    issues: &[String],
    user_description: &str,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> Result<UnifiedWorkflow, String> {
    let workflow_json = serde_json::to_string_pretty(workflow)
        .map_err(|e| format!("Failed to serialize workflow: {}", e))?;

    let prompt = build_fix_prompt(&workflow_json, issues, user_description);
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &task_context,
        doctor_handle,
        model_override,
        provider_override,
        None,
        None,
        None,
        None,
    );

    if !ai_result.success {
        return Err(format!(
            "Fixer AI error: {}",
            ai_result.error.unwrap_or_else(|| "unknown".to_string())
        ));
    }

    let json_text = extract_json_from_response(&ai_result.output);
    serde_json::from_str::<UnifiedWorkflow>(&json_text)
        .map_err(|e| format!("Fixer produced invalid JSON: {}", e))
}

/// Build the prompt for the fixer agent.
fn build_fix_prompt(workflow_json: &str, issues: &[String], user_description: &str) -> String {
    let issues_text = issues
        .iter()
        .enumerate()
        .map(|(i, issue)| format!("{}. {}", i + 1, issue))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are a workflow fixer agent for Qontinui Runner.

## Your task

A verification agent found issues in the workflow below. Fix ALL of them and return the corrected, complete UnifiedWorkflow JSON.

## Rules
- Fix every listed issue. Do NOT skip any.
- Preserve the overall structure, step ordering, IDs, and intent of the workflow.
- Do NOT add new steps unless an issue specifically requires it (e.g., "missing agentic step").
- Do NOT remove steps unless an issue specifically says to.
- All UUIDs must be valid v4 format.
- All `phase` fields must match the array they're in.
- Return ONLY valid JSON — no markdown, no explanations.

## The user's original request
{user_description}

## Issues to fix
{issues_text}

## Current workflow JSON
{workflow_json}"#,
        user_description = user_description,
        issues_text = issues_text,
        workflow_json = workflow_json,
    )
}

// ============================================================================
// Helpers
// ============================================================================

/// Apply the request's override options onto the parsed workflow.
fn apply_request_options(workflow: &mut UnifiedWorkflow, request: &GenerateWorkflowRequest) {
    if let Some(ref category) = request.category {
        workflow.category = category.clone();
    }
    if let Some(ref tags) = request.tags {
        workflow.tags = tags.clone();
    }
    if let Some(max_iterations) = request.max_iterations {
        workflow.max_iterations = max_iterations;
    }
    if let Some(ref provider) = request.provider {
        workflow.provider = Some(provider.clone());
    }
    if let Some(ref model) = request.model {
        workflow.model = Some(model.clone());
    }
    if let Some(skip_ai_summary) = request.skip_ai_summary {
        workflow.skip_ai_summary = skip_ai_summary;
    }
    if let Some(ref log_source) = request.log_source_selection {
        use crate::unified_workflows::LogSourceSelection;
        workflow.log_source_selection =
            if log_source == "default" || log_source == "ai" || log_source == "all" {
                LogSourceSelection::Mode(log_source.clone())
            } else {
                LogSourceSelection::Profile {
                    profile_id: log_source.clone(),
                }
            };
    }
    if let Some(ref prompt_template) = request.prompt_template {
        workflow.prompt_template = Some(prompt_template.clone());
    }
    if let Some(auto_include) = request.auto_include_contexts {
        workflow.auto_include_contexts = auto_include;
    }
    if let Some(reflection_mode) = request.reflection_mode {
        workflow.reflection_mode = reflection_mode;
    }
    if let Some(ref overrides) = request.model_overrides {
        workflow.model_overrides = overrides.clone();
    }
}

/// Extract JSON from AI response, handling markdown code blocks.
///
/// AI models often wrap JSON output in markdown code fences or add
/// explanatory text before/after. This function extracts the JSON
/// content by trying, in order:
/// 1. JSON in a ```json code block
/// 2. JSON in a generic ``` code block
/// 3. First `{` to last `}` in the text
/// 4. Original text (trimmed) as fallback
pub fn extract_json_from_response(response: &str) -> String {
    let trimmed = response.trim();

    // Try to find JSON in markdown code block
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            return trimmed[start + 7..start + 7 + end].trim().to_string();
        }
    }

    // Try to find JSON in generic code block
    if let Some(start) = trimmed.find("```") {
        let after_backticks = &trimmed[start + 3..];
        let json_start = if let Some(newline_pos) = after_backticks.find('\n') {
            newline_pos + 1
        } else {
            0
        };
        if let Some(end) = after_backticks[json_start..].find("```") {
            return after_backticks[json_start..json_start + end]
                .trim()
                .to_string();
        }
    }

    // Try to find JSON object directly
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return trimmed[start..=end].to_string();
            }
        }
    }

    trimmed.to_string()
}

// ============================================================================
// Known-Issue Verification Template Write-Back
// ============================================================================

/// Normalize a string to kebab-case for matching (lowercase, non-alphanumeric → hyphens,
/// collapse consecutive hyphens, trim leading/trailing hyphens).
fn to_kebab_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_hyphen = true; // start true to skip leading hyphens
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            result.push('-');
            prev_hyphen = true;
        }
    }
    // trim trailing hyphen
    if result.ends_with('-') {
        result.pop();
    }
    result
}

/// After the specification agent produces acceptance criteria, write back a
/// `verification_step_template` to each known issue that:
///   (a) doesn't already have one, and
///   (b) can be matched to a generated criterion.
fn write_back_verification_templates(
    conn: &Connection,
    issues: &[crate::known_issues::KnownIssue],
    criteria: &specification::AcceptanceCriteria,
) {
    use crate::known_issues::{storage::update_known_issue, UpdateKnownIssueRequest};

    for issue in issues {
        // Skip issues that already have a template
        if issue.verification_step_template.is_some() {
            continue;
        }

        let title_kebab = to_kebab_case(&issue.title);
        let title_lower = issue.title.to_lowercase();

        // Find a matching criterion
        let matched = criteria.criteria.iter().find(|c| {
            // criterion ID contains the kebab-cased title
            if !title_kebab.is_empty() && c.id.contains(&title_kebab) {
                return true;
            }
            // criterion description or hint contains the original title (case-insensitive)
            let desc_lower = c.description.to_lowercase();
            let hint_lower = c.verification_hint.to_lowercase();
            if !title_lower.is_empty()
                && (desc_lower.contains(&title_lower) || hint_lower.contains(&title_lower))
            {
                return true;
            }
            false
        });

        let criterion = match matched {
            Some(c) => c,
            None => continue,
        };

        // Build the verification_step_template based on the criterion's method
        let template = match criterion.method {
            specification::VerificationMethod::UiBridge => serde_json::json!({
                "type": "ui_bridge",
                "ui_bridge_action": "snapshot_assert",
                "ui_bridge_target": criterion.verification_hint,
            }),
            specification::VerificationMethod::Command => serde_json::json!({
                "type": "command",
                "command": criterion.verification_hint,
            }),
            specification::VerificationMethod::Test => serde_json::json!({
                "type": "command",
                "command": criterion.verification_hint,
                "test_type": true,
            }),
            specification::VerificationMethod::Manual => continue,
        };

        let req = UpdateKnownIssueRequest {
            title: None,
            description: None,
            category: None,
            scope_type: None,
            scope_value: None,
            scope_tags: None,
            detection_method: None,
            detection_config: None,
            pattern_template_id: None,
            reproduction_context: None,
            trigger_conditions: None,
            severity: None,
            status: None,
            confidence: None,
            verification_hint: None,
            verification_step_template: Some(template),
        };

        match update_known_issue(conn, &issue.id, &req) {
            Ok(_) => info!(
                "Wrote verification_step_template to known issue '{}' ({})",
                issue.title, issue.id
            ),
            Err(e) => warn!(
                "Failed to write verification_step_template to issue '{}': {}",
                issue.id, e
            ),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_from_code_block() {
        let response = r#"Here's the workflow:

```json
{"name": "test"}
```

Hope this helps!"#;

        let json = extract_json_from_response(response);
        assert_eq!(json, r#"{"name": "test"}"#);
    }

    #[test]
    fn test_extract_json_direct() {
        let response = r#"{"name": "test", "id": "123"}"#;
        let json = extract_json_from_response(response);
        assert_eq!(json, r#"{"name": "test", "id": "123"}"#);
    }

    #[test]
    fn test_extract_json_with_text() {
        let response =
            r#"Sure, here is the workflow: {"name": "test"} Let me know if you need changes."#;
        let json = extract_json_from_response(response);
        assert_eq!(json, r#"{"name": "test"}"#);
    }

    #[test]
    fn test_extract_json_array_from_response() {
        let response = r#"```json
["issue 1", "issue 2"]
```"#;
        let json = extract_json_array_from_response(response);
        assert_eq!(json, r#"["issue 1", "issue 2"]"#);
    }

    #[test]
    fn test_extract_json_array_direct() {
        let response = r#"["issue 1"]"#;
        let json = extract_json_array_from_response(response);
        assert_eq!(json, r#"["issue 1"]"#);
    }

    #[test]
    fn test_parse_verification_empty() {
        let issues = parse_verification_response("[]");
        assert!(issues.is_empty());
    }

    #[test]
    fn test_parse_verification_issues() {
        let response = r#"["bad command in step 0", "missing url"]"#;
        let issues = parse_verification_response(response);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0], "bad command in step 0");
    }

    #[test]
    fn test_parse_verification_no_issues_text() {
        // AI might respond with free text instead of JSON
        let issues = parse_verification_response("No issues found. Everything looks good.");
        assert!(issues.is_empty());
    }
}
