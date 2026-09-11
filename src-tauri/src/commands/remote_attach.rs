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
use tauri::{Emitter, Manager};
use tracing::{info, warn};

use super::CommandResponse;
use crate::mcp::remote_terminal::{client, ATTACH_TIMEOUT};
use crate::session::SessionRegistry;
use crate::settings::AcceptRemoteAttach;
use crate::terminal::pane_io::PaneIo;
use crate::terminal::remote_pane_io::{RemotePaneIo, ERROR_EXIT_CODE};
use crate::terminal::types::{RemoteTabIdentity, RemoteTerminalInfo};
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
pub(crate) struct AttachGrantResponse {
    pub grant: String,
    pub grant_jti: String,
    #[serde(default)]
    pub target_device_id: Option<String>,
    #[serde(default)]
    pub expires_at: Option<Value>,
}

/// Coord learns about a session through the registry's OUTBOX, not through the
/// call that registered it, so a session registered microseconds ago is not yet
/// a row coord's mint can resolve. A remote create hits exactly that window:
/// the target registers, answers `terminal_created`, and the source mints
/// against an id coord has not drained yet — a `404 session_not_found` that is
/// a RACE, not an absence. Retried for this long before it is reported.
const SESSION_VISIBILITY_RETRY: Duration = Duration::from_millis(750);
const SESSION_VISIBILITY_ATTEMPTS: u32 = 10;

/// [`mint_attach_grant`], retrying ONLY a `session_not_found` — the one
/// refusal that can become an admission by waiting. Every other refusal
/// (`attach_forbidden`, a credential answer, a transport failure) is returned
/// on the first attempt: retrying those would turn one honest refusal into a
/// long silence ending in the same refusal.
pub(crate) async fn mint_attach_grant_awaiting_session(
    coord_base: &str,
    session_id: uuid::Uuid,
) -> Result<AttachGrantResponse, String> {
    let mut last = String::new();
    for attempt in 0..SESSION_VISIBILITY_ATTEMPTS {
        match mint_attach_grant(coord_base, session_id).await {
            Ok(minted) => return Ok(minted),
            Err(e) if e.starts_with("remote_attach:session_not_found") => {
                last = e;
                if attempt + 1 < SESSION_VISIBILITY_ATTEMPTS {
                    tokio::time::sleep(SESSION_VISIBILITY_RETRY).await;
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(format!(
        "{last} (retried for {}s while coord's outbox caught up)",
        (SESSION_VISIBILITY_RETRY * SESSION_VISIBILITY_ATTEMPTS).as_secs_f32()
    ))
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

/// Tauri event carrying a remote tab's identity, emitted right after the
/// `terminal-created` event for the same terminal id (Phase 4). The frontend
/// buffers it when it lands first, exactly as it does the bypass mark.
pub const REMOTE_IDENTITY_EVENT: &str = "terminal-remote-identity";

/// Open a tab onto a session running on another device in the tenant.
///
/// `device_id` / `session_id` are the picker row (`fleet_sessions_list`);
/// `device_label`, `session_label` and `working_dir` are the row's display
/// values, used only for the tab title and the info's working dir — the
/// authority for WHERE the session lives is coord's answer to the mint, and
/// a disagreement with `device_id` is refused rather than followed.
///
/// Returns the ordinary `TerminalInfo` (flattened) plus the `remote`
/// identity the tab is keyed on. Re-running it for the same
/// `(device_id, session_id)` IS the reattach path: a fresh grant is minted
/// (the old one may have expired) and a fresh local terminal opens; the
/// frontend replaces the dead tab with it.
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
) -> Result<RemoteTerminalInfo, String> {
    let session_uuid = uuid::Uuid::parse_str(session_id.trim()).map_err(|e| {
        format!("remote_attach:invalid_session_id: {session_id:?} is not a session uuid: {e}")
    })?;
    let cols = cols.unwrap_or(120);
    let rows = rows.unwrap_or(30);

    let base = coord_base_for(&app_handle);
    let minted = mint_attach_grant(&base, session_uuid).await?;
    if !coord_places_session_on(&device_id, minted.target_device_id.as_deref()) {
        let target = minted.target_device_id.as_deref().unwrap_or("<unreported>");
        let asked = device_id.trim();
        return Err(format!(
            "remote_attach:target_mismatch: coord places session {session_uuid} on device \
             {target}, not {asked} — refresh the fleet list"
        ));
    }
    info!(
        session = %session_uuid,
        grant_jti = %minted.grant_jti,
        target = ?minted.target_device_id,
        expires_at = ?minted.expires_at,
        "remote attach: grant minted; presenting through the relay"
    );

    open_remote_tab(
        terminal_manager.inner(),
        &app_handle,
        OpenRemoteTab {
            minted,
            device_id,
            session_uuid,
            cols,
            rows,
            page_id,
            device_label,
            session_label,
            working_dir,
        },
    )
    .await
}

/// Everything [`open_remote_tab`] needs that is not the manager or the app
/// handle. A struct rather than eleven positional arguments, because two
/// callers now build it: the attach command above, and the remote CREATE
/// command, which mints its attach grant for a session the target had to
/// register first.
pub(crate) struct OpenRemoteTab {
    pub minted: AttachGrantResponse,
    pub device_id: String,
    pub session_uuid: uuid::Uuid,
    pub cols: u16,
    pub rows: u16,
    pub page_id: Option<String>,
    pub device_label: Option<String>,
    pub session_label: Option<String>,
    pub working_dir: Option<String>,
}

/// Present a minted ATTACH grant through the relay and open the tab around the
/// resulting [`RemotePaneIo`]. The single implementation of "a remote tab is
/// opened this way" — the create path composes over it rather than growing a
/// second one.
pub(crate) async fn open_remote_tab(
    terminal_manager: &Arc<TerminalManager>,
    app_handle: &tauri::AppHandle,
    req: OpenRemoteTab,
) -> Result<RemoteTerminalInfo, String> {
    let OpenRemoteTab {
        minted,
        device_id,
        session_uuid,
        cols,
        rows,
        page_id,
        device_label,
        session_label,
        working_dir,
    } = req;
    let session_id = session_uuid.to_string();
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
    let device_label = non_blank(device_label).unwrap_or_else(|| short_id(&target_id));
    let title = format!(
        "{}: {}",
        device_label,
        non_blank(session_label).unwrap_or_else(|| short_id(&session_id)),
    );
    let display_dir = non_blank(working_dir).unwrap_or_default();
    let identity = RemoteTabIdentity {
        device_id: target_id.clone(),
        device_label,
        session_id: session_uuid.to_string(),
        remote_terminal_id: attached.terminal_id.clone(),
        grant_jti: minted.grant_jti.clone(),
        history_available: pane.history_range().is_some(),
    };

    let io: Arc<dyn PaneIo> = pane.clone();
    let tm = terminal_manager.clone();
    let pinned = session_uuid.to_string();
    let spawn_title = title.clone();
    let spawn_app = app_handle.clone();
    let created = spawn_blocking_tracked(move || {
        tm.create_with_io(
            spawn_title,
            display_dir,
            page_id,
            cols,
            rows,
            spawn_app,
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
                history_available = identity.history_available,
                "remote attach: tab open"
            );
            terminal_manager.set_remote_identity(&info.id, identity.clone());
            // `create_with_io` already emitted `terminal-created`, which is
            // what opens the tab on its page; this decorates it. Emitted
            // AFTER the identity is recorded so a reconnecting webview that
            // misses the event reads it from `terminal_remote_identities`.
            if let Err(e) = app_handle.emit(
                REMOTE_IDENTITY_EVENT,
                json!({ "id": info.id, "remote": identity }),
            ) {
                warn!(error = %e, "remote attach: terminal-remote-identity emit failed");
            }
            Ok(RemoteTerminalInfo {
                info,
                remote: identity,
            })
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

/// Every live remote tab's identity keyed by LOCAL terminal id — what a
/// reconnecting webview reads after `terminal_list`, whose shared-schema
/// `TerminalInfo` cannot carry the remote identity.
#[tauri::command]
pub fn terminal_remote_identities(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<CommandResponse, String> {
    let map = terminal_manager.remote_identities();
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::to_value(map).map_err(|e| e.to_string())?),
    })
}

/// How long a history request waits for the target's ring range.
const HISTORY_TIMEOUT: Duration = Duration::from_secs(15);

/// Phase 5 lazy scrollback: fetch the target ring bytes OLDER than what the
/// attach reply shipped, for the remote tab `terminal_id`.
///
/// The bytes are returned to the caller (`data` base64, `startOffset` /
/// `endOffset` in the TARGET's stream) and deliberately NOT spliced into the
/// local session's ring: that ring is offset-anchored to the bytes it has
/// already tee'd, and prepending would shift every live chunk's offset under
/// the frontend's gap detection. The pane instead resets and re-renders
/// history + its own ring in one pass. A tab without earlier history answers
/// `success: false` with the reason rather than an empty payload.
#[tauri::command]
pub async fn terminal_remote_history_load(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let Some(identity) = terminal_manager.remote_identity(&terminal_id) else {
        return Err(format!(
            "remote_attach:not_remote: terminal {terminal_id} is not a remote tab"
        ));
    };
    let Some(pane) = client().pane(&identity.grant_jti) else {
        return Err(format!(
            "remote_attach:pane_gone: the remote pane behind terminal {terminal_id} is closed"
        ));
    };
    let Some((from, to)) = pane.history_range() else {
        return Ok(CommandResponse {
            success: false,
            message: Some(
                "The attach already delivered everything the remote ring holds — there is no \
                 earlier output to load"
                    .to_string(),
            ),
            data: None,
        });
    };
    let reply = client()
        .request_history(&pane, from, to, HISTORY_TIMEOUT)
        .await
        .map_err(|e| e.to_string())?;
    let start = reply.ring.start_offset;
    let end = start.saturating_add(reply.ring.buffer.len() as u64);
    info!(
        terminal_id = %terminal_id,
        grant_jti = %identity.grant_jti,
        requested_from = from,
        requested_to = to,
        got_from = start,
        got_to = end,
        "remote attach: earlier output loaded"
    );
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(json!({
            "data": base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &reply.ring.buffer
            ),
            "startOffset": start,
            "endOffset": end,
            "requestedFrom": from,
            "requestedTo": to,
        })),
    })
}

/// Does coord place this session on the device the operator addressed?
///
/// **The authority for WHERE a session lives is coord's answer to the mint, not
/// the device id the caller carried.** `terminal_attach_remote` has always
/// refused a disagreement; the create-then-attach path called the same mint,
/// read the same field, and DISCARDED it — on the path where the session id
/// arrived over the untrusted relay (review finding 3). One predicate now, so
/// the two cannot drift again.
///
/// `coord_placed == None` is TRUE: an older coord that does not report the
/// placement leaves the question unanswered, and there is nothing to disagree
/// with. That is a deliberate asymmetry with the rest of this module's
/// fail-closed posture — refusing there would break every attach against such a
/// coord — and it is safe only because the mint itself is device-authenticated.
/// `asked` empty is TRUE for the same reason: nothing was claimed.
pub(crate) fn coord_places_session_on(asked: &str, coord_placed: Option<&str>) -> bool {
    let asked = asked.trim();
    match coord_placed.map(str::trim).filter(|p| !p.is_empty()) {
        None => true,
        Some(placed) => asked.is_empty() || placed.eq_ignore_ascii_case(asked),
    }
}

#[cfg(test)]
mod placement_tests {
    use super::coord_places_session_on;

    /// Review finding 3. The scenario: the operator clicks New terminal on B, a
    /// relay rewrites `coord_session_id` on `remote_terminal_created` to a
    /// session living on C, and the source mints an attach grant for C's
    /// session while labelling the tab B — so the operator types into C's live
    /// agent session. Coord's placement is the only thing that catches it, and
    /// the create path was throwing it away.
    #[test]
    fn a_session_coord_places_elsewhere_is_refused() {
        assert!(!coord_places_session_on("device-b", Some("device-c")));
        assert!(!coord_places_session_on("  device-b  ", Some("device-c")));
    }

    /// Hex case is not identity.
    #[test]
    fn the_matching_case_is_case_insensitive_and_trimmed() {
        assert!(coord_places_session_on("DEVICE-B", Some("device-b")));
        assert!(coord_places_session_on("device-b", Some("  DEVICE-B ")));
    }

    /// An unanswered question is not a disagreement. Refusing here would break
    /// every attach against a coord that does not report the placement.
    #[test]
    fn an_unreported_placement_is_not_a_mismatch() {
        assert!(coord_places_session_on("device-b", None));
        assert!(coord_places_session_on("device-b", Some("")));
        assert!(coord_places_session_on("device-b", Some("   ")));
        assert!(coord_places_session_on("", Some("device-c")));
    }
}
