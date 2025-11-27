use super::events::EventForwarder;
use super::health::HealthMonitor;
use super::lifecycle::{parse_executor_message, ExecutorLifecycle, ExecutorMessage};
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

/// Handles output processing from Python executor
pub struct OutputProcessor;

impl OutputProcessor {
    /// Parse and log Python stderr output with appropriate log levels
    pub fn log_python_stderr(line: &str) {
        // Python structlog outputs in format: "YYYY-MM-DD HH:MM:SS [level    ] message"
        // Also handle plain output like "[EventTranslator] message"

        // Try to extract log level from structlog format
        if let Some(start) = line.find('[') {
            if let Some(end) = line[start..].find(']') {
                let level_part = &line[start + 1..start + end];
                let level = level_part.trim();

                // Map Python log levels to Rust tracing levels
                match level.to_lowercase().as_str() {
                    "debug" => debug!("Python: {}", line),
                    "info" => info!("Python: {}", line),
                    "warning" | "warn" => tracing::warn!("Python: {}", line),
                    "error" => error!("Python: {}", line),
                    "critical" => error!("Python CRITICAL: {}", line),
                    _ => {
                        // Not a recognized log level, check if it's an info-level message
                        // (like "[EventTranslator] message")
                        if line.contains("event emitted") || line.contains("successfully") {
                            info!("Python: {}", line);
                        } else {
                            // Default to debug for unknown bracketed content
                            debug!("Python: {}", line);
                        }
                    }
                }
                return;
            }
        }

        // No brackets found, treat as info if it looks informational, debug otherwise
        if line.contains("error") || line.contains("Error") || line.contains("ERROR") {
            error!("Python: {}", line);
        } else if line.contains("warning") || line.contains("Warning") || line.contains("WARN") {
            tracing::warn!("Python: {}", line);
        } else if !line.trim().is_empty() {
            debug!("Python: {}", line);
        }
    }

    /// Stdout reader task with lifecycle and health management
    pub async fn stdout_reader_task(
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
                                    EventForwarder::emit_message_to_frontend(&app_handle, msg)
                                        .await;
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

    /// Stderr reader task with log level parsing
    pub fn stderr_reader_task(stderr: std::process::ChildStderr) {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            Self::log_python_stderr(&line);
        }
        info!("Stderr reader thread ending");
    }
}
