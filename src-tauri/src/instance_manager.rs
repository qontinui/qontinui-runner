//! Instance Manager for spawning and tracking secondary runner processes.
//!
//! Each instance runs on its own port (via `QONTINUI_PORT` env var) and gets
//! its own Tauri window. The shared PostgreSQL database handles concurrent
//! access from multiple instances.
//!
//! Active instance IDs are persisted to `active_instances.json` so that
//! instances can be restored after a rebuild / restart.  The file is written
//! on every launch/stop and **deleted** on intentional close so that a normal
//! shutdown does not trigger restoration on the next start.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::database::pg::PgDb;
use crate::settings::{RunnerInstanceConfig, SpawnPlacement};

/// Status of a runner instance.
#[derive(Debug, Clone, Serialize)]
pub struct InstanceStatus {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub running: bool,
    pub pid: Option<u32>,
    pub api_ready: bool,
    /// `"configured"` for entries in `settings.json`, `"discovered"` for
    /// entries that exist only in the DB registry (e.g. supervisor-spawned
    /// runners that registered themselves but were never saved as a slot).
    pub source: &'static str,
    /// Per-instance spawn-window placement, if configured. Only present
    /// for `"configured"` entries — `"discovered"` rows from the DB
    /// registry don't carry placement info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_placement: Option<SpawnPlacement>,
}

/// Handle for a spawned runner instance.
struct InstanceHandle {
    config: RunnerInstanceConfig,
    child: std::process::Child,
}

/// What `deregister_instance` cleaned up. The two flags can disagree —
/// in-memory has the entry only if THIS runner accepted the original
/// `register_instance` call, while the DB row may persist independently
/// from a previous session that crashed before deregistering. Callers
/// returning HTTP responses use `.any()` to decide between 200 and 404.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DeregisterResult {
    pub removed_in_memory: bool,
    pub removed_db: bool,
}

impl DeregisterResult {
    pub fn any(self) -> bool {
        self.removed_in_memory || self.removed_db
    }
}

/// An externally-registered runner instance (not a child process of this runner).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredInstance {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub pid: Option<u32>,
    pub registered_at: chrono::DateTime<chrono::Utc>,
    pub last_heartbeat: chrono::DateTime<chrono::Utc>,
    pub running_tasks: u32,
}

/// Manages spawned runner instance processes and externally-registered instances.
pub struct InstanceManager {
    /// Child processes spawned by this runner.
    instances: Mutex<HashMap<String, InstanceHandle>>,
    /// Externally-registered instances (not child processes).
    registered: Mutex<HashMap<String, RegisteredInstance>>,
    /// PostgreSQL database for persistent instance registry.
    pg_db: Arc<PgDb>,
}

impl InstanceManager {
    pub fn new(pg_db: Arc<PgDb>) -> Self {
        Self {
            instances: Mutex::new(HashMap::new()),
            registered: Mutex::new(HashMap::new()),
            pg_db,
        }
    }

    /// Register the primary runner in the DB on startup.
    pub async fn register_self_as_primary(&self) {
        let port = crate::mcp::types::get_mcp_api_port();
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "localhost".to_string());
        let pid = std::process::id();
        let id = format!("primary-{}", port);

        eprintln!(
            "[InstanceManager] register_self_as_primary: id={} port={}",
            id, port
        );
        match self
            .pg_db
            .upsert_runner_instance(&id, "primary", port, &hostname, true, Some(pid), "healthy")
            .await
        {
            Ok(()) => {
                eprintln!("[InstanceManager] register_self_as_primary: OK");
                info!(
                    "Registered primary runner in DB (port={}, pid={})",
                    port, pid
                );
            }
            Err(e) => {
                eprintln!("[InstanceManager] register_self_as_primary: ERR: {}", e);
                warn!("Failed to register primary in DB: {}", e);
            }
        }
    }

    /// Register an externally-started runner instance.
    /// Returns the assigned or existing instance ID.
    pub async fn register_instance(&self, name: String, port: u16, pid: Option<u32>) -> String {
        let mut registered = self.registered.lock().await;

        // Check if already registered by port
        if let Some(existing) = registered.values_mut().find(|r| r.port == port) {
            existing.name = name;
            existing.pid = pid;
            existing.last_heartbeat = chrono::Utc::now();
            info!(
                "Updated registration for instance '{}' on port {}",
                existing.name, existing.port
            );
            return existing.id.clone();
        }

        let id = format!(
            "ext-{}-{}",
            port,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                % 100000
        );
        let now = chrono::Utc::now();
        let inst = RegisteredInstance {
            id: id.clone(),
            name: name.clone(),
            port,
            pid,
            registered_at: now,
            last_heartbeat: now,
            running_tasks: 0,
        };
        info!(
            "Registered external instance '{}' (id={}, port={})",
            name, id, port
        );
        registered.insert(id.clone(), inst);
        drop(registered); // release lock before async DB call

        // Write-through to PostgreSQL
        let hostname = "localhost".to_string();
        match self
            .pg_db
            .upsert_runner_instance(&id, &name, port, &hostname, false, pid, "healthy")
            .await
        {
            Ok(()) => info!(
                "DB write-through: registered instance {} on port {}",
                id, port
            ),
            Err(e) => warn!("Failed to persist registered instance to DB: {}", e),
        }

        id
    }

    /// Update heartbeat for a registered instance.
    pub async fn update_heartbeat(&self, id: &str, running_tasks: Option<u32>) -> bool {
        let mut registered = self.registered.lock().await;
        if let Some(inst) = registered.get_mut(id) {
            inst.last_heartbeat = chrono::Utc::now();
            if let Some(count) = running_tasks {
                inst.running_tasks = count;
            }
            drop(registered);

            // Write-through to DB
            let _ = self
                .pg_db
                .update_runner_instance_heartbeat(id, running_tasks, "healthy")
                .await;
            return true;
        }
        drop(registered);
        false
    }

    /// Deregister an externally-registered instance from both the in-memory
    /// `registered` map and the persistent DB registry. Either side may be
    /// empty for a given id — the in-memory map is per-process so a row
    /// from a previous session lives only in the DB; conversely a freshly
    /// registered instance whose DB write-through failed lives only in
    /// memory. The returned `DeregisterResult` tells callers which side(s)
    /// were actually cleaned up.
    pub async fn deregister_instance(&self, id: &str) -> DeregisterResult {
        let mut registered = self.registered.lock().await;
        let removed_in_memory = registered.remove(id).is_some();
        drop(registered);

        let removed_db = self.pg_db.remove_runner_instance(id).await.unwrap_or(false);

        DeregisterResult {
            removed_in_memory,
            removed_db,
        }
    }

    /// Get all registered (external) instances.
    pub async fn get_registered_instances(&self) -> Vec<RegisteredInstance> {
        let registered = self.registered.lock().await;
        registered.values().cloned().collect()
    }

    /// Remove registered instances whose ports are not reachable.
    /// Returns the number of instances removed.
    pub async fn purge_unreachable_registered(&self) -> u32 {
        let registered = self.registered.lock().await;
        let to_check: Vec<(String, u16)> = registered
            .values()
            .map(|r| (r.id.clone(), r.port))
            .collect();
        drop(registered);

        let mut removed = 0u32;
        for (id, port) in &to_check {
            if !crate::process_capture::health::is_port_in_use(*port) {
                self.deregister_instance(id).await;
                info!("Purged unreachable registered instance (port {})", port);
                removed += 1;
            }
        }
        removed
    }

    /// Allocate the next free port in the 9877-9899 range.
    ///
    /// Three-way check: in-memory child processes + DB registry + TCP port probe.
    pub async fn allocate_port(&self) -> Result<u16, String> {
        let instances = self.instances.lock().await;
        let child_ports: std::collections::HashSet<u16> =
            instances.values().map(|h| h.config.port).collect();
        drop(instances);

        let registered = self.registered.lock().await;
        let reg_ports: std::collections::HashSet<u16> =
            registered.values().map(|r| r.port).collect();
        drop(registered);

        let db_ports: std::collections::HashSet<u16> = self
            .pg_db
            .get_all_runner_instances()
            .await
            .unwrap_or_default()
            .iter()
            .map(|r| r.port as u16)
            .collect();

        let own_port = crate::mcp::types::get_mcp_api_port();

        (9877..=9899)
            .find(|p| {
                *p != own_port
                    && !child_ports.contains(p)
                    && !reg_ports.contains(p)
                    && !db_ports.contains(p)
                    && !crate::process_capture::health::is_port_in_use(*p)
            })
            .ok_or_else(|| {
                "No available ports in range 9877-9899. Stop some instances first.".to_string()
            })
    }

    /// Get the IDs of all currently running instances.
    pub async fn get_running_ids(&self) -> Vec<String> {
        let mut instances = self.instances.lock().await;
        let mut running = Vec::new();
        let mut dead = Vec::new();
        for (id, handle) in instances.iter_mut() {
            if is_process_alive(&mut handle.child) {
                running.push(id.clone());
            } else {
                dead.push(id.clone());
            }
        }
        // Clean up dead entries
        for id in dead {
            instances.remove(&id);
        }
        running
    }

    /// Launch a new runner instance with the given configuration.
    pub async fn launch_instance(&self, config: &RunnerInstanceConfig) -> Result<u32, String> {
        self.launch_instance_with_app(config, None).await
    }

    /// Launch a new runner instance, optionally resolving spawn
    /// placement against the supplied AppHandle. Callers from a Tauri
    /// context (commands, HTTP handlers) should pass `Some(app)` so
    /// `config.spawn_placement` can be honored. Callers without a
    /// Tauri handle (e.g. session-restore on startup before the
    /// AppHandle is shared with us) can pass `None` and the OS picks
    /// the position.
    pub async fn launch_instance_with_app(
        &self,
        config: &RunnerInstanceConfig,
        app: Option<&tauri::AppHandle>,
    ) -> Result<u32, String> {
        let mut instances = self.instances.lock().await;

        // Check if already running
        if let Some(handle) = instances.get_mut(&config.id) {
            if is_process_alive(&mut handle.child) {
                return Err(format!("Instance '{}' is already running", config.name));
            }
            // Dead process — remove stale handle
            instances.remove(&config.id);
        }

        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable path: {}", e))?;

        info!(
            "Launching runner instance '{}' on port {} (exe: {:?})",
            config.name, config.port, exe_path
        );

        let mut cmd = crate::process_helpers::no_window(&exe_path);

        // Set environment variables
        cmd.env("QONTINUI_PORT", config.port.to_string());
        cmd.env("QONTINUI_INSTANCE_NAME", &config.name);

        // Propagate the primary runner's port so secondaries can proxy
        // process capture requests back and send heartbeats.
        let own_port = crate::mcp::types::get_mcp_api_port();
        cmd.env("QONTINUI_PRIMARY_PORT", own_port.to_string());

        // Apply spawn-window placement if configured. This is best-effort:
        // a resolution failure (e.g. monitor disconnected since config) is
        // logged at WARN and the spawn proceeds without the env vars.
        if let Some(placement) = &config.spawn_placement {
            match app {
                Some(app) => {
                    match crate::spawn_placement::resolve_to_global_physical(app, placement) {
                        Ok(resolved) => {
                            info!(
                                "Applying spawn placement to instance '{}': global=({}, {}) size=({}x{}) [{}]",
                                config.name,
                                resolved.global_x,
                                resolved.global_y,
                                resolved.width,
                                resolved.height,
                                resolved.monitor_label
                            );
                            cmd.env("QONTINUI_WINDOW_X", resolved.global_x.to_string());
                            cmd.env("QONTINUI_WINDOW_Y", resolved.global_y.to_string());
                            cmd.env("QONTINUI_WINDOW_WIDTH", resolved.width.to_string());
                            cmd.env("QONTINUI_WINDOW_HEIGHT", resolved.height.to_string());
                            // Decorations is plumbed independently of the
                            // resolve step — it doesn't depend on monitor
                            // geometry and falls back to true (Tauri default)
                            // when the field is None.
                            if let Some(deco) = placement.decorations {
                                cmd.env(
                                    "QONTINUI_WINDOW_DECORATIONS",
                                    if deco { "1" } else { "0" },
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Spawn placement for '{}' could not be resolved ({}); launching without explicit position",
                                config.name, e
                            );
                        }
                    }
                }
                None => {
                    warn!(
                        "Instance '{}' has spawn_placement configured but launch was invoked without an AppHandle; skipping placement",
                        config.name
                    );
                }
            }
        }

        // Critical: remove CLAUDECODE env var so Claude CLI can start inside the instance
        cmd.env_remove("CLAUDECODE");

        // Create the process in a new process group (Windows)
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            // Combine with existing creation flags (no_window sets CREATE_NO_WINDOW)
            cmd.creation_flags(0x0800_0000 | CREATE_NEW_PROCESS_GROUP);
        }

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn instance '{}': {}", config.name, e))?;

        let pid = child.id();
        info!(
            "Instance '{}' launched with PID {} on port {}",
            config.name, pid, config.port
        );

        // Do NOT assign instances to the Job Object.  The supervisor
        // explicitly stops unprotected secondaries before killing the primary,
        // and protected instances must survive primary restarts/rebuilds.

        instances.insert(
            config.id.clone(),
            InstanceHandle {
                config: config.clone(),
                child,
            },
        );

        // Persist the running set so a rebuild can restore it
        let running: Vec<String> = instances.keys().cloned().collect();
        drop(instances); // release lock before file I/O
        save_active_instances(&running);

        // Write-through to DB
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "localhost".to_string());
        if let Err(e) = self
            .pg_db
            .upsert_runner_instance(
                &config.id,
                &config.name,
                config.port,
                &hostname,
                false,
                Some(pid),
                "starting",
            )
            .await
        {
            warn!("Failed to persist launched instance to DB: {}", e);
        }

        Ok(pid)
    }

    /// Stop a running instance by ID.
    pub async fn stop_instance(&self, id: &str) -> Result<(), String> {
        let mut instances = self.instances.lock().await;
        if let Some(mut handle) = instances.remove(id) {
            info!(
                "Stopping instance '{}' (PID: {})",
                handle.config.name,
                handle.child.id()
            );
            handle
                .child
                .kill()
                .map_err(|e| format!("Failed to kill instance '{}': {}", handle.config.name, e))?;
            let _ = handle.child.wait(); // Reap the process
            info!("Instance '{}' stopped", handle.config.name);

            // Update the persisted running set
            let running: Vec<String> = instances.keys().cloned().collect();
            drop(instances);
            save_active_instances(&running);

            // Remove from DB
            let _ = self.pg_db.remove_runner_instance(id).await;

            Ok(())
        } else {
            Err(format!("Instance '{}' is not running", id))
        }
    }

    /// Get status of a specific instance.
    ///
    /// `running` is true if EITHER (a) we own a live child process for this
    /// id OR (b) the port responds to `/status`. The drop-down consumers
    /// (Orchestration Loop target picker, Settings → Runner Instances) need
    /// to see externally-spawned runners — supervisor children, manually
    /// launched test runners — as live, even though no child handle is held
    /// here. `api_ready` is the probe result on its own, which lets callers
    /// distinguish "process alive but API still warming up" from "API
    /// answering."
    pub async fn get_instance_status(&self, config: &RunnerInstanceConfig) -> InstanceStatus {
        let mut instances = self.instances.lock().await;
        let (child_alive, pid) = if let Some(handle) = instances.get_mut(&config.id) {
            if is_process_alive(&mut handle.child) {
                (true, Some(handle.child.id()))
            } else {
                // Dead — clean up
                instances.remove(&config.id);
                (false, None)
            }
        } else {
            (false, None)
        };
        drop(instances);

        let api_ready = probe_instance_api(config.port).await;

        InstanceStatus {
            id: config.id.clone(),
            name: config.name.clone(),
            port: config.port,
            running: child_alive || api_ready,
            pid,
            api_ready,
            source: "configured",
            spawn_placement: config.spawn_placement.clone(),
        }
    }

    /// Get statuses of all configured instances.
    pub async fn get_all_statuses(&self, configs: &[RunnerInstanceConfig]) -> Vec<InstanceStatus> {
        let mut result = Vec::with_capacity(configs.len());
        for config in configs {
            result.push(self.get_instance_status(config).await);
        }
        result
    }

    /// Get the unified runner instance list: every configured slot from
    /// `settings.json`, merged with every non-primary entry in the DB
    /// `runner_instances` registry that isn't already represented (deduped
    /// by port). Each entry has its `running`/`api_ready` resolved from a
    /// fresh `/status` probe so the drop-down reflects what's actually
    /// alive — not just what's saved as a slot.
    ///
    /// This is the source the Orchestration Loop target drop-down should
    /// consume. The previous `get_all_statuses` path saw only configured
    /// slots, so a supervisor-spawned runner that registered itself in the
    /// DB would never appear in the picker even though it was answering on
    /// its port.
    pub async fn get_unified_instances(
        &self,
        configs: &[RunnerInstanceConfig],
    ) -> Vec<InstanceStatus> {
        let mut by_port: HashMap<u16, InstanceStatus> = HashMap::new();
        for cfg in configs {
            let status = self.get_instance_status(cfg).await;
            by_port.insert(cfg.port, status);
        }

        let own_port = crate::mcp::types::get_mcp_api_port();
        let db_rows = self
            .pg_db
            .get_all_runner_instances()
            .await
            .unwrap_or_default();

        // Probe each not-yet-seen DB row's port in parallel — sequential
        // probes block the whole drop-down load on the slowest endpoint
        // (1s timeout each), and there can easily be 5+ stale rows.
        let to_probe: Vec<_> = db_rows
            .into_iter()
            .filter(|row| {
                let port = row.port as u16;
                !row.is_primary && port != own_port && !by_port.contains_key(&port)
            })
            .collect();

        let probes = to_probe
            .iter()
            .map(|row| probe_instance_api(row.port as u16));
        let results = futures::future::join_all(probes).await;

        for (row, api_ready) in to_probe.into_iter().zip(results.into_iter()) {
            let port = row.port as u16;
            by_port.insert(
                port,
                InstanceStatus {
                    id: row.id,
                    name: row.name,
                    port,
                    running: api_ready,
                    pid: row.pid.map(|p| p as u32),
                    api_ready,
                    source: "discovered",
                    spawn_placement: None,
                },
            );
        }

        let mut result: Vec<_> = by_port.into_values().collect();
        result.sort_by_key(|s| s.port);
        result
    }
}

// ============================================================================
// Active-instance session persistence
// ============================================================================

/// Path to the session file that tracks which instances were running.
fn session_file_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("com.qontinui.runner").join("active_instances.json"))
}

/// Persist the set of running instance IDs.
/// Called automatically by `launch_instance` / `stop_instance`.
fn save_active_instances(ids: &[String]) {
    let Some(path) = session_file_path() else {
        return;
    };
    if ids.is_empty() {
        // No instances running — remove the file so a clean start doesn't restore anything
        let _ = std::fs::remove_file(&path);
        return;
    }
    match serde_json::to_string(ids) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("Failed to save active instances: {}", e);
            }
        }
        Err(e) => tracing::warn!("Failed to serialize active instances: {}", e),
    }
}

/// Delete the session file.  Called on intentional (user-initiated) close so
/// that the next normal startup does **not** restore instances.
pub fn clear_active_instances() {
    if let Some(path) = session_file_path() {
        let _ = std::fs::remove_file(&path);
    }
}

/// Load the list of instance IDs that were active before the last shutdown.
/// Clears the file after reading so instances aren't re-launched on every restart.
pub fn load_and_clear_active_instances() -> Vec<String> {
    let Some(path) = session_file_path() else {
        return Vec::new();
    };
    if !path.exists() {
        return Vec::new();
    }

    let ids: Vec<String> = match std::fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => return Vec::new(),
    };

    // Clear the file so we don't re-launch on every start
    let _ = std::fs::remove_file(&path);

    ids
}

/// Wait (synchronously) for a port to become free, with a timeout.
/// Returns `true` if the port is free, `false` if still occupied after timeout.
pub fn wait_for_port_free(port: u16, timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    let check_interval = std::time::Duration::from_millis(250);
    loop {
        if !crate::process_capture::health::is_port_in_use(port) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(check_interval);
    }
}

/// Check if a child process is still alive (non-blocking).
fn is_process_alive(child: &mut std::process::Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => false, // Exited
        Ok(None) => true,     // Still running
        Err(_) => false,      // Error checking — assume dead
    }
}

/// Probe an instance's HTTP API to check if it's ready to accept requests.
/// Returns true if the `/status` endpoint responds within 1 second.
async fn probe_instance_api(port: u16) -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build();

    let client = match client {
        Ok(c) => c,
        Err(_) => return false,
    };

    let url = format!("http://localhost:{}/status", port);
    client.get(&url).send().await.is_ok()
}
