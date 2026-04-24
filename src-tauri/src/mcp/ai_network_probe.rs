//! POST /ui-bridge/ai/network-probe — server-side HTTP probe for tests that
//! need to inspect backend state independent of the frontend's fetch lifecycle.
//!
//! Restricted to loopback destinations — rejects anything whose host isn't
//! `127.0.0.1`, `::1`, `localhost`, or a `.local` name. This is a test/diagnostic
//! shim, not a generic HTTP proxy.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{extract::State, http::StatusCode, response::Json, routing::post, Router};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

const HARD_TIMEOUT_CEILING_MS: u64 = 10_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkProbeRequest {
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub body: Option<String>,
}

fn default_method() -> String {
    "GET".to_string()
}

fn default_timeout_ms() -> u64 {
    2_000
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkProbeResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ============================================================================
// Loopback-only validation
// ============================================================================

/// Return `true` if the host is a loopback address (`127.0.0.0/8`, `::1`),
/// the literal `localhost`, or a `.local` mDNS name. Rejects everything else.
fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if host.to_ascii_lowercase().ends_with(".local") {
        return true;
    }
    // Try to parse as an IP (strip brackets for IPv6).
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    false
}

// ============================================================================
// Handler
// ============================================================================

pub async fn ai_network_probe_handler(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<NetworkProbeRequest>,
) -> Result<Json<ApiResponse<NetworkProbeResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let start = Instant::now();

    // Validate timeout
    if req.timeout_ms == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("timeoutMs must be > 0")),
        ));
    }
    let effective_timeout_ms = req.timeout_ms.min(HARD_TIMEOUT_CEILING_MS);

    // Parse URL
    let parsed = match url::Url::parse(&req.url) {
        Ok(u) => u,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("invalid url '{}': {}", req.url, e))),
            ));
        }
    };

    // Only http/https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "unsupported scheme '{}' — only http/https are allowed",
                    other
                ))),
            ));
        }
    }

    // Loopback-only gate
    let host = parsed.host_str().unwrap_or("");
    if !is_loopback_host(host) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "host '{}' is not loopback — only 127.0.0.1 / ::1 / localhost / *.local are allowed",
                host
            ))),
        ));
    }

    // Parse method
    let method = match Method::from_bytes(req.method.as_bytes()) {
        Ok(m) => m,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("invalid method '{}': {}", req.method, e))),
            ));
        }
    };

    info!(
        "ai/network-probe: {} {} timeout_ms={}",
        method.as_str(),
        req.url,
        effective_timeout_ms
    );

    // Build reqwest client (use a per-request client so we can scope the
    // timeout exactly; reqwest::ClientBuilder is cheap).
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(effective_timeout_ms))
        .redirect(reqwest::redirect::Policy::limited(2))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("reqwest client build failed: {}", e))),
            ));
        }
    };

    let mut builder = client.request(method, parsed);
    if let Some(ref hdrs) = req.headers {
        for (k, v) in hdrs {
            builder = builder.header(k, v);
        }
    }
    if let Some(ref body) = req.body {
        builder = builder.body(body.clone());
    }

    let response = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            return Ok(Json(ApiResponse::success(NetworkProbeResponse {
                ok: false,
                status: None,
                headers: None,
                body: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                error: Some(format!("request failed: {}", e)),
            })));
        }
    };

    let status = response.status();
    let headers_map: HashMap<String, String> = response
        .headers()
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_string(), s.to_string()))
        })
        .collect();

    let body_bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return Ok(Json(ApiResponse::success(NetworkProbeResponse {
                ok: false,
                status: Some(status.as_u16()),
                headers: Some(headers_map),
                body: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                error: Some(format!("body read failed: {}", e)),
            })));
        }
    };

    let body_str = match std::str::from_utf8(&body_bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            return Ok(Json(ApiResponse::success(NetworkProbeResponse {
                ok: false,
                status: Some(status.as_u16()),
                headers: Some(headers_map),
                body: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                error: Some("non-utf8 body".to_string()),
            })));
        }
    };

    Ok(Json(ApiResponse::success(NetworkProbeResponse {
        ok: status.is_success(),
        status: Some(status.as_u16()),
        headers: Some(headers_map),
        body: Some(body_str),
        elapsed_ms: start.elapsed().as_millis() as u64,
        error: None,
    })))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route(
        "/ui-bridge/ai/network-probe",
        post(ai_network_probe_handler),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_gate_accepts_ipv4() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.0.0.2"));
    }

    #[test]
    fn loopback_gate_accepts_ipv6() {
        assert!(is_loopback_host("::1"));
    }

    #[test]
    fn loopback_gate_accepts_localhost_literal() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));
    }

    #[test]
    fn loopback_gate_accepts_mdns_local() {
        assert!(is_loopback_host("my-phone.local"));
        assert!(is_loopback_host("something.LOCAL"));
    }

    #[test]
    fn loopback_gate_rejects_public_ipv4() {
        assert!(!is_loopback_host("1.1.1.1"));
        assert!(!is_loopback_host("8.8.8.8"));
        assert!(!is_loopback_host("192.168.1.1")); // LAN IPs are still rejected
    }

    #[test]
    fn loopback_gate_rejects_public_hostnames() {
        assert!(!is_loopback_host("example.com"));
        assert!(!is_loopback_host("google.com"));
    }

    #[test]
    fn loopback_gate_rejects_empty() {
        assert!(!is_loopback_host(""));
    }
}
