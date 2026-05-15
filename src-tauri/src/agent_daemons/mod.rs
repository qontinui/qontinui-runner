//! Unified per-agent daemon spawn site.
//!
//! Before this module, two long-lived per-agent Tokio daemons existed
//! with **no shared spawn path**:
//!
//! - `agent_pusher` (Row 9 Phase 3) — periodic `git push` to the
//!   coord-hosted origin. Built + `tick_once`-tested but its `spawn`
//!   had **no production call site** (coordination plan §3.0 step 3:
//!   PTY/spawn wiring not yet on disk).
//! - `dirty_poller` (Coordination Phase 6) — 5s `git status` →
//!   `POST /agents/:id/dirty-state`. Self-registered at
//!   `/agents/allocate-local` via its own `spawn_and_register` +
//!   private handle registry.
//!
//! Each carried its **own** `TokenSlot` and its **own** refresh loop,
//! so an agent ran two independent refreshers hitting coord's
//! `POST /agents/:id/refresh-token` — 2× the refresh load at 300
//! agents and two independently-rotating jtis per agent.
//!
//! This module is the single spawn site: one
//! [`crate::agent_token::SharedToken`] per agent, both daemons built
//! against it (`*State::with_shared_token`), one combined handle held
//! in one process-global registry keyed by `agent_id`. Re-allocating
//! the same agent drops the prior [`AgentDaemons`] → both old daemons
//! stop cleanly on their next tick.
//!
//! The unification also gives `agent_pusher` its first real spawn
//! site: every worktree-mode allocation now starts the pusher too,
//! not just the poller.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tracing::{info, warn};
use uuid::Uuid;

use crate::agent_pusher::{self, PusherHandle, PusherState};
use crate::agent_token;
use crate::agent_worktree::AllocateResult;
use crate::dirty_poller::{self, DirtyPollerHandle, DirtyPollerState};

/// Both daemons for one agent. Dropping this stops both: each handle's
/// `Drop` signals its task to exit on the next tick.
#[derive(Default)]
struct AgentDaemons {
    pusher: Option<PusherHandle>,
    poller: Option<DirtyPollerHandle>,
}

/// Process-global registry. Agents are long-lived; teardown is
/// best-effort and dies with the runner (the trade-off `agent_pusher`
/// documents). Keyed by `agent_id` so a re-allocation replaces — and
/// thereby stops — the prior daemons.
static REGISTRY: OnceLock<Mutex<HashMap<Uuid, AgentDaemons>>> = OnceLock::new();

/// Spawn (and retain) the pusher + poller for one freshly-allocated
/// agent, both sharing one refreshing token slot.
///
/// No-op when the allocation carried no JWT (coord JWT keys
/// unconfigured / dev fallback): both daemons' coord calls are
/// JWT-gated, so there is nothing to run. Best-effort throughout — a
/// missing daemon degrades observability/durability, never
/// correctness.
pub fn spawn_for_agent(allocate: &AllocateResult, coord_http_base: String, machine_id: Uuid) {
    let Some(token) = agent_token::from_allocate_result(allocate) else {
        info!(
            "agent_daemons: agent_id={} — no token in allocation; pusher+poller skipped (dev/no-JWT coord)",
            allocate.agent_id
        );
        return;
    };
    let agent_id = match Uuid::parse_str(&allocate.agent_id) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                "agent_daemons: unparseable agent_id {:?} ({e}) — daemons skipped",
                allocate.agent_id
            );
            return;
        }
    };

    let mut daemons = AgentDaemons::default();

    match PusherState::with_shared_token(allocate, coord_http_base.clone(), token.clone()) {
        Some(ps) => daemons.pusher = Some(agent_pusher::spawn(Arc::new(ps))),
        None => info!("agent_daemons: agent_id={agent_id} pusher skipped (no push targets)"),
    }
    match DirtyPollerState::with_shared_token(allocate, coord_http_base, machine_id, token) {
        Some(dp) => daemons.poller = Some(dirty_poller::spawn(Arc::new(dp))),
        None => info!("agent_daemons: agent_id={agent_id} poller skipped (no worktrees)"),
    }

    let reg = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut g) = reg.lock() {
        // Insert drops any prior AgentDaemons for this agent → both
        // old daemons observe cancellation and exit next tick.
        g.insert(agent_id, daemons);
        info!(
            "agent_daemons: agent_id={agent_id} pusher+poller live (agents tracked={})",
            g.len()
        );
    }
}

/// Test-only: is a specific agent currently tracked? Per-agent so
/// parallel tests don't race on a shared total count.
#[cfg(test)]
fn tracked_contains(id: Uuid) -> bool {
    REGISTRY
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.contains_key(&id)))
        .unwrap_or(false)
}

/// Test-only: how many `AgentDaemons` entries exist for `id` (a
/// HashMap key is unique, so this is 0 or 1 — proves re-allocation
/// replaces rather than accumulates).
#[cfg(test)]
fn entries_for(id: Uuid) -> usize {
    REGISTRY
        .get()
        .and_then(|m| m.lock().ok())
        .map(|g| g.keys().filter(|k| **k == id).count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_worktree::MaterializedWorktree;

    fn alloc(agent: &str, token: &str) -> AllocateResult {
        AllocateResult {
            agent_id: agent.to_string(),
            worktrees: vec![MaterializedWorktree {
                repo: "qontinui-coord".into(),
                branch: "agent/m-a".into(),
                parent_sha: "0".repeat(40),
                worktree_path: std::env::temp_dir(),
                push_ref: "refs/agent/m-a".into(),
            }],
            token: token.to_string(),
            token_jti: Uuid::nil(),
            token_exp: chrono::Utc::now().timestamp() + 4 * 3600,
        }
    }

    /// No token ⇒ no daemons for that agent (per-agent assertion, so
    /// parallel tests don't race a shared total).
    #[tokio::test]
    async fn tokenless_allocation_is_noop() {
        let id = Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap();
        spawn_for_agent(
            &alloc(&id.to_string(), ""),
            "http://127.0.0.1:1".into(),
            Uuid::nil(),
        );
        assert!(!tracked_contains(id), "tokenless alloc must not register");
    }

    /// A tokened allocation registers the agent; re-allocating the
    /// same agent_id replaces in place (still exactly one entry — the
    /// prior `AgentDaemons` is dropped, stopping both old daemons; the
    /// Drop→stop path itself is covered by the handles' own tests).
    /// Unreachable coord base ⇒ the spawned tasks just error+retry.
    #[tokio::test]
    async fn tokened_allocation_registers_then_replaces() {
        let id = Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap();
        spawn_for_agent(
            &alloc(&id.to_string(), "tok"),
            "http://127.0.0.1:1".into(),
            Uuid::nil(),
        );
        assert!(tracked_contains(id));
        assert_eq!(entries_for(id), 1);
        spawn_for_agent(
            &alloc(&id.to_string(), "tok2"),
            "http://127.0.0.1:1".into(),
            Uuid::nil(),
        );
        assert_eq!(
            entries_for(id),
            1,
            "re-allocation of the same agent must replace, not accumulate"
        );
    }
}
