//! Re-entry at the reset, and the first-sight transcript check (plan
//! `2026-11-05-runner-terminal-pages-collide-crowd-and-overfill-and-limit-stalled-sessions-never-resume`,
//! Phase 3, design decision D5.3).
//!
//! Phase 2 parks a session that stopped at a usage limit and cannot be
//! continued or migrated yet ([`super::limit_stop::park`]). This module drains
//! that park list: on every tick of the usage-limit scan loop
//! ([`super::usage_limit::scan_grids_once`]), after the screen pass, each park
//! whose due time has passed is handed back to
//! [`super::account_migration::handle_usage_limit_hint`] as a
//! [`REENTRY_HINT`] hint. No screen match is needed: the handler reads the
//! transcript, re-probes and decides again (continue, migrate or park). The
//! scan gate is untouched.
//!
//! Re-entries are paced: at most [`LIMIT_REENTRY_PER_MINUTE`] in any 60 s,
//! oldest due first ([`take_reentries`]). 97 sessions parked until one reset
//! re-enter over about 16 minutes instead of 97 turns starting in one tick.
//!
//! **First sight.** Each live terminal bound to an open lifecycle record gets
//! ONE transcript read per process ([`first_sight_pass`]), in its own blocking
//! task so the screen scan is not delayed. A `Stopped` read enrols the
//! terminal as due now ([`super::limit_stop::ParkList::enrol`]), and the
//! handler decides. That covers panes whose screen never matched a pattern
//! (43 of 103 restored panes on 2026-11-04), and a restart: the park list is in
//! memory only, and the next process re-derives every park from the
//! transcripts. The read includes the session's background subagents'
//! transcripts ([`super::limit_stop::transcript_limit_stop`]). A pane first
//! sighted BEFORE its limit is re-checked when its hint fires: the handler
//! reads the same transcripts on every hint
//! ([`super::account_migration::record_limit_stop`]).
//!
//! Both bounds here are this module's own. If a shared spawn pacer
//! (runner#2467) or the machine-budget admission lands, the dispatcher should
//! take that verdict instead; whichever lands second carries the rule.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use super::limit_stop::{LimitStopRead, Park, ParkList};

/// At most this many re-entries are dispatched in any 60 s.
pub(crate) const LIMIT_REENTRY_PER_MINUTE: usize = 6;

/// The pacing window.
const PACE_WINDOW_MS: i64 = 60_000;

/// The `matched_pattern` a re-entry hands the handler (it is logged, not
/// matched).
pub(crate) const REENTRY_HINT: &str = "limit-reset-due";

/// How often a first-sight sweep may start. A sweep reads only terminals not
/// yet read, so after boot most sweeps find nothing to do.
const FIRST_SIGHT_INTERVAL: Duration = Duration::from_secs(15);

// ── Pacing ───────────────────────────────────────────────────────────────────

/// When the last re-entries were dispatched, oldest first.
#[derive(Debug, Default)]
pub(crate) struct Pacer {
    sent_at_ms: VecDeque<i64>,
}

impl Pacer {
    /// How many more re-entries may go at `now_ms`.
    fn room(&mut self, now_ms: i64) -> usize {
        while self
            .sent_at_ms
            .front()
            .is_some_and(|&t| t <= now_ms - PACE_WINDOW_MS)
        {
            self.sent_at_ms.pop_front();
        }
        LIMIT_REENTRY_PER_MINUTE.saturating_sub(self.sent_at_ms.len())
    }

    fn record(&mut self, now_ms: i64, n: usize) {
        self.sent_at_ms.extend(std::iter::repeat_n(now_ms, n));
    }
}

/// The re-entries one tick dispatches: parks of terminals no longer live are
/// dropped, then at most the pacer's room of the due parks are taken, oldest
/// due first. The rest stay parked and due.
pub(crate) fn take_reentries(
    parks: &mut ParkList,
    pacer: &mut Pacer,
    live: &HashSet<&str>,
    now_ms: i64,
) -> Vec<Park> {
    parks.retain_live(live);
    let room = pacer.room(now_ms);
    if room == 0 {
        return Vec::new();
    }
    let due = parks.take_due(now_ms, room);
    pacer.record(now_ms, due.len());
    due
}

// ── First sight ──────────────────────────────────────────────────────────────

/// The terminals already read once in this process.
#[derive(Debug, Default)]
pub(crate) struct FirstSight {
    read: HashSet<String>,
}

impl FirstSight {
    /// The live terminals not yet read, in `live`'s order.
    pub(crate) fn unread(&self, live: &[String]) -> Vec<String> {
        live.iter()
            .filter(|t| !self.read.contains(*t))
            .cloned()
            .collect()
    }

    /// Forget terminals that are gone, like the scan gate does.
    pub(crate) fn retain_live(&mut self, live: &HashSet<&str>) {
        self.read.retain(|t| live.contains(t.as_str()));
    }
}

/// What one first-sight sweep found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FirstSightPass {
    /// Terminals with an open record whose transcript was read (whatever it
    /// said). A terminal with no open record yet is not here, so a later
    /// sweep reads it once its record exists.
    pub read: Vec<String>,
    /// `(terminal_id, claude_session_id)` of each read that is `Stopped`.
    pub stopped: Vec<(String, String)>,
    /// How many reads were UNKNOWN (`Unreadable`). Not enrolled: the hint path
    /// still reads such a pane when its screen matches.
    pub unreadable: usize,
}

/// One first-sight sweep over `candidates`. `open_record` finds a terminal's
/// open lifecycle record; `session_id` and `read_stop` read it. Pure apart
/// from what the closures do, so a test drives it with a fake store and real
/// transcript files.
pub(crate) fn first_sight_pass<R>(
    candidates: &[String],
    open_record: impl Fn(&str) -> Option<R>,
    session_id: impl Fn(&R) -> String,
    read_stop: impl Fn(&R) -> LimitStopRead,
) -> FirstSightPass {
    let mut pass = FirstSightPass::default();
    for terminal_id in candidates {
        let Some(record) = open_record(terminal_id) else {
            continue;
        };
        match read_stop(&record) {
            LimitStopRead::Stopped(_) => pass
                .stopped
                .push((terminal_id.clone(), session_id(&record))),
            LimitStopRead::NotStopped => {}
            LimitStopRead::Unreadable(_) => pass.unreadable += 1,
        }
        pass.read.push(terminal_id.clone());
    }
    pass
}

/// Record a sweep: its terminals are read, and each stopped one is enrolled
/// as due at `now_ms` unless it is already parked. Returns how many were
/// enrolled.
pub(crate) fn apply_first_sight(
    seen: &mut FirstSight,
    parks: &mut ParkList,
    pass: &FirstSightPass,
    now_ms: i64,
) -> usize {
    seen.read.extend(pass.read.iter().cloned());
    pass.stopped
        .iter()
        .filter(|(terminal_id, session)| parks.enrol(terminal_id, session, now_ms))
        .count()
}

// ── Wiring ───────────────────────────────────────────────────────────────────

/// The process's re-entry pacer.
static PACER: Mutex<Option<Pacer>> = Mutex::new(None);

/// The process's first-sight record, and when its last sweep started.
static FIRST_SIGHT: Mutex<Option<(FirstSight, Option<Instant>)>> = Mutex::new(None);

/// A first-sight sweep is running.
static FIRST_SIGHT_RUNNING: AtomicBool = AtomicBool::new(false);

/// Called by the scan loop after each screen pass, with the live terminal
/// ids: dispatch the due re-entries, and start a first-sight sweep when one is
/// owed. Cheap: the sweep's reads run on their own blocking task.
pub(crate) fn after_screen_pass(live: &[String]) {
    let live_set: HashSet<&str> = live.iter().map(String::as_str).collect();
    let now_ms = chrono::Utc::now().timestamp_millis();
    // Lock order: the park list, then the pacer. Nothing else takes both.
    let due = super::limit_stop::with_parks(|parks| {
        let mut pacer = PACER.lock().unwrap_or_else(|e| e.into_inner());
        take_reentries(
            parks,
            pacer.get_or_insert_with(Pacer::default),
            &live_set,
            now_ms,
        )
    });
    for park in due {
        info!(
            terminal_id = %park.terminal_id,
            session = %park.claude_session_id,
            due_at_ms = park.due_at_ms,
            "limit stop due — re-entering the usage-limit handler"
        );
        tauri::async_runtime::spawn(super::account_migration::handle_usage_limit_hint(
            park.terminal_id,
            REENTRY_HINT,
        ));
    }
    start_first_sight(live, &live_set);
}

/// Clears [`FIRST_SIGHT_RUNNING`] when a sweep ends, panicked or not.
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        FIRST_SIGHT_RUNNING.store(false, Ordering::SeqCst);
    }
}

fn start_first_sight(live: &[String], live_set: &HashSet<&str>) {
    let cli = crate::settings::get_ai_settings().claude_cli;
    if !super::limit_reset::hint_wanted(
        cli.auto_migrate_on_token_exhaustion,
        cli.auto_continue_after_limit_reset,
    ) {
        // Nothing would act on a stop; read nothing, and mark nothing read,
        // so a later switch-on still gets its first sight.
        return;
    }
    let candidates = {
        let mut guard = FIRST_SIGHT.lock().unwrap_or_else(|e| e.into_inner());
        let (seen, last_started) = guard.get_or_insert_with(Default::default);
        seen.retain_live(live_set);
        if last_started.is_some_and(|t| t.elapsed() < FIRST_SIGHT_INTERVAL) {
            return;
        }
        let candidates = seen.unread(live);
        if candidates.is_empty() || FIRST_SIGHT_RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        *last_started = Some(Instant::now());
        candidates
    };
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        let _running = RunningGuard;
        run_first_sight(&candidates);
    });
}

/// One sweep, on a blocking thread: read each candidate's transcript and enrol
/// the stopped ones.
fn run_first_sight(candidates: &[String]) {
    use tauri::Manager;
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(store) = app.try_state::<std::sync::Arc<
        crate::session::session_lifecycle_store::SessionLifecycleStore,
    >>() else {
        return;
    };
    let started = Instant::now();
    let pass = first_sight_pass(
        candidates,
        |terminal_id| store.find_open_by_terminal(terminal_id),
        |record| record.claude_session_id.clone(),
        |record| match record
            .config_dir
            .clone()
            .or_else(crate::ai_provider::get_resolved_config_dir)
        {
            Some(src) => super::account_migration::record_limit_stop(record, &src),
            None => LimitStopRead::Unreadable("the session's account is unknown".to_string()),
        },
    );
    let now_ms = chrono::Utc::now().timestamp_millis();
    let enrolled = {
        let mut guard = FIRST_SIGHT.lock().unwrap_or_else(|e| e.into_inner());
        let (seen, _) = guard.get_or_insert_with(Default::default);
        super::limit_stop::with_parks(|parks| apply_first_sight(seen, parks, &pass, now_ms))
    };
    for (terminal_id, session) in &pass.stopped {
        info!(
            terminal_id,
            session, "first-sight transcript check: session stopped at a usage limit — enrolled"
        );
    }
    let elapsed_ms = started.elapsed().as_millis();
    if pass.unreadable > 0 {
        warn!(
            unreadable = pass.unreadable,
            "first-sight transcript check: some transcripts could not be read — left to the screen hint"
        );
    }
    info!(
        candidates = candidates.len(),
        read = pass.read.len(),
        stopped = pass.stopped.len(),
        enrolled,
        elapsed_ms,
        "usage-limit first-sight transcript check"
    );
}

#[cfg(test)]
#[path = "limit_reentry_tests.rs"]
mod tests;
