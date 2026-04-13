//! DAG Workflow Async Driver — bridges `DagRuntime` with `StepExecutor`.
//!
//! This module provides the primary async entry point for executing a parsed
//! DAG workflow to completion. It drives the pure-sync `DagRuntime` state
//! machine, dispatching ready nodes in parallel to `StepExecutor`, persisting
//! execution events to the event log for crash-recovery replay, and collecting
//! per-node metrics.
//!
//! # Crash recovery
//!
//! Before dispatching a node, the driver queries the event log. If a
//! `completed` event already exists for that `(execution_id, node_id)` pair
//! the cached output is used and the node is not re-executed. This makes DAG
//! execution idempotent across runner restarts.
//!
//! # Parallel execution
//!
//! All nodes within a single layer are dispatched concurrently via
//! `futures::future::join_all`. Each node task creates its own `StepExecutor`
//! instance from the shared dependency Arcs so there are no cross-task
//! borrowing issues.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use futures::future::join_all;
use serde_json::json;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::database::pg::event_log::EventType;
use crate::step_executor::StepExecutor;
use crate::step_executor::executor_types::ExecutionStepConfig;
use crate::workflow::dag_executor::NodeOutcome;
use crate::workflow::dag_parser::dag_to_step_configs;
use crate::workflow::dag_runtime::{DagRuntime, DagWorkflowResult, LayerAdvance};
use crate::workflow::dag_schema::{ContextMode, DagNodeDef, DagWorkflowDef};

// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────

/// All dependencies required to execute a DAG workflow.
///
/// The caller is responsible for creating the execution_id (e.g. a UUID) and
/// ensuring the `DagWorkflowDef` has already been parsed and validated.
pub struct DagDriverDeps {
    pub app_state: Arc<AppState>,
    pub config_storage: Arc<TokioMutex<ConfigStorage>>,
    pub app_handle: Option<tauri::AppHandle>,
    pub execution_id: String,
    pub task_run_id: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Primary entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Execute a parsed DAG workflow to completion.
///
/// This is the async entry point used by callers that have already parsed
/// a `DagWorkflowDef`. It:
///
/// 1. Builds a `DagRuntime` from the definition.
/// 2. Pre-computes `ExecutionStepConfig`s keyed by node ID.
/// 3. Iterates layers via `runtime.advance()`, dispatching ready nodes to
///    `StepExecutor` concurrently within each layer.
/// 4. Consults the event log before each dispatch for crash-recovery replay.
/// 5. Reports outcomes back to the runtime and continues until complete.
/// 6. Returns a `DagWorkflowResult` with per-node metrics.
pub async fn execute_dag_workflow(
    def: DagWorkflowDef,
    deps: DagDriverDeps,
) -> Result<DagWorkflowResult, String> {
    let pg_db = deps.app_state.pg_db.clone();
    let execution_id = deps.execution_id.clone();

    // ── Build runtime ────────────────────────────────────────────────────────
    let mut runtime = DagRuntime::new(def.clone())
        .map_err(|e| format!("Failed to build DAG runtime: {}", e))?;

    // ── Pre-compute step configs keyed by node_id ────────────────────────────
    let step_configs = dag_to_step_configs(&def)
        .map_err(|e| format!("Failed to convert DAG to step configs: {}", e))?;

    let config_map: HashMap<String, ExecutionStepConfig> = step_configs
        .into_iter()
        .filter_map(|cfg| cfg.id.clone().map(|id| (id, cfg)))
        .collect();

    // ── Per-node duration and retry tracking ─────────────────────────────────
    let mut node_durations: HashMap<String, u64> = HashMap::new();
    let mut node_retry_counts: HashMap<String, u32> = HashMap::new();

    info!(
        execution_id = %execution_id,
        workflow = %def.name,
        "Starting DAG workflow execution"
    );

    // ── Main driver loop ─────────────────────────────────────────────────────
    loop {
        let advance = runtime.advance();

        match advance {
            LayerAdvance::Ready {
                layer_index,
                nodes,
                skipped,
            } => {
                info!(
                    execution_id = %execution_id,
                    layer = layer_index,
                    ready = nodes.len(),
                    skipped = skipped.len(),
                    "Processing DAG layer"
                );

                // Log skipped nodes to the event log.
                for s in &skipped {
                    let _ = pg_db
                        .event_log_append(
                            &execution_id,
                            &s.node_id,
                            &EventType::Skipped,
                            Some(&json!({ "reason": s.reason })),
                        )
                        .await;
                }

                // Build per-node futures for parallel dispatch.
                let mut node_futures = Vec::new();

                for ready_node in &nodes {
                    let node_id = ready_node.node_id.clone();
                    let pg = pg_db.clone();
                    let exec_id = execution_id.clone();

                    // ── Crash-recovery replay check ──────────────────────────
                    let cached = pg.event_log_node_completed(&exec_id, &node_id).await;

                    if let Ok(Some(cached_output)) = cached {
                        info!(
                            node_id = %node_id,
                            "Replaying completed node from event log (crash recovery)"
                        );
                        // Return the cached result immediately without re-executing.
                        node_futures.push(
                            Box::pin(async move {
                                (node_id, NodeOutcome::Success, Some(cached_output), 0u64, 0u32)
                            })
                                as std::pin::Pin<
                                    Box<
                                        dyn std::future::Future<
                                            Output = (
                                                String,
                                                NodeOutcome,
                                                Option<serde_json::Value>,
                                                u64,
                                                u32,
                                            ),
                                        > + Send,
                                    >,
                                >,
                        );
                        continue;
                    }

                    // ── Sub-workflow depth validation ────────────────────────
                    // Validate workflow_ref nodes before dispatching to prevent
                    // recursive sub-workflow chains that exceed the depth limit.
                    // Top-level DAG executions always start at depth 0.
                    if let Some(node_def) = def.nodes.get(&node_id) {
                        if let Err(e) = crate::workflow::dag_context::validate_node_sub_workflow(
                            node_def,
                            0, // top-level DAG execution depth
                        ) {
                            warn!(
                                node_id = %node_id,
                                error = %e,
                                "Sub-workflow depth validation failed — marking node failed"
                            );
                            let _ = pg
                                .event_log_append(
                                    &exec_id,
                                    &node_id,
                                    &EventType::Failed,
                                    Some(&json!({ "error": e, "reason": "sub_workflow_depth_exceeded" })),
                                )
                                .await;
                            node_futures.push(Box::pin(async move {
                                (node_id, NodeOutcome::Failed, None, 0u64, 0u32)
                            })
                                as std::pin::Pin<Box<dyn std::future::Future<Output = (String, NodeOutcome, Option<serde_json::Value>, u64, u32)> + Send>>);
                            continue;
                        }
                    }

                    // ── Look up the pre-computed step config ─────────────────
                    let mut step_config = match config_map.get(&node_id) {
                        Some(cfg) => cfg.clone(),
                        None => {
                            warn!(
                                node_id = %node_id,
                                "No step config found for node — marking failed"
                            );
                            node_futures.push(Box::pin(async move {
                                (node_id, NodeOutcome::Failed, None, 0u64, 0u32)
                            })
                                as std::pin::Pin<Box<dyn std::future::Future<Output = (String, NodeOutcome, Option<serde_json::Value>, u64, u32)> + Send>>);
                            continue;
                        }
                    };

                    // ── Feature 2: Fresh context for prompt nodes ────────────
                    // Before entering the async block, check if this prompt node
                    // requires fresh context.  We resolve it here (synchronously,
                    // before the future is spawned) so we don't need to move the
                    // variable store into the future.
                    if step_config.step_type == "prompt" {
                        if let Some(node_def) = def.nodes.get(&node_id) {
                            if matches!(node_def.context, Some(ContextMode::Fresh)) {
                                let variables_snapshot = runtime.variable_store().clone();
                                if let Some(fresh_prompt) =
                                    crate::workflow::dag_context::resolve_node_context(
                                        node_def,
                                        "",  // base_prompt not available at dag_driver level
                                        0,   // iteration 0 for single-pass DAG
                                        &variables_snapshot,
                                        &[], // no verification failures in DAG context
                                        &[], // no iteration diffs in DAG context
                                    )
                                {
                                    step_config.prompt_content = Some(fresh_prompt);
                                }
                            }
                        }
                    }

                    // ── Feature 1: Extract retry config before async block ───
                    // Clone the retry config values from the definition BEFORE
                    // entering the async block so we don't borrow `def` inside.
                    let (max_attempts, delay_ms, backoff) =
                        if let Some(node_def) = def.nodes.get(&node_id) {
                            if let Some(ref rc) = node_def.retry {
                                (
                                    rc.max_attempts.max(1),
                                    rc.delay_ms,
                                    rc.backoff_multiplier.unwrap_or(1.0),
                                )
                            } else {
                                (1u32, 0u64, 1.0f64)
                            }
                        } else {
                            (1u32, 0u64, 1.0f64)
                        };

                    // ── Log Started event ────────────────────────────────────
                    let _ = pg
                        .event_log_append(&exec_id, &node_id, &EventType::Started, None)
                        .await;

                    // ── Feature 3: Loop node inline execution ────────────────
                    // dag_loop nodes are handled by the driver, not StepExecutor.
                    // They run synchronously within their own task.
                    if step_config.step_type == "dag_loop" {
                        // Extract the node_def we need inside the future.
                        let loop_node_def = def.nodes.get(&node_id).cloned();

                        // Clone all Arc/owned values needed inside the async block.
                        let app_state_loop = deps.app_state.clone();
                        let config_storage_loop = deps.config_storage.clone();
                        let app_handle_loop = deps.app_handle.clone();
                        let task_run_id_loop = deps.task_run_id.clone();
                        let pg_log_loop = pg.clone();
                        let exec_id_log_loop = exec_id.clone();
                        let node_id_log_loop = node_id.clone();
                        let config_map_loop = config_map.clone();

                        let fut = async move {
                            let (outcome, output_data, duration_ms) = match loop_node_def {
                                Some(ref nd) => {
                                    execute_loop_node(
                                        &node_id_log_loop,
                                        nd,
                                        &config_map_loop,
                                        app_state_loop,
                                        config_storage_loop,
                                        app_handle_loop,
                                        task_run_id_loop,
                                        &pg_log_loop,
                                        &exec_id_log_loop,
                                    )
                                    .await
                                }
                                None => (NodeOutcome::Failed, None, 0u64),
                            };

                            let success = matches!(outcome, NodeOutcome::Success);
                            let event_type = if success { EventType::Completed } else { EventType::Failed };
                            let event_data = json!({
                                "success": success,
                                "duration_ms": duration_ms,
                                "output": output_data,
                            });
                            let _ = pg_log_loop
                                .event_log_append(
                                    &exec_id_log_loop,
                                    &node_id_log_loop,
                                    &event_type,
                                    Some(&event_data),
                                )
                                .await;

                            (node_id_log_loop, outcome, output_data, duration_ms, 0u32)
                        };

                        node_futures.push(Box::pin(fut)
                            as std::pin::Pin<Box<dyn std::future::Future<Output = (String, NodeOutcome, Option<serde_json::Value>, u64, u32)> + Send>>);
                        continue;
                    }

                    // Clone all Arc/owned values needed inside the async block.
                    let app_state = deps.app_state.clone();
                    let config_storage = deps.config_storage.clone();
                    let app_handle = deps.app_handle.clone();
                    let task_run_id = deps.task_run_id.clone();
                    let pg_log = pg.clone();
                    let exec_id_log = exec_id.clone();
                    let node_id_log = node_id.clone();

                    // ── Dispatch node execution as a future ──────────────────
                    // Each node builds its own StepExecutor from cloned Arcs to
                    // avoid cross-task borrow issues (StepExecutor is not Arc-shared).
                    let fut = async move {
                        // Build a fresh StepExecutor for this node.
                        let mut executor =
                            build_step_executor(app_state, config_storage, app_handle);
                        if let Some(ref id) = task_run_id {
                            executor.set_task_run_id(id.clone());
                        }

                        // ── Feature 1: Retry loop ────────────────────────────
                        let mut attempt = 0u32;
                        let mut last_success = false;
                        let mut last_error: Option<String> = None;
                        let mut last_output: Option<serde_json::Value> = None;
                        let mut total_duration_ms = 0u64;

                        loop {
                            attempt += 1;
                            let start = Instant::now();
                            let (success, error, _screenshot, output_data) =
                                executor.execute_single_step(&step_config).await;
                            total_duration_ms += start.elapsed().as_millis() as u64;

                            if success || attempt >= max_attempts {
                                last_success = success;
                                last_error = error;
                                last_output = output_data;
                                break;
                            }

                            // Log retry event before waiting.
                            let _ = pg_log
                                .event_log_append(
                                    &exec_id_log,
                                    &node_id_log,
                                    &EventType::Retried,
                                    Some(&json!({ "attempt": attempt, "error": error })),
                                )
                                .await;

                            // Exponential backoff: delay_ms * backoff^(attempt-1)
                            let wait_ms = (delay_ms as f64
                                * backoff.powi((attempt - 1) as i32))
                                as u64;
                            if wait_ms > 0 {
                                tokio::time::sleep(std::time::Duration::from_millis(wait_ms))
                                    .await;
                            }

                            last_error = error;
                            last_output = output_data;
                        }

                        let retries = attempt - 1; // retries = attempts - 1

                        let outcome = if last_success {
                            NodeOutcome::Success
                        } else {
                            NodeOutcome::Failed
                        };

                        // Persist Completed / Failed event.
                        let event_type = if last_success {
                            EventType::Completed
                        } else {
                            EventType::Failed
                        };
                        let event_data = json!({
                            "success": last_success,
                            "error": last_error,
                            "duration_ms": total_duration_ms,
                            "output": last_output,
                            "retries": retries,
                        });
                        let _ = pg_log
                            .event_log_append(
                                &exec_id_log,
                                &node_id_log,
                                &event_type,
                                Some(&event_data),
                            )
                            .await;

                        (node_id_log, outcome, last_output, total_duration_ms, retries)
                    };

                    node_futures.push(Box::pin(fut)
                        as std::pin::Pin<Box<dyn std::future::Future<Output = (String, NodeOutcome, Option<serde_json::Value>, u64, u32)> + Send>>);
                }

                // ── Await all node futures in parallel ───────────────────────
                let results = join_all(node_futures).await;

                // ── Report outcomes back to the runtime ──────────────────────
                for (node_id, outcome, output, duration_ms, retries) in results {
                    node_durations.insert(node_id.clone(), duration_ms);
                    if retries > 0 {
                        node_retry_counts.insert(node_id.clone(), retries);
                    }
                    runtime.report_node_outcome(&node_id, outcome, output);
                }
            }

            LayerAdvance::Complete { success, .. } => {
                info!(
                    execution_id = %execution_id,
                    success,
                    "DAG workflow complete"
                );
                break;
            }

            LayerAdvance::Cancelled { reason } => {
                info!(
                    execution_id = %execution_id,
                    reason = %reason,
                    "DAG workflow cancelled"
                );
                // Log the cancellation to the event log with a sentinel node_id.
                let _ = pg_db
                    .event_log_append(
                        &execution_id,
                        "__workflow__",
                        &EventType::Cancelled,
                        Some(&json!({ "reason": reason })),
                    )
                    .await;
                break;
            }
        }
    }

    let result = runtime.build_result(&node_durations, &node_retry_counts);
    Ok(result)
}

/// Convert a `DagWorkflowResult` into the `WorkflowResult` expected by
/// `spawn_workflow_with_panic_guard`.
///
/// This bridges the DAG execution system with the existing workflow lifecycle
/// (task_run completion, learning_outcomes, etc.).
pub fn dag_result_to_workflow_result(
    dag_result: &DagWorkflowResult,
    duration_ms: u64,
) -> crate::unified_workflow_executor::WorkflowResult {
    crate::unified_workflow_executor::WorkflowResult {
        success: dag_result.success,
        verification_passed: dag_result.success,
        step_results: Vec::new(),
        duration_ms,
        loop_result: None,
        worktree_path: None,
        worktree_branch: None,
        workflow_architecture: None,
        agentic_verification_config: None,
        multi_agent_pipeline_config: None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Feature 3: Loop node execution
// ─────────────────────────────────────────────────────────────────────────────

/// Execute a `dag_loop` node inline.
///
/// Iterates over the loop body nodes sequentially for each iteration.
/// Checks `until_bash` exit code to break early.  Prunes the event log at
/// `commit_interval` checkpoints to keep memory usage bounded.
///
/// Returns `(outcome, last_output, total_duration_ms)`.
#[allow(clippy::too_many_arguments)]
async fn execute_loop_node(
    node_id: &str,
    node_def: &DagNodeDef,
    config_map: &HashMap<String, ExecutionStepConfig>,
    app_state: Arc<AppState>,
    config_storage: Arc<TokioMutex<ConfigStorage>>,
    app_handle: Option<tauri::AppHandle>,
    task_run_id: Option<String>,
    pg_db: &crate::database::pg::PgDb,
    execution_id: &str,
) -> (NodeOutcome, Option<serde_json::Value>, u64) {
    let body_ids = match &node_def.loop_body {
        Some(ids) if !ids.is_empty() => ids.clone(),
        _ => {
            warn!(node_id, "dag_loop node has empty or missing loop_body — marking failed");
            return (NodeOutcome::Failed, None, 0);
        }
    };

    let max_iters = node_def.max_loop_iterations.unwrap_or(100);
    let commit_interval = node_def.commit_interval.unwrap_or(0);
    let mut total_duration_ms = 0u64;
    let mut last_output: Option<serde_json::Value> = None;

    for iteration in 0..max_iters {
        // Execute each body node sequentially within the loop.
        for body_id in &body_ids {
            let step = match config_map.get(body_id) {
                Some(s) => s.clone(),
                None => {
                    warn!(
                        node_id,
                        body_id,
                        "Loop body node not found in config_map — skipping"
                    );
                    continue;
                }
            };

            let mut executor = build_step_executor(
                app_state.clone(),
                config_storage.clone(),
                app_handle.clone(),
            );
            if let Some(ref id) = task_run_id {
                executor.set_task_run_id(id.clone());
            }

            let start = Instant::now();
            let (success, _error, _screenshot, output) =
                executor.execute_single_step(&step).await;
            total_duration_ms += start.elapsed().as_millis() as u64;
            last_output = output;

            if !success {
                warn!(
                    node_id,
                    body_id,
                    iteration,
                    "Loop body node failed — aborting loop"
                );
                return (NodeOutcome::Failed, last_output, total_duration_ms);
            }
        }

        // Check until_bash: exit code 0 means the termination condition is met.
        if let Some(ref bash_cmd) = node_def.until_bash {
            let exit_code = tokio::process::Command::new("bash")
                .arg("-c")
                .arg(bash_cmd)
                .status()
                .await
                .map(|s| s.code().unwrap_or(1))
                .unwrap_or(1);
            if exit_code == 0 {
                info!(
                    node_id,
                    iteration,
                    "Loop until_bash condition met — breaking"
                );
                break;
            }
        }

        // Commit interval: prune old event log entries to keep DB size bounded.
        if commit_interval > 0 && (iteration + 1) % commit_interval == 0 {
            let cursor = pg_db
                .event_log_latest_cursor(execution_id)
                .await
                .unwrap_or(0);
            let _ = pg_db.event_log_prune_before(execution_id, cursor).await;
        }
    }

    (NodeOutcome::Success, last_output, total_duration_ms)
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a fresh `StepExecutor` from the given dependency Arcs.
///
/// Each parallel node task creates its own executor to avoid shared mutable
/// state across concurrent futures. Because all expensive resources
/// (`AppState`, `ConfigStorage`) are behind `Arc`, this is cheap.
fn build_step_executor(
    app_state: Arc<AppState>,
    config_storage: Arc<TokioMutex<ConfigStorage>>,
    app_handle: Option<tauri::AppHandle>,
) -> StepExecutor {
    if let Some(handle) = app_handle {
        StepExecutor::with_app_handle(app_state, config_storage, handle)
    } else {
        StepExecutor::new(app_state, config_storage)
    }
}
