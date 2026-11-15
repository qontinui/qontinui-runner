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
//! began and the row's pane's monotonic bind sequence:
//!
//! - **An entry** is written by [`PaneRowOwners::advance`] — ONLY the re-key
//!   seam, and only for a source that reports the id the pane's process runs
//!   NOW (the provider registry, the SessionStart hook). The argv rung and the
//!   pane-resume re-key name the PIN, and a pin never writes an entry: the id a
//!   pane was spawned with says nothing about what it runs after `/clear` or an
//!   in-pane `/resume`. An entry is forward-only: a coord decline never rolls
//!   it back. Advancing a row to X drops every OTHER row's entry naming X, so
//!   one id never has two explicit rows. Only a row a live terminal holds is
//!   advanced.
//!
//!   **Accepted residual.** A pane with no SessionStart hook and no valid
//!   provider-registry row never gets an entry, so a live-pane adoption that
//!   finds another key on its row (the pin seed alone is contested) stays
//!   deferred for the pane's whole life. That is chosen over the alternative:
//!   letting the argv rung advance the owner would let a stale pin EVICT the
//!   session the pane runs after `/clear`, and a wrong eviction is worse than
//!   a deferral. The state is visible: each such `(row, session)` pair is
//!   logged once at info ("deferred: pin seed contested") and counted on
//!   `/health` as `registrarPinSeedTakesEverDeferred`.
//! - **A pin seed** answers for a row with no entry: the row of the
//!   newest-bound live pane PINNED to the id (the registrar's terminal lookup,
//!   which takes the newest bind).
//!
//! **Between two live panes, the one BOUND later wins**, whichever of them
//! confirmed last: an entry outranks a pin seed unless the pin seed's pane was
//! bound after the entry's pane, and an advance is refused while another row's
//! entry for the id belongs to a later-bound pane (a migration or respawn of
//! the session onto a new pane while the old one is still open). Comparing
//! the panes' BIND order, not confirmation times, is what keeps an old pane
//! that still re-confirms the id from taking it back tick after tick. The
//! order is the process-wide monotonic bind sequence, never the wall clock: a
//! clock stepped backwards during a migration would rank the old pane later,
//! and two binds in one millisecond would tie. An unknown sequence never
//! outranks anything.
//!
//! Only an entry licenses a transfer to EVICT other keys on a row. Entries
//! whose row no live terminal holds are swept ([`PaneRowOwners::sweep`]); a
//! lookup that cannot answer keeps them. [`live_terminal_row`] is the one rule,
//! with the live-terminal check injected so it is unit-testable without a PTY.
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
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard};

use uuid::Uuid;

use crate::session::NewSessionTenant;

/// How often a seam advance may sweep entries of closed panes.
pub(crate) const SWEEP_INTERVAL_MS: i64 = 30_000;

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
    /// a lifecycle record's `finished_at`). `None` when not known. Kept for the finish
    /// re-offer's revival guard (plan D5, Phase 3 — not yet built; nothing reads
    /// it today) — NEVER to rank panes: a wall
    /// clock can step backwards, and two binds can share a millisecond.
    pub bound_at_ms: Option<i64>,
    /// The terminal's process-wide monotonic bind sequence
    /// (`TerminalSession::coord_binding`): larger = bound later. THE key two
    /// panes are ranked by. `None` when not known.
    pub bind_seq: Option<u64>,
}

/// The answer of the registrar's injected pane-row lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneRowLookup {
    /// A live terminal holds the row.
    Live(PaneRowFacts),
    /// The terminal plane was READ and no live terminal holds the row.
    Gone,
    /// The terminal plane could not be read (not attached, a dropped handle,
    /// a poisoned lock). Never read as "gone": nothing is swept on it.
    Unknown,
}

impl PaneRowLookup {
    pub(crate) fn live(self) -> Option<PaneRowFacts> {
        match self {
            PaneRowLookup::Live(f) => Some(f),
            PaneRowLookup::Gone | PaneRowLookup::Unknown => None,
        }
    }
}

/// A test fake that only knows live-or-not: `None` reads as
/// [`PaneRowLookup::Gone`]. Test-only, so no production lookup can turn "could
/// not read" into "gone".
#[cfg(test)]
impl From<Option<PaneRowFacts>> for PaneRowLookup {
    fn from(f: Option<PaneRowFacts>) -> Self {
        f.map_or(PaneRowLookup::Gone, PaneRowLookup::Live)
    }
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
    /// entry, the terminal's bind instant for a pin seed. Kept for the finish
    /// re-offer's revival guard (plan D5, Phase 3 — not yet built; nothing
    /// reads it today).
    pub owned_since_ms: Option<i64>,
    /// The row's pane's monotonic bind sequence — what two panes are ranked
    /// by ([`PaneRowFacts::bind_seq`]).
    pub pane_bind_seq: Option<u64>,
}

/// Whether a pane with bind sequence `a` was bound strictly after one with
/// `b`. An unknown sequence is never "after" anything.
fn bound_after(a: Option<u64>, b: Option<u64>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a > b)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnerEntry {
    harness_id: String,
    since_ms: i64,
    pane_bind_seq: Option<u64>,
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
    /// When the last sweep ran (unix ms; 0 = never).
    last_sweep_ms: AtomicI64,
}

impl PaneRowOwners {
    /// Through a poisoned lock: both maps are plain data whose every write is
    /// a single insert/remove, so a panic elsewhere cannot leave them torn.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Make `harness_id` the owner of `row`, whose pane has bind sequence
    /// `pane_bind_seq`, forward-only. Drops every OTHER row's entry naming
    /// `harness_id` — unless that entry's pane was bound AFTER this row's,
    /// or this row's bind sequence is unknown while that entry's is known, in
    /// which case the advance is refused (the later-bound pane keeps the id;
    /// an unknown sequence never displaces a known one). A row already owned
    /// by the id keeps its original entry (its `since_ms` and stored sequence).
    /// Returns whether the row's owner changed.
    pub(crate) fn advance(
        &self,
        row: Uuid,
        harness_id: &str,
        now_ms: i64,
        pane_bind_seq: Option<u64>,
    ) -> bool {
        let harness_id = harness_id.trim();
        let mut s = self.lock();
        if s.owners.iter().any(|(r, e)| {
            *r != row
                && e.harness_id == harness_id
                && (bound_after(e.pane_bind_seq, pane_bind_seq)
                    || (pane_bind_seq.is_none() && e.pane_bind_seq.is_some()))
        }) {
            return false;
        }
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
                pane_bind_seq,
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

    /// The row an entry names `harness_id` the owner of.
    fn entry_for(&self, harness_id: &str) -> Option<(Uuid, OwnerEntry)> {
        let harness_id = harness_id.trim();
        self.lock()
            .owners
            .iter()
            .find(|(_, e)| e.harness_id == harness_id)
            .map(|(r, e)| (*r, e.clone()))
    }

    /// Whether a sweep is due at `now_ms` ([`SWEEP_INTERVAL_MS`]); claims it
    /// when it is, so concurrent callers sweep once.
    pub(crate) fn sweep_due(&self, now_ms: i64) -> bool {
        let last = self.last_sweep_ms.load(Ordering::Relaxed);
        now_ms.saturating_sub(last) >= SWEEP_INTERVAL_MS
            && self
                .last_sweep_ms
                .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }

    /// Drop every entry whose row `gone` says no live terminal holds (a
    /// closed pane). `gone` must answer `false` when it cannot tell: an
    /// unreadable terminal plane keeps every entry. Called with the lock
    /// RELEASED.
    pub(crate) fn sweep(&self, gone: impl Fn(Uuid) -> bool) {
        let judged: Vec<(Uuid, OwnerEntry)> = self
            .lock()
            .owners
            .iter()
            .map(|(r, e)| (*r, e.clone()))
            .collect();
        let dead: Vec<(Uuid, OwnerEntry)> = judged.into_iter().filter(|(r, _)| gone(*r)).collect();
        if dead.is_empty() {
            return;
        }
        let mut s = self.lock();
        for (r, judged_entry) in dead {
            // Re-checked under the lock: an entry advanced while `gone` ran is
            // not the one that was judged, and is kept.
            if s.owners.get(&r) == Some(&judged_entry) {
                s.owners.remove(&r);
            }
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
///    terminal holds it (an entry whose row is not live answers nothing; the
///    sweep removes it once the plane says the pane is gone).
/// 2. The PIN SEED: `pin_row` (the newest-bound live pane pinned to the id),
///    only while no entry on that row names another id — a pin whose row
///    another id has taken answers nothing.
/// 3. With both, the entry wins unless the pin's pane was bound strictly
///    AFTER the entry's pane, by the monotonic bind sequence. An unknown
///    sequence never outranks.
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
    let entry = owners.entry_for(harness_id).and_then(|(row, e)| {
        live(row).map(|f| LivePaneRow {
            row,
            tenant: f.tenant,
            owned_since_ms: Some(e.since_ms),
            pane_bind_seq: e.pane_bind_seq.or(f.bind_seq),
        })
    });
    let seed = pin_row(harness_id)
        .filter(|row| owners.admits(*row, harness_id))
        .and_then(|row| {
            live(row).map(|f| LivePaneRow {
                row,
                tenant: f.tenant,
                owned_since_ms: f.bound_at_ms,
                pane_bind_seq: f.bind_seq,
            })
        });
    match (entry, seed) {
        (Some(e), Some(p)) if p.row != e.row => {
            Some(if bound_after(p.pane_bind_seq, e.pane_bind_seq) {
                p
            } else {
                e
            })
        }
        (e, p) => e.or(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const X: &str = "0b5e0000-0000-4000-8000-00000000000a";
    const P: &str = "0b5e0000-0000-4000-8000-00000000000b";
    const T: Uuid = Uuid::from_u128(0x7e7a_0000_0000_4000_8000_0000_0000_0001);

    /// Facts for a pane bound with sequence `seq` (and, unless a test says
    /// otherwise, at the same number of milliseconds).
    fn facts(seq: u64) -> PaneRowFacts {
        PaneRowFacts {
            tenant: NewSessionTenant::Stated(T),
            bound_at_ms: Some(seq as i64),
            bind_seq: Some(seq),
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
                pane_bind_seq: Some(5),
            })
        );
        // The seam confirms X on R (bound at the same instant): the entry
        // wins over the pin.
        assert!(o.advance(r, X, 100, Some(5)));
        assert_eq!(
            live_terminal_row(&o, X, |_| Some(q), live).map(|l| (l.row, l.owned_since_ms)),
            Some((r, Some(100)))
        );
        // Re-advancing the same owner keeps the instant ownership began.
        assert!(!o.advance(r, X, 200, Some(5)));
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
        o.advance(r, X, 10, Some(1));
        assert_eq!(live_terminal_row(&o, P, |_| Some(r), live), None);
        assert!(!o.admits(r, P));
        assert!(o.admits(r, X));
        assert!(o.admits(Uuid::new_v4(), P), "an unowned row admits anyone");
    }

    #[test]
    fn one_id_never_has_two_entries_and_a_dead_row_answers_nothing() {
        let o = PaneRowOwners::default();
        let (r1, r2) = (Uuid::new_v4(), Uuid::new_v4());
        o.advance(r1, X, 1, Some(1));
        o.advance(r2, X, 2, Some(2));
        assert_eq!(o.owner_of(r1), None, "one entry per id");
        assert_eq!(o.owner_of(r2).as_deref(), Some(X));

        // r2's terminal is not live: the entry answers nothing, but only the
        // sweep removes it.
        assert_eq!(live_terminal_row(&o, X, |_| None, |_| None), None);
        assert_eq!(o.owner_of(r2).as_deref(), Some(X));
    }

    #[test]
    fn the_owner_does_not_roll_back_when_nothing_advances_it() {
        // A coord decline restores the SessionRegistry record's id; it never
        // reaches this map, so only a later advance can change the owner.
        let o = PaneRowOwners::default();
        let r = Uuid::new_v4();
        let live = |row: Uuid| (row == r).then(|| facts(1));
        o.advance(r, X, 1, Some(1));
        for _ in 0..8 {
            assert_eq!(
                live_terminal_row(&o, X, |_| None, live).map(|l| l.row),
                Some(r)
            );
            assert_eq!(live_terminal_row(&o, P, |_| Some(r), live), None);
        }
    }

    /// Migration overlap: the old pane's row carries a seam entry for X; a
    /// NEW pane pinned to X bound after the OLD PANE was bound outranks the
    /// entry — however recently the old pane confirmed — while a pin bound
    /// before it does not.
    #[test]
    fn a_pane_bound_after_the_entrys_pane_outranks_it() {
        let o = PaneRowOwners::default();
        let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
        // Confirmed very recently (since = 10_000), but its pane was bound at 100.
        o.advance(old, X, 10_000, Some(100));
        let at =
            |bound: u64| move |row: Uuid| Some(if row == new { facts(bound) } else { facts(100) });
        assert_eq!(
            live_terminal_row(&o, X, |_| Some(new), at(200)).map(|l| l.row),
            Some(new),
            "a later-bound pane outranks a more recent confirmation"
        );
        assert_eq!(
            live_terminal_row(&o, X, |_| Some(new), at(50)).map(|l| l.row),
            Some(old),
            "an earlier-bound pane does not"
        );
        assert_eq!(o.owner_of(new), None, "no entry is minted for the pin");
        let unknown = |row: Uuid| {
            Some(PaneRowFacts {
                tenant: NewSessionTenant::Undetermined,
                bound_at_ms: Some(10_000),
                bind_seq: (row != new).then_some(100),
            })
        };
        assert_eq!(
            live_terminal_row(&o, X, |_| Some(new), unknown).map(|l| l.row),
            Some(old),
            "an unknown bind sequence never outranks an entry"
        );
    }

    /// Fourth review: panes are ranked by the monotonic bind SEQUENCE, not the
    /// wall clock. A clock stepped backwards during a migration gives the new
    /// pane EARLIER millis, and two binds can share a millisecond; the
    /// sequence still says which pane was bound later.
    #[test]
    fn the_bind_sequence_ranks_panes_whatever_the_wall_clock_says() {
        let at = |seq: u64, ms: i64| PaneRowFacts {
            tenant: NewSessionTenant::Undetermined,
            bound_at_ms: Some(ms),
            bind_seq: Some(seq),
        };
        for (old_ms, new_ms, case) in [
            (5_000, 1_000, "the clock stepped backwards"),
            (5_000, 5_000, "both binds in one millisecond"),
        ] {
            let o = PaneRowOwners::default();
            let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
            assert!(o.advance(old, X, 1, Some(7)));
            let live = move |row: Uuid| {
                Some(if row == new {
                    at(8, new_ms)
                } else {
                    at(7, old_ms)
                })
            };
            assert_eq!(
                live_terminal_row(&o, X, |_| Some(new), live).map(|l| l.row),
                Some(new),
                "{case}: the later sequence wins"
            );
            assert!(o.advance(new, X, 2, Some(8)), "{case}");
            assert!(
                !o.advance(old, X, 3, Some(7)),
                "{case}: the earlier sequence cannot take it back"
            );
            assert_eq!(
                live_terminal_row(&o, X, |_| None, live).map(|l| l.owned_since_ms),
                Some(Some(2)),
                "{case}: the millis stay for the revival guard only"
            );
        }
    }

    /// The flip the second review found: once the later-bound pane holds the
    /// id's entry, the earlier-bound pane's re-confirmations are refused, so
    /// the owner cannot alternate between the two.
    #[test]
    fn an_earlier_bound_pane_cannot_take_the_entry_back() {
        let o = PaneRowOwners::default();
        let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(o.advance(old, X, 1, Some(100)));
        assert!(
            o.advance(new, X, 2, Some(200)),
            "the later-bound pane takes it"
        );
        for t in 3..10 {
            assert!(!o.advance(old, X, t, Some(100)), "refused at tick {t}");
            assert!(!o.advance(new, X, t, Some(200)));
        }
        assert_eq!(o.owner_of(new).as_deref(), Some(X));
        assert_eq!(o.owner_of(old), None);
        // An unknown instant never displaces a known one...
        assert!(!o.advance(old, X, 20, None));
        assert_eq!(o.owner_of(new).as_deref(), Some(X));
        // ...but between unknowns the latest confirmation still wins.
        let (u1, u2) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(o.advance(u1, P, 1, None));
        assert!(o.advance(u2, P, 2, None));
        assert_eq!(o.owner_of(u1), None);
    }

    /// Review 3, A7: an entry re-advanced while the sweep's liveness check
    /// ran is not the entry that was judged, so it is kept.
    #[test]
    fn a_sweep_keeps_an_entry_advanced_while_it_judged() {
        let o = PaneRowOwners::default();
        let r = Uuid::new_v4();
        o.advance(r, X, 1, None);
        o.sweep(|row| {
            // The row is judged gone, but a new owner lands before removal.
            o.advance(row, P, 2, None);
            true
        });
        assert_eq!(
            o.owner_of(r).as_deref(),
            Some(P),
            "the newer entry survives"
        );
    }

    #[test]
    fn a_sweep_drops_only_rows_the_plane_says_are_gone() {
        let o = PaneRowOwners::default();
        let (open, closed, unread) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        o.advance(open, X, 1, None);
        o.advance(closed, P, 1, None);
        o.advance(unread, "z", 1, None);
        let lookup = |r: Uuid| {
            if r == open {
                PaneRowLookup::Live(facts(1))
            } else if r == closed {
                PaneRowLookup::Gone
            } else {
                PaneRowLookup::Unknown
            }
        };
        o.sweep(|r| lookup(r) == PaneRowLookup::Gone);
        assert_eq!(o.owner_of(open).as_deref(), Some(X));
        assert_eq!(o.owner_of(closed), None);
        assert_eq!(o.owner_of(unread).as_deref(), Some("z"), "unknown keeps");
    }

    #[test]
    fn the_sweep_is_rate_limited() {
        let o = PaneRowOwners::default();
        assert!(o.sweep_due(SWEEP_INTERVAL_MS));
        assert!(!o.sweep_due(SWEEP_INTERVAL_MS + 1));
        assert!(o.sweep_due(2 * SWEEP_INTERVAL_MS));
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
