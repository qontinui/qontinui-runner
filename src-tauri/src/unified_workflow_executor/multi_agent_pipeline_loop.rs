//! Multi-Agent Pipeline Loop — DAG-structured workflow architecture.
//!
//! Specialized agents in a DAG-structured pipeline instead of a monolithic verify->fix loop.

use tracing::{debug, info, warn};

use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;

use super::loop_controller::LoopController;
use super::types::{LoopConfig, LoopResult};

/// Query token usage from the database for a specific execution_id and iteration.
///
/// Returns (input_tokens, output_tokens). Falls back to (0, 0) on error.
fn query_iteration_tokens(
    db: &crate::database::CheckpointDb,
    execution_id: &str,
    iteration: u32,
) -> (u64, u64) {
    match db.get_phase_token_usage(execution_id) {
        Ok(rows) => {
            let mut input = 0u64;
            let mut output = 0u64;
            for row in &rows {
                if row.iteration == Some(iteration) {
                    input += row.input_tokens;
                    output += row.output_tokens;
                }
            }
            (input, output)
        }
        Err(e) => {
            warn!("Failed to query phase token usage: {}", e);
            (0, 0)
        }
    }
}

impl LoopController {
    /// Multi-Agent Pipeline architecture: specialized agents in a DAG-structured pipeline.
    ///
    /// Instead of a monolithic verify→fix loop, this architecture:
    /// 1. Analyzes specs into acceptance criteria with dependency ordering (Spec Analyst)
    /// 2. Builds an execution DAG from criteria dependencies (deterministic)
    /// 3. Captures UI state via UI Bridge (Snapshot Agent)
    /// 4. Maps criteria to code locations (Locator Agent)
    /// 5. Assigns independent DAG subtrees to parallel Implementer agents
    /// 6. Verifies each subtree with isolated Verifier agents
    /// 7. Runs integration verification to catch cross-subtree regressions
    ///
    /// Each agent produces a typed, serialized trace (PipelineAgentTrace) that
    /// enables per-agent autoresearch benchmarking and replay.
    pub(super) async fn run_multi_agent_pipeline_loop(
        &self,
        config: &mut LoopConfig,
        verification_steps: &[ExecutionStepConfig],
        has_agentic_steps: bool,
        agentic_steps: &[ExecutionStepConfig],
        _all_step_results: &mut Vec<crate::step_executor::StepExecutionResult>,
        logger: &StepEventLogger,
    ) -> LoopResult {
        use crate::autoresearch::agentic_verification::*;

        let pipeline_config = config
            .multi_agent_pipeline_config
            .clone()
            .unwrap_or_default();

        // Load active prompt variants from the registry (populated by meta-optimizer).
        // If a variant exists for an agent type, it can be used to customize that agent's behavior.
        // Currently a no-op until the meta-optimizer populates the registry.
        let mut active_prompt_variants: std::collections::HashMap<String, String> = {
            let mut variants = std::collections::HashMap::new();
            for agent_type in &["spec_analyst", "locator", "implementer", "verifier"] {
                if let Ok(Some(variant)) = crate::meta_optimizer::prompt_registry::get_active_prompt(
                    &self.checkpoint_db,
                    agent_type,
                ) {
                    debug!(
                        "MULTI-AGENT-PIPELINE: Using prompt variant '{}' v{} for {}",
                        variant.variant_name, variant.version, agent_type
                    );
                    variants.insert(agent_type.to_string(), variant.prompt_content);
                }
            }
            variants
        };
        if !active_prompt_variants.is_empty() {
            info!(
                "MULTI-AGENT-PIPELINE: {} active prompt variant(s) loaded from registry",
                active_prompt_variants.len()
            );
        }

        // Check for active canary rollouts targeting pipeline agents.
        // If a canary is active, probabilistically decide whether this run uses the canary config.
        let active_canary: Option<(String, String)> = {
            // (canary_id, recommendation_id)
            match crate::meta_optimizer::canary::get_active_canaries(&self.checkpoint_db) {
                Ok(canaries) => canaries.into_iter().find_map(|c| {
                    if crate::meta_optimizer::canary::should_apply_canary(
                        &self.checkpoint_db,
                        &c.recommendation_id,
                    ) {
                        info!(
                            "MULTI-AGENT-PIPELINE: Canary rollout {} active for this run ({}%)",
                            c.id, c.percentage
                        );
                        Some((c.id, c.recommendation_id))
                    } else {
                        None
                    }
                }),
                Err(_) => None,
            }
        };
        let is_canary_run = active_canary.is_some();

        // For canary runs, load the recommendation's prompt overrides and inject them
        // into the active_prompt_variants map, replacing any existing variants for those agents.
        // For baseline runs (non-canary), the existing prompt variants remain as-is.
        if let Some((_, ref rec_id)) = active_canary {
            match crate::meta_optimizer::canary::get_canary_prompt_overrides(
                &self.checkpoint_db,
                rec_id,
            ) {
                Ok(overrides) => {
                    for (agent_type, prompt_content) in overrides {
                        info!(
                            "MULTI-AGENT-PIPELINE: Canary injecting prompt override for {}",
                            agent_type
                        );
                        active_prompt_variants.insert(agent_type, prompt_content);
                    }
                }
                Err(e) => {
                    warn!(
                        "MULTI-AGENT-PIPELINE: Failed to load canary prompt overrides: {}",
                        e
                    );
                }
            }
        }

        info!(
            "MULTI-AGENT-PIPELINE: Starting (max_parallel={}, max_retries={}, dag_strategy={}, level_strategy={}, max_total_iterations={})",
            pipeline_config.max_parallel_implementers,
            pipeline_config.max_retries_per_subtree,
            pipeline_config.dag_strategy,
            pipeline_config.level_strategy,
            pipeline_config.max_total_iterations,
        );

        let mut total_iterations: u32 = 0;
        let mut agent_traces: Vec<PipelineAgentTrace> = Vec::new();

        // ── Phase 1: Spec Analysis ──────────────────────────────────────
        // The Spec Analyst agent parses spec files into structured acceptance
        // criteria with dependency metadata. For now, we derive criteria from
        // the verification steps (which are already built from specs).
        info!("MULTI-AGENT-PIPELINE: Phase 1 — Spec Analysis");

        let analyst_start = std::time::Instant::now();
        let criteria: Vec<PipelineAcceptanceCriterion> = verification_steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                let step_name = step.name.clone().unwrap_or_else(|| format!("step_{}", i));
                PipelineAcceptanceCriterion {
                    id: format!("criterion_{}", i),
                    spec_assertion_id: step_name.clone(),
                    spec_group_id: step.id.clone().unwrap_or_default(),
                    description: step_name,
                    criterion_type: "deterministic".to_string(),
                    verification_method: step.step_type.clone(),
                    depends_on: vec![],
                    target_elements: vec![],
                    estimated_complexity: "simple".to_string(),
                    severity: "critical".to_string(),
                    enabled: true,
                }
            })
            .collect();
        let analyst_duration = analyst_start.elapsed().as_millis() as u64;

        agent_traces.push(PipelineAgentTrace {
            agent_type: "spec_analyst".to_string(),
            agent_id: "spec_analyst_0".to_string(),
            run_id: config.execution_id.clone(),
            input_snapshot: serde_json::json!({
                "verification_step_count": verification_steps.len(),
            }),
            output_snapshot: serde_json::json!({
                "criteria_count": criteria.len(),
            }),
            config: pipeline_config.spec_analyst.clone(),
            duration_ms: analyst_duration,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: 0.0,
            downstream_success: None,
            output_quality_score: None,
        });

        if criteria.is_empty() {
            info!("MULTI-AGENT-PIPELINE: No criteria derived — nothing to do");
            return LoopResult {
                iterations_run: 0,
                verification_passed: true,
                max_iterations_reached: false,
                critical_failure: false,
                was_stopped: false,
                unfixable_errors: false,
                iteration_results: vec![],
            };
        }

        info!(
            "MULTI-AGENT-PIPELINE: Spec Analyst produced {} criteria in {}ms",
            criteria.len(),
            analyst_duration
        );

        // ── Phase 2: DAG Construction (deterministic) ───────────────────
        info!(
            "MULTI-AGENT-PIPELINE: Phase 2 — DAG Construction (strategy={})",
            pipeline_config.dag_strategy
        );

        // Build a single subtree containing all criteria (flat DAG for initial impl).
        // The DAG builder will be enhanced to parse depends_on from analyst output.
        let dag = ExecutionDAG {
            nodes: criteria
                .iter()
                .map(|c| {
                    (
                        c.id.clone(),
                        DAGNode {
                            criterion_id: c.id.clone(),
                            dependencies: c.depends_on.clone(),
                            dependents: vec![],
                            level: 0,
                            subtree_id: "subtree_0".to_string(),
                        },
                    )
                })
                .collect(),
            roots: criteria.iter().map(|c| c.id.clone()).collect(),
            levels: vec![criteria.iter().map(|c| c.id.clone()).collect()],
            subtrees: vec![DAGSubtree {
                id: "subtree_0".to_string(),
                root_criteria: criteria.iter().map(|c| c.id.clone()).collect(),
                all_criteria: criteria.iter().map(|c| c.id.clone()).collect(),
                max_level: 0,
                estimated_complexity: "moderate".to_string(),
            }],
        };

        info!(
            "MULTI-AGENT-PIPELINE: DAG has {} subtree(s), {} level(s), {} total nodes",
            dag.subtrees.len(),
            dag.levels.len(),
            dag.nodes.len()
        );

        // ── Phase 3: Snapshot ───────────────────────────────────────────
        info!("MULTI-AGENT-PIPELINE: Phase 3 — UI Snapshot (delegated to verification steps)");

        // ── Phase 4: Code Location ─────────────────────────────────────
        info!("MULTI-AGENT-PIPELINE: Phase 4 — Code Location");

        let located_criteria: Vec<LocatedCriterion> = if pipeline_config
            .locator
            .max_tokens
            .unwrap_or(0)
            > 0
        {
            let locator_start = std::time::Instant::now();

            // Get the L0 directory tree (directory structure only) for the locator.
            // The locator can use tool use to list files in specific directories.
            let project_path = config
                .project_path
                .clone()
                .unwrap_or_else(|| ".".to_string());
            let file_tree = get_file_tree_l0(&project_path);

            // Build the locator prompt
            let criteria_text = criteria
                .iter()
                .map(|c| {
                    format!(
                        "- {} (id: {}): {}",
                        c.spec_assertion_id, c.id, c.description
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");

            let mut locator_prompt = format!(
                r#"You are a code locator agent. Given acceptance criteria and a project directory structure, identify which files are most relevant to each criterion.

## Acceptance Criteria
{criteria_text}

## Project Directory Structure (L0 — directories only)
{file_tree}

This shows only the directory tree, not individual files. Use tool use (e.g., `ls` or `find`) to list files within directories that look relevant to the criteria. Focus on directories whose names match the domain of each criterion.

## Instructions
1. Review the directory structure above and identify 2-4 directories most relevant to each criterion.
2. Use tool use to list files within those directories.
3. For each criterion, identify 1-5 files that are most likely to need changes or inspection.
4. Rate your confidence from 0.0 to 1.0 in each mapping.

Output JSON (and nothing else):

```json
[
  {{
    "criterion_id": "<the criterion id>",
    "spec_assertion_id": "<the spec_assertion_id>",
    "description": "<the criterion description>",
    "target_files": [
      {{"path": "src/components/MyComponent.tsx", "relevance": "primary"}}
    ],
    "related_files": [
      {{"path": "src/types/config.ts", "relevance": "type_definition"}}
    ],
    "confidence": 0.8
  }}
]
```

Only output the JSON array, nothing else."#,
                criteria_text = criteria_text,
                file_tree = file_tree,
            );

            // Inject active prompt variant for locator if available
            if let Some(variant_prompt) = active_prompt_variants.get("locator") {
                locator_prompt.push_str(&format!(
                    "\n\n## Additional Instructions\n{}",
                    variant_prompt
                ));
            }

            let locator_step = ExecutionStepConfig {
                step_type: "prompt".to_string(),
                name: Some("Locator: Map criteria to code locations".to_string()),
                prompt_content: Some(locator_prompt.clone()),
                ..Default::default()
            };

            // Run the locator through a single agentic iteration
            let (locator_outcome, _) = self
                .agentic_executor
                .run_agentic(
                    config,
                    0, // iteration
                    &locator_prompt,
                    true,
                    &[locator_step],
                    logger,
                )
                .await;

            // Parse the locator output into LocatedCriterion structs
            let parsed: Vec<LocatedCriterion> = if let Some(output) = locator_outcome.output() {
                // Try to find a JSON array in the output
                if let Some(start) = output.find('[') {
                    if let Some(end) = output.rfind(']') {
                        // Parse intermediate representation since the AI output schema
                        // differs from our LocatedCriterion struct (which embeds the full criterion)
                        #[derive(serde::Deserialize)]
                        struct LocatorOutputEntry {
                            criterion_id: String,
                            #[serde(default)]
                            target_files: Vec<CodeLocation>,
                            #[serde(default)]
                            related_files: Vec<CodeLocation>,
                            #[serde(default)]
                            confidence: f64,
                        }

                        match serde_json::from_str::<Vec<LocatorOutputEntry>>(&output[start..=end])
                        {
                            Ok(entries) => {
                                entries
                                    .into_iter()
                                    .filter_map(|entry| {
                                        // Find the matching criterion to embed in the LocatedCriterion
                                        criteria.iter().find(|c| c.id == entry.criterion_id).map(
                                            |c| LocatedCriterion {
                                                criterion: c.clone(),
                                                target_files: entry.target_files,
                                                related_files: entry.related_files,
                                                confidence: entry.confidence,
                                            },
                                        )
                                    })
                                    .collect()
                            }
                            Err(e) => {
                                warn!("MULTI-AGENT-PIPELINE: Failed to parse locator JSON: {}", e);
                                Vec::new()
                            }
                        }
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

            info!(
                "MULTI-AGENT-PIPELINE: Locator identified {} criteria locations",
                parsed.len()
            );

            let locator_duration = locator_start.elapsed().as_millis() as u64;

            // Query token usage recorded during the locator's run_agentic call (iteration=0).
            // Fall back to tokens carried on AgenticOutcome when DB has no records.
            let (mut locator_tokens_in, mut locator_tokens_out) =
                query_iteration_tokens(&self.checkpoint_db, &config.execution_id, 0);
            if locator_tokens_in == 0 && locator_tokens_out == 0 {
                let (ot_in, ot_out) = locator_outcome.token_usage();
                locator_tokens_in = ot_in.unwrap_or(0);
                locator_tokens_out = ot_out.unwrap_or(0);
            }
            let locator_model = config
                .resolve_model_for_phase("agentic")
                .unwrap_or_else(|| "claude-cli".to_string());
            let locator_cost = crate::ai_pricing::calculate_cost_usd(
                locator_tokens_in,
                locator_tokens_out,
                &locator_model,
            );
            if locator_tokens_in > 0 || locator_tokens_out > 0 {
                info!(
                    "MULTI-AGENT-PIPELINE: Locator tokens: in={}, out={}, cost=${:.4}",
                    locator_tokens_in, locator_tokens_out, locator_cost
                );
            }

            agent_traces.push(PipelineAgentTrace {
                agent_type: "locator".to_string(),
                agent_id: "locator_0".to_string(),
                run_id: config.execution_id.clone(),
                input_snapshot: serde_json::json!({
                    "criteria_count": criteria.len(),
                    "file_tree_lines": file_tree.lines().count(),
                    "file_tree_tier": "l0_directories",
                }),
                output_snapshot: serde_json::json!({
                    "located_criteria_count": parsed.len(),
                }),
                config: pipeline_config.locator.clone(),
                duration_ms: locator_duration,
                tokens_in: locator_tokens_in as u32,
                tokens_out: locator_tokens_out as u32,
                cost_usd: locator_cost,
                downstream_success: None,
                output_quality_score: None,
            });

            parsed
        } else {
            info!("MULTI-AGENT-PIPELINE: Phase 4 — Code Location (skipped, locator.max_tokens=0)");
            Vec::new()
        };

        // ── Phase 5: Implementation + Verification per subtree ──────────
        info!("MULTI-AGENT-PIPELINE: Phase 5 — Implementation + Verification");

        let mut subtree_results: Vec<SubtreeResult> = Vec::new();

        for subtree in &dag.subtrees {
            if self.is_task_stopped(&config.execution_id) {
                info!("MULTI-AGENT-PIPELINE: Stopped by user");
                // Sum tokens/cost from traces collected so far
                let stopped_total_tokens: u64 = agent_traces
                    .iter()
                    .map(|t| t.tokens_in as u64 + t.tokens_out as u64)
                    .sum();
                let stopped_total_cost: f64 = agent_traces.iter().map(|t| t.cost_usd).sum();
                let result = MultiAgentPipelineResult {
                    total_iterations,
                    goal_achieved: false,
                    was_stopped: true,
                    max_iterations_reached: false,
                    subtree_results,
                    integration_result: None,
                    agent_traces,
                    dag: dag.clone(),
                    total_criteria: criteria.len() as u32,
                    passed_criteria: 0,
                    total_tokens: stopped_total_tokens,
                    total_cost_usd: stopped_total_cost,
                };
                return result.to_loop_result();
            }

            info!(
                "MULTI-AGENT-PIPELINE: Processing subtree '{}' ({} criteria)",
                subtree.id,
                subtree.all_criteria.len()
            );

            let mut subtree_level_results: Vec<SubtreeLevelResult> = Vec::new();
            let mut subtree_all_passed = true;
            let mut retries_used: u32 = 0;

            // Process levels within this subtree
            for (level_idx, level_criteria_ids) in dag.levels.iter().enumerate() {
                // Filter to criteria in this subtree
                let level_criteria: Vec<&str> = level_criteria_ids
                    .iter()
                    .filter(|id| subtree.all_criteria.contains(id))
                    .map(|s| s.as_str())
                    .collect();

                if level_criteria.is_empty() {
                    continue;
                }

                // Retry loop for this level: implementer + verifier, with retries on failure
                let mut level_attempt: u32 = 0;
                let mut level_passed = false;
                let mut level_criterion_results: Vec<PipelineCriterionResult> = Vec::new();
                let mut last_implementer_trace: Option<PipelineAgentTrace> = None;
                let mut last_verifier_trace: Option<PipelineAgentTrace> = None;
                let mut prior_failure_feedback: Option<String> = None;

                loop {
                    if total_iterations >= pipeline_config.max_total_iterations {
                        info!(
                            "MULTI-AGENT-PIPELINE: Total iteration budget ({}) exhausted",
                            pipeline_config.max_total_iterations
                        );
                        subtree_all_passed = false;
                        break;
                    }

                    total_iterations += 1;
                    level_attempt += 1;

                    // Build failure context, including feedback from prior attempt if retrying
                    let mut failure_context = format!(
                        "Multi-Agent Pipeline: Implement criteria at level {} for subtree '{}'. Criteria: {}",
                        level_idx,
                        subtree.id,
                        level_criteria.join(", ")
                    );
                    if let Some(ref feedback) = prior_failure_feedback {
                        failure_context.push_str(&format!(
                            "\n\n## Previous Attempt Failed (attempt {}/{})\n{}",
                            level_attempt - 1,
                            pipeline_config.max_retries_per_subtree + 1,
                            feedback
                        ));
                    }

                    // Inject active prompt variant for implementer if available
                    if let Some(variant_prompt) = active_prompt_variants.get("implementer") {
                        failure_context.push_str(&format!(
                            "\n\n## Agent Instructions (from optimized prompt)\n{}",
                            variant_prompt
                        ));
                    }

                    // Add location context from the Locator agent if available
                    if !located_criteria.is_empty() {
                        failure_context.push_str("\n\n## Code Locations (from Locator Agent)\n");
                        for lc in &located_criteria {
                            if level_criteria.contains(&lc.criterion.id.as_str()) {
                                failure_context.push_str(&format!(
                                    "### {} (confidence: {:.0}%)\n",
                                    lc.criterion.id,
                                    lc.confidence * 100.0
                                ));
                                if !lc.target_files.is_empty() {
                                    failure_context.push_str("Target files:\n");
                                    for f in &lc.target_files {
                                        failure_context.push_str(&format!(
                                            "- `{}` ({})\n",
                                            f.path, f.relevance
                                        ));
                                    }
                                }
                                if !lc.related_files.is_empty() {
                                    failure_context.push_str("Related files:\n");
                                    for f in &lc.related_files {
                                        failure_context.push_str(&format!(
                                            "- `{}` ({})\n",
                                            f.path, f.relevance
                                        ));
                                    }
                                }
                            }
                        }
                    }

                    // ── Implementer phase ────────────────────────────────────
                    let implementer_start = std::time::Instant::now();

                    let (agentic_outcome, _new_steps) = if has_agentic_steps {
                        self.agentic_executor
                            .run_agentic(
                                config,
                                total_iterations,
                                &failure_context,
                                has_agentic_steps,
                                agentic_steps,
                                logger,
                            )
                            .await
                    } else {
                        (
                            crate::unified_workflow_executor::AgenticOutcome::Skipped,
                            vec![],
                        )
                    };

                    let implementer_duration = implementer_start.elapsed().as_millis() as u64;

                    // Query token usage recorded during the implementer's run_agentic call.
                    // Fall back to tokens carried on AgenticOutcome when DB has no records
                    // (e.g., API providers that don't call record_phase_token_usage).
                    let (mut impl_tokens_in, mut impl_tokens_out) = query_iteration_tokens(
                        &self.checkpoint_db,
                        &config.execution_id,
                        total_iterations,
                    );
                    if impl_tokens_in == 0 && impl_tokens_out == 0 {
                        let (ot_in, ot_out) = agentic_outcome.token_usage();
                        impl_tokens_in = ot_in.unwrap_or(0);
                        impl_tokens_out = ot_out.unwrap_or(0);
                    }
                    let impl_model = config
                        .resolve_model_for_phase("agentic")
                        .unwrap_or_else(|| "claude-cli".to_string());
                    let impl_cost = crate::ai_pricing::calculate_cost_usd(
                        impl_tokens_in,
                        impl_tokens_out,
                        &impl_model,
                    );
                    if impl_tokens_in > 0 || impl_tokens_out > 0 {
                        info!(
                            "MULTI-AGENT-PIPELINE: Implementer tokens: in={}, out={}, cost=${:.4}",
                            impl_tokens_in, impl_tokens_out, impl_cost
                        );
                    }

                    let implementer_trace = PipelineAgentTrace {
                        agent_type: "implementer".to_string(),
                        agent_id: format!("impl_{}_{}_{}", subtree.id, level_idx, level_attempt),
                        run_id: config.execution_id.clone(),
                        input_snapshot: serde_json::json!({
                            "subtree_id": subtree.id,
                            "level": level_idx,
                            "attempt": level_attempt,
                            "criteria": level_criteria,
                            "has_prior_feedback": prior_failure_feedback.is_some(),
                        }),
                        output_snapshot: serde_json::json!({
                            "outcome": format!("{:?}", agentic_outcome),
                        }),
                        config: pipeline_config.implementer.clone(),
                        duration_ms: implementer_duration,
                        tokens_in: impl_tokens_in as u32,
                        tokens_out: impl_tokens_out as u32,
                        cost_usd: impl_cost,
                        downstream_success: None,
                        output_quality_score: None,
                    };
                    agent_traces.push(implementer_trace.clone());
                    last_implementer_trace = Some(implementer_trace);

                    // ── Verifier phase ───────────────────────────────────────
                    let verifier_start = std::time::Instant::now();

                    let level_verification_steps: Vec<ExecutionStepConfig> = verification_steps
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| {
                            level_criteria.contains(&format!("criterion_{}", i).as_str())
                        })
                        .map(|(_, step)| step.clone())
                        .collect();

                    let (verification_result, _step_results) = self
                        .verification_executor
                        .run_verification(
                            &level_verification_steps,
                            &config.execution_id,
                            total_iterations,
                            &config.workflow_name,
                            logger,
                            config.stage_index,
                        )
                        .await;

                    level_criterion_results = Vec::new();
                    level_passed = verification_result.all_passed;

                    for step_result in &verification_result.step_results {
                        let details = step_result
                            .verification_details
                            .as_ref()
                            .and_then(|vd| vd.stdout.as_ref())
                            .or(step_result.error.as_ref())
                            .map(|s| s.chars().take(500).collect::<String>())
                            .unwrap_or_default();

                        level_criterion_results.push(PipelineCriterionResult {
                            criterion_id: step_result.step_name.clone(),
                            passed: step_result.success,
                            method_used: step_result.step_type.clone(),
                            confidence: if step_result.success { 1.0 } else { 0.0 },
                            details,
                            duration_ms: step_result.duration_ms,
                        });
                    }

                    let verifier_duration = verifier_start.elapsed().as_millis() as u64;

                    let verifier_trace = PipelineAgentTrace {
                        agent_type: "verifier".to_string(),
                        agent_id: format!("verify_{}_{}_{}", subtree.id, level_idx, level_attempt),
                        run_id: config.execution_id.clone(),
                        input_snapshot: serde_json::json!({
                            "subtree_id": subtree.id,
                            "level": level_idx,
                            "attempt": level_attempt,
                            "criteria_count": level_criterion_results.len(),
                        }),
                        output_snapshot: serde_json::json!({
                            "passed": level_passed,
                            "results": level_criterion_results.len(),
                        }),
                        config: pipeline_config.verifier.clone(),
                        duration_ms: verifier_duration,
                        tokens_in: 0,
                        tokens_out: 0,
                        cost_usd: 0.0,
                        downstream_success: Some(level_passed),
                        output_quality_score: None,
                    };
                    agent_traces.push(verifier_trace.clone());
                    last_verifier_trace = Some(verifier_trace);

                    // Record canary run outcome if this is a canary run
                    if let Some((ref canary_id, _)) = active_canary {
                        let _ = crate::meta_optimizer::canary::record_canary_run(
                            &self.checkpoint_db,
                            canary_id,
                            is_canary_run,
                            level_passed,
                            impl_cost,
                            (implementer_duration + verifier_duration) as f64,
                        );
                    }

                    info!(
                        "MULTI-AGENT-PIPELINE: Subtree '{}' level {} attempt {} — {} (impl={}ms, verify={}ms, tokens={}+{})",
                        subtree.id,
                        level_idx,
                        level_attempt,
                        if level_passed { "PASSED" } else { "FAILED" },
                        implementer_duration,
                        verifier_duration,
                        impl_tokens_in,
                        impl_tokens_out,
                    );

                    // Per-iteration token budget check
                    let running_total_tokens: u64 = agent_traces
                        .iter()
                        .map(|t| t.tokens_in as u64 + t.tokens_out as u64)
                        .sum();
                    if running_total_tokens > config.max_context_tokens as u64 {
                        warn!(
                            "MULTI-AGENT-PIPELINE: Token budget warning: {} / {} tokens used after iteration {}",
                            running_total_tokens, config.max_context_tokens, total_iterations
                        );
                    }

                    if level_passed {
                        break; // Level succeeded, move to next level
                    }

                    // Check if we have retries remaining
                    retries_used += 1;
                    if retries_used >= pipeline_config.max_retries_per_subtree {
                        info!(
                            "MULTI-AGENT-PIPELINE: Level {} exhausted retries ({}/{})",
                            level_idx, retries_used, pipeline_config.max_retries_per_subtree
                        );
                        subtree_all_passed = false;
                        break; // No more retries, move on
                    }

                    // Build tiered feedback from failed criteria for the next attempt.
                    // First retry: L0 summary (criterion IDs + pass/fail counts).
                    // Subsequent retries: L1 with full details to give the implementer
                    // more context about what specifically went wrong.
                    let failed_criteria: Vec<&PipelineCriterionResult> = level_criterion_results
                        .iter()
                        .filter(|c| !c.passed)
                        .collect();
                    let passed_count = level_criterion_results.iter().filter(|c| c.passed).count();

                    prior_failure_feedback = if retries_used == 1 {
                        // L0: summary only — saves tokens on first retry
                        let failed_ids: Vec<&str> = failed_criteria
                            .iter()
                            .map(|c| c.criterion_id.as_str())
                            .collect();
                        Some(format!(
                            "{}/{} criteria failed: {}\nFix these criteria and re-run verification.",
                            failed_criteria.len(),
                            failed_criteria.len() + passed_count,
                            failed_ids.join(", ")
                        ))
                    } else {
                        // L1: full details — the L0 summary wasn't enough
                        let detailed: Vec<String> = failed_criteria
                            .iter()
                            .map(|c| format!("- {} ({}): {}", c.criterion_id, c.method_used, c.details))
                            .collect();
                        Some(format!(
                            "{}/{} criteria still failing after {} attempts. Details:\n{}",
                            failed_criteria.len(),
                            failed_criteria.len() + passed_count,
                            retries_used,
                            detailed.join("\n")
                        ))
                    };

                    info!(
                        "MULTI-AGENT-PIPELINE: Level {} failed, retrying ({}/{} retries used)",
                        level_idx, retries_used, pipeline_config.max_retries_per_subtree
                    );
                } // end retry loop

                if !level_passed {
                    subtree_all_passed = false;
                }

                subtree_level_results.push(SubtreeLevelResult {
                    level: level_idx as u32,
                    implementer_trace: last_implementer_trace.unwrap_or_else(|| {
                        PipelineAgentTrace {
                            agent_type: "implementer".to_string(),
                            agent_id: format!("impl_{}_{}", subtree.id, level_idx),
                            run_id: config.execution_id.clone(),
                            input_snapshot: serde_json::json!(null),
                            output_snapshot: serde_json::json!(null),
                            config: pipeline_config.implementer.clone(),
                            duration_ms: 0,
                            tokens_in: 0,
                            tokens_out: 0,
                            cost_usd: 0.0,
                            downstream_success: None,
                            output_quality_score: None,
                        }
                    }),
                    verifier_trace: last_verifier_trace.unwrap_or_else(|| PipelineAgentTrace {
                        agent_type: "verifier".to_string(),
                        agent_id: format!("verify_{}_{}", subtree.id, level_idx),
                        run_id: config.execution_id.clone(),
                        input_snapshot: serde_json::json!(null),
                        output_snapshot: serde_json::json!(null),
                        config: pipeline_config.verifier.clone(),
                        duration_ms: 0,
                        tokens_in: 0,
                        tokens_out: 0,
                        cost_usd: 0.0,
                        downstream_success: None,
                        output_quality_score: None,
                    }),
                    retries: level_attempt.saturating_sub(1),
                    passed: level_passed,
                    criterion_results: level_criterion_results,
                });
            }

            subtree_results.push(SubtreeResult {
                subtree_id: subtree.id.clone(),
                level_results: subtree_level_results,
                retries_used,
                all_passed: subtree_all_passed,
                regressions: vec![],
            });
        }

        // ── Phase 6: Integration Verification ───────────────────────────
        let integration_result = if pipeline_config.integration_verification {
            info!("MULTI-AGENT-PIPELINE: Phase 6 — Integration Verification (full spec check)");

            let (int_verification_result, _int_step_results) = self
                .verification_executor
                .run_verification(
                    verification_steps,
                    &config.execution_id,
                    total_iterations + 1, // integration is an extra verification pass
                    &config.workflow_name,
                    logger,
                    config.stage_index,
                )
                .await;

            let mut integration_criteria: Vec<PipelineCriterionResult> = Vec::new();
            for step_result in &int_verification_result.step_results {
                let details = step_result
                    .verification_details
                    .as_ref()
                    .and_then(|vd| vd.stdout.as_ref())
                    .or(step_result.error.as_ref())
                    .map(|s| s.chars().take(500).collect::<String>())
                    .unwrap_or_default();

                integration_criteria.push(PipelineCriterionResult {
                    criterion_id: step_result.step_name.clone(),
                    passed: step_result.success,
                    method_used: step_result.step_type.clone(),
                    confidence: if step_result.success { 1.0 } else { 0.0 },
                    details,
                    duration_ms: step_result.duration_ms,
                });
            }

            let int_passed = int_verification_result.all_passed;
            let int_total = integration_criteria.len();
            let int_ok = integration_criteria.iter().filter(|c| c.passed).count();
            info!(
                "MULTI-AGENT-PIPELINE: Integration verification — {}/{} passed ({})",
                int_ok,
                int_total,
                if int_passed { "ALL PASS" } else { "FAILURES" },
            );

            Some(integration_criteria)
        } else {
            None
        };

        // ── Build final result ──────────────────────────────────────────
        let goal_achieved = if let Some(ref int_results) = integration_result {
            int_results.iter().all(|c| c.passed)
        } else {
            subtree_results.iter().all(|s| s.all_passed)
        };

        let passed_criteria = if let Some(ref int_results) = integration_result {
            int_results.iter().filter(|c| c.passed).count() as u32
        } else {
            subtree_results
                .iter()
                .flat_map(|s| s.level_results.iter())
                .flat_map(|l| l.criterion_results.iter())
                .filter(|c| c.passed)
                .count() as u32
        };

        // Backfill downstream_success on all traces
        for trace in &mut agent_traces {
            trace.downstream_success = Some(goal_achieved);
        }

        // Sum total tokens and cost across all agent traces
        let total_tokens: u64 = agent_traces
            .iter()
            .map(|t| t.tokens_in as u64 + t.tokens_out as u64)
            .sum();
        let total_cost_usd: f64 = agent_traces.iter().map(|t| t.cost_usd).sum();

        if total_tokens > 0 {
            info!(
                "MULTI-AGENT-PIPELINE: Total tokens={}, total cost=${:.4}",
                total_tokens, total_cost_usd
            );
        }

        // Check token budget
        if total_tokens > config.max_context_tokens as u64 {
            warn!(
                "MULTI-AGENT-PIPELINE: Token budget exceeded: {} / {} tokens used (cost=${:.4})",
                total_tokens, config.max_context_tokens, total_cost_usd
            );
        }

        let result = MultiAgentPipelineResult {
            total_iterations,
            goal_achieved,
            was_stopped: false,
            max_iterations_reached: total_iterations >= pipeline_config.max_total_iterations,
            subtree_results,
            integration_result,
            agent_traces,
            dag,
            total_criteria: criteria.len() as u32,
            passed_criteria,
            total_tokens,
            total_cost_usd,
        };

        info!("MULTI-AGENT-PIPELINE: {}", result.summary());

        // Persist agent traces for meta-optimizer analysis
        if let Err(e) = crate::database::pipeline_traces::save_pipeline_agent_traces(
            &self.checkpoint_db,
            &config.execution_id,
            &result.agent_traces,
        ) {
            warn!("Failed to persist pipeline agent traces: {}", e);
        }

        // Store the full pipeline result in task run result_data for autoresearch retrieval
        if let Ok(result_json) = serde_json::to_string(&result) {
            if let Err(e) = self
                .checkpoint_db
                .update_task_run_result_data(&config.execution_id, &result_json)
            {
                warn!("Failed to store pipeline result_data: {}", e);
            }
        }

        result.to_loop_result()
    }
}

/// Get a truncated file tree from the project directory using `git ls-files`.
pub(super) fn get_file_tree(project_path: &str) -> String {
    let output = std::process::Command::new("git")
        .args(["ls-files", "--others", "--cached", "--exclude-standard"])
        .current_dir(project_path)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let files = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = files.lines().take(500).collect();
            let total = files.lines().count();
            let mut result = lines.join("\n");
            if total > 500 {
                result.push_str(&format!("\n... and {} more files (truncated)", total - 500));
            }
            result
        }
        _ => {
            format!("(Could not list files in {})", project_path)
        }
    }
}

/// L0 file tree: directory structure only (unique directory paths).
/// Much smaller than the full file listing — typically 10-50x fewer lines.
pub(super) fn get_file_tree_l0(project_path: &str) -> String {
    let output = std::process::Command::new("git")
        .args(["ls-files", "--others", "--cached", "--exclude-standard"])
        .current_dir(project_path)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let files = String::from_utf8_lossy(&out.stdout);
            let total_files = files.lines().count();

            // Extract unique directory paths including parents
            let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for line in files.lines() {
                if let Some(last_slash) = line.rfind('/') {
                    let dir = &line[..last_slash];
                    let mut current = String::new();
                    for part in dir.split('/') {
                        if !current.is_empty() {
                            current.push('/');
                        }
                        current.push_str(part);
                        dirs.insert(current.clone());
                    }
                }
            }

            let dir_count = dirs.len();
            let mut result = dirs.into_iter().collect::<Vec<_>>().join("\n");
            result.push_str(&format!(
                "\n\n({} directories, {} total files)",
                dir_count, total_files
            ));
            result
        }
        _ => {
            format!("(Could not list files in {})", project_path)
        }
    }
}
