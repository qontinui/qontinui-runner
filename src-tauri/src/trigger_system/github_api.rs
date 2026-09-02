//! Lightweight GitHub REST API client for the PR watcher.
//!
//! Uses reqwest with token-based authentication. Rate-limit aware.
//!
//! # Budget discipline (plan
//! `2026-08-30-github-rest-budget-is-structurally-oversubscribed`, Phase B)
//!
//! Every exchange this module makes goes through [`GitHubClient::send_metered`]
//! and is folded into [`crate::github_budget`]. Two consequences are worth
//! stating out loud because both were defects before:
//!
//! 1. **Back-off reads one bucket.** The rate-limit counters used to live on the
//!    client instance, and four clients are constructed across this process —
//!    so four disjoint counters each tracked a quarter of the evidence about one
//!    shared GitHub bucket. The instance state is gone; back-off now consults
//!    the process-global reading.
//! 2. **Safe GETs are conditional.** A `304 Not Modified` does not decrement the
//!    GitHub budget, so every cacheable read echoes its held `ETag`. What that
//!    actually BUYS is measured, not assumed: coord observed
//!    `/repos/{o}/{r}/pulls/{n}` caching at ~1,446 charged against 12
//!    not-modified — GitHub embeds the mutable `head.repo`/`base.repo` object in
//!    every PR representation, so a push to any branch invalidates every open
//!    PR's validator in that repo at once — while its sibling
//!    `/pulls/{n}/files` cached at 93%. The `cacheMode` dimension on each
//!    recorded row is what will tell us which of ours land where.

use bytes::Bytes;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, ETAG, IF_NONE_MATCH, LINK, USER_AGENT,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

use crate::github_budget::{self, CacheMode};

/// Maximum total results to accumulate across paginated GitHub API calls.
const PAGINATION_CAP: usize = 500;

/// Requests left in the shared bucket at or below which a client sleeps until
/// reset rather than issuing more. Unchanged from the per-instance counters this
/// replaced — only the SOURCE of the reading changed.
const RATE_LIMIT_FLOOR: i64 = 10;

/// Longest single back-off, in seconds. Preserved verbatim from the per-instance
/// implementation: a GitHub reset can be an hour out, and blocking a poller for
/// an hour is indistinguishable from the poller being dead.
const MAX_BACKOFF_SECS: u64 = 300;

/// Cache key under which a page's `Link: rel="next"` is stored beside its body.
///
/// A `304` is not obliged to repeat `Link` — RFC 9110 names it among neither the
/// fields a validator response must send nor those it may omit — and a paginated
/// loop that reads an absent `Link` as "last page" returns a SILENTLY TRUNCATED
/// result, which is the wrong answer in the safe-looking direction. So the
/// header is cached alongside the body and replayed with it. A NUL byte cannot
/// occur in a URL, so this key can never collide with a real one.
pub(crate) fn link_cache_key(url: &str) -> String {
    format!("{url}\0link")
}

/// The `rel="next"` URL cached beside `url`'s body, if any.
pub(crate) fn cached_link_next(url: &str) -> Option<String> {
    let raw = github_budget::replay(&link_cache_key(url))?;
    let s = String::from_utf8(raw.to_vec()).ok()?;
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Unix seconds, saturating to 0 on a clock before the epoch.
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// How long to sleep before the single retry of a rate-limited response, or
/// `None` when this response must not be retried. Pure, unit-tested.
///
/// The two arms are genuinely different signals and were already treated as
/// such: a `429` carries `Retry-After`, while a `403` is a rate-limit refusal
/// only when it also says `x-ratelimit-remaining: 0` — a permission `403` must
/// not be slept on.
pub(crate) fn retry_backoff_secs(
    status: u16,
    retry_after: Option<u64>,
    remaining: Option<i64>,
    reset_at_unix: Option<i64>,
    now: i64,
) -> Option<u64> {
    let wait = match status {
        429 => retry_after.unwrap_or(60),
        403 if remaining == Some(0) => match reset_at_unix {
            Some(reset) if reset > now => (reset - now) as u64,
            _ => 60,
        },
        _ => return None,
    };
    Some(wait.min(MAX_BACKOFF_SECS))
}

/// A `403` that is NOT a rate-limit refusal — a permission or SSO failure. It is
/// terminal: retrying it burns budget to be told the same thing. Pure,
/// unit-tested.
pub(crate) fn is_terminal_403(status: u16, remaining: Option<i64>) -> bool {
    status == 403 && remaining != Some(0)
}

/// What a completed exchange means for the ETag entry the caller holds. Pure,
/// unit-tested, and shared with [`crate::ci_node::sibling`] so the two doors
/// cannot drift into disagreeing about when a cached body is still replayable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheAction {
    /// A `200` carrying a validator — cache the body against it.
    Store,
    /// A `200` with NO validator, or any non-success. Whatever we held is no
    /// longer known to be replayable, so stop claiming it is. This is the arm
    /// that keeps a deleted PR's body from being served forever.
    DropEntry,
    /// Nothing to decide — an unconditional read or a write.
    LeaveAlone,
}

pub(crate) fn cache_action(cache_mode: CacheMode, status: u16, has_etag: bool) -> CacheAction {
    if cache_mode != CacheMode::Cached {
        return CacheAction::LeaveAlone;
    }
    let success = (200..300).contains(&status);
    match (success, has_etag) {
        (true, true) => CacheAction::Store,
        _ => CacheAction::DropEntry,
    }
}

/// Answer a `304 Not Modified` out of the ETag cache.
///
/// `None` means the validator outlived the body it names — LRU byte eviction
/// between the `etag_for` that armed the request and the `replay` that would
/// answer it. That is recoverable (re-read unconditionally), but it must never
/// be mistaken for an empty body, so it is reported as absence rather than as an
/// empty `GhResponse`.
///
/// `header_link_next` takes precedence over the cached one when GitHub bothered
/// to repeat `Link` on the 304; see [`link_cache_key`] for why we cannot rely on
/// it doing so.
fn replay_not_modified(url: &str, header_link_next: Option<String>) -> Option<GhResponse> {
    let body = github_budget::replay(url)?;
    Some(GhResponse {
        status: reqwest::StatusCode::OK,
        body,
        link_next: header_link_next.or_else(|| cached_link_next(url)),
        from_cache: true,
    })
}

/// One completed GitHub exchange, body already read.
///
/// The send path has to read the body itself: a `304` carries none, and the only
/// honest way to answer the caller is with the body held in the ETag cache.
/// A live `reqwest::Response` cannot express that, so every caller in this
/// module reads from here instead.
#[derive(Debug)]
pub struct GhResponse {
    status: reqwest::StatusCode,
    body: Bytes,
    /// `rel="next"` for a paginated read, resolved at construction — see
    /// [`link_cache_key`] for why it is not simply read off the headers.
    link_next: Option<String>,
    /// True when `body` was replayed from the ETag cache after a `304`, i.e.
    /// this answer cost nothing against the GitHub budget.
    from_cache: bool,
}

impl GhResponse {
    pub fn status(&self) -> reqwest::StatusCode {
        self.status
    }

    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// True when this body came back free, from the ETag cache.
    pub fn served_from_cache(&self) -> bool {
        self.from_cache
    }

    /// The `rel="next"` page URL, header-or-cache resolved.
    pub fn next_page_url(&self) -> Option<String> {
        self.link_next.clone()
    }

    /// The body as text, lossily decoded — the same treatment
    /// `reqwest::Response::text` gives it.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_slice(&self.body).map_err(|e| e.to_string())
    }
}

/// The parts of one wire exchange the send path needs after the body is read.
struct RawExchange {
    status: reqwest::StatusCode,
    etag: Option<String>,
    link_next: Option<String>,
    retry_after: Option<u64>,
    remaining: Option<i64>,
    reset_at_unix: Option<i64>,
    body: Bytes,
}

/// Extract the `rel="next"` URL from a GitHub `Link` header value.
pub fn extract_next_url(link_header: &str) -> Option<String> {
    for part in link_header.split(',') {
        let trimmed = part.trim();
        if trimmed.ends_with("rel=\"next\"") {
            if let Some(url) = trimmed.strip_suffix("; rel=\"next\"") {
                let url = url.trim();
                if url.starts_with('<') && url.ends_with('>') {
                    return Some(url[1..url.len() - 1].to_string());
                }
            }
        }
    }
    None
}

/// Build the `GET /pulls?state=all&head=<owner>:<branch>` URL for
/// [`GitHubClient::list_prs_for_head`], percent-encoding each interpolated
/// segment. The branch is the load-bearing one: git ref names legally contain
/// URL-reserved characters (`#` most notably), and an unencoded `#` truncates
/// the query as a fragment — GitHub then ignores the malformed `head` filter
/// and returns ALL PRs. `state=all` so a merged/closed PR — the green case the
/// session dropdown must render — still resolves. Pure, unit-tested.
fn prs_by_head_url(owner: &str, repo: &str, branch: &str) -> String {
    format!(
        "https://api.github.com/repos/{}/{}/pulls?state=all&head={}:{}&per_page=20",
        urlencoding::encode(owner),
        urlencoding::encode(repo),
        urlencoding::encode(owner),
        urlencoding::encode(branch)
    )
}

/// The URL builders below exist so a read and the write that invalidates it
/// produce the BYTE-IDENTICAL string. The ETag cache keys on the full URL
/// including its query, so an invalidation spelled out by hand at the write site
/// would miss by a `?per_page=100` and leave a stale entry behind — a failure
/// that shows up as a wrong answer, not as an error. Pure, unit-tested.
fn pr_url(owner: &str, repo: &str, number: u64) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/pulls/{number}")
}

fn check_runs_url(owner: &str, repo: &str, sha: &str) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/commits/{sha}/check-runs?per_page=100")
}

fn pr_reviews_url(owner: &str, repo: &str, number: u64) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/pulls/{number}/reviews?per_page=100")
}

fn issue_comments_url(owner: &str, repo: &str, number: u64) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}/comments")
}

fn job_logs_url(owner: &str, repo: &str, job_id: u64) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/actions/jobs/{job_id}/logs")
}

/// The chip `qontinui-coord` applies to a PR it fast-forward-landed
/// (`pr_merge::engine::FF_LAND_LABEL`, mirrored to GitHub by
/// `announce_ff_land`). It is coord's own land verdict, delivered on the PR
/// payload the runner already fetches — so reading it costs no extra request.
///
/// **Positive-only.** coord sets it best-effort and only after its explanatory
/// comment posts, so its PRESENCE proves a land and its ABSENCE proves nothing
/// (`knowledge-base/qontinui-specific/coord-ff-lands.md`).
pub const COORD_LANDED_LABEL: &str = "coord:landed";

/// Label names off a PR payload's `labels` array.
///
/// A payload with no `labels` key — or one whose entries carry no `name` —
/// yields an EMPTY vec, which every caller must read as "no label observed",
/// never as "this PR has no labels". Pure, unit-tested.
pub fn parse_labels(pr: &serde_json::Value) -> Vec<String> {
    pr["labels"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Does this label set carry coord's [`COORD_LANDED_LABEL`] chip? Pure.
pub fn has_coord_landed_label(labels: &[String]) -> bool {
    labels.iter().any(|l| l == COORD_LANDED_LABEL)
}

/// A PR resolved from a head branch by [`GitHubClient::list_prs_for_head`].
/// Carries the head ref + sha so the attribution reconciler can verify the
/// PR's head-commit `Session-Id` trailer before recording it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadPr {
    pub number: u64,
    /// `"open"` | `"closed"`.
    pub state: String,
    /// True iff the PR was merged (the list endpoint has no `merged` boolean;
    /// `merged_at` being present is the equivalent signal).
    pub merged: bool,
    pub merged_at: Option<String>,
    /// RFC3339 close time, present for every non-open PR. The best available
    /// land timestamp for a fast-forward land, which sets no `merged_at`.
    pub closed_at: Option<String>,
    pub head_ref: String,
    pub head_sha: String,
    /// The PR's BASE branch name (e.g. `"main"`), unprefixed. The session-PR
    /// reconciler's content-proof land signal tests the head commit against
    /// `origin/<base_ref>`.
    pub base_ref: String,
    /// Label names on the PR. Carries coord's [`COORD_LANDED_LABEL`] chip,
    /// which is the land signal that survives a rebase (see the constant).
    ///
    /// `#[serde(default)]` is belt-and-braces only: this struct is built by
    /// hand from a `serde_json::Value` in this module and is not currently
    /// deserialized anywhere. It costs nothing and means a future
    /// `Deserialize` of an older payload yields an EMPTY set — "not observed"
    /// — rather than failing.
    #[serde(default)]
    pub labels: Vec<String>,
}

/// A minimal GitHub API client.
pub struct GitHubClient {
    client: reqwest::Client,
    token: String,
    /// Which subsystem's spend this client's calls are attributed to in the
    /// budget histogram. Several subsystems build their own client over the same
    /// credential, and telling them apart is the whole point of the row key.
    consumer: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrStatus {
    pub number: u64,
    pub state: String,
    pub merged: bool,
    pub mergeable: Option<bool>,
    pub head_sha: String,
    /// The PR's BASE branch name (e.g. `"main"`), unprefixed — see
    /// [`HeadPr::base_ref`].
    pub base_ref: String,
    /// RFC3339 close time — see [`HeadPr::closed_at`].
    pub closed_at: Option<String>,
    pub title: String,
    pub html_url: String,
    /// Label names on the PR — see [`HeadPr::labels`], including the note on
    /// `#[serde(default)]`.
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckRun {
    pub id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckRunsResponse {
    pub total_count: u64,
    pub check_runs: Vec<CheckRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReview {
    pub id: u64,
    pub state: String,
    pub user: ReviewUser,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewUser {
    pub login: String,
}

/// Aggregate CI status derived from check runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiStatus {
    Pending,
    Success,
    Failure { failed_checks: Vec<String> },
}

/// Aggregate review status derived from reviews.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewStatus {
    Pending,
    Approved,
    ChangesRequested,
}

impl GitHubClient {
    /// `consumer` is the budget-histogram label for whichever subsystem owns
    /// this client — `pr_watcher`, `session_pr_reconciler`, `review_subtask`.
    /// It never carries the token.
    pub fn new(token: &str, consumer: &'static str) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
        Ok(Self {
            client,
            token: token.to_string(),
            consumer,
        })
    }

    /// Wait if the SHARED bucket is close to empty.
    ///
    /// The reading comes from [`crate::github_budget`] rather than from this
    /// instance, because the bucket is per-credential and this process builds
    /// several clients over one credential. An instance counter can only ever
    /// see the fraction of the spend that went through it, so each client backed
    /// off late by however much the others had already drawn down.
    ///
    /// No reading at all is UNKNOWN, and UNKNOWN does not sleep: a cold process
    /// must be allowed to make its first request, which is what produces the
    /// first reading.
    async fn check_rate_limit(&self) {
        let Some(obs) = github_budget::last_rate_limit() else {
            return;
        };
        let (Some(remaining), Some(reset)) = (obs.remaining, obs.reset_at_unix) else {
            return;
        };
        if remaining > RATE_LIMIT_FLOOR {
            return;
        }
        let now = now_unix();
        if now >= reset {
            return;
        }
        let wait = ((reset - now) as u64).min(MAX_BACKOFF_SECS);
        warn!(
            "GitHub API rate limit low ({} remaining, shared bucket), waiting {}s",
            remaining, wait
        );
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
    }

    /// Put one request on the wire and fold it into the budget meter.
    ///
    /// The meter is updated from the response headers BEFORE the body is read,
    /// because by then GitHub has already charged the request: a body-read
    /// failure after a complete header exchange is not a transport error in
    /// budget terms and must not be recorded as one. A failure to reach GitHub
    /// at all IS recorded — it cost a socket, and leaving it out would make a
    /// storm of DNS failures read as "nothing happened".
    async fn send_once(
        &self,
        url: &str,
        cache_mode: CacheMode,
        request: reqwest::RequestBuilder,
    ) -> Result<RawExchange, String> {
        let resp = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                github_budget::record_transport_error(self.consumer, url, cache_mode);
                return Err(format!("GitHub API request failed: {}", e));
            }
        };

        let status = resp.status();
        let headers = resp.headers().clone();
        github_budget::record_response(self.consumer, url, cache_mode, status.as_u16(), &headers);

        let rate = github_budget::parse_rate_limit(&headers);
        let etag = headers
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let link_next = headers
            .get(LINK)
            .and_then(|v| v.to_str().ok())
            .and_then(extract_next_url);
        let retry_after = headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok());

        let body = resp
            .bytes()
            .await
            .map_err(|e| format!("GitHub API body read failed: {}", e))?;

        Ok(RawExchange {
            status,
            etag,
            link_next,
            retry_after,
            remaining: rate.remaining,
            reset_at_unix: rate.reset_at_unix,
            body,
        })
    }

    /// Send a request with shared-bucket pre-check, budget metering, conditional
    /// -request handling and a single 403/429 retry.
    ///
    /// `url` is passed explicitly rather than recovered from the built request:
    /// it is the ETag cache key AND the meter's template source, and a
    /// `RequestBuilder` will not surrender it without being consumed.
    async fn send_metered(
        &self,
        url: &str,
        cache_mode: CacheMode,
        request: reqwest::RequestBuilder,
    ) -> Result<GhResponse, String> {
        self.check_rate_limit().await;

        // Kept aside BEFORE the validator goes on, for the (rare) case where the
        // validator survives in cache but the body it names does not.
        let unconditional = request.try_clone();

        // Echo the held validator. The row is recorded `cached` even on the
        // first, validator-less request: its 304 ratio measures the conditional
        // policy as a whole, and cold starts belong in that denominator.
        let request = match cache_mode {
            CacheMode::Cached => match github_budget::etag_for(url) {
                Some(tag) => match HeaderValue::from_str(&tag) {
                    Ok(v) => request.header(IF_NONE_MATCH, v),
                    Err(_) => request,
                },
                None => request,
            },
            _ => request,
        };
        let retry_clone = request.try_clone();
        let mut ex = self.send_once(url, cache_mode, request).await?;

        if let Some(wait) = retry_backoff_secs(
            ex.status.as_u16(),
            ex.retry_after,
            ex.remaining,
            ex.reset_at_unix,
            now_unix(),
        ) {
            warn!(
                "GitHub API rate limited ({}), retrying after {}s",
                ex.status, wait
            );
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            let retry = retry_clone.ok_or("Failed to clone request for potential retry")?;
            ex = self.send_once(url, cache_mode, retry).await?;
        } else if is_terminal_403(ex.status.as_u16(), ex.remaining) {
            // A permission 403, not an empty bucket. Terminal, as before.
            return Err(format!(
                "GitHub API returned {}: {}",
                ex.status,
                String::from_utf8_lossy(&ex.body)
            ));
        }

        // A 304 is FREE against the bucket — the entire point of the conditional
        // request. Its body has to come from the cache.
        let mut effective_mode = cache_mode;
        if ex.status == reqwest::StatusCode::NOT_MODIFIED {
            if let Some(replayed) = replay_not_modified(url, ex.link_next.clone()) {
                return Ok(replayed);
            }
            // Re-ask ONCE, unconditionally, rather than handing back an empty
            // 304 no caller can parse. The row is `fresh` because that is what
            // actually went on the wire.
            debug!("GitHub ETag cache: validator held but body evicted for {url}; re-reading");
            let again = unconditional.ok_or("Failed to clone request for cache-miss re-read")?;
            ex = self.send_once(url, CacheMode::Fresh, again).await?;
            // Still cache the re-read: the caller asked for conditional
            // behaviour and the entry it lost is exactly what we want back.
            effective_mode = CacheMode::Cached;
        }

        let link_key = link_cache_key(url);
        match cache_action(effective_mode, ex.status.as_u16(), ex.etag.is_some()) {
            CacheAction::Store => {
                let tag = ex.etag.as_deref().unwrap_or_default();
                github_budget::store(url, tag, ex.body.clone());
                match &ex.link_next {
                    Some(next) => github_budget::store(&link_key, tag, Bytes::from(next.clone())),
                    // No next page NOW — a cached one from an earlier, longer
                    // result would resurrect a page that no longer exists.
                    None => github_budget::invalidate(&link_key),
                }
            }
            CacheAction::DropEntry => {
                github_budget::invalidate(url);
                github_budget::invalidate(&link_key);
            }
            CacheAction::LeaveAlone => {}
        }

        Ok(GhResponse {
            status: ex.status,
            body: ex.body,
            link_next: ex.link_next,
            from_cache: false,
        })
    }

    /// Issue a write and drop every cached representation the write changes.
    ///
    /// `invalidates` carries FULL urls built by the same builders the reads use
    /// — the cache is keyed on the exact URL including its query string, so a
    /// hand-typed near-miss would silently invalidate nothing.
    async fn send_write(
        &self,
        url: &str,
        invalidates: &[String],
        request: reqwest::RequestBuilder,
    ) -> Result<GhResponse, String> {
        let resp = self
            .send_metered(url, CacheMode::Uncacheable, request)
            .await?;
        if resp.is_success() {
            for stale in invalidates {
                github_budget::invalidate(stale);
                github_budget::invalidate(&link_cache_key(stale));
            }
        }
        Ok(resp)
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.token)).unwrap_or_else(|e| {
                tracing::warn!("Invalid GitHub token header value: {e}");
                HeaderValue::from_static("Bearer invalid-token")
            }),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("qontinui-runner"));
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        headers
    }

    /// Fetch PR status.
    ///
    /// Conditional. Do not expect much from it on a busy repo: GitHub embeds the
    /// mutable `head.repo`/`base.repo` object in this representation, so a push
    /// to ANY branch invalidates every open PR's validator at once (coord
    /// measured ~1,446 charged against 12 not-modified). It is wired anyway
    /// because the alternative is guessing, and the `cached` row's own ratio
    /// will say what it actually buys here.
    pub async fn get_pr(&self, owner: &str, repo: &str, number: u64) -> Result<PrStatus, String> {
        let url = pr_url(owner, repo, number);
        let resp = self
            .send_metered(
                &url,
                CacheMode::Cached,
                self.client.get(&url).headers(self.headers()),
            )
            .await?;

        if !resp.is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status(),
                resp.text()
            ));
        }

        let body: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Failed to parse PR response: {}", e))?;

        Ok(PrStatus {
            number,
            state: body["state"].as_str().unwrap_or("unknown").to_string(),
            merged: body["merged"].as_bool().unwrap_or(false),
            mergeable: body["mergeable"].as_bool(),
            head_sha: body["head"]["sha"].as_str().unwrap_or("").to_string(),
            base_ref: body["base"]["ref"].as_str().unwrap_or("").to_string(),
            closed_at: body["closed_at"].as_str().map(|s| s.to_string()),
            title: body["title"].as_str().unwrap_or("").to_string(),
            html_url: body["html_url"].as_str().unwrap_or("").to_string(),
            labels: parse_labels(&body),
        })
    }

    /// List ALL PRs (open, closed, or merged) whose head branch is
    /// `<owner>:<branch>`, for the runner-local session-PR attribution
    /// reconciler. Uses `state=all` so a session's already-merged/closed PR
    /// still resolves — the dropdown's green ("all merged") state depends on
    /// seeing the merged rows. One page (20) suffices; a single head branch
    /// realistically backs at most one or two PRs.
    pub async fn list_prs_for_head(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Vec<HeadPr>, String> {
        let url = prs_by_head_url(owner, repo, branch);
        let resp = self
            .send_metered(
                &url,
                CacheMode::Cached,
                self.client.get(&url).headers(self.headers()),
            )
            .await?;

        if !resp.is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status(),
                resp.text()
            ));
        }

        let body: Vec<serde_json::Value> = resp
            .json()
            .map_err(|e| format!("Failed to parse PR list response: {}", e))?;

        Ok(body
            .iter()
            .filter_map(|pr| {
                let number = pr["number"].as_u64()?;
                Some(HeadPr {
                    number,
                    state: pr["state"].as_str().unwrap_or("unknown").to_string(),
                    merged: pr["merged_at"].is_string(),
                    merged_at: pr["merged_at"].as_str().map(|s| s.to_string()),
                    closed_at: pr["closed_at"].as_str().map(|s| s.to_string()),
                    head_ref: pr["head"]["ref"].as_str().unwrap_or("").to_string(),
                    head_sha: pr["head"]["sha"].as_str().unwrap_or("").to_string(),
                    base_ref: pr["base"]["ref"].as_str().unwrap_or("").to_string(),
                    labels: parse_labels(pr),
                })
            })
            .collect())
    }

    /// Fetch check runs for a commit SHA (paginated).
    pub async fn get_check_runs(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> Result<Vec<CheckRun>, String> {
        let mut all_runs = Vec::new();
        let mut next_url: Option<String> = Some(check_runs_url(owner, repo, sha));

        while let Some(url) = next_url.take() {
            let resp = self
                .send_metered(
                    &url,
                    CacheMode::Cached,
                    self.client.get(&url).headers(self.headers()),
                )
                .await?;

            if !resp.is_success() {
                return Err(format!(
                    "GitHub API returned {}: {}",
                    resp.status(),
                    resp.text()
                ));
            }

            let link_header = resp.next_page_url();

            let body: CheckRunsResponse = resp
                .json()
                .map_err(|e| format!("Failed to parse check runs: {}", e))?;

            all_runs.extend(body.check_runs);

            if all_runs.len() >= PAGINATION_CAP {
                warn!(
                    "get_check_runs: pagination cap ({}) reached for {}/{} sha {}",
                    PAGINATION_CAP, owner, repo, sha
                );
                all_runs.truncate(PAGINATION_CAP);
                break;
            }

            next_url = link_header;
        }

        Ok(all_runs)
    }

    /// Fetch reviews for a PR (paginated).
    pub async fn get_pr_reviews(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Vec<PrReview>, String> {
        let mut all_reviews = Vec::new();
        let mut next_url: Option<String> = Some(pr_reviews_url(owner, repo, number));

        while let Some(url) = next_url.take() {
            let resp = self
                .send_metered(
                    &url,
                    CacheMode::Cached,
                    self.client.get(&url).headers(self.headers()),
                )
                .await?;

            if !resp.is_success() {
                return Err(format!(
                    "GitHub API returned {}: {}",
                    resp.status(),
                    resp.text()
                ));
            }

            let link_header = resp.next_page_url();

            let reviews: Vec<PrReview> = resp
                .json()
                .map_err(|e| format!("Failed to parse reviews: {}", e))?;

            all_reviews.extend(reviews);

            if all_reviews.len() >= PAGINATION_CAP {
                warn!(
                    "get_pr_reviews: pagination cap ({}) reached for {}/{} PR #{}",
                    PAGINATION_CAP, owner, repo, number
                );
                all_reviews.truncate(PAGINATION_CAP);
                break;
            }

            next_url = link_header;
        }

        Ok(all_reviews)
    }

    /// Fetch truncated log for a check run (last 4KB).
    ///
    /// Declared [`CacheMode::Fresh`], deliberately. This endpoint answers `302`
    /// to a signed, short-lived blob URL that reqwest then follows, so the body
    /// that arrives carries no API validator to condition on; and it is read
    /// once, at failure-diagnosis time, never polled — so a cache entry would
    /// spend the ETag store's byte budget on a possibly MB-scale body nothing
    /// will ask for twice. `fresh` is the honest label for that, and Phase A
    /// deliberately declines to judge a `fresh` row's hit rate.
    pub async fn get_check_run_log(
        &self,
        owner: &str,
        repo: &str,
        check_run_id: u64,
    ) -> Result<String, String> {
        let url = job_logs_url(owner, repo, check_run_id);
        let resp = self
            .send_metered(
                &url,
                CacheMode::Fresh,
                self.client.get(&url).headers(self.headers()),
            )
            .await?;

        if !resp.is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status(),
                resp.text()
            ));
        }

        let full_log = resp.text();

        // Truncate to last 4KB for prompt injection (UTF-8 safe)
        let truncated = if full_log.len() > 4096 {
            let mut start = full_log.len().saturating_sub(4096);
            while !full_log.is_char_boundary(start) && start < full_log.len() {
                start += 1;
            }
            format!("...(truncated)\n{}", &full_log[start..])
        } else {
            full_log
        };

        Ok(truncated)
    }

    /// Post a comment on a PR. Invalidates the issue's comment list, whose
    /// representation this write changes.
    pub async fn create_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<(), String> {
        let url = issue_comments_url(owner, repo, number);
        let resp = self
            .send_write(
                &url,
                std::slice::from_ref(&url),
                self.client
                    .post(&url)
                    .headers(self.headers())
                    .json(&serde_json::json!({ "body": body })),
            )
            .await?;

        if !resp.is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status(),
                resp.text()
            ));
        }

        Ok(())
    }

    /// Submit a PR review (APPROVE, REQUEST_CHANGES, or COMMENT).
    ///
    /// Invalidates both the review list AND the PR detail: an approval changes
    /// the PR's own `mergeable`/review-decision fields, so replaying a
    /// pre-review body would report the state this call just superseded.
    pub async fn submit_pr_review(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        event: &str,
        body: &str,
    ) -> Result<(), String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/reviews",
            owner, repo, number
        );
        let resp = self
            .send_write(
                &url,
                &[
                    pr_reviews_url(owner, repo, number),
                    pr_url(owner, repo, number),
                ],
                self.client
                    .post(&url)
                    .headers(self.headers())
                    .json(&serde_json::json!({ "event": event, "body": body })),
            )
            .await?;

        if !resp.is_success() {
            return Err(format!(
                "GitHub submit_pr_review returned {}: {}",
                resp.status(),
                resp.text()
            ));
        }

        Ok(())
    }

    /// Derive aggregate CI status from check runs.
    pub fn derive_ci_status(check_runs: &[CheckRun]) -> CiStatus {
        if check_runs.is_empty() {
            return CiStatus::Pending;
        }

        let mut failed = Vec::new();
        let mut all_complete = true;

        for cr in check_runs {
            if cr.status != "completed" {
                all_complete = false;
                continue;
            }
            match cr.conclusion.as_deref() {
                Some("failure") | Some("timed_out") | Some("cancelled") => {
                    failed.push(cr.name.clone());
                }
                Some("action_required") => {
                    failed.push(cr.name.clone());
                }
                _ => {} // success, neutral, skipped
            }
        }

        if !failed.is_empty() {
            CiStatus::Failure {
                failed_checks: failed,
            }
        } else if all_complete {
            CiStatus::Success
        } else {
            CiStatus::Pending
        }
    }

    /// Derive aggregate review status from reviews.
    /// Uses the last review per reviewer (GitHub semantics).
    pub fn derive_review_status(reviews: &[PrReview]) -> ReviewStatus {
        if reviews.is_empty() {
            return ReviewStatus::Pending;
        }

        // Take last review per user
        let mut latest: HashMap<String, &str> = HashMap::new();
        for review in reviews {
            latest.insert(review.user.login.clone(), &review.state);
        }

        let has_changes_requested = latest.values().any(|s| *s == "CHANGES_REQUESTED");
        let has_approval = latest.values().any(|s| *s == "APPROVED");

        if has_changes_requested {
            ReviewStatus::ChangesRequested
        } else if has_approval {
            ReviewStatus::Approved
        } else {
            ReviewStatus::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Back-off decision --------------------------------------------------

    /// A `429` sleeps for `Retry-After`, capped. A `403` sleeps only when the
    /// bucket is actually empty — a permission `403` retried is budget spent to
    /// be told the same thing twice.
    #[test]
    fn retry_backoff_distinguishes_an_empty_bucket_from_a_permission_refusal() {
        assert_eq!(
            retry_backoff_secs(429, Some(30), None, None, 1_000),
            Some(30)
        );
        assert_eq!(
            retry_backoff_secs(429, None, None, None, 1_000),
            Some(60),
            "no Retry-After falls back to a minute"
        );
        assert_eq!(
            retry_backoff_secs(429, Some(99_999), None, None, 1_000),
            Some(MAX_BACKOFF_SECS),
            "an hour-long sleep is indistinguishable from a dead poller"
        );

        // 403 + remaining:0 → sleep until reset, capped.
        assert_eq!(
            retry_backoff_secs(403, None, Some(0), Some(1_120), 1_000),
            Some(120)
        );
        assert_eq!(
            retry_backoff_secs(403, None, Some(0), Some(900), 1_000),
            Some(60),
            "a reset already in the past is not a negative sleep"
        );
        assert_eq!(
            retry_backoff_secs(403, None, Some(0), None, 1_000),
            Some(60),
            "an unknown reset is not an infinite sleep"
        );

        // Everything else is not retried at all.
        for status in [200, 304, 401, 403, 404, 422, 500] {
            if status == 403 {
                assert_eq!(retry_backoff_secs(403, None, Some(4_999), None, 1), None);
                continue;
            }
            assert_eq!(retry_backoff_secs(status, Some(5), None, None, 1), None);
        }
    }

    #[test]
    fn only_a_403_with_budget_left_is_terminal() {
        assert!(is_terminal_403(403, Some(4_999)));
        assert!(
            is_terminal_403(403, None),
            "unknown headroom is not an empty bucket"
        );
        assert!(
            !is_terminal_403(403, Some(0)),
            "an empty bucket is retryable"
        );
        assert!(!is_terminal_403(429, Some(4_999)));
        assert!(!is_terminal_403(200, None));
    }

    // -- Cache decision -----------------------------------------------------

    #[test]
    fn cache_action_stores_only_a_validated_success() {
        assert_eq!(
            cache_action(CacheMode::Cached, 200, true),
            CacheAction::Store
        );
        assert_eq!(
            cache_action(CacheMode::Cached, 201, true),
            CacheAction::Store
        );
    }

    /// A 200 with no validator, and every non-success, must DROP what we held —
    /// otherwise a deleted PR's body is replayed forever off an ETag GitHub will
    /// happily keep matching against nothing.
    #[test]
    fn cache_action_drops_the_entry_on_a_validatorless_success_or_any_failure() {
        assert_eq!(
            cache_action(CacheMode::Cached, 200, false),
            CacheAction::DropEntry
        );
        for status in [301, 401, 403, 404, 422, 500, 502] {
            assert_eq!(
                cache_action(CacheMode::Cached, status, true),
                CacheAction::DropEntry,
                "status {status}"
            );
        }
    }

    /// `fresh` and `uncacheable` rows have no cache decision to make, and a
    /// write must never be allowed to LEAVE a body behind under its own URL.
    #[test]
    fn cache_action_abstains_for_fresh_and_uncacheable() {
        for mode in [CacheMode::Fresh, CacheMode::Uncacheable] {
            for status in [200, 204, 404, 500] {
                assert_eq!(
                    cache_action(mode, status, true),
                    CacheAction::LeaveAlone,
                    "{mode:?} {status}"
                );
            }
        }
    }

    // -- ETag round trip: store, condition, replay --------------------------

    /// The full conditional cycle without a wire: a stored body arms the next
    /// request's validator, and a `304` is answered out of the cache.
    #[test]
    fn a_stored_body_arms_the_validator_and_a_304_replays_it() {
        let url = "https://api.github.com/repos/o/r/pulls/90210-store-then-condition";
        github_budget::invalidate(url);
        github_budget::invalidate(&link_cache_key(url));

        // Nothing held: no validator to send, and a 304 could not be answered.
        assert_eq!(github_budget::etag_for(url), None);
        assert!(
            replay_not_modified(url, None).is_none(),
            "a 304 with no cached body is UNKNOWN, never an empty body"
        );

        github_budget::store(url, "W/\"v1\"", Bytes::from_static(b"{\"number\":1}"));
        assert_eq!(github_budget::etag_for(url).as_deref(), Some("W/\"v1\""));

        let replayed = replay_not_modified(url, None).expect("cached body replays");
        assert!(
            replayed.is_success(),
            "a 304 is served to the caller as a 200"
        );
        assert!(replayed.served_from_cache());
        assert_eq!(replayed.text(), "{\"number\":1}");
        assert_eq!(replayed.next_page_url(), None);

        github_budget::invalidate(url);
        assert!(replay_not_modified(url, None).is_none());
    }

    /// A `304` is not obliged to repeat `Link`, and a paginated loop that reads
    /// an absent `Link` as "last page" silently truncates its result. The header
    /// is therefore cached beside the body — and a header actually present on
    /// the 304 still wins.
    #[test]
    fn the_next_page_link_survives_a_304_that_omits_it() {
        let url = "https://api.github.com/repos/o/r/pulls/1/reviews?per_page=100&probe=link";
        let key = link_cache_key(url);
        github_budget::invalidate(url);
        github_budget::invalidate(&key);

        assert_eq!(cached_link_next(url), None);
        github_budget::store(url, "e1", Bytes::from_static(b"[]"));
        github_budget::store(&key, "e1", Bytes::from(format!("{url}&page=2")));

        let replayed = replay_not_modified(url, None).expect("cached body replays");
        assert_eq!(
            replayed.next_page_url().as_deref(),
            Some(format!("{url}&page=2").as_str()),
            "pagination must not end just because the 304 dropped Link"
        );

        // A Link the 304 DID carry wins over the cached one.
        let replayed = replay_not_modified(url, Some("https://fresh/next".to_string()))
            .expect("cached body replays");
        assert_eq!(
            replayed.next_page_url().as_deref(),
            Some("https://fresh/next")
        );

        github_budget::invalidate(url);
        github_budget::invalidate(&key);
    }

    #[test]
    fn the_link_cache_key_cannot_collide_with_a_real_url() {
        let key = link_cache_key("https://api.github.com/repos/o/r/pulls/1");
        assert!(key.contains('\0'), "a NUL cannot occur in a URL: {key:?}");
        assert_ne!(key, "https://api.github.com/repos/o/r/pulls/1");
    }

    // -- Write invalidation -------------------------------------------------

    /// The read and the write that invalidates it must produce the SAME string,
    /// query included — the cache keys on the full URL, so a near-miss
    /// invalidates nothing and leaves a stale body behind.
    #[test]
    fn write_invalidation_targets_the_exact_urls_the_reads_cached() {
        assert_eq!(
            pr_reviews_url("o", "r", 7),
            "https://api.github.com/repos/o/r/pulls/7/reviews?per_page=100"
        );
        assert_eq!(
            pr_url("o", "r", 7),
            "https://api.github.com/repos/o/r/pulls/7"
        );
        assert_eq!(
            issue_comments_url("o", "r", 7),
            "https://api.github.com/repos/o/r/issues/7/comments"
        );
        assert_eq!(
            check_runs_url("o", "r", "deadbeef"),
            "https://api.github.com/repos/o/r/commits/deadbeef/check-runs?per_page=100"
        );
        assert_eq!(
            job_logs_url("o", "r", 42),
            "https://api.github.com/repos/o/r/actions/jobs/42/logs"
        );
    }

    /// Dropping a cached representation drops its cached `Link` with it: a body
    /// gone and a `rel="next"` left behind would resume a pagination whose first
    /// page no longer exists.
    #[test]
    fn invalidating_a_read_drops_its_body_and_its_cached_link() {
        let url = pr_reviews_url("o", "r", 424242);
        let key = link_cache_key(&url);
        github_budget::store(&url, "e", Bytes::from_static(b"[]"));
        github_budget::store(&key, "e", Bytes::from_static(b"https://next"));
        assert!(github_budget::replay(&url).is_some());

        // Exactly what `send_write` does on a successful write.
        github_budget::invalidate(&url);
        github_budget::invalidate(&key);

        assert_eq!(github_budget::replay(&url), None);
        assert_eq!(cached_link_next(&url), None);
    }

    // -- Transport errors ---------------------------------------------------

    /// A request that never reached GitHub still cost a socket. It must land in
    /// `transportError`, NOT in `charged` — a charged count inflated by DNS
    /// failures reads as budget spent that never was, and the reverse (dropping
    /// it) makes a failure storm read as "nothing happened".
    #[tokio::test]
    async fn an_unreachable_host_records_a_transport_error_and_no_charge() {
        // Port 1 on loopback: refused immediately, no network, no wait.
        let url = "http://127.0.0.1:1/probe/transport-error";
        let client = GitHubClient::new("t", "budget_test_transport").expect("client builds");

        let err = client
            .send_metered(url, CacheMode::Fresh, client.client.get(url))
            .await
            .expect_err("connection to port 1 must fail");
        assert!(err.contains("GitHub API request failed"), "{err}");

        let snap = github_budget::snapshot_top(usize::MAX);
        let row = snap
            .consumers
            .iter()
            .find(|r| r.consumer == "budget_test_transport")
            .expect("the failed call was recorded");
        assert_eq!(row.transport_error, 1);
        assert_eq!(row.charged, 0, "a call that never landed charged nothing");
        assert_eq!(row.not_modified, 0);
        assert_eq!(row.rate_limited, 0);
        assert_eq!(row.template, "/probe/transport-error");
    }

    #[test]
    fn session_pr_head_lookup_url_is_state_all_and_percent_encodes_the_branch() {
        // state=all so a merged/closed PR still resolves (the dropdown's green
        // "all merged" state depends on seeing the merged rows).
        let url = prs_by_head_url("qontinui", "qontinui-runner", "feat/x");
        assert!(url.contains("state=all"), "must query state=all: {url}");
        assert!(url.contains("head=qontinui:feat%2Fx"), "{url}");

        // Same '#'-fragment-truncation guard as the open-only builder.
        let url = prs_by_head_url("qontinui", "qontinui-runner", "fix/issue#42");
        assert!(!url.contains('#'), "raw '#' must never survive: {url}");
        assert!(url.contains("head=qontinui:fix%2Fissue%2342"), "{url}");
    }

    /// The shape GitHub actually sends: `labels` is an array of objects, and
    /// only `name` is load-bearing here.
    #[test]
    fn parse_labels_reads_the_name_of_every_label() {
        let pr = serde_json::json!({
            "labels": [
                {"id": 1, "name": "coord:landed", "color": "0e8a16"},
                {"id": 2, "name": "coord:tier1", "color": "ededed"}
            ]
        });
        assert_eq!(parse_labels(&pr), vec!["coord:landed", "coord:tier1"]);
        assert!(has_coord_landed_label(&parse_labels(&pr)));
    }

    /// ABSENCE IS NOT A NEGATIVE. Three different payloads mean "no label
    /// observed" and every one must yield an empty set rather than a panic or
    /// a partial parse - the land cascade reads an empty set as "coord's chip
    /// was not seen", never as "coord did not land this".
    #[test]
    fn absent_or_malformed_labels_yield_an_empty_set() {
        for pr in [
            // No `labels` key at all (an older cached payload).
            serde_json::json!({"number": 1}),
            // Present but empty.
            serde_json::json!({"labels": []}),
            // Present but not an array.
            serde_json::json!({"labels": "coord:landed"}),
            // An array whose entries carry no usable `name`.
            serde_json::json!({"labels": [{"id": 7}, {"name": 12}]}),
        ] {
            let labels = parse_labels(&pr);
            assert!(labels.is_empty(), "{pr}");
            assert!(
                !has_coord_landed_label(&labels),
                "an unobserved chip is not a present chip: {pr}"
            );
        }
    }

    /// The chip is matched EXACTLY. A near-miss must not read as coord's
    /// verdict - the label namespace is shared with human- and agent-set
    /// `coord:*` labels (`/coord-pr-label`).
    #[test]
    fn only_the_exact_coord_landed_chip_counts() {
        assert!(has_coord_landed_label(&["coord:landed".to_string()]));
        for near in [
            "coord:landed-2",
            "coord:land",
            "landed",
            "Coord:Landed",
            "coord:landing",
        ] {
            assert!(
                !has_coord_landed_label(&[near.to_string()]),
                "near-miss must not count: {near}"
            );
        }
    }
}
