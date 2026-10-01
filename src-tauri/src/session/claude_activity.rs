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
//!    background `bash` loops. So `has_live_children == Some(true)` is only a
//!    VETO on a pane-idle verdict, and `None` (uncomputable) cannot vouch for
//!    one either.
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
//!    - a missing, unparseable, ambiguous or unrecognised-status record →
//!      `unknown`, and so is a record that belongs to an EARLIER process that
//!      held the same pid ([`RecordReading::PidReused`]): on Linux the
//!      record's `procStart` is compared with `/proc/<pid>/stat` field 22
//!      (both are the start time in clock ticks since boot — equal on every
//!      record checked on merytshost 2026-09-29); where that cannot be read,
//!      `startedAt` is compared with the process start the census implies
//!      (its snapshot time minus the process age) inside the ONE-SIDED window
//!      [`START_LEAD_MS`] / [`START_LAG_MS`]. An empty or non-numeric
//!      `procStart` counts as absent and falls through to that `startedAt`
//!      check. A record neither check can reach is accepted
//!      on its `pid` match alone — the census only lists live `claude`
//!      processes, so a reused pid would also have to be a `claude`.
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
    /// No `<pid>.json` in any config dir.
    Missing,
    /// A file exists but did not parse, or named a different pid.
    Unparseable,
    /// Two config dirs hold a record for this pid naming different sessions —
    /// one of them is a stale file from a crashed process. Not a guess.
    Ambiguous,
    /// The record's start stamp disagrees with the live process's: it was
    /// written by an earlier process that held the same pid.
    PidReused,
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

/// Does `raw` describe the process that holds `live.pid` NOW? `Some(false)` is
/// a positive mismatch (pid reuse); `None` means neither check could be made.
fn same_process(
    raw: &RawRecord,
    live: LivePid,
    proc_start: &dyn Fn(u32) -> Option<String>,
) -> Option<bool> {
    if let (Some(recorded), Some(actual)) = (raw.proc_start(), proc_start(live.pid)) {
        return Some(recorded == actual);
    }
    let lag_ms = raw.started_at? - live.process_started_ms?;
    Some((-START_LEAD_MS..=START_LAG_MS).contains(&lag_ms))
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

/// Read Claude Code's record for each of `pids` across `config_dirs`.
///
/// Each `sessions/` dir is listed ONCE and only the wanted `<pid>.json` files
/// are opened; the transcript tail is read only for `busy`/`shell` records.
/// `proc_start` is [`proc_start_ticks`] in production and injectable in tests.
/// Blocking file I/O — call it off the async executor.
pub fn read_records(
    config_dirs: &[PathBuf],
    pids: &[LivePid],
    proc_start: &dyn Fn(u32) -> Option<String>,
) -> HashMap<u32, RecordReading> {
    let wanted: std::collections::HashSet<u32> = pids.iter().map(|p| p.pid).collect();
    // pid -> every (config dir, record path) that names it.
    let mut found: HashMap<u32, Vec<(PathBuf, PathBuf)>> = HashMap::new();
    for dir in config_dirs {
        let Ok(entries) = std::fs::read_dir(dir.join("sessions")) else {
            continue; // no sessions dir on this account — normal
        };
        for entry in entries.flatten() {
            let path = entry.path();
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

    pids.iter()
        .map(|&live| {
            let reading = match found.get(&live.pid) {
                None => RecordReading::Missing,
                Some(candidates) => read_one(live, candidates, proc_start),
            };
            (live.pid, reading)
        })
        .collect()
}

fn read_one(
    live: LivePid,
    candidates: &[(PathBuf, PathBuf)],
    proc_start: &dyn Fn(u32) -> Option<String>,
) -> RecordReading {
    let mut parsed: Vec<(&Path, RawRecord, RecordEvidence)> = Vec::new();
    let mut reused = false;
    for (dir, path) in candidates {
        // A body that does not parse is only decisive when nothing else names
        // this pid — see the `let Some(..) else` below.
        let Ok((raw, ev)) = std::fs::read_to_string(path)
            .map_err(|_| ())
            .and_then(|b| parse_record(&b, live.pid))
        else {
            continue;
        };
        if same_process(&raw, live, proc_start) == Some(false) {
            reused = true;
            continue;
        }
        parsed.push((dir.as_path(), raw, ev));
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
        // `candidates` is non-empty, so each one either failed to parse or
        // belonged to an earlier holder of this pid.
        return if reused {
            RecordReading::PidReused
        } else {
            RecordReading::Unparseable
        };
    };
    if ev.status == "busy" || ev.status == "shell" {
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
            RecordReading::Unparseable,
            RecordReading::Ambiguous,
            RecordReading::PidReused,
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
        );
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
    /// the process starts), and each edge is pinned.
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
        rec(9, "", started + START_LAG_MS + 1); // one past the lag edge
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
        let readings = read_records(&[dir], &pids, &kernel);
        for pid in [1, 3, 5, 6, 7, 8, 10] {
            assert_eq!(
                classify_from_record(&readings[&pid], NOW),
                Activity::Idle,
                "pid {pid}: {:?}",
                readings[&pid]
            );
        }
        for pid in [2, 4, 9, 11] {
            assert_eq!(readings[&pid], RecordReading::PidReused, "pid {pid}");
            assert_eq!(
                classify_from_record(&readings[&pid], NOW),
                Activity::Unknown
            );
        }
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
        let readings = read_records(&[dir], &[live(20)], &no_proc);
        assert_eq!(classify_from_record(&readings[&20], NOW), Activity::Stale);
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
