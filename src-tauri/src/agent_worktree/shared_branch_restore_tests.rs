//! Tests for [`super`] — against real temp repos, switched by the REAL
//! [`checkout_shared_branches`], so `prior_ref` / `switched_to_sha` are what
//! an allocation records and the canonical's excludes are what it writes.
//!
//! Every "kept" test asserts WHICH proof fired ([`RestoreKeep`]) and that HEAD
//! did not move, so deleting a proof is observable (policy
//! `testing/a-test-must-be-able-to-fail`).

use std::path::{Path, PathBuf};

use super::*;
use crate::agent_worktree::remove_unspawned::{KeepReason, TreeOutcome, TreeProvenance};
use crate::agent_worktree::{
    checkout_shared_branches, ActiveClaim, CoordAllocatedWorktree, MaterializedWorktree,
    ParentShaProvenance, SharedBranchRepo,
};

const AGENT_BRANCH: &str = "agent/sb-test";

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

/// A canonical checkout on `main` with one commit and NO `.gitignore` and no
/// `.git/info/exclude` entries, so nothing the runner writes is ignored unless
/// the shared-branch switch itself arranged it.
struct Canon {
    _tmp: tempfile::TempDir,
    path: PathBuf,
    parent_sha: String,
}

impl Canon {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("canon");
        std::fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "-q", "-b", "main"]);
        // Hermetic ignore rules: a machine-global ignore (`core.excludesFile`,
        // or the XDG `git/ignore` default) can hide files these tests create
        // (e.g. `.claude/settings.local.json`), making `status --porcelain`
        // empty and the restore proceed. A repo-local `core.excludesFile`
        // pointing at an empty file overrides both, and is seen by the
        // production git calls under test as well as by this helper.
        let empty_excludes = tmp.path().join("empty-excludes");
        std::fs::write(&empty_excludes, "").unwrap();
        git(
            &path,
            &[
                "config",
                "core.excludesFile",
                &empty_excludes.to_string_lossy().replace('\\', "/"),
            ],
        );
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        git(&path, &["add", "README.md"]);
        git(&path, &["commit", "-q", "-m", "init"]);
        let parent_sha = git(&path, &["rev-parse", "HEAD"]);
        Self {
            _tmp: tmp,
            path,
            parent_sha,
        }
    }

    /// Switch the canonical onto [`AGENT_BRANCH`] exactly as a `shared_branch`
    /// allocation does.
    fn switch(&self) -> SharedBranchRepo {
        let row = CoordAllocatedWorktree {
            repo: "canon".to_string(),
            branch: AGENT_BRANCH.to_string(),
            parent_sha: self.parent_sha.clone(),
            worktree_path: String::new(),
            status: "allocated".to_string(),
            push_ref: String::new(),
            origin: Some("requested".to_string()),
            sibling_store_sha: None,
            parent_sha_provenance: ParentShaProvenance::default(),
        };
        let mut out = checkout_shared_branches(&[(&row, &self.path)]).expect("switch");
        assert_eq!(out.len(), 1);
        let sb = out.remove(0);
        assert_eq!(self.head_ref(), format!("refs/heads/{AGENT_BRANCH}"));
        sb
    }

    fn head_ref(&self) -> String {
        git(&self.path, &["rev-parse", "--symbolic-full-name", "HEAD"])
    }

    fn head(&self) -> String {
        git(&self.path, &["rev-parse", "HEAD"])
    }

    fn agent_branch_exists(&self) -> bool {
        let mut cmd = crate::process_helpers::no_window("git");
        cmd.arg("-C").arg(&self.path).args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{AGENT_BRANCH}"),
        ]);
        crate::process_helpers::output_with_timeout(cmd, std::time::Duration::from_secs(60))
            .expect("git runs")
            .status
            .success()
    }
}

fn restore(c: &Canon, sb: &SharedBranchRepo) -> RestoreOutcome {
    restore_refused_shared_branch(&c.path, &sb.branch, &sb.prior_ref, &sb.switched_to_sha)
}

fn exclude(c: &Canon) -> String {
    std::fs::read_to_string(c.path.join(".git").join("info").join("exclude")).unwrap_or_default()
}

fn has_block(c: &Canon) -> bool {
    let ex = exclude(c);
    ex.contains(&format!("{BLOCK_BEGIN}{AGENT_BRANCH}"))
        || ex
            .lines()
            .any(|l| PROVISIONED_CLAUDE_EXCLUDES.contains(&l.trim()))
}

/// Seed an operator line before the allocation, as an operator's own
/// `.git/info/exclude` would carry.
fn seed_operator_line(c: &Canon) {
    let path = c.path.join(".git").join("info").join("exclude");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut ex = std::fs::read_to_string(&path).unwrap_or_default();
    ex.push_str("operator-before\n");
    std::fs::write(&path, ex).unwrap();
}

/// An operator line appended WHILE the allocation (and its block) stands.
fn append_operator_line(c: &Canon) {
    let path = c.path.join(".git").join("info").join("exclude");
    let mut ex = std::fs::read_to_string(&path).unwrap_or_default();
    ex.push_str("operator-after\n");
    std::fs::write(&path, ex).unwrap();
}

/// The block is gone, every line outside it survived, and an operator file
/// under `.claude/commands/` shows in `git status` again.
fn assert_block_removed_and_operator_visible(c: &Canon) {
    let ex = exclude(c);
    assert!(!has_block(c), "the allocation's block is gone: {ex:?}");
    assert!(!ex.contains(BLOCK_END), "{ex:?}");
    for kept in [
        "operator-before",
        "operator-after",
        ".mcp.json",
        ".coord-mcp-status",
    ] {
        assert!(
            ex.lines().any(|l| l.trim() == kept),
            "line {kept:?} outside the block survives: {ex:?}"
        );
    }
    std::fs::create_dir_all(c.path.join(".claude").join("commands")).unwrap();
    std::fs::write(c.path.join(".claude/commands/op.md"), "mine\n").unwrap();
    let status = git(&c.path, &["status", "--porcelain", "--untracked-files=all"]);
    assert!(
        status.contains(".claude/commands/op.md"),
        "an operator file under .claude/commands/ is visible again: {status:?}"
    );
}

// ---------------------------------------------------------------- Restored

#[test]
fn a_checkout_switched_from_a_branch_is_put_back_on_it() {
    let c = Canon::new();
    let sb = c.switch();
    assert_eq!(sb.prior_ref, "main");
    assert_eq!(sb.switched_to_sha, c.parent_sha);

    assert_eq!(restore(&c, &sb), RestoreOutcome::Restored);
    assert_eq!(c.head_ref(), "refs/heads/main");
}

#[test]
fn a_checkout_switched_from_a_detached_head_is_detached_again() {
    let c = Canon::new();
    git(&c.path, &["checkout", "-q", "--detach", "HEAD"]);
    let sb = c.switch();
    assert_eq!(sb.prior_ref, c.parent_sha, "a detached prior is its sha");

    assert_eq!(restore(&c, &sb), RestoreOutcome::Restored);
    assert_eq!(c.head_ref(), "HEAD", "detached again");
    assert_eq!(c.head(), c.parent_sha);
}

#[test]
fn the_agent_branch_survives_a_restore() {
    let c = Canon::new();
    let sb = c.switch();
    assert_eq!(restore(&c, &sb), RestoreOutcome::Restored);
    assert!(
        c.agent_branch_exists(),
        "the restore never deletes the agent branch, even one this allocation created"
    );
}

#[test]
fn a_second_restore_switches_nothing() {
    let c = Canon::new();
    let sb = c.switch();
    assert_eq!(restore(&c, &sb), RestoreOutcome::Restored);

    assert_eq!(
        restore(&c, &sb),
        RestoreOutcome::Kept(RestoreKeep::NotOnAgentBranch("refs/heads/main".into()))
    );
    assert_eq!(c.head_ref(), "refs/heads/main");
}

// ------------------------------------------------------------------- Kept

#[test]
fn a_checkout_already_on_the_agent_branch_reads_nothing_switched() {
    let c = Canon::new();
    // A live session put the checkout on the agent branch before this spawn.
    git(&c.path, &["checkout", "-q", "-b", AGENT_BRANCH]);
    let sb = c.switch();
    assert_eq!(sb.prior_ref, AGENT_BRANCH);

    assert_eq!(
        restore(&c, &sb),
        RestoreOutcome::Kept(RestoreKeep::NothingSwitched)
    );
    assert_eq!(c.head_ref(), format!("refs/heads/{AGENT_BRANCH}"));
    assert!(
        !has_block(&c),
        "an allocation that switched nothing adds no exclude block (it could be a live one's)"
    );
}

#[test]
fn a_checkout_someone_switched_away_is_left_alone() {
    let c = Canon::new();
    let sb = c.switch();
    git(&c.path, &["checkout", "-q", "-b", "operator/topic"]);

    assert_eq!(
        restore(&c, &sb),
        RestoreOutcome::Kept(RestoreKeep::NotOnAgentBranch(
            "refs/heads/operator/topic".into()
        ))
    );
    assert_eq!(c.head_ref(), "refs/heads/operator/topic");
}

#[test]
fn a_commit_on_the_agent_branch_keeps_it() {
    let c = Canon::new();
    let sb = c.switch();
    std::fs::write(c.path.join("work.txt"), "w\n").unwrap();
    git(&c.path, &["add", "work.txt"]);
    git(&c.path, &["commit", "-q", "-m", "work"]);
    let head = c.head();

    assert_eq!(
        restore(&c, &sb),
        RestoreOutcome::Kept(RestoreKeep::BranchMoved { head })
    );
    assert_eq!(c.head_ref(), format!("refs/heads/{AGENT_BRANCH}"));
}

fn assert_dirty_and_parked(c: &Canon, sb: &SharedBranchRepo, needle: &str) {
    match restore(c, sb) {
        RestoreOutcome::Kept(RestoreKeep::Dirty(p)) => {
            assert!(p.contains(needle), "porcelain {p:?} should name {needle}")
        }
        other => panic!("expected Kept(Dirty), got {other:?}"),
    }
    assert_eq!(c.head_ref(), format!("refs/heads/{AGENT_BRANCH}"));
}

#[test]
fn a_tracked_change_keeps_the_checkout() {
    let c = Canon::new();
    let sb = c.switch();
    std::fs::write(c.path.join("README.md"), "edited\n").unwrap();
    assert_dirty_and_parked(&c, &sb, "README.md");
}

#[test]
fn an_untracked_file_keeps_the_checkout() {
    let c = Canon::new();
    let sb = c.switch();
    std::fs::write(c.path.join("notes.txt"), "mine\n").unwrap();
    assert_dirty_and_parked(&c, &sb, "notes.txt");
}

#[test]
fn an_untracked_file_hidden_by_show_untracked_files_no_still_keeps_it() {
    let c = Canon::new();
    let sb = c.switch();
    git(&c.path, &["config", "status.showUntrackedFiles", "no"]);
    std::fs::write(c.path.join("notes.txt"), "mine\n").unwrap();
    assert_dirty_and_parked(&c, &sb, "notes.txt");
}

#[test]
fn an_untracked_claude_file_outside_the_provisioned_trees_keeps_it() {
    // Nothing is filtered: `.claude/settings.local.json` is not something the
    // runner provisions, so it is the operator's and the checkout stays put.
    let c = Canon::new();
    let sb = c.switch();
    std::fs::create_dir_all(c.path.join(".claude")).unwrap();
    std::fs::write(c.path.join(".claude/settings.local.json"), "{}\n").unwrap();
    assert_dirty_and_parked(&c, &sb, ".claude/settings.local.json");
}

#[test]
fn a_deleted_prior_branch_keeps_the_checkout() {
    let c = Canon::new();
    let sb = c.switch();
    git(&c.path, &["branch", "-q", "-D", "main"]);

    assert_eq!(
        restore(&c, &sb),
        RestoreOutcome::Kept(RestoreKeep::PriorRefMissing("main".into()))
    );
    assert_eq!(c.head_ref(), format!("refs/heads/{AGENT_BRANCH}"));
}

#[test]
fn an_incomplete_record_never_acts() {
    let c = Canon::new();
    let sb = c.switch();
    let out = restore_refused_shared_branch(&c.path, &sb.branch, &sb.prior_ref, "");
    assert!(
        matches!(out, RestoreOutcome::Kept(RestoreKeep::ReadFailed(_))),
        "{out:?}"
    );
    assert_eq!(c.head_ref(), format!("refs/heads/{AGENT_BRANCH}"));
}

// -------------------------------------------------- through the spawn door

/// Write into the canonical exactly what a session spawned there provisions:
/// the cwd `.mcp.json`, the `.coord-mcp-status` breadcrumb, and the fleet
/// commands / skills / agent definitions — the last three through the REAL
/// provisioners, so a new bundled path is covered without editing this test.
fn provision_like_a_session(dir: &Path) {
    std::fs::write(dir.join(".mcp.json"), "{\"mcpServers\":{}}\n").unwrap();
    std::fs::write(
        dir.join(crate::coord_mcp::COORD_MCP_STATUS_FILE),
        "degraded: test\n",
    )
    .unwrap();
    let claude = dir.join(".claude");
    let cmds = crate::fleet_commands::provision_fleet_commands_into(
        &claude.join("commands"),
        &crate::agent_commands::AgentCommandRegistry::new(),
    )
    .expect("commands");
    assert!(cmds.written > 0, "the fleet commands were written");
    let skills = crate::fleet_skills::provision_fleet_skills_into(
        &claude.join("skills"),
        &crate::agent_skills::AgentSkillRegistry::new(),
        None,
    )
    .expect("skills");
    assert!(skills.written > 0, "the fleet skills were written");
    let agents = crate::fleet_agents::provision_fleet_agents_into(
        &claude.join("agents"),
        &crate::provision_guard::KeepDecisions::default(),
        &mut Vec::new(),
        &mut Vec::new(),
    )
    .expect("agents");
    assert!(agents.written > 0, "the agent definitions were written");
}

/// A context for the shared-branch allocation `sb`, as `materialize_repos`
/// builds it: the canonical checkout as the "worktree", provenance from the
/// switch, and the `canonical_checkout` lease among its claims.
fn shared_branch_context(
    c: &Canon,
    sb: &SharedBranchRepo,
    agent_id: &str,
) -> crate::agent_worktree::isolated_edit::IsolatedEditContext {
    let mut ctx = crate::agent_worktree::isolated_edit::IsolatedEditContext::for_test_with_agent_id(
        agent_id.to_string(),
        vec![MaterializedWorktree {
            repo: sb.repo.clone(),
            branch: sb.branch.clone(),
            parent_sha: sb.parent_sha.clone(),
            worktree_path: sb.checkout_path.clone(),
            push_ref: sb.push_ref.clone(),
            parent_sha_provenance: sb.parent_sha_provenance.clone(),
        }],
        vec![ActiveClaim {
            kind: "canonical_checkout".to_string(),
            resource_key: format!("repo:{}", sb.repo),
            ttl_seconds: 300,
            agent_session_id: None,
        }],
    );
    ctx.set_provenance_for_test(
        &c.path,
        TreeProvenance::SharedBranch {
            branch: sb.branch.clone(),
            prior_ref: sb.prior_ref.clone(),
            switched_to_sha: sb.switched_to_sha.clone(),
        },
    );
    ctx
}

/// A refused spawn into a `shared_branch` placement, end to end through the
/// door's remover: provisioning has written everything a session writes into
/// the canonical checkout, and the checkout still ends back on `main`. If any
/// runner write showed in the restore's unfiltered listing this would read
/// `Kept(Dirty)` — the fix for that belongs at the write's source (the
/// excludes the switch applies), never in the restore.
#[tokio::test]
async fn a_refused_shared_branch_spawn_puts_the_canonical_back_despite_provisioning() {
    let _amb = super::super::test_coord::isolate();
    let c = Canon::new();
    let sb = c.switch();
    provision_like_a_session(&c.path);
    let ctx = shared_branch_context(&c, &sb, "sb-e2e-agent");

    let report = ctx
        .remove_unspawned_unreported_with("test door", false, "refused", |_| None)
        .await;

    assert_eq!(report.trees.len(), 1);
    assert_eq!(
        report.trees[0].1,
        TreeOutcome::Kept(KeepReason::SharedBranchCheckout(RestoreOutcome::Restored)),
        "{report:?}"
    );
    assert!(
        !report.removed_all(),
        "the canonical checkout is never removed"
    );
    assert!(c.path.join("README.md").exists());
    assert_eq!(c.head_ref(), "refs/heads/main");
    assert!(c.agent_branch_exists());
    // The runner's artifacts are still there (ignored, not deleted).
    assert!(c.path.join(".mcp.json").exists());
    assert!(c.path.join(".claude").join("commands").is_dir());
}

/// The restore runs while the allocation still holds its claims: the
/// `canonical_checkout` lease's release (sent by the context's `Drop`) must
/// find the checkout ALREADY back on `main`. A double coord records the
/// canonical's HEAD at the moment each release request arrives.
#[tokio::test]
async fn the_restore_runs_before_drop_releases_the_shared_branch_claim() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let _amb = super::super::test_coord::isolate();
    let c = Canon::new();
    let sb = c.switch();
    let mut ctx = shared_branch_context(&c, &sb, "sb-order-agent");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let canon = c.path.clone();
    tokio::spawn(async move {
        // Answer every request; record HEAD at the first `/claims/release`.
        // Any other Drop-time POST (an fs-observer push, say) is answered and
        // skipped, so it cannot be mistaken for the release.
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 16 * 1024];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let is_release = request.contains("/claims/release");
            // HEAD as it stands when the release ARRIVES.
            let head = if is_release {
                let canon = canon.clone();
                tokio::task::spawn_blocking(move || {
                    git(&canon, &["rev-parse", "--symbolic-full-name", "HEAD"])
                })
                .await
                .ok()
            } else {
                None
            };
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await;
            if let Some(head) = head {
                let _ = tx.send(head);
                return;
            }
        }
    });
    ctx.set_coord_http_base_for_test(base);

    let report = ctx
        .remove_unspawned_unreported_with("test door", false, "refused", |_| None)
        .await;
    assert_eq!(
        report.trees[0].1,
        TreeOutcome::Kept(KeepReason::SharedBranchCheckout(RestoreOutcome::Restored)),
        "{report:?}"
    );

    let head_at_release = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
        .await
        .expect("the claim release arrived")
        .expect("recorded");
    assert_eq!(
        head_at_release, "refs/heads/main",
        "the lease was released while the checkout was still parked on the agent branch"
    );
}

// ------------------------------------------- the allocation's exclude block

#[test]
fn the_switch_adds_the_block_and_a_restore_removes_exactly_it() {
    let c = Canon::new();
    seed_operator_line(&c);
    let sb = c.switch();
    assert!(
        has_block(&c),
        "the switch adds the block: {:?}",
        exclude(&c)
    );
    append_operator_line(&c);

    assert_eq!(restore(&c, &sb), RestoreOutcome::Restored);
    assert_block_removed_and_operator_visible(&c);
}

#[test]
fn a_switch_rollback_removes_the_block() {
    let first = Canon::new();
    let second = Canon::new();
    seed_operator_line(&first);
    // `operator-after` is seeded up front here: the rollback happens inside
    // the one call, so no line can be appended while the block stands.
    append_operator_line(&first);
    // The second checkout is dirty, so its switch is refused and the first
    // one is rolled back.
    std::fs::write(second.path.join("wip"), "uncommitted\n").unwrap();
    let row = |c: &Canon| CoordAllocatedWorktree {
        repo: "canon".to_string(),
        branch: AGENT_BRANCH.to_string(),
        parent_sha: c.parent_sha.clone(),
        worktree_path: String::new(),
        status: "allocated".to_string(),
        push_ref: String::new(),
        origin: Some("requested".to_string()),
        sibling_store_sha: None,
        parent_sha_provenance: ParentShaProvenance::default(),
    };
    let (r1, r2) = (row(&first), row(&second));

    assert!(checkout_shared_branches(&[(&r1, &first.path), (&r2, &second.path)]).is_err());
    assert_eq!(first.head_ref(), "refs/heads/main");
    assert_block_removed_and_operator_visible(&first);
}

#[tokio::test]
async fn a_shared_branch_context_dropping_normally_removes_the_block() {
    let _amb = super::super::test_coord::isolate();
    let c = Canon::new();
    seed_operator_line(&c);
    let sb = c.switch();
    append_operator_line(&c);
    let ctx = shared_branch_context(&c, &sb, "sb-drop-agent");
    assert!(has_block(&c));

    // The session ended: the context drops without any removal call.
    drop(ctx);

    // Drop removes the block on a spawned task (before the claim release).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while has_block(&c) && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_block_removed_and_operator_visible(&c);
    assert_eq!(
        c.head_ref(),
        format!("refs/heads/{AGENT_BRANCH}"),
        "a normal session end does not switch the checkout"
    );
}

#[test]
fn a_shared_branch_context_dropped_with_no_runtime_still_removes_the_block() {
    let c = Canon::new();
    seed_operator_line(&c);
    let sb = c.switch();
    append_operator_line(&c);
    let ctx = shared_branch_context(&c, &sb, "sb-drop-sync-agent");

    drop(ctx);

    assert_block_removed_and_operator_visible(&c);
}

#[tokio::test]
async fn release_repo_removes_the_block() {
    let _amb = super::super::test_coord::isolate();
    let c = Canon::new();
    seed_operator_line(&c);
    let sb = c.switch();
    append_operator_line(&c);
    let mut ctx = shared_branch_context(&c, &sb, "sb-release-agent");

    assert!(ctx.release_repo("canon").await);

    assert_block_removed_and_operator_visible(&c);
}

#[test]
fn a_versioned_claude_tree_gets_no_block_and_an_untracked_command_keeps_it_dirty() {
    let c = Canon::new();
    std::fs::create_dir_all(c.path.join(".claude").join("commands")).unwrap();
    std::fs::write(c.path.join(".claude/commands/a.md"), "repo-authored\n").unwrap();
    git(&c.path, &["add", ".claude/commands/a.md"]);
    git(&c.path, &["commit", "-q", "-m", "authored command"]);
    let c = Canon {
        parent_sha: git(&c.path, &["rev-parse", "HEAD"]),
        ..c
    };
    let sb = c.switch();
    assert!(
        !has_block(&c),
        "versioned .claude/ gets no block: {:?}",
        exclude(&c)
    );

    std::fs::write(c.path.join(".claude/commands/b.md"), "untracked\n").unwrap();
    match restore(&c, &sb) {
        RestoreOutcome::Kept(RestoreKeep::Dirty(p)) => {
            assert!(p.contains(".claude/commands/b.md"), "{p:?}")
        }
        other => panic!("expected Kept(Dirty), got {other:?}"),
    }
    assert_eq!(c.head_ref(), format!("refs/heads/{AGENT_BRANCH}"));
}

#[test]
fn an_unanswered_claude_read_adds_no_block() {
    let c = Canon::new();
    // `ls-tree` of a ref that does not resolve fails: UNKNOWN adds nothing.
    prepare_canonical_excludes(&c.path, "refs/heads/no-such-ref", AGENT_BRANCH);
    assert!(!has_block(&c), "{:?}", exclude(&c));
    // The roster half is unconditional and still applied.
    assert!(exclude(&c).lines().any(|l| l.trim() == ".mcp.json"));
}

// ------------------------------------------------ block bytes and guards

fn exclude_path(c: &Canon) -> PathBuf {
    c.path.join(".git").join("info").join("exclude")
}

fn write_exclude(c: &Canon, bytes: &[u8]) {
    let path = exclude_path(c);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn read_exclude_bytes(c: &Canon) -> Vec<u8> {
    std::fs::read(exclude_path(c)).unwrap()
}

/// The block `add_allocation_excludes` writes for `branch`, with `eol`.
fn block_text(branch: &str, eol: &str) -> String {
    let mut s = format!("{BLOCK_BEGIN}{branch}{eol}");
    for pat in PROVISIONED_CLAUDE_EXCLUDES {
        s.push_str(pat);
        s.push_str(eol);
    }
    s.push_str(&format!("{BLOCK_END}{branch}{eol}"));
    s
}

#[test]
fn removal_keeps_crlf_line_endings_outside_the_block() {
    let c = Canon::new();
    let before = format!("a\r\n{}b\r\n", block_text(AGENT_BRANCH, "\r\n"));
    write_exclude(&c, before.as_bytes());

    remove_allocation_excludes(&c.path, AGENT_BRANCH);

    assert_eq!(read_exclude_bytes(&c), b"a\r\nb\r\n".to_vec());
}

#[test]
fn removal_keeps_a_missing_final_newline() {
    let c = Canon::new();
    let before = format!("a\n{}b", block_text(AGENT_BRANCH, "\n"));
    write_exclude(&c, before.as_bytes());

    remove_allocation_excludes(&c.path, AGENT_BRANCH);

    assert_eq!(read_exclude_bytes(&c), b"a\nb".to_vec());
}

#[test]
fn removal_spares_another_branchs_block_even_one_this_branch_prefixes() {
    let c = Canon::new();
    // `agent/sb-test` is a prefix of `agent/sb-test-2`; that block comes first.
    let other = format!("{AGENT_BRANCH}-2");
    let before = format!(
        "a\n{}{}b\n",
        block_text(&other, "\n"),
        block_text(AGENT_BRANCH, "\n")
    );
    write_exclude(&c, before.as_bytes());

    remove_allocation_excludes(&c.path, AGENT_BRANCH);

    assert_eq!(
        String::from_utf8(read_exclude_bytes(&c)).unwrap(),
        format!("a\n{}b\n", block_text(&other, "\n"))
    );
}

#[test]
fn a_begin_line_with_no_end_is_left_alone() {
    let c = Canon::new();
    let before = format!("a\n{BLOCK_BEGIN}{AGENT_BRANCH}\n.claude/commands/\nb\n");
    write_exclude(&c, before.as_bytes());

    remove_allocation_excludes(&c.path, AGENT_BRANCH);

    assert_eq!(read_exclude_bytes(&c), before.into_bytes());
}

/// Pins behaviour this module had from its first revision (its reads always
/// refused non-UTF-8); the publisher's roster heal, which did NOT, is pinned
/// by `fleet`'s `ensure_repo_info_exclude_leaves_a_non_utf8_file_byte_identical`.
#[test]
fn a_non_utf8_exclude_file_is_never_rewritten() {
    let c = Canon::new();
    let mut before = b"op\xff\n".to_vec();
    before.extend_from_slice(block_text(AGENT_BRANCH, "\n").as_bytes());
    write_exclude(&c, &before);

    remove_allocation_excludes(&c.path, AGENT_BRANCH);
    assert_eq!(read_exclude_bytes(&c), before, "remove left it as is");

    assert!(add_allocation_excludes(&c.path, "agent/other").is_err());
    assert_eq!(read_exclude_bytes(&c), before, "add left it as is");
}

#[test]
fn a_context_that_switched_nothing_leaves_a_live_allocations_block_alone() {
    let c = Canon::new();
    git(&c.path, &["checkout", "-q", "-b", AGENT_BRANCH]);
    // A LIVE allocation on the same branch owns this block.
    add_allocation_excludes(&c.path, AGENT_BRANCH).expect("add");
    let sb = c.switch();
    assert_eq!(sb.prior_ref, sb.branch, "this allocation switched nothing");
    let ctx = shared_branch_context(&c, &sb, "sb-noswitch-agent");

    drop(ctx);

    assert!(
        has_block(&c),
        "the live allocation's block survives: {:?}",
        exclude(&c)
    );
    forget_live_for_test(&c.path, AGENT_BRANCH);
}

// ------------------------------------------------------- the stale sweep

/// A switched checkout whose runner went away without removing its block:
/// the checkout was put back by hand and the process forgot the allocation.
fn abandoned_block() -> Canon {
    let c = Canon::new();
    seed_operator_line(&c);
    c.switch();
    append_operator_line(&c);
    forget_live_for_test(&c.path, AGENT_BRANCH);
    assert!(has_block(&c));
    c
}

fn kept(out: &[SweepOutcome]) -> bool {
    matches!(out, [SweepOutcome::Kept { .. }])
}

#[test]
fn the_sweep_removes_a_stale_block() {
    let c = abandoned_block();
    git(&c.path, &["checkout", "-q", "main"]);

    let out = sweep_stale_blocks(&c.path, |_| HolderState::Free);

    assert_eq!(
        out,
        vec![SweepOutcome::Removed {
            branch: AGENT_BRANCH.into()
        }]
    );
    assert_block_removed_and_operator_visible(&c);
}

#[test]
fn the_sweep_keeps_a_block_on_the_current_head_with_a_live_holder() {
    let c = abandoned_block();
    let out = sweep_stale_blocks(&c.path, |_| HolderState::Held);
    assert!(kept(&out), "{out:?}");
    assert!(has_block(&c));
}

#[test]
fn the_sweep_keeps_a_block_on_the_current_head_even_with_no_holder() {
    let c = abandoned_block();
    let out = sweep_stale_blocks(&c.path, |_| HolderState::Free);
    assert!(kept(&out), "{out:?}");
    assert!(has_block(&c));
}

#[test]
fn the_sweep_keeps_a_block_whose_checkout_has_a_live_holder() {
    let c = abandoned_block();
    git(&c.path, &["checkout", "-q", "main"]);
    let out = sweep_stale_blocks(&c.path, |_| HolderState::Held);
    assert!(kept(&out), "{out:?}");
    assert!(has_block(&c));
}

#[test]
fn the_sweep_keeps_a_block_when_the_holder_is_unknown() {
    let c = abandoned_block();
    git(&c.path, &["checkout", "-q", "main"]);
    let out = sweep_stale_blocks(&c.path, |_| HolderState::Unknown("no answer".into()));
    assert!(kept(&out), "{out:?}");
    assert!(has_block(&c));
}

#[test]
fn the_sweep_removes_a_block_whose_branch_is_gone() {
    let c = abandoned_block();
    git(&c.path, &["checkout", "-q", "main"]);
    git(&c.path, &["branch", "-q", "-D", AGENT_BRANCH]);
    let out = sweep_stale_blocks(&c.path, |_| HolderState::Unknown("no answer".into()));
    assert_eq!(
        out,
        vec![SweepOutcome::Removed {
            branch: AGENT_BRANCH.into()
        }]
    );
    assert_block_removed_and_operator_visible(&c);
}

/// The wired sweep: this process's own live allocation is never swept even
/// when coord reports no holder; once the process has forgotten it (an exit or
/// crash), coord's `null` holder lets the sweep reap the block.
#[tokio::test]
async fn the_wired_sweep_spares_this_process_live_allocation_and_reaps_a_forgotten_one() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _amb = super::super::test_coord::isolate();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        // coord's `by-resource` answer for an unheld checkout: `null`.
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 16 * 1024];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnull",
                )
                .await;
        }
    });
    let c = Canon::new();
    seed_operator_line(&c);
    c.switch();
    append_operator_line(&c);
    git(&c.path, &["checkout", "-q", "main"]);

    sweep_checkout(c.path.clone(), Some(base.clone())).await;
    assert!(
        has_block(&c),
        "this process's live allocation is never swept"
    );

    forget_live_for_test(&c.path, AGENT_BRANCH);
    sweep_checkout(c.path.clone(), Some(base)).await;
    assert_block_removed_and_operator_visible(&c);
}

/// A branch probe that FAILS (git exit 128 — here, not a repository at all;
/// equally a `safe.directory` refusal) is not "the branch is gone": the sweep
/// keeps the block even though the holder lookup says nothing holds it.
#[test]
fn the_sweep_keeps_a_block_when_the_branch_probe_fails_fatally() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let not_a_repo = tmp.path().join("not-a-repo");
    let info = not_a_repo.join(".git").join("info");
    std::fs::create_dir_all(&info).unwrap();
    let before = format!("op\n{}", block_text(AGENT_BRANCH, "\n"));
    std::fs::write(info.join("exclude"), &before).unwrap();

    // Precondition: git walks PAST an invalid `.git` directory into the
    // parents, so this fixture is fatal (exit 128) only while no work tree sits
    // above the temp dir. A `TMPDIR` inside a repo would answer exit 1 from that
    // parent instead; name that environment fault here rather than as a sweep bug.
    let mut probe = crate::process_helpers::no_window("git");
    probe.arg("-C").arg(&not_a_repo).args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/{AGENT_BRANCH}"),
    ]);
    let probed =
        crate::process_helpers::output_with_timeout(probe, std::time::Duration::from_secs(60))
            .expect("git runs");
    assert_eq!(
        probed.status.code(),
        Some(128),
        "fixture precondition: the temp dir {} must not sit inside a git work tree \
         (the branch probe must fail fatally, not answer from a parent repo): {probed:?}",
        tmp.path().display()
    );

    let out = sweep_stale_blocks(&not_a_repo, |_| HolderState::Free);

    assert!(kept(&out), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(info.join("exclude")).unwrap(),
        before
    );
}

/// The startup sweep visits every primary checkout under the workspace root,
/// not only `qontinui-*` ones: a `shared_branch` allocation can switch a
/// sibling such as `ui-bridge`, and a block a crash left there has no other
/// reaper. The fixture's checkout is named `canon`, with no `qontinui-`
/// prefix; its branch is gone, so the sweep reaps it with no coord at all.
#[tokio::test]
async fn the_startup_sweep_reaps_a_stale_block_in_a_checkout_of_any_name() {
    let c = abandoned_block();
    git(&c.path, &["checkout", "-q", "main"]);
    git(&c.path, &["branch", "-q", "-D", AGENT_BRANCH]);
    let root = c
        .path
        .parent()
        .expect("the checkout has a parent")
        .to_path_buf();
    assert!(
        !c.path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("qontinui-")),
        "fixture precondition: the checkout's name must not carry the qontinui- prefix"
    );

    sweep_workspace_at(&root, None, |_| true).await;

    assert_block_removed_and_operator_visible(&c);
}

/// The publisher's per-cycle sweep reaches a checkout it does not publish
/// (no `qontinui-` prefix), so a block startup kept is judged again. coord is
/// unreachable here (holder UNKNOWN), and the block still goes because its
/// branch is gone — the arm that needs no holder answer.
#[tokio::test]
async fn the_per_cycle_sweep_reaches_a_checkout_the_publisher_does_not_walk() {
    let c = abandoned_block();
    git(&c.path, &["checkout", "-q", "main"]);
    git(&c.path, &["branch", "-q", "-D", AGENT_BRANCH]);
    let root = c
        .path
        .parent()
        .expect("the checkout has a parent")
        .to_path_buf();

    sweep_unpublished_checkouts(&root, "http://127.0.0.1:9".into()).await;

    assert_block_removed_and_operator_visible(&c);
}

/// …and it leaves the `qontinui-*` checkouts to the publisher's own walk.
#[tokio::test]
async fn the_per_cycle_sweep_skips_a_qontinui_checkout() {
    let c = abandoned_block();
    git(&c.path, &["checkout", "-q", "main"]);
    git(&c.path, &["branch", "-q", "-D", AGENT_BRANCH]);
    let root = c
        .path
        .parent()
        .expect("the checkout has a parent")
        .to_path_buf();
    let renamed = root.join("qontinui-canon");
    std::fs::rename(&c.path, &renamed).expect("rename");
    let exclude = renamed.join(".git").join("info").join("exclude");
    let before = std::fs::read(&exclude).expect("exclude");

    sweep_unpublished_checkouts(&root, "http://127.0.0.1:9".into()).await;

    assert_eq!(std::fs::read(&exclude).expect("exclude"), before);
}
