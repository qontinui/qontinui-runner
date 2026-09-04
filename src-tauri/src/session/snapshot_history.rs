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
//! one JSON line per snapshot — to the INSTANCE-scoped path resolved by
//! [`crate::session::session_lifecycle_store::snapshot_history_path`]
//! (primary: `~/.qontinui/runner/session-restore/session-snapshots.jsonl`;
//! secondary: `…/instance-<name>/session-restore/session-snapshots.jsonl`),
//! in the runner's own app-data, so recovery works
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
//!
//! ## Restorability is stamped at WRITE time, never inferred at read time
//!
//! A recovery tuple naming a session id that has no transcript on disk is not
//! recoverable at all — the 2026-07-08 phantom-id incident recorded exactly
//! that (`confirmed:false` + an id no `--resume` could ever load). `confirmed`
//! alone did not reveal it, because the registry's own confirmation flag is a
//! statement about a hook, not about the disk. So each entry additionally
//! carries [`SnapshotSession::transcript_exists`] and
//! [`SnapshotSession::restorable`] — a stat of the expected `*.jsonl` taken at
//! the instant the snapshot is written, when the file is still there to stat.
//!
//! This is a **self-description, not a read API**: the fields exist so a human
//! (or an assistant) reading a months-old line can tell a recoverable session
//! from a phantom WITHOUT re-deriving disk state that has since changed. The
//! module invariant above still holds — restore code must not read this file.
//! The probe is optional ([`TranscriptProbe`]); unattached, both fields are
//! omitted from the JSON entirely (`null` ⇒ "not probed", never "false").

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

/// Existence probe for a session's on-disk transcript — the evidence that makes
/// a recorded id actually `--resume`-able.
///
/// Injected (rather than called directly) so the store can stamp
/// [`SnapshotSession::transcript_exists`] without this module taking a
/// dependency on the disk layout, and so tests are deterministic.
/// [`crate::session::reconcile::DiskTranscriptIndex`] is the real
/// implementation. Read at write time (the snapshot stamp) and at list time
/// (`SessionLifecycleStore::probe_transcript_exists`, which feeds the
/// boot-restore tier).
pub trait TranscriptProbe: std::fmt::Debug + Send + Sync {
    /// Whether a transcript file exists for `session_id`, scoped by
    /// `working_dir` — the transcript's project path, from which the file path is
    /// DERIVED.
    ///
    /// The `bool` return cannot express "could not determine", and the disk
    /// implementation answers a bare `false` for a missing/blank `working_dir`.
    /// Callers that GATE behavior on the answer must therefore not pass an
    /// unusable `working_dir` — `SessionLifecycleStore::probe_transcript_exists`
    /// is the guarded entry point that maps those cases to UNKNOWN. Widening this
    /// return to `Option<bool>` so the distinction lives in one place is a
    /// worthwhile follow-up.
    fn transcript_exists(&self, session_id: &str, working_dir: Option<&str>) -> bool;
}

/// Is this recorded identity actually resumable? Both gates are required and
/// each rules out a real, observed failure mode:
///
/// - **`transcript_exists`** — the 2026-07-08 phantom class: a provisional id
///   pinned at spawn for a shell that never ran a provider. `--resume` on it
///   cannot work; nothing on disk backs it.
/// - **`confirmed`** — the unproven-bind class: an id the runner has evidence
///   FOR but no proof OF (a typed `--session-id` whose transcript hasn't landed
///   yet, or a degraded `reconciled` mtime guess that may name a FOREIGN
///   session in the same cwd). Resuming that would hijack someone else's
///   session, so the grade is quarantined until confirmation.
///
/// NOTE this is *identity* restorability — "this id can be `--resume`d" — and
/// is deliberately distinct from
/// [`SessionLifecycleStore::restorable_records`](crate::session::session_lifecycle_store::SessionLifecycleStore::restorable_records),
/// which answers the different question of whether a record is still within the
/// boot-restore grace window. A session can be identity-restorable but out of
/// grace, or in grace but a phantom.
pub fn is_restorable_identity(confirmed: bool, transcript_exists: bool) -> bool {
    confirmed && transcript_exists
}

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
    /// Whether a transcript for [`Self::claude_session_id`] existed on disk at
    /// SNAPSHOT time — stamped from the injected [`TranscriptProbe`]. `None`
    /// (field omitted) means "not probed" (no probe attached), which is
    /// deliberately distinct from `Some(false)` = "probed, and the id is a
    /// phantom".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_exists: Option<bool>,
    /// Whether this entry's id was actually resumable at snapshot time —
    /// [`is_restorable_identity`] of `confirmed` + `transcript_exists`. `None`
    /// when unprobed. This is the field the phantom-id incident lacked: it
    /// distinguishes a recovery tuple worth typing `--resume` for from one that
    /// never could have worked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restorable: Option<bool>,
    /// Unix millis the session was first opened (stable across snapshots).
    pub opened_at: i64,
}

impl SnapshotSession {
    /// Project a registry record into a snapshot entry, stamping the
    /// restorability tuple from `probe` when one is attached.
    ///
    /// The stat happens HERE, at write time, because that is the only moment the
    /// answer is knowable: the history outlives the registry, the grace window,
    /// and often the transcript itself, so a reader months later cannot re-derive
    /// it. With `probe: None` both fields stay `None` — the store works
    /// standalone (tests, ephemeral fallbacks) and an unprobed entry never
    /// masquerades as a proven-phantom one.
    pub fn from_record(rec: &TerminalSessionRecord, probe: Option<&dyn TranscriptProbe>) -> Self {
        let confirmed = rec.confirmed_at.is_some();
        let transcript_exists =
            probe.map(|p| p.transcript_exists(&rec.claude_session_id, rec.working_dir.as_deref()));
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
            confirmed,
            transcript_exists,
            restorable: transcript_exists.map(|e| is_restorable_identity(confirmed, e)),
            opened_at: rec.opened_at,
        }
    }
}

/// Unprobed projection — equivalent to [`SnapshotSession::from_record`] with no
/// probe. Retained so callers that have no probe to offer stay unchanged.
impl From<&TerminalSessionRecord> for SnapshotSession {
    fn from(rec: &TerminalSessionRecord) -> Self {
        SnapshotSession::from_record(rec, None)
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

// ── Display-only reader (previous-sessions listing) ──────────────────────────
//
// The module invariant (docs above) is that the RESTORE path must never read
// this file. The reader below is DISPLAY-ONLY: it powers the "previous
// sessions" listing, which needs ids OLDER than the mutable registry's ~24 h
// retention (the registry prunes closed rows; this append-only history keeps
// them for 14 days). It MUST NOT be wired into `restorable_records`,
// `terminal_session_list_open`, or reconcile — doing so would resurrect the
// split-brain the write-only invariant exists to prevent.
//
// **The port-keyed `snapshot_path_for_<port>` reader helper is GONE, and no
// reader may invent another.** (Its exact former name is spelled out only in
// `session_lifecycle_store.rs`'s
// `readers_must_not_re_derive_the_snapshot_history_path`, which greps the tree
// for it — naming it here in prose would make that guard flag this comment.)
// It resolved `session-snapshots[-<port>].jsonl` under the deliberately
// UNSCOPED `claude_hook::session_restore_dir()`, while the writer
// (`session_lifecycle_store::snapshot_history_path`) had already moved to an
// instance-scoped dir with a plain filename. Those coincide for the primary on
// 9876 and are a different DIRECTORY *and* a different FILENAME for every
// secondary — so `terminal_session_list_history` and `GET /sessions/history`
// read a file nothing writes, and on a recycled temp-runner port they read a
// PRIOR temp's history. Every reader now resolves through
// [`crate::session::session_lifecycle_store::snapshot_history_path`], the
// single write-side source of truth.

/// **Display-only** reader: the LATEST [`SnapshotSession`] per
/// `claude_session_id` across the whole history (a session appears in many
/// snapshots; the one from the newest [`SnapshotRecord`] wins). Powers the
/// "previous sessions" listing so ids older than the registry's retention still
/// surface.
///
/// MUST NOT be called from any restore path — see the section comment above and
/// the module invariant. Fail-open by construction: a missing/empty file → an
/// empty vec (no error), and a malformed line is skipped, never fatal.
pub fn read_all_snapshot_sessions(path: &Path) -> Vec<SnapshotSession> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(), // missing / unreadable — fail open
    };
    // Track the newest record `ts` seen per session id so a later snapshot
    // overwrites an earlier one.
    let mut latest: std::collections::HashMap<String, (i64, SnapshotSession)> =
        std::collections::HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: SnapshotRecord = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue, // skip a corrupt line — do not abort
        };
        for s in rec.sessions {
            let is_newer = latest
                .get(&s.claude_session_id)
                .map(|(ts, _)| rec.ts >= *ts)
                .unwrap_or(true);
            if is_newer {
                latest.insert(s.claude_session_id.clone(), (rec.ts, s));
            }
        }
    }
    latest.into_values().map(|(_, s)| s).collect()
}

// ── Terminal tree-reset reports (P0 tree-reset observability) ────────────────
//
// A remount of the terminal page tree (top-level state flip → restore respawns
// `claude --resume`) used to be visible only as a webview console.warn that
// nothing captured. The `terminal_report_tree_reset` command appends one durable
// row per mount here. Kept in its OWN JSONL file (never spliced into the
// session-snapshots file above): a tree-reset row is not a layout snapshot, and
// writing it through `SnapshotHistory` would disturb that file's change-dedupe
// key and heartbeat clock — a behavior change this observability-only surface
// must not make.

/// Frontend-collected payload of one terminal-tree reset report. Every field
/// is best-effort at the reporting site, so all but the mount counter are
/// optional/defaulted — a partial report is still worth a row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeResetReport {
    /// 1-based mount counter, MODULE-scoped in the frontend so it survives
    /// fiber destruction and resets only on a real document load. Mount #1 (a
    /// normal boot) is recorded too — consumers filter on this to isolate
    /// genuine REmounts (`> 1`). Dev-only StrictMode double-mounts write a
    /// spurious `2` on boot; exe-mode runners don't run StrictMode, so a
    /// `> 1` row in prod is a tree reset.
    pub mount_number: u32,
    /// `performance.timeOrigin` of the reporting document — rows sharing a
    /// value provably belong to ONE webview document, so a second row with
    /// the same `time_origin` is a tree reset by construction (the
    /// remount-vs-fresh-load discriminator `navigation_type` alone cannot
    /// provide: it is document-scoped and never changes on an in-app remount).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_origin: Option<f64>,
    /// `Date.now()` captured in the mount effect itself, BEFORE the awaited
    /// open-record fetch — orders the row against restore logs even when the
    /// server stamp (`ts`) lands hundreds of ms later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ts: Option<i64>,
    /// `authStatus?.authenticated` at reset time (`None` = auth state unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
    /// `PerformanceNavigationTiming.type` (`"navigate"` / `"reload"` / …) —
    /// the reload-vs-in-app-remount discriminator, the row's key field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation_type: Option<String>,
    /// The terminal page ids about to re-initialize.
    #[serde(default)]
    pub page_ids: Vec<String>,
    /// How many open records the restore is about to consider
    /// (`terminal_session_list_open` count at reset time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_record_count: Option<u32>,
}

/// One durable tree-reset row — timestamp stamps plus the report payload,
/// one JSON line per row (same shape conventions as [`SnapshotRecord`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeResetRecord {
    /// Unix epoch millis the report was recorded.
    pub ts: i64,
    /// RFC 3339 UTC rendering of `ts` (the file is a human-read artifact).
    pub at: String,
    #[serde(flatten)]
    pub report: TreeResetReport,
}

/// Resolve the tree-reset history path for a runner lifecycle `port`:
/// `tree-resets.jsonl` for the primary 9876, else `tree-resets-<port>.jsonl`,
/// under the unscoped session-restore app-data dir.
///
/// Deliberately still PORT-keyed, and deliberately NOT migrated alongside the
/// session-snapshots path. Tree-resets are SYMMETRIC: the write side
/// (`commands/terminal.rs:terminal_report_tree_reset`) and the read side
/// (`mcp/sessions.rs:list_tree_resets`) both resolve through this one helper,
/// so the two ends agree by construction. The session-snapshots defect was a
/// read/write DIVERGENCE, not the port-keying itself — unifying only one end
/// here would introduce exactly the bug that was removed there. A row records
/// that a remount happened, never what to restore, so a recycled port at worst
/// mingles two runners' observability rows.
pub fn tree_reset_path_for_port(port: u16) -> PathBuf {
    let file = if port == 9876 {
        "tree-resets.jsonl".to_string()
    } else {
        format!("tree-resets-{}.jsonl", port)
    };
    crate::session::claude_hook::session_restore_dir().join(file)
}

/// Filters for [`read_tree_resets`]. All optional; `Default` reads everything.
#[derive(Debug, Default, Clone)]
pub struct TreeResetQuery {
    /// Keep only rows whose server stamp `ts` is >= this epoch-millis bound.
    pub since_ms: Option<i64>,
    /// Keep only rows whose `mount_number` is >= this. Pass `Some(2)` for the
    /// genuine-REmount filter the [`TreeResetReport::mount_number`] doc
    /// describes (mount #1 is a normal boot, recorded but not a reset).
    pub min_mount_number: Option<u32>,
    /// Keep at most this many rows, retaining the NEWEST ones (the file is
    /// append-only, so this is a tail).
    pub limit: Option<usize>,
}

/// Reader for the tree-reset history written by [`record_tree_reset`].
///
/// The P0 observability artifact was write-only until this existed: rows landed
/// in `tree-resets.jsonl` and the only way to reach them was to locate the file
/// on the runner host by hand. The plan that introduced the writer
/// (`2026-07-23-runner-restore-duplication-and-auth-flap-fixes`, P0) states the
/// rows are "the only way to *verify* P1/P2 rather than assume them", so they
/// need a read path — see `GET /sessions/tree-resets`.
///
/// Rows come back in file order (chronological, oldest first) after filtering.
/// Fail-open exactly like [`read_all_snapshot_sessions`]: a missing or
/// unreadable file yields an empty vec, and a corrupt line is skipped rather
/// than aborting the read — an observability surface must never be the thing
/// that fails.
///
/// OBSERVABILITY-ONLY, and it does NOT weaken the module invariant above: that
/// invariant bans a read API over the SESSION-SNAPSHOTS file for restore code,
/// and this reads the separate tree-reset file. Like
/// [`read_all_snapshot_sessions`], it must never be called from a restore path
/// — a tree-reset row describes that a remount happened, never what to restore.
pub fn read_tree_resets(path: &Path, q: &TreeResetQuery) -> Vec<TreeResetRecord> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(), // missing / unreadable — fail open
    };
    let mut rows: Vec<TreeResetRecord> = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: TreeResetRecord = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue, // skip a corrupt line — do not abort
        };
        if q.since_ms.is_some_and(|since| rec.ts < since) {
            continue;
        }
        if q.min_mount_number
            .is_some_and(|min| rec.report.mount_number < min)
        {
            continue;
        }
        rows.push(rec);
    }
    // Tail to `limit`, keeping the newest rows but preserving chronological
    // order within the returned window.
    if let Some(limit) = q.limit {
        let len = rows.len();
        if len > limit {
            rows.drain(..len - limit);
        }
    }
    rows
}

/// Append one tree-reset row to `path`, stamping the timestamps now.
/// Best-effort like every append in this module: a failure is logged, never
/// propagated — the report must never affect the initialization flow.
pub fn record_tree_reset(path: &Path, report: TreeResetReport) {
    record_tree_reset_at(path, Utc::now().timestamp_millis(), report);
}

fn record_tree_reset_at(path: &Path, now_ms: i64, report: TreeResetReport) {
    let record = TreeResetRecord {
        ts: now_ms,
        at: rfc3339(now_ms),
        report,
    };
    let line = match serde_json::to_string(&record) {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, "tree_reset: serialize failed — report dropped");
            return;
        }
    };
    let write = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        // Single write_all syscall (not writeln!'s two) — O_APPEND makes the
        // one syscall atomic, so concurrent reports during a remount storm
        // cannot interleave into torn rows in the very file that must stay
        // trustworthy during that storm.
        f.write_all(format!("{line}\n").as_bytes())
    })();
    if let Err(e) = write {
        warn!(
            error = %e,
            path = %path.display(),
            "tree_reset: append failed — report dropped"
        );
    }
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
            transcript_exists: None,
            restorable: None,
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
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            finish_reason: None,
            finish_synced: false,
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
        // Unprobed ⇒ the restorability fields are ABSENT, not `false`.
        assert_eq!(s.transcript_exists, None);
        assert_eq!(s.restorable, None);
        let wire = serde_json::to_value(&s).unwrap();
        assert!(
            wire.get("transcriptExists").is_none() && wire.get("restorable").is_none(),
            "an unprobed entry must not claim a phantom verdict it never checked"
        );
    }

    /// A probe stub whose answer is fixed per session id.
    #[derive(Debug)]
    struct FakeProbe(std::collections::HashSet<String>);
    impl TranscriptProbe for FakeProbe {
        fn transcript_exists(&self, session_id: &str, _wd: Option<&str>) -> bool {
            self.0.contains(session_id)
        }
    }

    fn probe_rec(id: &str, confirmed: bool) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: Some("C:/cfg".to_string()),
            working_dir: Some("D:/repo".to_string()),
            page_id: "p".to_string(),
            zone_index: 0,
            title: Some("t".to_string()),
            terminal_id: "term-1".to_string(),
            opened_at: 1,
            last_seen_at: 2,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: "claude".to_string(),
            origin: Some("authoritative".to_string()),
            restore_pending_at: None,
            confirmed_at: confirmed.then_some(500),
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            finish_reason: None,
            finish_synced: false,
        }
    }

    /// B5: the write-time stamp separates a genuinely recoverable entry from
    /// the 2026-07-08 phantom — which `confirmed` alone could not do.
    #[test]
    fn from_record_stamps_restorability_from_the_probe() {
        let probe = FakeProbe(["real".to_string()].into_iter().collect());

        // Healthy: confirmed + a transcript on disk ⇒ restorable.
        let healthy = SnapshotSession::from_record(&probe_rec("real", true), Some(&probe));
        assert_eq!(healthy.transcript_exists, Some(true));
        assert_eq!(healthy.restorable, Some(true));

        // THE INCIDENT: confirmed:false AND no transcript ⇒ phantom, and the
        // line now SAYS so.
        let phantom = SnapshotSession::from_record(&probe_rec("0b32d739", false), Some(&probe));
        assert_eq!(phantom.transcript_exists, Some(false));
        assert_eq!(phantom.restorable, Some(false));

        // Confirmed but the transcript is gone (pruned/aged) — a hook once
        // fired, yet `--resume` would find nothing. Not restorable.
        let no_transcript =
            SnapshotSession::from_record(&probe_rec("vanished", true), Some(&probe));
        assert_eq!(no_transcript.restorable, Some(false));

        // Transcript exists but the bind was never confirmed — an unproven
        // guess that may name a FOREIGN session. Quarantined, not restorable.
        let unconfirmed = SnapshotSession::from_record(&probe_rec("real", false), Some(&probe));
        assert_eq!(unconfirmed.transcript_exists, Some(true));
        assert_eq!(unconfirmed.restorable, Some(false));

        // Both gates are required, and the wire is camelCase.
        let wire = serde_json::to_value(&healthy).unwrap();
        assert_eq!(
            wire.get("transcriptExists").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(wire.get("restorable").and_then(|v| v.as_bool()), Some(true));
    }

    // ── Display-only reader tests (previous-sessions listing) ────────────────

    /// The reader dedupes by id, keeping the entry from the NEWEST record.
    #[test]
    fn read_all_dedupes_newest_per_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session-snapshots.jsonl");
        let h = SnapshotHistory::open(&path).unwrap();

        // Two records: `a` moves zone 1 → 5 between them; `b` only in the first.
        h.record_change_at(1_000, vec![sess("a", 1), sess("b", 2)]);
        h.record_change_at(2_000, vec![sess("a", 5)]);

        let mut out = read_all_snapshot_sessions(&path);
        out.sort_by(|x, y| x.claude_session_id.cmp(&y.claude_session_id));
        assert_eq!(out.len(), 2, "one entry per distinct id");
        let a = out.iter().find(|s| s.claude_session_id == "a").unwrap();
        assert_eq!(a.zone_index, 5, "newest record wins for a");
        assert!(
            out.iter().any(|s| s.claude_session_id == "b"),
            "an id present only in an older record still surfaces"
        );
    }

    /// A corrupt line is skipped; the good records around it still return.
    #[test]
    fn read_all_fails_open_on_corrupt_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session-snapshots.jsonl");
        let h = SnapshotHistory::open(&path).unwrap();
        h.record_change_at(1_000, vec![sess("good1", 1)]);
        h.record_change_at(2_000, vec![sess("good2", 2)]);
        // Splice a garbage line into the middle of the file.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = raw.lines().collect();
        lines.insert(1, "{ this is not json");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let out = read_all_snapshot_sessions(&path);
        let ids: std::collections::HashSet<_> =
            out.iter().map(|s| s.claude_session_id.as_str()).collect();
        assert!(ids.contains("good1") && ids.contains("good2"));
    }

    /// A missing file returns an empty vec (no error).
    #[test]
    fn read_all_missing_file_is_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        assert!(read_all_snapshot_sessions(&path).is_empty());
    }

    #[test]
    fn is_restorable_identity_requires_both_gates() {
        assert!(is_restorable_identity(true, true));
        assert!(!is_restorable_identity(true, false));
        assert!(!is_restorable_identity(false, true));
        assert!(!is_restorable_identity(false, false));
    }

    // ── Tree-reset report rows (P0 tree-reset observability) ─────────────────

    /// Each report appends one camelCase JSONL row (creating parent dirs)
    /// that round-trips through serde with the flattened payload intact.
    #[test]
    fn tree_reset_appends_one_row_that_round_trips() {
        let dir = tempdir().unwrap();
        // Parent dir does not exist yet — the append must create it.
        let path = dir.path().join("session-restore").join("tree-resets.jsonl");
        let report = TreeResetReport {
            mount_number: 2,
            authenticated: Some(true),
            navigation_type: Some("navigate".to_string()),
            page_ids: vec!["default".to_string(), "page-2".to_string()],
            open_record_count: Some(7),
            time_origin: Some(1_699_999_990_000.5),
            client_ts: Some(1_699_999_999_900),
        };
        record_tree_reset_at(&path, 1_700_000_000_000, report.clone());
        record_tree_reset_at(
            &path,
            1_700_000_001_000,
            TreeResetReport {
                mount_number: 3,
                ..report.clone()
            },
        );

        let raw = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2, "one row per report, append-only");
        let rec: TreeResetRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(rec.ts, 1_700_000_000_000);
        assert!(rec.at.starts_with("2023-"), "human-readable timestamp");
        assert_eq!(rec.report, report, "flattened payload round-trips");
        // Wire shape: camelCase, payload flattened onto the row (no nesting).
        for field in [
            "mountNumber",
            "authenticated",
            "navigationType",
            "pageIds",
            "openRecordCount",
            "timeOrigin",
            "clientTs",
        ] {
            assert!(lines[0].contains(field), "row carries {field}");
        }
        assert!(!lines[0].contains("report"), "payload is flattened");
    }

    /// Optional fields the frontend could not collect are OMITTED (not
    /// `null`/`false`), and a minimal report still parses back.
    #[test]
    fn tree_reset_omits_uncollected_optional_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tree-resets.jsonl");
        record_tree_reset_at(
            &path,
            1_700_000_000_000,
            TreeResetReport {
                mount_number: 1,
                authenticated: None,
                navigation_type: None,
                page_ids: vec![],
                open_record_count: None,
                time_origin: None,
                client_ts: None,
            },
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        for absent in [
            "authenticated",
            "navigationType",
            "openRecordCount",
            "timeOrigin",
            "clientTs",
        ] {
            assert!(!raw.contains(absent), "uncollected {absent} is omitted");
        }
        let rec: TreeResetRecord = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(rec.report.mount_number, 1);
        assert_eq!(rec.report.authenticated, None);
    }

    /// A write failure is swallowed (logged), never panics/propagates — the
    /// report must not be able to affect initialization.
    #[test]
    fn tree_reset_append_failure_is_swallowed() {
        let dir = tempdir().unwrap();
        // The target path's parent is a FILE, so create_dir_all/open must fail.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let path = blocker.join("tree-resets.jsonl");
        record_tree_reset_at(
            &path,
            1_700_000_000_000,
            TreeResetReport {
                mount_number: 1,
                authenticated: None,
                navigation_type: None,
                page_ids: vec![],
                open_record_count: None,
                time_origin: None,
                client_ts: None,
            },
        ); // must not panic
    }

    // ── Tree-reset reader (`GET /sessions/tree-resets`) ──────────────────────

    /// Helper: a report carrying just the fields the reader filters on.
    fn reset_report(mount_number: u32) -> TreeResetReport {
        TreeResetReport {
            mount_number,
            authenticated: Some(true),
            navigation_type: Some("navigate".to_string()),
            page_ids: vec!["default".to_string()],
            open_record_count: Some(3),
            time_origin: Some(1_699_999_990_000.0),
            client_ts: Some(1_699_999_999_900),
        }
    }

    /// Rows come back in file order (chronological) with the flattened payload
    /// intact — the round-trip the HTTP handler serves.
    #[test]
    fn read_tree_resets_returns_rows_chronologically() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tree-resets.jsonl");
        record_tree_reset_at(&path, 1_700_000_000_000, reset_report(1));
        record_tree_reset_at(&path, 1_700_000_001_000, reset_report(2));
        record_tree_reset_at(&path, 1_700_000_002_000, reset_report(3));

        let rows = read_tree_resets(&path, &TreeResetQuery::default());
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![1_700_000_000_000, 1_700_000_001_000, 1_700_000_002_000],
            "oldest first"
        );
        assert_eq!(rows[1].report, reset_report(2), "payload round-trips");
    }

    /// `min_mount_number: 2` isolates genuine REmounts — mount #1 is a normal
    /// boot, recorded but not a tree reset. This is the filter P2 verification
    /// runs (the count must stay flat across an auth flip).
    #[test]
    fn read_tree_resets_min_mount_number_isolates_remounts() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tree-resets.jsonl");
        record_tree_reset_at(&path, 1_700_000_000_000, reset_report(1));
        record_tree_reset_at(&path, 1_700_000_001_000, reset_report(2));
        record_tree_reset_at(&path, 1_700_000_002_000, reset_report(1));

        let rows = read_tree_resets(
            &path,
            &TreeResetQuery {
                min_mount_number: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(rows.len(), 1, "only the mount #2 row is a remount");
        assert_eq!(rows[0].report.mount_number, 2);
    }

    /// `since_ms` is inclusive on the server stamp; `limit` tails to the
    /// NEWEST rows while keeping them in chronological order.
    #[test]
    fn read_tree_resets_applies_since_and_limit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tree-resets.jsonl");
        for i in 0..5 {
            record_tree_reset_at(&path, 1_700_000_000_000 + i * 1_000, reset_report(1));
        }

        let since = read_tree_resets(
            &path,
            &TreeResetQuery {
                since_ms: Some(1_700_000_002_000),
                ..Default::default()
            },
        );
        assert_eq!(since.len(), 3, "since_ms is inclusive");
        assert_eq!(since[0].ts, 1_700_000_002_000);

        let tail = read_tree_resets(
            &path,
            &TreeResetQuery {
                limit: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(tail.len(), 2);
        assert_eq!(
            tail.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![1_700_000_003_000, 1_700_000_004_000],
            "limit keeps the newest rows, still oldest-first"
        );
    }

    /// Fail-open like `read_all_snapshot_sessions`: a corrupt line is skipped
    /// rather than aborting, and a missing file reads as empty — an
    /// observability surface must never be the thing that fails.
    #[test]
    fn read_tree_resets_fails_open_on_corrupt_line_and_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tree-resets.jsonl");
        record_tree_reset_at(&path, 1_700_000_000_000, reset_report(2));
        record_tree_reset_at(&path, 1_700_000_001_000, reset_report(3));
        // Corrupt the FIRST line; the second must still be returned.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = raw.lines().collect();
        lines[0] = "{not json";
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let rows = read_tree_resets(&path, &TreeResetQuery::default());
        assert_eq!(rows.len(), 1, "corrupt line skipped, not fatal");
        assert_eq!(rows[0].report.mount_number, 3);

        let missing = read_tree_resets(&dir.path().join("nope.jsonl"), &TreeResetQuery::default());
        assert!(missing.is_empty(), "missing file reads as empty");
    }
}
