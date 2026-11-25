//! The close authority — the one value that lets the graceful-exit primitive
//! type into a live `claude` (plan `2026-10-28-one-session-lifecycle-authority`,
//! Phase 14; architecture node R0a).
//!
//! ## What it gates
//!
//! `TerminalManager::graceful_exit` and `graceful_exit::drive_graceful_exit` take an
//! `Option<CloseAuthority>`. Once the probe has found a live `claude` in the
//! pane, and before a single byte is typed into it, a call that presents
//! `None` is refused `no_close_authority`
//! (`GracefulExitOutcome::NoCloseAuthority`). The check sits after the probe on
//! purpose:
//!
//! * a pane with NO live `claude` still answers `NoLiveClaude`, untouched, so the
//!   relay's close of a bare shell pane is unchanged;
//! * an unreadable process table still answers `ProbeUnavailable`.
//!
//! Neither of those types anything, so neither needs an authority.
//!
//! ## Who may mint one: the `finished_close` arm, from a guard `Close`
//!
//! The type has ONE production constructor,
//! [`CloseAuthority::for_finished_close`], and ONE production minter, the
//! `finished_close` arm's `wind_down_exit`
//! (`session/wind_down_executor.rs`). The constructor takes a
//! [`GuardClose`](crate::session::finished_close_guard::GuardClose) BY VALUE:
//! the token only `finished_close_guard::judge` can build, which it hands out
//! only when every input served `finished-criteria` names was read fresh for
//! that one session and pane and came back ok (plan
//! `2026-11-16-finished-close-mints-its-close-authority-from-the-served-close-conditions`,
//! Phase 2). Nothing else can hold the token, so nothing else can obtain an
//! authority:
//!
//! * the type's one field is private, so no other module can build one with a
//!   struct literal;
//! * it derives nothing that builds or duplicates a value (`Default`, `Clone`,
//!   `Copy`, `Deserialize`);
//! * the only fns that return one are `for_finished_close` and the
//!   `#[cfg(test)]` `CloseAuthority::for_test`.
//!
//! Every other caller of the primitive passes `None` and is refused once a live
//! `claude` is found:
//!
//! * the wind-down drain arm (it presents no token, whatever the guard says);
//! * the steward stop's `?at=boundary`;
//! * the relay's remote end;
//! * the debug `graceful-exit` door.
//!
//! The refusal holds whatever else changes. A later relaxation of the
//! primitive's child refusal, by this plan or by a peer, cannot let any of
//! those callers close a live session. The authority admits a close ATTEMPT and
//! never forces one: the primitive's own refusals (a nested `claude`, a child
//! process, a screen not at an empty prompt, the echo confirmation, the
//! gone-probes) all still stand.
//!
//! The lifecycle Closer (Phase 93, architecture R6) later replaces the guard's
//! BODY with `decide()`; the token, this constructor and the one minter stay.
//! Phase 16 adds the relay's operator arm, which mints only for an end carrying
//! the relay's `origin: user_session` stamp. That stamp rests on a Cognito user
//! token, which no device credential can produce.
//!
//! ## What enforces it
//!
//! Rust privacy cannot confine a constructor to one module that is not an
//! ancestor of this one, so `for_finished_close` is `pub(crate)`. What confines
//! its CALLERS is a source scan in `runner_close_sites.rs`
//! (`close_authority_has_no_constructor_outside_test_code`), whose
//! `AUTHORIZED_CONSTRUCTORS` and `AUTHORIZED_MINTERS` lists hold exactly
//! `for_finished_close` and `wind_down_exit`. The scan is an allowlist:
//!
//! * The type is named only here, on a `use` line, as the
//!   `authority: Option<CloseAuthority>` parameter of the primitive's four
//!   entry points, or once inside the authorized minter, as the path
//!   `CloseAuthority::for_finished_close`.
//! * Every call of the primitive passes `None`, except an entry point
//!   forwarding its own parameter and the one authorized minter.
//! * The type keeps this shape: one named private field, `Debug` as its only
//!   derive, no trait impl, no `const`, `static`, alias or rename of it, and no
//!   non-test fn or struct literal that builds one outside the one constructor.
//!
//! So adding a second constructor, an associated const, a second mint or a
//! forged value is red in `cargo test` itself, not only in a non-test build.
//!
//! ## Consumed by value
//!
//! One authority is one close attempt. It cannot be cloned, so an authority
//! minted for one decision can never be replayed for another pane or a later
//! tick.

use crate::session::finished_close_guard::GuardClose;

/// Proof that the close guard decided THIS close — see the module docs. Absent
/// (`None`) on every production call but the `finished_close` arm's, and on
/// that arm's whenever the guard did not answer `Close`.
#[derive(Debug)]
#[must_use = "a CloseAuthority authorises nothing unless it is handed to the graceful-exit primitive"]
pub struct CloseAuthority {
    /// Private: no module but this one can construct the type. Never read.
    _sealed: (),
}

impl CloseAuthority {
    /// The one production constructor: an authority for the one close a
    /// [`GuardClose`] was judged for. Consumes the token, so one guard `Close`
    /// buys one authority, and the authority is consumed by the one primitive
    /// call it is handed to. Only `wind_down_exit` may call it (the source
    /// scan's `AUTHORIZED_MINTERS`), and it checks the token's ids against the
    /// close it is about to make first.
    pub(crate) fn for_finished_close(_proof: GuardClose) -> Self {
        Self { _sealed: () }
    }

    /// An authority for the primitive's OWN tests, which exercise the protocol
    /// past the authority check (typing `/exit`, the echo, the gone-probes,
    /// the close). Test-only by construction: no shipped build can call it.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self { _sealed: () }
    }
}
