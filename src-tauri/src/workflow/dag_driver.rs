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
use crate::workflow::dag_schema::DagWorkflowDef;

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

    // ── Per-node duration tracking ───────────────────────────────────────────
    let mut node_durations: HashMap<String, u64> = HashMap::new();

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
                                (node_id, NodeOutcome::Success, Some(cached_output), 0u64)
                            })
                                as std::pin::Pin<
                                    Box<
                                        dyn std::future::Future<
                                            Output = (
                                                String,
                                                NodeOutcome,
                                                Option<serde_json::Value>,
                                                u64,
                                            ),
                                        > + Send,
                                    >,
                                >,
                        );
                        continue;
                    }

                    // ── Look up the pre-computed step config ─────────────────
                    let step_config = match config_map.get(&node_id) {
                        Some(cfg) => cfg.clone(),
                        None => {
                            warn!(
                                node_id = %node_id,
                                "No step config found for node — marking failed"
                            );
                            node_futures.push(Box::pin(async move {
                                (node_id, NodeOutcome::Failed, None, 0u64)
                            })
                                as std::pin::Pin<Box<dyn std::future::Future<Output = (String, NodeOutcome, Option<serde_json::Value>, u64)> + Send>>);
                            continue;
                        }
                    };

                    // ── Log Started event ────────────────────────────────────
                    let _ = pg
                        .event_log_append(&exec_id, &node_id, &EventType::Started, None)
                        .await;

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

                        let start = Instant::now();
                        let (success, error, _screenshot, output_data) =
                            executor.execute_single_step(&step_config).await;
                        let duration_ms = start.elapsed().as_millis() as u64;

                        let outcome = if success {
                            NodeOutcome::Success
                        } else {
                            NodeOutcome::Failed
                        };

                        // Persist Completed / Failed event.
                        let event_type = if success {
                            EventType::Completed
                        } else {
                            EventType::Failed
                        };
                        let event_data = json!({
                            "success": success,
                            "error": error,
                            "duration_ms": duration_ms,
                            "output": output_data,
                        });
                        let _ = pg_log
                            .event_log_append(
                                &exec_id_log,
                                &node_id_log,
                                &event_type,
                                Some(&event_data),
                            )
                            .await;

                        (node_id_log, outcome, output_data, duration_ms)
                    };

                    node_futures.push(Box::pin(fut)
                        as std::pin::Pin<Box<dyn std::future::Future<Output = (String, NodeOutcome, Option<serde_json::Value>, u64)> + Send>>);
                }

                // ── Await all node futures in parallel ───────────────────────
                let results = join_all(node_futures).await;

                // ── Report outcomes back to the runtime ──────────────────────
                for (node_id, outcome, output, duration_ms) in results {
                    node_durations.insert(node_id.clone(), duration_ms);
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

    let result = runtime.build_result(&node_durations);
    Ok(result)
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
