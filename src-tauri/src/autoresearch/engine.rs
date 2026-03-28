//! Main autoresearch engine — the async experiment loop.

use super::metrics;
use super::mutations::{AiGuidedMutator, QLearningMutator, RandomPerturbator, SequentialMutator};
use super::types::*;
use crate::orchestration_loop::remote_client::RunnerClient;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};
use tracing::{debug, info, warn};

/// Shared state for the running campaign (accessed from both engine and loop task).
pub struct ResearchState {
    pub status: CampaignStatus,
    pub results: Vec<(u32, ExperimentResult)>,
    /// Cached control aggregate from last re-evaluation.
    pub cached_control_aggregate: Option<(AggregateMetrics, Vec<TrialResult>)>,
}

/// The research engine — manages one campaign at a time.
pub struct ResearchEngine {
    state: Arc<Mutex<ResearchState>>,
    stop_tx: Option<watch::Sender<bool>>,
}

impl ResearchEngine {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ResearchState {
                status: CampaignStatus {
                    campaign_id: String::new(),
                    name: String::new(),
                    status: CampaignState::Stopped,
                    experiment_count: 0,
                    accepted_count: 0,
                    current_control: ExperimentConfig {
                        model: None,
                        max_iterations: None,
                        multi_agent_mode: None,
                        max_context_tokens: None,
                        workflow_architecture: None,
                        agentic_verification_config: None,
                        multi_agent_pipeline_config: None,
                        extra: Default::default(),
                    },
                    current_experiment: None,
                    started_at: None,
                    error: None,
                    experiments_since_last_accept: 0,
                },
                results: Vec::new(),
                cached_control_aggregate: None,
            })),
            stop_tx: None,
        }
    }

    /// Start a new research campaign. Returns error if one is already running.
    pub async fn start(
        &mut self,
        config: ResearchConfig,
        db: Arc<crate::database::CheckpointDb>,
        pg_db: std::sync::Arc<crate::database::pg::PgDb>,
    ) -> Result<String, String> {
        // Check if already running
        {
            let s = self.state.lock().await;
            if s.status.status == CampaignState::Running {
                return Err("A research campaign is already running".to_string());
            }
        }

        let campaign_id = format!(
            "research-{}-{}",
            slug(&config.name),
            chrono::Utc::now().timestamp_millis()
        );

        // Save campaign to DB
        db.execute_sql(
            "INSERT INTO autoresearch_campaigns (id, name, config_json, current_control_json, status, experiment_count, accepted_count, created_at) VALUES (?1, ?2, ?3, ?4, 'running', 0, 0, datetime('now'))",
            &[
                &campaign_id as &dyn rusqlite::types::ToSql,
                &config.name,
                &serde_json::to_string(&config).unwrap_or_default(),
                &serde_json::to_string(&config.control_config).unwrap_or_default(),
            ],
        ).map_err(|e| format!("Failed to save campaign: {}", e))?;

        let (stop_tx, stop_rx) = watch::channel(false);
        self.stop_tx = Some(stop_tx);

        // Initialize state
        {
            let mut s = self.state.lock().await;
            s.status = CampaignStatus {
                campaign_id: campaign_id.clone(),
                name: config.name.clone(),
                status: CampaignState::Running,
                experiment_count: 0,
                accepted_count: 0,
                current_control: config.control_config.clone(),
                current_experiment: None,
                started_at: Some(chrono::Utc::now().to_rfc3339()),
                error: None,
                experiments_since_last_accept: 0,
            };
            s.results.clear();
            s.cached_control_aggregate = None;
        }

        // Spawn the loop
        let state_clone = self.state.clone();
        let campaign_id_clone = campaign_id.clone();
        tokio::spawn(async move {
            run_campaign_loop(config, campaign_id_clone, state_clone, stop_rx, db, pg_db).await;
        });

        info!("Started autoresearch campaign: {}", campaign_id);
        Ok(campaign_id)
    }

    /// Stop the currently running campaign.
    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(true);
            info!("Sent stop signal to autoresearch campaign");
            Ok(())
        } else {
            Err("No campaign is running".to_string())
        }
    }

    /// Get the current campaign status.
    pub async fn status(&self) -> CampaignStatus {
        let s = self.state.lock().await;
        s.status.clone()
    }

    /// Get all experiment results for the current campaign.
    pub async fn results(&self) -> Vec<(u32, ExperimentResult)> {
        let s = self.state.lock().await;
        s.results.clone()
    }

    /// Get results as TSV.
    pub async fn results_tsv(&self) -> String {
        let s = self.state.lock().await;
        metrics::format_results_tsv(&s.results)
    }
}

// =============================================================================
// Mutation strategy dispatcher
// =============================================================================

/// Wraps all mutation strategies behind a common interface.
enum Mutator {
    Sequential(SequentialMutator),
    Random(Box<RandomPerturbator>),
    AiGuided(Box<AiGuidedMutator>),
    QLearning(Box<QLearningMutator>),
}

impl Mutator {
    async fn from_strategy(
        strategy: &MutationStrategy,
        ai_config: &AiGuidanceConfig,
        pg: &crate::database::pg::PgDb,
        pg_q_rows: Option<Vec<(String, String, f64, u32)>>,
    ) -> Self {
        match strategy {
            MutationStrategy::Sequential => Mutator::Sequential(SequentialMutator::new()),
            MutationStrategy::RandomPerturbation => {
                Mutator::Random(Box::new(RandomPerturbator::new()))
            }
            MutationStrategy::AiGuided => Mutator::AiGuided(Box::new(AiGuidedMutator::new(
                ai_config.batch_size,
                ai_config.model.clone(),
            ))),
            MutationStrategy::QLearning => {
                let mut q_router = super::q_router::QRouter::new();
                // Use PG Q-table
                let rows = pg_q_rows.or_else(|| {
                    // Fallback: try to load synchronously via block_on (best-effort)
                    None
                }).or(load_q_table_from_pg(pg).await.ok());
                if let Some(rows) = rows {
                    q_router.load_from_rows(rows);
                    info!(
                        "Q-learning mutator loaded {} Q-table entries",
                        q_router.stats().total_state_action_pairs
                    );
                }
                // Load manual overrides from PG
                if let Ok(overrides) = load_q_overrides_from_pg(pg).await {
                    if !overrides.is_empty() {
                        info!("Q-learning mutator loaded {} overrides", overrides.len());
                        q_router.load_overrides(overrides);
                    }
                }
                Mutator::QLearning(Box::new(QLearningMutator::new(q_router)))
            }
        }
    }

    fn next_experiment(
        &mut self,
        control: &ExperimentConfig,
        dimensions: &[SearchDimension],
        history: &[(u32, ExperimentResult)],
    ) -> Option<ExperimentConfig> {
        match self {
            Mutator::Sequential(m) => m.next_experiment(control, dimensions, history),
            Mutator::Random(m) => m.next_experiment(control, dimensions, history),
            Mutator::AiGuided(m) => m.next_experiment(control, dimensions, history),
            Mutator::QLearning(m) => m.next_experiment(control, dimensions, history),
        }
    }

    /// Get the last AI recommendation text (only for AiGuided).
    fn last_ai_recommendation(&self) -> Option<&str> {
        match self {
            Mutator::AiGuided(m) => m.last_recommendation_text.as_deref(),
            _ => None,
        }
    }
}

// PG helpers delegated to q_router module
async fn load_q_table_from_pg(
    pg: &crate::database::pg::PgDb,
) -> Result<Vec<(String, String, f64, u32)>, String> {
    super::q_router::load_q_table_pg(pg).await
}

async fn load_q_overrides_from_pg(
    pg: &crate::database::pg::PgDb,
) -> Result<Vec<(String, String)>, String> {
    super::q_router::load_overrides_pg(pg).await
}

/// The main experiment loop (runs in a spawned task).
async fn run_campaign_loop(
    mut config: ResearchConfig,
    campaign_id: String,
    state: Arc<Mutex<ResearchState>>,
    stop_rx: watch::Receiver<bool>,
    db: Arc<crate::database::CheckpointDb>,
    pg_db: std::sync::Arc<crate::database::pg::PgDb>,
) {
    let runner = RunnerClient::new(config.target_runner_port);

    // Load Q-table from PG-first if available, fallback to SQLite
    let pg_q_rows = match pg_db.get_all_q_entries().await {
        Ok(rows) if !rows.is_empty() => {
            info!("Loaded {} Q-table entries from PG", rows.len());
            Some(rows)
        }
        _ => None,
    };

    let mut mutator = Mutator::from_strategy(&config.mutation_strategy, &config.ai_guidance, &pg_db, pg_q_rows).await;
    let mut experiment_number: u32 = 0;

    // Start config file watcher if configured
    let config_reload_rx = config
        .config_file_path
        .as_ref()
        .and_then(|path| start_config_watcher(path));

    info!(
        "Autoresearch loop started for campaign '{}' (workflow={}, dimensions={}, strategy={:?}, trials={})",
        config.name,
        config.benchmark_workflow_id,
        config.search_dimensions.len(),
        config.mutation_strategy,
        config.trials_per_experiment,
    );

    loop {
        // Check stop signal
        if *stop_rx.borrow() {
            info!("Autoresearch campaign '{}' stopped by user", campaign_id);
            let mut s = state.lock().await;
            s.status.status = CampaignState::Stopped;
            update_campaign_status(&db, &campaign_id, "stopped", &s.status);
            return;
        }

        // Check for config hot-reload
        if let Some(ref rx) = config_reload_rx {
            if let Ok(new_config) = rx.try_recv() {
                info!(
                    "Hot-reloading research config for campaign '{}'",
                    campaign_id
                );
                // Reload mutable fields only (not benchmark_workflow_id or name)
                config.search_dimensions = new_config.search_dimensions;
                config.acceptance_criteria = new_config.acceptance_criteria;
                config.trials_per_experiment = new_config.trials_per_experiment;
                config.control_reeval_interval = new_config.control_reeval_interval;
                if std::mem::discriminant(&config.mutation_strategy)
                    != std::mem::discriminant(&new_config.mutation_strategy)
                {
                    config.mutation_strategy = new_config.mutation_strategy;
                    // Reload PG Q-data on strategy change
                    let reload_pg_rows = pg_db.get_all_q_entries().await.ok().filter(|r| !r.is_empty());
                    mutator =
                        Mutator::from_strategy(&config.mutation_strategy, &config.ai_guidance, &pg_db, reload_pg_rows).await;
                    info!("Mutation strategy changed — resetting mutator");
                }
                // Invalidate cached control
                let mut s = state.lock().await;
                s.cached_control_aggregate = None;
            }
        }

        // Get current control
        let current_control = {
            let s = state.lock().await;
            s.status.current_control.clone()
        };

        // Pick next experiment
        let history = {
            let s = state.lock().await;
            s.results.clone()
        };
        let experiment_config =
            match mutator.next_experiment(&current_control, &config.search_dimensions, &history) {
                Some(c) => c,
                None => {
                    info!("No more experiments to try — campaign complete");
                    let mut s = state.lock().await;
                    s.status.status = CampaignState::Completed;
                    update_campaign_status(&db, &campaign_id, "completed", &s.status);
                    return;
                }
            };

        experiment_number += 1;
        info!(
            "Experiment #{}: {}",
            experiment_number,
            experiment_config.summary()
        );

        // Update current experiment in status
        {
            let mut s = state.lock().await;
            s.status.current_experiment = Some(experiment_config.clone());
        }

        // Inject worktree isolation if campaign requires it
        let mut experiment_config = experiment_config;
        if config.use_worktree {
            experiment_config
                .extra
                .insert("use_worktree".to_string(), serde_json::json!(true));
        }

        // Run experiment trials (across all benchmark workflows)
        let workflow_ids = config.effective_workflow_ids();
        let mut trials = run_trials_multi_workflow(
            &runner,
            &workflow_ids,
            &experiment_config,
            config.trials_per_experiment,
            experiment_number,
            &stop_rx,
        )
        .await;

        // Enrich trials with spec compliance data (if available)
        for trial in &mut trials {
            if let Ok(Some(compliance)) =
                crate::spec_experimentation::compliance::get_compliance_for_run(
                    &pg_db,
                    &trial.task_run_id,
                )
                .await
            {
                trial.spec_compliance_score = Some(compliance.overall_score);
                trial.spec_assertions_passed = Some(compliance.assertions_passed as u32);
                trial.spec_assertions_total = Some(compliance.assertions_total as u32);
            }
        }

        if *stop_rx.borrow() {
            let mut s = state.lock().await;
            s.status.status = CampaignState::Stopped;
            update_campaign_status(&db, &campaign_id, "stopped", &s.status);
            return;
        }

        // Compute experiment aggregate (multi-workflow aware)
        let aggregate =
            metrics::compute_aggregate_multi_workflow(&trials, &config.multi_workflow_aggregation);

        // Get control aggregate (with periodic re-evaluation)
        let (control_aggregate, control_trials) = get_control_aggregate(
            &runner,
            &config,
            &current_control,
            experiment_number,
            &state,
            &stop_rx,
        )
        .await;

        if *stop_rx.borrow() {
            let mut s = state.lock().await;
            s.status.status = CampaignState::Stopped;
            update_campaign_status(&db, &campaign_id, "stopped", &s.status);
            return;
        }

        let (accepted, reason, p_value) = metrics::compare_to_control(
            &aggregate,
            &control_aggregate,
            &trials,
            &control_trials,
            &config.acceptance_criteria,
        );

        info!(
            "Experiment #{} {}: {}",
            experiment_number,
            if accepted { "ACCEPTED" } else { "REJECTED" },
            reason
        );

        let ai_recommendation = mutator.last_ai_recommendation().map(|s| s.to_string());

        let mut result = ExperimentResult {
            config: experiment_config.clone(),
            trials: trials.clone(),
            aggregate,
            accepted,
            reason: reason.clone(),
            p_value,
            ai_recommendation,
        };

        // Attach pipeline agent traces from the DB (if multi-agent pipeline was used).
        // The loop_controller persists traces to pipeline_agent_traces during execution;
        // we collect them here so the UI can render per-agent effectiveness views.
        if experiment_config
            .workflow_architecture
            .as_ref()
            .map(|a| {
                matches!(
                    a,
                    super::agentic_verification::WorkflowArchitecture::MultiAgentPipeline
                )
            })
            .unwrap_or(false)
        {
            let mut all_traces = Vec::new();
            for trial in &trials {
                if trial.task_run_id.starts_with("error-") {
                    continue;
                }
                if let Ok(traces) = crate::database::pipeline_traces::get_traces_for_task_run(
                    &db,
                    &trial.task_run_id,
                ) {
                    all_traces.extend(traces);
                }
            }
            if !all_traces.is_empty() {
                result.config.extra.insert(
                    "pipeline_result".to_string(),
                    serde_json::json!({
                        "agent_traces": all_traces,
                        "total_criteria": 0,
                        "passed_criteria": 0,
                        "total_tokens": all_traces.iter().map(|t| (t.tokens_in + t.tokens_out) as u64).sum::<u64>(),
                        "total_cost_usd": all_traces.iter().map(|t| t.cost_usd).sum::<f64>(),
                        "total_iterations": result.aggregate.mean_iterations as u32,
                        "goal_achieved": result.aggregate.pass_rate > 0.5,
                        "was_stopped": false,
                        "max_iterations_reached": false,
                        "subtree_results": [],
                        "dag": { "nodes": {}, "roots": [], "levels": [], "subtrees": [] },
                    }),
                );
            }
        }

        // Save experiment to DB
        if let Err(e) = db.execute_sql(
            "INSERT INTO autoresearch_experiments (id, campaign_id, experiment_number, config_json, trials_json, aggregate_json, accepted, reason, p_value, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'))",
            &[
                &format!("{}-exp-{}", campaign_id, experiment_number) as &dyn rusqlite::types::ToSql,
                &campaign_id,
                &(experiment_number as i64),
                &serde_json::to_string(&experiment_config).unwrap_or_default(),
                &serde_json::to_string(&result.trials).unwrap_or_default(),
                &serde_json::to_string(&result.aggregate).unwrap_or_default(),
                &accepted,
                &reason,
                &p_value,
            ],
        ) {
            warn!("Failed to save experiment #{} to DB: {}", experiment_number, e);
        }

        // Feed results into learning system
        record_learning(&db, &config, &result, experiment_number);

        // Update state
        {
            let mut s = state.lock().await;
            s.status.experiment_count += 1;
            if accepted {
                s.status.accepted_count += 1;
                s.status.experiments_since_last_accept = 0;
                s.status.current_control = experiment_config;
                // Invalidate cached control since control changed
                s.cached_control_aggregate = None;
                // Update campaign control in DB
                if let Err(e) = db.execute_sql(
                    "UPDATE autoresearch_campaigns SET current_control_json = ?1, experiment_count = ?2, accepted_count = ?3 WHERE id = ?4",
                    &[
                        &serde_json::to_string(&s.status.current_control).unwrap_or_default() as &dyn rusqlite::types::ToSql,
                        &(s.status.experiment_count as i64),
                        &(s.status.accepted_count as i64),
                        &campaign_id,
                    ],
                ) {
                    warn!("Failed to update campaign control in DB: {}", e);
                }
            } else {
                s.status.experiments_since_last_accept += 1;
                if let Err(e) = db.execute_sql(
                    "UPDATE autoresearch_campaigns SET experiment_count = ?1 WHERE id = ?2",
                    &[
                        &(s.status.experiment_count as i64) as &dyn rusqlite::types::ToSql,
                        &campaign_id,
                    ],
                ) {
                    warn!("Failed to update campaign count in DB: {}", e);
                }
            }
            s.status.current_experiment = None;
            s.results.push((experiment_number, result));
        }

        // Convergence detection
        if config.convergence.enabled {
            let mut s = state.lock().await;
            if s.status.experiments_since_last_accept >= config.convergence.no_improvement_threshold
            {
                info!(
                    "Convergence detected: {} consecutive experiments without improvement (threshold={})",
                    s.status.experiments_since_last_accept,
                    config.convergence.no_improvement_threshold,
                );
                s.status.status = CampaignState::Converged;
                s.status.error = Some(format!(
                    "Converged: {} experiments without improvement",
                    s.status.experiments_since_last_accept,
                ));
                update_campaign_status(&db, &campaign_id, "converged", &s.status);
                return;
            }
        }
    }
}

/// Run N trials across multiple workflows for a given config.
/// For single-workflow campaigns, this is equivalent to run_trials.
async fn run_trials_multi_workflow(
    runner: &RunnerClient,
    workflow_ids: &[String],
    config: &ExperimentConfig,
    trials_per_experiment: u32,
    experiment_number: u32,
    stop_rx: &watch::Receiver<bool>,
) -> Vec<TrialResult> {
    let mut all_trials = Vec::new();
    for wf_id in workflow_ids {
        let mut trials = run_trials(
            runner,
            wf_id,
            config,
            trials_per_experiment,
            experiment_number,
            stop_rx,
        )
        .await;
        // Tag each trial with the workflow ID
        for trial in &mut trials {
            trial.workflow_id = Some(wf_id.clone());
        }
        all_trials.extend(trials);
        if *stop_rx.borrow() {
            break;
        }
    }
    all_trials
}

/// Run N trials for a given config against a single workflow.
async fn run_trials(
    runner: &RunnerClient,
    workflow_id: &str,
    config: &ExperimentConfig,
    trials_per_experiment: u32,
    experiment_number: u32,
    stop_rx: &watch::Receiver<bool>,
) -> Vec<TrialResult> {
    let mut trials = Vec::new();
    for trial_idx in 0..trials_per_experiment {
        if *stop_rx.borrow() {
            break;
        }

        info!(
            "  Trial {}/{} for experiment #{}",
            trial_idx + 1,
            trials_per_experiment,
            experiment_number
        );

        match run_single_trial(runner, workflow_id, config, stop_rx).await {
            Ok(trial) => {
                info!(
                    "  Trial result: passed={}, iterations={}, duration={}ms",
                    trial.passed, trial.iterations_used, trial.duration_ms
                );
                trials.push(trial);
            }
            Err(e) => {
                if e == "Loop stopped" {
                    break;
                }
                warn!("  Trial failed: {}", e);
                trials.push(TrialResult {
                    task_run_id: format!("error-{}-{}", experiment_number, trial_idx),
                    passed: false,
                    iterations_used: 0,
                    duration_ms: 0,
                    workflow_id: None,
                    spec_compliance_score: None,
                    spec_assertions_passed: None,
                    spec_assertions_total: None,
                    composite_agentic_score: None,
                    agentic_scores: None,
                });
            }
        }
    }
    trials
}

/// Get the control aggregate, with periodic re-evaluation caching.
/// When control_reeval_interval > 0, reuses cached results between refreshes.
async fn get_control_aggregate(
    runner: &RunnerClient,
    config: &ResearchConfig,
    current_control: &ExperimentConfig,
    experiment_number: u32,
    state: &Arc<Mutex<ResearchState>>,
    stop_rx: &watch::Receiver<bool>,
) -> (AggregateMetrics, Vec<TrialResult>) {
    let interval = config.control_reeval_interval;

    // Check if we can reuse the cached control aggregate
    if interval > 0 && experiment_number > 1 {
        let s = state.lock().await;
        if let Some(ref cached) = s.cached_control_aggregate {
            // Only re-evaluate every `interval` experiments
            #[allow(clippy::manual_is_multiple_of)]
            if experiment_number % interval != 0 {
                debug!(
                    "Reusing cached control aggregate (next refresh at experiment #{})",
                    ((experiment_number / interval) + 1) * interval,
                );
                return cached.clone();
            }
        }
    }

    // Run fresh control trials
    info!("  Running control baseline trials...");
    let mut control_with_worktree = current_control.clone();
    if config.use_worktree {
        control_with_worktree
            .extra
            .insert("use_worktree".to_string(), serde_json::json!(true));
    }
    let mut control_trials = Vec::new();
    for trial_idx in 0..config.trials_per_experiment {
        if *stop_rx.borrow() {
            break;
        }
        let wf_id = config
            .effective_workflow_ids()
            .into_iter()
            .next()
            .unwrap_or_default();
        match run_single_trial(runner, &wf_id, &control_with_worktree, stop_rx).await {
            Ok(t) => control_trials.push(t),
            Err(e) => {
                if e == "Loop stopped" {
                    break;
                }
                warn!("  Control trial failed: {}", e);
                control_trials.push(TrialResult {
                    task_run_id: format!("control-error-{}", trial_idx),
                    passed: false,
                    iterations_used: 0,
                    duration_ms: 0,
                    workflow_id: None,
                    spec_compliance_score: None,
                    spec_assertions_passed: None,
                    spec_assertions_total: None,
                    composite_agentic_score: None,
                    agentic_scores: None,
                });
            }
        }
    }

    let aggregate = metrics::compute_aggregate(&control_trials);

    // Cache the result
    {
        let mut s = state.lock().await;
        s.cached_control_aggregate = Some((aggregate.clone(), control_trials.clone()));
    }

    (aggregate, control_trials)
}

/// Run a single trial: start workflow with overrides, poll until done, extract metrics.
async fn run_single_trial(
    runner: &RunnerClient,
    workflow_id: &str,
    config: &ExperimentConfig,
    stop_rx: &watch::Receiver<bool>,
) -> Result<TrialResult, String> {
    let start = std::time::Instant::now();

    // Start workflow with overrides
    let overrides = config.to_overrides();
    let task_run_id = runner
        .start_workflow_with_overrides(workflow_id, &overrides)
        .await?;

    // Poll until complete
    let result = runner.poll_until_complete(&task_run_id, stop_rx).await?;

    let duration_ms = start.elapsed().as_millis() as u64;

    // Extract metrics from the workflow state
    let passed = result
        .get("goal_achieved")
        .and_then(|v| v.as_bool())
        .or_else(|| {
            // Fallback: check if status is "complete" (not "failed")
            result
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s == "complete" || s == "completed")
        })
        .unwrap_or(false);

    let iterations_used = result
        .get("iterations_used")
        .and_then(|v| v.as_u64())
        .or_else(|| result.get("sessions_count").and_then(|v| v.as_u64()))
        .unwrap_or(0) as u32;

    Ok(TrialResult {
        task_run_id,
        passed,
        iterations_used,
        duration_ms,
        workflow_id: None, // Set by caller for multi-workflow campaigns
        spec_compliance_score: None,
        spec_assertions_passed: None,
        spec_assertions_total: None,
        composite_agentic_score: None,
        agentic_scores: None,
    })
}

// =============================================================================
// Learning system integration
// =============================================================================

/// Record experiment results into the learning_outcomes and learning_patterns tables.
fn record_learning(
    db: &crate::database::CheckpointDb,
    config: &ResearchConfig,
    result: &ExperimentResult,
    experiment_number: u32,
) {
    let conn = match db.get_conn_string() {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to get DB connection for learning: {}", e);
            return;
        }
    };

    let avg_duration_secs = result.aggregate.mean_duration_ms / 1000.0;
    let avg_iterations = result.aggregate.mean_iterations as u32;

    let outcome = crate::orchestrator::learning_recorder::WorkflowOutcome {
        task_run_id: format!("autoresearch-exp-{}", experiment_number),
        workflow_name: config.effective_workflow_ids().join(","),
        category: "autoresearch".to_string(),
        status: if result.accepted {
            "success".to_string()
        } else {
            "failure".to_string()
        },
        duration_secs: avg_duration_secs,
        iterations: avg_iterations,
        verification_passed: result.aggregate.pass_rate >= 0.5,
        max_iterations_reached: false,
        was_stopped: false,
        tools_used: vec![],
        files_modified: vec![],
        error_type: None,
        error_message: if !result.accepted {
            Some(result.reason.clone())
        } else {
            None
        },
        workflow_architecture: result
            .config
            .workflow_architecture
            .as_ref()
            .map(|a| {
                serde_json::to_value(a)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| format!("{:?}", a).to_lowercase())
            })
            .or_else(|| Some("traditional".to_string())),
        step_count: None,
        verification_step_count: None,
        agentic_step_count: None,
        has_ui_bridge: false,
        total_tokens: None,
        total_cost_usd: None,
    };

    if let Err(e) =
        crate::orchestrator::learning_recorder::record_workflow_learning(&conn, &outcome)
    {
        warn!(
            "Failed to record learning for experiment #{}: {}",
            experiment_number, e
        );
    } else {
        debug!(
            "Recorded learning outcome for experiment #{}",
            experiment_number
        );
    }

    // Record autoresearch-specific pattern: config → outcome mapping
    let config_pattern = crate::orchestrator::learning_recorder::PatternInput {
        pattern_type: "autoresearch_config".to_string(),
        description: format!(
            "Config [{}] → pass_rate={:.2}, accepted={}",
            result.config.summary(),
            result.aggregate.pass_rate,
            result.accepted,
        ),
        confidence: if result.aggregate.trial_count >= 3 {
            0.7
        } else {
            0.4
        },
        context: Some(serde_json::json!({
            "campaign": config.name,
            "experiment_number": experiment_number,
            "config": result.config,
            "aggregate": result.aggregate,
            "accepted": result.accepted,
            "p_value": result.p_value,
        })),
    };

    if let Err(e) =
        crate::orchestrator::learning_recorder::record_learning_pattern(&conn, &config_pattern)
    {
        warn!("Failed to record autoresearch pattern: {}", e);
    }
}

// =============================================================================
// Config file hot-reload watcher
// =============================================================================

/// Start a file watcher on the research config file.
/// Returns a channel receiver that emits new ResearchConfig on file changes.
fn start_config_watcher(config_path: &str) -> Option<std::sync::mpsc::Receiver<ResearchConfig>> {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    let path = PathBuf::from(config_path);
    if !path.exists() {
        warn!(
            "Autoresearch config file not found for hot-reload: {}",
            config_path
        );
        return None;
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let config_path_owned = path.clone();

    // Debounce: only reload after 500ms of no changes
    let tx_clone = tx.clone();
    let mut watcher =
        match notify::recommended_watcher(move |result: Result<Event, notify::Error>| {
            if let Ok(event) = result {
                match event.kind {
                    EventKind::Modify(_) | EventKind::Create(_) => {
                        // Read and parse the config file
                        match std::fs::read_to_string(&config_path_owned) {
                            Ok(content) => {
                                // Try YAML first, then JSON
                                let parsed: Result<ResearchConfig, String> =
                                    serde_yaml::from_str(&content)
                                        .map_err(|e| format!("YAML parse error: {}", e))
                                        .or_else(|_| {
                                            serde_json::from_str(&content)
                                                .map_err(|e| format!("JSON parse error: {}", e))
                                        });

                                match parsed {
                                    Ok(new_config) => {
                                        info!("Config file changed — sending reload signal");
                                        let _ = tx_clone.send(new_config);
                                    }
                                    Err(e) => {
                                        warn!("Failed to parse reloaded config file: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Failed to read config file for reload: {}", e);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                warn!("Failed to create config file watcher: {}", e);
                return None;
            }
        };

    // Watch the parent directory (some editors write to temp then rename)
    let watch_path = path.parent().unwrap_or(&path);
    if let Err(e) = watcher.watch(watch_path, RecursiveMode::NonRecursive) {
        warn!("Failed to watch config file directory: {}", e);
        return None;
    }

    // Keep the watcher alive by leaking it (it runs until the process exits)
    // This is intentional — the watcher thread must outlive the function call.
    std::mem::forget(watcher);

    info!("Config file watcher started for: {}", config_path);
    Some(rx)
}

fn update_campaign_status(
    db: &crate::database::CheckpointDb,
    campaign_id: &str,
    status: &str,
    campaign_status: &CampaignStatus,
) {
    if let Err(e) = db.execute_sql(
        "UPDATE autoresearch_campaigns SET status = ?1, experiment_count = ?2, accepted_count = ?3 WHERE id = ?4",
        &[
            &status as &dyn rusqlite::types::ToSql,
            &(campaign_status.experiment_count as i64),
            &(campaign_status.accepted_count as i64),
            &campaign_id,
        ],
    ) {
        warn!("Failed to update campaign status to '{}': {}", status, e);
    }
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}
