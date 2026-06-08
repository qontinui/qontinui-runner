//! Thin HTTP wrapper around coord's `/coord/git-ops/*` routes used by
//! [`crate::observable_bridge::git_ops::GitOpBridge`].
//!
//! Mirrors [`super::memory_client`] exactly — there is no central
//! `coord_client` crate in the runner, so each observable bridge passes
//! its own long-lived `reqwest::Client` so TLS keepalives amortize across
//! the per-session call chain (a session emits many `record` POSTs).
//!
//! The bridge only ever *records* ops; it never lists/pulls git state
//! (coord does not dictate the working tree — see `GitOpBridge::pull`,
//! which is a no-op). So this module exposes a single `record` call.

use anyhow::{anyhow, Context, Result};
use qontinui_types::git_ops::RecordGitOpRequest;
use std::time::Duration;
use uuid::Uuid;

/// Per-call timeout. `record` writes one append-only row — it should land
/// well under this. Matches `memory_client`'s 10s budget for single-row
/// POSTs.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Coord's tenant-scoping header. Coord's `TenantId` extractor
/// (`qontinui-coord/src/tenant_scope.rs`) resolves the tenant **solely**
/// from this header — it does NOT parse the Bearer JWT. Without this
/// header every call 403s with `tenant_not_resolved`. The bridge resolves
/// `tenant_id` locally (paired_user.json / OAuth claim) and sends it here;
/// the Bearer token is retained (commented, like `memory_client`) for
/// forward-compat with the future authenticated path.
const TENANT_HEADER: &str = "X-Qontinui-Tenant-Id";

/// Build the long-lived reqwest client the bridge reuses across calls.
/// Identical posture to [`super::memory_client::build_client`]: a pooled
/// client with a 300s idle timeout + 60s TCP keepalive so the TLS
/// connection survives between a session's git ops.
pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .pool_idle_timeout(Duration::from_secs(300))
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .context("observable_bridge::git_ops_client: reqwest client build")
}

/// Resolve the coord HTTP base from the active profile's `coord_url`.
/// Delegates to [`super::memory_client::coord_http_base`] so the
/// `wss://…/ws` → `https://…` normalization lives in one place.
pub fn coord_http_base() -> Option<String> {
    super::memory_client::coord_http_base()
}

/// `POST /coord/git-ops/record` — append one git op to the tenant feed.
///
/// Best-effort: returns `Result` so the caller can log and continue. The
/// tenant is resolved server-side from the verified device-JWT bearer
/// (coord's `FleetPrincipal`); the `X-Qontinui-Tenant-Id` header is retained
/// transitionally but coord no longer reads it.
pub async fn record(
    client: &reqwest::Client,
    base: &str,
    device_token: &str,
    tenant_id: Uuid,
    req: &RecordGitOpRequest,
) -> Result<()> {
    let url = format!("{base}/coord/git-ops/record");
    let resp = client
        .post(&url)
        // coord resolves the tenant from the verified bearer (FleetPrincipal);
        // the header is retained transitionally (coord no longer reads it).
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
    Ok(())
}
