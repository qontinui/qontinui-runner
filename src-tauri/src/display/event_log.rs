use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Raw event from the executor
///
/// This struct stores events exactly as received from the library/executor
/// with NO filtering or processing. All filtering is done by DisplayProfiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvent {
    /// Unique event ID
    pub id: String,

    /// Event type (action_started, action_completed, workflow_started, etc.)
    pub event_type: String,

    /// Event timestamp (Unix epoch in seconds)
    pub timestamp: f64,

    /// Event data (flexible JSON structure - stores everything)
    pub data: serde_json::Value,

    /// Sequence number for ordering
    pub sequence: u64,
}

/// Storage for raw execution events
///
/// IMPORTANT: This stores ALL events without any filtering.
/// No events are excluded at this level - all filtering happens
/// in DisplayProfiles based on their specific requirements.
pub struct EventLog {
    /// All events in chronological order (NO FILTERING)
    events: Vec<RawEvent>,

    /// Index by event type for fast filtering
    type_index: HashMap<String, Vec<usize>>,

    /// Index by node ID for fast node lookup
    node_index: HashMap<String, Vec<usize>>,
}

impl EventLog {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            type_index: HashMap::new(),
            node_index: HashMap::new(),
        }
    }

    /// Add a new event to the log
    pub fn add_event(&mut self, event: RawEvent) {
        eprintln!(
            "[EVENT_LOG] add_event called: id={}, type={}, sequence={}",
            event.id, event.event_type, event.sequence
        );
        eprintln!(
            "[EVENT_LOG] Event data keys: {:?}",
            event.data.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );

        let event_idx = self.events.len();

        // Add to type index
        self.type_index
            .entry(event.event_type.clone())
            .or_default()
            .push(event_idx);

        // Add to node index if event has a node_id
        if let Some(node_id) = event
            .data
            .get("node")
            .and_then(|n| n.get("id"))
            .and_then(|id| id.as_str())
        {
            eprintln!("[EVENT_LOG] Found node_id in event: {}", node_id);
            self.node_index
                .entry(node_id.to_string())
                .or_default()
                .push(event_idx);
        } else {
            eprintln!("[EVENT_LOG] No node_id found in event data");
        }

        self.events.push(event);
        eprintln!(
            "[EVENT_LOG] Event added. Total events now: {}",
            self.events.len()
        );
    }

    /// Get all events
    pub fn events(&self) -> &[RawEvent] {
        &self.events
    }

    /// Get events by type
    #[allow(dead_code)]
    pub fn events_by_type(&self, event_type: &str) -> Vec<&RawEvent> {
        self.type_index
            .get(event_type)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&idx| self.events.get(idx))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get events for a specific node
    #[allow(dead_code)]
    pub fn events_for_node(&self, node_id: &str) -> Vec<&RawEvent> {
        self.node_index
            .get(node_id)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&idx| self.events.get(idx))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Clear all events
    pub fn clear(&mut self) {
        self.events.clear();
        self.type_index.clear();
        self.node_index.clear();
    }

    /// Get event count
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Check if log is empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl Default for EventLog {
    fn default() -> Self {
        Self::new()
    }
}
