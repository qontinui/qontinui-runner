//! GitHub Issues provider implementation.
//!
//! Every exchange goes through [`GitHubTicketProvider::send_metered`], the one
//! door this module has onto GitHub, so budget accounting and conditional
//! requests are properties of the module rather than something each call site
//! must remember. See [`crate::github_budget`] and the plan
//! `2026-08-30-github-rest-budget-is-structurally-oversubscribed`.
//!
//! Ticket SYNC — the polled reads, `fetch_actionable` and `fetch_comments` — is
//! BACKGROUND spend (plan
//! `2026-10-30-session-pr-reconciler-drains-the-user-github-rest-budget-re-polling-terminal-prs`,
//! D4): each read passes [`github_budget::background_gate`] for the configured
//! token's own bucket, and one under the headroom line is deferred — never sent,
//! never slept on. The next poll asks again.
//!
//! The WRITES (`add_comment`, `update_state`) are INTERACTIVE. They are one-shot
//! write-backs of a completed task (`ticket_system::service::on_task_completed`) that
//! nothing retries, so deferring one would drop it for good; instead they keep
//! the shared interactive floor ([`interactive_floor`]) and the single
//! rate-limit retry. The provider's own copy of the old floor is gone either way.

use async_trait::async_trait;
use bytes::Bytes;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, ETAG, IF_NONE_MATCH, LINK, USER_AGENT,
};
use tracing::{debug, warn};

use crate::github_budget::{self, CacheMode, CredentialKey, SpendClass};
use crate::ticket_system::types::{
    Ticket, TicketComment, TicketProvider, TicketProviderConfig, TicketSource, TicketState,
};
use crate::trigger_system::github_api::{
    cache_action, cached_link_next, extract_next_url, interactive_floor, is_terminal_403,
    link_cache_key, now_unix, retry_backoff_secs, CacheAction, GhError,
};

/// Maximum total results to accumulate across paginated GitHub API calls.
const PAGINATION_CAP: usize = 500;

/// The budget-histogram label for every call this module makes.
const BUDGET_CONSUMER: &str = "ticket_system";

/// One completed exchange, body already read — a `304` carries none, so the
/// answer has to come from the ETag cache and a live `reqwest::Response` cannot
/// express that.
struct TicketResponse {
    status: reqwest::StatusCode,
    body: Bytes,
    link_next: Option<String>,
}

impl TicketResponse {
    fn is_success(&self) -> bool {
        self.status.is_success()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_slice(&self.body).map_err(|e| e.to_string())
    }
}

/// The parts of one wire exchange the send path needs after the body is read.
struct RawExchange {
    resp: TicketResponse,
    etag: Option<String>,
    retry_after: Option<u64>,
    remaining: Option<i64>,
    reset_at_unix: Option<i64>,
}

pub struct GitHubTicketProvider {
    client: reqwest::Client,
}

impl GitHubTicketProvider {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
        Ok(Self { client })
    }

    /// Put one request on the wire and fold it into the budget meter.
    ///
    /// The meter is updated from the response headers BEFORE the body is read:
    /// by then GitHub has already charged the request, so a body-read failure is
    /// not a transport error in budget terms. A failure to reach GitHub at all
    /// IS one — it cost a socket, and omitting it would make a storm of DNS
    /// failures read as "nothing happened".
    async fn send_once(
        &self,
        credential: &CredentialKey,
        url: &str,
        cache_mode: CacheMode,
        request: reqwest::RequestBuilder,
    ) -> Result<RawExchange, String> {
        let resp = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                github_budget::record_transport_error(BUDGET_CONSUMER, url, cache_mode);
                return Err(format!("GitHub API request failed: {}", e));
            }
        };

        let status = resp.status();
        let headers = resp.headers().clone();
        github_budget::record_response(
            credential,
            BUDGET_CONSUMER,
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
            resp: TicketResponse {
                status,
                body,
                link_next,
            },
            etag,
            retry_after,
            remaining: rate.remaining,
            reset_at_unix: rate.reset_at_unix,
        })
    }

    /// Send a request with a budget pre-check for its spend class, budget
    /// metering and conditional-request handling.
    ///
    /// `token` is the configured credential, used only to key its bucket by
    /// digest. BACKGROUND: under that bucket's headroom line the request returns
    /// [`GhError::Deferred`] without being sent (already counted as `deferred`
    /// on the meter), and a rate-limited refusal returns
    /// [`GhError::RateLimited`] without a sleep or a retry. INTERACTIVE: the
    /// shared [`interactive_floor`] and one retry after a rate-limited refusal.
    /// Both ask the one classifier, [`retry_backoff_secs`].
    ///
    /// `url` is passed explicitly rather than recovered from the built request:
    /// it is the ETag cache key AND the meter's template source, and a
    /// `RequestBuilder` will not surrender it without being consumed.
    async fn send_metered(
        &self,
        token: &str,
        spend_class: SpendClass,
        url: &str,
        cache_mode: CacheMode,
        request: reqwest::RequestBuilder,
    ) -> Result<TicketResponse, GhError> {
        let credential = CredentialKey::of_token(token);
        match spend_class {
            SpendClass::Background => {
                github_budget::background_gate(&credential, BUDGET_CONSUMER, url, cache_mode)
                    .map_err(GhError::Deferred)?;
            }
            SpendClass::Interactive => interactive_floor(&credential).await,
        }

        let unconditional = request.try_clone();
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
        let mut ex = self
            .send_once(&credential, url, cache_mode, request)
            .await?;

        let backoff = retry_backoff_secs(
            ex.resp.status.as_u16(),
            ex.retry_after,
            ex.remaining,
            ex.reset_at_unix,
            now_unix(),
        );
        if backoff.is_some() && spend_class == SpendClass::Background {
            // Background spend is never slept on; the next poll asks again.
            return Err(GhError::RateLimited {
                status: ex.resp.status.as_u16(),
            });
        }
        if let Some(wait) = backoff {
            warn!(
                "GitHub API rate limited ({}), retrying after {}s",
                ex.resp.status, wait
            );
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            let retry = retry_clone.ok_or("Failed to clone request for potential retry")?;
            ex = self.send_once(&credential, url, cache_mode, retry).await?;
        } else if is_terminal_403(ex.resp.status.as_u16(), ex.remaining) {
            // A permission 403, not an empty bucket. Terminal, as before.
            return Err(GhError::Failed(format!(
                "GitHub API returned {}: {}",
                ex.resp.status,
                ex.resp.text()
            )));
        }

        let mut effective_mode = cache_mode;
        if ex.resp.status == reqwest::StatusCode::NOT_MODIFIED {
            // FREE against the bucket. The body has to come from the cache.
            if let Some(body) = github_budget::replay(url) {
                debug!("ticket_system: 304 on {url} — replayed from cache, no budget spent");
                return Ok(TicketResponse {
                    status: reqwest::StatusCode::OK,
                    body,
                    link_next: ex.resp.link_next.or_else(|| cached_link_next(url)),
                });
            }
            // Validator held, body evicted. Re-ask once, unconditionally,
            // rather than handing back an empty 304 no caller can parse.
            debug!("ticket_system: validator held but body evicted for {url}; re-reading");
            let again = unconditional.ok_or("Failed to clone request for cache-miss re-read")?;
            ex = self
                .send_once(&credential, url, CacheMode::Fresh, again)
                .await?;
            effective_mode = CacheMode::Cached;
        }

        let link_key = link_cache_key(url);
        match cache_action(effective_mode, ex.resp.status.as_u16(), ex.etag.is_some()) {
            CacheAction::Store => {
                let tag = ex.etag.as_deref().unwrap_or_default();
                github_budget::store(url, tag, ex.resp.body.clone());
                match &ex.resp.link_next {
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

        Ok(ex.resp)
    }

    /// Issue a write and drop every cached representation it changes.
    ///
    /// INTERACTIVE spend: a write is a one-shot write-back nothing retries, so
    /// it is never deferred (see the module docs).
    ///
    /// `invalidates` carries FULL urls built the same way the reads build them —
    /// the cache keys on the exact URL including its query string, so a
    /// hand-typed near-miss silently invalidates nothing.
    async fn send_write(
        &self,
        token: &str,
        url: &str,
        invalidates: &[String],
        request: reqwest::RequestBuilder,
    ) -> Result<TicketResponse, GhError> {
        let resp = self
            .send_metered(
                token,
                SpendClass::Interactive,
                url,
                CacheMode::Uncacheable,
                request,
            )
            .await?;
        if resp.is_success() {
            for stale in invalidates {
                github_budget::invalidate(stale);
                github_budget::invalidate(&link_cache_key(stale));
            }
        }
        Ok(resp)
    }

    fn headers(&self, token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token))
                .unwrap_or_else(|_| HeaderValue::from_static("")),
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

    /// The URL builders below exist so a read and the write that invalidates it
    /// produce the BYTE-IDENTICAL string: the ETag cache keys on the full URL
    /// including its query, so an invalidation spelled out by hand at the write
    /// site would miss by a `?per_page=100` and leave a stale entry behind — a
    /// failure that shows up as a wrong answer, not as an error. Pure,
    /// unit-tested.
    fn issues_url(owner: &str, repo: &str, labels: &str) -> String {
        format!(
            "https://api.github.com/repos/{owner}/{repo}/issues?state=open&per_page=100{labels}"
        )
    }

    fn issue_comments_url(owner: &str, repo: &str, ticket_id: &str) -> String {
        format!(
            "https://api.github.com/repos/{owner}/{repo}/issues/{ticket_id}/comments?per_page=100"
        )
    }

    fn issue_url(owner: &str, repo: &str, ticket_id: &str) -> String {
        format!("https://api.github.com/repos/{owner}/{repo}/issues/{ticket_id}")
    }

    fn issue_labels_url(owner: &str, repo: &str, ticket_id: &str) -> String {
        format!("https://api.github.com/repos/{owner}/{repo}/issues/{ticket_id}/labels")
    }

    /// The `&labels=` suffix [`Self::issues_url`] is polled with. It is part of
    /// the cache key, so a write that invalidates the poll list has to rebuild
    /// it from the SAME config rather than assume the unfiltered form.
    fn labels_query(config: &TicketProviderConfig) -> String {
        if config.actionable_labels.is_empty() {
            String::new()
        } else {
            format!("&labels={}", config.actionable_labels.join(","))
        }
    }

    fn parse_target(target: &str) -> Result<(&str, &str), String> {
        let parts: Vec<&str> = target.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(format!(
                "Invalid GitHub target (expected owner/repo): {}",
                target
            ));
        }
        Ok((parts[0], parts[1]))
    }

    /// Parse a single page of issues JSON into Ticket structs.
    fn parse_issues_page(body: &serde_json::Value) -> Result<Vec<Ticket>, String> {
        let arr = body.as_array().ok_or("Expected JSON array")?;
        let mut tickets = Vec::new();
        for item in arr {
            // Skip pull requests (GitHub returns PRs in the issues endpoint)
            if item.get("pull_request").is_some() {
                continue;
            }
            let labels_arr = item["labels"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l["name"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            tickets.push(Ticket {
                external_id: item["number"]
                    .as_u64()
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                source: TicketSource::GitHub,
                title: item["title"].as_str().unwrap_or("").to_string(),
                body: item["body"].as_str().unwrap_or("").to_string(),
                labels: labels_arr,
                assignee: item["assignee"]["login"].as_str().map(|s| s.to_string()),
                url: item["html_url"].as_str().unwrap_or("").to_string(),
                state: TicketState::Open,
                created_at: item["created_at"].as_str().unwrap_or("").to_string(),
                updated_at: item["updated_at"].as_str().unwrap_or("").to_string(),
            });
        }
        Ok(tickets)
    }

    /// Parse a single page of comments JSON into TicketComment structs.
    fn parse_comments_page(body: &serde_json::Value) -> Result<Vec<TicketComment>, String> {
        let arr = body.as_array().ok_or("Expected JSON array")?;
        Ok(arr
            .iter()
            .map(|item| TicketComment {
                id: item["id"]
                    .as_u64()
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                author: item["user"]["login"].as_str().unwrap_or("").to_string(),
                body: item["body"].as_str().unwrap_or("").to_string(),
                created_at: item["created_at"].as_str().unwrap_or("").to_string(),
            })
            .collect())
    }
}

#[async_trait]
impl TicketProvider for GitHubTicketProvider {
    fn source(&self) -> TicketSource {
        TicketSource::GitHub
    }

    async fn fetch_actionable(&self, config: &TicketProviderConfig) -> Result<Vec<Ticket>, String> {
        let (owner, repo) = Self::parse_target(&config.target)?;

        let labels = Self::labels_query(config);

        let mut all_tickets = Vec::new();
        let mut next_url: Option<String> = Some(Self::issues_url(owner, repo, &labels));

        while let Some(url) = next_url.take() {
            // Conditional: an issue list is polled on a schedule and changes
            // between polls only when someone touches the tracker.
            let resp = self
                .send_metered(
                    &config.api_token,
                    SpendClass::Background,
                    &url,
                    CacheMode::Cached,
                    self.client
                        .get(&url)
                        .headers(self.headers(&config.api_token)),
                )
                .await?;

            if !resp.is_success() {
                return Err(format!(
                    "GitHub API returned {}: {}",
                    resp.status,
                    resp.text()
                ));
            }

            let link_header = resp.link_next.clone();

            let body: serde_json::Value = resp
                .json()
                .map_err(|e| format!("Failed to parse issues response: {}", e))?;

            let page_tickets = Self::parse_issues_page(&body)?;
            all_tickets.extend(page_tickets);

            if all_tickets.len() >= PAGINATION_CAP {
                warn!(
                    "fetch_actionable: pagination cap ({}) reached for {}/{}",
                    PAGINATION_CAP, owner, repo
                );
                all_tickets.truncate(PAGINATION_CAP);
                break;
            }

            next_url = link_header;
        }

        Ok(all_tickets)
    }

    async fn fetch_comments(
        &self,
        ticket_id: &str,
        config: &TicketProviderConfig,
    ) -> Result<Vec<TicketComment>, String> {
        let (owner, repo) = Self::parse_target(&config.target)?;

        let mut all_comments = Vec::new();
        let mut next_url: Option<String> = Some(Self::issue_comments_url(owner, repo, ticket_id));

        while let Some(url) = next_url.take() {
            let resp = self
                .send_metered(
                    &config.api_token,
                    SpendClass::Background,
                    &url,
                    CacheMode::Cached,
                    self.client
                        .get(&url)
                        .headers(self.headers(&config.api_token)),
                )
                .await?;

            if !resp.is_success() {
                return Err(format!(
                    "GitHub API returned {}: {}",
                    resp.status,
                    resp.text()
                ));
            }

            let link_header = resp.link_next.clone();

            let body: serde_json::Value = resp
                .json()
                .map_err(|e| format!("Failed to parse comments: {}", e))?;

            let page_comments = Self::parse_comments_page(&body)?;
            all_comments.extend(page_comments);

            if all_comments.len() >= PAGINATION_CAP {
                warn!(
                    "fetch_comments: pagination cap ({}) reached for {}/{} issue #{}",
                    PAGINATION_CAP, owner, repo, ticket_id
                );
                all_comments.truncate(PAGINATION_CAP);
                break;
            }

            next_url = link_header;
        }

        Ok(all_comments)
    }

    async fn add_comment(
        &self,
        ticket_id: &str,
        comment: &str,
        config: &TicketProviderConfig,
    ) -> Result<(), String> {
        let (owner, repo) = Self::parse_target(&config.target)?;
        // POST target and cached-read URL differ by the read's `?per_page`, so
        // the invalidation names the READ's url, not this one.
        let url = format!(
            "https://api.github.com/repos/{}/{}/issues/{}/comments",
            owner, repo, ticket_id
        );

        let resp = self
            .send_write(
                &config.api_token,
                &url,
                &[Self::issue_comments_url(owner, repo, ticket_id)],
                self.client
                    .post(&url)
                    .headers(self.headers(&config.api_token))
                    .json(&serde_json::json!({ "body": comment })),
            )
            .await?;

        if !resp.is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status,
                resp.text()
            ));
        }
        Ok(())
    }

    async fn update_state(
        &self,
        ticket_id: &str,
        state: TicketState,
        config: &TicketProviderConfig,
    ) -> Result<(), String> {
        let (owner, repo) = Self::parse_target(&config.target)?;
        let headers = self.headers(&config.api_token);

        // GitHub Issues only have "open" or "closed" — we use labels to express
        // finer-grained states like InProgress.
        let state_reason = match state {
            TicketState::Open | TicketState::InProgress => None,
            TicketState::Done => Some("completed"),
            TicketState::Closed => Some("not_planned"),
        };

        // --- Issue state PATCH (only when actually changing open/closed) --------
        let needs_patch = matches!(state, TicketState::Done | TicketState::Closed);
        if needs_patch {
            let url = Self::issue_url(owner, repo, ticket_id);
            let mut body = serde_json::json!({ "state": "closed" });
            if let Some(reason) = state_reason {
                body["state_reason"] = serde_json::Value::String(reason.to_string());
            }
            // Closing an issue drops it out of the `state=open` list this
            // provider polls, so that list's cached body is now wrong.
            let resp = self
                .send_write(
                    &config.api_token,
                    &url,
                    &[
                        Self::issues_url(owner, repo, &Self::labels_query(config)),
                        url.clone(),
                    ],
                    self.client.patch(&url).headers(headers.clone()).json(&body),
                )
                .await?;
            if !resp.is_success() {
                return Err(format!(
                    "GitHub API returned {}: {}",
                    resp.status,
                    resp.text()
                ));
            }
        }

        // --- Label management ---------------------------------------------------
        // Labels to add and remove depend on the target state.
        let (add_labels, remove_labels): (Vec<&str>, Vec<&str>) = match state {
            TicketState::InProgress => (vec!["in-progress"], vec!["ready", "todo"]),
            TicketState::Open => (vec![], vec!["in-progress"]),
            TicketState::Done | TicketState::Closed => (vec![], vec!["in-progress"]),
        };

        if !add_labels.is_empty() {
            let url = Self::issue_labels_url(owner, repo, ticket_id);
            let req = self
                .client
                .post(&url)
                .headers(headers.clone())
                .json(&serde_json::json!({ "labels": add_labels }));
            // A label change alters the issue body AND can move the issue in or
            // out of the label-filtered list this provider polls.
            let invalidates = [
                Self::issues_url(owner, repo, &Self::labels_query(config)),
                Self::issue_url(owner, repo, ticket_id),
            ];
            match self
                .send_write(&config.api_token, &url, &invalidates, req)
                .await
            {
                Ok(resp) if !resp.is_success() => {
                    tracing::warn!(
                        "GitHub: failed to add labels {:?} to issue {}: {} {}",
                        add_labels,
                        ticket_id,
                        resp.status,
                        resp.text()
                    );
                }
                Err(e) => {
                    tracing::warn!("GitHub: labels API error for issue {}: {}", ticket_id, e);
                }
                _ => {}
            }
        }

        for label in remove_labels {
            let url = format!(
                "https://api.github.com/repos/{}/{}/issues/{}/labels/{}",
                owner, repo, ticket_id, label
            );
            let req = self.client.delete(&url).headers(headers.clone());
            let invalidates = [
                Self::issues_url(owner, repo, &Self::labels_query(config)),
                Self::issue_url(owner, repo, ticket_id),
            ];
            match self
                .send_write(&config.api_token, &url, &invalidates, req)
                .await
            {
                Ok(resp) if !resp.is_success() && resp.status.as_u16() != 404 => {
                    tracing::warn!(
                        "GitHub: failed to remove label '{}' from issue {}: {} {}",
                        label,
                        ticket_id,
                        resp.status,
                        resp.text()
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "GitHub: labels API error for issue {} label '{}': {}",
                        ticket_id,
                        label,
                        e,
                    );
                }
                _ => {}
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short_window() -> github_budget::RateLimitObservation {
        github_budget::RateLimitObservation {
            limit: Some(5000),
            remaining: Some(50),
            used: Some(4950),
            reset_at_unix: Some(crate::trigger_system::github_api::now_unix() + 1800),
            resource: Some("core".to_string()),
        }
    }

    fn row_for(consumer: &str, template: &str) -> Option<github_budget::ConsumerRow> {
        github_budget::snapshot_top(usize::MAX)
            .consumers
            .into_iter()
            .find(|r| r.consumer == consumer && r.template == template)
    }

    /// Ticket sync is background spend: under the configured token's headroom
    /// line a request is deferred — never sent (no transport error against a
    /// refusing port), never slept on.
    #[tokio::test]
    async fn ticket_sync_under_the_headroom_line_is_deferred_not_sent() {
        let token = "ticket-system-test-token-short";
        github_budget::seed_reading_for_test(&CredentialKey::of_token(token), &short_window());
        let provider = GitHubTicketProvider::new().expect("provider builds");
        let url = "http://127.0.0.1:1/probe/ticket-deferred";

        let err = provider
            .send_metered(
                token,
                SpendClass::Background,
                url,
                CacheMode::Cached,
                provider.client.get(url),
            )
            .await
            .err()
            .expect("a short window defers");
        assert!(matches!(err, GhError::Deferred(_)), "{err}");

        let row = row_for(BUDGET_CONSUMER, "/probe/ticket-deferred").expect("metered");
        assert_eq!(row.deferred, 1);
        assert_eq!(row.transport_error, 0, "nothing was put on the wire");
    }

    /// Another token's bucket says nothing about this one: with no reading of
    /// its own the request is sent (and fails on the refusing port).
    #[tokio::test]
    async fn ticket_sync_reads_its_own_tokens_bucket() {
        // Exhaust the SHARED slot with another token's reading, so a regression
        // to reading it (rather than this token's bucket) defers and goes red.
        let _global = github_budget::global_last_lock().await;
        let mut empty = short_window();
        empty.remaining = Some(0);
        github_budget::seed_global_last_for_test(
            &CredentialKey::of_token("ticket-system-test-other-token"),
            &empty,
        );
        let provider = GitHubTicketProvider::new().expect("provider builds");
        let url = "http://127.0.0.1:1/probe/ticket-own-bucket";

        let err = provider
            .send_metered(
                "ticket-system-test-healthy-token",
                SpendClass::Background,
                url,
                CacheMode::Cached,
                provider.client.get(url),
            )
            .await
            .err()
            .expect("port 1 refuses");
        assert!(matches!(err, GhError::Failed(_)), "{err}");
        let row = row_for(BUDGET_CONSUMER, "/probe/ticket-own-bucket").expect("metered");
        assert_eq!(row.transport_error, 1, "sent on its own bucket");
        assert_eq!(row.deferred, 0);
    }

    /// A completion write-back (`add_comment` / `update_state`) is one-shot and
    /// nothing retries it, so a write is INTERACTIVE: under the headroom line it
    /// is still sent (here, to a refusing port) — never deferred and dropped.
    #[tokio::test]
    async fn a_ticket_write_under_the_headroom_line_is_not_deferred() {
        let token = "ticket-system-test-token-write";
        github_budget::seed_reading_for_test(&CredentialKey::of_token(token), &short_window());
        let provider = GitHubTicketProvider::new().expect("provider builds");
        let url = "http://127.0.0.1:1/probe/ticket-write";

        let err = provider
            .send_write(token, url, &[], provider.client.post(url))
            .await
            .err()
            .expect("port 1 refuses");
        assert!(matches!(err, GhError::Failed(_)), "{err}");
        let row = row_for(BUDGET_CONSUMER, "/probe/ticket-write").expect("metered");
        assert_eq!(row.deferred, 0, "a write-back is never deferred");
        assert_eq!(row.transport_error, 1, "the write was sent");
    }
}
