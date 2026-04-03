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
        pg_db: std::sync::Arc<crate::database::pg::PgDb>,
    ) -> Result<String, String> {
        Err("SQLite removed".to_string())
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
                let rows = pg_q_rows
                    .or({
                        // Fallback: try to load synchronously via block_on (best-effort)
                        None
                    })
                    .or(load_q_table_from_pg(pg).await.ok());
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
    pg_db: std::sync::Arc<crate::database::pg::PgDb>,
) {
    // SQLite removed - no-op
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
fn record_learning(config: &ResearchConfig, result: &ExperimentResult, experiment_number: u32) {
    // SQLite removed - no-op
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

fn update_campaign_status(campaign_id: &str, status: &str, campaign_status: &CampaignStatus) {
    // SQLite removed - no-op
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
