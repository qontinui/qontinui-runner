//! Remote terminal CREATION — the operator-facing preference commands, and the
//! coord mirror that makes the two stores provably consistent (plan
//! `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase 3b).
//!
//! ## The problem this module exists to close
//!
//! Remote create shipped with TWO independent dials and nothing reconciling
//! them: a runner-local `settings.json` value (`remote_create.accept_remote_create`,
//! read by [`crate::mcp::remote_terminal::admit_terminal_create`]) and a coord
//! column (`coord.devices.accept_remote_create`, read by coord's mint gate).
//! Either could refuse what the other admitted, and nothing said which was
//! right or even that they disagreed.
//!
//! ## Which one is authoritative, and why
//!
//! **The runner-local setting is the device owner's expressed choice; coord's
//! column is a MIRROR of it.** That is the answer remote attach already
//! reached (`remote_attach.rs`, `PUT /coord/devices/me/attach-preference`), and
//! it transfers here unchanged, for reasons that are stronger for create than
//! they were for attach:
//!
//! * **One writer.** Coord's route is `/coord/devices/me/…` — only the device
//!   itself can write its own row. So the runner is the sole writer of both
//!   copies, and "which is authoritative" is a question about direction of
//!   flow, not about merging concurrent edits. Any other answer would need a
//!   second writer and a conflict rule.
//! * **The enforcing party owns the switch.** The runner owns the PTY. A dial
//!   that only coord held could be read by the mint and then contradicted by
//!   the machine that actually spawns — a promise coord cannot keep. Keeping
//!   the local value as the enforced one means the machine's own refusal is
//!   always the last word.
//! * **Divergence fails CLOSED in the direction that matters.** Both sides must
//!   admit for a create to happen: coord's mint reads the column, and the
//!   target's gate reads the local value. So a coord copy that is stale-OPEN is
//!   still refused by the runner, and a coord copy that is stale-SHUT merely
//!   refuses something the owner had enabled. Neither drift spawns a PTY the
//!   owner did not consent to. **That two-key property is why local-is-
//!   authoritative is the robust answer and not merely the conventional one.**
//!
//! ## Making divergence DETECTABLE rather than silent
//!
//! A mirror that is only ever written is a mirror nobody can check, which is
//! the shape the attach one has. This module reads back:
//!
//! * the `PUT` echoes what coord stored; [`mirror_create_preference_to_coord`]
//!   compares it with what was sent and reports a mismatch rather than
//!   assuming a 200 means agreement;
//! * [`verify_create_preference_against_coord`] then `GET`s the column and
//!   compares it with the LOCAL value — a separate read, so a mirror that
//!   silently no-ops is caught;
//! * the verdict is recorded in a process-global [`last_mirror_outcome`] and
//!   surfaced by `remote_create_preference_get`, so the divergence is visible
//!   to an operator and not only to a log line nobody greps.
//!
//! A `503 create_preference_unavailable` is its own outcome
//! ([`MirrorOutcome::NotStored`]) and NOT a divergence: coord is telling the
//! truth about a column its database does not have yet. Recording it as
//! agreement would be the same dishonesty the 503 exists to avoid.

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use super::CommandResponse;
use crate::mcp::remote_terminal::{EndGrant, EndOutcome, EndReply};
use crate::settings::AcceptRemoteCreate;

/// How long the attach-stage failure waits to hear whether its orphan closed. A
/// forced close answers quickly, and the target's attach-deadline reaper is the
/// backstop, so the operator's error is not held for the full `END_TIMEOUT`.
const ORPHAN_CLOSE_TIMEOUT: Duration = Duration::from_secs(30);

/// What one mirror+verify pass established about the two stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorOutcome {
    /// Coord's column and the local setting hold the same value. The only
    /// outcome that is evidence of consistency.
    Agreed(AcceptRemoteCreate),
    /// Coord answered, and its value is NOT the local one. The runner still
    /// enforces `local`; this says the mint may refuse (or, worse, admit)
    /// against a different dial until the next mirror succeeds.
    Diverged {
        local: AcceptRemoteCreate,
        coord: AcceptRemoteCreate,
    },
    /// Coord cannot store the preference: `coord.devices.accept_remote_create`
    /// is not on that database yet (the typed `503`). UNKNOWN, not agreement —
    /// coord's mint reads the default (`off`) meanwhile, so every remote create
    /// is refused whatever this runner's local dial says.
    NotStored(String),
    /// Coord was not reachable, or answered unusably. UNKNOWN.
    Unreachable(String),
}

impl MirrorOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            MirrorOutcome::Agreed(_) => "agreed",
            MirrorOutcome::Diverged { .. } => "diverged",
            MirrorOutcome::NotStored(_) => "not_stored",
            MirrorOutcome::Unreachable(_) => "unreachable",
        }
    }

    /// True only for [`MirrorOutcome::Agreed`]. The other three are UNKNOWN or
    /// worse — none of them is evidence the two stores match.
    pub fn is_consistent(&self) -> bool {
        matches!(self, MirrorOutcome::Agreed(_))
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            MirrorOutcome::Agreed(v) => json!({
                "state": "agreed",
                "consistent": true,
                "value": v.as_str(),
            }),
            MirrorOutcome::Diverged { local, coord } => json!({
                "state": "diverged",
                "consistent": false,
                "local": local.as_str(),
                "coord": coord.as_str(),
                "enforced": local.as_str(),
                "detail": "this runner enforces its LOCAL value; coord's mint gate reads its own \
                           column, so the two disagree until the next mirror succeeds",
            }),
            MirrorOutcome::NotStored(detail) => json!({
                "state": "not_stored",
                "consistent": false,
                "detail": detail,
            }),
            MirrorOutcome::Unreachable(detail) => json!({
                "state": "unreachable",
                "consistent": false,
                "detail": detail,
            }),
        }
    }
}

/// The last mirror verdict, for the operator-facing read. `None` means no
/// mirror has run in this process yet — UNKNOWN, never "consistent".
static LAST_MIRROR: OnceLock<Mutex<Option<MirrorOutcome>>> = OnceLock::new();

fn last_mirror_cell() -> &'static Mutex<Option<MirrorOutcome>> {
    LAST_MIRROR.get_or_init(|| Mutex::new(None))
}

pub fn last_mirror_outcome() -> Option<MirrorOutcome> {
    last_mirror_cell().lock().ok().and_then(|g| g.clone())
}

fn record_mirror_outcome(outcome: MirrorOutcome) {
    if let MirrorOutcome::Diverged { local, coord } = &outcome {
        warn!(
            local = local.as_str(),
            coord = coord.as_str(),
            "remote create: PREFERENCE DIVERGENCE — coord's accept_remote_create is not this \
             device's local setting; this runner enforces the local one and coord's mint gate \
             reads its own column"
        );
    }
    if let Ok(mut g) = last_mirror_cell().lock() {
        *g = Some(outcome);
    }
}

/// Coord's body from `GET`/`PUT /coord/devices/me/create-preference`.
#[derive(Debug, Clone, Deserialize)]
struct CreatePreferenceBody {
    accept_remote_create: String,
    #[serde(default)]
    #[allow(dead_code)] // `"column"` | `"default"`; read for the log, not the decision
    source: Option<String>,
}

/// The coord HTTP base this runner talks to. Shares
/// [`super::remote_attach::coord_base_for`] rather than re-deriving it — one
/// resolution, so the two preferences can never mirror to different coords.
pub use super::remote_attach::coord_base_for;

/// `PUT /coord/devices/me/create-preference`, then CHECK the echo.
///
/// A `200` alone is not evidence: coord echoes the value it stored, so a body
/// naming something other than what was sent is a divergence the caller must
/// see rather than a success. A `503` is [`MirrorOutcome::NotStored`] — the
/// column is absent on that database, which coord says plainly and this must
/// not launder into agreement.
pub async fn mirror_create_preference_to_coord(
    coord_base: &str,
    pref: AcceptRemoteCreate,
) -> MirrorOutcome {
    let Some(http) = crate::coord_http::coord_client() else {
        return MirrorOutcome::Unreachable("coord HTTP client unavailable".to_string());
    };
    let url = format!(
        "{}/coord/devices/me/create-preference",
        coord_base.trim_end_matches('/')
    );
    let resp = match crate::coord_http::coord_put(http, &url)
        .timeout(Duration::from_secs(10))
        .json(&json!({ "accept_remote_create": pref.as_str() }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return MirrorOutcome::Unreachable(format!("PUT {url}: {e}")),
    };
    let status = resp.status();
    if status.as_u16() == 503 {
        let body = resp.text().await.unwrap_or_default();
        return MirrorOutcome::NotStored(format!(
            "coord cannot store accept_remote_create yet (503): {}",
            body.chars().take(300).collect::<String>()
        ));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return MirrorOutcome::Unreachable(format!(
            "PUT {url} answered {}: {}",
            status.as_u16(),
            body.chars().take(300).collect::<String>()
        ));
    }
    match resp.json::<CreatePreferenceBody>().await {
        Ok(body) => classify_echo(pref, &body.accept_remote_create),
        Err(e) => MirrorOutcome::Unreachable(format!("decode create-preference echo: {e}")),
    }
}

/// Compare what coord says it holds with the local value. Pure, so every arm
/// is a unit test rather than a network-gated one.
pub fn classify_echo(local: AcceptRemoteCreate, coord_value: &str) -> MirrorOutcome {
    match AcceptRemoteCreate::from_wire(coord_value) {
        Some(coord) if coord == local => MirrorOutcome::Agreed(local),
        Some(coord) => MirrorOutcome::Diverged { local, coord },
        // Outside the vocabulary. Coord's own CHECK makes this impossible, so
        // it means something other than coord answered — UNKNOWN, and
        // emphatically not agreement.
        None => MirrorOutcome::Unreachable(format!(
            "coord answered accept_remote_create={coord_value:?}, which is not one of \
             off | same_user | tenant"
        )),
    }
}

/// `GET /coord/devices/me/create-preference` and compare with the LOCAL value.
///
/// The independent half: the `PUT` echo proves coord accepted what it was
/// sent, this proves the column still reads that way on a fresh request. A
/// mirror that silently no-ops passes the first check and fails this one.
pub async fn verify_create_preference_against_coord(
    coord_base: &str,
    local: AcceptRemoteCreate,
) -> MirrorOutcome {
    let Some(http) = crate::coord_http::coord_client() else {
        return MirrorOutcome::Unreachable("coord HTTP client unavailable".to_string());
    };
    let url = format!(
        "{}/coord/devices/me/create-preference",
        coord_base.trim_end_matches('/')
    );
    let resp = match crate::coord_http::coord_get(http, &url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return MirrorOutcome::Unreachable(format!("GET {url}: {e}")),
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return MirrorOutcome::Unreachable(format!(
            "GET {url} answered {}: {}",
            status.as_u16(),
            body.chars().take(300).collect::<String>()
        ));
    }
    match resp.json::<CreatePreferenceBody>().await {
        Ok(body) => classify_read(local, &body.accept_remote_create, body.source.as_deref()),
        Err(e) => MirrorOutcome::Unreachable(format!("decode create-preference read: {e}")),
    }
}

/// Classify a `GET` read. Pure, so the `source` arm is a unit test.
///
/// **`source: "default"` is [`MirrorOutcome::NotStored`] UNCONDITIONALLY**, and
/// that is the whole subtlety. It means coord served the column's default
/// because the column is absent or the row holds `NULL` — it is not reporting a
/// stored value at all. When the local dial is also `off` (the common case,
/// since `off` is the default on both sides) the two strings MATCH, and reading
/// that as agreement would report "the two stores hold the same value" from a
/// store holding no value. The whole point of this module is not to do that.
pub fn classify_read(
    local: AcceptRemoteCreate,
    coord_value: &str,
    source: Option<&str>,
) -> MirrorOutcome {
    if source == Some("default") {
        return MirrorOutcome::NotStored(format!(
            "coord serves accept_remote_create={coord_value} from the DEFAULT \
             (source=\"default\"), not from a stored value — the column is absent or the row is \
             NULL. This device's local setting is {}, and a matching string here would not be \
             agreement.",
            local.as_str()
        ));
    }
    classify_echo(local, coord_value)
}

/// Mirror, then independently verify, then RECORD the verdict. This is the
/// reconciliation seam: it runs after a save and on every relay connect, so a
/// save-time failure is retried and a silent drift is caught within one
/// reconnect.
pub async fn reconcile_create_preference(
    coord_base: &str,
    pref: AcceptRemoteCreate,
) -> MirrorOutcome {
    let put = mirror_create_preference_to_coord(coord_base, pref).await;
    // Only a successful write is worth reading back; the other arms already
    // carry the reason and a GET would only restate it.
    let outcome = match &put {
        MirrorOutcome::Agreed(_) => verify_create_preference_against_coord(coord_base, pref).await,
        _ => put,
    };
    match &outcome {
        MirrorOutcome::Agreed(v) => info!(
            preference = v.as_str(),
            "remote create: preference mirrored to coord and verified by read-back"
        ),
        MirrorOutcome::NotStored(detail) => info!(
            detail = %detail,
            "remote create: coord cannot store the preference yet — its mint reads the default \
             (off), so remote create stays refused until the migration lands"
        ),
        MirrorOutcome::Unreachable(detail) => warn!(
            detail = %detail,
            "remote create: preference mirror to coord failed (best-effort; retried on the next \
             relay connect)"
        ),
        MirrorOutcome::Diverged { .. } => { /* warned in record_mirror_outcome */ }
    }
    record_mirror_outcome(outcome.clone());
    outcome
}

/// The connect-time reconcile: read the saved preference and push+verify it,
/// logging rather than raising. Called by the backend relay on its `connected`
/// ack, beside the attach one.
pub async fn mirror_create_preference_logged(coord_base: String) {
    let pref = crate::settings::get_remote_create_preference();
    reconcile_create_preference(&coord_base, pref).await;
}

/// Read the `accept_remote_create` preference AND the last mirror verdict.
///
/// The verdict is part of the answer, not decoration: the value this runner
/// enforces is only half the picture while coord's mint reads its own column,
/// and an operator who cannot see a divergence cannot fix one.
#[tauri::command]
pub fn remote_create_preference_get() -> Result<CommandResponse, String> {
    let pref = crate::settings::get_remote_create_preference();
    let mirror = last_mirror_outcome();
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(json!({
            "accept_remote_create": pref.as_str(),
            // The value THIS device enforces when a `terminal_create` arrives.
            "enforced_by": "runner",
            "coord_mirror": match &mirror {
                Some(o) => o.to_json(),
                // No mirror has run in this process: UNKNOWN, not agreement.
                None => json!({"state": "unknown", "consistent": false,
                               "detail": "no mirror has run in this process yet"}),
            },
        })),
    })
}

/// Save the `accept_remote_create` preference, then reconcile it with coord.
#[tauri::command]
pub async fn remote_create_preference_set(
    app_handle: tauri::AppHandle,
    accept_remote_create: String,
) -> Result<CommandResponse, String> {
    let pref = AcceptRemoteCreate::from_wire(&accept_remote_create).ok_or_else(|| {
        format!(
            "remote_create:invalid_preference: {accept_remote_create:?} is not one of \
             off | same_user | tenant"
        )
    })?;
    crate::settings::save_remote_create_preference(pref)?;
    info!(
        preference = pref.as_str(),
        "remote create: preference saved"
    );
    let base = coord_base_for(&app_handle);
    let outcome = reconcile_create_preference(&base, pref).await;
    Ok(CommandResponse {
        success: true,
        message: Some(match &outcome {
            MirrorOutcome::Agreed(_) => {
                "Remote-create preference saved, mirrored to coord and verified".to_string()
            }
            MirrorOutcome::Diverged { local, coord } => format!(
                "Remote-create preference saved locally as `{}`, but coord's column reads \
                     `{}` — this runner enforces the local value; retried on the next relay \
                     connect",
                local.as_str(),
                coord.as_str()
            ),
            MirrorOutcome::NotStored(_) => {
                "Remote-create preference saved locally; coord cannot store it yet (its \
                     accept_remote_create column has not landed), so coord's mint refuses every \
                     remote create meanwhile"
                    .to_string()
            }
            MirrorOutcome::Unreachable(_) => {
                "Remote-create preference saved locally; the coord mirror failed and is \
                     retried on the next relay connect"
                    .to_string()
            }
        }),
        data: Some(json!({
            "accept_remote_create": pref.as_str(),
            "mirrored": outcome.is_consistent(),
            "coord_mirror": outcome.to_json(),
        })),
    })
}

/// Force a reconcile from the UI without changing the preference — the
/// "is my device's dial actually what coord thinks it is?" button.
#[tauri::command]
pub async fn remote_create_preference_reconcile(
    app_handle: tauri::AppHandle,
) -> Result<CommandResponse, String> {
    let pref = crate::settings::get_remote_create_preference();
    let base = coord_base_for(&app_handle);
    let outcome = reconcile_create_preference(&base, pref).await;
    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Remote-create preference reconcile: {}",
            outcome.as_str()
        )),
        data: Some(json!({
            "accept_remote_create": pref.as_str(),
            "coord_mirror": outcome.to_json(),
        })),
    })
}

// ---------------------------------------------------------------------------
// SOURCE role — "New remote terminal" (Phase 5)
// ---------------------------------------------------------------------------

/// A typed remote-create failure, returned to the frontend as an OBJECT rather
/// than a string.
///
/// **This is a deliverable, not ergonomics.** `accept_remote_create` defaults
/// `off` on every device, so the very first thing an operator meets is a
/// refusal — and a refusal flattened into one red line reads as a bug. Coord's
/// own `403` body already names the preference, the route that sets it, the
/// values that admit and a paragraph of prose ([`create_grants::create_forbidden`]
/// on the coord side); the target's own refusal frame names its local dial the
/// same way. Carrying `detail` through verbatim is what lets the picker render
/// "here is the switch and here is how to throw it" instead of "failed".
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreateError {
    /// Which step failed — `mint` (coord would not issue a grant), `create`
    /// (the relay or the target refused the spawn) or `attach` (the terminal
    /// EXISTS and the tab could not be opened onto it).
    pub stage: &'static str,
    /// The stable machine code the UI branches on.
    pub code: String,
    /// One human sentence. Always populated; never the only thing rendered.
    pub message: String,
    /// The refusing party's own structured body, verbatim and unflattened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    /// Set only on a `stage: "attach"` failure: the terminal the target DID
    /// spawn and this runner cannot say is gone. Reported because the operator
    /// now has a PTY running on another machine that this window is not showing
    /// them, and hiding that would be the dishonest half of the failure. Cleared
    /// once the target reports the terminal gone; the picker renders this field
    /// as "a terminal IS running on that machine".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_terminal_id: Option<String>,
}

impl RemoteCreateError {
    fn mint(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: "mint",
            code: code.into(),
            message: message.into(),
            detail: None,
            created_terminal_id: None,
        }
    }
}

/// Coord's `201` from `POST /coord/devices/{device_id}/create-grants`. Note
/// what is absent and cannot be present: any session field. A create grant
/// addresses a DEVICE, and the session it will produce does not exist yet.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateGrantResponse {
    pub grant: String,
    pub grant_jti: String,
    #[serde(default)]
    pub target_device_id: Option<String>,
    #[serde(default)]
    pub expires_at: Option<serde_json::Value>,
    /// See [`super::remote_attach::TargetRunner`]. Absent from an older coord.
    #[serde(default)]
    pub target_runner: Option<super::remote_attach::TargetRunner>,
}

/// Turn a non-2xx from the mint into a typed refusal, PRESERVING coord's body.
///
/// Pure, so the shape the picker renders is a unit test rather than something
/// only a live 403 can show. The `code` is `error` when coord names no reason
/// and `error:reason` when it does — `create_forbidden:preference_off` being
/// the one every fresh target answers.
pub fn classify_mint_refusal(status: u16, body: &str) -> RemoteCreateError {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let error = parsed
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let reason = parsed
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let code = match (error.is_empty(), reason.is_empty()) {
        (false, false) => format!("{error}:{reason}"),
        (false, true) => error.to_string(),
        // No typed body at all — a proxy, a gateway, a coord that fell over.
        // The status is then the only fact, and it is reported as one.
        _ => format!("coord_http_{status}"),
    };
    let message = parsed
        .get("hint")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!("coord refused to mint a create grant ({status}) and gave no explanation")
        });
    RemoteCreateError {
        stage: "mint",
        code,
        message,
        detail: if parsed.is_null() {
            // Not JSON. Keep the raw text rather than dropping the only
            // evidence there is — bounded, because a proxy error page is not
            // something to paste whole into a picker.
            Some(json!({ "status": status, "body": body.chars().take(600).collect::<String>() }))
        } else {
            Some(parsed)
        },
        created_terminal_id: None,
    }
}

/// `POST /coord/devices/{target}/create-grants` — mint one single-use create
/// grant. There is no request body: coord takes the source from the verified
/// device principal and the target from the path.
///
/// `scope` is the tenant the create is made IN — the one the Fleet view listed
/// the device under. The attach mint that follows presents the same one.
async fn mint_create_grant(
    coord_base: &str,
    target_device_id: uuid::Uuid,
    scope: crate::auth::TenantScope,
) -> Result<CreateGrantResponse, RemoteCreateError> {
    let Some(http) = crate::coord_http::coord_client() else {
        return Err(RemoteCreateError::mint(
            "coord_client_unavailable",
            "the shared coord HTTP client failed to build — this runner cannot reach coord",
        ));
    };
    let url = format!(
        "{}/coord/devices/{}/create-grants",
        coord_base.trim_end_matches('/'),
        target_device_id
    );
    let resp = crate::coord_http::coord_post_for(http, &url, scope)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| {
            RemoteCreateError::mint(
                "coord_unreachable",
                format!("POST {url} did not answer: {e}"),
            )
        })?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(classify_mint_refusal(status.as_u16(), &body));
    }
    serde_json::from_str(&body).map_err(|e| {
        RemoteCreateError::mint(
            "coord_parse",
            format!("could not decode coord's create-grant response: {e}"),
        )
    })
}

/// `terminal_create_remote` — **the button.**
///
/// Spawn a terminal on another fleet device and open a tab attached to it, in
/// one operator action. Four steps, each of which can refuse in its own voice:
///
/// 1. **Mint** a single-use CREATE grant from coord for `device_id`. Gated on
///    that device's `accept_remote_create`, which is `off` by default.
/// 2. **Present** it as `remote_terminal_create` through the relay, which
///    forwards it to the target as `terminal_create`.
/// 3. **Consume** the `remote_terminal_created` reply. The target chose the
///    directory; it also registered the new PTY as a coord session and says
///    which one.
/// 4. **Attach** — mint an ATTACH grant for that session and open the tab
///    through the ordinary [`super::remote_attach::open_remote_tab`] path, so
///    the operator lands IN the terminal rather than being told one exists.
///
/// **The caller never supplies a path.** `working_dir_key` is at most a LABEL
/// into the set the target offers, and `intent_repo` a member of the target's
/// own allowlist; the target answers both out of its own configuration and
/// refuses anything else. A caller-chosen working directory is the hole this
/// plan's D2 closed and this door does not re-open it.
///
/// An attach-stage failure leaves an orphan on the target, which this closes in
/// one round trip under the same create grant
/// ([`close_orphan_on_attach_failure`]).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn terminal_create_remote(
    terminal_manager: tauri::State<'_, Arc<crate::terminal::TerminalManager>>,
    app_handle: tauri::AppHandle,
    device_id: String,
    device_label: Option<String>,
    title: Option<String>,
    working_dir_key: Option<String>,
    intent_repo: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    page_id: Option<String>,
    tenant: Option<String>,
) -> Result<crate::terminal::types::RemoteTerminalInfo, RemoteCreateError> {
    let target = uuid::Uuid::parse_str(device_id.trim()).map_err(|e| {
        RemoteCreateError::mint(
            "invalid_device_id",
            format!("{device_id:?} is not a device uuid: {e}"),
        )
    })?;
    // The tenant both mints present — the one the Fleet view listed the device
    // under, by the rule that read used. Refused before any request.
    let scope = super::remote_attach::grant_scope_or_refusal(tenant)
        .await
        .map_err(|e| {
            let code = e.split(':').next().unwrap_or("tenant_refused").to_string();
            RemoteCreateError::mint(code, e)
        })?;
    let cols = cols.unwrap_or(120);
    let rows = rows.unwrap_or(30);
    let base = coord_base_for(&app_handle);

    // 1. Mint.
    let minted = mint_create_grant(&base, target, scope).await?;
    info!(
        target_device = %target,
        grant_jti = %minted.grant_jti,
        expires_at = ?minted.expires_at,
        "remote create: grant minted; presenting through the relay"
    );

    // 2/3. Present, and wait for the target to spawn.
    let created = match crate::mcp::remote_terminal::client()
        .create(
            &minted.grant,
            cols,
            rows,
            title.as_deref(),
            working_dir_key.as_deref(),
            intent_repo.as_deref(),
            crate::mcp::remote_terminal::CREATE_TIMEOUT,
        )
        .await
    {
        Ok(created) => created,
        Err(e) => {
            // Against a coord carrying the target-runner check, a minted create
            // grant is always `supports` (coord refuses `unknown` for create), so
            // the `unknown` arm of the explanation only serves an older coord.
            // This runner's own relay is sampled NOW, when the timeout fired.
            let message = if e.code == "timeout" {
                let source_relay = super::remote_attach::SourceRelay::sample(&app_handle).await;
                format!(
                    "{} The grant is single-use; if the target did spawn a terminal it is \
                     running there unattached.",
                    super::remote_attach::explain_relay_timeout(
                        "remote_terminal_created",
                        &target.to_string(),
                        minted.target_runner.as_ref(),
                        &source_relay,
                        crate::mcp::remote_terminal::CREATE_TIMEOUT.as_secs(),
                    )
                )
            } else {
                e.message.clone()
            };
            return Err(RemoteCreateError {
                stage: "create",
                code: e.code.clone(),
                message,
                detail: Some(json!({
                    "grantJti": minted.grant_jti,
                    "targetDeviceId": target.to_string(),
                })),
                created_terminal_id: None,
            });
        }
    };
    info!(
        target_device = %target,
        grant_jti = %created.grant_jti,
        remote_terminal_id = %created.terminal_id,
        coord_session = ?created.coord_session_id,
        working_dir = ?created.working_dir,
        "remote create: the target spawned a terminal"
    );

    // 4. Attach.
    let attached = attach_created_terminal(
        terminal_manager.inner(),
        &app_handle,
        &base,
        target,
        scope,
        &created,
        cols,
        rows,
        page_id,
        device_label,
    )
    .await;
    let grant = minted.grant.as_str();
    close_orphan_on_attach_failure(attached, |terminal_id| async move {
        crate::mcp::remote_terminal::client()
            .end(
                EndGrant::Create {
                    grant,
                    terminal_id: &terminal_id,
                },
                true,
                ORPHAN_CLOSE_TIMEOUT,
            )
            .await
    })
    .await
}

/// A create grant bought ONE spawn and the relay dropped it the moment it
/// forwarded the reply, so driving the new PTY needs an attach grant — minted
/// against the session the TARGET registered. Every `Err` here carries
/// `created_terminal_id`: the terminal exists.
#[allow(clippy::too_many_arguments)]
async fn attach_created_terminal(
    terminal_manager: &Arc<crate::terminal::TerminalManager>,
    app_handle: &tauri::AppHandle,
    base: &str,
    target: uuid::Uuid,
    scope: crate::auth::TenantScope,
    created: &crate::mcp::remote_terminal::CreatedReply,
    cols: u16,
    rows: u16,
    page_id: Option<String>,
    device_label: Option<String>,
) -> Result<crate::terminal::types::RemoteTerminalInfo, RemoteCreateError> {
    let Some(session_id) = created.coord_session_id.as_deref() else {
        return Err(RemoteCreateError {
            stage: "attach",
            code: "no_coord_session".to_string(),
            message: format!(
                "The terminal WAS created on the remote device (terminal {}), but it reported no \
                 coord session id — so no attach grant can be minted for it and this window \
                 cannot open a tab onto it. The remote runner's build may predate remote-create \
                 session registration, or its coord registration failed; its own log says which.",
                created.terminal_id
            ),
            detail: Some(json!({
                "targetDeviceId": target.to_string(),
                "remoteTerminalId": created.terminal_id,
                "workingDir": created.working_dir,
            })),
            created_terminal_id: Some(created.terminal_id.clone()),
        });
    };
    let session_uuid = uuid::Uuid::parse_str(session_id.trim()).map_err(|e| RemoteCreateError {
        stage: "attach",
        code: "invalid_coord_session".to_string(),
        message: format!(
            "The terminal was created, but the remote device reported {session_id:?} as its coord \
             session id, which is not a uuid: {e}"
        ),
        detail: None,
        created_terminal_id: Some(created.terminal_id.clone()),
    })?;

    // The target registers through the session registry's OUTBOX, so coord may
    // not have drained the row yet; this retries a `session_not_found` and
    // nothing else.
    let attach_grant =
        super::remote_attach::mint_attach_grant_awaiting_session(base, session_uuid, scope)
            .await
            .map_err(|e| RemoteCreateError {
                stage: "attach",
                // `remote_attach:<code>[:<reason>]: detail` — keep the code, which
                // is what tells `attach_forbidden:preference_off` (the OTHER dial,
                // `accept_remote_attach`, is also off on that device) apart from a
                // transport failure.
                code: e
                    .strip_prefix("remote_attach:")
                    .and_then(|rest| rest.split(':').next())
                    .unwrap_or("attach_grant_mint_failed")
                    .to_string(),
                message: format!(
                "The terminal WAS created on the remote device (terminal {}), but no attach grant \
                 could be minted for the session it registered, so this window cannot open a tab \
                 onto it: {e}",
                created.terminal_id
            ),
                detail: Some(json!({
                    "targetDeviceId": target.to_string(),
                    "remoteTerminalId": created.terminal_id,
                    "coordSessionId": session_uuid.to_string(),
                    "raw": e,
                })),
                created_terminal_id: Some(created.terminal_id.clone()),
            })?;

    // COORD places the session, not the relay. `terminal_attach_remote` has
    // always refused a `target_device_id` that disagrees with the device the
    // operator picked; the create-then-attach path called the same mint, got
    // the same field, and DISCARDED it — on the path where the session id
    // arrived over the untrusted relay (review finding 3).
    //
    // The scenario the check closes: the operator clicks New terminal on B, a
    // relay rewrites `coord_session_id` on `remote_terminal_created` to a
    // session living on C, and this window mints an attach grant for C's
    // session, labels the tab B, and types into C's live agent session. Coord's
    // answer is the only authority for where a session runs, and here it
    // disagrees with the device this create was addressed to.
    let asked = target.to_string();
    if !super::remote_attach::coord_places_session_on(
        &asked,
        attach_grant.target_device_id.as_deref(),
    ) {
        let placed = attach_grant
            .target_device_id
            .as_deref()
            .unwrap_or("<unreported>");
        return Err(RemoteCreateError {
            stage: "attach",
            code: "target_mismatch".to_string(),
            message: format!(
                "The terminal WAS created on the remote device (terminal {}), but coord places \
                 the session it reported ({session_uuid}) on device {placed}, not on {asked} — \
                 the session id this device was told about does not belong to the device the \
                 create was sent to, so no tab is opened onto it.",
                created.terminal_id
            ),
            detail: Some(json!({
                "targetDeviceId": asked,
                "coordPlacesSessionOn": placed,
                "remoteTerminalId": created.terminal_id,
                "coordSessionId": session_uuid.to_string(),
            })),
            created_terminal_id: Some(created.terminal_id.clone()),
        });
    }

    super::remote_attach::open_remote_tab(
        terminal_manager,
        app_handle,
        super::remote_attach::OpenRemoteTab {
            minted: attach_grant,
            device_id: target.to_string(),
            session_uuid,
            cols,
            rows,
            page_id,
            device_label,
            session_label: created.title.clone(),
            working_dir: created.working_dir.clone(),
            tenant: super::fleet_sessions::scope_tenant(scope),
        },
    )
    .await
    .map_err(|e| RemoteCreateError {
        stage: "attach",
        code: "tab_open_failed".to_string(),
        message: format!(
            "The terminal WAS created on the remote device (terminal {}) and a grant was minted, \
             but the tab could not be opened: {e}",
            created.terminal_id
        ),
        detail: Some(json!({
            "targetDeviceId": target.to_string(),
            "remoteTerminalId": created.terminal_id,
            "coordSessionId": session_uuid.to_string(),
            "raw": e,
        })),
        created_terminal_id: Some(created.terminal_id.clone()),
    })
}

/// The attach stage's failure leaves a terminal no source can reach: close it
/// through `send_close`, best-effort and once, then return the ORIGINAL error
/// with what became of the terminal. A failure with no created terminal, and a
/// success, send nothing. `send_close` is [`RemoteAttachClient::end`] under the
/// create grant, injected so this is testable without a relay.
///
/// [`RemoteAttachClient::end`]: crate::mcp::remote_terminal::RemoteAttachClient::end
async fn close_orphan_on_attach_failure<T, S, SF>(
    attached: Result<T, RemoteCreateError>,
    send_close: S,
) -> Result<T, RemoteCreateError>
where
    S: FnOnce(String) -> SF,
    SF: Future<Output = EndReply>,
{
    let error = match attached {
        Ok(attached) => return Ok(attached),
        Err(error) => error,
    };
    let Some(terminal_id) = error.created_terminal_id.clone() else {
        return Err(error);
    };
    let reply = send_close(terminal_id.clone()).await;
    info!(
        remote_terminal_id = %terminal_id,
        outcome = reply.outcome.as_str(),
        via = ?reply.via,
        reason = ?reply.reason,
        "remote create: attach failed; asked the target to close the orphan terminal"
    );
    Err(annotate_orphan_close(error, &terminal_id, &reply))
}

/// Add what the close did to `error`: `detail.orphanClose` and one sentence on
/// the message. Only `ended` and `not_found` say the terminal is gone, and only
/// they clear `created_terminal_id`; `unknown` is never rendered as closed.
fn annotate_orphan_close(
    mut error: RemoteCreateError,
    terminal_id: &str,
    reply: &EndReply,
) -> RemoteCreateError {
    let orphan_close = json!({
        "outcome": reply.outcome.as_str(),
        "via": reply.via,
        "reason": reply.reason,
    });
    match error.detail.as_mut().and_then(|d| d.as_object_mut()) {
        Some(detail) => {
            detail.insert("orphanClose".to_string(), orphan_close);
        }
        None => {
            error.detail = Some(match error.detail.take() {
                Some(original) => json!({ "original": original, "orphanClose": orphan_close }),
                None => json!({ "orphanClose": orphan_close }),
            });
        }
    }
    error.message = format!(
        "{} {}",
        error.message,
        orphan_close_sentence(terminal_id, reply)
    );
    if matches!(reply.outcome, EndOutcome::Ended | EndOutcome::NotFound) {
        error.created_terminal_id = None;
    }
    error
}

fn orphan_close_sentence(terminal_id: &str, reply: &EndReply) -> String {
    const BACKSTOP: &str =
        "It may still be running there; if nothing attaches, that device closes it itself once \
         its attach deadline passes.";
    let reason = reply.reason.as_deref().unwrap_or("no reason given");
    let attached_code = crate::mcp::remote_terminal::CreateRefusal::CloseAttached.code();
    match reply.outcome {
        EndOutcome::Ended => {
            format!("This runner then closed that terminal ({terminal_id}) on the remote device.")
        }
        EndOutcome::NotFound => format!(
            "The remote device reports that terminal ({terminal_id}) is already gone, so nothing \
             is left running."
        ),
        EndOutcome::Refused if reason.starts_with(attached_code) => format!(
            "The remote device refused to close that terminal ({terminal_id}) because something \
             has attached to it, so it was left running."
        ),
        EndOutcome::Refused => format!(
            "The remote device refused to close that terminal ({terminal_id}): {reason}. {BACKSTOP}"
        ),
        EndOutcome::StillRunning => format!(
            "That terminal ({terminal_id}) is still running on the remote device: {reason}. \
             {BACKSTOP}"
        ),
        EndOutcome::Unknown => {
            format!("Whether that terminal ({terminal_id}) closed is UNKNOWN: {reason}. {BACKSTOP}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::remote_terminal::RemoteAttachClient;

    fn attach_stage_error(detail: Option<serde_json::Value>) -> RemoteCreateError {
        RemoteCreateError {
            stage: "attach",
            code: "session_not_found".to_string(),
            message: "no attach grant could be minted.".to_string(),
            detail,
            created_terminal_id: Some("term-orphan".to_string()),
        }
    }

    async fn annotated_by(reply: EndReply) -> RemoteCreateError {
        close_orphan_on_attach_failure::<(), _, _>(
            Err(attach_stage_error(Some(
                json!({ "remoteTerminalId": "term-orphan" }),
            ))),
            |_| async move { reply },
        )
        .await
        .expect_err("the original failure is still the answer")
    }

    /// Through the REAL client: one frame, on the wire contract's shape, and the
    /// original error comes back saying the orphan closed.
    #[tokio::test]
    async fn an_attach_failure_sends_exactly_one_close_and_annotates() {
        let client = Arc::new(RemoteAttachClient::new());
        let mut pump = client.lock_outbound().await;
        let c = client.clone();
        let task = tokio::spawn(async move {
            close_orphan_on_attach_failure::<(), _, _>(
                Err(attach_stage_error(None)),
                |terminal_id| async move {
                    c.end(
                        EndGrant::Create {
                            grant: "create-jwt",
                            terminal_id: &terminal_id,
                        },
                        true,
                        ORPHAN_CLOSE_TIMEOUT,
                    )
                    .await
                },
            )
            .await
        });
        let sent = pump.recv().await.expect("the close frame");
        assert_eq!(sent["type"], "remote_terminal_end");
        assert_eq!(sent["create_grant"], "create-jwt");
        assert_eq!(sent["terminal_id"], "term-orphan");
        assert_eq!(sent["force"], true);
        assert!(client.handle_inbound(
            "remote_terminal_ended",
            &json!({
                "type": "remote_terminal_ended",
                "request_id": sent["request_id"],
                "terminal_id": "term-orphan",
                "outcome": "ended",
                "via": "force",
            }),
        ));
        let error = task.await.unwrap().expect_err("the original failure");
        assert!(pump.try_recv().is_err(), "exactly one close frame");

        assert_eq!(error.stage, "attach");
        assert_eq!(error.code, "session_not_found");
        assert!(error
            .message
            .starts_with("no attach grant could be minted."));
        assert!(error.message.contains("closed that terminal (term-orphan)"));
        let close = &error.detail.as_ref().expect("detail created")["orphanClose"];
        assert_eq!(close["outcome"], "ended");
        assert_eq!(close["via"], "force");
        assert_eq!(close["reason"], serde_json::Value::Null);
        // The picker renders this field as "a terminal IS running".
        assert_eq!(error.created_terminal_id, None);
    }

    #[tokio::test]
    async fn a_failure_with_no_created_terminal_and_a_success_send_nothing() {
        let create_stage = RemoteCreateError {
            stage: "create",
            code: "timeout".to_string(),
            message: "the create timed out".to_string(),
            detail: None,
            created_terminal_id: None,
        };
        let error = close_orphan_on_attach_failure::<(), _, _>(Err(create_stage), |_| async {
            panic!("a create-stage failure has nothing to close")
        })
        .await
        .unwrap_err();
        assert_eq!(error.message, "the create timed out");
        assert!(error.detail.is_none());

        let ok = close_orphan_on_attach_failure(Ok(7), |_| async {
            panic!("a successful attach closes nothing")
        })
        .await;
        assert_eq!(ok.unwrap(), 7);
    }

    /// `unknown` is the outcome a timeout and a dead relay land on. It says so,
    /// keeps the terminal reported as running, and never claims it closed.
    #[tokio::test]
    async fn an_unknown_reply_is_not_rendered_as_closed() {
        let error = annotated_by(EndReply::local(
            EndOutcome::Unknown,
            "timeout: no remote_terminal_ended within 30s",
        ))
        .await;
        assert!(error.message.contains("UNKNOWN"), "{}", error.message);
        assert!(error.message.contains("may still be running"));
        assert!(!error.message.contains("closed that terminal"));
        assert_eq!(error.created_terminal_id.as_deref(), Some("term-orphan"));
        let detail = error.detail.unwrap();
        assert_eq!(detail["orphanClose"]["outcome"], "unknown");
        assert_eq!(
            detail["remoteTerminalId"], "term-orphan",
            "original detail kept"
        );
    }

    #[tokio::test]
    async fn each_refusal_and_gone_outcome_says_what_it_means() {
        let attached = crate::mcp::remote_terminal::CreateRefusal::CloseAttached.code();
        let error = annotated_by(EndReply::local(
            EndOutcome::Refused,
            format!("{attached}: this terminal has been attached"),
        ))
        .await;
        assert!(error.message.contains("left running"), "{}", error.message);
        assert!(
            !error.message.contains("attach deadline"),
            "no reaper will take it"
        );
        assert_eq!(error.created_terminal_id.as_deref(), Some("term-orphan"));

        let error = annotated_by(EndReply::local(
            EndOutcome::Refused,
            "remote_create_grant_unknown",
        ))
        .await;
        assert!(
            error.message.contains("refused to close"),
            "{}",
            error.message
        );
        assert!(error.message.contains("remote_create_grant_unknown"));
        assert_eq!(error.created_terminal_id.as_deref(), Some("term-orphan"));
        assert_eq!(
            error.detail.unwrap()["orphanClose"]["reason"],
            "remote_create_grant_unknown"
        );

        let error = annotated_by(EndReply::local(EndOutcome::NotFound, "terminal_gone")).await;
        assert!(error.message.contains("already gone"), "{}", error.message);
        assert_eq!(error.created_terminal_id, None);
    }

    /// The command's own wiring is not reachable without Tauri state, so it is
    /// pinned by shape: the attach stage runs through the close, under the
    /// create grant, and the create stage returns before it.
    #[test]
    fn the_command_closes_the_orphan_under_the_create_grant_it_spent() {
        const SRC: &str = include_str!("remote_create.rs");
        let start = SRC
            .find("pub async fn terminal_create_remote(")
            .expect("the command");
        let command = SRC.get(start..).expect("char boundary");
        let end = command
            .find("\nasync fn attach_created_terminal(")
            .expect("attach_created_terminal follows the command");
        let command = command.get(..end).expect("char boundary");
        let create_stage = command.find("stage: \"create\",").expect("create stage");
        let attach = command
            .find("attach_created_terminal(")
            .expect("attach stage");
        let close = command
            .find("close_orphan_on_attach_failure(attached,")
            .expect("the close");
        let grant = command
            .find("EndGrant::Create {")
            .expect("under the create grant");
        assert!(create_stage < attach && attach < close && close < grant);
    }

    #[test]
    fn create_grant_response_reads_target_runner_when_present_and_tolerates_absence() {
        let old: CreateGrantResponse =
            serde_json::from_str(r#"{"grant":"g","grant_jti":"j"}"#).unwrap();
        assert!(old.target_runner.is_none());
        let new: CreateGrantResponse = serde_json::from_str(
            r#"{"grant":"g","grant_jti":"j","target_runner":{"state":"supports","required_sha":"abc"}}"#,
        )
        .unwrap();
        assert_eq!(new.target_runner.unwrap().state, "supports");
    }

    #[test]
    fn agreement_needs_the_same_value_and_nothing_less() {
        assert_eq!(
            classify_echo(AcceptRemoteCreate::Tenant, "tenant"),
            MirrorOutcome::Agreed(AcceptRemoteCreate::Tenant)
        );
        assert!(classify_echo(AcceptRemoteCreate::Tenant, "tenant").is_consistent());
    }

    /// **The detection this module exists for.** Coord answering `off` while
    /// the device's own setting says `tenant` is a DIVERGENCE, not a success —
    /// a `200` is not evidence the two stores match.
    #[test]
    fn a_different_value_from_coord_is_a_divergence_not_a_success() {
        let o = classify_echo(AcceptRemoteCreate::Tenant, "off");
        assert_eq!(
            o,
            MirrorOutcome::Diverged {
                local: AcceptRemoteCreate::Tenant,
                coord: AcceptRemoteCreate::Off,
            }
        );
        assert!(!o.is_consistent());
        let v = o.to_json();
        assert_eq!(v["state"], "diverged");
        assert_eq!(v["consistent"], false);
        assert_eq!(v["local"], "tenant");
        assert_eq!(v["coord"], "off");
        // The runner is the enforcing party; the report says so rather than
        // leaving an operator to guess which dial wins.
        assert_eq!(v["enforced"], "tenant");
    }

    /// A value outside the vocabulary is UNKNOWN, never agreement — coord's
    /// own CHECK makes it impossible, so seeing one means something else
    /// answered.
    #[test]
    fn an_out_of_vocabulary_answer_is_unknown_not_agreement() {
        for bogus in ["", "everyone", "Off", "same-user", "null"] {
            let o = classify_echo(AcceptRemoteCreate::SameUser, bogus);
            assert!(
                matches!(o, MirrorOutcome::Unreachable(_)),
                "{bogus:?} must be UNKNOWN, got {o:?}"
            );
            assert!(!o.is_consistent());
        }
    }

    /// Every non-agreed outcome reports `consistent: false`. Nothing but a
    /// verified match may read as consistency.
    #[test]
    fn only_agreement_reads_as_consistent() {
        for o in [
            MirrorOutcome::Diverged {
                local: AcceptRemoteCreate::Off,
                coord: AcceptRemoteCreate::Tenant,
            },
            MirrorOutcome::NotStored("column absent".into()),
            MirrorOutcome::Unreachable("coord down".into()),
        ] {
            assert!(!o.is_consistent(), "{o:?}");
            assert_eq!(o.to_json()["consistent"], false, "{o:?}");
        }
        assert!(MirrorOutcome::Agreed(AcceptRemoteCreate::Off).is_consistent());
    }

    /// **A coord that stores nothing never reads as agreement — not even when
    /// the strings match.**
    ///
    /// `off` is the default on BOTH sides, so a coord serving `off` from the
    /// column's default while the device's dial is also `off` produces two
    /// identical strings out of a store that holds no value. A
    /// same-value-means-agreed rule reports that as consistency, which is
    /// exactly the dishonesty the typed 503 exists to avoid.
    #[test]
    fn a_default_sourced_read_is_not_stored_even_when_the_values_match() {
        let o = classify_read(AcceptRemoteCreate::Off, "off", Some("default"));
        assert!(
            matches!(o, MirrorOutcome::NotStored(_)),
            "matching strings from a DEFAULT source are not agreement; got {o:?}"
        );
        assert!(!o.is_consistent());
        // …and the same for a mismatch, and for the other two values.
        for (local, served) in [
            (AcceptRemoteCreate::Tenant, "off"),
            (AcceptRemoteCreate::SameUser, "same_user"),
            (AcceptRemoteCreate::Tenant, "tenant"),
        ] {
            assert!(
                matches!(
                    classify_read(local, served, Some("default")),
                    MirrorOutcome::NotStored(_)
                ),
                "{local:?}/{served}"
            );
        }
        // A COLUMN-sourced read is judged on the value, as before.
        assert!(classify_read(AcceptRemoteCreate::Off, "off", Some("column")).is_consistent());
        assert!(classify_read(AcceptRemoteCreate::Off, "off", None).is_consistent());
        assert_eq!(
            classify_read(AcceptRemoteCreate::Off, "tenant", Some("column")),
            MirrorOutcome::Diverged {
                local: AcceptRemoteCreate::Off,
                coord: AcceptRemoteCreate::Tenant,
            }
        );
    }

    /// **The OFF-by-default refusal is the first thing every operator meets,
    /// and it must arrive with the switch attached.**
    ///
    /// This is the discriminating test: it asserts the refusal carries coord's
    /// WHOLE body through — `preference`, `preference_route`,
    /// `preference_allowed` and the `hint` prose — not merely a code and a
    /// status. A classifier that produced the right `code` and dropped
    /// `detail`, or that wrote its own message instead of coord's `hint`,
    /// passes every "did it fail?" assertion and still leaves the picker with
    /// nothing to tell the operator to go and change.
    #[test]
    fn the_off_by_default_refusal_carries_the_switch_not_just_the_code() {
        let body = json!({
            "error": "create_forbidden",
            "reason": "preference_off",
            "target_device_id": "11111111-1111-1111-1111-111111111111",
            "preference": "accept_remote_create",
            "preference_route": "PUT /coord/devices/me/create-preference",
            "preference_default": "off",
            "preference_allowed": ["off", "same_user", "tenant"],
            "hint": "Remote terminal CREATION is OFF on the target device — \
                     `accept_remote_create` reads `off`, which is the default until someone \
                     opts in.",
        })
        .to_string();
        let e = classify_mint_refusal(403, &body);

        assert_eq!(e.stage, "mint");
        // The reason is part of the code: `create_forbidden` alone cannot tell
        // "the dial is off" from "you are the wrong user".
        assert_eq!(e.code, "create_forbidden:preference_off");
        // coord's prose IS the message. Substituting our own would restate a
        // sentence coord keeps current.
        assert!(
            e.message.contains("accept_remote_create") && e.message.contains("off"),
            "the message must be coord's own hint, got {:?}",
            e.message
        );
        let detail = e.detail.expect("coord's body must reach the UI");
        for key in [
            "preference",
            "preference_route",
            "preference_allowed",
            "preference_default",
            "target_device_id",
        ] {
            assert!(
                detail.get(key).is_some(),
                "{key} must survive to the UI — it is how the operator finds the switch; got \
                 {detail}"
            );
        }
        assert_eq!(
            detail["preference_route"],
            "PUT /coord/devices/me/create-preference"
        );
        assert_eq!(
            detail["preference_allowed"],
            json!(["off", "same_user", "tenant"])
        );
        assert!(e.created_terminal_id.is_none());
    }

    /// A `same_user` refusal is a DIFFERENT code from an `off` one, so the UI
    /// can offer a different remedy (call from the paired user vs. widen the
    /// dial). Collapsing both to `create_forbidden` would send an operator to
    /// change a setting that is already correct.
    #[test]
    fn each_refusal_reason_keeps_its_own_code() {
        for reason in ["preference_off", "different_user", "cross_tenant"] {
            let body = json!({"error": "create_forbidden", "reason": reason}).to_string();
            assert_eq!(
                classify_mint_refusal(403, &body).code,
                format!("create_forbidden:{reason}")
            );
        }
    }

    /// A body that is not coord's — a proxy 502, an HTML error page, an empty
    /// answer — is reported as the STATUS it was, with the bytes kept. It must
    /// never be laundered into a `create_forbidden`, which would send the
    /// operator to a preference that had nothing to do with it.
    #[test]
    fn a_non_coord_body_is_reported_as_a_status_not_as_a_preference_refusal() {
        for (status, body) in [
            (502u16, "<html>bad gateway</html>"),
            (500, ""),
            (404, "not found"),
        ] {
            let e = classify_mint_refusal(status, body);
            assert_eq!(e.code, format!("coord_http_{status}"), "body {body:?}");
            assert!(!e.code.contains("preference"));
            assert_eq!(
                e.detail.as_ref().and_then(|d| d.get("status")),
                Some(&json!(status))
            );
        }
        // A JSON body naming an error but no reason keeps the error alone.
        let e = classify_mint_refusal(404, r#"{"error":"device_not_found"}"#);
        assert_eq!(e.code, "device_not_found");
    }

    /// The state tokens are the wire contract for the UI; keep them stable.
    #[test]
    fn outcome_state_tokens_are_stable() {
        assert_eq!(
            MirrorOutcome::Agreed(AcceptRemoteCreate::Off).as_str(),
            "agreed"
        );
        assert_eq!(
            MirrorOutcome::Diverged {
                local: AcceptRemoteCreate::Off,
                coord: AcceptRemoteCreate::Off
            }
            .as_str(),
            "diverged"
        );
        assert_eq!(
            MirrorOutcome::NotStored(String::new()).as_str(),
            "not_stored"
        );
        assert_eq!(
            MirrorOutcome::Unreachable(String::new()).as_str(),
            "unreachable"
        );
    }

    /// The recorder holds the last verdict, which is what the operator-facing
    /// read reports. Before any mirror runs it is `None` — UNKNOWN.
    #[test]
    fn the_last_outcome_is_recorded_for_the_operator_read() {
        record_mirror_outcome(MirrorOutcome::Diverged {
            local: AcceptRemoteCreate::Tenant,
            coord: AcceptRemoteCreate::Off,
        });
        let got = last_mirror_outcome().expect("recorded");
        assert_eq!(got.as_str(), "diverged");
        record_mirror_outcome(MirrorOutcome::Agreed(AcceptRemoteCreate::Tenant));
        assert!(last_mirror_outcome().unwrap().is_consistent());
    }
}
