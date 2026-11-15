//! `IsolatedEditContext::drop`, driven through `Drop` itself (edge `25c2bc0b`
//! of plan `2026-11-13-spawn-refused-tree-followups`): the credential cleanup
//! waits for an in-flight install, still runs when an install never finishes,
//! and a tree `remove_unspawned` removed is not handed to the fs-observer push
//! or the edit-effect verify.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::credential_helper::{config_file_path, track_install};

fn context(agent_id: &str) -> IsolatedEditContext {
    IsolatedEditContext::for_test_with_agent_id(agent_id.to_string(), Vec::new(), Vec::new())
}

/// Drive the runtime (an install is a task on it) until `done` or `bound`.
async fn wait_until(bound: Duration, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + bound;
    while !done() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// (i) The arming test: an install for the context's agent id writes the
/// helper config 300 ms in. Dropping the context must clean up AFTER that
/// install, so the file it wrote is gone — a cleanup that ran ahead of it, or
/// none at all, leaves the file behind.
#[tokio::test]
async fn drop_cleans_up_after_an_in_flight_install() {
    let id = format!("drop-arm-{}", uuid::Uuid::new_v4());
    let path = config_file_path(&id);
    let written = path.clone();
    let wrote = Arc::new(AtomicBool::new(false));
    let wrote_flag = wrote.clone();
    track_install(
        &id,
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            std::fs::write(&written, "{}").unwrap();
            wrote_flag.store(true, Ordering::SeqCst);
        }),
    );

    drop(context(&id));

    wait_until(Duration::from_secs(5), || {
        wrote.load(Ordering::SeqCst) && !path.exists()
    })
    .await;
    // Past the install, give a cleanup that ran AHEAD of it the chance to be
    // seen as one: the file it never deleted would still be there.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let left = path.exists();
    let _ = std::fs::remove_file(&path);
    assert!(wrote.load(Ordering::SeqCst), "the install wrote its config");
    assert!(!left, "Drop's cleanup ran after the install and removed it");
}

/// (ii) The abort arm through `Drop`: with the context's cleanup wait short
/// and an install that never finishes, the cleanup still runs. Under the
/// default 30 s wait the file would still be there at the 5 s bound.
#[tokio::test]
async fn drop_cleans_up_even_when_an_install_never_finishes() {
    let id = format!("drop-abort-{}", uuid::Uuid::new_v4());
    let path = config_file_path(&id);
    std::fs::write(&path, "{}").unwrap();
    track_install(
        &id,
        tokio::spawn(tokio::time::sleep(Duration::from_secs(3600))),
    );
    let mut ctx = context(&id);
    ctx.cred_cleanup_wait = Duration::from_millis(100);

    drop(ctx);

    wait_until(Duration::from_secs(5), || !path.exists()).await;
    let left = path.exists();
    let _ = std::fs::remove_file(&path);
    assert!(!left, "Drop's cleanup ran despite the stuck install");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args);
    let out = crate::process_helpers::output_with_timeout(cmd, std::time::Duration::from_secs(60))
        .expect("git runs");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn materialized(repo: &str, path: &Path, parent_sha: &str) -> MaterializedWorktree {
    MaterializedWorktree {
        repo: repo.to_string(),
        branch: String::new(),
        parent_sha: parent_sha.to_string(),
        worktree_path: path.to_path_buf(),
        push_ref: String::new(),
        parent_sha_provenance: Default::default(),
    }
}

/// (iii) The removed-tree skip, in the real order: `remove_unspawned_unreported_with`
/// consumes the context, so its `Drop` runs when the removal returns. Of two
/// trees, the fresh one is removed and the unrecorded one kept; `Drop` must
/// hand ONLY the kept tree to its producers.
#[tokio::test]
async fn drop_after_a_removal_observes_only_the_trees_still_on_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let canonical = tmp.path().join("canon");
    std::fs::create_dir_all(&canonical).unwrap();
    git(&canonical, &["init", "-q", "-b", "main"]);
    std::fs::write(canonical.join("README.md"), "hello\n").unwrap();
    git(&canonical, &["add", "README.md"]);
    git(&canonical, &["commit", "-q", "-m", "init"]);
    let sha = git(&canonical, &["rev-parse", "HEAD"]);
    let root = tmp.path().join("wt");
    let id = uuid::Uuid::new_v4().to_string();
    let allocate = |repo: &str| -> PathBuf {
        let path = root.join(&id).join(repo);
        git(
            &canonical,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("agent/{id}-{repo}"),
                path.to_str().unwrap(),
                &sha,
            ],
        );
        path
    };
    let removed = allocate("canon");
    let kept = allocate("sibling");
    let mut ctx = IsolatedEditContext::for_test_with_agent_id(
        id.clone(),
        vec![
            materialized("canon", &removed, &sha),
            materialized("sibling", &kept, &sha),
        ],
        Vec::new(),
    );
    ctx.set_provenance_for_test(
        &removed,
        TreeProvenance::FreshWorktree {
            canonical: canonical.clone(),
        },
    );
    let resolver_root = root.clone();
    let resolver = move |p: &Path| {
        super::super::canonical_paths::allocated_worktree_for_path_in(&resolver_root, p)
    };

    let report = ctx
        .remove_unspawned_unreported_with("test door", false, "refused", resolver)
        .await;

    assert_eq!(report.removed_paths(), vec![removed.clone()], "{report:?}");
    assert!(!removed.exists(), "the fresh tree is off disk");
    assert!(kept.exists(), "the unrecorded tree is never removed");
    assert_eq!(
        drop_observed_recorder::take(&id),
        Some(vec![kept]),
        "Drop hands the producers the kept tree and never the removed one"
    );
}
