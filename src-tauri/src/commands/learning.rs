//! Tauri commands for the Learning Insights Dashboard.
//!
//! Provides access to learning data persisted in SQLite for displaying
//! AI learning patterns, insights, and task outcome history.

use crate::commands::AppState;
use crate::orchestrator::learning::{
    AnalysisResult, Feedback, Insight, LearningSystem, LearningSummary, Pattern, TaskOutcome,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::State;

/// Global learning system instance for in-memory analysis.
/// The database stores outcomes; this provides real-time analysis.
static LEARNING_SYSTEM: Lazy<Mutex<LearningSystem>> = Lazy::new(|| {
    Mutex::new(LearningSystem::new())
});

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
pub fn get_learning_summary(state: State<'_, Arc<AppState>>) -> Result<LearningSummary, String> {
    // Get outcomes from database
    let outcomes = state.checkpoint_db.get_learning_outcomes(Some(500))?;
    let patterns = state.checkpoint_db.get_learning_patterns()?;

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
pub fn get_learning_patterns(state: State<'_, Arc<AppState>>) -> Result<Vec<Pattern>, String> {
    let patterns_json = state.checkpoint_db.get_learning_patterns()?;

    let patterns: Vec<Pattern> = patterns_json
        .into_iter()
        .map(|p| {
            let pattern_type = match p["pattern_type"].as_str().unwrap_or("unknown") {
                "SuccessStrategy" => crate::orchestrator::learning::PatternType::SuccessStrategy,
                "FailureMode" => crate::orchestrator::learning::PatternType::FailureMode,
                "ToolUsage" => crate::orchestrator::learning::PatternType::ToolUsage,
                "Collaboration" => crate::orchestrator::learning::PatternType::Collaboration,
                "IterationBehavior" => crate::orchestrator::learning::PatternType::IterationBehavior,
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
pub fn get_learning_insights(state: State<'_, Arc<AppState>>) -> Result<Vec<Insight>, String> {
    // Load outcomes from database into the learning system
    let outcomes = state.checkpoint_db.get_learning_outcomes(Some(500))?;
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
pub fn analyze_learning_data(state: State<'_, Arc<AppState>>) -> Result<AnalysisResult, String> {
    // Load outcomes from database
    let outcomes = state.checkpoint_db.get_learning_outcomes(Some(500))?;
    let mut system = LEARNING_SYSTEM.lock().map_err(|e| e.to_string())?;

    // Rebuild system from database outcomes
    *system = LearningSystem::new();
    for outcome_json in outcomes {
        if let Ok(outcome) = serde_json::from_value::<TaskOutcome>(outcome_json.clone()) {
            system.record_outcome(outcome);
        }
    }

    let result = system.analyze_patterns();

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
        let _ = state.checkpoint_db.save_learning_pattern(
            &pattern.id,
            pattern_type,
            &pattern.description,
            pattern.confidence,
            pattern.occurrences,
            Some(&context),
        );
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
pub fn get_learning_dashboard_data(state: State<'_, Arc<AppState>>) -> Result<LearningDashboardData, String> {
    // Load outcomes from database
    let outcomes = state.checkpoint_db.get_learning_outcomes(Some(500))?;
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
pub fn record_task_outcome(
    state: State<'_, Arc<AppState>>,
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
    let feedback_json = if outcome.metrics.is_empty() && outcome.decisions.is_empty() && outcome.tags.is_empty() {
        None
    } else {
        serde_json::to_value(serde_json::json!({
            "metrics": outcome.metrics,
            "decisions": outcome.decisions,
            "tags": outcome.tags,
        })).ok()
    };

    state.checkpoint_db.record_learning_outcome(
        &outcome.task_id,
        status,
        outcome.duration_secs.map(|d| d as f64),
        outcome.iterations,
        outcome.strategy.as_deref(),
        if tools.is_empty() { None } else { Some(&tools) },
        if agents.is_empty() { None } else { Some(&agents) },
        None, // error_type - not in TaskOutcome
        error_message.as_deref(),
        feedback_json.as_ref(),
    )?;

    Ok(())
}

/// Get the best performing strategy.
#[tauri::command]
pub fn get_best_strategy(state: State<'_, Arc<AppState>>) -> Result<Option<(String, f64)>, String> {
    let outcomes = state.checkpoint_db.get_learning_outcomes(Some(500))?;

    let mut strategy_stats: std::collections::HashMap<String, (u32, u32)> = std::collections::HashMap::new();
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
pub fn export_learning_data(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let outcomes = state.checkpoint_db.get_learning_outcomes(None)?;
    let patterns = state.checkpoint_db.get_learning_patterns()?;

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
pub fn add_sample_learning_data(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    // Add successful outcomes
    for i in 0..10 {
        let tools = vec!["grep".to_string(), "edit".to_string()];
        let strategy = if i % 2 == 0 { "incremental" } else { "parallel" };

        state.checkpoint_db.record_learning_outcome(
            &format!("sample-task-{}", i),
            "success",
            Some(60.0 + (i * 10) as f64),
            Some(3 + (i % 5) as u32),
            Some(strategy),
            Some(&tools),
            None,
            None,
            None,
            None,
        )?;
    }

    // Add some failures
    for i in 0..3 {
        let tools = vec!["grep".to_string()];

        state.checkpoint_db.record_learning_outcome(
            &format!("sample-fail-{}", i),
            "failure",
            None,
            Some(8),
            Some("exhaustive"),
            Some(&tools),
            None,
            Some("verification"),
            Some("Verification failed"),
            None,
        )?;
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
pub fn get_learning_outcomes_filtered(
    state: State<'_, Arc<AppState>>,
    filter: LearningOutcomeFilter,
) -> Result<Vec<serde_json::Value>, String> {
    state.checkpoint_db.get_learning_outcomes_filtered(
        filter.status.as_deref(),
        filter.strategy.as_deref(),
        filter.since.as_deref(),
        filter.limit,
    )
}

/// Get learning outcomes with pagination.
#[tauri::command]
pub fn get_learning_outcomes_paginated(
    state: State<'_, Arc<AppState>>,
    offset: i64,
    limit: i64,
) -> Result<PaginatedResult<Vec<serde_json::Value>>, String> {
    let items = state.checkpoint_db.get_learning_outcomes_paginated(offset, limit)?;
    let total = state.checkpoint_db.get_learning_outcomes_count()?;
    Ok(PaginatedResult {
        items,
        total,
        offset,
        limit,
    })
}

/// Get learning statistics for a date range.
#[tauri::command]
pub fn get_learning_stats_by_date_range(
    state: State<'_, Arc<AppState>>,
    start: String,
    end: String,
) -> Result<serde_json::Value, String> {
    state.checkpoint_db.get_learning_stats_by_date_range(&start, &end)
}

/// Get total count of learning outcomes.
#[tauri::command]
pub fn get_learning_outcomes_count(
    state: State<'_, Arc<AppState>>,
) -> Result<i64, String> {
    state.checkpoint_db.get_learning_outcomes_count()
}

// ============================================================================
// Task Run Integration (Recent Tasks with Learning Outcomes)
// ============================================================================

/// Get recent task runs with their learning outcomes.
/// This combines task run data with learning outcome data for the dashboard.
#[tauri::command]
pub fn get_recent_tasks_with_outcomes(
    state: State<'_, Arc<AppState>>,
    limit: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.unwrap_or(10);
    state.checkpoint_db.get_recent_task_runs_with_outcomes(limit)
}

/// Get the current running task (if any).
/// Returns the most recently updated running task.
#[tauri::command]
pub fn get_current_running_task(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<serde_json::Value>, String> {
    let running_tasks = state.checkpoint_db.get_running_task_runs()?;

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
pub fn get_most_recent_task_with_checkpoints(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, String> {
    state.checkpoint_db.get_most_recent_task_with_checkpoints()
}

/// Get learning statistics summary for dashboard display.
#[tauri::command]
pub fn get_learning_stats_summary(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    state.checkpoint_db.get_learning_stats_summary()
}
