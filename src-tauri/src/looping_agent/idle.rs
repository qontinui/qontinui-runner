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

/// The completed-turn line Claude Code leaves above the prompt when a turn
/// ends: `✻ Cooked for 5m 33s · done 8:55 PM`. The WHOLE line must match — a
/// spinner glyph, ONE word, ` for `, a duration of hours/minutes/seconds in
/// that order (`5m 33s`, `2s`, `1h 2m`, `5m`), optionally `· done <h:mm
/// AM/PM>` — or the glyph keeps counting as busy. Fail closed: a wording
/// Claude Code changes reads busy, which is the behaviour before this
/// exemption, never a new idle. Where on the screen the line must sit is the
/// predicate's job, not this pattern's (see [`snapshot_idle_verdict`]).
///
/// It replaces the shape check `def4c43e8` added (`is_completed_turn_summary`,
/// which exempted such a row ANYWHERE on screen and accepted any tail after
/// the first duration token — `✻ Waiting for 1 background agent to finish`
/// and `… · done 5:09 AM · 1 still running` both read as finished turns).
/// Every row this pattern accepts, that check accepted too: the verb is
/// alphabetic only and `for` is lowercase, as they were there.
///
/// Ported from `TURN_END` in qontinui-claude-config
/// `scripts/close-finished-sessions.py`, and NOT identical to it. Narrower:
/// it anchors both ends of the line (the script scans a byte stream, so it
/// cannot), it needs whitespace on both sides of `for`, and `done <time>` is
/// accepted only as this line's suffix (the script also matches a bare
/// `done 8:55 PM` anywhere). Wider: the duration need not end in seconds
/// (bare `5m`, `1h`, `1h 2m` match; the script requires an `Ns` part). And
/// the glyph class differs: it is [`SPINNER_GLYPHS`] minus `·` — the exact set
/// the glyph clause vetoes on — which includes `∗` (U+2217, outside the
/// script's U+2700–U+27BF dingbat range) and excludes every other dingbat in
/// that range.
static COMPLETED_TURN_LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(concat!(
        r"^\s*[✻✶✳✽✢∗]\s*\p{Alphabetic}+\s+for\s+",
        r"(?:\d+h(?:\s+\d+m)?(?:\s+\d+s)?|\d+m(?:\s+\d+s)?|\d+s)",
        r"(?:\s*·\s*done\s+\d{1,2}:\d{2}\s*[AaPp][Mm])?\s*$",
    ))
    .expect("COMPLETED_TURN_LINE compiles")
});

/// Is `line` exactly a completed-turn line (`COMPLETED_TURN_LINE`)?
pub fn is_completed_turn_line(line: &str) -> bool {
    COMPLETED_TURN_LINE.is_match(line)
}

/// Which clause of the turn-complete recognizer decided a snapshot — the
/// typed form of [`snapshot_looks_idle`]'s answer.
///
/// A busy variant names the FIRST clause that vetoed idle, in the order the
/// predicate checks them. [`IdleVerdict::Indicator`] carries the matched
/// [`PROCESSING_INDICATORS`] constant, never screen text: panes carry session
/// content, and this verdict reaches `GET /restart-readiness` as
/// `windDown.idleEvidence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleVerdict {
    /// Every clause holds: the screen looks ready for input.
    Idle,
    /// (1) A processing indicator is on screen.
    Indicator(&'static str),
    /// (2) A spinner glyph is on screen, outside a completed-turn line.
    Glyph(char),
    /// (3) No `❯` prompt row is visible.
    NoPrompt,
    /// (4) The cursor sits above the prompt row, up in streaming output.
    CursorAbovePrompt,
}

impl IdleVerdict {
    pub fn is_idle(self) -> bool {
        self == Self::Idle
    }
}

/// Decide whether a single rendered grid snapshot looks IDLE / ready for
/// input, and if not, which clause said so. Pure over the snapshot.
/// CONSERVATIVE: anything not positively idle is busy. Idle requires ALL of:
///
/// 1. NO processing-indicator text anywhere on screen
///    ([`PROCESSING_INDICATORS`]) — except inside Claude Code's idle
///    `/clear` hint ([`CLEAR_HINT`]), which alone is masked.
/// 2. NO spinner glyph on screen ([`SPINNER_GLYPHS`], `·` excepted) — except
///    on ONE line: the LAST glyph-bearing line on screen, when it is exactly a
///    completed-turn line ([`is_completed_turn_line`]) and sits directly above
///    the input area — only blank or box-edge rows between it and the next
///    `❯` row below it (an earlier `❯` row above it is an echoed user prompt).
///    Fail closed by construction: a screen still showing an EARLIER turn's
///    completed-turn line as well reads busy until that line scrolls off, and
///    so does any row between the line and the input that is not one of the
///    shapes [`is_turn_to_input_gap`] admits. A completed-turn-shaped line anywhere else (higher up in the
///    transcript, or with a live spinner below it) is not the turn that just
///    ended, and stays busy.
/// 3. A prompt row containing [`PROMPT_MARKER`] (`❯`) is visible.
/// 4. The cursor sits AT OR BELOW the prompt row (in the input area), not up
///    in streaming output.
///
/// The two exceptions exist because a FINISHED idle pane routinely shows both
/// shapes, and without them it read busy forever (plan
/// `2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-for-a-spinner`).
/// There is NO draft or dialog check here. A caller that TYPES must also pass
/// `terminal::graceful_exit`'s `exit_prompt_ready` / `input_prompt_ready` on
/// the screen it is about to type into — `graceful_exit::drive_graceful_exit`, the session
/// message poller's typed injection and the looping-agent nudge do. A pane
/// stopped at a dialog under a completed-turn line reads idle here.
pub fn snapshot_idle_verdict(lines: &[String], cursor_row: u16) -> IdleVerdict {
    // (1) any processing-indicator text ⇒ busy.
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(ind) = PROCESSING_INDICATORS
            .iter()
            .find(|ind| indicator_outside_clear_hint(&lower, ind))
        {
            return IdleVerdict::Indicator(ind);
        }
    }

    let prompt_row = lines.iter().position(|l| l.contains(PROMPT_MARKER));

    // (2) spinner glyphs (excluding bare `·`) ⇒ busy, unless the glyph line is
    // the turn that just ended: the LAST glyph line, a completed-turn line,
    // directly above the input area.
    let glyph_rows: Vec<(usize, char)> = lines
        .iter()
        .enumerate()
        .filter_map(|(row, line)| {
            line.chars()
                .find(|ch| *ch != '·' && SPINNER_GLYPHS.contains(ch))
                .map(|glyph| (row, glyph))
        })
        .collect();
    if let Some(&(last_row, _)) = glyph_rows.last() {
        // The input row is the first `❯` row BELOW the line — not the first
        // on screen: Claude Code echoes each earlier user prompt with `❯`,
        // so a `❯` row above the completed-turn line is transcript.
        let exempt = |row: usize| {
            row == last_row
                && is_completed_turn_line(&lines[row])
                && lines[row + 1..]
                    .iter()
                    .position(|l| l.contains(PROMPT_MARKER))
                    .is_some_and(|gap| is_turn_to_input_gap(&lines[row + 1..row + 1 + gap]))
        };
        if let Some(&(_, glyph)) = glyph_rows.iter().find(|(row, _)| !exempt(*row)) {
            return IdleVerdict::Glyph(glyph);
        }
    }

    // (3) prompt marker visible — remember its row for (4).
    let Some(prompt_row) = prompt_row else {
        // No visible input prompt ⇒ can't confirm ready-for-input ⇒ not idle.
        return IdleVerdict::NoPrompt;
    };

    // (4) cursor in the input area (at/below the prompt row).
    if (cursor_row as usize) >= prompt_row {
        IdleVerdict::Idle
    } else {
        IdleVerdict::CursorAbovePrompt
    }
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

/// [`snapshot_idle_verdict`] as a `bool`, for the consumers that only need
/// the bit. `true` only for [`IdleVerdict::Idle`].
pub fn snapshot_looks_idle(lines: &[String], cursor_row: u16) -> bool {
    snapshot_idle_verdict(lines, cursor_row).is_idle()
}

/// Key hints Claude Code prints beneath a dialog's choices — its own CHROME,
/// not prose, so they do not turn up in an ordinary finished turn. Shared with
/// `terminal::graceful_exit`'s prompt-ready checks, which scan for the same
/// hints. Lowercased before match.
pub const DIALOG_CHROME_MARKERS: &[&str] = &["esc to cancel", "enter to confirm"];

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

    /// Rows the shape check `def4c43e8` added exempted, and that the anchored
    /// completed-turn line does not: work that is still running after the
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
                snapshot_idle_verdict(&lines(&[row, "", "❯ "]), 2),
                IdleVerdict::Glyph('✻'),
                "{row}"
            );
        }
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

    /// The completed-turn glyph class is spelled out inside the regex; it must
    /// stay exactly `SPINNER_GLYPHS` minus the middot the glyph clause skips.
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
        assert_eq!(snapshot_idle_verdict(&below, 5), IdleVerdict::Glyph('✻'));

        let stray = lines(&[
            "✶ Baked for 2s",
            "  ⎿ a tool result still streaming",
            "● Working on the next step",
            "",
            &edge(),
            "❯\u{a0}",
            &edge(),
        ]);
        assert_eq!(snapshot_idle_verdict(&stray, 5), IdleVerdict::Glyph('✶'));

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
        assert_eq!(snapshot_idle_verdict(&measured, 7), IdleVerdict::Idle);
        // A narrow pane cuts the hint short; it is still the hint.
        let mut narrow = measured.clone();
        narrow[5] = "new task? /clear to 399.…".to_string();
        assert_eq!(snapshot_idle_verdict(&narrow, 7), IdleVerdict::Idle);
        // A recap whose closing hint wrapped onto its own row still closes.
        let mut wrapped = measured.clone();
        wrapped[4] = "  (disable recaps in".to_string();
        wrapped.insert(5, "  /config)".to_string());
        assert_eq!(snapshot_idle_verdict(&wrapped, 8), IdleVerdict::Idle);
        // A recap with no closing hint (one census pane) is still admitted
        // while its rows are prose...
        let mut unclosed = measured.clone();
        unclosed[4] = "  and more prose with no closing hint".to_string();
        assert_eq!(snapshot_idle_verdict(&unclosed, 7), IdleVerdict::Idle);
        // ...but a row inside it that starts a transcript item is not.
        let mut item_in_recap = measured.clone();
        item_in_recap[4] = "● Next I will run the migration.".to_string();
        assert_eq!(
            snapshot_idle_verdict(&item_in_recap, 7),
            IdleVerdict::Glyph('✻')
        );
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
            assert_eq!(
                snapshot_idle_verdict(&grid, 7),
                IdleVerdict::Glyph('✻'),
                "{item}"
            );
        }
        let mut result_in_recap = measured.clone();
        result_in_recap[4] = "  ⎿  Ran 3 tests".to_string();
        assert_eq!(
            snapshot_idle_verdict(&result_in_recap, 7),
            IdleVerdict::Glyph('✻')
        );
        // Arbitrary text AFTER a closed recap is not admitted either.
        let mut after_recap = measured.clone();
        after_recap.insert(5, "● Next I will run the migration.".to_string());
        assert_eq!(
            snapshot_idle_verdict(&after_recap, 8),
            IdleVerdict::Glyph('✻')
        );
        // Plain prose after a CLOSED recap is not admitted either.
        let mut prose_after = measured.clone();
        prose_after.insert(5, "  some more words".to_string());
        assert_eq!(
            snapshot_idle_verdict(&prose_after, 8),
            IdleVerdict::Glyph('✻')
        );
        // A live spinner directly above the hint row is busy.
        let mut live = measured.clone();
        live[4] = "✶ Pondering".to_string();
        assert_eq!(snapshot_idle_verdict(&live, 7), IdleVerdict::Glyph('✻'));
        // ...but prose that is not a recap block, in the same place, is not.
        let mut prose = measured.clone();
        prose[3] = "● Next I will run the migration.".to_string();
        assert_eq!(snapshot_idle_verdict(&prose, 7), IdleVerdict::Glyph('✻'));

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
        assert_eq!(snapshot_idle_verdict(&echoed, 5), IdleVerdict::Idle);

        // Below the prompt (or with no prompt at all) is never "above the input".
        let after = lines(&["❯\u{a0}", "✻ Cooked for 5m 33s"]);
        assert_eq!(snapshot_idle_verdict(&after, 0), IdleVerdict::Glyph('✻'));
        assert_eq!(
            snapshot_idle_verdict(&lines(&["✻ Cooked for 5m 33s"]), 0),
            IdleVerdict::Glyph('✻')
        );

        // Two completed-turn lines on screen: an earlier turn's line still
        // visible is not the one that just ended — fail closed (busy) until it
        // scrolls off.
        let mut two = finished_frame("✻ Cooked for 5m 33s · done 8:55 PM", PLAIN_FOOTER);
        two[0] = "✻ Baked for 2s".to_string();
        assert_eq!(snapshot_idle_verdict(&two, 4), IdleVerdict::Glyph('✻'));
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
            snapshot_idle_verdict(&grid, 4),
            IdleVerdict::Indicator("esc to interrupt")
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
            "✻ Cooked for 5m 33s · done 20:55",
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

    // ── snapshot_idle_verdict: every value ──────────────────────────────

    #[test]
    fn verdict_names_the_first_clause_that_vetoed_idle() {
        assert_eq!(
            snapshot_idle_verdict(&lines(&["cycle done.", "❯ "]), 1),
            IdleVerdict::Idle
        );
        // The indicator is the matched CONSTANT, not the screen text.
        assert_eq!(
            snapshot_idle_verdict(&lines(&["  12.3K TOKENS · ESC TO INTERRUPT", "❯ "]), 1),
            IdleVerdict::Indicator("esc to interrupt")
        );
        assert_eq!(
            snapshot_idle_verdict(&lines(&["↑ 1.2k tokens · working", "❯ "]), 1),
            IdleVerdict::Indicator("tokens ·")
        );
        // Indicators outrank glyphs (clause order).
        assert_eq!(
            snapshot_idle_verdict(&lines(&["✻ Thinking…", "❯ "]), 1),
            IdleVerdict::Indicator("thinking…")
        );
        assert_eq!(
            snapshot_idle_verdict(&lines(&["✶ Pondering", "❯ "]), 1),
            IdleVerdict::Glyph('✶')
        );
        assert_eq!(
            snapshot_idle_verdict(&lines(&["some output", "more output"]), 1),
            IdleVerdict::NoPrompt
        );
        assert_eq!(
            snapshot_idle_verdict(&lines(&["streaming output", "more", "❯ "]), 0),
            IdleVerdict::CursorAbovePrompt
        );
        // The bool view agrees with the verdict.
        for (grid, row) in [
            (lines(&["cycle done.", "❯ "]), 1),
            (lines(&["✶ Pondering", "❯ "]), 1),
            (lines(&["streaming output", "❯ "]), 0),
        ] {
            assert_eq!(
                snapshot_looks_idle(&grid, row),
                snapshot_idle_verdict(&grid, row).is_idle()
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
