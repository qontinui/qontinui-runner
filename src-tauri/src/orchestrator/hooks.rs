//! Lifecycle Hooks Module
//!
//! Provides pre/post execution and error handling hooks for task orchestration.
//! Hooks can execute commands, call webhooks, log messages, or send notifications.
//!
//! Inspired by n8n's workflow hooks pattern.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, error, info, warn};

// ============================================================================
// Hook Triggers
// ============================================================================

/// Events that can trigger hook execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookTrigger {
    /// Before task execution starts
    PreExecution,
    /// After task execution completes (success or failure)
    PostExecution,
    /// When an error occurs during execution
    OnError,
    /// When verification fails
    OnVerificationFail,
    /// When task completes successfully
    OnComplete,
    /// Before each iteration
    PreIteration,
    /// After each iteration
    PostIteration,
}

impl HookTrigger {
    /// Get a human-readable name for this trigger.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::PreExecution => "Pre-Execution",
            Self::PostExecution => "Post-Execution",
            Self::OnError => "On Error",
            Self::OnVerificationFail => "On Verification Fail",
            Self::OnComplete => "On Complete",
            Self::PreIteration => "Pre-Iteration",
            Self::PostIteration => "Post-Iteration",
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "pre_execution" | "preexecution" => Some(Self::PreExecution),
            "post_execution" | "postexecution" => Some(Self::PostExecution),
            "on_error" | "onerror" => Some(Self::OnError),
            "on_verification_fail" | "onverificationfail" => Some(Self::OnVerificationFail),
            "on_complete" | "oncomplete" => Some(Self::OnComplete),
            "pre_iteration" | "preiteration" => Some(Self::PreIteration),
            "post_iteration" | "postiteration" => Some(Self::PostIteration),
            _ => None,
        }
    }
}

// ============================================================================
// Hook Actions
// ============================================================================

/// Actions that a hook can perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookAction {
    /// Execute a shell command
    Command {
        /// The command to execute
        command: String,
        /// Working directory (optional)
        working_dir: Option<String>,
        /// Timeout in seconds
        #[serde(default = "default_command_timeout")]
        timeout_seconds: u64,
        /// Environment variables to set
        #[serde(default)]
        env: HashMap<String, String>,
    },

    /// Call a webhook URL
    Webhook {
        /// The URL to call
        url: String,
        /// HTTP method (GET, POST, PUT, etc.)
        #[serde(default = "default_webhook_method")]
        method: String,
        /// Request headers
        #[serde(default)]
        headers: HashMap<String, String>,
        /// Request body (for POST/PUT)
        body: Option<String>,
        /// Timeout in seconds
        #[serde(default = "default_webhook_timeout")]
        timeout_seconds: u64,
    },

    /// Log a message
    Log {
        /// Log level (info, warn, error, debug)
        #[serde(default = "default_log_level")]
        level: String,
        /// Message template (supports {{variable}} substitution)
        message: String,
    },

    /// Send a system notification
    Notification {
        /// Notification title
        title: String,
        /// Notification body
        body: String,
    },

    /// Trigger another workflow to run
    RunWorkflow {
        /// Workflow ID to run
        workflow_id: String,
        /// Pass source workflow context as variables
        #[serde(default)]
        pass_context: bool,
        /// Optional overrides (max_iterations, model, etc.)
        #[serde(default)]
        override_config: Option<serde_json::Value>,
    },
}

fn default_command_timeout() -> u64 {
    30
}

fn default_webhook_method() -> String {
    "POST".to_string()
}

fn default_webhook_timeout() -> u64 {
    30
}

fn default_log_level() -> String {
    "info".to_string()
}

// ============================================================================
// Hook Definition
// ============================================================================

/// A lifecycle hook definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    /// Unique identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// When this hook should trigger
    pub trigger: HookTrigger,
    /// What action to perform
    pub action: HookAction,
    /// Whether the hook is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Execution order (lower = earlier)
    #[serde(default)]
    pub execution_order: i32,
    /// Continue on failure (if false, hook failure stops execution)
    #[serde(default = "default_continue_on_failure")]
    pub continue_on_failure: bool,
    /// Conditions for execution (all must be true)
    #[serde(default)]
    pub conditions: Vec<HookCondition>,
}

fn default_enabled() -> bool {
    true
}

fn default_continue_on_failure() -> bool {
    true
}

/// Condition for hook execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCondition {
    /// Variable name to check
    pub variable: String,
    /// Operator: eq, ne, gt, lt, gte, lte, contains, matches
    pub operator: String,
    /// Value to compare against
    pub value: serde_json::Value,
}

// ============================================================================
// Hook Context
// ============================================================================

/// Context passed to hooks during execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookContext {
    /// Task run ID
    pub task_run_id: String,
    /// Task name
    pub task_name: String,
    /// Current iteration (1-based)
    pub iteration: u32,
    /// Current status
    pub status: String,
    /// Error message (if any)
    pub error: Option<String>,
    /// Additional variables
    pub variables: HashMap<String, serde_json::Value>,
}

impl HookContext {
    /// Create a new hook context.
    pub fn new(task_run_id: &str, task_name: &str) -> Self {
        Self {
            task_run_id: task_run_id.to_string(),
            task_name: task_name.to_string(),
            ..Default::default()
        }
    }

    /// Set the iteration number.
    pub fn with_iteration(mut self, iteration: u32) -> Self {
        self.iteration = iteration;
        self
    }

    /// Set the status.
    pub fn with_status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    /// Set the error message.
    pub fn with_error(mut self, error: &str) -> Self {
        self.error = Some(error.to_string());
        self
    }

    /// Add a variable.
    pub fn with_variable(mut self, name: &str, value: serde_json::Value) -> Self {
        self.variables.insert(name.to_string(), value);
        self
    }

    /// Get a variable value as string for substitution.
    fn get_value(&self, name: &str) -> Option<String> {
        match name {
            "task_run_id" => Some(self.task_run_id.clone()),
            "task_name" => Some(self.task_name.clone()),
            "iteration" => Some(self.iteration.to_string()),
            "status" => Some(self.status.clone()),
            "error" => self.error.clone(),
            _ => self.variables.get(name).map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                _ => v.to_string(),
            }),
        }
    }

    /// Substitute {{variable}} patterns in a string.
    pub fn substitute(&self, template: &str) -> String {
        let mut result = template.to_string();
        let pattern = regex::Regex::new(r"\{\{([^}]+)\}\}").unwrap();

        for cap in pattern.captures_iter(template) {
            let full_match = cap.get(0).unwrap().as_str();
            let var_name = cap.get(1).unwrap().as_str().trim();

            if let Some(value) = self.get_value(var_name) {
                result = result.replace(full_match, &value);
            }
        }

        result
    }
}

// ============================================================================
// Hook Result
// ============================================================================

/// Result of executing a hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookResult {
    /// Hook ID
    pub hook_id: String,
    /// Hook name
    pub hook_name: String,
    /// Whether execution succeeded
    pub success: bool,
    /// Output (command stdout, webhook response, etc.)
    pub output: Option<String>,
    /// Error message if failed
    pub error: Option<String>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

// ============================================================================
// Hook Executor
// ============================================================================

/// Executes hooks based on triggers.
pub struct HookExecutor {
    hooks: Vec<Hook>,
}

impl HookExecutor {
    /// Create a new hook executor with the given hooks.
    pub fn new(hooks: Vec<Hook>) -> Self {
        Self { hooks }
    }

    /// Create an empty hook executor.
    pub fn empty() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Add a hook.
    pub fn add_hook(&mut self, hook: Hook) {
        self.hooks.push(hook);
    }

    /// Get hooks for a specific trigger, sorted by execution order.
    pub fn hooks_for_trigger(&self, trigger: HookTrigger) -> Vec<&Hook> {
        let mut hooks: Vec<_> = self
            .hooks
            .iter()
            .filter(|h| h.enabled && h.trigger == trigger)
            .collect();
        hooks.sort_by_key(|h| h.execution_order);
        hooks
    }

    /// Execute all hooks for a trigger.
    ///
    /// Returns a list of results for each hook executed.
    /// If a hook fails and `continue_on_failure` is false, stops and returns.
    pub fn execute_trigger(&self, trigger: HookTrigger, context: &HookContext) -> Vec<HookResult> {
        let hooks = self.hooks_for_trigger(trigger);
        let mut results = Vec::new();

        info!("Executing {} hooks for trigger {:?}", hooks.len(), trigger);

        for hook in hooks {
            // Check conditions
            if !self.check_conditions(&hook.conditions, context) {
                debug!("Skipping hook {} - conditions not met", hook.name);
                continue;
            }

            let start = std::time::Instant::now();
            let result = self.execute_hook(hook, context);
            let duration_ms = start.elapsed().as_millis() as u64;

            let hook_result = HookResult {
                hook_id: hook.id.clone(),
                hook_name: hook.name.clone(),
                success: result.is_ok(),
                output: result.as_ref().ok().cloned(),
                error: result.as_ref().err().map(|e| e.to_string()),
                duration_ms,
            };

            if !hook_result.success {
                warn!("Hook {} failed: {:?}", hook.name, hook_result.error);

                if !hook.continue_on_failure {
                    error!(
                        "Hook {} failed and continue_on_failure is false, stopping",
                        hook.name
                    );
                    results.push(hook_result);
                    break;
                }
            } else {
                debug!("Hook {} completed successfully", hook.name);
            }

            results.push(hook_result);
        }

        results
    }

    /// Execute a single hook.
    fn execute_hook(&self, hook: &Hook, context: &HookContext) -> Result<String, String> {
        match &hook.action {
            HookAction::Command {
                command,
                working_dir,
                timeout_seconds: _,
                env,
            } => self.execute_command(command, working_dir.as_deref(), env, context),
            HookAction::Webhook {
                url,
                method,
                headers,
                body,
                timeout_seconds,
            } => self.execute_webhook(
                url,
                method,
                headers,
                body.as_deref(),
                *timeout_seconds,
                context,
            ),
            HookAction::Log { level, message } => {
                self.execute_log(level, message, context);
                Ok("logged".to_string())
            }
            HookAction::Notification { title, body } => {
                self.execute_notification(title, body, context);
                Ok("notified".to_string())
            }
            HookAction::RunWorkflow {
                workflow_id,
                pass_context,
                override_config,
            } => {
                // Send a workflow chain event to the trigger service.
                // The actual execution happens asynchronously via the trigger system.
                let wf_id = context.substitute(workflow_id);
                info!("Hook triggering workflow: {}", wf_id);

                let mut variables = std::collections::HashMap::new();
                variables.insert(
                    "source_task_run_id".to_string(),
                    context.task_run_id.clone(),
                );
                variables.insert("source_task_name".to_string(), context.task_name.clone());
                variables.insert("source_status".to_string(), context.status.clone());

                if *pass_context {
                    if let Some(ref error) = context.error {
                        variables.insert("source_error".to_string(), error.clone());
                    }
                    // Include all context variables
                    for (k, v) in &context.variables {
                        let str_val = match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        variables.insert(format!("source_{}", k), str_val);
                    }
                }

                // We can't call async from here, so just log the intent.
                // The actual RunWorkflow execution is wired through the trigger system
                // at the executor level (where we have async context).
                Ok(format!(
                    "run_workflow:{}",
                    serde_json::json!({
                        "workflow_id": wf_id,
                        "pass_context": pass_context,
                        "override_config": override_config,
                        "variables": variables,
                    })
                ))
            }
        }
    }

    /// Execute a command action.
    fn execute_command(
        &self,
        command: &str,
        working_dir: Option<&str>,
        env: &HashMap<String, String>,
        context: &HookContext,
    ) -> Result<String, String> {
        let command = context.substitute(command);
        info!("Executing hook command: {}", command);

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = crate::process_helpers::cmd_no_window();
            c.args(["/C", &command]);
            c
        } else {
            let mut c = crate::process_helpers::no_window("sh");
            c.args(["-c", &command]);
            c
        };

        if let Some(dir) = working_dir {
            cmd.current_dir(context.substitute(dir));
        }

        for (key, value) in env {
            cmd.env(key, context.substitute(value));
        }

        // Add context as environment variables
        cmd.env("QONTINUI_TASK_RUN_ID", &context.task_run_id);
        cmd.env("QONTINUI_TASK_NAME", &context.task_name);
        cmd.env("QONTINUI_ITERATION", context.iteration.to_string());
        cmd.env("QONTINUI_STATUS", &context.status);
        if let Some(ref error) = context.error {
            cmd.env("QONTINUI_ERROR", error);
        }

        let output = cmd
            .output()
            .map_err(|e| format!("Failed to execute command: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            Err(format!("Command failed: {}", stderr))
        }
    }

    /// Execute a webhook action.
    fn execute_webhook(
        &self,
        url: &str,
        method: &str,
        headers: &HashMap<String, String>,
        body: Option<&str>,
        timeout_seconds: u64,
        context: &HookContext,
    ) -> Result<String, String> {
        let url = context.substitute(url);
        info!("Executing hook webhook: {} {}", method, url);

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        let mut request = match method.to_uppercase().as_str() {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "DELETE" => client.delete(&url),
            "PATCH" => client.patch(&url),
            _ => return Err(format!("Unsupported HTTP method: {}", method)),
        };

        for (key, value) in headers {
            request = request.header(key, context.substitute(value));
        }

        if let Some(body) = body {
            let body = context.substitute(body);
            request = request.body(body);
        }

        let response = request
            .send()
            .map_err(|e| format!("Webhook request failed: {}", e))?;

        let status = response.status();
        let body = response
            .text()
            .map_err(|e| format!("Failed to read response: {}", e))?;

        if status.is_success() {
            Ok(body)
        } else {
            Err(format!("Webhook returned {}: {}", status, body))
        }
    }

    /// Execute a log action.
    fn execute_log(&self, level: &str, message: &str, context: &HookContext) {
        let message = context.substitute(message);

        match level.to_lowercase().as_str() {
            "error" => error!("[Hook] {}", message),
            "warn" | "warning" => warn!("[Hook] {}", message),
            "debug" => debug!("[Hook] {}", message),
            _ => info!("[Hook] {}", message),
        }
    }

    /// Execute a notification action.
    fn execute_notification(&self, title: &str, body: &str, context: &HookContext) {
        let title = context.substitute(title);
        let body = context.substitute(body);

        // For now, just log - actual notification implementation depends on platform
        info!("Notification: {} - {}", title, body);

        // TODO: Implement actual system notification
        // Could use notify-rust on Linux, winrt on Windows, etc.
    }

    /// Check if all conditions are met.
    fn check_conditions(&self, conditions: &[HookCondition], context: &HookContext) -> bool {
        for condition in conditions {
            if !self.check_condition(condition, context) {
                return false;
            }
        }
        true
    }

    /// Check a single condition.
    fn check_condition(&self, condition: &HookCondition, context: &HookContext) -> bool {
        let actual = match context.get_value(&condition.variable) {
            Some(v) => v,
            None => return false,
        };

        let expected = match &condition.value {
            serde_json::Value::String(s) => s.clone(),
            v => v.to_string(),
        };

        match condition.operator.to_lowercase().as_str() {
            "eq" | "==" | "equals" => actual == expected,
            "ne" | "!=" | "not_equals" => actual != expected,
            "contains" => actual.contains(&expected),
            "starts_with" => actual.starts_with(&expected),
            "ends_with" => actual.ends_with(&expected),
            "matches" => {
                if let Ok(re) = regex::Regex::new(&expected) {
                    re.is_match(&actual)
                } else {
                    false
                }
            }
            "gt" | ">" => {
                if let (Ok(a), Ok(e)) = (actual.parse::<f64>(), expected.parse::<f64>()) {
                    a > e
                } else {
                    actual > expected
                }
            }
            "lt" | "<" => {
                if let (Ok(a), Ok(e)) = (actual.parse::<f64>(), expected.parse::<f64>()) {
                    a < e
                } else {
                    actual < expected
                }
            }
            "gte" | ">=" => {
                if let (Ok(a), Ok(e)) = (actual.parse::<f64>(), expected.parse::<f64>()) {
                    a >= e
                } else {
                    actual >= expected
                }
            }
            "lte" | "<=" => {
                if let (Ok(a), Ok(e)) = (actual.parse::<f64>(), expected.parse::<f64>()) {
                    a <= e
                } else {
                    actual <= expected
                }
            }
            _ => {
                warn!("Unknown condition operator: {}", condition.operator);
                false
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_context_substitution() {
        let context = HookContext::new("run-123", "Test Task")
            .with_iteration(5)
            .with_status("running")
            .with_variable("custom", serde_json::json!("value"));

        let result = context.substitute("Task {{task_name}} iteration {{iteration}}: {{custom}}");
        assert_eq!(result, "Task Test Task iteration 5: value");
    }

    #[test]
    fn test_condition_evaluation() {
        let executor = HookExecutor::empty();
        let context = HookContext::new("run-123", "Test Task")
            .with_iteration(5)
            .with_status("running");

        // Test equals
        let condition = HookCondition {
            variable: "status".to_string(),
            operator: "eq".to_string(),
            value: serde_json::json!("running"),
        };
        assert!(executor.check_condition(&condition, &context));

        // Test not equals
        let condition = HookCondition {
            variable: "status".to_string(),
            operator: "ne".to_string(),
            value: serde_json::json!("stopped"),
        };
        assert!(executor.check_condition(&condition, &context));

        // Test greater than
        let condition = HookCondition {
            variable: "iteration".to_string(),
            operator: "gt".to_string(),
            value: serde_json::json!("3"),
        };
        assert!(executor.check_condition(&condition, &context));
    }

    #[test]
    fn test_hooks_for_trigger() {
        let hooks = vec![
            Hook {
                id: "h1".to_string(),
                name: "Hook 1".to_string(),
                trigger: HookTrigger::PreExecution,
                action: HookAction::Log {
                    level: "info".to_string(),
                    message: "Test".to_string(),
                },
                enabled: true,
                execution_order: 2,
                continue_on_failure: true,
                conditions: vec![],
            },
            Hook {
                id: "h2".to_string(),
                name: "Hook 2".to_string(),
                trigger: HookTrigger::PreExecution,
                action: HookAction::Log {
                    level: "info".to_string(),
                    message: "Test".to_string(),
                },
                enabled: true,
                execution_order: 1,
                continue_on_failure: true,
                conditions: vec![],
            },
            Hook {
                id: "h3".to_string(),
                name: "Hook 3".to_string(),
                trigger: HookTrigger::OnError,
                action: HookAction::Log {
                    level: "error".to_string(),
                    message: "Error".to_string(),
                },
                enabled: true,
                execution_order: 0,
                continue_on_failure: true,
                conditions: vec![],
            },
        ];

        let executor = HookExecutor::new(hooks);
        let pre_hooks = executor.hooks_for_trigger(HookTrigger::PreExecution);

        assert_eq!(pre_hooks.len(), 2);
        assert_eq!(pre_hooks[0].name, "Hook 2"); // execution_order 1
        assert_eq!(pre_hooks[1].name, "Hook 1"); // execution_order 2
    }

    #[test]
    fn test_trigger_display_names() {
        assert_eq!(HookTrigger::PreExecution.display_name(), "Pre-Execution");
        assert_eq!(HookTrigger::OnError.display_name(), "On Error");
        assert_eq!(HookTrigger::OnComplete.display_name(), "On Complete");
    }
}
