//! `GET /agent-tokens/health` — is the per-agent JWT refresh loop working?
//!
//! **Why this exists.** Before it, the refresh loop's only output was log
//! lines. On 2026-08-07 that loop degraded for 2.4 days — 96 agents whose
//! tokens coord had already rejected, retrying with no backoff — and the
//! sole evidence was `warn!` spam in a 50 MB/day file that nothing read.
//! The daemons went on POSTing with dead credentials until the machine ran
//! out of ephemeral ports and the runner stopped
//! (see [`crate::agent_http`] and [`crate::agent_token::RefreshHealth`]).
//!
//! The breaker now latches those agents off, which fixes the storm but
//! makes the failure *quieter still*: a latched agent is silent by design.
//! Silence that means "stopped working" has to be legible somewhere, so it
//! is legible here.
//!
//! This endpoint reports the loop's **own output** — per-agent state, the
//! streak, the rotating `jti` — never a caller's inputs. That is the lesson
//! recorded on `agent_worktree::on_demand::Survey::poller`, where a
//! `coord_reachable: true` field read healthy for five days while the
//! poller failed 100% of its pulls.
//!
//! **No token material crosses this boundary** — see
//! [`crate::agent_token::TokenSlot::health_report`].

use std::sync::Arc;

use axum::{response::Json, routing::get, Router};
use serde::Serialize;

use crate::agent_token::{AgentTokenHealth, TokenState};
use crate::mcp::types::{ApiResponse, ApiState};

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/agent-tokens/health", get(health_handler))
}

/// Roll-up counts. Present so a monitor can alert on one number without
/// walking the array.
#[derive(Debug, Clone, Serialize)]
pub struct TokenHealthSummary {
    pub total: usize,
    /// Latched off by coord's refusal. **This is the alert-worthy number** —
    /// any value above zero means agents have silently stopped pushing and
    /// polling, and cannot recover without a fresh allocation.
    pub rejected: usize,
    /// Accumulating transient failures, still retrying under backoff.
    pub degraded: usize,
    pub healthy: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenHealthReport {
    pub summary: TokenHealthSummary,
    /// Per-agent detail, `rejected` first, then `degraded`, then healthy.
    pub agents: Vec<AgentTokenHealth>,
}

/// `GET /agent-tokens/health`
///
/// ```jsonc
/// { "success": true, "data": {
///     "summary": { "total": 96, "rejected": 96, "degraded": 0, "healthy": 0 },
///     "agents": [
///       { "agent_id": "019fcb93-…", "jti": "…", "ttl_secs": -210553,
///         "needs_refresh": true, "rejected": true,
///         "consecutive_failures": 0, "retry_in_secs": 0,
///         "state": "rejected" }
///     ]
/// }}
/// ```
///
/// An **empty** `agents` array means no agent slots are registered in this
/// process — not that everything is fine. `total: 0` is the honest reading;
/// do not treat it as a green signal.
async fn health_handler() -> Json<ApiResponse<TokenHealthReport>> {
    let agents = crate::coord_mcp::agent_token_health_snapshot().await;
    Json(ApiResponse::success(TokenHealthReport {
        summary: summarize(&agents),
        agents,
    }))
}

/// Pure — unit-tested without a registry or a runtime.
fn summarize(agents: &[AgentTokenHealth]) -> TokenHealthSummary {
    let mut s = TokenHealthSummary {
        total: agents.len(),
        rejected: 0,
        degraded: 0,
        healthy: 0,
    };
    for a in agents {
        match a.state {
            TokenState::Rejected => s.rejected += 1,
            TokenState::Degraded => s.degraded += 1,
            TokenState::Healthy => s.healthy += 1,
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn agent(state: TokenState, streak: u32) -> AgentTokenHealth {
        AgentTokenHealth {
            agent_id: Uuid::nil(),
            jti: Uuid::nil(),
            ttl_secs: 0,
            needs_refresh: true,
            rejected: matches!(state, TokenState::Rejected),
            consecutive_failures: streak,
            retry_in_secs: 0,
            state,
        }
    }

    #[test]
    fn summary_counts_each_state() {
        let agents = vec![
            agent(TokenState::Rejected, 0),
            agent(TokenState::Rejected, 0),
            agent(TokenState::Degraded, 3),
            agent(TokenState::Healthy, 0),
        ];
        let s = summarize(&agents);
        assert_eq!(s.total, 4);
        assert_eq!(s.rejected, 2);
        assert_eq!(s.degraded, 1);
        assert_eq!(s.healthy, 1);
        assert_eq!(s.rejected + s.degraded + s.healthy, s.total);
    }

    /// An empty registry must not masquerade as healthy — every bucket is
    /// zero, including `healthy`, so no caller can read a green count.
    #[test]
    fn empty_registry_reports_zero_not_healthy() {
        let s = summarize(&[]);
        assert_eq!(s.total, 0);
        assert_eq!(s.healthy, 0);
        assert_eq!(s.rejected, 0);
        assert_eq!(s.degraded, 0);
    }

    /// The published record must never carry token material. Serialize a
    /// populated report and assert the wire shape has no token-ish key —
    /// this fails loudly if someone adds one to `AgentTokenHealth`.
    #[test]
    fn wire_shape_carries_no_token_material() {
        let report = TokenHealthReport {
            summary: summarize(&[agent(TokenState::Rejected, 0)]),
            agents: vec![agent(TokenState::Rejected, 0)],
        };
        let json = serde_json::to_string(&report).expect("serializes");
        assert!(!json.contains("\"token\""), "leaked a token field: {json}");
        assert!(!json.contains("Bearer"), "leaked a bearer value: {json}");
        // The non-secret rotation witness IS expected to be present.
        assert!(json.contains("\"jti\""));
        assert!(json.contains("\"state\":\"rejected\""));
    }
}
