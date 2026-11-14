//! Unit tests of the pre-claim account check: [`claim_if_account_ready`] driven
//! with counting posters, the stamp's wire value, and the source order that makes
//! the check a pre-claim one. The arming tests, which enter through coord's
//! delivery, are in `agent_runtime::tests::no_account_arming`.

use super::*;
use std::cell::{Cell, RefCell};

/// What one [`claim_if_account_ready`] run did through its injected seams.
#[derive(Debug)]
struct Counted {
    result: PreClaim,
    account_reads: usize,
    claims: usize,
    stamps: Vec<String>,
}

/// Drive the transition with a counting claim poster and a counting deferral
/// poster that forwards to the REAL [`defer_continuation_unclaimed`], so the
/// local-claim release under test is the production one. The caller holds an
/// isolated ambient, which is what keeps that forward off any real coord.
async fn run_counted(consume_target: ConsumeTarget, ready: bool) -> Counted {
    let account_reads = Cell::new(0usize);
    let claims = Cell::new(0usize);
    let stamps = RefCell::new(Vec::new());
    let device_id = uuid::Uuid::now_v7();
    let result = claim_if_account_ready(
        consume_target,
        "test-instance",
        || {
            account_reads.set(account_reads.get() + 1);
            std::future::ready(ready)
        },
        |_gate_id| {
            claims.set(claims.get() + 1);
            std::future::ready(SpawnDecision::Spawn)
        },
        |stamp: String| {
            stamps.borrow_mut().push(stamp.clone());
            defer_continuation_unclaimed(consume_target, device_id, stamp)
        },
    )
    .await;
    Counted {
        result,
        account_reads: account_reads.get(),
        claims: claims.get(),
        stamps: stamps.into_inner(),
    }
}

#[tokio::test]
async fn gate_not_ready_defers_once_and_never_claims() {
    let _amb = crate::test_env::isolated_ambient();
    let gate_id = uuid::Uuid::now_v7();
    assert!(claim_gate_dispatch(gate_id), "the dispatcher's claim");

    let run = run_counted(ConsumeTarget::Gate(gate_id, 0), false).await;

    assert_eq!(run.claims, 0, "a missing account must not post the claim");
    assert_eq!(
        run.stamps,
        vec!["no_authenticated_account:test-instance".to_string()]
    );
    assert_eq!(
        run.result,
        PreClaim::Deferred {
            stamp: "no_authenticated_account:test-instance".to_string()
        }
    );
    assert!(
        claim_gate_dispatch(gate_id),
        "the deferral must release the local claim so a re-delivery can dispatch"
    );
    release_gate_dispatch(gate_id);
}

#[tokio::test]
async fn dispatch_not_ready_defers_and_releases_without_a_claim() {
    let _amb = crate::test_env::isolated_ambient();
    let dispatch_id = uuid::Uuid::now_v7();
    assert!(
        claim_dispatch_dispatch(dispatch_id),
        "the dispatcher's claim"
    );

    let run = run_counted(ConsumeTarget::Dispatch(dispatch_id), false).await;

    assert_eq!(run.claims, 0);
    assert_eq!(run.stamps.len(), 1, "it goes through the deferral poster");
    assert!(matches!(run.result, PreClaim::Deferred { .. }), "{run:?}");
    assert!(
        claim_dispatch_dispatch(dispatch_id),
        "a Dispatch deferral must release its local claim too"
    );
    release_dispatch_dispatch(dispatch_id);
}

/// The legacy target has no re-delivery, so a deferral there would be a silent
/// drop. It is not even asked about: `ready` is false and nothing notices.
#[tokio::test]
async fn legacy_target_passes_through_without_reading_the_predicate() {
    let _amb = crate::test_env::isolated_ambient();

    let run = run_counted(ConsumeTarget::None, false).await;

    assert_eq!(run.account_reads, 0, "the predicate is not read for it");
    assert_eq!(run.claims, 0, "the legacy target posts no claim");
    assert!(run.stamps.is_empty(), "and no deferral");
    assert_eq!(run.result, PreClaim::Ready { decision: None });
}

#[tokio::test]
async fn gate_ready_claims_exactly_once_and_hands_the_decision_back() {
    let _amb = crate::test_env::isolated_ambient();

    let run = run_counted(ConsumeTarget::Gate(uuid::Uuid::now_v7(), 0), true).await;

    assert_eq!(run.account_reads, 1);
    assert_eq!(run.claims, 1);
    assert!(run.stamps.is_empty(), "a ready dispatch defers nothing");
    assert_eq!(
        run.result,
        PreClaim::Ready {
            decision: Some(SpawnDecision::Spawn)
        }
    );
}

/// The work-unit path has no claim-before-spawn, whatever the check says.
#[tokio::test]
async fn dispatch_ready_posts_no_claim_and_defers_nothing() {
    let _amb = crate::test_env::isolated_ambient();

    let run = run_counted(ConsumeTarget::Dispatch(uuid::Uuid::now_v7()), true).await;

    assert_eq!(run.account_reads, 1);
    assert_eq!(run.claims, 0);
    assert!(run.stamps.is_empty());
    assert_eq!(run.result, PreClaim::Ready { decision: None });
}

/// The read's own answer passes through. A read that panicked is no evidence
/// about the accounts, so it reads as ready: stamping `no_authenticated_account`
/// would tell coord something nobody observed, and the spawn seam still refuses.
#[tokio::test]
async fn an_account_read_that_did_not_finish_proceeds_to_the_claim() {
    assert!(settle_account_read(tokio::task::spawn_blocking(|| true)).await);
    assert!(!settle_account_read(tokio::task::spawn_blocking(|| false)).await);
    let panicked = tokio::task::spawn_blocking(|| -> bool { panic!("the read blew up") });
    assert!(settle_account_read(panicked).await);
}

/// A wire value: coord groups pending rows on the class before the first colon.
#[test]
fn the_no_account_stamp_follows_the_class_detail_grammar() {
    assert_eq!(
        no_account_stamp_reason("primary"),
        "no_authenticated_account:primary"
    );
    assert_eq!(
        no_account_stamp_reason("test-9877"),
        "no_authenticated_account:test-9877"
    );
}

/// Source order in [`run_gate_continuation_inner`]. The counting tests above prove
/// the transition is right; only this proves the transition is where the claim
/// is, with the registry check before it and no worktree cost ahead of it.
#[test]
fn the_account_check_precedes_the_claim_in_the_inner_run() {
    let source = include_str!("../agent_runtime.rs");
    let (prod, _) = source
        .split_once("#[cfg(test)]\nmod no_account_tests;")
        .expect("the test module declaration");
    let (_, rest) = prod
        .split_once("async fn run_gate_continuation_inner(")
        .expect("the inner run exists");
    let body = rest.split_once("\n}\n").map_or(rest, |(body, _)| body);
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("{needle} is in run_gate_continuation_inner"))
    };

    let authorization = at("crate::agent_authorization::authorize_spawn(");
    let account_check = at("claim_if_account_ready(");
    let claim = at("post_continuation_claim(");
    let worktree = at("acquire_continuation_workdir(");
    assert!(
        authorization < account_check,
        "the account check runs after step 1b's authorization"
    );
    assert!(
        account_check < claim,
        "the account check must precede the consume claim, or a runner with no account \
         burns the continuation"
    );
    assert!(claim < worktree, "the claim precedes the worktree allocate");
    assert_eq!(
        body.matches("post_continuation_claim(").count(),
        1,
        "the transition's injected poster is the only claim this run posts"
    );
}
