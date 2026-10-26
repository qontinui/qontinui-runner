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

// ── Hand-restored limit_hit sessions ─────────────────────────────────────────

/// A terminal spawned by the operator's hand restore of a restart-cohort row
/// that hit its account's usage limit during the prepare (`limit_hit`) — kept
/// on that same, possibly still exhausted, account by choice (plan
/// `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, D3).
/// `claude --resume` repaints the old "usage limit reached" line, and before
/// this marker the scanner fired on it ~2 s after the resume and migrated the
/// session the operator had just chosen to keep.
///
/// **Two rules, nothing heuristic about the screen:**
///
/// 1. **Inside the window — until the recorded reset plus
///    [`RESTORED_LIMIT_GRACE_MS`], or [`RESTORED_LIMIT_MAX_MS`] from the
///    restore when no reset was recorded — no usage-limit hint fires for the
///    terminal at all**, whatever the screen shows ([`hint_suppressed`]). The
///    scan does not even read the pane. Any limit text then (old or new) is
///    the exhaustion the operator accepted.
/// 2. **For the terminal's whole life, a hint may migrate it only on a
///    SUCCESSFUL probe that reads exhausted** ([`restored_marker`], read by
///    `account_migration::hint_gate`). Past the window the pane
///    takes the normal path, and stale limit text on screen then costs at most
///    a probe: after the reset a successful probe reads not exhausted. But the
///    fleet-wide confirm also counts a FAILED probe (no token, any non-2xx, a
///    network error) as exhausted (`commands::ai_settings::probe_result_exhausted`),
///    which would let stale text plus a transient probe failure migrate the
///    session; for this terminal a probe error is NotConfirmed instead.
///
/// The marker is kept for the terminal's life, not cleared on a later clean
/// probe: whether a later probe error is a real exhaustion or a transient
/// failure is exactly what cannot be told apart, and this session sits on the
/// account the operator chose, so it only ever moves on a positive reading.
/// A migration spawns a NEW terminal, which carries no marker. Dropped with
/// the terminal by the scan's live-set retain — but only a marker created
/// BEFORE that pass's terminal snapshot: the resume core marks a new pane while
/// a pass may be running, and that pane is not in the pass's snapshot yet
/// ([`retain_restored_limit_hits`]).
///
/// **Known limitation:** markers live in this process only. A runner restart
/// with a PTY-holder handoff adopts the pane in a new process with no marker,
/// so an adopted pane reverts to ordinary usage-limit semantics (the probe
/// still has to confirm before anything migrates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestoredLimitHit {
    /// No hint fires for the terminal before this instant (unix millis).
    pub suppress_until_ms: i64,
    /// When the marker was made, against which a scan pass's snapshot instant
    /// is compared before the marker may be dropped.
    pub created_at: Instant,
}

/// How long past the recorded reset hints stay suppressed: a usage endpoint
/// can keep reporting the old window as full for a while after it rolled.
pub(crate) const RESTORED_LIMIT_GRACE_MS: i64 = 10 * 60 * 1000;
/// The window when the account's reset time is unknown: six hours, the
/// exhausted-account cooldown's own clamp.
pub(crate) const RESTORED_LIMIT_MAX_MS: i64 = 6 * 3600 * 1000;

impl RestoredLimitHit {
    /// The marker for a restore at `now_ms` of a row whose account resets at
    /// `limit_reset_at_ms`. A reset (plus grace) already past gives a window
    /// that is already over; the marker still applies rule 2.
    pub(crate) fn new(limit_reset_at_ms: Option<i64>, now_ms: i64) -> Self {
        Self {
            suppress_until_ms: match limit_reset_at_ms {
                Some(reset) => reset.saturating_add(RESTORED_LIMIT_GRACE_MS),
                None => now_ms.saturating_add(RESTORED_LIMIT_MAX_MS),
            },
            created_at: Instant::now(),
        }
    }

    /// Rule 1 for this marker at `now_ms`.
    pub(crate) fn suppresses(&self, now_ms: i64) -> bool {
        now_ms < self.suppress_until_ms
    }
}

/// Per-terminal markers (see [`RestoredLimitHit`]).
static RESTORED_LIMIT_HITS: Mutex<Option<HashMap<String, RestoredLimitHit>>> = Mutex::new(None);

fn restored_limit_hits() -> std::sync::MutexGuard<'static, Option<HashMap<String, RestoredLimitHit>>>
{
    RESTORED_LIMIT_HITS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Mark `terminal_id`, the pane a hand restore of a `limit_hit` cohort row
/// just spawned. Returns the marker.
pub(crate) fn guard_restored_limit_hit(
    terminal_id: &str,
    limit_reset_at_ms: Option<i64>,
    now_ms: i64,
) -> RestoredLimitHit {
    let marker = RestoredLimitHit::new(limit_reset_at_ms, now_ms);
    restored_limit_hits()
        .get_or_insert_with(HashMap::new)
        .insert(terminal_id.to_string(), marker);
    info!(
        terminal_id,
        suppress_until_ms = marker.suppress_until_ms,
        "usage_limit: hand-restored limit_hit session — no usage-limit hint until its \
         reset window passes, and a migration only on a successful exhausted probe"
    );
    marker
}

/// `terminal_id`'s marker, if it was hand-restored from a `limit_hit` row.
pub(crate) fn restored_marker(terminal_id: &str) -> Option<RestoredLimitHit> {
    restored_limit_hits()
        .as_ref()
        .and_then(|m| m.get(terminal_id).copied())
}

/// Rule 1: whether no usage-limit hint may fire for `terminal_id` at `now_ms`.
pub(crate) fn hint_suppressed(terminal_id: &str, now_ms: i64) -> bool {
    restored_marker(terminal_id).is_some_and(|r| r.suppresses(now_ms))
}

/// Rule 2: whether `terminal_id` was hand-restored from a `limit_hit` row, so
/// only a successful exhausted probe may migrate it.
pub(crate) fn is_restored_limit_hit(terminal_id: &str) -> bool {
    restored_limit_hits()
        .as_ref()
        .is_some_and(|m| m.contains_key(terminal_id))
}

/// Drop the markers of terminals that are gone: not in `live`, the terminal
/// snapshot taken at `snapshot_at`. A marker created at or after that instant
/// is kept whatever `live` says — its terminal may simply be newer than the
/// snapshot (the resume core marks a pane while a pass runs); the next pass,
/// whose snapshot is later, judges it.
fn retain_restored_limit_hits(live: &HashSet<&String>, snapshot_at: Instant) {
    if let Some(m) = restored_limit_hits().as_mut() {
        m.retain(|tid, r| live.contains(tid) || r.created_at >= snapshot_at);
    }
}

/// Serializes the tests that read or write the process-global marker map, so
/// one test's retain cannot drop another's markers under parallel `cargo test`.
#[cfg(test)]
pub(crate) static MARKER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// [`MARKER_TEST_LOCK`] taken under the test-lock hierarchy (`env_lock`
/// first), as every shared test lock must be.
#[cfg(test)]
pub(crate) fn marker_test_lock() -> crate::test_env::TestLockGuard {
    crate::test_env::hierarchy_lock(&MARKER_TEST_LOCK)
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
    // Taken BEFORE the snapshot: a marker created after this instant belongs
    // to a terminal the snapshot may not hold yet, and survives this pass.
    let snapshot_at = Instant::now();
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
        for (tid, session) in &sessions {
            // A hand-restored limit_hit pane inside its reset window: no hint
            // at all, so the screen is not even read (`RestoredLimitHit`).
            if hint_suppressed(tid, now_ms) {
                continue;
            }
            // Skip sessions whose grid has not been mutated since the last
            // pass — the rendered screen is byte-identical, so a limit message
            // frozen on an idle terminal is not re-detected. One atomic load
            // instead of a grid lock + full screen render.
            if !gate.should_scan(tid, session.grid_generation()) {
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
        retain_restored_limit_hits(&live, snapshot_at);
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

    // ── hand-restored limit_hit sessions ──────────────────────────────────

    const T0: i64 = 1_800_000_000_000;

    #[test]
    fn the_window_is_the_reset_plus_grace_or_six_hours() {
        let reset = T0 + 3_600_000;
        assert_eq!(
            RestoredLimitHit::new(Some(reset), T0).suppress_until_ms,
            reset + RESTORED_LIMIT_GRACE_MS
        );
        assert_eq!(
            RestoredLimitHit::new(None, T0).suppress_until_ms,
            T0 + RESTORED_LIMIT_MAX_MS
        );
        // A known weekly reset is not clamped to six hours.
        let week = T0 + 7 * 86_400_000;
        assert_eq!(
            RestoredLimitHit::new(Some(week), T0).suppress_until_ms,
            week + RESTORED_LIMIT_GRACE_MS
        );
    }

    /// Serializes the tests that write or retain the process-global marker map.
    fn marker_lock() -> crate::test_env::TestLockGuard {
        marker_test_lock()
    }

    /// Rule 1: inside the window no hint fires for the terminal, whatever the
    /// screen shows; once it passes the normal `should_fire` path runs.
    #[test]
    fn inside_the_window_no_hint_fires_and_after_it_the_normal_path_runs() {
        let _l = marker_lock();
        let tid = "usage-limit-restored-window";
        let reset = T0 + 3_600_000;
        let marker = guard_restored_limit_hit(tid, Some(reset), T0);
        assert!(hint_suppressed(tid, T0 + 2_000), "the resume repaint");
        assert!(hint_suppressed(tid, reset), "at the exact reset");
        assert!(hint_suppressed(tid, marker.suppress_until_ms - 1));
        assert!(!hint_suppressed(tid, marker.suppress_until_ms));
        // Past the window the marker remains for rule 2 only.
        assert!(is_restored_limit_hit(tid));
        // An unmarked terminal is never suppressed.
        assert!(!hint_suppressed("usage-limit-unmarked", T0));
        assert!(!is_restored_limit_hit("usage-limit-unmarked"));
    }

    /// Re-review N3: a restore whose recorded reset (plus grace) has already
    /// passed suppresses nothing, but the terminal is still marked.
    #[test]
    fn an_already_elapsed_window_suppresses_nothing_but_still_marks() {
        let _l = marker_lock();
        let tid = "usage-limit-restored-elapsed";
        guard_restored_limit_hit(tid, Some(T0 - RESTORED_LIMIT_GRACE_MS - 1), T0);
        assert!(!hint_suppressed(tid, T0));
        assert!(is_restored_limit_hit(tid));
    }

    #[test]
    fn a_marker_is_dropped_with_its_terminal() {
        let _l = marker_lock();
        let (gone, kept) = ("usage-limit-restored-gone", "usage-limit-restored-kept");
        guard_restored_limit_hit(gone, None, T0);
        guard_restored_limit_hit(kept, None, T0);
        let snapshot_at = Instant::now();
        let kept_s = kept.to_string();
        let live: HashSet<&String> = [&kept_s].into_iter().collect();
        retain_restored_limit_hits(&live, snapshot_at);
        assert!(!is_restored_limit_hit(gone));
        assert!(!hint_suppressed(gone, T0));
        assert!(is_restored_limit_hit(kept));
    }

    /// Re-review M1: the resume core marks a new pane WHILE a scan pass runs;
    /// the pane is not in that pass's terminal snapshot, and its marker must
    /// survive the pass's retain (or the repainted old text fires inside the
    /// window).
    #[test]
    fn a_marker_made_after_the_snapshot_survives_the_retain() {
        let _l = marker_lock();
        let tid = "usage-limit-restored-mid-pass";
        let snapshot_at = Instant::now();
        // The pass's snapshot holds no such terminal.
        std::thread::sleep(std::time::Duration::from_millis(2));
        guard_restored_limit_hit(tid, None, T0);
        retain_restored_limit_hits(&HashSet::new(), snapshot_at);
        assert!(is_restored_limit_hit(tid), "wiped by a pass it postdates");
        assert!(hint_suppressed(tid, T0 + 1));
        // The next pass (a later snapshot) still without it: dropped.
        retain_restored_limit_hits(&HashSet::new(), Instant::now());
        assert!(!is_restored_limit_hit(tid));
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
