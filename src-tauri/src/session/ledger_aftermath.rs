//! The session ledger's **aftermath record**: what came back, closed and
//! finished after each restart, kept per observing boot and kept after the
//! registry prunes the row (plan
//! `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`,
//! Phase 2).
//!
//! ## Why this exists
//!
//! The ledger ([`crate::session::session_ledger`]) answers "which of the
//! sessions open before the restart came back?" from the registry as it stands
//! when the report is read: a row still open on a live pane, or a row carrying
//! this boot's restore stamp (`session_ledger::observed_back`). Every other way
//! a session comes back (the PTY-holder adoption, the usage-limit migration's
//! respawn, the resume door, a hand resume) leaves no stamp. Such a session read
//! `missing` as soon as its row closed or pointed anywhere but a live pane, and
//! every overlay read rows the registry prunes 24 h after they close. On
//! 2026-11-11 that reported 43 sessions that came back, and were closed a day
//! later, as "did not come back".
//!
//! ## What it records
//!
//! For each session on the PRIOR roster (the ledger this boot latched, keyed by
//! lowercased id):
//!
//! - **`came_back_at` / `came_back_by`**, set once, at the first observation:
//!   - `bound`: the liveness poll `KeepAlive`d the row on its OWN terminal (a
//!     [`PollTerminalMatch::Direct`] match). A `(page, title, cwd)` fallback
//!     match records nothing: it "does not make it the row's terminal", and a
//!     sticky record would make that misattribution permanent;
//!   - `stamp`: this boot's restore stamp (`restored_from_boot_at`), once the
//!     restore LANDED — its pending marker cleared and its tier promoted to
//!     `resumed`. `mark_restore_pending` writes the stamp the moment a resume is
//!     ATTEMPTED (tier `failed`, marker set), and a close demotes an unconfirmed
//!     `failed` to `terminal-only`, so a bare stamp is an attempt, not a return;
//!     latching it would record a restore that died as back for good;
//!   - `process`: a live `claude` holds the session id in Claude Code's own
//!     `<config_dir>/sessions/<pid>.json` — the registry read the poll's
//!     `live_identity` already makes ([`read_live_claude_sessions`]). That
//!     evidence does not depend on the row, its terminal or its account.
//! - **`closed_at` / `close_reason`** and **`finished_at`**, mirrored from the
//!   registry while the row exists (a re-opened row clears its close, an
//!   unfinished one its finish) and KEPT once the registry prunes the row.
//!
//! An entry exists for every prior-roster id whose row this boot saw in the
//! registry, even when nothing else is recorded about it, so "neither the
//! registry nor any record has it" is something the ledger can state rather
//! than guess.
//!
//! ## Fed by the liveness poll, at two points (`main.rs`)
//!
//! - (a) right after the ledger capture, before either prune:
//!   [`LedgerAftermath::observe_registry`] over `all_records()`;
//! - (b) after the classify loop, beside the observation epoch's `record_tick`:
//!   [`LedgerAftermath::observe_tick`], with the ids the loop `KeepAlive`d (and
//!   how each matched) and the tick's live-session read. An idle tick (no open
//!   row to classify) takes a process snapshot only while some prior-roster id
//!   is still unobserved, unfinished and unclosed
//!   ([`LedgerAftermath::observe_idle_tick`]); once none is, the idle path stays
//!   snapshot-free.
//!
//! A failed read records nothing and leaves that tick UNKNOWN. A PARTIAL read
//! (a `sessions/` dir it could not list, a live pid's record it could not
//! read) records the sessions it did find and also leaves the tick UNKNOWN,
//! because the ones it could not see may be live. A tick that skipped the read
//! by design is not UNKNOWN ([`ProcessEvidence`]).
//!
//! ## One record per observing boot
//!
//! `session-ledger-aftermath.<boot_ms>.json` beside the ledger, instance-scoped
//! through [`session_ledger::ledger_path`], naming the generation file the boot
//! observes. It is keyed by BOOT, not by file: `rotate_in` declines to duplicate
//! an unchanged roster, so two boots can observe one generation file, and each
//! keeps its own record. A record is retained while the generation it names is
//! retained and pruned with the generations; it is written atomically and only
//! when it changes; and it is fail-soft: a write failure logs and never breaks
//! the poll.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::process_capture::process_tree::ProcessSnapshot;
use crate::session::claude_session_registry::{
    live_pids_from_snapshot, may_host_claude, read_live_sessions, LiveSessions,
};
use crate::session::session_ledger::{self, SessionLedger, LEDGER_GENERATIONS_KEPT};
use crate::session::session_lifecycle_store::{
    PollTerminalMatch, TerminalSessionRecord, RESTORE_TIER_RESUMED,
};

/// Bumped only on a shape change a reader must branch on. A record of another
/// version is never half-parsed: it is not read, and it is pruned.
pub const AFTERMATH_VERSION: u32 = 1;

/// The liveness poll's tick interval: `main.rs` sleeps this long between
/// ticks. It lives here because [`PROCESS_EVIDENCE_MAX_AGE_MS`] is measured in
/// ticks, and the two must not drift apart.
pub const LIVENESS_POLL_INTERVAL_SECS: u64 = 45;

/// The oldest a tick's process read may be and still say which sessions a live
/// `claude` holds NOW: two tick intervals. One slow tick does not turn the
/// evidence UNKNOWN; a stalled poll does.
pub const PROCESS_EVIDENCE_MAX_AGE_MS: i64 = 2 * (LIVENESS_POLL_INTERVAL_SECS as i64) * 1_000;

/// At most this many records are retained per generation file, newest boot
/// first. Two boots name one file only when the roster did not change between
/// them, so a run of such boots is the only way past one record per file, and
/// this bounds it.
pub const RECORDS_KEPT_PER_GENERATION: usize = LEDGER_GENERATIONS_KEPT;

/// The close reason recorded for a closed row that carries none.
pub const UNRECORDED_CLOSE_REASON: &str = "unrecorded";

const FILE_PREFIX: &str = "session-ledger-aftermath.";
const FILE_SUFFIX: &str = ".json";

/// `session-ledger-aftermath.<boot_ms>.json`.
pub fn aftermath_file_name(boot_ms: i64) -> String {
    format!("{FILE_PREFIX}{boot_ms}{FILE_SUFFIX}")
}

/// `session-ledger-aftermath.<digits>.json` → `<digits>`. Anything else (a
/// generation, the live ledger, an atomic-write temp file) is not a record.
fn parse_aftermath_file_name(name: &str) -> Option<i64> {
    let ts = name.strip_prefix(FILE_PREFIX)?.strip_suffix(FILE_SUFFIX)?;
    if ts.is_empty() || !ts.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    ts.parse().ok()
}

// ---------------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------------

/// How a session was first observed alive this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CameBackBy {
    /// The poll `KeepAlive`d its row on the row's own terminal.
    Bound,
    /// This boot's restore stamped it (`restored_from_boot_at`) and the
    /// restore landed ([`restore_landed`]).
    Stamp,
    /// A live `claude` held its id in Claude Code's own session registry.
    Process,
}

/// What one boot observed about one prior-roster session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAftermath {
    /// Unix millis of the FIRST observation that it came back; never moves
    /// once set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub came_back_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub came_back_by: Option<CameBackBy>,
    /// When the row closed, as the registry last showed it. Absent while the
    /// row was open, and for a closed row that carries no `closed_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    /// The stored close reason, verbatim. Present iff the registry last showed
    /// the row closed ([`UNRECORDED_CLOSE_REASON`] when it carried none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    /// When it was marked finished, as the registry last showed it. Absent
    /// while it was unfinished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}

impl SessionAftermath {
    /// Record that it came back at `at_ms`, by `by` — only the first time.
    /// Returns whether anything changed.
    fn came_back(&mut self, at_ms: i64, by: CameBackBy) -> bool {
        if self.came_back_at.is_some() {
            return false;
        }
        self.came_back_at = Some(at_ms);
        self.came_back_by = Some(by);
        true
    }

    /// The recorded close, if the registry last showed the row closed.
    pub fn close(&self) -> Option<Close> {
        self.close_reason.as_ref().map(|reason| Close {
            at_ms: self.closed_at,
            reason: reason.clone(),
        })
    }

    /// Nothing is recorded that would end a process read's interest in it: it
    /// has not been seen back, and the registry last showed it neither
    /// finished nor closed.
    fn unsettled(&self) -> bool {
        self.came_back_at.is_none() && self.finished_at.is_none() && self.close_reason.is_none()
    }
}

/// A row's close: when, and the reason as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Close {
    /// Unix millis. `None` only for a closed row that carries no `closed_at`.
    pub at_ms: Option<i64>,
    /// The stored wire string, verbatim ([`UNRECORDED_CLOSE_REASON`] when the
    /// row carries none).
    pub reason: String,
}

impl Close {
    /// The close of `r`, when it is closed.
    pub fn of_record(r: &TerminalSessionRecord) -> Option<Self> {
        (r.state == "closed").then(|| Self {
            at_ms: r.closed_at,
            reason: r
                .close_reason
                .clone()
                .unwrap_or_else(|| UNRECORDED_CLOSE_REASON.to_string()),
        })
    }
}

/// One boot's record, as persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AftermathRecord {
    pub aftermath_version: u32,
    /// Unix millis at which the observing process booted.
    pub boot_ms: i64,
    /// The generation file this boot observes: the one its boot retained the
    /// prior ledger into. `None` when it retained none.
    pub generation_file: Option<String>,
    /// Per prior-roster session, by lowercased id.
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionAftermath>,
}

/// [`AftermathRecord`]'s wire shape, borrowed, so a write serializes the live
/// map without cloning it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AftermathRecordRef<'a> {
    aftermath_version: u32,
    boot_ms: i64,
    generation_file: Option<&'a str>,
    sessions: &'a BTreeMap<String, SessionAftermath>,
}

// ---------------------------------------------------------------------------
// Process evidence
// ---------------------------------------------------------------------------

/// One tick's live-session read, as the poll hands it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessRead {
    /// The read succeeded in full; these session ids were held by a live
    /// `claude`, and no other was.
    Held(Vec<String>),
    /// The read succeeded only in PART ([`LiveSessions::is_complete`] is
    /// false: a `sessions/` dir could not be listed, or a live pid's record
    /// could not be read). These ids WERE held by a live `claude`, so each is
    /// recorded as back; a session missing from them may be held too, so who
    /// holds a session now is UNKNOWN this tick.
    Partial(Vec<String>),
    /// The snapshot or the read failed: this tick observed nothing (UNKNOWN).
    Failed,
    /// The tick took no read by design: no prior-roster session was left for a
    /// read to find.
    SkippedByDesign,
}

impl ProcessRead {
    /// The tick's read: the ids it found ([`Self::Partial`] when the read was
    /// incomplete), or [`Self::Failed`] when the read failed or the tick's
    /// process snapshot did, whatever the read returned.
    pub fn of_tick(snapshot_ok: bool, read: &Result<LiveSessions, String>) -> Self {
        match read {
            Ok(read) if snapshot_ok => {
                let ids = read.sessions.iter().map(|s| s.session_id.clone()).collect();
                if read.is_complete() {
                    Self::Held(ids)
                } else {
                    Self::Partial(ids)
                }
            }
            _ => Self::Failed,
        }
    }
}

/// The newest tick's process read, kept in memory: it says who holds a session
/// NOW, which no later boot can use.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessTick {
    at_ms: i64,
    read: TickRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TickRead {
    Held(HashSet<String>),
    Failed,
    SkippedByDesign,
}

/// What the newest poll tick says about which sessions a live `claude` holds,
/// as of a report instant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProcessEvidence {
    /// No tick has read yet, the newest read failed, or it is older than
    /// [`PROCESS_EVIDENCE_MAX_AGE_MS`]. Nothing may be concluded from an
    /// absence.
    #[default]
    Unknown,
    /// The newest tick skipped the read by design: no prior-roster session was
    /// left for it to find. Never aged into [`Self::Unknown`].
    NotNeeded,
    /// The newest tick's read, at most two tick intervals old: the session ids
    /// a live `claude` held, lowercased.
    Held(HashSet<String>),
}

fn process_evidence(tick: Option<&ProcessTick>, now_ms: i64) -> ProcessEvidence {
    let Some(tick) = tick else {
        return ProcessEvidence::Unknown;
    };
    match &tick.read {
        TickRead::SkippedByDesign => ProcessEvidence::NotNeeded,
        TickRead::Failed => ProcessEvidence::Unknown,
        TickRead::Held(ids) if now_ms.saturating_sub(tick.at_ms) <= PROCESS_EVIDENCE_MAX_AGE_MS => {
            ProcessEvidence::Held(ids.clone())
        }
        TickRead::Held(_) => ProcessEvidence::Unknown,
    }
}

/// THE poll's live-session read: Claude Code's own `sessions/<pid>.json` for
/// every pid alive in `snap`, failing closed on an indeterminate snapshot
/// ([`live_pids_from_snapshot`]). The one read both the poll's `live_identity`
/// and this record's process evidence come from — never a second one.
/// `config_dirs` is only called when the snapshot is usable. An `Ok` read may
/// still be incomplete ([`LiveSessions::is_complete`]): the poll's identity
/// refresh uses its rows anyway, and [`ProcessRead::of_tick`] reads it as
/// [`ProcessRead::Partial`].
pub fn read_live_claude_sessions(
    snap: &ProcessSnapshot,
    config_dirs: impl FnOnce() -> Vec<PathBuf>,
) -> Result<LiveSessions, String> {
    let live_pids = live_pids_from_snapshot(snap)?;
    Ok(read_live_sessions(
        &config_dirs(),
        &live_pids,
        &may_host_claude(snap),
    ))
}

/// The instant a tick observed at: when its table's read BEGAN (a lower bound
/// on every row's observation), never later than when it was asked for — the
/// same instant the poll stamps the observation epoch with.
fn snapshot_instant(snap: &ProcessSnapshot, asked_at_ms: i64) -> i64 {
    if snap.observed_at_ms > 0 {
        snap.observed_at_ms.min(asked_at_ms)
    } else {
        asked_at_ms
    }
}

/// What an idle tick did about process evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleTick {
    /// It took a snapshot and read the live sessions.
    Read,
    /// It took a snapshot, and the snapshot or the read failed, in whole or
    /// in part (UNKNOWN).
    Failed,
    /// Nothing was left for a read to find, so it took no snapshot.
    SkippedByDesign,
}

// ---------------------------------------------------------------------------
// One process's record
// ---------------------------------------------------------------------------

/// What the ledger's classification reads from the aftermath, as plain data.
#[derive(Debug, Clone, Default)]
pub struct AftermathInputs {
    /// THIS boot's record (the one it writes). `None` when no record was
    /// opened in this process.
    pub this_boot: Option<AftermathRecord>,
    /// Earlier boots' records, retained on disk, newest boot first.
    pub earlier: Arc<Vec<AftermathRecord>>,
    /// The newest poll tick's process evidence, aged at the report instant.
    pub process: ProcessEvidence,
}

/// This process's aftermath record (see the module doc).
#[derive(Debug)]
pub struct LedgerAftermath {
    /// This boot's file.
    path: PathBuf,
    boot_ms: i64,
    generation_file: Option<String>,
    /// The prior roster: lowercased id → finished as captured.
    roster: HashMap<String, bool>,
    /// Earlier boots' records, read once at open: their writers are gone.
    earlier: Arc<Vec<AftermathRecord>>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    sessions: BTreeMap<String, SessionAftermath>,
    /// Something changed since the last write landed.
    dirty: bool,
    /// The bytes of the last write that landed: a write happens only when the
    /// record would change.
    written: Option<Vec<u8>>,
    newest_tick: Option<ProcessTick>,
}

/// The registry's state for one lowercased id, folded over every row carrying
/// it (the store keys rows by the exact id, so case variants are separate
/// rows).
#[derive(Debug, Default)]
struct RowNow {
    open: bool,
    /// The latest close among its closed rows.
    close: Option<Close>,
    finished_at: Option<i64>,
    /// The earliest restore stamp from this boot on a row whose restore
    /// landed ([`restore_landed`]).
    stamped_at: Option<i64>,
}

/// Did this row's boot restore LAND? Only then is its `restored_from_boot_at`
/// evidence that the session came back. `mark_restore_pending(boot = true)`
/// stamps `restored_from_boot_at` together with `restore_pending_at` and tier
/// `failed` BEFORE the resume is typed; only the verified handshake or the
/// poll's `KeepAlive` self-heal (`clear_restore_pending`) clears the marker and
/// promotes `failed` to `resumed`, and a close demotes an unconfirmed `failed`
/// to `terminal-only` (`reap_restore_marker_on_close`). So a stamp with the
/// marker still set, or with any tier but `resumed`, records an attempt, and
/// the record — which outlives the row — must not make it a return.
fn restore_landed(r: &TerminalSessionRecord) -> bool {
    r.restore_pending_at.is_none() && r.restore_tier.as_deref() == Some(RESTORE_TIER_RESUMED)
}

impl LedgerAftermath {
    /// Open this boot's record in `dir` and prune the records that no longer
    /// name a retained generation.
    ///
    /// `generation_file` is the generation this boot retained its prior ledger
    /// into; `prior` is that ledger (its sessions are the roster recorded);
    /// `retained_generation_files` are the generation files still in `dir`.
    pub fn open(
        dir: &Path,
        boot_ms: i64,
        generation_file: Option<String>,
        prior: Option<&SessionLedger>,
        retained_generation_files: &HashSet<String>,
    ) -> Self {
        let roster: HashMap<String, bool> = prior
            .map(|p| {
                p.sessions
                    .iter()
                    .map(|e| (e.claude_session_id.to_ascii_lowercase(), e.finished))
                    .collect()
            })
            .unwrap_or_default();
        let earlier = retain_earlier(dir, boot_ms, retained_generation_files);
        info!(
            boot_ms,
            generation_file = generation_file.as_deref().unwrap_or("<none>"),
            roster = roster.len(),
            earlier_records = earlier.len(),
            "ledger aftermath: opened this boot's record"
        );
        Self {
            path: dir.join(aftermath_file_name(boot_ms)),
            boot_ms,
            generation_file,
            roster,
            earlier: Arc::new(earlier),
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Feed (a), right after the poll's ledger capture and before any prune:
    /// this boot's landed restore stamps, and the close and finish of every
    /// prior-roster row the registry holds. A row the registry no longer holds
    /// keeps what was recorded — the record outlives the prune. Returns whether
    /// a write landed.
    pub fn observe_registry(&self, records: &[TerminalSessionRecord]) -> bool {
        if self.roster.is_empty() {
            return false;
        }
        let mut rows: HashMap<String, RowNow> = HashMap::new();
        for r in records {
            let id = r.claude_session_id.to_ascii_lowercase();
            if !self.roster.contains_key(&id) {
                continue;
            }
            let row = rows.entry(id).or_default();
            match Close::of_record(r) {
                None => row.open = true,
                Some(c) => {
                    if row.close.as_ref().is_none_or(|cur| c.at_ms > cur.at_ms) {
                        row.close = Some(c);
                    }
                }
            }
            row.finished_at = row.finished_at.max(r.finished_at);
            if let Some(at) = r
                .restored_from_boot_at
                .filter(|at| *at >= self.boot_ms && restore_landed(r))
            {
                row.stamped_at = Some(row.stamped_at.map_or(at, |cur| cur.min(at)));
            }
        }

        let mut st = self.lock();
        let mut changed = false;
        for (id, row) in rows {
            let mut next = st.sessions.get(&id).cloned().unwrap_or_default();
            if let Some(at) = row.stamped_at {
                next.came_back(at, CameBackBy::Stamp);
            }
            let close = if row.open { None } else { row.close };
            next.closed_at = close.as_ref().and_then(|c| c.at_ms);
            next.close_reason = close.map(|c| c.reason);
            next.finished_at = row.finished_at;
            if st.sessions.get(&id) != Some(&next) {
                st.sessions.insert(id, next);
                changed = true;
            }
        }
        st.dirty |= changed;
        self.persist_if_dirty(&mut st)
    }

    /// Feed (b), after the poll's classify loop: the ids it `KeepAlive`d this
    /// tick, with how each matched its terminal (only a
    /// [`PollTerminalMatch::Direct`] match records `bound`), and the tick's
    /// live-session read. A failed read records nothing and makes the tick
    /// UNKNOWN. Returns whether a write landed.
    pub fn observe_tick(
        &self,
        at_ms: i64,
        kept_alive: &[(String, PollTerminalMatch)],
        process: ProcessRead,
    ) -> bool {
        let mut st = self.lock();
        let mut changed = false;
        for (id, matched) in kept_alive {
            if *matched != PollTerminalMatch::Direct {
                continue;
            }
            let id = id.to_ascii_lowercase();
            if self.roster.contains_key(&id) {
                changed |= st
                    .sessions
                    .entry(id)
                    .or_default()
                    .came_back(at_ms, CameBackBy::Bound);
            }
        }
        let mut record_held = |st: &mut State, ids: &[String]| {
            let held: HashSet<String> = ids.iter().map(|s| s.to_ascii_lowercase()).collect();
            for id in held.iter().filter(|id| self.roster.contains_key(*id)) {
                changed |= st
                    .sessions
                    .entry(id.clone())
                    .or_default()
                    .came_back(at_ms, CameBackBy::Process);
            }
            held
        };
        let read = match process {
            ProcessRead::Held(ids) => TickRead::Held(record_held(&mut st, &ids)),
            // What a partial read found is proof; what it did not find is not.
            ProcessRead::Partial(ids) => {
                record_held(&mut st, &ids);
                TickRead::Failed
            }
            ProcessRead::Failed => TickRead::Failed,
            ProcessRead::SkippedByDesign => TickRead::SkippedByDesign,
        };
        st.newest_tick = Some(ProcessTick { at_ms, read });
        st.dirty |= changed;
        self.persist_if_dirty(&mut st)
    }

    /// Does an idle tick still need a process read? Only while some
    /// prior-roster session is unobserved, unfinished and unclosed: a read
    /// could still find it. A roster id whose row this boot never saw counts
    /// by its captured finish alone, so a session that is genuinely lost keeps
    /// the read armed for the whole boot.
    pub fn wants_process_read(&self) -> bool {
        let st = self.lock();
        self.roster
            .iter()
            .any(|(id, captured_finished)| match st.sessions.get(id) {
                Some(e) => e.unsettled(),
                None => !captured_finished,
            })
    }

    /// The idle tick's process evidence (`main.rs`, the poll's idle path): a
    /// snapshot and a live-session read while [`Self::wants_process_read`],
    /// recorded like any tick's; otherwise no snapshot at all, recorded as
    /// skipped by design. `take_snapshot` and `config_dirs` are called only
    /// when a read is wanted.
    pub async fn observe_idle_tick<S, Fut>(
        &self,
        take_snapshot: S,
        config_dirs: impl FnOnce() -> Vec<PathBuf>,
    ) -> IdleTick
    where
        S: FnOnce() -> Fut,
        Fut: Future<Output = ProcessSnapshot>,
    {
        if !self.wants_process_read() {
            self.observe_tick(
                chrono::Utc::now().timestamp_millis(),
                &[],
                ProcessRead::SkippedByDesign,
            );
            return IdleTick::SkippedByDesign;
        }
        let asked_at_ms = chrono::Utc::now().timestamp_millis();
        let snap = take_snapshot().await;
        let read = read_live_claude_sessions(&snap, config_dirs);
        if let Err(e) = &read {
            debug!(
                error = %e,
                "ledger aftermath: the idle tick's live-session read is indeterminate — UNKNOWN this tick"
            );
        }
        let process = ProcessRead::of_tick(!snap.is_unknown(), &read);
        let outcome = match process {
            ProcessRead::Held(_) => IdleTick::Read,
            ProcessRead::Partial(_) | ProcessRead::Failed => IdleTick::Failed,
            ProcessRead::SkippedByDesign => IdleTick::SkippedByDesign,
        };
        self.observe_tick(snapshot_instant(&snap, asked_at_ms), &[], process);
        outcome
    }

    /// Everything the ledger's classification reads, as of `now_ms`.
    pub fn inputs(&self, now_ms: i64) -> AftermathInputs {
        let st = self.lock();
        AftermathInputs {
            this_boot: Some(AftermathRecord {
                aftermath_version: AFTERMATH_VERSION,
                boot_ms: self.boot_ms,
                generation_file: self.generation_file.clone(),
                sessions: st.sessions.clone(),
            }),
            earlier: Arc::clone(&self.earlier),
            process: process_evidence(st.newest_tick.as_ref(), now_ms),
        }
    }

    /// Write the record when something changed and its bytes would differ.
    /// Fail-soft: a failure logs, leaves the record dirty (the next feed
    /// retries) and returns `false`.
    fn persist_if_dirty(&self, st: &mut State) -> bool {
        if !st.dirty {
            return false;
        }
        let record = AftermathRecordRef {
            aftermath_version: AFTERMATH_VERSION,
            boot_ms: self.boot_ms,
            generation_file: self.generation_file.as_deref(),
            sessions: &st.sessions,
        };
        let bytes = match serde_json::to_vec_pretty(&record) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "ledger aftermath: serialize failed — not persisted");
                return false;
            }
        };
        if st.written.as_deref() == Some(bytes.as_slice()) {
            st.dirty = false;
            return false;
        }
        if let Err(e) = write_record(&self.path, &bytes) {
            warn!(
                error = %e,
                path = %self.path.display(),
                "ledger aftermath: write failed — kept in memory, retried on the next change"
            );
            return false;
        }
        debug!(
            path = %self.path.display(),
            sessions = st.sessions.len(),
            "ledger aftermath: persisted"
        );
        st.written = Some(bytes);
        st.dirty = false;
        true
    }
}

fn write_record(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::fs_atomic::atomic_write(path, bytes)
}

/// How reading one record file went.
enum RecordFile {
    Ok(AftermathRecord),
    /// Not a record this build can read (corrupt, foreign, another version).
    Unreadable,
    /// The file could not be read at all — possibly transient, so it is kept.
    Io,
}

fn read_record_file(path: &Path) -> RecordFile {
    let Ok(bytes) = std::fs::read(path) else {
        return RecordFile::Io;
    };
    match serde_json::from_slice::<AftermathRecord>(&bytes) {
        Ok(r) if r.aftermath_version == AFTERMATH_VERSION => RecordFile::Ok(r),
        _ => RecordFile::Unreadable,
    }
}

/// Every record file in `dir`, as `(boot_ms, path)`.
fn record_files_in(dir: &Path) -> Vec<(i64, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let boot_ms = parse_aftermath_file_name(name.to_str()?)?;
            Some((boot_ms, e.path()))
        })
        .collect()
}

fn remove_record(path: &Path, why: &str) {
    match std::fs::remove_file(path) {
        Ok(()) => debug!(path = %path.display(), why, "ledger aftermath: pruned a record"),
        Err(e) => warn!(error = %e, path = %path.display(), "ledger aftermath: prune failed"),
    }
}

/// Read the earlier boots' records in `dir`, pruning every one that names no
/// retained generation, is unreadable, or is beyond
/// [`RECORDS_KEPT_PER_GENERATION`] for its generation. This boot's own file is
/// never touched. Newest boot first.
fn retain_earlier(
    dir: &Path,
    boot_ms: i64,
    retained_generation_files: &HashSet<String>,
) -> Vec<AftermathRecord> {
    let mut kept: Vec<(PathBuf, AftermathRecord)> = Vec::new();
    for (stamp, path) in record_files_in(dir) {
        if stamp == boot_ms {
            continue;
        }
        match read_record_file(&path) {
            RecordFile::Ok(rec)
                if rec
                    .generation_file
                    .as_deref()
                    .is_some_and(|g| retained_generation_files.contains(g)) =>
            {
                kept.push((path, rec));
            }
            RecordFile::Ok(_) => remove_record(&path, "names no retained generation"),
            RecordFile::Unreadable => remove_record(&path, "unreadable"),
            RecordFile::Io => {}
        }
    }
    kept.sort_by_key(|(_, rec)| std::cmp::Reverse(rec.boot_ms));
    let mut per_generation: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for (path, rec) in kept {
        let n = per_generation
            .entry(rec.generation_file.clone().unwrap_or_default())
            .or_default();
        *n += 1;
        if *n > RECORDS_KEPT_PER_GENERATION {
            remove_record(&path, "beyond the per-generation cap");
        } else {
            out.push(rec);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The process's instance
// ---------------------------------------------------------------------------

static PROCESS: OnceLock<Arc<LedgerAftermath>> = OnceLock::new();

/// Open THIS process's record, once: in the ledger's directory, for the ledger
/// [`session_ledger::load_prior_once`] latched and the generation it retained.
/// Call from setup right after `load_prior_once`; idempotent.
pub fn open_for_this_boot() -> &'static Arc<LedgerAftermath> {
    PROCESS.get_or_init(|| {
        let prior = session_ledger::load_prior_once();
        let live = session_ledger::ledger_path();
        let dir = live
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Arc::new(LedgerAftermath::open(
            &dir,
            session_ledger::process_boot_ms(),
            session_ledger::this_boot_generation_file_name(),
            prior,
            &session_ledger::generation_file_names_in(&dir),
        ))
    })
}

/// This process's record, once [`open_for_this_boot`] ran.
pub fn process() -> Option<&'static Arc<LedgerAftermath>> {
    PROCESS.get()
}

mod view;
pub use view::{AftermathView, RecordedSession};

#[cfg(test)]
mod tests;
