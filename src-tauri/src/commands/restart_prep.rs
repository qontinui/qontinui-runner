//! Tauri doors of the operator restart prepare (plan
//! `2026-10-21-operator-prepare-for-restart-and-progressive-restore`).
//!
//! * `restart_prep_status` (Phase 1) — a thin twin of
//!   `GET /control/restart-prep`: both call
//!   [`crate::restart_prep::status_from_app_blocking`], so the webview and an
//!   HTTP reader cannot disagree. Read-only.
//! * `restart_prep_prepare` / `restart_prep_cancel` (Phase 3) — the operator's
//!   own actions: take the spawn hold and freeze account moves, or let both
//!   go. Each state change is also announced as the `restart-prep-changed`
//!   event. These WRITE (a coord drain on the primary), so they follow the
//!   operator-door convention (`commands/operator_doors.rs` header): Tauri
//!   only, with no HTTP twin, and OFF the UI-Bridge invoke allowlist — an
//!   allowlisted command is reachable by any local process over
//!   `POST /ui-bridge/invoke/<name>`.
//! * `restart_prep_dismiss` (Phase 6) and `restart_prep_set_held` (Phase 5,
//!   the panel's held toggle) are operator doors of the same kind.

/// `restart_prep_status` — the persisted prepare record (if any) and the live
/// cohort a prepare taken now would pin.
#[tauri::command]
pub async fn restart_prep_status(
    app: tauri::AppHandle,
) -> Result<crate::restart_prep::RestartPrepStatus, String> {
    crate::restart_prep::status_from_app_blocking(app).await
}

/// `restart_prep_prepare` — prepare this runner for a restart. Idempotent
/// while a prepare is active (it returns that prepare).
#[tauri::command]
pub async fn restart_prep_prepare(
    app: tauri::AppHandle,
    reason: Option<String>,
) -> Result<crate::restart_prep::prepare::PrepChanged, String> {
    crate::restart_prep::prepare::prepare_from_app(app, reason.unwrap_or_default()).await
}

/// `restart_prep_cancel` — cancel the prepare (and the same-build banner's
/// Release hold): release the hold, report coord's outcome as given.
#[tauri::command]
pub async fn restart_prep_cancel() -> Result<crate::restart_prep::prepare::PrepChanged, String> {
    crate::restart_prep::prepare::cancel_from_app().await
}

/// `restart_prep_dismiss` — end a rebuilt runner's restore of the cohort
/// `prepare_id` (Phase 6): the record moves to `done`, so its waiting rows are
/// no longer placeholders nor held from the orphan close. Operator-only, like
/// the doors above.
#[tauri::command]
pub async fn restart_prep_dismiss(
    prepare_id: String,
) -> Result<crate::restart_prep::PrepareRecord, String> {
    crate::restart_prep::restore::dismiss(&prepare_id)
}

/// `restart_prep_set_held` — the panel's held toggle (D6): hold a cohort row
/// for a hand restore, or release it to the bulk restores. Operator-only, like
/// the doors above: it decides which sessions an operator's "Restore all"
/// brings back.
#[tauri::command]
pub async fn restart_prep_set_held(
    prepare_id: String,
    claude_session_id: String,
    held: bool,
) -> Result<crate::restart_prep::PrepareRecord, String> {
    crate::restart_prep::restore::set_held(&prepare_id, &claude_session_id, held).await
}

crate::ipc_group!(
    restart_prep_status,
    restart_prep_prepare,
    restart_prep_cancel,
    restart_prep_dismiss,
    restart_prep_set_held
);
