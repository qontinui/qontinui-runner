//! `/sessions/<id>/snapshots` and `/sessions/<id>/rewind` HTTP endpoints
//! — Phase 4 of the productivity stack. Backs the `/rewind-session` slash
//! command (see `D:/qontinui-root/.claude/commands/rewind-session.md`).
//!
//! `GET /sessions/<id>/snapshots` lists every `session_file_snapshots`
//! row for the given session — exposed so an external caller can inspect
//! the rollback set before triggering a restore.
//!
//! `POST /sessions/<id>/rewind` performs the actual restore: for each
//! pre-edit snapshot, verify the on-disk blob's sha256 matches the
//! recorded `blob_sha256`, then copy the blob over the original
//! `file_path`. This avoids the slash command needing to orchestrate
//! `cp` calls inside the LLM tool-call context.

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

use crate::database::pg::session_file_snapshots::SnapshotRow;
use crate::mcp::types::ApiState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSnapshotsResponse {
    pub session_id: String,
    pub snapshots: Vec<SnapshotRow>,
}

async fn get_snapshots_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<GetSnapshotsResponse>, (StatusCode, String)> {
    let snapshots = state
        .app_state
        .pg_db
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(GetSnapshotsResponse {
        session_id,
        snapshots,
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RewindSessionRequest {
    /// Reserved for future use (e.g. dry-run, scope-by-path). The body
    /// is currently empty `{}` per the slash command's contract; we
    /// accept extra fields liberally.
    #[serde(default)]
    pub _placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindError {
    pub file_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindSessionResponse {
    pub session_id: String,
    pub files_restored: usize,
    pub files_skipped: usize,
    pub errors: Vec<RewindError>,
}

/// Compute the sha256 of `path`'s contents as a lowercase hex string.
/// Returns `None` if the file cannot be read.
fn sha256_of_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(format!("{:x}", hasher.finalize()))
}

async fn rewind_session_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
    body: Option<Json<RewindSessionRequest>>,
) -> Result<Json<RewindSessionResponse>, (StatusCode, String)> {
    let _ = body; // body fields are reserved for future use

    let snapshots = state
        .app_state
        .pg_db
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut files_restored = 0usize;
    let mut files_skipped = 0usize;
    let mut errors: Vec<RewindError> = Vec::new();

    // Only the FIRST captured-before snapshot per file_path is the
    // rollback target. Subsequent rows are informational. Walk in
    // taken_at ASC order (the SELECT already does this) and skip a
    // file_path once we've handled it.
    let mut handled: std::collections::HashSet<String> = std::collections::HashSet::new();

    for snap in &snapshots {
        if !snap.captured_before {
            continue;
        }
        if !handled.insert(snap.file_path.clone()) {
            // already restored from the first snapshot for this path
            files_skipped += 1;
            continue;
        }

        let blob_path = Path::new(&snap.snapshot_blob_path);
        if !blob_path.exists() {
            errors.push(RewindError {
                file_path: snap.file_path.clone(),
                reason: format!("blob missing: {}", snap.snapshot_blob_path),
            });
            continue;
        }

        let actual_sha = match sha256_of_file(blob_path) {
            Some(h) => h,
            None => {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("blob unreadable: {}", snap.snapshot_blob_path),
                });
                continue;
            }
        };
        if actual_sha != snap.blob_sha256 {
            errors.push(RewindError {
                file_path: snap.file_path.clone(),
                reason: format!(
                    "blob sha256 mismatch: stored={}, actual={}",
                    snap.blob_sha256, actual_sha
                ),
            });
            continue;
        }

        // Ensure the destination's parent dir exists (the file may
        // have been deleted by the failed worker).
        if let Some(parent) = Path::new(&snap.file_path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("create parent dir failed: {}", e),
                });
                continue;
            }
        }

        match std::fs::copy(blob_path, Path::new(&snap.file_path)) {
            Ok(_) => {
                files_restored += 1;
                info!(
                    "rewind_session: restored {} from blob {} (session={})",
                    snap.file_path, snap.snapshot_blob_path, session_id
                );
            }
            Err(e) => {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("copy failed: {}", e),
                });
            }
        }
    }

    if !errors.is_empty() {
        warn!(
            "rewind_session: {} restored, {} errored, session={}",
            files_restored,
            errors.len(),
            session_id
        );
    }

    Ok(Json(RewindSessionResponse {
        session_id,
        files_restored,
        files_skipped,
        errors,
    }))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Avoid the v0.7-syntax false positive on `{name}` captures, mirroring
        // the workaround in `mcp/ai_session.rs::routes`.
        .without_v07_checks()
        .route(
            "/sessions/{session_id}/snapshots",
            get(get_snapshots_handler),
        )
        .route(
            "/sessions/{session_id}/rewind",
            post(rewind_session_handler),
        )
}
