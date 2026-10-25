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
use crate::mcp::remote_terminal::{
    grants, now_epoch_secs, AttachGrant, GrantInsert, RemoteAttachGrants,
};

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

/// Record one request in the process-wide grant table. See [`record_into`].
pub fn record(pending: PendingAttach) -> GrantInsert {
    record_into(grants(), pending, now_epoch_secs())
}

/// What one catch-up pass did with the rows coord listed — the fold of every
/// row's [`GrantInsert`].
///
/// Coord's list is every unexpired, unconsumed grant for the device, not only
/// the ones this process has not seen, so in steady state a pass is all
/// `known`. Only `new` is news.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatchupTally {
    /// Rows the table did not hold before this pass.
    pub new: usize,
    /// Rows it already held — expiry refreshed, nothing else changed.
    pub known: usize,
    /// Rows refused by the table, or unusable as a grant. Each is logged
    /// where it was refused.
    pub refused: usize,
    /// Create rows this process already spent (always 0 for attach).
    pub spent: usize,
}

impl CatchupTally {
    /// Count one row's outcome.
    pub fn note(&mut self, outcome: GrantInsert) {
        match outcome {
            GrantInsert::Recorded => self.new += 1,
            GrantInsert::Refreshed => self.known += 1,
            GrantInsert::Refused | GrantInsert::Unusable => self.refused += 1,
            GrantInsert::RefusedSpent => self.spent += 1,
        }
    }
}

/// Record one request in `table`, the seam behind [`record`] (which passes
/// [`grants`]) so a test can drive a table of its own.
///
/// Logs `grant recorded` at INFO only for a row the table did not hold, and
/// only once it is in: the push and every catch-up pass re-deliver the same
/// grant for its whole 15-minute life, and logging each delivery put ~50 INFO
/// lines in the log per grant. A refresh is TRACE. A capacity refusal is
/// WARNed inside `insert`, where its reason is known.
fn record_into(table: &RemoteAttachGrants, pending: PendingAttach, now: u64) -> GrantInsert {
    let jti = pending.grant_jti.clone();
    let session = pending.session_id;
    let source_user = pending.source_user_id;
    let Some(grant) = pending.into_grant() else {
        tracing::warn!(
            grant_jti = %jti,
            session = %session,
            "remote attach: request carries an unreadable expires_at — skipped"
        );
        return GrantInsert::Unusable;
    };
    let source_device = grant.source_device_id.clone();
    let expires_at = grant.expires_at;
    // `Refused` = a poisoned lock, or a table full of live bindings (logged
    // there). The row is still recorded on coord's side, so the source sees
    // `attach_grant_unknown` and can retry later.
    let outcome = table.insert(grant, now);
    match outcome {
        GrantInsert::Recorded => tracing::info!(
            grant_jti = %jti,
            session = %session,
            source_device = %source_device,
            source_user = ?source_user,
            expires_at,
            "remote attach: grant recorded"
        ),
        GrantInsert::Refreshed => tracing::trace!(
            grant_jti = %jti,
            expires_at,
            "remote attach: grant already recorded — expiry refreshed"
        ),
        GrantInsert::Refused | GrantInsert::RefusedSpent | GrantInsert::Unusable => {}
    }
    outcome
}

/// Record every row of one catch-up pass in `table` and log ONE summary line:
/// INFO when the pass recorded something new, TRACE otherwise. A refusal that
/// persists across passes must not bring back a per-tick INFO line — each one
/// is already logged where it is refused.
fn record_all(table: &RemoteAttachGrants, rows: Vec<PendingAttach>, now: u64) -> CatchupTally {
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
            "remote attach: catch-up recorded new pending grants"
        );
    } else {
        tracing::trace!(
            new = tally.new,
            known = tally.known,
            refused = tally.refused,
            spent = tally.spent,
            "remote attach: catch-up recorded new pending grants"
        );
    }
    tally
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
            record_all(grants(), list.requests, now_epoch_secs());
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
pub(super) mod tests {
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

    // ---- Logging only what is new (plan
    // `2026-10-19-runner-remote-attach-catch-up-re-records-the-same-grants`) --
    //
    // Coord's `GET /sessions/attach-requests` lists every unexpired, unconsumed
    // grant on every 15 s pass, not only unseen ones. Measured 2026-10-19:
    // 30,008 `grant recorded` INFO lines for 602 distinct jtis, and a
    // `count=20` summary every tick. Every test below drives a LOCAL table
    // except the entry-point one, which writes unique jtis to the global table.

    const NOW: u64 = 1_700_000_000;

    fn row(jti: &str) -> PendingAttach {
        PendingAttach {
            grant_jti: jti.to_string(),
            source_device_id: Some(Uuid::from_u128(1)),
            source_user_id: Some(Uuid::from_u128(2)),
            target_device_id: Some(Uuid::from_u128(10)),
            session_id: Uuid::from_u128(20),
            terminal_id: None,
            expires_at: json!(NOW + 900),
        }
    }

    fn rows(n: usize) -> Vec<PendingAttach> {
        (0..n).map(|i| row(&format!("jti-{i}"))).collect()
    }

    /// The lines of a capture other than its sentinel.
    fn besides<'a>(logs: &'a str, sentinel: &str) -> Vec<&'a str> {
        logs.lines().filter(|l| !l.contains(sentinel)).collect()
    }

    /// A row that cannot become a grant is `Unusable`, and never reaches the
    /// table; a readable one is `Recorded`, then `Refreshed`.
    #[test]
    fn record_into_answers_unusable_for_an_unreadable_expiry() {
        let table = RemoteAttachGrants::new();
        let mut bad = row("jti-bad");
        bad.expires_at = json!("not a time");
        assert_eq!(record_into(&table, bad, NOW), GrantInsert::Unusable);
        assert_eq!(table.len(), 0);
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
        let table = RemoteAttachGrants::new();
        assert_eq!(
            record_all(&table, rows(20), NOW),
            CatchupTally {
                new: 20,
                ..CatchupTally::default()
            }
        );
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
    /// Each capture first logs a sentinel the test requires, so a capture lost
    /// to tracing's global-cache race cannot pass as "logged nothing".
    #[test]
    fn a_pass_over_known_grants_logs_nothing_at_info() {
        use crate::resource_guard::capture_logs;
        const SENTINEL: &str = "sentinel: attach record_all capture is live";
        let table = RemoteAttachGrants::new();
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
        assert!(
            besides(&logs, SENTINEL).is_empty(),
            "a pass over 20 known grants logs nothing at INFO: {logs}"
        );

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
        let recorded: Vec<&&str> = others
            .iter()
            .filter(|l| l.contains("remote attach: grant recorded"))
            .collect();
        assert_eq!(recorded.len(), 1, "{logs}");
        assert!(recorded[0].contains("grant_jti=jti-new"), "{logs}");
        let summary: Vec<&&str> = others
            .iter()
            .filter(|l| l.contains("remote attach: catch-up recorded new pending grants"))
            .collect();
        assert_eq!(summary.len(), 1, "{logs}");
        assert!(summary[0].contains("new=1"), "{logs}");
        assert!(summary[0].contains("known=19"), "{logs}");
        assert_eq!(others.len(), 2, "nothing else at INFO: {logs}");
    }

    /// Serve `body` as the answer to every request, recording each request
    /// line — a canned coord for `GET /sessions/attach-requests`.
    pub(in crate::session) async fn serve_canned(
        listener: tokio::net::TcpListener,
        body: String,
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let text = String::from_utf8_lossy(&head);
            let request_line = text.lines().next().unwrap_or_default().to_string();
            seen.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(request_line);
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    }

    /// The entry point, end to end: `run_catchup` against a loopback coord
    /// serving the same 20 rows twice. The first pass records them (20
    /// `grant recorded` lines and one `new=20` summary); the second, which is
    /// every later 15 s tick in production, logs nothing at INFO.
    ///
    /// Writes the GLOBAL table (that is what `run_catchup` feeds), so its jtis
    /// are unique to this run and removed afterwards. Runs under
    /// `isolated_ambient` so `attach_device_auth` reads no real credential.
    #[test]
    fn run_catchup_logs_a_relisted_grant_once_rather_than_on_every_pass() {
        use crate::resource_guard::capture_logs;
        const SENTINEL: &str = "sentinel: attach run_catchup capture is live";
        // `count_and_resolve_bearer` (lib `auth`) logs this at INFO on every
        // 25th coord call process-wide, at most once a minute. It is not this
        // module's line, and which test's call is the 25th is not ours to fix.
        const UNRELATED: &str = "coord data-plane device-JWT coverage";
        let _amb = crate::test_env::isolated_ambient();

        let device = Uuid::new_v4();
        let run = Uuid::new_v4();
        let jtis: Vec<String> = (0..20).map(|i| format!("{run}-{i}")).collect();
        let expires = now_epoch_secs() + 900;
        let body = json!({
            "requests": jtis.iter().map(|jti| json!({
                "grant_jti": jti,
                "source_device_id": Uuid::from_u128(1).to_string(),
                "source_user_id": Uuid::from_u128(2).to_string(),
                "target_device_id": device.to_string(),
                "session_id": Uuid::from_u128(20).to_string(),
                "terminal_id": null,
                "expires_at": expires,
            })).collect::<Vec<_>>(),
            "storage": "present",
        })
        .to_string();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        rt.spawn(serve_canned(listener, body, seen.clone()));
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let coord_url = format!("http://{addr}");
        let pass = || {
            capture_logs(|| {
                tracing::info!("{SENTINEL}");
                rt.block_on(run_catchup(
                    &http,
                    &coord_url,
                    device,
                    Duration::from_secs(10),
                ));
            })
            .1
        };

        let first = pass();
        let held: Vec<bool> = jtis.iter().map(|j| grants().contains(j)).collect();
        let second = pass();
        for jti in &jtis {
            grants().remove(jti);
        }

        let expected_line = format!("GET /sessions/attach-requests?device_id={device} HTTP/1.1");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![expected_line.clone(), expected_line],
            "one GET per pass, on the catch-up route"
        );
        assert!(held.iter().all(|h| *h), "the first pass records every row");

        assert!(
            first.contains(SENTINEL),
            "the capture must be live: {first:?}"
        );
        assert_eq!(
            first.matches("remote attach: grant recorded").count(),
            20,
            "{first}"
        );
        let summary: Vec<&str> = first
            .lines()
            .filter(|l| l.contains("remote attach: catch-up recorded new pending grants"))
            .collect();
        assert_eq!(summary.len(), 1, "{first}");
        assert!(summary[0].contains("new=20"), "{first}");

        assert!(
            second.contains(SENTINEL),
            "the capture must be live: {second:?}"
        );
        let noise: Vec<&str> = besides(&second, SENTINEL)
            .into_iter()
            .filter(|l| !l.contains(UNRELATED))
            .collect();
        assert!(
            noise.is_empty(),
            "a pass that re-lists only known grants logs nothing at INFO: {noise:?}"
        );
    }
}
