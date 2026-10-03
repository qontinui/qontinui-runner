//! Settings → General → Sessions: the wind-down executor's `finished_close`
//! switch (plan
//! `2026-10-03-finished-runner-sessions-close-their-window-without-a-drain`,
//! D3).
//!
//! The arm closes the user's windows on their behalf, so its off-switch has to
//! be reachable in the product. The getter reports the SAVED value beside the
//! EFFECTIVE one, because `QONTINUI_FINISHED_SESSION_CLOSE=0` in the runner's
//! spawn environment overrides the setting — and a panel that showed `on`
//! while the machine kill switch held the arm off would be lying to the
//! operator about what the runner does.

use serde_json::json;
use tracing::info;

use super::CommandResponse;
use crate::session::finished_close;
use crate::settings::{self, FinishedSessionClose};

fn payload(saved: FinishedSessionClose) -> serde_json::Value {
    let effective = finished_close::effective_mode(
        saved,
        std::env::var(finished_close::KILL_ENV).ok().as_deref(),
    );
    json!({
        "finished_session_close": saved.as_str(),
        "effective": effective.as_str(),
        "kill_switch_engaged": finished_close::kill_switch_engaged(),
        "kill_switch_env": finished_close::KILL_ENV,
    })
}

/// The saved `finished_session_close` value, the effective mode, and whether
/// the machine kill switch is engaged.
#[tauri::command]
pub fn finished_session_close_get() -> Result<CommandResponse, String> {
    Ok(match settings::get_finished_session_close() {
        Ok(saved) => CommandResponse {
            success: true,
            message: None,
            data: Some(payload(saved)),
        },
        // The arm runs OFF while the file is unreadable; say why rather than
        // render the placeholder default as the operator's choice.
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!(
                "settings.json is unreadable ({e}) — the runner is not closing finished \
                 sessions until it can read the saved value"
            )),
            data: None,
        },
    })
}

/// Save `finished_session_close` (`on` | `shadow` | `off`). Live on the
/// executor's next tick — no restart.
#[tauri::command]
pub fn finished_session_close_set(
    finished_session_close: String,
) -> Result<CommandResponse, String> {
    let mode = FinishedSessionClose::from_wire(&finished_session_close).ok_or_else(|| {
        format!(
            "finished_session_close:invalid: {finished_session_close:?} is not one of \
             on | shadow | off"
        )
    })?;
    settings::save_finished_session_close(mode)?;
    info!(
        mode = mode.as_str(),
        "sessions: finished_session_close saved"
    );
    Ok(CommandResponse {
        success: true,
        message: Some("Saved — live on the wind-down executor's next tick".to_string()),
        data: Some(payload(mode)),
    })
}
