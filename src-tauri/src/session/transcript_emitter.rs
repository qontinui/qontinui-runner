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
//! The coord session UUID for a `task_run_id` comes from
//! [`crate::claude_session::coord_register::AiCoordRegistrar`]'s R4 index —
//! the same durable `coord.sessions.task_run_id` binding the session-
//! automation phase established. Workflow runs are registered into that
//! index by `LoopController::run` at run start (and closed at run end), so
//! the executor's emit hooks resolve. A task run with no resolvable coord
//! session (e.g. registration gated off via
//! `QONTINUI_SESSION_AUTOMATION_REGISTER`, or a run kind that never
//! registers) is skipped silently, with one debug line per run.
//!
//! ## Chunking
//!
//! Coord rejects bodies over ~1 MiB; a single agentic iteration can exceed
//! that. `emit_inner` therefore splits the redacted payload into
//! [`MAX_CHUNK_BYTES`]-sized chunks, each advancing `chunk_offset`, all
//! allocated inside one offsets-lock critical section so a concurrent emit
//! for the same session can't interleave.
//!
//! ## Failure posture
//!
//! Best-effort throughout: the outbox append is local file I/O; any failure
//! is logged and swallowed. Emission can never fail the task run.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::json;
use uuid::Uuid;

use crate::claude_session::coord_register::AiCoordRegistrar;

use super::local_store::OutboxWriter;
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

/// Emits transcript chunks into the session outbox. Managed as Tauri state
/// (`Arc<TranscriptEmitter>`); cheap to share. Holds the same
/// [`OutboxWriter`] the `CoordSync` drain loop drains.
pub struct TranscriptEmitter {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
    /// task_run_id → coord session UUID resolver (R4 index).
    registrar: Arc<AiCoordRegistrar>,
    /// Per-coord-session monotonic byte offset for the NEXT transcript
    /// chunk. In-memory by design: the offset lane lives and dies with the
    /// registrar's session binding (both reset together on restart).
    offsets: Mutex<HashMap<Uuid, i64>>,
    /// Task runs already debug-logged as "no coord session — skipping", so
    /// the skip line fires once per run, not once per chunk.
    skipped_runs: Mutex<HashSet<String>>,
}

impl TranscriptEmitter {
    /// Construct from the shared session outbox + this device's
    /// `machine_id` + the AI-session registrar. The outbox MUST be the same
    /// `Arc` the `CoordSync` drain loop reads.
    pub fn new(
        outbox: Arc<OutboxWriter>,
        machine_id: Uuid,
        registrar: Arc<AiCoordRegistrar>,
    ) -> Self {
        Self {
            outbox,
            machine_id,
            registrar,
            offsets: Mutex::new(HashMap::new()),
            skipped_runs: Mutex::new(HashSet::new()),
        }
    }

    /// Emit one transcript block for `task_run_id`. Gate 1
    /// (`Settings.cloud_sync_enabled`) is checked first — when off, this
    /// returns before any allocation or I/O and nothing leaves the machine.
    /// Never fails the caller: all errors are logged and swallowed.
    pub fn emit(&self, task_run_id: &str, text: &str) {
        if !crate::settings::get_cloud_sync_enabled() {
            return;
        }
        self.emit_inner(task_run_id, text);
    }

    /// Gate-2/3 + append path, split from [`Self::emit`] so tests can drive
    /// it without touching the machine's real `settings.json`.
    fn emit_inner(&self, task_run_id: &str, text: &str) {
        if text.is_empty() {
            return;
        }

        // Linkage: task_run_id → coord session UUID via the registrar's R4
        // index. No binding → skip silently (one debug line per run).
        let Some(session_id) = self.registrar.session_id_for(task_run_id) else {
            let mut skipped = self
                .skipped_runs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if skipped.insert(task_run_id.to_string()) {
                tracing::debug!(
                    task_run_id,
                    "transcript_emitter: no coord session for task run — skipping cloud sync"
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

        // Allocate the per-(session, transcript-stream) offsets and append.
        // Oversized blocks split into MAX_CHUNK_BYTES chunks, each
        // advancing the offset (coord rejects >1 MiB bodies with a 400 the
        // drain would ACK-drop — a permanent transcript gap). The lock
        // spans ALL outbox writes for this block so two concurrent emits
        // for the same session can't interleave offsets; the offset only
        // advances on a successful durable append so a failed write
        // re-sends the same offset next time (idempotent coord-side). On a
        // mid-block failure the remaining chunks are dropped rather than
        // written out of order.
        let mut offsets = self
            .offsets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut offset = offsets.get(&session_id).copied().unwrap_or(0);
        for chunk in bytes.chunks(MAX_CHUNK_BYTES) {
            let payload = json!({
                "stream": TRANSCRIPT_STREAM,
                "chunk_offset": offset,
                "payload_b64": base64::engine::general_purpose::STANDARD.encode(chunk),
            });
            match self.outbox.record(
                self.machine_id,
                session_id,
                SessionEventKind::OutputChunk,
                payload,
            ) {
                Ok(_) => {
                    offset += chunk.len() as i64;
                    offsets.insert(session_id, offset);
                }
                Err(e) => {
                    tracing::warn!(
                        task_run_id,
                        session = %session_id,
                        error = %e,
                        "transcript_emitter: outbox append failed (best-effort) — chunk (and any remainder of this block) dropped locally"
                    );
                    break;
                }
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
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        let machine_id = Uuid::new_v4();
        let registrar = Arc::new(AiCoordRegistrar::new(outbox.clone(), machine_id));
        let em = TranscriptEmitter::new(outbox.clone(), machine_id, registrar.clone());
        (em, registrar, outbox, dir)
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
