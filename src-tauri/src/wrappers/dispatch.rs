//! Dispatch router: `POST /wrappers/:id/dispatch`.
//!
//! Phase 1.4 of the wrapper-runner integration plan. Lazy-spawns the
//! wrapper via [`super::manager::WrapperManager`], then proxies the
//! action call to the wrapper's local HTTP endpoint and returns the
//! wrapper's response verbatim.
//!
//! ## Concurrency
//!
//! By default, dispatches run in parallel. Actions whose manifest entry
//! has `exclusive: true` are serialized through a per-(wrapper, action)
//! tokio mutex. The mutex map is created lazily and never shrinks — the
//! number of `(wrapper, action)` pairs is bounded by the registry, so
//! retention is fine.
//!
//! ## Transport
//!
//! Phase 1 only enables `transport: "api"` wrappers, which speak HTTP
//! locally. The dispatcher POSTs `{ action, params }` to
//! `http://127.0.0.1:<port>/dispatch`. If a wrapper exposes a different
//! path the framework's Node entrypoint is responsible for binding it;
//! the runner's contract is "POST /dispatch on the wrapper's port".

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::manager::WrapperManager;
use super::registry::WrapperRegistry;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Default per-dispatch timeout. Plan §1.4: "60s default, configurable
/// per dispatch via header."
pub const DEFAULT_DISPATCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Hard upper bound for the per-dispatch timeout header — prevents a
/// runaway request from holding axum workers indefinitely.
pub const MAX_DISPATCH_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// HTTP header used to override the default dispatch timeout.
pub const TIMEOUT_HEADER: &str = "X-Wrapper-Dispatch-Timeout";

/// Body for `POST /wrappers/:id/dispatch`.
#[derive(Debug, Deserialize)]
pub struct DispatchRequest {
    pub action: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Successful dispatch response. Mirrors the wrapper's own envelope.
#[derive(Debug, Serialize)]
pub struct DispatchResponse {
    pub result: serde_json::Value,
}

/// Per-(wrapper, action) mutex registry for `exclusive: true` actions.
///
/// Wrapped in a single tokio Mutex so the inner HashMap stays consistent
/// across concurrent inserts. Lookups are fast because the contention
/// point is the *outer* mutex, not the inner one.
#[derive(Default)]
pub struct ExclusivityRegistry {
    inner: Mutex<HashMap<(String, String), Arc<Mutex<()>>>>,
}

impl ExclusivityRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Get-or-insert the mutex for `(wrapper_id, action)`. The returned
    /// `Arc<Mutex<()>>` is locked by the caller for the duration of the
    /// dispatch.
    pub async fn lock_for(&self, wrapper_id: &str, action: &str) -> Arc<Mutex<()>> {
        let mut map = self.inner.lock().await;
        map.entry((wrapper_id.to_string(), action.to_string()))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

/// Compose all the moving parts the dispatch handler needs. Stored on
/// `ApiState` (or wherever the application wires it) so the axum handler
/// can pull it out of the shared state.
pub struct DispatchContext {
    pub registry: Arc<WrapperRegistry>,
    pub manager: Arc<WrapperManager>,
    pub exclusivity: Arc<ExclusivityRegistry>,
    pub http: reqwest::Client,
}

impl DispatchContext {
    pub fn new(registry: Arc<WrapperRegistry>, manager: Arc<WrapperManager>) -> Arc<Self> {
        // No global timeout on the client — we apply per-call timeouts
        // via tokio::time::timeout so the header override is exact.
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(4)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Arc::new(Self {
            registry,
            manager,
            exclusivity: ExclusivityRegistry::new(),
            http,
        })
    }
}

/// Resolve the `(timeout, action_is_exclusive)` flags for a request.
fn resolve_timeout(headers: &HeaderMap) -> Duration {
    let parsed = headers
        .get(TIMEOUT_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs);
    match parsed {
        Some(d) if d > MAX_DISPATCH_TIMEOUT => MAX_DISPATCH_TIMEOUT,
        Some(d) if d.is_zero() => DEFAULT_DISPATCH_TIMEOUT,
        Some(d) => d,
        None => DEFAULT_DISPATCH_TIMEOUT,
    }
}

/// Look up a wrapper's `exclusive` flag for an action. Returns `false`
/// for unknown actions — strictly we'd want a 404, but the wrapper itself
/// will reject the unknown action with a 404 / error envelope, so this
/// matches the user-visible behavior either way.
async fn action_is_exclusive(registry: &WrapperRegistry, wrapper_id: &str, action: &str) -> bool {
    match registry.get(wrapper_id).await {
        Some(w) => w
            .actions
            .iter()
            .find(|a| a.id == action)
            .map(|a| a.exclusive)
            .unwrap_or(false),
        None => false,
    }
}

/// `POST /wrappers/:id/dispatch` handler.
pub async fn dispatch_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(wrapper_id): AxumPath<String>,
    headers: HeaderMap,
    Json(req): Json<DispatchRequest>,
) -> Result<Json<ApiResponse<DispatchResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let ctx = match state
        .app_state
        .wrapper_state
        .get()
        .map(|ws| ws.dispatch.clone())
    {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(api_error("wrappers subsystem is not initialized")),
            ));
        }
    };

    // Verify the wrapper exists in the registry — surface a 404 instead
    // of a confusing readiness timeout.
    if ctx.registry.get(&wrapper_id).await.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "wrapper '{}' is not installed",
                wrapper_id
            ))),
        ));
    }

    let timeout = resolve_timeout(&headers);
    let exclusive = action_is_exclusive(&ctx.registry, &wrapper_id, &req.action).await;

    // Acquire the exclusivity guard *before* spawning so a slow spawn
    // can't starve other parallel dispatches — well, parallel ones don't
    // hit this branch, so the order is moot for them; for exclusive
    // ones we want serialization across the spawn too, so this is fine.
    let _exclusive_guard = if exclusive {
        let mtx = ctx.exclusivity.lock_for(&wrapper_id, &req.action).await;
        Some(mtx.lock_owned().await)
    } else {
        None
    };

    // Lazy-spawn (no-op if already running).
    let port = ctx.manager.spawn(&wrapper_id).await.map_err(|e| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error(format!(
                "failed to spawn wrapper '{}': {}",
                wrapper_id, e
            ))),
        )
    })?;

    let url = format!("http://127.0.0.1:{}/dispatch", port);
    let body = serde_json::json!({
        "action": req.action,
        "params": req.params,
    });

    let response_future = ctx.http.post(&url).json(&body).send();
    let response = match tokio::time::timeout(timeout, response_future).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!(
                    "wrapper '{}' dispatch failed: {}",
                    wrapper_id, e
                ))),
            ));
        }
        Err(_) => {
            return Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(api_error(format!(
                    "wrapper '{}' dispatch timed out after {}s",
                    wrapper_id,
                    timeout.as_secs()
                ))),
            ));
        }
    };

    let status = response.status();
    let parsed: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!(
                    "wrapper '{}' returned non-JSON body: {}",
                    wrapper_id, e
                ))),
            ));
        }
    };

    // Mark a successful round-trip even on logical errors — the wrapper
    // is alive, which is what the idle reaper cares about.
    ctx.manager.note_dispatch(&wrapper_id).await;

    if !status.is_success() {
        return Err((
            // Translate the wrapper's HTTP status straightforwardly so
            // upstream callers can distinguish "wrapper said 4xx" from
            // "infra failure".
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(api_error(format!(
                "wrapper '{}' returned {}: {}",
                wrapper_id, status, parsed
            ))),
        ));
    }

    // Two acceptable response shapes from the wrapper:
    //   { "result": ... }
    //   <bare value>
    // We prefer the explicit shape; fall back to the bare value so
    // wrappers that return a literal payload still work.
    let result = parsed.get("result").cloned().unwrap_or(parsed);
    Ok(Json(ApiResponse::success(DispatchResponse { result })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_header_clamps_to_max() {
        let mut h = HeaderMap::new();
        h.insert(
            TIMEOUT_HEADER,
            axum::http::HeaderValue::from_static("99999"),
        );
        assert_eq!(resolve_timeout(&h), MAX_DISPATCH_TIMEOUT);
    }

    #[test]
    fn timeout_header_default_on_missing() {
        let h = HeaderMap::new();
        assert_eq!(resolve_timeout(&h), DEFAULT_DISPATCH_TIMEOUT);
    }

    #[test]
    fn timeout_header_zero_falls_back_to_default() {
        let mut h = HeaderMap::new();
        h.insert(TIMEOUT_HEADER, axum::http::HeaderValue::from_static("0"));
        assert_eq!(resolve_timeout(&h), DEFAULT_DISPATCH_TIMEOUT);
    }

    #[tokio::test]
    async fn exclusivity_registry_returns_same_arc_for_same_pair() {
        let reg = ExclusivityRegistry::new();
        let a = reg.lock_for("v0", "act").await;
        let b = reg.lock_for("v0", "act").await;
        assert!(Arc::ptr_eq(&a, &b));
        let c = reg.lock_for("v0", "other").await;
        assert!(!Arc::ptr_eq(&a, &c));
    }
}
