//! Tauri commands for visual regression baseline CRUD.
//!
//! Provides persistent baseline storage backed by PostgreSQL, replacing the
//! in-memory `InMemoryBaselineStore` that loses data on reload.

use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::info;

use crate::commands::AppState;
use crate::database::pg::ui_bridge_baselines::{Baseline, BaselineMeta};

// =============================================================================
// Response types (base64-encoded PNG for transport over IPC)
// =============================================================================

#[derive(serde::Serialize)]
pub struct BaselineResponse {
    pub id: String,
    pub target_scope: String,
    pub fingerprint: Option<String>,
    pub png_base64: String,
    pub width: i32,
    pub height: i32,
    pub created_at: String,
    pub updated_at: String,
    pub metadata_json: Option<String>,
    pub ttl_days: Option<i32>,
}

impl From<Baseline> for BaselineResponse {
    fn from(b: Baseline) -> Self {
        use base64::Engine;
        Self {
            id: b.id,
            target_scope: b.target_scope,
            fingerprint: b.fingerprint,
            png_base64: base64::engine::general_purpose::STANDARD.encode(&b.png_bytes),
            width: b.width,
            height: b.height,
            created_at: b.created_at,
            updated_at: b.updated_at,
            metadata_json: b.metadata_json,
            ttl_days: b.ttl_days,
        }
    }
}

// =============================================================================
// Commands
// =============================================================================

/// Save (upsert) a visual regression baseline.
///
/// Accepts the PNG as base64. Width and height are decoded from the image
/// authoritatively — any caller-supplied dimensions are ignored.
#[tauri::command]
pub async fn sm_save_baseline(
    id: String,
    target_scope: String,
    png_base64: String,
    fingerprint: Option<String>,
    metadata_json: Option<String>,
    ttl_days: Option<i32>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<BaselineResponse, String> {
    use base64::Engine;

    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(&png_base64)
        .map_err(|e| format!("Failed to decode baseline base64: {}", e))?;

    let baseline = app_state
        .pg_db
        .baseline_save(
            &id,
            &target_scope,
            &png_bytes,
            fingerprint.as_deref(),
            metadata_json.as_deref(),
            ttl_days,
        )
        .await?;

    info!(
        "Saved baseline id={} scope={} {}x{}",
        baseline.id, baseline.target_scope, baseline.width, baseline.height
    );

    Ok(BaselineResponse::from(baseline))
}

/// Get a single baseline by ID, returning the full PNG as base64.
#[tauri::command]
pub async fn sm_get_baseline(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Option<BaselineResponse>, String> {
    let baseline = app_state.pg_db.baseline_get(&id).await?;
    Ok(baseline.map(BaselineResponse::from))
}

/// List baseline metadata with an optional target_scope filter.
/// Does NOT return PNG bytes — use `sm_get_baseline` for the full image.
#[tauri::command]
pub async fn sm_list_baselines(
    target_scope: Option<String>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<BaselineMeta>, String> {
    app_state.pg_db.baseline_list(target_scope.as_deref()).await
}

/// Delete a baseline by ID.
#[tauri::command]
pub async fn sm_delete_baseline(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let deleted = app_state.pg_db.baseline_delete(&id).await?;
    if deleted {
        info!("Deleted baseline id={}", id);
    }
    Ok(deleted)
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_ui_bridge_baselines")
        .invoke_handler(tauri::generate_handler![
            sm_save_baseline,
            sm_get_baseline,
            sm_list_baselines,
            sm_delete_baseline,
        ])
        .build()
}
