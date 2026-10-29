//! Durable retry for a gate continuation's OUTCOME post — plan
//! `2026-10-29-a-gate-continuation-spawned-through-a-claim-error-never-records-its-consume`,
//! Phase 2.
//!
//! ## The defect this closes
//!
//! `agent_runtime::post_continuation_outcome` makes ONE 5 s attempt at
//! `POST /coord/gates/{gate_id}/continuation-consumed` and swallows a failure.
//! On the `SpawnDespiteClaimError` path (the consume CLAIM failed in transport
//! and the runner spawned anyway, availability over consistency) the `spawned`
//! OUTCOME is the only write that ever tells coord the row was consumed —
//! coord accepts an OUTCOME-first post as "the claiming call itself". During
//! the 2026-10-22 coord brownout both posts failed for hundreds of gates, so
//! coord read each row as dispatched-and-never-consumed and later expired it
//! `ttl_7d_elapsed` while its session was still running.
//!
//! ## What this module is
//!
//! A producer and a drain arm for ONE session-outbox kind,
//! [`SessionEventKind::GateContinuationOutcome`], mirroring
//! [`super::closeout_spool`]: its own module, a synthetic lane, the shared
//! write classifier, and one install over the ONE shared [`OutboxWriter`].
//!
//! - [`handle_direct_failure`] is what `post_continuation_outcome` calls when
//!   its direct attempt did not reach a coord that could answer. It enqueues
//!   ONLY the retryable class (transport error, timeout, 5xx, 429). A direct
//!   400/404/409 is coord answering on the content and is not enqueued.
//! - [`push`] is the drain arm `coord_sync::push_record` dispatches the kind
//!   to. It classifies coord's answer ([`classify_replay_answer`]) into ACK,
//!   retry, or ACK-drop.
//!
//! ## Lane, ordering, and the two drop paths it opts out of
//!
//! One lane per GATE ([`lane`]), so a `spawned` queued for a gate stays ahead
//! of that gate's later `work_unreported`, and a retrying record never
//! head-of-line blocks a session's own `started`/`heartbeat`/`closed` chain.
//! A gate's direct post is routed BEHIND a record already queued for it
//! ([`queue_behind_pending`]), so the fast path cannot overtake the queue.
//!
//! The kind is NOT best-effort (`coord_sync::is_best_effort_kind`), so it is
//! never Ack-dropped after `BEST_EFFORT_MAX_ATTEMPTS`, and it is exempt from
//! the serving-failure quarantine (`coord_sync::is_quarantine_exempt_kind`):
//! a partial brownout that 500s this route while coord serves others would
//! otherwise quarantine the lane in about three minutes, which is fire-few,
//! not durable. Backoff is the drain's existing per-lane backoff (1 s doubling
//! to 60 s), kept across the lane's records, and in-flight pushes are bounded
//! by `MAX_CONCURRENT_PUSH_CHAINS`.
//!
//! ## Dedupe
//!
//! "The same gate enqueued twice yields one coord-visible record" means: an
//! enqueue whose `(gate_id, outcome, dispatched_at)` is identical to a record
//! still PENDING in the outbox is not appended a second time
//! ([`Enqueued::Duplicate`]). A different outcome for the same gate is a
//! different fact and is appended, in order, on the same lane. Coord's route is
//! itself idempotent on a repeated outcome, so the residual window (a record
//! whose ACK write failed after coord took it) replays as a no-op.
//!
//! ## Episode identity
//!
//! A record can arrive days late, after a re-clearance started a NEW dispatch
//! episode. The payload therefore carries the `dispatched_at` the runner polled
//! from `pending-continuations`, echoed verbatim, and coord answers a stale one
//! with `200` + `outcome_recorded: null` (handled as terminal below). Unknown
//! (a WS-delivered frame carries none) means the field is omitted, which is
//! coord's pre-guard behaviour.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use super::closeout_spool::{classify_coord_write_status, CoordWriteClass};
use super::local_store::{OutboxRecord, OutboxWriter};
use super::SessionEventKind;

/// One queued OUTCOME post. This is the outbox payload, verbatim.
///
/// `dispatched_at` is the string coord served in `pending-continuations`,
/// never reformatted: coord's episode guard compares it to the row's own
/// `continuation_dispatched_at`, and a re-rendered timestamp that names the
/// same instant in a different spelling would read as a stale episode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationOutcomeRecord {
    pub gate_id: Uuid,
    pub device_id: Uuid,
    /// coord's wire token: `spawned` / `spawn_failed` / `work_unreported`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_at: Option<String>,
}

impl ContinuationOutcomeRecord {
    /// The OUTCOME-shape body of `POST /coord/gates/{gate_id}/continuation-consumed`.
    ///
    /// Absent optionals are OMITTED, never sent as `null`: an absent
    /// `dispatched_at` is "episode unknown" and must keep coord's pre-guard
    /// behaviour, which a `null` might not.
    pub fn wire_body(&self) -> JsonValue {
        let mut body = json!({
            "device_id": self.device_id,
            "outcome": self.outcome,
        });
        let obj = body.as_object_mut().expect("json! object");
        if let Some(d) = &self.detail {
            obj.insert("detail".into(), JsonValue::String(d.clone()));
        }
        if let Some(s) = &self.claude_code_session_id {
            obj.insert(
                "claude_code_session_id".into(),
                JsonValue::String(s.clone()),
            );
        }
        if let Some(at) = &self.dispatched_at {
            obj.insert("dispatched_at".into(), JsonValue::String(at.clone()));
        }
        body
    }

    fn key(&self) -> PendingKey {
        PendingKey {
            outcome: self.outcome.clone(),
            dispatched_at: self.dispatched_at.clone(),
        }
    }
}

/// The synthetic outbox lane for one gate's outcome records. Coord never sees
/// this id; the drain arm builds its URL from the payload's `gate_id`.
pub fn lane(gate_id: Uuid) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("continuation-outcome:{gate_id}").as_bytes(),
    )
}

/// Whether a failed direct post of `outcome` earns a durable retry at all.
///
/// - `spawned` — always. On the claim-error path it is the only record of the
///   consume, which is the whole defect.
/// - `work_unreported` — always. It is a real outcome on a consumed row, and
///   it rides the same lane as `spawned`, so it stays behind it.
/// - `spawn_failed` — only when coord CONFIRMED the claim (a 200). On an
///   unclaimed row, an unrecorded `spawn_failed` leaves the row pending and the
///   replay backstop re-serves it on every poll (at-least-once). A late durable
///   `spawn_failed` would instead consume and retire a row nothing ever ran.
pub fn durable_retry_applies(outcome: &str, claim_confirmed: bool) -> bool {
    match outcome {
        "spawn_failed" => claim_confirmed,
        _ => true,
    }
}

/// Why a DIRECT outcome post did not produce a verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectFailure {
    /// coord answered a non-2xx status.
    Status { status: u16, body: String },
    /// The request never got an answer: connect, TLS, DNS, a timeout. Carries
    /// the rendered cause chain.
    Transport(String),
    /// The runner had no shared coord client to send with. The drain has its
    /// own client, so this is retryable.
    NoClient,
}

impl DirectFailure {
    /// The retryable class: a transport failure, a 5xx, or a 429. A 429 is
    /// coord pacing this runner, not refusing the content, so it is retried
    /// here although [`classify_coord_write_status`] files 4xx as permanent for
    /// the closeout spool.
    pub fn is_spoolable(&self) -> bool {
        match self {
            Self::Status { status, .. } => {
                *status == 429 || classify_coord_write_status(*status) == CoordWriteClass::Spoolable
            }
            Self::Transport(_) | Self::NoClient => true,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Status { status, body } => format!("HTTP {status}: {}", bounded(body)),
            Self::Transport(cause) => cause.clone(),
            Self::NoClient => "no shared coord client".to_string(),
        }
    }
}

/// What [`ContinuationOutcomeOutbox::enqueue`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enqueued {
    /// A new outbox row was written.
    Queued { lane: Uuid, seq: i64 },
    /// An identical record was already pending; nothing was written.
    Duplicate { lane: Uuid },
}

/// What [`handle_direct_failure`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureDisposition {
    /// The record is durably queued (newly, or it already was).
    Queued(Enqueued),
    /// coord answered on the content (400/404/409…): logged, not queued.
    NotSpoolable,
    /// This outcome earns no durable retry ([`durable_retry_applies`]).
    NotDurable,
    /// No durable store is installed — the outcome is LOST, and said so.
    NoStore,
    /// The outbox write itself failed — the outcome is LOST, and said so.
    WriteFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingKey {
    outcome: String,
    dispatched_at: Option<String>,
}

/// Writer for the outcome kind, over the SAME [`OutboxWriter`] the
/// `CoordSync` drain reads (a second writer over one file mints colliding
/// seqs — see [`super::closeout_spool::CloseoutSpool`]).
///
/// It keeps an in-memory book of the records still pending per gate, seeded
/// from the outbox on construction (so a restart sees the records the last
/// process left). The book is what makes the dedupe and the
/// route-behind-a-queued-record check cheap: the outbox file can be tens of
/// MiB of transcript chunks, and an outcome post must not rescan it.
pub struct ContinuationOutcomeOutbox {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
    pending: Mutex<HashMap<Uuid, Vec<PendingKey>>>,
}

impl ContinuationOutcomeOutbox {
    /// Build over `outbox`, seeding the pending book from its un-ACKed rows.
    pub fn new(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> Self {
        let mut book: HashMap<Uuid, Vec<PendingKey>> = HashMap::new();
        match outbox.pending() {
            Ok(rows) => {
                for row in rows
                    .iter()
                    .filter(|r| r.event_kind == SessionEventKind::GateContinuationOutcome.as_str())
                {
                    if let Ok(rec) =
                        serde_json::from_value::<ContinuationOutcomeRecord>(row.payload.clone())
                    {
                        book.entry(rec.gate_id).or_default().push(rec.key());
                    }
                }
            }
            Err(e) => tracing::warn!(
                error = %e,
                "continuation-outcome: could not read the outbox to seed the pending book — \
                 a replayed record may be enqueued a second time (coord is idempotent on it)"
            ),
        }
        Self {
            outbox,
            machine_id,
            pending: Mutex::new(book),
        }
    }

    /// The outbox this writes to — the one `CoordSync` drains.
    pub fn outbox(&self) -> &OutboxWriter {
        &self.outbox
    }

    /// Whether a record for `gate_id` is still pending.
    pub fn has_pending(&self, gate_id: Uuid) -> bool {
        self.book()
            .get(&gate_id)
            .is_some_and(|keys| !keys.is_empty())
    }

    /// Durably enqueue `rec` on its gate's lane, unless an identical record is
    /// already pending (see the module's "Dedupe").
    pub fn enqueue(&self, rec: &ContinuationOutcomeRecord) -> std::io::Result<Enqueued> {
        let lane = lane(rec.gate_id);
        // The book lock is held across the append, so two concurrent enqueues
        // of one record cannot both miss the other.
        let mut book = self.book();
        let key = rec.key();
        if book
            .get(&rec.gate_id)
            .is_some_and(|keys| keys.contains(&key))
        {
            return Ok(Enqueued::Duplicate { lane });
        }
        let payload = serde_json::to_value(rec).map_err(std::io::Error::other)?;
        let row = self.outbox.record(
            self.machine_id,
            lane,
            SessionEventKind::GateContinuationOutcome,
            payload,
        )?;
        book.entry(rec.gate_id).or_default().push(key);
        Ok(Enqueued::Queued { lane, seq: row.seq })
    }

    /// Forget one pending `rec` — the drain reached a terminal verdict on it.
    fn settle(&self, rec: &ContinuationOutcomeRecord) {
        let mut book = self.book();
        if let Some(keys) = book.get_mut(&rec.gate_id) {
            let key = rec.key();
            if let Some(i) = keys.iter().position(|k| *k == key) {
                keys.remove(i);
            }
            if keys.is_empty() {
                book.remove(&rec.gate_id);
            }
        }
    }

    fn book(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Vec<PendingKey>>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Process-wide handle, installed once by `main.rs` from the SAME outbox `Arc`
/// the drain reads — the [`super::closeout_spool`] pattern, for the same
/// reason: the caller (`agent_runtime`) has no state handle to thread it
/// through, and the outbox is a process singleton.
static GLOBAL: OnceLock<Arc<ContinuationOutcomeOutbox>> = OnceLock::new();

/// Install the process-wide store. `false` if one was already installed (the
/// second call is ignored).
pub fn install(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> bool {
    GLOBAL
        .set(Arc::new(ContinuationOutcomeOutbox::new(outbox, machine_id)))
        .is_ok()
}

/// The installed store, or `None` when the session subsystem never came up.
/// `None` means "no durable store": a caller must report the outcome as LOST.
pub fn global() -> Option<Arc<ContinuationOutcomeOutbox>> {
    GLOBAL.get().cloned()
}

/// Before a direct post: when a record for this gate is already queued, append
/// this one BEHIND it instead of posting directly, so the lane's order holds
/// (a direct `work_unreported` must not overtake a queued `spawned`).
///
/// Returns `true` when the record was handed to the queue and the caller must
/// NOT post it directly. A non-durable outcome is never routed (`false`).
pub fn queue_behind_pending(
    store: Option<&ContinuationOutcomeOutbox>,
    rec: &ContinuationOutcomeRecord,
    durable: bool,
) -> bool {
    let Some(store) = store.filter(|s| durable && s.has_pending(rec.gate_id)) else {
        return false;
    };
    match store.enqueue(rec) {
        Ok(enqueued) => {
            tracing::warn!(
                "continuation-outcome queued for retry gate_id={} outcome={} device_id={} \
                 cause=behind_a_record_already_queued_for_this_gate ({enqueued:?})",
                rec.gate_id,
                rec.outcome,
                rec.device_id,
            );
            true
        }
        // The queue could not take it: fall back to the direct attempt, which
        // is strictly better than doing nothing.
        Err(e) => {
            tracing::error!(
                "continuation-outcome gate_id={} outcome={} could not be queued behind the \
                 pending record ({e}) — posting directly",
                rec.gate_id,
                rec.outcome,
            );
            false
        }
    }
}

/// After a failed DIRECT outcome post: the single place that decides whether
/// the outcome is durably queued, and logs what happened to it.
///
/// `durable` is [`durable_retry_applies`] for this post. The caller must not
/// call this when no coord base is configured (there is nothing to retry
/// against).
pub fn handle_direct_failure(
    store: Option<&ContinuationOutcomeOutbox>,
    failure: &DirectFailure,
    rec: &ContinuationOutcomeRecord,
    durable: bool,
) -> FailureDisposition {
    let cause = failure.describe();
    if !failure.is_spoolable() {
        // coord reached a verdict on the content; a retry replays it.
        tracing::warn!(
            "continuation-outcome POST gate_id={} outcome={} device_id={} refused by coord \
             ({cause}) — not queued",
            rec.gate_id,
            rec.outcome,
            rec.device_id,
        );
        return FailureDisposition::NotSpoolable;
    }
    if !durable {
        tracing::warn!(
            "continuation-outcome POST gate_id={} outcome={} device_id={} failed ({cause}) — \
             not queued: a spawn_failed on an unconfirmed claim is left for the replay \
             backstop, which re-serves the unconsumed row",
            rec.gate_id,
            rec.outcome,
            rec.device_id,
        );
        return FailureDisposition::NotDurable;
    }
    let Some(store) = store else {
        tracing::error!(
            "continuation-outcome POST gate_id={} outcome={} device_id={} failed ({cause}) and \
             NO durable store is installed — the outcome is LOST",
            rec.gate_id,
            rec.outcome,
            rec.device_id,
        );
        return FailureDisposition::NoStore;
    };
    match store.enqueue(rec) {
        Ok(enqueued) => {
            tracing::warn!(
                "continuation-outcome queued for retry gate_id={} outcome={} device_id={} \
                 dispatched_at={} cause={cause} ({enqueued:?})",
                rec.gate_id,
                rec.outcome,
                rec.device_id,
                rec.dispatched_at.as_deref().unwrap_or("<unknown>"),
            );
            FailureDisposition::Queued(enqueued)
        }
        Err(e) => {
            tracing::error!(
                "continuation-outcome POST gate_id={} outcome={} device_id={} failed ({cause}) \
                 and the outbox write failed ({e}) — the outcome is LOST",
                rec.gate_id,
                rec.outcome,
                rec.device_id,
            );
            FailureDisposition::WriteFailed(e.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Episode book — the `dispatched_at` a live continuation was spawned under
// ---------------------------------------------------------------------------

/// Upper bound on remembered episodes; past it the book is cleared rather than
/// grown (a cleared entry only means a later `work_unreported` omits the field,
/// which is coord's pre-guard behaviour).
const EPISODE_BOOK_CAP: usize = 4096;

fn episode_book() -> &'static Mutex<HashMap<Uuid, String>> {
    static BOOK: OnceLock<Mutex<HashMap<Uuid, String>>> = OnceLock::new();
    BOOK.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Remember the `dispatched_at` gate `gate_id` was claimed under, so the PTY
/// exit fallback (which knows only the gate id) can name the same episode.
pub fn remember_episode(gate_id: Uuid, dispatched_at: Option<&str>) {
    let Some(at) = dispatched_at else {
        return;
    };
    let mut book = episode_book().lock().unwrap_or_else(|p| p.into_inner());
    if book.len() >= EPISODE_BOOK_CAP {
        book.clear();
    }
    book.insert(gate_id, at.to_string());
}

/// Take the remembered episode for `gate_id` (`None` when unknown).
pub fn take_episode(gate_id: Uuid) -> Option<String> {
    episode_book()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&gate_id)
}

// ---------------------------------------------------------------------------
// Drain arm
// ---------------------------------------------------------------------------

/// The drain's verdict on one record, mapped by `coord_sync::push_record` onto
/// its own `PushOutcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayVerdict {
    /// Terminal: ACK the row (coord recorded it, or answered in a way no retry
    /// can change).
    Acked,
    /// Retry on the lane's backoff. The string leads with the HTTP status when
    /// there was one, which the drain's failure typing reads.
    Retry(String),
    /// Terminal: coord refused the content (400/404/…); ACK-drop the row.
    Drop(String),
}

/// What coord's answer to a replayed OUTCOME post means. Pure over
/// `(record, status, body)` so every arm is testable without a coord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayAnswer {
    /// 200, and `outcome_recorded` is the token we posted.
    Recorded,
    /// 200, and a session work outcome (`work_completed` / `work_abandoned`)
    /// or the runner's own `work_unreported` stands — the legal supersede.
    SupersededByWork(String),
    /// 200, and some OTHER standing value: our write did not land.
    Superseded(String),
    /// 200 with `outcome_recorded: null`: a foreign consumer, a rerouted row,
    /// or a stale episode. D3's duplicate-run evidence.
    NullEcho,
    /// 200 with no readable `outcome_recorded`: coord took it and said nothing.
    NoEcho,
    /// 409 `{"error": "cancelled"}`.
    Cancelled(Option<String>),
    /// 409 with another `error` (`superseded` / `rerouted` / unparsed).
    Conflict(String),
    /// A status worth retrying (5xx, 429).
    RetryStatus(u16),
    /// A status coord will repeat for this body (400, 404, other 4xx).
    DropStatus(u16),
}

fn marker_matches(marker: &str, recorded: &str) -> bool {
    recorded == marker
        || recorded
            .strip_prefix(marker)
            .is_some_and(|rest| rest.starts_with(": "))
}

fn is_work_outcome(recorded: &str) -> bool {
    ["work_completed", "work_abandoned", "work_unreported"]
        .iter()
        .any(|m| marker_matches(m, recorded))
}

/// Classify coord's answer to a replayed outcome post.
pub fn classify_replay_answer(
    rec: &ContinuationOutcomeRecord,
    status: u16,
    body: &str,
) -> ReplayAnswer {
    let parsed: Option<JsonValue> = serde_json::from_str(body).ok();
    if (200..300).contains(&status) {
        return match parsed.as_ref().and_then(|v| v.get("outcome_recorded")) {
            Some(JsonValue::Null) => ReplayAnswer::NullEcho,
            Some(JsonValue::String(recorded)) if marker_matches(&rec.outcome, recorded) => {
                ReplayAnswer::Recorded
            }
            Some(JsonValue::String(recorded)) if is_work_outcome(recorded) => {
                ReplayAnswer::SupersededByWork(recorded.clone())
            }
            Some(JsonValue::String(recorded)) => ReplayAnswer::Superseded(recorded.clone()),
            _ => ReplayAnswer::NoEcho,
        };
    }
    if status == 409 {
        let error = parsed
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(JsonValue::as_str);
        return match error {
            Some("cancelled") => ReplayAnswer::Cancelled(
                parsed
                    .as_ref()
                    .and_then(|v| v.get("cancel_reason"))
                    .and_then(JsonValue::as_str)
                    .map(str::to_string),
            ),
            Some(other) => ReplayAnswer::Conflict(other.to_string()),
            None => ReplayAnswer::Conflict(format!("unparsed: {}", bounded(body))),
        };
    }
    if status == 429 || classify_coord_write_status(status) == CoordWriteClass::Spoolable {
        ReplayAnswer::RetryStatus(status)
    } else {
        ReplayAnswer::DropStatus(status)
    }
}

/// Map an answer onto the drain verdict, and whether it is ERROR-worthy (a
/// terminal answer that means the consume did NOT land the way the runner
/// recorded it). Pure, so the ACK/retry/drop table is pinned by a test.
pub fn disposition(answer: &ReplayAnswer, body: &str) -> (ReplayVerdict, bool) {
    match answer {
        ReplayAnswer::Recorded | ReplayAnswer::SupersededByWork(_) | ReplayAnswer::NoEcho => {
            (ReplayVerdict::Acked, false)
        }
        ReplayAnswer::Superseded(_) => (ReplayVerdict::Acked, false),
        ReplayAnswer::NullEcho | ReplayAnswer::Cancelled(_) | ReplayAnswer::Conflict(_) => {
            (ReplayVerdict::Acked, true)
        }
        ReplayAnswer::RetryStatus(status) => (
            ReplayVerdict::Retry(format!("{status}: {}", bounded(body))),
            false,
        ),
        ReplayAnswer::DropStatus(status) => (
            ReplayVerdict::Drop(format!("{status}: {}", bounded(body))),
            true,
        ),
    }
}

/// The drain arm: replay one queued outcome under the DEVICE credential.
///
/// `TenantScope::Device`, not the lane's session tenant: the lane is synthetic,
/// so a session lookup resolves `Unresolved`, and coord binds the
/// `claude_code_session_id` only for a request carrying a principal. The
/// route itself is keyed `gate_id + device_id` with no tenant dimension — the
/// same credential the direct post presents (`attach_device_auth`).
pub(crate) async fn push(http: &reqwest::Client, base: &str, row: &OutboxRecord) -> ReplayVerdict {
    let rec = match serde_json::from_value::<ContinuationOutcomeRecord>(row.payload.clone()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                session = %row.session_id,
                seq = row.seq,
                "continuation-outcome replay: unreadable payload ({e}) — ACK-dropping"
            );
            return ReplayVerdict::Drop(format!(
                "unreadable gate_continuation_outcome payload: {e}"
            ));
        }
    };
    let url = format!(
        "{}/coord/gates/{}/continuation-consumed",
        base.trim_end_matches('/'),
        rec.gate_id
    );
    let sent = crate::auth::attach_device_auth_for(
        http.post(&url).json(&rec.wire_body()),
        crate::auth::TenantScope::Device,
    )
    .send()
    .await;
    let resp = match sent {
        Ok(r) => r,
        Err(e) => {
            let cause = super::coord_sync::transport_error(&e);
            tracing::warn!(
                "continuation-outcome replay gate_id={} outcome={} failed in transport \
                 ({cause}) — retrying on the lane's backoff",
                rec.gate_id,
                rec.outcome,
            );
            return ReplayVerdict::Retry(cause);
        }
    };
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    let answer = classify_replay_answer(&rec, status, &body);
    let (verdict, is_error) = disposition(&answer, &body);
    log_replay(&rec, status, &answer, &verdict, is_error);
    if !matches!(verdict, ReplayVerdict::Retry(_)) {
        if let Some(store) = global() {
            store.settle(&rec);
        }
    }
    verdict
}

fn log_replay(
    rec: &ContinuationOutcomeRecord,
    status: u16,
    answer: &ReplayAnswer,
    verdict: &ReplayVerdict,
    is_error: bool,
) {
    let session = rec.claude_code_session_id.as_deref().unwrap_or("<none>");
    let episode = rec.dispatched_at.as_deref().unwrap_or("<unknown>");
    match verdict {
        ReplayVerdict::Retry(_) => tracing::warn!(
            "continuation-outcome replay gate_id={} outcome={} answered HTTP {status} — \
             retrying on the lane's backoff",
            rec.gate_id,
            rec.outcome,
        ),
        ReplayVerdict::Acked if !is_error => tracing::info!(
            "continuation-outcome replay acked gate_id={} outcome={} device_id={} answer={answer:?}",
            rec.gate_id,
            rec.outcome,
            rec.device_id,
        ),
        _ => tracing::error!(
            "continuation-outcome replay acked gate_id={} outcome={} device_id={} \
             claude_code_session_id={session} dispatched_at={episode} HTTP {status} \
             answer={answer:?} — coord did NOT record this outcome; terminal, not retried \
             (a null echo or a refusal means another device, a reroute or a newer episode \
             owns the row: a possible duplicate run, made visible here)",
            rec.gate_id,
            rec.outcome,
            rec.device_id,
        ),
    }
}

fn bounded(body: &str) -> String {
    const CAP: usize = 300;
    if body.chars().count() <= CAP {
        body.to_string()
    } else {
        let mut s: String = body.chars().take(CAP).collect();
        s.push('…');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn record(gate: Uuid, outcome: &str, at: Option<&str>) -> ContinuationOutcomeRecord {
        ContinuationOutcomeRecord {
            gate_id: gate,
            device_id: Uuid::from_u128(0xeb21_55ed),
            outcome: outcome.to_string(),
            detail: None,
            claude_code_session_id: Some("0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".into()),
            dispatched_at: at.map(str::to_string),
        }
    }

    fn store() -> (ContinuationOutcomeOutbox, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        (ContinuationOutcomeOutbox::new(outbox, Uuid::new_v4()), dir)
    }

    /// Lane key: one lane per gate, deterministic, and distinct across gates.
    #[test]
    fn lane_is_one_per_gate() {
        let (s, _d) = store();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_eq!(lane(a), lane(a));
        assert_ne!(lane(a), lane(b));
        s.enqueue(&record(a, "spawned", None)).unwrap();
        s.enqueue(&record(a, "work_unreported", None)).unwrap();
        s.enqueue(&record(b, "spawned", None)).unwrap();
        let rows = s.outbox().pending().unwrap();
        let on = |g: Uuid| rows.iter().filter(|r| r.session_id == lane(g)).count();
        assert_eq!(on(a), 2, "two records for one gate share its lane");
        assert_eq!(on(b), 1, "a different gate is a different lane");
        // Ordered within the lane: `spawned` stays ahead of `work_unreported`.
        let a_rows: Vec<_> = rows.iter().filter(|r| r.session_id == lane(a)).collect();
        assert!(a_rows[0].seq < a_rows[1].seq);
        assert_eq!(a_rows[0].payload["outcome"], json!("spawned"));
        assert_eq!(a_rows[1].payload["outcome"], json!("work_unreported"));
    }

    /// Repetition: the same gate+outcome+episode enqueued twice is ONE row.
    #[test]
    fn the_same_record_enqueued_twice_is_one_pending_row() {
        let (s, _d) = store();
        let g = Uuid::new_v4();
        let first = s
            .enqueue(&record(g, "spawned", Some("2026-10-22T18:48:46.1Z")))
            .unwrap();
        let second = s
            .enqueue(&record(g, "spawned", Some("2026-10-22T18:48:46.1Z")))
            .unwrap();
        assert!(matches!(first, Enqueued::Queued { .. }));
        assert_eq!(second, Enqueued::Duplicate { lane: lane(g) });
        assert_eq!(s.outbox().pending().unwrap().len(), 1);
        // A different episode is a different fact.
        s.enqueue(&record(g, "spawned", Some("2026-10-25T00:00:00Z")))
            .unwrap();
        assert_eq!(s.outbox().pending().unwrap().len(), 2);
    }

    /// Restart: a fresh writer over the same file seeds its book from the
    /// un-ACKed rows, so the pending record is still known (and not re-added).
    #[test]
    fn a_fresh_writer_sees_the_records_a_previous_one_left() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let g = Uuid::new_v4();
        {
            let outbox = Arc::new(OutboxWriter::open(&path).unwrap());
            let s = ContinuationOutcomeOutbox::new(outbox, Uuid::new_v4());
            s.enqueue(&record(g, "spawned", None)).unwrap();
        }
        let outbox = Arc::new(OutboxWriter::open(&path).unwrap());
        let s = ContinuationOutcomeOutbox::new(outbox, Uuid::new_v4());
        assert!(s.has_pending(g));
        assert_eq!(
            s.enqueue(&record(g, "spawned", None)).unwrap(),
            Enqueued::Duplicate { lane: lane(g) }
        );
        assert_eq!(s.outbox().pending().unwrap().len(), 1);
    }

    /// The payload carries `dispatched_at` verbatim when known, and the wire
    /// body omits it (never `null`) when not.
    #[test]
    fn payload_carries_dispatched_at_when_known_and_omits_it_when_not() {
        let (s, _d) = store();
        let g = Uuid::new_v4();
        let served = "2026-10-22T18:48:46.123456+00:00";
        s.enqueue(&record(g, "spawned", Some(served))).unwrap();
        let row = &s.outbox().pending().unwrap()[0];
        assert_eq!(row.event_kind, "gate_continuation_outcome");
        assert_eq!(row.payload["dispatched_at"], json!(served));
        assert_eq!(row.payload["gate_id"], json!(g));
        let replayed: ContinuationOutcomeRecord =
            serde_json::from_value(row.payload.clone()).unwrap();
        assert_eq!(replayed.wire_body()["dispatched_at"], json!(served));
        assert!(
            replayed.wire_body().get("gate_id").is_none(),
            "gate rides the path"
        );

        let unknown = record(g, "work_unreported", None).wire_body();
        assert!(unknown.get("dispatched_at").is_none());
        assert_eq!(unknown["outcome"], json!("work_unreported"));
    }

    /// A direct 409 (and 400/404) is coord answering on the content: never
    /// queued. 5xx / 429 / transport are.
    #[test]
    fn only_the_retryable_class_of_direct_failure_is_queued() {
        let (s, _d) = store();
        let g = Uuid::new_v4();
        for status in [400u16, 404, 409, 422] {
            let d = handle_direct_failure(
                Some(&s),
                &DirectFailure::Status {
                    status,
                    body: r#"{"error":"cancelled"}"#.into(),
                },
                &record(g, "spawned", None),
                true,
            );
            assert_eq!(d, FailureDisposition::NotSpoolable, "{status}");
        }
        assert!(
            s.outbox().pending().unwrap().is_empty(),
            "a direct 4xx is not enqueued"
        );
        for failure in [
            DirectFailure::Status {
                status: 503,
                body: String::new(),
            },
            DirectFailure::Status {
                status: 429,
                body: String::new(),
            },
            DirectFailure::Transport("[timeout] operation timed out".into()),
            DirectFailure::NoClient,
        ] {
            let d = handle_direct_failure(
                Some(&s),
                &failure,
                &record(Uuid::new_v4(), "spawned", None),
                true,
            );
            assert!(matches!(d, FailureDisposition::Queued(_)), "{failure:?}");
        }
        assert_eq!(s.outbox().pending().unwrap().len(), 4);
    }

    /// `spawn_failed` after a claim ERROR is not queued; after a confirmed
    /// claim it is. `spawned` and `work_unreported` always are.
    #[test]
    fn spawn_failed_is_durable_only_on_a_confirmed_claim() {
        assert!(!durable_retry_applies("spawn_failed", false));
        assert!(durable_retry_applies("spawn_failed", true));
        assert!(durable_retry_applies("spawned", false));
        assert!(durable_retry_applies("work_unreported", false));

        let (s, _d) = store();
        let g = Uuid::new_v4();
        let d = handle_direct_failure(
            Some(&s),
            &DirectFailure::Transport("[connect] refused".into()),
            &record(g, "spawn_failed", None),
            durable_retry_applies("spawn_failed", false),
        );
        assert_eq!(d, FailureDisposition::NotDurable);
        assert!(s.outbox().pending().unwrap().is_empty());
    }

    #[test]
    fn no_store_is_reported_as_lost_not_queued() {
        let d = handle_direct_failure(
            None,
            &DirectFailure::Transport("x".into()),
            &record(Uuid::new_v4(), "spawned", None),
            true,
        );
        assert_eq!(d, FailureDisposition::NoStore);
    }

    /// A gate with a queued record routes its next direct post behind it, so a
    /// direct `work_unreported` cannot overtake a queued `spawned`.
    #[test]
    fn a_post_for_a_gate_with_a_queued_record_goes_behind_it() {
        let (s, _d) = store();
        let g = Uuid::new_v4();
        assert!(!queue_behind_pending(
            Some(&s),
            &record(g, "spawned", None),
            true
        ));
        s.enqueue(&record(g, "spawned", None)).unwrap();
        assert!(queue_behind_pending(
            Some(&s),
            &record(g, "work_unreported", None),
            true
        ));
        // A non-durable outcome is never routed.
        assert!(!queue_behind_pending(
            Some(&s),
            &record(g, "spawn_failed", None),
            false
        ));
        let rows = s.outbox().pending().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].payload["outcome"], json!("work_unreported"));
        // Settling the head leaves the second still pending.
        s.settle(&record(g, "spawned", None));
        assert!(s.has_pending(g));
        s.settle(&record(g, "work_unreported", None));
        assert!(!s.has_pending(g));
    }

    #[test]
    fn replay_answers_classify_and_dispose() {
        let g = Uuid::new_v4();
        let spawned = record(g, "spawned", None);
        let cases: Vec<(u16, &str, ReplayAnswer, ReplayVerdict, bool)> = vec![
            (
                200,
                r#"{"outcome_recorded":"spawned"}"#,
                ReplayAnswer::Recorded,
                ReplayVerdict::Acked,
                false,
            ),
            (
                200,
                r#"{"outcome_recorded":"work_completed"}"#,
                ReplayAnswer::SupersededByWork("work_completed".into()),
                ReplayVerdict::Acked,
                false,
            ),
            (
                200,
                r#"{"outcome_recorded":"spawn_failed: x"}"#,
                ReplayAnswer::Superseded("spawn_failed: x".into()),
                ReplayVerdict::Acked,
                false,
            ),
            (
                200,
                r#"{"outcome_recorded":null}"#,
                ReplayAnswer::NullEcho,
                ReplayVerdict::Acked,
                true,
            ),
            (
                200,
                r#"{}"#,
                ReplayAnswer::NoEcho,
                ReplayVerdict::Acked,
                false,
            ),
            (
                409,
                r#"{"error":"cancelled","cancel_reason":"withdrawn"}"#,
                ReplayAnswer::Cancelled(Some("withdrawn".into())),
                ReplayVerdict::Acked,
                true,
            ),
            (
                409,
                r#"{"error":"superseded"}"#,
                ReplayAnswer::Conflict("superseded".into()),
                ReplayVerdict::Acked,
                true,
            ),
            (
                503,
                "down",
                ReplayAnswer::RetryStatus(503),
                ReplayVerdict::Retry("503: down".into()),
                false,
            ),
            (
                429,
                "slow",
                ReplayAnswer::RetryStatus(429),
                ReplayVerdict::Retry("429: slow".into()),
                false,
            ),
            (
                404,
                "nf",
                ReplayAnswer::DropStatus(404),
                ReplayVerdict::Drop("404: nf".into()),
                true,
            ),
            (
                400,
                "bad",
                ReplayAnswer::DropStatus(400),
                ReplayVerdict::Drop("400: bad".into()),
                true,
            ),
        ];
        for (status, body, want_answer, want_verdict, want_error) in cases {
            let answer = classify_replay_answer(&spawned, status, body);
            assert_eq!(answer, want_answer, "{status} {body}");
            assert_eq!(
                disposition(&answer, body),
                (want_verdict, want_error),
                "{status} {body}"
            );
        }
        // A detail-suffixed echo of OUR token is Recorded, not a supersede.
        let unreported = record(g, "work_unreported", None);
        assert_eq!(
            classify_replay_answer(
                &unreported,
                200,
                r#"{"outcome_recorded":"work_unreported: exit=crashed code=1"}"#
            ),
            ReplayAnswer::Recorded
        );
    }

    #[test]
    fn the_episode_book_round_trips_and_ignores_unknown() {
        let g = Uuid::new_v4();
        remember_episode(g, None);
        assert_eq!(take_episode(g), None);
        remember_episode(g, Some("2026-10-22T18:48:46Z"));
        assert_eq!(take_episode(g).as_deref(), Some("2026-10-22T18:48:46Z"));
        assert_eq!(take_episode(g), None, "taken once");
    }
}
