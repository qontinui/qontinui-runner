use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use tauri::Manager;
use tracing::{debug, error, info, warn};

/// Handles Python process spawning and management.
///
/// Python execution priority:
/// 1. QONTINUI_PYTHON_PATH environment variable (for advanced users)
/// 2. Bundled executor (for production builds)
/// 3. Poetry (for development)
/// 4. Virtual environment (.venv or venv)
/// 5. System Python (fallback with warning)
pub struct ProcessManager {
    app_handle: tauri::AppHandle,
}

impl ProcessManager {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self { app_handle }
    }

    /// Spawns the Python executor process.
    ///
    /// Priority:
    /// 1. QONTINUI_PYTHON_PATH env var - allows advanced users to override
    /// 2. Bundled executor - for production builds
    /// 3. Poetry - for development with full dependency management
    /// 4. Venv - for custom virtual environments
    /// 5. System Python - last resort fallback
    pub fn spawn_process(&self) -> Result<Child, String> {
        info!("Starting Python executor");

        // Priority 1: Check for QONTINUI_PYTHON_PATH environment variable
        if let Ok(custom_python) = std::env::var("QONTINUI_PYTHON_PATH") {
            let custom_path = PathBuf::from(&custom_python);
            if custom_path.exists() {
                info!(
                    "Using custom Python from QONTINUI_PYTHON_PATH: {:?}",
                    custom_path
                );
                let bridge_script = self.find_bridge_script()?;
                let mut cmd = Command::new(&custom_path);
                cmd.arg("-u"); // Unbuffered output
                cmd.arg(&bridge_script);
                return self.spawn_with_command(cmd);
            } else {
                error!(
                    "QONTINUI_PYTHON_PATH set but path does not exist: {:?}",
                    custom_path
                );
                // Fall through to other methods
            }
        }

        // Priority 2: Try bundled executor (for production builds)
        // Priority 3-5: Fall back to Python interpreter
        let mut cmd = if let Some(bundled_path) = self.find_bundled_executor() {
            info!("Using bundled executor mode");
            self.build_bundled_command(&bundled_path)
        } else {
            info!("Using Python interpreter mode");

            // Find the executor script
            let bridge_script = self.find_bridge_script()?;
            info!("Using Python executor: {:?}", bridge_script);

            // Build Python command using poetry/venv/system
            self.build_python_command(&bridge_script)?
        };

        // Disable console logging to prevent JSON parse errors
        // The executor expects all stderr/stdout to be valid JSON messages
        cmd.arg("--disable-console-logging");

        // Set PYTHONUNBUFFERED to force immediate output
        cmd.env("PYTHONUNBUFFERED", "1");

        // Set environment variable as backup
        cmd.env("QONTINUI_DISABLE_CONSOLE_LOGGING", "1");

        // Set the model cache directory for lazy loading
        if let Ok(app_data_dir) = self.app_handle.path().app_data_dir() {
            let models_dir = app_data_dir.join("models");
            cmd.env(
                "QONTINUI_MODELS_DIR",
                models_dir.to_string_lossy().to_string(),
            );
        }

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

    /// Helper to spawn with common configuration applied
    fn spawn_with_command(&self, mut cmd: Command) -> Result<Child, String> {
        // Disable console logging to prevent JSON parse errors
        cmd.arg("--disable-console-logging");

        // Set PYTHONUNBUFFERED to force immediate output
        cmd.env("PYTHONUNBUFFERED", "1");

        // Set environment variable as backup
        cmd.env("QONTINUI_DISABLE_CONSOLE_LOGGING", "1");

        // Set the model cache directory for lazy loading
        if let Ok(app_data_dir) = self.app_handle.path().app_data_dir() {
            let models_dir = app_data_dir.join("models");
            cmd.env(
                "QONTINUI_MODELS_DIR",
                models_dir.to_string_lossy().to_string(),
            );
        }

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

    /// Finds the Python executor script (qontinui_executor.py)
    fn find_bridge_script(&self) -> Result<PathBuf, String> {
        const SCRIPT_NAME: &str = "qontinui_executor.py";

        let possible_paths = vec![
            // Based on executable location (most reliable - works with supervisor)
            // Binary is at: qontinui-runner/src-tauri/target/debug/qontinui-runner.exe
            // Script is at: qontinui-runner/python-bridge/qontinui_executor.py
            std::env::current_exe().ok().and_then(|p| {
                // Go up from binary to target/debug or target/release
                p.parent() // target/debug/
                    .and_then(|p| p.parent()) // target/
                    .and_then(|p| p.parent()) // src-tauri/
                    .and_then(|p| p.parent()) // qontinui-runner/
                    .map(|p| p.join("python-bridge").join(SCRIPT_NAME))
            }),
            // When running from src-tauri/target/debug or release
            std::env::current_dir().ok().and_then(|p| {
                if p.ends_with("debug") || p.ends_with("release") {
                    p.parent()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.parent())
                        .map(|p| p.join("python-bridge").join(SCRIPT_NAME))
                } else if p.ends_with("src-tauri") {
                    p.parent()
                        .map(|p| p.join("python-bridge").join(SCRIPT_NAME))
                } else {
                    None
                }
            }),
            // When running from qontinui-runner directory
            std::env::current_dir()
                .ok()
                .map(|p| p.join("python-bridge").join(SCRIPT_NAME)),
            // When in src-tauri directory
            std::env::current_dir()
                .ok()
                .map(|p| p.join("..").join("python-bridge").join(SCRIPT_NAME)),
        ];

        debug!("Current exe: {:?}", std::env::current_exe());
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
                    "Python executor script {} not found in any expected location",
                    SCRIPT_NAME
                )
            })
    }

    /// Builds the Python command using poetry.
    /// Falls back to venv or system python if poetry is unavailable.
    fn build_python_command(&self, bridge_script: &PathBuf) -> Result<Command, String> {
        let python_bridge_dir = bridge_script
            .parent()
            .ok_or("Could not determine python-bridge directory")?;

        // Check for pyproject.toml (poetry project)
        let pyproject_path = python_bridge_dir.join("pyproject.toml");
        let poetry_available = pyproject_path.exists();

        debug!(
            "Checking for pyproject.toml at: {:?}, exists: {}",
            pyproject_path, poetry_available
        );

        if poetry_available {
            info!("Using Poetry to run Python from: {:?}", python_bridge_dir);
            let mut cmd = Command::new("poetry");
            cmd.current_dir(python_bridge_dir);
            cmd.arg("run");
            cmd.arg("python");
            cmd.arg("-u"); // Unbuffered output
            cmd.arg(bridge_script);
            return Ok(cmd);
        }

        // Fallback: Try to find a venv
        let venv_python = self.find_venv_python(python_bridge_dir);

        if let Some(venv_path) = venv_python {
            info!("Using venv Python: {:?}", venv_path);
            let mut cmd = Command::new(venv_path);
            cmd.arg("-u"); // Unbuffered output
            cmd.arg(bridge_script);
            return Ok(cmd);
        }

        // Last resort: system python (with warning)
        warn!("Poetry and venv not found - falling back to system Python. Dependencies may be missing!");

        let python_cmd = if cfg!(target_os = "windows") {
            "python"
        } else {
            "python3"
        };

        info!("Using system {}", python_cmd);
        let mut cmd = Command::new(python_cmd);
        cmd.arg("-u"); // Unbuffered output
        cmd.arg(bridge_script);
        Ok(cmd)
    }

    /// Finds a venv Python executable in the given directory
    fn find_venv_python(&self, dir: &std::path::Path) -> Option<PathBuf> {
        // Paths to check in order of preference
        let venv_paths = if cfg!(target_os = "windows") {
            vec![
                dir.join(".venv/Scripts/python.exe"),
                dir.join("venv/Scripts/python.exe"),
            ]
        } else {
            vec![dir.join(".venv/bin/python"), dir.join("venv/bin/python")]
        };

        for path in venv_paths {
            debug!("Checking venv path: {:?}, exists: {}", path, path.exists());
            if path.exists() {
                return Some(path);
            }
        }

        None
    }

    /// Finds the bundled executor sidecar executable.
    /// Returns None if running in development mode (executable not found).
    ///
    /// Validation:
    /// - File must exist
    /// - File must be larger than 1MB (to skip placeholder/empty files)
    fn find_bundled_executor(&self) -> Option<PathBuf> {
        // Minimum file size for a valid bundled executor (1MB)
        // A real PyInstaller bundle with Python + dependencies is ~100MB+
        const MIN_EXECUTOR_SIZE: u64 = 1_000_000;

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

        // Helper to validate a path
        let is_valid_executor = |path: &PathBuf| -> bool {
            if !path.exists() {
                return false;
            }
            match std::fs::metadata(path) {
                Ok(metadata) => {
                    let size = metadata.len();
                    if size < MIN_EXECUTOR_SIZE {
                        debug!(
                            "Executor at {:?} is too small ({} bytes), skipping",
                            path, size
                        );
                        false
                    } else {
                        true
                    }
                }
                Err(e) => {
                    debug!("Could not read metadata for {:?}: {}", path, e);
                    false
                }
            }
        };

        // Try to resolve using Tauri's resource directory
        if let Ok(resource_dir) = self.app_handle.path().resource_dir() {
            let sidecar_path = resource_dir.join(&sidecar_name);
            debug!("Checking bundled sidecar at: {:?}", sidecar_path);
            if is_valid_executor(&sidecar_path) {
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
            debug!("Checking sidecar path: {:?}", path);
            if is_valid_executor(&path) {
                info!("Found bundled executor at: {:?}", path);
                return Some(path);
            }
        }

        debug!("Bundled executor not found or invalid, will use Python mode");
        None
    }

    /// Builds a command for the bundled executor
    fn build_bundled_command(&self, executor_path: &PathBuf) -> Command {
        info!("Using bundled executor: {:?}", executor_path);
        Command::new(executor_path)
    }
}
