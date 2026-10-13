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

use crate::session::session_org_store::{
    SessionOrgApplyResponse, SessionOrgOp, SessionOrgSnapshot, SessionOrgStore,
};

/// The whole organization, or `status: "unknown"` with the reason and where
/// an unreadable file was preserved.
#[tauri::command]
pub fn session_org_get(store: tauri::State<'_, Arc<SessionOrgStore>>) -> SessionOrgSnapshot {
    store.snapshot()
}

/// Apply `ops` in order; returns one result per op and the resulting state.
#[tauri::command]
pub fn session_org_apply(
    store: tauri::State<'_, Arc<SessionOrgStore>>,
    ops: Vec<SessionOrgOp>,
) -> Result<SessionOrgApplyResponse, String> {
    store.apply(&ops)
}
