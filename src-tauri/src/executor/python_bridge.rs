use std::process::{Child, Command, Stdio};
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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

        // Always use qontinui_bridge.py - it handles both real and mock modes
        let script_name = "qontinui_bridge.py";

        // Get the path to the Python bridge script
        // Try multiple possible locations
        let possible_paths = vec![
            // When running from src-tauri
            std::env::current_dir().ok().and_then(|p| p.parent().map(|p| p.join("python-bridge").join(script_name))),
            // When running from qontinui-runner
            std::env::current_dir().ok().map(|p| p.join("python-bridge").join(script_name)),
            // Absolute path fallback
            Some(std::path::PathBuf::from(format!("/home/jspinak/qontinui_parent_directory/qontinui-runner/python-bridge/{}", script_name))),
        ];
        
        let bridge_script = possible_paths
            .into_iter()
            .flatten()
            .find(|p| p.exists())
            .ok_or(format!("Python bridge script {} not found in any expected location", script_name))?;

        if !bridge_script.exists() {
            return Err(format!("Python bridge script not found at: {:?}", bridge_script));
        }

        // Start the Python process with appropriate mode
        let mut cmd = Command::new("python3");
        cmd.arg(bridge_script);
        
        // Pass --mock flag for simulation/mock mode
        // executor_type values: "real", "mock", "simulation", "qontinui", "simple"
        // Only "real" mode should NOT have --mock flag
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
                        } else if let Ok(response) = serde_json::from_str::<ExecutorResponse>(&line) {
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
            for line in reader.lines() {
                if let Ok(line) = line {
                    eprintln!("Python stderr: {}", line);
                }
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

                let json = serde_json::to_string(&cmd)
                    .map_err(|e| e.to_string())?;
                
                writeln!(stdin, "{}", json)
                    .map_err(|e| format!("Failed to send command: {}", e))?;
                
                stdin.flush()
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
        self.send_command("load", Some(json!({
            "config_path": config_path
        })))
    }

    #[allow(dead_code)]
    pub fn start_execution(&mut self, mode: &str) -> Result<(), String> {
        self.send_command("start", Some(json!({
            "mode": mode
        })))
    }
    
    pub fn start_execution_with_params(&mut self, mode: &str, params: Option<serde_json::Value>) -> Result<(), String> {
        let mut command_params = json!({
            "mode": mode
        });
        
        // Merge additional params if provided
        if let Some(p) = params {
            if let serde_json::Value::Object(map) = p {
                for (key, value) in map {
                    command_params[key] = value;
                }
            }
        }
        
        self.send_command("start", Some(command_params))
    }

    pub fn stop_execution(&mut self) -> Result<(), String> {
        self.send_command("stop", None)
    }

    pub fn get_status(&mut self) -> Result<(), String> {
        self.send_command("status", None)
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