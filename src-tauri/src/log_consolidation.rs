//! Log consolidation utilities for AI output.
//!
//! This module provides functions to consolidate AI output logs from ai-output.jsonl
//! into a readable format grouped by source (e.g., "claude", "prompt").
//!
//! Used by:
//! - AI Data Viewer (frontend display)
//! - Continuation prompts (multi-iteration task context)

#![allow(dead_code)]

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use tracing::warn;

/// A chunk of consolidated AI output from a single source.
#[derive(Debug, Clone, Serialize)]
pub struct AiOutputChunk {
    /// Source of the output (e.g., "claude", "prompt")
    pub source: String,
    /// Start time of this chunk (formatted as HH:MM:SS)
    pub start_time: String,
    /// End time of this chunk (formatted as HH:MM:SS), None if single entry
    pub end_time: Option<String>,
    /// Combined content from all entries in this chunk
    pub content: String,
    /// Number of raw entries that were consolidated
    pub entry_count: usize,
}

/// Result of consolidating AI output.
#[derive(Debug, Clone, Serialize)]
pub struct ConsolidatedAiOutput {
    pub chunks: Vec<AiOutputChunk>,
    pub total_entries: usize,
}

/// Parse timestamp from a JSONL entry.
/// Supports various timestamp formats found in log entries.
fn parse_jsonl_timestamp(entry: &serde_json::Value) -> Option<DateTime<Utc>> {
    let timestamp_value = entry
        .get("timestamp")
        .or_else(|| entry.get("time"))
        .or_else(|| entry.get("ts"))?;

    // Try numeric timestamp (epoch milliseconds)
    if let Some(ms) = timestamp_value.as_i64() {
        return DateTime::from_timestamp_millis(ms).map(|dt| dt.with_timezone(&Utc));
    }

    // Try numeric as u64
    if let Some(ms) = timestamp_value.as_u64() {
        return DateTime::from_timestamp_millis(ms as i64).map(|dt| dt.with_timezone(&Utc));
    }

    // Try numeric as f64 (some systems use float timestamps)
    if let Some(ms) = timestamp_value.as_f64() {
        return DateTime::from_timestamp_millis(ms as i64).map(|dt| dt.with_timezone(&Utc));
    }

    // Try string formats
    let timestamp_str = timestamp_value.as_str()?;

    // Try RFC3339 format first (e.g., "2026-01-07T10:30:00Z")
    if let Ok(dt) = DateTime::parse_from_rfc3339(timestamp_str) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try ISO format without timezone (e.g., "2026-01-07T10:30:00")
    if let Ok(dt) = NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%dT%H:%M:%S") {
        return Some(DateTime::from_naive_utc_and_offset(dt, Utc));
    }

    // Try simple format (e.g., "2026-01-07 10:30:00")
    if let Ok(dt) = NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S") {
        return Some(DateTime::from_naive_utc_and_offset(dt, Utc));
    }

    // Try milliseconds format
    if let Ok(dt) = NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%dT%H:%M:%S%.3f") {
        return Some(DateTime::from_naive_utc_and_offset(dt, Utc));
    }

    None
}

/// Internal struct for parsed entries
struct ParsedEntry {
    timestamp: DateTime<Utc>,
    source: String,
    line: String,
}

/// Get the .dev-logs directory path.
fn get_dev_logs_dir() -> PathBuf {
    crate::paths::get_dev_logs_dir()
}

/// Consolidate AI output from ai-output.jsonl for a time range.
///
/// Reads the ai-output.jsonl file and groups consecutive entries by source
/// (e.g., "claude", "prompt") into readable chunks.
///
/// # Arguments
/// * `start_time` - Start of the time range (inclusive)
/// * `end_time` - End of the time range (inclusive), or None for now
///
/// # Returns
/// Consolidated output grouped by source with timestamps
pub fn consolidate_ai_output(
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
) -> Result<ConsolidatedAiOutput, String> {
    let dev_logs_dir = get_dev_logs_dir();
    let file_path = dev_logs_dir.join("ai-output.jsonl");

    if !file_path.exists() {
        return Ok(ConsolidatedAiOutput {
            chunks: vec![],
            total_entries: 0,
        });
    }

    // Read and filter entries
    let content = fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read ai-output.jsonl: {}", e))?;

    let end = end_time.unwrap_or_else(Utc::now);

    // Parse entries with timestamp and source
    let mut entries: Vec<ParsedEntry> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|entry| {
            let ts = parse_jsonl_timestamp(&entry)?;
            if ts < start_time || ts > end {
                return None;
            }
            let source = entry.get("source")?.as_str()?.to_string();
            let line = entry.get("line")?.as_str()?.to_string();
            Some(ParsedEntry {
                timestamp: ts,
                source,
                line,
            })
        })
        .collect();

    let total_entries = entries.len();

    // Sort by timestamp
    entries.sort_by_key(|e| e.timestamp);

    // Consolidate consecutive entries by source
    let chunks = consolidate_entries(entries);

    Ok(ConsolidatedAiOutput {
        chunks,
        total_entries,
    })
}

/// Consolidate parsed entries into chunks grouped by source.
fn consolidate_entries(entries: Vec<ParsedEntry>) -> Vec<AiOutputChunk> {
    let mut chunks: Vec<AiOutputChunk> = Vec::new();
    let mut current_source: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();
    let mut chunk_start: Option<DateTime<Utc>> = None;
    let mut chunk_end: Option<DateTime<Utc>> = None;
    let mut chunk_count: usize = 0;

    for entry in entries {
        if current_source.as_ref() == Some(&entry.source) {
            // Same source, add to current chunk
            current_lines.push(entry.line);
            chunk_end = Some(entry.timestamp);
            chunk_count += 1;
        } else {
            // Different source, finalize current chunk and start new one
            if let Some(source) = current_source.take() {
                if !current_lines.is_empty() {
                    chunks.push(AiOutputChunk {
                        source,
                        start_time: chunk_start
                            .map(|t| t.format("%H:%M:%S").to_string())
                            .unwrap_or_default(),
                        end_time: if chunk_count > 1 {
                            chunk_end.map(|t| t.format("%H:%M:%S").to_string())
                        } else {
                            None
                        },
                        content: current_lines.join("\n"),
                        entry_count: chunk_count,
                    });
                }
            }
            // Start new chunk
            current_source = Some(entry.source);
            current_lines = vec![entry.line];
            chunk_start = Some(entry.timestamp);
            chunk_end = Some(entry.timestamp);
            chunk_count = 1;
        }
    }

    // Don't forget the last chunk
    if let Some(source) = current_source {
        if !current_lines.is_empty() {
            chunks.push(AiOutputChunk {
                source,
                start_time: chunk_start
                    .map(|t| t.format("%H:%M:%S").to_string())
                    .unwrap_or_default(),
                end_time: if chunk_count > 1 {
                    chunk_end.map(|t| t.format("%H:%M:%S").to_string())
                } else {
                    None
                },
                content: current_lines.join("\n"),
                entry_count: chunk_count,
            });
        }
    }

    chunks
}

/// Format consolidated AI output as markdown for continuation prompts.
///
/// This produces a readable format showing previous AI session output
/// grouped by source (claude responses vs user/system prompts).
///
/// # Example output:
/// ```markdown
/// ### Previous AI Output
///
/// **[claude]** [14:58:42 - 14:59:15] (47 lines)
/// ```
/// I'll analyze the codebase...
/// [content here]
/// ```
///
/// **[prompt]** [14:59:16] (1 line)
/// ```
/// User provided additional context...
/// ```
/// ```
pub fn format_consolidated_for_prompt(consolidated: &ConsolidatedAiOutput) -> String {
    if consolidated.chunks.is_empty() {
        return String::new();
    }

    let mut output = String::new();
    output.push_str("### Previous AI Output\n\n");
    output.push_str(&format!(
        "_Consolidated from {} raw log entries_\n\n",
        consolidated.total_entries
    ));

    for chunk in &consolidated.chunks {
        // Header with source and time
        let time_str = if let Some(ref end_time) = chunk.end_time {
            format!("[{} - {}]", chunk.start_time, end_time)
        } else {
            format!("[{}]", chunk.start_time)
        };

        let lines_str = if chunk.entry_count == 1 {
            "1 line".to_string()
        } else {
            format!("{} lines", chunk.entry_count)
        };

        output.push_str(&format!(
            "**[{}]** {} ({})\n",
            chunk.source, time_str, lines_str
        ));
        output.push_str("```\n");
        output.push_str(&chunk.content);
        if !chunk.content.ends_with('\n') {
            output.push('\n');
        }
        output.push_str("```\n\n");
    }

    output
}

/// Consolidate AI output for a task run and format for continuation prompt.
///
/// This is the main entry point used by mcp_api.rs for building continuation
/// prompts in multi-iteration task runs.
///
/// # Arguments
/// * `task_created_at` - RFC3339 timestamp of when the task was created
/// * `task_completed_at` - RFC3339 timestamp of when the task completed (or None if still running)
///
/// # Returns
/// Formatted markdown string with consolidated AI output, or empty string on error
pub fn get_consolidated_output_for_task(
    task_created_at: &str,
    task_completed_at: Option<&str>,
) -> String {
    // Parse task run times
    let start_time = match DateTime::parse_from_rfc3339(task_created_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(e) => {
            warn!("Failed to parse task start time: {}", e);
            return String::new();
        }
    };

    let end_time = task_completed_at
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc));

    // Consolidate the output
    match consolidate_ai_output(start_time, end_time) {
        Ok(consolidated) => format_consolidated_for_prompt(&consolidated),
        Err(e) => {
            warn!("Failed to consolidate AI output: {}", e);
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consolidate_entries_empty() {
        let entries = vec![];
        let chunks = consolidate_entries(entries);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_consolidate_entries_single() {
        let entries = vec![ParsedEntry {
            timestamp: Utc::now(),
            source: "claude".to_string(),
            line: "Hello".to_string(),
        }];
        let chunks = consolidate_entries(entries);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].source, "claude");
        assert_eq!(chunks[0].content, "Hello");
        assert_eq!(chunks[0].entry_count, 1);
        assert!(chunks[0].end_time.is_none());
    }

    #[test]
    fn test_consolidate_entries_same_source() {
        let now = Utc::now();
        let entries = vec![
            ParsedEntry {
                timestamp: now,
                source: "claude".to_string(),
                line: "Line 1".to_string(),
            },
            ParsedEntry {
                timestamp: now + chrono::Duration::seconds(1),
                source: "claude".to_string(),
                line: "Line 2".to_string(),
            },
        ];
        let chunks = consolidate_entries(entries);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].source, "claude");
        assert_eq!(chunks[0].content, "Line 1\nLine 2");
        assert_eq!(chunks[0].entry_count, 2);
        assert!(chunks[0].end_time.is_some());
    }

    #[test]
    fn test_consolidate_entries_alternating_sources() {
        let now = Utc::now();
        let entries = vec![
            ParsedEntry {
                timestamp: now,
                source: "claude".to_string(),
                line: "AI response".to_string(),
            },
            ParsedEntry {
                timestamp: now + chrono::Duration::seconds(1),
                source: "prompt".to_string(),
                line: "User input".to_string(),
            },
            ParsedEntry {
                timestamp: now + chrono::Duration::seconds(2),
                source: "claude".to_string(),
                line: "Another AI response".to_string(),
            },
        ];
        let chunks = consolidate_entries(entries);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].source, "claude");
        assert_eq!(chunks[1].source, "prompt");
        assert_eq!(chunks[2].source, "claude");
    }
}
