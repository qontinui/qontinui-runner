//! Project-based log management commands
//!
//! Provides commands for managing project-specific log configurations,
//! reading external log files, and storing runner logs per project.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use tracing::{error, info, warn};

use super::CommandResponse;

/// A single external log source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSource {
    /// Unique identifier
    pub id: String,
    /// Human-readable name (e.g., "Frontend", "Backend")
    pub name: String,
    /// Type: "file" or "directory"
    #[serde(rename = "type")]
    pub source_type: String,
    /// Absolute path to log file or directory
    pub path: String,
    /// Glob pattern for directory type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Number of lines to tail (default: 100)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<u32>,
    /// Whether this source is enabled
    pub enabled: bool,
    /// Optional color for UI
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// A named profile containing a set of log sources
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSourceProfile {
    /// Unique identifier for this profile
    pub id: String,
    /// Human-readable name (e.g., "Qontinui Development", "Backend Only")
    pub name: String,
    /// Description of what this profile is for
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Log sources in this profile
    pub log_sources: Vec<LogSource>,
    /// When this profile was created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// When this profile was last modified
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl LogSourceProfile {
    /// Create a new profile with the given name and sources
    pub fn new(name: String, log_sources: Vec<LogSource>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: format!("profile-{}", uuid::Uuid::new_v4()),
            name,
            description: None,
            log_sources,
            created_at: Some(now.clone()),
            updated_at: Some(now),
        }
    }

    /// Create a default profile from legacy log_sources
    pub fn default_from_sources(log_sources: Vec<LogSource>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: "profile-default".to_string(),
            name: "Default".to_string(),
            description: Some("Migrated from legacy configuration".to_string()),
            log_sources,
            created_at: Some(now.clone()),
            updated_at: Some(now),
        }
    }
}

/// Project-specific log configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectLogConfig {
    /// Project identifier
    pub project_id: String,
    /// Project name
    pub project_name: String,
    /// Log source profiles (named configurations)
    #[serde(default)]
    pub profiles: Vec<LogSourceProfile>,
    /// ID of the currently active profile
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_profile_id: Option<String>,
    /// Legacy: External log sources (migrated to profiles on load)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_sources: Vec<LogSource>,
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

impl ProjectLogConfig {
    /// Get the active profile, or None if no profiles exist
    pub fn get_active_profile(&self) -> Option<&LogSourceProfile> {
        if let Some(active_id) = &self.active_profile_id {
            self.profiles.iter().find(|p| p.id == *active_id)
        } else {
            // Return first profile if no active set
            self.profiles.first()
        }
    }

    /// Get the active log sources (from active profile or legacy)
    pub fn get_active_log_sources(&self) -> &[LogSource] {
        if let Some(profile) = self.get_active_profile() {
            &profile.log_sources
        } else if !self.log_sources.is_empty() {
            // Legacy fallback
            &self.log_sources
        } else {
            &[]
        }
    }

    /// Migrate legacy log_sources to a profile if needed
    pub fn migrate_legacy_sources(&mut self) {
        if self.profiles.is_empty() && !self.log_sources.is_empty() {
            info!(
                "Migrating legacy log_sources to default profile for project '{}'",
                self.project_name
            );
            let default_profile =
                LogSourceProfile::default_from_sources(std::mem::take(&mut self.log_sources));
            let profile_id = default_profile.id.clone();
            self.profiles.push(default_profile);
            self.active_profile_id = Some(profile_id);
        }
    }
}

/// Content read from a log source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSourceContent {
    /// Source ID
    pub source_id: String,
    /// Source name
    pub source_name: String,
    /// Log lines
    pub lines: Vec<String>,
    /// Total line count
    pub total_lines: u64,
    /// File path that was read
    pub file_path: String,
    /// Last modified time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// Error if reading failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Get the base directory for all project configs
fn get_projects_base_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Failed to get home directory")?;
    let base = home.join(".qontinui").join("projects");

    // Create if doesn't exist
    if !base.exists() {
        fs::create_dir_all(&base)
            .map_err(|e| format!("Failed to create projects directory: {}", e))?;
    }

    Ok(base)
}

/// Get the directory for a specific project
fn get_project_dir(project_id: &str) -> Result<PathBuf, String> {
    let base = get_projects_base_dir()?;
    let project_dir = base.join(project_id);

    // Create if doesn't exist
    if !project_dir.exists() {
        fs::create_dir_all(&project_dir)
            .map_err(|e| format!("Failed to create project directory: {}", e))?;
    }

    Ok(project_dir)
}

/// Get the config file path for a project
fn get_project_config_path(project_id: &str) -> Result<PathBuf, String> {
    let project_dir = get_project_dir(project_id)?;
    Ok(project_dir.join("config.json"))
}

/// Load project log config from file (with automatic migration of legacy format)
fn load_project_config(project_id: &str) -> Result<Option<ProjectLogConfig>, String> {
    let config_path = get_project_config_path(project_id)?;

    if !config_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read project config: {}", e))?;

    let mut config: ProjectLogConfig = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse project config: {}", e))?;

    // Migrate legacy log_sources to profiles if needed
    let needs_migration = config.profiles.is_empty() && !config.log_sources.is_empty();
    config.migrate_legacy_sources();

    // Save migrated config
    if needs_migration {
        if let Err(e) = save_project_config(&config) {
            warn!("Failed to save migrated config: {}", e);
        }
    }

    Ok(Some(config))
}

/// Save project log config to file
fn save_project_config(config: &ProjectLogConfig) -> Result<(), String> {
    let config_path = get_project_config_path(&config.project_id)?;

    let contents = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize project config: {}", e))?;

    fs::write(&config_path, contents)
        .map_err(|e| format!("Failed to write project config: {}", e))?;

    info!(
        "Saved project config for '{}' to {:?}",
        config.project_name, config_path
    );

    Ok(())
}

/// Tail the last N lines from a file
fn tail_file(path: &PathBuf, num_lines: u32) -> Result<(Vec<String>, u64), String> {
    let file =
        File::open(path).map_err(|e| format!("Failed to open file {}: {}", path.display(), e))?;

    let reader = BufReader::new(&file);
    let all_lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
    let total_lines = all_lines.len() as u64;

    let start = if all_lines.len() > num_lines as usize {
        all_lines.len() - num_lines as usize
    } else {
        0
    };

    Ok((all_lines[start..].to_vec(), total_lines))
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

/// Read content from a single log source
#[tauri::command]
pub fn read_log_source(source: LogSource) -> CommandResponse {
    let path = PathBuf::from(&source.path);

    if !path.exists() {
        return CommandResponse {
            success: true,
            message: None,
            data: Some(
                serde_json::to_value(LogSourceContent {
                    source_id: source.id,
                    source_name: source.name,
                    lines: Vec::new(),
                    total_lines: 0,
                    file_path: source.path,
                    last_modified: None,
                    error: Some("File does not exist".to_string()),
                })
                .unwrap_or_default(),
            ),
        };
    }

    let tail_lines = source.tail_lines.unwrap_or(100);
    let last_modified = get_file_modified_time(&path);

    match tail_file(&path, tail_lines) {
        Ok((lines, total_lines)) => CommandResponse {
            success: true,
            message: None,
            data: Some(
                serde_json::to_value(LogSourceContent {
                    source_id: source.id,
                    source_name: source.name,
                    lines,
                    total_lines,
                    file_path: source.path,
                    last_modified,
                    error: None,
                })
                .unwrap_or_default(),
            ),
        },
        Err(e) => CommandResponse {
            success: true,
            message: None,
            data: Some(
                serde_json::to_value(LogSourceContent {
                    source_id: source.id,
                    source_name: source.name,
                    lines: Vec::new(),
                    total_lines: 0,
                    file_path: source.path,
                    last_modified,
                    error: Some(e),
                })
                .unwrap_or_default(),
            ),
        },
    }
}

/// Read content from all enabled log sources for a project
#[tauri::command]
pub fn read_project_logs(project_id: String) -> CommandResponse {
    let config = match load_project_config(&project_id) {
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

    let mut contents: Vec<LogSourceContent> = Vec::new();

    for source in config.get_active_log_sources().iter().filter(|s| s.enabled) {
        let path = PathBuf::from(&source.path);
        let tail_lines = source.tail_lines.unwrap_or(100);
        let last_modified = get_file_modified_time(&path);

        if !path.exists() {
            contents.push(LogSourceContent {
                source_id: source.id.clone(),
                source_name: source.name.clone(),
                lines: Vec::new(),
                total_lines: 0,
                file_path: source.path.clone(),
                last_modified: None,
                error: Some("File does not exist".to_string()),
            });
            continue;
        }

        match tail_file(&path, tail_lines) {
            Ok((lines, total_lines)) => {
                contents.push(LogSourceContent {
                    source_id: source.id.clone(),
                    source_name: source.name.clone(),
                    lines,
                    total_lines,
                    file_path: source.path.clone(),
                    last_modified,
                    error: None,
                });
            }
            Err(e) => {
                contents.push(LogSourceContent {
                    source_id: source.id.clone(),
                    source_name: source.name.clone(),
                    lines: Vec::new(),
                    total_lines: 0,
                    file_path: source.path.clone(),
                    last_modified,
                    error: Some(e),
                });
            }
        }
    }

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

// =============================================================================
// Profile Management Commands
// =============================================================================

/// Create a new log source profile for a project
#[tauri::command]
pub fn create_log_source_profile(
    project_id: String,
    name: String,
    description: Option<String>,
    log_sources: Vec<LogSource>,
) -> CommandResponse {
    let mut config = match load_project_config(&project_id) {
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

    let mut profile = LogSourceProfile::new(name, log_sources);
    profile.description = description;
    let profile_id = profile.id.clone();

    config.profiles.push(profile.clone());

    // If this is the first profile, make it active
    if config.profiles.len() == 1 {
        config.active_profile_id = Some(profile_id.clone());
    }

    config.updated_at = Some(chrono::Utc::now().to_rfc3339());

    match save_project_config(&config) {
        Ok(()) => {
            info!(
                "Created profile '{}' for project '{}'",
                profile.name, project_id
            );
            CommandResponse {
                success: true,
                message: Some(format!("Profile '{}' created", profile.name)),
                data: Some(serde_json::to_value(&profile).unwrap_or_default()),
            }
        }
        Err(e) => CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        },
    }
}

/// Update an existing log source profile
#[tauri::command]
pub fn update_log_source_profile(
    project_id: String,
    profile_id: String,
    name: Option<String>,
    description: Option<Option<String>>,
    log_sources: Option<Vec<LogSource>>,
) -> CommandResponse {
    let mut config = match load_project_config(&project_id) {
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

    let profile = match config.profiles.iter_mut().find(|p| p.id == profile_id) {
        Some(p) => p,
        None => {
            return CommandResponse {
                success: false,
                message: Some(format!("Profile '{}' not found", profile_id)),
                data: None,
            }
        }
    };

    if let Some(name) = name {
        profile.name = name;
    }
    if let Some(desc) = description {
        profile.description = desc;
    }
    if let Some(sources) = log_sources {
        profile.log_sources = sources;
    }
    profile.updated_at = Some(chrono::Utc::now().to_rfc3339());

    let updated_profile = profile.clone();
    config.updated_at = Some(chrono::Utc::now().to_rfc3339());

    match save_project_config(&config) {
        Ok(()) => {
            info!(
                "Updated profile '{}' for project '{}'",
                updated_profile.name, project_id
            );
            CommandResponse {
                success: true,
                message: Some(format!("Profile '{}' updated", updated_profile.name)),
                data: Some(serde_json::to_value(&updated_profile).unwrap_or_default()),
            }
        }
        Err(e) => CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        },
    }
}

/// Delete a log source profile
#[tauri::command]
pub fn delete_log_source_profile(project_id: String, profile_id: String) -> CommandResponse {
    let mut config = match load_project_config(&project_id) {
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

    let initial_len = config.profiles.len();
    config.profiles.retain(|p| p.id != profile_id);

    if config.profiles.len() == initial_len {
        return CommandResponse {
            success: false,
            message: Some(format!("Profile '{}' not found", profile_id)),
            data: None,
        };
    }

    // If we deleted the active profile, clear it or set to first available
    if config.active_profile_id.as_ref() == Some(&profile_id) {
        config.active_profile_id = config.profiles.first().map(|p| p.id.clone());
    }

    config.updated_at = Some(chrono::Utc::now().to_rfc3339());

    match save_project_config(&config) {
        Ok(()) => {
            info!(
                "Deleted profile '{}' from project '{}'",
                profile_id, project_id
            );
            CommandResponse {
                success: true,
                message: Some("Profile deleted".to_string()),
                data: None,
            }
        }
        Err(e) => CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        },
    }
}

/// Set the active log source profile for a project
#[tauri::command]
pub fn set_active_profile(project_id: String, profile_id: String) -> CommandResponse {
    let mut config = match load_project_config(&project_id) {
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

    // Verify the profile exists
    let profile = match config.profiles.iter().find(|p| p.id == profile_id) {
        Some(p) => p.clone(),
        None => {
            return CommandResponse {
                success: false,
                message: Some(format!("Profile '{}' not found", profile_id)),
                data: None,
            }
        }
    };

    config.active_profile_id = Some(profile_id);
    config.updated_at = Some(chrono::Utc::now().to_rfc3339());

    match save_project_config(&config) {
        Ok(()) => {
            info!(
                "Set active profile to '{}' for project '{}'",
                profile.name, project_id
            );
            CommandResponse {
                success: true,
                message: Some(format!("Active profile set to '{}'", profile.name)),
                data: Some(serde_json::to_value(&profile).unwrap_or_default()),
            }
        }
        Err(e) => CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        },
    }
}

/// List all profiles for a project
#[tauri::command]
pub fn list_log_source_profiles(project_id: String) -> CommandResponse {
    let config = match load_project_config(&project_id) {
        Ok(Some(config)) => config,
        Ok(None) => {
            return CommandResponse {
                success: true,
                message: None,
                data: Some(serde_json::json!({
                    "profiles": [],
                    "active_profile_id": null
                })),
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

    CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "profiles": config.profiles,
            "active_profile_id": config.active_profile_id
        })),
    }
}

/// Duplicate an existing profile with a new name
#[tauri::command]
pub fn duplicate_log_source_profile(
    project_id: String,
    profile_id: String,
    new_name: String,
) -> CommandResponse {
    let mut config = match load_project_config(&project_id) {
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

    let source_profile = match config.profiles.iter().find(|p| p.id == profile_id) {
        Some(p) => p.clone(),
        None => {
            return CommandResponse {
                success: false,
                message: Some(format!("Profile '{}' not found", profile_id)),
                data: None,
            }
        }
    };

    let mut new_profile = LogSourceProfile::new(new_name, source_profile.log_sources);
    new_profile.description = source_profile
        .description
        .map(|d| format!("Copy of: {}", d))
        .or(Some(format!("Copy of {}", source_profile.name)));

    let created_profile = new_profile.clone();
    config.profiles.push(new_profile);
    config.updated_at = Some(chrono::Utc::now().to_rfc3339());

    match save_project_config(&config) {
        Ok(()) => {
            info!(
                "Duplicated profile '{}' as '{}' for project '{}'",
                source_profile.name, created_profile.name, project_id
            );
            CommandResponse {
                success: true,
                message: Some(format!("Profile duplicated as '{}'", created_profile.name)),
                data: Some(serde_json::to_value(&created_profile).unwrap_or_default()),
            }
        }
        Err(e) => CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        },
    }
}
