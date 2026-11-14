//! Which harness id a live pane's coord row hosts — the registrar's PANE-ROW
//! OWNER, and the door each registrar row was bound through.
//!
//! Plan `2026-11-12-transcript-autobind-adopts-a-previous-boot-row-that-a-live-pane-has-replaced`
//! D1/D4. A terminal pane registers its own coord row through
//! `SessionRegistry`; the registrar's `reverse` index learns the harness id a
//! pane runs only when something binds it. At boot, transcript auto-bind used
//! to ask coord which row holds the id, and coord named the PREVIOUS boot's row
//! while a live pane already hosted the session on a new one. The runner has
//! better evidence: it hosts the process. This module is that evidence.
//!
//! ## The owner of a row
//!
//! One entry per coord row, `row → harness id`, with the instant the ownership
//! began:
//!
//! - **An entry** is written by [`PaneRowOwners::advance`] — the re-key seam
//!   confirming (pid-exact) that the pane's process runs that id, or a pane
//!   being bound to a row for the id it is pinned to. It is forward-only: a
//!   coord decline never rolls it back. Advancing a row to X drops every OTHER
//!   row's entry naming X, so one id never has two explicit rows (the latest
//!   confirmation wins).
//! - **A pin seed** answers for a row with no entry: the row of the
//!   newest-bound live pane PINNED to the id (the registrar's terminal lookup,
//!   which takes the newest bind).
//!
//! An entry outranks a pin seed. Entries whose row no live terminal holds are
//! pruned when read. [`live_terminal_row`] is the one rule over both, with the
//! live-terminal check injected so it is unit-testable without a PTY.
//!
//! ## The bind door
//!
//! [`BindDoor`] records which registrar door registered each row, beside the
//! registrar's `adopted` set, so the movers that may only move an auto-bind
//! entry can tell it from a sniffed typed resume or a pinned AI session.
//!
//! The lock here is a LEAF: no method calls out while holding it, so the
//! registrar may take it inside its `reverse` critical section.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use uuid::Uuid;

use crate::session::NewSessionTenant;

/// Which registrar door registered a coord row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindDoor {
    /// `bind_transcript_session` — transcript auto-bind or
    /// `POST /sessions/transcript-bind`, adopted or minted (the registrar's
    /// `adopted` set tells which).
    Transcript,
    /// `register_sniffed_session` — a typed `claude --resume X` / `--session-id
    /// X`. X has its OWN identity; no pane-row mover touches it except the
    /// bind trigger for a pane pinned to X itself.
    Sniffed,
    /// The pinned AI / task-run plane (`register_session[_in]`).
    Pinned,
}

/// What the runner knows about a coord row some LIVE terminal holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneRowFacts {
    /// The tenant the row's own writes are sent under (plan D3):
    /// `Stated(t)` for a row whose `SessionRegistry` scope is `Owned(t)`, else
    /// `Undetermined` — never the machine default.
    pub tenant: NewSessionTenant,
    /// When the terminal was bound to the row, unix milliseconds (the unit of
    /// a lifecycle record's `finished_at`). `None` when not known.
    pub bound_at_ms: Option<i64>,
}

/// A pane row's tenant per plan D3, from its `SessionRegistry` scope: the
/// owning tenant when the row's writes are sent under one, else
/// `Undetermined` — no top-level `tenant_id`, never the machine default.
pub(crate) fn tenant_from_scope(scope: crate::auth::TenantScope) -> NewSessionTenant {
    match scope {
        crate::auth::TenantScope::Owned(t) => NewSessionTenant::Stated(t),
        _ => NewSessionTenant::Undetermined,
    }
}

/// A live pane's row that a harness id may be moved onto
/// ([`live_terminal_row`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LivePaneRow {
    pub row: Uuid,
    /// The row's tenant per D3 (see [`PaneRowFacts::tenant`]).
    pub tenant: NewSessionTenant,
    /// When the id's ownership of the row began: the advance instant for an
    /// entry, the terminal's bind instant for a pin seed. Read by the finish
    /// re-offer's revival guard.
    pub owned_since_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnerEntry {
    harness_id: String,
    since_ms: i64,
}

#[derive(Default)]
struct State {
    owners: HashMap<Uuid, OwnerEntry>,
    doors: HashMap<Uuid, BindDoor>,
}

/// The owner map and the bind-door tags. One instance per registrar.
#[derive(Default)]
pub(crate) struct PaneRowOwners {
    state: Mutex<State>,
}

impl PaneRowOwners {
    /// Through a poisoned lock: both maps are plain data whose every write is
    /// a single insert/remove, so a panic elsewhere cannot leave them torn.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Make `harness_id` the owner of `row`, forward-only. Drops every OTHER
    /// row's entry naming `harness_id`. A row already owned by the id keeps
    /// its original instant (the ownership began then). Returns whether the
    /// row's owner changed.
    pub(crate) fn advance(&self, row: Uuid, harness_id: &str, now_ms: i64) -> bool {
        let harness_id = harness_id.trim();
        let mut s = self.lock();
        s.owners
            .retain(|r, e| *r == row || e.harness_id != harness_id);
        if s.owners
            .get(&row)
            .is_some_and(|e| e.harness_id == harness_id)
        {
            return false;
        }
        s.owners.insert(
            row,
            OwnerEntry {
                harness_id: harness_id.to_string(),
                since_ms: now_ms,
            },
        );
        true
    }

    /// The id an ENTRY names as `row`'s owner (a pin seed is not an entry).
    pub(crate) fn owner_of(&self, row: Uuid) -> Option<String> {
        self.lock().owners.get(&row).map(|e| e.harness_id.clone())
    }

    /// Whether `harness_id` may be routed to `row`: no entry on `row` names
    /// ANOTHER id.
    pub(crate) fn admits(&self, row: Uuid, harness_id: &str) -> bool {
        self.lock()
            .owners
            .get(&row)
            .is_none_or(|e| e.harness_id == harness_id.trim())
    }

    /// The row an entry names `harness_id` the owner of, with its instant.
    fn entry_for(&self, harness_id: &str) -> Option<(Uuid, i64)> {
        let harness_id = harness_id.trim();
        self.lock()
            .owners
            .iter()
            .find(|(_, e)| e.harness_id == harness_id)
            .map(|(r, e)| (*r, e.since_ms))
    }

    /// Drop `row`'s entry if it still names `harness_id` (no live terminal
    /// holds the row any more).
    fn prune(&self, row: Uuid, harness_id: &str) {
        let mut s = self.lock();
        if s.owners
            .get(&row)
            .is_some_and(|e| e.harness_id == harness_id.trim())
        {
            s.owners.remove(&row);
        }
    }

    /// Tag `row` with the door that registered it.
    pub(crate) fn set_door(&self, row: Uuid, door: BindDoor) {
        self.lock().doors.insert(row, door);
    }

    /// The door that registered `row`, if this registrar registered it.
    pub(crate) fn door_of(&self, row: Uuid) -> Option<BindDoor> {
        self.lock().doors.get(&row).copied()
    }

    /// Forget `row`'s door (its registration was evicted).
    pub(crate) fn forget_door(&self, row: Uuid) {
        self.lock().doors.remove(&row);
    }

    /// Carry `from`'s door onto `to` (an entry moved rows).
    pub(crate) fn move_door(&self, from: Uuid, to: Uuid) {
        let mut s = self.lock();
        if let Some(d) = s.doors.remove(&from) {
            s.doors.insert(to, d);
        }
    }
}

/// THE rule: the live pane row `harness_id` owns, if any.
///
/// 1. An ENTRY naming `harness_id` answers its row while `live` says a
///    terminal still holds it; a dead entry is pruned and the rule falls
///    through.
/// 2. Else the PIN SEED: `pin_row` (the newest-bound live pane pinned to the
///    id), but only while no entry on that row names another id — a pin whose
///    row another id has taken answers `None`.
///
/// `live` answers a row's [`PaneRowFacts`] when some live terminal holds it.
/// Neither closure is called with the owner lock held.
pub(crate) fn live_terminal_row(
    owners: &PaneRowOwners,
    harness_id: &str,
    pin_row: impl FnOnce(&str) -> Option<Uuid>,
    live: impl Fn(Uuid) -> Option<PaneRowFacts>,
) -> Option<LivePaneRow> {
    let harness_id = harness_id.trim();
    if harness_id.is_empty() {
        return None;
    }
    if let Some((row, since)) = owners.entry_for(harness_id) {
        match live(row) {
            Some(f) => {
                return Some(LivePaneRow {
                    row,
                    tenant: f.tenant,
                    owned_since_ms: Some(since),
                })
            }
            None => owners.prune(row, harness_id),
        }
    }
    let row = pin_row(harness_id)?;
    if !owners.admits(row, harness_id) {
        return None;
    }
    let f = live(row)?;
    Some(LivePaneRow {
        row,
        tenant: f.tenant,
        owned_since_ms: f.bound_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const X: &str = "0b5e0000-0000-4000-8000-00000000000a";
    const P: &str = "0b5e0000-0000-4000-8000-00000000000b";
    const T: Uuid = Uuid::from_u128(0x7e7a_0000_0000_4000_8000_0000_0000_0001);

    fn facts(bound_at_ms: i64) -> PaneRowFacts {
        PaneRowFacts {
            tenant: NewSessionTenant::Stated(T),
            bound_at_ms: Some(bound_at_ms),
        }
    }

    #[test]
    fn an_entry_outranks_the_pin_seed_and_carries_its_advance_instant() {
        let o = PaneRowOwners::default();
        let (r, q) = (Uuid::new_v4(), Uuid::new_v4());
        // Pin seed only: X pinned to Q.
        let live = |row: Uuid| (row == r || row == q).then(|| facts(5));
        assert_eq!(
            live_terminal_row(&o, X, |_| Some(q), live),
            Some(LivePaneRow {
                row: q,
                tenant: NewSessionTenant::Stated(T),
                owned_since_ms: Some(5),
            })
        );
        // The seam confirms X on R: the entry wins over the pin.
        assert!(o.advance(r, X, 100));
        assert_eq!(
            live_terminal_row(&o, X, |_| Some(q), live).map(|l| (l.row, l.owned_since_ms)),
            Some((r, Some(100)))
        );
        // Re-advancing the same owner keeps the instant ownership began.
        assert!(!o.advance(r, X, 200));
        assert_eq!(
            live_terminal_row(&o, X, |_| None, live).map(|l| l.owned_since_ms),
            Some(Some(100))
        );
    }

    #[test]
    fn a_pin_whose_row_another_id_owns_answers_nothing() {
        let o = PaneRowOwners::default();
        let r = Uuid::new_v4();
        let live = |row: Uuid| (row == r).then(|| facts(1));
        assert_eq!(
            live_terminal_row(&o, P, |_| Some(r), live).map(|l| l.row),
            Some(r)
        );
        o.advance(r, X, 10);
        assert_eq!(live_terminal_row(&o, P, |_| Some(r), live), None);
        assert!(!o.admits(r, P));
        assert!(o.admits(r, X));
        assert!(o.admits(Uuid::new_v4(), P), "an unowned row admits anyone");
    }

    #[test]
    fn one_id_never_has_two_entries_and_dead_rows_are_pruned() {
        let o = PaneRowOwners::default();
        let (r1, r2) = (Uuid::new_v4(), Uuid::new_v4());
        o.advance(r1, X, 1);
        o.advance(r2, X, 2);
        assert_eq!(o.owner_of(r1), None, "the latest confirmation wins");
        assert_eq!(o.owner_of(r2).as_deref(), Some(X));

        // r2's terminal is gone: the entry is pruned when read and the rule
        // falls through to the pin.
        assert_eq!(live_terminal_row(&o, X, |_| None, |_| None), None);
        assert_eq!(o.owner_of(r2), None);
    }

    #[test]
    fn the_owner_does_not_roll_back_when_nothing_advances_it() {
        // A coord decline restores the SessionRegistry record's id; it never
        // reaches this map, so only a later advance can change the owner.
        let o = PaneRowOwners::default();
        let r = Uuid::new_v4();
        let live = |row: Uuid| (row == r).then(|| facts(1));
        o.advance(r, X, 1);
        for _ in 0..8 {
            assert_eq!(
                live_terminal_row(&o, X, |_| None, live).map(|l| l.row),
                Some(r)
            );
            assert_eq!(live_terminal_row(&o, P, |_| Some(r), live), None);
        }
    }

    #[test]
    fn doors_move_with_their_entry() {
        let o = PaneRowOwners::default();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        o.set_door(a, BindDoor::Transcript);
        o.move_door(a, b);
        assert_eq!(o.door_of(a), None);
        assert_eq!(o.door_of(b), Some(BindDoor::Transcript));
        o.forget_door(b);
        assert_eq!(o.door_of(b), None);
    }
}
