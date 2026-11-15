//! A move re-offers a finish another row ACKed (plan
//! `2026-11-12-transcript-autobind-adopts-a-previous-boot-row-that-a-live-pane-has-replaced`
//! D5, Phase 3), on #2704's record.
//!
//! A harness id X whose registrar entry sat on a stale row R′ sent its
//! `Finished` to R′. #2704's observed-ACK stamping records which row confirmed
//! the mark
//! ([`crate::session::session_lifecycle_store::TerminalSessionRecord::finish_coord_session_id`]), so when a
//! mover later puts X on its live pane row R, the mark reads `finish_synced`
//! while R itself never read `finished`. [`AiCoordRegistrar::reoffer_finish_to`]
//! closes that gap: after a move onto R it re-offers X's finish to R when
//!
//! 1. X's lifecycle record is finished;
//! 2. its `finish_coord_session_id` names a row OTHER than R — `None` means the
//!    finish is still in flight, and its ACK sets the field for the next tick;
//! 3. **the revival guard**: X's ownership of R began strictly BEFORE the
//!    mark's `finished_at`. A finished session resumed into a new pane, or
//!    resumed in-pane, must not be re-finished on the row it now runs on. The
//!    ownership instant is the owner rule's
//!    ([`pane_row_owner::live_terminal_row`]'s `owned_since_ms`: an owner
//!    entry's advance instant, else the pin seed's bind instant). An UNKNOWN
//!    instant — no live-pane answer for X on R, or one with no instant — is
//!    never read as "before": nothing is re-offered (fail safe; a missed
//!    re-offer is recoverable by the next tick or the boot heal, a finish
//!    pushed onto a revived session's row is not).
//!
//! **The door** is #2704's, serialized under the lifecycle store's
//! `finish_forward` exactly as `deliver_owed_to` is: `clear_finish_synced`
//! (when the mark reads synced), then `redeliver_finish` — which re-reads the
//! mark under `finish_forward` and enqueues only while it is still finished
//! and unsynced at the same `finished_at` — then `note_finish_sent_row`, so the
//! next tick finds the field naming R and queues nothing more. An unmark that
//! races the re-offer therefore leaves nothing queued: before the clear, the
//! clear finds no synced mark; between the clear and the re-send, the re-send
//! finds no mark; after the re-send, its `working` is queued after the
//! `Finished`, in marker order. The mark reads synced again only on R's
//! observed ACK, which also stamps `finish_coord_session_id = R`.
//!
//! **Who calls it.** Every mover's `Moved` — the live-pane adoption
//! (`bind_transcript_session_on_live_pane`), the bind trigger
//! (`on_terminal_coord_bind`) and the seam (`note_pane_row`) — and every seam
//! tick whose entry already names R ([`super::MoveOutcome::Unchanged`]: a
//! stale row's ACK that drains AFTER the move is re-offered there, once). The
//! FINISH-TIME REPAIR is the exception: it runs inside `set_finished`'s finish
//! observer, with `finish_forward` HELD, so it cannot take the door (the lock
//! is not re-entrant). It needs no re-send either: the forward it repairs is
//! itself the finish, now addressed to R. It only records R as the row the
//! mark was sent to ([`AiCoordRegistrar::note_repaired_finish_row`]).
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
use crate::session::session_lifecycle_store::FinishSync;

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
    /// The mark's row already IS the pane row.
    AlreadyOnRow,
    /// X's ownership of the row has no known instant (fail safe).
    OwnershipUnknown,
    /// X's ownership of the row began at or after the mark's `finished_at`:
    /// a finished session revived onto this row (the revival guard).
    OwnedAfterFinish,
    /// The mark changed between the decision and the door (unmarked,
    /// restamped, or stamped by a concurrent re-offer): nothing was queued.
    MarkChanged,
    /// A `Finished` for this mark was already pending to the row: the stamp
    /// was withdrawn and the row recorded, nothing more queued.
    AlreadyQueued,
    /// The finish was re-queued to `row`.
    Reoffered { from: Uuid },
    /// The outbox write failed; the mark stays unsynced for the next tick.
    NotQueued,
}

/// The two points inside the door where a test lands a racing unmark.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DoorStage {
    /// After the decision was read, before `clear_finish_synced`.
    AfterDecision,
    /// After `clear_finish_synced`, before `redeliver_finish`.
    AfterClear,
}

#[cfg(test)]
type RaceHook = (DoorStage, Box<dyn FnOnce()>);

#[cfg(test)]
thread_local! {
    /// A test's racing write, run once at its stage on this thread.
    static RACE_HOOK: std::cell::RefCell<Option<RaceHook>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` once when this thread's next re-offer reaches `stage`.
#[cfg(test)]
pub(super) fn race_at(stage: DoorStage, f: impl FnOnce() + 'static) {
    RACE_HOOK.with(|h| *h.borrow_mut() = Some((stage, Box::new(f))));
}

#[cfg(test)]
fn race_point(stage: DoorStage) {
    // Taken out before it runs, so a hook that re-enters the registrar
    // cannot find itself still installed.
    let hook = RACE_HOOK.with(|h| {
        let mut h = h.borrow_mut();
        match h.as_ref() {
            Some((s, _)) if *s == stage => h.take(),
            _ => None,
        }
    });
    if let Some((_, f)) = hook {
        f();
    }
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
        // "before the finish": re-finishing a session revived onto this row
        // would be wrong and unrecoverable here, while a missed re-offer is
        // caught by the next seam tick or the boot heal.
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
                 owns that row only since {}: a finished session revived there",
                claude_session_id, finished_at, row, owned_since
            );
            return ReofferOutcome::OwnedAfterFinish;
        }

        // The door (#2704's), every step re-checking the mark at
        // `finished_at` under `finish_forward` or the map lock.
        #[cfg(test)]
        race_point(DoorStage::AfterDecision);
        if rec.finish_synced && !store.clear_finish_synced(claude_session_id, finished_at) {
            return ReofferOutcome::MarkChanged;
        }
        #[cfg(test)]
        race_point(DoorStage::AfterClear);
        // A `Finished` for this very mark already pending to `row` (the
        // late delivery a bind or a registration ran just before) will be
        // ACKed by `row` and stamp it: queueing another would only duplicate
        // the PATCH. An unreadable outbox is UNKNOWN, so the re-send goes
        // ahead (a duplicate is harmless; a missing finish is not).
        let outcome =
            if self.finished_row_pending(claude_session_id, finished_at, row) == Some(true) {
                ReofferOutcome::AlreadyQueued
            } else {
                match self.redeliver_finish(claude_session_id, finished_at, row) {
                    None => return ReofferOutcome::MarkChanged,
                    Some(FinishSync::Queued { .. }) => ReofferOutcome::Reoffered { from },
                    Some(FinishSync::LocalOnly(reason)) => {
                        warn!(
                            "ai_coord_register: re-offering {}'s finish to pane row {} queued \
                             nothing ({:?}) — the mark stays unsynced for the next tick",
                            claude_session_id, row, reason
                        );
                        return ReofferOutcome::NotQueued;
                    }
                }
            };
        store.note_finish_sent_row(claude_session_id, Some(finished_at), row);
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
    /// held. Only an unfinished-unsynced mark at `finished_at` is touched.
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
