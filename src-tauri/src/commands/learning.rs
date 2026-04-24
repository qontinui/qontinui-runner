//! Tauri commands for the Learning Insights Dashboard.
//!
//! Provides access to learning data persisted in SQLite for displaying
//! AI learning patterns, insights, and task outcome history.

use crate::commands::compartments::StorageCompartment;
use crate::orchestrator::learning::{
    AnalysisResult, Feedback, Insight, LearningSummary, LearningSystem, Pattern, TaskOutcome,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;

/// Global learning system instance for in-memory analysis.
/// The database stores outcomes; this provides real-time analysis.
static LEARNING_SYSTEM: Lazy<Mutex<LearningSystem>> =
    Lazy::new(|| Mutex::new(LearningSystem::new()));

/// Summary data for the learning dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningDashboardData {
    pub summary: LearningSummary,
    pub analysis: AnalysisResult,
    pub patterns: Vec<Pattern>,
    pub insights: Vec<Insight>,
}

/// Get the learning summary statistics.
#[tauri::command]
pub async fn get_learning_summary(
    state: State<'_, StorageCompartment>,
) -> Result<LearningSummary, String> {
    // Get outcomes from database
    let outcomes = state.pg_db().get_learning_outcomes(Some(500)).await?;
    let patterns = state.pg_db().get_learning_patterns().await?;

    // Calculate summary from outcomes
    let total = outcomes.len();
    let successes = outcomes.iter().filter(|o| o["status"] == "success").count();
    let failures = outcomes.iter().filter(|o| o["status"] == "failure").count();

    // Count unique strategies
    let mut strategies: std::collections::HashSet<String> = std::collections::HashSet::new();
    for outcome in &outcomes {
        if let Some(strategy) = outcome["strategy"].as_str() {
            strategies.insert(strategy.to_string());
        }
    }

    // Count unique tools
    let mut tools: std::collections::HashSet<String> = std::collections::HashSet::new();
    for outcome in &outcomes {
        if let Some(tools_arr) = outcome["tools_used"].as_array() {
            for tool in tools_arr {
                if let Some(t) = tool.as_str() {
                    tools.insert(t.to_string());
                }
            }
        }
    }

    Ok(LearningSummary {
        total_tasks: total,
        successful_tasks: successes,
        failed_tasks: failures,
        patterns_identified: patterns.len(),
        insights_generated: 0, // Would need to query insights
        feedback_items: 0,
        strategies_tracked: strategies.len(),
        tools_tracked: tools.len(),
    })
}

/// Get all identified patterns from the database.
#[tauri::command]
pub async fn get_learning_patterns(
    state: State<'_, StorageCompartment>,
) -> Result<Vec<Pattern>, String> {
    let patterns_json = state.pg_db().get_learning_patterns().await?;

    let patterns: Vec<Pattern> = patterns_json
        .into_iter()
        .map(|p| {
            let pattern_type = match p["pattern_type"].as_str().unwrap_or("unknown") {
                "SuccessStrategy" => crate::orchestrator::learning::PatternType::SuccessStrategy,
                "FailureMode" => crate::orchestrator::learning::PatternType::FailureMode,
                "ToolUsage" => crate::orchestrator::learning::PatternType::ToolUsage,
                "Collaboration" => crate::orchestrator::learning::PatternType::Collaboration,
                "IterationBehavior" => {
                    crate::orchestrator::learning::PatternType::IterationBehavior
                }
                "ErrorRecovery" => crate::orchestrator::learning::PatternType::ErrorRecovery,
                "DecisionMaking" => crate::orchestrator::learning::PatternType::DecisionMaking,
                _ => crate::orchestrator::learning::PatternType::SuccessStrategy,
            };
            let id = p["id"].as_str().unwrap_or("").to_string();
            let description = p["description"].as_str().unwrap_or("").to_string();

            // Extract context data
            let context = &p["context"];
            let name = context["name"].as_str().unwrap_or(&description).to_string();
            let success_rate = context["success_rate"].as_f64().unwrap_or(0.0);

            let mut pattern = Pattern::new(id, name, pattern_type)
                .with_description(description)
                .with_confidence(p["confidence"].as_f64().unwrap_or(0.0))
                .with_occurrences(p["occurrences"].as_i64().unwrap_or(0) as u32)
                .with_success_rate(success_rate);

            // Add triggers from context
            if let Some(triggers) = context["triggers"].as_array() {
                for trigger in triggers {
                    if let Some(t) = trigger.as_str() {
                        pattern = pattern.with_trigger(t);
                    }
                }
            }

            // Add recommendations from context
            if let Some(recs) = context["recommendations"].as_array() {
                for rec in recs {
                    if let Some(r) = rec.as_str() {
                        pattern = pattern.with_recommendation(r);
                    }
                }
            }

            pattern
        })
        .collect();

    Ok(patterns)
}

/// Get all generated insights (uses in-memory analysis).
#[tauri::command]
pub async fn get_learning_insights(
    state: State<'_, StorageCompartment>,
) -> Result<Vec<Insight>, String> {
    // Load outcomes from database into the learning system
    let outcomes = state.pg_db().get_learning_outcomes(Some(500)).await?;
    let mut system = LEARNING_SYSTEM.lock().map_err(|e| e.to_string())?;

    // Rebuild system from database outcomes
    *system = LearningSystem::new();
    for outcome_json in outcomes {
        if let Ok(outcome) = serde_json::from_value::<TaskOutcome>(outcome_json.clone()) {
            system.record_outcome(outcome);
        }
    }

    let result = system.analyze_patterns();
    Ok(result.insights)
}

/// Run full pattern analysis and return results.
#[tauri::command]
pub async fn analyze_learning_data(
    state: State<'_, StorageCompartment>,
) -> Result<AnalysisResult, String> {
    // Load outcomes from database
    let outcomes = state.pg_db().get_learning_outcomes(Some(500)).await?;
    let result = {
        let mut system = LEARNING_SYSTEM.lock().map_err(|e| e.to_string())?;

        // Rebuild system from database outcomes
        *system = LearningSystem::new();
        for outcome_json in outcomes {
            if let Ok(outcome) = serde_json::from_value::<TaskOutcome>(outcome_json.clone()) {
                system.record_outcome(outcome);
            }
        }

        system.analyze_patterns()
    }; // MutexGuard dropped here before any .await

    // Save patterns to database for persistence
    for pattern in &result.patterns {
        let pattern_type = match pattern.pattern_type {
            crate::orchestrator::learning::PatternType::SuccessStrategy => "SuccessStrategy",
            crate::orchestrator::learning::PatternType::FailureMode => "FailureMode",
            crate::orchestrator::learning::PatternType::ToolUsage => "ToolUsage",
            crate::orchestrator::learning::PatternType::Collaboration => "Collaboration",
            crate::orchestrator::learning::PatternType::IterationBehavior => "IterationBehavior",
            crate::orchestrator::learning::PatternType::ErrorRecovery => "ErrorRecovery",
            crate::orchestrator::learning::PatternType::DecisionMaking => "DecisionMaking",
        };
        // Serialize pattern metadata into context JSON
        let context = serde_json::json!({
            "name": pattern.name,
            "triggers": pattern.triggers,
            "recommendations": pattern.recommendations,
            "observed_in": pattern.observed_in,
            "success_rate": pattern.success_rate,
        });
        let context_str = context.to_string();
        let _ = state
            .pg_db()
            .save_learning_pattern(
                &pattern.id,
                pattern_type,
                &pattern.description,
                pattern.confidence,
                pattern.occurrences as i32,
                Some(context_str.as_str()),
            )
            .await;
    }

    Ok(result)
}

/// Get feedback applicable to a specific context.
#[tauri::command]
pub fn get_feedback_for_context(context: String) -> Result<Vec<Feedback>, String> {
    let system = LEARNING_SYSTEM.lock().map_err(|e| e.to_string())?;
    Ok(system
        .get_applicable_feedback(&context)
        .into_iter()
        .cloned()
        .collect())
}

/// Get all learning dashboard data in one call.
#[tauri::command]
pub async fn get_learning_dashboard_data(
    state: State<'_, StorageCompartment>,
) -> Result<LearningDashboardData, String> {
    // Load outcomes from database
    let outcomes = state.pg_db().get_learning_outcomes(Some(500)).await?;
    let mut system = LEARNING_SYSTEM.lock().map_err(|e| e.to_string())?;

    // Rebuild system from database outcomes
    *system = LearningSystem::new();
    for outcome_json in outcomes {
        if let Ok(outcome) = serde_json::from_value::<TaskOutcome>(outcome_json.clone()) {
            system.record_outcome(outcome);
        }
    }

    let analysis = system.analyze_patterns();
    Ok(LearningDashboardData {
        summary: system.get_summary(),
        patterns: analysis.patterns.clone(),
        insights: analysis.insights.clone(),
        analysis,
    })
}

/// Record a task outcome for learning (saves to database).
#[tauri::command]
pub async fn record_task_outcome(
    state: State<'_, StorageCompartment>,
    outcome: TaskOutcome,
) -> Result<(), String> {
    // Record to in-memory system for immediate analysis
    {
        let mut system = LEARNING_SYSTEM.lock().map_err(|e| e.to_string())?;
        system.record_outcome(outcome.clone());
    }

    // Persist to database
    let status = match outcome.status {
        crate::orchestrator::learning::OutcomeStatus::Success => "success",
        crate::orchestrator::learning::OutcomeStatus::PartialSuccess => "partial",
        crate::orchestrator::learning::OutcomeStatus::Failure => "failure",
        crate::orchestrator::learning::OutcomeStatus::Abandoned => "abandoned",
    };

    // Map TaskOutcome fields to database columns
    let tools = outcome.tools_used;
    let agents = outcome.agents_involved;

    // Join errors into a single message if any
    let error_message = if outcome.errors.is_empty() {
        None
    } else {
        Some(outcome.errors.join("; "))
    };

    // Serialize metrics/decisions/tags as feedback JSON
    let feedback_json =
        if outcome.metrics.is_empty() && outcome.decisions.is_empty() && outcome.tags.is_empty() {
            None
        } else {
            Some(
                serde_json::json!({
                    "metrics": outcome.metrics,
                    "decisions": outcome.decisions,
                    "tags": outcome.tags,
                })
                .to_string(),
            )
        };

    let id = uuid::Uuid::new_v4().to_string();
    let tools_json = if tools.is_empty() {
        None
    } else {
        Some(serde_json::json!(tools).to_string())
    };
    let agents_json = if agents.is_empty() {
        None
    } else {
        Some(serde_json::json!(agents).to_string())
    };

    state
        .pg_db()
        .record_learning_outcome(
            &id,
            &outcome.task_id,
            status,
            outcome.duration_secs.map(|d| d as f64),
            outcome.iterations.map(|i| i as i32),
            outcome.strategy.as_deref(),
            tools_json.as_deref(),
            agents_json.as_deref(),
            None, // error_type - not in TaskOutcome
            error_message.as_deref(),
            feedback_json.as_deref(),
            outcome.workflow_architecture.as_deref(),
            None,  // step_count
            None,  // verification_step_count
            None,  // agentic_step_count
            false, // has_ui_bridge
            None,  // total_tokens
            None,  // total_cost_usd
            None,  // technology_tags
            None,  // domain_tags
            None,  // complexity_tier
        )
        .await?;

    Ok(())
}

/// Get the best performing strategy.
#[tauri::command]
pub async fn get_best_strategy(
    state: State<'_, StorageCompartment>,
) -> Result<Option<(String, f64)>, String> {
    let outcomes = state.pg_db().get_learning_outcomes(Some(500)).await?;

    let mut strategy_stats: std::collections::HashMap<String, (u32, u32)> =
        std::collections::HashMap::new();
    for outcome in outcomes {
        if let Some(strategy) = outcome["strategy"].as_str() {
            let entry = strategy_stats.entry(strategy.to_string()).or_insert((0, 0));
            entry.0 += 1;
            if outcome["status"] == "success" {
                entry.1 += 1;
            }
        }
    }

    let best = strategy_stats
        .into_iter()
        .filter(|(_, (total, _))| *total >= 3) // Require at least 3 uses
        .map(|(name, (total, success))| (name, success as f64 / total as f64))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    Ok(best)
}

/// Export learning data as JSON for backup.
#[tauri::command]
pub async fn export_learning_data(state: State<'_, StorageCompartment>) -> Result<String, String> {
    let outcomes = state.pg_db().get_learning_outcomes(None).await?;
    let patterns = state.pg_db().get_learning_patterns().await?;

    let export = serde_json::json!({
        "outcomes": outcomes,
        "patterns": patterns,
        "exported_at": chrono::Utc::now().to_rfc3339(),
    });

    serde_json::to_string_pretty(&export).map_err(|e| e.to_string())
}

/// Import learning data from JSON (for restore/migration).
#[tauri::command]
pub fn import_learning_data(json: String) -> Result<(), String> {
    let imported = LearningSystem::import(&json).map_err(|e| e.to_string())?;
    let mut system = LEARNING_SYSTEM.lock().map_err(|e| e.to_string())?;
    *system = imported;
    Ok(())
}

/// Clear all learning data (resets in-memory system, database data persists).
#[tauri::command]
pub fn clear_learning_data() -> Result<(), String> {
    let mut system = LEARNING_SYSTEM.lock().map_err(|e| e.to_string())?;
    *system = LearningSystem::new();
    Ok(())
}

/// Add sample learning data for demonstration/testing.
#[tauri::command]
pub async fn add_sample_learning_data(state: State<'_, StorageCompartment>) -> Result<(), String> {
    // Add successful outcomes
    for i in 0..10 {
        let tools_json = serde_json::json!(["grep", "edit"]).to_string();
        let strategy = if i % 2 == 0 {
            "incremental"
        } else {
            "parallel"
        };

        let id = uuid::Uuid::new_v4().to_string();
        state
            .pg_db()
            .record_learning_outcome(
                &id,
                &format!("sample-task-{}", i),
                "success",
                Some(60.0 + (i * 10) as f64),
                Some(3 + (i % 5) as i32),
                Some(strategy),
                Some(&tools_json),
                None,
                None,
                None,
                None,
                Some("traditional"),
                None,  // step_count
                None,  // verification_step_count
                None,  // agentic_step_count
                false, // has_ui_bridge
                None,  // total_tokens
                None,  // total_cost_usd
                None,  // technology_tags
                None,  // domain_tags
                None,  // complexity_tier
            )
            .await?;
    }

    // Add some failures
    for i in 0..3 {
        let tools_json = serde_json::json!(["grep"]).to_string();

        let id = uuid::Uuid::new_v4().to_string();
        state
            .pg_db()
            .record_learning_outcome(
                &id,
                &format!("sample-fail-{}", i),
                "failure",
                None,
                Some(8),
                Some("exhaustive"),
                Some(&tools_json),
                None,
                Some("verification"),
                Some("Verification failed"),
                None,
                Some("traditional"),
                None,  // step_count
                None,  // verification_step_count
                None,  // agentic_step_count
                false, // has_ui_bridge
                None,  // total_tokens
                None,  // total_cost_usd
                None,  // technology_tags
                None,  // domain_tags
                None,  // complexity_tier
            )
            .await?;
    }

    Ok(())
}

// ============================================================================
// Enhanced Learning Queries (Filtering, Pagination, Date Ranges)
// ============================================================================

/// Filter options for learning outcomes query.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LearningOutcomeFilter {
    pub status: Option<String>,
    pub strategy: Option<String>,
    pub since: Option<String>,
    pub limit: Option<i64>,
}

/// Paginated result wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedResult<T> {
    pub items: T,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// Get learning outcomes with optional filtering.
#[tauri::command]
pub async fn get_learning_outcomes_filtered(
    state: State<'_, StorageCompartment>,
    filter: LearningOutcomeFilter,
) -> Result<Vec<serde_json::Value>, String> {
    state
        .pg_db()
        .get_learning_outcomes_filtered(
            filter.status.as_deref(),
            filter.strategy.as_deref(),
            filter.since.as_deref(),
            filter.limit,
        )
        .await
}

/// Get learning outcomes with pagination.
#[tauri::command]
pub async fn get_learning_outcomes_paginated(
    state: State<'_, StorageCompartment>,
    offset: i64,
    limit: i64,
) -> Result<PaginatedResult<Vec<serde_json::Value>>, String> {
    let items = state
        .pg_db()
        .get_learning_outcomes_paginated(offset, limit)
        .await?;
    let total = state.pg_db().get_learning_outcomes_count().await?;
    Ok(PaginatedResult {
        items,
        total,
        offset,
        limit,
    })
}

/// Get learning statistics for a date range.
#[tauri::command]
pub async fn get_learning_stats_by_date_range(
    state: State<'_, StorageCompartment>,
    start: String,
    end: String,
) -> Result<serde_json::Value, String> {
    state
        .pg_db()
        .get_learning_stats_by_date_range(&start, &end)
        .await
}

/// Get total count of learning outcomes.
#[tauri::command]
pub async fn get_learning_outcomes_count(state: State<'_, StorageCompartment>) -> Result<i64, String> {
    state.pg_db().get_learning_outcomes_count().await
}

// ============================================================================
// Task Run Integration (Recent Tasks with Learning Outcomes)
// ============================================================================

/// Get recent task runs with their learning outcomes.
/// This combines task run data with learning outcome data for the dashboard.
#[tauri::command]
pub async fn get_recent_tasks_with_outcomes(
    state: State<'_, StorageCompartment>,
    limit: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.unwrap_or(10);
    state.pg_db().get_recent_task_runs_with_outcomes(limit).await
}

/// Get the current running task (if any).
/// Returns the most recently updated running task.
#[tauri::command]
pub async fn get_current_running_task(
    state: State<'_, StorageCompartment>,
) -> Result<Option<serde_json::Value>, String> {
    let running_tasks = state.pg_db().get_running_task_runs(None).await?;

    // Return the first (most recently updated) running task with basic info
    if let Some(task) = running_tasks.first() {
        Ok(Some(serde_json::json!({
            "id": task.id,
            "task_name": task.task_name,
            "prompt": task.prompt,
            "task_type": task.task_type,
            "status": task.status,
            "sessions_count": task.sessions_count,
            "max_sessions": task.max_sessions,
            "created_at": task.created_at,
            "updated_at": task.updated_at,
        })))
    } else {
        Ok(None)
    }
}

/// Get the most recent task that has checkpoints.
/// Used for auto-selecting in the checkpoint browser.
#[tauri::command]
pub async fn get_most_recent_task_with_checkpoints(
    state: State<'_, StorageCompartment>,
) -> Result<Option<String>, String> {
    state.pg_db().get_most_recent_task_with_checkpoints().await
}

/// Get learning statistics summary for dashboard display.
#[tauri::command]
pub async fn get_learning_stats_summary(
    state: State<'_, StorageCompartment>,
) -> Result<serde_json::Value, String> {
    state.pg_db().get_learning_stats_summary().await
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_learning")
        .invoke_handler(tauri::generate_handler![
            get_learning_summary,
            get_learning_patterns,
            get_learning_insights,
            analyze_learning_data,
            get_feedback_for_context,
            get_learning_dashboard_data,
            record_task_outcome,
            get_best_strategy,
            export_learning_data,
            import_learning_data,
            clear_learning_data,
            add_sample_learning_data,
            get_learning_outcomes_filtered,
            get_learning_outcomes_paginated,
            get_learning_stats_by_date_range,
            get_learning_outcomes_count,
            get_recent_tasks_with_outcomes,
            get_current_running_task,
            get_most_recent_task_with_checkpoints,
            get_learning_stats_summary,
        ])
        .build()
}
