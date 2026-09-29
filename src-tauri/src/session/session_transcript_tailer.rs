//! Interactive-pane transcript tailer. Plan
//! `2026-08-26-claude-code-session-repository-in-qontinui-web` Phase 2.
//!
//! ## What it does
//!
//! [`crate::terminal::transcript_watcher`] already keeps a live `notify` watch
//! over every Claude Code JSONL under every discovered config dir, and already
//! reads each append line-by-line off a byte cursor. This module is the second
//! consumer of that same read: it takes the bytes the watcher just consumed and
//! hands them to [`TranscriptEmitter::emit`] under the pane's
//! `claude_code_session_id`, so an operator's interactive tab reaches coord's
//! `stream='transcript'` lane the same way a workflow run already does.
//!
//! Everything downstream of that call is shipped and unchanged: the emitter
//! redacts, allocates the durable offset lane, and appends to the session
//! outbox; [`crate::session::coord_sync`] drains it to
//! `POST /sessions/:id/output`.
//!
//! ## No linkage code, by design
//!
//! Interactive panes are ALREADY registered with coord.
//! `AiCoordRegistrar::register_sniffed_session` — called from
//! `terminal::claude_resume_sniff` when the operator's `claude --resume <id>`
//! line is sniffed off the PTY — writes a `session_kind="terminal_claude"` row
//! with no `task_run_id`, anchored on `claude_code_session_id`. And
//! `AiCoordRegistrar::session_id_for` is keyed on `claude_session_id` for BOTH
//! planes. So `emit(<claude_code_session_id>, …)` resolves through the
//! existing R4 index and this module adds no registrar work whatsoever.
//!
//! ## Coverage is the thing to watch, not liveness
//!
//! That linkage is also the failure mode. The sniffer binds only the panes it
//! actually sniffed; a pane it missed has no R4 entry, and the emitter skips it
//! **silently** — one `debug!` line per session, which is indistinguishable
//! from "no panes are active" in a log. "The tailer is running" and "the tailer
//! is reaching every pane" are different claims, and the plan makes the second
//! one a Phase 2 exit criterion.
//!
//! So this module counts, per session key, whether an append was emitted or
//! dropped for want of a binding, and logs a periodic summary naming the
//! unbound session ids (see [`SessionTranscriptTailer::start_coverage_reporter`]).
//! A session that starts unbound and is bound later — the normal ordering, since
//! the file exists before the sniffer sees the resume line — moves out of the
//! unbound set on its next append, so a *persistently* unbound id is a real
//! coverage hole rather than a startup race.
//!
//! ## Liveness, not the archive body
//!
//! Per plan §5 ("Two ingest paths, one digest") this path stays REDACTED. It
//! serves live tailing and handoff scrollback. The archive body is written
//! verbatim from disk by the Phase 1 scanner, which is the corpus's sole body
//! writer — so nothing here may bypass `redact_secrets`, and nothing here
//! computes a `content_sha256`.
//!
//! ## Binding on request, and the pre-bind prefix
//!
//! Plan `2026-09-28-an-author-session-holds-its-worktree-slot-until-its-pr-lands-so-idle-sessions-starve-coord-fixers`
//! Phase 4.4. The sniffer binds only panes it sniffed, and appends made before
//! a bind were dropped for good — Phase 0 of that plan measured transcript
//! coverage at 0 of 134 author sessions. `POST /sessions/transcript-bind`
//! (`mcp::sessions`) closes that from the session's side:
//! [`SessionTranscriptTailer::bind_and_replay`] writes the binding through the
//! registrar's ONE binder ([`AiCoordRegistrar::bind_transcript_session`]) and
//! then replays the file's un-emitted prefix through THIS module's emit path —
//! the same redaction, the same offset lane, the same outbox. There is no
//! second producer: once bound, later appends arrive from the watcher as
//! before.
//!
//! ### File marks
//!
//! Replay and live tailing must never emit the same file bytes twice or out of
//! order, so every emit goes through one path (`emit_range`) under a
//! PER-SESSION lock and advances a durable FILE MARK: the byte offset in the
//! JSONL through which the file has been handed to the emitter. Marks are keyed
//! on `(session, path)` and persisted beside the outbox (the emitter's own
//! [`TranscriptOffsetLog`] type, reused as a durable key → i64 map), so a
//! re-bind after a runner restart replays only what no process emitted.
//!
//! - **Overlap.** A watcher batch that overlaps the mark (a replay read the
//!   same lines first) is trimmed to its unseen suffix; one wholly below it is
//!   dropped.
//! - **Gap.** A batch that starts ABOVE the mark (a sniffer-bound resumed
//!   session whose tail started at EOF, appends dropped while unbound, a
//!   batch deferred under contention) first has `[mark, batch start)` read
//!   from the file and emitted, under the same lock, so file order holds and
//!   the prefix is never skipped. If that fill cannot complete, the batch is
//!   not emitted either and the mark stays: the next batch or bind retries.
//! - **Rewrite.** Beside each mark sits a fingerprint of the file's first line.
//!   A mark is trusted only while the file is at least that long, the byte
//!   before the mark is a newline, and the first line still hashes the same;
//!   otherwise the file was rewritten and the mark resets to 0. The watcher's
//!   `truncated` flag alone does not reset a mark whose fingerprint still
//!   matches — a replay may already have reset and re-emitted that content.
//!
//! **Crash posture — at most once across a crash, never duplicated.** The mark
//! is reserved (fsynced) BEFORE the emitter queues the bytes, mirroring the
//! emitter's own write-ahead offset lane, and rolled back on a clean outbox
//! failure. A crash between the two therefore leaves a hole of at most one
//! batch rather than re-sending bytes under fresh `chunk_offset`s, which coord
//! could not dedupe.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::claude_session::coord_register::{AiCoordRegistrar, TranscriptBindRefusal};

use super::transcript_emitter::{TranscriptEmitter, TranscriptOffsetLog};

/// Upper bound on one replay emit. Each emit is one outbox append + fsync and
/// one offset reservation, so a multi-megabyte prefix is replayed as a handful
/// of batches rather than one allocation of the whole file. Batches end on a
/// line boundary; a single line longer than this is emitted alone.
const REPLAY_BATCH_BYTES: usize = 1024 * 1024;

/// Longest first line the rewrite fingerprint reads. A longer one yields no
/// fingerprint, and the mark is then guarded by the length and newline checks.
const FINGERPRINT_LINE_CAP: u64 = 256 * 1024;

/// How far into a JSONL the ownership check looks for a `cwd` record.
const OWNERSHIP_SCAN_LINES: usize = 200;

/// How often the coverage summary is logged. Long enough that an idle fleet
/// costs one line a minute, short enough that a rebuild's recovery window is
/// covered by several samples.
const COVERAGE_REPORT_INTERVAL: Duration = Duration::from_secs(60);

/// Cap on unbound session ids named in one summary line. A coverage hole is
/// actionable from a handful of ids; the count carries the magnitude.
const MAX_REPORTED_UNBOUND: usize = 16;

/// Tails watched Claude Code transcripts into the coord transcript stream.
/// Managed as Tauri state (`Arc<SessionTranscriptTailer>`) and handed to the
/// transcript watcher, which feeds it from its per-session tail loop
/// ([`Self::admit`] then [`Self::try_emit_batch`] / [`Self::emit_batch`]).
pub struct SessionTranscriptTailer {
    emitter: Arc<TranscriptEmitter>,
    registrar: Arc<AiCoordRegistrar>,
    coverage: Mutex<Coverage>,
    /// Per-session emit locks. Every emit — a watcher batch, a gap fill or a
    /// replay batch — holds its session's lock across the mark read, the emit
    /// and the mark write, which is what keeps the offset lane in FILE order
    /// when a replay and a live append race. One entry per session key.
    session_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Durable file marks and their fingerprints — module header, "File marks".
    marks: TranscriptOffsetLog,
}

/// What the caller of [`SessionTranscriptTailer::bind_and_replay`] asks for.
#[derive(Debug, Clone, Copy, Default)]
pub struct BindRequest {
    /// An existing coord session to adopt — ONLY one the caller has confirmed
    /// with coord belongs to this Claude session in this tenant.
    pub adopt: Option<Uuid>,
    /// The caller's resolved tenant, stamped on a fresh registration.
    pub tenant: Option<Uuid>,
}

/// What a successful [`SessionTranscriptTailer::bind_and_replay`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindOutcome {
    /// The coord session the key is bound to (the existing one on a re-bind).
    pub coord_session_id: Uuid,
    /// The key was already bound before this call (by the sniffer, a workflow
    /// registration, or an earlier bind).
    pub already_bound: bool,
    /// This call bound the key to the requested existing coord session.
    pub adopted: bool,
    /// File bytes handed to the emitter by this call's replay.
    pub replayed_bytes: u64,
    /// Outbox chunks those bytes became.
    pub replayed_chunks: u64,
    /// The file offset the replay stopped at when it could not reach the last
    /// complete line (an outbox refusal, a non-UTF-8 line). `None` = complete.
    pub replay_stopped_at: Option<u64>,
}

/// Why [`SessionTranscriptTailer::bind_and_replay`] refused. Typed so the
/// route can answer each with its own status and the caller never has to
/// parse prose to decide whether to retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindRefusal {
    /// Gate 1 (`Settings.cloud_sync_enabled`) is off. Nothing was written —
    /// not the binding, not a byte.
    SyncDisabled,
    /// The registrar declined to bind (`QONTINUI_SESSION_AUTOMATION_REGISTER`
    /// off, or its `Started` write failed).
    RegistrationDisabled,
    /// The coord session to adopt is already bound to a different key.
    CoordSessionInUse,
    /// The JSONL could not be read for the replay. The binding HAS been
    /// written; later appends are tailed.
    Unreadable(String),
}

/// Why [`locate_session_jsonl`] found no file to bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateRefusal {
    /// The transcript watcher watches no config dir (not started, or no
    /// `projects/` root existed when it did).
    NoConfigDirs,
    /// No `<config_dir>/projects/*/<id>.jsonl` exists under any watched root.
    NotFound { searched: Vec<String> },
    /// The file carries the runner's workflow-session marker. The watcher
    /// never tails those — the executor is their transcript producer — so
    /// binding it here would be a second producer.
    WorkflowSession { path: String },
}

impl LocateRefusal {
    /// Operator-readable detail for the route's `422` body.
    pub fn detail(&self) -> String {
        match self {
            Self::NoConfigDirs => "the transcript watcher is watching no Claude config dir on \
                 this runner (it is not running, or no config dir had a projects/ root when it \
                 started), so no JSONL would be tailed after a bind"
                .to_string(),
            Self::NotFound { searched } => format!(
                "no <config_dir>/projects/*/<claude_code_session_id>.jsonl exists under any \
                 config dir the transcript watcher watches ({}); a transcript outside them is \
                 not tailed",
                searched.join(", ")
            ),
            Self::WorkflowSession { path } => format!(
                "{path} is a runner WORKFLOW session: the watcher does not tail it (the \
                 executor produces its transcript), so binding it here would be a second producer"
            ),
        }
    }
}

/// Find the Claude Code JSONL for `claude_code_session_id` under the config
/// dirs the transcript watcher ACTUALLY watches
/// (`transcript_watcher::watched_config_dirs`) — a fresh discovery could name
/// a dir added since the watcher started, whose file would then never be
/// tailed. Newest by mtime when several projects hold the id. The id must
/// already be validated as a UUID by the caller: it becomes a file name.
pub fn locate_session_jsonl(
    watched_config_dirs: &[PathBuf],
    claude_code_session_id: &str,
) -> Result<PathBuf, LocateRefusal> {
    if watched_config_dirs.is_empty() {
        return Err(LocateRefusal::NoConfigDirs);
    }
    let file_name = format!("{claude_code_session_id}.jsonl");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for dir in watched_config_dirs {
        let Ok(projects) = std::fs::read_dir(dir.join("projects")) else {
            continue;
        };
        for project in projects.flatten() {
            let candidate = project.path().join(&file_name);
            let Ok(meta) = std::fs::metadata(&candidate) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, candidate));
            }
        }
    }
    let Some((_, path)) = best else {
        return Err(LocateRefusal::NotFound {
            searched: watched_config_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect(),
        });
    };
    if head_is_workflow_session(&path) {
        return Err(LocateRefusal::WorkflowSession {
            path: path.display().to_string(),
        });
    }
    Ok(path)
}

/// The watcher's own workflow test: the marker within the first five lines.
fn head_is_workflow_session(path: &Path) -> bool {
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let head: String = BufReader::new(f)
        .lines()
        .take(5)
        .map_while(Result::ok)
        .map(|l| l + "\n")
        .collect();
    crate::terminal::transcript::is_workflow_session_marker(&head)
}

/// Does the Claude Code session in `path` belong to `workdir` — the workdir a
/// proxy nonce was provisioned into? The ownership half of
/// `POST /sessions/transcript-bind`'s authorization.
///
/// The session's own `cwd` record decides when the file carries one (Claude
/// Code stamps it on every user/assistant record): it must EQUAL the workdir,
/// not merely sit under it, so a nonce for a workspace root cannot claim every
/// session in the worktrees below it. A file with no `cwd` record in its first
/// [`OWNERSHIP_SCAN_LINES`] falls back to the project directory, which Claude
/// Code names after the launch cwd.
pub fn jsonl_belongs_to_workdir(path: &Path, workdir: &str) -> bool {
    let want = normalize_dir(workdir);
    if want.is_empty() {
        return false;
    }
    if let Ok(f) = std::fs::File::open(path) {
        for line in BufReader::new(f.take(4 * 1024 * 1024))
            .lines()
            .take(OWNERSHIP_SCAN_LINES)
            .map_while(Result::ok)
        {
            if !line.contains("\"cwd\"") {
                continue;
            }
            if let Some(cwd) = serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .and_then(|v| v.get("cwd").and_then(|c| c.as_str()).map(str::to_string))
            {
                return normalize_dir(&cwd) == want;
            }
        }
    }
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|dir| dir == crate::terminal::transcript_watcher::encode_for_lookup(workdir))
}

/// Separator-, trailing-slash- and (on Windows) case-insensitive directory form.
fn normalize_dir(dir: &str) -> String {
    let t = dir.trim().replace('\\', "/");
    let t = t.trim_end_matches('/');
    if cfg!(windows) {
        t.to_ascii_lowercase()
    } else {
        t.to_string()
    }
}

/// SHA-256 of the file's first complete line, folded to an `i64` so it fits
/// the mark log. Stable across runner builds (unlike `DefaultHasher`), which
/// matters because it is persisted. `None` when the first line is incomplete,
/// longer than [`FINGERPRINT_LINE_CAP`], or unreadable.
fn first_line_fingerprint(path: &Path) -> Option<i64> {
    let f = std::fs::File::open(path).ok()?;
    let mut line = Vec::new();
    BufReader::new(f.take(FINGERPRINT_LINE_CAP))
        .read_until(b'\n', &mut line)
        .ok()?;
    if line.last() != Some(&b'\n') {
        return None;
    }
    let digest = Sha256::digest(&line);
    let mut eight = [0u8; 8];
    eight.copy_from_slice(&digest[..8]);
    Some(i64::from_le_bytes(eight))
}

/// Is the byte just before `offset` a newline? (`offset` > 0.)
fn newline_precedes(path: &Path, offset: u64) -> Option<bool> {
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(offset - 1)).ok()?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b).ok()?;
    Some(b[0] == b'\n')
}

/// One replay pass over `[from, until)`.
#[derive(Debug, Default)]
struct ReplayPass {
    bytes: u64,
    chunks: u64,
    stopped_at: Option<u64>,
}

/// The mark key: session AND path, so a moved or re-created transcript for the
/// same session never inherits another file's offset.
fn mark_key(session_key: &str, path: &Path) -> String {
    format!("{session_key}\u{1f}{}", path.display())
}

/// The key the first-line fingerprint for mark `mk` is stored under.
fn fingerprint_key(mk: &str) -> String {
    format!("fp\u{1f}{mk}")
}

/// Mutable coverage state. Session-id sets rather than counters, because the
/// question the exit criterion asks is "which panes are we missing", not "how
/// many appends were dropped".
#[derive(Default)]
struct Coverage {
    /// Session keys at least one append was emitted for.
    tailed: HashSet<String>,
    /// Session keys seen with NO coord binding at their last append. Entries
    /// leave this set the moment a binding appears.
    unbound: BTreeSet<String>,
    appends_emitted: u64,
    bytes_emitted: u64,
    appends_skipped_unbound: u64,
    appends_skipped_gate_off: u64,
    /// Gate 1 as observed at the last append. `None` until an append is seen —
    /// which is why the report models it as an option: "no data yet" and
    /// "consent withheld" are different answers and a bare `false` conflates
    /// them.
    cloud_sync_enabled: Option<bool>,
    /// Counter total at the last summary, so the reporter can stay quiet on an
    /// idle fleet without losing the ability to say "still nothing bound".
    last_reported_total: u64,
}

/// Point-in-time coverage snapshot. `Serialize` so a health/diagnostic surface
/// can serve it without reformatting.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageReport {
    /// Gate 1 as observed at the last append; `None` before the first one. A
    /// zero-everything report means something different in each of the three
    /// states.
    pub cloud_sync_enabled: Option<bool>,
    /// Distinct sessions whose appends reached the outbox.
    pub sessions_tailed: usize,
    /// Distinct sessions currently missing a coord binding.
    pub sessions_unbound: usize,
    /// Up to [`MAX_REPORTED_UNBOUND`] of those ids, for the operator to chase.
    pub unbound_session_ids: Vec<String>,
    pub appends_emitted: u64,
    pub bytes_emitted: u64,
    pub appends_skipped_unbound: u64,
    pub appends_skipped_gate_off: u64,
}

impl SessionTranscriptTailer {
    pub fn new(emitter: Arc<TranscriptEmitter>, registrar: Arc<AiCoordRegistrar>) -> Self {
        let marks = TranscriptOffsetLog::open(emitter.file_mark_log_path());
        Self {
            emitter,
            registrar,
            coverage: Mutex::new(Coverage::default()),
            session_locks: Mutex::new(HashMap::new()),
            marks,
        }
    }

    /// Feed one batch of newly-appended transcript bytes for `session_key`
    /// (the JSONL stem — the pane's `claude_code_session_id`), BLOCKING on the
    /// session lock. The watcher does not call this from its async loop — it
    /// uses [`Self::admit`] + [`Self::try_emit_batch`] and falls back to
    /// [`Self::emit_batch`] on a blocking thread; this is the one-call form
    /// for synchronous callers and tests.
    ///
    /// `file_start` is the JSONL byte offset `appended` begins at, and
    /// `truncated` says the watcher saw the file shrink and restarted its
    /// cursor at 0 before this read — together they place the batch against
    /// the session's file mark (module header, "File marks").
    pub fn on_appended(
        &self,
        session_key: &str,
        path: &Path,
        file_start: u64,
        appended: &str,
        truncated: bool,
    ) {
        self.on_appended_gated(
            session_key,
            path,
            file_start,
            appended,
            truncated,
            crate::settings::get_cloud_sync_enabled(),
        );
    }

    /// [`Self::on_appended`] with Gate 1 supplied, so tests can drive both
    /// arms without touching the machine's real `settings.json` (the same
    /// split, for the same reason, as `TranscriptEmitter::emit_inner`).
    pub(crate) fn on_appended_gated(
        &self,
        session_key: &str,
        path: &Path,
        file_start: u64,
        appended: &str,
        truncated: bool,
        cloud_sync_enabled: bool,
    ) {
        if self.admit(session_key, appended.len(), cloud_sync_enabled) {
            self.emit_batch(session_key, path, file_start, appended, truncated);
        }
    }

    /// Gate 1 and the binding, with the coverage bookkeeping. `true` = this
    /// batch should be emitted (then call [`Self::try_emit_batch`] or
    /// [`Self::emit_batch`]); `false` = drop it. Never blocks on a session
    /// lock.
    ///
    /// Gate 1 (`Settings.cloud_sync_enabled`) is resolved by the caller rather
    /// than inside the emitter because the coverage summary needs its value —
    /// "off" and "on but reaching nobody" are different diagnoses. The emit
    /// path then calls `emit_inner`, which by contract does NOT re-check it.
    pub fn admit(&self, session_key: &str, len: usize, cloud_sync_enabled: bool) -> bool {
        if len == 0 {
            return false;
        }
        if !cloud_sync_enabled {
            let mut cov = self.lock_coverage();
            cov.cloud_sync_enabled = Some(false);
            cov.appends_skipped_gate_off += 1;
            return false;
        }

        // Resolve the binding HERE as well as inside the emitter. The
        // duplicate lookup is the price of coverage: the emitter's own skip is
        // a once-per-session debug line, which cannot answer "is every live
        // pane being tailed right now".
        let bound = self.registrar.session_id_for(session_key).is_some();

        let mut cov = self.lock_coverage();
        cov.cloud_sync_enabled = Some(true);
        if bound {
            cov.tailed.insert(session_key.to_string());
            cov.unbound.remove(session_key);
            cov.appends_emitted += 1;
            cov.bytes_emitted += len as u64;
        } else {
            cov.unbound.insert(session_key.to_string());
            cov.appends_skipped_unbound += 1;
        }
        bound
    }

    /// The NON-BLOCKING emit the watcher's async loop tries first. Emits and
    /// returns `true` only on the common case: the session lock is free, the
    /// watcher did not report a truncation, and the batch starts exactly at
    /// the mark (no overlap to trim, no gap to fill, nothing to re-validate).
    /// Anything else returns `false` having done NOTHING, and the caller runs
    /// [`Self::emit_batch`] on a blocking thread — so a long replay holding
    /// the lock, or a gap fill reading megabytes, never stalls a runtime
    /// worker, and no batch is ever dropped for contention.
    pub fn try_emit_batch(
        &self,
        session_key: &str,
        path: &Path,
        file_start: u64,
        appended: &str,
        truncated: bool,
    ) -> bool {
        if truncated {
            return false;
        }
        let lock = self.session_lock(session_key);
        let _held = match lock.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        let mk = mark_key(session_key, path);
        if self.current_mark(&mk) != file_start {
            return false;
        }
        let _ = self.emit_range(session_key, &mk, path, file_start, appended);
        true
    }

    /// The full, BLOCKING emit of one watcher batch: validate the mark
    /// against the file (rewrite detection), fill any gap below the batch from
    /// the file, then emit the batch's unseen part — all under the session
    /// lock. Synchronous file I/O; call it from a blocking context.
    pub fn emit_batch(
        &self,
        session_key: &str,
        path: &Path,
        file_start: u64,
        appended: &str,
        truncated: bool,
    ) {
        let lock = self.session_lock(session_key);
        let _held = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mk = mark_key(session_key, path);
        let mark = self.validated_mark(&mk, path, truncated);
        if file_start > mark {
            // Gap: the bytes below this batch were never emitted. Emit them
            // first, from the file, so the lane stays in file order.
            match self.replay_locked(session_key, &mk, path, mark, Some(file_start)) {
                Ok(pass) if pass.stopped_at.is_none() => {}
                other => {
                    tracing::warn!(
                        session_key,
                        mark,
                        file_start,
                        outcome = ?other,
                        "session_transcript_tailer: could not fill the gap below a batch — \
                         batch held back (mark unchanged; the next batch or a bind retries)"
                    );
                    return;
                }
            }
        }
        let _ = self.emit_range(session_key, &mk, path, file_start, appended);
    }

    /// The per-session emit lock for `session_key`.
    fn session_lock(&self, session_key: &str) -> Arc<Mutex<()>> {
        self.session_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session_key.to_string())
            .or_default()
            .clone()
    }

    /// The mark as recorded, `0` when none.
    fn current_mark(&self, mk: &str) -> u64 {
        self.marks
            .get(mk)
            .map(|m| u64::try_from(m).unwrap_or(0))
            .unwrap_or(0)
    }

    /// The mark, trusted only if it still describes THIS file — module
    /// header, "Rewrite". A rewritten file resets the mark to 0.
    fn validated_mark(&self, mk: &str, path: &Path, truncated: bool) -> u64 {
        let mark = self.current_mark(mk);
        if mark == 0 {
            return 0;
        }
        let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else {
            return mark; // unreadable now; the emit/replay reports it
        };
        let stored = self.marks.get(&fingerprint_key(mk));
        let now = first_line_fingerprint(path);
        let reason = if len < mark {
            Some("file shorter than the mark")
        } else if newline_precedes(path, mark) == Some(false) {
            Some("mark is not on a line boundary")
        } else if matches!((stored, now), (Some(s), Some(n)) if s != n) {
            Some("first line changed")
        } else if truncated && (stored.is_none() || now.is_none()) {
            // No fingerprint to overrule the watcher's own truncation report.
            Some("watcher reported truncation and no fingerprint is available")
        } else {
            None
        };
        match reason {
            Some(reason) => {
                tracing::info!(
                    mark,
                    len,
                    reason,
                    "session_transcript_tailer: transcript was rewritten — file mark reset to 0"
                );
                self.marks.set(mk, 0);
                0
            }
            None => mark,
        }
    }

    /// Hand the file range `[file_start, file_start + text.len())` to the
    /// emitter, minus whatever the mark says is already emitted. THE one emit
    /// path for live batches, gap fills and replay. The caller holds the
    /// session lock and has already filled any gap, so the range never starts
    /// above the mark.
    ///
    /// The mark is reserved BEFORE the emit and rolled back if nothing was
    /// queued (module header, "Crash posture"). Returns `(file bytes emitted,
    /// chunks queued)`, or `None` when the emitter queued nothing.
    fn emit_range(
        &self,
        session_key: &str,
        mk: &str,
        path: &Path,
        file_start: u64,
        text: &str,
    ) -> Option<(u64, u64)> {
        let mark = self.current_mark(mk);
        let file_end = file_start + text.len() as u64;
        let from = mark.max(file_start);
        if from >= file_end {
            return Some((0, 0)); // wholly emitted already
        }
        let skip = usize::try_from(from - file_start).unwrap_or(usize::MAX);
        let Some(unseen) = text.get(skip..) else {
            // `validated_mark` keeps marks on line boundaries, which are
            // character boundaries; reaching here means the batch itself is
            // not what the file holds. Emit nothing rather than guess.
            tracing::warn!(
                session_key,
                file_start,
                mark,
                "session_transcript_tailer: mark falls inside a character of this batch — skipped"
            );
            return Some((0, 0));
        };
        if mark == 0 {
            if let Some(fp) = first_line_fingerprint(path) {
                self.marks.set(&fingerprint_key(mk), fp);
            }
        }
        // Write-ahead reservation.
        self.marks.set(mk, file_end as i64);
        let chunks = self.emitter.emit_inner(session_key, unseen);
        if chunks == 0 {
            self.marks.set(mk, mark as i64);
            return None;
        }
        Some((file_end - from, chunks as u64))
    }

    /// Bind `session_key` (a Claude Code session id, the JSONL stem) into the
    /// transcript lane and replay the part of `path` not yet emitted. The
    /// route behind `POST /sessions/transcript-bind`; see the module header.
    ///
    /// Order is load-bearing: Gate 1 first (a disabled toggle writes nothing,
    /// the binding included), then the session lock, then the binding, then
    /// the replay — all under the one lock a concurrent watcher batch also
    /// takes, so the lane stays in file order. The replay covers whole lines
    /// only, exactly as the watcher reads them.
    ///
    /// A key the registrar ALREADY maps is never re-registered: its existing
    /// coord session is returned (`already_bound`), whatever `req.adopt` says.
    ///
    /// Synchronous file I/O; call it from a blocking context.
    pub fn bind_and_replay(
        &self,
        session_key: &str,
        path: &Path,
        req: BindRequest,
        cloud_sync_enabled: bool,
    ) -> Result<BindOutcome, BindRefusal> {
        if !cloud_sync_enabled {
            return Err(BindRefusal::SyncDisabled);
        }
        let lock = self.session_lock(session_key);
        let _held = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let existing = self.registrar.session_id_for(session_key);
        let already_bound = existing.is_some();
        let (coord_session_id, adopted) = match existing {
            Some(id) => (id, false),
            None => {
                let id = self
                    .registrar
                    .bind_transcript_session(session_key, req.adopt, req.tenant)
                    .map_err(|e| match e {
                        TranscriptBindRefusal::RegistrationDisabled => {
                            BindRefusal::RegistrationDisabled
                        }
                        TranscriptBindRefusal::CoordSessionInUse => BindRefusal::CoordSessionInUse,
                    })?;
                (id, req.adopt == Some(id))
            }
        };
        {
            let mut cov = self.lock_coverage();
            cov.unbound.remove(session_key);
            cov.tailed.insert(session_key.to_string());
        }

        let mk = mark_key(session_key, path);
        let mark = self.validated_mark(&mk, path, false);
        let pass = self
            .replay_locked(session_key, &mk, path, mark, None)
            .map_err(|e| BindRefusal::Unreadable(format!("{}: {e}", path.display())))?;
        tracing::info!(
            session_key,
            coord_session = %coord_session_id,
            already_bound,
            adopted,
            replayed_bytes = pass.bytes,
            replayed_chunks = pass.chunks,
            replay_stopped_at = ?pass.stopped_at,
            path = %path.display(),
            "session_transcript_tailer: transcript bound on request"
        );
        Ok(BindOutcome {
            coord_session_id,
            already_bound,
            adopted,
            replayed_bytes: pass.bytes,
            replayed_chunks: pass.chunks,
            replay_stopped_at: pass.stopped_at,
        })
    }

    /// Read `path` over `[from, until)` — `until = None` meaning "through the
    /// last complete line" — and feed it through [`Self::emit_range`] in
    /// bounded, line-aligned batches. Caller holds the session lock.
    ///
    /// `stopped_at` is set when the pass could not cover its range: the
    /// emitter queued nothing (the batch's start), a non-UTF-8 line, or — for a
    /// bounded gap fill — the file not holding complete lines up to `until`.
    fn replay_locked(
        &self,
        session_key: &str,
        mk: &str,
        path: &Path,
        from: u64,
        until: Option<u64>,
    ) -> std::io::Result<ReplayPass> {
        let mut file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        let end = until.unwrap_or(len).min(len);
        let mut pass = ReplayPass::default();
        if from >= end {
            if until.is_some_and(|u| from < u) {
                pass.stopped_at = Some(from);
            }
            return Ok(pass);
        }
        file.seek(SeekFrom::Start(from))?;
        let mut reader = BufReader::new(file.take(end - from));

        let mut pos = from;
        let mut batch = String::new();
        let mut batch_start = pos;
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            // EOF, or an unterminated tail the writer has not finished: the
            // watcher delivers that line once its newline lands.
            let complete = n > 0 && line.last() == Some(&b'\n');
            let text = if complete {
                std::str::from_utf8(&line).ok()
            } else {
                None
            };
            if let Some(text) = text {
                batch.push_str(text);
                pos += n as u64;
            } else if complete {
                // The watcher's `read_line` stops at an invalid-UTF-8 line
                // too, so the lane never gets past it either way.
                tracing::warn!(
                    session_key,
                    offset = pos,
                    "session_transcript_tailer: replay stopped at a non-UTF-8 line"
                );
                pass.stopped_at = Some(pos);
            }
            let stop = text.is_none();
            if !batch.is_empty() && (stop || batch.len() >= REPLAY_BATCH_BYTES) {
                match self.emit_range(session_key, mk, path, batch_start, &batch) {
                    Some((b, c)) => {
                        pass.bytes += b;
                        pass.chunks += c;
                    }
                    None => {
                        // Nothing queued: stop BEFORE this batch rather than
                        // skip ahead of a hole.
                        pass.stopped_at = Some(batch_start);
                        return Ok(pass);
                    }
                }
                batch.clear();
                batch_start = pos;
            }
            if stop {
                break;
            }
        }
        if pass.stopped_at.is_none() && until.is_some_and(|u| pos < u) {
            pass.stopped_at = Some(pos);
        }
        Ok(pass)
    }

    /// Current coverage. Cheap; safe to call from a command handler.
    pub fn coverage(&self) -> CoverageReport {
        let cov = self.lock_coverage();
        CoverageReport {
            cloud_sync_enabled: cov.cloud_sync_enabled,
            sessions_tailed: cov.tailed.len(),
            sessions_unbound: cov.unbound.len(),
            unbound_session_ids: cov
                .unbound
                .iter()
                .take(MAX_REPORTED_UNBOUND)
                .cloned()
                .collect(),
            appends_emitted: cov.appends_emitted,
            bytes_emitted: cov.bytes_emitted,
            appends_skipped_unbound: cov.appends_skipped_unbound,
            appends_skipped_gate_off: cov.appends_skipped_gate_off,
        }
    }

    /// Spawn the periodic coverage summary. One task for the process lifetime.
    ///
    /// It logs only when something moved since the last sample, EXCEPT that a
    /// run with unbound sessions and nothing emitted keeps reporting — that is
    /// precisely the state ("running, reaching nobody") a quiet log would hide,
    /// and it is the one the exit criterion is about. It is a `warn!` in that
    /// state and an `info!` otherwise, so the coverage hole is greppable
    /// without reading counts.
    pub fn start_coverage_reporter(self: &Arc<Self>) {
        let tailer = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(COVERAGE_REPORT_INTERVAL).await;
                tailer.report_coverage_once();
            }
        });
    }

    /// One summary emission. Split out from the loop so it is directly
    /// testable without a timer.
    fn report_coverage_once(&self) {
        let report = self.coverage();
        let total = report.appends_emitted
            + report.appends_skipped_unbound
            + report.appends_skipped_gate_off;

        let mut cov = self.lock_coverage();
        let moved = total != cov.last_reported_total;
        cov.last_reported_total = total;
        drop(cov);

        let blind = report.sessions_unbound > 0 && report.sessions_tailed == 0;
        if !moved && !blind {
            return;
        }

        if blind {
            tracing::warn!(
                cloud_sync_enabled = ?report.cloud_sync_enabled,
                sessions_unbound = report.sessions_unbound,
                unbound_session_ids = %report.unbound_session_ids.join(","),
                appends_skipped_unbound = report.appends_skipped_unbound,
                "session_transcript_tailer: RUNNING BUT REACHING NO PANE — every watched \
                 transcript lacks a coord session binding, so nothing is being synced. The \
                 binding is written by the claude --resume sniffer \
                 (claude_resume_sniff -> AiCoordRegistrar::register_sniffed_session); a pane \
                 launched without a sniffable resume line never gets one."
            );
        } else {
            tracing::info!(
                cloud_sync_enabled = ?report.cloud_sync_enabled,
                sessions_tailed = report.sessions_tailed,
                sessions_unbound = report.sessions_unbound,
                unbound_session_ids = %report.unbound_session_ids.join(","),
                appends_emitted = report.appends_emitted,
                bytes_emitted = report.bytes_emitted,
                appends_skipped_unbound = report.appends_skipped_unbound,
                appends_skipped_gate_off = report.appends_skipped_gate_off,
                "session_transcript_tailer: coverage"
            );
        }
    }

    fn lock_coverage(&self) -> std::sync::MutexGuard<'_, Coverage> {
        self.coverage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl std::fmt::Debug for SessionTranscriptTailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cov = self.lock_coverage();
        f.debug_struct("SessionTranscriptTailer")
            .field("sessions_tailed", &cov.tailed.len())
            .field("sessions_unbound", &cov.unbound.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::local_store::OutboxWriter;
    use crate::session::SessionEventKind;
    use tempfile::tempdir;
    use uuid::Uuid;

    /// A transcript path that does not exist — for the cases that exercise
    /// gating and binding but never need the file.
    const NO_FILE: &str = "/nonexistent/qontinui-tailer-test/none.jsonl";

    /// Build a tailer over a tempdir outbox, mirroring production wiring
    /// (registrar and emitter share the SAME outbox `Arc`, and the emitter
    /// derives its durable offset sidecar from that outbox's path).
    ///
    /// Calling this twice against one `dir` models a runner RESTART: fresh
    /// in-memory state everywhere, same files on disk.
    fn tailer(
        dir: &std::path::Path,
    ) -> (
        Arc<SessionTranscriptTailer>,
        Arc<AiCoordRegistrar>,
        Arc<OutboxWriter>,
    ) {
        let outbox = Arc::new(OutboxWriter::open(dir.join("outbox.jsonl")).unwrap());
        let machine_id = Uuid::new_v4();
        let registrar = Arc::new(AiCoordRegistrar::with_tenant_resolver(
            outbox.clone(),
            machine_id,
            || None,
        ));
        let emitter = Arc::new(TranscriptEmitter::new(
            outbox.clone(),
            machine_id,
            registrar.clone(),
        ));
        (
            Arc::new(SessionTranscriptTailer::new(emitter, registrar.clone())),
            registrar,
            outbox,
        )
    }

    /// Register an interactive pane exactly as `claude_resume_sniff` does.
    /// `register_sniffed_session` is gated on the process-global
    /// `QONTINUI_SESSION_AUTOMATION_REGISTER` env var that the coord_register
    /// suite toggles under its own lock, so retry briefly rather than flake.
    fn sniff_register(registrar: &AiCoordRegistrar, claude_session_id: &str) -> Uuid {
        (0..100)
            .find_map(|_| {
                registrar
                    .register_sniffed_session(claude_session_id, "Terminal 1", None)
                    .or_else(|| {
                        std::thread::sleep(Duration::from_millis(10));
                        None
                    })
            })
            .expect("register_sniffed_session")
    }

    /// Transcript chunk offsets grouped by the coord session that emitted
    /// them, in seq order within each — the grain `OutboxWriter::pending`
    /// actually guarantees. Use this wherever a case spans more than one coord
    /// session; the order BETWEEN sessions is decided by comparing two random
    /// UUIDv7 low halves and is not a property worth asserting.
    fn transcript_chunks(outbox: &OutboxWriter) -> std::collections::HashMap<Uuid, Vec<i64>> {
        let mut by_session: std::collections::HashMap<Uuid, Vec<i64>> =
            std::collections::HashMap::new();
        for r in outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::OutputChunk.as_str())
        {
            by_session
                .entry(r.session_id)
                .or_default()
                .push(r.payload["chunk_offset"].as_i64().unwrap());
        }
        by_session
    }

    fn transcript_offsets(outbox: &OutboxWriter) -> Vec<i64> {
        outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::OutputChunk.as_str())
            .map(|r| r.payload["chunk_offset"].as_i64().unwrap())
            .collect()
    }

    /// An UNBOUND pane is counted as a coverage hole rather than disappearing
    /// into a debug line, and binding it later clears the hole. This is the
    /// exit-criterion signal: "running" vs "running and reaching every pane".
    #[test]
    fn unbound_pane_is_counted_then_cleared_on_binding() {
        let dir = tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();

        t.on_appended_gated(
            &csid,
            NO_FILE.as_ref(),
            0,
            "{\"type\":\"user\"}\n",
            false,
            true,
        );
        let r = t.coverage();
        assert_eq!(r.sessions_unbound, 1, "unbound pane is visible");
        assert_eq!(r.unbound_session_ids, vec![csid.clone()]);
        assert_eq!(r.sessions_tailed, 0);
        assert_eq!(r.appends_skipped_unbound, 1);
        assert!(
            transcript_offsets(&outbox).is_empty(),
            "an unbound pane writes nothing"
        );

        sniff_register(&registrar, &csid);
        t.on_appended_gated(
            &csid,
            NO_FILE.as_ref(),
            16,
            "{\"type\":\"assistant\"}\n",
            false,
            true,
        );
        let r = t.coverage();
        assert_eq!(r.sessions_unbound, 0, "binding clears the coverage hole");
        assert_eq!(r.sessions_tailed, 1);
        assert_eq!(r.appends_emitted, 1);
    }

    /// Gate 1 off: nothing is written, and the skip is counted under its OWN
    /// label so a silent run is diagnosable as consent rather than as a
    /// coverage hole.
    #[test]
    fn gate_off_writes_nothing_and_is_counted_separately() {
        let dir = tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();
        sniff_register(&registrar, &csid);

        t.on_appended_gated(
            &csid,
            NO_FILE.as_ref(),
            0,
            "should not leave the machine",
            false,
            false,
        );

        assert!(transcript_offsets(&outbox).is_empty());
        let r = t.coverage();
        assert_eq!(r.cloud_sync_enabled, Some(false));
        assert_eq!(r.appends_skipped_gate_off, 1);
        assert_eq!(r.appends_skipped_unbound, 0);
        assert_eq!(r.sessions_tailed, 0);
        assert_eq!(r.sessions_unbound, 0);
    }

    /// A bound pane's appends land as transcript chunks with the emitter's
    /// monotonic offsets — the end-to-end Phase 2 path, minus the drain.
    #[test]
    fn bound_pane_appends_reach_the_outbox() {
        let dir = tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();
        sniff_register(&registrar, &csid);

        t.on_appended_gated(&csid, NO_FILE.as_ref(), 0, "aaaa", false, true);
        t.on_appended_gated(&csid, NO_FILE.as_ref(), 4, "bb", false, true);

        assert_eq!(transcript_offsets(&outbox), vec![0, 4]);
        let r = t.coverage();
        assert_eq!(r.appends_emitted, 2);
        assert_eq!(r.bytes_emitted, 6);
        assert_eq!(r.cloud_sync_enabled, Some(true));
    }

    /// **Restart mid-session** — the Phase 2(a) case. A pane is tailed, the
    /// runner process dies (a new tailer + emitter + registrar over the SAME
    /// outbox dir), the operator re-opens the pane so the sniffer registers it
    /// again — which mints a DIFFERENT coord session id — and tailing resumes.
    /// The offsets must continue past every byte already emitted rather than
    /// restarting at 0.
    #[test]
    fn restart_mid_session_resumes_the_offset_lane() {
        let dir = tempdir().unwrap();
        let csid = Uuid::new_v4().to_string();

        // ── Process 1 ────────────────────────────────────────────────────
        let first_coord_id = {
            let (t, registrar, outbox) = tailer(dir.path());
            let sid = sniff_register(&registrar, &csid);
            t.on_appended_gated(&csid, NO_FILE.as_ref(), 0, "0123456789", false, true);
            assert_eq!(transcript_offsets(&outbox), vec![0]);
            sid
        };

        // ── Process 2: same machine, same outbox, brand-new in-memory state ─
        let (t2, registrar2, outbox2) = tailer(dir.path());
        let second_coord_id = sniff_register(&registrar2, &csid);
        assert_ne!(
            first_coord_id, second_coord_id,
            "the registrar mints a fresh coord session id per process — which is \
             exactly why the lane cannot be keyed on it"
        );
        t2.on_appended_gated(&csid, NO_FILE.as_ref(), 10, "abcde", false, true);

        // The post-restart chunk continues the lane at 10, NOT at 0. At 0 it
        // would collide with the pre-restart chunk under any read that joins
        // the two coord sessions by claude_code_session_id, and coord's
        // ON CONFLICT DO NOTHING would silently drop it.
        //
        // Joined by SESSION rather than compared as a flat list, because the
        // flat comparison was a coin flip per run (observed on
        // qontinui-runner#1583 as `left: [10, 0]`, coord finding `805876ba`).
        // The cause is NOT test-run order and NOT append order:
        // `OutboxWriter::pending` sorts by `(session_id, seq)`
        // (`session/local_store.rs`), so the order between two sessions is
        // decided entirely by comparing their two ids — and both are UUIDv7s
        // from `session::uuid_v7()`, which uses `uuid::NoContext`
        // (`usable_bits() == 0`), so two minted in the SAME MILLISECOND differ
        // only in 74 RANDOM low bits. The registrar mints one per
        // process and this test runs both within a millisecond on a fast
        // filesystem, so which id sorts first is random per run. Running the
        // test alone therefore neither reproduces the failure nor proves a
        // fix.
        //
        // Joining by session is also STRICTER than sorting: it pins that the
        // post-restart chunk is the one at 10, which is the property the
        // test's name claims. A sorted `vec![0, 10]` would still pass if the
        // two offsets had swapped owners.
        let by_session = transcript_chunks(&outbox2);
        assert_eq!(
            by_session.get(&first_coord_id).map(Vec::as_slice),
            Some(&[0][..])
        );
        assert_eq!(
            by_session.get(&second_coord_id).map(Vec::as_slice),
            Some(&[10][..]),
            "offset lane survives the restart"
        );
        assert_eq!(t2.coverage().appends_emitted, 1);
    }

    // ── Phase 4.4: POST /sessions/transcript-bind ────────────────────────

    /// A Claude Code JSONL at `<dir>/cfg/projects/proj/<csid>.jsonl`, the
    /// layout `locate_session_jsonl` searches.
    fn jsonl(dir: &std::path::Path, csid: &str, body: &str) -> PathBuf {
        let p = dir.join("cfg").join("projects").join("proj");
        std::fs::create_dir_all(&p).unwrap();
        let f = p.join(format!("{csid}.jsonl"));
        std::fs::write(&f, body).unwrap();
        f
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    /// `bind_and_replay` with Gate 1 on, retried across the process-global
    /// registration kill switch the coord_register suite toggles (the same
    /// reason `sniff_register` retries).
    fn bind(t: &SessionTranscriptTailer, csid: &str, path: &Path) -> BindOutcome {
        for _ in 0..200 {
            match t.bind_and_replay(csid, path, BindRequest::default(), true) {
                Ok(o) => return o,
                Err(BindRefusal::RegistrationDisabled) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("bind refused: {e:?}"),
            }
        }
        panic!("registration stayed disabled")
    }

    /// Every transcript chunk queued for `coord`, in the outbox's own (seq)
    /// order, as `(chunk_offset, decoded text)`.
    fn chunks_for(outbox: &OutboxWriter, coord: Uuid) -> Vec<(i64, String)> {
        use base64::Engine as _;
        outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| {
                r.event_kind == SessionEventKind::OutputChunk.as_str() && r.session_id == coord
            })
            .map(|r| {
                let b = base64::engine::general_purpose::STANDARD
                    .decode(r.payload["payload_b64"].as_str().unwrap())
                    .unwrap();
                (
                    r.payload["chunk_offset"].as_i64().unwrap(),
                    String::from_utf8(b).unwrap(),
                )
            })
            .collect()
    }

    /// The transcript as the outbox will deliver it: chunks concatenated in
    /// QUEUE order, with the offset lane asserted contiguous and ascending in
    /// that same order — so a reordered or duplicated range fails here.
    fn delivered(outbox: &OutboxWriter, coord: Uuid) -> String {
        let mut next = None;
        let mut out = String::new();
        for (offset, text) in chunks_for(outbox, coord) {
            if let Some(n) = next {
                assert_eq!(offset, n, "offset lane out of order or gapped");
            }
            next = Some(offset + text.len() as i64);
            out.push_str(&text);
        }
        out
    }

    /// The replayed prefix goes through the emitter's redaction: a secret in
    /// the pre-bind part of the JSONL never reaches the outbox file.
    #[test]
    fn replayed_prefix_is_redacted() {
        let dir = tempdir().unwrap();
        let (t, _registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();
        let path = jsonl(
            dir.path(),
            &csid,
            "{\"type\":\"user\",\"text\":\"API_KEY=sk-oops-planted password: hunter2\"}\n",
        );

        let o = bind(&t, &csid, &path);
        assert!(!o.already_bound);
        assert!(o.replayed_chunks >= 1 && o.replayed_bytes > 0);

        let text = delivered(&outbox, o.coord_session_id);
        assert!(
            text.contains("\"type\":\"user\""),
            "the prefix was replayed: {text}"
        );
        assert!(!text.contains("sk-oops-planted"), "got: {text}");
        assert!(!text.contains("hunter2"), "got: {text}");
        let raw = std::fs::read_to_string(dir.path().join("outbox.jsonl")).unwrap();
        assert!(
            !raw.contains("sk-oops-planted"),
            "secret reached the outbox file"
        );
    }

    /// Bind, then the watcher's next batch: each file byte reaches the outbox
    /// exactly once and in file order. The watcher's batch deliberately
    /// OVERLAPS the replayed prefix (its cursor sat inside it — it read those
    /// lines while the session was unbound), which is the race the file mark
    /// exists for.
    #[test]
    fn bind_then_append_yields_each_byte_once_in_order() {
        let dir = tempdir().unwrap();
        let (t, _registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();
        let (l1, l2, l3) = ("{\"n\":1}\n", "{\"n\":2}\n", "{\"n\":3}\n");
        let path = jsonl(dir.path(), &csid, &format!("{l1}{l2}"));

        // Pre-bind: the watcher read l1 and dropped it (unbound).
        t.on_appended_gated(&csid, &path, 0, l1, false, true);
        assert!(transcript_offsets(&outbox).is_empty());

        let o = bind(&t, &csid, &path);
        assert_eq!(o.replayed_bytes, (l1.len() + l2.len()) as u64);

        // The writer appends l3; the watcher's cursor was after l1, so its
        // batch is l2 + l3 starting at l1.len().
        append(&path, l3);
        t.on_appended_gated(
            &csid,
            &path,
            l1.len() as u64,
            &format!("{l2}{l3}"),
            false,
            true,
        );
        // A re-delivery of an old batch is inert.
        t.on_appended_gated(&csid, &path, 0, l1, false, true);

        assert_eq!(
            delivered(&outbox, o.coord_session_id),
            std::fs::read_to_string(&path).unwrap()
        );
    }

    /// The same property under a REAL race: a writer thread appends lines and
    /// plays the watcher (cursor-based batches) while the main thread binds
    /// mid-stream. Whatever the interleaving, the delivered transcript is the
    /// file, once, in order — which holds only because the replay and the
    /// live path share the per-session lock and the mark.
    #[test]
    fn concurrent_bind_and_tail_never_duplicate_or_reorder() {
        for round in 0..25 {
            let dir = tempdir().unwrap();
            let (t, _registrar, outbox) = tailer(dir.path());
            let csid = Uuid::new_v4().to_string();
            let path = jsonl(dir.path(), &csid, "");

            let writer = {
                let (t, csid, path) = (t.clone(), csid.clone(), path.clone());
                std::thread::spawn(move || {
                    let mut cursor = 0u64;
                    for i in 0..200 {
                        let line = format!("{{\"round\":{round},\"i\":{i}}}\n");
                        append(&path, &line);
                        t.on_appended_gated(&csid, &path, cursor, &line, false, true);
                        cursor += line.len() as u64;
                    }
                })
            };
            std::thread::sleep(Duration::from_micros(200 * (round % 5)));
            let o = bind(&t, &csid, &path);
            writer.join().unwrap();

            assert_eq!(
                delivered(&outbox, o.coord_session_id),
                std::fs::read_to_string(&path).unwrap(),
                "round {round}"
            );
        }
    }

    /// Re-bind replays nothing: in the same process (already bound) AND after
    /// a restart (fresh registrar, same durable marks).
    #[test]
    fn rebind_replays_nothing() {
        let dir = tempdir().unwrap();
        let csid = Uuid::new_v4().to_string();
        let path = jsonl(dir.path(), &csid, "{\"n\":1}\n{\"n\":2}\n");
        {
            let (t, _registrar, outbox) = tailer(dir.path());
            let first = bind(&t, &csid, &path);
            assert!(first.replayed_bytes > 0);
            let rows = transcript_offsets(&outbox).len();

            let again = bind(&t, &csid, &path);
            assert!(again.already_bound);
            assert_eq!(again.coord_session_id, first.coord_session_id);
            assert_eq!((again.replayed_bytes, again.replayed_chunks), (0, 0));
            assert_eq!(transcript_offsets(&outbox).len(), rows);
        }
        // Restart: the index is empty again, the marks are not.
        let (t2, _registrar2, outbox2) = tailer(dir.path());
        let rows = transcript_offsets(&outbox2).len();
        let after_restart = bind(&t2, &csid, &path);
        assert!(!after_restart.already_bound, "a fresh process binds anew");
        assert_eq!(
            after_restart.replayed_bytes, 0,
            "but replays nothing already sent"
        );
        assert_eq!(transcript_offsets(&outbox2).len(), rows);

        // Bytes appended while no process carried them ARE replayed.
        append(&path, "{\"n\":3}\n");
        let (t3, _r3, _o3) = tailer(dir.path());
        assert_eq!(bind(&t3, &csid, &path).replayed_bytes, 8);
    }

    /// Gate 1 off: the bind refuses before anything — no binding, no
    /// `Started` row, no chunk.
    #[test]
    fn disabled_toggle_writes_nothing() {
        let dir = tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();
        let path = jsonl(dir.path(), &csid, "{\"n\":1}\n");

        assert_eq!(
            t.bind_and_replay(&csid, &path, BindRequest::default(), false),
            Err(BindRefusal::SyncDisabled)
        );
        assert!(
            registrar.session_id_for(&csid).is_none(),
            "no binding written"
        );
        assert!(
            outbox.pending().unwrap().is_empty(),
            "no outbox row of any kind"
        );
    }

    /// A supplied coord session id is ADOPTED: the index points at it and no
    /// `Started` row is minted beside the row that already exists.
    #[test]
    fn supplied_coord_session_id_is_adopted_without_a_started_row() {
        let dir = tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();
        let existing = Uuid::new_v4();
        let path = jsonl(dir.path(), &csid, "{\"n\":1}\n");

        let o = (0..200)
            .find_map(|_| {
                match t.bind_and_replay(
                    &csid,
                    &path,
                    BindRequest {
                        adopt: Some(existing),
                        tenant: None,
                    },
                    true,
                ) {
                    Ok(o) => Some(o),
                    Err(BindRefusal::RegistrationDisabled) => {
                        std::thread::sleep(Duration::from_millis(10));
                        None
                    }
                    Err(e) => panic!("{e:?}"),
                }
            })
            .expect("bind");
        assert_eq!(o.coord_session_id, existing);
        assert!(o.adopted);
        assert_eq!(registrar.session_id_for(&csid), Some(existing));
        let started = outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Started.as_str())
            .count();
        assert_eq!(started, 0);
        assert_eq!(delivered(&outbox, existing), "{\"n\":1}\n");
    }

    /// Adopting a coord session another key already holds is refused, and
    /// the holder's mapping is untouched.
    #[test]
    fn adopting_an_already_mapped_coord_session_is_refused() {
        let dir = tempdir().unwrap();
        let (t, registrar, _outbox) = tailer(dir.path());
        let (a, b) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
        let pa = jsonl(dir.path(), &a, "{\"n\":1}\n");
        let pb = jsonl(dir.path(), &b, "{\"n\":2}\n");
        let held = bind(&t, &a, &pa).coord_session_id;

        let refused = (0..200)
            .find_map(|_| {
                match t.bind_and_replay(
                    &b,
                    &pb,
                    BindRequest {
                        adopt: Some(held),
                        tenant: None,
                    },
                    true,
                ) {
                    Err(BindRefusal::RegistrationDisabled) => {
                        std::thread::sleep(Duration::from_millis(10));
                        None
                    }
                    other => Some(other),
                }
            })
            .expect("a verdict");
        assert_eq!(refused, Err(BindRefusal::CoordSessionInUse));
        assert_eq!(registrar.session_id_for(&a), Some(held));
        assert_eq!(registrar.session_id_for(&b), None);
    }

    /// A session bound by the SNIFFER while its file already held content (a
    /// resumed session: the watcher's tail started at EOF): the first live
    /// batch fills the prefix from the file first, so the transcript is the
    /// whole file, once, in order — and a later route bind replays nothing.
    #[test]
    fn sniffer_bound_resumed_session_gets_its_prefix_once() {
        let dir = tempdir().unwrap();
        let (t, registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();
        let (l1, l2, l3) = ("{\"n\":1}\n", "{\"n\":2}\n", "{\"n\":3}\n");
        let path = jsonl(dir.path(), &csid, &format!("{l1}{l2}"));
        let coord = sniff_register(&registrar, &csid);

        append(&path, l3);
        t.on_appended_gated(&csid, &path, (l1.len() + l2.len()) as u64, l3, false, true);
        assert_eq!(delivered(&outbox, coord), format!("{l1}{l2}{l3}"));

        let o = bind(&t, &csid, &path);
        assert!(o.already_bound);
        assert_eq!(
            o.coord_session_id, coord,
            "no second row for a mapped session"
        );
        assert_eq!((o.replayed_bytes, o.replayed_chunks), (0, 0));
        assert_eq!(delivered(&outbox, coord), format!("{l1}{l2}{l3}"));
    }

    /// A rewritten transcript (different first line) resets the mark; the
    /// replay that notices it re-sends the NEW content once, and the watcher's
    /// later `truncated` batch of the same content does not send it again.
    #[test]
    fn rewrite_is_detected_and_not_double_emitted() {
        let dir = tempdir().unwrap();
        let (t, _registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();
        let a = "{\"v\":\"a1\"}\n{\"v\":\"a2\"}\n";
        let path = jsonl(dir.path(), &csid, a);
        let coord = bind(&t, &csid, &path).coord_session_id;

        // Rewritten LONGER, so no shrink is visible — only the fingerprint
        // tells the old mark does not describe this file.
        let c = "{\"v\":\"c1-longer-line\"}\n{\"v\":\"c2\"}\n";
        std::fs::write(&path, c).unwrap();
        let again = bind(&t, &csid, &path);
        assert_eq!(again.replayed_bytes, c.len() as u64);
        // The watcher now reports the truncation it saw, with the same bytes.
        t.on_appended_gated(&csid, &path, 0, c, true, true);

        assert_eq!(delivered(&outbox, coord), format!("{a}{c}"));
    }

    /// Under contention the non-blocking path does NOTHING (so the async
    /// watcher never waits on a replay), and the blocking path then carries
    /// the batch — nothing is lost.
    #[test]
    fn contended_batch_is_deferred_not_dropped() {
        let dir = tempdir().unwrap();
        let (t, _registrar, outbox) = tailer(dir.path());
        let csid = Uuid::new_v4().to_string();
        let l1 = "{\"n\":1}\n";
        let path = jsonl(dir.path(), &csid, "");
        let coord = bind(&t, &csid, &path).coord_session_id;

        append(&path, l1);
        let lock = t.session_lock(&csid);
        {
            let _held = lock.lock().unwrap();
            assert!(!t.try_emit_batch(&csid, &path, 0, l1, false));
        }
        assert!(transcript_offsets(&outbox).is_empty());
        t.emit_batch(&csid, &path, 0, l1, false);
        assert_eq!(delivered(&outbox, coord), l1);
        // Uncontended and exactly at the mark: the fast path takes it.
        let l2 = "{\"n\":2}\n";
        append(&path, l2);
        assert!(t.try_emit_batch(&csid, &path, l1.len() as u64, l2, false));
        assert_eq!(delivered(&outbox, coord), format!("{l1}{l2}"));
    }

    /// Ownership: the session's own `cwd` decides; a workspace ROOT does not
    /// own the sessions in the worktrees under it.
    #[test]
    fn ownership_is_the_sessions_own_cwd() {
        let dir = tempdir().unwrap();
        let csid = Uuid::new_v4().to_string();
        let path = jsonl(
            dir.path(),
            &csid,
            "{\"type\":\"summary\"}\n{\"type\":\"user\",\"cwd\":\"/work/a\"}\n",
        );
        assert!(jsonl_belongs_to_workdir(&path, "/work/a"));
        assert!(jsonl_belongs_to_workdir(&path, "/work/a/"));
        assert!(!jsonl_belongs_to_workdir(&path, "/work/b"));
        assert!(!jsonl_belongs_to_workdir(&path, "/work"));
        assert!(!jsonl_belongs_to_workdir(&path, ""));
    }

    /// The locator's three refusals, and its one success.
    #[test]
    fn locate_refuses_what_the_watcher_does_not_tail() {
        let dir = tempdir().unwrap();
        let csid = Uuid::new_v4().to_string();
        assert_eq!(
            locate_session_jsonl(&[], &csid),
            Err(LocateRefusal::NoConfigDirs)
        );
        let cfg = dir.path().join("cfg");
        assert!(matches!(
            locate_session_jsonl(std::slice::from_ref(&cfg), &csid),
            Err(LocateRefusal::NotFound { .. })
        ));
        let path = jsonl(dir.path(), &csid, "{\"n\":1}\n");
        assert_eq!(
            locate_session_jsonl(std::slice::from_ref(&cfg), &csid),
            Ok(path)
        );

        let wf = Uuid::new_v4().to_string();
        jsonl(
            dir.path(),
            &wf,
            "{\"type\":\"queue-operation\",\"operation\":\"enqueue\"}\n",
        );
        assert!(matches!(
            locate_session_jsonl(std::slice::from_ref(&cfg), &wf),
            Err(LocateRefusal::WorkflowSession { .. })
        ));
    }

    /// `report_coverage_once` must not panic and must be callable with an
    /// empty ledger (the idle-fleet path).
    #[test]
    fn coverage_report_on_idle_ledger_is_quiet_and_safe() {
        let dir = tempdir().unwrap();
        let (t, _registrar, _outbox) = tailer(dir.path());
        t.report_coverage_once();
        let r = t.coverage();
        assert_eq!(r.sessions_tailed, 0);
        assert_eq!(r.sessions_unbound, 0);
        assert_eq!(r.appends_emitted, 0);
    }
}
