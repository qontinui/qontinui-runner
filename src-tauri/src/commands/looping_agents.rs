//! Looping-agent control-surface commands (Tier-0 primitive, Phase 1).
//!
//! Thin wrappers only: all decision logic lives in the lib crate
//! (`qontinui_runner_lib::looping_agent`) and the bin glue
//! (`crate::looping_agent_supervisor`) — this module just resolves managed
//! state and shapes the wire response, because the `commands` module is
//! declared in `main.rs` (NOT `lib.rs`) and is therefore invisible to
//! `cargo test --lib`; keeping logic out of here keeps it tested.
//!
//! Registered directly in main.rs's `generate_handler![...]` list (NOT the
//! plugin pattern — a `plugin()` fn that main.rs never calls ships commands
//! that silently don't exist; that exact failure shipped the DOA clone
//! picker).

use std::sync::Arc;

use tauri::Manager;
use tracing::info;

use crate::looping_agent_supervisor::{status_snapshot, LoopingAgentStatus};
use qontinui_runner_lib::looping_agent::registry::LoopingAgentRegistry;

/// Resolve the managed registry, or a stable error when the supervisor never
/// started (e.g. its store failed to open).
fn registry(app: &tauri::AppHandle) -> Result<Arc<LoopingAgentRegistry>, String> {
    app.try_state::<Arc<LoopingAgentRegistry>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| "looping-agent registry not available (supervisor not started)".to_string())
}

/// List every looping agent with its live status.
#[tauri::command]
pub fn looping_agents_list(app: tauri::AppHandle) -> Result<Vec<LoopingAgentStatus>, String> {
    let reg = registry(&app)?;
    Ok(reg
        .list()
        .iter()
        .map(|rec| status_snapshot(&app, rec))
        .collect())
}

/// Live status of one looping agent.
#[tauri::command]
pub fn looping_agent_status(
    app: tauri::AppHandle,
    agent_id: String,
) -> Result<LoopingAgentStatus, String> {
    let reg = registry(&app)?;
    let rec = reg
        .get(&agent_id)
        .ok_or_else(|| format!("unknown looping agent '{agent_id}'"))?;
    Ok(status_snapshot(&app, &rec))
}

/// Flip an agent's master switch. Enabling sets `desired_state=running` (the
/// supervisor spawns/adopts a tab within a tick); disabling sets `stopped`
/// (no more nudges/relaunches/respawns — an existing tab is left alone for
/// the operator to inspect or close).
#[tauri::command]
pub fn looping_agent_set_enabled(
    app: tauri::AppHandle,
    agent_id: String,
    enabled: bool,
) -> Result<LoopingAgentStatus, String> {
    let reg = registry(&app)?;
    let rec = reg
        .set_enabled(&agent_id, enabled)
        .ok_or_else(|| format!("unknown looping agent '{agent_id}'"))?;
    info!(
        agent = %agent_id,
        enabled,
        "looping_agents: operator flipped looping agent"
    );
    Ok(status_snapshot(&app, &rec))
}
