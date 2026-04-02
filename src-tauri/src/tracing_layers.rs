//! Custom tracing subscriber layers for span instrumentation.
//!
//! This module provides two layers:
//! 1. `JsonlSpanLayer` - Writes all spans to runner-spans.jsonl for real-time debugging
//! 2. `SqliteSpanLayer` - Persists summary spans to the database for AI analysis
//!
//! # Architecture
//!
//! ```text
//!                           ┌─────────────────────────────────┐
//!                           │     Tracing Subscriber          │
//!                           │  (tracing-subscriber Registry)  │
//!                           └─────────────┬───────────────────┘
//!                                         │
//!               ┌─────────────────────────┼─────────────────────────┐
//!               │                         │                         │
//!               ▼                         ▼                         ▼
//!     ┌─────────────────┐     ┌─────────────────────┐    ┌──────────────────┐
//!     │  Console/File   │     │   JSONL Layer       │    │  SQLite Layer    │
//!     │  (existing)     │     │   (all spans)       │    │  (summary only)  │
//!     └─────────────────┘     └─────────────────────┘    └──────────────────┘
//!            │                         │                         │
//!            ▼                         ▼                         ▼
//!     qontinui-runner.log    runner-spans.jsonl         execution_spans table
//!     (text, rolling)        (cleared per run)          (persistent)
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

// ============================================================================
// Types and Configuration
// ============================================================================

/// Configuration for span layers
#[derive(Debug, Clone)]
pub struct SpanLayerConfig {
    /// Directory for JSONL output
    pub dev_logs_dir: PathBuf,
    /// Whether to enable JSONL output
    pub enable_jsonl: bool,
    /// Whether to enable SQLite output
    pub enable_sqlite: bool,
    /// Span names to persist to SQLite (summary spans only)
    pub summary_span_names: Vec<String>,
}

impl Default for SpanLayerConfig {
    fn default() -> Self {
        use crate::semantic_conventions::spans;

        let names: Vec<String> = spans::SUMMARY_SPANS.iter().map(|s| s.to_string()).collect();

        Self {
            dev_logs_dir: crate::paths::get_dev_logs_dir(),
            enable_jsonl: true,
            enable_sqlite: true,
            summary_span_names: names,
        }
    }
}

/// A completed span entry for serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanEntry {
    /// Unique trace ID (shared across related spans)
    pub trace_id: String,
    /// Unique span ID
    pub span_id: String,
    /// Parent span ID (None for root spans)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    /// Span name (e.g., "workflow.execute")
    pub name: String,
    /// Target module
    pub target: String,
    /// Log level
    pub level: String,
    /// Start timestamp (ISO 8601)
    pub start_ts: String,
    /// End timestamp (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ts: Option<String>,
    /// Duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Span attributes/fields
    pub attributes: HashMap<String, serde_json::Value>,
    /// Whether the span completed successfully (no error event recorded)
    pub success: bool,
    /// Error message if the span failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Internal tracking data for an active span
#[derive(Debug, Clone)]
struct ActiveSpanData {
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    name: String,
    target: String,
    level: String,
    start_time: DateTime<Utc>,
    attributes: HashMap<String, serde_json::Value>,
    had_error: bool,
    error_message: Option<String>,
}

// ============================================================================
// JSONL Span Layer
// ============================================================================

/// Layer that writes all span events to a JSONL file for real-time debugging.
///
/// The JSONL file is cleared on runner startup and provides a complete
/// trace of all spans during the current execution.
pub struct JsonlSpanLayer {
    config: SpanLayerConfig,
    /// Track active spans by their ID
    active_spans: Arc<RwLock<HashMap<u64, ActiveSpanData>>>,
    /// File handle (lazily opened, mutex for thread safety)
    file: Arc<Mutex<Option<File>>>,
}

impl JsonlSpanLayer {
    /// Create a new JSONL span layer
    pub fn new(config: SpanLayerConfig) -> Self {
        Self {
            config,
            active_spans: Arc::new(RwLock::new(HashMap::new())),
            file: Arc::new(Mutex::new(None)),
        }
    }

    /// Get or create the output file.
    ///
    /// Resolves the dev_logs_dir at open time (not at construction time) by checking
    /// the settings override directly. This avoids the OnceLock in `get_dev_logs_dir()`
    /// which may have cached the default path before settings were loaded.
    fn get_file(&self) -> Option<std::sync::MutexGuard<'_, Option<File>>> {
        let mut file_guard = self.file.lock().ok()?;
        if file_guard.is_none() {
            // Check settings override directly to bypass the OnceLock cache
            let dev_logs_dir = match crate::settings::get_dev_logs_dir_override() {
                Some(override_path) => PathBuf::from(override_path),
                None => self.config.dev_logs_dir.clone(),
            };
            let path = dev_logs_dir.join("runner-spans.jsonl");
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(f) => *file_guard = Some(f),
                Err(e) => {
                    eprintln!(
                        "Failed to open spans JSONL file at {}: {}",
                        path.display(),
                        e
                    );
                    return None;
                }
            }
        }
        Some(file_guard)
    }

    /// Write a span entry to the JSONL file
    fn write_span(&self, entry: &SpanEntry) {
        if let Some(mut file_guard) = self.get_file() {
            if let Some(ref mut file) = *file_guard {
                if let Ok(json) = serde_json::to_string(entry) {
                    let _ = writeln!(file, "{}", json);
                }
            }
        }
    }

    /// Generate a trace ID (shared within a trace tree)
    fn generate_trace_id() -> String {
        uuid::Uuid::new_v4().to_string()[..8].to_string()
    }

    /// Generate a span ID
    fn generate_span_id() -> String {
        uuid::Uuid::new_v4().to_string()[..12].to_string()
    }
}

impl<S> Layer<S> for JsonlSpanLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if !self.config.enable_jsonl {
            return;
        }

        let span = ctx.span(id).expect("span should exist");
        let metadata = span.metadata();

        // Determine parent span ID and trace ID
        let (parent_span_id, trace_id) = if let Some(parent) = span.parent() {
            let parent_id = parent.id();
            if let Ok(spans) = self.active_spans.read() {
                if let Some(parent_data) = spans.get(&parent_id.into_u64()) {
                    (
                        Some(parent_data.span_id.clone()),
                        parent_data.trace_id.clone(),
                    )
                } else {
                    (None, Self::generate_trace_id())
                }
            } else {
                (None, Self::generate_trace_id())
            }
        } else {
            (None, Self::generate_trace_id())
        };

        // Collect attributes
        let mut attributes = HashMap::new();
        let mut visitor = AttributeVisitor(&mut attributes);
        attrs.record(&mut visitor);

        let span_data = ActiveSpanData {
            trace_id,
            span_id: Self::generate_span_id(),
            parent_span_id,
            name: metadata.name().to_string(),
            target: metadata.target().to_string(),
            level: format!("{:?}", metadata.level()),
            start_time: Utc::now(),
            attributes,
            had_error: false,
            error_message: None,
        };

        if let Ok(mut spans) = self.active_spans.write() {
            spans.insert(id.into_u64(), span_data);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        if !self.config.enable_jsonl {
            return;
        }

        if let Ok(mut spans) = self.active_spans.write() {
            if let Some(span_data) = spans.get_mut(&id.into_u64()) {
                let mut visitor = AttributeVisitor(&mut span_data.attributes);
                values.record(&mut visitor);
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if !self.config.enable_jsonl {
            return;
        }

        // Check if this is an error event
        let level = event.metadata().level();
        if *level == tracing::Level::ERROR {
            // Find the current span and mark it as having an error
            if let Some(span) = ctx.lookup_current() {
                if let Ok(mut spans) = self.active_spans.write() {
                    if let Some(span_data) = spans.get_mut(&span.id().into_u64()) {
                        span_data.had_error = true;
                        // Try to extract the error message
                        let mut message = String::new();
                        let mut visitor = MessageVisitor(&mut message);
                        event.record(&mut visitor);
                        if !message.is_empty() {
                            span_data.error_message = Some(message);
                        }
                    }
                }
            }
        }
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        if !self.config.enable_jsonl {
            return;
        }

        let span_data = {
            if let Ok(mut spans) = self.active_spans.write() {
                spans.remove(&id.into_u64())
            } else {
                None
            }
        };

        if let Some(data) = span_data {
            let end_time = Utc::now();
            let duration = end_time
                .signed_duration_since(data.start_time)
                .num_milliseconds().max(0) as u64;

            let entry = SpanEntry {
                trace_id: data.trace_id,
                span_id: data.span_id,
                parent_span_id: data.parent_span_id,
                name: data.name,
                target: data.target,
                level: data.level,
                start_ts: data.start_time.to_rfc3339(),
                end_ts: Some(end_time.to_rfc3339()),
                duration_ms: Some(duration),
                attributes: data.attributes,
                success: !data.had_error,
                error: data.error_message,
            };

            self.write_span(&entry);
        }
    }
}

// ============================================================================
// SQLite Span Layer (stub -- SQLite removed, PG span persistence TBD)
// ============================================================================

/// No-op span layer stub. Previously persisted spans to SQLite; the SQLite
/// backend has been removed. Retains the public API so callers compile.
pub struct SqliteSpanLayer {
    config: SpanLayerConfig,
    /// Track active spans by their ID (unused, kept for API compat)
    active_spans: Arc<RwLock<HashMap<u64, ActiveSpanData>>>,
}

impl SqliteSpanLayer {
    /// Create a new (no-op) span layer
    pub fn new(config: SpanLayerConfig) -> Self {
        Self {
            config,
            active_spans: Arc::new(RwLock::new(HashMap::new())),
        }
    }

}

// No-op Layer implementation for SqliteSpanLayer (SQLite removed)
impl<S> Layer<S> for SqliteSpanLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    // All methods use default no-op implementations
}

// ============================================================================
// Visitor Helpers
// ============================================================================

/// Visitor for collecting span attributes
struct AttributeVisitor<'a>(&'a mut HashMap<String, serde_json::Value>);

impl<'a> tracing::field::Visit for AttributeVisitor<'a> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(
            field.name().to_string(),
            serde_json::Value::String(format!("{:?}", value)),
        );
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0
            .insert(field.name().to_string(), serde_json::Value::Bool(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if let Some(n) = serde_json::Number::from_f64(value) {
            self.0
                .insert(field.name().to_string(), serde_json::Value::Number(n));
        }
    }
}

/// Visitor for extracting error messages from events
struct MessageVisitor<'a>(&'a mut String);

impl<'a> tracing::field::Visit for MessageVisitor<'a> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            *self.0 = format!("{:?}", value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            *self.0 = value.to_string();
        }
    }
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Clear the spans JSONL file (called on startup).
///
/// Clears from both the default and override paths to handle the case where
/// the file was previously written to a different location.
pub fn clear_spans_jsonl() {
    // Clear from default path
    let default_path = crate::paths::get_dev_logs_dir().join("runner-spans.jsonl");
    if default_path.exists() {
        if let Err(e) = std::fs::remove_file(&default_path) {
            eprintln!("Failed to clear spans JSONL (default): {}", e);
        }
    }

    // Also clear from override path if different
    if let Some(override_dir) = crate::settings::get_dev_logs_dir_override() {
        let override_path = PathBuf::from(override_dir).join("runner-spans.jsonl");
        if override_path != default_path && override_path.exists() {
            if let Err(e) = std::fs::remove_file(&override_path) {
                eprintln!("Failed to clear spans JSONL (override): {}", e);
            }
        }
    }
}

/// Get the path to the spans JSONL file
pub fn get_spans_jsonl_path() -> PathBuf {
    match crate::settings::get_dev_logs_dir_override() {
        Some(override_dir) => PathBuf::from(override_dir).join("runner-spans.jsonl"),
        None => crate::paths::get_dev_logs_dir().join("runner-spans.jsonl"),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_entry_serialization() {
        let entry = SpanEntry {
            trace_id: "abc12345".to_string(),
            span_id: "def456789012".to_string(),
            parent_span_id: None,
            name: "test.span".to_string(),
            target: "test_module".to_string(),
            level: "INFO".to_string(),
            start_ts: "2024-01-01T00:00:00Z".to_string(),
            end_ts: Some("2024-01-01T00:00:01Z".to_string()),
            duration_ms: Some(1000),
            attributes: HashMap::new(),
            success: true,
            error: None,
        };

        let json = serde_json::to_string(&entry).expect("serialization should succeed");
        assert!(json.contains("test.span"));
        assert!(json.contains("abc12345"));
    }

    #[test]
    fn test_config_default_summary_spans() {
        let config = SpanLayerConfig::default();
        assert!(config
            .summary_span_names
            .contains(&"qontinui.workflow".to_string()));
        assert!(config
            .summary_span_names
            .contains(&"qontinui.ai.session".to_string()));
    }
}
