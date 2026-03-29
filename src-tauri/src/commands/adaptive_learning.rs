//! Tauri commands for the Adaptive Learning system (Plan 15).
//!
//! Provides access to playbook entries, curated examples, template performance,
//! GEPA optimization history, and learning statistics.

use crate::commands::AppState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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
    let db = &state.checkpoint_db;
    let conn = db.get_conn()?;

    let playbook_total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM playbook_entries",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let active: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM playbook_entries WHERE status = 'active'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let staged: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM playbook_entries WHERE status = 'staged'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let retired: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM playbook_entries WHERE status = 'retired'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let examples: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM curated_examples",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let templates: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM template_performance",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let gepa: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM gepa_optimization_runs",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // Average helpfulness ratio for applied entries
    let avg_helpfulness: f64 = conn
        .query_row(
            "SELECT COALESCE(AVG(CAST(times_helped AS REAL) / NULLIF(times_applied, 0)), 0.0)
             FROM playbook_entries WHERE times_applied > 0",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0.0);

    Ok(AdaptiveLearningStats {
        playbook_entries: playbook_total,
        active_lessons: active,
        staged_lessons: staged,
        retired_lessons: retired,
        curated_examples: examples,
        templates_tracked: templates,
        gepa_runs: gepa,
        avg_lesson_helpfulness: avg_helpfulness,
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
    let db = &state.checkpoint_db;
    let conn = db.get_conn()?;
    let limit = limit.unwrap_or(100) as i64;

    let mut query = String::from(
        "SELECT id, lesson, category, domain, severity, positive, times_applied, times_helped, status, created_at
         FROM playbook_entries WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 0;

    if let Some(ref d) = domain {
        param_idx += 1;
        query.push_str(&format!(" AND domain = ?{}", param_idx));
        params.push(Box::new(d.clone()));
    }
    if let Some(ref s) = status {
        param_idx += 1;
        query.push_str(&format!(" AND status = ?{}", param_idx));
        params.push(Box::new(s.clone()));
    }

    query.push_str(
        " ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'important' THEN 1 ELSE 2 END,
          times_helped DESC",
    );
    param_idx += 1;
    query.push_str(&format!(" LIMIT ?{}", param_idx));
    params.push(Box::new(limit));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
    let entries = stmt
        .query_map(param_refs.as_slice(), |row| {
            let times_applied: i64 = row.get(6)?;
            let times_helped: i64 = row.get(7)?;
            let helpfulness = if times_applied > 0 {
                times_helped as f64 / times_applied as f64
            } else {
                0.0
            };

            Ok(PlaybookEntryResponse {
                id: row.get(0)?,
                lesson: row.get(1)?,
                category: row.get(2)?,
                domain: row.get(3)?,
                severity: row.get(4)?,
                positive: row.get::<_, i64>(5)? != 0,
                times_applied,
                times_helped,
                helpfulness_ratio: helpfulness,
                status: row.get(8)?,
                created_at: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
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
    let db = &state.checkpoint_db;
    let conn = db.get_conn()?;
    let limit = limit.unwrap_or(20) as i64;

    let mut stmt = conn
        .prepare(
            "SELECT id, domain, criterion_description, steps_json, quality_score, execution_verified, times_used, created_at
             FROM curated_examples
             WHERE domain = ?1
             ORDER BY quality_score DESC
             LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;

    let examples: Vec<serde_json::Value> = stmt
        .query_map(rusqlite::params![domain, limit], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "domain": row.get::<_, String>(1)?,
                "criterion_description": row.get::<_, String>(2)?,
                "steps_json": row.get::<_, String>(3)?,
                "quality_score": row.get::<_, f64>(4)?,
                "execution_verified": row.get::<_, i64>(5)? != 0,
                "times_used": row.get::<_, i64>(6)?,
                "created_at": row.get::<_, String>(7)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(examples)
}

/// Get template performance data.
#[tauri::command]
pub async fn get_template_performance(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TemplatePerformanceResponse>, String> {
    let db = &state.checkpoint_db;
    let conn = db.get_conn()?;

    let mut stmt = conn
        .prepare(
            "SELECT template_id, template_name, source, success_count, failure_count,
                    total_quality_score, last_used_at
             FROM template_performance
             ORDER BY (success_count + failure_count) DESC",
        )
        .map_err(|e| e.to_string())?;

    let templates: Vec<TemplatePerformanceResponse> = stmt
        .query_map([], |row| {
            let success: i64 = row.get(3)?;
            let failure: i64 = row.get(4)?;
            let total_quality: f64 = row.get(5)?;
            let total = success + failure;
            let confidence = if total > 0 {
                success as f64 / total as f64
            } else {
                0.0
            };
            let avg_quality = if total > 0 {
                total_quality / total as f64
            } else {
                0.0
            };

            Ok(TemplatePerformanceResponse {
                template_id: row.get(0)?,
                template_name: row.get(1)?,
                source: row.get(2)?,
                success_count: success,
                failure_count: failure,
                confidence,
                avg_quality,
                last_used_at: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(templates)
}

/// Get GEPA optimization run history.
#[tauri::command]
pub async fn get_gepa_runs(
    state: State<'_, Arc<AppState>>,
    limit: Option<u32>,
) -> Result<Vec<GepaRunResponse>, String> {
    let db = &state.checkpoint_db;
    let conn = db.get_conn()?;
    let limit = limit.unwrap_or(50) as i64;

    let mut stmt = conn
        .prepare(
            "SELECT id, domain, old_score, new_score, improvement, status, created_at
             FROM gepa_optimization_runs
             ORDER BY created_at DESC
             LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;

    let runs: Vec<GepaRunResponse> = stmt
        .query_map(rusqlite::params![limit], |row| {
            Ok(GepaRunResponse {
                id: row.get(0)?,
                domain: row.get(1)?,
                old_score: row.get(2)?,
                new_score: row.get(3)?,
                improvement: row.get(4)?,
                status: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(runs)
}

/// Get template lifecycle event history for a specific template.
#[tauri::command]
pub async fn get_template_lifecycle_history(
    state: State<'_, Arc<AppState>>,
    template_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let db = &state.checkpoint_db;
    let conn = db.get_conn()?;

    let mut stmt = conn
        .prepare(
            "SELECT id, template_id, action, old_source, new_source, confidence_at_transition, created_at
             FROM template_lifecycle_events
             WHERE template_id = ?1
             ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;

    let events: Vec<serde_json::Value> = stmt
        .query_map(rusqlite::params![template_id], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "template_id": row.get::<_, String>(1)?,
                "action": row.get::<_, String>(2)?,
                "old_source": row.get::<_, String>(3)?,
                "new_source": row.get::<_, String>(4)?,
                "confidence_at_transition": row.get::<_, f64>(5)?,
                "created_at": row.get::<_, String>(6)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(events)
}
