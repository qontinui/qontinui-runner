//! Extraction executor for running extraction operations in parallel with GUI automation.
//!
//! This is a separate Python process that handles extraction operations independently
//! of the main PythonBridge. This allows extraction to run concurrently with AI workflows
//! since extraction uses Playwright (browser automation) rather than HAL (GUI automation).

use super::lifecycle::{CommandResponseResult, ExecutorLifecycle};
use super::output::OutputProcessor;
use super::process::ProcessManager;
use super::protocol::{ExecutorCommand, ExecutorEvent, ProtocolHandler};
use super::state::ExecutorState;
use serde_json::Value;
use std::process::Child;
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::RwLock;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Dedicated executor for extraction operations.
///
/// This runs as a separate Python process from PythonBridge, allowing extraction
/// (which uses Playwright) to run in parallel with GUI automation workflows
/// (which use HAL for screen capture and input).
pub struct ExtractionExecutor {
    /// Python process handle
    process: Option<Child>,

    /// Process manager for spawning and configuring Python processes
    process_manager: ProcessManager,

    /// Protocol handler for command/response communication
    protocol_handler: ProtocolHandler,

    /// Lifecycle manager
    lifecycle: Arc<RwLock<ExecutorLifecycle>>,

    /// Tauri app handle for emitting events
    app_handle: tauri::AppHandle,

    /// Runtime for async tasks
    runtime: Arc<tokio::runtime::Runtime>,
}

impl ExtractionExecutor {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        let runtime =
            Arc::new(tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"));

        Self {
            process: None,
            process_manager: ProcessManager::new_for_extraction(app_handle.clone()),
            protocol_handler: ProtocolHandler::new(),
            lifecycle: Arc::new(RwLock::new(ExecutorLifecycle::new())),
            app_handle,
            runtime,
        }
    }

    /// Starts the extraction executor process.
    pub fn start(&mut self) -> Result<(), String> {
        // Check if process is already running
        if self.process.is_some() {
            return Err("Extraction executor already running".to_string());
        }

        info!("Starting extraction executor");

        // Spawn Python process using process manager
        let mut child = self.process_manager.spawn_process()?;

        info!("Extraction executor process spawned, waiting for READY signal");

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
        let app_handle = self.app_handle.clone();
        let runtime = Arc::clone(&self.runtime);

        // Spawn stdout reader task using output processor
        let lifecycle_stdout = Arc::clone(&lifecycle);
        let app_handle_stdout = app_handle.clone();

        std::thread::spawn(move || {
            runtime.block_on(async {
                OutputProcessor::extraction_stdout_reader_task(
                    stdout,
                    lifecycle_stdout,
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

        info!("Extraction executor started, will process READY signal asynchronously");

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
            if let Err(e) = self.app_handle.emit("extraction-event", &event) {
                error!("Failed to emit queued event: {}", e);
            }
        }

        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        info!("Stopping extraction executor");

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

        info!("Extraction executor stopped");
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

    /// Sends a command and waits for its response.
    #[allow(clippy::async_yields_async)]
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

        debug!("Sent extraction command '{}' with ID: {}", command, cmd_id);

        // Wait for response with timeout
        let result = self.runtime.block_on(async {
            match timeout(timeout_duration, response_rx).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(_)) => Err("Response channel closed unexpectedly".to_string()),
                Err(_) => Err(format!(
                    "Timeout waiting for extraction command '{}' response after {:?}",
                    command, timeout_duration
                )),
            }
        });

        result
    }

    pub fn is_running(&self) -> bool {
        debug!("[EXTRACTION_EXECUTOR] is_running() called");
        let result = self.runtime.block_on(async {
            let state = self.lifecycle.read().await.get_state().await;
            state.can_accept_commands()
        });
        debug!("[EXTRACTION_EXECUTOR] is_running() returning: {}", result);
        result
    }

    pub fn get_state(&self) -> ExecutorState {
        self.runtime
            .block_on(async { self.lifecycle.read().await.get_state().await })
    }

    /// Start extraction executor on-demand if not already running.
    pub fn ensure_started(&mut self) -> Result<(), String> {
        if self.process.is_none() {
            info!("Starting extraction executor on-demand");
            self.start()?;

            // Wait for the executor to become ready (with timeout)
            let start_time = std::time::Instant::now();
            let timeout_duration = std::time::Duration::from_secs(30);

            while start_time.elapsed() < timeout_duration {
                if self.is_running() {
                    info!("Extraction executor is ready");
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            warn!("Extraction executor started but may not be fully ready yet");
        }
        Ok(())
    }
}

impl Drop for ExtractionExecutor {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            error!("Error during ExtractionExecutor drop: {}", e);
        }
    }
}
