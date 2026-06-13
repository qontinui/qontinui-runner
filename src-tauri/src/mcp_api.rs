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

/// Health check endpoint (also served at `/ui-bridge/health` and
/// `/ui-bridge/status` — all three share this handler).
/// Includes `uiBridge` metadata so the app discovery scanner can detect the runner.
/// Returns rich diagnostics: frontend responsiveness, uptime, circuit breaker state.
async fn health(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    let uptime_secs = state.started_at.elapsed().as_secs();
    let last_pong = state.ui_bridge_last_pong.load(Ordering::Relaxed);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let pong_age_ms = if last_pong > 0 { now_ms - last_pong } else { 0 };
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
        crate::settings::AiProvider::Ollama => "ollama",
        crate::settings::AiProvider::OpenAiCompatible => "openai_compatible",
    };
    let ai_model: Option<String> = match &ai_settings.provider {
        crate::settings::AiProvider::ClaudeCli => None, // CLI manages its own model selection
        crate::settings::AiProvider::ClaudeApi => Some(ai_settings.claude_api.model.clone()),
        crate::settings::AiProvider::GeminiCli => Some(ai_settings.gemini_cli.model.clone()),
        crate::settings::AiProvider::GeminiApi => Some(ai_settings.gemini_api.model.clone()),
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
    let derived_status = crate::ui_error::compute_derived_status(
        ui_error_snapshot.is_some(),
        recent_crash_snapshot.is_some(),
        embedding_reachable_cached(),
    );

    // One-way readiness flag flipped by the first successful UI Bridge IPC
    // response (see `mcp::ui_bridge::request::ui_bridge_request_inner`).
    // Distinguishes "Tauri shell is responsive" from "React app has actually
    // mounted past App.tsx's loading screen and is processing UI Bridge IPC".
    let frontend_ready = state.app_state.frontend_ready.load(Ordering::Relaxed);

    let mut data = serde_json::json!({
        "status": status,
        "ready": last_pong > 0,
        "responsive": responsive,
        "frontendReady": frontend_ready,
        "lastHeartbeat": last_pong,
        "heartbeatAgeMs": pong_age_ms,
        "uptimeSeconds": uptime_secs,
        "pendingRequests": pending_count,
        "circuitBreaker": format!("{:?}", circuit_breaker_state),
        "consoleErrorCount": console_errors,
        "aiProviderCircuitBreakers": ai_provider_states,
        "embeddingService": embedding_health,
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
        // Compile-time build identifier (`<git-sha-short>-<unix-ms>`) baked
        // into the binary by build.rs. Vite bakes the same value into
        // index.html as `<meta name="build-id">`. The shared
        // `useBuildIdWatcher` hook polls this endpoint and fires its
        // onBuildIdChange callback when the page's meta tag diverges from
        // the running binary's value — the only divergence vector is a
        // mid-session binary swap behind the live webview.
        "buildId": env!("RUNNER_BUILD_ID"),
        "storage": {
            "apiPort": api_port,
            "namespaceSuffix": storage_namespace_suffix,
        },
        "derived_status": derived_status,
        "ui_error": ui_error_json.clone(),
        "recent_crash": recent_crash_json.clone(),
    });

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
        // Top-level mirror of `data.buildId` so the shared
        // `useBuildIdWatcher` hook (qontinui/ui-bridge/react) can read
        // the field directly from the response root without an adapter.
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
async fn coord_mcp_proxy_handler(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let nonce = headers
        .get(crate::coord_mcp::COORD_MCP_PROXY_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // Live read — the same fresh device JWT `backend_relay` reads on every
    // use. AuthManager does filesystem I/O, so keep it off the async executor.
    let bearer =
        tokio::task::spawn_blocking(|| crate::auth::AuthManager::new().get_access_token().ok())
            .await
            .ok()
            .flatten();

    if let Err((status, msg)) =
        crate::coord_mcp::proxy_request_gate(nonce.as_deref(), bearer.as_deref())
    {
        warn!("coord-mcp proxy: {msg}");
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

    let url = crate::coord_mcp::coord_mcp_url();
    let mut req = client.post(&url).bearer_auth(&bearer).body(body.to_vec());
    for (name, value) in headers.iter() {
        let n = name.as_str();
        // Hop-by-hop / recomputed headers, plus the two we own: the nonce must
        // not leak upstream and the Authorization slot is the live bearer.
        if matches!(
            n,
            "host"
                | "content-length"
                | "connection"
                | "transfer-encoding"
                | "accept-encoding"
                | "authorization"
        ) || n == crate::coord_mcp::COORD_MCP_PROXY_KEY_HEADER
        {
            continue;
        }
        req = req.header(n, value.as_bytes());
    }

    let upstream = match req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            warn!("coord-mcp proxy: forward to {url} failed: {e}");
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord /mcp unreachable: {e}"),
                    "code": "COORD_MCP_PROXY_UPSTREAM_UNREACHABLE",
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
            warn!("coord-mcp proxy: reading coord response body failed: {e}");
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord /mcp response read failed: {e}"),
                    "code": "COORD_MCP_PROXY_UPSTREAM_READ_FAILED",
                })),
            )
                .into_response();
        }
    };
    builder
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|e| {
            warn!("coord-mcp proxy: response build failed: {e}");
            axum::http::StatusCode::BAD_GATEWAY.into_response()
        })
}

/// The two — and ONLY two — coord routes the nonce-gated claims read
/// passthrough may reach (plan 2026-06-11-claims-read-auth-hardening, Phase 2).
///
/// Deliberately a closed enum rather than a path parameter: the per-session
/// proxy nonce authenticates a *session*, not an operator, so its authority
/// must stay scoped to these two read-only claims endpoints. A generic
/// `/coord-mcp/proxy/{path}` passthrough would let a leaked nonce reach any
/// coord route with the runner's device identity — arbitrary paths must be
/// structurally impossible, not merely unrouted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaimsReadTarget {
    /// `GET {coord}/coord/claims/list`
    List,
    /// `GET {coord}/coord/claims/by-resource`
    ByResource,
}

impl ClaimsReadTarget {
    fn coord_path(self) -> &'static str {
        match self {
            ClaimsReadTarget::List => "/coord/claims/list",
            ClaimsReadTarget::ByResource => "/coord/claims/by-resource",
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

    // Live read — the same fresh device JWT the `/coord-mcp` proxy injects.
    // AuthManager does filesystem I/O, so keep it off the async executor.
    let bearer =
        tokio::task::spawn_blocking(|| crate::auth::AuthManager::new().get_access_token().ok())
            .await
            .ok()
            .flatten();

    if let Err((status, msg)) =
        crate::coord_mcp::proxy_request_gate(nonce.as_deref(), bearer.as_deref())
    {
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

    let url = claims_upstream_url(
        &crate::coord_mcp::coord_base_url(),
        target,
        raw_query.as_deref(),
    );
    forward_claims_get(&url, &bearer).await
}

/// Forward a claims read to coord and return coord's status + headers + body
/// verbatim. Split from the handler (URL + bearer as plain params, no
/// `AuthManager`/env reads) so the forwarding leg is unit-testable against a
/// local mock coord with a synthetic bearer — the live credential lives in the
/// encrypted `AuthManager` slot, which a unit test cannot seed.
async fn forward_claims_get(url: &str, bearer: &str) -> axum::response::Response {
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
            warn!("coord-mcp claims proxy: forward to {url} failed: {e}");
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord claims endpoint unreachable: {e}"),
                    "code": "COORD_CLAIMS_PROXY_UPSTREAM_UNREACHABLE",
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
            warn!("coord-mcp claims proxy: reading coord response body failed: {e}");
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord claims response read failed: {e}"),
                    "code": "COORD_CLAIMS_PROXY_UPSTREAM_READ_FAILED",
                })),
            )
                .into_response();
        }
    };
    builder
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|e| {
            warn!("coord-mcp claims proxy: response build failed: {e}");
            axum::http::StatusCode::BAD_GATEWAY.into_response()
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
        ui_bridge_last_pong: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        ui_bridge_ready: Arc::new(tokio::sync::Notify::new()),
        ui_bridge_dedup: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        ui_bridge_console_error_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        ui_bridge_render_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
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
                    &payload_str[..payload_str.len().min(200)]
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
                    &payload_str[..payload_str.len().min(200)]
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
                    &payload_str[..payload_str.len().min(200)]
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
                    &payload_str[..payload_str.len().min(200)]
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
                    &payload_str[..payload_str.len().min(200)]
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
                        &payload_str[..payload_str.len().min(200)]
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
                        &payload_str[..payload_str.len().min(200)]
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
        let last_pong = api_state.ui_bridge_last_pong.clone();
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
        // Loopback live-token proxy for coord /mcp — device-provisioned
        // sessions' `.mcp.json` points here so each MCP request carries a
        // freshly-read device JWT instead of a 4h-TTL snapshot. Nonce-gated
        // (X-Coord-Mcp-Proxy-Key); see `coord_mcp_proxy_handler`.
        .route("/coord-mcp", post(coord_mcp_proxy_handler))
        // Nonce-gated claims READ passthrough for device sessions' hook
        // helper + skill wait-poll (plan 2026-06-11-claims-read-auth-hardening
        // Phase 2). Same gate + live device-JWT injection as /coord-mcp, but
        // allowlisted to EXACTLY these two read-only coord routes — never a
        // generic path passthrough (see `ClaimsReadTarget`).
        .route("/coord-mcp/claims/list", get(coord_claims_list_handler))
        .route(
            "/coord-mcp/claims/by-resource",
            get(coord_claims_by_resource_handler),
        )
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
        .merge(crate::mcp::completion_reports::routes())
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
        .merge(crate::mcp::testing::routes())
        .merge(crate::mcp::triggers::routes())
        .merge(crate::mcp::ui_bridge::routes())
        .merge(crate::mcp::ui_bridge_integration::routes())
        .merge(crate::mcp::unified_workflows::routes())
        .merge(crate::mcp::verification_tests::routes())
        .merge(crate::mcp::websocket::routes())
        .merge(crate::mcp::window_manager::routes())
        .merge(crate::mcp::worktrees::routes())
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
    //     [envelope_audit_middleware]  ← debug-only: panics if error is non-JSON
    //       [envelope_rewrite_middleware] ← rewrites 4xx text/plain → JSON envelope
    //         [TraceLayer, CORS, BodyLimit, ...]
    //           [handlers]
    //
    // The panic layer must remain outermost so it can catch panics that
    // originate in any layer below, including the audit and envelope layers.
    // The audit layer sits INSIDE CatchPanicLayer (panics are caught → 500
    // JSON) and OUTSIDE envelope_rewrite (observes the post-rewrite response).
    // A non-JSON error response after the rewrite is a definitive handler bug
    // surfaced as a panic in debug builds; compiles away entirely in release.
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
mod coord_claims_proxy_tests {
    use super::{
        claims_upstream_url, coord_claims_by_resource_handler, coord_claims_list_handler,
        forward_claims_get, ClaimsReadTarget,
    };
    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt;

    const CLAIMS_ROUTES: &[&str] = &["/coord-mcp/claims/list", "/coord-mcp/claims/by-resource"];

    fn claims_router() -> Router {
        Router::new()
            .route("/coord-mcp/claims/list", get(coord_claims_list_handler))
            .route(
                "/coord-mcp/claims/by-resource",
                get(coord_claims_by_resource_handler),
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
    }

    /// Absent `X-Coord-Mcp-Proxy-Key` → 401 from the runner with the claims
    /// proxy's own error code, on BOTH routes. The gate runs before any
    /// upstream I/O, so the request is never forwarded to coord.
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

    /// A wrong (unregistered) nonce → 401, same code, on BOTH routes —
    /// query string present to prove the gate fires regardless.
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
        let resp = forward_claims_get(&url, "test-device-jwt").await;
        assert_eq!(resp.status(), 200);
        let v = body_json(resp).await;
        assert_eq!(
            v["echo_query"],
            "repo=qontinui-runner&resource=src%2Fmain.rs"
        );
        assert_eq!(v["echo_auth"], "Bearer test-device-jwt");

        // Non-200 coord verdict: status + body verbatim, not reshaped.
        let url = claims_upstream_url(&base, ClaimsReadTarget::ByResource, None);
        let resp = forward_claims_get(&url, "test-device-jwt").await;
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
        let resp = forward_claims_get(&url, "test-device-jwt").await;
        assert_eq!(resp.status(), 502);
        let v = body_json(resp).await;
        assert_eq!(v["success"], false);
        assert_eq!(v["code"], "COORD_CLAIMS_PROXY_UPSTREAM_UNREACHABLE");
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
