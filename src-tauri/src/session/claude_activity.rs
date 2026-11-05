//! The ACTIVITY axis of a live `claude` process — *is it working right now?* —
//! as distinct from liveness (*is it running?*) and from the coord WORK axis
//! (*has it declared itself finished?*). Plan
//! `2026-09-29-quiet-is-measured-by-session-existence-and-machine-wide-so-a-24x7-box-never-gets-one`,
//! Phase 6.
//!
//! **Report-only.** `GET /restart-readiness` renders the aggregate as
//! `live_claude.by_activity`; nothing reads it into a verdict. An idle session
//! still dies on a restart, so "idle" here never means "safe to kill".
//!
//! # Two evidence sources, in precedence order
//!
//! 1. **The runner's own pane observation** — for a top-level terminal-hosted
//!    `claude` the runner spawned, the same OSC 9999 sideband + grid-idle
//!    snapshot the wind-down observer already took for this request
//!    ([`crate::session::wind_down_observer::TerminalObservation`]):
//!    - a sideband `working` reported within [`ACTIVE_WINDOW_MS`], or a busy
//!      grid → `working`. An OLDER sideband `working` is not decisive: a pane
//!      that last said "working" hours ago says nothing about now;
//!    - sideband not-working + an idle grid + a census that positively saw NO
//!      child process → `idle`.
//!    - Anything else is not decisive and falls through to the record.
//!
//!    **Child processes never decide `working`.** On this fleet an idle
//!    `claude` routinely holds long-lived children — measured 2026-09-29 on
//!    merytshost: 15 idle processes each holding a ~3-day-old
//!    `coord-mcp-shim.py` stdio child, `shell` sessions holding 10-day
//!    background `bash` loops. Since 2026-10-04 `has_live_children` counts
//!    only WORK children (`tracking_health::child_signals_work`): a stdio MCP
//!    server started with the session no longer counts, so a pane holding
//!    only that is decided `idle` here rather than falling through to the
//!    record, while a background `bash` loop still does. `Some(true)` remains
//!    only a VETO on a pane-idle verdict, and `None` (uncomputable) cannot
//!    vouch for one either.
//! 2. **Claude Code's own per-process record**
//!    `<config-dir>/sessions/<pid>.json` (`status`, `statusUpdatedAt`,
//!    `sessionId`, `cwd`, `procStart`, `startedAt`) — for every process the
//!    pane cannot decide, including every process the runner did not spawn:
//!    - `idle` / `waiting` → `idle`;
//!    - `busy` / `shell` → `working` iff the session's transcript
//!      (`<config-dir>/projects/<encoded cwd>/<sessionId>.jsonl`) carries a
//!      real `user`/`assistant` line within [`ACTIVE_WINDOW_MS`], else
//!      `stale`. The last [`TRANSCRIPT_TAIL_BYTES`] are read and decoded
//!      lossily (an invalid UTF-8 byte costs that line, not the read). When
//!      the transcript cannot be found, or its tail holds no message line
//!      that parses (one tool result longer than the tail can push every
//!      message out of it), the record's own `statusUpdatedAt` age stands in
//!      for the message age — a weaker signal (a `busy` status stamped 40 min
//!      ago by one long tool call reads `stale`), used only because the
//!      stronger one is absent. No `statusUpdatedAt` either is `unknown`;
//!    - a missing, unlisted, unparseable, ambiguous or unrecognised-status
//!      record → `unknown` (`unlisted`: no record in any dir that could be
//!      listed while some `<config-dir>/sessions` exists and could not be —
//!      [`SessionsListing`]), and so is a record that belongs to an EARLIER
//!      process that held the same pid ([`RecordReading::PidReused`]): on Linux the
//!      record's `procStart` is compared with `/proc/<pid>/stat` field 22
//!      (both are the start time in clock ticks since boot — equal on every
//!      record checked on merytshost 2026-09-29); where that cannot be read,
//!      `startedAt` is compared with the process start the census implies
//!      (its snapshot time minus the process age) inside the ONE-SIDED window
//!      [`START_LEAD_MS`] / [`START_LAG_MS`]. Only a stamp EARLIER than the
//!      window is reuse; one LATER than it is [`RecordReading::LateStamp`] —
//!      unknown, because a reused pid's stale record always predates the live
//!      process, so lateness is no evidence of reuse, and it is no proof of
//!      identity either. An empty or non-numeric
//!      `procStart` counts as absent and falls through to that `startedAt`
//!      check. A record neither check can reach is accepted
//!      on its `pid` match alone — the census only lists live `claude`
//!      processes, so a reused pid would also have to be a `claude`.
//!
//! The same reader is the graceful-exit probe's SECOND witness that a pid is a
//! live `claude` (`crate::terminal::graceful_exit::RegistryWitness`), at
//! [`RecordDepth::Identity`]: an npm-installed Claude Code runs as `node …
//! cli.js`, which no image-name check can recognise, but it writes this record
//! all the same.
//!
//! The session record is a Claude Code INTERNAL file, not a published
//! contract. Every way it can fail to answer lands on `unknown` — never on
//! `idle` — so a format change degrades the report to "we cannot tell", never
//! to a confident quiet [policy: `verification-and-evidence`
//! `unknown-must-not-render-as-a-default`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::wind_down_observer::TerminalObservation;
use qontinui_runner_lib::wind_down::{GridIdle, Sideband, SidebandState};

/// A `busy`/`shell` session counts as `working` only when it exchanged a
/// message within this window (30 min, the plan's `--active-minutes` default);
/// a sideband `working` report older than this is not decisive.
pub const ACTIVE_WINDOW_MS: i64 = 30 * 60 * 1000;

/// How much of a transcript's tail is read to find its last message line.
/// Only `busy`/`shell` records the pane could not decide pay it (single digits
/// to low tens on a 250-session box), so it is bounded per request.
pub const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;

/// The window `startedAt - process_start` must fall in for a record to
/// belong to the live process, where `process_start` is the census snapshot
/// time minus the process age. It is ONE-SIDED because Claude Code stamps
/// `startedAt` after its own startup: over 249 records on merytshost
/// (2026-09-29) the lag was always positive, from +0.9 s to +78 s, and 1 in
/// 249 exceeded 60 s. So the lead allows only for `age_s`'s one-second
/// resolution and clock rounding, and the lag allows a slow start with
/// headroom. A reused pid differs by the lifetime of the earlier process —
/// typically hours or days, and always on the NEGATIVE side, because the
/// stale record was written before the live process began.
pub const START_LEAD_MS: i64 = 5 * 1000;
/// See [`START_LEAD_MS`].
pub const START_LAG_MS: i64 = 5 * 60 * 1000;

/// One process's activity class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Working,
    Idle,
    Stale,
    Unknown,
}

/// The four-way aggregate. `working + idle + stale + unknown` equals the
/// number of processes classified — callers classify every live process, so
/// it equals `live_claude.total`.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct ActivityCounts {
    pub working: usize,
    pub idle: usize,
    pub stale: usize,
    pub unknown: usize,
}

impl ActivityCounts {
    pub fn add(&mut self, activity: Activity) {
        match activity {
            Activity::Working => self.working += 1,
            Activity::Idle => self.idle += 1,
            Activity::Stale => self.stale += 1,
            Activity::Unknown => self.unknown += 1,
        }
    }

    pub fn sum(&self) -> usize {
        self.working + self.idle + self.stale + self.unknown
    }
}

/// What Claude Code's record said about one pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordReading {
    /// No `<pid>.json` in any config dir, and every `sessions/` dir was
    /// listed — a positive absence.
    Missing,
    /// No record of THIS process in any dir that COULD be listed — none at
    /// all, or only an earlier holder's — while at least one `<dir>/sessions`
    /// exists and could not be ([`UnlistableSessionsDir`]). The live
    /// process's record may well be in that dir, so this is UNKNOWN — never
    /// [`Self::Missing`] or [`Self::PidReused`].
    Unlisted,
    /// A file exists but could not be read, did not parse, or named a
    /// different pid.
    Unparseable,
    /// Two config dirs hold a record for this pid naming different sessions —
    /// one of them is a stale file from a crashed process. Not a guess.
    Ambiguous,
    /// The record's start stamp disagrees with the live process's: it was
    /// written by an earlier process that held the same pid.
    PidReused,
    /// The record's `startedAt` is LATER than [`START_LAG_MS`] after the live
    /// process began, and no `procStart` decided. A reused pid's stale record
    /// is always EARLIER (it was written before the live process existed), so
    /// this is not reuse — but nor is it proof that the record is this
    /// process's. UNKNOWN; never [`Self::PidReused`].
    LateStamp,
    Parsed(RecordEvidence),
}

/// The fields of one parsed record the classification reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordEvidence {
    pub status: String,
    pub status_updated_at_ms: Option<i64>,
    /// Timestamp of the last real `user`/`assistant` transcript line, read
    /// only for `busy`/`shell` records. `None` when not read, not located, or
    /// no such line was in the tail.
    pub last_message_ms: Option<i64>,
}

/// A live `claude` pid whose record should be read, with the start time the
/// census implies for it (`checked_at_ms - age_s * 1000`, from the SAME
/// snapshot — never a later clock read, which would skew it by however long
/// the request took) for the `startedAt` identity check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivePid {
    pub pid: u32,
    pub process_started_ms: Option<i64>,
}

/// The raw on-disk record. Only the keys this module reads are modelled so a
/// new key cannot break parsing; every one is optional so a missing key reads
/// as "not stated" rather than as a parse failure of the whole file.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRecord {
    pid: Option<u32>,
    session_id: Option<String>,
    cwd: Option<String>,
    status: Option<String>,
    status_updated_at: Option<i64>,
    started_at: Option<i64>,
    /// A string on every record seen; a number is accepted too, since the
    /// comparison is textual against `/proc` either way.
    proc_start: Option<serde_json::Value>,
}

impl RawRecord {
    /// `None` for an absent, empty or non-numeric value: a `procStart` that
    /// cannot be a tick count says nothing, so the `startedAt` check decides
    /// rather than a guaranteed mismatch against `/proc`.
    fn proc_start(&self) -> Option<String> {
        let text = match self.proc_start.as_ref()? {
            serde_json::Value::String(s) => s.trim().to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => return None,
        };
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())).then_some(text)
    }
}

/// Classify from the runner's pane observation. `None` means "not decisive —
/// fall back to the record", never "idle". See the module docs: children
/// never decide `working`, and an old sideband `working` decides nothing.
pub fn classify_from_pane(
    observation: &TerminalObservation,
    has_live_children: Option<bool>,
    now_ms: i64,
) -> Option<Activity> {
    let recent_sideband_working = matches!(
        observation.sideband,
        Sideband::Reported {
            state: SidebandState::Working,
            set_at_ms,
        } if now_ms - set_at_ms <= ACTIVE_WINDOW_MS
    );
    if recent_sideband_working || observation.grid == GridIdle::Busy {
        return Some(Activity::Working);
    }
    let sideband_idle = matches!(
        observation.sideband,
        Sideband::Reported {
            state: SidebandState::NotWorking,
            ..
        }
    );
    let grid_idle = matches!(observation.grid, GridIdle::Idle { .. });
    // A live child VETOES pane-idle (it may be mid-tool-call) without being
    // evidence of work (it may be a days-old MCP shim); `None` is
    // UNCOMPUTABLE and does not get to vouch for idleness either. Both fall
    // through to the record.
    if sideband_idle && grid_idle && has_live_children == Some(false) {
        return Some(Activity::Idle);
    }
    None
}

/// Classify from Claude Code's record. See the module docs for the table.
pub fn classify_from_record(reading: &RecordReading, now_ms: i64) -> Activity {
    let RecordReading::Parsed(ev) = reading else {
        return Activity::Unknown;
    };
    match ev.status.as_str() {
        "idle" | "waiting" => Activity::Idle,
        "busy" | "shell" => {
            let Some(evidence_ms) = ev.last_message_ms.or(ev.status_updated_at_ms) else {
                return Activity::Unknown;
            };
            if now_ms - evidence_ms <= ACTIVE_WINDOW_MS {
                Activity::Working
            } else {
                Activity::Stale
            }
        }
        _ => Activity::Unknown,
    }
}

/// The full precedence: a decisive pane observation, else the record.
pub fn classify(
    observation: Option<&TerminalObservation>,
    has_live_children: Option<bool>,
    record: &RecordReading,
    now_ms: i64,
) -> Activity {
    observation
        .and_then(|obs| classify_from_pane(obs, has_live_children, now_ms))
        .unwrap_or_else(|| classify_from_record(record, now_ms))
}

/// Parse one record's bytes for `pid`. A body that does not parse, or that
/// names a different pid, is [`RecordReading::Unparseable`].
fn parse_record(bytes: &str, pid: u32) -> Result<(RawRecord, RecordEvidence), ()> {
    let raw: RawRecord = serde_json::from_str(bytes).map_err(|_| ())?;
    if raw.pid != Some(pid) {
        return Err(());
    }
    let evidence = RecordEvidence {
        // An absent `status` is an unrecognised status: it classifies unknown.
        status: raw.status.clone().unwrap_or_default(),
        status_updated_at_ms: raw.status_updated_at,
        last_message_ms: None,
    };
    Ok((raw, evidence))
}

/// How a record's identity stamp compares with the live process holding its
/// pid. DIRECTIONAL, because the two ways out of the window mean different
/// things: a stamp from before the live process began is an earlier holder's
/// record, while a stamp after the window is merely unexplained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    /// The same process: `procStart` agrees with the kernel, or `startedAt`
    /// falls inside the window.
    Same,
    /// An earlier holder of the pid wrote it: `procStart` disagrees with the
    /// kernel (either way — a stale record can survive a reboot, when the tick
    /// count restarts), or `startedAt` is more than [`START_LEAD_MS`] BEFORE
    /// the live process began. The one reading that is reuse.
    Earlier,
    /// `startedAt` is more than [`START_LAG_MS`] AFTER the live process began.
    /// Never reuse (see [`RecordReading::LateStamp`]), never proof either.
    Later,
    /// Neither check could be made.
    Unchecked,
}

/// Does `raw` describe the process that holds `live.pid` NOW? See
/// [`Identity`].
fn identity_of(
    raw: &RawRecord,
    live: LivePid,
    proc_start: &dyn Fn(u32) -> Option<String>,
) -> Identity {
    if let (Some(recorded), Some(actual)) = (raw.proc_start(), proc_start(live.pid)) {
        return if recorded == actual {
            Identity::Same
        } else {
            Identity::Earlier
        };
    }
    let (Some(started_at), Some(process_started)) = (raw.started_at, live.process_started_ms)
    else {
        return Identity::Unchecked;
    };
    let lag_ms = started_at - process_started;
    if lag_ms < -START_LEAD_MS {
        Identity::Earlier
    } else if lag_ms > START_LAG_MS {
        Identity::Later
    } else {
        Identity::Same
    }
}

/// The kernel's start time for `pid` in clock ticks since boot — field 22 of
/// `/proc/<pid>/stat`, the same number Claude Code writes as `procStart`.
/// Parsed after the LAST `)`, since field 2 (`comm`) may itself contain
/// spaces and parentheses. `None` off Linux and on any read failure.
pub fn proc_start_ticks(pid: u32) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    proc_start_from_stat(&stat)
}

fn proc_start_from_stat(stat: &str) -> Option<String> {
    let (_, after_comm) = stat.rsplit_once(')')?;
    // After `comm` the next field is field 3 (`state`), so field 22 is the
    // 20th whitespace-separated token.
    after_comm.split_whitespace().nth(19).map(str::to_string)
}

/// Timestamp (unix ms) of the last real `user`/`assistant` line in `tail`.
/// Harness-injected `isMeta` lines are not messages. A line without a
/// parseable `timestamp` is skipped — never stamped "now".
pub fn last_message_ms(tail: &str) -> Option<i64> {
    tail.lines().rev().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        let kind = value.get("type")?.as_str()?;
        if kind != "user" && kind != "assistant" {
            return None;
        }
        if value.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
            return None;
        }
        let ts = value.get("timestamp")?.as_str()?;
        chrono::DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|dt| dt.timestamp_millis())
    })
}

/// The last `max` bytes of `path`, decoded LOSSILY: an invalid UTF-8 byte
/// damages the line it sits in, never the whole read. When the read starts
/// mid-file its first (partial) line is dropped. `None` only on an I/O error.
pub fn read_tail_lossy(path: &Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start == 0 {
        return Some(text);
    }
    Some(match text.split_once('\n') {
        Some((_, rest)) => rest.to_string(),
        None => String::new(),
    })
}

/// A `<config-dir>/sessions` that exists but could not be listed in full:
/// `read_dir` failed with anything but `NotFound` (permission denied, an I/O
/// error, a `sessions` that is not a directory), or the listing broke off
/// part-way. Every record in it is UNKNOWN — never absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlistableSessionsDir {
    pub sessions_dir: PathBuf,
    pub error: String,
}

impl std::fmt::Display for UnlistableSessionsDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.sessions_dir.display(), self.error)
    }
}

/// One config dir's `sessions/` listing — typed, because an unlistable dir and
/// an absent one mean opposite things to a liveness reader. Until Phase 15
/// (b) of plan `2026-10-28-one-session-lifecycle-authority` every reader
/// `continue`d on EVERY `read_dir` error ("no sessions dir on this account —
/// normal"), so a dir it could not list read exactly like an account with no
/// live session, and a live `claude` recorded there read as gone.
#[derive(Debug)]
pub enum SessionsListing {
    /// Listed in full: the path of every entry.
    Listed(Vec<PathBuf>),
    /// `<dir>/sessions` does not exist — an account with no live session.
    /// The one genuine absence.
    Absent,
    /// It exists and could not be listed in full. See
    /// [`UnlistableSessionsDir`].
    Unlistable(UnlistableSessionsDir),
}

/// List `<config_dir>/sessions`. Only `NotFound` is an absence; every other
/// error — on opening the dir or on any entry of it — is
/// [`SessionsListing::Unlistable`]. Blocking file I/O.
pub fn list_sessions_dir(config_dir: &Path) -> SessionsListing {
    let sessions_dir = config_dir.join("sessions");
    let unlistable = |e: std::io::Error| {
        SessionsListing::Unlistable(UnlistableSessionsDir {
            sessions_dir: sessions_dir.clone(),
            error: e.to_string(),
        })
    };
    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SessionsListing::Absent,
        Err(e) => return unlistable(e),
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            // A listing that broke off is not a listing: the entries it did
            // not reach are as unknown as the whole dir would be.
            Err(e) => return unlistable(e),
        }
    }
    SessionsListing::Listed(paths)
}

/// How much of each record [`read_records`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordDepth {
    /// The record alone: whose it is (the pid and start-time identity check)
    /// and its `status`. What the graceful-exit probe's registry witness needs.
    Identity,
    /// The record, plus the transcript tail of every `busy`/`shell` record —
    /// what [`classify_from_record`] needs to tell `working` from `stale`.
    Activity,
}

/// What [`read_records`] saw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordReadings {
    /// One reading per pid asked about.
    pub by_pid: HashMap<u32, RecordReading>,
    /// Every `sessions/` dir that exists but could not be listed. While this
    /// is non-empty no pid reads [`RecordReading::Missing`] or
    /// [`RecordReading::PidReused`]: one with no record of its own process in
    /// the dirs that WERE listed reads [`RecordReading::Unlisted`].
    pub unlistable: Vec<UnlistableSessionsDir>,
}

/// One reader's memory of which `sessions/` dirs it has reported unlistable,
/// so the condition is logged once per TRANSITION of each dir — a warn when a
/// dir turns unlistable, an info when it lists again — rather than on every
/// read. A graceful exit probes every half second and restart-readiness
/// answers every Stop turn, so a per-read line would bury the log. Keyed per
/// DIR rather than on the whole set: two reads may be asked about different
/// dirs (the probe adds each candidate's own config dir), and a dir one read
/// was not asked about is neither reported again nor declared healed.
pub struct UnlistableLog {
    reported: std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
    reader: &'static str,
    consequence: &'static str,
}

impl UnlistableLog {
    /// `reader` names who read; `consequence` says what the condition costs it.
    pub const fn new(reader: &'static str, consequence: &'static str) -> Self {
        Self {
            reported: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            reader,
            consequence,
        }
    }

    /// Record what one read of `config_dirs` saw, logging only a change.
    pub fn observe(&self, config_dirs: &[PathBuf], unlistable: &[UnlistableSessionsDir]) {
        let mut reported = self
            .reported
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for dir in unlistable {
            if reported.insert(dir.sessions_dir.clone()) {
                tracing::warn!(
                    sessions_dir = %dir.sessions_dir.display(),
                    error = %dir.error,
                    "{}: a Claude Code sessions dir exists but cannot be listed — {}",
                    self.reader,
                    self.consequence
                );
            }
        }
        for config_dir in config_dirs {
            let sessions_dir = config_dir.join("sessions");
            let still_unlistable = unlistable.iter().any(|u| u.sessions_dir == sessions_dir);
            if !still_unlistable && reported.remove(&sessions_dir) {
                tracing::info!(
                    sessions_dir = %sessions_dir.display(),
                    "{}: a Claude Code sessions dir that could not be listed reads again",
                    self.reader
                );
            }
        }
    }

    /// The dirs currently reported unlistable. Test builds only.
    #[cfg(test)]
    pub(crate) fn reported(&self) -> Vec<PathBuf> {
        self.reported
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

/// Read Claude Code's record for each of `pids` across `config_dirs`.
///
/// Each `sessions/` dir is listed ONCE ([`list_sessions_dir`]) and only the
/// wanted `<pid>.json` files are opened; at [`RecordDepth::Activity`] the
/// transcript tail is read for `busy`/`shell` records too. A dir that exists
/// and cannot be listed is reported in [`RecordReadings::unlistable`] and
/// turns every would-be `Missing` into `Unlisted`. `proc_start` is
/// [`proc_start_ticks`] in production and injectable in tests. Blocking file
/// I/O — call it off the async executor.
pub fn read_records(
    config_dirs: &[PathBuf],
    pids: &[LivePid],
    proc_start: &dyn Fn(u32) -> Option<String>,
    depth: RecordDepth,
) -> RecordReadings {
    let wanted: std::collections::HashSet<u32> = pids.iter().map(|p| p.pid).collect();
    // pid -> every (config dir, record path) that names it.
    let mut found: HashMap<u32, Vec<(PathBuf, PathBuf)>> = HashMap::new();
    let mut unlistable = Vec::new();
    for dir in config_dirs {
        let paths = match list_sessions_dir(dir) {
            SessionsListing::Listed(paths) => paths,
            SessionsListing::Absent => continue, // no sessions dir on this account — normal
            SessionsListing::Unlistable(why) => {
                unlistable.push(why);
                continue;
            }
        };
        for path in paths {
            let Some(pid) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|stem| stem.parse::<u32>().ok())
            else {
                continue;
            };
            if wanted.contains(&pid) {
                found.entry(pid).or_default().push((dir.clone(), path));
            }
        }
    }

    let by_pid = pids
        .iter()
        .map(|&live| {
            let reading = match found.get(&live.pid) {
                None => RecordReading::Missing,
                Some(candidates) => read_one(live, candidates, proc_start, depth),
            };
            // While a dir is unlistable, "no record of THIS process in the
            // dirs that listed" is all `Missing` and `PidReused` can say: the
            // live process's own record may sit in the dir nobody could read.
            let reading = match reading {
                RecordReading::Missing | RecordReading::PidReused if !unlistable.is_empty() => {
                    RecordReading::Unlisted
                }
                other => other,
            };
            (live.pid, reading)
        })
        .collect();
    RecordReadings { by_pid, unlistable }
}

fn read_one(
    live: LivePid,
    candidates: &[(PathBuf, PathBuf)],
    proc_start: &dyn Fn(u32) -> Option<String>,
    depth: RecordDepth,
) -> RecordReading {
    let mut parsed: Vec<(&Path, RawRecord, RecordEvidence)> = Vec::new();
    let mut reused = false;
    let mut late = false;
    let mut unreadable = false;
    for (dir, path) in candidates {
        let body = match std::fs::read_to_string(path) {
            Ok(body) => body,
            // Listed, then gone before the open: Claude Code deletes its
            // record when the process exits, so this one no longer exists —
            // the same as never having been listed, not an unreadable file.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                unreadable = true;
                continue;
            }
        };
        // A body that does not parse is only decisive when nothing else names
        // this pid — see the `let Some(..) else` below.
        let Ok((raw, ev)) = parse_record(&body, live.pid) else {
            unreadable = true;
            continue;
        };
        match identity_of(&raw, live, proc_start) {
            Identity::Same | Identity::Unchecked => parsed.push((dir.as_path(), raw, ev)),
            Identity::Earlier => reused = true,
            Identity::Later => late = true,
        }
    }
    let first_session = parsed
        .first()
        .and_then(|(_, raw, _)| raw.session_id.clone());
    if parsed.len() > 1
        && parsed
            .iter()
            .any(|(_, raw, _)| raw.session_id != first_session)
    {
        return RecordReading::Ambiguous;
    }
    let Some((dir, raw, mut ev)) = parsed.into_iter().next() else {
        // Each candidate failed to read or parse, was stamped late, belonged
        // to an earlier holder of this pid, or vanished before it could be
        // opened. A file that could not be read, or one stamped late, OUTRANKS
        // a reused one: either may be the live process's own record, so the
        // pid stays undecided rather than settling on "an earlier holder's".
        return if unreadable {
            RecordReading::Unparseable
        } else if late {
            RecordReading::LateStamp
        } else if reused {
            RecordReading::PidReused
        } else {
            RecordReading::Missing
        };
    };
    if depth == RecordDepth::Activity && (ev.status == "busy" || ev.status == "shell") {
        if let (Some(cwd), Some(session_id)) = (raw.cwd.as_deref(), raw.session_id.as_deref()) {
            let transcript =
                crate::terminal::transcript::session_transcript_path(dir, cwd, session_id);
            ev.last_message_ms = read_tail_lossy(&transcript, TRANSCRIPT_TAIL_BYTES)
                .as_deref()
                .and_then(last_message_ms);
        }
    }
    RecordReading::Parsed(ev)
}

/// Test fixture: a `sessions/` dir this process cannot list, shared by every
/// test of a reader that must read such a dir as UNKNOWN.
#[cfg(all(test, unix))]
pub(crate) mod unlistable_fixture {
    use std::path::{Path, PathBuf};

    /// Make `<config_dir>/sessions` mode 000. `None` when this process can
    /// list it anyway (it runs as root): the caller then skips its assertion
    /// rather than asserting something false.
    pub(crate) fn unreadable_sessions_dir(config_dir: &Path) -> Option<PermissionRestore> {
        use std::os::unix::fs::PermissionsExt;
        let sessions = config_dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o000)).unwrap();
        let restore = PermissionRestore(sessions.clone());
        if std::fs::read_dir(&sessions).is_ok() {
            eprintln!(
                "skipped: {} is still listable (running as root?)",
                sessions.display()
            );
            return None;
        }
        Some(restore)
    }

    /// Puts the mode-000 dir back to 0755 on drop, so the temp dir can be
    /// removed.
    pub(crate) struct PermissionRestore(PathBuf);

    impl Drop for PermissionRestore {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000_000;
    const MIN: i64 = 60 * 1000;

    fn parsed(
        status: &str,
        status_age_min: Option<i64>,
        msg_age_min: Option<i64>,
    ) -> RecordReading {
        RecordReading::Parsed(RecordEvidence {
            status: status.to_string(),
            status_updated_at_ms: status_age_min.map(|m| NOW - m * MIN),
            last_message_ms: msg_age_min.map(|m| NOW - m * MIN),
        })
    }

    fn obs(sideband: Sideband, grid: GridIdle) -> TerminalObservation {
        TerminalObservation { sideband, grid }
    }

    fn working_since(age_min: i64) -> Sideband {
        Sideband::Reported {
            state: SidebandState::Working,
            set_at_ms: NOW - age_min * MIN,
        }
    }

    const NOT_WORKING: Sideband = Sideband::Reported {
        state: SidebandState::NotWorking,
        set_at_ms: 1,
    };
    const IDLE_GRID: GridIdle = GridIdle::Idle { since_ms: 1 };

    /// No `/proc` in tests: every identity check falls to `startedAt`.
    fn no_proc(_: u32) -> Option<String> {
        None
    }

    fn live(pid: u32) -> LivePid {
        LivePid {
            pid,
            process_started_ms: None,
        }
    }

    #[test]
    fn record_idle_and_waiting_are_idle_regardless_of_age() {
        assert_eq!(
            classify_from_record(&parsed("idle", Some(5000), None), NOW),
            Activity::Idle
        );
        assert_eq!(
            classify_from_record(&parsed("waiting", None, None), NOW),
            Activity::Idle
        );
    }

    #[test]
    fn record_busy_or_shell_is_working_only_with_a_recent_message() {
        for status in ["busy", "shell"] {
            // A recent message wins over an old status stamp.
            assert_eq!(
                classify_from_record(&parsed(status, Some(600), Some(5)), NOW),
                Activity::Working,
                "{status}"
            );
            // An old message is stale even when the status is freshly stamped:
            // the transcript is the stronger evidence.
            assert_eq!(
                classify_from_record(&parsed(status, Some(1), Some(31)), NOW),
                Activity::Stale,
                "{status}"
            );
            // The boundary is inclusive.
            assert_eq!(
                classify_from_record(&parsed(status, None, Some(30)), NOW),
                Activity::Working
            );
        }
    }

    #[test]
    fn record_busy_without_a_transcript_falls_back_to_status_age() {
        assert_eq!(
            classify_from_record(&parsed("busy", Some(10), None), NOW),
            Activity::Working
        );
        assert_eq!(
            classify_from_record(&parsed("shell", Some(2000), None), NOW),
            Activity::Stale
        );
        // Neither signal: no evidence either way.
        assert_eq!(
            classify_from_record(&parsed("busy", None, None), NOW),
            Activity::Unknown
        );
    }

    #[test]
    fn record_missing_unparseable_ambiguous_reused_or_unknown_status_is_unknown_never_idle() {
        for reading in [
            RecordReading::Missing,
            RecordReading::Unlisted,
            RecordReading::Unparseable,
            RecordReading::Ambiguous,
            RecordReading::PidReused,
            RecordReading::LateStamp,
            parsed("thinking-hard", Some(1), Some(1)),
            parsed("", Some(1), Some(1)),
        ] {
            assert_eq!(
                classify_from_record(&reading, NOW),
                Activity::Unknown,
                "{reading:?}"
            );
        }
    }

    #[test]
    fn pane_working_is_a_recent_sideband_or_a_busy_grid() {
        let unknown = RecordReading::Missing;
        assert_eq!(
            classify(
                Some(&obs(working_since(1), IDLE_GRID)),
                Some(false),
                &unknown,
                NOW
            ),
            Activity::Working
        );
        // The window boundary is inclusive.
        assert_eq!(
            classify(
                Some(&obs(working_since(30), IDLE_GRID)),
                None,
                &unknown,
                NOW
            ),
            Activity::Working
        );
        assert_eq!(
            classify(
                Some(&obs(NOT_WORKING, GridIdle::Busy)),
                Some(false),
                &unknown,
                NOW
            ),
            Activity::Working
        );
        // A decisive pane beats an idle record.
        assert_eq!(
            classify(
                Some(&obs(Sideband::NeverReported, GridIdle::Busy)),
                None,
                &parsed("idle", None, None),
                NOW
            ),
            Activity::Working
        );
    }

    /// A sideband `working` from hours ago says nothing about now: it is not
    /// decisive, and the record decides.
    #[test]
    fn an_expired_sideband_working_falls_through_to_the_record() {
        let old = obs(working_since(31), IDLE_GRID);
        assert_eq!(classify_from_pane(&old, Some(false), NOW), None);
        assert_eq!(
            classify(Some(&old), Some(false), &parsed("idle", None, None), NOW),
            Activity::Idle
        );
        assert_eq!(
            classify(Some(&old), Some(false), &RecordReading::Missing, NOW),
            Activity::Unknown
        );
    }

    /// Live children never decide `working` — an idle `claude` holds
    /// days-old MCP shims and background loops. They only veto pane-idle.
    #[test]
    fn live_children_never_decide_working_they_only_veto_pane_idle() {
        let idle_pane = obs(NOT_WORKING, IDLE_GRID);
        assert_eq!(classify_from_pane(&idle_pane, Some(true), NOW), None);
        assert_eq!(
            classify(
                Some(&idle_pane),
                Some(true),
                &parsed("idle", None, None),
                NOW
            ),
            Activity::Idle,
            "a shim child on an idle session is still idle"
        );
        assert_eq!(
            classify(Some(&idle_pane), Some(true), &RecordReading::Missing, NOW),
            Activity::Unknown
        );
        // Nor from the record side: a children flag never reaches it.
        assert_eq!(
            classify(None, Some(true), &parsed("idle", None, None), NOW),
            Activity::Idle
        );
    }

    #[test]
    fn pane_idle_needs_sideband_grid_and_known_childlessness() {
        let busy_record = parsed("busy", Some(1), Some(1));
        assert_eq!(
            classify(
                Some(&obs(NOT_WORKING, IDLE_GRID)),
                Some(false),
                &busy_record,
                NOW
            ),
            Activity::Idle,
            "a decisive idle pane outranks the record"
        );
        // Each missing leg falls through to the record rather than to idle.
        for (o, children) in [
            (obs(NOT_WORKING, IDLE_GRID), None),
            (obs(NOT_WORKING, IDLE_GRID), Some(true)),
            (obs(Sideband::NeverReported, IDLE_GRID), Some(false)),
            (obs(Sideband::Unreadable, IDLE_GRID), Some(false)),
            (obs(NOT_WORKING, GridIdle::Unknown), Some(false)),
            (TerminalObservation::UNOBSERVABLE, Some(false)),
        ] {
            assert_eq!(
                classify(Some(&o), children, &busy_record, NOW),
                Activity::Working
            );
            assert_eq!(
                classify(Some(&o), children, &RecordReading::Missing, NOW),
                Activity::Unknown,
                "{o:?} {children:?}"
            );
        }
    }

    #[test]
    fn no_pane_observation_falls_back_to_the_record() {
        assert_eq!(
            classify(None, Some(false), &parsed("idle", None, None), NOW),
            Activity::Idle
        );
        assert_eq!(
            classify(None, None, &RecordReading::Missing, NOW),
            Activity::Unknown
        );
    }

    #[test]
    fn last_message_skips_non_message_and_meta_lines() {
        let tail = [
            r#"{"type":"user","timestamp":"2026-09-29T10:00:00Z","message":{}}"#,
            r#"{"type":"assistant","timestamp":"2026-09-29T10:05:00.500Z","message":{}}"#,
            r#"{"type":"user","isMeta":true,"timestamp":"2026-09-29T10:09:00Z"}"#,
            r#"{"type":"summary","timestamp":"2026-09-29T10:10:00Z"}"#,
            r#"{"type":"assistant"}"#,
            r#"not json at all"#,
        ]
        .join("\n");
        let expected = chrono::DateTime::parse_from_rfc3339("2026-09-29T10:05:00.500Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(last_message_ms(&tail), Some(expected));
        assert_eq!(last_message_ms(r#"{"type":"summary"}"#), None);
    }

    #[test]
    fn proc_start_is_field_22_even_when_comm_has_spaces_and_parens() {
        // Fields 3..=21 then starttime (22) then the rest.
        let rest: Vec<String> = (3..=21).map(|n| format!("f{n}")).collect();
        let stat = format!("4242 (we ird) (claude) {} 225893133 99 100", rest.join(" "));
        assert_eq!(proc_start_from_stat(&stat).as_deref(), Some("225893133"));
        assert_eq!(proc_start_from_stat("garbage"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_start_of_this_process_reads_on_linux() {
        let ticks = proc_start_ticks(std::process::id()).expect("/proc/self readable");
        assert!(ticks.parse::<u64>().is_ok(), "{ticks}");
    }

    fn scratch(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("claude-activity-{tag}-"))
            .tempdir()
            .unwrap()
    }

    #[test]
    fn read_records_classifies_each_file_shape() {
        let root = scratch("shapes");
        let dir = root.path().join(".claude-a");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let cwd = "/work/repo";
        let recent = chrono::DateTime::from_timestamp_millis(NOW - 2 * MIN)
            .unwrap()
            .to_rfc3339();
        // pid 10: busy with a recent transcript message.
        std::fs::write(
            dir.join("sessions/10.json"),
            format!(
                r#"{{"pid":10,"sessionId":"s10","cwd":"{cwd}","status":"busy","statusUpdatedAt":{}}}"#,
                NOW - 600 * MIN
            ),
        )
        .unwrap();
        let transcript = crate::terminal::transcript::session_transcript_path(&dir, cwd, "s10");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript,
            format!("{{\"type\":\"assistant\",\"timestamp\":\"{recent}\"}}\n"),
        )
        .unwrap();
        // pid 11: idle. pid 12: garbage. pid 13: names another pid.
        std::fs::write(
            dir.join("sessions/11.json"),
            r#"{"pid":11,"sessionId":"s11","status":"idle"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("sessions/12.json"), "{not json").unwrap();
        std::fs::write(
            dir.join("sessions/13.json"),
            r#"{"pid":99,"sessionId":"s13","status":"idle"}"#,
        )
        .unwrap();
        // pid 15: two config dirs disagree on the session.
        let other = root.path().join(".claude-b");
        std::fs::create_dir_all(other.join("sessions")).unwrap();
        std::fs::write(
            dir.join("sessions/15.json"),
            r#"{"pid":15,"sessionId":"x","status":"idle"}"#,
        )
        .unwrap();
        std::fs::write(
            other.join("sessions/15.json"),
            r#"{"pid":15,"sessionId":"y","status":"idle"}"#,
        )
        .unwrap();

        let pids: Vec<LivePid> = [10, 11, 12, 13, 14, 15].into_iter().map(live).collect();
        let readings = read_records(
            &[dir.clone(), other, root.path().join("absent")],
            &pids,
            &no_proc,
            RecordDepth::Activity,
        );
        assert!(
            readings.unlistable.is_empty(),
            "an absent config dir is an absence, not an unlistable dir"
        );
        let readings = readings.by_pid;
        let class = |pid: u32| classify_from_record(&readings[&pid], NOW);
        assert_eq!(class(10), Activity::Working, "{:?}", readings[&10]);
        assert_eq!(class(11), Activity::Idle);
        assert_eq!(readings[&12], RecordReading::Unparseable);
        assert_eq!(readings[&13], RecordReading::Unparseable);
        assert_eq!(readings[&14], RecordReading::Missing);
        assert_eq!(readings[&15], RecordReading::Ambiguous);
        for pid in [12, 13, 14, 15] {
            assert_eq!(class(pid), Activity::Unknown, "pid {pid}");
        }
    }

    /// A record left by an EARLIER holder of the pid is `PidReused` →
    /// unknown: by `procStart` where the kernel's start time is readable, by
    /// `startedAt` against the census's implied process start where it is
    /// not. The `startedAt` window is one-sided (Claude Code stamps it AFTER
    /// the process starts), and each edge is pinned. Only the EARLY side is
    /// reuse: one past the lag edge is `LateStamp`, unknown but never reuse.
    #[test]
    fn a_record_from_an_earlier_holder_of_the_pid_is_pid_reused() {
        let root = scratch("reuse");
        let dir = root.path().join(".claude");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let rec = |pid: u32, proc_start: &str, started_at: i64| {
            std::fs::write(
                dir.join(format!("sessions/{pid}.json")),
                format!(
                    r#"{{"pid":{pid},"sessionId":"s{pid}","status":"idle","procStart":"{proc_start}","startedAt":{started_at}}}"#
                ),
            )
            .unwrap();
        };
        let started = NOW - 3_600_000; // the process began an hour ago
        rec(1, "500", started); // procStart matches the kernel
        rec(2, "400", started); // procStart disagrees
        rec(3, "", started + 78_000); // the largest lag measured: same process
        rec(4, "", NOW - 86_400_000); // written a day before the process began
        rec(5, "", started); // neither check reachable: accepted
        rec(6, "", started + 1_000); // empty procStart = absent: startedAt decides
        rec(7, "abc", started + 1_000); // non-numeric procStart = absent too
        rec(8, "", started + START_LAG_MS); // lag edge, inclusive
        rec(9, "", started + START_LAG_MS + 1); // one past the lag edge: late, not reused
        rec(10, "", started - START_LEAD_MS); // lead edge, inclusive
        rec(11, "", started - START_LEAD_MS - 1); // one past the lead edge
        let kernel = |pid: u32| match pid {
            1 | 2 | 6 | 7 => Some("500".to_string()),
            _ => None,
        };
        let at = |pid: u32| LivePid {
            pid,
            process_started_ms: Some(started),
        };
        let pids = [
            live(1),
            live(2),
            at(3),
            at(4),
            live(5),
            at(6),
            at(7),
            at(8),
            at(9),
            at(10),
            at(11),
        ];
        let readings = read_records(&[dir], &pids, &kernel, RecordDepth::Activity).by_pid;
        for pid in [1, 3, 5, 6, 7, 8, 10] {
            assert_eq!(
                classify_from_record(&readings[&pid], NOW),
                Activity::Idle,
                "pid {pid}: {:?}",
                readings[&pid]
            );
        }
        for pid in [2, 4, 11] {
            assert_eq!(readings[&pid], RecordReading::PidReused, "pid {pid}");
            assert_eq!(
                classify_from_record(&readings[&pid], NOW),
                Activity::Unknown
            );
        }
        assert_eq!(readings[&9], RecordReading::LateStamp);
        assert_eq!(classify_from_record(&readings[&9], NOW), Activity::Unknown);
    }

    /// A record stamped LATER than the lag window, beside one an earlier
    /// holder left in another dir, is `LateStamp`: the late one may be the
    /// live process's own record, so the pid is not settled as reused.
    #[test]
    fn a_late_stamp_outranks_a_reused_one() {
        let root = scratch("late-outrank");
        let a = root.path().join(".claude-a");
        let b = root.path().join(".claude-b");
        let started = NOW - 3_600_000;
        for (dir, started_at) in [(&a, started - 86_400_000), (&b, started + 6 * MIN)] {
            std::fs::create_dir_all(dir.join("sessions")).unwrap();
            std::fs::write(
                dir.join("sessions/60.json"),
                format!(
                    r#"{{"pid":60,"sessionId":"s60","status":"idle","startedAt":{started_at}}}"#
                ),
            )
            .unwrap();
        }
        let live = LivePid {
            pid: 60,
            process_started_ms: Some(started),
        };
        let readings = read_records(&[a, b], &[live], &no_proc, RecordDepth::Identity);
        assert_eq!(readings.by_pid[&60], RecordReading::LateStamp);
    }

    #[test]
    fn busy_record_with_no_transcript_uses_status_age() {
        let root = scratch("no-transcript");
        let dir = root.path().join(".claude");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(
            dir.join("sessions/20.json"),
            format!(
                r#"{{"pid":20,"sessionId":"s20","cwd":"/nowhere","status":"shell","statusUpdatedAt":{}}}"#,
                NOW - 90 * MIN
            ),
        )
        .unwrap();
        let readings = read_records(&[dir], &[live(20)], &no_proc, RecordDepth::Activity).by_pid;
        assert_eq!(classify_from_record(&readings[&20], NOW), Activity::Stale);
    }

    /// Only `NotFound` is an absence. Every other `read_dir` error is a dir
    /// that EXISTS and cannot be listed — the shape every reader used to fold
    /// into its `continue`.
    #[test]
    fn a_sessions_dir_is_absent_only_on_not_found() {
        let root = scratch("listing");
        assert!(matches!(
            list_sessions_dir(&root.path().join("no-such-account")),
            SessionsListing::Absent
        ));
        let listed = root.path().join(".claude-listed");
        std::fs::create_dir_all(listed.join("sessions")).unwrap();
        std::fs::write(listed.join("sessions/7.json"), "{}").unwrap();
        match list_sessions_dir(&listed) {
            SessionsListing::Listed(paths) => {
                assert_eq!(paths, vec![listed.join("sessions/7.json")]);
            }
            other => panic!("expected Listed, got {other:?}"),
        }
    }

    /// A `sessions` that is not a directory cannot be listed, and says so —
    /// it is not read as an account with no live session. Not gated on the
    /// platform or the user, because unlike a mode-000 dir it is unlistable to
    /// root as well. Measured on Linux; on Windows this test's CI run is the
    /// first evidence that `read_dir` on a file is not `NotFound` there.
    #[test]
    fn a_sessions_path_that_is_not_a_directory_is_unlistable() {
        let root = scratch("not-a-dir");
        let dir = root.path().join(".claude-file");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sessions"), "not a directory").unwrap();
        match list_sessions_dir(&dir) {
            SessionsListing::Unlistable(why) => {
                assert_eq!(why.sessions_dir, dir.join("sessions"));
                assert!(!why.error.is_empty());
            }
            other => panic!("expected Unlistable, got {other:?}"),
        }
    }

    /// The defect this typed listing exists for. A live `claude`'s record sits
    /// in a `sessions/` dir the runner cannot list: its pid reads `Unlisted`
    /// (unknown), never `Missing`, and the dir is reported. A record in a dir
    /// that WAS listed still reads normally beside it.
    #[cfg(unix)]
    #[test]
    fn an_unlistable_sessions_dir_turns_missing_into_unlisted() {
        let root = scratch("unlistable");
        let readable = root.path().join(".claude-a");
        std::fs::create_dir_all(readable.join("sessions")).unwrap();
        std::fs::write(
            readable.join("sessions/11.json"),
            r#"{"pid":11,"sessionId":"s11","status":"idle"}"#,
        )
        .unwrap();
        let hidden = root.path().join(".claude-b");
        std::fs::create_dir_all(hidden.join("sessions")).unwrap();
        std::fs::write(
            hidden.join("sessions/12.json"),
            r#"{"pid":12,"sessionId":"s12","status":"idle"}"#,
        )
        .unwrap();
        let Some(_restore) = super::unlistable_fixture::unreadable_sessions_dir(&hidden) else {
            return;
        };

        let readings = read_records(
            &[readable, hidden.clone()],
            &[live(11), live(12)],
            &no_proc,
            RecordDepth::Identity,
        );
        assert_eq!(readings.unlistable.len(), 1, "{:?}", readings.unlistable);
        assert_eq!(readings.unlistable[0].sessions_dir, hidden.join("sessions"));
        assert_eq!(
            classify_from_record(&readings.by_pid[&11], NOW),
            Activity::Idle
        );
        assert_eq!(readings.by_pid[&12], RecordReading::Unlisted);
        assert_eq!(
            classify_from_record(&readings.by_pid[&12], NOW),
            Activity::Unknown
        );
    }

    /// While a dir is unlistable, an earlier holder's record in a dir that
    /// did list is `Unlisted`, not `PidReused`: it says nothing about the live
    /// process, whose own record may be in the dir nobody could read. Not
    /// gated: a `sessions` that is not a directory is unlistable to root too.
    #[test]
    fn a_reused_record_is_unlisted_while_another_dir_is_unlistable() {
        let root = scratch("reused-unlisted");
        let listed = root.path().join(".claude-a");
        std::fs::create_dir_all(listed.join("sessions")).unwrap();
        std::fs::write(
            listed.join("sessions/70.json"),
            r#"{"pid":70,"sessionId":"old","status":"idle","procStart":"400"}"#,
        )
        .unwrap();
        let broken = root.path().join(".claude-file");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("sessions"), "not a directory").unwrap();
        let kernel = |_: u32| Some("500".to_string());
        let readings = read_records(
            &[listed.clone()],
            &[live(70)],
            &kernel,
            RecordDepth::Identity,
        );
        assert_eq!(
            readings.by_pid[&70],
            RecordReading::PidReused,
            "every dir listed"
        );
        let readings = read_records(
            &[listed, broken],
            &[live(70)],
            &kernel,
            RecordDepth::Identity,
        );
        assert_eq!(readings.by_pid[&70], RecordReading::Unlisted);
    }

    /// A record listed and then deleted before it could be opened (its
    /// process exited) is gone — `Missing` — not an unreadable file.
    #[test]
    fn a_record_that_vanishes_before_the_open_is_missing() {
        let root = scratch("vanished");
        let dir = root.path().join(".claude");
        let gone = dir.join("sessions/30.json");
        assert_eq!(
            read_one(
                live(30),
                &[(dir.clone(), gone)],
                &no_proc,
                RecordDepth::Identity
            ),
            RecordReading::Missing
        );
    }

    /// Two dirs name one pid: an earlier holder's record, and one that does
    /// not parse. The unparseable one may be the live process's own record
    /// caught mid-write, so the pid is `Unparseable`, not `PidReused`.
    #[test]
    fn an_unreadable_candidate_outranks_a_reused_one() {
        let root = scratch("outrank");
        let a = root.path().join(".claude-a");
        let b = root.path().join(".claude-b");
        std::fs::create_dir_all(a.join("sessions")).unwrap();
        std::fs::create_dir_all(b.join("sessions")).unwrap();
        std::fs::write(
            a.join("sessions/40.json"),
            r#"{"pid":40,"sessionId":"old","status":"idle","procStart":"400"}"#,
        )
        .unwrap();
        std::fs::write(b.join("sessions/40.json"), r#"{"pid":40,"sess"#).unwrap();
        let kernel = |_: u32| Some("500".to_string());
        let readings = read_records(&[a, b], &[live(40)], &kernel, RecordDepth::Identity);
        assert_eq!(readings.by_pid[&40], RecordReading::Unparseable);
    }

    /// `Identity` never opens a transcript; `Activity` does.
    #[test]
    fn identity_depth_reads_no_transcript() {
        let root = scratch("depth");
        let dir = root.path().join(".claude");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let cwd = "/work/repo";
        std::fs::write(
            dir.join("sessions/50.json"),
            format!(r#"{{"pid":50,"sessionId":"s50","cwd":"{cwd}","status":"busy"}}"#),
        )
        .unwrap();
        let transcript = crate::terminal::transcript::session_transcript_path(&dir, cwd, "s50");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let recent = chrono::DateTime::from_timestamp_millis(NOW - MIN)
            .unwrap()
            .to_rfc3339();
        std::fs::write(
            &transcript,
            format!("{{\"type\":\"assistant\",\"timestamp\":\"{recent}\"}}\n"),
        )
        .unwrap();
        let last =
            |depth| match &read_records(&[dir.clone()], &[live(50)], &no_proc, depth).by_pid[&50] {
                RecordReading::Parsed(ev) => ev.last_message_ms,
                other => panic!("expected Parsed, got {other:?}"),
            };
        assert_eq!(last(RecordDepth::Identity), None);
        assert_eq!(last(RecordDepth::Activity), Some(NOW - MIN));
    }

    /// An invalid UTF-8 byte costs its own line, not the whole read.
    #[test]
    fn transcript_tail_with_invalid_utf8_is_read_lossily() {
        let root = scratch("lossy");
        let path = root.path().join("t.jsonl");
        let mut bytes = b"{\"type\":\"user\",\"timestamp\":\"2026-09-29T10:00:00Z\"}\n".to_vec();
        bytes.extend_from_slice(b"{\"type\":\"assistant\",\"text\":\"\xff\xfe\"}\n");
        std::fs::write(&path, &bytes).unwrap();
        let tail = read_tail_lossy(&path, TRANSCRIPT_TAIL_BYTES).expect("lossy read");
        assert_eq!(
            last_message_ms(&tail),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-09-29T10:00:00Z")
                    .unwrap()
                    .timestamp_millis()
            )
        );
        // A short tail starting mid-file drops its partial first line.
        let short = read_tail_lossy(&path, 30).unwrap();
        assert!(!short.contains("timestamp"), "{short:?}");
    }

    /// The unlistable log tracks each dir: one read reports a dir, a read
    /// that was not asked about it leaves it reported, and only a read that
    /// lists it again clears it.
    #[test]
    fn the_unlistable_log_tracks_each_dir_separately() {
        let log = UnlistableLog::new("test", "nothing");
        let a = PathBuf::from("/h/.claude-a");
        let b = PathBuf::from("/h/.claude-b");
        let unlistable_a = [UnlistableSessionsDir {
            sessions_dir: a.join("sessions"),
            error: "Permission denied".to_string(),
        }];
        log.observe(&[a.clone(), b.clone()], &unlistable_a);
        assert_eq!(log.reported(), vec![a.join("sessions")]);
        log.observe(std::slice::from_ref(&b), &[]);
        assert_eq!(
            log.reported(),
            vec![a.join("sessions")],
            "a read that never asked about a dir does not declare it healed"
        );
        log.observe(std::slice::from_ref(&a), &[]);
        assert!(
            log.reported().is_empty(),
            "a dir that lists again is cleared"
        );
    }

    #[test]
    fn counts_sum_every_added_process() {
        let mut c = ActivityCounts::default();
        for a in [
            Activity::Working,
            Activity::Idle,
            Activity::Idle,
            Activity::Stale,
            Activity::Unknown,
        ] {
            c.add(a);
        }
        assert_eq!(
            c,
            ActivityCounts {
                working: 1,
                idle: 2,
                stale: 1,
                unknown: 1
            }
        );
        assert_eq!(c.sum(), 5);
    }
}
