//! Thin HTTP wrapper around coord's `/coord/memory/*` routes used by
//! [`crate::observable_bridge::memory::MemoryBridge`].
//!
//! Mirrors the ad-hoc idiom from `fleet.rs::register_with_coord` /
//! `fleet.rs::heartbeat_to_coord` — there is no central `coord_client`
//! crate in the runner. Each module passes its own long-lived
//! `reqwest::Client` so TLS keepalives amortize across the per-session
//! call chain (pull → many upserts → reconcile).
//!
//! Auth: every call carries `Authorization: Bearer <device-token JWT>`.
//! The token is fetched by the caller via
//! [`crate::auth::AuthManager::get_access_token`] and threaded through
//! as an argument so this module stays Tauri-free and testable.

use anyhow::{anyhow, Context, Result};
use qontinui_types::memory::{
    MemoryListResponse, MemoryUpsertRequest, MemoryUpsertResponse, MemoryWithHistory,
};
use std::time::Duration;
use uuid::Uuid;

/// Per-call timeout. List/get are cheap metadata reads; upsert writes
/// one row; delete writes one tombstone — all should land well under
/// this. Matches the 10s timeout `fleet::publish_budget` uses for
/// similar single-row POSTs.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Coord's tenant-scoping header. Coord's `TenantId` extractor
/// (`qontinui-coord/src/tenant_scope.rs`) resolves the tenant **solely**
/// from this header — it does NOT parse the Bearer JWT (device-token →
/// tenant resolution is a deferred coord hardening pass). The bridge
/// resolves `tenant_id` locally (paired_user.json / OAuth claim) and
/// sends it here; the Bearer token is retained for forward-compat with
/// that future authenticated path. Without this header every call 403s
/// with `tenant_not_resolved`.
const TENANT_HEADER: &str = "X-Qontinui-Tenant-Id";

/// Build the long-lived reqwest client the bridge reuses across calls.
/// Returned to the bridge once at construction; cloned cheaply (it's an
/// `Arc` internally) into each method call.
pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .pool_idle_timeout(Duration::from_secs(300))
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .context("observable_bridge::memory_client: reqwest client build")
}

/// Resolve the coord HTTP base from the active profile's `coord_url`.
///
/// Duplicated from `fleet.rs::coord_http_base` (private there) rather
/// than re-exporting to keep the bridge self-contained. Five lines, no
/// drift risk.
pub fn coord_http_base() -> Option<String> {
    let coord_url = crate::profiles::load_strict().ok()?.coord_url?;
    let trimmed = coord_url.trim_end_matches("/ws");
    let with_http = trimmed
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| {
            trimmed
                .strip_prefix("ws://")
                .map(|rest| format!("http://{rest}"))
        })
        .unwrap_or_else(|| trimmed.to_string());
    Some(with_http)
}

/// `GET /coord/memory/list` — metadata-only catalog of every live
/// memory in the caller's tenant pool. Tenant is resolved server-side
/// from the device-token JWT.
pub async fn list(
    client: &reqwest::Client,
    base: &str,
    device_token: &str,
    tenant_id: Uuid,
) -> Result<MemoryListResponse> {
    let url = format!("{base}/coord/memory/list");
    let resp = client
        .get(&url)
        .header(TENANT_HEADER, tenant_id.to_string())
        .bearer_auth(device_token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(400).collect();
        return Err(anyhow!("GET {url} -> HTTP {status}: {excerpt}"));
    }
    resp.json::<MemoryListResponse>()
        .await
        .with_context(|| format!("decode MemoryListResponse from {url}"))
}

/// `GET /coord/memory/:name` — latest row + up to 10 recent versions.
/// Returns the full content blob.
pub async fn get(
    client: &reqwest::Client,
    base: &str,
    device_token: &str,
    tenant_id: Uuid,
    name: &str,
) -> Result<MemoryWithHistory> {
    let url = format!("{base}/coord/memory/{}", urlencoding::encode(name));
    let resp = client
        .get(&url)
        .header(TENANT_HEADER, tenant_id.to_string())
        .bearer_auth(device_token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(400).collect();
        return Err(anyhow!("GET {url} -> HTTP {status}: {excerpt}"));
    }
    resp.json::<MemoryWithHistory>()
        .await
        .with_context(|| format!("decode MemoryWithHistory from {url}"))
}

/// `POST /coord/memory/upsert` — append a new version of `req.name`.
/// Coord assigns the version monotonically per-tenant.
pub async fn upsert(
    client: &reqwest::Client,
    base: &str,
    device_token: &str,
    tenant_id: Uuid,
    req: &MemoryUpsertRequest,
) -> Result<MemoryUpsertResponse> {
    let url = format!("{base}/coord/memory/upsert");
    let resp = client
        .post(&url)
        .header(TENANT_HEADER, tenant_id.to_string())
        .bearer_auth(device_token)
        .json(req)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(400).collect();
        return Err(anyhow!("POST {url} -> HTTP {status}: {excerpt}"));
    }
    resp.json::<MemoryUpsertResponse>()
        .await
        .with_context(|| format!("decode MemoryUpsertResponse from {url}"))
}

/// `POST /coord/federation/reports` — record a federation reconcile report.
/// Best-effort — the caller should log and continue on failure.
pub async fn post_federation_report(
    client: &reqwest::Client,
    base: &str,
    device_token: &str,
    tenant_id: Uuid,
    body: &serde_json::Value,
) -> Result<()> {
    let url = format!("{base}/coord/federation/reports");
    let resp = client
        .post(&url)
        .header(TENANT_HEADER, tenant_id.to_string())
        .bearer_auth(device_token)
        .json(body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(400).collect();
        anyhow::bail!("POST {url} -> HTTP {status}: {excerpt}");
    }
    Ok(())
}

/// `DELETE /coord/memory/:name` — soft delete (appends a tombstone row).
/// 200 on success, 404 if the memory never existed. 404 is treated as
/// success (idempotent intent: "this name should not exist in coord").
pub async fn delete(
    client: &reqwest::Client,
    base: &str,
    device_token: &str,
    tenant_id: Uuid,
    name: &str,
) -> Result<()> {
    let url = format!("{base}/coord/memory/{}", urlencoding::encode(name));
    let resp = client
        .delete(&url)
        .header(TENANT_HEADER, tenant_id.to_string())
        .bearer_auth(device_token)
        .send()
        .await
        .with_context(|| format!("DELETE {url}"))?;
    let status = resp.status();
    if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    let excerpt: String = body.chars().take(400).collect();
    Err(anyhow!("DELETE {url} -> HTTP {status}: {excerpt}"))
}
