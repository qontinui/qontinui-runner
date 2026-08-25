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
//! Every body below is COMPLETE and POSTs as-is. `task_run_id` and `name`
//! are REQUIRED by [`InjectSessionRequest`] (no `#[serde(default)]`), so the
//! abbreviated `{liveStatus:"…"}` forms this table used to show were not
//! bodies at all — copy-pasting one produced a 422/400 on deserialize before
//! any fixture logic ran, and the failure named a missing field rather than
//! the doc that omitted it. Give each fake a distinct `task_run_id`; the
//! runner uses it as the session id too.
//!
//! | StatusStrip bucket | `liveStatus` | mechanism | inject body |
//! |--------------------|--------------|-----------|-------------|
//! | working            | `active-in-zone` | short-circuit (`injected_live_status`) | `{"task_run_id":"fx-working","name":"fx working","liveStatus":"active-in-zone"}` |
//! | needs-input        | `needs-input`    | short-circuit | `{"task_run_id":"fx-needs-input","name":"fx needs input","liveStatus":"needs-input"}` |
//! | idle               | `frozen` (live tab) | **tab-backed**: a synthetic tab pre-aged past the 60s staleness sweep | `{"task_run_id":"fx-idle","name":"fx idle","liveStatus":"idle","tab_backed":true}` |
//! | error              | `error`          | **tab-backed**: a dead synthetic tab, exit_code != 0 | `{"task_run_id":"fx-error","name":"fx error","liveStatus":"error","tab_backed":true}` |
//! | completed          | `completed`      | **tab-backed**: a dead synthetic tab, exit_code 0 | `{"task_run_id":"fx-completed","name":"fx completed","liveStatus":"completed","tab_backed":true}` |
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
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

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
        resume_name: None,
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

// =============================================================================
// Injected ERROR EVENTS  (/ui-bridge/test/inject-errors, /seed-error-scenario)
// =============================================================================
//
// ## Why this seam exists
//
// The Error Monitor surface was UNOBSERVABLE to a manual test. Three separate
// iterations failed to verify a defect on `/error-monitor` because there was no
// honest way to put a row in front of it:
//
//   * nothing in `src-tauri/src/` inserted into `error_events` at all, so no
//     runner action could produce one;
//   * the store is a SHARED PostgreSQL instance, so writing to it directly
//     would corrupt every other agent's and the operator's view of the same
//     table — correctly refused;
//   * the only other lever was mutating the GLOBAL log-source settings, which
//     is shared machine state — also correctly refused.
//
// So the seam injects an in-process overlay instead. It touches no database and
// no shared settings: the rows live in this process's memory, are merged into
// the two read commands the page uses, and vanish with `clear-injected` or with
// the process.
//
// ## Contract
//
//   POST /ui-bridge/test/inject-errors
//        { "errors": [ { ...ErrorSpec... }, ... ] }
//        -> { success, injected, ids }
//        ADDITIVE. Appends to whatever is already injected.
//
//   POST /ui-bridge/test/seed-error-scenario
//        { "new": 2, "recurring": 1, "acknowledged": 0, "resolved": 0, "critical": 0 }
//     or { "errors": [ ... ] }                       (mutually exclusive; 400 on both)
//        -> { success, cleared_count, seeded, ids }
//        CLEAR-THEN-SEED, exactly like `seed-terminal-scenario`.
//
//   POST /ui-bridge/test/clear-injected     (the EXISTING route — not a new one)
//        also drops every injected error.
//
// ## Merge points
//
// `error_monitor::commands::query_error_events` and `::get_error_summary`, via
// the same cfg-gated shadowing rebind `transcript_list_sessions` uses for
// injected sessions. Injected rows are APPENDED to the real ones and are
// filtered by the caller's `ErrorQuery` exactly as a real row would be, so a
// filter that excludes them is honest rather than special-cased.
//
// Ids are negative (`-1`, `-2`, …). Real `error_events.id` values come from a
// positive-only `bigserial`, so an injected row can never be confused for a
// stored one, and a caller that tries to `update_error_status` an injected id
// gets a clean PG miss instead of mutating an unrelated real row.

use crate::error_monitor::types::{
    ErrorLocation, ErrorSeverity, ErrorStatus, ErrorSummary, StoredErrorEvent,
};

/// Process-wide overlay of injected error events.
///
/// Same shape as [`registry`]: function-local `OnceLock<Mutex<..>>`, accessed
/// poison-tolerantly. A `Vec` rather than a map — errors have no caller-supplied
/// stable key, and ordering is part of what the page renders.
fn error_registry() -> &'static Mutex<Vec<StoredErrorEvent>> {
    static ERRORS: OnceLock<Mutex<Vec<StoredErrorEvent>>> = OnceLock::new();
    ERRORS.get_or_init(|| Mutex::new(Vec::new()))
}

/// One injected error. Every field is optional except `message`, so the
/// smallest useful body is `{"errors":[{"message":"boom"}]}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InjectErrorRequest {
    pub message: String,
    /// `"critical"` | `"error"` | `"warning"` | `"info"`. Defaults to `error`.
    #[serde(default)]
    pub severity: Option<String>,
    /// `"new"` | `"recurring"` | `"acknowledged"` | `"in_progress"` |
    /// `"resolved"` | `"ignored"` | `"promoted"`. Defaults to `new`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub log_source_name: Option<String>,
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default)]
    pub stack_trace: Option<String>,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub line_number: Option<u32>,
    #[serde(default)]
    pub task_run_id: Option<String>,
    /// Occurrences. A `recurring` fake defaults to 3 so the page's
    /// occurrence-count column has something to render.
    #[serde(default)]
    pub occurrence_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InjectErrorsResponse {
    pub success: bool,
    pub injected: usize,
    pub ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ErrorScenarioCounts {
    #[serde(default)]
    pub new: u32,
    #[serde(default)]
    pub recurring: u32,
    #[serde(default)]
    pub acknowledged: u32,
    #[serde(default)]
    pub resolved: u32,
    #[serde(default)]
    pub critical: u32,
}

impl ErrorScenarioCounts {
    fn is_empty(&self) -> bool {
        self.new == 0
            && self.recurring == 0
            && self.acknowledged == 0
            && self.resolved == 0
            && self.critical == 0
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeedErrorScenarioRequest {
    #[serde(default, flatten)]
    pub counts: ErrorScenarioCounts,
    #[serde(default)]
    pub errors: Option<Vec<InjectErrorRequest>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeedErrorScenarioResponse {
    pub success: bool,
    pub cleared_count: usize,
    pub seeded: usize,
    pub ids: Vec<i64>,
}

/// Build the request list for a counts-based error scenario. Deterministic
/// messages (`Scenario <bucket> <n>`) so a test can assert exact membership.
fn error_requests_from_counts(counts: &ErrorScenarioCounts) -> Vec<InjectErrorRequest> {
    let mut reqs = Vec::new();
    let mut push = |bucket: &str, n: u32, severity: &str, status: &str, occurrences: u32| {
        for i in 1..=n {
            reqs.push(InjectErrorRequest {
                message: format!("Scenario {bucket} {i}"),
                severity: Some(severity.to_string()),
                status: Some(status.to_string()),
                log_source_name: Some("scenario".to_string()),
                error_type: Some(format!("Scenario{}Error", bucket.to_uppercase())),
                stack_trace: None,
                file_path: Some(format!("src/scenario/{bucket}.rs")),
                line_number: Some(i),
                task_run_id: None,
                occurrence_count: Some(occurrences),
            });
        }
    };
    push("new", counts.new, "error", "new", 1);
    push("recurring", counts.recurring, "error", "recurring", 3);
    push(
        "acknowledged",
        counts.acknowledged,
        "warning",
        "acknowledged",
        1,
    );
    push("resolved", counts.resolved, "error", "resolved", 1);
    push("critical", counts.critical, "critical", "new", 1);
    reqs
}

/// Project one request into a full `StoredErrorEvent`.
///
/// `id` is caller-supplied and NEGATIVE (see the module note above).
fn project_injected_error(req: &InjectErrorRequest, id: i64, now: &str) -> StoredErrorEvent {
    let severity = req
        .severity
        .as_deref()
        .and_then(ErrorSeverity::from_str)
        .unwrap_or(ErrorSeverity::Error);
    let status = match req.status.as_deref() {
        Some("recurring") => ErrorStatus::Recurring,
        Some("acknowledged") => ErrorStatus::Acknowledged,
        Some("in_progress") => ErrorStatus::InProgress,
        Some("resolved") => ErrorStatus::Resolved,
        Some("ignored") => ErrorStatus::Ignored,
        Some("promoted") => ErrorStatus::Promoted,
        _ => ErrorStatus::New,
    };
    StoredErrorEvent {
        id,
        log_source_id: None,
        log_source_name: req
            .log_source_name
            .clone()
            .unwrap_or_else(|| "injected".to_string()),
        task_run_id: req.task_run_id.clone(),
        workflow_name: None,
        workflow_step_id: None,
        log_timestamp: Some(now.to_string()),
        captured_at: now.to_string(),
        severity,
        error_type: req.error_type.clone(),
        error_code: None,
        message: req.message.clone(),
        stack_trace: req.stack_trace.clone(),
        context_lines: None,
        raw_entry: Some(req.message.clone()),
        location: req.file_path.as_ref().map(|fp| ErrorLocation {
            file_path: fp.clone(),
            line_number: req.line_number,
            column_number: None,
            function_name: None,
        }),
        signature_hash: format!("injected-{}", id.unsigned_abs()),
        occurrence_count: req.occurrence_count.unwrap_or(1),
        first_seen_at: now.to_string(),
        last_seen_at: now.to_string(),
        status,
        finding_id: None,
        resolved_by_task_run_id: None,
        resolution_notes: None,
        trace_id: None,
        acknowledged_at: None,
        resolved_at: None,
    }
}

/// Core insert path shared by both error routes. Appends and returns the ids.
fn insert_errors(reqs: &[InjectErrorRequest]) -> Vec<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut guard = error_registry().lock().unwrap_or_else(|p| p.into_inner());
    let mut ids = Vec::with_capacity(reqs.len());
    for req in reqs {
        // Ids continue DOWNWARD from the current length, so ids are unique
        // among the CURRENTLY injected set. They restart at -1 after a
        // `clear-injected`, which is intended: the cleared rows no longer
        // exist, and nothing persists an injected id.
        let id = -((guard.len() + 1) as i64);
        guard.push(project_injected_error(req, id, &now));
        ids.push(id);
    }
    ids
}

/// Drop every injected error. Shared by `/clear-injected` and the seeder.
fn clear_all_errors() -> usize {
    let mut guard = error_registry().lock().unwrap_or_else(|p| p.into_inner());
    let n = guard.len();
    guard.clear();
    info!(
        "test_fixtures: cleared {} injected error event(s); the error_events table untouched",
        n,
    );
    n
}

/// Read-only snapshot of the injected errors, for the merge points.
pub fn injected_error_events() -> Vec<StoredErrorEvent> {
    error_registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Append injected errors to a real result set, applying the caller's own
/// filters so an injected row is never MORE visible than a real one would be.
///
/// Appended, never substituted: a fixture that hid real rows would make the
/// page lie about the machine's actual state.
pub fn merge_with_injected_errors(
    mut real: Vec<StoredErrorEvent>,
    query: &crate::error_monitor::types::ErrorQuery,
) -> Vec<StoredErrorEvent> {
    let injected = injected_error_events();
    real.reserve(injected.len());
    for e in injected {
        if let Some(ref tid) = query.task_run_id {
            if e.task_run_id.as_deref() != Some(tid.as_str()) {
                continue;
            }
        }
        if let Some(ref src) = query.log_source_name {
            if &e.log_source_name != src {
                continue;
            }
        }
        if let Some(ref statuses) = query.status {
            if !statuses.is_empty() && !statuses.contains(&e.status) {
                continue;
            }
        }
        if let Some(ref severities) = query.severity {
            if !severities.is_empty() && !severities.contains(&e.severity) {
                continue;
            }
        }
        if let Some(ref et) = query.error_type {
            if e.error_type.as_deref() != Some(et.as_str()) {
                continue;
            }
        }
        real.push(e);
    }
    // Honour `limit` too. The contract above says an injected row is filtered
    // "exactly as a real row would be"; without this a `limit: 50` call could
    // return 53 and a driver asserting on page size would see the seam, not the
    // page.
    if let Some(limit) = query.limit {
        real.truncate(limit as usize);
    }
    real
}

/// Fold the injected errors into a real summary so the page's counters agree
/// with its list. A summary that ignored them would show "0 errors" above a
/// list of three.
pub fn merge_with_injected_summary(mut summary: ErrorSummary) -> ErrorSummary {
    for e in injected_error_events() {
        summary.total += 1;
        let unresolved = matches!(
            e.status,
            ErrorStatus::New
                | ErrorStatus::Recurring
                | ErrorStatus::Acknowledged
                | ErrorStatus::InProgress
                | ErrorStatus::Promoted
        );
        if unresolved {
            summary.unresolved_count += 1;
        }
        // The severity counters are UNRESOLVED-scoped in the real summary
        // (`COUNT(*) FILTER (WHERE severity = ... AND status IN (...))`), so the
        // overlay has to scope them the same way or an injected resolved row
        // would inflate a counter the page reads as "still broken".
        if unresolved {
            match e.severity {
                ErrorSeverity::Critical => summary.critical_count += 1,
                ErrorSeverity::Error => summary.error_count += 1,
                ErrorSeverity::Warning => summary.warning_count += 1,
                ErrorSeverity::Info => {}
            }
        }
        if matches!(e.status, ErrorStatus::New) {
            summary.new_count += 1;
        }
        *summary
            .by_source
            .entry(e.log_source_name.clone())
            .or_insert(0) += 1;
        if let Some(ref et) = e.error_type {
            *summary.by_error_type.entry(et.clone()).or_insert(0) += 1;
        }
        *summary
            .by_status
            .entry(e.status.as_str().to_string())
            .or_insert(0) += 1;
    }
    summary.has_actionable_errors =
        summary.has_actionable_errors || summary.critical_count > 0 || summary.error_count > 0;
    summary
}

async fn inject_errors_handler(
    Json(req): Json<InjectErrorsRequestBody>,
) -> Result<Json<InjectErrorsResponse>, (StatusCode, Json<InjectSessionError>)> {
    if req.errors.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(InjectSessionError {
                success: false,
                error: "inject-errors body must contain at least one error".to_string(),
            }),
        ));
    }
    for e in &req.errors {
        if e.message.trim().is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(InjectSessionError {
                    success: false,
                    error: "each error needs a non-empty message".to_string(),
                }),
            ));
        }
    }
    let ids = insert_errors(&req.errors);
    info!("test_fixtures: injected {} error event(s)", ids.len());
    emit_injected_changed("inject-errors", ids.len());
    Ok(Json(InjectErrorsResponse {
        success: true,
        injected: ids.len(),
        ids,
    }))
}

#[derive(Debug, Clone, Deserialize)]
pub struct InjectErrorsRequestBody {
    pub errors: Vec<InjectErrorRequest>,
}

async fn seed_error_scenario_handler(
    Json(req): Json<SeedErrorScenarioRequest>,
) -> Result<Json<SeedErrorScenarioResponse>, (StatusCode, Json<InjectSessionError>)> {
    let has_counts = !req.counts.is_empty();
    let has_errors = req.errors.as_ref().is_some_and(|e| !e.is_empty());

    if has_counts && has_errors {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(InjectSessionError {
                success: false,
                error: "supply either per-bucket counts OR an explicit `errors` list, not both"
                    .to_string(),
            }),
        ));
    }

    let requests = if has_errors {
        req.errors.unwrap_or_default()
    } else {
        error_requests_from_counts(&req.counts)
    };

    // Clear-then-seed by contract, mirroring `seed-terminal-scenario`.
    let cleared_count = clear_all_errors();
    let ids = insert_errors(&requests);

    info!(
        "test_fixtures: seeded error scenario cleared={} seeded={}",
        cleared_count,
        ids.len(),
    );
    emit_injected_changed("seed-errors", ids.len());

    Ok(Json(SeedErrorScenarioResponse {
        success: true,
        cleared_count,
        seeded: ids.len(),
        ids,
    }))
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
    // Injected ERROR events are torn down by the SAME one call, per the
    // existing teardown contract: `/clear-injected` is the superset that drops
    // every fixture this module owns. Adding a separate clear-errors route
    // would let a gate tear down its sessions and silently leave error rows
    // overlaying the next test's `/error-monitor` page.
    let cleared_errors = clear_all_errors();
    // The `identityEvidence` override is a fixture too, and it is PROCESS-GLOBAL
    // and sticky — a gate that forgot to clear it would otherwise leave every
    // later session-info read lying about its provenance. Teardown clears it
    // with the same one call that drops the injected fakes.
    crate::commands::session_info::set_forced_identity_evidence(None);
    emit_injected_changed("clear", cleared_count + cleared_errors);
    Json(ClearSessionsResponse {
        success: true,
        cleared_count,
    })
}

// =============================================================================
// /ui-bridge/test/force-identity-evidence
// =============================================================================

/// Body for `POST /ui-bridge/test/force-identity-evidence`.
///
/// `{"evidence":"provisional"}` forces every `session_info_get` /
/// `GET /control/sessions/info` projection to report that classification;
/// `{"evidence":null}` (or `{}`) clears the override and restores the real
/// classifier.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ForceIdentityEvidenceRequest {
    #[serde(default)]
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForceIdentityEvidenceResponse {
    pub success: bool,
    /// The override now in force (`null` = cleared, real classifier restored).
    pub evidence: Option<String>,
    /// What it replaced, so a gate can restore the prior state in teardown.
    pub previous: Option<String>,
}

/// Force (or clear) the `identityEvidence` a session-info projection reports.
///
/// WHY this seam exists: the `provisional` treatment — the amber panel note
/// plus the `— provisional` suffix on the account and Claude-id rows — had
/// never been observed rendered. A bare terminal has no `claudeSessionId`, so
/// no dropdown mounts for it at all; and a session that DOES bind is
/// hook-confirmed within seconds, so the classifier never lingers on
/// `provisional` long enough to look at. Forcing the classification drives the
/// treatment through the REAL projection, the REAL command and the REAL
/// component — no frontend fixture, no mocked hook, and nothing to keep in
/// sync with production rendering.
///
/// Process-global and sticky: it stays in force until cleared explicitly, by
/// `/ui-bridge/test/clear-injected`, or by the runner exiting. Rejects any
/// value that is not one of the three real classifications with a 400 naming
/// the accepted set, so a typo can never install an evidence string the
/// frontend has no branch for.
async fn force_identity_evidence_handler(
    body: Option<Json<ForceIdentityEvidenceRequest>>,
) -> Result<Json<ForceIdentityEvidenceResponse>, (StatusCode, Json<InjectSessionError>)> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let next = match req.evidence.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(v) if crate::commands::session_info::is_identity_evidence(v) => Some(v.to_string()),
        Some(v) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(InjectSessionError {
                    success: false,
                    error: format!(
                        "unknown identityEvidence '{v}' — expected one of: confirmed, transcript, provisional (or null to clear)"
                    ),
                }),
            ));
        }
    };
    let previous = crate::commands::session_info::set_forced_identity_evidence(next.clone());
    info!(
        "test_fixtures: identityEvidence override {:?} -> {:?}",
        previous, next
    );
    Ok(Json(ForceIdentityEvidenceResponse {
        success: true,
        evidence: next,
        previous,
    }))
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
// `SessionLifecycleStore` at boot and resurrects every restorable row. Restore
// tests otherwise have to hand-write the store's JSON with exact camelCase keys
// and compute the anchor/last-seen offset math by hand. These routes give them
// a seam:
//
//   - POST /ui-bridge/test/seed-lifecycle-store  — write this instance's store
//     from a JSON body of `{ records: [ {sessionId, state, lastSeenOffsetMs,
//     closeReason?, ...} ] }`. `lastSeenOffsetMs` is relative to "now" (negative
//     = the past), so a body can place open/ghost/closed rows at precise ages
//     without the caller knowing the wall clock. 400 on a malformed body.
//   - POST /ui-bridge/test/clear-lifecycle-store — empty this instance's store:
//     delete the snapshot AND its sibling WAL, then make the RUNNING store
//     adopt the emptiness (same `reload_from_disk` handshake as the seed).
//     Returns whether a file was removed and whether the live store reloaded.
//   - POST /ui-bridge/test/list-lifecycle-open — read this instance's store back
//     and return the `state == "open"` session ids (the StatusStrip restore
//     consumer's input), so a test can assert the seed round-tripped without
//     reaching into the filesystem. Reads the RUNNING store when one is
//     registered — see "The read-back must read the same store" below.
//
// ## The path is INSTANCE-namespaced, not port-namespaced
//
// This block used to say the file was namespaced by the runner's bound API
// port (`terminal-sessions[-<port>].json`). It is not, and has not been since
// `2026-08-10-temp-runner-session-restore-isolation`: all three routes resolve
// `session_lifecycle_store::store_path()`, which is
// `instance::scope_path(<runner dir>)/terminal-sessions.json` — the primary at
// `~/.qontinui/runner/`, every secondary under
// `~/.qontinui/runner/instance-<name>/`. The distinction is load-bearing for a
// caller reaching for the file directly: a recycled temp-runner PORT no longer
// aliases a previous temp's store, but two runners sharing an INSTANCE NAME
// would share one. Either way a temp runner seeds/reads its OWN file and never
// the primary's live sessions.
//
// ## The seed is applied to the RUNNING store, not just the file
//
// Writing the file is not enough inside a live runner: the running
// `SessionLifecycleStore` holds the authoritative map in memory and rewrites
// the whole file on its next persist, so a poll tick / close / compaction after
// the seed silently restored the pre-seed state while this route had already
// answered `success: true`. The handler therefore drops the sibling WAL (whose
// deltas describe the state being replaced and would otherwise replay over the
// seed on the next `open()`) and calls `reload_from_disk()` on the registered
// store. If a store IS registered and the reload FAILS, the route answers
// HTTP 409 rather than claiming a seed that will be overwritten.
//
// ## `clear` must clear the same store the seed writes to
//
// Manual-test-loop iteration 21: `clear-lifecycle-store` did only half of that
// handshake. It deleted the snapshot file and answered
// `{"success":true,"removed":true}` — while the RUNNING store still held every
// row in memory, so `restore-health?include=all` kept serving all of them and
// the store's next persist rewrote the file it had just deleted. Calling
// `clear` twice returned `removed: true` twice, which is only possible because
// something was re-creating the file behind it.
//
// `clear` therefore now does exactly what `seed` does, minus the records:
// remove the snapshot AND the sibling WAL (a surviving WAL replays its deltas
// over the empty snapshot on the next `open()` and resurrects the cleared
// rows), then `reload_from_disk()` so the live store adopts the empty map. A
// registered store whose reload FAILS is a 409, never a `success: true`.
//
// ## The read-back must read the same store
//
// `list-lifecycle-open` used to `SessionLifecycleStore::open(path)` a SECOND
// store over the file. Inside a live runner that reads whatever is on disk
// rather than what the runner is actually using — and after the old `clear`
// deleted the file it dutifully answered `open_session_ids: []` while the
// running store still held eight rows. A seam whose own read-back confirms a
// clear that did not happen is a FALSE-PASS SOURCE: every
// clear-then-assert-empty test passed unconditionally.
//
// The read-back now prefers the registered store and says which source it
// used (`source: "running-store" | "snapshot-file"`), so a caller can tell an
// in-process answer from an out-of-process one instead of guessing.
//
// ## Why `{"records": []}` is still a 400
//
// The natural "clear" spelling would be `seed-lifecycle-store {"records":[]}`,
// and it is deliberately NOT accepted. An empty `records` array is far more
// often a body that lost its rows (a mis-serialized fixture, a filtered list
// that came back empty) than a deliberate request to wipe the store — and a
// wipe is destructive and silent. There is a dedicated route whose NAME says
// what it does, and as of this iteration it actually does it, so the 400
// costs a caller nothing but a redirect to the right route. Two spellings for
// one destructive operation would only widen the surface this iteration just
// narrowed.

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
    /// Offset from now (millis) for `confirmed_at`. `None` leaves the row
    /// UNCONFIRMED, which was the only thing this seam could express — and
    /// `confirmed` is one of the two gates in
    /// [`crate::session::snapshot_history::is_restorable_identity`], so every
    /// seeded row reported `restorable: false` and the frontend's
    /// `decideColdResume` / drain-skip path could never be reached from a
    /// seeded store. Negative places the confirmation in the past, matching
    /// the other offsets on this struct.
    #[serde(default)]
    pub confirmed_at: Option<i64>,
    /// Offset from now (millis) for `restore_pending_at` — the durable
    /// in-flight-restore marker. `Some` puts the row in the `pending` bucket
    /// of `GET /control/sessions/restore-health`.
    #[serde(default)]
    pub restore_pending_at: Option<i64>,
    /// Verbatim restore tier — `"resumed"` / `"terminal-only"` / `"failed"`
    /// (see `session_lifecycle_store::RESTORE_TIER_*`). NOT an offset: it is
    /// stored as-is, and drives both the `failed` bucket and the rendered
    /// `restoreStatus` verdict.
    #[serde(default)]
    pub restore_tier: Option<String>,
    /// Verbatim id provenance — `"authoritative"` / `"observed"` /
    /// `"reconciled"`. `None` (the old hardcoded value) reads as
    /// `"reconciled"`, and the frontend's `classifyRestoreAction` returns
    /// `"terminal-only"` for anything that is not `authoritative`/`observed`.
    /// `confirmedAt` alone is therefore NOT enough to reach the drain's
    /// `decideColdResume` call site (`useTerminalInitialization.ts` ~1201):
    /// `isClaudeSession` is `restoreAction === "auto-resume"`, which needs
    /// BOTH a confirmation and a strong origin. Expressing only the timestamps
    /// would have moved `restorable` in the restore-health report while
    /// leaving the drain path exactly as unreachable as before.
    #[serde(default)]
    pub origin: Option<String>,
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
    /// Whether the RUNNING store adopted the seed (`reload_from_disk`). `false`
    /// means no store was registered in this process — the file is the whole
    /// state, which is the case for out-of-process callers and unit tests. A
    /// registered store that failed to reload is a 409, never a `false` here.
    pub reloaded: bool,
    /// Records the running store holds after the reload. Absent when
    /// `reloaded` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_memory_records: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListLifecycleOpenResponse {
    pub success: bool,
    pub open_session_ids: Vec<String>,
    pub path: String,
    /// WHERE the ids were read from: `"running-store"` when this process has a
    /// live `SessionLifecycleStore` registered (the authoritative answer inside
    /// a runner), `"snapshot-file"` when it does not and the file is the whole
    /// state. Never guess — a file read inside a live runner answers about a
    /// store nothing is using.
    pub source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClearLifecycleResponse {
    pub success: bool,
    /// Whether the snapshot file existed and was deleted.
    pub removed: bool,
    /// Whether the sibling WAL existed and was deleted. A WAL left behind
    /// replays its deltas over the empty snapshot on the next `open()` and
    /// resurrects the very rows the clear discarded.
    pub removed_wal: bool,
    pub path: String,
    /// Whether the RUNNING store adopted the clear (`reload_from_disk`).
    /// `false` means no store was registered in this process — the file was
    /// the whole state, which is the case for out-of-process callers and unit
    /// tests. A registered store that failed to reload is a 409, never a
    /// `false` here.
    pub reloaded: bool,
    /// Records the running store holds after the clear — `Some(0)` is the
    /// whole point of this route. Absent when `reloaded` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_memory_records: Option<usize>,
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
        provider: crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
        origin: seed.origin.clone(),
        // Resolved against `now_ms` exactly like `last_seen_at` — so a caller
        // can place a confirmation or an in-flight restore marker at a precise
        // age without knowing the wall clock. Absent means absent; these are
        // no longer hardcoded to `None`.
        restore_pending_at: seed.restore_pending_at.map(|off| now_ms + off),
        confirmed_at: seed.confirmed_at.map(|off| now_ms + off),
        handle: None,
        account_label: None,
        account_wrapper: None,
        session_name: None,
        name_source: None,
        tenant_id: None,
        task_run_id: None,
        bypass_permissions: None,
        restored_from_boot_at: None,
        restore_tier: seed.restore_tier.clone(),
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
        // Deliberately NOT treated as "clear" — see this section's
        // "Why `{\"records\": []}` is still a 400" note. An empty array is far
        // more often a body that lost its rows than a deliberate wipe, and the
        // wipe has its own route which (since iteration 21) actually empties
        // the running store rather than just deleting a file.
        return Err((
            StatusCode::BAD_REQUEST,
            "seed body must contain at least one record; to empty the store use \
             POST /ui-bridge/test/clear-lifecycle-store"
                .to_string(),
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
    // Drop the sibling write-ahead log. `SessionLifecycleStore::open` replays it
    // OVER the snapshot, so a WAL left behind by whatever wrote the store before
    // this seed would resurrect those records on the next open — the
    // clear-then-seed contract silently violated, with no error anywhere.
    let wal = crate::session::session_lifecycle_store::wal_path_for(path);
    match std::fs::remove_file(&wal) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to clear lifecycle store WAL {}: {e}", wal.display()),
            ))
        }
    }
    Ok(seeded)
}

/// Core read-back: the `state == "open"` session ids in the store at `path`
/// (sorted). A missing/unreadable store reads as empty.
///
/// Only correct when NO store is registered in this process. Inside a live
/// runner this opens a second store over the file and answers about state
/// nothing is using — use [`list_lifecycle_open_in`] on the running store
/// instead. See the module section "The read-back must read the same store".
fn list_lifecycle_open_at(path: &Path) -> Vec<String> {
    match SessionLifecycleStore::open(path) {
        Ok(store) => list_lifecycle_open_in(&store),
        Err(_) => Vec::new(),
    }
}

/// Core read-back against an ALREADY-OPEN store: the `state == "open"` session
/// ids it holds, sorted. This is the authoritative answer inside a live runner.
fn list_lifecycle_open_in(store: &SessionLifecycleStore) -> Vec<String> {
    let mut ids: Vec<String> = store
        .open_records()
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    ids.sort();
    ids
}

/// Core clear logic, path-injectable so it's unit-testable without an
/// `ApiState`. Removes the snapshot AND its sibling WAL.
///
/// Dropping the WAL is not optional: `SessionLifecycleStore::open` replays it
/// OVER the snapshot, so a WAL surviving a "clear" resurrects every row the
/// clear was asked to discard on the very next open — the same silent-loss
/// shape [`seed_lifecycle_store_at`] already guards against, inverted.
///
/// Returns `(snapshot_removed, wal_removed)`; a file that was already absent
/// reports `false` rather than erroring, so a clear is idempotent.
fn clear_lifecycle_store_at(path: &Path) -> Result<(bool, bool), (StatusCode, String)> {
    let wal = crate::session::session_lifecycle_store::wal_path_for(path);
    let mut removed = [false; 2];
    for (slot, target) in [(0usize, path), (1usize, wal.as_path())] {
        match std::fs::remove_file(target) {
            Ok(()) => removed[slot] = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to clear {}: {e}", target.display()),
                ))
            }
        }
    }
    Ok((removed[0], removed[1]))
}

async fn seed_lifecycle_store_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
    Json(req): Json<SeedLifecycleRequest>,
) -> Result<Json<SeedLifecycleResponse>, (StatusCode, Json<InjectSessionError>)> {
    // This control route runs INSIDE the target runner, so its own instance
    // identity already selects the correct file — resolve the canonical
    // self-scoped path (no port arg needed once the port is no longer the key).
    let path = crate::session::session_lifecycle_store::store_path();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let seeded = match seed_lifecycle_store_at(&path, &req, now_ms) {
        Ok(seeded) => seeded,
        Err((code, error)) => {
            return Err((
                code,
                Json(InjectSessionError {
                    success: false,
                    error,
                }),
            ))
        }
    };
    // Hand the seed to the RUNNING store. Without this the file is authoritative
    // only until the live store's next persist rewrites it from its own
    // in-memory map — see this module's header for the silent-loss defect.
    // `try_state` is a `Manager` method — imported locally so this module's
    // other handlers keep their existing (Manager-free) import surface.
    use tauri::Manager as _;
    let (reloaded, in_memory_records) = match state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    {
        Some(store) => match store.reload_from_disk() {
            Ok(count) => (true, Some(count)),
            Err(e) => {
                return Err((
                    StatusCode::CONFLICT,
                    Json(InjectSessionError {
                        success: false,
                        error: format!(
                            "seed written to {} but the running lifecycle store could not adopt it ({e}); it would be overwritten by the store's next persist",
                            path.display()
                        ),
                    }),
                ))
            }
        },
        // No store registered in this process (out-of-process callers, tests):
        // the file IS the whole state, so the seed stands as written.
        None => (false, None),
    };
    info!(
        "test_fixtures: seeded lifecycle store path={} records={} reloaded={}",
        path.display(),
        seeded,
        reloaded,
    );
    Ok(Json(SeedLifecycleResponse {
        success: true,
        seeded,
        path: path.display().to_string(),
        reloaded,
        in_memory_records,
    }))
}

async fn list_lifecycle_open_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> Json<ListLifecycleOpenResponse> {
    // Runs inside the target runner — its own instance identity selects the
    // correct file (canonical self-scoped path).
    let path = crate::session::session_lifecycle_store::store_path();
    // Prefer the RUNNING store. Reading the file instead is what made this
    // read-back lie: after a clear deleted the snapshot it answered "empty"
    // while the live store still served every row to `restore-health`.
    use tauri::Manager as _;
    let (open_session_ids, source) = match state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    {
        Some(store) => (list_lifecycle_open_in(&store), "running-store"),
        // No store registered in this process (out-of-process callers, tests):
        // the file IS the whole state.
        None => (list_lifecycle_open_at(&path), "snapshot-file"),
    };
    Json(ListLifecycleOpenResponse {
        success: true,
        open_session_ids,
        path: path.display().to_string(),
        source,
    })
}

async fn clear_lifecycle_store_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> Result<Json<ClearLifecycleResponse>, (StatusCode, Json<InjectSessionError>)> {
    // A control route that clears "self". It runs INSIDE the target runner, so
    // its own instance identity selects the correct file; under instance
    // scoping the former `api_port` query param is redundant with self-identity
    // (a port-named file is no longer the key) — clear the current instance's
    // own store, the only coherent meaning here.
    let path = crate::session::session_lifecycle_store::store_path();
    let (removed, removed_wal) = match clear_lifecycle_store_at(&path) {
        Ok(pair) => pair,
        Err((code, error)) => {
            return Err((
                code,
                Json(InjectSessionError {
                    success: false,
                    error,
                }),
            ))
        }
    };
    // Hand the clear to the RUNNING store — the half this route used to skip.
    // Deleting the file alone left every row in memory, so `restore-health`
    // kept serving them and the store's next persist re-created the file.
    // Mirrors `seed_lifecycle_store_handler` exactly, minus the records.
    use tauri::Manager as _;
    let (reloaded, in_memory_records) = match state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    {
        Some(store) => match store.reload_from_disk() {
            Ok(count) => (true, Some(count)),
            Err(e) => {
                return Err((
                    StatusCode::CONFLICT,
                    Json(InjectSessionError {
                        success: false,
                        error: format!(
                            "{} was deleted but the running lifecycle store could not adopt the clear ({e}); it still holds the rows and would rewrite them on its next persist",
                            path.display()
                        ),
                    }),
                ))
            }
        },
        // No store registered in this process: the file WAS the whole state,
        // so deleting it is the whole clear.
        None => (false, None),
    };
    info!(
        "test_fixtures: cleared lifecycle store path={} removed={} removed_wal={} reloaded={} in_memory={:?}",
        path.display(),
        removed,
        removed_wal,
        reloaded,
        in_memory_records,
    );
    Ok(Json(ClearLifecycleResponse {
        success: true,
        removed,
        removed_wal,
        path: path.display().to_string(),
        reloaded,
        in_memory_records,
    }))
}

// =============================================================================
// /ui-bridge/test/coord-mcp/seed-agent-token  (+ agent-token read-back)
// =============================================================================
//
// The agent-proxy refresh path (runner #592) holds each spawned agent's live JWT
// in a process-global `AGENT_TOKENS` slot (`coord_mcp.rs`), refreshed by
// `agent_token::maybe_refresh` off the slot's BOOKKEEPING `exp` (not the JWT's
// own exp claim). These seams let a test drive that path from OUTSIDE the spawn
// flow:
//   - seed-agent-token: build a `TokenSlot { token, jti: nil, exp }` from a real
//     (still-valid) JWT plus a deliberately-short bookkeeping `exp`, register it
//     under `agent_id`, and register an Agent-bound proxy nonce for `workdir`.
//     Returns the nonce. A short `exp` makes `maybe_refresh` fire immediately
//     while the real token still authenticates the refresh POST.
//   - agent-token (GET): observe the slot's `exp` / `jti` / `ttl_secs` so a test
//     can prove a refresh rotated the slot. NEVER returns the token string.

#[derive(Debug, Clone, Deserialize)]
pub struct SeedAgentTokenRequest {
    pub agent_id: Uuid,
    /// The real (still-valid) agent JWT to install in the slot. Untouched — only
    /// the bookkeeping `exp` below drives the refresh decision.
    pub jwt: String,
    /// Bookkeeping `exp` (unix seconds) to stamp into the slot. Pass a value near
    /// `now` to make the refresh boundary fire immediately even though `jwt` is
    /// still valid for hours.
    pub jwt_exp: i64,
    /// Session workdir the Agent-bound proxy nonce is provisioned for.
    pub workdir: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeedAgentTokenResponse {
    pub success: bool,
    pub nonce: String,
}

async fn seed_agent_token_handler(
    Json(req): Json<SeedAgentTokenRequest>,
) -> Result<Json<SeedAgentTokenResponse>, (StatusCode, Json<InjectSessionError>)> {
    if req.jwt.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(InjectSessionError {
                success: false,
                error: "jwt must not be empty".to_string(),
            }),
        ));
    }
    if req.workdir.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(InjectSessionError {
                success: false,
                error: "workdir must not be empty".to_string(),
            }),
        ));
    }

    let slot = Arc::new(tokio::sync::RwLock::new(crate::agent_token::TokenSlot {
        token: req.jwt.clone(),
        jti: Uuid::nil(),
        exp: req.jwt_exp,
        ..Default::default()
    }));
    crate::coord_mcp::register_agent_token(req.agent_id, slot);
    let nonce = crate::coord_mcp::register_agent_proxy_nonce(&req.workdir, req.agent_id);

    info!(
        "test_fixtures: seeded agent token agent_id={} exp={} workdir={} (agent-bound proxy nonce minted)",
        req.agent_id, req.jwt_exp, req.workdir,
    );

    Ok(Json(SeedAgentTokenResponse {
        success: true,
        nonce,
    }))
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTokenView {
    pub present: bool,
    /// Bookkeeping expiry (unix seconds). `None` when the slot is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exp: Option<i64>,
    /// Slot `jti` as a string. `None` when the slot is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
    /// Seconds until the bookkeeping `exp` from now (may be negative). `None`
    /// when the slot is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<i64>,
}

async fn agent_token_view_handler(
    axum::extract::Path(agent_id): axum::extract::Path<Uuid>,
) -> Json<AgentTokenView> {
    match crate::coord_mcp::lookup_agent_token(agent_id) {
        Some(slot) => {
            let guard = slot.read().await;
            let now = chrono::Utc::now().timestamp();
            Json(AgentTokenView {
                present: true,
                exp: Some(guard.exp),
                jti: Some(guard.jti.to_string()),
                ttl_secs: Some(guard.ttl_secs(now)),
            })
        }
        None => Json(AgentTokenView {
            present: false,
            exp: None,
            jti: None,
            ttl_secs: None,
        }),
    }
}

// =============================================================================
// /ui-bridge/test/append-transcript-record
// =============================================================================
//
// R1 of `2026-08-26-prompts-panel-manual-test-remediation`.
//
// The per-zone "my prompts" panel polls `transcript_read_user_prompts` every 5s
// and re-renders what changed. That LIVE-UPDATE arm was the one check the
// manual-test run could not make, because there is no cheap way to make Claude
// append to its own transcript on demand: a real prompt costs an API-billed
// turn, and a slash command only helps if it files a `user` record. `/status`
// was the obvious candidate and files NONE — measured, the terminal's byte
// count moved 5144 → 8247 while the panel's prompt count stayed at 4, so the
// input landed and the transcript genuinely did not change.
//
// This route makes the append deterministic instead: it writes ONE synthetic
// record into a named session's JSONL, so the poll has something to observe.
//
// ## It writes the path the reader OPENS — it does not re-derive it
//
// [`crate::terminal::transcript::read_user_prompts`] builds
// `<config_dir>/projects/<encoded(project_path)>/<session_id>.jsonl`, where the
// encoding is `encode_project_path` — a PRIVATE fn. The public handle onto the
// same construction is [`crate::terminal::transcript::session_transcript_path`],
// which this route calls. Nothing here reimplements the encoding, so a change
// to it cannot silently split the writer from the reader.
//
// ## The record kinds are exactly the distinctions the RUST reader makes
//
// `read_user_prompts` keeps a record only if it is `type:"user"`, carries none
// of the three machine flags (`is_machine_authored_user_record`), and yields
// text through `parse_user_record`. So a fixture that can only write a clean
// prompt cannot exercise the filter at all. [`TranscriptRecordKind`] spans it:
//
// | kind | shape written | reader's verdict |
// |------|---------------|------------------|
// | `prompt` (default) | plain `user` record, string content | SURFACED |
// | `meta_expansion` | `user` + `"isMeta":true` | dropped |
// | `compact_summary` | `user` + `"isCompactSummary":true` | dropped |
// | `sidechain` | `user` + `"isSidechain":true` | dropped |
// | `tool_result` | `user`, content `[{"type":"tool_result",…}]` | dropped (no text block) |
// | `assistant` | `type:"assistant"` | dropped (wrong record type) |
//
// `<task-notification>` is deliberately ABSENT from that list. That filter is
// TypeScript-side (`src/components/terminal/sessionPrompts.ts`), applied to the
// text the Rust reader has already surfaced — so a Rust knob for it would be a
// knob for a filter this code does not own. To exercise it, write a `prompt`
// whose `text` IS a task notification; the Rust reader will surface it (that is
// correct) and the TS envelope normalizer is then the thing under test.
//
// ## The mtime is MOVED, not hoped for
//
// The reader short-circuits: `since_mtime_ms == mtime_ms` returns
// `{unchanged: true, prompts: []}` without parsing. An append that lands inside
// the same millisecond tick as the caller's last read is therefore INVISIBLE to
// the panel — the fixture would report success and the poll would show nothing,
// which is precisely the failure this route exists to rule out. So the write is
// followed by [`ensure_mtime_moved`], which re-stats and, if the mtime did not
// move, sets it forward explicitly (escalating deltas) and re-stats again to
// VERIFY. A file whose mtime refuses to move is a 500, never a success.
//
// The guarantee is *strictly greater than* the pre-call mtime, not merely
// *different from* it: a bump lands the mtime a hair ahead of the wall clock,
// so the next append's natural mtime can be BEHIND it, and "different" would be
// satisfied by moving BACKWARDS onto a value the poller had already seen.
//
// The post-write `mtime_ms` comes back in the response in the SAME unit and
// epoch as `UserPromptsResult::mtime_ms`, so a caller hands it straight back as
// `since_mtime_ms` and asserts on the change instead of sleeping on it.
//
// ## The response carries the READER's verdict, not the fixture's opinion
//
// `visible_to_reader` / `prompts_after` are produced by calling
// `read_user_prompts` on the file this route just wrote — not by restating the
// table above in code. A fixture that predicted visibility from its own `kind`
// mapping would keep passing after the reader's filter changed underneath it,
// which is the exact drift a test seam must not have.
//
// ## Wire convention
//
// Request and response are snake_case (no `rename_all`), deliberately: the
// request body IS `read_user_prompts`'s parameter list (`config_dir`,
// `project_path`, `session_id`), and `mtime_ms` matches `UserPromptsResult`'s
// field name byte-for-byte so the round-trip needs no mental translation.

/// Which record shape to append. See the table in this section's header for
/// what the reader does with each.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRecordKind {
    /// A plain operator-typed prompt — the only kind the reader surfaces.
    #[default]
    Prompt,
    /// A slash-command EXPANSION (`isMeta`). Claude Code files the entire skill
    /// body under the `user` role; the flag is the only marker.
    MetaExpansion,
    /// A post-`/compact` continuation summary (`isCompactSummary`).
    CompactSummary,
    /// A subagent turn (`isSidechain`).
    Sidechain,
    /// A tool result: a `user` record whose content array holds no `text`
    /// block, so `parse_user_record` yields nothing.
    ToolResult,
    /// An assistant turn — filtered on record TYPE rather than on a flag.
    Assistant,
}

impl TranscriptRecordKind {
    /// Placeholder text used when the caller supplies none, chosen so each
    /// record still LOOKS like the thing it is standing in for.
    fn default_text(self) -> &'static str {
        match self {
            Self::Prompt => "fixture prompt",
            Self::MetaExpansion => {
                "<command-name>/fixture</command-name>\n# Fixture skill body\n\nMachine-authored."
            }
            Self::CompactSummary => {
                "This session is being continued from a previous conversation that ran out of context."
            }
            Self::Sidechain => "fixture subagent turn",
            Self::ToolResult => "fixture tool output",
            Self::Assistant => "fixture assistant reply",
        }
    }
}

/// Body of `POST /ui-bridge/test/append-transcript-record`.
///
/// The first three fields are `read_user_prompts`'s own parameters — give it
/// the same triple the panel is polling and the append lands where the panel
/// will look.
#[derive(Debug, Clone, Deserialize)]
pub struct AppendTranscriptRecordRequest {
    /// Claude config dir root — the `<config_dir>` of
    /// `<config_dir>/projects/<encoded project>/<session_id>.jsonl`.
    pub config_dir: String,
    /// Unencoded project path (e.g. `D:\qontinui-root\qontinui-runner`). The
    /// encoding is applied by `session_transcript_path`, never here.
    pub project_path: String,
    /// Session id — becomes the JSONL's file stem.
    pub session_id: String,
    /// Which shape to write. Defaults to `prompt`.
    #[serde(default)]
    pub kind: TranscriptRecordKind,
    /// The record's text. Defaults to a per-kind placeholder, so the triple
    /// plus a `kind` is a complete body.
    #[serde(default)]
    pub text: Option<String>,
    /// Record uuid. Defaults to a fresh v4; the response echoes whichever was
    /// used so a caller can pin the exact record the reader returns.
    #[serde(default)]
    pub uuid: Option<String>,
    /// ISO 8601 timestamp. Defaults to now.
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Truncate the transcript before appending. `false` (the default) appends.
    ///
    /// This is what lets repeated calls SEED a whole transcript from a known
    /// empty state — `reset` once, then append one record per kind — without a
    /// second route to clear the file.
    #[serde(default)]
    pub reset: bool,
}

/// Response of `POST /ui-bridge/test/append-transcript-record`.
#[derive(Debug, Clone, Serialize)]
pub struct AppendTranscriptRecordResponse {
    pub success: bool,
    /// Absolute path written — the path `read_user_prompts` opens for this
    /// triple, so a caller can assert the two agree without re-deriving the
    /// project-path encoding.
    pub path: String,
    /// Post-write mtime, in the same unit and epoch as
    /// `UserPromptsResult::mtime_ms`. Hand it back as `since_mtime_ms`.
    pub mtime_ms: u64,
    /// The file's mtime BEFORE this call, or `None` when it did not exist.
    /// Present so the caller can check the move itself rather than trusting
    /// that it happened.
    pub previous_mtime_ms: Option<u64>,
    /// Whether the mtime had to be pushed forward explicitly because the write
    /// landed inside the previous mtime's tick. Diagnostic only — either way a
    /// 200 guarantees `mtime_ms` is strictly greater than `previous_mtime_ms`.
    pub mtime_bumped: bool,
    /// The uuid of the appended record.
    pub uuid: String,
    /// The kind written, echoed back.
    pub kind: TranscriptRecordKind,
    /// Whether `read_user_prompts` actually surfaces this record — measured by
    /// re-reading the file through the real reader, not predicted from `kind`.
    pub visible_to_reader: bool,
    /// How many prompts the reader surfaces from the file after this append —
    /// i.e. how many cards the panel should render.
    pub prompts_after: usize,
    /// Total non-empty JSONL lines after this append. Confirms an append did
    /// not clobber the file.
    pub records_after: usize,
    /// Whether this call created the transcript file.
    pub created: bool,
}

/// Escalating deltas (millis) tried when the post-write mtime has not moved.
///
/// The first two cover a sub-millisecond-granularity filesystem writing twice
/// inside one tick; the rest cover coarse-granularity ones (some network and
/// FAT-derived filesystems quantize to 1s or 2s), so this does not silently
/// give up on the exact platforms where the collision is most likely.
const MTIME_BUMP_DELTAS_MS: [u64; 6] = [1, 2, 10, 100, 1_000, 2_000];

/// The file's mtime in millis since the Unix epoch, matching how
/// `read_user_prompts` computes the value it compares against.
fn transcript_mtime_ms(path: &Path) -> Result<u64, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("could not stat {}: {e}", path.display()))?;
    Ok(meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0))
}

/// Guarantee the transcript's mtime is strictly GREATER than `previous_ms`, so
/// the reader's `since_mtime_ms == mtime_ms` short-circuit cannot swallow this
/// append.
///
/// Returns `(mtime_ms, bumped)`. A write on a fast filesystem can land inside
/// the same millisecond as the caller's last read, so "we wrote, therefore it
/// changed" is not sound — this re-stats, and on a collision pushes the mtime
/// forward explicitly and re-stats to VERIFY rather than assuming the set took.
///
/// The contract is *strictly greater*, not merely *different*: a bump lands the
/// mtime slightly ahead of the wall clock, so the NEXT append's natural mtime
/// can be BEHIND it. "Different" would then be satisfied by going backwards
/// onto a value the caller had already seen — which is exactly a missed poll.
fn ensure_mtime_moved(path: &Path, previous_ms: Option<u64>) -> Result<(u64, bool), String> {
    let observed = transcript_mtime_ms(path)?;
    // A 0 mtime means the platform gave us no modification time at all. The
    // reader treats that as "always changed" (it refuses to let an unknown
    // masquerade as a match), so the short-circuit cannot bite and there is
    // nothing here to bump.
    if observed == 0 {
        return Ok((0, false));
    }
    let Some(previous) = previous_ms else {
        return Ok((observed, false));
    };
    if observed > previous {
        return Ok((observed, false));
    }
    for delta in MTIME_BUMP_DELTAS_MS {
        let target = std::time::UNIX_EPOCH
            + std::time::Duration::from_millis(previous.saturating_add(delta));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| format!("could not reopen {} to move its mtime: {e}", path.display()))?;
        file.set_modified(target)
            .map_err(|e| format!("could not set the mtime on {}: {e}", path.display()))?;
        drop(file);
        let after = transcript_mtime_ms(path)?;
        if after > previous {
            return Ok((after, true));
        }
    }
    Err(format!(
        "the mtime of {} would not move past {}ms after {} attempts — the reader's \
         since_mtime_ms short-circuit would swallow this append, so reporting success \
         here would be a lie",
        path.display(),
        previous,
        MTIME_BUMP_DELTAS_MS.len(),
    ))
}

/// Whether a newline must be written before the record so the file stays JSONL.
///
/// Reads the last byte rather than the whole file: a real transcript reaches
/// 10 MB, and this runs on every append.
fn transcript_needs_leading_newline(path: &Path) -> Result<bool, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("could not open {}: {e}", path.display())),
    };
    let len = file
        .metadata()
        .map_err(|e| format!("could not stat {}: {e}", path.display()))?
        .len();
    if len == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))
        .map_err(|e| format!("could not seek in {}: {e}", path.display()))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)
        .map_err(|e| format!("could not read the last byte of {}: {e}", path.display()))?;
    Ok(last[0] != b'\n')
}

/// Build the JSONL record for `kind`. Every shape here is one Claude Code
/// actually writes — the flags are its own, and the reader filters on exactly
/// these and nothing else.
fn build_transcript_record(
    kind: TranscriptRecordKind,
    uuid: &str,
    timestamp: &str,
    text: &str,
) -> serde_json::Value {
    // The flag key is chosen at runtime, so it is inserted rather than written
    // as a `json!` literal key.
    let flagged = |flag: &str| {
        let mut value = serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "timestamp": timestamp,
            "message": {"role": "user", "content": text},
        });
        value
            .as_object_mut()
            .expect("json! built an object")
            .insert(flag.to_string(), serde_json::Value::Bool(true));
        value
    };
    match kind {
        TranscriptRecordKind::Prompt => serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "timestamp": timestamp,
            "message": {"role": "user", "content": text},
        }),
        TranscriptRecordKind::MetaExpansion => flagged("isMeta"),
        TranscriptRecordKind::CompactSummary => flagged("isCompactSummary"),
        TranscriptRecordKind::Sidechain => flagged("isSidechain"),
        TranscriptRecordKind::ToolResult => serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "timestamp": timestamp,
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": format!("toolu_{uuid}"),
                "content": text,
            }]},
        }),
        TranscriptRecordKind::Assistant => serde_json::json!({
            "type": "assistant",
            "uuid": uuid,
            "timestamp": timestamp,
            "message": {
                "role": "assistant",
                "model": "claude-fixture",
                "content": [{"type": "text", "text": text}],
            },
        }),
    }
}

/// Core of the route, split out the way `seed_lifecycle_store_at` is: the
/// handler is a thin wrapper, and the unit tests drive THIS plus the real
/// reader so they pin the consequence rather than the write.
fn append_transcript_record_core(
    req: &AppendTranscriptRecordRequest,
) -> Result<AppendTranscriptRecordResponse, (StatusCode, String)> {
    let bad = |msg: String| (StatusCode::BAD_REQUEST, msg);

    let config_dir = req.config_dir.trim();
    let project_path = req.project_path.trim();
    let session_id = req.session_id.trim();
    if config_dir.is_empty() {
        return Err(bad("config_dir must not be empty".to_string()));
    }
    if project_path.is_empty() {
        return Err(bad("project_path must not be empty".to_string()));
    }
    if session_id.is_empty() {
        return Err(bad("session_id must not be empty".to_string()));
    }
    // `session_id` is interpolated straight into a FILENAME, so a separator or
    // a `..` in it writes outside the transcript dir. Cheap to reject, and the
    // reader could never have opened such a path anyway.
    if session_id.contains(['/', '\\', ':']) || session_id.contains("..") {
        return Err(bad(format!(
            "session_id {session_id:?} must be a bare file stem — it is interpolated \
             into <session_id>.jsonl, so path separators and `..` are refused"
        )));
    }

    let config_root = std::path::PathBuf::from(config_dir);
    // The ONE construction the reader uses. Do not re-derive it here.
    let path = crate::terminal::transcript::session_transcript_path(
        &config_root,
        project_path,
        session_id,
    );

    let existed = path.exists();
    let previous_mtime_ms = if existed {
        Some(transcript_mtime_ms(&path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?)
    } else {
        None
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not create {}: {e}", parent.display()),
            )
        })?;
    }
    if req.reset && existed {
        std::fs::write(&path, b"").map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not truncate {}: {e}", path.display()),
            )
        })?;
    }

    let uuid = req
        .uuid
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let timestamp = req
        .timestamp
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let text = req
        .text
        .as_deref()
        .unwrap_or_else(|| req.kind.default_text());

    let record = build_transcript_record(req.kind, &uuid, &timestamp, text);
    // Compact, not pretty: JSONL is one record per LINE, and a pretty-printed
    // record would parse as several malformed ones (which the reader skips
    // silently — the append would vanish with no error anywhere).
    let line = serde_json::to_string(&record).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not serialize the record: {e}"),
        )
    })?;
    let lead = transcript_needs_leading_newline(&path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not open {} for append: {e}", path.display()),
            )
        })?;
    let payload = if lead {
        format!("\n{line}\n")
    } else {
        format!("{line}\n")
    };
    file.write_all(payload.as_bytes()).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not append to {}: {e}", path.display()),
        )
    })?;
    file.flush().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not flush {}: {e}", path.display()),
        )
    })?;
    drop(file);

    let (mtime_ms, mtime_bumped) = ensure_mtime_moved(&path, previous_mtime_ms)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Ask the REAL reader what it now sees. This is the route's honesty
    // guarantee: `visible_to_reader` is the reader's verdict on the bytes just
    // written, so the fixture cannot drift from the filter it exists to test.
    let seen = crate::terminal::transcript::read_user_prompts(
        &config_root,
        project_path,
        session_id,
        None,
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "appended to {} but the reader could not read it back: {e}",
                path.display()
            ),
        )
    })?;
    let visible_to_reader = seen.prompts.iter().any(|p| p.uuid == uuid);

    let records_after = std::fs::read_to_string(&path)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not re-read {}: {e}", path.display()),
            )
        })?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();

    Ok(AppendTranscriptRecordResponse {
        success: true,
        path: path.display().to_string(),
        mtime_ms,
        previous_mtime_ms,
        mtime_bumped,
        uuid,
        kind: req.kind,
        visible_to_reader,
        prompts_after: seen.prompts.len(),
        records_after,
        created: !existed,
    })
}

async fn append_transcript_record_handler(
    Json(req): Json<AppendTranscriptRecordRequest>,
) -> Result<Json<AppendTranscriptRecordResponse>, (StatusCode, Json<InjectSessionError>)> {
    match append_transcript_record_core(&req) {
        Ok(resp) => {
            info!(
                "test_fixtures: appended transcript record path={} kind={:?} uuid={} \
                 mtime_ms={} bumped={} visible={} prompts_after={}",
                resp.path,
                resp.kind,
                resp.uuid,
                resp.mtime_ms,
                resp.mtime_bumped,
                resp.visible_to_reader,
                resp.prompts_after,
            );
            Ok(Json(resp))
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
        .route("/ui-bridge/test/inject-errors", post(inject_errors_handler))
        .route(
            "/ui-bridge/test/seed-error-scenario",
            post(seed_error_scenario_handler),
        )
        .route(
            "/ui-bridge/test/force-identity-evidence",
            post(force_identity_evidence_handler),
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
        .route(
            "/ui-bridge/test/coord-mcp/seed-agent-token",
            post(seed_agent_token_handler),
        )
        .route(
            "/ui-bridge/test/coord-mcp/agent-token/{agent_id}",
            get(agent_token_view_handler),
        )
        .route(
            "/ui-bridge/test/append-transcript-record",
            post(append_transcript_record_handler),
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

    /// F2: every inject body printed in this module's CONSUMER CONTRACT table
    /// must actually POST. The table used to show `{liveStatus:"idle",
    /// tab_backed:true}` — no `task_run_id`, no `name`, both REQUIRED — so a
    /// verification agent copying a documented body got a deserialize failure
    /// before any fixture logic ran.
    ///
    /// The bodies are read back OUT of this file's own doc comment rather than
    /// restated here, so the test cannot pass while the documentation it is
    /// vouching for says something else.
    #[test]
    fn every_documented_inject_body_is_postable() {
        let src = include_str!("test_fixtures.rs");
        let bodies: Vec<&str> = src
            .lines()
            .filter(|l| l.starts_with("//! |") && l.contains("liveStatus\":"))
            .filter_map(|l| {
                let start = l.find("`{")?;
                let end = l.rfind("}`")?;
                Some(&l[start + 1..end + 1])
            })
            .collect();
        assert_eq!(
            bodies.len(),
            5,
            "the contract table documents one body per StatusStrip bucket;              found {bodies:?}"
        );

        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        for body in bodies {
            let req: InjectSessionRequest = serde_json::from_str(body)
                .unwrap_or_else(|e| panic!("documented body does not deserialize: {body} — {e}"));
            // …and it must also survive the validation guards the same doc
            // describes, i.e. reach a 200 rather than a 400.
            insert_session(req).unwrap_or_else(|(code, err)| {
                panic!("documented body {body} → {code}: {}", err.error)
            });
        }
        clear_all_sessions();
    }

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
            "/ui-bridge/test/inject-errors",
            "/ui-bridge/test/seed-error-scenario",
            "/ui-bridge/test/force-identity-evidence",
            "/ui-bridge/test/seed-lifecycle-store",
            "/ui-bridge/test/list-lifecycle-open",
            "/ui-bridge/test/clear-lifecycle-store",
            "/ui-bridge/test/coord-mcp/seed-agent-token",
            "/ui-bridge/test/coord-mcp/agent-token/{agent_id}",
            "/ui-bridge/test/append-transcript-record",
        ] {
            assert!(
                src.contains(&format!("\"{route}\"")),
                "route {route} must remain wired in test_fixtures::routes()",
            );
        }
    }

    // =======================================================================
    // Injected ERROR EVENTS seam (manual-test-loop iter 16)
    //
    // The surface these make observable: three iterations could not verify an
    // Error Monitor defect because nothing in the runner inserted into
    // `error_events`, and the two alternatives (writing the SHARED PostgreSQL
    // directly, mutating global log-source settings) were correctly refused.
    // =======================================================================

    fn err(message: &str, status: &str, severity: &str) -> InjectErrorRequest {
        InjectErrorRequest {
            message: message.to_string(),
            severity: Some(severity.to_string()),
            status: Some(status.to_string()),
            log_source_name: Some("unit".to_string()),
            error_type: Some("UnitError".to_string()),
            stack_trace: None,
            file_path: None,
            line_number: None,
            task_run_id: None,
            occurrence_count: None,
        }
    }

    /// The core round trip: a seeded `recurring` error reaches the merge point
    /// that `query_error_events` uses, and `clear-injected` removes it.
    #[test]
    fn a_seeded_recurring_error_merges_and_then_tears_down() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_errors();

        let ids = insert_errors(&[err("boom", "recurring", "error")]);
        assert_eq!(ids.len(), 1);
        assert!(ids[0] < 0, "injected ids must be negative, got {}", ids[0]);

        let merged = merge_with_injected_errors(
            Vec::new(),
            &crate::error_monitor::types::ErrorQuery::default(),
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].message, "boom");
        assert_eq!(merged[0].status, ErrorStatus::Recurring);
        assert_eq!(merged[0].occurrence_count, 1);

        // TEARDOWN through the EXISTING clear route's shared core.
        let cleared = clear_all_errors();
        assert_eq!(cleared, 1);
        assert!(
            merge_with_injected_errors(
                Vec::new(),
                &crate::error_monitor::types::ErrorQuery::default()
            )
            .is_empty(),
            "clear-injected must remove every injected error"
        );
    }

    /// Real rows are APPENDED to, never replaced — a fixture that hid real
    /// rows would make the page lie about the machine's actual state.
    #[test]
    fn injected_errors_append_to_real_ones() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_errors();

        let real = vec![project_injected_error(
            &err("real", "new", "error"),
            42,
            "t0",
        )];
        insert_errors(&[err("fake", "new", "error")]);

        let merged =
            merge_with_injected_errors(real, &crate::error_monitor::types::ErrorQuery::default());
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, 42, "the real row must come first and survive");
        assert!(merged[1].id < 0);

        clear_all_errors();
    }

    /// The filter arm is load-bearing: an injected row must never be MORE
    /// visible than a stored one. Without this the overlay would leak into
    /// every filtered view and make a filter look broken.
    #[test]
    fn an_injected_error_obeys_the_callers_query_filters() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_errors();

        insert_errors(&[
            err("critical one", "new", "critical"),
            err("warning one", "new", "warning"),
        ]);

        let only_critical = crate::error_monitor::types::ErrorQuery {
            severity: Some(vec![ErrorSeverity::Critical]),
            ..Default::default()
        };
        let merged = merge_with_injected_errors(Vec::new(), &only_critical);
        assert_eq!(
            merged.len(),
            1,
            "the severity filter must exclude the warning"
        );
        assert_eq!(merged[0].message, "critical one");

        let wrong_source = crate::error_monitor::types::ErrorQuery {
            log_source_name: Some("not-unit".to_string()),
            ..Default::default()
        };
        assert!(
            merge_with_injected_errors(Vec::new(), &wrong_source).is_empty(),
            "the log-source filter must exclude injected rows too"
        );

        clear_all_errors();
    }

    /// The summary must agree with the list — otherwise the page renders
    /// "0 errors" above a list of three.
    #[test]
    fn the_summary_counts_the_injected_errors() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_errors();

        insert_errors(&[
            err("a", "new", "critical"),
            err("b", "recurring", "error"),
            // A RESOLVED row must NOT inflate the unresolved-scoped counters —
            // the real summary scopes them with `AND status IN (...)`.
            err("c", "resolved", "error"),
        ]);

        let merged = merge_with_injected_summary(ErrorSummary::default());
        assert_eq!(merged.total, 3);
        assert_eq!(merged.unresolved_count, 2);
        assert_eq!(merged.critical_count, 1);
        assert_eq!(
            merged.error_count, 1,
            "the resolved error must not be counted"
        );
        assert_eq!(merged.new_count, 1);
        assert!(merged.has_actionable_errors);
        assert_eq!(merged.by_status.get("recurring"), Some(&1));

        clear_all_errors();
    }

    /// `seed-error-scenario` is clear-then-seed, exactly like its terminal
    /// sibling: a second seed must not stack on the first.
    #[test]
    fn the_error_scenario_seeder_clears_before_it_seeds() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_errors();

        let first = error_requests_from_counts(&ErrorScenarioCounts {
            new: 2,
            recurring: 1,
            ..Default::default()
        });
        insert_errors(&first);
        assert_eq!(injected_error_events().len(), 3);

        // Second scenario: clear, then seed one.
        let cleared = clear_all_errors();
        assert_eq!(cleared, 3);
        insert_errors(&error_requests_from_counts(&ErrorScenarioCounts {
            critical: 1,
            ..Default::default()
        }));

        let now = injected_error_events();
        assert_eq!(now.len(), 1, "the seeder must not stack scenarios");
        assert_eq!(now[0].severity, ErrorSeverity::Critical);

        clear_all_errors();
    }

    /// The counts vocabulary must actually produce the buckets it names.
    #[test]
    fn the_counts_vocabulary_produces_the_named_buckets() {
        let reqs = error_requests_from_counts(&ErrorScenarioCounts {
            new: 1,
            recurring: 2,
            acknowledged: 1,
            resolved: 1,
            critical: 1,
        });
        assert_eq!(reqs.len(), 6);
        let recurring: Vec<_> = reqs
            .iter()
            .filter(|r| r.status.as_deref() == Some("recurring"))
            .collect();
        assert_eq!(recurring.len(), 2);
        assert_eq!(
            recurring[0].occurrence_count,
            Some(3),
            "a recurring fake needs an occurrence count > 1 or the page's \
             recurrence column renders nothing"
        );
    }

    /// The `identityEvidence` override seam: only the three real
    /// classifications are installable, a blank/absent value clears, and the
    /// previous value is reported back so a gate can restore it.
    #[tokio::test]
    async fn force_identity_evidence_accepts_only_real_classifications() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Start from a known-clear state regardless of what ran before.
        crate::commands::session_info::set_forced_identity_evidence(None);

        let res = force_identity_evidence_handler(Some(Json(ForceIdentityEvidenceRequest {
            evidence: Some("provisional".into()),
        })))
        .await
        .expect("provisional is a real classification");
        assert_eq!(res.0.evidence.as_deref(), Some("provisional"));
        assert_eq!(res.0.previous, None);
        assert_eq!(
            crate::commands::session_info::forced_identity_evidence().as_deref(),
            Some("provisional"),
        );

        // A typo must not install an evidence string the frontend has no
        // branch for — and must not disturb the override already in force.
        let err = force_identity_evidence_handler(Some(Json(ForceIdentityEvidenceRequest {
            evidence: Some("probational".into()),
        })))
        .await
        .expect_err("an unknown classification is a 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            crate::commands::session_info::forced_identity_evidence().as_deref(),
            Some("provisional"),
        );

        // Clearing reports what it replaced.
        let cleared = force_identity_evidence_handler(Some(Json(ForceIdentityEvidenceRequest {
            evidence: None,
        })))
        .await
        .expect("clearing always succeeds");
        assert_eq!(cleared.0.evidence, None);
        assert_eq!(cleared.0.previous.as_deref(), Some("provisional"));
        assert_eq!(
            crate::commands::session_info::forced_identity_evidence(),
            None
        );
    }

    /// Teardown parity: `clear-injected` is the documented one-call teardown,
    /// so it must drop the (process-global, sticky) evidence override too.
    #[tokio::test]
    async fn clear_injected_also_clears_the_evidence_override() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::commands::session_info::set_forced_identity_evidence(Some("provisional".into()));
        let _ = clear_injected_handler().await;
        assert_eq!(
            crate::commands::session_info::forced_identity_evidence(),
            None,
            "clear-injected must clear the identityEvidence override",
        );
    }

    /// Actually BUILD the router. `routes()` panics at construction on a bad
    /// path pattern (e.g. axum 0.8 rejects the legacy `:param` capture syntax
    /// and requires `{param}`) — a failure the source-string canary above and
    /// the direct handler-call tests both miss, because neither constructs the
    /// `Router`. Only a full boot does, which is why this regressed past unit
    /// tests and only surfaced in the runner boot smoke. This test makes the
    /// construction panic a cheap, local `cargo test` failure instead.
    #[test]
    fn routes_construct_without_panic() {
        let _ = routes();
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
            resume_name: None,
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
                    confirmed_at: None,
                    restore_pending_at: None,
                    restore_tier: None,
                    origin: None,
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
                    confirmed_at: None,
                    restore_pending_at: None,
                    restore_tier: None,
                    origin: None,
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
                    confirmed_at: None,
                    restore_pending_at: None,
                    restore_tier: None,
                    origin: None,
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
                    confirmed_at: None,
                    restore_pending_at: None,
                    restore_tier: None,
                    origin: None,
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
                    confirmed_at: None,
                    restore_pending_at: None,
                    restore_tier: None,
                    origin: None,
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
                confirmed_at: None,
                restore_pending_at: None,
                restore_tier: None,
                origin: None,
            }],
        };
        let err = seed_lifecycle_store_at(&path, &bad_state, now).expect_err("bad state rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            !path.exists(),
            "a rejected seed must not write the store file"
        );
    }

    // =========================================================================
    // Agent-proxy refresh test seam (coord-mcp/seed-agent-token + agent-token).
    //
    // These cover ONLY the new seams: seeding the AGENT_TOKENS slot + an
    // Agent-bound proxy nonce from outside the spawn path, and reading the slot
    // back. They do NOT re-cover `agent_token`/`coord_mcp` internals (those have
    // their own suites). The `AGENT_TOKENS` + proxy-nonce maps are process
    // globals, so each test uses a fresh random `agent_id` to stay isolated.
    // =========================================================================

    /// seed-agent-token registers a slot observable via `lookup_agent_token`
    /// with the seeded `exp`, and the returned nonce is AGENT-bound (proves the
    /// nonce→principal binding via `proxy_principal_for_nonce`).
    #[tokio::test]
    async fn seed_agent_token_registers_slot_and_agent_bound_nonce() {
        let agent_id = Uuid::new_v4();
        let seeded_exp = chrono::Utc::now().timestamp() + 5;
        let req = SeedAgentTokenRequest {
            agent_id,
            jwt: "header.payload.sig".to_string(),
            jwt_exp: seeded_exp,
            workdir: format!("C:/tmp/agent-seam-{agent_id}"),
        };

        let resp = seed_agent_token_handler(Json(req))
            .await
            .expect("seed should succeed");
        assert!(resp.success);
        assert!(!resp.nonce.is_empty(), "a nonce must be returned");

        // The slot is observable with the seeded exp.
        let slot = crate::coord_mcp::lookup_agent_token(agent_id)
            .expect("slot must be registered after seed");
        assert_eq!(slot.read().await.exp, seeded_exp);

        // The returned nonce is AGENT-bound to this agent_id.
        let principal = crate::coord_mcp::proxy_principal_for_nonce(&resp.nonce)
            .expect("nonce must resolve to a principal");
        assert_eq!(
            principal,
            crate::coord_mcp::ProxyPrincipal::Agent { agent_id },
            "the seeded nonce must be bound to ProxyPrincipal::Agent for this agent_id"
        );

        crate::coord_mcp::remove_agent_token(agent_id);
    }

    /// seed-agent-token rejects an empty jwt / workdir with 400.
    #[tokio::test]
    async fn seed_agent_token_rejects_empty_fields() {
        let agent_id = Uuid::new_v4();
        let empty_jwt = SeedAgentTokenRequest {
            agent_id,
            jwt: "   ".to_string(),
            jwt_exp: 0,
            workdir: "C:/tmp/x".to_string(),
        };
        let err = seed_agent_token_handler(Json(empty_jwt))
            .await
            .expect_err("empty jwt rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        let empty_workdir = SeedAgentTokenRequest {
            agent_id,
            jwt: "a.b.c".to_string(),
            jwt_exp: 0,
            workdir: " ".to_string(),
        };
        let err = seed_agent_token_handler(Json(empty_workdir))
            .await
            .expect_err("empty workdir rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    /// agent-token GET returns present:true with the seeded exp/jti for a seeded
    /// slot, and present:false for an unknown agent_id. NEVER leaks the token.
    #[tokio::test]
    async fn agent_token_view_present_and_absent() {
        // Unknown agent → present:false, no exp/jti/ttl.
        let unknown = Uuid::new_v4();
        let view = agent_token_view_handler(axum::extract::Path(unknown))
            .await
            .0;
        assert!(!view.present);
        assert!(view.exp.is_none());
        assert!(view.jti.is_none());
        assert!(view.ttl_secs.is_none());

        // Seed, then read back.
        let agent_id = Uuid::new_v4();
        let seeded_exp = chrono::Utc::now().timestamp() + 3;
        let seeded = seed_agent_token_handler(Json(SeedAgentTokenRequest {
            agent_id,
            jwt: "a.b.c".to_string(),
            jwt_exp: seeded_exp,
            workdir: format!("C:/tmp/view-{agent_id}"),
        }))
        .await
        .expect("seed should succeed");
        assert!(
            !seeded.0.nonce.is_empty(),
            "seed returns a non-empty agent-bound nonce"
        );

        let view = agent_token_view_handler(axum::extract::Path(agent_id))
            .await
            .0;
        assert!(view.present);
        assert_eq!(view.exp, Some(seeded_exp));
        // jti is Uuid::nil() per the seed path.
        assert_eq!(view.jti.as_deref(), Some(Uuid::nil().to_string().as_str()));
        // ttl is roughly the seeded delta (allow a couple seconds of slack).
        let ttl = view.ttl_secs.expect("ttl present for a seeded slot");
        assert!((0..=3).contains(&ttl), "ttl_secs ~= now+3, got {ttl}");

        crate::coord_mcp::remove_agent_token(agent_id);
    }

    /// A minimal live open record, built through the seam's own converter so
    /// the fixture cannot drift from the record shape the seam writes.
    fn open_row(id: &str) -> TerminalSessionRecord {
        record_from_seed(
            &SeedLifecycleRecord {
                session_id: id.to_string(),
                state: "open".to_string(),
                last_seen_offset_ms: 0,
                closed_at_offset_ms: None,
                close_reason: None,
                page_id: None,
                zone_index: None,
                title: None,
                working_dir: None,
                confirmed_at: None,
                restore_pending_at: None,
                restore_tier: None,
                origin: None,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .expect("valid seed row")
    }

    /// A seed body of N plain `open` rows, one per id.
    fn seed_of(ids: &[&str]) -> SeedLifecycleRequest {
        SeedLifecycleRequest {
            records: ids
                .iter()
                .map(|id| SeedLifecycleRecord {
                    session_id: (*id).to_string(),
                    state: "open".to_string(),
                    last_seen_offset_ms: -1_000,
                    closed_at_offset_ms: None,
                    close_reason: None,
                    page_id: None,
                    zone_index: None,
                    title: None,
                    working_dir: None,
                    confirmed_at: None,
                    restore_pending_at: None,
                    restore_tier: None,
                    origin: None,
                })
                .collect(),
        }
    }

    // -----------------------------------------------------------------------
    // Manual-test-loop iteration 10, item 5 — the seed-lifecycle-store seam
    // silently lost the seed.
    //
    // Two ways it went missing, both answered `success: true`:
    //   (a) a stale sibling WAL replayed OVER the seeded snapshot on the next
    //       `SessionLifecycleStore::open`, resurrecting the very records the
    //       clear-then-seed contract had just discarded;
    //   (b) a RUNNING store kept the pre-seed map in memory and rewrote the
    //       whole file from it on its next persist.
    // -----------------------------------------------------------------------

    /// (a) — the seed must drop the WAL, or the file it wrote is not what the
    /// next reader sees.
    #[test]
    fn seed_lifecycle_store_drops_a_stale_wal_that_would_replay_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let now = chrono::Utc::now().timestamp_millis();

        // A pre-existing store with one row, persisted through the real write
        // path so a genuine WAL exists beside the snapshot.
        {
            let store = SessionLifecycleStore::open(&path).unwrap();
            store.record_open(open_row("pre-seed"));
        }
        let wal = crate::session::session_lifecycle_store::wal_path_for(&path);
        assert!(wal.exists(), "precondition: the write path left a WAL");

        let req = SeedLifecycleRequest {
            records: vec![SeedLifecycleRecord {
                session_id: "seeded".to_string(),
                state: "open".to_string(),
                last_seen_offset_ms: -1_000,
                closed_at_offset_ms: None,
                close_reason: None,
                page_id: None,
                zone_index: None,
                title: None,
                working_dir: None,
                confirmed_at: None,
                restore_pending_at: None,
                restore_tier: None,
                origin: None,
            }],
        };
        seed_lifecycle_store_at(&path, &req, now).expect("seed should succeed");

        assert!(!wal.exists(), "the seed must clear the sibling WAL");
        assert_eq!(
            list_lifecycle_open_at(&path),
            vec!["seeded".to_string()],
            "the next reader sees the SEED, not the WAL-replayed pre-seed row"
        );
    }

    /// (b) — a live store adopts the seeded file, so its next persist writes
    /// the SEED back instead of clobbering it with the pre-seed map.
    #[test]
    fn a_running_store_adopts_the_seed_instead_of_overwriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let now = chrono::Utc::now().timestamp_millis();

        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(open_row("pre-seed"));

        let req = SeedLifecycleRequest {
            records: vec![SeedLifecycleRecord {
                session_id: "seeded".to_string(),
                state: "open".to_string(),
                last_seen_offset_ms: -1_000,
                closed_at_offset_ms: None,
                close_reason: None,
                page_id: None,
                zone_index: None,
                title: None,
                working_dir: None,
                confirmed_at: None,
                restore_pending_at: None,
                restore_tier: None,
                origin: None,
            }],
        };
        seed_lifecycle_store_at(&path, &req, now).expect("seed should succeed");

        // WITHOUT the reload this is where the seed dies: the live store still
        // holds `pre-seed` and knows nothing of `seeded`.
        let adopted = store.reload_from_disk().expect("reload");
        assert_eq!(adopted, 1);
        let ids: Vec<String> = store
            .open_records()
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        assert_eq!(
            ids,
            vec!["seeded".to_string()],
            "the RUNNING store holds the seed"
        );

        // The next lifecycle write persists the seeded map — the pre-seed row
        // does not come back.
        store.touch("seeded");
        assert_eq!(
            list_lifecycle_open_at(&path),
            vec!["seeded".to_string()],
            "a persist after the seed writes the SEED back, not the pre-seed state"
        );
    }

    /// The reload is a whole-map REPLACE: a merge would resurrect exactly the
    /// rows the clear-then-seed contract discarded.
    #[test]
    fn reload_from_disk_replaces_rather_than_merges() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");

        let store = SessionLifecycleStore::open(&path).unwrap();
        for id in ["a", "b"] {
            store.record_open(open_row(id));
        }

        let req = SeedLifecycleRequest {
            records: vec![SeedLifecycleRecord {
                session_id: "only".to_string(),
                state: "open".to_string(),
                last_seen_offset_ms: 0,
                closed_at_offset_ms: None,
                close_reason: None,
                page_id: None,
                zone_index: None,
                title: None,
                working_dir: None,
                confirmed_at: None,
                restore_pending_at: None,
                restore_tier: None,
                origin: None,
            }],
        };
        seed_lifecycle_store_at(&path, &req, chrono::Utc::now().timestamp_millis()).unwrap();
        assert_eq!(store.reload_from_disk().unwrap(), 1);
        assert!(
            store.get("a").is_none(),
            "a merge would have kept the pre-seed rows"
        );
        assert!(store.get("only").is_some());
    }

    // -----------------------------------------------------------------------
    // Manual-test-loop iteration 21, item 1 — `clear-lifecycle-store` did not
    // clear, and its own read-back confirmed the clear anyway.
    //
    // Measured on a live runner: `clear` answered
    // `{"success":true,"removed":true}` TWICE while
    // `restore-health?include=all` kept serving all 8 rows — the handler
    // deleted the snapshot file and left the RUNNING store's in-memory map
    // untouched, so the store re-created the file on its next persist. And
    // `list-lifecycle-open` read the DELETED FILE rather than the live store,
    // so it answered `open_session_ids: []`. A clear-then-assert-empty test
    // therefore passed unconditionally: a FALSE-PASS SOURCE.
    //
    // There was no test over this handler at all before these.
    // -----------------------------------------------------------------------

    /// Seed 4 → clear → BOTH the running store (what `restore-health` reads)
    /// and the read-back report 0, and a later persist does not resurrect the
    /// rows.
    #[test]
    fn clear_lifecycle_store_empties_the_running_store_not_just_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let now = chrono::Utc::now().timestamp_millis();

        let store = SessionLifecycleStore::open(&path).unwrap();
        seed_lifecycle_store_at(&path, &seed_of(&["s1", "s2", "s3", "s4"]), now).unwrap();
        assert_eq!(
            store.reload_from_disk().unwrap(),
            4,
            "precondition: 4 seeded"
        );
        assert_eq!(list_lifecycle_open_in(&store).len(), 4);

        let (removed, _removed_wal) = clear_lifecycle_store_at(&path).expect("clear");
        assert!(removed, "the snapshot existed and was deleted");
        // The half the handler used to skip. Without it the store still holds
        // all four and rewrites them on its next persist.
        assert_eq!(store.reload_from_disk().unwrap(), 0);

        assert!(
            list_lifecycle_open_in(&store).is_empty(),
            "the RUNNING store — the one `restore-health` reads — must be empty"
        );
        assert!(
            store.open_records().is_empty(),
            "restore-health's own input must be empty"
        );
        assert!(
            list_lifecycle_open_at(&path).is_empty(),
            "and the on-disk read-back agrees"
        );

        // A lifecycle write after the clear must not bring anything back.
        store.touch("s1");
        assert!(
            list_lifecycle_open_at(&path).is_empty(),
            "a persist after the clear must not resurrect the cleared rows"
        );
    }

    /// Negative control: seed 4 → clear → seed 2 must read back EXACTLY 2 —
    /// not 6 (clear did nothing) and not 0 (the read-back is reading a stale
    /// file). A test that only ever asserts "empty" cannot tell those apart.
    #[test]
    fn clear_then_reseed_reads_back_exactly_the_reseeded_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let now = chrono::Utc::now().timestamp_millis();

        let store = SessionLifecycleStore::open(&path).unwrap();
        seed_lifecycle_store_at(&path, &seed_of(&["a1", "a2", "a3", "a4"]), now).unwrap();
        store.reload_from_disk().unwrap();

        clear_lifecycle_store_at(&path).expect("clear");
        store.reload_from_disk().unwrap();

        seed_lifecycle_store_at(&path, &seed_of(&["b1", "b2"]), now).unwrap();
        store.reload_from_disk().unwrap();

        assert_eq!(
            list_lifecycle_open_in(&store),
            vec!["b1".to_string(), "b2".to_string()],
            "exactly the reseeded rows — 6 would mean the clear was a no-op, 0 \
             would mean the read-back is not reading the live store"
        );
        assert_eq!(
            list_lifecycle_open_at(&path),
            vec!["b1".to_string(), "b2".to_string()],
        );
    }

    /// The clear must drop the sibling WAL. A surviving WAL replays its deltas
    /// over the (now absent) snapshot on the next `open()` and resurrects
    /// exactly the rows the clear discarded — the seed's own hazard, inverted.
    #[test]
    fn clear_lifecycle_store_drops_the_wal_that_would_replay_the_cleared_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let wal = crate::session::session_lifecycle_store::wal_path_for(&path);

        {
            let store = SessionLifecycleStore::open(&path).unwrap();
            store.record_open(open_row("wal-row-1"));
            store.record_open(open_row("wal-row-2"));
        }
        assert!(wal.exists(), "precondition: the write path left a WAL");

        let (_removed, removed_wal) = clear_lifecycle_store_at(&path).expect("clear");
        assert!(removed_wal, "the clear must delete the WAL");
        assert!(!wal.exists());
        assert!(
            list_lifecycle_open_at(&path).is_empty(),
            "the next reader sees an empty store, not the WAL-replayed rows"
        );
    }

    /// The read-back must read the SAME store the runner is using. This is the
    /// lie in its raw form: delete only the snapshot (what the old `clear`
    /// did) and the file read answers "empty" while the live store still holds
    /// every row.
    #[test]
    fn a_file_read_back_lies_about_a_live_store_the_in_store_read_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let now = chrono::Utc::now().timestamp_millis();

        let store = SessionLifecycleStore::open(&path).unwrap();
        seed_lifecycle_store_at(&path, &seed_of(&["live1", "live2"]), now).unwrap();
        store.reload_from_disk().unwrap();

        // Exactly the old handler: remove the snapshot, tell the live store
        // nothing.
        std::fs::remove_file(&path).unwrap();

        assert!(
            list_lifecycle_open_at(&path).is_empty(),
            "the FILE read-back reports empty — this is the false PASS"
        );
        assert_eq!(
            list_lifecycle_open_in(&store),
            vec!["live1".to_string(), "live2".to_string()],
            "…while the running store still holds both rows"
        );
    }

    /// `seed-lifecycle-store {"records": []}` stays a 400, and says where to
    /// go instead. See the module note "Why `{\"records\": []}` is still a 400".
    #[test]
    fn an_empty_seed_body_is_rejected_and_names_the_clear_route() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let err = seed_lifecycle_store_at(
            &path,
            &SeedLifecycleRequest { records: vec![] },
            chrono::Utc::now().timestamp_millis(),
        )
        .expect_err("an empty body is not a clear");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            err.1.contains("clear-lifecycle-store"),
            "the rejection must name the route that DOES empty the store, got: {}",
            err.1
        );
    }

    // -----------------------------------------------------------------------
    // Manual-test-loop iteration 11, item 3 — the seam could not express the
    // fields the DRAIN path needs.
    //
    // `record_from_seed` hardcoded `confirmed_at` / `restore_pending_at` /
    // `restore_tier` — and `origin` — to `None`. `confirmed` is one of the two
    // gates in `is_restorable_identity`, so EVERY seeded row reported
    // `restorable: false` — which meant a seeded store could never produce a
    // pending restore, `decideColdResume` never ran, and iteration 10's
    // drain-skip fix had no end-to-end path to be exercised over.
    //
    // `origin` is the second half of the same blockage, found while verifying
    // the first: `restorable` is what `restore-health` REPORTS, but what the
    // drain actually gates on is `classifyRestoreAction === "auto-resume"`,
    // which needs `origin` to be `authoritative`/`observed` as well. Fixing
    // only the timestamps would have moved the report and left the drain path
    // exactly as unreachable.
    // -----------------------------------------------------------------------

    /// A seed carrying the four fields must resolve them onto the record —
    /// the two timestamps against `now_ms` like every other offset on the
    /// struct, the tier and the origin verbatim.
    #[test]
    fn a_seed_can_now_express_confirmation_pending_tier_and_origin() {
        let now = 1_700_000_000_000i64;
        let rec = record_from_seed(
            &SeedLifecycleRecord {
                session_id: "sess-1".to_string(),
                state: "open".to_string(),
                last_seen_offset_ms: -500,
                closed_at_offset_ms: None,
                close_reason: None,
                page_id: None,
                zone_index: None,
                title: None,
                working_dir: None,
                confirmed_at: Some(-1_000),
                restore_pending_at: Some(-250),
                restore_tier: Some("failed".to_string()),
                origin: Some("authoritative".to_string()),
            },
            now,
        )
        .expect("valid seed row");

        assert_eq!(rec.confirmed_at, Some(now - 1_000));
        assert_eq!(rec.restore_pending_at, Some(now - 250));
        assert_eq!(rec.restore_tier.as_deref(), Some("failed"));
        assert_eq!(rec.origin.as_deref(), Some("authoritative"));
    }

    /// Absent stays absent — the three fields are opt-in, so every existing
    /// caller's rows are unchanged.
    #[test]
    fn a_seed_that_omits_them_still_produces_an_unconfirmed_row() {
        let rec = open_row("sess-2");
        assert_eq!(rec.confirmed_at, None);
        assert_eq!(rec.restore_pending_at, None);
        assert_eq!(rec.restore_tier, None);
        assert_eq!(rec.origin, None);
    }

    /// THE CONSEQUENCE, pinned end-to-end through the real projection:
    /// `GET /control/sessions/restore-health` reports `restorable: true` for a
    /// seeded row that is confirmed AND transcript-backed. Before the fix the
    /// `confirmed` half was unreachable, so this could only ever be `false`.
    #[test]
    fn a_confirmed_seeded_row_projects_as_restorable() {
        use crate::install_effects_producer::{project_restore_health, RestoreHealthFilter};

        #[derive(Debug)]
        struct AlwaysPresent;
        impl crate::session::snapshot_history::TranscriptProbe for AlwaysPresent {
            fn transcript_exists(&self, _id: &str, _wd: Option<&str>) -> bool {
                true
            }
        }

        let now = chrono::Utc::now().timestamp_millis();
        let seed = |id: &str, confirmed_at: Option<i64>| {
            record_from_seed(
                &SeedLifecycleRecord {
                    session_id: id.to_string(),
                    state: "open".to_string(),
                    last_seen_offset_ms: -1_000,
                    closed_at_offset_ms: None,
                    close_reason: None,
                    page_id: None,
                    zone_index: None,
                    title: None,
                    working_dir: None,
                    confirmed_at,
                    restore_pending_at: None,
                    restore_tier: None,
                    origin: None,
                },
                now,
            )
            .expect("valid seed row")
        };

        let report = project_restore_health(
            vec![seed("confirmed", Some(-1_000)), seed("unconfirmed", None)],
            &AlwaysPresent,
            RestoreHealthFilter::open_only(),
        );
        let row = |id: &str| {
            report
                .sessions
                .iter()
                .find(|s| s.claude_session_id == id)
                .unwrap_or_else(|| panic!("{id} must be reported"))
        };
        assert!(
            row("confirmed").restorable,
            "a confirmed, transcript-backed seeded row must be restorable"
        );
        assert!(row("confirmed").confirmed);
        assert!(
            !row("unconfirmed").restorable,
            "omitting confirmedAt must still yield the old, unrestorable row"
        );
        assert_eq!(report.unrestorable, 1);
    }

    /// The `pending` and `failed` buckets of the restore-health filter are now
    /// reachable from a seed too — they key off exactly these two fields.
    #[test]
    fn seeded_pending_and_failed_rows_reach_their_buckets() {
        use crate::install_effects_producer::RestoreHealthFilter;
        let now = chrono::Utc::now().timestamp_millis();

        let pending = record_from_seed(
            &SeedLifecycleRecord {
                session_id: "pending".to_string(),
                state: "closed".to_string(),
                last_seen_offset_ms: -1_000,
                closed_at_offset_ms: Some(-500),
                close_reason: Some("pty-exit".to_string()),
                page_id: None,
                zone_index: None,
                title: None,
                working_dir: None,
                confirmed_at: None,
                restore_pending_at: Some(-100),
                restore_tier: Some(
                    crate::session::session_lifecycle_store::RESTORE_TIER_FAILED.to_string(),
                ),
                origin: None,
            },
            now,
        )
        .expect("valid seed row");

        // `pending` (marker set) and `failed` (tier) both select it; `open`
        // does not — it is a closed row.
        let report = crate::install_effects_producer::project_restore_health(
            vec![pending.clone()],
            &NoTranscripts,
            RestoreHealthFilter::open_only(),
        );
        assert!(report.sessions.is_empty(), "a closed row is not `open`");

        for spec in ["pending", "failed", "closed"] {
            let filter = crate::install_effects_producer::parse_restore_health_include(Some(spec))
                .expect("a valid include spec");
            let report = crate::install_effects_producer::project_restore_health(
                vec![pending.clone()],
                &NoTranscripts,
                filter,
            );
            assert_eq!(report.sessions.len(), 1, "include={spec}");
            assert_eq!(
                report.sessions[0].restore_status, "pending (not yet confirmed)",
                "include={spec}: the rendered verdict reads off the seeded tier + marker"
            );
        }
    }

    #[derive(Debug)]
    struct NoTranscripts;
    impl crate::session::snapshot_history::TranscriptProbe for NoTranscripts {
        fn transcript_exists(&self, _id: &str, _wd: Option<&str>) -> bool {
            false
        }
    }

    // =========================================================================
    // R1: append-transcript-record — the prompts panel's live-update seam
    //
    // These tests drive the REAL reader
    // (`crate::terminal::transcript::read_user_prompts`) after every write, not
    // just the filesystem: the thing R1 exists to make verifiable is what the
    // panel's poll SEES, and a test that only asserted "a file appeared" would
    // pass for a record the reader silently drops. None of them touch the
    // `registry()` singleton, so they need no `TEST_LOCK`.
    // =========================================================================

    use crate::terminal::transcript::{read_user_prompts, session_transcript_path};

    /// A complete body for `project`/`session`, defaulted the way an
    /// out-of-process caller's minimal JSON would be.
    fn append_req(
        config_dir: &Path,
        project_path: &str,
        session_id: &str,
        kind: TranscriptRecordKind,
    ) -> AppendTranscriptRecordRequest {
        AppendTranscriptRecordRequest {
            config_dir: config_dir.display().to_string(),
            project_path: project_path.to_string(),
            session_id: session_id.to_string(),
            kind,
            text: None,
            uuid: None,
            timestamp: None,
            reset: false,
        }
    }

    /// The file must land at EXACTLY the path `read_user_prompts` opens — that
    /// is the whole point of routing through `session_transcript_path` instead
    /// of re-deriving the project-path encoding. Asserted against the reader's
    /// own construction, and then against the reader actually returning it.
    #[test]
    fn append_transcript_record_lands_at_the_path_the_reader_opens() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("cfg");
        let project_path = r"D:\some\project_with_underscore";

        let req = append_req(
            &config_dir,
            project_path,
            "sess",
            TranscriptRecordKind::Prompt,
        );
        let resp = append_transcript_record_core(&req).expect("append succeeds");

        let expected = session_transcript_path(&config_dir, project_path, "sess");
        assert_eq!(
            std::path::PathBuf::from(&resp.path),
            expected,
            "the fixture must write the path the reader opens, encoding included"
        );
        assert!(expected.exists(), "the transcript file was created");
        assert!(resp.created, "a first append reports it created the file");
        assert_eq!(
            resp.previous_mtime_ms, None,
            "there was no file to have an mtime"
        );
        assert_eq!(resp.records_after, 1);
    }

    /// Pin the CONSEQUENCE: the reader returns the appended record, with the
    /// uuid and text the fixture reported.
    #[test]
    fn append_transcript_record_is_returned_by_the_real_reader() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("cfg");
        let project_path = r"D:\some\project";

        let mut req = append_req(
            &config_dir,
            project_path,
            "sess",
            TranscriptRecordKind::Prompt,
        );
        req.text = Some("run the manual test again".to_string());
        let resp = append_transcript_record_core(&req).expect("append succeeds");
        assert!(resp.visible_to_reader, "a plain prompt is surfaced");
        assert_eq!(resp.prompts_after, 1);

        let out = read_user_prompts(&config_dir, project_path, "sess", None).unwrap();
        assert!(!out.unchanged);
        assert_eq!(
            out.prompts.len(),
            1,
            "the reader sees exactly the appended record"
        );
        assert_eq!(out.prompts[0].uuid, resp.uuid);
        assert_eq!(out.prompts[0].text, "run the manual test again");
        assert_eq!(
            out.mtime_ms, resp.mtime_ms,
            "the response's mtime_ms is the reader's own value, handed back verbatim"
        );
    }

    /// The reason this route exists. A poll holds the mtime from its last read;
    /// if the append lands inside that same millisecond the reader
    /// short-circuits and the panel never updates. Assert the append is visible
    /// to a reader carrying the PRE-append mtime.
    #[test]
    fn append_moves_the_mtime_so_the_next_read_is_not_short_circuited() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("cfg");
        let project_path = r"D:\some\project";

        let seed = append_req(
            &config_dir,
            project_path,
            "sess",
            TranscriptRecordKind::Prompt,
        );
        append_transcript_record_core(&seed).expect("seed append succeeds");

        // What a polling caller holds after its last read.
        let first = read_user_prompts(&config_dir, project_path, "sess", None).unwrap();
        assert_eq!(first.prompts.len(), 1);
        // Same mtime handed back short-circuits — the state the panel is in
        // between appends.
        let idle =
            read_user_prompts(&config_dir, project_path, "sess", Some(first.mtime_ms)).unwrap();
        assert!(
            idle.unchanged,
            "the reader short-circuits on an unchanged mtime"
        );

        let resp = append_transcript_record_core(&seed).expect("second append succeeds");
        assert_eq!(resp.previous_mtime_ms, Some(first.mtime_ms));
        assert_ne!(
            resp.mtime_ms, first.mtime_ms,
            "the fixture must guarantee the mtime moved, bumping it if the write \
             landed inside the previous tick"
        );

        let after =
            read_user_prompts(&config_dir, project_path, "sess", Some(first.mtime_ms)).unwrap();
        assert!(
            !after.unchanged,
            "the poll must see a change — this is the live-update path R1 covers"
        );
        assert_eq!(after.prompts.len(), 2, "and it must see the new record");
    }

    /// The collision hazard is back-to-back appends, which on a fast filesystem
    /// share one mtime tick. Every one of them must still move the mtime.
    #[test]
    fn back_to_back_appends_each_move_the_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("cfg");
        let project_path = r"D:\some\project";
        let req = append_req(
            &config_dir,
            project_path,
            "sess",
            TranscriptRecordKind::Prompt,
        );

        let mut last: Option<u64> = None;
        for i in 0..6 {
            let resp = append_transcript_record_core(&req).expect("append succeeds");
            assert_eq!(
                resp.previous_mtime_ms, last,
                "append #{i} must report the mtime it started from"
            );
            if let Some(previous) = last {
                assert!(
                    resp.mtime_ms > previous,
                    "append #{i} left the mtime at {} (was {previous}) — a poll holding \
                     the old value would miss it, or worse see it go backwards",
                    resp.mtime_ms
                );
            }
            last = Some(resp.mtime_ms);
        }
        let out = read_user_prompts(&config_dir, project_path, "sess", None).unwrap();
        assert_eq!(
            out.prompts.len(),
            6,
            "all six appends are in the transcript"
        );
    }

    /// Every selectable kind must reach the verdict the section header claims —
    /// and the verdict is the READER's, measured, not the fixture's prediction.
    #[test]
    fn every_record_kind_reaches_the_readers_expected_verdict() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("cfg");
        let project_path = r"D:\some\project";

        let cases = [
            (TranscriptRecordKind::Prompt, true),
            (TranscriptRecordKind::MetaExpansion, false),
            (TranscriptRecordKind::CompactSummary, false),
            (TranscriptRecordKind::Sidechain, false),
            (TranscriptRecordKind::ToolResult, false),
            (TranscriptRecordKind::Assistant, false),
        ];

        let mut expected_prompts = 0usize;
        let mut expected_records = 0usize;
        let mut surviving_uuids = Vec::new();
        for (kind, should_surface) in cases {
            let req = append_req(&config_dir, project_path, "sess", kind);
            let resp = append_transcript_record_core(&req).expect("append succeeds");
            assert_eq!(
                resp.visible_to_reader, should_surface,
                "{kind:?}: the reader's verdict disagrees with the documented table"
            );
            expected_records += 1;
            if should_surface {
                expected_prompts += 1;
                surviving_uuids.push(resp.uuid.clone());
            }
            assert_eq!(
                resp.prompts_after, expected_prompts,
                "{kind:?}: prompt count"
            );
            assert_eq!(
                resp.records_after, expected_records,
                "{kind:?}: record count"
            );
        }

        // Every kind was written; only the prompt survives the filter.
        let out = read_user_prompts(&config_dir, project_path, "sess", None).unwrap();
        let ids: Vec<&str> = out.prompts.iter().map(|p| p.uuid.as_str()).collect();
        assert_eq!(
            ids, surviving_uuids,
            "exactly the operator-authored prompt reaches the panel"
        );
    }

    /// `reset` gives a manual test a known empty starting state without needing
    /// a second route to clear the file; the default appends instead.
    #[test]
    fn reset_truncates_before_appending_and_the_default_appends() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("cfg");
        let project_path = r"D:\some\project";
        let req = append_req(
            &config_dir,
            project_path,
            "sess",
            TranscriptRecordKind::Prompt,
        );

        append_transcript_record_core(&req).unwrap();
        let second = append_transcript_record_core(&req).unwrap();
        assert_eq!(
            second.records_after, 2,
            "the default is append, not overwrite"
        );
        assert!(!second.created, "the file already existed");

        let mut reset = req.clone();
        reset.reset = true;
        let resp = append_transcript_record_core(&reset).unwrap();
        assert_eq!(resp.records_after, 1, "reset truncates first");
        assert_eq!(resp.prompts_after, 1);
        let out = read_user_prompts(&config_dir, project_path, "sess", None).unwrap();
        assert_eq!(out.prompts.len(), 1);
        assert_eq!(out.prompts[0].uuid, resp.uuid);
    }

    /// A transcript whose last line has no trailing newline must not have the
    /// new record welded onto it — that would corrupt BOTH records, and the
    /// reader skips malformed lines silently, so the append would just vanish.
    #[test]
    fn an_append_onto_a_newline_less_transcript_stays_valid_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("cfg");
        let project_path = r"D:\some\project";
        let path = session_transcript_path(&config_dir, project_path, "sess");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // No trailing newline, exactly as a truncated/hand-written file looks.
        std::fs::write(
            &path,
            r#"{"type":"user","uuid":"pre","timestamp":"t0","message":{"role":"user","content":"already here"}}"#,
        )
        .unwrap();

        let req = append_req(
            &config_dir,
            project_path,
            "sess",
            TranscriptRecordKind::Prompt,
        );
        let resp = append_transcript_record_core(&req).expect("append succeeds");
        assert_eq!(resp.records_after, 2);

        let out = read_user_prompts(&config_dir, project_path, "sess", None).unwrap();
        let ids: Vec<&str> = out.prompts.iter().map(|p| p.uuid.as_str()).collect();
        assert_eq!(
            ids,
            vec!["pre", resp.uuid.as_str()],
            "both the pre-existing record and the appended one parse"
        );
    }

    /// A caller-supplied uuid/timestamp is used verbatim, so a gate can pin the
    /// exact card it expects the panel to render.
    #[test]
    fn a_caller_supplied_uuid_and_timestamp_are_used_verbatim() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("cfg");
        let project_path = r"D:\some\project";
        let mut req = append_req(
            &config_dir,
            project_path,
            "sess",
            TranscriptRecordKind::Prompt,
        );
        req.uuid = Some("pinned-uuid".to_string());
        req.timestamp = Some("2026-08-26T12:00:00Z".to_string());
        req.text = Some("pinned text".to_string());

        let resp = append_transcript_record_core(&req).unwrap();
        assert_eq!(resp.uuid, "pinned-uuid");

        let out = read_user_prompts(&config_dir, project_path, "sess", None).unwrap();
        assert_eq!(out.prompts[0].uuid, "pinned-uuid");
        assert_eq!(out.prompts[0].timestamp, "2026-08-26T12:00:00Z");
        assert_eq!(out.prompts[0].text, "pinned text");
    }

    /// Validation: the triple is required, and `session_id` is refused if it
    /// could escape the transcript directory — it is interpolated into a
    /// filename, and the reader could never open such a path anyway.
    #[test]
    fn append_rejects_an_empty_triple_or_an_escaping_session_id() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("cfg");
        let project_path = r"D:\some\project";

        for field in ["config_dir", "project_path", "session_id"] {
            let mut req = append_req(
                &config_dir,
                project_path,
                "sess",
                TranscriptRecordKind::Prompt,
            );
            match field {
                "config_dir" => req.config_dir = "   ".to_string(),
                "project_path" => req.project_path = String::new(),
                _ => req.session_id = " ".to_string(),
            }
            let err = append_transcript_record_core(&req)
                .err()
                .unwrap_or_else(|| panic!("an empty {field} must be rejected"));
            assert_eq!(err.0, StatusCode::BAD_REQUEST, "{field}");
        }

        for bad in [
            "../escape",
            r"..\escape",
            "nested/sess",
            r"nested\sess",
            "C:sess",
        ] {
            let mut req = append_req(
                &config_dir,
                project_path,
                "sess",
                TranscriptRecordKind::Prompt,
            );
            req.session_id = bad.to_string();
            let err = append_transcript_record_core(&req)
                .err()
                .unwrap_or_else(|| panic!("session_id {bad:?} must be rejected"));
            assert_eq!(err.0, StatusCode::BAD_REQUEST, "{bad:?}");
        }
    }

    /// The record shapes are the ones Claude Code actually writes, so the
    /// machine-flag predicate the reader uses must agree with what this fixture
    /// emits — checked against `is_machine_authored_user_record` directly so a
    /// renamed flag fails here rather than silently surfacing machine text.
    #[test]
    fn the_written_flags_are_the_ones_the_reader_filters_on() {
        for (kind, flag) in [
            (TranscriptRecordKind::MetaExpansion, "isMeta"),
            (TranscriptRecordKind::CompactSummary, "isCompactSummary"),
            (TranscriptRecordKind::Sidechain, "isSidechain"),
        ] {
            let record = build_transcript_record(kind, "u", "t", "body");
            assert_eq!(
                record.get(flag).and_then(|v| v.as_bool()),
                Some(true),
                "{kind:?} must carry {flag}"
            );
            assert!(
                crate::terminal::transcript::is_machine_authored_user_record(&record),
                "{kind:?} must read as machine-authored to the reader"
            );
        }
        let prompt = build_transcript_record(TranscriptRecordKind::Prompt, "u", "t", "body");
        assert!(
            !crate::terminal::transcript::is_machine_authored_user_record(&prompt),
            "a plain prompt must never read as machine-authored"
        );
    }

    /// The kind selector is part of the wire contract — a renamed variant
    /// silently breaks every stored body a manual test uses.
    #[test]
    fn record_kinds_serialize_with_their_documented_wire_names() {
        for (kind, wire) in [
            (TranscriptRecordKind::Prompt, "\"prompt\""),
            (TranscriptRecordKind::MetaExpansion, "\"meta_expansion\""),
            (TranscriptRecordKind::CompactSummary, "\"compact_summary\""),
            (TranscriptRecordKind::Sidechain, "\"sidechain\""),
            (TranscriptRecordKind::ToolResult, "\"tool_result\""),
            (TranscriptRecordKind::Assistant, "\"assistant\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), wire);
        }
        // And a body that omits `kind` defaults to a plain prompt.
        let req: AppendTranscriptRecordRequest = serde_json::from_str(
            r#"{"config_dir":"C:/cfg","project_path":"D:/p","session_id":"s"}"#,
        )
        .expect("the minimal documented body deserializes");
        assert_eq!(req.kind, TranscriptRecordKind::Prompt);
        assert!(!req.reset);
    }
}
