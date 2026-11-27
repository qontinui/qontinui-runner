//! Plan `2026-11-13-spawn-refused-tree-followups` Phase 2 (follow-up edge
//! `3fa9784f`): `run_continuation_headless`'s model-gateway refusal driven end
//! to end — through the real `spawn_claude_child` refusal, the real
//! `remove_unspawned_continuation` arm, and the real remover — against a real
//! repository and worktree under the isolated ambient workspace root.

#![cfg(test)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::{run_continuation_headless, AnchorReservation};
use crate::agent_worktree::isolated_edit::IsolatedEditContext;
use crate::agent_worktree::remove_unspawned::{
    KeepReason, RemovalReport, TreeOutcome, TreeProvenance,
};
use crate::agent_worktree::shared_branch_restore::{RestoreKeep, RestoreOutcome};

tokio::task_local! {
    /// What the headless arm's removal reported, for the ONE test future that
    /// scoped this slot ([`refuse_headless`]). Task-local, not a process-global
    /// map: a test reads only its own run, and nothing outlives the scope.
    static OBSERVED_REMOVAL: RefCell<Option<Option<RemovalReport>>>;
}

/// Called by `run_continuation_headless`'s refusal arm with what
/// `remove_unspawned_continuation` returned. A no-op outside a test future
/// that scoped [`OBSERVED_REMOVAL`].
pub(super) fn observe_removal(report: Option<&RemovalReport>) {
    let _ = OBSERVED_REMOVAL.try_with(|slot| *slot.borrow_mut() = Some(report.cloned()));
}

/// The two worktree-root overrides `agent_worktree_root` honours when
/// absolute. `isolated_ambient` captures and restores them but removes
/// neither, so an exported one would put the fixture's `git worktree add`
/// outside its tempdir.
const WORKTREE_ROOT_OVERRIDES: &[&str] = &["QONTINUI_WORKTREE_ROOT", "COORD_WORKTREE_ROOT"];

/// Remove both overrides for the test's lifetime; the guard restores them on
/// drop. Taken under the ambient fixture's env lock.
fn clear_worktree_root_overrides() -> crate::test_env::EnvVarRestore {
    let restore = crate::test_env::EnvVarRestore::capture(WORKTREE_ROOT_OVERRIDES);
    for key in WORKTREE_ROOT_OVERRIDES {
        std::env::remove_var(key);
    }
    restore
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

/// The fixture: a canonical checkout under the isolated workspace root, a
/// real `git worktree add` of it at the allocation root for `agent_id` on
/// agent `branch`, a context over that tree with `provenance`, and the sticky
/// gateway marker that makes `spawn_claude_child` refuse.
struct Fixture {
    canonical: PathBuf,
    tree: PathBuf,
    branch: String,
    agent_id: uuid::Uuid,
    ctx: IsolatedEditContext,
}

/// What the fixture placed, handed to the caller's provenance closure: the
/// canonical checkout, and the agent branch and commit its worktree sits on.
struct Placed<'a> {
    canonical: &'a Path,
    branch: &'a str,
    sha: &'a str,
}

fn fixture(sandbox: &Path, provenance: impl FnOnce(&Placed<'_>) -> TreeProvenance) -> Fixture {
    let ws = crate::workspace_paths::runner_workspace_root()
        .into_root()
        .expect("the isolated ambient workspace root");
    let canonical = ws.join("canon");
    std::fs::create_dir_all(&canonical).unwrap();
    git(&canonical, &["init", "-q", "-b", "main"]);
    std::fs::write(canonical.join("README.md"), "hi\n").unwrap();
    git(&canonical, &["add", "README.md"]);
    git(&canonical, &["commit", "-q", "-m", "init"]);
    let sha = git(&canonical, &["rev-parse", "HEAD"]);
    let agent_id = uuid::Uuid::new_v4();
    let tree = crate::agent_worktree::canonical_paths::agent_worktree_root(&canonical)
        .join(agent_id.to_string())
        .join("canon");
    assert!(
        tree.starts_with(sandbox),
        "the fixture's worktree {} must sit inside its sandbox {}",
        tree.display(),
        sandbox.display()
    );
    let branch = format!("agent/{agent_id}-canon");
    git(
        &canonical,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            tree.to_str().unwrap(),
            &sha,
        ],
    );
    let tree_provenance = provenance(&Placed {
        canonical: &canonical,
        branch: &branch,
        sha: &sha,
    });
    let mut ctx = IsolatedEditContext::for_test_with_agent_id(
        agent_id.to_string(),
        vec![crate::agent_worktree::MaterializedWorktree {
            repo: "canon".to_string(),
            branch: branch.clone(),
            parent_sha: sha,
            worktree_path: tree.clone(),
            push_ref: String::new(),
            parent_sha_provenance: Default::default(),
        }],
        Vec::new(),
    );
    ctx.set_provenance_for_test(&tree, tree_provenance);

    // The sticky marker of a configured gateway, with no gateway in settings:
    // `model_gateway::spawn_refusal` reads that as unresolved and refuses.
    let g = crate::model_gateway::ModelGatewaySettings {
        base_url: Some("https://headless-gw.example.com".to_string()),
        api_key_helper: Some("echo headless-key".to_string()),
        ..Default::default()
    }
    .resolve()
    .unwrap()
    .unwrap();
    crate::model_gateway::write_marker_at(&crate::model_gateway::gateway_dir().unwrap(), &g)
        .unwrap();

    Fixture {
        canonical,
        tree,
        branch,
        agent_id,
        ctx,
    }
}

/// Does `git worktree list` in `canonical` list `tree`?
fn worktree_listed(canonical: &Path, tree: &Path) -> bool {
    let want = std::fs::canonicalize(tree).unwrap_or_else(|_| tree.to_path_buf());
    git(canonical, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| {
            let p = Path::new(p);
            p == tree || std::fs::canonicalize(p).is_ok_and(|c| c == want)
        })
}

/// Run the refused continuation inside a scoped [`OBSERVED_REMOVAL`] slot and
/// return its result with what the removal reported (`None` when the arm
/// never reached the remover).
async fn refuse_headless(
    f: Fixture,
) -> (
    anyhow::Result<super::HeadlessExit>,
    Option<Option<RemovalReport>>,
) {
    OBSERVED_REMOVAL
        .scope(RefCell::new(None), async move {
            let res = run_continuation_headless(
                f.agent_id,
                &f.tree.to_string_lossy(),
                "p",
                &super::claude_bin_path(),
                AnchorReservation::none(),
                uuid::Uuid::new_v4(),
                Some(f.ctx),
                &super::LaunchFrame::default(),
            )
            .await;
            (res, OBSERVED_REMOVAL.with(|slot| slot.borrow_mut().take()))
        })
        .await
}

/// A headless continuation refused by an unresolved model gateway takes its
/// fresh, never-spawned tree back off disk and out of the canonical's
/// worktree list.
#[tokio::test]
async fn a_headless_continuation_refused_by_the_gateway_removes_its_fresh_tree() {
    let amb = crate::test_env::isolated_ambient();
    let _overrides = clear_worktree_root_overrides();
    let f = fixture(amb.dir(), |p| TreeProvenance::FreshWorktree {
        canonical: p.canonical.to_path_buf(),
    });
    let (canonical, tree) = (f.canonical.clone(), f.tree.clone());
    assert!(
        worktree_listed(&canonical, &tree),
        "the fixture's worktree is listed"
    );

    let (res, observed) = refuse_headless(f).await;

    let err = match res {
        Ok(exit) => panic!("an unresolved gateway must refuse the continuation: {exit:?}"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("model gateway"), "{err}");
    let report = observed
        .expect("the refusal arm reached the remover")
        .expect("the continuation held a context, so the removal reported");
    assert_eq!(report.trees.len(), 1, "{report:?}");
    assert_eq!(report.trees[0].1, TreeOutcome::Removed, "{report:?}");
    assert!(!tree.exists(), "the fresh, unspawned tree was removed");
    assert!(
        !worktree_listed(&canonical, &tree),
        "the canonical no longer lists the removed worktree"
    );
}

/// The negative twin: the same refusal over a `SharedBranch` placement keeps
/// the tree — still on disk, still listed, still on its agent branch — and
/// says why: the shared-branch arm hands it to the checkout restore, which
/// here switched nothing back (the placement recorded no switch: `prior_ref`
/// is the agent branch itself) and so leaves it where it is.
#[tokio::test]
async fn a_headless_continuation_refused_by_the_gateway_keeps_a_shared_branch_tree() {
    let amb = crate::test_env::isolated_ambient();
    let _overrides = clear_worktree_root_overrides();
    let f = fixture(amb.dir(), |p| TreeProvenance::SharedBranch {
        branch: p.branch.to_string(),
        prior_ref: p.branch.to_string(),
        switched_to_sha: p.sha.to_string(),
    });
    let (canonical, tree, branch) = (f.canonical.clone(), f.tree.clone(), f.branch.clone());

    let (res, observed) = refuse_headless(f).await;

    let err = match res {
        Ok(exit) => panic!("an unresolved gateway must refuse the continuation: {exit:?}"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("model gateway"), "{err}");
    let report = observed
        .expect("the refusal arm reached the remover")
        .expect("the continuation held a context, so the removal reported");
    assert_eq!(report.trees.len(), 1, "{report:?}");
    assert_eq!(
        report.trees[0].1,
        TreeOutcome::Kept(KeepReason::SharedBranchCheckout(RestoreOutcome::Kept(
            RestoreKeep::NothingSwitched
        ))),
        "{report:?}"
    );
    assert!(tree.exists(), "a shared-branch tree is never removed");
    assert!(
        worktree_listed(&canonical, &tree),
        "the canonical still lists the kept worktree"
    );
    assert_eq!(
        git(&tree, &["symbolic-ref", "--short", "HEAD"]),
        branch,
        "nothing was switched, so nothing is undone"
    );
}
