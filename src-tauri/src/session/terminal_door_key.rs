//! Door-scoped keys derived from a terminal's per-spawn secret — how a loopback
//! request proves WHICH runner terminal it comes from (plan
//! `2026-11-05-transcript-bind-refuses-a-hosted-session-whose-only-key-is-the-workdir-nonce`,
//! Phase 2).
//!
//! ## The evidence
//!
//! Every terminal `TerminalSession::spawn` starts carries a per-spawn secret in
//! its child environment, [`NONCE_ENV`] (`QONTINUI_PERMREQ_NONCE`). It is set
//! AFTER the caller's env, so neither a caller nor an inherited copy can plant
//! a known value; it is recorded as that terminal's grant once the child is up
//! and forgotten when the terminal drops ([`crate::session::permission_accept`]).
//! A sibling session holds ITS OWN terminal's secret, never this one's. That is
//! what a coord-mcp key shared by a whole directory cannot say, and what this
//! module turns into a proof.
//!
//! Only the spawn path records a grant, so an ADOPTED holder pane (and any
//! terminal a previous runner process spawned) holds none: every proof for it
//! fails closed.
//!
//! ## What it does not stop
//!
//! The proof is only as strong as same-user process isolation, exactly like
//! the permission pad's request key it sits beside. The secret lives in the
//! terminal's environment, and a process of the same OS user can read another
//! process's environment (`/proc/<pid>/environ` on Linux). So a door key
//! separates one runner terminal from its siblings and from other users; it
//! does not stand against a hostile process running as the same user, which
//! could read the secret and derive the key itself.
//!
//! ## The derivation
//!
//! ```text
//! terminal_door_key_for(door, nonce)
//!     = lowercase hex SHA-256(LABEL || door || "\0" || nonce)
//!     where LABEL = "qontinui-terminal-door-key\0"
//! ```
//!
//! - **The secret is never sent.** The derivation is one-way, so whoever
//!   receives a door key — a process squatting the runner's port receives it in
//!   the request — cannot produce the secret.
//! - **One door's key opens that door only.** The door name is hashed in, so a
//!   key harvested at one door replays at no other. A door key is in particular
//!   NOT the permission pad's request key ([`request_key_for`], a different
//!   label), so a transcript-bind proof harvested off the wire cannot buy the
//!   pad's nonce echo. A door name is a compile-time constant of the door that
//!   checks it, never request input.
//!
//! ## The coupling, named
//!
//! The client half is `scripts/worktree-handoff.sh` in qontinui-claude-config
//! (the plan's Phase 3): it derives the same key from `$QONTINUI_PERMREQ_NONCE`
//! in its own environment and sends it as `X-Qontinui-Terminal-Proof`. The
//! variable name ([`NONCE_ENV`]), [`LABEL`] and the separator are therefore a
//! cross-repo contract. This module's tests pin the vector below as a literal,
//! and that client's suite pins the same vector in qontinui-claude-config PR
//! #2399, not yet on its main:
//! nonce `0123456789abcdef0123456789abcdef`, door `transcript-bind` →
//! `300c88aed1868f359db36f35beb3b1cec8bd0a32494f57ad8cb28bf38f55b3f7`.
//! A change to any of the three must land in both repos.
//!
//! [`NONCE_ENV`]: crate::session::permission_accept::NONCE_ENV
//! [`request_key_for`]: crate::session::permission_accept::request_key_for

/// The derivation's fixed label: a domain separator, NUL-terminated so no door
/// name can extend it into another label.
pub const LABEL: &[u8] = b"qontinui-terminal-door-key\0";

/// A terminal's door-scoped key for `door`, from its per-spawn `nonce`: the
/// lowercase hex SHA-256 of [`LABEL`] ‖ `door` ‖ `\0` ‖ `nonce`. PURE.
pub fn terminal_door_key_for(door: &str, nonce: &str) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(LABEL);
    h.update(door.as_bytes());
    h.update(b"\0");
    h.update(nonce.as_bytes());
    hex::encode(h.finalize())
}

/// Whether `presented` is `terminal_id`'s door key for `door`, compared in
/// constant time ([`crate::coord_mcp::secret_eq`]).
///
/// `false` when nothing was presented, and `false` for a terminal this runner
/// process holds no grant for — never registered (it did not spawn the
/// terminal: an adopted holder pane, a previous process's terminal), or
/// forgotten (the terminal dropped). A proof is evidence only while the grant
/// it was derived from is live in THIS process.
pub fn terminal_door_key_matches(terminal_id: &str, door: &str, presented: Option<&str>) -> bool {
    let Some(presented) = presented else {
        return false;
    };
    let Some(nonce) = crate::session::permission_accept::terminal_grant_nonce(terminal_id) else {
        return false;
    };
    crate::coord_mcp::secret_eq(
        presented.as_bytes(),
        terminal_door_key_for(door, &nonce).as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::permission_accept::{
        forget_terminal_grant, new_nonce, register_terminal_grant, request_key_for,
    };

    /// The shared vector's nonce — also the one the client suite pins.
    const VECTOR_NONCE: &str = "0123456789abcdef0123456789abcdef";
    /// The door the transcript-bind route checks.
    const VECTOR_DOOR: &str = "transcript-bind";
    /// `terminal_door_key_for(VECTOR_DOOR, VECTOR_NONCE)`, as a LITERAL: a
    /// change to the label, the separator or the hash breaks this test and the
    /// client's together.
    const VECTOR_KEY: &str = "300c88aed1868f359db36f35beb3b1cec8bd0a32494f57ad8cb28bf38f55b3f7";

    /// A terminal id no other test (or test run) shares.
    fn terminal() -> String {
        format!("door-key-test-{}", uuid::Uuid::new_v4())
    }

    /// A grant this test registered for a fresh terminal, forgotten when the
    /// guard drops — on a failing assertion's unwind too, so a red test never
    /// leaves a slot behind in the process-wide grant registry.
    struct Granted {
        terminal: String,
        nonce: String,
    }

    impl Granted {
        fn new() -> Self {
            let (terminal, nonce) = (terminal(), new_nonce());
            register_terminal_grant(&terminal, nonce.clone(), None);
            Granted { terminal, nonce }
        }
    }

    impl Drop for Granted {
        fn drop(&mut self) {
            forget_terminal_grant(&self.terminal, &self.nonce);
        }
    }

    #[test]
    fn the_shared_vector_is_pinned_as_a_literal() {
        assert_eq!(terminal_door_key_for(VECTOR_DOOR, VECTOR_NONCE), VECTOR_KEY);
    }

    /// A door key is not the permission pad's request key for the same nonce,
    /// so a proof harvested at this door cannot be replayed at the pad. The
    /// request key is pinned too, so this cannot pass by both drifting.
    #[test]
    fn the_door_key_is_not_the_pads_request_key() {
        let request_key = request_key_for(VECTOR_NONCE);
        assert_eq!(
            request_key,
            "8693995c372233286aef708167a1c890d0600f4825d05ded275cb92e54a91ea9"
        );
        assert_ne!(
            terminal_door_key_for(VECTOR_DOOR, VECTOR_NONCE),
            request_key
        );
        // And one door's key is not another's.
        assert_ne!(
            terminal_door_key_for(VECTOR_DOOR, VECTOR_NONCE),
            terminal_door_key_for("another-door", VECTOR_NONCE)
        );
    }

    /// The positive control the refusals below are measured against: the
    /// terminal's own key for the door matches — and only for that door, and
    /// only while presented.
    #[test]
    fn the_terminals_own_door_key_matches() {
        let g = Granted::new();
        let key = terminal_door_key_for(VECTOR_DOOR, &g.nonce);
        assert!(terminal_door_key_matches(
            &g.terminal,
            VECTOR_DOOR,
            Some(&key)
        ));
        assert!(!terminal_door_key_matches(
            &g.terminal,
            "another-door",
            Some(&key)
        ));
        assert!(!terminal_door_key_matches(&g.terminal, VECTOR_DOOR, None));
        // The pad's request key is not a door key.
        assert!(!terminal_door_key_matches(
            &g.terminal,
            VECTOR_DOOR,
            Some(&request_key_for(&g.nonce))
        ));
        // The raw secret is not a door key either.
        assert!(!terminal_door_key_matches(
            &g.terminal,
            VECTOR_DOOR,
            Some(&g.nonce)
        ));
    }

    /// A terminal this process never spawned, or one whose grant was
    /// forgotten when it dropped, proves nothing — even with the key its
    /// nonce WOULD derive.
    #[test]
    fn a_forgotten_or_never_registered_terminal_is_not_proven() {
        let never = terminal();
        let key = terminal_door_key_for(VECTOR_DOOR, &new_nonce());
        assert!(!terminal_door_key_matches(&never, VECTOR_DOOR, Some(&key)));

        let forgotten = Granted::new();
        let (term, key) = (
            forgotten.terminal.clone(),
            terminal_door_key_for(VECTOR_DOOR, &forgotten.nonce),
        );
        assert!(
            terminal_door_key_matches(&term, VECTOR_DOOR, Some(&key)),
            "the control: registered, it matches"
        );
        drop(forgotten);
        assert!(!terminal_door_key_matches(&term, VECTOR_DOOR, Some(&key)));
    }

    /// Terminal B's door key does not prove terminal A, though both are live
    /// grants of this process: the sibling case the transcript-bind door turns
    /// on.
    #[test]
    fn another_terminals_key_does_not_match() {
        let (a, b) = (Granted::new(), Granted::new());
        let key_b = terminal_door_key_for(VECTOR_DOOR, &b.nonce);
        assert!(!terminal_door_key_matches(
            &a.terminal,
            VECTOR_DOOR,
            Some(&key_b)
        ));
        assert!(
            terminal_door_key_matches(&b.terminal, VECTOR_DOOR, Some(&key_b)),
            "the control: it does prove its own terminal"
        );
    }
}
