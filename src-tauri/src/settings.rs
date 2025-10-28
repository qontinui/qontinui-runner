use std::fs;
use std::path::PathBuf;
use tracing::{error, info};

const SETTINGS_FILE: &str = "settings.json";
#[allow(dead_code)]
const LAST_CONFIG_KEY: &str = "last_config_path";

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    last_config_path: Option<String>,
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
