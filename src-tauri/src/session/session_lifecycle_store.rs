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
/// reason (explicit user close), it is NOT restorable. 10 minutes in millis.
const RESTORABLE_PTY_EXIT_MS: i64 = 600_000;
/// A record closed with reason `"poll-dead"` (the liveness poll saw a live
/// shell with zero descendants for several consecutive idle ticks) is still
/// restorable for this long after the close. A `poll-dead` close is far less
/// certain than a `pty-exit` (the shell pty is, by definition, still alive —
/// the session was merely idle between tool calls), so an immediate restart
/// should bring it back rather than silently drop it. Beyond the window it is
/// NOT restorable (a genuinely abandoned session should not resurrect days
/// later). 10 minutes in millis.
const RESTORABLE_POLL_DEAD_MS: i64 = 600_000;
/// Anchored-recency grace for `state == "open"` rows in
/// [`SessionLifecycleStore::restorable_records`]. An open row is restorable
/// iff `anchor - last_seen_at <= grace`, where `anchor` is the registry's
/// LAST MOMENT OF LIFE (max of every row's `last_seen_at` / `closed_at`,
/// plus the prior shutdown marker's `at`). Recency relative to the anchor —
/// not wall-clock now — survives arbitrary downtime: a crash followed by an
/// hours-later boot still restores the rows that were fresh when the
/// previous process died, while a multi-day ghost row stays excluded on
/// EVERY boot kind. 10 minutes in millis (matches the close-grace windows;
/// live sessions are touched every ~45s by the poll, so anything on screen
/// at shutdown sits well inside it).
const RESTORABLE_OPEN_ANCHOR_GRACE_MS: i64 = 600_000;
/// Number of consecutive claude-absent ticks a *live* shell (`Some(true)` with
/// no Claude in its inclusive subtree) must accumulate before the poll closes
/// it `poll-dead`. A live Claude in the subtree is KeepAlive immediately and
/// never reaches this debounce. The debounce only governs the claude-absent
/// case: it absorbs a transient process-snapshot miss and gives an in-flight
/// relaunch a window, so a momentary blip doesn't drop a session. Requiring
/// several ticks (≈ N × 45s poll interval) only closes shells that are
/// claude-absent for an extended, unambiguous stretch (operator quit claude;
/// bare shell lingers). A `Some(false)` (pty actually dead) shell still closes
/// immediately — that path is unambiguous and bypasses this debounce entirely.
const LIVE_SHELL_DEAD_TICKS: u32 = 3;
/// Number of consecutive NO-MATCHING-TERMINAL ticks an open record must
/// accumulate before the poll closes it `"no-terminal"`. A record whose
/// terminal id (and fallback triple) matches nothing in THIS instance for
/// many consecutive ticks is not "uncertainty" — it is an orphan (e.g. a
/// stale ghost row inherited from a previous process) that would otherwise
/// stay `open` for up to 7 days and re-qualify for restore at every boot.
/// The close fires on tick N+1 (≈ 4 ticks × 45s ≈ 3 min), debounced so a
/// just-created record whose terminal is still registering never closes.
/// `"no-terminal"` is automatically NON-restorable: the restore grace match
/// only covers `"pty-exit"` / `"poll-dead"`.
const NO_TERMINAL_ORPHAN_TICKS: u32 = 3;

/// The provider every pre-provider-aware record is assumed to belong to. Used
/// as the `#[serde(default)]` for [`TerminalSessionRecord::provider`]: existing
/// on-disk records have no `provider` field and are all Claude today.
pub const DEFAULT_PROVIDER: &str = "claude";

/// `#[serde(default)]` source for [`TerminalSessionRecord::provider`].
fn default_provider() -> String {
    DEFAULT_PROVIDER.to_string()
}

/// Authoritative origin: the runner KNOWS the session id exactly (pre-pinned
/// `--session-id`, lifted from a typed flag, or a provider hook POSTed it).
pub const ORIGIN_AUTHORITATIVE: &str = "authoritative";
/// Reconciled origin: the id was recovered by a backstop and may name a foreign
/// session — restore treats it conservatively.
pub const ORIGIN_RECONCILED: &str = "reconciled";

/// Normalize a possibly-legacy `origin` value to the current vocabulary. Maps
/// the pre-migration `bind_origin` values (`"pinned"`→`"authoritative"`,
/// `"guessed"`→`"reconciled"`) and passes the new values through unchanged. Any
/// other string is left verbatim (forward-compat: a value this build doesn't
/// know is not silently rewritten). Returns `None` for `None`.
fn normalize_origin(origin: Option<String>) -> Option<String> {
    origin.map(|o| match o.as_str() {
        "pinned" => ORIGIN_AUTHORITATIVE.to_string(),
        "guessed" => ORIGIN_RECONCILED.to_string(),
        _ => o,
    })
}

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
    /// Which AI-CLI provider owns this session (`"claude"`, `"gemini"`, …).
    /// `#[serde(default)]` makes every existing record — written before the
    /// runner was provider-aware — deserialize as `"claude"` (see
    /// [`default_provider`]); they are all Claude today.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// How `claude_session_id` was bound:
    ///
    /// - `"authoritative"` — the runner KNOWS the id exactly (it pre-pinned
    ///   `--session-id`, lifted it from a typed `--resume`/`--session-id`, or
    ///   a provider hook POSTed it). Safe to auto-resume unattended.
    /// - `"reconciled"` — the id was recovered by a backstop (freshest-
    ///   transcript mtime / process-start-anchored reconcile) and may name a
    ///   foreign session. Restore treats it conservatively.
    ///
    /// Migration: on-disk records written by the previous schema carried this
    /// field as `bindOrigin` with the values `"pinned"`/`"guessed"`. The
    /// `alias = "bindOrigin"` reads the old JSON key, and [`load_map`]
    /// normalizes the legacy values (`pinned`→`authoritative`,
    /// `guessed`→`reconciled`) at load. Records predating the field entirely
    /// deserialize as `None`, which consumers read as `"reconciled"`.
    #[serde(default, alias = "bindOrigin", skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Unix millis when a boot-restore began re-typing this session's
    /// `claude --resume` (set via [`SessionLifecycleStore::mark_restore_pending`],
    /// cleared via [`SessionLifecycleStore::clear_restore_pending`] once the
    /// resume handshake is verified). While set, the liveness poll must NEVER
    /// flip the record `poll-dead` — a restore whose resume command silently
    /// failed to land needs its durable `open` state intact for the next
    /// attempt (operator retry or next boot), not destroyed mid-restore. The
    /// marker is backend-owned and durable so a frontend crash mid-restore
    /// can't lose it. Self-heals: the poll clears a stale marker the moment it
    /// observes the session confidently alive (KeepAlive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_pending_at: Option<i64>,
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
            entry.provider = rec.provider;
            // Unasserted origin (zone-move backstop / boot re-assert) must not
            // degrade an authoritative binding to reconciled — preserve on
            // None. The incoming value is normalized so a legacy caller still
            // passing "pinned"/"guessed" lands as authoritative/reconciled.
            if let Some(origin) = normalize_origin(rec.origin) {
                entry.origin = Some(origin);
            }
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

    /// Mark a present session as restore-pending (a boot-restore is about to
    /// type / has typed `claude --resume` and the handshake is not yet
    /// verified). While the marker is set the liveness poll skips the record
    /// entirely except for a confident-alive observation — see [`classify`].
    /// No-op (no write) if the session is absent.
    pub fn mark_restore_pending(&self, claude_session_id: &str) {
        let now = Utc::now().timestamp_millis();
        let snapshot = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on mark_restore_pending");
                    return;
                }
            };
            match m.get_mut(claude_session_id) {
                Some(rec) => rec.restore_pending_at = Some(now),
                None => return,
            }
            m.clone()
        };
        self.persist(&snapshot);
    }

    /// Clear a session's restore-pending marker (resume handshake verified —
    /// the session is live again). No-op (no write) if the session is absent
    /// or the marker is already clear.
    pub fn clear_restore_pending(&self, claude_session_id: &str) {
        let snapshot = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on clear_restore_pending");
                    return;
                }
            };
            match m.get_mut(claude_session_id) {
                Some(rec) if rec.restore_pending_at.is_some() => rec.restore_pending_at = None,
                _ => return, // absent or already clear — nothing to flush
            }
            m.clone()
        };
        self.persist(&snapshot);
    }

    /// Re-key a record from `old_id` to `new_id`: remove the entry stored under
    /// `old_id` and re-insert it under `new_id`, updating the record's own
    /// `claude_session_id` to match. Atomic under the same lock + persisted via
    /// the existing temp-file write.
    ///
    /// ## Why a re-key, not a field edit
    ///
    /// `claude_session_id` is the MAP KEY ([`record_open`] does
    /// `m.entry(rec.claude_session_id.clone())`, and the in-place refresh block
    /// only touches non-key fields). An adapter that reports a CORRECTED id can
    /// therefore not change the key by re-recording — it must re-key. This helper
    /// is the defensive primitive for that.
    ///
    /// No-op (no write) when: `old_id == new_id`; `old_id` is absent; or `new_id`
    /// is ALREADY present (re-keying onto a live row would clobber it — refuse
    /// rather than lose data). NOTE: no shipped adapter triggers this today (the
    /// audit refuted Claude id-rotation); it exists so a future adapter that does
    /// report a corrected id has a correct primitive and never leaves a
    /// permanently-stale row.
    pub fn rekey_session(&self, old_id: &str, new_id: &str) {
        if old_id == new_id {
            return;
        }
        let snapshot = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on rekey_session");
                    return;
                }
            };
            if m.contains_key(new_id) {
                warn!(
                    old_id,
                    new_id,
                    "session_lifecycle_store: rekey target already present — refusing to clobber"
                );
                return;
            }
            let Some(mut rec) = m.remove(old_id) else {
                return; // old id absent — nothing to re-key
            };
            rec.claude_session_id = new_id.to_string();
            m.insert(new_id.to_string(), rec);
            m.clone()
        };
        self.persist(&snapshot);
    }

    /// Clone of the open record currently hosted by `terminal_id`, if any.
    /// Terminal ids are fresh per PTY spawn, so at most one OPEN record can
    /// reference a given terminal at a time.
    pub fn find_open_by_terminal(&self, terminal_id: &str) -> Option<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => m
                .values()
                .find(|r| r.state == "open" && r.terminal_id == terminal_id)
                .cloned(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on find_open_by_terminal");
                None
            }
        }
    }

    /// Clone of the record for `claude_session_id`, open or closed.
    pub fn get(&self, claude_session_id: &str) -> Option<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => m.get(claude_session_id).cloned(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on get");
                None
            }
        }
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
    /// subset of records the restore path resurrects:
    ///
    /// - every `state == "open"` record that is RECENT relative to the
    ///   registry's anchor (see below) — a hard crash AND a clean shutdown
    ///   whose `pty-exit` close never flushed both leave fresh open rows,
    ///   while a stale ghost row (terminal long gone) carries an old
    ///   `last_seen_at` and is excluded on every boot kind, PLUS
    /// - every `state == "closed"` record whose `close_reason == "pty-exit"`
    ///   (its PTY died — the case a GRACEFUL restart produces by firing
    ///   `handleExit` on every live PTY) that closed within the
    ///   [`RESTORABLE_PTY_EXIT_MS`] grace window, PLUS
    /// - every `state == "closed"` record whose `close_reason == "poll-dead"`
    ///   (the liveness poll closed an idle-but-live shell) that closed within
    ///   the [`RESTORABLE_POLL_DEAD_MS`] grace window — a poll-dead close is
    ///   uncertain (the shell pty was still alive), so an immediate restart
    ///   should bring the session back rather than silently drop it.
    ///
    /// Records closed for any other reason (explicit user close, the poll's
    /// `"no-terminal"` orphan close), or `pty-exit` / `poll-dead` closes older
    /// than their grace windows, are excluded — a user who closes a tab does
    /// not want it resurrected, and a long-dead close is stale.
    ///
    /// ## Anchored recency (open rows)
    ///
    /// An open row is admitted iff
    /// `anchor - last_seen_at <= RESTORABLE_OPEN_ANCHOR_GRACE_MS`, where
    /// `anchor = max(every last_seen_at, every closed_at, prior_marker_at)` —
    /// the registry's last moment of life, NOT wall-clock now. A wall-clock
    /// rule (`now - last_seen <= grace`) would restore NOTHING after any
    /// downtime longer than the grace (crash, hours-later boot), while a
    /// clean-boot hard exclusion would silently lose a real on-screen session
    /// whose `pty-exit` close never flushed. `prior_marker_at` is the prior
    /// shutdown marker's `at` (shutdown instant on a clean exit, the crashed
    /// process's boot instant on a crash), captured ONCE at boot — see
    /// [`crate::session::shutdown_marker::boot_classification`].
    pub fn restorable_records(
        &self,
        now_ms: i64,
        prior_marker_at: Option<i64>,
    ) -> Vec<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => {
                // The registry's last moment of life across every row, plus
                // the prior shutdown marker.
                let anchor = m
                    .values()
                    .flat_map(|r| [Some(r.last_seen_at), r.closed_at])
                    .chain(std::iter::once(prior_marker_at))
                    .flatten()
                    .max();
                m.values()
                    .filter(|r| {
                        if r.state == "open" {
                            return match anchor {
                                Some(anchor) => {
                                    anchor - r.last_seen_at <= RESTORABLE_OPEN_ANCHOR_GRACE_MS
                                }
                                // Unreachable: an open row's own last_seen_at
                                // feeds the anchor. Admit defensively.
                                None => true,
                            };
                        }
                        if r.state == "closed" {
                            let grace = match r.close_reason.as_deref() {
                                Some("pty-exit") => Some(RESTORABLE_PTY_EXIT_MS),
                                Some("poll-dead") => Some(RESTORABLE_POLL_DEAD_MS),
                                _ => None,
                            };
                            if let Some(grace_ms) = grace {
                                return match r.closed_at {
                                    Some(closed_at) => now_ms - closed_at <= grace_ms,
                                    None => false,
                                };
                            }
                        }
                        false
                    })
                    .cloned()
                    .collect()
            }
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
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "session_lifecycle_store: read failed — starting empty");
            return HashMap::new();
        }
    };
    // Two-stage parse so ONE malformed record can't discard the whole store.
    // A real boot loads a registry that may hold a row written by an older /
    // newer schema or a partially-corrupted entry; the original
    // `from_slice::<HashMap<_, Record>>` failed the entire deserialize on the
    // first bad value, silently wiping every restorable on-screen session.
    // Instead: parse to a generic `HashMap<String, Value>` (only the top-level
    // object shape must be intact), then `from_value` each entry individually,
    // KEEPING the good rows and warn+dropping the bad ones.
    let raw: HashMap<String, serde_json::Value> = match serde_json::from_slice(&bytes) {
        Ok(raw) => raw,
        Err(e) => {
            // The file isn't even a JSON object map — unrecoverable; start empty.
            warn!(
                error = %e,
                path = %path.display(),
                "session_lifecycle_store: corrupt map file (not a JSON object) — starting empty"
            );
            return HashMap::new();
        }
    };

    let mut map = HashMap::with_capacity(raw.len());
    let mut dropped: Vec<(String, serde_json::Value)> = Vec::new();
    for (key, value) in raw {
        match serde_json::from_value::<TerminalSessionRecord>(value.clone()) {
            Ok(mut rec) => {
                // Migrate legacy `bind_origin` values to the current vocabulary
                // at load (`pinned`→`authoritative`, `guessed`→`reconciled`).
                // The field-name read is handled by `alias = "bindOrigin"`; this
                // maps the VALUES, which also changed.
                rec.origin = normalize_origin(rec.origin);
                map.insert(key, rec);
            }
            Err(e) => {
                warn!(
                    error = %e,
                    key = %key,
                    path = %path.display(),
                    "session_lifecycle_store: malformed record — dropped, keeping the rest"
                );
                dropped.push((key, value));
            }
        }
    }

    if !dropped.is_empty() {
        warn!(
            dropped = dropped.len(),
            kept = map.len(),
            path = %path.display(),
            "session_lifecycle_store: dropped malformed record(s); preserved the valid rows"
        );
        // Best-effort `.corrupt` side-car with the raw dropped entries so the
        // dropped data is inspectable after the fact. Never fails the load.
        let sidecar: HashMap<String, serde_json::Value> = dropped.into_iter().collect();
        let sidecar_path = path.with_extension("json.corrupt");
        match serde_json::to_vec_pretty(&sidecar) {
            Ok(side_bytes) => {
                if let Err(e) = std::fs::write(&sidecar_path, &side_bytes) {
                    warn!(
                        error = %e,
                        path = %sidecar_path.display(),
                        "session_lifecycle_store: failed to write .corrupt side-car for dropped records"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: failed to serialize dropped records side-car");
            }
        }
    }

    map
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
    /// The session's shell pty is alive but no live Claude is present in its
    /// inclusive subtree, and it has not yet reached [`LIVE_SHELL_DEAD_TICKS`]
    /// consecutive claude-absent ticks — wait more ticks before closing
    /// (debounce a transient snapshot miss / an in-flight relaunch).
    NeedsConfirm,
    /// The session is dead — flip it to `closed` (reason `"poll-dead"`).
    Close,
    /// No live terminal matched this record this tick, and it has not yet
    /// reached [`NO_TERMINAL_ORPHAN_TICKS`] consecutive no-match ticks —
    /// increment the no-match counter and wait (debounce registration races).
    NoMatchWait,
    /// No live terminal matched for > [`NO_TERMINAL_ORPHAN_TICKS`]
    /// consecutive ticks — the record is an orphan; close it with reason
    /// `"no-terminal"` (non-restorable).
    CloseNoTerminal,
    /// Uncertain — do nothing (NEVER close on uncertainty).
    Skip,
}

/// Pure liveness-classification core. Asymmetric by design: we only ever
/// close on a *confident* dead signal (a dead pty, or a no-match streak long
/// enough to be an orphan rather than a race), and `Skip` (never close) on
/// genuine uncertainty (snapshot failure, mid-restore records).
///
/// - `live_is_alive`: `Some(true)` shell pty alive, `Some(false)` dead,
///   `None` no matching pty for this session.
/// - `claude_present`: whether a live Claude process is present in the tracked
///   PID's *inclusive* subtree (the tracked PID's own image is `claude*`, or any
///   descendant's is). This replaces the old `descendant_count > 0` heuristic,
///   which mis-closed an idle **agent** session whose tracked PID *is* `claude`
///   (it spawns zero children while parked on a gate). The invariant: the
///   `Some(true)` arm never reaches `Close` while `claude_present` — a live
///   Claude here is the normal state, never a dead signal. `Close` is reachable
///   from `Some(true)` only when claude is ABSENT for `LIVE_SHELL_DEAD_TICKS`
///   (the preserved bare-shell cleanup: operator quit claude, shell PID lingers).
/// - `consecutive_dead`: count of prior consecutive claude-absent ticks.
/// - `consecutive_no_match`: count of prior consecutive ticks where NO live
///   terminal matched this record. "No matching terminal in THIS instance
///   for many consecutive ticks" is not uncertainty — it is an orphan that
///   would otherwise stay `open` for up to 7 days and re-qualify for restore
///   at every boot. Debounced via [`NO_TERMINAL_ORPHAN_TICKS`].
/// - `snapshot_ok`: whether the system-wide process snapshot succeeded.
/// - `restore_pending`: whether the record carries a restore-pending marker
///   (a boot-restore typed/queued `claude --resume` whose handshake has not
///   been verified yet — or the restore FAILED and awaits an operator retry).
///   A mid-restore record is uncertainty by definition: the restored pane may
///   be a plain shell whose resume never landed, and flipping it closed
///   would destroy the durable `open` state the next attempt needs. So while
///   pending, only a confident "alive and busy" (KeepAlive) observation
///   passes through — which also lets the caller self-heal a stale marker —
///   and every other outcome becomes Skip. In particular a mid-restore row
///   (which keeps its OLD `terminal_id` until the deferred re-assert) must
///   never accumulate no-match ticks: `None` + pending ⇒ Skip, not
///   `NoMatchWait`.
pub fn classify(
    live_is_alive: Option<bool>,
    claude_present: bool,
    consecutive_dead: u32,
    consecutive_no_match: u32,
    snapshot_ok: bool,
    restore_pending: bool,
) -> PollAction {
    if !snapshot_ok {
        return PollAction::Skip;
    }
    let base = match live_is_alive {
        None => {
            if consecutive_no_match < NO_TERMINAL_ORPHAN_TICKS {
                PollAction::NoMatchWait
            } else {
                PollAction::CloseNoTerminal
            }
        }
        Some(false) => PollAction::Close,
        Some(true) => {
            if claude_present {
                // A live Claude in the inclusive subtree — KeepAlive
                // unconditionally, no matter how long it has idled. This is the
                // floor that fixes the false poll-dead close of idle agents.
                PollAction::KeepAlive
            } else if consecutive_dead < LIVE_SHELL_DEAD_TICKS {
                // No Claude present, but debounce several ticks before closing:
                // absorbs a transient snapshot miss, and a restart in this
                // window does not drop a still-live session.
                PollAction::NeedsConfirm
            } else {
                // Claude absent for the full debounce — genuine poll-dead (e.g.
                // operator quit claude and only the bare shell PID lingers).
                PollAction::Close
            }
        }
    };
    // Restore-pending guard arm (never-close-on-uncertainty): a record mid-
    // restore (or whose restore failed) must keep its `open` state intact —
    // never Close, and don't even accumulate NeedsConfirm or no-match ticks
    // against it.
    if restore_pending && base != PollAction::KeepAlive {
        return PollAction::Skip;
    }
    base
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
            provider: DEFAULT_PROVIDER.to_string(),
            origin: None,
            restore_pending_at: None,
        }
    }

    /// Records predating the origin field must deserialize and read as
    /// reconciled (the conservative default).
    #[test]
    fn pre_origin_record_deserializes_as_reconciled() {
        let json = r#"{"claudeSessionId":"old-sess","configDir":null,"workingDir":"C:/repo",
            "pageId":"default","zoneIndex":1,"title":"Old","terminalId":"term-old",
            "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,"closeReason":null}"#;
        let rec: TerminalSessionRecord = serde_json::from_str(json).unwrap();
        // `None` IS the reconciled reading — consumers default absent to
        // "reconciled". Provider defaults to claude for pre-provider rows.
        assert_eq!(
            rec.origin.as_deref().unwrap_or(ORIGIN_RECONCILED),
            ORIGIN_RECONCILED
        );
        assert_eq!(rec.provider, DEFAULT_PROVIDER);
    }

    /// A legacy on-disk record carrying `bindOrigin:"pinned"` must load as
    /// `origin:"authoritative"`, and `bindOrigin:"guessed"` as
    /// `origin:"reconciled"` (the value migration, exercised through the real
    /// load path so the `alias` + `load_map` normalization both fire).
    #[test]
    fn legacy_bind_origin_values_migrate_on_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let file = r#"{
            "pin": {
                "claudeSessionId":"pin","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":1,"title":"P","terminalId":"t1",
                "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,
                "closeReason":null,"bindOrigin":"pinned"
            },
            "guess": {
                "claudeSessionId":"guess","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":2,"title":"G","terminalId":"t2",
                "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,
                "closeReason":null,"bindOrigin":"guessed"
            }
        }"#;
        std::fs::write(&path, file).unwrap();
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert_eq!(
            store.get("pin").unwrap().origin.as_deref(),
            Some(ORIGIN_AUTHORITATIVE),
            "legacy pinned migrates to authoritative"
        );
        assert_eq!(
            store.get("guess").unwrap().origin.as_deref(),
            Some(ORIGIN_RECONCILED),
            "legacy guessed migrates to reconciled"
        );
        // Pre-provider rows default to claude.
        assert_eq!(store.get("pin").unwrap().provider, DEFAULT_PROVIDER);
    }

    /// Authoritative origin survives an unasserted re-record; asserted updates.
    #[test]
    fn record_open_preserves_origin_when_unasserted() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let mut auth = rec("sess-1");
        auth.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
        store.record_open(auth);
        store.record_open(rec("sess-1")); // unasserted (None)
        assert_eq!(
            store.open_records()[0].origin.as_deref(),
            Some(ORIGIN_AUTHORITATIVE)
        );
        let mut reconciled = rec("sess-1");
        reconciled.origin = Some(ORIGIN_RECONCILED.to_string());
        store.record_open(reconciled);
        assert_eq!(
            store.open_records()[0].origin.as_deref(),
            Some(ORIGIN_RECONCILED)
        );
    }

    /// `record_open` normalizes a legacy origin value handed in by an
    /// un-migrated caller (`pinned` → `authoritative`).
    #[test]
    fn record_open_normalizes_legacy_origin_value() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let mut legacy = rec("sess-1");
        legacy.origin = Some("pinned".to_string());
        store.record_open(legacy);
        assert_eq!(
            store.open_records()[0].origin.as_deref(),
            Some(ORIGIN_AUTHORITATIVE)
        );
    }

    /// `rekey_session` moves a row from the old id to the new id, updating the
    /// record's own `claude_session_id`, and is a safe no-op on the edge cases.
    #[test]
    fn rekey_session_moves_row_and_guards_edges() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();
        let mut r = rec("old");
        r.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
        store.record_open(r);

        store.rekey_session("old", "new");
        assert!(store.get("old").is_none(), "old key removed");
        let moved = store.get("new").expect("row present under new id");
        assert_eq!(moved.claude_session_id, "new", "record id updated to match");
        assert_eq!(moved.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));

        // Durable across a reload.
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert!(store.get("new").is_some());

        // Edge: absent old id — no-op, no panic.
        store.rekey_session("ghost", "whatever");
        assert!(store.get("whatever").is_none());

        // Edge: old == new — no-op.
        store.rekey_session("new", "new");
        assert!(store.get("new").is_some());

        // Edge: target already present — refuse to clobber.
        store.record_open(rec("other"));
        store.rekey_session("other", "new");
        assert!(store.get("other").is_some(), "source kept on refusal");
        assert_eq!(
            store.get("new").unwrap().claude_session_id,
            "new",
            "target untouched on refusal"
        );
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
            .restorable_records(now, None)
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

    /// Item-1: one malformed record must NOT discard the whole store. A real
    /// boot loads a registry whose object shape is intact but one entry fails
    /// to deserialize (schema skew / partial corruption); the load must KEEP
    /// the valid rows and warn+drop only the bad one — the original
    /// whole-map deserialize wiped everything on the first bad value, silently
    /// losing every restorable on-screen session.
    #[test]
    fn malformed_record_drops_only_that_row_and_keeps_the_rest() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        // Two valid records + one malformed (wrong type for `zoneIndex`, which
        // is a required `i32`, so `from_value` rejects just this entry).
        let file = r#"{
            "good-1": {
                "claudeSessionId":"good-1","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":1,"title":"Good 1","terminalId":"term-1",
                "openedAt":100,"lastSeenAt":200,"state":"open","closedAt":null,"closeReason":null
            },
            "bad-1": {
                "claudeSessionId":"bad-1","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":"NOT-AN-INT","title":"Bad","terminalId":"term-bad",
                "openedAt":100,"lastSeenAt":200,"state":"open","closedAt":null,"closeReason":null
            },
            "good-2": {
                "claudeSessionId":"good-2","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":2,"title":"Good 2","terminalId":"term-2",
                "openedAt":300,"lastSeenAt":400,"state":"open","closedAt":null,"closeReason":null
            }
        }"#;
        std::fs::write(&path, file).unwrap();

        let store = SessionLifecycleStore::open(&path).unwrap();
        let mut ids: Vec<String> = store
            .open_records()
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["good-1".to_string(), "good-2".to_string()],
            "the two valid rows must survive; only the malformed row is dropped"
        );

        // The dropped raw entry is preserved in a `.corrupt` side-car.
        let sidecar = path.with_extension("json.corrupt");
        assert!(
            sidecar.exists(),
            "a .corrupt side-car must capture the drop"
        );
        let side: HashMap<String, serde_json::Value> =
            serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
        assert!(side.contains_key("bad-1"), "side-car holds the dropped row");
        assert!(!side.contains_key("good-1"), "side-car excludes good rows");

        // Store stays usable; a fresh write persists the kept-plus-new set.
        store.record_open(rec("new-1"));
        let store = SessionLifecycleStore::open(&path).unwrap();
        let mut ids2: Vec<String> = store
            .open_records()
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        ids2.sort();
        assert_eq!(
            ids2,
            vec![
                "good-1".to_string(),
                "good-2".to_string(),
                "new-1".to_string()
            ],
        );
    }

    // --- classify() — every branch -----------------------------------------

    #[test]
    fn classify_skip_on_snapshot_failure() {
        // snapshot_ok=false dominates every other input.
        assert_eq!(
            classify(Some(false), false, 5, 0, false, false),
            PollAction::Skip
        );
        assert_eq!(
            classify(Some(true), true, 0, 0, false, false),
            PollAction::Skip
        );
        assert_eq!(classify(None, false, 0, 0, false, false), PollAction::Skip);
        // Even a no-match streak past the orphan threshold must Skip (not
        // CloseNoTerminal) when the snapshot failed.
        assert_eq!(
            classify(None, false, 0, NO_TERMINAL_ORPHAN_TICKS + 5, false, false),
            PollAction::Skip
        );
    }

    #[test]
    fn classify_no_match_waits_below_orphan_ticks() {
        // A record matching no live terminal accumulates NoMatchWait ticks
        // for every tick strictly below NO_TERMINAL_ORPHAN_TICKS — it must
        // NOT close on a brief registration race.
        for prior in 0..NO_TERMINAL_ORPHAN_TICKS {
            assert_eq!(
                classify(None, false, 0, prior, true, false),
                PollAction::NoMatchWait,
                "no match, {prior} prior no-match ticks must NoMatchWait"
            );
        }
        // claude_present/dead-tick inputs are irrelevant to the no-match arm.
        assert_eq!(
            classify(None, true, 9, 0, true, false),
            PollAction::NoMatchWait
        );
    }

    #[test]
    fn classify_no_match_closes_no_terminal_after_orphan_ticks() {
        // Once the no-match streak reaches the threshold the record is an
        // orphan — close it with the (non-restorable) "no-terminal" reason.
        // With a 45s poll the close lands on the 4th consecutive tick ≈ 3min.
        assert_eq!(
            classify(None, false, 0, NO_TERMINAL_ORPHAN_TICKS, true, false),
            PollAction::CloseNoTerminal
        );
        assert_eq!(
            classify(None, false, 0, NO_TERMINAL_ORPHAN_TICKS + 5, true, false),
            PollAction::CloseNoTerminal
        );
    }

    #[test]
    fn classify_close_on_dead_shell() {
        // A dead pty closes regardless of claude_present — the unambiguous
        // confident-dead signal.
        assert_eq!(
            classify(Some(false), false, 0, 0, true, false),
            PollAction::Close
        );
        assert_eq!(
            classify(Some(false), true, 0, 0, true, false),
            PollAction::Close
        );
    }

    #[test]
    fn classify_keepalive_when_claude_present() {
        // Claude present in the inclusive subtree ⇒ KeepAlive immediately —
        // even with zero prior dead ticks (the idle-agent bug) and even after
        // a long prior claude-absent streak (claude came back).
        assert_eq!(
            classify(Some(true), true, 0, 0, true, false),
            PollAction::KeepAlive
        );
        assert_eq!(
            classify(Some(true), true, 9, 0, true, false),
            PollAction::KeepAlive
        );
    }

    #[test]
    fn classify_idle_agent_session_is_kept_alive() {
        // Regression for the live incident (2026-06-19): an idle agent /
        // gate-continuation session whose tracked PID *is* claude spawns zero
        // children while parked on a gate. Under the old `descendant_count > 0`
        // heuristic this read as dead and was closed `poll-dead` in ~135s while
        // perfectly alive. With `claude_present` the answer is KeepAlive on the
        // very first tick, forever, no matter how long it idles.
        assert_eq!(
            classify(Some(true), true, 0, 0, true, false),
            PollAction::KeepAlive
        );
        assert_eq!(
            classify(
                Some(true),
                true,
                LIVE_SHELL_DEAD_TICKS + 100,
                0,
                true,
                false
            ),
            PollAction::KeepAlive
        );
    }

    #[test]
    fn classify_operator_quit_claude_closes_after_debounce() {
        // The preserved bare-shell cleanup: operator quits claude, the shell
        // PID stays alive but no claude remains in its subtree → NeedsConfirm
        // through the debounce, then Close. (prior < LIVE_SHELL_DEAD_TICKS is
        // NeedsConfirm, NOT Close — the plan's test-plan "prior=2 ⇒ Close" bullet
        // predated keeping the 3-tick debounce; Change #2 keeps it, so Close
        // requires the full streak.)
        assert_eq!(
            classify(Some(true), false, LIVE_SHELL_DEAD_TICKS - 1, 0, true, false),
            PollAction::NeedsConfirm
        );
        assert_eq!(
            classify(Some(true), false, LIVE_SHELL_DEAD_TICKS, 0, true, false),
            PollAction::Close
        );
    }

    #[test]
    fn classify_needs_confirm_while_below_live_shell_dead_ticks() {
        // A live shell with NO claude present stays in NeedsConfirm for every
        // tick strictly below LIVE_SHELL_DEAD_TICKS — it must NOT close on a
        // brief blip (a transient snapshot miss / in-flight relaunch).
        for prior in 0..LIVE_SHELL_DEAD_TICKS {
            assert_eq!(
                classify(Some(true), false, prior, 0, true, false),
                PollAction::NeedsConfirm,
                "live shell, claude absent, {prior} prior ticks must NeedsConfirm"
            );
        }
        // Specifically: the second tick (prior == 1) no longer closes.
        assert_eq!(
            classify(Some(true), false, 1, 0, true, false),
            PollAction::NeedsConfirm
        );
    }

    #[test]
    fn classify_close_only_after_live_shell_dead_ticks_reached() {
        // Only once we've accumulated LIVE_SHELL_DEAD_TICKS consecutive
        // claude-absent ticks does a live shell finally close (operator quit
        // claude; the bare shell PID lingers).
        assert_eq!(
            classify(Some(true), false, LIVE_SHELL_DEAD_TICKS, 0, true, false),
            PollAction::Close
        );
        assert_eq!(
            classify(Some(true), false, LIVE_SHELL_DEAD_TICKS + 5, 0, true, false),
            PollAction::Close
        );
    }

    #[test]
    fn classify_restore_pending_never_closes() {
        // The incident shape: a restored pane whose resume silently failed.
        // Dead pty (the restore shell died / was never matched) → Skip, NOT
        // Close — the durable `open` record must survive for the next attempt.
        assert_eq!(
            classify(Some(false), false, 0, 0, true, true),
            PollAction::Skip
        );
        // Plain shell with no claude present, even past the debounce ticks
        // (the exact poll-dead flip the incident hit) → Skip.
        assert_eq!(
            classify(Some(true), false, LIVE_SHELL_DEAD_TICKS, 0, true, true),
            PollAction::Skip
        );
        assert_eq!(
            classify(Some(true), false, LIVE_SHELL_DEAD_TICKS + 5, 0, true, true),
            PollAction::Skip
        );
        // Below the debounce it must not even accumulate NeedsConfirm ticks.
        assert_eq!(
            classify(Some(true), false, 0, 0, true, true),
            PollAction::Skip
        );
        // No matching pty → Skip, NOT NoMatchWait: a mid-restore row keeps
        // its OLD terminal_id until the re-assert and must never accumulate
        // no-match ticks toward an orphan close.
        assert_eq!(classify(None, false, 0, 0, true, true), PollAction::Skip);
        // Even a streak past the orphan threshold must not close while the
        // restore-pending marker is set.
        assert_eq!(
            classify(None, false, 0, NO_TERMINAL_ORPHAN_TICKS + 1, true, true),
            PollAction::Skip
        );
    }

    #[test]
    fn classify_restore_pending_passes_through_confident_alive() {
        // A confidently-alive session (claude present) classifies KeepAlive
        // even while restore-pending — the caller uses this to self-heal a
        // stale marker.
        assert_eq!(
            classify(Some(true), true, 0, 0, true, true),
            PollAction::KeepAlive
        );
    }

    #[test]
    fn no_terminal_close_is_not_restorable() {
        // The poll's orphan close ("no-terminal") must NOT re-qualify for
        // restore — only "pty-exit"/"poll-dead" closes get a grace window.
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("orphan-sess"));
        store.record_close("orphan-sess", "no-terminal");

        let now = Utc::now().timestamp_millis();
        assert!(
            store.restorable_records(now, None).is_empty(),
            "a no-terminal close (even seconds old) must not be restorable"
        );
    }

    #[test]
    fn mark_and_clear_restore_pending_round_trip_and_persist() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-1"));
        assert!(store.open_records()[0].restore_pending_at.is_none());

        store.mark_restore_pending("sess-1");
        assert!(store.open_records()[0].restore_pending_at.is_some());

        // Durable across a "restart" — the marker must survive a frontend /
        // process crash mid-restore.
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert!(store.open_records()[0].restore_pending_at.is_some());

        store.clear_restore_pending("sess-1");
        assert!(store.open_records()[0].restore_pending_at.is_none());
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert!(store.open_records()[0].restore_pending_at.is_none());

        // Absent ids are no-ops (no panic).
        store.mark_restore_pending("ghost");
        store.clear_restore_pending("ghost");
        // Double-clear is a no-op.
        store.clear_restore_pending("sess-1");
    }

    #[test]
    fn record_open_preserves_restore_pending_marker() {
        // The restore path re-asserts the open record under the fresh terminal
        // id AFTER marking restore-pending — the re-assert must not wipe the
        // marker (only a verified handshake / confident-alive poll clears it).
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-1"));
        store.mark_restore_pending("sess-1");

        let mut r2 = rec("sess-1");
        r2.terminal_id = "term-new".to_string();
        store.record_open(r2);

        let open = store.open_records();
        assert_eq!(open[0].terminal_id, "term-new", "re-assert refreshed");
        assert!(
            open[0].restore_pending_at.is_some(),
            "record_open must preserve the restore-pending marker"
        );
    }

    /// Build a fully-specified fixture record (explicit timestamps, unlike
    /// `rec` whose timestamps `record_open` overwrites).
    fn fixture_rec(
        id: &str,
        state: &str,
        last_seen_at: i64,
        closed_at: Option<i64>,
        close_reason: Option<&str>,
    ) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: None,
            working_dir: Some("C:/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: Some(id.to_string()),
            terminal_id: format!("term-{id}"),
            opened_at: last_seen_at - 1_000,
            last_seen_at,
            state: state.to_string(),
            closed_at,
            close_reason: close_reason.map(str::to_string),
            provider: DEFAULT_PROVIDER.to_string(),
            origin: None,
            restore_pending_at: None,
        }
    }

    /// Write a registry fixture file directly (bypassing `record_open`'s
    /// now-stamping) so tests control every timestamp.
    fn write_fixture(path: &Path, recs: Vec<TerminalSessionRecord>) {
        let m: HashMap<String, TerminalSessionRecord> = recs
            .into_iter()
            .map(|r| (r.claude_session_id.clone(), r))
            .collect();
        std::fs::write(path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    }

    fn restorable_ids(
        store: &SessionLifecycleStore,
        now: i64,
        prior_marker_at: Option<i64>,
    ) -> Vec<String> {
        let mut ids: Vec<String> = store
            .restorable_records(now, prior_marker_at)
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        ids.sort();
        ids
    }

    /// Item-1 verification fixture (the on-page repro): two in-grace
    /// pty-exit rows (exactly what a graceful shutdown writes for on-screen
    /// panes), one explicit close, and one stale `open` ghost whose terminal
    /// died 72h before shutdown. After a clean restart only the two pty-exit
    /// rows may restore — the ghost spawns no pane.
    #[test]
    fn restorable_records_exclude_stale_open_ghost_after_clean_restart() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let shutdown = 1_700_000_000_000_i64; // registry's last moment of life
        let now = shutdown + 60_000; // restart one minute later
        write_fixture(
            &path,
            vec![
                fixture_rec(
                    "fixture-onscreen-0001",
                    "closed",
                    shutdown,
                    Some(shutdown),
                    Some("pty-exit"),
                ),
                fixture_rec(
                    "fixture-onscreen-0002",
                    "closed",
                    shutdown,
                    Some(shutdown),
                    Some("pty-exit"),
                ),
                fixture_rec(
                    "fixture-explicit-0003",
                    "closed",
                    shutdown,
                    Some(shutdown),
                    Some("explicit"),
                ),
                fixture_rec(
                    "fixture-ghost-0004",
                    "open",
                    shutdown - 72 * 3_600_000,
                    None,
                    None,
                ),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        let expected = vec![
            "fixture-onscreen-0001".to_string(),
            "fixture-onscreen-0002".to_string(),
        ];
        // With the clean-shutdown marker anchoring the registry…
        assert_eq!(restorable_ids(&store, now, Some(shutdown)), expected);
        // …and even without it: the pty-exit closes already anchor the
        // registry at the shutdown instant, so the 72h ghost stays out.
        assert_eq!(restorable_ids(&store, now, None), expected);
    }

    /// Anchored recency is downtime-proof: open rows fresh AT THE CRASH
    /// restore even when the next boot happens hours later (a wall-clock
    /// `now - last_seen <= grace` rule would restore nothing), while the
    /// stale ghost stays excluded.
    #[test]
    fn restorable_records_open_rows_survive_long_downtime_anchor_relative() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let crash = 1_700_000_000_000_i64;
        let now = crash + 6 * 3_600_000; // booted 6 hours later
        write_fixture(
            &path,
            vec![
                fixture_rec("fresh-a", "open", crash, None, None),
                fixture_rec("fresh-b", "open", crash - 30_000, None, None),
                fixture_rec("ghost", "open", crash - 72 * 3_600_000, None, None),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        // The crashed process's marker (`clean:false`, stamped at ITS boot,
        // a day earlier) must not shrink the anchor below the fresh rows.
        let prior_marker_at = Some(crash - 86_400_000);
        assert_eq!(
            restorable_ids(&store, now, prior_marker_at),
            vec!["fresh-a".to_string(), "fresh-b".to_string()],
            "fresh-at-crash open rows restore after multi-hour downtime; ghost excluded"
        );
    }

    /// A real on-screen session whose `pty-exit` close never flushed during
    /// a clean shutdown (the webview died before `handleExit` wrote) leaves
    /// an `open` row with a fresh-relative-to-anchor `last_seen_at` — it
    /// must restore on the clean boot (no silent session loss).
    #[test]
    fn restorable_records_admit_unflushed_close_open_row_on_clean_boot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let shutdown = 1_700_000_000_000_i64;
        let now = shutdown + 60_000;
        write_fixture(
            &path,
            vec![
                fixture_rec(
                    "flushed",
                    "closed",
                    shutdown,
                    Some(shutdown),
                    Some("pty-exit"),
                ),
                // Last touched by the 45s poll shortly before shutdown; its
                // pty-exit close never flushed.
                fixture_rec("unflushed", "open", shutdown - 90_000, None, None),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert_eq!(
            restorable_ids(&store, now, Some(shutdown)),
            vec!["flushed".to_string(), "unflushed".to_string()],
            "an open row within grace of the anchor restores even on a clean boot"
        );
    }

    /// A registry whose ONLY row is a stale open ghost self-anchors (its own
    /// `last_seen_at` is the max) — the prior shutdown marker supplies the
    /// honest "last moment of life" and excludes it.
    #[test]
    fn restorable_records_marker_anchor_excludes_lone_ghost() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let shutdown = 1_700_000_000_000_i64;
        let now = shutdown + 60_000;
        let ghost_seen = shutdown - 72 * 3_600_000;
        write_fixture(
            &path,
            vec![fixture_rec("ghost", "open", ghost_seen, None, None)],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        assert!(
            restorable_ids(&store, now, Some(shutdown)).is_empty(),
            "the marker anchor must exclude a lone stale ghost"
        );
        // Documented fallback: with no marker and no sibling rows the ghost
        // self-anchors and is admitted (defensive — better than losing a
        // real lone session on a registry with no other signal).
        assert_eq!(restorable_ids(&store, now, None), vec!["ghost".to_string()]);
    }

    #[test]
    fn restorable_records_includes_in_grace_poll_dead() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        // A poll-dead close inside the grace window IS restorable — the
        // shell was still alive when the poll fired, so an immediate restart
        // must bring it back rather than silently drop it.
        store.record_open(rec("poll-dead-fresh"));
        store.record_close("poll-dead-fresh", "poll-dead");
        // A poll-dead close aged beyond the grace window is NOT restorable.
        store.record_open(rec("poll-dead-stale"));
        store.record_close("poll-dead-stale", "poll-dead");

        let now = Utc::now().timestamp_millis();

        // Age `poll-dead-fresh` to now-30s (well inside grace) and
        // `poll-dead-stale` past the grace window.
        {
            let raw = std::fs::read(&path).unwrap();
            let mut m: HashMap<String, TerminalSessionRecord> =
                serde_json::from_slice(&raw).unwrap();
            m.get_mut("poll-dead-fresh").unwrap().closed_at = Some(now - 30_000);
            m.get_mut("poll-dead-stale").unwrap().closed_at =
                Some(now - RESTORABLE_POLL_DEAD_MS - 1000);
            let bytes = serde_json::to_vec_pretty(&m).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        let store = SessionLifecycleStore::open(&path).unwrap();

        let ids: Vec<String> = store
            .restorable_records(now, None)
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        assert!(
            ids.contains(&"poll-dead-fresh".to_string()),
            "poll-dead closed 30s ago must be restorable; got {ids:?}"
        );
        assert!(
            !ids.contains(&"poll-dead-stale".to_string()),
            "poll-dead aged past grace must NOT be restorable; got {ids:?}"
        );
    }
}
