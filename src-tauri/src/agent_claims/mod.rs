//! Process-global registry of active claim heartbeats per agent.
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-18-agent-spawn-coordination.md` Phase 3.
//!
//! When the runner's spawn flow pre-acquires a coord claim via
//! `POST /claims/acquire`, this module:
//!
//! 1. Spawns a tokio task that posts `/claims/heartbeat` every TTL/3
//!    seconds. On `HeartbeatResult::Stolen`, the task emits the Tauri
//!    `agent-claim-stolen` event to the webview and exits.
//! 2. Holds the [`ClaimHeartbeatHandle`] in a process-global map keyed
//!    by `agent_id` (UUID string), so [`release_for_agent`] can be
//!    called from the agent-completion / abandon path. Dropping the
//!    handle cancels the heartbeat task on its next tick boundary.
//!
//! Best-effort throughout. A missing claim heartbeat degrades
//! coordination observability — coord will eventually evict the Redis
//! key on TTL expiry — but never breaks correctness. The runner side
//! of this protocol mirrors `agent_daemons` (same per-agent registry
//! shape, same cancel-on-drop pattern, sibling concern).
//!
//! Tauri event emission requires an `AppHandle`. We resolve it via
//! [`crate::tauri_app_handle::current`] (set once at startup from
//! `main.rs`); if no handle is set (early-startup race or in unit
//! tests) the emit is silently dropped — by design, since the
//! webview isn't there to listen yet.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use tauri::Emitter;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::agent_worktree::{
    release_claim_best_effort, spawn_heartbeat_task, ActiveClaim, ClaimHeartbeatHandle,
};

/// Tauri event subject the webview's ConflictModal subscribes to.
pub const EVENT_CLAIM_CONFLICT: &str = "agent-claim-conflict";
/// Tauri event subject the webview's StolenBanner subscribes to.
pub const EVENT_CLAIM_STOLEN: &str = "agent-claim-stolen";

/// Payload shape emitted on `agent-claim-stolen`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StolenEventPayload {
    pub kind: String,
    pub resource_key: String,
    pub current_holder: Option<String>,
    pub agent_id: String,
}

static REGISTRY: OnceLock<Mutex<HashMap<String, ClaimHeartbeatHandle>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, ClaimHeartbeatHandle>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Spawn the per-agent heartbeat task and register it in the process
/// registry. Re-registering the same agent_id drops the prior handle,
/// stopping the old task on its next tick.
///
/// `coord_http_base` is the coord URL (already converted from ws→http).
/// `claim` carries `kind` / `resource_key` / `ttl_seconds` from the
/// `AllocateResult`.
pub fn register_active_claim(
    agent_id: &str,
    coord_http_base: String,
    machine_id: Uuid,
    claim: &ActiveClaim,
) {
    let kind = claim.kind.clone();
    let resource_key = claim.resource_key.clone();
    let ttl_seconds = claim.ttl_seconds;
    let agent_id_owned = agent_id.to_string();

    let on_stolen_agent_id = agent_id_owned.clone();
    let on_stolen_kind = kind.clone();
    let on_stolen_key = resource_key.clone();
    let handle = spawn_heartbeat_task(
        coord_http_base,
        machine_id,
        &kind,
        resource_key.clone(),
        ttl_seconds,
        move |current_holder| {
            emit_stolen_event(
                &on_stolen_agent_id,
                &on_stolen_kind,
                &on_stolen_key,
                current_holder,
            );
        },
    );

    let mut reg = match registry().lock() {
        Ok(g) => g,
        Err(e) => {
            warn!("agent_claims registry poisoned: {e}");
            return;
        }
    };
    reg.insert(agent_id_owned.clone(), handle);
    info!(
        "agent_claims: registered agent_id={} kind={} key={} ttl={}s",
        agent_id_owned, kind, resource_key, ttl_seconds
    );
}

/// Stop the heartbeat task for `agent_id` and best-effort release the
/// claim. Idempotent — no-op if no handle is registered.
pub async fn release_for_agent(agent_id: &str) {
    let handle = {
        let mut reg = match registry().lock() {
            Ok(g) => g,
            Err(e) => {
                warn!("agent_claims registry poisoned in release_for_agent: {e}");
                return;
            }
        };
        reg.remove(agent_id)
    };
    let Some(h) = handle else {
        debug!("agent_claims: release_for_agent {agent_id} — no handle registered");
        return;
    };
    let coord_http_base = h.coord_http_base.clone();
    let machine_id = h.machine_id;
    let kind = h.kind.clone();
    let resource_key = h.resource_key.clone();
    // Drop the handle to cancel the heartbeat task.
    drop(h);
    // Then call /claims/release. Best-effort.
    release_claim_best_effort(&coord_http_base, machine_id, &kind, &resource_key).await;
    info!(
        "agent_claims: released agent_id={} kind={} key={}",
        agent_id, kind, resource_key
    );
}

/// Number of registered claim heartbeats — exposed for diagnostics + tests.
pub fn tracked_count() -> usize {
    registry().lock().map(|g| g.len()).unwrap_or(0)
}

/// Returns `true` iff `agent_id` has an active claim heartbeat
/// registered. Test/debug helper.
pub fn tracked_contains(agent_id: &str) -> bool {
    registry()
        .lock()
        .map(|g| g.contains_key(agent_id))
        .unwrap_or(false)
}

fn emit_stolen_event(
    agent_id: &str,
    kind: &str,
    resource_key: &str,
    current_holder: Option<String>,
) {
    // Drop from the registry so a subsequent /claims/acquire for the
    // same agent doesn't race against the now-stale handle.
    if let Ok(mut reg) = registry().lock() {
        reg.remove(agent_id);
    }

    let payload = StolenEventPayload {
        kind: kind.to_string(),
        resource_key: resource_key.to_string(),
        current_holder,
        agent_id: agent_id.to_string(),
    };
    match crate::tauri_app_handle::current() {
        Some(app) => {
            if let Err(e) = app.emit(EVENT_CLAIM_STOLEN, &payload) {
                warn!("agent_claims: emit {EVENT_CLAIM_STOLEN} failed: {e}");
            }
        }
        None => {
            debug!(
                "agent_claims: no AppHandle registered — dropping {EVENT_CLAIM_STOLEN} \
                 emit for agent_id={agent_id}"
            );
        }
    }
}

/// Emit the `agent-claim-conflict` Tauri event so the ConflictModal
/// renders. Called by the Tauri command that drives the spawn flow
/// from the webview — the HTTP `/agents/allocate-local` path returns
/// the conflict as a structured 409 body, and the IPC translator
/// surfaces it as an event here.
pub fn emit_conflict_event(
    kind: &str,
    resource_key: &str,
    current_holder: &str,
    intent: Option<&str>,
) {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ConflictPayload<'a> {
        kind: &'a str,
        resource_key: &'a str,
        current_holder: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        intent: Option<&'a str>,
    }
    let payload = ConflictPayload {
        kind,
        resource_key,
        current_holder,
        intent,
    };
    match crate::tauri_app_handle::current() {
        Some(app) => {
            if let Err(e) = app.emit(EVENT_CLAIM_CONFLICT, &payload) {
                warn!("agent_claims: emit {EVENT_CLAIM_CONFLICT} failed: {e}");
            }
        }
        None => {
            debug!(
                "agent_claims: no AppHandle registered — dropping {EVENT_CLAIM_CONFLICT} \
                 emit for kind={kind} key={resource_key}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_count_starts_empty_or_consistent() {
        // The registry is process-global; other tests may register, so
        // we can only assert non-panicking access here.
        let _ = tracked_count();
    }

    #[test]
    fn release_unknown_agent_is_noop() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // No handle registered for this id — should be a silent no-op.
            release_for_agent("00000000-0000-0000-0000-0000000000ff").await;
        });
    }
}
