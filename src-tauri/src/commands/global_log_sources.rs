//! Global Log Source Management Commands
//!
//! Provides Tauri commands for managing global log sources that are shared
//! across all projects. Includes CRUD operations for sources and profiles,
//! and AI-assisted source selection.

#![allow(dead_code)]

use crate::settings::{
    self, GlobalLogSource, GlobalLogSourceProfile, GlobalLogSourceSettings,
    LogSourceAiSelectionMode, LogSourceCategory,
};
use chrono::Utc;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::CommandResponse;

// ============================================================================
// Source Management Commands
// ============================================================================

/// Get all global log source settings
#[tauri::command]
pub fn get_global_log_sources() -> Result<CommandResponse, String> {
    info!("Getting global log source settings");
    let settings = settings::get_global_log_source_settings();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(
            serde_json::to_value(&settings)
                .map_err(|e| format!("Failed to serialize settings: {}", e))?,
        ),
    })
}

/// Save all global log source settings
#[tauri::command]
pub fn save_global_log_sources(
    settings: GlobalLogSourceSettings,
) -> Result<CommandResponse, String> {
    info!(
        "Saving global log source settings: {} sources, {} profiles",
        settings.sources.len(),
        settings.profiles.len()
    );

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Global log source settings saved".to_string()),
        data: None,
    })
}

/// Add a new log source
#[tauri::command]
pub fn add_global_log_source(
    name: String,
    description: String,
    category: String,
    source_type: String,
    path: String,
    pattern: Option<String>,
    tail_lines: Option<u32>,
    color: Option<String>,
    keywords: Option<Vec<String>>,
) -> Result<CommandResponse, String> {
    info!("Adding global log source: {}", name);

    let mut settings = settings::get_global_log_source_settings();

    // Parse category
    let cat: LogSourceCategory =
        serde_json::from_str(&format!("\"{}\"", category)).unwrap_or(LogSourceCategory::General);

    let source = GlobalLogSource {
        id: format!("source-{}", Uuid::new_v4()),
        name,
        description,
        category: cat,
        source_type,
        path,
        pattern,
        tail_lines: tail_lines.unwrap_or(100),
        enabled: true,
        color,
        keywords: keywords.unwrap_or_default(),
        format: "plaintext".to_string(),
        parser: "generic".to_string(),
        timestamp_pattern: None,
        timezone: "local".to_string(),
        error_patterns: vec![],
        warning_patterns: vec![],
        ignore_patterns: vec![],
        poll_interval_ms: 5000,
    };

    let created_source = source.clone();
    settings.sources.push(source);

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Log source added".to_string()),
        data: Some(
            serde_json::to_value(&created_source)
                .map_err(|e| format!("Failed to serialize source: {}", e))?,
        ),
    })
}

/// Update an existing log source
#[tauri::command]
pub fn update_global_log_source(
    id: String,
    name: Option<String>,
    description: Option<String>,
    category: Option<String>,
    source_type: Option<String>,
    path: Option<String>,
    pattern: Option<Option<String>>,
    tail_lines: Option<u32>,
    enabled: Option<bool>,
    color: Option<Option<String>>,
    keywords: Option<Vec<String>>,
) -> Result<CommandResponse, String> {
    info!("Updating global log source: {}", id);

    let mut settings = settings::get_global_log_source_settings();

    let source = settings
        .sources
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("Source '{}' not found", id))?;

    if let Some(n) = name {
        source.name = n;
    }
    if let Some(d) = description {
        source.description = d;
    }
    if let Some(c) = category {
        source.category =
            serde_json::from_str(&format!("\"{}\"", c)).unwrap_or(LogSourceCategory::General);
    }
    if let Some(t) = source_type {
        source.source_type = t;
    }
    if let Some(p) = path {
        source.path = p;
    }
    if let Some(pat) = pattern {
        source.pattern = pat;
    }
    if let Some(tl) = tail_lines {
        source.tail_lines = tl;
    }
    if let Some(e) = enabled {
        source.enabled = e;
    }
    if let Some(col) = color {
        source.color = col;
    }
    if let Some(kw) = keywords {
        source.keywords = kw;
    }

    let updated_source = source.clone();

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Log source updated".to_string()),
        data: Some(
            serde_json::to_value(&updated_source)
                .map_err(|e| format!("Failed to serialize source: {}", e))?,
        ),
    })
}

/// Delete a log source
#[tauri::command]
pub fn delete_global_log_source(id: String) -> Result<CommandResponse, String> {
    info!("Deleting global log source: {}", id);

    let mut settings = settings::get_global_log_source_settings();

    let initial_len = settings.sources.len();
    settings.sources.retain(|s| s.id != id);

    if settings.sources.len() == initial_len {
        return Err(format!("Source '{}' not found", id));
    }

    // Also remove from any profiles
    for profile in &mut settings.profiles {
        profile.source_ids.retain(|sid| *sid != id);
    }

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Log source deleted".to_string()),
        data: None,
    })
}

// ============================================================================
// Profile Management Commands
// ============================================================================

/// Create a new log source profile
#[tauri::command]
pub fn create_global_log_source_profile(
    name: String,
    description: Option<String>,
    source_ids: Vec<String>,
) -> Result<CommandResponse, String> {
    info!("Creating global log source profile: {}", name);

    let mut settings = settings::get_global_log_source_settings();

    let now = Utc::now().to_rfc3339();
    let profile = GlobalLogSourceProfile {
        id: format!("profile-{}", Uuid::new_v4()),
        name,
        description,
        source_ids,
        created_at: Some(now.clone()),
        updated_at: Some(now),
    };

    let created_profile = profile.clone();
    settings.profiles.push(profile);

    // If this is the first profile, make it the default
    if settings.profiles.len() == 1 {
        settings.default_profile_id = Some(created_profile.id.clone());
    }

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Profile created".to_string()),
        data: Some(
            serde_json::to_value(&created_profile)
                .map_err(|e| format!("Failed to serialize profile: {}", e))?,
        ),
    })
}

/// Update an existing profile
#[tauri::command]
pub fn update_global_log_source_profile(
    id: String,
    name: Option<String>,
    description: Option<Option<String>>,
    source_ids: Option<Vec<String>>,
) -> Result<CommandResponse, String> {
    info!("Updating global log source profile: {}", id);

    let mut settings = settings::get_global_log_source_settings();

    let profile = settings
        .profiles
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("Profile '{}' not found", id))?;

    if let Some(n) = name {
        profile.name = n;
    }
    if let Some(d) = description {
        profile.description = d;
    }
    if let Some(sids) = source_ids {
        profile.source_ids = sids;
    }
    profile.updated_at = Some(Utc::now().to_rfc3339());

    let updated_profile = profile.clone();

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Profile updated".to_string()),
        data: Some(
            serde_json::to_value(&updated_profile)
                .map_err(|e| format!("Failed to serialize profile: {}", e))?,
        ),
    })
}

/// Delete a profile
#[tauri::command]
pub fn delete_global_log_source_profile(id: String) -> Result<CommandResponse, String> {
    info!("Deleting global log source profile: {}", id);

    let mut settings = settings::get_global_log_source_settings();

    let initial_len = settings.profiles.len();
    settings.profiles.retain(|p| p.id != id);

    if settings.profiles.len() == initial_len {
        return Err(format!("Profile '{}' not found", id));
    }

    // Clear default if we deleted it
    if settings.default_profile_id.as_ref() == Some(&id) {
        settings.default_profile_id = settings.profiles.first().map(|p| p.id.clone());
    }

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Profile deleted".to_string()),
        data: None,
    })
}

/// Set the default profile
#[tauri::command]
pub fn set_default_log_source_profile(
    profile_id: Option<String>,
) -> Result<CommandResponse, String> {
    info!("Setting default log source profile: {:?}", profile_id);

    let mut settings = settings::get_global_log_source_settings();

    // Verify profile exists if setting one
    if let Some(ref pid) = profile_id {
        if !settings.profiles.iter().any(|p| p.id == *pid) {
            return Err(format!("Profile '{}' not found", pid));
        }
    }

    settings.default_profile_id = profile_id;

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Default profile set".to_string()),
        data: None,
    })
}

/// Set the AI selection mode
#[tauri::command]
pub fn set_log_source_ai_selection_mode(mode: String) -> Result<CommandResponse, String> {
    info!("Setting log source AI selection mode: {}", mode);

    let mut settings = settings::get_global_log_source_settings();

    settings.ai_selection_mode = serde_json::from_str(&format!("\"{}\"", mode)).map_err(|_| {
        format!(
            "Invalid mode '{}'. Use 'dynamic', 'static', or 'disabled'",
            mode
        )
    })?;

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("AI selection mode set to '{}'", mode)),
        data: None,
    })
}

// ============================================================================
// Log Reading Commands
// ============================================================================

/// Content read from a log source
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogSourceContent {
    pub source_id: String,
    pub source_name: String,
    pub lines: Vec<String>,
    pub total_lines: u64,
    pub file_path: String,
    pub last_modified: Option<String>,
    pub error: Option<String>,
}

/// Read content from specific log sources by ID
#[tauri::command]
pub fn read_global_log_sources(source_ids: Vec<String>) -> Result<CommandResponse, String> {
    debug!("Reading global log sources: {:?}", source_ids);

    let settings = settings::get_global_log_source_settings();
    let mut contents: Vec<LogSourceContent> = Vec::new();

    for source_id in source_ids {
        if let Some(source) = settings.sources.iter().find(|s| s.id == source_id) {
            let content = read_single_source(source);
            contents.push(content);
        } else {
            warn!("Source '{}' not found", source_id);
        }
    }

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(
            serde_json::to_value(&contents)
                .map_err(|e| format!("Failed to serialize contents: {}", e))?,
        ),
    })
}

/// Read content from all enabled sources in a profile
#[tauri::command]
pub fn read_log_sources_by_profile(profile_id: Option<String>) -> Result<CommandResponse, String> {
    debug!("Reading log sources by profile: {:?}", profile_id);

    let sources = settings::get_enabled_log_sources(profile_id.as_deref());
    let mut contents: Vec<LogSourceContent> = Vec::new();

    for source in &sources {
        let content = read_single_source(source);
        contents.push(content);
    }

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(
            serde_json::to_value(&contents)
                .map_err(|e| format!("Failed to serialize contents: {}", e))?,
        ),
    })
}

/// Helper to read a single source
fn read_single_source(source: &GlobalLogSource) -> LogSourceContent {
    let path = PathBuf::from(&source.path);

    if !path.exists() {
        return LogSourceContent {
            source_id: source.id.clone(),
            source_name: source.name.clone(),
            lines: Vec::new(),
            total_lines: 0,
            file_path: source.path.clone(),
            last_modified: None,
            error: Some("File does not exist".to_string()),
        };
    }

    let last_modified = fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let datetime: chrono::DateTime<chrono::Utc> = t.into();
            datetime.to_rfc3339()
        });

    match tail_file(&path, source.tail_lines) {
        Ok((lines, total_lines)) => LogSourceContent {
            source_id: source.id.clone(),
            source_name: source.name.clone(),
            lines,
            total_lines,
            file_path: source.path.clone(),
            last_modified,
            error: None,
        },
        Err(e) => LogSourceContent {
            source_id: source.id.clone(),
            source_name: source.name.clone(),
            lines: Vec::new(),
            total_lines: 0,
            file_path: source.path.clone(),
            last_modified,
            error: Some(e),
        },
    }
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

// ============================================================================
// Migration Command
// ============================================================================

/// Implementation of migrate logic, callable from HTTP handlers
pub fn migrate_project_sources_to_global_impl() -> Result<u32, String> {
    info!("Migrating per-project log sources to global (impl)");

    let mut settings = settings::get_global_log_source_settings();
    let mut migrated_count: u32 = 0;

    let projects_dir = dirs::home_dir()
        .ok_or("Failed to get home directory")?
        .join(".qontinui")
        .join("projects");

    if !projects_dir.exists() {
        return Ok(0);
    }

    let existing_paths: std::collections::HashSet<String> = settings
        .sources
        .iter()
        .map(|s| s.path.to_lowercase())
        .collect();

    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            let config_path = entry.path().join("config.json");
            if config_path.exists() {
                if let Ok(content) = fs::read_to_string(&config_path) {
                    if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
                        let sources_to_migrate: Vec<serde_json::Value> = if let Some(profiles) =
                            config.get("profiles").and_then(|p| p.as_array())
                        {
                            profiles
                                .iter()
                                .filter_map(|p| p.get("log_sources"))
                                .filter_map(|ls| ls.as_array())
                                .flatten()
                                .cloned()
                                .collect()
                        } else if let Some(sources) =
                            config.get("log_sources").and_then(|s| s.as_array())
                        {
                            sources.clone()
                        } else {
                            continue;
                        };

                        for source in sources_to_migrate {
                            if let Some(path) = source.get("path").and_then(|p| p.as_str()) {
                                if existing_paths.contains(&path.to_lowercase()) {
                                    continue;
                                }

                                let name = source
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("Unknown")
                                    .to_string();
                                let source_type = source
                                    .get("type")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("file")
                                    .to_string();
                                let tail_lines = source
                                    .get("tail_lines")
                                    .and_then(|t| t.as_u64())
                                    .unwrap_or(100)
                                    as u32;
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

                                let category = infer_category_from_name(&name);

                                let global_source = GlobalLogSource {
                                    id: format!("source-{}", Uuid::new_v4()),
                                    name: name.clone(),
                                    description: format!("Migrated from project config: {}", name),
                                    category,
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

                                settings.sources.push(global_source);
                                migrated_count += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    settings::save_global_log_source_settings(settings)
        .map_err(|e| format!("Failed to save settings: {}", e))?;

    info!("Migrated {} log sources to global", migrated_count);
    Ok(migrated_count)
}

/// Migrate sources from per-project configs to global
/// This is a one-time migration that deduplicates sources by path
#[tauri::command]
pub fn migrate_project_sources_to_global() -> Result<CommandResponse, String> {
    let migrated_count = migrate_project_sources_to_global_impl()?;
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Migrated {} log sources to global", migrated_count)),
        data: Some(serde_json::json!({ "migrated": migrated_count })),
    })
}

// ============================================================================
// AI Selection Logic
// ============================================================================

/// Context for AI-based log source selection
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogSourceSelectionContext {
    /// Application name or description (e.g., "mobile app", "web frontend")
    pub application_name: Option<String>,
    /// Target platform (e.g., "mobile", "web", "desktop")
    pub target_platform: Option<String>,
    /// Step types being executed in the workflow
    pub step_types: Vec<String>,
    /// Additional keywords to match
    pub keywords: Vec<String>,
}

/// Result of AI log source selection
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogSourceSelectionResult {
    /// IDs of selected sources
    pub selected_source_ids: Vec<String>,
    /// Reason for each selection (for transparency)
    pub selection_reasons: Vec<SelectionReason>,
    /// Was AI selection used (vs fallback to all/profile)
    pub ai_selection_used: bool,
}

/// Reason why a source was selected
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SelectionReason {
    pub source_id: String,
    pub source_name: String,
    pub reason: String,
}

/// Select log sources based on workflow context
///
/// This implements rule-based selection that can be enhanced with actual AI later.
///
/// Selection rules:
/// 1. Match category to target platform (mobile -> Mobile, web -> Frontend/Backend)
/// 2. Match keywords in source names/descriptions to context keywords
/// 3. Always include Runner logs when step_types contain GUI automation
#[tauri::command]
pub fn select_log_sources_for_context(
    context: LogSourceSelectionContext,
) -> Result<CommandResponse, String> {
    info!("Selecting log sources for context: {:?}", context);

    let settings = settings::get_global_log_source_settings();

    // Check if AI selection is disabled
    if settings.ai_selection_mode == LogSourceAiSelectionMode::Disabled {
        // Use profile or all enabled sources
        let sources = settings::get_enabled_log_sources(settings.default_profile_id.as_deref());
        return Ok(CommandResponse {
            success: true,
            message: Some("AI selection disabled, using profile/all sources".to_string()),
            data: Some(
                serde_json::to_value(LogSourceSelectionResult {
                    selected_source_ids: sources.iter().map(|s| s.id.clone()).collect(),
                    selection_reasons: sources
                        .iter()
                        .map(|s| SelectionReason {
                            source_id: s.id.clone(),
                            source_name: s.name.clone(),
                            reason: "AI selection disabled".to_string(),
                        })
                        .collect(),
                    ai_selection_used: false,
                })
                .map_err(|e| format!("Failed to serialize result: {}", e))?,
            ),
        });
    }

    // Perform rule-based selection
    let result = select_sources_rule_based(&settings.sources, &context);

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Selected {} sources",
            result.selected_source_ids.len()
        )),
        data: Some(
            serde_json::to_value(result)
                .map_err(|e| format!("Failed to serialize result: {}", e))?,
        ),
    })
}

/// Rule-based source selection (can be enhanced with AI later)
fn select_sources_rule_based(
    sources: &[GlobalLogSource],
    context: &LogSourceSelectionContext,
) -> LogSourceSelectionResult {
    let mut selected: Vec<SelectionReason> = Vec::new();

    // Determine relevant categories based on target platform
    let relevant_categories = determine_relevant_categories(context);

    // Check if we have GUI automation steps
    let has_gui_automation = context
        .step_types
        .iter()
        .any(|t| matches!(t.as_str(), "workflow" | "state" | "action" | "screenshot"));
    let has_playwright = context.step_types.iter().any(|t| t == "playwright");

    for source in sources {
        // Skip disabled sources
        if !source.enabled {
            continue;
        }

        // Check category match
        let category_match = relevant_categories.contains(&source.category);

        // Check keyword match
        let keyword_match = check_keyword_match(source, context);

        // Always include runner logs for GUI automation
        let runner_match = has_gui_automation && source.category == LogSourceCategory::Runner;

        // Always include testing logs for playwright
        let testing_match = has_playwright && source.category == LogSourceCategory::Testing;

        if category_match || keyword_match || runner_match || testing_match {
            let reason = if runner_match {
                "Runner logs needed for GUI automation steps".to_string()
            } else if testing_match {
                "Testing logs needed for Playwright steps".to_string()
            } else if category_match {
                format!(
                    "Category '{}' matches target platform",
                    format_category(&source.category)
                )
            } else {
                format!("Keywords match: {}", source.keywords.join(", "))
            };

            selected.push(SelectionReason {
                source_id: source.id.clone(),
                source_name: source.name.clone(),
                reason,
            });
        }
    }

    // If nothing selected, fall back to General category
    if selected.is_empty() {
        for source in sources
            .iter()
            .filter(|s| s.enabled && s.category == LogSourceCategory::General)
        {
            selected.push(SelectionReason {
                source_id: source.id.clone(),
                source_name: source.name.clone(),
                reason: "Fallback to General category".to_string(),
            });
        }
    }

    LogSourceSelectionResult {
        selected_source_ids: selected.iter().map(|s| s.source_id.clone()).collect(),
        selection_reasons: selected,
        ai_selection_used: true,
    }
}

/// Determine relevant categories based on context
fn determine_relevant_categories(context: &LogSourceSelectionContext) -> Vec<LogSourceCategory> {
    let mut categories = Vec::new();

    // Check target platform
    if let Some(ref platform) = context.target_platform {
        let platform_lower = platform.to_lowercase();
        if platform_lower.contains("mobile")
            || platform_lower.contains("android")
            || platform_lower.contains("ios")
        {
            categories.push(LogSourceCategory::Mobile);
        }
        if platform_lower.contains("web") || platform_lower.contains("browser") {
            categories.push(LogSourceCategory::Frontend);
            categories.push(LogSourceCategory::Backend);
        }
        if platform_lower.contains("api") {
            categories.push(LogSourceCategory::Api);
        }
    }

    // Check application name for hints
    if let Some(ref app_name) = context.application_name {
        let app_lower = app_name.to_lowercase();
        if (app_lower.contains("mobile") || app_lower.contains("app"))
            && !categories.contains(&LogSourceCategory::Mobile)
        {
            categories.push(LogSourceCategory::Mobile);
        }
        if (app_lower.contains("frontend") || app_lower.contains("web") || app_lower.contains("ui"))
            && !categories.contains(&LogSourceCategory::Frontend)
        {
            categories.push(LogSourceCategory::Frontend);
        }
        if app_lower.contains("backend")
            || app_lower.contains("server")
            || app_lower.contains("api")
        {
            if !categories.contains(&LogSourceCategory::Backend) {
                categories.push(LogSourceCategory::Backend);
            }
            if !categories.contains(&LogSourceCategory::Api) {
                categories.push(LogSourceCategory::Api);
            }
        }
    }

    // Check context keywords
    for keyword in &context.keywords {
        let kw_lower = keyword.to_lowercase();
        if kw_lower.contains("mobile") && !categories.contains(&LogSourceCategory::Mobile) {
            categories.push(LogSourceCategory::Mobile);
        }
        if kw_lower.contains("database") && !categories.contains(&LogSourceCategory::Database) {
            categories.push(LogSourceCategory::Database);
        }
        if kw_lower.contains("test") && !categories.contains(&LogSourceCategory::Testing) {
            categories.push(LogSourceCategory::Testing);
        }
    }

    // Default to General if nothing specific identified
    if categories.is_empty() {
        categories.push(LogSourceCategory::General);
    }

    categories
}

/// Check if source keywords match context keywords
fn check_keyword_match(source: &GlobalLogSource, context: &LogSourceSelectionContext) -> bool {
    if source.keywords.is_empty() {
        return false;
    }

    let context_keywords: Vec<String> = context
        .keywords
        .iter()
        .chain(context.application_name.iter())
        .chain(context.target_platform.iter())
        .map(|s| s.to_lowercase())
        .collect();

    for source_kw in &source.keywords {
        let source_kw_lower = source_kw.to_lowercase();
        for context_kw in &context_keywords {
            if source_kw_lower.contains(context_kw) || context_kw.contains(&source_kw_lower) {
                return true;
            }
        }
    }

    false
}

/// Format category for display
fn format_category(category: &LogSourceCategory) -> &'static str {
    match category {
        LogSourceCategory::Frontend => "frontend",
        LogSourceCategory::Backend => "backend",
        LogSourceCategory::Api => "api",
        LogSourceCategory::Mobile => "mobile",
        LogSourceCategory::Database => "database",
        LogSourceCategory::Build => "build",
        LogSourceCategory::Testing => "testing",
        LogSourceCategory::Runner => "runner",
        LogSourceCategory::General => "general",
    }
}

// ============================================================================
// Public API for Orchestrator
// ============================================================================

use crate::step_executor::LogSourceConfig;

/// Get log sources selected for a given context.
///
/// This is the main entry point for the orchestrator to get log sources
/// based on the workflow context. It respects the AI selection mode:
/// - Dynamic: Call this at each verification round
/// - Static: Call once at workflow setup and cache the result
/// - Disabled: Uses profile or all enabled sources
///
/// Returns a vector of LogSourceConfig that can be used with the step executor.
pub fn get_log_sources_for_context(context: &LogSourceSelectionContext) -> Vec<LogSourceConfig> {
    let settings = settings::get_global_log_source_settings();

    // Get source IDs based on selection mode
    let source_ids: Vec<String> =
        if settings.ai_selection_mode == LogSourceAiSelectionMode::Disabled {
            // Use profile or all enabled sources
            settings::get_enabled_log_sources(settings.default_profile_id.as_deref())
                .iter()
                .map(|s| s.id.clone())
                .collect()
        } else {
            // Use rule-based selection
            let result = select_sources_rule_based(&settings.sources, context);
            result.selected_source_ids
        };

    // Convert GlobalLogSource to LogSourceConfig
    source_ids
        .iter()
        .filter_map(|id| {
            settings
                .sources
                .iter()
                .find(|s| s.id == *id)
                .map(|source| LogSourceConfig {
                    id: source.id.clone(),
                    name: source.name.clone(),
                    path: source.path.clone(),
                    enabled: source.enabled,
                })
        })
        .collect()
}

/// Check if AI selection is in Dynamic mode
pub fn is_dynamic_selection_mode() -> bool {
    let settings = settings::get_global_log_source_settings();
    settings.ai_selection_mode == LogSourceAiSelectionMode::Dynamic
}

/// Check if AI selection is in Static mode
pub fn is_static_selection_mode() -> bool {
    let settings = settings::get_global_log_source_settings();
    settings.ai_selection_mode == LogSourceAiSelectionMode::Static
}

/// Get all enabled log sources without AI selection (for backward compatibility)
pub fn get_all_enabled_log_sources() -> Vec<LogSourceConfig> {
    let settings = settings::get_global_log_source_settings();
    let sources = settings::get_enabled_log_sources(settings.default_profile_id.as_deref());

    sources
        .iter()
        .map(|source| LogSourceConfig {
            id: source.id.clone(),
            name: source.name.clone(),
            path: source.path.clone(),
            enabled: source.enabled,
        })
        .collect()
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Infer category from source name (public for migration use)
pub fn infer_category_from_name(name: &str) -> LogSourceCategory {
    let name_lower = name.to_lowercase();

    if name_lower.contains("frontend")
        || name_lower.contains("next")
        || name_lower.contains("react")
    {
        LogSourceCategory::Frontend
    } else if name_lower.contains("backend")
        || name_lower.contains("fastapi")
        || name_lower.contains("django")
    {
        LogSourceCategory::Backend
    } else if name_lower.contains("api") {
        LogSourceCategory::Api
    } else if name_lower.contains("mobile")
        || name_lower.contains("metro")
        || name_lower.contains("logcat")
        || name_lower.contains("android")
        || name_lower.contains("ios")
    {
        LogSourceCategory::Mobile
    } else if name_lower.contains("test")
        || name_lower.contains("playwright")
        || name_lower.contains("jest")
    {
        LogSourceCategory::Testing
    } else if name_lower.contains("runner") || name_lower.contains("tauri") {
        LogSourceCategory::Runner
    } else if name_lower.contains("build") || name_lower.contains("ci") {
        LogSourceCategory::Build
    } else if name_lower.contains("database")
        || name_lower.contains("db")
        || name_lower.contains("postgres")
    {
        LogSourceCategory::Database
    } else {
        LogSourceCategory::General
    }
}

// ============================================================================
// AI Discovery (moved from project_logs.rs)
// ============================================================================

/// AI-suggested log source (returned by find_log_sources_with_ai)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiSuggestedLogSource {
    /// Suggested name for the log source
    pub name: String,
    /// Type: "file" or "directory"
    #[serde(rename = "type")]
    pub source_type: String,
    /// Suggested path
    pub path: String,
    /// Glob pattern for directory type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Description of what this log source typically contains
    pub description: String,
    /// Suggested color for UI
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Find log sources using AI for a given application.
///
/// Returns suggestions as `AiSuggestedLogSource` items that can be
/// directly added to global settings.
#[tauri::command]
pub fn find_log_sources_with_ai(
    application_name: String,
    project_directory: Option<String>,
) -> CommandResponse {
    use crate::ai_provider;
    use std::path::Path;

    info!(
        "Finding log sources with AI for application: {} (project_dir: {:?})",
        application_name, project_directory
    );

    // Get OS information
    let os_name = std::env::consts::OS;
    let os_info = match os_name {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        _ => os_name,
    };

    // Scan for existing log files in the project directory
    let mut discovered_logs: Vec<String> = Vec::new();
    if let Some(ref dir) = project_directory {
        let project_path = Path::new(dir);
        if project_path.exists() {
            let search_dirs = vec![
                project_path.to_path_buf(),
                project_path.join("logs"),
                project_path.join("log"),
                project_path.join(".logs"),
                project_path.join("var/log"),
                project_path
                    .parent()
                    .map(|p| p.join(".dev-logs"))
                    .unwrap_or_default(),
            ];

            for search_dir in search_dirs {
                if search_dir.exists() && search_dir.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&search_dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.is_file() {
                                if let Some(ext) = path.extension() {
                                    if ext == "log" || ext == "txt" {
                                        if let Some(path_str) = path.to_str() {
                                            discovered_logs.push(path_str.replace("\\", "/"));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let discovered_context = if discovered_logs.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nExisting log files found on this system:\n{}\n\
            Include these files in your suggestions if they seem relevant to the application.",
            discovered_logs
                .iter()
                .take(20)
                .map(|p| format!("- {}", p))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let project_context = project_directory
        .as_ref()
        .map(|dir| format!("\nProject directory: {}", dir))
        .unwrap_or_default();

    let prompt = format!(
        "You are helping a developer find log file locations for an application.\n\n\
        Application: {}\n\
        Operating System: {}{}{}\n\n\
        Please suggest log file locations for this application on this specific OS. Consider:\n\
        1. Standard log locations for {} (e.g., {} for this OS)\n\
        2. Application-specific log directories\n\
        3. Development vs production log locations\n\
        4. The project directory structure if provided\n\n\
        Return ONLY a valid JSON array with no additional text, markdown, or explanation. Each object should have:\n\
        - \"name\": A descriptive name for the log source\n\
        - \"type\": Either \"file\" for a single file or \"directory\" for a folder with multiple logs\n\
        - \"path\": The FULL ABSOLUTE path for this specific OS (use forward slashes)\n\
        - \"pattern\": For directory types, the glob pattern to match log files (e.g., \"*.log\")\n\
        - \"description\": A brief description of what this log contains\n\
        - \"color\": A hex color code for UI display (e.g., \"#22c55e\")\n\n\
        Example response format:\n\
        [\n\
          {{\"name\": \"Application Log\", \"type\": \"file\", \"path\": \"C:/ProgramData/AppName/logs/app.log\", \"description\": \"Main application log\", \"color\": \"#22c55e\"}},\n\
          {{\"name\": \"Error Logs\", \"type\": \"directory\", \"path\": \"C:/ProgramData/AppName/logs/errors\", \"pattern\": \"*.log\", \"description\": \"Error log files\", \"color\": \"#ef4444\"}}\n\
        ]\n\n\
        Provide 3-8 relevant suggestions with FULL ABSOLUTE PATHS for {}. Focus on the most likely log locations for this specific application and OS combination.",
        application_name,
        os_info,
        project_context,
        discovered_context,
        application_name,
        if os_name == "windows" {
            "C:/ProgramData, %APPDATA%, etc."
        } else {
            "/var/log, ~/.local/share, etc."
        },
        os_info
    );

    let response = ai_provider::run_prompt_sync(&prompt, 60);

    if !response.success {
        return CommandResponse {
            success: false,
            message: Some(
                response
                    .error
                    .unwrap_or_else(|| "AI request failed".to_string()),
            ),
            data: None,
        };
    }

    let output = response.output.trim();

    let json_str = if output.starts_with("```") {
        output
            .lines()
            .skip(1)
            .take_while(|line| !line.starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        output.to_string()
    };

    match serde_json::from_str::<Vec<AiSuggestedLogSource>>(&json_str) {
        Ok(suggestions) => {
            info!(
                "AI suggested {} log sources for '{}'",
                suggestions.len(),
                application_name
            );
            CommandResponse {
                success: true,
                message: None,
                data: Some(serde_json::to_value(suggestions).unwrap_or_default()),
            }
        }
        Err(e) => {
            warn!(
                "Failed to parse AI response as JSON: {}. Response was: {}",
                e,
                &json_str[..json_str.len().min(500)]
            );
            CommandResponse {
                success: false,
                message: Some(format!(
                    "Failed to parse AI suggestions: {}. The AI may have returned an invalid format.",
                    e
                )),
                data: Some(serde_json::json!({ "raw_response": output })),
            }
        }
    }
}
