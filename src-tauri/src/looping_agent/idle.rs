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

/// The one [`PROCESSING_INDICATORS`] entry Claude Code's idle context hint
/// carries: a finished pane's footer reads `new task? /clear to save 399.4k
/// tokens · …` (often followed by `Auto-updating…` or `✔ Update installed ·
/// Restart to apply`), which is not a working line.
const TOKENS_INDICATOR: &str = "tokens ·";

/// The context hint's own text (lowercased). A line carrying it is exempt from
/// [`TOKENS_INDICATOR`] ONLY — every other indicator on the same line still
/// vetoes idle, so a working footer (`↑ 1.2k tokens · esc to interrupt`) is
/// unaffected. Mirrors `FOOTER_CHURN`'s `new task?\s*/clear to save` in
/// qontinui-claude-config `scripts/close-finished-sessions.py`.
const CLEAR_HINT: &str = "/clear to save";

/// The completed-turn line Claude Code leaves above the prompt when a turn
/// ends: `✻ Cooked for 5m 33s · done 8:55 PM`. The WHOLE line must match — a
/// spinner glyph, ONE word, ` for `, a duration of hours/minutes/seconds in
/// that order (`5m 33s`, `2s`, `1h 2m`), optionally `· done <h:mm AM/PM>` — or
/// the glyph keeps counting as busy. Fail closed: a wording Claude Code
/// changes reads busy, which is the behaviour before this exemption, never a
/// new idle.
///
/// Ports `TURN_END` from qontinui-claude-config
/// `scripts/close-finished-sessions.py` so the two recognizers agree. This one
/// is the stricter: it anchors both ends of the line (the script scans a byte
/// stream, so it cannot), and it admits an hours-and-minutes duration with no
/// seconds part (`1h 2m`). The glyph class is [`SPINNER_GLYPHS`] minus `·`,
/// the exact set the glyph clause vetoes on.
static COMPLETED_TURN_LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(concat!(
        r"(?i)^\s*[✻✶✳✽✢∗]\s*\w+\s+for\s+",
        r"(?:\d+h(?:\s+\d+m)?(?:\s+\d+s)?|\d+m(?:\s+\d+s)?|\d+s)",
        r"(?:\s*·\s*done\s+\d{1,2}:\d{2}\s*[AP]M)?\s*$",
    ))
    .expect("COMPLETED_TURN_LINE compiles")
});

/// Is `line` exactly a completed-turn line (`COMPLETED_TURN_LINE`)?
pub fn is_completed_turn_line(line: &str) -> bool {
    COMPLETED_TURN_LINE.is_match(line)
}

/// Decide whether a single rendered grid snapshot looks IDLE / ready for
/// input. Pure over the snapshot. CONSERVATIVE: returns `false` (NOT idle)
/// on any ambiguity. Idle requires ALL of:
///
/// 1. NO processing-indicator text anywhere on screen
///    ([`PROCESSING_INDICATORS`]) — except `tokens ·` on a line carrying the
///    `/clear to save` context hint (`CLEAR_HINT`).
/// 2. NO spinner glyph on screen ([`SPINNER_GLYPHS`], `·` excepted) — except
///    on a line that is exactly a completed-turn line
///    ([`is_completed_turn_line`]).
/// 3. A prompt row containing [`PROMPT_MARKER`] (`❯`) is visible.
/// 4. The cursor sits AT OR BELOW the prompt row (in the input area), not up
///    in streaming output.
///
/// The two exceptions exist because a FINISHED idle pane routinely shows both
/// shapes, and without them it read busy forever (plan
/// `2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-for-a-spinner`).
/// There is NO draft or dialog check here: `terminal::graceful_exit`'s
/// `prompt_ready_with` refuses those before anything is typed.
pub fn snapshot_looks_idle(lines: &[String], cursor_row: u16) -> bool {
    // (1) any processing-indicator text ⇒ busy.
    for line in lines {
        let lower = line.to_ascii_lowercase();
        for ind in PROCESSING_INDICATORS {
            if *ind == TOKENS_INDICATOR && lower.contains(CLEAR_HINT) {
                continue;
            }
            if lower.contains(ind) {
                return false;
            }
        }
    }

    // (2) spinner glyphs (excluding bare `·`) ⇒ busy, unless the whole line
    // is a completed-turn line.
    for line in lines {
        let has_glyph = line
            .chars()
            .any(|ch| ch != '·' && SPINNER_GLYPHS.contains(&ch));
        if has_glyph && !is_completed_turn_line(line) {
            return false;
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
