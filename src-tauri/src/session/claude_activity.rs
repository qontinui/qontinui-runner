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
//!    ([`crate::session::wind_down_observer::TerminalObservation`]) plus the
//!    census's `has_live_children` hint. Sideband `working`, a busy grid, or a
//!    live child is `working`; sideband not-working + an idle grid + no
//!    children is `idle`. Anything else is not decisive and falls through.
//! 2. **Claude Code's own per-process record**
//!    `<config-dir>/sessions/<pid>.json` (`status`, `statusUpdatedAt`,
//!    `sessionId`, `cwd`) — for every process the pane cannot decide,
//!    including every process the runner did not spawn:
//!    - `idle` / `waiting` → `idle`;
//!    - `busy` / `shell` → `working` iff the session's transcript
//!      (`<config-dir>/projects/<encoded cwd>/<sessionId>.jsonl`) carries a
//!      real `user`/`assistant` line within [`ACTIVE_WINDOW_MS`], else
//!      `stale`. When the transcript cannot be located, or its tail carries no
//!      parseable message line (one tool result larger than the tail read can
//!      hide every message), the record's own `statusUpdatedAt` age stands in
//!      for the message age — a weaker signal (a `busy` status stamped 40 min
//!      ago by one long tool call reads `stale`), used only because the
//!      stronger one is absent. No `statusUpdatedAt` either is `unknown`;
//!    - a missing, unparseable, ambiguous or unrecognised-status record →
//!      `unknown`.
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
/// message within this window (30 min, the plan's `--active-minutes` default).
pub const ACTIVE_WINDOW_MS: i64 = 30 * 60 * 1000;

/// How much of a transcript's tail is read to find its last message line.
/// Only `busy`/`shell` records pay it (single digits to low tens on a
/// 250-session box), so it is bounded per request.
const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;

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
}

/// Classify from the runner's pane observation. `None` means "not decisive —
/// fall back to the record", never "idle".
pub fn classify_from_pane(
    observation: &TerminalObservation,
    has_live_children: Option<bool>,
) -> Option<Activity> {
    let sideband_working = matches!(
        observation.sideband,
        Sideband::Reported {
            state: SidebandState::Working,
            ..
        }
    );
    if sideband_working || observation.grid == GridIdle::Busy || has_live_children == Some(true) {
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
    // `has_live_children: None` is UNCOMPUTABLE, not "no children" — it does
    // not get to vouch for idleness; the record decides instead.
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
        .and_then(|obs| classify_from_pane(obs, has_live_children))
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

/// Read Claude Code's record for each of `pids` across `config_dirs`.
///
/// Each `sessions/` dir is listed ONCE and only the wanted `<pid>.json` files
/// are opened; the transcript tail is read only for `busy`/`shell` records.
/// Blocking file I/O — call it off the async executor.
pub fn read_records(config_dirs: &[PathBuf], pids: &[u32]) -> HashMap<u32, RecordReading> {
    let wanted: std::collections::HashSet<u32> = pids.iter().copied().collect();
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
        .map(|&pid| {
            let reading = match found.get(&pid) {
                None => RecordReading::Missing,
                Some(candidates) => read_one(pid, candidates),
            };
            (pid, reading)
        })
        .collect()
}

fn read_one(pid: u32, candidates: &[(PathBuf, PathBuf)]) -> RecordReading {
    let mut parsed: Vec<(&Path, RawRecord, RecordEvidence)> = Vec::new();
    for (dir, path) in candidates {
        match std::fs::read_to_string(path)
            .map_err(|_| ())
            .and_then(|b| parse_record(&b, pid))
        {
            Ok((raw, ev)) => parsed.push((dir.as_path(), raw, ev)),
            // A body that does not parse is only decisive when nothing else
            // names this pid — see the `let Some(..) else` below.
            Err(()) => {}
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
        // `candidates` is non-empty, so every one of them failed to parse.
        return RecordReading::Unparseable;
    };
    if ev.status == "busy" || ev.status == "shell" {
        if let (Some(cwd), Some(session_id)) = (raw.cwd.as_deref(), raw.session_id.as_deref()) {
            let transcript =
                crate::terminal::transcript::session_transcript_path(dir, cwd, session_id);
            ev.last_message_ms =
                crate::terminal::transcript::read_tail_bytes(&transcript, TRANSCRIPT_TAIL_BYTES)
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

    const WORKING: Sideband = Sideband::Reported {
        state: SidebandState::Working,
        set_at_ms: 1,
    };
    const NOT_WORKING: Sideband = Sideband::Reported {
        state: SidebandState::NotWorking,
        set_at_ms: 1,
    };
    const IDLE_GRID: GridIdle = GridIdle::Idle { since_ms: 1 };

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
    fn record_missing_unparseable_ambiguous_or_unknown_status_is_unknown_never_idle() {
        for reading in [
            RecordReading::Missing,
            RecordReading::Unparseable,
            RecordReading::Ambiguous,
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
    fn pane_working_signals_each_decide_working() {
        let unknown = RecordReading::Missing;
        assert_eq!(
            classify(Some(&obs(WORKING, IDLE_GRID)), Some(false), &unknown, NOW),
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
        assert_eq!(
            classify(
                Some(&obs(NOT_WORKING, IDLE_GRID)),
                Some(true),
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
            classify(None, Some(true), &parsed("idle", None, None), NOW),
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
            format!(r#"{{"pid":10,"sessionId":"s10","cwd":"{cwd}","status":"busy","statusUpdatedAt":{}}}"#, NOW - 600 * MIN),
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

        let readings = read_records(
            &[dir.clone(), other, root.path().join("absent")],
            &[10, 11, 12, 13, 14, 15],
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

    #[test]
    fn busy_record_with_no_transcript_uses_status_age() {
        let root = scratch("no-transcript");
        let dir = root.path().join(".claude");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(
            dir.join("sessions/20.json"),
            format!(r#"{{"pid":20,"sessionId":"s20","cwd":"/nowhere","status":"shell","statusUpdatedAt":{}}}"#, NOW - 90 * MIN),
        )
        .unwrap();
        let readings = read_records(&[dir], &[20]);
        assert_eq!(classify_from_record(&readings[&20], NOW), Activity::Stale);
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
