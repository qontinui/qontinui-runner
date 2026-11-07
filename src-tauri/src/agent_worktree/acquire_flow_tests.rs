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
    ])
    .await;
    let o = overlap();
    let deadline =
        RetryDeadline::with_delays(Duration::from_secs(2), tokio::time::Instant::now(), 1, 1);
    let out = Run::new()
        .go(&coord, Some(&o), SharedBranchPolicy::Honor, deadline)
        .await;
    match out {
        Err(AllocateError::Other(msg)) => {
            assert!(msg.contains("returned 502"), "{msg}");
            assert!(msg.contains("attempts"), "{msg}");
        }
        other => panic!("expected Other, got {other:?}"),
    }
    assert!(coord.hits(ALLOCATE) >= 2);
    assert_eq!(coord.hits(RELEASE), 1);
    // No allocate succeeded: no row id is known, nothing to retire.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(coord.hits(DONE), 0);
}

#[tokio::test]
async fn final_answers_are_never_retried() {
    let _amb = super::test_coord::isolate();
    // A held claim before the allocate.
    let coord = ScriptedCoord::start(vec![(
        CLAIMS,
        vec![(
            200,
            r#"{"result":"held","current_holder":"peer"}"#.to_string(),
        )],
    )])
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
            vec![(200, r#"{"matched":1,"status":"abandoned"}"#.to_string())],
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
            "branch": "agent/x/shared",
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
            vec![(200, r#"{"matched":1,"status":"abandoned"}"#.to_string())],
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
            r#"{"status":"allocated","abandon_refused":"census_dirty"}"#.to_string(),
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
