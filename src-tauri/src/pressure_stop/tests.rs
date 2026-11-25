//! Tests for [`super`] — literal expected values, and the failing arm of every
//! rule the plan names: the coalescing window, the bounded drop-oldest queue,
//! null-not-zero stats, a stale census nulled, the 404 degrade.

use super::*;
use crate::ci_node::admission::{headroom_defer_term, headroom_defers, Headroom, HeadroomTerm};
use crate::fleet::resource_sample::{Saturation, SaturationSource};
use crate::resource_guard::{GateObservation, LaneMetric};

const GIB: u64 = 1024 * 1024 * 1024;

fn memory_observation() -> GateObservation {
    GateObservation {
        lane: "host".to_string(),
        metric: LaneMetric::FreeCommitBytes,
        observed: 1_000_000_000,
        limit: 1_610_612_736,
    }
}

fn thread_observation() -> GateObservation {
    GateObservation {
        lane: "threads".to_string(),
        metric: LaneMetric::ThreadCount,
        observed: 412,
        limit: 400,
    }
}

fn unknown_stats() -> BoxStats {
    BoxStats {
        session_count_terminal: None,
        session_count_live_claude: None,
        active_ci_runners: None,
        ci_jobs_queued: None,
        calibration: json!({}),
        fleet_calibration: fleet_calibration_snapshot(),
        box_stats: json!({}),
    }
}

fn known_stats() -> BoxStats {
    BoxStats {
        session_count_terminal: Some(7),
        session_count_live_claude: Some(12),
        active_ci_runners: Some(2),
        ci_jobs_queued: Some(1),
        calibration: json!({"thread_ceilings": {"warn": 256}}),
        fleet_calibration: fleet_calibration_snapshot(),
        box_stats: json!({"thread_count": 300}),
    }
}

fn fixed_instant() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-11-24T12:00:00Z")
        .expect("literal timestamp")
        .with_timezone(&chrono::Utc)
}

fn event(id: &str) -> PressureStopEvent {
    build_event(
        id.to_string(),
        fixed_instant(),
        StopKind::RefusedSpawn,
        Severity::Critical,
        WhatClass::TerminalSpawn,
        &StopReading::from_observation(&memory_observation()).expect("host lane"),
        None,
        unknown_stats(),
        0,
    )
}

/// Admit a stop of `(kind, lane, what)` at `now`.
fn adm(em: &mut Emitter, kind: StopKind, lane: StopLane, what: WhatClass, now: Instant) -> Admit {
    let stop = Stop {
        kind,
        severity: Severity::Warn,
        what,
        reading: StopReading {
            lane,
            guard_lane: "host",
            observed: Some(1),
            limit: Some(2),
        },
        reason: None,
        extra: Value::Null,
    };
    em.admit(&stop, now, fixed_instant())
}

fn ids(em: &mut Emitter) -> Vec<String> {
    let all = em.take_batch(usize::MAX);
    let out = all.iter().map(|e| e.client_event_id.clone()).collect();
    em.requeue(all);
    out
}

// ---------------------------------------------------------------------------
// Constants and vocabulary
// ---------------------------------------------------------------------------

#[test]
fn the_constants_are_the_plans_numbers() {
    assert_eq!(COALESCE_WINDOW, Duration::from_secs(60));
    assert_eq!(QUEUE_CAP, 64);
    assert_eq!(MAX_BATCH, 20);
    assert_eq!(POST_TIMEOUT, Duration::from_secs(8));
    assert_eq!(MAX_JSON_BYTES, 8192);
    // One census interval (600 s) plus 120 s of slack.
    assert_eq!(CENSUS_MAX_AGE, Duration::from_secs(720));
}

#[test]
fn the_vocabulary_is_coords_closed_sets() {
    assert_eq!(StopKind::RefusedSpawn.as_str(), "refused_spawn");
    assert_eq!(StopKind::DeferredLaunch.as_str(), "deferred_launch");
    assert_eq!(
        StopKind::DeferredContinuation.as_str(),
        "deferred_continuation"
    );
    assert_eq!(StopKind::DeferredCiJob.as_str(), "deferred_ci_job");
    assert_eq!(StopLane::MemoryFreeCommit.as_str(), "memory_free_commit");
    assert_eq!(StopLane::ThreadPressure.as_str(), "thread_pressure");
    assert_eq!(Severity::Warn.as_str(), "warn");
    assert_eq!(Severity::Critical.as_str(), "critical");
    assert_eq!(Severity::from_word("warn"), Some(Severity::Warn));
    assert_eq!(Severity::from_word("critical"), Some(Severity::Critical));
    assert_eq!(Severity::from_word("proceed"), None);
    for (what, wire) in [
        (WhatClass::TerminalSpawn, "terminal_spawn"),
        (WhatClass::RunnerInstance, "runner_instance"),
        (WhatClass::CoordLaunch, "coord_launch"),
        (WhatClass::GateContinuation, "gate_continuation"),
        (WhatClass::CiJob, "ci_job"),
        (WhatClass::LoopingAgent, "looping_agent"),
        (WhatClass::Other, "other"),
    ] {
        assert_eq!(what.as_str(), wire);
    }
}

#[test]
fn the_spawn_nouns_the_callers_pass_map_to_classes_and_nothing_is_guessed() {
    assert_eq!(
        WhatClass::from_spawn_noun("terminal session"),
        WhatClass::TerminalSpawn
    );
    assert_eq!(
        WhatClass::from_spawn_noun("runner instance"),
        WhatClass::RunnerInstance
    );
    assert_eq!(
        WhatClass::from_spawn_noun("looping-agent session"),
        WhatClass::LoopingAgent
    );
    assert_eq!(
        WhatClass::from_spawn_noun("scheduled remote agent"),
        WhatClass::Other
    );
    assert_eq!(
        WhatClass::from_spawn_noun("Terminal Session"),
        WhatClass::Other
    );
}

// ---------------------------------------------------------------------------
// Event construction
// ---------------------------------------------------------------------------

#[test]
fn a_memory_refusal_builds_the_literal_wire_event() {
    let ev = build_event(
        "id-1".to_string(),
        fixed_instant(),
        StopKind::RefusedSpawn,
        Severity::Critical,
        WhatClass::TerminalSpawn,
        &StopReading::from_observation(&memory_observation()).expect("host lane"),
        Some("commit_pressure:critical:1000000000_under_1610612736".to_string()),
        known_stats(),
        3,
    );
    let wire = serde_json::to_value(&ev).expect("serializes");
    assert_eq!(
        wire,
        json!({
            "client_event_id": "id-1",
            "observed_at": "2026-11-24T12:00:00.000Z",
            "stop_kind": "refused_spawn",
            "lane": "memory_free_commit",
            "guard_lane": "host",
            "severity": "critical",
            "what_class": "terminal_spawn",
            "observed": 1_000_000_000i64,
            "limit_value": 1_610_612_736i64,
            "reason": "commit_pressure:critical:1000000000_under_1610612736",
            "session_count_terminal": 7,
            "session_count_live_claude": 12,
            "active_ci_runners": 2,
            "ci_jobs_queued": 1,
            "calibration": {"thread_ceilings": {"warn": 256}},
            "fleet_calibration": {"state": "unknown", "reason": "fleet-calibration state source not shipped"},
            "suppressed_count": 3,
            "dropped_before": 0,
            "box_stats": {"thread_count": 300},
        })
    );
}

#[test]
fn a_thread_refusal_carries_the_thread_lane_and_the_threads_pool() {
    let reading = StopReading::from_observation(&thread_observation()).expect("threads lane");
    assert_eq!(
        reading,
        StopReading {
            lane: StopLane::ThreadPressure,
            guard_lane: "threads",
            observed: Some(412),
            limit: Some(400),
        }
    );
    let wsl = GateObservation {
        lane: "wsl".to_string(),
        ..memory_observation()
    };
    assert_eq!(
        StopReading::from_observation(&wsl).map(|r| r.guard_lane),
        Some("wsl")
    );
}

#[test]
fn an_unknown_guard_lane_is_not_sent_as_a_guess() {
    let bogus = GateObservation {
        lane: "container".to_string(),
        ..memory_observation()
    };
    assert_eq!(StopReading::from_observation(&bogus), None);
}

#[test]
fn unknown_counts_are_null_on_the_wire_never_zero() {
    let wire = serde_json::to_value(event("id-2")).expect("serializes");
    for field in [
        "session_count_terminal",
        "session_count_live_claude",
        "active_ci_runners",
        "ci_jobs_queued",
        "reason",
    ] {
        assert_eq!(
            wire.get(field),
            Some(&Value::Null),
            "{field} must be present and null"
        );
    }
}

#[test]
fn the_wire_carries_exactly_coords_field_set() {
    let wire = serde_json::to_value(event("id-3")).expect("serializes");
    let mut keys: Vec<&str> = wire
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "active_ci_runners",
            "box_stats",
            "calibration",
            "ci_jobs_queued",
            "client_event_id",
            "dropped_before",
            "fleet_calibration",
            "guard_lane",
            "lane",
            "limit_value",
            "observed",
            "observed_at",
            "reason",
            "session_count_live_claude",
            "session_count_terminal",
            "severity",
            "stop_kind",
            "suppressed_count",
            "what_class",
        ]
    );
}

#[test]
fn an_oversized_snapshot_becomes_a_typed_unknown_not_a_rejected_event() {
    let fat = json!({"blob": "x".repeat(MAX_JSON_BYTES)});
    let capped = cap_json("calibration", fat);
    assert_eq!(capped["state"], json!("unknown"));
    assert_eq!(
        capped["reason"],
        json!("calibration snapshot was 8203 bytes, over the 8192-byte wire cap")
    );
    let small = json!({"a": 1});
    assert_eq!(cap_json("calibration", small.clone()), small);
}

#[test]
fn fleet_calibration_is_a_typed_unknown_until_its_seam_is_filled() {
    assert_eq!(
        fleet_calibration_snapshot(),
        json!({"state": "unknown", "reason": "fleet-calibration state source not shipped"})
    );
}

// ---------------------------------------------------------------------------
// Coalescing
// ---------------------------------------------------------------------------

#[test]
fn two_refusals_inside_the_window_post_one_event_and_the_next_carries_suppressed_1() {
    let mut em = Emitter::new(QUEUE_CAP);
    let t0 = Instant::now();
    let key = (
        StopKind::RefusedSpawn,
        StopLane::MemoryFreeCommit,
        WhatClass::TerminalSpawn,
    );
    assert_eq!(
        adm(&mut em, key.0, key.1, key.2, t0),
        Admit::Post { suppressed: 0 }
    );
    assert_eq!(
        adm(&mut em, key.0, key.1, key.2, t0 + Duration::from_secs(30)),
        Admit::Suppressed
    );
    assert_eq!(
        adm(&mut em, key.0, key.1, key.2, t0 + Duration::from_secs(61)),
        Admit::Post { suppressed: 1 }
    );
}

#[test]
fn the_window_closes_at_exactly_60_seconds() {
    let mut em = Emitter::new(QUEUE_CAP);
    let t0 = Instant::now();
    let (k, l, w) = (
        StopKind::DeferredLaunch,
        StopLane::ThreadPressure,
        WhatClass::CoordLaunch,
    );
    assert_eq!(adm(&mut em, k, l, w, t0), Admit::Post { suppressed: 0 });
    assert_eq!(
        adm(&mut em, k, l, w, t0 + Duration::from_millis(59_999)),
        Admit::Suppressed
    );
    assert_eq!(
        adm(&mut em, k, l, w, t0 + Duration::from_millis(59_999)),
        Admit::Suppressed
    );
    assert_eq!(
        adm(&mut em, k, l, w, t0 + Duration::from_secs(60)),
        Admit::Post { suppressed: 2 }
    );
    // The window re-opened at 60 s: a stop at 61 s is inside it.
    assert_eq!(
        adm(&mut em, k, l, w, t0 + Duration::from_secs(61)),
        Admit::Suppressed
    );
}

#[test]
fn coalescing_is_per_kind_lane_and_class() {
    let mut em = Emitter::new(QUEUE_CAP);
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_secs(1);
    assert_eq!(
        adm(
            &mut em,
            StopKind::RefusedSpawn,
            StopLane::MemoryFreeCommit,
            WhatClass::TerminalSpawn,
            t0
        ),
        Admit::Post { suppressed: 0 }
    );
    // Each differs from the first in exactly one coordinate.
    assert_eq!(
        adm(
            &mut em,
            StopKind::RefusedSpawn,
            StopLane::MemoryFreeCommit,
            WhatClass::LoopingAgent,
            t1
        ),
        Admit::Post { suppressed: 0 }
    );
    assert_eq!(
        adm(
            &mut em,
            StopKind::RefusedSpawn,
            StopLane::ThreadPressure,
            WhatClass::TerminalSpawn,
            t1
        ),
        Admit::Post { suppressed: 0 }
    );
    assert_eq!(
        adm(
            &mut em,
            StopKind::DeferredContinuation,
            StopLane::MemoryFreeCommit,
            WhatClass::TerminalSpawn,
            t1
        ),
        Admit::Post { suppressed: 0 }
    );
}

// ---------------------------------------------------------------------------
// The bounded queue
// ---------------------------------------------------------------------------

#[test]
fn overflow_drops_the_oldest_and_the_next_event_says_so() {
    let mut em = Emitter::new(QUEUE_CAP);
    for i in 0..64 {
        em.push(event(&format!("e{i}")));
    }
    assert_eq!(em.len(), 64);
    em.push(event("e64"));
    assert_eq!(em.len(), 64);
    let batch = em.take_batch(usize::MAX);
    assert_eq!(
        batch.first().map(|e| e.client_event_id.as_str()),
        Some("e1")
    );
    assert_eq!(
        batch.last().map(|e| e.client_event_id.as_str()),
        Some("e64")
    );
    assert_eq!(batch.last().map(|e| e.dropped_before), Some(1));
    // Only the event that followed the drop carries it.
    assert_eq!(batch.iter().map(|e| e.dropped_before).sum::<u32>(), 1);
}

#[test]
fn a_dropped_event_hands_on_the_loss_it_was_carrying() {
    let mut em = Emitter::new(2);
    em.push(event("a"));
    em.note_lost(3);
    em.push(event("b")); // carries 3
    em.push(event("c")); // drops "a": 1
    em.push(event("d")); // drops "b": 1 + its 3
    let batch = em.take_batch(usize::MAX);
    let got: Vec<(String, u32)> = batch
        .iter()
        .map(|e| (e.client_event_id.clone(), e.dropped_before))
        .collect();
    assert_eq!(got, vec![("c".to_string(), 1), ("d".to_string(), 4)]);
}

#[test]
fn a_requeued_batch_goes_back_in_order_at_the_front() {
    let mut em = Emitter::new(QUEUE_CAP);
    for id in ["a", "b", "c", "d"] {
        em.push(event(id));
    }
    let batch = em.take_batch(3);
    assert_eq!(em.len(), 1);
    em.requeue(batch);
    assert_eq!(ids(&mut em), vec!["a", "b", "c", "d"]);
}

#[test]
fn take_batch_takes_at_most_the_batch_size() {
    let mut em = Emitter::new(QUEUE_CAP);
    for i in 0..25 {
        em.push(event(&format!("e{i}")));
    }
    assert_eq!(em.take_batch(MAX_BATCH).len(), 20);
    assert_eq!(em.len(), 5);
}

// ---------------------------------------------------------------------------
// The census: stale is nulled, absent is nulled, both with a reason
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_census_is_a_count_with_its_age() {
    let now_ms = 1_000_000_000;
    assert_eq!(
        live_claude_from_census(Some((now_ms - 1_000, 12)), now_ms, CENSUS_MAX_AGE),
        (Some(12), Some(1_000), None)
    );
    // Exactly at the bound is still fresh.
    assert_eq!(
        live_claude_from_census(Some((now_ms - 720_000, 0)), now_ms, CENSUS_MAX_AGE),
        (Some(0), Some(720_000), None)
    );
}

#[test]
fn a_stale_census_is_nulled_and_its_age_still_recorded() {
    let now_ms = 1_000_000_000;
    let (count, age, reason) =
        live_claude_from_census(Some((now_ms - 720_001, 12)), now_ms, CENSUS_MAX_AGE);
    assert_eq!(count, None);
    assert_eq!(age, Some(720_001));
    assert_eq!(
        reason.as_deref(),
        Some("the session-tracking census is 720001 ms old, past the 720000 ms bound")
    );
}

#[test]
fn no_census_yet_is_unknown_not_zero() {
    assert_eq!(
        live_claude_from_census(None, 5, CENSUS_MAX_AGE),
        (
            None,
            None,
            Some("no session-tracking census has completed yet".to_string())
        )
    );
}

// ---------------------------------------------------------------------------
// record_into: the switch, and a failing reader
// ---------------------------------------------------------------------------

fn a_stop() -> Stop {
    Stop {
        kind: StopKind::RefusedSpawn,
        severity: Severity::Critical,
        what: WhatClass::TerminalSpawn,
        reading: StopReading::from_observation(&memory_observation()).expect("host lane"),
        reason: None,
        extra: Value::Null,
    }
}

#[test]
fn a_stop_s_extra_facts_land_in_box_stats() {
    let sink = Mutex::new(Emitter::new(QUEUE_CAP));
    let stop = Stop {
        extra: json!({"observed_is_projection": true, "projected_pending_spawns": 2}),
        ..a_stop()
    };
    assert!(record_into(&sink, true, stop, known_stats));
    let batch = sink.lock().expect("lock").take_batch(1);
    assert_eq!(
        batch[0].box_stats,
        json!({
            "thread_count": 300,
            "observed_is_projection": true,
            "projected_pending_spawns": 2,
        })
    );
    // A non-object extra adds nothing.
    assert_eq!(merge_extra(json!({"a": 1}), &Value::Null), json!({"a": 1}));
}

#[test]
fn a_failed_stats_read_gives_its_carried_count_back_to_the_window() {
    let mut em = Emitter::new(QUEUE_CAP);
    let t0 = Instant::now();
    let stop = a_stop();
    assert_eq!(
        em.admit(&stop, t0, fixed_instant()),
        Admit::Post { suppressed: 0 }
    );
    assert_eq!(
        em.admit(&stop, t0 + Duration::from_secs(1), fixed_instant()),
        Admit::Suppressed
    );
    // The next window's event was owed 1; its stats read failed.
    let next = t0 + Duration::from_secs(61);
    assert_eq!(
        em.admit(&stop, next, fixed_instant()),
        Admit::Post { suppressed: 1 }
    );
    em.restore_suppressed(&stop, 1);
    // So the one after carries it.
    assert_eq!(
        em.admit(&stop, next + Duration::from_secs(61), fixed_instant()),
        Admit::Post { suppressed: 1 }
    );
}

#[test]
fn a_storm_that_ends_is_reported_by_a_trailing_event() {
    let mut em = Emitter::new(QUEUE_CAP);
    let t0 = Instant::now();
    let stop = a_stop();
    let last_at = chrono::DateTime::parse_from_rfc3339("2026-11-24T12:00:40Z")
        .expect("literal")
        .with_timezone(&chrono::Utc);
    assert_eq!(
        em.admit(&stop, t0, fixed_instant()),
        Admit::Post { suppressed: 0 }
    );
    for s in [10, 20, 40] {
        assert_eq!(
            em.admit(&stop, t0 + Duration::from_secs(s), last_at),
            Admit::Suppressed
        );
    }
    // Still inside the window: nothing trails yet.
    assert_eq!(queue_trailing(&mut em, t0 + Duration::from_secs(59)), 0);
    assert_eq!(queue_trailing(&mut em, t0 + Duration::from_secs(60)), 1);
    let batch = em.take_batch(usize::MAX);
    assert_eq!(batch.len(), 1);
    let ev = &batch[0];
    // Three suppressed stops: the trailing row IS the last one, carrying 2.
    assert_eq!(ev.suppressed_count, 2);
    assert_eq!(ev.observed_at, "2026-11-24T12:00:40.000Z");
    assert_eq!(ev.stop_kind, "refused_spawn");
    assert_eq!(ev.session_count_live_claude, None);
    assert_eq!(ev.box_stats["trailing"], json!(true));
    // The window is gone: the next stop opens a fresh one.
    assert_eq!(
        em.admit(&stop, t0 + Duration::from_secs(61), fixed_instant()),
        Admit::Post { suppressed: 0 }
    );
    // A window that suppressed nothing trails nothing.
    assert_eq!(queue_trailing(&mut em, t0 + Duration::from_secs(200)), 0);
}

#[test]
fn clearing_drops_the_queue_the_windows_and_the_pending_loss() {
    let mut em = Emitter::new(QUEUE_CAP);
    em.push(event("a"));
    em.note_lost(5);
    let stop = a_stop();
    let t0 = Instant::now();
    em.admit(&stop, t0, fixed_instant());
    em.clear();
    assert!(em.is_empty());
    assert_eq!(em.pending_dropped(), 0);
    assert_eq!(
        em.admit(&stop, t0, fixed_instant()),
        Admit::Post { suppressed: 0 }
    );
}

#[test]
fn the_switch_off_fills_nothing_and_never_reads_the_box() {
    let sink = Mutex::new(Emitter::new(QUEUE_CAP));
    let queued = record_into(&sink, false, a_stop(), || {
        panic!("the box must not be read when reporting is off")
    });
    assert!(!queued);
    assert_eq!(sink.lock().expect("lock").len(), 0);
}

#[test]
fn the_switch_on_queues_one_event_with_the_stats_read() {
    let sink = Mutex::new(Emitter::new(QUEUE_CAP));
    assert!(record_into(&sink, true, a_stop(), known_stats));
    // Inside the window: counted, not queued, and the box is not read again.
    assert!(!record_into(&sink, true, a_stop(), || {
        panic!("a suppressed stop must not read the box")
    }));
    let mut em = sink.lock().expect("lock");
    let batch = em.take_batch(usize::MAX);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].session_count_live_claude, Some(12));
    assert_eq!(batch[0].suppressed_count, 0);
}

#[test]
fn a_panicking_reader_queues_nothing_and_does_not_unwind_into_the_guard() {
    let sink = Mutex::new(Emitter::new(QUEUE_CAP));
    let queued = record_into(&sink, true, a_stop(), || -> BoxStats {
        panic!("a gauge read failed")
    });
    assert!(!queued);
    assert_eq!(sink.lock().expect("lock").len(), 0);
}

// ---------------------------------------------------------------------------
// Response triage, per status code
// ---------------------------------------------------------------------------

#[test]
fn a_404_or_405_keeps_the_events_and_backs_off_quietly() {
    for status in [404u16, 405] {
        match triage(status, Some("not found"), 3) {
            Delivery::Requeue { class, backoff, .. } => {
                assert_eq!(class, "route_absent");
                assert_eq!(backoff, Some(Duration::from_secs(300)));
            }
            other => panic!("{status}: {other:?}"),
        }
    }
}

#[test]
fn a_503_keeps_the_events_for_the_next_tick() {
    assert_eq!(
        triage(503, None, 1),
        Delivery::Requeue {
            class: "unavailable",
            backoff: None,
            detail: "HTTP 503".to_string(),
        }
    );
}

#[test]
fn a_401_or_403_keeps_the_events_and_backs_off() {
    for status in [401u16, 403] {
        match triage(status, None, 1) {
            Delivery::Requeue { class, backoff, .. } => {
                assert_eq!(class, "auth");
                assert_eq!(backoff, Some(Duration::from_secs(300)));
            }
            other => panic!("{status}: {other:?}"),
        }
    }
}

#[test]
fn a_422_drops_the_batch() {
    assert_eq!(
        triage(422, Some(r#"{"error":"batch_too_large"}"#), 21),
        Delivery::Drop {
            class: "unprocessable",
            detail: r#"HTTP 422: {"error":"batch_too_large"}"#.to_string(),
        }
    );
    match triage(400, None, 1) {
        Delivery::Drop { class, .. } => assert_eq!(class, "client_error"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_5xx_keeps_the_events_with_a_jittered_backoff() {
    match triage(500, None, 1) {
        Delivery::Requeue { class, backoff, .. } => {
            assert_eq!(class, "server_error");
            let b = backoff.expect("a backoff");
            assert!(
                b >= Duration::from_secs(30) && b <= Duration::from_secs(60),
                "{b:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_clean_200_is_stored_with_nothing_lost() {
    let body = r#"{"received":2,"accepted":2,"stored":1,"duplicate":1,"not_stored":[],
                  "rejected":[],"problems":[],"table_present":true,"persist_error":null}"#;
    assert_eq!(
        triage(200, Some(body), 2),
        Delivery::Stored {
            stored: 1,
            duplicate: 1,
            lost: 0,
            problems: vec![],
        }
    );
}

#[test]
fn a_200_with_a_persist_error_is_a_counted_loss_not_a_store() {
    let body = r#"{"received":2,"accepted":2,"stored":0,"duplicate":0,"not_stored":["a","b"],
                  "rejected":[],"table_present":true,"persist_error":"INSERT failed: boom"}"#;
    assert_eq!(
        triage(200, Some(body), 2),
        Delivery::Stored {
            stored: 0,
            duplicate: 0,
            lost: 2,
            problems: vec!["persist_error: INSERT failed: boom".to_string()],
        }
    );
}

#[test]
fn a_200_with_rejected_events_counts_them_lost() {
    let body = r#"{"received":2,"accepted":1,"stored":1,"duplicate":0,
                  "rejected":[{"index":1,"client_event_id":"b","reason":"unknown lane \"x\""}],
                  "table_present":true,"persist_error":null}"#;
    assert_eq!(
        triage(200, Some(body), 2),
        Delivery::Stored {
            stored: 1,
            duplicate: 0,
            lost: 1,
            problems: vec!["rejected: unknown lane \"x\"".to_string()],
        }
    );
}

#[test]
fn a_200_without_the_table_keeps_the_events() {
    let body = r#"{"received":1,"accepted":1,"stored":0,"duplicate":0,"table_present":false}"#;
    match triage(200, Some(body), 1) {
        Delivery::Requeue { class, backoff, .. } => {
            assert_eq!(class, "table_absent");
            assert_eq!(backoff, Some(Duration::from_secs(300)));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_200_that_is_not_the_documented_outcome_is_unknown() {
    assert!(matches!(
        triage(200, Some("ok"), 1),
        Delivery::Unknown { .. }
    ));
    assert!(matches!(triage(200, None, 1), Delivery::Unknown { .. }));
    assert!(matches!(
        triage(200, Some("{}"), 1),
        Delivery::Unknown { .. }
    ));
}

// ---------------------------------------------------------------------------
// apply: what each outcome does to the queue
// ---------------------------------------------------------------------------

#[test]
fn a_requeue_keeps_the_batch_and_arms_the_backoff() {
    let mut em = Emitter::new(QUEUE_CAP);
    em.push(event("a"));
    em.push(event("b"));
    let batch = em.take_batch(MAX_BATCH);
    let now = Instant::now();
    let lines = em.apply(triage(404, None, 2), batch, now);
    assert_eq!(em.len(), 2);
    assert!(em.backing_off(now + Duration::from_secs(299)));
    assert!(!em.backing_off(now + Duration::from_secs(300)));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].0, LogLevel::Info);
    // The same failure class logs once, not every tick.
    let batch = em.take_batch(MAX_BATCH);
    assert!(em.apply(triage(404, None, 2), batch, now).is_empty());
}

#[test]
fn a_second_outage_after_a_recovery_is_logged_again() {
    let mut em = Emitter::new(QUEUE_CAP);
    let now = Instant::now();
    let ok = r#"{"stored":1,"duplicate":0,"table_present":true,"persist_error":null}"#;
    em.push(event("a"));
    let b = em.take_batch(1);
    assert_eq!(em.apply(triage(404, None, 1), b, now).len(), 1);
    let b = em.take_batch(1);
    assert_eq!(
        em.apply(triage(200, Some(ok), 1), b, now),
        vec![(
            LogLevel::Debug,
            "coord stored 1, duplicate 0 pressure-stop event(s)".to_string()
        )]
    );
    em.push(event("b"));
    let b = em.take_batch(1);
    let lines = em.apply(triage(404, None, 1), b, now);
    assert_eq!(lines.len(), 1, "the outage after a recovery is news");
}

#[test]
fn a_drop_counts_the_batch_into_the_next_events_dropped_before() {
    let mut em = Emitter::new(QUEUE_CAP);
    em.push(event("a"));
    em.push(event("b"));
    let batch = em.take_batch(MAX_BATCH);
    let lines = em.apply(triage(422, None, 2), batch, Instant::now());
    assert_eq!(lines[0].0, LogLevel::Warn);
    assert_eq!(em.len(), 0);
    assert_eq!(em.pending_dropped(), 2);
    em.push(event("c"));
    assert_eq!(em.take_batch(1)[0].dropped_before, 2);
}

#[test]
fn a_persist_failure_is_logged_loudly_and_counted() {
    let mut em = Emitter::new(QUEUE_CAP);
    em.push(event("a"));
    let batch = em.take_batch(MAX_BATCH);
    let body = r#"{"stored":0,"duplicate":0,"table_present":true,"persist_error":"boom"}"#;
    let lines = em.apply(triage(200, Some(body), 1), batch, Instant::now());
    assert_eq!(lines[0].0, LogLevel::Warn);
    assert_eq!(em.pending_dropped(), 1);
    assert!(!em.backing_off(Instant::now()));
}

// ---------------------------------------------------------------------------
// CI headroom: the single author of the verdict, quoted as a stop
// ---------------------------------------------------------------------------

#[test]
fn the_commit_term_quotes_the_defer_floor_in_bytes() {
    let h = Headroom {
        commit_available_bytes: Some(2 * GIB),
        ..Headroom::default()
    };
    let term = headroom_defer_term(h);
    assert_eq!(
        term,
        Some(HeadroomTerm::Commit {
            free_bytes: 2 * GIB,
            floor_bytes: 8 * GIB,
        })
    );
    let stop = ci_headroom_stop(term.expect("tripped"));
    assert_eq!(
        stop.reading,
        StopReading {
            lane: StopLane::MemoryFreeCommit,
            guard_lane: "host",
            observed: Some(2 * GIB),
            limit: Some(8 * GIB),
        }
    );
    assert_eq!(
        stop.reason.as_deref(),
        Some("ci_headroom:commit:2147483648_under_8589934592")
    );
    assert_eq!(stop.kind, StopKind::DeferredCiJob);
    assert_eq!(stop.what, WhatClass::CiJob);
    assert_eq!(stop.severity, Severity::Warn);
    assert_eq!(stop.extra, json!({"ci_headroom_term": "commit"}));
}

#[test]
fn the_swap_term_quotes_half_the_ceiling() {
    let h = Headroom {
        swap_total_bytes: Some(10 * GIB),
        swap_used_bytes: Some(6 * GIB),
        ..Headroom::default()
    };
    assert_eq!(
        headroom_defer_term(h),
        Some(HeadroomTerm::Swap {
            used_bytes: 6 * GIB,
            limit_bytes: 5 * GIB,
        })
    );
    // Swap USED over a ceiling share is not "free commit under a floor": the
    // columns stay null and the numbers ride box_stats.
    let stop = ci_headroom_stop(headroom_defer_term(h).expect("tripped"));
    assert_eq!(stop.reading.lane, StopLane::MemoryFreeCommit);
    assert_eq!(stop.reading.observed, None);
    assert_eq!(stop.reading.limit, None);
    assert_eq!(
        stop.extra,
        json!({
            "ci_headroom_term": "swap",
            "swap_used_bytes": 6 * GIB,
            "swap_defer_bytes": 5 * GIB,
        })
    );
}

#[test]
fn the_saturation_term_is_the_thread_class() {
    let h = Headroom {
        saturation: Saturation::threads(Some(900), Some(1000), SaturationSource::Cgroup),
        ..Headroom::default()
    };
    let term = headroom_defer_term(h).expect("tripped");
    assert_eq!(
        term,
        HeadroomTerm::Saturation {
            used: 900,
            limit: 800,
            counter: "threads",
        }
    );
    let stop = ci_headroom_stop(term);
    assert_eq!(
        stop.reading,
        StopReading {
            lane: StopLane::ThreadPressure,
            guard_lane: "threads",
            observed: Some(900),
            limit: Some(800),
        }
    );
    assert_eq!(
        stop.reason.as_deref(),
        Some("ci_headroom:saturation:900_over_800")
    );
    assert_eq!(
        stop.extra,
        json!({"ci_headroom_term": "saturation", "saturation_counter": "threads"})
    );
    // A PID ceiling says so rather than passing for threads.
    let pids = Headroom {
        saturation: Saturation::pids(Some(90), Some(100), SaturationSource::Cgroup),
        ..Headroom::default()
    };
    assert_eq!(
        headroom_defer_term(pids),
        Some(HeadroomTerm::Saturation {
            used: 90,
            limit: 80,
            counter: "pids",
        })
    );
}

#[test]
fn a_box_with_headroom_or_no_readings_is_no_term_and_no_defer() {
    let calm = Headroom {
        commit_available_bytes: Some(40 * GIB),
        swap_total_bytes: Some(10 * GIB),
        swap_used_bytes: Some(GIB),
        saturation: Saturation::threads(Some(100), Some(1000), SaturationSource::Cgroup),
        session_warn_floor_bytes: None,
    };
    assert_eq!(headroom_defer_term(calm), None);
    assert!(!headroom_defers(calm));
    assert_eq!(headroom_defer_term(Headroom::default()), None);
    assert!(!headroom_defers(Headroom::default()));
}

// ---------------------------------------------------------------------------
// Wiring: each hook site calls the recorder, in production code
// ---------------------------------------------------------------------------

/// The text of one function: from its definition `signature` (unique in the
/// file, so a test function can never be the match) to the first `}` at
/// column 0 after it. Line endings are tolerated either way.
fn fn_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("`{signature}` must exist"));
    assert_eq!(
        src.matches(signature).count(),
        1,
        "`{signature}` must name exactly one definition"
    );
    let rest = src.get(start..).unwrap_or("");
    let end = rest
        .find("\n}\n")
        .or_else(|| rest.find("\n}\r\n"))
        .map_or(rest.len(), |e| e + 2);
    rest.get(..end).unwrap_or(rest)
}

#[test]
fn admit_spawn_records_its_critical_refusal_before_returning_it() {
    let src = include_str!("../resource_guard.rs");
    let body = fn_body(src, "pub(crate) fn admit_spawn(");
    let record = body
        .find("pressure_stop::record_observation(")
        .expect("admit_spawn records its refusal");
    let refusal = body
        .rfind("Err(critical_refusal(what, &observation))")
        .expect("admit_spawn refuses");
    assert!(
        record < refusal,
        "the stop is recorded before the refusal returns"
    );
    assert!(body.contains("StopKind::RefusedSpawn"));
    assert!(body.contains("WhatClass::from_spawn_noun(what)"));
    // Exactly one: the override and warn arms proceed, and are not stops.
    assert_eq!(
        body.matches("pressure_stop::record_observation(").count(),
        1
    );
}

#[test]
fn precheck_spawn_records_its_critical_refusal() {
    let src = include_str!("../resource_guard.rs");
    let body = fn_body(src, "pub(crate) fn precheck_spawn(");
    assert_eq!(
        body.matches("pressure_stop::record_observation(").count(),
        1
    );
    assert!(body.contains("StopKind::RefusedSpawn"));
}

#[test]
fn admit_launch_records_a_deferred_launch() {
    let src = include_str!("../agent_runtime.rs");
    let body = fn_body(src, "fn admit_launch(");
    assert!(body.contains("pressure_stop::record_observation("));
    assert!(body.contains("StopKind::DeferredLaunch"));
    assert!(body.contains("WhatClass::CoordLaunch"));
}

#[test]
fn the_continuation_load_pressure_arm_records_a_deferred_continuation() {
    let src = include_str!("../agent_runtime.rs");
    let body = fn_body(src, "async fn run_gate_continuation_inner(");
    let arm = body
        .find("ContinuationGuard::LoadPressure {")
        .expect("the load-pressure arm exists");
    let tail = body.get(arm..).unwrap_or("");
    let record = tail
        .find("StopKind::DeferredContinuation")
        .expect("the arm records the stop");
    let defer = tail
        .find("defer_continuation_unclaimed(")
        .expect("the arm defers");
    assert!(record < defer, "recorded before the deferral returns");
    assert!(tail.contains("WhatClass::GateContinuation"));
}

#[test]
fn the_ci_headroom_defer_records_and_the_at_cap_defer_does_not() {
    let src = include_str!("../ci_node/admission.rs");
    let body = fn_body(src, "fn admit(payload: CiDispatchPayload");
    let gate = body
        .find("if !at_cap {")
        .expect("the at-cap defer is excluded");
    let record = body
        .find("pressure_stop::record_ci_headroom(term)")
        .expect("the headroom defer records");
    assert!(gate < record);
    // `admission_decision` stays pure: no recorder inside it.
    let decision = fn_body(src, "pub(crate) fn admission_decision(");
    assert!(!decision.contains("pressure_stop"));
}

#[test]
fn a_flush_with_reporting_off_discards_before_it_can_post() {
    let src = include_str!("../pressure_stop.rs");
    let body = fn_body(src, "pub(crate) async fn flush(");
    let refresh = body
        .find("refresh_enabled_from_settings();")
        .expect("the switch is re-read");
    let off = body.find("if !enabled() {").expect("the off arm");
    let clear = body
        .find("lock_emitter().clear();")
        .expect("the off arm clears");
    let take = body.find("take_batch(MAX_BATCH)").expect("the batch");
    let post = body.find("send_batch(").expect("the post");
    assert!(refresh < off && off < clear && clear < take && take < post);
    let set = fn_body(src, "pub(crate) fn set_enabled(");
    assert!(
        set.contains("lock_emitter().clear();"),
        "switching off empties the queue"
    );
}

#[test]
fn the_republisher_flushes_the_queue_beside_the_sampler() {
    let src = include_str!("../fleet.rs");
    let body = fn_body(src, "pub fn spawn_budget_republisher(");
    let sample = body
        .find("resource_sample::publish_once().await")
        .expect("the sampler");
    let flush = body
        .find("crate::pressure_stop::flush().await")
        .expect("the flush");
    assert!(sample < flush);
    // The switch is read as SET before the loop's first tick.
    let refresh = body
        .find("crate::pressure_stop::refresh_enabled_from_settings();")
        .expect("the switch is read at start");
    let loop_start = body.find("loop {").expect("the loop");
    assert!(refresh < loop_start);
}

// ---------------------------------------------------------------------------
// Delivery against a fake coord
// ---------------------------------------------------------------------------

mod fake_coord {
    use super::*;
    use axum::extract::State as AxumState;
    use axum::http::StatusCode as AxumStatus;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokMutex;

    #[derive(Default)]
    struct Recorder {
        bodies: Vec<Value>,
        /// Answer 404 (route not deployed) while this is true.
        route_absent: bool,
    }

    async fn spawn(rec: Arc<TokMutex<Recorder>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app: Router = Router::new()
            .route(
                "/coord/devices/{device_id}/resource-stop",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<Recorder>>>,
                     Json(body): Json<Value>| async move {
                        let mut g = state.lock().await;
                        if g.route_absent {
                            return (AxumStatus::NOT_FOUND, "no route").into_response();
                        }
                        let n = body["events"].as_array().map_or(0, Vec::len);
                        g.bodies.push(body);
                        (
                            AxumStatus::OK,
                            Json(json!({
                                "received": n, "accepted": n, "stored": n, "duplicate": 0,
                                "not_stored": [], "rejected": [], "problems": [],
                                "table_present": true, "persist_error": null,
                            })),
                        )
                            .into_response()
                    },
                ),
            )
            .with_state(rec);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_404_degrades_quietly_then_the_same_events_land_once_the_route_exists() {
        let rec = Arc::new(TokMutex::new(Recorder {
            route_absent: true,
            ..Recorder::default()
        }));
        let base = spawn(rec.clone()).await;
        let url = stop_url(&base, "11111111-1111-1111-1111-111111111111");
        assert_eq!(
            url,
            format!("{base}/coord/devices/11111111-1111-1111-1111-111111111111/resource-stop")
        );
        let client = reqwest::Client::new();

        let mut em = Emitter::new(QUEUE_CAP);
        em.push(event("e1"));
        let now = Instant::now();

        let batch = em.take_batch(MAX_BATCH);
        let delivery = send_batch(&client, &url, &batch).await;
        assert!(matches!(
            delivery,
            Delivery::Requeue {
                class: "route_absent",
                ..
            }
        ));
        em.apply(delivery, batch, now);
        assert_eq!(em.len(), 1, "a 404 keeps the event");
        assert_eq!(em.pending_dropped(), 0, "a 404 is not a loss");

        rec.lock().await.route_absent = false;
        let batch = em.take_batch(MAX_BATCH);
        let delivery = send_batch(&client, &url, &batch).await;
        assert_eq!(
            delivery,
            Delivery::Stored {
                stored: 1,
                duplicate: 0,
                lost: 0,
                problems: vec![],
            }
        );
        em.apply(delivery, batch, now);
        assert_eq!(em.len(), 0);

        let bodies = rec.lock().await.bodies.clone();
        assert_eq!(bodies.len(), 1);
        let sent = &bodies[0]["events"][0];
        assert_eq!(sent["client_event_id"], json!("e1"));
        assert_eq!(sent["stop_kind"], json!("refused_spawn"));
        assert_eq!(sent["guard_lane"], json!("host"));
        assert_eq!(sent["session_count_live_claude"], Value::Null);
        // The body carries only the batch: the path is the device's identity.
        assert_eq!(
            bodies[0]
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>()),
            Some(vec!["events".to_string()])
        );
    }
}
