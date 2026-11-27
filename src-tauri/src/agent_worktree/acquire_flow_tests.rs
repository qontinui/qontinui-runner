//! `allocate_and_materialize_with_claim` driven end to end against a scripted
//! local coord ([`super::test_coord::ScriptedCoord`]) — the retry (plan
//! `2026-10-31-continuation-worktree-acquire-failure-strands-the-allocation-and-tells-the-session-nothing`
//! Phase 1) and the retire of an unwound allocation (Phase 3), through real
//! HTTP rather than the pure helpers alone.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use super::acquire_retry::RetryDeadline;
use super::test_coord::ScriptedCoord;
use super::{
    allocate_and_materialize_with_claim, AllocateError, MaterializeOutcome, RepoRequest,
    SharedBranchPolicy,
};

const CLAIMS: &str = "/claims/acquire";
const RELEASE: &str = "/claims/release";
const ALLOCATE: &str = "/agents/allocate";
const DONE: &str = "/coord/worktree-done/";

const REPO: &str = "qontinui-runner";

fn claimed() -> (u16, String) {
    (200, r#"{"result":"claimed","ttl_seconds":300}"#.to_string())
}

fn wait_answer() -> (u16, String) {
    (
        200,
        r#"{"agent_id":"agent-w","worktrees":[],"isolation":{"mode":"wait","reason":"disk_or_slot"}}"#
            .to_string(),
    )
}

fn shared_branch_answer() -> (u16, String) {
    (
        200,
        serde_json::json!({
            "agent_id": "agent-sb",
            "worktrees": [{
                "repo": REPO,
                "branch": "agent/x/shared",
                "parent_sha": "0000000000000000000000000000000000000000",
                "worktree_path": "agent-worktrees/agent-sb/qontinui-runner",
                "status": "allocated",
            }],
            "isolation": {"mode": "shared_branch"},
        })
        .to_string(),
    )
}

/// A budget with a zero-second delay sequence: retries happen at once, so the
/// tests exercise the loop's shape on real time without its patience.
fn quick_budget() -> RetryDeadline {
    RetryDeadline::with_delays(Duration::from_secs(30), tokio::time::Instant::now(), 0, 0)
}

struct Run {
    canonical: tempfile::TempDir,
}

impl Run {
    fn new() -> Self {
        Self {
            canonical: tempfile::tempdir().expect("tempdir"),
        }
    }

    async fn go(
        &self,
        coord: &ScriptedCoord,
        overlap: Option<&[String]>,
        policy: SharedBranchPolicy,
        deadline: RetryDeadline,
    ) -> Result<MaterializeOutcome, AllocateError> {
        let mut paths: HashMap<String, PathBuf> = HashMap::new();
        paths.insert(REPO.to_string(), self.canonical.path().to_path_buf());
        allocate_and_materialize_with_claim(
            &coord.base,
            &uuid::Uuid::nil(),
            None,
            &[RepoRequest {
                repo: REPO.to_string(),
                parent_sha: None,
            }],
            Some("test intent"),
            overlap,
            &paths,
            None,
            None,
            None,
            policy,
            &[],
            deadline,
        )
        .await
    }
}

fn overlap() -> Vec<String> {
    vec!["src/**".to_string()]
}

/// The retire is detached; wait (bounded) for it to land.
async fn wait_for_hits(coord: &ScriptedCoord, prefix: &str, n: usize) -> usize {
    for _ in 0..100 {
        if coord.hits(prefix) >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    coord.hits(prefix)
}

#[tokio::test]
async fn a_failing_then_succeeding_claim_is_retried_and_the_acquire_proceeds() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![
        (CLAIMS, vec![(503, "{}".to_string()), claimed()]),
        (ALLOCATE, vec![wait_answer()]),
        (RELEASE, vec![(200, "{}".to_string())]),
        // Scripted so the DONE assertion below can fail: an unscripted route
        // records no hit at all.
        (DONE, vec![(404, "{}".to_string())]),
    ])
    .await;
    let o = overlap();
    let out = Run::new()
        .go(&coord, Some(&o), SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(matches!(out, Ok(MaterializeOutcome::Wait(_))), "{out:?}");
    assert_eq!(coord.hits(CLAIMS), 2, "the 503 was retried once");
    assert_eq!(coord.hits(ALLOCATE), 1);
    // The retried claim was held when coord answered wait, so it is released.
    assert_eq!(coord.hits(RELEASE), 1);
    // A wait commits no row: nothing to retire.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(coord.hits(DONE), 0);
}

#[tokio::test]
async fn a_zero_budget_makes_one_attempt_as_before() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![
        (CLAIMS, vec![(503, "{}".to_string()), claimed()]),
        (ALLOCATE, vec![wait_answer()]),
    ])
    .await;
    let o = overlap();
    let out = Run::new()
        .go(
            &coord,
            Some(&o),
            SharedBranchPolicy::Honor,
            RetryDeadline::none(),
        )
        .await;
    match out {
        Err(AllocateError::Other(msg)) => {
            assert!(msg.contains("returned 503"), "{msg}");
            assert!(!msg.contains("attempts"), "one attempt only: {msg}");
        }
        other => panic!("expected a final Other, got {other:?}"),
    }
    assert_eq!(coord.hits(CLAIMS), 1);
    assert_eq!(coord.hits(ALLOCATE), 0);
}

#[tokio::test]
async fn a_failing_allocate_is_retried_and_keeps_its_claim_until_final() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![
        (CLAIMS, vec![claimed()]),
        (
            ALLOCATE,
            vec![
                (
                    500,
                    r#"{"error":"db_error","op":"agent.worktree.allocate"}"#.to_string(),
                ),
                wait_answer(),
            ],
        ),
        (RELEASE, vec![(200, "{}".to_string())]),
    ])
    .await;
    let o = overlap();
    let out = Run::new()
        .go(&coord, Some(&o), SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(matches!(out, Ok(MaterializeOutcome::Wait(_))), "{out:?}");
    assert_eq!(coord.hits(ALLOCATE), 2);
    // Released once, by the wait — never by the retried 500.
    assert_eq!(coord.hits(RELEASE), 1);
}

#[tokio::test]
async fn an_exhausted_budget_still_releases_the_claims_already_held() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![
        (CLAIMS, vec![claimed()]),
        (ALLOCATE, vec![(502, "bad gateway".to_string())]),
        (RELEASE, vec![(200, "{}".to_string())]),
        // coord's answer when no attempt committed a row for the id.
        (DONE, vec![(404, "{}".to_string())]),
    ])
    .await;
    let o = overlap();
    let deadline =
        RetryDeadline::with_delays(Duration::from_secs(2), tokio::time::Instant::now(), 1, 1);
    let out = Run::new()
        .go(&coord, Some(&o), SharedBranchPolicy::Honor, deadline)
        .await;
    match out {
        Err(AllocateError::Unavailable(msg)) => {
            assert!(msg.contains("returned 502"), "{msg}");
            assert!(msg.contains("attempts"), "{msg}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert!(coord.hits(ALLOCATE) >= 2);
    assert_eq!(coord.hits(RELEASE), 1);
    // No allocate answered, so no row path is known; the run's own id is the
    // only handle, and it is handed back by that id alone (plan 2026-11-26 D4).
    // The first 404 is "not yet" (the timed-out attempt may still be running
    // on coord), so it is asked exactly once more.
    assert_eq!(wait_for_hits(&coord, DONE, 2).await, 2);
    let id = coord.bodies(ALLOCATE)[0]["client_allocation_id"].clone();
    let ask = serde_json::json!({"client_allocation_id": id, "abandoned": true});
    assert_eq!(coord.bodies(DONE), vec![ask.clone(), ask]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(coord.hits(DONE), 2, "re-asked once, not more");
}

// ── plan 2026-11-26-allocate-under-a-continuation-burst-sends-isolated-
//    continuations-to-the-shared-workspace-root, Phase 2 ──

/// D3: the allocate's attempt is bounded by `ALLOCATE_REQUEST_TIMEOUT`, not by
/// the claims' `PER_ATTEMPT_TIMEOUT`. Under test those are 3 s and 1 s; the
/// stand-in answers after 2 s, in the gap between them. Before D3 the budgeted
/// path used `PER_ATTEMPT_TIMEOUT`, so this answer timed out on every attempt
/// and the continuation took the fallback cwd.
#[tokio::test]
async fn a_slow_but_alive_allocate_inside_its_own_bound_counts() {
    use super::acquire_retry::PER_ATTEMPT_TIMEOUT;
    let delay = Duration::from_secs(2);
    assert!(delay > PER_ATTEMPT_TIMEOUT && delay < super::ALLOCATE_REQUEST_TIMEOUT);
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start_with_delays(
        vec![(ALLOCATE, vec![wait_answer()])],
        vec![(ALLOCATE, delay)],
    )
    .await;
    let started = std::time::Instant::now();
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(matches!(out, Ok(MaterializeOutcome::Wait(_))), "{out:?}");
    assert!(started.elapsed() >= delay, "{:?}", started.elapsed());
    assert_eq!(coord.hits(ALLOCATE), 1, "answered on the first attempt");
}

/// D4: one id per allocation run, the same on every retry of it, a UUIDv7 as
/// coord requires; a second run mints its own.
#[tokio::test]
async fn every_retry_of_one_allocation_run_carries_the_same_client_allocation_id() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![(
        ALLOCATE,
        vec![
            (503, "{}".to_string()),
            (500, r#"{"error":"db_error"}"#.to_string()),
            wait_answer(),
        ],
    )])
    .await;
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(matches!(out, Ok(MaterializeOutcome::Wait(_))), "{out:?}");
    let ids: Vec<serde_json::Value> = coord
        .bodies(ALLOCATE)
        .iter()
        .map(|b| b["client_allocation_id"].clone())
        .collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert!(ids.iter().all(|i| *i == ids[0]), "{ids:?}");
    let id: uuid::Uuid = serde_json::from_value(ids[0].clone()).expect("a uuid");
    assert_eq!(id.get_version_num(), 7);

    let _ = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, quick_budget())
        .await;
    let next = coord.bodies(ALLOCATE)[3]["client_allocation_id"].clone();
    assert_ne!(next, ids[0], "a new run mints a new id");
}

/// D4: a coord that cannot store the id (or refuses it) is re-sent WITHOUT
/// it, inside the same attempt, instead of failing every allocate on it.
#[tokio::test]
async fn a_coord_that_cannot_store_the_id_is_retried_without_it() {
    let _amb = super::test_coord::isolate();
    for refusal in [
        (
            503,
            r#"{"error":"schema_migration_pending","column":"coord.agent_worktrees.client_allocation_id","cause":"undefined_column"}"#,
        ),
        (
            400,
            r#"{"error":"client_allocation_id_invalid","reason":"skewed"}"#,
        ),
    ] {
        let coord = ScriptedCoord::start(vec![(
            ALLOCATE,
            vec![(refusal.0, refusal.1.to_string()), wait_answer()],
        )])
        .await;
        // A zero budget too: the re-send happens inside the one attempt.
        let out = Run::new()
            .go(
                &coord,
                None,
                SharedBranchPolicy::Honor,
                RetryDeadline::none(),
            )
            .await;
        assert!(matches!(out, Ok(MaterializeOutcome::Wait(_))), "{out:?}");
        let bodies = coord.bodies(ALLOCATE);
        assert_eq!(bodies.len(), 2, "{refusal:?}");
        assert!(bodies[0].get("client_allocation_id").is_some());
        assert!(
            bodies[1].get("client_allocation_id").is_none(),
            "{refusal:?}"
        );
    }
}

/// D4: a retry answered `client_allocation_id_duplicate` means an earlier
/// attempt committed and its answer was lost. Nothing new was allocated and no
/// token came back, so the run ends Unavailable and hands the rows back by id.
#[tokio::test]
async fn a_duplicate_id_answer_ends_the_run_and_hands_its_rows_back() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![
        (
            ALLOCATE,
            vec![
                (503, "{}".to_string()),
                (
                    409,
                    r#"{"error":"client_allocation_id_duplicate","client_allocation_id":"x"}"#
                        .to_string(),
                ),
            ],
        ),
        (
            DONE,
            vec![(200, r#"{"matched":1,"trigger_signal":"agent_done","statuses":["allocated"],"abandoned":1,"abandon_refused":null,"note":"retired"}"#.to_string())],
        ),
    ])
    .await;
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(
        matches!(&out, Err(AllocateError::Unavailable(m)) if m.contains("client_allocation_id_duplicate")),
        "{out:?}"
    );
    assert_eq!(coord.hits(ALLOCATE), 2, "a duplicate is final");
    assert_eq!(wait_for_hits(&coord, DONE, 1).await, 1);
    let id = coord.bodies(ALLOCATE)[0]["client_allocation_id"].clone();
    assert_eq!(coord.bodies(DONE)[0]["client_allocation_id"], id);
}

/// D2's input: only a BUDGETED run that exhausts on transient failures is
/// Unavailable. A zero-budget caller's single transient failure stays Other,
/// and still hands its id back (the hand-back is gated on the id having been
/// sent on an unanswered attempt, not on the error type); a
/// `repo_not_registered` stays typed and hands nothing back.
#[tokio::test]
async fn only_a_budgeted_exhaustion_is_unavailable() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![
        (ALLOCATE, vec![(503, "{}".to_string())]),
        (DONE, vec![(404, "{}".to_string())]),
    ])
    .await;
    let out = Run::new()
        .go(
            &coord,
            None,
            SharedBranchPolicy::Honor,
            RetryDeadline::none(),
        )
        .await;
    assert!(
        matches!(&out, Err(AllocateError::Other(m)) if m.contains("returned 503")),
        "{out:?}"
    );
    assert_eq!(coord.hits(ALLOCATE), 1);
    assert!(
        wait_for_hits(&coord, DONE, 1).await >= 1,
        "the id is handed back"
    );

    let coord = ScriptedCoord::start(vec![
        (
            ALLOCATE,
            vec![(
                409,
                format!(r#"{{"error":"repo_not_registered","repo":"{REPO}"}}"#),
            )],
        ),
        (DONE, vec![(404, "{}".to_string())]),
    ])
    .await;
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(
        matches!(out, Err(AllocateError::RepoNotRegistered(_))),
        "{out:?}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(coord.hits(DONE), 0, "an answered refusal committed nothing");
}

/// A4: a run that DROPPED the id (coord refused it) hands nothing back by it,
/// even when an earlier id-carrying attempt went unanswered: coord stores no
/// row under an id it cannot store, and an id-only ask there would 503.
#[tokio::test]
async fn a_run_that_dropped_the_id_hands_nothing_back() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![
        (
            ALLOCATE,
            vec![
                (503, "{}".to_string()),
                (
                    503,
                    r#"{"error":"schema_migration_pending","column":"coord.agent_worktrees.client_allocation_id","cause":"undefined_column"}"#
                        .to_string(),
                ),
                (502, "bad gateway".to_string()),
            ],
        ),
        (DONE, vec![(404, "{}".to_string())]),
    ])
    .await;
    let deadline =
        RetryDeadline::with_delays(Duration::from_secs(2), tokio::time::Instant::now(), 1, 1);
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, deadline)
        .await;
    assert!(matches!(out, Err(AllocateError::Unavailable(_))), "{out:?}");
    let bodies = coord.bodies(ALLOCATE);
    assert!(bodies.len() >= 3, "{bodies:?}");
    assert!(bodies[2].get("client_allocation_id").is_none());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(coord.hits(DONE), 0);
}

#[tokio::test]
async fn final_answers_are_never_retried() {
    let _amb = super::test_coord::isolate();
    // A held claim before the allocate.
    let coord = ScriptedCoord::start(vec![
        (
            CLAIMS,
            vec![(
                200,
                r#"{"result":"held","current_holder":"peer"}"#.to_string(),
            )],
        ),
        // Scripted so the DONE assertion below can fail: an unscripted route
        // records no hit at all.
        (DONE, vec![(404, "{}".to_string())]),
    ])
    .await;
    let o = overlap();
    let out = Run::new()
        .go(&coord, Some(&o), SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(
        matches!(out, Err(AllocateError::ClaimConflict(_))),
        "{out:?}"
    );
    assert_eq!(coord.hits(CLAIMS), 1);
    assert_eq!(coord.hits(ALLOCATE), 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        coord.hits(DONE),
        0,
        "a pre-allocate conflict retires nothing"
    );

    // A 4xx from the claim door.
    let coord = ScriptedCoord::start(vec![(CLAIMS, vec![(400, "{}".to_string())])]).await;
    let out = Run::new()
        .go(&coord, Some(&o), SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(matches!(out, Err(AllocateError::Other(_))), "{out:?}");
    assert_eq!(coord.hits(CLAIMS), 1);

    // repo_not_registered from the allocate: typed, and its own retry is the
    // caller's.
    let coord = ScriptedCoord::start(vec![(
        ALLOCATE,
        vec![(
            409,
            format!(r#"{{"error":"repo_not_registered","repo":"{REPO}"}}"#),
        )],
    )])
    .await;
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(
        matches!(out, Err(AllocateError::RepoNotRegistered(_))),
        "{out:?}"
    );
    assert_eq!(coord.hits(ALLOCATE), 1);

    // Any other 4xx from the allocate.
    let coord = ScriptedCoord::start(vec![(ALLOCATE, vec![(422, "{}".to_string())])]).await;
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(matches!(out, Err(AllocateError::Other(_))), "{out:?}");
    assert_eq!(coord.hits(ALLOCATE), 1);

    // A wait.
    let coord = ScriptedCoord::start(vec![(ALLOCATE, vec![wait_answer()])]).await;
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(matches!(out, Ok(MaterializeOutcome::Wait(_))), "{out:?}");
    assert_eq!(coord.hits(ALLOCATE), 1);
}

#[tokio::test]
async fn a_post_allocate_unwind_retires_coords_rows_and_keeps_its_error() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![
        (ALLOCATE, vec![shared_branch_answer()]),
        (
            DONE,
            vec![(200, r#"{"matched":1,"trigger_signal":"agent_done","statuses":["allocated"],"abandoned":1,"abandon_refused":null,"note":"retired"}"#.to_string())],
        ),
    ])
    .await;
    // A caller that refuses the shared checkout unwinds right after coord
    // answered with a row.
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Refuse, quick_budget())
        .await;
    assert!(matches!(out, Err(AllocateError::Other(_))), "{out:?}");
    assert_eq!(wait_for_hits(&coord, DONE, 1).await, 1);
    assert_eq!(
        coord.bodies(DONE),
        vec![serde_json::json!({
            "worktree_path": "agent-worktrees/agent-sb/qontinui-runner",
            "abandoned": true,
        })]
    );
    assert_eq!(
        coord.paths(DONE),
        vec![format!("/coord/worktree-done/{}", uuid::Uuid::nil())]
    );
}

#[tokio::test]
async fn a_post_allocate_claim_conflict_retires() {
    let _amb = super::test_coord::isolate();
    let coord = ScriptedCoord::start(vec![
        (ALLOCATE, vec![shared_branch_answer()]),
        (
            CLAIMS,
            vec![(
                200,
                r#"{"result":"held","current_holder":"peer"}"#.to_string(),
            )],
        ),
        (
            DONE,
            vec![(200, r#"{"matched":1,"trigger_signal":"agent_done","statuses":["allocated"],"abandoned":1,"abandon_refused":null,"note":"retired"}"#.to_string())],
        ),
    ])
    .await;
    let out = Run::new()
        .go(&coord, None, SharedBranchPolicy::Honor, quick_budget())
        .await;
    assert!(
        matches!(out, Err(AllocateError::ClaimConflict(_))),
        "{out:?}"
    );
    assert_eq!(wait_for_hits(&coord, DONE, 1).await, 1);
}

#[tokio::test]
async fn a_refused_or_failing_retire_never_changes_the_returned_error() {
    let _amb = super::test_coord::isolate();
    for done in [
        (
            200,
            r#"{"matched":1,"trigger_signal":"agent_done","statuses":["allocated"],"abandoned":0,"abandon_refused":"retire_shortfall: 0 of 1 predicted row(s) moved","note":"x"}"#.to_string(),
        ),
        (404, "{}".to_string()),
        (503, "{}".to_string()),
    ] {
        let coord = ScriptedCoord::start(vec![
            (ALLOCATE, vec![shared_branch_answer()]),
            (DONE, vec![done]),
        ])
        .await;
        let out = Run::new()
            .go(&coord, None, SharedBranchPolicy::Refuse, quick_budget())
            .await;
        match out {
            Err(AllocateError::Other(msg)) => {
                assert!(!msg.contains("worktree-done"), "{msg}");
            }
            other => panic!("expected the shared-branch refusal, got {other:?}"),
        }
        assert!(wait_for_hits(&coord, DONE, 1).await >= 1);
    }
}
