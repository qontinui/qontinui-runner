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
    // coord-tenant-scope(escalated): a shared helper, not a route call -- `url` is the caller's argument and its downstream callers (21 at the Census E1 reading; that number is a census snapshot, not a live count -- `grep` for it) span session-, device- and work-scoped reads. The decision belongs at each caller via the existing `coord_get_for` (:123-129). Callers adopt it one at a time as each establishes its own class: the tenant-policy poll (`session/coord_sync.rs::fetch_session_coordination_flag`) is the first, per plan `2026-09-17-device-holds-one-credential-slot-so-a-session-cannot-work-a-bound-tenant` P3.
    qontinui_runner_lib::auth::attach_device_auth(client.get(url))
}

/// Build a coord POST request with the device-JWT bearer attached when one is
/// available — the write-side twin of [`coord_get`], same token source, same
/// never-fatal posture. Set the body and a per-request deadline on the
/// returned builder.
pub fn coord_post(client: &reqwest::Client, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
    // coord-tenant-scope(session-noop): unlike `coord_get`'s 21 cross-class callers, this helper
    // has exactly ONE caller -- `commands/remote_attach.rs:174`, `POST
    // /coord/sessions/{id}/attach-grants` -- so the helper's class is that route's class. The
    // route is session-scoped, but the runner can set no tenant on it: coord takes the SOURCE
    // device from the presented principal and the TARGET (and hence the tenant) from the
    // `coord.sessions` row named by the path id, which is the D6 mint's whole point -- a body
    // field could be forged, so neither end is read from one. Nothing to thread; terminal.
    // Re-classify if a second caller of a different class ever joins.
    qontinui_runner_lib::auth::attach_device_auth(client.post(url))
}

/// Build a coord PUT request with the device-JWT bearer attached when one is
/// available. Same contract as [`coord_post`].
pub fn coord_put(client: &reqwest::Client, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
    // coord-tenant-scope(device): single caller -- `commands/remote_attach.rs:61`, `PUT
    // /coord/devices/me/attach-preference`. `accept_remote_attach` is a column on
    // `coord.devices`, a table keyed by `device_id` with no tenant dimension at all, and `me`
    // resolves to the bearer's own device. The default binding's credential therefore names the
    // right row by construction, and stays right however many tenants this device is bound to.
    // Re-classify if a second caller of a different class ever joins.
    qontinui_runner_lib::auth::attach_device_auth(client.put(url))
}

/// Tenant-STATING variant of [`coord_get`] (Phase 8b, plan
/// `2026-07-02-session-scoped-multi-tenant-device-binding` §D4):
/// `TenantScope::Owned(t)` attaches that binding's device-JWT slot,
/// `TenantScope::Device` the default binding's (identical to [`coord_get`]),
/// and `TenantScope::Unresolved` degrades to unauthenticated on a multi-bound
/// device. Slot-miss posture is `auth::device_bearer_for`'s: a non-default
/// tenant with no slot sends the request UNAUTHENTICATED (never another
/// tenant's credential). This is the seam census E1 names as the resolution
/// for [`coord_get`]'s 21 cross-class callers — each states its own scope here
/// instead of inheriting one helper's guess.
/// First adopter: `session/coord_sync.rs::fetch_session_coordination_flag`,
/// the `/tenant-policy` poll, whose query names a tenant that coord requires
/// to EQUAL the presented token's claim.
///
/// **Takes `crate::auth::TenantScope`, not `qontinui_runner_lib::auth`'s**,
/// unlike the three unparameterized helpers above — and the reason is NOT a
/// naming convenience. This module is declared in `main.rs` alone, so it
/// compiles into the bin only and a bin caller could perfectly well name
/// `qontinui_runner_lib::auth::TenantScope`; no conversion was ever required.
///
/// What forces the choice is that `auth` IS compiled twice — `lib.rs`'s
/// `pub mod auth` and `main.rs`'s `mod auth` are two separate copies with
/// SEPARATE STATICS. `DATA_PLANE_TOTAL` / `DATA_PLANE_AUTHED` (the
/// coverage counters), `MISSING_TOKEN_WARNED`, `DEAD_LEGACY_SLOT_WARNED` and
/// the `warn_once_per_tenant_*` sets each exist once per copy. Routing this
/// helper through `crate::auth` puts the attach in the SAME copy as the
/// `crate::auth::presented_tenant` call that diagnoses its refusals and as
/// the other ~98 bin-side call sites, so the latch that says "already warned"
/// and the counter that says "N of M authed" are the ones those sites read.
/// Split across copies, a warning suppressed in one copy fires again from the
/// other and neither counter is the whole story.
///
/// Residual, named rather than fixed here: [`coord_get`], [`coord_post`] and
/// [`coord_put`] above still call the LIB copy's `attach_device_auth`, so the
/// runner emits the identically-worded
/// `"coord data-plane device-JWT coverage: X/Y (Z%)"` line from two
/// independent counters and an operator sees two indistinguishable series.
/// That split PRE-EXISTS this change and outlives it; converging the two
/// copies is a census item, not a line to slip into this one.
pub fn coord_get_for(
    client: &reqwest::Client,
    url: impl reqwest::IntoUrl,
    scope: crate::auth::TenantScope,
) -> reqwest::RequestBuilder {
    crate::auth::attach_device_auth_for(client.get(url), scope)
}

/// True iff a non-empty device-JWT is currently stored.
///
/// Callers that must distinguish "unpaired" (no token locally) from
/// "token present but rejected" (coord 401/403) use this — e.g.
/// `prompt_library`'s structured `auth` state. Mirrors the availability
/// check inside [`coord_get`]; never fatal, never panics.
pub fn have_device_token() -> bool {
    qontinui_runner_lib::auth::device_bearer().is_some()
}
