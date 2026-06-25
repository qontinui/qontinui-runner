//! Session intent — typed declaration of what a session is for, where it
//! runs, and what it expects to touch. Plan
//! [`2026-05-22-coord-native-session-coordination`] §D6.
//!
//! Every call to [`crate::session::Session::start`] requires an `Intent`. The
//! wire shape mirrors `coord.sessions.intent` (JSONB) so the runner-side
//! struct serializes verbatim into the row coord stores.
//!
//! Validation is intentionally light. The plan favors explicit-over-implicit:
//! we reject obvious mistakes (`purpose` blank, paths that don't exist) but
//! we do **not** sanitize free text — the dashboard is the readable surface
//! and it can render what coord stores.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::SessionKind;

/// Minimum length of a session purpose. Matches the dashboard's
/// "what is this session for?" copy — anything shorter usually means
/// the caller forgot to fill it in.
const MIN_PURPOSE_CHARS: usize = 3;

/// Typed declaration of session intent. Plan §D6 spec; the field set is the
/// stable wire contract between runner and coord (`coord.sessions.intent`).
///
/// The `Intent` lives in the `qontinui-runner` binary crate (not the
/// `qontinui_runner_lib` crate that other binaries import) — doctests are
/// therefore plain examples rather than executable docs. See the
/// `tests` module below for the executable round-trip + validation
/// coverage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Intent {
    /// Discriminator. Selects which transport handles the session.
    pub kind: SessionKind,
    /// Human-readable purpose. Surfaces in the dashboard Live Sessions
    /// panel; required, min [`MIN_PURPOSE_CHARS`] chars after trim.
    pub purpose: String,
    /// Optional repo slug. When set together with [`Intent::branch`], the
    /// session auto-acquires a `ClaimKind::RepoBranch` on
    /// `<repo>:<branch>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Optional branch name. See [`Intent::repo`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Optional plan slug this session is working on. Forwarded into
    /// `coord.sessions.intent` so coord can group sessions by plan and
    /// drive plan-scoped multi-repo dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_slug: Option<String>,
    /// Optional correlation topic for cross-machine peer discovery /
    /// rendezvous. Forwarded into `coord.sessions.intent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_topic: Option<String>,
    /// Optional page tab id the terminal pane was placed on. Set ONLY on
    /// the gate-continuation create path; a present `page_id` is the
    /// coord-side marker that the session is a gate continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_id: Option<String>,
    /// Paths the session intends to touch. The plan calls this out as the
    /// hook for fine-grained claim narrowing in later phases; for now the
    /// list is persisted verbatim in the intent JSON.
    #[serde(default)]
    pub declared_paths: Vec<PathBuf>,
    /// Plan §D10 — opt-in PTY output streaming. Off by default; sensitive
    /// sessions stay local. Phase 8 consumes this when wiring PTY bytes
    /// through JetStream.
    #[serde(default)]
    pub share_output: bool,
    /// Plan §D11 — when `Some(true)`, run a regex sweep against PTY output
    /// before fan-out. When `None`, defaults to the value of
    /// [`Intent::share_output`] (see [`Intent::effective_redact_secrets`]).
    #[serde(default)]
    pub redact_secrets: Option<bool>,
    /// Phase 6 (session-restore redesign) — the AI-CLI provider hosting this
    /// session (`"claude"`, `"codex"`, …). Threaded into the `started` outbox
    /// payload as a top-level `provider` key so the drain loop forwards it to
    /// coord's `CreateSessionRequest.provider`, populating
    /// `coord.sessions.provider`. `None` for a plain shell with no AI CLI or a
    /// caller that hasn't resolved a provider — coord tolerates its absence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

impl Intent {
    /// Resolve the effective `redact_secrets` flag. Plan §D11 — defaults to
    /// `share_output` so callers who opt into streaming get redaction
    /// without having to remember the second flag.
    pub fn effective_redact_secrets(&self) -> bool {
        self.redact_secrets.unwrap_or(self.share_output)
    }

    /// Validate the intent. Returns `Ok(())` on success; otherwise the
    /// first failed invariant. Plan §D6 — required at every session
    /// start.
    pub fn validate(&self) -> Result<(), IntentError> {
        let trimmed = self.purpose.trim();
        if trimmed.len() < MIN_PURPOSE_CHARS {
            return Err(IntentError::PurposeTooShort {
                got: trimmed.len(),
                min: MIN_PURPOSE_CHARS,
            });
        }

        // Branch without repo is a structural mismatch — a branch name only
        // makes sense scoped to a repo. Repo without branch is fine (whole
        // repo work).
        if self.branch.is_some() && self.repo.is_none() {
            return Err(IntentError::BranchWithoutRepo);
        }

        for path in &self.declared_paths {
            if path.as_os_str().is_empty() {
                return Err(IntentError::EmptyDeclaredPath);
            }
        }

        Ok(())
    }
}

/// Errors surfaced by [`Intent::validate`]. Stable enum — the Tauri command
/// handler maps each variant to a structured error code so the frontend can
/// branch without parsing strings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntentError {
    #[error("session purpose too short ({got} < {min} chars after trim)")]
    PurposeTooShort { got: usize, min: usize },
    #[error(
        "session intent has branch set but no repo — branch only makes sense scoped to a repo"
    )]
    BranchWithoutRepo,
    #[error("declared_paths contains an empty path")]
    EmptyDeclaredPath,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_intent() -> Intent {
        Intent {
            kind: SessionKind::TerminalShell,
            purpose: "fix the thing".into(),
            repo: None,
            branch: None,
            plan_slug: None,
            correlation_topic: None,
            page_id: None,
            declared_paths: vec![],
            share_output: false,
            redact_secrets: None,
            provider: None,
        }
    }

    #[test]
    fn accepts_minimal_intent() {
        good_intent().validate().unwrap();
    }

    #[test]
    fn rejects_short_purpose() {
        let mut i = good_intent();
        i.purpose = "ab".into();
        assert!(matches!(
            i.validate(),
            Err(IntentError::PurposeTooShort { .. })
        ));
    }

    #[test]
    fn rejects_whitespace_only_purpose() {
        let mut i = good_intent();
        i.purpose = "   ".into();
        assert!(matches!(
            i.validate(),
            Err(IntentError::PurposeTooShort { .. })
        ));
    }

    #[test]
    fn rejects_branch_without_repo() {
        let mut i = good_intent();
        i.branch = Some("main".into());
        assert_eq!(i.validate(), Err(IntentError::BranchWithoutRepo));
    }

    #[test]
    fn accepts_repo_without_branch() {
        let mut i = good_intent();
        i.repo = Some("qontinui-runner".into());
        i.validate().unwrap();
    }

    #[test]
    fn rejects_empty_declared_path() {
        let mut i = good_intent();
        i.declared_paths.push(PathBuf::new());
        assert_eq!(i.validate(), Err(IntentError::EmptyDeclaredPath));
    }

    #[test]
    fn redact_defaults_to_share_output() {
        let mut i = good_intent();
        i.share_output = true;
        assert!(i.effective_redact_secrets());
        i.share_output = false;
        assert!(!i.effective_redact_secrets());
    }

    #[test]
    fn redact_explicit_overrides_default() {
        let mut i = good_intent();
        i.share_output = true;
        i.redact_secrets = Some(false);
        assert!(!i.effective_redact_secrets());
    }

    #[test]
    fn intent_round_trips_through_json() {
        let i = Intent {
            kind: SessionKind::TerminalClaude,
            purpose: "drive the agentic loop".into(),
            repo: Some("qontinui-web".into()),
            branch: Some("main".into()),
            plan_slug: Some("2026-05-22-coord-native-session-coordination".into()),
            correlation_topic: Some("by-correlation-topic/demo".into()),
            page_id: Some("page-2".into()),
            declared_paths: vec![PathBuf::from("/repo/path")],
            share_output: true,
            redact_secrets: Some(false),
            provider: Some("claude".into()),
        };
        let serialized = serde_json::to_string(&i).unwrap();
        let back: Intent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(back, i);
    }

    #[test]
    fn page_id_none_omits_key_and_some_round_trips() {
        // `None` must serialize WITHOUT the key (skip_serializing_if) so
        // existing coord rows / consumers see no shape change.
        let serialized = serde_json::to_string(&good_intent()).unwrap();
        assert!(!serialized.contains("page_id"));

        // A JSON intent carrying `"page_id":"page-2"` round-trips.
        let mut with_page = good_intent();
        with_page.page_id = Some("page-2".into());
        let json = serde_json::to_string(&with_page).unwrap();
        assert!(json.contains("\"page_id\":\"page-2\""));
        let back: Intent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, with_page);
    }
}
