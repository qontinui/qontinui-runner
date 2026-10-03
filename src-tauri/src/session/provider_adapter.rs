//! Provider-agnostic session adapter contract (plan
//! `2026-06-25-runner-session-restore-redesign.md` §4).
//!
//! The runner's session-restore CORE is provider-agnostic: the lifecycle
//! store, the terminal↔zone↔page model, the per-PTY `QONTINUI_TERMINAL_ID`
//! correlation, the boot-restore orchestration, the reconcile backstop, and
//! the local registration endpoint all work the same regardless of which AI
//! CLI hosts the session. The runner is not a Claude client.
//!
//! ## Data vs behaviour
//!
//! A provider's FACTS — its resume argv, account-isolation variable, restore
//! tier, how it reveals its session id — are data in its
//! [`qontinui_runner_lib::cli_profile`] profile, looked up with
//! `cli_profile::profile_for(provider)`. An unknown provider has no profile,
//! so it has no resume either: nothing here degrades it to Claude.
//!
//! [`SessionProviderAdapter`] keeps only the BEHAVIOUR that cannot be data
//! (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phases 4 and 6): recovering the id of a session whose CLI mints its own
//! ([`IdentitySource::ReadBack`]). A [`IdentitySource::Pinned`] provider
//! (Claude) needs no adapter — the runner chooses the id, passes it on the
//! launch argv and records it at spawn.

use std::sync::Arc;

use qontinui_runner_lib::cli_profile::codex;
use qontinui_types::cli_session::{CliProfile, IdentitySource, RestoreTier as ProfileRestoreTier};

use crate::session::codex_capture;
use crate::session::session_lifecycle_store::SessionLifecycleStore;

/// Declared restore capability of a provider (plan §4 `restore_tier`). Drives
/// the honest-UX surface in Phase 5: `Full` adapters restore the conversation;
/// `TerminalOnly` adapters restore only terminal+cwd+launch-command with a
/// clear "fresh conversation" note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreTier {
    /// The provider can deterministically resume the FULL conversation by id
    /// (`--resume <id>`). Restore brings the chat back.
    Full,
    /// The provider can only re-open the terminal at the right cwd/launch
    /// command — no conversation resume. Restore is honest about the loss.
    TerminalOnly,
}

/// The two spellings of the SAME "restorable-at" concept, and the one place
/// that converts between them (plan `2026-08-23-single-source-derived-facts`,
/// item 1 step 4 — the vocabulary fork).
///
/// - The **coord wire** spelling (`"full"` / `"terminal_only"`, underscore) is
///   what the `restore-record` session event carries in `restore_tier`
///   (`restore_record_emitter`) and what a peer's `handoff` parses back. It is
///   stored verbatim in `coord.session_events`, so it is a binding contract and
///   does not change.
/// - The **frontend** spelling (`"full"` / `"terminal-only"`, hyphen) is the TS
///   `RestoreTier` union in `src/components/terminal/providerAdapter.ts`, and the
///   `"terminal-only"` arm of `classifyRestoreAction`'s `RestoreAction`.
///
/// In production Rust code the RESTORE-TIER wire vocabulary is written only
/// here: the emitter's `TIER_FULL` / `TIER_TERMINAL_ONLY` are const-derived
/// from [`RestoreTier::wire_str`], and `handoff` parses a peer's payload with
/// [`RestoreTier::from_wire_str`] — so the underscore/hyphen fork is an
/// explicit conversion at a named seam rather than two literals that happen to
/// disagree. (Doc comments and test fixtures still quote the literals; the
/// lifecycle store's `"terminal-only"` is the outcome vocabulary below, a
/// different concept that happens to share the frontend spelling.) The
/// cross-seam fixture
/// (`src/components/terminal/__fixtures__/restore-tier-crossproduct.json`)
/// carries both spellings per row and both test suites read it.
///
/// NOT the same concept as the lifecycle store's `RESTORE_TIER_RESUMED` /
/// `RESTORE_TIER_TERMINAL_ONLY` / `RESTORE_TIER_FAILED`: those record what
/// HAPPENED when a restore was attempted, not what a record is restorable AT.
impl RestoreTier {
    /// The coord wire spelling (`restore-record` event `restore_tier`).
    pub const fn wire_str(self) -> &'static str {
        match self {
            RestoreTier::Full => "full",
            RestoreTier::TerminalOnly => "terminal_only",
        }
    }

    /// Parse the coord wire spelling. `None` for anything else — including the
    /// frontend's hyphenated `"terminal-only"`, which is a different vocabulary
    /// and must be converted with [`RestoreTier::from_frontend_str`].
    pub fn from_wire_str(s: &str) -> Option<Self> {
        match s {
            "full" => Some(RestoreTier::Full),
            "terminal_only" => Some(RestoreTier::TerminalOnly),
            _ => None,
        }
    }

    /// The frontend spelling (TS `RestoreTier` in `providerAdapter.ts`).
    pub const fn frontend_str(self) -> &'static str {
        match self {
            RestoreTier::Full => "full",
            RestoreTier::TerminalOnly => "terminal-only",
        }
    }

    /// Parse the frontend spelling. `None` for anything else — including the
    /// wire's underscored `"terminal_only"`.
    pub fn from_frontend_str(s: &str) -> Option<Self> {
        match s {
            "full" => Some(RestoreTier::Full),
            "terminal-only" => Some(RestoreTier::TerminalOnly),
            _ => None,
        }
    }
}

/// A profile's declared tier in this module's vocabulary. The profile type
/// (`qontinui_types::cli_session::RestoreTier`) is the served wire DTO; this
/// enum is the one place the coord-wire and frontend spellings convert.
impl From<ProfileRestoreTier> for RestoreTier {
    fn from(tier: ProfileRestoreTier) -> Self {
        match tier {
            ProfileRestoreTier::Full => RestoreTier::Full,
            ProfileRestoreTier::TerminalOnly => RestoreTier::TerminalOnly,
        }
    }
}

/// Called with the captured session id and the account dir the store recorded
/// for it (`""` when none), once a read-back capture has recorded its session.
pub type OnRecorded = Box<dyn FnOnce(&str, &str) + Send>;

/// One provider's session-management BEHAVIOUR. Implemented once per
/// read-back capture mechanism; its facts are profile data — see the module
/// docs.
pub trait SessionProviderAdapter: Send + Sync {
    /// The provider id this adapter handles. Matches the stored
    /// [`crate::session::session_lifecycle_store::TerminalSessionRecord::provider`].
    fn provider(&self) -> &'static str;

    /// Start recovering the id of the session that `start` reports just began,
    /// recording it in `store` once found and then calling `on_recorded`.
    /// Returns at once; the capture runs on the async runtime and fails open.
    fn spawn_read_back(
        &self,
        store: Arc<SessionLifecycleStore>,
        start: codex_capture::CaptureStart,
        on_recorded: OnRecorded,
    );
}

/// The Codex CLI adapter: reads the self-minted session id back out of the
/// rollout file whose `session_meta.cwd` matches the terminal's
/// ([`codex_capture`], ported from qontinui-runner PR #651).
pub struct CodexAdapter;

impl SessionProviderAdapter for CodexAdapter {
    fn provider(&self) -> &'static str {
        codex::ID
    }

    fn spawn_read_back(
        &self,
        store: Arc<SessionLifecycleStore>,
        start: codex_capture::CaptureStart,
        on_recorded: OnRecorded,
    ) {
        tokio::spawn(codex_capture::capture_and_record_by_cwd(
            store,
            start,
            codex_capture::CAPTURE_POLL_INTERVAL,
            codex_capture::CAPTURE_TIMEOUT,
            on_recorded,
        ));
    }
}

/// The adapter that recovers `profile`'s session ids, or `None` when its
/// identity is not read back (a pinned id needs no recovery) or names a
/// capture mechanism the runner does not implement.
pub fn read_back_adapter(profile: &CliProfile) -> Option<&'static dyn SessionProviderAdapter> {
    match &profile.identity {
        IdentitySource::ReadBack { capture } if capture == codex::CAPTURE_SESSION_FILE => {
            Some(&CodexAdapter)
        }
        IdentitySource::ReadBack { .. } | IdentitySource::Pinned { .. } | IdentitySource::Unknown => {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_tier_spellings_round_trip_and_do_not_cross() {
        for tier in [RestoreTier::Full, RestoreTier::TerminalOnly] {
            assert_eq!(RestoreTier::from_wire_str(tier.wire_str()), Some(tier));
            assert_eq!(
                RestoreTier::from_frontend_str(tier.frontend_str()),
                Some(tier)
            );
        }
        // The pinned literals: the wire value is a stored coord contract, the
        // frontend value is the TS union — a change to either is a contract
        // change, not a refactor.
        assert_eq!(RestoreTier::TerminalOnly.wire_str(), "terminal_only");
        assert_eq!(RestoreTier::TerminalOnly.frontend_str(), "terminal-only");
        assert_eq!(RestoreTier::Full.wire_str(), "full");
        assert_eq!(RestoreTier::Full.frontend_str(), "full");
        // The two vocabularies do not silently accept each other's spelling —
        // that acceptance is exactly the unconverted seam this API replaces.
        assert_eq!(RestoreTier::from_wire_str("terminal-only"), None);
        assert_eq!(RestoreTier::from_frontend_str("terminal_only"), None);
        assert_eq!(RestoreTier::from_wire_str(""), None);
        assert_eq!(RestoreTier::from_wire_str("FULL"), None);
    }

    #[test]
    fn profile_tiers_convert_into_this_vocabulary() {
        assert_eq!(
            RestoreTier::from(ProfileRestoreTier::Full),
            RestoreTier::Full
        );
        assert_eq!(
            RestoreTier::from(ProfileRestoreTier::TerminalOnly),
            RestoreTier::TerminalOnly
        );
    }

    #[test]
    fn only_a_read_back_profile_with_a_known_capture_has_an_adapter() {
        use qontinui_runner_lib::cli_profile::{self, claude};

        let codex_profile = cli_profile::profile_for(codex::ID).unwrap();
        let adapter = read_back_adapter(codex_profile).expect("codex reads its id back");
        assert_eq!(adapter.provider(), codex::ID);

        // A pinned id needs no recovery.
        assert!(read_back_adapter(cli_profile::profile_for(claude::ID).unwrap()).is_none());

        // A read-back mechanism the runner does not implement has no adapter,
        // rather than borrowing Codex's.
        let mut other = codex_profile.clone();
        other.identity = IdentitySource::ReadBack {
            capture: "some_other_file".to_string(),
        };
        assert!(read_back_adapter(&other).is_none());
        other.identity = IdentitySource::Unknown;
        assert!(read_back_adapter(&other).is_none());
    }
}
