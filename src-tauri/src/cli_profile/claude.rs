//! The Claude Code [`CliProfile`].
//!
//! Every value here was lifted from the site that held it before this module
//! existed (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 4), or recorded by that plan's Phase 2 protocol probes against Claude
//! Code `2.1.285` on Linux:
//!
//! | Field | Lifted from |
//! |---|---|
//! | `handshake` | `src/components/terminal/providerAdapter.ts` `CLAUDE_HANDSHAKE_REGEXES` / `CLAUDE_RESUME_FAILURE_REGEXES` (deleted there; this is now their only home) |
//! | `usage_limit_phrases` | `terminal/usage_limit.rs` `USAGE_LIMIT_PATTERNS` |
//! | `graceful_exit` | `terminal/graceful_exit.rs` `EXIT_TEXT` |
//! | `auto_approve` | `terminal/manager.rs` `command_implies_bypass_permissions` (detect) and `session/provider_adapter.rs` `launch_with_identity` (argv) |
//! | `resume` | `session/provider_adapter.rs` `ClaudeAdapter::resume_command` (folded into this profile) |
//! | `account_isolation` | `session/provider_adapter.rs` `ClaudeAdapter::account_isolation` (folded into this profile) |
//! | `transcript` | `session_archive/discovery.rs` `PROJECTS_SUBDIR` |
//! | event capabilities | Phase 2 probes (`src-tauri/tests/fixtures/cli_protocol/claude/2.1.285/`) |
//!
//! The sites that still hold their own copy (usage-limit scan, graceful exit,
//! bypass detection) are replaced by lookups on this profile in the plan's
//! Phase 5; each carries a test pinning its copy to this profile until then.
//!
//! Handshake regex sources follow the [`super`] dialect contract: matched
//! case-insensitively by both engines, no inline flags, no look-around, no
//! backreferences.

use qontinui_types::cli_session::{
    AccountIsolation, AutoApprove, CapabilityState, CliProfile, GracefulExit, HandshakePatterns,
    IdentitySource, InstallCommands, RestoreTier, ResumeSpec, StructuredLane, TranscriptSpec,
    TrustDialog,
};

/// The provider id Claude sessions are recorded under — the same string as the
/// lifecycle store's `DEFAULT_PROVIDER`.
pub const ID: &str = "claude";

/// Build the Claude Code profile. Called once by [`super::all`].
pub(super) fn profile() -> CliProfile {
    let strings = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    CliProfile {
        id: ID.to_string(),
        display_name: "Claude Code".to_string(),
        programs: strings(&["claude"]),
        verified_version: Some("2.1.285".to_string()),
        identity: IdentitySource::Pinned {
            flag: "--session-id".to_string(),
        },
        resume: ResumeSpec::ByIdArgv {
            template: strings(&["claude", "--resume", "{id}"]),
        },
        account_isolation: AccountIsolation::EnvVar {
            name: "CLAUDE_CONFIG_DIR".to_string(),
        },
        auto_approve: AutoApprove::Flags {
            argv: strings(&["--permission-mode", "bypassPermissions"]),
            // Matched as substrings of the space-joined argv, so the joined
            // and the two-argument spellings of `--permission-mode` are both
            // listed.
            detect: strings(&[
                "--dangerously-skip-permissions",
                "--permission-mode bypassPermissions",
                "--permission-mode=bypassPermissions",
            ]),
        },
        graceful_exit: GracefulExit::TypedCommand {
            text: "/exit".to_string(),
        },
        handshake: HandshakePatterns {
            // Every marker is declared once, as a regex (plan
            // 2026-08-23-single-source-derived-facts item 9): the substring
            // lists stay empty.
            success: Vec::new(),
            failure: Vec::new(),
            // The Claude TUI actually rendered in the pane — deliberately
            // TUI-shaped, so a shell echo or an error line never counts. The
            // resume-size picker is Claude UI too and verifies.
            //
            // Claude Code v2 (measured on 2.1.286) dropped the rounded `╭───╮`
            // input box for plain `────` rules around a `❯` prompt, and its
            // first paint of a resumed session in a small pane showed none of
            // the older markers, so every boot-restore resume timed out and
            // was retyped into the live session's prompt. The two v2 markers
            // match that first paint, each anchored against a measured false
            // positive: "Claude Code" alone also appears in CLI errors printed
            // to the shell and in the folder-trust dialog, so the logo marker
            // needs the version; `❯` is a common shell prompt glyph and some
            // two-line prompts end their first line in a `─` fill, so the
            // frame marker needs a rule that starts its line, the prompt on
            // the next line, and a closing rule below it. Success markers are
            // matched against RENDERED text (cursor motion becomes whitespace,
            // `renderAnsi` in resumeVerification.ts), because v2 draws word
            // gaps with cursor moves.
            success_regex: strings(&[
                r"\? for shortcuts",           // status-line hint under the input box
                "esc to interrupt",            // shown while Claude is working
                "bypass permissions",          // permission-mode indicator
                "Welcome (?:back )?to Claude", // launch banner
                r"Claude Code v\d",            // v2 logo line ("▛███▛█ Claude Code v2.1.286")
                "[╭╰]─{3,}",                   // rounded input-box / dialog frame
                // v2 input box: `❯` between two `────` rules.
                r"(?:^|\n)[ \t]*─{3,}[ \t]*\r?\n[ \t]*❯[^\n]*\n[ \t]*─{3,}",
            ]),
            // Definitive evidence the REQUESTED session did not resume. Checked
            // before the success markers, because these screens are Claude UI.
            failure_regex: strings(&[
                "No conversation found",                       // `--resume <unknown-id>`
                "No conversations? (?:found|to resume)",       // empty history
                "Select a (?:session|conversation) to resume", // session picker
            ]),
            // Claude Code's own window title at launch: a spinner glyph, then
            // `Claude Code` (`✳ Claude Code`). Matched against the pane's
            // current OSC title only. The glyph is required because the shell
            // titles the window too, and a shell title can be a bare path such
            // as `~/Claude Code`.
            title_regex: strings(&[r"^[✳✢✶✻✽·*]\s*Claude Code$"]),
        },
        usage_limit_phrases: strings(&[
            "usage limit reached",
            "5-hour limit reached",
            "weekly limit reached",
            "hit your usage limit",
            "out of extra usage",
            "session limit reached",
            // Generic catch-alls last: specific phrases above win the label.
            "limit reached",
            "out of usage",
        ]),
        transcript: TranscriptSpec::JsonlUnderConfigDir {
            subdir: "projects".to_string(),
        },
        structured_lane: StructuredLane::ClaudeStreamJson,
        typed_permission: CapabilityState::Supported,
        turn_boundary_event: CapabilityState::Supported,
        rate_limit_event: CapabilityState::Supported,
        // Not lifted from any site yet: recorded as unknown, not guessed.
        trust_dialog: TrustDialog::Unknown,
        auth: Vec::new(),
        install: InstallCommands::default(),
        restore_tier: RestoreTier::Full,
        notes: strings(&[
            "verified_version covers the stream-json capabilities, probed on Linux against \
             2.1.285 (Phase 2). The handshake markers were last documented against the 2.1.175 \
             resume-size picker; the v2 logo, rule-prompt-rule frame and window-title \
             markers against 2.1.286's first paint of a resumed session (Linux, 51/53 live \
             panes verified). Windows and macOS are unverified.",
            "typed_permission: control_request{subtype:can_use_tool} arrives only without a \
             bypass mode and with the hidden flag `--permission-prompt-tool stdio` (2.1.285 \
             --help lists `--permission-prompts host|none` instead). The CLI accepts only the \
             SDK's nested control_response shape.",
            "turn_boundary_event: system/session_state_changed (idle|running|requires_action) \
             is emitted only when the child env sets CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1. \
             Without it the event never arrives, so a spawn that does not set it must treat \
             the capability as Unknown.",
            "rate_limit_event: arrives once per turn by default. Only status `allowed` was \
             observed; the spelling of a limited status is unknown.",
            "Agent teams under tmux: with TMUX set, the default `--teammate-mode auto` splits \
             the caller's tmux window. `--teammate-mode in-process` (hidden flag, 2.1.285, \
             Linux) keeps teammates in the session. A PTY launch should pass it and strip \
             TMUX/TMUX_PANE. Not applicable on Windows (no tmux).",
        ]),
    }
}
