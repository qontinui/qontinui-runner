//! One process-wide pooled HTTP client for the per-agent coord daemons.
//!
//! **Why this module exists.** A `reqwest::Client` owns its own
//! connection pool, so `reqwest::Client::new()` *per request* — the
//! pattern this module replaces in `agent_token` and `dirty_poller` —
//! means no connection is ever reused: every call opens a fresh TCP
//! socket, and every one of those sockets then sits in `TIME_WAIT` for
//! minutes after the response. At one request per tick per agent that is
//! merely wasteful; under a retry loop it is fatal.
//!
//! On **2026-08-07** it was fatal. 96 agents whose tokens had expired
//! ~2.4 days earlier retried `dirty-state` + `refresh-token` with no
//! backoff: ~42,400 requests in the final 55 minutes (19,790 +
//! 22,637), each on its own socket. Windows logged Tcpip event **4231**
//! at 02:13:57 — *"a request to allocate an ephemeral port number from
//! the global TCP port space has failed due to all such ports being in
//! use"* — against the default 16,384-port dynamic range. The runner
//! (PID 77784, up 4.1 days) stopped at 02:55:09. Nothing killed it:
//! no reboot, no OOM, no crash report, no AV action. The port space
//! recovered only once the process was gone.
//!
//! The retry loop itself is bounded in [`crate::agent_token`]; this
//! module removes the amplifier. Both are needed — a bounded loop on
//! unpooled sockets still burns ports, and a pooled socket under an
//! unbounded loop still hammers coord.

use std::time::Duration;

use once_cell::sync::Lazy;
use tracing::error;

/// Whole-request ceiling. Generous: these calls are background
/// bookkeeping, and a slow coord should defer a tick, not fail it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Connect-phase ceiling. Distinct from the whole-request timeout so a
/// black-holed route fails fast instead of holding a socket for 30s.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Idle connections kept per host. The daemons all talk to one coord
/// host, so this is the effective pool size — comfortably above the
/// per-tick concurrency without pinning sockets open needlessly.
const POOL_MAX_IDLE_PER_HOST: usize = 8;

/// How long an idle pooled connection survives before being dropped.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .build()
        .unwrap_or_else(|e| {
            // Realistically only TLS-backend init can fail here, and the
            // fallback would fail identically — but a background daemon
            // must not take the process down over a client build, so
            // degrade loudly rather than `expect`.
            error!(
                "agent_http: pooled client build failed ({e}); \
                 falling back to an unpooled default client"
            );
            reqwest::Client::new()
        })
});

/// The shared pooled client.
///
/// Returns a reference: cloning a `Client` is cheap and shares the same
/// pool, but there is no reason to clone for a single request. **Never**
/// build a `reqwest::Client` inline on a per-tick daemon path — that is
/// the exact defect this module exists to prevent.
pub fn client() -> &'static reqwest::Client {
    &CLIENT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pool is process-wide: two calls must hand back the same
    /// client, or callers are silently back to a pool per request.
    #[test]
    fn client_is_a_shared_singleton() {
        let a = client();
        let b = client();
        assert!(
            std::ptr::eq(a, b),
            "agent_http::client() must return one shared instance"
        );
    }

    /// Guards the settings that make it a *pool* rather than a
    /// connection factory. A zero idle allowance would reuse nothing.
    #[test]
    fn pool_settings_are_reuse_shaped() {
        assert!(
            POOL_MAX_IDLE_PER_HOST > 0,
            "idle pool must retain connections for reuse"
        );
        assert!(CONNECT_TIMEOUT <= REQUEST_TIMEOUT);
        assert!(POOL_IDLE_TIMEOUT > Duration::from_secs(0));
    }
}
