//! Append-only, timestamped session-layout snapshot HISTORY — the recovery
//! safety net for the session-restore path (plan
//! `2026-07-03-runner-session-restore-windows-path-shim-fix`, Phase 4).
//!
//! ## Problem (2026-07-03 incident)
//!
//! A shim race sent every open session `poll-dead`; they aged out of the
//! 10-minute restore grace, and the mutable registry
//! ([`crate::session::session_lifecycle_store::SessionLifecycleStore`]) had
//! destructively pruned everything an operator would need to re-open the set
//! by hand. The open set was reconstructed lossily from transcript mtimes —
//! idle sessions, all page/zone/layout, and `config_dir` were lost.
//!
//! ## Fix
//!
//! A durable audit trail DERIVED from the same registry: every meaningful
//! registry change (session open / close / move / rename / confirm) and a
//! periodic heartbeat append one COMPLETE snapshot of the full session set —
//! one JSON line per snapshot — to
//! `~/.qontinui/runner/session-restore/session-snapshots[-<port>].jsonl`
//! (the runner's own app-data, next to
//! [`crate::session::claude_hook::session_restore_dir`]), so recovery works
//! even when the app won't start. Each session entry carries the full
//! recovery tuple: `configDir` (account), `claudeSessionId`, `provider`,
//! `pageId`, `zoneIndex`, `title`, `workingDir`, and alive-state
//! (`state` + `closeReason` + `confirmed`).
//!
//! ## Invariants
//!
//! - **Append-only + retained.** Records are never grace-pruned and never
//!   destructively mutated; the history outlives the registry's
//!   restore-eligibility window and
//!   [`SessionLifecycleStore::prune`](crate::session::session_lifecycle_store::SessionLifecycleStore::prune).
//!   Growth is bounded ONLY by [`Self::maybe_compact_locked`]'s ring/time-window
//!   compaction, which keeps everything within the last
//!   [`RETENTION_WINDOW_MS`] OR the last [`MIN_KEEP_RECORDS`] records —
//!   whichever set is larger — and by construction NEVER drops the most
//!   recent snapshot.
//! - **NOT a second restore driver.** The restore path (boot restore,
//!   `restorable_records`, the `terminal_session_*` commands) MUST NOT read
//!   this file — it is surfaced only as the manual/assistant fallback when
//!   auto-restore yields nothing. This module deliberately exposes no
//!   snapshot-read API; do not add one for restore code.
//! - **Derived, not competing.** The registry stays the single mutable
//!   store; this history is a write-only projection of it (no split-brain).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::session::session_lifecycle_store::TerminalSessionRecord;

/// Snapshot appends triggered by a registry change within this interval of
/// the previous append still land immediately; HEARTBEAT appends are
/// suppressed until this much time has passed since the last append of any
/// kind. 5 minutes — a recent snapshot always predates any crash.
const HEARTBEAT_MIN_INTERVAL_MS: i64 = 300_000;
/// Time window of snapshot records the compaction always retains (14 days).
const RETENTION_WINDOW_MS: i64 = 14 * 86_400_000;
/// Minimum number of most-recent snapshot records the compaction always
/// retains, regardless of age. The union with the time window means the
/// history keeps "last 14 days or last N records, whichever is larger".
const MIN_KEEP_RECORDS: usize = 2048;
/// Compaction is considered only once the file holds more than this many
/// records (must exceed [`MIN_KEEP_RECORDS`] so a compaction can shrink the
/// file below the trigger when the excess records are old).
const COMPACT_TRIGGER_RECORDS: usize = 4096;
/// Once over the trigger, re-check compaction only every this many appends
/// (amortizes the full-file rewrite; ~every 21h at heartbeat cadence).
const COMPACT_CHECK_EVERY: usize = 256;

/// Snapshot reason: a meaningful registry change (open/close/move/rename).
pub const REASON_CHANGE: &str = "change";
/// Snapshot reason: the periodic freshness heartbeat.
pub const REASON_HEARTBEAT: &str = "heartbeat";

/// One session's full recovery tuple inside a snapshot record — exactly the
/// fields that were null/lost in the 2026-07-03 incident. Volatile
/// timestamps (`last_seen_at`) are deliberately excluded so identical
/// layouts hash identically for change-dedupe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSession {
    /// Stable provider session id (`claude --resume <id>`).
    pub claude_session_id: String,
    /// `CLAUDE_CONFIG_DIR` / account config dir — the field the incident
    /// reconstruction could not recover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// Working directory the terminal was opened in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// Which AI-CLI provider owns the session (`"claude"`, `"gemini"`).
    pub provider: String,
    /// Grid page the session's tile belongs to.
    pub page_id: String,
    /// Grid zone index (-1 = unassigned).
    pub zone_index: i32,
    /// Tab title at snapshot time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Alive-state: `"open"` | `"closed"`.
    pub state: String,
    /// Why the session closed, when `state == "closed"` (`"poll-dead"`,
    /// `"pty-exit"`, `"explicit"`, …) — lets recovery distinguish a crash
    /// casualty from a deliberate close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    /// Whether a provider hook confirmed a REAL session started here
    /// (`confirmed_at` set). Unconfirmed records may be phantom shells.
    pub confirmed: bool,
    /// Unix millis the session was first opened (stable across snapshots).
    pub opened_at: i64,
}

impl From<&TerminalSessionRecord> for SnapshotSession {
    fn from(rec: &TerminalSessionRecord) -> Self {
        SnapshotSession {
            claude_session_id: rec.claude_session_id.clone(),
            config_dir: rec.config_dir.clone(),
            working_dir: rec.working_dir.clone(),
            provider: rec.provider.clone(),
            page_id: rec.page_id.clone(),
            zone_index: rec.zone_index,
            title: rec.title.clone(),
            state: rec.state.clone(),
            close_reason: rec.close_reason.clone(),
            confirmed: rec.confirmed_at.is_some(),
            opened_at: rec.opened_at,
        }
    }
}

/// One complete snapshot of the full session set — one JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRecord {
    /// Unix epoch millis the snapshot was taken (machine-readable).
    pub ts: i64,
    /// RFC 3339 UTC rendering of `ts` (human-readable — the file is a
    /// manual-recovery artifact first).
    pub at: String,
    /// [`REASON_CHANGE`] or [`REASON_HEARTBEAT`].
    pub reason: String,
    /// The COMPLETE session set at `ts` (every registry record, open and
    /// recently-closed), sorted by (page, zone, id) for stable diffing.
    pub sessions: Vec<SnapshotSession>,
}

#[derive(Debug)]
struct Inner {
    /// Number of snapshot records (lines) currently in the file.
    line_count: usize,
    /// Content hash of the most recently appended snapshot's `sessions`
    /// (order-normalized) — consecutive identical change-snapshots dedupe.
    last_key: Option<u64>,
    /// `ts` of the most recent append of ANY reason — gates heartbeats.
    last_append_ms: i64,
    /// Appends since the last compaction check (amortization counter).
    appends_since_compact: usize,
}

/// Append-only JSONL writer for session-layout snapshot history. Cheap to
/// clone-share via `Arc`; all appends serialize through the internal lock.
///
/// Write-only by design — see the module docs: the restore path must never
/// read this file.
#[derive(Debug)]
pub struct SnapshotHistory {
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl SnapshotHistory {
    /// Open (or initialize) the history at `path`, creating parent dirs.
    /// Scans any existing file once to seed the record count and — from the
    /// last line — the dedupe key + heartbeat clock, so a restart neither
    /// duplicates the last snapshot on the first re-assert nor heartbeats
    /// immediately. Runs one compaction pass if the file is over the
    /// trigger.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut inner = Inner {
            line_count: 0,
            last_key: None,
            last_append_ms: 0,
            appends_since_compact: 0,
        };
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => {
                    let mut last_line: Option<&str> = None;
                    for line in contents.lines().filter(|l| !l.trim().is_empty()) {
                        inner.line_count += 1;
                        last_line = Some(line);
                    }
                    if let Some(line) = last_line {
                        if let Ok(rec) = serde_json::from_str::<SnapshotRecord>(line) {
                            inner.last_append_ms = rec.ts;
                            let mut sessions = rec.sessions;
                            sort_sessions(&mut sessions);
                            inner.last_key = Some(content_key(&sessions));
                        }
                    }
                }
                Err(e) => {
                    // Unreadable history: keep the file untouched (it may
                    // still be humanly recoverable) and start counting from
                    // zero — appends still work.
                    warn!(
                        error = %e,
                        path = %path.display(),
                        "snapshot_history: existing file unreadable — appending blind"
                    );
                }
            }
        }
        let history = Self {
            path,
            inner: Mutex::new(inner),
        };
        {
            let mut inner = history.inner.lock().expect("fresh mutex");
            history.maybe_compact_locked(&mut inner, Utc::now().timestamp_millis(), true);
        }
        Ok(history)
    }

    /// Record a CHANGE snapshot (a meaningful registry mutation: session
    /// open/close/move/rename/confirm). Appends immediately unless the
    /// snapshot content is identical to the previously appended one
    /// (order-insensitive) — re-asserts and no-op refreshes stay silent.
    pub fn record_change(&self, sessions: Vec<SnapshotSession>) {
        self.record_change_at(Utc::now().timestamp_millis(), sessions);
    }

    /// Record a HEARTBEAT snapshot: appended only when at least
    /// [`HEARTBEAT_MIN_INTERVAL_MS`] has passed since the last append of any
    /// reason. No content dedupe — an unchanged heartbeat is the freshness
    /// proof that the snapshot predates a crash.
    pub fn record_heartbeat(&self, sessions: Vec<SnapshotSession>) {
        self.record_heartbeat_at(Utc::now().timestamp_millis(), sessions);
    }

    fn record_change_at(&self, now_ms: i64, mut sessions: Vec<SnapshotSession>) {
        sort_sessions(&mut sessions);
        let key = content_key(&sessions);
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!(error = %e, "snapshot_history: lock poisoned on record_change");
                return;
            }
        };
        if inner.last_key == Some(key) {
            return; // identical layout — nothing meaningful changed
        }
        self.append_locked(&mut inner, now_ms, REASON_CHANGE, key, &sessions);
    }

    fn record_heartbeat_at(&self, now_ms: i64, mut sessions: Vec<SnapshotSession>) {
        sort_sessions(&mut sessions);
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!(error = %e, "snapshot_history: lock poisoned on record_heartbeat");
                return;
            }
        };
        if now_ms - inner.last_append_ms < HEARTBEAT_MIN_INTERVAL_MS {
            return; // a recent snapshot (change or heartbeat) already exists
        }
        let key = content_key(&sessions);
        self.append_locked(&mut inner, now_ms, REASON_HEARTBEAT, key, &sessions);
    }

    /// Append one snapshot line and update the in-memory bookkeeping.
    /// Best-effort: a write failure is logged, not propagated (the history
    /// is a safety net — it must never break the registry write path).
    fn append_locked(
        &self,
        inner: &mut Inner,
        now_ms: i64,
        reason: &str,
        key: u64,
        sessions: &[SnapshotSession],
    ) {
        let record = SnapshotRecord {
            ts: now_ms,
            at: rfc3339(now_ms),
            reason: reason.to_string(),
            sessions: sessions.to_vec(),
        };
        let line = match serde_json::to_string(&record) {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "snapshot_history: serialize failed — snapshot dropped");
                return;
            }
        };
        let write = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = write {
            warn!(
                error = %e,
                path = %self.path.display(),
                "snapshot_history: append failed — snapshot dropped"
            );
            return;
        }
        inner.line_count += 1;
        inner.last_key = Some(key);
        inner.last_append_ms = now_ms;
        inner.appends_since_compact += 1;
        self.maybe_compact_locked(inner, now_ms, false);
    }

    /// Ring/time-window compaction, amortized: only runs when the file holds
    /// more than [`COMPACT_TRIGGER_RECORDS`] records and (unless `force`)
    /// only every [`COMPACT_CHECK_EVERY`] appends.
    fn maybe_compact_locked(&self, inner: &mut Inner, now_ms: i64, force: bool) {
        if inner.line_count <= COMPACT_TRIGGER_RECORDS {
            return;
        }
        if !force && inner.appends_since_compact < COMPACT_CHECK_EVERY {
            return;
        }
        inner.appends_since_compact = 0;
        match compact_file(&self.path, now_ms, MIN_KEEP_RECORDS, RETENTION_WINDOW_MS) {
            Ok(kept) => inner.line_count = kept,
            Err(e) => warn!(
                error = %e,
                path = %self.path.display(),
                "snapshot_history: compaction failed — file left as-is"
            ),
        }
    }
}

/// Stable snapshot ordering: (page, zone, session id). Applied before
/// hashing so the change-dedupe is insensitive to registry map iteration
/// order, and so consecutive snapshot lines diff cleanly by eye.
fn sort_sessions(sessions: &mut [SnapshotSession]) {
    sessions.sort_by(|a, b| {
        (&a.page_id, a.zone_index, &a.claude_session_id).cmp(&(
            &b.page_id,
            b.zone_index,
            &b.claude_session_id,
        ))
    });
}

/// Order-normalized content hash of a snapshot's sessions (callers sort
/// first). Timestamps (`ts`/`at`) are not part of the key by construction.
fn content_key(sessions: &[SnapshotSession]) -> u64 {
    let serialized = serde_json::to_string(sessions).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    serialized.hash(&mut hasher);
    hasher.finish()
}

fn rfc3339(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// Rewrite the history keeping every record inside the retention window OR
/// inside the last-`min_keep` suffix — the union, so the kept set is "last
/// `retention_ms` or last `min_keep` records, whichever is larger". The most
/// recent record is always inside the suffix (`min_keep >= 1`), so a
/// compaction can NEVER drop the latest complete snapshot. A malformed line
/// outside the suffix is dropped (it is unusable junk); inside the suffix it
/// is kept verbatim (never destroy recent data on a parse hiccup). Atomic
/// via temp-file + rename. Returns the kept record count.
fn compact_file(
    path: &Path,
    now_ms: i64,
    min_keep: usize,
    retention_ms: i64,
) -> std::io::Result<usize> {
    let contents = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    let suffix_start = lines.len().saturating_sub(min_keep.max(1));
    let cutoff = now_ms - retention_ms;
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, line)| {
            *i >= suffix_start
                || serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v.get("ts").and_then(serde_json::Value::as_i64))
                    .is_some_and(|ts| ts >= cutoff)
        })
        .map(|(_, line)| *line)
        .collect();
    if kept.len() == lines.len() {
        return Ok(kept.len()); // nothing to drop — skip the rewrite
    }
    let tmp = path.with_extension("jsonl.tmp");
    let mut body = kept.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)?;
    Ok(kept.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sess(id: &str, zone: i32) -> SnapshotSession {
        SnapshotSession {
            claude_session_id: id.to_string(),
            config_dir: Some("C:/Users/op/.claude-paktis".to_string()),
            working_dir: Some("D:/repo".to_string()),
            provider: "claude".to_string(),
            page_id: "default".to_string(),
            zone_index: zone,
            title: Some(format!("Terminal {zone}")),
            state: "open".to_string(),
            close_reason: None,
            confirmed: true,
            opened_at: 1_000,
        }
    }

    fn read_records(path: &Path) -> Vec<SnapshotRecord> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("valid snapshot line"))
            .collect()
    }

    /// A change snapshot appends one JSONL line carrying the FULL recovery
    /// tuple per session, and the line round-trips through serde.
    #[test]
    fn change_appends_full_tuple_and_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session-snapshots.jsonl");
        let h = SnapshotHistory::open(&path).unwrap();

        let mut closed = sess("b-closed", 3);
        closed.state = "closed".to_string();
        closed.close_reason = Some("poll-dead".to_string());
        closed.confirmed = false;
        h.record_change_at(1_700_000_000_000, vec![sess("a-open", 1), closed.clone()]);

        let recs = read_records(&path);
        assert_eq!(recs.len(), 1);
        let rec = &recs[0];
        assert_eq!(rec.reason, REASON_CHANGE);
        assert_eq!(rec.ts, 1_700_000_000_000);
        assert!(rec.at.starts_with("2023-"), "human-readable timestamp");
        // Sorted by (page, zone, id): zone 1 first.
        assert_eq!(rec.sessions[0].claude_session_id, "a-open");
        assert_eq!(rec.sessions[1], closed, "full tuple round-trips");
        // The raw line is camelCase (matches the registry's on-disk shape).
        let raw = std::fs::read_to_string(&path).unwrap();
        for field in [
            "claudeSessionId",
            "configDir",
            "workingDir",
            "provider",
            "pageId",
            "zoneIndex",
            "title",
            "state",
            "closeReason",
            "confirmed",
            "openedAt",
        ] {
            assert!(raw.contains(field), "line carries {field}");
        }
    }

    /// Identical consecutive change snapshots dedupe (re-asserts stay
    /// silent) — including across a reopen, and insensitive to session
    /// order — while a real content change appends.
    #[test]
    fn change_dedupes_identical_content_including_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session-snapshots.jsonl");
        let h = SnapshotHistory::open(&path).unwrap();

        h.record_change_at(1_000, vec![sess("a", 1), sess("b", 2)]);
        h.record_change_at(2_000, vec![sess("a", 1), sess("b", 2)]);
        h.record_change_at(3_000, vec![sess("b", 2), sess("a", 1)]); // reordered
        assert_eq!(read_records(&path).len(), 1, "identical layouts dedupe");

        h.record_change_at(4_000, vec![sess("a", 5), sess("b", 2)]); // zone move
        assert_eq!(read_records(&path).len(), 2, "a real move appends");

        // Reopen seeds the dedupe key from the last line: the boot
        // re-assert of the same layout must not duplicate it.
        drop(h);
        let h = SnapshotHistory::open(&path).unwrap();
        h.record_change_at(5_000, vec![sess("b", 2), sess("a", 5)]);
        assert_eq!(read_records(&path).len(), 2, "no duplicate after reopen");
    }

    /// Heartbeats are gated on time since the LAST append (any reason), and
    /// unlike changes they append even when the content is unchanged.
    #[test]
    fn heartbeat_respects_interval_and_skips_content_dedupe() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session-snapshots.jsonl");
        let h = SnapshotHistory::open(&path).unwrap();
        let t0 = 1_700_000_000_000;

        h.record_change_at(t0, vec![sess("a", 1)]);
        h.record_heartbeat_at(t0 + 45_000, vec![sess("a", 1)]);
        assert_eq!(read_records(&path).len(), 1, "too soon — suppressed");

        h.record_heartbeat_at(t0 + HEARTBEAT_MIN_INTERVAL_MS, vec![sess("a", 1)]);
        let recs = read_records(&path);
        assert_eq!(recs.len(), 2, "unchanged content still heartbeats");
        assert_eq!(recs[1].reason, REASON_HEARTBEAT);

        // Empty set is an honest heartbeat too (registry genuinely empty).
        h.record_heartbeat_at(t0 + 2 * HEARTBEAT_MIN_INTERVAL_MS, vec![]);
        let recs = read_records(&path);
        assert_eq!(recs.len(), 3);
        assert!(recs[2].sessions.is_empty());
    }

    /// Compaction keeps the union of the retention window and the
    /// last-`min_keep` suffix, never drops the most recent record, and
    /// drops malformed junk outside the suffix.
    #[test]
    fn compaction_keeps_window_union_suffix_and_never_drops_latest() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session-snapshots.jsonl");
        let h = SnapshotHistory::open(&path).unwrap();

        let now = 1_700_000_000_000;
        let retention = 10_000i64;
        // 4 old records (outside window), 1 fresh, plus a garbage line first.
        std::fs::write(&path, "{not json\n").unwrap();
        for (i, ts) in [
            now - 50_000,
            now - 40_000,
            now - 30_000,
            now - 20_000,
            now - 1_000,
        ]
        .into_iter()
        .enumerate()
        {
            h.record_change_at(ts, vec![sess(&format!("s{i}"), i as i32)]);
        }
        // 6 lines on disk (1 garbage + 5 records). min_keep=2 → suffix is the
        // last 2 records; window (10s) admits only the fresh one.
        let kept = compact_file(&path, now, 2, retention).unwrap();
        assert_eq!(kept, 2, "suffix of 2 wins over 1-record window");
        let recs = read_records(&path);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[1].sessions[0].claude_session_id, "s4");
        assert_eq!(recs[1].ts, now - 1_000, "most recent snapshot survives");

        // Window larger than the suffix: everything inside stays.
        let kept = compact_file(&path, now, 1, 60_000).unwrap();
        assert_eq!(kept, 2, "window keeps both — nothing dropped");

        // Even when EVERYTHING is out-of-window, min_keep floors at the
        // latest record.
        let kept = compact_file(&path, now + 1_000_000, 1, retention).unwrap();
        assert_eq!(kept, 1);
        assert_eq!(
            read_records(&path)[0].ts,
            now - 1_000,
            "the most recent complete snapshot is NEVER dropped"
        );
    }

    /// `open` counts existing records and seeds the heartbeat clock from the
    /// last line so a restart doesn't immediately heartbeat.
    #[test]
    fn open_seeds_heartbeat_clock_from_last_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session-snapshots.jsonl");
        let t0 = 1_700_000_000_000;
        {
            let h = SnapshotHistory::open(&path).unwrap();
            h.record_change_at(t0, vec![sess("a", 1)]);
        }
        let h = SnapshotHistory::open(&path).unwrap();
        h.record_heartbeat_at(t0 + 45_000, vec![sess("a", 1)]);
        assert_eq!(
            read_records(&path).len(),
            1,
            "heartbeat gated by the persisted last-append time"
        );
        h.record_heartbeat_at(t0 + HEARTBEAT_MIN_INTERVAL_MS, vec![sess("a", 1)]);
        assert_eq!(read_records(&path).len(), 2);
    }

    /// The `From<&TerminalSessionRecord>` projection carries every
    /// recovery-tuple field and maps `confirmed_at` to the bool.
    #[test]
    fn snapshot_session_projects_registry_record() {
        let rec = TerminalSessionRecord {
            claude_session_id: "sess-1".to_string(),
            config_dir: Some("C:/cfg".to_string()),
            working_dir: Some("D:/repo".to_string()),
            page_id: "page-2".to_string(),
            zone_index: 4,
            title: Some("Fix build".to_string()),
            terminal_id: "term-1".to_string(),
            opened_at: 123,
            last_seen_at: 456,
            state: "closed".to_string(),
            closed_at: Some(789),
            close_reason: Some("pty-exit".to_string()),
            provider: "claude".to_string(),
            origin: Some("authoritative".to_string()),
            restore_pending_at: None,
            confirmed_at: Some(500),
        };
        let s = SnapshotSession::from(&rec);
        assert_eq!(s.claude_session_id, "sess-1");
        assert_eq!(s.config_dir.as_deref(), Some("C:/cfg"));
        assert_eq!(s.working_dir.as_deref(), Some("D:/repo"));
        assert_eq!(s.provider, "claude");
        assert_eq!(s.page_id, "page-2");
        assert_eq!(s.zone_index, 4);
        assert_eq!(s.title.as_deref(), Some("Fix build"));
        assert_eq!(s.state, "closed");
        assert_eq!(s.close_reason.as_deref(), Some("pty-exit"));
        assert!(s.confirmed);
        assert_eq!(s.opened_at, 123);
    }
}
