//! Candidate checkout: fetch the dispatched SHA into the warm primary
//! checkout, then materialize an ephemeral detached worktree for the build.
//!
//! Layout (all under QONTINUI_ROOT):
//! - primary repo dir: `<root>/<repo basename>` — the fetch target (warm
//!   objects, warm git config).
//! - **dispatch root**: `<root>/.ci-worktrees/<dispatch_id>` — the unit of
//!   cleanup. Always removed, success or failure.
//! - CI worktree: `<dispatch root>/<repo basename>` — the dispatched tree.
//! - siblings: `<dispatch root>/<sibling basename>` — materialised by
//!   [`super::sibling`].
//!
//! # Why the worktree gained a level
//!
//! It used to be `<root>/.ci-worktrees/<dispatch_id>` directly. That put the
//! build tree one level DEEPER than the primary clone at `<root>/<repo>`, so a
//! relative path-dep (`../qontinui-schemas/rust` in Cargo,
//! `../../qontinui-schemas` from `backend/` in Poetry — different depths, same
//! resolved location) pointed at `<root>/.ci-worktrees/qontinui-schemas`
//! rather than anywhere a checkout could be placed. Giving the dispatch its
//! own parent makes `../<sibling>` land INSIDE that parent, which is
//! simultaneously:
//!
//! - the one layout rule that satisfies both toolchains, and
//! - a strictly SMALLER cleanup surface than provisioning siblings anywhere
//!   else would be, because the parent is still a single `remove_dir_all`.
//!
//! [`cleanup_dispatch`] therefore keeps its three-step shape — worktree
//! remove, prune, directory delete — with the delete widened from the worktree
//! to the dispatch root. Nothing outside `<root>/.ci-worktrees/<dispatch_id>`
//! is ever touched.

use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Fetch budget — candidate refs are usually a few objects on top of a
/// warm clone, but a cold repo can be slow.
const GIT_FETCH_TIMEOUT: Duration = Duration::from_secs(600);
/// Everything else (cat-file, worktree add/remove/prune) is local.
const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(120);

/// Run git in `repo_dir`, capturing combined output. Err carries a
/// log-worthy one-liner.
pub(super) async fn run_git(
    repo_dir: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let mut cmd = crate::process_helpers::tokio_no_window("git");
    // kill_on_drop: a fetch raced against cancellation or cut off by its
    // timeout is dropped, and its git child must die with it, not linger
    // holding the repo's locks.
    cmd.current_dir(repo_dir)
        .kill_on_drop(true)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let fut = async {
        let out = cmd
            .output()
            .await
            .map_err(|e| format!("spawn git {}: {e}", args.join(" ")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if out.status.success() {
            Ok(stdout)
        } else {
            Err(format!(
                "git {} failed ({}): {}",
                args.join(" "),
                out.status,
                if stderr.is_empty() { &stdout } else { &stderr }
            ))
        }
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "git {} timed out after {}s",
            args.join(" "),
            timeout.as_secs()
        )),
    }
}

/// The dispatch-scoped parent: the dispatched worktree and every provisioned
/// sibling live under it, and it is the one directory cleanup removes.
pub(crate) fn ci_dispatch_root(root: &Path, dispatch_id: &str) -> PathBuf {
    root.join(".ci-worktrees").join(dispatch_id)
}

/// The CI worktree path for a dispatch — a child of [`ci_dispatch_root`]
/// named after the repo, so `../<sibling>` from inside it resolves to a
/// sibling of the worktree.
pub(crate) fn ci_worktree_path(root: &Path, dispatch_id: &str, repo: &str) -> PathBuf {
    ci_dispatch_root(root, dispatch_id).join(crate::agent_runtime::local_repo_name(repo))
}

/// How hard [`prepare_worktree`] tries to obtain the dispatched head.
///
/// Every number is bounded by ONE deadline, well inside coord's 15-minute
/// dispatch lease: a retry loop that could outlive the lease would turn a
/// missing head into a `lost` row instead of a `head_sha_unavailable` one.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeadFetchPolicy<'a> {
    /// Pauses between attempts; attempts = `backoff.len() + 1`.
    pub backoff: &'a [Duration],
    /// Per-fetch timeout on the first attempt (a cold repo can be slow).
    pub first_fetch_timeout: Duration,
    /// Per-fetch timeout on a retry — only the few objects on top of what the
    /// first attempt already brought in are missing by then.
    pub retry_fetch_timeout: Duration,
    /// Wall-clock bound on the whole fetch-and-retry loop. Every fetch and
    /// every pause is clipped to what remains of it.
    pub deadline: Duration,
}

/// Three attempts, 30 s + 60 s apart, inside a 10-minute deadline: absorbs a
/// publish-before-push race from a coord build that dispatches a tip before
/// the ref naming it has reached the mirror, and still leaves a third of the
/// 15-minute lease for the rest of setup.
pub(crate) const HEAD_FETCH_POLICY: HeadFetchPolicy<'static> = HeadFetchPolicy {
    backoff: &[Duration::from_secs(30), Duration::from_secs(60)],
    first_fetch_timeout: GIT_FETCH_TIMEOUT,
    retry_fetch_timeout: Duration::from_secs(120),
    deadline: Duration::from_secs(600),
};

/// `summary.reason` for a dispatch whose commit could not be obtained. Coord's
/// result route admits only `success|failure|cancelled`, so a missing head is
/// `cancelled` + this reason — a non-verdict — rather than a new state.
pub(crate) const HEAD_UNAVAILABLE_REASON: &str = "head_sha_unavailable";

/// Candidate-ref namespaces coord dispatches: the merge-candidate branch, and
/// the per-dispatch ref (plan Phase 2).
const CANDIDATE_REF_PREFIXES: [&str; 2] = ["refs/heads/merge-candidate/", "refs/ci-dispatch/"];

/// Why [`prepare_worktree`] produced no tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckoutError {
    /// The dispatched commit is not obtainable from the mirror: the candidate
    /// ref was absent or pointed elsewhere, and a bare-SHA fetch found nothing
    /// either, on every attempt the deadline allowed. This says nothing about
    /// the code under test — the candidate was never published or was replaced
    /// — so it must never be reported as a test `failure` (that poisons shadow
    /// parity).
    HeadUnavailable {
        head_sha: String,
        fetch_url: String,
        attempts: usize,
        detail: String,
    },
    /// The dispatch was cancelled while the checkout was fetching or waiting.
    Cancelled,
    /// Any other setup failure (an invalid payload, no primary checkout,
    /// worktree add failed…).
    Failed(String),
}

impl std::fmt::Display for CheckoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckoutError::HeadUnavailable {
                head_sha,
                fetch_url,
                attempts,
                detail,
            } => write!(
                f,
                "head_sha {head_sha} not obtainable from {fetch_url} after {attempts} attempt(s): {detail}"
            ),
            CheckoutError::Cancelled => write!(f, "dispatch cancelled during checkout"),
            CheckoutError::Failed(e) => f.write_str(e),
        }
    }
}

impl From<String> for CheckoutError {
    fn from(e: String) -> Self {
        CheckoutError::Failed(e)
    }
}

impl CheckoutError {
    /// The `(conclusion, summary.reason)` pair the executor reports.
    pub(crate) fn result_disposition(&self) -> (&'static str, Option<&'static str>) {
        match self {
            CheckoutError::HeadUnavailable { .. } => ("cancelled", Some(HEAD_UNAVAILABLE_REASON)),
            CheckoutError::Cancelled => ("cancelled", None),
            CheckoutError::Failed(_) => ("failure", None),
        }
    }

    /// The progress-log line prefix: a cancelled or unavailable checkout is
    /// not a "checkout failed", and the log must not say it is.
    pub(crate) fn log_prefix(&self) -> &'static str {
        match self {
            CheckoutError::HeadUnavailable { .. } => "[ci-node] checkout: head unavailable:",
            CheckoutError::Cancelled => "[ci-node] checkout cancelled:",
            CheckoutError::Failed(_) => "[ci-node] checkout failed:",
        }
    }
}

/// Refuse a dispatch payload whose git arguments could be anything but what
/// they claim to be. They come off the wire and end up on a `git fetch` argv,
/// where a value such as `--upload-pack=<cmd>` would EXECUTE — git parses
/// options after positionals. The `--` before every fetch's positionals is the
/// second, independent half of this guard.
fn validate_fetch_args(fetch_url: &str, candidate_ref: &str, head_sha: &str) -> Result<(), String> {
    if !super::sibling::is_full_sha(head_sha) {
        return Err(format!(
            "refusing dispatch: head_sha {head_sha:?} is not a full 40-hex commit id"
        ));
    }
    let ref_ok = CANDIDATE_REF_PREFIXES.iter().any(|prefix| {
        candidate_ref.strip_prefix(prefix).is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        })
    });
    if !ref_ok {
        return Err(format!(
            "refusing dispatch: candidate_ref {candidate_ref:?} is not under {}",
            CANDIDATE_REF_PREFIXES.join(" or ")
        ));
    }
    if fetch_url.is_empty()
        || fetch_url.starts_with('-')
        || fetch_url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!(
            "refusing dispatch: fetch_url {fetch_url:?} is not a plain URL"
        ));
    }
    Ok(())
}

/// `git cat-file -e <sha>^{commit}` — is the commit in the local object store?
async fn head_present(repo_dir: &Path, head_sha: &str) -> bool {
    run_git(
        repo_dir,
        &["cat-file", "-e", &format!("{head_sha}^{{commit}}")],
        GIT_LOCAL_TIMEOUT,
    )
    .await
    .is_ok()
}

/// A fetch that yields to cancellation and to the loop deadline. `Err(())`
/// means cancelled; the git child is killed (`run_git` sets `kill_on_drop`),
/// never orphaned.
async fn fetch_once(
    repo_dir: &Path,
    args: &[&str],
    per_fetch: Duration,
    deadline: tokio::time::Instant,
    cancel: &CancellationToken,
) -> Result<Result<String, String>, ()> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Ok(Err("head-fetch deadline exhausted".to_string()));
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(()),
        r = run_git(repo_dir, args, per_fetch.min(remaining)) => Ok(r),
    }
}

/// Make `head_sha` present in `repo_dir`, or say precisely why it is not.
///
/// Each attempt fetches `candidate_ref` and then PROBES for the commit — a
/// successful ref fetch proves only that the ref exists, not that it names the
/// dispatched commit (coord may still be serving the previous cut). When the
/// probe misses, whether the ref fetch errored or succeeded, a bare-SHA fetch
/// follows: the coord mirror honours want-by-SHA for any commit ever pushed.
///
/// Every fetch and every pause races `cancel`, and all of them are clipped to
/// `policy.deadline`. `before_retry(attempt)` runs after each pause, before
/// that attempt's fetches (a test seam; production passes a no-op).
async fn fetch_head(
    repo_dir: &Path,
    fetch_url: &str,
    candidate_ref: &str,
    head_sha: &str,
    policy: &HeadFetchPolicy<'_>,
    cancel: &CancellationToken,
    mut before_retry: impl FnMut(usize),
) -> Result<(), CheckoutError> {
    let deadline = tokio::time::Instant::now() + policy.deadline;
    let max_attempts = policy.backoff.len() + 1;
    let mut attempts = 0usize;
    let mut last: Vec<String> = Vec::new();
    for attempt in 1..=max_attempts {
        let per_fetch = if attempt == 1 {
            policy.first_fetch_timeout
        } else {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let wait = policy.backoff[attempt - 2].min(remaining);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(CheckoutError::Cancelled),
                _ = tokio::time::sleep(wait) => {}
            }
            before_retry(attempt);
            policy.retry_fetch_timeout
        };
        attempts = attempt;
        last.clear();
        let ref_args = ["fetch", "--", fetch_url, candidate_ref];
        match fetch_once(repo_dir, &ref_args, per_fetch, deadline, cancel).await {
            Err(()) => return Err(CheckoutError::Cancelled),
            Ok(Ok(_)) if head_present(repo_dir, head_sha).await => return Ok(()),
            Ok(Ok(_)) => last.push(format!(
                "{candidate_ref} fetched but does not contain {head_sha}"
            )),
            Ok(Err(e)) => last.push(format!("ref fetch: {e}")),
        }
        let sha_args = ["fetch", "--", fetch_url, head_sha];
        match fetch_once(repo_dir, &sha_args, per_fetch, deadline, cancel).await {
            Err(()) => return Err(CheckoutError::Cancelled),
            Ok(Ok(_)) if head_present(repo_dir, head_sha).await => return Ok(()),
            Ok(Ok(_)) => last.push(format!("bare-SHA fetch succeeded but {head_sha} is absent")),
            Ok(Err(e)) => last.push(format!("bare-SHA fetch: {e}")),
        }
        warn!(
            "ci_node: {head_sha} not obtainable (attempt {attempt}/{max_attempts}): {}",
            last.join("; ")
        );
    }
    Err(CheckoutError::HeadUnavailable {
        head_sha: head_sha.to_string(),
        fetch_url: fetch_url.to_string(),
        attempts,
        detail: last.join("; "),
    })
}

/// Validate + fetch + verify + worktree-add. Returns the worktree path.
///
/// `cancel` interrupts every fetch and every retry pause; a cancelled fetch's
/// git process is killed.
pub(crate) async fn prepare_worktree(
    root: &Path,
    repo: &str,
    dispatch_id: &str,
    fetch_url: &str,
    candidate_ref: &str,
    head_sha: &str,
    cancel: &CancellationToken,
) -> Result<PathBuf, CheckoutError> {
    prepare_worktree_with(
        root,
        repo,
        dispatch_id,
        fetch_url,
        candidate_ref,
        head_sha,
        &HEAD_FETCH_POLICY,
        cancel,
        |_| {},
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn prepare_worktree_with(
    root: &Path,
    repo: &str,
    dispatch_id: &str,
    fetch_url: &str,
    candidate_ref: &str,
    head_sha: &str,
    policy: &HeadFetchPolicy<'_>,
    cancel: &CancellationToken,
    before_retry: impl FnMut(usize),
) -> Result<PathBuf, CheckoutError> {
    validate_fetch_args(fetch_url, candidate_ref, head_sha)?;
    let repo_dir = root.join(crate::agent_runtime::local_repo_name(repo));
    if !repo_dir.join(".git").exists() {
        return Err(CheckoutError::Failed(format!(
            "primary checkout {} not present on this device (no .git)",
            repo_dir.display()
        )));
    }

    // Warm-SHA precheck (plan Phase 2): the primary checkout's agent daemons
    // fetch constantly, so the dispatched commit is often already local —
    // skip the network round-trip entirely when `git cat-file -e` says so.
    if head_present(&repo_dir, head_sha).await {
        info!(
            "ci_node: {head_sha} already present in {} — skipping fetch",
            repo_dir.display()
        );
    } else {
        fetch_head(
            &repo_dir,
            fetch_url,
            candidate_ref,
            head_sha,
            policy,
            cancel,
            before_retry,
        )
        .await?;
    }

    let dispatch_root = ci_dispatch_root(root, dispatch_id);
    let wt_path = ci_worktree_path(root, dispatch_id, repo);
    if dispatch_root.exists() {
        // Stale leftover from a crashed prior attempt — clear the WHOLE
        // dispatch root (not just the worktree), because a half-provisioned
        // sibling would otherwise trip materialise's "already exists"
        // refusal.
        warn!(
            "ci_node: stale dispatch dir {} exists; removing before re-add",
            dispatch_root.display()
        );
        cleanup_dispatch(root, repo, dispatch_id).await;
    }
    std::fs::create_dir_all(&dispatch_root)
        .map_err(|e| format!("create {}: {e}", dispatch_root.display()))?;

    let wt_str = wt_path.to_string_lossy().to_string();
    run_git(
        &repo_dir,
        &["worktree", "add", "--detach", &wt_str, head_sha],
        GIT_LOCAL_TIMEOUT,
    )
    .await?;
    info!(
        "ci_node: worktree ready at {} for {repo}@{head_sha}",
        wt_path.display()
    );
    Ok(wt_path)
}

/// Always-run cleanup: `git worktree remove --force` + prune, then a
/// best-effort delete of the whole dispatch root — which sweeps the worktree,
/// every provisioned sibling, and anything git left behind, in one call.
/// Failure is logged, never propagated (cleanup runs on failure paths too).
pub(crate) async fn cleanup_dispatch(root: &Path, repo: &str, dispatch_id: &str) {
    let repo_dir = root.join(crate::agent_runtime::local_repo_name(repo));
    let wt_path = ci_worktree_path(root, dispatch_id, repo);
    let dispatch_root = ci_dispatch_root(root, dispatch_id);
    let wt_str = wt_path.to_string_lossy().to_string();
    if let Err(e) = run_git(
        &repo_dir,
        &["worktree", "remove", "--force", &wt_str],
        GIT_LOCAL_TIMEOUT,
    )
    .await
    {
        warn!("ci_node: worktree remove failed (continuing to prune): {e}");
    }
    if let Err(e) = run_git(&repo_dir, &["worktree", "prune"], GIT_LOCAL_TIMEOUT).await {
        warn!("ci_node: worktree prune failed: {e}");
    }
    if dispatch_root.exists() {
        if let Err(e) = std::fs::remove_dir_all(&dispatch_root) {
            warn!(
                "ci_node: residual dispatch dir {} not removable: {e}",
                dispatch_root.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_worktree_path_is_dispatch_scoped_under_root() {
        let p = ci_worktree_path(Path::new("/root"), "d-123", "qontinui/qontinui-coord");
        assert!(p.starts_with("/root"));
        assert!(p.ends_with(
            PathBuf::from(".ci-worktrees")
                .join("d-123")
                .join("qontinui-coord")
        ));
    }

    /// The cleanup guarantee, stated as a path fact: everything a dispatch
    /// creates — its worktree and every sibling — is under the ONE directory
    /// `cleanup_dispatch` removes.
    #[test]
    fn everything_a_dispatch_creates_is_under_the_cleanup_unit() {
        let root = Path::new("/root");
        let dispatch_root = ci_dispatch_root(root, "d-123");
        assert_eq!(
            dispatch_root,
            PathBuf::from("/root").join(".ci-worktrees").join("d-123")
        );
        let wt = ci_worktree_path(root, "d-123", "qontinui/qontinui-coord");
        assert!(wt.starts_with(&dispatch_root));
        // A second dispatch is a disjoint tree, so concurrent builds cannot
        // clean up each other's siblings.
        assert!(!ci_dispatch_root(root, "d-124").starts_with(&dispatch_root));
    }

    /// The worktree is a CHILD of the dispatch root, not the dispatch root
    /// itself — that extra level is what makes `../<sibling>` resolvable.
    #[test]
    fn worktree_has_a_sibling_slot_next_to_it() {
        let root = Path::new("/root");
        let wt = ci_worktree_path(root, "d-123", "qontinui/qontinui-coord");
        assert_eq!(wt.parent(), Some(ci_dispatch_root(root, "d-123").as_path()));
    }

    // ── Head-fetch backstop, on real git repos ──────────────────────────────

    /// Synchronous git for fixtures; panics with git's output on failure.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=ci", "-c", "user.email=ci@example.invalid"])
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(dir: &Path, msg: &str) -> String {
        git(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
        git(dir, &["rev-parse", "HEAD"])
    }

    fn slash(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    /// `<tmp>/origin` — the "coord mirror", configured as coord's is
    /// (`uploadpack.allowAnySHA1InWant`) — holding commit A on `main` and on
    /// `merge-candidate/1`; and `<tmp>/root/qontinui-runner`, an empty primary
    /// checkout that has none of origin's objects.
    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        origin: PathBuf,
        a: String,
    }

    const REPO: &str = "qontinui/qontinui-runner";
    const CANDIDATE: &str = "refs/heads/merge-candidate/1";

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        git(
            &origin,
            &["config", "uploadpack.allowAnySHA1InWant", "true"],
        );
        let a = commit(&origin, "A");
        git(&origin, &["update-ref", CANDIDATE, &a]);
        let root = tmp.path().join("root");
        let primary = root.join("qontinui-runner");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        Fixture {
            _tmp: tmp,
            root,
            origin,
            a,
        }
    }

    /// A commit B on top of A in origin, reachable only from `other`.
    fn commit_b_on_other_branch(f: &Fixture) -> String {
        git(&f.origin, &["checkout", "-q", "-b", "other"]);
        let b = commit(&f.origin, "B");
        git(&f.origin, &["checkout", "-q", "main"]);
        b
    }

    /// Production's shape (three attempts), with no waiting.
    const TEST_POLICY: HeadFetchPolicy<'static> = HeadFetchPolicy {
        backoff: &[Duration::ZERO, Duration::ZERO],
        first_fetch_timeout: Duration::from_secs(60),
        retry_fetch_timeout: Duration::from_secs(60),
        deadline: Duration::from_secs(120),
    };

    const GHOST: &str = "0123456789abcdef0123456789abcdef01234567";

    async fn prepare_with(
        f: &Fixture,
        candidate_ref: &str,
        head: &str,
        policy: &HeadFetchPolicy<'_>,
        before_retry: impl FnMut(usize),
    ) -> Result<PathBuf, CheckoutError> {
        prepare_worktree_with(
            &f.root,
            REPO,
            "d-1",
            &slash(&f.origin),
            candidate_ref,
            head,
            policy,
            &CancellationToken::new(),
            before_retry,
        )
        .await
    }

    async fn prepare(
        f: &Fixture,
        candidate_ref: &str,
        head: &str,
    ) -> Result<PathBuf, CheckoutError> {
        prepare_with(f, candidate_ref, head, &TEST_POLICY, |_| {}).await
    }

    /// Arm 1: the candidate ref does not exist; the SHA is fetchable by want.
    #[tokio::test]
    async fn ref_fetch_errors_and_bare_sha_fetch_recovers() {
        let f = fixture();
        let wt = prepare(&f, "refs/heads/merge-candidate/absent", &f.a)
            .await
            .expect("bare-SHA fallback must recover");
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), f.a);
    }

    /// Arm 2: the candidate ref fetch SUCCEEDS but names the previous cut (A)
    /// while the dispatched head is B. Before the backstop, a successful ref
    /// fetch skipped the bare-SHA fetch and the dispatch failed at the gate.
    #[tokio::test]
    async fn ref_fetch_succeeds_on_the_wrong_commit_and_bare_sha_fetch_recovers() {
        let f = fixture();
        let b = commit_b_on_other_branch(&f);
        let wt = prepare(&f, CANDIDATE, &b)
            .await
            .expect("a stale candidate ref must not strand a fetchable head");
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), b);
    }

    /// The Phase 2 per-dispatch ref namespace is accepted too.
    #[tokio::test]
    async fn per_dispatch_ref_namespace_is_accepted() {
        let f = fixture();
        git(&f.origin, &["update-ref", "refs/ci-dispatch/d-1", &f.a]);
        let wt = prepare(&f, "refs/ci-dispatch/d-1", &f.a)
            .await
            .expect("refs/ci-dispatch/<id> is a valid candidate ref");
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), f.a);
    }

    /// Arm 3: the head exists nowhere. Every attempt runs, and the result is
    /// the typed non-verdict — reported `cancelled` + `head_sha_unavailable`,
    /// never `failure`.
    #[tokio::test]
    async fn head_fetchable_nowhere_is_head_unavailable_not_a_failure() {
        let f = fixture();
        let mut retries = 0usize;
        let err = prepare_with(&f, CANDIDATE, GHOST, &TEST_POLICY, |_| retries += 1)
            .await
            .expect_err("a ghost head cannot be checked out");
        assert!(
            matches!(&err, CheckoutError::HeadUnavailable { attempts: 3, head_sha, .. } if head_sha == GHOST),
            "{err:?}"
        );
        assert_eq!(retries, 2, "three attempts means two retries");
        assert_eq!(
            err.result_disposition(),
            ("cancelled", Some(HEAD_UNAVAILABLE_REASON))
        );
        assert!(
            !ci_dispatch_root(&f.root, "d-1").exists(),
            "no dispatch dir is created for an unavailable head"
        );
    }

    /// The deadline bounds the WHOLE loop: once it is spent no further attempt
    /// starts and no fetch is spawned, however many attempts the backoff
    /// would otherwise allow.
    #[tokio::test]
    async fn spent_deadline_stops_the_retry_loop() {
        let f = fixture();
        let policy = HeadFetchPolicy {
            deadline: Duration::ZERO,
            ..TEST_POLICY
        };
        let mut retries = 0usize;
        let err = prepare_with(&f, CANDIDATE, GHOST, &policy, |_| retries += 1)
            .await
            .expect_err("nothing can be fetched with no time left");
        match &err {
            CheckoutError::HeadUnavailable {
                attempts, detail, ..
            } => {
                assert_eq!(*attempts, 1);
                assert!(detail.contains("deadline exhausted"), "{detail}");
            }
            other => panic!("expected HeadUnavailable, got {other:?}"),
        }
        assert_eq!(retries, 0, "a spent deadline starts no retry");
    }

    /// The race the backoff exists for: the dispatch arrives before coord has
    /// pushed the ref naming its head. The head lands in origin before the
    /// first retry, and that retry checks it out.
    #[tokio::test]
    async fn head_published_during_backoff_is_picked_up_by_the_retry() {
        let f = fixture();
        // B exists only in a scratch clone until the retry hook pushes it.
        let scratch = f._tmp.path().join("scratch");
        git(
            f._tmp.path(),
            &["clone", "-q", &slash(&f.origin), &slash(&scratch)],
        );
        let b = commit(&scratch, "B");
        let origin = slash(&f.origin);
        let mut retries = 0usize;
        let wt = prepare_with(&f, CANDIDATE, &b, &TEST_POLICY, |_| {
            retries += 1;
            if retries == 1 {
                git(
                    &scratch,
                    &["push", "-q", "-f", &origin, &format!("HEAD:{CANDIDATE}")],
                );
            }
        })
        .await
        .expect("the retry must see the late-published head");
        assert_eq!(retries, 1);
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), b);
    }

    /// Through the PRODUCTION entry point, with its real backoff: a dispatch
    /// already cancelled returns `Cancelled` at once — the fetch races the
    /// token instead of running out its timeout, and no retry pause is slept.
    #[tokio::test]
    async fn cancelled_token_stops_the_real_prepare_worktree_at_once() {
        let f = fixture();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let started = std::time::Instant::now();
        let err = prepare_worktree(
            &f.root,
            REPO,
            "d-1",
            &slash(&f.origin),
            CANDIDATE,
            GHOST,
            &cancel,
        )
        .await
        .expect_err("cancelled");
        assert_eq!(err, CheckoutError::Cancelled);
        assert_eq!(err.result_disposition(), ("cancelled", None));
        assert_eq!(err.log_prefix(), "[ci-node] checkout cancelled:");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "cancellation must not wait out the 30 s backoff, took {:?}",
            started.elapsed()
        );
    }

    /// Payload values that would reach `git fetch`'s argv as options — or that
    /// are simply not what they claim — are refused before any git runs, and
    /// reported as a setup `failure`.
    #[tokio::test]
    async fn option_shaped_or_malformed_payload_is_refused() {
        let f = fixture();
        let url = slash(&f.origin);
        let cases: [(&str, &str, &str); 7] = [
            (url.as_str(), "--upload-pack=touch pwned", GHOST),
            (url.as_str(), "refs/heads/main", GHOST),
            (url.as_str(), "refs/heads/merge-candidate/", GHOST),
            (
                url.as_str(),
                "refs/heads/merge-candidate/1:refs/heads/main",
                GHOST,
            ),
            (url.as_str(), CANDIDATE, "--upload-pack=touch pwned"),
            (url.as_str(), CANDIDATE, "abc123"),
            ("--upload-pack=touch pwned", CANDIDATE, GHOST),
        ];
        for (fetch_url, candidate_ref, head) in cases {
            let err = prepare_worktree_with(
                &f.root,
                REPO,
                "d-1",
                fetch_url,
                candidate_ref,
                head,
                &TEST_POLICY,
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .expect_err("must be refused");
            assert!(
                matches!(&err, CheckoutError::Failed(m) if m.starts_with("refusing dispatch")),
                "({fetch_url:?}, {candidate_ref:?}, {head:?}) gave {err:?}"
            );
            assert_eq!(err.result_disposition(), ("failure", None));
        }
    }

    #[test]
    fn log_prefix_names_what_happened() {
        assert_eq!(
            CheckoutError::Failed("x".into()).log_prefix(),
            "[ci-node] checkout failed:"
        );
        assert_eq!(
            CheckoutError::HeadUnavailable {
                head_sha: GHOST.into(),
                fetch_url: "u".into(),
                attempts: 3,
                detail: "d".into(),
            }
            .log_prefix(),
            "[ci-node] checkout: head unavailable:"
        );
    }

    #[test]
    fn production_policy_stays_inside_the_lease() {
        let lease = Duration::from_secs(15 * 60);
        assert!(HEAD_FETCH_POLICY.deadline < lease);
        let pauses: Duration = HEAD_FETCH_POLICY.backoff.iter().sum();
        assert!(pauses < HEAD_FETCH_POLICY.deadline);
        assert!(HEAD_FETCH_POLICY.retry_fetch_timeout <= Duration::from_secs(120));
    }

    #[test]
    fn other_setup_failures_stay_failures() {
        assert_eq!(
            CheckoutError::Failed("no .git".into()).result_disposition(),
            ("failure", None)
        );
    }
}
