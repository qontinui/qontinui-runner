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
    /// The typed machine code a failure envelope carries, at the top level or
    /// (for a UI Bridge typed error) under `error_detail.code`.
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub error_detail: Option<serde_json::Value>,
}

impl RunnerClient {
    pub fn new(port: u16) -> Self {
        // The base URL is always the loopback runner, so an `HTTP_PROXY` in
        // the environment must not route these requests away from it.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .no_proxy()
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
    ///
    /// `Err` for anything that is not a success envelope carrying an object:
    /// a 503 "frontend not ready" envelope is an error here, never a report.
    pub async fn get_page_health(&self) -> Result<serde_json::Value, String> {
        self.ui_bridge_read(
            reqwest::Method::POST,
            "/ui-bridge/control/page-health",
            "page health",
        )
        .await
    }

    /// Get a DOM snapshot from UI Bridge. `Err` for anything that is not a
    /// success envelope carrying an object.
    pub async fn get_ui_snapshot(&self) -> Result<serde_json::Value, String> {
        self.ui_bridge_read(
            reqwest::Method::GET,
            "/ui-bridge/control/snapshot",
            "UI snapshot",
        )
        .await
    }

    /// One UI Bridge read, by the method its route is registered under
    /// (`/control/page-health` is POST, `/control/snapshot` is GET).
    async fn ui_bridge_read(
        &self,
        method: reqwest::Method,
        path: &str,
        what: &str,
    ) -> Result<serde_json::Value, String> {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.client.request(method.clone(), &url);
        if method == reqwest::Method::POST {
            request = request.json(&serde_json::json!({}));
        }
        let resp = request
            .send()
            .await
            .map_err(|e| format!("Failed to get {}: {}", what, e))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read {} (HTTP {}): {}", what, status, e))?;
        read_api_envelope(status, &text)
            .map_err(|refusal| format!("{} unavailable: {}", what, refusal.describe()))
    }

    /// Run assertions against the current UI state via UI Bridge.
    ///
    /// The contract is qontinui-schemas `DiagnosePhaseConfig::assertions`:
    /// "Each assertion is a JSON object passed to `POST /ui-bridge/control/assert`".
    /// That route takes ONE assertion per request, so each configured element
    /// is posted on its own and a malformed one fails alone (coord finding
    /// 0181f3d4).
    ///
    /// Returns one entry per assertion, in order. A verdict is the route's
    /// `AssertResult` (boolean `passed`). Anything else (a transport error, a
    /// refused request, a route that reached no verdict) is a NO-VERDICT entry
    /// `{"evaluated": false, "error", "status", "code", "assertion"}` with NO
    /// `passed` key, so no reader can count it as a failed check (plan
    /// 2026-11-12-structured-assert-arms-never-pass-and-diagnose-assertions-never-reach-the-route, D3).
    pub async fn run_assertions(&self, assertions: &[serde_json::Value]) -> Vec<serde_json::Value> {
        let url = format!("{}/ui-bridge/control/assert", self.base_url);
        let mut results = Vec::with_capacity(assertions.len());
        for assertion in assertions {
            results.push(self.run_one_assertion(&url, assertion).await);
        }
        results
    }

    async fn run_one_assertion(
        &self,
        url: &str,
        assertion: &serde_json::Value,
    ) -> serde_json::Value {
        let resp = match self.client.post(url).json(assertion).send().await {
            Ok(resp) => resp,
            Err(e) => {
                return no_verdict_entry(
                    assertion,
                    format!("Failed to run assertion: {}", e),
                    None,
                    None,
                )
            }
        };
        let status = resp.status().as_u16();
        let text = match resp.text().await {
            Ok(text) => text,
            Err(e) => {
                return no_verdict_entry(
                    assertion,
                    format!("Failed to read assertion response (HTTP {}): {}", status, e),
                    Some(status),
                    None,
                )
            }
        };
        assertion_result_from_response(assertion, status, &text)
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
        }
    }

    /// Restart a runner by ID.
    pub async fn restart_runner(&self, runner_id: &str, rebuild: bool) -> Result<(), String> {
        let url = format!("{}/runners/{}/restart", self.base_url, runner_id);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "rebuild": rebuild, "source": "workflow_loop" }))
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

/// Why a UI Bridge read produced no usable payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvelopeRefusal {
    /// The HTTP status the route answered.
    pub status: u16,
    /// The envelope's typed code (`code`, else `error_detail.code`).
    pub code: Option<String>,
    /// The envelope's `error`, else what the body was.
    pub error: String,
}

impl EnvelopeRefusal {
    pub(crate) fn describe(&self) -> String {
        match &self.code {
            Some(code) => format!("HTTP {} {}: {}", self.status, code, self.error),
            None => format!("HTTP {}: {}", self.status, self.error),
        }
    }
}

/// Read a runner `ApiResponse` envelope: `Ok(data)` only for a 2xx
/// `success: true` envelope whose `data` is an object. Every other answer (a
/// non-2xx, a failure envelope, an unparseable body) is a refusal naming the
/// status, the typed code and the message, never data.
pub(crate) fn read_api_envelope(
    status: u16,
    body: &str,
) -> Result<serde_json::Value, EnvelopeRefusal> {
    let Ok(envelope) = serde_json::from_str::<ApiResponse<serde_json::Value>>(body) else {
        return Err(EnvelopeRefusal {
            status,
            code: None,
            error: format!("not a runner envelope: {}", body.trim()),
        });
    };
    if (200..300).contains(&status) && envelope.success {
        if let Some(data) = envelope.data.filter(|d| d.is_object()) {
            return Ok(data);
        }
    }
    let code = envelope.code.or_else(|| {
        envelope
            .error_detail
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(|c| c.as_str())
            .map(str::to_owned)
    });
    let error = envelope.error.unwrap_or_else(|| {
        if envelope.success {
            "success envelope without an object `data`".to_string()
        } else {
            "failure envelope without an `error`".to_string()
        }
    });
    Err(EnvelopeRefusal {
        status,
        code,
        error,
    })
}

/// One assertion's entry for a request that produced no route verdict: NO
/// `passed` key, `evaluated: false`, and what the route said.
fn no_verdict_entry(
    assertion: &serde_json::Value,
    error: String,
    status: Option<u16>,
    code: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "evaluated": false,
        "error": error,
        "status": status,
        "code": code,
        "assertion": assertion,
    })
}

/// Read one `POST /ui-bridge/control/assert` response into that assertion's
/// entry: the route's `AssertResult` when it carries a boolean `passed`, else
/// a no-verdict entry naming the status, code and message the route sent.
fn assertion_result_from_response(
    assertion: &serde_json::Value,
    status: u16,
    body: &str,
) -> serde_json::Value {
    match read_api_envelope(status, body) {
        Ok(data) if data.get("passed").is_some_and(|p| p.is_boolean()) => data,
        Ok(data) => no_verdict_entry(
            assertion,
            format!("Assertion answer has no boolean `passed`: {}", data),
            Some(status),
            None,
        ),
        Err(refusal) => no_verdict_entry(
            assertion,
            format!("Assertion produced no verdict ({})", refusal.describe()),
            Some(refusal.status),
            refusal.code,
        ),
    }
}

#[cfg(test)]
mod run_assertions_tests {
    //! The diagnose client against a stub that answers as the real route does:
    //! it deserializes the route's OWN request type, builds the route's OWN
    //! script, maps the outcome through the route's OWN
    //! `structured_assert_response`, and sits behind the production envelope
    //! middleware. Coord finding 0181f3d4 (the batch body the route refused)
    //! and plan 2026-11-12-structured-assert-arms-never-pass-and-diagnose-assertions-never-reach-the-route
    //! (a refused or verdict-less assertion is not a failed check).

    use super::{assertion_result_from_response, read_api_envelope, RunnerClient};
    use crate::mcp::ui_bridge::structured_assert::{
        build_structured_assert_expression, refuse_request, structured_assert_response,
        EvalOutcome, StructuredAssertRequest,
    };
    use axum::{http::StatusCode, middleware, response::IntoResponse, routing::post, Json, Router};
    use serde_json::json;

    /// `#ok` passes, `#missing` fails, anything else is a page that returned
    /// `undefined` (no verdict).
    async fn stub_assert(Json(request): Json<StructuredAssertRequest>) -> axum::response::Response {
        if let Err(message) = build_structured_assert_expression(&request) {
            return refuse_request(message).into_response();
        }
        let page = match request.query.as_str() {
            "#ok" => json!({ "value": "{\"found\":1,\"passed\":true}", "type": "string" }),
            "#missing" => json!({ "value": "{\"found\":0,\"passed\":false}", "type": "string" }),
            _ => json!({ "type": "undefined" }),
        };
        structured_assert_response(&request, EvalOutcome::Ipc(Ok(page))).into_response()
    }

    async fn serve(app: Router) -> u16 {
        let app = app.layer(middleware::from_fn(
            crate::mcp::envelope::envelope_rewrite_middleware,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let port = listener.local_addr().expect("a bound address").port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("the stub serves");
        });
        port
    }

    async fn serve_stub_assert_route() -> u16 {
        serve(Router::new().route("/ui-bridge/control/assert", post(stub_assert))).await
    }

    #[tokio::test]
    async fn each_assertion_is_posted_as_its_own_request_the_route_accepts() {
        let port = serve_stub_assert_route().await;
        let assertions = vec![
            json!({ "query": "#ok", "assertion": "exists" }),
            json!({ "query": "#missing", "queryType": "css", "assertion": "exists" }),
            // Not a StructuredAssertRequest: the route refuses it.
            json!({ "type": "visible", "role": "tab" }),
        ];
        let results = RunnerClient::new(port).run_assertions(&assertions).await;

        assert_eq!(results.len(), 3, "one result per assertion: {results:?}");
        assert_eq!(results[0]["passed"], json!(true), "{results:?}");
        assert_eq!(results[0]["query"], json!("#ok"), "{results:?}");
        assert_eq!(results[1]["passed"], json!(false), "{results:?}");
        assert_eq!(results[1]["query"], json!("#missing"), "{results:?}");
        assert!(
            results[1].get("error").is_none(),
            "a failed check is not a refusal: {results:?}"
        );
        // The refused element is a no-verdict entry, not a failed check.
        assert!(
            results[2].get("passed").is_none(),
            "a refused assertion carries no `passed`: {results:?}"
        );
        assert_eq!(results[2]["evaluated"], json!(false), "{results:?}");
        assert_eq!(results[2]["status"], json!(422), "{results:?}");
        assert_eq!(
            results[2]["assertion"], assertions[2],
            "the refused request is echoed"
        );
    }

    /// T2: a request the route refuses is a no-verdict entry naming the
    /// rejected field, and the elements after it still run.
    #[tokio::test]
    async fn a_rejected_assertion_is_a_non_verdict_entry_and_the_rest_still_run() {
        let port = serve_stub_assert_route().await;
        let assertions = vec![
            json!({ "query": "#ok", "assertion": "hasText", "expected": "" }),
            json!({ "query": "#ok", "assertion": "exists" }),
        ];
        let results = RunnerClient::new(port).run_assertions(&assertions).await;
        assert_eq!(results.len(), 2, "{results:?}");
        assert!(results[0].get("passed").is_none(), "{results:?}");
        assert_eq!(results[0]["evaluated"], json!(false), "{results:?}");
        assert_eq!(results[0]["status"], json!(400), "{results:?}");
        let error = results[0]["error"].as_str().unwrap_or_default();
        assert!(error.contains("`expected`"), "names the field: {error}");
        assert!(results[0]["code"].is_string(), "a typed code: {results:?}");
        assert_eq!(results[1]["passed"], json!(true), "{results:?}");
    }

    /// T3: a route 500 (no verdict) is a no-verdict entry carrying its code.
    #[tokio::test]
    async fn a_route_500_is_a_non_verdict_entry_carrying_its_code() {
        let port = serve_stub_assert_route().await;
        let results = RunnerClient::new(port)
            .run_assertions(&[json!({ "query": "#broken", "assertion": "exists" })])
            .await;
        assert!(results[0].get("passed").is_none(), "{results:?}");
        assert_eq!(results[0]["evaluated"], json!(false), "{results:?}");
        assert_eq!(results[0]["status"], json!(500), "{results:?}");
        assert_eq!(results[0]["code"], json!("INTERNAL_ERROR"), "{results:?}");
    }

    /// T4: an unreachable runner is no verdict with no status.
    #[tokio::test]
    async fn an_unreachable_runner_is_a_non_verdict_entry_with_no_status() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let results = RunnerClient::new(port)
            .run_assertions(&[json!({ "query": "#ok", "assertion": "exists" })])
            .await;
        assert!(results[0].get("passed").is_none(), "{results:?}");
        assert_eq!(results[0]["evaluated"], json!(false), "{results:?}");
        assert_eq!(results[0]["status"], json!(null), "{results:?}");
    }

    /// T6: a page-health failure envelope is an error, never a report.
    #[tokio::test]
    async fn a_503_page_health_envelope_is_an_error() {
        let port = serve(Router::new().route(
            "/ui-bridge/control/page-health",
            post(|| async {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "success": false,
                        "error": "UI Bridge frontend did not become ready",
                        "code": "FRONTEND_NOT_READY",
                    })),
                )
            }),
        ))
        .await;
        let err = RunnerClient::new(port)
            .get_page_health()
            .await
            .expect_err("a 503 envelope is not a page-health report");
        assert!(err.contains("503"), "{err}");
        assert!(err.contains("FRONTEND_NOT_READY"), "{err}");
    }

    #[tokio::test]
    async fn a_page_health_report_in_the_producers_shape_is_read_as_data() {
        let port = serve(Router::new().route(
            "/ui-bridge/control/page-health",
            post(|| async {
                Json(json!({
                    "success": true,
                    "data": {
                        "summary": "OK",
                        "findings": [],
                        "heatmap": [],
                        "element_count": 3,
                        "visible_count": 3,
                    },
                }))
            }),
        ))
        .await;
        let report = RunnerClient::new(port)
            .get_page_health()
            .await
            .expect("a success envelope is a report");
        assert_eq!(report["summary"], json!("OK"));
    }

    #[test]
    fn a_failure_envelope_becomes_a_no_verdict_entry_naming_its_message() {
        let assertion = json!({ "query": "x", "assertion": "exists" });
        let result = assertion_result_from_response(
            &assertion,
            400,
            r#"{"success":false,"data":null,"error":"`query` must be non-empty"}"#,
        );
        assert!(result.get("passed").is_none(), "{result}");
        assert_eq!(result["evaluated"], json!(false));
        assert_eq!(result["status"], json!(400));
        assert_eq!(
            result["error"],
            json!("Assertion produced no verdict (HTTP 400: `query` must be non-empty)")
        );
    }

    #[test]
    fn a_success_envelope_yields_its_assert_result() {
        let assertion = json!({ "query": "x", "assertion": "exists" });
        let result = assertion_result_from_response(
            &assertion,
            200,
            r#"{"success":true,"data":{"passed":true,"query":"x"}}"#,
        );
        assert_eq!(result, json!({ "passed": true, "query": "x" }));
    }

    #[test]
    fn a_success_envelope_without_a_boolean_passed_is_no_verdict() {
        let assertion = json!({ "query": "x", "assertion": "exists" });
        let result = assertion_result_from_response(
            &assertion,
            200,
            r#"{"success":true,"data":{"query":"x"}}"#,
        );
        assert!(result.get("passed").is_none(), "{result}");
        assert_eq!(result["evaluated"], json!(false));
    }

    #[test]
    fn read_api_envelope_takes_a_typed_code_from_error_detail() {
        let refusal = read_api_envelope(
            500,
            r#"{"success":false,"error":"boom","error_detail":{"code":"INTERNAL_ERROR"}}"#,
        )
        .expect_err("a failure envelope");
        assert_eq!(refusal.code.as_deref(), Some("INTERNAL_ERROR"));
        assert_eq!(refusal.status, 500);
        assert!(read_api_envelope(200, "not json").is_err());
    }
}
