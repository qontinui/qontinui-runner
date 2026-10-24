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

use std::sync::LazyLock;

use regex::Regex;

/// Working/processing indicators that mean Claude is mid-turn. If ANY appears
/// on the rendered screen the terminal is NOT idle. Lowercased before match.
/// Sourced from the Claude Code TUI working line ("… esc to interrupt") and
/// its transient status lines; conservative — any hit vetoes idle.
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

/// Spinner glyphs the Claude Code TUI animates while working. Their presence
/// on screen is a strong "busy" signal independent of the text indicators
/// above. The `·` middot is common in static UI, so it is deliberately NOT
/// treated as a bare busy glyph (it is covered by the `"tokens ·"` text
/// indicator instead).
pub const SPINNER_GLYPHS: &[char] = &['✻', '✶', '✳', '✽', '✢', '·', '∗'];

/// The Claude Code input-prompt marker. When Claude is ready for input the
/// input box shows this caret.
pub const PROMPT_MARKER: char = '❯';

/// The turn-complete summary Claude Code leaves under a finished reply, e.g.
/// `✻ Worked for 8m 49s · done 5:04 PM`. It opens with a spinner glyph, but it
/// says the turn ENDED, so it is idle chrome and is exempt from the spinner
/// veto (2). A working line never has this shape: it reads `✻ Cogitating…
/// (12s · esc to interrupt)` or `✻ Waiting for 1 background agent to finish`.
/// Any other tail (`… · 1 shell still running`) is not matched, so it keeps
/// vetoing. Measured 2026-10-24 on a Linux box: this line, with the
/// [`CLEAR_HINT`], was the only veto keeping 42 finished, idle panes at
/// `grid_busy`, so none ever reached wind-down.
static TURN_DONE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*[✻✶✳✽✢∗] \p{L}+ for \d+[hms](?: \d+[hms])*(?: · done \d{1,2}:\d{2}(?: ?[AaPp][Mm])?)?\s*$",
    )
    .expect("TURN_DONE_LINE is a valid regex")
});

/// Claude Code's idle "new task? /clear to save 320.4k tokens · …" hint. It
/// carries the `"tokens ·"` working indicator's text, so it is masked out of a
/// line before the indicators are matched (1). Matched lowercased.
static CLEAR_HINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"/clear to save [0-9.]+[kmb]? tokens").expect("CLEAR_HINT is a valid regex")
});

/// Key hints a Claude Code dialog prints under its choices (lowercased).
/// Unlike `do you want` / `allow`, these do not turn up in an ordinary
/// finished turn, so near the bottom of the screen they mean a dialog is
/// showing. `terminal::graceful_exit` refuses on the same list.
pub const DIALOG_CHROME: &[&str] = &["enter to confirm", "esc to cancel"];

/// How far above the last `❯` row a dialog is looked for. A dialog sits at the
/// bottom of the screen; text further up is transcript, where a quoted dialog
/// must not hold a finished session busy. `terminal::graceful_exit` scans the
/// same depth above the cursor.
pub const DIALOG_SCAN_ROWS: usize = 8;

/// Whether the text after a `❯` marker is a numbered choice (`1. Yes`), i.e.
/// a dialog's selection rather than the input line.
fn is_numbered_choice(after_marker: &str) -> bool {
    let mut chars = after_marker.trim_start().chars().peekable();
    let mut digits = 0;
    while chars.next_if(char::is_ascii_digit).is_some() {
        digits += 1;
    }
    digits > 0 && chars.next() == Some('.') && chars.next() == Some(' ')
}

/// Whether `line` is the turn-complete summary ([`TURN_DONE_LINE`]).
fn is_turn_done_line(line: &str) -> bool {
    TURN_DONE_LINE.is_match(line)
}

/// Decide whether a single rendered grid snapshot looks IDLE / ready for
/// input. Pure over the snapshot. CONSERVATIVE: returns `false` (NOT idle)
/// on any ambiguity. Idle requires ALL of:
///
/// 1. NO processing-indicator text anywhere on screen
///    ([`PROCESSING_INDICATORS`]), outside the idle `/clear` hint
///    ([`CLEAR_HINT`]).
/// 2. NO spinner glyph on screen ([`SPINNER_GLYPHS`], `·` excepted), outside
///    the turn-complete summary line ([`TURN_DONE_LINE`]).
/// 3. A prompt row containing [`PROMPT_MARKER`] (`❯`) is visible.
/// 4. The cursor sits AT OR BELOW the prompt row (in the input area), not up
///    in streaming output.
/// 5. NO dialog is showing: from [`DIALOG_SCAN_ROWS`] above the last `❯` row
///    down, no `❯` row is a numbered choice and no [`DIALOG_CHROME`] hint is
///    drawn. A dialog draws `❯` on its selected choice, so without this a
///    usage-limit or permission dialog passes (3) and (4), and a consumer that
///    then submits would answer it.
pub fn snapshot_looks_idle(lines: &[String], cursor_row: u16) -> bool {
    // (1) any processing-indicator text ⇒ busy.
    for line in lines {
        let lower = line.to_ascii_lowercase();
        let lower = CLEAR_HINT.replace_all(&lower, "");
        for ind in PROCESSING_INDICATORS {
            if lower.contains(ind) {
                return false;
            }
        }
    }

    // (2) spinner glyphs (excluding bare `·`) ⇒ busy, outside the
    // turn-complete summary.
    for line in lines {
        if is_turn_done_line(line) {
            continue;
        }
        for ch in line.chars() {
            if ch != '·' && SPINNER_GLYPHS.contains(&ch) {
                return false;
            }
        }
    }

    // (3) prompt marker visible — remember its row for (4).
    let Some(prompt_row) = lines.iter().position(|l| l.contains(PROMPT_MARKER)) else {
        // No visible input prompt ⇒ can't confirm ready-for-input ⇒ not idle.
        return false;
    };

    // (4) cursor in the input area (at/below the prompt row).
    if (cursor_row as usize) < prompt_row {
        return false;
    }

    // (5) no dialog near the bottom of the screen.
    let last_marker_row = lines
        .iter()
        .rposition(|l| l.contains(PROMPT_MARKER))
        .unwrap_or(prompt_row);
    let dialog = lines
        .iter()
        .skip(last_marker_row.saturating_sub(DIALOG_SCAN_ROWS))
        .any(|l| {
            let lower = l.to_lowercase();
            DIALOG_CHROME.iter().any(|m| lower.contains(m))
                || l.split_once(PROMPT_MARKER)
                    .is_some_and(|(_, after)| is_numbered_choice(after))
        });
    !dialog
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

    /// The shapes measured on live panes on 2026-10-24. Every one of them was
    /// a finished, idle session that this predicate read as busy.
    #[test]
    fn turn_done_summary_and_clear_hint_are_idle_chrome() {
        for done in [
            "✻ Worked for 8m 49s · done 5:04 PM",
            "✻ Cooked for 44s · done 8:56 PM",
            "✻ Sautéed for 5m 13s · done 8:54 PM",
            "✻ Crunched for 1h 2m 3s",
            "✻ Brewed for 18m 1s · done 21:08",
        ] {
            let grid = lines(&[
                "SAFE TO CLOSE.",
                done,
                "  new task? /clear to save 320.4k tokens · ✔ Update installed · Restart to apply",
                "─────────────────────────── post-merge-coord#3392 ─",
                "❯ ",
                "──────────────────────────────────────────────────",
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            ]);
            assert!(snapshot_looks_idle(&grid, 4), "must be idle with: {done}");
        }
        // The hint truncated to the pane's width.
        let grid = lines(&[
            "✻ Cooked for 7m 8s · done 8:56 PM",
            "  new task? /clear to save 320.4… · ✔ Update installed",
            "❯ ",
        ]);
        assert!(snapshot_looks_idle(&grid, 2));
    }

    #[test]
    fn work_still_running_after_the_turn_vetoes_idle() {
        for busy in [
            // A background subagent runs inside claude, with no child process
            // for the wind-down children check to see.
            "✻ Waiting for 1 background agent to finish",
            "✻ Cogitated for 1m 41s · done 5:09 AM · 1 shell still running",
            "✻ Cogitating… (12s · esc to interrupt)",
            "✻ Worked for",
            "✻ Worked for 3s · done 5:09 AM · 2 background agents",
        ] {
            let grid = lines(&[busy, "❯ "]);
            assert!(
                !snapshot_looks_idle(&grid, 1),
                "must be busy with line: {busy}"
            );
        }
        // The hint is masked, not the whole line: an indicator beside it still
        // vetoes.
        let grid = lines(&[
            "/clear to save 1.2k tokens · 3.4k tokens · esc to interrupt",
            "❯ ",
        ]);
        assert!(!snapshot_looks_idle(&grid, 1));
    }

    /// Measured 2026-10-24: a usage-limit dialog over a finished turn. Its
    /// `❯` is the selected choice, so it passed (3) and (4), and only the
    /// turn-complete line's glyph had been keeping it from reading idle.
    #[test]
    fn a_dialog_is_not_idle() {
        let usage_limit = lines(&[
            "✻ Worked for 2s · done 3:44 PM",
            "▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔",
            "   What do you want to do?",
            "",
            "   ❯ 1. Stop and wait for limit to reset",
            "     2. Wait here, then continue automatically at Oct 28, 9pm",
            "",
            "   Enter to confirm · Esc to cancel",
        ]);
        assert!(!snapshot_looks_idle(&usage_limit, 7));
        // Either half alone refuses.
        assert!(!snapshot_looks_idle(&lines(&["Proceed?", "❯ 2. No"]), 1));
        assert!(!snapshot_looks_idle(
            &lines(&["❯ ", "Enter to confirm · Esc to cancel"]),
            0
        ));
        // A dialog above a plain input prompt is still a dialog showing.
        assert!(!snapshot_looks_idle(
            &lines(&["❯ 1. Yes", "  2. No", "Esc to cancel", "❯ "]),
            3
        ));
        // Numbered transcript output above the input box is not a dialog, and
        // neither is a hint quoted in the transcript beyond the scan depth.
        assert!(snapshot_looks_idle(
            &lines(&["1. First, run the tests", "❯ "]),
            1
        ));
        let mut quoted = vec!["it said: Esc to cancel"];
        quoted.extend(["…"; DIALOG_SCAN_ROWS]);
        quoted.push("❯ ");
        assert!(snapshot_looks_idle(&lines(&quoted), 9));
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
