//! Comprehensive backup and restore commands for all user data.
//!
//! This module provides Tauri commands for:
//! - Exporting all user data to a JSON file
//! - Importing data from a JSON backup file
//! - Getting a summary of what will be exported
//! - Preview of what will be imported

use crate::commands::AppState;
use crate::database::pg::PgDb;
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
pub async fn get_export_summary(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    info!("Getting export summary");
    state.pg_db.get_export_summary().await
}

/// Export all user data to a JSON structure.
#[tauri::command]
pub async fn export_all_data(
    state: State<'_, Arc<AppState>>,
    options: Option<ExportOptions>,
) -> Result<ComprehensiveExport, String> {
    export_all_data_impl(&state.pg_db, options).await
}

/// Free-function implementation of comprehensive export, callable from any
/// context that has access to a `PgDb` handle (Tauri commands, MCP handlers).
pub async fn export_all_data_impl(
    pg_db: &PgDb,
    options: Option<ExportOptions>,
) -> Result<ComprehensiveExport, String> {
    info!("Exporting all user data");
    let opts = options.unwrap_or_default();

    let summary = pg_db.get_export_summary().await?;

    let flows = if opts.flows {
        pg_db.export_all_flows().await?
    } else {
        vec![]
    };

    let flow_executions = if opts.flow_executions {
        pg_db.export_all_flow_executions().await?
    } else {
        vec![]
    };

    let checkpoints = if opts.checkpoints {
        pg_db.export_all_orchestrator_checkpoints().await?
    } else {
        vec![]
    };

    let learning_outcomes = if opts.learning_outcomes {
        pg_db.get_learning_outcomes(None).await?
    } else {
        vec![]
    };

    let learning_patterns = if opts.learning_patterns {
        pg_db.get_learning_patterns().await?
    } else {
        vec![]
    };

    let settings = if opts.settings {
        pg_db.export_all_settings().await?
    } else {
        vec![]
    };

    let prompts = if opts.prompts {
        pg_db.export_all_prompts().await?
    } else {
        vec![]
    };

    let unified_workflows = if opts.unified_workflows {
        pg_db.export_all_unified_workflows().await?
    } else {
        vec![]
    };

    let verification_tests = if opts.verification_tests {
        pg_db.export_all_verification_tests().await?
    } else {
        vec![]
    };

    let task_hooks = if opts.task_hooks {
        pg_db.export_all_task_hooks().await?
    } else {
        vec![]
    };

    let scheduled_tasks = if opts.scheduled_tasks {
        pg_db.export_all_scheduled_tasks().await?
    } else {
        vec![]
    };

    let saved_api_requests = if opts.saved_api_requests {
        pg_db.export_all_saved_api_requests().await?
    } else {
        vec![]
    };

    let configs = if opts.configs {
        pg_db.export_all_configs().await?
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
pub async fn import_all_data(
    state: State<'_, Arc<AppState>>,
    data: ComprehensiveExport,
    options: Option<ImportOptions>,
) -> Result<ComprehensiveImportResult, String> {
    import_all_data_impl(&state.pg_db, data, options).await
}

/// Free-function implementation of comprehensive import, callable from any
/// context that has access to a `PgDb` handle (Tauri commands, MCP handlers).
pub async fn import_all_data_impl(
    pg_db: &PgDb,
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
        match pg_db.import_flows(&data.flows).await {
            Ok(count) => {
                total_imported += count as usize;
                results.insert(
                    "flows".to_string(),
                    ImportResult {
                        imported: count as usize,
                        skipped: 0,
                        errors: vec![],
                    },
                );
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

    let is_overwrite = conflict_mode == "overwrite";

    // Import prompts via PG upsert
    if opts.prompts && !data.prompts.is_empty() {
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut errors = Vec::new();

        for item in &data.prompts {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let category = item
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("general");
            let content = item
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let variables = item
                .get("variables")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());

            if id.is_empty() || name.is_empty() {
                skipped += 1;
                continue;
            }

            let sql = if is_overwrite {
                r#"INSERT INTO prompts (id, name, category, content, variables)
                   VALUES ($1, $2, $3, $4, $5)
                   ON CONFLICT (id) DO UPDATE SET name = $2, category = $3, content = $4, variables = $5, updated_at = NOW()"#
            } else {
                r#"INSERT INTO prompts (id, name, category, content, variables)
                   VALUES ($1, $2, $3, $4, $5)
                   ON CONFLICT (id) DO NOTHING"#
            };

            match pg_db.pool().get().await {
                Ok(conn) => match conn
                    .execute(sql, &[&id, &name, &category, &content, &variables])
                    .await
                {
                    Ok(n) => {
                        if n > 0 {
                            imported += 1;
                        } else {
                            skipped += 1;
                        }
                    }
                    Err(e) => errors.push(format!("Prompt {}: {}", id, e)),
                },
                Err(e) => errors.push(format!("PG pool: {}", e)),
            }
        }
        total_imported += imported;
        total_skipped += skipped;
        total_errors += errors.len();
        results.insert(
            "prompts".to_string(),
            ImportResult {
                imported,
                skipped,
                errors,
            },
        );
    }

    // Import settings via PG upsert
    if opts.settings && !data.settings.is_empty() {
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut errors = Vec::new();

        for item in &data.settings {
            let key = item.get("key").and_then(|v| v.as_str()).unwrap_or_default();
            let value = item
                .get("value")
                .map(|v| {
                    if let Some(s) = v.as_str() {
                        s.to_string()
                    } else {
                        v.to_string()
                    }
                })
                .unwrap_or_default();

            if key.is_empty() {
                skipped += 1;
                continue;
            }

            let sql = if is_overwrite {
                "INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = NOW()"
            } else {
                "INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO NOTHING"
            };

            match pg_db.pool().get().await {
                Ok(conn) => match conn.execute(sql, &[&key, &value]).await {
                    Ok(n) => {
                        if n > 0 {
                            imported += 1;
                        } else {
                            skipped += 1;
                        }
                    }
                    Err(e) => errors.push(format!("Setting {}: {}", key, e)),
                },
                Err(e) => errors.push(format!("PG pool: {}", e)),
            }
        }
        total_imported += imported;
        total_skipped += skipped;
        total_errors += errors.len();
        results.insert(
            "settings".to_string(),
            ImportResult {
                imported,
                skipped,
                errors,
            },
        );
    }

    // Import unified_workflows via PG upsert
    if opts.unified_workflows && !data.unified_workflows.is_empty() {
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut errors = Vec::new();

        for item in &data.unified_workflows {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            if id.is_empty() || name.is_empty() {
                skipped += 1;
                continue;
            }

            let desc = item
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let category = item
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("general");
            let tags = item
                .get("tags")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());
            let setup = item
                .get("setup_steps")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());
            let verif = item
                .get("verification_steps")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());
            let agentic = item
                .get("agentic_steps")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());
            let completion = item
                .get("completion_steps")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());
            let max_iters = item
                .get("max_iterations")
                .and_then(|v| v.as_i64())
                .unwrap_or(10);

            let sql = if is_overwrite {
                r#"INSERT INTO unified_workflows (id, name, description, category, tags, setup_steps, verification_steps, agentic_steps, completion_steps, max_iterations)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                   ON CONFLICT (id) DO UPDATE SET
                     name = $2, description = $3, category = $4, tags = $5,
                     setup_steps = $6, verification_steps = $7, agentic_steps = $8,
                     completion_steps = $9, max_iterations = $10, updated_at = NOW()"#
            } else {
                r#"INSERT INTO unified_workflows (id, name, description, category, tags, setup_steps, verification_steps, agentic_steps, completion_steps, max_iterations)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                   ON CONFLICT (id) DO NOTHING"#
            };

            match pg_db.pool().get().await {
                Ok(conn) => match conn
                    .execute(
                        sql,
                        &[
                            &id,
                            &name,
                            &desc,
                            &category,
                            &tags,
                            &setup,
                            &verif,
                            &agentic,
                            &completion,
                            &max_iters,
                        ],
                    )
                    .await
                {
                    Ok(n) => {
                        if n > 0 {
                            imported += 1;
                        } else {
                            skipped += 1;
                        }
                    }
                    Err(e) => errors.push(format!("Workflow {}: {}", id, e)),
                },
                Err(e) => errors.push(format!("PG pool: {}", e)),
            }
        }
        total_imported += imported;
        total_skipped += skipped;
        total_errors += errors.len();
        results.insert(
            "unified_workflows".to_string(),
            ImportResult {
                imported,
                skipped,
                errors,
            },
        );
    }

    // Import learning_outcomes via PG upsert
    if opts.learning_outcomes && !data.learning_outcomes.is_empty() {
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut errors = Vec::new();

        for item in &data.learning_outcomes {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let task_id = item
                .get("task_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            if id.is_empty() || task_id.is_empty() {
                skipped += 1;
                continue;
            }

            let duration_secs: Option<f64> = item.get("duration_secs").and_then(|v| v.as_f64());
            let iterations: Option<i32> = item
                .get("iterations")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32);
            let strategy = item
                .get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            let sql = if is_overwrite {
                r#"INSERT INTO learning_outcomes (id, task_id, status, duration_secs, iterations, strategy)
                   VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT (id) DO UPDATE SET task_id = $2, status = $3, duration_secs = $4, iterations = $5, strategy = $6"#
            } else {
                r#"INSERT INTO learning_outcomes (id, task_id, status, duration_secs, iterations, strategy)
                   VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT (id) DO NOTHING"#
            };

            match pg_db.pool().get().await {
                Ok(conn) => match conn
                    .execute(
                        sql,
                        &[
                            &id,
                            &task_id,
                            &status,
                            &duration_secs,
                            &iterations,
                            &strategy,
                        ],
                    )
                    .await
                {
                    Ok(n) => {
                        if n > 0 {
                            imported += 1;
                        } else {
                            skipped += 1;
                        }
                    }
                    Err(e) => errors.push(format!("LO {}: {}", id, e)),
                },
                Err(e) => errors.push(format!("PG pool: {}", e)),
            }
        }
        total_imported += imported;
        total_skipped += skipped;
        total_errors += errors.len();
        results.insert(
            "learning_outcomes".to_string(),
            ImportResult {
                imported,
                skipped,
                errors,
            },
        );
    }

    // Import learning_patterns via PG upsert
    if opts.learning_patterns && !data.learning_patterns.is_empty() {
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut errors = Vec::new();

        for item in &data.learning_patterns {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let pattern_type = item
                .get("pattern_type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let description = item
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let confidence = item
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let occurrences = item
                .get("occurrences")
                .and_then(|v| v.as_i64())
                .unwrap_or(1) as i32;
            let context = item
                .get("context")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if id.is_empty() || pattern_type.is_empty() {
                skipped += 1;
                continue;
            }

            let sql = if is_overwrite {
                r#"INSERT INTO learning_patterns (id, pattern_type, description, confidence, occurrences, context)
                   VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT (id) DO UPDATE SET
                     pattern_type = $2, description = $3, confidence = $4, occurrences = $5, context = $6, updated_at = NOW()"#
            } else {
                r#"INSERT INTO learning_patterns (id, pattern_type, description, confidence, occurrences, context)
                   VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT (id) DO NOTHING"#
            };

            match pg_db.pool().get().await {
                Ok(conn) => match conn
                    .execute(
                        sql,
                        &[
                            &id,
                            &pattern_type,
                            &description,
                            &confidence,
                            &occurrences,
                            &context.as_deref(),
                        ],
                    )
                    .await
                {
                    Ok(n) => {
                        if n > 0 {
                            imported += 1;
                        } else {
                            skipped += 1;
                        }
                    }
                    Err(e) => errors.push(format!("LP {}: {}", id, e)),
                },
                Err(e) => errors.push(format!("PG pool: {}", e)),
            }
        }
        total_imported += imported;
        total_skipped += skipped;
        total_errors += errors.len();
        results.insert(
            "learning_patterns".to_string(),
            ImportResult {
                imported,
                skipped,
                errors,
            },
        );
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
