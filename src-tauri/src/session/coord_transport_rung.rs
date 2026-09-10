//! Per-call durable record of WHICH transport rung carried a coord call — plan
//! `2026-09-07-no-per-session-record-of-which-transport-rung-carried-a-coord-read`,
//! Phase 1 (Option 1).
//!
//! ## Why this exists
//!
//! The fleet's door cascade (`/policy`, `/gate`, `/coord-revive`, the runner's
//! own `/coord-mcp` proxy) has always been able to SAY which rung it used, in
//! prose, in one session's scrollback. Nothing kept that. So
//! `success_metric/coord-mcp-first-rung-reachability` has `baseline: null` and
//! cannot be computed: there is no population to count first-rung hits out of.
//!
//! This module is the durable half. The runner's `/coord-mcp` proxy handler
//! ([`crate::mcp_api`]) reads the caller's self-declared transport headers and
//! writes ONE `coord.session_events` row per proxied call, through the session
//! outbox the `CoordSync` drain already carries to coord.
//!
//! ## The declared transport is ADVISORY — the session id is not
//!
//! Same posture the proxy already takes on `x-coord-caller-session` (see
//! `coord_mcp_proxy_handler`'s forwarding loop): a header the CLIENT sets is a
//! CLAIM. A caller can name any rung it likes, including one it did not use.
//! What the runner OBSERVED — that the call arrived at the loopback proxy door,
//! and which session's nonce carried it — is authoritative, and is exactly what
//! the row's `session_id` and lane record.
//!
//! Two consequences, both deliberate:
//!
//! - Every declared value is validated against the CLOSED vocabulary
//!   [`TRANSPORT_RUNGS`] and anything unrecognised becomes
//!   [`TRANSPORT_UNKNOWN`]. A caller string never reaches
//!   `coord.session_events` verbatim.
//! - The four declaration headers are STRIPPED from the upstream forward
//!   (`coord_mcp_forward_header_is_dropped`), exactly as the caller-session
//!   header is, so a client-supplied claim never reaches coord as if coord had
//!   observed it.
//!
//! ## Untagged is a VISIBLE ARM, not a gap
//!
//! A caller that declares nothing still gets a row —
//! `transport: "unknown", reporter: "untagged"`. That is the whole point: the
//! metric's denominator has to include the calls nobody tagged, or "first rung
//! reachability" silently becomes "reachability among callers that already
//! cooperate". Never skip the emit for an untagged caller.
//!
//! ## Telemetry must never fail or slow the call it observes
//!
//! Every entry point here is best-effort and infallible from the caller's side:
//! no lane session id, no installed emitter, or an outbox write error all log
//! and return. A proxied coord call must never become an error because its
//! observation could not be recorded.

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use super::local_store::OutboxWriter;
use super::SessionEventKind;

// ---------------------------------------------------------------------------
// The wire vocabulary — ONE place, so Option 3's producers spell it the same
// ---------------------------------------------------------------------------

/// Payload schema version. Bump only for a breaking reshape; additive fields
/// do not need one.
pub const PAYLOAD_VERSION: u64 = 1;

/// Header a caller declares its transport rung under. Case-insensitive on the
/// wire (`http::HeaderMap` lowercases names), spelled lowercase here so it can
/// be compared against `HeaderName::as_str()` directly.
pub const TRANSPORT_HEADER: &str = "x-qontinui-transport";
/// Header a caller declares WHICH door skill/component it is.
pub const REPORTER_HEADER: &str = "x-qontinui-reporter";
/// Header a caller declares its skill-local step ordinal under (optional).
pub const REPORTER_STEP_HEADER: &str = "x-qontinui-reporter-step";
/// Header a caller declares the rungs it already tried and was refused by,
/// BEFORE this one — comma-separated, each value from [`TRANSPORT_RUNGS`].
pub const ATTEMPTED_HEADER: &str = "x-qontinui-transport-attempted";

/// Every declaration header, in one slice — the set
/// `coord_mcp_forward_header_is_dropped` strips from the upstream forward, and
/// the set a producer may set. Adding a header here and nowhere else is a bug:
/// the drop list reads THIS slice.
pub const DECLARATION_HEADERS: &[&str] = &[
    TRANSPORT_HEADER,
    REPORTER_HEADER,
    REPORTER_STEP_HEADER,
    ATTEMPTED_HEADER,
];

/// Is `name` one of this module's declaration headers?
///
/// `name` is expected already lowercased (`HeaderName::as_str()` guarantees
/// it); compared case-insensitively anyway so a hand-built call site cannot be
/// subtly wrong.
pub fn is_declaration_header(name: &str) -> bool {
    DECLARATION_HEADERS
        .iter()
        .any(|h| name.eq_ignore_ascii_case(h))
}

/// The transport rung a value could not be resolved to. Both "the caller
/// declared nothing" and "the caller declared something outside the closed
/// vocabulary" land here — they are the same fact for the metric (this call's
/// rung is not known), and keeping a raw caller string out of
/// `coord.session_events` matters more than telling them apart.
pub const TRANSPORT_UNKNOWN: &str = "unknown";

/// The rung meaning "this call did not go through the cascade at all".
/// Recorded as its own value AND as the derived `off_cascade` boolean.
pub const TRANSPORT_OFF_CASCADE: &str = "off_cascade";

/// THE closed transport vocabulary. A declared value outside this set is mapped
/// to [`TRANSPORT_UNKNOWN`] — never passed through raw.
///
/// MUST stay sorted: membership is a `binary_search`.
pub const TRANSPORT_RUNGS: &[&str] = &[
    "bootstrap_credential",
    "file_mirror",
    "http_agent_door",
    "loopback_proxy",
    "native_mcp",
    "off_cascade",
    "remote_mcp",
    "steering_cache",
    "unknown",
    "unreached",
    "write_forwarder",
];

/// The reporter a call gets when it declared none. Distinct from
/// [`TRANSPORT_UNKNOWN`]: "nobody tagged this call" and "this caller tagged it
/// with something we do not model" are different producer-side facts, and the
/// first is the one Option 3 is meant to shrink.
pub const REPORTER_UNTAGGED: &str = "untagged";

/// THE closed reporter vocabulary — which door component made the call.
///
/// MUST stay sorted: membership is a `binary_search`.
pub const REPORTERS: &[&str] = &[
    "coord",
    "coord-revive",
    "gate",
    "mcp-client",
    "policy",
    "runner-proxy",
];

/// Outcome: the declared rung carried the call to this door.
pub const OUTCOME_OK: &str = "ok";
/// Outcome: the rung answered and refused.
pub const OUTCOME_REFUSED: &str = "refused";
/// Outcome: the rung did not answer in time.
pub const OUTCOME_TIMEOUT: &str = "timeout";
/// Outcome: the rung's tool was not visible to the caller at all.
pub const OUTCOME_MASKED: &str = "masked";
/// Outcome: the rung's transport was dead (the "Command failed with no output"
/// class `/coord-revive` exists for).
pub const OUTCOME_DISCONNECTED: &str = "disconnected";

/// THE closed outcome vocabulary.
///
/// Phase 1's producer only ever emits [`OUTCOME_OK`] — it writes its row before
/// the upstream hop, so the only outcome it has OBSERVED is "this rung carried
/// the call to the door". The other four are declared here, not later, because
/// this slice is what a second producer (a `/policy` or `/coord-revive` rung
/// that reports its OWN refusal) must spell against, and a vocabulary invented
/// twice is a vocabulary that forks.
pub const OUTCOMES: &[&str] = &[
    OUTCOME_OK,
    OUTCOME_REFUSED,
    OUTCOME_TIMEOUT,
    OUTCOME_MASKED,
    OUTCOME_DISCONNECTED,
];

/// Operation class: this call reads coord state.
pub const OPERATION_READ: &str = "read";
/// Operation class: this call mutates coord state.
pub const OPERATION_WRITE: &str = "write";

/// THE closed operation vocabulary.
pub const OPERATIONS: &[&str] = &[OPERATION_READ, OPERATION_WRITE];

/// Longest `reporter_step` accepted. It is a skill-local ordinal ("2", "6b"),
/// not free text — bounded and charset-locked so a caller cannot use it as a
/// smuggling channel into `coord.session_events`.
const MAX_REPORTER_STEP_LEN: usize = 16;

/// Most `attempted` entries kept. The cascade is 5 rungs deep today; 8 leaves
/// room without letting a caller inflate a row.
const MAX_ATTEMPTED: usize = 8;

/// Resolve a caller-declared transport to the closed vocabulary.
///
/// `None` (no header) and any unrecognised value both yield
/// [`TRANSPORT_UNKNOWN`]. The returned `&'static str` is a slice of
/// [`TRANSPORT_RUNGS`], never of the caller's input — which is the property
/// that keeps caller bytes out of the payload.
pub fn parse_transport(raw: Option<&str>) -> &'static str {
    let Some(raw) = raw else {
        return TRANSPORT_UNKNOWN;
    };
    let trimmed = raw.trim().to_ascii_lowercase();
    match TRANSPORT_RUNGS.binary_search(&trimmed.as_str()) {
        Ok(i) => TRANSPORT_RUNGS[i],
        Err(_) => TRANSPORT_UNKNOWN,
    }
}

/// Resolve a caller-declared reporter to the closed vocabulary.
///
/// `None` ⇒ [`REPORTER_UNTAGGED`] (nobody tagged the call); an unrecognised
/// value ⇒ [`TRANSPORT_UNKNOWN`]'s spelling, `"unknown"` (a caller tagged it
/// with something not modelled). Both are visible arms, never a skipped emit.
pub fn parse_reporter(raw: Option<&str>) -> &'static str {
    let Some(raw) = raw else {
        return REPORTER_UNTAGGED;
    };
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return REPORTER_UNTAGGED;
    }
    match REPORTERS.binary_search(&trimmed.as_str()) {
        Ok(i) => REPORTERS[i],
        Err(_) => TRANSPORT_UNKNOWN,
    }
}

/// Parse the comma-separated `attempted` declaration into closed-vocabulary
/// rungs, in caller order, deduplicated and bounded by [`MAX_ATTEMPTED`].
///
/// Unrecognised entries are DROPPED rather than mapped to `"unknown"`: a list
/// of `["unknown", "unknown"]` says nothing, where an empty list honestly says
/// "no recognised prior rung was declared".
pub fn parse_attempted(raw: Option<&str>) -> Vec<&'static str> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut out: Vec<&'static str> = Vec::new();
    for part in raw.split(',') {
        if out.len() >= MAX_ATTEMPTED {
            break;
        }
        let trimmed = part.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(i) = TRANSPORT_RUNGS.binary_search(&trimmed.as_str()) {
            let v = TRANSPORT_RUNGS[i];
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

/// Bound and charset-lock a caller-declared `reporter_step`.
///
/// Kept only when it is 1..=[`MAX_REPORTER_STEP_LEN`] chars of
/// `[A-Za-z0-9._-]`; anything else becomes `None`. Same reasoning as coord's
/// own `validate_event_kind`: a value that lands in a durable store, unbounded
/// and uncharset-checked, is a smuggling channel.
pub fn parse_reporter_step(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_REPORTER_STEP_LEN {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    {
        return None;
    }
    Some(trimmed.to_string())
}

// ---------------------------------------------------------------------------
// read vs write
// ---------------------------------------------------------------------------

/// Coord MCP tools that READ, matched exactly. Everything not matched here or
/// by [`READ_TOOL_PREFIXES`] is reported as a WRITE — the conservative default,
/// because mislabelling a write as a read would overstate how much of the
/// fleet's coord traffic is the safely-retryable kind.
///
/// MUST stay sorted: membership is a `binary_search`.
const READ_TOOLS: &[&str] = &[
    "coord_agent_registry_effective",
    "coord_am_i_clear",
    "coord_blockers",
    "coord_build_info",
    "coord_can",
    "coord_change_conflict",
    "coord_conflict_check",
    "coord_diagnose",
    "coord_diff_impact",
    "coord_inbox",
    "coord_layering_triage",
    "coord_memory_overview",
    "coord_memory_search",
    "coord_merge_order",
    "coord_migration_queue",
    "coord_orient",
    "coord_pr_status",
    "coord_reevaluate_dry",
    "coord_session_worktrees",
    "coord_who_is_working_on",
];

/// Read-only tool FAMILIES by prefix.
///
/// `coord_gate_` is a read family in full (`coord_gate_doctor` / `_inspect` /
/// `_list` / `_status`); every gate WRITE is spelled the other way round
/// (`coord_register_gate`, `coord_attest_gate`, `coord_mute_gate`, …), so the
/// prefix cannot swallow one. `coord_can` is deliberately in [`READ_TOOLS`] as
/// an EXACT match rather than here — as a prefix it would also claim
/// `coord_cancel_continuation`, which is a write.
const READ_TOOL_PREFIXES: &[&str] = &[
    "coord_check_",
    "coord_edit_predict",
    "coord_explain_",
    "coord_find_",
    "coord_fixer_arm_",
    "coord_gate_",
    "coord_get_",
    "coord_is_",
    "coord_list_",
    "coord_predict_",
    "coord_query_",
    "coord_recent_",
    "coord_work_unit_list",
];

/// Classify one JSON-RPC call as a coord read or a coord write.
///
/// Everything that is not a `tools/call` (`initialize`, `tools/list`, `ping`,
/// the notification family) is a read: none of them mutates coord state.
///
/// ## This list can drift, and drifts SAFELY
///
/// Coord's tool surface moves; nothing here is generated from it. A newly added
/// read tool that matches neither table is reported as a `write`, which
/// understates reads rather than inventing them. That is the direction the
/// metric can survive: `operation` is a facet on the rung record, not the
/// record's reason for existing.
pub fn operation_for_call(method: &str, tool: Option<&str>) -> &'static str {
    if method != "tools/call" {
        return OPERATION_READ;
    }
    let Some(tool) = tool else {
        return OPERATION_WRITE;
    };
    if READ_TOOLS.binary_search(&tool).is_ok() {
        return OPERATION_READ;
    }
    if READ_TOOL_PREFIXES.iter().any(|p| tool.starts_with(p)) {
        return OPERATION_READ;
    }
    OPERATION_WRITE
}

/// Classify a raw JSON-RPC request body.
///
/// A batch is classified by its STRONGEST member: one write in the batch makes
/// the whole hop a write. An unparseable body (the body gate rejects those
/// before this is ever reached on the proxy path) is a write, by the same
/// conservative default as [`operation_for_call`].
pub fn operation_for_body(body: &[u8]) -> &'static str {
    let Ok(parsed) = serde_json::from_slice::<JsonValue>(body) else {
        return OPERATION_WRITE;
    };
    let one = |req: &JsonValue| -> &'static str {
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let tool = req.pointer("/params/name").and_then(|n| n.as_str());
        operation_for_call(method, tool)
    };
    match &parsed {
        JsonValue::Array(elems) => {
            if elems.iter().map(one).any(|o| o == OPERATION_WRITE) {
                OPERATION_WRITE
            } else {
                OPERATION_READ
            }
        }
        _ => one(&parsed),
    }
}

// ---------------------------------------------------------------------------
// The observation + its payload
// ---------------------------------------------------------------------------

/// One transport-rung observation, already reduced to closed-vocabulary values.
///
/// Build it with [`RungObservation::from_declaration`] so the validation cannot
/// be skipped by a call site that constructs the struct by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RungObservation {
    /// CALLER-DECLARED, validated against [`TRANSPORT_RUNGS`]. Advisory.
    pub transport: &'static str,
    /// RUNNER-OBSERVED. What this door saw happen to the call.
    pub outcome: &'static str,
    /// The coord URL this door actually dialed.
    pub door: String,
    /// CALLER-DECLARED, validated against [`REPORTERS`]. Advisory.
    pub reporter: &'static str,
    /// CALLER-DECLARED skill-local ordinal. Advisory, bounded, charset-locked.
    pub reporter_step: Option<String>,
    /// CALLER-DECLARED rungs refused before this one. Advisory.
    pub attempted: Vec<&'static str>,
    /// RUNNER-OBSERVED failure cause, when `outcome` is not
    /// [`OUTCOME_OK`]. Reserved: the proxy emits its row BEFORE the upstream
    /// forward (so the row exists even if the runner dies mid-hop), and at that
    /// point the only observed fact is that this rung carried the call to the
    /// door. A later option that emits a second, outcome-bearing row fills it.
    pub failure_reason: Option<String>,
    /// RUNNER-OBSERVED from the JSON-RPC body — see [`operation_for_body`].
    pub operation: &'static str,
    /// RUNNER-OBSERVED coord `agent_sessions.id` of the caller, when the
    /// proxy's own self-id chain resolved one.
    ///
    /// NOT a duplicate of the row's `session_id`: that is a
    /// `coord.sessions.id` (the id `coord.session_events.session_id`
    /// references), and this is the durable agent-session anchor. Different id
    /// spaces, both useful, so both are recorded.
    pub agent_session_id: Option<Uuid>,
}

impl RungObservation {
    /// Reduce four raw caller-declared header values (any or all absent) plus
    /// the runner's own observations to a validated observation.
    ///
    /// An untagged caller is a first-class result here, not a `None`:
    /// `transport = "unknown"`, `reporter = "untagged"`.
    #[allow(clippy::too_many_arguments)]
    pub fn from_declaration(
        transport: Option<&str>,
        reporter: Option<&str>,
        reporter_step: Option<&str>,
        attempted: Option<&str>,
        outcome: &'static str,
        door: impl Into<String>,
        operation: &'static str,
        agent_session_id: Option<Uuid>,
    ) -> Self {
        // The two RUNNER-OBSERVED enums are `&'static str` rather than Rust
        // enums so the payload builder stays allocation-free, which leaves this
        // as the one place that can catch a call site inventing a value the
        // consumer does not model. Debug-only: a bad string is a bug to fix in
        // development, never a reason to fail a live proxied coord call.
        debug_assert!(
            OUTCOMES.contains(&outcome),
            "outcome {outcome:?} is outside the closed vocabulary"
        );
        debug_assert!(
            OPERATIONS.contains(&operation),
            "operation {operation:?} is outside the closed vocabulary"
        );
        Self {
            transport: parse_transport(transport),
            outcome,
            door: door.into(),
            reporter: parse_reporter(reporter),
            reporter_step: parse_reporter_step(reporter_step),
            attempted: parse_attempted(attempted),
            failure_reason: None,
            operation,
            agent_session_id,
        }
    }

    /// Whether this call bypassed the cascade entirely — derived from the
    /// declared rung, never separately declared, so the two cannot disagree.
    pub fn off_cascade(&self) -> bool {
        self.transport == TRANSPORT_OFF_CASCADE
    }

    /// The v1 wire payload.
    ///
    /// `session_id` and `occurred_at` are deliberately ABSENT: they are the
    /// `coord.session_events` row's own columns (coord stamps `occurred_at`
    /// server-side), and duplicating them into the payload would create a
    /// second, divergeable copy. `at` is the CLIENT clock — kept because the
    /// outbox can replay a row hours after the call, and the skew between the
    /// two is itself the evidence that happened.
    pub fn payload(&self) -> JsonValue {
        json!({
            "v": PAYLOAD_VERSION,
            "transport": self.transport,
            "outcome": self.outcome,
            "door": self.door,
            "reporter": self.reporter,
            "reporter_step": self.reporter_step,
            "attempted": self.attempted,
            "failure_reason": self.failure_reason,
            "operation": self.operation,
            "off_cascade": self.off_cascade(),
            "agent_session_id": self.agent_session_id.map(|id| id.to_string()),
            // `Z`-suffixed millisecond UTC, not the default `+00:00` offset
            // form — the same spelling every other qontinui timestamp uses, so
            // a consumer needs one parser rather than two.
            "at": chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        })
    }
}

// ---------------------------------------------------------------------------
// The emitter
// ---------------------------------------------------------------------------

/// Writes [`SessionEventKind::CoordTransportRung`] rows into the SAME
/// [`OutboxWriter`] the `CoordSync` drain reads.
///
/// It must be the same `Arc` for the reason
/// [`super::closeout_spool::CloseoutSpool`] documents at length: two
/// `OutboxWriter`s over one file each keep their own per-`(machine_id,
/// session_id)` seq counter and append cursor, so a second one mints colliding
/// seqs and interleaves compaction rewrites. [`install`] takes the handle
/// `main.rs` already clones for every other producer.
pub struct RungEmitter {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
}

impl RungEmitter {
    pub fn new(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> Self {
        Self { outbox, machine_id }
    }

    /// Record one observation under `lane_session_id`.
    ///
    /// `lane_session_id` MUST be a `coord.sessions.id` — that is the column
    /// `coord.session_events.session_id` references, and coord's
    /// `POST /sessions/:id/events` 404s on anything else.
    ///
    /// Infallible from the caller's side: an outbox error is logged and
    /// swallowed. This is telemetry attached to a live proxied coord call, and
    /// failing that call because its observation could not be written would be
    /// strictly worse than losing the observation.
    pub fn emit(&self, lane_session_id: Uuid, obs: &RungObservation) {
        if let Err(e) = self.outbox.record(
            self.machine_id,
            lane_session_id,
            SessionEventKind::CoordTransportRung,
            obs.payload(),
        ) {
            tracing::debug!(
                session = %lane_session_id,
                transport = obs.transport,
                "coord_transport_rung: outbox write failed ({e}) — observation dropped"
            );
        }
    }
}

static GLOBAL: OnceLock<Arc<RungEmitter>> = OnceLock::new();

/// Install the process-wide emitter. `false` when one was already installed
/// (the first handle is kept — see [`RungEmitter`] on why a second
/// `OutboxWriter` over one file is a correctness bug, not just duplication).
///
/// A process global rather than `ApiState` for the same reason the closeout
/// spool is one: the proxy handler that needs it already carries `State`, but
/// the emitter is also reachable from paths that do not, and one install site
/// keeps the "same `Arc`" invariant checkable in one place.
pub fn install(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> bool {
    GLOBAL
        .set(Arc::new(RungEmitter::new(outbox, machine_id)))
        .is_ok()
}

/// The installed emitter, if `main.rs` got far enough to install one. `None` in
/// unit tests and in a runner whose session subsystem never came up — callers
/// treat that as "no observation recorded", never as an error.
pub fn global() -> Option<Arc<RungEmitter>> {
    GLOBAL.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_vocabulary_is_closed_and_sorted() {
        let mut sorted = TRANSPORT_RUNGS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted.as_slice(), TRANSPORT_RUNGS, "TRANSPORT_RUNGS sorted");
        let mut sorted = REPORTERS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted.as_slice(), REPORTERS, "REPORTERS sorted");
        let mut sorted = READ_TOOLS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted.as_slice(), READ_TOOLS, "READ_TOOLS sorted");
        // The eleven rungs the plan names. Spelled out so widening the
        // vocabulary is a deliberate edit here, not a side effect elsewhere.
        assert_eq!(TRANSPORT_RUNGS.len(), 11);
        // The outcome/operation vocabularies the plan names, pinned so a
        // consumer's `IN (...)` list cannot silently fall behind.
        assert_eq!(
            OUTCOMES,
            ["ok", "refused", "timeout", "masked", "disconnected"]
        );
        assert_eq!(OPERATIONS, ["read", "write"]);
        for want in [
            "native_mcp",
            "loopback_proxy",
            "remote_mcp",
            "write_forwarder",
            "http_agent_door",
            "bootstrap_credential",
            "steering_cache",
            "file_mirror",
            "off_cascade",
            "unreached",
            "unknown",
        ] {
            assert!(
                TRANSPORT_RUNGS.contains(&want),
                "{want} missing from the closed vocabulary"
            );
        }
    }

    #[test]
    fn parse_transport_maps_every_unrecognised_value_to_unknown() {
        // Recognised values round-trip, case- and whitespace-insensitively.
        assert_eq!(parse_transport(Some("loopback_proxy")), "loopback_proxy");
        assert_eq!(parse_transport(Some("  Native_MCP ")), "native_mcp");
        assert_eq!(parse_transport(Some("off_cascade")), "off_cascade");
        // Absent — the untagged arm.
        assert_eq!(parse_transport(None), TRANSPORT_UNKNOWN);
        // Unrecognised, empty, and a hostile string all collapse to "unknown"
        // and NEVER pass the caller's bytes through.
        for hostile in ["", "   ", "loopback-proxy", "carrier_pigeon", "a.b.*>"] {
            let got = parse_transport(Some(hostile));
            assert_eq!(got, TRANSPORT_UNKNOWN, "{hostile:?} must map to unknown");
            assert!(
                TRANSPORT_RUNGS.contains(&got),
                "the returned value is always from the closed set"
            );
        }
    }

    #[test]
    fn untagged_caller_is_a_visible_arm() {
        let obs = RungObservation::from_declaration(
            None,
            None,
            None,
            None,
            OUTCOME_OK,
            "https://coord.qontinui.io/mcp",
            OPERATION_READ,
            None,
        );
        assert_eq!(obs.transport, TRANSPORT_UNKNOWN);
        assert_eq!(obs.reporter, REPORTER_UNTAGGED);
        let p = obs.payload();
        assert_eq!(p["v"], json!(PAYLOAD_VERSION));
        assert_eq!(p["transport"], json!("unknown"));
        assert_eq!(p["reporter"], json!("untagged"));
        assert_eq!(p["off_cascade"], json!(false));
        assert_eq!(p["attempted"], json!([]));
        assert!(p["at"].as_str().is_some(), "client clock stamped");
        // The row's own columns must NOT be duplicated into the payload.
        assert!(p.get("session_id").is_none());
        assert!(p.get("occurred_at").is_none());
    }

    #[test]
    fn reporter_step_and_attempted_are_bounded_and_vocabulary_checked() {
        assert_eq!(parse_reporter_step(Some(" 6b ")).as_deref(), Some("6b"));
        assert_eq!(parse_reporter_step(Some("")), None);
        assert_eq!(parse_reporter_step(Some("step two")), None);
        assert_eq!(parse_reporter_step(Some(&"9".repeat(17))), None);

        assert_eq!(
            parse_attempted(Some("native_mcp, loopback_proxy , native_mcp, nonsense")),
            vec!["native_mcp", "loopback_proxy"],
            "unrecognised entries dropped, order kept, deduplicated"
        );
        assert!(parse_attempted(None).is_empty());
    }

    #[test]
    fn off_cascade_is_derived_from_the_declared_rung() {
        let obs = RungObservation::from_declaration(
            Some("off_cascade"),
            Some("policy"),
            None,
            None,
            OUTCOME_OK,
            "d",
            OPERATION_READ,
            None,
        );
        assert!(obs.off_cascade());
        assert_eq!(obs.payload()["off_cascade"], json!(true));
    }

    #[test]
    fn operation_classifies_reads_and_defaults_writes() {
        assert_eq!(operation_for_call("tools/list", None), OPERATION_READ);
        assert_eq!(operation_for_call("initialize", None), OPERATION_READ);
        assert_eq!(
            operation_for_call("tools/call", Some("coord_query_health")),
            OPERATION_READ
        );
        assert_eq!(
            operation_for_call("tools/call", Some("coord_gate_list")),
            OPERATION_READ
        );
        assert_eq!(
            operation_for_call("tools/call", Some("coord_can")),
            OPERATION_READ
        );
        // `coord_can` is exact, so the cancel WRITE is not swallowed by it.
        assert_eq!(
            operation_for_call("tools/call", Some("coord_cancel_continuation")),
            OPERATION_WRITE
        );
        assert_eq!(
            operation_for_call("tools/call", Some("coord_register_gate")),
            OPERATION_WRITE
        );
        // Unknown tool → write, the conservative default.
        assert_eq!(
            operation_for_call("tools/call", Some("coord_brand_new_thing")),
            OPERATION_WRITE
        );

        let read_body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"coord_query_health"}}"#;
        assert_eq!(operation_for_body(read_body), OPERATION_READ);
        let batch = br#"[{"method":"tools/call","params":{"name":"coord_query_health"}},{"method":"tools/call","params":{"name":"coord_register_gate"}}]"#;
        assert_eq!(
            operation_for_body(batch),
            OPERATION_WRITE,
            "a batch is as strong as its strongest member"
        );
        assert_eq!(operation_for_body(b"not json"), OPERATION_WRITE);
    }

    #[test]
    fn declaration_headers_are_recognised_case_insensitively() {
        for h in DECLARATION_HEADERS {
            assert!(is_declaration_header(h));
            assert!(is_declaration_header(&h.to_ascii_uppercase()));
        }
        assert!(!is_declaration_header("authorization"));
        assert!(!is_declaration_header("x-qontinui-transportation"));
    }

    #[test]
    fn emit_writes_one_outbox_row_of_the_new_kind() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Arc::new(
            OutboxWriter::open(dir.path().join("session-outbox.jsonl")).expect("outbox opens"),
        );
        let machine_id = Uuid::new_v4();
        let lane = Uuid::new_v4();
        let emitter = RungEmitter::new(outbox.clone(), machine_id);
        let obs = RungObservation::from_declaration(
            Some("loopback_proxy"),
            Some("policy"),
            Some("2"),
            Some("native_mcp"),
            OUTCOME_OK,
            "https://coord.qontinui.io/mcp",
            OPERATION_READ,
            Some(Uuid::new_v4()),
        );
        emitter.emit(lane, &obs);

        let pending = outbox.pending().expect("pending readable");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_kind, "coord-transport-rung");
        assert_eq!(pending[0].session_id, lane);
        assert_eq!(pending[0].payload["transport"], json!("loopback_proxy"));
        assert_eq!(pending[0].payload["reporter"], json!("policy"));
        assert_eq!(pending[0].payload["reporter_step"], json!("2"));
        assert_eq!(pending[0].payload["attempted"], json!(["native_mcp"]));
        assert_eq!(pending[0].payload["operation"], json!("read"));
    }
}
