//! Usage-limit detection over the rendered terminal grid.
//!
//! A periodic poller ([`scan_grids_once`] / [`spawn_grid_scan_loop`]) reads each
//! live terminal's *rendered* VT grid (not its raw byte stream) and watches for
//! the Claude CLI's token-exhaustion messages ("Claude usage limit reached",
//! "5-hour limit reached ∙ resets …", …). When one appears it hands a
//! (debounced) hint to [`super::account_migration::handle_usage_limit_hint`].
//!
//! ## Why scan the grid, not the byte stream
//!
//! This watcher used to be an [`super::interceptor::OutputHook`] over a small
//! raw-byte rolling window. That misses messages painted by a full-screen TUI:
//! Claude Code's Ink renderer emits whole-frame *synchronized output* updates
//! (DEC `?2026h … ?2026l`), so the limit phrase can be batched and trimmed out
//! of a small byte window before a substring scan ever sees it. The VT-parsed
//! grid is the resolved on-screen text, immune to that batching — the same fix
//! the fleet auto-response matcher uses (`terminal::auto_response`).
//!
//! ## Why a HINT, not a verdict
//!
//! Conversation text can echo these phrases (a diff of this very file, a quoted
//! error in a log review…), and a TUI repaint after `claude --resume` can
//! re-render a *historical* limit message. The downstream handler re-probes the
//! account's real usage and only acts when the probe confirms exhaustion. This
//! module only has to be cheap, tolerant, and debounced.
//!
//! ## Firing policy
//!
//! Level-triggered with a per-terminal debounce ([`HINT_DEBOUNCE`], 300s): while
//! a limit message stays visible the hint re-fires at most once per debounce
//! window, so a persistently-painted error keeps the (probe-guarded) migration
//! path armed without hammering it. This preserves the original hook's effective
//! behavior (where TUI repaints re-fed the bytes under the same debounce).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{debug, info};

use super::output_scan::normalize;

/// Lowercased substrings that indicate the Claude CLI hit a usage limit.
///
/// Matched against a lowercased, whitespace-collapsed snapshot of the rendered
/// screen (the VT grid is already ANSI-resolved). Generous on purpose — a false
/// hint costs one
/// (rate-limited) usage probe downstream, while a missed hint strands a session
/// on a dead account.
const USAGE_LIMIT_PATTERNS: &[&str] = &[
    "usage limit reached",
    "5-hour limit reached",
    "weekly limit reached",
    "hit your usage limit",
    "out of extra usage",
    "session limit reached",
];

/// Minimum gap between two hint firings for the same terminal. A limit message
/// that stays painted (full-screen TUI) must not hammer the probe path.
const HINT_DEBOUNCE: Duration = Duration::from_secs(300);

// ── Pattern matching ────────────────────────────────────────────────────────

/// First matching usage-limit pattern in ALREADY-normalized screen text, if any.
fn normalized_indicates_usage_limit(normalized: &str) -> Option<&'static str> {
    USAGE_LIMIT_PATTERNS
        .iter()
        .find(|p| normalized.contains(*p))
        .copied()
}

/// First matching usage-limit pattern in a raw (un-normalized) screen/window.
/// Normalizes then delegates — the public matcher seam used by the unit tests.
pub fn window_indicates_usage_limit(window: &str) -> Option<&'static str> {
    normalized_indicates_usage_limit(&normalize(window))
}

// ── Firing policy (per-terminal debounce) ─────────────────────────────────────

/// Per-terminal last-hint-fired instant (`None` = never fired). The outer
/// `Option` makes the static const-initializable (mirrors the auto-response
/// edge map).
static FIRE_STATE: Mutex<Option<HashMap<String, Option<Instant>>>> = Mutex::new(None);

/// Pure firing decision: given the matched pattern (if any), the terminal's
/// last-fired time, and `now`, return the pattern iff a hint should fire and
/// update `last_fired`. Debounce-gated level trigger. The unit-test seam.
fn should_fire(
    matched: Option<&'static str>,
    last_fired: &mut Option<Instant>,
    now: Instant,
    debounce: Duration,
) -> Option<&'static str> {
    let pattern = matched?;
    if let Some(prev) = *last_fired {
        if now.duration_since(prev) < debounce {
            return None;
        }
    }
    *last_fired = Some(now);
    Some(pattern)
}

// ── Grid scanner ─────────────────────────────────────────────────────────────

/// One scan pass over every live terminal: read its rendered screen, and for
/// any terminal showing a usage-limit message (debounce-permitting) spawn the
/// account-migration hint. Cheap: one `text_snapshot` + substring scan per live
/// terminal per tick.
pub fn scan_grids_once() {
    use tauri::Manager;

    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    let sessions = tm.sessions_snapshot();

    // Decide under the fire-state lock, then dispatch with NO lock held (the
    // hint handler is async + re-locks its own state).
    let mut to_fire: Vec<(String, &'static str)> = Vec::new();
    {
        let mut state = FIRE_STATE.lock().unwrap_or_else(|e| e.into_inner());
        let map = state.get_or_insert_with(HashMap::new);
        let now = Instant::now();
        for (tid, session) in &sessions {
            // Read the *rendered* screen text (rows joined by `\n`). The grid is
            // the VT-parsed cell buffer, so this sees text inside a full-screen
            // TUI that synchronized-output batching hides from a byte scan.
            let text = {
                let grid = session.grid();
                let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
                guard.text_snapshot().text
            };
            let matched = normalized_indicates_usage_limit(&normalize(&text));
            // `entry` defaults to "never fired" → the first appearance fires
            // immediately; thereafter the debounce gates re-fires.
            let last_fired = map.entry(tid.clone()).or_insert(None);
            if let Some(pattern) = should_fire(matched, last_fired, now, HINT_DEBOUNCE) {
                to_fire.push((tid.clone(), pattern));
            }
        }
        // Drop state for terminals that have gone away.
        let live: HashSet<&String> = sessions.iter().map(|(t, _)| t).collect();
        map.retain(|tid, _| live.contains(tid));
    } // fire-state lock released

    for (tid, pattern) in to_fire {
        info!(
            terminal_id = %tid,
            pattern, "usage-limit message detected on terminal screen"
        );
        tauri::async_runtime::spawn(super::account_migration::handle_usage_limit_hint(
            tid, pattern,
        ));
    }
}

/// Grid-scan interval. Overridable via `QONTINUI_USAGE_LIMIT_SCAN_INTERVAL_MS`
/// (floored at 200ms). 1.5s is far under the 300s debounce, so a freshly-painted
/// limit message is caught promptly while costing a trivial substring pass per
/// terminal per tick.
fn scan_interval() -> Duration {
    let ms = std::env::var("QONTINUI_USAGE_LIMIT_SCAN_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n >= 200)
        .unwrap_or(1500);
    Duration::from_millis(ms)
}

/// Spawn the periodic usage-limit grid scanner for the process lifetime.
/// Detached; each tick is best-effort and the loop never exits.
pub fn spawn_grid_scan_loop() {
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(scan_interval());
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        debug!("usage_limit: grid-scan loop started");
        loop {
            ticker.tick().await;
            scan_grids_once();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_limit_message_matches() {
        assert_eq!(
            window_indicates_usage_limit(
                "Claude usage limit reached. Your limit will reset at 3am."
            ),
            Some("usage limit reached")
        );
        assert_eq!(
            window_indicates_usage_limit("5-hour limit reached ∙ resets 3am"),
            Some("5-hour limit reached")
        );
        assert_eq!(
            window_indicates_usage_limit("Weekly limit reached — resets Thursday"),
            Some("weekly limit reached")
        );
    }

    #[test]
    fn case_and_whitespace_tolerant() {
        assert_eq!(
            window_indicates_usage_limit("USAGE   LIMIT\n REACHED"),
            Some("usage limit reached")
        );
    }

    #[test]
    fn ordinary_output_does_not_match() {
        assert_eq!(
            window_indicates_usage_limit("cargo build finished in 12s"),
            None
        );
        assert_eq!(
            window_indicates_usage_limit("approaching usage limit"),
            None
        );
    }

    #[test]
    fn matches_across_wrapped_grid_rows() {
        // The grid is already VT-resolved (no escape codes), but a message can
        // be split across rows joined by `\n` and padded — normalize collapses
        // that whitespace so the phrase still matches.
        assert_eq!(
            window_indicates_usage_limit(
                "…banner…\n  Claude usage   limit\n  reached ∙ resets 3am"
            ),
            Some("usage limit reached")
        );
    }

    #[test]
    fn first_appearance_fires_immediately() {
        let now = Instant::now();
        let mut last = None;
        assert_eq!(
            should_fire(
                Some("usage limit reached"),
                &mut last,
                now,
                Duration::from_secs(300)
            ),
            Some("usage limit reached")
        );
        assert_eq!(last, Some(now));
    }

    #[test]
    fn no_match_never_fires() {
        let now = Instant::now();
        let mut last = None;
        assert_eq!(
            should_fire(None, &mut last, now, Duration::from_secs(300)),
            None
        );
        assert_eq!(last, None);
    }

    #[test]
    fn debounce_suppresses_repeat_within_window() {
        let t0 = Instant::now();
        let mut last = None;
        assert!(should_fire(
            Some("usage limit reached"),
            &mut last,
            t0,
            Duration::from_secs(300)
        )
        .is_some());
        // Same message still painted 100s later — within the 300s window.
        let t1 = t0 + Duration::from_secs(100);
        assert_eq!(
            should_fire(
                Some("usage limit reached"),
                &mut last,
                t1,
                Duration::from_secs(300)
            ),
            None
        );
        // Last-fired must NOT advance on a suppressed hint.
        assert_eq!(last, Some(t0));
    }

    #[test]
    fn debounce_refires_after_window() {
        let t0 = Instant::now();
        let mut last = None;
        assert!(should_fire(
            Some("usage limit reached"),
            &mut last,
            t0,
            Duration::from_secs(300)
        )
        .is_some());
        let t1 = t0 + Duration::from_secs(301);
        assert_eq!(
            should_fire(
                Some("usage limit reached"),
                &mut last,
                t1,
                Duration::from_secs(300)
            ),
            Some("usage limit reached")
        );
        assert_eq!(last, Some(t1));
    }
}
