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

use tracing::{debug, info, warn};

use super::output_scan::normalize;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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
    // Claude Code's current banner: "You've hit your weekly limit · resets
    // Oct 21, 10pm". None of the "… limit reached" spellings match it.
    "hit your weekly limit",
    "hit your 5-hour limit",
    "hit your session limit",
    "out of extra usage",
    "session limit reached",
    // Generic catch-alls LAST (specific patterns above win the label). The
    // CLI's phrasing shifts across releases ("Opus weekly limit reached",
    // "limit reached ∙ resets 3am", "You're out of usage until…") — a missed
    // hint strands a session on a dead account, while a false hint costs one
    // probe-confirmed no-op.
    "limit reached",
    "out of usage",
    "hit your limit",
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

/// Per-terminal output-byte watermarks so a tick skips sessions whose rendered
/// screen cannot have changed since the previous pass (see
/// [`super::scan_gate`]). This scanner keeps its OWN gate — sharing one with
/// `auto_response` would let whichever tick ran first consume the change on the
/// other's behalf.
static SCAN_GATE: Mutex<Option<super::scan_gate::ScanGate>> = Mutex::new(None);

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

// ── Stale limit text on a hand-restored limit_hit session ────────────────────

/// A restart-cohort row that hit its account's usage limit during the prepare
/// (`limit_hit`) and that the operator restored BY HAND under the same,
/// still-exhausted account (plan
/// `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, D3: a
/// restore never re-targets an account; the operator clicks "Account … resets
/// at …" anyway). `claude --resume` repaints the conversation, including the
/// old "usage limit reached" line, so without this guard the scanner fired a
/// hint ~2 s after the resume, the probe (rightly) confirmed the account was
/// still exhausted, and the session was migrated to another account — undoing
/// exactly the choice the operator had just made.
///
/// **Design — suppressed for as long as the STALE text is what is on screen,
/// never by a blind timer alone.** Per terminal, while the guard is armed:
///
/// * the screen shows limit text → the hint is suppressed (nothing probed,
///   nothing migrated, the debounce state untouched so a later fresh hint
///   fires at once);
/// * the screen has painted its input prompt WITHOUT any limit text, at least
///   [`STALE_LIMIT_SETTLE_MS`] after the restore (the resume repaint is done)
///   → the guard lifts: any limit text that appears from then on is new
///   output, and the ordinary probe-guarded migration applies to it;
/// * the account's recorded reset time passes (or, with no recorded reset,
///   [`STALE_LIMIT_MAX_MS`] — the clamp the exhausted-account cooldown uses)
///   → the guard lifts: past the reset, a confirmed exhaustion is a new one.
///
/// A blank or still-repainting screen keeps the guard (it is neither stale
/// text nor a settled clean screen). Keyed by terminal id, dropped with the
/// terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StaleLimitGuard {
    pub armed_at_ms: i64,
    pub until_ms: i64,
}

/// How long after the restore a clean, prompt-painted screen is trusted to
/// mean the resume repaint has finished (so the guard may lift on it).
pub(crate) const STALE_LIMIT_SETTLE_MS: i64 = 15_000;
/// The longest a guard holds when the account's reset time is unknown (or
/// further away): six hours, the exhausted-account cooldown's own clamp.
pub(crate) const STALE_LIMIT_MAX_MS: i64 = 6 * 3600 * 1000;

/// What one scan does with an armed guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuardStep {
    /// Limit text on screen: do not fire a hint.
    Suppress,
    /// The guard ends; the scan proceeds as for any terminal.
    Lift,
    /// Nothing to fire, guard stays (screen still painting).
    Keep,
}

impl StaleLimitGuard {
    /// The guard for a restore at `now_ms` of a row whose account resets at
    /// `limit_reset_at_ms`. `None` when that reset has already passed — the
    /// probe then no longer confirms the stale text, so nothing would migrate.
    pub(crate) fn arm(limit_reset_at_ms: Option<i64>, now_ms: i64) -> Option<Self> {
        let cap = now_ms.saturating_add(STALE_LIMIT_MAX_MS);
        let until_ms = match limit_reset_at_ms {
            Some(reset) if reset <= now_ms => return None,
            Some(reset) => reset.min(cap),
            None => cap,
        };
        Some(Self {
            armed_at_ms: now_ms,
            until_ms,
        })
    }

    /// PURE: the step for one scan of the guarded terminal's screen.
    pub(crate) fn step(&self, limit_text: bool, prompt_painted: bool, now_ms: i64) -> GuardStep {
        if now_ms >= self.until_ms {
            return GuardStep::Lift;
        }
        if limit_text {
            return GuardStep::Suppress;
        }
        if prompt_painted && now_ms >= self.armed_at_ms.saturating_add(STALE_LIMIT_SETTLE_MS) {
            return GuardStep::Lift;
        }
        GuardStep::Keep
    }
}

/// Per-terminal stale-limit guards (see [`StaleLimitGuard`]).
static STALE_LIMIT_GUARDS: Mutex<Option<HashMap<String, StaleLimitGuard>>> = Mutex::new(None);

/// Arm the stale-limit guard on `terminal_id`, the pane a hand restore of a
/// `limit_hit` cohort row just spawned. Returns whether a guard was armed.
pub(crate) fn guard_restored_limit_hit(
    terminal_id: &str,
    limit_reset_at_ms: Option<i64>,
    now_ms: i64,
) -> bool {
    let Some(guard) = StaleLimitGuard::arm(limit_reset_at_ms, now_ms) else {
        return false;
    };
    STALE_LIMIT_GUARDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(terminal_id.to_string(), guard);
    info!(
        terminal_id,
        until_ms = guard.until_ms,
        "usage_limit: hand-restored limit_hit session — its repainted limit text will not \
         trigger an account migration"
    );
    true
}

#[cfg(test)]
pub(crate) fn stale_limit_guard_of(terminal_id: &str) -> Option<StaleLimitGuard> {
    STALE_LIMIT_GUARDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(terminal_id).copied())
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
        let mut gate_guard = SCAN_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let gate = gate_guard.get_or_insert_with(super::scan_gate::ScanGate::new);
        let now = Instant::now();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut guards_guard = STALE_LIMIT_GUARDS.lock().unwrap_or_else(|e| e.into_inner());
        let guards = guards_guard.get_or_insert_with(HashMap::new);
        for (tid, session) in &sessions {
            // Skip sessions whose grid has not been mutated since the last
            // pass — the rendered screen is byte-identical, so a limit message
            // frozen on an idle terminal is not re-detected. One atomic load
            // instead of a grid lock + full screen render.
            // A guarded pane is read every tick regardless: its guard can lift
            // on time alone (settle, reset), with no new output to wake the gate.
            let changed = gate.should_scan(tid, session.grid_generation());
            if !changed && !guards.contains_key(tid) {
                continue;
            }
            // Read the *rendered* screen text (rows joined by `\n`). The grid is
            // the VT-parsed cell buffer, so this sees text inside a full-screen
            // TUI that synchronized-output batching hides from a byte scan.
            let text = {
                let grid = session.grid();
                let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
                guard.text_snapshot().text
            };
            let matched = normalized_indicates_usage_limit(&normalize(&text));
            if let Some(guard) = guards.get(tid).copied() {
                match guard.step(matched.is_some(), text.contains('❯'), now_ms) {
                    GuardStep::Suppress => {
                        debug!(
                            terminal_id = %tid,
                            "usage_limit: limit text on a hand-restored limit_hit pane — \
                             suppressed (stale repaint)"
                        );
                        continue;
                    }
                    GuardStep::Keep => continue,
                    GuardStep::Lift => {
                        guards.remove(tid);
                        info!(terminal_id = %tid, "usage_limit: stale-limit guard lifted");
                    }
                }
            }
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
        guards.retain(|tid, _| live.contains(tid));
        gate.retain_live(&live);
    } // fire-state + scan-gate locks released

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
        assert_eq!(
            window_indicates_usage_limit(
                "  ⎿  You've hit your weekly limit · resets Oct 21, 10pm (Europe/Berlin)"
            ),
            Some("hit your weekly limit")
        );
        assert_eq!(
            window_indicates_usage_limit("You've hit your limit · resets 3am"),
            Some("hit your limit")
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

    // ── the stale-limit guard (hand-restored limit_hit rows) ──────────────

    const T0: i64 = 1_800_000_000_000;

    #[test]
    fn a_guard_suppresses_the_repainted_limit_text_until_the_reset() {
        let reset = T0 + 3_600_000;
        let g = StaleLimitGuard::arm(Some(reset), T0).expect("armed");
        assert_eq!(g.until_ms, reset);
        // The resume repaint ~2 s later shows the old limit line beside a
        // prompt: suppressed, not migrated (the E2E failure).
        assert_eq!(g.step(true, true, T0 + 2_000), GuardStep::Suppress);
        // Still painted an hour minus a second later: still suppressed.
        assert_eq!(g.step(true, true, reset - 1_000), GuardStep::Suppress);
        // Past the reset, a confirmed exhaustion is a new one.
        assert_eq!(g.step(true, true, reset), GuardStep::Lift);
    }

    #[test]
    fn a_guard_lifts_once_the_settled_screen_is_clean() {
        let g = StaleLimitGuard::arm(None, T0).expect("armed");
        assert_eq!(g.until_ms, T0 + STALE_LIMIT_MAX_MS);
        // Blank / still booting: keep.
        assert_eq!(g.step(false, false, T0 + 1_000), GuardStep::Keep);
        // A prompt without limit text before the repaint can have finished:
        // keep (the history may still be painting).
        assert_eq!(g.step(false, true, T0 + 1_000), GuardStep::Keep);
        // Settled and clean: lift, so later limit text is treated as fresh.
        assert_eq!(
            g.step(false, true, T0 + STALE_LIMIT_SETTLE_MS),
            GuardStep::Lift
        );
    }

    #[test]
    fn no_guard_once_the_recorded_reset_has_passed() {
        assert_eq!(StaleLimitGuard::arm(Some(T0 - 1), T0), None);
        assert_eq!(StaleLimitGuard::arm(Some(T0), T0), None);
        // A far reset is clamped to the cap.
        let far = StaleLimitGuard::arm(Some(T0 + 10 * STALE_LIMIT_MAX_MS), T0).unwrap();
        assert_eq!(far.until_ms, T0 + STALE_LIMIT_MAX_MS);
    }

    #[test]
    fn guard_restored_limit_hit_arms_per_terminal() {
        let tid = "usage-limit-guard-test-terminal";
        assert!(guard_restored_limit_hit(tid, Some(T0 + 60_000), T0));
        assert_eq!(
            stale_limit_guard_of(tid),
            Some(StaleLimitGuard {
                armed_at_ms: T0,
                until_ms: T0 + 60_000
            })
        );
        assert!(!guard_restored_limit_hit(
            "usage-limit-guard-test-2",
            Some(T0 - 5),
            T0
        ));
        assert_eq!(stale_limit_guard_of("usage-limit-guard-test-2"), None);
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
