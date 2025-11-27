use super::health::{HealthCheckTask, HealthMonitor};
use super::lifecycle::ExecutorLifecycle;
use super::output::OutputProcessor;
use super::process::ProcessManager;
use super::protocol::{ExecutorCommand, ProtocolHandler};
use super::state::ExecutorState;
use serde_json::{json, Value};
use std::process::Child;
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info};

// Re-export protocol types for backward compatibility
pub use super::protocol::{ExecutorEvent, ExecutorResponse};

/// Python executor bridge with lifecycle management and health monitoring
/// Acts as a facade that delegates to specialized handlers
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
}

impl PythonBridge {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        let runtime = Arc::new(
            tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"),
        );

        Self {
            process: None,
            process_manager: ProcessManager::new(app_handle.clone()),
            protocol_handler: ProtocolHandler::new(),
            lifecycle: Arc::new(RwLock::new(ExecutorLifecycle::new())),
            health_monitor: Arc::new(HealthMonitor::new()),
            app_handle,
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

        info!("Starting executor with type: {}", executor_type);

        // Load debug settings to send to Python
        let debug_settings = crate::settings::get_debug_settings();

        // Spawn Python process using process manager
        let mut child = self.process_manager.spawn_process(executor_type)?;

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
        let lifecycle_stdout = Arc::clone(&lifecycle);
        let app_handle_stdout = app_handle.clone();
        let health_monitor_stdout = Arc::clone(&health_monitor);

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

            if let Err(e) = self
                .runtime
                .block_on(async { cmd_tx.send(debug_cmd).await })
            {
                error!("Failed to send debug settings to Python: {}", e);
            }

            // Drain any queued events
            let queued_events = self
                .runtime
                .block_on(async { self.lifecycle.read().await.drain_queued_events().await });

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
        project_id: Option<i32>,
    ) -> Result<(), String> {
        self.send_command(
            "ws_configure",
            Some(json!({
                "enabled": enabled,
                "api_url": url,
                "jwt_token": token,
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

    pub fn is_running(&self) -> bool {
        self.runtime.block_on(async {
            let state = self.lifecycle.read().await.get_state().await;
            state.can_accept_commands()
        })
    }

    #[allow(dead_code)]
    pub fn get_state(&self) -> ExecutorState {
        self.runtime
            .block_on(async { self.lifecycle.read().await.get_state().await })
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
