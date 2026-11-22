//! Per-session resumability under a `runner-restart` barrier (plan
//! `2026-09-29-quiet-on-demand-…`, D2 / D5) — the `resume` block of
//! `GET /restart-readiness`.
//!
//! A runner-hosted session is **resumable** when a restart followed by the boot
//! restore (`claude --resume <id>`) would bring it back with nothing lost:
//!
//! 1. its turn has ended — the per-process ACTIVITY axis reads
//!    [`Activity::Idle`]. This is the SAME classification
//!    `live_claude.by_activity` counts
//!    ([`crate::session::claude_activity::classify`], via
//!    `restart_readiness::process_activity`): the pane observation where it is
//!    decisive (a recent sideband `working` or a busy grid is `working`; a
//!    sideband not-working + idle grid + positively no children is `idle`),
//!    else Claude Code's own `sessions/<pid>.json` record. One census, one set
//!    of blind spots — this module keeps no idle heuristic of its own. `working`
//!    is a `busy` straggler, `stale` (Claude Code says busy but nothing moved
//!    for 30 min — not a turn boundary) a `stale` one, `unknown` an `unknown`
//!    one;
//! 2. it is not waiting on a permission prompt or a question — Claude Code's
//!    own `sessions/<pid>.json` record does not read `waiting` (the activity
//!    axis counts `waiting` as `idle`, since no turn runs, but the prompt is
//!    lost on resume, so here it is a `waiting_human` straggler), AND the
//!    pane's last OSC 9999 self-report is not `waiting_human`. A permission
//!    menu paints the same `❯` caret an idle prompt does, so a record that
//!    could not be read (missing, unparseable, ambiguous, a reused pid, or a
//!    read that timed out or was skipped) and a pane that NEVER reported are
//!    both `unknown`, never idle;
//! 3. it has no descendants other than its declared MCP servers — those are
//!    recreated by `--resume`; anything else (a background shell, a build, a
//!    nested subagent) dies with the restart. The servers are resolved from the
//!    session's own MCP config (`--mcp-config` on its argv, `.mcp.json` from
//!    its cwd upward); a child the config does not account for — or any child
//!    at all when no config resolves — is non-MCP;
//! 4. its lifecycle record would be selected by `restorable_records` AND the
//!    restore census would mark it `restorable` (confirmed + transcript);
//! 5. the runner holds no deferred autonomous prompt for it IN MEMORY (an
//!    account-migration prompt-when-idle watcher, a scheduled or
//!    barrier-deferred auto-response — [`crate::quiet_barrier::pending`]).
//!    Such a prompt dies with the restart, so the session is a
//!    `pending_autonomous_prompt` straggler until it is delivered or dropped;
//! 6. it is not the barrier's own requester: a requester hosted by THIS runner
//!    would have its own turn ended by the restart it asked for, so it is a
//!    `requester` straggler (run the restart from outside this runner).
//!
//! Anything that cannot be read is `unknown` and is a straggler — never counted
//! as resumable [policy: `verification-and-evidence` `silent-empty-is-unknown`].
//! A session whose coord work axis reads `finished` is neither: it has no work
//! to protect and is not restored, so it is counted apart.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use crate::session::claude_activity::Activity;

/// The pane's last OSC 9999 state, as the readiness reader saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebandRead {
    Reported(String),
    NeverReported,
    Unreadable,
}

/// What Claude Code's own `sessions/<pid>.json` record says about a pending
/// permission prompt or question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordWaitingRead {
    /// The record reads `waiting`.
    Waiting,
    /// The record was read and reads some other status.
    NotWaiting,
    /// The record could not be read — missing, unparseable, ambiguous, a
    /// reused pid, or a read that timed out or was skipped (named).
    Unreadable(String),
}

/// The session's descendant processes, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Descendants {
    /// No children, or only children its MCP config declares.
    McpOnly,
    /// At least one child outside the declared MCP servers (named).
    NonMcp(Vec<String>),
    /// The process table could not answer.
    Unknown(String),
}

/// Would the boot restore bring this session back?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreSelection {
    /// Selected by `restorable_records` and `restorable` (confirmed +
    /// transcript present).
    Restorable,
    /// Selected, but not identity-restorable.
    NotRestorable,
    /// Not selected by `restorable_records` at all.
    NotSelected,
    /// The lifecycle store could not be read.
    Unknown(String),
}

/// Everything [`classify`] reads for one top-level terminal-hosted `claude`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInput {
    /// `None` = unattributed or ambiguous (two records claim the pid).
    pub session_id: Option<String>,
    pub terminal_id: Option<String>,
    pub pid: u32,
    /// The coord work axis reads exactly `finished`.
    pub finished: bool,
    /// The process's class on the activity axis — the same value
    /// `live_claude.by_activity` counts for it.
    pub activity: Activity,
    /// Claude Code's own record for the process, on the `waiting` question.
    /// Resume-only: the activity axis calls a `waiting` record `idle`.
    pub record: RecordWaitingRead,
    /// This session is the open barrier's own requester.
    pub is_requester: bool,
    /// Deferred autonomous prompts the runner holds in memory for this
    /// session (from [`crate::quiet_barrier::pending`]); they die with the
    /// restart.
    pub pending_autonomous: Vec<String>,
    pub sideband: SidebandRead,
    pub descendants: Descendants,
    pub restore: RestoreSelection,
}

/// One session (or process) that is not at a resumable safe point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Straggler {
    /// The Claude session id, when attributable.
    pub session_id: Option<String>,
    pub terminal_id: Option<String>,
    pub pid: Option<u32>,
    /// A stable token: `busy` | `stale` | `waiting_human` | `non_mcp_descendants` |
    /// `not_restorable` | `not_selected` | `pending_autonomous_prompt` |
    /// `requester` | `headless` | `ai_session` | `unknown`. A Phase 5 caller may corroborate an `unknown` with evidence
    /// the runner does not have (the session census); every other class is a
    /// positive observation.
    pub class: &'static str,
    pub reason: String,
}

impl Straggler {
    /// A straggler that is not a terminal-hosted session.
    pub fn other(pid: Option<u32>, class: &'static str, reason: impl Into<String>) -> Self {
        Self {
            session_id: None,
            terminal_id: None,
            pid,
            class,
            reason: reason.into(),
        }
    }
}

/// The `resume` block of `GET /restart-readiness`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResumeBlock {
    /// The open `runner-restart` barrier this was computed under.
    pub barrier_id: String,
    /// Top-level terminal-hosted sessions at a resumable safe point.
    pub resumable_count: usize,
    /// `stragglers.len()` — everything NOT at a resumable safe point.
    pub blocking_count: usize,
    /// Sessions whose coord work axis reads `finished`: no work to protect,
    /// not restored, still killed by the restart.
    pub finished_count: usize,
    /// The wake gate's DECISION, not an observation of the doors: `true` when
    /// [`crate::quiet_barrier::wake_decision`], asked about an anonymous
    /// `Autonomous` wake under the barrier state this response was computed
    /// from, answers `Defer`. The block is only built under an open barrier,
    /// so in practice it reads `true`. That the autonomous doors route through
    /// that function — the PTY funnel (`PtyWriteCaller::wake_class`), the SDK
    /// message funnel and its queue drain (`SdkMessageCaller::wake_class`) and
    /// both Stop-hook arms — is a property of the code, pinned by tests, not
    /// something this field checks at runtime.
    pub wake_paths_gated: bool,
    /// Claude session ids the boot restore is expected to bring back — the
    /// `restorable` rows of the restore census's own expected-set projection.
    pub expected_restore_set: Vec<String>,
    pub stragglers: Vec<Straggler>,
}

/// PURE: `Ok(())` when `input` is at a resumable safe point, else the
/// straggler class and reason. Callers skip `finished` sessions first.
pub fn classify(input: &SessionInput) -> Result<(), (&'static str, String)> {
    if input.session_id.is_none() {
        return Err((
            "unknown",
            "unknown: the process is not attributable to exactly one lifecycle record".to_string(),
        ));
    }
    if input.is_requester {
        return Err((
            "requester",
            "this is the barrier's own requester session, hosted by this runner: the restart \
             would end its turn — run the restart from outside this runner"
                .into(),
        ));
    }
    if !input.pending_autonomous.is_empty() {
        return Err((
            "pending_autonomous_prompt",
            format!(
                "the runner holds a deferred autonomous prompt for this session in memory, which \
                 the restart would drop: {}",
                input.pending_autonomous.join(", ")
            ),
        ));
    }
    match input.activity {
        Activity::Idle => {}
        Activity::Working => {
            return Err(("busy", "turn in progress (activity axis: `working`)".into()))
        }
        Activity::Stale => {
            return Err((
                "stale",
                "activity axis: `stale` — Claude Code reports `busy`/`shell` but no message moved \
                 in 30 min, so the session is not at a turn boundary"
                    .into(),
            ))
        }
        Activity::Unknown => {
            return Err((
                "unknown",
                "unknown: the activity axis could not classify the process (no decisive pane \
                 observation and no usable Claude Code session record)"
                    .into(),
            ))
        }
    }
    match &input.record {
        RecordWaitingRead::NotWaiting => {}
        RecordWaitingRead::Waiting => {
            return Err((
                "waiting_human",
                "waiting on a permission prompt or question (Claude Code's session record reads \
                 `waiting`) — lost on resume"
                    .into(),
            ))
        }
        RecordWaitingRead::Unreadable(why) => {
            return Err((
                "unknown",
                format!(
                    "unknown: Claude Code's session record could not be read ({why}), so a \
                     pending permission prompt cannot be ruled out"
                ),
            ))
        }
    }
    match &input.sideband {
        SidebandRead::Reported(state) if state == "waiting_human" => {
            return Err((
                "waiting_human",
                "waiting on a permission prompt or question (sideband `waiting_human`) — lost on resume"
                    .into(),
            ))
        }
        SidebandRead::Reported(_) => {}
        SidebandRead::NeverReported => {
            return Err((
                "unknown",
                "unknown: the pane never sent an OSC 9999 status, so a waiting permission prompt \
                 cannot be ruled out (it paints the same caret as an idle prompt)"
                    .into(),
            ))
        }
        SidebandRead::Unreadable => {
            return Err(("unknown", "unknown: the pane's status slot is unreadable".into()))
        }
    }
    match &input.descendants {
        Descendants::McpOnly => {}
        Descendants::NonMcp(names) => {
            return Err((
                "non_mcp_descendants",
                format!(
                    "has descendant process(es) its MCP config does not declare, which a restart \
                     kills: {}",
                    names.join(", ")
                ),
            ))
        }
        Descendants::Unknown(why) => return Err(("unknown", format!("unknown: {why}"))),
    }
    match &input.restore {
        RestoreSelection::Restorable => Ok(()),
        RestoreSelection::NotRestorable => Err((
            "not_restorable",
            "its record is unconfirmed or its transcript is missing, so `--resume` cannot bring it back"
                .into(),
        )),
        RestoreSelection::NotSelected => Err((
            "not_selected",
            "its lifecycle record would not be selected by the boot restore".into(),
        )),
        RestoreSelection::Unknown(why) => Err(("unknown", format!("unknown: {why}"))),
    }
}

/// PURE: fold the per-session verdicts, plus stragglers from outside the
/// terminal plane, into the `resume` block.
pub fn build_block(
    barrier_id: &str,
    sessions: &[SessionInput],
    mut other_stragglers: Vec<Straggler>,
    expected_restore_set: Vec<String>,
    wake_paths_gated: bool,
) -> ResumeBlock {
    let mut resumable_count = 0usize;
    let mut finished_count = 0usize;
    let mut stragglers: Vec<Straggler> = Vec::new();
    for s in sessions {
        if s.finished {
            finished_count += 1;
            continue;
        }
        match classify(s) {
            Ok(()) => resumable_count += 1,
            Err((class, reason)) => stragglers.push(Straggler {
                session_id: s.session_id.clone(),
                terminal_id: s.terminal_id.clone(),
                pid: Some(s.pid),
                class,
                reason,
            }),
        }
    }
    stragglers.append(&mut other_stragglers);
    ResumeBlock {
        barrier_id: barrier_id.to_string(),
        resumable_count,
        blocking_count: stragglers.len(),
        finished_count,
        wake_paths_gated,
        expected_restore_set,
        stragglers,
    }
}

// ---------------------------------------------------------------------------
// MCP servers and descendants
// ---------------------------------------------------------------------------

/// One stdio MCP server a config declares: the process it spawns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSig {
    pub command: String,
    pub args: Vec<String>,
}

/// The stdio servers in one MCP config document (`{"mcpServers": {...}}`).
/// Remote (`url`) servers spawn no process and are skipped.
pub fn servers_from_config_doc(doc: &serde_json::Value) -> Vec<McpServerSig> {
    let Some(servers) = doc.get("mcpServers").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    servers
        .values()
        .filter_map(|entry| {
            let command = entry.get("command")?.as_str()?.trim();
            if command.is_empty() {
                return None;
            }
            let args = entry
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some(McpServerSig {
                command: command.to_string(),
                args,
            })
        })
        .collect()
}

/// The `--mcp-config` values on a `claude` command line. On Windows the line
/// is WMI's raw `CommandLine`, quotes included, so it is split by the Windows
/// argv rule ([`split_windows_argv`]). Elsewhere it is `/proc/<pid>/cmdline`'s
/// argv joined with spaces and NO quoting, so it is split on whitespace — a
/// shell-like splitter would eat the quotes inside an inline-JSON value. A
/// value containing spaces cannot be recovered from that join and simply fails
/// to resolve later.
pub fn mcp_config_values_from_cmdline(cmdline: &str) -> Vec<String> {
    let owned: Vec<String> = if cfg!(windows) {
        split_windows_argv(cmdline)
    } else {
        cmdline.split_whitespace().map(str::to_string).collect()
    };
    let tokens: Vec<&str> = owned.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if let Some(v) = tok.strip_prefix("--mcp-config=") {
            if !v.is_empty() {
                out.push(v.to_string());
            }
        } else if tok == "--mcp-config" {
            // Variadic in Claude Code: every following non-flag token.
            let mut j = i + 1;
            while j < tokens.len() && !tokens[j].starts_with('-') {
                out.push(tokens[j].to_string());
                j += 1;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

fn basename_lower(s: &str) -> String {
    let s = strip_quotes(s);
    let base = s
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(s)
        .to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat"] {
        if let Some(stem) = base.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    base
}

/// `s` without one pair of surrounding `"…"` or `'…'` quotes (and outer
/// whitespace) — for declared `command`/`args` a config wrote pre-quoted.
fn strip_quotes(s: &str) -> &str {
    let t = s.trim();
    for q in ['"', '\''] {
        if t.len() >= 2 {
            if let Some(inner) = t.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
                return inner;
            }
        }
    }
    t
}

/// Split a Windows command line the way `CommandLineToArgvW` does: the program
/// name is delimited by quotes with no escaping; in every later argument,
/// whitespace separates outside quotes, `"` toggles quoting (`""` inside quotes
/// is a literal quote), `2n` backslashes before a `"` are `n` backslashes and
/// the quote toggles, `2n+1` are `n` backslashes and a literal quote, and
/// backslashes not before a quote are literal (so `C:\Program Files\…` paths
/// survive).
pub fn split_windows_argv(line: &str) -> Vec<String> {
    let c: Vec<char> = line.chars().collect();
    let n = c.len();
    let ws = |ch: char| ch == ' ' || ch == '\t';
    let mut out = Vec::new();
    let mut i = 0;
    while i < n && ws(c[i]) {
        i += 1;
    }
    if i >= n {
        return out;
    }
    let mut prog = String::new();
    let mut quoted = false;
    while i < n {
        if c[i] == '"' {
            quoted = !quoted;
        } else if !quoted && ws(c[i]) {
            break;
        } else {
            prog.push(c[i]);
        }
        i += 1;
    }
    out.push(prog);
    loop {
        while i < n && ws(c[i]) {
            i += 1;
        }
        if i >= n {
            break;
        }
        let mut arg = String::new();
        let mut quoted = false;
        while i < n {
            match c[i] {
                '\\' => {
                    let start = i;
                    while i < n && c[i] == '\\' {
                        i += 1;
                    }
                    let k = i - start;
                    if i < n && c[i] == '"' {
                        arg.extend(std::iter::repeat_n('\\', k / 2));
                        if k % 2 == 1 {
                            arg.push('"');
                            i += 1;
                        }
                    } else {
                        arg.extend(std::iter::repeat_n('\\', k));
                    }
                }
                '"' => {
                    if quoted && i + 1 < n && c[i + 1] == '"' {
                        arg.push('"');
                        i += 2;
                    } else {
                        quoted = !quoted;
                        i += 1;
                    }
                }
                ch if !quoted && ws(ch) => break,
                ch => {
                    arg.push(ch);
                    i += 1;
                }
            }
        }
        out.push(arg);
    }
    out
}

/// The platform's splitter for a CHILD process's command line: the Windows
/// argv rule on Windows (WMI returns the raw, quoted line); whitespace
/// elsewhere, because there the line is `/proc/<pid>/cmdline`'s argv joined
/// with spaces and NO quoting — a quote-aware splitter would mangle an
/// argument that merely contains `'`, `"` or `\` (an unbalanced `'` would
/// swallow the rest of the line). Same rule as
/// [`mcp_config_values_from_cmdline`].
pub fn split_cmdline(line: &str) -> Vec<String> {
    if cfg!(windows) {
        split_windows_argv(line)
    } else {
        line.split_whitespace().map(str::to_string).collect()
    }
}

/// Unwrap a `cmd /c …` (or `cmd.exe /d /s /c "…"`) launcher prefix — how
/// Windows usually runs `npx` — leaving the wrapped command's argv. A single
/// wrapped token that still holds whitespace (`/c "npx -y pkg"`) is split
/// again with `split`. Any other argv is returned unchanged.
fn unwrap_cmd_prefix(argv: Vec<String>, split: fn(&str) -> Vec<String>) -> Vec<String> {
    if argv.first().map(|p| basename_lower(p)).as_deref() != Some("cmd") {
        return argv;
    }
    let Some(c_at) = argv
        .iter()
        .skip(1)
        .position(|t| t.eq_ignore_ascii_case("/c") || t.eq_ignore_ascii_case("/k"))
        .map(|p| p + 1)
    else {
        return argv;
    };
    if !argv[1..c_at].iter().all(|t| t.starts_with('/')) {
        return argv;
    }
    let rest: Vec<String> = argv[c_at + 1..].to_vec();
    if rest.len() == 1 && rest[0].contains(char::is_whitespace) {
        return split(&rest[0]);
    }
    rest
}

/// Interpreters and launchers an MCP server's declared `command` may re-exec
/// through (`npx` → `node …/npx-cli.js <args>`, `uvx` → `python …`). A child
/// whose program is one of these matches a server whose `command` is also one
/// of these; any other program must be the declared command itself.
pub const KNOWN_LAUNCHERS: &[&str] = &[
    "node", "npx", "uvx", "uv", "python", "python3", "deno", "bun", "docker",
];

fn is_launcher(basename: &str) -> bool {
    KNOWN_LAUNCHERS.contains(&basename)
}

/// Does a child's command line belong to one of `sigs`? The line is split with
/// the platform's quote-aware rule ([`split_cmdline`]) and a `cmd /c` prefix is
/// unwrapped; then both must hold:
///
/// - **program:** the child's program basename is the declared `command`'s, or
///   both are [`KNOWN_LAUNCHERS`] (a launcher that re-execs through another);
/// - **argv:** every declared arg (surrounding quotes stripped) is present as
///   an EXACT argv token of the child — never a substring, so a background
///   `node server.js` does not pass for a server declared with
///   `["dist/server.js"]` or `["server"]`.
///
/// ⚠ **Limit, by construction:** a background job whose argv is IDENTICAL to a
/// declared server's (declared `node server.js`, job `node server.js`) cannot
/// be told apart from that server by its command line — nothing in it differs.
/// Such a job is counted as the MCP server.
pub fn cmdline_matches(cmdline: &str, sigs: &[McpServerSig]) -> bool {
    cmdline_matches_with(cmdline, sigs, split_cmdline)
}

/// [`cmdline_matches`] with an explicit splitter — so the Windows argv rule is
/// testable on every platform.
pub fn cmdline_matches_with(
    cmdline: &str,
    sigs: &[McpServerSig],
    split: fn(&str) -> Vec<String>,
) -> bool {
    let argv = unwrap_cmd_prefix(split(cmdline), split);
    let Some(program) = argv.first().map(|t| basename_lower(t)) else {
        return false;
    };
    let rest: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    sigs.iter().any(|sig| {
        let declared = basename_lower(&sig.command);
        let program_ok = program == declared || (is_launcher(&program) && is_launcher(&declared));
        if !program_ok {
            return false;
        }
        sig.args
            .iter()
            .map(|a| strip_quotes(a))
            .filter(|a| !a.is_empty())
            .all(|a| rest.contains(&a))
    })
}

/// PURE: classify the descendants of `pid`. `sigs` is `None` when the
/// session's MCP config could not be resolved, in which case every child is
/// non-MCP. A child whose command line is unreadable is non-MCP. The subtree of
/// a matched MCP server is the server's own (never inspected further).
pub fn classify_descendants(
    pid: u32,
    parent_map: &HashMap<u32, Vec<u32>>,
    cmdlines: &HashMap<u32, String>,
    sigs: Option<&[McpServerSig]>,
) -> Descendants {
    if parent_map.is_empty() {
        return Descendants::Unknown("the process table is unreadable".to_string());
    }
    let children = parent_map.get(&pid).cloned().unwrap_or_default();
    let mut non_mcp: Vec<String> = Vec::new();
    for child in children {
        let Some(cmd) = cmdlines.get(&child) else {
            non_mcp.push(format!("pid {child} (command line unreadable)"));
            continue;
        };
        let matched = sigs.is_some_and(|s| cmdline_matches(cmd, s));
        if !matched {
            let program = split_cmdline(cmd)
                .first()
                .map(|p| basename_lower(p))
                .unwrap_or_default();
            non_mcp.push(if sigs.is_none() {
                format!("pid {child} `{program}` (no MCP config resolved for this session)")
            } else {
                format!("pid {child} `{program}`")
            });
        }
    }
    if non_mcp.is_empty() {
        Descendants::McpOnly
    } else {
        Descendants::NonMcp(non_mcp)
    }
}

/// Resolve the stdio MCP servers a `claude` process was started with: every
/// `--mcp-config` value on its command line (a file path or inline JSON), plus
/// every `.mcp.json` from its cwd up to the filesystem root. `None` when no
/// source resolves, or when an explicit `--mcp-config` cannot be read — a
/// partially known config is not a known config.
pub fn resolve_mcp_servers(
    claude_cmdline: Option<&str>,
    cwd: Option<&str>,
) -> Option<Vec<McpServerSig>> {
    let mut sigs: Vec<McpServerSig> = Vec::new();
    let mut any_source = false;
    if let Some(cmd) = claude_cmdline {
        for value in mcp_config_values_from_cmdline(cmd) {
            let doc: serde_json::Value = if value.trim_start().starts_with('{') {
                serde_json::from_str(&value).ok()?
            } else {
                let bytes = std::fs::read(&value).ok()?;
                serde_json::from_slice(&bytes).ok()?
            };
            any_source = true;
            sigs.extend(servers_from_config_doc(&doc));
        }
    }
    if let Some(cwd) = cwd {
        let mut seen: HashSet<std::path::PathBuf> = HashSet::new();
        let mut dir: Option<&Path> = Some(Path::new(cwd));
        while let Some(d) = dir {
            let candidate = d.join(".mcp.json");
            if seen.insert(candidate.clone()) {
                if let Ok(bytes) = std::fs::read(&candidate) {
                    if let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        any_source = true;
                        sigs.extend(servers_from_config_doc(&doc));
                    }
                }
            }
            dir = d.parent();
        }
    }
    any_source.then_some(sigs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_input(id: &str) -> SessionInput {
        SessionInput {
            session_id: Some(id.to_string()),
            terminal_id: Some(format!("term-{id}")),
            pid: 100,
            finished: false,
            activity: Activity::Idle,
            record: RecordWaitingRead::NotWaiting,
            is_requester: false,
            pending_autonomous: vec![],
            sideband: SidebandRead::Reported("finished".to_string()),
            descendants: Descendants::McpOnly,
            restore: RestoreSelection::Restorable,
        }
    }

    #[test]
    fn an_idle_mcp_only_restorable_session_is_resumable() {
        assert_eq!(classify(&idle_input("a")), Ok(()));
    }

    #[test]
    fn every_failed_condition_names_its_class() {
        let cases: Vec<(SessionInput, &str)> = vec![
            (
                SessionInput {
                    session_id: None,
                    ..idle_input("a")
                },
                "unknown",
            ),
            (
                SessionInput {
                    activity: Activity::Working,
                    ..idle_input("a")
                },
                "busy",
            ),
            (
                SessionInput {
                    activity: Activity::Stale,
                    ..idle_input("a")
                },
                "stale",
            ),
            (
                SessionInput {
                    activity: Activity::Unknown,
                    ..idle_input("a")
                },
                "unknown",
            ),
            (
                SessionInput {
                    sideband: SidebandRead::Reported("waiting_human".into()),
                    ..idle_input("a")
                },
                "waiting_human",
            ),
            (
                SessionInput {
                    sideband: SidebandRead::NeverReported,
                    ..idle_input("a")
                },
                "unknown",
            ),
            (
                SessionInput {
                    sideband: SidebandRead::Unreadable,
                    ..idle_input("a")
                },
                "unknown",
            ),
            (
                SessionInput {
                    descendants: Descendants::NonMcp(vec!["pid 7 `bash`".into()]),
                    ..idle_input("a")
                },
                "non_mcp_descendants",
            ),
            (
                SessionInput {
                    descendants: Descendants::Unknown("x".into()),
                    ..idle_input("a")
                },
                "unknown",
            ),
            (
                SessionInput {
                    restore: RestoreSelection::NotRestorable,
                    ..idle_input("a")
                },
                "not_restorable",
            ),
            (
                SessionInput {
                    restore: RestoreSelection::NotSelected,
                    ..idle_input("a")
                },
                "not_selected",
            ),
            (
                SessionInput {
                    restore: RestoreSelection::Unknown("store".into()),
                    ..idle_input("a")
                },
                "unknown",
            ),
            (
                SessionInput {
                    record: RecordWaitingRead::Waiting,
                    ..idle_input("a")
                },
                "waiting_human",
            ),
            (
                SessionInput {
                    record: RecordWaitingRead::Unreadable("missing".into()),
                    ..idle_input("a")
                },
                "unknown",
            ),
            (
                SessionInput {
                    is_requester: true,
                    ..idle_input("a")
                },
                "requester",
            ),
            (
                SessionInput {
                    pending_autonomous: vec!["account_migration:continue_nudge".into()],
                    ..idle_input("a")
                },
                "pending_autonomous_prompt",
            ),
        ];
        for (input, class) in cases {
            let got = classify(&input).expect_err("not resumable");
            assert_eq!(got.0, class, "{input:?}");
        }
    }

    /// Plan D2: a session waiting on a permission prompt or question is lost
    /// on resume. The activity axis calls a `waiting` record `idle` (no turn
    /// runs), and the pane's sideband may say nothing more than "finished" —
    /// the record alone must still make it a `waiting_human` straggler.
    #[test]
    fn a_waiting_record_with_idle_activity_is_a_waiting_human_straggler() {
        let input = SessionInput {
            activity: Activity::Idle,
            record: RecordWaitingRead::Waiting,
            ..idle_input("w")
        };
        let (class, reason) = classify(&input).expect_err("not resumable");
        assert_eq!(class, "waiting_human");
        assert!(reason.contains("`waiting`"), "{reason}");
        let block = build_block("rr-1", &[input], vec![], vec![], true);
        assert_eq!(block.resumable_count, 0);
        assert_eq!(block.blocking_count, 1);
        assert_eq!(block.stragglers[0].class, "waiting_human");
        assert_eq!(block.stragglers[0].session_id.as_deref(), Some("w"));
    }

    #[test]
    fn build_block_counts_resumable_finished_and_stragglers_apart() {
        let sessions = vec![
            idle_input("a"),
            SessionInput {
                activity: Activity::Working,
                ..idle_input("b")
            },
            SessionInput {
                finished: true,
                activity: Activity::Working,
                ..idle_input("c")
            },
        ];
        let block = build_block(
            "rr-1",
            &sessions,
            vec![Straggler::other(Some(9), "headless", "headless")],
            vec!["a".into()],
            true,
        );
        assert_eq!(block.resumable_count, 1);
        assert_eq!(block.finished_count, 1);
        assert_eq!(block.blocking_count, 2);
        assert_eq!(block.stragglers[0].session_id.as_deref(), Some("b"));
        assert_eq!(block.stragglers[0].class, "busy");
        assert_eq!(block.stragglers[1].class, "headless");
        assert!(block.wake_paths_gated);
    }

    /// Item 3: an UNREADABLE record (missing, unparseable, a timed-out or
    /// skipped read) is never resumable — a permission prompt cannot be ruled
    /// out — while a read `NotWaiting` record lets an otherwise idle session
    /// through.
    #[test]
    fn an_unreadable_record_is_an_unknown_straggler_never_resumable() {
        for why in [
            "missing",
            "unparseable",
            "read timed out",
            "read skipped (in flight)",
        ] {
            let input = SessionInput {
                record: RecordWaitingRead::Unreadable(why.to_string()),
                ..idle_input("u")
            };
            let (class, reason) = classify(&input).expect_err("not resumable");
            assert_eq!(class, "unknown");
            assert!(reason.contains(why), "{reason}");
            let block = build_block("rr-1", &[input], vec![], vec![], true);
            assert_eq!(block.resumable_count, 0);
        }
        assert_eq!(classify(&idle_input("ok")), Ok(()));
    }

    /// Item 5: a deferred autonomous prompt held in memory dies with the
    /// restart, so the session is a `pending_autonomous_prompt` straggler
    /// naming the producer — even when every other condition holds.
    #[test]
    fn a_pending_in_memory_prompt_is_a_straggler_naming_its_producer() {
        let input = SessionInput {
            pending_autonomous: vec!["auto_response:rate-limit".into()],
            ..idle_input("p")
        };
        let (class, reason) = classify(&input).expect_err("not resumable");
        assert_eq!(class, "pending_autonomous_prompt");
        assert!(reason.contains("auto_response:rate-limit"), "{reason}");
    }

    /// Note 8: the requester gets its own class and the instruction.
    #[test]
    fn the_requester_is_its_own_straggler_class() {
        let input = SessionInput {
            is_requester: true,
            ..idle_input("r")
        };
        let (class, reason) = classify(&input).expect_err("not resumable");
        assert_eq!(class, "requester");
        assert!(
            reason.contains("run the restart from outside this runner"),
            "{reason}"
        );
    }

    /// `wake_paths_gated` is carried, not hard-coded.
    #[test]
    fn wake_paths_gated_is_what_the_caller_observed() {
        assert!(build_block("rr-1", &[], vec![], vec![], true).wake_paths_gated);
        assert!(!build_block("rr-1", &[], vec![], vec![], false).wake_paths_gated);
    }

    fn shim_sig() -> Vec<McpServerSig> {
        servers_from_config_doc(&serde_json::json!({
            "mcpServers": {
                "coord-mcp": {
                    "command": "python3",
                    "args": ["/home/u/.qontinui/coord-mcp-shim.py", "--credential", "/tmp/c"]
                },
                "remote": { "type": "http", "url": "http://127.0.0.1:9876/mcp" },
                "bare": { "command": "/usr/local/bin/my-server" }
            }
        }))
    }

    #[test]
    fn servers_from_config_doc_keeps_stdio_servers_only() {
        let sigs = shim_sig();
        assert_eq!(sigs.len(), 2);
    }

    #[test]
    fn mcp_children_pass_and_anything_else_is_non_mcp() {
        let sigs = shim_sig();
        let mut parent_map: HashMap<u32, Vec<u32>> = HashMap::new();
        parent_map.insert(100, vec![101, 102]);
        parent_map.insert(1, vec![100]);
        let mut cmdlines: HashMap<u32, String> = HashMap::new();
        cmdlines.insert(
            101,
            "/usr/bin/python3 /home/u/.qontinui/coord-mcp-shim.py --credential /tmp/c".into(),
        );
        cmdlines.insert(102, "my-server".into());
        assert_eq!(
            classify_descendants(100, &parent_map, &cmdlines, Some(&sigs)),
            Descendants::McpOnly
        );
        cmdlines.insert(102, "bash -c sleep 100".into());
        assert!(matches!(
            classify_descendants(100, &parent_map, &cmdlines, Some(&sigs)),
            Descendants::NonMcp(ref v) if v.len() == 1 && v[0].contains("bash")
        ));
        // Unresolved config: every child is non-MCP.
        cmdlines.insert(102, "my-server".into());
        assert!(matches!(
            classify_descendants(100, &parent_map, &cmdlines, None),
            Descendants::NonMcp(ref v) if v.len() == 2
        ));
        // No children at all passes even without a config.
        assert_eq!(
            classify_descendants(101, &parent_map, &cmdlines, None),
            Descendants::McpOnly
        );
        // An unreadable child command line is non-MCP.
        cmdlines.remove(&101);
        assert!(matches!(
            classify_descendants(100, &parent_map, &cmdlines, Some(&sigs)),
            Descendants::NonMcp(_)
        ));
        // An unreadable process table is unknown.
        assert!(matches!(
            classify_descendants(100, &HashMap::new(), &cmdlines, Some(&sigs)),
            Descendants::Unknown(_)
        ));
    }

    /// Item 4: a background job is not an MCP server just because its argv
    /// contains a declared arg as a SUBSTRING, or because some declared server
    /// uses the same launcher. Both the program and the exact argv tokens must
    /// match a declared server.
    #[test]
    fn a_background_node_server_js_is_not_mcp_without_a_matching_entry() {
        let sigs = servers_from_config_doc(&serde_json::json!({
            "mcpServers": {
                "coord-mcp": {
                    "command": "python3",
                    "args": ["/home/u/.qontinui/coord-mcp-shim.py"]
                },
                "srv": { "command": "uvx", "args": ["server"] },
                "dist": { "command": "node", "args": ["dist/server.js", "--stdio"] }
            }
        }));
        // Substring of `server`, `dist/server.js`; same `node` launcher.
        assert!(!cmdline_matches("node server.js", &sigs));
        assert!(!cmdline_matches("/usr/bin/node /tmp/job/server.js", &sigs));
        // A non-launcher program never passes for a launcher-declared server.
        assert!(!cmdline_matches("nodemon dist/server.js --stdio", &sigs));
        // The declared servers still match, including through a launcher.
        assert!(cmdline_matches("node dist/server.js --stdio", &sigs));
        assert!(cmdline_matches(
            "python3 /home/u/.qontinui/coord-mcp-shim.py",
            &sigs
        ));
        // A launcher with the declared arg only as part of a path: no token match.
        assert!(!cmdline_matches(
            "/usr/bin/python3 -I /root/.cache/uv/x/bin/server",
            &sigs
        ));
        assert!(cmdline_matches("python /x/uv/bin/tool server", &sigs));

        let mut parent_map: HashMap<u32, Vec<u32>> = HashMap::new();
        parent_map.insert(100, vec![101]);
        let mut cmdlines: HashMap<u32, String> = HashMap::new();
        cmdlines.insert(101, "node server.js".into());
        assert!(matches!(
            classify_descendants(100, &parent_map, &cmdlines, Some(&sigs)),
            Descendants::NonMcp(ref v) if v.len() == 1 && v[0].contains("node")
        ));
    }

    /// W4: the Windows argv rule — quoted program paths with spaces, quoted
    /// args, and the backslash rules.
    #[test]
    fn split_windows_argv_follows_command_line_to_argv() {
        assert_eq!(
            split_windows_argv(r#""C:\Program Files\nodejs\node.exe" "C:\a b\npx-cli.js" -y pkg"#),
            vec![
                r"C:\Program Files\nodejs\node.exe",
                r"C:\a b\npx-cli.js",
                "-y",
                "pkg"
            ]
        );
        assert_eq!(
            split_windows_argv(r#"p a\\"b c" d"#),
            vec!["p", r"a\b c", "d"]
        );
        assert_eq!(split_windows_argv(r#"p a\"b"#), vec!["p", r#"a"b"#]);
        assert_eq!(split_windows_argv(r"p C:\x\y"), vec!["p", r"C:\x\y"]);
        assert_eq!(split_windows_argv(r#"p "a""b""#), vec!["p", r#"a"b"#]);
        assert!(split_windows_argv("   ").is_empty());
    }

    /// Round-3 note 1: on Linux the child line is the unquoted `/proc` join, so
    /// an argument containing `'` (or `"`) is kept verbatim and the rest of the
    /// line is not swallowed — the declared server still matches.
    #[cfg(not(windows))]
    #[test]
    fn a_linux_child_line_with_a_quote_char_is_split_on_whitespace() {
        assert_eq!(
            split_cmdline("node /srv/it's/index.js --name=o'brien x"),
            vec!["node", "/srv/it's/index.js", "--name=o'brien", "x"]
        );
        let sigs = servers_from_config_doc(&serde_json::json!({
            "mcpServers": {
                "s": { "command": "node", "args": ["/srv/it's/index.js", "--name=o'brien"] }
            }
        }));
        assert!(cmdline_matches(
            "/usr/bin/node /srv/it's/index.js --name=o'brien",
            &sigs
        ));
    }

    /// W4: Windows-shaped child command lines match their declared servers —
    /// quoted node/npx paths, `cmd /c npx …`, `cmd.exe /d /s /c "npx …"`, and a
    /// config whose command and args are themselves quoted — while a
    /// background `node server.js` still does not.
    #[test]
    fn windows_quoted_and_cmd_wrapped_children_match_their_servers() {
        let sigs = servers_from_config_doc(&serde_json::json!({
            "mcpServers": {
                "x": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-x"] },
                "quoted": {
                    "command": "\"C:\\Program Files\\nodejs\\node.exe\"",
                    "args": ["\"C:\\srv\\index.js\""]
                },
                "dist": { "command": "node", "args": ["dist/server.js"] }
            }
        }));
        let win = split_windows_argv;
        assert!(cmdline_matches_with(
            r#""C:\Program Files\nodejs\node.exe" "C:\Program Files\nodejs\node_modules\npm\bin\npx-cli.js" -y @modelcontextprotocol/server-x"#,
            &sigs,
            win
        ));
        assert!(cmdline_matches_with(
            "cmd /c npx -y @modelcontextprotocol/server-x",
            &sigs,
            win
        ));
        assert!(cmdline_matches_with(
            r#"C:\Windows\system32\cmd.exe /d /s /c "npx -y @modelcontextprotocol/server-x""#,
            &sigs,
            win
        ));
        assert!(cmdline_matches_with(
            r#""C:\Program Files\nodejs\node.exe" "C:\srv\index.js""#,
            &sigs,
            win
        ));
        assert!(cmdline_matches_with(
            r"C:\tools\npx.cmd -y @modelcontextprotocol/server-x",
            &sigs,
            win
        ));
        // Not MCP: a background job, quoted or cmd-wrapped.
        assert!(!cmdline_matches_with(
            r#""C:\Program Files\nodejs\node.exe" server.js"#,
            &sigs,
            win
        ));
        assert!(!cmdline_matches_with("cmd /c node server.js", &sigs, win));
        assert!(!cmdline_matches_with("cmd /c dir", &sigs, win));
    }

    #[test]
    fn mcp_config_values_are_read_off_the_command_line() {
        assert_eq!(
            mcp_config_values_from_cmdline(
                "claude --resume abc --mcp-config /a.json /b.json --model opus"
            ),
            vec!["/a.json", "/b.json"]
        );
        assert_eq!(
            mcp_config_values_from_cmdline("claude --mcp-config=/c.json"),
            vec!["/c.json"]
        );
        assert!(mcp_config_values_from_cmdline("claude --resume abc").is_empty());
    }

    #[test]
    fn resolve_mcp_servers_reads_mcp_json_upward_and_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"s":{"command":"python3","args":["shim.py"]}}}"#,
        )
        .unwrap();
        let sub = root.join("repo/sub");
        std::fs::create_dir_all(&sub).unwrap();
        let sigs = resolve_mcp_servers(None, Some(sub.to_str().unwrap())).expect("resolved");
        assert!(sigs.iter().any(|s| s.args == vec!["shim.py".to_string()]));

        let flag = root.join("flag.json");
        std::fs::write(
            &flag,
            r#"{"mcpServers":{"f":{"command":"node","args":["f.js"]}}}"#,
        )
        .unwrap();
        let cmd = format!("claude --mcp-config {}", flag.display());
        let sigs = resolve_mcp_servers(Some(&cmd), None).expect("resolved");
        assert_eq!(sigs.len(), 1);

        // An explicit config that cannot be read makes the whole config unknown.
        assert!(resolve_mcp_servers(Some("claude --mcp-config /nonexistent.json"), None).is_none());
        // No source at all (no flag, no cwd): unresolvable. A cwd-based
        // negative is not asserted: a dev box's tempdir ancestors may carry a
        // real `.mcp.json`.
        assert!(resolve_mcp_servers(Some("claude"), None).is_none());
    }
}
