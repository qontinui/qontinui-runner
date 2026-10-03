//! Usage-limit detection over the rendered terminal grid.
//!
//! A periodic poller ([`scan_grids_once`] / [`spawn_grid_scan_loop`]) reads each
//! live AI session's *rendered* VT grid (not its raw byte stream) and watches
//! for the usage-limit phrases its CLI's profile declares
//! (`CliProfile::usage_limit_phrases` — for Claude "usage limit reached",
//! "5-hour limit reached ∙ resets …", …), keyed by the provider the terminal's
//! lifecycle record names. When one appears it REPORTS a (debounced)
//! `FailureSignal::GridPhrase` to `session::failure_recovery`, and does nothing
//! else: the recovery table decides what a `quota_exhausted` hint means (today
//! the confirm-then-migrate path in
//! [`super::account_migration::handle_usage_limit_hint`]). This scanner holds
//! no phrase list of its own and takes no action of its own (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 7), so a limit is never handled twice.
//!
//! A terminal with no lifecycle record, or whose recorded provider has no
//! profile, is not scanned: it has no declared phrases, and the migration path
//! could not act on it anyway (it needs the record's transcript binding).
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

use tracing::{debug, info, warn};

use super::output_scan::normalize;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// Minimum gap between two hint firings for the same terminal. A limit message
/// that stays painted (full-screen TUI) must not hammer the probe path.
const HINT_DEBOUNCE: Duration = Duration::from_secs(300);

// ── Pattern matching ────────────────────────────────────────────────────────

/// First of `phrases` (a profile's `usage_limit_phrases`, already lowercase,
/// in the profile's order — specific phrasings first, so they win the label)
/// found in ALREADY-normalized screen text.
///
/// The match is generous on purpose: a false hint costs one probe-confirmed
/// no-op downstream, while a missed hint strands a session on a dead account.
fn normalized_indicates_usage_limit<'a>(normalized: &str, phrases: &'a [String]) -> Option<&'a str> {
    phrases
        .iter()
        .map(String::as_str)
        .find(|p| normalized.contains(p))
}

/// [`normalized_indicates_usage_limit`] over a raw (un-normalized) screen.
pub fn window_indicates_usage_limit<'a>(window: &str, phrases: &'a [String]) -> Option<&'a str> {
    normalized_indicates_usage_limit(&normalize(window), phrases)
}

// ── Firing policy (per-terminal debounce) ─────────────────────────────────────

/// Per-terminal last-hint-fired instant (`None` = never fired). The outer
/// `Option` makes the static const-initializable (mirrors the auto-response
/// edge map).
static FIRE_STATE: Mutex<Option<HashMap<String, Option<Instant>>>> = Mutex::new(None);

/// Per-terminal output-byte watermarks so a tick skips sessions whose rendered
/// screen cannot have changed since the previous pass (see
/// [`super::scan_gate`]). This scanner keeps its OWN gate — sharing one with
/// `auto_response` would let whichever tick ran first consume the change on the
/// other's behalf.
static SCAN_GATE: Mutex<Option<super::scan_gate::ScanGate>> = Mutex::new(None);

/// Pure firing decision: given the matched pattern (if any), the terminal's
/// last-fired time, and `now`, return the pattern iff a hint should fire and
/// update `last_fired`. Debounce-gated level trigger. The unit-test seam.
fn should_fire<'a>(
    matched: Option<&'a str>,
    last_fired: &mut Option<Instant>,
    now: Instant,
    debounce: Duration,
) -> Option<&'a str> {
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
/// any AI session showing one of its profile's usage-limit phrases
/// (debounce-permitting) report a grid-phrase failure signal. Cheap: one
/// `text_snapshot` + substring scan per changed AI-session terminal per tick.
pub fn scan_grids_once() {
    use crate::session::failure::FailureSignal;
    use crate::session::failure_recovery::{self, RecoveryTarget};
    use crate::session::session_lifecycle_store::SessionLifecycleStore;
    use tauri::Manager;

    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    let Some(store) = app.try_state::<Arc<SessionLifecycleStore>>() else {
        return;
    };
    let sessions = tm.sessions_snapshot();

    // Decide under the fire-state lock, then report with NO lock held (the
    // recovery path re-locks its own state).
    let mut to_fire: Vec<(
        crate::session::session_lifecycle_store::TerminalSessionRecord,
        &'static str,
    )> = Vec::new();
    {
        let mut state = FIRE_STATE.lock().unwrap_or_else(|e| e.into_inner());
        let map = state.get_or_insert_with(HashMap::new);
        let mut gate_guard = SCAN_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let gate = gate_guard.get_or_insert_with(super::scan_gate::ScanGate::new);
        let now = Instant::now();
        for (tid, session) in &sessions {
            // Skip sessions whose grid has not been mutated since the last
            // pass — the rendered screen is byte-identical, so a limit message
            // frozen on an idle terminal is not re-detected. One atomic load
            // instead of a grid lock + full screen render.
            if !gate.should_scan(tid, session.grid_generation()) {
                continue;
            }
            // The phrases this terminal's CLI declares, from the provider its
            // lifecycle record names. No record or no profile ⇒ nothing
            // declared to look for.
            let Some(record) = store.find_open_by_terminal(tid) else {
                continue;
            };
            let Some(profile) = qontinui_runner_lib::cli_profile::profile_for(&record.provider)
            else {
                continue;
            };
            // Read the *rendered* screen text (rows joined by `\n`). The grid is
            // the VT-parsed cell buffer, so this sees text inside a full-screen
            // TUI that synchronized-output batching hides from a byte scan.
            let text = {
                let grid = session.grid();
                let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
                guard.text_snapshot().text
            };
            let matched =
                normalized_indicates_usage_limit(&normalize(&text), &profile.usage_limit_phrases);
            // `entry` defaults to "never fired" → the first appearance fires
            // immediately; thereafter the debounce gates re-fires.
            let last_fired = map.entry(tid.clone()).or_insert(None);
            if let Some(pattern) = should_fire(matched, last_fired, now, HINT_DEBOUNCE) {
                to_fire.push((record, pattern));
            }
        }
        // Drop state for terminals that have gone away.
        let live: HashSet<&String> = sessions.iter().map(|(t, _)| t).collect();
        map.retain(|tid, _| live.contains(tid));
        gate.retain_live(&live);
    } // fire-state + scan-gate locks released

    // A failure recorded against a terminal that has gone away described a
    // session that is gone too. Outside the locks: it announces the clears.
    failure_recovery::retain_live_terminals(&sessions.iter().map(|(t, _)| t).collect());

    for (record, pattern) in to_fire {
        info!(
            terminal_id = %record.terminal_id,
            provider = %record.provider,
            pattern, "usage-limit message detected on terminal screen"
        );
        let provider = record.provider.clone();
        let account = record.config_dir.clone();
        failure_recovery::report(
            RecoveryTarget::Pty {
                terminal_id: record.terminal_id.clone(),
                record: Some(Box::new(record)),
            },
            &provider,
            account,
            FailureSignal::GridPhrase {
                provider: provider.clone(),
                phrase: pattern.to_string(),
            },
        );
    }
}

/// Grid-scan interval. Resolved from `QONTINUI_USAGE_LIMIT_SCAN_INTERVAL_MS`
/// (the higher-precedence escape hatch), else
/// `settings.performance.grid_scan_interval_ms`, else the historical 1500 ms —
/// that setting's default. Floored at 200 ms either way. 1.5s is far under the
/// 300s debounce, so a freshly-painted limit message is caught promptly while
/// costing a trivial substring pass per terminal per tick. See
/// `terminal::scan_interval` for the precedence rules.
fn scan_interval() -> Duration {
    crate::terminal::scan_interval::scan_interval_from_env("QONTINUI_USAGE_LIMIT_SCAN_INTERVAL_MS")
}

/// Spawn the periodic usage-limit grid scanner for the process lifetime.
/// Detached; each tick is best-effort and the loop never exits.
pub fn spawn_grid_scan_loop() {
    // Supervised on Tauri's runtime (plan 2026-09-03-…-supervisor Phase 4).
    crate::worker_supervisor::spawn_supervised_on_tauri_with_heartbeat(
        "terminal.usage_limit.grid_scan",
        move |hb| async move {
            let mut ticker = tokio::time::interval(scan_interval());
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            debug!("usage_limit: grid-scan loop started");
            loop {
                ticker.tick().await;
                hb.tick();
                // Locking every session's grid and rendering a full screen is CPU
                // + lock work, not I/O — keep it off the runtime's worker pool
                // (same shape as `build_drift::run_periodic`). A panicked sweep is
                // logged and the loop keeps ticking.
                if let Err(e) = spawn_blocking_tracked(scan_grids_once).await {
                    warn!(error = %e, "usage_limit: grid-scan task panicked");
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Claude profile's declared phrases — the list a Claude session's
    /// grid is scanned for.
    fn phrases() -> &'static [String] {
        &qontinui_runner_lib::cli_profile::profile_for(
            qontinui_runner_lib::cli_profile::claude::ID,
        )
        .unwrap()
        .usage_limit_phrases
    }

    fn window_indicates_usage_limit(window: &str) -> Option<&'static str> {
        super::window_indicates_usage_limit(window, phrases())
    }

    /// A provider that declares no phrases is never matched: the scanner has
    /// no list of its own to fall back to.
    #[test]
    fn a_profile_with_no_phrases_matches_nothing() {
        assert_eq!(
            super::window_indicates_usage_limit("Claude usage limit reached", &[]),
            None
        );
    }

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
