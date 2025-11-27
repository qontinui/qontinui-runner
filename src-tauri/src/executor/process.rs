use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use tauri::Manager;
use tracing::{debug, info};

/// Handles Python process spawning and management
pub struct ProcessManager {
    app_handle: tauri::AppHandle,
}

impl ProcessManager {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self { app_handle }
    }

    /// Spawns a Python process with the given executor type
    pub fn spawn_process(&self, executor_type: &str) -> Result<Child, String> {
        info!("Starting executor with type: {}", executor_type);

        // Try bundled executor first, fall back to Python
        let mut cmd = if let Some(bundled_path) = self.find_bundled_executor() {
            info!("Using bundled executor mode");
            self.build_bundled_command(&bundled_path)
        } else {
            info!("Using Python interpreter mode");

            // Determine script name
            let script_name = match executor_type {
                "minimal" => "minimal_bridge.py",
                "real" => "qontinui_executor.py",
                _ => "qontinui_bridge.py",
            };

            // Find bridge script
            let bridge_script = self.find_bridge_script(script_name)?;
            info!("Using Python bridge script: {:?}", bridge_script);

            // Build Python command
            self.build_python_command(&bridge_script, executor_type)?
        };

        // Add mock flag if not real mode
        if executor_type != "real" {
            cmd.arg("--mock");
        }

        // CRITICAL: Disable console logging to prevent JSON parse errors
        // The executor expects all stderr/stdout to be valid JSON messages
        // Use command-line flag instead of environment variable for reliability
        cmd.arg("--disable-console-logging");

        // CRITICAL: Set environment variables AFTER building command
        // These must be set on the final Command object that will be spawned

        // Set PYTHONUNBUFFERED to force immediate output (more reliable than -u flag)
        cmd.env("PYTHONUNBUFFERED", "1");

        // Also set environment variable as backup (some code paths might check it)
        cmd.env("QONTINUI_DISABLE_CONSOLE_LOGGING", "1");

        // Spawn Python process
        let child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start Python process: {}", e))?;

        info!("Python process spawned successfully");

        Ok(child)
    }

    /// Finds the Python bridge script
    fn find_bridge_script(&self, script_name: &str) -> Result<PathBuf, String> {
        let possible_paths = vec![
            // When running from src-tauri/target/debug or release
            std::env::current_dir().ok().and_then(|p| {
                if p.ends_with("debug") || p.ends_with("release") {
                    p.parent()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.parent())
                        .map(|p| p.join("python-bridge").join(script_name))
                } else if p.ends_with("src-tauri") {
                    p.parent()
                        .map(|p| p.join("python-bridge").join(script_name))
                } else {
                    None
                }
            }),
            // When running from qontinui-runner directory
            std::env::current_dir()
                .ok()
                .map(|p| p.join("python-bridge").join(script_name)),
            // When in src-tauri directory
            std::env::current_dir()
                .ok()
                .map(|p| p.join("..").join("python-bridge").join(script_name)),
        ];

        debug!("Current directory: {:?}", std::env::current_dir());

        possible_paths
            .into_iter()
            .flatten()
            .inspect(|p| {
                debug!("Checking path: {:?}, exists: {}", p, p.exists());
            })
            .find(|p| p.exists())
            .ok_or_else(|| {
                format!(
                    "Python bridge script {} not found in any expected location",
                    script_name
                )
            })
    }

    /// Builds the Python command
    fn build_python_command(
        &self,
        bridge_script: &PathBuf,
        executor_type: &str,
    ) -> Result<Command, String> {
        let use_poetry = executor_type == "qontinui_executor" || executor_type == "qontinui";

        // Check for Poetry and qontinui library
        let poetry_available = if use_poetry {
            let qontinui_path = bridge_script
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|p| p.join("qontinui").join("pyproject.toml"));

            if let Some(ref path) = qontinui_path {
                debug!(
                    "Checking for qontinui at: {:?}, exists: {}",
                    path,
                    path.exists()
                );
                path.exists()
            } else {
                false
            }
        } else {
            false
        };

        let venv_python = bridge_script.parent().and_then(|p| {
            // Try Windows-style venv first (Scripts/python.exe)
            let venv_path_windows = p.join(".venv/Scripts/python.exe");
            debug!(
                "Checking Windows venv path: {:?}, exists: {}",
                venv_path_windows,
                venv_path_windows.exists()
            );
            if venv_path_windows.exists() {
                return Some(venv_path_windows);
            }

            // Try Unix-style venv (bin/python)
            let venv_path_unix = p.join(".venv/bin/python");
            debug!(
                "Checking Unix venv path: {:?}, exists: {}",
                venv_path_unix,
                venv_path_unix.exists()
            );
            if venv_path_unix.exists() {
                return Some(venv_path_unix);
            }

            None
        });

        let cmd = if poetry_available && use_poetry {
            info!("Using Poetry to run Python with qontinui library");
            let qontinui_dir = bridge_script
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|p| p.join("qontinui"))
                .expect("Could not determine qontinui directory");

            let mut poetry_cmd = Command::new("poetry");
            poetry_cmd.current_dir(&qontinui_dir);
            poetry_cmd.arg("run");
            poetry_cmd.arg("python");
            poetry_cmd.arg("-u"); // Unbuffered output
            poetry_cmd.arg(bridge_script);
            poetry_cmd
        } else if let Some(venv_path) = venv_python {
            info!("Using venv Python: {:?}", venv_path);
            let mut python_cmd = Command::new(venv_path);
            python_cmd.arg("-u"); // Unbuffered output
            python_cmd.arg(bridge_script);
            python_cmd
        } else if cfg!(target_os = "windows") {
            info!("Using system python");
            let mut python_cmd = Command::new("python");
            python_cmd.arg("-u"); // Unbuffered output
            python_cmd.arg(bridge_script);
            python_cmd
        } else {
            info!("Using system python3");
            let mut python_cmd = Command::new("python3");
            python_cmd.arg("-u"); // Unbuffered output
            python_cmd.arg(bridge_script);
            python_cmd
        };

        Ok(cmd)
    }

    /// Finds the bundled executor sidecar executable
    /// Returns None if running in development mode (executable not found)
    fn find_bundled_executor(&self) -> Option<PathBuf> {
        // Get the platform-specific executable name
        let exe_name = if cfg!(target_os = "windows") {
            "qontinui-executor.exe"
        } else {
            "qontinui-executor"
        };

        // Get the target triple for this platform
        let target_triple = if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            "x86_64-pc-windows-msvc"
        } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
            "aarch64-pc-windows-msvc"
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            "x86_64-apple-darwin"
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            "aarch64-apple-darwin"
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            "x86_64-unknown-linux-gnu"
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            "aarch64-unknown-linux-gnu"
        } else {
            return None;
        };

        // Platform-specific sidecar name (Tauri convention)
        let sidecar_name = if cfg!(target_os = "windows") {
            format!("qontinui-executor-{}.exe", target_triple)
        } else {
            format!("qontinui-executor-{}", target_triple)
        };

        // Try to resolve using Tauri's resource directory
        if let Ok(resource_dir) = self.app_handle.path().resource_dir() {
            let sidecar_path = resource_dir.join(&sidecar_name);
            debug!("Checking bundled sidecar at: {:?}", sidecar_path);
            if sidecar_path.exists() {
                info!("Found bundled executor at: {:?}", sidecar_path);
                return Some(sidecar_path);
            }
        }

        // Try relative paths for development/testing
        let possible_paths = vec![
            // In src-tauri/binaries (development)
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .map(|p| p.join("binaries").join(&sidecar_name)),
            // Relative to current directory
            std::env::current_dir()
                .ok()
                .map(|p| p.join("src-tauri").join("binaries").join(&sidecar_name)),
            // Simple name without target triple (for manual testing)
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .map(|p| p.join("binaries").join(exe_name)),
        ];

        for path in possible_paths.into_iter().flatten() {
            debug!("Checking sidecar path: {:?}, exists: {}", path, path.exists());
            if path.exists() {
                info!("Found bundled executor at: {:?}", path);
                return Some(path);
            }
        }

        debug!("Bundled executor not found, will use Python mode");
        None
    }

    /// Builds a command for the bundled executor
    fn build_bundled_command(&self, executor_path: &PathBuf) -> Command {
        info!("Using bundled executor: {:?}", executor_path);
        let cmd = Command::new(executor_path);

        // The bundled executor doesn't need the script path since it's compiled in
        // But it still accepts the same command-line arguments

        cmd
    }

    /// Determines if we should use the bundled executor or Python
    /// Returns true if bundled executor should be used
    #[allow(dead_code)]
    pub fn should_use_bundled(&self) -> bool {
        // Check environment variable override
        if let Ok(val) = std::env::var("QONTINUI_USE_PYTHON") {
            if val == "1" || val.to_lowercase() == "true" {
                info!("QONTINUI_USE_PYTHON set, forcing Python mode");
                return false;
            }
        }

        // Check if bundled executor exists
        self.find_bundled_executor().is_some()
    }
}
