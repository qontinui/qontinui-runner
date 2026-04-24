//! Tauri commands for the Adaptive Learning system (Plan 15).
//!
//! Provides access to playbook entries, curated examples, template performance,
//! GEPA optimization history, and learning statistics.
//! All queries go through PostgreSQL via `state.pg_db`.

use crate::commands::AppState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;

// ============================================================================
// Response types
// ============================================================================

/// Adaptive learning statistics overview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveLearningStats {
    pub playbook_entries: i64,
    pub active_lessons: i64,
    pub staged_lessons: i64,
    pub retired_lessons: i64,
    pub curated_examples: i64,
    pub templates_tracked: i64,
    pub gepa_runs: i64,
    pub avg_lesson_helpfulness: f64,
}

/// Playbook entry for frontend display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybookEntryResponse {
    pub id: String,
    pub lesson: String,
    pub category: String,
    pub domain: Option<String>,
    pub severity: String,
    pub positive: bool,
    pub times_applied: i64,
    pub times_helped: i64,
    pub helpfulness_ratio: f64,
    pub status: String,
    pub created_at: String,
}

/// Template performance for frontend display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplatePerformanceResponse {
    pub template_id: String,
    pub template_name: String,
    pub source: String,
    pub success_count: i64,
    pub failure_count: i64,
    pub confidence: f64,
    pub avg_quality: f64,
    pub last_used_at: Option<String>,
}

/// GEPA optimization run for frontend display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GepaRunResponse {
    pub id: String,
    pub domain: String,
    pub old_score: Option<f64>,
    pub new_score: Option<f64>,
    pub improvement: Option<f64>,
    pub status: String,
    pub created_at: String,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get adaptive learning statistics overview.
#[tauri::command]
pub async fn get_adaptive_learning_stats(
    state: State<'_, Arc<AppState>>,
) -> Result<AdaptiveLearningStats, String> {
    let pg = &state.pg_db;
    let stats = pg.get_adaptive_learning_stats().await?;

    Ok(AdaptiveLearningStats {
        playbook_entries: stats["playbook_entries"].as_i64().unwrap_or(0),
        active_lessons: stats["active_lessons"].as_i64().unwrap_or(0),
        staged_lessons: stats["staged_lessons"].as_i64().unwrap_or(0),
        retired_lessons: stats["retired_lessons"].as_i64().unwrap_or(0),
        curated_examples: stats["curated_examples"].as_i64().unwrap_or(0),
        templates_tracked: stats["templates_tracked"].as_i64().unwrap_or(0),
        gepa_runs: stats["gepa_runs"].as_i64().unwrap_or(0),
        avg_lesson_helpfulness: stats["avg_lesson_helpfulness"].as_f64().unwrap_or(0.0),
    })
}

/// Get playbook entries, optionally filtered by domain and status.
#[tauri::command]
pub async fn get_playbook_entries(
    state: State<'_, Arc<AppState>>,
    domain: Option<String>,
    status: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<PlaybookEntryResponse>, String> {
    let pg = &state.pg_db;
    let limit = limit.unwrap_or(100) as i64;

    let rows = pg
        .list_playbook_entries(domain.as_deref(), status.as_deref(), limit)
        .await?;

    let entries = rows
        .into_iter()
        .map(|v| PlaybookEntryResponse {
            id: v["id"].as_str().unwrap_or("").to_string(),
            lesson: v["lesson"].as_str().unwrap_or("").to_string(),
            category: v["category"].as_str().unwrap_or("").to_string(),
            domain: v["domain"].as_str().map(|s| s.to_string()),
            severity: v["severity"].as_str().unwrap_or("minor").to_string(),
            positive: v["positive"].as_bool().unwrap_or(false),
            times_applied: v["times_applied"].as_i64().unwrap_or(0),
            times_helped: v["times_helped"].as_i64().unwrap_or(0),
            helpfulness_ratio: v["helpfulness_ratio"].as_f64().unwrap_or(0.0),
            status: v["status"].as_str().unwrap_or("staged").to_string(),
            created_at: v["created_at"].as_str().unwrap_or("").to_string(),
        })
        .collect();

    Ok(entries)
}

/// Get curated few-shot examples for a domain.
#[tauri::command]
pub async fn get_curated_examples(
    state: State<'_, Arc<AppState>>,
    domain: String,
    limit: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let pg = &state.pg_db;
    let limit = limit.unwrap_or(20) as i64;
    pg.get_curated_examples_by_domain(&domain, limit).await
}

/// Get template performance data.
#[tauri::command]
pub async fn get_template_performance(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TemplatePerformanceResponse>, String> {
    let pg = &state.pg_db;
    let rows = pg.get_all_template_performance().await?;

    let templates = rows
        .into_iter()
        .map(|v| TemplatePerformanceResponse {
            template_id: v["template_id"].as_str().unwrap_or("").to_string(),
            template_name: v["template_name"].as_str().unwrap_or("").to_string(),
            source: v["source"].as_str().unwrap_or("manual").to_string(),
            success_count: v["success_count"].as_i64().unwrap_or(0),
            failure_count: v["failure_count"].as_i64().unwrap_or(0),
            confidence: v["confidence"].as_f64().unwrap_or(0.0),
            avg_quality: v["avg_quality"].as_f64().unwrap_or(0.0),
            last_used_at: v["last_used_at"].as_str().map(|s| s.to_string()),
        })
        .collect();

    Ok(templates)
}

/// Get GEPA optimization run history.
#[tauri::command]
pub async fn get_gepa_runs(
    state: State<'_, Arc<AppState>>,
    limit: Option<u32>,
) -> Result<Vec<GepaRunResponse>, String> {
    let pg = &state.pg_db;
    let limit = limit.unwrap_or(50) as i64;
    let rows = pg.get_recent_gepa_runs(limit).await?;

    let runs = rows
        .into_iter()
        .map(|v| GepaRunResponse {
            id: v["id"].as_str().unwrap_or("").to_string(),
            domain: v["domain"].as_str().unwrap_or("").to_string(),
            old_score: v["old_score"].as_f64(),
            new_score: v["new_score"].as_f64(),
            improvement: v["improvement"].as_f64(),
            status: v["status"].as_str().unwrap_or("pending").to_string(),
            created_at: v["created_at"].as_str().unwrap_or("").to_string(),
        })
        .collect();

    Ok(runs)
}

/// Get template lifecycle event history for a specific template.
#[tauri::command]
pub async fn get_template_lifecycle_history(
    state: State<'_, Arc<AppState>>,
    template_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let pg = &state.pg_db;
    pg.get_template_lifecycle_events(&template_id).await
}

// ============================================================================
// Mutation commands
// ============================================================================

/// Update a playbook entry's status (activate, retire, stage).
#[tauri::command]
pub async fn update_playbook_entry_status(
    state: State<'_, Arc<AppState>>,
    id: String,
    new_status: String,
) -> Result<(), String> {
    // Validate status
    if !["active", "staged", "retired"].contains(&new_status.as_str()) {
        return Err(format!(
            "Invalid status '{}'. Must be active, staged, or retired.",
            new_status
        ));
    }

    let pg = &state.pg_db;
    pg.update_playbook_status(&id, &new_status).await
}

/// Delete a playbook entry.
#[tauri::command]
pub async fn delete_playbook_entry(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let pg = &state.pg_db;
    pg.delete_playbook_entry(&id).await
}

/// Delete a curated example.
#[tauri::command]
pub async fn delete_curated_example(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let pg = &state.pg_db;
    pg.delete_curated_example(&id).await
}

/// Get full detail for a GEPA optimization run (including before/after prompts).
#[tauri::command]
pub async fn get_gepa_run_detail(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<serde_json::Value, String> {
    let pg = &state.pg_db;
    pg.get_gepa_run_detail(&id).await
}

/// Get playbook entry detail with source run context.
#[tauri::command]
pub async fn get_playbook_entry_detail(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<serde_json::Value, String> {
    let pg = &state.pg_db;
    pg.get_playbook_entry_detail(&id).await
}

/// Get learning trend data (entries created per day, rolling counts).
#[tauri::command]
pub async fn get_learning_trends(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<serde_json::Value, String> {
    let pg = &state.pg_db;
    let days = days.unwrap_or(30) as i64;
    pg.get_learning_trends(days).await
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_adaptive_learning")
        .invoke_handler(tauri::generate_handler![
            get_adaptive_learning_stats,
            get_playbook_entries,
            get_curated_examples,
            get_template_performance,
            get_gepa_runs,
            get_template_lifecycle_history,
            update_playbook_entry_status,
            delete_playbook_entry,
            delete_curated_example,
            get_gepa_run_detail,
            get_playbook_entry_detail,
            get_learning_trends,
        ])
        .build()
}
