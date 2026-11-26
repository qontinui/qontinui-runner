//! Project-based log management commands
//!
//! Projects reference global log sources (from Settings > Log Sources) instead
//! of embedding their own copies. The project config stores only source IDs
//! and/or a global profile reference.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

use crate::error::AppError;
use crate::settings::{self, GlobalLogSource};
use qontinui_runner_lib::file_tail;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

use super::CommandResponse;

/// Project-specific log configuration (slim — references global sources)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectLogConfig {
    /// Project identifier
    pub project_id: String,
    /// Project name
    pub project_name: String,
    /// ID of the global profile to use, or None for "all enabled"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_profile_id: Option<String>,
    /// Selected global source IDs (overrides profile when non-empty)
    #[serde(default)]
    pub selected_source_ids: Vec<String>,
    /// Directory for runner's logs
    pub log_directory: String,
    /// Directory for screenshots
    pub screenshot_directory: String,
    /// Directory for AI output
    pub ai_output_directory: String,
    /// Last updated timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Content read from a log source: a bounded tail of its file, never the whole
/// file ([`file_tail::tail_lines`], at most [`file_tail::LOG_TAIL_CAP`] bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSourceContent {
    /// Source ID
    pub source_id: String,
    /// Source name
    pub source_name: String,
    /// The file's last lines, oldest first — at most the source's `tail_lines`
    pub lines: Vec<String>,
    /// The file's line count, `Some` ONLY when the read covered the whole file.
    /// A tail of a larger file is not counted (counting reads every byte, and
    /// a day log reaches 2.9 GB), so `None` is "not counted", never zero.
    pub total_lines: Option<u64>,
    /// The file's size in bytes when it was read; `None` when it was not read
    pub file_bytes: Option<u64>,
    /// The file actually read: the configured path, or the newest dated
    /// rotation of it when that path is absent (see [`resolve_source_file`])
    pub file_path: String,
    /// Last modified time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// Error if reading failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// =============================================================================
// Source Resolution
// =============================================================================

/// Resolve which global sources this project should use
fn resolve_project_sources(config: &ProjectLogConfig) -> Vec<GlobalLogSource> {
    let global_settings = settings::get_global_log_source_settings();

    if !config.selected_source_ids.is_empty() {
        // Use explicitly selected sources
        global_settings
            .sources
            .iter()
            .filter(|s| config.selected_source_ids.contains(&s.id) && s.enabled)
            .cloned()
            .collect()
    } else if let Some(profile_id) = &config.global_profile_id {
        // Use profile's sources
        if let Some(profile) = global_settings
            .profiles
            .iter()
            .find(|p| p.id == *profile_id)
        {
            global_settings
                .sources
                .iter()
                .filter(|s| profile.source_ids.contains(&s.id) && s.enabled)
                .cloned()
                .collect()
        } else {
            // Profile not found, fall back to all enabled
            global_settings
                .sources
                .iter()
                .filter(|s| s.enabled)
                .cloned()
                .collect()
        }
    } else {
        // No selection, use all enabled
        global_settings
            .sources
            .iter()
            .filter(|s| s.enabled)
            .cloned()
            .collect()
    }
}

// =============================================================================
// File system helpers
// =============================================================================

/// Get the base directory for all project configs
fn get_projects_base_dir() -> Result<PathBuf, String> {
    let base = qontinui_runner_lib::ambient::qontinui_dir()
        .ok_or("Failed to get home directory")?
        .join("projects");

    // Create if doesn't exist
    if !base.exists() {
        fs::create_dir_all(&base).map_err(|e| {
            String::from(AppError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to create projects directory: {}", e),
            )))
        })?;
    }

    Ok(base)
}

/// Get the directory for a specific project
fn get_project_dir(project_id: &str) -> Result<PathBuf, String> {
    let base = get_projects_base_dir()?;
    let project_dir = base.join(project_id);

    // Create if doesn't exist
    if !project_dir.exists() {
        fs::create_dir_all(&project_dir).map_err(|e| {
            String::from(AppError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to create project directory: {}", e),
            )))
        })?;
    }

    Ok(project_dir)
}

/// Get the config file path for a project
fn get_project_config_path(project_id: &str) -> Result<PathBuf, String> {
    let project_dir = get_project_dir(project_id)?;
    Ok(project_dir.join("config.json"))
}

/// Load project log config from file, auto-migrating old format if needed
fn load_project_config(project_id: &str) -> Result<Option<ProjectLogConfig>, String> {
    let config_path = get_project_config_path(project_id)?;

    if !config_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&config_path).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to read project config: {}", e),
        )))
    })?;

    // First, try parsing as the new slim format
    if let Ok(config) = serde_json::from_str::<ProjectLogConfig>(&contents) {
        return Ok(Some(config));
    }

    // If that fails, try parsing as old format and migrate
    if let Ok(old) = serde_json::from_str::<serde_json::Value>(&contents) {
        info!(
            "Detected old-format project config for '{}', migrating to global source references",
            project_id
        );
        let migrated = migrate_old_config(&old, project_id)?;
        // Save migrated config
        if let Err(e) = save_project_config(&migrated) {
            warn!("Failed to save migrated config: {}", e);
        }
        return Ok(Some(migrated));
    }

    Err("Failed to parse project config in any known format".to_string())
}

/// Migrate an old-format config (with embedded profiles/log_sources) to the new slim format
fn migrate_old_config(
    old: &serde_json::Value,
    project_id: &str,
) -> Result<ProjectLogConfig, String> {
    let project_name = old
        .get("project_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    // Collect all embedded sources from old format
    let mut embedded_sources: Vec<serde_json::Value> = Vec::new();

    // Check profiles first
    if let Some(profiles) = old.get("profiles").and_then(|p| p.as_array()) {
        for profile in profiles {
            if let Some(sources) = profile.get("log_sources").and_then(|s| s.as_array()) {
                embedded_sources.extend(sources.iter().cloned());
            }
        }
    }

    // Check legacy log_sources
    if let Some(sources) = old.get("log_sources").and_then(|s| s.as_array()) {
        embedded_sources.extend(sources.iter().cloned());
    }

    // Match embedded sources to global sources by path, or create new global sources
    let mut global_settings = settings::get_global_log_source_settings();
    let mut selected_ids: Vec<String> = Vec::new();
    let mut new_sources_added = false;

    for source in &embedded_sources {
        let path = match source.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => continue,
        };

        // Try to find a matching global source by path
        if let Some(existing) = global_settings
            .sources
            .iter()
            .find(|s| s.path.eq_ignore_ascii_case(path))
        {
            if !selected_ids.contains(&existing.id) {
                selected_ids.push(existing.id.clone());
            }
        } else {
            // Create a new global source from the embedded data
            let name = source
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("Migrated Source")
                .to_string();
            let source_type = source
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("file")
                .to_string();
            let tail_lines = source
                .get("tail_lines")
                .and_then(|t| t.as_u64())
                .unwrap_or(100) as u32;
            let enabled = source
                .get("enabled")
                .and_then(|e| e.as_bool())
                .unwrap_or(true);
            let color = source
                .get("color")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let pattern = source
                .get("pattern")
                .and_then(|p| p.as_str())
                .map(|s| s.to_string());

            let new_id = format!("source-{}", uuid::Uuid::new_v4());
            let global_source = GlobalLogSource {
                id: new_id.clone(),
                name: name.clone(),
                description: format!("Migrated from project '{}'", project_name),
                category: crate::commands::global_log_sources::infer_category_from_name(&name),
                source_type,
                path: path.to_string(),
                pattern,
                tail_lines,
                enabled,
                color,
                keywords: Vec::new(),
                format: "plaintext".to_string(),
                parser: "generic".to_string(),
                timestamp_pattern: None,
                timezone: "local".to_string(),
                error_patterns: vec![],
                warning_patterns: vec![],
                ignore_patterns: vec![],
                poll_interval_ms: 5000,
            };

            global_settings.sources.push(global_source);
            selected_ids.push(new_id);
            new_sources_added = true;
        }
    }

    // Save updated global settings if we added new sources
    if new_sources_added {
        if let Err(e) = settings::save_global_log_source_settings(global_settings) {
            warn!("Failed to save global settings during migration: {}", e);
        }
    }

    // Build the new slim config
    let log_directory = old
        .get("log_directory")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let screenshot_directory = old
        .get("screenshot_directory")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ai_output_directory = old
        .get("ai_output_directory")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    info!(
        "Migrated project '{}': {} embedded sources -> {} global source references",
        project_name,
        embedded_sources.len(),
        selected_ids.len()
    );

    Ok(ProjectLogConfig {
        project_id: project_id.to_string(),
        project_name,
        global_profile_id: None,
        selected_source_ids: selected_ids,
        log_directory,
        screenshot_directory,
        ai_output_directory,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
    })
}

/// Save project log config to file
fn save_project_config(config: &ProjectLogConfig) -> Result<(), String> {
    let config_path = get_project_config_path(&config.project_id)?;

    let contents = serde_json::to_string_pretty(config).map_err(|e| {
        String::from(AppError::ParseError(format!(
            "Failed to serialize project config: {}",
            e
        )))
    })?;

    fs::write(&config_path, contents).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to write project config: {}", e),
        )))
    })?;

    info!(
        "Saved project config for '{}' to {:?}",
        config.project_name, config_path
    );

    Ok(())
}

/// Get file modification time as ISO string
fn get_file_modified_time(path: &PathBuf) -> Option<String> {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let datetime: chrono::DateTime<chrono::Utc> = t.into();
            datetime.to_rfc3339()
        })
}

/// The file a source's configured `path` names.
///
/// Rotation-aware: a daily-rolling sink (e.g. the runner's own
/// `qontinui-runner.log`) never writes the undated basename — the live file is
/// `<basename>.<YYYY-MM-DD>`. When the configured literal path is absent, this
/// resolves the newest matching dated sibling, so a source pointed at the
/// stable basename reads the current rotation instead of reporting "File does
/// not exist". A literal path that exists always wins.
///
/// `pub(crate)` for the `/debug/app/errors` scan, which reads the same
/// configured sources and must resolve them the same way.
pub(crate) fn resolve_source_file(configured: impl AsRef<Path>) -> PathBuf {
    let path = configured.as_ref().to_path_buf();
    if !path.exists() {
        if let (Some(dir), Some(prefix)) =
            (path.parent(), path.file_name().and_then(|n| n.to_str()))
        {
            if let Some(newest) = crate::paths::resolve_newest_rolling_file(dir, prefix) {
                return newest;
            }
        }
    }
    path
}

/// Read one global source: resolve its file ([`resolve_source_file`]), then
/// take one bounded tail of it. Blocking file I/O — call it off the main
/// thread and off the async executor.
fn read_global_source(source: &GlobalLogSource) -> LogSourceContent {
    let path = resolve_source_file(&source.path);
    let mut content = LogSourceContent {
        source_id: source.id.clone(),
        source_name: source.name.clone(),
        lines: Vec::new(),
        total_lines: None,
        file_bytes: None,
        // The file actually read (it may be the resolved dated rotation), not
        // the configured basename, so a caller can `tail -f` the right file.
        file_path: path.to_string_lossy().into_owned(),
        last_modified: None,
        error: None,
    };

    if !path.exists() {
        content.error = Some("File does not exist".to_string());
        return content;
    }
    content.last_modified = get_file_modified_time(&path);

    match file_tail::tail_lines(&path, source.tail_lines as usize, file_tail::LOG_TAIL_CAP) {
        Ok(tail) => {
            content.lines = tail.lines;
            content.total_lines = tail.total_lines;
            content.file_bytes = Some(tail.file_bytes);
        }
        Err(e) => {
            content.error = Some(format!("Failed to read file {}: {}", path.display(), e));
        }
    }
    content
}

/// Run a log read on a blocking thread and await it. The read commands are
/// `async` for this: as sync commands Tauri ran them on the main thread, where
/// a slow or large read froze the window (plan
/// `2026-11-11-runner-glibc-arena-retention-and-whole-log-tail` Phase 2). A
/// read that panics answers as a failure rather than dropping the IPC call.
async fn read_off_main_thread<F>(read: F) -> CommandResponse
where
    F: FnOnce() -> CommandResponse + Send + 'static,
{
    spawn_blocking_tracked(read)
        .await
        .unwrap_or_else(|e| CommandResponse {
            success: false,
            message: Some(format!("Log read failed: {e}")),
            data: None,
        })
}

// =============================================================================
// Tauri Commands
// =============================================================================

/// Get project log configuration
#[tauri::command]
pub fn get_project_log_config(project_id: String) -> CommandResponse {
    match load_project_config(&project_id) {
        Ok(Some(config)) => CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::to_value(config).unwrap_or_default()),
        },
        Ok(None) => CommandResponse {
            success: true,
            message: Some("No config found for this project".to_string()),
            data: None,
        },
        Err(e) => {
            error!("Failed to load project config: {}", e);
            CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }
        }
    }
}

/// Save project log configuration
#[tauri::command]
pub fn save_project_log_config(config: ProjectLogConfig) -> CommandResponse {
    // Ensure project directories exist
    let project_dir = match get_project_dir(&config.project_id) {
        Ok(dir) => dir,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }
        }
    };

    // Create subdirectories
    let log_dir = project_dir.join("logs");
    let screenshot_dir = project_dir.join("screenshots");
    let ai_output_dir = project_dir.join("ai-output");

    for dir in [&log_dir, &screenshot_dir, &ai_output_dir] {
        if let Err(e) = fs::create_dir_all(dir) {
            warn!("Failed to create directory {:?}: {}", dir, e);
        }
    }

    // Update config with actual paths
    let mut config = config;
    config.log_directory = log_dir.to_string_lossy().to_string();
    config.screenshot_directory = screenshot_dir.to_string_lossy().to_string();
    config.ai_output_directory = ai_output_dir.to_string_lossy().to_string();
    config.updated_at = Some(chrono::Utc::now().to_rfc3339());

    match save_project_config(&config) {
        Ok(()) => CommandResponse {
            success: true,
            message: Some("Project config saved".to_string()),
            data: Some(serde_json::to_value(&config).unwrap_or_default()),
        },
        Err(e) => {
            error!("Failed to save project config: {}", e);
            CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }
        }
    }
}

/// List all project configurations (internal, returns Result)
pub fn list_project_configs_internal() -> Result<Vec<ProjectLogConfig>, String> {
    let base_dir = get_projects_base_dir()?;
    let mut configs: Vec<ProjectLogConfig> = Vec::new();

    if let Ok(entries) = fs::read_dir(&base_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(project_id) = entry.file_name().to_str() {
                    if let Ok(Some(config)) = load_project_config(project_id) {
                        configs.push(config);
                    }
                }
            }
        }
    }

    Ok(configs)
}

/// List all project configurations
#[tauri::command]
pub fn list_project_configs() -> CommandResponse {
    match list_project_configs_internal() {
        Ok(configs) => CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::to_value(configs).unwrap_or_default()),
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        },
    }
}

/// Delete a project configuration
#[tauri::command]
pub fn delete_project_config(project_id: String) -> CommandResponse {
    let project_dir = match get_project_dir(&project_id) {
        Ok(dir) => dir,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }
        }
    };

    if project_dir.exists() {
        if let Err(e) = fs::remove_dir_all(&project_dir) {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to delete project directory: {}", e)),
                data: None,
            };
        }
    }

    CommandResponse {
        success: true,
        message: Some("Project config deleted".to_string()),
        data: None,
    }
}

/// Read content from a single log source by global source ID.
///
/// `async`, with the body on a blocking thread ([`read_off_main_thread`]). The
/// command name and its JS contract are unchanged.
#[tauri::command]
pub async fn read_log_source(source_id: String) -> CommandResponse {
    read_off_main_thread(move || read_log_source_blocking(&source_id)).await
}

fn read_log_source_blocking(source_id: &str) -> CommandResponse {
    let global_settings = settings::get_global_log_source_settings();

    let source = match global_settings.sources.iter().find(|s| s.id == source_id) {
        Some(s) => s,
        None => {
            return CommandResponse {
                success: false,
                message: Some(format!("Global source '{}' not found", source_id)),
                data: None,
            }
        }
    };

    let content = read_global_source(source);

    CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::to_value(content).unwrap_or_default()),
    }
}

/// Read content from all resolved log sources for a project.
///
/// `async`, with the body on a blocking thread ([`read_off_main_thread`]). The
/// command name and its JS contract are unchanged.
#[tauri::command]
pub async fn read_project_logs(project_id: String) -> CommandResponse {
    read_off_main_thread(move || read_project_logs_blocking(&project_id)).await
}

fn read_project_logs_blocking(project_id: &str) -> CommandResponse {
    let config = match load_project_config(project_id) {
        Ok(Some(config)) => config,
        Ok(None) => {
            return CommandResponse {
                success: false,
                message: Some("Project config not found".to_string()),
                data: None,
            }
        }
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }
        }
    };

    let sources = resolve_project_sources(&config);
    let contents: Vec<LogSourceContent> = sources.iter().map(read_global_source).collect();

    CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::to_value(contents).unwrap_or_default()),
    }
}

/// Get the project directories (logs, screenshots, ai-output)
#[tauri::command]
pub fn get_project_directories(project_id: String) -> CommandResponse {
    let project_dir = match get_project_dir(&project_id) {
        Ok(dir) => dir,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }
        }
    };

    CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "base": project_dir.to_string_lossy(),
            "logs": project_dir.join("logs").to_string_lossy(),
            "screenshots": project_dir.join("screenshots").to_string_lossy(),
            "ai_output": project_dir.join("ai-output").to_string_lossy(),
        })),
    }
}

/// Append a log entry to the project's runner log
#[tauri::command]
pub fn append_project_log(
    project_id: String,
    level: String,
    message: String,
    source: Option<String>,
) -> CommandResponse {
    let project_dir = match get_project_dir(&project_id) {
        Ok(dir) => dir,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }
        }
    };

    let log_file = project_dir.join("logs").join("runner.log");

    // Ensure log directory exists
    if let Some(parent) = log_file.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to create log directory: {}", e)),
                data: None,
            };
        }
    }

    let timestamp = chrono::Utc::now().to_rfc3339();
    let source_str = source.unwrap_or_else(|| "runner".to_string());
    let log_line = format!(
        "[{}] [{}] [{}] {}\n",
        timestamp,
        level.to_uppercase(),
        source_str,
        message
    );

    let mut file = match OpenOptions::new().create(true).append(true).open(&log_file) {
        Ok(f) => f,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to open log file: {}", e)),
                data: None,
            }
        }
    };

    if let Err(e) = file.write_all(log_line.as_bytes()) {
        return CommandResponse {
            success: false,
            message: Some(format!("Failed to write log: {}", e)),
            data: None,
        };
    }

    CommandResponse {
        success: true,
        message: None,
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source with every unset field at its serde default — the shape the
    /// global settings file stores.
    fn source(path: &Path, tail_lines: u32) -> GlobalLogSource {
        serde_json::from_value(serde_json::json!({
            "id": "source-test",
            "name": "Test log",
            "description": "",
            "path": path.to_string_lossy(),
            "tail_lines": tail_lines,
        }))
        .expect("a minimal source deserializes")
    }

    fn write_lines(path: &Path, prefix: &str, count: usize) {
        let text: String = (0..count).map(|i| format!("{prefix} {i}\n")).collect();
        std::fs::write(path, text).unwrap();
    }

    fn set_mtime(path: &Path, unix_secs: i64) {
        filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(unix_secs, 0)).unwrap();
    }

    /// A rolling sink never writes its undated basename. A source pointed at
    /// the basename reads the NEWEST dated rotation — the resolution this
    /// module took over from the deleted `read_global_log_sources` — and
    /// reports that file as the one read.
    #[test]
    fn a_rolling_basename_reads_its_newest_dated_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("app.log.2026-11-10");
        let newer = dir.path().join("app.log.2026-11-11");
        write_lines(&older, "old", 3);
        write_lines(&newer, "new", 3);
        set_mtime(&older, 1_790_000_000);
        set_mtime(&newer, 1_790_086_400);

        let content = read_global_source(&source(&dir.path().join("app.log"), 2));
        assert_eq!(content.error, None);
        assert_eq!(content.lines, ["new 1", "new 2"]);
        assert_eq!(content.total_lines, Some(3));
        assert_eq!(content.file_path, newer.to_string_lossy());
    }

    /// A configured path that exists is read as configured, even beside a
    /// newer rotation of it.
    #[test]
    fn a_literal_path_that_exists_wins_over_a_newer_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let literal = dir.path().join("app.log");
        let rotation = dir.path().join("app.log.2026-11-11");
        write_lines(&literal, "literal", 2);
        write_lines(&rotation, "rotated", 2);
        set_mtime(&literal, 1_790_000_000);
        set_mtime(&rotation, 1_790_086_400);

        let content = read_global_source(&source(&literal, 10));
        assert_eq!(content.lines, ["literal 0", "literal 1"]);
        assert_eq!(content.file_path, literal.to_string_lossy());
    }

    #[test]
    fn a_missing_source_reports_that_it_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.log");
        let content = read_global_source(&source(&path, 10));
        assert_eq!(content.error.as_deref(), Some("File does not exist"));
        assert!(content.lines.is_empty());
        assert_eq!(content.total_lines, None, "unread is not zero lines");
        assert_eq!(content.file_bytes, None);
        assert_eq!(content.file_path, path.to_string_lossy());
    }

    /// A log larger than the first window is tailed, not read whole: its size
    /// is reported, its line count is not.
    #[test]
    fn a_log_larger_than_the_first_window_is_tailed_without_a_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.log");
        write_lines(&path, "entry", 20_000);
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > file_tail::TAIL_FIRST_WINDOW);

        let content = read_global_source(&source(&path, 3));
        assert_eq!(content.error, None);
        assert_eq!(content.lines, ["entry 19997", "entry 19998", "entry 19999"]);
        assert_eq!(content.total_lines, None);
        assert_eq!(content.file_bytes, Some(size));
    }

    /// The wire shape the frontend's `RawLogContent` reads: `total_lines` and
    /// `file_bytes` are always present, and `null` when unknown — never
    /// omitted, never a 0 standing in for "unknown".
    #[test]
    fn unknown_sizes_serialize_as_null_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        let content = read_global_source(&source(&dir.path().join("absent.log"), 10));
        let value = serde_json::to_value(content).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.get("total_lines"), Some(&serde_json::Value::Null));
        assert_eq!(object.get("file_bytes"), Some(&serde_json::Value::Null));
    }

    /// Both read commands run off the main thread: each is an `async fn` whose
    /// future is `Send`, which Tauri spawns on its runtime instead of running
    /// it on the main thread. Constructed and never polled — polling would read
    /// the operator's settings.
    #[test]
    fn the_log_read_commands_are_async() {
        fn assert_send_future<F: std::future::Future<Output = CommandResponse> + Send>(_: F) {}
        assert_send_future(read_log_source(String::new()));
        assert_send_future(read_project_logs(String::new()));
    }

    /// The CODE of the production fn whose signature starts `header`: only
    /// the half above the test module is searched, so this test's own text
    /// (which names the needle) can never match; comments are removed and
    /// string and char literals emptied, so only call syntax can match.
    fn prod_fn_body(header: &str) -> String {
        let src = include_str!("project_logs.rs");
        let (prod, _) = src.split_once("#[cfg(test)]").expect("a test module");
        let (_, rest) = prod
            .split_once(header)
            .unwrap_or_else(|| panic!("`{header}` not found"));
        let (body, _) = rest.split_once("\n}\n").expect("fn closed at column 0");
        // Read through `process_helpers::code_lexer` (the lexer the lib's
        // source guards and `ci_node::executor`'s pins use), so a removed hop
        // cannot be satisfied by any comment or literal that still names it.
        use crate::process_helpers::code_lexer::{code_only_from, Lex};
        let mut lex = Lex::Code;
        body.lines()
            .map(|line| code_only_from(line, &mut lex))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Both read commands hand their body to `read_off_main_thread`, and that
    /// helper hops to the blocking pool, so the read never runs on a tokio
    /// worker or the main thread. `Send` alone (above) cannot see either hop
    /// removed.
    #[test]
    fn the_log_read_commands_read_on_the_blocking_pool() {
        for name in ["read_log_source", "read_project_logs"] {
            let body = prod_fn_body(&format!("pub async fn {name}("));
            assert!(
                body.contains("read_off_main_thread("),
                "{name} must read through read_off_main_thread"
            );
        }
        let helper = prod_fn_body("async fn read_off_main_thread<F>(");
        assert!(
            helper.contains("spawn_blocking_tracked("),
            "read_off_main_thread must hop to the blocking pool"
        );
    }
}
