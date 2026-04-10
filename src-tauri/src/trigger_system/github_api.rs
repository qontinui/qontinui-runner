//! Lightweight GitHub REST API client for the PR watcher.
//!
//! Uses reqwest with token-based authentication. Rate-limit aware.

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use tracing::warn;

/// Maximum total results to accumulate across paginated GitHub API calls.
const PAGINATION_CAP: usize = 500;

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

/// A minimal GitHub API client.
pub struct GitHubClient {
    client: reqwest::Client,
    token: String,
    rate_limit_remaining: AtomicU32,
    rate_limit_reset: AtomicU64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrStatus {
    pub number: u64,
    pub state: String,
    pub merged: bool,
    pub mergeable: Option<bool>,
    pub head_sha: String,
    pub title: String,
    pub html_url: String,
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
    pub fn new(token: &str) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
        Ok(Self {
            client,
            token: token.to_string(),
            rate_limit_remaining: AtomicU32::new(u32::MAX),
            rate_limit_reset: AtomicU64::new(0),
        })
    }

    /// Wait if we are close to the GitHub API rate limit.
    async fn check_rate_limit(&self) {
        let remaining = self.rate_limit_remaining.load(Ordering::Relaxed);
        if remaining < 10 {
            let reset = self.rate_limit_reset.load(Ordering::Relaxed);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now < reset {
                let wait = reset - now;
                warn!(
                    "GitHub API rate limit low ({} remaining), waiting {}s",
                    remaining, wait
                );
                tokio::time::sleep(std::time::Duration::from_secs(wait.min(300))).await;
            }
        }
    }

    /// Update rate limit tracking from response headers.
    fn update_rate_limit(&self, headers: &HeaderMap) {
        if let Some(val) = headers.get("x-ratelimit-remaining") {
            if let Ok(s) = val.to_str() {
                if let Ok(n) = s.parse::<u32>() {
                    self.rate_limit_remaining.store(n, Ordering::Relaxed);
                }
            }
        }
        if let Some(val) = headers.get("x-ratelimit-reset") {
            if let Ok(s) = val.to_str() {
                if let Ok(n) = s.parse::<u64>() {
                    self.rate_limit_reset.store(n, Ordering::Relaxed);
                }
            }
        }
    }

    /// Send a request with rate-limit pre-check, header tracking, and 403/429 retry.
    /// Returns the successful response or an error. Retries at most once.
    async fn send_with_rate_limit(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, String> {
        self.check_rate_limit().await;

        let req = request
            .try_clone()
            .ok_or("Failed to clone request for potential retry")?;

        let resp = req
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {}", e))?;

        self.update_rate_limit(resp.headers());

        let status = resp.status();
        if status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            // Determine wait duration
            let wait_secs = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                // Use Retry-After header if present
                resp.headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(60)
            } else {
                // 403 — check if rate-limited (remaining == 0)
                let remaining = resp
                    .headers()
                    .get("x-ratelimit-remaining")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u32>().ok());
                if remaining == Some(0) {
                    let reset = self.rate_limit_reset.load(Ordering::Relaxed);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if now < reset {
                        reset - now
                    } else {
                        60
                    }
                } else {
                    // Not a rate-limit 403, don't retry
                    return Err(format!(
                        "GitHub API returned {}: {}",
                        status,
                        resp.text().await.unwrap_or_default()
                    ));
                }
            };

            let capped = wait_secs.min(300);
            warn!(
                "GitHub API rate limited ({}), retrying after {}s",
                status, capped
            );
            tokio::time::sleep(std::time::Duration::from_secs(capped)).await;

            // Single retry
            let retry_resp = request
                .send()
                .await
                .map_err(|e| format!("GitHub API retry request failed: {}", e))?;

            self.update_rate_limit(retry_resp.headers());
            return Ok(retry_resp);
        }

        Ok(resp)
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.token))
                .unwrap_or_else(|e| {
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
    pub async fn get_pr(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<PrStatus, String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            owner, repo, number
        );
        let resp = self
            .send_with_rate_limit(self.client.get(&url).headers(self.headers()))
            .await?;

        if !resp.status().is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse PR response: {}", e))?;

        Ok(PrStatus {
            number,
            state: body["state"].as_str().unwrap_or("unknown").to_string(),
            merged: body["merged"].as_bool().unwrap_or(false),
            mergeable: body["mergeable"].as_bool(),
            head_sha: body["head"]["sha"].as_str().unwrap_or("").to_string(),
            title: body["title"].as_str().unwrap_or("").to_string(),
            html_url: body["html_url"].as_str().unwrap_or("").to_string(),
        })
    }

    /// Fetch check runs for a commit SHA (paginated).
    pub async fn get_check_runs(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> Result<Vec<CheckRun>, String> {
        let mut all_runs = Vec::new();
        let mut next_url: Option<String> = Some(format!(
            "https://api.github.com/repos/{}/{}/commits/{}/check-runs?per_page=100",
            owner, repo, sha
        ));

        while let Some(url) = next_url.take() {
            let resp = self
                .send_with_rate_limit(self.client.get(&url).headers(self.headers()))
                .await?;

            if !resp.status().is_success() {
                return Err(format!(
                    "GitHub API returned {}: {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                ));
            }

            // Extract next page URL before consuming the response body
            let link_header = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(extract_next_url);

            let body: CheckRunsResponse = resp
                .json()
                .await
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
        let mut next_url: Option<String> = Some(format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/reviews?per_page=100",
            owner, repo, number
        ));

        while let Some(url) = next_url.take() {
            let resp = self
                .send_with_rate_limit(self.client.get(&url).headers(self.headers()))
                .await?;

            if !resp.status().is_success() {
                return Err(format!(
                    "GitHub API returned {}: {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                ));
            }

            let link_header = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(extract_next_url);

            let reviews: Vec<PrReview> = resp
                .json()
                .await
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
    pub async fn get_check_run_log(
        &self,
        owner: &str,
        repo: &str,
        check_run_id: u64,
    ) -> Result<String, String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/actions/jobs/{}/logs",
            owner, repo, check_run_id
        );
        let resp = self
            .send_with_rate_limit(self.client.get(&url).headers(self.headers()))
            .await?;

        if !resp.status().is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }

        let full_log = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read log body: {}", e))?;

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

    /// Post a comment on a PR.
    pub async fn create_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<(), String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/issues/{}/comments",
            owner, repo, number
        );
        let resp = self
            .send_with_rate_limit(
                self.client
                    .post(&url)
                    .headers(self.headers())
                    .json(&serde_json::json!({ "body": body })),
            )
            .await?;

        if !resp.status().is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }

        Ok(())
    }

    /// Submit a PR review (APPROVE, REQUEST_CHANGES, or COMMENT).
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
            .send_with_rate_limit(
                self.client
                    .post(&url)
                    .headers(self.headers())
                    .json(&serde_json::json!({ "event": event, "body": body })),
            )
            .await?;

        if !resp.status().is_success() {
            return Err(format!(
                "GitHub submit_pr_review returned {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
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
