//! Shared coord data-plane GET helper.
//!
//! Phase P2 (plan `2026-06-05-coord-fleet-auth-followup-gaps`): the runner's
//! coord GET readers were issued with RAW unauthenticated `reqwest` even
//! though the process already holds a fresh device-JWT (minted by pairing,
//! stored in the access_token slot). Coord is about to gate these reader
//! routes with `FleetPrincipal`, so the runner must attach the device-JWT
//! NOW. This is backward-compatible: coord still accepts anonymous calls
//! today, so attaching a Bearer is safe to ship ahead of the coord gate.
//!
//! Both helpers delegate to [`qontinui_runner_lib::auth`] — the SAME token
//! source the write-path [`attach_device_auth`] uses — so reads and writes
//! present the identical bearer and feed the one auth-coverage metric.
//!
//! [`attach_device_auth`]: qontinui_runner_lib::auth::attach_device_auth
//!
//! # The shared client
//!
//! [`coord_client`] hands out ONE process-wide [`reqwest::Client`]. Every coord
//! caller must use it rather than building its own.
//!
//! A `reqwest::Client` is not a handle onto shared machinery — it OWNS a
//! connection pool and a DNS resolver. Building one per request therefore
//! defeats keep-alive (a fresh TCP + TLS handshake every time) and, because
//! reqwest's default `GaiResolver` resolves through blocking `getaddrinfo`
//! dispatched onto tokio's blocking pool, spends a blocking-pool slot per
//! request as well. With ~27 live agents heart-beating every 30s and
//! `post_log_line` firing per agent log line, that pattern pinned the pool near
//! its 512-thread ceiling and starved DNS resolution: requests exhausted their
//! 5s timeout while still queued, and coord — answering this box in under half
//! a second — was blamed for `operation timed out`.
//!
//! The client carries NO global timeout on purpose. Coord call sites want
//! different deadlines (3s for log lines, 5s for polls and heartbeats, 10s for
//! claim acquisition), so each sets its own via
//! [`reqwest::RequestBuilder::timeout`]. A global timeout here would silently
//! override them.

use std::sync::OnceLock;
use std::time::Duration;

/// Process-wide coord HTTP client, built once on first use.
///
/// `None` iff the one-time build failed (a broken TLS backend); cached so a
/// failure is not retried per request. Callers keep their existing
/// warn-and-continue posture on `None`.
static COORD_CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();

/// The shared coord [`reqwest::Client`] — one connection pool and one resolver
/// for the whole process.
///
/// Returns `None` only when the client could not be constructed at all, which
/// in practice means the TLS backend failed to initialise. That is the same
/// condition the old per-call `build()` sites reported, so callers that used to
/// `warn!` and skip on a build error keep doing exactly that.
///
/// Set a per-request deadline on the builder — this client has no global one:
///
/// ```ignore
/// let Some(client) = crate::coord_http::coord_client() else { return };
/// let resp = client
///     .post(&url)
///     .timeout(Duration::from_secs(5))
///     .json(&body)
///     .send()
///     .await?;
/// ```
pub fn coord_client() -> Option<&'static reqwest::Client> {
    COORD_CLIENT
        .get_or_init(|| {
            // `pool_max_idle_per_host` caps retained idle sockets per coord
            // host; the default is effectively unbounded, which under the old
            // churn is how the runner accumulated hundreds of half-live
            // connections. `pool_idle_timeout` keeps a warm connection across
            // the 30s heartbeat tick so the steady state is zero handshakes.
            match reqwest::Client::builder()
                .pool_max_idle_per_host(32)
                .pool_idle_timeout(Duration::from_secs(90))
                .tcp_keepalive(Duration::from_secs(60))
                .build()
            {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(
                        "coord_http: shared coord client build failed ({e:#}); \
                         coord calls will be skipped"
                    );
                    None
                }
            }
        })
        .as_ref()
}

/// Build a coord GET request with the device-JWT bearer attached when one is
/// available, otherwise return the builder unchanged.
///
/// The bearer comes from [`qontinui_runner_lib::auth::attach_device_auth`],
/// which reads `AuthManager::get_access_token()` per call (no caching, since
/// the JWT has a short TTL) and is NEVER fatal: an unpaired runner / empty
/// keychain / IO error all collapse to "send anonymously" — exactly the
/// pre-Phase-P2 behavior. Pre-pairing callers (credential-helper setup, the
/// tenant-policy poll, early handoff catch-up) thus keep working before any
/// token exists; coord 401/403 once it gates is a retry-after-auth signal,
/// not a fatal error.
///
/// Passing through `attach_device_auth` also drives the data-plane
/// auth-coverage metric, so these reads now count toward the same
/// unpaired→paired dogfood signal the write path reports.
pub fn coord_get(client: &reqwest::Client, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
    // coord-tenant-scope(escalated): a shared helper, not a route call -- `url` is the caller's argument and its 21 downstream callers span session-, device- and work-scoped reads. The decision belongs at each caller via the existing `coord_get_for` (:123-129). Census E1.
    qontinui_runner_lib::auth::attach_device_auth(client.get(url))
}

/// Tenant-selecting variant of [`coord_get`] (Phase 8b, plan
/// `2026-07-02-session-scoped-multi-tenant-device-binding` §D4):
/// `Some(tenant)` attaches that binding's device-JWT slot, `None` the
/// default binding's (identical to [`coord_get`]). Slot-miss posture is
/// `auth::device_bearer_for`'s: a non-default tenant with no slot sends the
/// request UNAUTHENTICATED (never another tenant's credential). Session-
/// scoped readers pass the owning session's tenant; everything else keeps
/// using [`coord_get`] and gets the default slot by construction.
#[allow(dead_code)] // seam: session-scoped GET readers adopt as they gain tenants
pub fn coord_get_for(
    client: &reqwest::Client,
    url: impl reqwest::IntoUrl,
    tenant: Option<&uuid::Uuid>,
) -> reqwest::RequestBuilder {
    qontinui_runner_lib::auth::attach_device_auth_for(client.get(url), tenant)
}

/// True iff a non-empty device-JWT is currently stored.
///
/// Callers that must distinguish "unpaired" (no token locally) from
/// "token present but rejected" (coord 401/403) use this — e.g.
/// `get_fleet_health`'s structured `auth` state. Mirrors the availability
/// check inside [`coord_get`]; never fatal, never panics.
pub fn have_device_token() -> bool {
    qontinui_runner_lib::auth::device_bearer().is_some()
}
