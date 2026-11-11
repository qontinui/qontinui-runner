//! Tests for [`super`] — against a temp git repo with REAL linked worktrees.
//!
//! Every "kept" test asserts WHICH guard fired ([`KeepReason`]), not only that
//! the tree survived: `git worktree remove` without `--force` is a second guard
//! that would keep a dirty tree on its own, so a test that checked survival
//! alone could not fail when its conjunct was deleted (policy
//! `testing/a-test-must-be-able-to-fail`).

use std::path::{Path, PathBuf};

use super::*;

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

/// A canonical checkout (`<tmp>/canon`, one commit, nothing ignored — no
/// `.gitignore` at all, so `.claude/` is NOT ignored) and a worktree root
/// (`<tmp>/wt`) to cut allocations under.
struct Fixture {
    _tmp: tempfile::TempDir,
    canonical: PathBuf,
    worktree_root: PathBuf,
    parent_sha: String,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let canonical = tmp.path().join("canon");
        std::fs::create_dir_all(&canonical).unwrap();
        git(&canonical, &["init", "-q", "-b", "main"]);
        std::fs::write(canonical.join("README.md"), "hello\n").unwrap();
        git(&canonical, &["add", "README.md"]);
        git(&canonical, &["commit", "-q", "-m", "init"]);
        let parent_sha = git(&canonical, &["rev-parse", "HEAD"]);
        let worktree_root = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree_root).unwrap();
        Self {
            _tmp: tmp,
            canonical,
            worktree_root,
            parent_sha,
        }
    }

    /// `git worktree add` a fresh allocation tree at
    /// `<root>/<agent_id>/<repo>` on its own branch at `parent_sha`.
    fn allocate(&self, agent_id: &str, repo: &str) -> PathBuf {
        let path = self.worktree_root.join(agent_id).join(repo);
        let branch = format!("agent/{agent_id}-{repo}");
        git(
            &self.canonical,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                path.to_str().unwrap(),
                &self.parent_sha,
            ],
        );
        path
    }

    fn fresh(&self, path: &Path) -> UnspawnedBase {
        UnspawnedBase {
            worktree_path: path.to_path_buf(),
            parent_sha: self.parent_sha.clone(),
            provenance: TreeProvenance::FreshWorktree {
                canonical: self.canonical.clone(),
            },
        }
    }

    fn resolver(&self) -> impl Fn(&Path) -> Option<PathBuf> + Clone + Send + 'static {
        let root = self.worktree_root.clone();
        move |p: &Path| super::super::canonical_paths::allocated_worktree_for_path_in(&root, p)
    }

    fn worktree_listed(&self, path: &Path) -> bool {
        let list = git(&self.canonical, &["worktree", "list", "--porcelain"]);
        let want = std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string();
        list.lines().any(|l| {
            l.strip_prefix("worktree ")
                .is_some_and(|w| w == want || Path::new(w) == path)
        })
    }
}

fn agent() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The scaffolding `provision_session_cwd` writes into a fresh worktree, with
/// the REAL writers for `.claude/commands` and `.claude/skills` (the embedded
/// bundle), plus `.mcp.json` and the degraded breadcrumb.
fn provision_scaffolding(tree: &Path) {
    let commands = tree.join(".claude").join("commands");
    crate::fleet_commands::provision_fleet_commands_into(
        &commands,
        &crate::agent_commands::AgentCommandRegistry::new(),
    )
    .expect("commands provisioned");
    let skills = tree.join(".claude").join("skills");
    crate::fleet_skills::provision_fleet_skills_into(
        &skills,
        &crate::agent_skills::AgentSkillRegistry::new(),
        None,
    )
    .expect("skills provisioned");
    std::fs::write(tree.join(".mcp.json"), "{\"mcpServers\":{}}\n").unwrap();
    std::fs::write(tree.join(".coord-mcp-status"), "degraded\n").unwrap();
}

async fn run(
    fx: &Fixture,
    agent_id: &str,
    bases: Vec<UnspawnedBase>,
    child_started: bool,
) -> RemovalReport {
    remove_unspawned_trees_with(
        agent_id,
        bases,
        child_started,
        "test refusal",
        fx.resolver(),
    )
    .await
}

fn only_outcome(report: &RemovalReport) -> &TreeOutcome {
    assert_eq!(report.trees.len(), 1, "{report:?}");
    &report.trees[0].1
}

/// FIRST, as the plan asks — the test most likely to falsify D1: a repo that
/// does not ignore `.claude/`, after the runner's real scaffolding was written
/// into the tree. Porcelain lists it all as `??`; the tree must still go, and
/// with it the coord-mcp nonce minted for that workdir and the `<agent_id>/`.
#[tokio::test]
async fn a_provisioned_tree_in_a_repo_that_does_not_ignore_claude_is_removed() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    provision_scaffolding(&tree);
    let porcelain = git(&tree, &["status", "--porcelain"]);
    assert!(porcelain.contains("?? .claude/"), "{porcelain}");
    assert!(porcelain.contains("?? .mcp.json"), "{porcelain}");
    // The runner-written agent-root files.
    let agent_root = fx.worktree_root.join(&id);
    std::fs::create_dir_all(agent_root.join(".cargo")).unwrap();
    std::fs::write(agent_root.join(".cargo").join("config.toml"), "[patch]\n").unwrap();
    std::fs::write(agent_root.join(".cargo").join("edit-siblings"), "x\n").unwrap();
    let nonce = crate::coord_mcp::register_agent_proxy_nonce(
        &tree.to_string_lossy(),
        uuid::Uuid::parse_str(&id).unwrap(),
    );
    assert!(crate::coord_mcp::workdir_for_nonce(&nonce).is_some());

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    assert_eq!(only_outcome(&report), &TreeOutcome::Removed);
    assert!(!tree.exists(), "the tree is gone from disk");
    assert!(!fx.worktree_listed(&tree), "and from git's worktree list");
    assert!(report.agent_root_pruned);
    assert!(!agent_root.exists(), "no <agent_id>/ husk is left");
    assert!(
        crate::coord_mcp::workdir_for_nonce(&nonce).is_none(),
        "the workdir's coord-mcp nonce is evicted"
    );
}

/// Conjunct 5 (clean modulo scaffolding): one operator-untracked file beside
/// the scaffolding keeps the tree — the dirty probe, not git, refuses it.
#[tokio::test]
async fn an_operator_untracked_file_keeps_the_tree() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    provision_scaffolding(&tree);
    std::fs::write(tree.join("notes.txt"), "mine\n").unwrap();

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    assert_eq!(
        only_outcome(&report),
        &TreeOutcome::Kept(KeepReason::Dirty(DirtyVerdict::Dirty))
    );
    assert!(tree.join("notes.txt").exists());
    assert!(
        tree.join(".claude").exists(),
        "nothing is deleted from a kept tree, scaffolding included"
    );
    assert!(fx.worktree_root.join(&id).exists());
}

/// Conjunct 5: a tracked-file edit keeps the tree.
#[tokio::test]
async fn a_tracked_file_edit_keeps_the_tree() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    std::fs::write(tree.join("README.md"), "edited\n").unwrap();

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    assert_eq!(
        only_outcome(&report),
        &TreeOutcome::Kept(KeepReason::Dirty(DirtyVerdict::Dirty))
    );
    assert!(tree.exists());
}

/// Conjunct 4: a commit past `parent_sha` keeps an otherwise clean tree (which
/// `git worktree remove` would happily remove on its own).
#[tokio::test]
async fn a_commit_past_parent_sha_keeps_the_tree() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    std::fs::write(tree.join("work.rs"), "fn x() {}\n").unwrap();
    git(&tree, &["add", "work.rs"]);
    git(&tree, &["commit", "-q", "-m", "work"]);
    let head = git(&tree, &["rev-parse", "HEAD"]);

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    assert_eq!(
        only_outcome(&report),
        &TreeOutcome::Kept(KeepReason::HeadMoved { head })
    );
    assert!(tree.exists());
}

/// Conjunct 3: a tree that is a CLONE root (`.git` is a directory), at the
/// right layout, clean and at `parent_sha`, is kept as not-a-linked-worktree.
#[tokio::test]
async fn a_clone_root_is_kept() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.worktree_root.join(&id).join("canon");
    std::fs::create_dir_all(tree.parent().unwrap()).unwrap();
    git(
        fx.worktree_root.as_path(),
        &[
            "clone",
            "-q",
            fx.canonical.to_str().unwrap(),
            tree.to_str().unwrap(),
        ],
    );
    assert_eq!(git(&tree, &["rev-parse", "HEAD"]), fx.parent_sha);

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    assert_eq!(
        only_outcome(&report),
        &TreeOutcome::Kept(KeepReason::NotLinkedWorktree(CloneRootness::CloneRoot))
    );
    assert!(tree.join(".git").is_dir());
}

/// Conjunct 2 (provenance), the vet's Why #4 shape: a `shared_branch` context
/// whose "worktree" is the canonical checkout — here a clean LINKED worktree
/// at `parent_sha`, laid out as an allocation root, so every OTHER conjunct
/// passes. It is never removed.
#[tokio::test]
async fn a_shared_branch_context_is_kept_even_when_every_other_check_passes() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    let base = UnspawnedBase {
        provenance: TreeProvenance::SharedBranch,
        ..fx.fresh(&tree)
    };

    let report = run(&fx, &id, vec![base], false).await;

    assert!(
        matches!(
            only_outcome(&report),
            TreeOutcome::Kept(KeepReason::NotFresh(why)) if why.contains("shared_branch")
        ),
        "{report:?}"
    );
    assert!(tree.exists());
    // Unrecorded provenance is kept the same way.
    let base = UnspawnedBase {
        provenance: TreeProvenance::Unrecorded,
        ..fx.fresh(&tree)
    };
    let report = run(&fx, &id, vec![base], false).await;
    assert!(matches!(
        only_outcome(&report),
        TreeOutcome::Kept(KeepReason::NotFresh(_))
    ));
}

/// Conjunct 2, second half: a fresh tree that is not the allocation root under
/// THIS agent id (another allocation's tree, or one outside the worktree root)
/// is kept.
#[tokio::test]
async fn a_tree_that_is_not_this_allocations_root_is_kept() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");

    let report = run(&fx, &agent(), vec![fx.fresh(&tree)], false).await;
    assert!(
        matches!(
            only_outcome(&report),
            TreeOutcome::Kept(KeepReason::NotFresh(why)) if why.contains("under the agent id: false")
        ),
        "{report:?}"
    );

    // Outside the worktree root entirely.
    let elsewhere = fx
        .canonical
        .parent()
        .unwrap()
        .join("elsewhere")
        .join(&id)
        .join("x");
    git(
        &fx.canonical,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "elsewhere",
            elsewhere.to_str().unwrap(),
        ],
    );
    let report = run(&fx, &id, vec![fx.fresh(&elsewhere)], false).await;
    assert!(
        matches!(
            only_outcome(&report),
            TreeOutcome::Kept(KeepReason::NotFresh(why)) if why.contains("allocation root: false")
        ),
        "{report:?}"
    );
    assert!(tree.exists() && elsewhere.exists());
}

/// Conjunct 1: a failure after the child spawn keeps a tree that would
/// otherwise be removed.
#[tokio::test]
async fn child_started_keeps_the_tree() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");

    let report = run(&fx, &id, vec![fx.fresh(&tree)], true).await;

    assert_eq!(
        only_outcome(&report),
        &TreeOutcome::Kept(KeepReason::ChildStarted)
    );
    assert!(tree.exists());
}

/// A git failure keeps the tree with the reason logged: a linked worktree
/// whose gitfile points at a gitdir that no longer exists cannot answer
/// `rev-parse HEAD`.
#[test]
fn a_git_failure_keeps_the_tree_and_logs_why() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    std::fs::write(tree.join(".git"), "gitdir: /nonexistent/gitdir\n").unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (report, logs) = crate::resource_guard::capture_logs(|| {
        rt.block_on(run(&fx, &id, vec![fx.fresh(&tree)], false))
    });

    assert!(
        matches!(
            only_outcome(&report),
            TreeOutcome::Kept(KeepReason::HeadUnreadable(_))
        ),
        "{report:?}"
    );
    assert!(tree.exists());
    assert!(logs.contains("remove_unspawned: KEPT"), "{logs}");
    assert!(logs.contains("HEAD could not be read"), "{logs}");
}

/// Two trees in one allocation: the `<agent_id>/` dir goes only when the LAST
/// is removed; while one is kept, the agent root and its `.cargo/` stay.
#[tokio::test]
async fn the_agent_dir_is_pruned_only_after_the_last_tree() {
    let fx = Fixture::new();
    let id = agent();
    let a = fx.allocate(&id, "canon");
    let b = fx.allocate(&id, "sibling");
    std::fs::write(b.join("README.md"), "dirty\n").unwrap();
    let cargo = fx
        .worktree_root
        .join(&id)
        .join(".cargo")
        .join("config.toml");
    std::fs::create_dir_all(cargo.parent().unwrap()).unwrap();
    std::fs::write(&cargo, "[patch]\n").unwrap();

    let report = run(&fx, &id, vec![fx.fresh(&a), fx.fresh(&b)], false).await;

    assert_eq!(report.trees[0].1, TreeOutcome::Removed);
    assert!(matches!(report.trees[1].1, TreeOutcome::Kept(_)));
    assert!(!report.agent_root_pruned);
    assert!(
        cargo.exists(),
        "the agent root is untouched while a tree is kept"
    );
}

/// After the last tree is removed, an `<agent_id>/` holding anything other
/// than the two runner-written `.cargo/` files is kept.
#[tokio::test]
async fn an_agent_dir_holding_anything_else_is_kept() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    let agent_root = fx.worktree_root.join(&id);
    std::fs::create_dir_all(agent_root.join(".cargo")).unwrap();
    std::fs::write(agent_root.join(".cargo").join("config.toml"), "[patch]\n").unwrap();
    std::fs::write(agent_root.join("operator-notes.md"), "keep me\n").unwrap();

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    assert_eq!(only_outcome(&report), &TreeOutcome::Removed);
    assert!(!report.agent_root_pruned);
    assert!(agent_root.join("operator-notes.md").exists());
    assert!(
        !agent_root.join(".cargo").exists(),
        "the runner-written files (and the dir they emptied) are gone"
    );
}

/// The context wrapper reports spawn-failed EXACTLY ONCE per allocation,
/// whether or not the trees were removed, and marks removed trees so `Drop`
/// skips them.
#[tokio::test]
async fn the_wrapper_reports_once_per_allocation_removed_or_not() {
    use crate::agent_worktree::isolated_edit::IsolatedEditContext;

    for (child_started, expect_removed) in [(false, true), (true, false)] {
        let fx = Fixture::new();
        let id = agent();
        let tree = fx.allocate(&id, "canon");
        let sibling = fx.allocate(&id, "sibling");
        let wt = |path: &Path, repo: &str| crate::agent_worktree::MaterializedWorktree {
            repo: repo.to_string(),
            branch: format!("agent/{id}-{repo}"),
            parent_sha: fx.parent_sha.clone(),
            worktree_path: path.to_path_buf(),
            push_ref: String::new(),
            parent_sha_provenance: Default::default(),
        };
        let mut ctx = IsolatedEditContext::for_test_with_agent_id(
            id.clone(),
            vec![wt(&tree, "canon"), wt(&sibling, "sibling")],
            Vec::new(),
        );
        for p in [&tree, &sibling] {
            ctx.set_provenance_for_test(
                p,
                TreeProvenance::FreshWorktree {
                    canonical: fx.canonical.clone(),
                },
            );
        }
        let mut posts = Vec::new();
        let report = ctx
            .remove_unspawned_with(
                "test door",
                child_started,
                "refused",
                |a, e| posts.push((a, e)),
                fx.resolver(),
            )
            .await;
        assert_eq!(
            posts,
            vec![(uuid::Uuid::parse_str(&id).unwrap(), "refused".to_string())],
            "exactly one post for the allocation (child_started={child_started})"
        );
        assert_eq!(report.removed_all(), expect_removed, "{report:?}");
        assert_eq!(tree.exists(), !expect_removed);
        assert_eq!(sibling.exists(), !expect_removed);
    }
}

/// Phase 3: the unreported form (the gate continuation's, whose own post is
/// the allocation's one report) removes a fresh, empty tree when no child
/// started, keeps it when one did, and marks what it removed for `Drop`.
#[tokio::test]
async fn the_unreported_wrapper_removes_only_when_no_child_started() {
    use crate::agent_worktree::isolated_edit::IsolatedEditContext;

    for (child_started, expect_removed) in [(false, true), (true, false)] {
        let fx = Fixture::new();
        let id = agent();
        let tree = fx.allocate(&id, "canon");
        provision_scaffolding(&tree);
        let mut ctx = IsolatedEditContext::for_test_with_agent_id(
            id.clone(),
            vec![crate::agent_worktree::MaterializedWorktree {
                repo: "canon".to_string(),
                branch: format!("agent/{id}-canon"),
                parent_sha: fx.parent_sha.clone(),
                worktree_path: tree.clone(),
                push_ref: String::new(),
                parent_sha_provenance: Default::default(),
            }],
            Vec::new(),
        );
        ctx.set_provenance_for_test(
            &tree,
            TreeProvenance::FreshWorktree {
                canonical: fx.canonical.clone(),
            },
        );
        let report = ctx
            .remove_unspawned_unreported_with("test door", child_started, "refused", fx.resolver())
            .await;
        assert_eq!(report.removed_all(), expect_removed, "{report:?}");
        assert_eq!(tree.exists(), !expect_removed);
        if child_started {
            assert_eq!(
                only_outcome(&report),
                &TreeOutcome::Kept(KeepReason::ChildStarted)
            );
        }
    }
}

/// Review fix 6: a removed tree's `agent/…` branch is safe-deleted
/// (`git branch -d`) from the canonical checkout.
#[tokio::test]
async fn a_removed_trees_agent_branch_is_deleted() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    let branch = format!("agent/{id}-canon");
    assert!(!git(&fx.canonical, &["branch", "--list", &branch]).is_empty());

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    assert_eq!(only_outcome(&report), &TreeOutcome::Removed);
    assert!(
        git(&fx.canonical, &["branch", "--list", &branch]).is_empty(),
        "the agent branch `worktree add -b` cut is gone"
    );
}

/// Review fix 4: when `git worktree remove` refuses AFTER the scaffolding was
/// deleted (here: a locked worktree), the keep reason says the scaffolding is
/// already gone.
#[tokio::test]
async fn a_refused_remove_after_scaffolding_cleanup_says_it_was_deleted() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    provision_scaffolding(&tree);
    git(&fx.canonical, &["worktree", "lock", tree.to_str().unwrap()]);

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    match only_outcome(&report) {
        TreeOutcome::Kept(KeepReason::RemoveFailed(why)) => {
            assert!(why.contains("runner scaffolding already deleted"), "{why}")
        }
        other => panic!("expected RemoveFailed, got {other:?}"),
    }
    assert!(tree.exists(), "the locked tree is kept");
    assert!(
        !tree.join(".mcp.json").exists(),
        "its scaffolding was deleted"
    );
}

/// Review fix 5: with `status.showUntrackedFiles=no` the dirty probe's plain
/// porcelain hides an operator's untracked file; the removal's own listing
/// passes `--untracked-files=all`, so the scaffolding guard sees and refuses it
/// (before `git worktree remove` would be the only thing left to).
#[tokio::test]
async fn an_untracked_file_hidden_by_show_untracked_no_still_keeps_the_tree() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    git(
        &fx.canonical,
        &["config", "status.showUntrackedFiles", "no"],
    );
    provision_scaffolding(&tree);
    std::fs::write(tree.join("notes.txt"), "operator work\n").unwrap();

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    match only_outcome(&report) {
        TreeOutcome::Kept(KeepReason::Scaffolding(why)) => {
            assert!(why.contains("notes.txt"), "{why}")
        }
        other => panic!("expected the scaffolding listing to refuse, got {other:?}"),
    }
    assert!(tree.join("notes.txt").exists());
}

/// Review fix 8: an untracked `.claude` (and a `.claude/commands`) that is a
/// SYMLINK pointing outside the tree is unlinked, never followed — the target
/// survives the removal.
#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_claude_dir_is_unlinked_and_its_target_survives() {
    for nested in [false, true] {
        let fx = Fixture::new();
        let id = agent();
        let tree = fx.allocate(&id, "canon");
        let outside = fx
            .worktree_root
            .parent()
            .unwrap()
            .join(format!("outside-{nested}"));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.md"), "operator file\n").unwrap();
        if nested {
            std::fs::create_dir_all(tree.join(".claude")).unwrap();
            std::os::unix::fs::symlink(&outside, tree.join(".claude").join("commands")).unwrap();
        } else {
            std::os::unix::fs::symlink(&outside, tree.join(".claude")).unwrap();
        }

        let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

        assert_eq!(
            only_outcome(&report),
            &TreeOutcome::Removed,
            "nested={nested}"
        );
        assert!(!tree.exists());
        assert!(
            outside.join("keep.md").exists(),
            "the symlink's target survives (nested={nested})"
        );
    }
}

/// Review fix 7: the removal waits for a credential-helper install still in
/// flight for the allocation before it judges (and removes) anything.
#[tokio::test]
async fn the_removal_waits_for_an_in_flight_credential_install() {
    let fx = Fixture::new();
    let id = agent();
    let tree = fx.allocate(&id, "canon");
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    crate::credential_helper::track_install(
        &id,
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }),
    );

    let report = run(&fx, &id, vec![fx.fresh(&tree)], false).await;

    assert!(
        done.load(std::sync::atomic::Ordering::SeqCst),
        "the install finished before the removal returned"
    );
    assert_eq!(only_outcome(&report), &TreeOutcome::Removed);
}
