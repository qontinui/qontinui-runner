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
//! Wiring in `mcp_api.rs` and `mcp/mod.rs` mirrors the same gate. A CI step
//! (`seam-gate` in `.github/workflows/ci.yml`) plus in-module canary tests
//! (`module_cfg_gate_is_first_non_comment_line` &c.) assert all three cfg
//! anchors stay intact so this seam can never leak into a release binary.
//!
//! ============================================================================
//! ## CONSUMER CONTRACT — read this before writing an acceptance gate
//! ============================================================================
//!
//! This seam exists so a verification-loop agent can place a fake Claude
//! session into any of the Terminal-page StatusStrip's FIVE buckets through
//! the REAL production render path, then assert the strip renders correctly.
//! The mechanics below are load-bearing for a gate that must not flake.
//!
//! ### The five buckets and how each is reached
//!
//! | StatusStrip bucket | `liveStatus` | mechanism | inject body |
//! |--------------------|--------------|-----------|-------------|
//! | working            | `active-in-zone` | short-circuit (`injected_live_status`) | `{liveStatus:"active-in-zone"}` |
//! | needs-input        | `needs-input`    | short-circuit | `{liveStatus:"needs-input"}` |
//! | idle               | `frozen` (live tab) | **tab-backed**: a synthetic tab pre-aged past the 60s staleness sweep | `{liveStatus:"idle", tab_backed:true}` |
//! | error              | `error`          | **tab-backed**: a dead synthetic tab, exit_code != 0 | `{liveStatus:"error", tab_backed:true}` |
//! | completed          | `completed`      | **tab-backed**: a dead synthetic tab, exit_code 0 | `{liveStatus:"completed", tab_backed:true}` |
//!
//! Two projection modes back these (see `project_test_session`):
//!   - **short-circuit** — `working` / `needs-input` (and the orphan-demo
//!     `frozen`, plus short-circuit `error` / `completed`): the fake carries an
//!     `injected_live_status` string that `useSessionManager` consults INSTEAD
//!     of correlating a tab. No synthetic tab is emitted.
//!   - **tab-backed** — `idle` / `error` / `completed`: NO `injected_live_status`;
//!     instead an `injected_tab` spec is emitted, from which the frontend
//!     (`syntheticTabs.ts`) derives a synthetic `TerminalTab` that flows through
//!     the SAME `useSessionStateTracking` staleness sweep + dead-tab branch the
//!     real PTY tabs use. This is the ONLY way to reach `idle` and tab-backed
//!     `error`/`completed` — the short-circuit path cannot (`idle` is dropped
//!     by the orphan filter; tab-backed exit-code semantics need a real tab).
//!
//! Validation guards (enforced by `insert_session`):
//!   - `liveStatus:"idle"` REQUIRES `tab_backed:true` (else 400) — a tab-less
//!     injected `frozen` is forced orphaned and `computeStatusCounts` drops it.
//!   - `tab_backed:true` is ONLY valid for `idle` / `error` / `completed`
//!     (else 400) — the other statuses have no tab-backed mechanism.
//!
//! ### Seed-then-POLL contract (do NOT single-read)
//!
//! After a seed call returns 200, the strip does NOT update synchronously.
//! Two async latencies stack before the rendered counts converge:
//!   1. The frontend must refetch `transcript_list_sessions`; only then does
//!      the seeded fake (and any derived synthetic tab) enter
//!      `useSessionManager`. Every mutating route here emits a
//!      `test-fixtures-injected-changed` Tauri event that
//!      `useTranscriptSessions` listens for and refetches on immediately —
//!      this is what makes seeding/clearing deterministic even when the
//!      runner window is hidden (its 30s auto-refresh poll is
//!      visibility-gated and never ticks in a backgrounded window).
//!   2. For a tab-backed `idle` fake, the 60s **staleness sweep tick** must
//!      fire once more after the synthetic tab's pre-aged `lastOutput` seed is
//!      installed before `frozen`→idle is counted. The pre-age guarantees the
//!      tab is ALREADY past threshold, so this is one sweep tick (≤ ~2s in the
//!      test cadence), not a 60s wait — but it is NOT zero.
//!
//! Therefore an acceptance gate MUST POLL the rendered StatusStrip with a
//! ~5s budget (re-read every ~500ms until the expected counts appear), never
//! assert on a single read immediately after the seed POST. A single-read gate
//! will false-negative on the very first tick.
//!
//! ### `isMultiZone = sessionCount > 1` render gate
//!
//! The session-count pill AND the working/done/idle breakdown pill only render
//! when `sessionCount > 1` (`StatusStrip.tsx`: `const isMultiZone = sessionCount
//! > 1`). A scenario that seeds exactly ONE session shows NO count pill and NO
//! breakdown — the attention pills (needs-input / error / stuck-lock) still
//! render, but the ambient counts do not. To exercise the count/breakdown
//! surface, seed **≥ 2** sessions (e.g. `{working:1, idle:1}` → sessionCount 2).
//!
//! ### Teardown contract — TTL + clear-injected
//!
//! Two independent cleanup paths, so a gate can't leak fakes into a later run:
//!   - **Explicit**: `POST /ui-bridge/test/clear-injected` (or the alias
//!     `/clear-sessions`) drops EVERY injected fake immediately and returns the
//!     count cleared. `/seed-terminal-scenario` is itself clear-then-seed, so a
//!     fresh seed implicitly resets prior fakes. The derived synthetic tabs
//!     disappear automatically on the next `transcript_list_sessions` poll
//!     (they exist only as long as their session carries an `injected_tab`).
//!   - **TTL backstop**: any fake older than `SESSION_TTL` (10 min) is evicted
//!     lazily on the next registry read (`evict_expired`), so an abandoned
//!     test's leftover fakes can't pollute a later poll even if `clear-injected`
//!     is never called.
//!
//! A gate should still call `clear-injected` in teardown (don't rely on the TTL
//! backstop) and re-poll the strip to confirm the counts returned to baseline.
//!
//! ### Frontend inertness (Phase 3 / F1)
//!
//! Synthetic tabs never render a terminal pane (excluded structurally from the
//! rendering / zone / file-lock consumers — see `TerminalSessionContext.tsx`).
//! Additionally, session-level process-control ops (resume / promote / commit,
//! incl. Ctrl+Shift+J jump-to-frozen and bulk-resume) early-return a no-op with
//! a `[test-fixtures] ignoring <op> on synthetic tab <id>` warning when the
//! target is an injected fake (`isInjectedSession` in `syntheticTabs.ts`). So a
//! gate that clicks Promote/Commit/Resume on a seeded card observes a console
//! warn and NO backend call — that is the designed behavior, not a bug.
//!
//! ## History
//!
//! Phases 1+2 wired the merge: `transcript_list_sessions` now appends
//! `merge_with_injected(real, injected_test_sessions())`, and the frontend
//! derives synthetic tabs from the projected `injected_tab` spec. The earlier
//! "follow-up patch still needed" caveat is resolved.

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
/// `qontinui-runner/src/components/terminal/useSessionManager.ts` that the
/// StatusStrip buckets care about. The first three drive the short-circuit
/// path (`canPromote` / `canCommit` in `SessionCard.tsx` plus the
/// `working` / `needs-input` strip buckets). `Idle`, `Error`, and
/// `Completed` exist so a fake can be placed in EVERY StatusStrip bucket
/// through the real production render path — see `project_test_session` for
/// the per-status mechanism (short-circuit vs. tab-backed correlation).
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TestLiveStatus {
    ActiveInZone,
    NeedsInput,
    Frozen,
    Idle,
    Error,
    Completed,
}

impl TestLiveStatus {
    /// Stringified form matching the frontend `SessionLiveStatus` literal,
    /// used ONLY on the non-tab-backed short-circuit path
    /// (`injected_live_status`).
    ///
    /// `Idle` deliberately has no short-circuit string: the frontend forces
    /// an injected `frozen` to be orphaned (`isOrphaned = true`), which
    /// `computeStatusCounts` then drops, so idle is unreachable via the
    /// short-circuit. Idle is only reachable tab-backed (a synthetic stale
    /// tab), and the handler rejects `Idle` without `tab_backed`. We return
    /// `"frozen"` here purely as a never-exercised fallback; callers must not
    /// route `Idle` through the short-circuit.
    pub fn as_frontend_str(&self) -> &'static str {
        match self {
            Self::ActiveInZone => "active-in-zone",
            Self::NeedsInput => "needs-input",
            Self::Frozen => "frozen",
            Self::Idle => "frozen",
            Self::Error => "error",
            Self::Completed => "completed",
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
    /// When true the fake is projected WITHOUT an `injected_live_status`
    /// override so the frontend takes the REAL tab-correlation path; the
    /// projection instead emits an `injected_tab` spec from which the
    /// frontend derives a synthetic `TerminalTab`. This is how `idle`
    /// (pre-aged stale tab) and tab-backed `error` / `completed`
    /// (dead-tab exit-code) buckets are reached through production code.
    pub tab_backed: bool,
    /// Pre-age, in milliseconds, for the synthetic tab's `lastOutput`. Only
    /// meaningful for a tab-backed `Idle` fake — the frontend seeds
    /// `lastOutputTimeRef[tabId] = now - quiet_ms` so the existing 60s
    /// staleness sweep marks the tab stale → `frozen` → counted as idle.
    pub quiet_ms: Option<u64>,
}

/// Default pre-age (ms) for a tab-backed `Idle` fake's synthetic tab. The
/// staleness sweep marks a tab stale at `> 60000`ms of quiet, so the default
/// (and the floor enforced in `project_test_session`) is just over 60s.
const IDLE_DEFAULT_QUIET_MS: u64 = 61_000;

/// Synthetic-tab spec emitted on the wire when a fake is `tab_backed`.
///
/// Re-exported from `crate::terminal::transcript` (defined there because the
/// `TranscriptSession.injected_tab` field type must resolve in release builds
/// without the cfg-gated `test-fixtures` feature). The frontend
/// (`syntheticTabs.ts`) turns this into a minimal `TerminalTab` that flows
/// through the REAL `useSessionManager` tab-correlation path:
///
/// - `is_alive: true` + `quiet_ms` → the staleness sweep marks the synthetic
///   tab stale → `frozen` with a live tab → counted as `idle`.
/// - `is_alive: false` + `exit_code` → the sweep's dead-tab branch sets
///   `sessionStates[tab.id]` to `completed` (exit 0) or `error` (exit != 0).
pub use crate::terminal::transcript::InjectedTabSpec;

/// Project a `TestSession` into the on-the-wire `TranscriptSession` shape
/// that `transcript_list_sessions` returns to the frontend.
///
/// The frontend filters out transcripts with `message_count == 0` (the
/// `message_count > 0` filter in `useSessionManager.ts`), so we hand back `1`
/// to ensure the fake renders.
///
/// Two projection modes:
///
/// - **non-tab-backed** (`tab_backed == false`): set `injected_live_status`
///   to the kebab string. `useSessionManager` consults this override instead
///   of computing live-status from tab correlation. Reaches `working`
///   (`active-in-zone`), `needs-input`, and the orphan-demo `frozen` plus the
///   short-circuit `error` / `completed` buckets.
/// - **tab-backed** (`tab_backed == true`): leave `injected_live_status` as
///   `None` so the frontend takes the REAL tab-correlation path, and emit an
///   `injected_tab` spec from which the frontend derives a synthetic tab.
///   Reaches `idle` (pre-aged stale tab) and tab-backed `error` / `completed`
///   (dead-tab exit-code).
pub fn project_test_session(
    session: &TestSession,
) -> crate::terminal::transcript::TranscriptSession {
    let (injected_live_status, injected_tab) = if session.tab_backed {
        let spec = match session.live_status {
            TestLiveStatus::Idle => InjectedTabSpec {
                is_alive: true,
                exit_code: None,
                quiet_ms: Some(
                    session
                        .quiet_ms
                        .unwrap_or(IDLE_DEFAULT_QUIET_MS)
                        .max(IDLE_DEFAULT_QUIET_MS),
                ),
            },
            TestLiveStatus::Error => InjectedTabSpec {
                is_alive: false,
                exit_code: Some(1),
                quiet_ms: None,
            },
            TestLiveStatus::Completed => InjectedTabSpec {
                is_alive: false,
                exit_code: Some(0),
                quiet_ms: None,
            },
            // The handler rejects tab_backed for ActiveInZone / NeedsInput /
            // Frozen before we ever get here; treat as a no-op spec to keep
            // this total without an unreachable!() panic on the wire path.
            TestLiveStatus::ActiveInZone | TestLiveStatus::NeedsInput | TestLiveStatus::Frozen => {
                InjectedTabSpec {
                    is_alive: true,
                    exit_code: None,
                    quiet_ms: None,
                }
            }
        };
        (None, Some(spec))
    } else {
        (
            Some(session.live_status.as_frontend_str().to_string()),
            None,
        )
    };

    crate::terminal::transcript::TranscriptSession {
        session_id: session.session_id.clone(),
        project_path: session.project_path.clone(),
        config_dir: session.config_dir.clone(),
        // `> 0` so the frontend's `message_count > 0` filter doesn't drop the
        // fake.
        message_count: 1,
        last_modified: session.injected_at.clone(),
        started_at: Some(session.injected_at.clone()),
        first_message_preview: Some(session.name.clone()),
        has_plans: false,
        display_name: session.name.clone(),
        injected_live_status,
        injected_tab,
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

/// TTL for an injected fake before lazy eviction removes it. Manual UI test
/// runs are short-lived; a 10-minute ceiling means a forgotten injection from
/// an abandoned test can't linger and pollute a later poll, while still being
/// generous enough that a human poking at the UI by hand never races it.
const SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Evict entries older than `SESSION_TTL` from an already-locked registry map.
///
/// Lazy eviction on access (called from every public read/write) instead of a
/// background sweep thread: this is a debug-only module, and spawning a tokio
/// task (or std thread) purely to GC a handful of manual-test fakes would add
/// a long-lived runtime fixture to a cfg-gated test seam for no real benefit.
/// Bounded write volume means the linear scan here is trivially cheap, and
/// "evict on touch" guarantees a stale entry is gone before anyone observes it
/// (the only observers are the same read/write paths that run this first).
///
/// An entry whose `injected_at` fails to parse as RFC3339 is treated as
/// non-expirable (kept) rather than silently dropped — every insert path here
/// stamps a valid RFC3339 string, so an unparseable value would signal a bug
/// we'd rather surface as a lingering entry than mask via eviction.
fn evict_expired(map: &mut HashMap<String, TestSession>) {
    let now = chrono::Utc::now();
    map.retain(
        |_, session| match chrono::DateTime::parse_from_rfc3339(&session.injected_at) {
            Ok(injected) => {
                let age = now.signed_duration_since(injected.with_timezone(&chrono::Utc));
                match age.to_std() {
                    // Positive age within TTL → keep.
                    Ok(elapsed) => elapsed < SESSION_TTL,
                    // Negative age (clock skew / future timestamp) → keep; it's
                    // not stale yet by any reasonable reading.
                    Err(_) => true,
                }
            }
            Err(_) => true,
        },
    );
}

/// Read-only snapshot of every currently-injected fake.
///
/// Public so a future patch can merge these into `transcript_list_sessions`
/// (or surface them through a parallel command) without re-importing the
/// internal `Mutex`. Returns an empty `Vec` when the registry hasn't been
/// touched yet.
///
/// Evicts TTL-expired entries first so a stale fake can never appear in the
/// merged transcript list.
pub fn injected_test_sessions() -> Vec<TestSession> {
    let mut guard = registry().lock().unwrap_or_else(|p| p.into_inner());
    evict_expired(&mut guard);
    guard.values().cloned().collect()
}

/// AppHandle captured at router-merge time (`mcp_api::create_router`) so the
/// mutating endpoints can emit `test-fixtures-injected-changed`.
///
/// `useTranscriptSessions` listens for that event and refetches immediately —
/// its 30s auto-refresh poll is visibility-gated (a hidden/backgrounded
/// window never ticks), so without the event a seeded or cleared scenario
/// would stay invisible to an acceptance driver until the window regains
/// focus. Unit tests never call `set_app_handle`, so the emit is a no-op
/// there by construction.
fn app_handle_slot() -> &'static OnceLock<tauri::AppHandle> {
    static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();
    &APP_HANDLE
}

/// Capture the AppHandle for change-event emission. Idempotent — only the
/// first call wins (create_router runs once per process).
pub fn set_app_handle(handle: tauri::AppHandle) {
    let _ = app_handle_slot().set(handle);
}

/// Best-effort `test-fixtures-injected-changed` emit after a registry
/// mutation. Never fails the request — the event is a freshness hint, not
/// part of the route contract.
fn emit_injected_changed(action: &str, count: usize) {
    if let Some(handle) = app_handle_slot().get() {
        use tauri::Emitter;
        let _ = handle.emit(
            "test-fixtures-injected-changed",
            serde_json::json!({ "action": action, "count": count }),
        );
    }
}

/// Core insert path shared by `inject_session_handler` and the
/// scenario-seeder. Validates the request, builds the `TestSession`, runs TTL
/// eviction, and inserts under `task_run_id`. Returns the stored
/// `(task_run_id, session_id)` on success.
///
/// Factored out so there is exactly ONE place that validates + builds + stamps
/// + inserts a fake — the seed-scenario route reuses this rather than
/// duplicating the build/insert logic.
fn insert_session(
    req: InjectSessionRequest,
) -> Result<(String, String), (StatusCode, InjectSessionError)> {
    if req.task_run_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            InjectSessionError {
                success: false,
                error: "task_run_id must not be empty".to_string(),
            },
        ));
    }
    if req.name.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            InjectSessionError {
                success: false,
                error: "name must not be empty".to_string(),
            },
        ));
    }

    // `idle` is only reachable tab-backed: the short-circuit path forces an
    // injected `frozen` to be orphaned, which `computeStatusCounts` drops, so
    // a tab-less idle can never land in the idle bucket.
    if matches!(req.live_status, TestLiveStatus::Idle) && !req.tab_backed {
        return Err((
            StatusCode::BAD_REQUEST,
            InjectSessionError {
                success: false,
                error: "liveStatus=idle requires tab_backed=true: the orphan filter \
                        drops a tab-less injected frozen, so idle is only reachable via \
                        a synthetic stale tab"
                    .to_string(),
            },
        ));
    }

    // tab-backed only has a defined mechanism for idle / error / completed.
    // working (active-in-zone) / needs-input / frozen stay on the legacy
    // short-circuit (frozen is the orphan-demo path).
    if req.tab_backed
        && !matches!(
            req.live_status,
            TestLiveStatus::Idle | TestLiveStatus::Error | TestLiveStatus::Completed
        )
    {
        return Err((
            StatusCode::BAD_REQUEST,
            InjectSessionError {
                success: false,
                error: "tab_backed=true is only valid for liveStatus in \
                        {idle, error, completed}; active-in-zone / needs-input / frozen \
                        have no tab-backed mechanism and stay on the short-circuit path"
                    .to_string(),
            },
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
        tab_backed: req.tab_backed,
        quiet_ms: req.quiet_ms,
    };

    let task_run_id = session.task_run_id.clone();
    let session_id = session.session_id.clone();

    {
        let mut guard = registry().lock().unwrap_or_else(|p| p.into_inner());
        evict_expired(&mut guard);
        guard.insert(task_run_id.clone(), session);
    }

    info!(
        "test_fixtures: injected session task_run_id={} liveStatus={} name=\"{}\"",
        task_run_id,
        req.live_status.as_frontend_str(),
        req.name,
    );

    Ok((task_run_id, session_id))
}

/// Drop ALL injected fakes. Shared core for `/clear-sessions` and
/// `/clear-injected` (the latter is a superset alias). Returns the number of
/// entries removed.
fn clear_all_sessions() -> usize {
    let mut guard = registry().lock().unwrap_or_else(|p| p.into_inner());
    let n = guard.len();
    guard.clear();
    info!(
        "test_fixtures: cleared {} injected session(s); real SessionManager untouched",
        n,
    );
    n
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
    /// When true, project the fake WITHOUT an `injected_live_status` override
    /// so the frontend takes the real tab-correlation path against a derived
    /// synthetic tab. Required to reach the `idle` bucket and the tab-backed
    /// `error` / `completed` buckets; rejected for `active-in-zone` /
    /// `needs-input` / `frozen` (those have no tab-backed mechanism).
    #[serde(default)]
    pub tab_backed: bool,
    /// Pre-age (ms) for a tab-backed `idle` fake's synthetic tab. Ignored for
    /// non-idle statuses. Defaults to (and is floored at) 61s so the existing
    /// 60s staleness sweep classifies the synthetic tab stale.
    #[serde(default)]
    pub quiet_ms: Option<u64>,
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
    match insert_session(req) {
        Ok((task_run_id, session_id)) => {
            emit_injected_changed("inject", 1);
            Ok(Json(InjectSessionResponse {
                success: true,
                task_run_id,
                session_id,
            }))
        }
        Err((code, err)) => Err((code, Json(err))),
    }
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
    let cleared_count = clear_all_sessions();
    emit_injected_changed("clear", cleared_count);
    Json(ClearSessionsResponse {
        success: true,
        cleared_count,
    })
}

// =============================================================================
// /ui-bridge/test/clear-injected
// =============================================================================

/// Drop ALL injected fakes. A superset alias of `/clear-sessions` named to
/// match the scenario-seeder's clear-then-seed vocabulary. The derived
/// synthetic tabs disappear automatically on the next `transcript_list_sessions`
/// poll: `deriveSyntheticTabs` (syntheticTabs.ts) iterates the transcript list
/// and only emits a tab when a session carries an `injected_tab`, so once the
/// fakes are gone from the registry, `merge_with_injected` stops projecting
/// them and the synthetic tabs cease to exist with no separate teardown.
async fn clear_injected_handler() -> Json<ClearSessionsResponse> {
    let cleared_count = clear_all_sessions();
    emit_injected_changed("clear", cleared_count);
    Json(ClearSessionsResponse {
        success: true,
        cleared_count,
    })
}

// =============================================================================
// /ui-bridge/test/seed-terminal-scenario
// =============================================================================

/// Per-bucket counts for the declarative scenario seeder. Every field is
/// optional and defaults to 0, so a body of `{ "working": 3 }` seeds exactly
/// three working fakes and nothing else.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScenarioCounts {
    #[serde(default)]
    pub working: u32,
    #[serde(default)]
    pub idle: u32,
    #[serde(default)]
    pub needs_input: u32,
    #[serde(default)]
    pub error: u32,
    #[serde(default)]
    pub completed: u32,
}

impl ScenarioCounts {
    fn is_empty(&self) -> bool {
        self.working == 0
            && self.idle == 0
            && self.needs_input == 0
            && self.error == 0
            && self.completed == 0
    }
}

/// Seed-scenario request body. Either declarative `counts` OR an explicit
/// `sessions` list (`InjectSessionRequest`-shaped), never both.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeedScenarioRequest {
    /// Per-bucket counts. Mutually exclusive with `sessions`.
    #[serde(default, flatten)]
    pub counts: ScenarioCounts,
    /// Explicit session list. Mutually exclusive with `counts`.
    #[serde(default)]
    pub sessions: Option<Vec<InjectSessionRequest>>,
}

/// Per-bucket seeded tally echoed back in the response.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SeededCounts {
    pub working: u32,
    pub idle: u32,
    pub needs_input: u32,
    pub error: u32,
    pub completed: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeedScenarioResponse {
    pub success: bool,
    pub cleared_count: usize,
    pub seeded: SeededCounts,
    pub session_ids: Vec<String>,
}

/// Default quiet pre-age (ms) for a scenario-seeded `idle` fake. Matches the
/// inject-session idle floor so the staleness sweep marks the synthetic tab
/// stale.
const SCENARIO_IDLE_QUIET_MS: u64 = 61_000;

/// Build the `InjectSessionRequest` list for a counts-based scenario. Ids and
/// names are deterministic (`scenario-<bucket>-<n>`) so a test can assert exact
/// membership.
fn requests_from_counts(counts: &ScenarioCounts) -> Vec<InjectSessionRequest> {
    let mut reqs = Vec::new();

    let mut push = |bucket: &str,
                    n: u32,
                    live_status: TestLiveStatus,
                    tab_backed: bool,
                    quiet_ms: Option<u64>| {
        for i in 1..=n {
            let id = format!("scenario-{bucket}-{i}");
            reqs.push(InjectSessionRequest {
                task_run_id: id.clone(),
                name: format!("Scenario {bucket} {i}"),
                live_status,
                worktree: None,
                project_path: None,
                config_dir: None,
                tab_backed,
                quiet_ms,
            });
        }
    };

    // working → active-in-zone short-circuit.
    push(
        "working",
        counts.working,
        TestLiveStatus::ActiveInZone,
        false,
        None,
    );
    // needs_input → needs-input short-circuit.
    push(
        "needs-input",
        counts.needs_input,
        TestLiveStatus::NeedsInput,
        false,
        None,
    );
    // idle → tab-backed Idle (pre-aged stale tab).
    push(
        "idle",
        counts.idle,
        TestLiveStatus::Idle,
        true,
        Some(SCENARIO_IDLE_QUIET_MS),
    );
    // error → tab-backed Error (dead tab, exit != 0).
    push("error", counts.error, TestLiveStatus::Error, true, None);
    // completed → tab-backed Completed (dead tab, exit 0).
    push(
        "completed",
        counts.completed,
        TestLiveStatus::Completed,
        true,
        None,
    );

    reqs
}

async fn seed_scenario_handler(
    Json(req): Json<SeedScenarioRequest>,
) -> Result<Json<SeedScenarioResponse>, (StatusCode, Json<InjectSessionError>)> {
    let has_counts = !req.counts.is_empty();
    let has_sessions = req.sessions.as_ref().is_some_and(|s| !s.is_empty());

    if has_counts && has_sessions {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(InjectSessionError {
                success: false,
                error: "supply either per-bucket counts OR an explicit `sessions` list, \
                        not both"
                    .to_string(),
            }),
        ));
    }

    // Track the per-bucket tally for the counts path so the response echoes
    // what was requested.
    let seeded = SeededCounts {
        working: req.counts.working,
        idle: req.counts.idle,
        needs_input: req.counts.needs_input,
        error: req.counts.error,
        completed: req.counts.completed,
    };

    let requests = if has_sessions {
        req.sessions.unwrap_or_default()
    } else {
        // Empty body or counts body both flow here; an all-zero counts body
        // is a valid "clear to empty" scenario.
        requests_from_counts(&req.counts)
    };

    // Clear-then-seed by contract: drop EVERY injected fake first, then seed
    // exactly the requested scenario. The clear count is the pre-existing
    // population so callers can confirm the reset happened.
    let cleared_count = clear_all_sessions();

    let mut session_ids = Vec::with_capacity(requests.len());
    for r in requests {
        // Reuse the Phase-1 insert primitive — one validate/build/insert path.
        match insert_session(r) {
            Ok((task_run_id, _session_id)) => session_ids.push(task_run_id),
            Err((code, err)) => return Err((code, Json(err))),
        }
    }

    info!(
        "test_fixtures: seeded scenario cleared={} seeded={} ids={:?}",
        cleared_count,
        session_ids.len(),
        session_ids,
    );

    emit_injected_changed("seed", session_ids.len());

    Ok(Json(SeedScenarioResponse {
        success: true,
        cleared_count,
        seeded: if has_sessions {
            // For the explicit-sessions path we don't classify per bucket;
            // report zeros and rely on `session_ids` for the actual set.
            SeededCounts::default()
        } else {
            seeded
        },
        session_ids,
    }))
}

// =============================================================================
// /ui-bridge/test/seed-lifecycle-store  (+ clear, + list-open read-back)
// =============================================================================
//
// The StatusStrip seam above seeds the *transcript-list* render path. It has no
// way to exercise the session-RESTORE path, which reads the durable
// `SessionLifecycleStore` (`<home>/.qontinui/runner/terminal-sessions[-<port>].json`)
// at boot and resurrects every restorable row. Restore tests otherwise have to
// hand-write a port-namespaced JSON file with exact camelCase keys and compute
// the anchor/last-seen offset math by hand. These routes give them a seam:
//
//   - POST /ui-bridge/test/seed-lifecycle-store  — write a port-namespaced
//     store from a JSON body of `{ records: [ {sessionId, state, lastSeenOffsetMs,
//     closeReason?, ...} ] }`. `lastSeenOffsetMs` is relative to "now" (negative
//     = the past), so a body can place open/ghost/closed rows at precise ages
//     without the caller knowing the wall clock. 400 on a malformed body.
//   - POST /ui-bridge/test/clear-lifecycle-store — delete the port-namespaced
//     store file. Returns whether a file was removed.
//   - POST /ui-bridge/test/list-lifecycle-open — read the port-namespaced store
//     back and return the `state == "open"` session ids (the StatusStrip
//     restore consumer's input), so a test can assert the seed round-tripped
//     without reaching into the filesystem.
//
// The store path is namespaced by the runner's actually-bound API port,
// mirroring `main.rs`'s `lifecycle_store` wiring (port 9876 → base name, every
// other port → `-<port>` suffix), so a temp runner seeds/reads its OWN file
// and never the primary's live sessions.

use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
use std::path::Path;

/// One record in a seed-lifecycle-store body. Timestamps are expressed as an
/// offset from "now" in millis (`last_seen_offset_ms`, negative = the past) so
/// a caller can place a row at a precise age without knowing the wall clock;
/// the handler resolves them against `Utc::now()` at write time.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedLifecycleRecord {
    pub session_id: String,
    /// `"open"` | `"closed"`.
    pub state: String,
    /// Offset from now (millis); negative places the touch/close in the past.
    #[serde(default)]
    pub last_seen_offset_ms: i64,
    /// Offset from now (millis) for `closed_at`; only meaningful when
    /// `state == "closed"`. Defaults to `last_seen_offset_ms`.
    #[serde(default)]
    pub closed_at_offset_ms: Option<i64>,
    /// Close reason (`"pty-exit"` / `"poll-dead"` / `"explicit"` / …). Only
    /// meaningful when `state == "closed"`.
    #[serde(default)]
    pub close_reason: Option<String>,
    /// Optional grid placement; defaults keep the row renderable.
    #[serde(default)]
    pub page_id: Option<String>,
    #[serde(default)]
    pub zone_index: Option<i32>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedLifecycleRequest {
    pub records: Vec<SeedLifecycleRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeedLifecycleResponse {
    pub success: bool,
    pub seeded: usize,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListLifecycleOpenResponse {
    pub success: bool,
    pub open_session_ids: Vec<String>,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClearLifecycleResponse {
    pub success: bool,
    pub removed: bool,
    pub path: String,
}

/// Resolve the port-namespaced lifecycle-store path for the runner bound to
/// `api_port`, mirroring `main.rs`'s wiring exactly: port 9876 → base name,
/// any other port → `-<port>` suffix; under `<home>/.qontinui/runner`.
fn lifecycle_store_path_for_port(api_port: u16) -> std::path::PathBuf {
    let file_name = if api_port == 9876 {
        "terminal-sessions.json".to_string()
    } else {
        format!("terminal-sessions-{}.json", api_port)
    };
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
        .join(file_name)
}

/// Build a full `TerminalSessionRecord` from a seed spec resolved against
/// `now_ms`. Returns an error string on an invalid `state`.
fn record_from_seed(
    seed: &SeedLifecycleRecord,
    now_ms: i64,
) -> Result<TerminalSessionRecord, String> {
    if seed.session_id.trim().is_empty() {
        return Err("each record needs a non-empty sessionId".to_string());
    }
    let state = match seed.state.as_str() {
        "open" | "closed" => seed.state.clone(),
        other => {
            return Err(format!(
                "record state must be \"open\" or \"closed\", got {other:?}"
            ))
        }
    };
    let last_seen_at = now_ms + seed.last_seen_offset_ms;
    let (closed_at, close_reason) = if state == "closed" {
        let closed_at = now_ms + seed.closed_at_offset_ms.unwrap_or(seed.last_seen_offset_ms);
        (Some(closed_at), seed.close_reason.clone())
    } else {
        (None, None)
    };
    Ok(TerminalSessionRecord {
        claude_session_id: seed.session_id.clone(),
        config_dir: None,
        working_dir: seed.working_dir.clone().or(Some("C:/repo".to_string())),
        page_id: seed
            .page_id
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        zone_index: seed.zone_index.unwrap_or(0),
        title: seed.title.clone().or_else(|| Some(seed.session_id.clone())),
        terminal_id: format!("term-{}", seed.session_id),
        opened_at: last_seen_at - 1_000,
        last_seen_at,
        state,
        closed_at,
        close_reason,
        bind_origin: None,
        restore_pending_at: None,
    })
}

/// Core seed logic, path-injectable so it's unit-testable without an
/// `ApiState`. Writes the port-namespaced map file the store reads on
/// `open()` directly (clear-then-seed: the whole file is overwritten) so the
/// caller's precise now-relative ages survive — `record_open` would re-stamp
/// them. Returns the number of records written, or a `(status, message)` error.
fn seed_lifecycle_store_at(
    path: &Path,
    req: &SeedLifecycleRequest,
    now_ms: i64,
) -> Result<usize, (StatusCode, String)> {
    if req.records.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "seed body must contain at least one record".to_string(),
        ));
    }
    let mut records = Vec::with_capacity(req.records.len());
    for seed in &req.records {
        match record_from_seed(seed, now_ms) {
            Ok(rec) => records.push(rec),
            Err(e) => return Err((StatusCode::BAD_REQUEST, e)),
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to create lifecycle store dir: {e}"),
            )
        })?;
    }
    let map: HashMap<String, TerminalSessionRecord> = records
        .into_iter()
        .map(|r| (r.claude_session_id.clone(), r))
        .collect();
    let seeded = map.len();
    let bytes = serde_json::to_vec_pretty(&map).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize seeded store: {e}"),
        )
    })?;
    std::fs::write(path, &bytes).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to write lifecycle store: {e}"),
        )
    })?;
    Ok(seeded)
}

/// Core read-back: the `state == "open"` session ids in the store at `path`
/// (sorted). A missing/unreadable store reads as empty.
fn list_lifecycle_open_at(path: &Path) -> Vec<String> {
    match SessionLifecycleStore::open(path) {
        Ok(store) => {
            let mut ids: Vec<String> = store
                .open_records()
                .into_iter()
                .map(|r| r.claude_session_id)
                .collect();
            ids.sort();
            ids
        }
        Err(_) => Vec::new(),
    }
}

async fn seed_lifecycle_store_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
    Json(req): Json<SeedLifecycleRequest>,
) -> Result<Json<SeedLifecycleResponse>, (StatusCode, Json<InjectSessionError>)> {
    let api_port = crate::mcp::types::runner_api_port(&state.app_state);
    let path = lifecycle_store_path_for_port(api_port);
    let now_ms = chrono::Utc::now().timestamp_millis();
    match seed_lifecycle_store_at(&path, &req, now_ms) {
        Ok(seeded) => {
            info!(
                "test_fixtures: seeded lifecycle store path={} records={}",
                path.display(),
                seeded,
            );
            Ok(Json(SeedLifecycleResponse {
                success: true,
                seeded,
                path: path.display().to_string(),
            }))
        }
        Err((code, error)) => Err((
            code,
            Json(InjectSessionError {
                success: false,
                error,
            }),
        )),
    }
}

async fn list_lifecycle_open_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> Json<ListLifecycleOpenResponse> {
    let api_port = crate::mcp::types::runner_api_port(&state.app_state);
    let path = lifecycle_store_path_for_port(api_port);
    Json(ListLifecycleOpenResponse {
        success: true,
        open_session_ids: list_lifecycle_open_at(&path),
        path: path.display().to_string(),
    })
}

async fn clear_lifecycle_store_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> Json<ClearLifecycleResponse> {
    let api_port = crate::mcp::types::runner_api_port(&state.app_state);
    let path = lifecycle_store_path_for_port(api_port);
    let removed = std::fs::remove_file(&path).is_ok();
    info!(
        "test_fixtures: cleared lifecycle store path={} removed={}",
        path.display(),
        removed,
    );
    Json(ClearLifecycleResponse {
        success: true,
        removed,
        path: path.display().to_string(),
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
        .route(
            "/ui-bridge/test/clear-injected",
            post(clear_injected_handler),
        )
        .route(
            "/ui-bridge/test/seed-terminal-scenario",
            post(seed_scenario_handler),
        )
        .route(
            "/ui-bridge/test/seed-lifecycle-store",
            post(seed_lifecycle_store_handler),
        )
        .route(
            "/ui-bridge/test/list-lifecycle-open",
            post(list_lifecycle_open_handler),
        )
        .route(
            "/ui-bridge/test/clear-lifecycle-store",
            post(clear_lifecycle_store_handler),
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

    // =========================================================================
    // Phase 3 / F2: release-build gate regression canaries.
    //
    // A full `cargo build --release` (no `test-fixtures`) that asserts the seam
    // is absent from the binary costs ~30 min and this repo's CI deliberately
    // ships NO release leg (ci.yml builds `--debug --no-bundle`). So instead of
    // a binary-string grep we anchor the gate at SOURCE level: these tests
    // `include_str!` the three files that carry the cfg gate and assert each
    // anchor is still present, in the right place. They FAIL the moment anyone
    // deletes the `#![cfg(...)]` from this module, the `#[cfg(...)]` from the
    // `pub mod test_fixtures` declaration in `mcp/mod.rs`, or the `#[cfg(...)]`
    // from the `routes()` merge in `mcp_api.rs` — which is exactly the
    // regression that would leak the debug seam into a release build.
    //
    // These compile only inside the already-cfg-gated module, so they vanish
    // from a release build alongside the seam they protect — but CI runs the
    // debug test leg, where they execute every PR. A complementary CI step
    // (ci.yml `seam-gate` job) greps the same anchors so the assertion holds
    // even if this whole module is ever removed.

    /// The cfg gate must be the FIRST non-comment, non-blank line of this
    /// module. An inner attribute (`#![cfg(...)]`) only gates the whole module
    /// when it precedes every item; if a refactor moves a `use` or item above
    /// it, the inner attribute becomes a hard compile error — but a subtler
    /// regression (someone replacing `#![cfg(...)]` with a narrower
    /// `#[cfg(test)]` on the tests only, leaving the module ungated) would
    /// silently ship the seam. Pin the exact line.
    #[test]
    fn module_cfg_gate_is_first_non_comment_line() {
        let src = include_str!("test_fixtures.rs");
        let first_significant = src
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("//"))
            .expect("module must have a non-comment line");
        assert_eq!(
            first_significant, r#"#![cfg(any(debug_assertions, feature = "test-fixtures"))]"#,
            "the module-level cfg gate must remain the first significant line of \
             test_fixtures.rs — without it the debug seam compiles into release builds",
        );
    }

    /// The `pub mod test_fixtures;` declaration in `mcp/mod.rs` must carry the
    /// matching cfg attribute on the line immediately above it.
    #[test]
    fn mod_declaration_is_cfg_gated() {
        let src = include_str!("mod.rs");
        let lines: Vec<&str> = src.lines().map(str::trim).collect();
        let decl_idx = lines
            .iter()
            .position(|l| *l == "pub mod test_fixtures;")
            .expect("mcp/mod.rs must declare `pub mod test_fixtures;`");
        assert!(
            decl_idx > 0
                && lines[decl_idx - 1]
                    == r#"#[cfg(any(debug_assertions, feature = "test-fixtures"))]"#,
            "`pub mod test_fixtures;` in mcp/mod.rs must be immediately preceded by the \
             cfg gate — without it the module ships in release builds",
        );
    }

    /// The `routes()` merge in `mcp_api.rs` must carry the matching cfg
    /// attribute on the line immediately above it.
    #[test]
    fn mcp_api_routes_merge_is_cfg_gated() {
        let src = include_str!("../mcp_api.rs");
        let lines: Vec<&str> = src.lines().map(str::trim).collect();
        let merge_idx = lines
            .iter()
            .position(|l| l.contains("crate::mcp::test_fixtures::routes()"))
            .expect("mcp_api.rs must merge the test-fixtures routes");
        assert!(
            merge_idx > 0
                && lines[merge_idx - 1]
                    == r#"#[cfg(any(debug_assertions, feature = "test-fixtures"))]"#,
            "the test_fixtures::routes() merge in mcp_api.rs must be immediately preceded \
             by the cfg gate — without it the debug routes mount in release builds",
        );
    }

    /// Regression canary: every `/ui-bridge/test/*` route this seam exposes
    /// must stay wired in `routes()`. Asserting the literal path strings appear
    /// in this module's source guards against a silent route deletion that
    /// would break the acceptance harness without any compile error.
    #[test]
    fn all_test_routes_remain_wired() {
        let src = include_str!("test_fixtures.rs");
        for route in [
            "/ui-bridge/test/inject-session",
            "/ui-bridge/test/clear-sessions",
            "/ui-bridge/test/clear-injected",
            "/ui-bridge/test/seed-terminal-scenario",
            "/ui-bridge/test/seed-lifecycle-store",
            "/ui-bridge/test/list-lifecycle-open",
            "/ui-bridge/test/clear-lifecycle-store",
        ] {
            assert!(
                src.contains(&format!("\"{route}\"")),
                "route {route} must remain wired in test_fixtures::routes()",
            );
        }
    }

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
            tab_backed: false,
            quiet_ms: None,
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
            tab_backed: false,
            quiet_ms: None,
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
            tab_backed: false,
            quiet_ms: None,
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
            injected_tab: None,
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

    /// Build a `TestSession` directly (bypassing the HTTP handler) so the
    /// projection can be exercised per (status, tab_backed) combination
    /// without touching the global registry.
    fn make_session(
        live_status: TestLiveStatus,
        tab_backed: bool,
        quiet_ms: Option<u64>,
    ) -> TestSession {
        TestSession {
            task_run_id: "proj-test".to_string(),
            session_id: "proj-test".to_string(),
            name: "Projection test".to_string(),
            live_status,
            worktree: None,
            injected_at: "2026-06-05T00:00:00Z".to_string(),
            project_path: DEFAULT_TEST_PROJECT_PATH.to_string(),
            config_dir: DEFAULT_TEST_CONFIG_DIR.to_string(),
            tab_backed,
            quiet_ms,
        }
    }

    /// Non-tab-backed working / needs-input / frozen project to the
    /// short-circuit override with NO synthetic tab.
    #[test]
    fn project_short_circuit_statuses_carry_override_no_tab() {
        for (status, expected) in [
            (TestLiveStatus::ActiveInZone, "active-in-zone"),
            (TestLiveStatus::NeedsInput, "needs-input"),
            (TestLiveStatus::Frozen, "frozen"),
            (TestLiveStatus::Error, "error"),
            (TestLiveStatus::Completed, "completed"),
        ] {
            let projected = project_test_session(&make_session(status, false, None));
            assert_eq!(
                projected.injected_live_status.as_deref(),
                Some(expected),
                "non-tab-backed {status:?} must carry the short-circuit override",
            );
            assert!(
                projected.injected_tab.is_none(),
                "non-tab-backed {status:?} must not emit a synthetic tab",
            );
            assert!(projected.message_count > 0);
        }
    }

    /// Tab-backed idle projects to NO override + a live synthetic tab pre-aged
    /// past the 60s staleness floor.
    #[test]
    fn project_tab_backed_idle_emits_live_pre_aged_tab() {
        let projected = project_test_session(&make_session(TestLiveStatus::Idle, true, None));
        assert!(
            projected.injected_live_status.is_none(),
            "tab-backed fake must take the real correlation path (no override)",
        );
        let spec = projected
            .injected_tab
            .expect("tab-backed idle must emit a synthetic tab");
        assert!(
            spec.is_alive,
            "idle synthetic tab must be alive (gets swept stale)"
        );
        assert_eq!(spec.exit_code, None);
        assert_eq!(
            spec.quiet_ms,
            Some(IDLE_DEFAULT_QUIET_MS),
            "idle default quiet must be the 61s floor",
        );
    }

    /// A caller-supplied quiet below the floor is clamped up to 61s so the
    /// sweep still marks the tab stale; above the floor is honored.
    #[test]
    fn project_tab_backed_idle_clamps_quiet_to_floor() {
        let too_small =
            project_test_session(&make_session(TestLiveStatus::Idle, true, Some(5_000)));
        assert_eq!(
            too_small.injected_tab.unwrap().quiet_ms,
            Some(IDLE_DEFAULT_QUIET_MS),
            "quiet below the 61s floor must be clamped up",
        );
        let large = project_test_session(&make_session(TestLiveStatus::Idle, true, Some(120_000)));
        assert_eq!(
            large.injected_tab.unwrap().quiet_ms,
            Some(120_000),
            "quiet above the floor must be honored",
        );
    }

    /// Tab-backed error / completed project to NO override + a DEAD synthetic
    /// tab carrying the exit code the dead-tab sweep branch maps.
    #[test]
    fn project_tab_backed_error_and_completed_emit_dead_tab() {
        let err = project_test_session(&make_session(TestLiveStatus::Error, true, None));
        assert!(err.injected_live_status.is_none());
        let err_spec = err.injected_tab.expect("error must emit a synthetic tab");
        assert!(!err_spec.is_alive, "error synthetic tab must be dead");
        assert_eq!(
            err_spec.exit_code,
            Some(1),
            "error exit code must be non-zero"
        );
        assert_eq!(err_spec.quiet_ms, None);

        let done = project_test_session(&make_session(TestLiveStatus::Completed, true, None));
        assert!(done.injected_live_status.is_none());
        let done_spec = done
            .injected_tab
            .expect("completed must emit a synthetic tab");
        assert!(!done_spec.is_alive, "completed synthetic tab must be dead");
        assert_eq!(
            done_spec.exit_code,
            Some(0),
            "completed exit code must be zero"
        );
        assert_eq!(done_spec.quiet_ms, None);
    }

    /// `idle` without `tab_backed` is a 400 — the orphan filter would drop it.
    #[tokio::test]
    async fn inject_rejects_idle_without_tab_backed() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let req = InjectSessionRequest {
            task_run_id: "idle-no-tab".to_string(),
            name: "idle no tab".to_string(),
            live_status: TestLiveStatus::Idle,
            worktree: None,
            project_path: None,
            config_dir: None,
            tab_backed: false,
            quiet_ms: None,
        };
        let err = inject_session_handler(Json(req))
            .await
            .expect_err("idle without tab_backed should be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(!err.1.success);
        assert!(err.1.error.contains("idle"));
    }

    /// `tab_backed` for a status with no tab-backed mechanism is a 400.
    #[tokio::test]
    async fn inject_rejects_tab_backed_for_short_circuit_status() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        for status in [
            TestLiveStatus::ActiveInZone,
            TestLiveStatus::NeedsInput,
            TestLiveStatus::Frozen,
        ] {
            let req = InjectSessionRequest {
                task_run_id: "tab-backed-bad".to_string(),
                name: "tab backed bad".to_string(),
                live_status: status,
                worktree: None,
                project_path: None,
                config_dir: None,
                tab_backed: true,
                quiet_ms: None,
            };
            let err = inject_session_handler(Json(req))
                .await
                .expect_err(&format!("tab_backed {status:?} should be rejected"));
            assert_eq!(err.0, StatusCode::BAD_REQUEST);
            assert!(!err.1.success);
        }
    }

    /// Tab-backed idle / error / completed all round-trip through the handler
    /// + registry and project to `injected_live_status: None`.
    #[tokio::test]
    async fn tab_backed_fakes_round_trip_with_no_override() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _ = clear_sessions_handler().await;

        for (status, run_id) in [
            (TestLiveStatus::Idle, "tb-idle"),
            (TestLiveStatus::Error, "tb-error"),
            (TestLiveStatus::Completed, "tb-completed"),
        ] {
            let req = InjectSessionRequest {
                task_run_id: run_id.to_string(),
                name: format!("tab-backed {run_id}"),
                live_status: status,
                worktree: None,
                project_path: None,
                config_dir: None,
                tab_backed: true,
                quiet_ms: None,
            };
            let _ = inject_session_handler(Json(req))
                .await
                .expect("tab-backed inject should succeed");
        }

        let merged = merge_with_injected(Vec::new(), injected_test_sessions());
        for run_id in ["tb-idle", "tb-error", "tb-completed"] {
            let fake = merged
                .iter()
                .find(|s| s.session_id == run_id)
                .unwrap_or_else(|| panic!("{run_id} should be in merged list"));
            assert!(
                fake.injected_live_status.is_none(),
                "tab-backed {run_id} must carry no short-circuit override",
            );
            assert!(
                fake.injected_tab.is_some(),
                "tab-backed {run_id} must carry a synthetic tab spec",
            );
        }

        let _ = clear_sessions_handler().await;
    }

    // =========================================================================
    // Phase 2: seed-scenario / clear-injected / TTL eviction
    // =========================================================================

    /// Counts-based scenario seeds exactly N per bucket with the correct
    /// tab_backed projection for each.
    #[tokio::test]
    async fn seed_scenario_counts_seed_exactly_n_per_bucket() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _ = clear_sessions_handler().await;

        let req = SeedScenarioRequest {
            counts: ScenarioCounts {
                working: 2,
                idle: 1,
                needs_input: 3,
                error: 1,
                completed: 2,
            },
            sessions: None,
        };

        let resp = seed_scenario_handler(Json(req))
            .await
            .expect("counts scenario should seed");
        assert!(resp.success);
        assert_eq!(resp.seeded.working, 2);
        assert_eq!(resp.seeded.idle, 1);
        assert_eq!(resp.seeded.needs_input, 3);
        assert_eq!(resp.seeded.error, 1);
        assert_eq!(resp.seeded.completed, 2);
        // 2 + 1 + 3 + 1 + 2 = 9 total session ids.
        assert_eq!(resp.session_ids.len(), 9);

        let sessions = injected_test_sessions();
        assert_eq!(sessions.len(), 9, "registry must hold exactly 9 fakes");

        // Deterministic ids exist.
        for expected in [
            "scenario-working-1",
            "scenario-working-2",
            "scenario-idle-1",
            "scenario-needs-input-1",
            "scenario-needs-input-2",
            "scenario-needs-input-3",
            "scenario-error-1",
            "scenario-completed-1",
            "scenario-completed-2",
        ] {
            assert!(
                sessions.iter().any(|s| s.task_run_id == expected),
                "{expected} must be seeded",
            );
        }

        // Bucket-correct tab_backed projection.
        let by_id = |id: &str| {
            sessions
                .iter()
                .find(|s| s.task_run_id == id)
                .cloned()
                .unwrap()
        };

        // working / needs-input → short-circuit (not tab-backed).
        assert!(!by_id("scenario-working-1").tab_backed);
        assert_eq!(
            by_id("scenario-working-1").live_status,
            TestLiveStatus::ActiveInZone
        );
        assert!(!by_id("scenario-needs-input-1").tab_backed);
        assert_eq!(
            by_id("scenario-needs-input-1").live_status,
            TestLiveStatus::NeedsInput
        );

        // idle / error / completed → tab-backed.
        let idle = by_id("scenario-idle-1");
        assert!(idle.tab_backed);
        assert_eq!(idle.live_status, TestLiveStatus::Idle);
        assert_eq!(idle.quiet_ms, Some(SCENARIO_IDLE_QUIET_MS));
        assert!(project_test_session(&idle).injected_tab.is_some());
        assert!(project_test_session(&idle).injected_live_status.is_none());

        let error = by_id("scenario-error-1");
        assert!(error.tab_backed);
        assert_eq!(error.live_status, TestLiveStatus::Error);
        let err_spec = project_test_session(&error).injected_tab.unwrap();
        assert!(!err_spec.is_alive);
        assert_eq!(err_spec.exit_code, Some(1));

        let completed = by_id("scenario-completed-1");
        assert!(completed.tab_backed);
        assert_eq!(completed.live_status, TestLiveStatus::Completed);
        let done_spec = project_test_session(&completed).injected_tab.unwrap();
        assert!(!done_spec.is_alive);
        assert_eq!(done_spec.exit_code, Some(0));

        let _ = clear_sessions_handler().await;
    }

    /// Supplying BOTH counts and an explicit `sessions` list is a 400.
    #[tokio::test]
    async fn seed_scenario_both_forms_is_400() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _ = clear_sessions_handler().await;

        let req = SeedScenarioRequest {
            counts: ScenarioCounts {
                working: 1,
                ..Default::default()
            },
            sessions: Some(vec![InjectSessionRequest {
                task_run_id: "explicit-1".to_string(),
                name: "explicit".to_string(),
                live_status: TestLiveStatus::ActiveInZone,
                worktree: None,
                project_path: None,
                config_dir: None,
                tab_backed: false,
                quiet_ms: None,
            }]),
        };

        let err = seed_scenario_handler(Json(req))
            .await
            .expect_err("both forms must be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(!err.1.success);
        // Nothing should have been seeded.
        assert!(injected_test_sessions().is_empty());
    }

    /// Explicit `sessions` form seeds exactly that list (reusing the Phase-1
    /// validation primitive).
    #[tokio::test]
    async fn seed_scenario_explicit_sessions_seed_that_list() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _ = clear_sessions_handler().await;

        let req = SeedScenarioRequest {
            counts: ScenarioCounts::default(),
            sessions: Some(vec![
                InjectSessionRequest {
                    task_run_id: "explicit-a".to_string(),
                    name: "Explicit A".to_string(),
                    live_status: TestLiveStatus::Frozen,
                    worktree: None,
                    project_path: None,
                    config_dir: None,
                    tab_backed: false,
                    quiet_ms: None,
                },
                InjectSessionRequest {
                    task_run_id: "explicit-b".to_string(),
                    name: "Explicit B".to_string(),
                    live_status: TestLiveStatus::Idle,
                    worktree: None,
                    project_path: None,
                    config_dir: None,
                    tab_backed: true,
                    quiet_ms: None,
                },
            ]),
        };

        let resp = seed_scenario_handler(Json(req))
            .await
            .expect("explicit sessions should seed");
        assert_eq!(resp.session_ids.len(), 2);
        let sessions = injected_test_sessions();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().any(|s| s.task_run_id == "explicit-a"));
        assert!(sessions.iter().any(|s| s.task_run_id == "explicit-b"));

        let _ = clear_sessions_handler().await;
    }

    /// Clear-then-seed semantics: a pre-existing injected entry is gone after a
    /// seed of a disjoint scenario.
    #[tokio::test]
    async fn seed_scenario_clears_then_seeds() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _ = clear_sessions_handler().await;

        // Pre-existing entry from a prior inject.
        let _ = inject_session_handler(Json(InjectSessionRequest {
            task_run_id: "pre-existing-1".to_string(),
            name: "Pre-existing".to_string(),
            live_status: TestLiveStatus::ActiveInZone,
            worktree: None,
            project_path: None,
            config_dir: None,
            tab_backed: false,
            quiet_ms: None,
        }))
        .await
        .expect("pre-seed inject should succeed");
        assert!(injected_test_sessions()
            .iter()
            .any(|s| s.task_run_id == "pre-existing-1"));

        // Seed a disjoint scenario.
        let resp = seed_scenario_handler(Json(SeedScenarioRequest {
            counts: ScenarioCounts {
                working: 1,
                ..Default::default()
            },
            sessions: None,
        }))
        .await
        .expect("seed should succeed");
        // The pre-existing entry must be in the cleared tally.
        assert!(resp.cleared_count >= 1);

        let sessions = injected_test_sessions();
        assert!(
            !sessions.iter().any(|s| s.task_run_id == "pre-existing-1"),
            "pre-existing entry must be cleared by clear-then-seed",
        );
        assert!(sessions
            .iter()
            .any(|s| s.task_run_id == "scenario-working-1"));
        assert_eq!(sessions.len(), 1, "only the seeded scenario should remain");

        let _ = clear_sessions_handler().await;
    }

    /// `clear-injected` is a superset alias of `clear-sessions`: it drops every
    /// injected fake and reports the count.
    #[tokio::test]
    async fn clear_injected_roundtrip() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _ = clear_sessions_handler().await;

        for id in ["ci-1", "ci-2"] {
            let _ = inject_session_handler(Json(InjectSessionRequest {
                task_run_id: id.to_string(),
                name: format!("clear-injected {id}"),
                live_status: TestLiveStatus::ActiveInZone,
                worktree: None,
                project_path: None,
                config_dir: None,
                tab_backed: false,
                quiet_ms: None,
            }))
            .await
            .expect("inject should succeed");
        }
        assert_eq!(injected_test_sessions().len(), 2);

        let resp = clear_injected_handler().await;
        assert!(resp.success);
        assert_eq!(resp.cleared_count, 2);
        assert!(injected_test_sessions().is_empty());
    }

    /// A session with a back-dated `injected_at` beyond the TTL is evicted on
    /// the next registry read.
    #[tokio::test]
    async fn ttl_evicts_stale_entry_on_next_read() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _ = clear_sessions_handler().await;

        // Insert a fresh entry the normal way.
        let _ = inject_session_handler(Json(InjectSessionRequest {
            task_run_id: "ttl-fresh".to_string(),
            name: "fresh".to_string(),
            live_status: TestLiveStatus::ActiveInZone,
            worktree: None,
            project_path: None,
            config_dir: None,
            tab_backed: false,
            quiet_ms: None,
        }))
        .await
        .expect("inject should succeed");

        // Inject a stale entry directly with a back-dated injected_at (well
        // beyond the 10-min TTL).
        let stale_at = (chrono::Utc::now() - chrono::Duration::minutes(30)).to_rfc3339();
        {
            let mut guard = registry().lock().unwrap_or_else(|p| p.into_inner());
            guard.insert(
                "ttl-stale".to_string(),
                TestSession {
                    task_run_id: "ttl-stale".to_string(),
                    session_id: "ttl-stale".to_string(),
                    name: "stale".to_string(),
                    live_status: TestLiveStatus::ActiveInZone,
                    worktree: None,
                    injected_at: stale_at,
                    project_path: DEFAULT_TEST_PROJECT_PATH.to_string(),
                    config_dir: DEFAULT_TEST_CONFIG_DIR.to_string(),
                    tab_backed: false,
                    quiet_ms: None,
                },
            );
        }

        // The next read evicts the stale entry but keeps the fresh one.
        let sessions = injected_test_sessions();
        assert!(
            sessions.iter().any(|s| s.task_run_id == "ttl-fresh"),
            "fresh entry must survive eviction",
        );
        assert!(
            !sessions.iter().any(|s| s.task_run_id == "ttl-stale"),
            "stale entry must be evicted on read",
        );

        let _ = clear_sessions_handler().await;
    }

    // =========================================================================
    // Item 4: lifecycle-store restore seam (seed → list_open → clear)
    // =========================================================================

    /// Seeding 3 open + 1 ghost(open, far-past) + 1 closed via the path-level
    /// core, then reading back `list_lifecycle_open_at`, returns exactly the 3
    /// fresh open ids; clearing removes the file so the read-back is empty.
    #[test]
    fn seed_lifecycle_store_round_trip_list_open_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions-9999.json");
        let now = chrono::Utc::now().timestamp_millis();

        let req = SeedLifecycleRequest {
            records: vec![
                SeedLifecycleRecord {
                    session_id: "open-a".to_string(),
                    state: "open".to_string(),
                    last_seen_offset_ms: -5_000,
                    closed_at_offset_ms: None,
                    close_reason: None,
                    page_id: None,
                    zone_index: None,
                    title: None,
                    working_dir: None,
                },
                SeedLifecycleRecord {
                    session_id: "open-b".to_string(),
                    state: "open".to_string(),
                    last_seen_offset_ms: -10_000,
                    closed_at_offset_ms: None,
                    close_reason: None,
                    page_id: None,
                    zone_index: None,
                    title: None,
                    working_dir: None,
                },
                SeedLifecycleRecord {
                    session_id: "open-c".to_string(),
                    state: "open".to_string(),
                    last_seen_offset_ms: -1_000,
                    closed_at_offset_ms: None,
                    close_reason: None,
                    page_id: None,
                    zone_index: None,
                    title: None,
                    working_dir: None,
                },
                // A ghost open row aged far into the past — still `state==open`,
                // so list_open returns it (open_records is strict-open; the
                // anchor/restore math is the store's job, exercised elsewhere).
                SeedLifecycleRecord {
                    session_id: "ghost".to_string(),
                    state: "open".to_string(),
                    last_seen_offset_ms: -(72 * 3_600_000),
                    closed_at_offset_ms: None,
                    close_reason: None,
                    page_id: None,
                    zone_index: None,
                    title: None,
                    working_dir: None,
                },
                // A closed row — must NOT appear in list_open.
                SeedLifecycleRecord {
                    session_id: "closed-1".to_string(),
                    state: "closed".to_string(),
                    last_seen_offset_ms: -2_000,
                    closed_at_offset_ms: Some(-2_000),
                    close_reason: Some("pty-exit".to_string()),
                    page_id: None,
                    zone_index: None,
                    title: None,
                    working_dir: None,
                },
            ],
        };

        let seeded = seed_lifecycle_store_at(&path, &req, now).expect("seed should succeed");
        assert_eq!(seeded, 5, "all 5 records written");
        assert!(path.exists(), "store file written at the namespaced path");

        // list_open returns only the open rows (3 fresh + the ghost), never the
        // closed one.
        let open = list_lifecycle_open_at(&path);
        assert_eq!(
            open,
            vec![
                "ghost".to_string(),
                "open-a".to_string(),
                "open-b".to_string(),
                "open-c".to_string(),
            ],
            "list_open returns every open row (incl. ghost), excludes closed"
        );

        // The seeded ages survived exactly (record_open would have re-stamped
        // them); confirm via the store directly.
        let store = SessionLifecycleStore::open(&path).unwrap();
        let recs = store.open_records();
        let ghost = recs
            .iter()
            .find(|r| r.claude_session_id == "ghost")
            .unwrap();
        assert_eq!(
            ghost.last_seen_at,
            now - 72 * 3_600_000,
            "the seeded now-relative age is preserved verbatim"
        );

        // Closed row carries its reason for restore-grace tests.
        let store_path_raw: HashMap<String, TerminalSessionRecord> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            store_path_raw
                .get("closed-1")
                .unwrap()
                .close_reason
                .as_deref(),
            Some("pty-exit"),
        );

        // Clear: deleting the file makes the read-back empty.
        std::fs::remove_file(&path).unwrap();
        assert!(
            list_lifecycle_open_at(&path).is_empty(),
            "after clear the read-back is empty"
        );
    }

    /// A malformed seed body (empty records / bad state) is a 400.
    #[test]
    fn seed_lifecycle_store_rejects_malformed_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions-9999.json");
        let now = chrono::Utc::now().timestamp_millis();

        // Empty records list → 400.
        let empty = SeedLifecycleRequest { records: vec![] };
        let err = seed_lifecycle_store_at(&path, &empty, now).expect_err("empty body rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Bad `state` → 400, and nothing is written.
        let bad_state = SeedLifecycleRequest {
            records: vec![SeedLifecycleRecord {
                session_id: "x".to_string(),
                state: "bogus".to_string(),
                last_seen_offset_ms: 0,
                closed_at_offset_ms: None,
                close_reason: None,
                page_id: None,
                zone_index: None,
                title: None,
                working_dir: None,
            }],
        };
        let err = seed_lifecycle_store_at(&path, &bad_state, now).expect_err("bad state rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            !path.exists(),
            "a rejected seed must not write the store file"
        );
    }
}
