use super::health::{HealthCheckTask, HealthMonitor};
use super::lifecycle::{CommandResponseResult, ExecutorLifecycle};
use super::output::OutputProcessor;
use super::process::ProcessManager;
use super::protocol::{ExecutorCommand, ProtocolHandler};
use super::state::ExecutorState;
use serde_json::{json, Value};
use std::process::Child;
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::timeout;
use tracing::{debug, error, info, instrument};

// Re-export protocol types for backward compatibility
pub use super::protocol::ExecutorEvent;

/// Python executor bridge with lifecycle management and health monitoring.
/// Acts as a facade that delegates to specialized handlers.
pub struct PythonBridge {
    /// Python process handle
    process: Option<Child>,

    /// Process manager for spawning and configuring Python processes
    process_manager: ProcessManager,

    /// Protocol handler for command/response communication
    protocol_handler: ProtocolHandler,

    /// Lifecycle manager
    lifecycle: Arc<RwLock<ExecutorLifecycle>>,

    /// Health monitor
    health_monitor: Arc<HealthMonitor>,

    /// Tauri app handle for emitting events
    app_handle: tauri::AppHandle,

    /// Runtime for async tasks
    runtime: Arc<tokio::runtime::Runtime>,

    /// Headless event channel (only used when in headless mode).
    /// Events go here instead of Tauri frontend emit.
    headless_events: Option<broadcast::Sender<serde_json::Value>>,
}

impl PythonBridge {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        let runtime =
            Arc::new(tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"));

        Self {
            process: None,
            process_manager: ProcessManager::new(app_handle.clone()),
            protocol_handler: ProtocolHandler::new(),
            lifecycle: Arc::new(RwLock::new(ExecutorLifecycle::new())),
            health_monitor: Arc::new(HealthMonitor::new()),
            app_handle,
            runtime,
            headless_events: None,
        }
    }

    /// Create a new PythonBridge in headless mode.
    ///
    /// Headless mode disables GUI interactions (screen capture, mouse/keyboard).
    /// Multiple headless bridges can run in parallel.
    pub fn new_headless(app_handle: tauri::AppHandle) -> Self {
        let runtime =
            Arc::new(tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"));

        Self {
            process: None,
            process_manager: ProcessManager::new_headless(app_handle.clone()),
            protocol_handler: ProtocolHandler::new(),
            lifecycle: Arc::new(RwLock::new(ExecutorLifecycle::new())),
            health_monitor: Arc::new(HealthMonitor::new()),
            app_handle,
            runtime,
            headless_events: None,
        }
    }

    /// Set headless mode before starting the bridge.
    ///
    /// Must be called before `start()`.
    pub fn set_headless(&mut self, headless: bool) {
        self.process_manager.set_headless(headless);
    }

    /// Check if this bridge is in headless mode.
    pub fn is_headless(&self) -> bool {
        self.process_manager.is_headless()
    }

    /// Set the headless event channel for this bridge.
    ///
    /// When set, events will be sent to this channel instead of being emitted
    /// to the Tauri frontend. Must be called before `start()`.
    pub fn set_headless_events(&mut self, tx: broadcast::Sender<serde_json::Value>) {
        self.headless_events = Some(tx);
    }

    /// Get a clone of the headless event sender, if any.
    pub fn get_headless_events(&self) -> Option<broadcast::Sender<serde_json::Value>> {
        self.headless_events.clone()
    }

    /// Starts the Python executor process.
    ///
    /// This is an async function that must be called from an async context.
    /// For synchronous contexts, use the bridge manager which handles the async call.
    #[instrument(name = "qontinui.python.startup", skip(self))]
    pub async fn start(&mut self) -> Result<(), String> {
        // Check if process is already running
        if self.process.is_some() {
            return Err("Python process already running".to_string());
        }

        info!("Starting Python executor");

        // Load debug settings to send to Python
        let debug_settings = crate::settings::get_debug_settings();

        // Spawn Python process using process manager
        let mut child = self.process_manager.spawn_process()?;

        info!("Python process spawned, waiting for READY signal");

        // Set up stdout reader with lifecycle management
        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let stderr = child.stderr.take().ok_or("Failed to capture stderr")?;

        // Create command channel using protocol handler
        let (cmd_tx, cmd_rx) = ProtocolHandler::create_channel();
        self.protocol_handler.set_sender(cmd_tx.clone());

        // Store stdin for command sending
        let stdin = child.stdin.take().ok_or("Failed to capture stdin")?;

        // Clone shared state for tasks
        let lifecycle = Arc::clone(&self.lifecycle);
        let health_monitor = Arc::clone(&self.health_monitor);
        let app_handle = self.app_handle.clone();
        let runtime = Arc::clone(&self.runtime);

        // Spawn stdout reader task using output processor
        // Use headless reader if we have a headless event channel
        let lifecycle_stdout = Arc::clone(&lifecycle);
        let app_handle_stdout = app_handle.clone();
        let health_monitor_stdout = Arc::clone(&health_monitor);
        let headless_tx = self.headless_events.clone();

        if let Some(tx) = headless_tx {
            // Headless mode: send events to dedicated channel instead of Tauri frontend
            info!("Starting headless stdout reader task");
            std::thread::spawn(move || {
                runtime.block_on(async {
                    OutputProcessor::headless_stdout_reader_task(
                        stdout,
                        lifecycle_stdout,
                        health_monitor_stdout,
                        app_handle_stdout,
                        tx,
                    )
                    .await;
                });
            });
        } else {
            // GUI mode: emit events to Tauri frontend
            std::thread::spawn(move || {
                runtime.block_on(async {
                    OutputProcessor::stdout_reader_task(
                        stdout,
                        lifecycle_stdout,
                        health_monitor_stdout,
                        app_handle_stdout,
                    )
                    .await;
                });
            });
        }

        // Spawn stderr reader task using output processor
        std::thread::spawn(move || {
            OutputProcessor::stderr_reader_task(stderr);
        });

        // Spawn command sender task using protocol handler
        let runtime_cmd = Arc::clone(&self.runtime);
        std::thread::spawn(move || {
            runtime_cmd.block_on(async {
                ProtocolHandler::command_sender_task(stdin, cmd_rx).await;
            });
        });

        // Store process handle
        self.process = Some(child);

        // Start immediately without waiting for READY signal
        // The READY signal will be processed asynchronously when it arrives
        info!("Python process started, will process READY signal asynchronously");

        // Health monitoring will be started by the output processor when READY is received
        // (not here, to avoid false-positive health check failures during Python startup)

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

        // Send debug settings to Python executor
        info!(
            "Sending debug settings to Python executor: {:?}",
            debug_settings
        );
        let debug_params = json!({
            "enable_image_debug": debug_settings.enable_image_debug,
            "top_matches_count": debug_settings.top_matches_count,
        });

        let debug_cmd = ExecutorCommand {
            cmd_type: "command".to_string(),
            id: uuid::Uuid::new_v4().to_string(),
            command: "set_debug_settings".to_string(),
            params: Some(debug_params),
        };

        if let Err(e) = cmd_tx.send(debug_cmd).await {
            error!("Failed to send debug settings to Python: {}", e);
        }

        // Drain any queued events
        let queued_events = self.lifecycle.read().await.drain_queued_events().await;

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

    /// Stops the Python executor process (async version).
    ///
    /// This is the preferred method when called from an async context.
    pub async fn stop(&mut self) -> Result<(), String> {
        info!("Stopping Python executor");

        // Stop health monitoring
        self.health_monitor.stop().await;

        // Initiate shutdown
        {
            let lifecycle = self.lifecycle.read().await;
            let _ = lifecycle.shutdown().await;
        }

        if let Some(mut process) = self.process.take() {
            // Send stop command
            let _ = self.send_command_async("stop", None).await;

            // Wait a bit for graceful shutdown
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            // Kill the process if still running
            process.kill().map_err(|e| e.to_string())?;
            process.wait().map_err(|e| e.to_string())?;
        }

        info!("Python executor stopped");
        Ok(())
    }

    /// Stops the Python executor process (sync version for Drop).
    ///
    /// This version runs the async operations in a separate thread to avoid
    /// "cannot start a runtime from within a runtime" panics when Drop is
    /// called from an async context.
    pub fn stop_sync(&mut self) -> Result<(), String> {
        info!("Stopping Python executor (sync)");

        // Run async cleanup in a dedicated thread to avoid nested runtime issues
        let runtime = Arc::clone(&self.runtime);
        let health_monitor = Arc::clone(&self.health_monitor);
        let lifecycle = Arc::clone(&self.lifecycle);
        let protocol_handler_sender = self.protocol_handler.get_sender();

        let cleanup_handle = std::thread::spawn(move || {
            runtime.block_on(async {
                // Stop health monitoring
                health_monitor.stop().await;

                // Initiate shutdown
                {
                    let lc = lifecycle.read().await;
                    let _ = lc.shutdown().await;
                }

                // Send stop command if channel is available
                if let Some(tx) = protocol_handler_sender {
                    let stop_cmd = ExecutorCommand {
                        cmd_type: "command".to_string(),
                        id: uuid::Uuid::new_v4().to_string(),
                        command: "stop".to_string(),
                        params: None,
                    };
                    let _ = tx.send(stop_cmd).await;
                }
            });
        });

        // Wait for cleanup thread with timeout
        let _ = cleanup_handle.join();

        if let Some(mut process) = self.process.take() {
            // Wait a bit for graceful shutdown
            std::thread::sleep(std::time::Duration::from_millis(500));

            // Kill the process if still running
            process.kill().map_err(|e| e.to_string())?;
            process.wait().map_err(|e| e.to_string())?;
        }

        info!("Python executor stopped (sync)");
        Ok(())
    }

    /// Send a command to the Python executor (async version).
    pub async fn send_command_async(
        &self,
        command: &str,
        params: Option<Value>,
    ) -> Result<(), String> {
        let cmd = ExecutorCommand {
            cmd_type: "command".to_string(),
            id: uuid::Uuid::new_v4().to_string(),
            command: command.to_string(),
            params,
        };

        if let Some(tx) = self.protocol_handler.get_sender() {
            tx.send(cmd)
                .await
                .map_err(|e| format!("Failed to send command: {}", e))?;
            Ok(())
        } else {
            Err("Command channel not initialized".to_string())
        }
    }

    /// Send a command to the Python executor (sync version).
    ///
    /// Note: This uses block_on internally and should NOT be called from async contexts.
    /// Use send_command_async instead when in an async context.
    pub fn send_command(&mut self, command: &str, params: Option<Value>) -> Result<(), String> {
        let cmd = ExecutorCommand {
            cmd_type: "command".to_string(),
            id: uuid::Uuid::new_v4().to_string(),
            command: command.to_string(),
            params,
        };

        if let Some(tx) = self.protocol_handler.get_sender() {
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

    pub fn start_execution_with_params(
        &mut self,
        params: Option<serde_json::Value>,
    ) -> Result<(), String> {
        self.send_command("start", params)
    }

    pub fn stop_execution(&mut self) -> Result<(), String> {
        self.send_command("stop", None)
    }

    pub fn pause_execution(&mut self) -> Result<(), String> {
        self.send_command("pause", None)
    }

    pub fn resume_execution(&mut self) -> Result<(), String> {
        self.send_command("resume", None)
    }

    pub fn get_status(&mut self) -> Result<(), String> {
        self.send_command("status", None)
    }

    pub fn set_debug_settings(
        &mut self,
        enable_image_debug: bool,
        top_matches_count: u32,
    ) -> Result<(), String> {
        self.send_command(
            "set_debug_settings",
            Some(json!({
                "settings": {
                    "enable_image_debug": enable_image_debug,
                    "top_matches_count": top_matches_count,
                }
            })),
        )
    }

    pub fn update_capture_settings(
        &self,
        settings: crate::config::ScreenshotCaptureSettings,
    ) -> Result<(), String> {
        if let Some(tx) = self.protocol_handler.get_sender() {
            let cmd = ExecutorCommand {
                cmd_type: "command".to_string(),
                id: uuid::Uuid::new_v4().to_string(),
                command: "update_capture_settings".to_string(),
                params: Some(json!({
                    "settings": settings
                })),
            };
            self.runtime
                .block_on(async { tx.send(cmd).await })
                .map_err(|e| format!("Failed to send command: {}", e))?;
            Ok(())
        } else {
            Err("Command channel not initialized".to_string())
        }
    }

    pub fn configure_websocket(
        &mut self,
        enabled: bool,
        url: String,
        token: String,
        project_id: Option<String>,
        runner_name: Option<String>,
        runner_port: Option<u16>,
    ) -> Result<(), String> {
        // Configure WebSocket
        self.send_command(
            "ws_configure",
            Some(json!({
                "enabled": enabled,
                "api_url": url,
                "jwt_token": token,
                "project_id": project_id,
                "runner_name": runner_name,
                "runner_port": runner_port,
            })),
        )?;

        // Also configure test results with the same credentials
        // Test results use HTTP API on the same backend
        if let Some(ref project) = project_id {
            self.send_command(
                "test_results_configure",
                Some(json!({
                    "enabled": enabled,
                    "api_url": url,
                    "access_token": token,
                    "project_id": project,
                })),
            )?;
        }

        Ok(())
    }

    #[allow(dead_code)] // Reserved for future use - test results configuration
    pub fn configure_test_results(
        &mut self,
        enabled: bool,
        api_url: String,
        access_token: String,
        project_id: Option<String>,
    ) -> Result<(), String> {
        self.send_command(
            "test_results_configure",
            Some(json!({
                "enabled": enabled,
                "api_url": api_url,
                "access_token": access_token,
                "project_id": project_id,
            })),
        )
    }

    pub fn connect_websocket(&mut self) -> Result<(), String> {
        self.send_command("ws_connect", None)
    }

    pub fn disconnect_websocket(&mut self) -> Result<(), String> {
        self.send_command("ws_disconnect", None)
    }

    /// Check if the executor is running (sync version).
    ///
    /// WARNING: This method uses block_on internally and MUST NOT be called from
    /// within a tokio async context. If you're in an async function, use
    /// `is_running_async()` instead.
    pub fn is_running(&self) -> bool {
        debug!("[PYTHON_BRIDGE] is_running() called");
        let result = self.runtime.block_on(async {
            debug!("[PYTHON_BRIDGE] is_running() - getting lifecycle read lock");
            let state = self.lifecycle.read().await.get_state().await;
            debug!("[PYTHON_BRIDGE] is_running() - got state: {}", state.name());
            let can_accept = state.can_accept_commands();
            debug!(
                "[PYTHON_BRIDGE] is_running() - can_accept_commands: {}",
                can_accept
            );
            can_accept
        });
        debug!("[PYTHON_BRIDGE] is_running() returning: {}", result);
        result
    }

    /// Check if the executor is running (async version).
    ///
    /// Use this version when calling from within an async context (e.g., Tauri async commands,
    /// axum handlers, async functions). This avoids the nested runtime panic that would occur
    /// with `is_running()`.
    pub async fn is_running_async(&self) -> bool {
        debug!("[PYTHON_BRIDGE] is_running_async() called");
        let state = self.lifecycle.read().await.get_state().await;
        debug!(
            "[PYTHON_BRIDGE] is_running_async() - got state: {}",
            state.name()
        );
        let can_accept = state.can_accept_commands();
        debug!(
            "[PYTHON_BRIDGE] is_running_async() - can_accept_commands: {}",
            can_accept
        );
        can_accept
    }

    /// Sends a command and waits for its response.
    /// This is useful for request-response style commands where
    /// we need the result before continuing.
    ///
    /// # Arguments
    /// * `command` - The command name
    /// * `params` - Optional command parameters
    /// * `timeout_duration` - Maximum time to wait for response
    ///
    /// # Returns
    /// The command response result or an error if timeout or send fails
    #[allow(clippy::async_yields_async)]
    #[instrument(
        name = "qontinui.python.command",
        skip(self, params, timeout_duration),
        fields(command = %command, timeout_ms = timeout_duration.as_millis() as u64)
    )]
    pub fn send_command_and_wait(
        &mut self,
        command: &str,
        params: Option<Value>,
        timeout_duration: Duration,
    ) -> Result<CommandResponseResult, String> {
        let cmd_id = uuid::Uuid::new_v4().to_string();

        let cmd = ExecutorCommand {
            cmd_type: "command".to_string(),
            id: cmd_id.clone(),
            command: command.to_string(),
            params,
        };

        let tx = self
            .protocol_handler
            .get_sender()
            .ok_or("Command channel not initialized")?;

        // Register for response before sending command
        let lifecycle = Arc::clone(&self.lifecycle);
        let response_rx = self.runtime.block_on(async {
            let lifecycle_guard = lifecycle.read().await;
            lifecycle_guard
                .register_command_response(cmd_id.clone())
                .await
        });

        // Send the command
        self.runtime
            .block_on(async { tx.send(cmd).await })
            .map_err(|e| format!("Failed to send command: {}", e))?;

        debug!("Sent command '{}' with ID: {}", command, cmd_id);

        // Wait for response with timeout
        let result = self.runtime.block_on(async {
            match timeout(timeout_duration, response_rx).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(_)) => Err("Response channel closed unexpectedly".to_string()),
                Err(_) => Err(format!(
                    "Timeout waiting for command '{}' response after {:?}",
                    command, timeout_duration
                )),
            }
        });

        result
    }

    /// Get the current executor state.
    ///
    /// WARNING: This method uses block_on internally and MUST NOT be called from
    /// within a tokio async context. If you're in an async function, use
    /// `get_state_async()` instead.
    pub fn get_state(&self) -> ExecutorState {
        debug!("[PYTHON_BRIDGE] get_state() called");
        let state = self
            .runtime
            .block_on(async { self.lifecycle.read().await.get_state().await });
        debug!("[PYTHON_BRIDGE] get_state() returning: {}", state.name());
        state
    }

    /// Get the current executor state asynchronously.
    ///
    /// Use this version when calling from within an async context (e.g., axum handlers,
    /// async functions). This avoids the nested runtime panic that would occur with
    /// `get_state()`.
    pub async fn get_state_async(&self) -> ExecutorState {
        debug!("[PYTHON_BRIDGE] get_state_async() called");
        let state = self.lifecycle.read().await.get_state().await;
        debug!(
            "[PYTHON_BRIDGE] get_state_async() returning: {}",
            state.name()
        );
        state
    }

    /// Returns a reference to the lifecycle for accessing completion notifications
    pub fn get_lifecycle(&self) -> Arc<RwLock<ExecutorLifecycle>> {
        Arc::clone(&self.lifecycle)
    }
}

impl Drop for PythonBridge {
    fn drop(&mut self) {
        // Always attempt to stop, regardless of reported state
        // This ensures cleanup even if state tracking is inconsistent
        // Use stop_sync to avoid nested runtime issues when Drop is called from async context
        if let Err(e) = self.stop_sync() {
            error!("Error during PythonBridge drop: {}", e);
        }
    }
}
