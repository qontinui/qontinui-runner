//! Bounded deferred re-check of a usage-limit hint the probe did not confirm.
//!
//! [`super::account_migration::handle_usage_limit_hint`] migrates a session only
//! when a fresh usage probe agrees its account is exhausted. A session that lands
//! on a fresh account and soon hits the limit there can be checked before the
//! probe catches up. The hint is then skipped, and because the limit menu is a
//! frozen screen, the grid scanner ([`super::usage_limit`]) never looks at it
//! again: the session sits on the menu indefinitely.
//!
//! This module schedules up to [`REPROBE_DELAYS`]`.len()` later looks per
//! `(terminal, session)` inside [`REPROBE_WINDOW`], at most one pending at a
//! time. A look stands down unless the pane is **on the limit menu now**: its
//! bottom [`BOTTOM_ROWS`] non-blank rows hold a menu option row (a number, a
//! dot, then one of [`MENU_MARKERS`]) plus a second option row or the menu's
//! question, and not the spinner's `esc to interrupt`. The general limit
//! phrases are not enough, because the last lines of an idle pane's final
//! reply also sit in those rows. Only then does it call
//! `handle_usage_limit_hint` again, which re-runs the full probe confirm. So a
//! banner repainted by `--resume` higher up the screen, or limit text quoted in
//! conversation, does not schedule a look, and no look migrates anything the
//! probe does not confirm exhausted; this
//! module adds a later look, never a new reason to migrate.
//!
//! The live-state window here mirrors `usage_limit::bottom_rows_text` /
//! `screen_shows_turn` from open PR qontinui-runner#2417 in the same shape; once
//! that lands, replace [`bottom_rows_text`] and [`screen_shows_turn`] with those.
//! Plan: `2026-11-14-account-migration-herding-and-unconfirmed-limit-reprobe`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::info;

use super::output_scan::normalize;

/// Delay before each deferred look, in order. The first sits well past the
/// usage probe's 60 s cache TTL; the last bounds how long a stranded pane can
/// wait for the probe to agree.
pub(crate) const REPROBE_DELAYS: [Duration; 3] = [
    Duration::from_secs(3 * 60),
    Duration::from_secs(15 * 60),
    Duration::from_secs(45 * 60),
];

/// The window the [`REPROBE_DELAYS`] budget is counted over, per key. After it
/// passes with no further unconfirmed hint the key is forgotten, so a pane that
/// hits the limit again much later gets a fresh budget.
pub(crate) const REPROBE_WINDOW: Duration = Duration::from_secs(2 * 3600);

/// Non-blank rows from the bottom of the screen that count as what the CLI is
/// showing NOW (the shape of #2417's `usage_limit::BOTTOM_ROWS`).
pub(crate) const BOTTOM_ROWS: usize = 8;

/// The limit menu's own option rows. A pane counts as on the menu only when
/// one of these is in its bottom rows; matched with whitespace removed, because
/// the renderer can drop the spaces between words.
pub(crate) const MENU_MARKERS: [&str; 2] = [
    "stop and wait for limit to reset",
    "wait here, then continue automatically",
];

/// The pattern reported to the hint handler when the menu is showing but no
/// general limit phrase is in the bottom rows.
const MENU_PATTERN: &str = "stop and wait for limit to reset";

/// Per-key budget record.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// When the first deferred look in the current window was admitted.
    first_at: Instant,
    /// Looks admitted in the current window.
    admitted: usize,
    /// A look is scheduled and has not fired yet.
    pending: bool,
}

/// The pure core: decides whether an unconfirmed hint earns a deferred look.
#[derive(Debug, Default)]
pub(crate) struct ReprobeLedger {
    entries: HashMap<(String, String), Entry>,
}

impl ReprobeLedger {
    /// Admit one deferred look for `(terminal_id, session_id)` at `now`, or
    /// `None` when one is already pending or the window's budget is spent.
    /// Returns the delay to wait before the look.
    pub(crate) fn admit(
        &mut self,
        terminal_id: &str,
        session_id: &str,
        now: Instant,
    ) -> Option<Duration> {
        self.entries
            .retain(|_, e| e.pending || now.saturating_duration_since(e.first_at) < REPROBE_WINDOW);
        let key = (terminal_id.to_string(), session_id.to_string());
        let entry = self.entries.entry(key).or_insert(Entry {
            first_at: now,
            admitted: 0,
            pending: false,
        });
        if entry.pending || entry.admitted >= REPROBE_DELAYS.len() {
            return None;
        }
        let delay = REPROBE_DELAYS[entry.admitted];
        entry.admitted += 1;
        entry.pending = true;
        Some(delay)
    }

    /// The look for this key fired (or was abandoned): it is no longer pending.
    pub(crate) fn settle(&mut self, terminal_id: &str, session_id: &str) {
        if let Some(e) = self
            .entries
            .get_mut(&(terminal_id.to_string(), session_id.to_string()))
        {
            e.pending = false;
        }
    }
}

static LEDGER: Mutex<Option<ReprobeLedger>> = Mutex::new(None);

/// The bottom [`BOTTOM_ROWS`] non-blank rows of a rendered screen, in screen
/// order, joined with `\n`.
pub(crate) fn bottom_rows_text(screen: &str) -> String {
    let mut rows: Vec<&str> = screen
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(BOTTOM_ROWS)
        .collect();
    rows.reverse();
    rows.join("\n")
}

/// Whether the screen shows a turn in progress: the spinner's marker in its
/// bottom rows.
pub(crate) fn screen_shows_turn(screen: &str) -> bool {
    normalize(&bottom_rows_text(screen)).contains(super::account_migration::BUSY_MARKER)
}

/// Whether one rendered row is a limit-menu option row: after optional
/// whitespace and the `❯` cursor, a number, a dot, then one of
/// [`MENU_MARKERS`] (compared with whitespace removed and case folded). The
/// marker words anywhere else on a row, such as a reply quoting them, do not
/// count.
fn is_menu_option_row(row: &str) -> bool {
    let squashed: String = normalize(row)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let rest = squashed.strip_prefix('❯').unwrap_or(&squashed);
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    let Some(rest) = rest[digits..].strip_prefix('.') else {
        return false;
    };
    MENU_MARKERS.iter().any(|m| {
        let marker: String = m.chars().filter(|c| !c.is_whitespace()).collect();
        rest.starts_with(&marker)
    })
}

/// Whether one rendered row is the menu's question, "What do you want to do?".
/// Matched on its stem with whitespace removed, because the renderer can drop
/// characters ("What do you wan to do?").
fn is_menu_question_row(row: &str) -> bool {
    let squashed: String = normalize(row)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    squashed.starts_with("whatdoyouwan")
}

/// Whether the bottom rows hold the limit menu: one of its option rows, plus
/// either a second option row or its question row. One option-shaped row alone
/// could be an item in a reply's numbered list.
pub(crate) fn screen_shows_limit_menu(screen: &str) -> bool {
    let rows = bottom_rows_text(screen);
    let options = rows.lines().filter(|r| is_menu_option_row(r)).count();
    options >= 2 || (options == 1 && rows.lines().any(is_menu_question_row))
}

/// The usage-limit pattern the pane shows NOW, or `None`: the limit menu in
/// the bottom rows of a pane that is not mid-turn. Limit text without the menu
/// (a quote in the last reply) is never live.
pub(crate) fn live_limit_match(screen: &str) -> Option<&'static str> {
    if screen_shows_turn(screen) || !screen_shows_limit_menu(screen) {
        return None;
    }
    Some(
        super::usage_limit::window_indicates_usage_limit(&bottom_rows_text(screen))
            .unwrap_or(MENU_PATTERN),
    )
}

/// The rendered screen of `terminal_id`, or `None` when the terminal is gone.
fn read_screen(terminal_id: &str) -> Option<String> {
    use tauri::Manager;
    let app = crate::tauri_app_handle::current()?;
    let tm = app.try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()?;
    let session = tm.get(terminal_id)?;
    let grid = session.grid();
    let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
    Some(guard.text_snapshot().text)
}

/// Called by `handle_usage_limit_hint` when the probe did not confirm a hint.
/// Schedules a deferred look when the pane is on the limit menu now and the
/// key's budget allows; otherwise does nothing.
pub fn note_unconfirmed(terminal_id: &str, claude_session_id: &str) {
    let Some(screen) = read_screen(terminal_id) else {
        return;
    };
    if live_limit_match(&screen).is_none() {
        // A repainted or quoted banner, not a live menu: nothing to re-check.
        return;
    }
    let delay = {
        let mut ledger = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
        ledger.get_or_insert_with(ReprobeLedger::default).admit(
            terminal_id,
            claude_session_id,
            Instant::now(),
        )
    };
    let Some(delay) = delay else {
        info!(
            terminal_id,
            session = %claude_session_id,
            "unconfirmed usage-limit hint: deferred re-check budget spent or one already pending"
        );
        return;
    };
    info!(
        terminal_id,
        session = %claude_session_id,
        delay_secs = delay.as_secs(),
        "unconfirmed usage-limit hint on a live limit menu: deferred re-check scheduled"
    );
    let terminal_id = terminal_id.to_string();
    let session_id = claude_session_id.to_string();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        fire(terminal_id, session_id).await;
    });
}

/// One deferred look: settle the pending flag, re-read the pane, and re-enter
/// the hint handler only while the pane is still on the limit menu.
async fn fire(terminal_id: String, session_id: String) {
    {
        let mut ledger = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(l) = ledger.as_mut() {
            l.settle(&terminal_id, &session_id);
        }
    }
    let Some(screen) = read_screen(&terminal_id) else {
        info!(terminal_id, session = %session_id, "deferred re-check: terminal gone — standing down");
        return;
    };
    let Some(pattern) = live_limit_match(&screen) else {
        info!(terminal_id, session = %session_id, "deferred re-check: pane no longer on the limit menu — standing down");
        return;
    };
    info!(terminal_id, session = %session_id, pattern, "deferred re-check: pane still on the limit menu — re-probing");
    // The full confirm runs again inside; a still-unconfirmed probe calls
    // note_unconfirmed, which admits the next look while the budget lasts.
    super::account_migration::handle_usage_limit_hint(terminal_id, pattern).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admit_walks_the_delay_schedule_one_pending_look_at_a_time() {
        let mut l = ReprobeLedger::default();
        let t = Instant::now();
        assert_eq!(l.admit("t1", "s1", t), Some(REPROBE_DELAYS[0]));
        // One pending at a time.
        assert_eq!(l.admit("t1", "s1", t), None);
        l.settle("t1", "s1");
        assert_eq!(l.admit("t1", "s1", t), Some(REPROBE_DELAYS[1]));
        l.settle("t1", "s1");
        assert_eq!(l.admit("t1", "s1", t), Some(REPROBE_DELAYS[2]));
        l.settle("t1", "s1");
        // Budget spent inside the window.
        assert_eq!(l.admit("t1", "s1", t + Duration::from_secs(60)), None);
    }

    #[test]
    fn the_budget_resets_after_the_window_and_keys_are_independent() {
        let mut l = ReprobeLedger::default();
        let t = Instant::now();
        for _ in 0..REPROBE_DELAYS.len() {
            assert!(l.admit("t1", "s1", t).is_some());
            l.settle("t1", "s1");
        }
        assert_eq!(l.admit("t1", "s1", t), None);
        // Another terminal, or the same terminal under another session, has its
        // own budget.
        assert_eq!(l.admit("t2", "s1", t), Some(REPROBE_DELAYS[0]));
        assert_eq!(l.admit("t1", "s2", t), Some(REPROBE_DELAYS[0]));
        // After the window the key starts over.
        assert_eq!(
            l.admit("t1", "s1", t + REPROBE_WINDOW + Duration::from_secs(1)),
            Some(REPROBE_DELAYS[0])
        );
    }

    #[test]
    fn a_pending_key_is_never_pruned_by_the_window() {
        let mut l = ReprobeLedger::default();
        let t = Instant::now();
        assert!(l.admit("t1", "s1", t).is_some());
        // Still pending past the window: no second look is admitted.
        assert_eq!(
            l.admit("t1", "s1", t + REPROBE_WINDOW + Duration::from_secs(1)),
            None
        );
    }

    const MENU: &str = "\
earlier conversation line
⎿  You've hit your weekly limit · resets Nov 19, 9am
  What do you want to do?
  ❯ 1. Stop and wait for limit to reset
    2. Switch to extra usage
";

    #[test]
    fn a_limit_menu_in_the_bottom_rows_is_live() {
        assert!(live_limit_match(MENU).is_some());
    }

    #[test]
    fn a_banner_above_the_bottom_rows_is_not_live() {
        // A resume repaint: the old banner sits above a full screen of newer
        // output and an idle prompt.
        let mut screen = String::from("⎿  You've hit your weekly limit · resets Nov 19, 9am\n");
        for i in 0..BOTTOM_ROWS {
            screen.push_str(&format!("newer output line {i}\n"));
        }
        screen.push_str("❯ \n");
        assert!(live_limit_match(&screen).is_none());
    }

    #[test]
    fn a_pane_mid_turn_is_not_live_even_with_a_limit_phrase_below() {
        let screen = "\
quoting: \"You've hit your weekly limit\"
✻ Thinking… (12s · esc to interrupt)
";
        assert!(screen_shows_turn(screen));
        assert!(live_limit_match(screen).is_none());
    }

    #[test]
    fn quoted_limit_text_in_an_idle_pane_is_not_live() {
        let screen = "\
I stopped because the coord quota was limit reached.
You've hit your weekly limit · resets Nov 19, 9am was the message.
❯ 
  ? for shortcuts
";
        assert!(live_limit_match(screen).is_none());
    }

    #[test]
    fn a_menu_drawn_without_spaces_is_live() {
        let screen =
            "  ❯ 1. Stopandwaitforlimittoreset\n    2. Waithere,thencontinueautomaticallyat9pm\n";
        assert!(screen_shows_limit_menu(screen));
        assert!(live_limit_match(screen).is_some());
    }

    #[test]
    fn a_reply_quoting_a_menu_option_is_not_live() {
        let screen = "\
I added \"Stop and wait for limit to reset\" to MENU_MARKERS.
Also matched: 2. Switch to extra usage
❯ 
";
        assert!(!screen_shows_limit_menu(screen));
        assert!(live_limit_match(screen).is_none());
    }

    #[test]
    fn one_option_shaped_list_item_in_a_reply_is_not_live() {
        let screen = "\
Changes:
1. Stop and wait for limit to reset is now in MENU_MARKERS
❯ 
";
        assert!(!screen_shows_limit_menu(screen));
    }

    #[test]
    fn a_narrow_menu_with_one_option_and_its_question_is_live() {
        let screen = "  What do you wan to do?\n  ❯ 1. Stop and wait for limit to reset\n";
        assert!(screen_shows_limit_menu(screen));
    }

    #[test]
    fn bottom_rows_skip_blank_rows() {
        let screen = "a\n\n   \nb\n\n";
        assert_eq!(bottom_rows_text(screen), "a\nb");
    }
}
