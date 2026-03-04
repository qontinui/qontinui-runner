//! JSONL format preprocessor.
//!
//! For JSONL log sources, parses JSON and extracts error information,
//! filtering out success events.

use crate::error_monitor::pipeline::traits::Processor;
use crate::error_monitor::pipeline::types::LogRecord;
use crate::error_monitor::types::LogFormat;
use async_trait::async_trait;

/// Preprocesses JSONL log records, filtering out success events
/// and extracting error messages.
pub struct JsonlPreprocessor;

#[async_trait]
impl Processor for JsonlPreprocessor {
    fn name(&self) -> &str {
        "jsonl_preprocess"
    }

    async fn process(&self, records: Vec<LogRecord>) -> Vec<LogRecord> {
        records
            .into_iter()
            .filter_map(|mut record| {
                // Only process JSONL format records
                if record.source_meta.format != LogFormat::Jsonl {
                    return Some(record);
                }

                // Try to parse as JSON
                let json: serde_json::Value = match serde_json::from_str(&record.raw) {
                    Ok(v) => v,
                    Err(_) => return Some(record), // Pass through malformed lines
                };

                // Skip spec verification events
                if is_spec_verification_event(&json) {
                    return None;
                }

                // Check for error indicators
                if let Some(error_msg) = extract_error_message(&json) {
                    record.raw = format!("ERROR: {}", error_msg);
                    Some(record)
                } else {
                    None // Success event, filter out
                }
            })
            .collect()
    }
}

/// Check if a JSONL event is a spec verification event.
fn is_spec_verification_event(json: &serde_json::Value) -> bool {
    let event_type = json
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if event_type != "action_failed" {
        return false;
    }

    // Check various locations for "SPEC: " prefix
    for name_path in &[
        json.get("name"),
        json.get("node").and_then(|n| n.get("name")),
    ] {
        if let Some(Some(name)) = name_path.map(|v| v.as_str()) {
            if name.starts_with("SPEC: ") {
                return true;
            }
        }
    }

    false
}

/// Extract error message from a JSON event, returning None for success events.
fn extract_error_message(json: &serde_json::Value) -> Option<String> {
    let success_statuses = [
        "success",
        "completed",
        "pending",
        "running",
        "started",
        "ok",
    ];

    // Find status at various nesting levels
    let status = find_status(json);

    if let Some(ref status) = status {
        let lower = status.to_lowercase();
        if success_statuses.iter().any(|s| lower == *s) {
            return None;
        }
        if lower == "failed" || lower == "error" || lower == "failure" {
            return Some(build_error_message(json, status));
        }
    }

    // Check explicit error fields
    if let Some(error) = json.get("error") {
        if let Some(msg) = error.as_str() {
            return Some(msg.to_string());
        }
        if let Some(msg) = error.get("message").and_then(|v| v.as_str()) {
            return Some(msg.to_string());
        }
    }

    if let Some(msg) = json.get("error_message").and_then(|v| v.as_str()) {
        return Some(msg.to_string());
    }

    if let Some(exc) = json.get("exception").and_then(|v| v.as_str()) {
        return Some(exc.to_string());
    }

    None
}

fn find_status(json: &serde_json::Value) -> Option<String> {
    for obj in [
        Some(json),
        json.get("node"),
        json.get("result"),
        json.get("data"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(status) = obj.get("status").and_then(|v| v.as_str()) {
            return Some(status.to_string());
        }
    }
    None
}

fn build_error_message(json: &serde_json::Value, status: &str) -> String {
    let mut parts = Vec::new();

    if let Some(event_type) = json.get("event_type").and_then(|v| v.as_str()) {
        parts.push(event_type.to_string());
    }

    let name = json.get("name").and_then(|v| v.as_str()).or_else(|| {
        json.get("node")
            .and_then(|n| n.get("name"))
            .and_then(|v| v.as_str())
    });
    if let Some(name) = name {
        parts.push(name.to_string());
    }

    parts.push(format!("status={}", status));

    let error_msg = json
        .get("error")
        .and_then(|v| v.as_str())
        .or_else(|| json.get("message").and_then(|v| v.as_str()));
    if let Some(err) = error_msg {
        parts.push(err.to_string());
    }

    if parts.is_empty() {
        status.to_string()
    } else {
        parts.join(" - ")
    }
}
