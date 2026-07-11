//! Restore-registry → coord mirror emitter. Plan
//! `2026-07-09-runner-session-history-cloud-sync` §3.4 (Phase 4, runner side).
//!
//! ## What it does
//!
//! Every time the durable restore registry
//! ([`super::session_lifecycle_store::SessionLifecycleStore`]) writes or
//! refreshes an open record ([`record_open`] / [`confirm_session`]), the
//! store hands the merged record to [`RestoreRecordEmitter::emit`]. When the
//! gates pass, one `restore-record` session event is appended to the SAME
//! local-first JSONL outbox the session lifecycle events ride
//! ([`super::local_store::OutboxWriter`]), with the binding wire payload
//!
//! ```json
//! {
//!   "provider": "<provider string from the registry record>",
//!   "authoritative_session_id": "<provider resume UUID, or null>",
//!   "cwd": "<working directory>",
//!   "launch_command": "<command>",
//!   "restore_tier": "full" | "terminal_only",
//!   "machine_id": "<this device uuid>"
//! }
//! ```
//!
//! The [`super::coord_sync`] drain then POSTs it to coord's session-events
//! ingest (`POST /sessions/:id/events`) — at-least-once, idempotent on
//! `(session_id, seq)`, offline-tolerant by construction. A remote runner's
//! "Resume here" (Phase-7 handoff) materializes a local registry record from
//! the newest such event — see [`super::handoff`].
//!
//! NO free-text session content and NO env vars / tokens ride this event:
//! the payload is exactly the six fields above (in particular the record's
//! `config_dir` — an account-selection path — is deliberately excluded).
//!
//! ## Gates
//!
//! 1. **Runner-global** — `Settings.session_metadata_sync_enabled` (default
//!    true — this event carries no conversation content). Checked before
//!    anything else: with the toggle off, no outbox entry is created and
//!    nothing leaves the machine.
//! 2. **Linkage** — the record's hosting terminal must have a live coord
//!    session mirror (`terminal_create` stores the coord session id on the
//!    `TerminalSession`; the injected resolver reads it back). No coord
//!    session → skip silently (one debug line per registry record).
//! 3. **Debounce** — an event is only (re-)emitted when the material wire
//!    fields actually changed since the last emission for that registry
//!    record (an in-memory map; a restart simply re-emits once, which is
//!    idempotent-safe for readers that take the NEWEST event).
//!
//! ## Tier honesty
//!
//! `restore_tier` mirrors the frontend restore classifier
//! (`classifyRestoreAction` in `useTerminalInitialization.ts`): `"full"` —
//! the provider can deterministically resume the conversation by id — is
//! claimed ONLY for a CONFIRMED (`confirmed_at` set) record of AUTHORITATIVE
//! or OBSERVED origin (observed = the continuous binder's process-anchored
//! transcript bind — resume-safe, unlike the mtime-guess `reconciled`) whose
//! provider adapter declares [`RestoreTier::Full`]. Everything
//! else (provisional phantom shells, reconciled ids, terminal-only
//! providers) is `"terminal_only"` with a null `authoritative_session_id` —
//! the remote materialization then restores terminal+cwd+command with a
//! fresh conversation, never a resume typed against an id that can't (or
//! shouldn't) resume.
//!
//! ## Failure posture
//!
//! Best-effort throughout: the outbox append is local file I/O; any failure
//! is logged and swallowed. Emission can never fail the registry write.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use super::local_store::OutboxWriter;
use super::provider_adapter::{adapter_for, RestoreTier};
use super::session_lifecycle_store::{
    TerminalSessionRecord, ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED,
};
use super::SessionEventKind;

/// Wire value of the mirrored event's kind. Binding contract with the web
/// UI (`coord.session_events.event_kind` stores it verbatim). Matches
/// [`SessionEventKind::RestoreRecord`]`.as_str()`.
pub const RESTORE_RECORD_EVENT: &str = "restore-record";

/// Wire value of the full restore tier (provider resume with authoritative
/// id).
pub const TIER_FULL: &str = "full";
/// Wire value of the terminal-only restore tier (terminal+cwd+command,
/// fresh conversation).
pub const TIER_TERMINAL_ONLY: &str = "terminal_only";

/// Resolves a registry record's hosting `terminal_id` to the coord session
/// UUID mirroring that terminal (the id `terminal_create` stored via
/// `TerminalSession::set_coord_session_id`). Injected so tests don't need a
/// live `TerminalManager`.
pub type CoordSessionResolver = Box<dyn Fn(&str) -> Option<Uuid> + Send + Sync>;

/// Gate-1 probe (`Settings.session_metadata_sync_enabled`). Injected so
/// tests never touch the machine's real `settings.json`.
pub type ConsentGate = Box<dyn Fn() -> bool + Send + Sync>;

/// Emits debounced `restore-record` events into the session outbox.
/// Attached to the [`super::session_lifecycle_store::SessionLifecycleStore`]
/// at startup (`main.rs` setup) via `attach_restore_record_emitter`.
pub struct RestoreRecordEmitter {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
    resolver: CoordSessionResolver,
    gate: ConsentGate,
    /// Debounce state: registry key (`claude_session_id`) → the
    /// `(coord session, payload)` last durably emitted. In-memory by
    /// design — a restart re-emits at most once per record, and readers
    /// take the newest event, so a duplicate is harmless.
    last_emitted: Mutex<HashMap<String, (Uuid, JsonValue)>>,
    /// Registry keys already debug-logged as "no coord session — skipping",
    /// so the skip line fires once per record, not once per refresh.
    skipped: Mutex<HashSet<String>>,
}

impl RestoreRecordEmitter {
    /// Production constructor: gate 1 reads the real
    /// `Settings.session_metadata_sync_enabled`. The outbox MUST be the same
    /// `Arc` the `CoordSync` drain loop reads.
    pub fn new(
        outbox: Arc<OutboxWriter>,
        machine_id: Uuid,
        resolver: CoordSessionResolver,
    ) -> Self {
        Self::with_gate(
            outbox,
            machine_id,
            resolver,
            Box::new(crate::settings::get_session_metadata_sync_enabled),
        )
    }

    /// Constructor with an injectable consent gate (tests drive both gate
    /// positions deterministically without touching `settings.json`).
    pub fn with_gate(
        outbox: Arc<OutboxWriter>,
        machine_id: Uuid,
        resolver: CoordSessionResolver,
        gate: ConsentGate,
    ) -> Self {
        Self {
            outbox,
            machine_id,
            resolver,
            gate,
            last_emitted: Mutex::new(HashMap::new()),
            skipped: Mutex::new(HashSet::new()),
        }
    }

    /// Mirror one registry record. Gate 1
    /// (`session_metadata_sync_enabled`) is checked first — when off, this
    /// returns before any allocation or I/O and nothing leaves the machine.
    /// Never fails the caller: all errors are logged and swallowed.
    pub fn emit(&self, rec: &TerminalSessionRecord) {
        if !(self.gate)() {
            return;
        }
        // Linkage: the hosting terminal's coord session mirror. Absent →
        // skip silently (one debug line per registry record); the next
        // refresh retries, so a record whose terminal registers its coord
        // session a beat later is not lost.
        let Some(session_id) = (self.resolver)(&rec.terminal_id) else {
            let mut skipped = self
                .skipped
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if skipped.insert(rec.claude_session_id.clone()) {
                tracing::debug!(
                    session = %rec.claude_session_id,
                    terminal = %rec.terminal_id,
                    "restore_record_emitter: no coord session for terminal — skipping mirror"
                );
            }
            return;
        };

        let payload = restore_record_payload(rec, self.machine_id);

        // Debounce: only re-emit when the material wire fields (or the
        // coord session they attach to) actually changed. The lock spans
        // the outbox write so two concurrent refreshes of the same record
        // can't double-emit; the debounce state only advances on a
        // successful durable append so a failed write retries next time.
        let mut last = self
            .last_emitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.get(&rec.claude_session_id) == Some(&(session_id, payload.clone())) {
            return;
        }
        match self.outbox.record(
            self.machine_id,
            session_id,
            SessionEventKind::RestoreRecord,
            payload.clone(),
        ) {
            Ok(_) => {
                last.insert(rec.claude_session_id.clone(), (session_id, payload));
            }
            Err(e) => {
                tracing::warn!(
                    session = %rec.claude_session_id,
                    coord_session = %session_id,
                    error = %e,
                    "restore_record_emitter: outbox append failed (best-effort) — mirror dropped locally"
                );
            }
        }
    }
}

impl std::fmt::Debug for RestoreRecordEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreRecordEmitter")
            .field("machine_id", &self.machine_id)
            .finish()
    }
}

/// Build the binding wire payload for one registry record. Pure — the tier
/// decision mirrors the frontend `classifyRestoreAction` gate exactly:
/// `"full"` requires authoritative origin AND confirmation AND a
/// [`RestoreTier::Full`] provider; everything else degrades honestly to
/// `"terminal_only"` with a null authoritative id.
///
/// `launch_command` is adapter-derived and carries NO env vars or tokens:
/// the deterministic resume argv for `"full"` (account isolation rides env
/// at spawn time, never the recorded command), the provider's bare program
/// for `"terminal_only"` (a fresh conversation needs no id).
pub fn restore_record_payload(rec: &TerminalSessionRecord, machine_id: Uuid) -> JsonValue {
    let adapter = adapter_for(&rec.provider);
    // `observed` sits with `authoritative` here, mirroring
    // `classifyRestoreAction`: a confirmed observed bind is process-anchored to
    // its transcript (never the mtime guess `reconciled` quarantines), so its
    // id is resume-safe.
    let full = matches!(
        rec.origin.as_deref(),
        Some(ORIGIN_AUTHORITATIVE) | Some(ORIGIN_OBSERVED)
    ) && rec.confirmed_at.is_some()
        && adapter.restore_tier() == RestoreTier::Full;

    let (tier, authoritative_session_id, launch_command) = if full {
        (
            TIER_FULL,
            JsonValue::String(rec.claude_session_id.clone()),
            adapter
                .resume_command(&rec.claude_session_id, None)
                .join(" "),
        )
    } else {
        // The provider's bare program (first element of its resume argv) —
        // enough for a remote terminal-only restore to relaunch a fresh
        // conversation at the right cwd.
        let program = adapter
            .resume_command("", None)
            .into_iter()
            .next()
            .unwrap_or_else(|| rec.provider.clone());
        (TIER_TERMINAL_ONLY, JsonValue::Null, program)
    };

    json!({
        "provider": rec.provider,
        "authoritative_session_id": authoritative_session_id,
        "cwd": rec.working_dir,
        "launch_command": launch_command,
        "restore_tier": tier,
        "machine_id": machine_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::DEFAULT_PROVIDER;
    use tempfile::tempdir;

    fn rec(id: &str, terminal_id: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: Some("C:/accounts/hotmail".to_string()),
            working_dir: Some("C:/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: 1,
            title: Some("Claude 1".to_string()),
            terminal_id: terminal_id.to_string(),
            opened_at: 1,
            last_seen_at: 2,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: DEFAULT_PROVIDER.to_string(),
            origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
            restore_pending_at: None,
            confirmed_at: Some(3),
        }
    }

    /// Emitter over a tempdir outbox with a fixed terminal→coord-session
    /// resolver and an open (true) consent gate.
    fn emitter(
        coord_session: Uuid,
        gate_open: bool,
    ) -> (RestoreRecordEmitter, Arc<OutboxWriter>, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        let em = RestoreRecordEmitter::with_gate(
            outbox.clone(),
            Uuid::new_v4(),
            Box::new(move |terminal_id: &str| {
                (terminal_id == "term-linked").then_some(coord_session)
            }),
            Box::new(move || gate_open),
        );
        (em, outbox, dir)
    }

    #[test]
    fn emits_full_tier_payload_with_binding_shape() {
        let sid = Uuid::new_v4();
        let (em, outbox, _dir) = emitter(sid, true);
        em.emit(&rec("sess-1", "term-linked"));

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let row = &pending[0];
        assert_eq!(row.session_id, sid);
        assert_eq!(row.event_kind, RESTORE_RECORD_EVENT);
        assert_eq!(row.payload["provider"], "claude");
        assert_eq!(row.payload["authoritative_session_id"], "sess-1");
        assert_eq!(row.payload["cwd"], "C:/repo");
        assert_eq!(row.payload["launch_command"], "claude --resume sess-1");
        assert_eq!(row.payload["restore_tier"], TIER_FULL);
        assert!(row.payload["machine_id"].as_str().is_some());
        // The account config dir must NEVER ride the wire.
        assert!(
            !row.payload.to_string().contains("hotmail"),
            "config_dir leaked into the payload: {}",
            row.payload
        );
    }

    #[test]
    fn debounces_unchanged_records_and_reemits_on_material_change() {
        let (em, outbox, _dir) = emitter(Uuid::new_v4(), true);
        let r = rec("sess-1", "term-linked");
        em.emit(&r);
        em.emit(&r); // identical refresh — debounced
        assert_eq!(outbox.pending().unwrap().len(), 1, "no duplicate emission");

        // A material change (cwd) re-emits.
        let mut moved = r.clone();
        moved.working_dir = Some("C:/other".to_string());
        em.emit(&moved);
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 2, "material change re-emits");
        assert_eq!(pending[1].payload["cwd"], "C:/other");

        // A non-material change (title) does NOT re-emit.
        let mut renamed = moved.clone();
        renamed.title = Some("renamed".to_string());
        em.emit(&renamed);
        assert_eq!(
            outbox.pending().unwrap().len(),
            2,
            "title is not a wire field — debounced"
        );
    }

    #[test]
    fn gate_off_writes_nothing() {
        let (em, outbox, _dir) = emitter(Uuid::new_v4(), false);
        em.emit(&rec("sess-1", "term-linked"));
        assert!(
            outbox.pending().unwrap().is_empty(),
            "gate off ⇒ no outbox entry, nothing leaves the machine"
        );
    }

    #[test]
    fn unlinked_terminal_writes_nothing() {
        let (em, outbox, _dir) = emitter(Uuid::new_v4(), true);
        em.emit(&rec("sess-1", "term-unlinked"));
        em.emit(&rec("sess-1", "term-unlinked")); // skip line dedups; still nothing
        assert!(outbox.pending().unwrap().is_empty());
    }

    #[test]
    fn confirmation_flip_is_a_material_change() {
        // provisional → terminal_only; the confirm flips the SAME record to
        // full and must re-emit (this is why confirm_session also mirrors).
        let (em, outbox, _dir) = emitter(Uuid::new_v4(), true);
        let mut provisional = rec("sess-1", "term-linked");
        provisional.confirmed_at = None;
        em.emit(&provisional);

        let mut confirmed = provisional.clone();
        confirmed.confirmed_at = Some(9);
        em.emit(&confirmed);

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].payload["restore_tier"], TIER_TERMINAL_ONLY);
        assert_eq!(pending[1].payload["restore_tier"], TIER_FULL);
    }

    // -- pure payload mapping ------------------------------------------------

    #[test]
    fn payload_full_requires_authoritative_and_confirmed() {
        let machine = Uuid::new_v4();

        // Authoritative + confirmed + full-tier provider ⇒ full.
        let full = restore_record_payload(&rec("sess-1", "t"), machine);
        assert_eq!(full["restore_tier"], TIER_FULL);
        assert_eq!(full["authoritative_session_id"], "sess-1");
        assert_eq!(full["machine_id"], machine.to_string());

        // Authoritative but UNCONFIRMED (possible phantom shell) ⇒
        // terminal_only, null id — mirrors classifyRestoreAction.
        let mut provisional = rec("sess-1", "t");
        provisional.confirmed_at = None;
        let p = restore_record_payload(&provisional, machine);
        assert_eq!(p["restore_tier"], TIER_TERMINAL_ONLY);
        assert!(p["authoritative_session_id"].is_null());
        assert_eq!(p["launch_command"], "claude");

        // Observed origin (continuous binder, process-anchored) + confirmed ⇒
        // full — mirrors classifyRestoreAction's observed branch.
        let mut observed = rec("sess-1", "t");
        observed.origin = Some(ORIGIN_OBSERVED.to_string());
        let o = restore_record_payload(&observed, machine);
        assert_eq!(o["restore_tier"], TIER_FULL);
        assert_eq!(o["authoritative_session_id"], "sess-1");

        // Observed but UNCONFIRMED ⇒ terminal_only.
        let mut observed_prov = rec("sess-1", "t");
        observed_prov.origin = Some(ORIGIN_OBSERVED.to_string());
        observed_prov.confirmed_at = None;
        let op = restore_record_payload(&observed_prov, machine);
        assert_eq!(op["restore_tier"], TIER_TERMINAL_ONLY);

        // Reconciled origin (backstop guess, may name a foreign session) ⇒
        // never claimed as full.
        let mut reconciled = rec("sess-1", "t");
        reconciled.origin =
            Some(crate::session::session_lifecycle_store::ORIGIN_RECONCILED.to_string());
        let r = restore_record_payload(&reconciled, machine);
        assert_eq!(r["restore_tier"], TIER_TERMINAL_ONLY);
        assert!(r["authoritative_session_id"].is_null());

        // Absent origin (pre-field record — read as reconciled) ⇒ same.
        let mut unfielded = rec("sess-1", "t");
        unfielded.origin = None;
        let u = restore_record_payload(&unfielded, machine);
        assert_eq!(u["restore_tier"], TIER_TERMINAL_ONLY);
    }
}
