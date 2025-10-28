use super::health::{HealthCheckTask, HealthMonitor};
use super::lifecycle::{parse_executor_message, ExecutorLifecycle, ExecutorMessage};
use super::state::ExecutorState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info};

const COMMAND_CHANNEL_CAPACITY: usize = 100;

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

/// Python executor bridge with lifecycle management and health monitoring
pub struct PythonBridge {
    /// Python process handle
    process: Option<Child>,

    /// Lifecycle manager
    lifecycle: Arc<RwLock<ExecutorLifecycle>>,

    /// Health monitor
    health_monitor: Arc<HealthMonitor>,

    /// Tauri app handle for emitting events
    app_handle: tauri::AppHandle,

    /// Channel for sending commands to Python
    command_tx: Option<mpsc::Sender<ExecutorCommand>>,

    /// Runtime for async tasks
    runtime: Arc<tokio::runtime::Runtime>,
}

impl PythonBridge {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        let runtime = Arc::new(
            tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"),
        );

        Self {
            process: None,
            lifecycle: Arc::new(RwLock::new(ExecutorLifecycle::new())),
            health_monitor: Arc::new(HealthMonitor::new()),
            app_handle,
            command_tx: None,
            runtime,
        }
    }

    #[allow(dead_code)]
    pub fn start(&mut self) -> Result<(), String> {
        self.start_with_executor("simple")
    }

    pub fn start_with_executor(&mut self, executor_type: &str) -> Result<(), String> {
        // Check if process is already running
        if self.process.is_some() {
            return Err("Python process already running".to_string());
        }

        info!("Starting Python executor with type: {}", executor_type);

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
        let mut cmd = self.build_python_command(&bridge_script, executor_type)?;

        // Add mock flag if not real mode
        if executor_type != "real" {
            cmd.arg("--mock");
        }

        // Set PYTHONUNBUFFERED to force immediate output (more reliable than -u flag)
        cmd.env("PYTHONUNBUFFERED", "1");

        // Spawn Python process
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start Python process: {}", e))?;

        info!("Python process spawned, waiting for READY signal");

        // Set up stdout reader with lifecycle management
        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let stderr = child.stderr.take().ok_or("Failed to capture stderr")?;

        // Create command channel
        let (cmd_tx, cmd_rx) = mpsc::channel::<ExecutorCommand>(COMMAND_CHANNEL_CAPACITY);
        self.command_tx = Some(cmd_tx.clone());

        // Store stdin for command sending
        let stdin = child.stdin.take().ok_or("Failed to capture stdin")?;

        // Clone shared state for tasks
        let lifecycle = Arc::clone(&self.lifecycle);
        let health_monitor = Arc::clone(&self.health_monitor);
        let app_handle = self.app_handle.clone();
        let runtime = Arc::clone(&self.runtime);

        // Spawn stdout reader task
        let lifecycle_stdout = Arc::clone(&lifecycle);
        let app_handle_stdout = app_handle.clone();
        let health_monitor_stdout = Arc::clone(&health_monitor);

        std::thread::spawn(move || {
            runtime.block_on(async {
                Self::stdout_reader_task(
                    stdout,
                    lifecycle_stdout,
                    health_monitor_stdout,
                    app_handle_stdout,
                )
                .await;
            });
        });

        // Spawn stderr reader task
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                error!("Python stderr: {}", line);
            }
            info!("Stderr reader thread ending");
        });

        // Spawn command sender task
        let runtime_cmd = Arc::clone(&self.runtime);
        std::thread::spawn(move || {
            runtime_cmd.block_on(async {
                Self::command_sender_task(stdin, cmd_rx).await;
            });
        });

        // Store process handle
        self.process = Some(child);

        // TEMPORARY FIX: Don't block on READY signal to restore original working behavior
        // The READY signal will still be processed asynchronously when it arrives
        // This avoids the 30-second buffering issue on Windows
        info!("Python process started, will process READY signal asynchronously");

        // Start immediately without waiting (original behavior that worked)
        {
                info!("Starting health monitoring");

                // Start health monitoring
                self.runtime.block_on(async {
                    self.health_monitor.start().await;
                });

                // Start health check task
                let health_monitor_task = Arc::clone(&self.health_monitor);
                let cmd_tx_health = cmd_tx.clone();
                let (ping_tx, mut ping_rx) = mpsc::channel::<()>(10);

                let runtime_health = Arc::clone(&self.runtime);
                std::thread::spawn(move || {
                    runtime_health.block_on(async {
                        let task = HealthCheckTask::new(health_monitor_task, ping_tx);
                        if let Err(e) = task.run().await {
                            error!("Health check task error: {}", e);
                        }
                    });
                });

                // Spawn task to send ping commands
                let runtime_ping = Arc::clone(&self.runtime);
                std::thread::spawn(move || {
                    runtime_ping.block_on(async {
                        while let Some(()) = ping_rx.recv().await {
                            let ping_cmd = ExecutorCommand {
                                cmd_type: "command".to_string(),
                                id: uuid::Uuid::new_v4().to_string(),
                                command: "ping".to_string(),
                                params: None,
                            };

                            if cmd_tx_health.send(ping_cmd).await.is_err() {
                                break;
                            }
                        }
                    });
                });

                // Drain any queued events
                let queued_events = self.runtime.block_on(async {
                    self.lifecycle.read().await.drain_queued_events().await
                });

                for queued in queued_events {
                    info!(
                        "Processing queued event: {} (queued at {})",
                        queued.event_type, queued.queued_at
                    );
                    let event = ExecutorEvent {
                        event_type: "event".to_string(),
                        event: queued.event_type,
                        timestamp: queued.queued_at as f64 / 1000.0,
                        sequence: 0,
                        data: queued.data,
                    };
                    if let Err(e) = self.app_handle.emit("executor-event", &event) {
                        error!("Failed to emit queued event: {}", e);
                    }
                }

                Ok(())
        }
    }

    /// Stdout reader task with lifecycle and health management
    async fn stdout_reader_task(
        stdout: std::process::ChildStdout,
        lifecycle: Arc<RwLock<ExecutorLifecycle>>,
        health_monitor: Arc<HealthMonitor>,
        app_handle: tauri::AppHandle,
    ) {
        let reader = BufReader::new(stdout);

        for line in reader.lines() {
            match line {
                Ok(line) => {
                    debug!("Python stdout: {}", line);

                    // Parse message
                    match parse_executor_message(&line) {
                        Ok(message) => {
                            debug!("Parsed message: {:?}", message);

                            // Handle pong messages for health monitoring
                            if matches!(message, ExecutorMessage::Pong { .. }) {
                                health_monitor.record_pong().await;
                                continue;
                            }

                            // Process message through lifecycle
                            let mut lifecycle_guard = lifecycle.write().await;
                            match lifecycle_guard.handle_message(message.clone()).await {
                                Ok(Some(msg)) => {
                                    // Forward message to frontend
                                    Self::emit_message_to_frontend(&app_handle, msg).await;
                                }
                                Ok(None) => {
                                    // Message was handled internally (e.g., queued)
                                    debug!("Message handled internally");
                                }
                                Err(e) => {
                                    error!("Error handling message: {}", e);

                                    // Emit error to frontend
                                    let error_event = json!({
                                        "type": "error",
                                        "error": "message_handling_error",
                                        "message": e.to_string(),
                                    });
                                    let _ = app_handle.emit("executor-error", &error_event);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to parse executor message: {} - Line: {}", e, line);

                            // Emit parse error to frontend with full context
                            let error_event = json!({
                                "type": "error",
                                "error": "parse_error",
                                "message": format!("Failed to parse executor output: {}", e),
                                "raw_line": line,
                                "timestamp": chrono::Utc::now().timestamp_millis(),
                            });
                            let _ = app_handle.emit("executor-error", &error_event);
                        }
                    }
                }
                Err(e) => {
                    error!("Error reading stdout: {}", e);
                    break;
                }
            }
        }

        info!("Stdout reader task ending");

        // Mark as failed if not already in terminal state
        let lifecycle_guard = lifecycle.write().await;
        let state = lifecycle_guard.get_state().await;
        if !state.is_terminal() {
            let _ = lifecycle_guard
                .mark_failed("Python process stdout closed unexpectedly".to_string())
                .await;

            // Emit error to frontend
            let error_event = json!({
                "type": "error",
                "error": "executor_crashed",
                "message": "Python executor process terminated unexpectedly",
            });
            let _ = app_handle.emit("executor-error", &error_event);
        }
    }

    /// Command sender task
    async fn command_sender_task(
        mut stdin: std::process::ChildStdin,
        mut cmd_rx: mpsc::Receiver<ExecutorCommand>,
    ) {
        while let Some(cmd) = cmd_rx.recv().await {
            match serde_json::to_string(&cmd) {
                Ok(json) => {
                    if let Err(e) = writeln!(stdin, "{}", json) {
                        error!("Failed to write command to stdin: {}", e);
                        break;
                    }

                    if let Err(e) = stdin.flush() {
                        error!("Failed to flush stdin: {}", e);
                        break;
                    }

                    debug!("Sent command: {}", cmd.command);
                }
                Err(e) => {
                    error!("Failed to serialize command: {}", e);
                }
            }
        }

        info!("Command sender task ending");
    }

    /// Emits a message to the Tauri frontend
    async fn emit_message_to_frontend(app_handle: &tauri::AppHandle, message: ExecutorMessage) {
        match message {
            ExecutorMessage::Event {
                event,
                data,
                timestamp,
                sequence,
            } => {
                let event_obj = ExecutorEvent {
                    event_type: "event".to_string(),
                    event,
                    timestamp,
                    sequence,
                    data,
                };
                if let Err(e) = app_handle.emit("executor-event", &event_obj) {
                    error!("Failed to emit event: {}", e);
                }
            }
            ExecutorMessage::Response {
                id,
                success,
                data,
                error,
            } => {
                let response = ExecutorResponse {
                    resp_type: "response".to_string(),
                    id,
                    success,
                    data,
                    error,
                };
                if let Err(e) = app_handle.emit("executor-response", &response) {
                    error!("Failed to emit response: {}", e);
                }
            }
            ExecutorMessage::Error { message, details } => {
                let error_event = json!({
                    "type": "error",
                    "error": "executor_error",
                    "message": message,
                    "details": details,
                });
                if let Err(e) = app_handle.emit("executor-error", &error_event) {
                    error!("Failed to emit error: {}", e);
                }
            }
            _ => {
                debug!("Unhandled message type: {:?}", message);
            }
        }
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
            let venv_path = p.join("venv/Scripts/python.exe");
            debug!(
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

    pub fn stop(&mut self) -> Result<(), String> {
        info!("Stopping Python executor");

        // Stop health monitoring
        self.runtime.block_on(async {
            self.health_monitor.stop().await;
        });

        // Initiate shutdown
        self.runtime.block_on(async {
            let lifecycle = self.lifecycle.read().await;
            let _ = lifecycle.shutdown().await;
        });

        if let Some(mut process) = self.process.take() {
            // Send stop command
            let _ = self.send_command("stop", None);

            // Wait a bit for graceful shutdown
            std::thread::sleep(std::time::Duration::from_millis(500));

            // Kill the process if still running
            process.kill().map_err(|e| e.to_string())?;
            process.wait().map_err(|e| e.to_string())?;
        }

        info!("Python executor stopped");
        Ok(())
    }

    pub fn send_command(&mut self, command: &str, params: Option<Value>) -> Result<(), String> {
        let cmd = ExecutorCommand {
            cmd_type: "command".to_string(),
            id: uuid::Uuid::new_v4().to_string(),
            command: command.to_string(),
            params,
        };

        if let Some(ref tx) = self.command_tx {
            self.runtime
                .block_on(async { tx.send(cmd).await })
                .map_err(|e| format!("Failed to send command: {}", e))?;
            Ok(())
        } else {
            Err("Command channel not initialized".to_string())
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
        self.runtime.block_on(async {
            let state = self.lifecycle.read().await.get_state().await;
            state.can_accept_commands()
        })
    }

    #[allow(dead_code)]
    pub fn get_state(&self) -> ExecutorState {
        self.runtime.block_on(async {
            self.lifecycle.read().await.get_state().await
        })
    }
}

impl Drop for PythonBridge {
    fn drop(&mut self) {
        // Always attempt to stop, regardless of reported state
        // This ensures cleanup even if state tracking is inconsistent
        if let Err(e) = self.stop() {
            error!("Error during PythonBridge drop: {}", e);
        }
    }
}
