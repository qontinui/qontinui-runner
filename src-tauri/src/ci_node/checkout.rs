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
    use crate::process_helpers::ChildTreeGuard;
    let mut cmd = crate::process_helpers::tokio_no_window("git");
    // A git call raced against cancellation or cut off by a timeout is
    // DROPPED mid-flight, and what must die with it is the whole process
    // TREE, not only `git` itself: a fetch runs `git-remote-https`,
    // `index-pack` and friends as children, and those — not `git` — are what
    // hold the mirror connection and the repo's locks. `kill_on_drop` kills
    // the direct child; the [`ChildTreeGuard`] (a kill-on-close Job Object on
    // Windows, a process group + `killpg` on Unix) kills everything under it
    // when the future is dropped. A call that runs to completion disarms the
    // guard, so nothing a finished git left behind is touched.
    cmd.current_dir(repo_dir)
        .kill_on_drop(true)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    ChildTreeGuard::arm_tokio(&mut cmd);
    let fut = async {
        let child = cmd
            .spawn()
            .map_err(|e| format!("spawn git {}: {e}", args.join(" ")))?;
        let tree = ChildTreeGuard::attach_armed_tokio(&child);
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| format!("wait for git {}: {e}", args.join(" ")))?;
        tree.disarm();
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

/// How hard [`prepare_worktree`] tries to obtain the dispatched head, and how
/// long the whole checkout may take.
///
/// `deadline` bounds the WHOLE checkout: the warm-SHA probe, every fetch, every
/// presence probe, every retry pause, the stale-dir cleanup and the worktree
/// add. Every one of them is clipped to what remains of it and races the
/// dispatch's cancel token. It sits well inside coord's 15-minute dispatch
/// lease, and each retry also pushes a progress line (which is what renews the
/// lease), so a slow or missing head ends as a reported result, never as a
/// `lost` row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeadFetchPolicy<'a> {
    /// Pauses between attempts; attempts = `backoff.len() + 1`.
    pub backoff: &'a [Duration],
    /// Per-fetch timeout on the first attempt (a cold repo can be slow).
    pub first_fetch_timeout: Duration,
    /// Per-fetch timeout on a retry — only the few objects on top of what the
    /// first attempt already brought in are missing by then.
    pub retry_fetch_timeout: Duration,
    /// Wall-clock bound on the whole checkout (see the type doc).
    pub deadline: Duration,
}

/// Three attempts, 30 s + 60 s apart, inside a 10-minute checkout deadline:
/// absorbs a publish-before-push race from a coord build that dispatches a tip
/// before the ref naming it has reached the mirror, and still leaves a third of
/// the 15-minute lease unused.
pub(crate) const HEAD_FETCH_POLICY: HeadFetchPolicy<'static> = HeadFetchPolicy {
    backoff: &[Duration::from_secs(30), Duration::from_secs(60)],
    first_fetch_timeout: GIT_FETCH_TIMEOUT,
    retry_fetch_timeout: Duration::from_secs(120),
    deadline: Duration::from_secs(600),
};

/// `summary.reason` for a dispatch whose commit the mirror says it does not
/// have. Coord's result route admits only `success|failure|cancelled`, so a
/// missing head is `cancelled` + this reason — a non-verdict — rather than a
/// new state.
pub(crate) const HEAD_UNAVAILABLE_REASON: &str = "head_sha_unavailable";

/// `summary.reason` for a fetch that failed for any reason OTHER than the
/// mirror saying the commit is not there (DNS, auth, TLS, disk, a corrupt
/// checkout, a fetch the deadline cut off). Reported as `cancelled` too: coord
/// reads the conclusion, not the reason, and `cancelled` is its only
/// non-verdict — a transient mirror fault must not become a red the candidate
/// never earned. The distinct reason is what keeps the two apart for a reader.
pub(crate) const FETCH_FAILED_REASON: &str = "fetch_failed";

/// Candidate-ref namespaces coord dispatches: the merge-candidate branch, and
/// the per-dispatch ref (plan Phase 2).
const CANDIDATE_REF_PREFIXES: [&str; 2] = ["refs/heads/merge-candidate/", "refs/ci-dispatch/"];

/// Transports a dispatch may name. Anything else — notably git's
/// `<helper>::<address>` remote-helper syntax, `ext::` above all, which runs a
/// command — is refused. A plain absolute path is the local-mirror form.
const FETCH_URL_SCHEMES: [&str; 3] = ["https://", "http://", "file://"];

/// Config for every fetch: a fetch killed by cancellation or the deadline must
/// not leave an auto-gc, maintenance or commit-graph writer (and its lock)
/// behind in the SHARED primary checkout; and an HTTP transfer that stalls
/// below 1 byte/s for 60 s aborts on its own — a backstop under the tree kill,
/// for a mirror that accepts the connection and then goes silent.
const FETCH_CONFIG: [&str; 10] = [
    "-c",
    "gc.auto=0",
    "-c",
    "maintenance.auto=false",
    "-c",
    "fetch.writeCommitGraph=false",
    "-c",
    "http.lowSpeedLimit=1",
    "-c",
    "http.lowSpeedTime=60",
];

/// git stderr that means "the remote answered, and it does not have that" —
/// the only fetch errors that are evidence of an unavailable head.
const MISSING_ON_REMOTE: [&str; 3] = [
    "not our ref",
    "couldn't find remote ref",
    "no such remote ref",
];

/// Why [`prepare_worktree`] produced no tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckoutError {
    /// The mirror says the dispatched commit is not there: on the last attempt
    /// the checkout completed, the bare-SHA fetch either succeeded without
    /// bringing the commit or was refused with a missing-ref error. This says
    /// nothing about the code under test — the candidate was never published
    /// or was replaced — so it must never be reported as a test `failure`
    /// (that poisons shadow parity).
    HeadUnavailable {
        head_sha: String,
        fetch_url: String,
        attempts: usize,
        detail: String,
    },
    /// The fetch failed for some other reason. Reported as `cancelled` +
    /// `fetch_failed`: like a missing head it says nothing about the code
    /// under test, so it must not be a red verdict, and the reason keeps it
    /// distinguishable from a missing head.
    FetchFailed(String),
    /// The dispatch was cancelled while the checkout was running.
    Cancelled,
    /// Any other setup failure (an invalid payload, no primary checkout, the
    /// deadline spent on a local step, worktree add failed…).
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
                "head_sha {head_sha} not available from {fetch_url} after {attempts} attempt(s): {detail}"
            ),
            CheckoutError::FetchFailed(e) => f.write_str(e),
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
            CheckoutError::FetchFailed(_) => ("cancelled", Some(FETCH_FAILED_REASON)),
            CheckoutError::Cancelled => ("cancelled", None),
            CheckoutError::Failed(_) => ("failure", None),
        }
    }

    /// The progress-log line prefix: a cancelled or unavailable checkout is
    /// not a "checkout failed", and the log must not say it is.
    pub(crate) fn log_prefix(&self) -> &'static str {
        match self {
            CheckoutError::HeadUnavailable { .. } => "[ci-node] checkout: head unavailable:",
            CheckoutError::FetchFailed(_) => "[ci-node] checkout: fetch failed:",
            CheckoutError::Cancelled => "[ci-node] checkout cancelled:",
            CheckoutError::Failed(_) => "[ci-node] checkout failed:",
        }
    }
}

/// Refuse a dispatch payload whose git arguments could be anything but what
/// they claim to be. They come off the wire and end up on a `git fetch` argv,
/// where a value such as `--upload-pack=<cmd>` would EXECUTE — git parses
/// options after positionals — and a `<helper>::` URL would run a remote
/// helper. The `--` before every fetch's positionals is the second,
/// independent half of this guard.
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
    let plain = !fetch_url.is_empty()
        && !fetch_url.starts_with('-')
        && !fetch_url.contains("::")
        && !fetch_url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control());
    // A local mirror must be LOCAL: `file:///…` (empty host) or an absolute
    // path on a local volume. A UNC path (`\\host\share`, `//host/share`), a
    // verbatim `\\?\` path or `file://host/…` would make this device reach
    // out to another machine over SMB on a payload's say-so.
    let remote_path = fetch_url.starts_with("\\\\") || fetch_url.starts_with("//");
    let transport_ok = if let Some(rest) = fetch_url.strip_prefix("file://") {
        rest.starts_with('/') && !rest.starts_with("//")
    } else {
        FETCH_URL_SCHEMES.iter().any(|s| fetch_url.starts_with(s))
            || (!remote_path && Path::new(fetch_url).is_absolute())
    };
    if !(plain && transport_ok) {
        return Err(format!(
            "refusing dispatch: fetch_url {fetch_url:?} is not an {} URL or an absolute path",
            FETCH_URL_SCHEMES.join(" / ")
        ));
    }
    Ok(())
}

/// Why a bounded step stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Cancelled,
    Deadline,
}

/// One checkout's bounds. Every git call and every pause goes through here, so
/// each races `cancel` and is clipped to the deadline; a git call dropped by
/// either has its whole process tree killed (see [`run_git`]), never orphaned.
struct Bounds<'a> {
    /// Behind a lock only so a test can move it (see [`Self::set_deadline`]);
    /// production sets it once.
    deadline: std::sync::Mutex<tokio::time::Instant>,
    cancel: &'a CancellationToken,
}

impl<'a> Bounds<'a> {
    fn new(deadline: tokio::time::Instant, cancel: &'a CancellationToken) -> Self {
        Self {
            deadline: std::sync::Mutex::new(deadline),
            cancel,
        }
    }

    fn deadline(&self) -> tokio::time::Instant {
        match self.deadline.lock() {
            Ok(d) => *d,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    /// Move the deadline — the test seam that lets a test spend the deadline
    /// at an exact point instead of sleeping until it runs out.
    #[cfg(test)]
    fn set_deadline(&self, to: tokio::time::Instant) {
        match self.deadline.lock() {
            Ok(mut d) => *d = to,
            Err(poisoned) => *poisoned.into_inner() = to,
        }
    }

    async fn run<T>(&self, fut: impl std::future::Future<Output = T>) -> Result<T, Stop> {
        if self.cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        let deadline = self.deadline();
        if tokio::time::Instant::now() >= deadline {
            return Err(Stop::Deadline);
        }
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(Stop::Cancelled),
            r = tokio::time::timeout_at(deadline, fut) => r.map_err(|_| Stop::Deadline),
        }
    }

    async fn git(
        &self,
        dir: &Path,
        args: &[&str],
        timeout: Duration,
    ) -> Result<Result<String, String>, Stop> {
        self.run(run_git(dir, args, timeout)).await
    }

    /// `git cat-file -e <sha>^{commit}` — is the commit in the local store?
    async fn head_present(&self, dir: &Path, head_sha: &str) -> Result<bool, Stop> {
        let spec = format!("{head_sha}^{{commit}}");
        Ok(self
            .git(dir, &["cat-file", "-e", &spec], GIT_LOCAL_TIMEOUT)
            .await?
            .is_ok())
    }

    async fn pause(&self, d: Duration) -> Result<(), Stop> {
        self.run(tokio::time::sleep(d)).await
    }

    fn remaining(&self) -> Duration {
        self.deadline()
            .saturating_duration_since(tokio::time::Instant::now())
    }
}

/// `git -c … fetch -- <url> <what>`.
fn fetch_args<'a>(fetch_url: &'a str, what: &'a str) -> Vec<&'a str> {
    let mut args: Vec<&str> = FETCH_CONFIG.to_vec();
    args.extend(["fetch", "--", fetch_url, what]);
    args
}

/// A local step's `Stop` as the checkout's error.
fn local_stop(stop: Stop, step: &str, policy: &HeadFetchPolicy<'_>) -> CheckoutError {
    match stop {
        Stop::Cancelled => CheckoutError::Cancelled,
        Stop::Deadline => CheckoutError::Failed(format!(
            "checkout deadline ({}s) exhausted during {step}",
            policy.deadline.as_secs()
        )),
    }
}

/// What one completed attempt established about the head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Evidence {
    /// The mirror answered and does not have the commit.
    Missing,
    /// The fetch failed for some other reason.
    Broken,
}

fn says_missing(git_error: &str) -> bool {
    MISSING_ON_REMOTE.iter().any(|m| git_error.contains(m))
}

/// Make `head_sha` present in `repo_dir`, or say precisely why it is not.
///
/// Each attempt fetches `candidate_ref` and then PROBES for the commit — a
/// successful ref fetch proves only that the ref exists, not that it names the
/// dispatched commit (coord may still be serving the previous cut). When the
/// probe misses, whether the ref fetch errored or succeeded, a bare-SHA fetch
/// follows: the coord mirror honours want-by-SHA for any commit ever pushed,
/// so that fetch's outcome is the attempt's evidence ([`Evidence`]).
///
/// After each failed attempt one progress line goes to `progress` (it reaches
/// coord, and renews the lease): either the retry and its delay, or — when the
/// deadline leaves no room for the full pause — that the deadline is reached,
/// and no further attempt is made. `after_attempt(attempt, bounds)` runs after
/// each failed attempt, before that line (a test seam; production passes a
/// no-op). The verdict is the evidence of the last attempt that COMPLETED — an
/// attempt the deadline cut off establishes nothing.
#[allow(clippy::too_many_arguments)]
async fn fetch_head(
    repo_dir: &Path,
    fetch_url: &str,
    candidate_ref: &str,
    head_sha: &str,
    policy: &HeadFetchPolicy<'_>,
    bounds: &Bounds<'_>,
    progress: &mut (dyn FnMut(&str) + Send),
    after_attempt: &mut (dyn FnMut(usize, &Bounds<'_>) + Send),
) -> Result<(), CheckoutError> {
    let max_attempts = policy.backoff.len() + 1;
    let mut attempts = 0usize;
    let mut verdict: Option<(Evidence, String)> = None;
    let mut cut_off: Option<String> = None;

    'attempts: for attempt in 1..=max_attempts {
        let per_fetch = if attempt == 1 {
            policy.first_fetch_timeout
        } else {
            after_attempt(attempt - 1, bounds);
            let backoff = policy.backoff[attempt - 2];
            let remaining = bounds.remaining();
            let what = match verdict.as_ref().map(|(e, _)| *e) {
                Some(Evidence::Missing) => "not yet available",
                _ => "fetch failed",
            };
            let detail = verdict.as_ref().map(|(_, d)| d.as_str()).unwrap_or("");
            if remaining <= backoff {
                // The pause would run into the deadline, so no retry can
                // start after it: say so, and stop here rather than sleep to
                // the deadline announcing a retry that will never run.
                progress(&format!(
                    "[ci-node] head {head_sha} {what} (attempt {}/{max_attempts}: {detail}); \
                     checkout deadline reached, no further retry",
                    attempt - 1
                ));
                break;
            }
            progress(&format!(
                "[ci-node] head {head_sha} {what} (attempt {}/{max_attempts}: {detail}); \
                 retry {attempt}/{max_attempts} in {}s",
                attempt - 1,
                backoff.as_secs()
            ));
            match bounds.pause(backoff).await {
                Err(Stop::Cancelled) => return Err(CheckoutError::Cancelled),
                Err(Stop::Deadline) => break,
                Ok(()) => {}
            }
            // The pause may have spent the last of the deadline; starting an
            // attempt now would only replace the real error with a
            // "deadline" one and inflate the attempt count.
            if bounds.remaining().is_zero() {
                break;
            }
            policy.retry_fetch_timeout
        };
        attempts = attempt;

        let mut notes: Vec<String> = Vec::new();
        match bounds
            .git(repo_dir, &fetch_args(fetch_url, candidate_ref), per_fetch)
            .await
        {
            Err(Stop::Cancelled) => return Err(CheckoutError::Cancelled),
            Err(Stop::Deadline) => {
                cut_off = Some(format!(
                    "attempt {attempt}: ref fetch cut off by the deadline"
                ));
                break 'attempts;
            }
            Ok(Ok(_)) => match bounds.head_present(repo_dir, head_sha).await {
                Err(Stop::Cancelled) => return Err(CheckoutError::Cancelled),
                Err(Stop::Deadline) => {
                    cut_off = Some(format!("attempt {attempt}: probe cut off by the deadline"));
                    break 'attempts;
                }
                Ok(true) => return Ok(()),
                Ok(false) => notes.push(format!(
                    "{candidate_ref} fetched but does not contain the head"
                )),
            },
            Ok(Err(e)) => notes.push(format!("ref fetch: {e}")),
        }

        let evidence = match bounds
            .git(repo_dir, &fetch_args(fetch_url, head_sha), per_fetch)
            .await
        {
            Err(Stop::Cancelled) => return Err(CheckoutError::Cancelled),
            Err(Stop::Deadline) => {
                cut_off = Some(format!(
                    "attempt {attempt}: bare-SHA fetch cut off by the deadline"
                ));
                break 'attempts;
            }
            Ok(Ok(_)) => match bounds.head_present(repo_dir, head_sha).await {
                Err(Stop::Cancelled) => return Err(CheckoutError::Cancelled),
                Err(Stop::Deadline) => {
                    cut_off = Some(format!("attempt {attempt}: probe cut off by the deadline"));
                    break 'attempts;
                }
                Ok(true) => return Ok(()),
                Ok(false) => {
                    notes.push("bare-SHA fetch succeeded but the head is absent".to_string());
                    Evidence::Missing
                }
            },
            Ok(Err(e)) => {
                let evidence = if says_missing(&e) {
                    Evidence::Missing
                } else {
                    Evidence::Broken
                };
                notes.push(format!("bare-SHA fetch: {e}"));
                evidence
            }
        };
        let detail = notes.join("; ");
        warn!("ci_node: {head_sha} not obtained (attempt {attempt}/{max_attempts}, {evidence:?}): {detail}");
        verdict = Some((evidence, detail));
    }

    let suffix = cut_off.map(|c| format!("; then {c}")).unwrap_or_default();
    match verdict {
        Some((Evidence::Missing, detail)) => Err(CheckoutError::HeadUnavailable {
            head_sha: head_sha.to_string(),
            fetch_url: fetch_url.to_string(),
            attempts,
            detail: format!("{detail}{suffix}"),
        }),
        Some((Evidence::Broken, detail)) => Err(CheckoutError::FetchFailed(format!(
            "fetching {head_sha} from {fetch_url} failed after {attempts} attempt(s): {detail}{suffix}"
        ))),
        None => Err(CheckoutError::FetchFailed(format!(
            "fetching {head_sha} from {fetch_url}: no attempt completed within the {}s checkout \
             deadline{suffix}",
            policy.deadline.as_secs()
        ))),
    }
}

/// Validate + fetch + verify + worktree-add. Returns the worktree path.
///
/// `cancel` interrupts every git call and every retry pause (a cancelled git
/// process tree is killed), and the whole call is bounded by
/// [`HEAD_FETCH_POLICY`]'s deadline. `progress` receives one line per retry.
pub(crate) async fn prepare_worktree(
    root: &Path,
    repo: &str,
    dispatch_id: &str,
    fetch_url: &str,
    candidate_ref: &str,
    head_sha: &str,
    cancel: &CancellationToken,
    progress: &mut (dyn FnMut(&str) + Send),
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
        progress,
        &mut |_, _| {},
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
    progress: &mut (dyn FnMut(&str) + Send),
    after_attempt: &mut (dyn FnMut(usize, &Bounds<'_>) + Send),
) -> Result<PathBuf, CheckoutError> {
    validate_fetch_args(fetch_url, candidate_ref, head_sha)?;
    let bounds = Bounds::new(tokio::time::Instant::now() + policy.deadline, cancel);
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
    let warm = bounds
        .head_present(&repo_dir, head_sha)
        .await
        .map_err(|s| local_stop(s, "the warm-SHA probe", policy))?;
    if warm {
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
            &bounds,
            progress,
            after_attempt,
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
        bounds
            .run(cleanup_dispatch(root, repo, dispatch_id))
            .await
            .map_err(|s| local_stop(s, "stale dispatch-dir cleanup", policy))?;
    }
    std::fs::create_dir_all(&dispatch_root)
        .map_err(|e| format!("create {}: {e}", dispatch_root.display()))?;

    let wt_str = wt_path.to_string_lossy().to_string();
    bounds
        .git(
            &repo_dir,
            &["worktree", "add", "--detach", &wt_str, head_sha],
            GIT_LOCAL_TIMEOUT,
        )
        .await
        .map_err(|s| local_stop(s, "worktree add", policy))??;
    info!(
        "ci_node: worktree ready at {} for {repo}@{head_sha}",
        wt_path.display()
    );
    Ok(wt_path)
}

/// Always-run cleanup: `git worktree remove -f -f` + prune, then a
/// best-effort delete of the whole dispatch root — which sweeps the worktree,
/// every provisioned sibling, and anything git left behind, in one call.
/// Failure is logged, never propagated (cleanup runs on failure paths too).
///
/// `-f` twice because a `worktree add` killed mid-way (cancel, deadline)
/// leaves its admin entry LOCKED ("initializing"), and a single `--force`
/// refuses a locked worktree. The directory delete runs on the blocking pool,
/// so a caller that bounds this future (the checkout's stale-dir cleanup)
/// stops waiting at its deadline; the delete itself then finishes in the
/// background, which is harmless — it only ever removes this dispatch's own
/// root.
pub(crate) async fn cleanup_dispatch(root: &Path, repo: &str, dispatch_id: &str) {
    let repo_dir = root.join(crate::agent_runtime::local_repo_name(repo));
    let wt_path = ci_worktree_path(root, dispatch_id, repo);
    let dispatch_root = ci_dispatch_root(root, dispatch_id);
    let wt_str = wt_path.to_string_lossy().to_string();
    if let Err(e) = run_git(
        &repo_dir,
        &["worktree", "remove", "-f", "-f", &wt_str],
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
        let target = dispatch_root.clone();
        match tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&target)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!(
                "ci_node: residual dispatch dir {} not removable: {e}",
                dispatch_root.display()
            ),
            Err(e) => warn!(
                "ci_node: delete of dispatch dir {} did not complete: {e}",
                dispatch_root.display()
            ),
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
        cancel: &CancellationToken,
        progress: &mut (dyn FnMut(&str) + Send),
        after_attempt: &mut (dyn FnMut(usize, &Bounds<'_>) + Send),
    ) -> Result<PathBuf, CheckoutError> {
        prepare_worktree_with(
            &f.root,
            REPO,
            "d-1",
            &slash(&f.origin),
            candidate_ref,
            head,
            policy,
            cancel,
            progress,
            after_attempt,
        )
        .await
    }

    async fn prepare(
        f: &Fixture,
        candidate_ref: &str,
        head: &str,
    ) -> Result<PathBuf, CheckoutError> {
        prepare_with(
            f,
            candidate_ref,
            head,
            &TEST_POLICY,
            &CancellationToken::new(),
            &mut |_| {},
            &mut |_, _| {},
        )
        .await
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

    /// Arm 3: the mirror answers and has no such commit. Every attempt runs,
    /// each retry is announced on the progress stream, and the result is the
    /// typed non-verdict — reported `cancelled` + `head_sha_unavailable`,
    /// never `failure`.
    #[tokio::test]
    async fn head_the_mirror_does_not_have_is_head_unavailable() {
        let f = fixture();
        let mut failed_attempts = 0usize;
        let mut lines: Vec<String> = Vec::new();
        let err = prepare_with(
            &f,
            CANDIDATE,
            GHOST,
            &TEST_POLICY,
            &CancellationToken::new(),
            &mut |l| lines.push(l.to_string()),
            &mut |_, _| failed_attempts += 1,
        )
        .await
        .expect_err("a ghost head cannot be checked out");
        match &err {
            CheckoutError::HeadUnavailable {
                attempts,
                head_sha,
                detail,
                ..
            } => {
                assert_eq!(*attempts, 3);
                assert_eq!(head_sha, GHOST);
                assert!(detail.contains("not our ref"), "{detail}");
            }
            other => panic!("expected HeadUnavailable, got {other:?}"),
        }
        assert_eq!(
            failed_attempts, 2,
            "the hook runs before each of the two retries"
        );
        assert_eq!(lines.len(), 2, "one progress line per retry: {lines:?}");
        assert!(lines[0].contains("not yet available") && lines[0].contains("retry 2/3"));
        assert_eq!(
            err.result_disposition(),
            ("cancelled", Some(HEAD_UNAVAILABLE_REASON))
        );
        assert!(
            !ci_dispatch_root(&f.root, "d-1").exists(),
            "no dispatch dir is created for an unavailable head"
        );
    }

    /// The other arm of the taxonomy: a fetch that fails for a reason other
    /// than "the mirror has no such commit" (here, no repository at the URL)
    /// is `fetch_failed` — distinct from a missing head, and like it a
    /// non-verdict (`cancelled`), never a red build.
    #[tokio::test]
    async fn a_fetch_that_fails_for_another_reason_is_fetch_failed() {
        let f = fixture();
        let nowhere = slash(&f._tmp.path().join("no-such-mirror"));
        let err = prepare_worktree_with(
            &f.root,
            REPO,
            "d-1",
            &nowhere,
            CANDIDATE,
            &f.a,
            &TEST_POLICY,
            &CancellationToken::new(),
            &mut |_| {},
            &mut |_, _| {},
        )
        .await
        .expect_err("there is no mirror to fetch from");
        assert!(
            matches!(&err, CheckoutError::FetchFailed(m) if m.contains("after 3 attempt(s)")),
            "{err:?}"
        );
        assert_eq!(
            err.result_disposition(),
            ("cancelled", Some(FETCH_FAILED_REASON))
        );
        assert_eq!(err.log_prefix(), "[ci-node] checkout: fetch failed:");
    }

    #[test]
    fn only_missing_ref_errors_are_evidence_of_a_missing_head() {
        assert!(says_missing(
            "git fetch failed (exit code: 128): fatal: remote error: upload-pack: not our ref 0123"
        ));
        assert!(says_missing(
            "fatal: couldn't find remote ref refs/heads/merge-candidate/1"
        ));
        assert!(says_missing("fatal: no such remote ref 0123"));
        assert!(!says_missing(
            "fatal: unable to access 'https://x/': Could not resolve host: x"
        ));
        assert!(!says_missing(
            "fatal: Authentication failed for 'https://x/'"
        ));
        assert!(!says_missing(
            "error: unable to write file: No space left on device"
        ));
    }

    /// A deadline spent before anything runs stops the checkout before any
    /// git process is spawned, as a setup failure naming the step.
    #[tokio::test]
    async fn spent_deadline_stops_before_any_git_runs() {
        let f = fixture();
        let policy = HeadFetchPolicy {
            deadline: Duration::ZERO,
            ..TEST_POLICY
        };
        let err = prepare_with(
            &f,
            CANDIDATE,
            GHOST,
            &policy,
            &CancellationToken::new(),
            &mut |_| {},
            &mut |_, _| {},
        )
        .await
        .expect_err("nothing can run with no time left");
        assert!(
            matches!(&err, CheckoutError::Failed(m) if m.contains("deadline") && m.contains("warm-SHA")),
            "{err:?}"
        );
    }

    /// A deadline that leaves no room for the next pause ends the loop right
    /// there: the progress line says the deadline is reached (it does not
    /// announce a retry that will never run), the verdict is attempt 1's real
    /// git error, and the attempt count is not inflated. The deadline is moved
    /// through the `Bounds` seam after attempt 1, so nothing sleeps and
    /// nothing depends on how fast this box runs git.
    #[tokio::test]
    async fn deadline_reached_before_a_retry_keeps_the_last_real_error() {
        let f = fixture();
        let policy = HeadFetchPolicy {
            backoff: &[Duration::from_secs(600)],
            deadline: Duration::from_secs(1800),
            ..TEST_POLICY
        };
        let mut lines: Vec<String> = Vec::new();
        let err = prepare_with(
            &f,
            CANDIDATE,
            GHOST,
            &policy,
            &CancellationToken::new(),
            &mut |l| lines.push(l.to_string()),
            &mut |_, bounds| {
                // Less than the 600 s pause, so no retry fits.
                bounds.set_deadline(tokio::time::Instant::now() + Duration::from_secs(60))
            },
        )
        .await
        .expect_err("a ghost head cannot be checked out");
        match &err {
            CheckoutError::HeadUnavailable {
                attempts, detail, ..
            } => {
                assert_eq!(*attempts, 1);
                assert!(detail.contains("not our ref"), "{detail}");
            }
            other => panic!("expected HeadUnavailable, got {other:?}"),
        }
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("deadline reached") && !lines[0].contains("retry 2"),
            "{lines:?}"
        );
    }

    /// The race the backoff exists for: the dispatch arrives before coord has
    /// pushed the ref naming its head. The head lands in origin after the
    /// first attempt, and the retry checks it out.
    #[tokio::test]
    async fn head_published_during_backoff_is_picked_up_by_the_retry() {
        let f = fixture();
        // B exists only in a scratch clone until the hook pushes it.
        let scratch = f._tmp.path().join("scratch");
        git(
            f._tmp.path(),
            &["clone", "-q", &slash(&f.origin), &slash(&scratch)],
        );
        let b = commit(&scratch, "B");
        let origin = slash(&f.origin);
        let mut failed_attempts = 0usize;
        let wt = prepare_with(
            &f,
            CANDIDATE,
            &b,
            &TEST_POLICY,
            &CancellationToken::new(),
            &mut |_| {},
            &mut |_, _| {
                failed_attempts += 1;
                if failed_attempts == 1 {
                    git(
                        &scratch,
                        &["push", "-q", "-f", &origin, &format!("HEAD:{CANDIDATE}")],
                    );
                }
            },
        )
        .await
        .expect("the retry must see the late-published head");
        assert_eq!(failed_attempts, 1);
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), b);
    }

    /// Cancellation DURING a real (non-zero) retry pause: the retry is
    /// announced, a separate task cancels ~200 ms later — by which time the
    /// checkout is parked in the 600 s pause — and the pause's cancel branch
    /// returns `Cancelled` at once.
    #[tokio::test]
    async fn cancel_during_backoff_is_cancelled() {
        let f = fixture();
        let policy = HeadFetchPolicy {
            backoff: &[Duration::from_secs(600), Duration::from_secs(600)],
            deadline: Duration::from_secs(1800),
            ..TEST_POLICY
        };
        let cancel = CancellationToken::new();
        let announced = std::sync::Arc::new(tokio::sync::Notify::new());
        let canceller = {
            let (cancel, announced) = (cancel.clone(), announced.clone());
            tokio::spawn(async move {
                announced.notified().await;
                tokio::time::sleep(Duration::from_millis(200)).await;
                cancel.cancel();
            })
        };
        let signal = announced.clone();
        let mut lines: Vec<String> = Vec::new();
        let started = std::time::Instant::now();
        let err = prepare_with(
            &f,
            CANDIDATE,
            GHOST,
            &policy,
            &cancel,
            &mut |l| {
                lines.push(l.to_string());
                signal.notify_one();
            },
            &mut |_, _| {},
        )
        .await
        .expect_err("cancelled");
        assert_eq!(err, CheckoutError::Cancelled);
        assert_eq!(lines.len(), 1, "exactly one retry was announced: {lines:?}");
        assert!(lines[0].contains("retry 2/3 in 600s"), "{lines:?}");
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "cancellation must cut the 600 s pause short, took {:?}",
            started.elapsed()
        );
        canceller.await.unwrap();
    }

    /// Cancellation of a fetch that is IN FLIGHT, and what it must kill. The
    /// mirror is a local TCP listener that accepts the connection and never
    /// answers, so the fetch would hang. The token is cancelled the moment the
    /// connection arrives. Asserted:
    ///
    /// - the checkout returns `Cancelled` at once;
    /// - the MIRROR CONNECTION CLOSES. That socket is held by git's transport
    ///   helper (`git-remote-http`), a CHILD of `git` — so its close is the
    ///   portable, observable proof that the whole process tree was killed,
    ///   not just `git`. With `kill_on_drop` alone the helper survives and the
    ///   connection stays open until `http.lowSpeedTime` (60 s) — longer than
    ///   this test waits.
    #[tokio::test]
    async fn cancel_during_a_hung_fetch_kills_the_whole_fetch_tree() {
        use tokio::io::AsyncReadExt;
        let f = fixture();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mirror.git", listener.local_addr().unwrap());
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            trigger.cancel();
            // Drain the request, then wait for the peer to go away.
            let closed = tokio::time::timeout(Duration::from_secs(20), async {
                let mut buf = [0u8; 4096];
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            })
            .await
            .map_err(|_| "the mirror connection outlived the cancel by 20 s".to_string());
            let _ = closed_tx.send(closed);
        });
        let started = std::time::Instant::now();
        let err = prepare_worktree_with(
            &f.root,
            REPO,
            "d-1",
            &url,
            CANDIDATE,
            GHOST,
            &TEST_POLICY,
            &cancel,
            &mut |_| {},
            &mut |_, _| {},
        )
        .await
        .expect_err("cancelled");
        assert_eq!(err, CheckoutError::Cancelled);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "a hung fetch must not be waited out, took {:?}",
            started.elapsed()
        );
        let closed = closed_rx.await.expect("the listener saw the connection");
        assert_eq!(
            closed,
            Ok(()),
            "the fetch's transport helper must be killed"
        );
        server.await.unwrap();
    }

    /// Through the PRODUCTION entry point: a dispatch already cancelled
    /// returns `Cancelled` before any git runs.
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
            &mut |_| {},
        )
        .await
        .expect_err("cancelled");
        assert_eq!(err, CheckoutError::Cancelled);
        assert_eq!(err.result_disposition(), ("cancelled", None));
        assert_eq!(err.log_prefix(), "[ci-node] checkout cancelled:");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    /// Payload values that would reach `git fetch`'s argv as options, name a
    /// remote helper, or are simply not what they claim, are refused before
    /// any git runs, and reported as a setup `failure`.
    #[tokio::test]
    async fn option_shaped_or_malformed_payload_is_refused() {
        let f = fixture();
        let url = slash(&f.origin);
        let cases: [(&str, &str, &str); 16] = [
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
            ("ext::sh -c touch% pwned", CANDIDATE, GHOST),
            ("ext::sh", CANDIDATE, GHOST),
            ("ssh://host/repo.git", CANDIDATE, GHOST),
            ("relative/path", CANDIDATE, GHOST),
            ("\\\\server\\share\\mirror.git", CANDIDATE, GHOST),
            ("//server/share/mirror.git", CANDIDATE, GHOST),
            ("\\\\?\\C:\\mirror.git", CANDIDATE, GHOST),
            ("file://server/share/mirror.git", CANDIDATE, GHOST),
            ("file:////server/share/mirror.git", CANDIDATE, GHOST),
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
                &mut |_| {},
                &mut |_, _| {},
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
    fn accepted_fetch_url_forms() {
        for ok in [
            "https://coord.qontinui.io/git/qontinui/qontinui-runner.git",
            "http://127.0.0.1:9/mirror.git",
            "file:///srv/mirror.git",
            "file:///C:/mirror.git",
        ] {
            assert!(validate_fetch_args(ok, CANDIDATE, GHOST).is_ok(), "{ok}");
        }
    }

    #[test]
    fn every_fetch_disables_background_writers() {
        let args = fetch_args("https://m/r.git", CANDIDATE);
        let joined = args.join(" ");
        for c in [
            "gc.auto=0",
            "maintenance.auto=false",
            "fetch.writeCommitGraph=false",
        ] {
            assert!(joined.contains(c), "{joined}");
        }
        for c in ["http.lowSpeedLimit=1", "http.lowSpeedTime=60"] {
            assert!(joined.contains(c), "{joined}");
        }
        let fetch_at = args.iter().position(|a| *a == "fetch").unwrap();
        assert_eq!(
            &args[fetch_at..],
            ["fetch", "--", "https://m/r.git", CANDIDATE]
        );
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
