//! Transcript-chunk outbox emitter — durable, opt-in cloud sync of AI
//! conversation transcripts. Plan
//! `2026-07-09-runner-session-history-cloud-sync` §3.2 (Phase 2, runner side).
//!
//! ## What it does
//!
//! The live AI-output persist sites (`unified_ai_session.rs` for
//! setup/completion, the loop_controller for the agentic phase + completion
//! sweep) hand each persisted transcript block to [`TranscriptEmitter::emit`].
//! When the gates pass, the block is redacted and appended to the SAME
//! local-first JSONL outbox the session lifecycle events ride
//! ([`super::local_store::OutboxWriter`]), as a
//! [`SessionEventKind::OutputChunk`] record with payload
//! `{stream: "transcript", chunk_offset, payload_b64}`. The
//! [`super::coord_sync`] drain loop then POSTs it to coord's
//! `POST /sessions/:id/output` — replay-safe, idempotent on
//! `(session_id, stream, chunk_offset)`, offline-tolerant by construction.
//!
//! **Deliberate divergence from the shipped PTY path:** PTY chunks go via
//! [`super::output_pipe`]'s direct POST and are *dropped* on 429/5xx (lossy
//! live telemetry); transcripts are the audit artifact and get at-least-once
//! delivery via the outbox. Transcript `chunk_offset`s are allocated by this
//! module's own per-session monotonic byte counter — a separate stream lane
//! from the PTY counter, per the binding PK decision
//! (`(session_id, stream, chunk_offset)` coord-side).
//!
//! ## Gates (all default-safe)
//!
//! 1. **Runner-global** — `Settings.cloud_sync_enabled` (default false).
//!    Checked before anything is written: with the toggle off, no outbox
//!    entry is created and nothing leaves the machine.
//! 2. **Per-tenant** — enforced coord-side (`session_coordination_enabled`
//!    + warm/cold quotas); the drain simply forwards.
//! 3. **Per-session** — redaction, which ALWAYS runs. Workflow runs have no
//!    coord-native [`super::Intent`] carrying a per-session opt-out (the
//!    registrar binding is a plain id ↔ id index), so every transcript byte
//!    goes through `redact_secrets` before it is durably written.
//!
//! ## Session linkage
//!
//! The coord session UUID for a session key comes from
//! [`crate::claude_session::coord_register::AiCoordRegistrar`]'s R4 index.
//! **That index is keyed on `claude_session_id` for BOTH planes**, which is
//! why every parameter here is named `session_key`: the runner pins each
//! registrar-managed session's CLI session id to its `task_run_id`, so a
//! workflow run's `task_run_id` *is* its `claude_session_id`, and the
//! interactive plane passes the sniffed `claude_code_session_id` directly.
//! (`AiCoordRegistrar::session_id_for`'s own doc records that the old
//! `task_run_id` parameter name "described one caller, not the key".)
//!
//! Two producers feed the index. Workflow runs are registered by
//! `LoopController::run` at run start and closed at run end, so the
//! executor's emit hooks resolve. Interactive panes are registered by
//! `AiCoordRegistrar::register_sniffed_session` from
//! `terminal::claude_resume_sniff`, so
//! [`super::session_transcript_tailer`]'s emits resolve. A session with no
//! resolvable coord session (registration gated off via
//! `QONTINUI_SESSION_AUTOMATION_REGISTER`, a run kind that never registers,
//! or — for the interactive plane — a pane the sniffer never saw) is skipped
//! silently, with one debug line per session. Because that skip is quiet by
//! design, the tailer counts it: see that module's coverage reporting.
//!
//! ## Chunking
//!
//! Coord rejects bodies over ~1 MiB; a single agentic iteration can exceed
//! that. `emit_inner` therefore splits the redacted payload into
//! [`MAX_CHUNK_BYTES`]-sized chunks, each advancing `chunk_offset`, all
//! allocated inside one offsets-lock critical section so a concurrent emit
//! for the same session can't interleave.
//!
//! ## The offset lane is DURABLE, and keyed on the durable anchor
//!
//! Plan `2026-08-26-claude-code-session-repository-in-qontinui-web` Phase 2(a).
//! Until that phase the lane was a bare `Mutex<HashMap<Uuid, i64>>`
//! constructed empty and never persisted, keyed on the coord session UUID.
//! Two properties of the interactive (`terminal_claude`) plane make that
//! unsafe in a way the workflow plane never exposed:
//!
//! 1. **Interactive sessions outlive the runner process.** Workflow runs are
//!    short-lived and never span a restart, so an empty map at boot always
//!    matched an empty server-side lane. An operator's Claude Code pane is the
//!    exact inverse — surviving a rebuild is the property the repository plan
//!    exists to deliver.
//! 2. **The coord session UUID is minted fresh every process.**
//!    `AiCoordRegistrar::register_inner` mints a `uuid_v7()` per registration
//!    and its R4/R6 index is explicitly documented as in-process and never
//!    rehydrated, while coord's `create_session` dedupes `ON CONFLICT (id)`
//!    only. So one `claude_session_id` accumulates a NEW `coord.sessions` row
//!    per runner lifetime, all joined to one `coord.agent_sessions` row via
//!    `claude_code_session_id` — which is exactly what coord's own read route
//!    (`GET /sessions/:id/output`, "`:id` may be the coord session id OR the
//!    Claude Code session UUID") and the web repository are built to read
//!    back.
//!
//! Keying the lane on the coord UUID therefore restarts every lifetime at 0,
//! and the moment anything concatenates a session's chunks by its durable
//! anchor it gets N overlapping runs that all begin at offset 0 — duplicate
//! offsets carrying different bytes, resolved by coord's `ON CONFLICT DO
//! NOTHING` into a silent, unlogged transcript gap. So the lane is now keyed
//! on the **session key** (`claude_session_id` — the same key
//! `AiCoordRegistrar::session_id_for` indexes for BOTH planes) and persisted
//! in [`TranscriptOffsetLog`], an append-only JSONL sidecar beside the outbox
//! file. A restart rehydrates the lane and the next chunk continues past
//! every byte this machine has already emitted, across coord session rows.
//!
//! **Write-ahead ordering.** The reservation is fsynced to the sidecar
//! *before* the chunks are appended to the outbox. Crashing between the two
//! burns an offset range whose bytes were never written — a hole in the
//! numbering, which coord does not care about (chunks are ordered by offset,
//! never required to be contiguous). The opposite order would re-issue a
//! live offset for different bytes, which is the truncation this phase
//! exists to prevent. Deciding priority: robustness — a silent, unlogged
//! truncation of the archive is strictly worse than a gap or a duplicated
//! chunk. A *clean* outbox failure (`record_batch` is all-or-nothing and
//! fsynced, so nothing was written) durably rewinds the reservation, keeping
//! the pre-existing retry-at-the-same-offset behaviour.
//!
//! **Why the sidecar and not a read-back from coord.** The plan's Phase 2(a)
//! text proposes seeding from "the highest `chunk_offset` coord already holds
//! for `(session_id, 'transcript')`". That read is **not reachable from the
//! runner**: coord's `GET /sessions/:id/output` takes the *required*
//! `TenantId` extractor, which resolves solely from an `OperatorContext`
//! stashed by `resolve_operator_optional` — and that middleware names a
//! device JWT as a bearer it deliberately passes through without producing
//! one, with the `X-Qontinui-Tenant-Id` fallback removed. Every runner call
//! there is a 403 `tenant_not_resolved`. The sidecar is also *stronger* than
//! that read would have been: it counts bytes already durable in the outbox
//! but not yet drained, which coord by definition cannot see, so seeding from
//! coord would rewind BELOW the outbox's own queued chunks and manufacture
//! the collision. The runner is the sole allocator of this lane, so its own
//! durable record is the authority.
//!
//! ## Failure posture
//!
//! Best-effort throughout: the outbox append is local file I/O; any failure
//! is logged and swallowed. Emission can never fail the task run.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use serde_json::json;
use uuid::Uuid;

use crate::claude_session::coord_register::AiCoordRegistrar;

use super::local_store::{OutboxEvent, OutboxWriter};
use super::redact::redact_secrets;
use super::SessionEventKind;

/// Wire value of the transcript stream discriminator. Matches coord's
/// `coord.session_output.stream` column (Phase 2 coord side; default "pty").
pub const TRANSCRIPT_STREAM: &str = "transcript";

/// Max payload bytes per outbox transcript chunk. Coord's ingest rejects
/// bodies over ~1 MiB (a 400 the drain would ACK-drop, leaving a permanent
/// transcript gap); 64 KiB keeps each POST comfortably under that with
/// base64 + JSON overhead.
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;

/// Rewrite the offset sidecar once it holds this many lines AND at least
/// half of them are superseded. Sized so a busy pane (one append per wake,
/// so at most ~1/s per session) rewrites at most a few times an hour, while
/// the file never becomes expensive to replay at boot.
const OFFSET_LOG_COMPACT_LINES: usize = 4096;

/// Derive the transcript offset sidecar path from the outbox path. Kept
/// beside the outbox rather than at a path of its own so the sidecar
/// inherits the outbox's instance scoping AND its unwritable-home tempdir
/// fallback (`main.rs` resolves both); a lane recorded against a different
/// outbox file would describe chunks this writer never queued.
fn offset_log_path_for(outbox_path: &Path) -> PathBuf {
    let stem = outbox_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session-outbox".to_string());
    outbox_path.with_file_name(format!("{stem}-transcript-offsets.jsonl"))
}

/// Durable per-session-key transcript offset lane — the *next* `chunk_offset`
/// this machine will allocate for each session. See the module header's
/// "The offset lane is DURABLE" section for why it is persisted and why it is
/// keyed on `claude_session_id` rather than the coord session UUID.
///
/// Append-only JSON lines, last-write-wins on replay, compacted in place —
/// the same shape (and for the same reasons) as the outbox it sits beside.
/// Never fails a caller: an unwritable sidecar degrades to the pre-Phase-2
/// in-memory-only behaviour with one warning, because losing cloud sync is
/// worse than losing durability of an offset that only matters across a
/// restart.
pub struct TranscriptOffsetLog {
    path: PathBuf,
    state: Mutex<OffsetLogState>,
}

struct OffsetLogState {
    /// session key (`claude_session_id`) → next `chunk_offset`.
    lanes: HashMap<String, i64>,
    /// Lines currently in the file, so compaction can fire on real growth
    /// rather than on a timer.
    lines: usize,
    /// Set once a write has failed. Latched so a full disk produces one
    /// warning rather than one per chunk; the in-memory lane keeps working.
    degraded: bool,
}

impl TranscriptOffsetLog {
    /// Replay the sidecar into memory. A missing file is a fresh machine, not
    /// an error. A malformed trailing line is a torn append from a crash:
    /// because the reservation is fsynced BEFORE the outbox write, the record
    /// it would have carried describes bytes that were never queued, so
    /// dropping it and keeping the previous value is the correct — and safe —
    /// recovery.
    pub fn open(path: PathBuf) -> Self {
        let mut lanes: HashMap<String, i64> = HashMap::new();
        let mut lines = 0usize;
        let mut degraded = false;
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                for line in body.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    lines += 1;
                    match serde_json::from_str::<serde_json::Value>(line) {
                        Ok(v) => {
                            match (
                                v.get("session_key").and_then(|k| k.as_str()),
                                v.get("next_offset").and_then(|n| n.as_i64()),
                            ) {
                                (Some(k), Some(n)) => {
                                    lanes.insert(k.to_string(), n);
                                }
                                _ => tracing::warn!(
                                    path = %path.display(),
                                    "transcript_emitter: offset log line missing session_key/next_offset — ignored"
                                ),
                            }
                        }
                        Err(e) => tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "transcript_emitter: torn offset log line — ignored (its bytes were never queued)"
                        ),
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                degraded = true;
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "transcript_emitter: offset log unreadable — transcript offsets are \
                     in-memory only this run, so a session that spans this restart may \
                     re-use offsets"
                );
            }
        }
        if !lanes.is_empty() {
            tracing::info!(
                path = %path.display(),
                lanes = lanes.len(),
                lines,
                "transcript_emitter: rehydrated durable transcript offset lanes"
            );
        }
        Self {
            path,
            state: Mutex::new(OffsetLogState {
                lanes,
                lines,
                degraded,
            }),
        }
    }

    /// Take the lane lock for one emit block. Held across the reservation AND
    /// the outbox append so two concurrent emits for the same session cannot
    /// interleave offsets.
    fn lock(&self) -> OffsetLane<'_> {
        OffsetLane {
            path: &self.path,
            state: self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }

    /// Next `chunk_offset` for `session_key`; `0` for a lane never written.
    pub fn next_offset(&self, session_key: &str) -> i64 {
        self.lock().next_offset(session_key)
    }
}

/// Locked view of the lane map. Every mutation is durable before it returns
/// (or the log has latched `degraded`).
struct OffsetLane<'a> {
    path: &'a Path,
    state: MutexGuard<'a, OffsetLogState>,
}

impl OffsetLane<'_> {
    fn next_offset(&self, session_key: &str) -> i64 {
        self.state.lanes.get(session_key).copied().unwrap_or(0)
    }

    /// Record `next_offset` for `session_key`, durably. Used both to reserve
    /// (write-ahead of the outbox append) and to rewind after a clean outbox
    /// failure.
    fn record(&mut self, session_key: &str, next_offset: i64) {
        self.state
            .lanes
            .insert(session_key.to_string(), next_offset);
        if self.state.degraded {
            return;
        }
        if let Err(e) = append_offset_line(self.path, session_key, next_offset) {
            self.state.degraded = true;
            tracing::warn!(
                path = %self.path.display(),
                session_key,
                error = %e,
                "transcript_emitter: offset log write failed — falling back to in-memory \
                 offsets for the rest of this run; a session spanning the next restart may \
                 re-use offsets"
            );
            return;
        }
        self.state.lines += 1;
        if self.state.lines >= OFFSET_LOG_COMPACT_LINES
            && self.state.lines >= self.state.lanes.len() * 2
        {
            match compact_offset_log(self.path, &self.state.lanes) {
                Ok(()) => self.state.lines = self.state.lanes.len(),
                // Non-fatal: the log stays append-only and correct, just
                // longer. Do NOT latch `degraded` — appends still work.
                Err(e) => tracing::warn!(
                    path = %self.path.display(),
                    error = %e,
                    "transcript_emitter: offset log compaction failed — continuing append-only"
                ),
            }
        }
    }
}

/// One durable append. `sync_data` is what makes the write-ahead ordering
/// mean anything: without it a crash could lose the reservation while the
/// outbox chunks it covers survived the outbox's own fsync.
fn append_offset_line(path: &Path, session_key: &str, next_offset: i64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = json!({ "session_key": session_key, "next_offset": next_offset }).to_string();
    line.push('\n');
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.sync_data()
}

/// Rewrite the log with one line per live lane, atomically (write temp →
/// fsync → rename), so a crash mid-compaction leaves the previous complete
/// log rather than a truncated one.
fn compact_offset_log(path: &Path, lanes: &HashMap<String, i64>) -> std::io::Result<()> {
    let tmp = path.with_extension("jsonl.compacting");
    let mut buf = String::new();
    for (key, offset) in lanes {
        buf.push_str(&json!({ "session_key": key, "next_offset": offset }).to_string());
        buf.push('\n');
    }
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(buf.as_bytes())?;
        f.sync_data()?;
    }
    std::fs::rename(&tmp, path)
}

/// Emits transcript chunks into the session outbox. Managed as Tauri state
/// (`Arc<TranscriptEmitter>`); cheap to share. Holds the same
/// [`OutboxWriter`] the `CoordSync` drain loop drains.
pub struct TranscriptEmitter {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
    /// session key (`claude_session_id`) → coord session UUID resolver
    /// (R4 index).
    registrar: Arc<AiCoordRegistrar>,
    /// DURABLE per-session-key monotonic byte offset for the NEXT transcript
    /// chunk, persisted beside the outbox. See the module header — this used
    /// to be an in-memory `HashMap<Uuid, i64>`, which silently truncated any
    /// session that outlived a runner restart.
    offsets: TranscriptOffsetLog,
    /// Session keys already debug-logged as "no coord session — skipping", so
    /// the skip line fires once per session, not once per chunk.
    skipped_runs: Mutex<HashSet<String>>,
}

impl TranscriptEmitter {
    /// Construct from the shared session outbox + this device's
    /// `machine_id` + the AI-session registrar. The outbox MUST be the same
    /// `Arc` the `CoordSync` drain loop reads — the durable offset lane is
    /// derived from its path and describes only the chunks queued into it.
    pub fn new(
        outbox: Arc<OutboxWriter>,
        machine_id: Uuid,
        registrar: Arc<AiCoordRegistrar>,
    ) -> Self {
        let offsets = TranscriptOffsetLog::open(offset_log_path_for(outbox.path()));
        Self {
            outbox,
            machine_id,
            registrar,
            offsets,
            skipped_runs: Mutex::new(HashSet::new()),
        }
    }

    /// The durable offset lane. Exposed for the tailer's coverage reporting
    /// and for tests that assert restart behaviour.
    pub fn offsets(&self) -> &TranscriptOffsetLog {
        &self.offsets
    }

    /// Emit one transcript block for `session_key`. Gate 1
    /// (`Settings.cloud_sync_enabled`) is checked first — when off, this
    /// returns before any allocation or I/O and nothing leaves the machine.
    /// Never fails the caller: all errors are logged and swallowed.
    ///
    /// **The parameter is the `claude_session_id`, for BOTH planes.** The
    /// pinned plane's `task_run_id` *is* its `claude_session_id` (the runner
    /// pins them), and the interactive plane passes the sniffed
    /// `claude_code_session_id` directly — see
    /// [`AiCoordRegistrar::session_id_for`], whose own doc records that the
    /// old `task_run_id` parameter name "described one caller, not the key".
    pub fn emit(&self, session_key: &str, text: &str) {
        if !crate::settings::get_cloud_sync_enabled() {
            return;
        }
        self.emit_inner(session_key, text);
    }

    /// Gate-2/3 + append path, split from [`Self::emit`] so tests can drive
    /// it without touching the machine's real `settings.json`.
    ///
    /// **Gate 1 (`Settings.cloud_sync_enabled`) is NOT checked here.** It is
    /// the caller's obligation, and skipping it means transcript bytes leave
    /// the machine without consent. `pub(crate)` for exactly one production
    /// caller — [`super::session_transcript_tailer`], which must know the
    /// gate's value anyway to report coverage and so checks it itself rather
    /// than paying the settings read twice per append. Every other caller
    /// goes through [`Self::emit`].
    pub(crate) fn emit_inner(&self, session_key: &str, text: &str) {
        if text.is_empty() {
            return;
        }

        // Linkage: claude_session_id → coord session UUID via the registrar's
        // R4 index. No binding → skip silently (one debug line per session).
        // Coverage against the live pane set is the tailer's job, not this
        // one's: `session_transcript_tailer` counts these skips so a pane the
        // resume-sniffer never bound is visible rather than merely quiet.
        let Some(session_id) = self.registrar.session_id_for(session_key) else {
            let mut skipped = self
                .skipped_runs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if skipped.insert(session_key.to_string()) {
                tracing::debug!(
                    session_key,
                    "transcript_emitter: no coord session for session key — skipping cloud sync"
                );
            }
            return;
        };

        // Gate 3 — redact unconditionally. Workflow runs carry no
        // coord-native Intent with a per-session opt-out, so no secret byte
        // ever reaches the durable outbox file.
        let bytes = redact_secrets(text.as_bytes());
        if bytes.is_empty() {
            return;
        }

        // Allocate the per-(session-key, transcript-stream) offsets and
        // append. Oversized blocks split into MAX_CHUNK_BYTES chunks, each
        // advancing the offset (coord rejects >1 MiB bodies with a 400 the
        // drain would ACK-drop — a permanent transcript gap). The lane lock
        // spans the reservation AND all outbox writes for this block, so two
        // concurrent emits for the same session can't interleave offsets.
        let mut lane = self.offsets.lock();
        let start_offset = lane.next_offset(session_key);
        let mut end_offset = start_offset;
        let mut events = Vec::new();
        for chunk in bytes.chunks(MAX_CHUNK_BYTES) {
            events.push(OutboxEvent::new(
                self.machine_id,
                session_id,
                SessionEventKind::OutputChunk,
                json!({
                    "stream": TRANSCRIPT_STREAM,
                    "chunk_offset": end_offset,
                    "payload_b64": base64::engine::general_purpose::STANDARD.encode(chunk),
                }),
            ));
            end_offset += chunk.len() as i64;
        }

        // WRITE-AHEAD the reservation: the lane must be durable BEFORE the
        // bytes it covers are queued. A crash between the two burns an offset
        // range whose bytes were never written (coord orders chunks by offset
        // and never requires contiguity, so a hole is inert); the opposite
        // order would re-issue a live offset for different bytes after the
        // restart, which coord's `ON CONFLICT DO NOTHING` turns into a silent
        // truncation. See the module header, Phase 2(a).
        lane.record(session_key, end_offset);

        // One append + one fsync for the whole block, and all-or-nothing. A
        // clean failure means NOTHING was written, so durably rewind the
        // reservation and the same chunks re-send next time (idempotent
        // coord-side) rather than leaving a permanent hole.
        match self.outbox.record_batch(events) {
            Ok(_) => {}
            Err(e) => {
                lane.record(session_key, start_offset);
                tracing::warn!(
                    session_key,
                    session = %session_id,
                    error = %e,
                    "transcript_emitter: outbox append failed (best-effort) — block dropped \
                     locally, offset lane rewound to {start_offset}"
                );
            }
        }
    }
}

impl std::fmt::Debug for TranscriptEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptEmitter")
            .field("machine_id", &self.machine_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Build an emitter over a tempdir outbox with a registrar sharing the
    /// SAME outbox (mirrors production wiring). Returns the parts a test
    /// needs to register linkages and inspect the outbox.
    fn emitter() -> (
        TranscriptEmitter,
        Arc<AiCoordRegistrar>,
        Arc<OutboxWriter>,
        tempfile::TempDir,
    ) {
        let dir = tempdir().unwrap();
        let (em, registrar, outbox) = emitter_in(dir.path());
        (em, registrar, outbox, dir)
    }

    /// [`emitter`] against an explicit directory, so a test can build a
    /// SECOND emitter over the same outbox file + offset sidecar — i.e.
    /// model a runner restart.
    fn emitter_in(dir: &Path) -> (TranscriptEmitter, Arc<AiCoordRegistrar>, Arc<OutboxWriter>) {
        let outbox = Arc::new(OutboxWriter::open(dir.join("outbox.jsonl")).unwrap());
        let machine_id = Uuid::new_v4();
        let registrar = Arc::new(AiCoordRegistrar::new(outbox.clone(), machine_id));
        let em = TranscriptEmitter::new(outbox.clone(), machine_id, registrar.clone());
        (em, registrar, outbox)
    }

    /// Register a task run with the registrar and drain the Started row so
    /// tests can assert on transcript rows alone.
    fn linked_run(registrar: &AiCoordRegistrar, outbox: &OutboxWriter) -> (String, Uuid) {
        let trid = Uuid::new_v4().to_string();
        // `register_session` is gated on the process-global
        // `QONTINUI_SESSION_AUTOMATION_REGISTER` env var, which the
        // coord_register test suite toggles under its own module-local lock.
        // Retry briefly so a concurrently-running "disabled gate" test can't
        // flake this suite.
        let sid = (0..100)
            .find_map(|_| {
                registrar
                    .register_session(&trid, "transcript emitter test", None)
                    .or_else(|| {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        None
                    })
            })
            .expect("register_session");
        let started: Vec<(Uuid, i64)> = outbox
            .pending()
            .unwrap()
            .into_iter()
            .map(|r| (r.session_id, r.seq))
            .collect();
        outbox.ack(&started).unwrap();
        (trid, sid)
    }

    fn transcript_rows(outbox: &OutboxWriter) -> Vec<crate::session::local_store::OutboxRecord> {
        outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::OutputChunk.as_str())
            .collect()
    }

    fn decode(rec: &crate::session::local_store::OutboxRecord) -> String {
        let b64 = rec.payload["payload_b64"].as_str().unwrap();
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn emits_transcript_chunk_with_stream_and_monotonic_offsets() {
        let (em, registrar, outbox, _dir) = emitter();
        let (trid, sid) = linked_run(&registrar, &outbox);

        em.emit_inner(&trid, "first block");
        em.emit_inner(&trid, "second");

        let rows = transcript_rows(&outbox);
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r.session_id, sid);
            assert_eq!(r.payload["stream"], serde_json::json!(TRANSCRIPT_STREAM));
        }
        // Byte offsets are per-stream monotonic: second chunk starts where
        // the first ended.
        assert_eq!(rows[0].payload["chunk_offset"], serde_json::json!(0));
        assert_eq!(
            rows[1].payload["chunk_offset"],
            serde_json::json!("first block".len() as i64)
        );
        assert_eq!(decode(&rows[0]), "first block");
        assert_eq!(decode(&rows[1]), "second");
    }

    #[test]
    fn unlinked_run_writes_nothing() {
        let (em, _registrar, outbox, _dir) = emitter();
        em.emit_inner("no-such-task-run", "should not be written");
        assert!(transcript_rows(&outbox).is_empty());
        // Repeat emission still writes nothing (and the skip set dedups the
        // debug line — behaviorally: still no rows).
        em.emit_inner("no-such-task-run", "still nothing");
        assert!(transcript_rows(&outbox).is_empty());
    }

    #[test]
    fn redacts_by_default_before_durable_write() {
        // Gate 3 redacts unconditionally: planted secrets must be masked
        // BEFORE the bytes hit the outbox file (i.e. in the durable
        // payload itself).
        let (em, registrar, outbox, _dir) = emitter();
        let (trid, _sid) = linked_run(&registrar, &outbox);

        em.emit_inner(
            &trid,
            "=== Agentic Phase (iteration 1) ===\nAPI_KEY=sk-oops password: hunter2\nBearer abcdef0123456789",
        );

        let rows = transcript_rows(&outbox);
        assert_eq!(rows.len(), 1);
        let text = decode(&rows[0]);
        assert!(!text.contains("sk-oops"), "got: {text}");
        assert!(!text.contains("hunter2"), "got: {text}");
        assert!(!text.contains("abcdef0123456789"), "got: {text}");
        assert!(text.contains("=== Agentic Phase (iteration 1) ==="));
        // The raw outbox line on disk must not carry the secret either
        // (the payload is base64 of the REDACTED bytes).
        let raw = std::fs::read_to_string(_dir.path().join("outbox.jsonl")).unwrap();
        assert!(!raw.contains("sk-oops"));
    }

    #[test]
    fn empty_text_is_a_noop() {
        let (em, registrar, outbox, _dir) = emitter();
        let (trid, _sid) = linked_run(&registrar, &outbox);
        em.emit_inner(&trid, "");
        assert!(transcript_rows(&outbox).is_empty());
    }

    /// F1 wiring, integration-style over the production trio
    /// (registrar → emitter → outbox): a workflow run REGISTERED with the
    /// registrar (as `LoopController::run` now does at run start) lands an
    /// output_chunk outbox row on emit; an UNREGISTERED workflow run is
    /// skipped silently.
    #[test]
    fn registered_workflow_run_lands_outbox_row_unregistered_skips() {
        let (em, registrar, outbox, _dir) = emitter();

        // Mirror LoopController::run's registration for a workflow-shaped
        // execution id.
        let trid = format!(
            "unified-workflow-{}-{}",
            Uuid::new_v4(),
            chrono::Utc::now().timestamp_millis()
        );
        let sid = (0..100)
            .find_map(|_| {
                registrar
                    .register_session(&trid, "unified workflow: Test Workflow", None)
                    .or_else(|| {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        None
                    })
            })
            .expect("register_session");
        let started: Vec<(Uuid, i64)> = outbox
            .pending()
            .unwrap()
            .into_iter()
            .map(|r| (r.session_id, r.seq))
            .collect();
        outbox.ack(&started).unwrap();

        em.emit_inner(&trid, "=== Agentic Phase (iteration 1) ===\nhello");
        let rows = transcript_rows(&outbox);
        assert_eq!(rows.len(), 1, "registered run emits an output_chunk row");
        assert_eq!(rows[0].session_id, sid);
        assert_eq!(rows[0].payload["stream"], serde_json::json!("transcript"));

        // A workflow run that never registered (e.g. registration gated
        // off) writes nothing — silent skip.
        em.emit_inner("unified-workflow-unregistered-123", "should not land");
        assert_eq!(
            transcript_rows(&outbox).len(),
            1,
            "unregistered run skipped"
        );
    }

    /// F2 — an oversized block splits into MAX_CHUNK_BYTES chunks, each
    /// advancing chunk_offset, in order, reassembling to the original.
    #[test]
    fn oversized_block_splits_into_ordered_chunks() {
        let (em, registrar, outbox, _dir) = emitter();
        let (trid, sid) = linked_run(&registrar, &outbox);

        // > 2 lanes worth so we get 3 chunks (last one partial). Plain
        // ASCII filler with no secret-shaped content.
        let total = MAX_CHUNK_BYTES * 2 + 1234;
        let text: String = std::iter::repeat("qontinui transcript block ")
            .flat_map(str::chars)
            .take(total)
            .collect();
        em.emit_inner(&trid, &text);

        let rows = transcript_rows(&outbox);
        assert_eq!(rows.len(), 3, "3 ordered chunks for 2×64KiB + remainder");
        let mut expected_offset = 0i64;
        let mut reassembled = String::new();
        for row in &rows {
            assert_eq!(row.session_id, sid);
            assert_eq!(
                row.payload["chunk_offset"],
                serde_json::json!(expected_offset),
                "each chunk advances the offset by the previous chunk's length"
            );
            let piece = decode(row);
            assert!(piece.len() <= MAX_CHUNK_BYTES, "chunk within the cap");
            expected_offset += piece.len() as i64;
            reassembled.push_str(&piece);
        }
        assert_eq!(reassembled, text, "chunks reassemble to the redacted block");
        assert_eq!(expected_offset, total as i64);

        // The next emit continues from the accumulated offset.
        em.emit_inner(&trid, "tail");
        let rows = transcript_rows(&outbox);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[3].payload["chunk_offset"],
            serde_json::json!(total as i64)
        );
    }

    /// Phase 2(a) — the offset lane is DURABLE. A second emitter over the
    /// same outbox directory (a runner restart) must resume the lane rather
    /// than restart it at 0, and must do so even though the restarted
    /// registrar mints a DIFFERENT coord session id for the same session key.
    #[test]
    fn offset_lane_survives_a_restart_mid_session() {
        let dir = tempdir().unwrap();

        let (trid, sid_before) = {
            let (em, registrar, outbox) = emitter_in(dir.path());
            let (trid, sid) = linked_run(&registrar, &outbox);
            em.emit_inner(&trid, "0123456789");
            assert_eq!(em.offsets().next_offset(&trid), 10);
            (trid, sid)
        };

        // Restart: everything in memory is new, only the files persist.
        let (em2, registrar2, outbox2) = emitter_in(dir.path());
        assert_eq!(
            em2.offsets().next_offset(&trid),
            10,
            "the lane is rehydrated from the sidecar before the first emit"
        );

        // Re-register the SAME session key. The registrar's R4 index is
        // in-process only, so this mints a fresh coord session id — which is
        // exactly why the lane may not be keyed on it.
        let sid_after = (0..100)
            .find_map(|_| {
                registrar2
                    .register_session(&trid, "transcript emitter test", None)
                    .or_else(|| {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        None
                    })
            })
            .expect("re-register");
        assert_ne!(sid_before, sid_after);
        let started: Vec<(Uuid, i64)> = outbox2
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Started.as_str())
            .map(|r| (r.session_id, r.seq))
            .collect();
        outbox2.ack(&started).unwrap();

        em2.emit_inner(&trid, "abcde");

        let rows = transcript_rows(&outbox2);
        let offsets: Vec<i64> = rows
            .iter()
            .map(|r| r.payload["chunk_offset"].as_i64().unwrap())
            .collect();
        assert_eq!(
            offsets,
            vec![0, 10],
            "post-restart chunk continues the lane instead of colliding at 0"
        );
        assert_eq!(rows[1].session_id, sid_after);
        assert_eq!(em2.offsets().next_offset(&trid), 15);
    }

    /// The sidecar is a sibling of the outbox file, so it inherits the
    /// outbox's instance scoping and its unwritable-home fallback.
    #[test]
    fn offset_sidecar_lands_beside_the_outbox() {
        let dir = tempdir().unwrap();
        let (em, registrar, outbox) = emitter_in(dir.path());
        let (trid, _sid) = linked_run(&registrar, &outbox);
        em.emit_inner(&trid, "hello");

        let sidecar = dir.path().join("outbox-transcript-offsets.jsonl");
        let body = std::fs::read_to_string(&sidecar).expect("sidecar written");
        assert!(body.contains(&trid), "got: {body}");
        assert!(body.contains("\"next_offset\":5"), "got: {body}");
    }

    /// Compaction collapses a superseded log to one line per lane and the
    /// replay of the compacted file yields the same offsets.
    #[test]
    fn offset_log_compacts_to_one_line_per_lane() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("offsets.jsonl");
        for n in [1i64, 2, 3, 4] {
            append_offset_line(&path, "session-a", n).unwrap();
        }
        append_offset_line(&path, "session-b", 99).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 5);

        let mut lanes = HashMap::new();
        lanes.insert("session-a".to_string(), 4i64);
        lanes.insert("session-b".to_string(), 99i64);
        compact_offset_log(&path, &lanes).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);

        let log = TranscriptOffsetLog::open(path);
        assert_eq!(log.next_offset("session-a"), 4);
        assert_eq!(log.next_offset("session-b"), 99);
        assert_eq!(log.next_offset("session-c"), 0);
    }

    /// A torn trailing line (a crash mid-append) is dropped, not fatal, and
    /// the previous durable value survives — which is safe precisely because
    /// the reservation is written BEFORE the bytes it covers.
    #[test]
    fn torn_offset_log_line_is_dropped_and_the_lane_survives() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("offsets.jsonl");
        append_offset_line(&path, "session-a", 7).unwrap();
        {
            use std::io::Write as _;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{\"session_key\":\"session-a\",\"next_off")
                .unwrap();
        }
        let log = TranscriptOffsetLog::open(path);
        assert_eq!(log.next_offset("session-a"), 7);
    }

    #[test]
    fn offsets_are_per_session_lanes() {
        let (em, registrar, outbox, _dir) = emitter();
        let (trid_a, sid_a) = linked_run(&registrar, &outbox);
        let (trid_b, sid_b) = linked_run(&registrar, &outbox);

        em.emit_inner(&trid_a, "aaaa");
        em.emit_inner(&trid_b, "bb");
        em.emit_inner(&trid_a, "cc");

        let rows = transcript_rows(&outbox);
        let offsets_a: Vec<i64> = rows
            .iter()
            .filter(|r| r.session_id == sid_a)
            .map(|r| r.payload["chunk_offset"].as_i64().unwrap())
            .collect();
        let offsets_b: Vec<i64> = rows
            .iter()
            .filter(|r| r.session_id == sid_b)
            .map(|r| r.payload["chunk_offset"].as_i64().unwrap())
            .collect();
        assert_eq!(offsets_a, vec![0, 4]);
        assert_eq!(offsets_b, vec![0]);
    }
}
