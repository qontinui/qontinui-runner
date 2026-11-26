//! Durable spool for `spawn-failed` reports coord did not receive — plan
//! `2026-11-14-coord-spawn-admission-uses-the-runners-own-launch-ceiling`,
//! Phase 6 (D6), runner half.
//!
//! A coord launch this runner refuses (a `deferred_load:` machine-load
//! deferral, or any other non-start) is reported with `POST
//! /agents/:id/spawn-failed`. When that report never reaches coord, coord keeps
//! counting the allocation as in flight until its 7200 s horizon, which inflates
//! the device's in-flight count toward its cap and — for a load deferral — keeps
//! the dispatch dedup marker that would let the work be offered again. On
//! 2026-11-14 spaceship logged 79 deferrals "NOT delivered after 3 attempts"
//! and then simply dropped them.
//!
//! So a report still undelivered after its bounded live attempts is written to
//! the SESSION outbox as [`SessionEventKind::SpawnFailedReport`], and the
//! `CoordSync` drain replays it under the device credential — the same drain →
//! auth → retry → ack machinery [`super::closeout_spool`] reuses for the
//! closeout kinds.
//!
//! ## What is spoolable and what is not
//!
//! Only the TRANSPORT class (nothing reached coord, or coord failed
//! transiently with a 5xx). A 4xx is coord reaching a verdict on the report
//! (e.g. a 404 for an agent it does not know); spooling it would only replay a
//! guaranteed refusal. The live poster classes a 4xx `Rejected` and never hands
//! it here; the drain maps a 4xx on replay to `PermanentFailure` (Ack-drop).
//!
//! ## The lane
//!
//! The outbox keys `seq` on `(machine_id, session_id)`, and a refused launch has
//! no coord session at all. Each report therefore rides a deterministic
//! synthetic lane PER AGENT ([`spawn_report_lane`]): coord never sees that id
//! (the drain builds the URL from the payload's `agent_id`), and one agent's
//! row stuck on a coord 5xx never holds up another agent's report — the
//! per-session chain is per lane.
//!
//! ## A replayed duplicate is harmless, not a no-op
//!
//! The window where the POST succeeded but the local ACK did not land
//! delivers the same report twice. coord does not dedupe it: it re-runs the
//! status UPDATE (the allocation is already `abandoned`, so the state does not
//! move), writes a SECOND `spawn_outcome` row, and — for a `deferred_load:`
//! reason — re-runs the marker retirement, which is idempotent per agent under
//! its advisory lock. The cost is one extra outcome row, never a wrong state.

use serde_json::{Map as JsonMap, Value as JsonValue};
use uuid::Uuid;

use super::closeout_spool::{CloseoutSpool, Spooled};
use super::SessionEventKind;

/// The payload key the agent id rides under. Not part of the POST body: the
/// drain lifts it out to build `/agents/<agent_id>/spawn-failed`.
pub const AGENT_ID_KEY: &str = "agent_id";

/// The synthetic outbox lane for one agent's spawn reports. Deterministic, so a
/// restart's replay of the same agent lands in the same lane.
pub fn spawn_report_lane(agent_id: Uuid) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("spawn-failed-report:{agent_id}").as_bytes(),
    )
}

/// Spool one undelivered `spawn-failed` report.
///
/// `body` is the exact `spawn-failed` request body the live POST sent
/// (`{reason, exit_code, restarts_attempted, phase?, push_ref?, pr?}`); it is
/// written with [`AGENT_ID_KEY`] added, and replayed verbatim minus that key.
/// A `body` that is not a JSON object is refused: the drain could not replay
/// it, so keeping it would only delay its loss.
pub fn spool_spawn_failed(
    spool: &CloseoutSpool,
    agent_id: Uuid,
    body: &JsonValue,
) -> std::io::Result<Spooled> {
    let JsonValue::Object(fields) = body else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "spawn-failed body is not a JSON object",
        ));
    };
    let mut payload = fields.clone();
    payload.insert(
        AGENT_ID_KEY.to_string(),
        JsonValue::String(agent_id.to_string()),
    );
    let rec = spool.outbox().record(
        spool.machine_id(),
        spawn_report_lane(agent_id),
        SessionEventKind::SpawnFailedReport,
        JsonValue::Object(payload),
    )?;
    Ok(Spooled {
        kind: SessionEventKind::SpawnFailedReport.as_str(),
        session_id: rec.session_id,
        seq: rec.seq,
    })
}

/// What the drain replays from a spooled row: the agent and the POST body.
///
/// `None` when the payload carries no parseable `agent_id` — such a row can
/// never be delivered, and the drain fails it permanently rather than burning
/// retries on a URL it cannot build.
pub fn replay_target(payload: &JsonValue) -> Option<(Uuid, JsonValue)> {
    let fields = payload.as_object()?;
    let agent_id = fields
        .get(AGENT_ID_KEY)
        .and_then(JsonValue::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())?;
    let body: JsonMap<String, JsonValue> = fields
        .iter()
        .filter(|(k, _)| k.as_str() != AGENT_ID_KEY)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Some((agent_id, JsonValue::Object(body)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::local_store::OutboxWriter;
    use serde_json::json;
    use std::sync::Arc;

    fn spool_in(dir: &std::path::Path) -> CloseoutSpool {
        let outbox = Arc::new(OutboxWriter::open(dir.join("outbox.jsonl")).unwrap());
        CloseoutSpool::new(outbox, Uuid::new_v4())
    }

    /// The spooled kind survives the outbox's on-disk encoding: written,
    /// re-read from a FRESH writer over the same file, it is still a
    /// `spawn_failed_report` row in this agent's lane whose replay target is
    /// the agent and the body that was spooled, minus nothing and plus
    /// nothing.
    ///
    /// Mutation: give the variant a different wire string in `as_str` than in
    /// its serde rename (the drain's `&str` match and the file disagree), or
    /// drop `agent_id` from the payload.
    #[test]
    fn the_spooled_kind_round_trips_the_outbox_encoding() {
        let dir = tempfile::tempdir().unwrap();
        let agent_id = Uuid::new_v4();
        let body = json!({
            "reason": "deferred_load:thread_pressure:warn:380_over_376",
            "exit_code": null,
            "restarts_attempted": 0,
            "phase": "blocked",
        });
        let spooled = {
            let spool = spool_in(dir.path());
            spool_spawn_failed(&spool, agent_id, &body).expect("spools")
        };
        assert_eq!(spooled.kind, "spawn_failed_report");
        assert_eq!(spooled.session_id, spawn_report_lane(agent_id));

        let reopened = OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap();
        let pending = reopened.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let row = &pending[0];
        assert_eq!(row.event_kind, SessionEventKind::SpawnFailedReport.as_str());
        assert_eq!(
            serde_json::to_value(SessionEventKind::SpawnFailedReport).unwrap(),
            json!(row.event_kind),
            "serde wire form and as_str must agree"
        );
        assert_eq!(
            serde_json::from_value::<SessionEventKind>(json!(row.event_kind)).unwrap(),
            SessionEventKind::SpawnFailedReport
        );
        assert_eq!(row.session_id, spawn_report_lane(agent_id));
        let (replay_agent, replay_body) = replay_target(&row.payload).expect("replayable");
        assert_eq!(replay_agent, agent_id);
        assert_eq!(replay_body, body, "the body replays verbatim");
    }

    /// One lane per agent, stable across calls, distinct between agents.
    #[test]
    fn each_agent_reports_on_its_own_deterministic_lane() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(spawn_report_lane(a), spawn_report_lane(a));
        assert_ne!(spawn_report_lane(a), spawn_report_lane(b));
        assert_ne!(spawn_report_lane(a), a, "the lane is not the agent id");
    }

    /// A payload with no usable agent id has no URL to replay to.
    #[test]
    fn a_payload_without_an_agent_id_is_not_replayable() {
        assert!(replay_target(&json!({"reason": "x"})).is_none());
        assert!(replay_target(&json!({"agent_id": "not-a-uuid", "reason": "x"})).is_none());
        assert!(replay_target(&json!("scalar")).is_none());
        let dir = tempfile::tempdir().unwrap();
        assert!(spool_spawn_failed(&spool_in(dir.path()), Uuid::new_v4(), &json!([1])).is_err());
    }
}
