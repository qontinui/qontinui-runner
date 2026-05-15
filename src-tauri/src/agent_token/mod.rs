//! Shared per-agent JWT slot + proactive refresh.
//!
//! Row 9 §3.3 tokens live 4h; long agent sessions outlive a single
//! token, so every per-agent daemon (`agent_pusher`, `dirty_poller`,
//! …) must refresh proactively. Before the spawn-site unification each
//! daemon carried its **own** `TokenSlot` + its own refresh loop —
//! correct, but at 300 agents that's 2× the `POST
//! /agents/:id/refresh-token` load and two independently-rotating jtis
//! per agent. This module is the single source of truth: one
//! `Arc<RwLock<TokenSlot>>` per agent, shared by every daemon
//! `agent_daemons::spawn_for_agent` starts, and one `maybe_refresh`
//! implementation.
//!
//! Standalone use (the modules' own unit/integration tests) still
//! builds a private token via [`from_allocate_result`]; only the
//! production spawn path shares one.

use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

/// Refresh the JWT proactively when this much time-to-expiry remains.
/// Tokens are 4h per §3.3; 30min margin ⇒ ~7 refresh opportunities
/// per token lifetime in the happy path.
pub const TOKEN_REFRESH_MARGIN_SECS: i64 = 30 * 60;

/// Mutable token + bookkeeping. Behind an `RwLock` (in
/// [`SharedToken`]) so a refresh never drops an in-flight push/poll.
#[derive(Debug, Clone)]
pub struct TokenSlot {
    pub token: String,
    pub jti: Uuid,
    /// Unix-seconds expiry. Refresh fires when
    /// `exp - now() < TOKEN_REFRESH_MARGIN_SECS`.
    pub exp: i64,
}

impl TokenSlot {
    /// Seconds until expiry from `now`.
    pub fn ttl_secs(&self, now_unix: i64) -> i64 {
        self.exp - now_unix
    }

    pub fn needs_refresh(&self, now_unix: i64) -> bool {
        self.ttl_secs(now_unix) < TOKEN_REFRESH_MARGIN_SECS
    }
}

/// One token slot shared by every daemon spawned for an agent.
pub type SharedToken = Arc<RwLock<TokenSlot>>;

/// Build a fresh shared token from a coord allocation. `None` when
/// the allocation carried no token (coord JWT keys unconfigured / dev
/// fallback) — callers skip daemon spawn and log + continue.
pub fn from_allocate_result(
    allocate: &crate::agent_worktree::AllocateResult,
) -> Option<SharedToken> {
    if allocate.token.is_empty() {
        return None;
    }
    Some(Arc::new(RwLock::new(TokenSlot {
        token: allocate.token.clone(),
        jti: allocate.token_jti,
        exp: allocate.token_exp,
    })))
}

#[derive(Debug, Deserialize)]
struct RefreshResponseBody {
    token: String,
    #[allow(dead_code)]
    agent_id: Uuid,
    jti: Uuid,
    exp: i64,
}

/// Refresh the shared token if it's within
/// `TOKEN_REFRESH_MARGIN_SECS` of expiry. Best-effort: a failed
/// refresh logs + returns `Ok` so the caller's next tick retries.
/// `who` is a short caller label for log lines
/// (`"agent_pusher"` / `"dirty_poller"`); behaviour is identical
/// regardless. Idempotent across daemons sharing the slot — the first
/// daemon to tick after the margin refreshes; the rest observe the
/// already-fresh token and no-op.
pub async fn maybe_refresh(
    token: &SharedToken,
    coord_http_base: &str,
    agent_id: Uuid,
    who: &str,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let (needs, ttl) = {
        let g = token.read().await;
        (g.needs_refresh(now), g.ttl_secs(now))
    };
    if !needs {
        return Ok(());
    }
    info!("{who}: agent_id={agent_id} refreshing token (ttl_secs={ttl})");
    let url = format!(
        "{}/agents/{}/refresh-token",
        coord_http_base.trim_end_matches('/'),
        agent_id
    );
    let current = token.read().await.token.clone();
    let resp = match reqwest::Client::new()
        .post(&url)
        .bearer_auth(&current)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("{who}: token refresh request failed: {e} — retry next tick");
            return Ok(());
        }
    };
    if !resp.status().is_success() {
        warn!(
            "{who}: token refresh returned {} — retry next tick",
            resp.status()
        );
        return Ok(());
    }
    let body: RefreshResponseBody = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!("{who}: token refresh body decode failed: {e}");
            return Ok(());
        }
    };
    let mut g = token.write().await;
    *g = TokenSlot {
        token: body.token,
        jti: body.jti,
        exp: body.exp,
    };
    info!(
        "{who}: agent_id={agent_id} token refreshed jti={} exp={}",
        g.jti, g.exp
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_refresh_threshold() {
        let now = 1_700_000_000;
        let fresh = TokenSlot {
            token: "x".into(),
            jti: Uuid::nil(),
            exp: now + 4 * 3600,
        };
        assert!(!fresh.needs_refresh(now));
        let stale = TokenSlot {
            token: "x".into(),
            jti: Uuid::nil(),
            exp: now + 600,
        };
        assert!(stale.needs_refresh(now));
        // Exactly one second under the margin → refresh.
        let edge = TokenSlot {
            token: "x".into(),
            jti: Uuid::nil(),
            exp: now + TOKEN_REFRESH_MARGIN_SECS - 1,
        };
        assert!(edge.needs_refresh(now));
    }
}
