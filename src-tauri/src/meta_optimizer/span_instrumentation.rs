//! Span instrumentation for granular per-step event collection.
//!
//! Provides composable `emit_*()` functions inspired by agent-lightning's span model.
//! Events are collected during pipeline agent execution and persisted alongside
//! the monolithic `PipelineAgentTrace` for richer optimization data.

use serde::{Deserialize, Serialize};

/// A single span event emitted during agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum SpanEvent {
    /// Scalar reward signal at a specific step.
    Reward {
        value: f64,
        metric: String,
        step_index: usize,
    },
    /// Structured data snapshot (input, output, config, etc.).
    Object {
        key: String,
        data: serde_json::Value,
        step_index: usize,
    },
    /// LLM message (prompt or response).
    Message {
        role: String,
        content: String,
        step_index: usize,
    },
}

impl SpanEvent {
    /// Returns the event type as a string for DB storage.
    pub fn event_type_str(&self) -> &'static str {
        match self {
            SpanEvent::Reward { .. } => "reward",
            SpanEvent::Object { .. } => "object",
            SpanEvent::Message { .. } => "message",
        }
    }

    /// Returns the step index for this event.
    pub fn step_index(&self) -> usize {
        match self {
            SpanEvent::Reward { step_index, .. }
            | SpanEvent::Object { step_index, .. }
            | SpanEvent::Message { step_index, .. } => *step_index,
        }
    }
}

/// Collects span events during a single agent's execution.
///
/// Thread through pipeline agent execution, call `emit_*()` methods,
/// then `finalize()` to get all events for batch persistence.
#[derive(Debug, Clone)]
pub struct SpanCollector {
    pub execution_id: String,
    pub agent_type: String,
    events: Vec<SpanEvent>,
    started_at: chrono::DateTime<chrono::Utc>,
}

impl SpanCollector {
    /// Create a new collector for an agent execution.
    pub fn new(execution_id: &str, agent_type: &str) -> Self {
        Self {
            execution_id: execution_id.to_string(),
            agent_type: agent_type.to_string(),
            events: Vec::new(),
            started_at: chrono::Utc::now(),
        }
    }

    /// Emit a scalar reward signal.
    pub fn emit_reward(&mut self, value: f64, metric: &str, step_index: usize) {
        self.events.push(SpanEvent::Reward {
            value,
            metric: metric.to_string(),
            step_index,
        });
    }

    /// Emit a structured data snapshot.
    pub fn emit_object(&mut self, key: &str, data: serde_json::Value, step_index: usize) {
        self.events.push(SpanEvent::Object {
            key: key.to_string(),
            data,
            step_index,
        });
    }

    /// Emit an LLM message (prompt or response).
    pub fn emit_message(&mut self, role: &str, content: &str, step_index: usize) {
        self.events.push(SpanEvent::Message {
            role: role.to_string(),
            content: content.to_string(),
            step_index,
        });
    }

    /// Total events collected so far.
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// When collection started.
    pub fn started_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.started_at
    }

    /// Consume the collector and return all events, sorted by step_index.
    pub fn finalize(mut self) -> Vec<SpanEvent> {
        self.events.sort_by_key(|e| e.step_index());
        self.events
    }
}

/// Row structure for persisting span events to PostgreSQL.
#[derive(Debug, Clone)]
pub struct SpanEventRow {
    pub id: String,
    pub execution_id: String,
    pub trace_id: String,
    pub agent_type: String,
    pub event_type: String,
    pub step_index: i32,
    pub metric_name: Option<String>,
    pub reward_value: Option<f64>,
    pub data_key: Option<String>,
    pub data_json: Option<String>,
    pub role: Option<String>,
    pub content: Option<String>,
}

impl SpanEventRow {
    /// Convert a persisted row back into a SpanEvent.
    ///
    /// Returns `None` if the row's event_type is unrecognized or required
    /// fields are missing (e.g. a reward row without `reward_value`).
    pub fn to_span_event(&self) -> Option<SpanEvent> {
        let step = self.step_index as usize;
        match self.event_type.as_str() {
            "reward" => Some(SpanEvent::Reward {
                value: self.reward_value?,
                metric: self.metric_name.clone().unwrap_or_default(),
                step_index: step,
            }),
            "object" => Some(SpanEvent::Object {
                key: self.data_key.clone().unwrap_or_default(),
                data: self
                    .data_json
                    .as_deref()
                    .and_then(|j| serde_json::from_str(j).ok())
                    .unwrap_or(serde_json::Value::Null),
                step_index: step,
            }),
            "message" => Some(SpanEvent::Message {
                role: self.role.clone().unwrap_or_default(),
                content: self.content.clone().unwrap_or_default(),
                step_index: step,
            }),
            _ => None,
        }
    }

    /// Convert a SpanEvent into a row for persistence.
    pub fn from_event(
        event: &SpanEvent,
        execution_id: &str,
        trace_id: &str,
        agent_type: &str,
    ) -> Self {
        let id = format!("se-{}", uuid::Uuid::new_v4());
        match event {
            SpanEvent::Reward {
                value,
                metric,
                step_index,
            } => Self {
                id,
                execution_id: execution_id.to_string(),
                trace_id: trace_id.to_string(),
                agent_type: agent_type.to_string(),
                event_type: "reward".to_string(),
                step_index: *step_index as i32,
                metric_name: Some(metric.clone()),
                reward_value: Some(*value),
                data_key: None,
                data_json: None,
                role: None,
                content: None,
            },
            SpanEvent::Object {
                key,
                data,
                step_index,
            } => Self {
                id,
                execution_id: execution_id.to_string(),
                trace_id: trace_id.to_string(),
                agent_type: agent_type.to_string(),
                event_type: "object".to_string(),
                step_index: *step_index as i32,
                metric_name: None,
                reward_value: None,
                data_key: Some(key.clone()),
                data_json: Some(data.to_string()),
                role: None,
                content: None,
            },
            SpanEvent::Message {
                role,
                content,
                step_index,
            } => Self {
                id,
                execution_id: execution_id.to_string(),
                trace_id: trace_id.to_string(),
                agent_type: agent_type.to_string(),
                event_type: "message".to_string(),
                step_index: *step_index as i32,
                metric_name: None,
                reward_value: None,
                data_key: None,
                data_json: None,
                role: Some(role.clone()),
                content: Some(content.clone()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_collector_emit_and_finalize() {
        let mut collector = SpanCollector::new("exec-1", "implementer");

        // Emit events out of order
        collector.emit_reward(0.85, "accuracy", 2);
        collector.emit_message("user", "Fix the bug", 0);
        collector.emit_object("config", serde_json::json!({"model": "claude"}), 1);
        collector.emit_reward(0.92, "completeness", 3);
        collector.emit_message("assistant", "Done", 4);

        assert_eq!(collector.event_count(), 5);

        let events = collector.finalize();
        assert_eq!(events.len(), 5);

        // Verify sorted by step_index
        let indices: Vec<usize> = events.iter().map(|e| e.step_index()).collect();
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);

        // Verify event types
        assert_eq!(events[0].event_type_str(), "message");
        assert_eq!(events[1].event_type_str(), "object");
        assert_eq!(events[2].event_type_str(), "reward");
        assert_eq!(events[3].event_type_str(), "reward");
        assert_eq!(events[4].event_type_str(), "message");
    }

    #[test]
    fn test_span_collector_empty() {
        let collector = SpanCollector::new("exec-2", "verifier");
        assert_eq!(collector.event_count(), 0);
        let events = collector.finalize();
        assert!(events.is_empty());
    }

    #[test]
    fn test_span_event_row_from_reward() {
        let event = SpanEvent::Reward {
            value: 0.9,
            metric: "accuracy".to_string(),
            step_index: 3,
        };
        let row = SpanEventRow::from_event(&event, "exec-1", "trace-1", "implementer");
        assert_eq!(row.event_type, "reward");
        assert_eq!(row.step_index, 3);
        assert_eq!(row.reward_value, Some(0.9));
        assert_eq!(row.metric_name, Some("accuracy".to_string()));
        assert!(row.data_key.is_none());
    }

    #[test]
    fn test_span_event_row_from_object() {
        let data = serde_json::json!({"key": "value"});
        let event = SpanEvent::Object {
            key: "output".to_string(),
            data: data.clone(),
            step_index: 1,
        };
        let row = SpanEventRow::from_event(&event, "exec-1", "trace-1", "locator");
        assert_eq!(row.event_type, "object");
        assert_eq!(row.data_key, Some("output".to_string()));
        assert!(row.data_json.is_some());
        assert!(row.reward_value.is_none());
    }

    #[test]
    fn test_span_event_row_from_message() {
        let event = SpanEvent::Message {
            role: "assistant".to_string(),
            content: "Response text".to_string(),
            step_index: 5,
        };
        let row = SpanEventRow::from_event(&event, "exec-1", "trace-1", "verifier");
        assert_eq!(row.event_type, "message");
        assert_eq!(row.role, Some("assistant".to_string()));
        assert_eq!(row.content, Some("Response text".to_string()));
    }

    #[test]
    fn test_span_event_serialization() {
        let event = SpanEvent::Reward {
            value: 0.75,
            metric: "task_completion".to_string(),
            step_index: 0,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("reward"));
        assert!(json.contains("0.75"));

        let deserialized: SpanEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type_str(), "reward");
        assert_eq!(deserialized.step_index(), 0);
    }
}
