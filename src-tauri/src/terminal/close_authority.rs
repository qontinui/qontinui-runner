//! The close authority — the one value that lets the graceful-exit primitive
//! type into a live `claude` (plan `2026-10-28-one-session-lifecycle-authority`,
//! Phase 14; architecture node R0a).
//!
//! ## What it gates
//!
//! `TerminalManager::graceful_exit` and `graceful_exit::drive` take an
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
//! ## Who may mint one: nobody, yet
//!
//! The type has NO constructor outside `#[cfg(test)]`, not even a `pub(crate)`
//! one:
//!
//! * its one field is private, so no other module can build one with a struct
//!   literal;
//! * it derives nothing that builds or duplicates a value (`Default`, `Clone`,
//!   `Copy`, `Deserialize`);
//! * the only fn that returns one is `CloseAuthority::for_test`, which is
//!   itself `#[cfg(test)]`.
//!
//! On every build that ships, therefore, every caller of the primitive passes
//! `None` and is refused once a live `claude` is found. The callers are:
//!
//! * the wind-down drain arm and its `finished_close` arm (runner#2426);
//! * the steward stop's `?at=boundary`;
//! * the relay's remote end;
//! * the debug `graceful-exit` door.
//!
//! The refusal holds whatever else changes. A later relaxation of the
//! primitive's child refusal, by this plan or by a peer, cannot let any of
//! those callers close a live session.
//!
//! Phase 93 (architecture R6) adds the one production minter, inside the
//! lifecycle Closer. It mints only from a `decide()` Close whose guard the
//! Closer re-read. Phase 16 adds the relay's operator arm, which mints only for
//! an end carrying the relay's `origin: user_session` stamp. That stamp rests
//! on a Cognito user token, which no device credential can produce.
//!
//! ## What enforces it
//!
//! Rust privacy cannot confine a constructor to one module that is not an
//! ancestor of this one, so the Phase 93 minter will have to be `pub(crate)`.
//! What confines its CALLERS is a source scan in `runner_close_sites.rs`
//! (`close_authority_has_no_constructor_outside_test_code`), whose
//! `AUTHORIZED_MINTERS` list is empty today. The scan is an allowlist:
//!
//! * The type is named only here, on a `use` line, or as the
//!   `authority: Option<CloseAuthority>` parameter of the primitive's four
//!   entry points.
//! * Every call of the primitive passes `None`, except an entry point
//!   forwarding its own parameter.
//! * The type keeps this shape: one named private field, `Debug` as its only
//!   derive, no trait impl, no `const`, `static`, alias or rename of it, and no
//!   non-test fn or struct literal that builds one.
//!
//! So adding a public constructor, an associated const or a forged value is
//! red in `cargo test` itself, not only in a non-test build.
//!
//! ## Consumed by value
//!
//! One authority is one close attempt. It cannot be cloned, so an authority
//! minted for one decision can never be replayed for another pane or a later
//! tick.

/// Proof that the lifecycle Closer decided THIS close — see the module docs.
/// Absent (`None`) on every production call today.
#[derive(Debug)]
#[must_use = "a CloseAuthority authorises nothing unless it is handed to the graceful-exit primitive"]
pub struct CloseAuthority {
    /// Private: no module but this one can construct the type. Never read.
    _sealed: (),
}

impl CloseAuthority {
    /// An authority for the primitive's OWN tests, which exercise the protocol
    /// past the authority check (typing `/exit`, the echo, the gone-probes,
    /// the close). Test-only by construction: no shipped build can call it.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self { _sealed: () }
    }
}
