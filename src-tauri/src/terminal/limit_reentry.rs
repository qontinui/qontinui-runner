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
//! A re-entry is not the end of a stop: [`take_reentries`] leaves a retry park
//! [`REENTRY_RETRY_MS`] out, which the handler's own decision replaces (a new
//! park) or removes (a continue that reached the pane, or a session no longer
//! stopped). A handler that returns early, or skips on an unreadable
//! transcript, leaves it, so a known stop is never dropped.
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
//! (43 of 103 restored panes on 2026-11-04, plan §Why item 4), and a restart: the park list is in
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

/// When a dispatched re-entry is looked at again if the handler neither
/// parks nor answers it.
pub(crate) const REENTRY_RETRY_MS: i64 = super::limit_reset::PARK_WITHOUT_RESET_MS;

/// A first-sight read that is UNKNOWN is retried on later sweeps, up to this
/// many reads in all, before the terminal counts as read.
const FIRST_SIGHT_UNREADABLE_TRIES: u32 = 3;

/// A live terminal with no confirmed open record (a plain shell, or a session
/// not yet bound) is looked up again this long after a sweep found none.
const NO_RECORD_RECHECK_MS: i64 = 5 * 60_000;

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
/// due first. The rest stay parked and due. Each one taken leaves a retry park
/// [`REENTRY_RETRY_MS`] out (see the module docs).
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
    for park in &due {
        parks.enrol(
            &park.terminal_id,
            &park.claude_session_id,
            now_ms + REENTRY_RETRY_MS,
        );
    }
    due
}

// ── First sight ──────────────────────────────────────────────────────────────

/// The terminals already read once in this process.
#[derive(Debug, Default)]
pub(crate) struct FirstSight {
    read: HashSet<String>,
    /// UNKNOWN reads so far of a terminal not yet counted as read.
    unreadable_tries: std::collections::HashMap<String, u32>,
    /// Terminals a sweep found no confirmed record for, and when to look
    /// again ([`NO_RECORD_RECHECK_MS`]).
    no_record_until_ms: std::collections::HashMap<String, i64>,
}

impl FirstSight {
    /// The live terminals not yet read and not waiting out a no-record
    /// recheck at `now_ms`, in `live`'s order.
    pub(crate) fn unread(&self, live: &[String], now_ms: i64) -> Vec<String> {
        live.iter()
            .filter(|t| !self.read.contains(*t))
            .filter(|t| self.no_record_until_ms.get(*t).is_none_or(|&u| u <= now_ms))
            .cloned()
            .collect()
    }

    /// Forget terminals that are gone, like the scan gate does.
    pub(crate) fn retain_live(&mut self, live: &HashSet<&str>) {
        self.read.retain(|t| live.contains(t.as_str()));
        self.unreadable_tries.retain(|t, _| live.contains(t.as_str()));
        self.no_record_until_ms.retain(|t, _| live.contains(t.as_str()));
    }
}

/// What one first-sight sweep found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FirstSightPass {
    /// Terminals with a confirmed open record whose transcript was read and
    /// answered (`Stopped` or `NotStopped`). A terminal with no confirmed
    /// record yet is not here, so a later sweep reads it once its session is
    /// bound.
    pub read: Vec<String>,
    /// `(terminal_id, claude_session_id)` of each read that is `Stopped`.
    pub stopped: Vec<(String, String)>,
    /// Terminals whose read was UNKNOWN (`Unreadable`). Not enrolled, and
    /// read again on later sweeps ([`FIRST_SIGHT_UNREADABLE_TRIES`]); the
    /// hint path also reads such a pane when its screen matches.
    pub unreadable: Vec<String>,
    /// Terminals with no confirmed open record yet: looked up again after
    /// [`NO_RECORD_RECHECK_MS`].
    pub no_record: Vec<String>,
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
            pass.no_record.push(terminal_id.clone());
            continue;
        };
        match read_stop(&record) {
            LimitStopRead::Stopped(_) => pass
                .stopped
                .push((terminal_id.clone(), session_id(&record))),
            LimitStopRead::NotStopped => {}
            LimitStopRead::Unreadable(_) => {
                pass.unreadable.push(terminal_id.clone());
                continue;
            }
        }
        pass.read.push(terminal_id.clone());
    }
    pass
}

/// Record a sweep: its answered terminals are read, an UNKNOWN one counts as
/// read only after [`FIRST_SIGHT_UNREADABLE_TRIES`] reads, and each stopped
/// one is enrolled as due at `now_ms` unless it is already parked. Returns how
/// many were enrolled.
pub(crate) fn apply_first_sight(
    seen: &mut FirstSight,
    parks: &mut ParkList,
    pass: &FirstSightPass,
    now_ms: i64,
) -> usize {
    for terminal_id in &pass.no_record {
        seen.no_record_until_ms
            .insert(terminal_id.clone(), now_ms + NO_RECORD_RECHECK_MS);
    }
    for terminal_id in &pass.read {
        seen.no_record_until_ms.remove(terminal_id);
        seen.unreadable_tries.remove(terminal_id);
        seen.read.insert(terminal_id.clone());
    }
    for terminal_id in &pass.unreadable {
        let tries = seen.unreadable_tries.entry(terminal_id.clone()).or_default();
        *tries += 1;
        if *tries >= FIRST_SIGHT_UNREADABLE_TRIES {
            seen.unreadable_tries.remove(terminal_id);
            seen.read.insert(terminal_id.clone());
        }
    }
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
    // Lock orders: the park list, then the pacer (here); the first-sight
    // record, then the park list ([`run_first_sight`]). Nothing takes the
    // park list and then the first-sight record.
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
        let candidates = seen.unread(live, chrono::Utc::now().timestamp_millis());
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
        // Confirmed only: the spawn-time identity seam writes a provisional
        // row for every terminal, and its minted id is not always the id the
        // session runs under, so its transcript would not be there yet.
        |terminal_id| store.find_confirmed_open_by_terminal(terminal_id),
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
    if !pass.unreadable.is_empty() {
        warn!(
            unreadable = pass.unreadable.len(),
            "first-sight transcript check: some transcripts could not be read — left to the screen hint"
        );
    }
    if pass.read.is_empty() && pass.unreadable.is_empty() {
        return;
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
