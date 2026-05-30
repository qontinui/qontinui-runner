//! Central registry for internal-service endpoint URLs.
//!
//! Resolution priority for each getter: ENV VAR override → compile-time default.
//! All getters return owned `String` for caller convenience. Call these instead
//! of hardcoding `"http://localhost:N"` anywhere in the Rust backend.
//!
//! # Recognized environment variables
//!
//! | Variable                    | Service                                | Default                                  |
//! |-----------------------------|----------------------------------------|------------------------------------------|
//! | `QONTINUI_WEB_BACKEND_URL`  | qontinui-web FastAPI backend (override)| (falls through to `QONTINUI_API_URL`)    |
//! | `QONTINUI_API_URL`          | qontinui-web FastAPI backend           | `http://127.0.0.1:8000` (debug) / prod   |
//! | `QONTINUI_RUNNER_API_URL`   | This runner's MCP HTTP API             | `http://127.0.0.1:{actual_port}`         |
//! | `QONTINUI_PORT`             | Bootstrap port for runner MCP API      | `9876`                                   |
//! | `QONTINUI_SUPERVISOR_URL`   | Supervisor HTTP API                    | `http://127.0.0.1:9875`                  |
//! | `TAURI_DEV_SERVER_URL`      | Tauri/Vite dev server (debug only)     | `http://localhost:1420`                  |
//!
//! Other internal services (OTel collector, embedding service, local AI
//! providers like vLLM/Gemma/Ollama, PRM service) are configured through their
//! own settings structs and are intentionally NOT routed through this module.

/// Default supervisor HTTP port (per `proj_arch_supervisor_test_login`).
pub const DEFAULT_SUPERVISOR_PORT: u16 = 9875;

/// Default Tauri dev server (Vite) port for debug builds.
pub const DEFAULT_TAURI_DEV_PORT: u16 = 1420;

/// Default qontinui-web FastAPI backend port.
pub const DEFAULT_BACKEND_PORT: u16 = 8000;

/// Canonical Qontinui production backend FQDN. Single source of truth for
/// `get_api_base_url` and `settings::default_web_integration_backend_url`.
pub const PROD_API_BASE_URL: &str = "https://api.qontinui.io";

/// Canonical Qontinui production web-frontend FQDN (the Next.js app on Vercel).
///
/// Production is a **split** deployment: `PROD_API_BASE_URL` (`api.qontinui.io`)
/// serves only `/api/v1/*`, while user-facing pages like `/login` and
/// `/connect-runner` are served by the web frontend at `qontinui.io`. The `api.`
/// host has no login page, so any UI that sends the user "to log in" must target
/// this origin, not the backend. See [`derive_web_base_url`].
pub const PROD_WEB_BASE_URL: &str = "https://qontinui.io";

/// Derive the user-facing web-frontend origin from an API `backend_url`.
///
/// Production splits the API (`https://api.qontinui.io`) from the web frontend
/// (`https://qontinui.io`) — the only difference is the leading `api.` host
/// label. When the backend host begins with `api.`, this strips that single
/// label (preserving scheme and port) to yield the frontend origin. For any
/// other host — localhost dev, a bare IP, or a unified deployment where one
/// origin serves both — the backend URL is returned unchanged, so the
/// long-standing "same origin serves the SPA" fallback and any explicit
/// `web_base_url` override keep working.
///
/// The returned value has no trailing slash and no path.
pub fn derive_web_base_url(backend_url: &str) -> String {
    let trimmed = backend_url.trim().trim_end_matches('/');
    // Canonical mapping: the production API host maps to the production web
    // origin. The general `api.`-stripping below also yields this, but pinning
    // it keeps the two constants coupled even if the web FQDN ever diverges
    // from a simple label strip.
    if trimmed == PROD_API_BASE_URL {
        return PROD_WEB_BASE_URL.to_string();
    }
    let (scheme, rest) = match trimmed.split_once("://") {
        Some(parts) => parts,
        None => return trimmed.to_string(),
    };
    // Authority is everything up to the first '/'; a ':' splits off the port.
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    match host.strip_prefix("api.") {
        // Only rewrite when an `api.` label was actually present.
        Some(frontend_host) => match port {
            Some(p) => format!("{}://{}:{}", scheme, frontend_host, p),
            None => format!("{}://{}", scheme, frontend_host),
        },
        None => trimmed.to_string(),
    }
}

/// Get API base URL for qontinui-web backend.
///
/// This is the SINGLE source of truth for the web-backend base across every
/// runner subsystem (auth, workflow-sync, heartbeat, task-sync, …). Previously
/// `heartbeat.rs` honored `QONTINUI_WEB_BACKEND_URL` while workflow-sync only
/// honored `QONTINUI_API_URL`, so the two could resolve to different hosts and
/// silently diverge (one path 401'ing against the wrong backend). Folding both
/// vars in here guarantees every caller resolves to the same host.
///
/// Resolution order:
/// 1. `QONTINUI_WEB_BACKEND_URL` environment variable (web-integration override)
/// 2. `QONTINUI_API_URL` environment variable
/// 3. Debug builds: `http://127.0.0.1:8000` (IPv4 — backend only binds IPv4,
///    localhost may resolve to IPv6 `::1` first)
/// 4. Release builds: `PROD_API_BASE_URL`
///
/// A trailing slash is trimmed so callers can safely `format!("{base}/api/...")`.
pub fn get_api_base_url() -> String {
    let raw = std::env::var("QONTINUI_WEB_BACKEND_URL")
        .or_else(|_| std::env::var("QONTINUI_API_URL"))
        .unwrap_or_else(|_| {
            if cfg!(debug_assertions) {
                format!("http://127.0.0.1:{}", DEFAULT_BACKEND_PORT)
            } else {
                PROD_API_BASE_URL.to_string()
            }
        });
    raw.trim().trim_end_matches('/').to_string()
}

/// MCP API base URL for the runner's own HTTP server.
///
/// Resolution order:
/// 1. `QONTINUI_RUNNER_API_URL` environment variable (if set)
/// 2. `http://127.0.0.1:{port}` where `port` comes from
///    [`crate::mcp::types::get_mcp_api_port`] (`QONTINUI_PORT` env var, then
///    the `MCP_API_PORT` constant fallback).
///
/// Note: callers that have an `AppState` should prefer
/// [`crate::mcp::types::get_self_base_url`], which reads the actually-bound
/// port from `app_state.api_port` (an `AtomicU16` set at bind time). This
/// getter is for paths without `AppState` access (e.g. helper modules,
/// pre-bind probes).
pub fn get_runner_api_url() -> String {
    if let Ok(url) = std::env::var("QONTINUI_RUNNER_API_URL") {
        return url.trim_end_matches('/').to_string();
    }
    crate::mcp::types::get_self_base_url_from_env()
}

/// Supervisor HTTP API base URL.
///
/// Resolution order:
/// 1. `QONTINUI_SUPERVISOR_URL` environment variable (if set)
/// 2. `http://127.0.0.1:9875`
pub fn get_supervisor_url() -> String {
    std::env::var("QONTINUI_SUPERVISOR_URL")
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", DEFAULT_SUPERVISOR_PORT))
}

/// Supervisor TCP socket address (`host:port`) for raw connect probes.
/// Best-effort parses [`get_supervisor_url`]; falls back to
/// `127.0.0.1:{DEFAULT_SUPERVISOR_PORT}` if the URL can't be parsed.
pub fn get_supervisor_socket_addr() -> String {
    let url = get_supervisor_url();
    // Strip scheme and any path.
    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or(url.as_str());
    let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("127.0.0.1:{}", DEFAULT_SUPERVISOR_PORT)
    }
}

/// Tauri dev server (Vite) URL — dev builds only. Returns `None` in release.
///
/// Resolution order (debug builds):
/// 1. `TAURI_DEV_SERVER_URL` environment variable (set by Tauri at build time)
/// 2. `http://localhost:1420`
pub fn get_tauri_dev_server_url() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    Some(
        std::env::var("TAURI_DEV_SERVER_URL")
            .unwrap_or_else(|_| format!("http://localhost:{}", DEFAULT_TAURI_DEV_PORT)),
    )
}

/// IPC response callback URL used by JS snippets the backend injects into
/// the WebView. Same host/port as the runner MCP API.
pub fn get_ipc_response_url() -> String {
    format!("{}/ui-bridge/ipc-response", get_runner_api_url())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_url_uses_default_port() {
        // We can't reliably clear env in a multi-test process, so just assert
        // the default port appears in the fallback path.
        std::env::remove_var("QONTINUI_SUPERVISOR_URL");
        let url = get_supervisor_url();
        assert!(
            url.contains(&DEFAULT_SUPERVISOR_PORT.to_string()),
            "supervisor URL should contain default port: {}",
            url
        );
    }

    #[test]
    fn ipc_response_url_appends_path() {
        let url = get_ipc_response_url();
        assert!(
            url.ends_with("/ui-bridge/ipc-response"),
            "ipc response url should end with /ui-bridge/ipc-response: {}",
            url
        );
    }

    #[test]
    fn tauri_dev_server_url_only_in_debug() {
        let url = get_tauri_dev_server_url();
        if cfg!(debug_assertions) {
            assert!(url.is_some());
        } else {
            assert!(url.is_none());
        }
    }

    /// Phase 9 calibration: lock in the canonical production backend URL.
    /// `PROD_API_BASE_URL` is the single source of truth used by both
    /// `get_api_base_url` (auth endpoints) and
    /// `settings::default_web_integration_backend_url` (WS relay default).
    /// A drift between the two surfaces is exactly the Phase 6 defect this
    /// constant was introduced to prevent — see plans/2026-05-20-runner-
    /// tier-decoupling.md.
    #[test]
    fn prod_api_base_url_is_canonical() {
        assert_eq!(PROD_API_BASE_URL, "https://api.qontinui.io");
    }

    #[test]
    fn derive_web_base_url_strips_prod_api_label() {
        // The headline case: api.qontinui.io (no login page) → qontinui.io.
        assert_eq!(derive_web_base_url(PROD_API_BASE_URL), PROD_WEB_BASE_URL);
        assert_eq!(
            derive_web_base_url("https://api.qontinui.io/"),
            "https://qontinui.io"
        );
    }

    #[test]
    fn derive_web_base_url_preserves_non_api_hosts() {
        // Localhost dev + unified deployments have no `api.` label to strip,
        // so the backend origin is returned unchanged (old fallback behavior).
        assert_eq!(
            derive_web_base_url("http://localhost:8000"),
            "http://localhost:8000"
        );
        assert_eq!(
            derive_web_base_url("http://127.0.0.1:8000/"),
            "http://127.0.0.1:8000"
        );
        assert_eq!(
            derive_web_base_url("https://qontinui.io"),
            "https://qontinui.io"
        );
    }

    #[test]
    fn derive_web_base_url_preserves_scheme_and_port() {
        assert_eq!(
            derive_web_base_url("http://api.example.test:8080"),
            "http://example.test:8080"
        );
        // A host that merely starts with the letters "api" but has no `api.`
        // label must NOT be rewritten.
        assert_eq!(
            derive_web_base_url("https://apiserver.example.test"),
            "https://apiserver.example.test"
        );
    }
}
