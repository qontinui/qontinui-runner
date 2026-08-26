//! Runner-local per-session PR attribution + status reconciler.
//!
//! Feeds `project.session_prs`, which the Terminal zone-header dropdown reads
//! (`commands::session_prs::session_prs_get`). Ground truth for attribution:
//! every commit a session makes carries a `Session-Id: <claude_session_id>`
//! git trailer (installed machine-wide via a `prepare-commit-msg` hook). So
//! "which PRs did session S open" = PRs whose head-branch HEAD commit carries
//! `Session-Id: S`.
//!
//! Each tick (~30s, plus a best-effort pass at startup):
//!
//! 1. Enumerate open terminal-session records ([`SessionLifecycleStore::open_records`]) —
//!    each carries the `claude_session_id` and the session's `working_dir`.
//! 2. Resolve each session's repo (git toplevel of its cwd) → `owner/name`
//!    from the `origin` remote.
//! 3. In that repo, read every local branch's HEAD-commit `Session-Id`
//!    trailer(s) in ONE `git for-each-ref` and keep the branches whose trailer
//!    names this session.
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
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::process::Command;
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

/// One local branch with its HEAD sha and the `Session-Id` trailer value(s) on
/// that HEAD commit.
struct BranchTrailer {
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

/// How long a resolved `working_dir → git toplevel` mapping is trusted.
///
/// A session's cwd effectively never changes repo, so this only needs to be
/// short enough that a moved/deleted checkout self-heals within a few minutes.
const TOPLEVEL_TTL: Duration = Duration::from_secs(600);

/// Bound on the cross-tick toplevel cache. Sessions come and go, so the map
/// needs a ceiling; well above any realistic live-session count.
const TOPLEVEL_CACHE_MAX: usize = 512;

/// `working_dir → git toplevel`, cached ACROSS ticks (plan
/// `2026-07-28-runner-many-sessions-performance` §7a/B8).
///
/// The per-tick `repo_cache` below cannot help here: it is keyed BY the
/// toplevel, so it can only dedupe work that happens *after* the toplevel is
/// known — the `git rev-parse --show-toplevel` subprocess that produces the key
/// still ran once per open record per tick, i.e. N process spawns every 30s.
///
/// Invalidation: TTL'd, bounded, and a failed resolution drops the entry so a
/// moved or deleted checkout re-resolves on the next tick rather than serving
/// a stale root forever.
#[derive(Default)]
struct TopLevelCache {
    entries: HashMap<String, (String, Instant)>,
}

impl TopLevelCache {
    fn get(&self, working_dir: &str) -> Option<&str> {
        let (toplevel, at) = self.entries.get(working_dir)?;
        if at.elapsed() > TOPLEVEL_TTL {
            return None;
        }
        Some(toplevel.as_str())
    }

    fn insert(&mut self, working_dir: String, toplevel: String) {
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
        self.entries.insert(working_dir, (toplevel, Instant::now()));
    }

    fn invalidate(&mut self, working_dir: &str) {
        self.entries.remove(working_dir);
    }
}

/// Start the reconciler as a detached background task for the process
/// lifetime (matching the lifecycle liveness-poll idiom in `main.rs`). Runs a
/// best-effort pass immediately, then every [`POLL_INTERVAL`].
pub fn start(lifecycle_store: std::sync::Arc<SessionLifecycleStore>) {
    tauri::async_runtime::spawn(async move {
        info!(
            "session-PR reconciler started (interval: {}s)",
            POLL_INTERVAL.as_secs()
        );
        let mut toplevels = TopLevelCache::default();
        loop {
            if let Err(e) = run_tick(&lifecycle_store, &mut toplevels).await {
                debug!("session-PR reconciler tick error (continuing): {e}");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

/// One reconcile pass. Returns `Err` only for a whole-tick precondition miss
/// (no DB / no token); per-session/per-repo failures are logged and skipped.
async fn run_tick(
    store: &SessionLifecycleStore,
    toplevels: &mut TopLevelCache,
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
        .collect();
    if records.is_empty() {
        return Ok(());
    }

    let Some(token) = resolve_github_token().await else {
        return Err("no GitHub token (env GITHUB_TOKEN/GH_TOKEN or `gh auth token`)".to_string());
    };
    let client = GitHubClient::new(&token)?;

    // Per-repo (git toplevel) resolution cached across sessions that share a
    // checkout: one `remote get-url` + one `for-each-ref` per repo per tick.
    let mut repo_cache: HashMap<String, Option<RepoInfo>> = HashMap::new();

    for (session_id, working_dir) in records {
        let toplevel = match toplevels.get(&working_dir) {
            Some(cached) => cached.to_string(),
            None => match git_toplevel(&working_dir).await {
                Some(resolved) => {
                    toplevels.insert(working_dir.clone(), resolved.clone());
                    resolved
                }
                None => {
                    toplevels.invalidate(&working_dir);
                    debug!("session-PR reconciler: {working_dir} is not a git repo — skipping");
                    continue;
                }
            },
        };

        if !repo_cache.contains_key(&toplevel) {
            let info = resolve_repo(&toplevel).await;
            repo_cache.insert(toplevel.clone(), info);
        }
        let Some(info) = repo_cache.get(&toplevel).and_then(|o| o.as_ref()) else {
            continue;
        };

        if let Err(e) = reconcile_session(&pg_db, &client, session_id, &toplevel, info).await {
            debug!(
                "session-PR reconciler: session {session_id} in {toplevel} failed (skipping): {e}"
            );
        }
    }

    Ok(())
}

/// Attribute + status-refresh one session's PRs within its repo.
async fn reconcile_session(
    pg_db: &PgDb,
    client: &GitHubClient,
    session_id: Uuid,
    repo_dir: &str,
    info: &RepoInfo,
) -> Result<(), String> {
    let session_str = session_id.to_string();
    let repo_full = format!("{}/{}", info.owner, info.repo);

    // (repo_full, pr_number) refreshed via the branch path this tick, so the
    // Phase-3 status pass can skip a redundant get_pr for them.
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
                    "session-PR reconciler: list_prs_for_head({repo_full}, {}) failed: {e}",
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
                warn!("session-PR reconciler: upsert_session_pr failed: {e}");
                continue;
            }
            refreshed.insert((repo_full.clone(), pr_number));
        }
    }

    // ---- Phase 3: status refresh for stored rows with no live branch -------
    // A session's merged PR often has its local branch deleted, so the branch
    // path above no longer refreshes it. Pull each such stored row's current
    // state directly. Scoped to this session's OWN PRs.
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
                let dir_for_row = (row.repo == repo_full).then_some(repo_dir);
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
                    warn!("session-PR reconciler: update_session_pr_status failed: {e}");
                }
            }
            Err(e) => {
                debug!(
                    "session-PR reconciler: get_pr({}/#{}) failed (leaving prior status): {e}",
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
    match Command::new("git")
        .args(["-C", dir, "rev-parse", "--verify", "--quiet", rev])
        .output()
        .await
    {
        Ok(out) => match out.status.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => {
                debug!(
                    "session-PR reconciler: rev-parse {rev} in {dir} errored: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                None
            }
        },
        Err(e) => {
            debug!("session-PR reconciler: rev-parse {rev} in {dir} spawn failed: {e}");
            None
        }
    }
}

/// `git -C <dir> merge-base --is-ancestor <ancestor> <descendant>` —
/// `Some(true)`/`Some(false)` for git's 0/1 answers, `None` for any OTHER exit
/// (128 = missing object / not a repo) or a spawn failure. A non-answer is
/// never reported as `false`.
async fn git_is_ancestor(dir: &str, ancestor: &str, descendant: &str) -> Option<bool> {
    match Command::new("git")
        .args([
            "-C",
            dir,
            "merge-base",
            "--is-ancestor",
            ancestor,
            descendant,
        ])
        .output()
        .await
    {
        Ok(out) => match out.status.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => {
                debug!(
                    "session-PR reconciler: is-ancestor {ancestor} {descendant} in {dir} errored: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                None
            }
        },
        Err(e) => {
            debug!("session-PR reconciler: is-ancestor spawn in {dir} failed: {e}");
            None
        }
    }
}

/// Committer timestamp (epoch seconds) of a commit, or `None` if git fails.
async fn git_commit_ts(dir: &str, rev: &str) -> Option<i64> {
    let out = Command::new("git")
        .args(["-C", dir, "log", "-1", "--format=%ct", rev])
        .output()
        .await
        .ok()?;
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
    let out = Command::new("git")
        .args(["-C", dir, "rev-parse", "--show-toplevel"])
        .output()
        .await
        .ok()?;
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
    let out = Command::new("git")
        .args(["-C", dir, "remote", "get-url", "origin"])
        .output()
        .await
        .ok()?;
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

/// Read every local branch's HEAD sha + `Session-Id` trailer value(s) in one
/// `git for-each-ref`. Line format: `<sha> <branch> <trailer values…>`
/// (git ref names contain no whitespace, so the first two whitespace fields
/// are unambiguous; the remainder is the space-joined trailer value set).
async fn branch_trailers(dir: &str) -> Vec<BranchTrailer> {
    let out = match Command::new("git")
        .args([
            "-C",
            dir,
            "for-each-ref",
            "--format=%(objectname) %(refname:short) %(trailers:key=Session-Id,valueonly,separator=%x20)",
            "refs/heads/",
        ])
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            debug!(
                "session-PR reconciler: for-each-ref in {dir} failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return Vec::new();
        }
        Err(e) => {
            debug!("session-PR reconciler: for-each-ref spawn in {dir} failed: {e}");
            return Vec::new();
        }
    };

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_branch_trailer_line)
        .collect()
}

/// Parse one `for-each-ref` line into a [`BranchTrailer`]. `None` for a line
/// missing the sha/branch fields.
fn parse_branch_trailer_line(line: &str) -> Option<BranchTrailer> {
    let mut it = line.split_whitespace();
    let sha = it.next()?.to_string();
    let branch = it.next()?.to_string();
    let session_ids: Vec<String> = it.map(|s| s.to_string()).collect();
    Some(BranchTrailer {
        branch,
        sha,
        session_ids,
    })
}

/// Read the `Session-Id` trailer value(s) on a specific commit, or `None` if
/// the object isn't present locally (or git fails).
async fn read_session_trailers(dir: &str, sha: &str) -> Option<Vec<String>> {
    let out = Command::new("git")
        .args([
            "-C",
            dir,
            "log",
            "-1",
            "--format=%(trailers:key=Session-Id,valueonly,separator=%x20)",
            sha,
        ])
        .output()
        .await
        .ok()?;
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
    let out = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .await
        .ok()?;
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

    #[test]
    fn parse_branch_trailer_line_splits_sha_branch_and_sessions() {
        // No trailer.
        let bt = parse_branch_trailer_line("abc123 main").unwrap();
        assert_eq!(bt.sha, "abc123");
        assert_eq!(bt.branch, "main");
        assert!(bt.session_ids.is_empty());

        // One Session-Id trailer.
        let bt = parse_branch_trailer_line("deadbeef feat/x 11111111-1111-1111-1111-111111111111")
            .unwrap();
        assert_eq!(bt.branch, "feat/x");
        assert_eq!(bt.session_ids, vec!["11111111-1111-1111-1111-111111111111"]);

        // Two (branch touched under two Session-Id trailers on one commit).
        let bt = parse_branch_trailer_line("sha b/1 aaa bbb").unwrap();
        assert_eq!(bt.session_ids, vec!["aaa", "bbb"]);

        // Malformed (sha only) → no branch field.
        assert!(parse_branch_trailer_line("loneword").is_none());
        assert!(parse_branch_trailer_line("").is_none());
    }

    #[test]
    fn parse_ts_roundtrips_github_rfc3339() {
        let dt = parse_ts("2026-07-13T01:02:03Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-07-13T01:02:03+00:00");
        assert!(parse_ts("not-a-date").is_none());
    }

    /// The whole point of the cross-tick cache: N sessions sharing a checkout
    /// resolve the toplevel ONCE, not once per session per 30s tick.
    #[test]
    fn toplevel_cache_serves_repeats_and_forgets_failures() {
        let mut cache = TopLevelCache::default();
        assert!(cache.get("D:/repo/sub").is_none(), "cold cache resolves");

        cache.insert("D:/repo/sub".to_string(), "D:/repo".to_string());
        assert_eq!(cache.get("D:/repo/sub"), Some("D:/repo"));
        // A second session in the same cwd costs no subprocess.
        assert_eq!(cache.get("D:/repo/sub"), Some("D:/repo"));
        // A different cwd is still a miss.
        assert!(cache.get("D:/other").is_none());

        // A cwd that stops resolving (checkout moved/deleted) is forgotten so
        // it re-resolves rather than serving a stale root forever.
        cache.invalidate("D:/repo/sub");
        assert!(cache.get("D:/repo/sub").is_none());
    }

    /// Bounded: the cache must not grow with churning session cwds.
    #[test]
    fn toplevel_cache_is_bounded() {
        let mut cache = TopLevelCache::default();
        for i in 0..(TOPLEVEL_CACHE_MAX * 2) {
            cache.insert(format!("D:/wd/{i}"), "D:/repo".to_string());
        }
        assert!(
            cache.entries.len() <= TOPLEVEL_CACHE_MAX,
            "cache grew past its bound: {}",
            cache.entries.len()
        );
    }
}
