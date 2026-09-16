//! Tauri commands behind the in-session deconflict advisory banner
//! (`src/components/terminal/DeconflictAdvisoryBanner.tsx`). The banner
//! listens for the `coordinator-decision-created` event the
//! `crate::deconflict` loop emits and dismisses an advisory through
//! `resolve_escalation`, which is the same `project.coordinator_decisions`
//! row update the Coordinator dashboard's Escalations panel used.
//!
//! Rehomed from `commands::productivity` by Phase 3 of
//! `2026-09-12-consolidate-local-orchestration-onto-conductor`; the command
//! name is unchanged so the banner needs no edit.

use crate::commands::require_app_state;

/// Mark an escalation resolved with a free-form `resolution` note. Returns
/// `true` if a row was updated (i.e. the decision existed and wasn't
/// already resolved).
#[tauri::command]
pub async fn resolve_escalation(
    app_handle: tauri::AppHandle,
    decision_id: String,
    resolution: String,
) -> Result<bool, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state
        .pg_db
        .resolve_coordinator_decision(&decision_id, &resolution)
        .await
}
