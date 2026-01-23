use super::file_logger::FileLogger;
use super::lifecycle::ExecutorMessage;
use super::protocol::ExecutorEvent;
use crate::commands::AppState;
use crate::display::RawEvent;
use crate::safe_eprintln;
use serde_json::json;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tracing::{debug, error, info, warn};

/// Handles event forwarding from Python executor to Tauri frontend and WebSocket clients
pub struct EventForwarder;

impl EventForwarder {
    /// Broadcasts an event to all connected WebSocket clients
    fn broadcast_to_websocket(app_handle: &tauri::AppHandle, event: &serde_json::Value) {
        if let Some(app_state) = app_handle.try_state::<Arc<AppState>>() {
            // Send to broadcast channel - ignore errors (no receivers is ok)
            if let Err(e) = app_state.event_broadcast.send(event.clone()) {
                // Only warn if there are supposed to be receivers
                // receiver_count() > 0 but send failed means something is wrong
                if app_state.event_broadcast.receiver_count() > 0 {
                    warn!("Failed to broadcast event to WebSocket clients: {}", e);
                }
            }
        }
    }

    /// Emits a message to the Tauri frontend, WebSocket clients, and feeds events to DisplayProcessor
    pub async fn emit_message_to_frontend(app_handle: &tauri::AppHandle, message: ExecutorMessage) {
        // Feed tree events to DisplayProcessor and RunRecordingHandler
        if let ExecutorMessage::TreeEvent {
            ref event_type,
            ref node,
            ref timestamp,
            ref sequence,
            ..
        } = message
        {
            safe_eprintln!(
                "[PYTHON_BRIDGE] Received TreeEvent: type={}, seq={}",
                event_type,
                sequence
            );
            safe_eprintln!("[PYTHON_BRIDGE] Node data: {:?}", node);

            // Get the AppState and add event to DisplayProcessor
            if let Some(app_state) = app_handle.try_state::<std::sync::Arc<AppState>>() {
                safe_eprintln!("[PYTHON_BRIDGE] AppState found, creating RawEvent");

                let raw_event = RawEvent {
                    id: uuid::Uuid::new_v4().to_string(),
                    event_type: event_type.clone(),
                    timestamp: *timestamp,
                    data: json!({
                        "node": node.clone(),
                    }),
                    sequence: *sequence as u64,
                };

                safe_eprintln!(
                    "[PYTHON_BRIDGE] RawEvent created: id={}, type={}",
                    raw_event.id,
                    raw_event.event_type
                );

                let mut processor = app_state.display_processor.lock().await;
                safe_eprintln!("[PYTHON_BRIDGE] Got display_processor lock, calling add_event");
                processor.event_log_mut().add_event(raw_event);
                safe_eprintln!("[PYTHON_BRIDGE] add_event completed");

                // Feed tree event to run recording handler
                app_state
                    .run_recording_handler
                    .on_tree_event(event_type, node)
                    .await;
            } else {
                safe_eprintln!("[PYTHON_BRIDGE] AppState NOT available!");
                debug!("AppState not available for feeding events to DisplayProcessor");
            }
        }

        // Feed execution_completed events to RunRecordingHandler
        if let ExecutorMessage::Event {
            ref event,
            ref data,
            ..
        } = message
        {
            if event == "execution_completed" {
                if let Some(app_state) = app_handle.try_state::<std::sync::Arc<AppState>>() {
                    info!("[RUN_RECORDING] Processing execution_completed event");
                    app_state
                        .run_recording_handler
                        .on_execution_completed(data)
                        .await;
                }
            }
        }

        // Feed image recognition events to RunRecordingHandler
        if let ExecutorMessage::ImageRecognition { ref data } = message {
            if let Some(app_state) = app_handle.try_state::<std::sync::Arc<AppState>>() {
                app_state
                    .run_recording_handler
                    .on_image_recognition(data)
                    .await;
            }
        }

        // Continue with normal event emission
        match message {
            ExecutorMessage::Event {
                event,
                data,
                timestamp,
                sequence,
            } => {
                // Log to file
                FileLogger::log_general_event(&event, &data, timestamp);

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
            ExecutorMessage::TreeEvent {
                event_type,
                node,
                path,
                timestamp,
                sequence,
            } => {
                // Log to file (action logs)
                FileLogger::log_tree_event(&event_type, &node, &path, timestamp, sequence);

                // Emit tree event with all data
                let tree_event = json!({
                    "type": "tree_event",
                    "event_type": event_type,
                    "node": node,
                    "path": path,
                    "timestamp": timestamp,
                    "sequence": sequence,
                });

                // Broadcast to WebSocket clients for live perception
                Self::broadcast_to_websocket(app_handle, &tree_event);

                if let Err(e) = app_handle.emit("executor-event", &tree_event) {
                    error!("Failed to emit tree event: {}", e);
                }
            }
            ExecutorMessage::Response {
                id,
                success,
                data,
                error,
            } => {
                let response = super::protocol::ExecutorResponse {
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
                // Log to file
                FileLogger::log_error(&message, details.as_deref());

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
            ExecutorMessage::ImageRecognition { data } => {
                // Log to file (with screenshot saving)
                FileLogger::log_image_recognition(&data);

                // Emit image recognition event with debug data
                // Use "event" field to match frontend's expectation (data.event === "image_recognition")
                let image_recognition_event = json!({
                    "event": "image_recognition",
                    "data": data,
                });

                // Broadcast to WebSocket clients for live perception
                // This includes found coordinates for displaying StateImages at runtime positions
                Self::broadcast_to_websocket(app_handle, &image_recognition_event);

                if let Err(e) = app_handle.emit("executor-event", &image_recognition_event) {
                    error!("Failed to emit image recognition event: {}", e);
                }
            }
            _ => {
                debug!("Unhandled message type: {:?}", message);
            }
        }
    }

    /// Emits an extraction message to the Tauri frontend.
    ///
    /// This is a simplified version of emit_message_to_frontend for the extraction executor.
    /// It emits to "extraction-event" instead of "executor-event" and doesn't process
    /// tree events or display processor updates since extraction doesn't use GUI automation.
    pub async fn emit_extraction_message_to_frontend(
        app_handle: &tauri::AppHandle,
        message: ExecutorMessage,
    ) {
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
                if let Err(e) = app_handle.emit("extraction-event", &event_obj) {
                    error!("Failed to emit extraction event: {}", e);
                }
            }
            ExecutorMessage::Response {
                id,
                success,
                data,
                error,
            } => {
                let response = super::protocol::ExecutorResponse {
                    resp_type: "response".to_string(),
                    id,
                    success,
                    data,
                    error,
                };
                if let Err(e) = app_handle.emit("extraction-response", &response) {
                    error!("Failed to emit extraction response: {}", e);
                }
            }
            ExecutorMessage::Error { message, details } => {
                let error_event = json!({
                    "type": "error",
                    "error": "extraction_error",
                    "message": message,
                    "details": details,
                });
                if let Err(e) = app_handle.emit("extraction-error", &error_event) {
                    error!("Failed to emit extraction error: {}", e);
                }
            }
            _ => {
                debug!("Unhandled extraction message type: {:?}", message);
            }
        }
    }
}
