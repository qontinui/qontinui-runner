use super::events::EventForwarder;
use super::health::HealthMonitor;
use super::lifecycle::{parse_executor_message, ExecutorLifecycle, ExecutorMessage};
use crate::event_system::EventEmitter;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

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
        info!("[OUTPUT_PROCESSOR] stdout_reader_task started");
        let emitter = EventEmitter::new(app_handle.clone());
        let reader = BufReader::new(stdout);
        let mut line_count = 0;

        for line in reader.lines() {
            line_count += 1;
            match line {
                Ok(line) => {
                    // Skip verbose logging for pong and heartbeat messages
                    let is_pong =
                        line.contains("\"type\": \"pong\"") || line.contains("\"type\":\"pong\"");
                    if !is_pong {
                        info!("[OUTPUT_PROCESSOR] Line #{}: {}", line_count, line);
                    }

                    // Check if this looks like a READY message
                    if line.contains("\"type\"") && line.contains("ready") {
                        info!("[OUTPUT_PROCESSOR] DETECTED READY MESSAGE: {}", line);
                    }

                    // Parse message
                    match parse_executor_message(&line) {
                        Ok(message) => {
                            // Handle pong messages for health monitoring (skip verbose logging)
                            if matches!(message, ExecutorMessage::Pong { .. }) {
                                health_monitor.record_pong().await;
                                continue;
                            }

                            // Start health monitoring when READY signal arrives
                            if matches!(&message, ExecutorMessage::Ready { .. }) {
                                info!("[OUTPUT_PROCESSOR] Starting health monitoring after READY signal");
                                health_monitor.start().await;
                            }

                            info!(
                                "[OUTPUT_PROCESSOR] Parsed message type: {:?}",
                                std::mem::discriminant(&message)
                            );
                            debug!("Parsed message full: {:?}", message);

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
                                    emitter.emit_raw_or_warn("executor-error", &error_event);
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
                            emitter.emit_raw_or_warn("executor-error", &error_event);
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
            emitter.emit_raw_or_error("executor-error", &error_event);
        }
    }

    /// Stderr reader task with log level parsing.
    ///
    /// If `error_monitor_tx` is provided, stderr lines are also forwarded to the
    /// error monitor ingestion task for automatic error detection and storage.
    pub fn stderr_reader_task(
        stderr: std::process::ChildStderr,
        error_monitor_tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) {
        let reader = BufReader::new(stderr);
        let mut dropped_count: u64 = 0;
        for line in reader.lines().map_while(Result::ok) {
            Self::log_python_stderr(&line);

            // Forward to error monitor if channel is available.
            // Use try_send to avoid blocking the stderr reader thread;
            // if the channel is full, the line is dropped (acceptable since
            // the error monitor also has periodic file-based polling).
            if let Some(ref tx) = error_monitor_tx {
                if tx.try_send(line).is_err() {
                    dropped_count += 1;
                    if dropped_count.is_multiple_of(100) {
                        warn!(
                            "Error monitor channel full: {} stderr lines dropped so far",
                            dropped_count
                        );
                    }
                }
            }
        }
        if dropped_count > 0 {
            warn!(
                "Stderr reader ending with {} total dropped lines (channel full)",
                dropped_count
            );
        }
        info!("Stderr reader thread ending");
    }

    /// Stdout reader task for extraction executor (no health monitor)
    ///
    /// This is a simplified version of stdout_reader_task for the extraction executor.
    /// It emits events to "extraction-event" instead of "executor-event".
    pub async fn extraction_stdout_reader_task(
        stdout: std::process::ChildStdout,
        lifecycle: Arc<RwLock<ExecutorLifecycle>>,
        app_handle: tauri::AppHandle,
    ) {
        info!("[EXTRACTION_EXECUTOR] stdout_reader_task started");
        let emitter = EventEmitter::new(app_handle.clone());
        let reader = BufReader::new(stdout);
        let mut line_count = 0;

        for line in reader.lines() {
            line_count += 1;
            match line {
                Ok(line) => {
                    // Log all messages for extraction (typically less verbose than main executor)
                    info!("[EXTRACTION_EXECUTOR] Line #{}: {}", line_count, line);

                    // Check if this looks like a READY message
                    if line.contains("\"type\"") && line.contains("ready") {
                        info!("[EXTRACTION_EXECUTOR] DETECTED READY MESSAGE: {}", line);
                    }

                    // Parse message
                    match parse_executor_message(&line) {
                        Ok(message) => {
                            // Skip pong messages for extraction (no health monitor)
                            if matches!(message, ExecutorMessage::Pong { .. }) {
                                continue;
                            }

                            debug!(
                                "[EXTRACTION_EXECUTOR] Parsed message type: {:?}",
                                std::mem::discriminant(&message)
                            );

                            // Process message through lifecycle
                            let mut lifecycle_guard = lifecycle.write().await;
                            match lifecycle_guard.handle_message(message.clone()).await {
                                Ok(Some(msg)) => {
                                    // Forward message to frontend via extraction-event channel
                                    EventForwarder::emit_extraction_message_to_frontend(
                                        &app_handle,
                                        msg,
                                    )
                                    .await;
                                }
                                Ok(None) => {
                                    debug!("Message handled internally");
                                }
                                Err(e) => {
                                    error!("Error handling extraction message: {}", e);

                                    let error_event = json!({
                                        "type": "error",
                                        "error": "message_handling_error",
                                        "message": e.to_string(),
                                    });
                                    emitter.emit_raw_or_warn("extraction-error", &error_event);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to parse extraction message: {} - Line: {}", e, line);

                            let error_event = json!({
                                "type": "error",
                                "error": "parse_error",
                                "message": format!("Failed to parse extraction output: {}", e),
                                "raw_line": line,
                                "timestamp": chrono::Utc::now().timestamp_millis(),
                            });
                            emitter.emit_raw_or_warn("extraction-error", &error_event);
                        }
                    }
                }
                Err(e) => {
                    error!("Error reading extraction stdout: {}", e);
                    break;
                }
            }
        }

        info!("Extraction stdout reader task ending");

        // Mark as failed if not already in terminal state
        let lifecycle_guard = lifecycle.write().await;
        let state = lifecycle_guard.get_state().await;
        if !state.is_terminal() {
            let _ = lifecycle_guard
                .mark_failed("Extraction process stdout closed unexpectedly".to_string())
                .await;

            let error_event = json!({
                "type": "error",
                "error": "extraction_executor_crashed",
                "message": "Extraction executor process terminated unexpectedly",
            });
            emitter.emit_raw_or_error("extraction-error", &error_event);
        }
    }

    /// Stdout reader task for headless bridges.
    ///
    /// This reader sends events to a dedicated broadcast channel instead of
    /// emitting to the Tauri frontend. Used for parallel headless execution
    /// where events shouldn't interfere with GUI bridge events.
    pub async fn headless_stdout_reader_task(
        stdout: std::process::ChildStdout,
        lifecycle: Arc<RwLock<ExecutorLifecycle>>,
        health_monitor: Arc<HealthMonitor>,
        app_handle: tauri::AppHandle,
        headless_tx: broadcast::Sender<serde_json::Value>,
    ) {
        info!("[HEADLESS_BRIDGE] stdout_reader_task started");
        let reader = BufReader::new(stdout);
        let mut line_count = 0;

        for line in reader.lines() {
            line_count += 1;
            match line {
                Ok(line) => {
                    // Skip verbose logging for pong and heartbeat messages
                    let is_pong =
                        line.contains("\"type\": \"pong\"") || line.contains("\"type\":\"pong\"");
                    if !is_pong {
                        debug!("[HEADLESS_BRIDGE] Line #{}: {}", line_count, line);
                    }

                    // Parse message
                    match parse_executor_message(&line) {
                        Ok(message) => {
                            // Handle pong messages for health monitoring
                            if matches!(message, ExecutorMessage::Pong { .. }) {
                                health_monitor.record_pong().await;
                                continue;
                            }

                            // Start health monitoring when READY signal arrives
                            if matches!(&message, ExecutorMessage::Ready { .. }) {
                                info!("[HEADLESS_BRIDGE] Starting health monitoring after READY signal");
                                health_monitor.start().await;
                            }

                            debug!(
                                "[HEADLESS_BRIDGE] Parsed message type: {:?}",
                                std::mem::discriminant(&message)
                            );

                            // Process message through lifecycle
                            let mut lifecycle_guard = lifecycle.write().await;
                            match lifecycle_guard.handle_message(message.clone()).await {
                                Ok(Some(msg)) => {
                                    // Forward message to headless channel (not Tauri frontend)
                                    EventForwarder::emit_message_to_headless_channel(
                                        &app_handle,
                                        msg,
                                        &headless_tx,
                                    )
                                    .await;
                                }
                                Ok(None) => {
                                    debug!("Headless message handled internally");
                                }
                                Err(e) => {
                                    error!("Error handling headless message: {}", e);

                                    let error_event = json!({
                                        "type": "error",
                                        "error": "message_handling_error",
                                        "message": e.to_string(),
                                    });
                                    let _ = headless_tx.send(error_event);
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                "Failed to parse headless executor message: {} - Line: {}",
                                e, line
                            );

                            let error_event = json!({
                                "type": "error",
                                "error": "parse_error",
                                "message": format!("Failed to parse executor output: {}", e),
                                "raw_line": line,
                                "timestamp": chrono::Utc::now().timestamp_millis(),
                            });
                            let _ = headless_tx.send(error_event);
                        }
                    }
                }
                Err(e) => {
                    error!("Error reading headless stdout: {}", e);
                    break;
                }
            }
        }

        info!("Headless stdout reader task ending");

        // Mark as failed if not already in terminal state
        let lifecycle_guard = lifecycle.write().await;
        let state = lifecycle_guard.get_state().await;
        if !state.is_terminal() {
            let _ = lifecycle_guard
                .mark_failed("Headless Python process stdout closed unexpectedly".to_string())
                .await;

            let error_event = json!({
                "type": "error",
                "error": "headless_executor_crashed",
                "message": "Headless executor process terminated unexpectedly",
            });
            let _ = headless_tx.send(error_event);
        }
    }
}
