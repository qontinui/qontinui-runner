//! Claude Code transcript reader — parses JSONL session transcripts from disk.
//!
//! Reads Claude Code's on-disk session transcripts (structured JSONL with message
//! types, text blocks, plan content) without any dependency on Claude Code itself.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

// ── Types ────────────────────────────────────────────────────────────────────

/// Metadata about a Claude Code session found on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSession {
    pub session_id: String,
    pub project_path: String,
    pub config_dir: String,
    pub message_count: usize,
    pub last_modified: String,                 // ISO 8601
    pub first_message_preview: Option<String>, // first ~80 chars of first user message
    pub has_plans: bool,                       // true if any message has planContent
}

/// A single parsed message from a Claude Code transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub uuid: String,
    pub msg_type: String,             // "user" | "assistant"
    pub timestamp: String,            // ISO 8601
    pub text: String,                 // extracted plain text
    pub plan_content: Option<String>, // planContent field from user records
    pub model: Option<String>,        // from assistant records
    pub has_tool_use: bool,           // whether assistant used tools
}

// ── Config Dir Discovery ─────────────────────────────────────────────────────

/// Find Claude Code config directories on this machine.
///
/// Checks (in order):
/// 1. `CLAUDE_CONFIG_DIR` env var
/// 2. User-configured dirs from settings (validated for `projects/` subfolder)
/// 3. `~/.claude` fallback (standard location)
pub fn find_claude_config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. Check CLAUDE_CONFIG_DIR env var first
    if let Ok(env_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(&env_dir);
        if p.join("projects").exists() {
            dirs.push(p);
        }
    }

    // 2. User-configured dirs from settings
    let configured = crate::settings::get_claude_config_dirs();
    for dir_str in configured {
        let p = PathBuf::from(&dir_str);
        if p.join("projects").exists() && !dirs.iter().any(|d| d == &p) {
            dirs.push(p);
        }
    }

    // 3. Fallback: user home directory for standard .claude location
    if let Ok(home) = std::env::var("USERPROFILE") {
        let home_claude = PathBuf::from(&home).join(".claude");
        if home_claude.join("projects").exists() && !dirs.iter().any(|d| d == &home_claude) {
            dirs.push(home_claude);
        }
    }

    dirs
}

/// Encode a project path to the directory name format used by Claude Code.
///
/// Example: `C:/Users/jspin/Documents/qontinui_parent` → `C--Users-jspin-Documents-qontinui-parent`
fn encode_project_path(project_path: &str) -> String {
    // Normalize to forward slashes, then apply Claude Code's encoding:
    // - Replace `:` with empty string (drive letter colon)
    // - Replace `/` with `-`
    // - Replace `\` with `-`
    let normalized = project_path
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();

    // Claude Code encoding: :/ → --, then all :, /, \, _ → -
    // C:/Users/jspin/Documents/qontinui_parent → C--Users-jspin-Documents-qontinui-parent
    normalized
        .replace(":/", "--")
        .replace([':', '/', '\\', '_'], "-")
}

// ── Session Listing ──────────────────────────────────────────────────────────

/// List Claude Code sessions for a given project path.
///
/// Scans the config dir's `projects/{encoded_path}/` for `.jsonl` files
/// and returns metadata for each session found.
pub fn list_sessions(
    config_dir: &Path,
    project_path: &str,
) -> Result<Vec<TranscriptSession>, String> {
    let encoded = encode_project_path(project_path);
    let project_dir = config_dir.join("projects").join(&encoded);

    if !project_dir.exists() {
        debug!(
            "No project directory found at {:?} (encoded from '{}')",
            project_dir, project_path
        );
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    let entries = fs::read_dir(&project_dir)
        .map_err(|e| format!("Failed to read project directory {:?}: {}", project_dir, e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let session_id = stem.to_string();

                // Get file metadata for last_modified and approximate message count
                let metadata = fs::metadata(&path);
                let last_modified = metadata
                    .as_ref()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| {
                        chrono::DateTime::<chrono::Utc>::from(t)
                            .format("%Y-%m-%dT%H:%M:%SZ")
                            .to_string()
                    })
                    .unwrap_or_default();

                // Read file content for line count, preview, and plan detection
                let content = fs::read_to_string(&path).unwrap_or_default();
                let line_count = content.lines().count();

                // Substring check for plans (cheap — no JSON parse needed)
                let has_plans = content.contains("\"planContent\"");

                // Extract first user message preview (scan first ~20 lines)
                let first_message_preview = extract_first_user_preview(&content);

                sessions.push(TranscriptSession {
                    session_id,
                    project_path: project_path.to_string(),
                    config_dir: config_dir.to_string_lossy().to_string(),
                    message_count: line_count,
                    last_modified,
                    first_message_preview,
                    has_plans,
                });
            }
        }
    }

    // Sort by last_modified descending (newest first)
    sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    Ok(sessions)
}

// ── Session Reading ──────────────────────────────────────────────────────────

/// Parse a Claude Code JSONL transcript file into structured messages.
///
/// Only extracts `user` and `assistant` message records. Skips `system`,
/// `progress`, `file-history-snapshot`, and other non-message record types.
pub fn read_session(
    config_dir: &Path,
    project_path: &str,
    session_id: &str,
) -> Result<Vec<TranscriptMessage>, String> {
    let encoded = encode_project_path(project_path);
    let file_path = config_dir
        .join("projects")
        .join(&encoded)
        .join(format!("{}.jsonl", session_id));

    if !file_path.exists() {
        return Err(format!("Session file not found: {:?}", file_path));
    }

    let content = fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read session file {:?}: {}", file_path, e))?;

    let mut messages = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // Skip malformed lines
        };

        let record_type = record
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default();

        match record_type {
            "user" => {
                if let Some(msg) = parse_user_record(&record) {
                    messages.push(msg);
                }
            }
            "assistant" => {
                if let Some(msg) = parse_assistant_record(&record) {
                    messages.push(msg);
                }
            }
            _ => {
                // Skip system, progress, file-history-snapshot, etc.
            }
        }
    }

    Ok(messages)
}

/// Parse a `user` type record from the JSONL transcript.
fn parse_user_record(record: &serde_json::Value) -> Option<TranscriptMessage> {
    let uuid = record.get("uuid")?.as_str()?.to_string();
    let timestamp = record
        .get("timestamp")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();

    // Check for planContent (top-level field on user records)
    let plan_content = record
        .get("planContent")
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());

    // Extract text from message.content
    let content = record.get("message")?.get("content")?;

    // content is either a string (direct user text) or an array
    let text = if let Some(text_str) = content.as_str() {
        text_str.to_string()
    } else if let Some(arr) = content.as_array() {
        // Filter for text blocks, skip tool_result blocks
        let texts: Vec<String> = arr
            .iter()
            .filter_map(|block| {
                let block_type = block.get("type")?.as_str()?;
                if block_type == "text" {
                    block.get("text").and_then(|t| t.as_str()).map(String::from)
                } else {
                    None // Skip tool_result and other block types
                }
            })
            .collect();

        if texts.is_empty() {
            return None; // No text content (e.g., only tool results)
        }
        texts.join("\n")
    } else {
        return None;
    };

    // Skip empty messages
    if text.trim().is_empty() && plan_content.is_none() {
        return None;
    }

    Some(TranscriptMessage {
        uuid,
        msg_type: "user".to_string(),
        timestamp,
        text,
        plan_content,
        model: None,
        has_tool_use: false,
    })
}

/// Parse an `assistant` type record from the JSONL transcript.
fn parse_assistant_record(record: &serde_json::Value) -> Option<TranscriptMessage> {
    let uuid = record.get("uuid")?.as_str()?.to_string();
    let timestamp = record
        .get("timestamp")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();

    let message = record.get("message")?;
    let model = message
        .get("model")
        .and_then(|m| m.as_str())
        .map(String::from);

    let content = message.get("content")?.as_array()?;

    let mut texts = Vec::new();
    let mut has_tool_use = false;

    for block in content {
        let block_type = block
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default();
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    texts.push(text.to_string());
                }
            }
            "tool_use" => {
                has_tool_use = true;
            }
            _ => {} // Skip thinking, etc.
        }
    }

    // Only include messages that have text content
    if texts.is_empty() {
        return None;
    }

    Some(TranscriptMessage {
        uuid,
        msg_type: "assistant".to_string(),
        timestamp,
        text: texts.join("\n"),
        plan_content: None,
        model,
        has_tool_use,
    })
}

// ── Text Extraction ──────────────────────────────────────────────────────────

/// Format messages as readable conversation text suitable for `inline_context`.
pub fn extract_text_from_messages(messages: &[TranscriptMessage]) -> String {
    let mut parts = Vec::new();

    for msg in messages {
        let role = match msg.msg_type.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            _ => &msg.msg_type,
        };

        // Include plan content if present
        if let Some(ref plan) = msg.plan_content {
            parts.push(format!("## {} (Plan)\n\n{}", role, plan));
        }

        if !msg.text.trim().is_empty() {
            parts.push(format!("## {}\n\n{}", role, msg.text));
        }
    }

    parts.join("\n\n---\n\n")
}

/// Get the most recent session ID from `.claude.json` or by file modification time.
pub fn get_latest_session_id(config_dir: &Path, project_path: &str) -> Option<TranscriptSession> {
    // Try reading .claude.json for lastSessionId
    let claude_json = PathBuf::from(project_path).join(".claude.json");
    if let Ok(content) = fs::read_to_string(&claude_json) {
        if let Ok(json) = serde_json::from_str::<HashMap<String, serde_json::Value>>(&content) {
            if let Some(session_id) = json.get("lastSessionId").and_then(|v| v.as_str()) {
                debug!("Found lastSessionId from .claude.json: {}", session_id);
                // Verify the session file exists
                let encoded = encode_project_path(project_path);
                let session_file = config_dir
                    .join("projects")
                    .join(&encoded)
                    .join(format!("{}.jsonl", session_id));
                if session_file.exists() {
                    let metadata = fs::metadata(&session_file).ok();
                    let last_modified = metadata
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .map(|t| {
                            chrono::DateTime::<chrono::Utc>::from(t)
                                .format("%Y-%m-%dT%H:%M:%SZ")
                                .to_string()
                        })
                        .unwrap_or_default();

                    let content = fs::read_to_string(&session_file).unwrap_or_default();
                    let line_count = content.lines().count();
                    let has_plans = content.contains("\"planContent\"");
                    let first_message_preview = extract_first_user_preview(&content);

                    return Some(TranscriptSession {
                        session_id: session_id.to_string(),
                        project_path: project_path.to_string(),
                        config_dir: config_dir.to_string_lossy().to_string(),
                        message_count: line_count,
                        last_modified,
                        first_message_preview,
                        has_plans,
                    });
                }
            }
        }
    }

    // Fallback: return the most recently modified session
    match list_sessions(config_dir, project_path) {
        Ok(sessions) if !sessions.is_empty() => Some(sessions[0].clone()),
        _ => None,
    }
}

/// Extract a preview from the first user message in a JSONL transcript.
///
/// Scans the first 20 lines for a `"type":"user"` record and extracts
/// the first ~80 characters of the user's text content.
fn extract_first_user_preview(content: &str) -> Option<String> {
    for line in content.lines().take(20) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Quick check before parsing JSON
        if !line.contains("\"type\":\"user\"") && !line.contains("\"type\": \"user\"") {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
            if record.get("type").and_then(|t| t.as_str()) == Some("user") {
                // Extract text from message.content
                if let Some(content_val) = record.get("message").and_then(|m| m.get("content")) {
                    let text = if let Some(s) = content_val.as_str() {
                        s.to_string()
                    } else if let Some(arr) = content_val.as_array() {
                        arr.iter()
                            .filter_map(|block| {
                                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    block.get("text").and_then(|t| t.as_str()).map(String::from)
                                } else {
                                    None
                                }
                            })
                            .next()
                            .unwrap_or_default()
                    } else {
                        continue;
                    };

                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let preview = if trimmed.len() > 80 {
                            // Find a valid char boundary at or before byte 80
                            let mut end = 80;
                            while end > 0 && !trimmed.is_char_boundary(end) {
                                end -= 1;
                            }
                            format!("{}...", &trimmed[..end])
                        } else {
                            trimmed.to_string()
                        };
                        return Some(preview);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_project_path() {
        assert_eq!(
            encode_project_path("C:/Users/jspin/Documents/qontinui_parent"),
            "C--Users-jspin-Documents-qontinui_parent"
        );
        assert_eq!(
            encode_project_path("C:\\Users\\jspin\\Documents\\qontinui_parent"),
            "C--Users-jspin-Documents-qontinui_parent"
        );
    }

    #[test]
    fn test_parse_user_record_text() {
        let record = serde_json::json!({
            "type": "user",
            "uuid": "test-uuid",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "role": "user",
                "content": "Hello world"
            }
        });
        let msg = parse_user_record(&record).unwrap();
        assert_eq!(msg.text, "Hello world");
        assert_eq!(msg.msg_type, "user");
    }

    #[test]
    fn test_parse_user_record_array_content() {
        let record = serde_json::json!({
            "type": "user",
            "uuid": "test-uuid",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "role": "user",
                "content": [
                    {"type": "text", "text": "Hello"},
                    {"type": "tool_result", "tool_use_id": "x", "content": "result"}
                ]
            }
        });
        let msg = parse_user_record(&record).unwrap();
        assert_eq!(msg.text, "Hello");
    }

    #[test]
    fn test_parse_user_record_tool_result_only() {
        let record = serde_json::json!({
            "type": "user",
            "uuid": "test-uuid",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "x", "content": "result"}
                ]
            }
        });
        assert!(parse_user_record(&record).is_none());
    }

    #[test]
    fn test_parse_assistant_record() {
        let record = serde_json::json!({
            "type": "assistant",
            "uuid": "test-uuid",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "model": "claude-opus-4-6",
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "..."},
                    {"type": "text", "text": "Here is my response"},
                    {"type": "tool_use", "id": "t1", "name": "Read", "input": {}}
                ]
            }
        });
        let msg = parse_assistant_record(&record).unwrap();
        assert_eq!(msg.text, "Here is my response");
        assert_eq!(msg.model, Some("claude-opus-4-6".to_string()));
        assert!(msg.has_tool_use);
    }

    #[test]
    fn test_extract_text_from_messages() {
        let messages = vec![
            TranscriptMessage {
                uuid: "1".to_string(),
                msg_type: "user".to_string(),
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                text: "Build a calculator".to_string(),
                plan_content: None,
                model: None,
                has_tool_use: false,
            },
            TranscriptMessage {
                uuid: "2".to_string(),
                msg_type: "assistant".to_string(),
                timestamp: "2025-01-01T00:00:01Z".to_string(),
                text: "I'll create a calculator app.".to_string(),
                plan_content: None,
                model: Some("claude-opus-4-6".to_string()),
                has_tool_use: true,
            },
        ];
        let text = extract_text_from_messages(&messages);
        assert!(text.contains("## User"));
        assert!(text.contains("Build a calculator"));
        assert!(text.contains("## Assistant"));
        assert!(text.contains("I'll create a calculator app."));
    }

    #[test]
    fn test_extract_first_user_preview() {
        let content = r#"{"type":"system","uuid":"s1","timestamp":"2025-01-01T00:00:00Z"}
{"type":"user","uuid":"u1","timestamp":"2025-01-01T00:00:01Z","message":{"role":"user","content":"Fix the login bug so that users can authenticate properly"}}
{"type":"assistant","uuid":"a1","timestamp":"2025-01-01T00:00:02Z","message":{"role":"assistant","content":[{"type":"text","text":"I'll fix it."}]}}"#;
        let preview = extract_first_user_preview(content);
        assert_eq!(
            preview,
            Some("Fix the login bug so that users can authenticate properly".to_string())
        );
    }

    #[test]
    fn test_extract_first_user_preview_truncates() {
        let long_msg = "a".repeat(120);
        let content = format!(
            r#"{{"type":"user","uuid":"u1","timestamp":"2025-01-01T00:00:00Z","message":{{"role":"user","content":"{}"}}}}"#,
            long_msg
        );
        let preview = extract_first_user_preview(&content).unwrap();
        assert!(preview.len() <= 84); // 80 chars + "..."
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn test_find_claude_config_dirs() {
        // This test just verifies it doesn't panic — results depend on the machine
        let dirs = find_claude_config_dirs();
        // On the dev machine, we should find at least one
        // (but don't assert that in CI)
        let _ = dirs;
    }
}
