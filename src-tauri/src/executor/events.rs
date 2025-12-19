use super::file_logger::FileLogger;
use super::lifecycle::ExecutorMessage;
use super::protocol::ExecutorEvent;
use crate::commands::AppState;
use crate::display::RawEvent;
use serde_json::json;
use tauri::{Emitter, Manager};
use tracing::{debug, error};

/// Handles event forwarding from Python executor to Tauri frontend
pub struct EventForwarder;

impl EventForwarder {
    /// Emits a message to the Tauri frontend and feeds events to DisplayProcessor
    pub async fn emit_message_to_frontend(app_handle: &tauri::AppHandle, message: ExecutorMessage) {
        // Feed tree events to DisplayProcessor
        if let ExecutorMessage::TreeEvent {
            ref event_type,
            ref node,
            ref timestamp,
            ref sequence,
            ..
        } = message
        {
            eprintln!(
                "[PYTHON_BRIDGE] Received TreeEvent: type={}, seq={}",
                event_type, sequence
            );
            eprintln!("[PYTHON_BRIDGE] Node data: {:?}", node);

            // Get the AppState and add event to DisplayProcessor
            if let Some(app_state) = app_handle.try_state::<std::sync::Arc<AppState>>() {
                eprintln!("[PYTHON_BRIDGE] AppState found, creating RawEvent");

                let raw_event = RawEvent {
                    id: uuid::Uuid::new_v4().to_string(),
                    event_type: event_type.clone(),
                    timestamp: *timestamp,
                    data: json!({
                        "node": node.clone(),
                    }),
                    sequence: *sequence as u64,
                };

                eprintln!(
                    "[PYTHON_BRIDGE] RawEvent created: id={}, type={}",
                    raw_event.id, raw_event.event_type
                );

                let mut processor = app_state.display_processor.lock().await;
                eprintln!("[PYTHON_BRIDGE] Got display_processor lock, calling add_event");
                processor.event_log_mut().add_event(raw_event);
                eprintln!("[PYTHON_BRIDGE] add_event completed");
            } else {
                eprintln!("[PYTHON_BRIDGE] AppState NOT available!");
                debug!("AppState not available for feeding events to DisplayProcessor");
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
                if let Err(e) = app_handle.emit("executor-event", &image_recognition_event) {
                    error!("Failed to emit image recognition event: {}", e);
                }
            }
            _ => {
                debug!("Unhandled message type: {:?}", message);
            }
        }
    }
}
