//! Instance Manager for spawning and tracking secondary runner processes.
//!
//! Each instance runs on its own port (via `QONTINUI_PORT` env var) and gets
//! its own Tauri window. The shared SQLite database (WAL mode) handles
//! concurrent access from multiple instances.

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

        InstanceStatus {
            id: config.id.clone(),
            name: config.name.clone(),
            port: config.port,
            running,
            pid,
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

/// Check if a child process is still alive (non-blocking).
fn is_process_alive(child: &mut std::process::Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => false, // Exited
        Ok(None) => true,     // Still running
        Err(_) => false,      // Error checking — assume dead
    }
}
