//! GitHub Issues provider implementation.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use tracing::warn;

use crate::ticket_system::types::{
    Ticket, TicketComment, TicketProvider, TicketProviderConfig, TicketSource, TicketState,
};
use crate::trigger_system::github_api::extract_next_url;

/// Maximum total results to accumulate across paginated GitHub API calls.
const PAGINATION_CAP: usize = 500;

pub struct GitHubTicketProvider {
    client: reqwest::Client,
    rate_limit_remaining: AtomicU32,
    rate_limit_reset: AtomicU64,
}

impl GitHubTicketProvider {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
        Ok(Self {
            client,
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
            let wait_secs = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                resp.headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(60)
            } else {
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

            let retry_resp = request
                .send()
                .await
                .map_err(|e| format!("GitHub API retry request failed: {}", e))?;

            self.update_rate_limit(retry_resp.headers());
            return Ok(retry_resp);
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

        let labels = if config.actionable_labels.is_empty() {
            String::new()
        } else {
            format!("&labels={}", config.actionable_labels.join(","))
        };

        let mut all_tickets = Vec::new();
        let mut next_url: Option<String> = Some(format!(
            "https://api.github.com/repos/{}/{}/issues?state=open&per_page=100{}",
            owner, repo, labels
        ));

        while let Some(url) = next_url.take() {
            let resp = self
                .send_with_rate_limit(
                    self.client
                        .get(&url)
                        .headers(self.headers(&config.api_token)),
                )
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

            let body: serde_json::Value = resp
                .json()
                .await
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
        let mut next_url: Option<String> = Some(format!(
            "https://api.github.com/repos/{}/{}/issues/{}/comments?per_page=100",
            owner, repo, ticket_id
        ));

        while let Some(url) = next_url.take() {
            let resp = self
                .send_with_rate_limit(
                    self.client
                        .get(&url)
                        .headers(self.headers(&config.api_token)),
                )
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

            let body: serde_json::Value = resp
                .json()
                .await
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
        let url = format!(
            "https://api.github.com/repos/{}/{}/issues/{}/comments",
            owner, repo, ticket_id
        );

        let resp = self
            .send_with_rate_limit(
                self.client
                    .post(&url)
                    .headers(self.headers(&config.api_token))
                    .json(&serde_json::json!({ "body": comment })),
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
            let url = format!(
                "https://api.github.com/repos/{}/{}/issues/{}",
                owner, repo, ticket_id
            );
            let mut body = serde_json::json!({ "state": "closed" });
            if let Some(reason) = state_reason {
                body["state_reason"] = serde_json::Value::String(reason.to_string());
            }
            let resp = self
                .send_with_rate_limit(self.client.patch(&url).headers(headers.clone()).json(&body))
                .await?;
            if !resp.status().is_success() {
                return Err(format!(
                    "GitHub API returned {}: {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
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
            let url = format!(
                "https://api.github.com/repos/{}/{}/issues/{}/labels",
                owner, repo, ticket_id
            );
            let req = self
                .client
                .post(&url)
                .headers(headers.clone())
                .json(&serde_json::json!({ "labels": add_labels }));
            match self.send_with_rate_limit(req).await {
                Ok(resp) if !resp.status().is_success() => {
                    tracing::warn!(
                        "GitHub: failed to add labels {:?} to issue {}: {} {}",
                        add_labels,
                        ticket_id,
                        resp.status(),
                        resp.text().await.unwrap_or_default()
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
            match self.send_with_rate_limit(req).await {
                Ok(resp) if !resp.status().is_success() && resp.status().as_u16() != 404 => {
                    tracing::warn!(
                        "GitHub: failed to remove label '{}' from issue {}: {} {}",
                        label,
                        ticket_id,
                        resp.status(),
                        resp.text().await.unwrap_or_default()
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
