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
//! (`classifyRestoreAction` in `useTerminalInitialization.ts`), gate for gate.
//! `"full"` — the provider can deterministically resume the conversation by
//! id — is claimed ONLY when ALL FIVE of the classifier's gates pass:
//!
//! 1. the id is shell-safe ([`crate::session::session_id::is_valid_session_id`],
//!    the Rust twin of the frontend's `isValidSessionId`; the frontend answers
//!    `"skip-invalid"` here);
//! 2. the origin is AUTHORITATIVE or OBSERVED (observed = the continuous
//!    binder's process-anchored transcript bind — resume-safe, unlike the
//!    mtime-guess `reconciled`);
//! 3. the record is CONFIRMED (`confirmed_at` set);
//! 4. its transcript was not probed ABSENT (unknown never downgrades);
//! 5. its provider adapter declares [`RestoreTier::Full`].
//!
//! Everything else (invalid ids, provisional phantom shells, reconciled ids,
//! transcript-less confirmations, terminal-only providers) is
//! `"terminal_only"` with a null `authoritative_session_id`, and the id is
//! never interpolated into `launch_command` — the remote materialization then
//! restores terminal+cwd+command with a fresh conversation, never a resume
//! typed against an id that can't (or shouldn't) resume.
//!
//! The mirror claim is ENFORCED, not asserted: the test
//! `emitter_tier_matches_the_frontend_classifier_on_every_crossproduct_row`
//! (this module) and the vitest `restoreTierCrossProduct.test.ts` (next to the
//! classifier) both read the SAME committed table,
//! `src/components/terminal/__fixtures__/restore-tier-crossproduct.json` — the
//! full 72-row cross product of the five gates' inputs. The vitest pins the
//! table to `classifyRestoreAction`; the Rust test pins it to
//! [`restore_record_payload`]. A gate added or dropped on either side turns
//! one of the two red.
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
use super::provider_adapter::{adapter_for, RestoreTier, SessionProviderAdapter};
use super::session_id::is_valid_session_id;
use super::session_lifecycle_store::{
    TerminalSessionRecord, ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED,
};
use super::SessionEventKind;

/// Wire value of the mirrored event's kind. Binding contract with the web
/// UI (`coord.session_events.event_kind` stores it verbatim). Matches
/// [`SessionEventKind::RestoreRecord`]`.as_str()`.
pub const RESTORE_RECORD_EVENT: &str = "restore-record";

/// COORD WIRE spelling of the full restore tier (provider resume with
/// authoritative id). Derived from [`RestoreTier::wire_str`] — the one place
/// the wire spelling is written — so the constant cannot drift from the enum.
pub const TIER_FULL: &str = RestoreTier::Full.wire_str();
/// COORD WIRE spelling of the terminal-only restore tier
/// (terminal+cwd+command, fresh conversation): `"terminal_only"`, underscore.
/// The frontend spells the same concept `"terminal-only"` (hyphen); the
/// conversion is [`RestoreTier::from_wire_str`] / [`RestoreTier::frontend_str`],
/// never a second literal.
pub const TIER_TERMINAL_ONLY: &str = RestoreTier::TerminalOnly.wire_str();

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
    ///
    /// `transcript_exists` is the caller's transcript probe result for this
    /// record (`None` ⇒ could not determine); it must match what the local
    /// restore path sees, or the mirrored tier and local behavior diverge.
    pub fn emit(&self, rec: &TerminalSessionRecord, transcript_exists: Option<bool>) {
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

        let payload = restore_record_payload(rec, self.machine_id, transcript_exists);

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
/// decision is [`mirrored_restore_tier`], which carries ALL FIVE of the
/// frontend `classifyRestoreAction` gates: `"full"` requires a shell-safe id
/// ([`crate::session::session_id::is_valid_session_id`]) AND authoritative or
/// observed origin AND confirmation AND a transcript that was not
/// probed-absent AND a [`RestoreTier::Full`] provider; everything else
/// degrades honestly to `"terminal_only"` with a null authoritative id. The
/// equivalence is pinned row-by-row by
/// `emitter_tier_matches_the_frontend_classifier_on_every_crossproduct_row`
/// against the same fixture the frontend classifier's vitest reads.
///
/// `transcript_exists` is the record's transcript probe result: `Some(false)`
/// only when the probe positively determined there is NO transcript, `None`
/// when it could not determine (see
/// [`crate::session::session_lifecycle_store::SessionLifecycleStore::probe_transcript_exists`]).
/// It belongs in this predicate because the mirrored tier is a PROMISE to the
/// remote consumer — `handoff` materializes `origin: authoritative` +
/// `confirmed_at` from `tier == full` alone — so mirroring `"full"` for an id
/// this machine now refuses to `--resume` would hand a peer a resume that can
/// only fail. Unknown never downgrades, matching the local gate.
///
/// `launch_command` is adapter-derived and carries NO env vars or tokens:
/// the deterministic resume argv for `"full"` (account isolation rides env
/// at spawn time, never the recorded command), the provider's bare program
/// for `"terminal_only"` (a fresh conversation needs no id).
pub fn restore_record_payload(
    rec: &TerminalSessionRecord,
    machine_id: Uuid,
    transcript_exists: Option<bool>,
) -> JsonValue {
    restore_record_payload_for_adapter(
        rec,
        machine_id,
        transcript_exists,
        adapter_for(&rec.provider).as_ref(),
    )
}

/// The five-gate "restorable at" predicate — the Rust half of the frontend
/// `classifyRestoreAction` (`"auto-resume"` ⇔ [`RestoreTier::Full`]). Pure,
/// and the ONLY place the emitter decides a tier.
///
/// Gate 1 (the id) is DEFENCE IN DEPTH: the load-bearing fix is the ingress
/// gate on `POST /control/session-open` (runner#1373), which keeps an unsafe id
/// out of the local registry altogether. This gate exists so the mirror cannot
/// promise a peer a resume the frontend classifier would refuse, whatever
/// route a record took into the registry.
pub fn mirrored_restore_tier(
    rec: &TerminalSessionRecord,
    transcript_exists: Option<bool>,
    provider_tier: RestoreTier,
) -> RestoreTier {
    // `observed` sits with `authoritative` here, mirroring
    // `classifyRestoreAction`: a confirmed observed bind is process-anchored to
    // its transcript (never the mtime guess `reconciled` quarantines), so its
    // id is resume-safe.
    let full = is_valid_session_id(&rec.claude_session_id)
        && matches!(
            rec.origin.as_deref(),
            Some(ORIGIN_AUTHORITATIVE) | Some(ORIGIN_OBSERVED)
        )
        && rec.confirmed_at.is_some()
        && transcript_exists != Some(false)
        && provider_tier == RestoreTier::Full;
    if full {
        RestoreTier::Full
    } else {
        RestoreTier::TerminalOnly
    }
}

/// [`restore_record_payload`] over an explicit adapter — the seam the
/// cross-product test uses to drive a terminal-only provider, which no shipped
/// adapter declares yet.
fn restore_record_payload_for_adapter(
    rec: &TerminalSessionRecord,
    machine_id: Uuid,
    transcript_exists: Option<bool>,
    adapter: &dyn SessionProviderAdapter,
) -> JsonValue {
    let full =
        mirrored_restore_tier(rec, transcript_exists, adapter.restore_tier()) == RestoreTier::Full;

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
        em.emit(&rec("sess-1", "term-linked"), None);

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
        em.emit(&r, None);
        em.emit(&r, None); // identical refresh — debounced
        assert_eq!(outbox.pending().unwrap().len(), 1, "no duplicate emission");

        // A material change (cwd) re-emits.
        let mut moved = r.clone();
        moved.working_dir = Some("C:/other".to_string());
        em.emit(&moved, None);
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 2, "material change re-emits");
        assert_eq!(pending[1].payload["cwd"], "C:/other");

        // A non-material change (title) does NOT re-emit.
        let mut renamed = moved.clone();
        renamed.title = Some("renamed".to_string());
        em.emit(&renamed, None);
        assert_eq!(
            outbox.pending().unwrap().len(),
            2,
            "title is not a wire field — debounced"
        );
    }

    #[test]
    fn gate_off_writes_nothing() {
        let (em, outbox, _dir) = emitter(Uuid::new_v4(), false);
        em.emit(&rec("sess-1", "term-linked"), None);
        assert!(
            outbox.pending().unwrap().is_empty(),
            "gate off ⇒ no outbox entry, nothing leaves the machine"
        );
    }

    #[test]
    fn unlinked_terminal_writes_nothing() {
        let (em, outbox, _dir) = emitter(Uuid::new_v4(), true);
        em.emit(&rec("sess-1", "term-unlinked"), None);
        em.emit(&rec("sess-1", "term-unlinked"), None); // skip line dedups; still nothing
        assert!(outbox.pending().unwrap().is_empty());
    }

    #[test]
    fn confirmation_flip_is_a_material_change() {
        // provisional → terminal_only; the confirm flips the SAME record to
        // full and must re-emit (this is why confirm_session also mirrors).
        let (em, outbox, _dir) = emitter(Uuid::new_v4(), true);
        let mut provisional = rec("sess-1", "term-linked");
        provisional.confirmed_at = None;
        em.emit(&provisional, None);

        let mut confirmed = provisional.clone();
        confirmed.confirmed_at = Some(9);
        em.emit(&confirmed, None);

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
        let full = restore_record_payload(&rec("sess-1", "t"), machine, None);
        assert_eq!(full["restore_tier"], TIER_FULL);
        assert_eq!(full["authoritative_session_id"], "sess-1");
        assert_eq!(full["machine_id"], machine.to_string());

        // Authoritative but UNCONFIRMED (possible phantom shell) ⇒
        // terminal_only, null id — mirrors classifyRestoreAction.
        let mut provisional = rec("sess-1", "t");
        provisional.confirmed_at = None;
        let p = restore_record_payload(&provisional, machine, None);
        assert_eq!(p["restore_tier"], TIER_TERMINAL_ONLY);
        assert!(p["authoritative_session_id"].is_null());
        assert_eq!(p["launch_command"], "claude");

        // Observed origin (continuous binder, process-anchored) + confirmed ⇒
        // full — mirrors classifyRestoreAction's observed branch.
        let mut observed = rec("sess-1", "t");
        observed.origin = Some(ORIGIN_OBSERVED.to_string());
        let o = restore_record_payload(&observed, machine, None);
        assert_eq!(o["restore_tier"], TIER_FULL);
        assert_eq!(o["authoritative_session_id"], "sess-1");

        // Observed but UNCONFIRMED ⇒ terminal_only.
        let mut observed_prov = rec("sess-1", "t");
        observed_prov.origin = Some(ORIGIN_OBSERVED.to_string());
        observed_prov.confirmed_at = None;
        let op = restore_record_payload(&observed_prov, machine, None);
        assert_eq!(op["restore_tier"], TIER_TERMINAL_ONLY);

        // Reconciled origin (backstop guess, may name a foreign session) ⇒
        // never claimed as full.
        let mut reconciled = rec("sess-1", "t");
        reconciled.origin =
            Some(crate::session::session_lifecycle_store::ORIGIN_RECONCILED.to_string());
        let r = restore_record_payload(&reconciled, machine, None);
        assert_eq!(r["restore_tier"], TIER_TERMINAL_ONLY);
        assert!(r["authoritative_session_id"].is_null());

        // Absent origin (pre-field record — read as reconciled) ⇒ same.
        let mut unfielded = rec("sess-1", "t");
        unfielded.origin = None;
        let u = restore_record_payload(&unfielded, machine, None);
        assert_eq!(u["restore_tier"], TIER_TERMINAL_ONLY);
    }

    /// The mirrored tier must not promise a resume this machine would refuse.
    /// A confirmed authoritative record whose transcript was probed ABSENT is
    /// mirrored `terminal_only` with a null id — otherwise the peer consumer
    /// (`handoff` rebuilds `origin: authoritative` + `confirmed_at` from
    /// `tier == full`) inherits a `--resume` that can only fail. UNKNOWN
    /// (`None`) must not downgrade, matching `classifyRestoreAction`.
    #[test]
    fn payload_full_also_requires_a_transcript_that_was_not_probed_absent() {
        let machine = Uuid::new_v4();
        let confirmed_authoritative = rec("sess-1", "t");

        let absent = restore_record_payload(&confirmed_authoritative, machine, Some(false));
        assert_eq!(
            absent["restore_tier"], TIER_TERMINAL_ONLY,
            "probed-absent transcript must not be mirrored as a full resume"
        );
        assert!(absent["authoritative_session_id"].is_null());
        assert_eq!(absent["launch_command"], "claude");

        let present = restore_record_payload(&confirmed_authoritative, machine, Some(true));
        assert_eq!(present["restore_tier"], TIER_FULL);
        assert_eq!(present["authoritative_session_id"], "sess-1");

        let unknown = restore_record_payload(&confirmed_authoritative, machine, None);
        assert_eq!(
            unknown["restore_tier"], TIER_FULL,
            "UNKNOWN must not downgrade — it is not evidence of absence"
        );
    }

    // -- item 1 step 2: the fifth gate ---------------------------------------

    /// A confirmed authoritative record whose id fails the shell-safety gate
    /// is `terminal_only` with a null id — the frontend classifies it
    /// `"skip-invalid"` — and the raw id never reaches `launch_command`.
    #[test]
    fn payload_refuses_full_for_an_id_that_fails_the_shell_safety_gate() {
        let machine = Uuid::new_v4();
        for bad in ["abc; rm -rf /", "$(id)", "abc\n", "a b", ""] {
            let p = restore_record_payload(&rec(bad, "t"), machine, Some(true));
            assert_eq!(p["restore_tier"], TIER_TERMINAL_ONLY, "id {bad:?}");
            assert!(p["authoritative_session_id"].is_null(), "id {bad:?}");
            assert_eq!(p["launch_command"], "claude", "id {bad:?} leaked into argv");
        }
        // Negative control: the same record with a safe id is full.
        let ok = restore_record_payload(&rec("sess-1", "t"), machine, Some(true));
        assert_eq!(ok["restore_tier"], TIER_FULL);
    }

    // -- the cross-seam guard (plan item 1 Verification, 12c #2) -------------

    /// The shared table. The vitest `restoreTierCrossProduct.test.ts` pins it
    /// to `classifyRestoreAction`; this module pins it to the emitter.
    const CROSSPRODUCT_FIXTURE: &str = include_str!(
        "../../../src/components/terminal/__fixtures__/restore-tier-crossproduct.json"
    );

    /// The fixture's terminal-only provider. No shipped adapter declares
    /// [`RestoreTier::TerminalOnly`], so this test supplies one (the vitest
    /// supplies its TS twin through a module mock); everything but the tier
    /// delegates to the real Claude adapter.
    const FIXTURE_TERMINAL_ONLY_PROVIDER: &str = "fixture-terminal-only";

    struct FixtureTerminalOnlyAdapter;

    impl SessionProviderAdapter for FixtureTerminalOnlyAdapter {
        fn provider(&self) -> &'static str {
            FIXTURE_TERMINAL_ONLY_PROVIDER
        }
        fn launch_with_identity(
            &self,
            cwd: &str,
            account: Option<&str>,
        ) -> crate::session::provider_adapter::LaunchSpec {
            crate::session::provider_adapter::ClaudeAdapter.launch_with_identity(cwd, account)
        }
        fn capture_hook_delivery(
            &self,
            cwd: &str,
        ) -> crate::session::provider_adapter::DeliverySpec {
            crate::session::provider_adapter::ClaudeAdapter.capture_hook_delivery(cwd)
        }
        fn resume_command(&self, session_id: &str, account: Option<&str>) -> Vec<String> {
            crate::session::provider_adapter::ClaudeAdapter.resume_command(session_id, account)
        }
        fn account_isolation(
            &self,
            account: Option<&str>,
        ) -> std::collections::BTreeMap<String, String> {
            crate::session::provider_adapter::ClaudeAdapter.account_isolation(account)
        }
        fn resume_handshake_patterns(&self) -> crate::session::provider_adapter::HandshakePatterns {
            crate::session::provider_adapter::ClaudeAdapter.resume_handshake_patterns()
        }
        fn restore_tier(&self) -> RestoreTier {
            RestoreTier::TerminalOnly
        }
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureRow {
        id_kind: String,
        session_id: String,
        origin: String,
        confirmed: bool,
        transcript: String,
        provider_tier: String,
        provider: String,
        action: String,
        wire_tier: String,
    }

    #[derive(serde::Deserialize)]
    struct Fixture {
        rows: Vec<FixtureRow>,
    }

    fn fixture_rows() -> Vec<FixtureRow> {
        serde_json::from_str::<Fixture>(CROSSPRODUCT_FIXTURE)
            .expect("restore-tier-crossproduct.json parses")
            .rows
    }

    /// The Rust emitter's tier equals the TS classifier's verdict on every row
    /// of the shared cross product (`"auto-resume"` ⇔ `"full"`). The
    /// shell-metacharacter rows are the ones that failed before the fifth gate.
    #[test]
    fn emitter_tier_matches_the_frontend_classifier_on_every_crossproduct_row() {
        let _amb = crate::test_env::isolated_ambient();
        let machine = Uuid::new_v4();
        let rows = fixture_rows();

        // Completeness, enumerated independently of the fixture: a truncated
        // or de-duplicated table must fail here, not silently shrink the pin.
        let mut expected_keys = HashSet::new();
        for id_kind in ["valid", "shell-metacharacter"] {
            for origin in ["authoritative", "observed", "reconciled"] {
                for confirmed in [true, false] {
                    for transcript in ["present", "absent", "unprobed"] {
                        for provider_tier in ["full", "terminal-only"] {
                            expected_keys.insert(format!(
                                "{id_kind}|{origin}|{confirmed}|{transcript}|{provider_tier}"
                            ));
                        }
                    }
                }
            }
        }
        let actual_keys: HashSet<String> = rows
            .iter()
            .map(|r| {
                format!(
                    "{}|{}|{}|{}|{}",
                    r.id_kind, r.origin, r.confirmed, r.transcript, r.provider_tier
                )
            })
            .collect();
        assert_eq!(rows.len(), 72, "fixture must carry the full cross product");
        assert_eq!(
            actual_keys, expected_keys,
            "fixture rows ≠ the cross product"
        );

        // The `claude` rows go through the REAL registry: pin that it is still
        // the full-tier provider the fixture says it is.
        assert_eq!(adapter_for("claude").restore_tier(), RestoreTier::Full);

        let mut full_rows = 0;
        for r in &rows {
            let label = format!(
                "row {}|{}|{}|{}|{} (id {:?})",
                r.id_kind, r.origin, r.confirmed, r.transcript, r.provider_tier, r.session_id
            );
            // The id kind must be what the Rust gate says it is — otherwise the
            // row is testing the JS/Rust regex-dialect divergence documented in
            // `session_id.rs`, not the predicate.
            assert_eq!(
                is_valid_session_id(&r.session_id),
                r.id_kind == "valid",
                "{label}: id kind disagrees with is_valid_session_id"
            );

            let mut record = rec(&r.session_id, "t");
            record.origin = Some(r.origin.clone());
            record.confirmed_at = r.confirmed.then_some(3);
            record.provider = r.provider.clone();
            let transcript_exists = match r.transcript.as_str() {
                "present" => Some(true),
                "absent" => Some(false),
                "unprobed" => None,
                other => panic!("{label}: unknown transcript axis {other:?}"),
            };

            let declared_tier = RestoreTier::from_frontend_str(&r.provider_tier)
                .unwrap_or_else(|| panic!("{label}: providerTier is not a frontend tier"));
            let payload = match declared_tier {
                RestoreTier::Full => {
                    assert_eq!(r.provider, "claude", "{label}");
                    restore_record_payload(&record, machine, transcript_exists)
                }
                RestoreTier::TerminalOnly => {
                    assert_eq!(r.provider, FIXTURE_TERMINAL_ONLY_PROVIDER, "{label}");
                    restore_record_payload_for_adapter(
                        &record,
                        machine,
                        transcript_exists,
                        &FixtureTerminalOnlyAdapter,
                    )
                }
            };

            // The TS verdict, converted across the seam explicitly: only
            // "auto-resume" is the full tier.
            let ts_tier = match r.action.as_str() {
                "auto-resume" => RestoreTier::Full,
                "terminal-only" | "skip-invalid" => RestoreTier::TerminalOnly,
                other => panic!("{label}: unknown RestoreAction {other:?}"),
            };
            assert_eq!(
                RestoreTier::from_wire_str(&r.wire_tier),
                Some(ts_tier),
                "{label}: fixture wireTier disagrees with its own action"
            );

            let emitted = payload["restore_tier"].as_str().unwrap_or_default();
            assert_eq!(
                emitted,
                ts_tier.wire_str(),
                "{label}: emitter mirrors {emitted:?}, classifyRestoreAction says {:?}",
                r.action
            );
            match ts_tier {
                RestoreTier::Full => {
                    full_rows += 1;
                    assert_eq!(payload["authoritative_session_id"], r.session_id.as_str());
                }
                RestoreTier::TerminalOnly => {
                    assert!(payload["authoritative_session_id"].is_null(), "{label}");
                    assert!(
                        !payload["launch_command"]
                            .as_str()
                            .unwrap_or_default()
                            .contains(&r.session_id),
                        "{label}: id interpolated into a terminal-only launch_command"
                    );
                }
            }
        }
        // Negative control: an emitter that returned terminal_only
        // unconditionally would satisfy every other row.
        assert!(
            full_rows > 0,
            "no row is full on both sides — the pin is vacuous"
        );
    }
}
