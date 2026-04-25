//! Wrapper Action Step Handler (Phase 3 of the wrapper-runner integration plan).
//!
//! Lets a workflow step dispatch a typed action through an installed wrapper
//! and (optionally) write the wrapper's result into the workflow's shared
//! variable namespace so subsequent steps can reference it via
//! `{{ variable_name }}` templates.
//!
//! # Step config
//!
//! Carried on `ExecutionStepConfig` via:
//!
//! - `wrapper_id` (alias `wrapperId`) — required.
//! - `wrapper_action_id` (alias `actionId`) — required.
//! - `wrapper_params` (alias `params`) — optional JSON object. String-typed
//!    leaf values may contain `{{ var }}` template references; they're
//!    resolved against `RuntimeContext` (and `SharedVariableStore`) before
//!    dispatch.
//! - `wrapper_result_variable` (alias `resultVariable`) — optional. When
//!    non-empty, the dispatch result is written to
//!    [`SharedVariableStore`][cropro] under that name as a JSON string so
//!    later steps can read it via the existing `{{ name }}` syntax.
//!
//! [cropro]: crate::orchestrator::context_propagation::SharedVariableStore
//!
//! # Dispatch transport
//!
//! The handler talks to the runner's own Phase 1 HTTP route
//! `POST /wrappers/:id/dispatch`. Going through HTTP keeps this handler
//! independent of `WrapperState` plumbing on `AppState` (which is itself
//! still landing as part of Phase 1) and matches the runner's existing
//! self-call pattern used by `ui_bridge`.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value};
use std::sync::atomic::Ordering;
use tracing::{debug, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::orchestrator::context_propagation::ExpressionEvaluator;
use crate::step_executor::ExecutionStepConfig;

/// Default per-dispatch timeout (mirrors `wrappers::dispatch::DEFAULT_DISPATCH_TIMEOUT`).
const DEFAULT_DISPATCH_TIMEOUT_SECS: u64 = 60;

/// Step handler for `"wrapper_action"` steps.
pub struct WrapperActionHandler;

#[async_trait]
impl StepHandler for WrapperActionHandler {
    fn step_type(&self) -> &'static str {
        "wrapper_action"
    }

    fn display_name(&self) -> &'static str {
        "Wrapper Action"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let wrapper_id = match step.wrapper_id.as_deref() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return StepHandlerResult::failure(
                    "wrapper_action: missing required field 'wrapperId'",
                )
            }
        };
        let action_id = match step.wrapper_action_id.as_deref() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return StepHandlerResult::failure(
                    "wrapper_action: missing required field 'actionId'",
                )
            }
        };

        // Resolve `{{ var }}` templates in the params payload before
        // dispatch. We pull from the runtime context first (the canonical
        // workflow variable namespace) and fall back to the shared variable
        // store (which is the namespace this handler writes results into).
        let raw_params = step
            .wrapper_params
            .clone()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let resolved_params = resolve_templates(&raw_params, context);

        // The runner's own HTTP API surface — bound by `mcp_api::start_server`
        // and recorded on `AppState::api_port`. If the API hasn't bound yet
        // (the executor was invoked before the server came up), surface a
        // clear failure rather than a confusing connection refused.
        let api_port = context.app_state.api_port.load(Ordering::Relaxed);
        if api_port == 0 {
            return StepHandlerResult::failure(
                "wrapper_action: runner HTTP API has not bound yet (api_port=0); \
                 cannot dispatch to wrapper",
            );
        }

        let url = format!(
            "http://127.0.0.1:{}/wrappers/{}/dispatch",
            api_port, wrapper_id
        );
        let body = serde_json::json!({
            "action": action_id,
            "params": resolved_params,
        });

        let timeout_secs = step
            .timeout_seconds
            .unwrap_or(DEFAULT_DISPATCH_TIMEOUT_SECS);
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1)))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "wrapper_action: failed to construct HTTP client: {}",
                    e
                ));
            }
        };

        debug!(
            "wrapper_action: dispatching {}.{} via {}",
            wrapper_id, action_id, url
        );

        let response = match client.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "wrapper_action: dispatch to '{}' failed: {}",
                    wrapper_id, e
                ));
            }
        };

        let status = response.status();
        let body_value: Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "wrapper_action: wrapper '{}' returned non-JSON body: {}",
                    wrapper_id, e
                ));
            }
        };

        if !status.is_success() {
            // The dispatch route returns the runner's standard ApiResponse
            // envelope; surface the inner `error` field when present.
            let error_msg = body_value
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| body_value.to_string());
            return StepHandlerResult::failure(format!(
                "wrapper_action: dispatch returned {}: {}",
                status, error_msg
            ));
        }

        // The /wrappers/:id/dispatch handler wraps the wrapper response in
        // the runner's `ApiResponse<DispatchResponse>` envelope:
        //   { "success": true, "data": { "result": <value> } }
        // Be tolerant of bare `{ "result": ... }` for forward compat.
        let result_value = extract_result(&body_value);

        if let Some(var_name) = step
            .wrapper_result_variable
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let serialized = match &result_value {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            context.shared_variables.set(var_name, serialized);
            debug!(
                "wrapper_action: stored dispatch result in shared variable '{}'",
                var_name
            );
        }

        StepHandlerResult::success_with_data(serde_json::json!({
            "wrapper_id": wrapper_id,
            "action_id": action_id,
            "result": result_value,
        }))
    }
}

/// Recursively resolve `{{ var }}` templates inside string-typed leaves of
/// a JSON value. Non-string values pass through unchanged.
fn resolve_templates(value: &Value, context: &HandlerContext) -> Value {
    let evaluator = ExpressionEvaluator::new();
    let runtime_ctx = &context.runtime_context;
    walk(value, &evaluator, runtime_ctx, &context.shared_variables)
}

fn walk(
    value: &Value,
    evaluator: &ExpressionEvaluator,
    runtime: &crate::orchestrator::context_propagation::RuntimeContext,
    shared: &crate::orchestrator::context_propagation::SharedVariableStore,
) -> Value {
    match value {
        Value::String(s) => {
            if !evaluator.has_expressions(s) {
                return Value::String(s.clone());
            }
            // First pass: try the runtime context's evaluator (handles
            // {{var}}, {{steps.X.output}}, special $ vars, etc.).
            let mut out = evaluator.evaluate(s, runtime);
            // Second pass: substitute any leftover {{ name }} from the
            // shared variable store (different namespace, simpler grammar:
            // just `{{ name }}` ↔ literal string value).
            if evaluator.has_expressions(&out) {
                out = substitute_shared(&out, shared);
            }
            // Try to round-trip the result back to JSON so callers that
            // pass numeric / boolean templates still get a typed value.
            // If the string is a valid JSON literal that's *not* a string,
            // upgrade. Otherwise keep as a string.
            match serde_json::from_str::<Value>(&out) {
                Ok(parsed) if !parsed.is_string() => parsed,
                _ => Value::String(out),
            }
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| walk(v, evaluator, runtime, shared)).collect())
        }
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), walk(v, evaluator, runtime, shared));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Best-effort `{{ name }}` substitution against `SharedVariableStore`.
/// Used as a second-pass fallback after `ExpressionEvaluator::evaluate`
/// because the two stores are intentionally separate; the runtime
/// evaluator only knows about its own variable map.
fn substitute_shared(
    text: &str,
    shared: &crate::orchestrator::context_propagation::SharedVariableStore,
) -> String {
    let re = match regex::Regex::new(r"\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*\}\}") {
        Ok(r) => r,
        Err(e) => {
            warn!("wrapper_action: regex compile failed: {}", e);
            return text.to_string();
        }
    };
    re.replace_all(text, |caps: &regex::Captures| {
        let name = &caps[1];
        match shared.get(name) {
            Some(v) => v,
            None => {
                debug!(
                    "wrapper_action: unresolved template variable '{}' (left as-is)",
                    name
                );
                caps[0].to_string()
            }
        }
    })
    .into_owned()
}

/// Pull the wrapper's `result` payload out of either:
///   - `{ "success": true, "data": { "result": ... } }` (runner envelope)
///   - `{ "result": ... }` (bare dispatch response)
///   - any other shape (returned verbatim)
fn extract_result(body: &Value) -> Value {
    if let Some(data) = body.get("data") {
        if let Some(result) = data.get("result") {
            return result.clone();
        }
        return data.clone();
    }
    if let Some(result) = body.get("result") {
        return result.clone();
    }
    body.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_result_handles_envelope() {
        let body = serde_json::json!({
            "success": true,
            "data": { "result": { "id": 42 } },
        });
        assert_eq!(extract_result(&body), serde_json::json!({ "id": 42 }));
    }

    #[test]
    fn extract_result_handles_bare_result() {
        let body = serde_json::json!({ "result": [1, 2, 3] });
        assert_eq!(extract_result(&body), serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn extract_result_falls_back_to_body() {
        let body = serde_json::json!({ "anything": true });
        assert_eq!(extract_result(&body), body);
    }

    #[test]
    fn substitute_shared_replaces_known_vars() {
        let store = crate::orchestrator::context_propagation::SharedVariableStore::new();
        store.set("name", "Ada".to_string());
        let out = substitute_shared("hello {{ name }} from {{ unknown }}", &store);
        assert_eq!(out, "hello Ada from {{ unknown }}");
    }
}
