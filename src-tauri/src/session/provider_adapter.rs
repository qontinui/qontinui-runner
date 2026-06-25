//! Provider-agnostic session adapter contract (plan
//! `2026-06-25-runner-session-restore-redesign.md` §4).
//!
//! The runner's session-restore CORE is provider-agnostic: the lifecycle
//! store, the terminal↔zone↔page model, the per-PTY `QONTINUI_TERMINAL_ID`
//! correlation, the boot-restore orchestration, the reconcile backstop, and
//! the local registration endpoint all work the same regardless of which AI
//! CLI hosts the session. Everything PROVIDER-SPECIFIC sits behind the
//! [`SessionProviderAdapter`] trait — one impl per provider (Claude is #1,
//! Gemini #2). The runner is not a Claude client.
//!
//! ## What this phase ships
//!
//! Phase 1 ships the TRAIT + its supporting types + the registry seam
//! ([`adapter_for`]) ONLY. The Claude reference adapter's resume/hook bodies
//! are Phase 2 — the [`ClaudeAdapter`] here is a minimal placeholder that
//! returns sensible defaults (NO `todo!()`/panics on any build- or test-
//! exercised path), so the crate compiles and Phase 2 fills the bodies in
//! without touching the registry seam.
//!
//! ## The key simplification (plan §4)
//!
//! Both shipped adapters support `--session-id` pinning, so the runner KNOWS
//! the session id at launch (it generated it) and records synchronously —
//! the SessionStart hook is **confirmation + liveness + resume-source
//! signal**, not required for identity. Identity is deterministic even before
//! any hook fires.

use std::collections::BTreeMap;

use crate::session::session_lifecycle_store::DEFAULT_PROVIDER;

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

/// The handshake/failure pattern sets a provider's resume produces, consumed
/// by `resumeVerification` (plan §4 `resume_handshake_patterns`). Patterns are
/// plain substrings matched against ANSI-stripped terminal output.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HandshakePatterns {
    /// Substrings whose presence confirms the resume landed (the conversation
    /// re-opened).
    pub success: Vec<String>,
    /// Substrings whose presence means the resume FAILED (id not found,
    /// expired, picker error) — drives the `ResumeFailedBanner`.
    pub failure: Vec<String>,
}

/// One provider's session-management contract. Implemented once per provider.
/// Phase 1 ships the trait + the registry seam; Phase 2 fills the Claude impl.
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

    /// The deterministic, non-interactive resume argv for `session_id` under
    /// `account` (plan §4): Claude `["claude", "--resume", "<id>"]`.
    fn resume_command(&self, session_id: &str, account: Option<&str>) -> Vec<String>;

    /// Config/home isolation env for `account` (plan §4): Claude
    /// `CLAUDE_CONFIG_DIR`; Gemini `HOME`/project separation.
    fn account_isolation(&self, account: Option<&str>) -> BTreeMap<String, String>;

    /// Success/failure handshake patterns for `resumeVerification` (plan §4).
    fn resume_handshake_patterns(&self) -> HandshakePatterns;

    /// Declared restore capability for honest UX (plan §4).
    fn restore_tier(&self) -> RestoreTier;
}

/// The Claude reference adapter — Phase 1 PLACEHOLDER. The trait surface
/// compiles and returns sensible defaults; **Phase 2 fills the resume/hook
/// bodies** (move `aiLaunchCommand.ts`'s `--session-id` logic behind
/// `launch_with_identity`, ship the bundled `--settings` hook, wire the
/// handshake patterns from `resumeVerification.ts`). No path here panics.
pub struct ClaudeAdapter;

impl SessionProviderAdapter for ClaudeAdapter {
    fn provider(&self) -> &'static str {
        DEFAULT_PROVIDER // "claude"
    }

    fn launch_with_identity(&self, _cwd: &str, account: Option<&str>) -> LaunchSpec {
        // Phase 2 fills the full argv/env. The deterministic skeleton —
        // runner-generated uuid pinned via `--session-id` — is correct now so
        // the registry seam + a Phase-1 caller see a usable shape.
        let pinned = uuid::Uuid::new_v4().to_string();
        LaunchSpec {
            argv: vec![
                "claude".to_string(),
                "--session-id".to_string(),
                pinned.clone(),
            ],
            env: self.account_isolation(account),
            pinned_session_id: pinned,
        }
    }

    fn capture_hook_delivery(&self, _cwd: &str) -> DeliverySpec {
        // Phase 2 ships the bundled hook settings file + path. Until then the
        // hook is absent (identity still rides the pinned `--session-id`).
        DeliverySpec::None
    }

    fn resume_command(&self, session_id: &str, _account: Option<&str>) -> Vec<String> {
        // Claude resume is `claude --resume <id>` (plan §4 / Phase 2). Stable
        // enough to ship now; Phase 2 layers account isolation onto the env,
        // not the argv.
        vec![
            "claude".to_string(),
            "--resume".to_string(),
            session_id.to_string(),
        ]
    }

    fn account_isolation(&self, account: Option<&str>) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        if let Some(dir) = account {
            // Claude config/home isolation is `CLAUDE_CONFIG_DIR` (plan §4).
            env.insert("CLAUDE_CONFIG_DIR".to_string(), dir.to_string());
        }
        env
    }

    fn resume_handshake_patterns(&self) -> HandshakePatterns {
        // Phase 2 ports the real patterns from `resumeVerification.ts`. Empty
        // sets are a safe default (no false confirm / no false failure).
        HandshakePatterns::default()
    }

    fn restore_tier(&self) -> RestoreTier {
        RestoreTier::Full
    }
}

/// Registry seam (plan §4): resolve the adapter for `provider`. Phase 1 knows
/// only the future Claude adapter; Phase 2 fleshes out [`ClaudeAdapter`] and
/// Phase 3 adds the Gemini arm here. An UNKNOWN provider degrades to the Claude
/// adapter (the only shipped provider today) rather than failing — a record
/// with an unexpected provider should still restore via the default path, never
/// be dropped.
pub fn adapter_for(provider: &str) -> Box<dyn SessionProviderAdapter> {
    match provider {
        // Phase 3 adds: "gemini" => Box::new(GeminiAdapter),
        _ => Box::new(ClaudeAdapter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_for_resolves_claude_and_defaults_unknown() {
        assert_eq!(adapter_for("claude").provider(), "claude");
        // Unknown provider degrades to the Claude adapter (never drops).
        assert_eq!(adapter_for("gemini").provider(), "claude");
        assert_eq!(adapter_for("totally-new").provider(), "claude");
    }

    #[test]
    fn claude_adapter_surface_is_sane_and_panic_free() {
        let a = ClaudeAdapter;
        assert_eq!(a.restore_tier(), RestoreTier::Full);

        // launch_with_identity pins a uuid into the argv + reports it.
        let spec = a.launch_with_identity("C:/repo", Some("C:/cfg"));
        assert_eq!(spec.argv.first().map(String::as_str), Some("claude"));
        assert!(spec.argv.contains(&"--session-id".to_string()));
        assert!(spec.argv.contains(&spec.pinned_session_id));
        assert_eq!(
            spec.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("C:/cfg")
        );

        // resume_command is the deterministic --resume form.
        assert_eq!(
            a.resume_command("sess-1", None),
            vec![
                "claude".to_string(),
                "--resume".to_string(),
                "sess-1".to_string()
            ]
        );

        // account_isolation maps the account to CLAUDE_CONFIG_DIR (or empty).
        assert!(a.account_isolation(None).is_empty());
        assert_eq!(
            a.account_isolation(Some("C:/cfg")).get("CLAUDE_CONFIG_DIR"),
            Some(&"C:/cfg".to_string())
        );

        // hook delivery + handshake default to the honest "none/empty" shapes.
        assert_eq!(a.capture_hook_delivery("C:/repo"), DeliverySpec::None);
        assert_eq!(a.resume_handshake_patterns(), HandshakePatterns::default());
    }
}
