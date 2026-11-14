// Drain-level tests for the observed finish ACK (plan
// `2026-11-05-finish-synced-is-stamped-on-a-2xx-that-did-not-move-session-status`,
// Phase 4): a `finished` row is stamped synced only when coord's own answer
// reads `finished`. A child of `coord_sync::tests`, so it drives the real
// `drain_tick` against the same fake coord, without growing `coord_sync.rs`.

use super::*;
use crate::session::coord_sync_finish_ack::{
    unconfirmed_message, FINISHED_UNCONFIRMED_DROPPED, FINISHED_UNCONFIRMED_KIND,
    FINISHED_UNCONFIRMED_MAX_ATTEMPTS, FINISHED_UNCONFIRMED_MIN_AGE,
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

/// Backdate `session`'s first unconfirmed answer past the age floor, as if
/// it had been retried for [`FINISHED_UNCONFIRMED_MIN_AGE`].
fn age_unconfirmed(state: &mut DrainState, session: Uuid) {
    let u = state
        .retry
        .get_mut(&session)
        .and_then(|r| r.unconfirmed_finish.as_mut())
        .expect("an unconfirmed finish is being counted");
    u.first_at = Instant::now()
        .checked_sub(FINISHED_UNCONFIRMED_MIN_AGE + Duration::from_secs(1))
        .expect("the monotonic clock is older than the age floor");
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
    let retry = state
        .retry
        .get(&s)
        .expect("the session is on its own backoff");
    assert_eq!(retry.failures, 1);
    assert_eq!(retry.serving_failures, 0, "never charged to the quarantine");
    let u = retry.unconfirmed_finish.expect("counted against the bound");
    assert_eq!((u.row, u.attempts), ((s, kept[0].0), 1));
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

/// A PATCH 409 ("row gone or already in target state") is not a finish
/// either: without a row reading `finished` in its body it is unconfirmed,
/// kept and retried — never stamped as the old idempotent-409 ACK did.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_409_on_the_finished_patch_is_not_stamped_and_the_row_is_kept() {
    let (_d, outbox, coord, _rec, acks) = finish_rig(Some((409, r#"{"error":"conflict"}"#))).await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-409", 42);
    let mut state = DrainState::default();
    drain_tick(&coord.inner, &mut state).await;
    assert!(acks.lock().unwrap().is_empty(), "never stamped");
    assert_eq!(kinds(&pending_of(&outbox, s)), vec!["finished"]);
    let retry = state.retry.get(&s).expect("on its own backoff");
    assert_eq!(retry.serving_failures, 0, "never charged to the quarantine");
    assert!(
        retry.unconfirmed_finish.is_some(),
        "counted against the bound"
    );
}

/// …while a 409 whose body carries the row reading `finished` is observed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_409_whose_body_reads_finished_stamps_the_mark() {
    let (_d, outbox, coord, _rec, acks) = finish_rig(Some((
        409,
        r#"{"id":"01a1ab84-1c55-7000-8000-000000000001","session_status":"finished"}"#,
    )))
    .await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-409-finished", 42);
    let mut state = DrainState::default();
    drain_tick(&coord.inner, &mut state).await;
    assert_eq!(
        *acks.lock().unwrap(),
        vec![("csid-409-finished".to_string(), Some(42))]
    );
    assert!(pending_of(&outbox, s).is_empty());
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
    // Both bounds: the age floor has passed too.
    age_unconfirmed(&mut state, s);
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
    assert!(
        !state.quarantined.contains(&s),
        "the bound fired, not the quarantine"
    );
    assert!(!state.retry.contains_key(&s));
    // Process-global: only this test drives a finish to its bound.
    assert_eq!(
        FINISHED_UNCONFIRMED_DROPPED.load(Ordering::Relaxed) - before,
        1
    );
    let health = render_session_outbox_health(&SessionOutboxHealth {
        observed_at: Some(Utc::now()),
        finished_unconfirmed: 1,
        ..Default::default()
    });
    assert_eq!(health["finishedUnconfirmed"], 1);
}

/// Review A1: the attempt count alone never drops a finish. Five (and more)
/// unconfirmed answers inside the age floor — about thirty seconds on the
/// session backoff — keep the row; it goes only once the first answer is
/// [`FINISHED_UNCONFIRMED_MIN_AGE`] old.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unconfirmed_answers_inside_the_age_floor_never_drop_the_finish() {
    let (_d, outbox, coord, rec, acks) =
        finish_rig(Some((200, r#"{"session_status":"working"}"#))).await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-floor", 42);
    let mut state = DrainState::default();
    let answers = FINISHED_UNCONFIRMED_MAX_ATTEMPTS + 3;
    for attempt in 1..=answers {
        drain_tick(&coord.inner, &mut state).await;
        assert_eq!(
            kinds(&pending_of(&outbox, s)),
            vec!["finished"],
            "still queued after unconfirmed answer {attempt}, inside the age floor"
        );
        retry_now(&mut state, s);
    }
    let u = state.retry[&s].unconfirmed_finish.expect("still counted");
    assert_eq!(u.attempts, answers, "the count kept climbing");
    assert_eq!(finished_patches(&*rec.lock().await, s), answers as usize);

    age_unconfirmed(&mut state, s);
    drain_tick(&coord.inner, &mut state).await;
    assert!(
        pending_of(&outbox, s).is_empty(),
        "dropped once both bounds hold"
    );
    assert!(acks.lock().unwrap().is_empty(), "dropped UNSTAMPED");
}

/// Review A2: an already-quarantined session's finished row is a held
/// lifecycle row; when coord keeps answering it unconfirmed it still reaches
/// the bound and is dropped unstamped — never retried forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quarantined_sessions_unconfirmed_finish_still_reaches_the_bound() {
    let (_d, outbox, coord, _rec, acks) =
        finish_rig(Some((200, r#"{"session_status":"working"}"#))).await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-quarantined", 42);
    let mut state = DrainState::default();
    state.quarantined.insert(s);
    for attempt in 1..FINISHED_UNCONFIRMED_MAX_ATTEMPTS {
        drain_tick(&coord.inner, &mut state).await;
        assert_eq!(
            kinds(&pending_of(&outbox, s)),
            vec!["finished"],
            "held after unconfirmed answer {attempt}"
        );
        let r = &state.retry[&s];
        assert_eq!(
            r.unconfirmed_finish.map(|u| u.attempts),
            Some(attempt),
            "a quarantined session's unconfirmed answers are counted"
        );
        assert!(
            r.next_attempt_at > Instant::now() + MAX_BACKOFF - Duration::from_secs(5),
            "still retried once a minute while quarantined"
        );
        retry_now(&mut state, s);
    }
    age_unconfirmed(&mut state, s);
    drain_tick(&coord.inner, &mut state).await;
    assert!(
        pending_of(&outbox, s).is_empty(),
        "the bound dropped the quarantined session's finish"
    );
    assert!(acks.lock().unwrap().is_empty(), "dropped UNSTAMPED");
    assert!(
        state.quarantined.contains(&s),
        "the drop does not lift the quarantine"
    );
}

/// Review A3, as split by the re-review: an unconfirmed 2xx is coord
/// ANSWERING — no outage, `has_been_online` — but not a row coord TOOK, so
/// `last_ack` / `ack_ticks` (the quarantine's "coord is serving rows"
/// evidence) do not move.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfirmed_answer_is_reachability_not_a_taken_row() {
    let (_d, outbox, coord, _rec, acks) =
        finish_rig(Some((200, r#"{"session_status":"working"}"#))).await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-serving", 42);
    let mut state = DrainState::default();
    assert!(!coord.inner.has_been_online.load(Ordering::Relaxed));
    let tick = drain_tick(&coord.inner, &mut state).await;
    assert!(!tick.outage, "coord answered: not an outage");
    assert!(coord.inner.has_been_online.load(Ordering::Relaxed));
    assert_eq!(state.last_ack, None, "coord took no row");
    assert_eq!(state.ack_ticks, 0, "coord took no row");
    assert!(acks.lock().unwrap().is_empty(), "never stamped");
    assert_eq!(kinds(&pending_of(&outbox, s)), vec!["finished"]);
}

/// Re-review 1: a session blocked on an unconfirmed finish (held for at least
/// the 10-minute floor, which is coord's Active→Stale threshold) still gets
/// its newest heartbeat out as a front keep-alive, inside its own backoff,
/// while the finish stays queued.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_held_on_an_unconfirmed_finish_keeps_its_heartbeat_flowing() {
    let (_d, outbox, coord, rec, acks) =
        finish_rig(Some((200, r#"{"session_status":"working"}"#))).await;
    let s = Uuid::new_v4();
    queue_finish(&outbox, s, "csid-keepalive", 42);
    let mut state = DrainState::default();
    drain_tick(&coord.inner, &mut state).await;
    assert!(state.retry[&s].unconfirmed_finish.is_some());
    assert!(
        state.retry[&s].next_attempt_at > Instant::now(),
        "the finish is inside its backoff"
    );

    let machine = Uuid::from_u128(0xeb2155ed_4152_4a91_be82_5d4346f717fc);
    for _ in 0..2 {
        outbox
            .record(machine, s, SessionEventKind::Heartbeat, json!({ "id": s }))
            .unwrap();
    }
    let patches_before = rec.lock().await.patches.len();
    drain_tick(&coord.inner, &mut state).await;

    assert_eq!(
        kinds(&pending_of(&outbox, s)),
        vec!["finished"],
        "the heartbeats left the queue (newest sent, older superseded); the finish is still held"
    );
    let recorder = rec.lock().await;
    let sent: Vec<_> = recorder.patches[patches_before..]
        .iter()
        .filter(|(id, _)| *id == s)
        .collect();
    assert_eq!(sent.len(), 1, "exactly the newest heartbeat went out: {sent:?}");
    assert_ne!(
        sent[0].1["progress"]["session_status"], "finished",
        "the finish was not retried inside its backoff"
    );
    drop(recorder);
    assert!(acks.lock().unwrap().is_empty(), "never stamped");
    assert!(
        state.retry[&s].unconfirmed_finish.is_some(),
        "the keep-alive leaves the finish's retry state alone"
    );
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
    assert_eq!(
        retry.unconfirmed_finish, None,
        "a 5xx is not the unconfirmed bound"
    );
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
    assert!(
        acks.lock().unwrap().is_empty(),
        "a refusal is never a delivery"
    );
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
    assert_eq!(
        kinds(&pending_of(&outbox, a)),
        vec!["finished"],
        "A is kept"
    );
    assert!(
        pending_of(&outbox, b).is_empty(),
        "B drained on the same tick"
    );
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
    assert_eq!(
        rec.finish_coord_session_id,
        Some(s),
        "the row the PATCH addressed"
    );
}
