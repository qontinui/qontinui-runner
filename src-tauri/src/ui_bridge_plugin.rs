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

    // db_assert: SQLite-based assertions removed. Return an error indicating
    // this feature needs PG migration.
    let _ = (state, expected_rows, expected_values);
    Err(format!(
        "db_assert not supported: SQLite checkpoint DB removed. Query: {}",
        query
    ))
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
    state
        .pg_db
        .save_artifact(&artifact)
        .await
        .map_err(|e| format!("Failed to save artifact: {e}"))?;
    Ok(serde_json::json!({ "saved": true }))
}

/// Get a single artifact by ID, deserializing JSON fields.
#[tauri::command]
async fn get_artifact(
    state: tauri::State<'_, Arc<AppState>>,
    artifact_id: String,
) -> Result<serde_json::Value, String> {
    info!("ui-bridge plugin: get_artifact id={artifact_id}");
    let artifact = state
        .pg_db
        .get_artifact(&artifact_id)
        .await
        .map_err(|e| format!("Failed to get artifact: {e}"))?;
    match artifact {
        Some(a) => {
            serde_json::to_value(&a).map_err(|e| format!("Failed to serialize artifact: {e}"))
        }
        None => Err(format!("Artifact '{artifact_id}' not found")),
    }
}

/// Query artifacts with optional filters (specId, dateRange, passedOnly/failedOnly, limit/offset).
/// Results are ordered by created_at DESC.
#[tauri::command]
async fn query_artifacts(
    state: tauri::State<'_, Arc<AppState>>,
    query: ArtifactQuery,
) -> Result<serde_json::Value, String> {
    info!("ui-bridge plugin: query_artifacts");
    let artifacts = state
        .pg_db
        .query_artifacts(&query)
        .await
        .map_err(|e| format!("Failed to query artifacts: {e}"))?;
    serde_json::to_value(&artifacts).map_err(|e| format!("Failed to serialize artifacts: {e}"))
}

/// Verify an artifact's integrity by recomputing the SHA-256 hash of the canonical
/// JSON payload (result + source + environment) and comparing to the artifact_id.
#[tauri::command]
async fn verify_artifact(
    state: tauri::State<'_, Arc<AppState>>,
    artifact_id: String,
) -> Result<serde_json::Value, String> {
    info!("ui-bridge plugin: verify_artifact id={artifact_id}");
    let artifact = state
        .pg_db
        .get_artifact(&artifact_id)
        .await
        .map_err(|e| format!("Failed to get artifact: {e}"))?;
    match artifact {
        Some(a) => {
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
}

/// Count artifacts with optional filters.
#[tauri::command]
async fn count_artifacts(
    state: tauri::State<'_, Arc<AppState>>,
    query: Option<ArtifactCountQuery>,
) -> Result<serde_json::Value, String> {
    info!("ui-bridge plugin: count_artifacts");
    let q = query.unwrap_or(ArtifactCountQuery {
        spec_id: None,
        date_from: None,
        date_to: None,
        passed_only: None,
        failed_only: None,
    });
    let count = state
        .pg_db
        .count_artifacts(&q)
        .await
        .map_err(|e| format!("Failed to count artifacts: {e}"))?;
    Ok(serde_json::json!({ "count": count }))
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
