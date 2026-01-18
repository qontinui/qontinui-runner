//! Comprehensive backup and restore commands for all user data.
//!
//! This module provides Tauri commands for:
//! - Exporting all user data to a JSON file
//! - Importing data from a JSON backup file
//! - Getting a summary of what will be exported
//! - Preview of what will be imported

use crate::commands::AppState;
use crate::database::ImportResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};

/// Export manifest with version information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportManifest {
    pub version: String,
    pub created_at: String,
    pub app_version: String,
}

/// Comprehensive export data structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComprehensiveExport {
    pub manifest: ExportManifest,
    pub summary: serde_json::Value,
    pub flows: Vec<serde_json::Value>,
    pub flow_executions: Vec<serde_json::Value>,
    pub checkpoints: Vec<serde_json::Value>,
    pub learning_outcomes: Vec<serde_json::Value>,
    pub learning_patterns: Vec<serde_json::Value>,
    pub settings: Vec<serde_json::Value>,
    pub prompts: Vec<serde_json::Value>,
    pub unified_workflows: Vec<serde_json::Value>,
    pub verification_tests: Vec<serde_json::Value>,
    pub task_hooks: Vec<serde_json::Value>,
    pub scheduled_tasks: Vec<serde_json::Value>,
    pub saved_api_requests: Vec<serde_json::Value>,
    pub configs: Vec<serde_json::Value>,
}

/// Options for what to include in the export.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExportOptions {
    #[serde(default = "default_true")]
    pub flows: bool,
    #[serde(default = "default_true")]
    pub flow_executions: bool,
    #[serde(default = "default_true")]
    pub checkpoints: bool,
    #[serde(default = "default_true")]
    pub learning_outcomes: bool,
    #[serde(default = "default_true")]
    pub learning_patterns: bool,
    #[serde(default = "default_true")]
    pub settings: bool,
    #[serde(default = "default_true")]
    pub prompts: bool,
    #[serde(default = "default_true")]
    pub unified_workflows: bool,
    #[serde(default = "default_true")]
    pub verification_tests: bool,
    #[serde(default = "default_true")]
    pub task_hooks: bool,
    #[serde(default = "default_true")]
    pub scheduled_tasks: bool,
    #[serde(default = "default_true")]
    pub saved_api_requests: bool,
    #[serde(default = "default_true")]
    pub configs: bool,
}

fn default_true() -> bool {
    true
}

/// Options for importing data.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImportOptions {
    /// Conflict resolution: "skip" or "overwrite"
    #[serde(default = "default_conflict_mode")]
    pub conflict_mode: String,
    #[serde(default = "default_true")]
    pub flows: bool,
    #[serde(default = "default_true")]
    pub flow_executions: bool,
    #[serde(default = "default_true")]
    pub checkpoints: bool,
    #[serde(default = "default_true")]
    pub learning_outcomes: bool,
    #[serde(default = "default_true")]
    pub learning_patterns: bool,
    #[serde(default = "default_true")]
    pub settings: bool,
    #[serde(default = "default_true")]
    pub prompts: bool,
    #[serde(default = "default_true")]
    pub unified_workflows: bool,
    #[serde(default = "default_true")]
    pub verification_tests: bool,
    #[serde(default = "default_true")]
    pub task_hooks: bool,
    #[serde(default = "default_true")]
    pub scheduled_tasks: bool,
    #[serde(default = "default_true")]
    pub saved_api_requests: bool,
    #[serde(default = "default_true")]
    pub configs: bool,
}

fn default_conflict_mode() -> String {
    "skip".to_string()
}

/// Result of a comprehensive import operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComprehensiveImportResult {
    pub success: bool,
    pub results: HashMap<String, ImportResult>,
    pub total_imported: usize,
    pub total_skipped: usize,
    pub total_errors: usize,
}

/// Preview of what will be imported from a backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportPreview {
    pub manifest: ExportManifest,
    pub counts: serde_json::Value,
    pub is_compatible: bool,
    pub compatibility_issues: Vec<String>,
}

/// Get a summary of all exportable data counts.
#[tauri::command]
pub fn get_export_summary(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    info!("Getting export summary");
    state.checkpoint_db.get_export_summary()
}

/// Export all user data to a JSON structure.
#[tauri::command]
pub fn export_all_data(
    state: State<'_, Arc<AppState>>,
    options: Option<ExportOptions>,
) -> Result<ComprehensiveExport, String> {
    info!("Exporting all user data");
    let opts = options.unwrap_or_default();

    let summary = state.checkpoint_db.get_export_summary()?;

    let flows = if opts.flows {
        state.checkpoint_db.export_all_flows()?
    } else {
        vec![]
    };

    let flow_executions = if opts.flow_executions {
        state.checkpoint_db.export_all_flow_executions()?
    } else {
        vec![]
    };

    let checkpoints = if opts.checkpoints {
        state.checkpoint_db.export_all_orchestrator_checkpoints()?
    } else {
        vec![]
    };

    let learning_outcomes = if opts.learning_outcomes {
        state.checkpoint_db.get_learning_outcomes(None)?
    } else {
        vec![]
    };

    let learning_patterns = if opts.learning_patterns {
        state.checkpoint_db.get_learning_patterns()?
    } else {
        vec![]
    };

    let settings = if opts.settings {
        state.checkpoint_db.export_all_settings()?
    } else {
        vec![]
    };

    let prompts = if opts.prompts {
        state.checkpoint_db.export_all_prompts()?
    } else {
        vec![]
    };

    let unified_workflows = if opts.unified_workflows {
        state.checkpoint_db.export_all_unified_workflows()?
    } else {
        vec![]
    };

    let verification_tests = if opts.verification_tests {
        state.checkpoint_db.export_all_verification_tests()?
    } else {
        vec![]
    };

    let task_hooks = if opts.task_hooks {
        state.checkpoint_db.export_all_task_hooks()?
    } else {
        vec![]
    };

    let scheduled_tasks = if opts.scheduled_tasks {
        state.checkpoint_db.export_all_scheduled_tasks()?
    } else {
        vec![]
    };

    let saved_api_requests = if opts.saved_api_requests {
        state.checkpoint_db.export_all_saved_api_requests()?
    } else {
        vec![]
    };

    let configs = if opts.configs {
        state.checkpoint_db.export_all_configs()?
    } else {
        vec![]
    };

    let export = ComprehensiveExport {
        manifest: ExportManifest {
            version: "2.0.0".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        summary,
        flows,
        flow_executions,
        checkpoints,
        learning_outcomes,
        learning_patterns,
        settings,
        prompts,
        unified_workflows,
        verification_tests,
        task_hooks,
        scheduled_tasks,
        saved_api_requests,
        configs,
    };

    info!(
        "Export complete: {} flows, {} settings, {} prompts",
        export.flows.len(),
        export.settings.len(),
        export.prompts.len()
    );

    Ok(export)
}

/// Get a preview of what will be imported from a backup file.
#[tauri::command]
pub fn get_import_preview(data: ComprehensiveExport) -> Result<ImportPreview, String> {
    info!("Getting import preview");

    let mut compatibility_issues = Vec::new();

    // Check version compatibility
    let version = &data.manifest.version;
    let is_compatible = version.starts_with("2.") || version.starts_with("1.");

    if !is_compatible {
        compatibility_issues.push(format!("Unknown backup version: {}", version));
    }

    let counts = serde_json::json!({
        "flows": data.flows.len(),
        "flow_executions": data.flow_executions.len(),
        "checkpoints": data.checkpoints.len(),
        "learning_outcomes": data.learning_outcomes.len(),
        "learning_patterns": data.learning_patterns.len(),
        "settings": data.settings.len(),
        "prompts": data.prompts.len(),
        "unified_workflows": data.unified_workflows.len(),
        "verification_tests": data.verification_tests.len(),
        "task_hooks": data.task_hooks.len(),
        "scheduled_tasks": data.scheduled_tasks.len(),
        "saved_api_requests": data.saved_api_requests.len(),
        "configs": data.configs.len(),
    });

    Ok(ImportPreview {
        manifest: data.manifest,
        counts,
        is_compatible,
        compatibility_issues,
    })
}

/// Import all data from a comprehensive backup.
#[tauri::command]
pub fn import_all_data(
    state: State<'_, Arc<AppState>>,
    data: ComprehensiveExport,
    options: Option<ImportOptions>,
) -> Result<ComprehensiveImportResult, String> {
    info!("Importing all user data");
    let opts = options.unwrap_or_default();
    let conflict_mode = &opts.conflict_mode;

    let mut results: HashMap<String, ImportResult> = HashMap::new();
    let mut total_imported = 0;
    let mut total_skipped = 0;
    let mut total_errors = 0;

    // Import flows
    if opts.flows && !data.flows.is_empty() {
        match state.checkpoint_db.import_flows(&data.flows, conflict_mode) {
            Ok(result) => {
                total_imported += result.imported;
                total_skipped += result.skipped;
                total_errors += result.errors.len();
                results.insert("flows".to_string(), result);
            }
            Err(e) => {
                error!("Failed to import flows: {}", e);
                results.insert(
                    "flows".to_string(),
                    ImportResult {
                        imported: 0,
                        skipped: 0,
                        errors: vec![e],
                    },
                );
            }
        }
    }

    // Import prompts
    if opts.prompts && !data.prompts.is_empty() {
        match state
            .checkpoint_db
            .import_prompts(&data.prompts, conflict_mode)
        {
            Ok(result) => {
                total_imported += result.imported;
                total_skipped += result.skipped;
                total_errors += result.errors.len();
                results.insert("prompts".to_string(), result);
            }
            Err(e) => {
                error!("Failed to import prompts: {}", e);
                results.insert(
                    "prompts".to_string(),
                    ImportResult {
                        imported: 0,
                        skipped: 0,
                        errors: vec![e],
                    },
                );
            }
        }
    }

    // Import settings
    if opts.settings && !data.settings.is_empty() {
        match state
            .checkpoint_db
            .import_settings(&data.settings, conflict_mode)
        {
            Ok(result) => {
                total_imported += result.imported;
                total_skipped += result.skipped;
                total_errors += result.errors.len();
                results.insert("settings".to_string(), result);
            }
            Err(e) => {
                error!("Failed to import settings: {}", e);
                results.insert(
                    "settings".to_string(),
                    ImportResult {
                        imported: 0,
                        skipped: 0,
                        errors: vec![e],
                    },
                );
            }
        }
    }

    // Import unified workflows
    if opts.unified_workflows && !data.unified_workflows.is_empty() {
        match state
            .checkpoint_db
            .import_unified_workflows(&data.unified_workflows, conflict_mode)
        {
            Ok(result) => {
                total_imported += result.imported;
                total_skipped += result.skipped;
                total_errors += result.errors.len();
                results.insert("unified_workflows".to_string(), result);
            }
            Err(e) => {
                error!("Failed to import unified workflows: {}", e);
                results.insert(
                    "unified_workflows".to_string(),
                    ImportResult {
                        imported: 0,
                        skipped: 0,
                        errors: vec![e],
                    },
                );
            }
        }
    }

    // Import learning outcomes
    if opts.learning_outcomes && !data.learning_outcomes.is_empty() {
        match state
            .checkpoint_db
            .import_learning_outcomes(&data.learning_outcomes, conflict_mode)
        {
            Ok(result) => {
                total_imported += result.imported;
                total_skipped += result.skipped;
                total_errors += result.errors.len();
                results.insert("learning_outcomes".to_string(), result);
            }
            Err(e) => {
                error!("Failed to import learning outcomes: {}", e);
                results.insert(
                    "learning_outcomes".to_string(),
                    ImportResult {
                        imported: 0,
                        skipped: 0,
                        errors: vec![e],
                    },
                );
            }
        }
    }

    // Import learning patterns
    if opts.learning_patterns && !data.learning_patterns.is_empty() {
        match state
            .checkpoint_db
            .import_learning_patterns(&data.learning_patterns, conflict_mode)
        {
            Ok(result) => {
                total_imported += result.imported;
                total_skipped += result.skipped;
                total_errors += result.errors.len();
                results.insert("learning_patterns".to_string(), result);
            }
            Err(e) => {
                error!("Failed to import learning patterns: {}", e);
                results.insert(
                    "learning_patterns".to_string(),
                    ImportResult {
                        imported: 0,
                        skipped: 0,
                        errors: vec![e],
                    },
                );
            }
        }
    }

    let success = total_errors == 0;

    info!(
        "Import complete: {} imported, {} skipped, {} errors",
        total_imported, total_skipped, total_errors
    );

    Ok(ComprehensiveImportResult {
        success,
        results,
        total_imported,
        total_skipped,
        total_errors,
    })
}
