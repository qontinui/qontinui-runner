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
//!
//! Durability is a **write-ahead log of per-record deltas** compacted into that
//! snapshot — see [`LifecycleDelta`] and [`SessionLifecycleStore::compact`].
//!
//! ## Durability scope — PROCESS crash, not power loss
//!
//! The WAL survives the runner process dying: an appended line is one
//! `write_all` into an append-mode handle, so a crash can only truncate the
//! tail (which [`replay_wal`] drops), and compaction writes the snapshot before
//! truncating the log, so a crash in between replays deltas the snapshot
//! already holds — idempotent.
//!
//! It is deliberately NOT power-loss durable. Appends are **not `fsync`ed**:
//! they sit in the OS page cache, so a host power cut or kernel panic can lose
//! recently appended deltas even though the `write_all` returned. That is the
//! accepted trade — an `fsync` per mutation would put a disk flush back on the
//! terminal spawn path, which is exactly the cost this design removed, and the
//! failure mode is bounded (a session record loses at most its latest state and
//! the liveness poll re-discovers live sessions). Do not read the crash-safety
//! ordering above as a power-loss guarantee.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::session::restore_record_emitter::RestoreRecordEmitter;
use crate::session::snapshot_history::{SnapshotHistory, SnapshotSession, TranscriptProbe};

/// The runner's own app-data root, `~/.qontinui/runner`, keeping the existing
/// "." fallback for a home-less environment. Base for the instance-scoped
/// path helpers below.
fn runner_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
}

/// Canonical, instance-scoped lifecycle-store path — the single source of
/// truth every consumer must resolve through (boot writer, fleet publishers,
/// message poller, session bus, the test-fixture control routes).
///
/// - Primary (`instance::data_subdir() == None`) → the legacy UNSCOPED
///   `~/.qontinui/runner/terminal-sessions.json`, byte-for-byte identical to
///   the pre-fix primary path, so its crash-recovery reattach is unchanged.
/// - Named/temp secondary → `~/.qontinui/runner/instance-<name>/terminal-sessions.json`.
///
/// This replaces the previous API-PORT keying. Temp-runner ports (9877-9899)
/// are RECYCLED across unrelated spawns, so a fresh runner on a reused port
/// used to `open()` the previous occupant's `terminal-sessions-<port>.json`
/// and auto-restore its foreign PTYs. The durable instance identity is unique
/// per spawn and stable per named runner, so no such inheritance is possible.
/// Mirrors the #788 `window-assignments` convention (scope the DIRECTORY via
/// `instance::scope_path`, keep the filename plain) and reuses its fail-closed
/// `instance-unnamed-<port>` quarantine for free.
pub fn store_path() -> PathBuf {
    crate::instance::scope_path(&runner_dir()).join("terminal-sessions.json")
}

/// Canonical, instance-scoped snapshot-history path, sibling to
/// [`store_path`]: primary →
/// `~/.qontinui/runner/session-restore/session-snapshots.jsonl`; secondary →
/// `~/.qontinui/runner/instance-<name>/session-restore/session-snapshots.jsonl`.
///
/// DELIBERATELY re-derives the `session-restore/` segment under the scoped
/// base rather than calling `crate::session::claude_hook::session_restore_dir()`:
/// that dir is ALSO the Claude SessionStart-hook materialization dir and must
/// stay UNSCOPED (scoping it globally would move hook delivery — out of scope).
/// Do NOT "consolidate" the two into one call.
pub fn snapshot_history_path() -> PathBuf {
    crate::instance::scope_path(&runner_dir())
        .join("session-restore")
        .join("session-snapshots.jsonl")
}

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
/// Observed origin: a claude-process-start-anchored, uniquely-correlated
/// transcript bind — the transcript proves the session exists, so restore
/// treats a CONFIRMED observed row the same as confirmed authoritative
/// (auto-resume-eligible), unlike the conservative `reconciled`/mtime guess.
pub const ORIGIN_OBSERVED: &str = "observed";

/// Normalize a possibly-legacy `origin` value to the current vocabulary. Maps
/// the pre-migration `bind_origin` values (`"pinned"`→`"authoritative"`,
/// `"guessed"`→`"reconciled"`) and passes the new values through unchanged —
/// including `"observed"`, which has no legacy alias and falls through the `_`
/// arm verbatim. Any other string is left verbatim (forward-compat: a value
/// this build doesn't know is not silently rewritten). Returns `None` for
/// `None`.
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
    /// Unix millis when a provider's SessionStart hook CONFIRMED that a real
    /// session actually started in this terminal (session-restore-redesign
    /// Phase 2 coordinator refinement). `None`/unset = PROVISIONAL.
    ///
    /// ## Why a provisional→confirmed distinction
    ///
    /// The spawn-time authoritative record ([`apply_identity_seam`]) is written
    /// for EVERY terminal — including a plain shell that never runs a provider
    /// (the runner can't know at spawn whether the user will type `claude`, run
    /// `ls`, or just sit at a prompt). Restoring those phantom shell "sessions"
    /// as `claude --resume <unused-uuid>` would manufacture failed resumes. The
    /// provider's SessionStart hook firing — POSTed to `/control/session-open`
    /// with `source:"startup"|"resume"` — is the observable proof that a REAL
    /// provider started here. The hook write sets this; the spawn-time write
    /// leaves it unset. Phase 4's restore classifier uses `confirmed_at`
    /// (OR a real transcript on disk) to gate auto-resume vs treat-as-plain-shell
    /// — this phase only PROVIDES the signal; it does NOT change the classifier.
    ///
    /// `#[serde(default)]`: every pre-Phase-2 on-disk record deserializes as
    /// `None` (provisional) — a confirming hook (or Phase 4's transcript check)
    /// re-establishes confirmation; nothing is lost, the field is purely
    /// additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<i64>,
    /// Coord-minted stable fleet session handle (`fsh_…`) for this session
    /// (session-identity fabric Phase 1, plan
    /// `2026-07-05-session-identity-messaging-restore-fabric.md` §4). Persisted
    /// next to `claude_session_id` so a restart re-presents the SAME handle on
    /// restore-rebind instead of re-minting. The registry is
    /// SERVER-authoritative — rebind is keyed on the durable
    /// `claude_session_id`, so a divergent local value is overwritten (server
    /// wins; see [`SessionLifecycleStore::set_handle`]).
    ///
    /// `#[serde(default)]`: every pre-fabric on-disk record deserializes as
    /// `None` (no handle acquired yet) — purely additive; legacy rows load
    /// cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

/// One durable, self-contained mutation appended to the write-ahead log.
///
/// The registry used to durably record a mutation by rewriting the WHOLE map
/// (`to_vec_pretty` → `.json.tmp` → `rename`), making every terminal spawn
/// O(total sessions) and the aggregate O(N²). A delta is O(1) in the number
/// of sessions: one `write_all` of one line.
///
/// Both variants are IDEMPOTENT and last-writer-wins per key, so replaying a
/// suffix that the snapshot already contains (the crash-between-snapshot-and-
/// truncate window) is harmless.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum LifecycleDelta {
    /// Insert-or-replace the record under its own `claude_session_id`.
    /// `Box`ed so the enum stays small (the record is ~20 fields).
    Upsert { rec: Box<TerminalSessionRecord> },
    /// Delete the record stored under `id`.
    Remove { id: String },
}

/// Append this many deltas before forcing a compaction, regardless of whether
/// the idle/heartbeat compaction has had a chance to run. Bounds both the WAL
/// file size and the boot replay cost under a spawn burst.
const WAL_COMPACT_APPENDS: usize = 512;

/// Open-in-append WAL handle plus its compaction bookkeeping.
///
/// The handle is kept open across appends (re-opening per mutation would put a
/// directory lookup + create back on the spawn path) and is dropped on
/// compaction, which truncates the file.
#[derive(Debug, Default)]
struct WalWriter {
    file: Option<std::fs::File>,
    /// Deltas appended since the last successful compaction.
    appends: usize,
    /// A mutation landed in the in-memory map but did NOT reach the WAL
    /// (serialize failure, open failure, `write_all` failure, poisoned lock).
    ///
    /// Without this the store would be silently dirty-but-uncounted: the map is
    /// already mutated, `appends` never moved, and `compact_if_dirty` gates on
    /// `appends > 0` — so if that mutation were the LAST one before a crash it
    /// would be lost outright. (The whole-map rewrite this replaced had no such
    /// hole: the next mutation's rewrite carried the earlier one along.) Cleared
    /// by a successful [`SessionLifecycleStore::compact`], which writes the
    /// whole map and therefore captures it.
    dirty: bool,
}

/// Durable map of `claude_session_id -> TerminalSessionRecord`.
///
/// Cheap to clone-share via `Arc`. Durability is a **write-ahead log of
/// per-record deltas** (`terminal-sessions.wal.jsonl`) compacted into the JSON
/// snapshot (`terminal-sessions.json`); a mutation costs one appended line, not
/// a whole-map rewrite. See [`LifecycleDelta`] and [`SessionLifecycleStore::compact`].
#[derive(Debug)]
pub struct SessionLifecycleStore {
    path: PathBuf,
    /// Sibling write-ahead log — `<path>` with the extension replaced by
    /// `wal.jsonl`. Derived once at [`Self::open`].
    wal_path: PathBuf,
    /// WAL append handle.
    ///
    /// LOCK ORDER — `map` then `wal`, never the reverse. Every delta is written
    /// while its mutation still holds the map lock, so the WAL order is exactly
    /// the mutation order and replay is last-writer-wins with no sequence
    /// numbers. (The old whole-map `persist` ran OUTSIDE the lock and could
    /// therefore let a stale snapshot land after a fresher one, silently losing
    /// a mutation; deltas under the lock remove that race outright.)
    /// [`Self::compact`] takes both in the same order, so an appender is either
    /// entirely inside the snapshot it truncates or entirely after it.
    wal: Mutex<WalWriter>,
    map: Mutex<HashMap<String, TerminalSessionRecord>>,
    /// Optional write-only sink for the append-only snapshot HISTORY
    /// ([`crate::session::snapshot_history`], Phase 4 of the session-restore
    /// shim-fix plan). When attached, every layout-meaningful mutation
    /// (open/close/rename/rekey/remove/confirm) appends one complete
    /// snapshot of the registry to the durable JSONL audit trail. The
    /// history is DERIVED from this store — never read back by it or by the
    /// restore path.
    snapshot_history: OnceLock<Arc<SnapshotHistory>>,
    /// Optional restore-registry → coord mirror (plan
    /// `2026-07-09-runner-session-history-cloud-sync` §3.4, Phase 4). When
    /// attached, every open-record write/refresh ([`record_open`](Self::record_open))
    /// and every confirmation flip ([`confirm_session`](Self::confirm_session)
    /// — confirmation changes the record's honest `restore_tier`) hands the
    /// merged record to the emitter, which gates (`session_metadata_sync_enabled`),
    /// debounces on the material wire fields, and enqueues a
    /// `restore-record` session event via the outbox. Best-effort by
    /// construction: the emitter swallows every failure and never blocks the
    /// registry write on the network (the drain loop does the pushing).
    /// Unattached (tests, ephemeral fallbacks) → no-op.
    restore_emitter: OnceLock<Arc<RestoreRecordEmitter>>,
    /// Optional transcript-existence probe. Read at two points: write time, to
    /// stamp every snapshot-history entry ([`SnapshotSession::restorable`] /
    /// `transcript_exists`), and list time, by callers going through
    /// [`Self::probe_transcript_exists`].
    ///
    /// The registry's own SELECTION logic never consults it: `restorable_records`
    /// and the reconcile paths are unchanged by an attached probe, so it cannot
    /// add or remove restore candidates. What it does feed is the honest
    /// restore TIER the frontend classifier picks — a candidate with no
    /// transcript is still restored as a terminal, just never `--resume`d.
    /// Unattached → every read is `None` ("not probed"), never `false`.
    transcript_probe: OnceLock<Arc<dyn TranscriptProbe>>,
    /// Optional close observer (session-identity fabric Phase 3, review W2):
    /// invoked with the `claude_session_id` AFTER a record actually flips
    /// open→closed (never on a repeat/absent close). main.rs attaches a
    /// closure that calls `AiCoordRegistrar::close_session` (through a Weak,
    /// avoiding the Arc cycle with the registrar's attached store), so
    /// sniffed-registered sessions don't accrue as never-closing
    /// `coord.sessions` rows when their terminal dies. A csid the registrar
    /// never registered no-ops inside `close_session` (index miss).
    /// Unattached (tests, ephemeral fallbacks) → no-op.
    close_observer: OnceLock<CloseObserver>,
}

/// Boxed close-observer callback (see `SessionLifecycleStore::close_observer`).
/// Newtype so the store can keep `#[derive(Debug)]`.
struct CloseObserver(Box<dyn Fn(&str) + Send + Sync>);

impl std::fmt::Debug for CloseObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloseObserver")
    }
}

impl SessionLifecycleStore {
    /// Open (or initialize) the store at `path`: load the JSON snapshot, then
    /// replay the sibling write-ahead log over it.
    ///
    /// A TORN TAIL (the process died mid-append) is dropped, never fatal — see
    /// [`replay_wal`]. When the replay found anything it could not use (a torn
    /// tail or an unparsable line), the store is compacted immediately so the
    /// next append can never be concatenated onto a partial line.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let wal_path = wal_path_for(&path);
        let mut map = load_map(&path);
        let replay = replay_wal(&wal_path, &mut map);
        let store = Self {
            path,
            wal_path,
            wal: Mutex::new(WalWriter::default()),
            map: Mutex::new(map),
            snapshot_history: OnceLock::new(),
            restore_emitter: OnceLock::new(),
            transcript_probe: OnceLock::new(),
            close_observer: OnceLock::new(),
        };
        if replay.applied > 0 || replay.damaged {
            info!(
                applied = replay.applied,
                damaged = replay.damaged,
                path = %store.wal_path.display(),
                "session_lifecycle_store: replayed write-ahead log — folding into the snapshot"
            );
            store.compact();
        }
        Ok(store)
    }

    /// Attach the close observer (once, at startup) — invoked with the
    /// `claude_session_id` after every real open→closed transition
    /// ([`Self::record_close`]). See the field doc for the production wiring
    /// (coord session-row closure, fabric Phase 3 / review W2). Without it
    /// every close is local-only, the pre-Phase-3 behavior.
    pub fn attach_close_observer(&self, f: impl Fn(&str) + Send + Sync + 'static) {
        if self.close_observer.set(CloseObserver(Box::new(f))).is_err() {
            warn!("session_lifecycle_store: close observer already attached — ignoring");
        }
    }

    /// Attach the append-only snapshot-history sink (once, at startup).
    /// Without an attached sink every history hook is a no-op — the store
    /// works standalone (tests, ephemeral fallbacks).
    pub fn attach_snapshot_history(&self, history: Arc<SnapshotHistory>) {
        if self.snapshot_history.set(history).is_err() {
            warn!("session_lifecycle_store: snapshot history already attached — ignoring");
        }
    }

    /// Attach the restore-record cloud-mirror emitter (once, at startup).
    /// Without an attached emitter the mirror hooks are no-ops — the store
    /// works standalone (tests, ephemeral fallbacks).
    pub fn attach_restore_record_emitter(&self, emitter: Arc<RestoreRecordEmitter>) {
        if self.restore_emitter.set(emitter).is_err() {
            warn!("session_lifecycle_store: restore-record emitter already attached — ignoring");
        }
    }

    /// Attach the transcript-existence probe (once, at startup). It stamps the
    /// restorability tuple onto snapshot-history entries AND answers
    /// [`Self::probe_transcript_exists`] for the boot-restore projection. Without
    /// it both read "not probed" — the store works standalone.
    pub fn attach_transcript_probe(&self, probe: Arc<dyn TranscriptProbe>) {
        if self.transcript_probe.set(probe).is_err() {
            warn!("session_lifecycle_store: transcript probe already attached — ignoring");
        }
    }

    /// Ask the attached transcript probe whether `session_id` has a provider
    /// transcript on disk.
    ///
    /// `None` means NOT PROBED (no probe attached) and callers MUST treat it as
    /// UNKNOWN — never as "absent".
    ///
    /// This is what lets the boot-restore projection tell an id whose
    /// conversation exists from one whose does not. `confirmed_at` cannot: it
    /// attests only that a provider session STARTED in the terminal (its
    /// SessionStart hook fired), while the transcript is written once the session
    /// carries messages — so a session launched and never used is confirmed
    /// forever with no transcript, and `claude --resume <id>` on it fails with
    /// "No conversation found" on every boot.
    ///
    /// Also `None` when `working_dir` is absent OR BLANK. A transcript path is
    /// DERIVED from the project dir, so without one the probe has nothing to
    /// stat — [`crate::session::reconcile::DiskTranscriptIndex`] answers a bare
    /// `false` there, which is "could not determine", NOT "does not exist".
    /// Gating on that raw `false` would downgrade a working-dir-less record to
    /// terminal-only and silently lose a resumable conversation, so the
    /// unanswerable case is mapped back to UNKNOWN here.
    ///
    /// Blank matters as much as absent because `""` is reachable: the
    /// hook-confirm merge writes `prior.working_dir.clone().unwrap_or_default()`
    /// (`install_effects_producer`), and unlike `config_dir` the registry does
    /// not normalize an empty `working_dir` away. An empty project path encodes
    /// to `""`, which stats a path that can never exist — a `false` that says
    /// nothing about the session.
    ///
    /// ## Known residual false-negative paths
    ///
    /// A `Some(false)` here is "the probe looked and found nothing at the path it
    /// derived", which is not quite "no transcript exists anywhere". Two ways to
    /// derive the wrong path, both fail-CLOSED (demote to terminal-only) rather
    /// than fail-open:
    ///
    /// 1. The transcript's location follows the PROVIDER PROCESS's cwd, while
    ///    this record carries the PTY's spawn cwd. `cd` then launch, and they
    ///    diverge permanently — the hook-confirm merge deliberately preserves
    ///    `prior.working_dir` (see the 2026-07-13 stranding incident), so it
    ///    never self-heals.
    /// 2. [`crate::session::reconcile::DiskTranscriptIndex::discover`] snapshots
    ///    the config-dir set ONCE at startup and ignores the record's own
    ///    `config_dir`, so a transcript under an account dir added later is
    ///    invisible. Contrast `past_sessions::resolve_transcript_path`, which
    ///    tries the record's `config_dir` first and re-discovers per call.
    ///
    /// Both degrade a restore to terminal-only (right cwd, fresh conversation)
    /// and neither loses data — the session stays resumable by hand from the
    /// Past Sessions surface, which uses the better resolver. Answering by
    /// SESSION ID across all project dirs (the walk exists:
    /// `transcript::list_recent_sessions_all_projects`) would remove the class
    /// outright and is the follow-up worth doing before anything DESTRUCTIVE
    /// (e.g. pruning transcript-less rows) is gated on this.
    pub fn probe_transcript_exists(
        &self,
        session_id: &str,
        working_dir: Option<&str>,
    ) -> Option<bool> {
        let working_dir = working_dir.filter(|s| !s.trim().is_empty())?;
        self.transcript_probe
            .get()
            .map(|p| p.transcript_exists(session_id, Some(working_dir)))
    }

    /// Project the registry into snapshot entries, stamping restorability from
    /// the attached probe (if any). Shared by the change + heartbeat sinks so
    /// both stamp identically.
    fn snapshot_sessions(
        &self,
        snapshot: impl IntoIterator<Item = TerminalSessionRecord>,
    ) -> Vec<SnapshotSession> {
        let probe = self.transcript_probe.get();
        snapshot
            .into_iter()
            .map(|rec| SnapshotSession::from_record(&rec, probe.map(|p| p.as_ref())))
            .collect()
    }

    /// Mirror one just-written OPEN record to coord via the attached
    /// emitter, if any. Called AFTER the registry persist; best-effort by
    /// construction (the emitter gates, debounces, and swallows failures —
    /// it never fails the registry path).
    ///
    /// The transcript probe result is threaded through so the MIRRORED restore
    /// tier matches what this machine's own restore path will do. Without it the
    /// mirror advertises `restore_tier: "full"` (which `handoff` turns back into
    /// `origin: authoritative` + `confirmed_at`) for exactly the ids the local
    /// classifier now refuses to `--resume`.
    fn mirror_restore_record(&self, rec: &TerminalSessionRecord) {
        if rec.state != "open" {
            return;
        }
        if let Some(emitter) = self.restore_emitter.get() {
            emitter.emit(
                rec,
                self.probe_transcript_exists(&rec.claude_session_id, rec.working_dir.as_deref()),
            );
        }
    }

    /// Append a CHANGE entry for the records this mutation actually touched,
    /// if a history sink is attached. Called by every layout-meaningful
    /// mutation AFTER the registry write; best-effort by construction (the sink
    /// never fails the registry path).
    ///
    /// PER-RECORD, not the full registry: an append used to serialize every
    /// session in the store on every mutation, making the audit trail O(N) per
    /// spawn and the aggregate O(N²) — the same cost class the WAL removed from
    /// the registry itself. The only reader,
    /// [`crate::session::snapshot_history::read_all_snapshot_sessions`], already
    /// merges newest-`ts`-per-session-id across lines, so a delta line and a
    /// whole-registry line reconstruct identically. The 5-minute HEARTBEAT
    /// ([`Self::snapshot_heartbeat`]) still writes the FULL registry and remains
    /// the recovery anchor.
    fn snapshot_change(&self, changed: impl IntoIterator<Item = TerminalSessionRecord>) {
        if let Some(history) = self.snapshot_history.get() {
            let sessions = self.snapshot_sessions(changed);
            if sessions.is_empty() {
                return;
            }
            history.record_change(sessions);
        }
    }

    /// Append a HEARTBEAT snapshot of the full registry to the history sink,
    /// if attached. Called periodically by the liveness poll; the sink
    /// itself enforces the minimum heartbeat spacing, so calling this every
    /// poll tick is cheap.
    pub fn snapshot_heartbeat(&self) {
        // Idle compaction: the liveness poll is the store's periodic tick, so
        // fold any accumulated WAL into the snapshot here. Runs whether or not
        // a history sink is attached, and does nothing when no mutation has
        // landed since the last fold — an idle runner converges on a compact
        // snapshot without any mutation ever paying an O(N) rewrite.
        self.compact_if_dirty();
        let Some(history) = self.snapshot_history.get() else {
            return;
        };
        // Clone the records OUT of the lock before projecting: the probe stats
        // the disk, and holding the registry mutex across filesystem I/O would
        // let a slow/hung volume block every session write in the runner.
        let records: Vec<TerminalSessionRecord> = match self.map.lock() {
            Ok(m) => m.values().cloned().collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on snapshot_heartbeat");
                return;
            }
        };
        history.record_heartbeat(self.snapshot_sessions(records));
    }

    /// Insert or refresh a session by `claude_session_id`. If a record
    /// already exists, its `opened_at` is preserved; the record is always
    /// (re-)marked `open` (clearing `closed_at` / `close_reason`), its
    /// mutable fields are refreshed, and `last_seen_at` is bumped to now.
    /// A brand-new record sets `opened_at == last_seen_at == now`. Atomic-
    /// writes after mutating.
    pub fn record_open(&self, rec: TerminalSessionRecord) {
        // Normalize a stray empty/whitespace-only config_dir to None: it must
        // never become a bogus `CLAUDE_CONFIG_DIR=""` on resume, and the sticky
        // guard below must treat "no account reported" uniformly as None (so an
        // empty incoming does not clobber a known-good binding).
        let rec = TerminalSessionRecord {
            config_dir: rec.config_dir.filter(|s| !s.trim().is_empty()),
            ..rec
        };
        let now = Utc::now().timestamp_millis();
        // Captured before `rec`'s fields are moved into the entry below — used
        // after the entry mutation to evict any phantom sibling on the same
        // terminal (see the supersede scan further down).
        let new_id = rec.claude_session_id.clone();
        let new_terminal_id = rec.terminal_id.clone();
        let new_is_authoritative =
            normalize_origin(rec.origin.clone()).as_deref() == Some(ORIGIN_AUTHORITATIVE);
        // A bind that carries confirmation proves a real session owns the
        // terminal, regardless of origin (authoritative OR the launch-agnostic
        // `observed` bind). `confirmed_at` is `Option<i64>` (Copy) — safe to
        // read here before `rec`'s fields are moved into the entry below.
        let new_is_confirmed = rec.confirmed_at.is_some();
        let (merged, superseded) = {
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
            // config_dir is STICKY (like the `confirmed_at` monotonic guard
            // below): a later write that omits it — a provider that doesn't
            // report an account, a zone-move backstop, Gemini — must NOT clobber
            // a known-good account binding. Take the incoming value only when it
            // is Some (empty already normalized to None at the top of the fn).
            if rec.config_dir.is_some() {
                entry.config_dir = rec.config_dir;
            }
            entry.working_dir = rec.working_dir;
            entry.page_id = rec.page_id;
            entry.zone_index = rec.zone_index;
            entry.title = rec.title;
            // terminal_id is STICKY against an empty/placeholder incoming (like
            // the `config_dir` guard above): a write that omits the terminal —
            // an empty/whitespace-only id (a disk-only record, a provider that
            // didn't report it) — must NOT clobber a real binding. Clobbering it
            // would orphan the row AND dodge the single-tenant-terminal invariant
            // below (which is gated on a non-empty terminal_id). Take the incoming
            // value only when it names a terminal.
            if !rec.terminal_id.trim().is_empty() {
                entry.terminal_id = rec.terminal_id;
            }
            entry.provider = rec.provider;
            // Unasserted origin (zone-move backstop / boot re-assert) must not
            // degrade an authoritative binding to reconciled — preserve on
            // None. The incoming value is normalized so a legacy caller still
            // passing "pinned"/"guessed" lands as authoritative/reconciled.
            if let Some(origin) = normalize_origin(rec.origin) {
                entry.origin = Some(origin);
            }
            // Confirmation is monotonic: a provisional re-record (the spawn-time
            // writer, a zone-move backstop) must NEVER clear a confirmation a
            // provider hook already set. An incoming `Some` (a hook-sourced write
            // carrying confirmation) DOES set it. So: take the incoming value when
            // present, else preserve the existing one.
            if rec.confirmed_at.is_some() {
                entry.confirmed_at = rec.confirmed_at;
            }
            // The fleet session handle is STICKY like `config_dir`: handle
            // acquisition is a separate best-effort write
            // ([`Self::set_handle`]) and virtually no `record_open` caller
            // knows the handle — a re-record without one (zone-move backstop,
            // boot re-assert, provider hook) must never clear it. Take the
            // incoming value only when present.
            if rec.handle.is_some() {
                entry.handle = rec.handle;
            }
            entry.state = "open".to_string();
            entry.closed_at = None;
            entry.close_reason = None;
            entry.last_seen_at = now;
            let merged = entry.clone();

            // Single-tenant-terminal invariant: a PTY terminal hosts at most
            // ONE live provider session. When a new record that PROVES a real
            // session owns the terminal binds it, any OTHER still-open record on
            // the SAME terminal that no provider hook ever confirmed is a
            // superseded phantom — evict it so restore never resurrects a
            // session that never ran.
            //
            // "Proves a real session" = either AUTHORITATIVE (the runner knows
            // the id exactly — a pinned/typed `--session-id`) OR CONFIRMED of
            // ANY origin. The confirmed arm is what closes the launch-agnostic
            // `observed`-orphan class: a process-anchored, uniquely-correlated
            // `observed` bind is confirmed the moment its transcript is found,
            // and that confirmation proves the terminal's real session — so its
            // unconfirmed siblings are phantoms, evicted here while the terminal
            // is still alive.
            //
            // The split this fixes: the always-on identity seam records a fresh
            // pinned id at SHELL spawn, but an account/CLI launcher then TYPES
            // `claude --session-id <its own id>` into that shell, so the seam's
            // row and the launcher's row bind the same terminal under two ids —
            // the seam row is the orphan (unconfirmed, no transcript).
            //
            // Two eviction arms, distinguished so the registry + logs stay
            // readable:
            //   * UNCONFIRMED sibling → `"superseded"` — the phantom seam row
            //     above. Retired by ANY authoritative-or-confirmed incoming.
            //   * CONFIRMED sibling → `"superseded-terminal-reuse"` — a PRIOR
            //     real run whose exit-close never fired (crash / missed poll) on
            //     a PTY that a long-lived shell then reused for the next `claude`
            //     run. A terminal hosts ONE live session at a time, so when a NEW
            //     CONFIRMED session binds it the prior confirmed run is a
            //     superseded previous tenant and must be closed — otherwise these
            //     accumulate as stale `open` confirmed rows and collapse onto one
            //     terminal at restore (the P4 mass-strand). Gated on
            //     `new_is_confirmed`: only a NEW CONFIRMED binding retires a prior
            //     confirmed sibling — an authoritative-but-UNCONFIRMED incoming
            //     (itself not yet proven) must NOT evict a confirmed row.
            let mut superseded: Vec<TerminalSessionRecord> = Vec::new();
            if (new_is_authoritative || new_is_confirmed) && !new_terminal_id.is_empty() {
                for other in m.values_mut() {
                    if other.claude_session_id == new_id
                        || other.terminal_id != new_terminal_id
                        || other.state != "open"
                    {
                        continue;
                    }
                    if other.confirmed_at.is_none() {
                        other.state = "closed".to_string();
                        other.closed_at = Some(now);
                        other.close_reason = Some("superseded".to_string());
                        superseded.push(other.clone());
                        info!(
                            terminal_id = %new_terminal_id,
                            superseded = %other.claude_session_id,
                            by = %new_id,
                            "session-restore: evicted unconfirmed phantom sibling — new authoritative session bound the terminal"
                        );
                    } else if new_is_confirmed {
                        other.state = "closed".to_string();
                        other.closed_at = Some(now);
                        other.close_reason = Some("superseded-terminal-reuse".to_string());
                        superseded.push(other.clone());
                        info!(
                            terminal_id = %new_terminal_id,
                            superseded = %other.claude_session_id,
                            by = %new_id,
                            "session-restore: retired prior confirmed session on a reused terminal — new confirmed session bound it"
                        );
                    }
                }
            }
            // O(1) durable write: the merged record plus any sibling this open
            // superseded — never the whole map.
            let mut deltas = Vec::with_capacity(1 + superseded.len());
            deltas.push(LifecycleDelta::Upsert {
                rec: Box::new(merged.clone()),
            });
            deltas.extend(superseded.iter().map(|r| LifecycleDelta::Upsert {
                rec: Box::new(r.clone()),
            }));
            self.persist(m, &deltas);
            (merged, superseded)
        };
        self.snapshot_change(std::iter::once(merged.clone()).chain(superseded));
        // Phase 4 cloud mirror — emit AFTER the durable local write, from
        // the MERGED record (origin/confirmation preservation applied).
        self.mirror_restore_record(&merged);
    }

    /// Mark an open session closed. No-op (no error) if the session is
    /// absent or already closed.
    ///
    /// Credential hygiene (Task 5): when this close ends the LAST open session
    /// for its workdir, the workdir's coord-mcp proxy nonce is revoked and its
    /// app-data session-restore config is reaped
    /// ([`crate::coord_mcp::release_workdir_on_session_close`]) — a closed
    /// session's credential must not stay live. Skipped for account-migration
    /// closes (the session continues under a new record with the same workdir)
    /// and when a sibling open session still shares the workdir.
    pub fn record_close(&self, claude_session_id: &str, reason: &str) {
        let now = Utc::now().timestamp_millis();
        let (closed, closed_workdir, workdir_still_in_use) = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on record_close");
                    return;
                }
            };
            let closed = match m.get_mut(claude_session_id) {
                Some(rec) if rec.state == "open" => {
                    rec.state = "closed".to_string();
                    rec.closed_at = Some(now);
                    rec.close_reason = Some(reason.to_string());
                    rec.clone()
                }
                _ => return, // absent or already closed — nothing to flush
            };
            let workdir = closed.working_dir.clone();
            // Answered under the lock rather than off a full-map clone — the
            // clone existed only to serve this one query.
            let still_in_use = workdir.as_deref().is_some_and(|wd| {
                m.values()
                    .any(|r| r.state == "open" && r.working_dir.as_deref() == Some(wd))
            });
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(closed.clone()),
                }],
            );
            (closed, workdir, still_in_use)
        };
        self.snapshot_change(std::iter::once(closed));
        // Fabric Phase 3 (review W2): notify AFTER the durable local write,
        // outside the map lock, and only on a REAL open→closed transition
        // (the early return above skips repeat/absent closes). Best-effort:
        // the production observer enqueues a coord `Closed` outbox row.
        if let Some(obs) = self.close_observer.get() {
            (obs.0)(claude_session_id);
        }

        // Credential hygiene: drop the coord-mcp device nonce bound to this
        // workdir once no OPEN record still uses it. Runs after the observer
        // notify above so a real close is always reported to coord, even when
        // the migration early-return below keeps the nonce alive.
        if reason == crate::terminal::account_migration::CLOSE_REASON_MIGRATED {
            return; // the session lives on under a new record — keep its nonce
        }
        if let Some(wd) = closed_workdir {
            if !workdir_still_in_use {
                crate::coord_mcp::release_workdir_on_session_close(&wd);
            }
        }
    }

    /// Bump `last_seen_at` on a present session. No-op (no write) if absent.
    pub fn touch(&self, claude_session_id: &str) {
        let now = Utc::now().timestamp_millis();
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on touch");
                return;
            }
        };
        let touched = match m.get_mut(claude_session_id) {
            Some(rec) => {
                rec.last_seen_at = now;
                rec.clone()
            }
            None => return,
        };
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(touched),
            }],
        );
    }

    /// Update the persisted `title` of the OPEN record hosted by
    /// `terminal_id` (plan
    /// `2026-07-03-runner-session-tracking-drift-and-guardrails` Phase 3
    /// item 4). Title renames (`terminal_set_title` → OSC 0 echo, the `/name`
    /// skill, an operator rename) previously mutated only the in-memory
    /// `TerminalSession` title, so the durable registry stayed frozen at the
    /// spawn-time title and a restart restored the pane under a stale name.
    /// Keyed by terminal id because the rename call sites hold only that —
    /// same resolution [`Self::find_open_by_terminal`] performs. No-op (no
    /// write) if no open record references the terminal, or the title is
    /// already current.
    pub fn update_title_by_terminal(&self, terminal_id: &str, title: &str) {
        let changed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on update_title_by_terminal");
                    return;
                }
            };
            let Some(rec) = m
                .values_mut()
                .find(|r| r.state == "open" && r.terminal_id == terminal_id)
            else {
                return; // no open record hosts this terminal — nothing to flush
            };
            if rec.title.as_deref() == Some(title) {
                return; // already current — nothing to flush
            }
            rec.title = Some(title.to_string());
            let changed = rec.clone();
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(changed.clone()),
                }],
            );
            changed
        };
        self.snapshot_change(std::iter::once(changed));
    }

    /// Re-point an OPEN record at the terminal that now hosts it, without
    /// touching any liveness or provenance field.
    ///
    /// A cold restore recreates the PTY under a fresh ephemeral terminal id, so
    /// the record's `terminal_id` goes stale the moment the old process is
    /// gone. Only the VERIFIED-resume path used to re-assert it (via
    /// `record_open`), which left every `terminal-only` / `quarantine` record
    /// permanently pointing at a dead id — so the next restore pass could not
    /// recognise its own work and cold-created ANOTHER terminal for the same
    /// record, every pass, forever. (Observed 2026-07-22: 48 stale records on
    /// one page had leaked ~330 orphan PTYs across ~7 restore passes.)
    ///
    /// `record_open` is the wrong tool for that rebind: it refreshes
    /// `last_seen_at`, which is exactly what made ghost rows immortal. This
    /// updates the binding ONLY — `last_seen_at`, `state`, `origin`,
    /// `confirmed_at` and `restore_pending_at` are all left untouched, so a
    /// stale row still ages out of the restorable set on schedule.
    ///
    /// No-op (no write, no flush) when the session is absent, not `open`, or
    /// already bound to `terminal_id`.
    pub fn rebind_terminal(&self, claude_session_id: &str, terminal_id: &str, zone_index: i32) {
        let changed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on rebind_terminal");
                    return;
                }
            };
            let Some(rec) = m.get_mut(claude_session_id) else {
                return; // unknown session — nothing to rebind
            };
            if rec.state != "open" {
                return; // closed/exited records are not restore targets
            }
            if rec.terminal_id == terminal_id && rec.zone_index == zone_index {
                return; // already current — no write, no flush
            }
            rec.terminal_id = terminal_id.to_string();
            rec.zone_index = zone_index;
            let changed = rec.clone();
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(changed.clone()),
                }],
            );
            changed
        };
        self.snapshot_change(std::iter::once(changed));
    }

    /// Mirror a terminal-page move into the durable registry, resolved by
    /// `terminal_id`. The `POST /terminals/{id}/move` surface mutates the
    /// in-memory `TerminalSession.page_id`; without this flush the durable
    /// record stays frozen at the spawn-time page, so a restart would restore
    /// the pane on its original page rather than where the operator moved it.
    /// Sibling of [`Self::update_title_by_terminal`]. No-op (no write) if no
    /// open record references the terminal, or the page is already current.
    pub fn update_page_by_terminal(&self, terminal_id: &str, page_id: &str) {
        let changed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on update_page_by_terminal");
                    return;
                }
            };
            let Some(rec) = m
                .values_mut()
                .find(|r| r.state == "open" && r.terminal_id == terminal_id)
            else {
                return; // no open record hosts this terminal — nothing to flush
            };
            if rec.page_id == page_id {
                return; // already current — nothing to flush
            }
            rec.page_id = page_id.to_string();
            let changed = rec.clone();
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(changed.clone()),
                }],
            );
            changed
        };
        self.snapshot_change(std::iter::once(changed));
    }

    /// Mark a present session as restore-pending (a boot-restore is about to
    /// type / has typed `claude --resume` and the handshake is not yet
    /// verified). While the marker is set the liveness poll skips the record
    /// entirely except for a confident-alive observation — see [`classify`].
    /// No-op (no write) if the session is absent.
    pub fn mark_restore_pending(&self, claude_session_id: &str) {
        let now = Utc::now().timestamp_millis();
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on mark_restore_pending");
                return;
            }
        };
        let changed = match m.get_mut(claude_session_id) {
            Some(rec) => {
                rec.restore_pending_at = Some(now);
                rec.clone()
            }
            None => return,
        };
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
    }

    /// Clear a session's restore-pending marker (resume handshake verified —
    /// the session is live again). No-op (no write) if the session is absent
    /// or the marker is already clear.
    pub fn clear_restore_pending(&self, claude_session_id: &str) {
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on clear_restore_pending");
                return;
            }
        };
        let changed = match m.get_mut(claude_session_id) {
            Some(rec) if rec.restore_pending_at.is_some() => {
                rec.restore_pending_at = None;
                rec.clone()
            }
            _ => return, // absent or already clear — nothing to flush
        };
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
    }

    /// Flip a present session from PROVISIONAL to CONFIRMED (a provider's
    /// SessionStart hook fired for this session id — session-restore-redesign
    /// Phase 2). Stamps `confirmed_at` with now iff it is not already set
    /// (confirmation is monotonic — the first hook wins; a later resume hook is
    /// a no-op). No-op (no write) if the session is absent or already confirmed.
    ///
    /// Phase 4 reads `confirmed_at` (OR a real transcript on disk) to decide
    /// auto-resume vs treat-as-plain-shell; this method just records the signal.
    pub fn confirm_session(&self, claude_session_id: &str) {
        let now = Utc::now().timestamp_millis();
        let confirmed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on confirm_session");
                    return;
                }
            };
            let confirmed = match m.get_mut(claude_session_id) {
                Some(rec) if rec.confirmed_at.is_none() => {
                    rec.confirmed_at = Some(now);
                    rec.clone()
                }
                _ => return, // absent or already confirmed — nothing to flush
            };
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(confirmed.clone()),
                }],
            );
            confirmed
        };
        self.snapshot_change(std::iter::once(confirmed.clone()));
        // Phase 4 cloud mirror — a confirmation flip changes the record's
        // honest restore tier (terminal_only → full for a Full-tier
        // provider), which is a material wire-field change.
        self.mirror_restore_record(&confirmed);
    }

    /// Persist the coord-minted stable fleet session handle (`fsh_…`) onto the
    /// record for `claude_session_id` (session-identity fabric Phase 1).
    ///
    /// SERVER WINS: coord's `coord.session_handles` registry rebinds on the
    /// durable `claude_session_id` (UNIQUE anchor), so when the local value
    /// diverges from what the server returned, the local file drifted —
    /// overwrite it and warn. Writes ONLY on change; a matching handle is a
    /// no-op, and an ABSENT record (e.g. an AI subprocess session that has no
    /// terminal-grid row) is a debug-logged no-op — this method never creates
    /// records (a handle-only row would be a phantom restore candidate).
    pub fn set_handle(&self, claude_session_id: &str, handle: &str) {
        {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on set_handle");
                    return;
                }
            };
            let Some(rec) = m.get_mut(claude_session_id) else {
                debug!(
                    claude_session_id,
                    handle,
                    "session_lifecycle_store: no record for session — handle not persisted locally (registry is server-authoritative)"
                );
                return;
            };
            match rec.handle.as_deref() {
                Some(existing) if existing == handle => return, // unchanged — no write
                Some(existing) => warn!(
                    claude_session_id,
                    local = existing,
                    server = handle,
                    "session_lifecycle_store: local session handle diverged from coord registry — server wins"
                ),
                None => info!(
                    claude_session_id,
                    handle, "session_lifecycle_store: bound fleet session handle"
                ),
            }
            rec.handle = Some(handle.to_string());
            let changed = rec.clone();
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(changed),
                }],
            );
        }
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
        let rekeyed = {
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
            m.insert(new_id.to_string(), rec.clone());
            self.persist(
                m,
                &[
                    LifecycleDelta::Remove {
                        id: old_id.to_string(),
                    },
                    LifecycleDelta::Upsert {
                        rec: Box::new(rec.clone()),
                    },
                ],
            );
            rec
        };
        self.snapshot_change(std::iter::once(rekeyed));
    }

    /// Remove a record outright (session-restore-redesign Phase 4 reconcile
    /// phantom-prune). Unlike [`record_close`], which leaves a `closed` row that
    /// the restore-grace logic might still resurrect, this DELETES the entry so a
    /// phantom provisional record (authoritative-but-unconfirmed, no live process
    /// and no transcript) can never auto-resume on any future boot. No-op (no
    /// write) if the id is absent.
    /// A removal appends only a `Remove` delta and NO snapshot-history entry: a
    /// `SnapshotSession` can only express a record that exists, and the history
    /// reader ([`crate::session::snapshot_history::read_all_snapshot_sessions`])
    /// merges the newest line per id — so even under the old full-registry
    /// append, a removed id kept showing up from the older lines that still
    /// carried it. Omitting the entry is therefore behavior-preserving.
    pub fn remove_session(&self, claude_session_id: &str) {
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on remove_session");
                return;
            }
        };
        if m.remove(claude_session_id).is_none() {
            return; // absent — nothing to flush
        }
        self.persist(
            m,
            &[LifecycleDelta::Remove {
                id: claude_session_id.to_string(),
            }],
        );
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

    /// Like [`Self::find_open_by_terminal`], but only returns a CONFIRMED
    /// record — one whose `confirmed_at` is set (a provider SessionStart hook
    /// fired, or a start-anchored `observed`/`reconciled` bind proved a real
    /// session exists here).
    ///
    /// This is what `terminal_list`'s `sessionIdsByTerminal` map uses to
    /// re-attach `claudeSessionId` to reconnected/other tabs. The confirmed
    /// gate is load-bearing: the spawn-time identity seam (`apply_identity_seam`)
    /// mints a fresh session id and records an AUTHORITATIVE-but-PROVISIONAL
    /// `open` row for EVERY terminal — including a plain interactive shell that
    /// never runs a provider (a phantom), and a non-pinned launch whose minted
    /// uuid is NOT the id the process actually runs under. Surfacing an
    /// unconfirmed id would bind a phantom / foreign / never-used id onto the
    /// tab: the per-session PR dropdown would poll coord for a non-existent
    /// session every minute, session-scoped UI-bridge selectors would key off a
    /// dead id, and — because the transcript-poll capture stops as soon as a tab
    /// carries ANY id (`useTabSessionIdCapture`) — the tab could be PERMANENTLY
    /// mis-bound to the seam's uuid instead of the real run id. Gating on
    /// `confirmed_at` mirrors the reconcile phantom gate
    /// ([`crate::session::reconcile::is_phantom_record`]) and the restore
    /// classifier, so only a real, correctly-identified session lights up
    /// session-scoped UI.
    pub fn find_confirmed_open_by_terminal(
        &self,
        terminal_id: &str,
    ) -> Option<TerminalSessionRecord> {
        self.find_open_by_terminal(terminal_id)
            .filter(|r| r.confirmed_at.is_some())
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

    /// Every session id the registry knows, OPEN or CLOSED. Used by the
    /// disk-only transcript-derived restore net (session-restore-redesign
    /// Phase 3 / G3) to exclude ANY id the registry already tracks: a
    /// restorable row wins on layout (real page/zone), and a NON-restorable row
    /// (user-closed, `no-terminal` orphan, stale ghost) already encodes a
    /// deliberate "do not restore" decision that the disk-only net must honor —
    /// a fresh transcript mtime must never resurrect a session the user closed.
    /// Only genuinely registry-ABSENT on-disk sessions (the true capture-miss)
    /// survive this exclusion.
    pub fn all_ids(&self) -> HashSet<String> {
        match self.map.lock() {
            Ok(m) => m.keys().cloned().collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on all_ids");
                HashSet::new()
            }
        }
    }

    /// Ids of every record whose `state == "closed"`.
    ///
    /// Used to build the disk-only-net exclusion set in
    /// `terminal_session_list_open`: the net excludes the restorable-set ids
    /// UNION these closed ids, so the ONLY registry rows that can leak into the
    /// quarantined disk-only candidate set are `open` rows dropped by the
    /// restorable grace gate (the crash-restart / Phase-1 victims). A closed
    /// row — user-closed (`no-terminal`/explicit), or a grace-EXPIRED
    /// `pty-exit`/`poll-dead` — always stays excluded so its transcript is
    /// never resurrected (the don't-resurrect-a-closed-tab property the old
    /// `all_ids` exclusion was buying).
    pub fn closed_ids(&self) -> HashSet<String> {
        match self.map.lock() {
            Ok(m) => m
                .values()
                .filter(|r| r.state == "closed")
                .map(|r| r.claude_session_id.clone())
                .collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on closed_ids");
                HashSet::new()
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

    /// Clone of EVERY record — OPEN and CLOSED alike (closed rows are retained
    /// until the 24 h prune). Unlike [`open_records`](Self::open_records) this
    /// applies no state filter. Used by the DISPLAY-only "previous sessions"
    /// listing ([`crate::session::past_sessions`]), which merges these with the
    /// snapshot-history reader; it is NOT a restore surface.
    pub fn all_records(&self) -> Vec<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => m.values().cloned().collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on all_records");
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
    /// `anchor - last_seen_at <= RESTORABLE_OPEN_ANCHOR_GRACE_MS`, where the
    /// `anchor` is the registry's LAST moment of genuine session life — the max
    /// of every row's `last_seen_at`/`closed_at`, NOT wall-clock now.
    ///
    /// A wall-clock rule (`now - last_seen <= grace`) would restore NOTHING
    /// after any downtime longer than the grace (a crash, an hours-later boot);
    /// the row-relative anchor is downtime-proof — when the whole registry dies
    /// together the anchor equals the crash instant, so `anchor - last_seen ≈ 0`
    /// and the cohort survives regardless of how long the runner was down.
    ///
    /// The anchor is derived ONLY from real session rows, never a boot marker.
    /// `prior_marker_at` is the prior shutdown marker's `at` — the shutdown
    /// instant on a clean exit, but the crashed process's OWN boot instant on a
    /// crash (see [`crate::session::shutdown_marker`]). Worse, an INTERMEDIATE
    /// boot/auto-restart during the downtime rewrites that marker to its own
    /// later boot time. Feeding that later marker into the anchor is exactly
    /// what pulled the 2026-07-19 anchor ~1h46m past the crash band and stranded
    /// 81 confirmed sessions. So the marker contributes to the anchor ONLY on a
    /// CLEAN boot (`boot_was_clean == true`), where it is an honest
    /// last-moment-of-life signal that correctly excludes a stale lone ghost; on
    /// an unclean (crash) boot it is dropped and the crash rows supply the
    /// anchor themselves. A genuinely-newer session row (one that really was
    /// alive later) legitimately advances the anchor; a crash cohort more than
    /// `grace` older than that is stale and excluded — but is still offered,
    /// quarantined, through the disk-only transcript net (see
    /// `commands::terminal::terminal_session_list_open`), so it is never lost.
    ///
    /// ## One-live-session-per-terminal (open rows)
    ///
    /// A PTY hosts at most ONE live provider session, but the durable registry
    /// can hold several `open` rows on one `terminal_id` (P4: a reused terminal
    /// whose prior runs' exit-closes never fired — see
    /// [`Self::repair_terminal_id_collisions`]). Restore maps a terminal to a
    /// single session, so returning N open rows on one terminal collapses them.
    /// The persistent boot repair fixes the durable store, but it must not RACE
    /// the frontend's on-mount restore read, so the read is deduped here too:
    /// among admitted `open` rows sharing a non-empty `terminal_id` we keep the
    /// single most-authoritative one (CONFIRMED over unconfirmed, then newest
    /// `last_seen_at`, then newest `opened_at`) and drop the rest. This is
    /// idempotent with the boot repair and immunizes the read regardless of when
    /// the repair persists.
    pub fn restorable_records(
        &self,
        now_ms: i64,
        prior_marker_at: Option<i64>,
        boot_was_clean: bool,
    ) -> Vec<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => {
                // The registry's last moment of genuine session life: the max
                // over every row's last_seen_at and every closed row's
                // closed_at, PLUS the prior shutdown marker ONLY on a clean
                // boot. On an unclean (crash) boot the marker is a boot artifact
                // (the crashed/intermediate process's boot instant), not session
                // liveness, so it is excluded and the rows supply the anchor.
                let anchor = m
                    .values()
                    .flat_map(|r| [Some(r.last_seen_at), r.closed_at])
                    .chain(std::iter::once(if boot_was_clean {
                        prior_marker_at
                    } else {
                        None
                    }))
                    .flatten()
                    .max();
                let admitted: Vec<TerminalSessionRecord> = m
                    .values()
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
                    .collect();
                dedupe_open_by_terminal(admitted)
            }
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on restorable_records");
                Vec::new()
            }
        }
    }

    /// Drop `closed` records closed > 24h ago and `open` records not seen
    /// for > 7d — EXCEPT any record whose terminal is still live.
    ///
    /// `live_terminal_ids` is the caller's current live-terminal view (the
    /// liveness poll's own `TerminalManager::list()` for this tick — the same
    /// source `classify` runs against, so prune and close-detection can never
    /// disagree about what "live" means). A closed row whose terminal still
    /// lives is the registry's only proof that terminal was ever recorded:
    /// deleting it on wall-clock retention while the terminal survives
    /// converts a "recorded then closed" terminal into an apparent "never
    /// recorded" one (measured live 2026-07: 234 of 255 live terminals looked
    /// never-recorded because their closed rows had been retention-pruned).
    /// The same gate covers the open-stale path: a not-seen-in-7d row whose
    /// terminal is still alive is a liveness-tracking gap to surface, not
    /// garbage to silently delete. Retention clocks start mattering only once
    /// the terminal itself is gone.
    ///
    /// The "recorded then closed" proof needs only the NEWEST closed row per
    /// live terminal, so only that row is exempt from retention; older closed
    /// rows under the same live terminal age out normally. Without this bound
    /// a long-lived terminal hosting many sequential claude runs would retain
    /// (and rewrite) every closed row for the primary's whole uptime.
    /// Atomic-writes only if something changed.
    pub fn prune(&self, now: i64, live_terminal_ids: &HashSet<String>) {
        {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on prune");
                    return;
                }
            };
            // Newest closed_at per live terminal — the one closed row per
            // terminal the liveness gate keeps unconditionally.
            let mut newest_closed: HashMap<&str, i64> = HashMap::new();
            for rec in m.values() {
                if rec.state == "closed" && live_terminal_ids.contains(&rec.terminal_id) {
                    if let Some(closed_at) = rec.closed_at {
                        let e = newest_closed
                            .entry(rec.terminal_id.as_str())
                            .or_insert(closed_at);
                        *e = (*e).max(closed_at);
                    }
                }
            }
            let newest_closed: HashMap<String, i64> = newest_closed
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            let before = m.len();
            m.retain(|_, rec| {
                let terminal_live = live_terminal_ids.contains(&rec.terminal_id);
                // A non-closed record whose terminal is STILL LIVE is never
                // pruned — it must outlive its terminal, whatever its age.
                if terminal_live && rec.state != "closed" {
                    return true;
                }
                if rec.state == "closed" {
                    match rec.closed_at {
                        Some(closed_at) => {
                            // Live terminal: its newest closed row is the
                            // recorded-then-closed proof — exempt from
                            // retention. Older siblings age out normally.
                            if terminal_live
                                && newest_closed.get(&rec.terminal_id) == Some(&closed_at)
                            {
                                return true;
                            }
                            now - closed_at <= CLOSED_RETENTION_MS
                        }
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
        }
        // A prune drops an unbounded number of rows and already scanned the
        // whole map, so it is O(N) by construction — fold straight into a fresh
        // snapshot rather than appending N `Remove` deltas. Runs on the slow
        // liveness poll, never on the spawn path.
        self.compact();
    }

    /// One-time boot repair for the P4 registry corruption (plan
    /// `2026-07-19-runner-session-restore-mass-strand-and-git-popup`, Phase 3):
    /// many CONFIRMED `open` rows accumulated onto a SINGLE reused `terminal_id`
    /// (a long-lived PTY hosting sequential `claude` runs, each prior run's
    /// exit-close never firing), so restore mapped N rows onto one terminal and
    /// they collapsed.
    ///
    /// For each group of `open` rows sharing a non-empty `terminal_id`, KEEP the
    /// newest live tenant — max `last_seen_at`, tie-broken by `opened_at` — and
    /// CLOSE the older ones with `close_reason = "superseded-terminal-reuse"`
    /// (the same reason `record_open`'s confirmed-reuse eviction stamps, so the
    /// boot repair and the ongoing invariant are indistinguishable in the
    /// registry). A terminal hosts ONE live session, so this collapses to the
    /// live tenant rather than fanning out synthetic ids — 53 of the 54 observed
    /// rows are DEAD prior runs and preserving them as distinct restorable rows
    /// would resurrect 53 phantoms.
    ///
    /// Idempotent: a healthy registry (≤1 open row per terminal) closes nothing.
    /// Returns the number of rows closed (INFO-logged by the boot caller — no
    /// silent cap).
    pub fn repair_terminal_id_collisions(&self) -> usize {
        let now = Utc::now().timestamp_millis();
        let (closed_recs, closed) = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on repair_terminal_id_collisions");
                    return 0;
                }
            };
            // Group the OPEN rows by their (non-empty) terminal_id.
            let mut by_terminal: HashMap<String, Vec<String>> = HashMap::new();
            for rec in m.values() {
                if rec.state == "open" && !rec.terminal_id.trim().is_empty() {
                    by_terminal
                        .entry(rec.terminal_id.clone())
                        .or_default()
                        .push(rec.claude_session_id.clone());
                }
            }
            let mut closed = 0usize;
            let mut closed_recs: Vec<TerminalSessionRecord> = Vec::new();
            for (_terminal, ids) in by_terminal {
                if ids.len() < 2 {
                    continue; // no collision on this terminal
                }
                // Rank the colliding rows by restore authority — keep [0]
                // (the row most likely to BE the live session), close the rest.
                // A CONFIRMED row must always outrank an unconfirmed one: the
                // record_open invariant is that an unconfirmed row never evicts
                // a confirmed one, and a naive newest-`last_seen` sort would
                // violate it (a later zone-move / boot re-assert can give an
                // unconfirmed phantom a marginally newer `last_seen_at` than the
                // real confirmed session, so we'd keep the phantom and close —
                // and thereby exclude from the disk-only rescue net — the real
                // one). Same key as the read-time dedupe in `restorable_records`.
                let mut ranked = ids;
                ranked.sort_by(|a, b| {
                    let ka = m.get(a.as_str()).map(open_authority_key).unwrap_or((
                        false,
                        i64::MIN,
                        i64::MIN,
                    ));
                    let kb = m.get(b.as_str()).map(open_authority_key).unwrap_or((
                        false,
                        i64::MIN,
                        i64::MIN,
                    ));
                    kb.cmp(&ka)
                });
                for id in ranked.into_iter().skip(1) {
                    if let Some(rec) = m.get_mut(id.as_str()) {
                        rec.state = "closed".to_string();
                        rec.closed_at = Some(now);
                        rec.close_reason = Some("superseded-terminal-reuse".to_string());
                        closed_recs.push(rec.clone());
                        closed += 1;
                    }
                }
            }
            if closed == 0 {
                return 0; // nothing collapsed — skip the write
            }
            (closed_recs, closed)
        };
        // Boot-time bulk repair — one compaction rather than N deltas.
        self.compact();
        self.snapshot_change(closed_recs);
        closed
    }

    /// Durably record one mutation as O(1) appended WAL lines.
    ///
    /// MUST be called while the caller still holds the `map` lock (the guard is
    /// taken by reference purely to make that a compile-time requirement) — see
    /// the `wal` field doc for why the ordering matters. Returns whether the WAL
    /// has grown past [`WAL_COMPACT_APPENDS`], which the caller folds into the
    /// snapshot AFTER releasing the map lock (see [`Self::persist`]).
    ///
    /// Best-effort exactly like the whole-map rewrite it replaces: a write
    /// failure is logged, not propagated — the in-memory map still reflects the
    /// mutation for this process. EVERY such failure sets
    /// [`WalWriter::dirty`], so the idle compaction still folds the map to disk
    /// rather than leaving the mutation dirty-but-uncounted.
    #[must_use = "a full WAL must be compacted once the map lock is released"]
    fn wal_append(
        &self,
        _guard: &std::sync::MutexGuard<'_, HashMap<String, TerminalSessionRecord>>,
        deltas: &[LifecycleDelta],
    ) -> bool {
        if deltas.is_empty() {
            return false;
        }
        let mut w = match self.wal.lock() {
            Ok(w) => w,
            Err(e) => {
                // A poisoned WAL lock is unrecoverable for BOTH the append and
                // the compaction that would rescue it, so there is no dirty flag
                // to usefully set here — the map stays authoritative in memory.
                warn!(error = %e, "session_lifecycle_store: WAL lock poisoned — mutation kept in memory only");
                return false;
            }
        };
        // One buffer, one `write_all`: a delta line must reach the file whole or
        // not at all as far as the OS is concerned, so a crash can only ever
        // truncate the tail (which `replay_wal` drops).
        let mut buf = Vec::with_capacity(deltas.len() * 512);
        for d in deltas {
            match serde_json::to_writer(&mut buf, d) {
                Ok(()) => buf.push(b'\n'),
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: WAL serialize failed — delta dropped");
                    w.dirty = true;
                    return false;
                }
            }
        }
        if w.file.is_none() {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.wal_path)
            {
                Ok(f) => w.file = Some(f),
                Err(e) => {
                    warn!(
                        error = %e,
                        path = %self.wal_path.display(),
                        "session_lifecycle_store: WAL open failed — mutation kept in memory only"
                    );
                    w.dirty = true;
                    return false;
                }
            }
        }
        if w.file.is_none() {
            // Unreachable in practice (the block above either opened the file or
            // returned), but the dirty flag makes the impossible case honest
            // rather than silently uncounted.
            w.dirty = true;
            return false;
        }
        let file = w.file.as_mut().expect("WAL handle present (checked above)");
        if let Err(e) = file.write_all(&buf) {
            warn!(
                error = %e,
                path = %self.wal_path.display(),
                "session_lifecycle_store: WAL append failed — mutation kept in memory only"
            );
            // Drop the handle so the next append re-opens rather than keeping
            // writing through a broken descriptor.
            w.file = None;
            w.dirty = true;
            return false;
        }
        w.appends += deltas.len();
        w.appends >= WAL_COMPACT_APPENDS
    }

    /// Fold the WAL into the JSON snapshot and truncate it.
    ///
    /// Ordering is what makes this crash-safe: the snapshot is written and
    /// renamed into place FIRST, and only then is the WAL truncated. A crash in
    /// between replays deltas the snapshot already contains — idempotent — while
    /// the reverse order would lose them outright.
    ///
    /// Takes `map` then `wal` (the store-wide lock order), so no append can
    /// straddle the truncation.
    pub fn compact(&self) {
        let m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on compact");
                return;
            }
        };
        let mut w = match self.wal.lock() {
            Ok(w) => w,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: WAL lock poisoned on compact");
                return;
            }
        };
        if let Err(e) = write_map(&self.path, &m) {
            warn!(
                error = %e,
                path = %self.path.display(),
                "session_lifecycle_store: snapshot rewrite failed — WAL kept, nothing lost"
            );
            return; // keep the WAL: it is still the durable record
        }
        // Snapshot is on disk and renamed into place — the WAL is now redundant.
        // The rewrite serialized the WHOLE map, so it also captured any mutation
        // that failed to reach the WAL (see [`WalWriter::dirty`]).
        w.file = None;
        w.dirty = false;
        match std::fs::File::create(&self.wal_path) {
            Ok(_) => w.appends = 0,
            Err(e) => warn!(
                error = %e,
                path = %self.wal_path.display(),
                "session_lifecycle_store: WAL truncate failed — replay will re-apply committed deltas (idempotent)"
            ),
        }
    }

    /// Fold the WAL into the snapshot when there is anything to fold. Called
    /// from the liveness poll's heartbeat so an idle runner converges on a
    /// compact snapshot without any mutation ever paying an O(N) rewrite.
    ///
    /// "Anything to fold" is appended deltas OR a mutation that never reached
    /// the WAL at all ([`WalWriter::dirty`]) — gating on `appends` alone would
    /// silently drop the latter.
    fn compact_if_dirty(&self) {
        let dirty = matches!(self.wal.lock(), Ok(w) if w.appends > 0 || w.dirty);
        if dirty {
            self.compact();
        }
    }

    /// Durably record one mutation, compacting when the WAL has grown past its
    /// bound. Call with the map guard still held; it is consumed so the
    /// compaction (which re-takes the map lock) can only run after release.
    fn persist(
        &self,
        guard: std::sync::MutexGuard<'_, HashMap<String, TerminalSessionRecord>>,
        deltas: &[LifecycleDelta],
    ) {
        let full = self.wal_append(&guard, deltas);
        drop(guard);
        if full {
            self.compact();
        }
    }
}

/// Restore-authority key for an `open` row on a contested terminal, greatest =
/// most likely to be the LIVE session. A CONFIRMED row (a provider hook proved
/// a real session ran) outranks any unconfirmed one regardless of timestamps;
/// among equal confirmation, the newest `last_seen_at` then `opened_at` wins.
/// Shared by the read-time dedupe ([`dedupe_open_by_terminal`]) and the boot
/// repair ([`SessionLifecycleStore::repair_terminal_id_collisions`]) so both
/// agree on which colliding row to keep.
fn open_authority_key(rec: &TerminalSessionRecord) -> (bool, i64, i64) {
    (rec.confirmed_at.is_some(), rec.last_seen_at, rec.opened_at)
}

/// Collapse admitted `open` rows that share a non-empty `terminal_id` down to
/// the single most-authoritative one (see [`open_authority_key`]). A PTY hosts
/// at most one live session, so the restore read must never return N open rows
/// for one terminal (P4 collision) — that would collapse them onto one terminal
/// at restore. This is the read-side guard that makes the restore immune to a
/// registry collision REGARDLESS of whether the persistent boot repair has run
/// yet (it is spawn-delayed and can race the frontend's on-mount read). Closed
/// rows and open rows with an empty `terminal_id` (uncorrelatable) pass through
/// untouched; input order is otherwise preserved.
fn dedupe_open_by_terminal(records: Vec<TerminalSessionRecord>) -> Vec<TerminalSessionRecord> {
    // Winner id per contested terminal_id.
    let mut winner: HashMap<&str, &TerminalSessionRecord> = HashMap::new();
    for r in &records {
        if r.state != "open" || r.terminal_id.trim().is_empty() {
            continue;
        }
        winner
            .entry(r.terminal_id.as_str())
            .and_modify(|best| {
                if open_authority_key(r) > open_authority_key(best) {
                    *best = r;
                }
            })
            .or_insert(r);
    }
    records
        .iter()
        .filter(|r| {
            if r.state != "open" || r.terminal_id.trim().is_empty() {
                return true; // closed / uncorrelatable rows always pass
            }
            // Keep only the winning open row for this terminal.
            winner
                .get(r.terminal_id.as_str())
                .is_some_and(|best| best.claude_session_id == r.claude_session_id)
        })
        .cloned()
        .collect()
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
    // fsync the temp file BEFORE the rename: the rename is what publishes the
    // snapshot, and publishing a name that points at unflushed bytes is exactly
    // how a compaction can lose the deltas it is about to truncate.
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Write-ahead-log path for a snapshot path: `terminal-sessions.json` →
/// `terminal-sessions.wal.jsonl`, always a sibling so both live under the same
/// instance-scoped directory.
fn wal_path_for(path: &Path) -> PathBuf {
    path.with_extension("wal.jsonl")
}

/// Outcome of a WAL replay — enough to decide whether the log needs folding
/// into the snapshot before the next append.
#[derive(Debug, Default, Clone, Copy)]
struct WalReplay {
    /// Deltas successfully applied to the map.
    applied: usize,
    /// The log ended mid-line (a crash during an append) or held a line that
    /// could not be parsed. Either way the file must be rewritten before any
    /// further append, or the next line would be concatenated onto the partial
    /// one and corrupt an otherwise-good record.
    damaged: bool,
}

/// Replay `wal_path` over `map`, in file order, last-writer-wins per key.
///
/// CRASH SAFETY. Appends are whole-line `write_all`s, so the only damage a
/// crash can do is truncate the final line. A trailing segment with no `\n`
/// terminator is therefore treated as NEVER COMMITTED and dropped — it is the
/// mutation that was in flight when the process died, which the old whole-map
/// rewrite would equally have lost (its `.json.tmp` would simply not have been
/// renamed). Every complete line before it is committed and is applied.
///
/// A complete-but-unparsable line (schema drift, media corruption) is skipped
/// with a warning rather than aborting the replay: one bad delta must not cost
/// the operator every restorable session, the same fail-soft rule
/// [`load_map`] applies to the snapshot.
fn replay_wal(wal_path: &Path, map: &mut HashMap<String, TerminalSessionRecord>) -> WalReplay {
    let bytes = match std::fs::read(wal_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return WalReplay::default(),
        Err(e) => {
            warn!(
                error = %e,
                path = %wal_path.display(),
                "session_lifecycle_store: WAL read failed — snapshot used as-is"
            );
            return WalReplay::default();
        }
    };
    if bytes.is_empty() {
        return WalReplay::default();
    }
    let mut out = WalReplay::default();
    let mut malformed = 0usize;
    // Only `\n`-terminated segments are committed; `split` yields a trailing
    // empty slice for a well-terminated file and the torn partial otherwise.
    let mut segments: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    match segments.pop() {
        Some(tail) if !tail.is_empty() => {
            out.damaged = true;
            warn!(
                bytes = tail.len(),
                path = %wal_path.display(),
                "session_lifecycle_store: torn WAL tail (crash mid-append) — dropping the uncommitted line"
            );
        }
        _ => {}
    }
    for seg in segments {
        if seg.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        match serde_json::from_slice::<LifecycleDelta>(seg) {
            Ok(LifecycleDelta::Upsert { rec }) => {
                let mut rec = *rec;
                rec.origin = normalize_origin(rec.origin);
                map.insert(rec.claude_session_id.clone(), rec);
                out.applied += 1;
            }
            Ok(LifecycleDelta::Remove { id }) => {
                map.remove(&id);
                out.applied += 1;
            }
            Err(e) => {
                malformed += 1;
                out.damaged = true;
                warn!(
                    error = %e,
                    path = %wal_path.display(),
                    "session_lifecycle_store: malformed WAL delta — skipped, keeping the rest"
                );
            }
        }
    }
    if malformed > 0 {
        warn!(
            malformed,
            applied = out.applied,
            path = %wal_path.display(),
            "session_lifecycle_store: WAL replay dropped malformed delta(s)"
        );
    }
    out
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
    /// The record's shell is dead (or claude never appeared) AND the record was
    /// never confirmed — no provider session ever started in this terminal, so
    /// there is nothing that died. Close it `"never-started"`, which is
    /// non-restorable (the restore grace match covers only `"pty-exit"` /
    /// `"poll-dead"`), instead of libelling a bare shell as a dead session.
    CloseNeverStarted,
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
/// - `confirmed`: whether the record carries a `confirmed_at` — i.e. a provider
///   session actually started in this terminal (`POST /control/session-open`,
///   or a transcript-anchored reconcile bind). Every PTY gets a PROVISIONAL
///   record at spawn by design (`apply_identity_seam`, `confirmed_at: None`) so
///   restore has a pre-minted identity; a bare PowerShell pane that never runs
///   a provider therefore holds an unconfirmed record forever. Such a record
///   has no session to lose, so it must never be closed `"poll-dead"` — that
///   reason means "a live session died" and buys a
///   [`RESTORABLE_POLL_DEAD_MS`] restore grace, which is exactly wrong here.
///   Handled symmetrically to `restore_pending`: a parameter that rewrites the
///   base outcome rather than a new lifecycle state (the record already carries
///   the fact).
pub fn classify(
    live_is_alive: Option<bool>,
    claude_present: bool,
    consecutive_dead: u32,
    consecutive_no_match: u32,
    snapshot_ok: bool,
    restore_pending: bool,
    confirmed: bool,
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
    // Never-confirmed guard: a provisional record whose provider never started
    // cannot have "died". Rewrite the poll-dead close to the non-restorable
    // `never-started` close so a bare shell is not preserved as a restore
    // candidate — and so `poll-dead` keeps meaning what it says.
    if !confirmed && base == PollAction::Close {
        return PollAction::CloseNeverStarted;
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Characterization of the WRITE side, which
    /// `2026-08-10-temp-runner-session-restore-isolation` Phase 4 did NOT change
    /// — [`snapshot_history_path`] was already instance-scoped, so this test
    /// passes verbatim against pre-Phase-4 code and does NOT gate that phase.
    /// It is kept as a contract pin for the property the readers now depend on:
    /// a secondary's history is not under the unscoped hook dir at all, and two
    /// spawns on one recycled port do not share a file.
    ///
    /// **The test that actually gates Phase 4 is
    /// [`readers_must_not_re_derive_the_snapshot_history_path`].** Phase 4's
    /// change was entirely in the two READERS
    /// (`commands/terminal.rs:terminal_session_list_history`,
    /// `mcp/sessions.rs:list_history`), which used to derive
    /// `session-snapshots[-<port>].jsonl` under
    /// [`crate::session::claude_hook::session_restore_dir`]. For the primary on
    /// 9876 the two coincide, which is why the divergence stayed invisible; for
    /// ANY secondary they are a different DIRECTORY *and* a different FILENAME,
    /// so the readers opened a file nothing wrote — and on a recycled
    /// temp-runner port, a prior temp's history.
    ///
    /// Sets only `QONTINUI_INSTANCE_NAME`, which `resolve_data_subdir` answers
    /// on its first branch without reading the port that `scheduler_service`'s
    /// tests mutate concurrently.
    #[test]
    fn snapshot_history_path_is_instance_scoped_and_outside_the_hook_dir() {
        let _env = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&["QONTINUI_INSTANCE_NAME"]);

        std::env::set_var("QONTINUI_INSTANCE_NAME", "test-19f6faa3bf8-0");
        let secondary = snapshot_history_path();
        let hook_dir = crate::session::claude_hook::session_restore_dir();
        assert!(
            !secondary.starts_with(&hook_dir),
            "a secondary's snapshot history must not live in the UNSCOPED hook dir {} (got {})",
            hook_dir.display(),
            secondary.display()
        );
        assert!(
            secondary.ends_with("session-snapshots.jsonl"),
            "the filename stays plain — the instance lives in the directory"
        );

        // Two spawns on one recycled port must not share the file.
        std::env::set_var("QONTINUI_INSTANCE_NAME", "test-19f6fd50c26-2");
        assert_ne!(secondary, snapshot_history_path());
    }

    /// Finding 3 (code review 2026-08-10): grep-shaped guard for what Phase 4
    /// ACTUALLY changed — the two readers. Follows the precedent the plan cites,
    /// `qontinui-supervisor` `process/claude_env.rs:every_known_spawn_site_file_still_calls_the_strip`.
    ///
    /// The failure it exists to catch: a future reader (a new MCP route, a
    /// diagnostic command) re-derives `session_restore_dir().join("session-snapshots…")`
    /// — which is exactly the shape the neighbouring [`crate::session::snapshot_history::tree_reset_path_for_port`]
    /// still legitimately uses — and silently re-introduces the divergence for
    /// every secondary. Silent because the wrong file simply does not exist, so
    /// it reads as "no history" rather than as a bug. Until this test, the only
    /// thing preventing that was a comment.
    ///
    /// Three assertions:
    /// 1. Both readers resolve through [`snapshot_history_path`].
    /// 2. The deleted port-keyed reader helper has not come back.
    /// 3. Outside the two modules that legitimately own this pairing (this file,
    ///    the writer; and `snapshot_history.rs`, which documents it and hosts
    ///    the port-keyed tree-reset sibling), no source file mentions BOTH
    ///    `session_restore_dir` and the `session-snapshots` filename — a reader
    ///    re-deriving the path has to name both.
    ///
    /// The needle for assertion 2 is assembled with `concat!` and appears
    /// nowhere in this file as a contiguous literal, deliberately: a
    /// grep-shaped guard that writes its own needle in prose flags ITSELF, which
    /// is how the first draft of this test failed.
    ///
    /// ## What this guard does NOT cover
    ///
    /// It catches the NAMED shape, not all re-derivation. Two known gaps, stated
    /// so nobody reads a green here as broader assurance than it is:
    /// - Assertion 1 is satisfied by a *mention* of `snapshot_history_path()`
    ///   anywhere in the file, including inside a doc comment. A reader that
    ///   keeps the comment while re-deriving the path elsewhere still passes.
    /// - Assertion 3's needle is the co-occurrence of `session_restore_dir` and
    ///   `session-snapshots`. A path hand-rolled without naming the helper is
    ///   invisible to it. That evasion shape already exists in-tree:
    ///   `instance.rs` composes
    ///   `base.join("session-restore").join("session-snapshots.jsonl")` and this
    ///   guard does not see it. That instance is benign — it is a pure
    ///   path-composition test with no runtime reader behind it — but it proves
    ///   the gap is reachable rather than theoretical.
    #[test]
    fn readers_must_not_re_derive_the_snapshot_history_path() {
        // Split so this file does not contain the literal it searches for.
        const DELETED_HELPER: &str = concat!("snapshot_path", "_for_port");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs_files(&src, &mut files);
        // Anchored to the real tree size (1486 files at the time of writing).
        // A low floor is worse than none: it lets a whole subtree drop out of
        // the walk while the assert still passes — `src/mcp/` alone is 152
        // files and `src/commands/` 112.
        assert!(
            files.len() > 1_000,
            "sanity: expected to walk the whole src tree, found only {} files — \
             a subtree is missing and this guard has scanned less than it thinks",
            files.len()
        );

        // 1. Both readers go through the write-side helper.
        for reader in ["commands/terminal.rs", "mcp/sessions.rs"] {
            let body = std::fs::read_to_string(src.join(reader))
                .unwrap_or_else(|e| panic!("reading {reader}: {e}"));
            assert!(
                body.contains("snapshot_history_path()"),
                "{reader} must resolve the snapshot history through \
                 session_lifecycle_store::snapshot_history_path()"
            );
        }

        // 2 + 3. Exempt by RELATIVE PATH, never by basename: a future
        // `src/mcp/snapshot_history.rs` or `src/diagnostics/session_lifecycle_store.rs`
        // would otherwise be silently exempt from assertion 3 — precisely the
        // "new reader in a new file" case this guard exists for, defeated by
        // choice of filename.
        const OWNS_THE_PAIRING: &[&str] = &[
            "session/session_lifecycle_store.rs",
            "session/snapshot_history.rs",
        ];
        for path in &files {
            let rel = path
                .strip_prefix(&src)
                .unwrap_or_else(|e| {
                    panic!("{} is not under {}: {e}", path.display(), src.display())
                })
                .to_string_lossy()
                .replace('\\', "/");
            let body = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            assert!(
                !body.contains(DELETED_HELPER),
                "{rel} references the DELETED port-keyed reader helper ({DELETED_HELPER}) — \
                 it WAS the read/write divergence; resolve through snapshot_history_path()"
            );
            if OWNS_THE_PAIRING.contains(&rel.as_str()) {
                continue;
            }
            assert!(
                !(body.contains("session_restore_dir") && body.contains("session-snapshots")),
                "{rel} mentions BOTH `session_restore_dir` and `session-snapshots`, which is \
                 how the Phase 4 divergence was written: the hook dir is deliberately UNSCOPED \
                 (it is the Claude SessionStart materialization target), so a snapshot path \
                 built from it points at a file nothing writes on every secondary. Resolve \
                 through session_lifecycle_store::snapshot_history_path() instead."
            );
        }
    }

    /// Recursively collect `*.rs` under `dir` (test helper for the grep-shaped
    /// guard above).
    ///
    /// PANICS on any walk error rather than skipping the subtree. Returning
    /// early on `Err` would drop a whole directory from the scan while the
    /// caller's file-count floor still passed — the guard would report green
    /// having examined less than it thinks (`verification-and-evidence`
    /// `silent-empty-is-unknown`). A Windows long-path or transient permission
    /// error on `src/mcp/` alone would hide 152 files.
    fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("walking {}: {e}", dir.display()));
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|e| panic!("reading an entry of {}: {e}", dir.display()));
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

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
            confirmed_at: None,
            handle: None,
        }
    }

    /// Age fields on the persisted registry that no public mutator can reach
    /// (`closed_at`, `last_seen_at`), the WAL-model-correct way.
    ///
    /// Under the write-ahead log a mutation lands as ONE appended delta and the
    /// JSON snapshot is only rewritten on compaction, so the pre-WAL idiom —
    /// read the snapshot, edit it, write it back — read a stale (or, on a store
    /// that has never compacted, an absent) file and then had the un-folded
    /// deltas replayed straight back over the edit on the next `open`.
    /// [`SessionLifecycleStore::compact`] first makes the snapshot the
    /// authoritative copy and truncates the WAL, so the edit is the last word:
    /// reopening the path afterwards loads exactly the tampered map.
    ///
    /// Callers must reopen the store afterwards — `store`'s in-memory map is
    /// deliberately NOT updated, matching the "aged on disk, then restarted"
    /// scenario every caller is reproducing.
    fn age_persisted_records(
        store: &SessionLifecycleStore,
        path: &Path,
        edit: impl FnOnce(&mut HashMap<String, TerminalSessionRecord>),
    ) {
        store.compact();
        let raw = std::fs::read(path).unwrap();
        let mut m: HashMap<String, TerminalSessionRecord> = serde_json::from_slice(&raw).unwrap();
        edit(&mut m);
        std::fs::write(path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    }

    // ── Write-ahead log (Phase 6, B1) ───────────────────────────────────────
    //
    // A mutation costs ONE appended delta line instead of a whole-map JSON
    // rewrite. These prove the three properties that buys us nothing without:
    // deltas survive a reopen, a torn tail loses only the uncommitted line, and
    // compaction is idempotent against replay.

    /// A mutation is durable via the WAL alone — no compaction needed — and the
    /// JSON snapshot has NOT been rewritten (that is the whole O(1) point).
    #[test]
    fn wal_makes_a_mutation_durable_without_rewriting_the_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let wal = wal_path_for(&path);

        {
            let store = SessionLifecycleStore::open(&path).unwrap();
            store.record_open(rec("sess-wal"));
            assert!(wal.exists(), "the mutation must have landed in the WAL");
            let snapshot = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                !snapshot.contains("sess-wal"),
                "the whole-map snapshot must NOT be rewritten per mutation — that is the \
                 O(total sessions) cost this replaces (snapshot was: {snapshot})"
            );
        }

        let reopened = SessionLifecycleStore::open(&path).unwrap();
        assert!(
            reopened.get("sess-wal").is_some(),
            "a WAL-only mutation must survive a reopen"
        );
    }

    /// Every mutation kind round-trips through the WAL, including the two that
    /// DELETE (`remove_session`, `rekey_session`) — a replay that only ever
    /// upserts would resurrect them.
    #[test]
    fn wal_replays_upserts_removals_and_rekeys_in_order() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        {
            let store = SessionLifecycleStore::open(&path).unwrap();
            store.record_open(rec("keep"));
            store.record_open(rec("drop"));
            store.record_open(rec("old-id"));
            store.remove_session("drop");
            store.rekey_session("old-id", "new-id");
            store.update_title_by_terminal("term-abc", "renamed");
        }
        let reopened = SessionLifecycleStore::open(&path).unwrap();
        assert!(reopened.get("keep").is_some());
        assert!(
            reopened.get("drop").is_none(),
            "a Remove delta must survive replay"
        );
        assert!(reopened.get("old-id").is_none(), "the rekey source is gone");
        assert!(
            reopened.get("new-id").is_some(),
            "the rekey target is there"
        );
    }

    /// CRASH SAFETY. A process killed mid-append leaves a partial final line.
    /// Every line before it is committed and must be replayed; the partial one
    /// was never committed and must be dropped — not fatal, and not silently
    /// half-parsed.
    #[test]
    fn torn_wal_tail_drops_only_the_uncommitted_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let wal = wal_path_for(&path);
        {
            let store = SessionLifecycleStore::open(&path).unwrap();
            store.record_open(rec("committed-1"));
            let mut r = rec("committed-2");
            r.terminal_id = "term-two".to_string();
            store.record_open(r);
        }
        // Append a HALF-written third delta: valid prefix, no newline.
        let torn = serde_json::to_string(&LifecycleDelta::Upsert {
            rec: Box::new(rec("torn")),
        })
        .unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&wal).unwrap();
        f.write_all(&torn.as_bytes()[..torn.len() / 2]).unwrap();
        drop(f);

        let reopened = SessionLifecycleStore::open(&path).unwrap();
        assert!(
            reopened.get("committed-1").is_some() && reopened.get("committed-2").is_some(),
            "a torn tail must not cost the committed records before it"
        );
        assert!(
            reopened.get("torn").is_none(),
            "an unterminated tail line was never committed and must be dropped"
        );
        // The damaged log is folded into a clean snapshot on open, so the NEXT
        // append can never be concatenated onto the partial line.
        assert_eq!(
            std::fs::read_to_string(&wal).unwrap(),
            "",
            "a damaged WAL must be compacted away at open"
        );
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("committed-1"));
    }

    /// A complete-but-unparsable delta (schema drift, media corruption) costs
    /// only itself — the same fail-soft rule `load_map` applies to the snapshot.
    #[test]
    fn malformed_wal_line_drops_only_that_delta() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let wal = wal_path_for(&path);
        {
            let store = SessionLifecycleStore::open(&path).unwrap();
            store.record_open(rec("before"));
        }
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&wal).unwrap();
            writeln!(f, "{{\"op\":\"upsert\",\"rec\":\"not a record\"}}").unwrap();
            let mut r = rec("after");
            r.terminal_id = "term-after".to_string();
            let good = serde_json::to_string(&LifecycleDelta::Upsert { rec: Box::new(r) }).unwrap();
            writeln!(f, "{good}").unwrap();
        }
        let reopened = SessionLifecycleStore::open(&path).unwrap();
        assert!(reopened.get("before").is_some());
        assert!(
            reopened.get("after").is_some(),
            "a bad line must not abort the replay of the good ones after it"
        );
    }

    /// Compaction publishes the snapshot BEFORE truncating the WAL, so the
    /// crash window replays deltas the snapshot already holds. Replay is
    /// last-writer-wins per key, so that is a no-op — prove it.
    #[test]
    fn replaying_a_wal_the_snapshot_already_contains_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let wal = wal_path_for(&path);
        {
            let store = SessionLifecycleStore::open(&path).unwrap();
            store.record_open(rec("sess-x"));
            let before = std::fs::read_to_string(&wal).unwrap();
            store.compact();
            // Simulate the crash window: snapshot written + renamed, WAL not
            // yet truncated.
            std::fs::write(&wal, before).unwrap();
        }
        let reopened = SessionLifecycleStore::open(&path).unwrap();
        let all = reopened.all_records();
        assert_eq!(all.len(), 1, "a replayed-twice record must not duplicate");
        assert_eq!(all[0].claude_session_id, "sess-x");
    }

    /// The WAL is bounded: past [`WAL_COMPACT_APPENDS`] the store folds it into
    /// the snapshot on its own, so boot replay cost stays bounded no matter how
    /// long the runner has been up.
    #[test]
    fn wal_compacts_itself_past_the_append_bound() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let wal = wal_path_for(&path);
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-touch"));
        for _ in 0..WAL_COMPACT_APPENDS {
            store.touch("sess-touch");
        }
        assert!(
            std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0) < 4096,
            "the WAL must have been folded into the snapshot, not grown without bound"
        );
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("sess-touch"));
    }

    /// Idle compaction: the liveness poll's heartbeat folds a dirty WAL into
    /// the snapshot, so an idle runner converges without any mutation paying an
    /// O(N) rewrite.
    #[test]
    fn heartbeat_folds_a_dirty_wal_into_the_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let wal = wal_path_for(&path);
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-hb"));
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        store.snapshot_heartbeat();
        assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
        assert!(std::fs::read_to_string(&path).unwrap().contains("sess-hb"));
    }

    /// The `terminal_id -> claude_session_id` reverse index that the enriched
    /// `terminal_list` (`sessionIdsByTerminal`) relies on to re-attach
    /// `claudeSessionId` to reconnected tabs: an OPEN record resolves by its
    /// terminal id (carrying the session id + config dir), and an unknown
    /// terminal resolves to `None`.
    #[test]
    fn find_open_by_terminal_resolves_session_id_for_reconnect() {
        let dir = tempdir().unwrap();
        let store =
            SessionLifecycleStore::open(&dir.path().join("terminal-sessions.json")).unwrap();

        let mut r = rec("sess-1");
        r.terminal_id = "term-1".to_string();
        store.record_open(r);

        let found = store
            .find_open_by_terminal("term-1")
            .expect("open record resolvable by its terminal id");
        assert_eq!(found.claude_session_id, "sess-1");
        assert_eq!(found.config_dir.as_deref(), Some("C:/cfg"));

        assert!(
            store.find_open_by_terminal("term-unknown").is_none(),
            "an unknown terminal id resolves to None"
        );
    }

    /// `find_confirmed_open_by_terminal` — the gate `terminal_list`'s
    /// `sessionIdsByTerminal` uses — returns a record ONLY once it is confirmed.
    /// The spawn-time identity seam records an authoritative-but-PROVISIONAL
    /// `open` row for every terminal (plain shells included, and non-pinned
    /// launches whose minted uuid is never the run id); surfacing those would
    /// bind phantom / wrong ids onto tabs. Provisional ⇒ `None`, confirmed ⇒
    /// `Some`, while the unfiltered `find_open_by_terminal` still resolves both.
    #[test]
    fn find_confirmed_open_by_terminal_excludes_provisional_phantoms() {
        let dir = tempdir().unwrap();
        let store =
            SessionLifecycleStore::open(&dir.path().join("terminal-sessions.json")).unwrap();

        // A provisional (unconfirmed) row — the phantom-shell / minted-uuid shape.
        let mut phantom = rec("phantom-sess");
        phantom.terminal_id = "term-phantom".to_string();
        phantom.confirmed_at = None;
        store.record_open(phantom);

        assert!(
            store
                .find_confirmed_open_by_terminal("term-phantom")
                .is_none(),
            "an unconfirmed (provisional) record is NOT surfaced to session-scoped UI"
        );
        assert!(
            store.find_open_by_terminal("term-phantom").is_some(),
            "the unfiltered lookup still resolves the provisional record"
        );

        // A confirmed row — a real session with a fired hook / start-anchored bind.
        let mut real = rec("real-sess");
        real.terminal_id = "term-real".to_string();
        real.confirmed_at = Some(1_700_000_000_000);
        store.record_open(real);

        let found = store
            .find_confirmed_open_by_terminal("term-real")
            .expect("a confirmed record resolves by its terminal id");
        assert_eq!(found.claude_session_id, "real-sess");
        assert_eq!(found.config_dir.as_deref(), Some("C:/cfg"));

        assert!(
            store
                .find_confirmed_open_by_terminal("term-unknown")
                .is_none(),
            "an unknown terminal id resolves to None"
        );
    }

    /// `confirm_session` flips provisional→confirmed monotonically, a
    /// provisional re-record never clears it, and pre-Phase-2 rows load
    /// provisional (session-restore-redesign Phase 2 coordinator refinement).
    #[test]
    fn confirm_session_flips_provisional_and_record_open_never_clears_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        // Spawn-time write is PROVISIONAL (confirmed_at unset).
        store.record_open(rec("sess-1"));
        assert!(
            store.get("sess-1").unwrap().confirmed_at.is_none(),
            "spawn-time record is provisional"
        );

        // The provider's SessionStart hook fires → confirm.
        store.confirm_session("sess-1");
        let confirmed_at = store.get("sess-1").unwrap().confirmed_at;
        assert!(confirmed_at.is_some(), "hook flips it to confirmed");

        // Confirmation is monotonic: a second confirm (a later resume hook) is a
        // no-op (does NOT bump the timestamp).
        store.confirm_session("sess-1");
        assert_eq!(
            store.get("sess-1").unwrap().confirmed_at,
            confirmed_at,
            "second confirm is a no-op (first hook wins)"
        );

        // A PROVISIONAL re-record (spawn-time writer / zone-move backstop) must
        // NOT clear an existing confirmation.
        store.record_open(rec("sess-1"));
        assert_eq!(
            store.get("sess-1").unwrap().confirmed_at,
            confirmed_at,
            "provisional re-record preserves confirmation"
        );

        // Durable across reload + confirming an absent id is a no-op.
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert!(store.get("sess-1").unwrap().confirmed_at.is_some());
        store.confirm_session("ghost"); // no panic

        // A pre-Phase-2 on-disk record (no confirmedAt key) loads provisional.
        let json = r#"{"old": {
            "claudeSessionId":"old","configDir":null,"workingDir":"C:/repo",
            "pageId":"default","zoneIndex":1,"title":"Old","terminalId":"t",
            "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,"closeReason":null
        }}"#;
        let p2 = dir.path().join("legacy.json");
        std::fs::write(&p2, json).unwrap();
        let s2 = SessionLifecycleStore::open(&p2).unwrap();
        assert!(
            s2.get("old").unwrap().confirmed_at.is_none(),
            "pre-Phase-2 record loads provisional"
        );
    }

    /// Session-identity fabric Phase 1 — the `fsh_` handle round-trips through
    /// disk, is STICKY across a handle-less re-record, only writes on change,
    /// the server wins on divergence, `set_handle` never creates records, and
    /// a LEGACY on-disk row without the field loads cleanly as `None`.
    #[test]
    fn handle_persists_sticky_and_legacy_rows_load_without_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        store.record_open(rec("sess-h"));
        assert!(
            store.get("sess-h").unwrap().handle.is_none(),
            "no handle until acquired"
        );

        // Acquire → persisted.
        store.set_handle("sess-h", "fsh_abc");
        assert_eq!(
            store.get("sess-h").unwrap().handle.as_deref(),
            Some("fsh_abc")
        );

        // A handle-less re-record (zone-move backstop / boot re-assert /
        // provider hook) must NOT clear it — sticky like config_dir.
        store.record_open(rec("sess-h"));
        assert_eq!(
            store.get("sess-h").unwrap().handle.as_deref(),
            Some("fsh_abc"),
            "handle-less re-record preserves the handle"
        );

        // Same value re-set is a no-op; a DIVERGENT server value overwrites
        // (server wins — rebind is keyed on claude_session_id server-side).
        store.set_handle("sess-h", "fsh_abc");
        store.set_handle("sess-h", "fsh_new");
        assert_eq!(
            store.get("sess-h").unwrap().handle.as_deref(),
            Some("fsh_new")
        );

        // set_handle on an absent id never creates a record (a handle-only
        // row would be a phantom restore candidate).
        store.set_handle("ghost", "fsh_ghost");
        assert!(store.get("ghost").is_none());

        // Durable across reload, and a record_open CARRYING a handle sets it.
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert_eq!(
            store.get("sess-h").unwrap().handle.as_deref(),
            Some("fsh_new"),
            "handle survives a reload"
        );
        let mut r = rec("sess-h2");
        r.handle = Some("fsh_h2".to_string());
        store.record_open(r);
        assert_eq!(
            store.get("sess-h2").unwrap().handle.as_deref(),
            Some("fsh_h2")
        );

        // A legacy on-disk row without the `handle` key loads cleanly as None
        // (serde default — the fabric field is purely additive).
        let json = r#"{"old": {
            "claudeSessionId":"old","configDir":null,"workingDir":"C:/repo",
            "pageId":"default","zoneIndex":1,"title":"Old","terminalId":"t",
            "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,"closeReason":null
        }}"#;
        let p2 = dir.path().join("legacy-handle.json");
        std::fs::write(&p2, json).unwrap();
        let s2 = SessionLifecycleStore::open(&p2).unwrap();
        assert!(
            s2.get("old").unwrap().handle.is_none(),
            "legacy row without the field loads with no handle"
        );
    }

    /// `config_dir` is STICKY and empty-normalized (G2 — account-correct
    /// restore): a known-good account binding survives a later `record_open`
    /// that omits the account (a provider that doesn't report it, a zone-move
    /// backstop, Gemini), a stray empty/whitespace incoming normalizes to
    /// `None` (never a bogus `CLAUDE_CONFIG_DIR=""` on resume), and a later
    /// non-empty `Some` still updates (sticky ≠ frozen).
    #[test]
    fn record_open_config_dir_is_sticky_and_empty_normalizes_to_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        // Bind a known-good account.
        let mut r = rec("sess-cfg");
        r.config_dir = Some("C:/cfg".to_string());
        store.record_open(r);
        assert_eq!(
            store.get("sess-cfg").unwrap().config_dir.as_deref(),
            Some("C:/cfg")
        );

        // A later write that OMITS config_dir (None) must NOT clobber it.
        let mut r2 = rec("sess-cfg");
        r2.config_dir = None;
        store.record_open(r2);
        assert_eq!(
            store.get("sess-cfg").unwrap().config_dir.as_deref(),
            Some("C:/cfg"),
            "a None re-record must preserve the known account binding"
        );

        // An empty/whitespace-only incoming normalizes to None → also preserves.
        let mut r3 = rec("sess-cfg");
        r3.config_dir = Some("   ".to_string());
        store.record_open(r3);
        assert_eq!(
            store.get("sess-cfg").unwrap().config_dir.as_deref(),
            Some("C:/cfg"),
            "an empty incoming normalizes to None and preserves the binding"
        );

        // A brand-new record whose only config_dir is empty stores None, never "".
        let mut r4 = rec("sess-empty");
        r4.config_dir = Some(String::new());
        store.record_open(r4);
        assert!(
            store.get("sess-empty").unwrap().config_dir.is_none(),
            "empty config_dir normalizes to None on a fresh record"
        );

        // A later non-empty Some DOES update (sticky is not frozen).
        let mut r5 = rec("sess-cfg");
        r5.config_dir = Some("C:/other".to_string());
        store.record_open(r5);
        assert_eq!(
            store.get("sess-cfg").unwrap().config_dir.as_deref(),
            Some("C:/other"),
            "a later non-empty Some updates the binding"
        );
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

    /// An authoritative record on the named terminal, unconfirmed by default.
    fn auth_on(id: &str, terminal: &str) -> TerminalSessionRecord {
        let mut r = rec(id);
        r.terminal_id = terminal.to_string();
        r.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
        r
    }

    /// Single-tenant-terminal invariant: a new AUTHORITATIVE session binding a
    /// terminal evicts an older UNCONFIRMED phantom sibling on that same
    /// terminal — the identity-seam-vs-account-launcher dual-id split, where the
    /// seam pins a fresh id at shell spawn and the launcher then types
    /// `claude --session-id <its own id>` into that shell.
    #[test]
    fn record_open_supersedes_unconfirmed_phantom_on_same_terminal() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        store.record_open(auth_on("seam-phantom", "term-1"));
        store.record_open(auth_on("real", "term-1"));

        let phantom = store.get("seam-phantom").unwrap();
        assert_eq!(phantom.state, "closed", "phantom superseded");
        assert_eq!(phantom.close_reason.as_deref(), Some("superseded"));
        assert_eq!(store.get("real").unwrap().state, "open", "real stays open");
        let open: Vec<String> = store
            .open_records()
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        assert_eq!(open, vec!["real".to_string()], "only the real session open");
    }

    /// Supersede guards: a CONFIRMED sibling, a sibling on a DIFFERENT terminal,
    /// and a non-authoritative insert all leave the sibling untouched.
    #[test]
    fn record_open_supersede_guards() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // (a) A confirmed sibling represents a session that actually started —
        // never superseded here (the exit / poll-dead path retires it).
        let mut confirmed = auth_on("confirmed", "term-a");
        confirmed.confirmed_at = Some(123);
        store.record_open(confirmed);
        store.record_open(auth_on("newer", "term-a"));
        assert_eq!(
            store.get("confirmed").unwrap().state,
            "open",
            "confirmed sibling survives"
        );

        // (b) A sibling on a different terminal is untouched.
        store.record_open(auth_on("other-term", "term-b"));
        store.record_open(auth_on("fresh", "term-c"));
        assert_eq!(
            store.get("other-term").unwrap().state,
            "open",
            "different-terminal sibling survives"
        );

        // (c) A non-authoritative (reconciled) insert never supersedes — it is
        // itself a guess and restore already quarantines it.
        store.record_open(auth_on("phantom2", "term-d"));
        let mut reconciled = rec("guess");
        reconciled.terminal_id = "term-d".to_string();
        reconciled.origin = Some(ORIGIN_RECONCILED.to_string());
        store.record_open(reconciled);
        assert_eq!(
            store.get("phantom2").unwrap().state,
            "open",
            "reconciled insert does not supersede"
        );
    }

    /// Launch-agnostic supersede: a CONFIRMED `observed` bind (process-anchored
    /// unique transcript correlation) evicts an unconfirmed same-terminal
    /// sibling, closing the observed-orphan class while the terminal is alive —
    /// even though `observed` is not authoritative.
    #[test]
    fn record_open_observed_confirmed_supersedes_unconfirmed_sibling() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // Spawn-time seam pin: authoritative but unconfirmed, on term-1.
        store.record_open(auth_on("seam-phantom", "term-1"));

        // The live binder observes the real session on the same terminal and
        // confirms it (a transcript proved it exists).
        let mut observed = rec("observed-real");
        observed.terminal_id = "term-1".to_string();
        observed.origin = Some(ORIGIN_OBSERVED.to_string());
        observed.confirmed_at = Some(999);
        store.record_open(observed);

        let phantom = store.get("seam-phantom").unwrap();
        assert_eq!(phantom.state, "closed", "unconfirmed sibling superseded");
        assert_eq!(phantom.close_reason.as_deref(), Some("superseded"));
        assert_eq!(
            store.get("observed-real").unwrap().state,
            "open",
            "the confirmed observed bind stays open"
        );
    }

    /// An UNconfirmed `observed` bind is itself only a candidate — it must NOT
    /// supersede a sibling (no proof yet a real session owns the terminal).
    #[test]
    fn record_open_observed_unconfirmed_does_not_supersede() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        store.record_open(auth_on("sibling", "term-2"));

        let mut observed = rec("observed-provisional");
        observed.terminal_id = "term-2".to_string();
        observed.origin = Some(ORIGIN_OBSERVED.to_string());
        // confirmed_at stays None.
        store.record_open(observed);

        assert_eq!(
            store.get("sibling").unwrap().state,
            "open",
            "an unconfirmed observed bind does not supersede its sibling"
        );
    }

    /// Terminal-reuse invariant (Phase 3 / P4): when a NEW CONFIRMED session
    /// binds a terminal, a PRIOR CONFIRMED `open` row on that SAME terminal (a
    /// dead prior run whose exit-close never fired) is retired with
    /// `close_reason == "superseded-terminal-reuse"`. A confirmed row on a
    /// DIFFERENT terminal is untouched.
    #[test]
    fn record_open_new_confirmed_retires_prior_confirmed_on_reused_terminal() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // A prior confirmed run on term-reuse (its exit-close never fired).
        let mut prior = auth_on("prior-run", "term-reuse");
        prior.confirmed_at = Some(100);
        store.record_open(prior);

        // A confirmed run on a DIFFERENT terminal — must survive untouched.
        let mut elsewhere = auth_on("other-term-run", "term-other");
        elsewhere.confirmed_at = Some(100);
        store.record_open(elsewhere);

        // A NEW confirmed session binds the SAME reused terminal.
        let mut next = auth_on("next-run", "term-reuse");
        next.confirmed_at = Some(200);
        store.record_open(next);

        let prior = store.get("prior-run").unwrap();
        assert_eq!(prior.state, "closed", "prior confirmed run retired");
        assert_eq!(
            prior.close_reason.as_deref(),
            Some("superseded-terminal-reuse"),
            "retired with the terminal-reuse reason (distinct from the phantom `superseded`)"
        );
        assert_eq!(
            store.get("next-run").unwrap().state,
            "open",
            "the new confirmed session stays open"
        );
        assert_eq!(
            store.get("other-term-run").unwrap().state,
            "open",
            "a confirmed row on a different terminal is untouched"
        );
    }

    /// The confirmed-reuse arm is gated on the INCOMING being confirmed: an
    /// authoritative-but-UNCONFIRMED incoming must NOT retire a confirmed
    /// sibling on the same terminal (it is not yet itself proven).
    #[test]
    fn record_open_unconfirmed_incoming_does_not_retire_confirmed_sibling() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        let mut confirmed = auth_on("confirmed-run", "term-x");
        confirmed.confirmed_at = Some(100);
        store.record_open(confirmed);

        // Authoritative but UNCONFIRMED incoming on the same terminal.
        store.record_open(auth_on("unconfirmed-incoming", "term-x"));

        assert_eq!(
            store.get("confirmed-run").unwrap().state,
            "open",
            "an authoritative-but-unconfirmed incoming must not retire a confirmed sibling"
        );
    }

    /// Part B: an incoming record with an empty/whitespace-only `terminal_id`
    /// must NOT clobber an existing non-empty binding (mirroring the `config_dir`
    /// sticky guard); a later non-empty terminal still updates it.
    #[test]
    fn record_open_empty_terminal_id_does_not_clobber_binding() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        let mut r = rec("sess-term");
        r.terminal_id = "term-real".to_string();
        store.record_open(r);
        assert_eq!(store.get("sess-term").unwrap().terminal_id, "term-real");

        // A later write that omits the terminal (whitespace-only) preserves it.
        let mut r2 = rec("sess-term");
        r2.terminal_id = "   ".to_string();
        store.record_open(r2);
        assert_eq!(
            store.get("sess-term").unwrap().terminal_id,
            "term-real",
            "an empty/whitespace incoming terminal_id preserves the real binding"
        );

        // A later non-empty terminal DOES update (sticky is not frozen).
        let mut r3 = rec("sess-term");
        r3.terminal_id = "term-moved".to_string();
        store.record_open(r3);
        assert_eq!(
            store.get("sess-term").unwrap().terminal_id,
            "term-moved",
            "a later non-empty terminal_id updates the binding"
        );
    }

    /// Part C: `repair_terminal_id_collisions` collapses N `open` rows sharing
    /// one terminal_id down to the NEWEST open (by last_seen_at), closing the
    /// rest as `superseded-terminal-reuse`; rows on distinct terminals are
    /// untouched; the return value is the number closed.
    #[test]
    fn repair_terminal_id_collisions_collapses_to_newest_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");

        // Three open rows collided on "shared", plus a distinct-terminal open
        // row and an already-closed row (which must not count).
        let mk = |id: &str, terminal: &str, last_seen: i64, state: &str| {
            let mut r = fixture_rec(
                id,
                state,
                last_seen,
                (state == "closed").then_some(last_seen),
                (state == "closed").then_some("pty-exit"),
            );
            r.terminal_id = terminal.to_string();
            r
        };
        write_fixture(
            &path,
            vec![
                mk("old-a", "shared", 1_000, "open"),
                mk("newest", "shared", 3_000, "open"),
                mk("old-b", "shared", 2_000, "open"),
                mk("solo", "other-term", 5_000, "open"),
                mk("already-closed", "shared", 9_000, "closed"),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        let closed = store.repair_terminal_id_collisions();
        assert_eq!(closed, 2, "the two older open rows on the shared terminal");

        // Newest open on the shared terminal survives.
        assert_eq!(store.get("newest").unwrap().state, "open");
        for dead in ["old-a", "old-b"] {
            let r = store.get(dead).unwrap();
            assert_eq!(r.state, "closed", "{dead} collapsed");
            assert_eq!(r.close_reason.as_deref(), Some("superseded-terminal-reuse"));
        }
        // Distinct-terminal row untouched.
        assert_eq!(
            store.get("solo").unwrap().state,
            "open",
            "distinct terminal untouched"
        );
        // A pre-closed row is not the "kept" one and is not re-closed/mutated.
        assert_eq!(
            store.get("already-closed").unwrap().close_reason.as_deref(),
            Some("pty-exit"),
            "an already-closed row is ignored (not counted, not re-stamped)"
        );

        // Idempotent: a second pass closes nothing.
        assert_eq!(store.repair_terminal_id_collisions(), 0, "idempotent");
    }

    /// The boot repair must be CONFIRMED-aware: a CONFIRMED real session on a
    /// reused terminal must survive even when an UNCONFIRMED phantom (a later
    /// zone-move / boot re-assert) carries a marginally NEWER `last_seen_at`.
    /// A naive newest-only rank would close the real session — and, since it is
    /// then `closed`, exclude it from the disk-only rescue net too — keeping a
    /// placeholder phantom instead. Mirrors `open_authority_key`.
    #[test]
    fn repair_terminal_id_collisions_keeps_confirmed_over_newer_unconfirmed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let mk = |id: &str, last_seen: i64, confirmed: Option<i64>| {
            let mut r = fixture_rec(id, "open", last_seen, None, None);
            r.terminal_id = "shared".to_string();
            r.confirmed_at = confirmed;
            r
        };
        write_fixture(
            &path,
            vec![
                mk("real-confirmed", 1_000, Some(1_000)),
                mk("phantom-newer", 2_000, None),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        assert_eq!(store.repair_terminal_id_collisions(), 1);
        assert_eq!(
            store.get("real-confirmed").unwrap().state,
            "open",
            "the confirmed real session survives despite an older last_seen"
        );
        let phantom = store.get("phantom-newer").unwrap();
        assert_eq!(
            phantom.state, "closed",
            "the unconfirmed newer phantom is closed"
        );
        assert_eq!(
            phantom.close_reason.as_deref(),
            Some("superseded-terminal-reuse")
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

    /// `update_title_by_terminal` durably persists a rename keyed by
    /// terminal id (Phase 3 item 4: the rename call sites hold only a
    /// terminal id), survives a reload, and no-ops cleanly on the edges
    /// (unknown terminal, closed record, already-current title).
    #[test]
    fn update_title_by_terminal_persists_rename_and_guards_edges() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-1")); // terminal "term-abc", title "Claude 1"

        store.update_title_by_terminal("term-abc", "634 agent_pusher fix");
        assert_eq!(
            store.get("sess-1").unwrap().title.as_deref(),
            Some("634 agent_pusher fix")
        );

        // Durable: a fresh open from the same path (simulated restart) sees
        // the renamed title, not the spawn-time one.
        let reloaded = SessionLifecycleStore::open(&path).unwrap();
        assert_eq!(
            reloaded.get("sess-1").unwrap().title.as_deref(),
            Some("634 agent_pusher fix"),
            "rename must survive restart"
        );

        // Edge: unknown terminal — no-op, no panic, nothing changed.
        store.update_title_by_terminal("no-such-terminal", "whatever");
        assert_eq!(
            store.get("sess-1").unwrap().title.as_deref(),
            Some("634 agent_pusher fix")
        );

        // Edge: closed record no longer matches (only OPEN rows are keyed by
        // terminal — a fresh PTY may reuse nothing, but a stale closed row
        // must not swallow the rename).
        store.record_close("sess-1", "pty-exit");
        store.update_title_by_terminal("term-abc", "should-not-land");
        assert_eq!(
            store.get("sess-1").unwrap().title.as_deref(),
            Some("634 agent_pusher fix"),
            "a closed record is not renamed"
        );
    }

    /// `remove_session` deletes a row outright (phantom-prune) so it never
    /// resurrects on a future boot, and is a no-op when the id is absent.
    #[test]
    fn remove_session_deletes_row_and_is_noop_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("phantom"));
        assert!(store.get("phantom").is_some());

        store.remove_session("phantom");
        assert!(store.get("phantom").is_none(), "row deleted");
        // Durable across reload — the row stays gone (not just closed-in-grace).
        let store = SessionLifecycleStore::open(&path).unwrap();
        assert!(store.get("phantom").is_none(), "stays removed after reload");
        assert!(
            store
                .restorable_records(Utc::now().timestamp_millis(), None, true)
                .is_empty(),
            "a removed phantom is never restorable"
        );
        // Absent id — no panic, no write.
        store.remove_session("ghost");
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
    fn close_observer_fires_only_on_real_open_to_closed_transitions() {
        // Fabric Phase 3 (review W2): the observer must see every REAL
        // open→closed flip exactly once — never an absent-id close, never a
        // repeat close. Unattached stores (every other test) stay no-op.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();

        let fired = std::sync::Arc::new(AtomicUsize::new(0));
        let last: std::sync::Arc<std::sync::Mutex<String>> = Default::default();
        {
            let fired = fired.clone();
            let last = last.clone();
            store.attach_close_observer(move |csid| {
                fired.fetch_add(1, Ordering::SeqCst);
                *last.lock().unwrap() = csid.to_string();
            });
        }

        // Absent id → no fire.
        store.record_close("ghost", "x");
        assert_eq!(fired.load(Ordering::SeqCst), 0);

        // Real transition → fires once with the csid.
        store.record_open(rec("sess-1"));
        store.record_close("sess-1", "poll-dead");
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(last.lock().unwrap().as_str(), "sess-1");

        // Repeat close → no refire.
        store.record_close("sess-1", "again");
        assert_eq!(fired.load(Ordering::SeqCst), 1);

        // Reopen + close again → a second REAL transition fires again.
        store.record_open(rec("sess-1"));
        store.record_close("sess-1", "poll-dead");
        assert_eq!(fired.load(Ordering::SeqCst), 2);
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
        age_persisted_records(&store, &path, |m| {
            m.get_mut("stale-pty-exit-sess").unwrap().closed_at =
                Some(now - RESTORABLE_PTY_EXIT_MS - 1000);
        });
        let store = SessionLifecycleStore::open(&path).unwrap();

        let mut ids: Vec<String> = store
            .restorable_records(now, None, true)
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

    /// B5 (phantom-id plan): with a transcript probe attached, every history
    /// entry carries the write-time restorability verdict — so a recovery line
    /// naming a phantom id says so, instead of looking identical to a healthy
    /// one (the 2026-07-08 failure). Without a probe the fields stay absent.
    #[test]
    fn snapshot_history_stamps_restorability_when_a_probe_is_attached() {
        #[derive(Debug)]
        struct FakeProbe;
        impl TranscriptProbe for FakeProbe {
            fn transcript_exists(&self, session_id: &str, _wd: Option<&str>) -> bool {
                session_id == "sess-1"
            }
        }

        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let history_path = dir.path().join("session-snapshots.jsonl");
        store.attach_snapshot_history(Arc::new(
            crate::session::snapshot_history::SnapshotHistory::open(&history_path).unwrap(),
        ));
        store.attach_transcript_probe(Arc::new(FakeProbe));

        let last_line = |path: &Path| -> serde_json::Value {
            let contents = std::fs::read_to_string(path).unwrap_or_default();
            let line = contents.lines().filter(|l| !l.trim().is_empty()).last();
            serde_json::from_str(line.expect("a snapshot line")).unwrap()
        };

        // A transcript-backed, CONFIRMED session → restorable:true.
        let mut healthy = rec("sess-1");
        healthy.confirmed_at = Some(42);
        store.record_open(healthy);
        let s = &last_line(&history_path)["sessions"][0];
        assert_eq!(s["claudeSessionId"], "sess-1");
        assert_eq!(s["transcriptExists"], true);
        assert_eq!(
            s["restorable"], true,
            "the newest entry carries restorable:true + a real id"
        );

        // A phantom (no transcript, unconfirmed) → the line names it as such.
        let mut phantom = rec("0b32d739");
        phantom.terminal_id = "943aa0b6".to_string();
        phantom.confirmed_at = None;
        store.record_open(phantom);
        let sessions = last_line(&history_path)["sessions"]
            .as_array()
            .unwrap()
            .clone();
        let p = sessions
            .iter()
            .find(|s| s["claudeSessionId"] == "0b32d739")
            .expect("the phantom is in the snapshot");
        assert_eq!(p["transcriptExists"], false);
        assert_eq!(p["restorable"], false, "the phantom is flagged, not silent");
    }

    /// Phase 4 (session-snapshot history): layout-meaningful mutations
    /// append CHANGE snapshots to the attached history; liveness churn
    /// (`touch`, restore-pending markers) and `prune` do NOT — and pruning
    /// the registry never removes already-appended history lines (the
    /// history outlives the registry's retention).
    #[test]
    fn snapshot_history_captures_changes_but_not_liveness_churn_or_prune() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let history_path = dir.path().join("session-snapshots.jsonl");
        store.attach_snapshot_history(Arc::new(
            crate::session::snapshot_history::SnapshotHistory::open(&history_path).unwrap(),
        ));

        let lines = |path: &Path| -> Vec<serde_json::Value> {
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        };

        // Open → one change snapshot carrying the full recovery tuple.
        store.record_open(rec("sess-1"));
        let recs = lines(&history_path);
        assert_eq!(recs.len(), 1, "record_open appends a change snapshot");
        assert_eq!(recs[0]["reason"], "change");
        let s = &recs[0]["sessions"][0];
        assert_eq!(s["claudeSessionId"], "sess-1");
        assert_eq!(s["configDir"], "C:/cfg");
        assert_eq!(s["workingDir"], "C:/repo");
        assert_eq!(s["provider"], "claude");
        assert_eq!(s["pageId"], "default");
        assert_eq!(s["zoneIndex"], 2);
        assert_eq!(s["title"], "Claude 1");
        assert_eq!(s["state"], "open");

        // Liveness churn must stay silent: touch, restore-pending markers,
        // an identical re-record (boot re-assert), and a snapshot-gated
        // heartbeat right after a change.
        store.touch("sess-1");
        store.mark_restore_pending("sess-1");
        store.clear_restore_pending("sess-1");
        store.record_open(rec("sess-1"));
        store.snapshot_heartbeat();
        assert_eq!(
            recs.len(),
            lines(&history_path).len(),
            "churn appends nothing"
        );

        // Close → a change snapshot with the alive-state fields.
        store.record_close("sess-1", "poll-dead");
        let recs = lines(&history_path);
        assert_eq!(recs.len(), 2, "record_close appends a change snapshot");
        let s = &recs[1]["sessions"][0];
        assert_eq!(s["state"], "closed");
        assert_eq!(s["closeReason"], "poll-dead");

        // Prune destroys the registry row (far-future now) but the history
        // keeps every appended line: the audit outlives `prune`.
        store.prune(
            Utc::now().timestamp_millis() + CLOSED_RETENTION_MS + 1_000,
            &HashSet::new(),
        );
        assert!(store.get("sess-1").is_none(), "registry row pruned");
        assert_eq!(lines(&history_path).len(), 2, "history retained past prune");
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

        let now = Utc::now().timestamp_millis();

        // No public mutator can push `last_seen_at`/`closed_at` into the past,
        // so age `stale-open` and `old-closed` past their thresholds on disk
        // and let the reopen below load them.
        age_persisted_records(&store, &path, |m| {
            m.get_mut("stale-open").unwrap().last_seen_at = now - OPEN_STALE_MS - 1000;
            m.get_mut("old-closed").unwrap().closed_at = Some(now - CLOSED_RETENTION_MS - 1000);
        });

        // Reopen so the store loads the aged timestamps, then prune. No
        // terminal is live, so retention alone decides.
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.prune(now, &HashSet::new());

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

    /// P4: prune retention vs terminal liveness. A closed row whose terminal
    /// is STILL LIVE survives the prune past `CLOSED_RETENTION_MS` (deleting
    /// it would make a recorded-then-closed terminal look never-recorded);
    /// the same-aged closed row whose terminal is gone is pruned. The
    /// open-stale (7d not-seen) path is gated identically.
    #[test]
    fn prune_keeps_rows_whose_terminal_is_still_live() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        let mut closed_live = rec("closed-live-term");
        closed_live.terminal_id = "term-live".to_string();
        let mut closed_live_older = rec("closed-live-term-older");
        closed_live_older.terminal_id = "term-live".to_string();
        let mut closed_gone = rec("closed-gone-term");
        closed_gone.terminal_id = "term-gone".to_string();
        let mut open_live = rec("open-live-term");
        open_live.terminal_id = "term-live".to_string();
        store.record_open(closed_live);
        store.record_open(closed_live_older);
        store.record_open(closed_gone);
        store.record_open(open_live);
        store.record_close("closed-live-term", "pty-exit");
        store.record_close("closed-live-term-older", "pty-exit");
        store.record_close("closed-gone-term", "pty-exit");

        let now = Utc::now().timestamp_millis();
        // Age both closed rows past retention and the open row past stale.
        age_persisted_records(&store, &path, |m| {
            m.get_mut("closed-live-term").unwrap().closed_at =
                Some(now - CLOSED_RETENTION_MS - 1_000);
            // An OLDER closed sibling under the same live terminal — only the
            // newest closed row per live terminal is retention-exempt.
            m.get_mut("closed-live-term-older").unwrap().closed_at =
                Some(now - CLOSED_RETENTION_MS - 2_000);
            m.get_mut("closed-gone-term").unwrap().closed_at =
                Some(now - CLOSED_RETENTION_MS - 1_000);
            m.get_mut("open-live-term").unwrap().last_seen_at = now - OPEN_STALE_MS - 1_000;
        });

        let store = SessionLifecycleStore::open(&path).unwrap();
        let live: HashSet<String> = ["term-live".to_string()].into_iter().collect();
        store.prune(now, &live);

        let m: HashMap<String, TerminalSessionRecord> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            m.contains_key("closed-live-term"),
            "newest closed row survives while its terminal lives"
        );
        assert!(
            !m.contains_key("closed-live-term-older"),
            "older closed sibling under the same live terminal ages out"
        );
        assert!(
            !m.contains_key("closed-gone-term"),
            "terminal gone → pruned after retention"
        );
        assert!(
            m.contains_key("open-live-term"),
            "stale-open row with a live terminal survives"
        );

        // Once the terminal is gone too, the same aged rows are pruned.
        store.prune(now, &HashSet::new());
        let m: HashMap<String, TerminalSessionRecord> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            !m.contains_key("closed-live-term"),
            "pruned once its terminal is gone"
        );
        assert!(!m.contains_key("open-live-term"));
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

    /// Every case below the marker predates the `confirmed` dimension and
    /// asserts the behaviour of a CONFIRMED record (a provider session really
    /// started). Pinning that one dimension here keeps those cases reading as
    /// the liveness matrix they are; the never-confirmed dimension is covered
    /// separately by `classify_never_confirmed_*`.
    fn classify_confirmed(
        live_is_alive: Option<bool>,
        claude_present: bool,
        consecutive_dead: u32,
        consecutive_no_match: u32,
        snapshot_ok: bool,
        restore_pending: bool,
    ) -> PollAction {
        classify(
            live_is_alive,
            claude_present,
            consecutive_dead,
            consecutive_no_match,
            snapshot_ok,
            restore_pending,
            true,
        )
    }

    #[test]
    fn classify_skip_on_snapshot_failure() {
        // snapshot_ok=false dominates every other input.
        assert_eq!(
            classify_confirmed(Some(false), false, 5, 0, false, false),
            PollAction::Skip
        );
        assert_eq!(
            classify_confirmed(Some(true), true, 0, 0, false, false),
            PollAction::Skip
        );
        assert_eq!(
            classify_confirmed(None, false, 0, 0, false, false),
            PollAction::Skip
        );
        // Even a no-match streak past the orphan threshold must Skip (not
        // CloseNoTerminal) when the snapshot failed.
        assert_eq!(
            classify_confirmed(None, false, 0, NO_TERMINAL_ORPHAN_TICKS + 5, false, false),
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
                classify_confirmed(None, false, 0, prior, true, false),
                PollAction::NoMatchWait,
                "no match, {prior} prior no-match ticks must NoMatchWait"
            );
        }
        // claude_present/dead-tick inputs are irrelevant to the no-match arm.
        assert_eq!(
            classify_confirmed(None, true, 9, 0, true, false),
            PollAction::NoMatchWait
        );
    }

    #[test]
    fn classify_no_match_closes_no_terminal_after_orphan_ticks() {
        // Once the no-match streak reaches the threshold the record is an
        // orphan — close it with the (non-restorable) "no-terminal" reason.
        // With a 45s poll the close lands on the 4th consecutive tick ≈ 3min.
        assert_eq!(
            classify_confirmed(None, false, 0, NO_TERMINAL_ORPHAN_TICKS, true, false),
            PollAction::CloseNoTerminal
        );
        assert_eq!(
            classify_confirmed(None, false, 0, NO_TERMINAL_ORPHAN_TICKS + 5, true, false),
            PollAction::CloseNoTerminal
        );
    }

    #[test]
    fn classify_close_on_dead_shell() {
        // A dead pty closes regardless of claude_present — the unambiguous
        // confident-dead signal.
        assert_eq!(
            classify_confirmed(Some(false), false, 0, 0, true, false),
            PollAction::Close
        );
        assert_eq!(
            classify_confirmed(Some(false), true, 0, 0, true, false),
            PollAction::Close
        );
    }

    #[test]
    fn classify_keepalive_when_claude_present() {
        // Claude present in the inclusive subtree ⇒ KeepAlive immediately —
        // even with zero prior dead ticks (the idle-agent bug) and even after
        // a long prior claude-absent streak (claude came back).
        assert_eq!(
            classify_confirmed(Some(true), true, 0, 0, true, false),
            PollAction::KeepAlive
        );
        assert_eq!(
            classify_confirmed(Some(true), true, 9, 0, true, false),
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
            classify_confirmed(Some(true), true, 0, 0, true, false),
            PollAction::KeepAlive
        );
        assert_eq!(
            classify_confirmed(
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
            classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS - 1, 0, true, false),
            PollAction::NeedsConfirm
        );
        assert_eq!(
            classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS, 0, true, false),
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
                classify_confirmed(Some(true), false, prior, 0, true, false),
                PollAction::NeedsConfirm,
                "live shell, claude absent, {prior} prior ticks must NeedsConfirm"
            );
        }
        // Specifically: the second tick (prior == 1) no longer closes.
        assert_eq!(
            classify_confirmed(Some(true), false, 1, 0, true, false),
            PollAction::NeedsConfirm
        );
    }

    #[test]
    fn classify_close_only_after_live_shell_dead_ticks_reached() {
        // Only once we've accumulated LIVE_SHELL_DEAD_TICKS consecutive
        // claude-absent ticks does a live shell finally close (operator quit
        // claude; the bare shell PID lingers).
        assert_eq!(
            classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS, 0, true, false),
            PollAction::Close
        );
        assert_eq!(
            classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS + 5, 0, true, false),
            PollAction::Close
        );
    }

    #[test]
    fn classify_restore_pending_never_closes() {
        // The incident shape: a restored pane whose resume silently failed.
        // Dead pty (the restore shell died / was never matched) → Skip, NOT
        // Close — the durable `open` record must survive for the next attempt.
        assert_eq!(
            classify_confirmed(Some(false), false, 0, 0, true, true),
            PollAction::Skip
        );
        // Plain shell with no claude present, even past the debounce ticks
        // (the exact poll-dead flip the incident hit) → Skip.
        assert_eq!(
            classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS, 0, true, true),
            PollAction::Skip
        );
        assert_eq!(
            classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS + 5, 0, true, true),
            PollAction::Skip
        );
        // Below the debounce it must not even accumulate NeedsConfirm ticks.
        assert_eq!(
            classify_confirmed(Some(true), false, 0, 0, true, true),
            PollAction::Skip
        );
        // No matching pty → Skip, NOT NoMatchWait: a mid-restore row keeps
        // its OLD terminal_id until the re-assert and must never accumulate
        // no-match ticks toward an orphan close.
        assert_eq!(
            classify_confirmed(None, false, 0, 0, true, true),
            PollAction::Skip
        );
        // Even a streak past the orphan threshold must not close while the
        // restore-pending marker is set.
        assert_eq!(
            classify_confirmed(None, false, 0, NO_TERMINAL_ORPHAN_TICKS + 1, true, true),
            PollAction::Skip
        );
    }

    #[test]
    fn classify_restore_pending_passes_through_confident_alive() {
        // A confidently-alive session (claude present) classifies KeepAlive
        // even while restore-pending — the caller uses this to self-heal a
        // stale marker.
        assert_eq!(
            classify_confirmed(Some(true), true, 0, 0, true, true),
            PollAction::KeepAlive
        );
    }

    /// Item 3 regression: 39 of 42 records on the primary read
    /// `closed/"poll-dead"` while their PTY was demonstrably alive. Every PTY
    /// gets a provisional record at spawn; a bare PowerShell pane never runs a
    /// provider, so `claude_present` is false forever and the debounce
    /// inevitably expires. Such a record must never be called `poll-dead`.
    #[test]
    fn classify_never_confirmed_bare_shell_is_never_poll_dead() {
        // The exact observed shape: live shell, no claude, debounce expired.
        assert_eq!(
            classify(
                Some(true),
                false,
                LIVE_SHELL_DEAD_TICKS,
                0,
                true,
                false,
                false
            ),
            PollAction::CloseNeverStarted,
        );
        assert_eq!(
            classify(
                Some(true),
                false,
                LIVE_SHELL_DEAD_TICKS + 5,
                0,
                true,
                false,
                false
            ),
            PollAction::CloseNeverStarted,
        );
        // A dead pty for a never-started record is likewise nothing dying.
        assert_eq!(
            classify(Some(false), false, 0, 0, true, false, false),
            PollAction::CloseNeverStarted,
        );
        // The verification case: a bare terminal after two poll cycles has not
        // reached the debounce, so it is still merely NeedsConfirm — and under
        // no input does it produce Close.
        for prior in 0..=(LIVE_SHELL_DEAD_TICKS + 5) {
            assert_ne!(
                classify(Some(true), false, prior, 0, true, false, false),
                PollAction::Close,
                "a never-confirmed record must never classify poll-dead ({prior} ticks)"
            );
        }
    }

    /// The guard must be narrow: it only rewrites the `poll-dead` close. A
    /// CONFIRMED record keeps closing `poll-dead` (the operator-quit-claude
    /// cleanup), and the never-confirmed guard never resurrects a record or
    /// suppresses the orphan / keep-alive arms.
    #[test]
    fn classify_never_confirmed_leaves_other_arms_intact() {
        // Confirmed + same inputs ⇒ still the real poll-dead close.
        assert_eq!(
            classify(
                Some(true),
                false,
                LIVE_SHELL_DEAD_TICKS,
                0,
                true,
                false,
                true
            ),
            PollAction::Close,
        );
        // An unconfirmed record with a live claude is a real session mid-bind
        // (the hook lands within seconds) — KeepAlive, not a close.
        assert_eq!(
            classify(Some(true), true, 0, 0, true, false, false),
            PollAction::KeepAlive,
        );
        // Orphan close keeps its own already-non-restorable reason.
        assert_eq!(
            classify(None, false, 0, NO_TERMINAL_ORPHAN_TICKS, true, false, false),
            PollAction::CloseNoTerminal,
        );
        // Uncertainty still dominates: snapshot failure and restore-pending.
        assert_eq!(
            classify(Some(false), false, 0, 0, false, false, false),
            PollAction::Skip,
        );
        assert_eq!(
            classify(Some(false), false, 0, 0, true, true, false),
            PollAction::Skip,
        );
    }

    /// `never-started` must be non-restorable — the whole point of not calling
    /// it `poll-dead`, which buys a [`RESTORABLE_POLL_DEAD_MS`] restore grace.
    #[test]
    fn never_started_close_is_not_restorable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("bare-shell"));
        store.record_close("bare-shell", "never-started");

        let now = Utc::now().timestamp_millis();
        assert!(
            store.restorable_records(now, None, true).is_empty(),
            "a never-started (bare shell) record must never be a restore candidate"
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
            store.restorable_records(now, None, true).is_empty(),
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
            confirmed_at: None,
            handle: None,
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
        boot_was_clean: bool,
    ) -> Vec<String> {
        let mut ids: Vec<String> = store
            .restorable_records(now, prior_marker_at, boot_was_clean)
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        ids.sort();
        ids
    }

    /// `closed_ids()` returns exactly the ids of `state == "closed"` rows —
    /// `open` rows (restorable or grace-gate-dropped ghost alike) never appear.
    #[test]
    fn closed_ids_returns_only_closed_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let t = 1_700_000_000_000_i64;
        write_fixture(
            &path,
            vec![
                fixture_rec("open-fresh", "open", t, None, None),
                fixture_rec("open-ghost", "open", t - 72 * 3_600_000, None, None),
                fixture_rec("closed-user", "closed", t, Some(t), Some("no-terminal")),
                fixture_rec("closed-pty", "closed", t, Some(t), Some("pty-exit")),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();
        let closed = store.closed_ids();
        assert_eq!(
            closed,
            ["closed-user".to_string(), "closed-pty".to_string()]
                .into_iter()
                .collect::<HashSet<String>>(),
            "closed_ids returns only the two closed rows"
        );
    }

    /// P3 end-to-end (store side): the disk-only-net exclusion set is
    /// `restorable ids ∪ closed_ids`. An `open` row DROPPED by the restorable
    /// grace gate (Phase-1 cohort-anchor victim) is absent from that set, so it
    /// can LEAK into the quarantined disk-only candidates; a user-closed row
    /// and an already-restorable row are both excluded (never resurrected /
    /// never double-offered).
    #[test]
    fn disk_only_exclusion_leaks_open_victim_not_closed_or_restorable() {
        use crate::session::reconcile::select_disk_only_candidates;
        use crate::terminal::transcript::RecentTranscript;

        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let crash = 1_700_000_000_000_i64;
        let now = crash + 60_000; // restart one minute later
        write_fixture(
            &path,
            vec![
                // Fresh-at-crash open row → admitted to the restorable set.
                fixture_rec("open-restorable", "open", crash, None, None),
                // Open row that died 72h before the crash cohort → EXCLUDED
                // from the restorable set (the grace-gate victim P3 rescues).
                fixture_rec("open-victim", "open", crash - 72 * 3_600_000, None, None),
                // User-closed row with an intentional close reason.
                fixture_rec(
                    "user-closed",
                    "closed",
                    crash,
                    Some(crash),
                    Some("no-terminal"),
                ),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        // Build the exclusion set exactly as `terminal_session_list_open` does:
        // restorable ids ∪ closed ids.
        let restorable = store.restorable_records(now, None, false);
        assert_eq!(
            restorable
                .iter()
                .map(|r| r.claude_session_id.as_str())
                .collect::<HashSet<&str>>(),
            ["open-restorable"].into_iter().collect::<HashSet<&str>>(),
            "only the fresh open row is restorable; the stale open row is dropped"
        );
        let mut excluded = store.closed_ids();
        excluded.extend(restorable.iter().map(|r| r.claude_session_id.clone()));

        // All three sessions have an equally-fresh transcript on disk.
        let recents = vec![
            RecentTranscript {
                session_id: "open-restorable".to_string(),
                config_dir: "C:/cfg".to_string(),
                working_dir: "C:/repo".to_string(),
                last_activity_ms: now - 1_000,
            },
            RecentTranscript {
                session_id: "open-victim".to_string(),
                config_dir: "C:/cfg".to_string(),
                working_dir: "C:/repo".to_string(),
                last_activity_ms: now - 1_000,
            },
            RecentTranscript {
                session_id: "user-closed".to_string(),
                config_dir: "C:/cfg".to_string(),
                working_dir: "C:/repo".to_string(),
                last_activity_ms: now - 1_000,
            },
        ];
        let offered = select_disk_only_candidates(&recents, &excluded, now);
        let ids: HashSet<&str> = offered
            .iter()
            .map(|r| r.claude_session_id.as_str())
            .collect();

        // (a) the grace-gate open victim leaks through, quarantined.
        assert!(
            ids.contains("open-victim"),
            "grace-gate-dropped open row is offered as a disk-only candidate"
        );
        let victim = offered
            .iter()
            .find(|r| r.claude_session_id == "open-victim")
            .unwrap();
        assert_eq!(
            victim.origin.as_deref(),
            Some(ORIGIN_RECONCILED),
            "leaked candidate is quarantine-tier (reconciled)"
        );
        assert!(
            victim.confirmed_at.is_none(),
            "leaked candidate is unconfirmed (one-click verified resume gated)"
        );
        // (b) a user-closed row is NOT resurrected by a fresh transcript.
        assert!(
            !ids.contains("user-closed"),
            "user-closed row is never re-offered"
        );
        // (c) an already-restorable row is NOT double-offered by the net.
        assert!(
            !ids.contains("open-restorable"),
            "restorable row is not double-offered by the disk-only net"
        );
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
        assert_eq!(restorable_ids(&store, now, Some(shutdown), true), expected);
        // …and even without it: the pty-exit closes already anchor the
        // registry at the shutdown instant, so the 72h ghost stays out.
        assert_eq!(restorable_ids(&store, now, None, true), expected);
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
        // Unclean boot: the crashed process's own marker is excluded from the
        // anchor; the dense crash cohort supplies it.
        assert_eq!(
            restorable_ids(&store, now, prior_marker_at, false),
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
            restorable_ids(&store, now, Some(shutdown), true),
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
            restorable_ids(&store, now, Some(shutdown), true).is_empty(),
            "the clean-boot marker anchor must exclude a lone stale ghost"
        );
        // Documented fallback: with no marker and no sibling rows the ghost
        // self-anchors and is admitted (defensive — better than losing a
        // real lone session on a registry with no other signal).
        assert_eq!(
            restorable_ids(&store, now, None, true),
            vec!["ghost".to_string()]
        );
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
        age_persisted_records(&store, &path, |m| {
            m.get_mut("poll-dead-fresh").unwrap().closed_at = Some(now - 30_000);
            m.get_mut("poll-dead-stale").unwrap().closed_at =
                Some(now - RESTORABLE_POLL_DEAD_MS - 1000);
        });
        let store = SessionLifecycleStore::open(&path).unwrap();

        let ids: Vec<String> = store
            .restorable_records(now, None, true)
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

    /// The 2026-07-19 mass-strand, reproduced. A crash cohort dies together at
    /// ~T; the runner is restarted ~2h later; an INTERMEDIATE boot during the
    /// downtime rewrote the shutdown marker to a later instant (T+90m). Under
    /// the old anchor that later marker `at` fed the global max and evicted
    /// every crash row (`anchor - last_seen = 90m > 10m grace`) — 81 sessions
    /// lost. Gating the marker to CLEAN boots only (this boot is unclean) keeps
    /// the whole cohort restorable: the rows supply the anchor themselves, at ~T.
    #[test]
    fn restorable_records_crash_cohort_survives_delayed_restart_marker_gated() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let t = 1_700_000_000_000_i64; // crash instant
        let ninety_min = 90 * 60_000_i64;
        let now = t + 2 * 3_600_000; // restarted 2h later

        // Five open rows that stopped together at ~T (the crash cohort).
        write_fixture(
            &path,
            vec![
                fixture_rec("crash-0", "open", t, None, None),
                fixture_rec("crash-1", "open", t - 1_000, None, None),
                fixture_rec("crash-2", "open", t - 2_000, None, None),
                fixture_rec("crash-3", "open", t - 3_000, None, None),
                fixture_rec("crash-4", "open", t - 4_000, None, None),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        // Unclean (crash) boot: the intermediate marker at T+90m is EXCLUDED
        // from the anchor, so the crash rows anchor themselves and all survive.
        let marker_at = Some(t + ninety_min);
        let ids = restorable_ids(&store, now, marker_at, false);
        assert_eq!(
            ids,
            vec![
                "crash-0".to_string(),
                "crash-1".to_string(),
                "crash-2".to_string(),
                "crash-3".to_string(),
                "crash-4".to_string(),
            ],
            "the whole crash cohort survives a delayed restart (got {ids:?})"
        );

        // Control: on a CLEAN boot the same later marker IS an honest
        // last-moment-of-life signal and correctly evicts the now-stale cohort.
        assert!(
            restorable_ids(&store, now, marker_at, true).is_empty(),
            "a clean-boot marker 90m newer than the cohort excludes it"
        );
    }

    /// A genuine stale lone ghost (its terminal died 72h before the crash) stays
    /// EXCLUDED: a fresh crash band supplies the anchor and the ghost is far
    /// outside grace below it.
    #[test]
    fn restorable_records_still_excludes_stale_ghost() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let t = 1_700_000_000_000_i64;
        let now = t + 3_600_000; // 1h later

        write_fixture(
            &path,
            vec![
                fixture_rec("crash-0", "open", t, None, None),
                fixture_rec("crash-1", "open", t - 1_000, None, None),
                fixture_rec("crash-2", "open", t - 2_000, None, None),
                fixture_rec("stale-ghost", "open", t - 72 * 3_600_000, None, None),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        let ids = restorable_ids(&store, now, None, false);
        assert_eq!(
            ids,
            vec![
                "crash-0".to_string(),
                "crash-1".to_string(),
                "crash-2".to_string(),
            ],
            "the crash band restores; the 72h stale ghost stays excluded"
        );
    }

    /// A genuinely-newer band advances the anchor and a crash band more than
    /// `grace` OLDER is EXCLUDED from the (full-restore) set — it is stale
    /// relative to the last moment of life, exactly as an old lone ghost is.
    /// (It is not lost: `terminal_session_list_open` still offers such a
    /// grace-gated open row through the QUARANTINED disk-only transcript net —
    /// see the P3 tests.) This pins the fix for the densest-cohort
    /// over-admission regression: an older-but-larger band must NOT re-admit
    /// stale sessions by pinning the anchor into the past.
    #[test]
    fn restorable_records_newer_band_excludes_older_crash_band() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let t = 1_700_000_000_000_i64;
        let thirty_min = 30 * 60_000_i64;
        let now = t + 2 * 3_600_000;

        write_fixture(
            &path,
            vec![
                // A LARGER but OLDER band (5 rows) at ~T.
                fixture_rec("old-0", "open", t, None, None),
                fixture_rec("old-1", "open", t - 1_000, None, None),
                fixture_rec("old-2", "open", t - 2_000, None, None),
                fixture_rec("old-3", "open", t - 3_000, None, None),
                fixture_rec("old-4", "open", t - 4_000, None, None),
                // A SMALLER but genuinely-NEWER band (2 rows) 30m later.
                fixture_rec("newer-0", "open", t + thirty_min, None, None),
                fixture_rec("newer-1", "open", t + thirty_min - 1_000, None, None),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        let ids = restorable_ids(&store, now, None, false);
        assert_eq!(
            ids,
            vec!["newer-0".to_string(), "newer-1".to_string()],
            "only the newest band restores; the 30m-older band is stale (got {ids:?})"
        );
    }

    /// The read-time one-live-session-per-terminal dedupe: when several admitted
    /// OPEN rows share a `terminal_id`, `restorable_records` returns exactly the
    /// most-authoritative one (CONFIRMED over unconfirmed, else newest), so the
    /// restore read never collapses N rows onto one terminal — regardless of
    /// whether the persistent boot repair has run yet.
    #[test]
    fn restorable_records_dedupes_collided_open_rows_prefers_confirmed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let t = 1_700_000_000_000_i64;
        let now = t + 60_000;

        let mk = |id: &str, terminal: &str, last_seen: i64, confirmed: Option<i64>| {
            let mut r = fixture_rec(id, "open", last_seen, None, None);
            r.terminal_id = terminal.to_string();
            r.confirmed_at = confirmed;
            r
        };
        write_fixture(
            &path,
            vec![
                // Same terminal: an older CONFIRMED real session and a newer
                // UNCONFIRMED phantom (a later re-assert). Confirmed must win.
                mk("confirmed-real", "shared", t - 5_000, Some(t - 5_000)),
                mk("unconfirmed-newer", "shared", t, None),
                // A distinct terminal — always kept.
                mk("solo", "other", t, None),
            ],
        );
        let store = SessionLifecycleStore::open(&path).unwrap();

        let mut ids = restorable_ids(&store, now, None, false);
        ids.sort();
        assert_eq!(
            ids,
            vec!["confirmed-real".to_string(), "solo".to_string()],
            "the confirmed row wins the shared terminal; the phantom is dropped; solo kept"
        );
    }

    /// Unit coverage for the read-time dedupe helper: confirmed beats a newer
    /// unconfirmed on the same terminal; distinct terminals both survive; an
    /// empty terminal_id is uncorrelatable and passes through; closed rows pass.
    #[test]
    fn dedupe_open_by_terminal_prefers_confirmed_then_newest() {
        let mk = |id: &str, state: &str, terminal: &str, last_seen: i64, confirmed: Option<i64>| {
            let mut r = fixture_rec(id, state, last_seen, None, None);
            r.terminal_id = terminal.to_string();
            r.confirmed_at = confirmed;
            r
        };
        let out = dedupe_open_by_terminal(vec![
            mk("conf-old", "open", "A", 1_000, Some(1_000)),
            mk("unconf-new", "open", "A", 2_000, None),
            mk("b-newer", "open", "B", 3_000, None),
            mk("b-older", "open", "B", 1_500, None),
            mk("empty-term-1", "open", "", 4_000, None),
            mk("empty-term-2", "open", "", 5_000, None),
        ]);
        let mut kept: Vec<String> = out.into_iter().map(|r| r.claude_session_id).collect();
        kept.sort();
        assert_eq!(
            kept,
            vec![
                "b-newer".to_string(),      // newest on terminal B
                "conf-old".to_string(),     // confirmed beats newer unconfirmed on A
                "empty-term-1".to_string(), // empty terminal_id: uncorrelatable, both pass
                "empty-term-2".to_string(),
            ]
        );
    }

    /// `rebind_terminal` re-points an open record at the terminal that now
    /// hosts it WITHOUT refreshing `last_seen_at` — the whole reason it exists
    /// rather than a `record_open` re-assert. A stale row must keep aging out
    /// on schedule even after a cold restore rebinds it, or ghost rows become
    /// immortal (the hazard that gated the re-assert to verified resumes and
    /// so let the orphan-PTY leak run unbounded).
    #[test]
    fn rebind_terminal_updates_binding_without_refreshing_liveness() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        store.record_open(rec("s1"));
        let before = store.get("s1").unwrap();
        assert_eq!(before.terminal_id, "term-abc");

        // Age `last_seen_at` so a refresh would be unmistakable.
        let aged = before.last_seen_at - 600_000;
        age_persisted_records(&store, &path, |m| {
            m.get_mut("s1").unwrap().last_seen_at = aged;
        });
        let store = SessionLifecycleStore::open(&path).unwrap();

        store.rebind_terminal("s1", "term-fresh", 5);

        let after = store.get("s1").unwrap();
        assert_eq!(after.terminal_id, "term-fresh", "binding must move");
        assert_eq!(after.zone_index, 5, "zone must move with the binding");
        assert_eq!(
            after.last_seen_at, aged,
            "rebind must NOT refresh last_seen_at (ghost rows would become immortal)"
        );
        assert_eq!(after.state, "open");
        assert_eq!(
            after.confirmed_at, before.confirmed_at,
            "rebind must not touch provenance"
        );

        // Survives a reload — the rebind is persisted, not in-memory only.
        let reloaded = SessionLifecycleStore::open(&path).unwrap();
        assert_eq!(reloaded.get("s1").unwrap().terminal_id, "term-fresh");
    }

    /// A rebind is a no-op on a record that is absent or no longer `open` —
    /// a closed row is not a restore target and must not be silently revived.
    #[test]
    fn rebind_terminal_is_a_noop_for_absent_or_closed_records() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let store = SessionLifecycleStore::open(&path).unwrap();

        // Absent — must not panic or create a row.
        store.rebind_terminal("ghost", "term-fresh", 1);
        assert!(store.get("ghost").is_none());

        store.record_open(rec("s2"));
        store.record_close("s2", "user-closed");
        store.rebind_terminal("s2", "term-fresh", 1);

        let after = store.get("s2").unwrap();
        assert_eq!(
            after.terminal_id, "term-abc",
            "a closed record must not be rebound"
        );
        assert_eq!(after.state, "closed");
    }
}
