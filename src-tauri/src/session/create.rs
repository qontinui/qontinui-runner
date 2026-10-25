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
//!    subject family [`super::handoff`] already receives through the
//!    `?subscribe=sessions` lane, which coord resolves from the upgrade
//!    credential to `qontinui.sessions.<tenant>.<self-device>.*` (see
//!    `qontinui_runner_lib::coord_ws::Subscription::Sessions`). No second
//!    socket: [`super::handoff::connect_and_pump`] forwards every frame here and
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

use super::attach::CatchupTally;
use super::handoff::HandoffError;
use super::SessionRegistry;
use crate::mcp::remote_terminal::{
    create_grants, now_epoch_secs, CreateGrant, GrantInsert, RemoteCreateGrants,
};

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
    ///
    /// `None` — the row is DROPPED — when `expires_at` is unreadable OR when
    /// the feed named no `source_device_id`.
    ///
    /// The second arm is not symmetry for its own sake. The source binding in
    /// [`crate::mcp::remote_terminal::RemoteCreateGrants::resolve`] is what
    /// stops the relay replaying a jti from a device coord never minted it
    /// for, and it compares against THIS string — so an absent field became an
    /// empty string, an empty string skipped the comparison, and a grant with
    /// no source binding at all was recorded (review finding 9). `#[serde(default)]`
    /// makes that a silent one-key edit on the wire rather than a parse error,
    /// which is exactly the shape that has to fail closed. Coord's column is
    /// `UUID NOT NULL`, so a row missing it is malformed, not merely partial —
    /// the same reading an unreadable `expires_at` already got.
    pub fn into_grant(self) -> Option<CreateGrant> {
        let expires_at = self.expires_at_secs()?;
        let source_device_id = self.source_device_id?.to_string();
        Some(CreateGrant {
            grant_jti: self.grant_jti,
            source_device_id,
            source_user_id: self
                .source_user_id
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

/// Record one request in the process-wide create-grant table. See
/// [`record_into`].
pub fn record(pending: PendingCreate) -> GrantInsert {
    record_into(create_grants(), pending, now_epoch_secs())
}

/// Record one request in `table`, the seam behind [`record`] (which passes
/// [`create_grants`]) so a test can drive a table of its own.
///
/// Logs by outcome, because coord re-lists a grant on every catch-up pass until
/// it expires or is consumed:
///
/// - `Recorded` — INFO `grant recorded`, once per grant.
/// - `Refreshed` — TRACE: the table already held it.
/// - `RefusedSpent` — nothing more than `insert`'s own DEBUG. A spent jti is
///   re-listed until coord flips `consumed_at`; the cases where that never
///   happens are WARNed by [`notify_consumed`].
/// - `Refused` and `Unusable` — WARN, as before: a poisoned lock and a
///   malformed row are exactly the drops a silent caller would hide, and a
///   later `remote_create_grant_unknown` is explicable only with this line.
fn record_into(table: &RemoteCreateGrants, pending: PendingCreate, now: u64) -> GrantInsert {
    let jti = pending.grant_jti.clone();
    let Some(grant) = pending.into_grant() else {
        tracing::warn!(
            grant_jti = %jti,
            "remote create: request carries an unreadable expires_at, or no \
             source_device_id — skipped (both are fail-closed: a grant with no source \
             binding would be replayable from any device)"
        );
        return GrantInsert::Unusable;
    };
    let source_device = grant.source_device_id.clone();
    let source_user = grant.source_user_id.clone();
    let expires_at = grant.expires_at;
    let outcome = table.insert(grant, now);
    match outcome {
        GrantInsert::Recorded => tracing::info!(
            grant_jti = %jti,
            source_device = %source_device,
            source_user = %source_user,
            expires_at,
            "remote create: grant recorded"
        ),
        GrantInsert::Refreshed => tracing::trace!(
            grant_jti = %jti,
            expires_at,
            "remote create: grant already recorded — expiry refreshed"
        ),
        GrantInsert::RefusedSpent => {}
        // `insert` names the reason; this says the row was DROPPED, so a
        // later `remote_create_grant_unknown` is explicable.
        GrantInsert::Refused | GrantInsert::Unusable => tracing::warn!(
            grant_jti = %jti,
            "remote create: grant NOT recorded — a frame presenting it will be refused"
        ),
    }
    outcome
}

/// Record every row of one catch-up pass in `table` and log ONE summary line:
/// INFO when the pass recorded something new, TRACE otherwise — the attach
/// twin's rule. A spent jti coord keeps re-listing must not bring back a
/// per-tick INFO line.
fn record_all(table: &RemoteCreateGrants, rows: Vec<PendingCreate>, now: u64) -> CatchupTally {
    let mut tally = CatchupTally::default();
    for pending in rows {
        tally.note(record_into(table, pending, now));
    }
    if tally.new > 0 {
        tracing::info!(
            new = tally.new,
            known = tally.known,
            refused = tally.refused,
            spent = tally.spent,
            "remote create: catch-up recorded new pending grants"
        );
    } else {
        tracing::trace!(
            new = tally.new,
            known = tally.known,
            refused = tally.refused,
            spent = tally.spent,
            "remote create: catch-up recorded new pending grants"
        );
    }
    tally
}

/// How long to keep re-trying `notify_consumed`, and how long to wait between
/// tries. Six attempts over ~2 minutes, inside a create grant's 15-minute life.
///
/// **The retry is the durable half of single-use, not a nicety.** The local
/// tombstone dies with the process, and the relay's Redis claim is released the
/// moment the create completes, so coord's `consumed_at` is the ONLY barrier
/// that survives a target restart. A single no-retry POST made that barrier
/// conditional on one 10-second window being healthy: a coord blip during it
/// left the row `consumed_at IS NULL`, the restarted runner's catch-up listed
/// the grant again, and the identical frame replayed into a second PTY — once
/// per restart, for the rest of the grant's TTL (review finding 4).
const CONSUME_BACKOFF: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(8),
    Duration::from_secs(20),
    Duration::from_secs(45),
];

/// Tell coord a grant is SPENT — `POST /sessions/create-requests/{jti}/consume`.
///
/// **The runner's own tombstone is not enough on its own.** Coord keeps serving
/// a grant in `GET /sessions/create-requests` until its `consumed_at` is set, so
/// without this call the row outlives its one use: this process refuses it (the
/// tombstone), but a restarted runner — or a second runner on the same device —
/// reads it back as pending.
///
/// Retried — see [`CONSUME_BACKOFF`]. A 4xx other than 408/429 is FINAL (coord
/// understood us and said no; retrying cannot change that, and a 404 from a
/// coord whose route has not landed would otherwise retry for two minutes per
/// spend). A transport error, a 5xx, a 408 and a 429 are all retried. Still
/// best-effort in the end: exhausting the schedule is logged loudly, because
/// what is lost is precisely the barrier a restart depends on.
pub async fn notify_consumed(registry: &Arc<SessionRegistry>, grant_jti: &str) {
    let http = registry.coord_sync().http_client();
    let coord_url = registry.coord_sync().coord_url().to_string();
    let url = format!(
        "{}/sessions/create-requests/{}/consume",
        coord_url.trim_end_matches('/'),
        grant_jti
    );
    let mut last = String::new();
    for attempt in 0..=CONSUME_BACKOFF.len() {
        let resp = crate::coord_http::coord_post(&http, &url)
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                tracing::debug!(
                    grant_jti,
                    attempt,
                    "remote create: coord marked the grant consumed"
                );
                return;
            }
            Ok(r) => {
                let status = r.status();
                last = format!("HTTP {}", status.as_u16());
                let retryable = status.is_server_error()
                    || status == reqwest::StatusCode::REQUEST_TIMEOUT
                    || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                if !retryable {
                    tracing::warn!(
                        grant_jti,
                        status = status.as_u16(),
                        "remote create: coord REFUSED to mark the grant consumed — final, not \
                         retried; the row stays in the pending list until it expires and this \
                         device refuses it locally in the meantime"
                    );
                    return;
                }
            }
            Err(e) => last = e.to_string(),
        }
        match CONSUME_BACKOFF.get(attempt) {
            Some(delay) => {
                tracing::debug!(
                    grant_jti,
                    attempt,
                    error = %last,
                    "remote create: consume notice failed — retrying"
                );
                tokio::time::sleep(*delay).await;
            }
            None => break,
        }
    }
    tracing::warn!(
        grant_jti,
        attempts = CONSUME_BACKOFF.len() + 1,
        error = %last,
        "remote create: coord never marked the grant consumed — the DURABLE half of single-use \
         is gone for this jti: this process still refuses it, but a runner restart inside the \
         grant's remaining life would read it back as pending"
    );
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
    timeout: Duration,
) -> Result<CreateListResponse, HandoffError> {
    let url = format!(
        "{}/sessions/create-requests?device_id={}",
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
    resp.json::<CreateListResponse>()
        .await
        .map_err(|e| HandoffError::Parse(format!("decode create list: {e}")))
}

/// The catch-up: GET the pending list and record every row. Best-effort, same
/// posture as the attach catch-up — one WARN line on a 401/403 (pre-pairing
/// window), debug otherwise. A failure costs reachability, never admission:
/// nothing is admitted on a jti this table does not hold.
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
                    "remote create: coord reports create_grants storage absent — push arm is \
                     the only feed"
                );
            }
            record_all(create_grants(), list.requests, now_epoch_secs());
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

/// The catch-up GET's timeout on the BACKGROUND paths — the 60 s poll and the
/// coord-WS / relay `connected` acks. Nothing is waiting on those.
pub const CATCHUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the catch-up once against the registry's coord — the door the backend
/// relay calls on its `connected` ack.
pub async fn catch_up_now(registry: &Arc<SessionRegistry>) {
    catch_up_now_within(registry, CATCHUP_TIMEOUT).await;
}

/// [`catch_up_now`] with an explicit HTTP timeout.
///
/// The on-demand re-read — the one `handle_terminal_create` awaits on the
/// backend relay's SERIAL read loop when a create names a jti this device does
/// not know — passes
/// [`crate::mcp::remote_terminal::GRANT_REREAD_TIMEOUT`] here instead of the
/// background [`CATCHUP_TIMEOUT`]. That call freezes terminal input, terminal
/// output and every other relay frame on the socket for as long as it runs, so
/// its bound is a LIVENESS budget for the whole device and not a patience
/// budget for one grant (review finding 5).
pub async fn catch_up_now_within(registry: &Arc<SessionRegistry>, timeout: Duration) {
    let http = registry.coord_sync().http_client();
    let coord_url = registry.coord_sync().coord_url().to_string();
    run_catchup(&http, &coord_url, registry.machine_id(), timeout).await;
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

    /// **A feed row naming no source device is DROPPED** — review finding 9.
    ///
    /// The source binding in `RemoteCreateGrants::resolve` is what stops the
    /// relay replaying a jti from a device coord never minted it for, and it
    /// compares against this string. `Option<Uuid>` + `#[serde(default)]` made
    /// an absent key a silent `None`, `None` became `""`, and `""` skipped the
    /// comparison — so ONE key removed on the wire disabled the binding
    /// entirely. Coord's column is `UUID NOT NULL`, so such a row is malformed,
    /// not partial, and gets the same reading an unreadable `expires_at`
    /// already got.
    ///
    /// Fails against the old `unwrap_or_default()`: that returned
    /// `Some(CreateGrant { source_device_id: "", .. })`.
    #[test]
    fn a_request_with_no_source_device_is_dropped_rather_than_unbound() {
        let device = Uuid::from_u128(10);
        let mut body = payload(device, json!(1_900_000_000));
        body.as_object_mut().unwrap().remove("source_device_id");
        let pending: PendingCreate = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(pending.source_device_id, None, "it still PARSES");
        assert!(
            pending.into_grant().is_none(),
            "…and is dropped rather than recorded with an empty binding"
        );
        assert_eq!(
            record(serde_json::from_value(body).unwrap()),
            GrantInsert::Unusable,
            "`record` reports the drop"
        );

        // An explicit null is the same row.
        let mut nulled = payload(device, json!(1_900_000_000));
        nulled["source_device_id"] = json!(null);
        let pending: PendingCreate = serde_json::from_value(nulled).unwrap();
        assert!(pending.into_grant().is_none());
    }

    /// The user the grant was minted for reaches the table, because the
    /// `same_user` arm of this device's own dial is the only thing that can
    /// enforce it locally (review finding 6). Absent stays EMPTY here — the
    /// refusal is made at the dial, not by inventing an identity.
    #[test]
    fn the_source_user_reaches_the_grant() {
        let device = Uuid::from_u128(10);
        let grant = serde_json::from_value::<PendingCreate>(payload(device, json!(1_900_000_000)))
            .unwrap()
            .into_grant()
            .expect("a complete row");
        assert_eq!(grant.source_user_id, Uuid::from_u128(2).to_string());

        let mut body = payload(device, json!(1_900_000_000));
        body.as_object_mut().unwrap().remove("source_user_id");
        let grant = serde_json::from_value::<PendingCreate>(body)
            .unwrap()
            .into_grant()
            .expect("a user-less row is still a usable grant under `tenant`");
        assert_eq!(grant.source_user_id, "");
    }

    /// The consume notice is RETRIED — it is the only barrier that survives a
    /// target restart (review finding 4). The schedule stays inside a create
    /// grant's 15-minute life, or a retry would outlive what it protects.
    #[test]
    fn the_consume_notice_retries_within_the_grants_life() {
        assert!(
            !CONSUME_BACKOFF.is_empty(),
            "a single attempt is the defect"
        );
        let total: Duration = CONSUME_BACKOFF.iter().sum();
        assert!(
            total < Duration::from_secs(900),
            "the schedule must finish inside the grant's own life"
        );
        // Monotonic: a flat schedule is a poll, not a backoff.
        for pair in CONSUME_BACKOFF.windows(2) {
            assert!(pair[1] > pair[0], "{pair:?}");
        }
    }

    /// The ON-DEMAND re-read's timeout is a fraction of the background
    /// catch-up's (review finding 5). It is awaited on the backend relay's
    /// SERIAL read loop, so it freezes terminal input, terminal output and
    /// every other frame on the socket for as long as it runs — a liveness
    /// budget for the whole device, not a patience budget for one grant. It
    /// must also be well under the throttle's cooldown, or the stalls run
    /// back to back however the cooldown is stamped.
    #[test]
    fn the_on_demand_reread_is_bounded_far_tighter_than_the_background_catch_up() {
        use crate::mcp::remote_terminal::{GRANT_REREAD_COOLDOWN_SECS, GRANT_REREAD_TIMEOUT};
        assert!(GRANT_REREAD_TIMEOUT < CATCHUP_TIMEOUT);
        assert!(
            GRANT_REREAD_TIMEOUT < Duration::from_secs(GRANT_REREAD_COOLDOWN_SECS),
            "a re-read that can run for a whole cooldown window makes the throttle a \
             frequency bound on STARTS rather than a gap between stalls"
        );
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

    // ---- Logging only what is new — the attach twin's rule (plan
    // `2026-10-19-runner-remote-attach-catch-up-re-records-the-same-grants`
    // Phase 2). Every test below drives a LOCAL table.

    const NOW: u64 = 1_700_000_000;

    fn row(jti: &str) -> PendingCreate {
        PendingCreate {
            grant_jti: jti.to_string(),
            source_device_id: Some(Uuid::from_u128(1)),
            source_user_id: Some(Uuid::from_u128(2)),
            target_device_id: Some(Uuid::from_u128(10)),
            expires_at: json!(NOW + 900),
        }
    }

    fn rows(n: usize) -> Vec<PendingCreate> {
        (0..n).map(|i| row(&format!("jti-{i}"))).collect()
    }

    /// The lines of a capture other than its sentinel.
    fn besides<'a>(logs: &'a str, sentinel: &str) -> Vec<&'a str> {
        logs.lines().filter(|l| !l.contains(sentinel)).collect()
    }

    /// A row that cannot become a grant is `Unusable` (both fail-closed
    /// readings), and a readable one is `Recorded`, then `Refreshed`.
    #[test]
    fn record_into_answers_unusable_then_recorded_then_refreshed() {
        let table = RemoteCreateGrants::new();
        let mut bad = row("jti-bad");
        bad.expires_at = json!("not a time");
        assert_eq!(record_into(&table, bad, NOW), GrantInsert::Unusable);
        let mut sourceless = row("jti-sourceless");
        sourceless.source_device_id = None;
        assert_eq!(record_into(&table, sourceless, NOW), GrantInsert::Unusable);
        assert!(table.is_empty());
        assert_eq!(
            record_into(&table, row("jti-ok"), NOW),
            GrantInsert::Recorded
        );
        assert_eq!(
            record_into(&table, row("jti-ok"), NOW),
            GrantInsert::Refreshed
        );
    }

    /// The tally a pass returns counts a re-listed grant as `known`, not
    /// `new`. Fails against an `insert` whose refresh arm answers `Recorded`.
    #[test]
    fn record_all_counts_only_unseen_rows_as_new() {
        let table = RemoteCreateGrants::new();
        assert_eq!(record_all(&table, rows(20), NOW).new, 20);
        assert_eq!(
            record_all(&table, rows(20), NOW),
            CatchupTally {
                known: 20,
                ..CatchupTally::default()
            }
        );
        let mut mixed = rows(19);
        mixed.push(row("jti-new"));
        assert_eq!(
            record_all(&table, mixed, NOW),
            CatchupTally {
                new: 1,
                known: 19,
                refused: 0,
                spent: 0,
            }
        );
    }

    /// The steady state is silent at INFO, and one new grant among known ones
    /// is exactly one `grant recorded` line plus one summary with `new=1`.
    /// Each capture first logs a sentinel the test requires.
    #[test]
    fn a_pass_over_known_grants_logs_nothing_at_info() {
        use crate::resource_guard::capture_logs;
        const SENTINEL: &str = "sentinel: create record_all capture is live";
        let table = RemoteCreateGrants::new();
        record_all(&table, rows(20), NOW);

        let (tally, logs) = capture_logs(|| {
            tracing::info!("{SENTINEL}");
            record_all(&table, rows(20), NOW)
        });
        assert_eq!(tally.known, 20);
        assert!(
            logs.contains(SENTINEL),
            "the capture must be live: {logs:?}"
        );
        assert!(besides(&logs, SENTINEL).is_empty(), "{logs}");

        let mut mixed = rows(19);
        mixed.push(row("jti-new"));
        let (tally, logs) = capture_logs(|| {
            tracing::info!("{SENTINEL}");
            record_all(&table, mixed.clone(), NOW)
        });
        assert_eq!(tally.new, 1);
        assert!(
            logs.contains(SENTINEL),
            "the capture must be live: {logs:?}"
        );
        let others = besides(&logs, SENTINEL);
        assert_eq!(
            others
                .iter()
                .filter(|l| l.contains("remote create: grant recorded")
                    && l.contains("grant_jti=jti-new"))
                .count(),
            1,
            "{logs}"
        );
        let summary: Vec<&&str> = others
            .iter()
            .filter(|l| l.contains("remote create: catch-up recorded new pending grants"))
            .collect();
        assert_eq!(summary.len(), 1, "{logs}");
        assert!(summary[0].contains("new=1"), "{logs}");
        assert_eq!(others.len(), 2, "nothing else at INFO: {logs}");
    }

    /// A grant this device SPENT stays in coord's list until coord flips
    /// `consumed_at`, so every later pass re-delivers it beside the known
    /// rows. Those passes report it as `spent` and log NOTHING at INFO or
    /// above — no per-tick summary, no `grant NOT recorded` WARN from
    /// `record_into`, no WARN from `insert`. Fails against the old WARN pair.
    #[test]
    fn a_relisted_spent_grant_keeps_later_passes_silent() {
        use crate::resource_guard::capture_logs;
        const SENTINEL: &str = "sentinel: create spent re-listing capture is live";
        let table = RemoteCreateGrants::new();
        assert_eq!(record_all(&table, rows(20), NOW).new, 20);
        let source = Uuid::from_u128(1).to_string();
        table
            .consume("jti-0", Some(&source), NOW)
            .expect("the first use spends jti-0");

        for pass in 2..=3 {
            let (tally, logs) = capture_logs(|| {
                tracing::warn!("{SENTINEL}");
                record_all(&table, rows(20), NOW)
            });
            assert_eq!(
                tally,
                CatchupTally {
                    new: 0,
                    known: 19,
                    refused: 0,
                    spent: 1,
                },
                "pass {pass}"
            );
            assert!(
                logs.contains(SENTINEL),
                "the capture must be live: {logs:?}"
            );
            assert!(
                besides(&logs, SENTINEL).is_empty(),
                "pass {pass} logs nothing at INFO or above: {logs}"
            );
        }
        assert!(!table.contains("jti-0"), "the spent grant stays spent");
    }
}
