//! Durable delivery for a gate continuation's OUTCOME post — plan
//! `2026-10-29-a-gate-continuation-spawned-through-a-claim-error-never-records-its-consume`,
//! Phase 2.
//!
//! ## The defect this closes
//!
//! `agent_runtime::post_continuation_outcome` used to make ONE 5 s attempt at
//! `POST /coord/gates/{gate_id}/continuation-consumed` and swallow a failure.
//! On the `SpawnDespiteClaimError` path (the consume CLAIM failed in transport
//! and the runner spawned anyway, availability over consistency) the `spawned`
//! OUTCOME is the only write that ever tells coord the row was consumed —
//! coord accepts an OUTCOME-first post as "the claiming call itself". During
//! the 2026-10-22 coord brownout the claim and the outcome posts each failed
//! hundreds of times, so coord read those rows as dispatched-and-never-consumed
//! and later expired them `ttl_7d_elapsed` while their sessions were running.
//!
//! ## What this module is
//!
//! A producer and a drain arm for ONE session-outbox kind,
//! [`SessionEventKind::GateContinuationOutcome`], mirroring
//! [`super::closeout_spool`]: its own module, a synthetic lane per gate, and
//! one install over the ONE shared [`OutboxWriter`].
//!
//! **Write-ahead.** A durable outcome is written to the outbox BEFORE the
//! direct post ([`Installer::write_ahead`]); the direct post is then the fast
//! path, and its answer only decides whether that row is ACKed now or left for
//! the drain:
//!
//! - a 2xx coord confirms (`consumed: true`) — the row is ACKed at once;
//! - a 400 / 404 / 409 / 422 — coord answered on the content, so the row is
//!   ACK-dropped and the refusal logged ([`handle_direct_failure`]);
//! - anything else (transport error, timeout, 5xx, 429, 401, 403, 408, an
//!   unconfirmed 2xx) — the row stays and the `coord_sync` drain retries it.
//!
//! So a runner that crashes during the 5 s attempt still has the record on
//! disk. A replay of a record coord already took is harmless: the route is
//! idempotent and the replay classifier ACKs a matching echo.
//!
//! [`push`] is the drain arm `coord_sync::push_record` dispatches the kind to.
//! It classifies coord's answer ([`classify_replay_answer`]) into ACK, retry,
//! or ACK-drop.
//!
//! ## Before the outbox exists
//!
//! `main.rs` builds the session outbox inside Tauri's `.setup()`, after the
//! agent runtime has started. An outcome posted in that window is held in a
//! bounded in-memory buffer ([`Installer`]) and written to the outbox when
//! [`install`] runs, instead of being lost.
//!
//! ## Lane, ordering, and the drop paths it opts out of
//!
//! One lane per GATE ([`lane`]), so a `spawned` queued for a gate stays ahead
//! of that gate's later `work_unreported`, and a retrying record never
//! head-of-line blocks a session's own `started`/`heartbeat`/`closed` chain. An
//! outcome for a gate that already has a pending record is written BEHIND it
//! and not posted directly ([`WriteAhead::Behind`]), so the fast path cannot
//! overtake the queue.
//!
//! The kind is NOT best-effort (`coord_sync::is_best_effort_kind`), so it is
//! never Ack-dropped after `BEST_EFFORT_MAX_ATTEMPTS`; it is exempt from the
//! serving-failure quarantine (`coord_sync::is_quarantine_exempt_kind`); and it
//! is exempt from the outbox's byte-cap trim (`local_store::is_trim_exempt_kind`),
//! which otherwise drops the OLDEST unacked rows first — exactly these, during
//! a long outage. Backoff is the drain's per-lane backoff (1 s doubling to
//! 60 s), kept across the lane's records.
//!
//! ## Dedupe
//!
//! "The same gate enqueued twice yields one coord-visible record" means: an
//! enqueue whose `(gate_id, outcome, dispatched_at)` is identical to a record
//! still PENDING in the outbox is not appended a second time
//! ([`Enqueued::Duplicate`]). A different outcome for the same gate is a
//! different fact and is appended, in order, on the same lane. Before
//! answering `Duplicate` the in-memory book is re-checked against the outbox,
//! so a record that left the outbox without the book hearing of it is written
//! again rather than silently skipped.
//!
//! ## Episode identity
//!
//! A record can arrive days late, after a re-clearance started a NEW dispatch
//! episode. The payload therefore carries the `dispatched_at` the runner polled
//! from `pending-continuations`, passed through verbatim, and coord answers a
//! stale one with `200` + `outcome_recorded: null` (terminal, logged with
//! coord's `stale_episode` detail). Unknown (a WS-delivered frame carries none)
//! means the field is omitted, which is coord's pre-guard behaviour.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use super::local_store::{OutboxRecord, OutboxWriter};
use super::SessionEventKind;

/// One queued OUTCOME post. This is the outbox payload, verbatim.
///
/// `dispatched_at` is the string coord served in `pending-continuations`,
/// never reformatted: coord's episode guard compares it to the row's own
/// `continuation_dispatched_at` to the microsecond.
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

/// Whether an outcome post earns durable delivery at all.
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

/// The statuses coord answers on the CONTENT of an outcome post: a retry would
/// get the same answer, so the record is terminal. 400/404/422 refuse the body
/// or the gate; 409 is coord's verdict on the row (cancelled, superseded, …).
/// Every other 4xx — 401, 403, 408, … — is about the request, not the record,
/// and is retried.
fn is_content_refusal(status: u16) -> bool {
    matches!(status, 400 | 404 | 409 | 422)
}

/// Why a DIRECT outcome post did not produce a confirmed answer.
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
    /// Whether the record should stay queued for the drain: everything except
    /// a content refusal ([`is_content_refusal`]).
    pub fn is_spoolable(&self) -> bool {
        match self {
            Self::Status { status, .. } => !is_content_refusal(*status),
            Self::Transport(_) | Self::NoClient => true,
        }
    }

    /// A 4xx retried anyway (401, 403, 408, …) is logged at ERROR: it is
    /// usually a credential or proxy fault someone has to fix.
    fn is_refused_retry(&self) -> bool {
        matches!(self, Self::Status { status, .. }
            if (400..500).contains(status) && *status != 429 && !is_content_refusal(*status))
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
    /// A new outbox row was written. `behind` is true when an earlier record
    /// for the same gate is still pending ahead of it.
    Queued { lane: Uuid, seq: i64, behind: bool },
    /// An identical record was already pending; nothing was written.
    Duplicate { lane: Uuid },
}

/// What [`Installer::write_ahead`] did with an outcome before its direct post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteAhead {
    /// Written (to the outbox, or to the pre-install buffer) and first in its
    /// gate's queue: post it directly, then settle it by the answer.
    Head,
    /// Written behind an earlier pending record for the same gate: do NOT post
    /// directly; the drain posts the lane in order.
    Behind,
    /// An identical record is already pending: do NOT post directly.
    Duplicate,
    /// This outcome earns no durable delivery ([`durable_retry_applies`]):
    /// nothing was written, the direct post is the only attempt.
    NotDurable,
    /// The outbox write failed: post directly, and the outcome is LOST if that
    /// fails too.
    WriteFailed(String),
}

impl WriteAhead {
    /// Whether the caller should make the direct attempt.
    pub fn posts_directly(&self) -> bool {
        matches!(self, Self::Head | Self::NotDurable | Self::WriteFailed(_))
    }
}

/// What [`handle_direct_failure`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureDisposition {
    /// The record stays queued for the drain.
    Queued,
    /// coord answered on the content (400/404/409/422): logged, not queued
    /// (a written-ahead row is ACK-dropped).
    NotSpoolable,
    /// This outcome earns no durable delivery ([`durable_retry_applies`]).
    NotDurable,
    /// Nothing durable holds it — the outcome is LOST, and said so.
    Lost(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingKey {
    outcome: String,
    dispatched_at: Option<String>,
}

/// The in-memory view of the pending outcome rows: per gate, each record's key
/// and outbox seq (the seq is what [`ContinuationOutcomeOutbox::complete`]
/// ACKs). `seen_dropped` is the outbox's trim counter at the last reconcile.
#[derive(Default)]
struct Book {
    by_gate: HashMap<Uuid, Vec<(PendingKey, i64)>>,
    seen_dropped: u64,
}

/// Writer for the outcome kind, over the SAME [`OutboxWriter`] the
/// `CoordSync` drain reads (a second writer over one file mints colliding
/// seqs — see [`super::closeout_spool::CloseoutSpool`]).
///
/// It keeps an in-memory book of the records still pending per gate, seeded
/// from the outbox on construction (so a restart sees the records the last
/// process left). The book is what makes the dedupe and the "behind a queued
/// record" check cheap: the outbox file can be tens of MiB of transcript
/// chunks, and an outcome post must not rescan it.
pub struct ContinuationOutcomeOutbox {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
    book: Mutex<Book>,
}

impl ContinuationOutcomeOutbox {
    /// Build over `outbox`, seeding the pending book from its un-ACKed rows.
    pub fn new(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> Self {
        let by_gate = match scan_pending(&outbox) {
            Ok(by_gate) => by_gate,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "continuation-outcome: could not read the outbox to seed the pending book — \
                     a replayed record may be enqueued a second time (coord is idempotent on it)"
                );
                HashMap::new()
            }
        };
        let seen_dropped = outbox.dropped_unacked();
        Self {
            outbox,
            machine_id,
            book: Mutex::new(Book {
                by_gate,
                seen_dropped,
            }),
        }
    }

    /// The outbox this writes to — the one `CoordSync` drains.
    pub fn outbox(&self) -> &OutboxWriter {
        &self.outbox
    }

    /// Whether a record for `gate_id` is still pending.
    pub fn has_pending(&self, gate_id: Uuid) -> bool {
        self.lock()
            .by_gate
            .get(&gate_id)
            .is_some_and(|keys| !keys.is_empty())
    }

    /// Durably enqueue `rec` on its gate's lane, unless an identical record is
    /// already pending (see the module's "Dedupe").
    pub fn enqueue(&self, rec: &ContinuationOutcomeRecord) -> std::io::Result<Enqueued> {
        let lane = lane(rec.gate_id);
        // The book lock is held across the append, so two concurrent enqueues
        // of one record cannot both miss the other. Lock order is always
        // book → the outbox's own write lock.
        let mut book = self.lock();
        if self.outbox.dropped_unacked() != book.seen_dropped {
            // The cap trim dropped unacked rows since we last looked. Our kind
            // is exempt from it, but the book must agree with the file, so
            // re-read it and say which records (if any) are gone.
            self.reconcile_locked(&mut book, true);
        }
        let key = rec.key();
        let listed = |book: &Book| {
            book.by_gate
                .get(&rec.gate_id)
                .is_some_and(|keys| keys.iter().any(|(k, _)| *k == key))
        };
        if listed(&book) {
            // Confirm against the outbox before trusting the book: a record
            // that left the file without the book hearing of it must be
            // written again, not reported as a duplicate.
            self.reconcile_locked(&mut book, false);
            if listed(&book) {
                return Ok(Enqueued::Duplicate { lane });
            }
        }
        let behind = book
            .by_gate
            .get(&rec.gate_id)
            .is_some_and(|keys| !keys.is_empty());
        let payload = serde_json::to_value(rec).map_err(std::io::Error::other)?;
        let row = self.outbox.record(
            self.machine_id,
            lane,
            SessionEventKind::GateContinuationOutcome,
            payload,
        )?;
        book.by_gate
            .entry(rec.gate_id)
            .or_default()
            .push((key, row.seq));
        Ok(Enqueued::Queued {
            lane,
            seq: row.seq,
            behind,
        })
    }

    /// ACK the pending row for `rec` — a direct post reached a terminal
    /// answer, so the drain must not replay it. `false` when no such row was
    /// pending. An ACK write failure leaves the row for the drain, whose
    /// replay coord answers idempotently.
    pub fn complete(&self, rec: &ContinuationOutcomeRecord) -> bool {
        let mut book = self.lock();
        let key = rec.key();
        let Some(seq) = remove_key(&mut book, rec.gate_id, &key) else {
            return false;
        };
        if let Err(e) = self.outbox.ack(&[(lane(rec.gate_id), seq)]) {
            tracing::warn!(
                "continuation-outcome gate_id={} outcome={} answered, but its outbox row could \
                 not be ACKed ({e}) — the drain will replay it (coord is idempotent on it)",
                rec.gate_id,
                rec.outcome,
            );
        }
        true
    }

    /// Forget one pending `rec` — the drain reached a terminal verdict on it
    /// and ACKs the row itself.
    fn settle(&self, rec: &ContinuationOutcomeRecord) {
        let mut book = self.lock();
        remove_key(&mut book, rec.gate_id, &rec.key());
    }

    /// Rebuild the book from the outbox. With `after_trim`, every record the
    /// book listed that the outbox no longer holds is logged at ERROR by gate:
    /// it left the outbox without coord ever answering it.
    fn reconcile_locked(&self, book: &mut Book, after_trim: bool) {
        let fresh = match scan_pending(&self.outbox) {
            Ok(fresh) => fresh,
            Err(e) => {
                tracing::warn!(
                    "continuation-outcome: could not re-read the outbox to reconcile the \
                     pending book ({e}) — keeping the book as it is"
                );
                return;
            }
        };
        if after_trim {
            for (gate, keys) in &book.by_gate {
                for (key, seq) in keys {
                    let still = fresh
                        .get(gate)
                        .is_some_and(|f| f.iter().any(|(k, s)| k == key && s == seq));
                    if !still {
                        tracing::error!(
                            "continuation-outcome record gate_id={gate} outcome={} \
                             dispatched_at={} seq={seq} is no longer in the outbox after a cap \
                             trim — it was LOST before coord answered it",
                            key.outcome,
                            key.dispatched_at.as_deref().unwrap_or("<unknown>"),
                        );
                    }
                }
            }
        }
        book.by_gate = fresh;
        book.seen_dropped = self.outbox.dropped_unacked();
    }

    fn lock(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn remove_key(book: &mut Book, gate_id: Uuid, key: &PendingKey) -> Option<i64> {
    let keys = book.by_gate.get_mut(&gate_id)?;
    let i = keys.iter().position(|(k, _)| k == key)?;
    let (_, seq) = keys.remove(i);
    if keys.is_empty() {
        book.by_gate.remove(&gate_id);
    }
    Some(seq)
}

/// The pending outcome rows of `outbox`, by gate, in seq order.
fn scan_pending(outbox: &OutboxWriter) -> std::io::Result<HashMap<Uuid, Vec<(PendingKey, i64)>>> {
    let mut by_gate: HashMap<Uuid, Vec<(PendingKey, i64)>> = HashMap::new();
    for row in outbox
        .pending()?
        .iter()
        .filter(|r| r.event_kind == SessionEventKind::GateContinuationOutcome.as_str())
    {
        if let Ok(rec) = serde_json::from_value::<ContinuationOutcomeRecord>(row.payload.clone()) {
            by_gate
                .entry(rec.gate_id)
                .or_default()
                .push((rec.key(), row.seq));
        }
    }
    Ok(by_gate)
}

/// Most records held before the outbox exists. Far above any real boot-window
/// count (one per continuation outcome posted in the first seconds); when it
/// is exceeded the OLDEST is dropped with an ERROR naming it.
const PRE_INSTALL_CAP: usize = 1024;

/// The install point for the outcome store, plus the bounded buffer that holds
/// outcomes posted before the store exists.
///
/// `install` and `write_ahead` both take the buffer lock, and `install` sets
/// the store while holding it, so a record is either buffered before the
/// install (and flushed by it) or written to the store after it — never
/// stranded in the buffer.
pub struct Installer {
    store: OnceLock<Arc<ContinuationOutcomeOutbox>>,
    buffer: Mutex<VecDeque<ContinuationOutcomeRecord>>,
}

impl Default for Installer {
    fn default() -> Self {
        Self::new()
    }
}

impl Installer {
    pub const fn new() -> Self {
        Self {
            store: OnceLock::new(),
            buffer: Mutex::new(VecDeque::new()),
        }
    }

    /// Install the store and write every buffered record into it, in order.
    /// `false` (and nothing changes) when a store was already installed.
    pub fn install(&self, store: Arc<ContinuationOutcomeOutbox>) -> bool {
        let mut buf = self.lock_buffer();
        if self.store.set(store.clone()).is_err() {
            return false;
        }
        for rec in buf.drain(..) {
            match store.enqueue(&rec) {
                Ok(enqueued) => tracing::info!(
                    "continuation-outcome gate_id={} outcome={} held before the outbox \
                     existed is now queued ({enqueued:?})",
                    rec.gate_id,
                    rec.outcome,
                ),
                Err(e) => tracing::error!(
                    "continuation-outcome gate_id={} outcome={} device_id={} was held before \
                     the outbox existed and could not be written to it ({e}) — LOST",
                    rec.gate_id,
                    rec.outcome,
                    rec.device_id,
                ),
            }
        }
        true
    }

    /// The installed store, if any.
    pub fn store(&self) -> Option<Arc<ContinuationOutcomeOutbox>> {
        self.store.get().cloned()
    }

    /// Write a durable outcome down BEFORE its direct post (see the module's
    /// "Write-ahead"). Never blocks on the network.
    pub fn write_ahead(&self, rec: &ContinuationOutcomeRecord, durable: bool) -> WriteAhead {
        if !durable {
            return WriteAhead::NotDurable;
        }
        let mut buf = self.lock_buffer();
        if let Some(store) = self.store.get().cloned() {
            drop(buf);
            return match store.enqueue(rec) {
                Ok(Enqueued::Queued { behind: false, .. }) => WriteAhead::Head,
                Ok(Enqueued::Queued { behind: true, .. }) => {
                    tracing::warn!(
                        "continuation-outcome queued for retry gate_id={} outcome={} \
                         device_id={} cause=behind_a_record_already_queued_for_this_gate",
                        rec.gate_id,
                        rec.outcome,
                        rec.device_id,
                    );
                    WriteAhead::Behind
                }
                Ok(Enqueued::Duplicate { .. }) => WriteAhead::Duplicate,
                Err(e) => {
                    tracing::error!(
                        "continuation-outcome gate_id={} outcome={} could not be written to the \
                         outbox ({e}) — posting directly with no durable copy",
                        rec.gate_id,
                        rec.outcome,
                    );
                    WriteAhead::WriteFailed(e.to_string())
                }
            };
        }
        if buf.iter().any(|r| r == rec) {
            return WriteAhead::Duplicate;
        }
        let behind = buf.iter().any(|r| r.gate_id == rec.gate_id);
        if buf.len() >= PRE_INSTALL_CAP {
            if let Some(lost) = buf.pop_front() {
                tracing::error!(
                    "continuation-outcome gate_id={} outcome={} device_id={} was held before the \
                     outbox existed and the {PRE_INSTALL_CAP}-record buffer is full — LOST",
                    lost.gate_id,
                    lost.outcome,
                    lost.device_id,
                );
            }
        }
        buf.push_back(rec.clone());
        tracing::warn!(
            "continuation-outcome gate_id={} outcome={} posted before the session outbox \
             exists — held in memory until it is installed",
            rec.gate_id,
            rec.outcome,
        );
        if behind {
            WriteAhead::Behind
        } else {
            WriteAhead::Head
        }
    }

    /// The direct post reached a terminal answer: remove the written-ahead
    /// copy (from the buffer, or ACK its outbox row) so nothing replays it.
    pub fn complete(&self, rec: &ContinuationOutcomeRecord) {
        let mut buf = self.lock_buffer();
        if let Some(i) = buf.iter().position(|r| r == rec) {
            buf.remove(i);
            return;
        }
        let store = self.store.get().cloned();
        drop(buf);
        if let Some(store) = store {
            store.complete(rec);
        }
    }

    /// Records held in the pre-install buffer.
    pub fn buffered(&self) -> usize {
        self.lock_buffer().len()
    }

    fn lock_buffer(&self) -> MutexGuard<'_, VecDeque<ContinuationOutcomeRecord>> {
        self.buffer.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Process-wide install point, filled once by `main.rs` from the SAME outbox
/// `Arc` the drain reads — the [`super::closeout_spool`] pattern, for the same
/// reason: the caller (`agent_runtime`) has no state handle to thread it
/// through, and the outbox is a process singleton.
static GLOBAL: Installer = Installer::new();

/// Install the process-wide store, flushing anything held before it. `false`
/// if one was already installed (the second call is ignored).
pub fn install(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> bool {
    GLOBAL.install(Arc::new(ContinuationOutcomeOutbox::new(outbox, machine_id)))
}

/// The process-wide install point every production post goes through.
pub fn installer() -> &'static Installer {
    &GLOBAL
}

/// The installed store, or `None` before `main.rs` installs it.
pub fn global() -> Option<Arc<ContinuationOutcomeOutbox>> {
    GLOBAL.store()
}

/// After a DIRECT post that did not get a 2xx: decide what happens to the
/// written-ahead copy, and log it. The single place a direct refusal is
/// turned into "not queued".
pub fn handle_direct_failure(
    installer: &Installer,
    written: &WriteAhead,
    failure: &DirectFailure,
    rec: &ContinuationOutcomeRecord,
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
        if *written == WriteAhead::Head {
            installer.complete(rec);
        }
        return FailureDisposition::NotSpoolable;
    }
    match written {
        WriteAhead::Head | WriteAhead::Behind | WriteAhead::Duplicate => {
            if failure.is_refused_retry() {
                tracing::error!(
                    "continuation-outcome POST gate_id={} outcome={} device_id={} refused \
                     ({cause}) — kept queued and retried: a 401/403/408 is about the request, \
                     not the record (check the device credential)",
                    rec.gate_id,
                    rec.outcome,
                    rec.device_id,
                );
            } else {
                tracing::warn!(
                    "continuation-outcome queued for retry gate_id={} outcome={} device_id={} \
                     dispatched_at={} cause={cause}",
                    rec.gate_id,
                    rec.outcome,
                    rec.device_id,
                    rec.dispatched_at.as_deref().unwrap_or("<unknown>"),
                );
            }
            FailureDisposition::Queued
        }
        WriteAhead::NotDurable => {
            tracing::warn!(
                "continuation-outcome POST gate_id={} outcome={} device_id={} failed ({cause}) — \
                 not queued: a spawn_failed on an unconfirmed claim is left for the replay \
                 backstop, which re-serves the unconsumed row",
                rec.gate_id,
                rec.outcome,
                rec.device_id,
            );
            FailureDisposition::NotDurable
        }
        WriteAhead::WriteFailed(e) => {
            tracing::error!(
                "continuation-outcome POST gate_id={} outcome={} device_id={} failed ({cause}) \
                 and its outbox write had failed ({e}) — the outcome is LOST",
                rec.gate_id,
                rec.outcome,
                rec.device_id,
            );
            FailureDisposition::Lost(e.clone())
        }
    }
}

/// After a DIRECT post that got a 2xx: ACK the written-ahead copy when coord
/// confirmed the consume, keep it for the drain when it did not. Returns
/// whether the answer was terminal.
pub fn handle_direct_answer(
    installer: &Installer,
    written: &WriteAhead,
    rec: &ContinuationOutcomeRecord,
    status: u16,
    body: &str,
) -> bool {
    let answer = classify_replay_answer(rec, status, body);
    let (verdict, is_error) = disposition(&answer, body);
    if let ReplayVerdict::Retry(why) = &verdict {
        if *written == WriteAhead::Head {
            tracing::warn!(
                "continuation-outcome queued for retry gate_id={} outcome={} device_id={} \
                 cause=unconfirmed_2xx ({why})",
                rec.gate_id,
                rec.outcome,
                rec.device_id,
            );
        }
        return false;
    }
    if is_error {
        log_terminal_error(rec, status, &answer, body);
    }
    if *written == WriteAhead::Head {
        installer.complete(rec);
    }
    true
}

// ---------------------------------------------------------------------------
// Episode book — the `dispatched_at` a live continuation was spawned under
// ---------------------------------------------------------------------------

/// Upper bound on remembered episodes. At the cap the OLDEST entry is evicted
/// (an evicted entry only means a later `work_unreported` omits the field,
/// which is coord's pre-guard behaviour).
const EPISODE_BOOK_CAP: usize = 4096;

#[derive(Default)]
struct EpisodeBook {
    /// gate → (insertion stamp, dispatched_at).
    entries: HashMap<Uuid, (u64, String)>,
    next: u64,
}

impl EpisodeBook {
    /// Insert, evicting the single OLDEST entry when a new gate would exceed
    /// `cap`. The scan is O(n) and runs only at the cap.
    fn remember(&mut self, gate_id: Uuid, at: &str, cap: usize) {
        if self.entries.len() >= cap && !self.entries.contains_key(&gate_id) {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (stamp, _))| *stamp)
                .map(|(g, _)| *g);
            if let Some(g) = oldest {
                self.entries.remove(&g);
            }
        }
        let stamp = self.next;
        self.next += 1;
        self.entries.insert(gate_id, (stamp, at.to_string()));
    }

    fn take(&mut self, gate_id: Uuid) -> Option<String> {
        self.entries.remove(&gate_id).map(|(_, at)| at)
    }
}

fn episode_book() -> MutexGuard<'static, EpisodeBook> {
    static BOOK: OnceLock<Mutex<EpisodeBook>> = OnceLock::new();
    BOOK.get_or_init(|| Mutex::new(EpisodeBook::default()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Remember the `dispatched_at` gate `gate_id` was claimed under, so the PTY
/// exit fallback (which knows only the gate id) can name the same episode.
pub fn remember_episode(gate_id: Uuid, dispatched_at: Option<&str>) {
    if let Some(at) = dispatched_at {
        episode_book().remember(gate_id, at, EPISODE_BOOK_CAP);
    }
}

/// Take the remembered episode for `gate_id` (`None` when unknown). Also how
/// a path that will never post `work_unreported` forgets it.
pub fn take_episode(gate_id: Uuid) -> Option<String> {
    episode_book().take(gate_id)
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
    /// Terminal: coord refused the content (400/404/422); ACK-drop the row.
    Drop(String),
}

/// What coord's answer to an outcome post means. Pure over
/// `(record, status, body)` so every arm is testable without a coord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayAnswer {
    /// 2xx, `consumed: true`, and `outcome_recorded` is the token we posted.
    Recorded,
    /// 2xx, and a session work outcome (`work_completed` / `work_abandoned`)
    /// or the runner's own `work_unreported` stands — the legal supersede.
    SupersededByWork(String),
    /// 2xx, and some OTHER standing value: our write did not land.
    Superseded(String),
    /// 2xx with `outcome_recorded: null`: a foreign consumer, a rerouted row,
    /// or a stale episode. D3's duplicate-run evidence.
    NullEcho,
    /// 2xx, `consumed: true`, and no `outcome_recorded` field: a coord that
    /// predates the echo took it.
    NoEcho,
    /// 2xx whose body is not JSON carrying `consumed: true`: nothing confirms
    /// the write landed, so it is retried.
    Unconfirmed,
    /// 409 `{"error": "cancelled"}`.
    Cancelled(Option<String>),
    /// 409 with another `error` (`superseded` / `rerouted` / unparsed).
    Conflict(String),
    /// A status worth retrying quietly (5xx, 429, anything not 4xx).
    RetryStatus(u16),
    /// A 4xx about the request rather than the record (401, 403, 408, …):
    /// retried, at ERROR.
    RefusedRetry(u16),
    /// A status coord will repeat for this body (400, 404, 422).
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

/// Classify coord's answer to an outcome post.
pub fn classify_replay_answer(
    rec: &ContinuationOutcomeRecord,
    status: u16,
    body: &str,
) -> ReplayAnswer {
    let parsed: Option<JsonValue> = serde_json::from_str(body).ok();
    if (200..300).contains(&status) {
        let confirmed = parsed
            .as_ref()
            .and_then(|v| v.get("consumed"))
            .and_then(JsonValue::as_bool)
            == Some(true);
        if !confirmed {
            return ReplayAnswer::Unconfirmed;
        }
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
    if is_content_refusal(status) {
        return ReplayAnswer::DropStatus(status);
    }
    if (400..500).contains(&status) && status != 429 {
        return ReplayAnswer::RefusedRetry(status);
    }
    ReplayAnswer::RetryStatus(status)
}

/// Map an answer onto the drain verdict, and whether it is ERROR-worthy (a
/// terminal answer that means the consume did NOT land the way the runner
/// recorded it, or a retry someone has to fix). Pure, so the ACK/retry/drop
/// table is pinned by a test.
pub fn disposition(answer: &ReplayAnswer, body: &str) -> (ReplayVerdict, bool) {
    match answer {
        ReplayAnswer::Recorded | ReplayAnswer::SupersededByWork(_) | ReplayAnswer::NoEcho => {
            (ReplayVerdict::Acked, false)
        }
        ReplayAnswer::Superseded(_)
        | ReplayAnswer::NullEcho
        | ReplayAnswer::Cancelled(_)
        | ReplayAnswer::Conflict(_) => (ReplayVerdict::Acked, true),
        ReplayAnswer::Unconfirmed => (
            ReplayVerdict::Retry(format!("2xx without consumed:true: {}", bounded(body))),
            false,
        ),
        ReplayAnswer::RetryStatus(status) => (
            ReplayVerdict::Retry(format!("{status}: {}", bounded(body))),
            false,
        ),
        ReplayAnswer::RefusedRetry(status) => (
            ReplayVerdict::Retry(format!("{status}: {}", bounded(body))),
            true,
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
    log_replay(&rec, status, &answer, &verdict, is_error, &body);
    if !matches!(verdict, ReplayVerdict::Retry(_)) {
        if let Some(store) = global() {
            store.settle(&rec);
        }
    }
    verdict
}

/// Coord's own explanation of a refusal or a null echo, when it sent one: the
/// `stale_episode` object (the episode guard) and any `expired` detail.
fn coord_detail(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<JsonValue>(body) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for field in ["stale_episode", "expired", "expired_at", "expired_reason"] {
        if let Some(value) = v.get(field).filter(|x| !x.is_null()) {
            parts.push(format!("{field}={value}"));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" coord_detail: {}", parts.join(" "))
    }
}

fn log_terminal_error(
    rec: &ContinuationOutcomeRecord,
    status: u16,
    answer: &ReplayAnswer,
    body: &str,
) {
    let session = rec.claude_code_session_id.as_deref().unwrap_or("<none>");
    let episode = rec.dispatched_at.as_deref().unwrap_or("<unknown>");
    let detail = coord_detail(body);
    tracing::error!(
        "continuation-outcome gate_id={} outcome={} device_id={} \
         claude_code_session_id={session} dispatched_at={episode} HTTP {status} \
         answer={answer:?}{detail} — coord did NOT record this outcome; terminal, not retried \
         (a null echo, a differing standing outcome or a refusal means another device, a \
         reroute or a newer episode owns the row: a possible duplicate run, made visible here)",
        rec.gate_id,
        rec.outcome,
        rec.device_id,
    );
}

fn log_replay(
    rec: &ContinuationOutcomeRecord,
    status: u16,
    answer: &ReplayAnswer,
    verdict: &ReplayVerdict,
    is_error: bool,
    body: &str,
) {
    match verdict {
        ReplayVerdict::Retry(_) if is_error => tracing::error!(
            "continuation-outcome replay gate_id={} outcome={} device_id={} answered HTTP \
             {status} — retrying on the lane's backoff; a 401/403/408 is about the request, \
             not the record (check the device credential)",
            rec.gate_id,
            rec.outcome,
            rec.device_id,
        ),
        ReplayVerdict::Retry(_) => tracing::warn!(
            "continuation-outcome replay gate_id={} outcome={} answered HTTP {status} \
             ({answer:?}) — retrying on the lane's backoff",
            rec.gate_id,
            rec.outcome,
        ),
        ReplayVerdict::Acked if !is_error => tracing::info!(
            "continuation-outcome replay acked gate_id={} outcome={} device_id={} answer={answer:?}",
            rec.gate_id,
            rec.outcome,
            rec.device_id,
        ),
        _ => log_terminal_error(rec, status, answer, body),
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

/// A raw-TCP fake of coord's consume route, shared by the drain tests in
/// `coord_sync` and the wiring tests in `agent_runtime`. Raw TCP rather than
/// axum, because a TRANSPORT failure (a connection closed with no response) is
/// one of the answers it has to give.
#[cfg(test)]
pub(crate) mod fake_coord {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Arc;

    use serde_json::{json, Value as JsonValue};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex as TokMutex;
    use uuid::Uuid;

    #[derive(Debug, Clone)]
    pub(crate) enum Reply {
        /// Read the request, then close the connection with no response.
        Drop,
        Status(u16, JsonValue),
    }

    /// coord's confirmed 200 for `outcome`.
    pub(crate) fn recorded(outcome: &str) -> Reply {
        Reply::Status(200, json!({"consumed": true, "outcome_recorded": outcome}))
    }

    #[derive(Default)]
    pub(crate) struct Script {
        /// Scripted answers per gate; once a gate's queue is empty it answers
        /// `200 {"consumed": true, "outcome_recorded": <the posted outcome>}`.
        pub(crate) replies: HashMap<Uuid, VecDeque<Reply>>,
        /// Every request that reached the fake: `(gate, body, status)`, with
        /// status `0` for a dropped connection.
        pub(crate) seen: Vec<(Uuid, JsonValue, u16)>,
    }

    impl Script {
        pub(crate) fn answered(&self, gate: Uuid) -> Vec<u16> {
            self.seen
                .iter()
                .filter(|(g, _, _)| *g == gate)
                .map(|(_, _, s)| *s)
                .collect()
        }
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, JsonValue)> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let n = sock.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let path = head.split_whitespace().nth(1)?.to_string();
        let len = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                if k.trim().eq_ignore_ascii_case("content-length") {
                    v.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        while buf.len() < header_end + len {
            let n = sock.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let end = (header_end + len).min(buf.len());
        let body = serde_json::from_slice(&buf[header_end..end]).unwrap_or(JsonValue::Null);
        Some((path, body))
    }

    /// Spawn the fake; returns its base URL and the shared script.
    pub(crate) async fn spawn() -> (String, Arc<TokMutex<Script>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let script = Arc::new(TokMutex::new(Script::default()));
        let shared = script.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let script = shared.clone();
                tokio::spawn(async move {
                    let Some((path, body)) = read_request(&mut sock).await else {
                        return;
                    };
                    let gate = path
                        .strip_prefix("/coord/gates/")
                        .and_then(|rest| rest.split('/').next())
                        .and_then(|g| Uuid::parse_str(g).ok())
                        .unwrap_or_default();
                    let reply = {
                        let mut s = script.lock().await;
                        let reply = s
                            .replies
                            .get_mut(&gate)
                            .and_then(VecDeque::pop_front)
                            .unwrap_or_else(|| {
                                Reply::Status(
                                    200,
                                    json!({
                                        "gate_id": gate,
                                        "consumed": true,
                                        "outcome_recorded": body["outcome"],
                                    }),
                                )
                            });
                        let status = match &reply {
                            Reply::Drop => 0,
                            Reply::Status(s, _) => *s,
                        };
                        s.seen.push((gate, body.clone(), status));
                        reply
                    };
                    match reply {
                        Reply::Drop => drop(sock),
                        Reply::Status(status, json) => {
                            let payload = json.to_string();
                            let resp = format!(
                                "HTTP/1.1 {status} Scripted\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                payload.len()
                            );
                            let _ = sock.write_all(resp.as_bytes()).await;
                            let _ = sock.shutdown().await;
                        }
                    }
                });
            }
        });
        (format!("http://{addr}"), script)
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

    /// An [`Installer`] with a fresh store installed, and that store's outbox.
    fn installed() -> (Installer, Arc<OutboxWriter>, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        let inst = Installer::new();
        assert!(inst.install(Arc::new(ContinuationOutcomeOutbox::new(
            outbox.clone(),
            Uuid::new_v4()
        ))));
        (inst, outbox, dir)
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
        assert!(matches!(first, Enqueued::Queued { behind: false, .. }));
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

    /// A record that left the outbox without the book hearing of it is written
    /// again, not reported as a duplicate.
    #[test]
    fn a_record_gone_from_the_outbox_is_requeued_not_deduped() {
        let (s, _d) = store();
        let g = Uuid::new_v4();
        let Enqueued::Queued { seq, .. } = s.enqueue(&record(g, "spawned", None)).unwrap() else {
            panic!("first enqueue must write");
        };
        // Out of band: the row leaves the outbox, and the book is not told.
        s.outbox().ack(&[(lane(g), seq)]).unwrap();
        assert!(s.outbox().pending().unwrap().is_empty());
        assert!(matches!(
            s.enqueue(&record(g, "spawned", None)).unwrap(),
            Enqueued::Queued { .. }
        ));
        assert_eq!(s.outbox().pending().unwrap().len(), 1);
    }

    /// The byte-cap trim drops the OLDEST unacked rows first; a queued consume
    /// outcome is exempt, so a long outage cannot evict it, and the book still
    /// agrees with the file afterwards.
    #[test]
    fn the_cap_trim_never_evicts_a_queued_outcome() {
        let dir = tempdir().unwrap();
        let cap = 8 * 1024;
        let outbox = Arc::new(
            OutboxWriter::open_with_max_bytes(dir.path().join("outbox.jsonl"), cap).unwrap(),
        );
        let s = ContinuationOutcomeOutbox::new(outbox.clone(), Uuid::new_v4());
        let g = Uuid::new_v4();
        // The OLDEST unacked row in the file.
        s.enqueue(&record(
            g,
            "spawned",
            Some("2026-10-22T18:48:46.123456+00:00"),
        ))
        .unwrap();
        // Fill far past the cap with another kind.
        let filler = Uuid::new_v4();
        for i in 0..200 {
            outbox
                .record(
                    Uuid::new_v4(),
                    filler,
                    SessionEventKind::Heartbeat,
                    json!({ "i": i, "pad": "x".repeat(200) }),
                )
                .unwrap();
        }
        assert!(outbox.dropped_unacked() > 0, "the trim ran");
        let rows = outbox.pending().unwrap();
        assert_eq!(
            rows.iter().filter(|r| r.session_id == lane(g)).count(),
            1,
            "the consume outcome survived the trim"
        );
        // The book reconciles after the trim and still dedupes the survivor.
        assert_eq!(
            s.enqueue(&record(
                g,
                "spawned",
                Some("2026-10-22T18:48:46.123456+00:00")
            ))
            .unwrap(),
            Enqueued::Duplicate { lane: lane(g) }
        );
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

    /// Written ahead, then: a direct 400/404/409/422 is coord answering on the
    /// content — the row is ACK-dropped. 401/403/408/429/5xx/transport keep it.
    #[test]
    fn only_the_retryable_class_of_direct_failure_is_queued() {
        let (inst, outbox, _d) = installed();
        for status in [400u16, 404, 409, 422] {
            let rec = record(Uuid::new_v4(), "spawned", None);
            let wa = inst.write_ahead(&rec, true);
            assert_eq!(wa, WriteAhead::Head);
            let d = handle_direct_failure(
                &inst,
                &wa,
                &DirectFailure::Status {
                    status,
                    body: r#"{"error":"cancelled"}"#.into(),
                },
                &rec,
            );
            assert_eq!(d, FailureDisposition::NotSpoolable, "{status}");
        }
        assert!(
            outbox.pending().unwrap().is_empty(),
            "a direct content refusal leaves nothing queued"
        );
        let kept = [
            DirectFailure::Status {
                status: 503,
                body: String::new(),
            },
            DirectFailure::Status {
                status: 429,
                body: String::new(),
            },
            DirectFailure::Status {
                status: 401,
                body: String::new(),
            },
            DirectFailure::Status {
                status: 403,
                body: String::new(),
            },
            DirectFailure::Status {
                status: 408,
                body: String::new(),
            },
            DirectFailure::Transport("[timeout] operation timed out".into()),
            DirectFailure::NoClient,
        ];
        for failure in &kept {
            let rec = record(Uuid::new_v4(), "spawned", None);
            let wa = inst.write_ahead(&rec, true);
            let d = handle_direct_failure(&inst, &wa, failure, &rec);
            assert_eq!(d, FailureDisposition::Queued, "{failure:?}");
        }
        assert_eq!(outbox.pending().unwrap().len(), kept.len());
        assert!(DirectFailure::Status {
            status: 401,
            body: String::new()
        }
        .is_refused_retry());
        assert!(!DirectFailure::Status {
            status: 429,
            body: String::new()
        }
        .is_refused_retry());
    }

    /// A confirmed 2xx ACKs the written-ahead row; an unconfirmed one keeps it.
    #[test]
    fn a_confirmed_direct_2xx_acks_the_written_ahead_row() {
        let (inst, outbox, _d) = installed();
        let rec = record(Uuid::new_v4(), "spawned", None);
        let wa = inst.write_ahead(&rec, true);
        assert_eq!(
            outbox.pending().unwrap().len(),
            1,
            "written before the post"
        );
        assert!(!handle_direct_answer(&inst, &wa, &rec, 200, "{}"));
        assert_eq!(outbox.pending().unwrap().len(), 1, "unconfirmed: kept");
        assert!(handle_direct_answer(
            &inst,
            &wa,
            &rec,
            200,
            r#"{"consumed":true,"outcome_recorded":"spawned"}"#
        ));
        assert!(outbox.pending().unwrap().is_empty(), "confirmed: ACKed");
    }

    /// `spawn_failed` after a claim ERROR is not written; after a confirmed
    /// claim it is. `spawned` and `work_unreported` always are.
    #[test]
    fn spawn_failed_is_durable_only_on_a_confirmed_claim() {
        assert!(!durable_retry_applies("spawn_failed", false));
        assert!(durable_retry_applies("spawn_failed", true));
        assert!(durable_retry_applies("spawned", false));
        assert!(durable_retry_applies("work_unreported", false));

        let (inst, outbox, _d) = installed();
        let rec = record(Uuid::new_v4(), "spawn_failed", None);
        let wa = inst.write_ahead(&rec, durable_retry_applies("spawn_failed", false));
        assert_eq!(wa, WriteAhead::NotDurable);
        let d = handle_direct_failure(
            &inst,
            &wa,
            &DirectFailure::Transport("[connect] refused".into()),
            &rec,
        );
        assert_eq!(d, FailureDisposition::NotDurable);
        assert!(outbox.pending().unwrap().is_empty());
    }

    /// Before the store is installed a durable outcome is held in memory, a
    /// terminal direct answer removes it, and the install writes the rest to
    /// the outbox in order.
    #[test]
    fn an_outcome_posted_before_install_is_held_then_flushed() {
        let inst = Installer::new();
        let g = Uuid::new_v4();
        let spawned = record(g, "spawned", None);
        let unreported = record(g, "work_unreported", None);
        let answered = record(Uuid::new_v4(), "spawned", None);
        assert_eq!(inst.write_ahead(&spawned, true), WriteAhead::Head);
        assert_eq!(inst.write_ahead(&unreported, true), WriteAhead::Behind);
        assert_eq!(inst.write_ahead(&spawned, true), WriteAhead::Duplicate);
        assert_eq!(inst.write_ahead(&answered, true), WriteAhead::Head);
        assert_eq!(inst.buffered(), 3);
        // The direct post of `answered` got coord's confirmation.
        inst.complete(&answered);
        assert_eq!(inst.buffered(), 2);

        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        assert!(inst.install(Arc::new(ContinuationOutcomeOutbox::new(
            outbox.clone(),
            Uuid::new_v4()
        ))));
        assert_eq!(inst.buffered(), 0);
        let rows = outbox.pending().unwrap();
        let outcomes: Vec<&str> = rows
            .iter()
            .map(|r| r.payload["outcome"].as_str().unwrap())
            .collect();
        assert_eq!(outcomes, ["spawned", "work_unreported"]);
        // A second install is refused and changes nothing.
        let (other, _d2) = store();
        assert!(!inst.install(Arc::new(other)));
        // After install, a write-ahead goes to the store.
        let late = record(Uuid::new_v4(), "spawned", None);
        assert_eq!(inst.write_ahead(&late, true), WriteAhead::Head);
        assert_eq!(outbox.pending().unwrap().len(), 3);
    }

    /// A gate with a queued record writes its next outcome behind it, so a
    /// direct `work_unreported` cannot overtake a queued `spawned`.
    #[test]
    fn a_post_for_a_gate_with_a_queued_record_goes_behind_it() {
        let (inst, outbox, _d) = installed();
        let g = Uuid::new_v4();
        assert_eq!(
            inst.write_ahead(&record(g, "spawned", None), true),
            WriteAhead::Head
        );
        assert_eq!(
            inst.write_ahead(&record(g, "work_unreported", None), true),
            WriteAhead::Behind
        );
        assert!(!WriteAhead::Behind.posts_directly());
        assert!(!WriteAhead::Duplicate.posts_directly());
        assert!(WriteAhead::Head.posts_directly());
        // A non-durable outcome is never written.
        assert_eq!(
            inst.write_ahead(&record(g, "spawn_failed", None), false),
            WriteAhead::NotDurable
        );
        let rows = outbox.pending().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].payload["outcome"], json!("work_unreported"));
        // Settling the head leaves the second still pending.
        let s = inst.store().unwrap();
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
                r#"{"consumed":true,"outcome_recorded":"spawned"}"#,
                ReplayAnswer::Recorded,
                ReplayVerdict::Acked,
                false,
            ),
            (
                200,
                r#"{"consumed":true,"outcome_recorded":"work_completed"}"#,
                ReplayAnswer::SupersededByWork("work_completed".into()),
                ReplayVerdict::Acked,
                false,
            ),
            (
                200,
                r#"{"consumed":true,"outcome_recorded":"spawn_failed: x"}"#,
                ReplayAnswer::Superseded("spawn_failed: x".into()),
                ReplayVerdict::Acked,
                true,
            ),
            (
                200,
                r#"{"consumed":true,"outcome_recorded":null}"#,
                ReplayAnswer::NullEcho,
                ReplayVerdict::Acked,
                true,
            ),
            (
                200,
                r#"{"consumed":true}"#,
                ReplayAnswer::NoEcho,
                ReplayVerdict::Acked,
                false,
            ),
            (
                200,
                r#"{}"#,
                ReplayAnswer::Unconfirmed,
                ReplayVerdict::Retry("2xx without consumed:true: {}".into()),
                false,
            ),
            (
                200,
                "not json",
                ReplayAnswer::Unconfirmed,
                ReplayVerdict::Retry("2xx without consumed:true: not json".into()),
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
                401,
                "who",
                ReplayAnswer::RefusedRetry(401),
                ReplayVerdict::Retry("401: who".into()),
                true,
            ),
            (
                403,
                "no",
                ReplayAnswer::RefusedRetry(403),
                ReplayVerdict::Retry("403: no".into()),
                true,
            ),
            (
                408,
                "slow",
                ReplayAnswer::RefusedRetry(408),
                ReplayVerdict::Retry("408: slow".into()),
                true,
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
            (
                422,
                "shape",
                ReplayAnswer::DropStatus(422),
                ReplayVerdict::Drop("422: shape".into()),
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
                r#"{"consumed":true,"outcome_recorded":"work_unreported: exit=crashed code=1"}"#
            ),
            ReplayAnswer::Recorded
        );
        // A null echo's explanation reaches the log.
        let detail = coord_detail(
            r#"{"consumed":true,"outcome_recorded":null,"stale_episode":{"row":"2026-10-25T00:00:00+00:00"}}"#,
        );
        assert!(detail.contains("stale_episode="), "{detail}");
        assert_eq!(coord_detail(r#"{"consumed":true}"#), "");
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

    /// At the cap the OLDEST entry is evicted, not the whole book.
    #[test]
    fn the_episode_book_evicts_the_oldest_entry_at_the_cap() {
        let mut book = EpisodeBook::default();
        let gates: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        for (i, g) in gates.iter().enumerate() {
            book.remember(*g, &format!("e{i}"), 3);
        }
        assert_eq!(book.entries.len(), 3, "one in, one out — never cleared");
        assert_eq!(book.take(gates[0]), None, "the oldest was evicted");
        assert_eq!(book.take(gates[1]).as_deref(), Some("e1"));
        assert_eq!(book.take(gates[3]).as_deref(), Some("e3"));
        // Re-remembering a present gate at the cap evicts nothing.
        book.remember(gates[2], "again", 1);
        assert_eq!(book.take(gates[2]).as_deref(), Some("again"));
    }
}
