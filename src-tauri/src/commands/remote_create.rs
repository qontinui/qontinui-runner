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

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use super::CommandResponse;
use crate::settings::AcceptRemoteCreate;

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

#[cfg(test)]
mod tests {
    use super::*;

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
