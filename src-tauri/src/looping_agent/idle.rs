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
    idle_veto(lines, cursor_row).is_none()
}

/// Which of [`snapshot_looks_idle`]'s checks vetoes idle on this snapshot, as
/// a short human-readable reason; `None` exactly when the snapshot looks idle.
/// The same checks in the same order — the diagnostic for a pane that keeps
/// reading busy (see `restart_prep::notice`'s told-but-busy log line).
pub fn idle_veto(lines: &[String], cursor_row: u16) -> Option<String> {
    // (1) any processing-indicator text ⇒ busy.
    for (row, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        for ind in PROCESSING_INDICATORS {
            if lower.contains(ind) {
                return Some(format!("processing indicator `{ind}` on row {row}"));
            }
        }
    }

    // (2) spinner glyphs (excluding bare `·`) ⇒ busy — except on the static
    // completed-turn line Claude Code leaves under a finished turn.
    for (row, line) in lines.iter().enumerate() {
        if is_completed_turn_line(line) {
            continue;
        }
        if let Some(ch) = line
            .chars()
            .find(|ch| *ch != '·' && SPINNER_GLYPHS.contains(ch))
        {
            return Some(format!(
                "spinner glyph `{ch}` on row {row}: {}",
                line.trim()
            ));
        }
    }

    // (3) prompt marker visible — remember its row for (4).
    let Some(prompt_row) = lines.iter().position(|l| l.contains(PROMPT_MARKER)) else {
        // No visible input prompt ⇒ can't confirm ready-for-input ⇒ not idle.
        return Some("no `❯` prompt row on screen".to_string());
    };

    // (4) cursor in the input area (at/below the prompt row).
    ((cursor_row as usize) < prompt_row)
        .then(|| format!("cursor on row {cursor_row}, above the `❯` prompt row {prompt_row}"))
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

/// `9m`, `42s`, `1h`, or several glued together (`2m39s`).
fn is_duration_token(w: &str) -> bool {
    let mut saw_unit = false;
    let mut digits = 0usize;
    for c in w.chars() {
        if c.is_ascii_digit() {
            digits += 1;
        } else if matches!(c, 'h' | 'm' | 's') && digits > 0 {
            digits = 0;
            saw_unit = true;
        } else {
            return false;
        }
    }
    saw_unit && digits == 0
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
