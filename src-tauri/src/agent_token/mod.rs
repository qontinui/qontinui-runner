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
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::agent_http;

/// Refresh the JWT proactively when this much time-to-expiry remains.
/// Tokens are 4h per §3.3; 30min margin ⇒ ~7 refresh opportunities
/// per token lifetime in the happy path.
pub const TOKEN_REFRESH_MARGIN_SECS: i64 = 30 * 60;

/// First delay after a *transient* refresh failure. Doubles per further
/// consecutive failure up to [`REFRESH_BACKOFF_CAP_SECS`]
/// (30s→1m→2m→4m→8m→16m→30m, then 30m forever).
pub const REFRESH_BACKOFF_BASE_SECS: i64 = 30;

/// Ceiling on the transient-failure backoff ladder.
pub const REFRESH_BACKOFF_CAP_SECS: i64 = 30 * 60;

/// Consecutive transient failures before the ladder escalates from
/// `warn!` to `error!`. Matches the poller-health escalation threshold
/// introduced by runner #930 so both background loops get loud at the
/// same streak length.
pub const REFRESH_ESCALATE_AFTER: u32 = 5;

/// Delay before the next refresh attempt after `consecutive_failures`
/// transient failures. Pure — unit-tested without clocks.
///
/// Deliberately *not* shared with `agent_pusher::backoff_delay_secs`:
/// that ladder governs one push target and has no terminal state, while
/// this one is paired with the [`RefreshHealth::rejected`] latch below.
/// They are different abstractions that happen to share a shape.
pub fn refresh_backoff_delay_secs(consecutive_failures: u32) -> i64 {
    if consecutive_failures == 0 {
        return 0;
    }
    let mut delay = REFRESH_BACKOFF_BASE_SECS.min(REFRESH_BACKOFF_CAP_SECS);
    for _ in 1..consecutive_failures {
        if delay >= REFRESH_BACKOFF_CAP_SECS {
            return REFRESH_BACKOFF_CAP_SECS;
        }
        delay = delay.saturating_mul(2);
    }
    delay.min(REFRESH_BACKOFF_CAP_SECS)
}

/// Refresh-loop health for one slot. Default = healthy, never tried.
///
/// Exists because the loop had **no terminal state**: before this, a
/// refusal logged `"retry next tick"` and returned `Ok`, leaving
/// `needs_refresh()` true forever. On 2026-08-07 that produced 22,637
/// refresh attempts in 55 minutes against tokens coord had been
/// rejecting for 2.4 days. Failure has to be *remembered* between ticks
/// or every tick starts from scratch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshHealth {
    /// Consecutive *transient* failures (transport error, 5xx, undecodable
    /// body). Reset by any success.
    pub consecutive_failures: u32,
    /// Unix seconds before which no further attempt is made. Set by the
    /// backoff ladder; 0 = attempt now.
    pub next_attempt_epoch_secs: i64,
    /// Latched when coord rejected the **bearer itself** (401/403).
    ///
    /// Terminal by construction: the refresh call authenticates with the
    /// very token being refreshed, so once coord refuses it, re-sending
    /// it can never start working. Only a fresh allocation clears this —
    /// see [`TokenSlot::adopt_fresh`].
    pub rejected: bool,
}

/// Mutable token + bookkeeping. Behind an `RwLock` (in
/// [`SharedToken`]) so a refresh never drops an in-flight push/poll.
#[derive(Debug, Clone, Default)]
pub struct TokenSlot {
    pub token: String,
    pub jti: Uuid,
    /// Unix-seconds expiry. Refresh fires when
    /// `exp - now() < TOKEN_REFRESH_MARGIN_SECS`.
    ///
    /// **Bookkeeping only** — deliberately decoupled from the JWT's own
    /// `exp` claim so tests can force the refresh boundary with a real,
    /// still-valid token (`mcp/test_fixtures.rs` seed-agent-token). Never
    /// infer "the bearer is dead" from this field; only coord's answer
    /// establishes that, which is what [`RefreshHealth::rejected`] records.
    pub exp: i64,
    /// Refresh-loop health. Not part of the token's identity.
    pub health: RefreshHealth,
}

impl TokenSlot {
    /// Seconds until expiry from `now`.
    pub fn ttl_secs(&self, now_unix: i64) -> i64 {
        self.exp - now_unix
    }

    pub fn needs_refresh(&self, now_unix: i64) -> bool {
        self.ttl_secs(now_unix) < TOKEN_REFRESH_MARGIN_SECS
    }

    /// True when coord has rejected this bearer and retrying is futile.
    /// Callers use this to skip network work that would only add load.
    pub fn is_rejected(&self) -> bool {
        self.health.rejected
    }

    /// Install a newly-issued token, clearing all failure state. The one
    /// way out of [`RefreshHealth::rejected`] — used by a successful
    /// refresh and by any caller that re-allocates the agent.
    pub fn adopt_fresh(&mut self, token: String, jti: Uuid, exp: i64) {
        self.token = token;
        self.jti = jti;
        self.exp = exp;
        self.health = RefreshHealth::default();
    }

    /// Fold one transient failure into the backoff ladder. Returns the
    /// new consecutive-failure count so the caller can pick a log level.
    pub fn record_transient_failure(&mut self, now_unix: i64) -> u32 {
        self.health.consecutive_failures = self.health.consecutive_failures.saturating_add(1);
        self.health.next_attempt_epoch_secs =
            now_unix.saturating_add(refresh_backoff_delay_secs(self.health.consecutive_failures));
        self.health.consecutive_failures
    }

    /// Latch the terminal rejection. Idempotent; returns `true` only on
    /// the transition, so the caller logs the escalation exactly once
    /// instead of once per tick.
    pub fn record_rejected(&mut self) -> bool {
        if self.health.rejected {
            return false;
        }
        self.health.rejected = true;
        true
    }

    /// Project this slot into its published health record.
    ///
    /// **Never carries `token`.** The slot holds a live JWT; a health
    /// surface that leaked it would turn an observability endpoint into
    /// a credential endpoint. Only derived facts cross this boundary.
    pub fn health_report(&self, agent_id: Uuid, now_unix: i64) -> AgentTokenHealth {
        AgentTokenHealth {
            agent_id,
            jti: self.jti,
            ttl_secs: self.ttl_secs(now_unix),
            needs_refresh: self.needs_refresh(now_unix),
            rejected: self.health.rejected,
            consecutive_failures: self.health.consecutive_failures,
            retry_in_secs: (self.health.next_attempt_epoch_secs - now_unix).max(0),
            state: if self.health.rejected {
                TokenState::Rejected
            } else if self.health.consecutive_failures > 0 {
                TokenState::Degraded
            } else {
                TokenState::Healthy
            },
        }
    }
}

/// Coarse per-agent verdict. Named states rather than a bare boolean so a
/// reader cannot mistake "retrying, may recover" for "latched off, will
/// never recover without a new allocation".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenState {
    /// Refreshing normally — no failures recorded.
    Healthy,
    /// Transient failures are accumulating; still retrying under backoff.
    Degraded,
    /// Coord refused the bearer. Terminal — the daemons for this agent
    /// have stopped doing network work and will not resume on their own.
    Rejected,
}

/// One agent's refresh health, as published by the runner API.
///
/// Reports the refresh loop's **own output**, not the caller's inputs —
/// the lesson `agent_worktree::on_demand::Survey::poller` records, where
/// a `coord_reachable: true` field read healthy for five days while the
/// poller failed 100% of its pulls.
#[derive(Debug, Clone, Serialize)]
pub struct AgentTokenHealth {
    pub agent_id: Uuid,
    /// Current token's `jti`. Rotates on every successful refresh, so a
    /// changing `jti` is the proof that refresh is actually working.
    pub jti: Uuid,
    /// Seconds until the bookkeeping expiry; negative once past it.
    pub ttl_secs: i64,
    pub needs_refresh: bool,
    pub rejected: bool,
    pub consecutive_failures: u32,
    /// Seconds until the next refresh attempt is allowed; 0 = eligible now.
    pub retry_in_secs: i64,
    pub state: TokenState,
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
        health: RefreshHealth::default(),
    })))
}

/// What one [`maybe_refresh`] call did — lets callers skip work that a
/// known-bad token would only waste on coord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Comfortably inside the token's lifetime; nothing attempted.
    Fresh,
    /// Refreshed successfully on this call.
    Refreshed,
    /// A refresh is due but was not attempted (inside the backoff
    /// window), or it failed transiently. The current token is still
    /// worth using — it may simply be near expiry, not invalid.
    Deferred,
    /// Coord rejected the bearer. Terminal until re-allocation.
    Rejected,
}

impl RefreshOutcome {
    /// True when the caller should skip network work using this token.
    ///
    /// Only [`RefreshOutcome::Rejected`] qualifies: a `Deferred` token is
    /// unproven, not refused, and dropping its work would break the
    /// normal near-expiry path. The 2026-08-07 storm was 19,790
    /// `dirty-state` POSTs sent *with a token coord had already
    /// refused* — this is the check that suppresses them.
    pub fn should_skip_work(self) -> bool {
        matches!(self, RefreshOutcome::Rejected)
    }
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
/// `TOKEN_REFRESH_MARGIN_SECS` of expiry. `who` is a short caller label
/// for log lines (`"agent_pusher"` / `"dirty_poller"`); behaviour is
/// identical regardless. Idempotent across daemons sharing the slot —
/// the first daemon to tick after the margin refreshes; the rest observe
/// the already-fresh token and no-op.
///
/// Still best-effort — a failure returns `Ok` so one bad refresh never
/// fails the caller's tick — but no longer *memoryless*. Three outcomes
/// are now distinguished, and the caller is told which:
///
/// - **transient** (transport error, 5xx, undecodable body) → exponential
///   backoff via [`TokenSlot::record_transient_failure`], escalating to
///   `error!` after [`REFRESH_ESCALATE_AFTER`] consecutive failures;
/// - **rejected** (401/403 — coord refused the bearer) → latched
///   terminal, logged once, never retried. Retrying cannot help: the
///   refresh request authenticates *with the token being refreshed*;
/// - **success** → all failure state cleared.
///
/// Returns the [`RefreshOutcome`] so callers can skip work a rejected
/// token would only waste (see [`RefreshOutcome::should_skip_work`]).
///
/// This is the loop that took the runner down on 2026-08-07 — see
/// [`RefreshHealth`] and [`crate::agent_http`].
pub async fn maybe_refresh(
    token: &SharedToken,
    coord_http_base: &str,
    agent_id: Uuid,
    who: &str,
) -> Result<RefreshOutcome> {
    let now = chrono::Utc::now().timestamp();
    let (needs, ttl, rejected, next_attempt) = {
        let g = token.read().await;
        (
            g.needs_refresh(now),
            g.ttl_secs(now),
            g.is_rejected(),
            g.health.next_attempt_epoch_secs,
        )
    };
    // Terminal: coord already refused this bearer. Silent by design —
    // the transition was logged once, and this branch runs on every tick
    // of every daemon sharing the slot.
    if rejected {
        return Ok(RefreshOutcome::Rejected);
    }
    if !needs {
        return Ok(RefreshOutcome::Fresh);
    }
    // Inside the transient-failure backoff window.
    if now < next_attempt {
        return Ok(RefreshOutcome::Deferred);
    }
    info!("{who}: agent_id={agent_id} refreshing token (ttl_secs={ttl})");
    let url = format!(
        "{}/agents/{}/refresh-token",
        coord_http_base.trim_end_matches('/'),
        agent_id
    );
    let current = token.read().await.token.clone();
    let resp = match agent_http::client()
        .post(&url)
        .bearer_auth(&current)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let streak = token.write().await.record_transient_failure(now);
            log_transient(who, agent_id, streak, &format!("request failed: {e}"));
            return Ok(RefreshOutcome::Deferred);
        }
    };
    let status = resp.status();
    if !status.is_success() {
        // 401/403 = the bearer itself was refused. Unrecoverable by
        // retry, because the retry would present the same bearer.
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            // Bind first: a write guard created inside an `if` condition
            // lives until the end of the whole `if` statement, which
            // would hold the slot locked across the log call.
            let newly_rejected = token.write().await.record_rejected();
            if newly_rejected {
                error!(
                    "{who}: agent_id={agent_id} token refresh REJECTED ({status}) — \
                     coord refused the bearer, so retrying it cannot succeed. \
                     Refresh loop latched off for this agent; it needs a fresh \
                     allocation to recover. (ttl_secs={ttl})"
                );
            }
            return Ok(RefreshOutcome::Rejected);
        }
        let streak = token.write().await.record_transient_failure(now);
        log_transient(who, agent_id, streak, &format!("returned {status}"));
        return Ok(RefreshOutcome::Deferred);
    }
    let body: RefreshResponseBody = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            let streak = token.write().await.record_transient_failure(now);
            log_transient(who, agent_id, streak, &format!("body decode failed: {e}"));
            return Ok(RefreshOutcome::Deferred);
        }
    };
    let mut g = token.write().await;
    g.adopt_fresh(body.token, body.jti, body.exp);
    info!(
        "{who}: agent_id={agent_id} token refreshed jti={} exp={}",
        g.jti, g.exp
    );
    Ok(RefreshOutcome::Refreshed)
}

/// Log a transient refresh failure, escalating to `error!` once the
/// streak reaches [`REFRESH_ESCALATE_AFTER`]. A long streak is how a
/// silently-degrading background loop becomes visible — the 2026-08-07
/// loop ran for 2.4 days entirely at `warn!`.
fn log_transient(who: &str, agent_id: Uuid, streak: u32, what: &str) {
    let retry_in = refresh_backoff_delay_secs(streak);
    if streak >= REFRESH_ESCALATE_AFTER {
        error!(
            "{who}: agent_id={agent_id} token refresh {what} — \
             {streak} consecutive failures, retry in {retry_in}s"
        );
    } else {
        warn!(
            "{who}: agent_id={agent_id} token refresh {what} — \
             retry in {retry_in}s (streak {streak})"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn slot(exp: i64) -> TokenSlot {
        TokenSlot {
            token: "x".into(),
            jti: Uuid::nil(),
            exp,
            ..Default::default()
        }
    }

    #[test]
    fn needs_refresh_threshold() {
        let now = NOW;
        assert!(!slot(now + 4 * 3600).needs_refresh(now));
        assert!(slot(now + 600).needs_refresh(now));
        // Exactly one second under the margin → refresh.
        assert!(slot(now + TOKEN_REFRESH_MARGIN_SECS - 1).needs_refresh(now));
    }

    /// A fresh slot is healthy: nothing rejected, no backoff window.
    #[test]
    fn default_health_is_clean() {
        let s = slot(NOW + 4 * 3600);
        assert!(!s.is_rejected());
        assert_eq!(s.health, RefreshHealth::default());
        assert_eq!(s.health.next_attempt_epoch_secs, 0);
    }

    /// The ladder doubles from the base and clamps at the cap — it must
    /// never grow without bound and never return 0 for a real failure.
    #[test]
    fn backoff_ladder_doubles_then_caps() {
        assert_eq!(refresh_backoff_delay_secs(0), 0);
        assert_eq!(refresh_backoff_delay_secs(1), REFRESH_BACKOFF_BASE_SECS);
        assert_eq!(refresh_backoff_delay_secs(2), REFRESH_BACKOFF_BASE_SECS * 2);
        assert_eq!(refresh_backoff_delay_secs(3), REFRESH_BACKOFF_BASE_SECS * 4);
        // Far past the cap — clamped, not overflowing.
        assert_eq!(refresh_backoff_delay_secs(64), REFRESH_BACKOFF_CAP_SECS);
        assert_eq!(
            refresh_backoff_delay_secs(u32::MAX),
            REFRESH_BACKOFF_CAP_SECS
        );
    }

    /// Consecutive transient failures push the next attempt further out.
    /// This is what the pre-fix loop lacked: without it every tick
    /// retried immediately, forever.
    #[test]
    fn transient_failures_extend_the_backoff_window() {
        let mut s = slot(NOW);
        assert_eq!(s.record_transient_failure(NOW), 1);
        let first = s.health.next_attempt_epoch_secs;
        assert_eq!(first, NOW + REFRESH_BACKOFF_BASE_SECS);

        assert_eq!(s.record_transient_failure(NOW), 2);
        assert!(
            s.health.next_attempt_epoch_secs > first,
            "second failure must back off further than the first"
        );
        // Still recoverable — a transient failure is not a rejection.
        assert!(!s.is_rejected());
    }

    /// Rejection latches, and latches exactly once so the escalation is
    /// logged a single time rather than once per tick per daemon.
    #[test]
    fn rejection_latches_once() {
        let mut s = slot(NOW);
        assert!(s.record_rejected(), "first rejection is the transition");
        assert!(s.is_rejected());
        assert!(
            !s.record_rejected(),
            "already-rejected must not re-report the transition"
        );
        assert!(s.is_rejected());
    }

    /// A successful refresh is the way out of every failure state —
    /// including the terminal one, since a new allocation issues a new
    /// bearer that coord has not refused.
    #[test]
    fn adopt_fresh_clears_all_failure_state() {
        let mut s = slot(NOW);
        s.record_transient_failure(NOW);
        s.record_rejected();
        assert!(s.is_rejected());

        let jti = Uuid::from_u128(7);
        s.adopt_fresh("new-token".into(), jti, NOW + 4 * 3600);

        assert_eq!(s.token, "new-token");
        assert_eq!(s.jti, jti);
        assert_eq!(s.exp, NOW + 4 * 3600);
        assert!(!s.is_rejected());
        assert_eq!(s.health, RefreshHealth::default());
        assert!(!s.needs_refresh(NOW));
    }

    /// Only a rejected token suppresses caller work. A `Deferred` token
    /// is unproven, not refused — treating it as dead would break the
    /// ordinary near-expiry path.
    #[test]
    fn only_rejected_skips_caller_work() {
        assert!(RefreshOutcome::Rejected.should_skip_work());
        assert!(!RefreshOutcome::Fresh.should_skip_work());
        assert!(!RefreshOutcome::Refreshed.should_skip_work());
        assert!(!RefreshOutcome::Deferred.should_skip_work());
    }

    /// The escalation threshold must be reachable by the ladder before
    /// the cap flattens it, or the `error!` tier would be unreachable in
    /// practice.
    #[test]
    fn escalation_threshold_is_reachable() {
        assert!(REFRESH_ESCALATE_AFTER > 0);
        assert!(
            refresh_backoff_delay_secs(REFRESH_ESCALATE_AFTER) <= REFRESH_BACKOFF_CAP_SECS,
            "escalation must fire at or before the ladder flattens"
        );
    }
}
