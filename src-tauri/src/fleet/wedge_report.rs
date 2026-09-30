//! G2 — wedge incidents, reported outward (plan
//! `2026-09-20-the-second-ratchet-domain-is-operations-and-its-cost-is-compared-to-the-first`,
//! Phase 5).
//!
//! The runner detects its own wedges in-process (`health_monitor.rs`,
//! `webview_recovery.rs`) and `coord_outside_observer.rs` records what it can
//! see of coord; all of them write `wedge-incidents.log` through
//! `health_monitor::append_wedge_incident` (and the watchdog's own appender),
//! which stays the SINGLE WRITER of that file. This module only READS it: the
//! tail since a persisted cursor, folded into a bounded list of incidents that
//! rides the device-status heartbeat as `details.wedge_incidents`. Without it,
//! coord sees a wedged-but-listening runner as nothing more than a stale
//! heartbeat, indistinguishable from a departed box.
//!
//! **Boundary.** Detection stays inside the runner process and recovery stays
//! in `webview_recovery.rs`; this is a report, and nothing user-facing depends
//! on anyone reading it.
//!
//! ## The two line shapes
//!
//! ```text
//! {rfc3339} {reason} {detail} (pid {pid})                 append_wedge_incident
//! {rfc3339} WATCHDOG {reason} — pid {pid}, probe …         the watchdog thread
//! ```
//!
//! The watchdog's reasons are hyphenated; they are mapped onto the same kind
//! vocabulary (`backend-wedged` IS `backend_wedged` — the watchdog reads the
//! monitor's own flag). A reason this module does not know is published as its
//! raw token, never dropped and never coerced into a known kind. A line whose
//! timestamp does not parse is skipped (it names no instant to report).
//!
//! ## When an incident ends — the rule, per kind
//!
//! The log records ONSETS only. `ended_at` is therefore set only from evidence
//! the runner actually holds, and `ended_by` names which evidence it was, so a
//! reader never has to guess how an end was derived:
//!
//! | kind | ends when | `ended_by` | `ended_at` |
//! |---|---|---|---|
//! | `backend_wedged`, `ui_thread_wedged`, `recovery_wedged` | this process's live predicate (`health_monitor::backend_wedged()` / `ui_thread_wedged()` / `webview_recovery::recovery_wedged()`) reads false | `cleared` | the first read that saw it false (an upper bound) |
//! | same three, written by ANOTHER runner process (pid differs) | a later line from a different pid, or else the first read by this process — one runner instance writes a given log, and a process's wedge cannot outlive the process | `process_exited` | that later line, else that read |
//! | `health_monitor_thread_stalled`, `health_metrics_thread_stalled` (watchdog, re-written every `WATCHDOG_REPEAT_SECS` while they hold) | no line for 2 × that cadence | `silent` | last line + one cadence (the latest instant the cadence allows it to have held) |
//! | `coord_*` and any unknown reason (written once per episode, latched) | a newer onset of the same kind — the writer's latch re-armed, so the earlier episode was over | `superseded` | the newer onset |
//! | same, with no newer onset | its onset is older than [`OPEN_WINDOW_SECS`] | `expired` | onset + that window — the runner stops vouching for it, which is NOT an observed end |
//!
//! Any kind is also `superseded` when a new episode of it begins while an
//! earlier one is still open. Lines that continue an open episode (the
//! watchdog's repeats, or the monitor's one escalation line landing beside the
//! watchdog's for the same wedge) extend it rather than opening another. One
//! stated undercount: two episodes of a live-predicate kind that begin AND end
//! entirely between two reads are indistinguishable from one when both lines
//! came from the watchdog alone.
//!
//! ## Cursor, persistence, rotation
//!
//! State (cursor + incidents + a small pid→build registry) is persisted as
//! [`STATE_FILE_NAME`] BESIDE the log, in the instance-scoped dev-logs
//! directory: the cursor indexes that one file, so it moves with a dev-logs
//! override and a secondary instance (which logs under `instance-<name>/`) never
//! shares a cursor with the primary. The cursor is a byte offset plus a
//! SHA-256 of the file's first bytes, so a truncated file (shorter than the
//! offset) or a replaced one (different head) is read again from byte 0 —
//! nothing is re-reported from the old file, because its lines are gone, and
//! nothing is lost from the new one. Only complete lines are consumed; a line
//! still being written is read on the next pass. The incidents are persisted
//! with the cursor, so a restart neither re-reports what was read nor loses
//! what was appended while the runner was down. A missing or corrupt state
//! file rebuilds the list from the whole log, which is correct, just slower.
//!
//! `build_id` is the build that WROTE the line: this binary's `git_sha` for
//! this process's pid, else the build a previous process registered for its
//! pid when it first read the log, else `null` (a process that died before its
//! first read never registered, and guessing the current build would misfile
//! a previous binary's wedge).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

/// Newest incidents published (and kept).
pub(crate) const WEDGE_REPORT_BOUND: usize = 20;

/// How long a once-per-episode incident with no end evidence stays open.
pub(crate) const OPEN_WINDOW_SECS: i64 = 86_400;

/// Persisted state, beside `wedge-incidents.log`.
pub(crate) const STATE_FILE_NAME: &str = "wedge-incidents.report-state.json";

/// Bytes of the file head fingerprinted into the cursor.
const HEAD_BYTES: u64 = 256;

/// Most bytes one pass reads. The first pass on a long-lived box reads the
/// whole history; past this, only the newest tail is folded (the list keeps
/// the newest 20 anyway), starting at the first complete line.
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// pid→build entries remembered.
const BUILD_REGISTRY_BOUND: usize = 16;

/// Schema version of the persisted state; a mismatch rebuilds from the log.
const STATE_VERSION: u32 = 1;

/// How an incident's end was established. See the module table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EndedBy {
    Cleared,
    ProcessExited,
    Silent,
    Superseded,
    Expired,
}

/// How a kind's end is established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndRule {
    /// A live in-process predicate answers "does it still hold?".
    LivePredicate,
    /// Re-written every watchdog cadence while it holds.
    Repeating,
    /// Written once per episode by a latched writer.
    OncePerEpisode,
}

fn end_rule(kind: &str) -> EndRule {
    match kind {
        "backend_wedged" | "ui_thread_wedged" | "recovery_wedged" => EndRule::LivePredicate,
        "health_monitor_thread_stalled" | "health_metrics_thread_stalled" => EndRule::Repeating,
        _ => EndRule::OncePerEpisode,
    }
}

/// The watchdog's cadence, from its single definition.
fn repeat_secs() -> i64 {
    crate::health_monitor::WATCHDOG_REPEAT_SECS
}

/// One parsed log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Onset {
    pub at: DateTime<Utc>,
    pub kind: String,
    pub pid: Option<u32>,
    /// Watchdog lines repeat while the condition holds; every other writer
    /// writes once per episode.
    pub repeats: bool,
}

/// Parse one line of either shape. `None` for a blank line or one whose
/// timestamp does not parse.
pub(crate) fn parse_line(line: &str) -> Option<Onset> {
    let line = line.trim_end_matches(['\r', '\n']);
    let mut tokens = line.splitn(3, ' ');
    let at = DateTime::parse_from_rfc3339(tokens.next()?)
        .ok()?
        .with_timezone(&Utc);
    let second = tokens.next().filter(|s| !s.is_empty())?;
    let rest = tokens.next().unwrap_or("");
    if second == "WATCHDOG" {
        let reason = rest.split_whitespace().next()?;
        let kind = match reason {
            "backend-wedged" => "backend_wedged".to_string(),
            "health-monitor-thread-stalled" => "health_monitor_thread_stalled".to_string(),
            "health-metrics-thread-stalled" => "health_metrics_thread_stalled".to_string(),
            other => other.to_string(),
        };
        // "— pid {pid}, probe heartbeat …"
        let pid = rest
            .split_once("pid ")
            .and_then(|(_, after)| leading_u32(after));
        Some(Onset {
            at,
            kind,
            pid,
            repeats: true,
        })
    } else {
        // "… (pid {pid})" at the very end.
        let pid = line
            .strip_suffix(')')
            .and_then(|s| s.rsplit_once("(pid "))
            .map(|(_, digits)| digits)
            .and_then(|s| s.parse::<u32>().ok());
        Some(Onset {
            at,
            kind: second.to_string(),
            pid,
            repeats: false,
        })
    }
}

fn leading_u32(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// One incident, as kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Incident {
    pub kind: String,
    pub began_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub ended_by: Option<EndedBy>,
    pub build_id: Option<String>,
    pub pid: Option<u32>,
    pub last_seen_at: DateTime<Utc>,
    /// Whether this episode already holds its one non-repeating line.
    pub once_line_seen: bool,
}

impl Incident {
    fn close(&mut self, at: DateTime<Utc>, by: EndedBy) {
        // Never before the onset, whatever the evidence's clock says.
        self.ended_at = Some(at.max(self.began_at));
        self.ended_by = Some(by);
    }

    /// The published shape: `{kind, began_at, ended_at, ended_by, build_id}`.
    fn published(&self) -> serde_json::Value {
        let ts = |d: &DateTime<Utc>| d.to_rfc3339_opts(SecondsFormat::Secs, true);
        serde_json::json!({
            "kind": self.kind,
            "began_at": ts(&self.began_at),
            "ended_at": self.ended_at.as_ref().map(ts),
            "ended_by": self.ended_by,
            "build_id": self.build_id,
        })
    }
}

/// Where the reader stopped, and which file it stopped in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cursor {
    pub offset: u64,
    pub head_len: u64,
    pub head_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BuildRegistration {
    pub pid: u32,
    pub build_id: String,
}

/// Everything persisted across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WedgeReportState {
    pub version: u32,
    pub cursor: Option<Cursor>,
    pub incidents: Vec<Incident>,
    pub builds: Vec<BuildRegistration>,
}

impl Default for WedgeReportState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            cursor: None,
            incidents: Vec::new(),
            builds: Vec::new(),
        }
    }
}

/// This process's view, sampled once per pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveView {
    pub pid: u32,
    pub build_id: Option<&'static str>,
    pub backend_wedged: bool,
    pub ui_thread_wedged: bool,
    pub recovery_wedged: bool,
}

impl LiveView {
    fn sample() -> Self {
        Self {
            pid: std::process::id(),
            build_id: crate::fleet::served_git_sha(),
            backend_wedged: crate::health_monitor::backend_wedged(),
            ui_thread_wedged: crate::health_monitor::ui_thread_wedged(),
            recovery_wedged: crate::webview_recovery::recovery_wedged(),
        }
    }

    fn holds(&self, kind: &str) -> bool {
        match kind {
            "backend_wedged" => self.backend_wedged,
            "ui_thread_wedged" => self.ui_thread_wedged,
            "recovery_wedged" => self.recovery_wedged,
            _ => false,
        }
    }
}

impl WedgeReportState {
    fn register_build(&mut self, live: &LiveView) {
        let Some(build) = live.build_id else { return };
        if self.builds.iter().any(|b| b.pid == live.pid) {
            return;
        }
        self.builds.push(BuildRegistration {
            pid: live.pid,
            build_id: build.to_string(),
        });
        let excess = self.builds.len().saturating_sub(BUILD_REGISTRY_BOUND);
        self.builds.drain(..excess);
    }

    fn build_for(&self, pid: Option<u32>, live: &LiveView) -> Option<String> {
        let pid = pid?;
        if pid == live.pid {
            return live.build_id.map(str::to_string);
        }
        self.builds
            .iter()
            .find(|b| b.pid == pid)
            .map(|b| b.build_id.clone())
    }

    /// Close whatever the clock alone proves over (`silent`, `expired`).
    fn settle_by_time(&mut self, now: DateTime<Utc>) {
        let cadence = repeat_secs();
        for inc in self.incidents.iter_mut().filter(|i| i.ended_at.is_none()) {
            match end_rule(&inc.kind) {
                EndRule::Repeating => {
                    if (now - inc.last_seen_at).num_seconds() > 2 * cadence {
                        let at = inc.last_seen_at + ChronoDuration::seconds(cadence);
                        inc.close(at, EndedBy::Silent);
                    }
                }
                EndRule::OncePerEpisode => {
                    if (now - inc.began_at).num_seconds() > OPEN_WINDOW_SECS {
                        let at = inc.began_at + ChronoDuration::seconds(OPEN_WINDOW_SECS);
                        inc.close(at, EndedBy::Expired);
                    }
                }
                EndRule::LivePredicate => {}
            }
        }
    }

    /// Fold one line in, in file order.
    pub(crate) fn fold(&mut self, onset: Onset, live: &LiveView) {
        self.settle_by_time(onset.at);
        // One runner instance writes a given log (`instance::scope_path`), so a
        // line from pid P proves every OTHER pid's process has exited — and a
        // process's wedge cannot outlive it.
        if let Some(writer) = onset.pid {
            for inc in self.incidents.iter_mut().filter(|i| {
                i.ended_at.is_none()
                    && end_rule(&i.kind) == EndRule::LivePredicate
                    && i.pid.is_some_and(|p| p != writer)
            }) {
                inc.close(onset.at, EndedBy::ProcessExited);
            }
        }
        let open = self
            .incidents
            .iter_mut()
            .rev()
            .find(|i| i.kind == onset.kind && i.ended_at.is_none());
        if let Some(open) = open {
            let continues = open.pid == onset.pid && (onset.repeats || !open.once_line_seen);
            if continues {
                open.last_seen_at = open.last_seen_at.max(onset.at);
                open.once_line_seen |= !onset.repeats;
                return;
            }
            open.close(onset.at, EndedBy::Superseded);
        }
        let build_id = self.build_for(onset.pid, live);
        self.incidents.push(Incident {
            kind: onset.kind,
            began_at: onset.at,
            ended_at: None,
            ended_by: None,
            build_id,
            pid: onset.pid,
            last_seen_at: onset.at,
            once_line_seen: !onset.repeats,
        });
        let excess = self.incidents.len().saturating_sub(WEDGE_REPORT_BOUND);
        self.incidents.drain(..excess);
    }

    /// Close what this process's own live state says is over.
    pub(crate) fn settle(&mut self, now: DateTime<Utc>, live: &LiveView) {
        self.settle_by_time(now);
        for inc in self.incidents.iter_mut().filter(|i| i.ended_at.is_none()) {
            if end_rule(&inc.kind) != EndRule::LivePredicate {
                continue;
            }
            match inc.pid {
                Some(pid) if pid != live.pid => inc.close(now, EndedBy::ProcessExited),
                _ if !live.holds(&inc.kind) => inc.close(now, EndedBy::Cleared),
                _ => {}
            }
        }
    }

    /// The published array, oldest first.
    pub(crate) fn published(&self) -> serde_json::Value {
        serde_json::Value::Array(self.incidents.iter().map(Incident::published).collect())
    }
}

/// What reading the log tail produced.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TailRead {
    /// No log file exists (nothing ever wedged here, or it was removed).
    Absent,
    /// New complete lines since the cursor, and the cursor to persist.
    /// `restarted` is true when a previous cursor was rejected (the file was
    /// truncated or replaced) and the read began again at byte 0.
    Lines {
        lines: Vec<String>,
        cursor: Cursor,
        restarted: bool,
    },
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_prefix(f: &mut std::fs::File, n: u64) -> std::io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(0))?;
    let mut buf = Vec::with_capacity(n as usize);
    f.by_ref().take(n).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Read the complete lines appended since `cursor`.
///
/// A cursor that no longer describes the file — the file is shorter than the
/// offset (truncated) or its head differs (replaced / rotated) — restarts at
/// byte 0.
pub(crate) fn read_tail(path: &Path, cursor: Option<&Cursor>) -> std::io::Result<TailRead> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TailRead::Absent),
        Err(e) => return Err(e),
    };
    let len = f.metadata()?.len();
    let start = match cursor {
        Some(c) if c.offset <= len && c.head_len <= len => {
            let head = read_prefix(&mut f, c.head_len)?;
            if sha256_hex(&head) == c.head_sha256 {
                c.offset
            } else {
                0
            }
        }
        _ => 0,
    };
    // Bound one pass: past MAX_READ_BYTES, skip to the newest tail and drop
    // the partial line the skip lands in.
    let (seek_to, drop_partial) = if len - start > MAX_READ_BYTES {
        (len - MAX_READ_BYTES, true)
    } else {
        (start, false)
    };
    f.seek(SeekFrom::Start(seek_to))?;
    let mut buf = Vec::with_capacity((len - seek_to) as usize);
    f.by_ref().take(len - seek_to).read_to_end(&mut buf)?;
    let mut body: &[u8] = &buf;
    let mut consumed_base = seek_to;
    if drop_partial {
        match body.iter().position(|&b| b == b'\n') {
            Some(i) => {
                body = &body[i + 1..];
                consumed_base += (i + 1) as u64;
            }
            None => body = &[],
        }
    }
    let complete = body
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let lines = String::from_utf8_lossy(&body[..complete])
        .lines()
        .map(str::to_string)
        .collect();
    let offset = consumed_base + complete as u64;
    let head_len = len.min(HEAD_BYTES);
    let head = read_prefix(&mut f, head_len)?;
    Ok(TailRead::Lines {
        lines,
        cursor: Cursor {
            offset,
            head_len,
            head_sha256: sha256_hex(&head),
        },
        restarted: cursor.is_some_and(|c| c.offset > 0) && start == 0,
    })
}

fn load_state(path: &Path) -> WedgeReportState {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return WedgeReportState::default(),
        Err(e) => {
            warn!(
                "fleet::wedge_report: state {} unreadable ({e}); rebuilding from the whole log",
                path.display()
            );
            return WedgeReportState::default();
        }
    };
    match serde_json::from_slice::<WedgeReportState>(&bytes) {
        Ok(s) if s.version == STATE_VERSION => s,
        Ok(s) => {
            warn!(
                "fleet::wedge_report: state version {} != {STATE_VERSION}; rebuilding from the \
                 whole log",
                s.version
            );
            WedgeReportState::default()
        }
        Err(e) => {
            warn!(
                "fleet::wedge_report: state {} is corrupt ({e}); rebuilding from the whole log",
                path.display()
            );
            WedgeReportState::default()
        }
    }
}

/// Atomic persist: sibling temp file carrying the pid, then rename.
fn persist_state(path: &Path, state: &WedgeReportState) -> Result<(), String> {
    let bytes = serde_json::to_vec(state).map_err(|e| format!("serialising state: {e}"))?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), path.display())
    })
}

/// In-process copy of the state, so a failed persist costs nothing until a
/// restart, and consecutive passes never race each other's read-modify-write.
static STATE: Mutex<Option<(PathBuf, WedgeReportState)>> = Mutex::new(None);

/// One pass over an explicit log path: read the tail, fold, settle, persist.
///
/// `Ok(array)` is the `details.wedge_incidents` value; `Err` means the log
/// exists but could not be read, and the key must be omitted.
pub(crate) fn report_from(
    log_path: &Path,
    now: DateTime<Utc>,
    live: &LiveView,
) -> Result<serde_json::Value, String> {
    let state_path = log_path.with_file_name(STATE_FILE_NAME);
    let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if guard.as_ref().is_some_and(|(p, _)| *p != state_path) {
        *guard = None;
    }
    let (_, state) = guard.get_or_insert_with(|| (state_path.clone(), load_state(&state_path)));
    state.register_build(live);
    let tail = read_tail(log_path, state.cursor.as_ref())
        .map_err(|e| format!("reading {} failed: {e}", log_path.display()))?;
    match tail {
        TailRead::Absent => state.cursor = None,
        TailRead::Lines {
            lines,
            cursor,
            restarted,
        } => {
            if restarted {
                tracing::info!(
                    "fleet::wedge_report: {} was truncated or replaced; reading it from byte 0",
                    log_path.display()
                );
            }
            for line in &lines {
                if let Some(onset) = parse_line(line) {
                    state.fold(onset, live);
                }
            }
            state.cursor = Some(cursor);
        }
    }
    state.settle(now, live);
    if let Err(e) = persist_state(&state_path, state) {
        warn!("fleet::wedge_report: state not persisted ({e}); a restart re-reads the log");
    }
    Ok(state.published())
}

/// Write this module's key into the heartbeat `details` object:
/// `wedge_incidents` (an array; `[]` when no log exists), or
/// `wedge_incidents_error` when the log could not be read. Blocking IO.
pub(crate) fn publish_into(details: &mut serde_json::Map<String, serde_json::Value>) {
    let log = crate::health_monitor::wedge_incidents_path(&crate::paths::get_dev_logs_dir());
    match report_from(&log, Utc::now(), &LiveView::sample()) {
        Ok(v) => {
            details.insert("wedge_incidents".to_string(), v);
        }
        Err(e) => {
            details.insert("wedge_incidents_error".to_string(), serde_json::json!(e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const ME: u32 = 4242;

    fn live(backend: bool) -> LiveView {
        LiveView {
            pid: ME,
            build_id: Some("abc123def456"),
            backend_wedged: backend,
            ui_thread_wedged: false,
            recovery_wedged: false,
        }
    }

    fn monitor_line(ts: &str, reason: &str, pid: u32) -> String {
        format!("{ts} {reason} runner backend wedged — /livez silent for 25s (pid {pid})\n")
    }

    fn watchdog_line(ts: &str, reason: &str, pid: u32) -> String {
        format!(
            "{ts} WATCHDOG {reason} — pid {pid}, probe heartbeat 3s old, metrics heartbeat \
             4s old, consecutive /livez failures 5. Written by the runtime-independent \
             watchdog thread, so this line survives a fully parked runtime.\n"
        )
    }

    fn append(path: &Path, s: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    /// Drop the in-process cache, as a restart would.
    fn restart() {
        *STATE.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Tests share the process-global cache; serialise them.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn parses_the_monitor_shape() {
        let o = parse_line(&monitor_line(
            "2026-09-29T15:36:51.923429718+00:00",
            "backend_wedged",
            81,
        ))
        .unwrap();
        assert_eq!(o.kind, "backend_wedged");
        assert_eq!(o.pid, Some(81));
        assert!(!o.repeats);
        assert_eq!(o.at, at("2026-09-29T15:36:51.923429718Z"));
    }

    #[test]
    fn parses_the_real_coord_liveness_line() {
        let l = "2026-09-29T21:13:46.277483151+00:00 coord_liveness_unknown Coord liveness is \
                 UNKNOWN to this runner — this runner has had NO usable observation (122 \
                 counted in all) — predicate (ii) was not observed on this cycle (pid 813952)";
        let o = parse_line(l).unwrap();
        assert_eq!(o.kind, "coord_liveness_unknown");
        assert_eq!(o.pid, Some(813952));
    }

    #[test]
    fn parses_the_watchdog_shape_and_maps_its_hyphenated_reasons() {
        let o = parse_line(&watchdog_line(
            "2026-09-29T10:00:00+00:00",
            "backend-wedged",
            7,
        ))
        .unwrap();
        assert_eq!(o.kind, "backend_wedged");
        assert_eq!(o.pid, Some(7));
        assert!(o.repeats);
        let o = parse_line(&watchdog_line(
            "2026-09-29T10:00:00+00:00",
            "health-monitor-thread-stalled",
            7,
        ))
        .unwrap();
        assert_eq!(o.kind, "health_monitor_thread_stalled");
    }

    #[test]
    fn unknown_reasons_survive_as_their_raw_token_and_bad_stamps_are_skipped() {
        let o = parse_line("2026-09-29T10:00:00Z brand_new_reason something (pid 3)").unwrap();
        assert_eq!(o.kind, "brand_new_reason");
        assert_eq!(
            parse_line("2026-09-29T10:00:00Z WATCHDOG future-thing — pid 3, x")
                .unwrap()
                .kind,
            "future-thing"
        );
        assert_eq!(parse_line("not-a-date backend_wedged x (pid 1)"), None);
        assert_eq!(parse_line(""), None);
        // A line with no pid is kept, attributed to no process.
        assert_eq!(
            parse_line("2026-09-29T10:00:00Z coord_no_leader x")
                .unwrap()
                .pid,
            None
        );
    }

    #[test]
    fn watchdog_repeats_and_the_monitor_escalation_are_one_episode_until_cleared() {
        let mut s = WedgeReportState::default();
        let lv = live(true);
        for l in [
            watchdog_line("2026-09-30T10:00:00Z", "backend-wedged", ME),
            monitor_line("2026-09-30T10:00:05Z", "backend_wedged", ME),
            watchdog_line("2026-09-30T10:05:00Z", "backend-wedged", ME),
        ] {
            s.fold(parse_line(&l).unwrap(), &lv);
        }
        s.settle(at("2026-09-30T10:06:00Z"), &lv);
        assert_eq!(s.incidents.len(), 1);
        assert_eq!(s.incidents[0].ended_at, None, "the predicate still holds");
        assert_eq!(s.incidents[0].build_id.as_deref(), Some("abc123def456"));
        s.settle(at("2026-09-30T10:11:00Z"), &live(false));
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::Cleared));
        assert_eq!(s.incidents[0].ended_at, Some(at("2026-09-30T10:11:00Z")));
        // A second monitor escalation is a NEW episode.
        s.fold(
            parse_line(&monitor_line("2026-09-30T11:00:00Z", "backend_wedged", ME)).unwrap(),
            &lv,
        );
        assert_eq!(s.incidents.len(), 2);
    }

    #[test]
    fn another_processes_wedge_ends_as_process_exited_with_its_registered_build() {
        let mut s = WedgeReportState::default();
        s.builds.push(BuildRegistration {
            pid: 99,
            build_id: "oldbuild0001".into(),
        });
        let lv = live(true); // THIS process is wedged; pid 99's wedge is still over.
        s.fold(
            parse_line(&monitor_line(
                "2026-09-30T09:00:00Z",
                "ui_thread_wedged",
                99,
            ))
            .unwrap(),
            &lv,
        );
        s.fold(
            parse_line(&monitor_line("2026-09-30T09:01:00Z", "backend_wedged", 98)).unwrap(),
            &lv,
        );
        s.settle(at("2026-09-30T10:00:00Z"), &lv);
        assert_eq!(s.incidents[0].build_id.as_deref(), Some("oldbuild0001"));
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::ProcessExited));
        assert_eq!(
            s.incidents[1].build_id, None,
            "an unregistered pid's build is unknown"
        );
        assert_eq!(s.incidents[1].ended_by, Some(EndedBy::ProcessExited));
    }

    #[test]
    fn latched_kinds_are_superseded_by_a_newer_onset_and_expire_after_the_window() {
        let mut s = WedgeReportState::default();
        let lv = live(false);
        for ts in ["2026-09-29T10:00:00Z", "2026-09-29T12:00:00Z"] {
            s.fold(
                parse_line(&format!("{ts} coord_liveness_unknown x (pid {ME})")).unwrap(),
                &lv,
            );
        }
        s.settle(at("2026-09-29T13:00:00Z"), &lv);
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::Superseded));
        assert_eq!(s.incidents[0].ended_at, Some(at("2026-09-29T12:00:00Z")));
        assert_eq!(
            s.incidents[1].ended_at, None,
            "the newest stays open inside the window"
        );
        s.settle(at("2026-09-30T12:00:01Z"), &lv);
        assert_eq!(s.incidents[1].ended_by, Some(EndedBy::Expired));
        assert_eq!(s.incidents[1].ended_at, Some(at("2026-09-30T12:00:00Z")));
    }

    #[test]
    fn a_watchdog_stall_that_falls_silent_closes_one_cadence_after_its_last_line() {
        let mut s = WedgeReportState::default();
        let lv = live(false);
        let cadence = repeat_secs();
        let t0 = at("2026-09-30T10:00:00Z");
        for i in 0..3 {
            let ts = (t0 + ChronoDuration::seconds(i * cadence)).to_rfc3339();
            s.fold(
                parse_line(&watchdog_line(&ts, "health-metrics-thread-stalled", ME)).unwrap(),
                &lv,
            );
        }
        assert_eq!(s.incidents.len(), 1, "repeats extend one episode");
        let last = t0 + ChronoDuration::seconds(2 * cadence);
        s.settle(last + ChronoDuration::seconds(2 * cadence), &lv);
        assert_eq!(
            s.incidents[0].ended_at, None,
            "not yet silent past two cadences"
        );
        s.settle(last + ChronoDuration::seconds(2 * cadence + 1), &lv);
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::Silent));
        assert_eq!(
            s.incidents[0].ended_at,
            Some(last + ChronoDuration::seconds(cadence))
        );
    }

    #[test]
    fn the_list_keeps_the_newest_twenty() {
        let mut s = WedgeReportState::default();
        let lv = live(false);
        let t0 = at("2026-09-30T00:00:00Z");
        for i in 0..25 {
            let ts = (t0 + ChronoDuration::minutes(i)).to_rfc3339();
            s.fold(
                parse_line(&format!("{ts} coord_no_leader x (pid {ME})")).unwrap(),
                &lv,
            );
        }
        assert_eq!(s.incidents.len(), WEDGE_REPORT_BOUND);
        assert_eq!(s.incidents[0].began_at, t0 + ChronoDuration::minutes(5));
        assert_eq!(s.published().as_array().unwrap().len(), WEDGE_REPORT_BOUND);
    }

    #[test]
    fn cursor_survives_a_restart_with_no_re_report_and_no_loss() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        restart();
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("wedge-incidents.log");
        let lv = live(false);
        let now = at("2026-09-30T12:00:00Z");
        append(
            &log,
            &format!("2026-09-30T10:00:00Z coord_unreachable a (pid {ME})\n"),
        );
        let v = report_from(&log, now, &lv).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);

        // Restart; append while "down"; the new line is read, the old one not
        // re-folded (it would otherwise supersede itself into two rows).
        restart();
        append(
            &log,
            &format!("2026-09-30T11:00:00Z coord_worker_dead b (pid {ME})\n"),
        );
        let v = report_from(&log, now, &lv).unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2, "{v}");
        assert_eq!(rows[0]["kind"], "coord_unreachable");
        assert_eq!(rows[1]["kind"], "coord_worker_dead");

        // A pass with nothing new changes nothing.
        restart();
        assert_eq!(report_from(&log, now, &lv).unwrap(), v);

        // A partial line is left for the next pass, then read whole.
        append(&log, "2026-09-30T11:30:00Z coord_no_le");
        assert_eq!(
            report_from(&log, now, &lv)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        append(&log, &format!("ader c (pid {ME})\n"));
        let v = report_from(&log, now, &lv).unwrap();
        assert_eq!(v.as_array().unwrap()[2]["kind"], "coord_no_leader");
        restart();
    }

    #[test]
    fn truncation_and_replacement_restart_from_byte_zero() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        restart();
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("wedge-incidents.log");
        let lv = live(false);
        let now = at("2026-09-30T12:00:00Z");
        append(
            &log,
            &format!("2026-09-30T08:00:00Z coord_unreachable a (pid {ME})\n"),
        );
        append(
            &log,
            &format!("2026-09-30T08:10:00Z coord_no_leader b (pid {ME})\n"),
        );
        report_from(&log, now, &lv).unwrap();

        // Truncated to shorter than the cursor: re-read from 0.
        std::fs::write(
            &log,
            format!("2026-09-30T09:00:00Z coord_worker_dead c (pid {ME})\n"),
        )
        .unwrap();
        let v = report_from(&log, now, &lv).unwrap();
        let kinds: Vec<_> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["kind"].clone())
            .collect();
        assert_eq!(
            kinds,
            ["coord_unreachable", "coord_no_leader", "coord_worker_dead"]
        );

        // Replaced by a LONGER file with a different head: the offset alone
        // would land mid-file; the head fingerprint catches it.
        let mut body = String::new();
        for m in 0..5 {
            body.push_str(&format!(
                "2026-09-30T10:0{m}:00Z coord_liveness_unknown r{m} (pid {ME})\n"
            ));
        }
        std::fs::write(&log, body).unwrap();
        let v = report_from(&log, now, &lv).unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 3 + 5);
        assert_eq!(rows[3]["began_at"], "2026-09-30T10:00:00Z");

        // Removed: publishes what is known, and a recreated file starts fresh.
        std::fs::remove_file(&log).unwrap();
        assert_eq!(
            report_from(&log, now, &lv)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            8
        );
        restart();
    }

    #[test]
    fn an_absent_log_is_an_empty_array_and_an_unreadable_one_is_an_error() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        restart();
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("wedge-incidents.log");
        assert_eq!(
            report_from(&log, Utc::now(), &live(false)).unwrap(),
            serde_json::json!([])
        );
        restart();
        // A directory where the log should be cannot be read as one.
        std::fs::create_dir(&log).unwrap();
        assert!(report_from(&log, Utc::now(), &live(false)).is_err());
        restart();
    }

    #[test]
    fn a_corrupt_state_file_rebuilds_from_the_whole_log() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        restart();
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("wedge-incidents.log");
        append(
            &log,
            &format!("2026-09-30T08:00:00Z coord_unreachable a (pid {ME})\n"),
        );
        std::fs::write(tmp.path().join(STATE_FILE_NAME), b"{garbage").unwrap();
        let v = report_from(&log, at("2026-09-30T09:00:00Z"), &live(false)).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        restart();
    }

    #[test]
    fn published_row_shape_is_exact() {
        let mut s = WedgeReportState::default();
        s.fold(
            parse_line(&monitor_line(
                "2026-09-30T10:00:00.5+00:00",
                "backend_wedged",
                ME,
            ))
            .unwrap(),
            &live(true),
        );
        assert_eq!(
            s.published(),
            serde_json::json!([{
                "kind": "backend_wedged",
                "began_at": "2026-09-30T10:00:00Z",
                "ended_at": null,
                "ended_by": null,
                "build_id": "abc123def456",
            }])
        );
        s.settle(at("2026-09-30T10:05:00Z"), &live(false));
        let row = &s.published()[0];
        assert_eq!(row["ended_at"], "2026-09-30T10:05:00Z");
        assert_eq!(row["ended_by"], "cleared");
    }
}
