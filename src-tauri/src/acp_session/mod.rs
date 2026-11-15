//! The runner's Agent Client Protocol (ACP) **client** lane.
//!
//! Plan `2026-11-13-acp-client-lane-for-acp-native-clis`. ACP is the one
//! protocol Gemini CLI, OpenCode, Goose, Qwen Code and (through `codex-acp` /
//! `pi-acp`) Codex and pi speak, so one client here lets the runner run a
//! HEADLESS coord continuation under any of them. It is an additional lane, not
//! a replacement: Claude stays on the PTY TUI / `claude -p` path (ACP would drop
//! the hooks the runner depends on), and nothing here is reachable except from
//! the headless continuation arm in `agent_runtime.rs` when a spawn asks for
//! `harness: {"acp": "<id>"}`.
//!
//! ## Shape
//!
//! * [`AcpLaunch`] describes the child; [`spawn_acp_agent`] spawns it as a
//!   `tokio::process::Command` run through
//!   [`crate::agent_runtime::finalize_headless_child_env`], so the credential
//!   scrub and the non-interactive git posture are identical to the Claude
//!   headless path. (The crate's own `process` feature spawner can only ADD env
//!   to a child, never remove it, which is why it is not used.)
//! * [`AcpSession`] owns the connection: `initialize`, `session/new`, then
//!   [`AcpSession::prompt`] per turn and [`AcpSession::close`], which ends the
//!   session and reaps the child. An ACP agent does not exit at `end_turn`, so
//!   the runner must do both itself.
//! * `map` turns `session/update` notifications into transcript lines and usage
//!   events; an update it cannot type is preserved verbatim, never dropped.
//!
//! ## What the protocol cannot carry (and how each is handled)
//!
//! * No Stop hook: the continuation verdict is derived by the caller from the
//!   [`AcpStopReason`] of each prompt plus the gate's self-reported outcome
//!   (plan Design decision 1; `agent_runtime::acp_continuation`).
//! * No `--session-id` pin: the agent mints the id at `session/new`; it is
//!   recorded in [`SessionInfo::agent_session_id`] and the runner-minted run id
//!   stays the join key (`IdentitySource::ReadBack`).
//! * Briefing: sent as `session/new` `systemPrompt` when the agent advertises it,
//!   otherwise prepended to the FIRST prompt ([`BriefingDelivery::PromptPrefix`]).

mod client;
#[cfg(test)]
pub(crate) mod fake_agent;
mod map;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use client::start_acp_session_over;
pub(crate) use client::{
    build_acp_command, spawn_acp_agent, AcpSession, McpChooser, PermissionPolicy, SessionConfig,
};
pub(crate) use map::AcpUsage;

use std::path::{Path, PathBuf};

/// How to launch one ACP agent child.
#[derive(Debug, Clone)]
pub(crate) struct AcpLaunch {
    /// Absolute path of the agent executable (see [`resolve_program`]).
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Extra environment, applied before the runner's own finalize step (which
    /// scrubs credential values last, so nothing here can reintroduce one it
    /// names).
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

/// A coord-mcp server handed to the agent on `session/new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpEntry {
    /// HTTP transport; only usable when the agent advertises `mcpCapabilities.http`.
    Http {
        name: String,
        url: String,
        headers: Vec<(String, String)>,
    },
    /// Stdio transport; mandatory for every ACP agent.
    Stdio {
        name: String,
        command: PathBuf,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
}

/// What the agent advertised on `initialize` that this lane acts on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AgentCaps {
    /// `sessionCapabilities.systemPrompt`.
    pub system_prompt: bool,
    /// `mcpCapabilities.http`.
    pub mcp_http: bool,
    /// `loadSession` (Phase 4 uses it; recorded now).
    pub load_session: bool,
}

/// How the runner briefing reached the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BriefingDelivery {
    /// `session/new` `systemPrompt`.
    SystemPrompt,
    /// Prepended to the first prompt (the agent has no `systemPrompt`).
    PromptPrefix,
    /// There was no briefing to deliver.
    None,
}

impl BriefingDelivery {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SystemPrompt => "system_prompt",
            Self::PromptPrefix => "prompt_prefix",
            Self::None => "none",
        }
    }
}

/// What a `session/new` produced.
#[derive(Debug, Clone)]
pub(crate) struct SessionInfo {
    /// The id the AGENT minted.
    pub agent_session_id: String,
    pub caps: AgentCaps,
    pub briefing_delivery: BriefingDelivery,
    /// Whether a coord-mcp server was handed to the agent.
    pub mcp_delivered: bool,
}

/// `PromptResponse.stopReason`, or a value this crate version does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpStopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    /// A stop reason added to the protocol after this build; carried by its
    /// debug spelling so it is never mistaken for `end_turn`.
    Other(String),
}

impl AcpStopReason {
    /// The wire spelling, which is also what `work_unreported` details carry.
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
            Self::Other(s) => s,
        }
    }
}

/// The result of one `session/prompt`.
#[derive(Debug, Clone)]
pub(crate) struct PromptOutcome {
    pub stop_reason: AcpStopReason,
    /// Notifications received during the turn.
    pub update_count: u32,
    /// How many of them were preserved raw as `unknown_update`.
    pub unknown_updates: u32,
    /// The last `usage_update` of the turn, if any.
    pub last_usage: Option<AcpUsage>,
}

/// Why an ACP operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpErrorKind {
    /// The child could not be spawned.
    Spawn,
    /// The child exited / the stream closed while a request was outstanding.
    TransportLost,
    /// The agent answered a request with an error, or violated the protocol.
    Protocol,
    /// The turn exceeded its wall-clock bound and did not stop after a cancel.
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpError {
    pub kind: AcpErrorKind,
    pub detail: String,
}

impl AcpError {
    pub(crate) fn new(kind: AcpErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// The token a `work_unreported` / `spawn_failed` detail leads with.
    pub(crate) fn token(&self) -> &'static str {
        match self.kind {
            AcpErrorKind::Spawn => "acp_spawn_failed",
            AcpErrorKind::TransportLost => "transport_lost",
            AcpErrorKind::Protocol => "acp_protocol_error",
            AcpErrorKind::TimedOut => "acp_turn_timed_out",
        }
    }
}

impl std::fmt::Display for AcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.token(), self.detail)
    }
}

impl std::error::Error for AcpError {}

/// Resolve an `acp_agents` `program` to an absolute launchable executable, or
/// `None`.
///
/// An absolute path must be a regular file (with an execute bit on unix). A bare
/// name is searched along `PATH` (and, on Windows, `PATHEXT`-style `.exe`/`.com`
/// candidates, the two `CreateProcessW` can launch directly). Blocking
/// filesystem stats: call from a blocking context.
pub(crate) fn resolve_program(program: &str) -> Option<PathBuf> {
    resolve_program_in(program, std::env::var_os("PATH").as_deref())
}

pub(crate) fn resolve_program_in(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let program = program.trim();
    if program.is_empty() {
        return None;
    }
    let p = Path::new(program);
    if p.is_absolute() {
        return is_launchable_file(p).then(|| p.to_path_buf());
    }
    // A relative path with a separator is not a PATH lookup and is ambiguous
    // about its base: refuse rather than resolve against the runner's cwd.
    if program.contains('/') || program.contains('\\') {
        return None;
    }
    for dir in std::env::split_paths(path_var?) {
        #[cfg(windows)]
        {
            for ext in ["", ".exe", ".com"] {
                let cand = dir.join(format!("{program}{ext}"));
                if is_launchable_file(&cand) {
                    return Some(cand);
                }
            }
        }
        #[cfg(not(windows))]
        {
            let cand = dir.join(program);
            if is_launchable_file(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

fn is_launchable_file(path: &Path) -> bool {
    let Ok(md) = std::fs::metadata(path) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[cfg(unix)]
    fn make_exe(dir: &Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn resolve_program_finds_a_bare_name_on_path_and_checks_the_execute_bit() {
        let d = tempfile::tempdir().unwrap();
        let exe = make_exe(d.path(), "acp-fake", 0o755);
        make_exe(d.path(), "acp-noexec", 0o644);
        let path = std::env::join_paths([d.path()]).unwrap();
        assert_eq!(
            resolve_program_in("acp-fake", Some(&path)),
            Some(exe.clone())
        );
        assert_eq!(resolve_program_in("acp-noexec", Some(&path)), None);
        assert_eq!(resolve_program_in("acp-absent", Some(&path)), None);
        assert_eq!(resolve_program_in("acp-fake", None), None);
        // An absolute path is checked in place.
        assert_eq!(
            resolve_program_in(exe.to_str().unwrap(), None),
            Some(exe.clone())
        );
        // A relative path with a separator is refused, never resolved on cwd.
        assert_eq!(resolve_program_in("./acp-fake", Some(&path)), None);
        assert_eq!(resolve_program_in("  ", Some(&path)), None);
    }

    #[test]
    fn error_tokens_and_display_are_stable() {
        let e = AcpError::new(AcpErrorKind::TransportLost, "child exited");
        assert_eq!(e.token(), "transport_lost");
        assert_eq!(e.to_string(), "transport_lost: child exited");
        assert_eq!(AcpStopReason::MaxTurnRequests.as_str(), "max_turn_requests");
        assert_eq!(AcpStopReason::Other("x".into()).as_str(), "x");
    }
}
