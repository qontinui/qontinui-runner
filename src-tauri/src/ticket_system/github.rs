//! GitHub Issues provider implementation.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};

use crate::ticket_system::types::{
    Ticket, TicketComment, TicketProvider, TicketProviderConfig, TicketSource, TicketState,
};

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
}

#[async_trait]
impl TicketProvider for GitHubTicketProvider {
    fn source(&self) -> TicketSource {
        TicketSource::GitHub
    }

    async fn fetch_actionable(
        &self,
        config: &TicketProviderConfig,
    ) -> Result<Vec<Ticket>, String> {
        let (owner, repo) = Self::parse_target(&config.target)?;

        let labels = if config.actionable_labels.is_empty() {
            String::new()
        } else {
            format!("&labels={}", config.actionable_labels.join(","))
        };

        let url = format!(
            "https://api.github.com/repos/{}/{}/issues?state=open&per_page=100{}",
            owner, repo, labels
        );

        let resp = self
            .client
            .get(&url)
            .headers(self.headers(&config.api_token))
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
            .map_err(|e| format!("Failed to parse issues response: {}", e))?;

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

    async fn fetch_comments(
        &self,
        ticket_id: &str,
        config: &TicketProviderConfig,
    ) -> Result<Vec<TicketComment>, String> {
        let (owner, repo) = Self::parse_target(&config.target)?;
        let url = format!(
            "https://api.github.com/repos/{}/{}/issues/{}/comments?per_page=100",
            owner, repo, ticket_id
        );

        let resp = self
            .client
            .get(&url)
            .headers(self.headers(&config.api_token))
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
            .map_err(|e| format!("Failed to parse comments: {}", e))?;

        let arr = body.as_array().ok_or("Expected JSON array")?;
        let comments = arr
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
            .collect();
        Ok(comments)
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
            .client
            .post(&url)
            .headers(self.headers(&config.api_token))
            .json(&serde_json::json!({ "body": comment }))
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

    async fn update_state(
        &self,
        ticket_id: &str,
        state: TicketState,
        config: &TicketProviderConfig,
    ) -> Result<(), String> {
        let (owner, repo) = Self::parse_target(&config.target)?;
        let url = format!(
            "https://api.github.com/repos/{}/{}/issues/{}",
            owner, repo, ticket_id
        );

        // GitHub Issues only have "open" or "closed" — InProgress/Done map differently
        let (state_str, state_reason) = match state {
            TicketState::Open | TicketState::InProgress => ("open", None),
            TicketState::Done => ("closed", Some("completed")),
            TicketState::Closed => ("closed", Some("not_planned")),
        };

        let mut body = serde_json::json!({ "state": state_str });
        if let Some(reason) = state_reason {
            body["state_reason"] = serde_json::Value::String(reason.to_string());
        }

        let resp = self
            .client
            .patch(&url)
            .headers(self.headers(&config.api_token))
            .json(&body)
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
}
