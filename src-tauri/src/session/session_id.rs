//! The ONE Rust-side answer to "is this string a usable provider session id?"
//!
//! Plan `2026-08-23-single-source-derived-facts`, item 1 step 1 (the ingress).
//!
//! # Why this module exists at all
//!
//! The frontend has owned this predicate since it was written, as
//! `isValidSessionId` in `src/components/terminal/useTerminalInitialization.ts`:
//!
//! ```text
//! /** Validate session IDs before interpolating into shell commands. */
//! const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;
//! ```
//!
//! It is the gate that stops a session id reaching a shell command line, and the
//! TS classifier applies it twice — once when classifying a restore record and
//! again at drain time, immediately before typing `--resume <id>`.
//!
//! **Rust had no equivalent anywhere.** `POST /control/session-open` — the
//! always-on identity shim's confirmation hook, and one of only two ingresses a
//! non-UUID-shaped id can arrive through — validated `session_id` for
//! `trim().is_empty()` and nothing else, then wrote it straight into the durable
//! lifecycle store as `origin: "authoritative"`. The record is junk the moment it
//! lands: permanently unrestorable, inflating the local session census and
//! `restore-health` with a row no operator action can clear, and mirrored onward
//! to peers as a confirmed authoritative row.
//!
//! This module is deliberately its own file rather than a private helper beside
//! the route. The plan this implements is *about* facts that get re-derived in a
//! second place because the first place was inconvenient to reach; adding a
//! sixth private copy of a session-id predicate to close a defect caused by
//! duplicate predicates would be self-defeating. Item 1 step 2 gives the restore
//! record emitter the same gate by calling [`is_valid_session_id`] — not by
//! writing the charset out again.
//!
//! # Deliberately STRICTER than a literal regex port
//!
//! JavaScript's `$` matches before a trailing newline when the `m` flag is
//! absent, so `"abc\n"` satisfies `/^[a-zA-Z0-9_-]+$/` in the frontend. That is
//! an accident of the regex dialect, not an intended affordance: a trailing
//! newline in a value destined for a shell command line is exactly the shape the
//! gate exists to refuse. This implementation is a whole-string character test,
//! so it rejects the newline. Diverging toward MORE refusal is safe — the
//! frontend re-checks every id it is about to type, so nothing this function
//! rejects could have been used downstream anyway.

/// True when `id` is safe to carry as a provider session id.
///
/// Non-empty, and every character is ASCII alphanumeric, `_` or `-`. Mirrors the
/// frontend's `isValidSessionId` (see the module docs for the one deliberate
/// divergence).
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::is_valid_session_id;

    #[test]
    fn accepts_the_shapes_the_runner_actually_mints() {
        // A uuid from the process-tree lift.
        assert!(is_valid_session_id("3f2b1c9d-4e5a-4b6c-8d7e-9f0a1b2c3d4e"));
        // A transcript stem, and the pinned-id spellings.
        assert!(is_valid_session_id("abc123"));
        assert!(is_valid_session_id("session_42"));
        assert!(is_valid_session_id("A-B_c-9"));
        // Single character is still an id.
        assert!(is_valid_session_id("a"));
    }

    #[test]
    fn refuses_every_shell_metacharacter_that_motivated_the_gate() {
        for bad in [
            "abc; rm -rf /",
            "abc&&whoami",
            "abc|tee /tmp/x",
            "$(id)",
            "`id`",
            "abc>out",
            "abc<in",
            "a b",
            "abc'quote",
            "abc\"quote",
            "abc\\esc",
            "abc\nnewline",
            "abc\ttab",
            "../../etc/passwd",
            "abc/def",
            "abc:def",
            "abc.def",
        ] {
            assert!(
                !is_valid_session_id(bad),
                "expected refusal for {bad:?}, which reaches a shell command line"
            );
        }
    }

    /// The trailing-newline case the JS regex lets through. Documented as a
    /// deliberate divergence in the module docs — pinned here so a later
    /// "port the regex faithfully" change has to argue with a red test.
    #[test]
    fn refuses_a_trailing_newline_the_js_regex_would_accept() {
        assert!(!is_valid_session_id("abc\n"));
    }

    #[test]
    fn refuses_empty_and_whitespace_only() {
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id(" "));
        assert!(!is_valid_session_id("\t"));
    }

    /// Negative control: without this, an implementation that returned `false`
    /// unconditionally would pass every refusal test above.
    #[test]
    fn is_not_vacuously_false() {
        assert!(is_valid_session_id("valid-id_1"));
    }
}
