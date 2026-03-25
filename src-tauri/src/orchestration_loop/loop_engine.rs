//! Core orchestration loop engine.
//!
//! Runs the iterative workflow loop: execute → reflect → evaluate exit → between-iterations.

use chrono::Utc;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};
use tracing::{error, info, warn};

use super::context_summarizer::{ContextSummarizer, IterationContext, estimate_tokens};
use super::fix_agent;
use super::intervention;
use super::remote_client::{RunnerClient, SupervisorClient};
use super::stall_detector::StallDetector;
use super::subtask_executor;
use super::task_decomposer;
use super::types::*;
use crate::ai_provider::routing::run_prompt_sync;

/// Shared loop state, protected by a mutex for concurrent access from commands/API.
pub type SharedLoopState = Arc<Mutex<LoopState>>;

/// Internal mutable state for the orchestration loop.
pub struct LoopState {
    pub running: bool,
    pub phase: LoopPhase,
    pub current_iteration: u32,
    pub config: Option<OrchestrationLoopConfig>,
    pub started_at: Option<chrono::DateTime<Utc>>,
    pub error: Option<String>,
    pub iteration_results: Vec<IterationResult>,
    pub stop_tx: Option<watch::Sender<bool>>,
    /// Flag set by external signal (workflow or API) to indicate a restart is needed.
    pub restart_signaled: bool,
    /// Resolved target runner port (set once the loop starts).
    pub resolved_target_port: u16,
}

impl LoopState {
    pub fn new() -> Self {
        Self {
            running: false,
            phase: LoopPhase::Idle,
            current_iteration: 0,
            config: None,
            started_at: None,
            error: None,
            iteration_results: Vec::new(),
            stop_tx: None,
            restart_signaled: false,
            resolved_target_port: 0,
        }
    }

    pub fn to_status(&self) -> OrchestrationLoopStatus {
        let config = self.config.as_ref();
        let port = if self.resolved_target_port > 0 {
            self.resolved_target_port
        } else {
            config.and_then(|c| c.target_runner_port).unwrap_or(0)
        };
        OrchestrationLoopStatus {
            running: self.running,
            phase: self.phase.clone(),
            current_iteration: self.current_iteration,
            max_iterations: config.map(|c| c.max_iterations).unwrap_or(0),
            workflow_id: config.map(|c| c.workflow_id.clone()).unwrap_or_default(),
            target_runner_port: port,
            target_runner_id: config.and_then(|c| c.target_runner_id.clone()),
            is_pipeline: config.map(|c| c.pipeline.is_some()).unwrap_or(false),
            started_at: self.started_at.map(|t| t.to_rfc3339()),
            error: self.error.clone(),
            iteration_results: self.iteration_results.clone(),
        }
    }
}

/// Start an orchestration loop. Spawns a background task and returns immediately.
pub async fn start_loop(
    loop_state: SharedLoopState,
    config: OrchestrationLoopConfig,
) -> Result<(), String> {
    let mut state = loop_state.lock().await;
    if state.running {
        return Err("Orchestration loop is already running".to_string());
    }

    // Verify target runner is healthy before starting
    let target_port = config.target_runner_port.unwrap_or_else(|| {
        std::env::var("QONTINUI_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(9876)
    });
    let runner = RunnerClient::new(target_port);
    if !runner.is_healthy().await {
        return Err(format!(
            "Target runner on port {} is not running. Start the runner before running workflows.",
            target_port
        ));
    }

    let (stop_tx, stop_rx) = watch::channel(false);

    state.running = true;
    state.phase = LoopPhase::RunningWorkflow;
    state.current_iteration = 0;
    state.config = Some(config.clone());
    state.started_at = Some(Utc::now());
    state.error = None;
    state.iteration_results.clear();
    state.stop_tx = Some(stop_tx);

    drop(state);

    let loop_state_clone = loop_state.clone();
    tokio::spawn(async move {
        run_loop(loop_state_clone, config, stop_rx).await;
    });

    Ok(())
}

/// Stop the orchestration loop.
pub async fn stop_loop(loop_state: SharedLoopState) -> Result<(), String> {
    let state = loop_state.lock().await;
    if !state.running {
        return Err("Orchestration loop is not running".to_string());
    }

    if let Some(tx) = &state.stop_tx {
        let _ = tx.send(true);
    }

    Ok(())
}

/// Get current loop status.
pub async fn get_status(loop_state: SharedLoopState) -> OrchestrationLoopStatus {
    let state = loop_state.lock().await;
    state.to_status()
}

/// Signal that a restart is needed between iterations.
/// Called by workflows or the HTTP API endpoint.
pub async fn signal_restart(loop_state: SharedLoopState) -> Result<(), String> {
    let mut state = loop_state.lock().await;
    if !state.running {
        return Err("Orchestration loop is not running".to_string());
    }
    state.restart_signaled = true;
    info!("Orchestration loop: restart signaled");
    Ok(())
}

/// The main loop implementation.
async fn run_loop(
    loop_state: SharedLoopState,
    config: OrchestrationLoopConfig,
    stop_rx: watch::Receiver<bool>,
) {
    // Resolve target runner port (default: self via QONTINUI_PORT or 9876)
    let target_port = config.target_runner_port.unwrap_or_else(|| {
        std::env::var("QONTINUI_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(9876)
    });

    let target_runner_id = config
        .target_runner_id
        .clone()
        .unwrap_or_else(|| "primary".to_string());

    let runner = RunnerClient::new(target_port);
    let supervisor = SupervisorClient::new(config.supervisor_port);

    // Store the resolved port in state so status queries can see it
    {
        let mut state = loop_state.lock().await;
        state.resolved_target_port = target_port;
    }

    info!(
        "Orchestration loop starting: workflow={}, target=:{}, max_iterations={}",
        config.workflow_id, target_port, config.max_iterations
    );

    // Wait for target runner to be healthy before starting
    if !runner.is_healthy().await {
        info!(
            "Waiting for target runner on port {} to be healthy...",
            target_port
        );
        if !runner.wait_for_healthy(120, &stop_rx).await {
            set_error(&loop_state, "Target runner not healthy after 120s").await;
            return;
        }
    }

    // Branch on pipeline mode vs simple mode
    if config.pipeline.is_some() {
        run_pipeline_loop(
            loop_state,
            config,
            stop_rx,
            runner,
            supervisor,
            &target_runner_id,
        )
        .await;
        return;
    }

    // Initialize stall detector and context summarizer from config
    let mut stall_detector = config.stall_detection.as_ref().map(|c| StallDetector::new(c.clone()));
    let mut context_summarizer_opt = config.summarization.as_ref().map(|c| ContextSummarizer::new(c.clone()));

    // Task decomposition / planning phase
    if let Some(ref decomp_config) = config.decomposition {
        if decomp_config.enabled {
            {
                let mut state = loop_state.lock().await;
                state.phase = LoopPhase::Planning;
            }
            let goal = config.workflow_id.clone();
            let prompt = task_decomposer::build_decomposition_prompt(&goal, decomp_config);
            info!("Task decomposition: planning phase for goal '{}'", goal);
            // TODO: AI call via run_prompt_sync(&prompt, None) — prompt and parser are ready
            // let ai_response = run_prompt_sync(&prompt, None);
            // if ai_response.success {
            //     if let Ok(plan) = task_decomposer::parse_decomposition_response(&goal, &ai_response.output, decomp_config) {
            //         info!("Decomposition plan: {} subtasks", plan.subtasks.len());
            //     }
            // }
            info!("Task decomposition: planning phase complete");
            {
                let mut state = loop_state.lock().await;
                state.phase = LoopPhase::Idle;
            }
        }
    }

    for iteration in 1..=config.max_iterations {
        // Check stop signal
        if *stop_rx.borrow() {
            info!("Orchestration loop stopped by user");
            let mut state = loop_state.lock().await;
            state.phase = LoopPhase::Stopped;
            state.running = false;
            return;
        }

        let iter_start = Utc::now();

        {
            let mut state = loop_state.lock().await;
            state.current_iteration = iteration;
            state.phase = LoopPhase::RunningWorkflow;
            state.restart_signaled = false; // Reset for this iteration
        }

        info!("=== Iteration {}/{} ===", iteration, config.max_iterations);

        // --- Phase 1: Execute workflow ---
        info!(
            "Starting workflow '{}' on target runner",
            config.workflow_id
        );
        let task_run_id = match runner.start_workflow(&config.workflow_id).await {
            Ok(id) => {
                info!("Workflow started: task_run_id={}", id);
                id
            }
            Err(e) => {
                set_error(&loop_state, &format!("Failed to start workflow: {}", e)).await;
                return;
            }
        };

        // Poll until workflow completes
        let _workflow_state = match runner.poll_until_complete(&task_run_id, &stop_rx).await {
            Ok(state) => state,
            Err(e) if e == "Loop stopped" => {
                let mut state = loop_state.lock().await;
                state.phase = LoopPhase::Stopped;
                state.running = false;
                return;
            }
            Err(e) => {
                set_error(&loop_state, &format!("Workflow polling failed: {}", e)).await;
                return;
            }
        };

        info!("Workflow completed: task_run_id={}", task_run_id);

        // --- Phase 1b: Check for workflow failure + retry ---
        let workflow_status = runner
            .get_task_run_status_pub(&task_run_id)
            .await
            .unwrap_or_else(|_| "unknown".to_string());

        // Stall detection: record this iteration's action and check for patterns
        let mut stall_detected_this_iteration = None;
        if let Some(ref mut detector) = stall_detector {
            let sig = format!("{}:{}", config.workflow_id, workflow_status);
            detector.record_action(sig, "workflow".to_string(), iteration, None);
            if let Some(pattern) = detector.check() {
                warn!("Stall detected: {}", pattern);
                stall_detected_this_iteration = Some(format!("{}", pattern));
                {
                    let mut state = loop_state.lock().await;
                    state.phase = LoopPhase::StallDetecting;
                }
                // Build intervention prompt for AI (prompt + parser ready)
                let recent_actions: Vec<String> = detector.recent_actions(5)
                    .iter()
                    .map(|a| a.signature.clone())
                    .collect();
                let _intervention_prompt = intervention::build_intervention_prompt(&pattern, &recent_actions);
                // TODO: AI call for intervention
                // let ai_response = run_prompt_sync(&_intervention_prompt, None);
                // if ai_response.success {
                //     let intervention = intervention::parse_intervention_response(&pattern, &ai_response.output);
                //     info!("Intervention suggested: {:?}", intervention.suggested_action);
                // }
                info!("Breaking loop due to stall: {}", pattern);
                // Record iteration result with stall info before breaking
                let result = IterationResult {
                    iteration,
                    started_at: iter_start.to_rfc3339(),
                    completed_at: Utc::now().to_rfc3339(),
                    task_run_id: task_run_id.clone(),
                    reflection_task_run_id: None,
                    fix_count: None,
                    exit_check: ExitCheckResult {
                        should_exit: true,
                        reason: format!("Stall detected: {}", pattern),
                    },
                    generated_workflow_id: None,
                    fixes_implemented: None,
                    rebuild_triggered: None,
                    stall_detected: stall_detected_this_iteration.clone(),
                    context_summarized: None,
                };
                {
                    let mut state = loop_state.lock().await;
                    state.iteration_results.push(result);
                    state.phase = LoopPhase::Complete;
                    state.running = false;
                }
                return;
            }
        }

        let workflow_failed = !matches!(workflow_status.as_str(), "completed" | "complete");

        if workflow_failed {
            info!("Workflow failed with status: {}", workflow_status);

            if config.retry_on_failure {
                // Wait for fixer before retrying
                if config.wait_for_fixer {
                    {
                        let mut state = loop_state.lock().await;
                        state.phase = LoopPhase::WaitingForFixer;
                    }

                    info!(
                        "Waiting for fixer workflow to complete for task_run_id={}",
                        task_run_id
                    );
                    match runner
                        .wait_for_fixer_complete(&task_run_id, 600, &stop_rx)
                        .await
                    {
                        Ok(true) => info!("Fixer workflow completed for {}", task_run_id),
                        Ok(false) => {
                            info!("No fixer workflow found or timed out for {}", task_run_id)
                        }
                        Err(e) if e == "Loop stopped" => {
                            let mut state = loop_state.lock().await;
                            state.phase = LoopPhase::Stopped;
                            state.running = false;
                            return;
                        }
                        Err(e) => warn!("Error waiting for fixer: {}", e),
                    }
                }

                // Record the failed iteration and continue to next
                let reason = format!("Workflow failed (status: {}) — retrying", workflow_status);
                info!("{}", reason);

                let result = IterationResult {
                    iteration,
                    started_at: iter_start.to_rfc3339(),
                    completed_at: Utc::now().to_rfc3339(),
                    task_run_id: task_run_id.clone(),
                    reflection_task_run_id: None,
                    fix_count: None,
                    exit_check: ExitCheckResult {
                        should_exit: false,
                        reason,
                    },
                    generated_workflow_id: None,
                    fixes_implemented: None,
                    rebuild_triggered: None,
                    stall_detected: None,
                    context_summarized: None,
                };

                {
                    let mut state = loop_state.lock().await;
                    state.iteration_results.push(result);
                }

                // Between iterations before retry
                if iteration < config.max_iterations {
                    if let Err(e) = handle_between_iterations(
                        &runner,
                        &supervisor,
                        &config,
                        &target_runner_id,
                        &loop_state,
                        &stop_rx,
                    )
                    .await
                    {
                        set_error(&loop_state, &format!("Between-iterations failed: {}", e)).await;
                        return;
                    }
                }

                continue;
            } else {
                // Not retrying — terminate with error
                set_error(
                    &loop_state,
                    &format!("Workflow failed (status: {})", workflow_status),
                )
                .await;
                return;
            }
        }

        // --- Phase 1c: Wait for fixer (if configured, for successful workflows) ---
        if config.wait_for_fixer {
            {
                let mut state = loop_state.lock().await;
                state.phase = LoopPhase::WaitingForFixer;
            }

            info!(
                "Waiting for fixer workflow to complete for task_run_id={}",
                task_run_id
            );
            match runner
                .wait_for_fixer_complete(&task_run_id, 600, &stop_rx)
                .await
            {
                Ok(true) => info!("Fixer workflow completed for {}", task_run_id),
                Ok(false) => info!("No fixer workflow found or timed out for {}", task_run_id),
                Err(e) if e == "Loop stopped" => {
                    let mut state = loop_state.lock().await;
                    state.phase = LoopPhase::Stopped;
                    state.running = false;
                    return;
                }
                Err(e) => warn!("Error waiting for fixer: {}", e),
            }
        }

        // --- Phase 2: Evaluate exit ---
        {
            let mut state = loop_state.lock().await;
            state.phase = LoopPhase::EvaluatingExit;
        }

        let exit_check = match &config.exit_strategy {
            ExitStrategy::Reflection => {
                evaluate_reflection_exit(&runner, &loop_state, &task_run_id, &stop_rx).await
            }
            ExitStrategy::WorkflowVerification => {
                // Workflow already confirmed successful above (failed workflows are handled earlier)
                Ok((
                    ExitCheckResult {
                        should_exit: true,
                        reason: "Workflow verification passed".to_string(),
                    },
                    None,
                    None,
                ))
            }
            ExitStrategy::FixedIterations => Ok((
                ExitCheckResult {
                    should_exit: iteration >= config.max_iterations,
                    reason: format!("Iteration {}/{}", iteration, config.max_iterations),
                },
                None,
                None,
            )),
        };

        let (exit_check, reflection_task_run_id, fix_count) = match exit_check {
            Ok(ec) => ec,
            Err(e) if e == "Loop stopped" => {
                let mut state = loop_state.lock().await;
                state.phase = LoopPhase::Stopped;
                state.running = false;
                return;
            }
            Err(e) => {
                set_error(&loop_state, &format!("Exit evaluation failed: {}", e)).await;
                return;
            }
        };

        info!(
            "Exit check: should_exit={}, reason={}",
            exit_check.should_exit, exit_check.reason
        );

        // Context summarization: track this iteration's context
        let mut context_summarized_this_iteration = None;
        if let Some(ref mut summarizer) = context_summarizer_opt {
            let ctx = IterationContext {
                iteration,
                workflow_output: format!("Iteration {} completed with status: {}", iteration, workflow_status),
                reflection_findings: vec![],
                fixes_applied: vec![],
                exit_check_reason: exit_check.reason.clone(),
                token_estimate: estimate_tokens(&format!("Iteration {} context", iteration)) + 100,
            };
            summarizer.add_iteration_context(ctx);
            if summarizer.should_summarize() {
                info!("Context summarization triggered at iteration {}", iteration);
                if let Some(prompt) = summarizer.build_summarization_prompt() {
                    // TODO: AI call for summarization
                    // let ai_response = run_prompt_sync(&prompt, None);
                    // if ai_response.success {
                    //     let original_tokens = summarizer.total_token_estimate();
                    //     let summary = summarizer.parse_summary_response(&ai_response.output, original_tokens);
                    //     summarizer.apply_summary(summary);
                    //     context_summarized_this_iteration = Some(true);
                    // }
                    let _ = prompt; // prompt is ready for when AI call is wired
                    context_summarized_this_iteration = Some(false); // not yet wired
                }
            }
        }

        // Record iteration result
        let result = IterationResult {
            iteration,
            started_at: iter_start.to_rfc3339(),
            completed_at: Utc::now().to_rfc3339(),
            task_run_id: task_run_id.clone(),
            reflection_task_run_id,
            fix_count,
            exit_check: exit_check.clone(),
            generated_workflow_id: None,
            fixes_implemented: None,
            rebuild_triggered: None,
            stall_detected: stall_detected_this_iteration,
            context_summarized: context_summarized_this_iteration,
        };

        {
            let mut state = loop_state.lock().await;
            state.iteration_results.push(result);
        }

        // Exit if should_exit
        if exit_check.should_exit {
            info!("Loop complete: {}", exit_check.reason);
            let mut state = loop_state.lock().await;
            state.phase = LoopPhase::Complete;
            state.running = false;
            return;
        }

        // --- Phase 3: Between iterations ---
        if iteration < config.max_iterations {
            if let Err(e) = handle_between_iterations(
                &runner,
                &supervisor,
                &config,
                &target_runner_id,
                &loop_state,
                &stop_rx,
            )
            .await
            {
                set_error(&loop_state, &format!("Between-iterations failed: {}", e)).await;
                return;
            }
        }
    }

    // Reached max iterations
    info!(
        "Orchestration loop completed after {} iterations",
        config.max_iterations
    );
    let mut state = loop_state.lock().await;
    state.phase = LoopPhase::Complete;
    state.running = false;
}

/// Evaluate exit using reflection strategy.
/// Returns (ExitCheckResult, reflection_task_run_id, fix_count).
async fn evaluate_reflection_exit(
    runner: &RunnerClient,
    loop_state: &SharedLoopState,
    task_run_id: &str,
    stop_rx: &watch::Receiver<bool>,
) -> Result<(ExitCheckResult, Option<String>, Option<u32>), String> {
    {
        let mut state = loop_state.lock().await;
        state.phase = LoopPhase::Reflecting;
    }

    info!("Triggering reflection on task_run_id={}", task_run_id);

    // Trigger reflection
    let reflection_id = match runner.trigger_reflection(task_run_id).await? {
        Some(id) => id,
        None => {
            warn!("No reflection task_run_id returned — assuming 0 fixes");
            return Ok((
                ExitCheckResult {
                    should_exit: true,
                    reason: "No reflection available — assuming complete".to_string(),
                },
                None,
                Some(0),
            ));
        }
    };

    info!("Reflection started: task_run_id={}", reflection_id);

    // Wait for reflection to complete
    let _reflection_state = runner.poll_until_complete(&reflection_id, stop_rx).await?;

    // Count fixes
    let fix_count = runner.count_reflection_fixes(&reflection_id).await?;
    info!("Reflection found {} fixes", fix_count);

    let should_exit = fix_count == 0;
    let reason = if should_exit {
        "Reflection found 0 new fixes — workflow is complete".to_string()
    } else {
        format!("Reflection found {} fixes — continuing", fix_count)
    };

    Ok((
        ExitCheckResult {
            should_exit,
            reason,
        },
        Some(reflection_id),
        Some(fix_count),
    ))
}

/// Handle between-iterations action.
async fn handle_between_iterations(
    runner: &RunnerClient,
    supervisor: &SupervisorClient,
    config: &OrchestrationLoopConfig,
    target_runner_id: &str,
    loop_state: &SharedLoopState,
    stop_rx: &watch::Receiver<bool>,
) -> Result<(), String> {
    {
        let mut state = loop_state.lock().await;
        state.phase = LoopPhase::BetweenIterations;
    }

    match &config.between_iterations {
        BetweenIterations::RestartRunner { rebuild } => {
            info!(
                "Restarting target runner '{}' (rebuild={})",
                target_runner_id, rebuild
            );
            supervisor
                .restart_runner(target_runner_id, *rebuild)
                .await?;

            {
                let mut state = loop_state.lock().await;
                state.phase = LoopPhase::WaitingForRunner;
            }

            info!("Waiting for target runner to be healthy...");
            if !runner.wait_for_healthy(120, stop_rx).await {
                return Err("Target runner not healthy after restart".to_string());
            }
            info!("Target runner is healthy");
        }
        BetweenIterations::RestartOnSignal { rebuild } => {
            let signaled = {
                let mut state = loop_state.lock().await;
                let s = state.restart_signaled;
                state.restart_signaled = false; // Consume the signal
                s
            };

            if signaled {
                info!(
                    "RestartOnSignal — restart signaled, restarting target runner '{}' (rebuild={})",
                    target_runner_id, rebuild
                );
                supervisor
                    .restart_runner(target_runner_id, *rebuild)
                    .await?;

                {
                    let mut state = loop_state.lock().await;
                    state.phase = LoopPhase::WaitingForRunner;
                }

                info!("Waiting for target runner to be healthy...");
                if !runner.wait_for_healthy(120, stop_rx).await {
                    return Err("Target runner not healthy after restart".to_string());
                }
                info!("Target runner is healthy");
            } else {
                info!("RestartOnSignal — no signal received, skipping restart");
                // Still wait for runner to be healthy
                {
                    let mut state = loop_state.lock().await;
                    state.phase = LoopPhase::WaitingForRunner;
                }
                if !runner.wait_for_healthy(120, stop_rx).await {
                    return Err("Target runner not healthy".to_string());
                }
            }
        }
        BetweenIterations::WaitHealthy => {
            {
                let mut state = loop_state.lock().await;
                state.phase = LoopPhase::WaitingForRunner;
            }
            info!("Waiting for target runner to be healthy...");
            if !runner.wait_for_healthy(120, stop_rx).await {
                return Err("Target runner not healthy".to_string());
            }
        }
        BetweenIterations::None => {
            info!("No between-iterations action");
        }
    }

    Ok(())
}

/// Pipeline loop: build → execute → reflect → implement fixes → repeat.
async fn run_pipeline_loop(
    loop_state: SharedLoopState,
    config: OrchestrationLoopConfig,
    stop_rx: watch::Receiver<bool>,
    runner: RunnerClient,
    supervisor: SupervisorClient,
    target_runner_id: &str,
) {
    let Some(pipeline) = config.pipeline.as_ref() else {
        error!("Pipeline loop called without pipeline config");
        return;
    };
    let mut current_workflow_id = config.workflow_id.clone();
    let mut rebuild_needed = pipeline.build.is_some(); // Build on first iteration if configured

    info!(
        "Pipeline loop starting: max_iterations={}, has_build={}, has_fixes={}",
        config.max_iterations,
        pipeline.build.is_some(),
        pipeline.implement_fixes.is_some()
    );

    for iteration in 1..=config.max_iterations {
        // Check stop signal
        if *stop_rx.borrow() {
            info!("Pipeline loop stopped by user");
            let mut state = loop_state.lock().await;
            state.phase = LoopPhase::Stopped;
            state.running = false;
            return;
        }

        let iter_start = Utc::now();
        let mut generated_workflow_id = None;
        let mut fixes_implemented = None;
        let mut rebuild_triggered = None;

        {
            let mut state = loop_state.lock().await;
            state.current_iteration = iteration;
            state.restart_signaled = false;
        }

        info!(
            "=== Pipeline Iteration {}/{} ===",
            iteration, config.max_iterations
        );

        // --- Phase 1: Build (conditional) ---
        if rebuild_needed {
            if let Some(build_config) = &pipeline.build {
                {
                    let mut state = loop_state.lock().await;
                    state.phase = LoopPhase::BuildingWorkflow;
                }

                info!("Building workflow from description...");
                match runner
                    .generate_workflow(
                        &build_config.description,
                        build_config.context.as_deref(),
                        build_config.context_ids.as_deref(),
                    )
                    .await
                {
                    Ok((wf_id, _task_run_id)) => {
                        info!("Workflow generated: {}", wf_id);
                        current_workflow_id = wf_id.clone();
                        generated_workflow_id = Some(wf_id);
                        rebuild_needed = false;
                    }
                    Err(e) => {
                        set_error(&loop_state, &format!("Workflow generation failed: {}", e)).await;
                        return;
                    }
                }
            }
        }

        // --- Phase 2: Execute ---
        {
            let mut state = loop_state.lock().await;
            state.phase = LoopPhase::RunningWorkflow;
        }

        if current_workflow_id.is_empty() {
            set_error(&loop_state, "No workflow ID available for execution").await;
            return;
        }

        info!("Executing workflow '{}'", current_workflow_id);
        let task_run_id = match runner.start_workflow(&current_workflow_id).await {
            Ok(id) => {
                info!("Workflow started: task_run_id={}", id);
                id
            }
            Err(e) => {
                set_error(&loop_state, &format!("Failed to start workflow: {}", e)).await;
                return;
            }
        };

        let _workflow_state = match runner.poll_until_complete(&task_run_id, &stop_rx).await {
            Ok(state) => state,
            Err(e) if e == "Loop stopped" => {
                let mut state = loop_state.lock().await;
                state.phase = LoopPhase::Stopped;
                state.running = false;
                return;
            }
            Err(e) => {
                set_error(&loop_state, &format!("Workflow polling failed: {}", e)).await;
                return;
            }
        };

        info!("Workflow completed: task_run_id={}", task_run_id);

        // --- Phase 3: Reflect ---
        {
            let mut state = loop_state.lock().await;
            state.phase = LoopPhase::Reflecting;
        }

        info!("Triggering reflection on task_run_id={}", task_run_id);
        let reflection_id = match runner.trigger_reflection(&task_run_id).await {
            Ok(Some(id)) => id,
            Ok(None) => {
                info!("No reflection available — assuming complete");
                record_pipeline_result(
                    &loop_state,
                    iteration,
                    &iter_start,
                    &task_run_id,
                    None,
                    Some(0),
                    true,
                    "No reflection available",
                    generated_workflow_id,
                    fixes_implemented,
                    rebuild_triggered,
                )
                .await;
                break;
            }
            Err(e) => {
                set_error(&loop_state, &format!("Reflection trigger failed: {}", e)).await;
                return;
            }
        };

        // Wait for reflection to complete
        let _reflection_state = match runner.poll_until_complete(&reflection_id, &stop_rx).await {
            Ok(state) => state,
            Err(e) if e == "Loop stopped" => {
                let mut state = loop_state.lock().await;
                state.phase = LoopPhase::Stopped;
                state.running = false;
                return;
            }
            Err(e) => {
                set_error(&loop_state, &format!("Reflection polling failed: {}", e)).await;
                return;
            }
        };

        // Get fixes
        let fixes = match runner.get_reflection_fixes(&reflection_id).await {
            Ok(f) => f,
            Err(e) => {
                warn!(
                    "Failed to get reflection fixes: {}, falling back to count",
                    e
                );
                Vec::new()
            }
        };

        let fix_count = fixes.len() as u32;
        info!("Reflection found {} fixes", fix_count);

        // Exit if 0 fixes
        if fix_count == 0 {
            info!("Pipeline complete: 0 fixes found");
            record_pipeline_result(
                &loop_state,
                iteration,
                &iter_start,
                &task_run_id,
                Some(reflection_id),
                Some(0),
                true,
                "Reflection found 0 fixes — workflow is complete",
                generated_workflow_id,
                fixes_implemented,
                rebuild_triggered,
            )
            .await;
            break;
        }

        // --- Phase 4: Implement fixes (if configured) ---
        if let Some(fix_config) = &pipeline.implement_fixes {
            if fix_count > 0 {
                {
                    let mut state = loop_state.lock().await;
                    state.phase = LoopPhase::ImplementingFixes;
                }

                let model = fix_config.model.as_deref().unwrap_or("claude-opus-4-6");
                let timeout = fix_config.timeout_secs.unwrap_or(600);

                let prompt =
                    fix_agent::build_fix_prompt(&fixes, fix_config.additional_context.as_deref());

                info!("Implementing {} fixes via Claude CLI...", fix_count);
                match fix_agent::run_fix_agent(&prompt, model, timeout, &stop_rx).await {
                    Ok(true) => {
                        info!("Fixes implemented successfully");
                        fixes_implemented = Some(true);
                    }
                    Ok(false) => {
                        warn!("Fix agent did not complete successfully");
                        fixes_implemented = Some(false);
                    }
                    Err(e) => {
                        warn!("Fix agent error: {}", e);
                        fixes_implemented = Some(false);
                    }
                }

                // Check if any fixes are structural (need workflow rebuild)
                if fix_agent::should_rebuild(&fixes) {
                    info!("Structural fixes detected — will rebuild workflow next iteration");
                    rebuild_needed = true;
                    rebuild_triggered = Some(true);
                }
            }
        }

        // Record iteration result
        let should_exit = iteration >= config.max_iterations;
        let reason = if should_exit {
            format!(
                "Reached max iterations ({}/{})",
                iteration, config.max_iterations
            )
        } else {
            format!("{} fixes found — continuing", fix_count)
        };

        record_pipeline_result(
            &loop_state,
            iteration,
            &iter_start,
            &task_run_id,
            Some(reflection_id),
            Some(fix_count),
            should_exit,
            &reason,
            generated_workflow_id,
            fixes_implemented,
            rebuild_triggered,
        )
        .await;

        if should_exit {
            break;
        }

        // --- Phase 5: Between iterations ---
        if iteration < config.max_iterations {
            if let Err(e) = handle_between_iterations(
                &runner,
                &supervisor,
                &config,
                target_runner_id,
                &loop_state,
                &stop_rx,
            )
            .await
            {
                set_error(&loop_state, &format!("Between-iterations failed: {}", e)).await;
                return;
            }
        }
    }

    // Mark complete
    let mut state = loop_state.lock().await;
    if state.running {
        info!("Pipeline loop completed");
        state.phase = LoopPhase::Complete;
        state.running = false;
    }
}

/// Record a pipeline iteration result.
#[allow(clippy::too_many_arguments)]
async fn record_pipeline_result(
    loop_state: &SharedLoopState,
    iteration: u32,
    iter_start: &chrono::DateTime<Utc>,
    task_run_id: &str,
    reflection_task_run_id: Option<String>,
    fix_count: Option<u32>,
    should_exit: bool,
    reason: &str,
    generated_workflow_id: Option<String>,
    fixes_implemented: Option<bool>,
    rebuild_triggered: Option<bool>,
) {
    let result = IterationResult {
        iteration,
        started_at: iter_start.to_rfc3339(),
        completed_at: Utc::now().to_rfc3339(),
        task_run_id: task_run_id.to_string(),
        reflection_task_run_id,
        fix_count,
        exit_check: ExitCheckResult {
            should_exit,
            reason: reason.to_string(),
        },
        generated_workflow_id,
        fixes_implemented,
        rebuild_triggered,
        stall_detected: None,
        context_summarized: None,
    };

    let mut state = loop_state.lock().await;
    state.iteration_results.push(result);

    if should_exit {
        state.phase = LoopPhase::Complete;
        state.running = false;
    }
}

/// Set error state and stop the loop.
async fn set_error(loop_state: &SharedLoopState, msg: &str) {
    error!("Orchestration loop error: {}", msg);
    let mut state = loop_state.lock().await;
    state.error = Some(msg.to_string());
    state.phase = LoopPhase::Error;
    state.running = false;
}
