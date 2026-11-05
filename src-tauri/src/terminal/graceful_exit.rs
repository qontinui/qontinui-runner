//! Graceful exit of a terminal-hosted `claude` — type `/exit` at an empty
//! prompt, wait for the process to leave, only then close the tab (plan
//! `2026-09-13-drained-runner-never-reaches-idle`, design decision D5).
//!
//! ## Why not `close`
//!
//! Every close in the runner ends in `TerminalSession::close_with_deadline`,
//! which kills the pane's process (an in-process tree kill on Windows, a `SIGTERM` to
//! the single shell pid on Unix) and releases the pty — a kill of a live agent
//! that can drop its transcript tail. Wind-down must never do that to a live
//! `claude`.
//!
//! ## The protocol ([`drive_graceful_exit`])
//!
//! 1. **Probe the pane's process subtree.** No `claude` → `NoLiveClaude`, and
//!    nothing is typed (typing into a bare shell is not a no-op). An
//!    unreadable table → `ProbeUnavailable`.
//!
//!    A pid is a `claude` on either of two witnesses: its image name, or a
//!    Claude Code registry record (`<CLAUDE_CONFIG_DIR>/sessions/<pid>.json`)
//!    that names it, whatever its image — the one an npm-installed Claude
//!    Code, which runs as `node … cli.js`, is seen by ([`RegistryWitness`]).
//!    A registry that cannot settle a pid whose image could be a JS-runtime
//!    Claude Code (`node`, `bun`; a `sessions/` dir that exists and cannot be
//!    listed, a record that does not parse) is an unreadable probe —
//!    `ProbeUnavailable` — never `NoLiveClaude`.
//!
//!    **Then check the close authority.** A live `claude` has been found and
//!    nothing has been typed yet. A caller that presents no
//!    [`CloseAuthority`] is refused `NoCloseAuthority` here (plan
//!    `2026-10-28-one-session-lifecycle-authority`, Phase 14 / R0a). No
//!    production code can mint one until the lifecycle Closer does (Phase
//!    93), so today EVERY production call that finds a live `claude` ends
//!    here — see [`crate::terminal::close_authority`].
//!
//!    Past the authority: more than one `claude` (a nested one would not
//!    leave with `/exit`), or a `claude` with child processes attached →
//!    `Refused`.
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
//! public entry point is `TerminalManager::graceful_exit`, which takes the
//! caller's `Option<CloseAuthority>` and hands it to [`drive_graceful_exit`]. Its close skips
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

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde::Serialize;

use crate::process_capture::process_tree::{
    inclusive_subtree_pids, is_countable_claude, may_be_js_runtime_claude, ProcessSnapshot,
};
use crate::session::claude_activity::{self, LivePid, RecordDepth, RecordReading};
use crate::terminal::close_authority::CloseAuthority;
use qontinui_runner_lib::looping_agent::idle::{
    snapshot_looks_idle, DIALOG_CHROME_MARKERS, DIALOG_SCAN_ROWS,
};

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

// `DIALOG_SCAN_ROWS` — rows above the cursor scanned for a dialog when no
// input-box edge bounds the scan. Shared with the looping-agent supervisor's
// dialog guard (`looping_agent::idle::snapshot_shows_dialog`).

/// Case-insensitive text that marks a dialog rather than the input prompt.
const DIALOG_MARKERS: &[&str] = &["do you want", "allow", "esc to cancel", "enter to confirm"];

/// Timing knobs for [`drive_graceful_exit`].
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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

// `DIALOG_CHROME_MARKERS` — dialog text that is Claude Code's own CHROME rather
// than prose: the key hints a dialog prints under its choices. Unlike `do you
// want` / `allow`, these do not turn up in an ordinary finished turn ("Do you
// want me to run the tests?", "the allowlist"), so a check that must ADMIT the
// common idle screen can scan for them without refusing it. The one list lives
// beside the idle predicate, which the looping-agent supervisor's dialog guard
// (`snapshot_shows_dialog`) reads too.

/// May `/exit` be typed now? `Ok` only for an idle screen whose cursor sits at
/// the start of an empty `❯` input line with no dialog showing. The `Err`
/// names what was seen.
///
/// Scans for the full [`DIALOG_MARKERS`] list, including prose-shaped
/// phrases. That errs toward refusing, which is right for `/exit`: a refusal
/// only postpones a wind-down.
pub fn exit_prompt_ready(screen: &ScreenText) -> Result<(), String> {
    prompt_ready_with(screen, DIALOG_MARKERS)
}

/// May a message be PASTED into the input box now? The same checks as
/// [`exit_prompt_ready`] — cursor-row `❯`, the shared turn-complete predicate,
/// cursor at the input start, nothing typed but the placeholder, no numbered
/// choice in the box region — but the dialog-text scan looks only for
/// [`DIALOG_CHROME_MARKERS`].
///
/// The difference is deliberate. The session message poller re-checks every
/// 10 s for as long as a message is pending, and an idle screen does not
/// change, so a check that refuses a common idle screen (a turn that ended
/// "Do you want me to …?") would hold every message to that session until
/// the operator next typed. A real permission dialog is still refused: its
/// selected choice sits on the cursor row as a numbered option, and it prints
/// the chrome hints.
pub fn input_prompt_ready(screen: &ScreenText) -> Result<(), String> {
    prompt_ready_with(screen, DIALOG_CHROME_MARKERS)
}

fn prompt_ready_with(screen: &ScreenText, dialog_markers: &[&str]) -> Result<(), String> {
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
        if let Some(marker) = dialog_markers.iter().find(|m| lower.contains(*m)) {
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

// ---------------------------------------------------------------------------
// The registry witness
// ---------------------------------------------------------------------------

/// How long one registry read may take before the witness reads UNKNOWN. The
/// read lists each account's `sessions/` dir and opens only the records of the
/// pids asked about. Its duration has not been measured; the budget matches
/// restart-readiness's record read (`RECORD_READ_TIMEOUT`, 3 s), which does
/// the same work.
pub const REGISTRY_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// At most this many registry reads run at once. A read that overran
/// [`REGISTRY_READ_TIMEOUT`] is abandoned but cannot be cancelled — it holds a
/// blocking-pool thread until the filesystem answers — and a graceful exit
/// probes every [`POLL_INTERVAL`] for up to a minute, so on a hung filesystem
/// an uncapped probe would leak a thread on every poll. Past the cap a probe
/// reads UNKNOWN instead. The cap is generous because concurrent probes are
/// ordinary (the steward, the message poller and a wind-down can all probe at
/// once).
pub const MAX_REGISTRY_READS_IN_FLIGHT: usize = 16;

static REGISTRY_READS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Which `sessions/` dirs the probe has reported unlistable.
static UNLISTABLE_SESSIONS_LOG: claude_activity::UnlistableLog = claude_activity::UnlistableLog::new(
    "graceful-exit probe",
    "a pane process whose image could be a JS-runtime Claude Code (node, nodejs, bun) and whose \
     record is not found elsewhere reads UNKNOWN until it is listable, in every pane on this box",
);

/// Claude Code's own session registry, read for one look at the process
/// table: the probe's SECOND witness that a pid is a live `claude` (plan
/// `2026-10-28-one-session-lifecycle-authority`, Phase 15 / R0c-2).
///
/// The first witness is the image name, and it cannot see an npm-installed
/// Claude Code, which runs as `node … cli.js`: `is_claude_image` reads that
/// `absent` (runner#2398's own recorded limit). Every consumer of the probe
/// therefore read such a pane as a bare shell — `NoLiveClaude`, which the
/// relay's end and the remote-create reaper answer with an ordinary
/// kill-close. Claude Code writes `<CLAUDE_CONFIG_DIR>/sessions/<pid>.json`
/// whatever binary it runs as, so a pid such a record names counts as a live
/// `claude` too, WHATEVER its image. The records are read through
/// [`claude_activity::read_records`] over the runner's known config dirs
/// ([`crate::terminal::transcript::find_claude_config_dirs`]), plus the
/// default config home (`<home>/.claude`) when it holds a `sessions/` dir, plus the own
/// `CLAUDE_CONFIG_DIR` of each candidate whose image could be a JS runtime
/// ([`crate::process_capture::process_tree::config_dir_of_pid`]).
///
/// What each reading of a pid's record makes of the pid:
///
/// | reading | the pid |
/// |---|---|
/// | `Parsed`, `Ambiguous` | a live `claude`, whatever its image: a record names it as the SAME process |
/// | `PidReused` (every dir listed) | judged by its image: `procStart` disagrees with the kernel, or `startedAt` predates the process — an earlier holder's record |
/// | `Missing` | judged by its image (see the residual below) |
/// | `LateStamp`, `Unparseable` | UNDECIDED: either may be the live process's own record |
/// | `Unlisted` (no record of THIS process — none, or only an earlier holder's — in any dir that listed, and some dir did not) | UNDECIDED |
/// | the read failed or overran its budget | UNDECIDED, for every pid |
///
/// A record found in a dir that DID list still decides while another dir
/// cannot be listed. An UNDECIDED pid makes the probe `Unreadable` only when
/// its image could BE a JS-runtime Claude Code — `node`, `nodejs`, `bun`, or
/// an image the snapshot could not read
/// ([`crate::process_capture::process_tree::may_be_js_runtime_claude`]) — and
/// no image-proven `claude` is its ancestor in the pane: under a native
/// `claude` (an `npx` MCP server, `npm test` from its Bash tool) the pane is
/// live either way, and any descendant already counts as that `claude`'s
/// child. Every other undecided pid is judged by its image, so a shell running
/// a native `claude` stays readable. What an unlistable dir still blinds:
/// every `node`/`bun` process outside a native `claude`, with no record in a
/// listed dir, in EVERY pane on the box while it lasts — box-wide for
/// npm-installed sessions whose record sits in that dir. It is logged once
/// each time a dir turns unlistable and once when it lists again.
///
/// "The same process" is `read_records`' own start-time check: `procStart`
/// against the kernel's start time on Linux, else `startedAt` against the
/// creation time the snapshot gives the pid. A record neither check can reach
/// is accepted on its pid. That errs toward `live`: every consumer that
/// closes, reaps or replaces a pane does so only on `absent`, and the two that
/// WRITE on `live` (a graceful exit holding an authority, the message poller)
/// each check the screen for Claude Code's own prompt between the probe and
/// the write.
///
/// **Residual `absent`.** A record under a config dir the runner never found
/// reads `Missing`. Discovery keeps only a home with a `projects/` dir, and on
/// Linux finds a `~/.claude-*` home only through the account roster (the
/// directory scan is `C:/claude/.claude-*`). The two extra dirs narrow that:
/// the default config home covers a first session there before it has a
/// `projects/` dir, and on Linux a candidate's own `CLAUDE_CONFIG_DIR` covers
/// any home it names. A `node` claude whose record sits under a home that is
/// neither — off Linux, under an undiscovered non-default home, or with an
/// environment the runner cannot read — still reads `absent`.
///
/// Opaque on purpose, and only [`PaneLook`] hands one out: the probe itself
/// ([`probe_from_snapshot`]) and the read ([`read_registry_witness`]) are
/// private to this module. So the only witness that names nothing comes from a
/// real read, and no caller can hand the probe an empty-but-"complete"
/// registry and quietly restore npm-reads-`absent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryWitness(Witness);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Witness {
    /// The registry was read.
    Read {
        /// Pids a record names as the same live process.
        named: BTreeSet<u32>,
        /// Pids whose record is UNDECIDED, and why.
        undecided: BTreeMap<u32, String>,
        /// The `sessions/` dirs that could not be listed, when any; every pid
        /// with no record in a listed dir is then in `undecided`.
        unlistable: Option<String>,
    },
    /// The registry could not be read at all: the read failed or overran its
    /// budget.
    Unreadable(String),
}

/// What the registry says about one pid.
enum RegistryAnswer {
    /// A record names it as the same live process.
    Named,
    /// No record names it (or only an earlier holder's does).
    NotNamed,
    /// The registry cannot say, and why.
    Undecided(String),
}

impl Default for RegistryWitness {
    /// No read taken is UNKNOWN — never "the registry named nothing".
    fn default() -> Self {
        Self::unreadable("Claude Code's session registry was not read")
    }
}

impl RegistryWitness {
    /// A registry that could not be read — the safe direction, so anyone may
    /// build one.
    pub fn unreadable(why: impl Into<String>) -> Self {
        Self(Witness::Unreadable(why.into()))
    }

    /// Why the registry could not answer in full — a `sessions/` dir that
    /// could not be listed, or a read that failed — or `None` when it could.
    pub fn incomplete_reason(&self) -> Option<&str> {
        match &self.0 {
            Witness::Unreadable(why) => Some(why.as_str()),
            Witness::Read { unlistable, .. } => unlistable.as_deref(),
        }
    }

    /// A look whose subtrees hold only `claude` images leaves the registry
    /// nothing to decide. Private: see the type's docs.
    fn nothing_to_decide() -> Self {
        Self(Witness::Read {
            named: BTreeSet::new(),
            undecided: BTreeMap::new(),
            unlistable: None,
        })
    }

    /// A complete read that named exactly `pids`. Test builds only.
    #[cfg(test)]
    pub(crate) fn naming(pids: &[u32]) -> Self {
        Self(Witness::Read {
            named: pids.iter().copied().collect(),
            undecided: BTreeMap::new(),
            unlistable: None,
        })
    }

    /// What the registry says about `pid`.
    fn answer_for(&self, pid: u32) -> RegistryAnswer {
        match &self.0 {
            Witness::Read {
                named, undecided, ..
            } => {
                if named.contains(&pid) {
                    RegistryAnswer::Named
                } else if let Some(why) = undecided.get(&pid) {
                    RegistryAnswer::Undecided(format!(
                        "pid {pid}'s Claude Code session record {why}, so whether that \
                         process is a JS-runtime `claude` is unknown"
                    ))
                } else {
                    RegistryAnswer::NotNamed
                }
            }
            Witness::Unreadable(why) => RegistryAnswer::Undecided(format!(
                "{why}, so whether pid {pid} in the pane is a JS-runtime `claude` is unknown"
            )),
        }
    }
}

/// The pids of `roots`' subtrees that the image does not already prove a
/// `claude` — every one the registry may name — each with the start time the
/// snapshot gives it, for the record's identity check.
fn registry_candidates(roots: &[u32], snapshot: &ProcessSnapshot) -> Vec<LivePid> {
    let mut seen = BTreeSet::new();
    roots
        .iter()
        .flat_map(|&root| inclusive_subtree_pids(root, snapshot))
        .filter(|&pid| !is_countable_claude(pid, snapshot) && seen.insert(pid))
        .map(|pid| LivePid {
            pid,
            process_started_ms: snapshot
                .creation_times
                .get(&pid)
                .copied()
                .filter(|&secs| secs > 0)
                .map(|secs| secs * 1000),
        })
        .collect()
}

/// The candidates whose image could be a JS-runtime Claude Code — the only
/// ones whose own `CLAUDE_CONFIG_DIR` is worth reading. A graceful exit probes
/// every half second, and a shell or a compiler is never the record's writer,
/// so reading every subtree pid's environment would be pure cost.
fn js_runtime_pids(candidates: &[LivePid], snapshot: &ProcessSnapshot) -> Vec<u32> {
    candidates
        .iter()
        .map(|candidate| candidate.pid)
        .filter(|&pid| may_be_js_runtime_claude(pid, snapshot))
        .collect()
}

/// The config dirs to read the registry from: the discovered ones, then each
/// of `pids`' own `CLAUDE_CONFIG_DIR` as `pid_config_dir` reports it, first
/// seen first and without repeats.
fn with_pid_config_dirs(
    discovered: Vec<PathBuf>,
    pids: &[u32],
    pid_config_dir: impl Fn(u32) -> Option<String>,
) -> Vec<PathBuf> {
    let mut dirs = discovered;
    for &pid in pids {
        if let Some(dir) = pid_config_dir(pid).map(PathBuf::from) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// `dirs` plus the DEFAULT Claude config home (`<home>/.claude`) when it holds
/// a `sessions/` dir and is not already listed. Discovery keeps only a home
/// with a `projects/` dir, so the default home's FIRST session — whose
/// `projects/` may not exist yet, and which carries no `CLAUDE_CONFIG_DIR` to
/// name it — would otherwise be read nowhere. Blocking file I/O (one
/// `is_dir`).
fn with_default_home(mut dirs: Vec<PathBuf>, default_home: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(default_home) = default_home {
        if !dirs.contains(&default_home) && default_home.join("sessions").is_dir() {
            dirs.push(default_home);
        }
    }
    dirs
}

/// The default Claude config home, through the runner's one resolver for it
/// (`commands::terminal::claude_default_config_home`, the allowlisted seam for
/// reading the user's home — see `ambient`'s `USER_GLOBAL_DIRS_ALLOWLIST`).
fn default_config_home() -> Option<PathBuf> {
    crate::commands::terminal::claude_default_config_home().map(PathBuf::from)
}

/// Read the witness for `candidates` from `config_dirs`. Blocking file I/O.
fn registry_witness_from(
    config_dirs: &[PathBuf],
    candidates: &[LivePid],
    proc_start: &dyn Fn(u32) -> Option<String>,
) -> RegistryWitness {
    let readings =
        claude_activity::read_records(config_dirs, candidates, proc_start, RecordDepth::Identity);
    UNLISTABLE_SESSIONS_LOG.observe(config_dirs, &readings.unlistable);
    // NO early return on an unlistable dir: a record found in a dir that DID
    // list is still a positive witness, and `read_records` has already turned
    // every pid with no record into `Unlisted`, which lands in `undecided`.
    let unlistable = (!readings.unlistable.is_empty()).then(|| {
        let dirs: Vec<String> = readings
            .unlistable
            .iter()
            .map(ToString::to_string)
            .collect();
        format!(
            "Claude Code's session registry could not be listed in full ({})",
            dirs.join("; ")
        )
    });
    let mut named = BTreeSet::new();
    let mut undecided = BTreeMap::new();
    for (pid, reading) in readings.by_pid {
        // Exhaustive on purpose: a new reading must be placed in the table on
        // `RegistryWitness`, not fall through to "not a claude".
        match reading {
            RecordReading::Parsed(_) | RecordReading::Ambiguous => {
                named.insert(pid);
            }
            RecordReading::Unparseable => {
                undecided.insert(pid, "could not be read or parsed".to_string());
            }
            RecordReading::LateStamp => {
                undecided.insert(
                    pid,
                    "is stamped later than its process's start allows".to_string(),
                );
            }
            RecordReading::Unlisted => {
                undecided.insert(
                    pid,
                    "is in no dir that listed, and may sit in a sessions dir that could not be listed"
                        .to_string(),
                );
            }
            RecordReading::Missing | RecordReading::PidReused => {}
        }
    }
    RegistryWitness(Witness::Read {
        named,
        undecided,
        unlistable,
    })
}

/// The registry witness for `roots`' subtrees in `snapshot`, over the runner's
/// known config dirs plus the extra ones [`RegistryWitness`] names: off the
/// async executor, bounded by [`REGISTRY_READ_TIMEOUT`] and
/// [`MAX_REGISTRY_READS_IN_FLIGHT`]. A look whose subtrees hold only `claude`
/// images needs no read and takes none. Private: [`PaneLook::take`] is the
/// door. A thin wrapper naming production's sources for
/// [`read_registry_witness_over`].
async fn read_registry_witness(roots: &[u32], snapshot: &ProcessSnapshot) -> RegistryWitness {
    read_registry_witness_over(
        roots,
        snapshot,
        crate::terminal::transcript::find_claude_config_dirs,
        default_config_home,
        crate::process_capture::process_tree::config_dir_of_pid,
        claude_activity::proc_start_ticks,
    )
    .await
}

/// [`read_registry_witness`] with where it looks injected: the discovered
/// config dirs, the default config home (read when it holds a `sessions/`
/// dir) and a pid's own `CLAUDE_CONFIG_DIR` (asked only of the candidates
/// whose image could be a JS runtime).
async fn read_registry_witness_over<F, H, E>(
    roots: &[u32],
    snapshot: &ProcessSnapshot,
    discovered: F,
    default_home: H,
    pid_config_dir: E,
    proc_start: fn(u32) -> Option<String>,
) -> RegistryWitness
where
    F: FnOnce() -> Vec<PathBuf> + Send + 'static,
    H: FnOnce() -> Option<PathBuf> + Send + 'static,
    E: Fn(u32) -> Option<String> + Send + 'static,
{
    read_registry_witness_with(
        roots,
        snapshot,
        move |js_runtime_pids: &[u32]| {
            with_pid_config_dirs(
                with_default_home(discovered(), default_home()),
                js_runtime_pids,
                pid_config_dir,
            )
        },
        proc_start,
        REGISTRY_READ_TIMEOUT,
        &REGISTRY_READS_IN_FLIGHT,
    )
    .await
}

/// [`read_registry_witness`] with every input injected. `config_dirs` is
/// handed the candidates whose image could be a JS runtime, and is resolved
/// INSIDE the blocking read, because finding the dirs reads the filesystem
/// too.
async fn read_registry_witness_with<D>(
    roots: &[u32],
    snapshot: &ProcessSnapshot,
    config_dirs: D,
    proc_start: fn(u32) -> Option<String>,
    timeout: Duration,
    in_flight: &'static AtomicUsize,
) -> RegistryWitness
where
    D: FnOnce(&[u32]) -> Vec<PathBuf> + Send + 'static,
{
    let candidates = registry_candidates(roots, snapshot);
    if candidates.is_empty() {
        return RegistryWitness::nothing_to_decide();
    }
    let js_runtime = js_runtime_pids(&candidates, snapshot);
    let Some(slot) = InFlightRead::claim(in_flight) else {
        return RegistryWitness::unreadable(format!(
            "{MAX_REGISTRY_READS_IN_FLIGHT} earlier Claude Code session-registry reads are still \
             in flight"
        ));
    };
    let read = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        // Released when the read ENDS (or unwinds), not when the caller stops
        // waiting for it.
        let _slot = slot;
        registry_witness_from(
            &config_dirs(js_runtime.as_slice()),
            &candidates,
            &proc_start,
        )
    });
    match tokio::time::timeout(timeout, read).await {
        Ok(Ok(witness)) => witness,
        Ok(Err(e)) => RegistryWitness::unreadable(format!(
            "the Claude Code session-registry read failed ({e})"
        )),
        Err(_) => RegistryWitness::unreadable(format!(
            "the Claude Code session-registry read exceeded {timeout:?}"
        )),
    }
}

/// One of [`MAX_REGISTRY_READS_IN_FLIGHT`] read slots, given back on drop.
struct InFlightRead(&'static AtomicUsize);

impl InFlightRead {
    fn claim(counter: &'static AtomicUsize) -> Option<Self> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_REGISTRY_READS_IN_FLIGHT).then_some(n + 1)
            })
            .ok()
            .map(|_| Self(counter))
    }
}

impl Drop for InFlightRead {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One look at the panes: a snapshot of the whole process table and the
/// registry witness read for the subtrees it will judge. The one public door
/// to the probe: [`probe_claude_under`] is its one-pane form, and a caller
/// judging several panes against one snapshot (the steward roster, the
/// operator-touch scan) takes one look for all of them.
#[derive(Debug, Default)]
pub struct PaneLook {
    snapshot: ProcessSnapshot,
    registry: RegistryWitness,
    /// The pane roots the registry was read for. A pane outside them was never
    /// asked about, so its probe is `Unreadable` — a registry read for OTHER
    /// subtrees says nothing about this one's `node` processes.
    roots: BTreeSet<u32>,
}

impl PaneLook {
    /// Snapshot the whole process table, then read the registry for the
    /// subtrees of `roots`.
    pub async fn take(roots: &[u32]) -> Self {
        let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
        let registry = read_registry_witness(roots, &snapshot).await;
        Self {
            snapshot,
            registry,
            roots: roots.iter().copied().collect(),
        }
    }

    /// A look assembled from its parts.
    #[cfg(test)]
    pub(crate) fn from_parts(
        roots: &[u32],
        snapshot: ProcessSnapshot,
        registry: RegistryWitness,
    ) -> Self {
        Self {
            snapshot,
            registry,
            roots: roots.iter().copied().collect(),
        }
    }

    /// The process table this look took.
    pub fn snapshot(&self) -> &ProcessSnapshot {
        &self.snapshot
    }

    /// The registry witness this look read.
    pub fn registry(&self) -> &RegistryWitness {
        &self.registry
    }

    /// [`probe_from_snapshot`] for the pane rooted at `root_pid`.
    pub fn probe(&self, root_pid: u32, tracked: &[ProcIdentity]) -> ClaudeProbe {
        if !self.roots.contains(&root_pid) {
            return ClaudeProbe::Unreadable(format!(
                "Claude Code's session registry was not read for the pane rooted at pid \
                 {root_pid}"
            ));
        }
        probe_from_snapshot(root_pid, &self.snapshot, tracked, &self.registry)
    }
}

/// Every live `claude` in the inclusive subtree, root first then
/// breadth-first: a pid whose image is a `claude`, or one `registry` names
/// (whatever its image). `Err` when the registry cannot settle a pid whose
/// image could be a JS-runtime Claude Code and which no image-proven `claude`
/// in the pane is an ancestor of; any other undecided pid is judged by its
/// image (see [`RegistryWitness`]).
fn subtree_claude_pids(
    root_pid: u32,
    snapshot: &ProcessSnapshot,
    registry: &RegistryWitness,
) -> Result<Vec<u32>, String> {
    let subtree = inclusive_subtree_pids(root_pid, snapshot);
    // Every descendant of an image-proven `claude` in the pane. Such a pid
    // cannot change the probe's verdict: the pane is live through its
    // ancestor, and the pid already counts among that `claude`'s children.
    let under_native_claude: BTreeSet<u32> = subtree
        .iter()
        .copied()
        .filter(|&pid| is_countable_claude(pid, snapshot))
        .flat_map(|claude| {
            inclusive_subtree_pids(claude, snapshot)
                .into_iter()
                .filter(move |&pid| pid != claude)
        })
        .collect();
    let mut pids = Vec::new();
    for pid in subtree {
        let live = is_countable_claude(pid, snapshot)
            || match registry.answer_for(pid) {
                RegistryAnswer::Named => true,
                RegistryAnswer::NotNamed => false,
                RegistryAnswer::Undecided(why)
                    if may_be_js_runtime_claude(pid, snapshot)
                        && !under_native_claude.contains(&pid) =>
                {
                    return Err(why)
                }
                RegistryAnswer::Undecided(_) => false,
            };
        if live {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// Pure probe over one snapshot of the whole process table and the registry
/// witness read for it ([`read_registry_witness`]).
///
/// A `claude` is a pid whose image is `claude`, or one a Claude Code registry
/// record names ([`RegistryWitness`]). When the registry cannot settle a pid
/// in the subtree that the image leaves open, the probe is `Unreadable` —
/// never a reading that silently leaves that pid out.
fn probe_from_snapshot(
    root_pid: u32,
    snapshot: &ProcessSnapshot,
    tracked: &[ProcIdentity],
    registry: &RegistryWitness,
) -> ClaudeProbe {
    if snapshot.parent_map.is_empty() {
        return ClaudeProbe::Unreadable(
            "the process table is unreadable (empty parent map)".to_string(),
        );
    }
    let pids = match subtree_claude_pids(root_pid, snapshot, registry) {
        Ok(pids) => pids,
        Err(detail) => return ClaudeProbe::Unreadable(detail),
    };
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
/// process table and the Claude Code registry read for that pane's subtree
/// ([`PaneLook`]).
pub async fn probe_claude_under(root_pid: Option<u32>, tracked: Vec<ProcIdentity>) -> ClaudeProbe {
    let Some(root) = root_pid else {
        return ClaudeProbe::Unreadable(
            "the pane has no local process id (a remote pane), so its subtree cannot be observed"
                .to_string(),
        );
    };
    // Test builds only: a pane registered with [`fake_process_table`] under a
    // pid no real process can hold reads its scripted view, or runs this same
    // derivation over its scripted table, instead of the OS table. Every real
    // pid falls through to the look below.
    #[cfg(test)]
    if let Some(scripted) = fake_process_table::probe(root, &tracked).await {
        return scripted;
    }
    PaneLook::take(&[root]).await.probe(root, &tracked)
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
    /// A live `claude` IS in the pane, and the caller presented no
    /// [`CloseAuthority`]. Nothing was typed and nothing was closed (plan
    /// `2026-10-28-one-session-lifecycle-authority`, Phase 14 / R0a).
    ///
    /// On every build that ships today, EVERY caller that finds a live
    /// `claude` gets this answer. No production code can mint an authority
    /// until the lifecycle Closer does (Phase 93).
    NoCloseAuthority {
        /// The `claude` pids the probe found.
        claude_pids: Vec<u32>,
    },
    /// The process table could not be read before starting; nothing was typed
    /// or closed.
    ProbeUnavailable { detail: String },
    /// Writing to the pane failed; nothing was closed.
    WriteFailed {
        error: String,
        claude_pids: Vec<u32>,
    },
    /// `claude` left, but the pane could NOT be proven clear at the moment of
    /// the close, so nothing was killed and the tab was not closed. See
    /// [`CloseTabResult`].
    CloseRefused {
        waited_ms: u64,
        /// The `claude` pids present when `/exit` was typed.
        claude_pids: Vec<u32>,
        reason: String,
    },
    /// `claude` left and the close was ATTEMPTED, but what it achieved is not
    /// known — the closing task panicked or was cancelled, or the pane was
    /// already gone from the manager. Distinct from [`Self::CloseRefused`],
    /// which is the positive statement that the pane was left untouched: this
    /// one asserts nothing about the pane either way.
    CloseOutcomeUnknown {
        waited_ms: u64,
        claude_pids: Vec<u32>,
        detail: String,
    },
}

impl GracefulExitOutcome {
    /// The outcome's wire word — exactly the `outcome` tag it serializes
    /// with — for log lines and refusal reasons, so a log reader greps the same
    /// word a JSON reader sees (`no_close_authority`, `exit_stuck`, …).
    ///
    /// An exhaustive `match`, so a new variant cannot ship unnamed.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Exited { .. } => "exited",
            Self::ExitStuck { .. } => "exit_stuck",
            Self::Refused { .. } => "refused",
            Self::NoLiveClaude => "no_live_claude",
            Self::NoCloseAuthority { .. } => "no_close_authority",
            Self::ProbeUnavailable { .. } => "probe_unavailable",
            Self::WriteFailed { .. } => "write_failed",
            Self::CloseRefused { .. } => "close_refused",
            Self::CloseOutcomeUnknown { .. } => "close_outcome_unknown",
        }
    }
}

/// What the close step did — the answer to the ONE question `drive_graceful_exit` cannot ask
/// itself, because the pane is reached only through the injected effects.
///
/// ## Why the close is allowed to refuse (Phase 1 review, hazard 2)
///
/// Between the last "gone" probe and the close there is a real window in which
/// something can start a NEW `claude` in the pane. `TerminalSession::
/// close_after_graceful_exit` decides whether to kill from `is_alive()`, which
/// is a coarse proxy — it answers "is the pane's own shell running", not "is a
/// live `claude` in this pane". Phase 1 could tolerate the window because
/// nothing drove this primitive; the Phase 4 wind-down executor drives it on
/// looping and steward panes, which are exactly the kinds with an external
/// relauncher. The closer therefore re-proves the pane clear immediately before
/// closing and REFUSES otherwise, and this type carries that refusal back so
/// the outcome is honest rather than a false `Exited`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseTabResult {
    /// The pane was closed.
    Closed,
    /// The pane was left exactly as it was — a POSITIVE statement, only
    /// returnable by a closer that decided not to act before acting.
    Refused { reason: String },
    /// The close was attempted and its result is not known. Never say
    /// [`Self::Refused`] here: a closer that got as far as removing the pane
    /// and then lost its task cannot claim the pane is untouched.
    Unknown { detail: String },
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
///
/// `authority` is the caller's [`CloseAuthority`]. It is checked once the first
/// probe has found a live `claude`, and before any byte reaches it. `None`
/// there answers [`GracefulExitOutcome::NoCloseAuthority`] with nothing typed
/// and nothing closed. A pane with no live `claude` answers `NoLiveClaude`, and
/// an unreadable table answers `ProbeUnavailable`, whatever was presented.
///
/// `pub(super)`: only `crate::terminal` may drive the protocol, and everything
/// else goes through `TerminalManager::graceful_exit`. The close-authority scan
/// (`runner_close_sites.rs`) checks the authority every caller passes. It reads
/// a call of this fn in every file, by a name no other fn in the crate uses, so
/// a macro defined elsewhere that expands to a call is read where it is
/// written.
pub(super) async fn drive_graceful_exit<W, S, P, PF, C, CF>(
    mut write: W,
    mut screen: S,
    mut probe: P,
    close_tab: C,
    timing: ExitTiming,
    authority: Option<CloseAuthority>,
) -> GracefulExitOutcome
where
    W: FnMut(&[u8]) -> Result<(), String>,
    S: FnMut() -> ScreenText,
    P: FnMut(Vec<ProcIdentity>) -> PF,
    PF: Future<Output = ClaudeProbe>,
    C: FnOnce() -> CF,
    CF: Future<Output = CloseTabResult>,
{
    // (1) The pane's processes.
    let initial = match probe(Vec::new()).await {
        ClaudeProbe::Unreadable(detail) => return GracefulExitOutcome::ProbeUnavailable { detail },
        ClaudeProbe::Readable(view) => view,
    };
    let claude_pids = pids_of(&initial.subtree_claude);
    if initial.subtree_claude.is_empty() {
        return GracefulExitOutcome::NoLiveClaude;
    }

    // (1b) The close authority (R0a). A live `claude` has just been found and
    // nothing has been typed, so this is the last point at which a refusal
    // touches nothing. It sits BEFORE the nested-claude and child refusals on
    // purpose. The refusal must not depend on any predicate a later change can
    // relax: a caller without an authority is refused here however few
    // children its `claude` has. And this is the refusal a box that restarts
    // into this build logs, so it shows up as `no_close_authority` in the
    // wind-down log. The binding is held to the end of the protocol, because
    // the authority covers this one close attempt and no other.
    let Some(_authority) = authority else {
        return GracefulExitOutcome::NoCloseAuthority { claude_pids };
    };

    let n = initial.subtree_claude.len();
    if n > 1 {
        return GracefulExitOutcome::Refused {
            reason: format!(
                "{n} claude processes in the pane — a nested claude does not leave with /exit"
            ),
            claude_pids,
        };
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
                        // (5) Gone. The closer re-proves the pane clear at the
                        // moment of the close and may refuse — see
                        // [`CloseTabResult`].
                        return match close_tab().await {
                            CloseTabResult::Closed => GracefulExitOutcome::Exited {
                                waited_ms: millis(started.elapsed()),
                                claude_pids,
                            },
                            CloseTabResult::Refused { reason } => {
                                GracefulExitOutcome::CloseRefused {
                                    waited_ms: millis(started.elapsed()),
                                    claude_pids,
                                    reason,
                                }
                            }
                            CloseTabResult::Unknown { detail } => {
                                GracefulExitOutcome::CloseOutcomeUnknown {
                                    waited_ms: millis(started.elapsed()),
                                    claude_pids,
                                    detail,
                                }
                            }
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

/// Test-only scripted process tables, keyed by a pane root pid that no real
/// process can hold.
///
/// [`drive_graceful_exit`]'s own tests script `probe` directly. A test that must enter
/// through a CALLER of the primitive cannot: the wind-down arms, the steward
/// stop, the relay's end and the debug door all reach `probe_claude_under`
/// through `TerminalManager::graceful_exit`, which reads the real OS table.
/// Registering a pane here gives that path either the same `PaneProcesses`
/// view the driver tests use ([`register`](fake_process_table::register)), or
/// a raw process table and registry dirs that the PRODUCTION derivation —
/// the registry read and [`probe_from_snapshot`] — runs over
/// ([`register_table`](fake_process_table::register_table)). Everything
/// between the caller and the probe stays production code.
#[cfg(test)]
pub(crate) mod fake_process_table {
    use super::{ClaudeProbe, PaneProcesses, ProcIdentity};
    use crate::process_capture::process_tree::ProcessSnapshot;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};

    /// First fake root pid, above Linux's `pid_max` ceiling (2^22). The table
    /// answers only for a pid a test registered, so it can shadow a real
    /// process only if a test also probes a real pane holding that same pid.
    const FAKE_PID_BASE: u32 = 4_000_000_000;

    static NEXT_PID: AtomicU32 = AtomicU32::new(FAKE_PID_BASE);

    /// What a registered pane answers with.
    #[derive(Clone)]
    enum Scripted {
        /// A finished view: the derivation is skipped.
        View(PaneProcesses),
        /// A raw table the production derivation runs over.
        Table(Arc<ScriptedTable>),
    }

    /// A scripted process table, plus the Claude Code config dirs whose
    /// registry the probe reads and the kernel start-time read it checks a
    /// record's identity against.
    pub(crate) struct ScriptedTable {
        pub(crate) snapshot: ProcessSnapshot,
        pub(crate) config_dirs: Vec<PathBuf>,
        pub(crate) proc_start: fn(u32) -> Option<String>,
    }

    fn table() -> &'static Mutex<HashMap<u32, Scripted>> {
        static TABLE: OnceLock<Mutex<HashMap<u32, Scripted>>> = OnceLock::new();
        TABLE.get_or_init(Mutex::default)
    }

    fn insert(root_pid: u32, scripted: Scripted) -> FakePane {
        table()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(root_pid, scripted);
        FakePane { root_pid }
    }

    /// A registered pane. Unregistered when dropped, so tests running in
    /// parallel never see each other's panes (each has its own pid).
    #[derive(Debug)]
    pub(crate) struct FakePane {
        pub(crate) root_pid: u32,
    }

    impl Drop for FakePane {
        fn drop(&mut self) {
            table()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&self.root_pid);
        }
    }

    /// Register a pane whose every probe reads `view`, under a fresh fake root
    /// pid.
    pub(crate) fn register(view: PaneProcesses) -> FakePane {
        let root_pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        insert(root_pid, Scripted::View(view))
    }

    /// Register a pane whose every probe runs the PRODUCTION derivation over
    /// the table `build` returns for the fresh fake root pid it is handed: the
    /// registry read over the table's config dirs, then
    /// [`super::probe_from_snapshot`].
    pub(crate) fn register_table(build: impl FnOnce(u32) -> ScriptedTable) -> FakePane {
        let root_pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        let scripted = Scripted::Table(Arc::new(build(root_pid)));
        insert(root_pid, scripted)
    }

    /// The scripted probe for `root_pid`, or `None` for any pid nobody
    /// registered (every real one). For a view: of the identities asked
    /// about, those the view holds (in its subtree or as alive elsewhere) read
    /// alive. For a table: whatever the production derivation reads.
    pub(super) async fn probe(root_pid: u32, tracked: &[ProcIdentity]) -> Option<ClaudeProbe> {
        let scripted = table()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&root_pid)?
            .clone();
        Some(match scripted {
            Scripted::View(view) => {
                let tracked_alive = tracked
                    .iter()
                    .copied()
                    .filter(|id| {
                        view.subtree_claude.contains(id) || view.tracked_alive.contains(id)
                    })
                    .collect();
                ClaudeProbe::Readable(PaneProcesses {
                    tracked_alive,
                    ..view
                })
            }
            Scripted::Table(scripted) => {
                let config_dirs = scripted.config_dirs.clone();
                let registry = super::read_registry_witness_with(
                    &[root_pid],
                    &scripted.snapshot,
                    move |_| config_dirs,
                    scripted.proc_start,
                    super::REGISTRY_READ_TIMEOUT,
                    &super::REGISTRY_READS_IN_FLIGHT,
                )
                .await;
                super::probe_from_snapshot(root_pid, &scripted.snapshot, tracked, &registry)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
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

    /// An idle screen whose finished turn ended with a question to the
    /// operator, directly above the empty input box.
    fn turn_ending_in_a_question() -> ScreenText {
        screen(
            vec![
                "● Done. The build is green. Do you want me to run the tests?".to_string(),
                "  (the allowlist was left unchanged)".to_string(),
                edge(),
                "❯\u{a0}Try \"write a test for <filepath>\"".to_string(),
                edge(),
                "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents".to_string(),
            ],
            3,
            2,
        )
    }

    #[test]
    fn input_prompt_ready_admits_a_turn_that_ended_in_a_question() {
        // The two checks differ ONLY in the dialog-text scan: `/exit` refuses
        // prose-shaped markers (a postponed wind-down is cheap), a message
        // paste must not (the idle screen would refuse every retry).
        let s = turn_ending_in_a_question();
        assert!(exit_prompt_ready(&s).is_err());
        assert_eq!(input_prompt_ready(&s), Ok(()));
        // Everything else is shared: the empty prompt passes both, and a
        // draft, a real permission dialog and a chrome hint refuse both.
        assert_eq!(input_prompt_ready(&empty_prompt()), Ok(()));
        assert!(input_prompt_ready(&draft_prompt()).is_err());
        assert!(input_prompt_ready(&permission_dialog()).is_err());
        let mut hinted = empty_prompt();
        hinted.lines[0] = "  Enter to confirm · Esc to cancel".to_string();
        assert!(input_prompt_ready(&hinted).is_err());
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
            probe_from_snapshot(
                100,
                &ProcessSnapshot::default(),
                &[id(200, 5)],
                &RegistryWitness::naming(&[])
            ),
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
        let ClaudeProbe::Readable(view) =
            probe_from_snapshot(100, &snap, &[id(200, 20)], &RegistryWitness::naming(&[]))
        else {
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
        let ClaudeProbe::Readable(view) =
            probe_from_snapshot(100, &snap, &[id(200, 20)], &RegistryWitness::naming(&[]))
        else {
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

    // ---- the registry witness (R0c-2) -----------------------------------------

    /// The pid of the `node` process an npm-installed Claude Code runs as.
    const NODE: u32 = 7_001;

    /// No `/proc` entry for a scripted pid: every record identity check falls
    /// to `startedAt`.
    fn no_kernel(_: u32) -> Option<String> {
        None
    }

    /// A Claude Code config dir `root/name` whose `sessions/` holds `records`
    /// (pid → file body).
    fn config_dir(root: &Path, name: &str, records: &[(u32, String)]) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        for (pid, body) in records {
            std::fs::write(dir.join("sessions").join(format!("{pid}.json")), body).unwrap();
        }
        dir
    }

    /// The record Claude Code writes for a process that began at `started_s`
    /// (epoch seconds): `startedAt` one second later, inside the +0.9 s to
    /// +78 s lag `claude_activity::START_LEAD_MS` documents as measured.
    fn npm_record(pid: u32, started_s: i64) -> String {
        npm_record_stamped(pid, started_s * 1000 + 1_000)
    }

    /// The same record with an explicit `startedAt` (epoch millis).
    fn npm_record_stamped(pid: u32, started_at_ms: i64) -> String {
        format!(
            r#"{{"pid":{pid},"sessionId":"npm-session","status":"idle","startedAt":{started_at_ms}}}"#
        )
    }

    /// A pane whose shell runs one `node` process — an npm-installed
    /// `claude`, when a record in `config_dirs` names it — as a scripted table
    /// the production derivation runs over.
    fn node_pane(
        config_dirs: Vec<PathBuf>,
        proc_start: fn(u32) -> Option<String>,
    ) -> fake_process_table::FakePane {
        fake_process_table::register_table(|root| fake_process_table::ScriptedTable {
            snapshot: snapshot(&[(root, 1, "bash", 10), (NODE, root, "node", 20)]),
            config_dirs,
            proc_start,
        })
    }

    /// The probe reads a pid that a Claude Code registry record names as
    /// `live`, whatever its image: an npm-installed Claude Code runs as `node
    /// … cli.js`. Without the record the very same pane reads `absent` — the
    /// image witness's own recorded limit (runner#2398), which this witness
    /// exists to close.
    #[tokio::test]
    async fn a_pid_with_a_registry_record_reads_live_whatever_its_image() {
        let root = tempfile::tempdir().unwrap();
        let recorded = config_dir(root.path(), ".claude-npm", &[(NODE, npm_record(NODE, 20))]);
        let pane = node_pane(vec![recorded], no_kernel);
        assert_eq!(
            probe_claude_under(Some(pane.root_pid), vec![]).await,
            readable(&[id(NODE, 20)], 0, &[]),
            "a registry-named `node` is a live claude"
        );

        let unrecorded = config_dir(root.path(), ".claude-other", &[]);
        let bare = node_pane(vec![unrecorded], no_kernel);
        assert_eq!(
            probe_claude_under(Some(bare.root_pid), vec![]).await,
            readable(&[], 0, &[]),
            "a `node` no record names is judged by its image"
        );
    }

    /// A `sessions/` dir that exists and cannot be listed may hold the npm
    /// session's record, so the probe reads UNKNOWN — never `absent`.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unlistable_sessions_dir_reads_unknown_never_absent() {
        let root = tempfile::tempdir().unwrap();
        let hidden = config_dir(root.path(), ".claude-npm", &[(NODE, npm_record(NODE, 20))]);
        let Some(_restore) = claude_activity::unlistable_fixture::unreadable_sessions_dir(&hidden)
        else {
            return;
        };
        let pane = node_pane(vec![hidden], no_kernel);
        match probe_claude_under(Some(pane.root_pid), vec![]).await {
            ClaudeProbe::Unreadable(detail) => {
                assert!(detail.contains("could not be listed"), "{detail}");
            }
            other => panic!("an unlistable registry must read unknown, got {other:?}"),
        }
    }

    /// A `sessions` path that is not a directory is unlistable too, and a
    /// pane whose shell runs a `node` reads UNKNOWN under it. Not gated on the
    /// platform or the user, because unlike a mode-000 dir it is unlistable to
    /// root as well. Measured on Linux; on Windows, where an npm `claude` is
    /// `node.exe`, this test's CI run is the first evidence.
    #[tokio::test]
    async fn a_sessions_path_that_is_not_a_directory_reads_unknown() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".claude-file");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sessions"), "not a directory").unwrap();
        let pane = node_pane(vec![dir], no_kernel);
        assert!(matches!(
            probe_claude_under(Some(pane.root_pid), vec![]).await,
            ClaudeProbe::Unreadable(_)
        ));
    }

    /// A `node` UNDER a native `claude` (an `npx` MCP server, `npm test` run
    /// from its Bash tool) does not blind the pane when the registry cannot
    /// answer: the pane is live through its `claude`, and the `node` already
    /// counts as that `claude`'s child.
    #[tokio::test]
    async fn a_node_under_a_native_claude_does_not_blind_the_pane() {
        const NATIVE: u32 = 7_002;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".claude-file");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sessions"), "not a directory").unwrap();
        let pane = fake_process_table::register_table(|r| fake_process_table::ScriptedTable {
            snapshot: snapshot(&[
                (r, 1, "bash", 10),
                (NATIVE, r, "claude", 20),
                (NODE, NATIVE, "node", 30),
            ]),
            config_dirs: vec![dir],
            proc_start: no_kernel,
        });
        assert_eq!(
            probe_claude_under(Some(pane.root_pid), vec![]).await,
            readable(&[id(NATIVE, 20)], 1, &[])
        );
    }

    /// A record found in a dir that DID list is a positive witness even while
    /// another dir cannot be listed: the npm session it names reads `live`,
    /// and the look still says the registry was incomplete.
    #[tokio::test]
    async fn a_record_in_a_listed_dir_decides_beside_an_unlistable_one() {
        let root = tempfile::tempdir().unwrap();
        let recorded = config_dir(root.path(), ".claude-npm", &[(NODE, npm_record(NODE, 20))]);
        let broken = root.path().join(".claude-file");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("sessions"), "not a directory").unwrap();
        let snap = snapshot(&[(100, 1, "bash", 10), (NODE, 100, "node", 20)]);
        let witness = read_registry_witness_with(
            &[100],
            &snap,
            move |_: &[u32]| vec![recorded, broken],
            no_kernel,
            REGISTRY_READ_TIMEOUT,
            &REGISTRY_READS_IN_FLIGHT,
        )
        .await;
        assert!(
            witness
                .incomplete_reason()
                .is_some_and(|why| why.contains("could not be listed")),
            "{witness:?}"
        );
        assert_eq!(
            probe_from_snapshot(100, &snap, &[], &witness),
            readable(&[id(NODE, 20)], 0, &[])
        );
    }

    /// The default config home (`<home>/.claude`) is read when it holds a
    /// `sessions/` dir, even when discovery skipped it (no `projects/` yet),
    /// so the default home's first npm session is seen. Without a `sessions/`
    /// dir, or already listed, it adds nothing.
    #[test]
    fn the_default_home_is_read_when_it_holds_a_sessions_dir() {
        let home = tempfile::tempdir().unwrap();
        let default_home = home.path().join(".claude");
        let other = PathBuf::from("/elsewhere/.claude-x");
        assert_eq!(
            with_default_home(vec![other.clone()], Some(default_home.clone())),
            vec![other.clone()],
            "no sessions/ dir yet: nothing to read"
        );
        std::fs::create_dir_all(default_home.join("sessions")).unwrap();
        assert_eq!(
            with_default_home(vec![other.clone()], Some(default_home.clone())),
            vec![other, default_home.clone()]
        );
        assert_eq!(
            with_default_home(vec![default_home.clone()], Some(default_home.clone())),
            vec![default_home],
            "already discovered: listed once"
        );
        assert!(with_default_home(Vec::new(), None).is_empty());
    }

    /// The read's own wiring, over a temp default home: the default home is
    /// read (its npm record makes the `node` live), and only the JS-runtime
    /// candidate is asked for its own `CLAUDE_CONFIG_DIR` — not the shell, not
    /// the compiler.
    #[tokio::test]
    async fn the_read_wires_the_default_home_and_asks_only_js_runtimes_for_their_dir() {
        let home = tempfile::tempdir().unwrap();
        let default_home = config_dir(home.path(), ".claude", &[(NODE, npm_record(NODE, 20))]);
        let asked: Arc<Mutex<Vec<u32>>> = Arc::default();
        let asked_by_read = Arc::clone(&asked);
        let snap = snapshot(&[
            (100, 1, "bash", 10),
            (NODE, 100, "node", 20),
            (220, 100, "rustc", 22),
        ]);
        let witness = read_registry_witness_over(
            &[100],
            &snap,
            Vec::new,
            move || Some(default_home),
            move |pid| {
                asked_by_read.lock().unwrap().push(pid);
                None
            },
            no_kernel,
        )
        .await;
        assert_eq!(
            probe_from_snapshot(100, &snap, &[], &witness),
            readable(&[id(NODE, 20)], 0, &[]),
            "the default home's record was read"
        );
        assert_eq!(*asked.lock().unwrap(), vec![NODE]);
    }

    /// While another dir cannot be listed, a stale record an EARLIER holder of
    /// the pid left in a dir that did list says nothing about the live
    /// process: its own record may sit in the unlistable dir, so the npm
    /// session reads UNKNOWN, never `absent`.
    #[tokio::test]
    async fn a_reused_record_beside_an_unlistable_dir_reads_unknown() {
        fn kernel_disagrees(_: u32) -> Option<String> {
            Some("500".to_string())
        }
        let root = tempfile::tempdir().unwrap();
        let stale = config_dir(
            root.path(),
            ".claude-a",
            &[(
                NODE,
                format!(r#"{{"pid":{NODE},"sessionId":"old","status":"idle","procStart":"400"}}"#),
            )],
        );
        let broken = root.path().join(".claude-file");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("sessions"), "not a directory").unwrap();
        let pane = node_pane(vec![stale, broken], kernel_disagrees);
        assert!(
            matches!(
                probe_claude_under(Some(pane.root_pid), vec![]).await,
                ClaudeProbe::Unreadable(_)
            ),
            "a reused record must not settle the npm session absent"
        );
    }

    /// Only a candidate whose image could be a JS runtime has its environment
    /// read for its own config dir; a shell or a compiler never wrote a record.
    #[test]
    fn only_js_runtime_candidates_have_their_config_dir_read() {
        let snap = snapshot(&[
            (100, 1, "bash", 10),
            (210, 100, "node", 21),
            (220, 100, "rustc", 22),
            (230, 100, "bun.exe", 23),
        ]);
        let candidates = registry_candidates(&[100], &snap);
        assert_eq!(js_runtime_pids(&candidates, &snap), vec![210, 230]);
    }

    /// A shell running a NATIVE `claude` stays readable — and live — under a
    /// `sessions` dir that cannot be listed: the image settles every process
    /// in the pane, so one bad account dir does not blind the probe to it.
    #[tokio::test]
    async fn a_shell_running_a_native_claude_stays_readable_under_an_unlistable_dir() {
        const NATIVE: u32 = 7_002;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".claude-file");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sessions"), "not a directory").unwrap();
        let pane = fake_process_table::register_table(|r| fake_process_table::ScriptedTable {
            snapshot: snapshot(&[(r, 1, "bash", 10), (NATIVE, r, "claude", 20)]),
            config_dirs: vec![dir],
            proc_start: no_kernel,
        });
        assert_eq!(
            probe_claude_under(Some(pane.root_pid), vec![]).await,
            readable(&[id(NATIVE, 20)], 0, &[])
        );
    }

    /// A record stamped MORE than `START_LAG_MS` after its process began (a
    /// slow start, a startup dialog) is not reuse — a reused pid's stale
    /// record always predates the live process — so the npm session it names
    /// is never read `absent`. It is not proven either, so the pane reads
    /// UNKNOWN.
    #[tokio::test]
    async fn a_late_started_at_never_reads_the_npm_session_absent() {
        let root = tempfile::tempdir().unwrap();
        let six_minutes_late = 20 * 1000 + 6 * 60 * 1000;
        let late = config_dir(
            root.path(),
            ".claude-npm",
            &[(NODE, npm_record_stamped(NODE, six_minutes_late))],
        );
        let pane = node_pane(vec![late], no_kernel);
        match probe_claude_under(Some(pane.root_pid), vec![]).await {
            ClaudeProbe::Unreadable(detail) => {
                assert!(detail.contains("stamped later"), "{detail}");
            }
            other => panic!("a late-stamped record must not read absent, got {other:?}"),
        }
    }

    /// Each JS-runtime candidate's own `CLAUDE_CONFIG_DIR` is read beside the
    /// discovered dirs, so a `node` claude under a home discovery never found
    /// is still seen. A dir already discovered, or reported twice, is listed
    /// once.
    #[test]
    fn each_candidates_own_config_dir_is_read_too() {
        let discovered = vec![PathBuf::from("/home/u/.claude")];
        let env = |pid: u32| match pid {
            10 => Some("/home/u/.claude-work".to_string()),
            11 => Some("/home/u/.claude".to_string()),
            _ => None,
        };
        assert_eq!(
            with_pid_config_dirs(discovered, &[10, 11, 12], env),
            vec![
                PathBuf::from("/home/u/.claude"),
                PathBuf::from("/home/u/.claude-work"),
            ]
        );
    }

    /// `NotFound` is the one genuine absence: an account with no `sessions/`
    /// dir, or no config dir at all, still lets a shell running a `node` that
    /// no record names read `absent` — the one pid an unlistable dir would
    /// leave UNKNOWN.
    #[tokio::test]
    async fn an_absent_sessions_dir_still_reads_a_bare_shell_absent() {
        let root = tempfile::tempdir().unwrap();
        let fresh = root.path().join(".claude-fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        let never = root.path().join(".claude-never-created");
        let pane = fake_process_table::register_table(|r| fake_process_table::ScriptedTable {
            snapshot: snapshot(&[(r, 1, "bash", 10), (NODE, r, "node", 20)]),
            config_dirs: vec![fresh, never],
            proc_start: no_kernel,
        });
        assert_eq!(
            probe_claude_under(Some(pane.root_pid), vec![]).await,
            readable(&[], 0, &[])
        );
    }

    /// A record an EARLIER holder of the pid left behind does not make the
    /// process now holding it a `claude`: by `procStart` against the kernel,
    /// and by `startedAt` against the snapshot where the kernel cannot be read.
    /// The same record does count once the kernel agrees it is this process.
    #[tokio::test]
    async fn a_reused_pids_stale_record_does_not_make_it_live() {
        fn kernel_disagrees(_: u32) -> Option<String> {
            Some("500".to_string())
        }
        fn kernel_agrees(_: u32) -> Option<String> {
            Some("400".to_string())
        }
        let root = tempfile::tempdir().unwrap();
        let stale = config_dir(
            root.path(),
            ".claude-a",
            &[(
                NODE,
                format!(r#"{{"pid":{NODE},"sessionId":"old","status":"idle","procStart":"400"}}"#),
            )],
        );
        let reused = node_pane(vec![stale.clone()], kernel_disagrees);
        assert_eq!(
            probe_claude_under(Some(reused.root_pid), vec![]).await,
            readable(&[], 0, &[]),
            "procStart disagrees with the kernel: an earlier holder's record"
        );
        let same = node_pane(vec![stale], kernel_agrees);
        assert_eq!(
            probe_claude_under(Some(same.root_pid), vec![]).await,
            readable(&[id(NODE, 20)], 0, &[])
        );

        // Written a day before the live process began (it began at 20 s).
        let day_old = config_dir(
            root.path(),
            ".claude-b",
            &[(
                NODE,
                format!(
                    r#"{{"pid":{NODE},"sessionId":"old","status":"idle","startedAt":{}}}"#,
                    20_000 - 86_400_000
                ),
            )],
        );
        let reused = node_pane(vec![day_old], no_kernel);
        assert_eq!(
            probe_claude_under(Some(reused.root_pid), vec![]).await,
            readable(&[], 0, &[]),
            "startedAt a day before the process began: an earlier holder's record"
        );
    }

    /// A record naming a subtree pid that cannot be parsed may be the live
    /// npm session's own, caught mid-write: UNKNOWN, not `absent`.
    #[tokio::test]
    async fn an_unparseable_record_for_a_subtree_pid_reads_unknown() {
        let root = tempfile::tempdir().unwrap();
        let torn = config_dir(
            root.path(),
            ".claude-npm",
            &[(NODE, format!(r#"{{"pid":{NODE},"sess"#))],
        );
        let pane = node_pane(vec![torn], no_kernel);
        match probe_claude_under(Some(pane.root_pid), vec![]).await {
            ClaudeProbe::Unreadable(detail) => {
                assert!(detail.contains(&format!("pid {NODE}")), "{detail}");
            }
            other => panic!("an unparseable record must read unknown, got {other:?}"),
        }
    }

    /// An undecided registry leaves open only a pid whose image could be a
    /// JS-runtime Claude Code. A shell running a native `claude` reads the
    /// same whatever the registry says; a shell running a `node` (or `bun`,
    /// or an image the table could not read) is UNKNOWN — and so is it under
    /// a look that took no registry read at all.
    #[test]
    fn an_undecided_registry_leaves_open_only_a_js_runtime_image() {
        let unreadable = RegistryWitness::unreadable("x");
        let native = snapshot(&[(100, 1, "bash", 10), (200, 100, "claude", 20)]);
        for witness in [unreadable.clone(), RegistryWitness::default()] {
            assert_eq!(
                probe_from_snapshot(100, &native, &[], &witness),
                readable(&[id(200, 20)], 0, &[]),
                "{witness:?}"
            );
        }
        for runtime in ["node", "nodejs", "bun", "node.exe", "/usr/bin/node"] {
            let js = snapshot(&[(100, 1, "bash", 10), (210, 100, runtime, 21)]);
            assert!(
                matches!(
                    probe_from_snapshot(100, &js, &[], &unreadable),
                    ClaudeProbe::Unreadable(_)
                ),
                "{runtime}"
            );
        }
        let mut nameless = snapshot(&[(100, 1, "bash", 10), (210, 100, "node", 21)]);
        nameless.names.remove(&210);
        assert!(matches!(
            probe_from_snapshot(100, &nameless, &[], &unreadable),
            ClaudeProbe::Unreadable(_)
        ));
    }

    /// A registry-named pid takes its place in the probe's root-first,
    /// breadth-first order, so it can be the top-level `claude` whose
    /// children are counted.
    #[test]
    fn a_registry_named_pid_takes_its_place_in_subtree_order() {
        // shell 100 -> node 210 (named) -> claude 220 ; shell 100 -> claude 230
        let snap = snapshot(&[
            (100, 1, "bash", 10),
            (210, 100, "node", 21),
            (230, 100, "claude", 23),
            (220, 210, "claude", 22),
        ]);
        let ClaudeProbe::Readable(view) =
            probe_from_snapshot(100, &snap, &[], &RegistryWitness::naming(&[210]))
        else {
            panic!("readable");
        };
        assert_eq!(pids_of(&view.subtree_claude), vec![210, 230, 220]);
        assert_eq!(view.top_level_children, 1);
    }

    /// A look answers only for the panes its registry read covered: another
    /// pane's `node` processes were never asked about.
    #[test]
    fn a_look_answers_only_for_the_panes_it_read_the_registry_for() {
        let look = PaneLook::from_parts(
            &[100],
            snapshot(&[(100, 1, "bash", 10), (300, 1, "bash", 30)]),
            RegistryWitness::naming(&[]),
        );
        assert_eq!(look.probe(100, &[]), readable(&[], 0, &[]));
        assert!(matches!(look.probe(300, &[]), ClaudeProbe::Unreadable(_)));
        assert!(matches!(
            PaneLook::default().probe(100, &[]),
            ClaudeProbe::Unreadable(_)
        ));
    }

    /// A subtree of `claude` images leaves the registry nothing to decide, so
    /// no read is taken — not even a claim on a read slot.
    #[tokio::test]
    async fn a_subtree_of_claude_images_takes_no_registry_read() {
        static FULL: AtomicUsize = AtomicUsize::new(MAX_REGISTRY_READS_IN_FLIGHT);
        let snap = snapshot(&[(100, 1, "claude", 10)]);
        let witness = read_registry_witness_with(
            &[100],
            &snap,
            |_: &[u32]| -> Vec<PathBuf> { panic!("no registry read is needed") },
            no_kernel,
            REGISTRY_READ_TIMEOUT,
            &FULL,
        )
        .await;
        assert_eq!(witness, RegistryWitness::naming(&[]));
    }

    /// Past the in-flight cap a read is refused, reads UNKNOWN, and takes no
    /// slot.
    #[tokio::test]
    async fn a_registry_read_past_the_in_flight_cap_reads_unknown() {
        static FULL: AtomicUsize = AtomicUsize::new(MAX_REGISTRY_READS_IN_FLIGHT);
        let snap = snapshot(&[(100, 1, "bash", 10)]);
        let witness = read_registry_witness_with(
            &[100],
            &snap,
            |_: &[u32]| Vec::new(),
            no_kernel,
            REGISTRY_READ_TIMEOUT,
            &FULL,
        )
        .await;
        assert!(
            witness
                .incomplete_reason()
                .is_some_and(|why| why.contains("in flight")),
            "{witness:?}"
        );
        assert_eq!(FULL.load(Ordering::SeqCst), MAX_REGISTRY_READS_IN_FLIGHT);
    }

    /// A read that overruns its budget reads UNKNOWN. The abandoned read keeps
    /// its slot until it actually ends, then gives it back.
    #[tokio::test]
    async fn a_registry_read_that_overruns_its_budget_reads_unknown() {
        static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
        let snap = snapshot(&[(100, 1, "bash", 10)]);
        let witness = read_registry_witness_with(
            &[100],
            &snap,
            |_: &[u32]| {
                std::thread::sleep(Duration::from_millis(200));
                Vec::new()
            },
            no_kernel,
            Duration::from_millis(20),
            &IN_FLIGHT,
        )
        .await;
        assert!(
            witness
                .incomplete_reason()
                .is_some_and(|why| why.contains("exceeded")),
            "{witness:?}"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while IN_FLIGHT.load(Ordering::SeqCst) != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the abandoned read never gave its slot back"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
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

    fn recording_close(log: Log) -> impl FnOnce() -> std::future::Ready<CloseTabResult> {
        move || {
            log.lock().unwrap().push("close".to_string());
            std::future::ready(CloseTabResult::Closed)
        }
    }

    /// A closer that finds the pane NOT clear and refuses (hazard 2).
    fn refusing_close(log: Log) -> impl FnOnce() -> std::future::Ready<CloseTabResult> {
        move || {
            log.lock().unwrap().push("close-refused".to_string());
            std::future::ready(CloseTabResult::Refused {
                reason: "a claude reappeared in the pane".to_string(),
            })
        }
    }

    fn entries(log: &Log) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    #[tokio::test(start_paused = true)]
    async fn exits_then_closes_only_after_two_consecutive_gone_probes() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), true);
        let outcome = drive_graceful_exit(
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
            Some(CloseAuthority::for_test()),
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

    /// Hazard 2: a closer that cannot prove the pane clear refuses, and the
    /// outcome says so rather than claiming a clean `Exited`.
    #[tokio::test(start_paused = true)]
    async fn a_close_that_refuses_is_reported_as_close_refused_not_exited() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), true);
        let outcome = drive_graceful_exit(
            pane.write(log.clone()),
            pane.screen(),
            scripted_probe(
                vec![
                    readable(&[CLAUDE], 0, &[]),
                    readable(&[], 0, &[]),
                    readable(&[], 0, &[]),
                ],
                log.clone(),
            ),
            refusing_close(log.clone()),
            timing(),
            Some(CloseAuthority::for_test()),
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::CloseRefused {
                // Two 500 ms polls: the gone streak is reached on the second.
                waited_ms: 1_000,
                claude_pids: vec![42],
                reason: "a claude reappeared in the pane".to_string(),
            }
        );
        assert!(entries(&log).iter().any(|e| e == "close-refused"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_claude_alive_elsewhere_in_the_table_blocks_the_close_until_the_deadline() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), true);
        let outcome = drive_graceful_exit(
            pane.write(log.clone()),
            pane.screen(),
            scripted_probe(
                vec![readable(&[CLAUDE], 0, &[]), readable(&[], 0, &[CLAUDE])],
                log.clone(),
            ),
            recording_close(log.clone()),
            timing(),
            Some(CloseAuthority::for_test()),
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
        let outcome = drive_graceful_exit(
            pane.write(log.clone()),
            pane.screen(),
            scripted_probe(vec![readable(&[CLAUDE], 0, &[CLAUDE])], log.clone()),
            recording_close(log.clone()),
            timing(),
            Some(CloseAuthority::for_test()),
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
        let outcome = drive_graceful_exit(
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
            Some(CloseAuthority::for_test()),
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
            let outcome = drive_graceful_exit(
                pane.write(log.clone()),
                pane.screen(),
                scripted_probe(vec![probe], log.clone()),
                recording_close(log.clone()),
                timing(),
                Some(CloseAuthority::for_test()),
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
        let outcome = drive_graceful_exit(
            pane.write(log.clone()),
            pane.screen(),
            scripted_probe(vec![readable(&[CLAUDE], 0, &[])], log.clone()),
            recording_close(log.clone()),
            timing(),
            Some(CloseAuthority::for_test()),
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
        let outcome = drive_graceful_exit(
            write,
            screen,
            scripted_probe(vec![readable(&[CLAUDE], 0, &[])], log.clone()),
            recording_close(log.clone()),
            timing(),
            Some(CloseAuthority::for_test()),
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

    /// No `claude`, or an unreadable table, answers the same with or WITHOUT
    /// an authority. Neither types anything, so the authority is never asked
    /// for. This is what keeps the relay's close of a bare shell pane
    /// (`NoLiveClaude` → its own `tm.close`) unchanged by R0a.
    #[tokio::test(start_paused = true)]
    async fn no_claude_or_an_unreadable_table_types_nothing() {
        for with_authority in [true, false] {
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
                let outcome = drive_graceful_exit(
                    pane.write(log.clone()),
                    pane.screen(),
                    scripted_probe(vec![probe], log.clone()),
                    recording_close(log.clone()),
                    timing(),
                    with_authority.then(CloseAuthority::for_test),
                )
                .await;
                assert_eq!(outcome, expected, "with_authority={with_authority}");
                assert_eq!(entries(&log).len(), 1, "with_authority={with_authority}");
            }
        }
    }

    /// R0a: a live `claude` and no authority is refused `NoCloseAuthority`
    /// after ONE probe, before any byte reaches the pane and before any close.
    ///
    /// The first case is the one that matters most: one `claude`, ZERO
    /// children, at an empty prompt that echoes. With an authority, that pane
    /// is typed into (`exits_then_closes_only_after_two_consecutive_gone_probes`).
    /// The other cases pin that the refusal does not wait on any later
    /// predicate: children, a nested `claude`, a draft or a dialog each answer
    /// `NoCloseAuthority` too, not their own refusal.
    #[tokio::test(start_paused = true)]
    async fn a_live_claude_without_an_authority_is_refused_before_any_byte() {
        let cases: Vec<(ClaudeProbe, ScreenText, Vec<u32>)> = vec![
            (readable(&[CLAUDE], 0, &[]), empty_prompt(), vec![42]),
            (readable(&[CLAUDE], 3, &[]), empty_prompt(), vec![42]),
            (
                readable(&[CLAUDE, id(43, 1_001)], 0, &[]),
                empty_prompt(),
                vec![42, 43],
            ),
            (readable(&[CLAUDE], 0, &[]), draft_prompt(), vec![42]),
            (readable(&[CLAUDE], 0, &[]), permission_dialog(), vec![42]),
        ];
        for (probe, initial, pids) in cases {
            let log: Log = Arc::default();
            let pane = FakePane::new(initial, true);
            let outcome = drive_graceful_exit(
                pane.write(log.clone()),
                pane.screen(),
                scripted_probe(vec![probe], log.clone()),
                recording_close(log.clone()),
                timing(),
                None,
            )
            .await;
            assert_eq!(
                outcome,
                GracefulExitOutcome::NoCloseAuthority {
                    claude_pids: pids.clone()
                },
                "pids {pids:?}"
            );
            assert!(
                pane.typed.lock().unwrap().is_empty(),
                "{pids:?}: a refusal without an authority typed into a live claude"
            );
            assert_eq!(
                entries(&log),
                vec!["probe(tracked=0)"],
                "{pids:?}: exactly one probe, no write, no close"
            );
        }
    }

    #[test]
    fn the_authority_refusal_has_its_own_wire_word() {
        let refusal = GracefulExitOutcome::NoCloseAuthority {
            claude_pids: vec![42],
        };
        assert_eq!(
            serde_json::to_value(&refusal).unwrap(),
            serde_json::json!({ "outcome": "no_close_authority", "claude_pids": [42] })
        );
        assert_eq!(refusal.word(), "no_close_authority");
    }

    /// `word()` is the serialized `outcome` tag for every variant, so a log
    /// line and a JSON reply never name one outcome two ways.
    #[test]
    fn every_outcome_word_is_its_wire_tag() {
        let all = [
            GracefulExitOutcome::Exited {
                waited_ms: 1,
                claude_pids: vec![1],
            },
            GracefulExitOutcome::ExitStuck {
                waited_ms: 1,
                claude_pids: vec![1],
                last_probe_unreadable: false,
            },
            GracefulExitOutcome::Refused {
                reason: "r".into(),
                claude_pids: vec![1],
            },
            GracefulExitOutcome::NoLiveClaude,
            GracefulExitOutcome::NoCloseAuthority {
                claude_pids: vec![1],
            },
            GracefulExitOutcome::ProbeUnavailable { detail: "d".into() },
            GracefulExitOutcome::WriteFailed {
                error: "e".into(),
                claude_pids: vec![1],
            },
            GracefulExitOutcome::CloseRefused {
                waited_ms: 1,
                claude_pids: vec![1],
                reason: "r".into(),
            },
            GracefulExitOutcome::CloseOutcomeUnknown {
                waited_ms: 1,
                claude_pids: vec![1],
                detail: "d".into(),
            },
        ];
        for outcome in all {
            let json = serde_json::to_value(&outcome).unwrap();
            assert_eq!(json["outcome"], outcome.word(), "{outcome:?}");
        }
    }

    /// The scripted table answers only for the pid it registered, and forgets
    /// the pane when its handle drops, so it cannot leak into a real probe.
    #[tokio::test]
    async fn a_scripted_pane_answers_only_while_registered() {
        let pane = fake_process_table::register(PaneProcesses {
            subtree_claude: vec![CLAUDE],
            top_level_children: 0,
            tracked_alive: Vec::new(),
        });
        let root = pane.root_pid;
        assert_eq!(
            probe_claude_under(Some(root), vec![CLAUDE, id(7, 7)]).await,
            readable(&[CLAUDE], 0, &[CLAUDE])
        );
        drop(pane);
        assert!(fake_process_table::probe(root, &[]).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_write_closes_nothing() {
        let log: Log = Arc::default();
        let pane = FakePane::new(empty_prompt(), true);
        let outcome = drive_graceful_exit(
            |_bytes: &[u8]| Err("terminal exited".to_string()),
            pane.screen(),
            scripted_probe(vec![readable(&[CLAUDE], 0, &[])], log.clone()),
            recording_close(log.clone()),
            timing(),
            Some(CloseAuthority::for_test()),
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
    /// path. `drive_graceful_exit` reaches the pane only through its injected effects, and
    /// the production close is `TerminalManager::graceful_exit`'s.
    #[test]
    fn this_module_contains_no_kill_path() {
        let source = include_str!("graceful_exit.rs");
        // Split at the test MODULE, not at the first `#[cfg(test)]`: the
        // scripted-probe seam inside `probe_claude_under` is an earlier
        // `#[cfg(test)]`, and splitting there would leave `drive_graceful_exit` unscanned.
        let production: String = source
            .split("#[cfg(test)]\nmod tests {")
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
