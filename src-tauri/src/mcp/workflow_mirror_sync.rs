//! Workflow mirror write-through queue.
//!
//! Phase 3.2 of plan
//! `D:/qontinui-root/plans/2026-05-22-mtc-iter3-remediation-web-dashboard.md`.
//!
//! After every successful local SQLite mutation (create/update/delete) on a
//! `UnifiedWorkflow`, the CRUD handlers in `mcp::unified_workflows` enqueue
//! a sync job here. The queue posts to `POST /api/v1/workflows/sync` on the
//! web backend with a runner-authoritative payload — the web backend keeps
//! a per-operator mirror so the dashboard's `/build/workflows` page can list
//! workflows even when the runner is offline.
//!
//! Design notes:
//!
//! * Fire-and-forget from the CRUD success path; the user's CRUD never
//!   blocks on the mirror write.
//! * Exponential backoff (250ms, 500ms, 1s, 2s, 4s), max 5 attempts. After
//!   the 5th failure we log an error and drop the job. The next local
//!   mutation supersedes it anyway because the runner re-publishes the
//!   full body on every CRUD.
//! * Auth via the device JWT stashed in `AuthManager`. If no token is
//!   available (runner is unauthenticated) we skip the post entirely —
//!   the dashboard can't show a workflow for an operator who hasn't
//!   logged in.
//! * `tenant_id` and `owner_user_id` are NOT in the payload. The web
//!   backend reads them from the device JWT — never trust runner-asserted
//!   tenancy.

use crate::auth::AuthManager;
use crate::unified_workflows::UnifiedWorkflow;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::api_config::get_api_base_url;

/// Process-wide singleton queue. Lazy-initialised so we only spawn the
/// background worker once per runner lifetime. CRUD handlers reach for it
/// via [`global_queue`].
static GLOBAL_QUEUE: OnceLock<WorkflowMirrorSyncQueue> = OnceLock::new();

/// Return (lazily initialising) the global sync queue.
///
/// The worker is `tokio::spawn`ed inside an existing async runtime; calling
/// this for the first time from outside an async context will panic. In
/// practice CRUD handlers are always invoked within the MCP axum runtime,
/// so this is safe in production. Tests should construct queues directly
/// via [`WorkflowMirrorSyncQueue::spawn`] from inside a `#[tokio::test]`.
pub fn global_queue() -> &'static WorkflowMirrorSyncQueue {
    GLOBAL_QUEUE.get_or_init(WorkflowMirrorSyncQueue::spawn)
}

/// Maximum number of attempts before giving up on a mirror sync job.
const MAX_ATTEMPTS: u32 = 5;

/// Backoff schedule (in ms) — index `i` is the delay before attempt `i+1`.
const BACKOFF_MS: [u64; MAX_ATTEMPTS as usize] = [250, 500, 1000, 2000, 4000];

/// HTTP timeout for a single sync attempt.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

/// A single sync intent — either an upsert or a delete for a given
/// workflow id.
#[derive(Debug, Clone)]
pub enum WorkflowMirrorSyncJob {
    /// Upsert: full body, runner sets `runner_updated_at`.
    Upsert(UnifiedWorkflow),
    /// Delete: id + mtime so the mirror can apply last-write-wins.
    Delete {
        id: String,
        runner_updated_at: DateTime<Utc>,
    },
}

/// On-the-wire payload for POST /api/v1/workflows/sync.
///
/// `tenant_id` and `owner_user_id` are deliberately absent — the web
/// backend reads those from the device JWT, not from the body.
#[derive(Debug, Serialize, Deserialize)]
struct SyncPayload {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    definition: serde_json::Value,
    runner_updated_at: DateTime<Utc>,
    #[serde(default)]
    deleted: bool,
}

/// Queue handle — clone freely; pushes are fire-and-forget.
#[derive(Debug, Clone)]
pub struct WorkflowMirrorSyncQueue {
    tx: mpsc::UnboundedSender<WorkflowMirrorSyncJob>,
}

impl WorkflowMirrorSyncQueue {
    /// Construct a queue + spawn the background worker.
    ///
    /// The worker terminates when the last `WorkflowMirrorSyncQueue`
    /// handle is dropped (tx closed).
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<WorkflowMirrorSyncJob>();
        let auth = Arc::new(AuthManager::new());
        let client = Arc::new(
            reqwest::Client::builder()
                .timeout(ATTEMPT_TIMEOUT)
                .build()
                .unwrap_or_default(),
        );

        tokio::spawn(run_worker(rx, auth, client, get_api_base_url));
        Self { tx }
    }

    /// Enqueue a sync intent. Returns immediately; the actual POST happens
    /// in the background worker.
    pub fn push(&self, job: WorkflowMirrorSyncJob) {
        // tx.send is sync-non-blocking; only fails if the worker is dead
        // (rx dropped). We log at debug to avoid log-spam during shutdown.
        if let Err(e) = self.tx.send(job) {
            debug!(
                "workflow_mirror_sync_queue: send failed (worker dropped?): {}",
                e
            );
        }
    }
}

/// Push an upsert intent. Convenience wrapper that records `now()` as the
/// runner's last-modified time and clones the workflow for the channel.
pub fn enqueue_upsert(queue: &WorkflowMirrorSyncQueue, workflow: UnifiedWorkflow) {
    queue.push(WorkflowMirrorSyncJob::Upsert(workflow));
}

/// Push a delete intent.
pub fn enqueue_delete(queue: &WorkflowMirrorSyncQueue, workflow_id: String) {
    queue.push(WorkflowMirrorSyncJob::Delete {
        id: workflow_id,
        runner_updated_at: Utc::now(),
    });
}

async fn run_worker(
    mut rx: mpsc::UnboundedReceiver<WorkflowMirrorSyncJob>,
    auth: Arc<AuthManager>,
    client: Arc<reqwest::Client>,
    api_base_fn: impl Fn() -> String + Send + 'static,
) {
    info!("workflow_mirror_sync worker started");
    while let Some(job) = rx.recv().await {
        let payload = build_payload(&job);
        let api_base = api_base_fn();
        let url = format!("{}/api/v1/workflows/sync", api_base);
        // Resolve the device JWT. In production this comes from
        // AuthManager (secure storage / keychain). In tests we accept an
        // env-var fallback so the integration test in
        // `tests/workflow_mirror_sync.rs` can drive the worker against a
        // mock backend without provisioning real auth.
        let token = std::env::var("QONTINUI_ACCESS_TOKEN")
            .ok()
            .or_else(|| auth.get_access_token().ok());
        match token {
            Some(token) => {
                run_with_retry(client.clone(), &url, &token, &payload).await;
            }
            None => {
                debug!(
                    "workflow_mirror_sync: no device token available, skipping sync for {}",
                    payload.id
                );
            }
        }
    }
    info!("workflow_mirror_sync worker shutting down");
}

fn build_payload(job: &WorkflowMirrorSyncJob) -> SyncPayload {
    match job {
        WorkflowMirrorSyncJob::Upsert(workflow) => {
            let runner_updated_at =
                parse_workflow_updated_at(&workflow.updated_at).unwrap_or_else(Utc::now);
            let definition = serde_json::to_value(workflow)
                .unwrap_or_else(|_| serde_json::json!({"id": workflow.id, "name": workflow.name}));
            SyncPayload {
                id: workflow.id.clone(),
                name: workflow.name.clone(),
                category: if workflow.category.is_empty() {
                    None
                } else {
                    Some(workflow.category.clone())
                },
                definition,
                runner_updated_at,
                deleted: false,
            }
        }
        WorkflowMirrorSyncJob::Delete {
            id,
            runner_updated_at,
        } => SyncPayload {
            id: id.clone(),
            name: String::new(),
            category: None,
            definition: serde_json::Value::Null,
            runner_updated_at: *runner_updated_at,
            deleted: true,
        },
    }
}

fn parse_workflow_updated_at(s: &str) -> Option<DateTime<Utc>> {
    // UnifiedWorkflow.updated_at is RFC3339; chrono can parse with
    // `parse_from_rfc3339` then convert to UTC.
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

async fn run_with_retry(
    client: Arc<reqwest::Client>,
    url: &str,
    token: &str,
    payload: &SyncPayload,
) {
    for attempt in 0..MAX_ATTEMPTS {
        match attempt_sync(&client, url, token, payload).await {
            Ok(()) => {
                debug!(
                    "workflow_mirror_sync: posted {} on attempt {} (deleted={})",
                    payload.id,
                    attempt + 1,
                    payload.deleted
                );
                return;
            }
            Err(e) => {
                let last = attempt + 1 == MAX_ATTEMPTS;
                if last {
                    error!(
                        "workflow_mirror_sync: giving up on {} after {} attempts: {}",
                        payload.id, MAX_ATTEMPTS, e
                    );
                    return;
                }
                let delay = BACKOFF_MS[attempt as usize];
                warn!(
                    "workflow_mirror_sync: attempt {} for {} failed ({}), retrying in {}ms",
                    attempt + 1,
                    payload.id,
                    e,
                    delay
                );
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
        }
    }
}

async fn attempt_sync(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    payload: &SyncPayload,
) -> Result<(), String> {
    let response = client
        .post(url)
        .bearer_auth(token)
        .json(payload)
        .send()
        .await
        .map_err(|e| format!("network: {}", e))?;

    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    // 409 Conflict means we sent a stale mtime. That's not retryable —
    // the mirror has a newer version, return Ok so we don't waste attempts.
    if status.as_u16() == 409 {
        info!(
            "workflow_mirror_sync: 409 conflict for {} (mirror already has a newer version)",
            payload.id
        );
        return Ok(());
    }

    let body = response
        .text()
        .await
        .unwrap_or_else(|_| String::from("<no body>"));
    Err(format!("status {}: {}", status, body))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router,
    };
    use std::sync::Mutex as StdMutex;

    #[test]
    fn parse_workflow_updated_at_rfc3339() {
        let parsed = parse_workflow_updated_at("2026-05-22T12:34:56Z");
        assert!(parsed.is_some());
        let dt = parsed.unwrap();
        // Round-trip — the actual epoch isn't what matters; what
        // matters is the parser accepts the RFC3339 string + the
        // resulting DateTime serialises back to the same instant.
        assert_eq!(dt.to_rfc3339(), "2026-05-22T12:34:56+00:00");
    }

    #[test]
    fn parse_workflow_updated_at_handles_garbage() {
        assert!(parse_workflow_updated_at("not a date").is_none());
        assert!(parse_workflow_updated_at("").is_none());
    }

    #[test]
    fn build_payload_delete_carries_deleted_flag() {
        let job = WorkflowMirrorSyncJob::Delete {
            id: "abc".to_string(),
            runner_updated_at: Utc::now(),
        };
        let p = build_payload(&job);
        assert!(p.deleted);
        assert_eq!(p.id, "abc");
        assert_eq!(p.name, "");
    }

    /// Build a minimal UnifiedWorkflow by deserialising a small JSON
    /// blob. Avoids depending on a Default impl on the schemas-crate
    /// type (which doesn't derive Default).
    fn _wf(id: &str, name: &str, category: &str) -> UnifiedWorkflow {
        let blob = serde_json::json!({
            "id": id,
            "name": name,
            "category": category,
            "updatedAt": "2026-05-22T00:00:00Z",
            "createdAt": "2026-05-22T00:00:00Z",
        });
        serde_json::from_value(blob).expect("UnifiedWorkflow from json")
    }

    #[test]
    fn build_payload_upsert_does_not_set_deleted() {
        let workflow = _wf("wf-1", "test", "ci");
        let job = WorkflowMirrorSyncJob::Upsert(workflow);
        let p = build_payload(&job);
        assert!(!p.deleted);
        assert_eq!(p.id, "wf-1");
        assert_eq!(p.name, "test");
        assert_eq!(p.category.as_deref(), Some("ci"));
    }

    #[test]
    fn build_payload_upsert_empty_category_is_none() {
        let workflow = _wf("wf-2", "no cat", "");
        let job = WorkflowMirrorSyncJob::Upsert(workflow);
        let p = build_payload(&job);
        assert!(p.category.is_none());
    }

    // ---------------------------------------------------------------
    // HTTP retry behaviour — exercises `attempt_sync` + `run_with_retry`
    // against an in-process axum mock-server.
    // ---------------------------------------------------------------

    #[derive(Default, Clone)]
    struct MockState {
        status_queue: Arc<StdMutex<Vec<u16>>>,
        captured: Arc<StdMutex<Vec<serde_json::Value>>>,
    }

    async fn mock_handler(
        State(state): State<MockState>,
        Json(body): Json<serde_json::Value>,
    ) -> impl IntoResponse {
        state.captured.lock().unwrap().push(body.clone());
        let code = {
            let mut q = state.status_queue.lock().unwrap();
            if q.is_empty() {
                200
            } else {
                q.remove(0)
            }
        };
        (
            StatusCode::from_u16(code).unwrap_or(StatusCode::OK),
            Json(serde_json::json!({"ok": true})),
        )
    }

    async fn spawn_mock() -> (String, MockState) {
        let state = MockState::default();
        let app = Router::new()
            .route("/api/v1/workflows/sync", post(mock_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        (format!("http://{}", addr), state)
    }

    fn test_payload(id: &str) -> SyncPayload {
        SyncPayload {
            id: id.to_string(),
            name: "test".to_string(),
            category: Some("ci".to_string()),
            definition: serde_json::json!({}),
            runner_updated_at: Utc::now(),
            deleted: false,
        }
    }

    #[tokio::test]
    async fn attempt_sync_returns_ok_on_200() {
        let (url, state) = spawn_mock().await;
        let client = reqwest::Client::new();
        let payload = test_payload("a");
        let result = attempt_sync(
            &client,
            &format!("{}/api/v1/workflows/sync", url),
            "test-token",
            &payload,
        )
        .await;
        assert!(result.is_ok());
        let cap = state.captured.lock().unwrap();
        assert_eq!(cap.len(), 1);
        // tenant_id / owner_user_id must not leak from the runner.
        assert!(cap[0].get("tenant_id").is_none());
        assert!(cap[0].get("owner_user_id").is_none());
    }

    #[tokio::test]
    async fn attempt_sync_treats_409_as_terminal_success() {
        let (url, state) = spawn_mock().await;
        *state.status_queue.lock().unwrap() = vec![409];
        let client = reqwest::Client::new();
        let payload = test_payload("b");
        let result = attempt_sync(
            &client,
            &format!("{}/api/v1/workflows/sync", url),
            "test-token",
            &payload,
        )
        .await;
        assert!(
            result.is_ok(),
            "409 must short-circuit (no further retries)"
        );
    }

    #[tokio::test]
    async fn run_with_retry_retries_then_succeeds() {
        let (url, state) = spawn_mock().await;
        // Two 500s then a 200 — total 3 captures.
        *state.status_queue.lock().unwrap() = vec![500, 503];
        let client = Arc::new(reqwest::Client::new());
        let payload = test_payload("c");
        run_with_retry(
            client,
            &format!("{}/api/v1/workflows/sync", url),
            "test-token",
            &payload,
        )
        .await;
        let cap = state.captured.lock().unwrap();
        assert_eq!(cap.len(), 3, "should have retried twice then succeeded");
    }

    #[tokio::test]
    async fn run_with_retry_gives_up_after_max_attempts() {
        let (url, state) = spawn_mock().await;
        // 5 consecutive failures — should stop trying after MAX_ATTEMPTS.
        *state.status_queue.lock().unwrap() = vec![500, 500, 500, 500, 500];
        let client = Arc::new(reqwest::Client::new());
        let payload = test_payload("d");
        run_with_retry(
            client,
            &format!("{}/api/v1/workflows/sync", url),
            "test-token",
            &payload,
        )
        .await;
        let cap = state.captured.lock().unwrap();
        assert_eq!(
            cap.len(),
            MAX_ATTEMPTS as usize,
            "should give up after MAX_ATTEMPTS"
        );
    }
}
