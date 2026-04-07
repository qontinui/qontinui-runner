//! Backup and Restore functionality for user data
//!
//! This module provides backup and restore capabilities for all user data:
//! - Settings (settings.json)
//! - Prompts (prompts.json)
//! - Playwright Tests (playwright-tests.json)
//!
//! Backups are stored as ZIP files with a manifest for version tracking.

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use tracing::{error, info, warn};
use zip::write::FileOptions;
use zip::{ZipArchive, ZipWriter};

/// Current backup format version
const BACKUP_VERSION: &str = "1.0.0";

/// Manifest file inside the backup ZIP
const MANIFEST_FILE: &str = "manifest.json";

/// Backup manifest containing metadata
#[derive(Debug, Serialize, Deserialize)]
pub struct BackupManifest {
    pub version: String,
    pub created_at: String,
    pub app_version: String,
    pub files: Vec<BackupFileInfo>,
}

/// Information about a backed up file
#[derive(Debug, Serialize, Deserialize)]
pub struct BackupFileInfo {
    pub name: String,
    pub original_path: String,
    pub size: u64,
}

/// Result of a backup operation
#[derive(Debug, Serialize, Deserialize)]
pub struct BackupResult {
    pub success: bool,
    pub files_backed_up: Vec<String>,
    pub errors: Vec<String>,
}

/// Result of a restore operation
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreResult {
    pub success: bool,
    pub files_restored: Vec<String>,
    pub errors: Vec<String>,
    pub backup_version: String,
    pub backup_date: String,
}

/// Get the path to settings.json.
///
/// `settings.json` is intentionally shared across all runners on the machine,
/// so this path is NOT scoped to the current instance.
fn get_settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("com.qontinui.runner").join("settings.json"))
}

/// Get the path to prompts.json.
///
/// Matches `prompts::get_prompts_path` — under `config_dir()/com.qontinui.runner`
/// (the previous implementation used `data_local_dir()` which was a bug: the
/// backup would always miss the real prompts file). Scoped per-runner for
/// secondary instances.
fn get_prompts_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| {
        crate::instance::scope_path(&d.join("com.qontinui.runner")).join("prompts.json")
    })
}

/// Get the path to playwright-tests.json (per-runner for secondary instances).
fn get_playwright_tests_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| {
        crate::instance::scope_path(&d.join("com.qontinui.runner"))
            .join("playwright")
            .join("playwright-tests.json")
    })
}

/// Files to backup with their archive names
fn get_backup_files() -> Vec<(&'static str, Option<PathBuf>)> {
    vec![
        ("settings.json", get_settings_path()),
        ("prompts.json", get_prompts_path()),
        ("playwright-tests.json", get_playwright_tests_path()),
    ]
}

/// Create a backup of all user data
///
/// Returns the backup as a ZIP file in bytes
pub fn create_backup() -> Result<(Vec<u8>, BackupResult), String> {
    info!("Creating backup of user data");

    let mut buffer = Vec::new();
    let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
    let options: FileOptions<'_, ()> =
        FileOptions::<'_, ()>::default().compression_method(zip::CompressionMethod::Deflated);

    let mut files_backed_up = Vec::new();
    let mut errors = Vec::new();
    let mut file_infos = Vec::new();

    // Backup each file
    for (archive_name, path_opt) in get_backup_files() {
        match path_opt {
            Some(path) => {
                if path.exists() {
                    match fs::read(&path) {
                        Ok(contents) => match zip.start_file(archive_name, options) {
                            Ok(_) => match zip.write_all(&contents) {
                                Ok(_) => {
                                    info!("Backed up: {} ({} bytes)", archive_name, contents.len());
                                    files_backed_up.push(archive_name.to_string());
                                    file_infos.push(BackupFileInfo {
                                        name: archive_name.to_string(),
                                        original_path: path.to_string_lossy().to_string(),
                                        size: contents.len() as u64,
                                    });
                                }
                                Err(e) => {
                                    let msg = format!("Failed to write {}: {}", archive_name, e);
                                    warn!("{}", msg);
                                    errors.push(msg);
                                }
                            },
                            Err(e) => {
                                let msg = format!("Failed to start file {}: {}", archive_name, e);
                                warn!("{}", msg);
                                errors.push(msg);
                            }
                        },
                        Err(e) => {
                            let msg = format!("Failed to read {}: {}", archive_name, e);
                            warn!("{}", msg);
                            errors.push(msg);
                        }
                    }
                } else {
                    info!("Skipping {} (file does not exist)", archive_name);
                }
            }
            None => {
                let msg = format!("Could not determine path for {}", archive_name);
                warn!("{}", msg);
                errors.push(msg);
            }
        }
    }

    // Create and add manifest
    let manifest = BackupManifest {
        version: BACKUP_VERSION.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        files: file_infos,
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;

    zip.start_file(MANIFEST_FILE, options)
        .map_err(|e| format!("Failed to start manifest file: {}", e))?;
    zip.write_all(manifest_json.as_bytes())
        .map_err(|e| format!("Failed to write manifest: {}", e))?;

    // Finish the ZIP
    zip.finish()
        .map_err(|e| format!("Failed to finish ZIP: {}", e))?;

    let result = BackupResult {
        success: !files_backed_up.is_empty(),
        files_backed_up,
        errors,
    };

    info!("Backup complete: {:?}", result);
    Ok((buffer, result))
}

/// Restore user data from a backup
///
/// Takes the ZIP file bytes and restores all files to their original locations
pub fn restore_backup(data: &[u8]) -> Result<RestoreResult, String> {
    info!("Restoring from backup ({} bytes)", data.len());

    let cursor = std::io::Cursor::new(data);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| format!("Failed to open backup archive: {}", e))?;

    let mut files_restored = Vec::new();
    let mut errors = Vec::new();
    let mut manifest: Option<BackupManifest> = None;

    // First, read the manifest
    if let Ok(mut manifest_file) = archive.by_name(MANIFEST_FILE) {
        let mut contents = String::new();
        if manifest_file.read_to_string(&mut contents).is_ok() {
            manifest = serde_json::from_str(&contents).ok();
        }
    }

    let backup_version = manifest
        .as_ref()
        .map(|m| m.version.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let backup_date = manifest
        .as_ref()
        .map(|m| m.created_at.clone())
        .unwrap_or_else(|| "unknown".to_string());

    info!(
        "Restoring backup version {} from {}",
        backup_version, backup_date
    );

    // Build a map of archive names to destination paths
    let file_destinations: Vec<(&str, Option<PathBuf>)> = get_backup_files();

    // Restore each file
    for (archive_name, dest_path_opt) in file_destinations {
        if archive_name == MANIFEST_FILE {
            continue;
        }

        match archive.by_name(archive_name) {
            Ok(mut file) => match dest_path_opt {
                Some(dest_path) => {
                    // Validate the destination path stays within expected directories
                    let allowed_bases: Vec<PathBuf> =
                        vec![dirs::config_dir(), dirs::data_local_dir()]
                            .into_iter()
                            .flatten()
                            .map(|d| d.join("com.qontinui.runner"))
                            .collect();

                    if let Some(parent) = dest_path.parent() {
                        if let Err(e) = fs::create_dir_all(parent) {
                            let msg =
                                format!("Failed to create directory for {}: {}", archive_name, e);
                            warn!("{}", msg);
                            errors.push(msg);
                            continue;
                        }
                    }

                    let canonical_dest = match dest_path.parent() {
                        Some(parent) => match parent.canonicalize() {
                            Ok(canonical_parent) => {
                                canonical_parent.join(dest_path.file_name().unwrap_or_default())
                            }
                            Err(_) => {
                                let msg = format!(
                                    "Failed to resolve destination path for {}",
                                    archive_name
                                );
                                error!("{}", msg);
                                errors.push(msg);
                                continue;
                            }
                        },
                        None => {
                            let msg =
                                format!("Failed to resolve destination path for {}", archive_name);
                            error!("{}", msg);
                            errors.push(msg);
                            continue;
                        }
                    };

                    let path_is_safe = allowed_bases.iter().any(|base| {
                        if let Ok(canonical_base) = base.canonicalize() {
                            canonical_dest.starts_with(&canonical_base)
                        } else {
                            false
                        }
                    });

                    if !path_is_safe {
                        let msg = format!(
                            "Destination path for {} is outside allowed directories",
                            archive_name
                        );
                        error!("{}", msg);
                        errors.push(msg);
                        continue;
                    }

                    // Read file contents
                    let mut contents = Vec::new();
                    match file.read_to_end(&mut contents) {
                        Ok(_) => {
                            // Write to destination
                            match File::create(&dest_path) {
                                Ok(mut dest_file) => match dest_file.write_all(&contents) {
                                    Ok(_) => {
                                        info!(
                                            "Restored: {} -> {} ({} bytes)",
                                            archive_name,
                                            dest_path.display(),
                                            contents.len()
                                        );
                                        files_restored.push(archive_name.to_string());
                                    }
                                    Err(e) => {
                                        let msg =
                                            format!("Failed to write {}: {}", archive_name, e);
                                        error!("{}", msg);
                                        errors.push(msg);
                                    }
                                },
                                Err(e) => {
                                    let msg =
                                        format!("Failed to create file {}: {}", archive_name, e);
                                    error!("{}", msg);
                                    errors.push(msg);
                                }
                            }
                        }
                        Err(e) => {
                            let msg =
                                format!("Failed to read {} from archive: {}", archive_name, e);
                            error!("{}", msg);
                            errors.push(msg);
                        }
                    }
                }
                None => {
                    let msg = format!("Could not determine destination for {}", archive_name);
                    warn!("{}", msg);
                    errors.push(msg);
                }
            },
            Err(_) => {
                // File not in archive, that's OK
                info!("{} not found in backup, skipping", archive_name);
            }
        }
    }

    let result = RestoreResult {
        success: !files_restored.is_empty() && errors.is_empty(),
        files_restored,
        errors,
        backup_version,
        backup_date,
    };

    info!("Restore complete: {:?}", result);
    Ok(result)
}

/// Get information about a backup without restoring it
pub fn get_backup_info(data: &[u8]) -> Result<BackupManifest, String> {
    let cursor = std::io::Cursor::new(data);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| format!("Failed to open backup archive: {}", e))?;

    let mut manifest_file = archive
        .by_name(MANIFEST_FILE)
        .map_err(|_| "Backup does not contain a manifest file")?;

    let mut contents = String::new();
    manifest_file
        .read_to_string(&mut contents)
        .map_err(|e| format!("Failed to read manifest: {}", e))?;

    serde_json::from_str(&contents).map_err(|e| format!("Failed to parse manifest: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_paths() {
        // Just verify paths can be retrieved
        assert!(get_settings_path().is_some());
        assert!(get_prompts_path().is_some());
        assert!(get_playwright_tests_path().is_some());
    }
}
