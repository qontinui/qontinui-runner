use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};

const SETTINGS_FILE: &str = "settings.json";
#[allow(dead_code)]
const LAST_CONFIG_KEY: &str = "last_config_path";

/// Debug settings for image matching and other diagnostic features
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugSettings {
    /// Enable detailed image matching debug information
    pub enable_image_debug: bool,
    /// Number of top matches to include in debug output
    pub top_matches_count: u32,
}

impl Default for DebugSettings {
    fn default() -> Self {
        Self {
            // Default to true to enable visual debug image generation for troubleshooting
            enable_image_debug: true,
            top_matches_count: 5,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    last_config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_monitor_index: Option<i32>,
    #[serde(default = "default_auto_load_last_config")]
    pub auto_load_last_config: bool,
    #[serde(default)]
    pub debug: DebugSettings,
}

fn default_auto_load_last_config() -> bool {
    false
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            last_config_path: None,
            last_workflow_id: None,
            last_monitor_index: None,
            auto_load_last_config: false,
            debug: DebugSettings::default(),
        }
    }
}

/// Get the settings file path in the app data directory
fn get_settings_path() -> Result<PathBuf, String> {
    let app_data_dir = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner");

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create app data directory: {}", e))?;
    }

    Ok(app_data_dir.join(SETTINGS_FILE))
}

/// Load settings from file
fn load_settings() -> Settings {
    match get_settings_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(settings) => settings,
                        Err(e) => {
                            error!("Failed to parse settings file: {}", e);
                            Settings::default()
                        }
                    },
                    Err(e) => {
                        error!("Failed to read settings file: {}", e);
                        Settings::default()
                    }
                }
            } else {
                Settings::default()
            }
        }
        Err(e) => {
            error!("Failed to get settings path: {}", e);
            Settings::default()
        }
    }
}

/// Save settings to file
fn save_settings(settings: &Settings) -> Result<(), String> {
    let path = get_settings_path()?;
    let contents = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    fs::write(&path, contents).map_err(|e| format!("Failed to write settings file: {}", e))?;

    Ok(())
}

/// Save the last loaded config path
pub fn save_last_config_path(path: &str) -> Result<(), String> {
    info!("Saving last config path: {}", path);
    let mut settings = load_settings();
    settings.last_config_path = Some(path.to_string());
    save_settings(&settings)?;
    Ok(())
}

/// Get the last loaded config path
pub fn get_last_config_path() -> Option<String> {
    let settings = load_settings();
    settings.last_config_path
}

/// Get the current debug settings
pub fn get_debug_settings() -> DebugSettings {
    let settings = load_settings();
    settings.debug
}

/// Save debug settings
pub fn save_debug_settings(debug_settings: DebugSettings) -> Result<(), String> {
    info!("Saving debug settings: {:?}", debug_settings);
    let mut settings = load_settings();
    settings.debug = debug_settings;
    save_settings(&settings)?;
    Ok(())
}

/// Save the last used workflow ID
pub fn save_last_workflow_id(workflow_id: &str) -> Result<(), String> {
    info!("Saving last workflow ID: {}", workflow_id);
    let mut settings = load_settings();
    settings.last_workflow_id = Some(workflow_id.to_string());
    save_settings(&settings)?;
    Ok(())
}

/// Get the last used workflow ID
pub fn get_last_workflow_id() -> Option<String> {
    let settings = load_settings();
    settings.last_workflow_id
}

/// Save the last used monitor index
pub fn save_last_monitor_index(monitor_index: i32) -> Result<(), String> {
    info!("Saving last monitor index: {}", monitor_index);
    let mut settings = load_settings();
    settings.last_monitor_index = Some(monitor_index);
    save_settings(&settings)?;
    Ok(())
}

/// Get the last used monitor index
pub fn get_last_monitor_index() -> Option<i32> {
    let settings = load_settings();
    settings.last_monitor_index
}

/// Get the auto-load last config setting
pub fn get_auto_load_last_config() -> bool {
    let settings = load_settings();
    settings.auto_load_last_config
}

/// Save the auto-load last config setting
pub fn save_auto_load_last_config(enabled: bool) -> Result<(), String> {
    info!("Saving auto-load last config setting: {}", enabled);
    let mut settings = load_settings();
    settings.auto_load_last_config = enabled;
    save_settings(&settings)?;
    Ok(())
}
