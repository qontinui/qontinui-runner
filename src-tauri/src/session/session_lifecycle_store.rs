//! Durable, backend-owned terminal-session lifecycle registry, keyed by
//! `claudeSessionId`.
//!
//! ## Problem
//!
//! The frontend grid previously tracked "which Claude sessions exist and
//! which zone each belongs to" in a `localStorage` snapshot. That snapshot
//! is fragile: it duplicates sessions across restarts (a fresh terminal id
//! per launch keys a new row), and it can't observe a session that died
//! while the UI wasn't looking. The result is ghost / duplicate session
//! tiles in the grid.
//!
//! ## Fix
//!
//! Make the runner backend the source of truth. Each terminal that hosts a
//! Claude session records a [`TerminalSessionRecord`] keyed by the *stable*
//! `claude_session_id`. Re-opening the same Claude session (same id) updates
//! the existing record in place rather than appending a duplicate — the
//! structural dedup-by-key is what kills the duplicate-session bug. A slow
//! liveness poll ([`classify`]) lazily flips dead sessions to `closed`,
//! never closing on uncertainty.
//!
//! ## Storage
//!
//! Mirrors [`crate::session::pane_store::PaneSessionStore`]: a single JSON
//! file at `<.qontinui>/runner/terminal-sessions.json` (an object map
//! `claude_session_id -> record`), rewritten atomically via temp-file +
//! rename. Missing / corrupt file → empty map (a fresh registry is the safe
//! default; the poll re-discovers live sessions).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Closed records older than this are pruned (24h in millis).
const CLOSED_RETENTION_MS: i64 = 86_400_000;
/// Open records not seen for this long are pruned (7d in millis).
const OPEN_STALE_MS: i64 = 604_800_000;
/// A record closed with reason `"pty-exit"` (its PTY died — e.g. a graceful
/// runner restart firing `handleExit` on every live PTY) is still restorable
/// for this long after the close. Beyond the window, or for any other close
/// reason (explicit user close, `"poll-dead"`, …), it is NOT restorable.
/// 10 minutes in millis.
const RESTORABLE_PTY_EXIT_MS: i64 = 600_000;

/// One persisted terminal-session lifecycle record, keyed by
/// `claude_session_id`. Timestamps are unix epoch millis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionRecord {
    /// Stable Claude session id — the map key and the source of truth for
    /// session identity across restarts.
    pub claude_session_id: String,
    /// `CLAUDE_CONFIG_DIR` / config dir the session was launched under, if
    /// known.
    pub config_dir: Option<String>,
    /// Working directory of the session, if known.
    pub working_dir: Option<String>,
    /// The grid page this session's tile belongs to.
    pub page_id: String,
    /// The grid zone index this session's tile belongs to.
    pub zone_index: i32,
    /// Human-readable title shown in the grid tile, if known.
    pub title: Option<String>,
    /// The runner terminal id currently hosting this session.
    pub terminal_id: String,
    /// Unix millis when this session was first opened.
    pub opened_at: i64,
    /// Unix millis of the most recent liveness/touch.
    pub last_seen_at: i64,
    /// `"open"` | `"closed"`.
    pub state: String,
    /// Unix millis when this session was closed, if closed.
    pub closed_at: Option<i64>,
    /// Why the session was closed (e.g. `"poll-dead"`, an explicit reason).
    pub close_reason: Option<String>,
}

/// Durable map of `claude_session_id -> TerminalSessionRecord`. Cheap to
/// clone-share via `Arc`; every mutation takes the lock and rewrites the
/// backing file atomically.
#[derive(Debug)]
pub struct SessionLifecycleStore {
    path: PathBuf,
    map: Mutex<HashMap<String, TerminalSessionRecord>>,
}

impl SessionLifecycleStore {
    /// Open (or initialize) the store at `path`, loading any existing map.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let map = load_map(&path);
        Ok(Self {
            path,
            map: Mutex::new(map),
        })
    }

    /// Insert or refresh a session by `claude_session_id`. If a record
    /// already exists, its `opened_at` is preserved; the record is always
    /// (re-)marked `open` (clearing `closed_at` / `close_reason`), its
    /// mutable fields are refreshed, and `last_seen_at` is bumped to now.
    /// A brand-new record sets `opened_at == last_seen_at == now`. Atomic-
    /// writes after mutating.
    pub fn record_open(&self, rec: TerminalSessionRecord) {
        let now = Utc::now().timestamp_millis();
        let snapshot = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on record_open");
                    return;
                }
            };
            let entry =
                m.entry(rec.claude_session_id.clone())
                    .or_insert_with(|| TerminalSessionRecord {
                        opened_at: now,
                        ..rec.clone()
                    });
            // Preserve opened_at on an existing record; refresh everything
            // else and re-open.
            entry.config_dir = rec.config_dir;
            entry.working_dir = rec.working_dir;
            entry.page_id = rec.page_id;
            entry.zone_index = rec.zone_index;
            entry.title = rec.title;
            entry.terminal_id = rec.terminal_id;
            entry.state = "open".to_string();
            entry.closed_at = None;
            entry.close_reason = None;
            entry.last_seen_at = now;
            m.clone()
        };
        self.persist(&snapshot);
    }

    /// Mark an open session closed. No-op (no error) if the session is
    /// absent or already closed.
    pub fn record_close(&self, claude_session_id: &str, reason: &str) {
        let now = Utc::now().timestamp_millis();
        let snapshot = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on record_close");
                    return;
                }
            };
            match m.get_mut(claude_session_id) {
                Some(rec) if rec.state == "open" => {
                    rec.state = "closed".to_string();
                    rec.closed_at = Some(now);
                    rec.close_reason = Some(reason.to_string());
                }
                _ => return, // absent or already closed — nothing to flush
            }
            m.clone()
        };
        self.persist(&snapshot);
    }

    /// Bump `last_seen_at` on a present session. No-op (no write) if absent.
    pub fn touch(&self, claude_session_id: &str) {
        let now = Utc::now().timestamp_millis();
        let snapshot = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on touch");
                    return;
                }
            };
            match m.get_mut(claude_session_id) {
                Some(rec) => rec.last_seen_at = now,
                None => return,
            }
            m.clone()
        };
        self.persist(&snapshot);
    }

    /// Clone of every record whose `state == "open"`.
    pub fn open_records(&self) -> Vec<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => m.values().filter(|r| r.state == "open").cloned().collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on open_records");
                Vec::new()
            }
        }
    }

    /// Clone of every record that should be RESTORED on boot. This is the
    /// superset of [`Self::open_records`] used by the restore path:
    ///
    /// - every `state == "open"` record (a hard crash leaves records open), PLUS
    /// - every `state == "closed"` record whose `close_reason == "pty-exit"`
    ///   (its PTY died — the case a GRACEFUL restart produces by firing
    ///   `handleExit` on every live PTY) that closed within the
    ///   [`RESTORABLE_PTY_EXIT_MS`] grace window.
    ///
    /// Records closed for any other reason (explicit user close, `"poll-dead"`,
    /// …) or pty-exit closes older than the grace window are excluded — a user
    /// who closes a tab does not want it resurrected, and a long-dead pty-exit
    /// close is stale.
    pub fn restorable_records(&self, now_ms: i64) -> Vec<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => m
                .values()
                .filter(|r| {
                    if r.state == "open" {
                        return true;
                    }
                    if r.state == "closed" && r.close_reason.as_deref() == Some("pty-exit") {
                        return match r.closed_at {
                            Some(closed_at) => now_ms - closed_at <= RESTORABLE_PTY_EXIT_MS,
                            None => false,
                        };
                    }
                    false
                })
                .cloned()
                .collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on restorable_records");
                Vec::new()
            }
        }
    }

    /// Drop `closed` records closed > 24h ago and `open` records not seen
    /// for > 7d. Atomic-writes only if something changed.
    pub fn prune(&self, now: i64) {
        let snapshot = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on prune");
                    return;
                }
            };
            let before = m.len();
            m.retain(|_, rec| {
                if rec.state == "closed" {
                    match rec.closed_at {
                        Some(closed_at) => now - closed_at <= CLOSED_RETENTION_MS,
                        // Closed without a timestamp — keep (shouldn't happen).
                        None => true,
                    }
                } else {
                    now - rec.last_seen_at <= OPEN_STALE_MS
                }
            });
            if m.len() == before {
                return; // nothing pruned — skip the write
            }
            m.clone()
        };
        self.persist(&snapshot);
    }

    /// Best-effort atomic flush. A write failure is logged, not propagated —
    /// the in-memory map still reflects the mutation for this process.
    fn persist(&self, snapshot: &HashMap<String, TerminalSessionRecord>) {
        if let Err(e) = write_map(&self.path, snapshot) {
            warn!(
                error = %e,
                path = %self.path.display(),
                "session_lifecycle_store: persist failed — kept in memory only"
            );
        }
    }
}

fn load_map(path: &Path) -> HashMap<String, TerminalSessionRecord> {
    if !path.exists() {
        return HashMap::new();
    }
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<HashMap<String, TerminalSessionRecord>>(&bytes)
        {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "session_lifecycle_store: corrupt map file — starting empty"
                );
                HashMap::new()
            }
        },
        Err(e) => {
            warn!(error = %e, "session_lifecycle_store: read failed — starting empty");
            HashMap::new()
        }
    }
}

fn write_map(path: &Path, map: &HashMap<String, TerminalSessionRecord>) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Liveness-poll decision core (pure)
// ---------------------------------------------------------------------------

/// What the liveness poll should do with one open session this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollAction {
    /// The session is alive and busy — refresh its `last_seen_at`.
    KeepAlive,
    /// The session's shell is alive but has zero descendants for the first
    /// time — wait one more tick before closing (debounce a momentary lull).
    NeedsConfirm,
    /// The session is dead — flip it to `closed`.
    Close,
    /// Uncertain — do nothing (NEVER close on uncertainty).
    Skip,
}

/// Pure liveness-classification core. Asymmetric by design: we only ever
/// `Close` on a *confident* dead signal, and `Skip` (never close) on any
/// uncertainty (snapshot failure or no matching pty).
///
/// - `live_is_alive`: `Some(true)` shell pty alive, `Some(false)` dead,
///   `None` no matching pty for this session.
/// - `descendant_count`: number of descendant processes of the shell pid.
/// - `consecutive_dead`: count of prior consecutive zero-descendant ticks.
/// - `snapshot_ok`: whether the system-wide process snapshot succeeded.
pub fn classify(
    live_is_alive: Option<bool>,
    descendant_count: usize,
    consecutive_dead: u32,
    snapshot_ok: bool,
) -> PollAction {
    if !snapshot_ok {
        return PollAction::Skip;
    }
    match live_is_alive {
        None => PollAction::Skip,
        Some(false) => PollAction::Close,
        Some(true) => {
            if descendant_count > 0 {
                PollAction::KeepAlive
            } else if consecutive_dead < 1 {
                PollAction::NeedsConfirm
            } else {
                PollAction::Close
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn rec(id: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: Some("C:/cfg".to_string()),
            working_dir: Some("C:/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: 2,
            title: Some("Claude 1".to_string()),
            terminal_id: "term-abc".to_string(),
            // These get overwritten by record_open; seed sane values.
            opened_at: 0,
            last_seen_at: 0,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
        }
    }

    #[test]
    fn record_open_inserts_and_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        {
            let store = SessionLifecycleStore::open(&path).unwrap();
            store.record_open(rec("sess-1"));
            let open = store.open_records();
            assert_eq!(open.len(), 1);
            assert_eq!(open[0].claude_session_id, "sess-1");
            assert_eq!(open[0].state, "open");
            assert!(open[0].opened_at > 0);
            assert_eq!(open[0].opened_at, open[0].last_seen_at);
        }
        // Survives a "restart".
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert_eq!(store.open_records().len(), 1);
    }

    #[test]
    fn record_open_dedups_by_key_and_preserves_opened_at() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        store.record_open(rec("sess-1"));
        let opened_at = store.open_records()[0].opened_at;

        std::thread::sleep(std::time::Duration::from_millis(2));

        // Re-open the SAME claude session id with a new zone/terminal.
        let mut r2 = rec("sess-1");
        r2.zone_index = 9;
        r2.terminal_id = "term-xyz".to_string();
        r2.title = Some("Claude 1 (moved)".to_string());
        store.record_open(r2);

        let open = store.open_records();
        // Structural dedup: still exactly one record (no duplicate).
        assert_eq!(open.len(), 1, "same claude_session_id must not duplicate");
        assert_eq!(open[0].opened_at, opened_at, "opened_at preserved");
        assert!(open[0].last_seen_at > opened_at, "last_seen_at bumped");
        assert_eq!(open[0].zone_index, 9, "zone refreshed");
        assert_eq!(open[0].terminal_id, "term-xyz", "terminal refreshed");
    }

    #[test]
    fn record_open_reopens_a_closed_record() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-1"));
        store.record_close("sess-1", "poll-dead");
        assert!(store.open_records().is_empty());

        store.record_open(rec("sess-1"));
        let open = store.open_records();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].state, "open");
        assert!(open[0].closed_at.is_none());
        assert!(open[0].close_reason.is_none());
    }

    #[test]
    fn record_close_marks_closed_and_is_noop_when_absent_or_double() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        // Absent → no-op, no panic.
        store.record_close("ghost", "x");

        store.record_open(rec("sess-1"));
        store.record_close("sess-1", "poll-dead");
        assert!(store.open_records().is_empty());

        // Double close → no-op.
        store.record_close("sess-1", "again");

        // Reload and confirm the closed record persisted with its reason.
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert!(store.open_records().is_empty());
    }

    #[test]
    fn restorable_records_includes_open_and_in_grace_pty_exit_only() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        // An open record (hard-crash case) — IS restorable.
        store.record_open(rec("open-sess"));
        // A pty-exit close (graceful-restart case) — IS restorable while in grace.
        store.record_open(rec("pty-exit-sess"));
        store.record_close("pty-exit-sess", "pty-exit");
        // An explicit/user close — is NOT restorable.
        store.record_open(rec("user-closed-sess"));
        store.record_close("user-closed-sess", "explicit");
        // A pty-exit close aged beyond the grace window — is NOT restorable.
        store.record_open(rec("stale-pty-exit-sess"));
        store.record_close("stale-pty-exit-sess", "pty-exit");

        let now = Utc::now().timestamp_millis();

        // Age the stale pty-exit record's closed_at past the grace window.
        {
            let raw = std::fs::read(&path).unwrap();
            let mut m: HashMap<String, TerminalSessionRecord> =
                serde_json::from_slice(&raw).unwrap();
            m.get_mut("stale-pty-exit-sess").unwrap().closed_at =
                Some(now - RESTORABLE_PTY_EXIT_MS - 1000);
            let bytes = serde_json::to_vec_pretty(&m).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        let store = SessionLifecycleStore::open(&path).unwrap();

        let mut ids: Vec<String> = store
            .restorable_records(now)
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["open-sess".to_string(), "pty-exit-sess".to_string()],
            "open + in-grace pty-exit restorable; explicit + stale-pty-exit excluded"
        );

        // open_records() keeps strict-open semantics (only the open one).
        let open_ids: Vec<String> = store
            .open_records()
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        assert_eq!(open_ids, vec!["open-sess".to_string()]);
    }

    #[test]
    fn touch_bumps_last_seen_only_when_present() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-1"));
        let before = store.open_records()[0].last_seen_at;
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.touch("sess-1");
        let after = store.open_records()[0].last_seen_at;
        assert!(after > before);
        // Touching an absent id is a no-op (no panic).
        store.touch("ghost");
    }

    #[test]
    fn prune_drops_old_closed_and_stale_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        store.record_open(rec("fresh-open"));
        store.record_open(rec("stale-open"));
        store.record_open(rec("recent-closed"));
        store.record_open(rec("old-closed"));
        store.record_close("recent-closed", "done");
        store.record_close("old-closed", "done");

        // Hand-tamper timestamps via reload-mutate-rewrite would require
        // internals; instead choose a `now` far in the future so the
        // "old-closed" (closed_at ~ real now) is > 24h old, but tune the
        // fresh ones to survive. We set last_seen far enough back below by
        // re-recording with a manual now. Simpler: drive prune with a `now`
        // chosen relative to the real timestamps we just wrote.
        let now = Utc::now().timestamp_millis();

        // Push the records we want pruned into the past by re-writing the
        // file directly through the store's open/close round-trip is awkward;
        // instead assert behavior with synthetic far-future `now` after
        // forcibly aging two records.
        // Age `stale-open` and `old-closed` beyond their thresholds.
        {
            // Reload, mutate ages in-place, persist via a fresh store write.
            let raw = std::fs::read(&path).unwrap();
            let mut m: HashMap<String, TerminalSessionRecord> =
                serde_json::from_slice(&raw).unwrap();
            m.get_mut("stale-open").unwrap().last_seen_at = now - OPEN_STALE_MS - 1000;
            m.get_mut("old-closed").unwrap().closed_at = Some(now - CLOSED_RETENTION_MS - 1000);
            let bytes = serde_json::to_vec_pretty(&m).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }

        // Reopen so the store loads the aged timestamps, then prune.
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.prune(now);

        let store = SessionLifecycleStore::open(&path).unwrap();
        let raw = std::fs::read(&path).unwrap();
        let m: HashMap<String, TerminalSessionRecord> = serde_json::from_slice(&raw).unwrap();
        assert!(m.contains_key("fresh-open"), "fresh open survives");
        assert!(m.contains_key("recent-closed"), "recent closed survives");
        assert!(!m.contains_key("stale-open"), "stale open pruned");
        assert!(!m.contains_key("old-closed"), "old closed pruned");
        // open_records reflects the pruned set.
        let open_ids: Vec<String> = store
            .open_records()
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        assert_eq!(open_ids, vec!["fresh-open".to_string()]);
    }

    #[test]
    fn corrupt_file_starts_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert!(store.open_records().is_empty());
        // Still usable after recovery.
        store.record_open(rec("sess-1"));
        assert_eq!(store.open_records().len(), 1);
    }

    // --- classify() — every branch -----------------------------------------

    #[test]
    fn classify_skip_on_snapshot_failure() {
        // snapshot_ok=false dominates every other input.
        assert_eq!(classify(Some(false), 0, 5, false), PollAction::Skip);
        assert_eq!(classify(Some(true), 3, 0, false), PollAction::Skip);
        assert_eq!(classify(None, 0, 0, false), PollAction::Skip);
    }

    #[test]
    fn classify_skip_on_no_matching_pty() {
        assert_eq!(classify(None, 0, 0, true), PollAction::Skip);
        assert_eq!(classify(None, 9, 9, true), PollAction::Skip);
    }

    #[test]
    fn classify_close_on_dead_shell() {
        assert_eq!(classify(Some(false), 0, 0, true), PollAction::Close);
        assert_eq!(classify(Some(false), 5, 0, true), PollAction::Close);
    }

    #[test]
    fn classify_keepalive_when_alive_with_descendants() {
        assert_eq!(classify(Some(true), 1, 0, true), PollAction::KeepAlive);
        assert_eq!(classify(Some(true), 7, 9, true), PollAction::KeepAlive);
    }

    #[test]
    fn classify_needs_confirm_on_first_zero_descendant_tick() {
        assert_eq!(classify(Some(true), 0, 0, true), PollAction::NeedsConfirm);
    }

    #[test]
    fn classify_close_on_second_zero_descendant_tick() {
        assert_eq!(classify(Some(true), 0, 1, true), PollAction::Close);
        assert_eq!(classify(Some(true), 0, 5, true), PollAction::Close);
    }
}
