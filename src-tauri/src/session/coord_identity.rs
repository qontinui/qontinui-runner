//! The ONE seam that re-keys a pane's `coord.sessions` row to the harness
//! (Claude Code) session id the runner has pid-exact evidence for.
//!
//! Plan `2026-10-23-a-hookless-resume-in-a-runner-pane-leaves-the-session-with-no-active-coord-row`
//! D1 / Phase 1a / Phase 2.
//!
//! Before this module the only thing that re-keyed a pane's row was the
//! provider's SessionStart hook (`POST /control/session-open`). A pane whose
//! `claude` bypassed the identity shim (an absolute-path account launcher) never
//! fires that hook, so its row kept whatever id it was first registered under —
//! and an R2-resumed pane row kept the FIRST-EVER pin it carried, across every
//! restart. Every source of identity evidence now goes through
//! [`confirm_for_terminal`] / [`confirm_for_session`]:
//!
//! | source              | evidence                                            |
//! |---------------------|-----------------------------------------------------|
//! | `hook`              | the SessionStart hook (`/control/session-open`)     |
//! | `rung2`             | the anchor's own argv (`--session-id` / `--resume`) |
//! | `provider_registry` | `<config>/sessions/<pid>.json` (Phase 1b)           |
//! | `r2_resume`         | an R2 pane resume re-keyed to the pane's pin        |
//!
//! Only pid-exact evidence may re-key a row (D5): a rung-3 transcript
//! correlation or a rung-4 guess never reaches this seam.
//!
//! ## What is counted, and when
//!
//! `Queued` means a `state_change` entered the outbox — nothing more. coord can
//! still decline it. So the drain reports each re-key's answer back into the
//! [`RekeyLedger`] the session's `CoordSync` owns: `landed` moves only on a 2xx,
//! `refused` on `409 harness_session_already_bound` (another ACTIVE row on the
//! device holds the id), and `rejected` on any other 4xx.
//!
//! ## An answer that did not land never strands the row
//!
//! `confirm_claude_code_session_id` moves the local record before coord answers,
//! and the idempotence check reads that record. So a declined re-key is RESTORED
//! here (the record goes back to what coord last accepted), and then RETRIED,
//! because evidence is not generally re-supplied: `run_reconcile` answers
//! `LeaveAlone` for a confirmed record, so a confirmed rung-2 bind, like a hook
//! or an R2 resume, happens once. Only `provider_registry` is read afresh every
//! tick. A `refused` re-key retries every tick until the competing row lapses; a
//! `rejected` one retries [`REJECTED_RETRY_LIMIT`] times and then only after
//! [`REJECTED_COOLDOWN`], so a row coord will never accept costs one PATCH an
//! hour rather than one a tick. Evidence that arrives before the pane has a coord
//! row is parked the same way, and is retried once the row exists.
//!
//! The drain never calls back into the registry: it writes the ledger, and this
//! module reads it.
//!
//! `/health` `sessionTracking.coordRekeys` serves the ledger's counters.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use super::reconcile::{BindOrigin, ReconcileAction};
use super::{SessionError, SessionRegistry, SessionState};

/// How many `rejected` answers one id gets, on the 45 s tick, before it waits
/// out [`REJECTED_COOLDOWN`] (about 30 minutes of retries).
pub const REJECTED_RETRY_LIMIT: u32 = 40;

/// How long an id that used up [`REJECTED_RETRY_LIMIT`] waits for its next round.
/// A credential outage ends and coord accepts it then, so this is a wait rather
/// than a give-up.
pub const REJECTED_COOLDOWN: Duration = Duration::from_secs(3600);

/// How many ticks parked evidence waits for its terminal's coord row. A pane's
/// row is bound within seconds of the spawn, so a terminal still without one
/// after about 30 minutes is gone.
const PARKED_TICKS: u32 = 40;

/// Which evidence asked for a re-key. The label rides the outbox row
/// (`rekey_source`, stripped before the PATCH) so the drain can count coord's
/// answer per source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RekeySource {
    /// The provider's SessionStart hook — the id it adopted, self-reported.
    Hook,
    /// Rung 2: the id the anchor claude's own argv names.
    Rung2,
    /// The provider's `<config>/sessions/<pid>.json` for the anchor pid
    /// (Phase 1b; no caller on this build).
    ProviderRegistry,
    /// An R2 pane resume, re-keyed to the pane's current pin (Phase 2).
    R2Resume,
}

impl RekeySource {
    /// Every source, in `/health` order.
    pub const ALL: [RekeySource; 4] = [
        RekeySource::Hook,
        RekeySource::Rung2,
        RekeySource::ProviderRegistry,
        RekeySource::R2Resume,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RekeySource::Hook => "hook",
            RekeySource::Rung2 => "rung2",
            RekeySource::ProviderRegistry => "provider_registry",
            RekeySource::R2Resume => "r2_resume",
        }
    }

    /// Parse an outbox row's `rekey_source`. A row with NO label was queued by
    /// a build that predates this module, where the hook was the only writer —
    /// so an absent label is `Hook`. An unknown label is `None` (not counted).
    pub fn from_payload(payload: &JsonValue) -> Option<Self> {
        match payload.get("rekey_source").and_then(JsonValue::as_str) {
            None => Some(RekeySource::Hook),
            Some(s) => Self::ALL.into_iter().find(|src| src.as_str() == s),
        }
    }

    fn index(self) -> usize {
        match self {
            RekeySource::Hook => 0,
            RekeySource::Rung2 => 1,
            RekeySource::ProviderRegistry => 2,
            RekeySource::R2Resume => 3,
        }
    }

    /// Whether a declined re-key from this source has to be retried here.
    /// Everything but `provider_registry`, which reads its id afresh every tick:
    /// a rung-2 bind stops being emitted once its record is confirmed.
    fn retried_after_decline(self) -> bool {
        !matches!(self, RekeySource::ProviderRegistry)
    }
}

/// What the seam did with one piece of identity evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RekeyOutcome {
    /// A `state_change{claude_code_session_id}` entered the outbox. Says
    /// nothing about coord's answer — that is the ledger's counters.
    Queued,
    /// The row already carries this id (locally, with no unsettled decline).
    AlreadyCurrent,
    /// The terminal has no coord row yet (or the registry does not hold it).
    NoCoordRow,
    /// The id is not a UUID; coord's PATCH would fail whole on it.
    Invalid,
    /// The local outbox write failed; the record was left as it was.
    OutboxFailed,
    /// The session is closed: nothing is running under the row any more.
    SessionClosed,
    /// coord keeps rejecting this id, so it waits out [`REJECTED_COOLDOWN`] and
    /// is retried then.
    CoolingDown,
}

/// How coord declined a re-key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclineKind {
    /// `409 harness_session_already_bound`: another ACTIVE row holds the id, and
    /// coord accepts it once that row lapses.
    AlreadyBound,
    /// Any other 4xx (the row is gone or foreign, the credential was refused).
    Rejected,
}

impl DeclineKind {
    fn as_str(self) -> &'static str {
        match self {
            DeclineKind::AlreadyBound => "harness_session_already_bound",
            DeclineKind::Rejected => "rejected",
        }
    }
}

/// A re-key coord declined, waiting for the seam to restore the local record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decline {
    /// The id coord declined to bind.
    pub refused_id: String,
    /// The record's id before that re-key moved it (from the outbox row).
    pub previous: Option<String>,
    pub source: RekeySource,
    pub kind: DeclineKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCounts {
    pub queued: u64,
    pub landed: u64,
    pub refused: u64,
    pub rejected: u64,
}

/// `rejected` answers for one id on one session.
#[derive(Debug)]
struct RejectedStreak {
    id: String,
    attempts: u32,
    cooldown_until: Option<Instant>,
}

#[derive(Debug, Default)]
struct SessionRekeyState {
    /// The `claude_code_session_id` coord last ACCEPTED (2xx) on a
    /// `state_change` for this session.
    last_landed: Option<String>,
    /// An unsettled decline, newest wins.
    declined: Option<Decline>,
    rejected: Option<RejectedStreak>,
    /// Evidence that arrived during a cooldown, retried when it ends.
    deferred: Option<(String, RekeySource)>,
}

/// Evidence for a terminal with no coord row yet.
#[derive(Debug, Clone)]
struct ParkedRekey {
    id: String,
    source: RekeySource,
    ticks_left: u32,
}

#[derive(Debug, Default)]
struct LedgerInner {
    sessions: HashMap<Uuid, SessionRekeyState>,
    parked: HashMap<String, ParkedRekey>,
    counts: [SourceCounts; 4],
    no_coord_row: u64,
    invalid: u64,
}

/// Per-`CoordSync` record of coord's answers to re-keys. Written by the drain
/// (landed / refused / rejected) and by the seam (queued / no_coord_row /
/// invalid); read by the seam and by `/health`.
#[derive(Debug, Default)]
pub struct RekeyLedger {
    inner: Mutex<LedgerInner>,
    /// Serialises moving a local record with appending its outbox row, so the
    /// hook handler and the tick cannot leave the record on one id and coord's
    /// last row on another.
    record_move: Mutex<()>,
}

impl RekeyLedger {
    fn with<R>(&self, f: impl FnOnce(&mut LedgerInner) -> R) -> R {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    }

    /// Held across a record's move and its outbox append (see `record_move`).
    pub(crate) fn record_move_guard(&self) -> MutexGuard<'_, ()> {
        self.record_move.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn note_queued(&self, source: RekeySource) {
        self.with(|l| l.counts[source.index()].queued += 1);
    }

    pub(crate) fn note_no_coord_row(&self) {
        self.with(|l| l.no_coord_row += 1);
    }

    pub(crate) fn note_invalid(&self) {
        self.with(|l| l.invalid += 1);
    }

    /// The drain saw a 2xx on a `state_change` carrying `id`.
    pub(crate) fn note_landed(&self, session: Uuid, id: &str, source: RekeySource) {
        self.with(|l| {
            l.counts[source.index()].landed += 1;
            let s = l.sessions.entry(session).or_default();
            s.last_landed = Some(id.to_string());
            s.rejected = None;
            // A decline of THIS id is moot once a later push of it landed.
            if s.declined.as_ref().is_some_and(|r| r.refused_id == id) {
                s.declined = None;
            }
        });
    }

    /// The drain saw `409 harness_session_already_bound` on a `state_change`
    /// carrying `id`.
    pub(crate) fn note_refused(
        &self,
        session: Uuid,
        id: &str,
        previous: Option<String>,
        source: RekeySource,
    ) {
        self.with(|l| {
            l.counts[source.index()].refused += 1;
            l.sessions.entry(session).or_default().record_decline(
                id,
                previous,
                source,
                DeclineKind::AlreadyBound,
            );
        });
    }

    /// The drain saw any other 4xx on a `state_change` carrying `id`: coord will
    /// not apply it, and the row leaves the queue.
    pub(crate) fn note_rejected(
        &self,
        session: Uuid,
        id: &str,
        previous: Option<String>,
        source: RekeySource,
    ) {
        self.note_rejected_at(session, id, previous, source, Instant::now());
    }

    fn note_rejected_at(
        &self,
        session: Uuid,
        id: &str,
        previous: Option<String>,
        source: RekeySource,
        now: Instant,
    ) {
        self.with(|l| {
            l.counts[source.index()].rejected += 1;
            let s = l.sessions.entry(session).or_default();
            s.record_decline(id, previous, source, DeclineKind::Rejected);
            if s.rejected.as_ref().is_none_or(|r| r.id != id) {
                s.rejected = Some(RejectedStreak {
                    id: id.to_string(),
                    attempts: 0,
                    cooldown_until: None,
                });
            }
            if let Some(streak) = s.rejected.as_mut() {
                streak.attempts += 1;
                if streak.attempts >= REJECTED_RETRY_LIMIT {
                    streak.attempts = 0;
                    streak.cooldown_until = Some(now + REJECTED_COOLDOWN);
                }
            }
        });
    }

    /// The id coord last accepted for `session`, if any re-key ever landed.
    pub fn last_landed(&self, session: Uuid) -> Option<String> {
        self.with(|l| l.sessions.get(&session).and_then(|s| s.last_landed.clone()))
    }

    fn take_decline(&self, session: Uuid) -> Option<Decline> {
        self.with(|l| l.sessions.get_mut(&session).and_then(|s| s.declined.take()))
    }

    fn declined_sessions(&self) -> Vec<Uuid> {
        self.with(|l| {
            l.sessions
                .iter()
                .filter(|(_, s)| s.declined.is_some())
                .map(|(id, _)| *id)
                .collect()
        })
    }

    fn cooling_down(&self, session: Uuid, id: &str, now: Instant) -> bool {
        self.with(|l| {
            l.sessions
                .get(&session)
                .and_then(|s| s.rejected.as_ref())
                .is_some_and(|r| r.id == id && r.cooldown_until.is_some_and(|until| until > now))
        })
    }

    fn defer(&self, session: Uuid, id: &str, source: RekeySource) {
        self.with(|l| {
            l.sessions.entry(session).or_default().deferred = Some((id.to_string(), source));
        });
    }

    fn clear_deferred(&self, session: Uuid) {
        self.with(|l| {
            if let Some(s) = l.sessions.get_mut(&session) {
                s.deferred = None;
            }
        });
    }

    /// Take every deferred re-key whose cooldown has ended.
    fn take_due_deferred(&self, now: Instant) -> Vec<(Uuid, String, RekeySource)> {
        self.with(|l| {
            let mut due = Vec::new();
            for (session, s) in &mut l.sessions {
                let Some((id, source)) = s.deferred.clone() else {
                    continue;
                };
                let cooling = s
                    .rejected
                    .as_ref()
                    .is_some_and(|r| r.id == id && r.cooldown_until.is_some_and(|u| u > now));
                if !cooling {
                    s.deferred = None;
                    due.push((*session, id, source));
                }
            }
            due
        })
    }

    /// End every running cooldown now, so a test can cross a cooldown's end
    /// without waiting [`REJECTED_COOLDOWN`].
    #[cfg(test)]
    pub(crate) fn end_cooldowns_for_test(&self) {
        self.with(|l| {
            for s in l.sessions.values_mut() {
                if let Some(r) = s.rejected.as_mut() {
                    r.cooldown_until = None;
                }
            }
        });
    }

    fn park(&self, terminal_id: &str, id: &str, source: RekeySource) {
        self.with(|l| {
            l.parked.insert(
                terminal_id.to_string(),
                ParkedRekey {
                    id: id.to_string(),
                    source,
                    ticks_left: PARKED_TICKS,
                },
            );
        });
    }

    fn unpark(&self, terminal_id: &str) {
        self.with(|l| {
            l.parked.remove(terminal_id);
        });
    }

    fn parked(&self) -> Vec<(String, ParkedRekey)> {
        self.with(|l| {
            l.parked
                .iter()
                .map(|(t, p)| (t.clone(), p.clone()))
                .collect()
        })
    }

    /// One more tick with no coord row for `terminal_id`.
    fn age_parked(&self, terminal_id: &str) {
        self.with(|l| {
            let expired = l.parked.get_mut(terminal_id).is_some_and(|p| {
                p.ticks_left = p.ticks_left.saturating_sub(1);
                p.ticks_left == 0
            });
            if expired {
                l.parked.remove(terminal_id);
            }
        });
    }

    /// One source's counters (tests and `/health`).
    pub fn counts(&self, source: RekeySource) -> SourceCounts {
        self.with(|l| l.counts[source.index()])
    }

    pub fn no_coord_row(&self) -> u64 {
        self.with(|l| l.no_coord_row)
    }

    pub fn invalid(&self) -> u64 {
        self.with(|l| l.invalid)
    }

    /// `/health` `sessionTracking.coordRekeys`:
    /// `{queued:{<source>:n}, landed:{…}, refused:{…}, rejected:{…}, no_coord_row, invalid}`.
    /// Every source key is always present — a zero is observed, not absent.
    pub fn health_json(&self) -> JsonValue {
        self.with(|l| {
            let per = |pick: fn(&SourceCounts) -> u64| {
                let mut m = serde_json::Map::new();
                for src in RekeySource::ALL {
                    m.insert(src.as_str().into(), json!(pick(&l.counts[src.index()])));
                }
                JsonValue::Object(m)
            };
            json!({
                "queued": per(|c| c.queued),
                "landed": per(|c| c.landed),
                "refused": per(|c| c.refused),
                "rejected": per(|c| c.rejected),
                "no_coord_row": l.no_coord_row,
                "invalid": l.invalid,
            })
        })
    }
}

impl SessionRekeyState {
    /// Record a decline, newest wins. When the pending decline's id is the one
    /// this re-key moved the record away from, coord never held that id either,
    /// so the earlier `previous` is what a restore has to go back to.
    fn record_decline(
        &mut self,
        id: &str,
        mut previous: Option<String>,
        source: RekeySource,
        kind: DeclineKind,
    ) {
        if let Some(pending) = &self.declined {
            if previous.as_deref() == Some(pending.refused_id.as_str()) {
                previous = pending.previous.clone();
            }
        }
        self.declined = Some(Decline {
            refused_id: id.to_string(),
            previous,
            source,
            kind,
        });
    }
}

/// A declined re-key whose local record was moved back.
struct Restored {
    session: Uuid,
    decline: Decline,
    restored_to: Option<String>,
}

/// Restore a declined re-key on `session`: move the local record back to what
/// coord holds (the last-landed id, else the row's id before the declined
/// re-key) — only if the record still holds the declined id; a newer re-key has
/// otherwise superseded it.
fn settle_session_decline(registry: &SessionRegistry, session: Uuid) -> Option<Restored> {
    let ledger = registry.coord_sync().rekeys();
    let decline = ledger.take_decline(session)?;
    let restored_to = ledger
        .last_landed(session)
        .or_else(|| decline.previous.clone());
    match registry.restore_claude_code_session_id(session, &decline.refused_id, restored_to.clone())
    {
        Ok(true) => {
            tracing::info!(
                coord_session_id = %session,
                refused = %decline.refused_id,
                restored_to = ?restored_to,
                source = decline.source.as_str(),
                kind = decline.kind.as_str(),
                "coord identity: coord declined the re-key — local record restored"
            );
            Some(Restored {
                session,
                decline,
                restored_to,
            })
        }
        Ok(false) | Err(_) => None,
    }
}

/// Restore every declined re-key the drain has recorded.
fn restore_declines(registry: &SessionRegistry) -> Vec<Restored> {
    registry
        .coord_sync()
        .rekeys()
        .declined_sessions()
        .into_iter()
        .filter_map(|session| settle_session_decline(registry, session))
        .collect()
}

/// Re-queue each restored re-key whose evidence will not be re-supplied, unless
/// this tick's newer evidence already moved the record: that supersedes the
/// retry, and a row gets at most one re-key per tick.
fn retry_restored(registry: &SessionRegistry, restored: Vec<Restored>) {
    for r in restored {
        if !r.decline.source.retried_after_decline() {
            continue;
        }
        let Ok(desc) = registry.describe_by_id(r.session) else {
            continue;
        };
        if desc.claude_code_session_id == r.restored_to {
            confirm_for_session(registry, r.session, &r.decline.refused_id, r.decline.source);
        }
    }
}

/// Retry evidence that arrived before its terminal had a coord row, now that it
/// has one.
fn retry_parked(registry: &SessionRegistry, coord_row_for: &impl Fn(&str) -> Option<Uuid>) {
    let ledger = registry.coord_sync().rekeys();
    for (terminal_id, parked) in ledger.parked() {
        match coord_row_for(&terminal_id) {
            Some(row) => {
                confirm_for_terminal(registry, &terminal_id, Some(row), &parked.id, parked.source);
            }
            None => ledger.age_parked(&terminal_id),
        }
    }
}

/// Retry evidence that waited out a cooldown.
fn retry_deferred(registry: &SessionRegistry) {
    let due = registry
        .coord_sync()
        .rekeys()
        .take_due_deferred(Instant::now());
    for (session, id, source) in due {
        confirm_for_session(registry, session, &id, source);
    }
}

/// THE seam, by coord session id. Validates `claude_id`, settles any decline
/// the drain recorded for this session, then asks the registry to re-key —
/// which queues its own `state_change` only when the record moves.
pub fn confirm_for_session(
    registry: &SessionRegistry,
    coord_session_id: Uuid,
    claude_id: &str,
    source: RekeySource,
) -> RekeyOutcome {
    confirm_inner(registry, None, coord_session_id, claude_id, source)
}

fn confirm_inner(
    registry: &SessionRegistry,
    terminal_id: Option<&str>,
    coord_session_id: Uuid,
    claude_id: &str,
    source: RekeySource,
) -> RekeyOutcome {
    let ledger = registry.coord_sync().rekeys();
    let claude_id = claude_id.trim();
    if Uuid::parse_str(claude_id).is_err() {
        ledger.note_invalid();
        tracing::warn!(
            terminal_id = ?terminal_id,
            coord_session_id = %coord_session_id,
            session_id = %claude_id,
            source = source.as_str(),
            "coord identity: harness id is not a uuid — not re-keyed"
        );
        return RekeyOutcome::Invalid;
    }
    settle_session_decline(registry, coord_session_id);

    // A closed session has nothing running under its row, and a refused re-key
    // would otherwise keep PATCHing it on the tick.
    if registry
        .describe_by_id(coord_session_id)
        .is_ok_and(|d| matches!(d.state, SessionState::Closed))
    {
        tracing::debug!(
            terminal_id = ?terminal_id,
            coord_session_id = %coord_session_id,
            source = source.as_str(),
            "coord identity: session is closed — not re-keyed"
        );
        return RekeyOutcome::SessionClosed;
    }
    if ledger.cooling_down(coord_session_id, claude_id, Instant::now()) {
        ledger.defer(coord_session_id, claude_id, source);
        tracing::debug!(
            terminal_id = ?terminal_id,
            coord_session_id = %coord_session_id,
            session_id = %claude_id,
            source = source.as_str(),
            "coord identity: coord keeps rejecting this id — waiting out the cooldown"
        );
        return RekeyOutcome::CoolingDown;
    }

    match registry.confirm_claude_code_session_id(coord_session_id, claude_id, source) {
        Ok(true) => {
            ledger.note_queued(source);
            ledger.clear_deferred(coord_session_id);
            tracing::info!(
                terminal_id = ?terminal_id,
                session_id = %claude_id,
                coord_session_id = %coord_session_id,
                source = source.as_str(),
                "coord row re-keyed to the CONFIRMED harness session id (queued)"
            );
            RekeyOutcome::Queued
        }
        Ok(false) => {
            // Rung 2 answers this every tick for an unconfirmed pane, so only
            // the once-per-session hook is worth an info line.
            if source == RekeySource::Hook {
                tracing::info!(
                    terminal_id = ?terminal_id,
                    coord_session_id = %coord_session_id,
                    source = source.as_str(),
                    last_landed = ?ledger.last_landed(coord_session_id),
                    "coord identity: row already carries the confirmed harness session id"
                );
            } else {
                tracing::debug!(
                    terminal_id = ?terminal_id,
                    coord_session_id = %coord_session_id,
                    source = source.as_str(),
                    last_landed = ?ledger.last_landed(coord_session_id),
                    "coord identity: row already carries the confirmed harness session id"
                );
            }
            RekeyOutcome::AlreadyCurrent
        }
        Err(SessionError::NotFound(_)) => {
            ledger.note_no_coord_row();
            tracing::info!(
                terminal_id = ?terminal_id,
                coord_session_id = %coord_session_id,
                source = source.as_str(),
                "coord identity: the registry holds no record for this coord row — not re-keyed"
            );
            RekeyOutcome::NoCoordRow
        }
        Err(SessionError::InvalidClaudeCodeSessionId(_)) => {
            ledger.note_invalid();
            RekeyOutcome::Invalid
        }
        Err(e) => {
            tracing::warn!(
                terminal_id = ?terminal_id,
                coord_session_id = %coord_session_id,
                session_id = %claude_id,
                source = source.as_str(),
                "coord identity: re-key not queued: {e}"
            );
            RekeyOutcome::OutboxFailed
        }
    }
}

/// THE seam, by terminal: `coord_row` is that terminal's
/// `TerminalSession::coord_session_id()` (resolved by the caller, so this stays
/// testable without a `TerminalManager`). Evidence for a terminal with no row
/// yet is parked and retried by [`apply_reconcile_tick`] once it has one; newer
/// evidence for the terminal replaces it.
pub fn confirm_for_terminal(
    registry: &SessionRegistry,
    terminal_id: &str,
    coord_row: Option<Uuid>,
    claude_id: &str,
    source: RekeySource,
) -> RekeyOutcome {
    if Uuid::parse_str(claude_id.trim()).is_err() {
        // Same order as the row-bound arm: a malformed id is Invalid whether
        // or not the terminal has a row yet.
        return confirm_for_session(registry, Uuid::nil(), claude_id, source);
    }
    let ledger = registry.coord_sync().rekeys();
    let Some(coord_session_id) = coord_row else {
        // Normal and transient: the coord mirror is registered after the pane
        // exists. Not a warn.
        ledger.note_no_coord_row();
        ledger.park(terminal_id, claude_id.trim(), source);
        tracing::info!(
            terminal_id = %terminal_id,
            session_id = %claude_id,
            source = source.as_str(),
            "coord identity: no coord session bound to this terminal yet — parked for a later tick"
        );
        return RekeyOutcome::NoCoordRow;
    };
    ledger.unpark(terminal_id);
    confirm_inner(
        registry,
        Some(terminal_id),
        coord_session_id,
        claude_id,
        source,
    )
}

/// Feed one reconcile pass's rung-2 binds through the seam (D5): only a
/// `Bind` graded [`BindOrigin::Authoritative`] (the anchor's own argv) — never
/// a rung-3 `Observed` or rung-4 `Reconciled` correlation. At most one seam
/// call per terminal, and none for a terminal whose anchor has a valid
/// provider row this tick (`has_valid_provider_row`; the provider-registry
/// source is the stronger, current evidence). Until Phase 1b lands that
/// predicate is always `false`.
pub fn rekey_rung2_binds(
    registry: &SessionRegistry,
    actions: &[ReconcileAction],
    coord_row_for: impl Fn(&str) -> Option<Uuid>,
    has_valid_provider_row: impl Fn(&str) -> bool,
) -> Vec<(String, RekeyOutcome)> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for action in actions {
        let ReconcileAction::Bind {
            terminal_id,
            session_id,
            origin: BindOrigin::Authoritative,
            ..
        } = action
        else {
            continue;
        };
        if !seen.insert(terminal_id.as_str()) || has_valid_provider_row(terminal_id) {
            continue;
        }
        let outcome = confirm_for_terminal(
            registry,
            terminal_id,
            coord_row_for(terminal_id),
            session_id,
            RekeySource::Rung2,
        );
        out.push((terminal_id.clone(), outcome));
    }
    out
}

/// `/health` `sessionTracking`, with `coordRekeys` — coord's answers to the
/// harness-id re-keys the session registry queued. `coordRekeys: null` = no
/// registry in state (UNKNOWN, not zero).
pub fn session_tracking_json(registry: Option<&SessionRegistry>) -> JsonValue {
    let mut tracking = crate::session::tracking_health::health_json();
    let rekeys = registry
        .map(|r| r.coord_sync().rekeys().health_json())
        .unwrap_or(JsonValue::Null);
    if let Some(obj) = tracking.as_object_mut() {
        obj.insert("coordRekeys".into(), rekeys);
    }
    tracking
}

// ---------------------------------------------------------------------------
// Tauri-state adapters (production callers)
// ---------------------------------------------------------------------------

fn app_parts(
    app: &tauri::AppHandle,
) -> Option<(Arc<crate::terminal::TerminalManager>, Arc<SessionRegistry>)> {
    use tauri::Manager;
    Some((
        app.try_state::<Arc<crate::terminal::TerminalManager>>()?
            .inner()
            .clone(),
        app.try_state::<Arc<SessionRegistry>>()?.inner().clone(),
    ))
}

/// [`confirm_for_terminal`] against the app's own terminal manager + registry.
/// `None` when either is not in Tauri state yet (a boot window before
/// `app.manage`).
pub fn confirm_for_terminal_in_app(
    app: &tauri::AppHandle,
    terminal_id: &str,
    claude_id: &str,
    source: RekeySource,
) -> Option<RekeyOutcome> {
    let Some((terminals, registry)) = app_parts(app) else {
        tracing::warn!(
            terminal_id = %terminal_id,
            session_id = %claude_id,
            source = source.as_str(),
            "coord identity: terminal manager or session registry not in Tauri state — \
             re-key skipped (best-effort)"
        );
        return None;
    };
    let coord_row = terminals
        .get(terminal_id)
        .and_then(|s| s.coord_session_id());
    Some(confirm_for_terminal(
        &registry,
        terminal_id,
        coord_row,
        claude_id,
        source,
    ))
}

/// One reconcile tick's coord-identity work, in order: restore the declines the
/// drain recorded since the last tick; feed the pass's rung-2 binds through the
/// seam; then retry what no pass will re-supply, skipping a row this tick
/// already moved. Pure over its inputs (the terminal → coord-row lookup is
/// injected), so the whole tick composition is testable against a fake coord.
pub fn apply_reconcile_tick(
    registry: &SessionRegistry,
    actions: &[ReconcileAction],
    coord_row_for: impl Fn(&str) -> Option<Uuid>,
) -> Vec<(String, RekeyOutcome)> {
    let restored = restore_declines(registry);
    let out = rekey_rung2_binds(
        registry,
        actions,
        &coord_row_for,
        // Phase 1b (the provider-registry source) is not on this build: no
        // terminal has a valid provider row yet, so rung 2 is never pre-empted.
        |_| false,
    );
    retry_parked(registry, &coord_row_for);
    retry_restored(registry, restored);
    retry_deferred(registry);
    out
}

/// [`apply_reconcile_tick`] against the app's terminal manager + registry.
/// Fire-and-forget; a no-op when either is not in Tauri state.
pub fn apply_reconcile_tick_in_app(app: &tauri::AppHandle, actions: &[ReconcileAction]) {
    let Some((terminals, registry)) = app_parts(app) else {
        return;
    };
    apply_reconcile_tick(&registry, actions, |t| {
        terminals.get(t).and_then(|s| s.coord_session_id())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::coord_sync::CoordSync;
    use crate::session::local_store::{OutboxRecord, OutboxWriter};
    use crate::session::{
        DynTransport, Intent, SessionEventKind, SessionKind, SessionTransports, Transport,
        TransportError, TransportHandle,
    };
    use std::time::Duration;

    struct Noop;
    impl Transport for Noop {
        fn start(&self, _i: &Intent) -> Result<TransportHandle, TransportError> {
            Ok(TransportHandle::External)
        }
        fn write_input(&self, _h: &TransportHandle, _b: &[u8]) -> Result<(), TransportError> {
            Ok(())
        }
        fn resize(&self, _h: &TransportHandle, _c: u16, _r: u16) -> Result<(), TransportError> {
            Ok(())
        }
        fn close(&self, _h: &TransportHandle) -> Result<(), TransportError> {
            Ok(())
        }
        fn tap_output(
            &self,
            _h: &TransportHandle,
        ) -> Option<tokio::sync::broadcast::Receiver<String>> {
            None
        }
    }

    fn intent() -> Intent {
        Intent {
            kind: SessionKind::TerminalShell,
            purpose: "coord identity test".into(),
            repo: Some("qontinui-runner".into()),
            branch: Some("main".into()),
            work_unit_slug: None,
            plan_slug: None,
            correlation_topic: None,
            page_id: None,
            declared_paths: vec![],
            share_output: false,
            redact_secrets: None,
            tenant_id: None,
        }
    }

    /// A registry whose coord is unreachable — the seam's outbox side is all
    /// these tests observe.
    fn registry(dir: &std::path::Path) -> (Arc<SessionRegistry>, Arc<OutboxWriter>) {
        let outbox = Arc::new(OutboxWriter::open(dir.join("outbox.jsonl")).unwrap());
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            "http://127.0.0.1:1".into(),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let transports = SessionTransports {
            pty: Arc::new(Noop) as DynTransport,
            claude_cli: Arc::new(Noop) as DynTransport,
            workflow: Arc::new(Noop) as DynTransport,
        };
        let reg = SessionRegistry::new(Uuid::new_v4(), transports, coord.clone());
        coord.attach_registry(&reg);
        (reg, outbox)
    }

    fn rekeys_for(outbox: &OutboxWriter, session: Uuid) -> Vec<OutboxRecord> {
        outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| {
                r.session_id == session
                    && r.event_kind == SessionEventKind::StateChange.as_str()
                    && r.payload.get("claude_code_session_id").is_some()
            })
            .collect()
    }

    fn bind(terminal: &str, id: &str, origin: BindOrigin) -> ReconcileAction {
        ReconcileAction::Bind {
            terminal_id: terminal.into(),
            session_id: id.into(),
            config_dir: String::new(),
            origin,
            confirmed: false,
        }
    }

    const PIN: &str = "2711c738-0000-4000-8000-000000000001";
    const LIVE: &str = "44348633-0000-4000-8000-000000000002";

    /// A rung-2 bind for a terminal with coord row R queues exactly one
    /// `state_change` for R carrying the id; the next tick queues none.
    #[test]
    fn a_rung2_bind_queues_one_rekey_and_the_next_tick_none() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        let actions = vec![
            bind("t1", LIVE, BindOrigin::Authoritative),
            // A duplicate bind for the same terminal in one pass is ignored.
            bind("t1", LIVE, BindOrigin::Authoritative),
        ];
        let lookup = |t: &str| (t == "t1").then_some(row);

        let first = rekey_rung2_binds(&reg, &actions, lookup, |_| false);
        assert_eq!(first, vec![("t1".to_string(), RekeyOutcome::Queued)]);
        let rows = rekeys_for(&outbox, row);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].payload["claude_code_session_id"], json!(LIVE));
        assert_eq!(rows[0].payload["rekey_source"], json!("rung2"));
        assert_eq!(
            reg.coord_sync().rekeys().counts(RekeySource::Rung2).queued,
            1
        );

        let second = rekey_rung2_binds(&reg, &actions, lookup, |_| false);
        assert_eq!(
            second,
            vec![("t1".to_string(), RekeyOutcome::AlreadyCurrent)]
        );
        assert_eq!(
            rekeys_for(&outbox, row).len(),
            1,
            "the next tick queues none"
        );
    }

    /// A terminal with a valid provider row is not re-keyed from rung 2
    /// (the precedence hook Phase 1b fills in).
    #[test]
    fn a_valid_provider_row_pre_empts_rung2() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        let out = rekey_rung2_binds(
            &reg,
            &[bind("t1", LIVE, BindOrigin::Authoritative)],
            |_| Some(row),
            |_| true,
        );
        assert!(out.is_empty());
        assert!(rekeys_for(&outbox, row).is_empty());
    }

    /// Rung-3 (`Observed`) and rung-4 (`Reconciled`) binds never re-key.
    #[test]
    fn a_rung3_or_rung4_bind_queues_nothing() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        let out = rekey_rung2_binds(
            &reg,
            &[
                bind("t1", LIVE, BindOrigin::Observed),
                bind("t2", LIVE, BindOrigin::Reconciled),
            ],
            |_| Some(row),
            |_| false,
        );
        assert!(out.is_empty(), "{out:?}");
        assert!(rekeys_for(&outbox, row).is_empty());
        assert_eq!(
            reg.coord_sync().rekeys().counts(RekeySource::Rung2),
            SourceCounts::default()
        );
    }

    /// An id the row already carries queues nothing.
    #[test]
    fn an_already_current_id_queues_nothing() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        assert_eq!(
            confirm_for_terminal(&reg, "t1", Some(row), PIN, RekeySource::Hook),
            RekeyOutcome::AlreadyCurrent
        );
        assert!(rekeys_for(&outbox, row).is_empty());
        assert_eq!(
            reg.coord_sync().rekeys().counts(RekeySource::Hook).queued,
            0
        );
    }

    /// No coord row on the terminal → `NoCoordRow`, counted; a row id the
    /// registry does not hold is the same answer.
    #[test]
    fn a_terminal_with_no_coord_row_is_no_coord_row() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        assert_eq!(
            confirm_for_terminal(&reg, "t1", None, LIVE, RekeySource::Rung2),
            RekeyOutcome::NoCoordRow
        );
        assert_eq!(
            confirm_for_terminal(&reg, "t1", Some(Uuid::new_v4()), LIVE, RekeySource::Rung2),
            RekeyOutcome::NoCoordRow
        );
        assert_eq!(reg.coord_sync().rekeys().no_coord_row(), 2);
        assert!(outbox.pending().unwrap().is_empty());
    }

    /// A non-UUID id → `Invalid`, counted, nothing queued, record unmoved.
    #[test]
    fn a_non_uuid_id_is_invalid() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        assert_eq!(
            confirm_for_terminal(&reg, "t1", Some(row), "not-a-uuid", RekeySource::Rung2),
            RekeyOutcome::Invalid
        );
        assert_eq!(
            confirm_for_terminal(&reg, "t1", None, "not-a-uuid", RekeySource::Hook),
            RekeyOutcome::Invalid
        );
        assert_eq!(reg.coord_sync().rekeys().invalid(), 2);
        assert!(rekeys_for(&outbox, row).is_empty());
        assert_eq!(
            reg.describe_by_id(row)
                .unwrap()
                .claude_code_session_id
                .as_deref(),
            Some(PIN)
        );
    }

    /// `/health` shape: every source present under each counter.
    #[test]
    fn health_json_serves_every_source_under_every_counter() {
        let ledger = RekeyLedger::default();
        ledger.note_queued(RekeySource::R2Resume);
        ledger.note_landed(Uuid::new_v4(), LIVE, RekeySource::Rung2);
        let j = ledger.health_json();
        for k in ["queued", "landed", "refused", "rejected"] {
            for src in RekeySource::ALL {
                assert!(j[k][src.as_str()].is_u64(), "{k}.{}: {j}", src.as_str());
            }
        }
        assert_eq!(j["queued"]["r2_resume"], json!(1));
        assert_eq!(j["landed"]["rung2"], json!(1));
        assert_eq!(j["no_coord_row"], json!(0));
        assert_eq!(j["invalid"], json!(0));
    }

    #[test]
    fn rekey_source_from_payload() {
        assert_eq!(
            RekeySource::from_payload(&json!({})),
            Some(RekeySource::Hook)
        );
        assert_eq!(
            RekeySource::from_payload(&json!({"rekey_source": "r2_resume"})),
            Some(RekeySource::R2Resume)
        );
        assert_eq!(
            RekeySource::from_payload(&json!({"rekey_source": "x"})),
            None
        );
    }

    /// `/health` `sessionTracking.coordRekeys`: null with no registry (UNKNOWN,
    /// not zero), and the ledger's counters with one.
    #[test]
    fn session_tracking_json_carries_coord_rekeys() {
        let _amb = crate::test_env::isolated_ambient();
        assert!(session_tracking_json(None)["coordRekeys"].is_null());
        let dir = tempfile::tempdir().unwrap();
        let (reg, _outbox) = registry(dir.path());
        reg.coord_sync().rekeys().note_queued(RekeySource::Hook);
        assert_eq!(
            session_tracking_json(Some(&reg))["coordRekeys"]["queued"]["hook"],
            json!(1)
        );
    }

    /// A failed outbox write leaves the record where it was, so the next
    /// confirmation of the id is not read as already current.
    #[test]
    fn an_outbox_failure_leaves_the_record_unmoved() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let (reg, _outbox) = registry(&sub);
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        std::fs::remove_dir_all(&sub).unwrap();
        assert_eq!(
            confirm_for_session(&reg, row, LIVE, RekeySource::Hook),
            RekeyOutcome::OutboxFailed
        );
        assert_eq!(
            reg.describe_by_id(row)
                .unwrap()
                .claude_code_session_id
                .as_deref(),
            Some(PIN)
        );
        assert_eq!(
            reg.coord_sync().rekeys().counts(RekeySource::Hook).queued,
            0
        );
    }

    /// A then B declined in one drain window: coord never held A, so a restore
    /// goes back to what the row held before A.
    #[test]
    fn a_chained_decline_restores_past_the_id_coord_also_declined() {
        const B: &str = "55555555-0000-4000-8000-000000000005";
        let ledger = RekeyLedger::default();
        let s = Uuid::new_v4();
        ledger.note_refused(s, LIVE, Some(PIN.into()), RekeySource::Hook);
        ledger.note_refused(s, B, Some(LIVE.into()), RekeySource::Rung2);
        let d = ledger.take_decline(s).unwrap();
        assert_eq!(d.refused_id, B);
        assert_eq!(d.previous.as_deref(), Some(PIN));
    }

    /// `rejected` answers run out into a cooldown; the cooldown ends, and a
    /// different id or a landed push is not affected by it.
    #[test]
    fn a_rejected_streak_cools_down_and_the_cooldown_ends() {
        const B: &str = "55555555-0000-4000-8000-000000000005";
        let ledger = RekeyLedger::default();
        let s = Uuid::new_v4();
        let t0 = Instant::now();
        for _ in 0..REJECTED_RETRY_LIMIT - 1 {
            ledger.note_rejected_at(s, LIVE, None, RekeySource::Hook, t0);
        }
        assert!(!ledger.cooling_down(s, LIVE, t0));
        ledger.note_rejected_at(s, LIVE, None, RekeySource::Hook, t0);
        assert!(ledger.cooling_down(s, LIVE, t0));
        assert!(!ledger.cooling_down(s, B, t0), "another id is not held");
        assert!(!ledger.cooling_down(s, LIVE, t0 + REJECTED_COOLDOWN));
        assert_eq!(
            ledger.counts(RekeySource::Hook).rejected,
            u64::from(REJECTED_RETRY_LIMIT)
        );

        ledger.defer(s, LIVE, RekeySource::Hook);
        assert!(ledger.take_due_deferred(t0).is_empty(), "still cooling");
        assert_eq!(
            ledger.take_due_deferred(t0 + REJECTED_COOLDOWN),
            vec![(s, LIVE.to_string(), RekeySource::Hook)]
        );
        assert!(
            ledger.take_due_deferred(t0 + REJECTED_COOLDOWN).is_empty(),
            "taken once"
        );

        ledger.note_landed(s, LIVE, RekeySource::Hook);
        assert!(!ledger.cooling_down(s, LIVE, t0));
    }

    /// Evidence for a terminal with no row is parked; the first tick that sees
    /// the row queues it, once. A terminal that never gets one ages out.
    #[test]
    fn parked_evidence_lands_when_the_row_appears_and_otherwise_ages_out() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        assert_eq!(
            confirm_for_terminal(&reg, "t1", None, LIVE, RekeySource::Hook),
            RekeyOutcome::NoCoordRow
        );
        apply_reconcile_tick(&reg, &[], |_| None);
        assert!(rekeys_for(&outbox, row).is_empty(), "still no row");

        apply_reconcile_tick(&reg, &[], |t| (t == "t1").then_some(row));
        let rows = rekeys_for(&outbox, row);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].payload["rekey_source"], json!("hook"));
        apply_reconcile_tick(&reg, &[], |t| (t == "t1").then_some(row));
        assert_eq!(
            rekeys_for(&outbox, row).len(),
            1,
            "parked evidence is spent"
        );

        confirm_for_terminal(&reg, "gone", None, LIVE, RekeySource::Rung2);
        for _ in 0..PARKED_TICKS {
            apply_reconcile_tick(&reg, &[], |_| None);
        }
        assert!(reg.coord_sync().rekeys().parked().is_empty());
    }

    /// A closed session is never re-keyed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_closed_session_is_not_rekeyed() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let (reg, outbox) = registry(dir.path());
        let row = reg
            .register_external_with_lineage(intent(), None, Some(PIN.into()))
            .unwrap();
        reg.close_by_id(row).unwrap();
        assert_eq!(
            confirm_for_session(&reg, row, LIVE, RekeySource::Hook),
            RekeyOutcome::SessionClosed
        );
        assert!(rekeys_for(&outbox, row).is_empty());
    }
}
