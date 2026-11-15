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
//! The caller ([`super::remove_unspawned`]) runs this under
//! [`super::checkout_materialize_lock`] and while the allocation's claims are
//! still held: `IsolatedEditContext::remove_unspawned*` consume the context,
//! so its `Drop` — which releases the `canonical_checkout` lease — runs only
//! after the restore returns.

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
        Ok(got) if got.trim().eq_ignore_ascii_case(&want) => RestoreOutcome::Restored,
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

/// Make the artifacts a session provisions into a just-switched canonical
/// checkout invisible to `git status`, so a refused spawn's restore can
/// require that checkout fully clean without filtering anything.
///
/// - The machine-local roster ([`crate::fleet::ensure_repo_info_exclude`]:
///   `.mcp.json`, `.coord-mcp-status`, …) — always; it is the roster the
///   publisher already applies to every primary checkout it walks, and a
///   canonical checkout it has not walked yet would otherwise read dirty.
/// - [`PROVISIONED_CLAUDE_EXCLUDES`] — ONLY when `.claude` tracks nothing on
///   the agent branch (the index, right after the switch) AND nothing on
///   `prior_ref`, both reads answered. That is the same predicate under which
///   provisioning writes these trees at all (a repo-authored `.claude/` is
///   never provisioned into), so in such a checkout everything under them is
///   the runner's bundle. Where `.claude/` IS authored, or either read fails,
///   nothing is added: an operator's new `.claude/commands/*.md` there must
///   stay visible, which is why the roster itself never carries `.claude/`.
pub(crate) fn prepare_canonical_excludes(canonical: &Path, prior_ref: &str) {
    crate::fleet::ensure_repo_info_exclude(canonical);
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
            crate::fleet::ensure_info_exclude_entries(canonical, PROVISIONED_CLAUDE_EXCLUDES);
        }
        (Ok(_), Ok(_)) => info!(
            checkout = %canonical.display(),
            "shared_branch_restore: .claude/ is repo-authored here — the provisioned-bundle \
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

#[cfg(test)]
#[path = "shared_branch_restore_tests.rs"]
mod tests;
