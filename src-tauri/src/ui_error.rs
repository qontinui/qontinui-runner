//! UI error reporting surface for the runner frontend.
//!
//! Tracks the latest unhandled error observed by the React `ErrorBoundary`
//! at the top of the component tree (see `src/ErrorBoundary.tsx`). The
//! state is exposed to ops via two sinks:
//!
//! 1. The existing [`/health`](crate::mcp_api) endpoint, which now carries
//!    a `derived_status: "healthy" | "degraded" | "errored"` field alongside
//!    a nullable `ui_error` object. Supervisors and the qontinui-web fleet
//!    view poll this to flag runners whose Rust backend is up but whose UI
//!    is broken (errored) or whose embedding subsystem is unreachable
//!    (degraded).
//! 2. The runner's heartbeats (both the operations heartbeat in
//!    [`crate::heartbeat`] and the runner-fleet heartbeat in
//!    [`crate::server_mode`]). Each heartbeat payload includes the
//!    `derived_status` and `ui_error` fields so receivers can react
//!    without polling `/health` separately.
//!
//! Storage is in-memory only (no persistence). A single [`UiErrorState`]
//! lives on [`crate::commands::AppState`]. Reads go through a `RwLock`;
//! writes coalesce repeat occurrences of the same `message`/`digest` into
//! a single record with an incrementing `count` and a sliding
//! `reported_at` timestamp while preserving `first_seen`.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tokio::sync::RwLock;

/// A single unhandled frontend error.
///
/// Serialized as part of the `/health` response and every heartbeat payload
/// when `UiErrorState::get()` returns `Some`. `first_seen` is pinned to the
/// first report; `reported_at` slides forward on coalesced repeats. `count`
/// counts the number of `report()` calls that collapsed into this record.
#[derive(Debug, Clone, Serialize)]
pub struct UiError {
    /// `error.message` from the React error boundary. Always present.
    pub message: String,
    /// `error.stack` if available. Minified builds may omit this.
    pub stack: Option<String>,
    /// React's `ErrorInfo.componentStack`, if available.
    pub component_stack: Option<String>,
    /// React 18+ production error digest (when the render error was from a
    /// minified bundle). Used as the coalescing key when both sides have one.
    pub digest: Option<String>,
    /// When the error first fired (pinned — not updated by repeat reports).
    pub first_seen: DateTime<Utc>,
    /// When the most recent matching report fired. Updated each coalesce.
    pub reported_at: DateTime<Utc>,
    /// Number of reports that collapsed into this record (>= 1).
    pub count: u32,
}

/// Shared in-memory holder for the current UI error, if any.
///
/// Wraps a single-slot `Option<UiError>`. Concurrent reads are cheap
/// (`RwLock::read`); writes serialize through `RwLock::write`. Construct
/// exactly one instance on [`crate::commands::AppState`].
#[derive(Debug, Default)]
pub struct UiErrorState {
    inner: Arc<RwLock<Option<UiError>>>,
}

impl UiErrorState {
    /// Create an empty state. Equivalent to `Default::default()`; provided
    /// so callers can be explicit at the AppState construction site.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Record a UI error.
    ///
    /// If an existing record matches (same `digest` when both records have
    /// one, otherwise same `message`), increments `count` and updates
    /// `reported_at` + the latest `stack`/`component_stack` snapshot, but
    /// keeps `first_seen`. Otherwise replaces the slot with a fresh
    /// record whose `first_seen = reported_at = now` and `count = 1`.
    pub async fn report(
        &self,
        message: String,
        stack: Option<String>,
        component_stack: Option<String>,
        digest: Option<String>,
    ) {
        let now = Utc::now();
        let mut guard = self.inner.write().await;

        let matches = guard
            .as_ref()
            .map(|existing| matches_existing(existing, &message, digest.as_deref()))
            .unwrap_or(false);

        if matches {
            if let Some(existing) = guard.as_mut() {
                existing.count = existing.count.saturating_add(1);
                existing.reported_at = now;
                // Keep first_seen pinned. Refresh the freshest snapshot of
                // optional fields in case the new report carries more
                // context than the first one did.
                if stack.is_some() {
                    existing.stack = stack;
                }
                if component_stack.is_some() {
                    existing.component_stack = component_stack;
                }
                if digest.is_some() {
                    existing.digest = digest;
                }
                // message intentionally not overwritten — matching key.
            }
        } else {
            *guard = Some(UiError {
                message,
                stack,
                component_stack,
                digest,
                first_seen: now,
                reported_at: now,
                count: 1,
            });
        }
    }

    /// Wipe the current record. Called by the frontend error boundary when
    /// it recovers from an error state.
    pub async fn clear(&self) {
        let mut guard = self.inner.write().await;
        *guard = None;
    }

    /// Read a clone of the current record, if any. Cheap for hot paths
    /// like `/health` and heartbeats (clone is a few small fields).
    pub async fn get(&self) -> Option<UiError> {
        self.inner.read().await.clone()
    }
}

/// Determine whether an incoming report should coalesce into `existing`.
///
/// Priority: if both sides carry a non-empty `digest`, match on that
/// (React 18+ production builds). Otherwise fall back to `message`
/// equality. An empty digest on either side falls back to message match.
fn matches_existing(existing: &UiError, message: &str, digest: Option<&str>) -> bool {
    match (existing.digest.as_deref(), digest) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => a == b,
        _ => existing.message == message,
    }
}

// ---------------------------------------------------------------------------
// Derived-status helper
// ---------------------------------------------------------------------------

/// Compute the runner's overall `derived_status` from its sub-signals.
///
/// Priority (highest wins): any `errored` signal → "errored"; otherwise any
/// `degraded` signal → "degraded"; otherwise "healthy".
///
/// Inputs:
/// * `has_ui_error` — true when a React `ErrorBoundary` report is outstanding.
/// * `has_recent_crash` — true when a fresh Rust crash dump was surfaced at
///   startup (non-unwinding panics abort before React sees them).
/// * `embedding_reachable` — `None` until the first probe has run (treated as
///   unknown, not degraded — avoids false positives during boot). `Some(true)`
///   = healthy. `Some(false)` = degraded.
///
/// All three heartbeat sinks (`/health`, operations heartbeat, web-backend
/// heartbeat) call this so their `derived_status` stays in lockstep.
pub fn compute_derived_status(
    has_ui_error: bool,
    has_recent_crash: bool,
    embedding_reachable: Option<bool>,
) -> &'static str {
    if has_ui_error || has_recent_crash {
        "errored"
    } else if matches!(embedding_reachable, Some(false)) {
        "degraded"
    } else {
        "healthy"
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Report a UI error observed by the React error boundary.
///
/// JS call shape (Tauri converts camelCase at the top-arg level):
/// `invoke("report_ui_error", { message, stack, componentStack, digest })`
#[tauri::command]
pub async fn report_ui_error(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
    message: String,
    stack: Option<String>,
    component_stack: Option<String>,
    digest: Option<String>,
) -> Result<(), String> {
    app_state
        .ui_error
        .report(message, stack, component_stack, digest)
        .await;
    Ok(())
}

/// Clear the current UI error state (called when the boundary recovers).
#[tauri::command]
pub async fn clear_ui_error(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<(), String> {
    app_state.ui_error.clear().await;
    Ok(())
}

/// Read the current UI error state. Useful for debugging and as an
/// allowlisted target for the Phase 3I UI Bridge invoke proxy.
#[tauri::command]
pub async fn get_ui_error(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<Option<UiError>, String> {
    Ok(app_state.ui_error.get().await)
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_ui_error")
        .invoke_handler(tauri::generate_handler![
            report_ui_error,
            clear_ui_error,
            get_ui_error,
        ])
        .build()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_report_sets_count_and_timestamps() {
        let state = UiErrorState::new();
        state.report("boom".to_string(), None, None, None).await;
        let got = state.get().await.expect("state should be populated");
        assert_eq!(got.message, "boom");
        assert_eq!(got.count, 1);
        assert_eq!(got.first_seen, got.reported_at);
    }

    #[tokio::test]
    async fn repeat_same_message_coalesces() {
        let state = UiErrorState::new();
        state.report("boom".to_string(), None, None, None).await;
        let first_seen = state.get().await.unwrap().first_seen;

        // Small gap so reported_at moves forward measurably.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        state
            .report("boom".to_string(), Some("stack".into()), None, None)
            .await;

        let got = state.get().await.unwrap();
        assert_eq!(got.count, 2);
        assert_eq!(got.first_seen, first_seen);
        assert!(got.reported_at >= first_seen);
        assert_eq!(got.stack.as_deref(), Some("stack"));
    }

    #[tokio::test]
    async fn different_message_replaces_record() {
        let state = UiErrorState::new();
        state.report("boom".to_string(), None, None, None).await;
        state.report("other".to_string(), None, None, None).await;
        let got = state.get().await.unwrap();
        assert_eq!(got.message, "other");
        assert_eq!(got.count, 1);
    }

    #[tokio::test]
    async fn digest_is_preferred_coalescing_key() {
        let state = UiErrorState::new();
        state
            .report(
                "minified error #185".to_string(),
                None,
                None,
                Some("185".into()),
            )
            .await;
        // Different message but same digest -> coalesce.
        state
            .report(
                "different minified message".to_string(),
                None,
                None,
                Some("185".into()),
            )
            .await;
        let got = state.get().await.unwrap();
        assert_eq!(got.count, 2);
        // Original message is kept (matching key).
        assert_eq!(got.message, "minified error #185");
    }

    #[tokio::test]
    async fn clear_wipes_state() {
        let state = UiErrorState::new();
        state.report("boom".to_string(), None, None, None).await;
        state.clear().await;
        assert!(state.get().await.is_none());
    }

    #[test]
    fn derived_status_errored_wins_over_everything() {
        assert_eq!(compute_derived_status(true, false, Some(true)), "errored");
        assert_eq!(compute_derived_status(true, true, Some(false)), "errored");
        assert_eq!(compute_derived_status(false, true, Some(true)), "errored");
    }

    #[test]
    fn derived_status_degraded_when_embedding_unreachable() {
        assert_eq!(
            compute_derived_status(false, false, Some(false)),
            "degraded"
        );
    }

    #[test]
    fn derived_status_healthy_when_embedding_reachable() {
        assert_eq!(compute_derived_status(false, false, Some(true)), "healthy");
    }

    #[test]
    fn derived_status_unknown_embedding_is_healthy_not_degraded() {
        // Boot-time: probe hasn't run yet. Avoid false-positive degraded.
        assert_eq!(compute_derived_status(false, false, None), "healthy");
    }

    /// Wire-contract snapshot: serialized `UiError` must carry the exact
    /// snake_case field names the supervisor (`qontinui-supervisor::health_cache::UiErrorSummary`)
    /// and the web backend (`app/schemas/runner_fleet.py::UiErrorPayload`)
    /// deserialize. Drift here silently breaks fleet-level aggregation.
    #[test]
    fn ui_error_json_shape_matches_consumer_contract() {
        let now = Utc::now();
        let err = UiError {
            message: "boom".to_string(),
            stack: Some("stack-trace".to_string()),
            component_stack: Some("component-stack".to_string()),
            digest: Some("185".to_string()),
            first_seen: now,
            reported_at: now,
            count: 3,
        };
        let v = serde_json::to_value(&err).expect("serialize UiError");
        let obj = v.as_object().expect("UiError serializes to a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> = [
            "message",
            "stack",
            "component_stack",
            "digest",
            "first_seen",
            "reported_at",
            "count",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            keys, expected,
            "UiError wire keys drifted; consumers will silently fail to parse"
        );
        // Sanity-check types the supervisor's serde structs rely on.
        assert!(obj["message"].is_string());
        assert!(obj["count"].is_u64());
        assert!(
            obj["first_seen"].is_string(),
            "DateTime serializes as ISO8601 string"
        );
    }
}
