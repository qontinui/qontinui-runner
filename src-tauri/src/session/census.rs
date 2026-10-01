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
//! (the one `TMUX_PANE` variable is extracted from `/proc/<pid>/environ` and
//! everything else in that buffer is dropped unread), no credentials.
//!
//! ## Field sources (contract §2)
//!
//! | field | source |
//! |---|---|
//! | `account` | config-dir basename minus `.claude-`; `default` for `.claude` |
//! | `last_acted_at` | `timestamp` of the last `user`/`assistant` line in the tail of `<config_dir>/projects/*/<session_id>.jsonl`, else registry `statusUpdatedAt`, else null — NEVER the file mtime (it moves with no new turn: coord finding `124c0ce9`) |
//! | `tmux_pane` | Linux `/proc/<pid>/environ` `TMUX_PANE`, else the registry `tmux` locator's `%N` suffix |
//! | `runner_hosted` | the session-message poller's own "live local session the runner hosts" predicate ([`crate::mcp::session_message_poller::runner_hosts`]) |
//!
//! `pid_alive` is always `true` here: [`read_live_sessions`] drops a registry
//! row whose pid is not in the live process table (a crashed process cannot
//! delete its own file), so a row in the snapshot is a live process. A dead
//! session is reported by its ABSENCE from a fresh snapshot.
//!
//! ## Failure posture
//!
//! Best-effort throughout. An indeterminate process snapshot skips the tick
//! (posting an empty set would read as "every session on this device died");
//! a failed POST is logged once per state change, never per tick; nothing
//! here can panic the loop or block the lifecycle poll.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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

/// The `timestamp` of the LAST `user`/`assistant` line in `tail`, or `None`.
/// The first line of a tail window may be cut mid-record; it simply fails to
/// parse and is skipped.
fn last_turn_in(tail: &str) -> Option<DateTime<Utc>> {
    tail.lines().rev().find_map(|line| {
        let stamp: TurnStamp = serde_json::from_str(line).ok()?;
        if !matches!(stamp.kind.as_deref(), Some("user" | "assistant")) {
            return None;
        }
        DateTime::parse_from_rfc3339(stamp.timestamp.as_deref()?)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    })
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
/// `None` when no transcript carries a turn line (yet).
pub fn last_acted_at(config_dir: &Path, session_id: &str) -> Option<DateTime<Utc>> {
    let file = format!("{session_id}.jsonl");
    let entries = std::fs::read_dir(config_dir.join("projects")).ok()?;
    entries
        .flatten()
        .map(|e| e.path().join(&file))
        .filter(|p| p.is_file())
        .filter_map(|p| {
            let tail = crate::terminal::transcript::read_tail_bytes(&p, TAIL_FIRST)?;
            last_turn_in(&tail).or_else(|| {
                let len = std::fs::metadata(&p).ok()?.len();
                (len > TAIL_FIRST)
                    .then(|| crate::terminal::transcript::read_tail_bytes(&p, TAIL_RETRY))
                    .flatten()
                    .and_then(|t| last_turn_in(&t))
            })
        })
        .max()
}

/// Extract `TMUX_PANE` from a raw `/proc/<pid>/environ` buffer (NUL-separated
/// `KEY=value` entries). Every other entry is skipped without being decoded or
/// retained. A pane id that is not `%<digits>` is rejected rather than passed
/// on, so nothing but a pane id can ever leave this function.
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
#[cfg(target_os = "linux")]
pub fn tmux_pane_of_pid(pid: u32) -> Option<String> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    tmux_pane_from_environ(&environ)
}

#[cfg(not(target_os = "linux"))]
pub fn tmux_pane_of_pid(_pid: u32) -> Option<String> {
    None
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Project one registry row into a census row.
fn row_from(
    config_dir: &Path,
    s: &LiveClaudeSession,
    tmux_pane_of: &dyn Fn(u32) -> Option<String>,
    is_runner_hosted: &dyn Fn(&str) -> bool,
) -> CensusRow {
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
        tmux_pane: tmux_pane_of(s.pid)
            .or_else(|| s.tmux.as_deref().and_then(tmux_pane_from_locator)),
        window_name: non_empty(&s.name),
        window_name_source: s.name_source.clone(),
        registry_status: non_empty(&s.status),
        registry_status_updated_at: s.status_updated_at.and_then(ms_to_rfc3339),
        // The last turn line's own timestamp; else the registry's
        // `statusUpdatedAt` (weaker — it moves on status changes, and lags
        // real activity by days on idle-status rows); else null.
        last_acted_at: last_acted_at(config_dir, &s.session_id)
            .map(|t| t.to_rfc3339())
            .or_else(|| s.status_updated_at.and_then(ms_to_rfc3339)),
        runner_hosted: is_runner_hosted(&s.session_id),
    }
}

/// Build the census snapshot — a pure function of the account homes, the live
/// pid set and the two injected per-row probes, so it is unit-tested against a
/// temp dir of fake registry rows and transcripts.
///
/// Rows are ordered by `(session_id, pid)` so two snapshots of an unchanged
/// machine serialize identically.
pub fn build_snapshot(
    config_dirs: &[PathBuf],
    live_pids: &HashSet<u32>,
    observed_at: DateTime<Utc>,
    runner_build: Option<String>,
    tmux_pane_of: &dyn Fn(u32) -> Option<String>,
    is_runner_hosted: &dyn Fn(&str) -> bool,
) -> CensusSnapshot {
    let mut sessions: Vec<CensusRow> = Vec::new();
    for dir in config_dirs {
        // One home at a time so each row keeps the config dir it came from —
        // `read_live_sessions` reports the account label but not the path.
        for s in read_live_sessions(std::slice::from_ref(dir), live_pids) {
            sessions.push(row_from(dir, &s, tmux_pane_of, is_runner_hosted));
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

/// The last snapshot's live session ids, and when it was taken.
struct LocalCensus {
    taken: Instant,
    live_session_ids: HashSet<String>,
}

fn local_census() -> &'static Mutex<Option<LocalCensus>> {
    static CELL: std::sync::OnceLock<Mutex<Option<LocalCensus>>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// Record a freshly built snapshot as this device's local census.
pub fn remember(snapshot: &CensusSnapshot, taken: Instant) {
    let live_session_ids = snapshot
        .sessions
        .iter()
        .filter(|r| r.pid_alive)
        .map(|r| r.session_id.clone())
        .collect();
    if let Ok(mut g) = local_census().lock() {
        *g = Some(LocalCensus {
            taken,
            live_session_ids,
        });
    }
}

/// Is `session_id` a LIVE Claude process on this device, per the runner's own
/// census? Three-valued: `None` when no census was taken yet or the last one
/// is older than [`CENSUS_STALE_AFTER`] — UNKNOWN, never "not live".
pub fn live_on_this_device(session_id: &str, now: Instant) -> Option<bool> {
    let g = local_census().lock().ok()?;
    let c = g.as_ref()?;
    if now.saturating_duration_since(c.taken) > CENSUS_STALE_AFTER {
        return None;
    }
    Some(c.live_session_ids.contains(session_id))
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
/// the same device would post the same device id with a different
/// `runner_hosted` view and the two whole-snapshot replaces would flap.
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
            "session census: process snapshot indeterminate — skipped (an empty census would \
             read as every session dead)"
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

/// The runner-hosted predicate, bound to this tick's substrate.
fn hosted_probe(app: &tauri::AppHandle) -> Box<dyn Fn(&str) -> bool + '_> {
    use tauri::Manager;
    let sm = app.try_state::<std::sync::Arc<crate::claude_session::SessionManager>>();
    let reg =
        app.try_state::<std::sync::Arc<crate::claude_session::coord_register::AiCoordRegistrar>>();
    let store = app
        .try_state::<std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>();
    match (sm, store) {
        (Some(sm), Some(store)) => Box::new(move |id: &str| {
            crate::mcp::session_message_poller::runner_hosts(
                sm.inner(),
                reg.as_ref().map(|r| r.inner().as_ref()),
                store.inner(),
                id,
            )
        }),
        // Substrate not up yet: nothing can be pushed to, so nothing is
        // runner-hosted in the sense the poller means.
        _ => Box::new(|_: &str| false),
    }
}

/// Build this tick's snapshot. Synchronous on purpose: the runner-hosted probe
/// borrows managed state (not `Send`), so it must not live across an `.await`.
fn snapshot_now(app: &tauri::AppHandle, live_pids: &HashSet<u32>) -> CensusSnapshot {
    let config_dirs = crate::terminal::transcript::find_claude_config_dirs();
    let hosted = hosted_probe(app);
    build_snapshot(
        &config_dirs,
        live_pids,
        Utc::now(),
        Some(env!("RUNNER_BUILD_ID").to_string()),
        &tmux_pane_of_pid,
        hosted.as_ref(),
    )
}

async fn tick(app: &tauri::AppHandle) -> PostState {
    let Some(device_id) = crate::agent_runtime::load_local_device_id() else {
        return PostState::NoDeviceId;
    };
    let paired = matches!(
        crate::auth::AuthManager::new().get_access_token(),
        Ok(t) if !t.trim().is_empty()
    );
    if !paired {
        return PostState::Unpaired;
    }

    let snap = crate::process_capture::process_tree::snapshot_process_table_public().await;
    let live_pids = match super::claude_session_registry::live_pids_from_snapshot(&snap) {
        Ok(p) => p,
        Err(e) => {
            debug!(error = %e, "session census: process snapshot indeterminate");
            return PostState::Snapshot;
        }
    };
    let taken = Instant::now();
    let snapshot = snapshot_now(app, &live_pids);
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
    // The PAIRED-DEVICE JWT (`TenantScope::Device` → the device's own slot):
    // coord's ingest 403s an agent or bootstrap token, and a path device that
    // differs from the token's. Unpaired was already turned away above, so
    // this never posts anonymously.
    match crate::auth::attach_device_auth(client.post(&url).json(&snapshot))
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
            &|pid| (pid == 200).then(|| "%414".to_string()),
            &|sid| sid == SID_B,
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
        let snap = build_snapshot(&[a], &live, Utc::now(), None, &|_| None, &|_| false);
        assert_eq!(snap.sessions[0].last_acted_at, ms_to_rfc3339(1790832214000));
    }

    #[test]
    fn the_local_census_is_three_valued_and_goes_unknown_when_stale() {
        // The ONLY test touching the process-global cell, so no ordering race.
        clear_local_census_for_test();
        let t0 = Instant::now();
        assert_eq!(live_on_this_device(SID_A, t0), None, "no census yet");

        let tmp = tempfile::tempdir().unwrap();
        let a = home(tmp.path(), ".claude-x");
        registry_row(&a, 100, SID_A, "");
        let live: HashSet<u32> = [100].into_iter().collect();
        let snap = build_snapshot(&[a], &live, Utc::now(), None, &|_| None, &|_| false);
        remember(&snap, t0);

        assert_eq!(live_on_this_device(SID_A, t0), Some(true));
        assert_eq!(live_on_this_device(SID_B, t0), Some(false));
        let stale = t0 + CENSUS_STALE_AFTER + Duration::from_secs(1);
        assert_eq!(live_on_this_device(SID_A, stale), None, "stale is UNKNOWN");
        clear_local_census_for_test();
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
