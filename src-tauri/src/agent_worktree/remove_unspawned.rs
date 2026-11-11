//! Remove the worktree a refused spawn just allocated — when, and only when,
//! the tree proves it holds nothing.
//!
//! Plan `2026-11-08-a-spawn-refused-after-its-worktree-allocation-leaves-the-tree-on-disk`
//! (D1–D3). A terminal spawn can be refused after `acquire_for_terminal` ran
//! its `git worktree add`. The context's `Drop` releases the claims, and the
//! spawn-failed report moves coord's ledger rows to `abandoned` — a ledger
//! fact that grants no disk authority (`worktree_reclaim`'s
//! `AGENT_DONE_IS_REMOVABLE`). The only party that knows no child ever ran in
//! the tree is the runner process that refused the spawn, so the removal
//! happens here, in the call that allocated.
//!
//! A tree is removed only when EVERY one of these reads true at removal time,
//! and any failed or unreadable check keeps it and says why
//! ([`KeepReason`]):
//!
//! 1. **no child started** — the failure was typed `child_started == false`
//!    ([`crate::terminal::spawn_refusal::SpawnFailure`]);
//! 2. **a fresh worktree this allocation created** — it came from
//!    `MaterializeOutcome::Worktrees` ([`TreeProvenance::FreshWorktree`]; a
//!    `shared_branch` placement is the operator's canonical checkout and never
//!    passes) AND it is the allocation root `<root>/<agent_id>/<repo>` that
//!    [`super::canonical_paths::allocated_worktree_for_path`] names, under this
//!    allocation's `agent_id`;
//! 3. **a linked worktree** — [`super::census::probe_clone_rootness`] reads
//!    `NotCloneRoot`, the tri-state, symlink-safe test reclaim's INV-W5 uses;
//! 4. **no commit was made** — `git rev-parse HEAD` equals the worktree's
//!    recorded `parent_sha`;
//! 5. **clean modulo runner scaffolding** —
//!    [`super::dirty::probe_reclaim_dirty`] permits removal. The runner's own
//!    provisioned scaffolding (`.mcp.json`, `.claude/commands`, `.claude/skills`)
//!    is then deleted — exactly the `??` entries porcelain lists that
//!    [`super::dirty::is_scaffolding_path`] accepts — and porcelain must then
//!    read EMPTY.
//!
//! The removal itself is `git worktree remove <path>` WITHOUT `--force`, so
//! git's own refusal of a dirty tree stays a second, independent guard (D3).
//! It runs under [`super::checkout_materialize_lock`] for the canonical
//! checkout, the lock every `git worktree add` against it takes, so a
//! concurrent add is never raced. Once the allocation's LAST tree is removed,
//! the two runner-written agent-root files (`.cargo/config.toml`,
//! `.cargo/edit-siblings`) are deleted and the `<agent_id>/` directory is
//! pruned when empty — never recursively.
//!
//! Naming: `remove_unspawned*`, never "retire" — `retire_abandoned` already
//! means a LEDGER-only move, and one verb must not mean two things.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use super::census::CloneRootness;
use super::dirty::DirtyVerdict;

/// Where one tree of an allocation came from — the provenance conjunct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TreeProvenance {
    /// A `git worktree add` this allocation ran (`MaterializeOutcome::Worktrees`),
    /// cut from `canonical` — the checkout whose materialize lock guards it.
    FreshWorktree { canonical: PathBuf },
    /// coord's `shared_branch` placement: the path IS the canonical checkout,
    /// switched onto an agent branch. Never removed.
    SharedBranch,
    /// Nothing recorded how the tree was placed (a test context, or a fresh
    /// worktree whose canonical checkout no longer resolves). Never removed.
    Unrecorded,
}

/// One tree of an allocation, as the removal judges it.
#[derive(Debug, Clone)]
pub(crate) struct UnspawnedBase {
    pub(crate) worktree_path: PathBuf,
    pub(crate) parent_sha: String,
    pub(crate) provenance: TreeProvenance,
}

/// Why a tree was kept. Every arm is a refusal to delete; the log line carries
/// it, and the tests assert WHICH guard fired, so deleting a conjunct is
/// observable even where a later guard (git's own refusal) would still keep
/// the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeepReason {
    /// The failure came after the child spawn; a live process may hold the tree.
    ChildStarted,
    /// Not a fresh worktree of this allocation (shared-branch, unrecorded, or
    /// not the allocation root under this agent id).
    NotFresh(String),
    /// `probe_clone_rootness` did not read `NotCloneRoot`.
    NotLinkedWorktree(CloneRootness),
    /// `git rev-parse HEAD` did not answer.
    HeadUnreadable(String),
    /// HEAD moved past the recorded `parent_sha`: a commit was made.
    HeadMoved { head: String },
    /// The reclaim-scoped dirty probe did not read `Clean` (`Dirty` or `Unknown`).
    Dirty(DirtyVerdict),
    /// Deleting the runner's scaffolding failed, or the tree did not read
    /// porcelain-empty afterwards.
    Scaffolding(String),
    /// `git worktree remove` (no `--force`) refused or did not answer.
    RemoveFailed(String),
    /// An IGNORED path that is not runner scaffolding: `git worktree remove`
    /// would delete it silently, so the tree is kept.
    IgnoredContent(String),
    /// A credential-helper install for this allocation was still running after
    /// the bounded wait and an abort; judging the tree under it is not safe.
    CredentialInstallUnsettled,
}

/// The answer for one tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TreeOutcome {
    Removed,
    Kept(KeepReason),
}

/// The answer for one allocation.
#[derive(Debug, Clone)]
pub(crate) struct RemovalReport {
    pub(crate) trees: Vec<(PathBuf, TreeOutcome)>,
    /// The `<agent_id>/` directory was pruned (every tree removed, nothing else
    /// left in it).
    pub(crate) agent_root_pruned: bool,
}

impl RemovalReport {
    pub(crate) fn removed_all(&self) -> bool {
        !self.trees.is_empty()
            && self
                .trees
                .iter()
                .all(|(_, o)| matches!(o, TreeOutcome::Removed))
    }

    pub(crate) fn removed_paths(&self) -> Vec<PathBuf> {
        self.trees
            .iter()
            .filter(|(_, o)| matches!(o, TreeOutcome::Removed))
            .map(|(p, _)| p.clone())
            .collect()
    }
}

/// Budget for each git child the removal runs (HEAD read, porcelain listing,
/// `worktree remove`) — the reclaim probe's own budget, so an unanswered child
/// is UNKNOWN and keeps the tree.
const GIT_TIMEOUT: std::time::Duration = super::dirty::RECLAIM_DIRTY_PROBE_TIMEOUT;

/// Remove every tree of allocation `agent_id` that proves it holds nothing,
/// after a spawn refused with `reason`. See the module docs for the conjuncts.
///
/// Free over the allocation's bases so a door holding no
/// `IsolatedEditContext` (the coord-dispatched agent launch) can call it over
/// exactly the rows it added. Never fails: every tree gets an outcome.
pub(crate) async fn remove_unspawned_trees(
    agent_id: &str,
    bases: Vec<UnspawnedBase>,
    child_started: bool,
    reason: &str,
) -> RemovalReport {
    remove_unspawned_trees_with(
        agent_id,
        bases,
        child_started,
        reason,
        super::canonical_paths::allocated_worktree_for_path,
    )
    .await
}

/// [`remove_unspawned_trees`] with the allocation-root resolver injected — the
/// seam the tests use to point it at a temp worktree root instead of the
/// operator's.
pub(crate) async fn remove_unspawned_trees_with<F>(
    agent_id: &str,
    bases: Vec<UnspawnedBase>,
    child_started: bool,
    reason: &str,
    allocated_root: F,
) -> RemovalReport
where
    F: Fn(&Path) -> Option<PathBuf> + Clone + Send + 'static,
{
    remove_unspawned_trees_bounded(
        agent_id,
        bases,
        child_started,
        reason,
        allocated_root,
        INSTALL_SETTLE_BOUND,
    )
    .await
}

/// How long the removal waits for an in-flight credential-helper install
/// (each wait, and again after an abort) — its coord fetches are network I/O.
const INSTALL_SETTLE_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// [`remove_unspawned_trees_with`] with the install-settle bound injected.
pub(crate) async fn remove_unspawned_trees_bounded<F>(
    agent_id: &str,
    bases: Vec<UnspawnedBase>,
    child_started: bool,
    reason: &str,
    allocated_root: F,
    settle_bound: std::time::Duration,
) -> RemovalReport
where
    F: Fn(&Path) -> Option<PathBuf> + Clone + Send + 'static,
{
    // A credential-helper install `materialize` detached for this allocation
    // may still be running: let it finish (bounded) BEFORE the trees are
    // judged, or a fast refusal removes the tree and runs the context's
    // credential cleanup ahead of the install, which then registers a token
    // file and a refresh loop nothing will ever reap. One that outlives the
    // bound is aborted; if even the abort does not land, every tree is KEPT.
    let settled = crate::credential_helper::settle_installs(agent_id, settle_bound).await;
    let mut trees = Vec::with_capacity(bases.len());
    for base in bases {
        let path = base.worktree_path.clone();
        let outcome = if settled {
            judge_one(agent_id, base, child_started, allocated_root.clone()).await
        } else {
            TreeOutcome::Kept(KeepReason::CredentialInstallUnsettled)
        };
        match &outcome {
            TreeOutcome::Removed => {
                info!(
                    agent_id = %agent_id,
                    worktree = %path.display(),
                    "remove_unspawned: removed the fresh, empty worktree of a spawn refused \
                     before its child started ({reason})"
                );
            }
            TreeOutcome::Kept(why) => warn!(
                agent_id = %agent_id,
                worktree = %path.display(),
                keep_reason = ?why,
                "remove_unspawned: KEPT the worktree of a refused spawn ({reason}) — {}",
                describe(why)
            ),
        }
        trees.push((path, outcome));
    }

    let mut report = RemovalReport {
        trees,
        agent_root_pruned: false,
    };
    if report.removed_all() {
        let last = report.trees[0].0.clone();
        report.agent_root_pruned = tokio::task::spawn_blocking(move || prune_agent_root(&last))
            .await
            .unwrap_or(false);
    }
    report
}

/// One sentence for the log: what a kept tree's reason means.
fn describe(why: &KeepReason) -> String {
    match why {
        KeepReason::ChildStarted => {
            "the failure came after the child spawn, so a live process may hold the tree".into()
        }
        KeepReason::NotFresh(d) => format!("not a fresh worktree of this allocation: {d}"),
        KeepReason::NotLinkedWorktree(r) => format!("not a linked worktree (clone-rootness {r:?})"),
        KeepReason::HeadUnreadable(e) => format!("HEAD could not be read: {e}"),
        KeepReason::HeadMoved { head } => {
            format!("HEAD is {head}, past the allocation's parent sha — a commit was made")
        }
        KeepReason::Dirty(v) => format!("the dirty probe read {v:?}, not Clean"),
        KeepReason::Scaffolding(d) => format!("runner scaffolding cleanup: {d}"),
        KeepReason::RemoveFailed(e) => format!("`git worktree remove` (no --force) refused: {e}"),
        KeepReason::IgnoredContent(e) => {
            format!("an ignored path that is not runner scaffolding would be lost: {e}")
        }
        KeepReason::CredentialInstallUnsettled => {
            "a credential-helper install for this allocation was still running after the bounded \
             wait and an abort"
                .into()
        }
    }
}

async fn judge_one<F>(
    agent_id: &str,
    base: UnspawnedBase,
    child_started: bool,
    allocated_root: F,
) -> TreeOutcome
where
    F: Fn(&Path) -> Option<PathBuf> + Send + 'static,
{
    if child_started {
        return TreeOutcome::Kept(KeepReason::ChildStarted);
    }
    let canonical = match &base.provenance {
        TreeProvenance::FreshWorktree { canonical } => canonical.clone(),
        TreeProvenance::SharedBranch => {
            return TreeOutcome::Kept(KeepReason::NotFresh(
                "a shared_branch placement — the path is the canonical checkout".into(),
            ))
        }
        TreeProvenance::Unrecorded => {
            return TreeOutcome::Kept(KeepReason::NotFresh(
                "no materialize outcome was recorded for this tree".into(),
            ))
        }
    };
    // Held across every check AND the removal, so "at removal time" holds
    // against a concurrent `git worktree add` from the same checkout.
    let lock = super::checkout_materialize_lock(&canonical);
    let _serialized = lock.lock().await;
    let agent_id = agent_id.to_string();
    tokio::task::spawn_blocking(move || {
        let outcome = judge_and_remove(&agent_id, &base, &canonical, &allocated_root);
        if outcome == TreeOutcome::Removed {
            // The coord-mcp proxy nonce provisioned into the tree's `.mcp.json`
            // is keyed by the workdir string the provisioning chokepoint minted
            // under — the worktree path's lossy string. Its only other reaper
            // fires on SESSION close, which a spawn that never started never
            // reaches. Evicted here, on the blocking pool, because eviction
            // rewrites a file synchronously.
            crate::coord_mcp::evict_proxy_nonces_for_workdir(&base.worktree_path.to_string_lossy());
        }
        outcome
    })
    .await
    .unwrap_or_else(|e| TreeOutcome::Kept(KeepReason::RemoveFailed(format!("task failed: {e}"))))
}

/// The blocking body: every conjunct after provenance, then the removal.
fn judge_and_remove(
    agent_id: &str,
    base: &UnspawnedBase,
    canonical: &Path,
    allocated_root: &dyn Fn(&Path) -> Option<PathBuf>,
) -> TreeOutcome {
    let path = base.worktree_path.as_path();

    // (2, second half) the allocation root under THIS agent id.
    let is_root =
        allocated_root(path).is_some_and(|root| super::canonical_paths::paths_equal(&root, path));
    let under_agent = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(agent_id));
    if !is_root || !under_agent {
        return TreeOutcome::Kept(KeepReason::NotFresh(format!(
            "not the live allocation root under agent {agent_id} \
             (allocation root: {is_root}, under the agent id: {under_agent})"
        )));
    }

    // (3) a linked worktree.
    let rootness = super::census::probe_clone_rootness(path);
    if rootness != CloneRootness::NotCloneRoot {
        return TreeOutcome::Kept(KeepReason::NotLinkedWorktree(rootness));
    }

    // (4) no commit past parent_sha.
    let head = match git_stdout(path, &["rev-parse", "HEAD"], "remove_unspawned: rev-parse") {
        Ok(h) => h.trim().to_string(),
        Err(e) => return TreeOutcome::Kept(KeepReason::HeadUnreadable(e)),
    };
    let parent = base.parent_sha.trim();
    if parent.is_empty() || !head.eq_ignore_ascii_case(parent) {
        return TreeOutcome::Kept(KeepReason::HeadMoved { head });
    }

    // (5) clean modulo scaffolding.
    let verdict = super::dirty::probe_reclaim_dirty(
        path,
        super::dirty::RECLAIM_DIRTY_PROBE_TIMEOUT,
        "remove_unspawned",
    );
    if !verdict.permits_removal() {
        return TreeOutcome::Kept(KeepReason::Dirty(verdict));
    }
    // The branch `git worktree add -b` cut for this tree, read while the tree
    // still exists; deleted (safely) after a successful remove.
    let branch = git_stdout(
        path,
        &["symbolic-ref", "--short", "-q", "HEAD"],
        "remove_unspawned: branch",
    )
    .ok()
    .map(|b| b.trim().to_string())
    .filter(|b| !b.is_empty());

    if let Err(e) = delete_scaffolding(path) {
        return TreeOutcome::Kept(e);
    }
    // From here the runner's scaffolding is GONE from the kept tree: every
    // keep reason below says so, so nobody reads a kept tree's missing
    // `.mcp.json` / `.claude/` as a provisioning fault.
    const DELETED: &str = "runner scaffolding already deleted";
    match git_stdout(path, PORCELAIN_ALL, "remove_unspawned: re-probe") {
        Ok(p) if p.trim().is_empty() => {}
        Ok(p) => {
            return TreeOutcome::Kept(KeepReason::Scaffolding(format!(
                "{DELETED}; not porcelain-empty afterwards: {}",
                p.trim()
            )))
        }
        Err(e) => {
            return TreeOutcome::Kept(KeepReason::Scaffolding(format!(
                "{DELETED}; the re-probe did not answer: {e}"
            )))
        }
    }

    // The removal — never `--force`.
    let Some(path_str) = path.to_str() else {
        return TreeOutcome::Kept(KeepReason::RemoveFailed(format!(
            "{DELETED}; path is not UTF-8"
        )));
    };
    if let Err(e) = git_stdout(
        canonical,
        &["worktree", "remove", path_str],
        "remove_unspawned: worktree remove",
    ) {
        return TreeOutcome::Kept(KeepReason::RemoveFailed(format!("{DELETED}; {e}")));
    }
    if let Some(branch) = branch {
        delete_agent_branch(canonical, &branch);
    }
    TreeOutcome::Removed
}

/// `git status` porcelain with EVERY untracked file listed, whatever the
/// repo's `status.showUntrackedFiles` says — `no` would otherwise hide an
/// operator's untracked file from the "must read empty" check — and every
/// IGNORED path too (`!!`), because `git worktree remove` deletes ignored
/// content without asking. Only the scaffolding's own ignored entries (a
/// `.mcp.json` the runner excludes, say) are deleted; any other is a keep.
const PORCELAIN_ALL: &[&str] = &[
    "status",
    "--porcelain",
    "--untracked-files=all",
    "--ignored",
];

/// Safe-delete the agent branch a removed tree was on: `git branch -d`, never
/// `-D`, so a branch with commits not merged anywhere survives; only an
/// `agent/…` branch is touched. Runs under the caller's materialize lock. A
/// refusal is logged, never an error — the tree is already gone.
fn delete_agent_branch(canonical: &Path, branch: &str) {
    if !branch.starts_with("agent/") {
        info!(branch = %branch, "remove_unspawned: not an agent/ branch — left in place");
        return;
    }
    match git_stdout(
        canonical,
        &["branch", "-d", branch],
        "remove_unspawned: branch -d",
    ) {
        Ok(_) => info!(branch = %branch, "remove_unspawned: deleted the removed tree's branch"),
        Err(e) => warn!(
            branch = %branch,
            "remove_unspawned: `git branch -d` refused the removed tree's branch — left in \
             place: {e}"
        ),
    }
}

/// Run `git -C <dir> <args>` under the budget; `Ok(stdout)` only on exit 0.
fn git_stdout(dir: &Path, args: &[&str], label: &str) -> Result<String, String> {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.arg("-C").arg(dir).args(args);
    match crate::process_helpers::run_probe(cmd, GIT_TIMEOUT, label) {
        crate::process_helpers::ProbeOutcome::Captured(out) => {
            Ok(String::from_utf8_lossy(&out).into_owned())
        }
        crate::process_helpers::ProbeOutcome::Degraded(why) => {
            Err(format!("git {} did not answer: {why:?}", args.join(" ")))
        }
    }
}

/// Delete exactly the untracked (`??`) and ignored (`!!`) porcelain entries
/// that are runner scaffolding. Any other entry keeps the tree: an ignored
/// one as [`KeepReason::IgnoredContent`], anything else as
/// [`KeepReason::Scaffolding`] — the dirty probe read the tree clean a moment
/// ago, so one now means something wrote into it.
fn delete_scaffolding(tree: &Path) -> Result<(), KeepReason> {
    let porcelain = git_stdout(tree, PORCELAIN_ALL, "remove_unspawned: scaffolding listing")
        .map_err(KeepReason::Scaffolding)?;
    let mut targets = Vec::new();
    for line in porcelain.lines() {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        let (ignored, rest) = match (line.strip_prefix("?? "), line.strip_prefix("!! ")) {
            (Some(rest), _) => (false, rest),
            (None, Some(rest)) => (true, rest),
            (None, None) => {
                return Err(KeepReason::Scaffolding(format!(
                    "unexpected porcelain entry {line:?}"
                )))
            }
        };
        let entry = super::dirty::unquote(rest.trim());
        if !super::dirty::is_scaffolding_path(entry) {
            return Err(if ignored {
                KeepReason::IgnoredContent(entry.to_string())
            } else {
                KeepReason::Scaffolding(format!("unexpected porcelain entry {line:?}"))
            });
        }
        let rel = Path::new(entry.trim_end_matches(['/', '\\']));
        // `is_scaffolding_path` is a prefix test: `.claude/../x` would pass it.
        // Only plain relative components may name what is deleted.
        if !rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return Err(KeepReason::Scaffolding(format!(
                "refusing a non-plain scaffolding path {entry:?}"
            )));
        }
        targets.push(tree.join(rel));
    }
    for target in targets {
        // `symlink_metadata`: a symlinked `.claude` is unlinked, never followed.
        let removed = match std::fs::symlink_metadata(&target) {
            Ok(md) if md.file_type().is_dir() => std::fs::remove_dir_all(&target),
            Ok(_) => std::fs::remove_file(&target),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
        removed.map_err(|e| {
            KeepReason::Scaffolding(format!("could not delete {}: {e}", target.display()))
        })?;
    }
    Ok(())
}

/// After the allocation's last tree is gone: delete the two runner-written
/// agent-root files, the `.cargo/` dir when that empties it, and the
/// `<agent_id>/` dir when THAT is empty — all non-recursive, so anything else
/// left there keeps the directory. Returns whether `<agent_id>/` is gone.
fn prune_agent_root(any_tree: &Path) -> bool {
    let Some(agent_root) = any_tree.parent() else {
        return false;
    };
    for file in [
        super::edit_sibling::config_path(agent_root),
        super::edit_sibling::marker_path(agent_root),
    ] {
        match std::fs::remove_file(&file) {
            Ok(()) => info!("remove_unspawned: deleted {}", file.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!("remove_unspawned: could not delete {}: {e}", file.display()),
        }
    }
    // Non-recursive: fails harmlessly when anything else is in `.cargo/`.
    let _ = std::fs::remove_dir(agent_root.join(".cargo"));
    super::reclaim::prune_empty_session_parent(any_tree);
    let gone = !agent_root.exists();
    if !gone {
        info!(
            agent_root = %agent_root.display(),
            "remove_unspawned: the allocation's agent dir holds something else — kept"
        );
    }
    gone
}

#[cfg(test)]
#[path = "remove_unspawned_tests.rs"]
mod tests;
