//! Embedded terminal system — PTY-backed terminal sessions.
//!
//! Provides full terminal emulation inside the runner via `portable-pty`.
//! Each session spawns a native shell (PowerShell on Windows, $SHELL on Unix)
//! with proper environment for running Claude CLI and other dev tools.

pub mod account_migration;
pub mod auto_response;
pub mod auto_response_fleet;
pub mod claude_resume_sniff;
pub mod commit_report;
pub mod context_watcher;
pub mod coord_warn;
pub mod grid;
pub mod interceptor;
pub mod manager;
pub mod output_scan;
mod scan_gate;
pub mod scan_interval;
pub mod session;
pub mod transcript;
pub mod transcript_watcher;
pub mod types;
pub mod usage_limit;
pub mod vt_sanitize;

pub use manager::TerminalManager;

/// The attributable source marker prefixed to [`runner_context`]'s briefing.
///
/// Shape: `[source: <package>/runner_context@<version>+<git-sha>]`. The git SHA
/// comes from `QONTINUI_GIT_SHA` (baked by `build.rs`, the same stamp `/health`
/// and the fleet drift check report) because `CARGO_PKG_VERSION` alone moves
/// only once per release — every locally-built runner on the fleet would
/// otherwise claim the same version. On a source-tarball build with no git the
/// SHA component is the literal `unknown`.
///
/// Exposed so consumers can recognize the marker without retyping the literal.
pub const RUNNER_CONTEXT_SOURCE_MARKER: &str = concat!(
    "[source: ",
    env!("CARGO_PKG_NAME"),
    "/runner_context@",
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("QONTINUI_GIT_SHA"),
    "]"
);

/// The canonical "you are inside the Qontinui Runner" briefing appended to the
/// system prompt of every `claude` session the runner hosts.
///
/// This is the SINGLE SOURCE OF TRUTH for the briefing text. Both launch paths
/// render it from here so they can never drift:
///   - Interactive panes (a human types `claude`) read it via the
///     `QONTINUI_RUNNER_CONTEXT` env var that [`session`] injects at spawn; the
///     `shell-integration.{ps1,bash,zsh}` wrapper passes it to
///     `--append-system-prompt`.
///   - Autonomous direct-exec spawns (gate continuations, fleet/batch, the
///     looping-agent supervisor) never source shell integration, so they
///     inject it into the argv directly via
///     `agent_runtime::build_continuation_claude_command`.
///
/// Pull-first lean protocol (session-autonomy-fabric Phase 5): this briefing
/// carries PROTOCOL + LINKS, never policy content. Policy/playbook bodies live
/// in coord's versioned prompt documents and are fetched on demand, so editing
/// a policy never requires a runner release. It also carries NO narration
/// instructions — transparency is structural (tool calls land in the
/// transcript).
///
/// It deliberately carries NO tenant/agent identity — that is an RCE-class
/// invariant (a prompt must never cross tenants) and is deferred to the vetted
/// session-identity fabric plan. Only tenant-agnostic protocol guidance lives
/// here.
///
/// Attributable source marker (part of the contract): the FIRST line of the
/// appended briefing is always [`RUNNER_CONTEXT_SOURCE_MARKER`]. An instruction
/// with no attributable source cannot mandate or forbid agent behavior (e.g.
/// spawns — incident coord #1242), so every consumer of this briefing can trace
/// it back to this function and the build that emitted it. Both delivery paths
/// above inherit the marker automatically because they render from this single
/// function. Note this is the first line of the text the runner *appends* — the
/// harness's own system prompt still precedes it.
pub fn runner_context(api_port: u16) -> String {
    // Coord HTTP base for the tool-less fallback links. Resolved the same way
    // every coord proxy route resolves it (COORD_HTTP_URL env → active
    // profile → localhost fallback) so the prompt never disagrees with the
    // runner's own coord client.
    let coord_url = crate::coord_mcp::coord_base_url();
    format!(
        "{RUNNER_CONTEXT_SOURCE_MARKER}
You are running inside the Qontinui Runner — an autonomous multi-agent \
development environment. Runner HTTP API: http://127.0.0.1:{api_port}.

You work autonomously. No human is watching this session; do not wait for \
human replies or pause for approval you can resolve yourself.

Policies and playbooks are pull-first documents, not baked into this prompt. \
Discover them via the coord MCP tools: coord_list_prompt_documents (names + \
descriptions, cheap) and coord_get_prompt_document (full body on demand). \
HTTP fallback if those tools are unavailable: \
GET {coord_url}/coord/prompt-documents (list, optional ?kind= filter) and \
GET {coord_url}/coord/prompt-documents/{{kind}}/{{name}}.

When you would ask the user a question: fetch the relevant policy and DECIDE, \
recording it via coord_request_policy / coord_record_decision. Only a \
question a human must answer goes to coord_ask_question — after asking, you \
will be left alone until it is answered.

When no policy clause covers a decision and you apply a category-default tier \
to proceed, report the gap so a clause can be authored: \
coord_ask_question(policy_gap={{category, proposed_clause, tier_applied}}). \
With tier_applied set it records non-blocking (pre-answered) — you do not wait.

Report status transitions via coord_report_status \
(working | blocked | waiting_human | finished). Set finished only after \
cleanup (worktrees, branches) is done.

Before starting new work, check the communal work ledger so you do not \
duplicate a peer: coord_who_is_working_on, then coord_declare_intent to \
record your own scope.

If context runs low, act BEFORE exhaustion: request a handoff \
(coord_request_handoff) or spawn a continuation session seeded with a \
summary via POST http://127.0.0.1:{api_port}/sessions/spawn.",
    )
}

/// Strip ANSI escape sequences from text for readable scrollback previews.
///
/// Handles CSI (`ESC [ ... letter`) and OSC (`ESC ] ... BEL`) sequences plus
/// generic two-char escapes. Preserves printable chars and `\n` / `\r` / `\t`.
/// Used by both the Tauri command surface (`commands::terminal`) and the MCP
/// HTTP API (`mcp::terminals`) to produce human-readable terminal output.
pub fn strip_ansi(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip CSI sequences: ESC [ ... letter
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            // Skip OSC sequences: ESC ] ... BEL
            } else if chars.peek() == Some(&']') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c == '\x07' {
                        break;
                    }
                }
            } else {
                // Skip next char (two-char escape)
                chars.next();
            }
        } else if ch >= ' ' || ch == '\n' || ch == '\r' || ch == '\t' {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{runner_context, RUNNER_CONTEXT_SOURCE_MARKER};

    /// The attributability contract: the briefing's FIRST line is always the
    /// source marker naming this package, its version and the build's git SHA,
    /// so any consumer of the injected system prompt can trace the instruction
    /// back to its origin (incident coord #1242).
    #[test]
    fn runner_context_starts_with_attributable_source_marker() {
        let briefing = runner_context(9876);
        assert_eq!(
            briefing.lines().next(),
            Some(RUNNER_CONTEXT_SOURCE_MARKER),
            "the source marker must be the first line of the briefing"
        );
    }

    /// The marker must actually discriminate builds — a bare package/version
    /// stamp moves only once per release, so every locally-built runner on the
    /// fleet would attribute to the same string. Guards the `+<git-sha>`
    /// component against being dropped back to `CARGO_PKG_VERSION` alone.
    #[test]
    fn source_marker_carries_build_identity() {
        let expected = format!(
            "[source: {}/runner_context@{}+{}]",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            env!("QONTINUI_GIT_SHA"),
        );
        assert_eq!(RUNNER_CONTEXT_SOURCE_MARKER, expected);
    }
}
