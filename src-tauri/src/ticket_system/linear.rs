//! Linear Issues provider implementation (GraphQL API).

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};

use crate::ticket_system::types::{
    Ticket, TicketComment, TicketProvider, TicketProviderConfig, TicketSource, TicketState,
};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";

pub struct LinearTicketProvider {
    client: reqwest::Client,
    /// Cache: team key → team ID (static per Linear workspace, safe to cache for provider lifetime)
    team_id_cache: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// Cache: (team_id, state_type) → workflow state ID
    workflow_state_cache: std::sync::Mutex<std::collections::HashMap<(String, String), String>>,
}

impl LinearTicketProvider {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
        Ok(Self {
            client,
            team_id_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            workflow_state_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn headers(&self, token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(token).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers
    }

    /// Execute a GraphQL query/mutation against the Linear API.
    async fn graphql(
        &self,
        token: &str,
        query: &str,
        variables: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let mut body = serde_json::json!({ "query": query });
        if let Some(vars) = variables {
            body["variables"] = vars;
        }

        let resp = self
            .client
            .post(LINEAR_API_URL)
            .headers(self.headers(token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Linear API request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Linear API returned {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse Linear response: {}", e))?;

        if let Some(errors) = result.get("errors") {
            return Err(format!("Linear GraphQL errors: {}", errors));
        }

        Ok(result)
    }

    /// Resolve the internal team ID from the team key (e.g., "ENG").
    /// Results are cached for the lifetime of this provider instance.
    async fn resolve_team_id(
        &self,
        token: &str,
        team_key: &str,
    ) -> Result<String, String> {
        // Check cache first
        if let Some(cached) = self.team_id_cache.lock().unwrap().get(team_key) {
            return Ok(cached.clone());
        }

        let query = r#"
            query($key: String!) {
                teams(filter: { key: { eq: $key } }) {
                    nodes { id key }
                }
            }
        "#;
        let vars = serde_json::json!({ "key": team_key });
        let result = self.graphql(token, query, Some(vars)).await?;

        let teams = &result["data"]["teams"]["nodes"];
        let team = teams
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| format!("Linear team '{}' not found", team_key))?;

        let id = team["id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "Team ID missing from response".to_string())?;

        self.team_id_cache.lock().unwrap().insert(team_key.to_string(), id.clone());
        Ok(id)
    }

    /// Fetch workflow states for a team and find the state ID matching a given type.
    /// Results are cached for the lifetime of this provider instance.
    async fn find_workflow_state_id(
        &self,
        token: &str,
        team_id: &str,
        state_type: &str,
    ) -> Result<String, String> {
        let cache_key = (team_id.to_string(), state_type.to_string());
        if let Some(cached) = self.workflow_state_cache.lock().unwrap().get(&cache_key) {
            return Ok(cached.clone());
        }

        let query = r#"
            query($teamId: String!) {
                workflowStates(filter: { team: { id: { eq: $teamId } } }) {
                    nodes { id name type }
                }
            }
        "#;
        let vars = serde_json::json!({ "teamId": team_id });
        let result = self.graphql(token, query, Some(vars)).await?;

        let states = result["data"]["workflowStates"]["nodes"]
            .as_array()
            .ok_or("Expected workflow states array")?;

        // Cache all states from this response (avoids re-fetching for different state types)
        for state in states {
            if let (Some(sid), Some(stype)) = (state["id"].as_str(), state["type"].as_str()) {
                self.workflow_state_cache.lock().unwrap()
                    .insert((team_id.to_string(), stype.to_string()), sid.to_string());
            }
        }

        self.workflow_state_cache.lock().unwrap()
            .get(&cache_key)
            .cloned()
            .ok_or_else(|| format!(
                "No workflow state with type '{}' found for team {}",
                state_type, team_id
            ))
    }

    /// Map a TicketState to a Linear workflow state type string.
    fn ticket_state_to_linear_type(state: TicketState) -> &'static str {
        match state {
            TicketState::Open => "unstarted",
            TicketState::InProgress => "started",
            TicketState::Done => "completed",
            TicketState::Closed => "cancelled",
        }
    }

    /// Map a Linear workflow state type to a TicketState.
    fn linear_type_to_ticket_state(state_type: &str) -> TicketState {
        match state_type {
            "started" => TicketState::InProgress,
            "completed" => TicketState::Done,
            "cancelled" => TicketState::Closed,
            // triage, backlog, unstarted all map to Open
            _ => TicketState::Open,
        }
    }
}

#[async_trait]
impl TicketProvider for LinearTicketProvider {
    fn source(&self) -> TicketSource {
        TicketSource::Linear
    }

    async fn fetch_actionable(
        &self,
        config: &TicketProviderConfig,
    ) -> Result<Vec<Ticket>, String> {
        let team_id = self
            .resolve_team_id(&config.api_token, &config.target)
            .await?;

        // Build label filter: if actionable_labels is non-empty, filter by them.
        // Sanitize label names to prevent GraphQL injection (escape backslashes and quotes).
        let label_filter = if config.actionable_labels.is_empty() {
            String::new()
        } else {
            let labels_json: Vec<String> = config
                .actionable_labels
                .iter()
                .map(|l| {
                    let escaped = l.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{}\"", escaped)
                })
                .collect();
            format!(", labels: {{ name: {{ in: [{}] }} }}", labels_json.join(", "))
        };

        let query = format!(
            r#"query {{
                team(id: "{}") {{
                    issues(filter: {{
                        state: {{ type: {{ in: ["triage", "backlog", "unstarted"] }} }}
                        {}
                    }}) {{
                        nodes {{
                            id
                            identifier
                            title
                            description
                            url
                            state {{ name type }}
                            labels {{ nodes {{ name }} }}
                            assignee {{ name }}
                            createdAt
                            updatedAt
                        }}
                    }}
                }}
            }}"#,
            team_id, label_filter
        );

        let result = self.graphql(&config.api_token, &query, None).await?;

        let issues = result["data"]["team"]["issues"]["nodes"]
            .as_array()
            .ok_or("Expected issues array in response")?;

        let mut tickets = Vec::new();
        for issue in issues {
            let labels_arr = issue["labels"]["nodes"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l["name"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let state_type = issue["state"]["type"].as_str().unwrap_or("unstarted");

            tickets.push(Ticket {
                external_id: issue["id"].as_str().unwrap_or("").to_string(),
                source: TicketSource::Linear,
                title: issue["title"].as_str().unwrap_or("").to_string(),
                body: issue["description"].as_str().unwrap_or("").to_string(),
                labels: labels_arr,
                assignee: issue["assignee"]["name"].as_str().map(|s| s.to_string()),
                url: issue["url"].as_str().unwrap_or("").to_string(),
                state: Self::linear_type_to_ticket_state(state_type),
                created_at: issue["createdAt"].as_str().unwrap_or("").to_string(),
                updated_at: issue["updatedAt"].as_str().unwrap_or("").to_string(),
            });
        }

        Ok(tickets)
    }

    async fn fetch_comments(
        &self,
        ticket_id: &str,
        config: &TicketProviderConfig,
    ) -> Result<Vec<TicketComment>, String> {
        let query = r#"
            query($issueId: String!) {
                issue(id: $issueId) {
                    comments {
                        nodes {
                            id
                            body
                            createdAt
                            user { name }
                        }
                    }
                }
            }
        "#;
        let vars = serde_json::json!({ "issueId": ticket_id });
        let result = self.graphql(&config.api_token, query, Some(vars)).await?;

        let comments_arr = result["data"]["issue"]["comments"]["nodes"]
            .as_array()
            .ok_or("Expected comments array in response")?;

        let comments = comments_arr
            .iter()
            .map(|c| TicketComment {
                id: c["id"].as_str().unwrap_or("").to_string(),
                author: c["user"]["name"].as_str().unwrap_or("").to_string(),
                body: c["body"].as_str().unwrap_or("").to_string(),
                created_at: c["createdAt"].as_str().unwrap_or("").to_string(),
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
        let query = r#"
            mutation($issueId: String!, $body: String!) {
                commentCreate(input: { issueId: $issueId, body: $body }) {
                    success
                }
            }
        "#;
        let vars = serde_json::json!({
            "issueId": ticket_id,
            "body": comment,
        });
        let result = self.graphql(&config.api_token, query, Some(vars)).await?;

        let success = result["data"]["commentCreate"]["success"]
            .as_bool()
            .unwrap_or(false);
        if !success {
            return Err("Linear commentCreate mutation returned success=false".to_string());
        }

        Ok(())
    }

    async fn update_state(
        &self,
        ticket_id: &str,
        state: TicketState,
        config: &TicketProviderConfig,
    ) -> Result<(), String> {
        // First resolve the team ID so we can look up workflow states.
        let team_id = self
            .resolve_team_id(&config.api_token, &config.target)
            .await?;

        // Find the workflow state ID matching the desired type.
        let linear_type = Self::ticket_state_to_linear_type(state);
        let state_id = self
            .find_workflow_state_id(&config.api_token, &team_id, linear_type)
            .await?;

        let query = r#"
            mutation($issueId: String!, $stateId: String!) {
                issueUpdate(id: $issueId, input: { stateId: $stateId }) {
                    success
                }
            }
        "#;
        let vars = serde_json::json!({
            "issueId": ticket_id,
            "stateId": state_id,
        });
        let result = self.graphql(&config.api_token, query, Some(vars)).await?;

        let success = result["data"]["issueUpdate"]["success"]
            .as_bool()
            .unwrap_or(false);
        if !success {
            return Err("Linear issueUpdate mutation returned success=false".to_string());
        }

        Ok(())
    }
}
