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

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::claude_session::coord_register::AiCoordRegistrar;

use super::transcript_emitter::TranscriptEmitter;

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
        Self {
            emitter,
            registrar,
            coverage: Mutex::new(Coverage::default()),
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
    pub fn on_appended(&self, session_key: &str, appended: &str) {
        // Gate 1 (`Settings.cloud_sync_enabled`) is resolved here rather than
        // inside the emitter because the coverage summary needs its value —
        // "off" and "on but reaching nobody" are different diagnoses. The
        // gated body then calls `emit_inner`, which by contract does NOT
        // re-check it.
        self.on_appended_gated(
            session_key,
            appended,
            crate::settings::get_cloud_sync_enabled(),
        );
    }

    /// [`Self::on_appended`] with Gate 1 supplied, so tests can drive both
    /// arms without touching the machine's real `settings.json` (the same
    /// split, for the same reason, as `TranscriptEmitter::emit_inner`).
    pub(crate) fn on_appended_gated(
        &self,
        session_key: &str,
        appended: &str,
        cloud_sync_enabled: bool,
    ) {
        if appended.is_empty() {
            return;
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
        self.emitter.emit_inner(session_key, appended);
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
        let registrar = Arc::new(AiCoordRegistrar::new(outbox.clone(), machine_id));
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

        t.on_appended_gated(&csid, "{\"type\":\"user\"}\n", true);
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
        t.on_appended_gated(&csid, "{\"type\":\"assistant\"}\n", true);
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

        t.on_appended_gated(&csid, "should not leave the machine", false);

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

        t.on_appended_gated(&csid, "aaaa", true);
        t.on_appended_gated(&csid, "bb", true);

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
            t.on_appended_gated(&csid, "0123456789", true);
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
        t2.on_appended_gated(&csid, "abcde", true);

        // The post-restart chunk continues the lane at 10, NOT at 0. At 0 it
        // would collide with the pre-restart chunk under any read that joins
        // the two coord sessions by claude_code_session_id, and coord's
        // ON CONFLICT DO NOTHING would silently drop it.
        assert_eq!(
            transcript_offsets(&outbox2),
            vec![0, 10],
            "offset lane survives the restart"
        );
        assert_eq!(t2.coverage().appends_emitted, 1);
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
