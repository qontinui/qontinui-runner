//! Claude Code process-topology env markers, and the strip rule for them.
//!
//! Claude Code sets a small number of env vars that describe a process's place
//! in a session tree. They are inherited by the entire process tree and nothing
//! clears them, so a long-lived daemon that happens to have been launched from
//! a Claude Code session passes them down forever — and every `claude` it
//! eventually spawns claims to be a nested session of something that exited
//! months ago.
//!
//! The rule, per `qontinui-runner/CLAUDE.md` "Claude CLI Spawning": **every
//! spawn site strips every marker in [`INHERITED_SESSION_MARKERS`]**. This
//! module exists so the spawn-site strips, the startup warning
//! ([`inherited_session_markers`]) and the `coord doctor` check share one
//! spelling of each name — a typo in any one of them is silent, removing
//! nothing while appearing to.
//!
//! ## The markers
//!
//! - [`CLAUDECODE_ENV`] — "you are running inside Claude Code". Long-standing;
//!   stripped at eleven spawn sites before this module existed.
//! - [`CLAUDE_CHILD_SESSION_ENV`] — "you are a child/nested session". Added by
//!   plan `2026-07-28-runner-transcript-persistence-env-leak`, which found it
//!   leaking into every fleet session.
//!
//! ## What the child-session marker does NOT do
//!
//! It was believed to disable transcript persistence. Vetting on 2026-08-03
//! **refuted** that by direct observation: a session with the marker set was
//! writing its JSONL transcript incrementally. So this strip is env hygiene —
//! removing a lie about process topology that the CLI is entitled to act on
//! however it likes — and NOT a fix for lost transcripts. Do not re-justify it
//! as data recovery; see §0 of that plan.

/// "You are running inside Claude Code."
pub const CLAUDECODE_ENV: &str = "CLAUDECODE";

/// "You are a child/nested Claude Code session."
pub const CLAUDE_CHILD_SESSION_ENV: &str = "CLAUDE_CODE_CHILD_SESSION";

/// Every marker a spawn site must strip, and the startup check must report.
///
/// Adding a marker here does NOT automatically strip it at spawn sites — those
/// call `env_remove` directly on their own `Command`/`CommandBuilder` types,
/// which share no trait. This slice is the canonical list the startup warning
/// and `coord doctor` iterate; keep new spawn-site strips in step with it.
pub const INHERITED_SESSION_MARKERS: &[&str] = &[CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV];

/// Which of [`INHERITED_SESSION_MARKERS`] this process inherited, in order.
///
/// A marker counts as inherited whenever it is **present**, including when set
/// to the empty string — `env_remove` is what clears one, not assigning `""`.
pub fn inherited_session_markers() -> Vec<&'static str> {
    INHERITED_SESSION_MARKERS
        .iter()
        .copied()
        .filter(|name| std::env::var_os(name).is_some())
        .collect()
}

/// One-line, operator-readable summary of an inherited-marker set.
///
/// Kept next to the detection so the runner startup warning and the
/// `coord doctor` check render the same sentence.
pub fn inherited_markers_detail(markers: &[&str]) -> String {
    format!(
        "inherited Claude Code session marker(s): {} — this process is mislabelled as a nested \
         session. They are stripped from every terminal/CLI spawn, so panes are unaffected; the \
         markers reach this process from whatever launched it (usually the supervisor, which \
         inherits them from a Claude Code session).",
        markers.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_names_are_the_cli_spellings() {
        // A typo here is silent: a spawn would keep leaking the real marker
        // while `env_remove` deleted a variable nobody sets.
        assert_eq!(CLAUDECODE_ENV, "CLAUDECODE");
        assert_eq!(CLAUDE_CHILD_SESSION_ENV, "CLAUDE_CODE_CHILD_SESSION");
    }

    #[test]
    fn every_marker_is_listed_for_detection() {
        assert!(INHERITED_SESSION_MARKERS.contains(&CLAUDECODE_ENV));
        assert!(INHERITED_SESSION_MARKERS.contains(&CLAUDE_CHILD_SESSION_ENV));
        assert_eq!(INHERITED_SESSION_MARKERS.len(), 2);
    }

    #[test]
    fn detection_reports_present_markers_including_empty_values() {
        // `std::env` is process-global — hold the shared lock and restore on
        // drop so a sibling test can neither race this nor inherit its writes.
        let _g = crate::test_env::env_lock();
        let _restore =
            crate::test_env::EnvVarRestore::capture(&[CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV]);

        for name in INHERITED_SESSION_MARKERS {
            std::env::remove_var(name);
        }
        assert!(
            inherited_session_markers().is_empty(),
            "no markers set → nothing inherited"
        );

        std::env::set_var(CLAUDE_CHILD_SESSION_ENV, "1");
        assert_eq!(
            inherited_session_markers(),
            vec![CLAUDE_CHILD_SESSION_ENV],
            "a set marker is detected"
        );

        // Empty-but-set is still inherited — this is the case a naive
        // `var() == Ok(non_empty)` check would wrongly clear.
        std::env::set_var(CLAUDECODE_ENV, "");
        assert_eq!(
            inherited_session_markers(),
            vec![CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV],
            "empty-but-set counts, and order follows INHERITED_SESSION_MARKERS"
        );
    }

    #[test]
    fn detail_names_every_marker_it_was_given() {
        let detail = inherited_markers_detail(&[CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV]);
        assert!(detail.contains(CLAUDECODE_ENV));
        assert!(detail.contains(CLAUDE_CHILD_SESSION_ENV));
    }
}
