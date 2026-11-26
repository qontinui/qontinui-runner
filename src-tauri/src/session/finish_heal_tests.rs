//! Tests for `session::finish_heal` (plan
//! `2026-11-05-finish-synced-is-stamped-on-a-2xx-that-did-not-move-session-status`,
//! Phase 5). Literal coord bodies throughout.

use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::mcp::session_work_status::{degraded_fetch_for_test, fetch_from_body_for_test};
use crate::session::local_store::OutboxWriter;
use crate::session::session_lifecycle_store::test_open_record;
use crate::session::SessionEventKind;

const ROW: &str = "01a1ab84-1c55-7a2e-9b41-6c0f3d2e8a17";
const OTHER_ROW: &str = "01a1ab99-0000-7000-8000-000000000002";
/// 2026-11-05T12:05:42Z.
const FINISHED_AT: i64 = 1_793_880_342_000;
const BEFORE: &str = "2026-11-05T11:00:00Z";
const AFTER: &str = "2026-11-05T13:00:00Z";

fn row() -> Uuid {
    Uuid::parse_str(ROW).unwrap()
}

fn observed(status: Option<&str>, since: Option<&str>, coord: Option<&str>) -> ObservedRow {
    ObservedRow {
        session_status: status.map(str::to_string),
        since: since.map(str::to_string),
        since_ms: since
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp_millis()),
        coord_session_id: coord.map(str::to_string),
        state: None,
        started_at: None,
        started_at_ms: None,
    }
}

/// [`observed`] for a row naming [`OTHER_ROW`], with its `state` and
/// `started_at` (Phase 4 of plan
/// `2026-11-07-a-runner-finish-is-addressed-to-a-stale-coord-row-after-a-pane-respawn`).
fn other(
    status: Option<&str>,
    since: Option<&str>,
    state: Option<&str>,
    started_at: Option<&str>,
) -> ObservedRow {
    ObservedRow {
        state: state.map(str::to_string),
        started_at: started_at.map(str::to_string),
        started_at_ms: started_at
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp_millis()),
        ..observed(status, since, Some(OTHER_ROW))
    }
}

fn other_row() -> Uuid {
    Uuid::parse_str(OTHER_ROW).unwrap()
}

fn decide(obs: &ObservedRow) -> HealDecision {
    heal_decision(
        FINISHED_AT,
        Some(row()),
        Observation::Row(Some(obs)),
        FINISHED_AT + 60_000,
    )
}

// ---------------------------------------------------------------------------
// D3, pure
// ---------------------------------------------------------------------------

#[test]
fn coord_null_on_the_marks_own_row_is_resent() {
    assert_eq!(
        decide(&observed(None, None, Some(ROW))),
        HealDecision::Resend { row: row() }
    );
}

/// Review F1: `coord_report_status` can report `working` over a row the lost
/// finish never moved WITHOUT advancing `state_started_at`, so a `since`
/// before the finish proves nothing — `working` is never healed.
#[test]
fn coord_working_is_never_resent_whatever_its_since() {
    for since in [Some(BEFORE), Some(AFTER), None] {
        assert_eq!(
            decide(&observed(Some("working"), since, Some(ROW))),
            HealDecision::Leave(HealSkip::WorkingRevivalUnprovable),
            "working since {since:?}"
        );
    }
    for word in ["blocked", "stalled", "waiting_human", "something_new"] {
        assert_eq!(
            decide(&observed(Some(word), Some(BEFORE), Some(ROW))),
            HealDecision::Leave(HealSkip::StatusNotNull),
            "{word}"
        );
    }
    assert_eq!(
        HealSkip::WorkingRevivalUnprovable.as_str(),
        "working_revival_unprovable"
    );
}

/// Mutation target: drop the `since` comparison and this goes red.
#[test]
fn coord_null_since_after_the_finish_is_a_revival_and_untouched() {
    assert_eq!(
        decide(&observed(None, Some(AFTER), Some(ROW))),
        HealDecision::Leave(HealSkip::RevivedAfterFinish)
    );
    // `since` exactly at the finish is not EARLIER than it.
    assert_eq!(
        decide(&observed(None, Some("2026-11-05T12:05:42Z"), Some(ROW))),
        HealDecision::Leave(HealSkip::RevivedAfterFinish)
    );
    assert_eq!(
        decide(&observed(None, Some(BEFORE), Some(ROW))),
        HealDecision::Resend { row: row() }
    );
}

#[test]
fn every_unknown_leaves_the_mark_alone() {
    assert_eq!(
        heal_decision(
            FINISHED_AT,
            Some(row()),
            Observation::Unreadable,
            FINISHED_AT
        ),
        HealDecision::Leave(HealSkip::CoordUnreadable)
    );
    assert_eq!(
        heal_decision(
            FINISHED_AT,
            Some(row()),
            Observation::Row(None),
            FINISHED_AT
        ),
        HealDecision::Leave(HealSkip::NoCoordRow)
    );
    assert_eq!(
        decide(&observed(None, None, Some(OTHER_ROW))),
        HealDecision::Leave(HealSkip::DifferentRowStateUnknown),
        "a different row with no served state is UNKNOWN, never re-addressed"
    );
    assert_eq!(
        decide(&observed(None, None, None)),
        HealDecision::Leave(HealSkip::DifferentRow),
        "a row naming no coord id is not the mark's own"
    );
    let obs = observed(None, None, Some(ROW));
    assert_eq!(
        heal_decision(FINISHED_AT, None, Observation::Row(Some(&obs)), FINISHED_AT),
        HealDecision::Leave(HealSkip::MarkRowUnknown)
    );
    let mut garbled = observed(None, None, Some(ROW));
    garbled.since = Some("yesterday-ish".to_string());
    assert_eq!(
        decide(&garbled),
        HealDecision::Leave(HealSkip::SinceUnreadable)
    );
}

#[test]
fn coord_finished_is_the_healthy_case() {
    assert_eq!(
        decide(&observed(
            Some("finished"),
            Some("2026-11-05T12:05:42Z"),
            Some(ROW)
        )),
        HealDecision::Leave(HealSkip::CoordFinished)
    );
}

#[test]
fn a_mark_older_than_fourteen_days_is_untouched() {
    let obs = observed(None, None, Some(ROW));
    let day = 24 * 60 * 60 * 1000;
    assert_eq!(
        heal_decision(
            FINISHED_AT,
            Some(row()),
            Observation::Row(Some(&obs)),
            FINISHED_AT + 15 * day
        ),
        HealDecision::Leave(HealSkip::OutOfWindow)
    );
    assert_eq!(
        heal_decision(
            FINISHED_AT,
            Some(row()),
            Observation::Row(Some(&obs)),
            FINISHED_AT + 13 * day
        ),
        HealDecision::Resend { row: row() }
    );
}

// ---------------------------------------------------------------------------
// apply_heal over a real store
// ---------------------------------------------------------------------------

/// A port whose own-row resolution is fixed and whose re-send goes through
/// the store's serialized door, recording each enqueue.
struct FakePort {
    store: Arc<SessionLifecycleStore>,
    own: Option<Uuid>,
    sent: std::sync::Mutex<Vec<(String, i64, Uuid)>>,
}

impl HealPort for FakePort {
    fn own_row(&self, rec: &TerminalSessionRecord) -> Option<Uuid> {
        // The production preference: the stored sent/confirmed row first.
        rec.finish_coord_session_id.or(self.own)
    }
    fn redeliver(&self, csid: &str, finished_at: i64, row: Uuid) -> Option<FinishSync> {
        self.store.with_unsynced_finished_records(|recs| {
            recs.into_iter()
                .find(|r| r.claude_session_id == csid && r.finished_at == Some(finished_at))
                .map(|_| {
                    self.sent
                        .lock()
                        .unwrap()
                        .push((csid.to_string(), finished_at, row));
                    FinishSync::Queued {
                        coord_session_id: row,
                    }
                })
        })
    }
    fn finish_queued(&self, csid: &str, finished_at: i64, row: Uuid) -> Option<bool> {
        // Nothing behind this port ever drains: what it sent is still queued.
        Some(
            self.sent
                .lock()
                .unwrap()
                .iter()
                .any(|(c, at, r)| c == csid && *at == finished_at && *r == row),
        )
    }
}

/// A store holding one finished mark for `csid`, stamped synced.
fn synced_store(csid: &str) -> (tempfile::TempDir, Arc<SessionLifecycleStore>, i64) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionLifecycleStore::open(dir.path().join("lifecycle.json")).unwrap());
    store.record_open(test_open_record(csid));
    let at = store
        .set_finished(csid, true, Some("done".into()))
        .unwrap()
        .record
        .finished_at
        .unwrap();
    store.mark_finish_synced(csid, Some(at));
    assert!(store.get(csid).unwrap().finish_synced);
    (dir, store, at)
}

fn body(
    csid: &str,
    status: serde_json::Value,
    since: serde_json::Value,
    coord: &str,
) -> serde_json::Value {
    json!({
        "statuses": {csid: {"session_status": status, "since": since,
                             "coord_session_id": coord, "state": "closed"}},
        "unknown": [], "invalid": [], "input_truncated": false,
        "sessionBridgeColumnPresent": true
    })
}

fn port(store: &Arc<SessionLifecycleStore>) -> FakePort {
    FakePort {
        store: store.clone(),
        own: Some(row()),
        sent: Default::default(),
    }
}

#[test]
fn a_synced_mark_over_coord_null_is_unstamped_and_requeued_once() {
    let (_d, store, at) = synced_store("heal-null");
    let p = port(&store);
    let fetch = fetch_from_body_for_test(body("heal-null", json!(null), json!(null), ROW), 1);
    let out = apply_heal(&store, Some(&p), &store.all_records(), &fetch, at + 1_000);
    assert_eq!(
        out,
        vec![("heal-null".to_string(), HealOutcome::Resent { row: row() })]
    );
    assert!(
        !store.get("heal-null").unwrap().finish_synced,
        "the stamp is withdrawn"
    );
    assert_eq!(
        *p.sent.lock().unwrap(),
        vec![("heal-null".to_string(), at, row())]
    );

    // The sweep run again: the mark is no longer synced, so nothing more.
    let again = apply_heal(&store, Some(&p), &store.all_records(), &fetch, at + 2_000);
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(
        p.sent.lock().unwrap().len(),
        1,
        "one re-queue across two sweeps"
    );
}

#[test]
fn coord_working_since_before_the_finish_leaves_the_mark_synced() {
    let (_d, store, at) = synced_store("heal-working");
    let p = port(&store);
    let since = chrono::DateTime::<Utc>::from_timestamp_millis(at - 3_600_000)
        .unwrap()
        .to_rfc3339();
    let fetch =
        fetch_from_body_for_test(body("heal-working", json!("working"), json!(since), ROW), 1);
    let out = apply_heal(&store, Some(&p), &store.all_records(), &fetch, at + 1_000);
    assert_eq!(
        out[0].1,
        HealOutcome::Left(HealSkip::WorkingRevivalUnprovable)
    );
    assert!(
        store.get("heal-working").unwrap().finish_synced,
        "untouched"
    );
    assert!(p.sent.lock().unwrap().is_empty());
}

#[test]
fn coord_null_since_after_the_finish_leaves_the_mark_synced() {
    let (_d, store, at) = synced_store("heal-revived");
    let p = port(&store);
    let since = chrono::DateTime::<Utc>::from_timestamp_millis(at + 3_600_000)
        .unwrap()
        .to_rfc3339();
    let fetch = fetch_from_body_for_test(body("heal-revived", json!(null), json!(since), ROW), 1);
    let out = apply_heal(
        &store,
        Some(&p),
        &store.all_records(),
        &fetch,
        at + 7_200_000,
    );
    assert_eq!(out[0].1, HealOutcome::Left(HealSkip::RevivedAfterFinish));
    assert!(
        store.get("heal-revived").unwrap().finish_synced,
        "untouched"
    );
    assert!(p.sent.lock().unwrap().is_empty());
}

#[test]
fn coord_unreachable_leaves_the_mark_and_counts_the_reason() {
    let (_d, store, at) = synced_store("heal-unreachable");
    let p = port(&store);
    let count = || {
        heal_health_json()["byOutcome"]["coord_unreadable"]
            .as_u64()
            .unwrap_or(0)
    };
    let before = count();
    let fetch = degraded_fetch_for_test("coord work-status: request timed out after 2s");
    let out = apply_heal(&store, Some(&p), &store.all_records(), &fetch, at + 1_000);
    assert_eq!(out[0].1, HealOutcome::Left(HealSkip::CoordUnreadable));
    assert!(store.get("heal-unreachable").unwrap().finish_synced);
    assert!(p.sent.lock().unwrap().is_empty());
    assert!(
        count() > before,
        "the reason is counted in /health finishHeal"
    );
}

#[test]
fn a_mark_older_than_the_window_is_never_resent() {
    let (_d, store, at) = synced_store("heal-old");
    let p = port(&store);
    let fetch = fetch_from_body_for_test(body("heal-old", json!(null), json!(null), ROW), 1);
    let out = apply_heal(
        &store,
        Some(&p),
        &store.all_records(),
        &fetch,
        at + HEAL_WINDOW_MS + 1,
    );
    assert_eq!(out[0].1, HealOutcome::Left(HealSkip::OutOfWindow));
    assert!(store.get("heal-old").unwrap().finish_synced);
    assert!(p.sent.lock().unwrap().is_empty());
}

#[test]
fn a_different_row_or_an_unresolved_own_row_changes_nothing() {
    let (_d, store, at) = synced_store("heal-other");
    let p = port(&store);
    let fetch =
        fetch_from_body_for_test(body("heal-other", json!(null), json!(null), OTHER_ROW), 1);
    let out = apply_heal(&store, Some(&p), &store.all_records(), &fetch, at + 1_000);
    assert_eq!(out[0].1, HealOutcome::Left(HealSkip::DifferentRowNotActive));
    let blind = FakePort {
        own: None,
        ..port(&store)
    };
    let fetch = fetch_from_body_for_test(body("heal-other", json!(null), json!(null), ROW), 1);
    let out = apply_heal(
        &store,
        Some(&blind),
        &store.all_records(),
        &fetch,
        at + 1_000,
    );
    assert_eq!(out[0].1, HealOutcome::Left(HealSkip::MarkRowUnknown));
    assert!(store.get("heal-other").unwrap().finish_synced);
}

#[test]
fn clear_finish_synced_clears_only_the_matching_synced_mark() {
    let (_d, store, at) = synced_store("clear-me");
    assert!(
        !store.clear_finish_synced("clear-me", at + 1),
        "another mark's time"
    );
    assert!(store.get("clear-me").unwrap().finish_synced);
    assert!(store.clear_finish_synced("clear-me", at));
    assert!(!store.get("clear-me").unwrap().finish_synced);
    assert!(
        store.get("clear-me").unwrap().finished_at == Some(at),
        "the mark stays"
    );
    assert!(
        !store.clear_finish_synced("clear-me", at),
        "already unsynced"
    );
    assert!(!store.clear_finish_synced("ghost", at));
}

// ---------------------------------------------------------------------------
// The real registrar: serialization and the revival paths
// ---------------------------------------------------------------------------

/// A registrar + store wired as main.rs wires them (finish observer → the
/// registrar), with the terminal plane resolving every id to [`ROW`].
fn wired() -> (
    tempfile::TempDir,
    Arc<OutboxWriter>,
    Arc<AiCoordRegistrar>,
    Arc<SessionLifecycleStore>,
) {
    let dir = tempfile::tempdir().unwrap();
    let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
    let reg = Arc::new(AiCoordRegistrar::with_tenant_resolver(
        outbox.clone(),
        Uuid::new_v4(),
        || None,
    ));
    let store = Arc::new(SessionLifecycleStore::open(dir.path().join("lifecycle.json")).unwrap());
    reg.attach_lifecycle_store(store.clone());
    reg.attach_terminal_coord_lookup(|_| Some(Uuid::parse_str(ROW).unwrap()));
    let weak = Arc::downgrade(&reg);
    store.attach_finish_observer(move |rec| match weak.upgrade() {
        Some(r) => r.forward_finish_change(rec),
        None => FinishSync::LocalOnly(
            crate::session::session_lifecycle_store::LocalOnlyReason::NoForwarder,
        ),
    });
    (dir, outbox, reg, store)
}

/// `(kind, session_status | claude_session_id)` per pending row, in order.
fn rows(outbox: &OutboxWriter) -> Vec<(String, String)> {
    let mut pending = outbox.pending().unwrap();
    pending.sort_by_key(|r| r.seq);
    pending
        .into_iter()
        .map(|r| {
            let detail = r.payload["session_status"]
                .as_str()
                .or_else(|| r.payload["claude_session_id"].as_str())
                .unwrap_or_default()
                .to_string();
            (r.event_kind, detail)
        })
        .collect()
}

fn drain_all(outbox: &OutboxWriter) {
    let keys: Vec<(Uuid, i64)> = outbox
        .pending()
        .unwrap()
        .into_iter()
        .map(|r| (r.session_id, r.seq))
        .collect();
    outbox.ack(&keys).unwrap();
}

/// A heal that loses the race to an unmark never enqueues `Finished` after the
/// unmark's `working`: the re-send re-reads the mark under `finish_forward`.
#[test]
fn a_heal_racing_an_unmark_never_enqueues_finished_after_the_working() {
    let (_d, outbox, reg, store) = wired();
    store.record_open(test_open_record("race"));
    let at = store
        .set_finished("race", true, Some("done".into()))
        .unwrap()
        .record
        .finished_at
        .unwrap();
    store.mark_finish_synced("race", Some(at));
    drain_all(&outbox);

    // The heal's first half wins the lock …
    assert!(store.clear_finish_synced("race", at));
    // … the operator's unmark lands between the two halves …
    store.set_finished("race", false, None).unwrap();
    // … and the re-send finds no finished mark to send.
    assert_eq!(reg.redeliver_finish("race", at, row()), None);
    assert_eq!(
        rows(&outbox),
        vec![("progress".to_string(), "working".to_string())],
        "only the unmark's working — no Finished after it"
    );

    // Without the race the same door queues the mark's own finish.
    let at2 = store
        .set_finished("race", true, Some("again".into()))
        .unwrap()
        .record
        .finished_at
        .unwrap();
    drain_all(&outbox);
    assert_eq!(
        reg.redeliver_finish("race", at2, row()),
        Some(FinishSync::Queued {
            coord_session_id: row()
        })
    );
    assert_eq!(
        rows(&outbox),
        vec![("finished".to_string(), "race".to_string())]
    );
}

/// Revival path 1: `unfinish_session` runs from `set_finished(false)`'s
/// observer, AFTER the local mark is cleared.
#[test]
fn an_unmark_clears_the_local_mark_before_its_working_is_queued() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionLifecycleStore::open(dir.path().join("lifecycle.json")).unwrap());
    store.record_open(test_open_record("unmark"));
    store
        .set_finished("unmark", true, Some("done".into()))
        .unwrap();
    let seen: Arc<std::sync::Mutex<Vec<Option<i64>>>> = Default::default();
    {
        let seen = seen.clone();
        let weak = Arc::downgrade(&store);
        store.attach_finish_observer(move |rec| {
            // What the store itself holds at the moment the working row
            // would be queued.
            let held = weak.upgrade().unwrap().get(&rec.claude_session_id).unwrap();
            seen.lock().unwrap().push(held.finished_at);
            FinishSync::LocalOnly(
                crate::session::session_lifecycle_store::LocalOnlyReason::NoForwarder,
            )
        });
    }
    store.set_finished("unmark", false, None).unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![None],
        "cleared before forwarding"
    );
}

/// Revival path 3: an interaction with a finished session clears the local
/// mark BEFORE any `working` row is queued; the drain's bump is no revival
/// and queues nothing for a finished session.
#[test]
fn an_interaction_clears_the_local_mark_first_and_a_drain_does_not_revive() {
    let _env = crate::test_env::env_lock();
    std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
    let (_d, outbox, reg, store) = wired();
    let trid = Uuid::new_v4().to_string();
    let coord = reg.register_session(&trid, "work", None).unwrap();
    store.record_open(test_open_record(&trid));
    let at = store
        .set_finished(&trid, true, Some("done".into()))
        .unwrap()
        .record
        .finished_at
        .unwrap();
    store.mark_finish_synced(&trid, Some(at));
    drain_all(&outbox);

    // The drain: no revival, no row, the mark kept.
    reg.progress_on_drain(&trid);
    assert!(rows(&outbox).is_empty(), "{:?}", rows(&outbox));
    assert_eq!(store.get(&trid).unwrap().finished_at, Some(at));

    // The interaction: the mark is gone, and exactly ONE working row is
    // queued — the unmark's own, not a second from the interaction bump
    // (review A5).
    reg.progress_on_interaction(&trid);
    assert_eq!(store.get(&trid).unwrap().finished_at, None);
    assert_eq!(
        rows(&outbox),
        vec![("progress".to_string(), "working".to_string())],
        "a revival queues one working row"
    );
    assert!(
        outbox
            .pending()
            .unwrap()
            .iter()
            .all(|r| r.event_kind == SessionEventKind::Progress.as_str() && r.session_id == coord),
        "every working row addresses the session's own registered row"
    );

    // A second interaction on the now-unfinished session is an ordinary bump.
    drain_all(&outbox);
    reg.progress_on_interaction(&trid);
    assert_eq!(
        rows(&outbox),
        vec![("progress".to_string(), "working".to_string())]
    );
}

// ---------------------------------------------------------------------------
// The read-back
// ---------------------------------------------------------------------------

fn read_back_response(store: &SessionLifecycleStore, csid: &str) -> serde_json::Value {
    let outcome = store.set_finished(csid, true, None).unwrap();
    assert_eq!(
        outcome.changed.as_str(),
        "none",
        "a read-back writes nothing"
    );
    outcome.response_json()
}

#[test]
fn a_read_back_over_coord_finished_reports_it_observed_and_synced() {
    let (_d, store, at) = synced_store("rb-finished");
    let p = port(&store);
    let mut resp = read_back_response(&store, "rb-finished");
    let since = chrono::DateTime::<Utc>::from_timestamp_millis(at)
        .unwrap()
        .to_rfc3339();
    let fetch = fetch_from_body_for_test(
        body("rb-finished", json!("finished"), json!(since.clone()), ROW),
        1,
    );
    let now = chrono::DateTime::<Utc>::from_timestamp_millis(at + 5_000).unwrap();
    let rb = read_back_from(&store, Some(&p), "rb-finished", &fetch, now);
    decorate_read_back(&mut resp, &rb, store.get("rb-finished").as_ref());
    assert_eq!(resp["session"]["finishSynced"], true);
    assert_eq!(resp["coord"]["observed"]["sessionStatus"], "finished");
    assert_eq!(resp["coord"]["observed"]["since"], since);
    assert_eq!(resp["coord"]["observed"]["coordSessionId"], ROW);
    assert_eq!(resp["coord"]["observed"]["observedAt"], json!(now));
    assert_eq!(resp["coord"]["observedReason"], json!(null));
    assert_eq!(resp["coord"]["heal"]["outcome"], "coord_finished");
}

#[test]
fn a_read_back_over_coord_null_heals_and_stops_reading_synced() {
    let (_d, store, at) = synced_store("rb-null");
    let p = port(&store);
    let mut resp = read_back_response(&store, "rb-null");
    assert_eq!(
        resp["session"]["finishSynced"], true,
        "the stored stamp, before the read"
    );
    let fetch = fetch_from_body_for_test(body("rb-null", json!(null), json!(null), ROW), 1);
    let now = chrono::DateTime::<Utc>::from_timestamp_millis(at + 5_000).unwrap();
    let rb = read_back_from(&store, Some(&p), "rb-null", &fetch, now);
    decorate_read_back(&mut resp, &rb, store.get("rb-null").as_ref());
    assert_eq!(resp["session"]["finishSynced"], false);
    assert_eq!(resp["coord"]["observed"]["sessionStatus"], json!(null));
    assert_eq!(resp["coord"]["heal"]["outcome"], "resent");
    assert_eq!(resp["coord"]["queued"], true);
    assert_eq!(resp["coord"]["coordSessionId"], ROW);
    assert_eq!(p.sent.lock().unwrap().len(), 1);
}

#[test]
fn a_read_back_with_coord_unreadable_keeps_the_stored_value_and_names_why() {
    let (_d, store, at) = synced_store("rb-dark");
    let p = port(&store);
    let mut resp = read_back_response(&store, "rb-dark");
    let fetch = degraded_fetch_for_test("coord work-status: HTTP 401");
    let now = chrono::DateTime::<Utc>::from_timestamp_millis(at + 5_000).unwrap();
    let rb = read_back_from(&store, Some(&p), "rb-dark", &fetch, now);
    decorate_read_back(&mut resp, &rb, store.get("rb-dark").as_ref());
    assert_eq!(resp["coord"]["observed"], json!(null));
    assert_eq!(
        resp["coord"]["observedReason"],
        "coord work-status: HTTP 401"
    );
    assert_eq!(resp["session"]["finishSynced"], true, "the stored value");
    assert_eq!(resp["coord"]["heal"]["outcome"], "coord_unreadable");
    assert!(p.sent.lock().unwrap().is_empty());
}

#[test]
fn a_read_back_over_another_rows_finish_does_not_read_synced() {
    let (_d, store, at) = synced_store("rb-other");
    let p = port(&store);
    let mut resp = read_back_response(&store, "rb-other");
    let fetch = fetch_from_body_for_test(
        body("rb-other", json!("finished"), json!(AFTER), OTHER_ROW),
        1,
    );
    let now = chrono::DateTime::<Utc>::from_timestamp_millis(at + 5_000).unwrap();
    let rb = read_back_from(&store, Some(&p), "rb-other", &fetch, now);
    decorate_read_back(&mut resp, &rb, store.get("rb-other").as_ref());
    assert_eq!(resp["session"]["finishSynced"], false);
    assert_eq!(resp["coord"]["heal"]["outcome"], "different_row_not_active");
    assert!(
        store.get("rb-other").unwrap().finish_synced,
        "the stamp itself is untouched"
    );
}

// ---------------------------------------------------------------------------
// Review round 2: the stored own row (F2), unsynced marks (F3), chunking
// ---------------------------------------------------------------------------

/// Null-status rows for every id, all answered by [`ROW`].
fn null_rows(ids: &[String]) -> StatusFetch {
    let statuses: serde_json::Map<String, serde_json::Value> = ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                json!({"session_status": null, "since": null,
                       "coord_session_id": ROW, "state": "closed"}),
            )
        })
        .collect();
    fetch_from_body_for_test(
        json!({"statuses": statuses, "unknown": [], "invalid": [],
               "input_truncated": false, "sessionBridgeColumnPresent": true}),
        ids.len(),
    )
}

/// F2: with a REAL registrar that knows no session (a runner restart), the
/// mark's own row is the one stored with its observed stamp; a mark stamped
/// without one (a pre-F2 stamp) stays UNKNOWN.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sweep_resolves_a_marks_stored_row_and_leaves_an_unstored_one_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
    let reg = AiCoordRegistrar::with_tenant_resolver(outbox.clone(), Uuid::new_v4(), || None);
    let store = Arc::new(SessionLifecycleStore::open(dir.path().join("lifecycle.json")).unwrap());
    reg.attach_lifecycle_store(store.clone());
    for id in ["stored-row", "legacy-row"] {
        store.record_open(test_open_record(id));
        store.set_finished(id, true, Some("done".into())).unwrap();
    }
    let stored_at = store.get("stored-row").unwrap().finished_at;
    let legacy_at = store.get("legacy-row").unwrap().finished_at;
    store.mark_finish_synced_on("stored-row", stored_at, Some(row()));
    store.mark_finish_synced("legacy-row", legacy_at);
    assert_eq!(
        store.get("stored-row").unwrap().finish_coord_session_id,
        Some(row())
    );
    assert_eq!(
        store.get("legacy-row").unwrap().finish_coord_session_id,
        None
    );
    drain_all(&outbox);

    let mut out = heal_sweep_with(&store, Some(&reg), None, false, |ids| async move {
        null_rows(&ids)
    })
    .await;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        out,
        vec![
            (
                "legacy-row".to_string(),
                HealOutcome::Left(HealSkip::MarkRowUnknown)
            ),
            ("stored-row".to_string(), HealOutcome::Resent { row: row() }),
        ]
    );
    assert_eq!(
        rows(&outbox),
        vec![("finished".to_string(), "stored-row".to_string())],
        "only the stored-row mark is re-sent, to its own row"
    );
    assert!(outbox
        .pending()
        .unwrap()
        .iter()
        .all(|r| r.session_id == row()));
    assert!(store.get("legacy-row").unwrap().finish_synced, "untouched");
}

/// F2: an unmark forgets the stored row; a later stamp without one keeps it.
#[test]
fn the_stored_row_follows_the_mark() {
    let (_d, store, at) = synced_store("stored");
    assert_eq!(store.get("stored").unwrap().finish_coord_session_id, None);
    assert!(store.clear_finish_synced("stored", at));
    store.mark_finish_synced_on("stored", Some(at), Some(row()));
    assert_eq!(
        store.get("stored").unwrap().finish_coord_session_id,
        Some(row())
    );
    // A later stamp WITHOUT a row keeps the stored one.
    assert!(store.clear_finish_synced("stored", at));
    store.mark_finish_synced("stored", Some(at));
    let rec = store.get("stored").unwrap();
    assert!(rec.finish_synced);
    assert_eq!(rec.finish_coord_session_id, Some(row()));
    // A restamp keeps it too; only an unmark forgets it.
    store
        .set_finished("stored", true, Some("again".into()))
        .unwrap();
    assert_eq!(
        store.get("stored").unwrap().finish_coord_session_id,
        Some(row())
    );
    store.set_finished("stored", false, None).unwrap();
    assert_eq!(store.get("stored").unwrap().finish_coord_session_id, None);
}

/// A store holding one finished mark for `csid`, NOT synced.
fn unsynced_store(csid: &str) -> (tempfile::TempDir, Arc<SessionLifecycleStore>, i64) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionLifecycleStore::open(dir.path().join("lifecycle.json")).unwrap());
    store.record_open(test_open_record(csid));
    let at = store
        .set_finished(csid, true, Some("done".into()))
        .unwrap()
        .record
        .finished_at
        .unwrap();
    (dir, store, at)
}

/// F3: the boot sweep re-offers an UNSYNCED mark (e.g. one the drain dropped
/// after the unconfirmed bound) only when its own row reads NULL; the
/// read-back arm never takes unsynced marks.
#[test]
fn the_sweep_reoffers_an_unsynced_mark_only_over_a_null_row() {
    let (_d, store, at) = unsynced_store("unsynced");
    let p = port(&store);
    let ids = vec!["unsynced".to_string()];

    let working = Observations::from_fetch(
        &ids,
        &fetch_from_body_for_test(body("unsynced", json!("working"), json!(BEFORE), ROW), 1),
    );
    let out = apply_heal_observed(
        &store,
        Some(&p),
        &store.all_records(),
        &working,
        at + 1_000,
        true,
    );
    assert_eq!(
        out[0].1,
        HealOutcome::Left(HealSkip::WorkingRevivalUnprovable)
    );
    assert!(p.sent.lock().unwrap().is_empty());

    let null = Observations::from_fetch(&ids, &null_rows(&ids));
    let read_back_arm = apply_heal_observed(
        &store,
        Some(&p),
        &store.all_records(),
        &null,
        at + 1_000,
        false,
    );
    assert!(
        read_back_arm.is_empty(),
        "the read-back arm skips unsynced marks"
    );
    let out = apply_heal_observed(
        &store,
        Some(&p),
        &store.all_records(),
        &null,
        at + 1_000,
        true,
    );
    assert_eq!(
        out,
        vec![(
            "unsynced".to_string(),
            HealOutcome::Reoffered { row: row() }
        )]
    );
    assert_eq!(
        *p.sent.lock().unwrap(),
        vec![("unsynced".to_string(), at, row())]
    );
    assert!(
        !store.get("unsynced").unwrap().finish_synced,
        "still owed, honestly"
    );
}

/// Ids are read in chunks at coord's per-request cap, so the 501st mark is
/// observed like the first instead of reading as `no_coord_row`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sweep_reads_ids_in_chunks_at_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionLifecycleStore::open(dir.path().join("lifecycle.json")).unwrap());
    let n = MAX_IDS + 1;
    for i in 0..n {
        let id = format!("cap-{i:04}");
        store.record_open(test_open_record(&id));
        let at = store
            .set_finished(&id, true, Some("done".into()))
            .unwrap()
            .record
            .finished_at;
        store.mark_finish_synced(&id, at);
    }
    let p = port(&store);
    let chunks: Arc<std::sync::Mutex<Vec<usize>>> = Default::default();
    let seen = chunks.clone();
    let out = heal_sweep_with(&store, Some(&p), None, false, move |ids| {
        let seen = seen.clone();
        async move {
            seen.lock().unwrap().push(ids.len());
            // Coord answers only for the ids it was asked about.
            null_rows(&ids)
        }
    })
    .await;
    assert_eq!(*chunks.lock().unwrap(), vec![MAX_IDS, 1]);
    assert_eq!(out.len(), n);
    assert!(
        out.iter()
            .all(|(_, o)| *o == HealOutcome::Resent { row: row() }),
        "no id past the cap reads as no_coord_row"
    );
}

// ---------------------------------------------------------------------------
// Review round 3: the SENT row of a dropped unconfirmed finish; truncation
// ---------------------------------------------------------------------------

/// The sent row is recorded without stamping, only on the matching mark.
#[test]
fn a_sent_row_is_recorded_without_stamping() {
    let (_d, store, at) = unsynced_store("sent");
    store.note_finish_sent_row("sent", Some(at + 1), row());
    assert_eq!(
        store.get("sent").unwrap().finish_coord_session_id,
        None,
        "another mark"
    );
    store.note_finish_sent_row("sent", Some(at), row());
    let rec = store.get("sent").unwrap();
    assert_eq!(rec.finish_coord_session_id, Some(row()));
    assert!(!rec.finish_synced, "never stamped");
    store.note_finish_sent_row("ghost", Some(at), row());
}

/// Review r4: a sent row never replaces the row that CONFIRMED a synced mark
/// (two chains for one mark: A confirmed and stamped, B dropped unconfirmed).
#[test]
fn a_sent_row_never_replaces_a_confirmed_one() {
    let (_d, store, at) = unsynced_store("confirmed");
    let confirmed = row();
    let sent = Uuid::parse_str(OTHER_ROW).unwrap();
    store.mark_finish_synced_on("confirmed", Some(at), Some(confirmed));
    store.note_finish_sent_row("confirmed", Some(at), sent);
    let rec = store.get("confirmed").unwrap();
    assert_eq!(rec.finish_coord_session_id, Some(confirmed), "still A");
    assert!(rec.finish_synced, "still synced");
}

/// F3 after a restart: a REAL registrar that resolves nothing, an unsynced
/// mark whose finish the drain dropped (its SENT row stored) — the boot sweep
/// re-offers it to that row over a NULL coord row, and not over `working`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sweep_reoffers_a_dropped_finish_to_its_sent_row_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
    let reg = AiCoordRegistrar::with_tenant_resolver(outbox.clone(), Uuid::new_v4(), || None);
    let store = Arc::new(SessionLifecycleStore::open(dir.path().join("lifecycle.json")).unwrap());
    reg.attach_lifecycle_store(store.clone());
    store.record_open(test_open_record("dropped"));
    let at = store
        .set_finished("dropped", true, Some("done".into()))
        .unwrap()
        .record
        .finished_at;
    store.note_finish_sent_row("dropped", at, row());
    let rec = store.get("dropped").unwrap();
    assert!(!rec.finish_synced);
    assert_eq!(
        reg.finish_row_for(&rec),
        None,
        "nothing resolves in-process"
    );
    drain_all(&outbox);

    let working = heal_sweep_with(&store, Some(&reg), None, true, |ids| async move {
        fetch_from_body_for_test(body(&ids[0], json!("working"), json!(BEFORE), ROW), 1)
    })
    .await;
    assert_eq!(
        working,
        vec![(
            "dropped".to_string(),
            HealOutcome::Left(HealSkip::WorkingRevivalUnprovable)
        )]
    );
    assert!(
        outbox.pending().unwrap().is_empty(),
        "nothing re-offered over working"
    );

    let null = heal_sweep_with(&store, Some(&reg), None, true, |ids| async move {
        null_rows(&ids)
    })
    .await;
    assert_eq!(
        null,
        vec![("dropped".to_string(), HealOutcome::Reoffered { row: row() })]
    );
    assert_eq!(
        rows(&outbox),
        vec![("finished".to_string(), "dropped".to_string())]
    );
    assert!(outbox
        .pending()
        .unwrap()
        .iter()
        .all(|r| r.session_id == row()));
}

/// A read whose input coord truncated: an id it served no row for may not
/// have been asked about, so it is unreadable, never `no_coord_row`.
#[test]
fn an_id_missing_from_a_truncated_read_is_unreadable() {
    let (_d, store, at) = synced_store("trunc");
    let p = port(&store);
    let fetch = fetch_from_body_for_test(
        json!({"statuses": {}, "unknown": [], "invalid": [],
               "input_truncated": true, "sessionBridgeColumnPresent": true}),
        1,
    );
    assert!(fetch.input_truncated);
    let out = apply_heal(&store, Some(&p), &store.all_records(), &fetch, at + 1_000);
    assert_eq!(out[0].1, HealOutcome::Left(HealSkip::CoordUnreadable));
    let whole = fetch_from_body_for_test(
        json!({"statuses": {}, "unknown": [], "invalid": [],
               "input_truncated": false, "sessionBridgeColumnPresent": true}),
        1,
    );
    let out = apply_heal(&store, Some(&p), &store.all_records(), &whole, at + 1_000);
    assert_eq!(out[0].1, HealOutcome::Left(HealSkip::NoCoordRow));
}

/// Review A6: the boot sweep does not re-offer an unsynced mark whose own
/// `Finished` row is still pending in the outbox — the drain still owes it,
/// and a second row would be a duplicate. Once that row has left the outbox
/// (dropped unconfirmed, say) the same mark IS re-offered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sweep_skips_a_mark_whose_finish_is_still_queued() {
    let (_d, outbox, reg, store) = wired();
    store.record_open(test_open_record("queued"));
    store
        .set_finished("queued", true, Some("done".into()))
        .unwrap();
    assert_eq!(
        rows(&outbox),
        vec![("finished".to_string(), "queued".to_string())],
        "the mark's own finish is pending"
    );

    let held = heal_sweep_with(
        &store,
        Some(reg.as_ref() as &dyn HealPort),
        None,
        true,
        |ids| async move { null_rows(&ids) },
    )
    .await;
    assert_eq!(
        held,
        vec![(
            "queued".to_string(),
            HealOutcome::Left(HealSkip::FinishStillQueued)
        )]
    );
    assert_eq!(rows(&outbox).len(), 1, "no duplicate queued beside it");

    drain_all(&outbox);
    let reoffered = heal_sweep_with(
        &store,
        Some(reg.as_ref() as &dyn HealPort),
        None,
        true,
        |ids| async move { null_rows(&ids) },
    )
    .await;
    assert_eq!(
        reoffered,
        vec![("queued".to_string(), HealOutcome::Reoffered { row: row() })]
    );
    assert_eq!(
        rows(&outbox),
        vec![("finished".to_string(), "queued".to_string())]
    );
}

// ---------------------------------------------------------------------------
// Re-address (plan
// `2026-11-07-a-runner-finish-is-addressed-to-a-stale-coord-row-after-a-pane-respawn`,
// Phase 4): a finish written to a different row than the one coord names
// ---------------------------------------------------------------------------

fn iso(ms: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap()
        .to_rfc3339()
}

/// A body whose row is [`OTHER_ROW`] with the given state and `started_at`.
fn other_body(
    csid: &str,
    status: serde_json::Value,
    since: serde_json::Value,
    state: &str,
    started_at: Option<&str>,
) -> serde_json::Value {
    let mut row = json!({"session_status": status, "since": since,
                         "coord_session_id": OTHER_ROW, "state": state});
    if let Some(s) = started_at {
        row["started_at"] = json!(s);
    }
    json!({
        "statuses": {csid: row},
        "unknown": [], "invalid": [], "input_truncated": false,
        "sessionBridgeColumnPresent": true
    })
}

#[test]
fn a_different_active_null_row_that_predates_the_finish_is_readdressed() {
    assert_eq!(
        decide(&other(None, None, Some("active"), Some(BEFORE))),
        HealDecision::Readdress { row: other_row() }
    );
    assert_eq!(
        decide(&other(None, Some(BEFORE), Some("active"), Some(BEFORE))),
        HealDecision::Readdress { row: other_row() },
        "a since before the finish is no revival"
    );
}

#[test]
fn every_other_different_row_is_left_and_counted_by_reason() {
    let cases = [
        (
            other(None, None, Some("closed"), Some(BEFORE)),
            HealSkip::DifferentRowNotActive,
        ),
        (
            other(None, None, None, Some(BEFORE)),
            HealSkip::DifferentRowStateUnknown,
        ),
        (
            other(Some("working"), None, Some("active"), Some(BEFORE)),
            HealSkip::DifferentRowStatusNotNull,
        ),
        (
            other(Some("finished"), Some(AFTER), Some("active"), Some(BEFORE)),
            HealSkip::DifferentRowStatusNotNull,
        ),
        (
            other(None, Some(AFTER), Some("active"), Some(BEFORE)),
            HealSkip::DifferentRowRevived,
        ),
        (
            other(None, Some("not-a-time"), Some("active"), Some(BEFORE)),
            HealSkip::SinceUnreadable,
        ),
        (
            other(None, None, Some("active"), Some(AFTER)),
            HealSkip::DifferentRowStartedAfterFinish,
        ),
        (
            other(None, None, Some("active"), None),
            HealSkip::DifferentRowStartedUnknown,
        ),
        (
            other(None, None, Some("active"), Some("yesterday")),
            HealSkip::DifferentRowStartedUnknown,
        ),
    ];
    for (obs, skip) in cases {
        assert_eq!(decide(&obs), HealDecision::Leave(skip), "{obs:?}");
    }
    // A row registered AT the finish instant is not before it.
    let at = iso(FINISHED_AT);
    assert_eq!(
        decide(&other(None, None, Some("active"), Some(&at))),
        HealDecision::Leave(HealSkip::DifferentRowStartedAfterFinish)
    );
}

#[test]
fn a_synced_mark_on_a_stale_row_is_readdressed_to_the_live_row_once() {
    let (_d, store, at) = synced_store("re-live");
    let p = port(&store);
    let started = iso(at - 3_600_000);
    let fetch = fetch_from_body_for_test(
        other_body(
            "re-live",
            json!(null),
            json!(null),
            "active",
            Some(&started),
        ),
        1,
    );
    let out = apply_heal(&store, Some(&p), &store.all_records(), &fetch, at + 1_000);
    assert_eq!(
        out,
        vec![(
            "re-live".to_string(),
            HealOutcome::Readdressed {
                from: row(),
                row: other_row()
            }
        )]
    );
    assert!(!store.get("re-live").unwrap().finish_synced);
    assert_eq!(
        store.get("re-live").unwrap().finish_coord_session_id,
        Some(other_row()),
        "the re-address target is recorded as the mark's sent row"
    );
    assert_eq!(
        *p.sent.lock().unwrap(),
        vec![("re-live".to_string(), at, other_row())],
        "addressed to the row coord OBSERVED, not the mark's own"
    );

    // The read-back arm again: the mark is unsynced, so nothing.
    let again = apply_heal(&store, Some(&p), &store.all_records(), &fetch, at + 2_000);
    assert!(again.is_empty(), "{again:?}");
    // The boot sweep again: the re-addressed Finished row is still queued.
    let ids = vec!["re-live".to_string()];
    let boot = apply_heal_observed(
        &store,
        Some(&p),
        &store.all_records(),
        &Observations::from_fetch(&ids, &fetch),
        at + 3_000,
        true,
    );
    assert_eq!(boot[0].1, HealOutcome::Left(HealSkip::FinishStillQueued));
    assert_eq!(
        p.sent.lock().unwrap().len(),
        1,
        "one re-send across three sweeps"
    );

    // The drain's ack on the observed row stamps it as the mark's own, so a
    // later read of that row reading finished is the healthy case.
    store.mark_finish_synced_on("re-live", Some(at), Some(other_row()));
    let rec = store.get("re-live").unwrap();
    assert!(rec.finish_synced);
    assert_eq!(rec.finish_coord_session_id, Some(other_row()));
    let finished = fetch_from_body_for_test(
        other_body(
            "re-live",
            json!("finished"),
            json!(iso(at + 500)),
            "active",
            Some(&started),
        ),
        1,
    );
    let out = apply_heal(
        &store,
        Some(&p),
        &store.all_records(),
        &finished,
        at + 4_000,
    );
    assert_eq!(out[0].1, HealOutcome::Left(HealSkip::CoordFinished));
}

#[test]
fn a_different_row_that_is_closed_revived_or_newer_is_untouched() {
    let (_d, store, at) = synced_store("re-no");
    let p = port(&store);
    let before = iso(at - 3_600_000);
    let after = iso(at + 3_600_000);
    for (b, skip) in [
        (
            other_body("re-no", json!(null), json!(null), "closed", Some(&before)),
            HealSkip::DifferentRowNotActive,
        ),
        (
            other_body("re-no", json!(null), json!(after), "active", Some(&before)),
            HealSkip::DifferentRowRevived,
        ),
        (
            other_body("re-no", json!(null), json!(null), "active", Some(&after)),
            HealSkip::DifferentRowStartedAfterFinish,
        ),
        (
            other_body("re-no", json!(null), json!(null), "active", None),
            HealSkip::DifferentRowStartedUnknown,
        ),
    ] {
        let fetch = fetch_from_body_for_test(b, 1);
        let out = apply_heal(&store, Some(&p), &store.all_records(), &fetch, at + 1_000);
        assert_eq!(out[0].1, HealOutcome::Left(skip));
        assert!(store.get("re-no").unwrap().finish_synced, "{skip:?}");
    }
    assert!(p.sent.lock().unwrap().is_empty());
}

/// An unsynced mark whose `Finished` row is still pending to its OWN row is
/// not re-addressed: the drain delivers that one first.
#[test]
fn an_unsynced_mark_still_queued_to_its_own_row_is_not_readdressed() {
    let (_d, store, at) = unsynced_store("re-queued");
    let p = port(&store);
    p.sent
        .lock()
        .unwrap()
        .push(("re-queued".to_string(), at, row()));
    let started = iso(at - 3_600_000);
    let ids = vec!["re-queued".to_string()];
    let obs = Observations::from_fetch(
        &ids,
        &fetch_from_body_for_test(
            other_body(
                "re-queued",
                json!(null),
                json!(null),
                "active",
                Some(&started),
            ),
            1,
        ),
    );
    let out = apply_heal_observed(
        &store,
        Some(&p),
        &store.all_records(),
        &obs,
        at + 1_000,
        true,
    );
    assert_eq!(out[0].1, HealOutcome::Left(HealSkip::FinishStillQueued));
    assert_eq!(p.sent.lock().unwrap().len(), 1);

    p.sent.lock().unwrap().clear();
    let out = apply_heal_observed(
        &store,
        Some(&p),
        &store.all_records(),
        &obs,
        at + 1_000,
        true,
    );
    assert_eq!(
        out[0].1,
        HealOutcome::Readdressed {
            from: row(),
            row: other_row()
        }
    );
}

#[test]
fn a_read_back_over_a_stale_row_readdresses_and_reports_the_live_row() {
    let (_d, store, at) = synced_store("rb-re");
    let p = port(&store);
    let mut resp = read_back_response(&store, "rb-re");
    let started = iso(at - 3_600_000);
    let fetch = fetch_from_body_for_test(
        other_body("rb-re", json!(null), json!(null), "active", Some(&started)),
        1,
    );
    let now = chrono::DateTime::<Utc>::from_timestamp_millis(at + 5_000).unwrap();
    let rb = read_back_from(&store, Some(&p), "rb-re", &fetch, now);
    decorate_read_back(&mut resp, &rb, store.get("rb-re").as_ref());
    assert_eq!(resp["session"]["finishSynced"], false);
    assert_eq!(resp["coord"]["heal"]["outcome"], "readdressed");
    assert_eq!(resp["coord"]["queued"], true);
    assert_eq!(resp["coord"]["coordSessionId"], OTHER_ROW);
    assert_eq!(resp["coord"]["observed"]["state"], "active");
    assert_eq!(resp["coord"]["observed"]["startedAt"], started);
}

/// Review A1: after a re-address, a reason-less re-run of the finish (the
/// retry door) sends to the re-addressed row, never to the stale row the
/// in-process resolvers still name — whose ack would stamp the mark synced on
/// the stale row — even after the drain drops the re-addressed finish
/// unconfirmed.
#[test]
fn the_retry_door_does_not_resend_to_the_stale_row_after_a_readdress() {
    let (_d, outbox, reg, store) = wired();
    store.record_open(test_open_record("re-retry"));
    let at = store
        .set_finished("re-retry", true, Some("done".into()))
        .unwrap()
        .record
        .finished_at
        .unwrap();
    // The first finish went to the stale row and was confirmed there.
    store.mark_finish_synced_on("re-retry", Some(at), Some(row()));
    drain_all(&outbox);

    let started = iso(at - 3_600_000);
    let fetch = fetch_from_body_for_test(
        other_body(
            "re-retry",
            json!(null),
            json!(null),
            "active",
            Some(&started),
        ),
        1,
    );
    let out = apply_heal(
        &store,
        Some(reg.as_ref() as &dyn HealPort),
        &store.all_records(),
        &fetch,
        at + 1_000,
    );
    assert_eq!(
        out[0].1,
        HealOutcome::Readdressed {
            from: row(),
            row: other_row()
        }
    );
    let rec = store.get("re-retry").unwrap();
    assert!(!rec.finish_synced);
    assert_eq!(
        rec.finish_coord_session_id,
        Some(other_row()),
        "the re-address target is the mark's sent row"
    );
    assert!(rec.finish_coord_readdressed);
    let finished_to = |outbox: &OutboxWriter| -> Vec<Uuid> {
        outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Finished.as_str())
            .map(|r| r.session_id)
            .collect()
    };
    assert_eq!(finished_to(&outbox), vec![other_row()]);

    // The retry door (a reason-less re-run): the terminal lookup still names
    // the stale row, but the finish goes to the re-addressed one.
    let again = store.set_finished("re-retry", true, None).unwrap();
    assert_eq!(
        again.coord,
        FinishSync::Queued {
            coord_session_id: other_row()
        }
    );
    assert_eq!(finished_to(&outbox), vec![other_row(), other_row()]);

    // The drain gives up on the re-addressed finish unconfirmed (it records
    // the row it sent to — the same row): the retry door still goes there.
    drain_all(&outbox);
    store.note_finish_sent_row("re-retry", Some(at), other_row());
    assert!(store.get("re-retry").unwrap().finish_coord_readdressed);
    store.set_finished("re-retry", true, None).unwrap();
    assert_eq!(
        finished_to(&outbox),
        vec![other_row()],
        "never back to the stale row"
    );

    // An unmark clears the re-address with the mark.
    store.set_finished("re-retry", false, None).unwrap();
    let rec = store.get("re-retry").unwrap();
    assert!(!rec.finish_coord_readdressed);
    assert_eq!(rec.finish_coord_session_id, None);
}

/// The re-address flag is recorded only on the matching unsynced mark, and a
/// DIFFERENT sent row recorded later clears it.
#[test]
fn the_readdress_flag_follows_only_its_own_row() {
    let (_d, store, at) = unsynced_store("flag");
    assert!(!store.note_finish_readdressed("flag", at + 1, other_row(), false, None));
    assert!(!store.note_finish_readdressed("ghost", at, other_row(), false, None));
    assert!(store.note_finish_readdressed(
        "flag",
        at,
        other_row(),
        false,
        store.get("flag").unwrap().finish_coord_session_id,
    ));
    let rec = store.get("flag").unwrap();
    assert!(rec.finish_coord_readdressed);
    assert_eq!(rec.finish_coord_session_id, Some(other_row()));

    store.note_finish_sent_row("flag", Some(at), row());
    let rec = store.get("flag").unwrap();
    assert!(
        !rec.finish_coord_readdressed,
        "another sent row is not the re-addressed one"
    );
    assert_eq!(rec.finish_coord_session_id, Some(row()));

    // A synced mark has its stamp withdrawn in the same step.
    let (_d2, synced, sat) = synced_store("flag-synced");
    assert!(synced.note_finish_readdressed(
        "flag-synced",
        sat,
        other_row(),
        true,
        synced.get("flag-synced").unwrap().finish_coord_session_id,
    ));
    // A second heal over the same synced snapshot is refused: one re-send.
    assert!(!synced.note_finish_readdressed(
        "flag-synced",
        sat,
        other_row(),
        true,
        synced.get("flag-synced").unwrap().finish_coord_session_id,
    ));
    let rec = synced.get("flag-synced").unwrap();
    assert!(!rec.finish_synced);
    assert!(rec.finish_coord_readdressed);

    // A different row confirming the finish clears the flag; the same row
    // keeps it.
    synced.mark_finish_synced_on("flag-synced", Some(sat), Some(other_row()));
    assert!(synced.get("flag-synced").unwrap().finish_coord_readdressed);
    assert!(synced.note_finish_readdressed(
        "flag-synced",
        sat,
        other_row(),
        true,
        synced.get("flag-synced").unwrap().finish_coord_session_id,
    ));
    synced.mark_finish_synced_on("flag-synced", Some(sat), Some(row()));
    let rec = synced.get("flag-synced").unwrap();
    assert!(!rec.finish_coord_readdressed);
    assert_eq!(rec.finish_coord_session_id, Some(row()));
}
