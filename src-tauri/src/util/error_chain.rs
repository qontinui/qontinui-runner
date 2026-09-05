//! THE one renderer of an error's full `source()` chain.
//!
//! # Why this is a shared module and not a private helper
//!
//! `Display` on a `reqwest::Error` alone collapses connect/DNS/TLS/OS detail
//! into the generic `error sending request for url (…)`. Every WARN that
//! interpolates a bare `{e}` therefore prints the same opaque line whatever the
//! actual fault was, and the root cause — `os error 10053`, `operation timed
//! out`, a schannel verdict — stays hidden one `source()` hop down.
//!
//! Three separate modules learned that the hard way and each grew its own
//! private copy:
//!
//! * `fleet.rs` — after the 2026-06-03 fleet-heartbeat outage, whose failing
//!   ticks were undiagnosable from the logs by construction.
//! * `agent_worktree::reclaim` — after the reclaim poller failed 100 % of its
//!   pulls for five days (2026-07-28 → 2026-08-01), emitting ~140 identical
//!   cause-free WARNs per day.
//! * `env_agent` — whose copy's own doc comment said *"cloned from
//!   `fleet::error_chain`"*.
//!
//! Three copies of a fix for one defect class is the drift this module exists
//! to stop: the fourth site to need it (the coord-egress proxies) would
//! otherwise have cloned it again. There is now one implementation, and adding
//! a chain to a new call site is an import rather than a copy.
//!
//! # Compiled into BOTH crates, from ONE file
//!
//! The runner is a lib crate (`qontinui_runner_lib`) plus a bin crate that
//! share a source tree. `env_agent` lives in the lib; `fleet`,
//! `agent_worktree` and the coord proxies live in the bin. `util` is declared
//! in `main.rs` and — as an inline `pub mod util { pub mod error_chain; }` —
//! in `lib.rs`, so **this file** is the single source of truth reachable from
//! either crate under the same spelling, `crate::util::error_chain::…`. That
//! is the established pattern here (`coord_doctor`, `process_helpers`), and it
//! is what keeps the promotion from re-introducing a fourth copy by the back
//! door.

/// Render `e` followed by every nested `source()`, joined with `": "`.
///
/// The top-level `Display` is emitted verbatim first, so any consumer already
/// matching on the leading text keeps working — the chain only ever *grows*
/// the human string, it never rewrites its head.
///
/// Allocation: one `String`, grown in place. Safe on a failure path.
pub(crate) fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    use std::fmt::Write as _;
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let _ = write!(out, ": {cause}");
        source = cause.source();
    }
    out
}

/// [`error_chain`] for the tail of the chain only — everything BELOW the
/// top-level `Display`, or an empty string when the error has no source.
///
/// The forensics streams want this rather than the full render: a rotation
/// line already carries the operation and the URL in its own columns, so
/// repeating the generic `error sending request for url (…)` head in `cause`
/// spends the field on the one part that is never informative. What a reader
/// needs there is `os error 10053` / `operation timed out`.
pub(crate) fn error_chain_tail(e: &(dyn std::error::Error + 'static)) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mut source = e.source();
    while let Some(cause) = source {
        if !out.is_empty() {
            out.push_str(": ");
        }
        let _ = write!(out, "{cause}");
        source = cause.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Leaf(&'static str);
    impl std::fmt::Display for Leaf {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Leaf {}

    #[derive(Debug)]
    struct Mid(Leaf);
    impl std::fmt::Display for Mid {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("connection error")
        }
    }
    impl std::error::Error for Mid {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[derive(Debug)]
    struct Top(Mid);
    impl std::fmt::Display for Top {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("error sending request for url (https://coord/mcp)")
        }
    }
    impl std::error::Error for Top {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    /// The whole point: the OS-level leaf reaches the rendered string. A bare
    /// `{e}` stops at the first line and is why an egress incident reads as
    /// "the same opaque WARN, 5,967 times".
    #[test]
    fn error_chain_reaches_the_os_leaf() {
        let rendered = error_chain(&Top(Mid(Leaf(
            "An established connection was aborted by the software in your host machine. (os error 10053)",
        ))));
        assert!(
            rendered.starts_with("error sending request for url"),
            "the top-level Display must stay the HEAD (compat): {rendered}"
        );
        assert!(rendered.contains("connection error"), "{rendered}");
        assert!(rendered.contains("os error 10053"), "{rendered}");
        // Three levels, two joins.
        assert_eq!(rendered.matches(": ").count(), 2, "{rendered}");
    }

    /// A source-less error renders exactly as `Display` did — the promotion
    /// must not change the ordinary line.
    #[test]
    fn error_chain_of_a_leaf_is_just_its_display() {
        assert_eq!(
            error_chain(&Leaf("operation timed out")),
            "operation timed out"
        );
        assert_eq!(error_chain_tail(&Leaf("operation timed out")), "");
    }

    /// The tail drops the uninformative head and keeps everything under it.
    #[test]
    fn error_chain_tail_drops_the_head_only() {
        let tail = error_chain_tail(&Top(Mid(Leaf("os error 10053"))));
        assert_eq!(tail, "connection error: os error 10053");
    }
}
