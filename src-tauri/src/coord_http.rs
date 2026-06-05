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
    qontinui_runner_lib::auth::attach_device_auth(client.get(url))
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
