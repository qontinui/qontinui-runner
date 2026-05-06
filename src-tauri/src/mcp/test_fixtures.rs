//! Debug-gated session-injection HTTP endpoints for manual UI testing.
//!
//! Phase 5.1 of the UI Bridge discoverability/effectiveness plan
//! (`qontinui-dev-notes/ui-bridge/docs/2026-05-03-ui-bridge-discoverability-effectiveness-plan.md`).
//!
//! ## What this is for
//!
//! `SessionCard` and the surrounding "Promote to Worktree" / "Commit Progress"
//! buttons only render for sessions whose `liveStatus` is one of
//! `active-in-zone | needs-input | frozen`. Producing such a session via the
//! real path requires spawning a Claude CLI child process — slow, environment-
//! dependent, and impractical for snapshot-driven UI tests that just want to
//! verify the buttons register.
//!
//! These endpoints let a test harness inject a fake session (no PID, no
//! threads, no PG row) so the UI can render the card. Verification is
//! "does the button register in the UI Bridge snapshot?" — clicking it
//! through to actual promotion is out of scope (and would 404 anyway because
//! there is no live `ClaudeSession` in the manager).
//!
//! ## Why a parallel `TestSession` instead of a fake `ClaudeSession`
//!
//! `ClaudeSession::spawn` runs the full CLI handshake (spawn process, init
//! request, wait for `Ready` state). Every field on the struct
//! (`Child` PID, mpsc channels, JoinHandles, atomic state tracker) assumes a
//! real subprocess. A constructor that bypasses all of that would be larger
//! and more invasive than the standalone struct here. The plan's caveat
//! ("If `ClaudeSession` is too coupled to a real PID, define a parallel
//! 'test session' type") applies — this is the parallel type.
//!
//! ## Cfg gating
//!
//! The entire module compiles only when `debug_assertions` is true OR the
//! explicit `test-fixtures` feature is set. Production release builds with
//! the feature off see no module, no routes, no extra compilation cost.
//! Wiring in `mcp_api.rs` and `mcp/mod.rs` mirrors the same gate.
//!
//! ## What this module does NOT do
//!
//! It does not modify `transcript_list_sessions` or the React side that
//! merges transcripts + tabs + digests into `UnifiedSession[]`. To make
//! `SessionCard` actually render an injected fake on screen, a follow-up
//! patch needs to merge `injected_test_sessions()` into the transcript list
//! (or have the frontend call this endpoint directly). The plan defers that
//! frontend wiring to a separate change; this module ships the backend
//! storage and HTTP surface so it's ready when that wiring lands.

#![cfg(any(debug_assertions, feature = "test-fixtures"))]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use super::types::ApiState;

// =============================================================================
// Test session storage
// =============================================================================

/// Live-status hints accepted by `/test/inject-session`.
///
/// Mirrors the subset of `SessionLiveStatus` enum values from
/// `qontinui-runner/src/components/terminal/useSessionManager.ts` that gate
/// the Promote / Commit buttons (the `canPromote` / `canCommit` predicates in
/// `SessionCard.tsx`). Other live-statuses (`completed`, `dormant`, etc.)
/// are not useful for the test fixture's purpose.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TestLiveStatus {
    ActiveInZone,
    NeedsInput,
    Frozen,
}

impl TestLiveStatus {
    /// Stringified form matching the frontend `SessionLiveStatus` literal.
    pub fn as_frontend_str(&self) -> &'static str {
        match self {
            Self::ActiveInZone => "active-in-zone",
            Self::NeedsInput => "needs-input",
            Self::Frozen => "frozen",
        }
    }
}

/// Worktree metadata payload accepted on the inject body. Matches the shape
/// of `claude_session::session::WorktreeInfo` for downstream callers that
/// want to mirror a promoted session — but the fields are independent so
/// the test never has to stand up a real worktree on disk.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TestWorktreeInfo {
    pub id: String,
    pub path: String,
    pub branch: String,
}

/// Fake-session record. Stored in the in-process registry under
/// `task_run_id` (which equals `session_id` for live PTY sessions in this
/// runner — see the `tabSessionMap` lookup in `useSessionManager.ts`).
///
/// The fields are a flat superset of what `UnifiedSession` consumes plus
/// the bare-minimum metadata the Promote/Commit handlers would receive on a
/// real call. No `Arc`, no threads, no PID — that is the point.
#[derive(Debug, Clone, Serialize)]
pub struct TestSession {
    pub task_run_id: String,
    pub session_id: String,
    pub name: String,
    pub live_status: TestLiveStatus,
    pub worktree: Option<TestWorktreeInfo>,
    /// ISO 8601 timestamp written at injection time. Drives the
    /// `lastModified` / `startedAt` columns that `SessionCard` formats via
    /// `formatTimeAgo`.
    pub injected_at: String,
    /// Synthetic project path the fake should masquerade as living under.
    /// Defaults to `"<test-fixture>"` so the frontend's project-label
    /// extractor produces a stable, recognizable string. Tests that care
    /// about per-project grouping can pass a real workspace path.
    pub project_path: String,
    /// Synthetic config dir for the fake. Defaults to
    /// `"C:\\claude\\.claude-test-fixture"` so `extractAccountLabel` in
    /// `useSessionManager.ts` produces the obvious label `test-fixture`.
    pub config_dir: String,
}

/// Project a `TestSession` into the on-the-wire `TranscriptSession` shape
/// that `transcript_list_sessions` returns to the frontend.
///
/// The frontend filters out transcripts with `message_count == 0` (see
/// `useSessionManager.ts:498`), so we hand back `1` to ensure the fake
/// renders. `injected_live_status` is the override that
/// `useSessionManager` consults instead of computing live-status from tab
/// correlation — without it, fakes would all fall through to `dormant`
/// and `canPromote` / `canCommit` would never fire (predicates at
/// `SessionCard.tsx:143` and `:217`).
pub fn project_test_session(
    session: &TestSession,
) -> crate::terminal::transcript::TranscriptSession {
    crate::terminal::transcript::TranscriptSession {
        session_id: session.session_id.clone(),
        project_path: session.project_path.clone(),
        config_dir: session.config_dir.clone(),
        // `> 0` so the frontend filter at `useSessionManager.ts:498`
        // doesn't drop the fake.
        message_count: 1,
        last_modified: session.injected_at.clone(),
        started_at: Some(session.injected_at.clone()),
        first_message_preview: Some(session.name.clone()),
        has_plans: false,
        display_name: session.name.clone(),
        injected_live_status: Some(session.live_status.as_frontend_str().to_string()),
    }
}

/// Merge real and injected sessions for the transcript list response.
///
/// Real sessions stay first (preserving the existing recency sort done
/// upstream by `collect_all_sessions`). Fakes are appended in registry
/// iteration order — they're a debug affordance, not real data, so giving
/// them a deterministic but boring position keeps the UI predictable.
///
/// Extracted as a standalone function so the projection can be unit-tested
/// without standing up the full Tauri command + filesystem scan.
pub fn merge_with_injected(
    mut real_sessions: Vec<crate::terminal::transcript::TranscriptSession>,
    injected: Vec<TestSession>,
) -> Vec<crate::terminal::transcript::TranscriptSession> {
    real_sessions.reserve(injected.len());
    for fake in &injected {
        real_sessions.push(project_test_session(fake));
    }
    real_sessions
}

/// Process-wide registry of injected test sessions, keyed by `task_run_id`.
///
/// `OnceLock` for lazy init; the inner `Mutex<HashMap>` is fine — write
/// volume is bounded by manual test runs, and there is no real-time path
/// reading this. Real `ClaudeSession`s live in `SessionManager` and are
/// untouched by this store.
fn registry() -> &'static Mutex<HashMap<String, TestSession>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, TestSession>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Read-only snapshot of every currently-injected fake.
///
/// Public so a future patch can merge these into `transcript_list_sessions`
/// (or surface them through a parallel command) without re-importing the
/// internal `Mutex`. Returns an empty `Vec` when the registry hasn't been
/// touched yet.
pub fn injected_test_sessions() -> Vec<TestSession> {
    match registry().lock() {
        Ok(guard) => guard.values().cloned().collect(),
        Err(poisoned) => {
            // Poisoned just means a writer panicked mid-update; the data is
            // still readable. Return what we have so a follow-up clear()
            // can recover the lock cleanly.
            poisoned.into_inner().values().cloned().collect()
        }
    }
}

// =============================================================================
// /ui-bridge/test/inject-session
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct InjectSessionRequest {
    pub task_run_id: String,
    pub name: String,
    #[serde(rename = "liveStatus")]
    pub live_status: TestLiveStatus,
    #[serde(default)]
    pub worktree: Option<TestWorktreeInfo>,
    /// Optional masquerade-as project path. Falls back to `"<test-fixture>"`
    /// if omitted; the frontend's `extractProjectLabel` handles either.
    #[serde(default)]
    pub project_path: Option<String>,
    /// Optional masquerade-as config dir. Falls back to
    /// `"C:\\claude\\.claude-test-fixture"`; the label `test-fixture` is
    /// what `extractAccountLabel` will produce in the UI.
    #[serde(default)]
    pub config_dir: Option<String>,
}

/// Default project path used when an inject body omits `project_path`. A
/// recognizably synthetic value keeps fakes visually distinct in the
/// session manager.
const DEFAULT_TEST_PROJECT_PATH: &str = "<test-fixture>";

/// Default config dir used when an inject body omits `config_dir`. The
/// trailing path component drives `extractAccountLabel` → `test-fixture`.
const DEFAULT_TEST_CONFIG_DIR: &str = "C:\\claude\\.claude-test-fixture";

#[derive(Debug, Clone, Serialize)]
pub struct InjectSessionResponse {
    pub success: bool,
    pub task_run_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InjectSessionError {
    pub success: bool,
    pub error: String,
}

async fn inject_session_handler(
    Json(req): Json<InjectSessionRequest>,
) -> Result<Json<InjectSessionResponse>, (StatusCode, Json<InjectSessionError>)> {
    if req.task_run_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(InjectSessionError {
                success: false,
                error: "task_run_id must not be empty".to_string(),
            }),
        ));
    }
    if req.name.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(InjectSessionError {
                success: false,
                error: "name must not be empty".to_string(),
            }),
        ));
    }

    let session = TestSession {
        task_run_id: req.task_run_id.clone(),
        // For interactive PTY sessions in this runner the Claude session id
        // and the task_run_id are the same value (see `claudeSessionId`
        // wiring in `useTerminalManager.ts`). Mirror that here so any
        // downstream consumer that looks up by either key finds the fake.
        session_id: req.task_run_id.clone(),
        name: req.name.clone(),
        live_status: req.live_status,
        worktree: req.worktree.clone(),
        injected_at: chrono::Utc::now().to_rfc3339(),
        project_path: req
            .project_path
            .clone()
            .unwrap_or_else(|| DEFAULT_TEST_PROJECT_PATH.to_string()),
        config_dir: req
            .config_dir
            .clone()
            .unwrap_or_else(|| DEFAULT_TEST_CONFIG_DIR.to_string()),
    };

    let task_run_id = session.task_run_id.clone();
    let session_id = session.session_id.clone();

    match registry().lock() {
        Ok(mut guard) => {
            guard.insert(task_run_id.clone(), session);
        }
        Err(mut poisoned) => {
            // Recover by overwriting with the new entry — same as Ok branch,
            // we just had a previous panic-poisoned lock.
            poisoned.get_mut().insert(task_run_id.clone(), session);
        }
    }

    info!(
        "test_fixtures: injected session task_run_id={} liveStatus={} name=\"{}\"",
        task_run_id,
        req.live_status.as_frontend_str(),
        req.name,
    );

    Ok(Json(InjectSessionResponse {
        success: true,
        task_run_id,
        session_id,
    }))
}

// =============================================================================
// /ui-bridge/test/clear-sessions
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct ClearSessionsResponse {
    pub success: bool,
    pub cleared_count: usize,
}

async fn clear_sessions_handler() -> Json<ClearSessionsResponse> {
    let cleared = match registry().lock() {
        Ok(mut guard) => {
            let n = guard.len();
            guard.clear();
            n
        }
        Err(mut poisoned) => {
            let map = poisoned.get_mut();
            let n = map.len();
            map.clear();
            n
        }
    };

    info!(
        "test_fixtures: cleared {} injected session(s); real SessionManager untouched",
        cleared,
    );

    Json(ClearSessionsResponse {
        success: true,
        cleared_count: cleared,
    })
}

// =============================================================================
// Routes
// =============================================================================

/// Mount `/ui-bridge/test/*` endpoints.
///
/// Caller (`mcp_api::create_router`) merges this router under the same
/// cfg-gate so the routes only exist on debug or `--features test-fixtures`
/// builds. The path prefix mirrors `/ui-bridge/sdk/*` and friends so it
/// stays clearly off the production `/control/*` surface.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/ui-bridge/test/inject-session",
            post(inject_session_handler),
        )
        .route(
            "/ui-bridge/test/clear-sessions",
            post(clear_sessions_handler),
        )
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Tests in this module all touch the shared `registry()` singleton, so
    /// they must run serially to avoid stomping on each other's clear/insert
    /// sequences (cargo runs tests in parallel by default). A test-only
    /// mutex serializes them without forcing the suite-wide `--test-threads=1`.
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    /// Inject + clear roundtrip exercises every public path in this module
    /// without standing up a full axum server. Cleans up after itself so it
    /// can run alongside other tests that touch the global registry.
    #[tokio::test]
    async fn inject_then_clear_roundtrip() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Start from a known-empty state — earlier tests in the suite may
        // have inserted entries.
        let _ = clear_sessions_handler().await;

        let req = InjectSessionRequest {
            task_run_id: "fixture-roundtrip-1".to_string(),
            name: "Fixture roundtrip".to_string(),
            live_status: TestLiveStatus::ActiveInZone,
            worktree: Some(TestWorktreeInfo {
                id: "wt-roundtrip-1".to_string(),
                path: "C:/tmp/fixture-wt".to_string(),
                branch: "fixture-roundtrip-1-wt".to_string(),
            }),
            project_path: None,
            config_dir: None,
        };

        let resp = inject_session_handler(Json(req))
            .await
            .expect("inject should succeed");
        assert!(resp.success);
        assert_eq!(resp.task_run_id, "fixture-roundtrip-1");
        assert_eq!(resp.session_id, "fixture-roundtrip-1");

        let listed = injected_test_sessions();
        let entry = listed
            .iter()
            .find(|s| s.task_run_id == "fixture-roundtrip-1")
            .expect("entry should be visible after inject");
        assert_eq!(entry.live_status, TestLiveStatus::ActiveInZone);
        assert!(entry.worktree.is_some());

        let cleared = clear_sessions_handler().await;
        assert!(cleared.success);
        assert!(cleared.cleared_count >= 1);
        assert!(injected_test_sessions().is_empty());
    }

    /// Empty / whitespace-only `task_run_id` is rejected with 400 so callers
    /// can't pollute the registry with anonymous entries that
    /// `clear-sessions` then has to enumerate.
    #[tokio::test]
    async fn inject_rejects_empty_task_run_id() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let req = InjectSessionRequest {
            task_run_id: "   ".to_string(),
            name: "still has a name".to_string(),
            live_status: TestLiveStatus::Frozen,
            worktree: None,
            project_path: None,
            config_dir: None,
        };
        let err = inject_session_handler(Json(req))
            .await
            .expect_err("empty id should be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(!err.1.success);
    }

    /// `merge_with_injected` is the helper `transcript_list_sessions` calls
    /// to weave fakes into the real-session list. This test exercises it in
    /// isolation so we can verify projection without standing up the full
    /// Tauri command + filesystem scan.
    #[tokio::test]
    async fn merge_appends_injected_after_real_with_status_override() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Start clean so we know exactly what's in the registry.
        let _ = clear_sessions_handler().await;

        // Inject one fake.
        let req = InjectSessionRequest {
            task_run_id: "fixture-merge-1".to_string(),
            name: "Merge target".to_string(),
            live_status: TestLiveStatus::NeedsInput,
            worktree: None,
            project_path: Some("D:/qontinui-root".to_string()),
            config_dir: None,
        };
        let _ = inject_session_handler(Json(req))
            .await
            .expect("inject should succeed");

        // Pretend we have one real on-disk transcript already.
        let real = crate::terminal::transcript::TranscriptSession {
            session_id: "real-session-abc".to_string(),
            project_path: "D:/qontinui-root".to_string(),
            config_dir: "C:/claude/.claude".to_string(),
            message_count: 42,
            last_modified: "2026-05-03T12:00:00Z".to_string(),
            started_at: Some("2026-05-03T11:00:00Z".to_string()),
            first_message_preview: Some("hello".to_string()),
            has_plans: false,
            display_name: "Real one".to_string(),
            injected_live_status: None,
        };

        let merged = merge_with_injected(vec![real], injected_test_sessions());

        // Real first.
        assert_eq!(merged[0].session_id, "real-session-abc");
        assert!(merged[0].injected_live_status.is_none());

        // Find the injected fake.
        let fake = merged
            .iter()
            .find(|s| s.session_id == "fixture-merge-1")
            .expect("injected fake should be in merged list");

        // Frontend filter at useSessionManager.ts:498 drops message_count == 0;
        // make sure projection bumps it to a renderable value.
        assert!(
            fake.message_count > 0,
            "fake must pass message_count > 0 filter"
        );
        assert_eq!(
            fake.injected_live_status.as_deref(),
            Some("needs-input"),
            "live-status override must reach the wire"
        );
        assert_eq!(fake.display_name, "Merge target");
        assert_eq!(fake.project_path, "D:/qontinui-root");
        // Default config dir should produce the `test-fixture` account label.
        assert!(
            fake.config_dir.contains("test-fixture"),
            "default config_dir should encode the test-fixture account label, got {}",
            fake.config_dir
        );

        // Cleanup so we don't leak into the next test.
        let _ = clear_sessions_handler().await;
    }
}
