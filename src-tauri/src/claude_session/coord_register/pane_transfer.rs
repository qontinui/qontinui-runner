//! One transfer moves a harness id onto a live pane's coord row, and the
//! movers that call it (plan
//! `2026-11-12-transcript-autobind-adopts-a-previous-boot-row-that-a-live-pane-has-replaced`
//! D2/D3/D4; the bind trigger is qontinui-runner#2711's `rebind`, rebuilt by
//! route-around).
//!
//! The registrar's `reverse` index is the ONE routing truth: heartbeats,
//! progress, transcript chunks, finishes and `Closed` all follow it. A pane
//! registers its own row through `SessionRegistry`, so when the registrar's
//! entry for the harness id a pane runs names ANOTHER row (a previous boot's
//! row coord named at auto-bind, a row minted before the pane's process
//! switched id, the row of a pane that was respawned), every write goes to the
//! wrong row — and, because coord counts those writes as heartbeats, keeps that
//! row active and the pane row's re-key refused. Each mover below corrects
//! `reverse` through [`AiCoordRegistrar::move_onto_pane_row`] rather than
//! layering a second precedence rule over it:
//!
//! | mover | fires when | entries it may move |
//! |---|---|---|
//! | live-pane adoption | auto-bind, the id unbound | none — `register_inner` reserves one (`take_from_live_pane`) |
//! | [`Mover::Bind`] | a pane PINNED to the id is bound to a row | any |
//! | [`Mover::Seam`] | the re-key seam confirms (row, id) | `bind_transcript_session` entries only |
//! | [`Mover::Repair`] | a finish finds the entry off the live pane row | `bind_transcript_session` entries only |
//!
//! **Known gap — transcript history is not replayed onto the new row.** Chunks
//! emitted after a move address the new row; those emitted before it stay on
//! the old one. The tailer's lane offset and unsent-prefix floor are keyed by
//! harness id, not by row (the earlier plan's vet D6 chose recording this over
//! replaying). The live-pane adoption avoids the split at boot by adopting the
//! right row before the replay.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::AiCoordRegistrar;
use crate::claude_session::pane_row_owner::{self, BindDoor, LivePaneRow, PaneRowFacts};
use crate::session::session_lifecycle_store::FinishSync;
use crate::session::{NewSessionTenant, SessionEventKind};

/// How many times a finish found a harness id's registrar entry on a row
/// other than its live pane's row and the finish-time repair moved it
/// ([`Mover::Repair`]). Served on `/health` as
/// `registrarTerminalRowDisagreements` (#2711's name and door). With the
/// live-pane adoption, the bind trigger and the seam in place it should stay
/// near zero; a climb means one of them missed an ordering.
static REGISTRAR_TERMINAL_ROW_DISAGREEMENTS: AtomicU64 = AtomicU64::new(0);

/// See [`REGISTRAR_TERMINAL_ROW_DISAGREEMENTS`].
pub(crate) fn registrar_terminal_row_disagreements() -> u64 {
    REGISTRAR_TERMINAL_ROW_DISAGREEMENTS.load(Ordering::Relaxed)
}

/// How many moves were refused because an owner entry on the target row names
/// ANOTHER harness id ([`MoveOutcome::NotOwner`]). Served on `/health` as
/// `registrarPaneRowMovesNotOwner`.
static PANE_ROW_MOVES_NOT_OWNER: AtomicU64 = AtomicU64::new(0);

/// See [`PANE_ROW_MOVES_NOT_OWNER`].
pub(crate) fn pane_row_moves_not_owner() -> u64 {
    PANE_ROW_MOVES_NOT_OWNER.load(Ordering::Relaxed)
}

/// Which mover asked for a transfer — it decides which entry may move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mover {
    /// A pane pinned to the id was just bound to the row
    /// ([`AiCoordRegistrar::on_terminal_coord_bind`]): the event says a pane
    /// was created for the id, so ANY entry moves.
    Bind,
    /// The re-key seam confirmed the row onto the id
    /// ([`AiCoordRegistrar::note_pane_row`]).
    Seam,
    /// A finish found the entry off the id's live pane row
    /// ([`AiCoordRegistrar::forward_finish_change`]).
    Repair,
}

impl Mover {
    fn as_str(self) -> &'static str {
        match self {
            Mover::Bind => "bind",
            Mover::Seam => "seam",
            Mover::Repair => "repair",
        }
    }
}

/// What [`AiCoordRegistrar::move_onto_pane_row`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoveOutcome {
    /// The id has no registrar entry: nothing moves, and no mover binds one.
    Unbound,
    /// The entry already names the row.
    Unchanged,
    /// The row is not the id's live pane row: an owner entry on it names
    /// another id (or, for the seam and the repair, no live terminal holds
    /// it). Nothing moves; counted.
    NotOwner,
    /// The entry was registered through a door this mover may not move (a
    /// sniffed typed resume or the pinned AI plane, for the seam and the
    /// repair). Nothing moves.
    NotEligible,
    /// The entry and its row-keyed state moved from `from`.
    Moved { from: Uuid },
    /// The `reverse` lock is poisoned: nothing can be moved.
    Disabled,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl AiCoordRegistrar {
    /// Attach the live-pane-row lookup (once, at startup). See
    /// `Inner::pane_row_tenant`.
    pub fn attach_pane_row_tenant(
        &self,
        f: impl Fn(Uuid) -> Option<PaneRowFacts> + Send + Sync + 'static,
    ) {
        if self.inner.pane_row_tenant.set(Box::new(f)).is_err() {
            warn!("ai_coord_register: pane-row tenant lookup already attached — ignoring");
        }
    }

    /// `row`'s facts when some live terminal holds it.
    fn pane_row_facts(&self, row: Uuid) -> Option<PaneRowFacts> {
        self.inner.pane_row_tenant.get().and_then(|f| f(row))
    }

    /// The tenant a mover stamps on `row` (plan D3): the live pane row's own
    /// scope, else `Undetermined` — never the machine default.
    fn pane_row_tenant(&self, row: Uuid) -> NewSessionTenant {
        self.pane_row_facts(row)
            .map(|f| f.tenant)
            .unwrap_or(NewSessionTenant::Undetermined)
    }

    /// The live pane row `claude_session_id` owns, by the pane-row owner rule
    /// ([`pane_row_owner::live_terminal_row`]): an owner entry naming the id,
    /// else the newest-bound live pane PINNED to it whose row no entry gives
    /// to another id. Reads the terminal plane, so never call it while holding
    /// a registrar map, the `TerminalManager` `sessions` lock or the
    /// `SessionRegistry` map.
    pub(crate) fn live_terminal_row(&self, claude_session_id: &str) -> Option<LivePaneRow> {
        pane_row_owner::live_terminal_row(
            &self.inner.pane_rows,
            claude_session_id,
            |id| {
                self.inner
                    .terminal_coord_lookup
                    .get()
                    .and_then(|lookup| lookup(id))
            },
            |row| self.pane_row_facts(row),
        )
    }

    /// Evict every key other than `keep` bound to `row` from `reverse`, with
    /// the row's per-row state those keys left behind (`forward`, `tenants`,
    /// `undecided`) — the caller re-stamps it for `keep`. Called with `rev`
    /// (the `reverse` guard) held: takes `tenants`, `undecided` and `forward`
    /// inside it, the registrar's lock order. No `Closed` is queued: the row
    /// stays live under `keep`. Returns the evicted keys.
    pub(super) fn evict_other_keys_on(
        &self,
        rev: &mut HashMap<String, Uuid>,
        row: Uuid,
        keep: &str,
    ) -> Vec<String> {
        let evicted: Vec<String> = rev
            .iter()
            .filter(|(k, v)| **v == row && k.as_str() != keep)
            .map(|(k, _)| k.clone())
            .collect();
        if evicted.is_empty() {
            return evicted;
        }
        for k in &evicted {
            rev.remove(k);
        }
        self.inner
            .tenants
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&row);
        self.inner
            .undecided
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&row);
        if let Ok(mut fwd) = self.inner.forward.lock() {
            if fwd.get(&row).is_some_and(|k| evicted.contains(k)) {
                fwd.remove(&row);
            }
        }
        info!(
            "ai_coord_register: {} takes coord session {}; evicted {:?} (they keep no row in \
             this process)",
            keep, row, evicted
        );
        evicted
    }

    /// THE transfer (plan D4): move `claude_session_id`'s registrar entry onto
    /// the live pane row `row`, stamping `row` with `tenant` (plan D3).
    ///
    /// 1. **Owner precondition.** No owner entry on `row` may name another id
    ///    ([`MoveOutcome::NotOwner`], counted). The live-terminal half of the
    ///    owner rule is the CALLER's (it reads the terminal plane, so it runs
    ///    before this section).
    /// 2. **One critical section** under `reverse`, taking `tenants`,
    ///    `undecided`, `adopted`, the pane-row leaf and `forward` inside it in
    ///    that order; `reverse` is never taken while holding another.
    /// 3. `reverse[id] == row` → [`MoveOutcome::Unchanged`]; no entry →
    ///    [`MoveOutcome::Unbound`] (no mover binds). The seam and the repair
    ///    move only a `bind_transcript_session` entry
    ///    ([`MoveOutcome::NotEligible`] otherwise).
    /// 4. Every other key on `row` is evicted; `reverse[id] = row`;
    ///    `forward[row]` takes the old row's key, else the id; `row` joins
    ///    `adopted` (a pane row is minted by `SessionRegistry`, never here) and
    ///    carries the old row's bind door.
    /// 5. The old row's `forward` / `tenants` / `undecided` / `adopted` entries
    ///    are evicted. An old row this process MINTED (not adopted) gets a
    ///    `Closed`, stamped with ITS OWN tenant as read before the eviction:
    ///    nothing else in the runner will ever close it, and coord honours an
    ///    explicit close at once (no reaper lapse). An ADOPTED old row gets no
    ///    `Closed` — a sibling runner may hold it; coord's reaper owns it.
    pub(crate) fn move_onto_pane_row(
        &self,
        claude_session_id: &str,
        row: Uuid,
        mover: Mover,
        tenant: NewSessionTenant,
    ) -> MoveOutcome {
        let (from, minted_from_tenant) = {
            let Ok(mut rev) = self.inner.reverse.lock() else {
                warn!(
                    "ai_coord_register: reverse-index lock poisoned — not moving {} onto {}",
                    claude_session_id, row
                );
                return MoveOutcome::Disabled;
            };
            let Some(from) = rev.get(claude_session_id).copied() else {
                return MoveOutcome::Unbound;
            };
            if from == row {
                return MoveOutcome::Unchanged;
            }
            if matches!(mover, Mover::Seam | Mover::Repair)
                && self.inner.pane_rows.door_of(from) != Some(BindDoor::Transcript)
            {
                return MoveOutcome::NotEligible;
            }
            if !self.inner.pane_rows.admits(row, claude_session_id) {
                PANE_ROW_MOVES_NOT_OWNER.fetch_add(1, Ordering::Relaxed);
                debug!(
                    "ai_coord_register: not moving {} onto {} ({}) — another id owns that row",
                    claude_session_id,
                    row,
                    mover.as_str()
                );
                return MoveOutcome::NotOwner;
            }
            self.evict_other_keys_on(&mut rev, row, claude_session_id);
            rev.insert(claude_session_id.to_string(), row);
            // Per-row state moves in the SAME section: from here on
            // `session_id_for` answers `row`, so a concurrent row must already
            // find its tenant there. Through a poisoned lock, as `evict_tenant`.
            let from_tenant = {
                let mut tenants = self.inner.tenants.lock().unwrap_or_else(|p| p.into_inner());
                let from_tenant = tenants.remove(&from);
                tenants.remove(&row);
                if let Some(t) = tenant.tenant() {
                    tenants.insert(row, t);
                }
                from_tenant
            };
            {
                let mut undecided = self
                    .inner
                    .undecided
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                undecided.remove(&from);
                undecided.remove(&row);
                if tenant.tenant().is_none() && tenant.is_standing() {
                    undecided.insert(row, tenant);
                }
            }
            let from_minted = {
                let mut adopted = self.inner.adopted.lock().unwrap_or_else(|p| p.into_inner());
                let was_adopted = adopted.remove(&from);
                adopted.insert(row);
                !was_adopted
            };
            self.inner.pane_rows.move_door(from, row);
            if let Ok(mut fwd) = self.inner.forward.lock() {
                let key = fwd
                    .remove(&from)
                    .unwrap_or_else(|| claude_session_id.to_string());
                fwd.insert(row, key);
            }
            (from, from_minted.then_some(from_tenant))
        };
        info!(
            "ai_coord_register: moved {} onto pane row {} from {} ({}; old row {})",
            claude_session_id,
            row,
            from,
            mover.as_str(),
            if minted_from_tenant.is_some() {
                "minted here — closing it"
            } else {
                "adopted — left to coord"
            }
        );
        if let Some(from_tenant) = minted_from_tenant {
            let mut payload = json!({ "id": from });
            if let Some(t) = from_tenant {
                payload["tenant_id"] = json!(t);
            }
            if let Err(e) = self.inner.outbox.record(
                self.inner.machine_id,
                from,
                SessionEventKind::Closed,
                payload,
            ) {
                warn!(
                    "ai_coord_register: outbox Closed write failed for the superseded row {} of {} \
                     (best-effort): {}",
                    from, claude_session_id, e
                );
            }
        }
        MoveOutcome::Moved { from }
    }

    /// The coord-bind observer's body (main.rs): terminal `terminal_id`,
    /// PINNED to harness id `pinned_session_id`, has just been bound to coord
    /// row `coord_session_id` (a fresh pane, a respawn, an account migration,
    /// the resume door — each mints a new row).
    ///
    /// 1. The bind is evidence that the row hosts the pinned id, so — unless
    ///    an owner entry on the row already names another id — the row's owner
    ///    advances to it, dropping the id's entry on any older pane's row.
    /// 2. The registrar's entry for the id is moved onto the row
    ///    ([`Mover::Bind`]: any entry), stamped with the row's tenant.
    /// 3. THEN the finished marks owed to the row are delivered
    ///    ([`Self::deliver_owed_finish`]), so an owed mark resolves to the new
    ///    row, never the dead one.
    ///
    /// Same lock rules as [`Self::deliver_owed_finish`].
    pub fn on_terminal_coord_bind(
        &self,
        pinned_session_id: &str,
        terminal_id: &str,
        coord_session_id: Uuid,
    ) -> Vec<FinishSync> {
        if pinned_session_id.trim().is_empty() {
            // A pane pinned to no harness id (a plain shell) hosts no session
            // the registrar could follow.
            return self.deliver_owed_finish(pinned_session_id, terminal_id, coord_session_id);
        }
        let facts = self.pane_row_facts(coord_session_id);
        if self
            .inner
            .pane_rows
            .admits(coord_session_id, pinned_session_id)
        {
            let at = facts.and_then(|f| f.bound_at_ms).unwrap_or_else(now_ms);
            self.inner
                .pane_rows
                .advance(coord_session_id, pinned_session_id, at);
        }
        let tenant = facts
            .map(|f| f.tenant)
            .unwrap_or(NewSessionTenant::Undetermined);
        match self.move_onto_pane_row(pinned_session_id, coord_session_id, Mover::Bind, tenant) {
            MoveOutcome::NotOwner => warn!(
                "ai_coord_register: terminal {} (pinned {}) bound to coord session {}, but \
                 another session owns that row — the registrar's entry stays where it is",
                terminal_id, pinned_session_id, coord_session_id
            ),
            MoveOutcome::Disabled => warn!(
                "ai_coord_register: terminal {} (pinned {}) bound to coord session {}, but the \
                 registrar could not follow it — its writes stay on the old row",
                terminal_id, pinned_session_id, coord_session_id
            ),
            _ => {}
        }
        self.deliver_owed_finish(pinned_session_id, terminal_id, coord_session_id)
    }

    /// The re-key seam's observer (plan D2): `SessionRegistry` has just
    /// answered `Queued` or `AlreadyCurrent` for pane row `row` and harness id
    /// `claude_session_id` — pid-exact evidence that the pane runs that id.
    /// Fired on `Queued`, before coord accepts, and a coord decline never
    /// undoes it: the stale row stays active only while this runner keeps
    /// writing to it, and the move stops those writes.
    ///
    /// 1. Advances `row`'s owner to the id (forward-only; drops any other
    ///    row's entry naming it).
    /// 2. When a live terminal holds `row`, moves the id's
    ///    `bind_transcript_session` entry — adopted or minted — onto it
    ///    ([`Mover::Seam`]). A minted old row is `Closed`; an adopted one is
    ///    not. A sniffed or pinned-plane entry is never moved, and an absent
    ///    entry is never bound: binding here would skip auto-bind's replay of
    ///    the transcript's backlog.
    ///
    /// Runs outside every registry lock (the seam fires after
    /// `confirm_claude_code_session_id` returns) and reads the terminal plane.
    pub fn note_pane_row(&self, row: Uuid, claude_session_id: &str) -> MoveOutcome {
        let claude_session_id = claude_session_id.trim();
        self.inner
            .pane_rows
            .advance(row, claude_session_id, now_ms());
        let Some(facts) = self.pane_row_facts(row) else {
            return MoveOutcome::NotOwner;
        };
        self.move_onto_pane_row(claude_session_id, row, Mover::Seam, facts.tenant)
    }

    /// The finish-time repair (plan D4): when `claude_session_id`'s entry is
    /// on a row other than its live pane row, move it there ([`Mover::Repair`]:
    /// auto-bind entries only) and count it. Never binds: an unbound id's
    /// finish resolves through the owner-checked terminal arm.
    pub(super) fn repair_onto_live_pane(&self, claude_session_id: &str) {
        let Some(current) = self.session_id_for(claude_session_id) else {
            return;
        };
        let Some(live) = self.live_terminal_row(claude_session_id) else {
            return;
        };
        if live.row == current {
            return;
        }
        if let MoveOutcome::Moved { from } =
            self.move_onto_pane_row(claude_session_id, live.row, Mover::Repair, live.tenant)
        {
            REGISTRAR_TERMINAL_ROW_DISAGREEMENTS.fetch_add(1, Ordering::Relaxed);
            info!(
                "ai_coord_register: finish-time repair moved {} from {} onto its live pane row {} \
                 — a mover missed this ordering",
                claude_session_id, from, live.row
            );
        }
    }
}
