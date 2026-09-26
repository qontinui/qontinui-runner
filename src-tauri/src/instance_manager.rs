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
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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
    ///
    /// `resource_override` is forwarded to the spawn-time resource gate — see
    /// [`Self::launch_instance_with_app`].
    pub async fn launch_instance(
        &self,
        config: &RunnerInstanceConfig,
        resource_override: bool,
    ) -> Result<u32, String> {
        self.launch_instance_with_app(config, None, resource_override)
            .await
    }

    /// Launch a new runner instance, optionally resolving spawn
    /// placement against the supplied AppHandle. Callers from a Tauri
    /// context (commands, HTTP handlers) should pass `Some(app)` so
    /// `config.spawn_placement` can be honored. Callers without a
    /// Tauri handle (e.g. session-restore on startup before the
    /// AppHandle is shared with us) can pass `None` and the OS picks
    /// the position.
    ///
    /// `resource_override` carries an explicit "start it anyway" past the
    /// spawn-time resource gate (plan
    /// `2026-08-07-runner-resource-guard-and-session-protection` §Part D). A
    /// secondary runner is a whole second copy of this application — its own
    /// webview, its own embedded services, its own PTYs — so it is exactly the
    /// kind of new process the incident showed at risk, and it gets the same
    /// gate the PTY seam does. See [`crate::resource_guard`].
    pub async fn launch_instance_with_app(
        &self,
        config: &RunnerInstanceConfig,
        app: Option<&tauri::AppHandle>,
        resource_override: bool,
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

        // Spawn-time resource gate (§Part D). Placed AFTER the already-running
        // check so a redundant launch of a live instance cannot produce a
        // spurious low-memory notice, and BEFORE anything is created so a
        // refusal leaves no half-launched process and touches nothing that is
        // already running. Off the async worker: the probe refreshes sysinfo and
        // enumerates volumes, which is milliseconds but is still blocking work.
        {
            let app_for_gate = app.cloned();
            spawn_blocking_tracked(move || {
                crate::resource_guard::admit_spawn(
                    "runner instance",
                    resource_override,
                    app_for_gate.as_ref(),
                )
            })
            .await
            .map_err(|e| format!("resource guard task failed: {e}"))??;
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
        // Same rule, sibling marker — see `session::transport::claude_cli` docs.
        cmd.env_remove(qontinui_runner_lib::claude_env::CLAUDE_CHILD_SESSION_ENV);

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

    /// The live child this runner spawned on `port`, if any.
    ///
    /// Only a handle whose process is still alive counts — a slot whose child
    /// exited (crashed, killed by hand) is NOT owned any more, even though its
    /// stale handle may linger until the next launch/status call reaps it.
    /// This is the lookup the Orchestration Loop resolves a restart target by:
    /// by PORT, so it is immune to the supervisor-id vs settings-slot-id split
    /// (plan `2026-09-22-orchestration-loop-restart-modes-depend-on-the-dev-only-supervisor`).
    pub async fn owned_instance_on_port(&self, port: u16) -> Option<RunnerInstanceConfig> {
        let mut instances = self.instances.lock().await;
        for handle in instances.values_mut() {
            if handle.config.port == port && is_process_alive(&mut handle.child) {
                return Some(handle.config.clone());
            }
        }
        None
    }

    /// The live child this runner spawned for slot `id`, if any. Same
    /// liveness rule as [`Self::owned_instance_on_port`].
    pub async fn owned_instance(&self, id: &str) -> Option<RunnerInstanceConfig> {
        let mut instances = self.instances.lock().await;
        let handle = instances.get_mut(id)?;
        is_process_alive(&mut handle.child).then(|| handle.config.clone())
    }

    /// Restart a runner-owned child in place: stop it, wait for its port to
    /// free, relaunch the SAME slot config, and verify it came back on the
    /// same port. See [`restart_with`] for the ordering and error contract.
    ///
    /// Pass the `AppHandle` whenever one is available: the slot's
    /// `spawn_placement` is only honoured with it, and dropping it would move
    /// the user's configured window on every restart.
    pub async fn restart_instance(
        &self,
        id: &str,
        app: Option<&tauri::AppHandle>,
    ) -> Result<u32, InstanceRestartError> {
        restart_with(self, id, app).await
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

        for (row, api_ready) in to_probe.into_iter().zip(results) {
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
// In-place restart of a runner-owned child
// ============================================================================

/// How long [`restart_with`] waits for a stopped child's port to free before
/// giving up with [`InstanceRestartError::PortNeverFreed`].
pub const RESTART_PORT_FREE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The lifecycle operations an in-place restart is composed of.
///
/// [`InstanceManager`] is the only production implementation; the trait
/// exists so the restart ORDERING (stop → port free → launch → same port) and
/// every error arm can be tested without spawning a real runner process.
#[async_trait::async_trait]
pub trait InstanceLifecycle: Send + Sync {
    /// See [`InstanceManager::owned_instance_on_port`].
    async fn owned_instance_on_port(&self, port: u16) -> Option<RunnerInstanceConfig>;
    /// See [`InstanceManager::owned_instance`].
    async fn owned_instance(&self, id: &str) -> Option<RunnerInstanceConfig>;
    /// Kill and reap the child for `id`.
    async fn stop(&self, id: &str) -> Result<(), String>;
    /// Block (off the async workers) until `port` is free or `timeout` passes.
    async fn wait_port_free(&self, port: u16, timeout: std::time::Duration) -> bool;
    /// Launch `config` through the normal, resource-gated launch path.
    async fn launch(
        &self,
        config: &RunnerInstanceConfig,
        app: Option<&tauri::AppHandle>,
    ) -> Result<u32, String>;
}

#[async_trait::async_trait]
impl InstanceLifecycle for InstanceManager {
    async fn owned_instance_on_port(&self, port: u16) -> Option<RunnerInstanceConfig> {
        InstanceManager::owned_instance_on_port(self, port).await
    }

    async fn owned_instance(&self, id: &str) -> Option<RunnerInstanceConfig> {
        InstanceManager::owned_instance(self, id).await
    }

    async fn stop(&self, id: &str) -> Result<(), String> {
        self.stop_instance(id).await
    }

    async fn wait_port_free(&self, port: u16, timeout: std::time::Duration) -> bool {
        // `wait_for_port_free` sleeps in a loop — never on an async worker.
        spawn_blocking_tracked(move || wait_for_port_free(port, timeout))
            .await
            .unwrap_or(false)
    }

    async fn launch(
        &self,
        config: &RunnerInstanceConfig,
        app: Option<&tauri::AppHandle>,
    ) -> Result<u32, String> {
        // No resource override: a restart is gated exactly like any launch,
        // and a refusal surfaces as a typed error rather than a forced spawn.
        self.launch_instance_with_app(config, app, false).await
    }
}

/// Why an in-place restart of a runner-owned child did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceRestartError {
    /// No live child for this slot — nothing of ours to restart.
    NotRunning { id: String },
    /// Killing the child failed; nothing was relaunched.
    StopFailed { id: String, cause: String },
    /// The child was stopped but its port was still bound after the timeout,
    /// so a relaunch would have collided. The child is NOT running now.
    PortNeverFreed { id: String, port: u16, waited_secs: u64 },
    /// The relaunch was refused (resource gate) or failed to spawn. The child
    /// is NOT running now.
    LaunchFailed { id: String, cause: String },
    /// The relaunch spawned a process that was already gone when checked.
    ChildVanishedAfterLaunch { id: String },
    /// The relaunched child is on a different port than before — callers
    /// that resolved the target by port would now be pointing at nothing.
    PortChanged { id: String, before: u16, after: u16 },
}

impl std::fmt::Display for InstanceRestartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning { id } => {
                write!(f, "instance '{id}' is not a live child of this runner")
            }
            Self::StopFailed { id, cause } => {
                write!(f, "stopping instance '{id}' failed: {cause}")
            }
            Self::PortNeverFreed {
                id,
                port,
                waited_secs,
            } => write!(
                f,
                "instance '{id}' was stopped but port {port} was still in use after \
                 {waited_secs}s; not relaunched"
            ),
            Self::LaunchFailed { id, cause } => write!(
                f,
                "instance '{id}' was stopped but relaunching it failed: {cause}"
            ),
            Self::ChildVanishedAfterLaunch { id } => write!(
                f,
                "instance '{id}' was relaunched but its process was already gone"
            ),
            Self::PortChanged { id, before, after } => write!(
                f,
                "instance '{id}' came back on port {after}, not {before}"
            ),
        }
    }
}

impl std::error::Error for InstanceRestartError {}

/// Restart the runner-owned child `id` through `lifecycle`.
///
/// Ordering, each step gating the next:
/// 1. capture the slot config (only a LIVE child is restartable);
/// 2. stop it;
/// 3. wait up to [`RESTART_PORT_FREE_TIMEOUT`] for its port to free;
/// 4. relaunch the captured config (resource-gated like any launch);
/// 5. assert the relaunched child is live on the SAME port.
///
/// Every failure is a typed [`InstanceRestartError`]; nothing waits unbounded.
/// Returns the new child's PID.
pub async fn restart_with(
    lifecycle: &(impl InstanceLifecycle + ?Sized),
    id: &str,
    app: Option<&tauri::AppHandle>,
) -> Result<u32, InstanceRestartError> {
    let config = lifecycle
        .owned_instance(id)
        .await
        .ok_or_else(|| InstanceRestartError::NotRunning { id: id.to_string() })?;
    let port = config.port;

    lifecycle
        .stop(id)
        .await
        .map_err(|cause| InstanceRestartError::StopFailed {
            id: id.to_string(),
            cause,
        })?;

    if !lifecycle
        .wait_port_free(port, RESTART_PORT_FREE_TIMEOUT)
        .await
    {
        return Err(InstanceRestartError::PortNeverFreed {
            id: id.to_string(),
            port,
            waited_secs: RESTART_PORT_FREE_TIMEOUT.as_secs(),
        });
    }

    let pid = lifecycle
        .launch(&config, app)
        .await
        .map_err(|cause| InstanceRestartError::LaunchFailed {
            id: id.to_string(),
            cause,
        })?;

    match lifecycle.owned_instance(id).await {
        None => Err(InstanceRestartError::ChildVanishedAfterLaunch { id: id.to_string() }),
        Some(after) if after.port != port => Err(InstanceRestartError::PortChanged {
            id: id.to_string(),
            before: port,
            after: after.port,
        }),
        Some(_) => {
            info!("Instance '{}' restarted in place (PID {}, port {})", id, pid, port);
            Ok(pid)
        }
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

#[cfg(test)]
mod restart_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn slot(id: &str, port: u16) -> RunnerInstanceConfig {
        RunnerInstanceConfig {
            id: id.to_string(),
            name: format!("{id}-name"),
            port,
            spawn_placement: None,
        }
    }

    /// A scripted [`InstanceLifecycle`] that records every call in order.
    struct FakeLifecycle {
        calls: StdMutex<Vec<String>>,
        /// What `owned_instance` answers, popped front-first per call; an
        /// exhausted script answers `None`.
        owned_script: StdMutex<Vec<Option<RunnerInstanceConfig>>>,
        stop_result: Result<(), String>,
        port_frees: bool,
        launch_result: Result<u32, String>,
    }

    impl FakeLifecycle {
        fn new(owned_script: Vec<Option<RunnerInstanceConfig>>) -> Self {
            Self {
                calls: StdMutex::new(Vec::new()),
                owned_script: StdMutex::new(owned_script),
                stop_result: Ok(()),
                port_frees: true,
                launch_result: Ok(4242),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl InstanceLifecycle for FakeLifecycle {
        async fn owned_instance_on_port(&self, port: u16) -> Option<RunnerInstanceConfig> {
            self.calls.lock().unwrap().push(format!("on_port:{port}"));
            None
        }
        async fn owned_instance(&self, id: &str) -> Option<RunnerInstanceConfig> {
            self.calls.lock().unwrap().push(format!("owned:{id}"));
            let mut script = self.owned_script.lock().unwrap();
            if script.is_empty() {
                None
            } else {
                script.remove(0)
            }
        }
        async fn stop(&self, id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("stop:{id}"));
            self.stop_result.clone()
        }
        async fn wait_port_free(&self, port: u16, timeout: std::time::Duration) -> bool {
            self.calls
                .lock()
                .unwrap()
                .push(format!("wait_free:{port}:{}", timeout.as_secs()));
            self.port_frees
        }
        async fn launch(
            &self,
            config: &RunnerInstanceConfig,
            app: Option<&tauri::AppHandle>,
        ) -> Result<u32, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("launch:{}:{}:app={}", config.id, config.port, app.is_some()));
            self.launch_result.clone()
        }
    }

    #[tokio::test]
    async fn restart_runs_stop_then_port_free_then_launch_then_verifies_the_port() {
        let fake = FakeLifecycle::new(vec![Some(slot("a", 9877)), Some(slot("a", 9877))]);
        let pid = restart_with(&fake, "a", None).await.expect("restart");
        assert_eq!(pid, 4242);
        assert_eq!(
            fake.calls(),
            vec![
                "owned:a".to_string(),
                "stop:a".to_string(),
                "wait_free:9877:30".to_string(),
                "launch:a:9877:app=false".to_string(),
                "owned:a".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn restart_of_a_slot_with_no_live_child_touches_nothing() {
        let fake = FakeLifecycle::new(vec![None]);
        let err = restart_with(&fake, "a", None).await.unwrap_err();
        assert_eq!(err, InstanceRestartError::NotRunning { id: "a".into() });
        assert_eq!(fake.calls(), vec!["owned:a".to_string()]);
    }

    #[tokio::test]
    async fn a_failed_stop_does_not_wait_or_relaunch() {
        let mut fake = FakeLifecycle::new(vec![Some(slot("a", 9877))]);
        fake.stop_result = Err("access denied".into());
        let err = restart_with(&fake, "a", None).await.unwrap_err();
        assert!(matches!(err, InstanceRestartError::StopFailed { .. }), "{err}");
        assert_eq!(fake.calls(), vec!["owned:a".to_string(), "stop:a".to_string()]);
    }

    #[tokio::test]
    async fn a_port_that_never_frees_is_typed_and_never_relaunched() {
        let mut fake = FakeLifecycle::new(vec![Some(slot("a", 9877))]);
        fake.port_frees = false;
        let err = restart_with(&fake, "a", None).await.unwrap_err();
        assert_eq!(
            err,
            InstanceRestartError::PortNeverFreed {
                id: "a".into(),
                port: 9877,
                waited_secs: 30
            }
        );
        assert!(
            !fake.calls().iter().any(|c| c.starts_with("launch:")),
            "must not relaunch onto a busy port: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn a_launch_refused_by_the_resource_gate_is_typed() {
        let mut fake = FakeLifecycle::new(vec![Some(slot("a", 9877))]);
        fake.launch_result = Err("resource guard refused: low memory".into());
        let err = restart_with(&fake, "a", None).await.unwrap_err();
        match err {
            InstanceRestartError::LaunchFailed { id, cause } => {
                assert_eq!(id, "a");
                assert!(cause.contains("resource guard"));
            }
            other => panic!("expected LaunchFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_child_that_dies_right_after_launch_is_typed() {
        let fake = FakeLifecycle::new(vec![Some(slot("a", 9877)), None]);
        let err = restart_with(&fake, "a", None).await.unwrap_err();
        assert_eq!(
            err,
            InstanceRestartError::ChildVanishedAfterLaunch { id: "a".into() }
        );
    }

    #[tokio::test]
    async fn a_relaunch_on_a_different_port_is_typed() {
        let fake = FakeLifecycle::new(vec![Some(slot("a", 9877)), Some(slot("a", 9880))]);
        let err = restart_with(&fake, "a", None).await.unwrap_err();
        assert_eq!(
            err,
            InstanceRestartError::PortChanged {
                id: "a".into(),
                before: 9877,
                after: 9880
            }
        );
    }

    /// The real lookup honours liveness: a handle whose process exited is not
    /// "owned", even while the stale handle is still in the table.
    #[cfg(unix)]
    #[tokio::test]
    async fn owned_instance_on_port_sees_only_a_live_child() {
        let mgr = InstanceManager::new(crate::database::pg::PgDb::new_noop_for_test());
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        mgr.instances.lock().await.insert(
            "slot-1".to_string(),
            InstanceHandle {
                config: slot("slot-1", 9891),
                child,
            },
        );

        assert_eq!(
            mgr.owned_instance_on_port(9891).await.map(|c| c.id),
            Some("slot-1".to_string())
        );
        assert!(mgr.owned_instance_on_port(9892).await.is_none());
        assert_eq!(
            mgr.owned_instance("slot-1").await.map(|c| c.port),
            Some(9891)
        );

        {
            let mut instances = mgr.instances.lock().await;
            let handle = instances.get_mut("slot-1").expect("handle");
            handle.child.kill().expect("kill sleep");
            handle.child.wait().expect("reap sleep");
        }
        assert!(mgr.owned_instance_on_port(9891).await.is_none());
        assert!(mgr.owned_instance("slot-1").await.is_none());
    }
}
