use super::types::QontinuiConfig;
use std::fs;
use std::path::Path;
use serde_json;

pub struct ConfigLoader;

impl ConfigLoader {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<QontinuiConfig, String> {
        let path = path.as_ref();
        
        if !path.exists() {
            return Err(format!("Configuration file not found: {:?}", path));
        }

        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read configuration file: {}", e))?;

        Self::load_from_string(&content)
    }

    pub fn load_from_string(json_str: &str) -> Result<QontinuiConfig, String> {
        // Debug: Print first 500 chars of JSON to see what we're parsing
        eprintln!("DEBUG: Loading JSON (first 500 chars): {}", 
                  &json_str.chars().take(500).collect::<String>());
        
        // Try to parse as generic JSON first to see structure
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
            // Check if states array exists and print first state
            if let Some(states) = value.get("states") {
                if let Some(first_state) = states.as_array().and_then(|arr| arr.first()) {
                    eprintln!("DEBUG: First state in JSON: {}", 
                             serde_json::to_string_pretty(first_state).unwrap_or_default());
                }
            }
        }
        
        let config: QontinuiConfig = serde_json::from_str(json_str)
            .map_err(|e| {
                eprintln!("DEBUG: Deserialization error details: {:?}", e);
                format!("Failed to parse JSON configuration: {}", e)
            })?;

        // Validate the configuration
        config.validate()
            .map_err(|errors| errors.join(", "))?;

        Ok(config)
    }

}