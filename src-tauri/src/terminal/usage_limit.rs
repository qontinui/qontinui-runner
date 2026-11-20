//! Usage-limit detection over the rendered terminal grid.
//!
//! A periodic poller ([`scan_grids_once`] / [`spawn_grid_scan_loop`]) reads each
//! live terminal's *rendered* VT grid (not its raw byte stream) and watches for
//! the Claude CLI's token-exhaustion messages ("Claude usage limit reached",
//! "5-hour limit reached ∙ resets …", "You've hit your weekly limit · resets …",
//! …). When one appears it hands a (debounced) hint to
//! [`super::account_migration::handle_usage_limit_hint`].
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
//! Two filters per terminal: a pass scans a screen only when its grid generation
//! has advanced ([`super::scan_gate`]), and a match fires only when the debounce
//! ([`HINT_DEBOUNCE`], 300s) has passed since that terminal's last hint. A match the
//! debounce suppresses keeps the screen due for a scan, so a limit message that
//! paints inside the window and then freezes still fires once the window ends.
//!
//! A pane that keeps repainting a limit message therefore re-fires at most every
//! 300s. A message frozen on an idle screen, such as the limit menu, fires once,
//! and then the way that hint ended ([`HintOutcome`]) decides whether it fires
//! again on the same grid ([`HintOutcome::retry`], applied by [`after_hint`]):
//!
//! - **Every debounce** — no usable target, a failed migration that started no
//!   resumed `claude`, a respawn that died or never painted and was rolled back
//!   onto the old pane (`RespawnDiedRolledBack`, `UnconfirmedRolledBack`), or
//!   another migration of the session in flight.
//!   The screen stays due and fires again
//!   300s after the last hint, until a hint ends another way: the session
//!   moves once a target frees, and failed attempts stop at the migration cap.
//!   A no-target hint gives its cap slot back, so waiting for a target does
//!   not use up the cap.
//! - **Once per appearance** — the probe did not confirm exhaustion. One more
//!   hint, 300s later, on the same grid generation: the probe's sample can be
//!   up to 60s stale, the one cause of an unconfirmed probe that time fixes.
//! - **Never** — migrated (or an unpainted resumed pane kept the session,
//!   `UnconfirmedKept`), the migration cap reached, auto-migration disabled,
//!   a pane the hint cannot apply to, a session that has already moved,
//!   closed or ended with its respawn (`RespawnDiedEnded`), or a migration
//!   the user declined by closing the resumed pane (`DeclinedByUser`). The
//!   screen rests until its grid changes. A migration that failed after its
//!   resumed `claude` may have started rests its SESSION longer, for the
//!   migration cap's rolling 24h window: that child may still be running, and
//!   a migration would resume the session again beside it. Every hint on that
//!   session ends the same way without migrating, whatever this scanner
//!   recorded in between.
//!
//! Each retry re-probes every configured account, but the probe's 60s usage
//! cache bounds upstream traffic to at most one probe per account per minute
//! for the whole runner, however many panes are stranded. A retried no-target
//! or failed hint is reported to the UI once per episode, not on every retry:
//! the episode ends when a scan finds no limit message on the screen, so the
//! next stranding is reported afresh.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use super::account_migration::{HintOutcome, Retry};
use super::output_scan::normalize;
use super::scan_gate::ScanGate;
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
    // CLI 2.1.x banner: "You've hit your weekly limit · resets Oct 17, 10pm".
    // None of the "… limit reached" spellings match it (2026-10-16 and
    // 2026-10-20: sessions stranded on it with a healthy migration target
    // idle). The leading "You've" is omitted — apostrophe style varies.
    "hit your weekly limit",
    "hit your 5-hour limit",
    "hit your session limit",
    "out of extra usage",
    "session limit reached",
    "hit your opus limit",
    "hit your sonnet limit",
    "hit your fable limit",
    // A per-model cap's other spelling: "You've reached your Fable limit. Run
    // /usage-credits to continue or switch models".
    "reached your fable limit",
    "hit your limit",
    // An org/account monthly SPEND cap: "You've hit your org's monthly spend
    // limit". No "… limit reached" / "hit your limit" spelling matches it
    // (plan 2026-11-13-devops-scheduled-remote-agent-relaunches-into-an-exhausted-account,
    // Evidence 3). The usage probe cannot see a spend cap, so this banner is
    // the only signal there is. The headless classifier (`ai_provider::limit_hit`)
    // treats it as a catch-all that only a stated reset confirms.
    "spend limit",
    // Generic catch-alls LAST (specific patterns above win the label). The
    // CLI's phrasing shifts across releases ("Opus weekly limit reached",
    // "limit reached ∙ resets 3am", "You're out of usage until…") — a missed
    // hint strands a session on a dead account, while a false hint costs one
    // probe-confirmed no-op.
    "limit reached",
    "out of usage",
    // Menu-only signal, after the catch-alls; any banner keeps the label (plan
    // 2026-11-12-a-limit-hit-by-a-background-subagent-shows-only-the-rate-limit-menu-so-the-pane-is-never-migrated).
    // When a BACKGROUND subagent hits the limit, the pane paints only the
    // /rate-limit-options menu ("What do you want to do? 1. Stop and wait for
    // limit to reset …") under "Waiting for 1 background agent to finish" —
    // no "hit your …" banner, so none of the entries above matched and the
    // pane sat unmigrated for 14 h. The menu's stop option is matched, not
    // its question, because "What do you want to do?" titles unrelated Claude
    // Code menus too; claude-config's `LIMIT_MENU` (close-finished-sessions.py)
    // reads this menu by the same option text. The label is conditional in
    // the CLI (2.1.323): it is just "Stop" for a usage-billed account, and for
    // a `credits_required` overage rejection on any account, and this entry
    // does not match that shorter menu (plan follow-up d5c89e8e). A weekly or
    // 5-hour limit on a subscription account, the measured case, draws the
    // full text.
    "stop and wait for limit to reset",
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
/// Normalizes then delegates — the matcher [`scan_grids_once`] runs on every
/// due terminal's rendered screen, and the seam the unit tests enter through.
pub fn window_indicates_usage_limit(window: &str) -> Option<&'static str> {
    normalized_indicates_usage_limit(&normalize(window))
}

// ── Firing policy (per-terminal debounce) ─────────────────────────────────────

/// One terminal's hint history.
#[derive(Debug, Default)]
struct FireState {
    /// When its last hint fired (`None` = never).
    last_fired: Option<Instant>,
    /// How its last finished hint ended, with the grid generation of the screen
    /// that raised it ([`after_hint`]). Cleared by a scan that finds no limit
    /// message on the screen ([`scan_terminal`]): that stranding episode is
    /// over, so the next one is reported afresh.
    last_outcome: Option<(u64, HintOutcome)>,
}

/// Per-terminal hint history. The outer `Option` makes the static
/// const-initializable (mirrors the auto-response edge map). Taken only through
/// [`with_scan_state`].
static FIRE_STATE: Mutex<Option<HashMap<String, FireState>>> = Mutex::new(None);

/// Per-terminal grid-generation watermarks so a tick skips sessions whose rendered
/// screen cannot have changed since the previous pass (see
/// [`super::scan_gate`]). This scanner keeps its OWN gate — sharing one with
/// `auto_response` would let whichever tick ran first consume the change on the
/// other's behalf. Taken only through [`with_scan_state`].
static SCAN_GATE: Mutex<Option<ScanGate>> = Mutex::new(None);

/// Run `f` on the scanner's state under both of its locks, taken in the one
/// order every caller shares: [`FIRE_STATE`], then [`SCAN_GATE`]. They are std
/// mutexes, and a scan pass holds them across every due terminal's grid
/// render, so call this only off the async runtime.
fn with_scan_state<R>(f: impl FnOnce(&mut HashMap<String, FireState>, &mut ScanGate) -> R) -> R {
    let mut state = FIRE_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let mut gate = SCAN_GATE.lock().unwrap_or_else(|e| e.into_inner());
    f(
        state.get_or_insert_with(HashMap::new),
        gate.get_or_insert_with(ScanGate::new),
    )
}

/// Pure firing decision: given the matched pattern (if any), the terminal's
/// last-fired time, and `now`, return the pattern iff a hint should fire and
/// update `last_fired`. The unit-test seam.
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

/// [`should_fire`], plus what a suppressed match owes the scan gate. The gate has
/// already recorded this screen as seen, so a limit message the debounce swallows
/// and that then freezes (a menu painted just after an earlier hint) would never
/// be scanned again. Forgetting the watermark keeps it due until the debounce
/// lets it fire.
fn fire_or_keep_due(
    terminal_id: &str,
    matched: Option<&'static str>,
    last_fired: &mut Option<Instant>,
    gate: &mut ScanGate,
    now: Instant,
    debounce: Duration,
) -> Option<&'static str> {
    let fired = should_fire(matched, last_fired, now, debounce);
    if matched.is_some() && fired.is_none() {
        gate.forget(terminal_id);
    }
    fired
}

/// Record how a hint on `terminal_id` ended, and keep the screen that raised it
/// due when the outcome's retry rule says so ([`HintOutcome::retry`]).
///
/// [`scan_terminal`] recorded that screen (grid `generation`) as seen before
/// the hint fired, so on a pane frozen at the limit menu nothing else would ever
/// scan it again. Forgetting the watermark hands the screen to
/// [`fire_or_keep_due`], which keeps it due on every tick, firing nothing, until
/// the debounce lets it fire once more at the last hint + [`HINT_DEBOUNCE`].
/// [`Retry::OncePerAppearance`] forgets only when this terminal's previous hint
/// did not end the same way on the same generation. A terminal that went away
/// while the hint ran has no entry left, and is skipped.
///
/// Blocks on the scanner's locks ([`with_scan_state`]), so [`dispatch_hint`]
/// runs it on the blocking pool.
fn after_hint(terminal_id: &str, generation: u64, outcome: HintOutcome) {
    with_scan_state(|state, gate| {
        let Some(entry) = state.get_mut(terminal_id) else {
            return;
        };
        let rescan = match outcome.retry() {
            Retry::EveryDebounce => true,
            Retry::OncePerAppearance => entry.last_outcome != Some((generation, outcome)),
            Retry::Never => false,
        };
        entry.last_outcome = Some((generation, outcome));
        if rescan {
            gate.forget(terminal_id);
        }
        debug!(
            terminal_id,
            generation,
            ?outcome,
            rescan,
            "usage-limit hint ended"
        );
    });
}

// ── Grid scanner ─────────────────────────────────────────────────────────────

/// A hint one scan pass decided to fire.
#[derive(Debug)]
struct DueHint {
    terminal_id: String,
    pattern: &'static str,
    /// The grid generation of the screen it matched on, which the scan gate
    /// recorded as seen.
    generation: u64,
    /// How the terminal's previous hint ended.
    previous: Option<HintOutcome>,
}

/// One terminal's share of a scan pass, under [`with_scan_state`]'s locks: the
/// whole per-terminal decision [`scan_grids_once`] makes, and the seam the tests
/// drive. `screen` renders the terminal's screen text and is called only when
/// the gate says the screen is due.
///
/// Skips a terminal whose grid has not been mutated since the last pass: the
/// rendered screen is byte-identical, so a limit message frozen on an idle
/// terminal is not re-detected once it has fired, unless its hint's outcome
/// kept it due ([`after_hint`]). That costs one atomic load instead of a grid
/// lock + full screen render. Otherwise it fires a hint, keeps a debounced
/// match due ([`fire_or_keep_due`]), or — no limit message on screen — ends
/// the terminal's stranding episode by clearing its last outcome, so the next
/// episode's outcome is reported afresh.
fn scan_terminal(
    terminal_id: &str,
    generation: u64,
    screen: impl FnOnce() -> String,
    state: &mut HashMap<String, FireState>,
    gate: &mut ScanGate,
    now: Instant,
) -> Option<DueHint> {
    if !gate.should_scan(terminal_id, generation) {
        return None;
    }
    // The same matcher the unit tests enter through, so every fixture there
    // is a statement about this scan's matcher.
    let matched = window_indicates_usage_limit(&screen());
    // `entry` defaults to "never fired" → the first appearance fires
    // immediately; thereafter the debounce gates re-fires.
    let entry = state.entry(terminal_id.to_string()).or_default();
    if matched.is_none() {
        entry.last_outcome = None;
    }
    let pattern = fire_or_keep_due(
        terminal_id,
        matched,
        &mut entry.last_fired,
        gate,
        now,
        HINT_DEBOUNCE,
    )?;
    Some(DueHint {
        terminal_id: terminal_id.to_string(),
        pattern,
        generation,
        previous: entry.last_outcome.map(|(_, outcome)| outcome),
    })
}

/// The hint task [`scan_grids_once`] spawns for each [`DueHint`]: run it through
/// `handler` — in production
/// [`super::account_migration::handle_usage_limit_hint`] — then record how it
/// ended ([`after_hint`]). `after_hint` waits on the scanner's std mutexes,
/// which a scan pass holds across every due terminal's grid render, so it runs
/// on the blocking pool rather than parking an async-runtime worker.
async fn dispatch_hint<H, F>(hint: DueHint, handler: H)
where
    H: FnOnce(String, &'static str, Option<HintOutcome>) -> F,
    F: std::future::Future<Output = HintOutcome>,
{
    let DueHint {
        terminal_id,
        pattern,
        generation,
        previous,
    } = hint;
    let outcome = handler(terminal_id.clone(), pattern, previous).await;
    if let Err(e) =
        spawn_blocking_tracked(move || after_hint(&terminal_id, generation, outcome)).await
    {
        warn!(error = %e, "usage_limit: recording a hint's outcome panicked");
    }
}

/// One scan pass over every live terminal: read its rendered screen, and for
/// any terminal showing a usage-limit message (debounce-permitting) spawn the
/// account-migration hint ([`dispatch_hint`]). Cheap: one `text_snapshot` +
/// substring scan per terminal the scan gate reports as due, per tick.
pub fn scan_grids_once() {
    use tauri::Manager;

    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    let sessions = tm.sessions_snapshot();

    // Decide under the scanner's locks, then dispatch with NO lock held (the
    // hint handler is async + re-locks its own state, and `after_hint` takes
    // both of these locks when it ends).
    let to_fire = with_scan_state(|state, gate| {
        let now = Instant::now();
        let mut to_fire = Vec::new();
        for (tid, session) in &sessions {
            // Read the *rendered* screen text (rows joined by `\n`). The grid is
            // the VT-parsed cell buffer, so this sees text inside a full-screen
            // TUI that synchronized-output batching hides from a byte scan.
            let screen = || {
                let grid = session.grid();
                let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
                guard.text_snapshot().text
            };
            if let Some(hint) =
                scan_terminal(tid, session.grid_generation(), screen, state, gate, now)
            {
                to_fire.push(hint);
            }
        }
        // Drop state for terminals that have gone away.
        let live: HashSet<&String> = sessions.iter().map(|(t, _)| t).collect();
        state.retain(|tid, _| live.contains(tid));
        gate.retain_live(&live);
        to_fire
    });

    for hint in to_fire {
        info!(
            terminal_id = %hint.terminal_id,
            pattern = hint.pattern,
            "usage-limit message detected on terminal screen"
        );
        tauri::async_runtime::spawn(dispatch_hint(
            hint,
            super::account_migration::handle_usage_limit_hint,
        ));
    }
}

/// Grid-scan interval. Resolved from `QONTINUI_USAGE_LIMIT_SCAN_INTERVAL_MS`
/// (the higher-precedence escape hatch), else
/// `settings.performance.grid_scan_interval_ms`, else the historical 1500 ms —
/// that setting's default. Floored at 200 ms either way. 1.5s is far under the
/// 300s debounce, so a freshly-painted limit message is caught promptly while
/// costing a trivial substring pass per due terminal per tick. See
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
                // Locking each due session's grid and rendering a full screen is CPU
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
    fn hit_your_limit_phrasing_matches() {
        // Verbatim from a stranded session transcript, 2026-10-16.
        assert_eq!(
            window_indicates_usage_limit(
                "You've hit your weekly limit \u{b7} resets Oct 17, 10pm (Europe/Berlin)"
            ),
            Some("hit your weekly limit")
        );
        assert_eq!(
            window_indicates_usage_limit("You\u{2019}ve hit your 5-hour limit \u{b7} resets 3am"),
            Some("hit your 5-hour limit")
        );
    }

    #[test]
    fn every_pattern_is_reachable() {
        // `find` returns the FIRST match, so a pattern that repeats an earlier
        // one, or contains an earlier one as a substring, can never be the
        // label returned. Two parallel fixes once left five such dead entries.
        for (j, later) in USAGE_LIMIT_PATTERNS.iter().enumerate() {
            for earlier in &USAGE_LIMIT_PATTERNS[..j] {
                assert!(
                    !later.contains(earlier),
                    "pattern {later:?} is shadowed by earlier pattern {earlier:?}"
                );
            }
            assert_eq!(window_indicates_usage_limit(later), Some(*later));
        }
    }

    #[test]
    fn hit_your_banners_keep_their_label_beside_other_phrases() {
        // Priority order, not just reachability: these labels are what the
        // migration reads, so a co-painted generic phrase must not take over.
        // The last block pins the menu-only entry below every banner AND both
        // catch-alls (the plan names this test as that entry's priority test).
        assert_eq!(
            window_indicates_usage_limit("out of extra usage\nYou've hit your weekly limit"),
            Some("hit your weekly limit")
        );
        assert_eq!(
            window_indicates_usage_limit("session limit reached · You've hit your session limit"),
            Some("hit your session limit")
        );
        assert_eq!(
            window_indicates_usage_limit("limit reached · You've hit your limit · out of usage"),
            Some("hit your limit")
        );
        // A banner painted beside the limit menu keeps the banner's label: the
        // menu entry sits after the catch-alls.
        assert_eq!(
            window_indicates_usage_limit(&format!(
                "{LIMIT_MENU_ONLY_TAIL}\nYou've hit your weekly limit"
            )),
            Some("hit your weekly limit")
        );
        assert_eq!(
            window_indicates_usage_limit(&format!(
                "{LIMIT_MENU_ONLY_TAIL}\nlimit reached · resets 3am"
            )),
            Some("limit reached")
        );
        // ...and it sits after BOTH catch-alls, not between them.
        assert_eq!(
            window_indicates_usage_limit(&format!("{LIMIT_MENU_ONLY_TAIL}\nYou're out of usage")),
            Some("out of usage")
        );
    }

    /// The bottom of the rendered screen of a pane whose usage limit was hit by
    /// a BACKGROUND subagent: the spinner, panel note, menu and bottom border
    /// are reconstructed from the tail measured on 2026-11-12 (merytshost,
    /// pane db0a8eb9; PTY bytes, ANSI-stripped) and laid out as the rendered
    /// grid would show them, not copied byte for byte; the footer row
    /// after the border is illustrative, not measured. No "hit your …" banner
    /// anywhere (plan
    /// 2026-11-12-a-limit-hit-by-a-background-subagent-shows-only-the-rate-limit-menu-so-the-pane-is-never-migrated).
    const LIMIT_MENU_ONLY_TAIL: &str = "\
✻ Waiting for 1 background agent to finish

 Background task update waiting while this panel is open

 What do you want to do?

 ❯ 1. Stop and wait for limit to reset
   2. Wait here, then continue automatically at Nov 18, 9pm
   3. Switch to usage credits

 Enter to confirm · Esc to cancel
▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
  ⏵⏵ bypass permissions on (shift+tab to cycle)
";

    #[test]
    fn limit_menu_without_a_banner_matches() {
        assert_eq!(
            window_indicates_usage_limit(LIMIT_MENU_ONLY_TAIL),
            Some("stop and wait for limit to reset")
        );
    }

    #[test]
    fn an_unrelated_menu_question_does_not_match() {
        // "What do you want to do?" opens other Claude Code menus too.
        assert_eq!(
            window_indicates_usage_limit("What do you want to do?\n 1. Yes\n 2. No"),
            None
        );
    }

    #[test]
    fn the_short_stop_label_is_not_matched() {
        // The known limitation, pinned: the CLI's short "Stop" label (a
        // usage-billed account, or a credits_required overage rejection) is
        // not matched by the menu entry (plan follow-up d5c89e8e).
        assert_eq!(
            window_indicates_usage_limit(
                "What do you want to do?\n ❯ 1. Stop\n   2. Switch to usage credits"
            ),
            None
        );
    }

    #[test]
    fn reached_your_model_limit_matches() {
        // Verbatim from the fleet's provider-limit tooling
        // (qontinui-claude-config scripts/rate-limit-reset-test.sh).
        assert_eq!(
            window_indicates_usage_limit(
                "You've reached your Fable limit. Run /usage-credits to continue or switch models"
            ),
            Some("reached your fable limit")
        );
        // The CLI's "You've hit your ${limit}" banner template (read from the 2.1.304
        // binary) with its "Fable limit" cap name; the reset text is illustrative.
        assert_eq!(
            window_indicates_usage_limit("You've hit your Fable limit \u{b7} resets Oct 21, 10pm"),
            Some("hit your fable limit")
        );
        assert_eq!(
            window_indicates_usage_limit("You've used 82% of your Fable limit"),
            None
        );
    }

    #[test]
    fn spend_limit_banner_matches() {
        // Plan `2026-11-13-devops-scheduled-remote-agent-relaunches-into-an-exhausted-account`
        // test (f): the monthly spend-cap banner matched none of the patterns.
        assert_eq!(
            window_indicates_usage_limit(
                "You've hit your org's monthly spend limit \u{b7} resets Dec 1, 12am (UTC)"
            ),
            Some("spend limit")
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
    fn a_suppressed_match_that_freezes_still_fires_when_the_debounce_ends() {
        let debounce = Duration::from_secs(300);
        let t0 = Instant::now();
        let at = |secs| t0 + Duration::from_secs(secs);
        let mut gate = crate::terminal::scan_gate::ScanGate::new();
        let mut last = None;
        let hit = Some("hit your weekly limit");

        // An earlier hint fires and opens the debounce window.
        assert!(gate.should_scan("t", 1));
        assert_eq!(
            fire_or_keep_due("t", hit, &mut last, &mut gate, at(0), debounce),
            hit
        );

        // The limit menu paints inside the window, then the screen freezes on
        // generation 2: suppressed, but still due for a scan on every later tick.
        assert!(gate.should_scan("t", 2));
        assert_eq!(
            fire_or_keep_due("t", hit, &mut last, &mut gate, at(10), debounce),
            None
        );
        assert!(gate.should_scan("t", 2));
        assert_eq!(
            fire_or_keep_due("t", hit, &mut last, &mut gate, at(299), debounce),
            None
        );

        // The window ends: it fires once, and the frozen screen then rests.
        assert!(gate.should_scan("t", 2));
        assert_eq!(
            fire_or_keep_due("t", hit, &mut last, &mut gate, at(300), debounce),
            hit
        );
        assert!(!gate.should_scan("t", 2));
    }

    // ---- a frozen screen whose hint did not migrate -------------------------
    // Plan 2026-11-11-a-frozen-usage-limit-screen-whose-hint-did-not-migrate-is-never-rescanned.

    /// A frozen limit menu, as the grid renders it.
    const LIMIT_SCREEN: &str = "  You've hit your weekly limit · resets Oct 21, 10pm";

    /// One scan pass's work for terminal `tid` showing `screen` on grid
    /// `generation` at `now`: [`scan_terminal`] under [`with_scan_state`],
    /// exactly as [`scan_grids_once`] runs it for each terminal, on the
    /// scanner's own statics (the ones [`after_hint`] writes). Returns whether
    /// the gate let the pass read the screen, and the hint it decided to fire.
    /// No test process sets an app handle, so `scan_grids_once` itself never
    /// runs here, and each test owns its terminal id.
    fn scan(tid: &str, generation: u64, screen: &str, now: Instant) -> (bool, Option<DueHint>) {
        let mut read = false;
        let hint = with_scan_state(|state, gate| {
            let render = || {
                read = true;
                screen.to_string()
            };
            scan_terminal(tid, generation, render, state, gate, now)
        });
        (read, hint)
    }

    /// What one scan tick did for a terminal showing a limit message.
    #[derive(Debug, PartialEq, Eq)]
    enum Tick {
        /// The scan gate skipped the screen: it is at rest.
        Rest,
        /// Scanned, and nothing fired.
        Due,
        /// Scanned, and a hint fired.
        Fired,
    }

    /// [`scan`] of [`LIMIT_SCREEN`], as a [`Tick`].
    fn tick(tid: &str, generation: u64, now: Instant) -> Tick {
        match scan(tid, generation, LIMIT_SCREEN, now) {
            (false, _) => Tick::Rest,
            (true, None) => Tick::Due,
            (true, Some(_)) => Tick::Fired,
        }
    }

    /// The outcome `after_hint` last recorded for `tid`, if any.
    fn recorded_outcome(tid: &str) -> Option<(u64, HintOutcome)> {
        with_scan_state(|state, _| state.get(tid).and_then(|entry| entry.last_outcome))
    }

    /// The (terminal, `previous`) each stub handler call was handed.
    type HandlerCalls = Arc<Mutex<Vec<(String, Option<HintOutcome>)>>>;

    /// A stub for `handle_usage_limit_hint`: records what it was handed and
    /// ends the hint in `outcome`.
    fn stub_handler(
        calls: &HandlerCalls,
        outcome: HintOutcome,
    ) -> impl FnOnce(String, &'static str, Option<HintOutcome>) -> std::future::Ready<HintOutcome>
    {
        let calls = Arc::clone(calls);
        move |terminal_id: String, _pattern: &'static str, previous: Option<HintOutcome>| {
            calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((terminal_id, previous));
            std::future::ready(outcome)
        }
    }

    /// The hint task as `scan_grids_once` spawns it, with a stub handler that
    /// ends the first hint `NoUsableTarget`: the frozen screen is due again
    /// afterwards, and the next hint carries the screen's real grid generation
    /// and that outcome as its `previous`, which the handler is handed.
    /// Mutations: drop the `after_hint` call from `dispatch_hint` (the screen
    /// rests at 1.5 s), or build the `DueHint` with `previous: None` (the second
    /// hint carries nothing).
    #[tokio::test]
    async fn a_dispatched_no_target_hint_keeps_its_screen_due_and_hands_its_outcome_on() {
        let tid = "test/usage_limit/dispatch";
        let calls = HandlerCalls::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        let first = scan(tid, 9, LIMIT_SCREEN, at(0))
            .1
            .expect("the first appearance fires");
        assert_eq!((first.generation, first.previous), (9, None));
        dispatch_hint(first, stub_handler(&calls, HintOutcome::NoUsableTarget)).await;
        assert_eq!(
            recorded_outcome(tid),
            Some((9, HintOutcome::NoUsableTarget))
        );

        assert_eq!(
            tick(tid, 9, at(1_500)),
            Tick::Due,
            "the frozen screen is due again"
        );
        let second = scan(tid, 9, LIMIT_SCREEN, at(300_000))
            .1
            .expect("it fires again when the debounce ends");
        assert_eq!(
            (second.generation, second.previous),
            (9, Some(HintOutcome::NoUsableTarget))
        );
        dispatch_hint(second, stub_handler(&calls, HintOutcome::CapReached)).await;

        assert_eq!(tick(tid, 9, at(301_500)), Tick::Rest);
        assert_eq!(
            *calls.lock().unwrap_or_else(|e| e.into_inner()),
            [
                (tid.to_string(), None),
                (tid.to_string(), Some(HintOutcome::NoUsableTarget))
            ]
        );
    }

    /// Report once per episode: a scan that finds no limit message on the
    /// screen ends the terminal's stranding episode, so the next no-target hint
    /// is reported again. Mutation: keep `last_outcome` through a no-match scan
    /// — the next hint then carries `NoUsableTarget` and is not reported.
    #[test]
    fn a_screen_without_the_limit_message_ends_the_stranding_episode() {
        let tid = "test/usage_limit/episode";
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        assert_eq!(tick(tid, 1, at(0)), Tick::Fired);
        after_hint(tid, 1, HintOutcome::NoUsableTarget);
        assert_eq!(
            recorded_outcome(tid),
            Some((1, HintOutcome::NoUsableTarget))
        );

        // The pane moved on: a scan of its new screen finds no limit message.
        let (read, hint) = scan(tid, 2, "cargo build finished in 12s", at(10_000));
        assert!(read && hint.is_none());
        assert_eq!(recorded_outcome(tid), None, "the no-match scan ended it");

        // A later stranding is a new episode: its no-target outcome is news.
        let next = scan(tid, 3, LIMIT_SCREEN, at(300_000))
            .1
            .expect("the new appearance fires");
        assert_eq!(next.previous, None);
        assert!(crate::terminal::account_migration::worth_reporting(
            next.previous,
            HintOutcome::NoUsableTarget
        ));
    }

    /// A migration that failed after its resumed `claude` may have started
    /// rests its session at the scanner too: its frozen screen is not kept
    /// due, and a later repaint on a new grid generation — even after a scan
    /// without the limit text cleared the terminal's last outcome — fires a
    /// hint that ends `FailedAfterChild` with no migration target picked.
    /// Mutation: neutralise the per-session rest check in `cap_then_pick` —
    /// the repaint's hint then picks a target.
    #[tokio::test]
    async fn a_repaint_after_a_child_started_failure_picks_no_target() {
        use crate::terminal::account_migration::{
            cap_then_pick, settle_hint_migration, MigrateError,
        };
        let tid = "test/usage_limit/child-started";
        let sid = "test/usage_limit/child-started-session";
        let stamp = 1_797_000_000_000i64;
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        let first = scan(tid, 1, LIMIT_SCREEN, at(0))
            .1
            .expect("the first appearance fires");
        dispatch_hint(first, move |_tid, _pattern, previous| {
            std::future::ready(settle_hint_migration(
                sid,
                stamp,
                previous,
                Err(MigrateError::FailedAfterChild(
                    "reader wiring failed".to_string(),
                )),
                || {},
                |_| {},
            ))
        })
        .await;
        assert_eq!(
            recorded_outcome(tid),
            Some((1, HintOutcome::FailedAfterChild))
        );
        assert_eq!(tick(tid, 1, at(1_500)), Tick::Rest, "not kept due");

        // A scan without the limit text clears the terminal's outcome...
        let (read, hint) = scan(tid, 2, "cargo build finished in 12s", at(10_000));
        assert!(read && hint.is_none());
        assert_eq!(recorded_outcome(tid), None);

        // ...and the repaint on a new generation fires, with nothing behind it.
        let repaint = scan(tid, 3, LIMIT_SCREEN, at(300_000))
            .1
            .expect("the repaint fires");
        assert_eq!((repaint.generation, repaint.previous), (3, None));
        let picked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let picked_in = Arc::clone(&picked);
        dispatch_hint(repaint, move |_tid, _pattern, previous| {
            let outcome = match cap_then_pick(
                sid,
                "C:/a",
                stamp + 300_000,
                previous,
                move || {
                    picked_in.store(true, std::sync::atomic::Ordering::SeqCst);
                    Some("C:/b".to_string())
                },
                |_| {},
            ) {
                Ok(_) => HintOutcome::Migrated,
                Err(outcome) => outcome,
            };
            std::future::ready(outcome)
        })
        .await;
        assert!(
            !picked.load(std::sync::atomic::Ordering::SeqCst),
            "no migration target is picked beside a child that may still run"
        );
        assert_eq!(
            recorded_outcome(tid),
            Some((3, HintOutcome::FailedAfterChild))
        );
    }

    /// (c) A pane frozen on the limit menu whose hint found no usable target is
    /// rescanned on every tick, fires its next hint at fired_at +
    /// `HINT_DEBOUNCE` and not sooner, and rests once a hint ends for good.
    /// Mutation: drop the `forget` in `after_hint` — the screen then rests at
    /// 1.5 s and never fires again.
    #[test]
    fn a_no_target_hint_keeps_its_frozen_screen_due_until_the_debounce_ends() {
        let tid = "test/usage_limit/no-target";
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        assert_eq!(tick(tid, 7, at(0)), Tick::Fired);
        after_hint(tid, 7, HintOutcome::NoUsableTarget);

        // Frozen on generation 7: due on every tick, nothing fired...
        assert_eq!(tick(tid, 7, at(1_500)), Tick::Due);
        assert_eq!(tick(tid, 7, at(298_500)), Tick::Due);
        // ...until the debounce ends.
        assert_eq!(tick(tid, 7, at(300_000)), Tick::Fired);

        // A final outcome leaves the frozen screen at rest.
        after_hint(tid, 7, HintOutcome::CapReached);
        assert_eq!(tick(tid, 7, at(301_500)), Tick::Rest);
    }

    /// (d) An unconfirmed probe is retried once per appearance: the second
    /// `NotConfirmed` on the same generation rests, and a new generation
    /// restores the allowance. Mutation: treat `NotConfirmed` as
    /// `Retry::EveryDebounce` — the second one then keeps the screen due.
    #[test]
    fn an_unconfirmed_hint_is_retried_once_per_appearance() {
        let tid = "test/usage_limit/not-confirmed";
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        assert_eq!(tick(tid, 3, at(0)), Tick::Fired);
        after_hint(tid, 3, HintOutcome::NotConfirmed);
        assert_eq!(tick(tid, 3, at(1_500)), Tick::Due, "the first is retried");
        assert_eq!(tick(tid, 3, at(300_000)), Tick::Fired);

        after_hint(tid, 3, HintOutcome::NotConfirmed);
        assert_eq!(
            tick(tid, 3, at(301_500)),
            Tick::Rest,
            "the second on the same generation rests"
        );

        // The grid changed: a new appearance, with its own retry.
        assert_eq!(tick(tid, 4, at(600_000)), Tick::Fired);
        after_hint(tid, 4, HintOutcome::NotConfirmed);
        assert_eq!(tick(tid, 4, at(601_500)), Tick::Due);
    }

    #[test]
    fn a_screen_with_no_match_is_not_kept_due() {
        let t0 = Instant::now();
        let mut gate = crate::terminal::scan_gate::ScanGate::new();
        let mut last = Some(t0);
        assert!(gate.should_scan("t", 1));
        assert_eq!(
            fire_or_keep_due(
                "t",
                None,
                &mut last,
                &mut gate,
                t0,
                Duration::from_secs(300)
            ),
            None
        );
        assert!(!gate.should_scan("t", 1));
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
