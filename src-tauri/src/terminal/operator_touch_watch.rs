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

/// Per-terminal `since_ms` of the idle episode this watcher last fired a
/// touch for. `None` recorded for a terminal not yet fired for its CURRENT
/// episode is represented by simple absence from the map; a present entry
/// whose value differs from the current `since_ms` means a NEW episode has
/// started (see module docs). Never pruned except for terminals no longer
/// live — mirrors `context_watcher::SCAN_GATE`'s own liveness prune.
static LAST_FIRED_SINCE_MS: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

/// One grid-scan tick: evaluate every live terminal's tracked idle window and
/// emit an `idle_at_prompt` touch for any terminal that just crossed
/// [`IDLE_THRESHOLD_MS`] in its CURRENT idle episode. Called from the same
/// tick as `context_watcher::scan_terminals_once` — see
/// `terminal::auto_response::scan_once_blocking`.
///
/// Best-effort and non-blocking by construction: every underlying call
/// (`observe_grid_idle`, `operator_touch::emit`) is synchronous and
/// lock-bounded, matching the tick's existing cost profile.
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
        let mut guard = LAST_FIRED_SINCE_MS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(map) = guard.as_mut() {
            map.retain(|tid, _| live.contains(tid));
        }
    }

    for (tid, session) in sessions {
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

        let already_fired_this_episode = {
            let guard = LAST_FIRED_SINCE_MS
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard
                .as_ref()
                .and_then(|m| m.get(&tid))
                .is_some_and(|&fired_since| fired_since == since_ms)
        };
        if already_fired_this_episode {
            continue;
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
            Ok(()) => {
                let mut guard = LAST_FIRED_SINCE_MS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                guard.get_or_insert_with(HashMap::new).insert(tid, since_ms);
            }
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

/// Handle one `Notification` hook POST for terminal/session key `key` — the
/// SAME key space `context_watcher::on_precompact_signal` uses (the runner
/// terminal id the hook script sends, falling back to the Claude session
/// id). Fail-open at every step: a hook that cannot be attributed to a live
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
