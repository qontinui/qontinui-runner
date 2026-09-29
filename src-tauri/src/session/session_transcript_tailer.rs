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
//! order, so every emit goes through [`SessionTranscriptTailer::emit_file_range`]
//! under a PER-SESSION lock, and advances a durable FILE MARK: the byte offset
//! in the JSONL through which the file has been handed to the emitter. A
//! watcher batch that overlaps the mark (a replay read the same lines first)
//! is trimmed to its unseen suffix; one wholly below it is dropped. The marks
//! persist in a sidecar beside the outbox (the emitter's own
//! [`TranscriptOffsetLog`] type, reused as a durable key → i64 map), so a
//! re-bind after a runner restart replays only the bytes the previous process
//! never emitted rather than the whole file again.
//!
//! The mark is an at-least-once record: it advances AFTER the emitter reports
//! its chunks durably queued, so a crash between the two re-sends a batch
//! rather than skipping one.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use uuid::Uuid;

use crate::claude_session::coord_register::AiCoordRegistrar;

use super::transcript_emitter::{TranscriptEmitter, TranscriptOffsetLog};

/// Upper bound on one replay emit. Each emit is one outbox append + fsync and
/// one offset reservation, so a multi-megabyte prefix is replayed as a handful
/// of batches rather than one allocation of the whole file. Batches end on a
/// line boundary; a single line longer than this is emitted alone.
const REPLAY_BATCH_BYTES: usize = 1024 * 1024;

/// How often the coverage summary is logged. Long enough that an idle fleet
/// costs one line a minute, short enough that a rebuild's recovery window is
/// covered by several samples.
const COVERAGE_REPORT_INTERVAL: Duration = Duration::from_secs(60);

/// Cap on unbound session ids named in one summary line. A coverage hole is
/// actionable from a handful of ids; the count carries the magnitude.
const MAX_REPORTED_UNBOUND: usize = 16;

/// Tails watched Claude Code transcripts into the coord transcript stream.
/// Managed as Tauri state (`Arc<SessionTranscriptTailer>`) and handed to the
/// transcript watcher, which calls [`Self::on_appended`] from its per-session
/// tail loop.
pub struct SessionTranscriptTailer {
    emitter: Arc<TranscriptEmitter>,
    registrar: Arc<AiCoordRegistrar>,
    coverage: Mutex<Coverage>,
    /// Per-session emit locks. Every emit — a watcher batch or a replay batch
    /// — holds its session's lock across the mark read, the emit and the mark
    /// write, which is what keeps the offset lane in FILE order when a replay
    /// and a live append race. Grows by one entry per distinct session key.
    session_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Durable file marks — see the module header's "File marks".
    marks: TranscriptOffsetLog,
}

/// What a successful [`SessionTranscriptTailer::bind_and_replay`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindOutcome {
    /// The coord session the key is bound to (the existing one on a re-bind).
    pub coord_session_id: Uuid,
    /// The key was already bound before this call (by the sniffer, a workflow
    /// registration, or an earlier bind).
    pub already_bound: bool,
    /// File bytes handed to the emitter by this call's replay.
    pub replayed_bytes: u64,
    /// Outbox chunks those bytes became.
    pub replayed_chunks: u64,
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
    /// The JSONL could not be read for the replay. The binding HAS been
    /// written; later appends are tailed.
    Unreadable(String),
}

/// Why [`locate_session_jsonl`] found no file to bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateRefusal {
    /// No Claude config dir was discovered, so the watcher watches nothing.
    NoConfigDirs,
    /// No `<config_dir>/projects/*/<id>.jsonl` exists under any of them.
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
            Self::NoConfigDirs => "no Claude config dir was discovered on this machine, so the \
                 transcript watcher watches no JSONL at all"
                .to_string(),
            Self::NotFound { searched } => format!(
                "no <config_dir>/projects/*/<claude_code_session_id>.jsonl exists under any \
                 discovered config dir ({}); a transcript outside them is not watched",
                searched.join(", ")
            ),
            Self::WorkflowSession { path } => format!(
                "{path} is a runner WORKFLOW session: the watcher does not tail it (the \
                 executor produces its transcript), so binding it here would be a second producer"
            ),
        }
    }
}

/// Find the Claude Code JSONL for `claude_code_session_id` under the
/// discovered config dirs — the same `<config_dir>/projects/<project>/` tree
/// the transcript watcher watches recursively, so a file found here is one the
/// watcher tails once bound. Newest by mtime when several projects hold the
/// id. The id must already be validated as a UUID by the caller: it becomes a
/// file name.
pub fn locate_session_jsonl(
    config_dirs: &[PathBuf],
    claude_code_session_id: &str,
) -> Result<PathBuf, LocateRefusal> {
    if config_dirs.is_empty() {
        return Err(LocateRefusal::NoConfigDirs);
    }
    let file_name = format!("{claude_code_session_id}.jsonl");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for dir in config_dirs {
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
            searched: config_dirs
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
    /// (the JSONL stem — the pane's `claude_code_session_id`).
    ///
    /// Called from the watcher's tail loop once per wake with every
    /// fully-terminated line it just consumed, NOT once per line: one call is
    /// one outbox batch and one offset reservation, so batching here is what
    /// keeps the fsync rate at the wake rate rather than the line rate.
    ///
    /// Never fails and never blocks the watcher — the emitter swallows its own
    /// I/O errors by contract.
    ///
    /// `file_start` is the JSONL byte offset `appended` begins at, and
    /// `truncated` says the watcher saw the file shrink and restarted its
    /// cursor at 0 before this read — together they place the batch against
    /// the session's file mark (module header, "File marks").
    pub fn on_appended(&self, session_key: &str, file_start: u64, appended: &str, truncated: bool) {
        // Gate 1 (`Settings.cloud_sync_enabled`) is resolved here rather than
        // inside the emitter because the coverage summary needs its value —
        // "off" and "on but reaching nobody" are different diagnoses. The
        // gated body then calls `emit_inner`, which by contract does NOT
        // re-check it.
        self.on_appended_gated(
            session_key,
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
        file_start: u64,
        appended: &str,
        truncated: bool,
        cloud_sync_enabled: bool,
    ) {
        if appended.is_empty() {
            return;
        }

        if truncated && self.file_mark(session_key).is_some() {
            // The file was rewritten under the same name: its bytes restart at
            // 0, so a mark describing the OLD content would swallow the new.
            // Reset whether or not the session is bound or synced right now —
            // the mark must describe this file when it next matters.
            let lock = self.session_lock(session_key);
            let _held = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.marks.set(session_key, file_start as i64);
        }

        if !cloud_sync_enabled {
            let mut cov = self.lock_coverage();
            cov.cloud_sync_enabled = Some(false);
            cov.appends_skipped_gate_off += 1;
            return;
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
            cov.bytes_emitted += appended.len() as u64;
        } else {
            cov.unbound.insert(session_key.to_string());
            cov.appends_skipped_unbound += 1;
        }
        drop(cov);

        if !bound {
            // Emitting would be a no-op with a silent skip; returning here
            // keeps the redaction pass off the hot path for a pane that has
            // nowhere to send bytes.
            return;
        }

        // Gate 1 is already satisfied above — see `emit_inner`'s contract.
        let lock = self.session_lock(session_key);
        let _held = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.emit_file_range(session_key, file_start, appended);
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

    /// The session's file mark, `None` before anything was emitted for it.
    fn file_mark(&self, session_key: &str) -> Option<u64> {
        self.marks
            .get(session_key)
            .map(|m| u64::try_from(m).unwrap_or(0))
    }

    /// Hand the file range `[file_start, file_start + text.len())` to the
    /// emitter, minus whatever the file mark says is already emitted, and
    /// advance the mark. THE one emit path for both live appends and replay.
    ///
    /// **The caller holds `session_key`'s lock** ([`Self::session_lock`]).
    /// Returns `(file bytes emitted, outbox chunks queued)`.
    ///
    /// A range starting ABOVE the mark (a gap: appends dropped while unbound
    /// in an earlier process, or before an upgrade that introduced marks) is
    /// emitted as it stands and the gap is left — filling it here would put
    /// its bytes AFTER the ones just emitted, out of file order.
    fn emit_file_range(&self, session_key: &str, file_start: u64, text: &str) -> (u64, u64) {
        let file_end = file_start + text.len() as u64;
        let from = match self.file_mark(session_key) {
            Some(mark) if mark > file_start => mark,
            _ => file_start,
        };
        if from >= file_end {
            return (0, 0); // wholly emitted already (a replay got there first)
        }
        let skip = usize::try_from(from - file_start).unwrap_or(usize::MAX);
        let Some(unseen) = text.get(skip..) else {
            // Marks sit on the line boundaries the watcher and the replay both
            // cut at, so this is a mark from DIFFERENT content. Emitting the
            // whole text would duplicate, trimming mid-character is impossible;
            // drop the batch loudly rather than guess.
            tracing::warn!(
                session_key,
                file_start,
                mark = from,
                "session_transcript_tailer: file mark is not on a character boundary of \
                 this batch — batch skipped; a re-bind replays from the mark"
            );
            return (0, 0);
        };
        let chunks = self.emitter.emit_inner(session_key, unseen);
        if chunks == 0 {
            // Nothing queued (no binding, or a clean outbox failure): the mark
            // stays, so a later replay can still carry these bytes.
            return (0, 0);
        }
        self.marks.set(session_key, file_end as i64);
        (file_end - from, chunks as u64)
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
    /// Synchronous file I/O; call it from a blocking context.
    pub fn bind_and_replay(
        &self,
        session_key: &str,
        path: &Path,
        coord_session_id: Option<Uuid>,
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
        let coord_session_id = match existing {
            Some(id) => id,
            None => self
                .registrar
                .bind_transcript_session(session_key, coord_session_id)
                .ok_or(BindRefusal::RegistrationDisabled)?,
        };
        {
            let mut cov = self.lock_coverage();
            cov.unbound.remove(session_key);
            cov.tailed.insert(session_key.to_string());
        }

        let (replayed_bytes, replayed_chunks) = self.replay_locked(session_key, path)?;
        tracing::info!(
            session_key,
            coord_session = %coord_session_id,
            already_bound,
            replayed_bytes,
            replayed_chunks,
            path = %path.display(),
            "session_transcript_tailer: transcript bound on request"
        );
        Ok(BindOutcome {
            coord_session_id,
            already_bound,
            replayed_bytes,
            replayed_chunks,
        })
    }

    /// Read `path` from the file mark to its last complete line and feed it
    /// through [`Self::emit_file_range`] in bounded batches. Caller holds the
    /// session lock.
    fn replay_locked(&self, session_key: &str, path: &Path) -> Result<(u64, u64), BindRefusal> {
        let unreadable =
            |e: std::io::Error| BindRefusal::Unreadable(format!("{}: {e}", path.display()));
        let mut file = std::fs::File::open(path).map_err(unreadable)?;
        let len = file.metadata().map_err(unreadable)?.len();
        let mut pos = match self.file_mark(session_key) {
            // A mark past the end: the file was rewritten since. Its content is
            // new, so it is replayed from the start (the truncation rule the
            // watcher applies).
            Some(mark) if mark > len => {
                self.marks.set(session_key, 0);
                0
            }
            Some(mark) => mark,
            None => 0,
        };
        file.seek(SeekFrom::Start(pos)).map_err(unreadable)?;
        let mut reader = BufReader::new(file.take(len - pos));

        let (mut bytes, mut chunks) = (0u64, 0u64);
        let mut batch = String::new();
        let mut batch_start = pos;
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line).map_err(unreadable)?;
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
            }
            let stop = text.is_none();
            if !batch.is_empty() && (stop || batch.len() >= REPLAY_BATCH_BYTES) {
                let (b, c) = self.emit_file_range(session_key, batch_start, &batch);
                if c == 0 && self.file_mark(session_key).unwrap_or(0) < pos {
                    // Nothing queued and the mark did not cover it: the outbox
                    // refused. Stop rather than skip ahead of a hole.
                    break;
                }
                bytes += b;
                chunks += c;
                batch.clear();
                batch_start = pos;
            }
            if stop {
                break;
            }
        }
        Ok((bytes, chunks))
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

        t.on_appended_gated(&csid, 0, "{\"type\":\"user\"}\n", false, true);
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
        t.on_appended_gated(&csid, 16, "{\"type\":\"assistant\"}\n", false, true);
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

        t.on_appended_gated(&csid, 0, "should not leave the machine", false, false);

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

        t.on_appended_gated(&csid, 0, "aaaa", false, true);
        t.on_appended_gated(&csid, 4, "bb", false, true);

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
            t.on_appended_gated(&csid, 0, "0123456789", false, true);
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
        t2.on_appended_gated(&csid, 10, "abcde", false, true);

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
            match t.bind_and_replay(csid, path, None, true) {
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
        t.on_appended_gated(&csid, 0, l1, false, true);
        assert!(transcript_offsets(&outbox).is_empty());

        let o = bind(&t, &csid, &path);
        assert_eq!(o.replayed_bytes, (l1.len() + l2.len()) as u64);

        // The writer appends l3; the watcher's cursor was after l1, so its
        // batch is l2 + l3 starting at l1.len().
        append(&path, l3);
        t.on_appended_gated(&csid, l1.len() as u64, &format!("{l2}{l3}"), false, true);
        // A re-delivery of an old batch is inert.
        t.on_appended_gated(&csid, 0, l1, false, true);

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
                        t.on_appended_gated(&csid, cursor, &line, false, true);
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
            t.bind_and_replay(&csid, &path, None, false),
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
            .find_map(
                |_| match t.bind_and_replay(&csid, &path, Some(existing), true) {
                    Ok(o) => Some(o),
                    Err(BindRefusal::RegistrationDisabled) => {
                        std::thread::sleep(Duration::from_millis(10));
                        None
                    }
                    Err(e) => panic!("{e:?}"),
                },
            )
            .expect("bind");
        assert_eq!(o.coord_session_id, existing);
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
