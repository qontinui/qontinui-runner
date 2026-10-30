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

/// Footer hints that only appear under a Claude Code modal dialog — the
/// workspace-trust prompt above all. Lowercased before match, and only looked
/// for BELOW the selection row (see [`snapshot_shows_dialog`]). A dialog is not
/// the input line: anything typed into it answers the dialog.
///
/// Measured 2026-10-28 (plan
/// `2026-10-28-merge-shepherd-slot-is-won-by-any-runner-regardless-of-role-or-load`
/// D6): a Merge Shepherd tab launched in an untrusted directory sat on the trust
/// prompt; five minutes later the supervisor's cycle nudge typed its text plus
/// Enter into it, which answered the dialog with "No, exit", and Claude Code
/// exited. The prompt's selection cursor is the same `❯` glyph as the input
/// marker, which is how the dialog passed for an idle input line.
pub const DIALOG_INDICATORS: &[&str] = &["enter to confirm"];

/// How many rows BELOW the selected option a dialog's footer hint
/// ([`DIALOG_INDICATORS`]) may sit. Below the last `❯` row an idle screen shows
/// only the input box's status lines, never transcript output, so matching
/// there cannot be tripped by an agent quoting the hint in its own output.
const DIALOG_FOOTER_ROWS: usize = 8;

/// How many rows above the anchor a dialog's selected choice may sit when the
/// input line is parked below it. Also `terminal::graceful_exit`'s dialog scan
/// depth (`DIALOG_SCAN_ROWS` is defined as this), so the two scans cannot drift.
pub const DIALOG_MENU_ROWS: usize = 8;

/// Whether the rendered screen shows a Claude Code selection dialog rather
/// than the input line.
///
/// Anchored on the LAST row carrying [`PROMPT_MARKER`], the row nearest the
/// input area. The screen is a dialog when a numbered menu option
/// (`❯ 1. Yes, proceed`, with or without a box border before it) sits ON the
/// anchor, or ABOVE it when the cursor is parked on a plain `❯` row under the
/// choices. That second region starts at the input box's TOP edge
/// ([`is_box_top_edge`]) when one sits within 2 rows above the anchor, so
/// nothing above an idle input box is
/// read and an agent's own output can quote a dialog without latching the
/// screen. With no box edge, the region is [`DIALOG_MENU_ROWS`] rows. This is
/// the same region rule as `terminal::graceful_exit`'s `option_first`. A
/// [`DIALOG_INDICATORS`] footer hint within [`DIALOG_FOOTER_ROWS`] rows below
/// the anchor is a dialog too: below the input line an idle screen shows only
/// status lines, never transcript output (the slash menu, too, opens above
/// the input box).
pub fn snapshot_shows_dialog(lines: &[String]) -> bool {
    let Some(anchor) = lines.iter().rposition(|l| l.contains(PROMPT_MARKER)) else {
        return false;
    };
    let box_edge = (anchor.saturating_sub(2)..anchor)
        .rev()
        .find(|&r| lines.get(r).is_some_and(|l| is_box_top_edge(l)));
    let first = box_edge.unwrap_or_else(|| anchor.saturating_sub(DIALOG_MENU_ROWS));
    if lines
        .iter()
        .skip(first)
        .take(anchor + 1 - first)
        .any(|l| is_menu_cursor_row(l))
    {
        return true;
    }
    lines
        .iter()
        .skip(anchor)
        .take(DIALOG_FOOTER_ROWS + 1)
        .any(|line| {
            let lower = line.to_ascii_lowercase();
            DIALOG_INDICATORS.iter().any(|ind| lower.contains(ind))
        })
}

/// A row drawn only of box-drawing characters — an input box's top or bottom
/// edge. The one copy: `terminal::graceful_exit` uses it too.
pub fn is_box_edge(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| "─━╭╮╰╯┌┐└┘│".contains(c))
}

/// A box edge that can only be a box's TOP: it carries a top corner (`╭`/`┌`)
/// or is a plain horizontal rule. A bottom border (`╰…╯`) under a dialog is not
/// the input box's top edge, so it must not cut the dialog's menu out of the
/// region [`snapshot_shows_dialog`] scans.
fn is_box_top_edge(line: &str) -> bool {
    let t = line.trim();
    is_box_edge(line)
        && (t.contains('╭') || t.contains('┌') || t.chars().all(|c| c == '─' || c == '━'))
}

/// `❯` followed by optional spaces, one or more ASCII digits and a `.` — the
/// cursor on a numbered menu option.
fn is_menu_cursor_row(line: &str) -> bool {
    line.match_indices(PROMPT_MARKER).any(|(idx, marker)| {
        let rest = line.get(idx + marker.len()..).unwrap_or("").trim_start();
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && rest.get(digits..).is_some_and(|tail| tail.starts_with('.'))
    })
}

/// Decide whether a single rendered grid snapshot looks IDLE / ready for
/// input. Pure over the snapshot. CONSERVATIVE: returns `false` (NOT idle)
/// on any ambiguity. Idle requires ALL of:
///
/// 1. NO processing-indicator text anywhere on screen
///    ([`PROCESSING_INDICATORS`]).
/// 2. NO spinner glyph on screen ([`SPINNER_GLYPHS`], `·` excepted).
/// 3. A prompt row containing [`PROMPT_MARKER`] (`❯`) is visible.
/// 4. The cursor sits AT OR BELOW the prompt row (in the input area), not up
///    in streaming output.
/// 5. NO modal dialog on screen ([`snapshot_shows_dialog`]): a dialog's menu
///    cursor is a `❯` too, and typing into it answers the dialog.
pub fn snapshot_looks_idle(lines: &[String], cursor_row: u16) -> bool {
    // (5) a dialog is never the input line, whatever else the screen shows.
    if snapshot_shows_dialog(lines) {
        return false;
    }

    // (1) any processing-indicator text ⇒ busy.
    for line in lines {
        let lower = line.to_ascii_lowercase();
        for ind in PROCESSING_INDICATORS {
            if lower.contains(ind) {
                return false;
            }
        }
    }

    // (2) spinner glyphs (excluding bare `·`) ⇒ busy.
    for line in lines {
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
    (cursor_row as usize) >= prompt_row
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

    /// The workspace-trust prompt as Claude Code renders it: a bordered box, a
    /// numbered menu whose cursor is the same `❯` glyph as the input marker,
    /// and the cursor parked on the menu row.
    fn trust_dialog() -> Vec<String> {
        lines(&[
            "╭──────────────────────────────────────────────────────────╮",
            "│ Do you trust the files in this folder?                   │",
            "│                                                          │",
            "│ /home/u/.qontinui/runner/looping-agents/merge-shepherd   │",
            "│                                                          │",
            "│ ❯ 1. Yes, proceed                                        │",
            "│   2. No, exit                                            │",
            "│                                                          │",
            "│ Enter to confirm · Esc to exit                           │",
            "╰──────────────────────────────────────────────────────────╯",
        ])
    }

    #[test]
    fn the_trust_dialog_is_never_idle_even_with_the_cursor_on_its_menu() {
        let grid = trust_dialog();
        assert!(snapshot_shows_dialog(&grid));
        for cursor_row in 0..grid.len() as u16 {
            assert!(
                !snapshot_looks_idle(&grid, cursor_row),
                "a dialog must never read as an idle input line (cursor row {cursor_row})"
            );
        }
    }

    #[test]
    fn the_newer_trust_prompt_wording_is_a_dialog_too() {
        let grid = lines(&[
            " Is this a project you created or one you trust?",
            " ❯ 1. Yes, I trust this folder",
            "   2. No, exit",
        ]);
        assert!(snapshot_shows_dialog(&grid));
        assert!(!snapshot_looks_idle(&grid, 1));
    }

    #[test]
    fn a_bare_numbered_menu_cursor_is_a_dialog_without_any_known_phrase() {
        // Any `❯ <n>.` selection row — not only the trust prompt.
        let grid = lines(&["Pick one", "❯2. Continue", "  3. Stop"]);
        assert!(snapshot_shows_dialog(&grid));
        assert!(!snapshot_looks_idle(&grid, 1));
    }

    #[test]
    fn the_ordinary_input_line_is_not_a_dialog() {
        // The idle input box: `❯` with nothing (or free text) after it.
        for row in [
            "❯ ",
            "│ ❯                    │",
            "❯ run your next cycle",
            "❯ 12 PRs open",
        ] {
            let grid = lines(&["done.", row]);
            assert!(
                !snapshot_shows_dialog(&grid),
                "{row:?} is the input line, not a menu"
            );
        }
        assert!(snapshot_looks_idle(&lines(&["done.", "❯ "]), 1));
    }

    #[test]
    fn a_finished_turn_that_quotes_a_dialog_above_the_input_line_stays_idle() {
        // R2: an idle screen does not repaint, so a dialog phrase or a quoted
        // `❯ 1.` row in the agent's own output above the input box must not
        // latch the screen as a dialog (it would never be nudged again).
        let grid = lines(&[
            "The Shepherd saw this on spaceship's tab:",
            "  Do you trust the files in this folder?",
            "  ❯ 1. Yes, proceed",
            "    2. No, exit",
            "  Enter to confirm · Esc to exit",
            "Cycle summary: 3 PRs checked, 0 actions.",
            "╭──────────────────────────────╮",
            "│ ❯                            │",
            "╰──────────────────────────────╯",
            "  ? for shortcuts",
        ]);
        assert!(
            !snapshot_shows_dialog(&grid),
            "quoted text above the input line is not a dialog"
        );
        assert!(snapshot_looks_idle(&grid, 7), "the input line is idle");
    }

    #[test]
    fn a_dialog_with_the_cursor_parked_on_a_plain_prompt_row_below_it_is_a_dialog() {
        // R2a: graceful_exit's own `parked` shape — the permission dialog with
        // a plain `❯` row pushed below its choices and the cursor parked there.
        let grid = lines(&[
            " Bash command",
            "   rm -rf build",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don't ask again for rm commands in this project",
            "   3. No, and tell Claude what to do differently (esc)",
            "❯ ",
        ]);
        assert!(snapshot_shows_dialog(&grid));
        assert!(
            !snapshot_looks_idle(&grid, 6),
            "never nudge into a parked dialog"
        );
    }

    #[test]
    fn a_bordered_dialog_with_a_plain_prompt_row_under_its_bottom_border_is_a_dialog() {
        // R2b: the bottom border is not the input box's top edge, so the menu
        // above it must still be scanned.
        let mut grid = trust_dialog();
        grid.push("❯ ".to_string());
        let cursor = (grid.len() - 1) as u16;
        assert!(snapshot_shows_dialog(&grid));
        assert!(
            !snapshot_looks_idle(&grid, cursor),
            "never nudge into a parked dialog"
        );
    }
}
