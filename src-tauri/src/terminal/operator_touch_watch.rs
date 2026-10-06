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
static LAST_FIRED_SINCE_MS: Mutex<Option<HashMap<String, FiredLatch>>> = Mutex::new(None);

/// One terminal's fired idle episode: the `since_ms` it fired for, and how
/// many CONSECUTIVE ticks have since read the pane `Busy` (plan
/// `2026-10-05-operator-touch-close-path` Phase 3, D4's episode-end row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FiredLatch {
    since_ms: i64,
    busy_ticks: u32,
}

/// Consecutive `Busy` ticks that END a fired idle episode. Two, not one: a
/// single busy frame can be a transient redraw at the prompt, and closing the
/// open on it would turn the human's answer that follows into a
/// `self_resolved`.
const EPISODE_END_BUSY_TICKS: u32 = 2;

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

    // Close path (plan `2026-10-05-operator-touch-close-path` Phase 3, D4):
    // the terminals holding a remembered open are read BEFORE the live
    // snapshot, so an open remembered after the snapshot cannot be mistaken
    // for an orphan of a terminal the snapshot predates.
    let open_touch_terminals = registry.coord_sync().open_touches().terminal_ids();

    let sessions = tm.sessions_snapshot();

    // A remembered open whose terminal is gone (removed, exited, or minted by
    // a previous process — terminal ids are fresh per process, so this is
    // also the startup recovery) is closed `abandoned`.
    {
        let alive: std::collections::HashSet<&str> = sessions
            .iter()
            .filter(|(_, s)| s.is_alive())
            .map(|(t, _)| t.as_str())
            .collect();
        crate::session::operator_touch_close::close_orphaned(
            &registry,
            &open_touch_terminals,
            |tid| alive.contains(tid),
        );
    }

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
        // An exited pane stays in the snapshot (a non-zero exit is kept
        // visible), and its frozen last frame can read as "idle at the
        // prompt" forever. A dead process is not waiting on the operator.
        if !session.is_alive() {
            continue;
        }
        let Some(coord_session_id) = session.coord_session_id() else {
            // No coord mirror ⇒ nothing to attribute the touch to. Not an
            // error: plenty of terminals (a bare shell tab, a session whose
            // registration failed) never get one.
            continue;
        };
        let state = session.observe_grid_idle();

        // D4's "episode ends with no input" (plan
        // `2026-10-05-operator-touch-close-path` Phase 3): the pane has left
        // the idle episode this latch fired for. An input that answered it
        // already took its open; whatever is left closes `self_resolved`, and
        // the latch is released.
        let episode_ended = {
            let mut guard = LAST_FIRED_SINCE_MS
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let map = guard.get_or_insert_with(HashMap::new);
            let mut latch = map.get(&tid).copied();
            let ended = idle_episode_step(&mut latch, &state);
            match latch {
                Some(l) => map.insert(tid.clone(), l),
                None => map.remove(&tid),
            };
            ended
        };
        if episode_ended {
            crate::session::operator_touch_close::close_idle_episode(&registry, &tid);
        }

        let since_ms = match state {
            qontinui_runner_lib::wind_down::GridIdle::Idle { since_ms } => since_ms,
            qontinui_runner_lib::wind_down::GridIdle::Busy
            | qontinui_runner_lib::wind_down::GridIdle::Unknown => continue,
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        if now_ms - since_ms < IDLE_THRESHOLD_MS {
            continue;
        }

        let latch = {
            let guard = LAST_FIRED_SINCE_MS
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.as_ref().and_then(|m| m.get(&tid).copied())
        };
        // ONE wait is ONE touch (plan `2026-10-05-operator-touch-close-path`
        // Phase 3): a repaint restarts `since_ms` while the pane still waits,
        // so a live idle open on this terminal suppresses a new touch until
        // that open is closed by some path.
        let holds_live_open = registry
            .coord_sync()
            .open_touches()
            .holds_live_idle_open(&tid);
        if !should_fire_idle_touch(latch, since_ms, holds_live_open) {
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
        //
        // Backstop first. Reaching here means no LIVE idle open is held (one
        // would have suppressed this episode above), so this can never close
        // an open whose episode is still live. What it does close is an older
        // idle open put back after a failed close (input, episode end or
        // death): it retries that close with the words already decided —
        // `self_resolved` for a failed episode end, `answered` for a failed
        // input — before the new episode opens.
        crate::session::operator_touch_close::close_idle_episode(&registry, &tid);

        match crate::session::operator_touch::emit(
            &registry,
            coord_session_id,
            crate::session::operator_touch::KIND_IDLE_AT_PROMPT,
            Some(session.pinned_session_id()),
            Some(&tid),
        ) {
            Ok(_) => {
                let mut guard = LAST_FIRED_SINCE_MS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                guard.get_or_insert_with(HashMap::new).insert(
                    tid,
                    FiredLatch {
                        since_ms,
                        busy_ticks: 0,
                    },
                );
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

/// One tick of a terminal's fired-episode latch. `true` iff this tick ENDS
/// the episode the latch fired for, in which case the latch is released
/// (`*latch = None`). Pure.
///
/// Only `Busy` ends an episode, and only on [`EPISODE_END_BUSY_TICKS`]
/// CONSECUTIVE ticks. An `Idle` reading — even with a new `since_ms` —
/// never does: `GridIdleTracker` restarts `since_ms` on ANY grid-generation
/// bump (a resize/SIGWINCH redraw, a status-line refresh, hook output) while
/// the pane still sits at the prompt, so a changed `since_ms` is not evidence
/// the wait ended. A genuinely new idle episode is covered by the backstop
/// when it fires. `Idle` and `Unknown` (the grid could not be read) both
/// break a busy streak — the streak must be consecutive evidence.
fn idle_episode_step(
    latch: &mut Option<FiredLatch>,
    state: &qontinui_runner_lib::wind_down::GridIdle,
) -> bool {
    use qontinui_runner_lib::wind_down::GridIdle;
    let Some(l) = latch.as_mut() else {
        return false;
    };
    match state {
        GridIdle::Busy => {
            l.busy_ticks += 1;
            if l.busy_ticks >= EPISODE_END_BUSY_TICKS {
                *latch = None;
                return true;
            }
            false
        }
        GridIdle::Idle { .. } | GridIdle::Unknown => {
            l.busy_ticks = 0;
            false
        }
    }
}

/// Should this tick emit a new `idle_at_prompt` touch for an idle episode
/// starting at `since_ms` that has crossed the threshold? Not if the latch
/// already fired for this exact episode, and not while the terminal holds a
/// live idle open — a repaint restarts `since_ms` while the operator is still
/// on the same wait, and counting it again would turn one wait into several
/// touches. Pure.
fn should_fire_idle_touch(latch: Option<FiredLatch>, since_ms: i64, holds_live_open: bool) -> bool {
    if latch.is_some_and(|l| l.since_ms == since_ms) {
        return false;
    }
    !holds_live_open
}

/// `src` with every `//` comment removed, line by line — so a source guard's
/// substring match cannot be satisfied by a commented-out call. Crude by
/// design: it also cuts at a `//` inside a string literal, and it leaves
/// `/* */` block comments in place.
#[cfg(test)]
pub(crate) fn code_only(src: &str) -> String {
    src.lines()
        .map(|l| l.split_once("//").map_or(l, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
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
        Some(session.terminal_id()),
    ) {
        Ok(_) => done(
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

    fn fired(since_ms: i64) -> Option<FiredLatch> {
        Some(FiredLatch {
            since_ms,
            busy_ticks: 0,
        })
    }

    /// A redraw at the prompt (`GridIdleTracker` restarts `since_ms` on any
    /// generation bump) is not the end of the wait.
    #[test]
    fn a_repaint_that_stays_idle_ends_nothing() {
        use qontinui_runner_lib::wind_down::GridIdle;
        let mut latch = fired(100);
        for since_ms in [100, 250, 400] {
            assert!(!idle_episode_step(&mut latch, &GridIdle::Idle { since_ms }));
        }
        assert_eq!(latch, fired(100), "the latch is untouched");
    }

    #[test]
    fn one_busy_tick_ends_nothing() {
        use qontinui_runner_lib::wind_down::GridIdle;
        let mut latch = fired(100);
        assert!(!idle_episode_step(&mut latch, &GridIdle::Busy));
        assert!(latch.is_some());
        // A busy streak must be consecutive: Idle or Unknown resets it.
        assert!(!idle_episode_step(
            &mut latch,
            &GridIdle::Idle { since_ms: 500 }
        ));
        assert!(!idle_episode_step(&mut latch, &GridIdle::Busy));
        assert!(!idle_episode_step(&mut latch, &GridIdle::Unknown));
        assert!(!idle_episode_step(&mut latch, &GridIdle::Busy));
        assert!(latch.is_some());
    }

    /// Two consecutive Busy ticks end the episode — and the close that
    /// follows (`close_idle_episode`'s decision) is `self_resolved` / `none`.
    #[test]
    fn two_consecutive_busy_ticks_end_the_episode_and_self_resolve_its_open() {
        use crate::session::operator_touch_close::{
            idle_episode_end_closes, CloseActorClass, OpenTouch, OpenTouchStore, Resolution,
        };
        use qontinui_runner_lib::wind_down::GridIdle;
        let mut latch = fired(100);
        assert!(!idle_episode_step(&mut latch, &GridIdle::Busy));
        assert!(idle_episode_step(&mut latch, &GridIdle::Busy));
        assert!(latch.is_none(), "the latch is released");
        assert!(
            !idle_episode_step(&mut latch, &GridIdle::Busy),
            "no latch, nothing to end"
        );

        let sid = uuid::Uuid::new_v4();
        let kind = crate::session::operator_touch::KIND_IDLE_AT_PROMPT;
        let store = OpenTouchStore::in_memory();
        store
            .remember(OpenTouch {
                terminal_id: "t1".to_string(),
                kind: kind.to_string(),
                coord_session_id: sid,
                idempotency_key: crate::session::operator_touch::idempotency_key(sid, kind, 60),
                open_payload: crate::session::operator_touch::touch_payload(kind, sid, None, 60),
                open_recorded_at: chrono::Utc::now(),
                open_confirmed: true,
                failed_close: None,
                tenant_id: None,
                reloaded: false,
            })
            .unwrap();
        let closes = idle_episode_end_closes(&store, "t1");
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0].resolution, Resolution::SelfResolved);
        assert_eq!(closes[0].actor_class, Some(CloseActorClass::None));
    }

    fn idle_open(sid: uuid::Uuid) -> crate::session::operator_touch_close::OpenTouch {
        let kind = crate::session::operator_touch::KIND_IDLE_AT_PROMPT;
        crate::session::operator_touch_close::OpenTouch {
            terminal_id: "t1".to_string(),
            kind: kind.to_string(),
            coord_session_id: sid,
            idempotency_key: crate::session::operator_touch::idempotency_key(sid, kind, 60),
            open_payload: crate::session::operator_touch::touch_payload(kind, sid, None, 60),
            open_recorded_at: chrono::Utc::now(),
            open_confirmed: true,
            failed_close: None,
            tenant_id: None,
            reloaded: false,
        }
    }

    /// One wait is one touch: a repaint 60 s+ later restarts `since_ms`, but
    /// the held open suppresses a second touch.
    #[test]
    fn a_repaint_with_an_idle_open_held_emits_no_second_touch() {
        use crate::session::operator_touch_close::OpenTouchStore;
        let store = OpenTouchStore::in_memory();
        store.remember(idle_open(uuid::Uuid::new_v4())).unwrap();
        let latch = fired(100);
        assert!(!should_fire_idle_touch(
            latch,
            100,
            store.holds_live_idle_open("t1")
        ));
        assert!(
            !should_fire_idle_touch(latch, 70_000, store.holds_live_idle_open("t1")),
            "a restarted since_ms is still the same wait"
        );
    }

    #[test]
    fn after_a_two_busy_tick_end_the_next_idle_episode_emits_a_new_touch() {
        use crate::session::operator_touch_close::{idle_episode_end_closes, OpenTouchStore};
        use qontinui_runner_lib::wind_down::GridIdle;
        let store = OpenTouchStore::in_memory();
        store.remember(idle_open(uuid::Uuid::new_v4())).unwrap();
        let mut latch = fired(100);
        assert!(!idle_episode_step(&mut latch, &GridIdle::Busy));
        assert!(idle_episode_step(&mut latch, &GridIdle::Busy));
        assert_eq!(idle_episode_end_closes(&store, "t1").len(), 1);
        assert!(should_fire_idle_touch(
            latch,
            70_000,
            store.holds_live_idle_open("t1")
        ));
    }

    #[test]
    fn after_an_input_close_the_next_idle_episode_emits_a_new_touch() {
        use crate::session::operator_touch_close::{closes_for_input, OpenTouchStore};
        let store = OpenTouchStore::in_memory();
        store.remember(idle_open(uuid::Uuid::new_v4())).unwrap();
        let latch = fired(100); // an input does not release the latch
        let closed = closes_for_input(
            &store,
            "t1",
            &crate::terminal::session::PtyWriteCaller::TauriTerminalWrite,
        );
        assert_eq!(closed.len(), 1);
        assert!(should_fire_idle_touch(
            latch,
            70_000,
            store.holds_live_idle_open("t1")
        ));
        assert!(
            !should_fire_idle_touch(latch, 100, false),
            "never twice for the exact episode the latch fired for"
        );
    }

    /// SOURCE GUARD: the tick is the only production caller of the orphan
    /// close, the episode-end close and its backstop, and it needs a Tauri
    /// app to run — so deleting a call would otherwise pass every test.
    #[test]
    fn the_tick_wires_every_close_signal_it_owns() {
        let src = code_only(include_str!("operator_touch_watch.rs"));
        let tick = src
            .split_once("pub fn scan_idle_touches_once()")
            .expect("the tick exists")
            .1
            .split_once("\nfn idle_episode_step")
            .expect("the tick ends before the episode step")
            .0;
        assert!(tick.contains("open_touches().terminal_ids()"));
        assert!(tick.contains("operator_touch_close::close_orphaned("));
        assert!(tick.contains("idle_episode_step(&mut latch, &state)"));
        assert!(tick.contains("should_fire_idle_touch(latch, since_ms, holds_live_open)"));
        assert!(
            tick.find("should_fire_idle_touch(").unwrap()
                < tick
                    .rfind("operator_touch_close::close_idle_episode(")
                    .unwrap(),
            "the one-wait-one-touch suppression runs before the backstop"
        );
        assert_eq!(
            tick.matches("operator_touch_close::close_idle_episode(")
                .count(),
            2,
            "episode end + new-episode backstop"
        );
        assert!(
            tick.find("open_touches().terminal_ids()").unwrap()
                < tick.find("tm.sessions_snapshot()").unwrap(),
            "candidates must be read BEFORE the live snapshot"
        );
        let hook = src
            .split_once("pub fn on_notification_signal(")
            .expect("the hook exists")
            .1;
        assert!(
            hook.contains("Some(session.terminal_id())"),
            "the permission prompt is tracked on its terminal"
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
