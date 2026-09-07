//! Remote session tabs — the operator-facing commands (plan
//! `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 3c).
//!
//! - `remote_attach_preference_get` / `remote_attach_preference_set` — the
//!   `accept_remote_attach` preference (`same_user` | `tenant` | `off`),
//!   persisted in runner settings and mirrored to coord's device row with
//!   `PUT /coord/devices/me/attach-preference` on every change and once per
//!   relay connect (best-effort, logged). The preference is the OFF switch;
//!   the per-attach grant is the safeguard — they are separate mechanisms.
//! - `terminal_attach_remote {device_id, session_id}` — the SOURCE-role attach
//!   flow: mint a grant from coord with the device JWT, present it through the
//!   backend socket, wait for the target's ring, and open an ordinary
//!   `TerminalSession` around a [`RemotePaneIo`]. The returned `TerminalInfo`
//!   is what the frontend opens as a tab — no new terminal backend.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Manager;
use tracing::{info, warn};

use super::CommandResponse;
use crate::mcp::remote_terminal::{client, ATTACH_TIMEOUT};
use crate::session::SessionRegistry;
use crate::settings::AcceptRemoteAttach;
use crate::terminal::pane_io::PaneIo;
use crate::terminal::remote_pane_io::{RemotePaneIo, ERROR_EXIT_CODE};
use crate::terminal::types::TerminalInfo;
use crate::terminal::TerminalManager;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// The coord HTTP base this runner talks to: the session registry's resolved
/// one when the registry is up, else the profile policy's.
pub fn coord_base_for(app: &tauri::AppHandle) -> String {
    app.try_state::<Arc<SessionRegistry>>()
        .map(|r| r.coord_sync().coord_url().trim_end_matches('/').to_string())
        .unwrap_or_else(|| {
            qontinui_runner_lib::profiles::coord_base_with_source()
                .0
                .trim_end_matches('/')
                .to_string()
        })
}

/// `PUT /coord/devices/me/attach-preference`. `Err` carries the reason; the
/// local setting is already saved by the time this runs, so a failure here
/// means coord's mint may read a stale value until the next mirror.
pub async fn mirror_attach_preference_to_coord(
    coord_base: &str,
    pref: AcceptRemoteAttach,
) -> Result<(), String> {
    let Some(http) = crate::coord_http::coord_client() else {
        return Err("coord HTTP client unavailable".to_string());
    };
    let url = format!(
        "{}/coord/devices/me/attach-preference",
        coord_base.trim_end_matches('/')
    );
    let resp = crate::coord_http::coord_put(http, &url)
        .timeout(Duration::from_secs(10))
        .json(&json!({ "accept_remote_attach": pref.as_str() }))
        .send()
        .await
        .map_err(|e| format!("PUT {url}: {e}"))?;
    let status = resp.status();
    if status.is_success() {
        info!(
            preference = pref.as_str(),
            "remote attach: preference mirrored to coord"
        );
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    Err(format!(
        "PUT {url} answered {}: {}",
        status.as_u16(),
        body.chars().take(300).collect::<String>()
    ))
}

/// The connect-time mirror: read the saved preference and push it, logging
/// rather than raising. Called by the backend relay on its `connected` ack.
pub async fn mirror_attach_preference_logged(coord_base: String) {
    let pref = crate::settings::get_remote_attach_preference();
    if let Err(e) = mirror_attach_preference_to_coord(&coord_base, pref).await {
        warn!(
            error = %e,
            "remote attach: preference mirror to coord failed (best-effort; retried on the next relay connect)"
        );
    }
}

/// Read the `accept_remote_attach` preference.
#[tauri::command]
pub fn remote_attach_preference_get() -> Result<CommandResponse, String> {
    let pref = crate::settings::get_remote_attach_preference();
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(json!({ "accept_remote_attach": pref.as_str() })),
    })
}

/// Save the `accept_remote_attach` preference and mirror it to coord.
#[tauri::command]
pub async fn remote_attach_preference_set(
    app_handle: tauri::AppHandle,
    accept_remote_attach: String,
) -> Result<CommandResponse, String> {
    let pref = AcceptRemoteAttach::from_wire(&accept_remote_attach).ok_or_else(|| {
        format!(
            "remote_attach:invalid_preference: {accept_remote_attach:?} is not one of \
             same_user | tenant | off"
        )
    })?;
    crate::settings::save_remote_attach_preference(pref)?;
    info!(
        preference = pref.as_str(),
        "remote attach: preference saved"
    );
    let base = coord_base_for(&app_handle);
    let mirrored = mirror_attach_preference_to_coord(&base, pref).await;
    if let Err(e) = &mirrored {
        warn!(error = %e, "remote attach: preference mirror to coord failed after save");
    }
    Ok(CommandResponse {
        success: true,
        message: Some(
            match &mirrored {
                Ok(()) => "Remote-attach preference saved and mirrored to coord",
                Err(_) => {
                    "Remote-attach preference saved locally; the coord mirror failed and is \
                     retried on the next relay connect"
                }
            }
            .to_string(),
        ),
        data: Some(json!({
            "accept_remote_attach": pref.as_str(),
            "mirrored": mirrored.is_ok(),
            "mirror_error": mirrored.err(),
        })),
    })
}

/// Coord's `201` from `POST /coord/sessions/{id}/attach-grants`.
#[derive(Debug, Clone, Deserialize)]
struct AttachGrantResponse {
    grant: String,
    grant_jti: String,
    #[serde(default)]
    target_device_id: Option<String>,
    #[serde(default)]
    expires_at: Option<Value>,
}

async fn mint_attach_grant(
    coord_base: &str,
    session_id: uuid::Uuid,
) -> Result<AttachGrantResponse, String> {
    let Some(http) = crate::coord_http::coord_client() else {
        return Err(
            "remote_attach:coord_client_unavailable: shared coord HTTP client failed to build"
                .to_string(),
        );
    };
    let url = format!(
        "{}/coord/sessions/{}/attach-grants",
        coord_base.trim_end_matches('/'),
        session_id
    );
    let resp = crate::coord_http::coord_post(http, &url)
        .timeout(Duration::from_secs(15))
        .json(&json!({ "terminal_id": null }))
        .send()
        .await
        .map_err(|e| format!("remote_attach:coord_unreachable: POST {url}: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // Coord answers typed bodies: {"error": "...", "reason": "..."}.
        let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let code = parsed
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("coord_error");
        let reason = parsed
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|r| format!(":{r}"))
            .unwrap_or_default();
        return Err(format!(
            "remote_attach:{code}{reason}: coord answered {} for POST {url}",
            status.as_u16()
        ));
    }
    serde_json::from_str(&body)
        .map_err(|e| format!("remote_attach:coord_parse: could not decode the grant response: {e}"))
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn non_blank(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Open a tab onto a session running on another device in the tenant.
///
/// `device_id` / `session_id` are the picker row (`fleet_sessions_list`);
/// `device_label`, `session_label` and `working_dir` are the row's display
/// values, used only for the tab title and the info's working dir — the
/// authority for WHERE the session lives is coord's answer to the mint, and
/// a disagreement with `device_id` is refused rather than followed.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn terminal_attach_remote(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    app_handle: tauri::AppHandle,
    device_id: String,
    session_id: String,
    cols: Option<u16>,
    rows: Option<u16>,
    page_id: Option<String>,
    device_label: Option<String>,
    session_label: Option<String>,
    working_dir: Option<String>,
) -> Result<TerminalInfo, String> {
    let session_uuid = uuid::Uuid::parse_str(session_id.trim()).map_err(|e| {
        format!("remote_attach:invalid_session_id: {session_id:?} is not a session uuid: {e}")
    })?;
    let cols = cols.unwrap_or(120);
    let rows = rows.unwrap_or(30);

    let base = coord_base_for(&app_handle);
    let minted = mint_attach_grant(&base, session_uuid).await?;
    if let Some(target) = minted.target_device_id.as_deref() {
        let asked = device_id.trim();
        if !asked.is_empty() && !target.eq_ignore_ascii_case(asked) {
            return Err(format!(
                "remote_attach:target_mismatch: coord places session {session_uuid} on device \
                 {target}, not {asked} — refresh the fleet list"
            ));
        }
    }
    info!(
        session = %session_uuid,
        grant_jti = %minted.grant_jti,
        target = ?minted.target_device_id,
        expires_at = ?minted.expires_at,
        "remote attach: grant minted; presenting through the relay"
    );

    let attached = client()
        .attach(&minted.grant, cols, rows, ATTACH_TIMEOUT)
        .await
        .map_err(|e| e.to_string())?;
    if attached.grant_jti != minted.grant_jti {
        client().discard_pending_output(&attached.grant_jti);
        return Err(format!(
            "remote_attach:grant_mismatch: the target answered for grant {} but {} was presented",
            attached.grant_jti, minted.grant_jti
        ));
    }

    let pane = Arc::new(RemotePaneIo::new(
        minted.grant_jti.clone(),
        attached.terminal_id.clone(),
        minted.grant.clone(),
        client().sink(),
        cols,
        rows,
        attached.ring,
    ));
    client().register_pane(pane.clone());

    let target_id = minted
        .target_device_id
        .clone()
        .unwrap_or_else(|| device_id.clone());
    let title = format!(
        "{}: {}",
        non_blank(device_label).unwrap_or_else(|| short_id(&target_id)),
        non_blank(session_label).unwrap_or_else(|| short_id(&session_id)),
    );
    let display_dir = non_blank(working_dir).unwrap_or_default();

    let io: Arc<dyn PaneIo> = pane.clone();
    let tm = terminal_manager.inner().clone();
    let pinned = session_uuid.to_string();
    let spawn_title = title.clone();
    let created = spawn_blocking_tracked(move || {
        tm.create_with_io(
            spawn_title,
            display_dir,
            page_id,
            cols,
            rows,
            app_handle,
            io,
            pinned,
        )
    })
    .await
    .map_err(|e| format!("remote attach spawn task failed: {e}"))
    .and_then(|r| r);

    match created {
        Ok(info) => {
            info!(
                terminal_id = %info.id,
                remote_terminal_id = %attached.terminal_id,
                grant_jti = %minted.grant_jti,
                title = %title,
                "remote attach: tab open"
            );
            Ok(info)
        }
        Err(e) => {
            // Tell the target this viewer is gone and let the client sweep
            // the pane; otherwise the grant stays bound to a pane nobody owns.
            let _ = pane.release(Duration::from_millis(200));
            pane.mark_exit(ERROR_EXIT_CODE);
            client().discard_pending_output(&minted.grant_jti);
            Err(format!("remote_attach:session_spawn_failed: {e}"))
        }
    }
}
