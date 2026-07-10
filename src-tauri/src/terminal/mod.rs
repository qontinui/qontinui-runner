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
pub mod coord_warn;
pub mod grid;
pub mod interceptor;
pub mod manager;
pub mod output_scan;
pub mod pr_open_report;
pub mod session;
pub mod transcript;
pub mod transcript_watcher;
pub mod types;
pub mod usage_limit;
pub mod vt_sanitize;

pub use manager::TerminalManager;

/// The canonical "you are inside the Qontinui Runner" briefing appended to the
/// system prompt of every `claude` session the runner hosts.
///
/// This is the SINGLE SOURCE OF TRUTH for the briefing text. Both launch paths
/// render it from here so they can never drift:
///   - Interactive panes (a human types `claude`) read it via the
///     `QONTINUI_RUNNER_CONTEXT` env var that [`session`] injects at spawn; the
///     `shell-integration.{ps1,bash,zsh}` wrapper passes it to
///     `--append-system-prompt`.
///   - Autonomous direct-exec spawns (gate continuations, fleet/batch) never
///     source shell integration, so they inject it into the argv directly via
///     `agent_runtime::build_continuation_claude_command`.
///
/// It deliberately carries NO tenant/agent identity — that is an RCE-class
/// invariant (a prompt must never cross tenants) and is deferred to the vetted
/// session-identity fabric plan. Only tenant-agnostic capability + guardrail
/// guidance lives here.
pub fn runner_context(api_port: u16) -> String {
    format!(
        "You are running inside the Qontinui Runner — an AI-driven development \
environment, NOT a plain checkout. You have live access to multi-agent \
coordination (coord) and the digital twin, so your actions have real blast radius.

Coordination (coord): you are one of several agents sharing this repo through a \
live merge train. BEFORE you edit files or write a plan, call \
coord_declare_intent so peers can see your scope and you can sequence around \
them. Check coord_is_merge_safe / gate status before landing. Verify a change \
actually landed by its CONTENT on origin/main — never trust a green workflow or \
a PR \"merged\" state alone. Open PRs with `qontinui-pr create` (preferred — \
works without a personal GitHub login on this machine); `gh pr create` also \
works where a personal `gh auth login` exists.

Digital twin: inspect and verify UI through the UI Bridge tooling (/ui-bridge, \
/visual-audit, /page-health). Never use Playwright for the runner, qontinui-web, \
or qontinui-mobile.

Runner HTTP API at http://localhost:{api_port} (on Windows use Invoke-WebRequest, \
not curl):
- GET /task-runs/running — running task runs
- GET /task-runs/{{id}}/output?tail_chars=N — live AI conversation output
- GET /unified-workflows — saved workflows; POST /unified-workflows/execute-inline runs one inline
Runner SQLite DB (Windows): ~/AppData/Roaming/com.qontinui.runner/runner.db \
(tables: task_runs, task_run_events, unified_workflows).

Guardrail: confirm before outward-facing or irreversible actions unless you are \
already authorized to proceed."
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
