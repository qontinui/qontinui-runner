//! The OpenAI Codex CLI [`CliProfile`] — the runner's second PTY-hosted
//! provider (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 6).
//!
//! Data only. The behaviour a Codex session needs — reading its self-minted id
//! back out of the rollout file, matched by cwd — is runner code
//! (`session/codex_capture.rs`, reached through `session/provider_adapter.rs`
//! `CodexAdapter`). It was ported from qontinui-runner PR #651 (`75198ffd4`),
//! which merged into a stacked base and never reached `main`.
//!
//! | Field | Source |
//! |---|---|
//! | `programs`, `resume`, `auto_approve`, `auth` | `codex --help` / `codex resume --help`, codex-cli `0.159.1` (plan probe Q5) |
//! | `identity`, `transcript` | Q5's unauthenticated `codex exec`: `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`, id in `session_meta.payload` |
//! | `account_isolation` | #651 `CodexAdapter` (`CODEX_HOME`) |
//! | `install` | the npm package the probe ran (`@openai/codex`) |
//!
//! Everything a probe could not establish is recorded as unknown or empty, and
//! the reason is in [`CliProfile::notes`]. Nothing is guessed.

use std::sync::LazyLock;

use qontinui_types::cli_session::{
    AccountIsolation, AuthMethod, AutoApprove, CapabilityState, CliProfile, GracefulExit,
    HandshakePatterns, IdentitySource, InstallCommands, RestoreTier, ResumeSpec, StructuredLane,
    TranscriptSpec, TrustDialog,
};

/// The provider id Codex sessions are recorded under.
pub const ID: &str = "codex";

/// The [`IdentitySource::ReadBack`] capture mechanism name: the runner reads
/// the id from the session's rollout file (`session/codex_capture.rs`).
pub const CAPTURE_SESSION_FILE: &str = "codex_session_file";

/// The registered Codex CLI profile.
pub static PROFILE: LazyLock<CliProfile> = LazyLock::new(profile);

/// The install command the probe used, which is the same on every OS (npm).
const NPM_INSTALL: &str = "npm i -g @openai/codex";

/// Build the Codex profile. Called once, by [`PROFILE`].
fn profile() -> CliProfile {
    let strings = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    CliProfile {
        id: ID.to_string(),
        display_name: "Codex CLI".to_string(),
        // A stem, like Claude's: `profile_for_program` strips a path and a
        // Windows launcher suffix, so `codex.exe` and npm's `codex.cmd` resolve
        // here without being listed.
        programs: strings(&["codex"]),
        verified_version: Some("0.159.1".to_string()),
        // Codex has no flag that takes a session id: it mints its own (a
        // UUIDv7) and writes it into the rollout file.
        identity: IdentitySource::ReadBack {
            capture: CAPTURE_SESSION_FILE.to_string(),
        },
        // `codex resume [OPTIONS] [SESSION_ID] [PROMPT]` — a subcommand with a
        // positional id, so there is no resume FLAG and no alias.
        resume: ResumeSpec::ByIdArgv {
            template: strings(&["codex", "resume", "{id}"]),
        },
        resume_flag_aliases: Vec::new(),
        // `resume` without an id opens Codex's picker, and `fork` starts from
        // an existing session; either way the user chose the session.
        session_choice_args: strings(&["resume", "fork"]),
        pty_args: Vec::new(),
        // `CODEX_HOME` selects the directory holding Codex's auth, config and
        // sessions (default `~/.codex`). It is a variable naming one
        // per-account directory — the same shape as `CLAUDE_CONFIG_DIR` — so
        // `EnvVar`. `HomeDir` would mean relocating `$HOME`, which moves every
        // other tool's state too and is not how Codex scopes an account.
        account_isolation: AccountIsolation::EnvVar {
            name: "CODEX_HOME".to_string(),
        },
        auto_approve: AutoApprove::Flags {
            argv: strings(&["--dangerously-bypass-approvals-and-sandbox"]),
            // Every 0.159.1 spelling that runs without approval prompts. The
            // `never` approval policy still sandboxes, but it is prompt-free,
            // which is the property detection reports. `--full-auto` is not a
            // 0.159.1 flag (absent from `--help`).
            detect: strings(&[
                "--dangerously-bypass-approvals-and-sandbox",
                "--ask-for-approval never",
                "--ask-for-approval=never",
                "-a never",
            ]),
        },
        graceful_exit: GracefulExit::Unknown,
        // Deliberately empty: #651's markers were derived against 0.142.2 and
        // flagged unverified by their author, and no authenticated Codex TUI has
        // been recorded since. An empty set verifies nothing and fails nothing.
        handshake: HandshakePatterns::default(),
        usage_limit_phrases: Vec::new(),
        transcript: TranscriptSpec::SessionFileGlob {
            glob: ".codex/sessions/**/rollout-*.jsonl".to_string(),
        },
        structured_lane: StructuredLane::CodexAppServer,
        typed_permission: CapabilityState::Unknown,
        turn_boundary_event: CapabilityState::Unknown,
        rate_limit_event: CapabilityState::Unknown,
        trust_dialog: TrustDialog::Unknown,
        auth: vec![AuthMethod::CliLogin {
            args: strings(&["login"]),
        }],
        // Terminal-only until resume continuity is observed. `codex resume
        // <id>` exists, but whether it brings the conversation back is UNKNOWN
        // (the probe CLI was unauthenticated). Promising `Full` would type a
        // resume at every restored Codex zone and present the result as the old
        // conversation; terminal-only restores the pane and cwd and says the
        // conversation starts fresh, which is true either way.
        restore_tier: RestoreTier::TerminalOnly,
        install: InstallCommands {
            linux: Some(NPM_INSTALL.to_string()),
            macos: Some(NPM_INSTALL.to_string()),
            windows: Some(NPM_INSTALL.to_string()),
        },
        notes: strings(&[
            "verified_version: `--help` and `resume --help` were read from codex-cli 0.159.1 \
             on Linux. That CLI was NOT authenticated, so nothing that needs a live session \
             was observed.",
            "Resume continuity is UNKNOWN: `codex resume <id>` exists, but whether it restores \
             the conversation has not been observed. restore_tier stays terminal_only until an \
             authenticated probe shows it does.",
            "identity: read back from `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl` \
             (line 1 `session_meta`, id in `payload`), matched by cwd and written after the \
             launch. Ids are UUIDv7; never assume UUIDv4.",
            "transcript: the glob is relative to the home directory. With CODEX_HOME set, that \
             directory replaces `.codex`.",
            "handshake: empty on purpose. #651's markers were derived against 0.142.2 and were \
             never checked against an authenticated CLI.",
            "graceful_exit, usage_limit_phrases, trust_dialog and the three event capabilities \
             are unknown: none were observable without authentication.",
            "structured_lane: the CLI offers `codex app-server` (experimental in 0.159.1). The \
             runner implements NO Codex structured lane, so a launch surface must never offer \
             one; every Codex session is PTY-hosted.",
            "Platforms: Linux help text only. macOS is the same code path but unverified on \
             hardware. Windows is UNKNOWN; the identity shim's `.cmd` wrapper is untested there.",
        ]),
    }
}
