//! Sibling path-dep provisioning for one CI dispatch.
//!
//! # The bug this closes
//!
//! `qontinui-coord/Cargo.toml:95` and `:101` carry Cargo **path** deps on a
//! sibling repo (`../qontinui-schemas/rust`, `../qontinui-schemas/code-graph`),
//! and `qontinui-web/backend/pyproject.toml:57` expresses the same dependency
//! at a different depth (`../../qontinui-schemas`). Both resolve to the same
//! place: **a sibling of the repo checkout**. The CI-node executor checked out
//! only the dispatched repo, one level deeper than the primary clone, so
//! `../qontinui-schemas` resolved into the `.ci-worktrees` directory and a
//! pre-existing clone at the device root did NOT satisfy the dep. On a dev box
//! that clone usually exists, so the failure looked like a cargo bug rather
//! than a layout mismatch.
//!
//! # The layout rule
//!
//! ```text
//! <QONTINUI_ROOT>/.ci-worktrees/<dispatch_id>/          <- the cleanup unit
//!                                 ├── <repo>/           <- dispatched worktree
//!                                 └── <sibling>/        <- one per [[siblings]]
//! ```
//!
//! ONE rule, deliberately not one per toolchain: the dispatched worktree sits
//! where a workspace-root sibling resolves, which satisfies Cargo's
//! `../qontinui-schemas/rust` from the repo root and Poetry's
//! `../../qontinui-schemas` from `backend/` alike. One invariant to hold
//! beats one per language.
//!
//! It also keeps `checkout::cleanup_dispatch`'s guarantee intact: everything
//! this module writes lands inside the dispatch-scoped parent, so cleanup is
//! still a single directory removal and nothing survives a dispatch — success
//! or failure. That is the precise reason an in-manifest `git clone` is
//! rejected: such a step WOULD pass argv validation (no banned metacharacter,
//! and the executor does not sandbox the filesystem), but it would be
//! unpinned and it would leak.
//!
//! # No shared clone cache
//!
//! Each dispatch clones its siblings fresh (`--depth 1`, by exact SHA). A
//! shared cache would have to live OUTSIDE the dispatch-scoped directory —
//! i.e. outside the cleanup guarantee — and would then need its own pruning,
//! its own concurrent-dispatch locking, and its own answer for a half-fetched
//! tree. The cost avoided is one shallow fetch of one repo per dispatch; the
//! property preserved is that a dispatch leaves nothing behind. Tools are
//! cached ([`super::tools`]) because their key is an exact version; a sibling's
//! key is a commit that moves, so the two are not the same trade.
//!
//! # Which commit — the Actions lane's rule, not a third one
//!
//! Resolution mirrors `.github/actions/checkout-sibling/action.yml` (Phase A
//! landed as `a543cc7e1`/#984; Phase B is PR #1008), because the two lanes
//! must agree on which sibling tree a change compiles against or "parity"
//! means nothing:
//!
//! * The hatch is a **declared adaptation PR** — the coord dep edge the pair
//!   already declares for merge ordering — never a branch NAME. A branch name
//!   is not an identity, and the same name-matching mechanism was exploited in
//!   production on 2026-07-28 when a consumer gate went green against a branch
//!   with no pull request at all.
//! * BOTH label forms are read, ALWAYS, and every match is collected rather
//!   than taking the first or the last — that is the only way two declarations
//!   naming DIFFERENT sibling PRs can be detected. Exactly one tree can be
//!   checked out, so an ambiguity is the author's to resolve, not ours to
//!   guess.
//! * Comparison is **owner-stripped**, because the owner is optional in
//!   coord's dep-edge grammar.
//! * "Absent" and "unanswered" are different answers and only the first is a
//!   NO. A rate-limited or unreachable probe is UNKNOWN and hard-fails; a
//!   404 on a PR that WAS declared is a semantic red about the declaration.
//! * Resolution is keyed on a pull request, never on the pushed ref. The
//!   CI-node lane dispatches `refs/heads/merge-candidate/<proposal_id>`, and
//!   coord pushes the SAME candidate ref into every repo of a multi-repo
//!   proposal — so keying off the ref name would compile against a tree that
//!   is force-pushed and deleted as the proposal resolves. That event class
//!   resolves to the sibling's branch with no API call at all, exactly as the
//!   composite action does.
//!
//! **Stronger than #1008 in one place, and only one.** #1008 fetches the
//! declared PR's head SHA but the no-declaration path by BRANCH. Here both
//! paths end at the same primitive: resolve to a SHA first (`ls-remote` for
//! the branch case), then fetch that SHA and assert the checkout landed on
//! exactly it. Same resolution rule, one materialisation invariant instead of
//! two — and the resolved SHA is recorded in the dispatch log either way, so
//! "which tree did this build compile against" is answerable after the fact.

use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

use super::checkout::run_git;
use super::manifest::{CiSibling, SiblingPin};

/// Budget for the sibling fetch (a `--depth 1` fetch of one commit).
const SIBLING_FETCH_TIMEOUT: Duration = Duration::from_secs(600);
/// Budget for local git plumbing.
const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Per-request GitHub API budget.
const API_TIMEOUT: Duration = Duration::from_secs(60);
/// Pagination bound on the reverse declaration search. An unbounded scan of
/// open PRs is a denial-of-service surface; 20 pages of 100 is far past any
/// real repo.
const MAX_PR_PAGES: u32 = 20;

const DOWNSTREAM_PREFIX: &str = "coord:downstream-of=";
const UPSTREAM_PREFIX: &str = "coord:upstream-of=";

// ─────────────────────────── pure decision core ───────────────────────────

/// Labels on one open sibling PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SiblingPrLabels {
    pub number: u64,
    pub labels: Vec<String>,
}

/// Everything the declaration rule reads. Collected by IO, decided purely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeclarationInputs {
    /// Label names on the dispatched repo's pull request.
    pub this_pr_labels: Vec<String>,
    /// Every OPEN pull request on the sibling, with its labels.
    pub sibling_open_prs: Vec<SiblingPrLabels>,
}

/// Which declaration form resolved the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeclarationSource {
    DownstreamOf,
    UpstreamOf,
    BothAgreeing,
}

/// Outcome of the declaration rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclaredTarget {
    /// No declaration — resolve to the sibling's branch.
    None,
    Pr {
        number: u64,
        source: DeclarationSource,
    },
}

/// Basename of an `[owner/]name` slug — the owner is optional in coord's
/// dep-edge grammar, so every comparison here is owner-stripped. Do NOT
/// "simplify" this to an exact string match: the schemas-side twin does
/// exactly that and silently misses every bare-form label.
fn short_name(slug: &str) -> &str {
    slug.rsplit('/').next().unwrap_or(slug)
}

/// Split a `[<owner>/]<repo>#<n>` dep-edge spec.
fn parse_dep_spec(spec: &str) -> Option<(&str, &str)> {
    let (repo, num) = spec.rsplit_once('#')?;
    (!repo.is_empty() && !num.is_empty()).then_some((repo, num))
}

/// Deduplicate while preserving order.
fn dedup(mut v: Vec<u64>) -> Vec<u64> {
    let mut seen = Vec::with_capacity(v.len());
    v.retain(|n| {
        if seen.contains(n) {
            false
        } else {
            seen.push(*n);
            true
        }
    });
    v
}

fn ambiguous(what: &str, sibling_repo: &str, numbers: &[u64]) -> String {
    let list = numbers
        .iter()
        .map(|n| format!("{sibling_repo}#{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "contradictory declarations — {what}: {list}. Exactly one {} tree can be checked out, \
         so this is yours to resolve: keep the declaration naming the PR this change actually \
         adapts to and remove the other(s).",
        short_name(sibling_repo)
    )
}

/// The declaration rule, in full and pure.
///
/// Both forms are evaluated unconditionally even when the primary hits, and
/// every match in each form is collected. There are three ways to be
/// ambiguous and all three hard-fail alike: two `downstream-of` labels on the
/// dispatched PR, two sibling PRs declaring `upstream-of` at it, or the two
/// forms naming different PRs.
pub(crate) fn resolve_declaration(
    sibling_repo: &str,
    this_repo: &str,
    this_pr: u64,
    inputs: &DeclarationInputs,
) -> Result<DeclaredTarget, String> {
    let sibling_short = short_name(sibling_repo);
    let this_short = short_name(this_repo);

    // Form 2 (PRIMARY, self-read): coord:downstream-of on the dispatched PR.
    let mut downs: Vec<u64> = Vec::new();
    for label in &inputs.this_pr_labels {
        let Some(spec) = label.strip_prefix(DOWNSTREAM_PREFIX) else {
            continue;
        };
        let Some((repo_part, num_part)) = parse_dep_spec(spec) else {
            return Err(format!(
                "malformed label {label:?} — expected {DOWNSTREAM_PREFIX}[<owner>/]<repo>#<n>"
            ));
        };
        if short_name(repo_part) != sibling_short {
            continue;
        }
        let n: u64 = num_part.parse().map_err(|_| {
            format!("malformed label {label:?} — {num_part:?} is not a PR number")
        })?;
        downs.push(n);
    }
    let downs = dedup(downs);
    if downs.len() > 1 {
        return Err(ambiguous(
            &format!(
                "this pull request carries {DOWNSTREAM_PREFIX} labels naming DIFFERENT \
                 {sibling_short} pull requests"
            ),
            sibling_repo,
            &downs,
        ));
    }

    // Form 1 (FALLBACK, reverse search): coord:upstream-of on a sibling PR.
    // Evaluated even when the primary hit, so a contradiction cannot be
    // masked by ordering.
    let mut ups: Vec<u64> = Vec::new();
    for pr in &inputs.sibling_open_prs {
        for label in &pr.labels {
            let Some(spec) = label.strip_prefix(UPSTREAM_PREFIX) else {
                continue;
            };
            let Some((repo_part, num_part)) = parse_dep_spec(spec) else {
                continue; // a malformed label on someone ELSE's PR is not ours to red.
            };
            if short_name(repo_part) == this_short && num_part.parse::<u64>() == Ok(this_pr) {
                ups.push(pr.number);
            }
        }
    }
    let ups = dedup(ups);
    if ups.len() > 1 {
        return Err(ambiguous(
            &format!(
                "several open {sibling_short} pull requests declare \
                 {UPSTREAM_PREFIX}{this_short}#{this_pr}"
            ),
            sibling_repo,
            &ups,
        ));
    }

    match (downs.first().copied(), ups.first().copied()) {
        (Some(d), Some(u)) if d != u => Err(ambiguous(
            &format!(
                "this pull request declares {DOWNSTREAM_PREFIX}{sibling_short}#{d}, but \
                 {sibling_repo}#{u} declares {UPSTREAM_PREFIX}{this_short}#{this_pr}"
            ),
            sibling_repo,
            &[d, u],
        )),
        (Some(d), Some(_)) => Ok(DeclaredTarget::Pr {
            number: d,
            source: DeclarationSource::BothAgreeing,
        }),
        (Some(d), None) => Ok(DeclaredTarget::Pr {
            number: d,
            source: DeclarationSource::DownstreamOf,
        }),
        (None, Some(u)) => Ok(DeclaredTarget::Pr {
            number: u,
            source: DeclarationSource::UpstreamOf,
        }),
        (None, None) => Ok(DeclaredTarget::None),
    }
}

/// The fields of a sibling PR the declaration validation reads. Flattened
/// from the API's nested shape at the IO boundary rather than derived, so the
/// validation below is a function of five strings and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SiblingPrDetail {
    pub state: String,
    pub base_ref: String,
    pub head_ref: String,
    pub head_sha: String,
    pub head_repo_full_name: String,
}

/// PRESENT is not enough; a declaration must be VERIFIABLE. Pure, so every
/// rejection is testable without the network. Returns the pinned head SHA.
pub(crate) fn validate_declared_pr(
    sibling_repo: &str,
    number: u64,
    d: &SiblingPrDetail,
) -> Result<String, String> {
    let invalid = |why: String| {
        Err(format!(
            "declared adaptation {sibling_repo}#{number} is invalid: {why} \
             This is a red about the DECLARATION, not an infra fault, and it deliberately does \
             NOT fall back to the default branch — an author who declared something wrong must \
             see it rather than get a silent main build."
        ))
    };
    if d.state != "open" {
        return invalid(format!(
            "it is '{}', not 'open'. If it has ALREADY LANDED, remove the label — it no longer \
             applies.",
            d.state
        ));
    }
    // A PR based on another feature branch AUTO-CLOSES UNLANDED when its
    // parent lands, so accepting one would let this resolve to a declaration
    // that can silently evaporate.
    if d.base_ref != "main" {
        return invalid(format!(
            "it is based on '{}', not 'main'. A PR based on a feature branch auto-closes \
             UNLANDED when its parent lands, so the declaration can silently evaporate.",
            d.base_ref
        ));
    }
    // A fork's head commit is not fetchable from the upstream, so accepting
    // one would produce a fetch failure wearing a declaration's clothes.
    if d.head_repo_full_name != sibling_repo {
        return invalid(format!(
            "its head is on '{}', not '{sibling_repo}'. A fork's head commit is not fetchable \
             from the upstream.",
            d.head_repo_full_name
        ));
    }
    if !is_full_sha(&d.head_sha) {
        return invalid(format!(
            "its head sha {:?} is not a 40-character hex commit id.",
            d.head_sha
        ));
    }
    Ok(d.head_sha.clone())
}

/// A full 40-hex commit id — the only shape accepted as a pin, so a short or
/// abbreviated id can never be handed to `git fetch` where it would resolve
/// ambiguously.
pub(crate) fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Destination directory for a sibling under the dispatch-scoped parent.
/// Pure — the layout rule itself.
pub(crate) fn sibling_dest(dispatch_root: &Path, sibling: &CiSibling) -> PathBuf {
    dispatch_root.join(sibling.dir_name())
}

/// Clone URL for a sibling slug. Public HTTPS only: the sibling repos this
/// lane provisions are public, and reaching for a credential here would put
/// the user's GitHub token on a network path the manifest chose.
fn clone_url(repo: &str) -> String {
    format!("https://github.com/{repo}.git")
}

// ──────────────────────────────── IO ──────────────────────────────────────

/// A GitHub API answer, split the way the Actions action splits it: an
/// exchange that COMPLETED (any status) is an answer; anything else is
/// UNKNOWN and every caller hard-fails on it.
struct ApiAnswer {
    status: u16,
    body: String,
}

async fn api_get(client: &reqwest::Client, url: &str, token: Option<&str>) -> Option<ApiAnswer> {
    let mut req = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "qontinui-runner-ci-node");
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await.ok()?;
    let status = resp.status().as_u16();
    let body = resp.text().await.ok()?;
    Some(ApiAnswer { status, body })
}

/// Turn an answer into either its body or an UNKNOWN hard-fail. `404` is left
/// to the caller, because whether a 404 is semantic depends on whether the
/// thing being read was DECLARED.
fn expect_ok(what: &str, answer: Option<ApiAnswer>) -> Result<ApiAnswer, String> {
    let Some(a) = answer else {
        return Err(format!(
            "{what}: no answer at all (network/DNS/TLS). An unanswered probe is UNKNOWN, not \
             'no declaration' — refusing to guess a sibling tree."
        ));
    };
    match a.status {
        200 => Ok(a),
        403 | 429 => Err(format!(
            "{what}: rate-limited (HTTP {}). UNKNOWN, not 'no declaration'. Set GITHUB_TOKEN/\
             GH_TOKEN or authenticate `gh` on this device to raise the limit.",
            a.status
        )),
        404 => Ok(a),
        s => Err(format!("{what}: unexpected HTTP {s}. UNKNOWN — refusing to guess.")),
    }
}

/// Read the label names on a pull request.
async fn fetch_pr_labels(
    client: &reqwest::Client,
    token: Option<&str>,
    repo: &str,
    number: u64,
) -> Result<Vec<String>, String> {
    let what = format!("reading labels on {repo}#{number}");
    let url = format!("https://api.github.com/repos/{repo}/issues/{number}/labels?per_page=100");
    let a = expect_ok(&what, api_get(client, &url, token).await)?;
    if a.status == 404 {
        return Err(format!("{what}: HTTP 404 — no such pull request."));
    }
    let arr: Vec<serde_json::Value> = serde_json::from_str(&a.body)
        .map_err(|e| format!("{what}: response was not a JSON array ({e}). UNKNOWN."))?;
    Ok(arr
        .iter()
        .filter_map(|v| v.get("name")?.as_str().map(|s| s.to_string()))
        .collect())
}

/// Read every OPEN pull request on the sibling, with labels. Paginated
/// EXPLICITLY: a silently-truncated page reads as "no declaration", which is
/// the wrong answer in the safe-looking direction.
async fn fetch_sibling_open_prs(
    client: &reqwest::Client,
    token: Option<&str>,
    sibling_repo: &str,
) -> Result<Vec<SiblingPrLabels>, String> {
    let mut out = Vec::new();
    for page in 1..=MAX_PR_PAGES {
        let what = format!("reading open pull requests on {sibling_repo} (page {page})");
        let url = format!(
            "https://api.github.com/repos/{sibling_repo}/pulls?state=open&per_page=100&page={page}"
        );
        let a = expect_ok(&what, api_get(client, &url, token).await)?;
        if a.status == 404 {
            return Err(format!("{what}: HTTP 404 — no such repository."));
        }
        let arr: Vec<serde_json::Value> = serde_json::from_str(&a.body)
            .map_err(|e| format!("{what}: response was not a JSON array ({e}). UNKNOWN."))?;
        let n = arr.len();
        for pr in arr {
            let Some(number) = pr.get("number").and_then(|v| v.as_u64()) else {
                continue;
            };
            let labels = pr
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l.get("name")?.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            out.push(SiblingPrLabels { number, labels });
        }
        if n < 100 {
            return Ok(out);
        }
    }
    Err(format!(
        "refusing to paginate {sibling_repo} open pull requests past {} entries — declaration \
         lookup is unbounded.",
        MAX_PR_PAGES * 100
    ))
}

/// Read one sibling PR's detail. A 404 here is a SEMANTIC red (the
/// declaration names a PR that does not exist), never an infra fault —
/// reporting it as infra would be the "absent vs unanswered" confusion
/// inverted.
async fn fetch_sibling_pr_detail(
    client: &reqwest::Client,
    token: Option<&str>,
    sibling_repo: &str,
    number: u64,
) -> Result<SiblingPrDetail, String> {
    let what = format!("reading {sibling_repo}#{number}");
    let url = format!("https://api.github.com/repos/{sibling_repo}/pulls/{number}");
    let a = expect_ok(&what, api_get(client, &url, token).await)?;
    if a.status == 404 {
        return Err(format!(
            "declared adaptation {sibling_repo}#{number} is invalid: no such pull request \
             (HTTP 404). Check the number."
        ));
    }
    let v: serde_json::Value = serde_json::from_str(&a.body)
        .map_err(|e| format!("{what}: response was not a JSON object ({e}). UNKNOWN."))?;
    let s = |p: &[&str]| -> String {
        let mut cur = &v;
        for k in p {
            match cur.get(*k) {
                Some(next) => cur = next,
                None => return String::new(),
            }
        }
        cur.as_str().unwrap_or_default().to_string()
    };
    Ok(SiblingPrDetail {
        state: s(&["state"]),
        base_ref: s(&["base", "ref"]),
        head_ref: s(&["head", "ref"]),
        head_sha: s(&["head", "sha"]),
        head_repo_full_name: s(&["head", "repo", "full_name"]),
    })
}

/// Resolve a branch tip to a SHA so both resolution paths converge on the
/// same "fetch this exact commit" primitive.
async fn ls_remote_branch_sha(cwd: &Path, repo: &str, branch: &str) -> Result<String, String> {
    let url = clone_url(repo);
    let refspec = format!("refs/heads/{branch}");
    let out = run_git(
        cwd,
        &["ls-remote", "--exit-code", &url, &refspec],
        SIBLING_FETCH_TIMEOUT,
    )
    .await
    .map_err(|e| format!("resolving {repo}@{branch}: {e}"))?;
    let sha = out
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    if !is_full_sha(&sha) {
        return Err(format!(
            "resolving {repo}@{branch}: ls-remote returned {sha:?}, not a commit id"
        ));
    }
    Ok(sha)
}

/// What a sibling resolved to, for the dispatch log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSibling {
    pub repo: String,
    pub sha: String,
    /// Human-readable account of WHY this commit — printed into the dispatch
    /// log so "which tree did this build compile against" stays answerable.
    pub provenance: String,
}

/// Resolve one sibling to an exact commit.
async fn resolve_one(
    cwd: &Path,
    client: &reqwest::Client,
    token: Option<&str>,
    sibling: &CiSibling,
    this_repo: &str,
    pr_number: Option<u64>,
) -> Result<ResolvedSibling, String> {
    let branch_fallback = |sha: String, why: &str| ResolvedSibling {
        repo: sibling.repo.clone(),
        sha,
        provenance: format!("{}@{} ({why})", sibling.repo, sibling.branch),
    };

    if sibling.pin == SiblingPin::DefaultBranch {
        let sha = ls_remote_branch_sha(cwd, &sibling.repo, &sibling.branch).await?;
        return Ok(branch_fallback(sha, "pin = default-branch"));
    }

    // Property 3: no pull request ⇒ the branch, with NO api call at all.
    let Some(pr) = pr_number else {
        let sha = ls_remote_branch_sha(cwd, &sibling.repo, &sibling.branch).await?;
        return Ok(branch_fallback(
            sha,
            "no pull request on this dispatch — no declaration probe",
        ));
    };

    let this_labels = fetch_pr_labels(client, token, this_repo, pr).await?;
    let sibling_prs = fetch_sibling_open_prs(client, token, &sibling.repo).await?;
    let inputs = DeclarationInputs {
        this_pr_labels: this_labels,
        sibling_open_prs: sibling_prs,
    };
    match resolve_declaration(&sibling.repo, this_repo, pr, &inputs)? {
        DeclaredTarget::None => {
            let sha = ls_remote_branch_sha(cwd, &sibling.repo, &sibling.branch).await?;
            Ok(branch_fallback(sha, "no declaration"))
        }
        DeclaredTarget::Pr { number, source } => {
            let detail = fetch_sibling_pr_detail(client, token, &sibling.repo, number).await?;
            let sha = validate_declared_pr(&sibling.repo, number, &detail)?;
            Ok(ResolvedSibling {
                repo: sibling.repo.clone(),
                sha,
                provenance: format!(
                    "declared adaptation {}#{number} (via {source:?}, head {})",
                    sibling.repo, detail.head_ref
                ),
            })
        }
    }
}

/// Materialise a resolved sibling at `dest`, pinned. Mirrors #1008's
/// `git init` → `fetch --depth 1 <sha>` → `checkout FETCH_HEAD` → **assert we
/// got exactly that commit**. The assertion is the point: without it a fetch
/// that resolved something else is a silent substitution.
async fn materialise(dest: &Path, resolved: &ResolvedSibling) -> Result<(), String> {
    if dest.exists() {
        return Err(format!(
            "{} already exists — refusing to overwrite a checkout that is already there",
            dest.display()
        ));
    }
    let parent = dest
        .parent()
        .ok_or_else(|| format!("{} has no parent", dest.display()))?;
    std::fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let dest_str = dest.to_string_lossy().to_string();
    let url = clone_url(&resolved.repo);

    run_git(parent, &["init", "-q", &dest_str], GIT_LOCAL_TIMEOUT).await?;
    run_git(dest, &["remote", "add", "origin", &url], GIT_LOCAL_TIMEOUT).await?;
    run_git(
        dest,
        &["fetch", "--depth", "1", "--no-tags", "origin", &resolved.sha],
        SIBLING_FETCH_TIMEOUT,
    )
    .await
    .map_err(|e| {
        format!(
            "fetching {}@{}: {e}. (The commit must be reachable from {url}; a fork's head is not.)",
            resolved.repo, resolved.sha
        )
    })?;
    run_git(dest, &["checkout", "-q", "FETCH_HEAD"], GIT_LOCAL_TIMEOUT).await?;

    let got = run_git(dest, &["rev-parse", "HEAD"], GIT_LOCAL_TIMEOUT).await?;
    if got.trim() != resolved.sha {
        return Err(format!(
            "asked for {} but checked out {} — refusing a substituted tree",
            resolved.sha,
            got.trim()
        ));
    }
    Ok(())
}

/// Provision every declared sibling into the dispatch-scoped parent.
///
/// `worktree_dir_name` is the directory the dispatched repo occupies; a
/// sibling that would land on it is rejected before any IO, because
/// materialising over the worktree is the one collision the "destination
/// already exists" refusal would report far too late to be legible.
pub(crate) async fn provision(
    dispatch_root: &Path,
    worktree_dir_name: &str,
    siblings: &[CiSibling],
    this_repo: &str,
    pr_number: Option<u64>,
    // `+ Send` because a dispatch runs on a `tokio::spawn`ed task and this
    // callback is held across awaits.
    log: &mut (dyn FnMut(String) + Send),
) -> Result<Vec<ResolvedSibling>, String> {
    if siblings.is_empty() {
        return Ok(Vec::new());
    }
    for s in siblings {
        if s.dir_name() == worktree_dir_name {
            return Err(format!(
                "sibling '{}' would be materialised over the dispatched repo's own worktree \
                 directory {worktree_dir_name:?}",
                s.repo
            ));
        }
    }

    let token = crate::session_pr_reconciler::resolve_github_token().await;
    if token.is_none() && pr_number.is_some() {
        // Unauthenticated GitHub reads work for public repos but are limited
        // to 60/hour per IP. Say so BEFORE the rate-limit hard-fail, so the
        // fix is obvious when it happens.
        warn!(
            "ci_node: no GitHub token (GITHUB_TOKEN/GH_TOKEN or `gh auth token`) — sibling \
             declaration probes will run unauthenticated at 60 requests/hour"
        );
    }
    let client = reqwest::Client::builder()
        .timeout(API_TIMEOUT)
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let mut resolved_all = Vec::with_capacity(siblings.len());
    for sibling in siblings {
        let resolved = resolve_one(
            dispatch_root,
            &client,
            token.as_deref(),
            sibling,
            this_repo,
            pr_number,
        )
        .await?;
        let dest = sibling_dest(dispatch_root, sibling);
        log(format!(
            "[ci-node] sibling {} -> {} @ {} [{}]",
            sibling.repo,
            dest.display(),
            resolved.sha,
            resolved.provenance
        ));
        materialise(&dest, &resolved).await?;
        info!(
            "ci_node: sibling {} materialised at {} ({})",
            sibling.repo,
            dest.display(),
            resolved.sha
        );
        resolved_all.push(resolved);
    }
    Ok(resolved_all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sib(repo: &str) -> CiSibling {
        let text = format!(
            "version = 1\n[[siblings]]\nrepo = {repo:?}\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n"
        );
        super::super::manifest::parse_and_validate(&text)
            .expect("test sibling must parse")
            .siblings
            .remove(0)
    }

    const SCHEMAS: &str = "qontinui/qontinui-schemas";
    const RUNNER: &str = "qontinui/qontinui-runner";

    fn inputs(this: &[&str], sibling: &[(u64, &[&str])]) -> DeclarationInputs {
        DeclarationInputs {
            this_pr_labels: this.iter().map(|s| s.to_string()).collect(),
            sibling_open_prs: sibling
                .iter()
                .map(|(n, l)| SiblingPrLabels {
                    number: *n,
                    labels: l.iter().map(|s| s.to_string()).collect(),
                })
                .collect(),
        }
    }

    // ── the layout rule ───────────────────────────────────────────────────

    /// The whole point of Phase 1: a sibling lands NEXT TO the dispatched
    /// worktree, inside the dispatch-scoped parent, so `../<sibling>` from
    /// the repo root resolves — and so cleanup is still one directory.
    #[test]
    fn sibling_is_a_worktree_sibling_inside_the_dispatch_scope() {
        let dispatch_root = Path::new("/root/.ci-worktrees/d-123");
        let dest = sibling_dest(dispatch_root, &sib(SCHEMAS));
        assert_eq!(
            dest,
            PathBuf::from("/root/.ci-worktrees/d-123/qontinui-schemas")
        );
        // Cargo's `../qontinui-schemas/rust` from <dispatch>/qontinui-coord
        // and Poetry's `../../qontinui-schemas` from
        // <dispatch>/qontinui-web/backend both name this same directory.
        let from_cargo = dispatch_root.join("qontinui-coord").join("../qontinui-schemas");
        let from_poetry = dispatch_root
            .join("qontinui-web")
            .join("backend")
            .join("../../qontinui-schemas");
        for p in [from_cargo, from_poetry] {
            let normalised: PathBuf = p
                .components()
                .fold(PathBuf::new(), |mut acc, c| match c {
                    std::path::Component::ParentDir => {
                        acc.pop();
                        acc
                    }
                    other => {
                        acc.push(other);
                        acc
                    }
                });
            assert_eq!(normalised, dest);
        }
        // Nothing escapes the cleanup unit.
        assert!(dest.starts_with(dispatch_root));
    }

    // ── declaration resolution (pure) ─────────────────────────────────────

    #[test]
    fn no_declaration_resolves_to_none() {
        let got = resolve_declaration(SCHEMAS, RUNNER, 7, &inputs(&["bug", "coord:blocked"], &[]))
            .unwrap();
        assert_eq!(got, DeclaredTarget::None);
    }

    #[test]
    fn downstream_of_on_this_pr_resolves() {
        let got = resolve_declaration(
            SCHEMAS,
            RUNNER,
            7,
            &inputs(&["coord:downstream-of=qontinui/qontinui-schemas#104"], &[]),
        )
        .unwrap();
        assert_eq!(
            got,
            DeclaredTarget::Pr {
                number: 104,
                source: DeclarationSource::DownstreamOf
            }
        );
    }

    /// The owner is OPTIONAL in coord's grammar, so comparison is
    /// owner-stripped. An exact match here is the schemas twin's live bug.
    #[test]
    fn bare_and_owned_label_forms_both_match() {
        for spec in [
            "qontinui-schemas#104",
            "qontinui/qontinui-schemas#104",
            "someoneelse/qontinui-schemas#104",
        ] {
            let got = resolve_declaration(
                SCHEMAS,
                RUNNER,
                7,
                &inputs(&[&format!("coord:downstream-of={spec}")], &[]),
            )
            .unwrap();
            assert!(
                matches!(got, DeclaredTarget::Pr { number: 104, .. }),
                "{spec}: got {got:?}"
            );
        }
    }

    #[test]
    fn upstream_of_on_the_sibling_pr_resolves() {
        let got = resolve_declaration(
            SCHEMAS,
            RUNNER,
            7,
            &inputs(&[], &[(104, &["coord:upstream-of=qontinui-runner#7"])]),
        )
        .unwrap();
        assert_eq!(
            got,
            DeclaredTarget::Pr {
                number: 104,
                source: DeclarationSource::UpstreamOf
            }
        );
    }

    /// One declaration satisfies both repos' gates, so both forms agreeing is
    /// the common case — and must not read as a contradiction.
    #[test]
    fn both_forms_agreeing_is_not_a_contradiction() {
        let got = resolve_declaration(
            SCHEMAS,
            RUNNER,
            7,
            &inputs(
                &["coord:downstream-of=qontinui-schemas#104"],
                &[(104, &["coord:upstream-of=qontinui-runner#7"])],
            ),
        )
        .unwrap();
        assert_eq!(
            got,
            DeclaredTarget::Pr {
                number: 104,
                source: DeclarationSource::BothAgreeing
            }
        );
    }

    /// Three ways to be ambiguous; all three hard-fail alike, because exactly
    /// one tree can be checked out and picking one would resolve an ambiguity
    /// that is the author's to resolve.
    #[test]
    fn all_three_ambiguities_hard_fail() {
        // (1) two downstream-of labels on this PR.
        let err = resolve_declaration(
            SCHEMAS,
            RUNNER,
            7,
            &inputs(
                &[
                    "coord:downstream-of=qontinui-schemas#104",
                    "coord:downstream-of=qontinui-schemas#105",
                ],
                &[],
            ),
        )
        .unwrap_err();
        assert!(err.contains("contradictory"), "got: {err}");
        assert!(err.contains("#104") && err.contains("#105"), "got: {err}");

        // (2) two sibling PRs both claiming this one.
        let err = resolve_declaration(
            SCHEMAS,
            RUNNER,
            7,
            &inputs(
                &[],
                &[
                    (104, &["coord:upstream-of=qontinui-runner#7"]),
                    (105, &["coord:upstream-of=qontinui-runner#7"]),
                ],
            ),
        )
        .unwrap_err();
        assert!(err.contains("contradictory"), "got: {err}");

        // (3) the two forms naming DIFFERENT PRs.
        let err = resolve_declaration(
            SCHEMAS,
            RUNNER,
            7,
            &inputs(
                &["coord:downstream-of=qontinui-schemas#104"],
                &[(105, &["coord:upstream-of=qontinui-runner#7"])],
            ),
        )
        .unwrap_err();
        assert!(err.contains("contradictory"), "got: {err}");
    }

    /// Duplicates of the SAME declaration are not a contradiction.
    #[test]
    fn duplicate_identical_declarations_are_not_ambiguous() {
        let got = resolve_declaration(
            SCHEMAS,
            RUNNER,
            7,
            &inputs(
                &[
                    "coord:downstream-of=qontinui-schemas#104",
                    "coord:downstream-of=qontinui/qontinui-schemas#104",
                ],
                &[],
            ),
        )
        .unwrap();
        assert!(matches!(got, DeclaredTarget::Pr { number: 104, .. }));
    }

    /// Labels naming a DIFFERENT sibling, or a different PR of this repo, are
    /// simply not ours.
    #[test]
    fn unrelated_declarations_are_ignored() {
        let got = resolve_declaration(
            SCHEMAS,
            RUNNER,
            7,
            &inputs(
                &["coord:downstream-of=some-other-repo#1"],
                &[
                    (104, &["coord:upstream-of=qontinui-runner#8"]),
                    (105, &["coord:upstream-of=qontinui-web#7"]),
                ],
            ),
        )
        .unwrap();
        assert_eq!(got, DeclaredTarget::None);
        // `#12` must not match `#123`.
        let got = resolve_declaration(
            SCHEMAS,
            RUNNER,
            12,
            &inputs(&[], &[(104, &["coord:upstream-of=qontinui-runner#123"])]),
        )
        .unwrap();
        assert_eq!(got, DeclaredTarget::None);
    }

    /// A malformed declaration ON THIS PR is the author's error and reds; a
    /// malformed label on somebody else's PR is not ours to red.
    #[test]
    fn malformed_labels_red_only_when_they_are_ours() {
        for bad in [
            "coord:downstream-of=qontinui-schemas#not-a-number",
            "coord:downstream-of=qontinui-schemas#",
        ] {
            let err = resolve_declaration(SCHEMAS, RUNNER, 7, &inputs(&[bad], &[])).unwrap_err();
            assert!(err.contains("malformed label"), "{bad}: got {err}");
        }
        let got = resolve_declaration(
            SCHEMAS,
            RUNNER,
            7,
            &inputs(&[], &[(104, &["coord:upstream-of=garbage"])]),
        )
        .unwrap();
        assert_eq!(got, DeclaredTarget::None);
    }

    // ── declaration validation (pure) ─────────────────────────────────────

    fn good_detail() -> SiblingPrDetail {
        SiblingPrDetail {
            state: "open".into(),
            base_ref: "main".into(),
            head_ref: "feat/x".into(),
            head_sha: "a".repeat(40),
            head_repo_full_name: SCHEMAS.into(),
        }
    }

    #[test]
    fn a_valid_declaration_pins_to_its_head_sha() {
        let sha = validate_declared_pr(SCHEMAS, 104, &good_detail()).unwrap();
        assert_eq!(sha, "a".repeat(40));
    }

    #[test]
    fn declaration_validation_rejections() {
        let cases: Vec<(SiblingPrDetail, &str)> = vec![
            (
                SiblingPrDetail {
                    state: "closed".into(),
                    ..good_detail()
                },
                "not 'open'",
            ),
            (
                SiblingPrDetail {
                    base_ref: "feat/parent".into(),
                    ..good_detail()
                },
                "not 'main'",
            ),
            (
                SiblingPrDetail {
                    head_repo_full_name: "fork/qontinui-schemas".into(),
                    ..good_detail()
                },
                "not fetchable",
            ),
            (
                SiblingPrDetail {
                    head_sha: "abc123".into(),
                    ..good_detail()
                },
                "40-character hex",
            ),
        ];
        for (detail, needle) in cases {
            let err = validate_declared_pr(SCHEMAS, 104, &detail).unwrap_err();
            assert!(err.contains(needle), "expected {needle:?}, got: {err}");
            // Every rejection says it is about the DECLARATION, so nobody
            // reads it as an infra flake and retries.
            assert!(err.contains("declared adaptation"), "got: {err}");
        }
    }

    /// Only a full 40-hex id may be handed to `git fetch` as a pin.
    #[test]
    fn sha_shape_gate() {
        assert!(is_full_sha(&"0".repeat(40)));
        assert!(is_full_sha("a543cc7e1fab11c83299b3f8648a4686f223a093"));
        assert!(!is_full_sha("a543cc7e1"));
        assert!(!is_full_sha(&"z".repeat(40)));
        assert!(!is_full_sha(""));
    }

    #[test]
    fn clone_urls_are_public_https() {
        assert_eq!(
            clone_url(SCHEMAS),
            "https://github.com/qontinui/qontinui-schemas.git"
        );
    }
}
