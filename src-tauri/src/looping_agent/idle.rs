//! Rendered-grid idle + context-low predicates for looping agents.
//!
//! Both predicates run over the terminal's **rendered VT cell grid** (rows
//! joined as lines + cursor row), NOT the raw byte stream — the fleet's
//! proven approach (`terminal/auto_response.rs`, `terminal/usage_limit.rs`):
//! Claude Code paints each whole frame as one synchronized-output update, so
//! a rolling byte window only ever retains the bottom of a frame and misses
//! mid-screen text. The grid always reflects what is actually on screen.
//!
//! [`snapshot_looks_idle`] is the ONE turn-complete recognizer in the runner,
//! and `TerminalSession::idle_quiescence_probe` is the ONE debounce around it.
//! Its consumers, all of them: the looping-agent supervisor's tick and the
//! session message poller's PTY injection gate, both through that probe
//! (directly, or through the `TerminalSession::looks_idle_quiescent` bool gate
//! over it); the wind-down grid observation through `observe_grid_idle`, whose
//! generation check stands in for a debounce without sleeping; and two SINGLE
//! reads with no debounce at all, deliberately — `terminal::graceful_exit`'s
//! `prompt_ready_with`, which must judge the screen it is about to type into
//! at that instant, and `looping_agent_supervisor::status_snapshot`, which
//! reports rather than acts. A byte-identical copy of the PREDICATE used to
//! live in `mcp/session_message_poller.rs` with an "extend BOTH sites"
//! comment; it was deleted by plan
//! `2026-09-13-drained-runner-never-reaches-idle` Phase 1, and the last copy
//! of the DEBOUNCE around it — in the looping-agent supervisor, which needed
//! the settled screen the old `bool` gate threw away — went in that plan's
//! post-merge follow-up. So there is now exactly one indicator list to extend
//! and one place the two-read rule lives.
//!
//! [`snapshot_shows_dialog`] is a separate conjunct, not part of the idle
//! predicate: a Claude Code dialog (workspace trust, a permission prompt)
//! reads idle to it, so a caller about to TYPE into the session checks both.
//! The looping-agent supervisor does; `terminal::graceful_exit`'s prompt-ready
//! checks scan the same [`DIALOG_CHROME_MARKERS`] within the same
//! [`DIALOG_SCAN_ROWS`], anchored on the cursor row it has and this does not.

use std::sync::LazyLock;

use regex::Regex;

/// Working/processing indicators that mean Claude is mid-turn. If ANY appears
/// on the rendered screen the terminal is NOT idle. Lowercased before match.
/// Sourced from the Claude Code TUI working line ("… esc to interrupt") and
/// its transient status lines; conservative — any hit vetoes idle. An
/// indicator found only inside Claude Code's idle `/clear` hint
/// ([`CLEAR_HINT`]) does not count.
pub const PROCESSING_INDICATORS: &[&str] = &[
    "esc to interrupt",
    "to interrupt)",
    "interrupt)",
    "tokens ·",
    "thinking…",
    "thinking...",
    "compacting",
    "summarizing",
];

/// Claude Code's IDLE footer hint, `new task? /clear to save 320.4k tokens ·
/// …`, matched lowercased. It carries the `"tokens ·"` working indicator's
/// text, so a finished pane at an empty prompt read as busy for as long as
/// the hint was painted (plan
/// `2026-11-04-runner-finished-close-closes-nothing-on-any-box` cause 4:
/// 5 of 48 finished sessions held at `grid_busy` on one box; runner#2369:
/// 42 of 43 on another, together with the turn-done line). Only the hint
/// itself is masked, never the rest of its line.
static CLEAR_HINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"/clear to save [0-9][0-9.,]*[kmb]? tokens").expect("CLEAR_HINT is a valid regex")
});

/// Does `ind` occur in the lowercased row `lower` OUTSIDE the idle `/clear`
/// hint? `false` both when it does not occur at all and when every
/// occurrence lies inside the hint.
fn indicator_outside_clear_hint(lower: &str, ind: &str) -> bool {
    if !lower.contains("/clear") {
        return lower.contains(ind);
    }
    CLEAR_HINT.replace_all(lower, "").contains(ind)
}

/// Spinner glyphs the Claude Code TUI animates while working. Their presence
/// on screen is a strong "busy" signal independent of the text indicators
/// above. The `·` middot is common in static UI, so it is deliberately NOT
/// treated as a bare busy glyph (it is covered by the `"tokens ·"` text
/// indicator instead).
pub const SPINNER_GLYPHS: &[char] = &['✻', '✶', '✳', '✽', '✢', '·', '∗'];

/// The Claude Code input-prompt marker. When Claude is ready for input the
/// input box shows this caret.
pub const PROMPT_MARKER: char = '❯';

/// Decide whether a single rendered grid snapshot looks IDLE / ready for
/// input. Pure over the snapshot. CONSERVATIVE: returns `false` (NOT idle)
/// on any ambiguity. Idle requires ALL of:
///
/// 1. NO processing-indicator text anywhere on screen
///    ([`PROCESSING_INDICATORS`]).
/// 2. NO spinner glyph on screen ([`SPINNER_GLYPHS`], `·` excepted), save on
///    ONE line: the static completed-turn line ([`is_completed_turn_line`])
///    of the turn that just ended — the LAST glyph-bearing line on screen,
///    sitting directly above the input row with only what Claude Code puts
///    there in between ([`is_turn_to_input_gap`]).
/// 3. A prompt row containing [`PROMPT_MARKER`] (`❯`) is visible; the
///    bottom-most such row is the input box.
/// 4. The cursor sits AT OR BELOW the prompt row (in the input area), not up
///    in streaming output.
///
/// There is NO draft or dialog check here. A caller that TYPES must also pass
/// `terminal::graceful_exit`'s `exit_prompt_ready` / `input_prompt_ready` on
/// the screen it is about to type into.
pub fn snapshot_looks_idle(lines: &[String], cursor_row: u16) -> bool {
    idle_verdict(lines, cursor_row).is_none()
}

/// Which of [`snapshot_looks_idle`]'s checks vetoes idle on this snapshot, as
/// a short human-readable reason; `None` exactly when the snapshot looks idle.
/// The same checks in the same order — the diagnostic for a pane that keeps
/// reading busy (restart-prep's told-but-busy log line, #2569, reads it). It
/// is [`IdleVeto::describe`] over [`idle_verdict`], so the two cannot drift.
pub fn idle_veto(lines: &[String], cursor_row: u16) -> Option<String> {
    idle_verdict(lines, cursor_row).map(|veto| veto.describe(lines))
}

/// Which check of [`snapshot_looks_idle`] vetoed idle — the ONE typed veto.
/// [`idle_veto`] renders it for a log line; [`IdleVeto::wire`] renders it,
/// as constants only, for `GET /restart-readiness`'s `windDown.idleEvidence`
/// (plan `2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-for-a-spinner`,
/// D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleVeto {
    /// (1) A [`PROCESSING_INDICATORS`] constant on `row`.
    Indicator { indicator: &'static str, row: usize },
    /// (2) A spinner glyph on `row`, outside the exempt completed-turn line.
    Glyph { glyph: char, row: usize },
    /// (3) No `❯` prompt row on screen.
    NoPrompt,
    /// (4) The cursor above the input box's `❯` row.
    CursorAbovePrompt { cursor_row: u16, prompt_row: usize },
}

impl IdleVeto {
    /// The wire value: `indicator:<constant>`, `glyph:<char>`, `no_prompt` or
    /// `cursor_above_prompt`. Constants only — never screen text, because
    /// panes carry session content and this reaches an HTTP response.
    pub fn wire(self) -> String {
        match self {
            Self::Indicator { indicator, .. } => format!("indicator:{indicator}"),
            Self::Glyph { glyph, .. } => format!("glyph:{glyph}"),
            Self::NoPrompt => "no_prompt".to_string(),
            Self::CursorAbovePrompt { .. } => "cursor_above_prompt".to_string(),
        }
    }

    /// The human-readable reason, naming the row (and, for a glyph, quoting
    /// the line) — for a local log line, never a wire field.
    pub fn describe(self, lines: &[String]) -> String {
        match self {
            Self::Indicator { indicator, row } => {
                format!("processing indicator `{indicator}` on row {row}")
            }
            Self::Glyph { glyph, row } => format!(
                "spinner glyph `{glyph}` on row {row}: {}",
                lines.get(row).map_or("", |l| l.trim())
            ),
            Self::NoPrompt => "no `❯` prompt row on screen".to_string(),
            Self::CursorAbovePrompt {
                cursor_row,
                prompt_row,
            } => format!("cursor on row {cursor_row}, above the `❯` prompt row {prompt_row}"),
        }
    }
}

/// The typed core of [`snapshot_looks_idle`]: `None` when the snapshot looks
/// idle, else the FIRST check that vetoed it, in the order listed there.
pub fn idle_verdict(lines: &[String], cursor_row: u16) -> Option<IdleVeto> {
    // (1) any processing-indicator text ⇒ busy.
    for (row, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        if let Some(indicator) = PROCESSING_INDICATORS
            .iter()
            .find(|ind| indicator_outside_clear_hint(&lower, ind))
        {
            return Some(IdleVeto::Indicator { indicator, row });
        }
    }

    // The BOTTOM-most `❯` row is the input box (as `snapshot_shows_dialog`
    // reads it): an earlier one is transcript — a `❯ <your message>` echo in
    // scrollback — and taking it would make (4) pass for a cursor up in
    // streaming output.
    let prompt_row = lines.iter().rposition(|l| l.contains(PROMPT_MARKER));

    // (2) spinner glyphs (excluding bare `·`) ⇒ busy — except on the static
    // completed-turn line of the turn that just ended: the LAST glyph line,
    // whose first `❯` row below is the input box, with only the rows
    // `is_turn_to_input_gap` admits in between. A completed-turn line
    // anywhere else (an earlier turn's, or one with output or a live spinner
    // under it) keeps its veto: fail closed.
    let glyph_rows: Vec<(usize, char)> = lines
        .iter()
        .enumerate()
        .filter_map(|(row, line)| {
            line.chars()
                .find(|ch| *ch != '·' && SPINNER_GLYPHS.contains(ch))
                .map(|glyph| (row, glyph))
        })
        .collect();
    let exempt = |row: usize| {
        glyph_rows.last().is_some_and(|&(last, _)| last == row)
            && is_completed_turn_line(&lines[row])
            && lines
                .get(row + 1..)
                .and_then(|below| below.iter().position(|l| l.contains(PROMPT_MARKER)))
                .is_some_and(|gap| {
                    Some(row + 1 + gap) == prompt_row
                        && is_turn_to_input_gap(&lines[row + 1..row + 1 + gap])
                })
    };
    if let Some(&(row, glyph)) = glyph_rows.iter().find(|(row, _)| !exempt(*row)) {
        return Some(IdleVeto::Glyph { glyph, row });
    }

    // (3) prompt marker visible — its row is (4)'s anchor.
    let Some(prompt_row) = prompt_row else {
        // No visible input prompt ⇒ can't confirm ready-for-input ⇒ not idle.
        return Some(IdleVeto::NoPrompt);
    };

    // (4) cursor in the input area (at/below the prompt row).
    ((cursor_row as usize) < prompt_row).then_some(IdleVeto::CursorAbovePrompt {
        cursor_row,
        prompt_row,
    })
}

/// Is `line` the static line Claude Code leaves on screen once a turn has
/// FINISHED — `✻ Cogitated for 9m 42s · done 6:36 AM`,
/// `✻ Churned for 1m 31s · done 7:49 AM` (2.1.310; shape
/// `<glyph> <Verb>ed for <duration>[ · done <time>]`)?
///
/// It carries the same glyph as the live spinner, so a glyph-only veto read
/// every finished session as busy for as long as the line stayed painted —
/// i.e. forever, until the next turn. The live spinner line differs in shape,
/// never only in glyph: a present-tense verb with an ellipsis and an elapsed
/// counter in parentheses (`✻ Cogitating… (12s · ↑ 1.2k tokens · esc to
/// interrupt)`). So this accepts ONLY the exact completed shape:
///
/// * a spinner glyph, then ONE alphabetic word (the verb — not checked for an
///   `-ed` ending, because Claude Code's verb list includes irregular forms),
/// * then `for` and a duration made solely of `<digits><unit>` tokens
///   (`h`/`m`/`s`, e.g. `9m 42s`, `45s`, `1h 2m 3s`),
/// * then nothing, or `· done` followed by the clock time.
///
/// Any ellipsis, parenthesis or other text anywhere on the line rejects it, so
/// a live spinner can never match. Conservative: a line that is not exactly
/// this shape keeps vetoing idle as before.
pub fn is_completed_turn_line(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('│').trim();
    if trimmed.contains('…') || trimmed.contains("...") || trimmed.contains('(') {
        return false;
    }
    let mut chars = trimmed.chars();
    let Some(glyph) = chars.next() else {
        return false;
    };
    if glyph == '·' || !SPINNER_GLYPHS.contains(&glyph) {
        return false;
    }
    let rest = chars.as_str();
    // Split off an optional `· done <time>` tail.
    let (head, tail) = match rest.split_once('·') {
        Some((h, t)) => (h, Some(t)),
        None => (rest, None),
    };
    if let Some(tail) = tail {
        let mut words = tail.split_whitespace();
        if words.next() != Some("done") {
            return false;
        }
        // The clock time: `6:36 AM`, `18:02`. Digits, colons and an AM/PM.
        // A turn that ended on an earlier day dates it first, in the locale's
        // words: `done Tuesday 9:26 PM`, `done Wednesday, Oct 28, 9:26 PM`,
        // `done Mittwoch, 21:26` (shapes from runner#2369) — so before the
        // clock, and only before it, a run of letters or of digits, each
        // optionally ending in `,`, is the date.
        let mut saw_time = false;
        for w in words {
            let is_clock = w.contains(':') && w.chars().all(|c| c.is_ascii_digit() || c == ':');
            let is_meridiem = matches!(w.to_ascii_uppercase().as_str(), "AM" | "PM");
            let date_word = w.strip_suffix(',').unwrap_or(w);
            let is_date = !saw_time
                && !date_word.is_empty()
                && (date_word.chars().all(char::is_alphabetic)
                    || date_word.chars().all(|c| c.is_ascii_digit()));
            if is_clock {
                saw_time = true;
            } else if !is_meridiem && !is_date {
                return false;
            }
        }
        if !saw_time {
            return false;
        }
    }
    // `<Verb>ed for <duration>`, tolerating a missing space after the glyph,
    // between the verb and `for`, and between `for` and the duration — a
    // grid row can lose a blank column (an orphaned wide-glyph half under a
    // cursor-forward move read `✻Brewed for2m 39s`). Every other check stays.
    let head = head.trim_start();
    let verb_end = head
        .char_indices()
        .find(|(_, c)| !c.is_alphabetic())
        .map_or(head.len(), |(i, _)| i);
    let (verb, after) = head.split_at(verb_end);
    let after = after.trim_start();
    let duration = if let Some(d) = after.strip_prefix("for") {
        if verb.is_empty() {
            return false;
        }
        d
    } else if after.starts_with(|c: char| c.is_ascii_digit()) {
        // `Brewedfor2m`: the verb run swallowed `for`.
        match verb.strip_suffix("for") {
            Some(v) if !v.is_empty() => after,
            _ => return false,
        }
    } else {
        return false;
    };
    // The duration must follow `for` directly or after blanks, never another
    // letter (`format`, `fortune`).
    if !duration.starts_with(|c: char| c.is_ascii_digit() || c.is_whitespace()) {
        return false;
    }
    let mut saw_duration = false;
    for w in duration.split_whitespace() {
        if !is_duration_token(w) {
            return false;
        }
        saw_duration = true;
    }
    saw_duration
}

/// `9m`, `42s`, `1h`, `1d`, or several glued together (`2m39s`).
fn is_duration_token(w: &str) -> bool {
    let mut saw_unit = false;
    let mut digits = 0usize;
    for c in w.chars() {
        if c.is_ascii_digit() {
            digits += 1;
        } else if matches!(c, 'd' | 'h' | 'm' | 's') && digits > 0 {
            digits = 0;
            saw_unit = true;
        } else {
            return false;
        }
    }
    saw_unit && digits == 0
}

/// Do `rows` — everything between a completed-turn line and the input row
/// below it — hold only what Claude Code puts there after a turn ends?
/// Fail closed: any other row, or a recap block that does not close, means
/// the glyph stays busy.
///
/// Measured over the census's 44 finished panes (Claude Code 2.1.309, plan
/// `2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-for-a-spinner`
/// census): blank rows; the `/clear to save …` context hint (often with an
/// update notice); the input box's top edge, which carries the session's
/// name (`──── my-session ─`); and, on some panes, a `※ recap: …`
/// paragraph between the turn line and the hint, usually closed by a
/// `(… /config)` hint (one census pane's was not). With this rule 40 of the
/// 44 read idle; the other 4 fail on the completed-turn line's own wording. Inside an open recap its
/// prose rows are admitted, but never a row that starts a transcript item;
/// after a closed recap, only blank, hint and edge rows. Any other row means
/// the turn line is not the one that just ended, and the glyph stays busy.
fn is_turn_to_input_gap(rows: &[String]) -> bool {
    #[derive(PartialEq)]
    enum Recap {
        None,
        Open,
        Closed,
    }
    let mut recap = Recap::None;
    rows.iter().all(|row| {
        let trimmed = row.trim();
        if trimmed.is_empty() || is_box_edge_row(trimmed) || is_context_hint_row(trimmed) {
            return true;
        }
        match recap {
            // Before any recap only a recap may start; after a CLOSED one,
            // nothing but blank, hint and edge rows.
            Recap::None if trimmed.starts_with('※') => {
                recap = if closes_recap(trimmed) {
                    Recap::Closed
                } else {
                    Recap::Open
                };
                true
            }
            Recap::None | Recap::Closed => false,
            // Inside an open recap its prose is admitted — but never a row
            // that opens a transcript item, which is what text that is not
            // recap prose looks like on this screen.
            Recap::Open => {
                if starts_transcript_item(trimmed) {
                    return false;
                }
                if closes_recap(trimmed) {
                    recap = Recap::Closed;
                }
                true
            }
        }
    })
}

/// The leading glyphs of a transcript item: the assistant bullet in both its
/// renderings (`●`, and `⏺` — see `terminal/transport_cost.rs`), the
/// tool-result elbow, an echoed prompt, and the todo-list checkboxes.
const TRANSCRIPT_ITEM_GLYPHS: &[char] = &['●', '⏺', '⎿', PROMPT_MARKER, '☐', '☒', '✔', '✓'];

/// A row that begins a transcript item rather than continuing prose: one of
/// [`TRANSCRIPT_ITEM_GLYPHS`], or a spinner.
fn starts_transcript_item(trimmed: &str) -> bool {
    trimmed.starts_with(TRANSCRIPT_ITEM_GLYPHS)
        || trimmed
            .chars()
            .next()
            .is_some_and(|c| c != '·' && SPINNER_GLYPHS.contains(&c))
}

/// The last row of a `※ recap` block: it ends with the parenthesised
/// `/command` hint the block closes with — `… (disable recaps in /config)` —
/// which on a narrow pane can wrap so that only its tail, `… /config)`, sits
/// on this row. Matched by that shape (a `/word` closing the parenthesis), as
/// the census measured it, not by the words inside.
fn closes_recap(trimmed: &str) -> bool {
    let Some(body) = trimmed.strip_suffix(')') else {
        return false;
    };
    body.rsplit_once('/')
        .is_some_and(|(_, word)| !word.is_empty() && word.chars().all(char::is_alphanumeric))
}

/// The context hint row. Recognised by its START — `new task?` and a
/// `/clear` — not by the full `/clear to save` text: on a narrow pane the row
/// is cut short with `…` (12 of the census's 44 finished panes), so the full phrase is
/// not there to match.
fn is_context_hint_row(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("new task?") && lower.contains("/clear")
}

/// The input box's border row, titled or not: it starts with a box-drawing
/// corner or rule.
fn is_box_edge_row(trimmed: &str) -> bool {
    trimmed.starts_with(['─', '━', '╭', '╰'])
}

/// A fingerprint of what the screen shows, for the wind-down grace clock's
/// per-process floor (`wind_down::ScreenQuietTracker`): every row's text and
/// the cursor position, EXCEPT the context-hint row, whose update notice
/// (`Auto-updating…` → `✔ Update installed`) changes on a pane nobody is
/// using. Content, not the grid generation, so a status line redrawn
/// unchanged leaves it alone. Only ever compared within one process.
pub fn screen_fingerprint(lines: &[String], cursor_row: u16, cursor_col: u16) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (row, line) in lines.iter().enumerate() {
        if !is_context_hint_row(line.trim()) {
            (row, line).hash(&mut hasher);
        }
    }
    (cursor_row, cursor_col).hash(&mut hasher);
    hasher.finish()
}

/// Key hints Claude Code prints beneath a dialog's choices — its own CHROME,
/// not prose, so they do not turn up in an ordinary finished turn. Shared with
/// `terminal::graceful_exit`'s prompt-ready checks, which scan for the same
/// hints. Lowercased before match.
///
/// The last five are the other dialogs' key hints (a select list, a detail
/// view, a menu that closes or exits, a nested step), ported from runner#2369.
pub const DIALOG_CHROME_MARKERS: &[&str] = &[
    "esc to cancel",
    "enter to confirm",
    "enter to select",
    "enter to view",
    "esc to close",
    "esc to exit",
    "esc to go back",
];

/// Literal text of Claude Code's workspace-trust dialog (both the current
/// "Quick safety check" wording and the older "Do you trust the files" one).
/// Lowercased before match.
pub const TRUST_DIALOG_MARKERS: &[&str] = &[
    "yes, i trust this folder",
    "quick safety check",
    "do you trust the files in this folder",
];

/// How many rows above the prompt/cursor a dialog-text scan reads: a dialog
/// prints its question and hints around its choices, while an idle turn's
/// transcript sits further up. The ONE value, shared with
/// `terminal::graceful_exit`'s prompt-ready scan.
pub const DIALOG_SCAN_ROWS: usize = 8;

/// Does the rendered screen show a Claude Code DIALOG — the workspace-trust
/// dialog, a tool-permission prompt, or any other select-one-of-N screen —
/// rather than the empty input prompt? Returns the marker that decided it.
///
/// [`snapshot_looks_idle`] cannot tell them apart: a dialog paints no
/// working indicator, and its selection pointer is the same `❯` glyph as the
/// input caret, so the trust dialog reads as an idle prompt. That is the
/// defect in coord finding `9089767f`: the looping-agent supervisor judged the
/// trust dialog idle and typed its cycle nudge into it; the nudge's
/// keystrokes answered the dialog with "No, exit" and `claude` exited.
/// Anything that is about to TYPE into a session (and press Enter) must check
/// this as well as idleness.
///
/// Both signals are anchored on the LAST `❯` row — the live input caret or
/// the dialog's selection pointer, whichever is painted at the bottom — so
/// transcript text higher up (a quoted dialog, an echoed `❯ 1. …` line) is
/// not mistaken for a live dialog:
/// 1. That row's text is a numbered choice (`❯ 1. Yes, I trust this folder`,
///    `❯ 1. Yes`) — the selected option of a dialog.
/// 2. Dialog chrome ([`DIALOG_CHROME_MARKERS`]) or trust-dialog text
///    ([`TRUST_DIALOG_MARKERS`]) at or below a point [`DIALOG_SCAN_ROWS`]
///    rows above that row (the whole screen when there is no `❯`).
pub fn snapshot_shows_dialog(lines: &[String]) -> Option<&'static str> {
    let last_prompt = lines.iter().rposition(|l| l.contains(PROMPT_MARKER));
    if last_prompt
        .and_then(|r| lines.get(r))
        .is_some_and(|l| prompt_row_is_numbered_choice(l))
    {
        return Some("selected numbered choice on a `❯` row");
    }
    let first = last_prompt
        .map(|r| r.saturating_sub(DIALOG_SCAN_ROWS))
        .unwrap_or(0);
    for line in lines.iter().skip(first) {
        let lower = line.to_lowercase();
        if let Some(m) = DIALOG_CHROME_MARKERS
            .iter()
            .chain(TRUST_DIALOG_MARKERS)
            .find(|m| lower.contains(*m))
        {
            return Some(m);
        }
    }
    None
}

/// `❯ 1. …`: the `❯` marker followed (after whitespace) by digits and `. `.
fn prompt_row_is_numbered_choice(line: &str) -> bool {
    let Some((_, after)) = line.split_once(PROMPT_MARKER) else {
        return false;
    };
    let body = after.trim_start_matches(|c: char| c.is_whitespace());
    let digits = body.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0
        && body
            .get(digits..)
            .is_some_and(|rest| rest.starts_with(". "))
}

/// Context-low grid markers. The percentage form
/// (`"Context left until auto-compact: N%"`) is threshold-gated via
/// [`snapshot_context_low`]; the bare forms trigger unconditionally.
const CONTEXT_LOW_BARE_MARKERS: &[&str] = &["context low"];
/// Prefix of the Claude Code auto-compact countdown status line (lowercased).
const AUTO_COMPACT_MARKER: &str = "until auto-compact:";

/// Decide whether the rendered screen shows a context-low signal that
/// warrants a fresh relaunch (kill + fresh `--session-id` + re-read journal).
///
/// Triggers when either:
/// - a bare `"context low"` marker is visible, OR
/// - the `"… until auto-compact: N%"` countdown is visible AND `N <=
///   threshold_pct`.
///
/// An auto-compact line whose percentage does not parse is IGNORED (fail
/// closed on relaunch — the every-K-cycles backstop in the lifecycle policy
/// still bounds context growth, so a missed marker only delays the relaunch,
/// it never wedges the loop).
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
pub fn snapshot_context_low(lines: &[String], threshold_pct: u32) -> bool {
    for line in lines {
        let lower = line.to_ascii_lowercase();
        for m in CONTEXT_LOW_BARE_MARKERS {
            if lower.contains(m) {
                return true;
            }
        }
        if let Some(idx) = lower.find(AUTO_COMPACT_MARKER) {
            if let Some(pct) = parse_leading_percentage(&lower[idx + AUTO_COMPACT_MARKER.len()..]) {
                if pct <= threshold_pct {
                    return true;
                }
            }
        }
    }
    false
}

/// Parse the first `N%` in `s` (skipping leading whitespace/noise up to the
/// first digit run). `None` when no `digits + '%'` shape is present.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn parse_leading_percentage(s: &str) -> Option<u32> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let rest = &s[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if !rest[end..].starts_with('%') {
        return None;
    }
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|s| s.to_string()).collect()
    }

    // ── snapshot_looks_idle ──────────────────────────────────────────────

    #[test]
    fn idle_requires_prompt_marker() {
        // Clean screen but no ❯ prompt: cannot confirm ready-for-input.
        assert!(!snapshot_looks_idle(
            &lines(&["some output", "more output"]),
            1
        ));
        // Prompt visible, cursor on the prompt row: idle.
        assert!(snapshot_looks_idle(&lines(&["cycle done.", "❯ "]), 1));
    }

    #[test]
    fn processing_indicators_veto_idle() {
        for busy in [
            "✳ Reticulating splines… (esc to interrupt)",
            "  12.3k tokens · esc to interrupt",
            "Thinking…",
            "Compacting conversation",
            "summarizing prior messages",
        ] {
            let grid = lines(&[busy, "❯ "]);
            assert!(
                !snapshot_looks_idle(&grid, 1),
                "must be busy with line: {busy}"
            );
        }
    }

    #[test]
    fn spinner_glyphs_veto_idle_except_bare_middot() {
        assert!(!snapshot_looks_idle(&lines(&["✻ working", "❯ "]), 1));
        assert!(!snapshot_looks_idle(&lines(&["✶", "❯ "]), 1));
        // A bare middot in static UI must NOT veto (it is everywhere).
        assert!(snapshot_looks_idle(&lines(&["a · b", "❯ "]), 1));
    }

    /// The 2.1.310 finished-turn screen: the completed-turn line stays painted
    /// above the input box after every finished turn, with the same `✻` glyph
    /// the live spinner uses. It must read idle, or no session ever pauses.
    #[test]
    fn completed_turn_line_does_not_veto_idle() {
        for done in [
            "✻ Cogitated for 9m 42s · done 6:36 AM",
            "✻ Churned for 1m 31s · done 7:49 AM",
            "✻ Worked for 45s · done 18:02",
            "✻ Baked for 1h 2m 3s",
            "  ✻ Sautéed for 12s · done 9:01 pm",
            // A turn that ended on an earlier day (runner#2369's shapes).
            "✻ Brewed for 1d 2h 3m",
            "✻ Cooked for 9s · done Tuesday 9:26 PM",
            "✻ Cooked for 9s · done Wednesday, Oct 28, 9:26 PM",
            "✻ Cooked for 9s · done Mittwoch, 21:26",
        ] {
            let grid = lines(&[
                "● Done — the change is committed.",
                "",
                done,
                "",
                "────────────────────────────────────────",
                "❯\u{a0}",
                "────────────────────────────────────────",
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            ]);
            assert!(is_completed_turn_line(done), "completed shape: {done:?}");
            assert!(snapshot_looks_idle(&grid, 5), "must be idle with: {done:?}");
        }
    }

    /// E2E: a hidden pane's grid lost blank columns (an orphaned wide-glyph
    /// half under a cursor-forward move). The completed-turn line still reads
    /// idle with those spaces missing; a live spinner missing them still reads
    /// busy.
    #[test]
    fn completed_turn_line_tolerates_missing_spaces() {
        for done in [
            "✻Brewed for2m 39s · done 11:46 AM",
            "✻ Brewedfor 2m 39s · done 11:46 AM",
            "✻Brewedfor2m39s · done 11:46 AM",
            "✻ Brewed for 2m 39s · done 11:46 AM",
        ] {
            assert!(is_completed_turn_line(done), "completed: {done:?}");
            assert!(snapshot_looks_idle(&lines(&[done, "❯ "]), 1), "{done:?}");
        }
        for busy in [
            "✻Brewing…(2m 39s · esc to interrupt)",
            "✻Brewing… (2m 39s)",
            "✻Brewed format",
            "✻ for2m 39s",
            "✻Brewed for2mx",
            "✻Brewed fortune 2m",
        ] {
            assert!(!is_completed_turn_line(busy), "not completed: {busy:?}");
            assert!(!snapshot_looks_idle(&lines(&[busy, "❯ "]), 1), "{busy:?}");
        }
    }

    /// The live spinner keeps vetoing idle — with or without the
    /// `esc to interrupt` text — and so does anything that merely resembles
    /// the completed line.
    #[test]
    fn live_spinner_shapes_still_veto_idle() {
        for busy in [
            "✻ Cogitating… (12s · ↑ 1.2k tokens · esc to interrupt)",
            "✻ Cogitating… (2m 3s · ↓ 4.5k tokens)",
            "✶ Pondering…",
            "✢ Reticulating... (5s)",
            "✻ Cogitated for 9m 42s · done 6:36 AM (esc to interrupt)",
            "✻ Cogitated for a while",
            "✻ Cogitated for 9m 42s · still going",
            "✻ Cogitated for",
            "✻ working",
            "✻ Thinking for 3s…",
            // A finished turn that left background work running is NOT idle
            // (main's `is_completed_turn_summary` read both of these as done).
            "✻ Worked for 1m 2s · 1 shell still running",
            "✻ Cogitated for 9m 42s · done 6:36 AM · 1 shell still running",
            "✻ Waiting for 1 background agent to finish",
            // Date words are read only BEFORE the clock, and a clock is still
            // required.
            "✻ Cooked for 9s · done 9:26 PM still running",
            "✻ Cooked for 9s · done Tuesday",
            "✻ Cooked for 9s · done Tuesday, 3 shells",
            "✻ Brewed for 1x 2h",
        ] {
            assert!(!is_completed_turn_line(busy), "not completed: {busy:?}");
            let grid = lines(&[busy, "❯ "]);
            assert!(!snapshot_looks_idle(&grid, 1), "must be busy: {busy:?}");
        }
        // A completed line from the last turn does not hide a live spinner
        // painted for the next one.
        let grid = lines(&[
            "✻ Cogitated for 9m 42s · done 6:36 AM",
            "✶ Pondering… (3s)",
            "❯ ",
        ]);
        assert!(!snapshot_looks_idle(&grid, 2));
    }

    /// A `❯ <message>` echo in scrollback is not the input box: with the
    /// cursor up in streaming output, between the echo and the real prompt,
    /// the screen is busy.
    #[test]
    fn a_scrollback_prompt_echo_does_not_stand_in_for_the_input_row() {
        let grid = lines(&[
            "❯ Run the shell command and report back",
            "  streaming answer text",
            "  more streaming answer text",
            "❯ ",
        ]);
        assert!(!snapshot_looks_idle(&grid, 1), "cursor in streaming output");
        assert!(idle_veto(&grid, 1).unwrap().contains("prompt row 3"));
        assert!(snapshot_looks_idle(&grid, 3), "cursor on the input row");
    }

    #[test]
    fn idle_veto_names_the_check_that_fired() {
        let v = |rows: &[&str], c: u16| idle_veto(&lines(rows), c);
        assert_eq!(v(&["done", "❯ "], 1), None);
        assert!(v(&["  12.3k tokens · esc to interrupt", "❯ "], 1)
            .unwrap()
            .starts_with("processing indicator `esc to interrupt`"));
        assert!(v(&["✶ Pondering", "❯ "], 1)
            .unwrap()
            .starts_with("spinner glyph `✶` on row 0"));
        assert_eq!(v(&["out"], 0).unwrap(), "no `❯` prompt row on screen");
        assert_eq!(
            v(&["a", "❯ "], 0).unwrap(),
            "cursor on row 0, above the `❯` prompt row 1"
        );
        // The completed-turn line names no veto.
        assert_eq!(v(&["✻ Cooked for 16s · done 10:30 AM", "❯ "], 1), None);
    }

    #[test]
    fn a_completed_turn_summary_row_does_not_veto_idle() {
        // Real rows Claude Code leaves in the scrollback after a turn ends.
        for done in [
            "✻ Worked for 1m 23s · done 1:44 PM",
            "✻ Churned for 40s",
            "  ✶ Cooked for 2h 3m 4s",
        ] {
            assert!(
                snapshot_looks_idle(&lines(&[done, "", "❯ "]), 2),
                "a finished-turn summary must not read as a spinner: {done}"
            );
        }
        // A LIVE spinner row still vetoes, even beside a summary row.
        assert!(!snapshot_looks_idle(
            &lines(&["✻ Worked for 1m 23s", "✢ Beaming… (34s)", "❯ "]),
            2
        ));
        // A glyph row that merely mentions "for" is not a summary.
        assert!(!snapshot_looks_idle(
            &lines(&["✻ working for you", "❯ "]),
            1
        ));
        assert!(!snapshot_looks_idle(&lines(&["✻ Worked for", "❯ "]), 1));
    }

    #[test]
    fn the_merge_shepherd_idle_screen_with_a_non_ascii_verb_reads_idle() {
        // The settled screen the merge shepherd's tab sat on for two days
        // (2026-11-03 to 2026-11-05) while a pre-def4c43e8 runner never nudged
        // it, replayed from the tab's own PTY bytes. Claude Code's past-tense
        // verbs are not all ASCII ("Sautéed"), so the summary-row test must
        // stay Unicode-aware; the footer rows are the real ones too.
        let grid = lines(&[
            "  - Clauses not followed: none.",
            "",
            "✻ Sautéed for 4m 36s · done 3:36 PM",
            "",
            "──────────────────────────────── loop-Merge Shepherd ─",
            "❯ ",
            "───────────────────────────────────────────────────────",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle)   ✔ Update installed · Restart to apply",
            "new task? /clear to save 188k tokens",
        ]);
        assert!(snapshot_looks_idle(&grid, 7));
    }

    #[test]
    fn cursor_above_prompt_row_is_not_idle() {
        let grid = lines(&["streaming output", "more", "❯ "]);
        // Cursor up in the output region ⇒ still painting ⇒ not idle.
        assert!(!snapshot_looks_idle(&grid, 0));
        // Cursor at/below the prompt row ⇒ idle.
        assert!(snapshot_looks_idle(&grid, 2));
    }

    // The next four cases came over from the deleted poller copy of this
    // predicate: they exercise the boxed input frame Claude Code actually
    // renders rather than a bare `❯` row.

    #[test]
    fn boxed_input_frame_with_cursor_on_the_prompt_row_is_idle() {
        let grid = lines(&[
            "Some earlier output line.",
            "Another line of a finished turn.",
            "",
            "╭──────────────────────────────────────────╮",
            "│ ❯                                          │",
            "╰──────────────────────────────────────────╯",
        ]);
        assert!(snapshot_looks_idle(&grid, 4));
    }

    #[test]
    fn working_line_vetoes_idle_even_with_a_boxed_prompt() {
        let grid = lines(&[
            "✻ Thinking…",
            "  Reticulating splines… (esc to interrupt)",
            "│ ❯                                          │",
        ]);
        assert!(!snapshot_looks_idle(&grid, 2));
    }

    #[test]
    fn static_middot_chrome_beside_a_boxed_prompt_is_idle() {
        let grid = lines(&[
            "Context · 42% used",
            "│ ❯                                          │",
        ]);
        assert!(snapshot_looks_idle(&grid, 1));
    }

    #[test]
    fn streaming_output_with_no_prompt_is_not_idle() {
        let grid = lines(&[
            "Here is a long answer still being written",
            "and another line of output",
            "and more output",
        ]);
        assert!(!snapshot_looks_idle(&grid, 2));
    }

    #[test]
    fn empty_or_booting_grid_is_not_idle() {
        // A just-spawned tab (blank grid) must read NOT idle — conservative.
        assert!(!snapshot_looks_idle(&lines(&["", "", ""]), 0));
        assert!(!snapshot_looks_idle(&[], 0));
    }

    // ── snapshot_shows_dialog ────────────────────────────────────────────

    /// Claude Code's workspace-trust dialog as the runner's grid renders it
    /// (coord finding `9089767f`): literal rows, cursor parked on the
    /// selected choice (row 13).
    fn trust_dialog() -> Vec<String> {
        lines(&[
            "────────────────────────────────────────────────────────────────",
            " Accessing workspace:",
            "",
            " C:\\Users\\jspin\\.qontinui\\runner\\looping-agents\\merge-shepherd",
            "",
            " Quick safety check: Is this a project you created or one you trust? (Like your",
            " own code, a well-known open source project, or work from your team). If not,",
            " take a moment to review what's in this folder first.",
            "",
            " Claude Code'll be able to read, edit, and execute files here.",
            "",
            " Security guide",
            "",
            " ❯ 1. Yes, I trust this folder",
            "   2. No, exit",
            "",
            " Enter to confirm · Esc to cancel",
        ])
    }

    #[test]
    fn the_trust_dialog_fools_the_idle_predicate_but_is_a_dialog() {
        let grid = trust_dialog();
        // The defect: no working indicator, a `❯` row, cursor on it => "idle".
        assert!(snapshot_looks_idle(&grid, 13));
        // The guard: it is a dialog, so nothing may type Enter into it.
        assert_eq!(
            snapshot_shows_dialog(&grid),
            Some("selected numbered choice on a `❯` row")
        );
    }

    #[test]
    fn the_trust_dialog_is_caught_by_its_text_without_the_pointer_row() {
        // A renderer that drew the selection differently still prints the
        // dialog's own text and chrome.
        let mut grid = trust_dialog();
        grid[13] = "   1. Yes, I trust this folder".to_string();
        assert_eq!(snapshot_shows_dialog(&grid), Some("quick safety check"));
        grid[5] = String::new();
        assert_eq!(
            snapshot_shows_dialog(&grid),
            Some("yes, i trust this folder")
        );
        grid[13] = String::new();
        assert_eq!(snapshot_shows_dialog(&grid), Some("esc to cancel"));
    }

    #[test]
    fn a_permission_prompt_is_a_dialog() {
        let grid = lines(&[
            " Bash command",
            "   gh pr list --state open",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don't ask again for gh commands in this project",
            "   3. No, and tell Claude what to do differently (esc)",
        ]);
        assert!(snapshot_looks_idle(&grid, 3));
        assert_eq!(
            snapshot_shows_dialog(&grid),
            Some("selected numbered choice on a `❯` row")
        );
    }

    #[test]
    fn every_dialog_key_hint_is_a_dialog() {
        for hint in [
            "Esc to cancel",
            "Enter to confirm · Esc to cancel",
            "↑/↓ to navigate · Enter to select · Esc to go back",
            "Enter to view",
            "Esc to close",
            "Esc to exit",
        ] {
            let grid = lines(&[" Select a model", "   Sonnet", "❯ Opus", "", hint]);
            assert!(snapshot_shows_dialog(&grid).is_some(), "{hint}");
        }
    }

    #[test]
    fn the_idle_input_prompt_is_not_a_dialog() {
        let grid = lines(&[
            "● Cycle 12 complete — 3 PRs shepherded, journal updated.",
            "1. First, run the tests",
            "╭──────────────────────────────────────────╮",
            "│ ❯                                          │",
            "╰──────────────────────────────────────────╯",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
        ]);
        assert!(snapshot_looks_idle(&grid, 3));
        assert_eq!(snapshot_shows_dialog(&grid), None);
        // A typed draft that happens to start with a digit is not `1. ` shaped.
        let mut draft = grid.clone();
        draft[3] = "│ ❯ 12 PRs                                  │".to_string();
        assert_eq!(snapshot_shows_dialog(&draft), None);
    }

    #[test]
    fn dialog_text_quoted_far_up_the_transcript_is_not_a_dialog() {
        let mut rows = vec!["the dialog said: Enter to confirm · Esc to cancel".to_string()];
        rows.extend((0..10).map(|i| format!("transcript line {i}")));
        rows.push("│ ❯                                          │".to_string());
        assert_eq!(snapshot_shows_dialog(&rows), None);
        // An echoed `❯ 1. …` line up in the transcript is not the live row.
        let echoed = lines(&[
            "❯ 1. Fix the flaky test",
            "● Done.",
            "│ ❯                                          │",
        ]);
        assert_eq!(snapshot_shows_dialog(&echoed), None);
        // ...but the same text within reach of the prompt is.
        let near = lines(&["Enter to confirm · Esc to cancel", "│ ❯ │"]);
        assert_eq!(snapshot_shows_dialog(&near), Some("esc to cancel"));
    }

    // ── the idle `/clear` hint ───────────────────────────────────────────

    /// Claude Code's idle footer carries `tokens ·`, the working indicator's
    /// text. A finished pane at an empty prompt showing it must read idle
    /// (plan `2026-11-04-runner-finished-close-closes-nothing-on-any-box`
    /// Phase 3; adopted from runner#2369).
    #[test]
    fn the_clear_hint_footer_reads_idle() {
        for hint in [
            "  new task? /clear to save 320.4k tokens · Checking for updates",
            "  new task? /clear to save 1.2M tokens · ✔ Update installed · Restart to apply",
            "  new task? /clear to save 87,512 tokens · Checking for updates",
            "  new task? /clear to save 912 tokens · …",
            // Truncated to the pane's width: no `tokens ·` survives, so this
            // guards against a false busy rather than exercising the mask.
            "  new task? /clear to save 320.4… · Checking for updates",
        ] {
            let grid = lines(&[
                "● Done — the change is committed.",
                "",
                hint,
                "────────────────────────────────────────",
                "❯\u{a0}",
                "────────────────────────────────────────",
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            ]);
            assert!(snapshot_looks_idle(&grid, 4), "must be idle with: {hint}");
            assert_eq!(snapshot_shows_dialog(&grid), None, "{hint}");
        }
    }

    /// Only the hint is masked: a real working indicator still vetoes, on
    /// the hint's own row or on any other.
    #[test]
    fn the_clear_hint_does_not_mask_real_work() {
        let hint = "  new task? /clear to save 320.4k tokens · Checking for updates";
        for busy in [
            "✻ Cogitating… (12s · ↑ 1.2k tokens · esc to interrupt)",
            "  12.3k tokens · esc to interrupt",
            "  4.5k tokens · thinking)",
            "Compacting conversation",
        ] {
            assert!(
                !snapshot_looks_idle(&lines(&[busy, hint, "❯ "]), 2),
                "must be busy with: {busy}"
            );
        }
        // An indicator on the hint's own row, outside the hint.
        let same_row = "new task? /clear to save 320.4k tokens · 2.0k tokens · esc";
        assert!(!snapshot_looks_idle(&lines(&[same_row, "❯ "]), 1));
        // `tokens ·` with no hint around it still vetoes.
        assert!(!snapshot_looks_idle(&lines(&["  2.0k tokens · ", "❯ "]), 1));
        // A hint-like string with no token count is not the hint.
        assert!(!snapshot_looks_idle(
            &lines(&["/clear to save tokens · now", "❯ "]),
            1
        ));
    }

    /// The hint can sit above a genuine dialog. Masking it must not make the
    /// dialog guard pass: the dialog is still a dialog.
    #[test]
    fn a_dialog_under_the_clear_hint_is_still_a_dialog() {
        let grid = lines(&[
            "  new task? /clear to save 320.4k tokens · Checking for updates",
            " Bash command",
            "   gh pr list --state open",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. No, and tell Claude what to do differently (esc)",
            "",
            " Enter to confirm · Esc to cancel",
        ]);
        // With the hint masked the idle predicate reads this screen idle
        // (cursor on the `❯ 1. Yes` row), which is exactly why the dialog
        // guard must be the conjunct that refuses it.
        assert!(snapshot_looks_idle(&grid, 4));
        assert_eq!(
            snapshot_shows_dialog(&grid),
            Some("selected numbered choice on a `❯` row")
        );
    }

    /// [`idle_verdict`] as its wire value, so a test names the clause.
    fn verdict(grid: &[String], cursor_row: u16) -> Option<String> {
        idle_verdict(grid, cursor_row).map(IdleVeto::wire)
    }

    /// Rows the shape check `def4c43e8` once exempted, and that
    /// `is_completed_turn_line` does not: work that is still running after the
    /// turn's summary (plan
    /// `2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-for-a-spinner`,
    /// discovered edge). Each must keep its glyph's veto.
    #[test]
    fn a_summary_shaped_row_with_work_still_running_vetoes_idle() {
        for row in [
            "✻ Waiting for 1 background agent to finish",
            "✻ Cogitated for 1m 41s · done 5:09 AM · 1 shell still running",
            "✻ Worked FOR 1m 23s",
        ] {
            assert_eq!(
                verdict(&lines(&[row, "", "❯ "]), 2),
                Some("glyph:✻".to_string()),
                "{row}"
            );
        }
    }

    // ── finished-pane fixtures (plan 2026-10-23-wind-down-idle-check-
    //    mistakes-the-completed-turn-line-for-a-spinner, Phase 1) ──────────
    //
    // SYNTHESIZED frames, not captures: panes carry session content. Each
    // models the bottom of a Claude Code 2.1.309 screen (the build on the box
    // the plan measured) as the runner's grid renders it — a finished turn
    // above an empty boxed input, the status footer below it. A, B and C are
    // what a finished idle session routinely shows; D and G must stay busy.

    fn edge() -> String {
        "─".repeat(75)
    }

    /// A finished turn above an empty prompt, `footer` under the box. The
    /// cursor sits on the prompt row (row 4).
    fn finished_frame(turn_line: &str, footer: &str) -> Vec<String> {
        vec![
            "● Done. The PR is open and CI is green.".to_string(),
            String::new(),
            turn_line.to_string(),
            edge(),
            "❯\u{a0}".to_string(),
            edge(),
            footer.to_string(),
        ]
    }

    const PLAIN_FOOTER: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";

    /// (A) CLI 2.1.309 — the completed-turn line left above the prompt.
    #[test]
    fn fixture_a_completed_turn_line_above_an_empty_prompt_is_idle() {
        for turn in [
            "✻ Cooked for 5m 33s · done 8:55 PM",
            "✻ Cooked for 5m 33s",
            "✶ Baked for 2s",
            "✻ Sautéed for 1h 2m 3s · done 11:04 AM",
            "✻ Worked for 1h 2m",
        ] {
            let grid = finished_frame(turn, PLAIN_FOOTER);
            assert!(
                snapshot_looks_idle(&grid, 4),
                "must be idle with line: {turn}"
            );
        }
    }

    /// (B) CLI 2.1.309 — the context hint while the auto-updater runs.
    #[test]
    fn fixture_b_clear_hint_with_auto_updating_is_idle() {
        let grid = finished_frame(
            "● Pushed.",
            "  new task? /clear to save 399.4k tokens · Auto-updating…",
        );
        assert!(snapshot_looks_idle(&grid, 4));
    }

    /// (C) CLI 2.1.309 — the context hint after the update installed.
    #[test]
    fn fixture_c_clear_hint_with_update_installed_is_idle() {
        let grid = finished_frame(
            "✻ Cooked for 5m 33s · done 8:55 PM",
            "  new task? /clear to save 399.4k tokens · ✔ Update installed · Restart to apply",
        );
        assert!(snapshot_looks_idle(&grid, 4));
    }

    /// (D) CLI 2.1.309 — a WORKING spinner line. Busy by both routes: the
    /// ellipsis/parenthesis break the completed-turn shape, and `esc to
    /// interrupt` is an indicator.
    #[test]
    fn fixture_d_working_spinner_is_busy() {
        for working in [
            "✻ Cooking… (12s · esc to interrupt)",
            "✻ Cooking… (5m 33s · ↑ 1.2k tokens · esc to interrupt)",
        ] {
            let grid = finished_frame(working, PLAIN_FOOTER);
            assert!(
                !snapshot_looks_idle(&grid, 4),
                "must be busy with line: {working}"
            );
        }
        // A working token footer is NOT exempted by the `/clear` rule.
        let grid = finished_frame("● Pushed.", "  ↑ 1.2k tokens · esc to interrupt");
        assert!(!snapshot_looks_idle(&grid, 4));
        // A completed-turn line does not hide a live spinner elsewhere.
        let mut grid = finished_frame("✻ Cooked for 5m 33s · done 8:55 PM", PLAIN_FOOTER);
        grid[1] = "✶ Pondering".to_string();
        assert!(!snapshot_looks_idle(&grid, 4));
    }

    /// The completed-turn line's glyph class is exactly `SPINNER_GLYPHS` minus
    /// the middot the glyph clause skips.
    #[test]
    fn completed_turn_glyph_class_is_the_spinner_set() {
        for glyph in SPINNER_GLYPHS.iter().filter(|g| **g != '·') {
            assert!(
                is_completed_turn_line(&format!("{glyph} Cooked for 2s")),
                "{glyph}"
            );
        }
        assert!(!is_completed_turn_line("· Cooked for 2s"));
        assert!(!is_completed_turn_line("● Cooked for 2s"));
    }

    /// Review L-2: only the turn that just ended is exempt — the LAST glyph
    /// line, directly above the input area. A live spinner BELOW a
    /// completed-turn line, or a completed-turn-shaped line higher up with
    /// output under it, stays busy.
    #[test]
    fn only_the_last_glyph_line_directly_above_the_input_is_exempt() {
        let mut below = finished_frame("✻ Cooked for 5m 33s · done 8:55 PM", PLAIN_FOOTER);
        below.insert(3, "✶ Pondering".to_string());
        // The first non-exempt glyph line is reported: the completed-turn
        // line itself, which is no longer the last.
        assert_eq!(verdict(&below, 5), Some("glyph:✻".to_string()));

        let stray = lines(&[
            "✶ Baked for 2s",
            "  ⎿ a tool result still streaming",
            "● Working on the next step",
            "",
            &edge(),
            "❯\u{a0}",
            &edge(),
        ]);
        assert_eq!(verdict(&stray, 5), Some("glyph:✶".to_string()));

        // The layout the census measured on real finished panes: a recap
        // paragraph, the context hint and the box's titled top edge between
        // the completed-turn line and the input row.
        let measured = lines(&[
            "● Pushed; CI is green.",
            "✻ Cooked for 5m 33s · done 8:55 PM",
            "",
            "※ recap: fixed the build and opened the PR; CI is green.",
            "  (disable recaps in /config)",
            "  new task? /clear to save 399.4k tokens · ✔ Update installed · Restart to apply",
            "──────────────── my-session ─",
            "❯\u{a0}",
            &edge(),
        ]);
        assert_eq!(verdict(&measured, 7), None);
        // A narrow pane cuts the hint short; it is still the hint.
        let mut narrow = measured.clone();
        narrow[5] = "new task? /clear to 399.…".to_string();
        assert_eq!(verdict(&narrow, 7), None);
        // A recap whose closing hint wrapped onto its own row still closes.
        let mut wrapped = measured.clone();
        wrapped[4] = "  (disable recaps in".to_string();
        wrapped.insert(5, "  /config)".to_string());
        assert_eq!(verdict(&wrapped, 8), None);
        // A recap with no closing hint (one census pane) is still admitted
        // while its rows are prose...
        let mut unclosed = measured.clone();
        unclosed[4] = "  and more prose with no closing hint".to_string();
        assert_eq!(verdict(&unclosed, 7), None);
        // ...but a row inside it that starts a transcript item is not.
        let mut item_in_recap = measured.clone();
        item_in_recap[4] = "● Next I will run the migration.".to_string();
        assert_eq!(verdict(&item_in_recap, 7), Some("glyph:✻".to_string()));
        // Every transcript-item glyph ends a recap's admission.
        for item in [
            "⏺ Next step",
            "☐ Run the tests",
            "☒ Lint",
            "✔ Build",
            "✓ Done",
        ] {
            let mut grid = measured.clone();
            grid[4] = format!("  {item}");
            assert_eq!(verdict(&grid, 7), Some("glyph:✻".to_string()), "{item}");
        }
        let mut result_in_recap = measured.clone();
        result_in_recap[4] = "  ⎿  Ran 3 tests".to_string();
        assert_eq!(verdict(&result_in_recap, 7), Some("glyph:✻".to_string()));
        // Arbitrary text AFTER a closed recap is not admitted either.
        let mut after_recap = measured.clone();
        after_recap.insert(5, "● Next I will run the migration.".to_string());
        assert_eq!(verdict(&after_recap, 8), Some("glyph:✻".to_string()));
        // Plain prose after a CLOSED recap is not admitted either.
        let mut prose_after = measured.clone();
        prose_after.insert(5, "  some more words".to_string());
        assert_eq!(verdict(&prose_after, 8), Some("glyph:✻".to_string()));
        // A live spinner directly above the hint row is busy.
        let mut live = measured.clone();
        live[4] = "✶ Pondering".to_string();
        assert_eq!(verdict(&live, 7), Some("glyph:✻".to_string()));
        // ...but prose that is not a recap block, in the same place, is not.
        let mut prose = measured.clone();
        prose[3] = "● Next I will run the migration.".to_string();
        assert_eq!(verdict(&prose, 7), Some("glyph:✻".to_string()));

        // An echoed earlier user prompt (`❯ …`) ABOVE the completed-turn line
        // is transcript, not the input; the input row below still counts.
        let echoed = lines(&[
            "❯ please fix the build",
            "● Fixed and pushed.",
            "✻ Cooked for 5m 33s · done 8:55 PM",
            "",
            &edge(),
            "❯\u{a0}",
            &edge(),
        ]);
        assert_eq!(verdict(&echoed, 5), None);

        // Below the prompt (or with no prompt at all) is never "above the input".
        let after = lines(&["❯\u{a0}", "✻ Cooked for 5m 33s"]);
        assert_eq!(verdict(&after, 0), Some("glyph:✻".to_string()));
        assert_eq!(
            verdict(&lines(&["✻ Cooked for 5m 33s"]), 0),
            Some("glyph:✻".to_string())
        );

        // Two completed-turn lines on screen: an earlier turn's line still
        // visible is not the one that just ended — fail closed (busy) until it
        // scrolls off.
        let mut two = finished_frame("✻ Cooked for 5m 33s · done 8:55 PM", PLAIN_FOOTER);
        two[0] = "✻ Baked for 2s".to_string();
        assert_eq!(verdict(&two, 4), Some("glyph:✻".to_string()));
    }

    /// The grace floor's fingerprint moves with what an operator does at the
    /// prompt, and NOT with the context hint's update notice.
    #[test]
    fn screen_fingerprint_ignores_only_the_context_hint() {
        let base = finished_frame("✻ Cooked for 5m 33s · done 8:55 PM", PLAIN_FOOTER);
        let fp = |grid: &[String], col: u16| screen_fingerprint(grid, 4, col);
        let mut hint_a = base.clone();
        hint_a.insert(
            3,
            "  new task? /clear to save 399.4k tokens · Auto-updating…".into(),
        );
        let mut hint_b = base.clone();
        hint_b.insert(
            3,
            "  new task? /clear to save 399.4k tokens · ✔ Update installed".into(),
        );
        assert_eq!(fp(&hint_a, 2), fp(&hint_b, 2));
        let mut draft = base.clone();
        draft[4] = "❯\u{a0}d".to_string();
        assert_ne!(fp(&base, 2), fp(&draft, 3));
        assert_ne!(fp(&base, 2), fp(&base, 3));
        assert_eq!(fp(&base, 2), fp(&base.clone(), 2));
    }

    /// Review L-4: the `/clear` exemption covers `tokens ·` ONLY — a working
    /// indicator on the same line still reads busy.
    #[test]
    fn a_clear_hint_line_that_also_says_esc_to_interrupt_is_busy() {
        let grid = finished_frame(
            "● Pushed.",
            "  new task? /clear to save 399.4k tokens · esc to interrupt",
        );
        assert_eq!(
            verdict(&grid, 4),
            Some("indicator:esc to interrupt".to_string())
        );
    }

    /// (G) CLI 2.1.309 shape, unrecognised wording — fail closed: a glyph line
    /// that does not match the WHOLE completed-turn shape stays busy.
    #[test]
    fn fixture_g_unrecognised_completed_turn_wording_is_busy() {
        for odd in [
            "✻ Cooked for about 5 minutes",
            "✻ Cooked for 5 min 33 sec",
            "✻ Cooked for 5m 33s and counting",
            "✻ Cooked for 5m 33s · done 8:55 PM · still going",
            "✻ Really cooked for 5m 33s",
            "✻ Cooked for",
            "✻ Cooked for ",
            "● ✻ Cooked for 5m 33s",
        ] {
            let grid = finished_frame(odd, PLAIN_FOOTER);
            assert!(
                !snapshot_looks_idle(&grid, 4),
                "must be busy with line: {odd}"
            );
        }
    }

    // ── idle_verdict: every value ──────────────────────────────

    #[test]
    fn verdict_names_the_first_clause_that_vetoed_idle() {
        assert_eq!(verdict(&lines(&["cycle done.", "❯ "]), 1), None);
        // The indicator is the matched CONSTANT, not the screen text.
        assert_eq!(
            verdict(&lines(&["  12.3K TOKENS · ESC TO INTERRUPT", "❯ "]), 1),
            Some("indicator:esc to interrupt".to_string())
        );
        assert_eq!(
            verdict(&lines(&["↑ 1.2k tokens · working", "❯ "]), 1),
            Some("indicator:tokens ·".to_string())
        );
        // Indicators outrank glyphs (clause order).
        assert_eq!(
            verdict(&lines(&["✻ Thinking…", "❯ "]), 1),
            Some("indicator:thinking…".to_string())
        );
        assert_eq!(
            verdict(&lines(&["✶ Pondering", "❯ "]), 1),
            Some("glyph:✶".to_string())
        );
        assert_eq!(
            verdict(&lines(&["some output", "more output"]), 1),
            Some("no_prompt".to_string())
        );
        assert_eq!(
            verdict(&lines(&["streaming output", "more", "❯ "]), 0),
            Some("cursor_above_prompt".to_string())
        );
        // The bool view agrees with the verdict.
        for (grid, row) in [
            (lines(&["cycle done.", "❯ "]), 1),
            (lines(&["✶ Pondering", "❯ "]), 1),
            (lines(&["streaming output", "❯ "]), 0),
        ] {
            assert_eq!(
                snapshot_looks_idle(&grid, row),
                verdict(&grid, row).is_none()
            );
        }
    }

    // ── snapshot_context_low ─────────────────────────────────────────────

    #[test]
    fn context_low_bare_marker_triggers() {
        let grid = lines(&["Context low (12% remaining) · Run /compact", "❯ "]);
        assert!(snapshot_context_low(&grid, 15));
    }

    #[test]
    fn auto_compact_percentage_is_threshold_gated() {
        let low = lines(&["Context left until auto-compact: 8%"]);
        assert!(snapshot_context_low(&low, 15));
        let exact = lines(&["Context left until auto-compact: 15%"]);
        assert!(snapshot_context_low(&exact, 15), "boundary is inclusive");
        let plenty = lines(&["Context left until auto-compact: 42%"]);
        assert!(
            !snapshot_context_low(&plenty, 15),
            "healthy countdown must not trigger a relaunch"
        );
    }

    #[test]
    fn unparseable_auto_compact_line_is_ignored() {
        let grid = lines(&["context left until auto-compact: soon"]);
        assert!(!snapshot_context_low(&grid, 15));
        let grid2 = lines(&["until auto-compact: 8 percent"]);
        assert!(!snapshot_context_low(&grid2, 15));
    }

    #[test]
    fn clean_screen_is_not_context_low() {
        assert!(!snapshot_context_low(&lines(&["all good", "❯ "]), 15));
        assert!(!snapshot_context_low(&[], 15));
    }

    #[test]
    fn parse_leading_percentage_shapes() {
        assert_eq!(parse_leading_percentage(" 8%"), Some(8));
        assert_eq!(parse_leading_percentage("  15% left"), Some(15));
        assert_eq!(parse_leading_percentage("100%"), Some(100));
        assert_eq!(parse_leading_percentage("soon"), None);
        assert_eq!(parse_leading_percentage("8 percent"), None);
        assert_eq!(parse_leading_percentage(""), None);
    }
}
