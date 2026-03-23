//! Multi-Agent Pipeline Loop — DAG-structured workflow architecture.
//!
//! Specialized agents in a DAG-structured pipeline instead of a monolithic verify->fix loop.
//!
//! ## PipelineContext
//!
//! Structured inter-agent data that flows through all pipeline phases.
//! Each phase reads from previous phases' outputs and writes its own,
//! replacing the previous pattern of passing failure context as concatenated strings.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::autoresearch::agentic_verification::*;
use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;

use super::loop_controller::LoopController;
use super::types::{LoopConfig, LoopResult};

/// Structured inter-agent context that accumulates outputs from each pipeline phase.
///
/// Instead of passing failure context as concatenated strings between agents,
/// each phase reads structured data from previous phases and writes its own.
/// This enables:
/// - Type-safe data flow between agents
/// - Richer context for downstream agents (e.g., locator results inform implementer)
/// - Structured telemetry for per-agent optimization
#[derive(Debug, Clone, serde::Serialize)]
pub struct PipelineContext {
    /// Structured acceptance criteria from the Spec Analyst phase.
    pub spec_results: Vec<PipelineAcceptanceCriterion>,

    /// Code location mappings from the Locator Agent phase.
    pub locator_results: Vec<LocatedCriterion>,

    /// Changes made by Implementer agents, tracked per subtree+level.
    pub implementer_changes: Vec<ImplementerChange>,

    /// Verification failures from Verifier agents.
    pub verifier_failures: Vec<VerifierFailure>,
}

/// A record of changes made by an implementer agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImplementerChange {
    /// Subtree this change belongs to.
    pub subtree_id: String,
    /// DAG level within the subtree.
    pub level: usize,
    /// Attempt number (1-indexed).
    pub attempt: u32,
    /// Criteria IDs addressed in this change.
    pub criteria_ids: Vec<String>,
    /// Whether the implementer succeeded.
    pub success: bool,
    /// Token usage for this implementation pass.
    pub tokens_in: u64,
    /// Output tokens for this implementation pass.
    pub tokens_out: u64,
}

/// A verification failure from a verifier agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifierFailure {
    /// Criterion that failed verification.
    pub criterion_id: String,
    /// Verification method used.
    pub method: String,
    /// Truncated failure details.
    pub details: String,
    /// Which attempt this failure occurred on.
    pub attempt: u32,
}

impl PipelineContext {
    fn new() -> Self {
        Self {
            spec_results: Vec::new(),
            locator_results: Vec::new(),
            implementer_changes: Vec::new(),
            verifier_failures: Vec::new(),
        }
    }

    /// Build implementer context string from structured data for a specific level.
    fn build_implementer_context(
        &self,
        subtree_id: &str,
        level_idx: usize,
        level_criteria: &[&str],
        prior_failure_feedback: Option<&str>,
        attempt: u32,
        max_retries: u32,
    ) -> String {
        let mut context = format!(
            "Multi-Agent Pipeline: Implement criteria at level {} for subtree '{}'. Criteria: {}",
            level_idx,
            subtree_id,
            level_criteria.join(", ")
        );

        // Add prior failure feedback
        if let Some(feedback) = prior_failure_feedback {
            context.push_str(&format!(
                "\n\n## Previous Attempt Failed (attempt {}/{})\n{}",
                attempt - 1,
                max_retries + 1,
                feedback
            ));
        }

        // Add location context from the Locator agent
        if !self.locator_results.is_empty() {
            context.push_str("\n\n## Code Locations (from Locator Agent)\n");
            for lc in &self.locator_results {
                if level_criteria.contains(&lc.criterion.id.as_str()) {
                    context.push_str(&format!(
                        "### {} (confidence: {:.0}%)\n",
                        lc.criterion.id,
                        lc.confidence * 100.0
                    ));
                    if !lc.target_files.is_empty() {
                        context.push_str("Target files:\n");
                        for f in &lc.target_files {
                            context.push_str(&format!("- `{}` ({})\n", f.path, f.relevance));
                        }
                    }
                    if !lc.related_files.is_empty() {
                        context.push_str("Related files:\n");
                        for f in &lc.related_files {
                            context.push_str(&format!("- `{}` ({})\n", f.path, f.relevance));
                        }
                    }
                }
            }
        }

        context
    }
}

/// Collected output from processing a single subtree.
///
/// Returned by `process_subtree` and merged into the pipeline-level accumulators
/// after all parallel subtrees complete.
struct SubtreeOutput {
    result: SubtreeResult,
    traces: Vec<PipelineAgentTrace>,
    implementer_changes: Vec<ImplementerChange>,
    verifier_failures: Vec<VerifierFailure>,
    iterations_used: u32,
    was_stopped: bool,
}

/// Lightweight, `Clone + Send + Sync` handle that carries only the shared state
/// required by `process_subtree`.  Created once per pipeline invocation from the
/// parent `LoopController` and cheaply cloned into each parallel subtree future
/// so that `buffer_unordered` can satisfy `Send + 'static` bounds.
#[derive(Clone)]
struct PipelineShared {
    checkpoint_db: Arc<crate::database::CheckpointDb>,
    agentic_executor: Arc<super::phases::AgenticExecutor>,
    verification_executor: Arc<super::phases::VerificationExecutor>,
}

impl PipelineShared {
    fn from_controller(ctrl: &LoopController) -> Self {
        Self {
            checkpoint_db: Arc::clone(&ctrl.checkpoint_db),
            agentic_executor: Arc::clone(&ctrl.agentic_executor),
            verification_executor: Arc::clone(&ctrl.verification_executor),
        }
    }

    /// Mirror of `LoopController::is_task_stopped` — only needs `checkpoint_db`.
    fn is_task_stopped(&self, execution_id: &str) -> bool {
        let task_id_to_check = super::types::get_parent_task_id(execution_id);
        match self.checkpoint_db.get_task_run(&task_id_to_check) {
            Ok(Some(task)) => {
                if task.status == "stopped" {
                    info!(
                        "Task {} has been stopped externally - aborting workflow",
                        task_id_to_check
                    );
                    true
                } else {
                    false
                }
            }
            Ok(None) => false,
            Err(_) => false,
        }
    }
}

// =============================================================================
// DAG Construction
// =============================================================================

/// Build a flat DAG: single subtree, all criteria at level 0.
/// Used as the "flat" baseline strategy for A/B comparison.
fn build_flat_dag(criteria: &[PipelineAcceptanceCriterion]) -> ExecutionDAG {
    ExecutionDAG {
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
    }
}

/// Build a dependency-aware DAG with topological sort and independent subtree partitioning.
///
/// 1. Compute levels via Kahn's algorithm (criteria with no deps → level 0, etc.)
/// 2. Partition criteria into independent subtrees using Union-Find on dependency edges
/// 3. Subtrees with no shared dependencies can execute in parallel
fn build_dependency_dag(criteria: &[PipelineAcceptanceCriterion]) -> ExecutionDAG {
    let ids: Vec<&str> = criteria.iter().map(|c| c.id.as_str()).collect();
    let id_set: HashSet<&str> = ids.iter().copied().collect();

    // Build adjacency and in-degree for topological sort
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents_map: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in &ids {
        in_degree.insert(id, 0);
        dependents_map.insert(id, Vec::new());
    }

    for c in criteria {
        for dep in &c.depends_on {
            if id_set.contains(dep.as_str()) {
                if let Some(deg) = in_degree.get_mut(c.id.as_str()) {
                    *deg += 1;
                }
                if let Some(deps) = dependents_map.get_mut(dep.as_str()) {
                    deps.push(&c.id);
                }
            }
        }
    }

    // Kahn's algorithm: compute levels
    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut node_level: HashMap<&str, usize> = HashMap::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    let mut processed = 0usize;

    for (&id, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(id);
        }
    }

    while !queue.is_empty() {
        let layer_size = queue.len();
        let mut layer = Vec::with_capacity(layer_size);
        let level_idx = levels.len();

        for _ in 0..layer_size {
            let Some(node) = queue.pop_front() else { break };
            layer.push(node.to_string());
            node_level.insert(node, level_idx);
            processed += 1;

            for &dep in dependents_map.get(node).unwrap_or(&Vec::new()) {
                if let Some(deg) = in_degree.get_mut(dep) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(dep);
                    }
                }
            }
        }

        levels.push(layer);
    }

    // Handle cycle (shouldn't happen, but fall back to flat)
    if processed != criteria.len() {
        warn!(
            "MULTI-AGENT-PIPELINE: Dependency cycle detected ({}/{} criteria ordered) — falling back to flat DAG",
            processed,
            criteria.len()
        );
        return build_flat_dag(criteria);
    }

    // Union-Find to partition criteria into independent subtrees.
    // Two criteria share a subtree if they have a dependency relationship (transitively).
    let mut uf_parent: HashMap<&str, &str> = HashMap::new();
    for id in &ids {
        uf_parent.insert(id, id);
    }

    fn uf_find<'a>(parent: &mut HashMap<&'a str, &'a str>, x: &'a str) -> &'a str {
        let p = parent[x];
        if p == x {
            return x;
        }
        let root = uf_find(parent, p);
        parent.insert(x, root);
        root
    }

    fn uf_union<'a>(parent: &mut HashMap<&'a str, &'a str>, a: &'a str, b: &'a str) {
        let ra = uf_find(parent, a);
        let rb = uf_find(parent, b);
        if ra != rb {
            parent.insert(ra, rb);
        }
    }

    for c in criteria {
        for dep in &c.depends_on {
            if id_set.contains(dep.as_str()) {
                uf_union(&mut uf_parent, &c.id, dep);
            }
        }
    }

    // Group criteria by subtree root
    let mut subtree_groups: HashMap<String, Vec<&str>> = HashMap::new();
    for id in &ids {
        let root = uf_find(&mut uf_parent, id).to_string();
        subtree_groups.entry(root).or_default().push(id);
    }

    // Sort subtree groups deterministically by first criterion ID
    let mut sorted_groups: Vec<(String, Vec<&str>)> = subtree_groups.into_iter().collect();
    sorted_groups.sort_by(|a, b| a.0.cmp(&b.0));

    // Build subtrees and nodes
    let mut nodes = HashMap::new();
    let mut subtrees = Vec::new();
    let mut roots = Vec::new();

    for (idx, (_root, group)) in sorted_groups.iter().enumerate() {
        let subtree_id = format!("subtree_{}", idx);
        let mut root_criteria = Vec::new();
        let all_criteria: Vec<String> = group.iter().map(|s| s.to_string()).collect();
        let mut max_level = 0usize;

        for &crit_id in group {
            let level = node_level.get(crit_id).copied().unwrap_or(0);
            if level > max_level {
                max_level = level;
            }

            let Some(c) = criteria.iter().find(|c| c.id == crit_id) else {
                continue;
            };
            let has_internal_dep = c.depends_on.iter().any(|d| group.contains(&d.as_str()));
            if !has_internal_dep {
                root_criteria.push(crit_id.to_string());
                roots.push(crit_id.to_string());
            }

            nodes.insert(
                crit_id.to_string(),
                DAGNode {
                    criterion_id: crit_id.to_string(),
                    dependencies: c.depends_on.clone(),
                    dependents: dependents_map
                        .get(crit_id)
                        .map(|v| v.iter().map(|s| s.to_string()).collect())
                        .unwrap_or_default(),
                    level: level as u32,
                    subtree_id: subtree_id.clone(),
                },
            );
        }

        let complexity = if all_criteria.len() <= 2 {
            "simple"
        } else if all_criteria.len() <= 5 {
            "moderate"
        } else {
            "complex"
        };

        subtrees.push(DAGSubtree {
            id: subtree_id,
            root_criteria,
            all_criteria,
            max_level: max_level as u32,
            estimated_complexity: complexity.to_string(),
        });
    }

    ExecutionDAG {
        nodes,
        roots,
        levels,
        subtrees,
    }
}

/// Query token usage from the database for a specific execution_id and iteration.
///
/// Returns (input_tokens, output_tokens). Falls back to (0, 0) on error.
pub(super) fn query_iteration_tokens(
    db: &crate::database::CheckpointDb,
    execution_id: &str,
    iteration: u32,
) -> (u64, u64) {
    match db.get_iteration_token_totals(execution_id, iteration) {
        Ok(totals) => totals,
        Err(e) => {
            warn!("Failed to query iteration token totals: {}", e);
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

        // Canary detection, config overrides, recording, and restoration are handled
        // at the loop_controller::run() level (shared across all architectures).
        // Here we only inject prompt overrides, which are pipeline-specific
        // (they target named agent roles like "implementer", "locator").
        if let Some((_, ref rec_id)) = config.active_canary {
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

        // ── Check for existing checkpoint (resume support) ────────────────
        let checkpoint = match crate::database::pipeline_traces::get_pipeline_checkpoint(
            &self.checkpoint_db,
            &config.execution_id,
        ) {
            Ok(Some(cp)) => {
                info!(
                    "MULTI-AGENT-PIPELINE: Found checkpoint at phase {} ({} subtrees completed, {} iterations consumed)",
                    cp.last_completed_phase,
                    cp.completed_subtrees.len(),
                    cp.total_iterations,
                );
                Some(cp)
            }
            Ok(None) => None,
            Err(e) => {
                warn!(
                    "MULTI-AGENT-PIPELINE: Failed to load checkpoint: {}. Starting fresh.",
                    e
                );
                None
            }
        };

        let mut total_iterations: u32 = 0;
        let mut agent_traces: Vec<PipelineAgentTrace> = Vec::new();
        let mut pipeline_ctx = PipelineContext::new();

        // If resuming from checkpoint, restore iteration count and reload persisted traces.
        if let Some(ref cp) = checkpoint {
            if cp.last_completed_phase >= 4 {
                total_iterations = cp.total_iterations;
                // Reload agent traces persisted during the previous (crashed) run
                match crate::database::pipeline_traces::get_traces_for_task_run(
                    &self.checkpoint_db,
                    &config.execution_id,
                ) {
                    Ok(traces) => {
                        info!(
                            "MULTI-AGENT-PIPELINE: Restored {} agent traces from previous run",
                            traces.len()
                        );
                        agent_traces = traces;
                    }
                    Err(e) => {
                        warn!(
                            "MULTI-AGENT-PIPELINE: Failed to reload traces on resume: {}",
                            e
                        );
                    }
                }
            }
        }

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

        let spec_trace = PipelineAgentTrace {
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
            parent_span_id: None,
            span_type: "agent".to_string(),
            guardrail_results: vec![],
            handoff_received: None,
        };
        // Persist incrementally so trace survives pipeline crashes
        if let Err(e) = crate::database::pipeline_traces::save_pipeline_agent_trace(
            &self.checkpoint_db,
            &config.execution_id,
            &spec_trace,
        ) {
            warn!("Failed to persist spec_analyst trace: {}", e);
        }
        agent_traces.push(spec_trace);

        // Store spec results in pipeline context
        pipeline_ctx.spec_results = criteria.clone();

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
                total_tokens: None,
                total_cost_usd: None,
                files_modified: Vec::new(),
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

        let dag = if pipeline_config.dag_strategy == "flat" {
            // Flat strategy: single subtree, all at level 0 (baseline for comparison)
            build_flat_dag(&criteria)
        } else {
            // Strict/permissive: topological sort + subtree partitioning
            build_dependency_dag(&criteria)
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

        // If we have a checkpoint with located_criteria from a previous run,
        // skip the expensive AI locator call and reuse the saved results.
        let has_checkpoint_located = checkpoint
            .as_ref()
            .is_some_and(|cp| cp.last_completed_phase >= 4 && cp.located_criteria.is_some());

        let empty_located: Vec<LocatedCriterion> = Vec::new();
        let mut located_criteria: Vec<LocatedCriterion> = if has_checkpoint_located {
            let saved = checkpoint
                .as_ref()
                .and_then(|cp| cp.located_criteria.as_ref())
                .unwrap_or(&empty_located);
            info!(
                "MULTI-AGENT-PIPELINE: Phase 4 — Restoring {} located criteria from checkpoint (skipping AI locator)",
                saved.len()
            );
            saved.clone()
        } else if pipeline_config.locator.max_tokens.unwrap_or(0) > 0 {
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

            let locator_trace = PipelineAgentTrace {
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
                tokens_in: u32::try_from(locator_tokens_in).unwrap_or(u32::MAX),
                tokens_out: u32::try_from(locator_tokens_out).unwrap_or(u32::MAX),
                cost_usd: locator_cost,
                downstream_success: None,
                output_quality_score: None,
                parent_span_id: None,
                span_type: "agent".to_string(),
                guardrail_results: vec![],
                handoff_received: None,
            };
            if let Err(e) = crate::database::pipeline_traces::save_pipeline_agent_trace(
                &self.checkpoint_db,
                &config.execution_id,
                &locator_trace,
            ) {
                warn!("Failed to persist locator trace: {}", e);
            }
            agent_traces.push(locator_trace);

            parsed
        } else {
            info!("MULTI-AGENT-PIPELINE: Phase 4 — Code Location (skipped, locator.max_tokens=0)");
            Vec::new()
        };

        // Store locator results in pipeline context
        pipeline_ctx.locator_results = located_criteria.clone();

        // Guardrail: check locator output quality before proceeding to implementer
        {
            use crate::autoresearch::agentic_verification::guardrail_locator_output_schema;
            let locator_check = guardrail_locator_output_schema(&serde_json::json!({
                "located_criteria_count": located_criteria.len(),
            }));
            if locator_check.tripwire_triggered {
                info!(
                    "MULTI-AGENT-PIPELINE: Locator guardrail tripped — {}. Skipping locator results; implementer will proceed without location guidance.",
                    locator_check.check_description
                );
                // Clear locator results so downstream agents don't use partial/empty data
                located_criteria = Vec::new();
                pipeline_ctx.locator_results = Vec::new();
            }
        }

        // Guardrail: check token budget before entering implementation loop
        {
            use crate::autoresearch::agentic_verification::guardrail_token_budget;
            let tokens_so_far: u64 = agent_traces
                .iter()
                .map(|t| t.tokens_in as u64 + t.tokens_out as u64)
                .sum();
            let budget_check = guardrail_token_budget(
                tokens_so_far,
                config.max_context_tokens as u64,
                1000, // minimum tokens needed for at least one implementer call
            );
            if budget_check.tripwire_triggered {
                warn!(
                    "MULTI-AGENT-PIPELINE: Token budget guardrail tripped — {}. Returning early before implementation phase.",
                    budget_check.check_description
                );
                let budget_total_tokens: u64 = agent_traces
                    .iter()
                    .map(|t| t.tokens_in as u64 + t.tokens_out as u64)
                    .sum();
                let budget_total_cost: f64 = agent_traces.iter().map(|t| t.cost_usd).sum();
                let result = MultiAgentPipelineResult {
                    total_iterations,
                    goal_achieved: false,
                    was_stopped: false,
                    max_iterations_reached: true, // budget exhausted is conceptually similar
                    subtree_results: Vec::new(),
                    integration_result: None,
                    agent_traces,
                    dag: dag.clone(),
                    total_criteria: criteria.len() as u32,
                    passed_criteria: 0,
                    total_tokens: budget_total_tokens,
                    total_cost_usd: budget_total_cost,
                };
                // Canary config restoration handled at loop_controller::run() level
                return result.to_loop_result();
            }
        }

        // ── Save checkpoint at Phase 4 boundary ──────────────────────────
        // This is the most valuable checkpoint: phases 1-4 are done, Phase 5 (implementation)
        // is the most expensive. Saving here means a crash during implementation doesn't lose
        // the locator's AI-generated results.
        {
            let cp = PipelineCheckpoint {
                last_completed_phase: 4,
                criteria: Some(criteria.clone()),
                located_criteria: Some(located_criteria.clone()),
                completed_subtrees: vec![],
                total_iterations,
                agent_trace_count: agent_traces.len(),
            };
            if let Err(e) = crate::database::pipeline_traces::save_pipeline_checkpoint(
                &self.checkpoint_db,
                &config.execution_id,
                &cp,
            ) {
                warn!(
                    "MULTI-AGENT-PIPELINE: Failed to save Phase 4 checkpoint: {}",
                    e
                );
            } else {
                info!("MULTI-AGENT-PIPELINE: Saved checkpoint at Phase 4 boundary");
            }
        }

        // ── Phase 5: Implementation + Verification per subtree ──────────
        info!("MULTI-AGENT-PIPELINE: Phase 5 — Implementation + Verification");

        let mut subtree_results: Vec<SubtreeResult> = Vec::new();

        // Collect IDs of subtrees already completed in a previous run (from checkpoint).
        let checkpoint_completed_ids: std::collections::HashSet<String> = checkpoint
            .as_ref()
            .map(|cp| {
                cp.completed_subtrees
                    .iter()
                    .map(|s| s.subtree_id.clone())
                    .collect()
            })
            .unwrap_or_default();

        // Pre-populate subtree_results with checkpoint data for already-completed subtrees.
        if let Some(ref cp) = checkpoint {
            for completed in &cp.completed_subtrees {
                subtree_results.push(completed.clone());
            }
            if !cp.completed_subtrees.is_empty() {
                info!(
                    "MULTI-AGENT-PIPELINE: Restored {} completed subtree(s) from checkpoint",
                    cp.completed_subtrees.len()
                );
            }
        }

        // Filter out subtrees already completed in a previous run.
        let pending_subtrees: Vec<&DAGSubtree> = dag
            .subtrees
            .iter()
            .filter(|s| !checkpoint_completed_ids.contains(&s.id))
            .collect();

        // Process independent subtrees concurrently using JoinSet.
        // PipelineShared is Clone + Send + 'static (Arc-only fields), enabling tokio::spawn.
        // A semaphore bounds concurrency to max_parallel_implementers.
        let shared = PipelineShared::from_controller(self);
        {
            let max_parallel = pipeline_config.max_parallel_implementers.max(1) as usize;
            let subtree_count = pending_subtrees.len();

            if subtree_count > 1 && max_parallel > 1 {
                info!(
                    "MULTI-AGENT-PIPELINE: Processing {} subtrees with concurrency={}",
                    subtree_count,
                    max_parallel.min(subtree_count)
                );
            }

            let semaphore = Arc::new(tokio::sync::Semaphore::new(max_parallel));
            let mut join_set = tokio::task::JoinSet::new();

            for subtree in &pending_subtrees {
                let sem = semaphore.clone();
                let shared_clone = shared.clone();
                // Clone all borrowed data for 'static lifetime in spawned task
                let subtree_owned = (*subtree).clone();
                let config_owned = config.clone();
                let pipeline_config_owned = pipeline_config.clone();
                let dag_levels_owned = dag.levels.clone();
                let located_owned = located_criteria.clone();
                let prompts_owned = active_prompt_variants.clone();
                let vsteps_owned = verification_steps.to_vec();
                let asteps_owned = agentic_steps.to_vec();
                let logger_owned = logger.clone();
                let pctx_owned = pipeline_ctx.clone();

                join_set.spawn(async move {
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    shared_clone
                        .process_subtree(
                            &subtree_owned,
                            &config_owned,
                            &pipeline_config_owned,
                            &dag_levels_owned,
                            &located_owned,
                            &prompts_owned,
                            &vsteps_owned,
                            has_agentic_steps,
                            &asteps_owned,
                            &logger_owned,
                            &pctx_owned,
                        )
                        .await
                });
            }

            // Collect results as subtrees complete
            while let Some(join_result) = join_set.join_next().await {
                let output = match join_result {
                    Ok(o) => o,
                    Err(e) => {
                        warn!("MULTI-AGENT-PIPELINE: Subtree task panicked: {}", e);
                        continue;
                    }
                };
                total_iterations += output.iterations_used;
                agent_traces.extend(output.traces);
                pipeline_ctx
                    .implementer_changes
                    .extend(output.implementer_changes);
                pipeline_ctx
                    .verifier_failures
                    .extend(output.verifier_failures);
                subtree_results.push(output.result.clone());

                // Update checkpoint with this completed subtree so progress survives crashes.
                let cp = PipelineCheckpoint {
                    last_completed_phase: 5,
                    criteria: Some(criteria.clone()),
                    located_criteria: Some(located_criteria.clone()),
                    completed_subtrees: subtree_results.clone(),
                    total_iterations,
                    agent_trace_count: agent_traces.len(),
                };
                if let Err(e) = crate::database::pipeline_traces::save_pipeline_checkpoint(
                    &self.checkpoint_db,
                    &config.execution_id,
                    &cp,
                ) {
                    warn!("MULTI-AGENT-PIPELINE: Failed to update checkpoint: {}", e);
                }
            }

        }

        // Check for user stop after all parallel subtrees complete.
        // In parallel mode, we can't break mid-stream, so check after collection.
        // The process_subtree method checks is_task_stopped at entry and returns
        // was_stopped=true, so subtrees started after a stop return early.
        {
            let any_stopped = self.is_task_stopped(&config.execution_id);
            if any_stopped {
                info!("MULTI-AGENT-PIPELINE: Stopped by user");
                let stopped_total_tokens: u64 = agent_traces
                    .iter()
                    .map(|t| t.tokens_in as u64 + t.tokens_out as u64)
                    .sum();
                let stopped_total_cost: f64 = agent_traces.iter().map(|t| t.cost_usd).sum();
                let stopped_passed_criteria = subtree_results
                    .iter()
                    .flat_map(|s| s.level_results.iter())
                    .flat_map(|l| l.criterion_results.iter())
                    .filter(|c| c.passed)
                    .count() as u32;
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
                    passed_criteria: stopped_passed_criteria,
                    total_tokens: stopped_total_tokens,
                    total_cost_usd: stopped_total_cost,
                };
                // Canary config restoration handled at loop_controller::run() level
                return result.to_loop_result();
            }
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
        let mut goal_achieved = if let Some(ref int_results) = integration_result {
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
            if config.enforce_token_budget {
                warn!(
                    "MULTI-AGENT-PIPELINE: Token budget ENFORCED — exceeded: {} / {} tokens used (cost=${:.4})",
                    total_tokens, config.max_context_tokens, total_cost_usd
                );
                // Mark goal as not achieved when budget is enforced and exceeded
                goal_achieved = false;
            } else {
                warn!(
                    "MULTI-AGENT-PIPELINE: Token budget exceeded: {} / {} tokens used (cost=${:.4})",
                    total_tokens, config.max_context_tokens, total_cost_usd
                );
            }
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

        // Traces were persisted incrementally after each agent phase.
        // Backfill downstream_success on all traces now that final outcome is known.
        if let Err(e) = crate::database::pipeline_traces::backfill_downstream_success(
            &self.checkpoint_db,
            &config.execution_id,
            result.goal_achieved,
        ) {
            warn!(
                "Failed to backfill downstream_success on pipeline traces: {}",
                e
            );
        }

        // Store the full pipeline result + context in task run result_data for analytics.
        // PipelineContext provides structured inter-agent data (implementer changes,
        // verifier failures, locator mappings) that downstream analytics can query.
        {
            let result_with_context = serde_json::json!({
                "pipeline_result": serde_json::to_value(&result).unwrap_or_default(),
                "pipeline_context": serde_json::to_value(&pipeline_ctx).unwrap_or_default(),
            });
            if let Ok(result_json) = serde_json::to_string(&result_with_context) {
                if let Err(e) = self
                    .checkpoint_db
                    .update_task_run_result_data(&config.execution_id, &result_json)
                {
                    warn!("Failed to store pipeline result_data: {}", e);
                }
            }
        }

        // Clear the checkpoint now that the pipeline completed successfully.
        // This prevents stale checkpoint data from being used on a future re-run.
        if let Err(e) = crate::database::pipeline_traces::clear_pipeline_checkpoint(
            &self.checkpoint_db,
            &config.execution_id,
        ) {
            warn!("Failed to clear pipeline checkpoint: {}", e);
        }

        result.to_loop_result()
    }

}

impl PipelineShared {
    /// Process a single subtree: iterate through levels, run implementer + verifier
    /// with retries, and return all collected outputs as a `SubtreeOutput`.
    ///
    /// Lives on `PipelineShared` (not `LoopController`) so that an `Arc<PipelineShared>`
    /// can be cheaply cloned into each `buffer_unordered` future, satisfying
    /// `Send + 'static` without wrapping the full controller in an Arc.
    #[allow(clippy::too_many_arguments)]
    async fn process_subtree(
        &self,
        subtree: &DAGSubtree,
        config: &LoopConfig,
        pipeline_config: &MultiAgentPipelineConfig,
        dag_levels: &[Vec<String>],
        located_criteria: &[LocatedCriterion],
        active_prompt_variants: &std::collections::HashMap<String, String>,
        verification_steps: &[ExecutionStepConfig],
        has_agentic_steps: bool,
        agentic_steps: &[ExecutionStepConfig],
        logger: &StepEventLogger,
        pipeline_ctx: &PipelineContext,
    ) -> SubtreeOutput {
        // Check stop before starting this subtree
        if self.is_task_stopped(&config.execution_id) {
            return SubtreeOutput {
                result: SubtreeResult {
                    subtree_id: subtree.id.clone(),
                    level_results: vec![],
                    retries_used: 0,
                    all_passed: false,
                    regressions: vec![],
                },
                traces: vec![],
                implementer_changes: vec![],
                verifier_failures: vec![],
                iterations_used: 0,
                was_stopped: true,
            };
        }

        info!(
            "MULTI-AGENT-PIPELINE: Processing subtree '{}' ({} criteria)",
            subtree.id,
            subtree.all_criteria.len()
        );

        let mut subtree_level_results: Vec<SubtreeLevelResult> = Vec::new();
        let mut subtree_all_passed = true;
        let mut retries_used: u32 = 0;
        let mut local_iterations: u32 = 0;
        let mut local_traces: Vec<PipelineAgentTrace> = Vec::new();
        let mut local_impl_changes: Vec<ImplementerChange> = Vec::new();
        let mut local_verifier_failures: Vec<VerifierFailure> = Vec::new();

        // Process levels within this subtree
        for (level_idx, level_criteria_ids) in dag_levels.iter().enumerate() {
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
                if local_iterations >= pipeline_config.max_total_iterations {
                    info!(
                        "MULTI-AGENT-PIPELINE: Total iteration budget ({}) exhausted for subtree '{}'",
                        pipeline_config.max_total_iterations,
                        subtree.id
                    );
                    subtree_all_passed = false;
                    break;
                }

                local_iterations += 1;
                level_attempt += 1;

                // Build failure context from structured PipelineContext
                let mut failure_context = pipeline_ctx.build_implementer_context(
                    &subtree.id,
                    level_idx,
                    &level_criteria,
                    prior_failure_feedback.as_deref(),
                    level_attempt,
                    pipeline_config.max_retries_per_subtree,
                );

                // Inject active prompt variant for implementer if available
                if let Some(variant_prompt) = active_prompt_variants.get("implementer") {
                    failure_context.push_str(&format!(
                        "\n\n## Agent Instructions (from optimized prompt)\n{}",
                        variant_prompt
                    ));
                }

                // Build structured handoff context: locator -> implementer
                let mut implementer_handoff =
                    crate::autoresearch::agentic_verification::HandoffContext {
                        from_agent: "locator".to_string(),
                        to_agent: "implementer".to_string(),
                        payload: serde_json::json!({
                            "subtree_id": subtree.id,
                            "level": level_idx,
                            "criteria": &level_criteria,
                            "located_files": located_criteria.iter()
                                .filter(|lc| level_criteria.contains(&lc.criterion.id.as_str()))
                                .map(|lc| &lc.target_files)
                                .collect::<Vec<_>>(),
                        }),
                        forwarded_items: vec![],
                        validated: false,
                    };

                // Guardrail: validate handoff payload before passing to implementer
                let handoff_check =
                    crate::autoresearch::agentic_verification::guardrail_handoff_payload_present(
                        &implementer_handoff,
                    );
                implementer_handoff.validated = !handoff_check.tripwire_triggered;
                if handoff_check.tripwire_triggered {
                    info!(
                        "MULTI-AGENT-PIPELINE: Handoff guardrail tripped — {}. Continuing with failure_context alone; handoff payload is supplementary.",
                        handoff_check.check_description
                    );
                }

                // ── Implementer phase ────────────────────────────────────
                let implementer_start = std::time::Instant::now();

                let (agentic_outcome, _new_steps) = if has_agentic_steps {
                    self.agentic_executor
                        .run_agentic(
                            config,
                            local_iterations,
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
                let (mut impl_tokens_in, mut impl_tokens_out) = query_iteration_tokens(
                    &self.checkpoint_db,
                    &config.execution_id,
                    local_iterations,
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
                    tokens_in: u32::try_from(impl_tokens_in).unwrap_or(u32::MAX),
                    tokens_out: u32::try_from(impl_tokens_out).unwrap_or(u32::MAX),
                    cost_usd: impl_cost,
                    downstream_success: None,
                    output_quality_score: None,
                    parent_span_id: None,
                    span_type: "agent".to_string(),
                    guardrail_results: vec![],
                    handoff_received: Some(implementer_handoff.clone()),
                };
                if let Err(e) = crate::database::pipeline_traces::save_pipeline_agent_trace(
                    &self.checkpoint_db,
                    &config.execution_id,
                    &implementer_trace,
                ) {
                    warn!("Failed to persist implementer trace: {}", e);
                }
                local_traces.push(implementer_trace.clone());
                last_implementer_trace = Some(implementer_trace);

                // Record implementer change
                local_impl_changes.push(ImplementerChange {
                    subtree_id: subtree.id.clone(),
                    level: level_idx,
                    attempt: level_attempt,
                    criteria_ids: level_criteria.iter().map(|s| s.to_string()).collect(),
                    success: agentic_outcome.is_success(),
                    tokens_in: impl_tokens_in,
                    tokens_out: impl_tokens_out,
                });

                // ── Verifier phase ───────────────────────────────────────
                let verifier_start = std::time::Instant::now();

                let level_verification_steps: Vec<ExecutionStepConfig> = verification_steps
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| level_criteria.contains(&format!("criterion_{}", i).as_str()))
                    .map(|(_, step)| step.clone())
                    .collect();

                let (verification_result, _step_results) = self
                    .verification_executor
                    .run_verification(
                        &level_verification_steps,
                        &config.execution_id,
                        local_iterations,
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

                // Build structured handoff context: implementer -> verifier
                let verifier_handoff = crate::autoresearch::agentic_verification::HandoffContext {
                    from_agent: "implementer".to_string(),
                    to_agent: "verifier".to_string(),
                    payload: serde_json::json!({
                        "subtree_id": subtree.id,
                        "level": level_idx,
                        "attempt": level_attempt,
                        "implementer_success": agentic_outcome.is_success(),
                    }),
                    forwarded_items: vec![],
                    validated: true,
                };

                // Guardrail: check verifier output has a parseable verdict
                let verifier_output_json = serde_json::json!({
                    "passed": level_passed,
                    "results": level_criterion_results.len(),
                });
                let verdict_check =
                    crate::autoresearch::agentic_verification::guardrail_verifier_verdict(
                        &verifier_output_json,
                    );
                let mut verifier_guardrails = vec![];
                if verdict_check.tripwire_triggered {
                    warn!(
                        "MULTI-AGENT-PIPELINE: Verifier verdict guardrail tripped — {}. Treating level as FAILED; will retry if retries remain.",
                        verdict_check.check_description
                    );
                    level_passed = false;
                }
                verifier_guardrails.push(verdict_check);

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
                    output_snapshot: verifier_output_json,
                    config: pipeline_config.verifier.clone(),
                    duration_ms: verifier_duration,
                    tokens_in: 0,
                    tokens_out: 0,
                    cost_usd: 0.0,
                    downstream_success: Some(level_passed),
                    output_quality_score: None,
                    parent_span_id: None,
                    span_type: "agent".to_string(),
                    guardrail_results: verifier_guardrails,
                    handoff_received: Some(verifier_handoff),
                };
                if let Err(e) = crate::database::pipeline_traces::save_pipeline_agent_trace(
                    &self.checkpoint_db,
                    &config.execution_id,
                    &verifier_trace,
                ) {
                    warn!("Failed to persist verifier trace: {}", e);
                }
                local_traces.push(verifier_trace.clone());
                last_verifier_trace = Some(verifier_trace);

                // Record verifier failures
                for cr in &level_criterion_results {
                    if !cr.passed {
                        local_verifier_failures.push(VerifierFailure {
                            criterion_id: cr.criterion_id.clone(),
                            method: cr.method_used.clone(),
                            details: cr.details.clone(),
                            attempt: level_attempt,
                        });
                    }
                }

                // Canary outcome recording handled at loop_controller::run() level

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

                // Per-iteration token budget check (local to this subtree)
                let running_total_tokens: u64 = local_traces
                    .iter()
                    .map(|t| t.tokens_in as u64 + t.tokens_out as u64)
                    .sum();
                if running_total_tokens > config.max_context_tokens as u64 {
                    if config.enforce_token_budget {
                        warn!(
                            "MULTI-AGENT-PIPELINE: Token budget ENFORCED — stopping subtree '{}': {} / {} tokens used",
                            subtree.id, running_total_tokens, config.max_context_tokens
                        );
                        break;
                    } else {
                        warn!(
                            "MULTI-AGENT-PIPELINE: Token budget warning: {} / {} tokens used after iteration {}",
                            running_total_tokens, config.max_context_tokens, local_iterations
                        );
                    }
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
                    break;
                }

                // Build tiered feedback from failed criteria for the next attempt.
                let failed_criteria: Vec<&PipelineCriterionResult> = level_criterion_results
                    .iter()
                    .filter(|c| !c.passed)
                    .collect();
                let passed_count = level_criterion_results.iter().filter(|c| c.passed).count();

                prior_failure_feedback = if retries_used == 1 {
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
                implementer_trace: last_implementer_trace.unwrap_or_else(|| PipelineAgentTrace {
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
                    parent_span_id: None,
                    span_type: "agent".to_string(),
                    guardrail_results: vec![],
                    handoff_received: None,
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
                    parent_span_id: None,
                    span_type: "agent".to_string(),
                    guardrail_results: vec![],
                    handoff_received: None,
                }),
                retries: level_attempt.saturating_sub(1),
                passed: level_passed,
                criterion_results: level_criterion_results,
            });
        }

        SubtreeOutput {
            result: SubtreeResult {
                subtree_id: subtree.id.clone(),
                level_results: subtree_level_results,
                retries_used,
                all_passed: subtree_all_passed,
                regressions: vec![],
            },
            traces: local_traces,
            implementer_changes: local_impl_changes,
            verifier_failures: local_verifier_failures,
            iterations_used: local_iterations,
            was_stopped: false,
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
