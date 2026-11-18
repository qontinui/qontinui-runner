//! Tests for [`super`] (plan 2026-11-05 Phase 3): the paced re-entry, the
//! first-sight sweep, and a fresh process re-deriving a park.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use super::*;
use crate::terminal::limit_stop::transcript_limit_stop;

const SEVEN_DAY_STOP: &str = include_str!("fixtures/limit_stop/seven_day_stop.jsonl");
const SUBAGENT_STOP: &str = include_str!("fixtures/limit_stop/subagent_stop.jsonl");
const SUBAGENT_PARENT: &str = include_str!("fixtures/limit_stop/subagent_parent.jsonl");

fn live<'a>(ids: &'a [&'a str]) -> HashSet<&'a str> {
    ids.iter().copied().collect()
}

// ── Pacing ───────────────────────────────────────────────────────────────────

#[test]
fn a_park_fires_at_its_due_time_and_not_before() {
    let mut parks = ParkList::default();
    let mut pacer = Pacer::default();
    parks.park("t1", "s1", 10_000, "not-yet-reset");
    assert!(take_reentries(&mut parks, &mut pacer, &live(&["t1"]), 9_999).is_empty());
    let due = take_reentries(&mut parks, &mut pacer, &live(&["t1"]), 10_000);
    assert_eq!(
        due,
        vec![Park {
            terminal_id: "t1".to_string(),
            claude_session_id: "s1".to_string(),
            due_at_ms: 10_000,
        }]
    );
    // Taken once: the handler decides what happens next. A retry park an
    // hour out stands in until it does.
    assert!(take_reentries(&mut parks, &mut pacer, &live(&["t1"]), 20_000).is_empty());
    assert_eq!(parks.get("t1").map(|p| p.due_at_ms), Some(10_000 + 3_600_000));
}

#[test]
fn a_handler_that_answers_nothing_does_not_drop_the_stop() {
    let mut parks = ParkList::default();
    let mut pacer = Pacer::default();
    parks.park("t1", "s1", 1_000, "not-yet-reset");
    assert_eq!(take_reentries(&mut parks, &mut pacer, &live(&["t1"]), 1_000).len(), 1);
    // The handler returned early: nothing replaced or removed the retry park.
    let again = take_reentries(&mut parks, &mut pacer, &live(&["t1"]), 1_000 + 3_600_000);
    assert_eq!(again.iter().map(|p| p.terminal_id.as_str()).collect::<Vec<_>>(), vec!["t1"]);
    // A handler that parks replaces the retry; one that answers removes it.
    parks.park("t1", "s1", 5_000_000, "not-yet-reset");
    assert_eq!(parks.get("t1").map(|p| p.due_at_ms), Some(5_000_000));
    parks.forget("t1");
    assert!(parks.get("t1").is_none());
}

#[test]
fn ninety_seven_parks_dispatch_at_six_per_minute_oldest_first() {
    let ids: Vec<String> = (0..97).map(|i| format!("t{i:03}")).collect();
    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let live_set = live(&id_refs);
    let mut parks = ParkList::default();
    let mut pacer = Pacer::default();
    // Parked in reverse id order, due in id order: FIFO is by due time.
    for (i, id) in ids.iter().enumerate().rev() {
        parks.park(id, "s", i as i64, "not-yet-reset");
    }
    // The scan loop's 1.5 s tick, for 17 minutes from t = 1 s.
    let mut batches: Vec<(i64, usize)> = Vec::new();
    let mut order: Vec<String> = Vec::new();
    let mut now = 1_000;
    while now <= 17 * 60_000 {
        let due = take_reentries(&mut parks, &mut pacer, &live_set, now);
        if !due.is_empty() {
            batches.push((now, due.len()));
            order.extend(due.into_iter().map(|p| p.terminal_id));
        }
        now += 1_500;
    }
    assert_eq!(order, ids);
    assert_eq!(batches.len(), 17);
    for (j, (at, n)) in batches.iter().enumerate() {
        assert_eq!(*at, 1_000 + 60_000 * j as i64);
        assert_eq!(*n, if j < 16 { 6 } else { 1 });
    }
}

#[test]
fn a_closed_terminal_leaves_the_list() {
    let mut parks = ParkList::default();
    let mut pacer = Pacer::default();
    parks.park("t1", "s1", 1_000, "not-yet-reset");
    parks.park("t2", "s2", 1_000, "not-yet-reset");
    let due = take_reentries(&mut parks, &mut pacer, &live(&["t2"]), 5_000);
    assert_eq!(
        due.iter().map(|p| p.terminal_id.as_str()).collect::<Vec<_>>(),
        vec!["t2"]
    );
    assert!(parks.get("t1").is_none());

    let mut seen = FirstSight::default();
    seen.read.insert("t1".to_string());
    seen.read.insert("t2".to_string());
    seen.retain_live(&live(&["t2"]));
    assert_eq!(
        seen.unread(&["t1".to_string(), "t2".to_string()], 0),
        vec!["t1".to_string()]
    );
}

// ── First sight ──────────────────────────────────────────────────────────────

/// A fake lifecycle store: terminal id → (session id, transcript path).
struct Store(HashMap<String, (String, PathBuf)>);

impl Store {
    fn pass(&self, candidates: &[String]) -> FirstSightPass {
        first_sight_pass(
            candidates,
            |t| self.0.get(t).cloned(),
            |(sid, _)| sid.clone(),
            |(_, path)| transcript_limit_stop(path),
        )
    }
}

/// Write `body` as `<dir>/<name>.jsonl`.
fn transcript(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(format!("{name}.jsonl"));
    std::fs::write(&path, body).expect("write transcript");
    path
}

fn not_stopped_body() -> String {
    // The seven-day stop answered by a later user prompt.
    let prompt = serde_json::json!({
        "parentUuid": null, "isSidechain": false, "type": "user",
        "uuid": "99999999-0000-4000-8000-000000000099",
        "timestamp": "2026-11-04T21:05:00.000Z",
        "message": {"role": "user", "content": "keep going"}
    });
    format!("{SEVEN_DAY_STOP}{prompt}\n")
}

#[test]
fn a_terminal_whose_transcript_reads_stopped_at_first_sight_is_handled_with_no_screen_match() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store(HashMap::from([
        (
            "parked".to_string(),
            ("s-parked".to_string(), transcript(&dir, "s-parked", SEVEN_DAY_STOP)),
        ),
        (
            "working".to_string(),
            ("s-working".to_string(), transcript(&dir, "s-working", &not_stopped_body())),
        ),
        (
            "gone".to_string(),
            ("s-gone".to_string(), dir.path().join("s-gone.jsonl")),
        ),
    ]));
    let candidates: Vec<String> = ["parked", "working", "gone", "shell"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let pass = store.pass(&candidates);
    assert_eq!(
        pass,
        FirstSightPass {
            read: vec!["parked".into(), "working".into()],
            stopped: vec![("parked".into(), "s-parked".into())],
            unreadable: vec!["gone".into()],
            no_record: vec!["shell".into()],
        }
    );

    let mut seen = FirstSight::default();
    let mut parks = ParkList::default();
    assert_eq!(apply_first_sight(&mut seen, &mut parks, &pass, 50_000), 1);
    // Read once per process. An UNKNOWN read stays unread until its third
    // try; a terminal with no confirmed record is looked up again 15 s later,
    // then after 30 s, 60 s, ... up to 5 min.
    assert_eq!(seen.unread(&candidates, 50_000), vec!["gone".to_string()]);
    assert_eq!(
        seen.unread(&candidates, 65_000),
        vec!["gone".to_string(), "shell".to_string()]
    );
    let shell_again = FirstSightPass {
        no_record: vec!["shell".into()],
        ..FirstSightPass::default()
    };
    apply_first_sight(&mut seen, &mut parks, &shell_again, 65_000);
    assert_eq!(seen.unread(&candidates, 94_999), vec!["gone".to_string()]);
    assert_eq!(seen.unread(&candidates, 95_000).len(), 2);
    for _ in 0..10 {
        apply_first_sight(&mut seen, &mut parks, &shell_again, 100_000);
    }
    assert_eq!(seen.unread(&candidates, 399_999), vec!["gone".to_string()]);
    assert_eq!(seen.unread(&candidates, 400_000).len(), 2);
    let gone_again = FirstSightPass {
        unreadable: vec!["gone".into()],
        ..FirstSightPass::default()
    };
    apply_first_sight(&mut seen, &mut parks, &gone_again, 50_000);
    assert_eq!(seen.unread(&candidates, 50_000), vec!["gone".to_string()]);
    apply_first_sight(&mut seen, &mut parks, &gone_again, 50_000);
    assert!(seen.unread(&candidates, 50_000).is_empty());
    assert_eq!(seen.unread(&candidates, 400_000), vec!["shell".to_string()]);
    // Enrolled as due now: the next tick hands it to the handler.
    let mut pacer = Pacer::default();
    let due = take_reentries(
        &mut parks,
        &mut pacer,
        &live(&["parked", "working", "gone", "shell"]),
        50_000,
    );
    assert_eq!(
        due,
        vec![Park {
            terminal_id: "parked".to_string(),
            claude_session_id: "s-parked".to_string(),
            due_at_ms: 50_000,
        }]
    );
}

#[test]
fn a_background_subagent_stop_is_enrolled_at_first_sight() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sid = "0f1e2d3c-4b5a-4968-8776-655443322110";
    let main = transcript(&dir, sid, SUBAGENT_PARENT);
    let sub = dir.path().join(sid).join("subagents");
    std::fs::create_dir_all(&sub).expect("mkdir");
    std::fs::write(sub.join("agent-a0c1d2e3f4a5b6c7d.jsonl"), SUBAGENT_STOP).expect("write");
    let store = Store(HashMap::from([(
        "db0a8eb9".to_string(),
        (sid.to_string(), main),
    )]));
    let pass = store.pass(&["db0a8eb9".to_string()]);
    assert_eq!(pass.stopped, vec![("db0a8eb9".to_string(), sid.to_string())]);
}

#[test]
fn an_existing_park_keeps_its_due_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store(HashMap::from([(
        "t1".to_string(),
        ("s1".to_string(), transcript(&dir, "s1", SEVEN_DAY_STOP)),
    )]));
    let mut seen = FirstSight::default();
    let mut parks = ParkList::default();
    parks.park("t1", "s1", 900_000, "not-yet-reset");
    let pass = store.pass(&["t1".to_string()]);
    assert_eq!(apply_first_sight(&mut seen, &mut parks, &pass, 1_000), 0);
    assert_eq!(parks.get("t1").map(|p| p.due_at_ms), Some(900_000));
}

#[test]
fn a_fresh_registry_re_derives_the_park() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store(HashMap::from([(
        "t1".to_string(),
        ("s1".to_string(), transcript(&dir, "s1", SEVEN_DAY_STOP)),
    )]));
    let candidates = vec!["t1".to_string()];

    // The first process parks the stop...
    let mut parks = ParkList::default();
    parks.park("t1", "s1", 900_000, "not-yet-reset");
    let mut seen = FirstSight::default();
    apply_first_sight(&mut seen, &mut parks, &store.pass(&candidates), 1_000);

    // ...and a restart loses both. The new process reads the transcript at
    // first sight and enrols it again.
    let mut seen = FirstSight::default();
    let mut parks = ParkList::default();
    assert_eq!(seen.unread(&candidates, 0), candidates);
    let pass = store.pass(&seen.unread(&candidates, 0));
    assert_eq!(apply_first_sight(&mut seen, &mut parks, &pass, 2_000), 1);
    assert_eq!(parks.get("t1").map(|p| p.due_at_ms), Some(2_000));
}

#[test]
fn three_hundred_first_sight_reads_finish_within_a_bound() {
    // About the boot population (plan §Why: 339 terminals on merytshost on
    // 2026-11-05; the plan sizes the boot read at about 430): each
    // transcript is 300 KiB, so the read needs its first tail window and then
    // widens past filler, and every other session has a subagent dir with one
    // file written since its last turn.
    let dir = tempfile::tempdir().expect("tempdir");
    let filler = serde_json::json!({
        "type": "attachment", "timestamp": "2026-11-02T16:30:00.000Z",
        "content": "x".repeat(4_000)
    })
    .to_string();
    let mut padded = SEVEN_DAY_STOP.to_string();
    while padded.len() < 300 * 1024 {
        padded.push_str(&filler);
        padded.push('\n');
    }
    let mut map = HashMap::new();
    let mut candidates = Vec::new();
    for i in 0..300 {
        let tid = format!("t{i:03}");
        let sid = format!("s{i:03}");
        let path = if i % 2 == 0 {
            transcript(&dir, &sid, &padded)
        } else {
            let main = transcript(&dir, &sid, SUBAGENT_PARENT);
            let sub = dir.path().join(&sid).join("subagents");
            std::fs::create_dir_all(&sub).expect("mkdir");
            std::fs::write(sub.join("agent-a0c1d2e3f4a5b6c7d.jsonl"), SUBAGENT_STOP)
                .expect("write");
            main
        };
        map.insert(tid.clone(), (sid, path));
        candidates.push(tid);
    }
    let store = Store(map);
    let started = Instant::now();
    let pass = store.pass(&candidates);
    let elapsed = started.elapsed();
    eprintln!("300 first-sight reads took {elapsed:?}");
    assert_eq!(pass.read.len(), 300);
    assert_eq!(pass.stopped.len(), 300);
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "300 first-sight reads took {elapsed:?}"
    );
}
