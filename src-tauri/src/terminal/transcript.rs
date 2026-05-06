//! Claude Code transcript reader — parses JSONL session transcripts from disk.
//!
//! Reads Claude Code's on-disk session transcripts (structured JSONL with message
//! types, text blocks, plan content) without any dependency on Claude Code itself.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
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
    pub started_at: Option<String>,            // ISO 8601 — timestamp of first record
    pub first_message_preview: Option<String>, // first ~80 chars of first user message
    pub has_plans: bool,                       // true if any message has planContent
    pub display_name: String,                  // human-readable title derived from content
    /// Optional override for the frontend's computed `liveStatus`.
    ///
    /// `None` for real on-disk transcripts (the field is omitted from the
    /// serialized JSON via `skip_serializing_if`, preserving the legacy wire
    /// shape for every existing consumer). `Some(...)` is set only by the
    /// debug-gated test-fixtures path (`mcp::test_fixtures`) when an injected
    /// fake session is projected into this struct so that
    /// `useSessionManager` can render Promote / Commit buttons without a
    /// real PTY tab. Accepted values mirror the
    /// `SessionLiveStatus` enum on the frontend
    /// (`active-in-zone | needs-input | frozen | …`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub injected_live_status: Option<String>,
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

/// Lightweight digest of a session's tail — used for frozen detection and work summary hints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDigest {
    pub session_id: String,
    pub config_dir: String,
    pub project_path: String,
    pub last_message_type: String,      // "user" | "assistant" | ""
    pub last_message_timestamp: String, // ISO 8601
    pub last_message_preview: String,   // last ~200 chars of text
    pub last_assistant_had_tool_use: bool,
    pub likely_frozen: bool, // heuristic: recent + mid-task + not completed
    pub work_summary_hint: String, // "Task: {first} — Last: {last_snippet}"
}

// ── Session Cache ───────────────────────────────────────────────────────────

/// Cached session entry: stores parsed metadata alongside the file's mtime
/// so we can skip re-reading files whose content hasn't changed.
struct CachedSession {
    mtime: SystemTime,
    session: Option<TranscriptSession>, // None = workflow or empty session (filtered)
}

static SESSION_CACHE: once_cell::sync::Lazy<Mutex<HashMap<PathBuf, CachedSession>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

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

    // 3. Scan C:\claude\.claude-*\ (multi-account setups on Windows)
    let claude_root = std::path::Path::new("C:\\claude");
    if claude_root.is_dir() {
        if let Ok(entries) = std::fs::read_dir(claude_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with(".claude-")
                            && path.join("projects").exists()
                            && !dirs.iter().any(|d| d == &path)
                        {
                            dirs.push(path);
                        }
                    }
                }
            }
        }
    }

    // 4. Fallback: user home directory for standard .claude location
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

    let mut cache = SESSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        seen.insert(path.clone());

        // Get file mtime
        let metadata = fs::metadata(&path);
        let mtime = metadata.as_ref().ok().and_then(|m| m.modified().ok());

        // Check cache: if we've seen this file with the same mtime, reuse the result
        if let Some(mtime) = mtime {
            if let Some(cached) = cache.get(&path) {
                if cached.mtime == mtime {
                    if let Some(ref session) = cached.session {
                        // Update project_path/config_dir in case they differ
                        let mut s = session.clone();
                        s.project_path = project_path.to_string();
                        s.config_dir = config_dir.to_string_lossy().to_string();
                        sessions.push(s);
                    }
                    // else: cached as filtered (workflow/empty) — skip
                    continue;
                }
            }
        }

        let session_id = stem.to_string();
        let last_modified = mtime
            .map(|t| {
                chrono::DateTime::<chrono::Utc>::from(t)
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string()
            })
            .unwrap_or_default();

        // Read file content for line count, preview, and plan detection
        let content = fs::read_to_string(&path).unwrap_or_default();

        // Skip workflow-spawned sessions (they use bypassPermissions)
        if is_workflow_session(&content) {
            if let Some(mt) = mtime {
                cache.insert(
                    path.clone(),
                    CachedSession {
                        mtime: mt,
                        session: None,
                    },
                );
            }
            continue;
        }

        // Count actual user/assistant messages (cheap substring check)
        let message_count = content
            .lines()
            .filter(|l| {
                l.contains("\"type\":\"user\"")
                    || l.contains("\"type\": \"user\"")
                    || l.contains("\"type\":\"assistant\"")
                    || l.contains("\"type\": \"assistant\"")
            })
            .count();

        // Skip sessions with no real messages
        if message_count == 0 {
            if let Some(mt) = mtime {
                cache.insert(
                    path.clone(),
                    CachedSession {
                        mtime: mt,
                        session: None,
                    },
                );
            }
            continue;
        }

        // Substring check for plans (cheap — no JSON parse needed)
        let has_plans = content.contains("\"planContent\"");

        // Extract first user message preview (scan first ~20 lines)
        let first_message_preview = extract_first_user_preview(&content);
        let display_name = generate_display_name(&first_message_preview, &last_modified);

        // Extract started_at from first record's timestamp
        let started_at = extract_first_timestamp(&content);

        let session = TranscriptSession {
            session_id,
            project_path: project_path.to_string(),
            config_dir: config_dir.to_string_lossy().to_string(),
            message_count,
            last_modified,
            started_at,
            first_message_preview,
            has_plans,
            display_name,
            // Real on-disk sessions never carry a status override; the
            // frontend computes `liveStatus` from tab/digest correlation.
            injected_live_status: None,
        };

        if let Some(mt) = mtime {
            cache.insert(
                path.clone(),
                CachedSession {
                    mtime: mt,
                    session: Some(session.clone()),
                },
            );
        }

        sessions.push(session);
    }

    // Evict orphaned cache entries: any path under this project_dir that we
    // didn't observe in this scan corresponds to a session file that was
    // deleted from disk. Only touch entries under the *current* project_dir
    // so we don't disturb cached entries for other projects/config dirs.
    cache.retain(|path, _| !path.starts_with(&project_dir) || seen.contains(path));

    drop(cache);

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
                    let message_count = content
                        .lines()
                        .filter(|l| {
                            l.contains("\"type\":\"user\"")
                                || l.contains("\"type\": \"user\"")
                                || l.contains("\"type\":\"assistant\"")
                                || l.contains("\"type\": \"assistant\"")
                        })
                        .count();
                    let has_plans = content.contains("\"planContent\"");
                    let first_message_preview = extract_first_user_preview(&content);
                    let started_at = extract_first_timestamp(&content);
                    let display_name =
                        generate_display_name(&first_message_preview, &last_modified);

                    return Some(TranscriptSession {
                        session_id: session_id.to_string(),
                        project_path: project_path.to_string(),
                        config_dir: config_dir.to_string_lossy().to_string(),
                        message_count,
                        last_modified,
                        started_at,
                        first_message_preview,
                        has_plans,
                        display_name,
                        // Real on-disk session — no override.
                        injected_live_status: None,
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

/// Check if a JSONL transcript belongs to a workflow-spawned session.
///
/// Runner-spawned sessions (workflow generation, summarization, auto-debug) are
/// identified by two markers:
/// 1. A `queue-operation` record in the first line (all runner one-shot sessions)
/// 2. `permissionMode: "bypassPermissions"` in the first user message (supervisor sessions)
///
/// Interactive sessions never have either marker.
fn is_workflow_session(content: &str) -> bool {
    for line in content.lines().take(5) {
        // queue-operation is the only reliable signal — runner-spawned one-shot
        // sessions always start with this record; interactive sessions never have it.
        // Do NOT filter on bypassPermissions: users running Claude with
        // --dangerously-skip-permissions also have that in their permission-mode
        // record, and excluding them hides every interactive session.
        if line.contains("\"queue-operation\"") {
            return true;
        }
    }
    false
}

/// Strip XML/HTML tags from text (simple regex-free approach).
fn strip_xml_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result
}

/// Check if a user message is system-generated or uninformative.
fn is_system_or_noise(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        || t.starts_with("<local-command-caveat>")
        || t.starts_with("<command-name>")
        || t.starts_with("<command-message>")
        || t.starts_with("<command-args>")
        || t.starts_with("[Request interrupted")
        || t.starts_with("<system-reminder>")
        || t.starts_with("<available-deferred-tools>")
        || t.starts_with("<task-notification>")
}

/// Common generic prefixes that should be stripped to reveal the actual content.
const GENERIC_PREFIXES: &[&str] = &[
    "Implement the following plan:",
    "Implement this plan:",
    "Please implement the following:",
    "Please implement:",
    "Execute the following plan:",
    "Here is my plan:",
];

/// Generate a human-readable display name from the session's first user message.
///
/// Strips XML/HTML tags, markdown heading markers, and generic prefixes, then
/// truncates to ~50 chars at a word boundary. Falls back to a date-based name
/// for short or uninformative messages (slash commands, "continue", etc.).
fn generate_display_name(first_preview: &Option<String>, last_modified: &str) -> String {
    if let Some(preview) = first_preview {
        // Strip XML tags and clean up
        let stripped = strip_xml_tags(preview);
        let trimmed = stripped.trim();

        // Skip short/uninformative messages
        if trimmed.len() >= 8 && !trimmed.starts_with('/') {
            // Strip generic prefixes
            let after_prefix = strip_generic_prefix(trimmed);

            // Strip markdown heading markers and find first meaningful line
            let first_line = after_prefix
                .lines()
                .map(|l| l.trim().trim_start_matches('#').trim())
                .find(|l| l.len() >= 5)
                .unwrap_or(after_prefix);

            // Strip "..." suffix from the preview if present
            let clean = first_line.trim_end_matches("...");
            if clean.len() <= 50 {
                return first_line.to_string();
            }
            // Find a word boundary near 50 chars
            let truncated = &clean[..clean.len().min(50)];
            if let Some(last_space) = truncated.rfind(' ') {
                if last_space > 20 {
                    return format!("{}...", &clean[..last_space]);
                }
            }
            // No good word boundary — truncate at char boundary
            let mut end = 50.min(clean.len());
            while end > 0 && !clean.is_char_boundary(end) {
                end -= 1;
            }
            return format!("{}...", &clean[..end]);
        }
    }

    // Fallback: date-based name
    if !last_modified.is_empty() {
        let iso = last_modified.trim_end_matches('Z');
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S") {
            return format!("Session {}", dt.format("%b %-d, %H:%M"));
        }
    }

    "Untitled session".to_string()
}

/// Strip known generic prefixes to get to the actual meaningful content.
fn strip_generic_prefix(text: &str) -> &str {
    let lower = text.to_lowercase();
    for prefix in GENERIC_PREFIXES {
        if lower.starts_with(&prefix.to_lowercase()) {
            return text[prefix.len()..].trim();
        }
    }
    text
}

/// Extract the timestamp from the first record in a JSONL transcript.
fn extract_first_timestamp(content: &str) -> Option<String> {
    for line in content.lines().take(10) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(ts) = record.get("timestamp").and_then(|t| t.as_str()) {
                if !ts.is_empty() {
                    return Some(ts.to_string());
                }
            }
        }
    }
    None
}

/// Extract a preview from the first meaningful user message in a JSONL transcript.
///
/// Scans user records, skipping system-generated content (caveats, command
/// markup, interruption notices) to find the actual user prompt. Returns
/// up to ~80 characters of the first meaningful message.
fn extract_first_user_preview(content: &str) -> Option<String> {
    // Scan up to 50 lines to find a meaningful user message (system caveats
    // and command records can consume the first 5-10 lines).
    for line in content.lines().take(50) {
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

                    // Skip system-generated and noise messages
                    if is_system_or_noise(&text) {
                        continue;
                    }

                    // Strip XML tags that might be in the message
                    let cleaned = strip_xml_tags(&text);
                    let trimmed = cleaned.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    let preview = if trimmed.len() > 80 {
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
    None
}

// ── Session Digests (for frozen detection) ───────────────────────────────────

/// Read the tail of a JSONL session file to produce a lightweight digest.
///
/// Reads the last ~4KB of the file (seeking from end) to extract the last
/// user and assistant messages without parsing the entire transcript.
pub fn session_digest(
    config_dir: &Path,
    project_path: &str,
    session_id: &str,
    first_preview: &Option<String>,
) -> Result<SessionDigest, String> {
    let encoded = encode_project_path(project_path);
    let file_path = config_dir
        .join("projects")
        .join(&encoded)
        .join(format!("{}.jsonl", session_id));

    if !file_path.exists() {
        return Err(format!("Session file not found: {:?}", file_path));
    }

    // Read last ~4KB for efficiency (enough for 2-3 messages)
    let content = {
        let metadata =
            fs::metadata(&file_path).map_err(|e| format!("Failed to read metadata: {}", e))?;
        let file_size = metadata.len();

        if file_size <= 4096 {
            fs::read_to_string(&file_path).map_err(|e| format!("Failed to read file: {}", e))?
        } else {
            use std::io::{Read, Seek, SeekFrom};
            let mut file =
                fs::File::open(&file_path).map_err(|e| format!("Failed to open file: {}", e))?;
            file.seek(SeekFrom::End(-4096))
                .map_err(|e| format!("Failed to seek: {}", e))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|e| format!("Failed to read tail: {}", e))?;
            // Use lossy conversion to handle mid-character seek gracefully
            let buf = String::from_utf8_lossy(&bytes).into_owned();
            // Drop the first partial line (may be truncated from seek)
            if let Some(pos) = buf.find('\n') {
                buf[pos + 1..].to_string()
            } else {
                buf
            }
        }
    };

    let last_modified = fs::metadata(&file_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            chrono::DateTime::<chrono::Utc>::from(t)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
        .unwrap_or_default();

    // Parse lines from the tail to find the last user and assistant messages
    let mut last_msg_type = String::new();
    let mut last_msg_timestamp = String::new();
    let mut last_msg_preview = String::new();
    let mut last_assistant_had_tool_use = false;
    let mut last_assistant_preview = String::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let record_type = record
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default();

        match record_type {
            "user" => {
                if let Some(msg) = parse_user_record(&record) {
                    if !is_system_or_noise(&msg.text) {
                        last_msg_type = "user".to_string();
                        last_msg_timestamp = msg.timestamp.clone();
                        let preview_text = if msg.text.len() > 200 {
                            let mut end = 200;
                            while end > 0 && !msg.text.is_char_boundary(end) {
                                end -= 1;
                            }
                            format!("{}...", &msg.text[..end])
                        } else {
                            msg.text.clone()
                        };
                        last_msg_preview = preview_text;
                    }
                }
            }
            "assistant" => {
                if let Some(msg) = parse_assistant_record(&record) {
                    last_msg_type = "assistant".to_string();
                    last_msg_timestamp = msg.timestamp.clone();
                    last_assistant_had_tool_use = msg.has_tool_use;
                    let preview_text = if msg.text.len() > 200 {
                        let mut end = 200;
                        while end > 0 && !msg.text.is_char_boundary(end) {
                            end -= 1;
                        }
                        format!("{}...", &msg.text[..end])
                    } else {
                        msg.text.clone()
                    };
                    last_msg_preview = preview_text.clone();
                    last_assistant_preview = preview_text;
                }
            }
            _ => {}
        }
    }

    // Frozen heuristic:
    // 1. Last modified within 4 hours but at least 5 minutes ago (too recent = likely still active)
    // 2. Last message is assistant with tool_use (mid-task) OR last message is user (no response)
    let likely_frozen = {
        let age_secs = fs::metadata(&file_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| {
                std::time::SystemTime::now()
                    .duration_since(modified)
                    .ok()
                    .map(|d| d.as_secs())
            })
            .unwrap_or(u64::MAX);

        let is_in_frozen_window = (5 * 60..4 * 3600).contains(&age_secs);

        is_in_frozen_window
            && ((last_msg_type == "assistant" && last_assistant_had_tool_use)
                || last_msg_type == "user")
    };

    // Build work summary hint
    let task_part = first_preview.as_deref().unwrap_or("Unknown task");
    let last_part = if last_assistant_preview.is_empty() {
        "No assistant output".to_string()
    } else {
        let truncated = if last_assistant_preview.len() > 100 {
            let mut end = 100;
            while end > 0 && !last_assistant_preview.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &last_assistant_preview[..end])
        } else {
            last_assistant_preview.clone()
        };
        truncated
    };
    let work_summary_hint = format!("Task: {} — Last: {}", task_part, last_part);

    Ok(SessionDigest {
        session_id: session_id.to_string(),
        config_dir: config_dir.to_string_lossy().to_string(),
        project_path: project_path.to_string(),
        last_message_type: last_msg_type,
        last_message_timestamp: if last_msg_timestamp.is_empty() {
            last_modified
        } else {
            last_msg_timestamp
        },
        last_message_preview: last_msg_preview,
        last_assistant_had_tool_use,
        likely_frozen,
        work_summary_hint,
    })
}

/// Compute digests for a batch of sessions.
pub fn session_digests_batch(sessions: &[TranscriptSession]) -> Vec<SessionDigest> {
    sessions
        .iter()
        .filter_map(|s| {
            session_digest(
                Path::new(&s.config_dir),
                &s.project_path,
                &s.session_id,
                &s.first_message_preview,
            )
            .ok()
        })
        .collect()
}

// ── External Claude Process Detection ────────────────────────────────────────

/// A Claude Code process running outside this Runner instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalClaudeProcess {
    pub pid: u32,
    pub working_directory: Option<String>,
}

/// Try to extract a project working directory from a Claude Code command line.
/// Looks for common patterns in the command line arguments.
fn extract_workdir_from_cmdline(cmdline: &str) -> Option<String> {
    // Look for --project or -p flag
    for (i, part) in cmdline.split_whitespace().enumerate() {
        if part == "--project" || part == "-p" {
            return cmdline.split_whitespace().nth(i + 1).map(|s| s.to_string());
        }
    }
    // Look for a path-like argument that's not a flag or node binary
    for part in cmdline.split_whitespace().rev() {
        if !part.starts_with('-')
            && !part.contains("node")
            && !part.contains("claude")
            && (part.contains('/') || part.contains('\\'))
            && !part.contains("node_modules")
        {
            return Some(part.to_string());
        }
    }
    None
}

/// Detect Claude Code processes running outside this Runner.
///
/// Uses `wmic` on Windows to find node.exe processes whose command line
/// contains Claude Code markers. Excludes PIDs from the runner's own tracker.
pub fn find_external_claude_processes(exclude_pids: &[u32]) -> Vec<ExternalClaudeProcess> {
    let exclude_set: std::collections::HashSet<u32> = exclude_pids.iter().copied().collect();
    let mut results = Vec::new();

    #[cfg(target_os = "windows")]
    {
        // Use PowerShell to get Claude Code processes with their PIDs and command lines
        // Note: Win32_Process doesn't expose CWD directly, so we extract the
        // CLAUDE_CONFIG_DIR or project path from the command line as a proxy.
        let output = crate::process_helpers::no_window("powershell")
            .args([
                "-NoProfile",
                "-Command",
                r#"Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'node.exe' -and $_.CommandLine -match 'claude' } | Select-Object ProcessId, CommandLine | ForEach-Object { "$($_.ProcessId)|$($_.CommandLine)" }"#,
            ])
            .output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.splitn(2, '|').collect();
                if let Some(pid_str) = parts.first() {
                    if let Ok(pid) = pid_str.trim().parse::<u32>() {
                        if !exclude_set.contains(&pid) {
                            // Try to extract a meaningful working directory from the command line
                            let cmdline = parts.get(1).map(|s| s.trim()).unwrap_or("");
                            let working_dir = extract_workdir_from_cmdline(cmdline);
                            results.push(ExternalClaudeProcess {
                                pid,
                                working_directory: working_dir,
                            });
                        }
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // On Unix, use ps + grep
        let output = std::process::Command::new("ps").args(["aux"]).output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if line.contains("claude") && line.contains("node") {
                    let fields: Vec<&str> = line.split_whitespace().collect();
                    if let Some(pid_str) = fields.get(1) {
                        if let Ok(pid) = pid_str.parse::<u32>() {
                            if !exclude_set.contains(&pid) {
                                results.push(ExternalClaudeProcess {
                                    pid,
                                    working_directory: None,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_project_path() {
        assert_eq!(
            encode_project_path("C:/Users/jspin/Documents/qontinui_parent"),
            "C--Users-jspin-Documents-qontinui-parent"
        );
        assert_eq!(
            encode_project_path("C:\\Users\\jspin\\Documents\\qontinui_parent"),
            "C--Users-jspin-Documents-qontinui-parent"
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
    fn test_generate_display_name_from_message() {
        let preview = Some("Fix the login bug so users can log in".to_string());
        let name = generate_display_name(&preview, "2025-03-07T14:30:00Z");
        assert_eq!(name, "Fix the login bug so users can log in");
    }

    #[test]
    fn test_generate_display_name_truncates_long() {
        let preview =
            Some("Refactor the entire authentication system to use OAuth2 with PKCE flow and add refresh token rotation".to_string());
        let name = generate_display_name(&preview, "2025-03-07T14:30:00Z");
        assert!(name.len() <= 55); // ~50 chars + "..."
        assert!(name.ends_with("..."));
    }

    #[test]
    fn test_generate_display_name_fallback_to_date() {
        let name = generate_display_name(&Some("/commit".to_string()), "2025-03-07T14:30:00Z");
        assert!(name.starts_with("Session "));
    }

    #[test]
    fn test_generate_display_name_fallback_no_message() {
        let name = generate_display_name(&None, "2025-03-07T14:30:00Z");
        assert!(name.starts_with("Session "));
    }

    #[test]
    fn test_generate_display_name_strips_generic_prefix() {
        let preview = Some(
            "Implement the following plan:\n\n# UI Bridge Integration\n\nSome details..."
                .to_string(),
        );
        let name = generate_display_name(&preview, "2025-03-07T14:30:00Z");
        assert_eq!(name, "UI Bridge Integration");
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
