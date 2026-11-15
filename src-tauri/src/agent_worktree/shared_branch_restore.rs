//! Put a canonical checkout back on the ref it was on, after a spawn that was
//! given a `shared_branch` placement was refused before its child started.
//!
//! Plan `2026-11-13-spawn-refused-tree-followups` D2 / Phase 4 (edge
//! `82118348`). A `shared_branch` placement does not cut a worktree: it
//! switches the operator's canonical checkout onto the agent's branch
//! ([`super::checkout_shared_branches`]). When the spawn is then refused, the
//! tree is never removed (it IS the canonical checkout), but nothing used to
//! switch it back either, so the checkout stayed parked on an agent branch no
//! session would ever work on.
//!
//! The restore is a branch switch on fleet-owned state, so it acts only on
//! ESTABLISHED facts, and every unestablished one keeps the checkout as it is
//! with a typed reason ([`RestoreKeep`]):
//!
//! 0. **a switch happened** — `prior_ref != branch`. When the checkout was
//!    already on the agent branch, the allocation switched nothing and may
//!    share the checkout with a live session; that is
//!    [`RestoreKeep::NothingSwitched`], checked before anything is read.
//! 1. **still on the agent branch** — `HEAD` is the symbolic ref
//!    `refs/heads/<branch>`;
//! 2. **no commit was made** — `rev-parse HEAD` equals the sha recorded right
//!    after the switch;
//! 3. **fully clean** — `status --porcelain --untracked-files=all
//!    --ignored=no` prints nothing. NOTHING is filtered: in the canonical
//!    checkout any entry could be a peer's or the operator's. The runner's own
//!    provisioned artifacts are kept out of that listing at their source,
//!    by [`prepare_canonical_excludes`], never by filtering here.
//!
//! Then `git switch --no-guess <prior>` (or `git switch --detach <sha>` when
//! the checkout was detached), with no `--force` / `--discard-changes`, and a
//! read-back of HEAD. The agent branch is never deleted, even one this
//! allocation created: it may already be pushed or claimed.
//!
//! The caller ([`super::remove_unspawned`]) runs this while the allocation's
//! claims are still held: `IsolatedEditContext::remove_unspawned*` consume the
//! context, so its `Drop` — which releases the `canonical_checkout` lease, the
//! thing that keeps a peer's shared-branch switch off this checkout — runs
//! only after the restore returns. It also holds
//! [`super::checkout_materialize_lock`], which serializes it against the
//! in-process work that takes that lock (a `git worktree add` cut from the
//! checkout, and the exclude-block removal below).
//!
//! The runner's `.claude/` provisioning is hidden from proof 3 by a
//! runner-managed block in the checkout's `.git/info/exclude`
//! ([`prepare_canonical_excludes`]) that lives only as long as the
//! allocation: it is removed after a `Restored`, on a switch rollback, by
//! `release_repo`, and when the allocation's context drops. A block all of
//! those missed (a runner exit, crash or handoff restart, or a racing writer
//! in another process resurrecting it) is reaped by [`sweep_stale_blocks`] at
//! runner startup and on every tree-publisher cycle, and only once its
//! allocation is provably not live. Every in-process writer of the file holds
//! [`crate::fleet::info_exclude_lock`].

use std::path::Path;

use tracing::{info, warn};

/// Why a refused spawn's canonical checkout was left where it is. Every arm
/// is a refusal to switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestoreKeep {
    /// The checkout was already on the agent branch; this allocation switched
    /// nothing, so there is nothing of its own to undo.
    NothingSwitched,
    /// HEAD is no longer the agent branch (someone switched away, or HEAD is
    /// detached). Carries what HEAD read.
    NotOnAgentBranch(String),
    /// HEAD moved past the sha the switch left: a commit was made on the
    /// agent branch.
    BranchMoved { head: String },
    /// The checkout is not fully clean. Carries the porcelain listing.
    Dirty(String),
    /// The prior ref no longer resolves (the branch was deleted meanwhile).
    PriorRefMissing(String),
    /// A read the proofs depend on did not answer, or the recorded switch
    /// facts are empty — UNKNOWN, which never acts.
    ReadFailed(String),
    /// Every proof held, but `git switch` refused or did not land.
    SwitchFailed(String),
}

/// The answer for one refused shared-branch checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestoreOutcome {
    /// The checkout is back on its prior ref (read back after the switch).
    Restored,
    Kept(RestoreKeep),
}

impl RestoreKeep {
    /// One sentence for the log line.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::NothingSwitched => {
                "the checkout was already on the agent branch before this spawn — nothing was \
                 switched, so nothing is undone"
                    .into()
            }
            Self::NotOnAgentBranch(head) => {
                format!("HEAD reads {head:?}, not the agent branch — someone switched away")
            }
            Self::BranchMoved { head } => {
                format!("HEAD is {head}, past the post-switch sha — a commit was made")
            }
            Self::Dirty(p) => format!("the checkout is not fully clean: {}", p.trim()),
            Self::PriorRefMissing(r) => format!("the prior ref {r:?} no longer resolves"),
            Self::ReadFailed(e) => format!("a proof could not be read (UNKNOWN never acts): {e}"),
            Self::SwitchFailed(e) => format!("`git switch` (no force) did not land: {e}"),
        }
    }
}

/// Budget for each git child — the removal's own budget, so an unanswered
/// child is UNKNOWN and keeps the checkout.
const GIT_TIMEOUT: std::time::Duration = super::dirty::RECLAIM_DIRTY_PROBE_TIMEOUT;

/// The full-cleanliness listing proof 3 requires empty: every untracked file,
/// whatever `status.showUntrackedFiles` says, and ignored paths left out
/// (ignored content is not carried or clobbered by a switch to a ref that
/// does not track it, and the runner's excluded artifacts are ignored).
const PORCELAIN_FULL: &[&str] = &[
    "status",
    "--porcelain",
    "--untracked-files=all",
    "--ignored=no",
];

/// Put `checkout` back on `prior_ref` when every proof holds; otherwise say
/// why not. Blocking (runs git); never fails — every call gets an outcome.
pub(crate) fn restore_refused_shared_branch(
    checkout: &Path,
    branch: &str,
    prior_ref: &str,
    switched_to_sha: &str,
) -> RestoreOutcome {
    let outcome = judge_and_switch(checkout, branch, prior_ref, switched_to_sha);
    match &outcome {
        RestoreOutcome::Restored => info!(
            checkout = %checkout.display(),
            branch = %branch,
            prior_ref = %prior_ref,
            "shared_branch_restore: put the canonical checkout of a refused spawn back on its \
             prior ref (the agent branch is kept)"
        ),
        RestoreOutcome::Kept(why) => warn!(
            checkout = %checkout.display(),
            branch = %branch,
            prior_ref = %prior_ref,
            keep_reason = ?why,
            "shared_branch_restore: LEFT the canonical checkout of a refused spawn on the agent \
             branch — {}",
            why.describe()
        ),
    }
    outcome
}

fn judge_and_switch(
    checkout: &Path,
    branch: &str,
    prior_ref: &str,
    switched_to_sha: &str,
) -> RestoreOutcome {
    use RestoreKeep::*;
    let keep = RestoreOutcome::Kept;

    // (0) No switch happened → nothing of this allocation's to undo.
    if prior_ref == branch {
        return keep(NothingSwitched);
    }
    if branch.is_empty() || prior_ref.is_empty() || switched_to_sha.is_empty() {
        return keep(ReadFailed(format!(
            "the recorded switch is incomplete (branch {branch:?}, prior ref {prior_ref:?}, \
             post-switch sha {switched_to_sha:?})"
        )));
    }

    // (1) Still on the agent branch. `--symbolic-full-name` prints `HEAD` when
    // detached and exits 0 either way, so a non-zero exit is a failed read.
    let head_ref = match git_stdout(
        checkout,
        &["rev-parse", "--symbolic-full-name", "HEAD"],
        "shared_branch_restore: head ref",
    ) {
        Ok(r) => r.trim().to_string(),
        Err(e) => return keep(ReadFailed(e)),
    };
    if head_ref != format!("refs/heads/{branch}") {
        return keep(NotOnAgentBranch(head_ref));
    }

    // (2) No commit since the switch.
    let head = match git_stdout(
        checkout,
        &["rev-parse", "HEAD"],
        "shared_branch_restore: head",
    ) {
        Ok(h) => h.trim().to_string(),
        Err(e) => return keep(ReadFailed(e)),
    };
    if !head.eq_ignore_ascii_case(switched_to_sha.trim()) {
        return keep(BranchMoved { head });
    }

    // (3) Fully clean, filtering nothing.
    match git_stdout(checkout, PORCELAIN_FULL, "shared_branch_restore: status") {
        Ok(p) if p.trim().is_empty() => {}
        Ok(p) => return keep(Dirty(p)),
        Err(e) => return keep(ReadFailed(e)),
    }

    // Resolve what the prior ref names: a local branch, else (when HEAD was
    // detached) a commit sha.
    let target = match resolve_prior(checkout, prior_ref) {
        Ok(t) => t,
        Err(k) => return keep(k),
    };
    let switch_args: Vec<&str> = match &target {
        Prior::Branch => vec!["switch", "--no-guess", prior_ref],
        Prior::Detached => vec!["switch", "--detach", prior_ref],
    };
    if let Err(e) = git_stdout(checkout, &switch_args, "shared_branch_restore: switch") {
        return keep(SwitchFailed(e));
    }

    // Read back: a zero exit is not the state it names.
    let (args, want): (&[&str], String) = match target {
        Prior::Branch => (
            &["rev-parse", "--symbolic-full-name", "HEAD"],
            format!("refs/heads/{prior_ref}"),
        ),
        Prior::Detached => (&["rev-parse", "HEAD"], prior_ref.to_ascii_lowercase()),
    };
    match git_stdout(checkout, args, "shared_branch_restore: read-back") {
        Ok(got) if got.trim().eq_ignore_ascii_case(&want) => {
            // Back on the prior ref: this allocation's `.claude/` excludes
            // must not outlive it there.
            remove_allocation_excludes(checkout, branch);
            RestoreOutcome::Restored
        }
        Ok(got) => keep(SwitchFailed(format!(
            "`git switch` exited 0 but HEAD reads {:?}, not {want:?}",
            got.trim()
        ))),
        Err(e) => keep(SwitchFailed(format!(
            "`git switch` exited 0 but the read-back did not answer: {e}"
        ))),
    }
}

/// What the recorded prior ref turned out to name.
enum Prior {
    Branch,
    Detached,
}

/// A local branch named `prior` wins; otherwise a full hex sha naming a
/// commit is a detached prior. Anything else is
/// [`RestoreKeep::PriorRefMissing`]. A probe that did not answer (as opposed
/// to git's own "no such ref") is [`RestoreKeep::ReadFailed`].
fn resolve_prior(checkout: &Path, prior: &str) -> Result<Prior, RestoreKeep> {
    if ref_exists(checkout, &format!("refs/heads/{prior}"), "verify")? {
        return Ok(Prior::Branch);
    }
    let hex = matches!(prior.len(), 40 | 64) && prior.bytes().all(|b| b.is_ascii_hexdigit());
    if hex && ref_exists(checkout, &format!("{prior}^{{commit}}"), "commit")? {
        return Ok(Prior::Detached);
    }
    Err(RestoreKeep::PriorRefMissing(prior.to_string()))
}

/// `Ok(true)` when `rev` resolves, `Ok(false)` on git's own non-zero answer,
/// `Err(ReadFailed)` when git did not answer at all.
fn ref_exists(checkout: &Path, rev: &str, label: &str) -> Result<bool, RestoreKeep> {
    use crate::process_helpers::{run_probe_quiet, DegradeReason, ProbeOutcome};
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.arg("-C")
        .arg(checkout)
        .args(["rev-parse", "--verify", "--quiet", rev]);
    match run_probe_quiet(cmd, GIT_TIMEOUT, &format!("shared_branch_restore: {label}")) {
        ProbeOutcome::Captured(_) => Ok(true),
        ProbeOutcome::Degraded(DegradeReason::Status) => Ok(false),
        ProbeOutcome::Degraded(why) => Err(RestoreKeep::ReadFailed(format!(
            "git rev-parse --verify {rev} did not answer: {why:?}"
        ))),
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

/// The `.claude/` subtrees the runner provisions into a session cwd: the fleet
/// commands, the fleet skills, and the named-subagent definitions.
pub(crate) const PROVISIONED_CLAUDE_EXCLUDES: &[&str] =
    &[".claude/commands/", ".claude/skills/", ".claude/agents/"];

/// First line of the runner-managed block one shared-branch allocation adds to
/// a canonical checkout's `.git/info/exclude`; the agent branch follows it.
const BLOCK_BEGIN: &str = "# qontinui-runner managed: shared-branch allocation ";
/// Last line of that block; the agent branch follows it.
const BLOCK_END: &str = "# end qontinui-runner managed: shared-branch allocation ";

/// Make the artifacts a session provisions into a just-switched canonical
/// checkout invisible to `git status`, so a refused spawn's restore can
/// require that checkout fully clean without filtering anything.
///
/// - The machine-local roster ([`crate::fleet::ensure_repo_info_exclude`]:
///   `.mcp.json`, `.coord-mcp-status`, …) — always. It is the roster the
///   publisher already applies, permanently, to every primary checkout it
///   walks; a canonical checkout it has not walked yet would otherwise read
///   dirty.
/// - [`PROVISIONED_CLAUDE_EXCLUDES`] — only for the LIFE OF THIS ALLOCATION,
///   inside a block bracketed by [`BLOCK_BEGIN`]/[`BLOCK_END`] naming
///   `branch`, which [`remove_allocation_excludes`] takes out again (on a
///   restore, on a switch rollback, and when the allocation's context drops).
///   Added only when a switch actually happened (`prior_ref != branch`) and
///   `.claude` tracks nothing on the agent branch (the index, right after the
///   switch) AND nothing on `prior_ref`, both reads answered. That is
///   narrower than provisioning's own guard, which skips tracked files one by
///   one and still writes the untracked ones into a checkout that versions
///   some of `.claude/`: in such a checkout the block is not added, so the
///   runner's files there stay visible and a refused spawn's restore keeps the
///   checkout as `Dirty` — the safe side. Either read failing adds nothing too.
///   While the block stands it hides operator files under those trees, which
///   is why it never outlives the allocation and never carries `.claude/`
///   itself.
pub(crate) fn prepare_canonical_excludes(canonical: &Path, prior_ref: &str, branch: &str) {
    crate::fleet::ensure_repo_info_exclude(canonical);
    if prior_ref == branch {
        return;
    }
    let index = git_stdout(
        canonical,
        &["ls-files", "--", ".claude"],
        "shared_branch_restore: ls-files .claude",
    );
    let prior = git_stdout(
        canonical,
        &["ls-tree", "-r", "--name-only", prior_ref, "--", ".claude"],
        "shared_branch_restore: ls-tree .claude",
    );
    match (index, prior) {
        (Ok(i), Ok(p)) if i.trim().is_empty() && p.trim().is_empty() => {
            if let Err(e) = add_allocation_excludes(canonical, branch) {
                warn!(
                    checkout = %canonical.display(),
                    "shared_branch_restore: could not add the allocation's exclude block: {e}"
                );
            }
        }
        (Ok(_), Ok(_)) => info!(
            checkout = %canonical.display(),
            "shared_branch_restore: .claude/ is versioned here — the provisioned-bundle \
             excludes are not added"
        ),
        (i, p) => warn!(
            checkout = %canonical.display(),
            index = ?i.err(),
            prior = ?p.err(),
            "shared_branch_restore: could not establish that .claude/ tracks nothing — the \
             provisioned-bundle excludes are not added (a refused spawn's restore may then keep \
             the checkout as Dirty)"
        ),
    }
}

/// `<checkout>/.git/info/exclude` for a PRIMARY checkout (`.git` is a
/// directory); `None` for a linked worktree or an odd layout, which is never
/// written.
fn info_exclude_path(checkout: &Path) -> Option<std::path::PathBuf> {
    let git_dir = checkout.join(".git");
    git_dir
        .is_dir()
        .then(|| git_dir.join("info").join("exclude"))
}

/// The allocations whose block THIS process added and has not removed —
/// `(checkout key, branch)`. The sweep never removes one of these: they are
/// live here by construction, whatever coord can be asked.
fn live_here() -> std::sync::MutexGuard<'static, std::collections::HashSet<(String, String)>> {
    static LIVE: once_cell::sync::Lazy<
        std::sync::Mutex<std::collections::HashSet<(String, String)>>,
    > = once_cell::sync::Lazy::new(Default::default);
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

fn live_key(checkout: &Path, branch: &str) -> (String, String) {
    (super::worktree_resource_key(checkout), branch.to_string())
}

/// Test-only: drop this process's record of a live allocation, as a runner
/// exit or crash does, leaving its block on disk.
#[cfg(test)]
pub(crate) fn forget_live_for_test(checkout: &Path, branch: &str) {
    live_here().remove(&live_key(checkout, branch));
}

/// The file's lines WITH their terminators (`\n` or `\r\n`, or none on a last
/// line with no final newline), so a rewrite can keep every byte outside the
/// block exactly as it was.
fn lines_with_terminators(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// A line's content: its terminator stripped.
fn content(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

/// Append this allocation's block (idempotent: a block already naming
/// `branch` is left as it is), under the shared exclude lock, in the file's
/// own line ending. Lines outside the block are never touched.
fn add_allocation_excludes(checkout: &Path, branch: &str) -> Result<(), String> {
    let Some(path) = info_exclude_path(checkout) else {
        return Ok(());
    };
    let _excl = crate::fleet::info_exclude_lock();
    let existing = crate::fleet::read_info_exclude(&path)?;
    let begin = format!("{BLOCK_BEGIN}{branch}");
    if !lines_with_terminators(&existing)
        .iter()
        .any(|l| content(l) == begin)
    {
        let eol = if existing.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut out = existing;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(&begin);
        out.push_str(eol);
        for pat in PROVISIONED_CLAUDE_EXCLUDES {
            out.push_str(pat);
            out.push_str(eol);
        }
        out.push_str(&format!("{BLOCK_END}{branch}{eol}"));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, out).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    live_here().insert(live_key(checkout, branch));
    Ok(())
}

/// Take out exactly the block [`prepare_canonical_excludes`] added for
/// `branch` — its begin line through its end line — and nothing else; every
/// byte outside it (line endings, a missing final newline) is kept.
/// Idempotent (no block is a no-op). A begin line with no matching end is
/// left alone and reported: removing to end-of-file could delete operator
/// lines. A file that cannot be read, or is not UTF-8, is not rewritten.
/// Best-effort and logged; never fails the caller.
pub(crate) fn remove_allocation_excludes(checkout: &Path, branch: &str) {
    live_here().remove(&live_key(checkout, branch));
    if let Err(e) = try_remove_allocation_excludes(checkout, branch) {
        warn!(
            checkout = %checkout.display(),
            branch = %branch,
            "shared_branch_restore: the allocation's exclude block was NOT removed: {e}"
        );
    }
}

fn try_remove_allocation_excludes(checkout: &Path, branch: &str) -> Result<(), String> {
    let Some(path) = info_exclude_path(checkout) else {
        return Ok(());
    };
    let _excl = crate::fleet::info_exclude_lock();
    let existing = crate::fleet::read_info_exclude(&path)?;
    let begin = format!("{BLOCK_BEGIN}{branch}");
    let end = format!("{BLOCK_END}{branch}");
    let lines = lines_with_terminators(&existing);
    let Some(b) = lines.iter().position(|l| content(l) == begin) else {
        return Ok(());
    };
    let Some(e) = lines[b..]
        .iter()
        .position(|l| content(l) == end)
        .map(|i| b + i)
    else {
        return Err(format!(
            "{} has a begin line for {branch:?} with no end line — left as is",
            path.display()
        ));
    };
    let out: String = lines[..b].concat() + &lines[e + 1..].concat();
    std::fs::write(&path, out).map_err(|err| format!("write {}: {err}", path.display()))?;
    info!(
        checkout = %checkout.display(),
        branch = %branch,
        "shared_branch_restore: removed the allocation's exclude block"
    );
    Ok(())
}

/// The agent branch of every allocation block in `checkout`'s exclude file
/// (each begin line's suffix). Empty when there is no file, no block, or the
/// file cannot be read.
fn block_branches(checkout: &Path) -> Vec<String> {
    let Some(path) = info_exclude_path(checkout) else {
        return Vec::new();
    };
    let Ok(text) = crate::fleet::read_info_exclude(&path) else {
        return Vec::new();
    };
    lines_with_terminators(&text)
        .iter()
        .filter_map(|l| content(l).strip_prefix(BLOCK_BEGIN))
        .map(str::to_string)
        .collect()
}

/// Whether a live session holds a checkout, as far as can be established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HolderState {
    /// A live holder (this process's own allocation, or coord's claim).
    Held,
    /// coord answered: nothing holds it.
    Free,
    /// Could not be established — keeps the block.
    Unknown(String),
}

/// What the sweep did with one block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SweepOutcome {
    /// The allocation is provably not live; the block was taken out.
    Removed { branch: String },
    /// Kept, and why.
    Kept { branch: String, why: String },
}

/// Reap allocation blocks left behind by a runner that exited, crashed, or
/// was restarted before it removed its own (and any a racing writer in
/// another process resurrected). A block is removed only when its allocation
/// is PROVABLY not live:
///
/// - its `<branch>` no longer exists (git's own "no such ref"); or
/// - the checkout's HEAD is not `refs/heads/<branch>` AND `holder` says
///   nothing holds the checkout ([`HolderState::Free`]).
///
/// Every unknown — an unreadable HEAD, a branch probe that did not answer, an
/// unanswered holder lookup — keeps the block, and says so. Blocking.
pub(crate) fn sweep_stale_blocks(
    checkout: &Path,
    holder: impl Fn(&str) -> HolderState,
) -> Vec<SweepOutcome> {
    let mut out = Vec::new();
    for branch in block_branches(checkout) {
        let outcome = judge_block(checkout, &branch, &holder);
        match &outcome {
            SweepOutcome::Removed { branch } => {
                remove_allocation_excludes(checkout, branch);
                info!(
                    checkout = %checkout.display(),
                    branch = %branch,
                    "shared_branch_restore: swept a stale allocation exclude block"
                );
            }
            SweepOutcome::Kept { branch, why } => info!(
                checkout = %checkout.display(),
                branch = %branch,
                "shared_branch_restore: kept an allocation exclude block — {why}"
            ),
        }
        out.push(outcome);
    }
    out
}

fn judge_block(
    checkout: &Path,
    branch: &str,
    holder: &impl Fn(&str) -> HolderState,
) -> SweepOutcome {
    let kept = |why: String| SweepOutcome::Kept {
        branch: branch.to_string(),
        why,
    };
    let removed = || SweepOutcome::Removed {
        branch: branch.to_string(),
    };
    match ref_exists(checkout, &format!("refs/heads/{branch}"), "sweep branch") {
        Ok(false) => return removed(),
        Ok(true) => {}
        Err(k) => return kept(format!("the branch probe did not answer: {k:?}")),
    }
    let head = match git_stdout(
        checkout,
        &["rev-parse", "--symbolic-full-name", "HEAD"],
        "shared_branch_restore: sweep head",
    ) {
        Ok(h) => h.trim().to_string(),
        Err(e) => return kept(format!("HEAD could not be read: {e}")),
    };
    if head == format!("refs/heads/{branch}") {
        return kept("the checkout is on the allocation's branch".into());
    }
    match holder(branch) {
        HolderState::Free => removed(),
        HolderState::Held => kept("a live holder holds the checkout".into()),
        HolderState::Unknown(e) => kept(format!(
            "whether anything holds the checkout is unknown: {e}"
        )),
    }
}

/// [`sweep_stale_blocks`] for one checkout with its holder resolved: this
/// process's own live allocation first, then coord's `kind=worktree` claim on
/// the checkout (the claim a shared-branch session holds on the canonical
/// checkout it works in). No coord base, or no answer, is
/// [`HolderState::Unknown`]. Cheap when the file holds no block: nothing is
/// asked.
pub(crate) async fn sweep_checkout(checkout: std::path::PathBuf, coord_base: Option<String>) {
    let probe = checkout.clone();
    let branches = tokio::task::spawn_blocking(move || block_branches(&probe))
        .await
        .unwrap_or_default();
    if branches.is_empty() {
        return;
    }
    let coord = match coord_base {
        Some(base) => {
            let key = super::worktree_resource_key(&checkout);
            match super::fs_backstop::canonical_checkout_claim_holder(&base, &key).await {
                Ok(Some(_)) => HolderState::Held,
                Ok(None) => HolderState::Free,
                Err(e) => HolderState::Unknown(e),
            }
        }
        None => HolderState::Unknown("no coord base configured".into()),
    };
    let _ = tokio::task::spawn_blocking(move || {
        sweep_stale_blocks(&checkout, |branch| {
            if live_here().contains(&live_key(&checkout, branch)) {
                HolderState::Held
            } else {
                coord.clone()
            }
        })
    })
    .await;
}

/// Run [`sweep_checkout`] over every `qontinui-*` primary checkout under the
/// workspace root — at runner startup, before any allocation of this process
/// can hold a block.
pub(crate) async fn sweep_workspace(coord_base: Option<String>) {
    let Some(root) = super::census::qontinui_root() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_repo = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("qontinui-"));
        if is_repo && path.join(".git").is_dir() {
            sweep_checkout(path, coord_base.clone()).await;
        }
    }
}

#[cfg(test)]
#[path = "shared_branch_restore_tests.rs"]
mod tests;
