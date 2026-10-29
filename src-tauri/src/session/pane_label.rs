//! The ONE composition of a pane's conversation label (plan
//! `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
//! Phase 4 / D3).
//!
//! A pane carries two titles from two different sources:
//!
//! - **The conversation's identity.** This is the lifecycle record's
//!   `claude_session_id` plus its name: the sticky live-registry
//!   `session_name`, else the record's `title`, which only the
//!   provider-registry refresh (`reconcile::refresh_titles`) and operator
//!   renames write.
//! - **The activity title.** This is the OSC-0/2 string the CLI emits for what
//!   it is doing right now (e.g. the description of the last subagent it
//!   launched). It is held on `TerminalSession::activity_title` and is never
//!   persisted into the record.
//!
//! Until D3 one `title` field had both writers, so a current-activity title
//! erased the conversation's identity. On 2026-10-27 that is how the operator
//! failed to recognise a live lead pane and opened a second copy of it. The
//! label composed here leads with the identity: `<name> · <id8>`,
//! `(unnamed) · <id8>` when the provider has no name, and
//! `no conversation bound` when no id is bound.
//!
//! The TypeScript twin is `src/components/terminal/paneLabel.ts`. Both are
//! pinned to the same table,
//! `src/components/terminal/__fixtures__/pane-label.json`.

use crate::session::session_lifecycle_store::TerminalSessionRecord;

/// The name shown when a conversation is bound but the provider has no name
/// for it.
pub const UNNAMED: &str = "(unnamed)";

/// The whole label when no conversation id is bound to the pane.
pub const NO_CONVERSATION_BOUND: &str = "no conversation bound";

/// The separator between the name and the short id.
pub const SEPARATOR: &str = " · ";

/// How many leading characters of the conversation id the label shows.
pub const ID_PREFIX_CHARS: usize = 8;

fn clean(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|t| !t.is_empty())
}

/// The first [`ID_PREFIX_CHARS`] characters of a (trimmed) conversation id.
pub fn id8(claude_session_id: &str) -> String {
    claude_session_id
        .trim()
        .chars()
        .take(ID_PREFIX_CHARS)
        .collect()
}

/// Compose a pane's conversation label.
///
/// - `<name> · <id8>` when both are known;
/// - `(unnamed) · <id8>` when the id is bound but the name is absent or blank;
/// - `no conversation bound` when no id is bound, whatever the name says. A
///   name without an id identifies nothing a resume could act on.
pub fn conversation_label(name: Option<&str>, claude_session_id: Option<&str>) -> String {
    let Some(id) = clean(claude_session_id) else {
        return NO_CONVERSATION_BOUND.to_string();
    };
    let name = clean(name).unwrap_or(UNNAMED);
    format!("{name}{SEPARATOR}{}", id8(id))
}

/// A pane's conversation identity as the HTTP surfaces render it
/// (`/terminal-pages`, `/restart-readiness`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationIdentity {
    /// The bound conversation id, or `None` when the pane has no open record
    /// (a plain shell) or the record carries an empty id.
    pub claude_session_id: Option<String>,
    /// The conversation's name: the record's `session_name` (the provider's
    /// live-registry name, sticky), else its `title`, which only the
    /// provider-registry refresh and operator renames write. `None` when
    /// neither is known.
    pub conversation_name: Option<String>,
    /// [`conversation_label`] over the two fields above.
    pub conversation_label: String,
}

/// The conversation identity of the pane whose open record is `rec`.
pub fn identity_of_record(rec: Option<&TerminalSessionRecord>) -> ConversationIdentity {
    let claude_session_id = rec
        .and_then(|r| clean(Some(r.claude_session_id.as_str())))
        .map(str::to_string);
    let conversation_name = rec
        .and_then(|r| clean(r.session_name.as_deref()).or_else(|| clean(r.title.as_deref())))
        .map(str::to_string);
    let conversation_label =
        conversation_label(conversation_name.as_deref(), claude_session_id.as_deref());
    ConversationIdentity {
        claude_session_id,
        conversation_name,
        conversation_label,
    }
}

/// Normalise an OSC activity title: trimmed, and `None` when blank.
pub fn clean_activity_title(title: Option<&str>) -> Option<String> {
    clean(title).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared table. `paneLabel.test.ts` pins the TypeScript twin to it.
    const FIXTURE: &str =
        include_str!("../../../src/components/terminal/__fixtures__/pane-label.json");

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        case: String,
        name: Option<String>,
        claude_session_id: Option<String>,
        label: String,
    }

    #[derive(serde::Deserialize)]
    struct Fixture {
        cases: Vec<Case>,
    }

    fn cases() -> Vec<Case> {
        serde_json::from_str::<Fixture>(FIXTURE)
            .expect("pane-label.json parses")
            .cases
    }

    #[test]
    fn pane_label_matches_the_shared_fixture_table() {
        let cases = cases();
        // A table that silently parsed to nothing would pass vacuously.
        assert!(cases.len() >= 8, "fixture has {} cases", cases.len());
        for c in &cases {
            assert_eq!(
                conversation_label(c.name.as_deref(), c.claude_session_id.as_deref()),
                c.label,
                "case: {}",
                c.case
            );
        }
    }

    #[test]
    fn pane_label_name_and_id() {
        assert_eq!(
            conversation_label(Some("maxim"), Some("9b40e22d-deec-415c-9379-5f85ae994ad8")),
            "maxim · 9b40e22d"
        );
    }

    #[test]
    fn pane_label_no_name_falls_back_to_unnamed() {
        assert_eq!(
            conversation_label(None, Some("9b40e22d-deec-415c-9379-5f85ae994ad8")),
            "(unnamed) · 9b40e22d"
        );
    }

    #[test]
    fn pane_label_no_id_is_explicit() {
        assert_eq!(
            conversation_label(Some("maxim"), None),
            "no conversation bound"
        );
        assert_eq!(conversation_label(None, None), "no conversation bound");
    }

    #[test]
    fn pane_label_long_name_keeps_the_id_suffix() {
        let long = "x".repeat(500);
        let label = conversation_label(Some(&long), Some("3da13dea-0000"));
        assert!(label.ends_with(" · 3da13dea"), "{label}");
        assert!(label.starts_with(&long));
    }

    #[test]
    fn pane_label_id8_counts_characters_not_bytes() {
        // A non-ASCII id must not panic on a byte boundary.
        assert_eq!(id8("ééééééééé"), "éééééééé");
    }

    fn record(id: &str, title: Option<&str>, session_name: Option<&str>) -> TerminalSessionRecord {
        serde_json::from_value(serde_json::json!({
            "claudeSessionId": id,
            "pageId": "p",
            "zoneIndex": 0,
            "title": title,
            "terminalId": "t",
            "openedAt": 0,
            "lastSeenAt": 0,
            "state": "open",
            "sessionName": session_name,
        }))
        .expect("a minimal record deserializes")
    }

    #[test]
    fn pane_label_identity_of_record() {
        let rec = record("9b40e22d-deec-415c-9379-5f85ae994ad8", Some("maxim"), None);
        let id = identity_of_record(Some(&rec));
        assert_eq!(
            id.claude_session_id.as_deref(),
            Some("9b40e22d-deec-415c-9379-5f85ae994ad8")
        );
        assert_eq!(id.conversation_name.as_deref(), Some("maxim"));
        assert_eq!(id.conversation_label, "maxim · 9b40e22d");

        let untitled = record("9b40e22d-deec", None, None);
        assert_eq!(
            identity_of_record(Some(&untitled)).conversation_label,
            "(unnamed) · 9b40e22d"
        );

        let none = identity_of_record(None);
        assert_eq!(none.claude_session_id, None);
        assert_eq!(none.conversation_name, None);
        assert_eq!(none.conversation_label, "no conversation bound");

        let empty_id = record("", Some("maxim"), None);
        assert_eq!(
            identity_of_record(Some(&empty_id)).conversation_label,
            "no conversation bound"
        );

        // The live registry's name wins over a spawn-time title the 45 s
        // refresh has not converged yet.
        let spawn_titled = record("9b40e22d-deec", Some("Terminal 3"), Some("maxim"));
        assert_eq!(
            identity_of_record(Some(&spawn_titled)).conversation_label,
            "maxim · 9b40e22d"
        );
    }

    #[test]
    fn pane_label_clean_activity_title() {
        assert_eq!(
            clean_activity_title(Some("  Fix it  ")).as_deref(),
            Some("Fix it")
        );
        assert_eq!(clean_activity_title(Some("   ")), None);
        assert_eq!(clean_activity_title(None), None);
    }
}
