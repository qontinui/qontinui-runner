//! Runner-local per-session PR attribution + status reconciler.
//!
//! Feeds `project.session_prs`, which the Terminal zone-header dropdown reads
//! (`commands::session_info::session_info_get`). Ground truth for attribution:
//! every commit a session makes carries a `Session-Id: <claude_session_id>`
//! git trailer (installed machine-wide via a `prepare-commit-msg` hook). So
//! "which PRs did session S open" = PRs whose head-branch HEAD commit carries
//! `Session-Id: S`.
//!
//! Each tick (~30s, plus a best-effort pass at startup and an on-demand nudge
//! when the dropdown opens — see [`nudge_session`]):
//!
//! 1. Enumerate open terminal-session records ([`SessionLifecycleStore::open_records`]) —
//!    each carries the `claude_session_id` and the session's `working_dir`.
//! 2. Resolve the session's repo SET ([`resolve_repo_set`]) — the git toplevel
//!    of its cwd when that resolves, PLUS every depth-1 child directory of the
//!    cwd that is itself a repo — then each repo → `owner/name` from `origin`.
//!    A session whose cwd is the WORKSPACE PARENT (`D:\qontinui-root`, which
//!    holds the clones but is not itself a repo) used to resolve to nothing and
//!    was silently dropped from every tick; a session that commits to several
//!    sibling repos used to be attributed only PRs from one of them.
//! 3. In each repo, read every local AND `origin/*` branch's HEAD-commit
//!    `Session-Id` trailer(s) in ONE `git for-each-ref` and keep the branches
//!    whose trailer names this session.
//! 4. Resolve each matching branch → its PR(s) via the GitHub API
//!    (`GitHubClient::list_prs_for_head`, `state=all`).
//! 5. VERIFY the PR's head-commit `Session-Id` trailer == this session (a
//!    branch touched by multiple sessions carries only the LAST session's
//!    trailer on its HEAD; this disambiguates), then upsert the PR — the
//!    upsert also lands fresh open/merged status.
//! 6. STATUS (Phase 3): for any already-stored PR of this session whose local
//!    branch is gone (so step 4 didn't refresh it this tick), refresh
//!    open/merged via `GitHubClient::get_pr` and `update_session_pr_status`.
//!    Folded into this same tick — NOT a second GitHub poll loop; scoped to
//!    the session's OWN PRs (never a fleet scan).
//!
//! LANDED IS NOT GITHUB'S `merged` BOOLEAN. coord fast-forward lands are the
//! majority of this fleet's landings and leave `merged=false` / `state=closed`
//! (`knowledge-base/qontinui-specific/coord-ff-lands.md`), so both write paths
//! run the [`land_verdict`] cascade instead: GitHub merge → local content
//! proof (`ff-land`) → coord's own `coord:landed` chip on a CLOSED PR
//! (`coord-label`) →
//! otherwise one of TWO distinct terminal states, `not-landed` (evaluated) and
//! `land-unknown` (could not be evaluated, always with a reason). The cascade
//! never fetches — see [`probe_land`].
//!
//! AND A FAILED ANCESTRY TEST IS NOT A LAND VERDICT. coord's majority land
//! shape rebases the PR's commits and pushes the REPLAYED shas, so the PR's
//! own head sha is not on `origin/<base>` after an ordinary land and the
//! content proof cannot pass. Until 2026-08-26 that failure was recorded as a
//! confident `not-landed`, which the Terminal dropdown rendered as
//! `closed, not landed` on PRs that had shipped — the precise thing
//! `coord-ff-lands.md` says never to do with ancestry. It is now
//! `land-unknown` / `rebase_land_or_abandoned`, and the `coord:landed` chip
//! is what turns the common case back into a positive.
//!
//! Best-effort throughout: PG unavailable, no GitHub token, a git/gh failure,
//! or an API error skips the affected unit and logs — a passive dropdown never
//! justifies noisy failures.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::database::pg::session_pr_ops::SessionPrStatus;
use crate::database::pg::PgDb;
use crate::session::session_lifecycle_store::SessionLifecycleStore;
use crate::trigger_system::github_api::{has_coord_landed_label, GitHubClient};

/// Reconcile cadence. Matches the "similar poller" cadence in the crate
/// (min-10s pollers, 45s lifecycle poll) — 30s is responsive enough for a
/// passive status indicator without hammering the GitHub API.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Wall-clock budget for one LOCAL git query — `rev-parse`, `remote get-url`,
/// `for-each-ref`, `log -1`, `merge-base --is-ancestor`.
///
/// Every one of these reads local refs/objects with no network and no lock this
/// process holds, so on a healthy checkout they return in single-digit
/// milliseconds. 15s is three orders of magnitude of headroom (a cold page
/// cache over a very large object store, a filesystem waking a spun-down disk,
/// an antivirus scanning the pack files) while still bounding the damage: git
/// can no longer wedge a tick forever, only cost it 15s per stuck command.
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Wall-clock budget for `gh auth token`.
///
/// It reads the local keyring / `hosts.yml` and should answer in well under a
/// second. It nonetheless CAN block indefinitely — an observed `gh.exe` on this
/// fleet sat blocked for 4.5 hours, and because [`resolve_github_token`] awaited
/// it with no timeout, [`run_tick`] never returned for that entire session. Not
/// an error, not a slow tick: a permanent block, indistinguishable from "the
/// reconciler is not running". 10s is generous for a prompt-free credential
/// read and bounds the whole tick's worst case.
const GH_TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `cmd` to completion under `budget`, KILLING the child if it overruns.
///
/// Single door for every subprocess in this module. Before this existed each
/// call site awaited `.output()` directly, which has no timeout at all: one
/// hung child blocked its caller, which blocked [`run_tick`], forever. A
/// timeout bolted onto one call site would only have moved the hang to the
/// next, so all of them go through here.
///
/// `None` means "this command could not answer" — spawn failure OR timeout —
/// and callers must treat it exactly as they already treat a spawn failure:
/// never as a negative result. A timeout is logged at `warn!` with the command
/// and the budget it blew, because the whole cost of the original defect was
/// that four and a half hours of total failure looked like silence.
///
/// The child is killed via `kill_on_drop`: when `timeout` resolves to `Err` its
/// inner future — which owns the `Child` — is dropped, and tokio's drop guard
/// kills the process and hands it to the orphan reaper. That matters
/// concretely: the leaked `gh auth token` processes still resident on this box
/// are what NOT killing looks like.
async fn output_with_timeout(cmd: &mut Command, budget: Duration, what: &str) -> Option<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            debug!("session-pr-reconciler: {what} spawn failed: {e}");
            return None;
        }
    };
    // Captured before `wait_with_output` consumes the child, so the warn can
    // name the process an operator would go looking for in Task Manager.
    let pid = child.id();

    match tokio::time::timeout(budget, child.wait_with_output()).await {
        Ok(Ok(out)) => Some(out),
        Ok(Err(e)) => {
            debug!("session-pr-reconciler: {what} failed: {e}");
            None
        }
        Err(_) => {
            warn!(
                "session-pr-reconciler: {what} did not finish within {}s (pid {}) — killed it and \
                 treating it as NO ANSWER; a subprocess that hangs here blocks the whole \
                 reconcile tick, so this must never be silent",
                budget.as_secs(),
                pid.map(|p| p.to_string())
                    .unwrap_or_else(|| "?".to_string()),
            );
            None
        }
    }
}

/// `git -C <dir> <args…>` under [`GIT_COMMAND_TIMEOUT`]. Every git invocation
/// in this module goes through here; `None` is "git could not answer".
///
/// `tokio_no_window`, not a bare `Command::new`: the runner is a GUI-subsystem
/// process in release builds (`main.rs`'s `windows_subsystem = "windows"`), so
/// on Windows a console child spawned without `CREATE_NO_WINDOW` allocates its
/// own console — a visible window that flashes open and shut. This reconciler
/// forks git several times per session per 30s tick, which is the highest-rate
/// spawn site in the runner; spawned bare it is a continuous flicker on any
/// installed (non-debug) build. Same defect as commit 2318026ae, which this
/// module was written one week after and so never inherited.
async fn run_git(dir: &str, args: &[&str]) -> Option<Output> {
    let mut cmd = crate::process_helpers::tokio_no_window("git");
    cmd.arg("-C").arg(dir).args(args);
    let what = format!("git {} (in {dir})", args.join(" "));
    output_with_timeout(&mut cmd, GIT_COMMAND_TIMEOUT, &what).await
}

/// One branch with its HEAD sha and the `Session-Id` trailer value(s) on that
/// HEAD commit. Sourced from local heads AND `refs/remotes/origin/*`, so a
/// branch that was pushed and then deleted locally is still attributable.
struct BranchTrailer {
    /// Short branch name, with any `origin/` prefix already stripped — the
    /// spelling `list_prs_for_head` needs.
    branch: String,
    sha: String,
    session_ids: Vec<String>,
}

/// Cached per-repo resolution for one tick.
struct RepoInfo {
    owner: String,
    repo: String,
    branches: Vec<BranchTrailer>,
}

/// How long a resolved `working_dir → repo set` mapping is trusted.
///
/// A session's cwd effectively never changes repo, so this only needs to be
/// short enough that a moved/deleted checkout (or a freshly cloned sibling)
/// self-heals within a few minutes. It is also what keeps the depth-1
/// directory scan off the 30s tick: the per-tick cost stays one cheap
/// `for-each-ref` per repo, never a filesystem walk.
const TOPLEVEL_TTL: Duration = Duration::from_secs(600);

/// Bound on the cross-tick repo-set cache. Sessions come and go, so the map
/// needs a ceiling; well above any realistic live-session count.
const TOPLEVEL_CACHE_MAX: usize = 512;

/// Ceiling on how many repos one session's cwd may resolve to. A session
/// pointed at a huge tree must not be able to melt the tick (each repo costs a
/// `for-each-ref` plus, potentially, GitHub calls).
///
/// 128, not 32. The previous value was BELOW this workspace's real repo count:
/// `D:\qontinui-root` holds 38 depth-1 clones today, so five were dropped from
/// every single tick — and because candidates arrive name-sorted, the drop was
/// alphabetically biased, permanently excluding the tail (`ui-bridge*`,
/// `wrappers-registry`, `qontinui-workflow-*`) from PR attribution. A cap that
/// a normal workspace crosses is not a safety valve, it is a silent data loss.
/// 128 is >3x the current count and well beyond any workspace a human curates
/// by hand, while still bounding the pathological "session cwd is C:\" case;
/// and when it IS crossed, [`prioritise_by_recency`] makes the survivors the
/// repos someone is actually working in rather than the alphabetical head.
const REPO_SET_MAX: usize = 128;

/// Depth-1 child directory names that are NEVER a session's source repo.
///
/// These are build/worktree scratch, not source clones: `.spawn-origin_main`
/// and `qontinui-worktrees` hold throwaway checkouts (whose branches would be
/// attributed to whichever session last touched them), `target` /
/// `target-pool` / `dist` / `node_modules` are build output, and `.claude` is
/// agent state. Anything else beginning with `.` is skipped too — dotfile
/// directories are tooling, not checkouts.
const REPO_SCAN_SKIP_DIRS: &[&str] = &[
    ".spawn-origin_main",
    "qontinui-worktrees",
    ".claude",
    "node_modules",
    "target",
    "target-pool",
    "dist",
];

/// `working_dir → the set of git repo roots to reconcile for it`, cached
/// ACROSS ticks (plan `2026-07-28-runner-many-sessions-performance` §7a/B8).
///
/// The per-tick `repo_cache` below cannot help here: it is keyed BY the repo
/// root, so it can only dedupe work that happens *after* the roots are known —
/// the `git rev-parse --show-toplevel` subprocess (and now the depth-1
/// directory scan) that produces those keys still ran once per open record per
/// tick, i.e. N process spawns every 30s.
///
/// Invalidation: TTL'd and bounded. An EMPTY set is cached like any other —
/// "this cwd resolves to no repos" is an answer, and rescanning the filesystem
/// every 30s to re-learn it is exactly the cost the cache exists to avoid.
#[derive(Default)]
struct RepoSetCache {
    entries: HashMap<String, (Vec<String>, Instant)>,
}

impl RepoSetCache {
    fn get(&self, working_dir: &str) -> Option<&[String]> {
        let (repos, at) = self.entries.get(working_dir)?;
        if at.elapsed() > TOPLEVEL_TTL {
            return None;
        }
        Some(repos.as_slice())
    }

    fn insert(&mut self, working_dir: String, repos: Vec<String>) {
        if self.entries.len() >= TOPLEVEL_CACHE_MAX {
            // Drop everything expired; if that frees nothing, drop the whole
            // map rather than growing without bound. Re-resolution is one
            // subprocess per live session, not a correctness event.
            self.entries
                .retain(|_, (_, at)| at.elapsed() <= TOPLEVEL_TTL);
            if self.entries.len() >= TOPLEVEL_CACHE_MAX {
                self.entries.clear();
            }
        }
        self.entries.insert(working_dir, (repos, Instant::now()));
    }

    fn invalidate(&mut self, working_dir: &str) {
        self.entries.remove(working_dir);
    }
}

/// Depth-1 child directories of `working_dir` that LOOK like git repositories
/// (a `.git` entry exists — a directory for a normal clone, a file for a linked
/// worktree). Pure filesystem, no subprocess, so it unit-tests without git.
///
/// Depth 1 only, deliberately: recursing would turn a workspace parent into an
/// unbounded walk, and this fleet's layout puts every clone exactly one level
/// under the workspace root. Sorted by name so the resolved set — and therefore
/// the reconcile order and the `scannedRepos` the dropdown renders — is stable
/// across ticks.
///
/// NOT truncated: capping here made truncation UNOBSERVABLE to the caller,
/// which could then only infer it from `len() >= REPO_SET_MAX` and so warned
/// "PRs in the remainder will not be attributed" for a workspace holding
/// EXACTLY [`REPO_SET_MAX`] repos, with nothing dropped. The cap belongs to
/// [`resolve_repo_set_capped`], which applies it as a real total (the cwd's own
/// toplevel included) and knows how many candidates it actually dropped.
fn candidate_child_repos(working_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(working_dir) else {
        return Vec::new();
    };
    let mut names: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || REPO_SCAN_SKIP_DIRS.contains(&name.as_str()) {
                return None;
            }
            let path = e.path();
            if !path.is_dir() || !path.join(".git").exists() {
                return None;
            }
            Some((name, path))
        })
        .collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    names.into_iter().map(|(_, p)| p).collect()
}

/// A repo dir whose `.git` is a FILE is a LINKED WORKTREE (the file points at
/// its canonical clone's `.git/worktrees/<name>`), not a clone of its own.
///
/// Both shapes are scanned — a worktree's branches are the session's branches
/// too — but they share ONE ref store, so two such dirs resolve to the same
/// `owner/name`. When that happens the canonical clone wins (see
/// [`resolve_repo_set_capped`]): it is the dir a content proof should run in,
/// and it is the name a human reading `scannedRepos` expects.
fn is_linked_worktree(repo_dir: &Path) -> bool {
    repo_dir.join(".git").is_file()
}

/// The set of git repo roots to reconcile for one session cwd.
///
/// = `git rev-parse --show-toplevel` of the cwd (when it resolves) ∪ the
/// depth-1 child repos of the cwd. BOTH, always — the cwd being a repo does not
/// mean its siblings are irrelevant: a session rooted in one clone that also
/// commits to a neighbour is the normal shape here, and before this the
/// neighbour's PRs were simply never attributed.
///
/// Returns an EMPTY vec when nothing resolves. That is a real answer ("this cwd
/// contains no repos"), distinct from "never looked" — the caller records which
/// one happened, so the dropdown can say so instead of printing a confident
/// "no PRs".
async fn resolve_repo_set(working_dir: &str) -> Vec<String> {
    let (repos, dropped) = resolve_repo_set_capped(working_dir, REPO_SET_MAX).await;
    if dropped > 0 {
        // ONLY when something was genuinely dropped, and it says how many: the
        // predecessor inferred truncation from `len() >= REPO_SET_MAX` and so
        // warned about a "remainder" that did not exist. Not per-tick noise
        // either: resolution is TTL-cached for TOPLEVEL_TTL, so a given cwd can
        // warn at most once per cache lifetime.
        warn!(
            "session-pr-reconciler: {working_dir} resolves to more than {REPO_SET_MAX} git repos — \
             capping the scan at {REPO_SET_MAX}; {dropped} candidate repo(s) were NOT scanned and \
             their PRs will not be attributed"
        );
    }
    repos
}

/// [`resolve_repo_set`] with the ceiling injected, returning `(repos, dropped)`
/// so the cap is OBSERVABLE rather than inferred — and unit-testable at a small
/// `max` instead of needing 33 real repos on disk.
///
/// `max` is a real TOTAL: the cwd's own toplevel occupies a slot like any child
/// (the old code pushed it before the cap loop, making the effective ceiling
/// `REPO_SET_MAX + 1`). `dropped` counts candidate directories the cap stopped
/// us from even looking at — 0 means every candidate was resolved.
///
/// Dedupe: two local dirs can be the SAME GitHub repo (a linked worktree and
/// its clone share one ref store), which would run `resolve_repo` +
/// `branch_trailers` twice over identical refs every tick and render a
/// duplicate-looking `scannedRepos` entry. Collapsed here, by `owner/name` —
/// the same key `repo_dirs` uses in `run_tick` — preferring the canonical
/// clone. The extra `git remote get-url` this costs is per RESOLUTION, not per
/// tick: the result is `RepoSetCache`d for `TOPLEVEL_TTL`.
async fn resolve_repo_set_capped(working_dir: &str, max: usize) -> (Vec<String>, usize) {
    // The cwd itself first (it may be a repo, or inside one), then its depth-1
    // children — one list so the cap applies to the whole set.
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from(working_dir)];
    candidates.extend(candidate_child_repos(Path::new(working_dir)));

    // Only when the cap can actually bind does the ORDER decide who gets
    // dropped, and alphabetical order is an arbitrary thing to decide that on.
    // Below the cap this is skipped entirely, so the name-sorted order — and
    // with it the stable `scannedRepos` the dropdown renders — is untouched in
    // every normal case.
    if candidates.len() > max {
        prioritise_by_recency(&mut candidates);
    }

    let mut repos: Vec<String> = Vec::new();
    // `owner/name` → index into `repos`.
    let mut by_full: HashMap<String, usize> = HashMap::new();
    let mut dropped = 0usize;

    for (i, dir) in candidates.iter().enumerate() {
        if repos.len() >= max {
            dropped = candidates.len() - i;
            break;
        }
        let Some(top) = git_toplevel(&dir.to_string_lossy()).await else {
            continue;
        };
        if repos.iter().any(|r| r == &top) {
            continue;
        }
        let Some(full) = repo_full_name(&top).await else {
            // No parseable `origin` ⇒ nothing to dedupe on. Keep it: the tick
            // will skip it anyway, and dropping it here would hide it from
            // `scannedRepos`.
            repos.push(top);
            continue;
        };
        match by_full.get(&full).copied() {
            Some(idx) => {
                // Same GitHub repo through a second local dir. Keep ONE, and
                // prefer the canonical clone over a linked worktree.
                if is_linked_worktree(Path::new(&repos[idx]))
                    && !is_linked_worktree(Path::new(&top))
                {
                    repos[idx] = top;
                }
            }
            None => {
                by_full.insert(full, repos.len());
                repos.push(top);
            }
        }
    }

    (repos, dropped)
}

/// Reorder `candidates[1..]` most-recently-touched FIRST, so that when the cap
/// binds the repos it drops are the ones nobody has worked in.
///
/// `candidates[0]` is the session's own cwd and is pinned: it is the one repo
/// that is certainly relevant, so it must never be a cap casualty.
///
/// Recency = the newer of the directory's own mtime and its `.git` mtime.
/// Neither is a perfect "last commit" signal (dir mtime moves on top-level
/// file churn, `.git` on ref/index writes) but together they separate an active
/// checkout from one untouched for months, which is all the cap needs. Cost is
/// two `stat`s per candidate, paid ONLY on the pathological path — a workspace
/// with more than [`REPO_SET_MAX`] repos.
///
/// Ties, and candidates whose mtime cannot be read, fall back to name order, so
/// a quiescent workspace still resolves deterministically tick after tick. The
/// trade-off accepted here: when the cap binds, `scannedRepos` order tracks
/// filesystem activity rather than the alphabet. Dropping the right repos
/// matters more than a stable order in a set that is already being truncated.
fn prioritise_by_recency(candidates: &mut [PathBuf]) {
    fn touched_at(p: &Path) -> Option<std::time::SystemTime> {
        let own = std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let git = std::fs::metadata(p.join(".git"))
            .and_then(|m| m.modified())
            .ok();
        own.max(git)
    }
    if candidates.len() < 2 {
        return;
    }
    // `None` sorts BEFORE `Some`, so reversing the mtime comparison also puts
    // unreadable candidates last — where they belong.
    candidates[1..].sort_by(|a, b| {
        touched_at(b)
            .cmp(&touched_at(a))
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });
}

/// `owner/name` for a repo toplevel, from its `origin` remote.
async fn repo_full_name(toplevel: &str) -> Option<String> {
    let remote = git_remote_url(toplevel).await?;
    let (owner, repo) = parse_owner_repo(&remote)?;
    Some(format!("{owner}/{repo}"))
}

/// Per-session record of the repo set the reconciler LAST resolved.
///
/// A process global rather than Tauri state, matching this subsystem's existing
/// idiom (`PgDb::try_global()` — see `commands::session_info::load_prs`), so the
/// Tauri command and its HTTP twin read it identically with no `ApiState`
/// plumbing.
///
/// An ABSENT entry means "the reconciler has never resolved this session" — the
/// state that used to be indistinguishable from "scanned and found no PRs", and
/// which the dropdown now spells out.
static SCANNED_REPOS: OnceLock<Mutex<HashMap<Uuid, Vec<String>>>> = OnceLock::new();

fn scanned_repos_map() -> &'static Mutex<HashMap<Uuid, Vec<String>>> {
    SCANNED_REPOS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record which repos were scanned for `session_id` this tick.
fn record_scanned_repos(session_id: Uuid, repos: &[String]) {
    let Ok(mut map) = scanned_repos_map().lock() else {
        return;
    };
    if map.len() >= TOPLEVEL_CACHE_MAX && !map.contains_key(&session_id) {
        // Bounded like the repo-set cache: sessions churn, and losing a record
        // degrades to "not scanned yet", never to a wrong answer.
        map.clear();
    }
    map.insert(session_id, repos.to_vec());
}

/// The repos the reconciler last scanned for `session_id`.
///
/// `None` ⇒ never scanned (no tick has resolved this session). `Some([])` ⇒
/// scanned, and the cwd resolved to no git repos at all. `Some([..])` ⇒ these
/// repos were searched. The three are different claims and the dropdown renders
/// them differently.
pub fn last_scanned_repos(session_id: Uuid) -> Option<Vec<String>> {
    scanned_repos_map().lock().ok()?.get(&session_id).cloned()
}

/// Capacity of the on-demand nudge channel. Tiny on purpose: a nudge is an
/// optimisation, so a full channel drops rather than queues — nudges must never
/// stack up into a backlog of ticks.
const NUDGE_CHANNEL_CAP: usize = 8;

/// Minimum gap between two on-demand reconciles OF THE SAME SESSION. Five
/// seconds: short enough that opening the dropdown, seeing a stale count and
/// reopening it feels responsive, long enough that a panel re-render loop or a
/// UI-Bridge script hammering the trigger cannot turn into a GitHub API storm.
const NUDGE_DEBOUNCE: Duration = Duration::from_secs(5);

/// Sender for [`nudge_session`], published by [`start`].
static NUDGE_TX: OnceLock<mpsc::Sender<Uuid>> = OnceLock::new();

/// Ask the reconciler to reconcile ONE session promptly, out of band with the
/// 30s tick. Fire-and-forget by contract: the caller (`session_info_get`)
/// returns the currently-stored ledger immediately and never blocks on this.
///
/// Silently drops when the reconciler is not running or the channel is full.
/// This is an optimisation on top of the interval tick, never a correctness
/// path — the next tick reconciles the session regardless. Because it is async,
/// the FIRST dropdown open after a PR is created may still show the old count;
/// the next poll shows the new one.
pub fn nudge_session(session_id: Uuid) {
    if let Some(tx) = NUDGE_TX.get() {
        let _ = tx.try_send(session_id);
    }
}

/// What the tick-failure ledger decided to say about one `run_tick` error.
#[derive(Debug, PartialEq, Eq)]
enum TickFailureReport {
    /// First tick to fail, or the reason CHANGED — always worth a warn.
    New,
    /// The same reason is still failing, and the backoff says report it now.
    Persisting { consecutive: u32, elapsed: Duration },
    /// Same reason, inside the current backoff window — stay quiet.
    Quiet,
}

/// Deduped ledger for WHOLE-TICK failures.
///
/// `run_tick` returns `Err` only for a precondition miss (PG unavailable, no
/// GitHub token) — a complete outage of the feature, not a per-session skip.
/// The predecessor logged that at `debug!`, which lands in no log this fleet
/// keeps, so 4.5 hours during which the reconciler did nothing at all was
/// indistinguishable from silence.
///
/// It cannot simply become an unconditional `warn!` either: at a 30s tick a
/// persistent condition (an operator with PG stopped for the afternoon) would
/// emit 120 identical lines an hour and train everyone to ignore it. So: warn
/// on the FIRST failure and on any change of reason, then on an exponentially
/// widening schedule (ticks 1, 2, 4, 8, 16, …), each repeat saying how long the
/// condition has persisted. A four-hour outage costs ~9 lines, and none of them
/// can be mistaken for a transient.
#[derive(Default)]
struct TickFailureLedger {
    current: Option<TickFailureRun>,
}

struct TickFailureRun {
    reason: String,
    since: Instant,
    consecutive: u32,
    /// The `consecutive` value at which the next warn fires. Doubles each time.
    warn_at: u32,
}

impl TickFailureLedger {
    /// Record a failed tick and decide whether it should be reported.
    fn record(&mut self, reason: &str) -> TickFailureReport {
        match self.current.as_mut() {
            Some(run) if run.reason == reason => {
                run.consecutive += 1;
                if run.consecutive >= run.warn_at {
                    run.warn_at = run.warn_at.saturating_mul(2);
                    TickFailureReport::Persisting {
                        consecutive: run.consecutive,
                        elapsed: run.since.elapsed(),
                    }
                } else {
                    TickFailureReport::Quiet
                }
            }
            // No run, or the reason changed — a different failure is news.
            _ => {
                self.current = Some(TickFailureRun {
                    reason: reason.to_string(),
                    since: Instant::now(),
                    consecutive: 1,
                    warn_at: 2,
                });
                TickFailureReport::New
            }
        }
    }

    /// Record a successful tick. Returns `Some((consecutive, elapsed))` when
    /// that success ENDED a run of failures — recovery is worth one line, so
    /// the log says when the outage stopped and not only that it started.
    fn clear(&mut self) -> Option<(u32, Duration)> {
        self.current
            .take()
            .map(|run| (run.consecutive, run.since.elapsed()))
    }
}

/// Start the reconciler as a detached background task for the process
/// lifetime (matching the lifecycle liveness-poll idiom in `main.rs`). Runs a
/// best-effort pass immediately, then every [`POLL_INTERVAL`] — or sooner for
/// ONE session when [`nudge_session`] fires (the dropdown was opened).
///
/// Exactly ONE per process: a second call warns and returns without starting
/// anything (see below).
pub fn start(lifecycle_store: std::sync::Arc<SessionLifecycleStore>) {
    let (tx, mut rx) = mpsc::channel::<Uuid>(NUDGE_CHANNEL_CAP);
    // Only the FIRST reconciler owns the nudge door — and a second reconciler
    // must not run at all. Two of them ticking the same sessions against the
    // same table is not something to half-support, and the half-supported
    // version was worse than useless: `set` would drop this `tx`, leaving the
    // task we are about to spawn with an `rx` whose sender is already gone, so
    // `recv()` resolves to `None` instantly and forever — a 100%-CPU spin for
    // the process lifetime, silent. Refuse loudly instead of spawning.
    if NUDGE_TX.set(tx).is_err() {
        warn!(
            "session-pr-reconciler: a second reconciler was started; only the first owns the \
             nudge door — NOT starting this one (one reconciler per process)"
        );
        return;
    }

    tauri::async_runtime::spawn(async move {
        info!(
            "session-pr-reconciler: started (interval: {}s, on-demand nudge debounce: {}s)",
            POLL_INTERVAL.as_secs(),
            NUDGE_DEBOUNCE.as_secs()
        );
        let mut repo_sets = RepoSetCache::default();
        // Sessions already warned about having no resolvable repos — the warn
        // fires ONCE per session, not every 30s (item 2: the skip must be
        // visible without being noise).
        let mut warned_no_repos: HashSet<Uuid> = HashSet::new();
        // Last on-demand reconcile per session, for the NUDGE_DEBOUNCE gate.
        let mut last_nudge: HashMap<Uuid, Instant> = HashMap::new();
        // Whole-tick failures, deduped — see `TickFailureLedger`.
        let mut tick_failures = TickFailureLedger::default();

        // `interval` (not `sleep`) so a nudge cannot postpone the periodic
        // tick, and `Delay` so a slow tick does not produce a burst of
        // catch-up ticks afterwards.
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Independent of the guard above: a dropped sender makes `rx.recv()`
        // resolve to `None` IMMEDIATELY and forever, so an arm that merely
        // `continue`d would busy-loop with no yielding await. Disabling the
        // branch instead retires the channel for good — the periodic tick keeps
        // running, and nothing spins.
        let mut nudges_open = true;

        loop {
            let only: Option<Uuid> = tokio::select! {
                _ = ticker.tick() => None,
                maybe = rx.recv(), if nudges_open => match maybe {
                    Some(session_id) => {
                        let now = Instant::now();
                        let fresh = last_nudge
                            .get(&session_id)
                            .is_none_or(|at| now.duration_since(*at) >= NUDGE_DEBOUNCE);
                        if !fresh {
                            continue;
                        }
                        if last_nudge.len() >= TOPLEVEL_CACHE_MAX {
                            last_nudge.retain(|_, at| now.duration_since(*at) < NUDGE_DEBOUNCE);
                        }
                        last_nudge.insert(session_id, now);
                        Some(session_id)
                    }
                    // Sender dropped: nothing can nudge again. Stop selecting
                    // on the channel (a re-`continue` here would hot-spin) and
                    // keep ticking on the interval alone.
                    None => {
                        nudges_open = false;
                        continue;
                    }
                },
            };

            // A whole-tick error means the feature produced NOTHING this pass.
            // Reported at `warn!` — deduped by `TickFailureLedger` so a
            // persistent condition does not emit every 30s.
            match run_tick(&lifecycle_store, &mut repo_sets, &mut warned_no_repos, only).await {
                Err(e) => match tick_failures.record(&e) {
                    TickFailureReport::New => warn!(
                        "session-pr-reconciler: tick FAILED — {e}; no session PRs are being \
                         attributed while this persists (further identical failures are \
                         reported on a widening interval)"
                    ),
                    TickFailureReport::Persisting {
                        consecutive,
                        elapsed,
                    } => warn!(
                        "session-pr-reconciler: tick STILL failing — {e}; {consecutive} \
                         consecutive failed ticks over {}s, no session PRs attributed in that \
                         time",
                        elapsed.as_secs()
                    ),
                    TickFailureReport::Quiet => {
                        debug!("session-pr-reconciler: tick error (continuing): {e}")
                    }
                },
                Ok(()) => {
                    if let Some((consecutive, elapsed)) = tick_failures.clear() {
                        info!(
                            "session-pr-reconciler: RECOVERED — a tick succeeded after \
                             {consecutive} consecutive failures over {}s",
                            elapsed.as_secs()
                        );
                    }
                }
            }
        }
    });
}

/// One reconcile pass. Returns `Err` only for a whole-tick precondition miss
/// (no DB / no token); per-session/per-repo failures are logged and skipped.
///
/// `only` restricts the pass to a single session (the on-demand nudge path);
/// `None` reconciles every open session.
async fn run_tick(
    store: &SessionLifecycleStore,
    repo_sets: &mut RepoSetCache,
    warned_no_repos: &mut HashSet<Uuid>,
    only: Option<Uuid>,
) -> Result<(), String> {
    if !crate::database::pg::pg_available() {
        return Err("PG unavailable".to_string());
    }
    let Some(pg_db) = PgDb::try_global() else {
        return Err("PgDb global not set".to_string());
    };

    // Consider only live Claude sessions with a known cwd — the trailer is
    // keyed by `claude_session_id`, so non-Claude providers never match.
    let records: Vec<(Uuid, String)> = store
        .open_records()
        .into_iter()
        .filter(|r| r.provider == "claude")
        .filter_map(|r| {
            let wd = r.working_dir.clone()?;
            let id = Uuid::parse_str(r.claude_session_id.trim()).ok()?;
            Some((id, wd))
        })
        .filter(|(id, _)| only.is_none_or(|want| *id == want))
        .collect();
    if records.is_empty() {
        // Still log. "The reconciler ran and saw zero sessions" is exactly what
        // you want to see when debugging this subsystem, and returning silently
        // made it indistinguishable from the reconciler not running at all.
        info!(
            "session-pr-reconciler: tick done — sessions_seen=0 sessions_with_repos=0 \
             repos_scanned=0 prs_upserted=0 scope={}",
            tick_scope(only)
        );
        return Ok(());
    }

    let Some(token) = resolve_github_token().await else {
        return Err("no GitHub token (env GITHUB_TOKEN/GH_TOKEN or `gh auth token`)".to_string());
    };
    let client = GitHubClient::new(&token)?;

    // Per-repo (git toplevel) resolution cached across sessions that share a
    // checkout: one `remote get-url` + one `for-each-ref` per repo per tick.
    let mut repo_cache: HashMap<String, Option<RepoInfo>> = HashMap::new();

    // Per-tick counters (item 2). Emitted at `info!` so "the reconciler ran and
    // saw N sessions, M of which had a repo" is answerable from the log alone —
    // the whole class of defect here was a skip nobody could observe.
    let sessions_seen = records.len();
    let mut sessions_with_repos = 0usize;
    let mut repos_scanned = 0usize;
    let mut prs_upserted = 0usize;

    for (session_id, working_dir) in records {
        let repos: Vec<String> = match repo_sets.get(&working_dir) {
            Some(cached) => cached.to_vec(),
            None => {
                let resolved = resolve_repo_set(&working_dir).await;
                // An EMPTY result is cached like any other. Do NOT
                // cache-and-forget the miss the way the old toplevel-only path
                // did: caching the empty answer is what keeps the depth-1
                // directory scan off the 30s tick, and the TTL is what lets a
                // newly cloned sibling still appear within a few minutes.
                repo_sets.insert(working_dir.clone(), resolved.clone());
                resolved
            }
        };

        // Record the resolution BEFORE reconciling: `session_info_get` reads
        // this to tell "scanned, no PRs" apart from "never looked at".
        record_scanned_repos(session_id, &repos);

        if repos.is_empty() {
            // ONCE per session, not once per tick. The predecessor logged this
            // at `debug!`, which appears in no log this fleet keeps — so a
            // session whose cwd is the workspace parent was dropped from every
            // tick for weeks with no observable trace.
            if warned_no_repos.insert(session_id) {
                warn!(
                    "session-pr-reconciler: session {session_id} has NO resolvable git repo — \
                     working_dir {working_dir} is not a git repository and has no depth-1 child \
                     repositories; this session's PRs cannot be attributed"
                );
            }
            continue;
        }
        // The session resolved this time: re-arm the warn so a checkout that
        // later disappears is reported again.
        warned_no_repos.remove(&session_id);
        sessions_with_repos += 1;

        // Union of every repo's Phase-2 results, plus `owner/name → local dir`
        // for the whole set, so the once-per-session Phase-3 pass below can run
        // its content proof against the PR's OWN checkout.
        let mut refreshed: HashSet<(String, i64)> = HashSet::new();
        let mut repo_dirs: HashMap<String, String> = HashMap::new();

        for toplevel in &repos {
            if !repo_cache.contains_key(toplevel) {
                let info = resolve_repo(toplevel).await;
                repo_cache.insert(toplevel.clone(), info);
            }
            let Some(info) = repo_cache.get(toplevel).and_then(|o| o.as_ref()) else {
                continue;
            };
            repos_scanned += 1;
            repo_dirs.insert(format!("{}/{}", info.owner, info.repo), toplevel.clone());

            match attribute_session_in_repo(&pg_db, &client, session_id, toplevel, info).await {
                Ok(keys) => {
                    prs_upserted += keys.len();
                    refreshed.extend(keys);
                }
                Err(e) => debug!(
                    "session-pr-reconciler: session {session_id} in {toplevel} failed (skipping): {e}"
                ),
            }
        }

        if let Err(e) =
            refresh_stored_statuses(&pg_db, &client, session_id, &refreshed, &repo_dirs).await
        {
            debug!("session-pr-reconciler: status refresh for {session_id} failed: {e}");
        }
    }

    info!(
        "session-pr-reconciler: tick done — sessions_seen={sessions_seen} \
         sessions_with_repos={sessions_with_repos} repos_scanned={repos_scanned} \
         prs_upserted={prs_upserted} scope={}",
        tick_scope(only)
    );

    Ok(())
}

/// `scope=` field of the per-tick counters line — shared by the zero-session
/// early return and the normal end-of-tick line so both read identically.
fn tick_scope(only: Option<Uuid>) -> String {
    match only {
        Some(id) => format!("on-demand:{id}"),
        None => "all".to_string(),
    }
}

/// Phase 2 — attribute this session's branches in ONE repo to PRs and upsert
/// them. Returns the `(repo_full, pr_number)` keys it refreshed, so the
/// once-per-session Phase-3 pass ([`refresh_stored_statuses`]) can skip a
/// redundant `get_pr` for them.
///
/// Phase 3 is deliberately NOT here: a session now reconciles against a SET of
/// repos, and running the stored-row status refresh per repo would re-`get_pr`
/// every stored row once per repo in the set.
async fn attribute_session_in_repo(
    pg_db: &PgDb,
    client: &GitHubClient,
    session_id: Uuid,
    repo_dir: &str,
    info: &RepoInfo,
) -> Result<HashSet<(String, i64)>, String> {
    let session_str = session_id.to_string();
    let repo_full = format!("{}/{}", info.owner, info.repo);

    let mut refreshed: HashSet<(String, i64)> = HashSet::new();

    // ---- Phase 2: attribution (branch → PR), for THIS session's branches ---
    for bt in info
        .branches
        .iter()
        .filter(|b| b.session_ids.iter().any(|s| s == &session_str))
    {
        let prs = match client
            .list_prs_for_head(&info.owner, &info.repo, &bt.branch)
            .await
        {
            Ok(prs) => prs,
            Err(e) => {
                debug!(
                    "session-pr-reconciler: list_prs_for_head({repo_full}, {}) failed: {e}",
                    bt.branch
                );
                continue;
            }
        };

        for pr in prs {
            // VERIFY the PR's head-commit trailer names this session. When the
            // PR head equals the local branch HEAD we already matched, it is
            // verified by construction; otherwise read the PR head's trailer
            // locally (works because the runner pushed it), and fall back to
            // the branch-HEAD match if that object isn't present locally.
            let verified = if pr.head_sha.is_empty() || pr.head_sha == bt.sha {
                true
            } else {
                match read_session_trailers(repo_dir, &pr.head_sha).await {
                    Some(ids) => ids.iter().any(|s| s == &session_str),
                    None => true,
                }
            };
            if !verified {
                continue;
            }

            let pr_number = pr.number as i64;
            // CALL SITE 1 of the land cascade (the branch-attribution path).
            let proof = probe_land(
                Some(repo_dir),
                pr.merged,
                &pr.state,
                &pr.head_sha,
                &pr.base_ref,
            )
            .await;
            let verdict = land_verdict(
                pr.merged,
                &pr.state,
                has_coord_landed_label(&pr.labels),
                proof,
            );
            let status = session_pr_status(
                &verdict,
                &pr.state,
                pr.merged_at.as_deref().and_then(parse_ts),
                pr.closed_at.as_deref().and_then(parse_ts),
            );

            if let Err(e) = pg_db
                .upsert_session_pr(
                    session_id,
                    &repo_full,
                    pr_number,
                    Some(bt.branch.as_str()),
                    &status,
                )
                .await
            {
                warn!("session-pr-reconciler: upsert_session_pr failed: {e}");
                continue;
            }
            refreshed.insert((repo_full.clone(), pr_number));
        }
    }

    Ok(refreshed)
}

/// Phase 3 — status refresh for this session's stored rows that the branch
/// path did not reach this tick. A session's merged PR usually has its local
/// branch deleted, so Phase 2 no longer refreshes it; pull each such row's
/// current state directly. Scoped to this session's OWN PRs — never a fleet
/// scan — and run ONCE per session, over the union of every repo's Phase-2
/// results.
///
/// `repo_dirs` maps `owner/name` → a local checkout of that repo (every repo in
/// the session's resolved set). The land content-proof needs a local checkout OF
/// THE PR'S OWN REPO; a row whose repo is not in the set stays UNKNOWN
/// (`not_a_repo`), never a confident negative.
async fn refresh_stored_statuses(
    pg_db: &PgDb,
    client: &GitHubClient,
    session_id: Uuid,
    refreshed: &HashSet<(String, i64)>,
    repo_dirs: &HashMap<String, String>,
) -> Result<(), String> {
    let stored = pg_db.list_session_prs(session_id).await.unwrap_or_default();
    for row in stored {
        if refreshed.contains(&(row.repo.clone(), row.pr_number)) {
            continue;
        }
        let Some((owner, repo)) = row.repo.split_once('/') else {
            continue;
        };
        match client.get_pr(owner, repo, row.pr_number as u64).await {
            Ok(pr) => {
                // CALL SITE 2 of the land cascade. THIS is the ff-land path in
                // practice: a landed PR's branch is usually deleted, which is
                // exactly why the branch path above no longer reaches it.
                //
                // The content proof needs a local checkout OF THIS PR'S repo.
                // A session that moved between repos can hold rows for a repo
                // this tick did not resolve — that is UNKNOWN (`not_a_repo`),
                // never a confident negative.
                let dir_for_row = repo_dirs.get(&row.repo).map(String::as_str);
                let proof = probe_land(
                    dir_for_row,
                    pr.merged,
                    &pr.state,
                    &pr.head_sha,
                    &pr.base_ref,
                )
                .await;
                let verdict = land_verdict(
                    pr.merged,
                    &pr.state,
                    has_coord_landed_label(&pr.labels),
                    proof,
                );
                // `get_pr`'s PrStatus exposes no merged_at; preserve any stamp
                // the branch path already recorded.
                let status = session_pr_status(
                    &verdict,
                    &pr.state,
                    row.merged_at,
                    pr.closed_at.as_deref().and_then(parse_ts),
                );
                if let Err(e) = pg_db
                    .update_session_pr_status(session_id, &row.repo, row.pr_number, &status)
                    .await
                {
                    warn!("session-pr-reconciler: update_session_pr_status failed: {e}");
                }
            }
            Err(e) => {
                debug!(
                    "session-pr-reconciler: get_pr({}/#{}) failed (leaving prior status): {e}",
                    row.repo, row.pr_number
                );
            }
        }
    }

    Ok(())
}

/// Map the LAND verdict + GitHub's lifecycle to the projection's `pr_state`
/// label.
///
/// The first argument is the cascade's `landed` verdict, **not** GitHub's
/// `merged` boolean: a fast-forward land is a land, and reading it off
/// `merged` is the defect this cascade exists to fix (G3).
fn pr_state_label(landed: bool, state: &str) -> &'static str {
    if landed {
        "merged"
    } else if state == "closed" {
        "closed"
    } else {
        "open"
    }
}

/// Why a land verdict could not be established. Persisted verbatim in
/// `project.session_prs.land_reason` so the dropdown can say *why* it cannot
/// answer instead of asserting a negative it never established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LandUnknown {
    /// No local checkout to read: the PR's repo is not the one this tick
    /// resolved for the session, or `git` could not run there.
    NotARepo,
    /// `origin/<base>` does not exist locally.
    NoBaseRef,
    /// The PR head commit is not present locally — the usual state after a
    /// post-land branch delete (and a later `git gc`).
    HeadObjectMissing,
    /// Both objects exist and git answered "not an ancestor", but the local
    /// `origin/<base>` tip is no newer than the PR head — so that ref could
    /// not contain the head whatever happened upstream, and the negative is an
    /// artefact of a stale ref rather than evidence. Measured: this repo and
    /// the running runner were both 19 commits behind `origin/main` at plan
    /// vet time, so this is the COMMON case on this fleet, not the exotic one.
    RefStale,
    /// Every input was present, `origin/<base>` was strictly NEWER than the PR
    /// head, and git still answered "not an ancestor".
    ///
    /// This is not evidence of "not landed". coord lands by rebasing the PR's
    /// commits onto the base and pushing the REPLAYED shas, so the PR's own
    /// head sha is not on `origin/<base>` after a perfectly ordinary land —
    /// the ancestry test cannot pass on that shape and never could. The two
    /// live possibilities are "coord rebase-landed it" and "it was closed
    /// unlanded", and ancestry does not separate them:
    ///
    /// > *Ancestry is a one-way signal … a pass is positive evidence, but a
    /// > fail is not evidence of "unlanded". Never use it as a negative test.*
    /// > — `knowledge-base/qontinui-specific/coord-ff-lands.md`
    ///
    /// Reporting this as `not-landed` is exactly the defect that put a
    /// `closed, not landed` chip on landed PRs in the Terminal dropdown.
    RebaseLandOrAbandoned,
    /// coord's `coord:landed` chip is on the PR, but GitHub reports the PR is
    /// NOT closed. Two live signals disagree, so we have no verdict.
    ///
    /// The chip is NOT unconditionally a land proof. coord's phantom
    /// empty-diff sweep applies it off `tree_already_on_main` — a
    /// `git merge-tree` CONTENT-equality check — and that is equally true of a
    /// superseded PR, a duplicate, and a PR that was a no-op empty diff from
    /// the start and landed nothing. Worse, the sweep applies it on its
    /// `CommentedButCloseFailed` arm, whose own log records that the close
    /// PATCH failed and *the PR is still OPEN*
    /// (`qontinui-coord/src/pr_merge/mod.rs`, the label write beneath that
    /// match). coord has no path that ever REMOVES the chip.
    ///
    /// For a CLOSED PR the chip is still the best signal available and the
    /// cascade takes it: even in the superseded case the session's content
    /// shipped. For an OPEN one it would override GitHub's live statement that
    /// the PR is not merged, which is the one direction this module must never
    /// go — a false "landed" is worse than an honest unknown, and the
    /// projection's UPSERT would then pin it.
    CoordChipOnOpenPr,
    /// GitHub's `state` was never observed. Both API parsers default a
    /// missing or non-string `state` to the literal `"unknown"`, so a partial
    /// payload or schema drift reaches the cascade as a state that is neither
    /// `open` nor `closed`. That is not an evaluated negative — it is the
    /// absence of the observation the negative would rest on.
    PrStateUnobserved,
}

impl LandUnknown {
    fn as_str(self) -> &'static str {
        match self {
            LandUnknown::NotARepo => "not_a_repo",
            LandUnknown::NoBaseRef => "no_base_ref",
            LandUnknown::HeadObjectMissing => "head_object_missing",
            LandUnknown::RefStale => "ref_stale",
            LandUnknown::RebaseLandOrAbandoned => "rebase_land_or_abandoned",
            LandUnknown::CoordChipOnOpenPr => "coord_chip_on_open_pr",
            LandUnknown::PrStateUnobserved => "pr_state_unobserved",
        }
    }
}

/// Outcome of signal 2 — the local content proof (`git merge-base
/// --is-ancestor`). Injected into [`land_verdict`] so the cascade is pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentProof {
    /// The PR head is an ancestor of `origin/<base>`: the commits ARE on the
    /// base branch. Land-path independent — this is what catches an ff-land.
    Ancestor,
    /// Every input was present, the base tip was strictly newer, and git
    /// answered "not an ancestor". **Not a negative verdict** — see
    /// [`LandUnknown::RebaseLandOrAbandoned`], which is what the cascade
    /// turns this into.
    NotAncestor,
    /// The probe could not be evaluated. Carries the recorded reason.
    Unknown(LandUnknown),
    /// Deliberately not attempted, and no subprocess was spawned: GitHub
    /// already reported the PR merged (signal 1 wins), or GitHub still reports
    /// it OPEN. GitHub closes a PR as soon as its head commits become
    /// reachable from the base branch — ff-lands included — so `open` is an
    /// EVALUATED negative, not an unevaluated one.
    NotAttempted,
}

/// The land verdict for one PR: what the projection stores and the dropdown
/// renders.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LandVerdict {
    /// True iff some signal PROVED the PR landed.
    landed: bool,
    /// `"github-merge"` | `"ff-land"` | `"coord-label"` | `"not-landed"` |
    /// `"land-unknown"`.
    signal: &'static str,
    /// Set iff `signal == "land-unknown"`.
    reason: Option<&'static str>,
}

/// THE CASCADE — first hit wins, each hit recorded. Pure over its inputs, so
/// it unit-tests without git, PG or the network; `proof` is the injected
/// result of signal 2 ([`probe_land`]) and `coord_landed` the injected result
/// of signal 3.
///
/// 1. GitHub `merged == true` → `github-merge`.
/// 2. Content proof: the head commit is an ancestor of `origin/<base>` →
///    `ff-land`. 1 and 3 corroborate it.
/// 3. coord's own verdict, read off the [`COORD_LANDED_LABEL`] chip on the PR
///    payload the caller already fetched → `coord-label`, **but only on a PR
///    GitHub reports CLOSED**. This is the signal that CARRIES this fleet,
///    because 1 and 2 both structurally miss coord's majority land shape:
///    coord rebases and pushes REPLAYED shas, so GitHub records no merge and
///    the PR's own head sha is never on `origin/<base>`.
///    (The doc-comment this replaces reserved slot 3 for a direct coord PR
///    -status client and called it NOT WIRED. The label is the same verdict
///    from the same authority over a transport the runner already pays for —
///    no new client, no network dependency on a 30s background tick. A direct
///    client, if one ever exists, corroborates rather than replaces it.)
///
///    The closed-only gate is load-bearing, not defensive: the chip does not
///    prove a land on its own, and on an OPEN PR it would contradict GitHub's
///    live state. See [`LandUnknown::CoordChipOnOpenPr`] for the two coord
///    paths that produce exactly that.
/// 4. Otherwise TWO distinct terminal states, never merged into one:
///    `not-landed` (every signal was evaluated and said no) and `land-unknown`
///    (a signal could not be evaluated), the latter always with its reason.
///
/// **A failed ancestry test is NOT a `not-landed`.** It lands in 4's unknown
/// arm as [`LandUnknown::RebaseLandOrAbandoned`]. The ONLY evaluated negative
/// is [`ContentProof::NotAttempted`] on a PR GitHub positively reports `open`
/// — a state that is neither `open` nor `closed` was never observed at all and
/// is [`LandUnknown::PrStateUnobserved`].
fn land_verdict(
    github_merged: bool,
    github_state: &str,
    coord_landed: bool,
    proof: ContentProof,
) -> LandVerdict {
    if github_merged {
        return LandVerdict {
            landed: true,
            signal: "github-merge",
            reason: None,
        };
    }
    if proof == ContentProof::Ancestor {
        return LandVerdict {
            landed: true,
            signal: "ff-land",
            reason: None,
        };
    }
    if coord_landed {
        // Closed-only. On any other state coord's chip and GitHub's live view
        // disagree, and the honest answer is that we cannot tell.
        return if github_state == "closed" {
            LandVerdict {
                landed: true,
                signal: "coord-label",
                reason: None,
            }
        } else {
            LandVerdict {
                landed: false,
                signal: "land-unknown",
                reason: Some(LandUnknown::CoordChipOnOpenPr.as_str()),
            }
        };
    }
    match proof {
        // Handled above; repeated so the match stays exhaustive without a
        // catch-all that would silently absorb a future variant.
        ContentProof::Ancestor => LandVerdict {
            landed: true,
            signal: "ff-land",
            reason: None,
        },
        ContentProof::Unknown(reason) => LandVerdict {
            landed: false,
            signal: "land-unknown",
            reason: Some(reason.as_str()),
        },
        ContentProof::NotAncestor => LandVerdict {
            landed: false,
            signal: "land-unknown",
            reason: Some(LandUnknown::RebaseLandOrAbandoned.as_str()),
        },
        // Reached only when GitHub did not report the PR closed (a `merged`
        // PR short-circuits at signal 1). `open` is a real observation and a
        // real negative; anything else is the parsers' `"unknown"` default,
        // i.e. no observation at all.
        ContentProof::NotAttempted if github_state == "open" => LandVerdict {
            landed: false,
            signal: "not-landed",
            reason: None,
        },
        ContentProof::NotAttempted => LandVerdict {
            landed: false,
            signal: "land-unknown",
            reason: Some(LandUnknown::PrStateUnobserved.as_str()),
        },
    }
}

/// Assemble the projection's status columns from the GitHub payload and the
/// land verdict.
///
/// `landed_at` is the LAND time by whichever signal proved it: GitHub's
/// `merged_at` for a merge, the PR's `closed_at` for an ff-land (when GitHub
/// or coord closed it once the commits were on the base branch). Never `now()`
/// — that is the observation time, not the land time.
fn session_pr_status(
    verdict: &LandVerdict,
    github_state: &str,
    merged_at: Option<DateTime<Utc>>,
    closed_at: Option<DateTime<Utc>>,
) -> SessionPrStatus {
    SessionPrStatus {
        pr_state: Some(pr_state_label(verdict.landed, github_state).to_string()),
        landed: verdict.landed,
        merged_at,
        land_signal: Some(verdict.signal.to_string()),
        land_reason: verdict.reason.map(|r| r.to_string()),
        landed_at: if verdict.landed {
            merged_at.or(closed_at)
        } else {
            None
        },
    }
}

/// Signal 2: is the PR's head commit already on `origin/<base>`?
///
/// GUARDED, because `git merge-base --is-ancestor` exits non-zero for a
/// MISSING OBJECT and for NOT AN ANCESTOR alike — conflating the two is what
/// turns a landed PR into a confident "not landed". Both revisions are
/// verified present first, and every path that cannot answer returns
/// [`ContentProof::Unknown`] with a reason.
///
/// NEVER FETCHES. A background tick that runs `git fetch` across every
/// session's repo is a cost and concurrency hazard this reconciler
/// deliberately avoids (its per-tick budget is one `remote get-url` + one
/// `for-each-ref` per repo). The price is that a stale `origin/<base>` yields
/// `ref_stale` UNKNOWN rather than a verdict — which is the honest answer.
///
/// Cost: zero subprocesses unless GitHub says closed-and-not-merged, then two
/// `rev-parse` guards + one `merge-base`, plus two `log -1` timestamp reads
/// only on the negative path.
async fn probe_land(
    repo_dir: Option<&str>,
    github_merged: bool,
    github_state: &str,
    head_sha: &str,
    base_ref: &str,
) -> ContentProof {
    if github_merged || github_state != "closed" {
        return ContentProof::NotAttempted;
    }
    let Some(dir) = repo_dir else {
        return ContentProof::Unknown(LandUnknown::NotARepo);
    };
    let (head_sha, base_ref) = (head_sha.trim(), base_ref.trim());
    if head_sha.is_empty() {
        return ContentProof::Unknown(LandUnknown::HeadObjectMissing);
    }
    if base_ref.is_empty() {
        return ContentProof::Unknown(LandUnknown::NoBaseRef);
    }

    let head_rev = format!("{head_sha}^{{commit}}");
    let base_rev = format!("origin/{base_ref}^{{commit}}");

    match git_rev_present(dir, &head_rev).await {
        Some(true) => {}
        Some(false) => return ContentProof::Unknown(LandUnknown::HeadObjectMissing),
        None => return ContentProof::Unknown(LandUnknown::NotARepo),
    }
    match git_rev_present(dir, &base_rev).await {
        Some(true) => {}
        Some(false) => return ContentProof::Unknown(LandUnknown::NoBaseRef),
        None => return ContentProof::Unknown(LandUnknown::NotARepo),
    }

    match git_is_ancestor(dir, &head_rev, &base_rev).await {
        Some(true) => ContentProof::Ancestor,
        Some(false) => classify_negative(
            git_commit_ts(dir, &head_rev).await,
            git_commit_ts(dir, &base_rev).await,
        ),
        None => ContentProof::Unknown(LandUnknown::NotARepo),
    }
}

/// Classify a git "not an ancestor" answer given both commits' committer
/// timestamps. Pure.
///
/// A base tip that is no newer than the PR head cannot contain that head, so
/// the negative says nothing about whether the PR landed — it says the local
/// ref is stale. Only a base tip strictly NEWER than the head makes the
/// negative evidence.
fn classify_negative(head_ts: Option<i64>, base_ts: Option<i64>) -> ContentProof {
    match (head_ts, base_ts) {
        (Some(head), Some(base)) if base > head => ContentProof::NotAncestor,
        (Some(_), Some(_)) => ContentProof::Unknown(LandUnknown::RefStale),
        // git could not date an object it had just verified: a broken repo,
        // not a negative.
        _ => ContentProof::Unknown(LandUnknown::NotARepo),
    }
}

/// `git -C <dir> rev-parse --verify --quiet <rev>` — `Some(true)` present,
/// `Some(false)` absent (git's quiet exit 1), `None` for any other exit (128 =
/// not a git repository / fatal) or a spawn failure. "git could not answer" is
/// never reported as "absent".
async fn git_rev_present(dir: &str, rev: &str) -> Option<bool> {
    // `None` from `run_git` (spawn failure OR timeout) is "git could not
    // answer", which is exactly the `None` this function already means.
    let out = run_git(dir, &["rev-parse", "--verify", "--quiet", rev]).await?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => {
            debug!(
                "session-pr-reconciler: rev-parse {rev} in {dir} errored: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            None
        }
    }
}

/// `git -C <dir> merge-base --is-ancestor <ancestor> <descendant>` —
/// `Some(true)`/`Some(false)` for git's 0/1 answers, `None` for any OTHER exit
/// (128 = missing object / not a repo) or a spawn failure. A non-answer is
/// never reported as `false`.
async fn git_is_ancestor(dir: &str, ancestor: &str, descendant: &str) -> Option<bool> {
    let out = run_git(dir, &["merge-base", "--is-ancestor", ancestor, descendant]).await?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => {
            debug!(
                "session-pr-reconciler: is-ancestor {ancestor} {descendant} in {dir} errored: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            None
        }
    }
}

/// Committer timestamp (epoch seconds) of a commit, or `None` if git fails.
async fn git_commit_ts(dir: &str, rev: &str) -> Option<i64> {
    let out = run_git(dir, &["log", "-1", "--format=%ct", rev]).await?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<i64>()
        .ok()
}

/// Parse a GitHub RFC3339 timestamp to `DateTime<Utc>`.
fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Resolve `owner/name` + the branch→session-trailer map for a repo toplevel.
async fn resolve_repo(toplevel: &str) -> Option<RepoInfo> {
    let remote = git_remote_url(toplevel).await?;
    let (owner, repo) = parse_owner_repo(&remote)?;
    let branches = branch_trailers(toplevel).await;
    Some(RepoInfo {
        owner,
        repo,
        branches,
    })
}

/// `git -C <dir> rev-parse --show-toplevel` — the repo root, or `None` if the
/// dir isn't inside a git work tree.
async fn git_toplevel(dir: &str) -> Option<String> {
    let out = run_git(dir, &["rev-parse", "--show-toplevel"]).await?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `git -C <dir> remote get-url origin`.
async fn git_remote_url(dir: &str) -> Option<String> {
    let out = run_git(dir, &["remote", "get-url", "origin"]).await?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Parse `owner/name` from a GitHub remote URL — SSH (`git@github.com:o/n.git`),
/// HTTPS (`https://github.com/o/n(.git)`), or `x-access-token@` forms.
fn parse_owner_repo(remote: &str) -> Option<(String, String)> {
    let s = remote.trim();
    // Take everything after the host separator: `:` for scp-like SSH,
    // the path after `github.com/` for URL forms.
    let path = if let Some(idx) = s.find("github.com") {
        let rest = &s[idx + "github.com".len()..];
        rest.trim_start_matches([':', '/'])
    } else {
        return None;
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let name = parts.next()?.trim();
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

/// Read every branch's HEAD sha + `Session-Id` trailer value(s) in one
/// `git for-each-ref`, over BOTH `refs/heads/` and `refs/remotes/origin/`.
///
/// The remote half matters because the common shape after opening a PR is
/// push-then-delete-the-local-branch (and coord's land flow deletes it for
/// you): scanning only local heads dropped exactly those PRs. This is ONE
/// `for-each-ref` invocation with two patterns, not extra GitHub fan-out — the
/// `Session-Id` trailer filter still runs entirely locally, before any API call.
///
/// Line format: `<sha> <full refname> <trailer values…>` (git ref names contain
/// no whitespace, so the first two whitespace fields are unambiguous; the
/// remainder is the space-joined trailer value set). The FULL refname is used
/// rather than `refname:short` so local and remote are distinguishable without
/// guessing at an `origin/` prefix.
async fn branch_trailers(dir: &str) -> Vec<BranchTrailer> {
    let out = match run_git(
        dir,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname) %(trailers:key=Session-Id,valueonly,separator=%x20)",
            "refs/heads/",
            "refs/remotes/origin/",
        ],
    )
    .await
    {
        Some(o) if o.status.success() => o,
        Some(o) => {
            debug!(
                "session-pr-reconciler: for-each-ref in {dir} failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return Vec::new();
        }
        // Spawn failure or timeout — already logged by `run_git`.
        None => return Vec::new(),
    };

    normalize_branch_trailers(String::from_utf8_lossy(&out.stdout).lines())
}

/// Parse + dedupe `for-each-ref` output into the branch set to attribute. Pure
/// — unit-tested.
///
/// A branch present as BOTH a local head and `origin/<name>` must produce ONE
/// entry, not two (two entries would double every `list_prs_for_head` call for
/// it). Local wins: it is the ref the session actually committed on, so its
/// HEAD sha is the one the PR-head verification compares against.
/// `refs/remotes/origin/HEAD` is dropped — it is a symbolic alias for the
/// default branch, not a branch of its own.
fn normalize_branch_trailers<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<BranchTrailer> {
    let mut out: Vec<BranchTrailer> = Vec::new();
    let mut remote: Vec<BranchTrailer> = Vec::new();
    for line in lines {
        let Some((sha, refname, session_ids)) = split_for_each_ref_line(line) else {
            continue;
        };
        if let Some(branch) = refname.strip_prefix("refs/heads/") {
            out.push(BranchTrailer {
                branch: branch.to_string(),
                sha,
                session_ids,
            });
        } else if let Some(branch) = refname.strip_prefix("refs/remotes/origin/") {
            if branch == "HEAD" || branch.is_empty() {
                continue;
            }
            remote.push(BranchTrailer {
                branch: branch.to_string(),
                sha,
                session_ids,
            });
        }
    }
    for bt in remote {
        if !out.iter().any(|l| l.branch == bt.branch) {
            out.push(bt);
        }
    }
    out
}

/// Split one `for-each-ref` line into `(sha, full refname, trailer values)`.
/// `None` for a line missing the sha/refname fields.
fn split_for_each_ref_line(line: &str) -> Option<(String, &str, Vec<String>)> {
    let mut it = line.split_whitespace();
    let sha = it.next()?.to_string();
    let refname = it.next()?;
    let session_ids: Vec<String> = it.map(|s| s.to_string()).collect();
    Some((sha, refname, session_ids))
}

/// Read the `Session-Id` trailer value(s) on a specific commit, or `None` if
/// the object isn't present locally (or git fails).
async fn read_session_trailers(dir: &str, sha: &str) -> Option<Vec<String>> {
    let out = run_git(
        dir,
        &[
            "log",
            "-1",
            "--format=%(trailers:key=Session-Id,valueonly,separator=%x20)",
            sha,
        ],
    )
    .await?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(s.split_whitespace().map(|t| t.to_string()).collect())
}

/// Resolve a GitHub token, runner-locally: env `GITHUB_TOKEN` / `GH_TOKEN`
/// first, then `gh auth token` (the operator's authenticated GitHub CLI — the
/// same credential the interactive `gh pr create` sessions this feature serves
/// already use). `None` when no source yields a non-empty token.
pub(crate) async fn resolve_github_token() -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    // Under a HARD timeout. `gh auth token` has been observed blocked for
    // hours on this fleet; awaited bare it does not fail the tick, it suspends
    // it forever — the reconciler stops producing any output at all, which is
    // exactly how a total outage came to be mistaken for "the fix does not
    // work". A timeout here yields the same `None` ("no token") the error path
    // already returns, so the tick fails FAST and visibly instead of hanging.
    // `tokio_no_window` for the same reason `run_git` uses it — this one runs
    // once per tick even when every git call is cache-served.
    let mut cmd = crate::process_helpers::tokio_no_window("gh");
    cmd.args(["auth", "token"]);
    let out = output_with_timeout(&mut cmd, GH_TOKEN_TIMEOUT, "gh auth token").await?;
    if !out.status.success() {
        return None;
    }
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tok.is_empty() {
        None
    } else {
        Some(tok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_owner_repo_handles_ssh_https_and_token_forms() {
        for (url, want) in [
            (
                "git@github.com:qontinui/qontinui-runner.git",
                ("qontinui", "qontinui-runner"),
            ),
            (
                "https://github.com/qontinui/qontinui-runner.git",
                ("qontinui", "qontinui-runner"),
            ),
            (
                "https://github.com/qontinui/qontinui-runner",
                ("qontinui", "qontinui-runner"),
            ),
            (
                "https://x-access-token:TOKEN@github.com/o/n.git",
                ("o", "n"),
            ),
            ("ssh://git@github.com/o/n.git", ("o", "n")),
        ] {
            let got = parse_owner_repo(url).unwrap();
            assert_eq!((got.0.as_str(), got.1.as_str()), want, "url={url}");
        }
        // Non-GitHub remotes are ignored.
        assert!(parse_owner_repo("https://gitlab.com/o/n.git").is_none());
        assert!(parse_owner_repo("").is_none());
    }

    /// The pinning label test, updated (not deleted) for the land cascade: its
    /// first argument is now the LAND verdict, not GitHub's `merged` boolean —
    /// so an ff-landed PR (`merged=false`, `state=closed`, landed=true) labels
    /// `merged` instead of `closed`. That row is the G3 regression guard.
    #[test]
    fn pr_state_label_prefers_landed_then_closed_then_open() {
        assert_eq!(pr_state_label(true, "closed"), "merged");
        assert_eq!(pr_state_label(true, "open"), "merged");
        assert_eq!(pr_state_label(false, "closed"), "closed");
        assert_eq!(pr_state_label(false, "open"), "open");
    }

    // ---- The land-signal cascade (D3 / G3) --------------------------------

    /// THE HEADLINE CASE. A coord fast-forward land: GitHub reports
    /// `merged=false, state=closed`, but the head commit IS on `origin/<base>`
    /// — the majority of this fleet's landings. Must read as landed via
    /// `ff-land`.
    #[test]
    fn ff_landed_pr_is_landed_with_the_ff_land_signal() {
        let verdict = land_verdict(false, "closed", false, ContentProof::Ancestor);
        assert!(verdict.landed, "an ff-land is a land");
        assert_eq!(verdict.signal, "ff-land");
        assert_eq!(verdict.reason, None);
        // …and it reaches the projection as a landed row, labelled `merged`.
        let closed_at = parse_ts("2026-08-15T10:00:00Z");
        let status = session_pr_status(&verdict, "closed", None, closed_at);
        assert!(status.landed);
        assert_eq!(status.pr_state.as_deref(), Some("merged"));
        assert_eq!(status.land_signal.as_deref(), Some("ff-land"));
        assert_eq!(status.land_reason, None);
        // No GitHub merge stamp exists for an ff-land — the close time is the
        // honest land time.
        assert_eq!(status.merged_at, None);
        assert_eq!(status.landed_at, closed_at);
    }

    /// THE REGRESSION GUARD for the `closed, not landed` chip on landed PRs.
    ///
    /// The ancestor test evaluated and said no. That is NOT a negative land
    /// verdict: coord's majority land shape rebases the PR's commits and
    /// pushes the replayed shas, so the PR's own head sha is never on
    /// `origin/<base>` after an ordinary land, and this branch is exactly what
    /// a landed PR produces. Until 2026-08-26 it recorded `not-landed`, and
    /// the Terminal dropdown printed `closed, not landed` on shipped work.
    ///
    /// It must be `land-unknown` with a reason. That is a WEAKER claim than
    /// before, deliberately: the honest answer here is "ancestry cannot tell",
    /// and the `coord:landed` chip (below) is what recovers the positive.
    #[test]
    fn non_ancestor_is_land_unknown_never_a_confident_negative() {
        let verdict = land_verdict(false, "closed", false, ContentProof::NotAncestor);
        assert!(!verdict.landed);
        assert_ne!(
            verdict.signal, "not-landed",
            "a failed ancestry test is not evidence of unlanded - coord-ff-lands.md"
        );
        assert_eq!(verdict.signal, "land-unknown");
        assert_eq!(verdict.reason, Some("rebase_land_or_abandoned"));
        let status = session_pr_status(&verdict, "closed", None, parse_ts("2026-08-15T10:00:00Z"));
        assert_eq!(status.pr_state.as_deref(), Some("closed"));
        assert_eq!(status.land_signal.as_deref(), Some("land-unknown"));
        assert_eq!(
            status.land_reason.as_deref(),
            Some("rebase_land_or_abandoned")
        );
        // Unknown carries no land stamp - we did not establish a land.
        assert_eq!(status.landed_at, None);
    }

    /// The ONLY confident negative left: GitHub still reports the PR open, so
    /// no probe was attempted and none was needed. An open PR has not landed.
    #[test]
    fn open_pr_is_the_only_confident_not_landed() {
        let verdict = land_verdict(false, "open", false, ContentProof::NotAttempted);
        assert!(!verdict.landed);
        assert_eq!(verdict.signal, "not-landed");
        assert_eq!(verdict.reason, None);
        let status = session_pr_status(&verdict, "open", None, None);
        assert_eq!(status.pr_state.as_deref(), Some("open"));
        assert_eq!(status.landed_at, None);
    }

    /// SIGNAL 3. coord's `coord:landed` chip rescues every shape the local
    /// content proof structurally cannot reach - which on this fleet is the
    /// common one. Each of these probe results would otherwise have been an
    /// unknown (or, before 2026-08-26, a false `not-landed`).
    #[test]
    fn coord_landed_label_lands_every_shape_the_content_proof_cannot_reach() {
        for proof in [
            // The rebase-land shape: replayed shas, head not on base.
            ContentProof::NotAncestor,
            // The usual post-land state - branch deleted, head object pruned.
            ContentProof::Unknown(LandUnknown::HeadObjectMissing),
            ContentProof::Unknown(LandUnknown::RefStale),
            ContentProof::Unknown(LandUnknown::NotARepo),
            ContentProof::Unknown(LandUnknown::NoBaseRef),
        ] {
            let verdict = land_verdict(false, "closed", true, proof);
            assert!(verdict.landed, "coord's own chip is a land: {proof:?}");
            assert_eq!(verdict.signal, "coord-label");
            assert_eq!(verdict.reason, None);

            let closed_at = parse_ts("2026-08-20T08:41:31Z");
            let status = session_pr_status(&verdict, "closed", None, closed_at);
            assert!(status.landed);
            assert_eq!(status.pr_state.as_deref(), Some("merged"));
            assert_eq!(status.land_signal.as_deref(), Some("coord-label"));
            // No GitHub merge stamp on this shape; the close time is the land.
            assert_eq!(status.landed_at, closed_at);
        }
    }

    /// Signal precedence: the local CONTENT proof outranks the chip, so a PR
    /// that is provably on the base reports `ff-land` even when coord also
    /// labelled it. Both say landed - the recorded WHY is the strongest one.
    #[test]
    fn content_proof_outranks_the_coord_chip() {
        let verdict = land_verdict(false, "closed", true, ContentProof::Ancestor);
        assert!(verdict.landed);
        assert_eq!(verdict.signal, "ff-land");
    }

    /// The chip is POSITIVE-ONLY. coord applies it best-effort and only after
    /// its explanatory comment posts, so its absence establishes nothing - an
    /// unlabelled PR whose probe could not be evaluated stays UNKNOWN, and
    /// must not be demoted to a confident negative by the absent label.
    #[test]
    fn an_absent_coord_label_proves_nothing() {
        let verdict = land_verdict(
            false,
            "closed",
            false,
            ContentProof::Unknown(LandUnknown::HeadObjectMissing),
        );
        assert!(!verdict.landed);
        assert_eq!(verdict.signal, "land-unknown");
        assert_eq!(verdict.reason, Some("head_object_missing"));
    }

    /// THE SHIP-BLOCKER FROM REVIEW. coord's chip must NOT override GitHub's
    /// live statement that a PR is still OPEN.
    ///
    /// coord's phantom empty-diff sweep applies `coord:landed` off a
    /// `git merge-tree` CONTENT-equality proof, and it does so on the
    /// `CommentedButCloseFailed` arm — where its own log records that the
    /// close PATCH failed and the PR is still open. Taking the chip there
    /// would write `pr_state='merged'`, `landed_at=NULL` for a live PR, the
    /// dropdown would show `landed (coord)`, and `allLanded` could go green
    /// while the work is still in flight. Two live signals disagreeing is an
    /// UNKNOWN, and it says which two.
    #[test]
    fn coord_chip_never_overrides_a_pr_github_still_reports_open() {
        let verdict = land_verdict(false, "open", true, ContentProof::NotAttempted);
        assert!(
            !verdict.landed,
            "coord's chip must not out-vote GitHub's live open state"
        );
        assert_eq!(verdict.signal, "land-unknown");
        assert_eq!(verdict.reason, Some("coord_chip_on_open_pr"));

        // And it must not reach the projection as a landed row.
        let status = session_pr_status(&verdict, "open", None, None);
        assert!(!status.landed);
        assert_eq!(status.pr_state.as_deref(), Some("open"));
        assert_eq!(status.landed_at, None);
    }

    /// The same guard, stated as the property rather than the instance: for
    /// EVERY non-closed state, the chip yields an unknown, never a land.
    #[test]
    fn the_coord_chip_is_taken_only_on_a_closed_pr() {
        for state in ["open", "unknown", "", "draft"] {
            let verdict = land_verdict(false, state, true, ContentProof::NotAttempted);
            assert!(!verdict.landed, "state={state}");
            assert_eq!(verdict.signal, "land-unknown", "state={state}");
        }
        // Closed is the one state that takes it.
        let closed = land_verdict(false, "closed", true, ContentProof::NotAttempted);
        assert!(closed.landed);
        assert_eq!(closed.signal, "coord-label");
    }

    /// An UNOBSERVED state is not an evaluated negative. Both API parsers
    /// default a missing or non-string `state` to the literal `"unknown"`, so
    /// a partial payload reaches the cascade as neither `open` nor `closed`.
    /// Now that `not-landed` has exactly one producer, that producer must not
    /// also absorb "we never saw the state".
    #[test]
    fn an_unobserved_pr_state_is_unknown_not_a_confident_negative() {
        let verdict = land_verdict(false, "unknown", false, ContentProof::NotAttempted);
        assert!(!verdict.landed);
        assert_ne!(verdict.signal, "not-landed");
        assert_eq!(verdict.signal, "land-unknown");
        assert_eq!(verdict.reason, Some("pr_state_unobserved"));
    }

    /// GitHub's own merge still outranks the chip - signal 1 is first.
    #[test]
    fn github_merge_outranks_the_coord_chip() {
        let verdict = land_verdict(true, "closed", true, ContentProof::NotAttempted);
        assert_eq!(verdict.signal, "github-merge");
    }

    /// The probe COULD NOT BE EVALUATED — a missing base ref, a pruned head
    /// object, a stale local ref, or no local checkout. Every one of these is
    /// `land-unknown` WITH a reason and must never collapse into the confident
    /// `not-landed` above; that collapse is the `silent-empty-is-unknown`
    /// defect applied to the land verdict.
    #[test]
    fn unevaluable_probe_is_land_unknown_with_a_reason_never_not_landed() {
        for (unknown, want_reason) in [
            (LandUnknown::NoBaseRef, "no_base_ref"),
            (LandUnknown::HeadObjectMissing, "head_object_missing"),
            (LandUnknown::RefStale, "ref_stale"),
            (LandUnknown::NotARepo, "not_a_repo"),
        ] {
            let verdict = land_verdict(false, "closed", false, ContentProof::Unknown(unknown));
            assert!(!verdict.landed, "unknown is not a land: {want_reason}");
            assert_eq!(verdict.signal, "land-unknown", "reason={want_reason}");
            assert_ne!(
                verdict.signal, "not-landed",
                "an unevaluable probe must not become a confident negative"
            );
            assert_eq!(verdict.reason, Some(want_reason));

            let status = session_pr_status(&verdict, "closed", None, None);
            assert_eq!(status.land_signal.as_deref(), Some("land-unknown"));
            assert_eq!(status.land_reason.as_deref(), Some(want_reason));
            assert!(!status.landed);
            assert_eq!(status.landed_at, None);
        }
    }

    /// Signal 1 still wins, and wins outright — GitHub's merge needs no local
    /// proof, so even an unevaluable probe cannot demote it.
    #[test]
    fn github_merge_wins_the_cascade_regardless_of_the_probe() {
        for proof in [
            ContentProof::NotAttempted,
            ContentProof::Ancestor,
            ContentProof::NotAncestor,
            ContentProof::Unknown(LandUnknown::HeadObjectMissing),
        ] {
            let verdict = land_verdict(true, "closed", false, proof);
            assert!(verdict.landed);
            assert_eq!(verdict.signal, "github-merge");
            assert_eq!(verdict.reason, None);
        }
        let merged_at = parse_ts("2026-08-14T09:00:00Z");
        let status = session_pr_status(
            &land_verdict(true, "closed", false, ContentProof::NotAttempted),
            "closed",
            merged_at,
            parse_ts("2026-08-14T09:00:05Z"),
        );
        assert_eq!(status.pr_state.as_deref(), Some("merged"));
        // GitHub's merge stamp is preferred over the close stamp.
        assert_eq!(status.landed_at, merged_at);
    }

    /// Two DIFFERENT unknowns, and the split still earns its two subprocesses.
    /// A "not an ancestor" answer against a base tip strictly NEWER than the
    /// head is a rebase-land-or-abandoned unknown; against an equal or older
    /// tip the ref could not have contained the head at all, which is a
    /// stale-ref unknown. Neither is a land verdict - the cascade turns both
    /// into `land-unknown` - but they are different diagnoses, and the
    /// dropdown reports the reason it was actually given. (Measured at plan-vet time: this repo and the running runner
    /// were both 19 commits behind `origin/main`.)
    #[test]
    fn stale_base_ref_turns_a_negative_into_unknown() {
        // Base tip newer than the head ⇒ the negative is real.
        assert_eq!(
            classify_negative(Some(1_000), Some(2_000)),
            ContentProof::NotAncestor
        );
        // Base tip OLDER than the head ⇒ it could not possibly contain it.
        assert_eq!(
            classify_negative(Some(2_000), Some(1_000)),
            ContentProof::Unknown(LandUnknown::RefStale)
        );
        // Same second, distinct commits ⇒ inconclusive, so unknown.
        assert_eq!(
            classify_negative(Some(1_000), Some(1_000)),
            ContentProof::Unknown(LandUnknown::RefStale)
        );
        // git could not date an object it had just verified ⇒ broken repo.
        assert_eq!(
            classify_negative(None, Some(1_000)),
            ContentProof::Unknown(LandUnknown::NotARepo)
        );
        assert_eq!(
            classify_negative(Some(1_000), None),
            ContentProof::Unknown(LandUnknown::NotARepo)
        );
    }

    /// The probe spends NO subprocess when the answer is already known, and
    /// degrades to a reasoned unknown when there is nothing local to read.
    #[tokio::test]
    async fn probe_skips_work_it_does_not_need_and_never_guesses() {
        // GitHub already said merged — signal 1 wins, nothing to prove.
        assert_eq!(
            probe_land(Some("D:/nonexistent"), true, "closed", "abc", "main").await,
            ContentProof::NotAttempted
        );
        // Still open on GitHub: GitHub closes a PR once its commits reach the
        // base branch, so `open` is an evaluated negative.
        assert_eq!(
            probe_land(Some("D:/nonexistent"), false, "open", "abc", "main").await,
            ContentProof::NotAttempted
        );
        // No local checkout for this PR's repo ⇒ unknown, not "not landed".
        assert_eq!(
            probe_land(None, false, "closed", "abc", "main").await,
            ContentProof::Unknown(LandUnknown::NotARepo)
        );
        // A PR payload with no head sha / no base ref is unevaluable, and each
        // gap names itself.
        assert_eq!(
            probe_land(Some("D:/nonexistent"), false, "closed", "  ", "main").await,
            ContentProof::Unknown(LandUnknown::HeadObjectMissing)
        );
        assert_eq!(
            probe_land(Some("D:/nonexistent"), false, "closed", "abc", "").await,
            ContentProof::Unknown(LandUnknown::NoBaseRef)
        );
    }

    /// Parse exactly one `for-each-ref` line through the real normalizer.
    fn one(line: &str) -> BranchTrailer {
        normalize_branch_trailers([line].into_iter())
            .pop()
            .unwrap_or_else(|| panic!("line produced no branch: {line:?}"))
    }

    #[test]
    fn for_each_ref_lines_split_into_sha_branch_and_sessions() {
        // No trailer.
        let bt = one("abc123 refs/heads/main");
        assert_eq!(bt.sha, "abc123");
        assert_eq!(bt.branch, "main");
        assert!(bt.session_ids.is_empty());

        // One Session-Id trailer.
        let bt = one("deadbeef refs/heads/feat/x 11111111-1111-1111-1111-111111111111");
        assert_eq!(bt.branch, "feat/x");
        assert_eq!(bt.session_ids, vec!["11111111-1111-1111-1111-111111111111"]);

        // Two (branch touched under two Session-Id trailers on one commit).
        let bt = one("sha refs/heads/b/1 aaa bbb");
        assert_eq!(bt.session_ids, vec!["aaa", "bbb"]);

        // Malformed (sha only) → no refname field.
        assert!(normalize_branch_trailers(["loneword"].into_iter()).is_empty());
        assert!(normalize_branch_trailers([""].into_iter()).is_empty());
        // A ref under neither pattern is not a branch.
        assert!(normalize_branch_trailers(["sha refs/tags/v1"].into_iter()).is_empty());
    }

    /// Exactly one entry for a branch that exists locally AND on `origin`, and
    /// the LOCAL sha is the one kept — it is the ref the session committed on,
    /// so it is what the PR-head verification must compare against.
    #[test]
    fn branch_trailers_dedupe_local_and_remote_keeping_local() {
        let got = normalize_branch_trailers(
            [
                "localsha refs/heads/feat/dup session-a",
                "localsha2 refs/heads/only-local session-a",
                "remotesha refs/remotes/origin/feat/dup session-a",
                "remotesha2 refs/remotes/origin/only-remote session-b",
                // Symbolic alias for the default branch — never a branch.
                "headsha refs/remotes/origin/HEAD",
            ]
            .into_iter(),
        );
        let names: Vec<&str> = got.iter().map(|b| b.branch.as_str()).collect();
        assert_eq!(names, vec!["feat/dup", "only-local", "only-remote"]);
        assert_eq!(
            got[0].sha, "localsha",
            "local ref must win the dedupe, not the remote copy"
        );
        // The remote-only branch is the whole point of scanning refs/remotes:
        // a pushed-then-deleted local branch is still attributable.
        assert_eq!(got[2].sha, "remotesha2");
        assert_eq!(got[2].session_ids, vec!["session-b"]);
    }

    #[test]
    fn parse_ts_roundtrips_github_rfc3339() {
        let dt = parse_ts("2026-07-13T01:02:03Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-07-13T01:02:03+00:00");
        assert!(parse_ts("not-a-date").is_none());
    }

    /// The whole point of the cross-tick cache: N sessions sharing a checkout
    /// resolve the repo set ONCE, not once per session per 30s tick.
    #[test]
    fn repo_set_cache_serves_repeats_and_forgets_failures() {
        let mut cache = RepoSetCache::default();
        assert!(cache.get("D:/repo/sub").is_none(), "cold cache resolves");

        cache.insert("D:/repo/sub".to_string(), vec!["D:/repo".to_string()]);
        assert_eq!(cache.get("D:/repo/sub"), Some(&["D:/repo".to_string()][..]));
        // A second session in the same cwd costs no subprocess.
        assert_eq!(cache.get("D:/repo/sub"), Some(&["D:/repo".to_string()][..]));
        // A different cwd is still a miss.
        assert!(cache.get("D:/other").is_none());

        // An EMPTY set is a cached ANSWER, not a miss — that is what keeps the
        // depth-1 directory scan off the 30s tick for a cwd with no repos.
        cache.insert("D:/empty".to_string(), Vec::new());
        assert_eq!(cache.get("D:/empty"), Some(&[][..]));

        // A cwd that stops resolving (checkout moved/deleted) is forgotten so
        // it re-resolves rather than serving a stale root forever.
        cache.invalidate("D:/repo/sub");
        assert!(cache.get("D:/repo/sub").is_none());
    }

    /// Bounded: the cache must not grow with churning session cwds.
    #[test]
    fn repo_set_cache_is_bounded() {
        let mut cache = RepoSetCache::default();
        for i in 0..(TOPLEVEL_CACHE_MAX * 2) {
            cache.insert(format!("D:/wd/{i}"), vec!["D:/repo".to_string()]);
        }
        assert!(
            cache.entries.len() <= TOPLEVEL_CACHE_MAX,
            "cache grew past its bound: {}",
            cache.entries.len()
        );
    }

    // ---------------------------------------------------------------------
    // Repo-SET resolution. The defect these cover: the operator's terminal sits
    // at the workspace parent (`D:\qontinui-root`), which HOLDS the clones but
    // is not itself a repo, so the old `git_toplevel(cwd)`-or-skip gate dropped
    // that session from every tick and its PR counts sat at 0 forever.
    // ---------------------------------------------------------------------

    /// Make `<parent>/<name>` look like a git repo to the pure scanner (a
    /// `.git` entry is exactly what it tests for) without paying `git init`.
    fn fake_repo(parent: &std::path::Path, name: &str) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    fn scanned_names(parent: &std::path::Path) -> Vec<String> {
        candidate_child_repos(parent)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect()
    }

    /// (b) A non-repo parent holding N child clones yields all N.
    #[test]
    fn child_scan_finds_every_depth_one_repo_under_a_non_repo_parent() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["qontinui-runner", "qontinui-web", "multistate"] {
            fake_repo(tmp.path(), name);
        }
        // A plain directory that is NOT a repo is not a candidate.
        std::fs::create_dir_all(tmp.path().join("knowledge-base")).unwrap();
        // Depth 1 only — a repo nested two levels down is NOT picked up.
        fake_repo(&tmp.path().join("knowledge-base"), "nested-repo");

        assert_eq!(
            scanned_names(tmp.path()),
            vec!["multistate", "qontinui-runner", "qontinui-web"],
            "sorted by name so the resolved set is stable across ticks"
        );
    }

    /// (c) Build/worktree scratch is never a session's source repo.
    #[test]
    fn child_scan_excludes_the_skip_list_and_dot_directories() {
        let tmp = tempfile::tempdir().unwrap();
        fake_repo(tmp.path(), "real-repo");
        for name in REPO_SCAN_SKIP_DIRS {
            fake_repo(tmp.path(), name);
        }
        // Any other dotted directory is tooling, not a checkout.
        fake_repo(tmp.path(), ".dev-logs");

        assert_eq!(scanned_names(tmp.path()), vec!["real-repo"]);
    }

    /// (d) The scanner REPORTS every candidate; capping is the resolver's job.
    /// Truncating here made truncation unobservable, which is how "exactly at
    /// the cap" came to warn about a remainder that did not exist.
    #[test]
    fn child_scan_reports_every_candidate_and_leaves_capping_to_the_caller() {
        let tmp = tempfile::tempdir().unwrap();
        let over_cap = REPO_SET_MAX + 1;
        for i in 0..over_cap {
            fake_repo(tmp.path(), &format!("repo-{i:03}"));
        }
        assert_eq!(candidate_child_repos(tmp.path()).len(), over_cap);
    }

    /// The cap must sit ABOVE a real workspace, not inside it.
    ///
    /// It was 32 while `D:\qontinui-root` held 38 depth-1 clones, so five repos
    /// were dropped from EVERY tick — and since candidates arrive name-sorted,
    /// the loss was alphabetically biased: the tail (`ui-bridge*`,
    /// `wrappers-registry`, `qontinui-workflow-*`) could never have a PR
    /// attributed, while attribution for `qontinui-runner` worked only because
    /// it happens to sort 23rd. A cap a normal workspace crosses is silent data
    /// loss, not a safety valve.
    #[test]
    fn repo_set_cap_clears_a_real_workspace_with_room_to_grow() {
        /// Depth-1 git repos counted in `D:\qontinui-root` when this was found.
        const OBSERVED_WORKSPACE_REPOS: usize = 38;
        assert!(
            REPO_SET_MAX > OBSERVED_WORKSPACE_REPOS,
            "REPO_SET_MAX ({REPO_SET_MAX}) is at or below the {OBSERVED_WORKSPACE_REPOS} repos \
             this workspace already held — repos would be dropped from every tick"
        );
        assert!(
            REPO_SET_MAX >= OBSERVED_WORKSPACE_REPOS * 3,
            "REPO_SET_MAX ({REPO_SET_MAX}) leaves no room for the workspace to grow past \
             {OBSERVED_WORKSPACE_REPOS} repos"
        );
    }

    /// When the cap binds, the survivors are chosen by RECENCY, not by name —
    /// so an actively worked repo is never the one silently dropped. The cwd
    /// (index 0) is pinned regardless of how stale it looks.
    #[test]
    fn recency_prioritisation_pins_the_cwd_and_promotes_the_freshest_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = fake_repo(tmp.path(), "aaa-cwd");
        // `zzz` sorts LAST by name, so it can only survive a cap by recency.
        let stale = fake_repo(tmp.path(), "bbb-stale");
        let fresh = fake_repo(tmp.path(), "zzz-fresh");

        // Make `fresh` unambiguously newer than `stale`. Filesystem mtime
        // granularity can be coarse, so write rather than trust creation order.
        std::fs::write(stale.join(".git").join("HEAD"), "ref: refs/heads/x\n").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(fresh.join(".git").join("HEAD"), "ref: refs/heads/x\n").unwrap();

        let mut candidates = vec![cwd.clone(), stale.clone(), fresh.clone()];
        prioritise_by_recency(&mut candidates);

        assert_eq!(candidates[0], cwd, "the session's own cwd is never moved");
        assert_eq!(
            candidates[1], fresh,
            "the most recently touched repo outranks the alphabetically earlier stale one"
        );
        assert_eq!(candidates[2], stale);
    }

    /// Ordering stays deterministic when nothing distinguishes the candidates:
    /// equal (or unreadable) mtimes fall back to name order.
    #[test]
    fn recency_prioritisation_falls_back_to_name_order_for_ties() {
        let missing_a = PathBuf::from("Z:/does-not-exist/alpha");
        let missing_b = PathBuf::from("Z:/does-not-exist/beta");
        let cwd = PathBuf::from("Z:/does-not-exist");
        let mut candidates = vec![cwd.clone(), missing_b.clone(), missing_a.clone()];
        prioritise_by_recency(&mut candidates);
        assert_eq!(candidates, vec![cwd, missing_a, missing_b]);
        // Degenerate inputs must not panic.
        prioritise_by_recency(&mut []);
        prioritise_by_recency(&mut [PathBuf::from("Z:/x")]);
    }

    /// A linked worktree is scanned like a clone, but is identifiable as one:
    /// its `.git` is a FILE pointing into the canonical clone. That is the
    /// tie-break the dedupe uses.
    #[test]
    fn child_scan_sees_linked_worktrees_and_tells_them_from_clones() {
        let tmp = tempfile::tempdir().unwrap();
        let clone = fake_repo(tmp.path(), "qontinui-runner");
        let worktree = tmp.path().join("qontinui-runner-wt-appcmd");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            "gitdir: ../qontinui-runner/.git/worktrees/appcmd\n",
        )
        .unwrap();

        assert_eq!(
            scanned_names(tmp.path()),
            vec!["qontinui-runner", "qontinui-runner-wt-appcmd"],
            "both shapes are candidates — a worktree's branches are the session's too"
        );
        assert!(!is_linked_worktree(&clone), "a `.git` DIR is a clone");
        assert!(
            is_linked_worktree(&worktree),
            "a `.git` FILE is a linked worktree"
        );
        // A directory with no `.git` at all is not a worktree either.
        assert!(!is_linked_worktree(tmp.path()));
    }

    /// (e) Neither a repo nor holding repos ⇒ EMPTY, which the tick records as
    /// "scanned, found nothing" rather than silently skipping the session.
    #[test]
    fn child_scan_of_a_bare_directory_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("notes")).unwrap();
        assert!(candidate_child_repos(tmp.path()).is_empty());
        // A path that does not exist at all is empty, never a panic.
        assert!(candidate_child_repos(&tmp.path().join("nope")).is_empty());
    }

    /// `git init` a real repo — the resolver runs `git rev-parse` per candidate,
    /// so these cases need actual repositories, not just a `.git` directory.
    fn git_init(dir: &std::path::Path) -> bool {
        std::fs::create_dir_all(dir).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn canon(p: &str) -> String {
        std::fs::canonicalize(p)
            .map(|c| c.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| p.replace('\\', "/"))
            .trim_start_matches("//?/")
            .to_string()
    }

    /// (a) cwd IS a repo → unchanged behaviour: the toplevel is in the set.
    /// (b) cwd is a non-repo parent of N repos → all N are in the set.
    /// Both halves in one test so the two shapes share the `git init` cost.
    #[tokio::test]
    async fn resolve_repo_set_covers_the_cwd_repo_and_its_child_repos() {
        let tmp = tempfile::tempdir().unwrap();
        if !git_init(&tmp.path().join("alpha")) {
            eprintln!("git unavailable — skipping resolve_repo_set coverage");
            return;
        }
        git_init(&tmp.path().join("beta"));
        // Scratch that must never be attributed to a session.
        git_init(&tmp.path().join("qontinui-worktrees"));

        // (a) cwd is itself a repo.
        let alpha = tmp.path().join("alpha").to_string_lossy().to_string();
        let got = resolve_repo_set(&alpha).await;
        assert_eq!(got.len(), 1, "got {got:?}");
        assert_eq!(canon(&got[0]), canon(&alpha));

        // (b) cwd is the non-repo workspace parent — the P0 shape.
        let parent = tmp.path().to_string_lossy().to_string();
        let got: Vec<String> = resolve_repo_set(&parent)
            .await
            .iter()
            .map(|p| canon(p))
            .collect();
        assert_eq!(got.len(), 2, "expected alpha+beta only, got {got:?}");
        assert!(got.iter().any(|p| p.ends_with("/alpha")), "{got:?}");
        assert!(got.iter().any(|p| p.ends_with("/beta")), "{got:?}");
        assert!(
            !got.iter().any(|p| p.contains("qontinui-worktrees")),
            "worktree scratch must not be scanned: {got:?}"
        );
    }

    /// (e) A cwd that is neither a repo nor a parent of repos resolves to the
    /// EMPTY set — the state the tick warns about exactly once.
    #[tokio::test]
    async fn resolve_repo_set_of_a_repo_less_directory_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("notes")).unwrap();
        let got = resolve_repo_set(&tmp.path().to_string_lossy()).await;
        assert!(got.is_empty(), "got {got:?}");
    }

    /// Run a git subcommand in `dir`; `false` if git is unavailable or it fails.
    fn git_in(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// (d) The cap is a real TOTAL and it only reports drops that happened.
    /// "Exactly at the cap" keeps every repo and drops nothing; one more than
    /// the cap keeps `max` and reports the remainder — the distinction the
    /// inferred `len() >= REPO_SET_MAX` test could not make.
    #[tokio::test]
    async fn resolve_repo_set_caps_as_a_total_and_reports_only_real_drops() {
        let tmp = tempfile::tempdir().unwrap();
        if !git_init(&tmp.path().join("alpha")) {
            eprintln!("git unavailable — skipping resolve_repo_set cap coverage");
            return;
        }
        git_init(&tmp.path().join("beta"));
        git_init(&tmp.path().join("gamma"));
        let parent = tmp.path().to_string_lossy().to_string();

        // Exactly at the cap: all three kept, NOTHING dropped ⇒ no warning.
        let (repos, dropped) = resolve_repo_set_capped(&parent, 3).await;
        assert_eq!(repos.len(), 3, "got {repos:?}");
        assert_eq!(dropped, 0, "nothing was dropped, so nothing may be claimed");

        // Over the cap: capped, and the drop is counted.
        let (repos, dropped) = resolve_repo_set_capped(&parent, 2).await;
        assert_eq!(repos.len(), 2, "got {repos:?}");
        assert_eq!(dropped, 1);

        // The cwd's OWN toplevel occupies a slot — the ceiling used to be
        // REPO_SET_MAX + 1 because it was pushed before the cap loop.
        let alpha = tmp.path().join("alpha");
        git_init(&alpha.join("inner"));
        let (repos, dropped) = resolve_repo_set_capped(&alpha.to_string_lossy(), 1).await;
        assert_eq!(repos.len(), 1, "got {repos:?}");
        assert_eq!(canon(&repos[0]), canon(&alpha.to_string_lossy()));
        assert_eq!(dropped, 1, "the toplevel counts toward the total");
    }

    /// A linked worktree and its clone are ONE GitHub repo: they share a ref
    /// store, so keeping both meant `resolve_repo` + `branch_trailers` twice per
    /// tick over identical refs and a duplicate-looking `scannedRepos` row. The
    /// worktree here sorts FIRST, so the canonical clone can only win by being
    /// actively preferred.
    #[tokio::test]
    async fn resolve_repo_set_collapses_a_worktree_into_its_canonical_clone() {
        let tmp = tempfile::tempdir().unwrap();
        let clone = tmp.path().join("zeta");
        let seeded = git_init(&clone)
            && git_in(
                &clone,
                &[
                    "-c",
                    "user.email=t@example.com",
                    "-c",
                    "user.name=t",
                    "commit",
                    "--allow-empty",
                    "-q",
                    "-m",
                    "seed",
                ],
            )
            && git_in(
                &clone,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/acme/zeta.git",
                ],
            )
            && git_in(
                &clone,
                &["worktree", "add", "-q", "-b", "wt", "../alpha-wt"],
            );
        if !seeded {
            eprintln!("git worktree unavailable — skipping dedupe coverage");
            return;
        }
        assert!(
            is_linked_worktree(&tmp.path().join("alpha-wt")),
            "fixture must actually be a linked worktree"
        );

        let got = resolve_repo_set(&tmp.path().to_string_lossy()).await;
        assert_eq!(got.len(), 1, "one owner/name ⇒ one entry: {got:?}");
        assert!(
            canon(&got[0]).ends_with("/zeta"),
            "the canonical clone wins over the worktree: {got:?}"
        );
    }

    /// The warn-once ledger the tick keeps: a session with no repos is reported
    /// the FIRST time and then stays quiet, and re-arms if it later resolves.
    #[test]
    fn no_repo_warning_fires_once_per_session_and_rearms() {
        let mut warned: HashSet<Uuid> = HashSet::new();
        let id = Uuid::nil();
        assert!(warned.insert(id), "first skip warns");
        assert!(!warned.insert(id), "subsequent skips are silent");
        // The checkout came back: re-arm so a later disappearance is reported.
        warned.remove(&id);
        assert!(warned.insert(id), "re-armed after a successful resolution");
    }

    /// The per-session scan ledger the dropdown reads: absent ⇒ never scanned,
    /// `Some([])` ⇒ scanned and found no repos, `Some([..])` ⇒ these were
    /// searched. Three different claims, and only the first two used to be
    /// indistinguishable in the UI.
    #[test]
    fn scanned_repos_ledger_distinguishes_never_scanned_from_scanned_empty() {
        let never = Uuid::from_u128(0xA1);
        let empty = Uuid::from_u128(0xA2);
        let full = Uuid::from_u128(0xA3);

        assert_eq!(last_scanned_repos(never), None);

        record_scanned_repos(empty, &[]);
        assert_eq!(last_scanned_repos(empty), Some(Vec::new()));

        record_scanned_repos(full, &["D:/x".to_string()]);
        assert_eq!(last_scanned_repos(full), Some(vec!["D:/x".to_string()]));
    }

    /// The nudge door is fire-and-forget: calling it before the reconciler
    /// exists must be a silent no-op, never a panic or a block.
    #[test]
    fn nudge_without_a_running_reconciler_is_a_silent_no_op() {
        nudge_session(Uuid::from_u128(0xB1));
    }

    /// A command that sleeps roughly `secs` seconds, using only what the
    /// platform is guaranteed to ship: `sleep` on unix, `ping -n` on Windows
    /// (which has no `sleep`, and where `ping -n N` waits about `N-1` seconds).
    ///
    /// Spawned DIRECTLY, never through a shell: `cmd /C ping` would make the
    /// killed child the shell and orphan the `ping` behind it — the exact leak
    /// this test exists to catch.
    fn sleeping_command(secs: u32) -> Command {
        if cfg!(windows) {
            let mut c = Command::new("ping");
            c.arg("-n").arg((secs + 1).to_string()).arg("127.0.0.1");
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg(secs.to_string());
            c
        }
    }

    /// The whole point of `output_with_timeout`: a command that overruns its
    /// budget returns "no answer" PROMPTLY and its child is killed.
    ///
    /// This is the defect that mattered most — `gh auth token` awaited with no
    /// timeout blocked `run_tick` for 4.5 hours, and the leaked `gh.exe`
    /// processes it left behind are what forgetting the kill looks like.
    ///
    /// The leak assertion has real teeth on Windows, where a live process holds
    /// its cwd open and the directory cannot be removed until it dies; on unix
    /// removing a running process's cwd is legal, so there it is merely a
    /// no-op. That is why the test also asserts the elapsed budget, which is
    /// meaningful everywhere.
    #[tokio::test]
    async fn output_with_timeout_kills_a_command_that_overruns_its_budget() {
        // Positive control FIRST: on a box where the sleeper cannot run at all,
        // `None` below would prove nothing, so skip instead of passing falsely.
        let probe = output_with_timeout(
            &mut sleeping_command(0),
            Duration::from_secs(30),
            "sleeper probe",
        )
        .await;
        let Some(probe) = probe else {
            eprintln!("no runnable sleeper on this platform — skipping timeout coverage");
            return;
        };
        assert!(
            probe.status.success(),
            "the probe must complete normally and return its output"
        );

        let held = tempfile::tempdir().unwrap();
        let held_path = held.keep();

        let mut cmd = sleeping_command(30);
        cmd.current_dir(&held_path);
        let started = Instant::now();
        let got = output_with_timeout(&mut cmd, Duration::from_millis(300), "test sleeper").await;
        let waited = started.elapsed();

        assert!(
            got.is_none(),
            "an overrun must report NO ANSWER, never a result"
        );
        assert!(
            waited < Duration::from_secs(5),
            "returned only after {waited:?} — the budget was not enforced"
        );

        // The kill is asynchronous (tokio signals the child, then reaps it), so
        // give it a bounded grace period rather than racing it.
        let mut removed = false;
        for _ in 0..40 {
            if std::fs::remove_dir_all(&held_path).is_ok() {
                removed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            removed,
            "the timed-out child still holds {} — it was leaked, not killed",
            held_path.display()
        );
    }

    /// A whole-tick failure must be LOUD once and then quiet, not silent
    /// forever (the old `debug!`) and not every 30s (which trains everyone to
    /// ignore it). Warns at consecutive ticks 1, 2, 4, 8, …
    #[test]
    fn tick_failure_ledger_warns_once_then_backs_off() {
        let mut ledger = TickFailureLedger::default();

        assert_eq!(
            ledger.record("PG unavailable"),
            TickFailureReport::New,
            "the first failure is always reported"
        );
        // Tick 2 is a scheduled repeat; tick 3 falls inside the widened window.
        assert!(matches!(
            ledger.record("PG unavailable"),
            TickFailureReport::Persisting { consecutive: 2, .. }
        ));
        assert_eq!(ledger.record("PG unavailable"), TickFailureReport::Quiet);
        assert!(matches!(
            ledger.record("PG unavailable"),
            TickFailureReport::Persisting { consecutive: 4, .. }
        ));
        for _ in 5..8 {
            assert_eq!(ledger.record("PG unavailable"), TickFailureReport::Quiet);
        }
        assert!(matches!(
            ledger.record("PG unavailable"),
            TickFailureReport::Persisting { consecutive: 8, .. }
        ));
    }

    /// A DIFFERENT failure is news even mid-backoff — losing the token is not
    /// the same outage as losing PG, and must not be swallowed by the previous
    /// condition's widened window.
    #[test]
    fn tick_failure_ledger_reports_a_changed_reason_immediately() {
        let mut ledger = TickFailureLedger::default();
        assert_eq!(ledger.record("PG unavailable"), TickFailureReport::New);
        assert!(matches!(
            ledger.record("PG unavailable"),
            TickFailureReport::Persisting { .. }
        ));
        assert_eq!(ledger.record("PG unavailable"), TickFailureReport::Quiet);
        assert_eq!(
            ledger.record("no GitHub token"),
            TickFailureReport::New,
            "a changed reason restarts the run and reports at once"
        );
        // …and the new reason gets its own fresh backoff, not the old one's.
        assert!(matches!(
            ledger.record("no GitHub token"),
            TickFailureReport::Persisting { consecutive: 2, .. }
        ));
    }

    /// Recovery is worth exactly one line, and it clears the run so the NEXT
    /// outage is reported as new rather than as a continuation of the old one.
    #[test]
    fn tick_failure_ledger_reports_recovery_and_rearms() {
        let mut ledger = TickFailureLedger::default();
        assert!(
            ledger.clear().is_none(),
            "a success with no preceding failure says nothing"
        );

        ledger.record("PG unavailable");
        ledger.record("PG unavailable");
        let (consecutive, _) = ledger.clear().expect("ending a run is reported");
        assert_eq!(consecutive, 2);

        assert!(ledger.clear().is_none(), "the run was consumed");
        assert_eq!(
            ledger.record("PG unavailable"),
            TickFailureReport::New,
            "re-armed: the same reason recurring later is a NEW outage"
        );
    }
}
