//! Lightweight read-only GitHub REST API client for the session PR reconciler.
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
//!    client instance, and four clients were constructed across this process —
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
use tracing::{debug, warn};

use crate::github_budget::{self, BudgetDeferral, CacheMode, CredentialKey, SpendClass};

/// Requests left in the credential's bucket at or below which an INTERACTIVE
/// client sleeps one chunk before sending. A background client never reaches
/// it: the headroom gate ([`github_budget::background_gate`]) defers it first.
const RATE_LIMIT_FLOOR: i64 = 10;

/// Why a [`GitHubClient`] read produced no answer.
///
/// `Deferred` is NOT a failure: the request was never sent because the
/// background headroom gate judged the credential's window short. A caller
/// must tell the two apart — a deferral says "stop spending this cycle", a
/// failure says "this one read went wrong".
#[derive(Debug)]
pub enum GhError {
    /// Decided against before sending (background spend class only).
    Deferred(BudgetDeferral),
    /// Sent, and GitHub refused it for RATE (a `429`, a primary-limit `403`, or
    /// a secondary-limit `403` with `Retry-After`) — background spend class
    /// only, which never sleeps on it. Like a deferral it says "stop spending
    /// this cycle", not "this one read went wrong".
    RateLimited { status: u16 },
    /// Sent, and GitHub answered a non-success status that is NOT a rate
    /// refusal (a permission `401`/`403`, a `404`, a `5xx`). Carries the status
    /// so a caller can tell a terminal refusal from a transient one.
    HttpStatus { status: u16, message: String },
    /// Sent (or attempted) and failed without a usable status (transport,
    /// body read, parse).
    Failed(String),
}

impl GhError {
    /// True when the answer means "the budget said no" — a deferral or a
    /// rate-limited refusal — rather than a failure of this one read.
    pub fn is_budget_stop(&self) -> bool {
        matches!(self, GhError::Deferred(_) | GhError::RateLimited { .. })
    }
}

impl std::fmt::Display for GhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GhError::Deferred(d) => write!(f, "{d}"),
            GhError::RateLimited { status } => write!(
                f,
                "GitHub API rate limited ({status}); background request not slept on or retried"
            ),
            GhError::HttpStatus { message, .. } => f.write_str(message),
            GhError::Failed(e) => f.write_str(e),
        }
    }
}

impl From<String> for GhError {
    fn from(e: String) -> Self {
        GhError::Failed(e)
    }
}

impl From<&str> for GhError {
    fn from(e: &str) -> Self {
        GhError::Failed(e.to_string())
    }
}

impl From<GhError> for String {
    fn from(e: GhError) -> Self {
        e.to_string()
    }
}

/// Longest single back-off GitHub's own wait is capped at, in seconds, BEFORE
/// the resume jitter ([`github_budget::resume_after`]) adds up to
/// [`github_budget::RESUME_JITTER_SECS`]. Preserved verbatim from the
/// per-instance implementation: a GitHub reset can be an hour out, and blocking
/// a poller for an hour is indistinguishable from the poller being dead.
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
/// `Some` exactly when [`github_budget::is_rate_limited_refusal`] — the one
/// classifier the meter, the ticket provider and `ci_node::sibling` also ask —
/// says GitHub refused this for rate, not for permission.
///
/// The wait: a `429` or a secondary-limit `403` waits its `Retry-After` (a
/// `429` without one, a minute); a primary-limit `403`
/// (`x-ratelimit-remaining: 0`) waits for the reset. A permission `403` carries
/// neither signal and must not be slept on.
pub(crate) fn retry_backoff_secs(
    status: u16,
    retry_after: Option<u64>,
    remaining: Option<i64>,
    reset_at_unix: Option<i64>,
    now: i64,
) -> Option<u64> {
    if !github_budget::is_rate_limited_refusal(status, retry_after, remaining) {
        return None;
    }
    let wait = match (status, remaining, retry_after) {
        (403, Some(0), _) => match reset_at_unix {
            Some(reset) if reset > now => (reset - now) as u64,
            _ => 60,
        },
        (_, _, Some(secs)) => secs,
        _ => 60,
    };
    Some(wait.min(MAX_BACKOFF_SECS))
}

/// How long [`interactive_floor`] sleeps BEFORE jitter, or `None` when it does
/// not sleep: the bucket is above [`RATE_LIMIT_FLOOR`], the reset is unknown,
/// or the window has already reset. Capped at [`MAX_BACKOFF_SECS`]. Pure,
/// unit-tested.
pub(crate) fn interactive_floor_wait_secs(
    remaining: i64,
    reset_at_unix: Option<i64>,
    now: i64,
) -> Option<u64> {
    let reset = reset_at_unix?;
    if remaining > RATE_LIMIT_FLOOR || now >= reset {
        return None;
    }
    Some(((reset - now) as u64).min(MAX_BACKOFF_SECS))
}

/// INTERACTIVE only: sleep one chunk when `credential`'s own bucket is at or
/// below [`RATE_LIMIT_FLOOR`] and its window has not reset — then let the caller
/// send. Shared by [`GitHubClient`] and the ticket provider's writes so the two
/// cannot drift.
///
/// The reading is the credential's own, never another token's. No reading at
/// all is UNKNOWN, and UNKNOWN does not sleep: a cold process must be allowed
/// to make its first request, which is what produces the first reading.
pub(crate) async fn interactive_floor(credential: &CredentialKey) {
    let Some(obs) = github_budget::rate_limit_for(credential) else {
        return;
    };
    let Some(remaining) = obs.remaining else {
        return;
    };
    let Some(wait) = interactive_floor_wait_secs(remaining, obs.reset_at_unix, now_unix()) else {
        return;
    };
    // Jittered past the reset: every runner sharing the bucket computed the
    // same reset, and resuming exactly there is the synchronized burst.
    let wait = github_budget::resume_after(wait);
    warn!(
        "GitHub API rate limit low ({} remaining, credential {}), waiting {}s",
        remaining, credential, wait
    );
    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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

/// The URL of one PR, for [`GitHubClient::get_pr`].
fn pr_url(owner: &str, repo: &str, number: u64) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/pulls/{number}")
}

/// Maximum total results a paginated read accumulates before it stops. Shared by
/// every `Link`-paged reader in the crate (the ticket provider's issue and
/// comment lists, and [`GitHubClient::list_repo_prs`]) rather than copied.
pub(crate) const PAGINATION_CAP: usize = 500;

/// Which repo-wide PR listing [`GitHubClient::list_repo_prs`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoPrSlice<'a> {
    /// Every open PR (`state=open&per_page=100`), paged to the end.
    Open,
    /// Closed PRs, most recently updated first
    /// (`state=closed&sort=updated&direction=desc&per_page=30`), paged back
    /// until a page reaches `since` — the caller's high-water `updated_at`.
    /// `None` (no mark yet) reads the first page only: there is nothing to
    /// page back to, and rows older than it are not this listing's job.
    RecentlyClosed { since: Option<&'a str> },
}

/// The first page of a repo-wide PR listing, for
/// [`GitHubClient::list_repo_prs`]. Later pages come from `Link`. Pure,
/// unit-tested.
fn repo_prs_url(owner: &str, repo: &str, slice: RepoPrSlice<'_>) -> String {
    let query = match slice {
        RepoPrSlice::Open => "state=open&per_page=100",
        RepoPrSlice::RecentlyClosed { .. } => {
            "state=closed&sort=updated&direction=desc&per_page=30"
        }
    };
    format!(
        "https://api.github.com/repos/{}/{}/pulls?{query}",
        urlencoding::encode(owner),
        urlencoding::encode(repo),
    )
}

/// Does this page of an `updated`-descending closed listing reach the
/// high-water `mark` — i.e. carry an item updated no later than it? Paging
/// stops there: everything older was seen by an earlier read. An item whose
/// `updated_at` does not parse proves nothing and does not stop the paging.
/// Pure, unit-tested.
pub(crate) fn closed_page_reaches_mark(page: &[HeadPr], mark: &str) -> bool {
    let Ok(mark) = chrono::DateTime::parse_from_rfc3339(mark) else {
        // An unreadable mark cannot bound anything: read one page, as for none.
        return true;
    };
    page.iter().any(|pr| {
        pr.updated_at
            .as_deref()
            .and_then(|u| chrono::DateTime::parse_from_rfc3339(u).ok())
            .is_some_and(|u| u <= mark)
    })
}

/// One repo-wide PR listing from [`GitHubClient::list_repo_prs`].
#[derive(Debug, Default)]
pub struct RepoPrListing {
    pub prs: Vec<HeadPr>,
    /// The read stopped at [`PAGINATION_CAP`] with more pages left: the listing
    /// is INCOMPLETE, and a PR's absence from it proves nothing.
    pub overflow: bool,
    /// Requests answered (charged or `304`), for the reconciler's
    /// `discovery_calls` counter.
    pub requests: usize,
}

/// Parse one PR object of a `pulls` list into a [`HeadPr`]. `None` when it
/// carries no number.
fn parse_head_pr(pr: &serde_json::Value) -> Option<HeadPr> {
    let number = pr["number"].as_u64()?;
    Some(HeadPr {
        number,
        state: pr["state"].as_str().unwrap_or("unknown").to_string(),
        merged: pr["merged_at"].is_string(),
        merged_at: pr["merged_at"].as_str().map(|s| s.to_string()),
        closed_at: pr["closed_at"].as_str().map(|s| s.to_string()),
        head_ref: pr["head"]["ref"].as_str().unwrap_or("").to_string(),
        head_label: pr["head"]["label"].as_str().unwrap_or("").to_string(),
        head_sha: pr["head"]["sha"].as_str().unwrap_or("").to_string(),
        base_ref: pr["base"]["ref"].as_str().unwrap_or("").to_string(),
        updated_at: pr["updated_at"].as_str().map(|s| s.to_string()),
        labels: parse_labels(pr),
    })
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// `head.label` — `<owner>:<ref>` of the repo the head branch lives in. A
    /// repo-wide listing also carries FORK PRs, whose `head_ref` can equal one
    /// of ours, so attribution keys on this, never on `head_ref` alone. Empty
    /// when the payload carried none (and for a [`GitHubClient::get_pr`]-built
    /// value, which does not read it).
    #[serde(default)]
    pub head_label: String,
    pub head_sha: String,
    /// The PR's BASE branch name (e.g. `"main"`), unprefixed. The session-PR
    /// reconciler's content-proof land signal tests the head commit against
    /// `origin/<base_ref>`.
    pub base_ref: String,
    /// RFC3339 `updated_at` — the closed listing's paging key.
    #[serde(default)]
    pub updated_at: Option<String>,
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
    /// The bucket this client spends, by token digest — never the token.
    credential: CredentialKey,
    /// Which subsystem's spend this client's calls are attributed to in the
    /// budget histogram. Other subsystems spend the same credential through their
    /// own clients, and telling them apart is the whole point of the row key.
    consumer: &'static str,
    /// Background requests are gated on the credential's headroom and
    /// deferred, never slept on; interactive ones keep the floor and the
    /// rate-limit retry. Interactive unless [`Self::with_spend_class`] says so.
    spend_class: SpendClass,
    /// Scheme + host every route is built against: [`GITHUB_API_BASE`] in
    /// production. A test points it at a refusing port so a request that
    /// escapes a gate shows up as a transport error instead of reaching GitHub.
    api_base: &'static str,
}

/// The GitHub REST API origin every URL builder in this module spells.
pub(crate) const GITHUB_API_BASE: &str = "https://api.github.com";

/// `url` (built against [`GITHUB_API_BASE`]) re-rooted at `base`. The identity
/// whenever `base` is [`GITHUB_API_BASE`], i.e. in production; a test points
/// `base` at a loopback port so a request that escapes a gate never reaches
/// GitHub. Shared by [`GitHubClient`] and the ticket provider.
pub(crate) fn at_api_base(base: &str, url: String) -> String {
    match url.strip_prefix(GITHUB_API_BASE) {
        Some(path) if base != GITHUB_API_BASE => format!("{base}{path}"),
        _ => url,
    }
}

/// One canned HTTP response on a loopback port, for the rate-limit arms a
/// refusing port cannot produce. Returns the base URL. Test-only.
#[cfg(test)]
pub(crate) async fn one_shot_server_for_test(response: &'static str) -> &'static str {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    Box::leak(format!("http://{addr}").into_boxed_str())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrStatus {
    pub state: String,
    pub merged: bool,
    /// RFC3339 merge time, present only for a GitHub merge (a fast-forward
    /// land sets none) — see [`HeadPr::merged_at`].
    pub merged_at: Option<String>,
    pub head_sha: String,
    /// The PR's BASE branch name (e.g. `"main"`), unprefixed — see
    /// [`HeadPr::base_ref`].
    pub base_ref: String,
    /// RFC3339 close time — see [`HeadPr::closed_at`].
    pub closed_at: Option<String>,
    /// Label names on the PR — see [`HeadPr::labels`], including the note on
    /// `#[serde(default)]`.
    #[serde(default)]
    pub labels: Vec<String>,
}

impl GitHubClient {
    /// `consumer` is the budget-histogram label for whichever subsystem owns
    /// this client — `session_pr_reconciler`.
    /// It never carries the token.
    pub fn new(token: &str, consumer: &'static str) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
        Ok(Self {
            client,
            token: token.to_string(),
            credential: CredentialKey::of_token(token),
            consumer,
            spend_class: SpendClass::Interactive,
            api_base: GITHUB_API_BASE,
        })
    }

    /// Point every route at `base` instead of [`GITHUB_API_BASE`].
    #[cfg(test)]
    pub(crate) fn with_api_base(mut self, base: &'static str) -> Self {
        self.api_base = base;
        self
    }

    /// `url` (built against [`GITHUB_API_BASE`]) re-rooted at this client's
    /// `api_base`. The identity in production.
    fn at_base(&self, url: String) -> String {
        at_api_base(self.api_base, url)
    }

    /// Set this client's spend class (default [`SpendClass::Interactive`]).
    pub fn with_spend_class(mut self, spend_class: SpendClass) -> Self {
        self.spend_class = spend_class;
        self
    }

    /// The bucket this client spends.
    pub fn credential(&self) -> &CredentialKey {
        &self.credential
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
        github_budget::record_response(
            &self.credential,
            self.consumer,
            url,
            cache_mode,
            status.as_u16(),
            &headers,
        );

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

    /// Send a request with a budget pre-check, budget metering, conditional
    /// -request handling and — interactive only — a single 403/429 retry.
    ///
    /// The pre-check depends on the spend class. A BACKGROUND request under the
    /// credential's headroom line ([`github_budget::background_gate`]) is never
    /// sent and never slept on: it returns [`GhError::Deferred`], already
    /// counted on the meter as `deferred`. An INTERACTIVE request keeps the
    /// sleep-then-send floor ([`interactive_floor`]).
    ///
    /// `url` is passed explicitly rather than recovered from the built request:
    /// it is the ETag cache key AND the meter's template source, and a
    /// `RequestBuilder` will not surrender it without being consumed.
    async fn send_metered(
        &self,
        url: &str,
        cache_mode: CacheMode,
        request: reqwest::RequestBuilder,
    ) -> Result<GhResponse, GhError> {
        match self.spend_class {
            SpendClass::Background => {
                github_budget::background_gate(&self.credential, self.consumer, url, cache_mode)
                    .map_err(GhError::Deferred)?;
            }
            SpendClass::Interactive => interactive_floor(&self.credential).await,
        }

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

        let backoff = retry_backoff_secs(
            ex.status.as_u16(),
            ex.retry_after,
            ex.remaining,
            ex.reset_at_unix,
            now_unix(),
        );
        // Every runner sharing this bucket computed the same wait; each adds its
        // own jitter so they do not all re-send in the second the window opens.
        let backoff = backoff.map(github_budget::resume_after);
        if let (Some(wait), SpendClass::Background) = (backoff, self.spend_class) {
            // A background poller is never slept on. The refusal is already on
            // the meter as `rate_limited`; the hold keeps the next tick or
            // nudge from re-sending inside GitHub's window; and the caller
            // reads `RateLimited` as "stop spending this cycle".
            github_budget::record_rate_limited_refusal(&self.credential, wait);
            return Err(GhError::RateLimited {
                status: ex.status.as_u16(),
            });
        }
        if let Some(wait) = backoff {
            // Hold this credential's BACKGROUND spend for the same window, so a
            // poller does not send inside the Retry-After GitHub just gave.
            github_budget::record_rate_limited_refusal(&self.credential, wait);
            warn!(
                "GitHub API rate limited ({}), retrying after {}s",
                ex.status, wait
            );
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            let retry = retry_clone.ok_or("Failed to clone request for potential retry")?;
            ex = self.send_once(url, cache_mode, retry).await?;
        } else if is_terminal_403(ex.status.as_u16(), ex.remaining) {
            // A permission 403, not a rate refusal. Terminal, as before.
            return Err(GhError::HttpStatus {
                status: ex.status.as_u16(),
                message: format!(
                    "GitHub API returned {}: {}",
                    ex.status,
                    String::from_utf8_lossy(&ex.body)
                ),
            });
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
    pub async fn get_pr(&self, owner: &str, repo: &str, number: u64) -> Result<PrStatus, GhError> {
        let url = self.at_base(pr_url(owner, repo, number));
        let resp = self
            .send_metered(
                &url,
                CacheMode::Cached,
                self.client.get(&url).headers(self.headers()),
            )
            .await?;

        if !resp.is_success() {
            return Err(GhError::HttpStatus {
                status: resp.status().as_u16(),
                message: format!("GitHub API returned {}: {}", resp.status(), resp.text()),
            });
        }

        let body: serde_json::Value = resp
            .json()
            .map_err(|e| GhError::Failed(format!("Failed to parse PR response: {}", e)))?;

        Ok(PrStatus {
            state: body["state"].as_str().unwrap_or("unknown").to_string(),
            merged: body["merged"].as_bool().unwrap_or(false),
            merged_at: body["merged_at"].as_str().map(|s| s.to_string()),
            head_sha: body["head"]["sha"].as_str().unwrap_or("").to_string(),
            base_ref: body["base"]["ref"].as_str().unwrap_or("").to_string(),
            closed_at: body["closed_at"].as_str().map(|s| s.to_string()),
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
    ) -> Result<Vec<HeadPr>, GhError> {
        let url = self.at_base(prs_by_head_url(owner, repo, branch));
        let resp = self
            .send_metered(
                &url,
                CacheMode::Cached,
                self.client.get(&url).headers(self.headers()),
            )
            .await?;

        if !resp.is_success() {
            return Err(GhError::HttpStatus {
                status: resp.status().as_u16(),
                message: format!("GitHub API returned {}: {}", resp.status(), resp.text()),
            });
        }

        let body: Vec<serde_json::Value> = resp
            .json()
            .map_err(|e| GhError::Failed(format!("Failed to parse PR list response: {}", e)))?;

        Ok(body.iter().filter_map(parse_head_pr).collect())
    }

    /// One repo-wide PR listing — the session-PR reconciler's per-repo
    /// discovery (plan 2026-10-30-…, D3), which replaces a per-branch
    /// [`Self::list_prs_for_head`] for every branch of every session.
    ///
    /// Conditional and `Link`-paged like `GitHubTicketProvider::fetch_actionable`
    /// (the `304` replay keeps `Link`, see [`link_cache_key`]). Bounded by
    /// [`PAGINATION_CAP`]; a read stopped there with pages left reports
    /// `overflow`, and the caller must then treat absence as proving nothing.
    /// A deferral on any page ends the read with [`GhError::Deferred`].
    pub async fn list_repo_prs(
        &self,
        owner: &str,
        repo: &str,
        slice: RepoPrSlice<'_>,
    ) -> Result<RepoPrListing, GhError> {
        let mut out = RepoPrListing::default();
        let mut next_url = Some(self.at_base(repo_prs_url(owner, repo, slice)));

        while let Some(url) = next_url.take() {
            let resp = self
                .send_metered(
                    &url,
                    CacheMode::Cached,
                    self.client.get(&url).headers(self.headers()),
                )
                .await?;
            out.requests += 1;

            if !resp.is_success() {
                return Err(GhError::HttpStatus {
                    status: resp.status().as_u16(),
                    message: format!("GitHub API returned {}: {}", resp.status(), resp.text()),
                });
            }
            let link_next = resp.next_page_url();
            let body: Vec<serde_json::Value> = resp
                .json()
                .map_err(|e| GhError::Failed(format!("Failed to parse PR list response: {}", e)))?;
            let page: Vec<HeadPr> = body.iter().filter_map(parse_head_pr).collect();

            let reached_mark = match slice {
                RepoPrSlice::Open => false,
                RepoPrSlice::RecentlyClosed { since: None } => true,
                RepoPrSlice::RecentlyClosed { since: Some(mark) } => {
                    closed_page_reaches_mark(&page, mark)
                }
            };
            out.prs.extend(page);

            if reached_mark || link_next.is_none() {
                break;
            }
            if out.prs.len() >= PAGINATION_CAP {
                debug!(
                    "list_repo_prs: pagination cap ({PAGINATION_CAP}) reached for {owner}/{repo} \
                     ({slice:?}) — listing incomplete"
                );
                out.overflow = true;
                break;
            }
            next_url = link_next;
        }

        Ok(out)
    }

    /// The login this client's token authenticates as — a conditional
    /// `GET /user`. The caller asks once per credential per process and, on a
    /// terminal `401`/`403` (a credential that cannot answer `/user`), backs off
    /// ([`crate::github_budget::login_lookup_due`]) rather than re-asking every
    /// tick. Spent under the client's own spend class, so a background client
    /// may get [`GhError::Deferred`] here like anywhere else.
    pub async fn authenticated_login(&self) -> Result<String, GhError> {
        let url = self.at_base(USER_URL.to_string());
        let resp = self
            .send_metered(
                &url,
                CacheMode::Cached,
                self.client.get(&url).headers(self.headers()),
            )
            .await?;
        if !resp.is_success() {
            return Err(GhError::HttpStatus {
                status: resp.status().as_u16(),
                message: format!("GitHub API returned {}: {}", resp.status(), resp.text()),
            });
        }
        let body: serde_json::Value = resp
            .json()
            .map_err(|e| GhError::Failed(format!("Failed to parse /user response: {}", e)))?;
        body["login"]
            .as_str()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .ok_or_else(|| GhError::Failed("/user response carried no login".to_string()))
    }
}

/// The authenticated-user route, for [`GitHubClient::authenticated_login`].
const USER_URL: &str = "https://api.github.com/user";

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

        // A SECONDARY-limit 403: budget left, but Retry-After — a rate limit.
        assert_eq!(
            retry_backoff_secs(403, Some(45), Some(4_000), None, 1_000),
            Some(45)
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
    fn interactive_floor_sleeps_only_at_or_below_the_floor_before_the_reset() {
        assert_eq!(
            interactive_floor_wait_secs(RATE_LIMIT_FLOOR + 1, Some(1_120), 1_000),
            None
        );
        assert_eq!(
            interactive_floor_wait_secs(RATE_LIMIT_FLOOR, Some(1_120), 1_000),
            Some(120)
        );
        assert_eq!(
            interactive_floor_wait_secs(0, Some(1_000), 1_000),
            None,
            "already reset"
        );
        assert_eq!(
            interactive_floor_wait_secs(0, None, 1_000),
            None,
            "unknown reset is UNKNOWN"
        );
        assert_eq!(
            interactive_floor_wait_secs(0, Some(1_000 + 3_600), 1_000),
            Some(MAX_BACKOFF_SECS),
            "an hour-out reset is capped"
        );
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

    // -- Cache invalidation -------------------------------------------------

    /// A typo here fails every reconciler `get_pr`, and no other test would see it.
    #[test]
    fn pr_url_is_the_pulls_route() {
        assert_eq!(
            pr_url("o", "r", 7),
            "https://api.github.com/repos/o/r/pulls/7"
        );
    }

    /// Dropping a cached representation drops its cached `Link` with it: a body
    /// gone and a `rel="next"` left behind would resume a pagination whose first
    /// page no longer exists.
    #[test]
    fn invalidating_a_read_drops_its_body_and_its_cached_link() {
        let url = pr_url("o", "r", 424242);
        let key = link_cache_key(&url);
        github_budget::store(&url, "e", Bytes::from_static(b"[]"));
        github_budget::store(&key, "e", Bytes::from_static(b"https://next"));
        assert!(github_budget::replay(&url).is_some());

        // Exactly what `send_metered` does for `CacheAction::DropEntry`.
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
        assert!(
            matches!(&err, GhError::Failed(e) if e.contains("GitHub API request failed")),
            "{err}"
        );

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

    // -- Spend class: the background headroom gate (plan 2026-10-30-…, D4) --

    /// A reading 1% from empty, half a window from reset: under any sane
    /// headroom line, and above the interactive floor (50 > 10).
    fn one_percent_left() -> github_budget::RateLimitObservation {
        github_budget::RateLimitObservation {
            limit: Some(5000),
            remaining: Some(50),
            used: Some(4950),
            reset_at_unix: Some(now_unix() + 1800),
            resource: Some("core".to_string()),
        }
    }

    fn row_for(consumer: &str) -> Option<github_budget::ConsumerRow> {
        github_budget::snapshot_top(usize::MAX)
            .consumers
            .into_iter()
            .find(|r| r.consumer == consumer)
    }

    /// A background request under the headroom line is NEVER sent: against a
    /// port that refuses every connection it records no transport error —
    /// only a `deferred` — and comes back as a deferral, not a failure.
    #[tokio::test]
    async fn a_background_request_under_the_headroom_line_is_never_sent() {
        let url = "http://127.0.0.1:1/probe/background-deferred";
        let client = GitHubClient::new("budget-test-background-short", "budget_test_bg_short")
            .expect("client builds")
            .with_spend_class(SpendClass::Background);
        github_budget::seed_reading_for_test(client.credential(), &one_percent_left());

        let err = client
            .send_metered(url, CacheMode::Cached, client.client.get(url))
            .await
            .expect_err("a short window defers");
        assert!(matches!(err, GhError::Deferred(_)), "{err}");

        let row = row_for("budget_test_bg_short").expect("the deferral was metered");
        assert_eq!(row.deferred, 1);
        assert_eq!(row.transport_error, 0, "nothing was put on the wire");
        assert_eq!(row.charged + row.not_modified + row.rate_limited, 0);
    }

    /// The same 1%-left bucket does not touch an INTERACTIVE caller: it is
    /// sent (and so fails on the refusing port as a transport error).
    #[tokio::test]
    async fn an_interactive_request_is_unaffected_at_one_percent_remaining() {
        let url = "http://127.0.0.1:1/probe/interactive-one-percent";
        let client = GitHubClient::new("budget-test-interactive-low", "budget_test_interactive")
            .expect("client builds");
        github_budget::seed_reading_for_test(client.credential(), &one_percent_left());

        let err = client
            .send_metered(url, CacheMode::Cached, client.client.get(url))
            .await
            .expect_err("port 1 refuses");
        assert!(matches!(err, GhError::Failed(_)), "{err}");
        let row = row_for("budget_test_interactive").expect("recorded");
        assert_eq!(row.transport_error, 1, "the interactive request was sent");
        assert_eq!(row.deferred, 0);
    }

    /// One credential's exhausted bucket never defers ANOTHER credential's
    /// background request.
    #[tokio::test]
    async fn one_credentials_exhausted_bucket_never_defers_another() {
        let exhausted = CredentialKey::of_token("budget-test-exhausted-other-token");
        let mut empty = one_percent_left();
        empty.remaining = Some(0);
        // Exhaust the SHARED slot too, so a regression to reading it (rather
        // than this credential's own bucket) defers and turns this red.
        let _global = github_budget::global_last_lock();
        github_budget::seed_global_last_for_test(&exhausted, &empty);

        let url = "http://127.0.0.1:1/probe/background-other-credential";
        let client = GitHubClient::new("budget-test-healthy-token", "budget_test_bg_other")
            .expect("client builds")
            .with_spend_class(SpendClass::Background);
        assert_ne!(client.credential(), &exhausted);

        let err = client
            .send_metered(url, CacheMode::Cached, client.client.get(url))
            .await
            .expect_err("port 1 refuses");
        assert!(matches!(err, GhError::Failed(_)), "{err}");
        let row = row_for("budget_test_bg_other").expect("recorded");
        assert_eq!(row.transport_error, 1, "sent on its own (unknown) bucket");
        assert_eq!(row.deferred, 0);
    }

    /// An INTERACTIVE rate refusal holds the credential's BACKGROUND spend
    /// for GitHub's Retry-After window: while the interactive caller sleeps
    /// before its one retry, a background read on the same credential is
    /// deferred, not sent.
    #[tokio::test]
    async fn an_interactive_rate_refusal_holds_the_credentials_background_spend() {
        let token = "budget-test-interactive-refusal-hold";
        let base = one_shot_server_for_test(
            "HTTP/1.1 429 Too Many Requests\r\nretry-after: 3\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await;
        let interactive = GitHubClient::new(token, "budget_test_interactive_hold")
            .expect("client builds")
            .with_api_base(base);
        let pending = tokio::spawn(async move { interactive.get_pr("o", "r", 1).await });
        // Wait — bounded, never a fixed guess — until the interactive refusal
        // has landed and the credential's hold is observable.
        let credential = CredentialKey::of_token(token);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let held = matches!(
                github_budget::background_verdict_for_test(&credential),
                Err(d) if matches!(d.reason, github_budget::DeferralReason::RetryAfter { .. })
            );
            if held {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the interactive refusal never set the credential's hold"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let background = GitHubClient::new(token, "budget_test_interactive_hold_bg")
            .expect("client builds")
            .with_spend_class(SpendClass::Background)
            .with_api_base("http://127.0.0.1:1");
        let err = background.get_pr("o", "r", 2).await.expect_err("held");
        assert!(
            matches!(
                &err,
                GhError::Deferred(d)
                    if matches!(d.reason, github_budget::DeferralReason::RetryAfter { .. })
            ),
            "{err}"
        );
        // The interactive retry would go to the (now closed) one-shot server
        // after a JITTERED sleep of up to `RESUME_JITTER_SECS` past the
        // `Retry-After`; its outcome is not under test, so do not wait for it.
        pending.abort();
    }

    /// On the background path a rate-limited refusal — a `429`, or a
    /// secondary-limit `403` with budget left and `Retry-After` — is the typed
    /// `RateLimited`, a budget stop, never a plain failure and never slept on;
    /// it is metered `rate_limited` (not `charged`); and the credential's next
    /// background request is deferred inside the `Retry-After` window instead
    /// of being re-sent.
    ///
    /// RULE for any test whose server answers real `X-RateLimit-*` headers: it
    /// writes the process-global `last` reading, so it holds
    /// `github_budget::global_last_lock` for its whole body.
    #[tokio::test]
    async fn a_background_rate_limited_refusal_is_typed_and_not_slept_on() {
        let _global = github_budget::global_last_lock();
        for (why, token, consumer, response) in [
            (
                "429",
                "budget-test-rate-limited-429",
                "budget_test_rl_429",
                "HTTP/1.1 429 Too Many Requests\r\nretry-after: 120\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            ),
            (
                "secondary-limit 403",
                "budget-test-rate-limited-403",
                "budget_test_rl_403",
                "HTTP/1.1 403 Forbidden\r\nretry-after: 120\r\nx-ratelimit-remaining: 4000\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            ),
        ] {
            let base = one_shot_server_for_test(response).await;
            let client = GitHubClient::new(token, consumer)
                .expect("client builds")
                .with_spend_class(SpendClass::Background)
                .with_api_base(base);
            let started = std::time::Instant::now();
            let err = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                client.get_pr("o", "r", 1),
            )
            .await
            .unwrap_or_else(|_| panic!("{why}: a background request was slept on"))
            .expect_err("a refusal is an error");
            assert!(
                matches!(err, GhError::RateLimited { .. }) && err.is_budget_stop(),
                "{why}: {err}"
            );
            assert!(started.elapsed() < std::time::Duration::from_secs(20), "{why}");

            // Inside Retry-After the next background request is deferred — never
            // sent (the one-shot server is gone; a send would be a transport
            // failure).
            let again = client.get_pr("o", "r", 2).await.expect_err("held");
            assert!(
                matches!(
                    &again,
                    GhError::Deferred(d)
                        if matches!(d.reason, github_budget::DeferralReason::RetryAfter { .. })
                ),
                "{why}: {again}"
            );

            let row = row_for(consumer).expect("metered");
            assert_eq!(row.rate_limited, 1, "{why}: a rate refusal is not charged");
            assert_eq!(row.charged, 0, "{why}");
            assert_eq!(row.deferred, 1, "{why}");
            assert_eq!(row.transport_error, 0, "{why}");
        }
    }

    /// The reconciler's discovery listing goes through the same gate: under
    /// the headroom line it is a typed deferral with nothing sent, so the
    /// cycle stops spending rather than paging on.
    #[tokio::test]
    async fn a_background_repo_listing_under_the_headroom_line_is_never_sent() {
        let client = GitHubClient::new("budget-test-background-listing", "budget_test_bg_listing")
            .expect("client builds")
            .with_spend_class(SpendClass::Background)
            .with_api_base("http://127.0.0.1:1");
        github_budget::seed_reading_for_test(client.credential(), &one_percent_left());

        for slice in [
            RepoPrSlice::Open,
            RepoPrSlice::RecentlyClosed {
                since: Some("2026-11-19T10:00:00Z"),
            },
        ] {
            let err = client
                .list_repo_prs("o", "r", slice)
                .await
                .expect_err("a short window defers");
            assert!(matches!(&err, GhError::Deferred(_)), "{err}");
        }
        let row = row_for("budget_test_bg_listing").expect("the deferrals were metered");
        assert_eq!(row.deferred, 2);
        assert_eq!(row.transport_error, 0, "nothing was put on the wire");
        assert_eq!(row.charged + row.not_modified + row.rate_limited, 0);
    }

    #[test]
    fn the_api_base_override_re_roots_every_route_and_is_the_identity_by_default() {
        let prod = GitHubClient::new("t", "c").expect("builds");
        assert_eq!(prod.at_base(pr_url("o", "r", 7)), pr_url("o", "r", 7));
        let test = GitHubClient::new("t", "c")
            .expect("builds")
            .with_api_base("http://127.0.0.1:1");
        assert_eq!(
            test.at_base(pr_url("o", "r", 7)),
            "http://127.0.0.1:1/repos/o/r/pulls/7"
        );
    }

    #[test]
    fn a_new_client_is_interactive_and_keys_its_credential_by_digest() {
        let client = GitHubClient::new("budget-test-default-class", "c").expect("builds");
        assert_eq!(client.spend_class, SpendClass::Interactive);
        assert_eq!(
            client.credential(),
            &CredentialKey::of_token("budget-test-default-class")
        );
    }

    // -- Repo-wide PR discovery (plan 2026-10-30-…, D3) ----------------------

    #[test]
    fn repo_pr_listing_urls_are_open_by_100_and_closed_by_updated_desc_by_30() {
        assert_eq!(
            repo_prs_url("qontinui", "qontinui-runner", RepoPrSlice::Open),
            "https://api.github.com/repos/qontinui/qontinui-runner/pulls?state=open&per_page=100"
        );
        assert_eq!(
            repo_prs_url(
                "qontinui",
                "qontinui-runner",
                RepoPrSlice::RecentlyClosed {
                    since: Some("2026-11-19T10:00:00Z")
                }
            ),
            "https://api.github.com/repos/qontinui/qontinui-runner/pulls\
             ?state=closed&sort=updated&direction=desc&per_page=30"
        );
        // Not the per-branch route: no `head=` filter, so one read serves
        // every branch of the repo.
        assert!(!repo_prs_url("o", "r", RepoPrSlice::Open).contains("head="));
    }

    fn listed(updated_at: Option<&str>) -> HeadPr {
        parse_head_pr(&serde_json::json!({
            "number": 1,
            "state": "closed",
            "head": {"ref": "b", "label": "o:b", "sha": "s"},
            "base": {"ref": "main"},
            "updated_at": updated_at,
        }))
        .expect("parses")
    }

    #[test]
    fn a_closed_page_reaches_the_mark_only_with_an_item_no_newer_than_it() {
        let mark = "2026-11-19T10:00:00Z";
        let newer = listed(Some("2026-11-19T10:00:01Z"));
        let same = listed(Some("2026-11-19T10:00:00Z"));
        let older = listed(Some("2026-11-19T09:59:59Z"));
        assert!(
            !closed_page_reaches_mark(std::slice::from_ref(&newer), mark),
            "all newer: keep paging"
        );
        assert!(closed_page_reaches_mark(&[newer.clone(), same], mark));
        assert!(closed_page_reaches_mark(&[newer, older], mark));
        // An item with no readable `updated_at` proves nothing.
        assert!(!closed_page_reaches_mark(&[listed(None)], mark));
        assert!(
            !closed_page_reaches_mark(&[], mark),
            "an empty page is not the mark"
        );
    }

    #[test]
    fn a_listed_pr_carries_its_head_label_and_updated_at() {
        let pr = parse_head_pr(&serde_json::json!({
            "number": 7,
            "state": "open",
            "head": {"ref": "feat/x", "label": "someone-else:feat/x", "sha": "abc"},
            "base": {"ref": "main"},
            "updated_at": "2026-11-19T10:00:00Z",
            "merged_at": null,
        }))
        .expect("parses");
        assert_eq!(pr.head_label, "someone-else:feat/x");
        assert_eq!(pr.head_ref, "feat/x");
        assert_eq!(pr.updated_at.as_deref(), Some("2026-11-19T10:00:00Z"));
        assert!(!pr.merged);
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
