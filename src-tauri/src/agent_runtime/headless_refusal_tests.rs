//! Plan `2026-11-13-spawn-refused-tree-followups` Phase 2 (follow-up edge
//! `3fa9784f`): `run_continuation_headless`'s model-gateway refusal driven end
//! to end — through the real `spawn_claude_child` refusal, the real
//! `remove_unspawned_continuation` arm, and the real remover — against a real
//! repository and worktree under the isolated ambient workspace root.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{run_continuation_headless, AnchorReservation};
use crate::agent_worktree::isolated_edit::IsolatedEditContext;
use crate::agent_worktree::remove_unspawned::{
    KeepReason, RemovalReport, TreeOutcome, TreeProvenance,
};

/// What the continuation remover reported, per allocation (`agent_id`) —
/// recorded by `remove_unspawned_continuation`, so a test can read which
/// guard kept a tree when the door itself discards the report.
static REMOVAL_REPORTS: Mutex<Option<HashMap<String, RemovalReport>>> = Mutex::new(None);

/// Record `report` for allocation `agent_id` (called from
/// `remove_unspawned_continuation`).
pub(super) fn record_continuation_removal(agent_id: String, report: RemovalReport) {
    REMOVAL_REPORTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(agent_id, report);
}

fn removal_report(agent_id: &str) -> Option<RemovalReport> {
    REMOVAL_REPORTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_mut()
        .and_then(|m| m.remove(agent_id))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The fixture: a canonical checkout under the isolated workspace root, a
/// real `git worktree add` of it at the allocation root for `agent_id`, a
/// context over that tree with `provenance`, and the sticky gateway marker
/// that makes `spawn_claude_child` refuse.
struct Fixture {
    canonical: PathBuf,
    tree: PathBuf,
    agent_id: uuid::Uuid,
    ctx: IsolatedEditContext,
}

fn fixture(provenance: impl FnOnce(&Path) -> TreeProvenance) -> Fixture {
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
    let mut ctx = IsolatedEditContext::for_test_with_agent_id(
        agent_id.to_string(),
        vec![crate::agent_worktree::MaterializedWorktree {
            repo: "canon".to_string(),
            branch,
            parent_sha: sha,
            worktree_path: tree.clone(),
            push_ref: String::new(),
            parent_sha_provenance: Default::default(),
        }],
        Vec::new(),
    );
    ctx.set_provenance_for_test(&tree, provenance(&canonical));

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

async fn refuse_headless(f: Fixture) -> anyhow::Result<super::HeadlessExit> {
    run_continuation_headless(
        f.agent_id,
        &f.tree.to_string_lossy(),
        "p",
        AnchorReservation::none(),
        uuid::Uuid::new_v4(),
        Some(f.ctx),
    )
    .await
}

/// A headless continuation refused by an unresolved model gateway takes its
/// fresh, never-spawned tree back off disk and out of the canonical's
/// worktree list.
#[tokio::test]
async fn a_headless_continuation_refused_by_the_gateway_removes_its_fresh_tree() {
    let _amb = crate::test_env::isolated_ambient();
    let f = fixture(|canonical| TreeProvenance::FreshWorktree {
        canonical: canonical.to_path_buf(),
    });
    let (canonical, tree, agent_id) = (f.canonical.clone(), f.tree.clone(), f.agent_id);
    assert!(worktree_listed(&canonical, &tree), "the fixture's worktree is listed");

    let res = refuse_headless(f).await;

    let err = match res {
        Ok(exit) => panic!("an unresolved gateway must refuse the continuation: {exit:?}"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("model gateway"), "{err}");
    let report = removal_report(&agent_id.to_string()).expect("the remover ran");
    assert_eq!(report.trees.len(), 1, "{report:?}");
    assert_eq!(report.trees[0].1, TreeOutcome::Removed, "{report:?}");
    assert!(!tree.exists(), "the fresh, unspawned tree was removed");
    assert!(
        !worktree_listed(&canonical, &tree),
        "the canonical no longer lists the removed worktree"
    );
}

/// The negative twin: the same refusal over a `SharedBranch` placement keeps
/// the tree — still on disk, still listed — and says why (`NotFresh`).
#[tokio::test]
async fn a_headless_continuation_refused_by_the_gateway_keeps_a_shared_branch_tree() {
    let _amb = crate::test_env::isolated_ambient();
    let f = fixture(|_| TreeProvenance::SharedBranch);
    let (canonical, tree, agent_id) = (f.canonical.clone(), f.tree.clone(), f.agent_id);

    let res = refuse_headless(f).await;

    let err = match res {
        Ok(exit) => panic!("an unresolved gateway must refuse the continuation: {exit:?}"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("model gateway"), "{err}");
    let report = removal_report(&agent_id.to_string()).expect("the remover ran");
    assert_eq!(report.trees.len(), 1, "{report:?}");
    assert!(
        matches!(&report.trees[0].1, TreeOutcome::Kept(KeepReason::NotFresh(_))),
        "{report:?}"
    );
    assert!(tree.exists(), "a shared-branch tree is never removed");
    assert!(
        worktree_listed(&canonical, &tree),
        "the canonical still lists the kept worktree"
    );
}
