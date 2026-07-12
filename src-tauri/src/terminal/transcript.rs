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
    /// Optional synthetic-tab spec, set only by the debug-gated test-fixtures
    /// path (`mcp::test_fixtures`) for a *tab-backed* injected fake.
    ///
    /// `None` for real on-disk transcripts (omitted from the serialized JSON
    /// via `skip_serializing_if`). When `Some(...)`, the frontend derives a
    /// minimal `TerminalTab` from it (`syntheticTabs.ts`) and feeds it through
    /// the REAL `useSessionManager` tab-correlation path so an injected fake
    /// can land in the `idle` / tab-backed `error` / tab-backed `completed`
    /// StatusStrip buckets that the `injected_live_status` short-circuit
    /// cannot reach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub injected_tab: Option<InjectedTabSpec>,
}

/// Synthetic-tab spec carried on a *tab-backed* injected fake (see
/// `TranscriptSession::injected_tab`). Defined here (an always-compiled
/// module) rather than in the cfg-gated `mcp::test_fixtures` so the field type
/// resolves in release builds without the `test-fixtures` feature. Only ever
/// populated behind that gated seam; the frontend consumer is dead code in a
/// release build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InjectedTabSpec {
    /// `true` → a live synthetic tab the staleness sweep can age into
    /// `frozen` (idle bucket). `false` → a dead tab whose `exit_code` the
    /// dead-tab sweep branch maps to `completed` / `error`.
    pub is_alive: bool,
    /// Exit code for a dead synthetic tab (`0` → completed, non-zero →
    /// error). `None` for a live (idle) tab.
    pub exit_code: Option<i32>,
    /// Pre-age in ms for a live tab's `lastOutput` so the 60s staleness sweep
    /// classifies it stale. `None` for dead tabs.
    pub quiet_ms: Option<u64>,
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

/// Absolute path to the on-disk JSONL transcript for a `(config_dir,
/// project_path, session_id)` triple, whether or not it exists.
///
/// Mirrors the path construction used by [`read_session`] / [`session_digest`]
/// (`<config_dir>/projects/<encoded(project_path)>/<session_id>.jsonl`).
/// Exposed so the chat-resume path (Phase 3, restart resilience) can probe
/// whether a lossless `--resume` is possible before deciding to fall back to a
/// lossy output_log summary.
pub fn session_transcript_path(config_dir: &Path, project_path: &str, session_id: &str) -> PathBuf {
    config_dir
        .join("projects")
        .join(encode_project_path(project_path))
        .join(format!("{}.jsonl", session_id))
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
                // Millisecond precision so `get_latest_session_id`'s `since` filter
                // can compare against `Date.now()`-derived spawn timestamps without
                // a second-boundary truncation race that drops fresh sessions whose
                // mtime falls in the same wall-clock second as the spawn.
                chrono::DateTime::<chrono::Utc>::from(t)
                    .format("%Y-%m-%dT%H:%M:%S%.3fZ")
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
            injected_tab: None,
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

// ── Disk-only restore scan (session-restore-redesign Phase 3 / G3) ───────────
//
// The boot-restore recovery layer is a projection of the registry, so it
// inherits every registry gap: a session that was LIVE at crash but that the
// registry never captured (the spawn-record AND the provider hook both missed,
// AND the crash beat the next reconcile poll) has no restorable row and is
// silently lost. This scan closes that gap from the DISK side — it enumerates
// EVERY project dir under a config dir (so a session whose working dir the
// registry never recorded is still found) and reports the recently-active ones
// as crash-recovery candidates. The account is the config dir that holds the
// transcript — derived DYNAMICALLY (the caller iterates `find_claude_config_dirs`),
// never a hardcoded account list.

/// A registry-absent, on-disk session candidate for the disk-only restore net.
/// Discovered by a full projects-tree scan of one config dir (unlike
/// [`list_sessions`], which needs a KNOWN project path).
#[derive(Debug, Clone)]
pub struct RecentTranscript {
    /// Transcript id (the `.jsonl` file stem).
    pub session_id: String,
    /// The config dir that holds it — THE ACCOUNT this session must resume
    /// under.
    pub config_dir: String,
    /// The session's real working dir, recovered from the transcript's OWN
    /// `cwd` field — NOT the lossy encoded project-dir name (`_`, `/`, `:`,
    /// `\` all collapse to `-`, so decoding it is ambiguous). Preserved
    /// verbatim so a `claude --resume` launched in this dir re-encodes to the
    /// exact same project path Claude wrote the transcript under.
    pub working_dir: String,
    /// File mtime as unix epoch millis — the session's last on-disk activity
    /// (the liveness signal the window filter ranks).
    pub last_activity_ms: i64,
}

/// Recover the launching working directory from a transcript's own records.
/// Claude's `user`/`assistant` records carry a top-level `cwd`; the leading
/// `queue-operation`/summary records do not. Returns the first non-empty `cwd`
/// in the first ~20 lines, verbatim. `None` when no record carried one (an
/// empty or malformed transcript) — such a candidate can't be resumed to the
/// right cwd and is dropped by the scan.
fn extract_cwd(content: &str) -> Option<String> {
    for line in content.lines().take(20) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(cwd) = record.get("cwd").and_then(|c| c.as_str()) {
                if !cwd.trim().is_empty() {
                    return Some(cwd.to_string());
                }
            }
        }
    }
    None
}

/// Scan EVERY project dir under `<config_dir>/projects/` and return one
/// [`RecentTranscript`] per transcript whose file was modified within
/// `window_ms` before `now_ms`.
///
/// Pre-filters on the CHEAP mtime (a dir-entry metadata read) BEFORE opening a
/// file, so only recently-touched transcripts are parsed for their `cwd`. Skips
/// workflow-spawned sessions (the same first-5-lines marker [`list_sessions`]
/// uses) and any transcript with no recoverable `cwd`. Fail-soft by
/// construction: an unreadable projects root / project dir / file is skipped,
/// never fatal — the worst case is an empty result (the caller degrades to the
/// registry-only restorable set).
pub fn list_recent_sessions_all_projects(
    config_dir: &Path,
    now_ms: i64,
    window_ms: i64,
) -> Vec<RecentTranscript> {
    let projects_root = config_dir.join("projects");
    let mut out = Vec::new();
    let Ok(project_dirs) = fs::read_dir(&projects_root) else {
        return out; // no projects dir (or unreadable) — nothing to scan
    };
    for pd in project_dirs.flatten() {
        let pdir = pd.path();
        if !pdir.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(&pdir) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Cheap mtime pre-filter — skip files outside the window WITHOUT
            // reading their content. A future-dated mtime (clock skew) yields a
            // saturating 0 age and is admitted (treated as fresh).
            let mtime_ms = match fs::metadata(&path).ok().and_then(|m| m.modified().ok()) {
                Some(t) => chrono::DateTime::<chrono::Utc>::from(t).timestamp_millis(),
                None => continue,
            };
            if now_ms.saturating_sub(mtime_ms) > window_ms {
                continue;
            }
            let content = fs::read_to_string(&path).unwrap_or_default();
            if content.is_empty() {
                continue;
            }
            // Runner-spawned one-shot sessions (workflow gen, summarization,
            // auto-debug) are not user sessions — never offer them for restore.
            if is_workflow_session(&content) {
                continue;
            }
            let Some(working_dir) = extract_cwd(&content) else {
                continue; // no recoverable cwd — can't resume to the right dir
            };
            out.push(RecentTranscript {
                session_id: stem.to_string(),
                config_dir: config_dir.to_string_lossy().to_string(),
                working_dir,
                last_activity_ms: mtime_ms,
            });
        }
    }
    out
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

// ── Touched-File Extraction (Phase 1.5 — transcript-tail populator) ──────────
//
// These helpers walk an `{type:"assistant"}` JSONL record and emit a
// `TouchedFile` for each `Edit` / `Write` / `MultiEdit` `tool_use` block. They
// are pure (no I/O) and live next to `parse_assistant_record` because they
// share the same `serde_json::Value` walking style and timestamp field.
//
// Consumed by `terminal::transcript_watcher` to populate
// `coord.session_touched_files` for PTY-launched AI tabs (the SDK
// `auto_register_file` path covers SDK chat sessions).

/// One file-edit observation extracted from a single tool-use block in a
/// transcript record. The runtime tail task converts these into
/// `pg.record_file_touched` calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TouchedFile {
    /// The CLI-side session id (the JSONL file's stem). Carried through so
    /// the consumer doesn't have to thread it separately.
    pub session_id: String,
    /// Absolute file path as the tool reported it. NOT canonicalized — the
    /// downstream dirty-subset query buckets by git toplevel, which handles
    /// path-normalization there.
    pub file_path: String,
    /// Which tool produced the touch.
    pub tool: ToolKind,
    /// Wall-clock ms when the record was written, parsed from the record's
    /// top-level `timestamp` field (ISO-8601). Falls back to "now" when
    /// missing or unparseable.
    pub recorded_at_ms: u64,
}

/// Tool that wrote to a file. Match arm is intentionally explicit so adding a
/// new tool (NotebookEdit, etc.) is a deliberate extension, not a guess.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Edit,
    Write,
    MultiEdit,
}

/// Errors from `parse_line_for_touched_files`. Malformed JSON is the only
/// case — the caller logs and continues, never panics.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("malformed JSON: {0}")]
    MalformedJson(String),
}

/// Parse the record's top-level `timestamp` field (ISO-8601) into wall-clock
/// ms. Falls back to `SystemTime::now()` when absent or unparseable.
fn record_timestamp_ms(record: &serde_json::Value) -> u64 {
    if let Some(ts) = record.get("timestamp").and_then(|t| t.as_str()) {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
            let ms = dt.timestamp_millis();
            if ms >= 0 {
                return ms as u64;
            }
        }
    }
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Walk `content[]` of an `{type:"assistant"}` record and emit a TouchedFile
/// for each Edit/Write/MultiEdit `tool_use` block. Returns 0..N entries (a
/// single record can carry multiple tool_use blocks). Skips non-touching
/// blocks, unknown tools, and blocks with missing/non-string `input.file_path`
/// silently — never fails.
///
/// The input shape is identical to what `parse_assistant_record:444` already
/// consumes; reuse that mental model.
pub fn extract_touched_files_from_assistant_record(
    session_id: &str,
    record: &serde_json::Value,
) -> Vec<TouchedFile> {
    let recorded_at_ms = record_timestamp_ms(record);

    let Some(message) = record.get("message") else {
        return Vec::new();
    };
    let Some(content) = message.get("content").and_then(|c| c.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for block in content {
        let block_type = block
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default();
        if block_type != "tool_use" {
            continue;
        }
        let name = match block.get("name").and_then(|n| n.as_str()) {
            Some(n) => n,
            None => continue,
        };
        let tool = match name {
            "Edit" => ToolKind::Edit,
            "Write" => ToolKind::Write,
            "MultiEdit" => ToolKind::MultiEdit,
            // Future tools (NotebookEdit, etc.) need an explicit arm — don't
            // guess. Read/Bash/Grep/Glob/etc. fall through silently.
            _ => continue,
        };

        // All three tools key the path on `input.file_path` (string). Skip
        // blocks that are missing/non-string rather than failing; the parser
        // is intentionally tolerant of upstream shape drift.
        let Some(file_path) = block
            .get("input")
            .and_then(|i| i.get("file_path"))
            .and_then(|p| p.as_str())
        else {
            continue;
        };

        out.push(TouchedFile {
            session_id: session_id.to_string(),
            file_path: file_path.to_string(),
            tool,
            recorded_at_ms,
        });
    }
    out
}

/// Convenience wrapper: parse one JSONL line, dispatch by `type`, and return
/// touched files (empty for non-assistant records). Wraps `serde_json::from_str`
/// + `extract_touched_files_from_assistant_record`. Malformed JSON →
/// `Err(ParseError)`; the caller logs and continues — never panics.
pub fn parse_line_for_touched_files(
    session_id: &str,
    line: &str,
) -> Result<Vec<TouchedFile>, ParseError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let record: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| ParseError::MalformedJson(e.to_string()))?;

    let record_type = record
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    if record_type != "assistant" {
        return Ok(Vec::new());
    }
    Ok(extract_touched_files_from_assistant_record(
        session_id, &record,
    ))
}

// ── Agent-log Extraction (Phase 2 — transcript → coord.agent_logs) ───────────
//
// These helpers walk an `{type:"assistant"}` JSONL record and emit the
// meaningful conversational content — assistant text + tool_use invocations —
// as `AgentLogObs` rows. The transcript watcher converts each into a
// `coord.agent_logs` entry so PTY-launched CLI Claude sessions become visible
// on the `/admin/coord/agents` dashboard (the runner-managed-session path is
// covered separately by the `AgentLogEmitter` Phase-1 milestones). Pure (no
// I/O); shares the same `serde_json::Value` walking style as
// `extract_touched_files_from_assistant_record`.

/// Cap on emitted payload text (assistant text and serialized tool input). Keeps
/// per-line coord traffic bounded for verbose assistant turns / large tool
/// inputs; the watcher streams a continuous tail, not a one-shot snapshot.
const AGENT_LOG_TEXT_CAP: usize = 8 * 1024;

/// One meaningful observation extracted from an assistant transcript record,
/// ready to become a `coord.agent_logs` entry. The watcher maps each to an
/// `AgentLogEmitter::emit(level, event, payload)` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLogObs {
    /// An assistant text block. `event:"assistant"`, `payload:{text}`.
    Assistant { text: String },
    /// A tool invocation. `event:"tool_use"`, `payload:{tool, input?}`.
    ToolUse {
        tool: String,
        /// Compact-serialized, truncated tool input (omitted when absent).
        input: Option<String>,
    },
}

/// Truncate `s` to at most `AGENT_LOG_TEXT_CAP` bytes on a char boundary,
/// appending an ellipsis marker when it was cut. Bounds per-line volume.
fn truncate_for_log(s: &str) -> String {
    if s.len() <= AGENT_LOG_TEXT_CAP {
        return s.to_string();
    }
    let mut end = AGENT_LOG_TEXT_CAP;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated]", &s[..end])
}

/// Walk `content[]` of an `{type:"assistant"}` record and emit an `AgentLogObs`
/// for each text block (assistant turn) and each `tool_use` block (tool
/// invocation), in document order. Skips `thinking`, `tool_result`, and other
/// noise. Non-assistant records and missing/blank text yield nothing.
pub fn extract_agent_log_obs_from_assistant_record(record: &serde_json::Value) -> Vec<AgentLogObs> {
    let Some(content) = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for block in content {
        let block_type = block
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default();
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if !text.trim().is_empty() {
                        out.push(AgentLogObs::Assistant {
                            text: truncate_for_log(text),
                        });
                    }
                }
            }
            "tool_use" => {
                let Some(tool) = block.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let input = block
                    .get("input")
                    .filter(|i| !i.is_null())
                    .map(|i| truncate_for_log(&serde_json::to_string(i).unwrap_or_default()));
                out.push(AgentLogObs::ToolUse {
                    tool: tool.to_string(),
                    input,
                });
            }
            _ => {} // thinking / tool_result / etc. — skip.
        }
    }
    out
}

/// Convenience wrapper: parse one JSONL line, dispatch by `type`, and return
/// agent-log observations (empty for non-assistant or malformed records — the
/// watcher tolerates noise and never fails on a single bad line).
pub fn parse_line_for_agent_log(line: &str) -> Vec<AgentLogObs> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(record) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Vec::new();
    };
    if record.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return Vec::new();
    }
    extract_agent_log_obs_from_assistant_record(&record)
}

/// Test-only re-export of `is_workflow_session` so the `transcript_watcher`
/// module can apply the same first-5-lines filter without duplicating the
/// logic. The function itself stays private to this module.
pub(crate) fn is_workflow_session_marker(content: &str) -> bool {
    is_workflow_session(content)
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

/// Parse a `last_modified` string back into a `DateTime<Utc>`.
///
/// Accepts both RFC 3339 (the format produced by Claude Code's own JSONL
/// records) and the `"%Y-%m-%dT%H:%M:%SZ"` shape this module emits via
/// `chrono::DateTime::format`. Returns `None` for unparseable input — the
/// caller decides whether that is "stale" or "drop".
fn parse_session_timestamp(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            // Fallback for the "%Y-%m-%dT%H:%M:%SZ" format used in this module.
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
                .ok()
                .map(|ndt| {
                    chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(ndt, chrono::Utc)
                })
        })
}

/// Get the most recent session ID from `.claude.json` or by file modification time.
///
/// `since` (optional) filters out sessions whose `last_modified` is at or
/// before the supplied threshold. Both branches honour the filter:
///
/// - The `.claude.json` `lastSessionId` shortcut falls through (instead of
///   early-returning) when its session's mtime is `<= since`. Records with
///   unparseable timestamps are also treated as stale so the mtime fallback
///   can still find a fresher session.
/// - The mtime-sorted fallback drops sessions whose `last_modified` is `<=
///   since` (or unparseable when `since` is `Some`).
pub fn get_latest_session_id(
    config_dir: &Path,
    project_path: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<TranscriptSession> {
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
                            // Millisecond precision: see comment in `list_sessions`.
                            chrono::DateTime::<chrono::Utc>::from(t)
                                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                                .to_string()
                        })
                        .unwrap_or_default();

                    // Honour `since`: if the shortcut record is not strictly
                    // newer than the threshold (or its timestamp is
                    // unparseable), fall through to the mtime fallback so
                    // it can find a fresher session. Treating unparseable
                    // timestamps as "stale enough to fall through" is safer
                    // than binding the wrong session.
                    let shortcut_is_stale = match since {
                        Some(threshold) => match parse_session_timestamp(&last_modified) {
                            Some(parsed) => parsed <= threshold,
                            None => true,
                        },
                        None => false,
                    };

                    if !shortcut_is_stale {
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
                            injected_tab: None,
                        });
                    }
                }
            }
        }
    }

    // Fallback: return the most recently modified session that satisfies
    // the `since` filter. `list_sessions` already sorts newest-first, so
    // the first surviving entry is the freshest post-`since` session.
    match list_sessions(config_dir, project_path) {
        Ok(sessions) => {
            for session in sessions {
                if let Some(threshold) = since {
                    match parse_session_timestamp(&session.last_modified) {
                        Some(parsed) if parsed > threshold => return Some(session),
                        // Drop entries that don't parse — better to miss
                        // than to bind the wrong session.
                        _ => continue,
                    }
                } else {
                    return Some(session);
                }
            }
            None
        }
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

    // Fallback: date-based name. Accepts both the legacy second-precision
    // and the current millisecond-precision (`%.3f`) formats — see the
    // formatter comment in `list_sessions`.
    if !last_modified.is_empty() {
        let iso = last_modified.trim_end_matches('Z');
        let parsed = chrono::NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S%.3f")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S"));
        if let Ok(dt) = parsed {
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
            // Millisecond precision: see comment in `list_sessions`.
            chrono::DateTime::<chrono::Utc>::from(t)
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
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
    fn extract_cwd_reads_first_record_with_cwd() {
        // Leading queue-operation record has NO cwd; the user record does.
        let content = "\
{\"type\":\"queue-operation\",\"timestamp\":\"2026-06-14T06:49:11.888Z\"}
{\"type\":\"user\",\"cwd\":\"C:\\\\Users\\\\jspin\\\\proj\",\"timestamp\":\"2026-06-14T06:49:11.902Z\",\"message\":{\"content\":\"hi\"}}";
        assert_eq!(extract_cwd(content).as_deref(), Some("C:\\Users\\jspin\\proj"));
        // A transcript with no cwd anywhere → None.
        assert_eq!(
            extract_cwd("{\"type\":\"summary\",\"summary\":\"x\"}"),
            None
        );
    }

    /// The full projects-tree scan finds a recently-active, real user
    /// transcript across an UNKNOWN project dir, recovers its real cwd, and
    /// stamps the config dir as the account — while skipping workflow sessions
    /// and cwd-less transcripts (session-restore-redesign Phase 3 / G3).
    #[test]
    fn list_recent_sessions_all_projects_finds_and_filters() {
        let cfg = tempfile::tempdir().unwrap();
        let cfg_dir = cfg.path();
        let proj = cfg_dir.join("projects").join("C--Users-jspin-proj");
        fs::create_dir_all(&proj).unwrap();

        // A real interactive session with a recoverable cwd.
        fs::write(
            proj.join("real-sess.jsonl"),
            "{\"type\":\"user\",\"cwd\":\"C:/Users/jspin/proj\",\"timestamp\":\"2026-06-14T06:49:11.902Z\",\"message\":{\"content\":\"do the thing\"}}\n",
        )
        .unwrap();
        // A runner-spawned workflow session — must be skipped.
        fs::write(
            proj.join("workflow.jsonl"),
            "{\"type\":\"queue-operation\",\"cwd\":\"C:/Users/jspin/proj\"}\n",
        )
        .unwrap();
        // A transcript with no recoverable cwd — must be skipped.
        fs::write(
            proj.join("no-cwd.jsonl"),
            "{\"type\":\"user\",\"timestamp\":\"2026-06-14T06:49:11.902Z\",\"message\":{\"content\":\"x\"}}\n",
        )
        .unwrap();

        let now = chrono::Utc::now().timestamp_millis();
        // Wide window so the fresh temp files (mtime≈now) are in-window.
        let out = list_recent_sessions_all_projects(cfg_dir, now, 24 * 60 * 60 * 1000);

        assert_eq!(out.len(), 1, "only the real user session is offered");
        let s = &out[0];
        assert_eq!(s.session_id, "real-sess");
        assert_eq!(s.working_dir, "C:/Users/jspin/proj", "real cwd recovered");
        assert_eq!(
            s.config_dir,
            cfg_dir.to_string_lossy(),
            "account = the holding config dir"
        );

        // A tiny window (0ms) excludes the fresh files entirely.
        let none = list_recent_sessions_all_projects(cfg_dir, now, 0);
        // The files' mtime is ~now so age is ~0; 0-window admits only age<=0.
        // Assert the scan does not PANIC and returns <= the wide-window result.
        assert!(none.len() <= 1);
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

    // ── Touched-File Extraction tests (Phase 1.5) ────────────────────────────

    #[test]
    fn test_extract_touched_files_single_edit() {
        // Real-shape JSONL fixture with one `Edit` tool_use.
        let record = serde_json::json!({
            "type": "assistant",
            "uuid": "asst-1",
            "timestamp": "2026-05-09T12:34:56.789Z",
            "message": {
                "model": "claude-opus-4-7",
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "I'll edit the file."},
                    {
                        "type": "tool_use",
                        "id": "tool-1",
                        "name": "Edit",
                        "input": {
                            "file_path": "D:/qontinui-root/foo.rs",
                            "old_string": "fn old()",
                            "new_string": "fn new()"
                        }
                    }
                ]
            }
        });
        let touched = extract_touched_files_from_assistant_record("sess-A", &record);
        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].session_id, "sess-A");
        assert_eq!(touched[0].file_path, "D:/qontinui-root/foo.rs");
        assert_eq!(touched[0].tool, ToolKind::Edit);
        // Timestamp parsed from ISO-8601 is non-zero and matches what
        // chrono produces — exact value is brittle across leap-second
        // tables, so just bound it. The fixture is mid-2026.
        let ms = touched[0].recorded_at_ms;
        let approx_2026_min = 1_700_000_000_000u64; // late 2023
        let approx_2030_max = 1_900_000_000_000u64; // 2030
        assert!(
            ms > approx_2026_min && ms < approx_2030_max,
            "expected mid-2020s timestamp ms, got {}",
            ms
        );
    }

    #[test]
    fn test_extract_touched_files_unknown_tool_skipped() {
        let record = serde_json::json!({
            "type": "assistant",
            "uuid": "asst-2",
            "timestamp": "2026-05-09T12:34:56Z",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tool-2",
                        "name": "Read",
                        "input": {"file_path": "/should/not/track.rs"}
                    }
                ]
            }
        });
        let touched = extract_touched_files_from_assistant_record("sess-B", &record);
        assert!(
            touched.is_empty(),
            "Read tool must NOT produce a TouchedFile"
        );
    }

    #[test]
    fn test_extract_touched_files_missing_file_path_silent() {
        let record = serde_json::json!({
            "type": "assistant",
            "uuid": "asst-3",
            "timestamp": "2026-05-09T12:34:56Z",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tool-3",
                        "name": "Edit",
                        "input": {
                            "old_string": "x",
                            "new_string": "y"
                            // file_path missing
                        }
                    }
                ]
            }
        });
        let touched = extract_touched_files_from_assistant_record("sess-C", &record);
        assert!(
            touched.is_empty(),
            "Missing file_path must produce empty result, not error"
        );
    }

    #[test]
    fn test_parse_line_malformed_json_returns_err() {
        let bad = r#"{"type": "assistant", "broken": "#;
        let result = parse_line_for_touched_files("sess-D", bad);
        assert!(matches!(result, Err(ParseError::MalformedJson(_))));
    }

    #[test]
    fn test_extract_touched_files_multiedit_one_per_call() {
        // MultiEdit with 5 inner edits to the same file → 1 TouchedFile.
        let record = serde_json::json!({
            "type": "assistant",
            "uuid": "asst-4",
            "timestamp": "2026-05-09T12:34:56Z",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tool-4",
                        "name": "MultiEdit",
                        "input": {
                            "file_path": "/tmp/multi.rs",
                            "edits": [
                                {"old_string": "a", "new_string": "1"},
                                {"old_string": "b", "new_string": "2"},
                                {"old_string": "c", "new_string": "3"},
                                {"old_string": "d", "new_string": "4"},
                                {"old_string": "e", "new_string": "5"}
                            ]
                        }
                    }
                ]
            }
        });
        let touched = extract_touched_files_from_assistant_record("sess-E", &record);
        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].file_path, "/tmp/multi.rs");
        assert_eq!(touched[0].tool, ToolKind::MultiEdit);
    }

    #[test]
    fn test_extract_touched_files_mixed_blocks() {
        // Read + Edit blocks → exactly 1 TouchedFile (the Edit).
        let record = serde_json::json!({
            "type": "assistant",
            "uuid": "asst-5",
            "timestamp": "2026-05-09T12:34:56Z",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tool-5a",
                        "name": "Read",
                        "input": {"file_path": "/skip/me.rs"}
                    },
                    {
                        "type": "tool_use",
                        "id": "tool-5b",
                        "name": "Edit",
                        "input": {
                            "file_path": "/keep/me.rs",
                            "old_string": "x",
                            "new_string": "y"
                        }
                    }
                ]
            }
        });
        let touched = extract_touched_files_from_assistant_record("sess-F", &record);
        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].file_path, "/keep/me.rs");
        assert_eq!(touched[0].tool, ToolKind::Edit);
    }

    #[test]
    fn test_parse_line_for_touched_files_non_assistant_returns_empty() {
        // user records produce no touched files even if they reference paths.
        let line = r#"{"type":"user","uuid":"u1","timestamp":"2026-05-09T00:00:00Z","message":{"role":"user","content":"please edit foo.rs"}}"#;
        let result = parse_line_for_touched_files("sess-G", line).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_line_for_touched_files_empty_line_ok() {
        // Trailing/empty lines must not error.
        assert!(parse_line_for_touched_files("sess-H", "")
            .unwrap()
            .is_empty());
        assert!(parse_line_for_touched_files("sess-H", "   ")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_parse_line_for_touched_files_full_pipeline_write() {
        // End-to-end: full JSONL line → TouchedFile.
        let line = r#"{"type":"assistant","uuid":"a-9","timestamp":"2026-05-09T01:02:03Z","message":{"model":"claude-opus-4-7","role":"assistant","content":[{"type":"tool_use","id":"t","name":"Write","input":{"file_path":"/new/file.rs","content":"fn main(){}"}}]}}"#;
        let touched = parse_line_for_touched_files("sess-I", line).unwrap();
        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].tool, ToolKind::Write);
        assert_eq!(touched[0].file_path, "/new/file.rs");
        assert_eq!(touched[0].session_id, "sess-I");
    }

    // ── Agent-log extraction (Phase 2) ───────────────────────────────────────

    #[test]
    fn test_parse_line_for_agent_log_assistant_and_tool_use_in_order() {
        // An assistant record carrying a text block followed by a tool_use
        // block yields both observations in document order.
        let line = r#"{"type":"assistant","uuid":"a-1","timestamp":"2026-05-09T01:02:03Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading the file now."},{"type":"tool_use","id":"t","name":"Read","input":{"file_path":"/a/b.rs"}}]}}"#;
        let obs = parse_line_for_agent_log(line);
        assert_eq!(
            obs,
            vec![
                AgentLogObs::Assistant {
                    text: "Reading the file now.".to_string()
                },
                AgentLogObs::ToolUse {
                    tool: "Read".to_string(),
                    input: Some(r#"{"file_path":"/a/b.rs"}"#.to_string()),
                },
            ]
        );
    }

    #[test]
    fn test_parse_line_for_agent_log_skips_user_lines() {
        // A `user` record (and other non-assistant noise) produces no
        // observations — the watcher only streams assistant activity.
        let user = r#"{"type":"user","uuid":"u1","timestamp":"2026-05-09T00:00:01Z","message":{"role":"user","content":"hi"}}"#;
        assert!(parse_line_for_agent_log(user).is_empty());

        // Empty / malformed lines are tolerated (no panic, no output).
        assert!(parse_line_for_agent_log("").is_empty());
        assert!(parse_line_for_agent_log("   ").is_empty());
        assert!(parse_line_for_agent_log("{not json").is_empty());
    }

    #[test]
    fn test_parse_line_for_agent_log_skips_thinking_and_empty_text() {
        // `thinking` blocks and blank text blocks are skipped; a tool_use with
        // no input emits a ToolUse with `input: None`.
        let line = r#"{"type":"assistant","uuid":"a-2","timestamp":"2026-05-09T01:02:03Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"   "},{"type":"tool_use","id":"t","name":"TodoWrite"}]}}"#;
        let obs = parse_line_for_agent_log(line);
        assert_eq!(
            obs,
            vec![AgentLogObs::ToolUse {
                tool: "TodoWrite".to_string(),
                input: None,
            }]
        );
    }

    #[test]
    fn test_truncate_for_log_caps_oversized_text() {
        let big = "x".repeat(AGENT_LOG_TEXT_CAP + 100);
        let out = truncate_for_log(&big);
        assert!(out.len() < big.len());
        assert!(out.ends_with("…[truncated]"));
        // Small text is passed through verbatim.
        assert_eq!(truncate_for_log("small"), "small");
    }

    // ── get_latest_session_id `since` filter (Phase 1.5) ─────────────────────
    //
    // These cover the two new behaviours added by
    // `plans/traffic-light-session-id-followups.md` Phase 1:
    // - the mtime-sorted fallback drops sessions whose mtime is `<= since`
    // - the `.claude.json` shortcut falls through (instead of early-returning)
    //   when its session's mtime is `<= since`

    /// One JSONL record with a single user message — enough for `list_sessions`
    /// to consider the session non-empty (`message_count == 1`) and produce a
    /// real `TranscriptSession`. Workflow-marker free.
    fn minimal_user_jsonl() -> &'static str {
        r#"{"type":"user","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"hello"}}"#
    }

    /// Write a synthetic JSONL session at `<config_dir>/projects/<encoded>/<id>.jsonl`
    /// and force its mtime via `filetime::set_file_mtime`. Returns the file path.
    fn write_session_with_mtime(
        config_dir: &Path,
        project_path: &str,
        session_id: &str,
        mtime: chrono::DateTime<chrono::Utc>,
    ) -> PathBuf {
        let encoded = encode_project_path(project_path);
        let project_dir = config_dir.join("projects").join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        let file_path = project_dir.join(format!("{}.jsonl", session_id));
        std::fs::write(&file_path, minimal_user_jsonl()).unwrap();

        // Preserve sub-second precision — the clock-skew boundary test needs
        // mtime resolution finer than 1 second.
        let ft =
            filetime::FileTime::from_unix_time(mtime.timestamp(), mtime.timestamp_subsec_nanos());
        filetime::set_file_mtime(&file_path, ft).unwrap();
        file_path
    }

    /// Clear cached entries that may shadow our fixture mtimes. The shared
    /// `SESSION_CACHE` is process-wide and other tests in this binary touch
    /// it; if a stale entry lingers under our tempdir's prefix, `list_sessions`
    /// can return a cached `TranscriptSession` whose `last_modified` doesn't
    /// match the freshly-set fixture mtime.
    fn clear_cache_under(prefix: &Path) {
        let mut cache = SESSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|p, _| !p.starts_with(prefix));
    }

    #[test]
    fn get_latest_session_id_filters_by_since() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().to_path_buf();
        // Use a project path that won't have a real .claude.json sitting on
        // disk — the lookup below would otherwise read the user's actual
        // workspace `.claude.json` and pick a foreign session.
        let project_path = temp
            .path()
            .join("fake_project")
            .to_string_lossy()
            .to_string();
        std::fs::create_dir_all(temp.path().join("fake_project")).unwrap();

        let before = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let after = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_001_000, 0).unwrap();
        let since = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_500, 0).unwrap();

        let _ = write_session_with_mtime(&config_dir, &project_path, "old-session", before);
        let _ = write_session_with_mtime(&config_dir, &project_path, "new-session", after);
        clear_cache_under(temp.path());

        // No filter → freshest wins.
        let latest = get_latest_session_id(&config_dir, &project_path, None).unwrap();
        assert_eq!(latest.session_id, "new-session");

        // Filter excludes old-session, keeps new-session.
        let filtered = get_latest_session_id(&config_dir, &project_path, Some(since)).unwrap();
        assert_eq!(filtered.session_id, "new-session");

        // Filter excludes BOTH → None.
        let strict = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_002_000, 0).unwrap();
        assert!(get_latest_session_id(&config_dir, &project_path, Some(strict)).is_none());
    }

    #[test]
    fn get_latest_session_id_skips_claude_json_when_stale() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().to_path_buf();
        let project_dir = temp.path().join("fake_project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let project_path = project_dir.to_string_lossy().to_string();

        let before = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let after = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_001_000, 0).unwrap();
        let since = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_500, 0).unwrap();

        let _ = write_session_with_mtime(&config_dir, &project_path, "old-session", before);
        let _ = write_session_with_mtime(&config_dir, &project_path, "new-session", after);
        clear_cache_under(temp.path());

        // Point .claude.json's lastSessionId at the OLDER (pre-since) session
        // — this is the shadowing case the plan calls out.
        let claude_json = project_dir.join(".claude.json");
        std::fs::write(
            &claude_json,
            serde_json::json!({ "lastSessionId": "old-session" }).to_string(),
        )
        .unwrap();

        // Without `since` → shortcut wins, returns old-session as-is.
        let no_filter = get_latest_session_id(&config_dir, &project_path, None).unwrap();
        assert_eq!(
            no_filter.session_id, "old-session",
            "without `since`, .claude.json shortcut should win"
        );

        // With `since` past old-session's mtime → shortcut falls through,
        // mtime fallback returns new-session.
        let filtered = get_latest_session_id(&config_dir, &project_path, Some(since)).unwrap();
        assert_eq!(
            filtered.session_id, "new-session",
            ".claude.json shortcut must NOT shadow a fresher session when stale"
        );
    }

    /// Regression: a hook spawn at `t = T.345` (millis) and a Claude write at
    /// `t = T.789` BOTH truncate to the same wall-clock second `T`. With
    /// second-precision timestamps, the strict `parsed > since` filter would
    /// drop the legitimate fresh session. With millisecond-precision
    /// timestamps the filter accepts it.
    ///
    /// Verifies the formatter at `list_sessions` emits `%.3fZ` so that the
    /// JSONL mtime, parsed back, retains enough precision to win the strict
    /// `>` comparison against `Date.now()`-derived spawn timestamps.
    #[test]
    fn get_latest_session_id_clock_skew_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().to_path_buf();
        let project_dir = temp.path().join("fake_project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let project_path = project_dir.to_string_lossy().to_string();

        // Spawn at .345s, JSONL mtime at .789s of the same wall-clock second.
        let spawn =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_700_000_000_345).unwrap();
        let mtime =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_700_000_000_789).unwrap();

        let _ = write_session_with_mtime(&config_dir, &project_path, "fresh-session", mtime);
        clear_cache_under(temp.path());

        let result = get_latest_session_id(&config_dir, &project_path, Some(spawn));
        assert!(
            result.is_some(),
            "session whose mtime is in the same second as `since` (789ms vs 345ms) \
             must NOT be dropped — that's the common hook-spawn case"
        );
        assert_eq!(result.unwrap().session_id, "fresh-session");
    }
}
