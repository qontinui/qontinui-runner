//! The `idle_at_prompt` operator-touch trigger (plan
//! `2026-08-27-operator-touch-observation-runner-emitter`, Phase B2, §2a's
//! RESOLVED seam).
//!
//! ## Why this rides `context_watcher`'s tick instead of the looping-agent
//! ## supervisor's
//!
//! The plan's own first draft routed this through
//! `looping_agent_supervisor.rs`'s ticker. That seam sees only terminals
//! reached via a REGISTERED LOOPING AGENT (`LoopingAgentRegistry::list()`),
//! not ordinary operator sessions — instrumenting it would measure a subset
//! of the fleet and report the number as the whole fleet, which is exactly
//! the "emission gap reading as a good week" failure the plan's own Risks
//! section warns about. `terminal::context_watcher::scan_terminals_once`
//! rides the SAME auto-response grid-scan tick but evaluates EVERY live
//! terminal, keyed per terminal id — the right population. This module is a
//! sibling to `context_watcher`, not a change to it, so the two once-per-
//! session latches (context-handoff's and this one's) never share a keyspace
//! and cannot suppress each other.
//!
//! ## "Once per episode", not "once per terminal forever"
//!
//! `context_watcher`'s own `mark_fired` is a permanent once-per-terminal
//! latch, correct for context-handoff (a terminal that hands off is done).
//! An idle-at-prompt touch is different: a terminal can go idle, become busy
//! again (the operator or the agent responds), and go idle again later — a
//! NEW episode that should be counted again. So this module does not reuse
//! `mark_fired`'s keyspace; instead it tracks, per terminal id, the
//! `since_ms` of the idle episode it last fired for
//! ([`qontinui_runner_lib::wind_down::GridIdle::Idle`], via
//! `TerminalSession::observe_grid_idle`, the SAME tracked-idle-window
//! primitive `wind_down` already carries per pane — reused rather than
//! re-implementing a second copy of the idle debounce). A tick only fires
//! when the CURRENT episode's `since_ms` differs from the one already
//! recorded for that terminal, which naturally re-arms across a busy→idle
//! transition without needing to observe "busy" directly. Even if two ticks
//! raced past that check, coord's `ON CONFLICT (idempotency_key) DO NOTHING`
//! on the same 60s bucket absorbs the duplicate — this latch only saves the
//! wasted HTTP round trip (plan §2a's resolution: "a latch at the source is
//! cheaper... but the idempotency key's epoch bucket bounds it").
//!
//! ## Only a pane that hosts `claude` counts
//!
//! The idle heuristic (`snapshot_looks_idle`) keys on Claude Code's `❯`
//! input caret, and plain shell prompts draw the same glyph (starship,
//! zsh-pure). Every terminal created through `terminal_create` gets a coord
//! mirror, so a bare shell tab sitting at its prompt would otherwise read as
//! an agent waiting on the operator and be recorded as one — inflating the
//! very metric this store exists to measure. So before a candidate fires,
//! the scan proves a `claude` process lives in the pane's inclusive process
//! subtree (`mcp::steward::pane_hosts_claude`, the probe the steward already
//! uses). The process-table snapshot is taken at most once per tick and ONLY
//! when some pane is about to fire, so the steady-state tick stays as cheap
//! as before.
//!
//! Three outcomes, see [`decide_candidate`]: proven `claude` → emit; proven
//! no `claude` → the episode is settled without a touch (no re-probe until
//! the pane's next idle episode); could not tell → no touch, episode left
//! open. A pane with no local pid (a remote pane) is never a candidate here,
//! and an unreadable table decides nothing and backs further probes off for
//! [`UNREADABLE_PROBE_BACKOFF_MS`]. "Could not look" is never recorded as a
//! touch.
//!
//! ## The threshold
//!
//! 60 seconds — the same width as [`crate::session::operator_touch`]'s dedup
//! bucket, and the plan's own reasoning for it: Claude Code's `Notification`
//! idle event fires at 60s, so one idle episode maps to one bucket either
//! way this is observed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tracing::warn;

/// How long a terminal must have looked continuously idle before this counts
/// as an `idle_at_prompt` touch, in milliseconds. Same width as
/// [`crate::session::operator_touch::BUCKET_WIDTH_SECS`] — see the module
/// docs for why.
const IDLE_THRESHOLD_MS: i64 = crate::session::operator_touch::BUCKET_WIDTH_SECS * 1000;

/// Per-terminal `since_ms` of the idle episode this watcher last SETTLED —
/// fired a touch for, or proved hosts no `claude` (module docs). A terminal
/// whose CURRENT episode is unsettled is simply absent, or present with a
/// different `since_ms` (a NEW episode has started). Never pruned except for
/// terminals no longer live — mirrors `context_watcher::SCAN_GATE`'s own
/// liveness prune.
static LAST_SETTLED_SINCE_MS: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

/// How long to wait before re-taking the process-table snapshot after one
/// came back unreadable. On Windows a snapshot is a PowerShell/WMI spawn
/// bounded by its own timeout; without this a persistently failing table
/// would cost one such spawn on every grid-scan tick.
const UNREADABLE_PROBE_BACKOFF_MS: i64 = 60_000;

/// Wall-clock millis before which no process-table snapshot is taken (the
/// backoff above). `0` = no backoff in force.
static PROBE_NOT_BEFORE_MS: Mutex<i64> = Mutex::new(0);

/// What one idle candidate's episode resolves to, given whether its pane is
/// proven to host a `claude` process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateDecision {
    /// A `claude` is in the pane — record the `idle_at_prompt` touch.
    Emit,
    /// Proven no `claude` (a shell at its own `❯` prompt) — settle the
    /// episode without a touch, so it is not re-probed every tick.
    SettleWithoutTouch,
    /// Could not tell — no touch, episode left open for a later tick.
    Undecided,
}

/// Pure: map `pane_hosts_claude`'s three-valued answer to a decision.
fn decide_candidate(hosts_claude: Option<bool>) -> CandidateDecision {
    match hosts_claude {
        Some(true) => CandidateDecision::Emit,
        Some(false) => CandidateDecision::SettleWithoutTouch,
        None => CandidateDecision::Undecided,
    }
}

fn settle_episode(tid: String, since_ms: i64) {
    let mut guard = LAST_SETTLED_SINCE_MS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(HashMap::new).insert(tid, since_ms);
}

/// One grid-scan tick: evaluate every live terminal's tracked idle window and
/// emit an `idle_at_prompt` touch for any terminal that just crossed
/// [`IDLE_THRESHOLD_MS`] in its CURRENT idle episode and whose pane is proven
/// to host a `claude` process (module docs). Called from the same tick as
/// `context_watcher::scan_terminals_once` — see
/// `terminal::auto_response::scan_once_blocking`.
///
/// Best-effort by construction: `observe_grid_idle` and `operator_touch::emit`
/// are synchronous and lock-bounded. The one heavier call, the process-table
/// snapshot, runs only on a tick where some pane is about to fire, at most
/// once per tick, and is itself timeout-bounded. The tick already runs on a
/// blocking-pool thread (`auto_response::scan_once_blocking`), so blocking on
/// it here never parks an async worker.
pub fn scan_idle_touches_once() {
    use tauri::Manager;

    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    let registry = app
        .try_state::<Arc<crate::session::SessionRegistry>>()
        .map(|s| s.inner().clone());
    let Some(registry) = registry else {
        return;
    };

    let sessions = tm.sessions_snapshot();

    // Prune entries for terminals that have gone away, so a closed terminal's
    // id cannot outlive it in this map (same discipline as
    // `context_watcher::SCAN_GATE::retain_live`).
    {
        let live: std::collections::HashSet<&String> = sessions.iter().map(|(t, _)| t).collect();
        let mut guard = LAST_SETTLED_SINCE_MS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(map) = guard.as_mut() {
            map.retain(|tid, _| live.contains(tid));
        }
    }

    // Candidates: live, coord-mirrored, idle past the threshold, and not yet
    // settled for this episode. Collected first so the process-table snapshot
    // below is taken only when at least one exists.
    let mut candidates = Vec::new();
    for (tid, session) in sessions {
        // An exited pane stays in the snapshot (a non-zero exit is kept
        // visible), and its frozen last frame can read as "idle at the
        // prompt" forever. A dead process is not waiting on the operator.
        if !session.is_alive() {
            continue;
        }
        // No local pid (a remote pane) ⇒ the `claude`-host proof below can
        // never be made here, and skipping now keeps such a pane from
        // forcing a process-table snapshot on every tick.
        if session.child_pid().is_none() {
            continue;
        }
        let Some(coord_session_id) = session.coord_session_id() else {
            // No coord mirror ⇒ nothing to attribute the touch to. Not an
            // error: plenty of terminals (a bare shell tab, a session whose
            // registration failed) never get one.
            continue;
        };
        let since_ms = match session.observe_grid_idle() {
            qontinui_runner_lib::wind_down::GridIdle::Idle { since_ms } => since_ms,
            qontinui_runner_lib::wind_down::GridIdle::Busy
            | qontinui_runner_lib::wind_down::GridIdle::Unknown => continue,
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        if now_ms - since_ms < IDLE_THRESHOLD_MS {
            continue;
        }

        let already_settled_this_episode = {
            let guard = LAST_SETTLED_SINCE_MS
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard
                .as_ref()
                .and_then(|m| m.get(&tid))
                .is_some_and(|&settled_since| settled_since == since_ms)
        };
        if already_settled_this_episode {
            continue;
        }
        candidates.push((tid, session, coord_session_id, since_ms));
    }
    if candidates.is_empty() {
        return;
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    if now_ms
        < *PROBE_NOT_BEFORE_MS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    {
        return;
    }
    let snapshot = tauri::async_runtime::block_on(
        crate::process_capture::process_tree::snapshot_process_table_public(),
    );
    if snapshot.parent_map.is_empty() {
        // Unreadable table: nothing can be proven for any candidate this tick.
        *PROBE_NOT_BEFORE_MS
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = now_ms + UNREADABLE_PROBE_BACKOFF_MS;
        warn!(
            candidates = candidates.len(),
            "operator_touch_watch: process table unreadable — idle_at_prompt candidates left undecided, retrying after backoff"
        );
        return;
    }

    for (tid, session, coord_session_id, since_ms) in candidates {
        match decide_candidate(crate::mcp::steward::pane_hosts_claude(
            &snapshot,
            session.child_pid(),
        )) {
            CandidateDecision::Emit => {}
            CandidateDecision::SettleWithoutTouch => {
                settle_episode(tid, since_ms);
                continue;
            }
            CandidateDecision::Undecided => continue,
        }

        // `&str`, not `Option` — a terminal with no identity-seam pin reports
        // an empty string, which `touch_payload` already treats as absent.
        //
        // Latch the episode ONLY on a successful enqueue. `emit`'s `Err` means
        // the LOCAL outbox append itself failed (disk full, poisoned lock) —
        // nothing was durably recorded — so latching here regardless would
        // silently drop the touch for the rest of a continuous idle episode,
        // which for a wedged terminal can be hours. Leaving it unlatched
        // means the very next tick retries, at the cost of one extra
        // synchronous append attempt per tick until it succeeds.
        match crate::session::operator_touch::emit(
            &registry,
            coord_session_id,
            crate::session::operator_touch::KIND_IDLE_AT_PROMPT,
            Some(session.pinned_session_id()),
        ) {
            Ok(()) => settle_episode(tid, since_ms),
            Err(e) => {
                warn!(
                    terminal_id = %tid,
                    coord_session = %coord_session_id,
                    error = %e,
                    "operator_touch_watch: idle_at_prompt enqueue failed — will retry next tick"
                );
            }
        }
    }
}

// ===========================================================================
// The `Notification` hook landing pad — triggers 2/3 (permission_prompt,
// collapsing the question-tool case; plan §2a2/§2a3/§2a4)
// ===========================================================================

/// Substrings Claude Code's own 60-second-idle `Notification.message` is
/// known to carry. Trigger 1 (`idle_at_prompt`) already owns that event via
/// [`scan_idle_touches_once`]; recording it AGAIN here under
/// `permission_prompt` would double-count the same real-world touch under
/// two DIFFERENT `kind`s, which coord's per-kind idempotency key cannot
/// dedup (§2a3: the payload distinguishes permission-needed from idle only
/// by this free-text field). Named as a constant, not inlined, so the
/// double-count hazard stays legible at the call site rather than folded
/// into a boolean.
const IDLE_SHAPED_MESSAGE_SUBSTRINGS: [&str; 2] = ["waiting for your input", "waiting for input"];

/// Pure classifier: does this `Notification` payload look like the 60s-idle
/// case rather than a permission/question prompt? See the constant above.
fn message_is_idle_shaped(payload: &Value) -> bool {
    let Some(message) = payload.get("message").and_then(Value::as_str) else {
        return false;
    };
    let lower = message.to_ascii_lowercase();
    IDLE_SHAPED_MESSAGE_SUBSTRINGS
        .iter()
        .any(|needle| lower.contains(needle))
}

/// Wire-facing outcome of one `Notification` hook POST.
#[derive(Debug, Clone)]
pub struct NotificationOutcome {
    pub recorded: bool,
    pub kind: Option<&'static str>,
    pub reason: String,
}

/// Handle one `Notification` hook POST for terminal key `key` — the runner
/// terminal id (`QONTINUI_TERMINAL_ID`) the hook script sends, looked up with
/// a plain `TerminalManager::get`. There is deliberately NO Claude
/// session-id fallback (plan vet D1): the hook keys on the terminal id only
/// and stands down when it has none. Fail-open at every step: a hook that
/// cannot be attributed to a live
/// terminal, a coord mirror, or a registry records nothing rather than
/// erroring, matching `on_precompact_signal`'s own posture (a broken watcher
/// must never break a hook).
///
/// §2a3's collapse, restated as code: everything that is NOT idle-shaped is
/// recorded as `permission_prompt` — Claude Code gives this hook no way to
/// tell a permission prompt from a rendered question tool, so the plan
/// accepts the collapse rather than fabricating a distinction.
pub fn on_notification_signal(key: &str, payload: &Value) -> NotificationOutcome {
    let done = |recorded: bool, kind: Option<&'static str>, reason: &str| NotificationOutcome {
        recorded,
        kind,
        reason: reason.to_string(),
    };

    if message_is_idle_shaped(payload) {
        return done(
            false,
            None,
            "idle-shaped Notification — owned by the idle_at_prompt grid-scan \
             trigger, not re-recorded here (see IDLE_SHAPED_MESSAGE_SUBSTRINGS)",
        );
    }

    use tauri::Manager;
    let Some(app) = crate::tauri_app_handle::current() else {
        return done(false, None, "no app handle");
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return done(false, None, "no terminal manager");
    };
    let Some(session) = tm.get(key) else {
        return done(false, None, "unknown terminal/session key");
    };
    let Some(coord_session_id) = session.coord_session_id() else {
        return done(false, None, "terminal has no coord mirror");
    };
    let Some(registry) = app
        .try_state::<Arc<crate::session::SessionRegistry>>()
        .map(|s| s.inner().clone())
    else {
        return done(false, None, "no session registry");
    };

    match crate::session::operator_touch::emit(
        &registry,
        coord_session_id,
        crate::session::operator_touch::KIND_PERMISSION_PROMPT,
        Some(session.pinned_session_id()),
    ) {
        Ok(()) => done(
            true,
            Some(crate::session::operator_touch::KIND_PERMISSION_PROMPT),
            "recorded",
        ),
        Err(e) => {
            warn!(key = %key, error = %e, "operator_touch_watch: permission_prompt enqueue failed");
            done(false, None, "enqueue failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `IDLE_THRESHOLD_MS` must stay derived from the shared bucket constant,
    /// not re-typed as an independent literal that could drift from it.
    #[test]
    fn idle_threshold_matches_the_shared_bucket_width_in_milliseconds() {
        assert_eq!(
            IDLE_THRESHOLD_MS,
            crate::session::operator_touch::BUCKET_WIDTH_SECS * 1000
        );
        assert_eq!(IDLE_THRESHOLD_MS, 60_000);
    }

    #[test]
    fn only_a_proven_claude_pane_emits_an_idle_touch() {
        assert_eq!(decide_candidate(Some(true)), CandidateDecision::Emit);
        assert_eq!(
            decide_candidate(Some(false)),
            CandidateDecision::SettleWithoutTouch,
            "a shell at its own ❯ prompt is not an agent waiting on the operator"
        );
        assert_eq!(
            decide_candidate(None),
            CandidateDecision::Undecided,
            "could-not-look is never recorded as a touch, nor settled"
        );
    }

    #[test]
    fn idle_shaped_notification_messages_are_recognized() {
        assert!(message_is_idle_shaped(
            &serde_json::json!({"message": "Claude is waiting for your input"})
        ));
        assert!(message_is_idle_shaped(
            &serde_json::json!({"message": "Still waiting for input on this one"})
        ));
    }

    #[test]
    fn permission_and_question_shaped_messages_are_not_idle_shaped() {
        assert!(!message_is_idle_shaped(
            &serde_json::json!({"message": "Claude needs your permission to use Bash"})
        ));
        assert!(!message_is_idle_shaped(&serde_json::json!({})));
        assert!(!message_is_idle_shaped(&serde_json::Value::Null));
    }

    #[test]
    fn message_matching_is_case_insensitive() {
        assert!(message_is_idle_shaped(
            &serde_json::json!({"message": "WAITING FOR YOUR INPUT"})
        ));
    }
}
