//! Provider-agnostic session adapter contract (plan
//! `2026-06-25-runner-session-restore-redesign.md` §4).
//!
//! The runner's session-restore CORE is provider-agnostic: the lifecycle
//! store, the terminal↔zone↔page model, the per-PTY `QONTINUI_TERMINAL_ID`
//! correlation, the boot-restore orchestration, the reconcile backstop, and
//! the local registration endpoint all work the same regardless of which AI
//! CLI hosts the session. Everything PROVIDER-SPECIFIC sits behind the
//! [`SessionProviderAdapter`] trait — one impl per provider ([`ClaudeAdapter`]
//! is #1, [`CodexAdapter`] is #2). The runner is not a Claude client.
//!
//! ## What this ships
//!
//! Phase 1 shipped the TRAIT + its supporting types + the registry seam
//! ([`adapter_for`]); Phase 2 filled in [`ClaudeAdapter`]; **Phase 3 adds the
//! second provider, [`CodexAdapter`], and generalizes the identity model**.
//!
//! ## Identity is pin-OR-read-back (the Phase 3 generalization)
//!
//! The original contract was PIN-ONLY (`LaunchSpec.pinned_session_id`): it
//! assumed the runner GENERATED the id (Claude `--session-id <uuid>`) and knew
//! it synchronously at spawn. Codex breaks that — it has no `--session-id` flag
//! and generates its own id, which the runner must READ BACK post-launch. So
//! identity is now an [`IdentitySource`] enum: [`IdentitySource::Pinned`]
//! (Claude — known at spawn, recorded authoritatively) OR
//! [`IdentitySource::ReadBack`] (Codex — recovered post-launch via an
//! [`IdentityCapture`] channel, recorded provisionally then confirmed). The spawn
//! seam ([`crate::terminal::session`]) branches once on the arm; every provider
//! is modeled, not just the pin-only one.

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

/// How the runner learns a launched session's id (plan §4, generalized in
/// Phase 3 for the read-back providers). The original contract was **pin-only**
/// (`pinned_session_id: String`) — it baked in the assumption that the runner
/// GENERATED the id and therefore knew it synchronously at spawn (Claude
/// `--session-id <uuid>`). Codex breaks that assumption: it has NO `--session-id`
/// flag and GENERATES its own id, which the runner must READ BACK post-launch.
///
/// [`IdentitySource`] is the genuinely general model — every provider declares
/// HOW its id becomes known, and the spawn seam ([`crate::terminal::session`])
/// branches once on the arm:
///
/// - [`IdentitySource::Pinned`] — known synchronously; record authoritatively at
///   spawn exactly as before (Claude).
/// - [`IdentitySource::ReadBack`] — provider-generated; record a PROVISIONAL row
///   at spawn (no real id yet) and capture the real id via the declared channel,
///   then `confirm_session` it (Codex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentitySource {
    /// The runner pinned the id at launch (Claude `--session-id <uuid>`) — known
    /// synchronously, recorded authoritatively at spawn with zero transcript race.
    Pinned(String),
    /// The provider generates the id; the runner reads it back post-launch via
    /// the declared [`IdentityCapture`] channel. The spawn-time record is
    /// PROVISIONAL until the capture confirms it.
    ReadBack(IdentityCapture),
}

/// The post-launch channel a [`IdentitySource::ReadBack`] provider exposes for
/// the runner to recover the provider-generated session id. Both arms are
/// deterministic + fail-open: a miss degrades to the existing reconcile
/// backstop, never panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityCapture {
    /// Parse a JSONL stream event from the child's stdout. Codex
    /// `exec --json` emits `{"type":"thread.started","thread_id":"<uuid>"}` as its
    /// FIRST stdout line, before auth. `event_type` is the `type` value to match
    /// (`"thread.started"`); `id_field` is the field carrying the id
    /// (`"thread_id"`). Used for headless `codex exec --json` launches where the
    /// runner owns the child's stdout.
    StreamEvent {
        /// The JSONL event's `type` value to match (`"thread.started"`).
        event_type: String,
        /// The field on the matched event carrying the session id (`"thread_id"`).
        id_field: String,
    },
    /// Watch a session-file directory under the runner-isolated provider home and
    /// recover the id from the newest matching file written post-spawn. Codex
    /// writes `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<uuid>.jsonl` at
    /// session start; line 1 is
    /// `{"type":"session_meta","payload":{"session_id":"<uuid>",...}}`. This is
    /// the ROBUST channel — it works for ANY launch mode (interactive TUI
    /// included), since `CODEX_HOME` is runner-isolated (no cross-account
    /// pollution makes "the newest post-spawn rollout" deterministic).
    SessionFile {
        /// Env var naming the runner-isolated provider home to scan under
        /// (`"CODEX_HOME"`).
        dir_env: String,
        /// Glob (relative to the provider home) selecting session files
        /// (`"sessions/**/rollout-*.jsonl"`).
        glob: String,
        /// The line-1 meta event's `type` value (`"session_meta"`).
        meta_line_type: String,
        /// The field (under the meta event, incl. its `payload`) carrying the id
        /// (`"session_id"`).
        id_field: String,
    },
}

/// The spawn recipe an adapter produces so the runner can launch the provider
/// and learn its session id (plan §4 `launch_with_identity`). The runner injects
/// `env` into the PTY child, runs `argv`, and resolves identity per the
/// [`IdentitySource`] arm: a [`IdentitySource::Pinned`] id is recorded
/// authoritatively at spawn; a [`IdentitySource::ReadBack`] id is recorded
/// provisionally then captured + confirmed via the declared channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// The program-and-args to run as the PTY child
    /// (`["claude", "--session-id", "<uuid>"]`; `["codex"]`).
    pub argv: Vec<String>,
    /// Extra env to inject into the child (account isolation, identity vars).
    pub env: BTreeMap<String, String>,
    /// How the runner learns this session's id — pinned (known at spawn) or
    /// read-back (recovered post-launch). See [`IdentitySource`].
    pub identity: IdentitySource,
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

    fn launch_with_identity(&self, cwd: &str, account: Option<&str>) -> LaunchSpec {
        // The Rust home for what `aiLaunchCommand.ts` does: a runner-generated
        // uuid pinned via `--session-id` so identity is KNOWN at spawn (recorded
        // synchronously, zero transcript race — plan §3b/§4). Autonomous mode
        // (`--permission-mode bypassPermissions`) matches the operator's
        // clg/clh/clp wrappers so a runner-spawned session never stalls on a
        // permission prompt (mirrors `aiLaunchCommand.ts`).
        let pinned = uuid::Uuid::new_v4().to_string();
        let mut argv = vec![
            "claude".to_string(),
            "--permission-mode".to_string(),
            "bypassPermissions".to_string(),
            "--session-id".to_string(),
            pinned.clone(),
        ];
        // Attach the SessionStart capture hook ADDITIVELY via `--settings`
        // (never touches `~/.claude`). When the delivery resolves to a settings
        // file, it rides on the argv; otherwise identity still rides the pin.
        if let DeliverySpec::SettingsFlag { settings_path } = self.capture_hook_delivery(cwd) {
            argv.push("--settings".to_string());
            argv.push(settings_path);
        }
        LaunchSpec {
            argv,
            env: self.account_isolation(account),
            // Claude pins the id at launch — known synchronously, recorded
            // authoritatively at spawn (zero transcript race). Byte-identical
            // behavior to the pre-Phase-3 pin-only contract.
            identity: IdentitySource::Pinned(pinned),
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

    fn resume_command(&self, session_id: &str, _account: Option<&str>) -> Vec<String> {
        // Claude resume is the deterministic, non-interactive `claude --resume
        // <id>` (plan §4). Account isolation rides the ENV
        // (`account_isolation`), not the argv — the resume must look identical
        // across accounts so the typed-resume sniff + handshake stay stable.
        vec![
            "claude".to_string(),
            "--resume".to_string(),
            session_id.to_string(),
        ]
    }

    fn account_isolation(&self, account: Option<&str>) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        if let Some(dir) = account {
            // Claude config/home isolation is `CLAUDE_CONFIG_DIR` (plan §4) —
            // the SAME var `terminal/session.rs` sets around line 501 from
            // `ai_provider::get_effective_config_dir`. `account` here is the
            // already-resolved per-account config dir; the adapter just names
            // the env var. An absent account ⇒ empty (the runner's
            // process-global resolved dir applies, set by the spawn path).
            env.insert("CLAUDE_CONFIG_DIR".to_string(), dir.to_string());
        }
        env
    }

    fn resume_handshake_patterns(&self) -> HandshakePatterns {
        // Mirror of `resumeVerification.ts` — the SUCCESS set is the Claude TUI
        // handshake markers (the CLI took over the terminal); the FAILURE set is
        // definitive "the requested session did NOT resume" evidence (unknown id
        // / fell through to the session picker), checked BEFORE success since a
        // failure dialog is itself Claude UI. Plain substrings matched against
        // ANSI-stripped output (the TS uses regexes; these are the literal
        // substrings those regexes key on, since the trait contract is
        // substring-based).
        HandshakePatterns {
            success: vec![
                "? for shortcuts".to_string(),  // status-line hint under the input box
                "esc to interrupt".to_string(), // shown while Claude is working
                "bypass permissions".to_string(), // permission-mode indicator
                "Welcome to Claude".to_string(), // launch banner
                "Welcome back to Claude".to_string(), // resumed-banner variant
            ],
            failure: vec![
                "No conversation found".to_string(), // `--resume <unknown-id>` error
                "No conversations found".to_string(), // empty-history variant
                "No conversations to resume".to_string(),
                "Select a session to resume".to_string(), // interactive picker frame
                "Select a conversation to resume".to_string(),
            ],
        }
    }

    fn restore_tier(&self) -> RestoreTier {
        RestoreTier::Full
    }
}

/// The OpenAI Codex CLI adapter (provider #2, Phase 3). The seam-proving second
/// provider: Codex has **no `--session-id` flag** — it GENERATES its own session
/// id (a UUIDv7 it calls `thread_id`/`session_id`), which the runner READS BACK
/// post-launch. So this adapter returns [`IdentitySource::ReadBack`] rather than
/// `Pinned`, exercising the generalized identity model end-to-end.
///
/// ## Identity is read-back, not pinned
///
/// Codex writes `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<uuid>.jsonl`
/// at session start (any launch mode — interactive TUI included); line 1 is
/// `{"type":"session_meta","payload":{"session_id":"<uuid>",...}}`. The runner
/// isolates `CODEX_HOME` per account, so "the newest rollout written after this
/// spawn" deterministically names THIS session. [`crate::session::codex_capture`]
/// actuates the [`IdentityCapture::SessionFile`] channel.
///
/// Pin-your-version: empirically verified against codex-cli 0.142.2 on Windows.
pub struct CodexAdapter;

/// The env var naming Codex's runner-isolated config+auth+sessions home — the
/// `CLAUDE_CONFIG_DIR` analogue. Relocating it isolates the account AND makes the
/// rollout-file read-back deterministic (no cross-account session pollution).
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";

impl SessionProviderAdapter for CodexAdapter {
    fn provider(&self) -> &'static str {
        "codex"
    }

    fn launch_with_identity(&self, _cwd: &str, account: Option<&str>) -> LaunchSpec {
        // Interactive hand-start argv. Codex has NO `--session-id` flag — it
        // generates its own id — so we never append one; identity is read-back.
        // (We deliberately do not force a permission/yolo flag here: the
        // interactive Codex TUI hand-start mirrors the operator's own use, and
        // Codex's approval posture is configured via CODEX_HOME, not argv.)
        let argv = vec!["codex".to_string()];
        LaunchSpec {
            argv,
            env: self.account_isolation(account),
            // Codex generates the id — the runner reads it back from the newest
            // post-spawn rollout file under the runner-isolated CODEX_HOME. The
            // spawn-time record is PROVISIONAL until the capture confirms it.
            identity: IdentitySource::ReadBack(IdentityCapture::SessionFile {
                dir_env: CODEX_HOME_ENV.to_string(),
                glob: "sessions/**/rollout-*.jsonl".to_string(),
                meta_line_type: "session_meta".to_string(),
                id_field: "session_id".to_string(),
            }),
        }
    }

    fn capture_hook_delivery(&self, _cwd: &str) -> DeliverySpec {
        // Codex has NO reliable SessionStart hook: the upstream `SessionStart`
        // hook does NOT fire in `codex exec` (verified upstream bug), so we do
        // NOT deliver one. Identity instead rides the READ-BACK channel
        // (`IdentityCapture::SessionFile` over the runner-isolated CODEX_HOME) —
        // the rollout file's line-1 `session_meta` is the confirmation signal,
        // not a POSTing hook. Hence `DeliverySpec::None`: there is no settings
        // file or project-local hook to attach.
        DeliverySpec::None
    }

    fn resume_command(&self, session_id: &str, _account: Option<&str>) -> Vec<String> {
        // Interactive (typed-into-the-restored-PTY) resume — the `claude --resume`
        // analogue. Codex accepts the UUID directly: `codex resume <uuid>`.
        // (Headless is `codex exec resume <uuid>`; the interactive form is what
        // the boot-restore types into a restored tab.) Account isolation rides
        // the ENV (`account_isolation`), not the argv, so the resume looks
        // identical across accounts.
        vec![
            "codex".to_string(),
            "resume".to_string(),
            session_id.to_string(),
        ]
    }

    fn account_isolation(&self, account: Option<&str>) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        if let Some(dir) = account {
            // Codex config/auth/sessions/keyring isolation is `CODEX_HOME` (the
            // `CLAUDE_CONFIG_DIR` analogue). `account` is the already-resolved
            // per-account dir; the adapter just names the env var. Absent ⇒ empty
            // (the runner's process-global CODEX_HOME, if any, applies).
            env.insert(CODEX_HOME_ENV.to_string(), dir.to_string());
        }
        env
    }

    fn resume_handshake_patterns(&self) -> HandshakePatterns {
        // Best-effort Codex-TUI markers. BEST-EFFORT / PIN-YOUR-VERSION: these
        // plain substrings are derived from the codex-cli 0.142.2 TUI surface and
        // MAY need refinement against a live AUTHENTICATED codex (the success
        // markers especially — an unauthenticated probe can't render the full
        // resumed-conversation frame). Failure is checked first by consumers
        // since a "no previous sessions" dialog is itself Codex UI.
        HandshakePatterns {
            success: vec![
                "Codex".to_string(),          // the TUI banner / header
                "send a message".to_string(), // input-box hint
                "/help".to_string(),          // status-line shortcut hint
            ],
            failure: vec![
                "No previous sessions".to_string(), // resume with no history
                "not found".to_string(),            // unknown id
                "No such session".to_string(),
                "error: ".to_string(), // generic codex CLI error prefix
            ],
        }
    }

    fn restore_tier(&self) -> RestoreTier {
        // Codex resumes the FULL conversation by id (`codex resume <uuid>`).
        RestoreTier::Full
    }
}

/// Registry seam (plan §4): resolve the adapter for `provider`. Phase 1 knew
/// only the Claude adapter; Phase 2 filled it in; Phase 3 adds the Codex arm. An
/// UNKNOWN provider degrades to the Claude adapter (the reference provider)
/// rather than failing — a record with an unexpected provider should still
/// restore via the default path, never be dropped.
pub fn adapter_for(provider: &str) -> Box<dyn SessionProviderAdapter> {
    match provider {
        "codex" => Box::new(CodexAdapter),
        _ => Box::new(ClaudeAdapter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_for_resolves_claude_codex_and_defaults_unknown() {
        assert_eq!(adapter_for("claude").provider(), "claude");
        assert_eq!(adapter_for("codex").provider(), "codex");
        // Unknown provider degrades to the Claude adapter (never drops).
        assert_eq!(adapter_for("gemini").provider(), "claude");
        assert_eq!(adapter_for("totally-new").provider(), "claude");
    }

    /// Helper: pull the pinned id out of a `Pinned` identity (test ergonomics).
    fn pinned_id(spec: &LaunchSpec) -> &str {
        match &spec.identity {
            IdentitySource::Pinned(id) => id,
            other => panic!("expected Pinned identity, got {other:?}"),
        }
    }

    #[test]
    fn claude_adapter_surface_is_sane_and_panic_free() {
        let a = ClaudeAdapter;
        assert_eq!(a.restore_tier(), RestoreTier::Full);

        // launch_with_identity pins a uuid into the argv + reports it, in
        // autonomous (bypassPermissions) mode (mirrors aiLaunchCommand.ts).
        let spec = a.launch_with_identity("C:/repo", Some("C:/cfg"));
        assert_eq!(spec.argv.first().map(String::as_str), Some("claude"));
        assert!(spec.argv.contains(&"--permission-mode".to_string()));
        assert!(spec.argv.contains(&"bypassPermissions".to_string()));
        assert!(spec.argv.contains(&"--session-id".to_string()));
        // Claude's identity is Pinned (known synchronously) and the pinned id is
        // in the argv.
        let pinned = pinned_id(&spec).to_string();
        assert!(spec.argv.contains(&pinned));
        assert_eq!(
            spec.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("C:/cfg")
        );

        // resume_command is the deterministic --resume form (account rides env).
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

        // resume_handshake_patterns ports the real sets from resumeVerification.ts
        // (non-empty success + failure; failure is checked first by consumers).
        let hp = a.resume_handshake_patterns();
        assert!(hp.success.iter().any(|s| s.contains("for shortcuts")));
        assert!(hp
            .failure
            .iter()
            .any(|s| s.contains("No conversation found")));
    }

    #[test]
    fn codex_adapter_surface_is_read_back_and_panic_free() {
        let a = CodexAdapter;
        assert_eq!(a.provider(), "codex");
        // Codex resumes the full conversation by id.
        assert_eq!(a.restore_tier(), RestoreTier::Full);

        // launch_with_identity: argv is a bare `["codex"]` hand-start — NO
        // `--session-id` (codex has no such flag). Identity is read-back via the
        // CODEX_HOME rollout-file channel.
        let spec = a.launch_with_identity("C:/repo", Some("C:/codex-home"));
        assert_eq!(spec.argv, vec!["codex".to_string()]);
        assert!(
            !spec.argv.iter().any(|a| a == "--session-id"),
            "codex has NO --session-id flag"
        );
        match &spec.identity {
            IdentitySource::ReadBack(IdentityCapture::SessionFile {
                dir_env,
                glob,
                meta_line_type,
                id_field,
            }) => {
                assert_eq!(dir_env, "CODEX_HOME");
                assert_eq!(glob, "sessions/**/rollout-*.jsonl");
                assert_eq!(meta_line_type, "session_meta");
                assert_eq!(id_field, "session_id");
            }
            other => panic!("expected ReadBack(SessionFile), got {other:?}"),
        }

        // CODEX_HOME account isolation (the CLAUDE_CONFIG_DIR analogue).
        assert_eq!(
            spec.env.get("CODEX_HOME").map(String::as_str),
            Some("C:/codex-home")
        );
        assert!(a.account_isolation(None).is_empty());
        assert_eq!(
            a.account_isolation(Some("C:/codex-home")).get("CODEX_HOME"),
            Some(&"C:/codex-home".to_string())
        );

        // resume_command is the interactive `codex resume <id>` form.
        assert_eq!(
            a.resume_command("11111111-2222-7333-8444-555555555555", None),
            vec![
                "codex".to_string(),
                "resume".to_string(),
                "11111111-2222-7333-8444-555555555555".to_string()
            ]
        );

        // capture_hook_delivery is None — codex uses read-back, not a hook.
        assert_eq!(a.capture_hook_delivery("C:/repo"), DeliverySpec::None);

        // Handshake patterns are non-empty (best-effort) on both sides.
        let hp = a.resume_handshake_patterns();
        assert!(!hp.success.is_empty());
        assert!(hp
            .failure
            .iter()
            .any(|s| s.contains("No previous sessions")));
    }

    #[test]
    fn capture_hook_delivery_is_settings_flag_never_dot_claude() {
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
