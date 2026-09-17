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
use crate::terminal::remote_pane_io::{DetachOutcome, RemotePaneIo, ERROR_EXIT_CODE};
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
    /// What coord could establish about the TARGET's runner build (plan
    /// `2026-09-17-remote-attach-to-a-pre-feature-target-times-out-silently`).
    /// Absent from a coord that predates it.
    #[serde(default)]
    pub target_runner: Option<TargetRunner>,
}

/// Coord's `target_runner` block on a grant mint: `state` is `supports` or
/// `unknown` (a positive `predates` is a 409, never a 201).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub(crate) struct TargetRunner {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub required_sha: Option<String>,
}

/// Explain a relay timeout — the target never answered — with what is actually
/// known, instead of guessing "relay disconnected or target offline".
///
/// A runner built before the target-side handler IGNORES the frame without a
/// reply, which is indistinguishable from a wedged target on the wire; coord's
/// `target_runner` block is the only evidence that separates them. `what` is
/// the reply that never came (`remote_terminal_attached` /
/// `remote_terminal_created`). Pure, so every arm is a unit test.
pub(crate) fn explain_relay_timeout(
    what: &str,
    target_device_id: &str,
    target_runner: Option<&TargetRunner>,
    timeout_secs: u64,
) -> String {
    let head = format!("no {what} from target device {target_device_id} within {timeout_secs}s");
    match target_runner {
        Some(tr) if tr.state == "supports" => format!(
            "{head}. Coord observed that device serving a runner build that carries the handler, \
             so the target runner is wedged or offline, or the relay lost the frame — its own \
             runner log says which."
        ),
        Some(tr) if tr.state == "unknown" => format!(
            "{head}. Coord could not establish the target's runner build ({}). A runner older \
             than {} ignores this request without answering, which looks exactly like this.",
            tr.reason.as_deref().unwrap_or("no reason given"),
            tr.required_sha
                .as_deref()
                .unwrap_or("the remote-terminal handler"),
        ),
        _ => format!(
            "{head}. Coord did not report the target's runner build. The target may be offline, \
             or running a runner too old to answer — such a runner ignores the request silently."
        ),
    }
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
        return Err(format_mint_refusal(status.as_u16(), &body, &url));
    }
    serde_json::from_str(&body)
        .map_err(|e| format!("remote_attach:coord_parse: could not decode the grant response: {e}"))
}

/// Pure: the typed error string for a non-2xx attach-grant mint.
///
/// Shape `remote_attach:<error>[:<reason>]: [<hint> ](coord answered <status>
/// for POST <url>)` — the picker's `attachErrorMessage` lifts the code out of
/// it. Coord's `hint` (e.g. which runner build the target serves and which it
/// needs) leads the detail when present. `reason` is only folded into the code
/// when it is a bare `[a-z_]+` token, because the picker's pattern admits
/// nothing else there and a non-matching string would be shown raw.
pub(crate) fn format_mint_refusal(status: u16, body: &str, url: &str) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = parsed
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("coord_error");
    let raw_reason = parsed
        .get("reason")
        .and_then(|v| v.as_str())
        .filter(|r| !r.is_empty());
    let liftable = |r: &str| r.chars().all(|c| c.is_ascii_lowercase() || c == '_');
    let reason = raw_reason
        .filter(|r| liftable(r))
        .map(|r| format!(":{r}"))
        .unwrap_or_default();
    // A reason the picker cannot lift into the code still reaches the detail.
    let unliftable = raw_reason
        .filter(|r| !liftable(r))
        .map(|r| format!("reason: {r}; "))
        .unwrap_or_default();
    let hint = parsed
        .get("hint")
        .and_then(|v| v.as_str())
        .map(|h| format!("{h} "))
        .unwrap_or_default();
    format!(
        "remote_attach:{code}{reason}: {hint}({unliftable}coord answered {status} for POST {url})"
    )
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
        .map_err(|mut e| {
            if e.code == "timeout" {
                e.message = explain_relay_timeout(
                    "remote_terminal_attached",
                    minted
                        .target_device_id
                        .as_deref()
                        .unwrap_or(device_id.trim()),
                    minted.target_runner.as_ref(),
                    ATTACH_TIMEOUT.as_secs(),
                );
            }
            e.to_string()
        })?;
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
            terminal_manager.set_remote_pane(&info.id, pane.clone());
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

/// What closing a remote tab did about the relay's `(target, terminal)`
/// binding — plan
/// `2026-09-16-remote-tab-cannot-be-released-so-the-target-terminal-stays-claimed`,
/// Phase 1.
///
/// `terminal_close` used to answer a bare `success: true` for a remote tab
/// whether or not its `remote_terminal_detach` was ever queued, so an
/// operator (or a headless harness) could not tell "detach sent" from
/// "closed locally, binding still held until the grant expires". Capture the
/// probe BEFORE the close — the close removes the identity and the pane — and
/// render it after.
pub(crate) struct RemoteCloseProbe {
    identity: RemoteTabIdentity,
    pane: Option<Arc<RemotePaneIo>>,
    /// The relay pump as it stood before the close; compared after it.
    pump_before: (bool, u64),
}

/// `None` for a local tab: its close response is unchanged.
pub(crate) fn probe_remote_close(
    tm: &TerminalManager,
    terminal_id: &str,
) -> Option<RemoteCloseProbe> {
    let identity = tm.remote_identity(terminal_id)?;
    // The manager keeps the pane for the tab's whole life; the client's
    // routing table drops it when the remote side exits, which is exactly
    // the DEAD tab an operator closes later.
    let pane = tm
        .remote_pane(terminal_id)
        .or_else(|| client().pane(&identity.grant_jti));
    Some(RemoteCloseProbe {
        identity,
        pane,
        pump_before: client().outbound_pump_state(),
    })
}

/// The rendered outcome: a one-line `message` and the `remoteDetach` object.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RemoteCloseReport {
    pub message: String,
    pub remote_detach: Value,
}

impl RemoteCloseProbe {
    /// Read the pane's detach outcome now (after the close ran).
    pub(crate) fn report(&self) -> RemoteCloseReport {
        let pump_after = client().outbound_pump_state();
        render_remote_close(
            &self.identity,
            self.pane.as_ref().map(|p| p.detach_outcome()),
            stable_pump(self.pump_before, pump_after),
        )
    }
}

/// Whether a relay connection held the outbound pump across the whole close:
/// `Some(attached)` when the before/after readings agree, `None` when the
/// connection changed in between — a frame queued in that window may have
/// been drained by the old connection, the new one, or discarded.
pub(crate) fn stable_pump(before: (bool, u64), after: (bool, u64)) -> Option<bool> {
    (before == after).then_some(before.0)
}

/// Pure rendering, unit-tested per outcome × pump state. `outcome` is `None`
/// when no pane could be found for the tab, which is UNKNOWN. Nothing here is
/// ever reported as released: the relay's handling of the frame is not
/// observable from this runner, and only a re-attach of the same terminal
/// proves the binding is gone.
pub(crate) fn render_remote_close(
    identity: &RemoteTabIdentity,
    outcome: Option<DetachOutcome>,
    pump: Option<bool>,
) -> RemoteCloseReport {
    const NO_DRAIN_TAIL: &str = "The relay drops a source's bindings when it sees that \
         connection close (not observed from here); either way the binding lasts no longer \
         than its grant.";
    let what = format!(
        "remote terminal {} on {}",
        short_id(&identity.remote_terminal_id),
        identity.device_label
    );
    let (code, error, message) = match (outcome, pump) {
        (Some(DetachOutcome::Queued), Some(true)) => (
            "queued",
            None,
            format!(
                "Closed; detach for {what} queued on the live relay connection. The relay \
                 drops the binding if it receives it."
            ),
        ),
        (Some(DetachOutcome::Queued), Some(false)) => (
            "queued",
            None,
            format!(
                "Closed; detach for {what} queued, but no relay connection is draining the \
                 queue, so the frame will be discarded on reconnect. {NO_DRAIN_TAIL}"
            ),
        ),
        (Some(DetachOutcome::Queued), None) => (
            "queued",
            None,
            format!(
                "Closed; detach for {what} queued while the relay connection changed, so \
                 whether it was delivered is unknown. {NO_DRAIN_TAIL}"
            ),
        ),
        (Some(DetachOutcome::Failed(e)), Some(true)) => (
            "failed",
            Some(e.clone()),
            format!(
                "Closed locally, but the detach for {what} could not be queued ({e}). The \
                 relay keeps the binding until this runner's relay connection drops or the \
                 grant expires."
            ),
        ),
        (Some(DetachOutcome::Failed(e)), _) => (
            "failed",
            Some(e.clone()),
            format!(
                "Closed locally, but the detach for {what} could not be queued ({e}), and \
                 no relay connection was steadily draining the queue. {NO_DRAIN_TAIL}"
            ),
        ),
        (Some(DetachOutcome::NotAttempted), _) => (
            "not_attempted",
            None,
            format!(
                "Closed locally, but no detach was attempted for {what}. The binding lasts \
                 until this runner's relay connection drops or the grant expires."
            ),
        ),
        (None, _) => (
            "unknown",
            None,
            format!(
                "Closed; no pane was found for {what}, so whether a detach was queued is \
                 unknown. The binding lasts no longer than its grant."
            ),
        ),
    };
    RemoteCloseReport {
        message,
        remote_detach: json!({
            "outcome": code,
            "error": error,
            "relayPumpAttached": pump,
            "targetDeviceId": identity.device_id,
            "remoteTerminalId": identity.remote_terminal_id,
        }),
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
/// **Every arm fails CLOSED.** An unreported placement is not an agreement: the
/// question this predicate asks is *which session may this caller address*, and
/// device authentication answers *who is asking* — a different question, so it
/// is no justification for taking silence as a yes. Against a coord that omits
/// `target_device_id`, create-then-attach is exactly as unbound as it was
/// before the check existed, which is the whole defect (review round 2, finding
/// 5). Coord's `AttachGrantResponse.target_device_id` is a non-`Option` `Uuid`
/// today, so nothing reaches the permissive arm — and the field being
/// `Option<String>` here is only wire tolerance, not a supported coord that
/// omits it. An `asked` this device could not name is refused for the same
/// reason: nothing was claimed, so nothing is confirmed.
///
/// Both refusals surface as the caller's existing `target_mismatch`, which
/// names `<unreported>` when coord said nothing — so the operator sees the
/// unanswered question rather than a silent bind.
pub(crate) fn coord_places_session_on(asked: &str, coord_placed: Option<&str>) -> bool {
    let asked = asked.trim();
    if asked.is_empty() {
        return false;
    }
    match coord_placed.map(str::trim).filter(|p| !p.is_empty()) {
        None => false,
        Some(placed) => placed.eq_ignore_ascii_case(asked),
    }
}

#[cfg(test)]
mod relay_timeout_tests {
    use super::{explain_relay_timeout, AttachGrantResponse, TargetRunner};

    const DEV: &str = "84c02292-32cb-4983-be85-d00f868b7003";

    /// A relay that is not connected refuses the attach at once as
    /// `relay_unavailable`, and a relay drop mid-wait settles it as
    /// `relay_disconnected` (`RemoteAttachClient::attach` /
    /// `on_relay_disconnected`), so no timeout arm blames the relay.
    #[test]
    fn every_arm_names_the_target_device_and_never_guesses_a_relay_disconnect() {
        let supports = TargetRunner {
            state: "supports".into(),
            ..Default::default()
        };
        let unknown = TargetRunner {
            state: "unknown".into(),
            reason: Some("served-sha heartbeat is STALE".into()),
            required_sha: Some("f521e1012e1e".into()),
        };
        for tr in [None, Some(&supports), Some(&unknown)] {
            let m = explain_relay_timeout("remote_terminal_attached", DEV, tr, 20);
            assert!(m.contains(DEV), "{m}");
            assert!(m.contains("20s"), "{m}");
            assert!(!m.contains("relay may be disconnected"), "{m}");
        }
    }

    #[test]
    fn unknown_carries_coords_reason_and_the_required_build() {
        let tr = TargetRunner {
            state: "unknown".into(),
            reason: Some("served-sha heartbeat is STALE".into()),
            required_sha: Some("f521e1012e1e".into()),
        };
        let m = explain_relay_timeout("remote_terminal_attached", DEV, Some(&tr), 20);
        assert!(m.contains("served-sha heartbeat is STALE"), "{m}");
        assert!(m.contains("f521e1012e1e"), "{m}");
    }

    #[test]
    fn supports_points_away_from_the_build() {
        let tr = TargetRunner {
            state: "supports".into(),
            ..Default::default()
        };
        let m = explain_relay_timeout("remote_terminal_created", DEV, Some(&tr), 45);
        assert!(m.contains("wedged or offline"), "{m}");
        assert!(!m.contains("older"), "{m}");
    }

    #[test]
    fn a_mint_refusal_leads_with_coords_hint_and_keeps_the_picker_shape() {
        let body = r#"{"error":"target_runner_predates_remote_attach","target_device_id":"84c02292-32cb-4983-be85-d00f868b7003","served_sha":"3472fc6a1c58","required_sha":"f521e1012e1e3f84e1dd62ec90dbc3d274f322b0","hint":"The target device 84c02292 is serving qontinui-runner 3472fc6a1c58, which predates remote attach."}"#;
        let m = super::format_mint_refusal(409, body, "https://coord/x");
        assert_eq!(
            m,
            "remote_attach:target_runner_predates_remote_attach: The target device 84c02292 is \
             serving qontinui-runner 3472fc6a1c58, which predates remote attach. (coord answered \
             409 for POST https://coord/x)"
        );
        // The existing reason arm is unchanged.
        assert_eq!(
            super::format_mint_refusal(
                403,
                r#"{"error":"attach_forbidden","reason":"preference_off"}"#,
                "u"
            ),
            "remote_attach:attach_forbidden:preference_off: (coord answered 403 for POST u)"
        );
        // A reason the picker's `[a-z_]+` cannot lift is not folded into the code.
        assert_eq!(
            super::format_mint_refusal(409, r#"{"error":"x","reason":"sha-3472fc6a"}"#, "u"),
            "remote_attach:x: (reason: sha-3472fc6a; coord answered 409 for POST u)"
        );
        // A non-JSON body still yields a typed code.
        assert!(super::format_mint_refusal(502, "<html>", "u")
            .starts_with("remote_attach:coord_error: "));
    }

    #[test]
    fn the_mint_response_tolerates_a_coord_without_the_block_and_reads_it_when_present() {
        let old: AttachGrantResponse =
            serde_json::from_str(r#"{"grant":"g","grant_jti":"j"}"#).unwrap();
        assert!(old.target_runner.is_none());
        let new: AttachGrantResponse = serde_json::from_str(
            r#"{"grant":"g","grant_jti":"j","target_runner":{"state":"unknown","required_sha":"abc","reason":"why"}}"#,
        )
        .unwrap();
        let tr = new.target_runner.unwrap();
        assert_eq!(tr.state, "unknown");
        assert_eq!(tr.reason.as_deref(), Some("why"));
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

    /// Review round 2, finding 5. An unanswered question is not a yes.
    ///
    /// The permissive arm was justified as "safe because the mint is
    /// device-authenticated" — but device auth establishes WHO is asking, not
    /// WHICH session they may address. Against a coord that omits the field,
    /// create-then-attach was exactly as unbound as before the check.
    #[test]
    fn an_unreported_placement_is_refused() {
        assert!(!coord_places_session_on("device-b", None));
        assert!(!coord_places_session_on("device-b", Some("")));
        assert!(!coord_places_session_on("device-b", Some("   ")));
        // …and a caller that named no device confirms nothing either.
        assert!(!coord_places_session_on("", Some("device-c")));
        assert!(!coord_places_session_on("   ", Some("device-c")));
        assert!(!coord_places_session_on("", None));
    }
}

#[cfg(test)]
mod remote_close_tests {
    use super::{
        probe_remote_close, render_remote_close, stable_pump, DetachOutcome, RemoteTabIdentity,
    };
    use crate::terminal::TerminalManager;
    use serde_json::Value;

    fn identity() -> RemoteTabIdentity {
        RemoteTabIdentity {
            device_id: "c79a07d5-0000-0000-0000-000000000000".into(),
            device_label: "spaceship".into(),
            session_id: "11111111-2222-3333-4444-555555555555".into(),
            remote_terminal_id: "490212f5-aaaa-bbbb-cccc-dddddddddddd".into(),
            grant_jti: "01a0905e".into(),
            history_available: false,
        }
    }

    fn every_case() -> Vec<(Option<DetachOutcome>, Option<bool>)> {
        let outcomes = [
            Some(DetachOutcome::Queued),
            Some(DetachOutcome::Failed("backlog is full".into())),
            Some(DetachOutcome::NotAttempted),
            None,
        ];
        let pumps = [Some(true), Some(false), None];
        outcomes
            .iter()
            .flat_map(|o| pumps.iter().map(move |p| (o.clone(), *p)))
            .collect()
    }

    /// Plan 2026-09-16 Phase 1, R2/R3: every outcome × pump state names the
    /// outcome it actually got, never claims a release, and never leaks the
    /// grant jti (`RemoteTabIdentity` withholds it from the frontend too).
    #[test]
    fn no_remote_close_message_claims_a_release_or_leaks_the_grant() {
        for (outcome, pump) in every_case() {
            let r = render_remote_close(&identity(), outcome.clone(), pump);
            let text = r.remote_detach.to_string() + &r.message;
            assert!(
                !r.message.to_lowercase().contains("released"),
                "{outcome:?}/{pump:?} reads as released: {}",
                r.message
            );
            assert!(!text.contains("01a0905e"), "grant jti leaked: {text}");
            assert!(r.message.contains("spaceship"), "{}", r.message);
            assert_eq!(
                r.remote_detach["targetDeviceId"],
                "c79a07d5-0000-0000-0000-000000000000"
            );
            assert_eq!(
                r.remote_detach["remoteTerminalId"],
                "490212f5-aaaa-bbbb-cccc-dddddddddddd"
            );
            match pump {
                Some(b) => assert_eq!(r.remote_detach["relayPumpAttached"], b),
                None => assert_eq!(r.remote_detach["relayPumpAttached"], Value::Null),
            }
        }
    }

    #[test]
    fn remote_close_reports_each_detach_outcome() {
        let id = identity();

        let r = render_remote_close(&id, Some(DetachOutcome::Queued), Some(true));
        assert_eq!(r.remote_detach["outcome"], "queued");
        assert!(r.message.contains("live relay connection"), "{}", r.message);

        let r = render_remote_close(&id, Some(DetachOutcome::Queued), Some(false));
        assert_eq!(r.remote_detach["outcome"], "queued");
        assert!(
            r.message.contains("discarded on reconnect"),
            "{}",
            r.message
        );

        let r = render_remote_close(&id, Some(DetachOutcome::Queued), None);
        assert_eq!(r.remote_detach["outcome"], "queued");
        assert!(r.message.contains("connection changed"), "{}", r.message);

        let failed = Some(DetachOutcome::Failed("backlog is full".into()));
        let r = render_remote_close(&id, failed.clone(), Some(true));
        assert_eq!(r.remote_detach["outcome"], "failed");
        assert_eq!(r.remote_detach["error"], "backlog is full");
        assert!(r.message.contains("could not be queued (backlog is full)"));
        assert!(r.message.contains("keeps the binding"), "{}", r.message);

        // With no connection draining the queue, a failed queue must not
        // claim the relay still holds the binding "until the connection
        // drops" — that connection is already gone.
        let r = render_remote_close(&id, failed, Some(false));
        assert_eq!(r.remote_detach["outcome"], "failed");
        assert!(!r.message.contains("keeps the binding"), "{}", r.message);
        assert!(
            r.message.contains("not observed from here"),
            "{}",
            r.message
        );

        let r = render_remote_close(&id, Some(DetachOutcome::NotAttempted), Some(true));
        assert_eq!(r.remote_detach["outcome"], "not_attempted");
        assert_eq!(r.remote_detach["error"], Value::Null);

        let r = render_remote_close(&id, None, Some(true));
        assert_eq!(r.remote_detach["outcome"], "unknown");
        assert!(r.message.contains("unknown"), "{}", r.message);
    }

    /// A reconnect during the close — either half of the pump reading moving
    /// — makes the pump state unknown rather than whichever end was read.
    #[test]
    fn a_pump_that_changed_during_the_close_is_unknown() {
        assert_eq!(stable_pump((true, 3), (true, 3)), Some(true));
        assert_eq!(stable_pump((false, 3), (false, 3)), Some(false));
        assert_eq!(stable_pump((false, 3), (true, 4)), None);
        assert_eq!(stable_pump((true, 3), (true, 4)), None);
        assert_eq!(stable_pump((true, 3), (false, 3)), None);
    }

    /// Review round 2, nit B: a DEAD tab's pane is gone from the attach
    /// client's routing table (it is dropped on `remote_terminal_exit`), but
    /// the manager still holds it — the probe must read the manager's copy
    /// and report the detach the close queued, not "unknown".
    #[test]
    fn a_dead_tab_still_reports_the_detach_its_close_queued() {
        use crate::terminal::pane_io::PaneIo;
        use crate::terminal::remote_pane_io::tests::RecordingSink;
        use crate::terminal::remote_pane_io::{AttachedRing, RemoteFrameSink, RemotePaneIo};
        use std::sync::Arc;
        use std::time::Duration;

        let tm = TerminalManager::new();
        let mut id = identity();
        id.grant_jti = "jti-never-registered-with-the-client".into();
        tm.set_remote_identity("local-tab-1", id);
        let sink = Arc::new(RecordingSink::default());
        let dyn_sink: Arc<dyn RemoteFrameSink> = sink.clone();
        let pane = Arc::new(RemotePaneIo::new(
            "jti-never-registered-with-the-client",
            "490212f5-aaaa-bbbb-cccc-dddddddddddd",
            "grant.jwt",
            dyn_sink,
            80,
            24,
            AttachedRing::default(),
        ));
        tm.set_remote_pane("local-tab-1", pane.clone());

        let probe = probe_remote_close(&tm, "local-tab-1").expect("a remote tab");
        pane.kill(Duration::from_millis(10)).unwrap();
        let report = probe.report();
        assert_eq!(
            report.remote_detach["outcome"], "queued",
            "{}",
            report.message
        );
        assert_eq!(sink.frames().len(), 1);
    }

    /// A local tab has no remote identity, so both close doors keep their
    /// unchanged response.
    #[test]
    fn a_local_tab_yields_no_remote_close_probe() {
        let tm = TerminalManager::new();
        assert!(probe_remote_close(&tm, "not-a-remote-tab").is_none());
    }
}
