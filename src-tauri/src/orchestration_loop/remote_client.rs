//! HTTP clients for communicating with target runners and the supervisor.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

/// Client for interacting with a target runner's HTTP API.
///
/// It also remembers every task run it STARTED (or discovered as a child of
/// one it started) on the target — the loop's own runs. The between-iterations
/// restart gate uses that set to tell the loop's own AI-plane sessions from
/// anyone else's (see `restart_path::ReadinessSummary::foreign`).
pub struct RunnerClient {
    client: reqwest::Client,
    base_url: String,
    launched: Arc<StdMutex<BTreeSet<String>>>,
}

/// Client for interacting with the supervisor's HTTP API.
pub struct SupervisorClient {
    client: reqwest::Client,
    base_url: String,
    /// How often a detached rebuild-restart's status is polled.
    poll_interval: Duration,
}

/// How long a detached supervisor rebuild-restart may run before the loop
/// gives up on it.
pub const DETACHED_REBUILD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Default poll interval for a detached rebuild-restart.
const DETACHED_REBUILD_POLL: Duration = Duration::from_secs(5);

/// Why a supervisor restart did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorRestartError {
    /// The request itself failed or was refused.
    Request(String),
    /// The detached rebuild-restart reached a terminal FAILED state.
    BuildFailed {
        submission_id: String,
        error: String,
    },
    /// The detached rebuild-restart had not finished within the bound.
    TimedOut {
        submission_id: String,
        waited_secs: u64,
    },
    /// The loop was stopped while waiting.
    Stopped,
}

impl std::fmt::Display for SupervisorRestartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(e) => write!(f, "supervisor restart request failed: {e}"),
            Self::BuildFailed {
                submission_id,
                error,
            } => write!(
                f,
                "supervisor rebuild-restart {submission_id} failed: {error}"
            ),
            Self::TimedOut {
                submission_id,
                waited_secs,
            } => write!(
                f,
                "supervisor rebuild-restart {submission_id} did not finish within {waited_secs}s"
            ),
            Self::Stopped => write!(f, "Loop stopped"),
        }
    }
}

impl std::error::Error for SupervisorRestartError {}

/// The state of a detached supervisor submission, read from
/// `GET /build/{id}/status` (qontinui-supervisor `build_submissions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachedBuildState {
    /// Queued or running.
    Pending,
    /// Built and restarted.
    Succeeded,
    /// Terminal failure, with the supervisor's error.
    Failed(String),
}

/// Parse a `GET /build/{id}/status` body. The submission's `status.state` is
/// `queued` | `running` | `succeeded` | `failed`; a detached action also
/// carries `detached: {http_status, body}` once terminal, and a detached
/// action whose inner restart failed reports that as a non-2xx `http_status`.
pub fn parse_detached_build_status(body: &serde_json::Value) -> DetachedBuildState {
    let state = body
        .pointer("/status/state")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let detached_status = body
        .pointer("/detached/http_status")
        .and_then(|v| v.as_u64());
    let error = || {
        body.pointer("/detached/body/error")
            .or_else(|| body.pointer("/status/error"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("submission ended {state:?} with no error text"))
    };
    match state {
        "failed" => DetachedBuildState::Failed(error()),
        "succeeded" => match detached_status {
            Some(code) if (200..300).contains(&code) => DetachedBuildState::Succeeded,
            Some(_) => DetachedBuildState::Failed(error()),
            // Terminal but the outcome body has not been attached yet.
            None => DetachedBuildState::Pending,
        },
        _ => DetachedBuildState::Pending,
    }
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
            launched: Arc::new(StdMutex::new(BTreeSet::new())),
        }
    }

    /// Remember a task run this loop started (or a child of one it started).
    fn record_launched(&self, task_run_id: &str) {
        if let Ok(mut set) = self.launched.lock() {
            set.insert(task_run_id.to_string());
        }
    }

    /// Every task run this client started on the target, or discovered as a
    /// reflection / fixer of one it started.
    pub fn launched_run_ids(&self) -> BTreeSet<String> {
        self.launched.lock().map(|s| s.clone()).unwrap_or_default()
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
        self.start_workflow_with_overrides(workflow_id, &serde_json::json!({}))
            .await
    }

    /// Start a workflow with config overrides and return its task_run_id.
    /// Overrides are applied to the LoopConfig before execution (e.g. model, max_iterations).
    pub async fn start_workflow_with_overrides(
        &self,
        workflow_id: &str,
        overrides: &serde_json::Value,
    ) -> Result<String, String> {
        let url = format!("{}/unified-workflows/{}/run", self.base_url, workflow_id);

        let mut body = serde_json::json!({
            "force_fresh_start": true,
        });
        if let Some(obj) = overrides.as_object() {
            if !obj.is_empty() {
                body["overrides"] = overrides.clone();
            }
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Failed to start workflow: {}", e))?;

        let resp_body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse start response: {}", e))?;

        // Handle ApiResponse wrapper
        let data = if resp_body.get("success").is_some() {
            resp_body.get("data").cloned().unwrap_or(resp_body.clone())
        } else {
            resp_body
        };

        let id = data
            .get("task_run_id")
            .or_else(|| data.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("No task_run_id in response: {}", data))?;
        self.record_launched(&id);
        Ok(id)
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
            let url = format!("{}/task-runs/{}/workflow-state", self.base_url, task_run_id);
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
                                result["is_complete"] = serde_json::Value::Bool(true);
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
    pub async fn trigger_reflection(&self, task_run_id: &str) -> Result<Option<String>, String> {
        let url = format!("{}/reflection/trigger/{}", self.base_url, task_run_id);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| format!("Failed to trigger reflection: {}", e))?;

        if resp.status().as_u16() == 409 {
            // Already running — find the existing reflection
            let found = self.find_reflection_for(task_run_id).await?;
            if let Some(id) = &found {
                self.record_launched(id);
            }
            return Ok(found);
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
        if let Some(id) = &id {
            self.record_launched(id);
        }

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
                    return Ok(run
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()));
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
        self.record_launched(&task_run_id);

        // Poll until the meta-workflow completes
        let stop_rx_dummy = tokio::sync::watch::channel(false).1;
        let result = self
            .poll_until_complete(&task_run_id, &stop_rx_dummy)
            .await?;

        // Fetch result-data which contains the generated_workflow_id
        let result_url = format!("{}/task-runs/{}/result-data", self.base_url, task_run_id);
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
            result_body
                .get("data")
                .cloned()
                .unwrap_or(result_body.clone())
        } else {
            result_body
        };

        let workflow_id = result_data
            .get("generated_workflow_id")
            .or_else(|| result_data.get("workflow_id"))
            .or_else(|| result.get("generated_workflow_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("No generated_workflow_id in result-data: {}", result_data))?;

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
    /// Returns Ok(true) if fixer completed, Ok(false) if timed out or no fixer found,
    /// or Err("Loop stopped") if the stop signal is received.
    pub async fn wait_for_fixer_complete(
        &self,
        source_task_run_id: &str,
        timeout_secs: u64,
        stop_rx: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<bool, String> {
        let deadline = std::time::Duration::from_secs(timeout_secs);
        let poll = std::time::Duration::from_secs(5);
        let start = std::time::Instant::now();

        // Short initial delay (5s) to let the fixer be created, but abort on stop signal.
        let mut stop_rx_clone = stop_rx.clone();
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            _ = stop_rx_clone.changed() => {
                if *stop_rx.borrow() {
                    return Err("Loop stopped".to_string());
                }
            }
        }

        loop {
            if *stop_rx.borrow() {
                return Err("Loop stopped".to_string());
            }

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
                            // A fixer spawned for one of this loop's runs is
                            // the loop's own work.
                            if let Some(id) = run.get("id").and_then(|v| v.as_str()) {
                                self.record_launched(id);
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

            let mut stop_rx_clone2 = stop_rx.clone();
            tokio::select! {
                _ = tokio::time::sleep(poll) => {}
                _ = stop_rx_clone2.changed() => {
                    if *stop_rx.borrow() {
                        return Err("Loop stopped".to_string());
                    }
                }
            }
        }
    }

    /// Get the status of a task run (public version).
    pub async fn get_task_run_status_pub(&self, task_run_id: &str) -> Result<String, String> {
        self.get_task_run_status(task_run_id).await
    }

    /// Get page health diagnostic from UI Bridge.
    pub async fn get_page_health(&self) -> Result<serde_json::Value, String> {
        let url = format!("{}/ui-bridge/control/page-health", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| format!("Failed to get page health: {}", e))?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse page health: {}", e))?;
        let data = if body.get("success").is_some() {
            body.get("data").cloned().unwrap_or(body.clone())
        } else {
            body
        };
        Ok(data)
    }

    /// Get a DOM snapshot from UI Bridge.
    pub async fn get_ui_snapshot(&self) -> Result<serde_json::Value, String> {
        let url = format!("{}/ui-bridge/control/snapshot", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| format!("Failed to get UI snapshot: {}", e))?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse UI snapshot: {}", e))?;
        let data = if body.get("success").is_some() {
            body.get("data").cloned().unwrap_or(body.clone())
        } else {
            body
        };
        Ok(data)
    }

    /// Run assertions against the current UI state via UI Bridge.
    pub async fn run_assertions(
        &self,
        assertions: &[serde_json::Value],
    ) -> Result<Vec<serde_json::Value>, String> {
        let url = format!("{}/ui-bridge/control/assert", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "assertions": assertions }))
            .send()
            .await
            .map_err(|e| format!("Failed to run assertions: {}", e))?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse assertion response: {}", e))?;
        let data = if body.get("success").is_some() {
            body.get("data").cloned().unwrap_or(body.clone())
        } else {
            body
        };
        Ok(data.as_array().cloned().unwrap_or_else(|| vec![data]))
    }

    /// Wait for the runner to become healthy.
    /// Returns false if the timeout elapses or the stop signal is received.
    pub async fn wait_for_healthy(
        &self,
        timeout_secs: u64,
        stop_rx: &tokio::sync::watch::Receiver<bool>,
    ) -> bool {
        let deadline = Duration::from_secs(timeout_secs);
        let poll = Duration::from_secs(2);
        let start = std::time::Instant::now();

        loop {
            if *stop_rx.borrow() {
                return false;
            }

            if start.elapsed() > deadline {
                return false;
            }

            if self.is_healthy().await {
                return true;
            }

            let mut stop_rx_clone = stop_rx.clone();
            tokio::select! {
                _ = tokio::time::sleep(poll) => {}
                _ = stop_rx_clone.changed() => {
                    if *stop_rx.borrow() {
                        return false;
                    }
                }
            }
        }
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
            poll_interval: DETACHED_REBUILD_POLL,
        }
    }

    /// Override the detached-rebuild poll interval (tests).
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    /// Restart a runner by ID and return only once the restart is DONE.
    ///
    /// A `rebuild: true` restart is answered `202 Accepted` with a
    /// `submission_id`: the supervisor builds and restarts DETACHED from the
    /// request. A 202 is therefore not completion — this polls
    /// `GET /build/{id}/status` until the submission is terminal (bounded by
    /// `timeout`, abandoned on `stop_rx`), and a failed build is a typed
    /// [`SupervisorRestartError::BuildFailed`].
    pub async fn restart_runner(
        &self,
        runner_id: &str,
        rebuild: bool,
        timeout: Duration,
        stop_rx: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), SupervisorRestartError> {
        let url = format!("{}/runners/{}/restart", self.base_url, runner_id);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "rebuild": rebuild, "source": "workflow_loop" }))
            .send()
            .await
            .map_err(|e| {
                SupervisorRestartError::Request(format!("Failed to restart runner: {e}"))
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SupervisorRestartError::Request(format!(
                "Supervisor returned HTTP {status}: {body}"
            )));
        }
        if status.as_u16() != 202 {
            return Ok(());
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            SupervisorRestartError::Request(format!(
                "unparseable 202 body from the supervisor: {e}"
            ))
        })?;
        let submission_id = body
            .get("submission_id")
            .or_else(|| body.get("build_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                SupervisorRestartError::Request(format!(
                    "the supervisor accepted the rebuild but named no submission id: {body}"
                ))
            })?;
        self.wait_for_detached(&submission_id, timeout, stop_rx)
            .await
    }

    /// Poll a detached submission until it is terminal.
    async fn wait_for_detached(
        &self,
        submission_id: &str,
        timeout: Duration,
        stop_rx: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), SupervisorRestartError> {
        let start = std::time::Instant::now();
        let url = format!("{}/build/{}/status", self.base_url, submission_id);
        loop {
            if *stop_rx.borrow() {
                return Err(SupervisorRestartError::Stopped);
            }
            if start.elapsed() > timeout {
                return Err(SupervisorRestartError::TimedOut {
                    submission_id: submission_id.to_string(),
                    waited_secs: timeout.as_secs(),
                });
            }
            // A transient poll failure (the supervisor briefly busy) is not a
            // verdict; only a terminal status decides.
            if let Ok(resp) = self.client.get(&url).send().await {
                if resp.status().is_success() {
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        match parse_detached_build_status(&body) {
                            DetachedBuildState::Succeeded => return Ok(()),
                            DetachedBuildState::Failed(error) => {
                                return Err(SupervisorRestartError::BuildFailed {
                                    submission_id: submission_id.to_string(),
                                    error,
                                })
                            }
                            DetachedBuildState::Pending => {}
                        }
                    }
                } else if resp.status().as_u16() == 404 {
                    return Err(SupervisorRestartError::BuildFailed {
                        submission_id: submission_id.to_string(),
                        error: "the supervisor no longer knows this submission (restarted?)"
                            .to_string(),
                    });
                }
            }
            let mut rx = stop_rx.clone();
            tokio::select! {
                _ = tokio::time::sleep(self.poll_interval) => {}
                _ = rx.changed() => {}
            }
        }
    }
}
