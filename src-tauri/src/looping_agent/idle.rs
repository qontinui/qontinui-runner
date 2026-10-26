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

/// Decide whether a single rendered grid snapshot looks IDLE / ready for
/// input. Pure over the snapshot. CONSERVATIVE: returns `false` (NOT idle)
/// on any ambiguity. Idle requires ALL of:
///
/// 1. NO processing-indicator text anywhere on screen
///    ([`PROCESSING_INDICATORS`]).
/// 2. NO spinner glyph on screen ([`SPINNER_GLYPHS`], `·` excepted), save on
///    the static completed-turn line ([`is_completed_turn_line`]).
/// 3. A prompt row containing [`PROMPT_MARKER`] (`❯`) is visible.
/// 4. The cursor sits AT OR BELOW the prompt row (in the input area), not up
///    in streaming output.
pub fn snapshot_looks_idle(lines: &[String], cursor_row: u16) -> bool {
    // (1) any processing-indicator text ⇒ busy.
    for line in lines {
        let lower = line.to_ascii_lowercase();
        for ind in PROCESSING_INDICATORS {
            if lower.contains(ind) {
                return false;
            }
        }
    }

    // (2) spinner glyphs (excluding bare `·`) ⇒ busy — except on the static
    // completed-turn line Claude Code leaves under a finished turn.
    for line in lines {
        if is_completed_turn_line(line) {
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
    (cursor_row as usize) >= prompt_row
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
        let mut saw_time = false;
        for w in words {
            let is_clock = w.contains(':') && w.chars().all(|c| c.is_ascii_digit() || c == ':');
            let is_meridiem = matches!(w.to_ascii_uppercase().as_str(), "AM" | "PM");
            if is_clock {
                saw_time = true;
            } else if !is_meridiem {
                return false;
            }
        }
        if !saw_time {
            return false;
        }
    }
    let mut words = head.split_whitespace();
    let verb_ok = words
        .next()
        .is_some_and(|v| !v.is_empty() && v.chars().all(char::is_alphabetic));
    if !verb_ok || words.next() != Some("for") {
        return false;
    }
    let mut saw_duration = false;
    for w in words {
        let Some(unit) = w.chars().last() else {
            return false;
        };
        let digits = w.strip_suffix(unit).unwrap_or_default();
        if !matches!(unit, 'h' | 'm' | 's')
            || digits.is_empty()
            || !digits.chars().all(|c| c.is_ascii_digit())
        {
            return false;
        }
        saw_duration = true;
    }
    saw_duration
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
