// Drain-level tests for the observed finish ACK (plan
// `2026-11-05-finish-synced-is-stamped-on-a-2xx-that-did-not-move-session-status`,
// Phase 4): a `finished` row is stamped synced only when coord's own answer
// reads `finished`. A child of `coord_sync::tests`, so it drives the real
// `drain_tick` against the same fake coord, without growing `coord_sync.rs`.

use super::*;
use crate::session::coord_sync_finish_ack::{
    unconfirmed_message, FINISHED_UNCONFIRMED_DROPPED, FINISHED_UNCONFIRMED_KIND,
    FINISHED_UNCONFIRMED_MAX_ATTEMPTS,
};

type Acks = Arc<std::sync::Mutex<Vec<(String, Option<i64>)>>>;

/// A drain wired to the fake coord, with the finished-ACK observer recording
/// every stamp it is asked to make.
async fn finish_rig(
    answer: Option<(u16, &str)>,
) -> (
    tempfile::TempDir,
    Arc<OutboxWriter>,
    CoordSync,
    Arc<TokMutex<CoordRecorder>>,
    Acks,
) {
    let dir = tempfile::tempdir().unwrap();
    let outbox = build_outbox(dir.path());
    let (base, rec) = spawn_fake_coord().await;
    rec.lock().await.finished_answer = answer.map(|(c, b)| (c, b.to_string()));
    let coord = CoordSync::new_for_test(
        outbox.clone(),
        base,
        Duration::from_millis(100),
        Duration::from_secs(60),
    );
    let acks: Acks = Default::default();
    {
        let acks = acks.clone();
        coord.attach_finished_ack_observer(move |csid, at| {
            acks.lock().unwrap().push((csid.to_string(), at))
        });
    }
    (dir, outbox, coord, rec, acks)
}

fn queue_finish(outbox: &OutboxWriter, session: Uuid, csid: &str, at: i64) {
    // One machine id for every row, so the outbox's seq counter is shared and
    // two rows of one session never collide on (session, seq).
    outbox
        .record(
            Uuid::from_u128(0xeb2155ed_4152_4a91_be82_5d4346f717fc),
            session,
            SessionEventKind::Finished,
            json!({ "id": session, "claude_session_id": csid, "finished_at": at }),
        )
        .unwrap();
}

/// Let `session` retry now instead of sitting out its backoff.
fn retry_now(state: &mut DrainState, session: Uuid) {
    if let Some(r) = state.retry.get_mut(&session) {
        r.next_attempt_at = Instant::now();
    }
}

fn kinds(rows: &[(i64, String)]) -> Vec<&str> {
    rows.iter().map(|(_, k)| k.as_str()).collect()
}

fn finished_patches(rec: &CoordRecorder, session: Uuid) -> usize {
    rec.patches
        .iter()
        .filter(|(id, b)| *id == session && b["progress"]["session_status"] == "finished")
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_2xx_whose_row_reads_finished_stamps_the_mark() {
    let (_d, outbox, coord, _rec, acks) = finish_rig(Some((
        200,
        r#"{"id":"01a1ab84-1c55-7000-8000-000000000001","session_status":"finished"}"#,
    )))
    .await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-observed", 1_793_880_342_000);
    let mut state = DrainState::default();
    drain_tick(&coord.inner, &mut state).await;
    assert_eq!(
        *acks.lock().unwrap(),
        vec![("csid-observed".to_string(), Some(1_793_880_342_000))]
    );
    assert!(pending_of(&outbox, s).is_empty(), "the row left the outbox");
    assert!(!state.retry.contains_key(&s));
}

/// The defect's own shape: 2xx, but coord's re-hydrated row still reads the
/// old status.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_2xx_whose_row_reads_working_is_not_stamped_and_the_row_is_kept() {
    let (_d, outbox, coord, rec, acks) = finish_rig(Some((
        200,
        r#"{"id":"01a1ab84-1c55-7000-8000-000000000001","session_status":"working"}"#,
    )))
    .await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-working", 42);
    let mut state = DrainState::default();
    let tick = drain_tick(&coord.inner, &mut state).await;
    assert!(acks.lock().unwrap().is_empty(), "never stamped");
    let kept = pending_of(&outbox, s);
    assert_eq!(kinds(&kept), vec!["finished"], "the finish stays queued");
    let retry = state.retry.get(&s).expect("the session is on its own backoff");
    assert_eq!(retry.failures, 1);
    assert_eq!(retry.serving_failures, 0, "never charged to the quarantine");
    assert_eq!(retry.unconfirmed_finish, Some(((s, kept[0].0), 1)));
    assert!(!tick.outage, "coord answered: not an outage");
    assert_eq!(finished_patches(&*rec.lock().await, s), 1);
}

/// coord's pre-migration shape skips serializing an unset axis: an absent
/// `session_status` is UNKNOWN, never `finished`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_2xx_with_no_session_status_is_not_stamped_and_the_row_is_kept() {
    let (_d, outbox, coord, _rec, acks) = finish_rig(Some((
        200,
        r#"{"id":"01a1ab84-1c55-7000-8000-000000000001"}"#,
    )))
    .await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-absent", 42);
    let mut state = DrainState::default();
    drain_tick(&coord.inner, &mut state).await;
    assert!(acks.lock().unwrap().is_empty(), "never stamped");
    assert_eq!(kinds(&pending_of(&outbox, s)), vec!["finished"]);
    assert!(state.retry.contains_key(&s));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn five_unconfirmed_answers_drop_the_row_unstamped_and_count_it() {
    let (_d, outbox, coord, rec, acks) =
        finish_rig(Some((200, r#"{"session_status":"working"}"#))).await;
    let sent: Arc<std::sync::Mutex<Vec<(String, Option<i64>, Uuid)>>> = Default::default();
    {
        let sent = sent.clone();
        coord.attach_finished_unconfirmed_observer(move |csid, at, row| {
            sent.lock().unwrap().push((csid.to_string(), at, row))
        });
    }
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-bounded", 42);
    let before = FINISHED_UNCONFIRMED_DROPPED.load(Ordering::Relaxed);
    let mut state = DrainState::default();
    for attempt in 1..FINISHED_UNCONFIRMED_MAX_ATTEMPTS {
        drain_tick(&coord.inner, &mut state).await;
        assert_eq!(
            pending_of(&outbox, s).len(),
            1,
            "still queued after unconfirmed answer {attempt}"
        );
        retry_now(&mut state, s);
    }
    drain_tick(&coord.inner, &mut state).await;
    assert_eq!(FINISHED_UNCONFIRMED_MAX_ATTEMPTS, 5);
    assert!(
        pending_of(&outbox, s).is_empty(),
        "dropped after the fifth unconfirmed answer"
    );
    assert_eq!(finished_patches(&*rec.lock().await, s), 5);
    assert!(acks.lock().unwrap().is_empty(), "dropped UNSTAMPED");
    assert_eq!(
        *sent.lock().unwrap(),
        vec![("csid-bounded".to_string(), Some(42), s)],
        "the drop hands over the row the finish was SENT to, once"
    );
    assert!(!state.quarantined.contains(&s), "the bound fired, not the quarantine");
    assert!(!state.retry.contains_key(&s));
    // Process-global: only this test drives a finish to its bound.
    assert_eq!(FINISHED_UNCONFIRMED_DROPPED.load(Ordering::Relaxed) - before, 1);
    let health = render_session_outbox_health(&SessionOutboxHealth {
        observed_at: Some(Utc::now()),
        finished_unconfirmed: 1,
        ..Default::default()
    });
    assert_eq!(health["finishedUnconfirmed"], 1);
}

/// Phase 3's coord answer to a swallowed progress write is an ordinary
/// retryable 5xx.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_503_progress_write_not_applied_is_ordinary_transport() {
    let (_d, outbox, coord, _rec, acks) = finish_rig(Some((
        503,
        r#"{"error":"progress_write_not_applied","detail":"session 01a1ab84-1c55-7000-8000-000000000001"}"#,
    )))
    .await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-503", 42);
    let mut state = DrainState::default();
    drain_tick(&coord.inner, &mut state).await;
    assert!(acks.lock().unwrap().is_empty());
    assert_eq!(kinds(&pending_of(&outbox, s)), vec!["finished"]);
    let retry = state.retry.get(&s).expect("retried on the session backoff");
    assert_eq!(retry.unconfirmed_finish, None, "a 5xx is not the unconfirmed bound");
}

/// Plan 2026-11-01's refusal: dropped, and never reported as synced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_422_stale_finish_refused_is_dropped_unstamped() {
    let (_d, outbox, coord, _rec, acks) = finish_rig(Some((
        422,
        r#"{"error":"stale_finish_refused","detail":"the row was revived after this finish"}"#,
    )))
    .await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-422", 42);
    let mut state = DrainState::default();
    drain_tick(&coord.inner, &mut state).await;
    assert!(acks.lock().unwrap().is_empty(), "a refusal is never a delivery");
    assert!(pending_of(&outbox, s).is_empty(), "the 4xx is ACK-dropped");
}

/// The same confirmed finish delivered twice stamps the mark once: the second
/// observed ACK finds it already synced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_confirmed_finish_twice_is_stamped_once() {
    use crate::session::session_lifecycle_store::{test_open_record, SessionLifecycleStore};
    let dir = tempfile::tempdir().unwrap();
    let outbox = build_outbox(dir.path());
    let (base, _rec) = spawn_fake_coord().await;
    let coord = CoordSync::new_for_test(
        outbox.clone(),
        base,
        Duration::from_millis(100),
        Duration::from_secs(60),
    );
    let store = Arc::new(SessionLifecycleStore::open(dir.path().join("lifecycle.json")).unwrap());
    store.record_open(test_open_record("csid-twice"));
    let at = store
        .set_finished("csid-twice", true, Some("done".into()))
        .unwrap()
        .record
        .finished_at
        .unwrap();
    let stamps: Arc<std::sync::Mutex<Vec<bool>>> = Default::default();
    {
        let store = store.clone();
        let stamps = stamps.clone();
        coord.attach_finished_ack_observer(move |csid, finished_at| {
            let before = store.get(csid).unwrap().finish_synced;
            store.mark_finish_synced(csid, finished_at);
            let after = store.get(csid).unwrap().finish_synced;
            stamps.lock().unwrap().push(!before && after);
        });
    }
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-twice", at);
    queue_finish(&outbox, s, "csid-twice", at);
    let mut state = DrainState::default();
    drain_tick(&coord.inner, &mut state).await;
    assert!(pending_of(&outbox, s).is_empty());
    assert_eq!(
        *stamps.lock().unwrap(),
        vec![true, false],
        "the first observed ACK stamps, the second finds it synced"
    );
    let rec = store.get("csid-twice").unwrap();
    assert!(rec.finish_synced);
    assert_eq!(rec.finished_at, Some(at));
}

#[test]
fn an_unconfirmed_finish_is_classified_as_its_own_kind() {
    assert_eq!(
        classify_push_failure(
            &unconfirmed_message(200, "coord's row reads session_status \"working\""),
            false
        ),
        (FINISHED_UNCONFIRMED_KIND, Some(200))
    );
    assert_eq!(
        classify_push_failure("503 Service Unavailable: {}", false),
        ("server_error", Some(503))
    );
}

/// Review: an unconfirmed finish blocks only ITS session's chain — another
/// session's rows drain on the same tick, and the tick is not an outage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfirmed_finish_on_one_session_does_not_stop_another_from_draining() {
    let (_d, outbox, coord, rec, acks) =
        finish_rig(Some((200, r#"{"session_status":"working"}"#))).await;
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    queue_finish(&outbox, a, "csid-a", 42);
    record_started(&outbox, Uuid::new_v4(), b);
    let mut state = DrainState::default();
    let tick = drain_tick(&coord.inner, &mut state).await;
    assert_eq!(kinds(&pending_of(&outbox, a)), vec!["finished"], "A is kept");
    assert!(pending_of(&outbox, b).is_empty(), "B drained on the same tick");
    assert!(
        rec.lock().await.posts.iter().any(|p| p["id"] == json!(b)),
        "B's started row reached coord"
    );
    assert!(!tick.outage, "coord answered A: not an outage");
    assert!(state.retry.contains_key(&a));
    assert!(!state.retry.contains_key(&b));
    assert!(acks.lock().unwrap().is_empty());
}

/// Review F2: the observed ACK hands the observer the row that answered, and
/// the stamp records it on the mark.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_observed_ack_records_the_answering_row_on_the_mark() {
    use crate::session::session_lifecycle_store::{test_open_record, SessionLifecycleStore};
    let dir = tempfile::tempdir().unwrap();
    let outbox = build_outbox(dir.path());
    let (base, _rec) = spawn_fake_coord().await;
    let coord = CoordSync::new_for_test(
        outbox.clone(),
        base,
        Duration::from_millis(100),
        Duration::from_secs(60),
    );
    let store = Arc::new(SessionLifecycleStore::open(dir.path().join("lifecycle.json")).unwrap());
    store.record_open(test_open_record("csid-row"));
    let at = store
        .set_finished("csid-row", true, Some("done".into()))
        .unwrap()
        .record
        .finished_at
        .unwrap();
    {
        let store = store.clone();
        coord.attach_finished_ack_observer_on(move |csid, finished_at, row| {
            store.mark_finish_synced_on(csid, finished_at, Some(row));
        });
    }
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-row", at);
    let mut state = DrainState::default();
    drain_tick(&coord.inner, &mut state).await;
    let rec = store.get("csid-row").unwrap();
    assert!(rec.finish_synced);
    assert_eq!(rec.finish_coord_session_id, Some(s), "the row the PATCH addressed");
}
