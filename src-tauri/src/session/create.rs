//! Remote-CREATE grants — the coord → target-runner feed (plan
//! `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase 3b).
//!
//! The exact twin of [`super::attach`], and deliberately so: remote attach
//! already had this shape and remote create shipped without it, which left the
//! target trusting the web relay's word that a create grant existed. Two
//! mechanisms for one concept is what this plan has been correcting, so this
//! module mirrors the attach one mechanism for mechanism rather than inventing
//! a second idiom.
//!
//! Coord mints a create grant on `POST /coord/devices/{id}/create-grants` and
//! tells the TARGET device two ways:
//!
//! 1. **Push** — a `create_request` directive on
//!    `qontinui.sessions.<tenant>.<target-device>.create_request`, i.e. the
//!    subject family [`super::handoff`] already PSUBSCRIBEs as
//!    `qontinui.sessions.*`. No second socket:
//!    [`super::handoff::connect_and_pump`] forwards every frame here and
//!    [`parse_create_push`] claims only the `.create_request` suffix — the same
//!    disambiguation-by-trailing-segment the handoff, respawn and attach arms
//!    use.
//! 2. **Catch-up** — `GET /sessions/create-requests?device_id=<me>`, device-
//!    bound exactly like `GET /sessions/attach-requests`. Run on every coord WS
//!    (re)connect, on every backend-relay `connected` ack, and on a 60 s timer,
//!    so a grant minted while a socket was down still reaches the table before
//!    its 15-minute life ends.
//!
//! Either way the row lands in
//! [`crate::mcp::remote_terminal::create_grants`], the table
//! `admit_terminal_create` consults before spawning a PTY. Nothing here decides
//! who may create — coord decided that when it minted, under the target
//! device's own `accept_remote_create` preference; this module only makes the
//! decision visible to the process that owns the PTY, so that process does not
//! have to believe a broker.
//!
//! **`storage: "absent"` is UNKNOWN, never "no grants".** A coord whose
//! `coord.create_grants` table has not landed can only push, so a frame whose
//! jti never arrived is still refused — the fail-closed direction. The absence
//! of a durable list is not licence to admit.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use uuid::Uuid;

use super::handoff::HandoffError;
use super::SessionRegistry;
use crate::mcp::remote_terminal::{create_grants, now_epoch_secs, CreateGrant};

/// Catch-up poll cadence. Grants live 900 s; one minute keeps the window a
/// dropped push can hide in well inside that. Same value as the attach poll,
/// for the same reason.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// One create request, as carried in the `create_request` push payload and in
/// each row of `GET /sessions/create-requests`.
///
/// Note what is NOT here, and cannot be: no `session_id` (a create has no
/// session yet) and no `terminal_id` (the terminal is what it will make).
/// Optional fields are read as `Option` so a partial payload is UNKNOWN rather
/// than a nil UUID.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PendingCreate {
    pub grant_jti: String,
    #[serde(default)]
    pub source_device_id: Option<Uuid>,
    #[serde(default)]
    pub source_user_id: Option<Uuid>,
    #[serde(default)]
    pub target_device_id: Option<Uuid>,
    /// RFC 3339 timestamp or unix seconds — coord serializes a
    /// `DateTime<Utc>`; both spellings are accepted so a future integer form
    /// cannot silently drop every grant.
    pub expires_at: serde_json::Value,
}

impl PendingCreate {
    /// Unix seconds, or `None` when `expires_at` is unreadable — such a row is
    /// skipped with a warning rather than given an invented lifetime.
    pub fn expires_at_secs(&self) -> Option<u64> {
        match &self.expires_at {
            serde_json::Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
            serde_json::Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.timestamp().max(0) as u64),
            _ => None,
        }
    }

    /// The table row this request becomes.
    pub fn into_grant(self) -> Option<CreateGrant> {
        let expires_at = self.expires_at_secs()?;
        Some(CreateGrant {
            grant_jti: self.grant_jti,
            source_device_id: self
                .source_device_id
                .map(|u| u.to_string())
                .unwrap_or_default(),
            expires_at,
        })
    }
}

/// Envelope coord returns from `GET /sessions/create-requests`. `storage:
/// "absent"` means the `coord.create_grants` table has not landed on that coord
/// yet — the push arm is then the only feed, and an empty list is UNKNOWN
/// rather than "no grants".
#[derive(Debug, Clone, Deserialize)]
pub struct CreateListResponse {
    #[serde(default)]
    pub requests: Vec<PendingCreate>,
    #[serde(default)]
    pub storage: Option<String>,
}

/// Pure parse+filter of a coord `/ws` envelope into a [`PendingCreate`]
/// addressed to `device_id`. `None` when the frame is not a create request for
/// this device — the handoff, respawn and attach arms see the same text and
/// filter on their own suffixes.
pub(super) fn parse_create_push(text: &str, device_id: Uuid) -> Option<PendingCreate> {
    let envelope: serde_json::Value = serde_json::from_str(text).ok()?;
    let channel = envelope.get("channel").and_then(|c| c.as_str())?;

    // `machine_subject_raw` is exactly
    // `qontinui.sessions.<tenant>.<machine>.<kind>` — five segments. Checking
    // the count as well as the two ends means a channel with an extra segment
    // spliced between them (which would make `<tenant>` something other than a
    // tenant) does not pass on its endpoints alone.
    let suffix = format!(".{device_id}.create_request");
    if !channel.starts_with("qontinui.sessions.")
        || !channel.ends_with(&suffix)
        || channel.split('.').count() != 5
    {
        return None;
    }

    // Payload may be a JSON string (the Redis arm) or an inlined object.
    let payload_val = match envelope.get("payload") {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s).ok()?,
        Some(other) => other.clone(),
        None => return None,
    };

    let pending: PendingCreate = serde_json::from_value(payload_val).ok()?;

    // Defense-in-depth: the channel already filtered by device; if the body
    // names a different target, trust the address, not the body.
    if pending.target_device_id.is_some_and(|t| t != device_id) {
        return None;
    }
    Some(pending)
}

/// Record one request in the create-grant table. Returns `false` when the row
/// was unusable (no readable expiry), when the jti is one this process already
/// spent, or when the table's lock is poisoned.
///
/// The `false` is LOGGED here rather than discarded by every caller: the two
/// non-obvious causes — a spent-jti tombstone and a poisoned lock — are exactly
/// the ones a silent drop would hide.
pub fn record(pending: PendingCreate) -> bool {
    let jti = pending.grant_jti.clone();
    let source_user = pending.source_user_id;
    match pending.into_grant() {
        Some(grant) => {
            let source_device = grant.source_device_id.clone();
            let expires_at = grant.expires_at;
            let recorded = create_grants().insert(grant, now_epoch_secs());
            if recorded {
                tracing::info!(
                    grant_jti = %jti,
                    source_device = %source_device,
                    source_user = ?source_user,
                    expires_at,
                    "remote create: grant recorded"
                );
            } else {
                // `insert` names the reason; this says the row was DROPPED, so
                // a later `remote_create_grant_unknown` is explicable.
                tracing::warn!(
                    grant_jti = %jti,
                    "remote create: grant NOT recorded — a frame presenting it will be refused"
                );
            }
            recorded
        }
        None => {
            tracing::warn!(
                grant_jti = %jti,
                "remote create: request carries an unreadable expires_at — skipped"
            );
            false
        }
    }
}

/// Tell coord a grant is SPENT — `POST /sessions/create-requests/{jti}/consume`.
///
/// **The runner's own tombstone is not enough on its own.** Coord keeps serving
/// a grant in `GET /sessions/create-requests` until its `consumed_at` is set, so
/// without this call the row outlives its one use: this process refuses it (the
/// tombstone), but a restarted runner — or a second runner on the same device —
/// reads it back as pending. Best-effort and logged: a failure costs the
/// durable half of single-use, never the local half.
pub async fn notify_consumed(registry: &Arc<SessionRegistry>, grant_jti: &str) {
    let http = registry.coord_sync().http_client();
    let coord_url = registry.coord_sync().coord_url().to_string();
    let url = format!(
        "{}/sessions/create-requests/{}/consume",
        coord_url.trim_end_matches('/'),
        grant_jti
    );
    let resp = crate::coord_http::coord_post(&http, &url)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            tracing::debug!(grant_jti, "remote create: coord marked the grant consumed");
        }
        Ok(r) => {
            tracing::warn!(
                grant_jti,
                status = r.status().as_u16(),
                "remote create: coord did not mark the grant consumed — it stays in the pending \
                 list until it expires; this device still refuses it locally"
            );
        }
        Err(e) => {
            tracing::warn!(
                grant_jti,
                error = %e,
                "remote create: could not reach coord to mark the grant consumed — it stays in \
                 the pending list until it expires; this device still refuses it locally"
            );
        }
    }
}

/// Handle one inbound `/ws` frame on the create arm. Not a create request for
/// this device → ignored silently.
pub(super) fn handle_push_frame(device_id: Uuid, text: &str) {
    if let Some(pending) = parse_create_push(text, device_id) {
        record(pending);
    }
}

/// Fetch the durable pending-create list for this device.
pub(super) async fn fetch_pending(
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
) -> Result<CreateListResponse, HandoffError> {
    let url = format!(
        "{}/sessions/create-requests?device_id={}",
        coord_url.trim_end_matches('/'),
        device_id
    );
    let resp = crate::coord_http::coord_get(http, &url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| HandoffError::Http(format!("GET {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            body.chars().take(300).collect(),
        ));
    }
    resp.json::<CreateListResponse>()
        .await
        .map_err(|e| HandoffError::Parse(format!("decode create list: {e}")))
}

/// The catch-up: GET the pending list and record every row. Best-effort, same
/// posture as the attach catch-up — one WARN line on a 401/403 (pre-pairing
/// window), debug otherwise. A failure costs reachability, never admission:
/// nothing is admitted on a jti this table does not hold.
pub(super) async fn run_catchup(http: &reqwest::Client, coord_url: &str, device_id: Uuid) {
    match fetch_pending(http, coord_url, device_id).await {
        Ok(list) => {
            if list.storage.as_deref() == Some("absent") {
                tracing::debug!(
                    "remote create: coord reports create_grants storage absent — push arm is \
                     the only feed"
                );
            }
            if !list.requests.is_empty() {
                tracing::info!(
                    count = list.requests.len(),
                    "remote create: catch-up recording pending grants"
                );
            }
            for pending in list.requests {
                record(pending);
            }
            create_grants().purge_expired(now_epoch_secs());
        }
        Err(HandoffError::Status(401 | 403, _)) => {
            tracing::warn!(
                "remote create: catch-up GET unauthorized (401/403) — retrying after device \
                 pairing/auth"
            );
        }
        Err(HandoffError::Status(404, _)) => {
            tracing::debug!(
                "remote create: coord has no /sessions/create-requests route yet (404) — push \
                 arm only"
            );
        }
        Err(e) => {
            tracing::debug!(error = %e, "remote create: catch-up GET failed (push path still active)");
        }
    }
}

/// Run the catch-up once against the registry's coord — the door the backend
/// relay calls on its `connected` ack.
pub async fn catch_up_now(registry: &Arc<SessionRegistry>) {
    let http = registry.coord_sync().http_client();
    let coord_url = registry.coord_sync().coord_url().to_string();
    run_catchup(&http, &coord_url, registry.machine_id()).await;
}

/// The 60 s catch-up loop. Returns the handle so the caller can hold it for the
/// process lifetime.
pub fn start_poll_task(registry: Arc<SessionRegistry>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(POLL_INTERVAL);
        // The first tick fires immediately; the coord-WS connect catch-up
        // already covers boot, so skip it.
        tick.tick().await;
        loop {
            tick.tick().await;
            catch_up_now(&registry).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(target: Uuid, expires: serde_json::Value) -> serde_json::Value {
        json!({
            "event_kind": "create_request",
            "grant_jti": "0192a1b2-0000-7000-8000-000000000001",
            "source_device_id": Uuid::from_u128(1).to_string(),
            "source_user_id": Uuid::from_u128(2).to_string(),
            "target_device_id": target.to_string(),
            "expires_at": expires,
        })
    }

    /// A string-payload envelope on this device's `.create_request` subject
    /// parses; the same body on another device's subject, or on any of the
    /// three sibling suffixes, is ignored. The `.attach_request` case is the
    /// load-bearing one: the two arms share one socket, and an attach grant
    /// leaking into the CREATE table would make it buy a PTY spawn.
    #[test]
    fn parses_only_this_devices_create_request_suffix() {
        let device = Uuid::from_u128(10);
        let other = Uuid::from_u128(11);
        let tenant = Uuid::from_u128(30);
        let body = payload(device, json!("2030-01-01T00:00:00Z"));

        let ok = json!({
            "channel": format!("qontinui.sessions.{tenant}.{device}.create_request"),
            "payload": body.to_string(),
        });
        let parsed = parse_create_push(&ok.to_string(), device).expect("parsed");
        assert_eq!(parsed.target_device_id, Some(device));
        assert_eq!(parsed.grant_jti, "0192a1b2-0000-7000-8000-000000000001");

        for wrong in [
            format!("qontinui.sessions.{tenant}.{other}.create_request"),
            format!("qontinui.sessions.{tenant}.{device}.attach_request"),
            format!("qontinui.sessions.{tenant}.{device}.respawn_request"),
            format!("qontinui.sessions.{tenant}.{device}.handoff_request"),
            format!("qontinui.other.{tenant}.{device}.create_request"),
        ] {
            let env = json!({"channel": wrong, "payload": body.to_string()});
            assert!(
                parse_create_push(&env.to_string(), device).is_none(),
                "{wrong} must not parse"
            );
        }
    }

    /// The attach arm must not claim a create frame either — the two filters
    /// are disjoint in BOTH directions, so neither table can be fed the other's
    /// grants by a mis-stamped subject.
    #[test]
    fn the_attach_arm_does_not_claim_a_create_frame() {
        let device = Uuid::from_u128(10);
        let tenant = Uuid::from_u128(30);
        let env = json!({
            "channel": format!("qontinui.sessions.{tenant}.{device}.create_request"),
            "payload": payload(device, json!(1_900_000_000)).to_string(),
        });
        assert!(super::super::attach::parse_attach_push(&env.to_string(), device).is_none());
    }

    /// An inlined-object payload parses too, and a body whose target disagrees
    /// with the address is dropped.
    #[test]
    fn inlined_payload_and_target_mismatch() {
        let device = Uuid::from_u128(10);
        let other = Uuid::from_u128(11);
        let tenant = Uuid::from_u128(30);
        let inlined = json!({
            "channel": format!("qontinui.sessions.{tenant}.{device}.create_request"),
            "payload": payload(device, json!(1_900_000_000)),
        });
        assert!(parse_create_push(&inlined.to_string(), device).is_some());

        let mismatched = json!({
            "channel": format!("qontinui.sessions.{tenant}.{device}.create_request"),
            "payload": payload(other, json!(1_900_000_000)),
        });
        assert!(parse_create_push(&mismatched.to_string(), device).is_none());
    }

    /// RFC 3339 and integer expiries both become a grant; an unreadable one
    /// yields no grant rather than an invented lifetime.
    #[test]
    fn expiry_forms() {
        let device = Uuid::from_u128(10);
        let mk = |expires: serde_json::Value| -> PendingCreate {
            serde_json::from_value(payload(device, expires)).unwrap()
        };
        assert_eq!(
            mk(json!("2026-09-07T12:00:00Z")).expires_at_secs(),
            Some(1_788_782_400)
        );
        assert_eq!(
            mk(json!(1_788_782_400u64)).expires_at_secs(),
            Some(1_788_782_400)
        );
        assert_eq!(mk(json!("not a time")).expires_at_secs(), None);
        assert!(mk(json!("not a time")).into_grant().is_none());
        let g = mk(json!("2026-09-07T12:00:00+02:00")).into_grant().unwrap();
        assert_eq!(g.expires_at, 1_788_775_200);
        assert_eq!(g.source_device_id, Uuid::from_u128(1).to_string());
    }

    /// The list envelope's `storage` marker round-trips, and rows decode.
    #[test]
    fn list_response_decodes_storage_marker() {
        let absent: CreateListResponse =
            serde_json::from_str(r#"{"requests":[],"storage":"absent"}"#).unwrap();
        assert!(absent.requests.is_empty());
        assert_eq!(absent.storage.as_deref(), Some("absent"));
        let present: CreateListResponse = serde_json::from_value(json!({
            "requests": [payload(Uuid::from_u128(10), json!(1_900_000_000))],
            "storage": "present",
        }))
        .unwrap();
        assert_eq!(present.requests.len(), 1);
    }

    /// A create payload carries no session or terminal field, and adding one
    /// must not be silently tolerated as "an attach row in disguise": the
    /// struct has no place to put either, so extra keys are dropped by serde
    /// and the grant that results is a create grant and nothing more.
    #[test]
    fn a_session_field_on_a_create_payload_is_not_carried() {
        let device = Uuid::from_u128(10);
        let mut body = payload(device, json!(1_900_000_000));
        body["session_id"] = json!(Uuid::from_u128(99).to_string());
        body["terminal_id"] = json!("term-7");
        let pending: PendingCreate = serde_json::from_value(body).unwrap();
        let grant = pending.into_grant().unwrap();
        assert_eq!(grant.grant_jti, "0192a1b2-0000-7000-8000-000000000001");
        assert_eq!(grant.expires_at, 1_900_000_000);
    }
}
