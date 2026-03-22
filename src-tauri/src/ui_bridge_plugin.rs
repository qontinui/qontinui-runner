//! Tauri plugin: `ui-bridge`
//!
//! Exposes IPC commands consumed by the UI Bridge SDK's `IpcLayerExecutor`:
//!   - `plugin:ui-bridge|fs_assert`  – filesystem assertions
//!   - `plugin:ui-bridge|db_assert`  – database (SQLite) assertions
//!
//! These commands run entirely on the Rust side so the React frontend
//! never needs direct access to the filesystem or database.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tauri::{
    plugin::{Builder, TauriPlugin},
    Runtime,
};
use tracing::{info, warn};

use crate::commands::AppState;
use crate::database::types::{Artifact, ArtifactCountQuery, ArtifactQuery};

// ---------------------------------------------------------------------------
// fs_assert
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsAssertResult {
    pub passed: bool,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[tauri::command]
async fn fs_assert(
    path: String,
    check: String,
    expected: Option<serde_json::Value>,
) -> Result<FsAssertResult, String> {
    info!("ui-bridge plugin: fs_assert path={path} check={check}");

    let p = std::path::Path::new(&path);
    let file_exists = p.exists();

    match check.as_str() {
        "exists" => Ok(FsAssertResult {
            passed: file_exists,
            exists: file_exists,
            content: None,
            size: None,
        }),

        "notExists" => Ok(FsAssertResult {
            passed: !file_exists,
            exists: file_exists,
            content: None,
            size: None,
        }),

        "contentContains" => {
            if !file_exists {
                return Ok(FsAssertResult {
                    passed: false,
                    exists: false,
                    content: None,
                    size: None,
                });
            }
            let content =
                std::fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {e}"))?;
            let needle = expected.as_ref().and_then(|v| v.as_str()).unwrap_or("");
            let passed = content.contains(needle);
            Ok(FsAssertResult {
                passed,
                exists: true,
                content: Some(content),
                size: None,
            })
        }

        "contentEquals" => {
            if !file_exists {
                return Ok(FsAssertResult {
                    passed: false,
                    exists: false,
                    content: None,
                    size: None,
                });
            }
            let content =
                std::fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {e}"))?;
            let expected_str = expected.as_ref().and_then(|v| v.as_str()).unwrap_or("");
            let passed = content == expected_str;
            Ok(FsAssertResult {
                passed,
                exists: true,
                content: Some(content),
                size: None,
            })
        }

        "sizeGreaterThan" => {
            if !file_exists {
                return Ok(FsAssertResult {
                    passed: false,
                    exists: false,
                    content: None,
                    size: None,
                });
            }
            let meta = std::fs::metadata(&path)
                .map_err(|e| format!("Failed to get file metadata: {e}"))?;
            let size = meta.len();
            let threshold: u64 = expected.as_ref().and_then(|v| v.as_u64()).unwrap_or(0);
            let passed = size > threshold;
            Ok(FsAssertResult {
                passed,
                exists: true,
                content: None,
                size: Some(size),
            })
        }

        other => Err(format!("Unknown fs_assert check type: {other}")),
    }
}

// ---------------------------------------------------------------------------
// db_assert
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbAssertResult {
    pub passed: bool,
    pub row_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<serde_json::Value>>,
}

#[tauri::command]
async fn db_assert(
    state: tauri::State<'_, Arc<AppState>>,
    query: String,
    connection_ref: Option<String>,
    expected_rows: Option<i64>,
    expected_values: Option<Vec<serde_json::Value>>,
) -> Result<DbAssertResult, String> {
    info!("ui-bridge plugin: db_assert query={query} connection_ref={connection_ref:?}");

    if let Some(ref cref) = connection_ref {
        warn!("db_assert: connection_ref '{cref}' ignored – using runner's checkpoint DB");
    }

    let db = state.checkpoint_db.clone();

    // Run the query on a blocking thread to avoid blocking the async runtime
    tokio::task::spawn_blocking(move || {
        let conn = db.get_conn_string()?;

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare query: {e}"))?;

        let column_count = stmt.column_count();
        let column_names: Vec<String> = (0..column_count)
            .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
            .collect();

        let rows_result: Vec<serde_json::Value> = {
            let mapped = stmt
                .query_map([], |row| {
                    let mut map = serde_json::Map::new();
                    for (i, col_name) in column_names.iter().enumerate() {
                        let val: rusqlite::types::Value = row.get(i)?;
                        let json_val = match val {
                            rusqlite::types::Value::Null => serde_json::Value::Null,
                            rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                            rusqlite::types::Value::Real(f) => serde_json::json!(f),
                            rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
                            rusqlite::types::Value::Blob(b) => {
                                serde_json::Value::String(format!("<blob {} bytes>", b.len()))
                            }
                        };
                        map.insert(col_name.clone(), json_val);
                    }
                    Ok(serde_json::Value::Object(map))
                })
                .map_err(|e| format!("Query execution failed: {e}"))?;
            mapped
                .collect::<Result<Vec<serde_json::Value>, rusqlite::Error>>()
                .map_err(|e| format!("Failed to collect rows: {e}"))?
        };

        let row_count = rows_result.len() as i64;

        // Determine pass/fail
        let mut passed = true;

        if let Some(expected) = expected_rows {
            if row_count != expected {
                passed = false;
            }
        }

        if let Some(ref expected_vals) = expected_values {
            // Compare each expected value object against the actual rows.
            // Each entry in expected_values should match the corresponding row.
            for (i, expected_row) in expected_vals.iter().enumerate() {
                if i >= rows_result.len() {
                    passed = false;
                    break;
                }
                let actual_row = &rows_result[i];
                if let (Some(expected_obj), Some(actual_obj)) =
                    (expected_row.as_object(), actual_row.as_object())
                {
                    for (key, expected_val) in expected_obj {
                        if let Some(actual_val) = actual_obj.get(key.as_str()) {
                            if actual_val != expected_val {
                                passed = false;
                            }
                        } else {
                            passed = false;
                        }
                    }
                } else {
                    // Fall back to direct equality
                    if actual_row != expected_row {
                        passed = false;
                    }
                }
            }
        }

        Ok(DbAssertResult {
            passed,
            row_count,
            rows: Some(rows_result),
        })
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

// ---------------------------------------------------------------------------
// Artifact commands (IpcArtifactStore)
// ---------------------------------------------------------------------------

/// Save a new artifact to the database.
/// Errors if an artifact with the same artifact_id already exists.
#[tauri::command]
async fn save_artifact(
    state: tauri::State<'_, Arc<AppState>>,
    artifact: Artifact,
) -> Result<serde_json::Value, String> {
    info!(
        "ui-bridge plugin: save_artifact id={}",
        artifact.artifact_id
    );

    let db = state.checkpoint_db.clone();
    tokio::task::spawn_blocking(move || {
        db.save_artifact(&artifact)?;
        Ok(serde_json::json!({ "saved": true }))
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

/// Get a single artifact by ID, deserializing JSON fields.
#[tauri::command]
async fn get_artifact(
    state: tauri::State<'_, Arc<AppState>>,
    artifact_id: String,
) -> Result<serde_json::Value, String> {
    info!("ui-bridge plugin: get_artifact id={artifact_id}");

    let db = state.checkpoint_db.clone();
    tokio::task::spawn_blocking(move || {
        let artifact = db.get_artifact(&artifact_id)?;
        match artifact {
            Some(a) => {
                serde_json::to_value(&a).map_err(|e| format!("Failed to serialize artifact: {e}"))
            }
            None => Err(format!("Artifact '{artifact_id}' not found")),
        }
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

/// Query artifacts with optional filters (specId, dateRange, passedOnly/failedOnly, limit/offset).
/// Results are ordered by created_at DESC.
#[tauri::command]
async fn query_artifacts(
    state: tauri::State<'_, Arc<AppState>>,
    query: ArtifactQuery,
) -> Result<serde_json::Value, String> {
    info!("ui-bridge plugin: query_artifacts");

    let db = state.checkpoint_db.clone();
    tokio::task::spawn_blocking(move || {
        let artifacts = db.query_artifacts(&query)?;
        serde_json::to_value(&artifacts).map_err(|e| format!("Failed to serialize artifacts: {e}"))
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

/// Verify an artifact's integrity by recomputing the SHA-256 hash of the canonical
/// JSON payload (result + source + environment) and comparing to the artifact_id.
#[tauri::command]
async fn verify_artifact(
    state: tauri::State<'_, Arc<AppState>>,
    artifact_id: String,
) -> Result<serde_json::Value, String> {
    info!("ui-bridge plugin: verify_artifact id={artifact_id}");

    let db = state.checkpoint_db.clone();
    tokio::task::spawn_blocking(move || {
        let artifact = db.get_artifact(&artifact_id)?;
        match artifact {
            Some(a) => {
                // Build canonical JSON in a deterministic order: result, source, environment
                let canonical = serde_json::json!({
                    "result": serde_json::from_str::<serde_json::Value>(&a.result_json)
                        .unwrap_or(serde_json::Value::Null),
                    "source": serde_json::from_str::<serde_json::Value>(&a.source_json)
                        .unwrap_or(serde_json::Value::Null),
                    "environment": serde_json::from_str::<serde_json::Value>(&a.environment_json)
                        .unwrap_or(serde_json::Value::Null),
                });

                let canonical_str = serde_json::to_string(&canonical)
                    .map_err(|e| format!("Failed to serialize canonical JSON: {e}"))?;

                let mut hasher = Sha256::new();
                hasher.update(canonical_str.as_bytes());
                let hash = hex::encode(hasher.finalize());

                let valid = hash == artifact_id;

                Ok(serde_json::json!({
                    "valid": valid,
                    "computedHash": hash,
                    "artifactId": artifact_id,
                }))
            }
            None => Err(format!("Artifact '{artifact_id}' not found")),
        }
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

/// Count artifacts with optional filters.
#[tauri::command]
async fn count_artifacts(
    state: tauri::State<'_, Arc<AppState>>,
    query: Option<ArtifactCountQuery>,
) -> Result<serde_json::Value, String> {
    info!("ui-bridge plugin: count_artifacts");

    let db = state.checkpoint_db.clone();
    tokio::task::spawn_blocking(move || {
        let q = query.unwrap_or(ArtifactCountQuery {
            spec_id: None,
            date_from: None,
            date_to: None,
            passed_only: None,
            failed_only: None,
        });
        let count = db.count_artifacts(&q)?;
        Ok(serde_json::json!({ "count": count }))
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

// ---------------------------------------------------------------------------
// Plugin initializer
// ---------------------------------------------------------------------------

/// Build the `ui-bridge` Tauri plugin.
///
/// Register with: `.plugin(ui_bridge_plugin::init())`
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("ui-bridge")
        .invoke_handler(tauri::generate_handler![
            fs_assert,
            db_assert,
            save_artifact,
            get_artifact,
            query_artifacts,
            verify_artifact,
            count_artifacts,
        ])
        .build()
}
