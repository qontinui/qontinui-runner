//! HTTP clients for communicating with target runners and the supervisor.

use serde::Deserialize;
use std::time::Duration;

/// Client for interacting with a target runner's HTTP API.
pub struct RunnerClient {
    client: reqwest::Client,
    base_url: String,
}

/// Client for interacting with the supervisor's HTTP API.
pub struct SupervisorClient {
    client: reqwest::Client,
    base_url: String,
}

#[derive(Deserialize)]
struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl RunnerClient {
    pub fn new(port: u16) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            base_url: format!("http://127.0.0.1:{}", port),
        }
    }

    /// Check if the runner's API is responding.
    pub async fn is_healthy(&self) -> bool {
        self.client
            .get(format!("{}/health", self.base_url))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Start a workflow and return its task_run_id.
    pub async fn start_workflow(&self, workflow_id: &str) -> Result<String, String> {
        let url = format!(
            "{}/unified-workflows/{}/run",
            self.base_url, workflow_id
        );
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| format!("Failed to start workflow: {}", e))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse start response: {}", e))?;

        // Handle ApiResponse wrapper
        let data = if body.get("success").is_some() {
            body.get("data").cloned().unwrap_or(body.clone())
        } else {
            body
        };

        data.get("task_run_id")
            .or_else(|| data.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("No task_run_id in response: {}", data))
    }

    /// Poll a task run until it completes. Returns the workflow state.
    pub async fn poll_until_complete(
        &self,
        task_run_id: &str,
        stop_rx: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<serde_json::Value, String> {
        let poll_interval = Duration::from_secs(5);
        let mut stale_count = 0u32;

        loop {
            // Check stop signal
            if *stop_rx.borrow() {
                return Err("Loop stopped".to_string());
            }

            // Check workflow state
            let url = format!(
                "{}/task-runs/{}/workflow-state",
                self.base_url, task_run_id
            );
            if let Ok(resp) = self.client.get(&url).send().await {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let data = if body.get("success").is_some() {
                        body.get("data").cloned().unwrap_or(body.clone())
                    } else {
                        body
                    };

                    if data.get("is_complete").and_then(|v| v.as_bool()) == Some(true) {
                        return Ok(data);
                    }

                    stale_count += 1;

                    // Fallback: check task run status directly after many stale polls
                    if stale_count > 10 {
                        if let Ok(status) = self.get_task_run_status(task_run_id).await {
                            if matches!(
                                status.as_str(),
                                "completed" | "complete" | "failed" | "stopped"
                            ) {
                                let mut result = data;
                                result["is_complete"] =
                                    serde_json::Value::Bool(true);
                                return Ok(result);
                            }
                        }
                        stale_count = 0;
                    }
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Get the status field of a task run.
    async fn get_task_run_status(&self, task_run_id: &str) -> Result<String, String> {
        let url = format!("{}/task-runs/{}", self.base_url, task_run_id);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to get task run: {}", e))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse task run: {}", e))?;

        let data = if body.get("success").is_some() {
            body.get("data").cloned().unwrap_or(body.clone())
        } else {
            body
        };

        data.get("status")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "No status field".to_string())
    }

    /// Trigger reflection on a completed task run.
    /// Returns the reflection task_run_id, or None if already running.
    pub async fn trigger_reflection(
        &self,
        task_run_id: &str,
    ) -> Result<Option<String>, String> {
        let url = format!(
            "{}/reflection/trigger/{}",
            self.base_url, task_run_id
        );
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| format!("Failed to trigger reflection: {}", e))?;

        if resp.status().as_u16() == 409 {
            // Already running — find the existing reflection
            return self.find_reflection_for(task_run_id).await;
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse reflection response: {}", e))?;

        let data = if body.get("success").is_some() {
            body.get("data").cloned().unwrap_or(body.clone())
        } else {
            body
        };

        let id = data
            .get("task_run_id")
            .or_else(|| data.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(id)
    }

    /// Find an auto-triggered reflection for a source task run.
    async fn find_reflection_for(
        &self,
        source_task_run_id: &str,
    ) -> Result<Option<String>, String> {
        let url = format!("{}/task-runs", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to list task runs: {}", e))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse task runs: {}", e))?;

        let data = if body.get("success").is_some() {
            body.get("data").cloned().unwrap_or(body.clone())
        } else {
            body
        };

        if let Some(runs) = data.as_array() {
            for run in runs {
                let is_reflection = run
                    .get("is_reflection")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let source = run
                    .get("reflection_source_task_run_id")
                    .and_then(|v| v.as_str());

                if is_reflection && source == Some(source_task_run_id) {
                    return Ok(run.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()));
                }
            }
        }

        Ok(None)
    }

    /// Count new reflection fixes for a task run.
    pub async fn count_reflection_fixes(
        &self,
        reflection_task_run_id: &str,
    ) -> Result<u32, String> {
        let url = format!(
            "{}/task-runs/{}/reflection-fixes",
            self.base_url, reflection_task_run_id
        );

        if let Ok(resp) = self.client.get(&url).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                let data = if body.get("success").is_some() {
                    body.get("data").cloned().unwrap_or(body.clone())
                } else {
                    body
                };

                if let Some(arr) = data.as_array() {
                    return Ok(arr.len() as u32);
                }
            }
        }

        // Fallback: check output
        let url = format!(
            "{}/task-runs/{}/output?tail_chars=5000",
            self.base_url, reflection_task_run_id
        );
        if let Ok(resp) = self.client.get(&url).send().await {
            if let Ok(text) = resp.text().await {
                let count = text
                    .lines()
                    .filter(|l| {
                        let lower = l.to_lowercase();
                        lower.contains("fixed") || lower.contains("fix applied")
                    })
                    .count();
                return Ok(count as u32);
            }
        }

        Ok(0)
    }

    /// Generate a workflow from a description. Returns (workflow_id, task_run_id).
    pub async fn generate_workflow(
        &self,
        description: &str,
        context: Option<&str>,
        context_ids: Option<&[String]>,
    ) -> Result<(String, String), String> {
        let url = format!("{}/unified-workflows/generate-async", self.base_url);

        let mut body = serde_json::json!({
            "description": description,
            "max_fix_iterations": 3,
        });

        if let Some(ctx) = context {
            body["inline_context"] = serde_json::Value::String(ctx.to_string());
        }
        if let Some(ids) = context_ids {
            body["context_ids"] = serde_json::json!(ids);
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Failed to start workflow generation: {}", e))?;

        let resp_body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse generate response: {}", e))?;

        let data = if resp_body.get("success").is_some() {
            resp_body.get("data").cloned().unwrap_or(resp_body.clone())
        } else {
            resp_body
        };

        let task_run_id = data
            .get("task_run_id")
            .or_else(|| data.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("No task_run_id in generate response: {}", data))?;

        // Poll until the meta-workflow completes
        let stop_rx_dummy = tokio::sync::watch::channel(false).1;
        let result = self.poll_until_complete(&task_run_id, &stop_rx_dummy).await?;

        // Fetch result-data which contains the generated_workflow_id
        let result_url = format!(
            "{}/task-runs/{}/result-data",
            self.base_url, task_run_id
        );
        let result_resp = self
            .client
            .get(&result_url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch result-data: {}", e))?;

        let result_body: serde_json::Value = result_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse result-data: {}", e))?;

        let result_data = if result_body.get("success").is_some() {
            result_body.get("data").cloned().unwrap_or(result_body.clone())
        } else {
            result_body
        };

        let workflow_id = result_data
            .get("generated_workflow_id")
            .or_else(|| result_data.get("workflow_id"))
            .or_else(|| result.get("generated_workflow_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                format!(
                    "No generated_workflow_id in result-data: {}",
                    result_data
                )
            })?;

        Ok((workflow_id, task_run_id))
    }

    /// Get the full list of reflection fixes for a task run.
    pub async fn get_reflection_fixes(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let url = format!(
            "{}/task-runs/{}/reflection-fixes",
            self.base_url, task_run_id
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to get reflection fixes: {}", e))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse reflection fixes: {}", e))?;

        let data = if body.get("success").is_some() {
            body.get("data").cloned().unwrap_or(body.clone())
        } else {
            body
        };

        Ok(data.as_array().cloned().unwrap_or_default())
    }

    /// Wait for the fixer workflow associated with a source task run to complete.
    /// Returns Ok(true) if fixer completed, Ok(false) if timed out or no fixer found.
    pub async fn wait_for_fixer_complete(
        &self,
        source_task_run_id: &str,
        timeout_secs: u64,
    ) -> Result<bool, String> {
        let deadline = std::time::Duration::from_secs(timeout_secs);
        let poll = std::time::Duration::from_secs(5);
        let start = std::time::Instant::now();

        // Wait a bit for the fixer to be created
        tokio::time::sleep(std::time::Duration::from_secs(35)).await;

        loop {
            if start.elapsed() > deadline {
                return Ok(false);
            }

            // Query task runs to find a fixer for this source
            let url = format!(
                "{}/task-runs?parent_task_run_id={}&limit=50",
                self.base_url, source_task_run_id
            );
            if let Ok(resp) = self.client.get(&url).send().await {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let data = if body.get("success").is_some() {
                        body.get("data").cloned().unwrap_or(body.clone())
                    } else {
                        body
                    };

                    if let Some(runs) = data.as_array() {
                        for run in runs {
                            let is_fixer = run
                                .get("is_fixer")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            if !is_fixer {
                                continue;
                            }
                            let status = run
                                .get("status")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            if matches!(status, "completed" | "complete" | "failed" | "stopped") {
                                return Ok(true);
                            }
                            // Fixer exists but still running — keep waiting
                        }
                    }
                }
            }

            tokio::time::sleep(poll).await;
        }
    }

    /// Get the status of a task run (public version).
    pub async fn get_task_run_status_pub(&self, task_run_id: &str) -> Result<String, String> {
        self.get_task_run_status(task_run_id).await
    }

    /// Wait for the runner to become healthy.
    pub async fn wait_for_healthy(&self, timeout_secs: u64) -> bool {
        let deadline = Duration::from_secs(timeout_secs);
        let poll = Duration::from_secs(2);

        let result = tokio::time::timeout(deadline, async {
            loop {
                if self.is_healthy().await {
                    return true;
                }
                tokio::time::sleep(poll).await;
            }
        })
        .await;

        result.unwrap_or(false)
    }
}

impl SupervisorClient {
    pub fn new(port: u16) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            base_url: format!("http://127.0.0.1:{}", port),
        }
    }

    /// Restart a runner by ID.
    pub async fn restart_runner(
        &self,
        runner_id: &str,
        rebuild: bool,
    ) -> Result<(), String> {
        let url = format!("{}/runners/{}/restart", self.base_url, runner_id);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "rebuild": rebuild }))
            .send()
            .await
            .map_err(|e| format!("Failed to restart runner: {}", e))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Supervisor returned error: {}", body));
        }

        Ok(())
    }

    /// Stop a runner by ID.
    pub async fn stop_runner(&self, runner_id: &str) -> Result<(), String> {
        let url = format!("{}/runners/{}/stop", self.base_url, runner_id);
        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to stop runner: {}", e))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Supervisor returned error: {}", body));
        }

        Ok(())
    }

    /// Restart the runner using the legacy single-runner endpoint (backward compat).
    pub async fn restart_runner_legacy(&self, rebuild: bool) -> Result<(), String> {
        let url = format!("{}/runner/restart", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "rebuild": rebuild }))
            .send()
            .await
            .map_err(|e| format!("Failed to restart runner: {}", e))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Supervisor returned error: {}", body));
        }

        Ok(())
    }
}
