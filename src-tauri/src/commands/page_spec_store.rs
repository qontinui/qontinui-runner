use std::path::PathBuf;
use tauri::{AppHandle, Manager, Runtime};
use tracing::{info, warn};

const SPEC_DIR: &str = "user-specs";

fn spec_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {e}"))?;
    let dir = crate::instance::scope_path(&base).join(SPEC_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create spec store directory: {e}"))?;
    Ok(dir)
}

/// Persist a page spec JSON to the user-specific spec store.
///
/// The file is written to `<app-data>/user-specs/<spec_id>.json`.
/// Spec IDs may contain colons (e.g. `runner:settings-account`); those are
/// replaced with dashes so they're safe as file names.
#[tauri::command]
pub async fn save_page_spec<R: Runtime>(
    app: AppHandle<R>,
    spec_id: String,
    spec_json: String,
) -> Result<(), String> {
    // Validate JSON before writing
    serde_json::from_str::<serde_json::Value>(&spec_json)
        .map_err(|e| format!("Invalid spec JSON: {e}"))?;

    let dir = spec_dir(&app)?;
    let safe_name = spec_id
        .replace(':', "_")
        .replace(['/', '\\', '<', '>', '"', '|', '?', '*'], "_");
    let path = dir.join(format!("{safe_name}.json"));

    std::fs::write(&path, &spec_json).map_err(|e| format!("Failed to write spec file: {e}"))?;

    info!(spec_id = %spec_id, path = %path.display(), "User page spec saved");
    Ok(())
}

/// Load all user-saved page specs from the spec store.
///
/// Returns a list of `{ specId, specJson }` objects for all specs that were
/// previously saved with `save_page_spec`.
#[tauri::command]
pub async fn load_user_specs<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<serde_json::Value>, String> {
    let dir = spec_dir(&app)?;
    let mut specs = Vec::new();

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read user spec store directory: {}", e);
            return Ok(specs);
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let spec_json = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to read spec file {}: {}", path.display(), e);
                continue;
            }
        };

        // Reconstruct spec_id from filename: reverse the safe_name transform
        // for the colon (first underscore before a known prefix becomes colon).
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        // Restore "runner_" → "runner:" prefix
        let spec_id = if stem.starts_with("runner_") {
            format!("runner:{}", &stem["runner_".len()..])
        } else {
            stem
        };

        match serde_json::from_str::<serde_json::Value>(&spec_json) {
            Ok(parsed) => {
                specs.push(serde_json::json!({
                    "specId": spec_id,
                    "specJson": parsed,
                }));
            }
            Err(e) => {
                warn!("Skipping malformed spec file {}: {}", path.display(), e);
            }
        }
    }

    info!("Loaded {} user page specs", specs.len());
    Ok(specs)
}

/// Delete a user-saved page spec from the store.
#[tauri::command]
pub async fn delete_page_spec<R: Runtime>(
    app: AppHandle<R>,
    spec_id: String,
) -> Result<(), String> {
    let dir = spec_dir(&app)?;
    let safe_name = spec_id
        .replace(':', "_")
        .replace(['/', '\\', '<', '>', '"', '|', '?', '*'], "_");
    let path = dir.join(format!("{safe_name}.json"));

    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Failed to delete spec file: {e}"))?;
        info!(spec_id = %spec_id, "User page spec deleted");
    }
    Ok(())
}
