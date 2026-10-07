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
//! reader never has to guess how an end was derived. An incident with
//! `ended_at: null` is one whose condition this process can vouch still holds
//! — except for the one row below that says otherwise, unknown reason tokens.
//!
//! | kind | ends when | `ended_by` | `ended_at` |
//! |---|---|---|---|
//! | every known kind below, written by ANOTHER runner process (pid differs) | a later line from a different pid, or else the first read by this process — one runner instance writes a given log, and every writer runs in the process it reports on, so nothing it latched outlives the process | `process_exited` | that later line, else that read |
//! | `backend_wedged`, `ui_thread_wedged`, `recovery_wedged` | this process's live predicate (`health_monitor::backend_wedged()` / `ui_thread_wedged()` / `webview_recovery::recovery_wedged()`) reads false | `cleared` | the first read that saw it false (an upper bound) |
//! | `coord_unreachable`, `coord_worker_dead`, `coord_no_leader`, `coord_liveness_unknown` | the outside observer's reporting latch for that class has re-armed (`LatchSnapshot::latch` reads `Closed`) — the latch is set before the line is written and re-arms when the predicate stops holding | `cleared` | the first read that saw it re-armed |
//! | same four, while the observer has not folded a probe within its staleness bound (`max(180 s, 3 × effective probe period)`; the latch reads `Unknown`) | at once — a frozen latch vouches for nothing, so the row is never held open on it | `observer_silent` | the observer's last fold (the last instant it vouched), else that read |
//! | `health_monitor_thread_stalled`, `health_metrics_thread_stalled` (watchdog, re-written every `WATCHDOG_REPEAT_SECS` while they hold) | no line for 2 × that cadence | `silent` | last line + one cadence (the latest instant the cadence allows it to have held) |
//! | any other (unknown) reason token — its writer, cadence and predicate are unknown to this build | a newer onset of the same token | `superseded` | the newer onset |
//! | same, with no newer onset | its onset is older than [`OPEN_WINDOW_SECS`] | `expired` | onset + that window — the runner stops vouching for it, which is NOT an observed end |
//!
//! A gap is not a clear: when the observer is fresh again with that class's
//! latch Open, and this process's newest row of the kind ended
//! `observer_silent`, a SUCCESSOR row opens (`began_at` = the start of the new
//! fresh streak, `ended_at: null`). Came back Closed: no successor.
//!
//! Any kind is also `superseded` when a new episode of it begins while an
//! earlier one is still open. Lines that continue an open episode (the
//! watchdog's repeats, or the monitor's one escalation line landing beside the
//! watchdog's for the same wedge) extend it rather than opening another. Two
//! stated undercounts: two episodes of a live-predicate kind that begin AND end
//! entirely between two reads are indistinguishable from one when both lines
//! came from the watchdog alone; and `coord_worker_dead` is per WORKER in the
//! log but per KIND here, so a second worker's death supersedes the first
//! worker's row while the kind (some worker is dead) stays open on the newest.
//!
//! ## The bound
//!
//! At most [`WEDGE_REPORT_BOUND`] rows are kept and published, NEWEST FIRST.
//! Closed rows are evicted before open ones, oldest first; an open row is
//! dropped only when more than the bound are open at once — an open wedge is
//! the one row coord must not lose to twenty closed coord flaps.
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
//! what was appended while the runner was down; a pass that changed nothing
//! does not rewrite the file. A missing or corrupt state file rebuilds the list
//! from the whole log, which is correct, just slower.
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

use crate::coord_outside_observer::{FaultClass, FaultLatch};

/// Most incidents published (and kept). See "The bound" above.
pub(crate) const WEDGE_REPORT_BOUND: usize = 20;

/// How long an incident of an UNKNOWN reason token with no end evidence stays
/// open. Known kinds never use it.
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
    ObserverSilent,
}

/// How a kind's end is established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndRule {
    /// A live in-process predicate answers "does it still hold?".
    LivePredicate,
    /// Re-written every watchdog cadence while it holds.
    Repeating,
    /// An unknown token: writer, cadence and predicate all unknown.
    Unknown,
}

/// The kinds with a live in-process predicate ([`LiveView::holds`]).
const LIVE_PREDICATE_KINDS: [&str; 7] = [
    "backend_wedged",
    "ui_thread_wedged",
    "recovery_wedged",
    "coord_unreachable",
    "coord_worker_dead",
    "coord_no_leader",
    "coord_liveness_unknown",
];

fn end_rule(kind: &str) -> EndRule {
    if LIVE_PREDICATE_KINDS.contains(&kind) {
        EndRule::LivePredicate
    } else if matches!(
        kind,
        "health_monitor_thread_stalled" | "health_metrics_thread_stalled"
    ) {
        EndRule::Repeating
    } else {
        EndRule::Unknown
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

/// Whether the coord outside observer's latches can be believed right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObserverView {
    /// It folded a probe recently; `holding` carries its open classes.
    /// `since` is when its current fresh streak began (the first fold after
    /// the last stale gap), `None` if unknown.
    Fresh { since: Option<DateTime<Utc>> },
    /// It has not folded within its staleness bound
    /// (`coord_outside_observer::open_faults_stale_after_secs`); `since` is its
    /// last fold, `None` if it never folded.
    Silent { since: Option<DateTime<Utc>> },
}

/// This process's view, sampled once per pass AFTER the log tail is read: its
/// pid, its build, which live-predicate kinds hold right now, and whether the
/// coord observer's latches are current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveView {
    pub pid: u32,
    pub build_id: Option<&'static str>,
    /// The [`LIVE_PREDICATE_KINDS`] whose predicate reads true.
    pub holding: Vec<&'static str>,
    pub observer: ObserverView,
}

impl LiveView {
    fn sample() -> Self {
        let mut holding = Vec::new();
        if crate::health_monitor::backend_wedged() {
            holding.push("backend_wedged");
        }
        if crate::health_monitor::ui_thread_wedged() {
            holding.push("ui_thread_wedged");
        }
        if crate::webview_recovery::recovery_wedged() {
            holding.push("recovery_wedged");
        }
        let latches = crate::coord_outside_observer::latch_snapshot();
        let now = Utc::now();
        let mut observer = ObserverView::Fresh {
            since: latches.fresh_since(),
        };
        for class in FaultClass::ALL {
            match latches.latch(class, now) {
                FaultLatch::Open => holding.push(class.breadcrumb_reason()),
                FaultLatch::Closed => {}
                FaultLatch::Unknown { last_fold } => {
                    observer = ObserverView::Silent { since: last_fold }
                }
            }
        }
        Self {
            pid: std::process::id(),
            build_id: crate::fleet::served_git_sha(),
            holding,
            observer,
        }
    }

    fn is_coord_kind(kind: &str) -> bool {
        FaultClass::from_breadcrumb_reason(kind).is_some()
    }

    fn holds(&self, kind: &str) -> bool {
        self.holding.contains(&kind)
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
                EndRule::Unknown => {
                    if (now - inc.began_at).num_seconds() > OPEN_WINDOW_SECS {
                        let at = inc.began_at + ChronoDuration::seconds(OPEN_WINDOW_SECS);
                        inc.close(at, EndedBy::Expired);
                    }
                }
                EndRule::LivePredicate => {}
            }
        }
    }

    /// Close every open KNOWN-kind incident written by a pid other than
    /// `alive`. Unknown tokens are exempt: their writer is unknown, so its
    /// process is too.
    fn close_other_processes(&mut self, alive: u32, at: DateTime<Utc>) {
        for inc in self.incidents.iter_mut().filter(|i| {
            i.ended_at.is_none()
                && end_rule(&i.kind) != EndRule::Unknown
                && i.pid.is_some_and(|p| p != alive)
        }) {
            inc.close(at, EndedBy::ProcessExited);
        }
    }

    /// Enforce [`WEDGE_REPORT_BOUND`]: closed rows go first, oldest first;
    /// open rows only when more than the bound are open.
    fn enforce_bound(&mut self) {
        while self.incidents.len() > WEDGE_REPORT_BOUND {
            let victim = self
                .incidents
                .iter()
                .position(|i| i.ended_at.is_some())
                .unwrap_or(0);
            self.incidents.remove(victim);
        }
    }

    /// Fold one line in, in file order.
    pub(crate) fn fold(&mut self, onset: Onset, live: &LiveView) {
        self.settle_by_time(onset.at);
        // One runner instance writes a given log (`instance::scope_path`), so a
        // line from pid P proves every OTHER pid's process has exited.
        if let Some(writer) = onset.pid {
            self.close_other_processes(writer, onset.at);
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
        self.enforce_bound();
    }

    /// Close what this process's own live state says is over.
    pub(crate) fn settle(&mut self, now: DateTime<Utc>, live: &LiveView) {
        self.settle_by_time(now);
        self.close_other_processes(live.pid, now);
        for inc in self
            .incidents
            .iter_mut()
            .filter(|i| i.ended_at.is_none() && end_rule(&i.kind) == EndRule::LivePredicate)
        {
            if LiveView::is_coord_kind(&inc.kind) {
                if let ObserverView::Silent { since } = live.observer {
                    inc.close(since.unwrap_or(now), EndedBy::ObserverSilent);
                    continue;
                }
            }
            if !live.holds(&inc.kind) {
                inc.close(now, EndedBy::Cleared);
            }
        }
        self.open_successors(now, live);
    }

    /// Re-open a coord episode that an observer GAP closed.
    ///
    /// `observer_silent` records that the observer stopped vouching, not that
    /// the fault cleared. When the observer is fresh again and its latch for a
    /// class is Open, and this process's newest row of that kind ended
    /// `observer_silent` with no open row beside it, the fault is ongoing: a
    /// successor row opens at the start of the fresh streak (never before the
    /// gap's recorded end). A latch that came back Closed opens nothing — the
    /// episode ended somewhere inside the gap, and the silent row already says
    /// the runner cannot say where.
    fn open_successors(&mut self, now: DateTime<Utc>, live: &LiveView) {
        let ObserverView::Fresh { since } = live.observer else {
            return;
        };
        for class in FaultClass::ALL {
            let kind = class.breadcrumb_reason();
            if !live.holds(kind) {
                continue;
            }
            let mine = |i: &&Incident| i.kind == kind && i.pid == Some(live.pid);
            if self
                .incidents
                .iter()
                .filter(mine)
                .any(|i| i.ended_at.is_none())
            {
                continue;
            }
            let Some(prior) = self.incidents.iter().rev().find(mine) else {
                continue;
            };
            if prior.ended_by != Some(EndedBy::ObserverSilent) {
                continue;
            }
            let gap_end = prior.ended_at.unwrap_or(prior.began_at);
            let began_at = since.unwrap_or(now).max(gap_end);
            self.incidents.push(Incident {
                kind: kind.to_string(),
                began_at,
                ended_at: None,
                ended_by: None,
                build_id: live.build_id.map(str::to_string),
                pid: Some(live.pid),
                last_seen_at: began_at,
                once_line_seen: true,
            });
        }
        self.enforce_bound();
    }

    /// The published array, NEWEST first.
    pub(crate) fn published(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.incidents
                .iter()
                .rev()
                .map(Incident::published)
                .collect(),
        )
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
    // Bound one pass: past MAX_READ_BYTES, skip to the newest tail. The skip
    // lands mid-line unless the byte BEFORE it is a newline, and only a
    // mid-line landing drops its partial first line.
    let seek_to = if len - start > MAX_READ_BYTES {
        len - MAX_READ_BYTES
    } else {
        start
    };
    let drop_partial = if seek_to > start {
        f.seek(SeekFrom::Start(seek_to - 1))?;
        let mut prev = [0u8; 1];
        f.read_exact(&mut prev)?;
        prev[0] != b'\n'
    } else {
        false
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

/// The state, and the exact bytes last written for it (`None` when nothing
/// valid is on disk), so an unchanged pass skips the rewrite.
fn load_state(path: &Path) -> (WedgeReportState, Option<Vec<u8>>) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (WedgeReportState::default(), None)
        }
        Err(e) => {
            warn!(
                "fleet::wedge_report: state {} unreadable ({e}); rebuilding from the whole log",
                path.display()
            );
            return (WedgeReportState::default(), None);
        }
    };
    match serde_json::from_slice::<WedgeReportState>(&bytes) {
        Ok(s) if s.version == STATE_VERSION => (s, Some(bytes)),
        Ok(s) => {
            warn!(
                "fleet::wedge_report: state version {} != {STATE_VERSION}; rebuilding from the \
                 whole log",
                s.version
            );
            (WedgeReportState::default(), None)
        }
        Err(e) => {
            warn!(
                "fleet::wedge_report: state {} is corrupt ({e}); rebuilding from the whole log",
                path.display()
            );
            (WedgeReportState::default(), None)
        }
    }
}

/// Atomic persist: sibling temp file carrying the pid, then rename.
fn persist_state(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), path.display())
    })
}

/// The in-process copy of one log's state.
struct Cached {
    state_path: PathBuf,
    state: WedgeReportState,
    /// The bytes last persisted (or loaded); `None` = nothing valid on disk.
    persisted: Option<Vec<u8>>,
}

/// In-process copy of the state, so a failed persist costs nothing until a
/// restart, and consecutive passes never race each other's read-modify-write.
static STATE: Mutex<Option<Cached>> = Mutex::new(None);

/// One pass over an explicit log path: read the tail, fold, settle, persist.
///
/// `Ok(array)` is the `details.wedge_incidents` value; `Err` means the log
/// exists but could not be read. The error names the file, never its
/// directory — it is published off-box.
///
/// `sample` yields `now` and the [`LiveView`], and is called only AFTER the log
/// tail has been read: every writer sets its live predicate before it appends
/// its line, so any line this pass folds had its predicate set before the view
/// that settles it was taken — a view sampled first could see a predicate
/// still unset and close as `cleared` an incident whose line it then read.
pub(crate) fn report_from(
    log_path: &Path,
    sample: impl FnOnce() -> (DateTime<Utc>, LiveView),
) -> Result<serde_json::Value, String> {
    let state_path = log_path.with_file_name(STATE_FILE_NAME);
    let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if guard.as_ref().is_some_and(|c| c.state_path != state_path) {
        *guard = None;
    }
    let cached = guard.get_or_insert_with(|| {
        let (state, persisted) = load_state(&state_path);
        Cached {
            state_path: state_path.clone(),
            state,
            persisted,
        }
    });
    let state = &mut cached.state;
    let file_name = log_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "the wedge incident log".to_string());
    let tail = read_tail(log_path, state.cursor.as_ref())
        .map_err(|e| format!("reading {file_name} failed: {e}"))?;
    let (now, live) = sample();
    let live = &live;
    state.register_build(live);
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
    let published = state.published();
    match serde_json::to_vec(&*state) {
        Ok(bytes) if cached.persisted.as_deref() != Some(bytes.as_slice()) => {
            match persist_state(&state_path, &bytes) {
                Ok(()) => cached.persisted = Some(bytes),
                Err(e) => warn!(
                    "fleet::wedge_report: state not persisted ({e}); a restart re-reads the log"
                ),
            }
        }
        Ok(_) => {}
        Err(e) => warn!("fleet::wedge_report: serialising state failed ({e}); not persisted"),
    }
    Ok(published)
}

/// The `details` pair this module owns, from one pass's outcome. BOTH keys
/// are always written, the unused one as JSON `null`: coord merges `details`
/// per top-level key and a `null` removes it, so this is what retires a
/// previous pass's error (or a previous pass's array beside a new error)
/// instead of leaving it on the row looking current.
pub(crate) fn details_pair(
    outcome: Result<serde_json::Value, String>,
) -> [(&'static str, serde_json::Value); 2] {
    match outcome {
        Ok(v) => [
            ("wedge_incidents", v),
            ("wedge_incidents_error", serde_json::Value::Null),
        ],
        Err(e) => [
            ("wedge_incidents", serde_json::Value::Null),
            ("wedge_incidents_error", serde_json::Value::String(e)),
        ],
    }
}

/// Write this module's keys into the heartbeat `details` object:
/// `wedge_incidents` (an array, newest first; `[]` when no log exists) and
/// `wedge_incidents_error` (`null`, or why the log could not be read — then
/// `wedge_incidents` is `null`). Blocking IO.
pub(crate) fn publish_into(details: &mut serde_json::Map<String, serde_json::Value>) {
    let log = crate::health_monitor::wedge_incidents_path(&crate::paths::get_dev_logs_dir());
    for (k, v) in details_pair(report_from(&log, || (Utc::now(), LiveView::sample()))) {
        details.insert(k.to_string(), v);
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
        let kinds: &[&'static str] = if backend { &["backend_wedged"] } else { &[] };
        live_holding(kinds)
    }

    fn live_holding(kinds: &[&'static str]) -> LiveView {
        LiveView {
            pid: ME,
            build_id: Some("abc123def456"),
            holding: kinds.to_vec(),
            observer: ObserverView::Fresh { since: None },
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
    fn coord_kinds_stay_open_exactly_while_the_observer_latch_holds() {
        let mut s = WedgeReportState::default();
        let open = live_holding(&["coord_liveness_unknown"]);
        s.fold(
            parse_line(&format!(
                "2026-09-29T10:00:00Z coord_liveness_unknown x (pid {ME})"
            ))
            .unwrap(),
            &open,
        );
        // Far past any window: still open, because the latch still holds.
        s.settle(at("2026-10-05T10:00:00Z"), &open);
        assert_eq!(s.incidents[0].ended_at, None);
        s.settle(at("2026-10-05T10:05:00Z"), &live(false));
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::Cleared));
        assert_eq!(s.incidents[0].ended_at, Some(at("2026-10-05T10:05:00Z")));

        // Written by an earlier process: over the moment this one reads it,
        // whatever this process's own latch says.
        let mut s = WedgeReportState::default();
        s.fold(
            parse_line("2026-09-29T10:00:00Z coord_no_leader x (pid 77)").unwrap(),
            &live_holding(&["coord_no_leader"]),
        );
        s.settle(
            at("2026-09-29T11:00:00Z"),
            &live_holding(&["coord_no_leader"]),
        );
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::ProcessExited));
    }

    #[test]
    fn a_frozen_observer_closes_coord_rows_at_its_last_fold_and_a_fresh_one_holds() {
        let line = format!("2026-09-30T10:00:00Z coord_unreachable x (pid {ME})");
        let mut s = WedgeReportState::default();
        s.fold(parse_line(&line).unwrap(), &live_holding(&[]));

        // Fresh observer, latch open: held.
        s.settle(
            at("2026-09-30T10:30:00Z"),
            &live_holding(&["coord_unreachable"]),
        );
        assert_eq!(s.incidents[0].ended_at, None);

        // Frozen observer: its (frozen) latch is ignored and the row closes at
        // the last instant the observer vouched.
        let mut frozen = live_holding(&[]);
        frozen.observer = ObserverView::Silent {
            since: Some(at("2026-09-30T10:31:00Z")),
        };
        s.settle(at("2026-09-30T11:00:00Z"), &frozen);
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::ObserverSilent));
        assert_eq!(s.incidents[0].ended_at, Some(at("2026-09-30T10:31:00Z")));
        assert_eq!(s.published()[0]["ended_by"], "observer_silent");

        // An observer that never folded closes at the read; non-coord kinds
        // are untouched by the observer's silence.
        let mut s = WedgeReportState::default();
        s.fold(parse_line(&line).unwrap(), &live_holding(&[]));
        s.fold(
            parse_line(&monitor_line("2026-09-30T10:01:00Z", "backend_wedged", ME)).unwrap(),
            &live_holding(&[]),
        );
        let mut never = live_holding(&["backend_wedged"]);
        never.observer = ObserverView::Silent { since: None };
        s.settle(at("2026-09-30T10:02:00Z"), &never);
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::ObserverSilent));
        assert_eq!(s.incidents[0].ended_at, Some(at("2026-09-30T10:02:00Z")));
        assert_eq!(s.incidents[1].ended_at, None, "backend_wedged still holds");
    }

    #[test]
    fn a_gap_closes_observer_silent_and_a_fresh_open_latch_opens_a_successor() {
        let line = format!("2026-09-30T10:00:00Z coord_no_leader x (pid {ME})");
        let mut gapped = live_holding(&[]);
        gapped.observer = ObserverView::Silent {
            since: Some(at("2026-09-30T10:10:00Z")),
        };
        let back = |since: &str, kinds: &[&'static str]| {
            let mut lv = live_holding(kinds);
            lv.observer = ObserverView::Fresh {
                since: Some(at(since)),
            };
            lv
        };

        // Gap, then fresh with the latch still Open: a successor opens.
        let mut s = WedgeReportState::default();
        s.fold(
            parse_line(&line).unwrap(),
            &live_holding(&["coord_no_leader"]),
        );
        s.settle(at("2026-09-30T10:20:00Z"), &gapped);
        assert_eq!(s.incidents.len(), 1);
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::ObserverSilent));
        let fresh_open = back("2026-09-30T10:25:00Z", &["coord_no_leader"]);
        s.settle(at("2026-09-30T10:26:00Z"), &fresh_open);
        assert_eq!(s.incidents.len(), 2);
        assert_eq!(s.incidents[1].kind, "coord_no_leader");
        assert_eq!(s.incidents[1].began_at, at("2026-09-30T10:25:00Z"));
        assert_eq!(s.incidents[1].ended_at, None);
        assert_eq!(s.incidents[1].build_id.as_deref(), Some("abc123def456"));
        // Idempotent: the open successor is not duplicated on the next pass.
        s.settle(at("2026-09-30T10:27:00Z"), &fresh_open);
        assert_eq!(s.incidents.len(), 2);
        // And it clears like any coord row once the latch re-arms.
        s.settle(
            at("2026-09-30T10:30:00Z"),
            &back("2026-09-30T10:25:00Z", &[]),
        );
        assert_eq!(s.incidents[1].ended_by, Some(EndedBy::Cleared));

        // Gap, then fresh with the latch Closed: no successor.
        let mut s = WedgeReportState::default();
        s.fold(
            parse_line(&line).unwrap(),
            &live_holding(&["coord_no_leader"]),
        );
        s.settle(at("2026-09-30T10:20:00Z"), &gapped);
        s.settle(
            at("2026-09-30T10:26:00Z"),
            &back("2026-09-30T10:25:00Z", &[]),
        );
        assert_eq!(s.incidents.len(), 1);
        assert_eq!(s.incidents[0].ended_by, Some(EndedBy::ObserverSilent));

        // A row cleared normally is never resurrected by a later Open latch
        // (that is a NEW episode, and it arrives with its own line).
        let mut s = WedgeReportState::default();
        s.fold(
            parse_line(&line).unwrap(),
            &live_holding(&["coord_no_leader"]),
        );
        s.settle(
            at("2026-09-30T10:05:00Z"),
            &back("2026-09-30T10:00:00Z", &[]),
        );
        s.settle(
            at("2026-09-30T10:06:00Z"),
            &back("2026-09-30T10:00:00Z", &["coord_no_leader"]),
        );
        assert_eq!(s.incidents.len(), 1);
    }

    #[test]
    fn the_live_view_is_sampled_after_the_tail_is_read() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        restart();
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("wedge-incidents.log");
        let now = at("2026-09-30T12:00:00Z");
        append(
            &log,
            &format!("2026-09-30T10:00:00Z coord_no_leader a (pid {ME})\n"),
        );
        // The sampler plays a writer racing the pass: it sets its predicate
        // and appends its line. Had the view been sampled BEFORE the read, this
        // pass would fold that line against a view that predates it.
        let v = report_from(&log, || {
            append(
                &log,
                &format!("2026-09-30T11:00:00Z coord_unreachable b (pid {ME})\n"),
            );
            (now, live_holding(&["coord_no_leader", "coord_unreachable"]))
        })
        .unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the line appended during sampling is NOT in this pass"
        );
        assert_eq!(rows[0]["ended_at"], serde_json::Value::Null);
        let v = report_from(&log, || {
            (now, live_holding(&["coord_no_leader", "coord_unreachable"]))
        })
        .unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2, "and is read by the next");
        restart();
    }

    #[test]
    fn unknown_tokens_are_superseded_by_a_newer_onset_and_expire_after_the_window() {
        let mut s = WedgeReportState::default();
        let lv = live(false);
        for ts in ["2026-09-29T10:00:00Z", "2026-09-29T12:00:00Z"] {
            s.fold(
                parse_line(&format!("{ts} some_future_reason x (pid {ME})")).unwrap(),
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

        // An unknown token's writer is unknown, so a line from another pid
        // proves nothing about it.
        let mut s = WedgeReportState::default();
        s.fold(
            parse_line("2026-09-29T10:00:00Z some_future_reason x (pid 5)").unwrap(),
            &lv,
        );
        s.settle(at("2026-09-29T11:00:00Z"), &lv);
        assert_eq!(s.incidents[0].ended_at, None);
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
    fn the_list_keeps_twenty_evicting_closed_before_open_and_publishes_newest_first() {
        let mut s = WedgeReportState::default();
        let lv = live_holding(&["backend_wedged"]);
        let t0 = at("2026-09-30T00:00:00Z");
        // One OPEN wedge first, then 24 coord flaps each superseding the last.
        s.fold(
            parse_line(&monitor_line(&t0.to_rfc3339(), "backend_wedged", ME)).unwrap(),
            &lv,
        );
        for i in 1..25 {
            let ts = (t0 + ChronoDuration::minutes(i)).to_rfc3339();
            s.fold(
                parse_line(&format!("{ts} coord_no_leader x (pid {ME})")).unwrap(),
                &lv,
            );
        }
        assert_eq!(s.incidents.len(), WEDGE_REPORT_BOUND);
        assert_eq!(
            s.incidents[0].kind, "backend_wedged",
            "the oldest row survives because it is OPEN"
        );
        assert_eq!(s.incidents[1].began_at, t0 + ChronoDuration::minutes(6));
        let rows = s.published();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), WEDGE_REPORT_BOUND);
        assert_eq!(rows[0]["began_at"], "2026-09-30T00:24:00Z", "newest first");
        assert_eq!(rows[WEDGE_REPORT_BOUND - 1]["kind"], "backend_wedged");

        // More than twenty OPEN rows: only then does an open row go, oldest first.
        let mut s = WedgeReportState::default();
        for i in 0..22 {
            let ts = (t0 + ChronoDuration::minutes(i)).to_rfc3339();
            s.fold(
                parse_line(&format!("{ts} reason_{i} x (pid {ME})")).unwrap(),
                &lv,
            );
        }
        assert_eq!(s.incidents.len(), WEDGE_REPORT_BOUND);
        assert_eq!(s.incidents[0].kind, "reason_2");
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
        let v = report_from(&log, || (now, lv.clone())).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);

        // Restart; append while "down"; the new line is read, the old one not
        // re-folded (it would otherwise supersede itself into two rows).
        restart();
        append(
            &log,
            &format!("2026-09-30T11:00:00Z coord_worker_dead b (pid {ME})\n"),
        );
        let v = report_from(&log, || (now, lv.clone())).unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2, "{v}");
        assert_eq!(rows[0]["kind"], "coord_worker_dead", "newest first");
        assert_eq!(rows[1]["kind"], "coord_unreachable");

        // A pass with nothing new changes nothing — not even the state file.
        restart();
        let state_file = tmp.path().join(STATE_FILE_NAME);
        let before = std::fs::metadata(&state_file).unwrap().modified().unwrap();
        std::fs::write(&state_file, std::fs::read(&state_file).unwrap()).unwrap();
        let rewritten = std::fs::metadata(&state_file).unwrap().modified().unwrap();
        assert!(rewritten >= before);
        assert_eq!(report_from(&log, || (now, lv.clone())).unwrap(), v);
        assert_eq!(
            std::fs::metadata(&state_file).unwrap().modified().unwrap(),
            rewritten,
            "an unchanged pass does not rewrite the state"
        );

        // A partial line is left for the next pass, then read whole.
        append(&log, "2026-09-30T11:30:00Z coord_no_le");
        assert_eq!(
            report_from(&log, || (now, lv.clone()))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        append(&log, &format!("ader c (pid {ME})\n"));
        let v = report_from(&log, || (now, lv.clone())).unwrap();
        assert_eq!(v.as_array().unwrap()[0]["kind"], "coord_no_leader");
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
        report_from(&log, || (now, lv.clone())).unwrap();

        // Truncated to shorter than the cursor: re-read from 0.
        std::fs::write(
            &log,
            format!("2026-09-30T09:00:00Z coord_worker_dead c (pid {ME})\n"),
        )
        .unwrap();
        let v = report_from(&log, || (now, lv.clone())).unwrap();
        let kinds: Vec<_> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["kind"].clone())
            .collect();
        assert_eq!(
            kinds,
            ["coord_worker_dead", "coord_no_leader", "coord_unreachable"]
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
        let v = report_from(&log, || (now, lv.clone())).unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 3 + 5);
        assert_eq!(rows[0]["began_at"], "2026-09-30T10:04:00Z");
        assert_eq!(rows[4]["began_at"], "2026-09-30T10:00:00Z");

        // Removed: publishes what is known, and a recreated file starts fresh.
        std::fs::remove_file(&log).unwrap();
        assert_eq!(
            report_from(&log, || (now, lv.clone()))
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
            report_from(&log, || (Utc::now(), live(false))).unwrap(),
            serde_json::json!([])
        );
        restart();
        // A directory where the log should be cannot be read as one.
        std::fs::create_dir(&log).unwrap();
        let err = report_from(&log, || (Utc::now(), live(false))).unwrap_err();
        assert!(
            err.starts_with("reading wedge-incidents.log failed"),
            "{err}"
        );
        assert!(
            !err.contains(&tmp.path().display().to_string()),
            "never an absolute path off-box: {err}"
        );
        restart();
    }

    #[test]
    fn both_keys_of_the_pair_are_always_sent_the_unused_one_null() {
        assert_eq!(
            details_pair(Ok(serde_json::json!([]))),
            [
                ("wedge_incidents", serde_json::json!([])),
                ("wedge_incidents_error", serde_json::Value::Null),
            ]
        );
        assert_eq!(
            details_pair(Err("boom".into())),
            [
                ("wedge_incidents", serde_json::Value::Null),
                ("wedge_incidents_error", serde_json::json!("boom")),
            ]
        );
    }

    #[test]
    fn a_capped_read_that_lands_on_a_line_start_keeps_that_line() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("wedge-incidents.log");
        let line = format!("2026-09-30T08:00:00Z coord_unreachable a (pid {ME})\n");
        // Build the file so the capped window starts EXACTLY on a line start:
        // 300 bytes of padding lines, one `y…` line, then the tail lines.
        let tail_lines = (MAX_READ_BYTES as usize) / line.len() - 1;
        let lead = (MAX_READ_BYTES as usize) - tail_lines * line.len();
        let mut body = ("x".repeat(99) + "\n").repeat(3);
        body.push_str(&"y".repeat(lead - 1));
        body.push('\n');
        body.push_str(&line.repeat(tail_lines));
        std::fs::write(&log, &body).unwrap();
        let TailRead::Lines { lines, cursor, .. } = read_tail(&log, None).unwrap() else {
            panic!("expected lines");
        };
        assert_eq!(cursor.offset, body.len() as u64);
        // The byte before the window is a newline, so the `y…` line is whole
        // and is kept rather than dropped as a partial.
        assert_eq!(lines.len(), tail_lines + 1);
        assert!(lines[0].starts_with('y'));
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
        let v = report_from(&log, || (at("2026-09-30T09:00:00Z"), live(false))).unwrap();
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
