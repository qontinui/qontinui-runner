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
//! **and its `step_fingerprint` still matches the work about to run**, the
//! cached output is used and the node is not re-executed. This makes DAG
//! execution idempotent across runner restarts.
//!
//! The fingerprint is what stops idempotence from becoming staleness. The
//! journal key carries no content at all, so before it existed, editing a
//! node's prompt and re-running under the same `execution_id` served the OLD
//! output while still reporting a resume. A row whose fingerprint differs — or
//! that has none, which is every row written before the column shipped — is a
//! MISS: the node re-executes and the journal is rewritten. See
//! `crate::workflow_state::fingerprint` for exactly which inputs are hashed
//! and which are deliberately not.
//!
//! Loop-body work is journaled the same way, under the composite key
//! `"<loop_node_id>/iter<N>/<body_node_id>"` (see `loop_body_journal_key`),
//! so resuming a partially-run loop replays the iterations that already
//! finished instead of re-executing — and re-billing — them.
//!
//! Journal writes are never discarded silently: every append goes through
//! `journal_append`, which retries a bounded number of times and then logs.
//! A lost `completed` append is logged at ERROR because it *guarantees*
//! re-execution on resume.
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
use tracing::{error, info, warn};

use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::database::pg::event_log::{EventType, NodeReplay};
use crate::step_executor::executor_types::ExecutionStepConfig;
use crate::step_executor::StepExecutor;
use crate::workflow::dag_executor::NodeOutcome;
use crate::workflow::dag_parser::dag_to_step_configs;
use crate::workflow::dag_runtime::{DagRuntime, DagWorkflowResult, LayerAdvance};
use crate::workflow::dag_schema::{ContextMode, DagNodeDef, DagWorkflowDef};
use crate::workflow::variable_engine::VariableStore;
use crate::workflow_state::StepFingerprint;

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
    let mut runtime =
        DagRuntime::new(def.clone()).map_err(|e| format!("Failed to build DAG runtime: {}", e))?;

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
                    // No fingerprint: a skipped node produced no output, and
                    // only `completed` rows are ever replayed.
                    journal_append(
                        &pg_db,
                        &execution_id,
                        &s.node_id,
                        EventType::Skipped,
                        Some(&json!({ "reason": s.reason })),
                        None,
                    )
                    .await;
                }

                // Build per-node futures for parallel dispatch.
                let mut node_futures = Vec::new();

                for ready_node in &nodes {
                    let node_id = ready_node.node_id.clone();
                    let pg = pg_db.clone();
                    let exec_id = execution_id.clone();

                    // ── Resolve the step config BEFORE the replay check ──────
                    // The replay lookup is fingerprint-gated, and the
                    // fingerprint is taken from the RESOLVED config (a
                    // fresh-context prompt included), so the config has to
                    // exist first. This is a reordering only: the "no step
                    // config" branch below still marks the node failed exactly
                    // as it did, and `resolve_node_context` is a pure function
                    // of the node definition plus the current variable store.
                    let prepared_config: Option<ExecutionStepConfig> =
                        config_map.get(&node_id).cloned().map(|mut cfg| {
                            // Feature 2: fresh context for prompt nodes.
                            if cfg.step_type == "prompt" {
                                if let Some(node_def) = def.nodes.get(&node_id) {
                                    if matches!(node_def.context, Some(ContextMode::Fresh)) {
                                        let variables_snapshot = runtime.variable_store().clone();
                                        if let Some(fresh_prompt) =
                                            crate::workflow::dag_context::resolve_node_context(
                                                node_def,
                                                "", // base_prompt not available at dag_driver level
                                                0,  // iteration 0 for single-pass DAG
                                                &variables_snapshot,
                                                &[], // no verification failures in DAG context
                                                &[], // no iteration diffs in DAG context
                                            )
                                        {
                                            cfg.prompt_content = Some(fresh_prompt);
                                        }
                                    }
                                }
                            }
                            cfg
                        });

                    // An unresolvable config yields an EMPTY fingerprint, which
                    // never matches — so the node re-executes rather than
                    // replaying against a definition we could not read.
                    let node_fingerprint = prepared_config
                        .as_ref()
                        .map(|cfg| {
                            dag_node_fingerprint(
                                cfg,
                                def.nodes.get(&node_id),
                                &config_map,
                                runtime.variable_store(),
                            )
                        })
                        .unwrap_or_default();

                    // ── Crash-recovery replay check ──────────────────────────
                    let cached = pg
                        .event_log_node_completed(&exec_id, &node_id, &node_fingerprint)
                        .await;

                    if let Err(ref e) = cached {
                        // Not knowing is not the same as "not completed": the
                        // node is about to be re-executed (and re-billed) on
                        // the strength of a failed query, so say so.
                        warn!(
                            execution_id = %exec_id,
                            node_id = %node_id,
                            error = %e,
                            "Replay lookup failed — node will be re-executed and re-billed"
                        );
                    }

                    if let Ok(NodeReplay::FingerprintMismatch { ref stored }) = cached {
                        // Distinguishable from "no journal row" on purpose:
                        // this node DID complete here, but the row does not
                        // vouch for the current definition. The re-execution is
                        // correct either way — but the CAUSE is not.
                        //
                        // `stored: None` is not "the inputs changed": it is a
                        // row that carries no fingerprint at all, either a
                        // legacy row or — while the `step_fingerprint`
                        // migration is merged but not yet DEPLOYED — every row,
                        // because the column read comes back absent. Reporting
                        // that as "the definition changed" would make the
                        // undeployed-migration window indistinguishable from a
                        // real edit, and during that window it is the only
                        // thing this log would say.
                        if stored.is_none() {
                            info!(
                                execution_id = %exec_id,
                                node_id = %node_id,
                                journalled_fingerprint = ?stored,
                                expected_fingerprint = %node_fingerprint,
                                "Replay skipped: no journalled fingerprint (legacy row, or the \
                                 step_fingerprint migration is not yet deployed) — node will be \
                                 re-executed"
                            );
                        } else {
                            info!(
                                execution_id = %exec_id,
                                node_id = %node_id,
                                journalled_fingerprint = ?stored,
                                expected_fingerprint = %node_fingerprint,
                                "Replay skipped: the node definition or its inputs changed since \
                                 the journalled run — node will be re-executed"
                            );
                        }
                    }

                    if let Ok(NodeReplay::Hit(cached_output)) = cached {
                        info!(
                            node_id = %node_id,
                            "Replaying completed node from event log (crash recovery)"
                        );
                        // Return the cached result immediately without re-executing.
                        node_futures.push(Box::pin(async move {
                            (
                                node_id,
                                NodeOutcome::Success,
                                Some(cached_output),
                                0u64,
                                0u32,
                            )
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
                            >);
                        continue;
                    }

                    // ── Sub-workflow depth validation ────────────────────────
                    // Validate workflow_ref nodes before dispatching to prevent
                    // recursive sub-workflow chains that exceed the depth limit.
                    // Top-level DAG executions always start at depth 0.
                    if let Some(node_def) = def.nodes.get(&node_id) {
                        if let Err(e) = crate::workflow::dag_context::validate_node_sub_workflow(
                            node_def, 0, // top-level DAG execution depth
                        ) {
                            warn!(
                                node_id = %node_id,
                                error = %e,
                                "Sub-workflow depth validation failed — marking node failed"
                            );
                            journal_append(
                                &pg,
                                &exec_id,
                                &node_id,
                                EventType::Failed,
                                Some(
                                    &json!({ "error": e, "reason": "sub_workflow_depth_exceeded" }),
                                ),
                                Some(&node_fingerprint),
                            )
                            .await;
                            node_futures.push(Box::pin(async move {
                                (node_id, NodeOutcome::Failed, None, 0u64, 0u32)
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
                                >);
                            continue;
                        }
                    }

                    // ── Take the config resolved above ──────────────────────
                    let step_config = match prepared_config {
                        Some(cfg) => cfg,
                        None => {
                            warn!(
                                node_id = %node_id,
                                "No step config found for node — marking failed"
                            );
                            node_futures.push(Box::pin(async move {
                                (node_id, NodeOutcome::Failed, None, 0u64, 0u32)
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
                                >);
                            continue;
                        }
                    };

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
                    journal_append(
                        &pg,
                        &exec_id,
                        &node_id,
                        EventType::Started,
                        None,
                        Some(&node_fingerprint),
                    )
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
                        let fingerprint_loop = node_fingerprint.clone();

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
                            let event_type = if success {
                                EventType::Completed
                            } else {
                                EventType::Failed
                            };
                            let event_data = json!({
                                "success": success,
                                "duration_ms": duration_ms,
                                "output": output_data,
                            });
                            // This is the ONLY journal write the loop node
                            // itself gets, so losing it un-journals the entire
                            // loop, not one event.
                            journal_append(
                                &pg_log_loop,
                                &exec_id_log_loop,
                                &node_id_log_loop,
                                event_type,
                                Some(&event_data),
                                Some(&fingerprint_loop),
                            )
                            .await;

                            (node_id_log_loop, outcome, output_data, duration_ms, 0u32)
                        };

                        node_futures.push(Box::pin(fut)
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
                            >);
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
                    let fingerprint_log = node_fingerprint.clone();

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
                            journal_append(
                                &pg_log,
                                &exec_id_log,
                                &node_id_log,
                                EventType::Retried,
                                Some(&json!({ "attempt": attempt, "error": error })),
                                Some(&fingerprint_log),
                            )
                            .await;

                            // Exponential backoff: delay_ms * backoff^(attempt-1)
                            let wait_ms =
                                (delay_ms as f64 * backoff.powi((attempt - 1) as i32)) as u64;
                            if wait_ms > 0 {
                                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
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
                        journal_append(
                            &pg_log,
                            &exec_id_log,
                            &node_id_log,
                            event_type,
                            Some(&event_data),
                            Some(&fingerprint_log),
                        )
                        .await;

                        (
                            node_id_log,
                            outcome,
                            last_output,
                            total_duration_ms,
                            retries,
                        )
                    };

                    node_futures.push(Box::pin(fut)
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
                        >);
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
                // `__workflow__` is a sentinel key, not a step: it has no
                // definition to fingerprint and is never replayed.
                journal_append(
                    &pg_db,
                    &execution_id,
                    "__workflow__",
                    EventType::Cancelled,
                    Some(&json!({ "reason": reason })),
                    None,
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
/// Checks `until_bash` exit code to break early.
///
/// # Crash recovery
///
/// Every body-node execution is journaled under
/// `loop_body_journal_key` — `"<loop_id>/iter<N>/<body_id>"` — and consulted
/// before dispatch, so a resumed loop replays the iterations that already
/// finished instead of re-running (and re-billing) them.
///
/// # The `commit_interval` prune is a real trade, not just memory hygiene
///
/// At each `commit_interval` checkpoint the loop prunes its **own** journal
/// rows below the current cursor. That bounds journal growth for long loops,
/// but it *deliberately gives up replay* for the iterations it discards: a
/// crash after a checkpoint re-executes every body node completed since that
/// checkpoint. The prune is scoped to this loop's key subtree
/// ([`crate::database::pg::PgDb::event_log_prune_before`]), so it never costs
/// any other node its replay — an execution-wide prune used to delete the
/// whole upstream DAG's completions.
///
/// Setting `commit_interval` to 0 (the default) disables pruning entirely and
/// keeps full replay fidelity.
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
            warn!(
                node_id,
                "dag_loop node has empty or missing loop_body — marking failed"
            );
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
                        body_id, "Loop body node not found in config_map — skipping"
                    );
                    continue;
                }
            };

            // ── Crash-recovery replay check (per iteration) ──────────────
            // Keyed so iteration 3 of a body node is distinct work from
            // iteration 7 — see `loop_body_journal_key`.
            let journal_key = loop_body_journal_key(node_id, iteration, body_id);
            let body_fingerprint = loop_body_fingerprint(&step, node_id, iteration);

            match pg_db
                .event_log_node_completed(execution_id, &journal_key, &body_fingerprint)
                .await
            {
                Ok(NodeReplay::Hit(recorded)) => {
                    info!(
                        node_id,
                        body_id,
                        iteration,
                        journal_key = %journal_key,
                        "Replaying completed loop body node from event log (crash recovery)"
                    );
                    // Restore exactly what a fresh run would have produced:
                    // the raw step output, not the journal envelope.
                    last_output = recorded.get("output").cloned().filter(|v| !v.is_null());
                    continue;
                }
                Ok(NodeReplay::FingerprintMismatch { stored }) => {
                    // Not the same as "never ran here": this iteration IS
                    // journalled, but the body step's definition changed since.
                    info!(
                        node_id,
                        body_id,
                        iteration,
                        journal_key = %journal_key,
                        journalled_fingerprint = ?stored,
                        expected_fingerprint = %body_fingerprint,
                        "Replay skipped: the loop body definition changed since the journalled \
                         run — body node will be re-executed"
                    );
                }
                Ok(NodeReplay::NoRow) => {}
                Err(e) => {
                    warn!(
                        node_id,
                        body_id,
                        iteration,
                        error = %e,
                        "Loop body replay lookup failed — body node will be re-executed and re-billed"
                    );
                }
            }

            let mut executor = build_step_executor(
                app_state.clone(),
                config_storage.clone(),
                app_handle.clone(),
            );
            if let Some(ref id) = task_run_id {
                executor.set_task_run_id(id.clone());
            }

            let start = Instant::now();
            let (success, error, _screenshot, output) = executor.execute_single_step(&step).await;
            let step_duration_ms = start.elapsed().as_millis() as u64;
            total_duration_ms += step_duration_ms;
            last_output = output;

            let event_type = if success {
                EventType::Completed
            } else {
                EventType::Failed
            };
            journal_append(
                pg_db,
                execution_id,
                &journal_key,
                event_type,
                Some(&json!({
                    "success": success,
                    "error": error,
                    "duration_ms": step_duration_ms,
                    "output": last_output,
                    "loop_node_id": node_id,
                    "body_node_id": body_id,
                    "iteration": iteration,
                })),
                Some(&body_fingerprint),
            )
            .await;

            if !success {
                warn!(
                    node_id,
                    body_id, iteration, "Loop body node failed — aborting loop"
                );
                return (NodeOutcome::Failed, last_output, total_duration_ms);
            }
        }

        // Check until_bash: exit code 0 means the termination condition is met.
        if let Some(ref bash_cmd) = node_def.until_bash {
            // On Windows, bare "bash" resolves via PATH and often lands on
            // WSL's C:\Windows\System32\bash.exe, which errors with
            // `execvpe(/bin/bash) failed` when no WSL distro is installed.
            // Route through ShellCommandHandler so we get Git Bash with
            // MSYS /usr/bin on PATH; on non-Windows the "bash" binary is
            // available on the standard shell PATH so bare invocation is
            // fine.
            #[cfg(target_os = "windows")]
            let exit_code = {
                let (_bash_path, mut c) = crate::step_executor::handlers::shell_command::ShellCommandHandler::spawn_git_bash_with_msys_path();
                c.args(["-c", bash_cmd])
                    .status()
                    .await
                    .map(|s| s.code().unwrap_or(1))
                    .unwrap_or(1)
            };
            #[cfg(not(target_os = "windows"))]
            let exit_code = crate::process_helpers::tokio_no_window("bash")
                .arg("-c")
                .arg(bash_cmd)
                .status()
                .await
                .map(|s| s.code().unwrap_or(1))
                .unwrap_or(1);
            if exit_code == 0 {
                info!(
                    node_id,
                    iteration, "Loop until_bash condition met — breaking"
                );
                break;
            }
        }

        // Commit interval: discard this loop's superseded journal rows to keep
        // DB size bounded for long loops. Scoped to `node_id` and its
        // `"<node_id>/…"` subtree so sibling nodes keep their replay records.
        if commit_interval > 0 && (iteration + 1) % commit_interval == 0 {
            match pg_db.event_log_latest_cursor(execution_id).await {
                Ok(cursor) => {
                    if let Err(e) = pg_db
                        .event_log_prune_before(execution_id, node_id, cursor)
                        .await
                    {
                        warn!(
                            node_id,
                            iteration,
                            error = %e,
                            "Loop checkpoint prune failed — journal keeps growing for this loop"
                        );
                    }
                }
                Err(e) => {
                    // Pruning below a fabricated cursor of 0 is a no-op, so
                    // skip rather than guess.
                    warn!(
                        node_id,
                        iteration,
                        error = %e,
                        "Could not read event-log cursor — skipping loop checkpoint prune"
                    );
                }
            }
        }
    }

    (NodeOutcome::Success, last_output, total_duration_ms)
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// How many times `journal_append` tries a single event-log write before
/// giving up. Journal writes fail overwhelmingly for transient reasons (pool
/// exhaustion, a connection reset mid-workflow), and a retry is far cheaper
/// than the re-execution a lost record causes.
const JOURNAL_APPEND_ATTEMPTS: u32 = 3;

/// Content fingerprint for one DAG node.
///
/// Hashed:
///
/// * the node's RESOLVED prompt (fresh-context substitution already applied by
///   the caller), its model and provider;
/// * the whole authored `ExecutionStepConfig`;
/// * the node's `DagNodeDef`. This is NOT redundant with the config:
///   `dag_to_step_configs` projects a node onto the executor's flat step shape
///   and drops fields that have no executor meaning — `loop_body`, `until`,
///   `until_bash`, `max_loop_iterations`, `commit_interval`, `when`,
///   `trigger_rule`, `retry`, `approval`, `workflow_ref`. Every one of those is
///   authored, and every one changes what the node does. Hashing the whole
///   `DagNodeDef` covers fields added later by construction, rather than by
///   someone remembering to extend a list;
/// * for a `dag_loop` node, the `ExecutionStepConfig` of every body step it
///   names, in `loop_body` order. The loop node's own journal row replays the
///   WHOLE loop, so without this, editing a body step's prompt would leave the
///   loop node's fingerprint untouched and serve the stale loop output — the
///   exact stale hit this column exists to prevent;
/// * the resolved values of the node's DECLARED `inputs` map.
///
/// Deliberately NOT hashed: the execution id, the layer index, timings, or the
/// outputs of upstream nodes this node does not declare an input from. A DAG
/// almost always contains at least one nondeterministic step (a shell command
/// whose output embeds a timestamp or a commit sha); hashing every reachable
/// upstream output would make that one step invalidate everything downstream of
/// it on every single resume, which silently disables replay for the whole
/// graph. `inputs` is the declared data-flow contract, and that is the line.
fn dag_node_fingerprint(
    cfg: &ExecutionStepConfig,
    node_def: Option<&DagNodeDef>,
    config_map: &HashMap<String, ExecutionStepConfig>,
    variables: &VariableStore,
) -> String {
    let mut fp = StepFingerprint::new()
        .with_prompt_opt(cfg.prompt_content.as_deref())
        .with_model(cfg.model.as_deref())
        .with_provider(cfg.provider.as_deref())
        .with_definition(cfg);

    if let Some(nd) = node_def {
        fp = fp.with_definition(nd);

        // A loop node replays its whole body from one journal row, so the body
        // definitions are part of what determines its output.
        if let Some(body_ids) = nd.loop_body.as_ref() {
            for body_id in body_ids {
                match config_map.get(body_id) {
                    Some(body_cfg) => fp = fp.with_definition(body_cfg),
                    // A named-but-missing body step is itself a change in the
                    // loop's meaning; record it rather than silently skipping,
                    // which would make a broken loop hash the same as a loop
                    // with one fewer step.
                    None => fp = fp.with_slice(format!("missing_body.{}", body_id), "1"),
                }
            }
        }

        if let Some(inputs) = nd.inputs.as_ref() {
            let resolved = variables.resolve_inputs(inputs);
            fp = fp.with_upstream_values(resolved.iter());
        }
    }

    fp.digest()
}

/// Content fingerprint for one loop-body step execution.
///
/// The loop node id and iteration are folded in even though
/// [`loop_body_journal_key`] already separates them: the fingerprint is then
/// self-describing, so a future change to the key shape cannot silently make
/// two iterations look like the same work.
///
/// KNOWN GAP, stated rather than hidden: `execute_loop_node` holds no variable
/// store, so a body step's DECLARED `inputs` contribute their references (via
/// the config) but not their resolved values. A body step whose prompt consumes
/// an upstream value can therefore still replay after that value changed. The
/// loop-body journal is bounded per iteration and is pruned by
/// `commit_interval`, so the exposure is one loop's worth of steps, not the
/// graph.
fn loop_body_fingerprint(cfg: &ExecutionStepConfig, loop_node_id: &str, iteration: u32) -> String {
    StepFingerprint::new()
        .with_prompt_opt(cfg.prompt_content.as_deref())
        .with_model(cfg.model.as_deref())
        .with_provider(cfg.provider.as_deref())
        .with_definition(cfg)
        .with_slice("loop_node_id", loop_node_id)
        .with_slice("iteration", iteration.to_string())
        .digest()
}

/// Journal key for one loop-body node execution.
///
/// The outer DAG journal is keyed `(execution_id, node_id)`, which is too
/// coarse for a loop: a body node at iteration 3 is different work from the
/// same node at iteration 7, and collapsing them would let a resume replay
/// iteration 7 with iteration 3's output. The key therefore nests the
/// iteration under the loop node:
///
/// ```text
/// <loop_node_id>/iter<N>/<body_node_id>
/// ```
///
/// The `<loop_node_id>/` prefix is load-bearing twice over: it makes the key
/// unique per loop even when two loops share a body node, and it is exactly
/// the subtree that `event_log_prune_before` scopes a checkpoint prune to.
fn loop_body_journal_key(loop_node_id: &str, iteration: u32, body_node_id: &str) -> String {
    format!("{}/iter{}/{}", loop_node_id, iteration, body_node_id)
}

/// Append an event to the workflow journal, retrying briefly and **never**
/// discarding the failure.
///
/// A dropped append is not cosmetic: the journal is the only thing that stops
/// a resumed workflow from re-executing work that already ran, so a lost
/// `completed` record means the node runs again and is billed again.
///
/// A failed append deliberately does **not** fail the node. The work has
/// already been done and paid for; marking a successful node failed would
/// abort the run and discard every completed node with it, and the resumed run
/// would re-execute this node anyway because the record is still missing. So
/// failing converts a *possible* duplicate charge into a *certain* total loss.
/// Instead the loss is made loud — ERROR for a `completed` append, WARN for
/// the rest — so it is visible in the logs rather than inferred later from a
/// double bill.
async fn journal_append(
    pg: &crate::database::pg::PgDb,
    execution_id: &str,
    node_id: &str,
    event_type: EventType,
    event_data: Option<&serde_json::Value>,
    step_fingerprint: Option<&str>,
) {
    let mut last_error = String::new();

    for attempt in 1..=JOURNAL_APPEND_ATTEMPTS {
        match pg
            .event_log_append(
                execution_id,
                node_id,
                &event_type,
                event_data,
                step_fingerprint,
            )
            .await
        {
            Ok(_) => return,
            Err(e) => {
                last_error = e;
                if attempt < JOURNAL_APPEND_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(50 * attempt as u64)).await;
                }
            }
        }
    }

    if matches!(event_type, EventType::Completed) {
        error!(
            execution_id = %execution_id,
            node_id = %node_id,
            attempts = JOURNAL_APPEND_ATTEMPTS,
            error = %last_error,
            "Failed to journal COMPLETED event — this node is unrecorded and WILL re-execute \
             and re-bill if the workflow resumes"
        );
    } else {
        warn!(
            execution_id = %execution_id,
            node_id = %node_id,
            event_type = event_type.as_str(),
            attempts = JOURNAL_APPEND_ATTEMPTS,
            error = %last_error,
            "Failed to journal workflow event"
        );
    }
}

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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_body_journal_key_distinguishes_iterations() {
        let a = loop_body_journal_key("build", 3, "compile");
        let b = loop_body_journal_key("build", 7, "compile");
        assert_ne!(
            a, b,
            "the same body node at two iterations is two distinct units of work"
        );
        assert_eq!(a, "build/iter3/compile");
    }

    #[test]
    fn loop_body_journal_key_distinguishes_body_nodes_and_loops() {
        assert_ne!(
            loop_body_journal_key("build", 0, "compile"),
            loop_body_journal_key("build", 0, "test"),
        );
        assert_ne!(
            loop_body_journal_key("build", 0, "compile"),
            loop_body_journal_key("deploy", 0, "compile"),
            "two loops sharing a body node must not share a journal key"
        );
    }

    /// The checkpoint prune is scoped to `"<loop_id>"` plus its `"<loop_id>/"`
    /// subtree, so every body key MUST sit under that prefix — otherwise the
    /// prune stops bounding journal growth.
    #[test]
    fn loop_body_journal_key_is_nested_under_the_loop_node_prefix() {
        let key = loop_body_journal_key("build", 12, "compile");
        assert!(
            key.starts_with("build/"),
            "body key {} is not under the loop node's prune scope",
            key
        );
        // A sibling node whose id merely shares a prefix is NOT in the subtree.
        assert!(!"build-other".starts_with("build/"));
    }

    // ── Node fingerprints (Phase 3b) ────────────────────────────────────────

    fn node_def(value: serde_json::Value) -> DagNodeDef {
        serde_json::from_value(value).expect("DagNodeDef fixture")
    }

    fn prompt_cfg(prompt: &str) -> ExecutionStepConfig {
        ExecutionStepConfig {
            step_type: "prompt".to_string(),
            prompt_content: Some(prompt.to_string()),
            ..Default::default()
        }
    }

    /// Identical inputs replay; without this the `assert_ne!` tests below could
    /// all pass because nothing ever matches.
    #[test]
    fn node_fingerprint_is_stable_for_identical_inputs() {
        let cfg = prompt_cfg("summarise the diff");
        let nd = node_def(serde_json::json!({ "prompt": "summarise the diff" }));
        let map = HashMap::new();
        let vars = VariableStore::new();
        assert_eq!(
            dag_node_fingerprint(&cfg, Some(&nd), &map, &vars),
            dag_node_fingerprint(&cfg, Some(&nd), &map, &vars)
        );
    }

    /// The headline defect: an edited prompt must not serve the cached output.
    #[test]
    fn node_fingerprint_changes_when_the_prompt_changes() {
        let nd = node_def(serde_json::json!({ "prompt": "p" }));
        let map = HashMap::new();
        let vars = VariableStore::new();
        assert_ne!(
            dag_node_fingerprint(&prompt_cfg("before"), Some(&nd), &map, &vars),
            dag_node_fingerprint(&prompt_cfg("after"), Some(&nd), &map, &vars),
        );
    }

    #[test]
    fn node_fingerprint_changes_when_the_model_changes() {
        let nd = node_def(serde_json::json!({ "prompt": "p" }));
        let map = HashMap::new();
        let vars = VariableStore::new();
        let mut a = prompt_cfg("p");
        a.model = Some("model-1".into());
        let mut b = prompt_cfg("p");
        b.model = Some("model-2".into());
        assert_ne!(
            dag_node_fingerprint(&a, Some(&nd), &map, &vars),
            dag_node_fingerprint(&b, Some(&nd), &map, &vars),
        );
    }

    /// A DECLARED upstream input whose VALUE changed must miss.
    #[test]
    fn node_fingerprint_changes_when_a_declared_upstream_value_changes() {
        let cfg = prompt_cfg("summarise $report");
        let nd = node_def(serde_json::json!({
            "prompt": "summarise $report",
            "inputs": { "report": "upstream.output" }
        }));
        let map = HashMap::new();

        let mut before = VariableStore::new();
        before.set_output("upstream", serde_json::json!({ "output": "all green" }));
        let mut after = VariableStore::new();
        after.set_output("upstream", serde_json::json!({ "output": "3 failures" }));

        assert_ne!(
            dag_node_fingerprint(&cfg, Some(&nd), &map, &before),
            dag_node_fingerprint(&cfg, Some(&nd), &map, &after),
            "a changed upstream input value must re-execute the node"
        );
        // ...and an unchanged one still replays.
        assert_eq!(
            dag_node_fingerprint(&cfg, Some(&nd), &map, &before),
            dag_node_fingerprint(&cfg, Some(&nd), &map, &before),
        );
    }

    /// `dag_to_step_configs` drops loop fields, so the node definition has to
    /// be hashed as well as the config.
    #[test]
    fn node_fingerprint_covers_loop_fields_the_step_config_does_not_carry() {
        let cfg = ExecutionStepConfig {
            step_type: "dag_loop".to_string(),
            ..Default::default()
        };
        let map = HashMap::new();
        let vars = VariableStore::new();
        let a = node_def(serde_json::json!({
            "loop_body": ["compile"], "max_loop_iterations": 3
        }));
        let b = node_def(serde_json::json!({
            "loop_body": ["compile"], "max_loop_iterations": 9
        }));
        assert_ne!(
            dag_node_fingerprint(&cfg, Some(&a), &map, &vars),
            dag_node_fingerprint(&cfg, Some(&b), &map, &vars),
            "an edited loop bound is not visible in ExecutionStepConfig at all"
        );
    }

    /// A loop node's single journal row replays the WHOLE loop, so editing a
    /// body step must invalidate the loop node too - otherwise the loop replays
    /// its stale output and the edited body step never runs.
    #[test]
    fn loop_node_fingerprint_covers_its_body_step_definitions() {
        let cfg = ExecutionStepConfig {
            step_type: "dag_loop".to_string(),
            ..Default::default()
        };
        let nd = node_def(serde_json::json!({ "loop_body": ["compile"] }));
        let vars = VariableStore::new();

        let mut before = HashMap::new();
        before.insert("compile".to_string(), prompt_cfg("build it"));
        let mut after = HashMap::new();
        after.insert("compile".to_string(), prompt_cfg("build it, but faster"));

        assert_ne!(
            dag_node_fingerprint(&cfg, Some(&nd), &before, &vars),
            dag_node_fingerprint(&cfg, Some(&nd), &after, &vars),
        );

        // A body step that vanished from the config map is a change too, not a
        // silent no-op.
        assert_ne!(
            dag_node_fingerprint(&cfg, Some(&nd), &before, &vars),
            dag_node_fingerprint(&cfg, Some(&nd), &HashMap::new(), &vars),
        );
    }

    /// Editing one node must not invalidate its peers - that is what keeps an
    /// edit's re-billing bounded to the edited node.
    #[test]
    fn editing_one_node_does_not_change_a_peer_fingerprint() {
        let map = HashMap::new();
        let vars = VariableStore::new();
        let peer_cfg = prompt_cfg("peer prompt");
        let peer_def = node_def(serde_json::json!({ "prompt": "peer prompt" }));
        let before = dag_node_fingerprint(&peer_cfg, Some(&peer_def), &map, &vars);

        // A completely different node is edited.
        let _edited = dag_node_fingerprint(
            &prompt_cfg("edited"),
            Some(&node_def(serde_json::json!({ "prompt": "edited" }))),
            &map,
            &vars,
        );

        assert_eq!(
            before,
            dag_node_fingerprint(&peer_cfg, Some(&peer_def), &map, &vars),
            "the peer must still replay"
        );
    }

    #[test]
    fn loop_body_fingerprint_separates_iterations_and_tracks_the_definition() {
        let cfg = prompt_cfg("do the thing");
        assert_ne!(
            loop_body_fingerprint(&cfg, "build", 0),
            loop_body_fingerprint(&cfg, "build", 1),
        );
        assert_ne!(
            loop_body_fingerprint(&cfg, "build", 0),
            loop_body_fingerprint(&cfg, "deploy", 0),
        );
        assert_ne!(
            loop_body_fingerprint(&cfg, "build", 0),
            loop_body_fingerprint(&prompt_cfg("do the OTHER thing"), "build", 0),
        );
        assert_eq!(
            loop_body_fingerprint(&cfg, "build", 0),
            loop_body_fingerprint(&cfg, "build", 0),
        );
    }
}
