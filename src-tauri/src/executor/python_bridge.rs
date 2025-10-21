use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::Emitter;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorCommand {
    #[serde(rename = "type")]
    pub cmd_type: String,
    pub id: String,
    pub command: String,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorResponse {
    #[serde(rename = "type")]
    pub resp_type: String,
    pub id: String,
    pub success: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub event: String,
    pub timestamp: f64,
    pub sequence: u32,
    pub data: Value,
}

pub struct PythonBridge {
    process: Option<Child>,
    is_running: Arc<Mutex<bool>>,
    app_handle: tauri::AppHandle,
}

impl PythonBridge {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self {
            process: None,
            is_running: Arc::new(Mutex::new(false)),
            app_handle,
        }
    }

    #[allow(dead_code)]
    pub fn start(&mut self) -> Result<(), String> {
        self.start_with_executor("simple")
    }

    pub fn start_with_executor(&mut self, executor_type: &str) -> Result<(), String> {
        if *self.is_running.lock().unwrap() {
            return Err("Python process already running".to_string());
        }

        // Use minimal_bridge.py for testing when executor_type is "minimal"
        // Use qontinui_executor.py for "real" mode (has recording support)
        // Otherwise use qontinui_bridge.py which handles both real and mock modes
        let script_name = if executor_type == "minimal" {
            "minimal_bridge.py"
        } else if executor_type == "real" {
            "qontinui_executor.py"
        } else {
            "qontinui_bridge.py"
        };

        // Get the path to the Python bridge script
        // Try multiple possible locations
        let possible_paths = vec![
            // When running from src-tauri (most common in development)
            std::env::current_dir().ok().and_then(|p| {
                // Go up from src-tauri/target/debug to qontinui-runner
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

        // Debug: Print current directory
        eprintln!("Current directory: {:?}", std::env::current_dir());

        let bridge_script = possible_paths
            .into_iter()
            .flatten()
            .inspect(|p| eprintln!("Checking path: {:?}, exists: {}", p, p.exists()))
            .find(|p| p.exists())
            .ok_or(format!(
                "Python bridge script {} not found in any expected location",
                script_name
            ))?;

        eprintln!("Using Python bridge script: {:?}", bridge_script);

        if !bridge_script.exists() {
            return Err(format!(
                "Python bridge script not found at: {:?}",
                bridge_script
            ));
        }

        // Start the Python process with appropriate mode
        // Strategy:
        // 1. For qontinui_executor.py and qontinui_bridge.py: use Poetry (needs qontinui library)
        // 2. For minimal_bridge.py: use system Python (no dependencies)
        // 3. Fall back to venv if it exists

        let use_poetry = script_name == "qontinui_executor.py" || script_name == "qontinui_bridge.py";

        // Check for Poetry and qontinui library location
        let poetry_available = if use_poetry {
            // Check if we can find the qontinui library directory
            let qontinui_path = bridge_script.parent()
                .and_then(|p| p.parent()) // Go up from python-bridge to qontinui-runner
                .and_then(|p| p.parent()) // Go up to qontinui_parent
                .map(|p| p.join("qontinui").join("pyproject.toml"));

            if let Some(ref path) = qontinui_path {
                eprintln!("Checking for qontinui at: {:?}, exists: {}", path, path.exists());
                path.exists()
            } else {
                false
            }
        } else {
            false
        };

        let venv_python = bridge_script.parent().and_then(|p| {
            let venv_path = p.join("venv/Scripts/python.exe");
            eprintln!(
                "Checking venv path: {:?}, exists: {}",
                venv_path,
                venv_path.exists()
            );
            if venv_path.exists() {
                Some(venv_path)
            } else {
                None
            }
        });

        let mut cmd = if poetry_available && use_poetry {
            eprintln!("Using Poetry to run Python with qontinui library");
            let qontinui_dir = bridge_script.parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|p| p.join("qontinui"))
                .expect("Could not determine qontinui directory");

            let mut poetry_cmd = Command::new("poetry");
            poetry_cmd.current_dir(&qontinui_dir);
            poetry_cmd.arg("run");
            poetry_cmd.arg("python");
            poetry_cmd.arg(bridge_script);
            poetry_cmd
        } else if let Some(venv_path) = venv_python {
            eprintln!("Using venv Python: {:?}", venv_path);
            let mut python_cmd = Command::new(venv_path);
            python_cmd.arg(bridge_script);
            python_cmd
        } else if cfg!(target_os = "windows") {
            eprintln!("Using system python");
            let mut python_cmd = Command::new("python");
            python_cmd.arg(bridge_script);
            python_cmd
        } else {
            eprintln!("Using system python3");
            let mut python_cmd = Command::new("python3");
            python_cmd.arg(bridge_script);
            python_cmd
        };

        // Pass --mock flag for simulation/mock mode
        // executor_type values: "real", "mock", "simulation", "qontinui", "simple", "minimal"
        // Only "real" mode should NOT have --mock flag
        // "minimal" uses minimal_bridge.py for testing without qontinui dependency
        if executor_type != "real" {
            cmd.arg("--mock");
        }

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start Python process: {}", e))?;

        // Set up stdout reader
        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let app_handle = self.app_handle.clone();
        let _is_running = self.is_running.clone();

        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        // Debug: Print raw line received from Python
                        eprintln!("Python stdout: {}", line);

                        if let Ok(event) = serde_json::from_str::<ExecutorEvent>(&line) {
                            eprintln!("Parsed as event: {:?}", event);
                            // Emit event to frontend
                            match app_handle.emit("executor-event", &event) {
                                Ok(_) => eprintln!("Event emitted successfully"),
                                Err(e) => eprintln!("Failed to emit event: {}", e),
                            }
                        } else if let Ok(response) = serde_json::from_str::<ExecutorResponse>(&line)
                        {
                            eprintln!("Parsed as response: {:?}", response);
                            // Emit response to frontend
                            match app_handle.emit("executor-response", &response) {
                                Ok(_) => eprintln!("Response emitted successfully"),
                                Err(e) => eprintln!("Failed to emit response: {}", e),
                            }
                        } else {
                            eprintln!("Could not parse line as event or response");
                        }
                    }
                    Err(e) => {
                        eprintln!("Error reading stdout: {}", e);
                        break;
                    }
                }
            }
            eprintln!("Stdout reader thread ending");
            // Don't mark as not running here - let the process itself determine that
        });

        // Set up stderr reader
        let stderr = child.stderr.take().ok_or("Failed to capture stderr")?;
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("Python stderr: {}", line);
            }
        });

        self.process = Some(child);
        *self.is_running.lock().unwrap() = true;

        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(mut process) = self.process.take() {
            // Send stop command
            self.send_command("stop", None)?;

            // Wait a bit for graceful shutdown
            std::thread::sleep(std::time::Duration::from_millis(500));

            // Kill the process if still running
            process.kill().map_err(|e| e.to_string())?;
            process.wait().map_err(|e| e.to_string())?;

            *self.is_running.lock().unwrap() = false;
        }
        Ok(())
    }

    pub fn send_command(&mut self, command: &str, params: Option<Value>) -> Result<(), String> {
        if let Some(ref mut process) = self.process {
            if let Some(ref mut stdin) = process.stdin {
                let cmd = ExecutorCommand {
                    cmd_type: "command".to_string(),
                    id: uuid::Uuid::new_v4().to_string(),
                    command: command.to_string(),
                    params,
                };

                let json = serde_json::to_string(&cmd).map_err(|e| e.to_string())?;

                writeln!(stdin, "{}", json)
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                stdin
                    .flush()
                    .map_err(|e| format!("Failed to flush stdin: {}", e))?;

                Ok(())
            } else {
                Err("No stdin available".to_string())
            }
        } else {
            Err("Python process not running".to_string())
        }
    }

    pub fn load_configuration(&mut self, config_path: &str) -> Result<(), String> {
        self.send_command(
            "load",
            Some(json!({
                "config_path": config_path
            })),
        )
    }

    #[allow(dead_code)]
    pub fn start_execution(&mut self, mode: &str) -> Result<(), String> {
        self.send_command(
            "start",
            Some(json!({
                "mode": mode
            })),
        )
    }

    pub fn start_execution_with_params(
        &mut self,
        params: Option<serde_json::Value>,
    ) -> Result<(), String> {
        self.send_command("start", params)
    }

    pub fn stop_execution(&mut self) -> Result<(), String> {
        self.send_command("stop", None)
    }

    pub fn get_status(&mut self) -> Result<(), String> {
        self.send_command("status", None)
    }

    pub fn start_recording(&mut self, base_dir: &str) -> Result<(), String> {
        self.send_command(
            "start_recording",
            Some(json!({
                "base_dir": base_dir
            })),
        )
    }

    pub fn stop_recording(&mut self) -> Result<(), String> {
        self.send_command("stop_recording", None)
    }

    pub fn get_recording_status(&mut self) -> Result<(), String> {
        self.send_command("recording_status", None)
    }

    pub fn is_running(&self) -> bool {
        if let Some(ref _process) = self.process {
            // Check if the process is actually still running
            // The child process handle doesn't have a direct is_running method,
            // so we rely on our tracking flag
            *self.is_running.lock().unwrap()
        } else {
            false
        }
    }
}

impl Drop for PythonBridge {
    fn drop(&mut self) {
        if self.is_running() {
            self.stop().ok();
        }
    }
}
