//! Per-kind, per-reason counts of the session-outbox LIFECYCLE rows the drain
//! dropped — `GET /health` `sessionOutbox.lifecycleDrops` (plan
//! `2026-10-29-session-outbox-drops-the-finish-of-a-session-coord-never-registered`,
//! Phase 1).
//!
//! A lifecycle row (`started` / `state_change` / `closed` / `finished`, the
//! kinds `coord_sync::is_lifecycle_kind` names) is a one-shot fact no later row
//! repeats, so every drop of one leaves coord's session row wrong for good.
//! Before this block a drop left only a log line, and a successful push left
//! nothing at all, so "was the `started` row lost or refused?" could not be
//! answered from the runner. Every drop path is now counted here:
//!
//! | reason           | where it is counted                                         |
//! |------------------|-------------------------------------------------------------|
//! | `refused_4xx`    | `push_chain`'s `PermanentFailure` arm, any 4xx but the next |
//! | `not_found`      | the same arm, a 404 whose body `error` is `session_not_found` |
//! | `quarantine_cap` | `quarantine_rows`' cap branch (the sidecar is full)         |
//! | `outbox_trim`    | `OutboxWriter::enforce_cap_locked` (read from the writer)   |
//! | `not_found_closed` | `recover_row_missing`: a 404 for a session whose `closed` coord already ACKed |
//! | `not_found_superseded` | the same, for an id a resume replaced with a fresh one |
//! | `orphaned_no_body` | the same, no create body is recorded for the session |
//! | `orphaned_id_collision` | the same, the re-register `POST /sessions` answered 409 |
//! | `orphaned_reregister_refused` | the same, the re-register answered another 4xx |
//!
//! Since Phase 2 a `session_not_found` 404 on a path-addressed PATCH is not
//! dropped as `not_found`: it re-creates the coord row and retries. So
//! `not_found` should read `0`; a non-zero value means a lifecycle 404 reached
//! the drop arm some other way, which is itself the finding. When the row must
//! NOT be re-created (the session was closed or superseded — re-creating it
//! would resurrect a session nothing will ever close) or CANNOT be (the three
//! `orphaned_*` reasons: coord gives the same answer on every retry), the row
//! is dropped under one of the five reasons after `outbox_trim`, each distinct
//! so `coord.delivery` can name why a finish never landed. Only a transient
//! re-register outcome (no answer, or the per-session budget is spent) holds
//! the row instead (`super::coord_sync_reregister`).
//!
//! Heartbeats are deliberately absent: a sibling plan
//! (`2026-10-29-session-outbox-heartbeat-drops-are-uncounted-by-cause`) owns
//! their counters.
//!
//! The rendered block is DENSE — every lifecycle kind with every reason, zeros
//! included — so `lifecycleDrops.finished.not_found` reads `0` rather than
//! being absent; an absent key would be UNKNOWN, not zero.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Map, Value as JsonValue};

use super::SessionEventKind;

/// The lifecycle kinds this block counts, in render order. Must equal the set
/// `coord_sync::is_lifecycle_kind` accepts — pinned by a test in `coord_sync`.
pub(crate) const LIFECYCLE_DROP_KINDS: [SessionEventKind; 4] = [
    SessionEventKind::Started,
    SessionEventKind::StateChange,
    SessionEventKind::Closed,
    SessionEventKind::Finished,
];

/// Why a lifecycle row left the outbox without coord taking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleDropReason {
    /// Coord answered a 4xx other than a `session_not_found` 404.
    Refused4xx,
    /// Coord answered 404 `{"error":"session_not_found"}` — it has no row.
    NotFound,
    /// The quarantine sidecar was at its cap, so the row was dropped.
    QuarantineCap,
    /// The outbox's own byte cap trimmed the row before it was sent.
    OutboxTrim,
    /// A `session_not_found` 404 for a session whose `closed` row coord had
    /// already ACKed: coord GC'd a closed row. Re-creating it would bring the
    /// session back LIVE with no `closed` ever following, so it is not.
    NotFoundClosed,
    /// A `session_not_found` 404 for an id a resume replaced with a fresh one
    /// (`SessionRegistry::resume_external`'s 404 fallback). Re-creating the old
    /// id would leave a zombie row beside the fresh one, or race the fresh
    /// `started` into a `harness_session_already_bound` conflict.
    NotFoundSuperseded,
    /// A `session_not_found` 404 with no create body recorded for the session,
    /// so the row cannot be re-created — on any retry.
    OrphanedNoBody,
    /// The re-register `POST /sessions` answered 409: the id is held by
    /// another device / tenant, which no retry changes.
    OrphanedIdCollision,
    /// The re-register `POST /sessions` answered another 4xx (400
    /// `invalid_intent`, 403 …): the same body is refused the same way again.
    OrphanedReregisterRefused,
}

/// How many [`LifecycleDropReason`]s there are — the counter matrix's width.
const REASONS: usize = 9;

impl LifecycleDropReason {
    /// Every reason, in render order.
    pub(crate) const ALL: [LifecycleDropReason; REASONS] = [
        Self::Refused4xx,
        Self::NotFound,
        Self::QuarantineCap,
        Self::OutboxTrim,
        Self::NotFoundClosed,
        Self::NotFoundSuperseded,
        Self::OrphanedNoBody,
        Self::OrphanedIdCollision,
        Self::OrphanedReregisterRefused,
    ];

    /// The wire key under `lifecycleDrops.<kind>`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Refused4xx => "refused_4xx",
            Self::NotFound => "not_found",
            Self::QuarantineCap => "quarantine_cap",
            Self::OutboxTrim => "outbox_trim",
            Self::NotFoundClosed => "not_found_closed",
            Self::NotFoundSuperseded => "not_found_superseded",
            Self::OrphanedNoBody => "orphaned_no_body",
            Self::OrphanedIdCollision => "orphaned_id_collision",
            Self::OrphanedReregisterRefused => "orphaned_reregister_refused",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Refused4xx => 0,
            Self::NotFound => 1,
            Self::QuarantineCap => 2,
            Self::OutboxTrim => 3,
            Self::NotFoundClosed => 4,
            Self::NotFoundSuperseded => 5,
            Self::OrphanedNoBody => 6,
            Self::OrphanedIdCollision => 7,
            Self::OrphanedReregisterRefused => 8,
        }
    }

    /// Classify a `PushOutcome::PermanentFailure` message. `status` is the
    /// leading HTTP status the drain already parsed from it; the message is
    /// `"<status>: <coord body>"`. Only a 404 whose body's PARSED `error` field
    /// is coord's `session_not_found` is [`Self::NotFound`] — a substring match
    /// would let a proxy page or a `detail` string pass for coord's answer.
    pub(crate) fn for_permanent_failure(status: Option<u16>, message: &str) -> Self {
        let not_found = status == Some(404)
            && message
                .split_once(": ")
                .and_then(|(_, body)| serde_json::from_str::<JsonValue>(body).ok())
                .and_then(|v| v.get("error")?.as_str().map(|e| e == "session_not_found"))
                .unwrap_or(false);
        if not_found {
            Self::NotFound
        } else {
            Self::Refused4xx
        }
    }
}

/// Row index of `kind` in [`LIFECYCLE_DROP_KINDS`]; `None` for any other kind.
fn kind_index(kind: &str) -> Option<usize> {
    LIFECYCLE_DROP_KINDS.iter().position(|k| k.as_str() == kind)
}

/// The live, process-lifetime counter one `CoordSync` drain increments.
#[derive(Debug, Default)]
pub(crate) struct LifecycleDropCounter {
    counts: [[AtomicU64; REASONS]; 4],
}

impl LifecycleDropCounter {
    /// Count one dropped row of `kind`. A non-lifecycle kind is ignored (and
    /// `false` returned), so a caller may pass any row's `event_kind`.
    pub(crate) fn record(&self, kind: &str, reason: LifecycleDropReason) -> bool {
        match kind_index(kind) {
            Some(k) => {
                self.counts[k][reason.index()].fetch_add(1, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// A point-in-time copy for the health snapshot.
    pub(crate) fn snapshot(&self) -> LifecycleDrops {
        let mut out = LifecycleDrops::default();
        for (k, row) in self.counts.iter().enumerate() {
            for (r, n) in row.iter().enumerate() {
                out.counts[k][r] = n.load(Ordering::Relaxed);
            }
        }
        out
    }
}

/// A snapshot of [`LifecycleDropCounter`], carried by `SessionOutboxHealth`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LifecycleDrops {
    counts: [[u64; REASONS]; 4],
}

impl LifecycleDrops {
    /// Add `n` drops of `kind` for `reason`; a non-lifecycle kind is ignored.
    /// The outbox-trim totals are merged in this way from the writer's own
    /// per-kind counter.
    pub(crate) fn add(&mut self, kind: &str, reason: LifecycleDropReason, n: u64) {
        if let Some(k) = kind_index(kind) {
            self.counts[k][reason.index()] += n;
        }
    }

    /// The count for one cell; `0` for a non-lifecycle kind.
    pub(crate) fn get(&self, kind: &str, reason: LifecycleDropReason) -> u64 {
        kind_index(kind).map_or(0, |k| self.counts[k][reason.index()])
    }

    /// `{<kind>: {<reason>: count}}`, dense over every lifecycle kind and reason.
    pub(crate) fn to_json(&self) -> JsonValue {
        let mut kinds = Map::new();
        for (k, kind) in LIFECYCLE_DROP_KINDS.iter().enumerate() {
            let mut reasons = Map::new();
            for reason in LifecycleDropReason::ALL {
                reasons.insert(
                    reason.as_str().to_string(),
                    json!(self.counts[k][reason.index()]),
                );
            }
            kinds.insert(kind.as_str().to_string(), JsonValue::Object(reasons));
        }
        JsonValue::Object(kinds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_coords_parsed_session_not_found_404_is_not_found() {
        use LifecycleDropReason::*;
        let cases = [
            (
                Some(404),
                r#"404 Not Found: {"error":"session_not_found"}"#,
                NotFound,
            ),
            // The legacy fake fixture's spaced spelling is not coord's code.
            (
                Some(404),
                r#"404 Not Found: {"error":"session not found"}"#,
                Refused4xx,
            ),
            (
                Some(404),
                "404 Not Found: <html>session_not_found</html>",
                Refused4xx,
            ),
            (
                Some(404),
                r#"404 Not Found: {"detail":"session_not_found"}"#,
                Refused4xx,
            ),
            (
                Some(422),
                r#"422 Unprocessable Entity: {"error":"session_not_found"}"#,
                Refused4xx,
            ),
            (
                Some(403),
                r#"403 Forbidden: {"error":"refused"}"#,
                Refused4xx,
            ),
            (None, "payload carries no usable slug", Refused4xx),
        ];
        for (status, msg, want) in cases {
            assert_eq!(
                LifecycleDropReason::for_permanent_failure(status, msg),
                want,
                "{msg}"
            );
        }
    }

    #[test]
    fn counter_ignores_non_lifecycle_kinds_and_renders_dense() {
        let c = LifecycleDropCounter::default();
        assert!(c.record("finished", LifecycleDropReason::NotFound));
        assert!(!c.record("heartbeat", LifecycleDropReason::NotFound));
        let mut snap = c.snapshot();
        snap.add("started", LifecycleDropReason::OutboxTrim, 3);
        snap.add("output_chunk", LifecycleDropReason::OutboxTrim, 9);
        let v = snap.to_json();
        assert_eq!(v["finished"]["not_found"], 1);
        assert_eq!(v["started"]["outbox_trim"], 3);
        assert_eq!(v["closed"]["quarantine_cap"], 0, "dense: zeros present");
        assert!(v.get("heartbeat").is_none() && v.get("output_chunk").is_none());
        assert_eq!(v.as_object().unwrap().len(), 4);
        for kind in v.as_object().unwrap().values() {
            assert_eq!(
                kind.as_object().unwrap().len(),
                LifecycleDropReason::ALL.len()
            );
        }
    }

    /// `index` is a bijection onto `0..REASONS` and every wire key is
    /// distinct, so no two reasons share a counter cell or a JSON key.
    #[test]
    fn every_reason_has_its_own_cell_and_wire_key() {
        let mut idx: Vec<usize> = LifecycleDropReason::ALL.iter().map(|r| r.index()).collect();
        idx.sort_unstable();
        assert_eq!(idx, (0..REASONS).collect::<Vec<_>>());
        let keys: std::collections::HashSet<&str> = LifecycleDropReason::ALL
            .iter()
            .map(|r| r.as_str())
            .collect();
        assert_eq!(keys.len(), REASONS);
        let c = LifecycleDropCounter::default();
        c.record("finished", LifecycleDropReason::NotFoundClosed);
        c.record("closed", LifecycleDropReason::OrphanedIdCollision);
        let v = c.snapshot().to_json();
        assert_eq!(v["finished"]["not_found_closed"], 1);
        assert_eq!(v["closed"]["orphaned_id_collision"], 1);
        assert_eq!(v["finished"]["not_found_superseded"], 0);
    }
}
