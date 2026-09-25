//! Remote-attach grants — the coord → target-runner feed (plan
//! `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 3c, D6).
//!
//! Coord mints an attach grant on `POST /coord/sessions/{id}/attach-grants`
//! and tells the TARGET device two ways:
//!
//! 1. **Push** — an `attach_request` directive on
//!    `qontinui.sessions.<tenant>.<target-device>.attach_request`, i.e. the
//!    subject family [`super::handoff`] already receives through the
//!    `?subscribe=sessions` lane, which coord resolves from the upgrade
//!    credential to `qontinui.sessions.<tenant>.<self-device>.*` (see
//!    `qontinui_runner_lib::coord_ws::Subscription::Sessions`). This module
//!    adds no second socket: [`super::handoff::connect_and_pump`] forwards
//!    every frame here and
//!    [`parse_attach_push`] claims only the `.attach_request` suffix — the
//!    same disambiguation-by-trailing-segment the handoff and respawn arms use.
//! 2. **Catch-up** — `GET /sessions/attach-requests?device_id=<me>`, device-
//!    bound exactly like `GET /sessions/respawn-requests`. Run on every coord
//!    WS (re)connect beside the other two catch-ups, on every backend-relay
//!    `connected` ack, and on a [`POLL_INTERVAL`] timer, so a grant minted
//!    while a socket was down reaches the table inside the SOURCE's own attach
//!    budget rather than merely inside the grant's 15-minute life — see that
//!    constant for why the difference is the whole point.
//!
//! Either way the row lands in
//! [`crate::mcp::remote_terminal::grants`], the table the relay's terminal
//! handlers consult before honouring a `remote` frame. Nothing here decides
//! who may attach — coord decided that when it minted; this module only makes
//! the decision visible to the process that owns the PTY.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use uuid::Uuid;

use super::handoff::HandoffError;
use super::SessionRegistry;
use crate::mcp::remote_terminal::{grants, now_epoch_secs, AttachGrant};

/// Catch-up poll cadence.
///
/// **Sized against the SOURCE's
/// [`crate::mcp::remote_terminal::ATTACH_TIMEOUT`] (20 s), not against the
/// grant's 900 s life** — the reading this constant carried until 2026-09-20,
/// when it was 60 s "well inside" 900 s. The grant's life is what bounds how
/// long a grant stays USEFUL; it says nothing about how long the source is
/// willing to wait, and those are different clocks. A source that mints a
/// grant and presents it immediately is refused `attach_grant_unknown` until
/// this device has recorded the row, and every retry from the picker mints a
/// FRESH jti — so a cadence longer than the source's attach budget turns a
/// dropped push into a PERMANENT failure rather than a flaky one. That is the
/// observed failure: attach from one box to a target polling at 60 s failed
/// reproducibly, three times on-page.
///
/// At 15 s the worst-case discovery latency for a grant whose push was lost is
/// one tick, which lands inside the 20 s budget with ~5 s of slack for the GET
/// itself (measured sub-second against coord).
///
/// **The cost, stated rather than assumed:** this is an unconditional coord
/// GET per tick on every runner in the fleet, so 60 s → 15 s takes it from 60
/// to **240 requests/hour/device** — 4x on one small device-scoped route, with
/// no payload growth and no per-session fan-out.
///
/// Two residues it deliberately does NOT close, both covered from the SOURCE
/// side by the same-grant re-presentation in
/// [`crate::commands::remote_attach`] rather than by shortening this further:
/// a catch-up GET may take its full [`CATCHUP_TIMEOUT`] (10 s) and carry
/// discovery past 20 s anyway; and a TARGET whose build predates this change
/// still polls at 60 s, which no change on this side can shorten.
pub const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// One attach request, as carried in the `attach_request` push payload and
/// in each row of `GET /sessions/attach-requests`. Optional fields are read
/// as `Option` so a partial payload is UNKNOWN rather than a nil UUID.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PendingAttach {
    pub grant_jti: String,
    #[serde(default)]
    pub source_device_id: Option<Uuid>,
    #[serde(default)]
    pub source_user_id: Option<Uuid>,
    #[serde(default)]
    pub target_device_id: Option<Uuid>,
    pub session_id: Uuid,
    #[serde(default)]
    pub terminal_id: Option<String>,
    /// RFC 3339 timestamp or unix seconds — coord serializes a
    /// `DateTime<Utc>`; both spellings are accepted so a future integer form
    /// cannot silently drop every grant.
    pub expires_at: serde_json::Value,
}

impl PendingAttach {
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
    pub fn into_grant(self) -> Option<AttachGrant> {
        let expires_at = self.expires_at_secs()?;
        Some(AttachGrant {
            grant_jti: self.grant_jti,
            source_device_id: self
                .source_device_id
                .map(|u| u.to_string())
                .unwrap_or_default(),
            session_id: self.session_id,
            terminal_id: self.terminal_id,
            expires_at,
        })
    }
}

/// Envelope coord returns from `GET /sessions/attach-requests`. `storage:
/// "absent"` means the `coord.attach_grants` table has not landed on that
/// coord yet — the push arm is then the only feed, and an empty list is
/// UNKNOWN rather than "no grants".
#[derive(Debug, Clone, Deserialize)]
pub struct AttachListResponse {
    #[serde(default)]
    pub requests: Vec<PendingAttach>,
    #[serde(default)]
    pub storage: Option<String>,
}

/// Pure parse+filter of a coord `/ws` envelope into a [`PendingAttach`]
/// addressed to `device_id`. `None` when the frame is not an attach request
/// for this device — the handoff and respawn arms see the same text and
/// filter on their own suffixes.
pub(super) fn parse_attach_push(text: &str, device_id: Uuid) -> Option<PendingAttach> {
    let envelope: serde_json::Value = serde_json::from_str(text).ok()?;
    let channel = envelope.get("channel").and_then(|c| c.as_str())?;

    let suffix = format!(".{device_id}.attach_request");
    if !channel.starts_with("qontinui.sessions.") || !channel.ends_with(&suffix) {
        return None;
    }

    // Payload may be a JSON string (the Redis arm) or an inlined object.
    let payload_val = match envelope.get("payload") {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s).ok()?,
        Some(other) => other.clone(),
        None => return None,
    };

    let pending: PendingAttach = serde_json::from_value(payload_val).ok()?;

    // Defense-in-depth: the channel already filtered by device; if the body
    // names a different target, trust the address, not the body.
    if pending.target_device_id.is_some_and(|t| t != device_id) {
        return None;
    }
    Some(pending)
}

/// Record one request in the grant table. Returns `false` when the row was
/// unusable (no readable expiry).
pub fn record(pending: PendingAttach) -> bool {
    let jti = pending.grant_jti.clone();
    let session = pending.session_id;
    let source_user = pending.source_user_id;
    match pending.into_grant() {
        Some(grant) => {
            tracing::info!(
                grant_jti = %jti,
                session = %session,
                source_device = %grant.source_device_id,
                source_user = ?source_user,
                expires_at = grant.expires_at,
                "remote attach: grant recorded"
            );
            // `false` = table full of live bindings; the refusal is logged
            // there. The row is still recorded on coord's side, so the
            // source sees `attach_grant_unknown` and can retry later.
            grants().insert(grant, now_epoch_secs())
        }
        None => {
            tracing::warn!(
                grant_jti = %jti,
                session = %session,
                "remote attach: request carries an unreadable expires_at — skipped"
            );
            false
        }
    }
}

/// Handle one inbound `/ws` frame on the attach arm. Not an attach request
/// for this device → ignored silently.
pub(super) fn handle_push_frame(device_id: Uuid, text: &str) {
    if let Some(pending) = parse_attach_push(text, device_id) {
        record(pending);
    }
}

/// Fetch the durable pending-attach list for this device.
pub(super) async fn fetch_pending(
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
    timeout: Duration,
) -> Result<AttachListResponse, HandoffError> {
    let url = format!(
        "{}/sessions/attach-requests?device_id={}",
        coord_url.trim_end_matches('/'),
        device_id
    );
    let resp = crate::coord_http::coord_get(http, &url)
        .timeout(timeout)
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
    resp.json::<AttachListResponse>()
        .await
        .map_err(|e| HandoffError::Parse(format!("decode attach list: {e}")))
}

/// The catch-up: GET the pending list and record every row. Best-effort,
/// same posture as the handoff and respawn catch-ups — one WARN line on a
/// 401/403 (pre-pairing window), debug otherwise.
pub(super) async fn run_catchup(
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
    timeout: Duration,
) {
    match fetch_pending(http, coord_url, device_id, timeout).await {
        Ok(list) => {
            if list.storage.as_deref() == Some("absent") {
                tracing::debug!(
                    "remote attach: coord reports attach_grants storage absent — push arm is the only feed"
                );
            }
            if !list.requests.is_empty() {
                tracing::info!(
                    count = list.requests.len(),
                    "remote attach: catch-up recording pending grants"
                );
            }
            for pending in list.requests {
                record(pending);
            }
            grants().purge_expired(now_epoch_secs());
        }
        Err(HandoffError::Status(401 | 403, _)) => {
            tracing::warn!(
                "remote attach: catch-up GET unauthorized (401/403) — retrying after device pairing/auth"
            );
        }
        Err(HandoffError::Status(404, _)) => {
            tracing::debug!(
                "remote attach: coord has no /sessions/attach-requests route yet (404) — push arm only"
            );
        }
        Err(e) => {
            tracing::debug!(error = %e, "remote attach: catch-up GET failed (push path still active)");
        }
    }
}

/// The background catch-up's HTTP budget. Generous because nothing is waiting
/// on it — it runs on the [`POLL_INTERVAL`] poll and on the coord-WS / relay
/// `connected` acks.
pub const CATCHUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the catch-up once against the registry's coord — the door the backend
/// relay calls on its `connected` ack.
pub async fn catch_up_now(registry: &Arc<SessionRegistry>) {
    catch_up_now_within(registry, CATCHUP_TIMEOUT).await;
}

/// [`catch_up_now`] with an explicit HTTP timeout.
///
/// The on-demand re-read — the one `handle_terminal_attach` awaits on the
/// backend relay's SERIAL read loop when an attach names a jti this device
/// does not know — passes
/// [`crate::mcp::remote_terminal::GRANT_REREAD_TIMEOUT`] here instead of the
/// background [`CATCHUP_TIMEOUT`]. That call freezes terminal input, terminal
/// output and every other relay frame on the socket for as long as it runs, so
/// its bound is a LIVENESS budget for the whole device and not a patience
/// budget for one grant — the same reasoning as the create twin.
pub async fn catch_up_now_within(registry: &Arc<SessionRegistry>, timeout: Duration) {
    let http = registry.coord_sync().http_client();
    let coord_url = registry.coord_sync().coord_url().to_string();
    run_catchup(&http, &coord_url, registry.machine_id(), timeout).await;
}

/// The [`POLL_INTERVAL`] catch-up loop. Returns the handle so the caller can
/// hold it for the process lifetime.
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

    fn payload(session: Uuid, target: Uuid, expires: serde_json::Value) -> serde_json::Value {
        json!({
            "grant_jti": "0192a1b2-0000-7000-8000-000000000001",
            "source_device_id": Uuid::from_u128(1).to_string(),
            "source_user_id": Uuid::from_u128(2).to_string(),
            "target_device_id": target.to_string(),
            "session_id": session.to_string(),
            "terminal_id": null,
            "expires_at": expires,
        })
    }

    /// A string-payload envelope on this device's `.attach_request` subject
    /// parses; the same body on another device's subject, or on the
    /// `.respawn_request` / `.handoff_request` suffixes, is ignored.
    #[test]
    fn parses_only_this_devices_attach_request_suffix() {
        let device = Uuid::from_u128(10);
        let other = Uuid::from_u128(11);
        let session = Uuid::from_u128(20);
        let tenant = Uuid::from_u128(30);
        let body = payload(session, device, json!("2030-01-01T00:00:00Z"));

        let ok = json!({
            "channel": format!("qontinui.sessions.{tenant}.{device}.attach_request"),
            "payload": body.to_string(),
        });
        let parsed = parse_attach_push(&ok.to_string(), device).expect("parsed");
        assert_eq!(parsed.session_id, session);
        assert_eq!(parsed.target_device_id, Some(device));
        assert_eq!(parsed.grant_jti, "0192a1b2-0000-7000-8000-000000000001");

        for wrong in [
            format!("qontinui.sessions.{tenant}.{other}.attach_request"),
            format!("qontinui.sessions.{tenant}.{device}.respawn_request"),
            format!("qontinui.sessions.{tenant}.{device}.handoff_request"),
            format!("qontinui.other.{tenant}.{device}.attach_request"),
        ] {
            let env = json!({"channel": wrong, "payload": body.to_string()});
            assert!(
                parse_attach_push(&env.to_string(), device).is_none(),
                "{wrong} must not parse"
            );
        }
    }

    /// An inlined-object payload parses too, and a body whose target disagrees
    /// with the address is dropped.
    #[test]
    fn inlined_payload_and_target_mismatch() {
        let device = Uuid::from_u128(10);
        let other = Uuid::from_u128(11);
        let tenant = Uuid::from_u128(30);
        let inlined = json!({
            "channel": format!("qontinui.sessions.{tenant}.{device}.attach_request"),
            "payload": payload(Uuid::from_u128(20), device, json!(1_900_000_000)),
        });
        assert!(parse_attach_push(&inlined.to_string(), device).is_some());

        let mismatched = json!({
            "channel": format!("qontinui.sessions.{tenant}.{device}.attach_request"),
            "payload": payload(Uuid::from_u128(20), other, json!(1_900_000_000)),
        });
        assert!(parse_attach_push(&mismatched.to_string(), device).is_none());
    }

    /// RFC 3339 and integer expiries both become a grant; an unreadable one
    /// yields no grant rather than an invented lifetime.
    #[test]
    fn expiry_forms() {
        let device = Uuid::from_u128(10);
        let mk = |expires: serde_json::Value| -> PendingAttach {
            serde_json::from_value(payload(Uuid::from_u128(20), device, expires)).unwrap()
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
        assert_eq!(g.terminal_id, None);
    }

    /// Item 3's whole point: the target must be able to LEARN a grant inside
    /// the SOURCE's own attach budget, not merely inside the grant's life.
    /// Every source retry mints a fresh jti, so a cadence at or past
    /// [`crate::mcp::remote_terminal::ATTACH_TIMEOUT`] makes a dropped push a
    /// permanent failure rather than a flaky one — which is exactly what was
    /// observed at the old 60 s. Fails on any future widening of the poll.
    #[test]
    fn the_poll_lands_inside_the_sources_attach_timeout() {
        use crate::mcp::remote_terminal::ATTACH_TIMEOUT;
        assert!(
            POLL_INTERVAL < ATTACH_TIMEOUT,
            "poll {POLL_INTERVAL:?} vs attach budget {ATTACH_TIMEOUT:?}: a target that learns a grant more slowly than the source waits makes a dropped attach_request permanent"
        );
        // …and with room left for the GET itself inside the same budget.
        assert!(
            POLL_INTERVAL + Duration::from_secs(5) <= ATTACH_TIMEOUT,
            "no slack left for the catch-up GET inside the attach budget"
        );
    }

    /// The list envelope's `storage` marker round-trips, and rows decode.
    #[test]
    fn list_response_decodes_storage_marker() {
        let absent: AttachListResponse =
            serde_json::from_str(r#"{"requests":[],"storage":"absent"}"#).unwrap();
        assert!(absent.requests.is_empty());
        assert_eq!(absent.storage.as_deref(), Some("absent"));
        let present: AttachListResponse = serde_json::from_value(json!({
            "requests": [payload(Uuid::from_u128(20), Uuid::from_u128(10), json!(1_900_000_000))],
            "storage": "present",
        }))
        .unwrap();
        assert_eq!(present.requests.len(), 1);
    }
}
