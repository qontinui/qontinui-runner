//! MCP API Server
//!
//! Provides an HTTP API for the MCP server to communicate with the runner.
//! This allows Claude Code (running in WSL) to control the Windows runner.
//!
//! # Multi-Monitor Coordinate System
//!
//! Windows uses a "virtual desktop" coordinate system where all monitors are combined
//! into one large coordinate space. The primary monitor is usually at (0, 0), and other
//! monitors can have negative coordinates if positioned to the left or above.
//!
//! ## Example 3-Monitor Setup:
//! ```text
//!     Left Monitor        Primary Monitor       Right Monitor
//!     (-1920, 702)        (0, 0)                (3840, 702)
//!     1920x1080           3840x2160             1920x1080
//!
//!     Virtual Desktop Origin: (-1920, 0) - the minimum X and Y across all monitors
//!     Virtual Desktop Size: 7680x2160
//! ```
//!
//! ## Key Insight: FIND vs CLICK Coordinates
//!
//! When the FIND action captures a screenshot, it captures the **entire virtual desktop**
//! (all monitors combined). The coordinates returned by FIND are relative to the
//! **virtual desktop origin** (the minimum X, minimum Y point across all monitors).
//!
//! When a CLICK action targets the FIND result, pyautogui needs **absolute virtual
//! desktop coordinates** to position the mouse correctly.

#![allow(dead_code)]
//!
//! ## The Offset Calculation
//!
//! The `monitor_offset_x` and `monitor_offset_y` values passed to Python represent
//! the **virtual desktop origin** - NOT a specific monitor's position.
//!
//! ```text
//! Example: User clicks on left monitor at FIND result (65, 1372)
//!
//! Virtual desktop origin: (-1920, 0)  ← minimum X and Y across all monitors
//! FIND result (relative to screenshot): (65, 1372)
//! Final absolute coordinates: (65 + -1920, 1372 + 0) = (-1855, 1372)
//!
//! This correctly places the click on the left monitor!
//! ```
//!
//! ## Common Pitfall (Fixed)
//!
//! Previously, the code incorrectly used the **specific monitor's position** as the offset.
//! For the left monitor at (-1920, 702), this added 702 to the Y coordinate, causing clicks
//! to land on the wrong monitor (702 pixels too low).
//!
//! The fix: Always calculate the virtual desktop origin (min X, min Y across all monitors)
//! regardless of which monitor is specified, because FIND always captures the full virtual desktop.

use async_graphql_axum::{GraphQL, GraphQLSubscription};
use axum::{
    response::Json,
    routing::{get, post},
    Router,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::action_service::UnifiedActionService;
use crate::commands::rag::RAGState;
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::mcp::awas::{
    awas_check_support, awas_discover, awas_execute, awas_extract_elements, awas_list_actions,
};

use crate::mcp::shared::get_workspace_paths_internal;
use crate::mcp::types::ApiState;
use crate::str_utils::truncate_str;

// Cached embedding-service probe state. Module-scope so heartbeats can read
// the reachability bit without running the full async probe path — the
// /health handler is the sole writer (compare_exchange-gated on 30s stale).
static EMBEDDING_LAST_CHECK_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static EMBEDDING_LAST_REACHABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static EMBEDDING_LAST_CHECKED_AT_LEAST_ONCE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static EMBEDDING_LAST_ERROR: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

/// Read the most recent cached reachability of the embedding service.
///
/// Returns `None` until the first probe completes (/health path runs the
/// probe; heartbeats read this value). Callers treat `None` as "unknown —
/// don't flag as degraded yet" to avoid false positives during boot.
pub fn embedding_reachable_cached() -> Option<bool> {
    if EMBEDDING_LAST_CHECKED_AT_LEAST_ONCE.load(Ordering::Acquire) {
        Some(EMBEDDING_LAST_REACHABLE.load(Ordering::Acquire))
    } else {
        None
    }
}

/// Cached embedding-service health probe.  Calls GET /api/embeddings/status
/// at most once every 30 seconds.  Returns a JSON value suitable for inlining
/// into the `/health` response.
async fn embedding_service_health() -> serde_json::Value {
    let url = crate::database::embedding_client::EmbeddingClient::default_url();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let prev = EMBEDDING_LAST_CHECK_MS.load(Ordering::Relaxed);
    let stale = now_ms.saturating_sub(prev) > 30_000;

    if stale
        && EMBEDDING_LAST_CHECK_MS
            .compare_exchange(prev, now_ms, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    {
        // Status endpoint is at the same base minus the last path segment.
        let status_url = url.replace("/compute-text", "/status");
        let ok = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
        {
            Ok(c) => match c.get(&status_url).send().await {
                Ok(r) => r.status().is_success(),
                Err(e) => {
                    let err_mtx = EMBEDDING_LAST_ERROR.get_or_init(|| std::sync::Mutex::new(None));
                    if let Ok(mut g) = err_mtx.lock() {
                        *g = Some(e.to_string());
                    }
                    false
                }
            },
            Err(e) => {
                let err_mtx = EMBEDDING_LAST_ERROR.get_or_init(|| std::sync::Mutex::new(None));
                if let Ok(mut g) = err_mtx.lock() {
                    *g = Some(format!("Failed to build HTTP client: {e}"));
                }
                false
            }
        };
        EMBEDDING_LAST_REACHABLE.store(ok, Ordering::Release);
        EMBEDDING_LAST_CHECKED_AT_LEAST_ONCE.store(true, Ordering::Release);
        if ok {
            let err_mtx = EMBEDDING_LAST_ERROR.get_or_init(|| std::sync::Mutex::new(None));
            if let Ok(mut g) = err_mtx.lock() {
                *g = None;
            }
        }
    }

    let reachable = EMBEDDING_LAST_REACHABLE.load(Ordering::Acquire);
    let err_msg = EMBEDDING_LAST_ERROR
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|g| g.clone());

    serde_json::json!({
        "reachable": reachable,
        "url": url,
        "lastCheckMs": EMBEDDING_LAST_CHECK_MS.load(Ordering::Relaxed),
        "lastErrorMessage": err_msg,
    })
}

/// Bounded PG liveness probe for `/health` (iter4 B-5). Runs a `SELECT 1`
/// through the global deadpool pool with a hard 2s ceiling *on top of* the
/// pool's own bounded `get()` timeout (B-1), so the health handler can never
/// hang on a wedged data layer even in a pathological pool state.
///
/// Returns:
/// * `None` — no PG configured (the global `PgDb` was never set). Do not
///   downgrade a runner that legitimately runs without PG.
/// * `Some(true)` — a connection was checked out and `SELECT 1` succeeded
///   within the window.
/// * `Some(false)` — the pool errored (unreachable / exhausted, incl. the
///   degraded-boot pool with no live backend), or the probe exceeded 2s.
///
/// Unlike the embedding probe this is intentionally NOT cached: it is a single
/// sub-millisecond round-trip on a healthy DB, and when the DB is down the 2s
/// ceiling bounds the cost — surfacing the outage on the very next `/health`
/// poll is the whole point (the B-5 observability gap the plan calls out).
async fn pg_liveness_probe() -> Option<bool> {
    let pg = crate::database::pg::PgDb::try_global()?;
    let probe = async move {
        let conn = pg.pool().get().await.map_err(|e| e.to_string())?;
        conn.simple_query("SELECT 1")
            .await
            .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    };
    match tokio::time::timeout(std::time::Duration::from_secs(2), probe).await {
        Ok(Ok(())) => Some(true),
        Ok(Err(_)) => Some(false),
        Err(_) => Some(false),
    }
}

// ---------------------------------------------------------------------------
// PR-credential probe (plan qontinui-pr-credential-provisioning, Phase 0)
// ---------------------------------------------------------------------------
// Cached `gh auth status` probe surfaced on `/health` as `prCredential`, so
// fleet consumers can see "this machine has no PR credential" instead of
// discovering it when an agent's `gh pr create` fails. Same single-flight
// compare_exchange discipline as the embedding probe, but the process spawn
// runs on a DETACHED blocking task — /health never waits on it (first call
// returns a pending shape and kicks the probe).

/// Probe TTL: re-run `gh auth status` at most once every 5 minutes.
const PR_CRED_PROBE_TTL_MS: u64 = 5 * 60 * 1000;
/// Hard cap on how long a single `gh auth status` child may run before it is
/// killed and the probe resolves to the `unknown` state. Without this a hung
/// `gh` (network stall, credential-helper prompt) would pin a blocking-pool
/// thread forever — and the TTL would keep kicking NEW probes on top of it.
const PR_CRED_PROBE_CHILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

static PR_CRED_LAST_KICK_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// In-flight guard: exactly one probe child at a time. The TTL timestamp alone
/// is NOT the gate — a kick is taken only when this flag is acquired, so a
/// probe that has not resolved yet can never be stacked on.
static PR_CRED_PROBE_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// One-shot latch for the Phase-0 "no PR credential" startup warning — the
/// tracing line fires on the FIRST unauthenticated resolution only.
static PR_CRED_WARNED_UNAUTHENTICATED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The most recent resolved probe result (`None` until the first probe
/// completes). Written only by [`run_pr_credential_probe`].
#[derive(Clone, Debug)]
struct PrCredentialProbe {
    /// `Some(true)` authenticated, `Some(false)` no credential, `None` the
    /// probe could not determine it (child timed out and was killed).
    gh_cli_authenticated: Option<bool>,
    checked_at_ms: u64,
    /// Actionable hint, present when unauthenticated or unknown.
    hint: Option<String>,
}

fn pr_cred_result_slot() -> &'static std::sync::Mutex<Option<PrCredentialProbe>> {
    static SLOT: std::sync::OnceLock<std::sync::Mutex<Option<PrCredentialProbe>>> =
        std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new(None))
}

/// Wait on `child` for at most `timeout`. Returns `Some(status)` when it
/// exits in time; on expiry (or a wait error) kills the child, reaps it, and
/// returns `None`. Blocking (poll + sleep) — call off the async executor.
fn wait_child_with_timeout(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Run `gh auth status` (blocking — call off the async executor) and resolve a
/// [`PrCredentialProbe`]. Exit code 0 ⇒ authenticated; non-zero ⇒ no credential;
/// a missing `gh` binary resolves unauthenticated with a distinct hint. The
/// child is hard-capped at [`PR_CRED_PROBE_CHILD_TIMEOUT`] — on expiry it is
/// KILLED and the probe resolves to the `unknown` state (never a leaked
/// blocking-pool thread).
fn run_pr_credential_probe() -> PrCredentialProbe {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let spawned = crate::process_helpers::no_window("gh")
        .args(["auth", "status"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let (authenticated, hint) = match spawned {
        Ok(mut child) => match wait_child_with_timeout(&mut child, PR_CRED_PROBE_CHILD_TIMEOUT) {
            Some(status) if status.success() => (Some(true), None),
            Some(_) => (
                Some(false),
                Some(
                    "no PR credential — `gh auth login` is the interim unblock; \
                     `qontinui-pr create` (coord-brokered) needs no personal login"
                        .to_string(),
                ),
            ),
            None => (
                None,
                Some(format!(
                    "gh auth status did not finish within {}s and was killed — \
                     credential state unknown; `qontinui-pr create` \
                     (coord-brokered) needs no personal login",
                    PR_CRED_PROBE_CHILD_TIMEOUT.as_secs()
                )),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            Some(false),
            Some(
                "gh CLI not installed — `qontinui-pr create` (coord-brokered) \
                 needs no personal login"
                    .to_string(),
            ),
        ),
        Err(e) => (
            Some(false),
            Some(format!("gh auth status probe failed: {e}")),
        ),
    };
    if authenticated == Some(false)
        && PR_CRED_WARNED_UNAUTHENTICATED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    {
        tracing::warn!(
            "no PR credential on this machine — `gh auth login` is the interim \
             unblock; the coord-brokered PR path removes this requirement"
        );
    }
    PrCredentialProbe {
        gh_cli_authenticated: authenticated,
        checked_at_ms: now_ms,
        hint,
    }
}

/// Decide whether THIS call should kick a fresh probe, and if so acquire the
/// in-flight flag + advance the TTL timestamp. Split from
/// [`pr_credential_health`] so the guard logic is unit-testable. A kick is
/// taken only when (a) the TTL has expired AND (b) no probe is currently in
/// flight — the timestamp alone is never the gate, so an unresolved probe can
/// not be stacked on. The caller that receives `true` MUST run the probe and
/// release the flag via [`pr_cred_probe_finished`].
fn pr_cred_try_begin_probe(now_ms: u64) -> bool {
    let prev = PR_CRED_LAST_KICK_MS.load(Ordering::Relaxed);
    let stale = now_ms.saturating_sub(prev) > PR_CRED_PROBE_TTL_MS;
    if !stale {
        return false;
    }
    if PR_CRED_PROBE_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        // A prior probe is still running (bounded by the child timeout) —
        // do not stack another on top of it.
        return false;
    }
    PR_CRED_LAST_KICK_MS.store(now_ms, Ordering::Relaxed);
    true
}

/// Release the probe in-flight flag (after storing the result).
fn pr_cred_probe_finished() {
    PR_CRED_PROBE_IN_FLIGHT.store(false, Ordering::Release);
}

/// Build the `/health` `prCredential` section from the cached probe, kicking a
/// fresh DETACHED probe when the cache is stale and no probe is already in
/// flight ([`pr_cred_try_begin_probe`]). NEVER blocks: before the first probe
/// resolves this returns a `pending` shape.
fn pr_credential_health() -> serde_json::Value {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if pr_cred_try_begin_probe(now_ms) {
        // Off the hot path: the process spawn runs on a blocking thread and
        // stores its result for the NEXT /health read. The child is hard-capped
        // at PR_CRED_PROBE_CHILD_TIMEOUT, so the in-flight flag always frees.
        tokio::task::spawn_blocking(|| {
            let probe = run_pr_credential_probe();
            if let Ok(mut slot) = pr_cred_result_slot().lock() {
                *slot = Some(probe);
            }
            pr_cred_probe_finished();
        });
    }
    let cached = pr_cred_result_slot().lock().ok().and_then(|g| g.clone());
    match cached {
        Some(p) => serde_json::json!({
            // `unknown` = the last child timed out; distinct from `pending`
            // (no probe has resolved yet) so consumers can tell "gh is
            // wedged" from "still starting up".
            "state": if p.gh_cli_authenticated.is_some() { "resolved" } else { "unknown" },
            "ghCliAuthenticated": p.gh_cli_authenticated,
            "checkedAt": p.checked_at_ms,
            "hint": p.hint,
        }),
        None => serde_json::json!({
            "state": "pending",
            "ghCliAuthenticated": serde_json::Value::Null,
            "checkedAt": serde_json::Value::Null,
            "hint": serde_json::Value::Null,
        }),
    }
}

/// Health check endpoint (also served at `/ui-bridge/health` and
/// `/ui-bridge/status` — all three share this handler).
/// Includes `uiBridge` metadata so the app discovery scanner can detect the runner.
/// Returns rich diagnostics: frontend responsiveness, uptime, circuit breaker state.
async fn health(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    let uptime_secs = state.started_at.elapsed().as_secs();
    let last_pong = state.app_state.ui_bridge_last_pong.load(Ordering::Relaxed);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    // `last_pong` is a wall-clock stamp, so a backwards clock step (NTP
    // correction, sleep/resume) can leave it AHEAD of `now_ms`. A plain
    // subtraction underflows and panics here — in the one handler the
    // supervisor polls. Saturate: a pong from the "future" reads as age 0,
    // i.e. maximally fresh, which is the honest answer.
    let pong_age_ms = if last_pong > 0 {
        now_ms.saturating_sub(last_pong)
    } else {
        0
    };
    let responsive = last_pong > 0 && pong_age_ms < 15000;

    let pending_count = state
        .ui_bridge_pending_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let circuit_breaker_state = state.ui_bridge_circuit_breaker.get_state().await;

    let status = if last_pong > 0 { "ok" } else { "starting" };
    let console_errors = state.ui_bridge_console_error_count.load(Ordering::Relaxed);

    // AI provider circuit breaker states
    let ai_provider_states: Vec<serde_json::Value> =
        crate::ai_provider::circuit_breaker::all_provider_states()
            .into_iter()
            .map(|(key, cb_state)| {
                let available = crate::ai_provider::circuit_breaker::is_provider_available(&key);
                serde_json::json!({
                    "providerKey": key,
                    "state": cb_state.to_string(),
                    "available": available,
                })
            })
            .collect();

    // When the frontend has not connected after 30s of uptime, auto-attach a
    // native window screenshot so health consumers (agents, supervisor) can see
    // what the webview is actually showing (e.g., ERR_CONNECTION_REFUSED).
    let diagnostic_screenshot = if last_pong == 0 && uptime_secs >= 30 {
        crate::mcp::ui_bridge::capture_runner_window_base64(&state).await
    } else {
        None
    };

    // Embedding service health probe (cached, refreshed every 30s).
    let embedding_health = embedding_service_health().await;

    // Expose the instanceStorage port-namespace suffix so test scripts can
    // compute keys mechanically instead of reading instance-storage.ts. The
    // rule (see qontinui-runner/src/lib/instance-storage.ts::namespacedKey):
    // primary (9876) uses bare keys, every other port suffixes ":<port>".
    // Tests use this to read/write per-instance flags like
    // "specs.useAiGeneration" via `<key><suffix>`.
    let api_port = state.app_state.api_port.load(Ordering::Relaxed);
    let storage_namespace_suffix = if api_port == 9876 {
        String::new()
    } else {
        format!(":{}", api_port)
    };

    // AI configuration & runtime status — distinguishes "not configured" from
    // "configured but idle" from "actively running".
    let ai_settings = crate::settings::get_ai_settings();
    let ai_configured = true; // A provider is always selected (enum has a default)
    let ai_running = {
        let pids =
            crate::safe_lock::safe_lock_or_recover(&state.current_ai_pids, "current_ai_pids");
        !pids.is_empty()
    };
    let ai_provider_name: &str = match &ai_settings.provider {
        crate::settings::AiProvider::ClaudeCli => "claude_cli",
        crate::settings::AiProvider::ClaudeApi => "claude_api",
        crate::settings::AiProvider::GeminiCli => "gemini_cli",
        crate::settings::AiProvider::GeminiApi => "gemini_api",
        crate::settings::AiProvider::PiCli => "pi_cli",
        crate::settings::AiProvider::Ollama => "ollama",
        crate::settings::AiProvider::OpenAiCompatible => "openai_compatible",
    };
    let ai_model: Option<String> = match &ai_settings.provider {
        crate::settings::AiProvider::ClaudeCli => None, // CLI manages its own model selection
        crate::settings::AiProvider::ClaudeApi => Some(ai_settings.claude_api.model.clone()),
        crate::settings::AiProvider::GeminiCli => Some(ai_settings.gemini_cli.model.clone()),
        crate::settings::AiProvider::GeminiApi => Some(ai_settings.gemini_api.model.clone()),
        // pi manages its own model selection when none is configured
        crate::settings::AiProvider::PiCli => ai_settings.pi_cli.model.clone(),
        crate::settings::AiProvider::Ollama => Some(ai_settings.ollama.model.clone()),
        crate::settings::AiProvider::OpenAiCompatible => {
            Some(ai_settings.openai_compatible.model.clone())
        }
    };

    // Phase 3J.2 — surface the in-memory UI error state captured by the React
    // ErrorBoundary. `derived_status` is an additive field; the existing
    // `status` ("ok" / "starting") literal is preserved so older consumers
    // keep working.
    //
    // The `ui_error` object is null when no error is tracked.
    let ui_error_snapshot = state.app_state.ui_error.get().await;
    let ui_error_json = match &ui_error_snapshot {
        Some(err) => serde_json::to_value(err).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    };

    // Post-3J follow-up — surface the most recent Rust crash dump picked up
    // at startup. React's ErrorBoundary can't report non-unwinding panics
    // (the process aborts across the WebView2 FFI boundary before the
    // boundary catches anything), so a runner that's been force-restarted
    // after a Rust panic looks healthy to fleet consumers unless we surface
    // the disk artifact here.
    let recent_crash_snapshot = state.app_state.crash_dumps.get().await;
    let recent_crash_json = match &recent_crash_snapshot {
        Some(c) => serde_json::to_value(c).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    };

    // Derived status: either a live UI error OR a fresh crash dump tips the
    // runner to `errored`. An embedding-service outage downgrades to
    // `degraded` (functional, but semantic-search is unavailable). Callers
    // who care about the distinction should branch on `ui_error` /
    // `recent_crash` / `embeddingService.reachable` presence instead of
    // re-parsing the compound status string.
    // iter4 B-5: bounded PG liveness. `/health` previously reported "healthy"
    // in 3ms while every PG-backed panel hung forever (B-1). Fold a hard-capped
    // `SELECT 1` into the aggregation so a wedged/unreachable data layer
    // downgrades the runner to `degraded`. `None` (no PG configured) does NOT
    // downgrade. Depends on B-1's pool timeout — plus a 2s ceiling here — so
    // the probe itself can never hang the handler.
    let pg_reachable = pg_liveness_probe().await;
    // A dead WebView2 host leaves the Rust backend serving happily, so neither
    // `ui_error` (React error boundary) nor `recent_crash` (startup-only dump
    // scan) can see it. `last_pong` can — it advances off the unconditional 3s
    // ping loop for as long as any UI is alive.
    let ui_dead =
        crate::ui_error::ui_stale(last_pong, pong_age_ms, crate::ui_error::UI_DEAD_AFTER_MS);
    let derived_status = crate::ui_error::compute_derived_status(
        ui_error_snapshot.is_some(),
        recent_crash_snapshot.is_some(),
        Some(ui_dead),
        embedding_reachable_cached(),
        pg_reachable,
    );

    // `frontendReady` is DERIVED, not latched (2026-08-05).
    //
    // It used to be `AppState::frontend_ready`, a one-way latch flipped by the
    // first successful UI Bridge IPC response. Nothing inside the runner ever
    // drives that path — the 3s `ui-bridge-ping`/`ui-bridge-pong` loop bypasses
    // it, and the startup `ui_bridge_invoke_probe` uses its own invoke store —
    // so on a perfectly healthy, idle runner the latch stayed `false` until
    // some EXTERNAL client happened to call a `/ui-bridge/*` route. It measured
    // "has anyone used the UI Bridge yet", not "is the frontend ready", and
    // because it was one-way it was also wrong in the other direction: once
    // true it stayed true through a dead WebView2 host.
    //
    // Worse, the two states were indistinguishable: a frontend that never
    // loaded (2026-08-05 morning: every invoke probe logged "frontend did not
    // reply within probe timeout") and a frontend serving a full 16-element
    // snapshot both read `frontendReady: false`.
    //
    // `classify_frontend_state` already answers this exact question correctly
    // and is IPC-free by construction, so it works precisely when the React
    // tree is down. It was built for the identical defect on the UI-Bridge
    // diagnostics routes ("`ready`/`sdk_connected` used to be `last_pong > 0`,
    // a latch that flips true on the first pong and is never reset"); /health
    // simply never adopted it. The pong is emitted from React
    // (`useUIBridgeEventHandler.ts`), from the SAME `useEffect` that registers
    // the `ui-bridge-request` listener — so a pong already proves both that the
    // app mounted past the loading screen and that the UI-Bridge listener is
    // wired. That is the whole distinction the latch claimed to draw, and the
    // pong establishes it on a self-driving 3s loop instead of waiting on
    // external traffic.
    let main_window = {
        use tauri::Manager;
        state
            .app_handle
            .get_webview_window(qontinui_runner_lib::get_main_window_label())
    };
    let frontend_state = crate::mcp::ui_bridge::request::classify_frontend_state(
        crate::mcp::ui_bridge::request::FrontendStateInputs {
            window_exists: main_window.is_some(),
            window_visible: main_window
                .as_ref()
                .and_then(|w| w.is_visible().ok())
                .unwrap_or(false),
            last_pong,
            last_pong_age_ms: pong_age_ms,
            console_error_count: console_errors,
            process_uptime_ms: uptime_secs.saturating_mul(1000),
            has_ui_error: ui_error_snapshot.is_some(),
        },
    );
    // `Responsive` specifically, NOT `is_ready()`. The two diagnostics routes
    // use `is_ready()` to answer a different question ("has the SDK ever
    // connected and not since crashed"), and it is deliberately lenient:
    // `last_pong > 0 && != TreeCrashed` reports READY for a `WindowMissing`
    // frontend whose WebView is gone (asserted in `request.rs`'s
    // `frontend_state_tests`). For `/health`'s `frontendReady` the useful
    // question is "can the frontend serve a UI Bridge call right now", so every
    // non-`Responsive` branch — missing window, booting, never ponged, crashed
    // tree, gone silent — is not ready.
    //
    // This makes `frontendReady` strictly stronger than the sibling
    // `responsive` field (a 15s pong-age test): it additionally requires that
    // the WebView window exists and that React's error boundary is not holding
    // a throw. Those are precisely the two failures a pong CANNOT see, because
    // the SDK's pong loop survives under the error boundary's fallback.
    let frontend_ready =
        frontend_state == crate::mcp::ui_bridge::request::FrontendState::Responsive;

    // The raw latch is preserved under an honest name rather than deleted: "a
    // full UI-Bridge request/response round-trip has completed at least once
    // since boot" is genuinely useful — it exercises the entire path end to end
    // (request emitted → frontend handler ran → response decoded on the oneshot
    // channel), which a pong does not. It just isn't readiness, and it can only
    // ever become true if something external asks.
    let ui_bridge_ipc_observed = state.app_state.frontend_ready.load(Ordering::Relaxed);

    // Build/deploy drift vs origin/main (plan 2026-07-03-runner-session-
    // tracking-drift-and-guardrails Phase 3 item 3). `mainSha` is
    // origin/main's current SHA (null when unresolvable — production install
    // without a repo, no network); `buildDrift.behind` compares it against
    // the embedded `gitSha` prefix. Populated by the background checker in
    // `crate::build_drift`.
    let (main_sha_json, build_drift_json) = crate::build_drift::health_fields();

    let mut data = serde_json::json!({
        "status": status,
        "ready": last_pong > 0,
        "responsive": responsive,
        "frontendReady": frontend_ready,
        // Why `frontendReady` is what it is. A bare boolean cannot separate
        // "still booting" from "never mounted" from "mounted then crashed" from
        // "went silent"; this names the branch that decided.
        "frontendState": frontend_state.as_str(),
        // "A UI-Bridge request/response round-trip has completed since boot."
        // NOT readiness — externally driven and one-way. See the derivation
        // comment above.
        "uiBridgeIpcObserved": ui_bridge_ipc_observed,
        "lastHeartbeat": last_pong,
        "heartbeatAgeMs": pong_age_ms,
        "uptimeSeconds": uptime_secs,
        "pendingRequests": pending_count,
        "circuitBreaker": format!("{:?}", circuit_breaker_state),
        "consoleErrorCount": console_errors,
        "aiProviderCircuitBreakers": ai_provider_states,
        "embeddingService": embedding_health,
        // iter4 B-5: PG data-layer liveness (bounded `SELECT 1`). `reachable`
        // is null when unprobed/unconfigured, true/false otherwise. Drives the
        // `degraded` downgrade above so `/health` mirrors the data layer.
        "database": {
            "reachable": pg_reachable,
        },
        // PR-credential surface (plan qontinui-pr-credential-provisioning,
        // Phase 0): cached `gh auth status` verdict. `state: "pending"` +
        // null fields until the first detached probe resolves; `hint` is
        // populated only when unauthenticated (incl. the gh-not-installed
        // case). /health never blocks on the probe.
        "prCredential": pr_credential_health(),
        // Session-fabric Phase 0: how each proxied coord call resolved the
        // caller's own coord agent_session_id, since this process booted.
        // coord sees only "header present / absent" — this is the ONLY place
        // the break point in `nonce → workdir → task_run_id → session_id` is
        // knowable, so read it here before concluding the arm is off.
        // Interactive/sniffed sessions resolve either deterministically off
        // the nonce's terminal (`injected_via_terminal`) or via the Phase-3
        // workdir fallback (`injected_via_lifecycle`). The former
        // `no_task_run` bucket is split into the gate that actually rejected:
        // the TERMINAL leg's — `terminal_record_missing` /
        // `terminal_record_unadmitted` / `terminal_anchor_not_uuid` /
        // `ambiguous_terminal`, all of which are FINAL for the call (a known
        // terminal never falls through to the workdir legs, which would answer
        // with a sibling terminal's session) — and the WORKDIR leg's:
        // `no_lifecycle_record` / `record_unregistered` /
        // `record_anchor_not_uuid` / `ambiguous_workdir` /
        // `resolver_state_missing`. `recent_misses` carries a bounded
        // sample (proxy workdir + the record dirs on hand) so a
        // `no_lifecycle_record` verdict can be told apart from a
        // wrong-granularity one.
        "selfId": self_id_health_snapshot(),
        // Semantic recall (plan 2026-07-30, Phase 3): how each proxied
        // `coord_memory_search` ended — did it get a query vector or not.
        // Non-search traffic is neither touched nor counted, so `enriched`
        // climbing is the only positive proof the semantic arm is actually
        // firing; the arm can silently stop (dead embedder, tool rename, a
        // coord parameter rename) and degrade to FTS-only with no other
        // signal at the call site.
        "memorySearchEnrichment": memory_enrich_health_snapshot(),
        // Session-fabric Phase 3: how each `POST /coord/session-handles/register`
        // ended, since this process booted. coord records nothing for a
        // REFUSED bind, so a systematically-denied registry (the wrong
        // `agent_session_id` id space — see `claude_session::session_handle`)
        // is only visible here: `denied` climbing with `minted`/`rebound`
        // flat means every handle is being dropped, which reads identically
        // to "coord has not deployed the route" from the outside.
        "sessionHandles": crate::claude_session::session_handle::health_snapshot(),
        "ai": {
            "configured": ai_configured,
            "running": ai_running,
            "provider": ai_provider_name,
            "model": ai_model,
        },
        // Git SHA of the commit this binary was built from (12-char). Embedded
        // by build.rs via QONTINUI_GIT_SHA. Manual-test sessions can assert
        // the temp runner is actually running the commit under debug.
        "gitSha": env!("QONTINUI_GIT_SHA"),
        // Compile-time build PROVENANCE: which Vite dist this binary
        // embedded. `build.rs` reads `dist/build-id.txt` (written by
        // `vite.config.ts`, format `<git-sha-short>-<unix-ms>`) and re-emits
        // it, so this value names the exact frontend bundle inside the exe.
        // A build made without a prior `pnpm run build` reports the explicit
        // `unstamped-<git-sha>` sentinel instead.
        //
        // NOT a staleness signal. This is a compile-time constant of the
        // running process, as is the `<meta name="build-id">` tag in the
        // embedded HTML — replacing the exe on disk changes neither, so
        // comparing them can only ever detect a BUILD-time inconsistency,
        // never a live binary swap. The runner's refresh banner made exactly
        // that comparison and was a permanent false positive; it was deleted
        // (plan 2026-07-28-runner-build-id-banner-permanent-false-positive).
        // For "is this runner out of date", use `buildDrift` below — it is
        // the only field here that can change while the window is open.
        "buildId": env!("RUNNER_BUILD_ID"),
        // origin/main's current SHA + drift verdict vs the embedded gitSha
        // (see `crate::build_drift`). All-null until the first background
        // check completes, and permanently null on a repo-less install.
        "mainSha": main_sha_json,
        "buildDrift": build_drift_json,
        // Session-tracking health (see `crate::session::tracking_health`):
        // last cross-reference timestamp, live-but-untracked / tracked-but-
        // dead counts + detail, and the untracked-backend-spawn counter.
        "sessionTracking": crate::session::tracking_health::health_json(),
        "storage": {
            "apiPort": api_port,
            "namespaceSuffix": storage_namespace_suffix,
        },
        "derived_status": derived_status,
        "ui_error": ui_error_json.clone(),
        "recent_crash": recent_crash_json.clone(),
    });

    // Debug builds only: routes caught shipping a non-JSON error body, i.e.
    // handlers that bypassed the canonical envelope. `count` is 0 on a healthy
    // build; anything else is a handler bug with the offending routes listed.
    //
    // This is the surface that replaced `envelope_audit`'s panic. A panic was
    // swallowed by `CatchPanicLayer` and left nothing a caller could assert on;
    // this is queryable, cumulative, and survives the request that produced it.
    #[cfg(debug_assertions)]
    {
        data.as_object_mut().unwrap().insert(
            "envelopeViolations".to_string(),
            serde_json::json!({
                "count": crate::mcp::envelope_audit::violation_count(),
                "recent": crate::mcp::envelope_audit::violations(),
            }),
        );
    }

    if let Some((screenshot, width, height)) = diagnostic_screenshot {
        data.as_object_mut().unwrap().insert(
            "diagnosticScreenshot".to_string(),
            serde_json::json!({
                "screenshot": screenshot,
                "width": width,
                "height": height,
                "reason": "Frontend SDK has not connected after 30s of uptime"
            }),
        );
    }

    Json(serde_json::json!({
        "success": true,
        "data": data,
        "uiBridge": {
            "appId": "qontinui-runner",
            "appName": "Qontinui Runner",
            "appType": "desktop",
            "framework": "tauri",
            "capabilities": ["control", "renderLog", "debug"],
        },
        // Top-level mirror of `data.buildId` so fleet consumers (the
        // supervisor's health cache, manual-test sessions asserting "this
        // temp runner is the commit I'm debugging") can read the field from
        // the response root without descending into `data`. Same provenance
        // semantics — and same non-semantics — as `data.buildId` above.
        "buildId": env!("RUNNER_BUILD_ID"),
        // Phase 3J.2 — top-level mirrors of `derived_status` and `ui_error`
        // so supervisor/fleet consumers can read them without descending into
        // the inner `data` block. The inner `data.status` / `data.derived_status`
        // / `data.ui_error` fields remain authoritative for existing callers.
        // `recent_crash` mirrors for the same reason.
        "derived_status": derived_status,
        "ui_error": ui_error_json,
        "recent_crash": recent_crash_json,
        // SDK feature inventory baked at compile time. Surfaced top-level
        // (sibling to `data` and `uiBridge`) to match the supervisor's `/health`
        // shape so test drivers can probe a single endpoint to discover the
        // bundled `@qontinui/ui-bridge` capabilities without parsing the inner
        // `data` envelope. See `crate::sdk_features`.
        "sdkFeatures": crate::sdk_features::SDK_FEATURES,
        "sdkFeaturesDocUrl": crate::sdk_features::SDK_FEATURE_DOC_URL,
        "timestamp": now_ms,
    }))
}

/// `POST /drain` — graceful drain on planned restart (Phase 2 of
/// `2026-06-06-runner-dev-loop-and-restart-resilience`).
///
/// Triggers the [`crate::drain::drain`] sequence: flips the global draining
/// flag (refuses new AI turns), flushes in-flight turns to `output_log`,
/// auto-commits each session's dirty worktree to `refs/wip/<agent_session_id>`,
/// and heartbeats coord claims — all bounded by a hard timeout so a stuck
/// session can never block a deploy. The supervisor calls this before its
/// `taskkill`; the in-process exit seam also calls the drain fn directly.
///
/// Safe to call when idle (no sessions → fast no-op). Idempotent: a second
/// call after a completed drain returns `{already_drained: true}` instantly.
async fn drain_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> Json<crate::drain::DrainSummary> {
    let app_handle = state.app_handle.clone();
    let timeout = crate::drain::configured_timeout();
    // The drain sequence is synchronous (blocking git + bounded polling), so
    // run it on a blocking thread to keep the async executor free.
    let summary = tokio::task::spawn_blocking(move || crate::drain::drain(&app_handle, timeout))
        .await
        .unwrap_or_else(|e| {
            tracing::error!("drain task panicked: {e}");
            crate::drain::DrainSummary::default()
        });
    Json(summary)
}

/// `POST /coord-mcp` — runner-local loopback proxy for coord's `/mcp`
/// streamable-HTTP endpoint, injecting a FRESHLY-READ device JWT per request
/// (plan 2026-06-09-coord-mcp-live-token-proxy).
///
/// Why: coord device JWTs have a ~4h TTL and Claude Code's MCP client reads a
/// session's `.mcp.json` exactly once at connect — a baked static bearer dies
/// with its snapshot and re-stamping the file does nothing. Device-provisioned
/// sessions therefore point at this route (`coord_mcp::write_coord_mcp_proxy_config`)
/// and authenticate with a per-session nonce; the live bearer is read from
/// `AuthManager` on every request.
///
/// Gate (`coord_mcp::proxy_request_gate`, 401 before any network I/O):
/// registered `X-Coord-Mcp-Proxy-Key` nonce AND the live bearer decodes
/// `sub_type == "device"` — the proxy must never attach a non-device token
/// (agent-spawn sessions carry their own narrower JWT and never route here).
///
/// Transport: coord `/mcp` is single-shot `application/json` JSON-RPC per POST
/// (no SSE, no session machinery), so this is a plain passthrough — forward
/// the body + non-hop-by-hop request headers, return coord's status + headers
/// + body bytes verbatim. Generic header passthrough keeps us correct if coord
/// ever adds SSE negotiation headers, but no streaming machinery is built.
/// Which link of the caller-self resolution chain produced the answer
/// (session-fabric Phase 0). Every variant except [`SelfIdOutcome::Injected`]
/// means coord will fall back to its fuzzy `recent_session_for_device` guess —
/// and, crucially, coord CANNOT tell them apart: from its side every one of
/// them looks identical (`coord_self_id_header_total{state="absent"}`). The
/// runner is the only place the break point is knowable, so it is counted here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelfIdOutcome {
    /// The header was sent, resolved via the primary chain
    /// (`nonce → workdir → task_run_id → agent_session_id`).
    Injected,
    /// The header was sent, resolved via the DETERMINISTIC terminal leg
    /// (`nonce → terminal_id → the OPEN lifecycle record for that terminal →
    /// its anchor`). A THIRD success arm, and the one to read as the health
    /// of the identity feature: every hop is 1:1 (the runner mints a nonce
    /// per terminal and hosts one session per terminal), so this arm carries
    /// no tie-break and can never be ambiguous.
    InjectedViaTerminal,
    /// The header was sent, resolved via the interactive/sniffed-session
    /// fallback (`nonce → workdir → lifecycle-store record → its anchor`)
    /// after the terminal leg and the primary chain both missed —
    /// session-identity fabric Phase 3. Reached for bindings that carry no
    /// terminal id (restored/adopted nonces, the mint route, an in-cwd
    /// `.mcp.json`), where the key is a workdir and therefore 1:N. coord sees
    /// an identical header from all three success arms; the /health split
    /// keeps the resolving chain diagnosable per-plane.
    InjectedViaLifecycle,
    /// An agent-spawn session — out of scope by design; those carry their own
    /// scoped identity.
    NonDevicePrincipal,
    /// No `X-Coord-Mcp-Proxy-Key` on the request.
    NoNonce,
    /// Terminal leg, gate 1: the binding NAMES a terminal, but no OPEN
    /// lifecycle record carries that `terminal_id`. Terminal-leg misses are
    /// TERMINAL for the whole chain — they never fall through to the workdir
    /// legs, because a workdir shared with a sibling terminal would answer
    /// with the SIBLING's session id (see [`TerminalLeg`]). A wrong id is
    /// worse than no id.
    TerminalRecordMissing,
    /// Terminal leg, gate 2: the terminal's OPEN record exists but its
    /// `origin` is not one the runner may publish as an identity claim (see
    /// [`lifecycle_record_anchor_is_trusted`] — `reconciled` "may name a
    /// foreign session", a `None` origin predates the field). Withheld, not
    /// fallen through: same reason as [`SelfIdOutcome::TerminalRecordMissing`].
    TerminalRecordUnadmitted,
    /// Terminal leg, gate 3: the terminal's OPEN record is admitted but its
    /// `claude_session_id` does not parse as a uuid, so it cannot name a
    /// `coord.agent_sessions` row (see [`anchor_as_caller_session`]).
    TerminalAnchorNotUuid,
    /// Terminal leg, gate 4: MORE THAN ONE open record names this terminal,
    /// so the terminal key is not 1:1 for this call and the caller is
    /// genuinely unidentifiable on it.
    ///
    /// The durable registry really can hold several `open` rows on one
    /// `terminal_id` — a reused terminal whose prior runs' exit-closes never
    /// fired (`session_lifecycle_store::repair_terminal_id_collisions`, which
    /// records **54** open rows on one terminal). Deliberately refused rather
    /// than ranked: the store's own `open_authority_key` prefers CONFIRMED
    /// rows, and in the dangerous window (a reused terminal between PTY spawn
    /// and the SessionStart hook's confirmation) the STALE row is the
    /// confirmed one — so authority-ranking would actively prefer the
    /// PREVIOUS run's session id. See [`select_terminal_caller`].
    AmbiguousTerminal,
    /// The nonce is not in the live binding map.
    NoWorkdir,
    /// Lifecycle leg, gate 1: no OPEN lifecycle record's `working_dir`
    /// matched the proxy workdir (exact string, then `canonicalize`). Means
    /// the calling terminal has no durable open record at this granularity —
    /// either its record is closed, or it was opened against a different path
    /// (a parent/child dir; matching is deliberately NOT ancestor-aware).
    /// The `/health` `recent_misses` sample carries the proxy workdir and the
    /// open record dirs so those two causes are distinguishable without a
    /// debugger.
    NoLifecycleRecord,
    /// Lifecycle leg, gate 2: ≥1 record matched the workdir, but none carried
    /// a TRUSTED anchor origin — i.e. every match was a phantom spawn-time
    /// shell record (`origin: None`) or a `reconciled` id that "may name a
    /// foreign session". Correctly withheld: a wrong id is worse than no id.
    ///
    /// The admitted set is `authoritative` **and `observed`** (see
    /// [`lifecycle_record_anchor_is_trusted`]) — an earlier build admitted
    /// only `authoritative` and so bucketed 8 of the operator's 34 open
    /// records here, which made this counter read as "untrustworthy anchor"
    /// when the real cause was a too-narrow admission set. It now means what
    /// it says.
    RecordUnregistered,
    /// Lifecycle leg, gate 3: ≥1 admitted record, but no `claude_session_id`
    /// parsed as a UUID, so none can name a `coord.agent_sessions` row (see
    /// [`anchor_as_caller_session`]). Measured 0 of 34 open records on the
    /// live store at 2026-08-04 — a non-zero reading here is new information.
    RecordAnchorNotUuid,
    /// Lifecycle leg, gate 4: more than one ADMITTED UUID candidate shares
    /// this workdir, so the caller is genuinely unidentifiable on the
    /// workdir key (the workspace root hosts 13 open records). Deliberately
    /// resolved to no header rather than to an arbitrary winner — see
    /// [`select_lifecycle_caller`]. This is the honest residual the terminal
    /// leg exists to shrink.
    AmbiguousWorkdir,
    /// The lifecycle store is absent from Tauri state, so the lifecycle leg
    /// could not run at all. Should read **0** in production: the store is
    /// managed at `main.rs:2786`. That is what makes this arm a useful
    /// assertion rather than a guess — before it existed, a missing consumer
    /// was indistinguishable from a genuine record miss.
    ResolverStateMissing,
    /// The primary chain resolved a `task_run_id` but the registrar holds no
    /// coord `agent_session_id` for it. Transient for a freshly-spawned
    /// session. (No lifecycle fallback here — the fallback would key on the
    /// same id and miss identically.)
    ///
    /// Measured 0 across 740+ calls, and it is currently **unreachable via
    /// the fallback leg by construction**: it is only ever produced on the
    /// `Some(task_run_id)` arm, and a runner-managed session with a task run
    /// registers before it can proxy. Kept because that is a property of the
    /// spawn ordering, not a guarantee.
    NoSession,
}

impl SelfIdOutcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Injected => "injected",
            Self::InjectedViaTerminal => "injected_via_terminal",
            Self::InjectedViaLifecycle => "injected_via_lifecycle",
            Self::NonDevicePrincipal => "non_device_principal",
            Self::NoNonce => "no_nonce",
            Self::TerminalRecordMissing => "terminal_record_missing",
            Self::TerminalRecordUnadmitted => "terminal_record_unadmitted",
            Self::TerminalAnchorNotUuid => "terminal_anchor_not_uuid",
            Self::AmbiguousTerminal => "ambiguous_terminal",
            Self::NoWorkdir => "no_workdir",
            Self::NoLifecycleRecord => "no_lifecycle_record",
            Self::RecordUnregistered => "record_unregistered",
            Self::RecordAnchorNotUuid => "record_anchor_not_uuid",
            Self::AmbiguousWorkdir => "ambiguous_workdir",
            Self::ResolverStateMissing => "resolver_state_missing",
            Self::NoSession => "no_session",
        }
    }

    /// This outcome's counter slot, as an EXHAUSTIVE match.
    ///
    /// The counter index must never be derived by searching [`Self::ALL`]:
    /// that was `ALL.iter().position(..).unwrap_or(0)`, and slot 0 is
    /// [`Self::Injected`] — the SUCCESS counter. A variant missing from `ALL`
    /// (or a duplicated `ALL` entry masking one) therefore counted MISSES AS
    /// INJECTIONS, the worst possible failure direction for a diagnostic whose
    /// only job is to say which link broke. Nothing caught it either:
    /// `Cargo.toml` sets `dead_code`/`unused_*` to `"allow"`, and the arity
    /// assertion in the tests passes just fine for 13 variants with one `ALL`
    /// entry written twice.
    ///
    /// An exhaustive `match` makes the COMPILER the guard: adding a variant
    /// without giving it a slot is a build error, not a silent miscount.
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Injected => 0,
            Self::InjectedViaTerminal => 1,
            Self::InjectedViaLifecycle => 2,
            Self::NonDevicePrincipal => 3,
            Self::NoNonce => 4,
            Self::TerminalRecordMissing => 5,
            Self::TerminalRecordUnadmitted => 6,
            Self::TerminalAnchorNotUuid => 7,
            Self::AmbiguousTerminal => 8,
            Self::NoWorkdir => 9,
            Self::NoLifecycleRecord => 10,
            Self::RecordUnregistered => 11,
            Self::RecordAnchorNotUuid => 12,
            Self::AmbiguousWorkdir => 13,
            Self::ResolverStateMissing => 14,
            Self::NoSession => 15,
        }
    }

    /// Every outcome, in counter-slot order — `ALL[i].index() == i`, asserted
    /// in the tests so the two orderings cannot drift.
    pub(crate) const ALL: [Self; 16] = [
        Self::Injected,
        Self::InjectedViaTerminal,
        Self::InjectedViaLifecycle,
        Self::NonDevicePrincipal,
        Self::NoNonce,
        Self::TerminalRecordMissing,
        Self::TerminalRecordUnadmitted,
        Self::TerminalAnchorNotUuid,
        Self::AmbiguousTerminal,
        Self::NoWorkdir,
        Self::NoLifecycleRecord,
        Self::RecordUnregistered,
        Self::RecordAnchorNotUuid,
        Self::AmbiguousWorkdir,
        Self::ResolverStateMissing,
        Self::NoSession,
    ];
}

/// Per-outcome counters, indexed by [`SelfIdOutcome::index`] (which is the
/// declaration order of [`SelfIdOutcome::ALL`]).
fn self_id_counters() -> &'static [std::sync::atomic::AtomicU64; 16] {
    static COUNTERS: std::sync::OnceLock<[std::sync::atomic::AtomicU64; 16]> =
        std::sync::OnceLock::new();
    COUNTERS.get_or_init(Default::default)
}

fn record_self_id_outcome(outcome: SelfIdOutcome) {
    self_id_counters()[outcome.index()].fetch_add(1, Ordering::Relaxed);
}

/// How many recent lifecycle-leg misses `GET /health` carries. BOUNDED on
/// purpose: this leg missed 678 of 678 times before the terminal leg landed,
/// so an unbounded diagnostic would be a slow leak keyed on failure volume.
const SELF_ID_MISS_SAMPLE_CAP: usize = 8;
/// How many record dirs one sample entry carries, per list.
const SELF_ID_MISS_DIR_CAP: usize = 8;

/// One recorded lifecycle-leg miss, for the `/health` diagnostic sample.
///
/// A bare counter cannot separate "the record is at a different granularity"
/// from "the record is closed" — both read as [`SelfIdOutcome::NoLifecycleRecord`].
/// Carrying the proxy workdir beside the record dirs that were actually on
/// hand makes that call from the outside.
#[derive(Debug, Clone)]
struct SelfIdMissSample {
    /// Which gate rejected — a [`SelfIdOutcome::label`].
    gate: &'static str,
    /// The proxy-provisioned workdir the nonce resolved to.
    workdir: String,
    /// Dirs of the OPEN records that MATCHED that workdir. Empty exactly when
    /// the gate is `no_lifecycle_record`.
    candidate_dirs: Vec<String>,
    /// Bounded distinct sample of every OPEN record's dir, matched or not.
    open_dirs: Vec<String>,
}

/// The miss ring itself: newest at the back, capped at
/// [`SELF_ID_MISS_SAMPLE_CAP`].
type SelfIdMissRing = std::sync::Mutex<std::collections::VecDeque<SelfIdMissSample>>;

fn self_id_miss_samples() -> &'static SelfIdMissRing {
    static SAMPLES: std::sync::OnceLock<SelfIdMissRing> = std::sync::OnceLock::new();
    SAMPLES.get_or_init(Default::default)
}

/// Push one miss onto the ring, evicting the oldest past the cap. Lock-cheap:
/// the strings are built by the caller, the lock covers a `pop_front` +
/// `push_back` and nothing else, and a poisoned lock silently drops the
/// sample (a diagnostic must never fail a proxy request).
fn record_self_id_miss_sample(
    gate: SelfIdOutcome,
    workdir: &str,
    candidate_dirs: Vec<String>,
    open_dirs: Vec<String>,
) {
    let sample = SelfIdMissSample {
        gate: gate.label(),
        workdir: workdir.to_string(),
        candidate_dirs,
        open_dirs,
    };
    let Ok(mut q) = self_id_miss_samples().lock() else {
        return;
    };
    while q.len() >= SELF_ID_MISS_SAMPLE_CAP {
        q.pop_front();
    }
    q.push_back(sample);
}

/// The two dir lists for a miss sample: the OPEN records that matched
/// `workdir`, and a bounded distinct sample of every open record's dir.
fn self_id_miss_sample_dirs(
    records: &[crate::session::session_lifecycle_store::TerminalSessionRecord],
    workdir: &str,
    target_canon: Option<&std::path::Path>,
) -> (Vec<String>, Vec<String>) {
    let mut candidates: Vec<String> = Vec::new();
    let mut open: Vec<String> = Vec::new();
    for rec in records {
        let Some(dir) = rec.working_dir.as_deref() else {
            continue;
        };
        if open.len() < SELF_ID_MISS_DIR_CAP && !open.iter().any(|d| d == dir) {
            open.push(dir.to_string());
        }
        if candidates.len() < SELF_ID_MISS_DIR_CAP
            && lifecycle_workdir_matches(dir, workdir, target_canon)
            && !candidates.iter().any(|d| d == dir)
        {
            candidates.push(dir.to_string());
        }
    }
    (candidates, open)
}

/// The recorded miss ring, oldest first, for `GET /health`.
fn self_id_miss_sample_json() -> serde_json::Value {
    let samples = match self_id_miss_samples().lock() {
        Ok(q) => q.iter().cloned().collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    serde_json::Value::Array(
        samples
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "gate": s.gate,
                    "workdir": s.workdir,
                    "candidate_dirs": s.candidate_dirs,
                    "open_dirs": s.open_dirs,
                })
            })
            .collect(),
    )
}

/// Snapshot of the self-id chain counters for `GET /health`, plus the bounded
/// `recent_misses` diagnostic sample (last [`SELF_ID_MISS_SAMPLE_CAP`]
/// lifecycle-leg misses).
pub(crate) fn self_id_health_snapshot() -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for outcome in SelfIdOutcome::ALL {
        // Keyed on `index()`, not the iteration position — the same
        // compiler-checked mapping `record_self_id_outcome` writes through, so
        // a series can never render another variant's count.
        obj.insert(
            outcome.label().to_string(),
            serde_json::json!(self_id_counters()[outcome.index()].load(Ordering::Relaxed)),
        );
    }
    obj.insert("recent_misses".to_string(), self_id_miss_sample_json());
    serde_json::Value::Object(obj)
}

/// Resolve the calling terminal's coord `agent_session_id` for the caller-self
/// header (session-fabric Phase 0). Bridges the only thing the proxy knows —
/// the per-session nonce — back to a coord session id, over three legs tried
/// in order of how deterministic they are:
///
/// 1. **Terminal leg (exact).** `nonce → terminal_id → the OPEN lifecycle
///    record for that terminal → its anchor`. The runner mints a nonce per
///    terminal and a PTY hosts one LIVE session, so the hop is 1:1 by intent:
///    no recency tie-break, no guess. Only bindings minted with a terminal
///    reach it — restored/adopted nonces, the mint route and an in-cwd
///    `.mcp.json` carry none, and those (and ONLY those) fall through to the
///    workdir legs. A binding whose terminal IS known but does not resolve
///    STOPS here with a typed `terminal_*` outcome; it must never fall
///    through, because the workdir legs would answer with a same-cwd sibling
///    terminal's session id. See [`TerminalLeg`].
/// 2. **Primary chain.** `nonce → workdir → task_run_id → coord
///    agent_session_id`, for runner-managed sessions with a worktree.
/// 3. **Lifecycle fallback (fabric Phase 3).** `nonce → workdir → open
///    lifecycle record → coord agent_session_id`, for the interactive plane.
///    Keyed on a workdir, which is 1:N — so it resolves only when the workdir
///    names exactly one admitted session, and otherwise reports
///    [`SelfIdOutcome::AmbiguousWorkdir`] rather than picking one.
///
/// Every link is best-effort; a break anywhere yields `None` (the caller then
/// omits the header and coord keeps its fuzzy fallback), and the break point
/// is reported as a [`SelfIdOutcome`] so the chain is diagnosable from
/// `GET /health` instead of by reading the runner's process memory.
///
/// Not memoized. The one expensive hop — `task_run_id_for_workdir`'s
/// per-session `canonicalize` — no longer holds the session-manager lock across
/// its syscalls (it snapshots under the lock and canonicalizes lock-free), so
/// running the chain per call costs a handful of lock-free stats before a
/// network round-trip to coord — cheap enough to not warrant a cache. A memo
/// keyed on the nonce would ALSO be unsound: a persistent DEVICE nonce is built
/// to outlive its session, so a cached `nonce → session_id` would misattribute
/// a *new* session that reused the same on-disk nonce + workdir. Recomputing
/// every call reads the live session set and is always correct.
fn resolve_caller_session_id(
    state: &Arc<ApiState>,
    nonce: Option<&str>,
) -> (Option<uuid::Uuid>, SelfIdOutcome) {
    let Some(nonce) = nonce else {
        return (None, SelfIdOutcome::NoNonce);
    };
    // Leg 1 — the deterministic terminal key (Phase A's `terminal_id_for_nonce`).
    // Tried BEFORE any workdir resolution because it is exact where the
    // workdir legs are a guess. THREE-WAY on purpose: "this binding has no
    // terminal" and "this binding's terminal did not resolve" are opposite
    // situations, and collapsing them into one `None` is what let a
    // terminal-known miss inherit a SIBLING terminal's id off the shared
    // workdir — see [`TerminalLeg`].
    match resolve_caller_via_terminal(state, nonce) {
        TerminalLeg::Resolved(sid) => return (Some(sid), SelfIdOutcome::InjectedViaTerminal),
        TerminalLeg::Miss(outcome) => return (None, outcome),
        TerminalLeg::NoTerminal => {}
    }
    let Some(workdir) = crate::coord_mcp::workdir_for_nonce(nonce) else {
        return (None, SelfIdOutcome::NoWorkdir);
    };
    let task_run_id = state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .and_then(|sm| sm.task_run_id_for_workdir(&workdir));
    match task_run_id {
        // Primary chain hit.
        Some(task_run_id) => {
            let Some(registrar) = state
                .app_handle
                .try_state::<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
            else {
                return (None, SelfIdOutcome::NoSession);
            };
            // The registrar lookup is the REGISTERED-ness FILTER, not the
            // value: it proves this session actually registered with coord
            // (and so has a `coord.agent_sessions` row), but the id it holds
            // is the per-boot `coord.sessions.id`, which is the wrong id
            // space for this header — see `anchor_as_caller_session`.
            match registrar.session_id_for(&task_run_id) {
                Some(_) => match anchor_as_caller_session(&task_run_id) {
                    Some(sid) => (Some(sid), SelfIdOutcome::Injected),
                    None => (None, SelfIdOutcome::NoSession),
                },
                None => (None, SelfIdOutcome::NoSession),
            }
        }
        // Session-identity fabric Phase 3 — interactive fallback. The workdir
        // named no worktree-carrying SessionManager session (the interactive
        // plane never has one), so resolve through the durable lifecycle
        // store instead: workdir → the single admitted open record → its own
        // anchor. Every miss arrives already typed as the gate that rejected.
        None => match resolve_caller_via_lifecycle(state, &workdir) {
            Ok(sid) => (Some(sid), SelfIdOutcome::InjectedViaLifecycle),
            Err(outcome) => (None, outcome),
        },
    }
}

/// The three genuinely different things leg 1 can say. Collapsing them into
/// an `Option` is the FALLTHROUGH DEFECT: a `None` meant both "no terminal on
/// this binding, use the workdir chain" and "this terminal's record was
/// rejected", and the second must NOT reach the workdir chain.
///
/// The counterexample is ordinary on this box. Terminal `T1`'s nonce carries
/// `terminal_id = T1`; `T1`'s record is absent or unadmitted. Terminal `T2`
/// shares the cwd and is that workdir's SINGLE admitted candidate. Leg 3 then
/// resolves — with full confidence — to `T2`'s anchor, and every one of
/// `T1`'s coord calls is labelled as `T2`. The earlier claim that a rejected
/// record "falls through to the workdir chain, which applies the same guard
/// and reports `RecordUnregistered`" only holds when the workdir hosts no
/// OTHER admitted record; when it does, the guard is satisfied by the wrong
/// session. The runner had the information to know better, and now uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalLeg {
    /// The binding carries NO terminal (restore, adopt, the mint route, an
    /// in-cwd `.mcp.json`) — the majority of persisted nonces today. Fall
    /// through to the workdir chain, unchanged.
    NoTerminal,
    /// The terminal named exactly one admitted open record with a uuid anchor.
    Resolved(uuid::Uuid),
    /// The terminal IS known but did not resolve. STOP — never fall through:
    /// the workdir chain would answer with a DIFFERENT terminal's session.
    Miss(SelfIdOutcome),
}

/// Leg 1: resolve the caller from the nonce's TERMINAL — the finest key the
/// runner owns, and the only one that is 1:1 with a live session.
///
/// `terminal_id_for_nonce` (coord_mcp) returns the terminal a live binding was
/// minted for, and the OPEN lifecycle record for that terminal carries the
/// coord `agent_sessions` anchor (see [`anchor_as_caller_session`]). A PTY
/// hosts at most one LIVE session, so the *intended* mapping is exact — but
/// the durable registry can still hold several stale `open` rows on one
/// terminal, which is why [`select_terminal_caller`] refuses instead of
/// picking. Deliberately no recency/authority tie-break; see there.
///
/// Applies the same anchor-trust guard as the lifecycle leg
/// ([`lifecycle_record_anchor_is_trusted`]), and the reason is worth stating
/// because the obvious argument against it is wrong. That argument: the guard
/// exists to stop a workdir match resolving to a same-cwd *sibling's* id, and
/// here the record is THE record for the terminal this nonce was minted for —
/// no sibling to confuse it with. True, and irrelevant: `origin` does not
/// describe how many records matched, it describes whether the ANCHOR ITSELF
/// is trustworthy. A `"reconciled"` record was recovered by a backstop
/// (freshest-transcript mtime) and its own doc says it "may name a foreign
/// session". Being the unique record for a terminal does not make a guessed
/// anchor correct — uniqueness is not correctness. Shipping it would hand
/// coord a confidently wrong identity, the one failure this whole chain is
/// built to avoid (a wrong id is worse than no id, because coord's fuzzy
/// fallback is at least honestly fuzzy).
///
/// Every rejection is a [`TerminalLeg::Miss`], NOT a fallthrough — see
/// [`TerminalLeg`] for why. That includes a missing lifecycle store: the
/// terminal is known and we cannot check it, so refusing is the honest answer
/// (counted as [`SelfIdOutcome::ResolverStateMissing`], which should read 0 —
/// the store is managed at `main.rs:2786`).
fn resolve_caller_via_terminal(state: &Arc<ApiState>, nonce: &str) -> TerminalLeg {
    let terminal_id = crate::coord_mcp::terminal_id_for_nonce(nonce);
    if terminal_id.is_none() {
        // Same verdict [`terminal_leg`] would give, taken BEFORE the store
        // fetch so the no-terminal majority never snapshots `open_records()`
        // for nothing — and so the workdir leg's own snapshot is never a
        // second clone of the same set.
        return TerminalLeg::NoTerminal;
    }
    let Some(store) = state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    else {
        return TerminalLeg::Miss(SelfIdOutcome::ResolverStateMissing);
    };
    let records = store.open_records(); // snapshot under the store lock
    terminal_leg(&records, terminal_id.as_deref())
}

/// The pure three-way leg-1 decision (unit-testable without a Tauri app), so
/// the "known terminal ⇒ never fall through" rule is asserted against the code
/// production actually runs rather than re-derived in a test.
fn terminal_leg(
    records: &[crate::session::session_lifecycle_store::TerminalSessionRecord],
    terminal_id: Option<&str>,
) -> TerminalLeg {
    let Some(terminal_id) = terminal_id else {
        return TerminalLeg::NoTerminal;
    };
    match select_terminal_caller(records, terminal_id) {
        Ok(sid) => TerminalLeg::Resolved(sid),
        Err(outcome) => TerminalLeg::Miss(outcome),
    }
}

/// Pure terminal-keyed selection (unit-testable without a Tauri app): the
/// anchor of the single OPEN record hosted by `terminal_id`.
///
/// ## Why this counts instead of using `.find()`
///
/// This used to be `records.iter().find(|rec| rec.terminal_id == terminal_id)`,
/// on the belief that the store holds at most one OPEN record per terminal.
/// It does not. `session_lifecycle_store` documents that the durable registry
/// "can hold several `open` rows on one `terminal_id`" (a reused terminal
/// whose prior runs' exit-closes never fired) and ships
/// `repair_terminal_id_collisions` to collapse them — whose doc records **54**
/// open rows on a single terminal. `open_records()` iterates a `HashMap`'s
/// `values()`, so `.find()` was HASH-ORDER NONDETERMINISTIC and could return a
/// PREVIOUS run's `claude_session_id`.
///
/// ## Why it refuses instead of ranking
///
/// The store's `open_authority_key` — `(confirmed_at.is_some(),
/// last_seen_at, opened_at)` — is the right tie-break for RESTORE, and the
/// wrong one here. In the dangerous window (a reused terminal between PTY
/// spawn and the new session's SessionStart-hook confirmation) the STALE row
/// is the confirmed one and the fresh row is not, so authority-ranking
/// actively PREFERS the wrong session. Exactly the window where a caller is
/// most likely to be misattributed. So >1 open record naming the terminal is
/// [`SelfIdOutcome::AmbiguousTerminal`]: counted, named, and headerless.
///
/// Two records naming the SAME session are one candidate, not an ambiguity —
/// there is only one identity to publish (mirrors
/// [`select_lifecycle_caller`]).
fn select_terminal_caller(
    records: &[crate::session::session_lifecycle_store::TerminalSessionRecord],
    terminal_id: &str,
) -> Result<uuid::Uuid, SelfIdOutcome> {
    let mut matched = 0usize;
    let mut admitted = 0usize;
    let mut candidates: Vec<uuid::Uuid> = Vec::new();
    for rec in records {
        if rec.terminal_id != terminal_id {
            continue;
        }
        matched += 1;
        if !lifecycle_record_anchor_is_trusted(rec) {
            continue;
        }
        admitted += 1;
        let Some(sid) = anchor_as_caller_session(&rec.claude_session_id) else {
            continue;
        };
        if !candidates.contains(&sid) {
            candidates.push(sid);
        }
    }
    if matched == 0 {
        return Err(SelfIdOutcome::TerminalRecordMissing);
    }
    if admitted == 0 {
        return Err(SelfIdOutcome::TerminalRecordUnadmitted);
    }
    match candidates.len() {
        0 => Err(SelfIdOutcome::TerminalAnchorNotUuid),
        1 => Ok(candidates[0]),
        _ => Err(SelfIdOutcome::AmbiguousTerminal),
    }
}

/// Phase-3 fallback leg: resolve the caller's coord `agent_session_id` from
/// the lifecycle store when the terminal leg and the SessionManager worktree
/// chain both miss.
///
/// Lock discipline mirrors the #841 pattern: `open_records()` clones the
/// record set under the store's lock (a snapshot), and every path comparison
/// runs lock-free afterwards. Cost per call: one `canonicalize` of the target
/// workdir and a string compare per open record (with a canonicalize fallback
/// only for records whose string form differs) — bounded by the operator's
/// open-terminal count, and only ever run after two legs already missed.
/// Never fails the request; the miss is returned as the [`SelfIdOutcome`] to
/// count, and the header is simply absent.
///
/// The bounded `/health` miss sample is recorded HERE rather than inside the
/// pure selector, so the selector stays a pure function and the sample costs
/// nothing on the success path.
fn resolve_caller_via_lifecycle(
    state: &Arc<ApiState>,
    workdir: &str,
) -> Result<uuid::Uuid, SelfIdOutcome> {
    let Some(store) = state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    else {
        return Err(SelfIdOutcome::ResolverStateMissing);
    };
    let records = store.open_records(); // snapshot under the store lock
    let target_canon = std::fs::canonicalize(workdir).ok();
    select_lifecycle_caller(&records, workdir, target_canon.as_deref()).map_err(|miss| {
        let outcome = miss.outcome();
        let (candidates, open) =
            self_id_miss_sample_dirs(&records, workdir, target_canon.as_deref());
        record_self_id_miss_sample(outcome, workdir, candidates, open);
        outcome
    })
}

/// The caller-session id to put on `X-Coord-Caller-Session` for a session
/// anchored on `claude_session_id`.
///
/// coord validates this header with `agent_sessions::session_on_device`
/// (fail-closed) before trusting it, so it MUST be a `coord.agent_sessions.id`
/// bound to this device — which is the durable anchor, NOT the per-boot
/// `coord.sessions.id` that `AiCoordRegistrar` mints and holds in its R4
/// index. coord's `create_session` upserts
/// `coord.agent_sessions(id = claude_code_session_id, device_id)` from the
/// anchor the runner publishes, so the anchor is the id that exists in that
/// table.
///
/// Shipping the registrar's value instead is why the Phase-0 chain could not
/// work even fully armed: the header arrived, failed `session_on_device`, and
/// coord silently fell back to its fuzzy `recent_session_for_device` guess —
/// visible only as `coord_self_id_resolution_total{outcome="injected_invalid"}`.
/// That is a DIFFERENT failure from the 2026-07-22 arm-collapse (the header
/// never being SENT at all).
fn anchor_as_caller_session(claude_session_id: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(claude_session_id.trim()).ok()
}

/// Why the pure lifecycle selection produced no caller. Each variant names the
/// FIRST gate a workdir's records failed, so the `/health` counters partition
/// the misses instead of collapsing them into one bucket (they were a single
/// `no_task_run` before, which is why 678 identical misses were undiagnosable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleMiss {
    /// No OPEN record's `working_dir` matched.
    NoRecord,
    /// Records matched, none carried a trusted anchor origin
    /// ([`lifecycle_record_anchor_is_trusted`]).
    Unregistered,
    /// Admitted records existed, none had a uuid anchor.
    AnchorNotUuid,
    /// More than one admitted uuid candidate on this workdir.
    Ambiguous,
}

impl LifecycleMiss {
    const fn outcome(self) -> SelfIdOutcome {
        match self {
            Self::NoRecord => SelfIdOutcome::NoLifecycleRecord,
            Self::Unregistered => SelfIdOutcome::RecordUnregistered,
            Self::AnchorNotUuid => SelfIdOutcome::RecordAnchorNotUuid,
            Self::Ambiguous => SelfIdOutcome::AmbiguousWorkdir,
        }
    }
}

/// Whether a lifecycle record's ANCHOR is one the runner KNOWS, and may
/// therefore publish as this device's caller identity.
///
/// All four live `origin` values, and the verdict on each
/// (`session/session_lifecycle_store.rs`):
///
/// | `origin` | admitted | why |
/// |---|---|---|
/// | `Some("authoritative")` | **yes** | the runner pre-pinned `--session-id`, lifted it from a typed `--resume`/`--session-id`, or a provider hook POSTed it — the id is KNOWN, not derived. |
/// | `Some("observed")` | **yes** | a claude-process-start-anchored, uniquely-correlated transcript bind: "the transcript proves the session exists". Derived, but derived from proof, not from a guess. |
/// | `Some("reconciled")` | no | recovered by a freshest-transcript-mtime backstop and documented as possibly naming a **foreign** session. A guessed anchor stays out however unique its record is. |
/// | `None` | no | predates the field — the phantom spawn-time shell record this guard exists to exclude. |
///
/// The `observed` tier is not a new judgement here: `restore_record_emitter.rs:264`
/// already gates the FULL restore tier (which re-resumes a session by id) on
/// `Some(ORIGIN_AUTHORITATIVE) | Some(ORIGIN_OBSERVED)`, i.e. the runner already
/// trusts an observed anchor enough to hand it back to `claude --resume`. Naming
/// the same session to coord is strictly the weaker claim, and coord re-validates
/// it fail-closed with `session_on_device` on top. Admitting only `authoritative`
/// silently withheld **8 of the operator's 34 open records** at 2026-08-04 and
/// mis-bucketed them into `record_unregistered`, whose `/health` doc then
/// misdescribed the residual.
///
/// Note the deliberate asymmetry with `restore_record_emitter`: it additionally
/// requires `confirmed_at.is_some()`, because a wrong RESTORE re-opens a foreign
/// conversation in the operator's face. This gate needs no such confirmation —
/// `origin` alone answers "is the anchor derived from proof", and an unconfirmed
/// row that is the workdir's/terminal's sole candidate is still the right id.
fn lifecycle_record_anchor_is_trusted(
    rec: &crate::session::session_lifecycle_store::TerminalSessionRecord,
) -> bool {
    use crate::session::session_lifecycle_store::{ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED};
    matches!(
        rec.origin.as_deref(),
        Some(ORIGIN_AUTHORITATIVE) | Some(ORIGIN_OBSERVED)
    )
}

/// Pure candidate selection for the lifecycle fallback (unit-testable without
/// a Tauri app). Among OPEN lifecycle records whose `working_dir` matches the
/// proxy-provisioned `workdir` and which pass
/// [`lifecycle_record_anchor_is_trusted`], resolves the caller **only when
/// exactly one** distinct uuid anchor remains.
///
/// ## Ambiguity: no winner, ever
///
/// This used to pick the greatest `last_seen_at`. That pick was a
/// deterministic tie-break, NOT a caller-activity signal — `last_seen_at` is
/// refreshed by the liveness poll for every live session, so its ordering
/// among concurrently-live sessions sharing one workdir is effectively
/// arbitrary (poll-order) and the winner may not be the session making the
/// call. On this device the workspace root alone hosts 13 open records, so
/// that was a ~1-in-13 chance of a CONFIDENTLY WRONG identity. A wrong id is
/// worse than no id: coord's fuzzy device-wide fallback is at least honestly
/// fuzzy, while a wrong header is believed. So a multi-candidate workdir now
/// resolves to [`LifecycleMiss::Ambiguous`] — counted, named, and headerless.
/// The single-candidate workdir, the common case, still resolves exactly. The
/// terminal leg ([`select_terminal_caller`]) applies the SAME refusal on its
/// own key — the terminal→session mapping is 1:1 for LIVE sessions but the
/// durable registry can retain stale `open` rows on a reused terminal, so it
/// too counts rather than picks.
///
/// ## Why there is no registrar consultation here
///
/// The admission test used to be `registrar.session_id_for(csid).is_some()`.
/// That reads as "is this session registered", but `AiCoordRegistrar`'s
/// reverse index is an in-process `HashMap` built EMPTY at every runner boot
/// and never rehydrated, whose only interactive-plane producer is the
/// `claude --resume <uuid>` sniffer — so it actually meant "**this runner
/// process** registered it **since boot**". Measured effect: it dropped every
/// candidate, 678 of 678, and `injected_via_lifecycle` was exactly 0 rather
/// than merely low.
///
/// Deleting it does not weaken any check, because it was never the check that
/// mattered: coord validates `X-Coord-Caller-Session` with
/// `agent_sessions::session_on_device` **fail-closed** before trusting it, and
/// counts a rejection as `coord_self_id_resolution_total{outcome="injected_invalid"}`
/// (see [`anchor_as_caller_session`]). An anchor with no `coord.agent_sessions`
/// row is therefore already rejected server-side and coord falls back exactly
/// as it does today — the runner-side filter prevented no misattribution. The
/// durable anchor-origin guard that replaces it
/// ([`lifecycle_record_anchor_is_trusted`]) keeps the
/// anti-phantom property AND survives a restart, which an in-process map
/// cannot.
fn select_lifecycle_caller(
    records: &[crate::session::session_lifecycle_store::TerminalSessionRecord],
    workdir: &str,
    target_canon: Option<&std::path::Path>,
) -> Result<uuid::Uuid, LifecycleMiss> {
    let mut matched = 0usize;
    let mut admitted = 0usize;
    let mut candidates: Vec<uuid::Uuid> = Vec::new();
    for rec in records {
        let Some(dir) = rec.working_dir.as_deref() else {
            continue;
        };
        if !lifecycle_workdir_matches(dir, workdir, target_canon) {
            continue;
        }
        matched += 1;
        if !lifecycle_record_anchor_is_trusted(rec) {
            continue;
        }
        admitted += 1;
        // The ANCHOR is the caller id (see `anchor_as_caller_session`); a
        // record whose anchor is not a uuid cannot name a
        // `coord.agent_sessions` row, so it is not a candidate.
        let Some(sid) = anchor_as_caller_session(&rec.claude_session_id) else {
            continue;
        };
        // Distinct ids only: two records naming the SAME session are one
        // candidate, not an ambiguity.
        if !candidates.contains(&sid) {
            candidates.push(sid);
        }
    }
    if matched == 0 {
        return Err(LifecycleMiss::NoRecord);
    }
    if admitted == 0 {
        return Err(LifecycleMiss::Unregistered);
    }
    match candidates.len() {
        0 => Err(LifecycleMiss::AnchorNotUuid),
        1 => Ok(candidates[0]),
        _ => Err(LifecycleMiss::Ambiguous),
    }
}

/// Whether a lifecycle record's `working_dir` names the proxy workdir. Exact
/// string equality first (the common case: both strings originate from the
/// same terminal-create working_dir); a `canonicalize` of the record path
/// only as a fallback, mirroring `worktree_path_matches` in the primary
/// chain.
fn lifecycle_workdir_matches(
    record_dir: &str,
    workdir: &str,
    target_canon: Option<&std::path::Path>,
) -> bool {
    if record_dir == workdir {
        return true;
    }
    match target_canon {
        Some(tc) => std::fs::canonicalize(record_dir).ok().as_deref() == Some(tc),
        None => false,
    }
}

/// JSON-RPC methods the loopback `/coord-mcp` proxy forwards (credential-
/// hygiene Task 4). Everything else is rejected BEFORE any bearer is read or
/// any byte reaches coord. The MCP handshake + tool surface is exactly:
/// `initialize` / `notifications/*` housekeeping / `ping` / `tools/list` /
/// `tools/call` — and `tools/call` is further gated per tool name by
/// [`coord_mcp_tool_is_allowed`]. Sorted for `binary_search`.
const COORD_MCP_ALLOWED_METHODS: &[&str] = &[
    "initialize",
    "notifications/cancelled",
    "notifications/initialized",
    "notifications/progress",
    "ping",
    "tools/call",
    "tools/list",
];

/// Coord MCP tools a proxied session may invoke (credential-hygiene Task 4).
///
/// Same posture as the sibling [`ClaimsReadTarget`] / [`CoordWriteTarget`]
/// enums: the per-session nonce authenticates a *session*, not an operator,
/// so its authority is an ENUMERATED set — the legitimate session-coordination
/// surface (introspection reads + the coordination writes: declare-intent,
/// work-unit upsert/transition, gate register/attest, claims, expectations,
/// findings/messaging/memory, orient/status/conflict-check/blockers). A leaked
/// nonce must not reach anything beyond that with the runner's device
/// identity. Coord is being hardened server-side in parallel; this list is
/// defense-in-depth, not the sole authority.
///
/// DELIBERATELY EXCLUDED (add only with a security rationale): the onboarding
/// / enrollment family (`coord_onboard_*`, `coord_onboarding_doctor`),
/// privilege escalation (`coord_attest_escalate_override`), merge authority
/// (`coord_request_merge`, `coord_cancel_merge`, `coord_pr_merge_verdict`,
/// `coord_pr_merge_profile`), code publication (`coord_create_pr`,
/// `coord_push_to_branch` — sessions open PRs via the dedicated
/// `/vcs/pull-requests` loopback route, not this proxy), reservations
/// (`coord_migration_reserve`, `coord_reserve_resource`), and policy/state
/// mutation (`coord_request_policy`, `coord_flag_state`, `coord_flag_states`).
///
/// LOAD-BEARING, do not strip as "reads nobody uses": `coord_list_prompt_documents`
/// and `coord_get_prompt_document` are how the fleet reads served POLICY, and the
/// `/policy` skill's transport cascade calls them through THIS proxy (a
/// `tools/call` POST to `/coord-mcp` with the `.mcp.json` nonce). Every session is
/// required to read policy before substantive work, so omitting them does not fail
/// loudly — `/policy` silently falls through to its last-resort `qontinui-dev-notes`
/// mirrors and the fleet starts booting off stale policy with nothing surfaced.
/// `coord_gate_list` / `coord_withdraw_gate` are the same story for `/gate-sweep`
/// and `/gate`, which already document driving them over this proxy.
///
/// `coord_write_prompt_document` is IN, and the reason it used to be out no longer
/// holds. The old rationale — "policy authorship is tenant-gated and belongs to an
/// operator, not a device nonce" — described a coord that no longer exists: coord
/// deliberately grants this tool to device/agent principals
/// (`coord::mcp::agent_tool_access`) precisely so a session can CLOSE a POLICY_GAP
/// it found instead of recording one that waits on the operator, which was the
/// bottleneck that made gaps accumulate faster than they could be hand-published.
/// Withholding it HERE did not re-impose an operator gate; it silently removed a
/// capability coord had already decided to grant, and the denial surfaced as a bare
/// `-32601` that reads like "no such tool" rather than "your proxy withholds it".
///
/// Nor is the grant unguarded — coord enforces non-loosening server-side, so this
/// proxy is not the thing standing between a device nonce and a weakened policy:
/// `append` preserves the existing body verbatim, `create` only authors names that
/// do not yet exist, `edit_clause` lands only a provable tightening or no-op and
/// otherwise becomes a pending operator proposal, and the meta-policies
/// (`session-protocol`, `security-and-autonomy`, `escalation-bar`) are refused
/// outright in either direction. Tenant comes from the verified `CallerIdentity`,
/// never an argument; every landed write is versioned, attributed and revertible.
///
/// Keep it in. If policy authorship should ever be operator-only again, that is a
/// decision to make in coord's grant — one place, enforced for every transport —
/// not by re-diverging this list from it.
///
/// `coord_agent_registry_effective` is IN, and it is the same divergence one step
/// further on. It is a read-only, tenant-scoped fold of (registry row × user
/// preference) — it records nothing and changes no preference — and it is the ONLY
/// way to tell a genuinely recorded `degrade` from a `degrade` assumed because the
/// read failed. Policy binds a gate to exactly that distinction:
/// `pre-pr-review` makes the user's recorded disposition govern how the review gate
/// is satisfied, and `registry-unreadable-falls-back-to-degrade` says an unreadable
/// registry is UNKNOWN and must fall back to the degraded inline self-review.
///
/// Withholding it here did not add a gate — it made one permanently unreachable.
/// Every device/agent session was forced onto the degraded path and could never
/// observe an `enabled` disposition, so a policy-required `code-reviewer` pass was
/// silently downgraded to an author reviewing their own diff, for every such session,
/// regardless of what the user had actually recorded. That is the precise failure
/// `harness-injected-agent-prohibitions` exists to prevent, arriving by a different
/// road. Same shape as `coord_write_prompt_document` above: the denial surfaced as a
/// bare `-32601` that reads like "no such tool" rather than "your proxy withholds it".
///
/// And as with that tool, coord had ALREADY decided the other way — this list was the
/// only thing still dissenting. `coord::mcp::agent_tool_access` grants the tool to
/// device/agent principals and registers its HTTP twin
/// `GET /coord/agent-registry/effective` as `DoorAdmits::DeviceAgent` over the shared
/// `agent_registry::resolve_effective`, with the tenant floor enforced inside that
/// shared function rather than at either door. Coord's own note on the entry reaches
/// this same conclusion in its own words: "Masking it was not a boundary, it was a
/// self-inflicted blind spot … The session bound BY the setting was the one principal
/// that could not read it." A device JWT could always get this answer over HTTP; the
/// proxy simply refused the MCP door to the same data.
///
/// Reading the registry grants no authority to SPAWN. `agent-spawn-authorization`
/// still governs that, and a genuine user deselection remains a never — this only
/// lets a session find out which it is.
///
/// MUST stay sorted — membership is a `binary_search`.
const COORD_MCP_ALLOWED_TOOLS: &[&str] = &[
    "coord_ack_message",
    "coord_agent_registry_effective",
    "coord_am_i_clear",
    "coord_ask_question",
    "coord_attest_gate",
    "coord_blockers",
    "coord_build_info",
    "coord_can",
    "coord_change_conflict",
    "coord_check_gate_predicate",
    "coord_check_install_safety",
    "coord_check_publish_safety",
    "coord_claim_acquire",
    "coord_claim_check",
    "coord_claim_heartbeat",
    "coord_claim_release",
    "coord_conflict_check",
    "coord_declare_intent",
    "coord_diagnose",
    "coord_diff_impact",
    "coord_edit_predict",
    "coord_edit_predict_and_check",
    "coord_edit_verify",
    "coord_expectation_checkin",
    "coord_expectation_close",
    "coord_expectation_register",
    "coord_explain_isolation_decision",
    "coord_explain_pr_close",
    "coord_explain_ref_event",
    "coord_explain_worktree",
    "coord_find_references",
    "coord_gate_doctor",
    "coord_gate_inspect",
    "coord_gate_list",
    "coord_gate_status",
    "coord_get_answer",
    "coord_get_prompt_document",
    "coord_inbox",
    "coord_is_commit_live",
    "coord_is_merge_safe",
    "coord_layering_triage",
    "coord_list_prompt_documents",
    "coord_list_worktrees",
    "coord_memory_overview",
    "coord_memory_record",
    "coord_memory_search",
    "coord_merge_order",
    "coord_migration_queue",
    "coord_orient",
    "coord_post_finding",
    "coord_pr_status",
    "coord_predict_resource_collisions",
    "coord_recent_errors",
    "coord_recent_findings",
    "coord_record_decision",
    "coord_register_gate",
    "coord_report_status",
    "coord_request_handoff",
    "coord_resolve_origin",
    "coord_resolve_pr_author_session",
    "coord_resolve_session",
    "coord_secret_presence",
    "coord_send_message",
    "coord_signature",
    "coord_slo_metrics",
    "coord_symbol_lookup",
    "coord_twin_catalog",
    "coord_typecheck_file",
    "coord_who_is_working_on",
    "coord_withdraw_gate",
    "coord_work_unit_add_citation",
    "coord_work_unit_list",
    "coord_work_unit_list_citations",
    "coord_work_unit_remove_citation",
    "coord_work_unit_transition",
    "coord_work_unit_upsert",
    "coord_write_prompt_document",
    "coord_yield",
];

/// Read-only tool FAMILIES allowed by prefix. `coord_query_*` is coord's
/// naming convention for its read-only twin/drift/state queries (~25 tools,
/// all reads); enumerating each would drift out of date with every new query
/// while adding no authority a session doesn't already have via the other
/// reads.
const COORD_MCP_ALLOWED_TOOL_PREFIXES: &[&str] = &["coord_query_"];

fn coord_mcp_tool_is_allowed(name: &str) -> bool {
    COORD_MCP_ALLOWED_TOOLS.binary_search(&name).is_ok()
        || COORD_MCP_ALLOWED_TOOL_PREFIXES
            .iter()
            .any(|p| name.starts_with(p))
}

/// Is this request body a `tools/list` (single or anywhere in a batch)?
///
/// Deliberately permissive about the batch case: only a `tools/list` response
/// carries `result.tools`, so a batch that contains one is enough to justify
/// walking the response.
fn coord_mcp_request_is_tools_list(body: &[u8]) -> bool {
    // Cheap pre-check first. This runs on EVERY proxied request, and the gate
    // has already parsed the body once; a substring miss rules out `tools/list`
    // without a second full parse. A hit still falls through to the real parse,
    // so the literal appearing inside an argument string proves nothing on its
    // own.
    const NEEDLE: &[u8] = b"tools/list";
    if !body.windows(NEEDLE.len()).any(|w| w == NEEDLE) {
        return false;
    }
    let parsed: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let is_list =
        |v: &serde_json::Value| v.get("method").and_then(|m| m.as_str()) == Some("tools/list");
    match &parsed {
        serde_json::Value::Array(elems) => elems.iter().any(is_list),
        other => is_list(other),
    }
}

/// Drop non-allowlisted entries from ONE response object's `result.tools`,
/// recording each dropped name in `removed`. An entry without a string `name`
/// is removed too: it can never satisfy [`coord_mcp_tool_is_allowed`], so it is
/// exactly as uncallable as a refused one; it is recorded as `(unnamed)`.
///
/// The names are collected rather than merely counted so the caller can LOG
/// them. A capability this door subtracts silently is the exact failure this
/// whole function exists to stop being invisible.
fn coord_mcp_retain_allowed_tools(resp: &mut serde_json::Value, removed: &mut Vec<String>) {
    let tools = match resp
        .pointer_mut("/result/tools")
        .and_then(|t| t.as_array_mut())
    {
        Some(t) => t,
        None => return,
    };
    tools.retain(|t| match t.get("name").and_then(|n| n.as_str()) {
        Some(name) if coord_mcp_tool_is_allowed(name) => true,
        Some(name) => {
            removed.push(name.to_string());
            false
        }
        None => {
            removed.push("(unnamed)".to_string());
            false
        }
    });
}

/// Filter a `tools/list` RESPONSE through the SAME allowlist that gates
/// `tools/call` ([`coord_mcp_tool_is_allowed`]).
///
/// Without this the two gates disagree. The request gate ([`coord_mcp_body_gate`])
/// refuses a `tools/call` for a tool missing from [`COORD_MCP_ALLOWED_TOOLS`],
/// but the response was forwarded verbatim — so `tools/list` advertised every
/// tool coord grants the device, including ones this proxy will never forward.
/// A session could see such a tool, load its schema, and get -32601 only on
/// use. Verified 2026-08-10 with `coord_memory_overview`: landed and deployed
/// in coord's device grant, listed here, uncallable through this door.
///
/// Absence from `tools/list` is the signal sessions already read this door with
/// (a tool that is not there is understood as not available). Making the list
/// agree with the gate is what keeps that signal honest — and it is why adding
/// a name to [`COORD_MCP_ALLOWED_TOOLS`] is the ONE place that now controls
/// both visibility and callability.
///
/// Returns `None` when nothing was removed, so the untouched upstream bytes are
/// forwarded byte-identically — the overwhelmingly common case, and the one
/// where re-serialising would be pure risk for no gain. `Some((bytes, removed))`
/// carries the dropped tool NAMES so the caller can log them.
fn coord_mcp_filter_tools_list_response(
    request: &[u8],
    response: &[u8],
) -> Option<(Vec<u8>, Vec<String>)> {
    if !coord_mcp_request_is_tools_list(request) {
        return None;
    }
    let mut parsed: serde_json::Value = serde_json::from_slice(response).ok()?;
    let mut removed: Vec<String> = Vec::new();
    match &mut parsed {
        serde_json::Value::Array(elems) => {
            for elem in elems.iter_mut() {
                coord_mcp_retain_allowed_tools(elem, &mut removed);
            }
        }
        other => coord_mcp_retain_allowed_tools(other, &mut removed),
    }
    if removed.is_empty() {
        return None;
    }
    let bytes = serde_json::to_vec(&parsed).ok()?;
    Some((bytes, removed))
}

/// A gate rejection: the JSON-RPC `id` to echo (Null when unparseable) plus a
/// human-actionable message.
struct CoordMcpBodyRejection {
    id: serde_json::Value,
    message: String,
}

/// Allowlist gate over the `/coord-mcp` proxy's JSON-RPC body (credential-
/// hygiene Task 4): the `method` must be in [`COORD_MCP_ALLOWED_METHODS`] and
/// a `tools/call` must name an allowed tool ([`coord_mcp_tool_is_allowed`]).
/// Batch (array) bodies are validated per element — one bad element rejects
/// the whole request (coord's `/mcp` is single-shot per POST anyway). Pure
/// over its input so the rejection matrix is unit-testable.
fn coord_mcp_body_gate(body: &[u8]) -> Result<(), CoordMcpBodyRejection> {
    let parsed: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return Err(CoordMcpBodyRejection {
                id: serde_json::Value::Null,
                message: format!("request body is not valid JSON-RPC: {e}"),
            });
        }
    };
    match &parsed {
        serde_json::Value::Array(elems) => {
            if elems.is_empty() {
                return Err(CoordMcpBodyRejection {
                    id: serde_json::Value::Null,
                    message: "empty JSON-RPC batch".to_string(),
                });
            }
            for elem in elems {
                coord_mcp_request_gate_one(elem)?;
            }
            Ok(())
        }
        _ => coord_mcp_request_gate_one(&parsed),
    }
}

/// Gate ONE JSON-RPC request object. See [`coord_mcp_body_gate`].
fn coord_mcp_request_gate_one(req: &serde_json::Value) -> Result<(), CoordMcpBodyRejection> {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let reject = |message: String| {
        Err(CoordMcpBodyRejection {
            id: id.clone(),
            message,
        })
    };
    let method = match req.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => return reject("JSON-RPC request has no string `method`".to_string()),
    };
    if COORD_MCP_ALLOWED_METHODS.binary_search(&method).is_err() {
        return reject(format!(
            "JSON-RPC method {method:?} is not on the /coord-mcp proxy allowlist"
        ));
    }
    if method == "tools/call" {
        let tool = match req.pointer("/params/name").and_then(|n| n.as_str()) {
            Some(t) => t,
            None => return reject("tools/call has no string `params.name`".to_string()),
        };
        if !coord_mcp_tool_is_allowed(tool) {
            return reject(format!(
                "tool {tool:?} is not on the /coord-mcp proxy allowlist for \
                 device/agent sessions"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Semantic recall: fill in `coord_memory_search`'s query vector
// (plan `2026-07-30-semantic-recall-query-embedding-via-runner`, Phases 2-3)
//
// The web backend accepts a query vector and by DESIGN never computes one, so
// coord's memory search has always run full-text only. The runner is the
// fleet's embedder (`2026-07-13-runner-paid-embedding`) and is already sitting
// in the request path, so it is the one place that can put an existing vector
// into an existing field. Coord's side of the wire contract landed first
// (coord `bd0d30f3`) and must be SERVING before this enriches anything —
// `additionalProperties: false` would otherwise reject the fields.
// ---------------------------------------------------------------------------

/// Hard ceiling on the query embed.
///
/// This MUST be imposed out here: `EmbeddingClient` builds its own reqwest
/// client with a **30 s** timeout, 200x this budget, so relying on the client's
/// own ceiling would stall a search for half a minute on a wedged embedding
/// service — the precise failure the fail-open design exists to prevent.
///
/// Sized from measurement, not guesswork: the local service was probed at
/// ~10 ms warm and 6.01 s cold. The warm path keeps a ~15x margin; the cold
/// path deliberately blows the budget and loses its semantic arm, because a
/// 6-second search is worse than a lexical one.
const MEMORY_EMBED_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);

/// Where a `coord_memory_search` enrichment attempt ended.
///
/// Counted ONLY for requests that are themselves `coord_memory_search`
/// tools/calls — every other proxied request is untouched and uncounted, so
/// these series read as "of the searches that went through, how many got a
/// vector". Without them "semantic recall is on" is unfalsifiable, which is
/// exactly the failure mode this work exists to correct: the arm can silently
/// stop firing (a tool rename, a dead embedder, a coord parameter rename) and
/// degrade to FTS-only with no signal at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryEnrichOutcome {
    /// A query vector was computed and injected.
    Enriched,
    /// The caller supplied `query_embedding` themselves — never overwritten.
    SkippedPresent,
    /// The local embedding service errored or was unreachable.
    ///
    /// Read this TOGETHER with [`Self::SkippedTimeout`] as "the embedder did
    /// not answer": which of the two a hard-down service lands in depends on
    /// whether the OS refuses the connection inside
    /// [`MEMORY_EMBED_TIMEOUT`], and measurably it does not always — a
    /// connect to a closed local port exceeded the 150 ms budget under test.
    /// The split is a latency hint, not a clean up/down signal.
    SkippedUnavailable,
    /// The embed exceeded [`MEMORY_EMBED_TIMEOUT`] — a slow embedder, a cold
    /// start (measured 6.01 s), or a down one whose refusal was slower than
    /// the budget. See [`Self::SkippedUnavailable`].
    SkippedTimeout,
    /// A `coord_memory_search` call whose shape could not be enriched (a
    /// JSON-RPC batch, or no usable string `params.arguments.query_text`).
    SkippedParse,
    /// The embedding service answered 200 with a vector of the WRONG WIDTH.
    /// Its own client does not check length, coord deliberately validates no
    /// dimension, and the backend rejects a non-`EMBEDDING_DIM` vector with a
    /// 422 — so injecting one would make every search fail CLOSED, which is
    /// precisely what this design forbids. Degrade to FTS instead.
    SkippedDimension,
}

impl MemoryEnrichOutcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Enriched => "enriched",
            Self::SkippedPresent => "skipped_present",
            Self::SkippedUnavailable => "skipped_unavailable",
            Self::SkippedTimeout => "skipped_timeout",
            Self::SkippedParse => "skipped_parse",
            Self::SkippedDimension => "skipped_dimension",
        }
    }

    /// Index into [`memory_enrich_counters`]. An EXHAUSTIVE match, deliberately
    /// — the obvious `ALL.iter().position(..).unwrap_or(0)` silently folds any
    /// outcome missing from `ALL` into index 0 (`Enriched`), inflating the one
    /// series that is supposed to be the positive proof the arm is firing.
    /// This way the compiler refuses to build until a new variant is mapped.
    pub(crate) const fn idx(self) -> usize {
        match self {
            Self::Enriched => 0,
            Self::SkippedPresent => 1,
            Self::SkippedUnavailable => 2,
            Self::SkippedTimeout => 3,
            Self::SkippedParse => 4,
            Self::SkippedDimension => 5,
        }
    }

    pub(crate) const ALL: [Self; 6] = [
        Self::Enriched,
        Self::SkippedPresent,
        Self::SkippedUnavailable,
        Self::SkippedTimeout,
        Self::SkippedParse,
        Self::SkippedDimension,
    ];
}

/// Per-outcome counters, indexed by [`MemoryEnrichOutcome::idx`].
fn memory_enrich_counters() -> &'static [std::sync::atomic::AtomicU64; 6] {
    static COUNTERS: std::sync::OnceLock<[std::sync::atomic::AtomicU64; 6]> =
        std::sync::OnceLock::new();
    COUNTERS.get_or_init(Default::default)
}

fn record_memory_enrich_outcome(outcome: MemoryEnrichOutcome) {
    memory_enrich_counters()[outcome.idx()].fetch_add(1, Ordering::Relaxed);
}

/// Snapshot of the enrichment counters for `GET /health`.
pub(crate) fn memory_enrich_health_snapshot() -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for outcome in MemoryEnrichOutcome::ALL {
        // Keyed on `idx()`, NOT on position in `ALL`. Writes go through
        // `idx()`, so reading by position would silently mislabel every series
        // the moment someone reorders `ALL` — a plausible cosmetic edit, since
        // `ALL` also dictates the key order of this JSON. Both sides must agree
        // via the same function; `all_labels_read_back_their_own_slot` pins it.
        obj.insert(
            outcome.label().to_string(),
            serde_json::json!(memory_enrich_counters()[outcome.idx()].load(Ordering::Relaxed)),
        );
    }
    serde_json::Value::Object(obj)
}

/// What a proxied body is, as far as query-vector enrichment is concerned.
enum MemorySearchShape<'a> {
    /// Not a `coord_memory_search` tools/call. Do not touch, do not count —
    /// this is the overwhelming majority of proxied traffic and its bytes stay
    /// identical.
    NotASearch,
    /// Enrichable; carries the query text to embed.
    ///
    /// `needs_cleanup` marks a body that carries a HALF-PAIR we must remove if
    /// we end up NOT enriching — a `query_embedding: null`, or a
    /// `query_embedding_model` with no vector. Coord rejects both shapes
    /// (`(Some(Null), _)` fails its `is_array` check; `(None, Some(_))` fails
    /// its pair check), so forwarding such a body "untouched" on a degrade path
    /// would hard-error the search instead of degrading it to FTS. Byte-identity
    /// is only owed to bodies coord would actually accept.
    Enrichable {
        query_text: &'a str,
        needs_cleanup: bool,
    },
    /// A search, but not one we will rewrite. Carries the outcome to record.
    Skip(MemoryEnrichOutcome),
}

fn is_memory_search_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && req.pointer("/params/name").and_then(|n| n.as_str()) == Some("coord_memory_search")
}

/// Decide whether a parsed proxy body is an enrichable `coord_memory_search`.
///
/// Pure over its input, so the whole trigger matrix is unit-testable without an
/// HTTP server or a live embedding service — which matters more than usual
/// here, because the guarantee being protected is that EVERY other request
/// stays byte-identical.
fn classify_memory_search(parsed: &serde_json::Value) -> MemorySearchShape<'_> {
    // Batches are never rewritten. coord's `/mcp` is single-shot per POST, so a
    // batch carrying a search is a shape we do not produce; rewriting one would
    // mean reasoning about partial-batch enrichment for no caller that exists.
    if let Some(elems) = parsed.as_array() {
        return if elems.iter().any(is_memory_search_call) {
            MemorySearchShape::Skip(MemoryEnrichOutcome::SkippedParse)
        } else {
            MemorySearchShape::NotASearch
        };
    }
    if !is_memory_search_call(parsed) {
        return MemorySearchShape::NotASearch;
    }
    let args = parsed.pointer("/params/arguments");
    // Hands off ONLY when the caller supplied a real VECTOR: theirs may be in a
    // deliberately different space, and replacing it would make the response's
    // `vector_arm` describe an arm they never asked for.
    //
    // Deliberately NOT keyed on the model tag as well. A tag without a vector
    // names a space that has nothing in it — there is no cross-space risk to
    // protect, the vector can only come from us, and coord rejects that lone
    // tag outright. Skipping on it would convert a search that works today into
    // a hard error. An explicit JSON `null` is likewise not a supplied vector;
    // some MCP clients serialize absent optionals that way.
    let real_vector = args
        .and_then(|a| a.get("query_embedding"))
        .is_some_and(|v| !v.is_null());
    if real_vector {
        return MemorySearchShape::Skip(MemoryEnrichOutcome::SkippedPresent);
    }
    // Either leftover key makes the body one coord would REFUSE if we forwarded
    // it unchanged, so a degrade path has to strip it. See `Enrichable`.
    let needs_cleanup = args.is_some_and(|a| {
        a.get("query_embedding").is_some() || a.get("query_embedding_model").is_some()
    });
    match args
        .and_then(|a| a.get("query_text"))
        .and_then(|t| t.as_str())
    {
        Some(t) if !t.trim().is_empty() => MemorySearchShape::Enrichable {
            query_text: t,
            needs_cleanup,
        },
        _ => MemorySearchShape::Skip(MemoryEnrichOutcome::SkippedParse),
    }
}

/// Remove a leftover `query_embedding` / `query_embedding_model` half-pair.
/// Returns whether anything was actually removed.
fn strip_query_embedding_pair(parsed: &mut serde_json::Value) -> bool {
    let Some(args) = parsed
        .pointer_mut("/params/arguments")
        .and_then(|a| a.as_object_mut())
    else {
        return false;
    };
    let had_vec = args.remove("query_embedding").is_some();
    let had_tag = args.remove("query_embedding_model").is_some();
    had_vec || had_tag
}

/// Record a non-enriching outcome and produce the body to forward.
///
/// `None` means "forward the original bytes". A cleaned body is returned only
/// when the caller left a half-pair behind that coord would refuse — the whole
/// point of the degrade path is that the search still returns FTS hits.
fn degraded_body(
    parsed: &mut serde_json::Value,
    needs_cleanup: bool,
    outcome: MemoryEnrichOutcome,
) -> Option<Vec<u8>> {
    record_memory_enrich_outcome(outcome);
    if !needs_cleanup || !strip_query_embedding_pair(parsed) {
        return None;
    }
    serde_json::to_vec(parsed).ok()
}

/// Write the computed pair into a parsed `coord_memory_search` body.
///
/// The model tag travels under **`query_embedding_model`** — the QUERY leg's
/// field name. The WRITE leg of the same memory API calls the same tag
/// `embedding_model`, and both coord and the backend reject a vector whose
/// space is unnamed, so using the write leg's name here would make every
/// enriched search fail CLOSED with a 422 instead of degrading to FTS. That is
/// not hypothetical: the runner's sibling tenant-memory arm shipped with
/// exactly that mistake.
fn inject_query_embedding(parsed: &mut serde_json::Value, embedding: Vec<f32>) -> bool {
    let Some(args) = parsed
        .pointer_mut("/params/arguments")
        .and_then(|a| a.as_object_mut())
    else {
        return false;
    };
    args.insert("query_embedding".to_string(), serde_json::json!(embedding));
    args.insert(
        "query_embedding_model".to_string(),
        serde_json::Value::String(
            crate::database::embedding_client::EMBEDDING_MODEL_TAG.to_string(),
        ),
    );
    true
}

/// Fill in `coord_memory_search`'s query vector, or leave the body alone.
///
/// `None` means "forward the original bytes unchanged" — every non-search
/// request and every failure path over a body coord already accepts.
/// `Some(bytes)` means a rewritten body: usually the enriched one, but also the
/// CLEANED one on a degrade path when the caller left a half-pair behind.
///
/// **Fail open, always.** Embedder down, embed error, timeout, unexpected
/// shape — the search still reaches coord and still returns FTS hits
/// (`vector_arm: "skipped_no_embedding"`). Recall must never break because a
/// local service is slow or missing. Note that "forward untouched" is the
/// wrong move for a body carrying a lone/`null` half-pair: coord refuses those,
/// so degrading correctly means stripping them rather than preserving bytes.
async fn enrich_memory_search_body(body: &[u8]) -> Option<Vec<u8>> {
    enrich_memory_search_body_with(body, None).await
}

/// The process-wide embedding client.
///
/// Shared (same posture as `COORD_PROXY_CLIENT` below) so the connection pool
/// survives between searches — which is exactly what the 150 ms budget needs.
/// Built on FIRST USE by a real search, not on the first proxied request of any
/// kind: `reqwest::Client::builder().build()` does synchronous backend init
/// inside a `OnceLock` that parks concurrent callers, and ordinary traffic
/// should not pay for it.
fn shared_embed_client() -> &'static crate::database::embedding_client::EmbeddingClient {
    static EMBED_CLIENT: std::sync::OnceLock<crate::database::embedding_client::EmbeddingClient> =
        std::sync::OnceLock::new();
    EMBED_CLIENT.get_or_init(crate::database::embedding_client::EmbeddingClient::new)
}

/// Embedder-parameterized core of [`enrich_memory_search_body`], so tests can
/// point it at an unreachable (or slow) service without depending on whether
/// this machine happens to be running the real one. Same shape as the sibling
/// `retrieve_tenant_memory_at`.
async fn enrich_memory_search_body_with(
    body: &[u8],
    client: Option<&crate::database::embedding_client::EmbeddingClient>,
) -> Option<Vec<u8>> {
    let mut parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    let (query_text, needs_cleanup) = match classify_memory_search(&parsed) {
        MemorySearchShape::NotASearch => return None,
        MemorySearchShape::Skip(outcome) => {
            record_memory_enrich_outcome(outcome);
            return None;
        }
        MemorySearchShape::Enrichable {
            query_text,
            needs_cleanup,
        } => (query_text.to_string(), needs_cleanup),
    };

    // Only a real search reaches the client. No `is_available()` pre-check
    // either: it is a second round trip AND a TOCTOU window — the service can
    // die between the probe and the compute, so the error path below has to
    // exist regardless. One call, one failure path.
    // The closure (rather than passing `shared_embed_client` directly) is
    // load-bearing: as a bare fn item its `&'static` return unifies the
    // parameter's lifetime to `'static`, which would forbid a caller — every
    // test — from passing a borrow of a local client.
    let client = client.unwrap_or_else(|| shared_embed_client());

    let embedding = match tokio::time::timeout(
        MEMORY_EMBED_TIMEOUT,
        client.compute_text_embedding(&query_text),
    )
    .await
    {
        // The service can answer 200 with a vector of the wrong width — a model
        // swap or a misconfigured deployment — and NOTHING between here and the
        // database catches it: `compute_text_embedding` checks no length, coord
        // deliberately validates no dimension (it is the backend's call to
        // make), and the backend then rejects it with a 422. Injecting such a
        // vector would therefore turn EVERY search into a hard error while the
        // health series happily reported `enriched`. Degrade instead — the
        // whole point is that a broken embedder costs a search its semantic
        // arm, never its response.
        Ok(Ok(e)) if e.len() == crate::database::embeddings::EMBEDDING_DIM => e,
        Ok(Ok(e)) => {
            tracing::debug!(
                dims = e.len(),
                expected = crate::database::embeddings::EMBEDDING_DIM,
                "coord-mcp proxy: embedder returned an unexpected vector width — \
                 forwarding search FTS-only"
            );
            return degraded_body(
                &mut parsed,
                needs_cleanup,
                MemoryEnrichOutcome::SkippedDimension,
            );
        }
        Ok(Err(e)) => {
            tracing::debug!(
                error = %e,
                "coord-mcp proxy: query embed failed — forwarding search FTS-only"
            );
            return degraded_body(
                &mut parsed,
                needs_cleanup,
                MemoryEnrichOutcome::SkippedUnavailable,
            );
        }
        Err(_) => {
            tracing::debug!(
                timeout_ms = MEMORY_EMBED_TIMEOUT.as_millis() as u64,
                "coord-mcp proxy: query embed timed out — forwarding search FTS-only"
            );
            return degraded_body(
                &mut parsed,
                needs_cleanup,
                MemoryEnrichOutcome::SkippedTimeout,
            );
        }
    };

    if !inject_query_embedding(&mut parsed, embedding) {
        return degraded_body(
            &mut parsed,
            needs_cleanup,
            MemoryEnrichOutcome::SkippedParse,
        );
    }
    match serde_json::to_vec(&parsed) {
        Ok(bytes) => {
            record_memory_enrich_outcome(MemoryEnrichOutcome::Enriched);
            Some(bytes)
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                "coord-mcp proxy: enriched body failed to serialize — forwarding original"
            );
            record_memory_enrich_outcome(MemoryEnrichOutcome::SkippedParse);
            None
        }
    }
}

async fn coord_mcp_proxy_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let nonce = headers
        .get(crate::coord_mcp::COORD_MCP_PROXY_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // Resolve the principal FROM THE NONCE first — the binding (not the bearer)
    // decides which identity's token this session may inject. An unregistered /
    // absent nonce 401s before any token I/O.
    let principal = match nonce
        .as_deref()
        .and_then(crate::coord_mcp::proxy_principal_for_nonce)
    {
        Some(p) => p,
        None => {
            warn!("coord-mcp proxy: missing or unrecognized X-Coord-Mcp-Proxy-Key");
            // Rotation forensics: THE transport-death event. Everything the
            // rotation log records up to here is what the runner did to a key;
            // this is a client dying on one, and the `key_prefix` join is what
            // ties it back to the mint/evict line that caused it.
            crate::coord_mcp::spawn_log_proxy_nonce_rejected(
                nonce.as_deref(),
                "missing, unregistered, or expired proxy key (401)",
            );
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "success": false,
                    "error": "missing or unrecognized X-Coord-Mcp-Proxy-Key",
                    "code": "COORD_MCP_PROXY_UNAUTHORIZED",
                })),
            )
                .into_response();
        }
    };

    // Credential-hygiene Task 4: allowlist the JSON-RPC method + tool BEFORE
    // any token I/O — a nonce authenticates a *session*, not an operator, so a
    // request outside the enumerated coordination surface is never forwarded
    // (mirrors the sibling ClaimsReadTarget/CoordWriteTarget posture). -32601
    // ("method not found") deliberately does not disclose whether the tool
    // exists upstream.
    if let Err(reject) = coord_mcp_body_gate(&body) {
        warn!(
            "coord-mcp proxy: refused non-allowlisted JSON-RPC request: {}",
            reject.message
        );
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": reject.id,
                "error": {
                    "code": -32601,
                    "message": reject.message,
                    "data": { "code": "COORD_MCP_PROXY_METHOD_NOT_ALLOWED" },
                },
            })),
        )
            .into_response();
    }

    // Pick the bearer by principal:
    //  - Device → the live device JWT read from AuthManager (filesystem I/O, so
    //    off the async executor), the same fresh token `backend_relay` reads.
    //  - Agent  → THAT agent's own refreshed JWT from its AGENT_TOKENS slot; a
    //    belt-and-suspenders `maybe_refresh` keeps it live on the request path
    //    too. An absent slot (torn-down / restarted agent) is a hard 401.
    let bearer = match &principal {
        crate::coord_mcp::ProxyPrincipal::Device => {
            // B3: select the bearer for the SESSION's tenant (frozen on the
            // nonce at provision time), NOT the legacy `access_token` slot.
            // `device_bearer_for(Some(t))` serves the legacy slot only when `t`
            // IS the default binding; for a NON-default tenant it returns the
            // tenant's own slot, or `None` on a miss — it NEVER presents another
            // tenant's credential. A `None` session tenant (single-tenant / no
            // active pin) resolves the legacy default slot, unchanged pre-B3.
            let session_tenant = nonce
                .as_deref()
                .and_then(crate::coord_mcp::proxy_session_tenant_for_nonce);
            // Read the live device JWT (filesystem I/O → off the async executor),
            // the same fresh token `backend_relay` reads for this tenant.
            let mut tok = tokio::task::spawn_blocking(move || {
                crate::auth::device_bearer_for(session_tenant.as_ref())
            })
            .await
            .ok()
            .flatten();
            // Phase 3 (terminal-autonomy-survives-logout): a momentarily-missing
            // device JWT is almost always the refresher's re-mint window or a
            // transient-backoff gap (Phases 1-2) — NOT a dead session. Kick the
            // refresher and wait a tightly-bounded time for a re-mint before
            // degrading, so an in-flight tool call the AI makes mid-turn rides
            // through the gap instead of erroring on a bare 401. For a
            // non-default tenant with no slot this stays empty (never the legacy
            // slot) → the degrade path below, per the B3 invariant.
            if tok.as_deref().map(str::trim).unwrap_or("").is_empty() {
                tok = crate::coord_mcp::await_device_jwt_remint_for(session_tenant).await;
            }
            match tok {
                Some(t) if !t.trim().is_empty() => Some(t),
                _ => {
                    // STILL no JWT after the bounded wait: degrade to an
                    // actionable, retry-shaped error (NOT the bare 401, NOT a
                    // hang) so the autonomous caller knows to retry shortly.
                    let (status, msg) = crate::coord_mcp::device_jwt_refreshing_error();
                    warn!(
                        "coord-mcp proxy: device JWT still missing after bounded \
                         re-mint wait — degrading to retry ({status})"
                    );
                    return (
                        axum::http::StatusCode::from_u16(status)
                            .unwrap_or(axum::http::StatusCode::SERVICE_UNAVAILABLE),
                        Json(serde_json::json!({
                            "success": false,
                            "error": msg,
                            "code": "COORD_MCP_PROXY_CREDENTIAL_REFRESHING",
                            "retryable": true,
                        })),
                    )
                        .into_response();
                }
            }
        }
        crate::coord_mcp::ProxyPrincipal::Agent { agent_id } => {
            match crate::coord_mcp::lookup_agent_token(*agent_id) {
                Some(slot) => {
                    let _ = crate::agent_token::maybe_refresh(
                        &slot,
                        &crate::coord_mcp::coord_base_url(),
                        *agent_id,
                        "agent_mcp",
                    )
                    .await;
                    Some(slot.read().await.token.clone())
                }
                None => {
                    warn!(
                        "coord-mcp proxy: no live token slot for agent_id={agent_id} \
                         (torn-down or restarted agent) — failing closed"
                    );
                    crate::coord_mcp::spawn_log_proxy_nonce_rejected(
                        nonce.as_deref(),
                        "no live agent token slot — torn-down or restarted agent (401)",
                    );
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({
                            "success": false,
                            "error": "no live agent token for this proxy session",
                            "code": "COORD_MCP_PROXY_AGENT_GONE",
                        })),
                    )
                        .into_response();
                }
            }
        }
    };

    if let Err((status, msg)) =
        crate::coord_mcp::proxy_request_gate(nonce.as_deref(), bearer.as_deref(), &principal)
    {
        warn!("coord-mcp proxy: {msg}");
        // The gate's own message is the cause — it never embeds key material
        // (it reports the nonce as absent/unrecognized, or names the bearer's
        // `sub_type`), so it is safe to carry into a forensics line verbatim.
        crate::coord_mcp::spawn_log_proxy_nonce_rejected(
            nonce.as_deref(),
            format!("{msg} ({status})"),
        );
        return (
            axum::http::StatusCode::from_u16(status)
                .unwrap_or(axum::http::StatusCode::UNAUTHORIZED),
            Json(serde_json::json!({
                "success": false,
                "error": msg,
                "code": "COORD_MCP_PROXY_UNAUTHORIZED",
            })),
        )
            .into_response();
    }
    let bearer = bearer.unwrap_or_default(); // gate guarantees Some(non-empty)

    // Session-fabric Phase 0: resolve the calling terminal's OWN coord
    // agent_session_id so coord self-identifies the caller deterministically
    // instead of guessing the device's most-recent session. DEVICE principal
    // only — agent-spawn sessions carry their own scoped identity and are out
    // of scope here. Best-effort: any missing link yields None ⇒ the header is
    // omitted and coord falls back to its fuzzy pick.
    //
    // DELIBERATELY UNGATED on the runner side. This used to also require
    // `COORD_SESSION_SELF_ID=observe` in the RUNNER's process env, which made
    // the feature un-armable in practice: a runner snapshots its environment at
    // launch, fleet policy forbids restarting an active runner, and so the flag
    // could be correct on disk and absent in-process indefinitely — which is
    // exactly what kept Phase 0 inert from the day it shipped until it was
    // measured on 2026-07-22. coord's own `COORD_SESSION_SELF_ID` is now the
    // single arm: it decides whether to HONOR the header, and it is IaC, so a
    // flip needs a deploy rather than a runner restart. Sending unconditionally
    // is safe because the header is advisory — coord trusts it only after
    // `session_on_device` proves device binding (fail-closed), the JWT remains
    // the authorization boundary, and the strip of any CLIENT-supplied copy
    // below is likewise unconditional, so no client can spoof a sibling
    // session's identity.
    let (caller_session_id, self_id_outcome) =
        if matches!(&principal, crate::coord_mcp::ProxyPrincipal::Device) {
            resolve_caller_session_id(&state, nonce.as_deref())
        } else {
            (None, SelfIdOutcome::NonDevicePrincipal)
        };
    record_self_id_outcome(self_id_outcome);

    // Shared client: connect fast-fail, generous overall timeout (coord MCP
    // tool calls can legitimately run long).
    static COORD_PROXY_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = COORD_PROXY_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("coord-mcp proxy reqwest client")
    });

    // Semantic recall (Phase 2): put a locally-computed query vector into
    // `coord_memory_search`'s existing field. `None` — every other request, and
    // every failure path — forwards the ORIGINAL bytes, byte-identically.
    // `Bytes` clones are refcounted, so the untouched path costs a pointer
    // bump rather than a full copy of every proxied body.
    let forward_body: reqwest::Body = match enrich_memory_search_body(&body).await {
        Some(rewritten) => rewritten.into(),
        None => body.clone().into(),
    };

    // Resolve the upstream once, WITH its source, so every error emitted below
    // can name both the exact URL dialed and how it was chosen (plan
    // 2026-07-16-runner-prod-coord-base-default-and-502-self-diagnosis, D3) —
    // a bare 502 that names neither cost real diagnostic time in the incident.
    let (url, coord_base_source) = crate::coord_mcp::coord_mcp_url_with_source();
    let mut req = client.post(&url).bearer_auth(&bearer).body(forward_body);
    for (name, value) in headers.iter() {
        let n = name.as_str();
        // Hop-by-hop / recomputed headers, plus the ones we own: the nonce must
        // not leak upstream, the Authorization slot is the live bearer, and the
        // caller-session header is authoritative ONLY when the RUNNER sets it —
        // a client-supplied one must never pass through (it could otherwise name
        // a sibling session on the same device to spoof its identity).
        if matches!(
            n,
            "host"
                | "content-length"
                | "connection"
                | "transfer-encoding"
                | "accept-encoding"
                | "authorization"
        ) || n == crate::coord_mcp::COORD_MCP_PROXY_KEY_HEADER
            || n == crate::coord_mcp::CALLER_SESSION_HEADER
        {
            continue;
        }
        req = req.header(n, value.as_bytes());
    }
    // Inject the runner-resolved caller-session id (Phase 0). Coord still
    // validates it fail-closed as bound to the caller's device before trusting
    // it, so this is advisory — but stripping any client copy above means coord
    // only ever sees the runner's own attribution.
    if let Some(sid) = caller_session_id {
        req = req.header(crate::coord_mcp::CALLER_SESSION_HEADER, sid.to_string());
    }

    let upstream = match req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            warn!(
                "coord-mcp proxy: forward to {url} failed \
                 (coord_base_source={coord_base_source}): {e}"
            );
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord /mcp unreachable: {e}"),
                    "code": "COORD_MCP_PROXY_UPSTREAM_UNREACHABLE",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                })),
            )
                .into_response();
        }
    };

    let status = upstream.status().as_u16();
    let status_code =
        axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK);
    let upstream_content_type = upstream
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    // `upstream.bytes()` consumes the response, so snapshot the headers we
    // forward before reading the body.
    let upstream_headers = upstream.headers().clone();

    let bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "coord-mcp proxy: reading coord response body from {url} failed \
                 (coord_base_source={coord_base_source}): {e}"
            );
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord /mcp response read failed: {e}"),
                    "code": "COORD_MCP_PROXY_UPSTREAM_READ_FAILED",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                })),
            )
                .into_response();
        }
    };

    // Envelope non-JSON upstream errors instead of mirroring them.
    //
    // A gateway in front of coord (ALB, CloudFront) answers a 502/504 with an
    // HTML error page. Forwarding that verbatim gave this route a `text/html`
    // 5xx, which is useless to an MCP client (it can only parse JSON) AND trips
    // the debug-only `envelope_audit` layer, whose premise — "a non-JSON error
    // response is a handler bug" — does not hold for a pass-through proxy.
    // That panic is caught by CatchPanicLayer, but the panic hook still writes a
    // crash dump, so a transient coord blip made a healthy runner look crashed.
    //
    // The upstream STATUS is preserved (a 504 stays a 504); only the unusable
    // body is replaced with a parseable envelope carrying a snippet for triage.
    let is_error = status_code.is_client_error() || status_code.is_server_error();
    if is_error && !upstream_content_type.starts_with("application/json") {
        let snippet: String = String::from_utf8_lossy(&bytes)
            .chars()
            .take(200)
            .collect::<String>()
            .trim()
            .to_owned();
        warn!(
            status,
            content_type = %if upstream_content_type.is_empty() {
                "(missing)"
            } else {
                &upstream_content_type
            },
            upstream_url = %url,
            coord_base_source = %coord_base_source,
            "coord-mcp proxy: enveloping non-JSON upstream error from coord"
        );
        return (
            status_code,
            Json(serde_json::json!({
                "success": false,
                "error": format!(
                    "coord /mcp returned {status} with a non-JSON body (likely a gateway \
                     error page, not coord itself)"
                ),
                "code": "COORD_MCP_PROXY_UPSTREAM_NON_JSON_ERROR",
                "upstreamStatus": status,
                "upstreamContentType": upstream_content_type,
                "upstreamBodySnippet": snippet,
                "upstream_url": url,
                "coord_base_source": coord_base_source.as_str(),
            })),
        )
            .into_response();
    }

    // Keep the two gates in agreement: a `tools/list` answer is filtered
    // through the same allowlist that gates `tools/call`, so this door never
    // advertises a tool it would refuse to forward. See
    // [`coord_mcp_filter_tools_list_response`]. `None` = nothing removed =
    // forward the upstream bytes untouched.
    let out_body = match coord_mcp_filter_tools_list_response(&body, &bytes) {
        Some((filtered, removed)) => {
            // INFO, not debug: this is a capability coord granted that this
            // door withholds. It is usually correct (privileged families), but
            // when it is NOT — a tool added to coord's grant and missed here —
            // this line is the only thing standing between the next engineer
            // and an hour of "why is it listed but -32601?".
            info!(
                removed_count = removed.len(),
                removed = %removed.join(","),
                "coord-mcp proxy: withheld non-allowlisted tools from a tools/list response"
            );
            axum::body::Body::from(filtered)
        }
        None => axum::body::Body::from(bytes),
    };

    let mut builder = axum::http::Response::builder().status(status_code);
    for (name, value) in upstream_headers.iter() {
        // `content-length` is dropped here anyway, which is also what makes the
        // tools/list filter above safe: the forwarded body may be shorter than
        // upstream's.
        if matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    builder.body(out_body).unwrap_or_else(|e| {
        warn!(
            "coord-mcp proxy: response build failed \
                 (upstream_url={url}, coord_base_source={coord_base_source}): {e}"
        );
        (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "success": false,
                "error": format!("coord /mcp response build failed: {e}"),
                "code": "COORD_MCP_PROXY_RESPONSE_BUILD_FAILED",
                "upstream_url": url,
                "coord_base_source": coord_base_source.as_str(),
            })),
        )
            .into_response()
    })
}

/// The ONLY coord READ routes the nonce-gated read passthrough may reach:
/// the two claims reads (plan 2026-06-11-claims-read-auth-hardening, Phase 2)
/// plus the work-unit dependency-edge read (device-session coord surface
/// hardening follow-up) — coord serves `GET /coord/work-units/{slug}/deps`
/// on its device-JWT `work_units_agent_authed` sub-router (`require_jwt`),
/// so a device bearer is accepted there.
///
/// Deliberately a closed enum rather than a path parameter: the per-session
/// proxy nonce authenticates a *session*, not an operator, so its authority
/// must stay scoped to these enumerated read-only endpoints. A generic
/// `/coord-mcp/proxy/{path}` passthrough would let a leaked nonce reach any
/// coord route with the runner's device identity — arbitrary paths must be
/// structurally impossible, not merely unrouted. The `WorkUnitDeps` slug is
/// validated to a safe charset ([`slug_is_valid`]) before any URL is built,
/// exactly like [`CoordWriteTarget`]'s dynamic segments.
///
/// NOTE the deliberate EXCLUSION: `GET /coord/work-units/{slug}` (the bare
/// work-unit read) moved to coord's operator read sub-router — its `TenantId`
/// extractor resolves SOLELY from an `OperatorContext` (Cognito bearer), so a
/// device JWT gets 403 `tenant_not_resolved`. Do not forward it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaimsReadTarget {
    /// `GET {coord}/coord/claims/list`
    List,
    /// `GET {coord}/coord/claims/by-resource`
    ByResource,
    /// `GET {coord}/coord/work-units/{slug}/deps`
    WorkUnitDeps { slug: String },
}

impl ClaimsReadTarget {
    fn coord_path(&self) -> String {
        match self {
            ClaimsReadTarget::List => "/coord/claims/list".to_string(),
            ClaimsReadTarget::ByResource => "/coord/claims/by-resource".to_string(),
            ClaimsReadTarget::WorkUnitDeps { slug } => {
                format!("/coord/work-units/{slug}/deps")
            }
        }
    }

    /// Validate the dynamic segment (if any). Returns `Err((status, code, msg))`
    /// on a bad shape so the caller can emit a runner-originated 400 — the
    /// segment is rejected BEFORE any coord URL is built, mirroring
    /// [`CoordWriteTarget::validate`].
    fn validate(&self) -> Result<(), (u16, &'static str, String)> {
        match self {
            ClaimsReadTarget::List | ClaimsReadTarget::ByResource => Ok(()),
            ClaimsReadTarget::WorkUnitDeps { slug } => {
                if slug_is_valid(slug) {
                    Ok(())
                } else {
                    Err((
                        400,
                        "COORD_CLAIMS_PROXY_BAD_TARGET",
                        format!("invalid work-unit slug: {slug:?}"),
                    ))
                }
            }
        }
    }
}

/// Build the upstream coord URL for a claims read: allowlisted path from the
/// [`ClaimsReadTarget`] enum + the inbound query string forwarded VERBATIM
/// (still percent-encoded — `axum::extract::RawQuery` hands us the raw form,
/// so coord decodes exactly what the session's client encoded).
fn claims_upstream_url(base: &str, target: ClaimsReadTarget, raw_query: Option<&str>) -> String {
    let mut url = format!("{}{}", base.trim_end_matches('/'), target.coord_path());
    if let Some(q) = raw_query {
        if !q.is_empty() {
            url.push('?');
            url.push_str(q);
        }
    }
    url
}

/// `GET /coord-mcp/claims/list` + `GET /coord-mcp/claims/by-resource` — the
/// nonce-gated claims READ passthrough for device-provisioned sessions (plan
/// 2026-06-11-claims-read-auth-hardening, Phase 2).
///
/// Why: the claims consumers (PreToolUse hook helper, skill wait-poll) need
/// plain REST GETs against coord's claims endpoints, but a device session's
/// `.mcp.json` carries no bearer anymore (live-token proxy, runner #546) —
/// only the per-session loopback nonce. This route lets those helpers reuse
/// the nonce: same gate, same live `AuthManager` device-JWT injection, same
/// coord-base resolution as `coord_mcp_proxy_handler`, but restricted to the
/// two read-only claims routes in [`ClaimsReadTarget`].
///
/// Gate (`coord_mcp::proxy_request_gate`, 401 before any network I/O):
/// registered `X-Coord-Mcp-Proxy-Key` nonce AND the live bearer decodes
/// `sub_type == "device"` — absent/wrong nonce or a missing/non-device token
/// means the request is NEVER forwarded to coord.
///
/// Coord's response status + body are returned verbatim (no reshaping) so the
/// helper sees exactly what coord said; runner-originated failures use the
/// distinct `COORD_CLAIMS_PROXY_*` codes below so they can't be mistaken for
/// a coord verdict.
async fn coord_claims_read_proxy_handler(
    target: ClaimsReadTarget,
    headers: axum::http::HeaderMap,
    raw_query: Option<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let nonce = headers
        .get(crate::coord_mcp::COORD_MCP_PROXY_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // This route injects the DEVICE bearer, so it serves DEVICE-bound nonces
    // only — reject an agent nonce up front so it can never borrow the device
    // identity (the scope-elevation trap on this passthrough).
    match nonce
        .as_deref()
        .and_then(crate::coord_mcp::proxy_principal_for_nonce)
    {
        Some(crate::coord_mcp::ProxyPrincipal::Device) => {}
        _ => {
            warn!("coord-mcp claims proxy: missing/non-device X-Coord-Mcp-Proxy-Key");
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "success": false,
                    "error": "missing or non-device X-Coord-Mcp-Proxy-Key",
                    "code": "COORD_CLAIMS_PROXY_UNAUTHORIZED",
                })),
            )
                .into_response();
        }
    }

    // Live read — the same fresh device JWT the `/coord-mcp` proxy injects, for
    // the SESSION's tenant (B3): select via `device_bearer_for` off the nonce's
    // frozen tenant, never the legacy default slot. A non-default slot miss
    // resolves `None` here → the gate 401s (the safe degrade), never another
    // tenant's credential. AuthManager does filesystem I/O, so keep it off the
    // async executor.
    let session_tenant = nonce
        .as_deref()
        .and_then(crate::coord_mcp::proxy_session_tenant_for_nonce);
    let bearer = tokio::task::spawn_blocking(move || {
        crate::auth::device_bearer_for(session_tenant.as_ref())
    })
    .await
    .ok()
    .flatten();

    if let Err((status, msg)) = crate::coord_mcp::proxy_request_gate(
        nonce.as_deref(),
        bearer.as_deref(),
        &crate::coord_mcp::ProxyPrincipal::Device,
    ) {
        warn!("coord-mcp claims proxy: {msg}");
        return (
            axum::http::StatusCode::from_u16(status)
                .unwrap_or(axum::http::StatusCode::UNAUTHORIZED),
            Json(serde_json::json!({
                "success": false,
                "error": msg,
                "code": "COORD_CLAIMS_PROXY_UNAUTHORIZED",
            })),
        )
            .into_response();
    }
    let bearer = bearer.unwrap_or_default(); // gate guarantees Some(non-empty)

    // Validate the dynamic segment (if any) BEFORE building any coord URL — a
    // bad shape is a runner-originated 400, never forwarded.
    if let Err((status, code, msg)) = target.validate() {
        warn!("coord-mcp claims proxy: {msg}");
        return (
            axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::BAD_REQUEST),
            Json(serde_json::json!({
                "success": false,
                "error": msg,
                "code": code,
            })),
        )
            .into_response();
    }

    let (coord_base, coord_base_source) = crate::coord_mcp::coord_base_url_with_source();
    let url = claims_upstream_url(&coord_base, target, raw_query.as_deref());
    forward_claims_get(&url, &bearer, coord_base_source).await
}

/// Forward a claims read to coord and return coord's status + headers + body
/// verbatim. Split from the handler (URL + bearer + base-source as plain
/// params, no `AuthManager`/env reads) so the forwarding leg is unit-testable
/// against a local mock coord with a synthetic bearer — the live credential
/// lives in the encrypted `AuthManager` slot, which a unit test cannot seed.
/// `coord_base_source` names how the upstream base was chosen; it is echoed
/// into every runner-originated 502 body alongside the URL dialed (D3).
async fn forward_claims_get(
    url: &str,
    bearer: &str,
    coord_base_source: qontinui_runner_lib::profiles::CoordBaseSource,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // Shared client: connect fast-fail like the `/coord-mcp` proxy client, but
    // a much shorter overall timeout — these are bounded REST reads (a hook
    // helper sits on the response), not long-running MCP tool calls.
    static COORD_CLAIMS_PROXY_CLIENT: std::sync::OnceLock<reqwest::Client> =
        std::sync::OnceLock::new();
    let client = COORD_CLAIMS_PROXY_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("coord claims proxy reqwest client")
    });

    let upstream = match client.get(url).bearer_auth(bearer).send().await {
        Ok(resp) => resp,
        Err(e) => {
            warn!(
                "coord-mcp claims proxy: forward to {url} failed \
                 (coord_base_source={coord_base_source}): {e}"
            );
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord claims endpoint unreachable: {e}"),
                    "code": "COORD_CLAIMS_PROXY_UPSTREAM_UNREACHABLE",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                })),
            )
                .into_response();
        }
    };

    let status = upstream.status().as_u16();
    let mut builder = axum::http::Response::builder()
        .status(axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK));
    for (name, value) in upstream.headers() {
        if matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "coord-mcp claims proxy: reading coord response body from {url} failed \
                 (coord_base_source={coord_base_source}): {e}"
            );
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord claims response read failed: {e}"),
                    "code": "COORD_CLAIMS_PROXY_UPSTREAM_READ_FAILED",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                })),
            )
                .into_response();
        }
    };
    builder
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|e| {
            warn!(
                "coord-mcp claims proxy: response build failed \
                 (upstream_url={url}, coord_base_source={coord_base_source}): {e}"
            );
            (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord claims response build failed: {e}"),
                    "code": "COORD_CLAIMS_PROXY_RESPONSE_BUILD_FAILED",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                })),
            )
                .into_response()
        })
}

/// `GET /coord-mcp/claims/list` — see [`coord_claims_read_proxy_handler`].
async fn coord_claims_list_handler(
    headers: axum::http::HeaderMap,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> axum::response::Response {
    coord_claims_read_proxy_handler(ClaimsReadTarget::List, headers, raw_query).await
}

/// `GET /coord-mcp/claims/by-resource` — see [`coord_claims_read_proxy_handler`].
async fn coord_claims_by_resource_handler(
    headers: axum::http::HeaderMap,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> axum::response::Response {
    coord_claims_read_proxy_handler(ClaimsReadTarget::ByResource, headers, raw_query).await
}

/// The ONLY coord WRITE routes the nonce-gated device-JWT forwarder may reach:
/// the gate attest write (plan 2026-06-15-coord-mcp-live-token-write-forwarder,
/// Phase 1), the claim-anchored gate register (plan
/// 2026-07-21-gate-cascade-step3-proxy-rebase, Phase 1b), plus the work-unit
/// registry writes (device-session coord surface hardening follow-up). The
/// write sibling of [`ClaimsReadTarget`].
///
/// There is deliberately NO plan-anchored gate-register variant. Coord's
/// `POST /coord/plans/{slug}/register-gate` was DELETED with the rest of the
/// `/coord/plans*` surface (coord P4 Phase 3), so a forwarder aimed at it could
/// only ever 404 whatever the body. Its live replacement is
/// [`CoordWriteTarget::WorkUnitRegisterGate`] below — note the bodies are NOT
/// interchangeable: the removed plan route took coord's `PlanGateRequest`
/// (plan-lifecycle `status`/`title`, run through the plan status vocabulary),
/// while the work-unit route takes `UnitGateRequest` (no `status`/`title`;
/// work-unit statuses are opaque). Removed by plan
/// 2026-08-03-gate-class-producers-and-clearance-rules-inert (P4).
///
/// Every work-unit variant maps to coord's device-JWT `work_units_agent_authed`
/// sub-router (layered with `require_jwt`, which accepts the device bearer):
/// `POST /coord/work-units/upsert`, `POST /coord/work-units/{slug}/transition`,
/// `POST /coord/work-units/{slug}/register-gate`, and
/// `POST /coord/work-units/{slug}/deps`. Deliberately EXCLUDED (operator-only
/// on coord — a device JWT gets 403 `tenant_not_resolved` or 401):
/// `GET /coord/work-units/{slug}` / `/history` / the list read (operator
/// `TenantId` sub-router) and `POST /coord/work-units/{slug}/operator-transition`
/// (admin-gated operator lever).
///
/// Deliberately a closed enum carrying a *validated* dynamic segment rather
/// than a free path parameter, for exactly the security boundary documented on
/// [`ClaimsReadTarget`] at mcp_api.rs:559-567: the per-session proxy nonce
/// authenticates a *session*, not an operator, so its authority must stay
/// scoped to these enumerated device-authed write endpoints. A generic
/// `/coord-mcp/proxy/{path}` POST passthrough would let a leaked nonce reach
/// any coord write route with the runner's device identity — arbitrary paths
/// must be structurally impossible, not merely unrouted. The dynamic segment
/// (`slug` / `gate_id`) is validated to a safe charset before any URL is built,
/// so it can never smuggle a path (`..`, `/`, `%2f`, …) past the fixed route
/// template.
///
/// Note: coord's continuation-cancel route is operator/`TenantId`-only (not
/// device-authed), so it is deliberately excluded — a `ContinuationCancel`
/// variant would never authenticate through this device-JWT path anyway.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CoordWriteTarget {
    /// `POST {coord}/coord/gates/{gate_id}/attest`
    AttestGate { gate_id: String },
    /// `POST {coord}/coord/gates/register-agent` — coord's device-authed
    /// claim-anchored register route (the REST twin of MCP
    /// `coord_register_gate`). The claim anchor (`claim_kind` +
    /// `resource_key`) travels in the JSON body; no dynamic path segment.
    RegisterGate,
    /// `POST {coord}/coord/work-units/upsert` (slug travels in the JSON body;
    /// no dynamic path segment)
    WorkUnitUpsert,
    /// `POST {coord}/coord/work-units/{slug}/transition`
    WorkUnitTransition { slug: String },
    /// `POST {coord}/coord/work-units/{slug}/register-gate`
    WorkUnitRegisterGate { slug: String },
    /// `POST {coord}/coord/work-units/{slug}/deps` (replace-set dependency
    /// edge write)
    WorkUnitSetDeps { slug: String },
}

/// A coord plan slug stem: lowercase alphanumeric + hyphens, must start with an
/// alphanumeric (`^[a-z0-9][a-z0-9-]*$`). Rejects `/`, `.`, `%`, whitespace, and
/// uppercase, so a slug can never carry a path separator or escape sequence into
/// the fixed coord route template.
fn slug_is_valid(slug: &str) -> bool {
    let mut chars = slug.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A coord gate id: a canonical UUID (8-4-4-4-12 hex, length 36). Parsed via the
/// `uuid` crate (already a dependency) so only a real UUID — never a path
/// fragment — can reach the fixed `/coord/gates/{gate_id}/attest` template.
fn gate_id_is_valid(gate_id: &str) -> bool {
    uuid::Uuid::parse_str(gate_id).is_ok()
}

impl CoordWriteTarget {
    /// Validate the dynamic segment. Returns `Err((status, code, msg))` on a bad
    /// shape so the caller can emit a runner-originated 400 (the segment is
    /// rejected before any coord URL is built — a bad path can never be smuggled).
    fn validate(&self) -> Result<(), (u16, &'static str, String)> {
        match self {
            CoordWriteTarget::RegisterGate | CoordWriteTarget::WorkUnitUpsert => Ok(()),
            CoordWriteTarget::WorkUnitTransition { slug }
            | CoordWriteTarget::WorkUnitRegisterGate { slug }
            | CoordWriteTarget::WorkUnitSetDeps { slug } => {
                if slug_is_valid(slug) {
                    Ok(())
                } else {
                    Err((
                        400,
                        "COORD_WRITE_PROXY_BAD_TARGET",
                        format!("invalid slug: {slug:?}"),
                    ))
                }
            }
            CoordWriteTarget::AttestGate { gate_id } => {
                if gate_id_is_valid(gate_id) {
                    Ok(())
                } else {
                    Err((
                        400,
                        "COORD_WRITE_PROXY_BAD_TARGET",
                        format!("invalid gate id (must be a UUID): {gate_id:?}"),
                    ))
                }
            }
        }
    }
}

/// Build the upstream coord URL for a write: a FIXED coord route template with
/// the (already-validated) dynamic segment interpolated. The constant template
/// is the whole point — the validated charset means plain interpolation cannot
/// alter the path structure.
///
/// Callers MUST have called [`CoordWriteTarget::validate`] first (the handler
/// does); this builder assumes the segment is already safe.
fn write_upstream_url(base: &str, target: &CoordWriteTarget) -> String {
    let base = base.trim_end_matches('/');
    match target {
        CoordWriteTarget::AttestGate { gate_id } => {
            format!("{base}/coord/gates/{gate_id}/attest")
        }
        CoordWriteTarget::RegisterGate => {
            format!("{base}/coord/gates/register-agent")
        }
        CoordWriteTarget::WorkUnitUpsert => {
            format!("{base}/coord/work-units/upsert")
        }
        CoordWriteTarget::WorkUnitTransition { slug } => {
            format!("{base}/coord/work-units/{slug}/transition")
        }
        CoordWriteTarget::WorkUnitRegisterGate { slug } => {
            format!("{base}/coord/work-units/{slug}/register-gate")
        }
        CoordWriteTarget::WorkUnitSetDeps { slug } => {
            format!("{base}/coord/work-units/{slug}/deps")
        }
    }
}

/// `POST /coord-mcp/gates/register` +
/// `POST /coord-mcp/gates/{gate_id}/attest` +
/// `POST /coord-mcp/work-units/{upsert | {slug}/transition |
/// {slug}/register-gate | {slug}/deps}` — the nonce-gated device-JWT WRITE
/// forwarder for device-provisioned sessions (plan
/// 2026-06-15-coord-mcp-live-token-write-forwarder, Phase 1; work-unit surface
/// added by the device-session coord surface hardening follow-up;
/// claim-anchored gate register added by plan
/// 2026-07-21-gate-cascade-step3-proxy-rebase, Phase 1b).
///
/// Why: a device session's `.mcp.json` carries no bearer anymore (live-token
/// proxy, runner #546) — only the per-session loopback nonce. The gate-register,
/// gate-attest, and work-unit registry flows need to POST against coord's
/// device-authed write routes, so this lets those callers reuse the nonce:
/// same gate, same live `AuthManager` device-JWT injection, same coord-base
/// resolution as `coord_mcp_proxy_handler`, but restricted to the enumerated
/// write routes in [`CoordWriteTarget`] with a validated dynamic segment.
///
/// Gate (`coord_mcp::proxy_request_gate`, 401 before any network I/O):
/// registered `X-Coord-Mcp-Proxy-Key` nonce AND the live bearer decodes
/// `sub_type == "device"` — absent/wrong nonce or a missing/non-device token
/// means the request is NEVER forwarded to coord.
///
/// Coord's response status + body are returned verbatim (no reshaping) so the
/// caller sees exactly what coord said; runner-originated failures use the
/// distinct `COORD_WRITE_PROXY_*` codes so they can't be mistaken for a coord
/// verdict.
async fn coord_write_proxy_handler(
    target: CoordWriteTarget,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let nonce = headers
        .get(crate::coord_mcp::COORD_MCP_PROXY_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // This route injects the DEVICE bearer, so it serves DEVICE-bound nonces
    // only — reject an agent nonce up front so it can never borrow the device
    // identity (the scope-elevation trap on this passthrough).
    match nonce
        .as_deref()
        .and_then(crate::coord_mcp::proxy_principal_for_nonce)
    {
        Some(crate::coord_mcp::ProxyPrincipal::Device) => {}
        _ => {
            warn!("coord-mcp write proxy: missing/non-device X-Coord-Mcp-Proxy-Key");
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "success": false,
                    "error": "missing or non-device X-Coord-Mcp-Proxy-Key",
                    "code": "COORD_WRITE_PROXY_UNAUTHORIZED",
                })),
            )
                .into_response();
        }
    }

    // Live read — the same fresh device JWT the `/coord-mcp` proxy injects, for
    // the SESSION's tenant (B3): select via `device_bearer_for` off the nonce's
    // frozen tenant, never the legacy default slot. A non-default slot miss
    // resolves `None` here → the gate 401s (the safe degrade), never another
    // tenant's credential. AuthManager does filesystem I/O, so keep it off the
    // async executor.
    let session_tenant = nonce
        .as_deref()
        .and_then(crate::coord_mcp::proxy_session_tenant_for_nonce);
    let bearer = tokio::task::spawn_blocking(move || {
        crate::auth::device_bearer_for(session_tenant.as_ref())
    })
    .await
    .ok()
    .flatten();

    if let Err((status, msg)) = crate::coord_mcp::proxy_request_gate(
        nonce.as_deref(),
        bearer.as_deref(),
        &crate::coord_mcp::ProxyPrincipal::Device,
    ) {
        warn!("coord-mcp write proxy: {msg}");
        return (
            axum::http::StatusCode::from_u16(status)
                .unwrap_or(axum::http::StatusCode::UNAUTHORIZED),
            Json(serde_json::json!({
                "success": false,
                "error": msg,
                "code": "COORD_WRITE_PROXY_UNAUTHORIZED",
            })),
        )
            .into_response();
    }
    let bearer = bearer.unwrap_or_default(); // gate guarantees Some(non-empty)

    // Validate the dynamic segment BEFORE building any coord URL — a bad shape
    // is a runner-originated 400, never forwarded.
    if let Err((status, code, msg)) = target.validate() {
        warn!("coord-mcp write proxy: {msg}");
        return (
            axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::BAD_REQUEST),
            Json(serde_json::json!({
                "success": false,
                "error": msg,
                "code": code,
            })),
        )
            .into_response();
    }

    let (coord_base, coord_base_source) = crate::coord_mcp::coord_base_url_with_source();
    let url = write_upstream_url(&coord_base, &target);
    forward_coord_write_post(&url, &bearer, body, coord_base_source).await
}

/// Forward a write POST to coord and return coord's status + headers + body
/// verbatim. Split from the handler (URL + bearer + body + base-source as
/// plain params, no `AuthManager`/env reads) so the forwarding leg is
/// unit-testable against a local mock coord with a synthetic bearer — the live
/// credential lives in the encrypted `AuthManager` slot, which a unit test
/// cannot seed. `coord_base_source` names how the upstream base was chosen; it
/// is echoed into every runner-originated 502 body alongside the URL dialed.
async fn forward_coord_write_post(
    url: &str,
    bearer: &str,
    body: axum::body::Bytes,
    coord_base_source: qontinui_runner_lib::profiles::CoordBaseSource,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // Shared client: connect fast-fail like the claims proxy client, with a
    // short overall timeout — these are bounded REST writes (a caller sits on
    // the response), not long-running MCP tool calls.
    static COORD_WRITE_PROXY_CLIENT: std::sync::OnceLock<reqwest::Client> =
        std::sync::OnceLock::new();
    let client = COORD_WRITE_PROXY_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("coord write proxy reqwest client")
    });

    let upstream = match client
        .post(url)
        .bearer_auth(bearer)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            warn!(
                "coord-mcp write proxy: forward to {url} failed \
                 (coord_base_source={coord_base_source}): {e}"
            );
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord write endpoint unreachable: {e}"),
                    "code": "COORD_WRITE_PROXY_UPSTREAM_UNREACHABLE",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                })),
            )
                .into_response();
        }
    };

    let status = upstream.status().as_u16();
    let mut builder = axum::http::Response::builder()
        .status(axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK));
    for (name, value) in upstream.headers() {
        if matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "coord-mcp write proxy: reading coord response body from {url} failed \
                 (coord_base_source={coord_base_source}): {e}"
            );
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord write response read failed: {e}"),
                    "code": "COORD_WRITE_PROXY_UPSTREAM_READ_FAILED",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                })),
            )
                .into_response();
        }
    };
    builder
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|e| {
            warn!(
                "coord-mcp write proxy: response build failed \
                 (upstream_url={url}, coord_base_source={coord_base_source}): {e}"
            );
            (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord write response build failed: {e}"),
                    "code": "COORD_WRITE_PROXY_RESPONSE_BUILD_FAILED",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                })),
            )
                .into_response()
        })
}

/// `POST /coord-mcp/gates/register` — see [`coord_write_proxy_handler`].
async fn coord_register_gate_handler(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    coord_write_proxy_handler(CoordWriteTarget::RegisterGate, headers, body).await
}

/// `POST /coord-mcp/gates/{gate_id}/attest` — see [`coord_write_proxy_handler`].
async fn coord_attest_gate_handler(
    axum::extract::Path(gate_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    coord_write_proxy_handler(CoordWriteTarget::AttestGate { gate_id }, headers, body).await
}

/// `POST /coord-mcp/work-units/upsert` — see [`coord_write_proxy_handler`].
async fn coord_work_unit_upsert_handler(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    coord_write_proxy_handler(CoordWriteTarget::WorkUnitUpsert, headers, body).await
}

/// `POST /coord-mcp/work-units/{slug}/transition` — see
/// [`coord_write_proxy_handler`].
async fn coord_work_unit_transition_handler(
    axum::extract::Path(slug): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    coord_write_proxy_handler(CoordWriteTarget::WorkUnitTransition { slug }, headers, body).await
}

/// `POST /coord-mcp/work-units/{slug}/register-gate` — see
/// [`coord_write_proxy_handler`].
async fn coord_work_unit_register_gate_handler(
    axum::extract::Path(slug): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    coord_write_proxy_handler(
        CoordWriteTarget::WorkUnitRegisterGate { slug },
        headers,
        body,
    )
    .await
}

/// `POST /coord-mcp/work-units/{slug}/deps` — see [`coord_write_proxy_handler`].
async fn coord_work_unit_set_deps_handler(
    axum::extract::Path(slug): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    coord_write_proxy_handler(CoordWriteTarget::WorkUnitSetDeps { slug }, headers, body).await
}

/// `GET /coord-mcp/work-units/{slug}/deps` — see
/// [`coord_claims_read_proxy_handler`] (the nonce-gated device-JWT READ
/// passthrough; this route rides the same gate + forward leg as the claims
/// reads, with the slug validated before any URL is built).
async fn coord_work_unit_deps_get_handler(
    axum::extract::Path(slug): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> axum::response::Response {
    coord_claims_read_proxy_handler(ClaimsReadTarget::WorkUnitDeps { slug }, headers, raw_query)
        .await
}

// ---------------------------------------------------------------------------
// Session coord-identity mint route (plan
// 2026-07-17-universal-coord-device-identity-for-any-session, §1)
// ---------------------------------------------------------------------------

/// Request body for `POST /coord-mcp/provision-session`.
#[derive(Debug, Clone, serde::Deserialize)]
struct ProvisionSessionBody {
    /// The requesting session's working directory. The minted nonce is BOUND to
    /// this path (`NonceBinding::workdir`), which is what gives a bare session
    /// correct per-session attribution: coord resolves
    /// nonce → workdir → task_run_id → `agent_session_id`, and
    /// `coord_declare_intent`'s peer-overlap derivation is only meaningful if
    /// that workdir is the session's REAL cwd.
    cwd: String,
}

/// `POST /coord-mcp/provision-session` — mint coord device identity for a
/// session the runner did NOT spawn (plan
/// `2026-07-17-universal-coord-device-identity-for-any-session` §1).
///
/// # Why the route exists
///
/// Coord device identity is not ambient: it is injected at spawn time by
/// `TerminalSession::apply_identity_seam`, whose sole non-test call site is the
/// runner's own PTY-terminal spawn (`terminal/session.rs:509`). A session
/// launched any other way — a bare PowerShell/Git-Bash window, VS Code's
/// integrated terminal, a cron-fired agent — never passes through that seam, so
/// it gets no `--mcp-config`, no nonce, and authenticates with neither a
/// `device_id` nor an `agent_id` claim, which is exactly the shape the
/// device-scoped coord tools reject. A launcher (the identity shim, a *parent*
/// of `claude`) POSTs here to ask the runner to mint, then passes the returned
/// document to `claude --mcp-config`. The nonce is minted IN-PROCESS and can
/// only be minted here — no external process can fabricate one, and none must
/// ever be given a mechanism to.
///
/// # ⚠ SECURITY — this route is the ONE structural exception in its family. Do
/// # not "fix" the missing nonce check.
///
/// Every OTHER `/coord-mcp/*` route is nonce-gated: `coord_mcp_proxy_handler`
/// 401s an unrecognized `X-Coord-Mcp-Proxy-Key` *before any token I/O*, and the
/// claims/work-unit/gate/PR forwarders all inherit that discipline. **This route
/// cannot be nonce-gated — it is what ISSUES the nonce** (chicken-and-egg: a
/// caller that already held a valid nonce would not need to call it). Its
/// authorization is therefore [`crate::coord_mcp::session_identity_gate`], which
/// stands **IN PLACE OF** the nonce check:
///
/// 1. the master flag `QONTINUI_SESSION_COORD_IDENTITY_ENABLED` — default OFF,
///    so the feature ships dark and an un-flagged runner exposes nothing; AND
/// 2. a per-machine operator opt-in marker
///    (`~/.qontinui/allow-session-coord-identity`).
///
/// Both are required. If you are reading this because the missing nonce check
/// looked like a bug: it is deliberate, and those two gates are the entire
/// authorization story. Removing or weakening either grants any local process —
/// including a compromised dependency's post-install script — the ability to
/// mint device identity and act as the operator against coord. "It came from
/// 127.0.0.1" is NOT an authorization signal on a single-user box, where every
/// process runs as the same OS user.
///
/// Three properties contain the blast radius of what is issued (see
/// `coord_mcp::NonceLifetime`): the nonce is DEVICE-principal (never agent — no
/// scope elevation), bound to the caller's `cwd`, and Ephemeral — bounded TTL,
/// never written to disk, and revoked the instant the operator deletes the
/// marker.
///
/// # Contract
///
/// Request: `{"cwd": "<absolute path to an existing directory>"}`.
///
/// `200` — body IS the `.mcp.json` document, verbatim and ready to write to a
/// temp file and pass as `--mcp-config <path>`:
///
/// ```json
/// {"mcpServers":{"coord-mcp":{"type":"http",
///   "url":"http://127.0.0.1:<bound_port>/coord-mcp",
///   "headers":{"X-Coord-Mcp-Proxy-Key":"<nonce>"}}}}
/// ```
///
/// The URL names THIS runner's own bound port, so the nonce↔port pairing is
/// automatic and the caller must never re-derive the port (the load-bearing
/// no-scan rule at `bin/qontinui_cli.rs:434-437`: a nonce paired with a scanned
/// port 401s).
///
/// Every failure is an explicit typed reason with a non-2xx status and a
/// `{success:false, error, code}` body — NEVER a silent empty (the runner's
/// no-silent-empty rule), because "denied" and "broken" have different fixes:
///
/// | Status | `code` | Meaning |
/// |---|---|---|
/// | 400 | `COORD_MCP_PROVISION_INVALID_BODY` | body is not `{cwd:String}` |
/// | 400 | `COORD_MCP_PROVISION_INVALID_CWD` | `cwd` empty or not an existing dir |
/// | 403 | `COORD_MCP_PROVISION_DISABLED` | master flag off (the default) |
/// | 403 | `COORD_MCP_PROVISION_NOT_OPTED_IN` | no opt-in marker on this machine |
/// | 503 | `COORD_MCP_PROVISION_PORT_UNRESOLVABLE` | bound port unresolvable — fail-closed |
///
/// Callers are expected to fail OPEN on every one of these: a launcher that
/// cannot get identity must still launch the session, un-shimmed.
async fn coord_provision_session_handler(body: axum::body::Bytes) -> axum::response::Response {
    use axum::response::IntoResponse;

    let err = |status: axum::http::StatusCode, code: &str, msg: String| {
        (
            status,
            Json(serde_json::json!({
                "success": false,
                "error": msg,
                "code": code,
            })),
        )
            .into_response()
    };

    // Gate FIRST — before parsing, before any registry or credential touch. In
    // the default (dark) posture this is one env read and the route is
    // indistinguishable from one that mints nothing.
    if let Err(denial) = crate::coord_mcp::session_identity_gate() {
        warn!(
            "coord-mcp provision-session: denied ({}) — {}",
            denial.code(),
            denial.message()
        );
        return err(
            axum::http::StatusCode::FORBIDDEN,
            denial.code(),
            denial.message(),
        );
    }

    let req: ProvisionSessionBody = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return err(
                axum::http::StatusCode::BAD_REQUEST,
                "COORD_MCP_PROVISION_INVALID_BODY",
                format!("expected a JSON body of the shape {{\"cwd\": \"<path>\"}}: {e}"),
            );
        }
    };

    // A nonce bound to a directory that does not exist is bound to a fiction:
    // the workdir binding is what coord resolves back to a session, so a bogus
    // cwd would silently produce un-attributable identity. Note we do NOT
    // canonicalize — the seam registers raw cwd strings, and Windows
    // canonicalization yields `\\?\`-prefixed paths that would never match them.
    let cwd = req.cwd.trim();
    if cwd.is_empty() || !std::path::Path::new(cwd).is_dir() {
        return err(
            axum::http::StatusCode::BAD_REQUEST,
            "COORD_MCP_PROVISION_INVALID_CWD",
            format!("cwd {cwd:?} is empty or not an existing directory"),
        );
    }

    // The shared mint core (§2) — fail-closed on an unresolvable bound port.
    match crate::coord_mcp::provision_session_proxy_config(cwd) {
        Some(config) => {
            info!(
                cwd = %cwd,
                "coord-mcp provision-session: minted an ephemeral device session config"
            );
            (axum::http::StatusCode::OK, Json(config)).into_response()
        }
        None => {
            warn!(
                cwd = %cwd,
                "coord-mcp provision-session: bound API port unresolvable — refusing to \
                 mint (a config on a bootstrap-default port would be dead on any \
                 secondary/temp runner)"
            );
            err(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "COORD_MCP_PROVISION_PORT_UNRESOLVABLE",
                "the runner's bound API port is unresolvable — refusing to mint a \
                 session config that would point at a dead port; retry once the \
                 runtime is up"
                    .to_string(),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Loopback PR-creation proxy (plan qontinui-pr-credential-provisioning, 2b)
// ---------------------------------------------------------------------------

/// Request body for `POST /vcs/pull-requests`. `repo` is `owner/name`; every
/// other field is forwarded to coord's PR-creation route (the coord body is
/// this struct MINUS `repo`, which travels in the coord URL path instead).
#[derive(Debug, Clone, serde::Deserialize)]
struct VcsPullRequestBody {
    repo: String,
    head: String,
    #[serde(default)]
    base: Option<String>,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: Option<bool>,
}

/// Split + validate an `owner/name` repo slug. Each segment must start with an
/// ASCII alphanumeric and continue in `[A-Za-z0-9._-]`, with `.`/`..` rejected —
/// so a segment can never smuggle a path separator or escape sequence into the
/// fixed coord route template (same boundary as [`slug_is_valid`] /
/// [`gate_id_is_valid`] on the coord-mcp write forwarder).
fn parse_owner_repo(repo: &str) -> Option<(&str, &str)> {
    let (owner, name) = repo.split_once('/')?;
    let segment_ok = |s: &str| {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphanumeric() => {}
            _ => return false,
        }
        s != "." && s != ".." && chars.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    };
    if segment_ok(owner) && segment_ok(name) && !name.contains('/') {
        Some((owner, name))
    } else {
        None
    }
}

/// Build coord's PR-creation URL for an (already-validated) `owner`/`name`:
/// `POST {coord}/coord/repos/{owner}/{repo}/pull-requests`.
fn vcs_pr_upstream_url(base: &str, owner: &str, name: &str) -> String {
    format!(
        "{}/coord/repos/{owner}/{name}/pull-requests",
        base.trim_end_matches('/')
    )
}

/// The JSON body forwarded to coord: the inbound body MINUS `repo` (which is
/// path-encoded), with absent optional fields omitted rather than nulled.
fn vcs_pr_upstream_body(req: &VcsPullRequestBody) -> serde_json::Value {
    let mut body = serde_json::json!({
        "head": req.head,
        "title": req.title,
    });
    let obj = body.as_object_mut().expect("literal object");
    if let Some(base) = &req.base {
        obj.insert("base".to_string(), serde_json::json!(base));
    }
    if let Some(b) = &req.body {
        obj.insert("body".to_string(), serde_json::json!(b));
    }
    if let Some(draft) = req.draft {
        obj.insert("draft".to_string(), serde_json::json!(draft));
    }
    body
}

/// `POST /vcs/pull-requests` — the nonce-gated PR-creation forwarder (plan
/// qontinui-pr-credential-provisioning, Phase 2b).
///
/// Why: agents in runner-hosted terminals have no personal GitHub login on the
/// machine, so `gh pr create` fails. Coord brokers PR creation with its own
/// installation credential (`POST /coord/repos/{owner}/{repo}/pull-requests`,
/// JWT-authed — coord accepts BOTH device and agent bearers on that route);
/// this loopback route lets any session that holds a per-session coord-mcp
/// proxy nonce reach it — the `qontinui-pr create` CLI is the intended caller.
///
/// Gate: exactly the coord-mcp proxy discipline — a registered
/// `X-Coord-Mcp-Proxy-Key` nonce, Device- OR Agent-bound (coord-spawned agent
/// sessions get Agent-bound nonces via `write_coord_mcp_agent_proxy_config`
/// and are this feature's primary population), and a live bearer matched to
/// the nonce's principal by [`crate::coord_mcp::proxy_request_gate`] (device
/// JWT for Device nonces, THAT agent's own refreshed JWT for Agent nonces —
/// the nonce→principal binding prevents scope elevation in either direction).
/// A device JWT that stays missing after the bounded
/// [`crate::coord_mcp::DEVICE_JWT_REMINT_WAIT`] re-mint wait is a 503
/// `runner not paired — no device JWT` (a pairing gap, not an auth failure).
/// Coord's status + body pass through verbatim so 403/404 (repo not in the
/// caller tenant) and 429 (rate limit) surface honestly; runner-originated
/// failures use distinct `VCS_PR_PROXY_*` codes.
async fn vcs_create_pull_request_handler(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let nonce = headers
        .get(crate::coord_mcp::COORD_MCP_PROXY_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // Resolve the principal FROM THE NONCE first — the binding decides which
    // identity's bearer this route injects (device JWT vs the agent's own
    // JWT). An absent/unregistered nonce 401s before any body parsing or
    // credential I/O.
    let principal = match nonce
        .as_deref()
        .and_then(crate::coord_mcp::proxy_principal_for_nonce)
    {
        Some(p) => p,
        None => {
            warn!("vcs pr proxy: missing or unrecognized X-Coord-Mcp-Proxy-Key");
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "success": false,
                    "error": "missing or unrecognized X-Coord-Mcp-Proxy-Key",
                    "code": "VCS_PR_PROXY_UNAUTHORIZED",
                })),
            )
                .into_response();
        }
    };

    // Parse + validate the request BEFORE any credential I/O — a bad shape is
    // a runner-originated 400, never forwarded.
    let req: VcsPullRequestBody = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("invalid request body: {e}"),
                    "code": "VCS_PR_PROXY_BAD_REQUEST",
                })),
            )
                .into_response();
        }
    };
    let (owner, name) = match parse_owner_repo(&req.repo) {
        Some(pair) => pair,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!(
                        "invalid repo (expected owner/name): {:?}",
                        req.repo
                    ),
                    "code": "VCS_PR_PROXY_BAD_REQUEST",
                })),
            )
                .into_response();
        }
    };
    if req.head.trim().is_empty() || req.title.trim().is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": "head and title are required",
                "code": "VCS_PR_PROXY_BAD_REQUEST",
            })),
        )
            .into_response();
    }

    // Pick the bearer by principal — the same per-principal token discipline
    // as `coord_mcp_proxy_handler`.
    let bearer = match &principal {
        crate::coord_mcp::ProxyPrincipal::Device => {
            // B3: select the bearer for the SESSION's tenant (frozen on the
            // nonce), NOT the legacy default slot — a non-default miss returns
            // `None` (the degrade path below), never another tenant's token.
            let session_tenant = nonce
                .as_deref()
                .and_then(crate::coord_mcp::proxy_session_tenant_for_nonce);
            // Live read — the same fresh device JWT the `/coord-mcp` proxy
            // injects. AuthManager does filesystem I/O, so keep it off the
            // async executor.
            let mut tok = tokio::task::spawn_blocking(move || {
                crate::auth::device_bearer_for(session_tenant.as_ref())
            })
            .await
            .ok()
            .flatten();
            // A momentarily-missing device JWT is usually the refresher's
            // re-mint window, not a dead pairing — kick the refresher and wait
            // the bounded DEVICE_JWT_REMINT_WAIT (~5s) before degrading, the
            // same idiom as the coord-mcp proxy. Only a JWT that is STILL
            // missing after the wait reports the hard 503.
            if tok.as_deref().map(str::trim).unwrap_or("").is_empty() {
                tok = crate::coord_mcp::await_device_jwt_remint_for(session_tenant).await;
            }
            match tok {
                Some(t) if !t.trim().is_empty() => Some(t),
                _ => {
                    warn!(
                        "vcs pr proxy: no device JWT after bounded re-mint wait — \
                         runner not paired"
                    );
                    return (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "success": false,
                            "error": "runner not paired — no device JWT",
                            "code": "VCS_PR_PROXY_NOT_PAIRED",
                        })),
                    )
                        .into_response();
                }
            }
        }
        crate::coord_mcp::ProxyPrincipal::Agent { agent_id } => {
            match crate::coord_mcp::lookup_agent_token(*agent_id) {
                Some(slot) => {
                    let _ = crate::agent_token::maybe_refresh(
                        &slot,
                        &crate::coord_mcp::coord_base_url(),
                        *agent_id,
                        "vcs_pr_proxy",
                    )
                    .await;
                    Some(slot.read().await.token.clone())
                }
                None => {
                    warn!(
                        "vcs pr proxy: no live token slot for agent_id={agent_id} \
                         (torn-down or restarted agent) — failing closed"
                    );
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({
                            "success": false,
                            "error": "no live agent token for this proxy session",
                            "code": "VCS_PR_PROXY_AGENT_GONE",
                        })),
                    )
                        .into_response();
                }
            }
        }
    };

    if let Err((status, msg)) =
        crate::coord_mcp::proxy_request_gate(nonce.as_deref(), bearer.as_deref(), &principal)
    {
        warn!("vcs pr proxy: {msg}");
        return (
            axum::http::StatusCode::from_u16(status)
                .unwrap_or(axum::http::StatusCode::UNAUTHORIZED),
            Json(serde_json::json!({
                "success": false,
                "error": msg,
                "code": "VCS_PR_PROXY_UNAUTHORIZED",
            })),
        )
            .into_response();
    }
    let bearer = bearer.unwrap_or_default(); // gate guarantees Some(non-empty)

    // Same coord-base resolution as every other loopback forwarder
    // (`COORD_HTTP_URL` override → profiles resolver → dev localhost).
    let url = vcs_pr_upstream_url(&crate::coord_mcp::coord_base_url(), owner, name);
    let upstream_body = vcs_pr_upstream_body(&req);
    forward_vcs_pr_post(&url, &bearer, &upstream_body).await
}

/// Forward the PR-creation POST to coord and return coord's status + headers +
/// body verbatim. Split from the handler (URL + bearer + body as plain params,
/// no `AuthManager`/env reads) so the forwarding leg is unit-testable against a
/// local mock coord with a synthetic bearer — same seam discipline as
/// [`forward_coord_write_post`], but with the plan's 10s overall timeout (PR
/// creation is a single bounded GitHub write behind coord).
async fn forward_vcs_pr_post(
    url: &str,
    bearer: &str,
    body: &serde_json::Value,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    static VCS_PR_PROXY_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = VCS_PR_PROXY_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("vcs pr proxy reqwest client")
    });

    let upstream = match client.post(url).bearer_auth(bearer).json(body).send().await {
        Ok(resp) => resp,
        Err(e) => {
            warn!("vcs pr proxy: forward to {url} failed: {e}");
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord PR endpoint unreachable: {e}"),
                    "code": "VCS_PR_PROXY_UPSTREAM_UNREACHABLE",
                })),
            )
                .into_response();
        }
    };

    let status = upstream.status().as_u16();
    let mut builder = axum::http::Response::builder()
        .status(axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK));
    for (name, value) in upstream.headers() {
        if matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!("vcs pr proxy: reading coord response body failed: {e}");
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord PR response read failed: {e}"),
                    "code": "VCS_PR_PROXY_UPSTREAM_READ_FAILED",
                })),
            )
                .into_response();
        }
    };
    builder
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|e| {
            warn!("vcs pr proxy: response build failed: {e}");
            axum::http::StatusCode::BAD_GATEWAY.into_response()
        })
}

/// Create the API router
pub fn create_router(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
    instance_manager: Arc<crate::instance_manager::InstanceManager>,
) -> Router {
    // Get dev_logs path for session manager
    let dev_logs_path = get_workspace_paths_internal()
        .map(|(_, dev_logs, _)| dev_logs)
        .unwrap_or_else(|_| std::path::PathBuf::from(".dev-logs"));

    // Ensure dev_logs directory exists
    let _ = std::fs::create_dir_all(&dev_logs_path);

    // Initialize config storage (graceful degradation if directory creation fails)
    let config_storage = match ConfigStorage::new() {
        Ok(storage) => {
            info!("Config storage initialized successfully");
            Arc::new(tokio::sync::Mutex::new(storage))
        }
        Err(e) => {
            warn!(
                "Config storage initialization failed (non-fatal): {}. Using degraded mode.",
                e
            );
            Arc::new(tokio::sync::Mutex::new(ConfigStorage::new_degraded()))
        }
    };

    // Create UnifiedActionService for deterministic execution
    let action_service = Arc::new(UnifiedActionService::new(
        app_state.clone(),
        config_storage.clone(),
    ));

    let current_ai_pids = app_state.ai_pid_tracker.clone();
    let shared_sdk_connection = app_state.sdk_connection.clone();

    // Phase 1 wrapper framework wiring:
    // - AppRegistry is shared between the HTTP phone-home path and the WS
    //   upgrade path.
    // - WsConnectionManager owns the outbound mpsc channels to wrappers.
    // - CommandRelay owns the pending-command oneshot map and uses the
    //   connection manager to dispatch frames.
    // - AppDispatcher is the single entry point handlers should use when
    //   proxying to a specific app_id; it picks HTTP or WS based on the
    //   registry entry's transport.
    let shared_app_registry = crate::mcp::app_registry::AppRegistry::new();
    let shared_ws_manager = crate::mcp::ws_relay::WsConnectionManager::new();
    let shared_command_relay =
        crate::mcp::command_relay::CommandRelay::new(shared_ws_manager.clone());
    let shared_app_dispatcher = crate::mcp::app_dispatch::AppDispatcher::new(
        shared_app_registry.clone(),
        shared_command_relay.clone(),
    );

    // Bridge the registry + dispatcher onto AppState so non-HTTP code paths
    // (workflow generation, prompt assembly) can reach them without taking a
    // dep on ApiState. The cells are pre-allocated by `main::shared_app_state`
    // construction; setting them here is a one-shot init for the lifetime of
    // the runner process.
    let _ = app_state.app_registry.set(shared_app_registry.clone());
    let _ = app_state.app_dispatcher.set(shared_app_dispatcher.clone());

    // Wrapper subsystem (Phase 1 of the wrapper-runner integration plan).
    // `create_router` is sync, so spawn the async bootstrap onto the tokio
    // runtime. The registry's filesystem scan + file watcher startup happen
    // on the spawned task. Until the OnceCell is populated `/wrappers/*`
    // returns 503 — `routes::require_wrapper_state` already handles that.
    //
    // Primary-only ownership (wrapper-primary-migration plan, Phase 1.2):
    // secondaries do not bootstrap a local WrapperState. Their `/wrappers/*`
    // handlers proxy to the primary via `wrappers::primary_proxy`. Skipping
    // the bootstrap on secondaries eliminates the cross-runner `notify`
    // watcher / `pnpm install` contention (`os error 32`).
    if !crate::process_capture::primary_proxy::is_secondary() {
        match crate::wrappers::launcher::ensure_launcher_installed() {
            Ok(path) => tracing::info!("wrappers: launcher installed at {}", path.display()),
            Err(e) => tracing::warn!("wrappers: launcher install failed: {}", e),
        }
        let cell = app_state.wrapper_state.clone();
        tokio::spawn(async move {
            let ws = crate::wrappers::WrapperState::new_default().await;
            let _ = cell.set(ws);
        });
    } else {
        tracing::info!(
            "wrappers: secondary instance — skipping local WrapperState bootstrap (proxying to primary)"
        );
    }

    // D4+D6 Blind-Spot Recommender (Phase 2): read-through facade. Holds
    // clones of the SAME shared Arcs `ApiState` holds (`shared_sdk_connection`
    // and `app_state`) so its reads are live, not snapshots.
    let observer_registry = crate::observer_registry::ObserverRegistry::new(
        shared_sdk_connection.clone(),
        app_state.clone(),
    );

    let api_state = Arc::new(ApiState {
        app_state,
        rag_state,
        app_handle: app_handle.clone(),
        current_config_id: std::sync::Mutex::new(None),
        config_storage,
        action_service,
        current_ai_pids,
        extraction_state: Arc::new(crate::mcp::extraction::ExtractionState::new()),
        sdk_connection: shared_sdk_connection,
        ui_bridge_pending: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        ui_bridge_pending_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        ui_bridge_circuit_breaker: Arc::new(crate::mcp::ui_bridge::UiBridgeCircuitBreaker::new()),
        ui_bridge_semaphore: Arc::new(tokio::sync::Semaphore::new(6)),
        ui_bridge_ready: Arc::new(tokio::sync::Notify::new()),
        ui_bridge_dedup: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        ui_bridge_console_error_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        ui_bridge_render_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        ui_bridge_relay: Arc::new(crate::mcp::ui_bridge::relay::RelayRegistry::new()),
        ui_bridge_last_discovered: Arc::new(tokio::sync::RwLock::new(None)),
        doctor_handle: None, // Doctor handle accessed via app_state.doctor_handle when needed
        started_at: std::time::Instant::now(),
        instance_manager,
        ui_bridge_event_sequence: std::sync::atomic::AtomicI64::new(0),
        knowledge_graph_cache: Arc::new(tokio::sync::RwLock::new(None)),
        graph_cache_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        accessibility_manager: Arc::new(tokio::sync::Mutex::new(
            qontinui_runner_lib::accessibility::AccessibilityManager::new(),
        )),
        physical_device_registry: Arc::new(
            crate::mcp::physical_device::PhysicalDeviceRegistry::new(),
        ),
        app_registry: shared_app_registry.clone(),
        ws_connection_manager: shared_ws_manager,
        ws_command_relay: shared_command_relay,
        app_dispatcher: shared_app_dispatcher,
        pairing_manager: Arc::new(crate::mcp::transport::pairing::PairingManager::new()),
        tunnel_client: Arc::new(crate::tunnel::RatholeClient::new()),
        ios_transport: Arc::new(crate::mcp::transport::ios::IosTransport::new()),
        ui_bridge_invoke_store: Arc::new(crate::ui_bridge_invoke::InvokeRequestStore::new()),
        ui_bridge_evaluate_store: Arc::new(crate::ui_bridge_evaluate::EvaluateRequestStore::new()),
        // D5 Phase 1 Git Supervision Channel — bounded ring + Tauri emitter.
        supervision_state: crate::git_supervision::SupervisionState::new(),
        // D4+D6 Blind-Spot Recommender Phase 2 — read-through observer facade.
        observer_registry,
        // Phase 3 vision pipeline (cache + concurrency).
        // Cache root = `tmp_vision_cache/` under the runner's working dir
        // (scratch-file whitelist per feedback_scratch_file_paths.md).
        // Cap = 512 MB per plan §3.5.
        vision_cache: {
            let runner_root = crate::mcp::shared::current_runner_path()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                });
            let root = runner_root.join("tmp_vision_cache");
            Arc::new(
                qontinui_vision_core::VisionCache::new(&root, 512 * 1024 * 1024)
                    .unwrap_or_else(|e| panic!("vision cache init at {}: {e}", root.display())),
            )
        },
        // 2 permits — xcap is GDI-bound, fits-2-parallel pattern per
        // proj_supervisor_build_pool.md.
        vision_capture_semaphore: Arc::new(tokio::sync::Semaphore::new(2)),
        vision_mutation_id: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        // Capture-backend telemetry — per-backend counters + last-fallback record.
        vision_capture_preview_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        vision_monitor_crop_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        vision_capture_fallback_seen: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vision_last_fallback: Arc::new(std::sync::Mutex::new(None)),
        // Phase 6 baselines registry — in-process, non-persistent.
        vision_baselines: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    });

    // Publish the capture-backend telemetry handles so the device-scoped 30s
    // fleet heartbeat (`fleet::heartbeat_to_coord`, on its own OS thread spawned
    // before this `ApiState` exists) can report them to coord.devices. Shares
    // the live `Arc`s the capture path bumps — see plan
    // 2026-06-07-fleet-capture-backend-telemetry.md work item 1.
    crate::fleet::publish_capture_telemetry_handles(crate::fleet::CaptureTelemetryHandles {
        capture_preview_count: api_state.vision_capture_preview_count.clone(),
        monitor_crop_count: api_state.vision_monitor_crop_count.clone(),
        last_fallback: api_state.vision_last_fallback.clone(),
    });

    // Register api_state as Tauri-managed so `#[tauri::command]` functions taking
    // `State<'_, Arc<ApiState>>` can resolve it.
    app_handle.manage(api_state.clone());

    // Spawn the background sweeper that evicts stale phone-home registrations.
    crate::mcp::app_registry::spawn_sweeper(api_state.app_registry.clone());

    // Set up UI Bridge response listener
    // This listens for "ui-bridge-response" events from the React frontend
    // and delivers responses to waiting HTTP handlers
    {
        let pending = api_state.ui_bridge_pending.clone();
        let pending_count = api_state.ui_bridge_pending_count.clone();
        let handle = app_handle.clone();

        // We need to use tauri's listen which returns a sync result
        // The listener callback will be called on the main thread

        use tauri::Listener;

        let pending_for_listener = pending.clone();
        let pending_count_for_listener = pending_count.clone();
        let _listener_id = handle.listen("ui-bridge-response", move |event| {
            let pending = pending_for_listener.clone();
            let pending_count = pending_count_for_listener.clone();

            // Parse the response payload.
            // Tauri 2.x may double-serialize: emit(name, obj) serializes obj to JSON,
            // but event.payload() can return a JSON string that itself contains a
            // JSON-encoded string (e.g., "\"{ ... }\""). Try direct parse first,
            // then unwrap one layer of string quoting if needed.
            let payload_str = event.payload();
            let response: Option<serde_json::Value> =
                serde_json::from_str::<serde_json::Value>(payload_str)
                    .ok()
                    .and_then(|v| {
                        if v.is_object() {
                            Some(v) // Direct parse succeeded as an object — good
                        } else if let Some(s) = v.as_str() {
                            // Payload was double-stringified: outer parse gave us a
                            // JSON string, inner content is the actual JSON object
                            serde_json::from_str::<serde_json::Value>(s).ok()
                        } else {
                            Some(v)
                        }
                    });

            if let Some(response) = response {
                // Spawn a task to handle the response since we need async
                let runtime = tokio::runtime::Handle::try_current();
                if let Ok(rt) = runtime {
                    rt.spawn(async move {
                        crate::mcp::ui_bridge::handle_ui_bridge_response(
                            pending,
                            pending_count,
                            response,
                        )
                        .await;
                    });
                } else {
                    warn!("UI Bridge: No tokio runtime available for response handling");
                }
            } else {
                warn!(
                    "UI Bridge: Failed to parse response payload: {}",
                    truncate_str(&payload_str, 200)
                );
            }
        });
        info!("UI Bridge: Response listener set up");
    }

    // Set up UI Bridge invoke-proxy response listener (Phase 3I.1).
    //
    // Mirrors the `ui-bridge-response` listener above, but for the typed
    // invoke-proxy flow. The React hook (useUIBridgeInvokeHandler) emits
    // `{ request_id, ok, result?, error? }` after calling `invoke(command, args)`;
    // we forward it to the matching pending oneshot by id.
    //
    // Payload tolerance: Tauri 2.x emit+listen may double-stringify JSON
    // payloads — try a direct object parse first, fall back to unwrapping
    // one level of string quoting if the outer payload is a JSON string.
    {
        let invoke_store = api_state.ui_bridge_invoke_store.clone();
        let handle = app_handle.clone();

        use tauri::Listener;

        let _listener_id = handle.listen("ui-bridge:invoke-response", move |event| {
            let payload_str = event.payload();
            let parsed: Option<serde_json::Value> =
                serde_json::from_str::<serde_json::Value>(payload_str)
                    .ok()
                    .and_then(|v| {
                        if v.is_object() {
                            Some(v)
                        } else if let Some(s) = v.as_str() {
                            serde_json::from_str::<serde_json::Value>(s).ok()
                        } else {
                            Some(v)
                        }
                    });

            let Some(parsed) = parsed else {
                warn!(
                    "UI Bridge invoke: failed to parse invoke-response payload: {}",
                    truncate_str(&payload_str, 200)
                );
                return;
            };

            let request_id = parsed
                .get("request_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let Some(request_id) = request_id else {
                warn!(
                    "UI Bridge invoke: invoke-response missing request_id: {}",
                    truncate_str(&payload_str, 200)
                );
                return;
            };

            let ok = parsed.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let result = parsed.get("result").cloned();
            let error = parsed
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let response = crate::ui_bridge_invoke::InvokeResponse { ok, result, error };

            // Deliver on a tokio task — the listener callback runs on the
            // main thread and deliver() needs a tokio context to await the
            // async mutex.
            let store = invoke_store.clone();
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    let delivered = store.deliver(&request_id, response).await;
                    if !delivered {
                        tracing::debug!(
                            "UI Bridge invoke: response for unknown request_id {} (likely timed out)",
                            request_id
                        );
                    }
                });
            } else {
                warn!(
                    "UI Bridge invoke: no tokio runtime available — dropping response for {}",
                    request_id
                );
            }
        });
        info!("UI Bridge: invoke-proxy response listener set up");
    }

    // Set up UI Bridge page/evaluate response listener (Plan item D,
    // post-Phase-3J).
    //
    // Mirrors the `ui-bridge:invoke-response` listener above, but for the
    // typed page/evaluate flow. The React hook (useUIBridgeEvaluateHandler)
    // runs the expression and emits
    // `{ request_id, ok, result?, error? }`; we forward it to the matching
    // pending oneshot by id so concurrent `/page/evaluate` callers never
    // observe each other's results.
    //
    // Payload tolerance: Tauri 2.x emit+listen may double-stringify JSON
    // payloads — try a direct object parse first, fall back to unwrapping
    // one level of string quoting if the outer payload is a JSON string.
    {
        let evaluate_store = api_state.ui_bridge_evaluate_store.clone();
        let handle = app_handle.clone();

        use tauri::Listener;

        let _listener_id = handle.listen("ui-bridge:evaluate-response", move |event| {
            let payload_str = event.payload();
            let parsed: Option<serde_json::Value> =
                serde_json::from_str::<serde_json::Value>(payload_str)
                    .ok()
                    .and_then(|v| {
                        if v.is_object() {
                            Some(v)
                        } else if let Some(s) = v.as_str() {
                            serde_json::from_str::<serde_json::Value>(s).ok()
                        } else {
                            Some(v)
                        }
                    });

            let Some(parsed) = parsed else {
                warn!(
                    "UI Bridge evaluate: failed to parse evaluate-response payload: {}",
                    truncate_str(&payload_str, 200)
                );
                return;
            };

            let request_id = parsed
                .get("request_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let Some(request_id) = request_id else {
                warn!(
                    "UI Bridge evaluate: evaluate-response missing request_id: {}",
                    truncate_str(&payload_str, 200)
                );
                return;
            };

            // The responding window echoes its own label (camelCase, mirroring
            // the request envelope). Absent → "main": both the single-window
            // default and any pre-window-aware frontend register/deliver under
            // "main", so the key matches what the dispatcher stored.
            let window_label = parsed
                .get("windowLabel")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("main")
                .to_string();

            let ok = parsed.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let result = parsed.get("result").cloned();
            let error = parsed
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let response = crate::ui_bridge_evaluate::EvaluateResponse { ok, result, error };

            let store = evaluate_store.clone();
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    let delivered = store.deliver(&window_label, &request_id, response).await;
                    if !delivered {
                        tracing::debug!(
                            "UI Bridge evaluate: response for unknown request_id {} (likely timed out)",
                            request_id
                        );
                    }
                });
            } else {
                warn!(
                    "UI Bridge evaluate: no tokio runtime available — dropping response for {}",
                    request_id
                );
            }
        });
        info!("UI Bridge: page/evaluate response listener set up");
    }

    // Set up `ui-bridge:project-current-scenario-response` listener
    // (Section 11 / Phase B2 — runtime-aware scenario projection IPC).
    //
    // Mirrors the `ui-bridge:invoke-response` listener above and reuses
    // the same `ui_bridge_invoke_store` for routing responses by
    // `request_id` — the response payload shape (`{ ok, result, error }`)
    // is identical, so a separate store would just be duplication.
    {
        let invoke_store = api_state.ui_bridge_invoke_store.clone();
        let handle = app_handle.clone();

        use tauri::Listener;

        let _listener_id =
            handle.listen("ui-bridge:project-current-scenario-response", move |event| {
                let payload_str = event.payload();
                let parsed: Option<serde_json::Value> =
                    serde_json::from_str::<serde_json::Value>(payload_str)
                        .ok()
                        .and_then(|v| {
                            if v.is_object() {
                                Some(v)
                            } else if let Some(s) = v.as_str() {
                                serde_json::from_str::<serde_json::Value>(s).ok()
                            } else {
                                Some(v)
                            }
                        });

                let Some(parsed) = parsed else {
                    warn!(
                        "scenarios/current: failed to parse projection response payload: {}",
                        truncate_str(&payload_str, 200)
                    );
                    return;
                };

                let request_id = parsed
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let Some(request_id) = request_id else {
                    warn!(
                        "scenarios/current: projection response missing request_id: {}",
                        truncate_str(&payload_str, 200)
                    );
                    return;
                };

                let ok = parsed.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                let result = parsed.get("result").cloned();
                let error = parsed
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let response = crate::ui_bridge_invoke::InvokeResponse { ok, result, error };

                let store = invoke_store.clone();
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    rt.spawn(async move {
                        let delivered = store.deliver(&request_id, response).await;
                        if !delivered {
                            tracing::debug!(
                                "scenarios/current: response for unknown request_id {} (likely timed out)",
                                request_id
                            );
                        }
                    });
                } else {
                    warn!(
                        "scenarios/current: no tokio runtime available — dropping response for {}",
                        request_id
                    );
                }
            });
        info!("scenarios/current: projection response listener set up");
    }

    // UI Bridge invoke-proxy wire-contract probe (Phase 1 of
    // optimized-toasting-badger). Dry-runs every allowlisted command with
    // `{}` args on startup and cross-checks Tauri's "missing required key"
    // reply against the declared args_schema. Catches schema drift between
    // the hand-written schema string and the actual Rust command signature
    // (the exact bug fixed in commit 01ba1085b). Purely diagnostic —
    // logs-only, never fails boot.
    {
        let probe_handle = app_handle.clone();
        let probe_store = api_state.ui_bridge_invoke_store.clone();
        tokio::spawn(async move {
            // Give the React side a moment to mount the invoke-response
            // listener before we start firing probes at it.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let results = crate::ui_bridge_invoke_probe::probe_allowlist_wire_contracts(
                &probe_handle,
                probe_store,
            )
            .await;
            crate::ui_bridge_invoke_probe::log_probe_results(&results);
            tracing::debug!(
                probe_count = results.len(),
                "ui_bridge_invoke_probe: startup probe completed"
            );
        });
    }

    // Set up UI Bridge pong listener for frontend liveness tracking
    {
        let last_pong = api_state.app_state.ui_bridge_last_pong.clone();
        let ready = api_state.ui_bridge_ready.clone();
        let handle = app_handle.clone();

        use tauri::Listener;

        handle.listen("ui-bridge-pong", move |_event| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            last_pong.store(now, std::sync::atomic::Ordering::Relaxed);
            // Unblock any requests waiting for frontend readiness
            ready.notify_waiters();
        });
    }

    // Start UI Bridge ping task (every 3s)
    {
        let handle = app_handle.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                interval.tick().await;
                let _ = handle.emit(
                    "ui-bridge-ping",
                    serde_json::json!({ "timestamp": chrono::Utc::now().timestamp_millis() }),
                );
            }
        });
    }

    // Resume interrupted unified workflows on startup
    let state_for_resume = api_state.clone();
    let resume_config_storage = api_state.config_storage.clone();
    let resume_pid_tracker = api_state.current_ai_pids.clone();
    tokio::spawn(async move {
        // Small delay to let the server fully start
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Note: We no longer use global_auto_continue here.
        // Each workflow's per-task auto_continue setting determines whether it gets resumed.
        // The global setting is now only used for the UI toggle, not startup resume logic.

        // Log to debug file
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(crate::paths::get_workflow_debug_log_path())
        {
            use std::io::Write;
            let _ = writeln!(
                f,
                "[{}] STARTUP_RESUME_CHECK: Processing interrupted workflows (per-task auto_continue)",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
            );
        }

        // Process interrupted workflows - each workflow's per-task auto_continue setting
        // determines whether it gets resumed or marked as failed
        let resume_config = crate::unified_workflow_executor::ResumeConfig {
            resume_enabled: true, // Let the function check per-task auto_continue
        };

        let count = crate::unified_workflow_executor::resume_interrupted_workflows(
            state_for_resume.app_state.clone(),
            resume_config_storage,
            state_for_resume.app_handle.clone(),
            resume_pid_tracker,
            resume_config,
        )
        .await;

        if count > 0 {
            info!(
                "Processed {} interrupted unified workflow(s) on startup",
                count
            );
        }
    });

    // Resume interrupted chat sessions on startup
    {
        let chat_handle = app_handle.clone();
        // Access session manager from Tauri state (managed separately from AppState)
        let chat_sm: Arc<crate::claude_session::SessionManager> = app_handle
            .state::<Arc<crate::claude_session::SessionManager>>()
            .inner()
            .clone();
        tokio::spawn(async move {
            // Wait a bit longer than unified workflows to let the server fully start
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            // Phase 4 — crash recovery vs planned restart. The classification
            // is captured EXACTLY ONCE per process by `classify_boot` (which
            // reads the prior marker, stashes the result, and re-marks the
            // marker `clean:false` for the NOW-running process so a crash of
            // THIS process is detected next boot; the clean drain / exit seam
            // flips it back to `clean:true`). `main.rs` setup performs the
            // classification synchronously at boot; this call is idempotent
            // and simply reads the stash (OnceLock) — re-reading the marker
            // file here would ALWAYS classify "crash" post-`mark_running`.
            let marker_path =
                crate::session::shutdown_marker::marker_path(crate::mcp::types::get_mcp_api_port());
            let crash_recovery =
                crate::session::shutdown_marker::classify_boot(&marker_path).crash_recovery;
            if crash_recovery {
                warn!("Startup recovery: previous shutdown was NOT clean — this is crash recovery");
            }

            let summary = crate::commands::ai_session::resume_ai_sessions(
                chat_sm,
                chat_handle.clone(),
                crash_recovery,
            )
            .await;

            if summary.resumed_count > 0 {
                info!(
                    "Resumed {} AI session(s) on startup (crash_recovery={})",
                    summary.resumed_count, summary.crash_recovery
                );
            }

            // Emit the structured startup-recovery summary ONCE so the
            // frontend can surface a (prominent on crash / quiet on planned)
            // banner with honest per-session fidelity + claim + WIP status.
            // Always emit — even with zero sessions — so a late-mounting
            // banner that requested a replay can settle; the frontend hides
            // itself when there's nothing to show.
            crate::commands::ai_session::emit_session_recovery_summary(&chat_handle, &summary);
        });
    }

    // Auto-start cloud relay if configured
    {
        let relay_api_state = api_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            crate::mcp::backend_relay::commands::auto_start_cloud_relay(relay_api_state).await;
        });
    }

    // Auto-start device-JWT refresher (Phase 2 of unified-devices migration).
    // The refresher is tier-aware: it idles unless RunnerTier::QontinuiAccount,
    // so spawning unconditionally on Tier 0/1 just wastes a watch channel — no
    // network calls happen until the user signs into Qontinui.
    {
        let refresher_api_state = api_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            crate::mcp::device_jwt_refresher::commands::auto_start_device_jwt_refresher(
                refresher_api_state,
            )
            .await;
        });
    }

    // Auto-start the fleet-policy poller (P3 of the fleet-policy channel
    // redesign). Device-scoped: it polls coord's effective
    // `install_interception` level every ~45s and caches it for the
    // interception pre-call to make the per-install mode dynamic (P4). It
    // no-ops quietly while unpaired (no device JWT) and fails safe to `off`
    // before its first success, so spawning unconditionally is harmless.
    {
        let poller_api_state = api_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            crate::mcp::fleet_policy_poller::commands::auto_start_fleet_policy_poller(
                poller_api_state,
            )
            .await;
        });
    }

    // Auto-start the in-session continuation delivery poller (Phase 2 of
    // `2026-06-21-in-session-continuation-delivery.md`). Device-scoped: it
    // consumes coord's directed-message mailbox and injects each message as a
    // prompt into the live LOCAL session it targets — SDK sessions queue safely
    // mid-turn; PTY sessions are gated on terminal idle so a running turn is
    // never clobbered. It no-ops while unpaired (no device JWT) and parks under
    // the `RUNNER_SESSION_MESSAGE_DELIVERY_DISABLED` kill-switch, so spawning
    // unconditionally is harmless. Supersedes the retired `session_bus`
    // executor (its spawn is removed in main.rs) so only ONE consumer races the
    // mailbox.
    {
        let poller_api_state = api_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            crate::mcp::session_message_poller::commands::auto_start_session_message_poller(
                poller_api_state,
            )
            .await;
        });
    }

    // Helper Task Queue (plan 2026-06-29, Phase 1.3 Part D) — poll collected
    // helper answers from coord into the persisted store that feeds the
    // reflection-context section and the Helper Tasks Review tab. The tick
    // runs when settings.helper_tasks.emit_enabled is on OR the store already
    // has tasks/answers (pausing emission must not pause collection), and is
    // device-JWT-gated — spawning unconditionally adds no always-on traffic
    // for an opted-out runner with an empty store.
    {
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            crate::helper_tasks::poller::run_forever().await;
        });
    }

    // Restore persisted coord-mcp proxy nonces + reconcile session configs to
    // the current bound port (plan 2026-06-13 Phases 3b + 3c, plan 2026-07-07
    // Change 1). 3b makes an already-written `.mcp.json` keep validating across a
    // restart (the nonce is a loopback-only key, persisted at rest under
    // COORD_MCP_PERSIST_NONCES); 3c rewrites any live session whose proxy URL
    // names a stale port back to the instance's current bound port. Ordered
    // AFTER the restore so a rewrite reuses the restored map where possible, and
    // guarded by `coord_mcp_safe_to_write` so it never clobbers an agent-spawn
    // config. Root self-heal (`reconcile_root_config`) runs UNCONDITIONALLY —
    // NOT gated on session presence — so a boot with zero open sessions still
    // repairs a stale-port root config (plan 2026-07-07 Change 1 secondary gap),
    // and adopts (rather than rewrites) the on-disk nonce on a same-port restart
    // so a live MCP client's cached nonce keeps validating (Change 1 core fix).
    {
        let reconcile_app_handle = app_handle.clone();
        let reconcile_bound_port = api_state
            .app_state
            .api_port
            .load(std::sync::atomic::Ordering::Relaxed);
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            // Phase 3b — restore the persisted nonce map FIRST (run-once). The
            // restored count is half of the Change-2 observability signal: if it
            // is 0 and root self-heal then has to Rewrite, a silent nonce
            // rotation just happened (the exact incident this plan fixes).
            let restored = crate::coord_mcp::restore_proxy_nonces_from_store();

            // Credential-hygiene Task 5 — reap stale app-data session-restore
            // coord-mcp configs (dead port, or our port with an
            // unregistered/expired nonce). AFTER the restore so the registered
            // set is authoritative; on a blocking thread because it does TCP
            // liveness probes + file I/O.
            let reaped = tokio::task::spawn_blocking(move || {
                crate::coord_mcp::reap_stale_session_restore_configs(reconcile_bound_port)
            })
            .await
            .unwrap_or(0);

            // Phase 3c — reconcile live session `.mcp.json` ports. Pull the live
            // session workdirs from the managed lifecycle store (open records).
            use tauri::Manager;
            let workdirs: Vec<String> = match reconcile_app_handle
                .try_state::<std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>(
                ) {
                Some(store) => store
                    .inner()
                    .open_records()
                    .into_iter()
                    .filter_map(|r| r.working_dir)
                    .collect(),
                None => Vec::new(),
            };
            let session_rewritten = if workdirs.is_empty() {
                0
            } else {
                crate::coord_mcp::reconcile_session_configs(workdirs, reconcile_bound_port)
            };

            // Plan 2026-07-07 Change 1 secondary gap: root self-heal ALWAYS runs,
            // independent of session presence. Change 1 core fix: on a same-port
            // restart this ADOPTS the on-disk nonce (no file rewrite) instead of
            // minting a fresh one that would strand a live client's cached nonce.
            let root_action = crate::coord_mcp::reconcile_root_config(reconcile_bound_port);

            // Change 2 observability: one structured summary of restore vs
            // self-heal so a future silent rotation (restored=0 → root Rewrite) is
            // greppable. A root `Rewrite` after `restored == 0` is the smell.
            //
            // The summary NAMES THE INSTANCE: `SkippedSecondary` is expected and
            // routine on a temp/named runner, but an operator debugging a stale
            // root `.mcp.json` needs to see *which* instance last declined to fix
            // it — otherwise the skip reads as the repair silently not running.
            // A NAMELESS secondary must not be labelled "primary" here — that is
            // the exact fail-open `owns_shared_root_state` exists to catch, and
            // mislabelling it in the log would hide it from the operator too.
            let reconcile_instance = crate::instance::instance_name().unwrap_or_else(|| {
                if crate::instance::owns_shared_root_state() {
                    "primary".to_string()
                } else {
                    "unnamed-secondary".to_string()
                }
            });
            info!(
                "coord_mcp boot reconcile: restored {restored} persisted nonce(s), \
                 reaped {reaped} stale session-restore config(s), \
                 rewrote {session_rewritten} session config(s), root self-heal = {root_action:?} \
                 (instance {reconcile_instance}, bound port :{reconcile_bound_port})"
            );
            if matches!(root_action, crate::coord_mcp::RootReconcileAction::Rewrite)
                && restored == 0
            {
                warn!(
                    "coord_mcp: root .mcp.json was REWRITTEN with a fresh nonce after restoring 0 \
                     persisted nonces — a live MCP client that cached the prior nonce will 401 \
                     until it reconnects (`/mcp` reconnect). Investigate why the persisted nonce \
                     did not restore (COORD_MCP_PERSIST_NONCES, secure storage, or snapshot gap)."
                );
            }
        });
    }

    // Sync workflows from web backend on startup (background task)
    {
        let sync_pg_db = api_state.app_state.pg_db.clone();
        tokio::spawn(async move {
            // Wait for server to start and auth to be available
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            match crate::mcp::web_backend_workflows::sync_workflows_from_backend(&sync_pg_db).await
            {
                Ok(count) => {
                    if count > 0 {
                        info!("Synced {} workflows from web backend", count);
                    }
                }
                Err(e) => {
                    warn!("Workflow sync from backend skipped: {}", e);
                }
            }
        });
    }

    // Start zombie task run sweep (detects and cleans up stale "running" tasks)
    {
        let sweep_handle = app_handle.clone();
        let sweep_sm: Arc<crate::claude_session::SessionManager> = app_handle
            .state::<Arc<crate::claude_session::SessionManager>>()
            .inner()
            .clone();
        let sweep_pg = api_state.app_state.pg_db.clone();
        let sweep_port = api_state
            .app_state
            .api_port
            .load(std::sync::atomic::Ordering::Relaxed);
        crate::zombie_sweep::start_zombie_sweep(sweep_pg, sweep_sm, sweep_handle, sweep_port);
    }

    // Periodic file registry cleanup (sweep stale entries every 60s)
    {
        let cleanup_registry = api_state.app_state.file_registry_manager.clone();
        let cleanup_db = api_state.app_state.pg_db.clone();
        tokio::spawn(async move {
            // Wait for server to fully start
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            loop {
                cleanup_registry.cleanup_stale(&cleanup_db).await;
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            }
        });
    }

    // One-time audit event cleanup based on audit_retention_days setting
    {
        let pg_db = api_state.app_state.pg_db.clone();
        tokio::spawn(async move {
            // Wait for server startup to settle
            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
            let settings = crate::settings::get_security_settings();
            if settings.audit_retention_days > 0 {
                match pg_db
                    .cleanup_old_audit_events(settings.audit_retention_days)
                    .await
                {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!(
                            "Cleaned up {} old audit events (retention: {} days)",
                            n,
                            settings.audit_retention_days
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Audit event cleanup failed: {}", e);
                    }
                }
            }
        });
    }

    // Start trigger service (event-driven workflow automation)
    {
        let trigger_app_state = api_state.app_state.clone();
        let trigger_config_storage = api_state.config_storage.clone();
        let trigger_handle = app_handle.clone();
        let trigger_pids = api_state.current_ai_pids.clone();
        tokio::spawn(async move {
            // Wait for server to be ready
            tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
            crate::trigger_system::start_trigger_service(
                trigger_app_state,
                trigger_config_storage,
                trigger_handle,
                trigger_pids,
            )
            .await;
        });
    }

    // Productivity Coordinator (Rust) scheduler — Phase 1b of
    // `productivity-coordinator-rust-promotion.md`, with Phase 1.5
    // (`productivity-stack-product-readiness.md`) runtime toggle.
    //
    // The scheduler ALWAYS starts at boot now. Whether it does work each
    // tick is governed by an Arc<AtomicBool> flag wrapped in
    // `CoordinatorSchedulerHandle`. The flag's initial value still comes
    // from the env var (`QONTINUI_COORDINATOR_RUST_SCHEDULER`) for first
    // boot, but the `launch_coordinator_session` /
    // `stop_coordinator_session` Tauri commands flip it at runtime via
    // the handle stashed in Tauri state — no process restart needed.
    {
        let coord_config = crate::coordinator::config::CoordinatorSchedulerConfig::from_env();
        tracing::info!(
            "Coordinator (Rust) scheduler starting: initial_enabled={} interval={}s",
            coord_config.rust_scheduler_enabled,
            coord_config.interval_secs,
        );
        let scheduler_handle = crate::coordinator::scheduler::start_coordinator_scheduler(
            api_state.clone(),
            coord_config,
        );
        // Stash the runtime-toggle handle so launch_coordinator_session
        // can flip the flag without a process restart.
        app_handle.manage(scheduler_handle);
    }

    // Physical device USB scanner (30-second interval)
    // Discovers ADB-attached Android devices and registers them with PhysicalDeviceRegistry.
    {
        let state = api_state.clone();
        tokio::spawn(async move {
            // Wait for ADB and other services to initialize
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            let mobile_settings = crate::settings::get_mobile_settings();
            let usb_transport = crate::mcp::transport::usb::UsbTransport::new();

            // Publish for the CloseRequested shutdown handler in main.rs, which
            // calls release_all so this process's `adb forward` entries don't
            // linger. Graceful-only — supervisor force-kill (taskkill /F) still
            // leaks. See plan adb-forwarder-port.md §1.6a.
            if state
                .app_state
                .usb_transport
                .set(usb_transport.clone())
                .is_err()
            {
                tracing::warn!("UsbTransport OnceCell already populated; scanner task restarted?");
            }

            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));

            loop {
                interval.tick().await;

                let devices = usb_transport.scan_devices().await;
                let registry = &state.physical_device_registry;

                // Register newly connected physical devices.
                //
                // Re-establishment after transport eviction: the dedup below
                // skips a device only when it already has a live USB transport
                // entry in the registry. If the health monitor removed the
                // USB transport (3 consecutive failures → `remove_transport`
                // leaves the device entry with `transports: []` and
                // `health_state: Unreachable`), the next scanner pass must
                // retry `establish_forward` — otherwise the device is
                // permanently dead in the registry while still being a live
                // adb target. Previously the dedup checked `get_device(...).is_some()`,
                // which also matched the empty-transport "stuck" state and
                // produced `transports: []` + `healthState: unreachable`
                // responses on /ui-bridge/devices even though raw probes to
                // localhost:<ui_bridge_port> succeeded.
                for (device_id, status, model) in &devices {
                    if status != "device" {
                        continue;
                    }
                    if device_id.starts_with("emulator-") {
                        continue; // Skip emulators — handled by app_discovery
                    }

                    // Reverse the runner HTTP API port back to the device so the
                    // qontinui-mobile app's data path can reach the runner at
                    // `localhost:<port>` on the phone. Without this, USB-attached
                    // phones see "Network request failed". This is independent of
                    // UI Bridge discovery below — the data path is needed whenever
                    // a physical device is attached, registered or not — so it runs
                    // before the already-registered skip. Idempotent + only logged
                    // on first install per serial (re-runs are skipped via the
                    // active_reverses map).
                    if usb_transport
                        .active_reverses
                        .lock()
                        .await
                        .get(device_id)
                        .is_none()
                    {
                        let runner_port = state.app_state.api_port.load(Ordering::Relaxed);
                        if let Err(e) = usb_transport
                            .establish_reverse(device_id, runner_port)
                            .await
                        {
                            tracing::debug!(
                                "Failed to establish ADB reverse for {} (port {}): {}",
                                device_id,
                                runner_port,
                                e
                            );
                        }
                    }

                    // Skip devices that already have a live USB transport.
                    // A device entry with empty `transports` (or only
                    // non-USB transports) falls through to re-establishment.
                    if registry
                        .has_transport_kind(device_id, crate::mcp::transport::TransportKind::Usb)
                        .await
                    {
                        continue;
                    }

                    // Attempt ADB port forward to the UI Bridge port
                    match usb_transport
                        .establish_forward(device_id, mobile_settings.ui_bridge_port)
                        .await
                    {
                        Ok(local_port) => {
                            // Quick health check
                            let client = reqwest::Client::builder()
                                .timeout(std::time::Duration::from_secs(2))
                                .build()
                                .unwrap_or_else(|_| reqwest::Client::new());
                            let url = format!("http://127.0.0.1:{}/ui-bridge/health", local_port);
                            if client.get(&url).send().await.is_ok() {
                                let now = chrono::Utc::now().timestamp_millis();
                                let info = crate::mcp::physical_device::PhysicalDeviceInfo {
                                    id: device_id.clone(),
                                    os: crate::mcp::transport::DeviceOs::Android,
                                    device_kind: "physical".to_string(),
                                    model: model.clone(),
                                    app_id: None,
                                    ui_bridge_version: None,
                                    first_seen_at: now,
                                    pairing_token: None,
                                };
                                let transport = crate::mcp::physical_device::ActiveTransport {
                                    kind: crate::mcp::transport::TransportKind::Usb,
                                    proxy_url: format!("http://127.0.0.1:{}", local_port),
                                    established_at: now,
                                    last_healthy_at: now,
                                    fail_count: 0,
                                };
                                registry.register(info, transport).await;
                                tracing::info!(
                                    "USB device registered: {} (local port {})",
                                    device_id,
                                    local_port
                                );
                            } else {
                                // No UI Bridge on device — release the forward
                                let _ = usb_transport.release_forward(device_id).await;
                                tracing::debug!(
                                    "USB device {} reachable via ADB but no UI Bridge (port {})",
                                    device_id,
                                    local_port
                                );
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                "Failed to establish ADB forward for {}: {}",
                                device_id,
                                e
                            );
                        }
                    }
                }

                // Clean up devices that are no longer connected via USB
                let registered = registry.list_all().await;
                for device in registered {
                    let has_usb = device
                        .transports
                        .iter()
                        .any(|t| t.kind == crate::mcp::transport::TransportKind::Usb);
                    if !has_usb {
                        continue;
                    }
                    let still_connected = devices
                        .iter()
                        .any(|(id, status, _)| id == &device.info.id && status == "device");
                    if !still_connected {
                        registry
                            .remove_transport(
                                &device.info.id,
                                crate::mcp::transport::TransportKind::Usb,
                            )
                            .await;
                        let _ = usb_transport.release_forward(&device.info.id).await;
                        tracing::info!("USB device disconnected: {}", device.info.id);
                    }
                }

                // Tear down ADB reverses for devices no longer attached. Reverses
                // are installed for every physical device (even those without a UI
                // Bridge, which never register in the registry above), so they need
                // their own disconnect sweep keyed off the live `adb devices` list.
                let reversed_serials: Vec<String> = {
                    let reverses = usb_transport.active_reverses.lock().await;
                    reverses.keys().cloned().collect()
                };
                for serial in reversed_serials {
                    let still_connected = devices
                        .iter()
                        .any(|(id, status, _)| id == &serial && status == "device");
                    if !still_connected {
                        let _ = usb_transport.release_reverse(&serial).await;
                        tracing::info!("USB device reverse released (disconnected): {}", serial);
                    }
                }
            }
        });
    }

    // Physical device health monitor (15-second interval)
    // Checks liveness of registered devices and removes unhealthy transports.
    {
        let registry = api_state.physical_device_registry.clone();
        tokio::spawn(async move {
            // Wait for USB scanner to do its first pass
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());

            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            // Intentionally leaked — monitor runs for the entire app lifetime
            std::mem::forget(shutdown_tx);

            registry.start_health_monitor(client, shutdown_rx);
        });
    }

    // Cloud device registry poller (if cloud device bridge is enabled)
    // Polls the qontinui.io backend for remotely registered devices.
    {
        let state = api_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;

            let cloud_settings =
                crate::config_facade::get_setting::<crate::settings::CloudRelaySettings>();
            if !cloud_settings.device_bridge_enabled {
                tracing::debug!("Cloud device bridge disabled, skipping registry poller");
                return;
            }

            let poller = crate::mcp::discovery::cloud_registry::CloudRegistryPoller::new(
                cloud_settings.backend_url.clone(),
                cloud_settings.cloud_registry_poll_secs,
            );
            // Use localhost for the tunnel WS connection — runner is co-located
            // with the backend. The tunnel URL (Cloudflare) may reject WS upgrades.
            let cloud_transport =
                std::sync::Arc::new(crate::mcp::transport::cloud::CloudTransport::new(
                    crate::api_config::get_api_base_url(),
                ));

            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                cloud_settings.cloud_registry_poll_secs,
            ));

            loop {
                interval.tick().await;

                // Refresh auth token on each poll cycle
                let token = crate::auth::AuthManager::new().get_access_token().ok();
                if let Some(token) = token {
                    match poller.poll(&token).await {
                        Ok(devices) => {
                            let registry = &state.physical_device_registry;
                            for device in devices {
                                tracing::debug!(
                                    "Cloud device available: {} ({})",
                                    device.device_id,
                                    device.display_name
                                );
                                // Skip if already registered (any transport)
                                if registry.get_device(&device.device_id).await.is_some() {
                                    continue;
                                }
                                // Open a cloud tunnel for this device so the registry
                                // has a working proxy URL. The tunnel stays open for
                                // the device's lifetime in the registry.
                                match cloud_transport.open_tunnel(&device.device_id, &token).await {
                                    Ok(local_port) => {
                                        let os = match device.platform.to_lowercase().as_str() {
                                            "ios" => crate::mcp::transport::DeviceOs::Ios,
                                            _ => crate::mcp::transport::DeviceOs::Android,
                                        };
                                        let now = chrono::Utc::now().timestamp_millis();
                                        let info =
                                            crate::mcp::physical_device::PhysicalDeviceInfo {
                                                id: device.device_id.clone(),
                                                os,
                                                device_kind: "physical".to_string(),
                                                model: None,
                                                app_id: if device.app_id.is_empty() {
                                                    None
                                                } else {
                                                    Some(device.app_id.clone())
                                                },
                                                ui_bridge_version: None,
                                                first_seen_at: now,
                                                pairing_token: None,
                                            };
                                        let transport =
                                            crate::mcp::physical_device::ActiveTransport {
                                                kind: crate::mcp::transport::TransportKind::Cloud,
                                                proxy_url: format!(
                                                    "http://127.0.0.1:{}",
                                                    local_port
                                                ),
                                                established_at: now,
                                                last_healthy_at: now,
                                                fail_count: 0,
                                            };
                                        registry.register(info, transport).await;
                                        tracing::info!(
                                            "Cloud device registered: {} via tunnel on port {}",
                                            device.device_id,
                                            local_port
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Failed to open cloud tunnel for {}: {}",
                                            device.device_id,
                                            e
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!("Cloud registry poll failed: {}", e);
                        }
                    }
                }
            }
        });
    }

    // mDNS LAN discovery (if enabled)
    // Discovers UI Bridge instances advertising themselves on the local network.
    {
        let state = api_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;

            let mobile_settings = crate::settings::get_mobile_settings();
            if !mobile_settings.lan_discovery_enabled {
                tracing::debug!("LAN discovery disabled, skipping mDNS scanner");
                return;
            }

            let (event_tx, mut event_rx) =
                tokio::sync::mpsc::channel::<crate::mcp::discovery::mdns_scanner::MdnsEvent>(32);
            let scanner = crate::mcp::discovery::mdns_scanner::MdnsScanner::new();
            scanner.start(event_tx);

            // Keep the scanner alive for the duration of the task
            let _scanner = scanner;

            // Process mDNS discovery events
            while let Some(event) = event_rx.recv().await {
                match event {
                    crate::mcp::discovery::mdns_scanner::MdnsEvent::Discovered(info) => {
                        if let Some(&addr) = info.addresses.first() {
                            let socket_addr = std::net::SocketAddr::new(addr, info.port);
                            tracing::info!(
                                "mDNS device found: {} at {}",
                                info.device_id,
                                socket_addr
                            );

                            // Register as a LAN transport in the physical device registry.
                            // We proxy directly to the device's IP/port; no local TCP proxy
                            // is needed for same-LAN connections.
                            let now = chrono::Utc::now().timestamp_millis();
                            // Determine OS from the "platform" TXT record if present
                            let os = match info
                                .txt_records
                                .get("platform")
                                .map(|s| s.to_lowercase())
                                .as_deref()
                            {
                                Some("ios") | Some("iphone") | Some("ipad") => {
                                    crate::mcp::transport::DeviceOs::Ios
                                }
                                _ => crate::mcp::transport::DeviceOs::Android,
                            };
                            let device_info = crate::mcp::physical_device::PhysicalDeviceInfo {
                                id: info.device_id.clone(),
                                os,
                                device_kind: "physical".to_string(),
                                model: info.txt_records.get("model").cloned(),
                                app_id: info.txt_records.get("app_id").cloned(),
                                ui_bridge_version: info.txt_records.get("version").cloned(),
                                first_seen_at: now,
                                pairing_token: info.txt_records.get("pairing_token").cloned(),
                            };
                            let transport = crate::mcp::physical_device::ActiveTransport {
                                kind: crate::mcp::transport::TransportKind::Lan,
                                proxy_url: format!("http://{}", socket_addr),
                                established_at: now,
                                last_healthy_at: now,
                                fail_count: 0,
                            };
                            state
                                .physical_device_registry
                                .register(device_info, transport)
                                .await;
                        }
                    }
                    crate::mcp::discovery::mdns_scanner::MdnsEvent::Removed(name) => {
                        tracing::debug!("mDNS device removed: {}", name);
                        // The health monitor will evict the transport when health
                        // checks start failing; no immediate action needed here.
                    }
                }
            }
        });
    }

    // Start cascade event buffer (collects cascade detection events for /cascade/events)
    crate::mcp::cascade::start_buffer_task(&api_state);

    // Build GraphQL schema with ApiState as context data
    let graphql_schema = crate::graphql::build_schema(api_state.clone());

    // CORS: Permissive (allow any origin) is intentional.
    // This localhost-only API (port 9876) must be accessible from:
    //   - The Tauri webview (tauri://localhost origin)
    //   - External MCP clients (Claude Desktop, Cursor, etc.)
    //   - WSL environments
    // Adding origin restrictions would break MCP client compatibility.
    // Security is enforced by binding to localhost, not by CORS.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // GraphQL sub-router with concurrency limit (max 20 concurrent GraphQL requests).
    // This prevents a burst of expensive queries from starving REST endpoints.
    // WebSocket subscriptions are excluded — they're long-lived by design.
    let graphql_routes = Router::new()
        .route(
            "/graphql",
            get(crate::graphql::schema::graphiql_handler)
                .post(crate::graphql::schema::graphql_handler),
        )
        .layer(tower::limit::ConcurrencyLimitLayer::new(20));

    let base_router = Router::new()
        // GraphQL endpoints (typed API alongside REST)
        .merge(graphql_routes)
        .route_service(
            "/graphql/ws",
            GraphQLSubscription::new(graphql_schema.clone()),
        )
        // Local routes
        .route("/health", get(health))
        .route("/ui-bridge/health", get(health))
        .route("/ui-bridge/status", get(health))
        // Phase 2 — graceful drain on planned restart. The supervisor POSTs
        // here before its hard taskkill so in-flight turns flush, dirty
        // worktrees are stashed to refs/wip/*, and coord claims persist.
        .route("/drain", post(drain_handler))
        // Manually trigger the dead-webview recovery ladder (plan
        // 2026-08-01-runner-dead-webview-is-invisible-to-health, Phase 2). An
        // operator/debug affordance ONLY — the shipped detection path is the
        // push `ProcessFailed` event plus the heartbeat backstop. It exists
        // because nothing on this API could reach the webview before:
        // `ui_bridge_reload_webview` is a `#[tauri::command]` that is absent
        // from the invoke allowlist, so /reload, /ui/reload, /ui-bridge/reload
        // and /api/reload all 404.
        .route(
            "/ui/recover",
            post(crate::webview_recovery::recover_ui_handler),
        )
        // Loopback live-token proxy for coord /mcp — device-provisioned
        // sessions' `.mcp.json` points here so each MCP request carries a
        // freshly-read device JWT instead of a 4h-TTL snapshot. Nonce-gated
        // (X-Coord-Mcp-Proxy-Key); see `coord_mcp_proxy_handler`.
        .route("/coord-mcp", post(coord_mcp_proxy_handler))
        // Session coord-identity MINT route (plan
        // 2026-07-17-universal-coord-device-identity-for-any-session §1) — how a
        // session the runner did NOT spawn (a bare terminal, a cron-fired agent)
        // obtains a device-scoped coord-mcp config at launch.
        //
        // ⚠ THE ONE ROUTE IN THIS FAMILY THAT IS NOT NONCE-GATED, and it cannot
        // be: it is what ISSUES the nonce. It carries the master flag
        // (`QONTINUI_SESSION_COORD_IDENTITY_ENABLED`, default OFF) + a
        // per-machine operator opt-in marker IN PLACE OF the nonce check. Read
        // `coord_provision_session_handler`'s doc before touching this.
        .route(
            "/coord-mcp/provision-session",
            post(coord_provision_session_handler),
        )
        // Nonce-gated claims READ passthrough for device sessions' hook
        // helper + skill wait-poll (plan 2026-06-11-claims-read-auth-hardening
        // Phase 2). Same gate + live device-JWT injection as /coord-mcp, but
        // allowlisted to EXACTLY the read-only coord routes enumerated in
        // `ClaimsReadTarget` — never a generic path passthrough. (The
        // work-unit deps read rides the same passthrough; it is registered
        // below with the work-unit forward-list.)
        .route("/coord-mcp/claims/list", get(coord_claims_list_handler))
        .route(
            "/coord-mcp/claims/by-resource",
            get(coord_claims_by_resource_handler),
        )
        // Nonce-gated device-JWT WRITE forwarder for device sessions
        // (plan 2026-06-15-coord-mcp-live-token-write-forwarder Phase 1).
        // Same gate + live device-JWT injection as /coord-mcp, but allowlisted
        // to EXACTLY these enumerated device-authed coord write routes with a
        // validated dynamic segment — never a generic path passthrough (see
        // `CoordWriteTarget`). NOTE: there is no `gates/register-plan/{slug}`
        // route — coord deleted its `/coord/plans/{slug}/register-gate` upstream
        // (coord P4 Phase 3), so the forwarder was removed too rather than left
        // as a guaranteed-404 shim; use the work-unit register-gate route below.
        // Claim-anchored register (plan 2026-07-21-gate-cascade-step3-proxy-rebase
        // Phase 1b): forwards to coord's device-authed
        // `POST /coord/gates/register-agent` — the REST twin of MCP
        // `coord_register_gate`. Claim anchor travels in the body; no dynamic
        // segment.
        .route(
            "/coord-mcp/gates/register",
            post(coord_register_gate_handler),
        )
        .route(
            "/coord-mcp/gates/{gate_id}/attest",
            post(coord_attest_gate_handler),
        )
        // Work-unit registry forward-list (device-session coord surface
        // hardening follow-up): coord serves all four under its device-JWT
        // `work_units_agent_authed` sub-router (`require_jwt`), so a device
        // bearer authenticates. The bare work-unit read
        // (`GET /coord/work-units/{slug}`) is deliberately NOT forwarded —
        // it is operator/`TenantId`-only on coord (403 under a device JWT).
        .route(
            "/coord-mcp/work-units/upsert",
            post(coord_work_unit_upsert_handler),
        )
        .route(
            "/coord-mcp/work-units/{slug}/transition",
            post(coord_work_unit_transition_handler),
        )
        .route(
            "/coord-mcp/work-units/{slug}/register-gate",
            post(coord_work_unit_register_gate_handler),
        )
        .route(
            "/coord-mcp/work-units/{slug}/deps",
            get(coord_work_unit_deps_get_handler).post(coord_work_unit_set_deps_handler),
        )
        // Nonce-gated PR-creation forwarder (plan
        // qontinui-pr-credential-provisioning, Phase 2b). Same gate + live
        // per-principal JWT injection (device OR agent) as the coord-mcp
        // proxy, fixed to coord's
        // `POST /coord/repos/{owner}/{repo}/pull-requests` route with a
        // validated owner/name — never a generic passthrough. The
        // `qontinui-pr create` session CLI is the intended caller.
        .route("/vcs/pull-requests", post(vcs_create_pull_request_handler))
        // Relay web-integration diagnostic — exposes the idle-gating state
        // (tier, enabled, device-JWT presence, WS connection, last error) so
        // an operator can see WHY a runner never appears to the cloud/mobile.
        .merge(crate::mcp::backend_relay::routes())
        // AWAS routes (imported directly)
        .route("/awas/discover", post(awas_discover))
        .route("/awas/execute", post(awas_execute))
        .route("/awas/check-support", post(awas_check_support))
        .route("/awas/actions", get(awas_list_actions))
        .route("/awas/extract-elements", post(awas_extract_elements))
        // Module routes
        .merge(crate::mcp::accessibility::routes())
        .merge(crate::mcp::canvas::routes())
        .merge(crate::mcp::cascade::routes())
        .merge(crate::mcp::ai_generation::routes())
        .merge(crate::mcp::ai_network_probe::routes())
        .merge(crate::mcp::ai_wait_for::routes())
        .merge(crate::mcp::api_requests::routes())
        .merge(crate::mcp::app_discovery::routes())
        .merge(crate::mcp::ws_relay::routes())
        .merge(crate::wrappers::routes::router())
        .merge(crate::mcp::physical_device_api::routes())
        .merge(crate::mcp::plans::routes())
        .merge(crate::mcp::coordinator::routes())
        .merge(crate::mcp::subagent_api::routes())
        .merge(crate::mcp::completion_reports::routes())
        // Approach-D Conductor/Engine Phase 2 §3 — the `orchestration_report_subtask`
        // MCP tool (POST /orchestration/report-subtask). Mounted alongside the
        // other completion-report route modules.
        .merge(crate::mcp::orchestration_report::routes())
        // Approach-D Conductor/Engine Phase 3 — orchestration-run control
        // surface (start/list/status/stop). Mirrors orchestration_loop_api.
        .merge(crate::mcp::orchestration_run_api::routes())
        .merge(crate::mcp::completion_sources::routes())
        .merge(crate::mcp::reflection::routes())
        .merge(crate::mcp::sessions::routes())
        .merge(crate::mcp::tunnel_api::routes())
        .merge(crate::mcp::automation_runs::routes())
        .merge(crate::mcp::comparison_api::routes())
        .merge(crate::mcp::checks::routes())
        .merge(crate::mcp::code_semantics::routes())
        .merge(crate::mcp::configs::routes())
        .merge(crate::mcp::constraints_api::routes())
        .merge(crate::mcp::contexts::routes())
        .merge(crate::mcp::development_intelligence::routes())
        .merge(crate::mcp::dom_capture::routes())
        .merge(crate::mcp::error_monitor::routes())
        .merge(crate::mcp::extraction::routes())
        .merge(crate::mcp::file_browser::routes())
        .merge(crate::mcp::file_registry::routes())
        .merge(crate::mcp::findings_api::routes())
        // D5 Phase 1 — Git Supervision Channel diagnostic endpoint.
        .merge(crate::mcp::git_supervision_api::routes())
        // D4+D6 Phase 2 — Blind-Spot Recommender endpoint (GET /blind-spots).
        .merge(crate::mcp::blind_spots_api::routes())
        .merge(crate::mcp::debug_builder_prompt::routes())
        .merge(crate::mcp::generation_rules_api::routes())
        .merge(crate::mcp::meta_optimizer_api::routes())
        .merge(crate::mcp::generator_eval::routes())
        .merge(crate::mcp::step_evaluation_api::routes())
        .merge(crate::mcp::hooks::routes())
        .merge(crate::mcp::inngest::routes())
        .merge(crate::mcp::api_spec_verify::routes())
        .merge(crate::mcp::headless_browser::routes())
        .merge(crate::mcp::interaction_recording::routes())
        .merge(crate::mcp::log_sources::routes())
        .merge(crate::mcp::macros::routes())
        .merge(crate::mcp::mcp_servers::routes())
        .merge(crate::mcp::misc::routes())
        .merge(crate::mcp::ai_session::routes())
        .merge(crate::mcp::auto_continue::routes())
        .merge(crate::mcp::backup_restore::routes())
        .merge(crate::mcp::playwright_collection::routes())
        .merge(crate::mcp::models::routes())
        .merge(crate::mcp::monitors::routes())
        .merge(crate::mcp::orchestration_loop_api::routes())
        .merge(crate::mcp::playwright::routes())
        .merge(crate::mcp::processes::routes())
        .merge(crate::mcp::provider_health::routes())
        .merge(crate::mcp::prompts::routes())
        .merge(crate::mcp::prompt_home::routes())
        .merge(crate::mcp::query_tool::routes())
        .merge(crate::mcp::queue::routes())
        .merge(crate::mcp::rag::routes())
        .merge(crate::mcp::recordings::routes())
        .merge(crate::mcp::reflection_api::routes())
        .merge(crate::mcp::graph_api::routes())
        .merge(crate::mcp::observations_api::routes())
        .merge(crate::mcp::entity_profiles_api::routes())
        .merge(crate::mcp::online_learning_api::routes())
        .merge(crate::mcp::memory_consolidation_api::routes())
        .merge(crate::mcp::query_memory_tool::routes())
        .merge(crate::mcp::decision_trail_api::routes())
        .merge(crate::mcp::saved_api_requests::routes())
        .merge(crate::mcp::scheduler::routes())
        .merge(crate::mcp::sdk_client::routes())
        .merge(crate::mcp::prompt_snippets::routes())
        .merge(crate::mcp::settings::routes())
        .merge(crate::mcp::shell_commands::routes())
        .merge(crate::mcp::skills::routes())
        .merge(crate::mcp::state_explorer::routes())
        .merge(crate::mcp::state_machine::routes())
        .merge(crate::state_discovery::routes())
        .merge(crate::mcp::executor::routes())
        .merge(crate::mcp::gui_config::routes())
        .merge(crate::mcp::image_quality_tests::routes())
        .merge(crate::mcp::step_type_knowledge_api::routes())
        .merge(crate::mcp::step_type_metadata_api::routes())
        .merge(crate::mcp::task_run_inspection::routes())
        .merge(crate::mcp::task_runs::routes())
        .merge(crate::mcp::terminals::routes())
        .merge(crate::mcp::steward::routes())
        .merge(crate::mcp::testing::routes())
        .merge(crate::mcp::triggers::routes())
        .merge(crate::mcp::ui_bridge::routes())
        .merge(crate::mcp::ui_bridge_integration::routes())
        .merge(crate::mcp::unified_workflows::routes())
        .merge(crate::mcp::verification_tests::routes())
        .merge(crate::mcp::websocket::routes())
        .merge(crate::mcp::window_manager::routes())
        .merge(crate::mcp::worktrees::routes())
        // `/agent-worktrees/*` — on-demand cleanup of AGENT worktrees. A
        // DIFFERENT resource from `worktrees::routes()` directly above (which
        // owns the task-run "isolated run" worktrees under `.worktrees`, incl.
        // a differently-meaning `POST /worktrees/remove`). Kept in its own
        // namespace so the two destructive routes can never be confused.
        .merge(crate::mcp::agent_worktrees::routes())
        .merge(crate::mcp::agent_tokens::routes())
        .merge(crate::install_effects_producer::routes())
        .merge(crate::mcp::token_analytics::routes())
        .merge(crate::mcp::otel_status::routes())
        .merge(crate::mcp::container_status::routes())
        .merge(crate::mcp::security_audit::routes())
        .merge(crate::mcp::knowledge::routes())
        .merge(crate::mcp::knowledge_acquisition_api::routes())
        .merge(crate::mcp::reviews::routes())
        .merge(crate::mcp::snapshots::routes())
        .merge(crate::mcp::session_recap::routes())
        .merge(crate::mcp::api_surface::routes())
        .merge(crate::mcp::api_surface_diff::routes())
        .merge(crate::mcp::prm_export::routes())
        .merge(crate::mcp::restate_api::routes())
        .merge(crate::mcp::hitl::routes())
        .merge(crate::mcp::streaming::routes())
        .merge(crate::vga::routes())
        // Section 2 of UI Bridge redesign — `/spec/...` Spec API. Mounted
        // outside `/ui-bridge/...` because the surface is consumed
        // differently (IR + projection storage, not page-control RPC).
        .merge(crate::spec_api::routes())
        // Section 11 / Phase B2 — `/scenarios/...` scenario projection.
        // Static endpoint is pure Rust; runtime endpoint IPC's into the
        // webview to combine with the live registry. Both load the IR
        // through the same `spec_api::storage` layer.
        .merge(crate::scenarios::routes())
        // Section 11 follow-up FU-3 — `/runs/:run_id/drift[/:entry_id]`
        // drift report endpoints. Pass-through of `regression_runs.drift_report_json`
        // for the qontinui-web drift dashboard proxy. Reads only — the
        // executor writes drift reports via the `record_regression_run`
        // Tauri command.
        .merge(crate::regression_api::routes())
        // Section 5b of UI Bridge redesign — `/trace/...` Trace API. Gated
        // on `settings.trace_api.enabled` (default false) because it
        // depends on the Alembic migration `section_5b_01_ui_bridge_causal_columns`
        // being applied to the shared Postgres host.
        .merge(if crate::settings::load_settings().trace_api.enabled {
            crate::trace_api::routes()
        } else {
            axum::Router::new()
        });

    // Phase 5.1 of the UI Bridge discoverability/effectiveness plan:
    // debug-only `/ui-bridge/test/inject-session` + `/ui-bridge/test/clear-sessions`
    // for SessionCard manual-render tests. The cfg gate matches the one on
    // the `mcp::test_fixtures` module declaration in `mcp/mod.rs`, so
    // production release builds without the `test-fixtures` feature compile
    // away both the module and the merge call entirely.
    // Hand the seam an AppHandle so its mutating endpoints can emit
    // `test-fixtures-injected-changed` — `useTranscriptSessions` refetches on
    // that event instead of waiting for its 30s visibility-gated poll (a
    // hidden window never ticks, which would leave a seeded/cleared scenario
    // invisible to the acceptance driver until refocus).
    #[cfg(any(debug_assertions, feature = "test-fixtures"))]
    crate::mcp::test_fixtures::set_app_handle(app_handle.clone());
    #[cfg(any(debug_assertions, feature = "test-fixtures"))]
    let base_router = base_router.merge(crate::mcp::test_fixtures::routes());

    // Canonical JSON 404 for unmatched routes. axum's default fallback returns
    // an empty body with no Content-Type, which both breaks the canonical
    // envelope contract and trips the debug envelope_audit layer (it sees a
    // non-application/json 4xx and panics). Returning the envelope here keeps
    // the error shape universal — route-not-found included — and is the JSON
    // the audit expects.
    let base_router = base_router.fallback(not_found_handler);

    // Layer ordering (`.layer()` is bottom-up — last call = outermost):
    //
    //   [CatchPanicLayer]              ← outermost: panics → 500 JSON
    //     [envelope_audit_middleware]  ← debug-only: REPORTS non-JSON errors
    //       [envelope_rewrite_middleware] ← rewrites 4xx/5xx text/plain → JSON
    //         [TraceLayer, CORS, BodyLimit, ...]
    //           [handlers]
    //
    // The panic layer must remain outermost so it can catch panics that
    // originate in any layer below, including the audit and envelope layers.
    // The audit layer sits OUTSIDE envelope_rewrite so it observes the
    // post-rewrite response. A non-JSON error response after the rewrite is a
    // definitive handler bug; as of 2026-08-05 the audit REPORTS it (ERROR log
    // + violation registry) and forwards the response unchanged rather than
    // panicking — see `mcp::envelope_audit` for why the panic was net-harmful.
    // Compiles away entirely in release.
    //
    // The #[cfg(debug_assertions)] audit layer cannot be placed inline in a
    // method-chain call, so we use a let-rebind pattern:
    //   1. Build the router through all release-build layers up to envelope_rewrite.
    //   2. In debug builds only, add the audit layer via a rebind.
    //   3. Add the outermost CatchPanicLayer + state unconditionally.
    let router_with_inner_layers = base_router
        // Degraded-boot guard (D2): when PG is unavailable (QONTINUI_ALLOW_NO_DB),
        // short-circuit KNOWN DB-backed routes to a clean 503. No-op (one atomic
        // load) on the normal PG-available path. See crate::mcp::pg_guard.
        .layer(axum::middleware::from_fn(
            crate::mcp::pg_guard::pg_degraded_guard_middleware,
        ))
        .layer(axum::middleware::from_fn(
            crate::middleware::trace_propagation_middleware,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(RequestBodyLimitLayer::new(100 * 1024 * 1024))
        .layer(axum::Extension(graphql_schema))
        .layer(axum::middleware::from_fn(
            crate::mcp::envelope::envelope_rewrite_middleware,
        ));

    // Debug-only audit layer: asserts every error response is application/json.
    // Placed between envelope_rewrite (inner) and CatchPanicLayer (outer) so:
    //   - it sees the post-rewrite response,
    //   - its panics are absorbed by CatchPanicLayer → 500 JSON in server mode,
    //   - but surface as test failures in #[tokio::test] (no catch layer there).
    // Compiles away entirely in release builds — zero production overhead.
    #[cfg(debug_assertions)]
    let router_with_inner_layers = router_with_inner_layers.layer(axum::middleware::from_fn(
        crate::mcp::envelope_audit::envelope_audit_middleware,
    ));

    router_with_inner_layers
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(
            runner_panic_handler,
        ))
        .with_state(api_state)
}

/// Build the `NOT_FOUND` error message for an unmatched `(method, path)`.
///
/// For an unmatched path under the `/ui-bridge/` surface we append a discovery
/// hint pointing at the two self-describing endpoints (`/ui-bridge/_routes` for
/// the route manifest, `/ui-bridge/commands` for invokable commands) so a caller
/// that fat-fingered a route can find the real one without grepping the source.
/// Non-`/ui-bridge/` paths keep the bare message unchanged.
fn not_found_message(method: &axum::http::Method, path: &str) -> String {
    let base = format!("No route for {} {}", method, path);
    if path.starts_with("/ui-bridge/") {
        format!(
            "{base} — discover routes at GET /ui-bridge/_routes, \
             invokable commands at GET /ui-bridge/commands"
        )
    } else {
        base
    }
}

/// Canonical JSON 404 handler for unmatched routes. See the `.fallback(...)`
/// call in `build_router` for why this exists (envelope universality + the
/// debug envelope_audit layer). For unmatched `/ui-bridge/*` paths the error
/// message also names the discovery endpoints — see `not_found_message`.
async fn not_found_handler(
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let body = crate::mcp::types::ApiResponse::<()>::error_with_code(
        not_found_message(&method, uri.path()),
        "NOT_FOUND",
    );
    (axum::http::StatusCode::NOT_FOUND, axum::Json(body)).into_response()
}

/// Convert a caught handler panic into a JSON 500 response that matches the
/// runner's `ApiResponse<()>` shape. Wired onto the main API router via
/// `tower_http::catch_panic::CatchPanicLayer::custom`.
///
/// The panic payload is `Box<dyn Any + Send>`; downcast to the two common
/// concrete types (`&'static str` from `panic!("literal")` and `String` from
/// `panic!("{}", x)`) and fall back to a generic message otherwise.
fn runner_panic_handler(err: Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    use axum::response::IntoResponse;

    let msg = if let Some(s) = err.downcast_ref::<&'static str>() {
        s.to_string()
    } else if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    };

    tracing::error!(handler_panic = true, message = %msg, "runner HTTP handler panicked");

    // The panic hook already wrote a `crash_*.txt` (it runs before unwinding and
    // cannot know the panic is about to be caught). We DID catch it — the
    // process lives and the caller gets a 500 envelope — so downgrade the dump.
    // Left as-is, the next startup's crash-dump scan would adopt it and report
    // `derived_status: errored` for a runner that never died.
    let retracted = crate::logging::retract_last_crash_dump("HTTP handler panic");
    tracing::debug!(
        retracted,
        "caught-panic crash-dump retraction after handler panic"
    );

    let body = crate::mcp::types::api_error(format!("handler panicked: {}", msg));
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(body),
    )
        .into_response()
}

/// Whether a bound socket address is reachable from the LAN: any
/// non-loopback IP counts (including the unspecified address `0.0.0.0`,
/// which listens on all interfaces); `127.0.0.1`/`::1` does not.
///
/// Used at bind time to populate `AppState.api_lan_bound`, which the
/// backend heartbeat advertises as `lan_reachable` (plan
/// 2026-06-12-mobile-account-usage-error-recovery, runner P1). Derived
/// from the listener's REAL local address so a future non-loopback bind
/// flips the advertisement without further changes.
pub(crate) fn lan_reachable_for_bound_addr(addr: &std::net::SocketAddr) -> bool {
    !addr.ip().is_loopback()
}

/// Try to bind to a port with SO_REUSEADDR
fn try_bind_port(port: u16) -> Result<std::net::TcpListener, std::io::Error> {
    // Create socket with SO_REUSEADDR to allow binding even if there are zombie connections
    // This is necessary on Windows where TIME_WAIT/CLOSE_WAIT sockets can block port binding
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    // Bind loopback only: all consumers (Claude Code, supervisor, web frontend) are
    // co-located with the runner. Loopback traffic bypasses Windows Firewall, so
    // every uniquely-renamed `qontinui-runner-<id>.exe` copy avoids triggering a
    // first-run permission prompt. Also reduces attack surface — the API is never
    // exposed to LAN.
    socket.bind(&std::net::SocketAddr::from(([127, 0, 0, 1], port)).into())?;
    socket.listen(1024)?;
    Ok(socket.into())
}

/// Start the MCP API server
pub async fn start_server(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
    port: u16,
    instance_manager: Arc<crate::instance_manager::InstanceManager>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let emitter = app_handle.clone();
    let api_ready_flag = app_state.clone();
    info!("MCP API server: building router via create_router...");
    let router = create_router(app_state, rag_state, app_handle, instance_manager);
    info!("MCP API server: create_router returned, entering bind loop");

    // D3 (self-healing degraded mode): if the runner booted degraded (PG
    // unreachable + QONTINUI_ALLOW_NO_DB), start a background probe that
    // reconnects + provisions and lifts the pg_guard 503 gate when PG returns —
    // no restart needed for a transient outage. No-op on a normal boot.
    if !crate::database::pg::pg_available() {
        if let Some(pg) = crate::database::pg::PgDb::try_global() {
            tracing::warn!(
                "Runner booted DEGRADED (no PG); starting background PG reconnect probe"
            );
            pg.spawn_reconnect_probe();
        }
    }

    // Try the requested port first, then fallback ports if zombie connections are blocking
    // This can happen on Windows when previous process crashes leave orphaned sockets
    let ports_to_try = [port, port + 1, port + 2];
    let mut last_error = None;

    for try_port in ports_to_try {
        info!("MCP API server: try_bind_port({})...", try_port);
        match try_bind_port(try_port) {
            Ok(std_listener) => {
                // Record whether the ACTUAL bound address is LAN-reachable
                // (non-loopback). Read from the listener rather than assumed,
                // so the heartbeat's `lan_reachable` advertisement stays
                // honest if the bind strategy ever changes. Currently always
                // false: `try_bind_port` binds 127.0.0.1 only (intentional —
                // see the comment there).
                let lan_bound = std_listener
                    .local_addr()
                    .map(|addr| lan_reachable_for_bound_addr(&addr))
                    .unwrap_or(false);
                api_ready_flag
                    .api_lan_bound
                    .store(lan_bound, Ordering::Relaxed);
                let listener = tokio::net::TcpListener::from_std(std_listener)?;
                if try_port != port {
                    warn!(
                        "Primary port {} was blocked, using fallback port {}. \
                         Restart the app after zombie connections clear.",
                        port, try_port
                    );
                }
                info!("MCP API server listening on port {}", try_port);

                // Store the actual bound port in AppState
                api_ready_flag.api_port.store(try_port, Ordering::Relaxed);

                // Mirror the bound port into the install-interception shim's
                // process-global so the terminal env-seam (which has no
                // `app_state`) injects the CORRECT loopback port for
                // secondary/temp runners — plan §6's #1 live-verification
                // footgun. Set HERE, the same place app_state.api_port lands.
                crate::install_effects_producer::intercept::set_bound_port(try_port);

                // Port stored in api_ready_flag.api_port above; PG queries accept runner_port as parameter.

                // Signal that the API is ready for requests
                api_ready_flag.api_ready.store(true, Ordering::Relaxed);
                if let Err(e) = emitter.emit("api-ready", try_port) {
                    warn!("Failed to emit api-ready event: {}", e);
                } else {
                    info!("Emitted api-ready event (port {})", try_port);
                }

                // Update window title if using non-default port or instance name
                let default_port = crate::mcp::types::MCP_API_PORT;
                let instance_name = crate::instance::instance_name();
                let needs_title_update = try_port != default_port || instance_name.is_some();
                if needs_title_update {
                    let title = match instance_name {
                        Some(name) => format!("Qontinui Runner — {} [:{}]", name, try_port),
                        None => format!("Qontinui Runner [:{}]", try_port),
                    };
                    if let Some(window) =
                        emitter.get_webview_window(qontinui_runner_lib::get_main_window_label())
                    {
                        if let Err(e) = window.set_title(&title) {
                            warn!("Failed to set window title: {}", e);
                        } else {
                            info!("Window title set to: {}", title);
                        }
                    }
                }

                axum::serve(listener, router).await?;
                return Ok(());
            }
            Err(e) => {
                warn!("Failed to bind to port {}: {}", try_port, e);
                last_error = Some(e);
            }
        }
    }

    Err(Box::new(last_error.unwrap_or_else(|| {
        std::io::Error::other("All ports failed")
    })))
}

/// Nonce-gated claims read passthrough (plan
/// 2026-06-11-claims-read-auth-hardening, Phase 2).
///
/// Route-level tests build a minimal router with the two real handlers and
/// assert the gate's 401 paths (missing/wrong nonce — never forwarded; the
/// 401 is produced before any upstream I/O, structurally guaranteed by
/// `coord_mcp::proxy_request_gate` running first and separately unit-tested
/// in `coord_mcp::tests`). The forwarding leg cannot be exercised end-to-end
/// through the route because the live bearer comes from the encrypted
/// `AuthManager` slot (not seedable in a unit test), so it is tested through
/// the `forward_claims_get` seam against a local mock coord with a synthetic
/// bearer — covering live-bearer injection, verbatim query forwarding, and
/// verbatim status+body passthrough including non-200 upstream verdicts.
#[cfg(test)]
mod self_id_chain_tests {
    use super::{
        select_lifecycle_caller, select_terminal_caller, self_id_health_snapshot,
        self_id_miss_sample_dirs, self_id_miss_samples, terminal_leg, LifecycleMiss, SelfIdOutcome,
        TerminalLeg, SELF_ID_MISS_SAMPLE_CAP,
    };
    use crate::session::session_lifecycle_store::{
        TerminalSessionRecord, ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED, ORIGIN_RECONCILED,
    };

    #[test]
    fn every_outcome_has_a_distinct_label() {
        let labels: Vec<&str> = SelfIdOutcome::ALL.iter().map(|o| o.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            labels.len(),
            "duplicate self-id outcome label — the /health series would collide: {labels:?}"
        );
        // 12 before the terminal-leg fix; +4 terminal-leg gates
        // (`terminal_record_missing`, `terminal_record_unadmitted`,
        // `terminal_anchor_not_uuid`, `ambiguous_terminal`).
        assert_eq!(SelfIdOutcome::ALL.len(), 16);
    }

    /// FIX 5: the counter slot is a compiler-checked `match`, not a search of
    /// `ALL` that fell back to slot 0 — which is `Injected`, the SUCCESS
    /// counter. This asserts the two orderings agree AND that every variant
    /// lands in a slot of its own, end to end through `/health`.
    #[test]
    fn every_outcome_counts_into_its_own_slot() {
        for (i, outcome) in SelfIdOutcome::ALL.iter().enumerate() {
            assert_eq!(
                outcome.index(),
                i,
                "`{}` occupies ALL[{i}] but indexes to slot {} — the /health \
                 series would render another variant's count",
                outcome.label(),
                outcome.index()
            );
        }

        // Round-trip: bumping each variant once must move exactly its own
        // series by exactly one. A variant miscounted into slot 0 shows up
        // here as `injected` moving by 2.
        let before = self_id_health_snapshot();
        for outcome in SelfIdOutcome::ALL {
            super::record_self_id_outcome(outcome);
        }
        let after = self_id_health_snapshot();
        for outcome in SelfIdOutcome::ALL {
            let b = before[outcome.label()].as_u64().expect("counter is a u64");
            let a = after[outcome.label()].as_u64().expect("counter is a u64");
            assert_eq!(
                a,
                b + 1,
                "`{}` did not increment its own slot ({b} → {a})",
                outcome.label()
            );
        }
    }

    #[test]
    fn health_snapshot_renders_every_series() {
        // A missing series reads as "this break never happens", which is the
        // ambiguity the whole counter family exists to remove — so every
        // outcome must be present even at zero.
        let snap = self_id_health_snapshot();
        let obj = snap.as_object().expect("selfId snapshot must be an object");
        for outcome in SelfIdOutcome::ALL {
            assert!(
                obj.contains_key(outcome.label()),
                "GET /health selfId is missing the `{}` series",
                outcome.label()
            );
        }
        // Every counter series, plus the bounded diagnostic sample.
        assert!(
            obj["recent_misses"].is_array(),
            "the miss sample must be rendered as an array"
        );
        assert_eq!(obj.len(), SelfIdOutcome::ALL.len() + 1);
    }

    #[test]
    fn the_three_injected_arms_are_the_only_success_outcomes() {
        // coord cannot distinguish the failure arms: from its side every
        // non-injected outcome is an identical `absent`. Guards against a
        // future variant being added as another "success" without the header
        // actually going. Phase 3 added `injected_via_lifecycle`; the
        // terminal-keyed fix adds `injected_via_terminal`.
        let successes: Vec<&str> = SelfIdOutcome::ALL
            .iter()
            .filter(|o| {
                matches!(
                    o,
                    SelfIdOutcome::Injected
                        | SelfIdOutcome::InjectedViaTerminal
                        | SelfIdOutcome::InjectedViaLifecycle
                )
            })
            .map(|o| o.label())
            .collect();
        assert_eq!(
            successes,
            vec![
                "injected",
                "injected_via_terminal",
                "injected_via_lifecycle"
            ]
        );
    }

    #[test]
    fn every_lifecycle_miss_maps_to_a_distinct_counted_outcome() {
        // The split is only worth anything if two different gates cannot land
        // on one counter.
        let outcomes: Vec<&str> = [
            LifecycleMiss::NoRecord,
            LifecycleMiss::Unregistered,
            LifecycleMiss::AnchorNotUuid,
            LifecycleMiss::Ambiguous,
        ]
        .iter()
        .map(|m| m.outcome().label())
        .collect();
        assert_eq!(
            outcomes,
            vec![
                "no_lifecycle_record",
                "record_unregistered",
                "record_anchor_not_uuid",
                "ambiguous_workdir"
            ]
        );
    }

    /// Minimal open-record builder for the pure-selection tests. Records are
    /// `authoritative` by default — the admissible case; the origin-guard
    /// tests override it explicitly.
    fn rec(csid: &str, working_dir: Option<&str>, last_seen_at: i64) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: csid.to_string(),
            config_dir: None,
            working_dir: working_dir.map(str::to_string),
            page_id: "default".to_string(),
            zone_index: 0,
            title: None,
            terminal_id: format!("term-{csid}"),
            opened_at: 0,
            last_seen_at,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
            origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
            restore_pending_at: None,
            confirmed_at: None,
            handle: None,
        }
    }

    /// Same record with a different durable `origin`.
    fn rec_with_origin(
        csid: &str,
        working_dir: Option<&str>,
        origin: Option<&str>,
    ) -> TerminalSessionRecord {
        TerminalSessionRecord {
            origin: origin.map(str::to_string),
            ..rec(csid, working_dir, 10)
        }
    }

    /// Anchors are real uuids in every selection test now: the selected
    /// caller id IS the record's anchor (a `coord.agent_sessions` id), so a
    /// placeholder like `"match"` is no longer a valid fixture.
    const ANCHOR_A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const ANCHOR_B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
    const ANCHOR_C: &str = "cccccccc-0000-4000-8000-000000000003";

    fn uuid_of(s: &str) -> uuid::Uuid {
        uuid::Uuid::parse_str(s).expect("fixture anchor must be a uuid")
    }

    #[test]
    fn lifecycle_selection_yields_the_anchor_not_the_registrar_value() {
        // THE id-space fix: coord validates `X-Coord-Caller-Session` with
        // `session_on_device` against `coord.agent_sessions`, where the
        // registrar's per-boot `coord.sessions` uuid never appears. The
        // selected id must therefore be the record's own anchor.
        let records = vec![
            rec(ANCHOR_B, Some("D:/elsewhere"), 50),
            rec(ANCHOR_A, Some("D:/repo"), 10),
        ];
        let got = select_lifecycle_caller(&records, "D:/repo", None);
        assert_eq!(got, Ok(uuid_of(ANCHOR_A)));
    }

    #[test]
    fn lifecycle_selection_admits_a_record_the_registrar_filter_would_have_dropped() {
        // THE 678→0 fix. This record is `authoritative` on disk but was NEVER
        // registered with this runner PROCESS since boot (the registrar's
        // reverse map is in-process and starts empty), so the old
        // `is_registered` closure dropped it — every single time, 678 of 678.
        // It must now resolve.
        let records = vec![rec(ANCHOR_A, Some("D:/repo"), 99)];
        assert_eq!(
            select_lifecycle_caller(&records, "D:/repo", None),
            Ok(uuid_of(ANCHOR_A)),
            "an authoritative durable record must resolve without any registrar consultation"
        );
    }

    /// FIX 3: the admitted set is the two anchor origins derived from
    /// PROOF — `authoritative` (the id was known) and `observed` (a
    /// process-start-anchored, uniquely-correlated transcript bind, the same
    /// tier `restore_record_emitter.rs:264` already trusts enough to
    /// `claude --resume`). `reconciled` ("may name a foreign session") and a
    /// `None` origin (predates the field) stay out.
    ///
    /// This test used to assert `authoritative` was the ONLY admitted origin.
    /// That was the defect: `observed` is a live third value, and admitting
    /// only `authoritative` silently withheld 8 of the operator's 34 open
    /// records while mis-bucketing them into `record_unregistered`.
    #[test]
    fn lifecycle_selection_admits_the_proof_backed_origins_only() {
        for origin in [Some(ORIGIN_RECONCILED), None] {
            let records = vec![rec_with_origin(ANCHOR_A, Some("D:/repo"), origin)];
            assert_eq!(
                select_lifecycle_caller(&records, "D:/repo", None),
                Err(LifecycleMiss::Unregistered),
                "origin {origin:?} must not be admitted"
            );
        }
        for origin in [ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED] {
            let records = vec![rec_with_origin(ANCHOR_A, Some("D:/repo"), Some(origin))];
            assert_eq!(
                select_lifecycle_caller(&records, "D:/repo", None),
                Ok(uuid_of(ANCHOR_A)),
                "origin {origin} must be admitted"
            );
        }
        // An `observed` record is a full candidate, not a tiebreak-loser: it
        // makes a workdir AMBIGUOUS alongside an authoritative sibling rather
        // than being silently dropped so the other one "wins".
        let mixed = vec![
            rec_with_origin(ANCHOR_A, Some("D:/repo"), Some(ORIGIN_AUTHORITATIVE)),
            rec_with_origin(ANCHOR_B, Some("D:/repo"), Some(ORIGIN_OBSERVED)),
        ];
        assert_eq!(
            select_lifecycle_caller(&mixed, "D:/repo", None),
            Err(LifecycleMiss::Ambiguous)
        );
    }

    #[test]
    fn lifecycle_selection_skips_non_uuid_anchors() {
        // An anchor that is not a uuid cannot name a `coord.agent_sessions`
        // row, so it is not a candidate even when admitted.
        let records = vec![rec("not-a-uuid", Some("D:/repo"), 99)];
        assert_eq!(
            select_lifecycle_caller(&records, "D:/repo", None),
            Err(LifecycleMiss::AnchorNotUuid),
            "non-uuid anchor must not inject"
        );
    }

    #[test]
    fn lifecycle_selection_refuses_to_pick_a_winner_on_a_shared_workdir() {
        // THE ambiguity rule. Two admitted sessions share one workdir (the
        // workspace root hosts 13). The old code returned the greatest
        // `last_seen_at` — a poll-order artifact, i.e. a confidently WRONG
        // identity ~1 time in 13. It must now resolve to nothing, counted as
        // `ambiguous_workdir`.
        let records = vec![
            rec(ANCHOR_A, Some("D:/repo"), 100),
            rec(ANCHOR_B, Some("D:/repo"), 200),
        ];
        let got = select_lifecycle_caller(&records, "D:/repo", None);
        assert_eq!(got, Err(LifecycleMiss::Ambiguous));
        assert_eq!(got.unwrap_err().outcome(), SelfIdOutcome::AmbiguousWorkdir);

        // Input order cannot smuggle a winner back in either.
        let reversed: Vec<_> = records.iter().rev().cloned().collect();
        assert_eq!(
            select_lifecycle_caller(&reversed, "D:/repo", None),
            Err(LifecycleMiss::Ambiguous)
        );

        // …but if only ONE of them is admissible, that one resolves exactly:
        // the guard narrows the candidate set before ambiguity is judged.
        let mixed = vec![
            rec_with_origin(ANCHOR_A, Some("D:/repo"), Some(ORIGIN_AUTHORITATIVE)),
            rec_with_origin(ANCHOR_B, Some("D:/repo"), Some(ORIGIN_RECONCILED)),
        ];
        assert_eq!(
            select_lifecycle_caller(&mixed, "D:/repo", None),
            Ok(uuid_of(ANCHOR_A))
        );
    }

    #[test]
    fn lifecycle_selection_treats_duplicate_anchors_as_one_candidate() {
        // Two records naming the SAME session (e.g. a re-hosted terminal) are
        // not an ambiguity — there is only one identity to publish.
        let mut second = rec(ANCHOR_A, Some("D:/repo"), 200);
        second.terminal_id = "term-other".to_string();
        let records = vec![rec(ANCHOR_A, Some("D:/repo"), 100), second];
        assert_eq!(
            select_lifecycle_caller(&records, "D:/repo", None),
            Ok(uuid_of(ANCHOR_A))
        );
    }

    #[test]
    fn terminal_selection_is_exact_on_a_shared_workdir() {
        // The deterministic leg: the nonce names a terminal, the terminal
        // names one open record, that record's anchor is the caller. The
        // 13-way-shared workdir is irrelevant here — which is the whole point
        // of keying on the terminal.
        let records = vec![
            rec(ANCHOR_A, Some("D:/repo"), 100),
            rec(ANCHOR_B, Some("D:/repo"), 200),
        ];
        assert_eq!(
            select_terminal_caller(&records, &format!("term-{ANCHOR_A}")),
            Ok(uuid_of(ANCHOR_A))
        );
        assert_eq!(
            select_terminal_caller(&records, &format!("term-{ANCHOR_B}")),
            Ok(uuid_of(ANCHOR_B))
        );
        // Each gate is now its own typed outcome instead of a bare `None`.
        assert_eq!(
            select_terminal_caller(&records, "term-nope"),
            Err(SelfIdOutcome::TerminalRecordMissing)
        );
        let bad = vec![rec("not-a-uuid", Some("D:/repo"), 1)];
        assert_eq!(
            select_terminal_caller(&bad, "term-not-a-uuid"),
            Err(SelfIdOutcome::TerminalAnchorNotUuid)
        );
    }

    /// FIX 1: the durable registry really can hold SEVERAL open rows on one
    /// `terminal_id` (`repair_terminal_id_collisions` records 54 on one), and
    /// `open_records()` walks a `HashMap`, so the old `.find()` returned a
    /// hash-order-arbitrary row — possibly a PREVIOUS run's session id.
    ///
    /// The fixture is the dangerous window deliberately: the STALE row is the
    /// CONFIRMED one and the fresh row is not, which is precisely the shape
    /// where the store's `open_authority_key` (`confirmed_at.is_some()`,
    /// `last_seen_at`, `opened_at`) would rank the WRONG session first. So the
    /// leg refuses rather than ranks.
    #[test]
    fn terminal_selection_refuses_two_open_rows_on_one_terminal() {
        let mut stale = rec(ANCHOR_A, Some("D:/repo"), 100);
        stale.terminal_id = "term-reused".to_string();
        stale.confirmed_at = Some(50); // the stale row is the CONFIRMED one
        let mut fresh = rec(ANCHOR_B, Some("D:/repo"), 200);
        fresh.terminal_id = "term-reused".to_string();
        fresh.confirmed_at = None; // …the live one is not confirmed yet

        // Order-independent on purpose: the bug this replaces was hash-order
        // dependent, so an order-sensitive test would pass while broken.
        for records in [
            vec![stale.clone(), fresh.clone()],
            vec![fresh.clone(), stale.clone()],
        ] {
            assert_eq!(
                select_terminal_caller(&records, "term-reused"),
                Err(SelfIdOutcome::AmbiguousTerminal),
                "two open rows on one terminal must refuse, in EITHER input order"
            );
        }

        // Two rows naming the SAME session are one candidate, not an
        // ambiguity — there is only one identity to publish.
        let mut twin = stale.clone();
        twin.last_seen_at = 900;
        assert_eq!(
            select_terminal_caller(&[stale, twin], "term-reused"),
            Ok(uuid_of(ANCHOR_A))
        );
    }

    /// FIX 2 — THE fallthrough regression. A binding whose terminal is
    /// KNOWN-but-unresolvable must not inherit a same-cwd SIBLING terminal's
    /// id. Before the three-way [`TerminalLeg`], leg 1 returned a bare `None`
    /// here and the workdir chain answered with T2's anchor at full
    /// confidence — every one of T1's coord calls labelled as T2.
    #[test]
    fn a_known_terminal_that_misses_never_inherits_a_siblings_id() {
        let workdir = "D:/repo";
        // T2: the workdir's SINGLE admitted candidate.
        let mut t2 = rec(ANCHOR_B, Some(workdir), 200);
        t2.terminal_id = "T2".to_string();
        // T1: same cwd, record present but NOT admitted.
        let mut t1 = rec_with_origin(ANCHOR_A, Some(workdir), Some(ORIGIN_RECONCILED));
        t1.terminal_id = "T1".to_string();

        for (records, expected) in [
            (
                vec![t1.clone(), t2.clone()],
                SelfIdOutcome::TerminalRecordUnadmitted,
            ),
            // …and the same when T1 has no open record at all.
            (vec![t2.clone()], SelfIdOutcome::TerminalRecordMissing),
        ] {
            // The payload the bug would have shipped: the workdir leg DOES
            // resolve here, confidently, to the wrong session.
            assert_eq!(
                select_lifecycle_caller(&records, workdir, None),
                Ok(uuid_of(ANCHOR_B)),
                "fixture invalid: the workdir must have exactly one admitted candidate"
            );
            // Leg 1 must therefore STOP, not fall through.
            assert_eq!(
                terminal_leg(&records, Some("T1")),
                TerminalLeg::Miss(expected),
                "a known-but-unresolvable terminal must be a typed miss, never a fallthrough"
            );
        }

        // The ONLY fallthrough arm: the binding carries no terminal at all
        // (restore, adopt, mint route, in-cwd `.mcp.json`) — unchanged.
        assert_eq!(terminal_leg(&[t2.clone()], None), TerminalLeg::NoTerminal);
        // And a terminal that DOES resolve still resolves.
        assert_eq!(
            terminal_leg(&[t2], Some("T2")),
            TerminalLeg::Resolved(uuid_of(ANCHOR_B))
        );
    }

    /// The terminal leg applies the anchor-trust guard too. Being the UNIQUE
    /// record for a terminal does not make a guessed anchor correct: a
    /// `reconciled` id "may name a foreign session" by its own definition, so
    /// resolving it would ship a confidently wrong identity — the exact failure
    /// this chain exists to prevent. Uniqueness is not correctness.
    #[test]
    fn terminal_selection_refuses_an_untrusted_anchor() {
        for origin in [Some(ORIGIN_RECONCILED.to_string()), None] {
            let mut r = rec(ANCHOR_A, Some("D:/repo"), 100);
            r.origin = origin.clone();
            assert_eq!(
                select_terminal_caller(&[r], &format!("term-{ANCHOR_A}")),
                Err(SelfIdOutcome::TerminalRecordUnadmitted),
                "a {origin:?}-origin anchor must NOT resolve, even as the terminal's only record"
            );
        }
        // FIX 3: both TRUSTED origins resolve on this leg — so the guard
        // rejects untrustworthy anchors, not the leg itself.
        for origin in [ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED] {
            let good = rec_with_origin(ANCHOR_A, Some("D:/repo"), Some(origin));
            assert_eq!(
                select_terminal_caller(&[good], &format!("term-{ANCHOR_A}")),
                Ok(uuid_of(ANCHOR_A)),
                "origin {origin} must be admitted"
            );
        }
    }

    #[test]
    fn miss_sample_dirs_separate_matched_from_merely_open() {
        // What makes a `no_lifecycle_record` verdict actionable: the proxy
        // workdir had no match, but the operator can see WHICH dirs did have
        // open records (wrong granularity vs record closed).
        let records = vec![
            rec(ANCHOR_A, Some("D:/root"), 1),
            rec(ANCHOR_B, Some("D:/root/repo"), 1),
            rec(ANCHOR_C, None, 1),
        ];
        let (candidates, open) = self_id_miss_sample_dirs(&records, "D:/root/other", None);
        assert!(candidates.is_empty(), "nothing matched the proxy workdir");
        assert_eq!(
            open,
            vec!["D:/root".to_string(), "D:/root/repo".to_string()]
        );

        let (candidates, _) = self_id_miss_sample_dirs(&records, "D:/root", None);
        assert_eq!(candidates, vec!["D:/root".to_string()]);
    }

    #[test]
    fn miss_sample_ring_records_a_miss_and_stays_bounded() {
        // Bounded on purpose: this leg missed 678/678 before the fix, so an
        // unbounded sample would grow with the failure rate.
        let overflow = SELF_ID_MISS_SAMPLE_CAP + 3;
        for i in 0..overflow {
            super::record_self_id_miss_sample(
                SelfIdOutcome::NoLifecycleRecord,
                &format!("D:/repo/{i}"),
                vec![],
                vec!["D:/root".to_string()],
            );
        }
        let q = self_id_miss_samples().lock().expect("miss ring poisoned");
        assert_eq!(q.len(), SELF_ID_MISS_SAMPLE_CAP, "the ring must be capped");
        let newest = q.back().expect("ring is non-empty");
        assert_eq!(newest.gate, "no_lifecycle_record");
        assert_eq!(newest.workdir, format!("D:/repo/{}", overflow - 1));
        assert_eq!(newest.open_dirs, vec!["D:/root".to_string()]);
        // Oldest entries were evicted, newest kept.
        assert_eq!(
            q.front().expect("ring is non-empty").workdir,
            format!("D:/repo/{}", overflow - SELF_ID_MISS_SAMPLE_CAP)
        );
    }

    #[test]
    fn anchor_as_caller_session_accepts_only_uuids() {
        assert_eq!(
            super::anchor_as_caller_session(ANCHOR_A),
            Some(uuid_of(ANCHOR_A))
        );
        assert_eq!(
            super::anchor_as_caller_session(&format!("  {ANCHOR_A}  ")),
            Some(uuid_of(ANCHOR_A)),
            "surrounding whitespace is tolerated"
        );
        assert_eq!(super::anchor_as_caller_session("not-a-uuid"), None);
        assert_eq!(super::anchor_as_caller_session(""), None);
    }

    #[test]
    fn lifecycle_selection_misses_cleanly() {
        // No records / no workdir match / recordless working_dir → the
        // `no_lifecycle_record` gate (header simply absent, as today).
        assert_eq!(
            select_lifecycle_caller(&[], "D:/repo", None),
            Err(LifecycleMiss::NoRecord)
        );
        let records = vec![rec(ANCHOR_A, None, 10), rec(ANCHOR_B, Some("D:/other"), 10)];
        let got = select_lifecycle_caller(&records, "D:/repo", None);
        assert_eq!(got, Err(LifecycleMiss::NoRecord));
        assert_eq!(
            got.unwrap_err().outcome(),
            SelfIdOutcome::NoLifecycleRecord,
            "a workdir with no open record must be counted as its own gate"
        );
    }
}

/// Semantic-recall enrichment (plan
/// `2026-07-30-semantic-recall-query-embedding-via-runner`, Phases 2-3).
///
/// The classifier and the injector are pure, so the whole trigger matrix is
/// asserted here without an HTTP server or a live embedding service. The
/// load-bearing guarantee is the NEGATIVE one — every request that is not an
/// enrichable `coord_memory_search` must reach coord byte-identical — so the
/// non-trigger cases get as much attention as the trigger case.
#[cfg(test)]
mod memory_search_enrichment_tests {
    use super::{
        classify_memory_search, inject_query_embedding, memory_enrich_health_snapshot,
        MemoryEnrichOutcome, MemorySearchShape, MEMORY_EMBED_TIMEOUT,
    };
    use serde_json::json;

    fn search_call(args: serde_json::Value) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "coord_memory_search", "arguments": args },
        })
    }

    /// (a) The passthrough regression test: a non-`coord_memory_search` request
    /// is never classified as enrichable, so its bytes are forwarded verbatim.
    #[test]
    fn other_requests_are_never_touched() {
        let others = vec![
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
            search_call(json!({"query_text": "x"})).clone_with_tool("coord_memory_record"),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"coord_orient","arguments":{"query_text":"x"}}}),
        ];
        for body in others {
            assert!(
                matches!(classify_memory_search(&body), MemorySearchShape::NotASearch),
                "must not be enrichable: {body}"
            );
        }
    }

    /// (c) A caller-supplied vector is NEVER overwritten — theirs may be in a
    /// deliberately different space.
    #[test]
    fn caller_supplied_vector_is_never_overwritten() {
        let body = search_call(json!({
            "query_text": "login",
            "query_embedding": [0.1, 0.2, 0.3],
        }));
        assert!(matches!(
            classify_memory_search(&body),
            MemorySearchShape::Skip(MemoryEnrichOutcome::SkippedPresent)
        ));
    }

    #[test]
    fn a_plain_search_is_enrichable() {
        let body = search_call(json!({"query_text": "how does the merge train hold a PR"}));
        match classify_memory_search(&body) {
            MemorySearchShape::Enrichable {
                query_text,
                needs_cleanup,
            } => {
                assert_eq!(query_text, "how does the merge train hold a PR");
                assert!(!needs_cleanup, "a clean body needs no stripping");
            }
            _ => panic!("a plain coord_memory_search must be enrichable"),
        }
    }

    /// Shapes that ARE a search but cannot be enriched are counted, not
    /// silently dropped — otherwise a body shape we stopped handling would
    /// look identical to "no searches happened".
    #[test]
    fn unenrichable_search_shapes_are_classified_as_parse_skips() {
        let missing_text = search_call(json!({"limit": 5}));
        let blank_text = search_call(json!({"query_text": "   "}));
        let non_string = search_call(json!({"query_text": 42}));
        // A JSON-RPC batch carrying a search: deliberately not rewritten.
        let batch = json!([search_call(json!({"query_text": "a"}))]);
        for body in [missing_text, blank_text, non_string, batch] {
            assert!(
                matches!(
                    classify_memory_search(&body),
                    MemorySearchShape::Skip(MemoryEnrichOutcome::SkippedParse)
                ),
                "expected a parse skip for {body}"
            );
        }
    }

    /// A batch with no search at all is ordinary traffic — not a skip, not
    /// counted.
    #[test]
    fn a_batch_without_a_search_is_ordinary_traffic() {
        let batch = json!([json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})]);
        assert!(matches!(
            classify_memory_search(&batch),
            MemorySearchShape::NotASearch
        ));
    }

    /// (d) The enriched body carries the vector AND the exact model tag —
    /// under the QUERY leg's field name. Naming it `embedding_model` (the
    /// write leg's spelling) makes the backend reject every enriched search
    /// with a 422, i.e. fail CLOSED, which is the opposite of the design.
    #[test]
    fn injection_writes_the_pair_under_the_query_leg_field_names() {
        let mut body = search_call(json!({"query_text": "login"}));
        let embedding: Vec<f32> = (0..384).map(|i| i as f32 * 0.001).collect();
        assert!(inject_query_embedding(&mut body, embedding));

        let args = &body["params"]["arguments"];
        assert_eq!(
            args["query_embedding"]
                .as_array()
                .expect("vector must be present")
                .len(),
            384
        );
        assert_eq!(
            args["query_embedding_model"],
            crate::database::embedding_client::EMBEDDING_MODEL_TAG
        );
        assert!(
            args.get("embedding_model").is_none(),
            "the WRITE leg's field name must never appear on the query leg"
        );
        assert_eq!(
            args["query_text"], "login",
            "the query text still rides along"
        );
    }

    #[test]
    fn injection_declines_a_body_with_no_arguments_object() {
        let mut body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "coord_memory_search" },
        });
        assert!(
            !inject_query_embedding(&mut body, vec![0.0; 384]),
            "no arguments object ⇒ decline rather than fabricate one"
        );
    }

    /// (b) Embedder unavailable ⇒ the body is forwarded UNMODIFIED. This is
    /// the fail-open guarantee: recall degrades to today's FTS-only answer,
    /// it never errors. Pointed at a port nothing listens on, so the verdict
    /// does not depend on whether this machine runs the real service.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unavailable_embedder_leaves_the_body_untouched() {
        let client =
            crate::database::embedding_client::EmbeddingClient::with_url("http://127.0.0.1:1/none");
        let body = serde_json::to_vec(&search_call(json!({"query_text": "login"}))).unwrap();
        let out = super::enrich_memory_search_body_with(&body, Some(&client)).await;
        assert!(
            out.is_none(),
            "a dead embedder must forward the original bytes, not fail the search"
        );
    }

    /// The outcome→series mapping itself, which nothing else covers: a wrong
    /// `idx()` would silently inflate `enriched` — the one series meant to be
    /// positive proof the arm is firing. Counters are process-global, so this
    /// asserts a DELTA rather than an absolute.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_skip_lands_in_its_own_series_and_not_in_enriched() {
        let read = |k: &str| {
            memory_enrich_health_snapshot()
                .get(k)
                .and_then(|v| v.as_u64())
                .expect("series must exist")
        };
        let before_present = read("skipped_present");
        let before_enriched = read("enriched");

        // Drive the SkippedPresent path deliberately: it is decided by
        // `classify_memory_search` BEFORE any network call, so this asserts the
        // outcome→series mapping deterministically. Routing through a dead port
        // instead would land in either skipped_unavailable OR skipped_timeout
        // depending on how fast the OS refuses the connect — a real flake, and
        // the reason this test does not use one.
        let client =
            crate::database::embedding_client::EmbeddingClient::with_url("http://127.0.0.1:1/none");
        let body = serde_json::to_vec(&search_call(json!({
            "query_text": "login",
            "query_embedding": [0.1, 0.2],
        })))
        .unwrap();
        let out = super::enrich_memory_search_body_with(&body, Some(&client)).await;

        assert!(out.is_none(), "a caller-supplied vector is left alone");
        assert!(
            read("skipped_present") >= before_present + 1,
            "the skip must land in its OWN series"
        );
        assert_eq!(
            read("enriched"),
            before_enriched,
            "a skip must NEVER be counted as enriched — that series is the only \
             positive proof the semantic arm is firing"
        );
    }

    /// Every outcome maps to a distinct slot, and every slot is in range.
    /// Guards the exhaustive `idx()` against a copy-paste collision.
    #[test]
    fn every_outcome_maps_to_a_distinct_in_range_slot() {
        let mut seen: Vec<usize> = MemoryEnrichOutcome::ALL.iter().map(|o| o.idx()).collect();
        assert!(
            seen.iter().all(|i| *i < MemoryEnrichOutcome::ALL.len()),
            "every index must be in range"
        );
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "outcome indices must be distinct");
    }

    /// A LONE model tag must NOT block enrichment. It names a space with
    /// nothing in it, the vector can only come from us, and coord rejects the
    /// lone tag outright — so skipping on it would convert a search that works
    /// today into a hard error. It is flagged for cleanup instead.
    #[test]
    fn a_lone_model_tag_is_enrichable_and_flagged_for_cleanup() {
        let body = search_call(json!({
            "query_text": "login",
            "query_embedding_model": "some-other-space@v9",
        }));
        match classify_memory_search(&body) {
            MemorySearchShape::Enrichable {
                query_text,
                needs_cleanup,
            } => {
                assert_eq!(query_text, "login");
                assert!(
                    needs_cleanup,
                    "a lone tag must be stripped if we end up not enriching"
                );
            }
            _ => panic!("a lone model tag must not block enrichment"),
        }
    }

    /// An explicit JSON `null` is ABSENT, not supplied — some MCP clients
    /// serialize absent optionals that way, and reading it as "caller supplied
    /// one" would permanently cost them the semantic arm.
    #[test]
    fn an_explicit_null_pair_is_still_enrichable() {
        let body = search_call(json!({
            "query_text": "login",
            "query_embedding": serde_json::Value::Null,
            "query_embedding_model": serde_json::Value::Null,
        }));
        match classify_memory_search(&body) {
            MemorySearchShape::Enrichable {
                query_text,
                needs_cleanup,
            } => {
                assert_eq!(query_text, "login");
                assert!(needs_cleanup, "null halves must be stripped on a degrade");
            }
            _ => panic!("an explicit null pair must still be enrichable"),
        }
    }

    /// THE fail-open case the previous round missed: a caller who left a
    /// half-pair behind AND an embedder that cannot answer. Forwarding those
    /// bytes "untouched" would make coord refuse the request outright — so the
    /// degrade path must hand back a CLEANED body, not the original.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_degrade_strips_a_half_pair_so_coord_still_accepts_the_search() {
        let client =
            crate::database::embedding_client::EmbeddingClient::with_url("http://127.0.0.1:1/none");

        for args in [
            json!({"query_text": "login", "query_embedding": serde_json::Value::Null}),
            json!({"query_text": "login", "query_embedding_model": "other@v9"}),
            json!({"query_text": "login",
                   "query_embedding": serde_json::Value::Null,
                   "query_embedding_model": serde_json::Value::Null}),
        ] {
            let body = serde_json::to_vec(&search_call(args.clone())).unwrap();
            let out = super::enrich_memory_search_body_with(&body, Some(&client))
                .await
                .unwrap_or_else(|| {
                    panic!("a half-pair body must be CLEANED on degrade, not forwarded: {args}")
                });

            let sent: serde_json::Value = serde_json::from_slice(&out).unwrap();
            let sent_args = &sent["params"]["arguments"];
            assert!(
                sent_args.get("query_embedding").is_none()
                    && sent_args.get("query_embedding_model").is_none(),
                "both halves must be gone so coord accepts the body: {sent_args}"
            );
            assert_eq!(
                sent_args["query_text"], "login",
                "the query itself must survive — this is still a search"
            );
        }
    }

    /// A clean body on a degrade path is forwarded byte-identical: there is
    /// nothing to strip, so nothing may be rewritten.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_degrade_on_a_clean_body_forwards_the_original_bytes() {
        let client =
            crate::database::embedding_client::EmbeddingClient::with_url("http://127.0.0.1:1/none");
        let body = serde_json::to_vec(&search_call(json!({"query_text": "login"}))).unwrap();
        assert!(
            super::enrich_memory_search_body_with(&body, Some(&client))
                .await
                .is_none(),
            "nothing to clean ⇒ forward the original bytes"
        );
    }

    /// Read side and write side must agree on the slot. Reading by position in
    /// `ALL` while writing via `idx()` mislabels every series the moment `ALL`
    /// is reordered — and every other test still passes.
    #[test]
    fn all_labels_read_back_their_own_slot() {
        for (i, outcome) in MemoryEnrichOutcome::ALL.iter().enumerate() {
            assert_eq!(
                outcome.idx(),
                i,
                "ALL[{i}] ({}) must sit at its own idx()",
                outcome.label()
            );
        }
    }

    /// The same fail-open path covers non-search traffic without ever dialing
    /// the embedder at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn non_search_traffic_is_returned_untouched() {
        let client =
            crate::database::embedding_client::EmbeddingClient::with_url("http://127.0.0.1:1/none");
        let body = serde_json::to_vec(&json!({
            "jsonrpc":"2.0","id":1,"method":"tools/list"
        }))
        .unwrap();
        assert!(super::enrich_memory_search_body_with(&body, Some(&client))
            .await
            .is_none());
    }

    /// (e) The budget must stay far under the embedding client's OWN 30 s
    /// ceiling, or a wedged service stalls the search instead of losing its
    /// semantic arm.
    #[test]
    fn the_embed_budget_is_far_below_the_clients_own_timeout() {
        assert!(
            MEMORY_EMBED_TIMEOUT < std::time::Duration::from_secs(1),
            "a sub-second ceiling is the whole point: EmbeddingClient's own \
             timeout is 30s, which would stall a search"
        );
    }

    /// A missing series reads as "this outcome never happens", which is the
    /// ambiguity the counters exist to remove.
    #[test]
    fn health_snapshot_renders_every_series() {
        let snap = memory_enrich_health_snapshot();
        let obj = snap.as_object().expect("snapshot must be an object");
        for outcome in MemoryEnrichOutcome::ALL {
            assert!(
                obj.contains_key(outcome.label()),
                "missing series: {}",
                outcome.label()
            );
        }
        assert_eq!(obj.len(), MemoryEnrichOutcome::ALL.len());
    }

    #[test]
    fn every_outcome_has_a_distinct_label() {
        let mut labels: Vec<_> = MemoryEnrichOutcome::ALL.iter().map(|o| o.label()).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(before, labels.len(), "outcome labels must be distinct");
    }

    /// Small helper so the non-trigger table above stays readable.
    trait WithTool {
        fn clone_with_tool(&self, name: &str) -> serde_json::Value;
    }
    impl WithTool for serde_json::Value {
        fn clone_with_tool(&self, name: &str) -> serde_json::Value {
            let mut v = self.clone();
            v["params"]["name"] = serde_json::Value::String(name.to_string());
            v
        }
    }
}

#[cfg(test)]
mod coord_mcp_body_gate_tests {
    use super::{
        coord_mcp_body_gate, coord_mcp_filter_tools_list_response, coord_mcp_tool_is_allowed,
        COORD_MCP_ALLOWED_METHODS, COORD_MCP_ALLOWED_TOOLS,
    };

    fn gate(v: serde_json::Value) -> Result<(), (serde_json::Value, String)> {
        coord_mcp_body_gate(v.to_string().as_bytes()).map_err(|r| (r.id, r.message))
    }

    /// Both membership tables are sorted — `binary_search` correctness.
    #[test]
    fn allowlist_tables_are_sorted() {
        assert!(
            COORD_MCP_ALLOWED_METHODS.windows(2).all(|w| w[0] < w[1]),
            "COORD_MCP_ALLOWED_METHODS must be sorted + deduped"
        );
        assert!(
            COORD_MCP_ALLOWED_TOOLS.windows(2).all(|w| w[0] < w[1]),
            "COORD_MCP_ALLOWED_TOOLS must be sorted + deduped"
        );
    }

    /// The MCP handshake + the legitimate coordination surface forwards.
    #[test]
    fn allows_handshake_and_coordination_tools() {
        for method in [
            "initialize",
            "tools/list",
            "ping",
            "notifications/initialized",
        ] {
            assert!(
                gate(serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":{}}))
                    .is_ok(),
                "{method} must pass the gate"
            );
        }
        for tool in [
            "coord_declare_intent",
            "coord_who_is_working_on",
            "coord_work_unit_upsert",
            "coord_work_unit_transition",
            "coord_register_gate",
            "coord_attest_gate",
            "coord_orient",
            "coord_report_status",
            "coord_conflict_check",
            "coord_blockers",
            "coord_post_finding",
            "coord_send_message",
            "coord_query_health", // prefix family
            // Policy authorship. Pinned here because this proxy previously
            // withheld it while coord's own device/agent grant allowed it —
            // a silent capability subtraction that made every device session
            // unable to close a POLICY_GAP. Non-loosening is enforced
            // coord-side, so re-excluding it here would re-open that gap
            // rather than restore a gate.
            "coord_write_prompt_document",
            // The review-gate disposition read. Same class as the line above:
            // withholding this read-only fold did not add a gate, it made one
            // unreachable — a session that cannot read the registry cannot tell
            // a recorded `degrade` from a `degrade` it assumed because the read
            // failed, so `pre-pr-review` silently collapsed to an author
            // reviewing their own diff for every device/agent session. Reading
            // the registry confers no authority to spawn; that stays with
            // `agent-spawn-authorization`.
            "coord_agent_registry_effective",
            // The corpus-orientation read. THIRD instance of the same silent
            // capability subtraction as the two above: it landed in coord's
            // device grant (`DEVICE_DEFAULT_TOOLS`) and deployed, but this
            // list was not updated, so every device session could SEE it in
            // `tools/list` and got -32601 on use. Blind `coord_memory_search`
            // is exactly what it exists to prevent, so withholding it here
            // re-opened the gap it closed.
            "coord_memory_overview",
        ] {
            assert!(
                gate(serde_json::json!({
                    "jsonrpc":"2.0","id":2,"method":"tools/call",
                    "params":{"name":tool,"arguments":{}}
                }))
                .is_ok(),
                "{tool} must pass the gate"
            );
        }
    }

    /// Build a `tools/list` response advertising `names`.
    fn tools_list_response(names: &[&str]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": names
                    .iter()
                    .map(|n| serde_json::json!({"name": n, "description": "x"}))
                    .collect::<Vec<_>>()
            }
        }))
        .unwrap()
    }

    const TOOLS_LIST_REQ: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;

    /// Names surviving the filter, plus the names it reported removing.
    fn filtered_names(request: &str, response: &[u8]) -> Option<(Vec<String>, Vec<String>)> {
        let (out, removed) = coord_mcp_filter_tools_list_response(request.as_bytes(), response)?;
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let kept = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        Some((kept, removed))
    }

    /// THE REGRESSION. `tools/list` must not advertise a tool `tools/call`
    /// would refuse — the two gates share `coord_mcp_tool_is_allowed`, so a
    /// tool is visible if and only if it is callable.
    #[test]
    fn tools_list_never_advertises_an_uncallable_tool() {
        let advertised = [
            "coord_orient",          // allowlisted
            "coord_memory_overview", // allowlisted (the 2026-08-10 fix)
            "coord_query_health",    // prefix family
            "coord_request_merge",   // privileged — must be hidden
            "coord_create_pr",       // privileged — must be hidden
            "totally_made_up_tool",  // unknown — must be hidden
        ];
        let (names, removed) = filtered_names(TOOLS_LIST_REQ, &tools_list_response(&advertised))
            .expect("privileged tools were advertised, so something must be removed");
        assert_eq!(
            names,
            vec![
                "coord_orient",
                "coord_memory_overview",
                "coord_query_health"
            ]
        );
        // The withheld names are reported so the caller can log them by name.
        assert_eq!(
            removed,
            vec![
                "coord_request_merge",
                "coord_create_pr",
                "totally_made_up_tool"
            ]
        );
        // The property, stated directly: everything still listed passes the
        // very gate that guards `tools/call`.
        for n in &names {
            assert!(
                gate(serde_json::json!({
                    "jsonrpc":"2.0","id":1,"method":"tools/call",
                    "params":{"name":n,"arguments":{}}
                }))
                .is_ok(),
                "{n} is listed but would be refused on call"
            );
        }
    }

    /// Nothing to remove → `None` → the upstream bytes are forwarded
    /// byte-identically. Re-serialising a clean response would be risk for no
    /// gain, and this is the common case.
    #[test]
    fn tools_list_untouched_when_every_tool_is_allowed() {
        let clean = tools_list_response(&["coord_orient", "coord_query_health"]);
        assert!(coord_mcp_filter_tools_list_response(TOOLS_LIST_REQ.as_bytes(), &clean).is_none());
    }

    /// The filter is scoped to `tools/list`. A different method carrying a
    /// `result.tools`-shaped payload must pass through untouched — the filter
    /// must not become a general response rewriter.
    #[test]
    fn filter_only_applies_to_tools_list_requests() {
        let resp = tools_list_response(&["coord_request_merge"]);
        for req in [
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"coord_orient"}}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            "not json at all",
            // The literal "tools/list" inside an ARGUMENT must not be mistaken
            // for the method — the cheap substring pre-check in
            // `coord_mcp_request_is_tools_list` hits here, so this pins that it
            // still falls through to the real parse and answers on `method`.
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"coord_send_message","arguments":{"body":"see tools/list output"}}}"#,
        ] {
            assert!(
                coord_mcp_filter_tools_list_response(req.as_bytes(), &resp).is_none(),
                "{req} must not trigger response filtering"
            );
        }
    }

    /// Batch bodies: a `tools/list` anywhere in the batch filters every
    /// element that carries `result.tools`.
    #[test]
    fn filter_handles_batch_bodies() {
        let req = format!("[{TOOLS_LIST_REQ}]");
        let resp = serde_json::to_vec(&serde_json::json!([{
            "jsonrpc":"2.0","id":1,
            "result":{"tools":[{"name":"coord_orient"},{"name":"coord_create_pr"}]}
        }]))
        .unwrap();
        let (out, _removed) =
            coord_mcp_filter_tools_list_response(req.as_bytes(), &resp).expect("filtered");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let tools = v[0]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "coord_orient");
    }

    /// A malformed entry with no string `name` is uncallable by construction
    /// (the gate needs a name), so it is removed rather than advertised.
    #[test]
    fn filter_drops_entries_without_a_name() {
        let resp = serde_json::to_vec(&serde_json::json!({
            "jsonrpc":"2.0","id":1,
            "result":{"tools":[{"name":"coord_orient"},{"description":"nameless"}]}
        }))
        .unwrap();
        let (names, removed) =
            filtered_names(TOOLS_LIST_REQ, &resp).expect("the nameless entry is removed");
        assert_eq!(names, vec!["coord_orient"]);
        assert_eq!(removed, vec!["(unnamed)"]);
    }

    /// A response that is not JSON, or has no `result.tools`, is forwarded
    /// untouched — the filter never turns an upstream body into an error.
    #[test]
    fn filter_tolerates_unparseable_and_shapeless_responses() {
        for resp in [
            b"<html>gateway error</html>".as_slice(),
            br#"{"jsonrpc":"2.0","id":1,"result":{}}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#.as_slice(),
        ] {
            assert!(
                coord_mcp_filter_tools_list_response(TOOLS_LIST_REQ.as_bytes(), resp).is_none(),
                "unparseable/shapeless responses must forward untouched"
            );
        }
    }

    /// Non-allowlisted tools are refused with the request's id echoed —
    /// including the deliberately-excluded privileged families.
    #[test]
    fn rejects_privileged_and_unknown_tools() {
        for tool in [
            "coord_onboard_enroll_installation",
            "coord_attest_escalate_override",
            "coord_request_merge",
            "coord_cancel_merge",
            "coord_create_pr",
            "coord_push_to_branch",
            "coord_migration_reserve",
            "coord_request_policy",
            "coord_flag_state",
            "totally_made_up_tool",
        ] {
            let (id, msg) = gate(serde_json::json!({
                "jsonrpc":"2.0","id":7,"method":"tools/call",
                "params":{"name":tool,"arguments":{}}
            }))
            .expect_err(&format!("{tool} must be refused"));
            assert_eq!(id, serde_json::json!(7), "the request id is echoed");
            assert!(msg.contains(tool), "message names the refused tool: {msg}");
            assert!(!coord_mcp_tool_is_allowed(tool));
        }
    }

    /// Non-allowlisted METHODS are refused — the generic-passthrough hole this
    /// gate closes (resources/prompts/logging/arbitrary methods).
    #[test]
    fn rejects_non_allowlisted_methods_and_malformed_bodies() {
        for method in [
            "resources/read",
            "prompts/get",
            "logging/setLevel",
            "shutdown",
        ] {
            assert!(
                gate(serde_json::json!({"jsonrpc":"2.0","id":1,"method":method})).is_err(),
                "{method} must be refused"
            );
        }
        // No method at all.
        assert!(gate(serde_json::json!({"jsonrpc":"2.0","id":1})).is_err());
        // tools/call with no tool name.
        assert!(gate(
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}})
        )
        .is_err());
        // Not JSON at all.
        let err = coord_mcp_body_gate(b"not json {").unwrap_err();
        assert_eq!(err.id, serde_json::Value::Null);
    }

    /// A batch is validated per element: one disallowed element rejects the
    /// whole request (and an empty batch is refused outright).
    #[test]
    fn batch_bodies_are_validated_per_element() {
        assert!(gate(serde_json::json!([
            {"jsonrpc":"2.0","id":1,"method":"tools/list"},
            {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"coord_orient"}},
        ]))
        .is_ok());
        let (id, _) = gate(serde_json::json!([
            {"jsonrpc":"2.0","id":1,"method":"tools/list"},
            {"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"coord_request_merge"}},
        ]))
        .unwrap_err();
        assert_eq!(
            id,
            serde_json::json!(9),
            "the offending element's id is echoed"
        );
        assert!(gate(serde_json::json!([])).is_err(), "empty batch refused");
    }
}

#[cfg(test)]
mod coord_claims_proxy_tests {
    use super::{
        claims_upstream_url, coord_claims_by_resource_handler, coord_claims_list_handler,
        coord_work_unit_deps_get_handler, forward_claims_get, ClaimsReadTarget,
    };
    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt;

    // Concrete request paths used to hit the read routes in tests (the deps
    // read is registered under the `{slug}` template below).
    const CLAIMS_ROUTES: &[&str] = &[
        "/coord-mcp/claims/list",
        "/coord-mcp/claims/by-resource",
        "/coord-mcp/work-units/2026-07-03-some-unit/deps",
    ];

    fn claims_router() -> Router {
        Router::new()
            .route("/coord-mcp/claims/list", get(coord_claims_list_handler))
            .route(
                "/coord-mcp/claims/by-resource",
                get(coord_claims_by_resource_handler),
            )
            .route(
                "/coord-mcp/work-units/{slug}/deps",
                get(coord_work_unit_deps_get_handler),
            )
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The upstream URL is built from the closed enum's allowlisted paths,
    /// with the inbound query string appended verbatim (still
    /// percent-encoded) and no dangling `?` when there is no query.
    #[test]
    fn claims_upstream_url_allowlists_paths_and_forwards_query_verbatim() {
        assert_eq!(
            claims_upstream_url("https://coord.example.test", ClaimsReadTarget::List, None),
            "https://coord.example.test/coord/claims/list"
        );
        // Trailing slash on the base must not double up.
        assert_eq!(
            claims_upstream_url(
                "https://coord.example.test/",
                ClaimsReadTarget::ByResource,
                Some("resource=src%2Fmain.rs&repo=qontinui-runner"),
            ),
            "https://coord.example.test/coord/claims/by-resource?resource=src%2Fmain.rs&repo=qontinui-runner"
        );
        // Empty query → no dangling '?'.
        assert_eq!(
            claims_upstream_url("http://127.0.0.1:9870", ClaimsReadTarget::List, Some("")),
            "http://127.0.0.1:9870/coord/claims/list"
        );
        // Work-unit deps read: the FIXED coord route template with the
        // (already-validated) slug interpolated.
        assert_eq!(
            claims_upstream_url(
                "https://coord.example.test",
                ClaimsReadTarget::WorkUnitDeps {
                    slug: "2026-07-03-some-unit".to_string()
                },
                None,
            ),
            "https://coord.example.test/coord/work-units/2026-07-03-some-unit/deps"
        );
    }

    /// `validate()` on the read targets: the claims paths carry no dynamic
    /// segment (always Ok); the work-unit deps slug is charset-validated with
    /// the runner-originated 400 code on a bad shape — a path can never be
    /// smuggled into the fixed coord route template.
    #[test]
    fn read_target_validate_rejects_bad_slugs() {
        assert!(ClaimsReadTarget::List.validate().is_ok());
        assert!(ClaimsReadTarget::ByResource.validate().is_ok());
        assert!(ClaimsReadTarget::WorkUnitDeps {
            slug: "2026-07-03-some-unit".to_string()
        }
        .validate()
        .is_ok());
        for bad in ["../etc", "a/b", "A", "a%2f", "a.b", "a b", "", "-leading"] {
            let err = ClaimsReadTarget::WorkUnitDeps {
                slug: bad.to_string(),
            }
            .validate()
            .unwrap_err();
            assert_eq!(err.0, 400, "{bad:?} must be rejected");
            assert_eq!(err.1, "COORD_CLAIMS_PROXY_BAD_TARGET");
        }
    }

    /// Absent `X-Coord-Mcp-Proxy-Key` → 401 from the runner with the claims
    /// proxy's own error code, on EVERY forwarded read route. The gate runs
    /// before any upstream I/O, so the request is never forwarded to coord.
    #[tokio::test]
    async fn claims_routes_missing_nonce_is_401_never_forwarded() {
        for path in CLAIMS_ROUTES {
            let resp = claims_router()
                .oneshot(Request::builder().uri(*path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), 401, "{path} without a nonce must 401");
            let v = body_json(resp).await;
            assert_eq!(v["success"], false);
            assert_eq!(v["code"], "COORD_CLAIMS_PROXY_UNAUTHORIZED");
        }
    }

    /// A wrong (unregistered) nonce → 401, same code, on EVERY forwarded read
    /// route — query string present to prove the gate fires regardless.
    #[tokio::test]
    async fn claims_routes_wrong_nonce_is_401() {
        for path in CLAIMS_ROUTES {
            let resp = claims_router()
                .oneshot(
                    Request::builder()
                        .uri(format!("{path}?repo=qontinui-runner"))
                        .header("X-Coord-Mcp-Proxy-Key", "not-a-registered-nonce")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), 401, "{path} with a wrong nonce must 401");
            let v = body_json(resp).await;
            assert_eq!(v["success"], false);
            assert_eq!(v["code"], "COORD_CLAIMS_PROXY_UNAUTHORIZED");
        }
    }

    /// The forwarding leg against a local mock coord: the bearer is injected
    /// as `Authorization: Bearer <token>` per request, the query string
    /// arrives verbatim, and coord's status + body come back unreshaped —
    /// including a non-200 coord verdict.
    #[tokio::test]
    async fn forward_claims_get_injects_bearer_and_passes_through_verbatim() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/coord/claims/list",
                get(
                    |headers: axum::http::HeaderMap,
                     axum::extract::RawQuery(q): axum::extract::RawQuery| async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        axum::Json(serde_json::json!({"echo_query": q, "echo_auth": auth}))
                    },
                ),
            )
            .route(
                "/coord/claims/by-resource",
                get(|| async {
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        r#"{"detail":"tenant_not_resolved"}"#,
                    )
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let base = format!("http://{addr}");

        // Happy path: query forwarded verbatim, synthetic bearer injected.
        let url = claims_upstream_url(
            &base,
            ClaimsReadTarget::List,
            Some("repo=qontinui-runner&resource=src%2Fmain.rs"),
        );
        let resp = forward_claims_get(
            &url,
            "test-device-jwt",
            qontinui_runner_lib::profiles::CoordBaseSource::Profile,
        )
        .await;
        assert_eq!(resp.status(), 200);
        let v = body_json(resp).await;
        assert_eq!(
            v["echo_query"],
            "repo=qontinui-runner&resource=src%2Fmain.rs"
        );
        assert_eq!(v["echo_auth"], "Bearer test-device-jwt");

        // Non-200 coord verdict: status + body verbatim, not reshaped.
        let url = claims_upstream_url(&base, ClaimsReadTarget::ByResource, None);
        let resp = forward_claims_get(
            &url,
            "test-device-jwt",
            qontinui_runner_lib::profiles::CoordBaseSource::Profile,
        )
        .await;
        assert_eq!(resp.status(), 403);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"detail":"tenant_not_resolved"}"#,
            "coord's body must come back verbatim"
        );
    }

    /// Coord unreachable → 502 from the runner with the distinct upstream
    /// code, so the hook helper's fail-open path triggers cleanly.
    #[tokio::test]
    async fn forward_claims_get_unreachable_coord_is_502() {
        // Bind then drop a listener so the port actively refuses connections.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let url = claims_upstream_url(
            &format!("http://127.0.0.1:{port}"),
            ClaimsReadTarget::List,
            None,
        );
        let resp = forward_claims_get(
            &url,
            "test-device-jwt",
            qontinui_runner_lib::profiles::CoordBaseSource::DevLocalhostFallback,
        )
        .await;
        assert_eq!(resp.status(), 502);
        let v = body_json(resp).await;
        assert_eq!(v["success"], false);
        assert_eq!(v["code"], "COORD_CLAIMS_PROXY_UPSTREAM_UNREACHABLE");
        // D3 self-diagnosis fields: the exact upstream dialed + how it was
        // chosen must ride in the error body.
        assert_eq!(v["upstream_url"], url);
        assert_eq!(v["coord_base_source"], "dev_localhost_fallback");
    }
}

/// Nonce-gated device-JWT WRITE forwarder (plan
/// 2026-06-15-coord-mcp-live-token-write-forwarder, Phase 1).
///
/// Same shape as `coord_claims_proxy_tests`: the gate's 401 paths are asserted
/// through the real route handlers (missing/wrong nonce — never forwarded; the
/// 401 is produced before any upstream I/O, structurally guaranteed by
/// `coord_mcp::proxy_request_gate` running first and separately unit-tested in
/// `coord_mcp::tests`). The dynamic-segment validators and the URL builder are
/// pure functions tested directly. The forwarding leg cannot be exercised
/// end-to-end through the route (the live device bearer comes from the encrypted
/// `AuthManager` slot, not seedable in a unit test), so it is tested through the
/// `forward_coord_write_post` seam against a local mock coord with a synthetic
/// bearer — covering live-bearer injection, JSON content-type, verbatim body
/// forwarding, and verbatim status+body passthrough including non-200 verdicts.
#[cfg(test)]
mod coord_write_proxy_tests {
    use super::{
        coord_attest_gate_handler, coord_register_gate_handler,
        coord_work_unit_register_gate_handler, coord_work_unit_set_deps_handler,
        coord_work_unit_transition_handler, coord_work_unit_upsert_handler,
        forward_coord_write_post, gate_id_is_valid, slug_is_valid, write_upstream_url,
        CoordWriteTarget,
    };
    use axum::{body::Body, http::Request, routing::post, Router};
    use tower::ServiceExt;

    // Concrete route templates (axum 0.8 `{param}` form) registered in the real router.
    const WRITE_ROUTES: &[&str] = &[
        "/coord-mcp/gates/register",
        "/coord-mcp/gates/{gate_id}/attest",
        "/coord-mcp/work-units/upsert",
        "/coord-mcp/work-units/{slug}/transition",
        "/coord-mcp/work-units/{slug}/register-gate",
        "/coord-mcp/work-units/{slug}/deps",
    ];
    // Concrete request paths used to hit those routes in tests.
    const WRITE_REQUEST_PATHS: &[&str] = &[
        "/coord-mcp/gates/register",
        "/coord-mcp/gates/123e4567-e89b-12d3-a456-426614174000/attest",
        "/coord-mcp/work-units/upsert",
        "/coord-mcp/work-units/2026-07-03-some-unit/transition",
        "/coord-mcp/work-units/2026-07-03-some-unit/register-gate",
        "/coord-mcp/work-units/2026-07-03-some-unit/deps",
    ];

    fn write_router() -> Router {
        Router::new()
            .route(WRITE_ROUTES[0], post(coord_register_gate_handler))
            .route(WRITE_ROUTES[1], post(coord_attest_gate_handler))
            .route(WRITE_ROUTES[2], post(coord_work_unit_upsert_handler))
            .route(WRITE_ROUTES[3], post(coord_work_unit_transition_handler))
            .route(WRITE_ROUTES[4], post(coord_work_unit_register_gate_handler))
            .route(WRITE_ROUTES[5], post(coord_work_unit_set_deps_handler))
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The slug validator: accepts a real plan stem, rejects path-smuggling and
    /// out-of-charset shapes.
    #[test]
    fn slug_validator_accepts_plan_stems_rejects_path_smuggling() {
        assert!(slug_is_valid("2026-06-15-some-plan"));
        assert!(slug_is_valid("a"));
        assert!(slug_is_valid("plan-1"));
        // Rejections: path separators, dot-dot, uppercase, percent-encoding,
        // whitespace, empty, leading hyphen.
        assert!(!slug_is_valid("../etc"));
        assert!(!slug_is_valid("a/b"));
        assert!(!slug_is_valid("A"));
        assert!(!slug_is_valid("a%2f"));
        assert!(!slug_is_valid("a.b"));
        assert!(!slug_is_valid("a b"));
        assert!(!slug_is_valid(""));
        assert!(!slug_is_valid("-leading"));
    }

    /// The gate-id validator: accepts a canonical UUID, rejects anything else.
    #[test]
    fn gate_id_validator_accepts_uuid_rejects_non_uuid() {
        assert!(gate_id_is_valid("123e4567-e89b-12d3-a456-426614174000"));
        assert!(!gate_id_is_valid("not-a-uuid"));
        assert!(!gate_id_is_valid("123e4567-e89b-12d3-a456")); // too short
        assert!(!gate_id_is_valid("../../etc/passwd"));
        assert!(!gate_id_is_valid(""));
    }

    /// The upstream URL is built from the closed enum's FIXED route template
    /// with the validated dynamic segment interpolated — and no double slash on
    /// a trailing-slash base.
    #[test]
    fn write_upstream_url_builds_fixed_coord_routes() {
        assert_eq!(
            write_upstream_url(
                "https://coord.example.test/",
                &CoordWriteTarget::AttestGate {
                    gate_id: "123e4567-e89b-12d3-a456-426614174000".to_string()
                },
            ),
            "https://coord.example.test/coord/gates/123e4567-e89b-12d3-a456-426614174000/attest"
        );
        // Claim-anchored register (plan 2026-07-21-gate-cascade-step3-proxy-rebase
        // Phase 1b): fixed route, no dynamic segment.
        assert_eq!(
            write_upstream_url(
                "https://coord.example.test",
                &CoordWriteTarget::RegisterGate
            ),
            "https://coord.example.test/coord/gates/register-agent"
        );
        // Work-unit registry forward-list (device-session coord surface
        // hardening follow-up).
        assert_eq!(
            write_upstream_url(
                "https://coord.example.test",
                &CoordWriteTarget::WorkUnitUpsert
            ),
            "https://coord.example.test/coord/work-units/upsert"
        );
        assert_eq!(
            write_upstream_url(
                "https://coord.example.test",
                &CoordWriteTarget::WorkUnitTransition {
                    slug: "2026-07-03-some-unit".to_string()
                },
            ),
            "https://coord.example.test/coord/work-units/2026-07-03-some-unit/transition"
        );
        assert_eq!(
            write_upstream_url(
                "https://coord.example.test",
                &CoordWriteTarget::WorkUnitRegisterGate {
                    slug: "2026-07-03-some-unit".to_string()
                },
            ),
            "https://coord.example.test/coord/work-units/2026-07-03-some-unit/register-gate"
        );
        assert_eq!(
            write_upstream_url(
                "https://coord.example.test/",
                &CoordWriteTarget::WorkUnitSetDeps {
                    slug: "2026-07-03-some-unit".to_string()
                },
            ),
            "https://coord.example.test/coord/work-units/2026-07-03-some-unit/deps"
        );
    }

    /// `validate()` returns the runner-originated 400 code for a bad segment and
    /// `Ok` for a good one.
    #[test]
    fn target_validate_rejects_bad_segments() {
        assert!(CoordWriteTarget::WorkUnitRegisterGate {
            slug: "2026-07-03-some-unit".to_string()
        }
        .validate()
        .is_ok());
        let err = CoordWriteTarget::WorkUnitRegisterGate {
            slug: "../etc".to_string(),
        }
        .validate()
        .unwrap_err();
        assert_eq!(err.0, 400);
        assert_eq!(err.1, "COORD_WRITE_PROXY_BAD_TARGET");

        assert!(CoordWriteTarget::AttestGate {
            gate_id: "123e4567-e89b-12d3-a456-426614174000".to_string()
        }
        .validate()
        .is_ok());
        let err = CoordWriteTarget::AttestGate {
            gate_id: "not-a-uuid".to_string(),
        }
        .validate()
        .unwrap_err();
        assert_eq!(err.0, 400);
        assert_eq!(err.1, "COORD_WRITE_PROXY_BAD_TARGET");

        // Segment-free targets are always valid: the claim-anchored gate
        // register and the work-unit upsert both carry their payload in the
        // JSON body — no dynamic segment, no traversal surface.
        assert!(CoordWriteTarget::RegisterGate.validate().is_ok());
        assert!(CoordWriteTarget::WorkUnitUpsert.validate().is_ok());
        for target in [
            CoordWriteTarget::WorkUnitTransition {
                slug: "2026-07-03-some-unit".to_string(),
            },
            CoordWriteTarget::WorkUnitRegisterGate {
                slug: "2026-07-03-some-unit".to_string(),
            },
            CoordWriteTarget::WorkUnitSetDeps {
                slug: "2026-07-03-some-unit".to_string(),
            },
        ] {
            assert!(target.validate().is_ok(), "{target:?} must validate");
        }
        for target in [
            CoordWriteTarget::WorkUnitTransition {
                slug: "../etc".to_string(),
            },
            CoordWriteTarget::WorkUnitRegisterGate {
                slug: "a/b".to_string(),
            },
            CoordWriteTarget::WorkUnitSetDeps {
                slug: "a%2f".to_string(),
            },
        ] {
            let err = target.validate().unwrap_err();
            assert_eq!(err.0, 400);
            assert_eq!(err.1, "COORD_WRITE_PROXY_BAD_TARGET");
        }
    }

    /// Absent `X-Coord-Mcp-Proxy-Key` → 401 from the runner with the write
    /// proxy's own error code, on EVERY forwarded write route. The gate runs
    /// before any upstream I/O (and before segment validation), so nothing is
    /// forwarded.
    #[tokio::test]
    async fn write_routes_missing_nonce_is_401_never_forwarded() {
        for path in WRITE_REQUEST_PATHS {
            let resp = write_router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(*path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), 401, "{path} without a nonce must 401");
            let v = body_json(resp).await;
            assert_eq!(v["success"], false);
            assert_eq!(v["code"], "COORD_WRITE_PROXY_UNAUTHORIZED");
        }
    }

    /// A wrong (unregistered) nonce → 401, same code, on EVERY forwarded write
    /// route.
    #[tokio::test]
    async fn write_routes_wrong_nonce_is_401() {
        for path in WRITE_REQUEST_PATHS {
            let resp = write_router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(*path)
                        .header("X-Coord-Mcp-Proxy-Key", "not-a-registered-nonce")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), 401, "{path} with a wrong nonce must 401");
            let v = body_json(resp).await;
            assert_eq!(v["success"], false);
            assert_eq!(v["code"], "COORD_WRITE_PROXY_UNAUTHORIZED");
        }
    }

    /// The forwarding leg against a local mock coord: the bearer is injected as
    /// `Authorization: Bearer <token>`, the body arrives verbatim with a JSON
    /// content-type, and coord's status + body come back unreshaped — including
    /// a non-200 coord verdict.
    #[tokio::test]
    async fn forward_coord_write_post_injects_bearer_and_passes_through_verbatim() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/coord/work-units/{slug}/register-gate",
                post(
                    |headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let ct = headers
                            .get("content-type")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let body = String::from_utf8_lossy(&body).to_string();
                        axum::Json(serde_json::json!({
                            "echo_auth": auth,
                            "echo_ct": ct,
                            "echo_body": body,
                        }))
                    },
                ),
            )
            .route(
                "/coord/gates/{gate_id}/attest",
                post(|| async {
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        r#"{"detail":"tenant_not_resolved"}"#,
                    )
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let base = format!("http://{addr}");

        // Happy path: body forwarded verbatim, synthetic bearer + JSON ct injected.
        let url = write_upstream_url(
            &base,
            &CoordWriteTarget::WorkUnitRegisterGate {
                slug: "2026-07-03-some-unit".to_string(),
            },
        );
        let resp = forward_coord_write_post(
            &url,
            "test-device-jwt",
            axum::body::Bytes::from_static(br#"{"resource_key":"work-units/u"}"#),
            qontinui_runner_lib::profiles::CoordBaseSource::Profile,
        )
        .await;
        assert_eq!(resp.status(), 200);
        let v = body_json(resp).await;
        assert_eq!(v["echo_auth"], "Bearer test-device-jwt");
        assert_eq!(v["echo_ct"], "application/json");
        assert_eq!(v["echo_body"], r#"{"resource_key":"work-units/u"}"#);

        // Non-200 coord verdict: status + body verbatim, not reshaped.
        let url = write_upstream_url(
            &base,
            &CoordWriteTarget::AttestGate {
                gate_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            },
        );
        let resp = forward_coord_write_post(
            &url,
            "test-device-jwt",
            axum::body::Bytes::new(),
            qontinui_runner_lib::profiles::CoordBaseSource::Profile,
        )
        .await;
        assert_eq!(resp.status(), 403);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"detail":"tenant_not_resolved"}"#,
            "coord's body must come back verbatim"
        );
    }

    /// Coord unreachable → 502 from the runner with the distinct upstream code.
    #[tokio::test]
    async fn forward_coord_write_post_unreachable_coord_is_502() {
        // Bind then drop a listener so the port actively refuses connections.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let url = write_upstream_url(
            &format!("http://127.0.0.1:{port}"),
            &CoordWriteTarget::WorkUnitRegisterGate {
                slug: "2026-07-03-some-unit".to_string(),
            },
        );
        let resp = forward_coord_write_post(
            &url,
            "test-device-jwt",
            axum::body::Bytes::new(),
            qontinui_runner_lib::profiles::CoordBaseSource::TierDefault,
        )
        .await;
        assert_eq!(resp.status(), 502);
        let v = body_json(resp).await;
        assert_eq!(v["success"], false);
        assert_eq!(v["code"], "COORD_WRITE_PROXY_UPSTREAM_UNREACHABLE");
        // D3 self-diagnosis fields: the exact upstream dialed + how it was
        // chosen must ride in the error body.
        assert_eq!(v["upstream_url"], url);
        assert_eq!(v["coord_base_source"], "tier_default");
    }
}

/// Nonce-gated device-JWT PR-creation forwarder (plan
/// qontinui-pr-credential-provisioning, Phase 2b).
///
/// Same shape as `coord_write_proxy_tests`: the gate's 401 paths are asserted
/// through the real route handler (missing/wrong nonce — never forwarded); the
/// request-shaping helpers (`parse_owner_repo`, `vcs_pr_upstream_url`,
/// `vcs_pr_upstream_body`) are pure functions tested directly; the forwarding
/// leg is tested through the `forward_vcs_pr_post` seam against a local mock
/// coord with a synthetic bearer.
#[cfg(test)]
mod vcs_pr_proxy_tests {
    use super::{
        forward_vcs_pr_post, parse_owner_repo, vcs_create_pull_request_handler,
        vcs_pr_upstream_body, vcs_pr_upstream_url, VcsPullRequestBody,
    };
    use axum::{body::Body, http::Request, routing::post, Router};
    use tower::ServiceExt;

    fn vcs_router() -> Router {
        Router::new().route("/vcs/pull-requests", post(vcs_create_pull_request_handler))
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The owner/name validator: accepts real GitHub slugs, rejects
    /// path-smuggling and out-of-charset shapes.
    #[test]
    fn parse_owner_repo_accepts_real_slugs_rejects_path_smuggling() {
        assert_eq!(
            parse_owner_repo("qontinui/qontinui-runner"),
            Some(("qontinui", "qontinui-runner"))
        );
        assert_eq!(parse_owner_repo("a/b.c_d-e"), Some(("a", "b.c_d-e")));
        // Rejections: no slash, extra path segments, dot-dot, leading
        // punctuation, empty segments, whitespace, percent-encoding.
        assert_eq!(parse_owner_repo("no-slash"), None);
        assert_eq!(parse_owner_repo("a/b/c"), None);
        assert_eq!(parse_owner_repo("../etc/passwd"), None);
        assert_eq!(parse_owner_repo("a/.."), None);
        assert_eq!(parse_owner_repo("a/."), None);
        assert_eq!(parse_owner_repo("-lead/repo"), None);
        assert_eq!(parse_owner_repo("owner/.hidden"), None);
        assert_eq!(parse_owner_repo("/repo"), None);
        assert_eq!(parse_owner_repo("owner/"), None);
        assert_eq!(parse_owner_repo("a b/c"), None);
        assert_eq!(parse_owner_repo("a%2f/c"), None);
    }

    /// The upstream URL is the FIXED coord route template with the validated
    /// segments interpolated — no double slash on a trailing-slash base.
    #[test]
    fn vcs_pr_upstream_url_builds_fixed_coord_route() {
        assert_eq!(
            vcs_pr_upstream_url("https://coord.example.test", "qontinui", "qontinui-runner"),
            "https://coord.example.test/coord/repos/qontinui/qontinui-runner/pull-requests"
        );
        assert_eq!(
            vcs_pr_upstream_url("http://127.0.0.1:9870/", "a", "b"),
            "http://127.0.0.1:9870/coord/repos/a/b/pull-requests"
        );
    }

    /// The forwarded body is the inbound body MINUS `repo`, with absent
    /// optional fields omitted (not nulled) and present ones carried through.
    #[test]
    fn vcs_pr_upstream_body_strips_repo_and_omits_absent_options() {
        let minimal = VcsPullRequestBody {
            repo: "o/r".to_string(),
            head: "feat/x".to_string(),
            base: None,
            title: "feat: x".to_string(),
            body: None,
            draft: None,
        };
        let v = vcs_pr_upstream_body(&minimal);
        assert_eq!(v, serde_json::json!({"head": "feat/x", "title": "feat: x"}));
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("repo"), "repo travels in the URL path");
        assert!(!obj.contains_key("base") && !obj.contains_key("draft"));

        let full = VcsPullRequestBody {
            repo: "o/r".to_string(),
            head: "feat/x".to_string(),
            base: Some("main".to_string()),
            title: "feat: x".to_string(),
            body: Some("body text".to_string()),
            draft: Some(true),
        };
        assert_eq!(
            vcs_pr_upstream_body(&full),
            serde_json::json!({
                "head": "feat/x",
                "base": "main",
                "title": "feat: x",
                "body": "body text",
                "draft": true,
            })
        );
    }

    /// Absent `X-Coord-Mcp-Proxy-Key` → 401 with the route's own code, before
    /// any body parsing or upstream I/O.
    #[tokio::test]
    async fn missing_nonce_is_401_never_forwarded() {
        let resp = vcs_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/vcs/pull-requests")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"repo":"o/r","head":"h","title":"t"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
        let v = body_json(resp).await;
        assert_eq!(v["success"], false);
        assert_eq!(v["code"], "VCS_PR_PROXY_UNAUTHORIZED");
    }

    /// A wrong (unregistered) nonce → the same 401.
    #[tokio::test]
    async fn wrong_nonce_is_401() {
        let resp = vcs_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/vcs/pull-requests")
                    .header("X-Coord-Mcp-Proxy-Key", "not-a-registered-nonce")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"repo":"o/r","head":"h","title":"t"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
        let v = body_json(resp).await;
        assert_eq!(v["code"], "VCS_PR_PROXY_UNAUTHORIZED");
    }

    /// An AGENT-bound nonce is ACCEPTED by the principal gate (coord-spawned
    /// agent sessions are this feature's primary population — coord's upstream
    /// route takes agent JWTs too). With no live token slot registered for the
    /// agent (torn-down/restarted), the handler fails closed with the DISTINCT
    /// agent-gone 401 — proving it got PAST the old device-only rejection.
    #[tokio::test]
    async fn agent_nonce_passes_principal_gate_and_fails_closed_without_token_slot() {
        let agent_id = uuid::Uuid::new_v4();
        let nonce = crate::coord_mcp::register_agent_proxy_nonce(
            "/tmp/vcs-pr-proxy-agent-nonce-test",
            agent_id,
        );

        // A malformed body with a VALID agent nonce is a 400 — the shape is
        // validated before any credential lookup.
        let resp = vcs_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/vcs/pull-requests")
                    .header("X-Coord-Mcp-Proxy-Key", &nonce)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"not":"a pr body"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "agent nonce must reach body validation");
        let v = body_json(resp).await;
        assert_eq!(v["code"], "VCS_PR_PROXY_BAD_REQUEST");

        // A well-formed body proceeds to the agent-token lookup, which has no
        // slot for this agent → the distinct fail-closed 401 (NOT the generic
        // unrecognized-nonce 401, and NOT a forward).
        let resp = vcs_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/vcs/pull-requests")
                    .header("X-Coord-Mcp-Proxy-Key", &nonce)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"repo":"o/r","head":"h","title":"t"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
        let v = body_json(resp).await;
        assert_eq!(v["code"], "VCS_PR_PROXY_AGENT_GONE");
    }

    /// The forwarding leg against a local mock coord: the bearer is injected as
    /// `Authorization: Bearer <token>`, the JSON body arrives verbatim, and
    /// coord's status + body come back unreshaped — including the honest
    /// non-2xx verdicts (403 repo-not-in-tenant, 429 rate limit).
    #[tokio::test]
    async fn forward_vcs_pr_post_injects_bearer_and_passes_through_verbatim() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/coord/repos/{owner}/{repo}/pull-requests",
                post(
                    |axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
                     headers: axum::http::HeaderMap,
                     body: axum::body::Bytes| async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let body = String::from_utf8_lossy(&body).to_string();
                        (
                            axum::http::StatusCode::CREATED,
                            axum::Json(serde_json::json!({
                                "number": 42,
                                "url": format!("https://github.com/{owner}/{repo}/pull/42"),
                                "echo_auth": auth,
                                "echo_body": body,
                            })),
                        )
                    },
                ),
            )
            .route(
                "/coord/repos/other/denied/pull-requests",
                post(|| async {
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        r#"{"detail":"repo not in caller tenant"}"#,
                    )
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let base = format!("http://{addr}");

        // Happy path: 201 + coord's body verbatim, bearer injected.
        let url = vcs_pr_upstream_url(&base, "qontinui", "qontinui-runner");
        let body = serde_json::json!({"head": "feat/x", "title": "feat: x"});
        let resp = forward_vcs_pr_post(&url, "test-device-jwt", &body).await;
        assert_eq!(resp.status(), 201);
        let v = body_json(resp).await;
        assert_eq!(v["number"], 42);
        assert_eq!(
            v["url"],
            "https://github.com/qontinui/qontinui-runner/pull/42"
        );
        assert_eq!(v["echo_auth"], "Bearer test-device-jwt");
        assert_eq!(
            v["echo_body"],
            serde_json::to_string(&body).unwrap(),
            "the JSON body must arrive verbatim"
        );

        // Non-2xx coord verdict: status + body verbatim, not reshaped.
        // (The specific mock route wins over the {owner}/{repo} template.)
        let url = vcs_pr_upstream_url(&base, "other", "denied");
        let resp = forward_vcs_pr_post(&url, "test-device-jwt", &body).await;
        assert_eq!(resp.status(), 403);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"detail":"repo not in caller tenant"}"#,
            "coord's error body must come back verbatim"
        );
    }

    /// Coord unreachable → 502 from the runner with the distinct upstream code.
    #[tokio::test]
    async fn forward_vcs_pr_post_unreachable_coord_is_502() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let url = vcs_pr_upstream_url(&format!("http://127.0.0.1:{port}"), "o", "r");
        let resp =
            forward_vcs_pr_post(&url, "test-device-jwt", &serde_json::json!({"head": "h"})).await;
        assert_eq!(resp.status(), 502);
        let v = body_json(resp).await;
        assert_eq!(v["success"], false);
        assert_eq!(v["code"], "VCS_PR_PROXY_UPSTREAM_UNREACHABLE");
    }
}

#[cfg(test)]
mod pr_credential_probe_tests {
    use super::*;

    /// The kick decision: TTL-stale alone is NOT enough — the in-flight flag
    /// must also be free, so an unresolved probe is never stacked on. Runs the
    /// whole sequence in ONE test (the statics are process-global).
    #[test]
    fn probe_kick_requires_ttl_stale_and_no_in_flight_probe() {
        // Reset the process-global gate state.
        PR_CRED_LAST_KICK_MS.store(0, Ordering::Relaxed);
        pr_cred_probe_finished();

        let t1 = PR_CRED_PROBE_TTL_MS + 1;
        assert!(
            pr_cred_try_begin_probe(t1),
            "stale + no in-flight probe → kick"
        );
        assert!(
            !pr_cred_try_begin_probe(t1),
            "fresh timestamp → no kick regardless of flag"
        );

        // The TTL expires AGAIN while the probe is still unresolved: the old
        // timestamp-only gating would kick a second probe here; the in-flight
        // guard must refuse.
        let t2 = t1 + PR_CRED_PROBE_TTL_MS + 1;
        assert!(
            !pr_cred_try_begin_probe(t2),
            "in-flight probe blocks a new kick even after the TTL expires"
        );

        // Once the probe resolves and releases the flag, the stale TTL kicks.
        pr_cred_probe_finished();
        assert!(
            pr_cred_try_begin_probe(t2),
            "released flag + stale TTL → kick"
        );
        pr_cred_probe_finished();
    }

    /// The child wait helper: a fast child yields its exit status; a child that
    /// outlives the timeout is KILLED and yields `None` (→ the `unknown` probe
    /// state) instead of pinning the thread.
    #[test]
    fn wait_child_with_timeout_reaps_fast_child_and_kills_slow_child() {
        use std::process::{Command, Stdio};
        use std::time::Duration;

        #[cfg(windows)]
        let mut fast = Command::new("cmd");
        #[cfg(windows)]
        fast.args(["/C", "exit 0"]);
        #[cfg(not(windows))]
        let mut fast = Command::new("sh");
        #[cfg(not(windows))]
        fast.args(["-c", "exit 0"]);
        let mut child = fast
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn fast child");
        let status = wait_child_with_timeout(&mut child, Duration::from_secs(10))
            .expect("fast child resolves in time");
        assert!(status.success());

        // A child that would run ~30s must be killed at the (short) deadline.
        #[cfg(windows)]
        let mut slow = Command::new("ping");
        #[cfg(windows)]
        slow.args(["-n", "30", "127.0.0.1"]);
        #[cfg(not(windows))]
        let mut slow = Command::new("sleep");
        #[cfg(not(windows))]
        slow.arg("30");
        let mut child = slow
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn slow child");
        let started = std::time::Instant::now();
        let status = wait_child_with_timeout(&mut child, Duration::from_millis(500));
        assert!(status.is_none(), "slow child times out → None (unknown)");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the kill happens at the deadline, not after the child's own runtime"
        );
    }
}

#[cfg(test)]
mod panic_catcher_tests {
    use super::runner_panic_handler;
    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt;

    async fn panicking_handler() -> &'static str {
        panic!("intentional test panic");
    }

    #[tokio::test]
    async fn test_panic_caught_returns_500_json() {
        let app = Router::new().route("/boom", get(panicking_handler)).layer(
            tower_http::catch_panic::CatchPanicLayer::custom(runner_panic_handler),
        );

        let req = Request::builder().uri("/boom").body(Body::empty()).unwrap();
        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), 500);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("panicked"));
    }
}

#[cfg(test)]
mod not_found_message_tests {
    use super::not_found_message;
    use axum::http::Method;

    #[test]
    fn ui_bridge_path_appends_discovery_hint() {
        let msg = not_found_message(&Method::GET, "/ui-bridge/elements");
        // Keeps the canonical prefix...
        assert!(msg.starts_with("No route for GET /ui-bridge/elements"));
        // ...and names both discovery endpoints so the hint can't dangle.
        assert!(msg.contains("GET /ui-bridge/_routes"));
        assert!(msg.contains("GET /ui-bridge/commands"));
    }

    #[test]
    fn non_ui_bridge_path_is_unchanged() {
        let msg = not_found_message(&Method::POST, "/api/v1/whatever");
        assert_eq!(msg, "No route for POST /api/v1/whatever");
    }
}

/// Route-coverage test: every mutating route (POST/PUT/PATCH) that takes a JSON
/// body MUST return the canonical `ApiResponse` envelope on malformed input.
///
/// ## Approach: representative-subset probe router
///
/// Building the full `ApiState` in a test is infeasible — it requires a
/// `tauri::AppHandle`, `RAGState`, `InstanceManager`, and a running Tauri
/// runtime. Instead, this module builds a lightweight *probe router*: a
/// dedicated `axum::Router` with 15 representative routes whose handlers accept
/// `axum::Json<serde_json::Value>` (the same extractor family as all real
/// handlers). The middleware stack matches production exactly:
///
/// ```text
/// [envelope_rewrite_middleware]   ← same as mcp_api.rs
///   [probe handlers]              ← minimal stubs, same Json<T> extractor
/// ```
///
/// Sending a POST/PUT/PATCH with no `Content-Type` header triggers the same
/// `MissingJsonContentType` rejection that real handlers produce. The test
/// asserts that after the rewrite middleware the response is:
///   - `Content-Type: application/json`
///   - body parses as JSON with `success == false` and a non-empty `code`
///
/// All 15 routes are tested in a single pass; violations are collected and
/// reported together so the failure message lists every offending route.
///
/// The `envelope_audit_middleware` is NOT used inside this test — the test
/// itself is the collector. The audit middleware serves a different purpose:
/// it fires as a panic inside the running server when a gap is detected.
#[cfg(test)]
mod envelope_coverage_tests {
    use axum::{
        body::Body,
        http::{self, Request, StatusCode},
        middleware,
        response::Json,
        routing::{patch, post, put},
        Router,
    };
    use serde::Deserialize;
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::mcp::envelope::envelope_rewrite_middleware;

    // ── Minimal request body shared by all probe handlers ────────────────────

    #[derive(Debug, Deserialize)]
    struct ProbeBody {
        #[allow(dead_code)]
        _marker: Option<String>,
    }

    // ── Probe handler: accepts a typed JSON body, returns a trivial 200 ───────
    //
    // Using a typed `Deserialize` struct (not `serde_json::Value`) ensures that
    // missing `Content-Type` fires `MissingJsonContentType` rejection — the same
    // path as real handlers that parse named request structs.

    async fn probe_post(axum::Json(_): axum::Json<ProbeBody>) -> Json<Value> {
        Json(serde_json::json!({"success": true}))
    }

    // ── Representative route table ────────────────────────────────────────────
    //
    // 15 routes covering the major endpoint families:
    //   - UI Bridge control (error-sessions, page navigation, elements, forms)
    //   - AI session / task management
    //   - Spec / scenario authoring
    //   - Settings and config mutations
    //
    // Method is listed per route so it is verified independently if methods
    // differ. All use the same handler body (envelope behaviour is middleware-
    // level; the specific handler logic doesn't affect the 415 path).

    const PROBE_ROUTES: &[(&str, &str)] = &[
        // UI Bridge — error sessions
        ("POST", "/ui-bridge/control/error-sessions/start"),
        ("POST", "/ui-bridge/control/error-sessions/end"),
        // UI Bridge — page navigation
        ("POST", "/ui-bridge/page/navigate"),
        ("POST", "/ui-bridge/page/navigate-and-wait"),
        // UI Bridge — element interaction
        ("POST", "/ui-bridge/control/elements/find"),
        ("POST", "/ui-bridge/control/elements/click"),
        // UI Bridge — forms
        ("POST", "/ui-bridge/control/forms/fill"),
        ("POST", "/ui-bridge/control/forms/snapshot"),
        // AI session lifecycle
        ("POST", "/sessions/start"),
        ("POST", "/sessions/continue"),
        // Task runs
        ("POST", "/task-runs"),
        // Settings mutations
        ("PUT", "/settings"),
        ("PATCH", "/settings"),
        // Spec authoring
        ("POST", "/spec/pages"),
        // Scenarios
        ("POST", "/scenarios/run"),
    ];

    /// Build a probe router whose routes mirror the method+path pairs from
    /// `PROBE_ROUTES`. Every handler uses the same `Json<ProbeBody>` extractor
    /// so the 415 rejection path is exercised identically for all routes.
    fn build_probe_router() -> Router {
        let mut router = Router::new();
        for (method, path) in PROBE_ROUTES {
            let route = match *method {
                "PUT" => put(probe_post),
                "PATCH" => patch(probe_post),
                _ => post(probe_post),
            };
            router = router.route(path, route);
        }
        router.layer(middleware::from_fn(envelope_rewrite_middleware))
    }

    // ── Helper: send a request with no Content-Type ───────────────────────────

    async fn send_no_content_type(
        app: &mut Router,
        method: &str,
        path: &str,
    ) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            // Deliberately omit Content-Type — triggers 415 in Json<T> extractor.
            .body(Body::from(r#"{"_marker":"probe"}"#))
            .unwrap();

        // `oneshot` consumes the router, so we clone for each call.
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            Value::String(format!(
                "(non-JSON body: {})",
                String::from_utf8_lossy(&bytes)
            ))
        });
        (status, body)
    }

    // ── The coverage assertion ────────────────────────────────────────────────

    /// Every route in `PROBE_ROUTES` must return the canonical JSON envelope
    /// when hit with a body but no `Content-Type` header.
    ///
    /// Failures are collected across all routes before asserting, so a single
    /// run surfaces every violating route — not just the first one.
    #[tokio::test]
    async fn all_mutating_routes_return_json_envelope_on_missing_content_type() {
        let mut app = build_probe_router();
        let mut violations: Vec<String> = Vec::new();

        for &(method, path) in PROBE_ROUTES {
            let (status, body) = send_no_content_type(&mut app, method, path).await;

            // Classify each failure mode separately for clear diagnostics.
            if !status.is_client_error() {
                violations.push(format!(
                    "{} {} → expected 4xx, got {}",
                    method, path, status
                ));
                continue;
            }

            let ct = body.as_str().map(|s| s.to_owned()); // only set when body was non-JSON
            if ct.is_some() {
                violations.push(format!(
                    "{} {} → response body was not JSON: {}",
                    method, path, body
                ));
                continue;
            }

            if body["success"] != false {
                violations.push(format!(
                    "{} {} → success field was not false: {}",
                    method, path, body
                ));
            }

            let code = body["code"].as_str().unwrap_or("");
            if code.is_empty() {
                violations.push(format!(
                    "{} {} → code field missing or empty (body={})",
                    method, path, body
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "Envelope coverage failures ({} route(s) violated the canonical envelope):\n  - {}",
            violations.len(),
            violations.join("\n  - ")
        );
    }

    /// Smoke test: a request WITH the correct Content-Type + valid body succeeds
    /// (middleware must not interfere with 2xx responses).
    #[tokio::test]
    async fn valid_request_passes_through_unchanged() {
        let mut app = build_probe_router();
        let req = Request::builder()
            .method(http::Method::POST)
            .uri("/ui-bridge/page/navigate")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"_marker":"ok"}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
