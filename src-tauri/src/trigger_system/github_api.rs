//! Lightweight GitHub REST API client for the PR watcher.
//!
//! Uses reqwest with token-based authentication. Rate-limit aware.

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A minimal GitHub API client.
pub struct GitHubClient {
    client: reqwest::Client,
    token: String,
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
        })
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.token)).unwrap(),
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
            .client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {}", e))?;

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

    /// Fetch check runs for a commit SHA.
    pub async fn get_check_runs(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> Result<Vec<CheckRun>, String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/commits/{}/check-runs?per_page=100",
            owner, repo, sha
        );
        let resp = self
            .client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }

        let body: CheckRunsResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse check runs: {}", e))?;

        Ok(body.check_runs)
    }

    /// Fetch reviews for a PR.
    pub async fn get_pr_reviews(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Vec<PrReview>, String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/reviews?per_page=100",
            owner, repo, number
        );
        let resp = self
            .client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }

        let reviews: Vec<PrReview> = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse reviews: {}", e))?;

        Ok(reviews)
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
            .client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {}", e))?;

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
            .client
            .post(&url)
            .headers(self.headers())
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "GitHub API returned {}: {}",
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
