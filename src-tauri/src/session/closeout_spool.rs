//! Durable spool for the two closeout coord writes — plan
//! `2026-08-28-closeout-has-no-durable-store-when-the-runner-is-offline`,
//! Phase 3 (the PRODUCER half).
//!
//! Phase 2 widened the session outbox with [`SessionEventKind::GateRegistration`]
//! and [`SessionEventKind::FindingPosted`] and taught the `CoordSync` drain to
//! replay them. Nothing WROTE those kinds, so the replay half was
//! dead-but-tested. This module is the writer: the runner's loopback coord-write
//! forwarders (`mcp_api::coord_write_proxy_handler` for register-gate,
//! `mcp_api::coord_mcp_proxy_handler` for the `coord_post_finding` tool call)
//! hand an unreachable write here instead of returning a bare error, and the
//! existing drain → auth → retry machinery carries it to coord when coord comes
//! back.
//!
//! ## What is spoolable and what is not
//!
//! ONLY the transport class. A `400`/`403`/`404`/`422` is coord *reaching a
//! verdict on the content*; spooling it would replay a guaranteed failure
//! `coord_sync::BEST_EFFORT_MAX_ATTEMPTS` times and then Ack-drop it,
//! turning a visible immediate failure into an invisible delayed one. The split
//! is [`classify_coord_write_status`], which both halves call so they cannot
//! drift: `coord_sync`'s drain maps it to `PermanentFailure` vs `Transport`,
//! and the forwarders map it to "return coord's verdict" vs "spool".
//!
//! ## The credential this does NOT need
//!
//! The plan's other Phase-3 half — an agent session draining the outbox itself
//! with a runner-independent credential — is blocked on an open operator
//! security ruling (coord gate `ece99898-30c6-4f8c-be8e-1de5f09abebc`) about an
//! unauthenticated credential mint, and is deliberately NOT built here. This
//! half needs no such credential: the runner is alive (it is serving the
//! loopback request being spooled), so the drain replays under the device
//! credential the runner already holds. Only the runner-fully-wedged residual
//! needs the blocked half.

use std::sync::{Arc, OnceLock};

use serde_json::{json, Map as JsonMap, Value as JsonValue};
use uuid::Uuid;

use super::local_store::{OutboxRecord, OutboxWriter};
use super::SessionEventKind;

/// The runner-only hint key a caller may put in a register-gate request body to
/// seed Phase 2's lazy `work_unit_not_found` bootstrap.
///
/// Coord's `UnitGateRequest` is NOT `deny_unknown_fields`, so this key would be
/// silently ignored upstream — but the forwarder STRIPS it before forwarding
/// anyway, so nothing coord does not model ever crosses the wire. It survives
/// only into the spooled outbox payload, where the drain sends it to
/// `POST /coord/work-units/upsert` after (and only after) coord answers 404
/// `work_unit_not_found`.
///
/// Absent is fine and is the common case: Phase 2 Ack-drops the 404 with a warn
/// when no bootstrap was recorded, which is the honest outcome for a spool that
/// only ever knew the slug.
pub const WORK_UNIT_UPSERT_HINT_KEY: &str = "work_unit_upsert";

/// How a non-2xx coord write response divides for retry purposes.
///
/// The ONE definition both halves of the plan read. `coord_sync`'s drain turns
/// it into `PushOutcome::{PermanentFailure, Transport}`; the loopback forwarders
/// turn it into "hand coord's verdict back to the caller" vs "spool it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordWriteClass {
    /// Coord answered, and the answer refuses the CONTENT. Neither a retry nor
    /// a spool can change a body the queue is unable to edit.
    Permanent,
    /// The write did not reach a coord that could have accepted it. Worth
    /// retrying, and therefore worth spooling.
    Spoolable,
}

/// Classify a coord write's HTTP status.
///
/// `4xx` is [`CoordWriteClass::Permanent`]; everything else non-2xx (`5xx`, and
/// any nonsense status) is [`CoordWriteClass::Spoolable`]. This is exactly the
/// rule Phase 2's drain arms already applied inline (`status.is_client_error()`
/// → `PermanentFailure`, else `Transport`) — extracted rather than restated so
/// the producer and the drain cannot disagree about which failures are worth
/// keeping.
///
/// A *transport* failure (connection refused, DNS, TLS, timeout) produces no
/// status at all; those callers use [`CoordWriteClass::Spoolable`] directly.
///
/// Deliberately NOT carved out here: `429` and `408`, which are arguably
/// retryable 4xx. Phase 2's `gate_registration`/`finding_posted` arms treat all
/// 4xx alike, and this function's job is to state that shared rule, not to
/// change it under Phase 2's feet. The generic `output_chunk` arm in
/// `coord_sync` has its own 429 handling and stays where it is.
pub fn classify_coord_write_status(status: u16) -> CoordWriteClass {
    if (400..500).contains(&status) {
        CoordWriteClass::Permanent
    } else {
        CoordWriteClass::Spoolable
    }
}

/// A write that could not reach coord and was written to the outbox instead.
#[derive(Debug, Clone)]
pub struct Spooled {
    /// Wire kind of the outbox row — `"gate_registration"` / `"finding_posted"`.
    pub kind: &'static str,
    /// The synthetic seq lane the row landed in.
    pub session_id: Uuid,
    /// Monotonic seq within that lane. `(session_id, seq)` is the row's
    /// identity for an ACK.
    pub seq: i64,
}

impl Spooled {
    fn from_record(kind: &'static str, rec: &OutboxRecord) -> Self {
        Self {
            kind,
            session_id: rec.session_id,
            seq: rec.seq,
        }
    }
}

/// Writer for the two closeout kinds, over the SAME [`OutboxWriter`] the
/// `CoordSync` drain loop reads.
///
/// It must be the same `Arc` — two `OutboxWriter`s over one file is a
/// correctness bug, not merely duplication: each keeps its own in-memory
/// per-`(machine_id, session_id)` seq counter and its own append cursor, so two
/// of them mint colliding seqs and interleave partial rewrites during
/// compaction. [`install`] takes the handle `main.rs` already clones for the
/// AI-session coord registrar and the helper-task registrar, for exactly that
/// reason.
pub struct CloseoutSpool {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
}

impl CloseoutSpool {
    pub fn new(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> Self {
        Self { outbox, machine_id }
    }

    /// The outbox this spool writes to — the same one `CoordSync` drains.
    /// Exposed so a caller (and a test in another module) can read back what
    /// was spooled without a second handle on the file.
    pub fn outbox(&self) -> &OutboxWriter {
        &self.outbox
    }

    /// Spool a `POST /coord/work-units/{slug}/register-gate` the forwarder
    /// could not deliver.
    ///
    /// `register_gate_body` is the caller's `UnitGateRequest` object with the
    /// runner-only [`WORK_UNIT_UPSERT_HINT_KEY`] already lifted out into
    /// `work_unit_upsert`. The payload written is the shape Phase 2's
    /// `gate_registration` drain arm reads: the register-gate fields at the top
    /// level, plus `work_unit_slug` (a PATH segment, so it cannot ride in the
    /// body) and the optional bootstrap.
    ///
    /// ## ⚠️ Replay is NOT idempotent — there is no coord-side anchor uniqueness
    ///
    /// The plan asserted "idempotent replay via coord-side
    /// `UNIQUE (session_id, seq)`". That constraint is real, but it is on
    /// `coord.session_events`, and `register_unit_gate` never writes that table
    /// — it goes straight to `gates::register_gate_core`, which has no
    /// duplicate detection on `(work_unit_id, phase_name)` or on any other
    /// anchor. So the one window that matters here — the POST SUCCEEDED but the
    /// local ACK write failed, leaving the row pending — produces a SECOND GATE
    /// on the next drain tick, not a no-op.
    ///
    /// That window is narrow (the forwarder only spools when it got no
    /// success), and a duplicate gate is visible and withdrawable, so it is the
    /// better failure than losing the gate entirely. But do not read Phase 2's
    /// bounded retry as safe-by-idempotency: it is safe by being bounded. The
    /// durable fix is a coord-side uniqueness check on the gate anchor, which
    /// cannot be made from this repo.
    pub fn spool_gate_registration(
        &self,
        slug: &str,
        register_gate_body: &JsonMap<String, JsonValue>,
        work_unit_upsert: Option<JsonValue>,
    ) -> std::io::Result<Spooled> {
        let mut payload = register_gate_body.clone();
        payload.insert(
            "work_unit_slug".to_string(),
            JsonValue::String(slug.to_string()),
        );
        if let Some(upsert) = work_unit_upsert {
            payload.insert(WORK_UNIT_UPSERT_HINT_KEY.to_string(), upsert);
        }
        let rec = self.outbox.record(
            self.machine_id,
            gate_lane(slug),
            SessionEventKind::GateRegistration,
            JsonValue::Object(payload),
        )?;
        Ok(Spooled::from_record(
            SessionEventKind::GateRegistration.as_str(),
            &rec,
        ))
    }

    /// Spool a `POST /coord/agent-findings` the forwarder could not deliver.
    ///
    /// `body` is written VERBATIM — coord's `PostFindingBody` is
    /// `deny_unknown_fields` and rejects `tenant_id` / `author_session` /
    /// `author_device` BY NAME, so the payload must carry exactly what the
    /// caller sent and nothing this module adds. Identity is lifted from the
    /// credential the drain presents, which is the runner's, so a replayed
    /// finding is attributed to this device — the same attribution the live
    /// forwarder would have produced.
    pub fn spool_finding(&self, body: &JsonValue) -> std::io::Result<Spooled> {
        let rec = self.outbox.record(
            self.machine_id,
            FINDING_LANE,
            SessionEventKind::FindingPosted,
            body.clone(),
        )?;
        Ok(Spooled::from_record(
            SessionEventKind::FindingPosted.as_str(),
            &rec,
        ))
    }
}

/// The outbox keys its monotonic `seq` on `(machine_id, session_id)`, but a
/// closeout write has no coord session of its own — the drain arms for both
/// kinds build their URL from the payload, never from `session_id`, and coord
/// never sees this id.
///
/// Gates get one deterministic lane PER WORK UNIT so several gates for one plan
/// replay in the order they were registered, without a busy plan's lane
/// serializing an unrelated one. Mirrors `HelperTaskRegistrar`'s per-app lane.
fn gate_lane(slug: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("closeout-gate:{slug}").as_bytes(),
    )
}

/// One lane for findings: they carry no anchor to partition on, and posting
/// order is the only ordering a reader could care about.
const FINDING_LANE: Uuid = Uuid::from_u128(0x9f2c_1d64_7a3b_5e18_b0c4_2f6d_8a19_47e3);

/// Process-wide handle, installed once by `main.rs` from the SAME outbox `Arc`
/// the drain reads.
///
/// A global rather than a field on `ApiState` because the write forwarder that
/// needs it (`coord_write_proxy_handler`) takes no axum `State` at all, and the
/// outbox is genuinely a process singleton — the same reason `coord_mcp`'s
/// nonce registry and the forwarder's `reqwest` clients live in statics here.
/// Every code path that CONSUMES it takes the spool as an explicit parameter,
/// so the global is read at exactly one place per handler and unit tests build
/// their own [`CloseoutSpool`] over a tempdir without touching it.
static GLOBAL: OnceLock<Arc<CloseoutSpool>> = OnceLock::new();

/// Install the process-wide spool. Returns `false` if one was already
/// installed (the second call is ignored — a second writer over the same file
/// is the bug this guards).
pub fn install(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> bool {
    GLOBAL
        .set(Arc::new(CloseoutSpool::new(outbox, machine_id)))
        .is_ok()
}

/// The installed spool, or `None` when the session subsystem never came up
/// (headless test harnesses, a runner whose outbox open failed outright).
/// `None` means "no durable store available" — the caller must then report the
/// write as LOST, never as spooled.
pub fn global() -> Option<Arc<CloseoutSpool>> {
    GLOBAL.get().cloned()
}

/// What [`parse_gate_registration_body`] recovered from a register-gate request.
#[derive(Debug, Clone)]
pub struct GateRegistrationInput {
    /// The caller's `UnitGateRequest` body with the runner-only hint removed —
    /// what both the upstream forward and the spooled payload use.
    pub body: JsonMap<String, JsonValue>,
    /// The lifted [`WORK_UNIT_UPSERT_HINT_KEY`] object, when the caller sent a
    /// usable one.
    pub work_unit_upsert: Option<JsonValue>,
    /// Whether the raw body carried the hint key AT ALL — in any shape, usable
    /// or not. The forwarder must re-serialize before forwarding whenever this
    /// is true, so a key coord does not model never crosses the wire even when
    /// it was junk.
    pub hint_present: bool,
}

/// Extract the register-gate body a spool would replay, and the runner-only
/// `work_unit_upsert` hint, from the caller's raw request bytes.
///
/// `None` — do NOT spool — when the body is not a JSON object, or when it is
/// missing either field coord REQUIRES (`predicate`, `phase_name`). Such a body
/// is a guaranteed 422 whenever coord comes back, so keeping it would only
/// convert an immediate visible failure into a delayed silent one, which is the
/// exact defect this plan exists to remove.
pub fn parse_gate_registration_body(raw: &[u8]) -> Option<GateRegistrationInput> {
    let parsed: JsonValue = serde_json::from_slice(raw).ok()?;
    let mut obj = match parsed {
        JsonValue::Object(o) => o,
        _ => return None,
    };
    if !obj.contains_key("predicate") || !obj.contains_key("phase_name") {
        return None;
    }
    let raw_hint = obj.remove(WORK_UNIT_UPSERT_HINT_KEY);
    Some(GateRegistrationInput {
        body: obj,
        hint_present: raw_hint.is_some(),
        work_unit_upsert: raw_hint.filter(|v| v.is_object()),
    })
}

/// The `coord_post_finding` arguments a spool would replay, from a JSON-RPC
/// request body.
///
/// `None` — do NOT spool — unless the body is a `tools/call` naming
/// `coord_post_finding` whose `arguments` is an object carrying non-empty
/// `title` and `body` strings. Those two are coord's only required fields;
/// without them the replay is a guaranteed 400, and the same
/// delayed-silent-failure argument as above applies.
///
/// The returned object is the request body for `POST /coord/agent-findings`
/// verbatim: coord's MCP tool declares `additionalProperties: false` over
/// exactly `PostFindingBody`'s field set, so the tool's `arguments` object IS
/// the REST body.
pub fn parse_post_finding_arguments(raw: &[u8]) -> Option<JsonValue> {
    let req: JsonValue = serde_json::from_slice(raw).ok()?;
    if req.get("method").and_then(JsonValue::as_str) != Some("tools/call") {
        return None;
    }
    let params = req.get("params")?;
    if params.get("name").and_then(JsonValue::as_str) != Some("coord_post_finding") {
        return None;
    }
    let args = params.get("arguments")?.as_object()?;
    for required in ["title", "body"] {
        match args.get(required).and_then(JsonValue::as_str) {
            Some(s) if !s.trim().is_empty() => {}
            _ => return None,
        }
    }
    Some(JsonValue::Object(args.clone()))
}

/// The body a forwarder returns when it spooled instead of delivering.
///
/// Deliberately NOT success-shaped. The caller asked coord to register a gate
/// or post a finding; neither happened, and reporting either as done is the
/// failure mode this whole plan exists to close. The response therefore keeps
/// its non-2xx status and `"success": false`, and adds `"spooled": true` as the
/// affirmative half — the write is not lost, and the caller must NOT retry it
/// (a retry double-spools; the runner replays this one itself).
pub fn spooled_response_body(
    spooled: &Spooled,
    cause: &str,
    upstream_url: &str,
    coord_base_source: &str,
) -> JsonValue {
    json!({
        "success": false,
        "spooled": true,
        "error": format!(
            "coord did not accept this write ({cause}) — it was durably recorded in the \
             runner's session outbox as `{kind}` and will be replayed when coord is \
             reachable. It is NOT registered with coord yet; do not retry (that would \
             enqueue a second copy).",
            kind = spooled.kind,
        ),
        "code": "COORD_WRITE_PROXY_SPOOLED",
        "spooled_kind": spooled.kind,
        "spooled_session_id": spooled.session_id.to_string(),
        "spooled_seq": spooled.seq,
        "caller_should_retry": false,
        "cause": cause,
        "upstream_url": upstream_url,
        "coord_base_source": coord_base_source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn spool() -> (CloseoutSpool, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        (CloseoutSpool::new(outbox, Uuid::new_v4()), dir)
    }

    #[test]
    fn classify_splits_4xx_from_everything_else() {
        for permanent in [400, 401, 403, 404, 409, 422, 429, 499] {
            assert_eq!(
                classify_coord_write_status(permanent),
                CoordWriteClass::Permanent,
                "{permanent} must not be spooled — coord answered on the content"
            );
        }
        for spoolable in [500, 502, 503, 504, 599] {
            assert_eq!(
                classify_coord_write_status(spoolable),
                CoordWriteClass::Spoolable,
                "{spoolable} is the retryable class"
            );
        }
    }

    #[test]
    fn gate_spool_round_trips_to_the_phase_2_payload_shape() {
        let (spool, _dir) = spool();
        let input = parse_gate_registration_body(
            br#"{
                "predicate": {"kind": "pr_merged", "repo": "qontinui/qontinui-runner",
                              "number": 1},
                "phase_name": "Phase 3",
                "clearance_audience": "operator",
                "work_unit_upsert": {"title": "Closeout has no durable store",
                                     "status": "in_progress"}
            }"#,
        )
        .expect("a complete register-gate body is spoolable");
        assert!(input.hint_present);
        let out = spool
            .spool_gate_registration(
                "2026-08-28-closeout-store",
                &input.body,
                input.work_unit_upsert,
            )
            .unwrap();
        assert_eq!(out.kind, "gate_registration");

        let pending = spool.outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let row = &pending[0];
        assert_eq!(row.event_kind, SessionEventKind::GateRegistration.as_str());
        assert_eq!(row.session_id, out.session_id);
        assert_eq!(row.seq, out.seq);
        // The slug rides IN the payload: it is a PATH segment for the drain.
        assert_eq!(
            row.payload["work_unit_slug"],
            json!("2026-08-28-closeout-store")
        );
        assert_eq!(row.payload["phase_name"], json!("Phase 3"));
        assert_eq!(row.payload["predicate"]["kind"], json!("pr_merged"));
        assert_eq!(row.payload["clearance_audience"], json!("operator"));
        // The bootstrap survives, so Phase 2's lazy 404 recovery can fire.
        assert_eq!(
            row.payload["work_unit_upsert"]["title"],
            json!("Closeout has no durable store")
        );
    }

    #[test]
    fn gate_lane_is_per_work_unit_and_deterministic() {
        assert_eq!(gate_lane("a-unit"), gate_lane("a-unit"));
        assert_ne!(gate_lane("a-unit"), gate_lane("b-unit"));
    }

    #[test]
    fn gate_body_without_the_fields_coord_requires_is_not_spoolable() {
        // No `phase_name` — a guaranteed 422 on replay.
        assert!(
            parse_gate_registration_body(br#"{"predicate": {"kind": "unit_ready"}}"#).is_none()
        );
        // No `predicate`.
        assert!(parse_gate_registration_body(br#"{"phase_name": "Phase 3"}"#).is_none());
        // Not an object at all.
        assert!(parse_gate_registration_body(br#"["predicate"]"#).is_none());
        assert!(parse_gate_registration_body(b"not json").is_none());
    }

    #[test]
    fn gate_body_upsert_hint_is_lifted_out_never_left_in_the_forwarded_body() {
        let input = parse_gate_registration_body(
            br#"{"predicate": {"kind": "unit_ready"}, "phase_name": "P1",
                 "work_unit_upsert": {"title": "T"}}"#,
        )
        .unwrap();
        assert!(
            !input.body.contains_key(WORK_UNIT_UPSERT_HINT_KEY),
            "the runner-only hint must not reach coord"
        );
        assert!(input.hint_present);
        assert_eq!(input.work_unit_upsert.unwrap()["title"], json!("T"));

        // A non-object hint is discarded rather than spooled as a bootstrap
        // coord would refuse — but it is still `hint_present`, so the
        // forwarder re-serializes and the junk key never reaches coord.
        let input = parse_gate_registration_body(
            br#"{"predicate": {"kind": "unit_ready"}, "phase_name": "P1",
                 "work_unit_upsert": "a title"}"#,
        )
        .unwrap();
        assert!(input.work_unit_upsert.is_none());
        assert!(input.hint_present);
        assert!(!input.body.contains_key(WORK_UNIT_UPSERT_HINT_KEY));

        // No hint at all: the forwarder keeps the caller's ORIGINAL bytes.
        let input = parse_gate_registration_body(
            br#"{"predicate": {"kind": "unit_ready"}, "phase_name": "P1"}"#,
        )
        .unwrap();
        assert!(!input.hint_present);
    }

    #[test]
    fn finding_spool_writes_the_arguments_object_verbatim() {
        let (spool, _dir) = spool();
        let args = parse_post_finding_arguments(
            br#"{"jsonrpc":"2.0","id":7,"method":"tools/call",
                 "params":{"name":"coord_post_finding","arguments":{
                     "title":"register-gate has no idempotency arm",
                     "body":"register_gate_core does no duplicate detection.",
                     "kind":"gotcha",
                     "resource_keys":["qontinui-coord/crates/coord/src/gates.rs"]}}}"#,
        )
        .expect("a complete coord_post_finding call is spoolable");
        let out = spool.spool_finding(&args).unwrap();
        assert_eq!(out.kind, "finding_posted");

        let pending = spool.outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let row = &pending[0];
        assert_eq!(row.event_kind, SessionEventKind::FindingPosted.as_str());
        assert_eq!(row.payload, args);
        // Nothing this module invents: coord's PostFindingBody is
        // deny_unknown_fields and rejects the identity fields BY NAME.
        for forbidden in ["tenant_id", "author_session", "author_device"] {
            assert!(row.payload.get(forbidden).is_none());
        }
    }

    #[test]
    fn only_a_complete_coord_post_finding_tools_call_is_spoolable() {
        // A different tool.
        assert!(parse_post_finding_arguments(
            br#"{"method":"tools/call","params":{"name":"coord_orient","arguments":{}}}"#
        )
        .is_none());
        // A different method.
        assert!(parse_post_finding_arguments(
            br#"{"method":"tools/list","params":{"name":"coord_post_finding",
                 "arguments":{"title":"t","body":"b"}}}"#
        )
        .is_none());
        // Missing `body` — a guaranteed 400 on replay.
        assert!(parse_post_finding_arguments(
            br#"{"method":"tools/call","params":{"name":"coord_post_finding",
                 "arguments":{"title":"t"}}}"#
        )
        .is_none());
        // Blank `title`.
        assert!(parse_post_finding_arguments(
            br#"{"method":"tools/call","params":{"name":"coord_post_finding",
                 "arguments":{"title":"   ","body":"b"}}}"#
        )
        .is_none());
        // Not JSON.
        assert!(parse_post_finding_arguments(b"<html/>").is_none());
    }

    #[test]
    fn spooled_response_is_never_success_shaped() {
        let (spool, _dir) = spool();
        let out = spool
            .spool_finding(&json!({"title": "t", "body": "b"}))
            .unwrap();
        let v = spooled_response_body(&out, "coord unreachable: connection refused", "u", "env");
        assert_eq!(v["success"], json!(false));
        assert_eq!(v["spooled"], json!(true));
        assert_eq!(v["code"], json!("COORD_WRITE_PROXY_SPOOLED"));
        assert_eq!(v["spooled_kind"], json!("finding_posted"));
        assert_eq!(v["caller_should_retry"], json!(false));
        // No field a caller could mistake for a coord-side identity.
        assert!(v.get("gate_id").is_none());
        assert!(v.get("finding_id").is_none());
        assert!(v["error"].as_str().unwrap().contains("NOT registered"));
    }

    /// Two spools over ONE outbox `Arc` share the seq lane. The counterpart —
    /// two `OutboxWriter::open` calls on one path — is the correctness bug
    /// `install` exists to prevent, and this pins the property the fix relies
    /// on.
    #[test]
    fn one_outbox_arc_keeps_seqs_monotonic_across_spool_handles() {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        let machine_id = Uuid::new_v4();
        let a = CloseoutSpool::new(outbox.clone(), machine_id);
        let b = CloseoutSpool::new(outbox.clone(), machine_id);
        let first = a
            .spool_finding(&json!({"title": "t", "body": "b"}))
            .unwrap();
        let second = b
            .spool_finding(&json!({"title": "t2", "body": "b2"}))
            .unwrap();
        assert_eq!(first.session_id, second.session_id);
        assert_eq!(second.seq, first.seq + 1);
        assert_eq!(outbox.pending().unwrap().len(), 2);
    }
}
