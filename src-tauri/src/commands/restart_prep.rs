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
//! * `restart_prep_dismiss` (Phase 6), and `restart_prep_extend` and
//!   `restart_prep_set_held` (Phase 5, the panel's Extend and held toggle),
//!   follow the same convention.
//! * `restart_prep_current` (Phase 5) — the record and view from this
//!   process's memory, the payload `restart-prep-changed` carries. Read-only.
//!
//! What "off the invoke allowlist" does and does not mean here: no local
//! process can call these commands by name. But the Terminal page's
//! restart-prep panel registers UI-Bridge component actions that call them
//! (prepare, cancel, extend, dismiss, the held toggle), and the bridge serves
//! those to any local caller — deliberately: the plan names their ids and
//! drives them through the bridge. They are declared `destructive` (the held
//! toggle `write`); see route 3 in `commands/operator_doors.rs`.

/// `restart_prep_status` — the persisted prepare record (if any) and the live
/// cohort a prepare taken now would pin.
#[tauri::command]
pub async fn restart_prep_status(
    app: tauri::AppHandle,
) -> Result<crate::restart_prep::RestartPrepStatus, String> {
    crate::restart_prep::status_from_app_blocking(app).await
}

/// `restart_prep_current` — the persisted prepare record and its view, as
/// this process holds them (the payload `restart-prep-changed` carries). What
/// the panel reads: unlike `restart_prep_status` it takes no live cohort
/// capture. Read-only.
#[tauri::command]
pub async fn restart_prep_current(
) -> Result<Option<crate::restart_prep::prepare::PrepChanged>, String> {
    crate::restart_prep::prepare::current_from_app()
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

/// `restart_prep_extend` — the panel's Extend: push the spawn hold's `until`
/// out by 6 h (D1), re-sent to coord as the same hold. Off the invoke
/// allowlist like the doors above (see the module doc for the panel route).
#[tauri::command]
pub async fn restart_prep_extend() -> Result<crate::restart_prep::prepare::PrepChanged, String> {
    crate::restart_prep::prepare::extend_from_app().await
}

/// `restart_prep_dismiss` — end a rebuilt runner's restore of the cohort
/// `prepare_id` (Phase 6): the record moves to `done`, so its waiting rows are
/// no longer placeholders nor held from the orphan close, and their
/// restore-pending markers are cleared here — the one dismiss every surface
/// (the panel, the strip) goes through. Off the invoke allowlist like the
/// doors above (see the module doc for the panel route).
#[tauri::command]
pub async fn restart_prep_dismiss(
    store: tauri::State<
        '_,
        std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>,
    >,
    prepare_id: String,
) -> Result<crate::restart_prep::PrepareRecord, String> {
    crate::restart_prep::restore::dismiss(&prepare_id, &|id| store.clear_restore_pending(id))
}

/// `restart_prep_set_held` — the panel's held toggle (D6): hold a cohort row
/// for a hand restore, or release it to the bulk restores. Off the invoke
/// allowlist like the doors above: it decides which sessions an operator's
/// "Restore all" brings back.
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
    restart_prep_current,
    restart_prep_prepare,
    restart_prep_cancel,
    restart_prep_extend,
    restart_prep_dismiss,
    restart_prep_set_held
);
