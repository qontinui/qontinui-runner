//! Intermediate types for the error monitoring pipeline.

use crate::error_monitor::types::{ErrorEvent, ErrorSeverity, LogFormat, ParserType};
use std::collections::HashMap;

/// Metadata about the source of a log record.
#[derive(Debug, Clone)]
pub struct SourceMeta {
    /// Parser type for this source.
    pub parser_type: ParserType,
    /// Log format.
    pub format: LogFormat,
    /// File path (if file-based).
    pub path: Option<String>,
}

/// Universal intermediate type flowing through the pipeline.
///
/// Receivers produce LogRecords; Processors transform them;
/// Exporters consume the final result.
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// Raw log line or content.
    pub raw: String,
    /// Name of the log source.
    pub source_name: String,
    /// Source metadata.
    pub source_meta: SourceMeta,
    /// Parsed timestamp from the log entry.
    pub timestamp: Option<String>,
    /// Detected severity.
    pub severity: Option<ErrorSeverity>,
    /// Fully parsed error event (populated by ParserProcessor).
    pub parsed: Option<ErrorEvent>,
    /// Arbitrary metadata (trace_id, labels, etc.).
    pub metadata: HashMap<String, String>,
}

impl LogRecord {
    /// Create a new LogRecord from raw content.
    pub fn new(raw: String, source_name: String, source_meta: SourceMeta) -> Self {
        Self {
            raw,
            source_name,
            source_meta,
            timestamp: None,
            severity: None,
            parsed: None,
            metadata: HashMap::new(),
        }
    }
}

impl From<ErrorEvent> for LogRecord {
    fn from(event: ErrorEvent) -> Self {
        let source_name = event.log_source_name.clone();
        Self {
            raw: event.raw_entry.clone(),
            source_name,
            source_meta: SourceMeta {
                parser_type: ParserType::Generic,
                format: LogFormat::Plaintext,
                path: None,
            },
            timestamp: event.log_timestamp.clone(),
            severity: Some(event.severity.clone()),
            parsed: Some(event),
            metadata: HashMap::new(),
        }
    }
}
