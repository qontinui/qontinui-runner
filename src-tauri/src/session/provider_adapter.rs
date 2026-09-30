//! Provider-agnostic session adapter contract (plan
//! `2026-06-25-runner-session-restore-redesign.md` §4).
//!
//! The runner's session-restore CORE is provider-agnostic: the lifecycle
//! store, the terminal↔zone↔page model, the per-PTY `QONTINUI_TERMINAL_ID`
//! correlation, the boot-restore orchestration, the reconcile backstop, and
//! the local registration endpoint all work the same regardless of which AI
//! CLI hosts the session. Everything PROVIDER-SPECIFIC sits behind the
//! [`SessionProviderAdapter`] trait — one impl per provider (Claude is #1).
//! The runner is not a Claude client.
//!
//! ## Data vs behaviour
//!
//! A provider's FACTS — its resume argv, account-isolation variable, restore
//! tier, resume handshake markers — are data in its
//! [`qontinui_runner_lib::cli_profile`] profile, looked up with
//! `cli_profile::profile_for(provider)`. An unknown provider has no profile,
//! so it has no resume either: nothing here degrades it to Claude. This trait
//! keeps only BEHAVIOUR that cannot be data (materializing a capture hook,
//! building a pinned launch) (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 4).
//!
//! ## The key simplification (plan §4)
//!
//! Both shipped adapters support `--session-id` pinning, so the runner KNOWS
//! the session id at launch (it generated it) and records synchronously —
//! the SessionStart hook is **confirmation + liveness + resume-source
//! signal**, not required for identity. Identity is deterministic even before
//! any hook fires.

use std::collections::BTreeMap;

use qontinui_runner_lib::cli_profile::{self, claude};
use qontinui_types::cli_session::{AutoApprove, IdentitySource, RestoreTier as ProfileRestoreTier};

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

/// The spawn recipe an adapter produces so the runner can launch the provider
/// with a KNOWN-up-front session id (plan §4 `launch_with_identity`). The
/// runner injects `env` into the PTY child, runs `argv`, and records
/// `pinned_session_id` authoritatively at spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// The program-and-args to run as the PTY child (`["claude", "--session-id", "<uuid>"]`).
    pub argv: Vec<String>,
    /// Extra env to inject into the child (account isolation, identity vars).
    pub env: BTreeMap<String, String>,
    /// The session id the runner pinned (`--session-id <uuid>`) — recorded
    /// authoritatively at spawn, zero transcript race.
    pub pinned_session_id: String,
}

/// How the runner attaches its SessionStart capture hook WITHOUT editing the
/// user's provider config (plan §4 `capture_hook_delivery`). Claude:
/// `--settings <bundled>`; Gemini: a project-local `.gemini/settings.json` or
/// `--extensions`. The variants enumerate the delivery mechanisms; Phase 2/3
/// fill the concrete payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliverySpec {
    /// Pass an extra settings file on the launch argv (Claude `--settings`).
    /// Additive — never touches `~/.<provider>`.
    SettingsFlag {
        /// Absolute path to the runner-app-data settings file carrying the hook.
        settings_path: String,
    },
    /// Write a runner-managed project-local settings file in the controlled
    /// cwd (Gemini `.gemini/settings.json`). The path is relative to the
    /// session cwd; the runner ensures it is gitignored.
    ProjectLocalFile {
        /// Path relative to the session cwd (e.g. `.gemini/settings.json`).
        relative_path: String,
        /// The settings JSON to write registering the SessionStart hook.
        contents: String,
    },
    /// No hook delivery available on this provider/version — identity rides on
    /// the runner-pinned `--session-id` alone (still deterministic) and the
    /// confirmation hook is simply absent.
    None,
}

/// One provider's session-management BEHAVIOUR. Implemented once per provider.
/// Its facts (resume argv, account isolation, restore tier) are profile data —
/// see the module docs.
pub trait SessionProviderAdapter: Send + Sync {
    /// The provider id this adapter handles (`"claude"`, `"gemini"`). Matches
    /// the stored [`crate::session::session_lifecycle_store::TerminalSessionRecord::provider`].
    fn provider(&self) -> &'static str;

    /// Build the spawn recipe with the id known up front (plan §4). `cwd` is
    /// the session working dir; `account` is the optional account selector
    /// (Claude config dir / Gemini home).
    fn launch_with_identity(&self, cwd: &str, account: Option<&str>) -> LaunchSpec;

    /// How to attach the runner's SessionStart capture hook without editing
    /// user config (plan §4).
    fn capture_hook_delivery(&self, cwd: &str) -> DeliverySpec;
}

/// The Claude reference adapter. Its launch argv reads the Claude profile's
/// identity flag, auto-approve flags and account-isolation variable; only the
/// hook materialization is code. No path here panics.
///
/// Resume handshake/failure markers are profile data too
/// (`cli_profile::claude`), served to the frontend's `resumeVerification.ts`
/// and pinned across both languages by the shared screen fixtures under
/// `src-tauri/tests/fixtures/cli_screens/`.
pub struct ClaudeAdapter;

impl SessionProviderAdapter for ClaudeAdapter {
    fn provider(&self) -> &'static str {
        claude::ID
    }

    fn launch_with_identity(&self, cwd: &str, account: Option<&str>) -> LaunchSpec {
        // A runner-generated uuid pinned via the profile's identity flag, so
        // identity is KNOWN at spawn (recorded synchronously, zero transcript
        // race — plan §3b/§4), in the profile's auto-approve mode so a
        // runner-spawned session never stalls on a permission prompt.
        let profile = claude_profile();
        let pinned = uuid::Uuid::new_v4().to_string();
        let mut argv: Vec<String> = profile.programs.iter().take(1).cloned().collect();
        if let AutoApprove::Flags { argv: flags, .. } = &profile.auto_approve {
            argv.extend(flags.iter().cloned());
        }
        if let IdentitySource::Pinned { flag } = &profile.identity {
            argv.push(flag.clone());
            argv.push(pinned.clone());
        }
        // Attach the SessionStart capture hook ADDITIVELY via `--settings`
        // (never touches `~/.claude`). When the delivery resolves to a settings
        // file, it rides on the argv; otherwise identity still rides the pin.
        if let DeliverySpec::SettingsFlag { settings_path } = self.capture_hook_delivery(cwd) {
            argv.push("--settings".to_string());
            argv.push(settings_path);
        }
        LaunchSpec {
            argv,
            env: cli_profile::account_isolation_env(profile, account),
            pinned_session_id: pinned,
        }
    }

    fn capture_hook_delivery(&self, _cwd: &str) -> DeliverySpec {
        // Materialize the bundled SessionStart hook (script + `--settings` file)
        // into the runner's OWN app-data dir (`~/.qontinui/runner/session-
        // restore/`) — NEVER `~/.claude` — and report its path. The hook POSTs
        // `{terminal_id, session_id, source, provider, cwd}` to
        // `/control/session-open` on startup AND `--resume` (Phase-0-proven
        // additive `--settings` delivery). Fail-open: a materialize failure
        // degrades to `None` (identity still rides the pinned `--session-id`).
        let dir = crate::session::claude_hook::session_restore_dir();
        match crate::session::claude_hook::materialize(&dir) {
            Some(settings_path) => DeliverySpec::SettingsFlag {
                settings_path: settings_path.to_string_lossy().into_owned(),
            },
            None => DeliverySpec::None,
        }
    }
}

/// The Claude profile. It is compiled into the runner, so its absence is a
/// build defect, not a runtime condition.
fn claude_profile() -> &'static qontinui_types::cli_session::CliProfile {
    cli_profile::profile_for(claude::ID).expect("the Claude profile is compiled in")
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
    fn claude_adapter_surface_is_sane_and_panic_free() {
        let _amb = crate::test_env::isolated_ambient();
        let a = ClaudeAdapter;
        assert_eq!(a.provider(), "claude");

        // launch_with_identity pins a uuid into the argv + reports it, in
        // the profile's autonomous (bypassPermissions) mode.
        let spec = a.launch_with_identity("C:/repo", Some("C:/cfg"));
        assert_eq!(spec.argv.first().map(String::as_str), Some("claude"));
        assert!(spec.argv.contains(&"--permission-mode".to_string()));
        assert!(spec.argv.contains(&"bypassPermissions".to_string()));
        assert!(spec.argv.contains(&"--session-id".to_string()));
        assert!(spec.argv.contains(&spec.pinned_session_id));
        assert_eq!(
            spec.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("C:/cfg")
        );
        // No account ⇒ no isolation env (the spawn path's resolution applies).
        assert!(a.launch_with_identity("C:/repo", None).env.is_empty());
    }

    #[test]
    fn capture_hook_delivery_is_settings_flag_never_dot_claude() {
        let _amb = crate::test_env::isolated_ambient();
        // The Claude hook delivery is an additive `--settings <file>` pointing
        // at a runner-app-data settings file — NEVER `~/.claude`. (This
        // materializes into the real ~/.qontinui/runner/session-restore/ dir on
        // the test host; that dir is the runner's own app data, not the user's
        // claude config, which is exactly the out-of-box guarantee.)
        let a = ClaudeAdapter;
        match a.capture_hook_delivery("C:/repo") {
            DeliverySpec::SettingsFlag { settings_path } => {
                assert!(
                    settings_path.contains("session-restore"),
                    "delivery is the runner-app-data hook settings file"
                );
                assert!(
                    !settings_path.replace('\\', "/").contains("/.claude/"),
                    "delivery NEVER points into the user's ~/.claude"
                );
                // The pinned-launch argv carries the same `--settings` flag.
                let spec = a.launch_with_identity("C:/repo", None);
                assert!(spec.argv.contains(&"--settings".to_string()));
            }
            // Fail-open: if the runner app-data dir couldn't be written on this
            // host, the delivery degrades to None and the launch omits
            // `--settings` (identity still rides the pin) — also acceptable.
            DeliverySpec::None => {
                let spec = a.launch_with_identity("C:/repo", None);
                assert!(!spec.argv.contains(&"--settings".to_string()));
            }
            other => panic!("unexpected Claude delivery: {other:?}"),
        }
    }
}
