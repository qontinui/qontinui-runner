//! Tauri commands for the session-failure surface (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 7).
//!
//! The frontend's `SessionFailureBanner` reads a terminal's active failures
//! here at mount (the `session-failure` event carries every later change), and
//! the resume verifier — which runs in the webview, where the handshake is
//! watched — reports its outcome here instead of owning a failure state of its
//! own. A headless consumer reads the same store at
//! `GET /terminals/{id}/failures`.

use qontinui_types::cli_session::SessionFailure;
use tauri::Manager;

use crate::session::failure::FailureSignal;
use crate::session::failure_recovery::{self, Evidence, Lane};
use crate::session::session_lifecycle_store::SessionLifecycleStore;

/// Every failure currently active for terminal `terminal_id`.
#[tauri::command]
pub fn terminal_failures(terminal_id: String) -> Vec<SessionFailure> {
    failure_recovery::active(&terminal_id)
}

/// The resume verifier typed a resume into `terminal_id` and the CLI's
/// handshake never appeared. Classified as a `resume_failed` hint through the
/// one classifier and recorded; the retry stays the verifier's (the kind's
/// policy is `never`). `provider` is the tab's provider when it knows one;
/// otherwise the terminal's lifecycle record names it.
#[tauri::command]
pub fn terminal_report_resume_failure(
    app: tauri::AppHandle,
    terminal_id: String,
    provider: Option<String>,
) -> Option<SessionFailure> {
    let provider = provider.filter(|p| !p.is_empty()).or_else(|| {
        app.try_state::<std::sync::Arc<SessionLifecycleStore>>()
            .and_then(|store| store.find_open_by_terminal(&terminal_id))
            .map(|record| record.provider)
    });
    // A provider nobody can name is recorded as such, never guessed.
    let provider = provider.unwrap_or_else(|| "unknown".to_string());
    failure_recovery::record_only(
        &terminal_id,
        Lane::Pty,
        &provider,
        &FailureSignal::HandshakeTimeout,
    )
}

/// The resume verifier saw `terminal_id`'s handshake: the conversation is
/// back, which is the evidence that ends a `resume_failed` failure.
#[tauri::command]
pub fn terminal_report_resume_verified(terminal_id: String) {
    failure_recovery::clear_on_evidence(&terminal_id, Evidence::HandshakeVerified);
}

/// The operator acknowledged failure `failure_id` on `terminal_id`. Returns
/// whether it was active.
#[tauri::command]
pub fn terminal_failure_dismiss(terminal_id: String, failure_id: String) -> bool {
    failure_recovery::dismiss(&terminal_id, &failure_id)
}
