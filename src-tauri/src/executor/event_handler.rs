use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorEvent {
    pub event: String,
    pub timestamp: f64,
    pub data: Value,
}

// EventHandler removed as it was unused
// If needed in the future, event handling is done directly through Tauri's event system
