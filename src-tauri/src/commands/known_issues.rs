//! Tauri commands for known issue operations.
//!
//! Provides commands for querying and managing known issues
//! stored in PostgreSQL.

use std::sync::Arc;
use tauri::State;
use tracing::info;

use crate::commands::AppState;
use crate::known_issues::{
    CreateKnownIssueRequest, CreatePatternTemplateRequest, IssuePatternTemplate, KnownIssue,
    ListKnownIssuesQuery, UpdateKnownIssueRequest,
};

// ── Export / Import types ──────────────────────────────────────────────

/// Portable representation of a single issue for export/import.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ExportableIssue {
    title: String,
    description: String,
    category: String,
    scope_type: String,
    scope_value: Option<String>,
    scope_tags: Vec<String>,
    detection_method: String,
    detection_config: serde_json::Value,
    pattern_template_id: Option<String>,
    reproduction_context: Option<String>,
    trigger_conditions: Vec<String>,
    severity: String,
    confidence: f64,
    provenance: String,
    source_finding_ids: Vec<String>,
    source_task_run_id: Option<String>,
    verification_hint: Option<String>,
    verification_step_template: Option<serde_json::Value>,
}

impl From<&KnownIssue> for ExportableIssue {
    fn from(issue: &KnownIssue) -> Self {
        Self {
            title: issue.title.clone(),
            description: issue.description.clone(),
            category: serde_json::to_value(&issue.category)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "other".to_string()),
            scope_type: serde_json::to_value(&issue.scope_type)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "global".to_string()),
            scope_value: issue.scope_value.clone(),
            scope_tags: issue.scope_tags.clone(),
            detection_method: serde_json::to_value(&issue.detection_method)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "ai_judgment".to_string()),
            detection_config: issue.detection_config.clone(),
            pattern_template_id: issue.pattern_template_id.clone(),
            reproduction_context: issue.reproduction_context.clone(),
            trigger_conditions: issue.trigger_conditions.clone(),
            severity: serde_json::to_value(&issue.severity)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "medium".to_string()),
            confidence: issue.confidence,
            provenance: serde_json::to_value(&issue.provenance)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "manual".to_string()),
            source_finding_ids: issue.source_finding_ids.clone(),
            source_task_run_id: issue.source_task_run_id.clone(),
            verification_hint: issue.verification_hint.clone(),
            verification_step_template: issue.verification_step_template.clone(),
        }
    }
}

/// Top-level export envelope.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct KnownIssuesExport {
    version: String,
    exported_at: String,
    issues: Vec<ExportableIssue>,
}

/// Result returned by the import command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
}

/// List known issues with optional filters.
#[tauri::command]
pub async fn list_known_issues(
    query: ListKnownIssuesQuery,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<KnownIssue>, String> {
    app_state.pg_db.list_known_issues(&query).await
}

/// Find known issues relevant to a specific spec.
#[tauri::command]
pub async fn find_issues_for_spec(
    spec_id: String,
    page_url: Option<String>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<KnownIssue>, String> {
    app_state
        .pg_db
        .find_issues_for_spec(&spec_id, page_url.as_deref())
        .await
}

/// Create a new known issue.
#[tauri::command]
pub async fn create_known_issue(
    request: CreateKnownIssueRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<KnownIssue, String> {
    let issue = app_state.pg_db.create_known_issue(&request).await?;
    info!("Created known issue: {} ({})", issue.title, issue.id);
    Ok(issue)
}

/// Update an existing known issue.
#[tauri::command]
pub async fn update_known_issue(
    id: String,
    request: UpdateKnownIssueRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<KnownIssue, String> {
    let issue = app_state.pg_db.update_known_issue(&id, &request).await?;
    info!("Updated known issue: {} ({})", issue.title, issue.id);
    Ok(issue)
}

/// Delete a known issue.
#[tauri::command]
pub async fn delete_known_issue(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let deleted = app_state.pg_db.delete_known_issue(&id).await?;
    if deleted {
        info!("Deleted known issue: {}", id);
    }
    Ok(deleted)
}

/// Resolve a known issue (set status to resolved).
#[tauri::command]
pub async fn resolve_known_issue(
    id: String,
    resolution: Option<String>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    app_state
        .pg_db
        .resolve_known_issue(&id, resolution.as_deref())
        .await?;
    info!("Resolved known issue: {}", id);
    Ok(())
}

/// List all issue pattern templates.
#[tauri::command]
pub async fn list_pattern_templates(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<IssuePatternTemplate>, String> {
    app_state.pg_db.list_pattern_templates().await
}

/// Export known issues as a portable JSON string.
///
/// Accepts optional `status` and `category` filters. Instance-specific
/// fields (id, timestamps, counters) are stripped so the export can be
/// imported into any runner instance.
#[tauri::command]
pub async fn export_known_issues(
    status: Option<String>,
    category: Option<String>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let query = ListKnownIssuesQuery {
        status,
        category,
        ..Default::default()
    };

    let issues = app_state.pg_db.list_known_issues(&query).await?;

    let export = KnownIssuesExport {
        version: "1.0".to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        issues: issues.iter().map(ExportableIssue::from).collect(),
    };

    serde_json::to_string_pretty(&export).map_err(|e| format!("Failed to serialize export: {e}"))
}

/// Import known issues from a portable JSON string.
///
/// Each issue receives a fresh ID and has its provenance set to
/// `"imported"`, status to `"active"`, times_detected to 1, and
/// times_checked to 0.  Issues whose title already exists in the
/// database are silently skipped.
#[tauri::command]
pub async fn import_known_issues(
    json_data: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<ImportResult, String> {
    let export: KnownIssuesExport =
        serde_json::from_str(&json_data).map_err(|e| format!("Invalid JSON: {e}"))?;

    if export.version != "1.0" {
        return Err(format!(
            "Unsupported export version: {}. Expected 1.0",
            export.version
        ));
    }

    // Collect existing titles for dedup.
    let existing = app_state
        .pg_db
        .list_known_issues(&ListKnownIssuesQuery::default())
        .await?;
    let existing_titles: std::collections::HashSet<String> =
        existing.iter().map(|i| i.title.clone()).collect();

    let mut imported = 0usize;
    let mut skipped = 0usize;

    for item in &export.issues {
        if existing_titles.contains(&item.title) {
            skipped += 1;
            continue;
        }

        use crate::known_issues::types::*;

        let req = CreateKnownIssueRequest {
            title: item.title.clone(),
            description: item.description.clone(),
            category: IssueCategory::from_str(&item.category).unwrap_or(IssueCategory::Other),
            scope_type: ScopeType::from_str(&item.scope_type).unwrap_or(ScopeType::Global),
            scope_value: item.scope_value.clone(),
            scope_tags: Some(item.scope_tags.clone()),
            detection_method: DetectionMethod::from_str(&item.detection_method)
                .unwrap_or(DetectionMethod::AiJudgment),
            detection_config: Some(item.detection_config.clone()),
            pattern_template_id: item.pattern_template_id.clone(),
            reproduction_context: item.reproduction_context.clone(),
            trigger_conditions: Some(item.trigger_conditions.clone()),
            severity: IssueSeverity::from_str(&item.severity).unwrap_or(IssueSeverity::Medium),
            provenance: Some(IssueProvenance::Imported),
            source_finding_ids: Some(item.source_finding_ids.clone()),
            source_task_run_id: item.source_task_run_id.clone(),
            verification_hint: item.verification_hint.clone(),
            verification_step_template: item.verification_step_template.clone(),
        };

        match app_state.pg_db.create_known_issue(&req).await {
            Ok(issue) => {
                info!("Imported known issue: {} ({})", issue.title, issue.id);
                imported += 1;
            }
            Err(e) => {
                tracing::warn!("Failed to import issue '{}': {}", item.title, e);
                skipped += 1;
            }
        }
    }

    info!(
        "Import complete: {} imported, {} skipped",
        imported, skipped
    );
    Ok(ImportResult { imported, skipped })
}

/// Create a new pattern template.
#[tauri::command]
pub async fn create_pattern_template(
    request: CreatePatternTemplateRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<IssuePatternTemplate, String> {
    let template = app_state.pg_db.insert_pattern_template(&request).await?;
    info!(
        "Created pattern template: {} ({})",
        template.name, template.id
    );
    Ok(template)
}
