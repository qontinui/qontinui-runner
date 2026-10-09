//! Tauri commands for per-config chunk label CRUD.
//!
//! Chunk labels let the user override the auto-derived chunk name shown in
//! the chunked state-machine graph view. A chunk_id is a stable djb2 hash
//! of a chunk's sorted state ids (see `chunkStateMachine` in
//! `@qontinui/workflow-utils`), so labels survive input re-orderings.

use tauri::State;

use crate::commands::compartments::StorageCompartment;
use crate::database::pg::chunk_labels::ChunkLabel;

/// List all user-chosen chunk labels for the given config.
#[tauri::command]
pub async fn list_chunk_labels(
    storage: State<'_, StorageCompartment>,
    config_id: String,
) -> Result<Vec<ChunkLabel>, String> {
    storage.pg_db().list_chunk_labels(&config_id).await
}

/// Upsert a chunk label for (config_id, chunk_id).
#[tauri::command]
pub async fn upsert_chunk_label(
    storage: State<'_, StorageCompartment>,
    config_id: String,
    chunk_id: String,
    label: String,
) -> Result<(), String> {
    storage
        .pg_db()
        .upsert_chunk_label(&config_id, &chunk_id, &label)
        .await
}

/// Delete a chunk label (revert to the auto-derived name).
#[tauri::command]
pub async fn delete_chunk_label(
    storage: State<'_, StorageCompartment>,
    config_id: String,
    chunk_id: String,
) -> Result<(), String> {
    storage
        .pg_db()
        .delete_chunk_label(&config_id, &chunk_id)
        .await
}

// Tauri commands this module owns — the ONLY registration site (see
// `crate::ipc_registry`). A `#[tauri::command]` fn missing here is
// unreachable from the frontend.
crate::ipc_group!(delete_chunk_label, list_chunk_labels, upsert_chunk_label,);
