//! The TYPED close-reason set: every reason production writes when it closes a
//! lifecycle row, with its wire string (plan
//! `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`, D2 and
//! Phase 3).
//!
//! Production closes a row in three shapes, and all three take this enum:
//!
//! - the `record_close` / `record_close_checked` / `record_close_gated` calls
//!   (the webview's tab close and PTY exit through `terminal_session_record_close`,
//!   the liveness poll's closes, the orchestration worker's teardown);
//! - the direct eviction and repair writes in the lifecycle store
//!   ([`CloseReason::Superseded`], [`CloseReason::SupersededTerminalReuse`]);
//! - the reason `close_reason_for_dead_shell` hands the poll, and the
//!   `launch-failed` rule that may rewrite a death close
//!   ([`crate::terminal::launch_failure`]) before either writer records it.
//!
//! A source scan of `record_close*` literals sees only the first shape, which is
//! why the set is a type rather than a table. The stored wire strings are
//! unchanged, so a row written before the enum existed still reads, and a stored
//! string outside the set (a row from an older build) parses to `None` — never
//! to a guess.
//!
//! Whether a close was DELIBERATE is [`CloseReason::is_deliberate`], an
//! exhaustive `match`: a new reason cannot compile until it is classified. Today
//! `explicit` (any webview tab close, the operator's click or a webview
//! automation) is the only deliberate reason. Once a runtime close door records
//! who closed a row (lifecycle-authority Phase 15, runner#2714 — not on `main`
//! when this was written), the ledger should read the door's class instead.

use std::fmt;
use std::str::FromStr;

/// Why a lifecycle row was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseReason {
    /// The webview closed the tab: the operator's click, or a webview
    /// automation. The data does not say which.
    Explicit,
    /// The PTY exited: a crash, `DELETE /terminals/{id}`, or the shell ending.
    /// One string for all of them (finding `9d26f909`). Restorable.
    PtyExit,
    /// The liveness poll found the shell alive with no `claude` in it for the
    /// whole debounce. Restorable.
    PollDead,
    /// A death close (`poll-dead` or `pty-exit`) of a pane whose Claude CLI
    /// never launched, rewritten on positive evidence that nothing ran there
    /// ([`crate::terminal::launch_failure`]). Restorable exactly like
    /// `poll-dead`: the conversation is as it was.
    LaunchFailed,
    /// The poll matched the row to no terminal for several ticks: an orphan.
    NoTerminal,
    /// A provisional row whose provider never started (a bare shell).
    NeverStarted,
    /// A bare shell whose restore already landed terminal-only went idle
    /// ([`crate::session::session_lifecycle_store::close_reason_for_dead_shell`]).
    TerminalOnlyIdle,
    /// An unconfirmed phantom row evicted when an authoritative session bound
    /// its terminal.
    Superseded,
    /// A prior confirmed row retired when a new confirmed session bound its
    /// reused terminal.
    SupersededTerminalReuse,
    /// An orchestration worker torn down because its dispatch could not bind it
    /// to its subtask row.
    WorkerNotBound,
}

impl CloseReason {
    /// Every reason, once. A test pins that each wire string round-trips.
    pub const ALL: [Self; 10] = [
        Self::Explicit,
        Self::PtyExit,
        Self::PollDead,
        Self::LaunchFailed,
        Self::NoTerminal,
        Self::NeverStarted,
        Self::TerminalOnlyIdle,
        Self::Superseded,
        Self::SupersededTerminalReuse,
        Self::WorkerNotBound,
    ];

    /// The wire string stored in a row's `close_reason`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::PtyExit => "pty-exit",
            Self::PollDead => "poll-dead",
            Self::LaunchFailed => "launch-failed",
            Self::NoTerminal => "no-terminal",
            Self::NeverStarted => "never-started",
            Self::TerminalOnlyIdle => "terminal-only-idle",
            Self::Superseded => "superseded",
            Self::SupersededTerminalReuse => "superseded-terminal-reuse",
            Self::WorkerNotBound => "orchestration dispatch could not bind the worker",
        }
    }

    /// The reason a stored wire string names, or `None` for a string outside
    /// the set (a row written by an older build).
    pub fn from_wire(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// Was the session closed ON PURPOSE? Exhaustive, so a new reason cannot
    /// compile unclassified. Only `explicit` is: every other reason is the
    /// runner retiring a row, or an exit whose intent the string cannot carry
    /// (a `pty-exit` from `DELETE /terminals/{id}` and one from a crash are the
    /// same string). `launch-failed` is a death whose CLI never ran, restorable
    /// like `poll-dead`, so it is not deliberate either.
    pub const fn is_deliberate(self) -> bool {
        match self {
            Self::Explicit => true,
            Self::PtyExit
            | Self::PollDead
            | Self::LaunchFailed
            | Self::NoTerminal
            | Self::NeverStarted
            | Self::TerminalOnlyIdle
            | Self::Superseded
            | Self::SupersededTerminalReuse
            | Self::WorkerNotBound => false,
        }
    }

    /// Is a stored wire string a deliberate close? A string outside the set is
    /// not: nothing says it was.
    pub fn is_deliberate_wire(s: &str) -> bool {
        Self::from_wire(s).is_some_and(Self::is_deliberate)
    }
}

impl fmt::Display for CloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A close reason outside [`CloseReason::ALL`] — what the IPC door
/// (`terminal_session_record_close`) refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCloseReason(pub String);

impl fmt::Display for UnknownCloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let known: Vec<&str> = CloseReason::ALL.iter().map(|r| r.as_str()).collect();
        write!(
            f,
            "unknown close reason {:?} (known: {})",
            self.0,
            known.join(", ")
        )
    }
}

impl std::error::Error for UnknownCloseReason {}

impl FromStr for CloseReason {
    type Err = UnknownCloseReason;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_wire(s).ok_or_else(|| UnknownCloseReason(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every wire string round-trips, and no two reasons share one. Mutation:
    /// a variant given another's string (`PollDead => "pty-exit"`).
    #[test]
    fn every_close_reason_wire_string_round_trips_through_the_typed_set() {
        for reason in CloseReason::ALL {
            assert_eq!(CloseReason::from_wire(reason.as_str()), Some(reason));
            assert_eq!(reason.as_str().parse::<CloseReason>(), Ok(reason));
            assert_eq!(reason.to_string(), reason.as_str());
        }
        let mut wire: Vec<&str> = CloseReason::ALL.iter().map(|r| r.as_str()).collect();
        wire.sort_unstable();
        wire.dedup();
        assert_eq!(wire.len(), CloseReason::ALL.len(), "one string per reason");
    }

    /// The stored strings are unchanged, so rows written before the enum read
    /// the same.
    #[test]
    fn the_wire_strings_are_the_ones_rows_already_carry() {
        let wire: Vec<&str> = CloseReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            wire,
            vec![
                "explicit",
                "pty-exit",
                "poll-dead",
                "launch-failed",
                "no-terminal",
                "never-started",
                "terminal-only-idle",
                "superseded",
                "superseded-terminal-reuse",
                "orchestration dispatch could not bind the worker",
            ]
        );
    }

    /// A string outside the set is refused, never guessed — including the dead
    /// `user-close` key nothing ever wrote. Mutation: `from_wire` falling back
    /// to `Explicit`.
    #[test]
    fn a_string_outside_the_set_is_refused() {
        for s in [
            "user-close",
            "migrated",
            "launch_failed",
            "",
            "Explicit",
            "explicit ",
        ] {
            assert_eq!(CloseReason::from_wire(s), None, "{s:?}");
            let err = s.parse::<CloseReason>().unwrap_err();
            assert_eq!(err, UnknownCloseReason(s.to_string()));
            assert!(
                err.to_string().contains("explicit"),
                "names the known set: {err}"
            );
            assert!(!CloseReason::is_deliberate_wire(s));
        }
    }

    /// Only `explicit` is deliberate today. Mutations: `PtyExit` classified
    /// deliberate; `LaunchFailed` classified deliberate.
    #[test]
    fn only_an_explicit_close_is_deliberate() {
        let deliberate: Vec<CloseReason> = CloseReason::ALL
            .into_iter()
            .filter(|r| r.is_deliberate())
            .collect();
        assert_eq!(deliberate, vec![CloseReason::Explicit]);
        assert!(CloseReason::is_deliberate_wire("explicit"));
        assert!(!CloseReason::is_deliberate_wire("pty-exit"));
        assert!(!CloseReason::is_deliberate_wire("launch-failed"));
    }
}
