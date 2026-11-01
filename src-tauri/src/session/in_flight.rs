//! The ONE session-scope in-flight registry: which Claude session ids a
//! respawn is moving RIGHT NOW, and by whom.
//!
//! Plan `2026-10-20-account-migration-respawn-dies-during-cli-reinstall-churn`
//! Phase 4. Promoted from the resume door's private `IN_FLIGHT` set, which
//! excluded a second resume-door call but nothing else: a resume-door call
//! naming a session that was mid-account-migration passed straight through.
//! Three movers claim here — the account migration, the resume door and the
//! cross-machine respawn receiver — and each refuses an id another holds.
//!
//! The lifecycle poll reads it too ([`hold_for_in_flight_respawn`]). While a
//! respawn is proving its new pane alive (up to 3 minutes, with retries), the
//! session's row is bound to a pane whose child may already have exited; a
//! `poll-dead` close there would destroy the row the retry or the rollback is
//! about to rebind. So for an in-flight session every arm but `KeepAlive`
//! becomes `Skip`, the way a `restore_pending` row is held.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use super::session_lifecycle_store::PollAction;

static IN_FLIGHT: LazyLock<Mutex<HashMap<String, &'static str>>> = LazyLock::new(Default::default);

/// RAII claim on one session id; released on drop, including on a panic or a
/// cancelled future.
#[derive(Debug)]
#[must_use = "dropping the claim releases the session at once"]
pub(crate) struct SessionInFlight(String);

impl SessionInFlight {
    /// Claim `id` for `holder` (a short label: `account-migration`,
    /// `resume-door`, `respawn-receiver`). `Err(current holder)` when another
    /// mover already holds it.
    pub(crate) fn claim(id: &str, holder: &'static str) -> Result<Self, &'static str> {
        let mut g = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(current) = g.get(id) {
            return Err(current);
        }
        g.insert(id.to_string(), holder);
        Ok(Self(id.to_string()))
    }
}

impl Drop for SessionInFlight {
    fn drop(&mut self) {
        IN_FLIGHT
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Is a respawn moving `id` right now?
pub(crate) fn is_in_flight(id: &str) -> bool {
    IN_FLIGHT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains_key(id)
}

/// Rewrite a liveness-poll verdict for a session a respawn is moving: only a
/// confident `KeepAlive` passes; every close (and every tick-accumulating
/// arm) becomes `Skip`.
pub fn hold_for_in_flight_respawn(action: PollAction, in_flight: bool) -> PollAction {
    if in_flight && action != PollAction::KeepAlive {
        PollAction::Skip
    } else {
        action
    }
}

/// The liveness poll's FINAL verdict for one record: the classified action,
/// held for a withheld boot restore / pending PTY adoption (`withheld`), then
/// for an in-flight respawn of `claude_session_id`. The poll calls this, so the
/// in-flight hold cannot be dropped from the poll without reddening its test.
pub fn hold_poll_action(
    classified: PollAction,
    withheld: bool,
    claude_session_id: &str,
) -> PollAction {
    hold_for_in_flight_respawn(
        super::session_lifecycle_store::hold_for_withheld_boot_restore(classified, withheld),
        is_in_flight(claude_session_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round 2, finding 4a: the poll's composed verdict holds a session a
    /// respawn is moving — the wiring `main.rs`'s liveness poll goes through.
    #[test]
    fn the_poll_verdict_holds_an_in_flight_session_and_only_that() {
        let id = "in-flight-test-poll-wiring";
        assert_eq!(
            hold_poll_action(PollAction::Close, false, id),
            PollAction::Close
        );
        let claim = SessionInFlight::claim(id, "respawn-receiver").expect("free");
        assert_eq!(
            hold_poll_action(PollAction::Close, false, id),
            PollAction::Skip
        );
        assert_eq!(
            hold_poll_action(PollAction::KeepAlive, false, id),
            PollAction::KeepAlive
        );
        drop(claim);
        assert_eq!(
            hold_poll_action(PollAction::Close, false, id),
            PollAction::Close
        );
        assert_eq!(
            hold_poll_action(PollAction::CloseNoTerminal, true, id),
            PollAction::Skip,
            "the withheld-restore hold still composes"
        );
    }
    use crate::session::session_lifecycle_store::{classify, WorkerPlane};

    #[test]
    fn a_second_claim_is_refused_with_the_holder_until_the_first_is_released() {
        let id = "in-flight-test-1";
        let first = SessionInFlight::claim(id, "account-migration").expect("free");
        assert!(is_in_flight(id));
        assert_eq!(
            SessionInFlight::claim(id, "resume-door").unwrap_err(),
            "account-migration"
        );
        drop(first);
        assert!(!is_in_flight(id));
        let _again = SessionInFlight::claim(id, "resume-door").expect("released");
    }

    /// A row in the registry survives a poll whose pane is dead: the dead-pty
    /// verdict `classify` reaches is held to `Skip`.
    #[test]
    fn an_in_flight_row_survives_a_poll_whose_pane_is_dead() {
        let dead = classify(
            Some(false),
            false,
            10,
            0,
            true,
            false,
            true,
            WorkerPlane::NotWorker,
        );
        assert_eq!(
            dead,
            PollAction::Close,
            "fixture: the unheld verdict closes"
        );
        assert_eq!(hold_for_in_flight_respawn(dead, true), PollAction::Skip);
        assert_eq!(hold_for_in_flight_respawn(dead, false), PollAction::Close);
        for a in [
            PollAction::NeedsConfirm,
            PollAction::CloseNeverStarted,
            PollAction::NoMatchWait,
            PollAction::CloseNoTerminal,
        ] {
            assert_eq!(
                hold_for_in_flight_respawn(a, true),
                PollAction::Skip,
                "{a:?}"
            );
        }
        assert_eq!(
            hold_for_in_flight_respawn(PollAction::KeepAlive, true),
            PollAction::KeepAlive
        );
    }
}
