//! Learning outcome types, tag/complexity helpers, and LLM/RAG judge spawners.
//!
//! The persistence paths now live on `PgDb` (`record_learning_outcome`,
//! `record_workflow_learning_from_outcome`, `upsert_q_entry`, etc.). This
//! module keeps the shared `WorkflowOutcome` type, the pure tag/complexity
//! inference helpers used by the online learning coordinator, and the
//! async judge spawners that run fire-and-forget after a workflow completes.

use chrono::Utc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Outcome of a workflow execution for learning purposes.
#[derive(Clone)]
pub struct WorkflowOutcome {
    pub task_run_id: String,
    pub workflow_name: String,
    pub category: String,
    pub status: String,
    pub duration_secs: f64,
    pub iterations: u32,
    pub verification_passed: bool,
    pub max_iterations_reached: bool,
    pub was_stopped: bool,
    pub tools_used: Vec<String>,
    pub files_modified: Vec<String>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub workflow_architecture: Option<String>,
    pub step_count: Option<i64>,
    pub verification_step_count: Option<i64>,
    pub agentic_step_count: Option<i64>,
    pub has_ui_bridge: bool,
    pub total_tokens: Option<u64>,
    pub total_cost_usd: Option<f64>,
    /// Primary model used for this workflow execution (for online learning bandit).
    pub model_used: Option<String>,
}

/// Infer technology tags from file paths based on file extensions.
///
/// Returns a deduplicated sorted list of technology identifiers.
pub fn infer_technology_tags(files: &[String]) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut tags = BTreeSet::new();

    for file in files {
        let lower = file.to_lowercase();
        // Match by extension
        if lower.ends_with(".rs") {
            tags.insert("rust".to_string());
        }
        if lower.ends_with(".ts") || lower.ends_with(".tsx") {
            tags.insert("typescript".to_string());
        }
        if lower.ends_with(".js") || lower.ends_with(".jsx") {
            tags.insert("javascript".to_string());
        }
        if lower.ends_with(".py") {
            tags.insert("python".to_string());
        }
        if lower.ends_with(".css") || lower.ends_with(".scss") {
            tags.insert("css".to_string());
        }
        if lower.ends_with(".html") {
            tags.insert("html".to_string());
        }
        if lower.ends_with(".sql") {
            tags.insert("sql".to_string());
        }
        if lower.ends_with(".json") {
            tags.insert("json".to_string());
        }
        if lower.ends_with(".toml") {
            tags.insert("toml".to_string());
        }
        if lower.ends_with(".yaml") || lower.ends_with(".yml") {
            tags.insert("yaml".to_string());
        }
        // Framework/tool detection from path segments
        if lower.contains("tauri") {
            tags.insert("tauri".to_string());
        }
        if lower.contains("next") || lower.contains("nextjs") {
            tags.insert("nextjs".to_string());
        }
        if lower.contains("react") {
            tags.insert("react".to_string());
        }
    }

    tags.into_iter().collect()
}

/// Infer domain/area tags from file paths and workflow metadata.
///
/// Returns a deduplicated sorted list of domain identifiers.
pub fn infer_domain_tags(
    files: &[String],
    workflow_name: &str,
    category: &str,
    has_ui_bridge: bool,
) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut tags = BTreeSet::new();

    // Infer from file paths
    for file in files {
        let lower = file.to_lowercase().replace('\\', "/");
        if lower.contains("/database/") || lower.contains("/migrations") || lower.contains(".sql") {
            tags.insert("database".to_string());
        }
        if lower.contains("/ui-bridge/") || lower.contains("/ui_bridge/") {
            tags.insert("ui-bridge".to_string());
        }
        if lower.contains("/test") || lower.contains("_test.") || lower.contains(".test.") {
            tags.insert("testing".to_string());
        }
        if lower.contains("/api/") || lower.contains("/mcp/") || lower.contains("/http/") {
            tags.insert("api".to_string());
        }
        if lower.contains("/frontend/") || lower.contains(".tsx") || lower.contains(".jsx") {
            tags.insert("frontend".to_string());
        }
        if lower.contains("/backend/") {
            tags.insert("backend".to_string());
        }
        if lower.contains("/orchestrator/") || lower.contains("/workflow") {
            tags.insert("orchestration".to_string());
        }
        if lower.contains("/meta_optimizer/") || lower.contains("/autoresearch/") {
            tags.insert("meta-optimizer".to_string());
        }
        if lower.contains("/generation/") || lower.contains("generator") {
            tags.insert("generation".to_string());
        }
    }

    // Infer from workflow name
    let name_lower = workflow_name.to_lowercase();
    if name_lower.contains("generate") || name_lower.starts_with("ai generate") {
        tags.insert("generation".to_string());
    }
    if name_lower.contains("fix") || name_lower.contains("error") {
        tags.insert("error-fix".to_string());
    }
    if name_lower.contains("test") {
        tags.insert("testing".to_string());
    }
    if name_lower.contains("refactor") {
        tags.insert("refactoring".to_string());
    }

    // Infer from category
    let cat_lower = category.to_lowercase();
    if cat_lower.contains("autoresearch") {
        tags.insert("autoresearch".to_string());
    }

    // From UI Bridge flag
    if has_ui_bridge {
        tags.insert("ui-bridge".to_string());
    }

    tags.into_iter().collect()
}

/// Compute a complexity tier from step counts, iteration count, and duration.
///
/// 5 tiers matching the Q-learning state space:
/// - "trivial": single-step, no iterations, very fast
/// - "simple": few steps, low iterations, fast
/// - "moderate": medium complexity
/// - "complex": many steps, high iterations, or long duration
/// - "highly_complex": extreme step counts, many iterations, very long duration
pub fn compute_complexity_tier(
    step_count: Option<i64>,
    iterations: u32,
    duration_secs: f64,
    agentic_step_count: Option<i64>,
) -> String {
    let steps = step_count.unwrap_or(0);
    let agentic = agentic_step_count.unwrap_or(0);

    // Heuristic: score based on multiple factors (0-11 range)
    let mut score: u32 = 0;

    // Step count contribution (0-4)
    if steps > 30 {
        score += 4;
    } else if steps > 15 {
        score += 3;
    } else if steps > 8 {
        score += 2;
    } else if steps > 3 {
        score += 1;
    }

    // Agentic step count contribution (0-3)
    if agentic > 10 {
        score += 3;
    } else if agentic > 5 {
        score += 2;
    } else if agentic > 2 {
        score += 1;
    }

    // Iteration contribution (0-2)
    if iterations > 5 {
        score += 2;
    } else if iterations > 2 {
        score += 1;
    }

    // Duration contribution (0-2)
    if duration_secs > 600.0 {
        score += 2;
    } else if duration_secs > 120.0 {
        score += 1;
    }

    if score >= 8 {
        "highly_complex".to_string()
    } else if score >= 5 {
        "complex".to_string()
    } else if score >= 2 {
        "moderate".to_string()
    } else if score >= 1 {
        "simple".to_string()
    } else {
        "trivial".to_string()
    }
}

/// Categorize an error message into a standard error type.
///
/// Matches patterns from meta_optimizer_api.rs top failure patterns query.
pub fn categorize_error(msg: &str) -> String {
    let lower = msg.to_lowercase();
    if lower.contains("max iterations") {
        "max_iterations_reached"
    } else if lower.contains("max sessions") {
        "max_sessions_reached"
    } else if lower.contains("generation") || lower.contains("schema") {
        "generation_error"
    } else if lower.contains("timeout") {
        "timeout"
    } else {
        "runtime_error"
    }
    .to_string()
}

/// Update the Q-routing table in PostgreSQL.
///
/// Computes the Q-value update against PG's current state using the composite
/// agentic score as the reward signal.
pub async fn update_q_routing_table_pg(
    pg: &crate::database::pg::PgDb,
    outcome: &WorkflowOutcome,
    composite_score: f64,
) -> Result<(), String> {
    use crate::autoresearch::q_router::{QRouter, TaskState};

    let architecture = match outcome.workflow_architecture.as_deref() {
        Some(arch) if !arch.is_empty() => arch,
        _ => return Ok(()),
    };

    if composite_score < 0.0 {
        return Ok(());
    }

    let technology_tags = infer_technology_tags(&outcome.files_modified);
    let technology_tags_json =
        serde_json::to_string(&technology_tags).unwrap_or_else(|_| "[]".into());
    let domain_tags = infer_domain_tags(
        &outcome.files_modified,
        &outcome.workflow_name,
        &outcome.category,
        outcome.has_ui_bridge,
    );
    let domain_tags_json = serde_json::to_string(&domain_tags).unwrap_or_else(|_| "[]".into());
    let complexity_tier = compute_complexity_tier(
        outcome.step_count,
        outcome.iterations,
        outcome.duration_secs,
        outcome.agentic_step_count,
    );

    let task_state =
        TaskState::from_outcome_tags(&technology_tags_json, &domain_tags_json, &complexity_tier);
    let state_key = task_state.to_key();

    // Load current Q-entry from PG
    let entries = pg
        .get_q_entries_for_state(&state_key)
        .await
        .unwrap_or_default();
    let (current_q, current_visits) = entries
        .iter()
        .find(|(a, _, _)| a == architecture)
        .map(|(_, q, v)| (*q, *v))
        .unwrap_or((0.0, 0));

    let reward = QRouter::compute_reward(composite_score, outcome.total_cost_usd);
    let alpha = 0.1;
    let new_q = current_q + alpha * (reward - current_q);
    let new_visits = current_visits + 1;

    pg.upsert_q_entry(&state_key, architecture, new_q, new_visits)
        .await?;

    info!(
        "PG Q-routing update: state={}, arch={}, reward={:.3}, Q={:.3}→{:.3}, visits={}",
        task_state, architecture, reward, current_q, new_q, new_visits
    );

    Ok(())
}

/// Spawn async LLM-as-judge evaluation for a completed workflow run.
///
/// This is fire-and-forget: it gathers data from the DB, calls the LLM judge
/// in a blocking task, and persists the results. Errors are logged but do not
/// propagate.
///
/// Call this after `record_workflow_learning()` completes. It checks eligibility
/// internally via `should_llm_judge()`.
pub fn spawn_llm_judge_if_eligible(
    pg_db: std::sync::Arc<crate::database::pg::PgDb>,
    task_run_id: String,
    iterations: u32,
    verification_passed: bool,
    was_stopped: bool,
) {
    use crate::meta_optimizer::agentic_metrics::llm_judge;

    if !llm_judge::should_llm_judge(iterations, verification_passed, was_stopped) {
        debug!("LLM judge: skipping task {} (not eligible)", task_run_id);
        return;
    }

    info!("LLM judge: spawning evaluation for task {}", task_run_id);

    tokio::spawn(async move {
        // 1. Gather input from PG
        let input = match gather_llm_judge_input_pg(&pg_db, &task_run_id).await {
            Ok(input) => input,
            Err(e) => {
                warn!(
                    "LLM judge: failed to gather input for {}: {}",
                    task_run_id, e
                );
                return;
            }
        };

        // 2. Call the LLM judge (blocking AI call)
        let results =
            match tokio::task::spawn_blocking(move || llm_judge::evaluate_sync(&input)).await {
                Ok(results) => results,
                Err(e) => {
                    warn!(
                        "LLM judge: spawn_blocking panicked for {}: {}",
                        task_run_id, e
                    );
                    return;
                }
            };

        if results.is_empty() {
            debug!("LLM judge: no results for task {}", task_run_id);
            return;
        }

        // 3. Persist scores to PG
        if let Err(e) = persist_judge_scores_pg(&pg_db, &task_run_id, &results).await {
            warn!(
                "LLM judge: failed to persist scores for {}: {}",
                task_run_id, e
            );
        }
    });
}

/// Spawn async RAG judge evaluation for runs that captured retrieval events.
///
/// Fire-and-forget: loads retrieval events from task_run_events, calls the RAG
/// LLM judge, persists scores. Only runs if the task has retrieval events.
pub fn spawn_rag_judge_if_eligible(
    pg_db: std::sync::Arc<crate::database::pg::PgDb>,
    task_run_id: String,
) {
    info!("RAG judge: spawning evaluation for task {}", task_run_id);

    tokio::spawn(async move {
        // 1. Load retrieval events from PG
        let retrieval_events = match pg_db.load_retrieval_events(&task_run_id).await {
            Ok(events) => events,
            Err(e) => {
                warn!(
                    "RAG judge: failed to load retrieval events for {}: {}",
                    task_run_id, e
                );
                return;
            }
        };

        if retrieval_events.is_empty() {
            debug!(
                "RAG judge: no retrieval events for task {}, skipping",
                task_run_id
            );
            return;
        }

        // 2. Get task prompt for context
        let task_prompt = match pg_db.get_task_prompt(&task_run_id).await {
            Ok(prompt) => prompt,
            Err(e) => {
                warn!(
                    "RAG judge: failed to get task prompt for {}: {}",
                    task_run_id, e
                );
                return;
            }
        };

        // 3. Get outcome summary as the generated output
        let generated_output = pg_db
            .get_execution_trace_summary(&task_run_id)
            .await
            .ok()
            .flatten();

        let input = crate::meta_optimizer::agentic_metrics::rag_judge::RagJudgeInput {
            task_run_id: task_run_id.clone(),
            task_prompt,
            retrieval_events,
            generated_output,
        };

        // 4. Call the RAG judge (blocking AI call)
        let results = match tokio::task::spawn_blocking(move || {
            crate::meta_optimizer::agentic_metrics::rag_judge::evaluate_rag_sync(&input)
        })
        .await
        {
            Ok(results) => results,
            Err(e) => {
                warn!(
                    "RAG judge: spawn_blocking panicked for {}: {}",
                    task_run_id, e
                );
                return;
            }
        };

        if results.is_empty() {
            debug!("RAG judge: no results for task {}", task_run_id);
            return;
        }

        // 5. Persist scores to PG
        if let Err(e) = persist_judge_scores_pg(&pg_db, &task_run_id, &results).await {
            warn!(
                "RAG judge: failed to persist scores for {}: {}",
                task_run_id, e
            );
        }
    });
}

/// Persist judge scores to PG.
async fn persist_judge_scores_pg(
    pg: &crate::database::pg::PgDb,
    task_run_id: &str,
    results: &[crate::meta_optimizer::agentic_metrics::MetricResult],
) -> Result<(), String> {
    for score in results {
        let id = format!("ams-{}", Uuid::new_v4());
        let now = chrono::Utc::now().to_rfc3339();
        pg.upsert_agentic_metric_score(
            &id,
            task_run_id,
            score.metric.as_str(),
            score.score,
            score.confidence,
            &score.rationale,
            score.is_llm_judged,
            score.model_used.as_deref(),
            &now,
        )
        .await?;
    }

    // Recompute composite
    let all_scores_raw = pg.get_all_metric_scores(task_run_id).await?;
    let all_scores: Vec<crate::meta_optimizer::agentic_metrics::MetricResult> = all_scores_raw
        .into_iter()
        .filter_map(|(mt, s)| {
            metric_from_str(&mt).map(|m| crate::meta_optimizer::agentic_metrics::MetricResult {
                metric: m,
                score: s,
                confidence: 0.7,
                rationale: String::new(),
                is_llm_judged: false,
                model_used: None,
            })
        })
        .collect();
    let composite = crate::meta_optimizer::agentic_metrics::composite_score(&all_scores);
    pg.update_composite_agentic_score(task_run_id, composite)
        .await?;

    info!(
        "Judge scores persisted (PG) for {}: composite={:.3}",
        task_run_id, composite
    );
    Ok(())
}

/// Gather LLM judge input from PG.
async fn gather_llm_judge_input_pg(
    pg: &crate::database::pg::PgDb,
    task_run_id: &str,
) -> Result<crate::meta_optimizer::agentic_metrics::llm_judge::LlmJudgeInput, String> {
    let (prompt, execution_steps, summary, goal_achieved, status) =
        pg.get_task_run_for_judge(task_run_id).await?;
    let execution_trace_summary = pg
        .get_execution_trace_summary(task_run_id)
        .await
        .ok()
        .flatten();

    Ok(
        crate::meta_optimizer::agentic_metrics::llm_judge::LlmJudgeInput {
            task_run_id: task_run_id.to_string(),
            task_prompt: prompt.unwrap_or_default(),
            execution_plan: execution_steps,
            outcome_summary: summary,
            goal_achieved,
            status,
            execution_trace_summary,
        },
    )
}

/// Parse a metric type string back to an AgenticMetric enum.
fn metric_from_str(s: &str) -> Option<crate::meta_optimizer::agentic_metrics::AgenticMetric> {
    use crate::meta_optimizer::agentic_metrics::AgenticMetric;
    match s {
        "task_completion" => Some(AgenticMetric::TaskCompletion),
        "step_efficiency" => Some(AgenticMetric::StepEfficiency),
        "tool_correctness" => Some(AgenticMetric::ToolCorrectness),
        "argument_correctness" => Some(AgenticMetric::ArgumentCorrectness),
        "plan_adherence" => Some(AgenticMetric::PlanAdherence),
        "goal_accuracy" => Some(AgenticMetric::GoalAccuracy),
        "plan_quality" => Some(AgenticMetric::PlanQuality),
        "rag_contextual_precision" => Some(AgenticMetric::RagContextualPrecision),
        "rag_contextual_recall" => Some(AgenticMetric::RagContextualRecall),
        "rag_faithfulness" => Some(AgenticMetric::RagFaithfulness),
        "rag_answer_relevancy" => Some(AgenticMetric::RagAnswerRelevancy),
        _ => None,
    }
}
