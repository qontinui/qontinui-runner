//! Machine-wide live Claude session census — the runner half of plan
//! `2026-10-01-a-commit-author-session-is-unreachable-because-every-session-roster-is-per-account`
//! (Phase 2, plus the device-local read Phase 4's `no_pusher` reason needs).
//!
//! ## What it is
//!
//! Every [`CENSUS_INTERVAL`] the PRIMARY runner enumerates every live Claude
//! Code process on this machine — across EVERY account home
//! ([`crate::terminal::transcript::find_claude_config_dirs`], which since
//! Phase 1 scans `~/.claude-*` on POSIX as well as `C:\claude\.claude-*` on
//! Windows) — from Claude Code's own registry
//! ([`super::claude_session_registry::read_live_sessions`]) and posts the whole
//! set to `POST {coord}/coord/session-census/{device_id}`, which replaces this
//! device's snapshot.
//!
//! A per-device ENUMERATION, not a per-session event: it reports sessions
//! whose hooks never fire and sessions the runner does not host (a tmux pane
//! launched from an account shortcut), which is the population no other coord
//! door sees. Absence is measured by the enumerator, never inferred from
//! silence.
//!
//! ## What it never carries
//!
//! No transcript content (only the `timestamp` of its last turn line — `type`
//! and `timestamp` are the only keys ever deserialized), no environment values
//! (`/proc/<pid>/environ` is read whole into memory, scanned for the one
//! `TMUX_PANE` entry, and the buffer is dropped when the function returns —
//! nothing else in it is copied, stored or logged), no credentials.
//!
//! ## Field sources (contract §2)
//!
//! | field | source |
//! |---|---|
//! | `account` | config-dir basename minus `.claude-`; `default` for `.claude` |
//! | `last_acted_at` | `timestamp` of the last `user`/`assistant` line in the tail of `<config_dir>/projects/*/<session_id>.jsonl`, else registry `statusUpdatedAt`, else null — NEVER the file mtime (it moves with no new turn: coord finding `124c0ce9`) |
//! | `tmux_pane` | Linux `/proc/<pid>/environ` `TMUX_PANE`, else the registry `tmux` locator's `%N` suffix |
//! | `runner_hosted` | THIS runner hosts it (the session-message poller's own predicate, [`crate::mcp::session_message_poller::runner_hosts`]) OR its NEAREST supervising process — the first `qontinui-runner` or `claude` ancestor — is a runner ([`runner_ancestor`]); a `claude` nested inside another session's tool call is not runner-hosted |
//!
//! `pid_alive` is always `true` here: [`read_live_sessions`] drops a registry
//! row whose pid is not in the live process table (a crashed process cannot
//! delete its own file), so a row in the snapshot is a live process. A dead
//! session is reported by its ABSENCE from a fresh snapshot.
//!
//! ## Why ancestry, not only this runner's predicate
//!
//! A secondary/temp runner on the same device hosts sessions THIS runner's
//! stores know nothing about. Judged by this runner's predicate alone they
//! would read as "live but unhosted" and the poller would claim `no_pusher`
//! for a session another runner can push to. The ppid walk closes that blind
//! spot on Linux. The walk stops at the first `claude` ancestor too: a session
//! started from a tool call inside a runner-hosted session has a runner above
//! it but no pusher of its own. On other OSes ancestry is not read ([`ANCESTRY_SUPPORTED`]):
//! `runner_hosted` is this runner's predicate alone and a live, unhosted
//! session still reports `no_pusher` — the pre-ancestry behaviour.
//!
//! ## Failure posture
//!
//! Best-effort throughout. An indeterminate process snapshot skips the tick
//! (posting an empty set would read as "every session on this device died");
//! a failed POST is logged once per state change, never per tick; the
//! filesystem walk runs on the blocking pool, so nothing here can panic the
//! loop or stall an async worker.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use tracing::{debug, info, warn};

use super::claude_session_registry::{read_live_sessions, LiveClaudeSession};

/// How often the census is taken and posted. coord reads a device snapshot
/// older than 3× this as `unknown` (contract §3), so changing it is a
/// cross-repo change.
pub const CENSUS_INTERVAL: Duration = Duration::from_secs(60);

/// A census older than this is UNKNOWN to local readers too (the same 3×
/// rule coord applies).
pub const CENSUS_STALE_AFTER: Duration = Duration::from_secs(180);

/// One live Claude Code process. Field names are the coord ingest contract's
/// (`POST /coord/session-census/:device_id`, contract §2) — do not rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CensusRow {
    pub session_id: String,
    pub pid: u32,
    pub pid_alive: bool,
    pub proc_start: Option<String>,
    pub started_at: Option<String>,
    pub account: Option<String>,
    pub config_dir: Option<String>,
    pub cwd: Option<String>,
    pub entrypoint: Option<String>,
    pub kind: Option<String>,
    pub tmux_pane: Option<String>,
    pub window_name: Option<String>,
    pub window_name_source: Option<String>,
    pub registry_status: Option<String>,
    pub registry_status_updated_at: Option<String>,
    pub last_acted_at: Option<String>,
    pub runner_hosted: bool,
    /// Local only, never posted: did the ppid walk find a `qontinui-runner`
    /// ancestor? `None` = not read (non-Linux) or unreadable mid-walk.
    #[serde(skip)]
    pub runner_ancestor: Option<bool>,
}

/// The whole-device snapshot body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CensusSnapshot {
    pub observed_at: String,
    pub runner_build: Option<String>,
    pub account_homes: usize,
    pub sessions: Vec<CensusRow>,
}

/// The contract's account label for a config dir: the basename minus a
/// leading `.claude-` (`~/.claude-tiohorst` → `tiohorst`), `default` for a
/// plain `.claude`, and the bare basename for anything else (a
/// `CLAUDE_CONFIG_DIR` pinned somewhere unconventional).
///
/// Deliberately NOT [`crate::session::past_sessions::account_from_config_dir`]:
/// that labels the default home `unknown` because the session-repository
/// identity key depends on it; the census contract names it `default`.
pub fn account_label(config_dir: &Path) -> Option<String> {
    let base = config_dir.file_name()?.to_str()?;
    if base == ".claude" {
        return Some("default".to_string());
    }
    Some(
        base.strip_prefix(".claude-")
            .filter(|s| !s.is_empty())
            .unwrap_or(base)
            .to_string(),
    )
}

/// Epoch milliseconds → RFC 3339, `None` for a non-positive or out-of-range
/// value (a registry row that omitted the field parses as `0`).
fn ms_to_rfc3339(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|t| t.to_rfc3339())
}

/// A hyphenated, 36-character UUID — the only shape of session id this module
/// will splice into a file name. A registry row is written by another program,
/// so an id like `../x` must never reach a path join.
pub fn is_uuid_shaped(s: &str) -> bool {
    s.len() == 36 && uuid::Uuid::try_parse(s).is_ok()
}

// ===========================================================================
// last_acted_at — the last turn line's own timestamp
// ===========================================================================

/// The tail window [`last_acted_at`] reads first, and the wider one it
/// retries with when the first held no complete turn line (one tool result can
/// exceed 64 KB on its own).
const TAIL_FIRST: u64 = 64 * 1024;
const TAIL_RETRY: u64 = 1024 * 1024;

/// The only two keys read out of a transcript line. Every other key —
/// `message`, tool payloads, everything with content in it — is skipped by the
/// deserializer and never retained.
#[derive(serde::Deserialize)]
struct TurnStamp {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
}

/// The last `max` bytes of `path`, and whether the window starts mid-file
/// (so its first line is probably a fragment).
fn read_tail(path: &Path, max: u64) -> Option<(Vec<u8>, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let truncated = len > max;
    if truncated {
        file.seek(SeekFrom::Start(len - max)).ok()?;
    }
    let mut bytes = Vec::with_capacity(len.min(max) as usize);
    file.take(max).read_to_end(&mut bytes).ok()?;
    Some((bytes, truncated))
}

/// The `timestamp` of the LAST `user`/`assistant` line in a tail window, or
/// `None`. Lines are decoded one at a time and LOSSILY, so a multibyte
/// character a writer has only half-flushed spoils one line, never the window;
/// a fragment that fails to parse is simply skipped. When the window starts
/// mid-file its first line is dropped as a fragment.
fn last_turn_in(bytes: &[u8], truncated: bool) -> Option<DateTime<Utc>> {
    let lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    let skip = usize::from(truncated);
    lines.iter().skip(skip).rev().find_map(|raw| {
        let line = String::from_utf8_lossy(raw);
        let stamp: TurnStamp = serde_json::from_str(line.trim()).ok()?;
        if !matches!(stamp.kind.as_deref(), Some("user" | "assistant")) {
            return None;
        }
        DateTime::parse_from_rfc3339(stamp.timestamp.as_deref()?)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    })
}

/// Results of the 1 MB retry, keyed by `(path, size)`. A transcript whose final
/// line alone exceeds the first window would otherwise be re-read at 1 MB on
/// every tick while it sits idle; an unchanged size means an unchanged tail
/// for an append-only file. Cleared wholesale when it grows past
/// [`RETRY_CACHE_CAP`] so it stays bounded.
type RetryCache = HashMap<PathBuf, (u64, Option<DateTime<Utc>>)>;
const RETRY_CACHE_CAP: usize = 1024;

fn retry_cache() -> &'static Mutex<RetryCache> {
    static CELL: std::sync::OnceLock<Mutex<RetryCache>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The tail reader [`last_turn_in_file_with`] is given — `read_tail` in
/// production, injectable so a transient failure can be simulated.
type TailReader<'a> = &'a dyn Fn(&Path, u64) -> Option<(Vec<u8>, bool)>;

/// The last turn timestamp in ONE transcript file.
fn last_turn_in_file(path: &Path) -> Option<DateTime<Utc>> {
    last_turn_in_file_with(path, &read_tail)
}

fn last_turn_in_file_with(path: &Path, read: TailReader<'_>) -> Option<DateTime<Utc>> {
    let (bytes, truncated) = read(path, TAIL_FIRST)?;
    if let Some(t) = last_turn_in(&bytes, truncated) {
        return Some(t);
    }
    if !truncated {
        return None; // the whole file was read
    }
    let len = std::fs::metadata(path).ok()?.len();
    if let Ok(cache) = retry_cache().lock() {
        if let Some((size, hit)) = cache.get(path) {
            if *size == len {
                return *hit;
            }
        }
    }
    // Only a SUCCESSFUL read is cached: a transient failure (the file rotated,
    // a permission blip) must be retried next tick, not remembered as "no
    // turn line" for as long as the size holds.
    let (bytes, truncated) = read(path, TAIL_RETRY)?;
    let found = last_turn_in(&bytes, truncated);
    if let Ok(mut cache) = retry_cache().lock() {
        if cache.len() >= RETRY_CACHE_CAP {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), (len, found));
    }
    found
}

/// When the session last ACTED: the `timestamp` of the last `user` or
/// `assistant` line across every `<config_dir>/projects/*/<session_id>.jsonl`.
///
/// NOT the transcript's mtime. Phase 0 measured mtimes moving with no new turn
/// (coord finding `124c0ce9`), so an mtime would report an idle session as
/// active. Only the tail is read, and only `type` + `timestamp` are parsed.
///
/// A session id names one transcript, but the project directory it lives in
/// is the encoded cwd at launch, which a later `cd` does not move — so every
/// project dir is probed rather than guessing one from the registry `cwd`.
/// `None` when no transcript carries a turn line (yet), or when the id is not
/// UUID-shaped ([`is_uuid_shaped`]).
pub fn last_acted_at(config_dir: &Path, session_id: &str) -> Option<DateTime<Utc>> {
    if !is_uuid_shaped(session_id) {
        return None;
    }
    let file = format!("{session_id}.jsonl");
    let entries = std::fs::read_dir(config_dir.join("projects")).ok()?;
    entries
        .flatten()
        .map(|e| e.path().join(&file))
        .filter(|p| p.is_file())
        .filter_map(|p| last_turn_in_file(&p))
        .max()
}

// ===========================================================================
// tmux pane
// ===========================================================================

/// Extract `TMUX_PANE` from a raw `/proc/<pid>/environ` buffer (NUL-separated
/// `KEY=value` entries). Only the `TMUX_PANE=` entry is decoded; nothing else
/// is copied out of the buffer. A pane id that is not `%<digits>` is rejected
/// rather than passed on, so nothing but a pane id can ever leave this
/// function.
pub fn tmux_pane_from_environ(environ: &[u8]) -> Option<String> {
    const KEY: &[u8] = b"TMUX_PANE=";
    let value = environ
        .split(|b| *b == 0)
        .find_map(|entry| entry.strip_prefix(KEY))?;
    valid_pane(std::str::from_utf8(value).ok()?)
}

/// The `%N` pane out of a registry `tmux` locator
/// (`<tmux session>:@<window>.%<pane>`).
pub fn tmux_pane_from_locator(locator: &str) -> Option<String> {
    valid_pane(locator.rsplit('.').next()?)
}

fn valid_pane(s: &str) -> Option<String> {
    let digits = s.strip_prefix('%')?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| s.to_string())
}

/// This process's tmux pane, read from the live process environment.
/// Linux only — no other OS exposes another process's environment cheaply.
/// The environ buffer is read whole and dropped when this returns.
#[cfg(target_os = "linux")]
pub fn tmux_pane_of_pid(pid: u32) -> Option<String> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    tmux_pane_from_environ(&environ)
}

#[cfg(not(target_os = "linux"))]
pub fn tmux_pane_of_pid(_pid: u32) -> Option<String> {
    None
}

// ===========================================================================
// Process ancestry — is ANY runner on this device an ancestor?
// ===========================================================================

/// Whether [`read_proc_stat`] can read ancestry on this OS. Off Linux the
/// census and the poller keep the pre-ancestry behaviour (module doc).
pub const ANCESTRY_SUPPORTED: bool = cfg!(target_os = "linux");

/// Bound on the ppid walk, so a cycle or a pathological tree cannot spin.
const ANCESTRY_MAX_DEPTH: usize = 64;

/// The runner's process name as `/proc/<pid>/comm` reports it (the kernel
/// truncates `comm` to 15 bytes, which is exactly this string).
const RUNNER_COMM: &str = "qontinui-runner";

/// The Claude Code CLI's process name in `/proc/<pid>/comm`.
const CLAUDE_COMM: &str = "claude";

/// `(comm, ppid)` out of one `/proc/<pid>/stat` line. `comm` may itself
/// contain spaces and parentheses, so it is the text between the FIRST `(`
/// and the LAST `)`; `ppid` is the second field after that.
pub fn parse_proc_stat(stat: &str) -> Option<(String, u32)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?.to_string();
    let mut fields = stat.get(close + 1..)?.split_whitespace();
    let _state = fields.next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((comm, ppid))
}

/// `/proc/<pid>/stat` → `(comm, ppid)`. Linux only.
#[cfg(target_os = "linux")]
pub fn read_proc_stat(pid: u32) -> Option<(String, u32)> {
    parse_proc_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

#[cfg(not(target_os = "linux"))]
pub fn read_proc_stat(_pid: u32) -> Option<(String, u32)> {
    None
}

/// Is `pid`'s NEAREST supervising process a `qontinui-runner` — this runner
/// or any other on the device? Walks ppids up to [`ANCESTRY_MAX_DEPTH`]
/// through the injected reader and stops at the FIRST ancestor that is either
/// a runner or another `claude` process.
///
/// The `claude` stop is what keeps a nested session honest: a `claude`
/// started from a tool call inside a runner-hosted session has a runner far up
/// its tree, but nothing pushes to IT — the runner's PTY belongs to the outer
/// session. So `runner_hosted` means "its nearest supervisor is a runner", not
/// "a runner is somewhere above it".
///
/// `Some(true)` the nearest supervisor is a runner; `Some(false)` it is
/// another `claude`, or the walk reached init (pid ≤ 1) with neither; `None`
/// UNKNOWN — a link was unreadable (the process exited mid-walk, or the OS has
/// no `/proc`), the tree cycled, or the depth bound was hit. A CLI that runs
/// under a different process name (e.g. `node`) is not recognised as a
/// `claude` stop, so such a nested session still reads as hosted.
pub fn runner_ancestor(pid: u32, read_stat: &dyn Fn(u32) -> Option<(String, u32)>) -> Option<bool> {
    let (_, mut cur) = read_stat(pid)?;
    for _ in 0..ANCESTRY_MAX_DEPTH {
        if cur <= 1 {
            return Some(false);
        }
        let (comm, ppid) = read_stat(cur)?;
        if comm.starts_with(RUNNER_COMM) {
            return Some(true);
        }
        if comm == CLAUDE_COMM {
            return Some(false);
        }
        if ppid == cur {
            return None;
        }
        cur = ppid;
    }
    None
}

// ===========================================================================
// Snapshot
// ===========================================================================

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// The per-row probes [`build_snapshot`] takes, injected so it is a pure
/// function of paths in tests.
pub struct Probes<'a> {
    pub tmux_pane_of: &'a dyn Fn(u32) -> Option<String>,
    pub read_stat: &'a dyn Fn(u32) -> Option<(String, u32)>,
    pub is_runner_hosted: &'a dyn Fn(&str) -> bool,
}

/// Project one registry row into a census row.
fn row_from(config_dir: &Path, s: &LiveClaudeSession, probes: &Probes<'_>) -> CensusRow {
    let runner_ancestor = runner_ancestor(s.pid, probes.read_stat);
    CensusRow {
        session_id: s.session_id.clone(),
        pid: s.pid,
        pid_alive: true,
        proc_start: s.proc_start.clone(),
        started_at: ms_to_rfc3339(s.started_at),
        account: account_label(config_dir),
        config_dir: Some(config_dir.to_string_lossy().into_owned()),
        cwd: non_empty(&s.working_dir),
        entrypoint: s.entrypoint.clone(),
        kind: non_empty(&s.kind),
        tmux_pane: (probes.tmux_pane_of)(s.pid)
            .or_else(|| s.tmux.as_deref().and_then(tmux_pane_from_locator)),
        window_name: non_empty(&s.name),
        window_name_source: s.name_source.clone(),
        registry_status: non_empty(&s.status),
        registry_status_updated_at: s.status_updated_at.and_then(ms_to_rfc3339),
        // The last turn line's own timestamp; else the registry's
        // `statusUpdatedAt` (weaker — it moves on status changes only); else
        // null.
        last_acted_at: last_acted_at(config_dir, &s.session_id)
            .map(|t| t.to_rfc3339())
            .or_else(|| s.status_updated_at.and_then(ms_to_rfc3339)),
        runner_hosted: (probes.is_runner_hosted)(&s.session_id) || runner_ancestor == Some(true),
        runner_ancestor,
    }
}

/// Build the census snapshot — a pure function of the account homes, the live
/// pid set and the injected [`Probes`], so it is unit-tested against a temp dir
/// of fake registry rows and transcripts.
///
/// Rows are ordered by `(session_id, pid)` so two snapshots of an unchanged
/// machine serialize identically.
pub fn build_snapshot(
    config_dirs: &[PathBuf],
    live_pids: &HashSet<u32>,
    observed_at: DateTime<Utc>,
    runner_build: Option<String>,
    probes: &Probes<'_>,
) -> CensusSnapshot {
    let mut sessions: Vec<CensusRow> = Vec::new();
    for dir in config_dirs {
        // One home at a time so each row keeps the config dir it came from —
        // `read_live_sessions` reports the account label but not the path.
        for s in read_live_sessions(std::slice::from_ref(dir), live_pids) {
            sessions.push(row_from(dir, &s, probes));
        }
    }
    sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id).then(a.pid.cmp(&b.pid)));
    // Two homes may resolve to one path only if discovery failed to dedupe;
    // the (session_id, pid) primary key on coord's side must never collide.
    sessions.dedup_by(|a, b| a.session_id == b.session_id && a.pid == b.pid);
    CensusSnapshot {
        observed_at: observed_at.to_rfc3339(),
        runner_build,
        account_homes: config_dirs.len(),
        sessions,
    }
}

// ===========================================================================
// The device-local read (Phase 4 `no_pusher`)
// ===========================================================================

/// What the local census says about one session id, for the poller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalVerdict {
    /// No census yet, or the last one is stale — or a live row's ancestry
    /// could not be read on an OS that supports reading it.
    Unknown,
    /// No live Claude process on this device carries the id.
    NotLive,
    /// Live, and hosted by a runner — this one, or another on the device.
    LiveUnderARunner,
    /// Live, and no runner on the device hosts it: nothing can push to it.
    LiveUnhosted,
}

/// Per session id, folded over every live row (a session may have several
/// processes): did ANY row have a runner as host/ancestor, and was ANY row's
/// ancestry unreadable?
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Presence {
    any_runner: bool,
    any_unknown: bool,
}

/// The verdict for a live session. Pure, so both OS arms are tested anywhere.
fn presence_verdict(p: Presence, ancestry_supported: bool) -> LocalVerdict {
    if p.any_runner {
        LocalVerdict::LiveUnderARunner
    } else if ancestry_supported && p.any_unknown {
        LocalVerdict::Unknown
    } else {
        LocalVerdict::LiveUnhosted
    }
}

/// The last snapshot's live sessions, and when it was taken.
struct LocalCensus {
    taken: Instant,
    live: HashMap<String, Presence>,
}

fn local_census() -> &'static Mutex<Option<LocalCensus>> {
    static CELL: std::sync::OnceLock<Mutex<Option<LocalCensus>>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// Record a freshly built snapshot as this device's local census.
pub fn remember(snapshot: &CensusSnapshot, taken: Instant) {
    let mut live: HashMap<String, Presence> = HashMap::new();
    for r in snapshot.sessions.iter().filter(|r| r.pid_alive) {
        let p = live.entry(r.session_id.clone()).or_default();
        p.any_runner |= r.runner_hosted;
        p.any_unknown |= r.runner_ancestor.is_none();
    }
    if let Ok(mut g) = local_census().lock() {
        *g = Some(LocalCensus { taken, live });
    }
}

/// What the runner's own census says about `session_id`. A missing or stale
/// census (older than [`CENSUS_STALE_AFTER`]) is [`LocalVerdict::Unknown`],
/// never "not live".
pub fn local_verdict(session_id: &str, now: Instant) -> LocalVerdict {
    let Ok(g) = local_census().lock() else {
        return LocalVerdict::Unknown;
    };
    let Some(c) = g.as_ref() else {
        return LocalVerdict::Unknown;
    };
    if now.saturating_duration_since(c.taken) > CENSUS_STALE_AFTER {
        return LocalVerdict::Unknown;
    }
    match c.live.get(session_id) {
        None => LocalVerdict::NotLive,
        Some(p) => presence_verdict(*p, ANCESTRY_SUPPORTED),
    }
}

#[cfg(test)]
pub(crate) fn clear_local_census_for_test() {
    if let Ok(mut g) = local_census().lock() {
        *g = None;
    }
}

// ===========================================================================
// The publisher loop
// ===========================================================================

/// What the last post attempt came to — the edge the once-per-state-change log
/// fires on. Carries only a coarse class, never a response body.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PostState {
    Ok,
    NoDeviceId,
    Unpaired,
    Snapshot,
    /// coord answered 503 `session_census_unavailable`: its census tables are
    /// not deployed yet. A quiet, retryable state — not a fault of this runner.
    Unavailable,
    Http(u16),
    Transport,
}

/// Spawn the census publisher. PRIMARY runner only: a secondary/temp runner on
/// the same device would post the same device id and the two whole-snapshot
/// replaces would flap. (The primary still sees a secondary's sessions as
/// runner-hosted, through [`runner_ancestor`].)
pub fn spawn_publisher(app: tauri::AppHandle) {
    if !crate::instance::owns_shared_root_state() {
        info!("session census: not the primary runner — publisher not started");
        return;
    }
    crate::worker_supervisor::spawn_supervised_on_tauri("session_census_publisher", move || {
        let app = app.clone();
        async move { publisher_loop(app).await }
    });
}

async fn publisher_loop(app: tauri::AppHandle) {
    info!(
        "session census publisher started (interval={}s)",
        CENSUS_INTERVAL.as_secs()
    );
    let mut last: Option<PostState> = None;
    let mut ticker = tokio::time::interval(CENSUS_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let state = tick(&app).await;
        if last.as_ref() != Some(&state) {
            log_transition(&state);
            last = Some(state);
        }
    }
}

fn log_transition(state: &PostState) {
    match state {
        PostState::Ok => info!("session census: posted to coord"),
        PostState::NoDeviceId => {
            info!("session census: no local device id — not posting (state change)")
        }
        PostState::Unpaired => {
            info!("session census: no device credential (unpaired) — not posting (state change)")
        }
        PostState::Snapshot => warn!(
            "session census: process snapshot or census build indeterminate — skipped (an \
             empty census would read as every session dead)"
        ),
        PostState::Unavailable => info!(
            "session census: coord answered 503 (census store not deployed yet) — retrying each \
             tick quietly"
        ),
        PostState::Http(code) => {
            warn!("session census: coord answered HTTP {code} — will retry each tick")
        }
        PostState::Transport => {
            warn!("session census: coord unreachable — will retry each tick")
        }
    }
}

/// The managed state THIS runner's hosted predicate reads, copied out of
/// Tauri's state as owned `Arc`s so the census build can move to the blocking
/// pool.
struct HostedSubstrate {
    sessions: Option<Arc<crate::claude_session::SessionManager>>,
    registrar: Option<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>,
    lifecycle: Option<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>,
}

impl HostedSubstrate {
    fn from_app(app: &tauri::AppHandle) -> Self {
        use tauri::Manager;
        Self {
            sessions: app
                .try_state::<Arc<crate::claude_session::SessionManager>>()
                .map(|s| s.inner().clone()),
            registrar: app
                .try_state::<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
                .map(|s| s.inner().clone()),
            lifecycle: app
                .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
                .map(|s| s.inner().clone()),
        }
    }

    fn hosts(&self, id: &str) -> bool {
        match (&self.sessions, &self.lifecycle) {
            (Some(sm), Some(store)) => crate::mcp::session_message_poller::runner_hosts(
                sm,
                self.registrar.as_deref(),
                store,
                id,
            ),
            // Substrate not up yet: nothing can be pushed to by THIS runner.
            _ => false,
        }
    }
}

async fn tick(app: &tauri::AppHandle) -> PostState {
    let Some(device_id) = crate::agent_runtime::load_local_device_id() else {
        return PostState::NoDeviceId;
    };
    // The PAIRED-DEVICE JWT, resolved ONCE and presented verbatim below.
    // coord's ingest 403s an agent or bootstrap token, and a path device that
    // differs from the token's. No credential ⇒ no post: never anonymous.
    let Some(token) = crate::auth::device_bearer_scoped(crate::auth::TenantScope::Device) else {
        return PostState::Unpaired;
    };

    let snap = crate::process_capture::process_tree::snapshot_process_table_public().await;
    let live_pids = match super::claude_session_registry::live_pids_from_snapshot(&snap) {
        Ok(p) => p,
        Err(e) => {
            debug!(error = %e, "session census: process snapshot indeterminate");
            return PostState::Snapshot;
        }
    };
    let taken = Instant::now();
    let substrate = HostedSubstrate::from_app(app);
    // Every account home's registry, every transcript tail, every /proc read:
    // filesystem work, so it runs on the blocking pool, never on an async worker.
    let built = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        let hosted = |id: &str| substrate.hosts(id);
        build_snapshot(
            &crate::terminal::transcript::find_claude_config_dirs(),
            &live_pids,
            Utc::now(),
            Some(env!("RUNNER_BUILD_ID").to_string()),
            &Probes {
                tmux_pane_of: &tmux_pane_of_pid,
                read_stat: &read_proc_stat,
                is_runner_hosted: &hosted,
            },
        )
    })
    .await;
    let snapshot = match built {
        Ok(s) => s,
        Err(e) => {
            debug!(error = %e, "session census: build task failed");
            return PostState::Snapshot;
        }
    };
    remember(&snapshot, taken);
    debug!(
        rows = snapshot.sessions.len(),
        homes = snapshot.account_homes,
        "session census: snapshot built"
    );

    let (base, _src) = qontinui_runner_lib::profiles::coord_base_with_source();
    let url = format!(
        "{}/coord/session-census/{device_id}",
        base.trim_end_matches('/')
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(_) => return PostState::Transport,
    };
    // coord-auth-exempt(device-jwt-required): `token` is the paired-device JWT
    // resolved once above via `device_bearer_scoped(TenantScope::Device)`; the
    // tick returns `Unpaired` before reaching here when there is none, so this
    // never posts anonymously. coord's census ingest accepts only that
    // credential, for the path device id.
    match client
        .post(&url)
        .bearer_auth(&token)
        .json(&snapshot)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => PostState::Ok,
        Ok(resp) if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE => {
            PostState::Unavailable
        }
        Ok(resp) => PostState::Http(resp.status().as_u16()),
        Err(_) => PostState::Transport,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const SID_A: &str = "11111111-1111-4111-8111-111111111111";
    const SID_B: &str = "22222222-2222-4222-8222-222222222222";
    const SID_DEAD: &str = "33333333-3333-4333-8333-333333333333";

    fn no_pane(_: u32) -> Option<String> {
        None
    }
    fn no_stat(_: u32) -> Option<(String, u32)> {
        None
    }
    fn not_hosted(_: &str) -> bool {
        false
    }
    fn no_probes() -> Probes<'static> {
        Probes {
            tmux_pane_of: &no_pane,
            read_stat: &no_stat,
            is_runner_hosted: &not_hosted,
        }
    }

    fn home(root: &Path, name: &str) -> PathBuf {
        let d = root.join(name);
        fs::create_dir_all(d.join("sessions")).unwrap();
        fs::create_dir_all(d.join("projects")).unwrap();
        d
    }

    fn registry_row(home: &Path, pid: u32, sid: &str, extra: &str) {
        let body = format!(
            r#"{{"pid":{pid},"sessionId":"{sid}","cwd":"/work/{pid}","startedAt":1790794061417,"kind":"interactive","status":"idle","updatedAt":1790832214533{extra}}}"#
        );
        fs::write(home.join("sessions").join(format!("{pid}.json")), body).unwrap();
    }

    /// A transcript whose last turn line is stamped `ts`, followed by a
    /// non-turn line (a summary) that must not count.
    fn transcript(home: &Path, project: &str, sid: &str, ts: &str) {
        let p = home.join("projects").join(project);
        fs::create_dir_all(&p).unwrap();
        let body = format!(
            "{{\"type\":\"user\",\"timestamp\":\"2020-01-01T00:00:00Z\",\"message\":{{\"content\":\"secret\"}}}}\n\
             {{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"message\":{{\"content\":\"x\"}}}}\n\
             {{\"type\":\"summary\",\"timestamp\":\"2030-01-01T00:00:00Z\"}}\n"
        );
        fs::write(p.join(format!("{sid}.jsonl")), body).unwrap();
    }

    #[test]
    fn account_labels_follow_the_contract() {
        assert_eq!(
            account_label(Path::new("/home/x/.claude-tiohorst")).as_deref(),
            Some("tiohorst")
        );
        assert_eq!(
            account_label(Path::new("/home/x/.claude")).as_deref(),
            Some("default")
        );
        assert_eq!(
            account_label(Path::new("/srv/pinned")).as_deref(),
            Some("pinned")
        );
    }

    #[test]
    fn tmux_pane_is_the_only_value_read_from_environ() {
        let env = b"SECRET_TOKEN=hunter2\0TMUX=/tmp/tmux-1000/default,1,0\0TMUX_PANE=%452\0HOME=/home/x\0";
        assert_eq!(tmux_pane_from_environ(env).as_deref(), Some("%452"));
        assert_eq!(tmux_pane_from_environ(b"HOME=/home/x\0"), None);
        // Anything that is not a pane id is refused, not forwarded.
        assert_eq!(tmux_pane_from_environ(b"TMUX_PANE=oops\0"), None);
        assert_eq!(tmux_pane_from_environ(b"TMUX_PANE=%\0"), None);
        assert_eq!(
            tmux_pane_from_locator("t-sess:@650.%652").as_deref(),
            Some("%652")
        );
        assert_eq!(tmux_pane_from_locator("no-pane"), None);
    }

    #[test]
    fn snapshot_spans_every_home_and_keeps_only_live_pids() {
        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-tiohorst");
        let d = home(tmp.path(), ".claude");
        registry_row(
            &a,
            100,
            SID_A,
            r#","procStart":"285826587","entrypoint":"cli","name":"steward","nameSource":"user","statusUpdatedAt":1790832214000,"tmux":"s:@1.%9""#,
        );
        registry_row(
            &d,
            200,
            SID_B,
            r#","name":"root-bb","nameSource":"derived""#,
        );
        // A stale file from a crashed process: its pid is not live.
        registry_row(&a, 300, SID_DEAD, "");
        transcript(&a, "-work-100", SID_A, "2026-10-01T05:00:00Z");
        // Same id, second project dir, a LATER turn: the newest turn wins.
        transcript(&a, "-elsewhere", SID_A, "2026-10-01T06:04:00Z");

        let live: HashSet<u32> = [100, 200].into_iter().collect();
        let observed = Utc.timestamp_opt(1_790_900_000, 0).single().unwrap();
        let snap = build_snapshot(
            &[a.clone(), d.clone()],
            &live,
            observed,
            Some("build-1".into()),
            &Probes {
                tmux_pane_of: &|pid| (pid == 200).then(|| "%414".to_string()),
                // pid 100 → 50 → 1: no runner ancestor; pid 200 is not walked
                // past its own unreadable parent.
                read_stat: &|pid| match pid {
                    100 => Some(("claude".into(), 50)),
                    50 => Some(("tmux: server".into(), 1)),
                    _ => None,
                },
                is_runner_hosted: &|sid| sid == SID_B,
            },
        );

        assert_eq!(snap.account_homes, 2);
        assert_eq!(snap.runner_build.as_deref(), Some("build-1"));
        assert_eq!(snap.observed_at, observed.to_rfc3339());
        let ids: Vec<&str> = snap
            .sessions
            .iter()
            .map(|r| r.session_id.as_str())
            .collect();
        assert_eq!(ids, vec![SID_A, SID_B], "dead pid dropped, rows sorted");

        let ra = &snap.sessions[0];
        assert_eq!(ra.pid, 100);
        assert!(ra.pid_alive);
        assert_eq!(ra.account.as_deref(), Some("tiohorst"));
        assert_eq!(ra.config_dir.as_deref(), Some(a.to_string_lossy().as_ref()));
        assert_eq!(ra.cwd.as_deref(), Some("/work/100"));
        assert_eq!(ra.proc_start.as_deref(), Some("285826587"));
        assert_eq!(ra.entrypoint.as_deref(), Some("cli"));
        assert_eq!(ra.kind.as_deref(), Some("interactive"));
        assert_eq!(ra.window_name.as_deref(), Some("steward"));
        assert_eq!(ra.window_name_source.as_deref(), Some("user"));
        assert_eq!(ra.registry_status.as_deref(), Some("idle"));
        assert!(ra.registry_status_updated_at.is_some());
        assert!(ra.started_at.is_some());
        // No environ pane for pid 100 → the registry locator's pane.
        assert_eq!(ra.tmux_pane.as_deref(), Some("%9"));
        assert_eq!(
            ra.last_acted_at.as_deref(),
            Some("2026-10-01T06:04:00+00:00"),
            "last turn line's timestamp, never the later summary line"
        );
        assert!(!ra.runner_hosted);
        assert_eq!(ra.runner_ancestor, Some(false));

        let rb = &snap.sessions[1];
        assert_eq!(rb.account.as_deref(), Some("default"));
        assert_eq!(rb.tmux_pane.as_deref(), Some("%414"), "environ wins");
        assert_eq!(
            rb.last_acted_at, None,
            "no transcript and no statusUpdatedAt ⇒ null"
        );
        assert_eq!(rb.proc_start, None);
        assert!(rb.runner_hosted);
    }

    #[test]
    fn an_mtime_only_touch_does_not_move_last_acted_at() {
        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-x");
        transcript(&a, "p1", SID_A, "2026-09-26T16:07:00Z");
        let path = a.join("projects/p1").join(format!("{SID_A}.jsonl"));
        let before = last_acted_at(&a, SID_A).unwrap();
        // Touch: the file's mtime jumps to now with no new turn line.
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::SystemTime::now())
            .unwrap();
        assert_eq!(last_acted_at(&a, SID_A).unwrap(), before);
        assert_eq!(before.to_rfc3339(), "2026-09-26T16:07:00+00:00");
        assert_eq!(last_acted_at(&a, SID_B), None);
    }

    #[test]
    fn a_turn_line_beyond_the_first_tail_window_is_still_found() {
        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-x");
        let p = a.join("projects/p");
        fs::create_dir_all(&p).unwrap();
        // One turn line, then >64 KB of non-turn records after it.
        let mut body = String::from("{\"type\":\"user\",\"timestamp\":\"2026-10-01T01:00:00Z\"}\n");
        let filler = format!(
            "{{\"type\":\"progress\",\"pad\":\"{}\"}}\n",
            "x".repeat(1000)
        );
        for _ in 0..100 {
            body.push_str(&filler);
        }
        fs::write(p.join(format!("{SID_A}.jsonl")), body).unwrap();
        assert_eq!(
            last_acted_at(&a, SID_A).map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-01T01:00:00+00:00")
        );
    }

    #[test]
    fn no_turn_line_falls_back_to_registry_status_updated_at() {
        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-x");
        registry_row(&a, 100, SID_A, r#","statusUpdatedAt":1790832214000"#);
        let live: HashSet<u32> = [100].into_iter().collect();
        let snap = build_snapshot(&[a], &live, Utc::now(), None, &no_probes());
        assert_eq!(snap.sessions[0].last_acted_at, ms_to_rfc3339(1790832214000));
    }

    #[test]
    fn the_local_census_is_tri_state_and_goes_unknown_when_stale() {
        // The ONLY test touching the process-global cell, so no ordering race.
        clear_local_census_for_test();
        let t0 = Instant::now();
        assert_eq!(
            local_verdict(SID_A, t0),
            LocalVerdict::Unknown,
            "no census yet"
        );

        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-x");
        registry_row(&a, 100, SID_A, "");
        registry_row(&a, 200, SID_B, "");
        let live: HashSet<u32> = [100, 200].into_iter().collect();
        // pid 100 sits under ANOTHER runner (pid 7, a secondary); pid 200 has
        // no runner anywhere above it.
        let snap = build_snapshot(
            &[a],
            &live,
            Utc::now(),
            None,
            &Probes {
                tmux_pane_of: &|_| None,
                read_stat: &|pid| match pid {
                    100 => Some(("claude".into(), 7)),
                    7 => Some(("qontinui-runner".into(), 1)),
                    200 => Some(("claude".into(), 1)),
                    _ => None,
                },
                is_runner_hosted: &|_| false,
            },
        );
        assert!(
            snap.sessions[0].runner_hosted,
            "a secondary runner's session is hosted"
        );
        assert!(!snap.sessions[1].runner_hosted);
        remember(&snap, t0);

        assert_eq!(local_verdict(SID_A, t0), LocalVerdict::LiveUnderARunner);
        assert_eq!(local_verdict(SID_B, t0), LocalVerdict::LiveUnhosted);
        assert_eq!(local_verdict(SID_DEAD, t0), LocalVerdict::NotLive);
        let stale = t0 + CENSUS_STALE_AFTER + Duration::from_secs(1);
        assert_eq!(
            local_verdict(SID_A, stale),
            LocalVerdict::Unknown,
            "stale is UNKNOWN"
        );
        clear_local_census_for_test();
    }

    #[test]
    fn presence_verdict_claims_unhosted_only_without_any_runner() {
        let runner = Presence {
            any_runner: true,
            any_unknown: true,
        };
        let unknown = Presence {
            any_runner: false,
            any_unknown: true,
        };
        let clear = Presence {
            any_runner: false,
            any_unknown: false,
        };
        for supported in [true, false] {
            assert_eq!(
                presence_verdict(runner, supported),
                LocalVerdict::LiveUnderARunner
            );
            assert_eq!(
                presence_verdict(clear, supported),
                LocalVerdict::LiveUnhosted
            );
        }
        // Linux: an unreadable walk is UNKNOWN, never "no runner".
        assert_eq!(presence_verdict(unknown, true), LocalVerdict::Unknown);
        // Off Linux ancestry is never read: the pre-ancestry behaviour.
        assert_eq!(presence_verdict(unknown, false), LocalVerdict::LiveUnhosted);
    }

    #[test]
    fn runner_ancestry_walks_a_fake_proc_tree() {
        // 900 (claude) → 800 (bash) → 700 (qontinui-runner) → 1
        let tree = |pid: u32| match pid {
            900 => Some(("claude".to_string(), 800)),
            800 => Some(("bash".to_string(), 700)),
            700 => Some(("qontinui-runner".to_string(), 1)),
            // 600 (claude) → 500 (tmux) → 1, no runner
            600 => Some(("claude".to_string(), 500)),
            500 => Some(("tmux: server".to_string(), 1)),
            // 400 → 300 whose stat is unreadable (exited mid-walk)
            400 => Some(("claude".to_string(), 300)),
            // 200 → 100 → 200: a cycle
            200 => Some(("a".to_string(), 100)),
            100 => Some(("b".to_string(), 200)),
            // 50 → 50: self-parented
            50 => Some(("c".to_string(), 50)),
            _ => None,
        };
        assert_eq!(runner_ancestor(900, &tree), Some(true));
        assert_eq!(runner_ancestor(600, &tree), Some(false));
        assert_eq!(
            runner_ancestor(400, &tree),
            None,
            "unreadable link is UNKNOWN"
        );
        assert_eq!(
            runner_ancestor(200, &tree),
            None,
            "a cycle hits the depth bound"
        );
        assert_eq!(runner_ancestor(50, &tree), None);
        assert_eq!(runner_ancestor(12345, &tree), None, "unreadable self");
        // The process itself is never its own ancestor.
        let me_runner = |pid: u32| match pid {
            10 => Some(("qontinui-runner".to_string(), 1)),
            _ => None,
        };
        assert_eq!(runner_ancestor(10, &me_runner), Some(false));
    }

    #[test]
    fn a_claude_nested_in_a_runner_hosted_session_is_not_runner_hosted() {
        // 900 (inner claude, started by a tool call) → 850 (bash) →
        // 800 (outer claude, runner-hosted) → 700 (qontinui-runner) → 1
        let tree = |pid: u32| match pid {
            900 => Some(("claude".to_string(), 850)),
            850 => Some(("bash".to_string(), 800)),
            800 => Some(("claude".to_string(), 700)),
            700 => Some(("qontinui-runner".to_string(), 1)),
            _ => None,
        };
        assert_eq!(
            runner_ancestor(800, &tree),
            Some(true),
            "outer: nearest is the runner"
        );
        assert_eq!(
            runner_ancestor(900, &tree),
            Some(false),
            "inner: nearest supervisor is another claude, not a runner"
        );
    }

    #[test]
    fn a_transient_retry_failure_is_never_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-x");
        let p = a.join("projects/p");
        fs::create_dir_all(&p).unwrap();
        let path = p.join(format!("{SID_A}.jsonl"));
        let filler = format!(
            "{{\"type\":\"progress\",\"pad\":\"{}\"}}\n",
            "x".repeat(1000)
        );
        let mut body = String::from("{\"type\":\"user\",\"timestamp\":\"2026-10-01T03:00:00Z\"}\n");
        for _ in 0..100 {
            body.push_str(&filler);
        }
        fs::write(&path, body).unwrap();
        // The first window reads; the wide retry fails transiently.
        let flaky = |p: &Path, max: u64| {
            if max == TAIL_FIRST {
                read_tail(p, max)
            } else {
                None
            }
        };
        assert_eq!(last_turn_in_file_with(&path, &flaky), None);
        assert!(
            !retry_cache().lock().unwrap().contains_key(&path),
            "a failed read must not be cached"
        );
        // Next tick the read succeeds and finds the turn.
        assert_eq!(
            last_turn_in_file(&path).map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-01T03:00:00+00:00")
        );
    }

    #[test]
    fn proc_stat_parsing_survives_spaces_and_parens_in_comm() {
        assert_eq!(
            parse_proc_stat("1234 (qontinui-runner) S 1 1234 1234 0 -1"),
            Some(("qontinui-runner".to_string(), 1))
        );
        assert_eq!(
            parse_proc_stat("77 (tmux: server (x)) S 5 77 77"),
            Some(("tmux: server (x)".to_string(), 5))
        );
        assert_eq!(parse_proc_stat("garbage"), None);
    }

    #[test]
    fn a_non_uuid_session_id_never_reaches_a_path() {
        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-x");
        transcript(&a, "p", SID_A, "2026-10-01T01:00:00Z");
        assert!(is_uuid_shaped(SID_A));
        for bad in ["../x", "", "not-a-uuid", "11111111111141118111111111111111"] {
            assert!(!is_uuid_shaped(bad), "{bad}");
            assert_eq!(last_acted_at(&a, bad), None);
        }
    }

    #[test]
    fn a_half_written_multibyte_character_does_not_null_last_acted_at() {
        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-x");
        let p = a.join("projects/p");
        fs::create_dir_all(&p).unwrap();
        let mut body =
            b"{\"type\":\"assistant\",\"timestamp\":\"2026-10-01T02:00:00Z\"}\n".to_vec();
        // A trailing record cut inside a 3-byte UTF-8 character.
        body.extend_from_slice(b"{\"type\":\"user\",\"x\":\"\xE2\x82");
        fs::write(p.join(format!("{SID_A}.jsonl")), body).unwrap();
        assert_eq!(
            last_acted_at(&a, SID_A).map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-01T02:00:00+00:00")
        );
    }

    #[test]
    fn the_wide_retry_is_cached_per_path_and_size() {
        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-x");
        let p = a.join("projects/p");
        fs::create_dir_all(&p).unwrap();
        let path = p.join(format!("{SID_A}.jsonl"));
        let filler = format!(
            "{{\"type\":\"progress\",\"pad\":\"{}\"}}\n",
            "x".repeat(1000)
        );
        let write = |ts: &str| {
            let mut body = format!("{{\"type\":\"user\",\"timestamp\":\"{ts}\"}}\n");
            for _ in 0..100 {
                body.push_str(&filler);
            }
            fs::write(&path, body).unwrap();
        };
        write("2026-10-01T01:00:00Z");
        let first = last_acted_at(&a, SID_A).unwrap();
        // Same SIZE, different content: the cached answer is served, proving
        // the 1 MB window was not re-read.
        write("2026-10-01T09:00:00Z");
        assert_eq!(last_acted_at(&a, SID_A).unwrap(), first);
        // A size change invalidates it.
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut f, filler.as_bytes()).unwrap();
        assert_eq!(
            last_acted_at(&a, SID_A).map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-10-01T09:00:00+00:00")
        );
    }

    #[test]
    fn snapshot_serializes_with_the_contract_field_names() {
        let snap = CensusSnapshot {
            observed_at: "2026-10-01T00:00:00+00:00".into(),
            runner_build: None,
            account_homes: 17,
            sessions: vec![CensusRow {
                session_id: SID_A.into(),
                pid: 1,
                pid_alive: true,
                proc_start: None,
                started_at: None,
                account: Some("tiohorst".into()),
                config_dir: None,
                cwd: None,
                entrypoint: None,
                kind: None,
                tmux_pane: Some("%1".into()),
                window_name: None,
                window_name_source: None,
                registry_status: None,
                registry_status_updated_at: None,
                last_acted_at: None,
                runner_hosted: false,
                runner_ancestor: Some(true),
            }],
        };
        let v = serde_json::to_value(&snap).unwrap();
        for k in ["observed_at", "runner_build", "account_homes", "sessions"] {
            assert!(v.get(k).is_some(), "missing top-level {k}");
        }
        let row = &v["sessions"][0];
        for k in [
            "session_id",
            "pid",
            "pid_alive",
            "proc_start",
            "started_at",
            "account",
            "config_dir",
            "cwd",
            "entrypoint",
            "kind",
            "tmux_pane",
            "window_name",
            "window_name_source",
            "registry_status",
            "registry_status_updated_at",
            "last_acted_at",
            "runner_hosted",
        ] {
            assert!(row.get(k).is_some(), "missing row field {k}");
        }
        assert_eq!(row.as_object().unwrap().len(), 17, "no extra fields");
    }
}
