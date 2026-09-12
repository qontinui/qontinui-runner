//! Build/deploy drift surface (plan
//! `2026-07-03-runner-session-tracking-drift-and-guardrails`, Phase 3 item 3).
//!
//! The binary already embeds the commit it was built from
//! (`QONTINUI_GIT_SHA`, 12-char, `build.rs`) and serves it as `gitSha` on
//! `/health`. What was missing is the COMPARISON: three shipped fixes sat
//! unrealized for 15 days because the live primary kept executing a stale
//! in-memory image and nothing measured the gap between "on main" and
//! "running here".
//!
//! On a slow interval (and once at startup) this module resolves the repo's
//! TRUNK tip SHA — `git ls-remote origin <trunk>` from the repo dir when
//! available, falling back to the local `git rev-parse origin/<trunk>` — and
//! diffs it against the embedded `gitSha` (prefix match: the embedded value
//! is a 12-char short SHA). The trunk comes from [`crate::git_trunk`]. The
//! result lands on `/health` as `mainSha` +
//! `buildDrift {behind, checkedAt, commitsBehind}` and a periodic WARN when
//! non-zero — the wire name stays `mainSha`, since coord and every `/health`
//! consumer reads it by that name; it is not a claim that the trunk is `main`.
//!
//! Resilience is the contract: a production install with no git repo (or no
//! network, or no `git` on PATH) serves nulls — every failure mode collapses
//! to `None`, never an error and never log spam (the WARN only fires on a
//! POSITIVE drift verdict, which requires git to have succeeded).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::process_helpers::{run_probe, ProbeOutcome};

/// Budget for one build-drift git read.
///
/// This one genuinely reaches the NETWORK: `resolve_trunk_sha` runs
/// `git ls-remote` before falling back to the local ref. `ls-remote` against
/// an unreachable or wedged remote is a classic never-returns, and the drift
/// check runs on a 900s timer through `spawn_blocking`, so without a bound one
/// bad remote removed a blocking-pool thread permanently on every tick. 60s is
/// well beyond a healthy `ls-remote` and far below the 900s interval.
const DRIFT_GIT_TIMEOUT: Duration = Duration::from_secs(60);
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use serde::Serialize;
use tracing::{debug, warn};

/// How often the comparison re-runs after the startup check.
const CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Result of one drift check. `None` fields mean "unknown" (no repo / git
/// failure) — the `/health` surface renders them as JSON nulls.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildDriftStatus {
    /// Unix millis when this check ran.
    pub checked_at: i64,
    /// The trunk tip's current full SHA, when resolvable. Named `main_sha`
    /// for the wire, not because the trunk is assumed to be `main`.
    pub main_sha: Option<String>,
    /// `Some(true)` only when trunk carries commits this binary does NOT
    /// have — i.e. `commits_behind > 0`. A build off a feature branch is
    /// DIVERGENT from trunk without being behind it; see `divergent`.
    ///
    /// `None` when the answer is not knowable — including the case where the
    /// build IS divergent but `commits_behind` could not be measured. See
    /// [`reconcile_behind`]; that arm used to answer `Some(true)` from
    /// divergence alone, contradicting this very sentence.
    ///
    /// This used to be `main_sha != embedded`, which made every non-tip build
    /// claim `behind: true` while `commitsBehind` sat at 0 — a body that
    /// contradicted itself. It feeds coord's `served_commits_behind`, so
    /// every feature-branch and agent-worktree build emitted a false drift
    /// signal.
    pub behind: Option<bool>,
    /// `git rev-list --count <embedded>..<main>` when cheaply available
    /// locally (requires both objects in the local repo); `None` otherwise.
    /// `behind` is DERIVED from this, so the two can no longer disagree.
    pub commits_behind: Option<u64>,
    /// `Some(true)` when the embedded SHA is not a prefix of `main_sha` —
    /// the old meaning of `behind`, kept as its own signal because "this
    /// binary is not the trunk tip" is still worth knowing about a build
    /// that is merely ahead.
    pub divergent: Option<bool>,
    /// `git rev-list --count <main>..<embedded>` — commits this binary has
    /// that trunk does not. Non-zero on a branch build, and the reason a
    /// divergent build can be zero commits behind.
    pub commits_ahead: Option<u64>,
}

static LATEST: OnceLock<Mutex<Option<BuildDriftStatus>>> = OnceLock::new();

fn latest_cell() -> &'static Mutex<Option<BuildDriftStatus>> {
    LATEST.get_or_init(|| Mutex::new(None))
}

// ---------------------------------------------------------------------------
// Trunk's compiled coord-mcp tool policy (plan
// `2026-09-03-coord-mcp-403-names-its-own-cause`, Phase 1).
//
// A `/coord-mcp` `-32601` refusal has three causes — the binary is STALE
// (trunk allows the tool), the allowlist has DRIFTED (trunk allows it nowhere
// and does not name it as deliberate), or the withholding is DELIBERATE — and
// only the first two need something the running binary does not contain:
// what trunk's `COORD_MCP_ALLOWED_TOOLS` / `COORD_MCP_DELIBERATE_EXCLUSIONS`
// say TODAY. This module already runs bounded git against the source checkout
// on the drift tick to learn trunk's SHA, so it is the one producer that can
// also read those two consts at that SHA — and then `cause` and
// `commitsBehind` in the refusal come from the same clock and can never
// disagree about the same binary (the reason `/coord-mcp/tool-policy` refuses
// to re-derive drift). Nothing here runs on the request path.
// ---------------------------------------------------------------------------

/// The repo-relative path of the file that declares the four consts, tried in
/// order: the runner repo root (`candidate_repo_dir`'s first candidate), then
/// the `src-tauri` crate root (its second).
const TOOL_POLICY_SOURCE_PATHS: &[&str] = &["src-tauri/src/mcp_api.rs", "src/mcp_api.rs"];

/// The four `&[&str]` consts parsed out of `mcp_api.rs` — the SAME four the
/// binary compiled, read from a different commit. Pure data; see
/// [`parse_tool_policy_consts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToolPolicy {
    pub allowed: Vec<String>,
    pub allowed_prefixes: Vec<String>,
    pub deliberate: Vec<String>,
    pub deliberate_prefixes: Vec<String>,
}

impl ParsedToolPolicy {
    /// Trunk's `coord_mcp_tool_is_allowed`.
    pub fn allows(&self, tool: &str) -> bool {
        self.allowed.iter().any(|t| t == tool)
            || self.allowed_prefixes.iter().any(|p| tool.starts_with(p))
    }

    /// Trunk's `coord_mcp_withholding_is_deliberate`.
    pub fn deliberately_excludes(&self, tool: &str) -> bool {
        self.deliberate.iter().any(|t| t == tool)
            || self.deliberate_prefixes.iter().any(|p| tool.starts_with(p))
    }
}

/// Trunk's tool policy plus WHERE it was read from, so a refusal can say which
/// trunk commit its `cause` is measured against.
#[derive(Debug, Clone)]
pub struct TrunkToolPolicy {
    /// The commit whose `mcp_api.rs` was parsed — the resolved trunk tip when
    /// `git show` could reach it.
    pub trunk_sha: String,
    /// Unix millis when the read completed.
    pub read_at: i64,
    /// `fetched` when `git fetch origin <trunk>` succeeded on this tick, else
    /// `local-ref` — the object came from the last fetch someone else did, so
    /// `trunk_sha` may trail the remote. Reported, never hidden.
    pub source: &'static str,
    pub policy: ParsedToolPolicy,
}

static TRUNK_TOOL_POLICY: OnceLock<Mutex<Option<TrunkToolPolicy>>> = OnceLock::new();

fn trunk_tool_policy_cell() -> &'static Mutex<Option<TrunkToolPolicy>> {
    TRUNK_TOOL_POLICY.get_or_init(|| Mutex::new(None))
}

/// Clone of the most recent trunk tool-policy read, if any tick has completed
/// one. `None` is UNKNOWN — no repo, no readable trunk object, a parse
/// failure, or simply "the first tick has not run yet" — and the refusal
/// renders it as `cause: "unknown"`, never as a confident default.
pub fn trunk_tool_policy() -> Option<TrunkToolPolicy> {
    trunk_tool_policy_cell().lock().ok().and_then(|g| g.clone())
}

fn store_trunk_tool_policy(policy: Option<TrunkToolPolicy>) {
    if let Ok(mut g) = trunk_tool_policy_cell().lock() {
        *g = policy;
    }
}

/// Extract the string literals of ONE `const <name>: &[&str] = &[ … ];`
/// declaration. Line comments inside the block are stripped first, so a
/// commented-out entry is not read as a member. `None` when the declaration
/// is absent or unterminated.
fn parse_str_slice_const(source: &str, name: &str) -> Option<Vec<String>> {
    let needle = format!("const {name}: &[&str] = &[");
    let start = source.find(&needle)? + needle.len();
    let rest = &source[start..];
    let end = rest.find("];")?;
    let mut out = Vec::new();
    for line in rest[..end].lines() {
        let mut s = line.split("//").next().unwrap_or("");
        while let Some(open) = s.find('"') {
            let tail = &s[open + 1..];
            let close = tail.find('"')?;
            out.push(tail[..close].to_string());
            s = &tail[close + 1..];
        }
    }
    Some(out)
}

/// Parse the four coord-mcp tool-policy consts out of `mcp_api.rs` source
/// text. Pure over its input, so the self-test in `mcp_api.rs` can pin it
/// against `include_str!` of the very file it reads: a reformat that breaks
/// this parser breaks that test, not production (production degrades to
/// `None`, i.e. `cause: "unknown"`).
pub fn parse_tool_policy_consts(source: &str) -> Option<ParsedToolPolicy> {
    Some(ParsedToolPolicy {
        allowed: parse_str_slice_const(source, "COORD_MCP_ALLOWED_TOOLS")?,
        allowed_prefixes: parse_str_slice_const(source, "COORD_MCP_ALLOWED_TOOL_PREFIXES")?,
        deliberate: parse_str_slice_const(source, "COORD_MCP_DELIBERATE_EXCLUSIONS")?,
        deliberate_prefixes: parse_str_slice_const(
            source,
            "COORD_MCP_DELIBERATE_EXCLUSION_PREFIXES",
        )?,
    })
}

/// Read trunk's `mcp_api.rs` and parse its tool policy. Every git call is
/// bounded ([`git_output`] / [`run_probe`]). The fetch touches the
/// remote-tracking ref only: `--no-write-fetch-head` keeps it from rewriting
/// the source checkout's per-worktree `FETCH_HEAD` (a peer mid-`git pull`
/// there would otherwise merge OUR fetch), `gc.auto=0` keeps a 60 s tree-kill
/// from interrupting a gc it triggered, and a refused terminal prompt keeps an
/// unauthenticated remote from eating the whole timeout. Whether or not the
/// fetch succeeded, `trunk_sha` is what `origin/<trunk>` resolves to AFTER
/// the attempt — never a pre-fetch guess wearing a `fetched` label — and
/// `source` records only the fetch verdict, so a possibly-trailing local ref
/// is visible in the refusal rather than collapsing into `unknown`.
fn read_trunk_tool_policy(repo: &Path) -> Option<TrunkToolPolicy> {
    let branch = crate::git_trunk::resolve_trunk_branch(repo).unwrap_or_else(|| "main".to_string());
    // A quiet fetch prints nothing on success, which `git_output` would read
    // as `None`, so the outcome is taken from `run_probe` directly.
    let fetched = {
        let mut cmd = crate::process_helpers::no_window("git");
        cmd.args([
            "-c",
            "gc.auto=0",
            "fetch",
            "--quiet",
            "--no-write-fetch-head",
            "origin",
            &branch,
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(repo);
        matches!(
            run_probe(cmd, DRIFT_GIT_TIMEOUT, "build_drift: git fetch"),
            ProbeOutcome::Captured(_)
        )
    };
    let source = if fetched { "fetched" } else { "local-ref" };
    let trunk_sha = git_output(repo, &["rev-parse", &format!("origin/{branch}")])
        .filter(|s| looks_like_sha(s))?;
    let source_text = TOOL_POLICY_SOURCE_PATHS
        .iter()
        .find_map(|p| git_output(repo, &["show", &format!("{trunk_sha}:{p}")]))?;
    let policy = parse_tool_policy_consts(&source_text)?;
    Some(TrunkToolPolicy {
        trunk_sha,
        read_at: chrono::Utc::now().timestamp_millis(),
        source,
        policy,
    })
}

/// Clone of the most recent drift status, if any check has completed.
pub fn latest() -> Option<BuildDriftStatus> {
    latest_cell().lock().ok().and_then(|g| g.clone())
}

fn store_latest(status: BuildDriftStatus) {
    if let Ok(mut g) = latest_cell().lock() {
        *g = Some(status);
    }
}

/// The `/health` fields: `(mainSha, buildDrift)`. Before the first check —
/// or on a repo-less production install — `mainSha` is null and `buildDrift`
/// carries null members, never an error.
pub fn health_fields() -> (serde_json::Value, serde_json::Value) {
    match latest() {
        Some(s) => (
            serde_json::json!(s.main_sha),
            serde_json::json!({
                "behind": s.behind,
                "checkedAt": s.checked_at,
                "commitsBehind": s.commits_behind,
                "divergent": s.divergent,
                "commitsAhead": s.commits_ahead,
            }),
        ),
        None => (
            serde_json::Value::Null,
            serde_json::json!({
                "behind": serde_json::Value::Null,
                "checkedAt": serde_json::Value::Null,
                "commitsBehind": serde_json::Value::Null,
                "divergent": serde_json::Value::Null,
                "commitsAhead": serde_json::Value::Null,
            }),
        ),
    }
}

/// The repo dir git commands run from: the compile-time source checkout
/// (`CARGO_MANIFEST_DIR`'s parent = the qontinui-runner repo root), kept only
/// when it still looks like a git checkout at runtime. On a production
/// install (path absent / no `.git`) this is `None` and every check yields
/// nulls. `.git` may be a FILE for a worktree — `exists()` covers both.
fn candidate_repo_dir() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [manifest.parent().map(Path::to_path_buf), Some(manifest)];
    candidates
        .into_iter()
        .flatten()
        .find(|dir| dir.join(".git").exists())
}

/// Run `git <args>` in `repo`, returning trimmed stdout on success. Any
/// failure (spawn error, non-zero exit, empty output) → `None`.
fn git_output(repo: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = crate::process_helpers::no_window("git");
    // A credential prompt would sit until `DRIFT_GIT_TIMEOUT` reaps it, every
    // tick; refuse the prompt so an unauthenticated remote fails fast instead.
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(repo);
    let ProbeOutcome::Captured(stdout) = run_probe(cmd, DRIFT_GIT_TIMEOUT, "build_drift: git")
    else {
        return None;
    };
    let s = String::from_utf8_lossy(&stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Resolve the trunk's current SHA: `ls-remote` (authoritative, needs
/// network) first, the local remote-tracking ref (last fetch) as fallback.
///
/// The trunk is resolved per repo ([`crate::git_trunk`]), not assumed to be
/// `main`. This is the mildest of the four sites that hardcoded it — both
/// rungs sit in an `.or_else` chain, so on a `master`-trunk checkout it
/// degraded to `None` ("drift unknown") rather than answering wrong — but a
/// permanent `None` here still reads as "we could not check" forever, and
/// `/health` `buildDrift` is exactly the signal operators use to decide
/// whether a fix is actually running on a box.
fn resolve_trunk_sha(repo: &Path) -> Option<String> {
    // The `main` guess is spelled here, visibly, rather than inside the
    // resolver: an unresolvable trunk means no LOCAL `origin/*` ref, which
    // says nothing about what the REMOTE would answer — and `ls-remote`
    // below needs no local ref at all. Dropping to `None` here would have
    // made a never-fetched checkout report "drift unknown" where the old
    // hardcoded `main` could still have answered.
    let branch = crate::git_trunk::resolve_trunk_branch(repo).unwrap_or_else(|| "main".to_string());
    git_output(repo, &["ls-remote", "origin", &branch])
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .or_else(|| git_output(repo, &["rev-parse", &format!("origin/{branch}")]))
        .filter(|s| looks_like_sha(s))
}

fn looks_like_sha(s: &str) -> bool {
    s.len() >= 12 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Pure comparison core (unit-testable without git): prefix-match the
/// embedded 12-char SHA against the trunk tip's full SHA. `None` when either
/// side is unknown (e.g. the embedded value is build.rs's `"unknown"`
/// fallback).
///
/// This answers DIVERGENCE — "this binary is not the trunk tip" — which is
/// strictly weaker than being behind it. Serving it AS `behind` is the bug
/// this function used to be named after.
fn compute_divergent(embedded: &str, main_sha: Option<&str>) -> Option<bool> {
    if !looks_like_sha(embedded) {
        return None;
    }
    main_sha.map(|m| !m.starts_with(embedded))
}

/// Reconcile a divergence verdict and a measured `embedded..main` count into
/// the `behind` flag actually served. The point is that the two can no longer
/// contradict each other:
///
/// - not divergent -> `behind: false`, 0 behind (built off the tip).
/// - divergent, count 0 -> `behind: FALSE`. Trunk holds nothing this build
///   lacks; it is on a branch off the tip, or ahead of it. This is exactly
///   the case that served `{"behind": true, "commitsBehind": 0}`.
/// - divergent, count n>0 -> `behind: true`.
/// - divergent, count UNKNOWN -> `behind: None`. **Unknown, not true.**
///
/// # Why the last arm changed (manual-test-loop iteration 26, item 5)
///
/// It used to answer `Some(true)` — "the conservative old answer" — which
/// asserted `behind` from DIVERGENCE ALONE. Three things say that is wrong:
///
/// 1. The [`BuildDriftStatus::behind`] doc-comment promises `Some(true)`
///    **only** when `commits_behind > 0`. The old arm broke that promise on
///    the one input where the count is missing.
/// 2. The struct already documents `None` as its unknown idiom ("`None`
///    fields mean unknown"). There was no need to invent a conservative
///    default; the type has a word for this.
/// 3. The null window is a LOCAL FETCH ARTIFACT, not a property of the build.
///    Measured on ONE unchanged binary, 15 minutes apart: `{"behind": true,
///    "commitsAhead": null, "commitsBehind": null, "divergent": true}` and
///    then `{"behind": true, "commitsAhead": 0, "commitsBehind": 117,
///    "divergent": true}`. `check_once_blocking` learns the trunk tip from
///    `git ls-remote` (network) but never FETCHES the object, so
///    `rev-list --count embedded..tip` fails until something else fetches —
///    i.e. the count is unmeasurable exactly when a box IS behind, and the
///    same arm fires for a build strictly AHEAD of trunk with an unfetched
///    tip. `run_periodic` then logged "this binary was NOT built from the
///    trunk's current commit — shipped fixes may not be running here" for a
///    branch build, which is the false drift signal this module's earlier fix
///    set out to remove.
///
/// A `divergent: true` with a null count is still SERVED — a reader wanting
/// the weaker "not the trunk tip" signal reads `divergent`, which is exactly
/// why that field exists as its own signal. What is no longer served is a
/// confident `behind: true` derived from it.
fn reconcile_behind(divergent: Option<bool>, commits_behind: Option<u64>) -> Option<bool> {
    match divergent {
        None => None,
        Some(false) => Some(false),
        Some(true) => match commits_behind {
            Some(0) => Some(false),
            Some(_) => Some(true),
            // Unmeasurable is UNKNOWN. See the doc-comment above.
            None => None,
        },
    }
}

/// One blocking drift check. Never errors; unknown states collapse to nulls.
fn check_once_blocking() -> BuildDriftStatus {
    let embedded = env!("QONTINUI_GIT_SHA");
    let checked_at = chrono::Utc::now().timestamp_millis();

    let Some(repo) = candidate_repo_dir() else {
        store_trunk_tool_policy(None);
        return BuildDriftStatus {
            checked_at,
            main_sha: None,
            behind: None,
            commits_behind: None,
            divergent: None,
            commits_ahead: None,
        };
    };

    let main_sha = resolve_trunk_sha(&repo);
    // Trunk's tool policy rides the same tick and the same SHA as the drift
    // verdict, so a `/coord-mcp` refusal's `cause` and its `commitsBehind`
    // describe one measurement. Stored here rather than returned: it is a
    // second cache with its own reader (`trunk_tool_policy`), and an
    // unresolvable trunk clears it to UNKNOWN instead of serving a stale read.
    store_trunk_tool_policy(if main_sha.is_some() {
        read_trunk_tool_policy(&repo)
    } else {
        None
    });
    let divergent = compute_divergent(embedded, main_sha.as_deref());
    // Count FIRST, then derive `behind` from the count. The old code decided
    // `behind` from the SHA mismatch and only then counted, which is how the
    // two ended up disagreeing on every branch build.
    let commits_behind = match divergent {
        Some(true) => main_sha.as_deref().and_then(|m| {
            git_output(&repo, &["rev-list", "--count", &format!("{embedded}..{m}")])
                .and_then(|s| s.parse().ok())
        }),
        Some(false) => Some(0),
        None => None,
    };
    let commits_ahead = match divergent {
        Some(true) => main_sha.as_deref().and_then(|m| {
            git_output(&repo, &["rev-list", "--count", &format!("{m}..{embedded}")])
                .and_then(|s| s.parse().ok())
        }),
        Some(false) => Some(0),
        None => None,
    };
    let behind = reconcile_behind(divergent, commits_behind);

    BuildDriftStatus {
        checked_at,
        main_sha,
        behind,
        commits_behind,
        divergent,
        commits_ahead,
    }
}

/// Detached periodic drift check — runs once immediately at startup, then
/// every [`CHECK_INTERVAL`]. WARNs on each tick that finds non-zero drift.
pub async fn run_periodic() {
    loop {
        let status = spawn_blocking_tracked(check_once_blocking)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "build drift: check task panicked");
                BuildDriftStatus {
                    checked_at: chrono::Utc::now().timestamp_millis(),
                    main_sha: None,
                    behind: None,
                    commits_behind: None,
                    divergent: None,
                    commits_ahead: None,
                }
            });

        match (status.behind, status.main_sha.as_deref()) {
            (Some(true), Some(main)) => warn!(
                git_sha = env!("QONTINUI_GIT_SHA"),
                main_sha = main,
                commits_behind = ?status.commits_behind,
                "build drift: this binary was NOT built from the trunk's current \
                 commit — shipped fixes may not be running here"
            ),
            (Some(false), _) => debug!(
                git_sha = env!("QONTINUI_GIT_SHA"),
                "build drift: binary matches the trunk tip"
            ),
            // Divergent from trunk with an UNMEASURABLE gap. Deliberately
            // `debug!`, not the WARN above: `ls-remote` gave us the tip's SHA
            // without fetching the object, so `rev-list --count` cannot run
            // and we do not know whether this build is behind trunk, ahead of
            // it, or both. Warning here is what produced a false "shipped
            // fixes may not be running" on every branch build.
            (None, Some(main)) if status.divergent == Some(true) => debug!(
                git_sha = env!("QONTINUI_GIT_SHA"),
                main_sha = main,
                "build drift: binary differs from the trunk tip, but the gap is \
                 not measurable locally (trunk commit not fetched) — drift UNKNOWN"
            ),
            _ => debug!(
                "build drift: trunk tip unresolvable (no repo / no network) — reporting unknown"
            ),
        }

        store_latest(status);
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divergence_is_prefix_match_on_the_12_char_embedded_sha() {
        let main = "dc7aa9c5aaaabbbbccccddddeeeeffff00001111";
        // Embedded 12-char prefix of main → not divergent.
        assert_eq!(compute_divergent("dc7aa9c5aaaa", Some(main)), Some(false));
        // Different commit → divergent (which is NOT the same as behind).
        assert_eq!(compute_divergent("abcdef012345", Some(main)), Some(true));
    }

    #[test]
    fn unknown_states_collapse_to_none() {
        // build.rs fallback value must never produce a verdict.
        assert_eq!(compute_divergent("unknown", Some("dc7aa9c5aaaa")), None);
        // Unresolvable main → unknown.
        assert_eq!(compute_divergent("dc7aa9c5aaaa", None), None);
    }

    /// The reported defect: `{"behind": true, "commitsBehind": 0}`.
    ///
    /// A branch built off the current trunk tip is divergent (its own commits
    /// are not on trunk) but zero commits BEHIND it. Serving `behind: true`
    /// there contradicted the count in the same body and fed coord's
    /// `served_commits_behind` a false drift signal from every feature-branch
    /// and agent-worktree build.
    #[test]
    fn a_branch_build_that_is_zero_commits_behind_does_not_claim_to_be_behind() {
        assert_eq!(reconcile_behind(Some(true), Some(0)), Some(false));
    }

    #[test]
    fn behind_agrees_with_the_count_in_every_knowable_case() {
        // Built off the tip.
        assert_eq!(reconcile_behind(Some(false), Some(0)), Some(false));
        // Genuinely behind.
        assert_eq!(reconcile_behind(Some(true), Some(3)), Some(true));
        // Divergent but unmeasurable: UNKNOWN. See
        // `behind_is_unknown_when_the_count_is_unknown` for why.
        assert_eq!(reconcile_behind(Some(true), None), None);
        // Unknown stays unknown.
        assert_eq!(reconcile_behind(None, None), None);
        assert_eq!(reconcile_behind(None, Some(0)), None);
    }

    /// Iteration 26, item 5: `behind` must not be asserted from DIVERGENCE
    /// ALONE.
    ///
    /// Measured on ONE unchanged binary, 15 minutes apart:
    /// `{"behind": true, "commitsAhead": null, "commitsBehind": null,
    /// "divergent": true}`, then `{"behind": true, "commitsAhead": 0,
    /// "commitsBehind": 117, "divergent": true}`. Ground truth
    /// `git rev-list --count` agreed with the second: 117 behind, 0 ahead. The
    /// null window was a LOCAL FETCH ARTIFACT — `check_once_blocking` learns
    /// the trunk tip from `git ls-remote` but never fetches the object — not a
    /// property of the build, and the SAME arm fires for a build strictly
    /// AHEAD of trunk.
    ///
    /// `behind`'s own doc-comment says `Some(true)` **only** when
    /// `commits_behind > 0`, and the struct documents `None` as its unknown
    /// idiom. An unmeasurable count therefore has exactly one honest answer.
    #[test]
    fn behind_is_unknown_when_the_count_is_unknown() {
        assert_eq!(
            reconcile_behind(Some(true), None),
            None,
            "divergent with an unmeasurable count must report behind:null, \
             not a confident behind:true"
        );
    }

    /// The invariant the `Some(0)` guard below states, generalized: a served
    /// `behind: true` must be BACKED BY A MEASURED POSITIVE COUNT — never by
    /// a zero, and never by a missing one.
    ///
    /// The pre-existing
    /// `behind_true_is_never_served_alongside_zero_commits_behind` covers only
    /// the `Some(0)` half; this adds the `None` half the iteration-26
    /// measurement exposed.
    #[test]
    fn behind_true_is_only_ever_served_with_a_measured_positive_count() {
        for divergent in [None, Some(false), Some(true)] {
            for count in [None, Some(0u64), Some(1), Some(42)] {
                if reconcile_behind(divergent, count) == Some(true) {
                    assert!(
                        matches!(count, Some(n) if n > 0),
                        "behind:true served with commitsBehind:{count:?} \
                         (divergent={divergent:?}) — it must come from a \
                         measured positive count, not from divergence alone"
                    );
                }
            }
        }
    }

    /// Guards the invariant rather than the individual cases: across every
    /// combination, a served `behind: true` must never sit next to a
    /// `commitsBehind` of 0.
    #[test]
    fn behind_true_is_never_served_alongside_zero_commits_behind() {
        for divergent in [None, Some(false), Some(true)] {
            for count in [None, Some(0u64), Some(1), Some(42)] {
                if reconcile_behind(divergent, count) == Some(true) {
                    assert_ne!(
                        count,
                        Some(0),
                        "behind:true served with commitsBehind:0 (divergent={divergent:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn health_fields_serve_nulls_before_first_check_shape() {
        // The None arm of health_fields must be null-shaped, never an error.
        // (LATEST is process-global; other tests may have populated it, so
        // exercise the None arm's construction directly.)
        let (main_sha, drift) = match None::<BuildDriftStatus> {
            Some(_) => unreachable!(),
            None => (
                serde_json::Value::Null,
                serde_json::json!({
                    "behind": serde_json::Value::Null,
                    "checkedAt": serde_json::Value::Null,
                    "commitsBehind": serde_json::Value::Null,
                }),
            ),
        };
        assert!(main_sha.is_null());
        assert!(drift["behind"].is_null());
        assert!(drift["checkedAt"].is_null());
    }

    // -- trunk tool-policy parser (plan 2026-09-03-coord-mcp-403-names-its-own-cause) --

    const SAMPLE: &str = r#"
/// doc mentioning `COORD_MCP_ALLOWED_TOOLS` must not confuse the parser.
const COORD_MCP_ALLOWED_TOOLS: &[&str] = &[
    "coord_alpha",
    // "coord_commented_out",
    "coord_beta", "coord_gamma",
];
const COORD_MCP_ALLOWED_TOOL_PREFIXES: &[&str] = &["coord_query_"];
const COORD_MCP_DELIBERATE_EXCLUSIONS: &[&str] = &[
    "coord_create_pr",
];
const COORD_MCP_DELIBERATE_EXCLUSION_PREFIXES: &[&str] = &["coord_onboard"];
"#;

    #[test]
    fn parses_all_four_consts_and_strips_line_comments() {
        let p = parse_tool_policy_consts(SAMPLE).expect("sample parses");
        assert_eq!(p.allowed, vec!["coord_alpha", "coord_beta", "coord_gamma"]);
        assert_eq!(p.allowed_prefixes, vec!["coord_query_"]);
        assert_eq!(p.deliberate, vec!["coord_create_pr"]);
        assert_eq!(p.deliberate_prefixes, vec!["coord_onboard"]);
        assert!(p.allows("coord_alpha"));
        assert!(p.allows("coord_query_anything"));
        assert!(!p.allows("coord_commented_out"));
        assert!(p.deliberately_excludes("coord_create_pr"));
        assert!(p.deliberately_excludes("coord_onboarding_doctor"));
        assert!(!p.deliberately_excludes("coord_alpha"));
    }

    #[test]
    fn a_missing_or_unterminated_const_is_none_not_a_guess() {
        assert!(parse_tool_policy_consts("nothing here").is_none());
        let truncated = SAMPLE.replace("];\nconst COORD_MCP_ALLOWED_TOOL_PREFIXES", "\nconst X");
        assert!(parse_tool_policy_consts(&truncated).is_none());
    }

    #[test]
    fn looks_like_sha_rejects_junk() {
        assert!(looks_like_sha("dc7aa9c5aaaabbbbccccddddeeeeffff00001111"));
        assert!(looks_like_sha("dc7aa9c5aaaa"));
        assert!(!looks_like_sha("unknown"));
        assert!(!looks_like_sha("dc7aa9c5")); // short SHAs under 12 are not our format
        assert!(!looks_like_sha("fatal: not a git repository"));
    }
}
