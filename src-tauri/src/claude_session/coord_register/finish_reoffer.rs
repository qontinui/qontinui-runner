//! A move re-offers a finish another row ACKed (plan
//! `2026-11-12-transcript-autobind-adopts-a-previous-boot-row-that-a-live-pane-has-replaced`
//! D5, Phase 3), on #2704's record.
//!
//! A harness id X whose registrar entry sat on a stale row R′ sent its
//! `Finished` to R′. #2704's observed-ACK stamping records which row confirmed
//! the mark
//! ([`crate::session::session_lifecycle_store::TerminalSessionRecord::finish_coord_session_id`]),
//! so when a mover later puts X on its live pane row R, the mark reads
//! `finish_synced` while R itself never read `finished`.
//! [`AiCoordRegistrar::reoffer_finish_to`] closes that gap: after a move onto
//! R it re-offers X's finish to R when
//!
//! 1. X's lifecycle record is finished;
//! 2. its `finish_coord_session_id` names a row OTHER than R — `None` means the
//!    finish is still in flight, and its ACK sets the field for the next tick;
//! 3. **the revival guard**: X's ownership of R began strictly BEFORE the
//!    mark's `finished_at`. A finished session resumed into a new pane, or
//!    resumed in-pane, must not be re-finished on the row it now runs on. The
//!    ownership instant is the owner rule's
//!    ([`crate::claude_session::pane_row_owner::live_terminal_row`]'s
//!    `owned_since_ms`): the pane's
//!    bind instant for the pane PINNED to X, while no other id was ever seen
//!    running on its row; else the instant a running-id source (the hook, the
//!    provider registry) first confirmed X there. An UNKNOWN instant — no
//!    live-pane answer for X on R, or one with no instant — is never read as
//!    "before": nothing is re-offered (fail safe).
//!
//! **What a refusal costs.** Nothing else re-sends a mark its own row
//! confirmed to a DIFFERENT row: #2704's boot heal re-offers a mark only to its
//! own recorded row, and only when that row does not read `finished`. So a
//! re-offer the guard refuses stays refused — R′ reads `finished`, R does not —
//! until the session is finished again (a new mark is forwarded to the row X
//! is on). That is chosen over the alternative: a finish pushed onto a revived
//! session's row would close live work.
//!
//! **A pane bound AFTER the finish never qualifies.** `TerminalManager::
//! bind_coord_session` stamps the pane's bind instant at the bind itself, so
//! for a pane created after X finished — a respawn, an account migration, the
//! resume door, or a holder pane re-bound by its create door after a handoff
//! restart — X's ownership of its row begins after the finish, and the guard
//! refuses. That is why the bind trigger (`on_terminal_coord_bind`) is not a
//! caller: at the bind no truthful pre-finish instant exists (a finished
//! session is not restored on boot, and a pane-store resume re-keys the row to
//! whatever pin the new pane carries, so the row's earlier history is not X's).
//! A seam tick on that pane later re-reads the same instant and refuses too.
//!
//! **The door** is one critical section under the lifecycle store's
//! `finish_forward` ([`crate::session::session_lifecycle_store::SessionLifecycleStore::redirect_finish`]):
//! re-read the mark (still finished at `finished_at`, still ACKed by another
//! row), withdraw its `finish_synced` stamp, enqueue the `Finished` to R —
//! unless one for this very mark is already pending there — and record R as
//! the row it was sent to. An unmark therefore lands either before (nothing is
//! queued) or after (its `working` follows the `Finished`, in marker order),
//! and two concurrent re-offers of one mark queue exactly one `Finished`: the
//! second re-reads the mark naming R. The mark reads synced again only on R's
//! observed ACK, which also stamps `finish_coord_session_id = R`.
//!
//! **Who calls it.** The live-pane adoption
//! (`bind_transcript_session_on_live_pane`) and the seam (`note_pane_row`) on
//! `Moved`, and the seam on every tick whose entry already names R
//! ([`super::MoveOutcome::Unchanged`]: a stale row's ACK that drains AFTER the
//! move is re-offered there, once). The FINISH-TIME REPAIR runs inside
//! `set_finished`'s finish observer, with `finish_forward` HELD, so it cannot
//! take the door; it needs none: the forward it repairs is itself the finish,
//! now addressed to R. It only records R as the row the mark was sent to
//! ([`AiCoordRegistrar::note_repaired_finish_row`]).
//!
//! Lock order: [`AiCoordRegistrar::reoffer_finish_to`] takes `finish_forward`
//! and reads the terminal plane, so it must never run from the finish
//! observer, nor while holding the store map, a registrar map, the
//! `TerminalManager` `sessions` lock or the outbox — the same constraints as
//! `deliver_owed_finish`, which every one of its callers already obeys.

use std::sync::atomic::{AtomicU64, Ordering};

use tracing::{debug, info, warn};
use uuid::Uuid;

use super::AiCoordRegistrar;
use crate::claude_session::pane_row_owner::LivePaneRow;
use crate::session::session_lifecycle_store::{FinishSync, RedirectFinish};

/// How many finishes a move re-offered to a pane row after another row had
/// ACKed them (plan D5). Served on `/health` as `registrarFinishReoffers`.
static FINISH_REOFFERS: AtomicU64 = AtomicU64::new(0);

/// See [`FINISH_REOFFERS`].
pub(crate) fn registrar_finish_reoffers() -> u64 {
    FINISH_REOFFERS.load(Ordering::Relaxed)
}

/// What [`AiCoordRegistrar::reoffer_finish_to`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReofferOutcome {
    /// No lifecycle store attached, no record for the id, or the record is
    /// not finished.
    NotFinished,
    /// The mark has no `finish_coord_session_id`: its finish is still in
    /// flight, and its ACK sets the field for the next tick.
    InFlight,
    /// The mark's row already IS the pane row (read before the door, or
    /// re-read inside it after a concurrent re-offer won).
    AlreadyOnRow,
    /// X's ownership of the row has no known instant (fail safe).
    OwnershipUnknown,
    /// X's ownership of the row began at or after the mark's `finished_at`:
    /// a finished session revived onto this row (the revival guard).
    OwnedAfterFinish,
    /// The mark changed between the decision and the door (unmarked or
    /// restamped): nothing was queued.
    MarkChanged,
    /// A `Finished` for this mark was already pending to the row: the stamp
    /// was withdrawn and the row recorded, nothing more queued.
    AlreadyQueued,
    /// The finish was re-queued to `row`; `from` is the row that had ACKed it.
    Reoffered { from: Uuid },
    /// The outbox write failed: the stamp is withdrawn, the row is NOT
    /// recorded, so the next tick tries again.
    NotQueued,
}

#[cfg(test)]
thread_local! {
    /// A test's racing write, run once when this thread's next re-offer has
    /// read its decision and not yet entered the door.
    static AFTER_DECISION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` once, when this thread's next re-offer has made its decision.
#[cfg(test)]
pub(super) fn race_after_decision(f: impl FnOnce() + 'static) {
    AFTER_DECISION.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

impl AiCoordRegistrar {
    /// Re-offer `claude_session_id`'s finish to its pane row `row` after a
    /// move (plan D5; see the module docs for the conditions and the door).
    /// Resolves X's ownership instant of `row` through the owner rule; a
    /// lookup that does not answer `row` for X is UNKNOWN and re-offers
    /// nothing.
    pub(crate) fn reoffer_finish_to(&self, claude_session_id: &str, row: Uuid) -> ReofferOutcome {
        // The record first: the common case (not finished) costs no
        // terminal-plane read.
        if !self.local_mark_finished(claude_session_id) {
            return ReofferOutcome::NotFinished;
        }
        let live = self
            .live_terminal_row(claude_session_id)
            .filter(|l| l.row == row);
        self.reoffer_finish_on(claude_session_id, row, live)
    }

    /// [`Self::reoffer_finish_to`] with X's live-pane answer already read
    /// (`live`: the owner rule's answer when it names `row`, else `None`).
    pub(super) fn reoffer_finish_on(
        &self,
        claude_session_id: &str,
        row: Uuid,
        live: Option<LivePaneRow>,
    ) -> ReofferOutcome {
        let Some(store) = self.inner.lifecycle_store.get() else {
            return ReofferOutcome::NotFinished;
        };
        let Some(rec) = store.get(claude_session_id) else {
            return ReofferOutcome::NotFinished;
        };
        let Some(finished_at) = rec.finished_at else {
            return ReofferOutcome::NotFinished;
        };
        let from = match rec.finish_coord_session_id {
            None => return ReofferOutcome::InFlight,
            Some(r) if r == row => return ReofferOutcome::AlreadyOnRow,
            Some(r) => r,
        };
        // The revival guard. An unknown ownership instant is never read as
        // "before the finish": a finish pushed onto a session revived on this
        // row would close live work. (A refusal is not retried elsewhere — see
        // the module docs.)
        let Some(owned_since) = live.filter(|l| l.row == row).and_then(|l| l.owned_since_ms) else {
            debug!(
                "ai_coord_register: not re-offering {}'s finish (ACKed by {}) to pane row {} — \
                 when it began owning that row is unknown",
                claude_session_id, from, row
            );
            return ReofferOutcome::OwnershipUnknown;
        };
        if owned_since >= finished_at {
            debug!(
                "ai_coord_register: not re-offering {}'s finish (at {}) to pane row {} — it \
                 owns that row only since {}: a finished session revived there, or a pane \
                 bound after the finish",
                claude_session_id, finished_at, row, owned_since
            );
            return ReofferOutcome::OwnedAfterFinish;
        }

        #[cfg(test)]
        if let Some(f) = AFTER_DECISION.with(|h| h.borrow_mut().take()) {
            f();
        }

        // The door: one `finish_forward` critical section (re-read, clear,
        // send, record). `enqueue_finished` takes the outbox and the tenants
        // map — never `finish_forward` — so it is safe inside.
        let outcome = store.redirect_finish(claude_session_id, finished_at, row, |rec| {
            // A `Finished` for this very mark already pending to `row` (the
            // late delivery a registration ran just before) will be ACKed by
            // `row` and stamp it: queueing another would only duplicate the
            // PATCH. An unreadable outbox is UNKNOWN, so the send goes ahead.
            if self.finished_row_pending(claude_session_id, finished_at, row) == Some(true) {
                return (ReofferOutcome::AlreadyQueued, true);
            }
            match self.enqueue_finished(
                claude_session_id,
                row,
                Some(finished_at),
                rec.finish_reason.as_deref(),
            ) {
                FinishSync::Queued { .. } => (ReofferOutcome::Reoffered { from }, true),
                FinishSync::LocalOnly(reason) => {
                    warn!(
                        "ai_coord_register: re-offering {}'s finish to pane row {} queued \
                         nothing ({:?}) — the mark stays unsynced for the next tick",
                        claude_session_id, row, reason
                    );
                    (ReofferOutcome::NotQueued, false)
                }
            }
        });
        let outcome = match outcome {
            RedirectFinish::MarkChanged => ReofferOutcome::MarkChanged,
            RedirectFinish::AlreadyOnRow => ReofferOutcome::AlreadyOnRow,
            RedirectFinish::Ran(o) => o,
        };
        if let ReofferOutcome::Reoffered { from: acked_by } = outcome {
            FINISH_REOFFERS.fetch_add(1, Ordering::Relaxed);
            info!(
                "ai_coord_register: re-offered {}'s finish to pane row {} — coord row {} ACKed \
                 it, but {} is the row the session runs on (it reads synced again only on {}'s \
                 own ACK)",
                claude_session_id, row, acked_by, row, row
            );
        }
        outcome
    }

    /// The finish-time repair's half of D5: the repair moved X onto `row`
    /// inside the finish observer, and the forward that follows queued the
    /// mark itself to `row`. Record `row` as the row the mark was sent to, so
    /// the next seam tick finds the field naming it and re-offers nothing.
    /// Map lock only — safe inside the observer, where `finish_forward` is
    /// held. Only a finished, unsynced mark at `finished_at` is touched.
    pub(super) fn note_repaired_finish_row(
        &self,
        claude_session_id: &str,
        finished_at: i64,
        row: Uuid,
    ) {
        if let Some(store) = self.inner.lifecycle_store.get() {
            store.note_finish_sent_row(claude_session_id, Some(finished_at), row);
        }
    }
}
