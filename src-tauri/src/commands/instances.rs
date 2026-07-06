//! Tauri commands for managing runner instances (dev feature).

use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Manager;
use tauri::State;
use tracing::info;

use super::CommandResponse;
use crate::commands::compartments::HealthCompartment;
use crate::instance_manager::InstanceManager;
use crate::settings::{self, RunnerInstanceConfig, SpawnPlacement};
use crate::spawn_placement::{resolve_to_global_physical, ResolvedPlacement};

/// Get every runner instance the picker should know about: every slot in
/// `settings.json` plus every non-primary entry in the DB registry that
/// isn't already covered by a slot. Each row has its `running`/`api_ready`
/// resolved from a live `/status` probe so the Orchestration Loop target
/// drop-down (and the Settings → Runner Instances panel) reflects what's
/// actually alive, including externally-spawned children that registered
/// themselves but were never saved as a configured slot.
#[tauri::command]
pub async fn get_runner_instances(
    instance_manager: State<'_, Arc<InstanceManager>>,
) -> Result<serde_json::Value, String> {
    let configs = settings::get_runner_instances();
    let statuses = instance_manager.get_unified_instances(&configs).await;
    serde_json::to_value(&statuses).map_err(|e| e.to_string())
}

/// Save or update an instance configuration.
#[tauri::command]
pub async fn save_runner_instance(
    id: String,
    name: String,
    port: u16,
    spawn_placement: Option<SpawnPlacement>,
) -> Result<CommandResponse, String> {
    if port < 1024 {
        return Err(format!(
            "Port {} is in the privileged range (0-1023). Use a port >= 1024.",
            port
        ));
    }
    let config = RunnerInstanceConfig {
        id,
        name: name.clone(),
        port,
        spawn_placement,
    };
    settings::save_runner_instance(config)?;
    info!("Saved runner instance config: {} on port {}", name, port);
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Instance '{}' saved", name)),
        data: None,
    })
}

/// Preview how a `SpawnPlacement` would resolve against the current
/// monitor layout, without persisting anything. The UI calls this
/// while the user drags / edits the placement so they can see the
/// resulting global coords before saving.
#[tauri::command]
pub async fn preview_spawn_placement(
    app: tauri::AppHandle,
    placement: SpawnPlacement,
) -> Result<ResolvedPlacement, String> {
    resolve_to_global_physical(&app, &placement)
}

/// Per-monitor metadata returned to the placement editor UI. Mirrors
/// the shape of `MonitorInfoResponse` from `mcp::types` so a frontend
/// can use either source interchangeably.
#[derive(Debug, serde::Serialize)]
pub struct MonitorListEntry {
    pub index: usize,
    pub name: Option<String>,
    /// Spatial role: "left" / "center" / "right" (or "center" if there
    /// is only one monitor). Matches `mcp::monitors::get_monitors`.
    pub position_label: String,
    /// Top-left corner of the monitor in virtual-desktop physical px.
    pub x: i32,
    pub y: i32,
    /// Monitor size in physical pixels.
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub is_primary: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct MonitorList {
    pub count: usize,
    pub monitors: Vec<MonitorListEntry>,
}

/// List monitors with the metadata the placement editor needs. This
/// duplicates the labeling logic from `mcp::monitors::get_monitors`
/// (which returns a slightly different shape and goes through the MCP
/// HTTP server) so the Tauri command path doesn't have to spin up the
/// HTTP layer just to get monitor info.
#[tauri::command]
pub async fn list_monitors_for_placement(app: tauri::AppHandle) -> Result<MonitorList, String> {
    let monitors = app
        .available_monitors()
        .map_err(|e| format!("available_monitors failed: {}", e))?;

    let primary = app.primary_monitor().ok().flatten();

    let xs: Vec<i32> = monitors.iter().map(|m| m.position().x).collect();
    let min_x = xs.iter().min().copied().unwrap_or(0);
    let max_x = xs.iter().max().copied().unwrap_or(0);
    let single = monitors.len() == 1;

    let entries: Vec<MonitorListEntry> = monitors
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let pos = m.position();
            let size = m.size();
            let is_primary = match &primary {
                Some(prim) => {
                    let pp = prim.position();
                    let ps = prim.size();
                    pos.x == pp.x
                        && pos.y == pp.y
                        && size.width == ps.width
                        && size.height == ps.height
                }
                None => idx == 0,
            };
            let position_label = if single {
                "center".to_string()
            } else if pos.x == min_x {
                "left".to_string()
            } else if pos.x == max_x {
                "right".to_string()
            } else {
                "center".to_string()
            };
            MonitorListEntry {
                index: idx,
                name: m.name().map(|n| n.to_string()),
                position_label,
                x: pos.x,
                y: pos.y,
                width: size.width,
                height: size.height,
                scale_factor: m.scale_factor(),
                is_primary,
            }
        })
        .collect();

    Ok(MonitorList {
        count: entries.len(),
        monitors: entries,
    })
}

/// Delete an instance configuration (only when stopped).
#[tauri::command]
pub async fn delete_runner_instance(
    id: String,
    instance_manager: State<'_, Arc<InstanceManager>>,
) -> Result<CommandResponse, String> {
    // Check that it's not running
    let configs = settings::get_runner_instances();
    if let Some(config) = configs.iter().find(|c| c.id == id) {
        let status = instance_manager.get_instance_status(config).await;
        if status.running {
            return Err("Cannot delete a running instance. Stop it first.".into());
        }
    }
    settings::delete_runner_instance(&id)?;
    info!("Deleted runner instance config: {}", id);
    Ok(CommandResponse {
        success: true,
        message: Some("Instance deleted".into()),
        data: None,
    })
}

/// Launch a runner instance.
#[tauri::command]
pub async fn launch_runner_instance(
    app: tauri::AppHandle,
    id: String,
    instance_manager: State<'_, Arc<InstanceManager>>,
) -> Result<CommandResponse, String> {
    let configs = settings::get_runner_instances();
    let config = configs
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| format!("Instance '{}' not found in settings", id))?;

    let pid = instance_manager
        .launch_instance_with_app(config, Some(&app))
        .await?;
    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Instance '{}' launched (PID: {})",
            config.name, pid
        )),
        data: Some(serde_json::json!({ "pid": pid })),
    })
}

/// Stop a running instance.
#[tauri::command]
pub async fn stop_runner_instance(
    id: String,
    instance_manager: State<'_, Arc<InstanceManager>>,
) -> Result<CommandResponse, String> {
    instance_manager.stop_instance(&id).await?;
    Ok(CommandResponse {
        success: true,
        message: Some("Instance stopped".into()),
        data: None,
    })
}

/// Get the current list of temp-runner spawn placements. Distinct from
/// `runner_instances[i].spawn_placement` — the supervisor uses these
/// when spawning temp runners via `POST /runners/spawn-test`.
#[tauri::command]
pub async fn get_temp_spawn_placements() -> Result<Vec<SpawnPlacement>, String> {
    Ok(settings::get_temp_spawn_placements())
}

/// Replace the temp-runner spawn placement list. Each placement is
/// validated by attempting to resolve it against the live monitor list
/// (a smoke test — invalid placements are rejected before persistence).
/// Returns the persisted list on success.
#[tauri::command]
pub async fn set_temp_spawn_placements(
    app: tauri::AppHandle,
    placements: Vec<SpawnPlacement>,
) -> Result<Vec<SpawnPlacement>, String> {
    // Smoke-test each placement against the live monitor layout. This
    // mirrors the validation other placement-handling commands do
    // implicitly via `preview_spawn_placement`.
    for (idx, placement) in placements.iter().enumerate() {
        resolve_to_global_physical(&app, placement)
            .map_err(|e| format!("placement[{}] failed to resolve: {}", idx, e))?;
    }
    settings::save_temp_spawn_placements(placements.clone())?;
    info!("Saved {} temp spawn placement(s)", placements.len());
    Ok(placements)
}

/// One worktree discovered via `git worktree list --porcelain`, surfaced to
/// the "Test My Change" dev-loop UI so a developer can pick the checkout to
/// build into an isolated temp runner. Mirrors the TS `DevLoopWorktree`
/// interface in `DevLoopSettings.tsx`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DevLoopWorktree {
    /// Absolute path to the worktree root (the `worktree <path>` line).
    pub path: String,
    /// Branch short name (`refs/heads/<name>` → `<name>`), or `None` when
    /// the worktree is detached / bare.
    pub branch: Option<String>,
    /// The checked-out HEAD sha, if reported.
    pub head: Option<String>,
    /// True for the first entry — the main worktree (the live tree). The
    /// supervisor rejects the live tree as a `worktree_path`, so the UI
    /// steers the user to a git ref for this one.
    pub is_main: bool,
    /// True when the worktree has no branch (`detached` line present).
    pub is_detached: bool,
    /// True when `<path>/src-tauri/Cargo.toml` exists on disk — i.e. this
    /// checkout can actually be built by the supervisor's spawn-test.
    pub buildable: bool,
}

/// Discover the git worktrees under `repo_root` for the "Test My Change"
/// dev-loop UI. `repo_root` is typically the supervisor's `project_dir`
/// (`.../qontinui-runner/src-tauri`); if it ends in `src-tauri` we walk up
/// to the repo root before asking git.
///
/// Returns ALL worktrees (including main, flagged `is_main`). On any git
/// failure (non-zero exit, git not on PATH) returns `Err(String)` — the
/// frontend treats that as "discovery unavailable" and falls back to manual
/// entry of a git ref or path.
#[tauri::command]
pub async fn list_repo_worktrees(repo_root: String) -> Result<Vec<DevLoopWorktree>, String> {
    use std::path::Path;

    // Normalize: if the final path component is `src-tauri` (case-insensitive)
    // use the parent directory as the git repo root, otherwise use as-is.
    let root_path = Path::new(&repo_root);
    let git_root: &Path = match root_path.file_name().and_then(|n| n.to_str()) {
        Some(name) if name.eq_ignore_ascii_case("src-tauri") => {
            root_path.parent().unwrap_or(root_path)
        }
        _ => root_path,
    };

    let output = crate::process_helpers::no_window("git")
        .arg("-C")
        .arg(git_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git worktree list failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees: Vec<DevLoopWorktree> = Vec::new();

    // Porcelain format: records separated by blank lines. Each record starts
    // with `worktree <path>` and may carry `HEAD <sha>`, `branch refs/...`,
    // `detached`, `bare`.
    let mut current_path: Option<String> = None;
    let mut current_head: Option<String> = None;
    let mut current_branch: Option<String> = None;
    let mut current_detached = false;

    let flush = |path: Option<String>,
                 head: Option<String>,
                 branch: Option<String>,
                 detached: bool,
                 is_main: bool|
     -> Option<DevLoopWorktree> {
        let path = path?;
        let buildable = Path::new(&path)
            .join("src-tauri")
            .join("Cargo.toml")
            .exists();
        Some(DevLoopWorktree {
            path,
            branch,
            head,
            is_main,
            is_detached: detached,
            buildable,
        })
    };

    for line in stdout.lines() {
        if line.is_empty() {
            // End of a record.
            if let Some(wt) = flush(
                current_path.take(),
                current_head.take(),
                current_branch.take(),
                current_detached,
                worktrees.is_empty(),
            ) {
                worktrees.push(wt);
            }
            current_detached = false;
            continue;
        }

        if let Some(rest) = line.strip_prefix("worktree ") {
            current_path = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            current_head = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            current_branch = Some(rest.strip_prefix("refs/heads/").unwrap_or(rest).to_string());
        } else if line == "detached" {
            current_detached = true;
        }
        // `bare` and any other lines are ignored.
    }

    // Flush a trailing record (porcelain output may not end with a blank line).
    if let Some(wt) = flush(
        current_path.take(),
        current_head.take(),
        current_branch.take(),
        current_detached,
        worktrees.is_empty(),
    ) {
        worktrees.push(wt);
    }

    Ok(worktrees)
}

/// Get identity info about this runner instance: whether it's secondary and what primary it proxies to.
#[tauri::command]
pub async fn get_runner_identity(
    health: State<'_, HealthCompartment>,
) -> Result<serde_json::Value, String> {
    let is_secondary = crate::process_capture::primary_proxy::is_secondary();
    let instance_name = crate::instance::instance_name();
    let primary_port = crate::process_capture::primary_proxy::primary_port();
    let own_port = health.api_port().load(std::sync::atomic::Ordering::Relaxed);

    Ok(serde_json::json!({
        "is_secondary": is_secondary,
        "instance_name": instance_name,
        "primary_port": primary_port,
        "port": own_port,
    }))
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// The plugin is non-generic (defaulting to `tauri::Wry`) because
/// some commands here take `tauri::AppHandle` (Wry-default) directly.
/// This matches the pattern used by `commands::ui_bridge` and
/// `commands::ai_session`.
pub fn plugin() -> TauriPlugin<tauri::Wry> {
    PluginBuilder::new("qontinui_instances")
        .invoke_handler(tauri::generate_handler![
            get_runner_instances,
            save_runner_instance,
            delete_runner_instance,
            launch_runner_instance,
            stop_runner_instance,
            get_runner_identity,
            list_repo_worktrees,
            preview_spawn_placement,
            list_monitors_for_placement,
            get_temp_spawn_placements,
            set_temp_spawn_placements,
        ])
        .build()
}
