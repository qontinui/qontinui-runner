//! Instance Manager for spawning and tracking secondary runner processes.
//!
//! Each instance runs on its own port (via `QONTINUI_PORT` env var) and gets
//! its own Tauri window. The shared SQLite database (WAL mode) handles
//! concurrent access from multiple instances.
//!
//! Active instance IDs are persisted to `active_instances.json` so that
//! instances can be restored after a rebuild / restart.  The file is written
//! on every launch/stop and **deleted** on intentional close so that a normal
//! shutdown does not trigger restoration on the next start.

use serde::Serialize;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tracing::info;

use crate::settings::RunnerInstanceConfig;

/// Status of a runner instance.
#[derive(Debug, Clone, Serialize)]
pub struct InstanceStatus {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub running: bool,
    pub pid: Option<u32>,
    pub api_ready: bool,
}

/// Handle for a spawned runner instance.
struct InstanceHandle {
    config: RunnerInstanceConfig,
    child: std::process::Child,
}

/// Manages spawned runner instance processes.
pub struct InstanceManager {
    instances: Mutex<HashMap<String, InstanceHandle>>,
}

impl InstanceManager {
    pub fn new() -> Self {
        Self {
            instances: Mutex::new(HashMap::new()),
        }
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

        // Assign to Job Object so it's cleaned up if parent crashes
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::OpenProcess;
            const PROCESS_ALL_ACCESS: u32 = 0x001F_0FFF;
            unsafe {
                let process_handle = OpenProcess(PROCESS_ALL_ACCESS, 0, pid);
                if !process_handle.is_null() {
                    crate::job_object::assign_process_to_job(process_handle);
                    CloseHandle(process_handle);
                }
            }
        }

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

            Ok(())
        } else {
            Err(format!("Instance '{}' is not running", id))
        }
    }

    /// Get status of a specific instance.
    pub async fn get_instance_status(&self, config: &RunnerInstanceConfig) -> InstanceStatus {
        let mut instances = self.instances.lock().await;
        let (running, pid) = if let Some(handle) = instances.get_mut(&config.id) {
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

        // Probe the instance's HTTP API to check if it's actually ready
        let api_ready = if running {
            probe_instance_api(config.port).await
        } else {
            false
        };

        InstanceStatus {
            id: config.id.clone(),
            name: config.name.clone(),
            port: config.port,
            running,
            pid,
            api_ready,
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
