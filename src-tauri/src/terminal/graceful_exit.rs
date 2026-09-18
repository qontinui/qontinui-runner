//! Graceful exit of a terminal-hosted `claude` — type `/exit` at an empty
//! prompt, wait for the process to leave, only then close the tab (plan
//! `2026-09-13-drained-runner-never-reaches-idle`, design decision D5).
//!
//! ## Why not `close`
//!
//! Every close in the runner ends in `TerminalSession::close_with_deadline`,
//! which kills the pane's process (`taskkill /F /T` on Windows, a `SIGTERM` to
//! the single shell pid on Unix) and releases the pty — a kill of a live agent
//! that can drop its transcript tail. Wind-down must never do that to a live
//! `claude`.
//!
//! ## The protocol ([`drive`])
//!
//! 1. **Probe the pane's process subtree.** No `claude` → `NoLiveClaude`, and
//!    nothing is typed (typing into a bare shell is not a no-op). An
//!    unreadable table → `ProbeUnavailable`. More than one `claude` (a nested
//!    one would not leave with `/exit`), or a `claude` with child processes
//!    attached → `Refused`.
//! 2. **Check the screen** ([`exit_prompt_ready`]): the cursor sits at the
//!    start of an empty `❯` input line, the screen looks idle, and no dialog is
//!    showing. Otherwise `Refused` — typing `\r` into a permission dialog would
//!    APPROVE a tool call, and typing `/exit` after an unsent draft would
//!    submit `draft/exit`.
//! 3. **Type `/exit`, confirm the echo** ([`exit_echoed`]) on the input line,
//!    then submit with `\r`. No echo within [`ECHO_TIMEOUT`] → clear the line
//!    (Ctrl-U) and `Refused`.
//! 4. **Wait** up to the deadline, probing every [`POLL_INTERVAL`]. The
//!    `claude` counts as gone only when [`GONE_PROBES_REQUIRED`] consecutive
//!    probes find no `claude` in the subtree AND every `claude` identity ever
//!    seen (pid plus start time, so a reused pid is not mistaken for it) is
//!    absent from the WHOLE process table — a process re-parented out of the
//!    subtree while still attached to the pty is still alive.
//! 5. **Gone → close the tab → `Exited`.** Still there at the deadline →
//!    `ExitStuck`, the process LEFT RUNNING and the tab left open.
//!
//! There is no kill anywhere in this module; the tab-closing callback is
//! reachable only from the gone arm, and the tests below pin that. The one
//! public entry point is `TerminalManager::graceful_exit`, whose close skips
//! the kill entirely when the pane's own process (the `claude`, when it is the
//! pane root) has already exited.
//!
//! ## Evidence
//!
//! Measured 2026-09-13 against Claude Code 2.1.270 in a pty harness (`claude
//! --dangerously-skip-permissions`, no prompt ever sent, 120×40):
//!
//! - `/exit` + `\r` ended the process with exit code 0 in every trial,
//!   0.85–0.93 s after the `\r` (6 of 6 across one-write and split shapes, and
//!   again in the echo-confirmed shape this module uses: 0.895 s).
//! - An EMPTY prompt is not blank: it renders a placeholder
//!   (`❯ Try "write a test for <filepath>"`) with the cursor at the input
//!   start (column 2). An unsent draft renders the same way as text, but
//!   leaves the cursor after the draft (column 21 for a 19-character draft).
//!   The cursor column is therefore the discriminator, with the placeholder's
//!   `Try "` prefix as a second check.
//! - After typing `/exit` the input line reads `❯ /exit` with the cursor at
//!   column 7, while a slash-command menu opens above the input box.

use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

use serde::Serialize;

use crate::process_capture::process_tree::{claude_pids_in_inclusive_subtree, ProcessSnapshot};
use qontinui_runner_lib::looping_agent::idle::snapshot_looks_idle;

/// The command typed at the prompt. Submitted separately ([`SUBMIT`]) once it
/// has echoed on the input line.
pub const EXIT_TEXT: &[u8] = b"/exit";

/// Enter.
pub const SUBMIT: &[u8] = b"\r";

/// Ctrl-U: clears the input line when `/exit` did not echo as expected.
pub const CLEAR_INPUT_LINE: &[u8] = b"\x15";

/// How long to wait for `claude` to leave before reporting `ExitStuck`.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(60);

/// Spacing between process-table probes while waiting. One probe snapshots the
/// whole table: cheap `/proc` reads on Unix, but a PowerShell/CIM subprocess on
/// Windows, so Windows probes at most once a second.
#[cfg(windows)]
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Spacing between process-table probes while waiting (see the Windows twin).
#[cfg(not(windows))]
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long typed `/exit` may take to appear on the input line.
pub const ECHO_TIMEOUT: Duration = Duration::from_secs(2);

/// Spacing between screen reads while waiting for the echo.
pub const ECHO_POLL: Duration = Duration::from_millis(50);

/// Consecutive "gone" probes required before the tab is closed.
pub const GONE_PROBES_REQUIRED: u32 = 2;

/// The placeholder Claude Code renders on an empty input line.
const PLACEHOLDER_PREFIX: &str = "Try \"";

/// Rows above the cursor scanned for a dialog when no input-box edge bounds
/// the scan.
const DIALOG_SCAN_ROWS: usize = 8;

/// Case-insensitive text that marks a dialog rather than the input prompt.
const DIALOG_MARKERS: &[&str] = &["do you want", "allow", "esc to cancel", "enter to confirm"];

/// Timing knobs for [`drive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitTiming {
    pub deadline: Duration,
    pub poll: Duration,
    pub echo_timeout: Duration,
    pub echo_poll: Duration,
}

impl ExitTiming {
    /// Production timing with the given deadline.
    pub fn with_deadline(deadline: Duration) -> Self {
        Self {
            deadline,
            poll: POLL_INTERVAL,
            echo_timeout: ECHO_TIMEOUT,
            echo_poll: ECHO_POLL,
        }
    }
}

// ---------------------------------------------------------------------------
// Screen checks
// ---------------------------------------------------------------------------

/// The rendered grid as the screen checks read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenText {
    pub lines: Vec<String>,
    pub cursor_row: u16,
    pub cursor_col: u16,
}

/// The `❯` input line on the cursor row: the marker's column and the text
/// after it, trimmed (including a non-breaking space and a box edge `│`).
struct PromptLine {
    marker_col: usize,
    input: String,
}

fn prompt_line(screen: &ScreenText) -> Option<PromptLine> {
    let row = screen.lines.get(usize::from(screen.cursor_row))?;
    let chars: Vec<char> = row.chars().collect();
    let marker_col = chars.iter().position(|&c| c == '❯')?;
    let rest: String = chars[marker_col + 1..].iter().collect();
    let input = rest.trim().trim_end_matches('│').trim().to_string();
    Some(PromptLine { marker_col, input })
}

/// The column typed input starts at: the marker, one separator, then input.
fn input_start(line: &PromptLine) -> usize {
    line.marker_col + 2
}

/// `1. Yes`-shaped: a numbered choice, after any leading marker or box edge.
fn is_numbered_option(text: &str) -> bool {
    let body = text.trim_start_matches(|c: char| c.is_whitespace() || c == '│' || c == '❯');
    let digits = body.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && body[digits..].starts_with(". ")
}

/// Only box-drawing characters: the input box's top edge.
fn is_box_edge(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| "─━╭╮╰╯┌┐└┘│".contains(c))
}

/// May `/exit` be typed now? `Ok` only for an idle screen whose cursor sits at
/// the start of an empty `❯` input line with no dialog showing. The `Err`
/// names what was seen.
pub fn exit_prompt_ready(screen: &ScreenText) -> Result<(), String> {
    let Some(line) = prompt_line(screen) else {
        return Err("no `❯` input prompt on the cursor row".to_string());
    };
    if !snapshot_looks_idle(&screen.lines, screen.cursor_row) {
        return Err("the screen does not look idle".to_string());
    }
    if is_numbered_option(&line.input) {
        return Err("a numbered choice is selected — a dialog, not the input prompt".to_string());
    }
    let start = input_start(&line);
    if usize::from(screen.cursor_col) != start {
        return Err(format!(
            "the input line holds unsent text (cursor at column {}, input starts at column {start})",
            screen.cursor_col
        ));
    }
    if !line.input.is_empty() && !line.input.starts_with(PLACEHOLDER_PREFIX) {
        return Err(
            "the input line holds text that is not the empty-prompt placeholder".to_string(),
        );
    }

    // TWO scan regions, because the two signals do not have the same
    // false-positive profile.
    //
    // `marker_first` — always at least DIALOG_SCAN_ROWS above the cursor, and
    // the input box's top edge too when that sits higher. Taking the box edge
    // ALONE was the bug: the edge is found within two rows of the cursor,
    // which is the normal input-box rendering, so the scan began one or two
    // rows above the cursor and everything above it — dialog included — went
    // unscanned. That made the guard's reach a property of a two-row layout
    // heuristic rather than of the dialog markers, so a TUI layout change
    // could silently delete the check.
    //
    // `option_first` — the box region only. A numbered line ABOVE the box is
    // ordinary transcript ("1. First, run the tests" in an agent's own
    // output), so scanning for numbered options up there would refuse constantly.
    // The DIALOG_MARKERS phrases do not have that problem: they are specific
    // enough to mean a dialog wherever they appear near the cursor.
    //
    // Erring toward refusing is correct here either way: a false positive is a
    // `Refused`, which leaves the pane untouched, while a false negative types
    // `\r` into a live agent's terminal.
    let cursor_row = usize::from(screen.cursor_row);
    let box_edge = (cursor_row.saturating_sub(2)..cursor_row)
        .rev()
        .find(|&r| screen.lines.get(r).is_some_and(|l| is_box_edge(l)));
    let option_first = box_edge.unwrap_or_else(|| cursor_row.saturating_sub(DIALOG_SCAN_ROWS));
    let marker_first = option_first.min(cursor_row.saturating_sub(DIALOG_SCAN_ROWS));
    for (r, text) in screen.lines.iter().enumerate().skip(marker_first) {
        if r == cursor_row {
            continue;
        }
        if r >= option_first && is_numbered_option(text) {
            return Err(format!(
                "a numbered choice is showing on row {r} — a dialog"
            ));
        }
        let lower = text.to_lowercase();
        if let Some(marker) = DIALOG_MARKERS.iter().find(|m| lower.contains(*m)) {
            return Err(format!("dialog marker `{marker}` on row {r}"));
        }
    }
    Ok(())
}

/// Has typed `/exit` echoed? The input line reads exactly `/exit` and the
/// cursor sits right after it.
pub fn exit_echoed(screen: &ScreenText) -> bool {
    let Some(line) = prompt_line(screen) else {
        return false;
    };
    let typed = std::str::from_utf8(EXIT_TEXT).unwrap_or_default();
    line.input == typed && usize::from(screen.cursor_col) == input_start(&line) + typed.len()
}

/// Does the input line hold ONLY (a prefix of) the `/exit` we just wrote?
///
/// This gates the Ctrl-U recovery. Ctrl-U clears the WHOLE input line, not
/// just our bytes, so firing it blindly when the echo failed to arrive can
/// destroy something the operator typed into the pane during the echo window —
/// the pane is live, and that window is whole seconds long.
///
/// `true` only when what is on the line is a prefix of `/exit` (our own text,
/// possibly partially rendered). Anything else — the operator's own draft, or
/// our text with theirs appended — is NOT ours to clear, and the caller leaves
/// the line alone and says so instead. An empty line has nothing to clear, so
/// it is false too: the recovery is pointless there.
pub fn input_is_only_our_exit_text(screen: &ScreenText) -> bool {
    let Some(line) = prompt_line(screen) else {
        return false;
    };
    let typed = std::str::from_utf8(EXIT_TEXT).unwrap_or_default();
    !line.input.is_empty() && typed.starts_with(line.input.as_str())
}

// ---------------------------------------------------------------------------
// Process probe
// ---------------------------------------------------------------------------

/// A process as the probe identifies it: pid plus start time, so a reused pid
/// is not mistaken for the original. `started_at` is epoch seconds, `0` when
/// the platform could not resolve it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ProcIdentity {
    pub pid: u32,
    pub started_at: i64,
}

/// One readable look at the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneProcesses {
    /// `claude` processes in the pane's subtree, top-level first.
    pub subtree_claude: Vec<ProcIdentity>,
    /// Child processes of the top-level `claude` (0 when there is none).
    pub top_level_children: usize,
    /// Of the identities asked about, those alive ANYWHERE in the table.
    pub tracked_alive: Vec<ProcIdentity>,
}

/// What a probe saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeProbe {
    Readable(PaneProcesses),
    /// The table could not be read; says nothing either way.
    Unreadable(String),
}

/// Is `id` alive anywhere in `snapshot`? A pid absent from the table is not.
/// A present pid whose start time differs from the recorded one is a reuse and
/// is not. When either start time is unknown the process cannot be ruled out,
/// so it counts as alive.
pub fn alive_in(snapshot: &ProcessSnapshot, id: ProcIdentity) -> bool {
    let present =
        snapshot.creation_times.contains_key(&id.pid) || snapshot.names.contains_key(&id.pid);
    if !present {
        return false;
    }
    let now_start = snapshot.creation_times.get(&id.pid).copied().unwrap_or(0);
    id.started_at <= 0 || now_start <= 0 || now_start == id.started_at
}

/// Pure probe over one snapshot of the whole process table.
pub fn probe_from_snapshot(
    root_pid: u32,
    snapshot: &ProcessSnapshot,
    tracked: &[ProcIdentity],
) -> ClaudeProbe {
    if snapshot.parent_map.is_empty() {
        return ClaudeProbe::Unreadable(
            "the process table is unreadable (empty parent map)".to_string(),
        );
    }
    let pids = claude_pids_in_inclusive_subtree(root_pid, snapshot);
    let subtree_claude: Vec<ProcIdentity> = pids
        .iter()
        .map(|&pid| ProcIdentity {
            pid,
            started_at: snapshot.creation_times.get(&pid).copied().unwrap_or(0),
        })
        .collect();
    // Inclusive-subtree order is root first, then breadth-first, so the first
    // `claude` has no `claude` ancestor inside the pane.
    let top_level_children = pids
        .first()
        .and_then(|pid| snapshot.parent_map.get(pid))
        .map_or(0, Vec::len);
    let tracked_alive = tracked
        .iter()
        .copied()
        .filter(|&id| alive_in(snapshot, id))
        .collect();
    ClaudeProbe::Readable(PaneProcesses {
        subtree_claude,
        top_level_children,
        tracked_alive,
    })
}

/// Probe the pane rooted at `root_pid` against a fresh snapshot of the whole
/// process table.
pub async fn probe_claude_under(root_pid: Option<u32>, tracked: Vec<ProcIdentity>) -> ClaudeProbe {
    let Some(root) = root_pid else {
        return ClaudeProbe::Unreadable(
            "the pane has no local process id (a remote pane), so its subtree cannot be observed"
                .to_string(),
        );
    };
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    probe_from_snapshot(root, &snapshot, &tracked)
}

// ---------------------------------------------------------------------------
// Outcome + driver
// ---------------------------------------------------------------------------

/// What a graceful exit did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GracefulExitOutcome {
    /// `claude` left within the deadline and the tab was closed.
    Exited {
        waited_ms: u64,
        /// The `claude` pids present when `/exit` was typed.
        claude_pids: Vec<u32>,
    },
    /// `claude` was still alive at the deadline. NOTHING was killed and the
    /// tab was not closed.
    ExitStuck {
        waited_ms: u64,
        /// The `claude` pids alive at the last readable probe.
        claude_pids: Vec<u32>,
        last_probe_unreadable: bool,
    },
    /// The pane was not in a state where `/exit` is safe to type. Nothing was
    /// submitted (no `\r`) and nothing was closed.
    ///
    /// The pane may still have been WRITTEN TO, and `reason` says exactly what
    /// happened: when the readiness check refused, nothing was typed at all;
    /// when `/exit` was typed but never echoed, the recovery either cleared
    /// the line with Ctrl-U or deliberately left it alone. That Ctrl-U clears
    /// the WHOLE input line, so where it fired it also removed anything the
    /// operator had typed into the pane during the echo window — which is why
    /// it now fires only while the line holds nothing but our own `/exit`
    /// ([`input_is_only_our_exit_text`]), and `reason` states which of the two
    /// it did.
    Refused {
        reason: String,
        claude_pids: Vec<u32>,
    },
    /// No `claude` in the pane's subtree; nothing was typed or closed.
    NoLiveClaude,
    /// The process table could not be read before starting; nothing was typed
    /// or closed.
    ProbeUnavailable { detail: String },
    /// Writing to the pane failed; nothing was closed.
    WriteFailed {
        error: String,
        claude_pids: Vec<u32>,
    },
}

fn pids_of(ids: &[ProcIdentity]) -> Vec<u32> {
    ids.iter().map(|id| id.pid).collect()
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Drive the protocol over injected effects. `write` types into the pane,
/// `screen` reads its rendered grid, `probe` looks at its processes (given the
/// identities to check for anywhere in the table), `close_tab` closes it. The
/// pane is reached no other way.
pub async fn drive<W, S, P, PF, C, CF>(
    mut write: W,
    mut screen: S,
    mut probe: P,
    close_tab: C,
    timing: ExitTiming,
) -> GracefulExitOutcome
where
    W: FnMut(&[u8]) -> Result<(), String>,
    S: FnMut() -> ScreenText,
    P: FnMut(Vec<ProcIdentity>) -> PF,
    PF: Future<Output = ClaudeProbe>,
    C: FnOnce() -> CF,
    CF: Future<Output = ()>,
{
    // (1) The pane's processes.
    let initial = match probe(Vec::new()).await {
        ClaudeProbe::Unreadable(detail) => return GracefulExitOutcome::ProbeUnavailable { detail },
        ClaudeProbe::Readable(view) => view,
    };
    let claude_pids = pids_of(&initial.subtree_claude);
    match initial.subtree_claude.len() {
        0 => return GracefulExitOutcome::NoLiveClaude,
        1 => {}
        n => {
            return GracefulExitOutcome::Refused {
                reason: format!(
                    "{n} claude processes in the pane — a nested claude does not leave with /exit"
                ),
                claude_pids,
            }
        }
    }
    if initial.top_level_children > 0 {
        return GracefulExitOutcome::Refused {
            reason: format!(
                "the claude process has {} child process(es) attached",
                initial.top_level_children
            ),
            claude_pids,
        };
    }

    // (2) The screen.
    if let Err(why) = exit_prompt_ready(&screen()) {
        return GracefulExitOutcome::Refused {
            reason: format!("not at an empty prompt: {why}"),
            claude_pids,
        };
    }

    // (3) Type, confirm the echo, submit.
    if let Err(error) = write(EXIT_TEXT) {
        return GracefulExitOutcome::WriteFailed { error, claude_pids };
    }
    let echo_started = tokio::time::Instant::now();
    while !exit_echoed(&screen()) {
        if echo_started.elapsed() >= timing.echo_timeout {
            // Ctrl-U clears the WHOLE line. Only do it when the line still
            // holds nothing but our own (possibly partial) `/exit` — never
            // when the operator has typed into the pane during the echo
            // window, where clearing would destroy their text.
            let cleared = if input_is_only_our_exit_text(&screen()) {
                match write(CLEAR_INPUT_LINE) {
                    Ok(()) => "the partially-typed /exit was cleared".to_string(),
                    Err(e) => format!("clearing the line failed: {e}"),
                }
            } else {
                "the line was LEFT AS IS — it no longer holds only our /exit, so \
                 clearing it could have destroyed text typed in the pane; any \
                 leftover /exit characters are still on the input line"
                    .to_string()
            };
            return GracefulExitOutcome::Refused {
                reason: format!(
                    "typed /exit did not echo on the input line within {} ms; {cleared}",
                    millis(timing.echo_timeout)
                ),
                claude_pids,
            };
        }
        tokio::time::sleep(timing.echo_poll).await;
    }
    if let Err(error) = write(SUBMIT) {
        return GracefulExitOutcome::WriteFailed { error, claude_pids };
    }

    // (4) Wait for every claude ever seen to be gone from the whole table.
    let mut tracked: BTreeSet<ProcIdentity> = initial.subtree_claude.iter().copied().collect();
    let mut last_alive = claude_pids.clone();
    let mut last_probe_unreadable = false;
    let mut gone_streak = 0u32;
    let started = tokio::time::Instant::now();
    loop {
        let elapsed = started.elapsed();
        if elapsed >= timing.deadline {
            return GracefulExitOutcome::ExitStuck {
                waited_ms: millis(elapsed),
                claude_pids: last_alive,
                last_probe_unreadable,
            };
        }
        tokio::time::sleep(timing.poll.min(timing.deadline - elapsed)).await;
        match probe(tracked.iter().copied().collect()).await {
            ClaudeProbe::Readable(view) => {
                last_probe_unreadable = false;
                if view.subtree_claude.is_empty() && view.tracked_alive.is_empty() {
                    gone_streak += 1;
                    if gone_streak >= GONE_PROBES_REQUIRED {
                        // (5) Gone.
                        close_tab().await;
                        return GracefulExitOutcome::Exited {
                            waited_ms: millis(started.elapsed()),
                            claude_pids,
                        };
                    }
                } else {
                    gone_streak = 0;
                    tracked.extend(view.subtree_claude.iter().copied());
                    let alive: BTreeSet<u32> = view
                        .subtree_claude
                        .iter()
                        .chain(view.tracked_alive.iter())
                        .map(|id| id.pid)
                        .collect();
                    last_alive = alive.into_iter().collect();
                }
            }
            ClaudeProbe::Unreadable(_) => {
                gone_streak = 0;
                last_probe_unreadable = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // ---- screen fixtures (rows 35-39 of a real 120x40 Claude Code 2.1.270
    //      capture; see module docs) ---------------------------------------

    fn edge() -> String {
        "─".repeat(120)
    }

    fn screen(rows: Vec<String>, cursor_row: u16, cursor_col: u16) -> ScreenText {
        ScreenText {
            lines: rows,
            cursor_row,
            cursor_col,
        }
    }

    /// An empty prompt showing its placeholder, cursor at the input start.
    fn empty_prompt() -> ScreenText {
        screen(
            vec![
                format!("{}● high · /effort", " ".repeat(102)),
                edge(),
                "❯\u{a0}Try \"write a test for <filepath>\"".to_string(),
                edge(),
                "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents".to_string(),
            ],
            2,
            2,
        )
    }

    fn draft_prompt() -> ScreenText {
        screen(
            vec![
                format!("{}● high · /effort", " ".repeat(102)),
                edge(),
                "❯\u{a0}draft text not sent".to_string(),
                edge(),
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string(),
            ],
            2,
            21,
        )
    }

    fn echoed_prompt() -> ScreenText {
        screen(
            vec![
                "  /exit                         Exit the CLI".to_string(),
                "  /usage-credits                Configure usage credits".to_string(),
                edge(),
                "❯\u{a0}/exit".to_string(),
                edge(),
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string(),
            ],
            3,
            7,
        )
    }

    fn permission_dialog() -> ScreenText {
        screen(
            vec![
                " Bash command".to_string(),
                "   rm -rf build".to_string(),
                " Do you want to proceed?".to_string(),
                " ❯ 1. Yes".to_string(),
                "   2. Yes, and don't ask again for rm commands in this project".to_string(),
                "   3. No, and tell Claude what to do differently (esc)".to_string(),
            ],
            3,
            3,
        )
    }

    #[test]
    fn the_empty_prompt_placeholder_is_ready() {
        assert_eq!(exit_prompt_ready(&empty_prompt()), Ok(()));
        // A bare prompt with no placeholder, as older builds render it.
        let bare = screen(vec!["done.".into(), "❯ ".into()], 1, 2);
        assert_eq!(exit_prompt_ready(&bare), Ok(()));
        // After Ctrl-U the transcript row above the box reads a hint; it is
        // above the input box, so it is not scanned.
        let mut cleared = empty_prompt();
        cleared.lines[0] = format!("{}Ctrl+Y to paste deleted text", " ".repeat(90));
        assert_eq!(exit_prompt_ready(&cleared), Ok(()));
    }

    #[test]
    fn an_unsent_draft_is_refused() {
        let why = exit_prompt_ready(&draft_prompt()).unwrap_err();
        assert!(why.contains("unsent text"), "{why}");
        // Even with the cursor moved back to the input start, draft text that
        // is not the placeholder is refused.
        let mut home = draft_prompt();
        home.cursor_col = 2;
        let why = exit_prompt_ready(&home).unwrap_err();
        assert!(why.contains("not the empty-prompt placeholder"), "{why}");
    }

    #[test]
    fn a_permission_dialog_is_refused() {
        let why = exit_prompt_ready(&permission_dialog()).unwrap_err();
        assert!(why.contains("numbered choice"), "{why}");
        // The same dialog with the cursor parked on a plain `❯` row below it.
        let mut parked = permission_dialog();
        parked.lines.push("❯ ".to_string());
        parked.cursor_row = 6;
        parked.cursor_col = 2;
        assert!(exit_prompt_ready(&parked).is_err());
    }

    #[test]
    fn dialog_markers_inside_the_input_box_region_are_refused() {
        let mut s = empty_prompt();
        s.lines[4] = "  Enter to confirm · Esc to cancel".to_string();
        let why = exit_prompt_ready(&s).unwrap_err();
        assert!(why.contains("dialog marker"), "{why}");
    }

    #[test]
    fn numbered_output_above_the_input_box_is_not_a_dialog() {
        let mut s = empty_prompt();
        s.lines[0] = "1. First, run the tests".to_string();
        assert_eq!(exit_prompt_ready(&s), Ok(()));
    }

    /// A dialog marker ABOVE the input box's top edge is still caught.
    ///
    /// The box edge renders one or two rows above the cursor, so keying the
    /// scan's start on it alone truncated the scan to almost nothing and left
    /// anything higher unread. The marker scan now always reaches
    /// `DIALOG_SCAN_ROWS` above the cursor as well, so the guard depends on the
    /// dialog markers rather than on a two-row layout assumption.
    #[test]
    fn a_dialog_marker_above_the_box_edge_is_still_caught() {
        let mut s = empty_prompt();
        // row 1 is the box edge, row 2 the prompt/cursor — so row 0 is above
        // the edge and was previously never scanned.
        s.lines[0] = " Do you want to proceed?".to_string();
        let why = exit_prompt_ready(&s).unwrap_err();
        assert!(why.contains("dialog marker"), "{why}");
        assert!(why.contains("row 0"), "{why}");
    }

    /// The Ctrl-U recovery only fires while the line holds our own text.
    #[test]
    fn only_our_own_exit_text_may_be_cleared() {
        // Fully echoed, and partially echoed — both ours.
        assert!(input_is_only_our_exit_text(&echoed_prompt()));
        let mut partial = echoed_prompt();
        partial.lines[3] = "❯\u{a0}/exi".to_string();
        assert!(input_is_only_our_exit_text(&partial));

        // The operator's own draft is NOT ours to clear.
        assert!(!input_is_only_our_exit_text(&draft_prompt()));

        // Our text with theirs appended is not ours either — Ctrl-U would take
        // both.
        let mut mixed = echoed_prompt();
        mixed.lines[3] = "❯\u{a0}/exit and then some".to_string();
        assert!(!input_is_only_our_exit_text(&mixed));

        // An empty prompt has nothing of ours to clear.
        assert!(!input_is_only_our_exit_text(&empty_prompt()));
    }

    #[test]
    fn a_working_screen_or_a_missing_prompt_is_refused() {
        let mut working = empty_prompt();
        working.lines[0] = "✻ Compiling… (esc to interrupt)".to_string();
        assert!(exit_prompt_ready(&working).is_err());
        let mut elsewhere = empty_prompt();
        elsewhere.cursor_row = 4;
        let why = exit_prompt_ready(&elsewhere).unwrap_err();
        assert!(why.contains("no `❯` input prompt"), "{why}");
    }

    #[test]
    fn echo_detection() {
        assert!(exit_echoed(&echoed_prompt()));
        assert!(!exit_echoed(&empty_prompt()));
        assert!(!exit_echoed(&draft_prompt()));
        let mut early = echoed_prompt();
        early.cursor_col = 5;
        assert!(!exit_echoed(&early), "cursor not after the echo");
        let mut prefixed = echoed_prompt();
        prefixed.lines[3] = "❯\u{a0}draft/exit".to_string();
        prefixed.cursor_col = 12;
        assert!(!exit_echoed(&prefixed));
    }

    // ---- process probe ------------------------------------------------------

    /// root 100 (shell) -> 200 (claude) ; unrelated 300 ; optional extras.
    fn snapshot(entries: &[(u32, u32, &str, i64)]) -> ProcessSnapshot {
        let mut snap = ProcessSnapshot::default();
        for &(pid, ppid, name, started) in entries {
            snap.parent_map.entry(ppid).or_default().push(pid);
            snap.creation_times.insert(pid, started);
            snap.names.insert(pid, name.to_string());
        }
        snap
    }

    fn id(pid: u32, started_at: i64) -> ProcIdentity {
        ProcIdentity { pid, started_at }
    }

    #[tokio::test]
    async fn a_pane_without_a_local_pid_is_unreadable() {
        assert!(matches!(
            probe_claude_under(None, vec![]).await,
            ClaudeProbe::Unreadable(_)
        ));
    }

    #[test]
    fn an_empty_process_table_is_unreadable() {
        assert!(matches!(
            probe_from_snapshot(100, &ProcessSnapshot::default(), &[id(200, 5)]),
            ClaudeProbe::Unreadable(_)
        ));
    }

    #[test]
    fn probe_reports_subtree_claude_children_and_tracked_liveness() {
        let snap = snapshot(&[
            (100, 1, "bash", 10),
            (200, 100, "claude", 20),
            (210, 200, "node", 21),
            (300, 1, "sshd", 5),
        ]);
        let ClaudeProbe::Readable(view) = probe_from_snapshot(100, &snap, &[id(200, 20)]) else {
            panic!("readable");
        };
        assert_eq!(view.subtree_claude, vec![id(200, 20)]);
        assert_eq!(view.top_level_children, 1);
        assert_eq!(view.tracked_alive, vec![id(200, 20)]);
    }

    #[test]
    fn a_claude_gone_from_the_subtree_but_alive_elsewhere_is_still_alive() {
        // Re-parented to init (pid 1) while still running.
        let snap = snapshot(&[(100, 1, "bash", 10), (200, 1, "claude", 20)]);
        let ClaudeProbe::Readable(view) = probe_from_snapshot(100, &snap, &[id(200, 20)]) else {
            panic!("readable");
        };
        assert!(view.subtree_claude.is_empty());
        assert_eq!(view.tracked_alive, vec![id(200, 20)]);
    }

    #[test]
    fn a_reused_pid_is_not_the_original_process() {
        let snap = snapshot(&[(100, 1, "bash", 10), (200, 1, "vim", 99)]);
        assert!(!alive_in(&snap, id(200, 20)));
        // Unknown start time on either side cannot rule the process out.
        assert!(alive_in(&snap, id(200, 0)));
        let mut unknown = snapshot(&[(100, 1, "bash", 10), (200, 1, "vim", 99)]);
        unknown.creation_times.insert(200, 0);
        assert!(alive_in(&unknown, id(200, 20)));
        assert!(!alive_in(&snap, id(4242, 20)));
    }

    // ---- driver --------------------------------------------------------------

    type Log = Arc<Mutex<Vec<String>>>;

    fn timing() -> ExitTiming {
        ExitTiming {
            deadline: Duration::from_secs(60),
            poll: Duration::from_millis(500),
            echo_timeout: Duration::from_secs(2),
            echo_poll: Duration::from_millis(50),
        }
    }

    fn readable(subtree: &[ProcIdentity], children: usize, alive: &[ProcIdentity]) -> ClaudeProbe {
        ClaudeProbe::Readable(PaneProcesses {
            subtree_claude: subtree.to_vec(),
            top_level_children: children,
            tracked_alive: alive.to_vec(),
        })
    }

    const CLAUDE: ProcIdentity = ProcIdentity {
        pid: 42,
        started_at: 1_000,
    };

    /// A probe that replays `script`, repeating its last entry forever.
    fn scripted_probe(
        script: Vec<ClaudeProbe>,
        log: Log,
    ) -> impl FnMut(Vec<ProcIdentity>) -> std::future::Ready<ClaudeProbe> {
        let mut i = 0usize;
        move |tracked| {
            let p = script[i.min(script.len() - 1)].clone();
            i += 1;
            log.lock()
                .unwrap()
                .push(format!("probe(tracked={})", tracked.len()));
            std::future::ready(p)
        }
    }

    /// A pane that echoes typed bytes onto its input line when `echoes`.
    struct FakePane {
        typed: Arc<Mutex<Vec<u8>>>,
        echoes: bool,
        initial: ScreenText,
    }

    impl FakePane {
        fn new(initial: ScreenText, echoes: bool) -> Self {
            Self {
                typed: Arc::default(),
                echoes,
                initial,
            }
        }

        fn write(&self, log: Log) -> impl FnMut(&[u8]) -> Result<(), String> {
            let typed = Arc::clone(&self.typed);
            move |bytes| {
                typed.lock().unwrap().extend_from_slice(bytes);
                log.lock()
                    .unwrap()
                    .push(format!("write:{:?}", String::from_utf8_lossy(bytes)));
                Ok(())
            }
        }

        fn screen(&self) -> impl FnMut() -> ScreenText {
            let typed = Arc::clone(&self.typed);
            let echoes = self.echoes;
            let initial = self.initial.clone();
            move || {
                if echoes && typed.lock().unwrap().starts_with(EXIT_TEXT) {
                    echoed_prompt()
                } else {
                    initial.clone()
                }
            }
        }
    }

    fn recording_close(log: Log) -> impl FnOnce() -> std::future::Ready<()> {
        move || {
            log.lock().unwrap().push("close".to_string());
            std::future::ready(())
        }
    }

    fn entries(log: &Log) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    #[tokio::test(start_paused = true)]
    async fn exits_then_closes_only_after_two_consecutive_gone_probes() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), true);
        let outcome = drive(
            pane.write(log.clone()),
            pane.screen(),
            scripted_probe(
                vec![
                    readable(&[CLAUDE], 0, &[]),
                    readable(&[], 0, &[]),
                    readable(&[CLAUDE], 0, &[CLAUDE]),
                    readable(&[], 0, &[]),
                    readable(&[], 0, &[]),
                ],
                log.clone(),
            ),
            recording_close(log.clone()),
            timing(),
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::Exited {
                waited_ms: 2_000,
                claude_pids: vec![42]
            }
        );
        assert_eq!(
            entries(&log),
            vec![
                "probe(tracked=0)",
                "write:\"/exit\"",
                "write:\"\\r\"",
                "probe(tracked=1)",
                "probe(tracked=1)",
                "probe(tracked=1)",
                "probe(tracked=1)",
                "close",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_claude_alive_elsewhere_in_the_table_blocks_the_close_until_the_deadline() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), true);
        let outcome = drive(
            pane.write(log.clone()),
            pane.screen(),
            scripted_probe(
                vec![readable(&[CLAUDE], 0, &[]), readable(&[], 0, &[CLAUDE])],
                log.clone(),
            ),
            recording_close(log.clone()),
            timing(),
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::ExitStuck {
                waited_ms: 60_000,
                claude_pids: vec![42],
                last_probe_unreadable: false,
            }
        );
        assert!(!entries(&log).iter().any(|e| e == "close"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_claude_that_outlives_the_deadline_is_left_running() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), true);
        let outcome = drive(
            pane.write(log.clone()),
            pane.screen(),
            scripted_probe(vec![readable(&[CLAUDE], 0, &[CLAUDE])], log.clone()),
            recording_close(log.clone()),
            timing(),
        )
        .await;
        assert!(
            matches!(
                outcome,
                GracefulExitOutcome::ExitStuck {
                    waited_ms: 60_000,
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert!(!entries(&log).iter().any(|e| e == "close"));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreadable_probe_breaks_the_gone_streak() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), true);
        let outcome = drive(
            pane.write(log.clone()),
            pane.screen(),
            scripted_probe(
                vec![
                    readable(&[CLAUDE], 0, &[]),
                    readable(&[], 0, &[]),
                    ClaudeProbe::Unreadable("boom".into()),
                    readable(&[], 0, &[]),
                    ClaudeProbe::Unreadable("boom".into()),
                ],
                log.clone(),
            ),
            recording_close(log.clone()),
            ExitTiming {
                deadline: Duration::from_secs(5),
                ..timing()
            },
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::ExitStuck {
                waited_ms: 5_000,
                claude_pids: vec![42],
                last_probe_unreadable: true,
            }
        );
        assert!(!entries(&log).iter().any(|e| e == "close"));
    }

    #[tokio::test(start_paused = true)]
    async fn refusals_before_typing_write_nothing() {
        let cases: Vec<(ClaudeProbe, ScreenText, &str)> = vec![
            (
                readable(&[CLAUDE, id(43, 1_001)], 0, &[]),
                empty_prompt(),
                "2 claude processes",
            ),
            (
                readable(&[CLAUDE], 3, &[]),
                empty_prompt(),
                "3 child process",
            ),
            (
                readable(&[CLAUDE], 0, &[]),
                draft_prompt(),
                "not at an empty prompt",
            ),
            (
                readable(&[CLAUDE], 0, &[]),
                permission_dialog(),
                "not at an empty prompt",
            ),
        ];
        for (probe, initial, expect) in cases {
            let log: Log = Arc::default();
            let pane = FakePane::new(initial, true);
            let outcome = drive(
                pane.write(log.clone()),
                pane.screen(),
                scripted_probe(vec![probe], log.clone()),
                recording_close(log.clone()),
                timing(),
            )
            .await;
            match &outcome {
                GracefulExitOutcome::Refused { reason, .. } => {
                    assert!(reason.contains(expect), "{reason} !~ {expect}")
                }
                other => panic!("expected Refused ({expect}), got {other:?}"),
            }
            assert!(pane.typed.lock().unwrap().is_empty(), "{expect}: typed");
            assert!(!entries(&log).iter().any(|e| e == "close"));
        }
    }

    /// A missing echo never submits — and does NOT blind-clear a line that is
    /// not showing our text.
    ///
    /// The pane here never echoes, so the input line still shows its
    /// placeholder. Ctrl-U clears the WHOLE line, so firing it on a line we
    /// cannot see our own text on risks destroying whatever the operator typed
    /// during the echo window. `\r` is never written either way.
    #[tokio::test(start_paused = true)]
    async fn a_missing_echo_never_submits_and_leaves_a_foreign_line_alone() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), false);
        let outcome = drive(
            pane.write(log.clone()),
            pane.screen(),
            scripted_probe(vec![readable(&[CLAUDE], 0, &[])], log.clone()),
            recording_close(log.clone()),
            timing(),
        )
        .await;
        match &outcome {
            GracefulExitOutcome::Refused { reason, .. } => {
                assert!(reason.contains("did not echo"), "{reason}");
                assert!(reason.contains("LEFT AS IS"), "{reason}");
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert_eq!(
            pane.typed.lock().unwrap().as_slice(),
            b"/exit",
            "no Ctrl-U (0x15) and above all no \\r"
        );
        assert!(!entries(&log).iter().any(|e| e == "close"));
    }

    /// When the line IS showing our own partially-typed `/exit`, the Ctrl-U
    /// recovery still fires — the narrowing removed the blind clear, not the
    /// clear.
    #[tokio::test(start_paused = true)]
    async fn a_partially_echoed_exit_is_cleared() {
        let log: Log = Arc::default();
        let typed: Arc<Mutex<Vec<u8>>> = Arc::default();
        let write = {
            let typed = Arc::clone(&typed);
            move |bytes: &[u8]| {
                typed.lock().unwrap().extend_from_slice(bytes);
                Ok(())
            }
        };
        // Empty until we type (so the readiness check passes), then a PARTIAL
        // `/exi` — ours, but never completing, so the echo check never passes.
        let screen = {
            let typed = Arc::clone(&typed);
            move || {
                if typed.lock().unwrap().is_empty() {
                    empty_prompt()
                } else {
                    let mut partial = echoed_prompt();
                    partial.lines[3] = "❯\u{a0}/exi".to_string();
                    partial.cursor_col = 6;
                    partial
                }
            }
        };
        let outcome = drive(
            write,
            screen,
            scripted_probe(vec![readable(&[CLAUDE], 0, &[])], log.clone()),
            recording_close(log.clone()),
            timing(),
        )
        .await;
        match &outcome {
            GracefulExitOutcome::Refused { reason, .. } => {
                assert!(reason.contains("did not echo"), "{reason}");
                assert!(reason.contains("cleared"), "{reason}");
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert_eq!(
            typed.lock().unwrap().as_slice(),
            b"/exit\x15",
            "our own partial text is cleared with Ctrl-U, and still no \\r"
        );
        assert!(!entries(&log).iter().any(|e| e == "close"));
    }

    #[tokio::test(start_paused = true)]
    async fn no_claude_or_an_unreadable_table_types_nothing() {
        for (probe, expected) in [
            (readable(&[], 0, &[]), GracefulExitOutcome::NoLiveClaude),
            (
                ClaudeProbe::Unreadable("x".into()),
                GracefulExitOutcome::ProbeUnavailable {
                    detail: "x".to_string(),
                },
            ),
        ] {
            let log: Log = Arc::default();
            let pane = FakePane::new(empty_prompt(), true);
            let outcome = drive(
                pane.write(log.clone()),
                pane.screen(),
                scripted_probe(vec![probe], log.clone()),
                recording_close(log.clone()),
                timing(),
            )
            .await;
            assert_eq!(outcome, expected);
            assert_eq!(entries(&log).len(), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_write_closes_nothing() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), true);
        let outcome = drive(
            |_bytes: &[u8]| Err("terminal exited".to_string()),
            pane.screen(),
            scripted_probe(vec![readable(&[CLAUDE], 0, &[])], log.clone()),
            recording_close(log.clone()),
            timing(),
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::WriteFailed {
                error: "terminal exited".to_string(),
                claude_pids: vec![42]
            }
        );
        assert_eq!(entries(&log), vec!["probe(tracked=0)"]);
    }

    #[test]
    fn outcome_wire_shape() {
        let json = serde_json::to_value(GracefulExitOutcome::Refused {
            reason: "not at an empty prompt".into(),
            claude_pids: vec![1],
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "outcome": "refused", "reason": "not at an empty prompt", "claude_pids": [1]
            })
        );
    }

    #[test]
    fn windows_probes_at_most_once_a_second() {
        if cfg!(windows) {
            assert!(POLL_INTERVAL >= Duration::from_secs(1));
        } else {
            assert_eq!(POLL_INTERVAL, Duration::from_millis(500));
        }
    }

    /// Structural pin on the invariant: this module's code never names a kill
    /// path. `drive` reaches the pane only through its injected effects, and
    /// the production close is `TerminalManager::graceful_exit`'s.
    #[test]
    fn this_module_contains_no_kill_path() {
        let source = include_str!("graceful_exit.rs");
        let production: String = source
            .split("#[cfg(test)]")
            .next()
            .expect("module has a production section")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            ".kill(",
            "close_with_deadline",
            "taskkill",
            "SIGKILL",
            "SIGTERM",
        ] {
            assert!(
                !production.contains(forbidden),
                "graceful_exit production code must not contain `{forbidden}`"
            );
        }
    }
}
