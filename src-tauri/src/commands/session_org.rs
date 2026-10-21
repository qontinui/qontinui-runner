//! Tauri commands for the Session Manager organization store (plan
//! `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
//! Phase 2).
//!
//! Thin shells over [`SessionOrgStore`]: every decision lives in
//! `session::session_org_store`, and the loopback HTTP twins
//! (`GET/POST /terminal/session-org`, `mcp::session_org`) call the same two
//! methods, so the window and an agent cannot see different organizations.
//! The store itself emits [`SESSION_ORG_CHANGED_EVENT`] after every apply that
//! changed something, whichever door the apply came through.
//!
//! [`SESSION_ORG_CHANGED_EVENT`]: crate::session::session_org_store::SESSION_ORG_CHANGED_EVENT

use std::sync::Arc;

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

use crate::session::session_org_store::{
    SessionOrgApplyResponse, SessionOrgOp, SessionOrgSnapshot, SessionOrgStatus, SessionOrgStore,
};

/// The whole organization, or `status: "unknown"` with the reason and where
/// an unreadable file was preserved.
///
/// `async` + blocking pool: the store lock is held across an apply's file
/// write, so even this read must not wait for it on the main thread. Never
/// rejects — a failed task answers `unknown` with its reason, the same shape
/// the frontend already renders.
#[tauri::command]
pub async fn session_org_get(
    store: tauri::State<'_, Arc<SessionOrgStore>>,
) -> Result<SessionOrgSnapshot, String> {
    let store = Arc::clone(store.inner());
    Ok(spawn_blocking_tracked(move || store.snapshot())
        .await
        .unwrap_or_else(|e| SessionOrgSnapshot {
            status: SessionOrgStatus::Unknown,
            org: None,
            reason: Some(format!("session_org_get task failed: {e}")),
            preserved_at: None,
        }))
}

/// Apply `ops` in order; returns one result per op and the resulting state.
/// The apply persists the org to disk, so it runs on the blocking pool, never
/// on the main thread.
#[tauri::command]
pub async fn session_org_apply(
    store: tauri::State<'_, Arc<SessionOrgStore>>,
    ops: Vec<SessionOrgOp>,
) -> Result<SessionOrgApplyResponse, String> {
    let store = Arc::clone(store.inner());
    spawn_blocking_tracked(move || store.apply(&ops))
        .await
        .map_err(|e| format!("session_org_apply task failed: {e}"))?
}
