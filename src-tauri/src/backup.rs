//! Backup and Restore functionality for user data
//!
//! This module provides backup and restore capabilities for all user data:
//! - Settings (settings.json)
//! - Prompts (prompts.json)
//! - Playwright Tests (playwright-tests.json)
//! - The Claude account roster (claude-accounts.json)
//!
//! Backups are stored as ZIP files with a manifest for version tracking.
//!
//! # Every file is located by its OWNING module
//!
//! A backup must read, and a restore must write, exactly the file the rest of
//! this runner reads and writes, so [`get_backup_files`] asks each file's own
//! resolver and builds no path itself. A private copy of a resolver agrees with
//! the real one only until the real one moves, and both copies this module used
//! to keep had drifted. `prompts.json` was once looked up under
//! `data_local_dir()`, so every backup silently missed it. `settings.json` was
//! built from `dirs::config_dir()`, ignoring `QONTINUI_CONFIG_DIR`. The
//! supervisor points that variable at a per-instance directory for every temp
//! and named runner, and a subject runner's startup points it at
//! `<root>/config`. So on any of them `POST /restore` overwrote the PRIMARY
//! runner's `settings.json`, and a backup taken there exported the primary's
//! file instead of its own.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use tracing::{error, info, warn};
use zip::write::FileOptions;
use zip::{ZipArchive, ZipWriter};

/// Current backup format version
const BACKUP_VERSION: &str = "1.0.0";

/// Manifest file inside the backup ZIP
const MANIFEST_FILE: &str = "manifest.json";

/// Archive name of the runner's settings document.
const SETTINGS_ARCHIVE_NAME: &str = "settings.json";

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

/// One user-data file a backup carries.
struct BackupFile {
    /// Its fixed name inside the archive.
    archive_name: &'static str,
    /// Where THIS runner keeps it, per the owning module's resolver, or why
    /// that could not be resolved.
    path: Result<PathBuf, String>,
}

/// The files a backup carries, each located by its OWNING module's resolver.
///
/// - `settings.json`: [`crate::settings::resolve_settings_path`], the one
///   resolver `load_settings` and `save_settings` use. It honours
///   `QONTINUI_CONFIG_DIR`, so this is the runner's OWN file. That is a
///   per-instance file on a supervisor temp/named runner and on a subject
///   runner, and the platform file only on a runner launched without the
///   override.
/// - `prompts.json` / `playwright-tests.json`: the stores' own resolvers,
///   per-runner on a secondary (`instance::scope_path`).
/// - `claude-accounts.json`: [`crate::claude_accounts::claude_accounts_file_path`],
///   machine-global by design. Every runner on the box shares that one roster.
///
/// Every resolver here creates nothing. [`restore_backup`] creates a
/// destination's directory only for a file the archive actually carries.
fn get_backup_files() -> Vec<BackupFile> {
    vec![
        BackupFile {
            archive_name: SETTINGS_ARCHIVE_NAME,
            path: crate::settings::resolve_settings_path(),
        },
        BackupFile {
            archive_name: "prompts.json",
            path: crate::prompts::resolve_prompts_path(),
        },
        BackupFile {
            archive_name: "playwright-tests.json",
            path: crate::playwright::resolve_tests_path(),
        },
        BackupFile {
            archive_name: "claude-accounts.json",
            path: crate::claude_accounts::claude_accounts_file_path()
                .ok_or_else(|| "Failed to get config directory".to_string()),
        },
    ]
}

/// The directories a restore may write into, in the order they are tried.
///
/// 1. The config dir this runner's settings resolve to
///    ([`crate::settings::resolve_config_dir`]), which is per-instance under
///    `QONTINUI_CONFIG_DIR`.
/// 2. The platform `com.qontinui.runner` dir that the prompts, Playwright and
///    roster stores live under ([`crate::settings::resolve_platform_config_dir`]).
///
/// Both come from the settings resolver, so this check cannot drift from the
/// destinations it guards. It used to hard-code the platform dirs. A
/// per-instance `settings.json` does not lie under those, so a correctly
/// resolved destination would have been refused.
fn allowed_restore_bases() -> Vec<PathBuf> {
    [
        crate::settings::resolve_config_dir().map(|(dir, _source)| dir),
        crate::settings::resolve_platform_config_dir(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Create a backup of all user data
///
/// Returns the backup as a ZIP file in bytes
pub fn create_backup() -> Result<(Vec<u8>, BackupResult), String> {
    create_backup_of(get_backup_files())
}

/// [`create_backup`] over an explicit file list: the archive-building core.
///
/// Split out so a test can back up only the entries it controls, without
/// reading the operator's real prompt library or roster.
fn create_backup_of(files: Vec<BackupFile>) -> Result<(Vec<u8>, BackupResult), String> {
    info!("Creating backup of user data");

    let mut buffer = Vec::new();
    let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
    let options: FileOptions<'_, ()> =
        FileOptions::<'_, ()>::default().compression_method(zip::CompressionMethod::Deflated);

    let mut files_backed_up = Vec::new();
    let mut errors = Vec::new();
    let mut file_infos = Vec::new();

    // Backup each file
    for BackupFile { archive_name, path } in files {
        match path {
            Ok(path) => {
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
            Err(e) => {
                let msg = format!("Could not determine path for {}: {}", archive_name, e);
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
/// Takes the ZIP file bytes and restores each file to where THIS runner keeps
/// it (see [`get_backup_files`]), which is not necessarily where it was backed
/// up from.
pub fn restore_backup(data: &[u8]) -> Result<RestoreResult, String> {
    restore_backup_into(data, get_backup_files())
}

/// [`restore_backup`] over an explicit destination list: the restoring core.
///
/// Split out so a test can drive a restore whose only possible destinations
/// are the ones it controls.
fn restore_backup_into(data: &[u8], files: Vec<BackupFile>) -> Result<RestoreResult, String> {
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

    // Every destination must stay within the directories this runner owns.
    let allowed_bases = allowed_restore_bases();

    // Restore each file
    for BackupFile {
        archive_name,
        path: dest_path,
    } in files
    {
        if archive_name == MANIFEST_FILE {
            continue;
        }

        match archive.by_name(archive_name) {
            Ok(mut file) => match dest_path {
                Ok(dest_path) => {
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

                    // Lazy and in order: a destination under the first base
                    // never makes the second one be stat'ed.
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
                        // Atomic: these files are read concurrently (settings on
                        // every load), and a torn write would read as corrupt.
                        Ok(_) => match crate::fs_atomic::atomic_write(&dest_path, &contents) {
                            Ok(()) => {
                                info!(
                                    "Restored: {} -> {} ({} bytes)",
                                    archive_name,
                                    dest_path.display(),
                                    contents.len()
                                );
                                if archive_name == SETTINGS_ARCHIVE_NAME {
                                    // The settings parse cache is keyed on
                                    // mtime + size, which a same-tick,
                                    // same-length replacement slips past. Every
                                    // in-process writer of the file owes this
                                    // (see `settings::save_settings`).
                                    crate::settings::invalidate_settings_cache();
                                }
                                files_restored.push(archive_name.to_string());
                            }
                            Err(e) => {
                                let msg = format!("Failed to write {}: {}", archive_name, e);
                                error!("{}", msg);
                                errors.push(msg);
                            }
                        },
                        Err(e) => {
                            let msg =
                                format!("Failed to read {} from archive: {}", archive_name, e);
                            error!("{}", msg);
                            errors.push(msg);
                        }
                    }
                }
                Err(e) => {
                    let msg = format!(
                        "Could not determine destination for {}: {}",
                        archive_name, e
                    );
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
    use crate::settings::{
        read_settings_from_disk, resolve_primary_settings_path, resolve_settings_path,
        SettingsProvenance,
    };
    use crate::test_env::{isolated_ambient, EnvVarRestore, IsolatedAmbient};
    use std::path::Path;

    /// A runner whose `settings.json` is NOT the primary's: inside an
    /// [`IsolatedAmbient`], `QONTINUI_CONFIG_DIR` names a per-instance dir, as
    /// the supervisor sets it for a temp/named runner and a subject runner's
    /// startup sets it to `<root>/config`.
    ///
    /// The PLATFORM config dir is moved to a separate fixture dir as well: by
    /// `XDG_CONFIG_HOME` on Linux, and by the fixture's `HOME` on macOS. The
    /// two arms of the settings resolver then name different files there, as
    /// they do on a real temp runner. Windows takes its platform dir from the
    /// known-folder API, which a test cannot point at a fixture dir. Measured
    /// under this fixture, whose `USERPROFILE` redirect that lookup expands,
    /// it resolved to nothing at all; on a box where it resolves, it is the
    /// operator's REAL `%APPDATA%`. Either way it is not the test's to touch,
    /// which is why every test below asserts the destination BEFORE any I/O
    /// and drives only the `settings.json` entry.
    ///
    /// Field order is drop order: the `XDG_CONFIG_HOME` restore runs while the
    /// fixture still holds the env lock.
    struct PerInstanceRunner {
        _xdg: EnvVarRestore,
        amb: IsolatedAmbient,
        config_dir: PathBuf,
    }

    fn per_instance_runner() -> PerInstanceRunner {
        let amb = isolated_ambient();
        let xdg = EnvVarRestore::capture(&["XDG_CONFIG_HOME"]);
        std::env::set_var("XDG_CONFIG_HOME", amb.dir().join("platform-config"));
        let config_dir = amb.dir().join("instance-config");
        std::fs::create_dir_all(&config_dir).expect("the per-instance config dir");
        std::env::set_var("QONTINUI_CONFIG_DIR", &config_dir);
        PerInstanceRunner {
            _xdg: xdg,
            amb,
            config_dir,
        }
    }

    /// The backup list narrowed to its `settings.json` entry, AFTER asserting
    /// that entry is `expected`. Every test that reads or writes through the
    /// list goes through this first. With the resolver regressed, the test
    /// stops here and never touches the default location, which on Windows
    /// can be the operator's real settings file.
    fn settings_entry_asserting(expected: &Path) -> Vec<BackupFile> {
        let entries: Vec<BackupFile> = get_backup_files()
            .into_iter()
            .filter(|f| f.archive_name == SETTINGS_ARCHIVE_NAME)
            .collect();
        assert_eq!(entries.len(), 1, "the backup list must carry settings.json");
        assert_eq!(
            entries[0].path,
            Ok(expected.to_path_buf()),
            "the backup list must locate settings.json where THIS runner keeps it \
             (QONTINUI_CONFIG_DIR), never the platform default"
        );
        entries
    }

    /// A backup archive carrying `settings.json` (and a manifest) only.
    fn archive_with_settings(settings: &[u8]) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: FileOptions<'_, ()> = FileOptions::<'_, ()>::default();
            zip.start_file(SETTINGS_ARCHIVE_NAME, options)
                .expect("start settings entry");
            zip.write_all(settings).expect("write settings entry");
            let manifest = BackupManifest {
                version: BACKUP_VERSION.to_string(),
                created_at: "2026-10-12T00:00:00Z".to_string(),
                app_version: "test".to_string(),
                files: Vec::new(),
            };
            zip.start_file(MANIFEST_FILE, options)
                .expect("start manifest entry");
            zip.write_all(serde_json::to_string(&manifest).unwrap().as_bytes())
                .expect("write manifest entry");
            zip.finish().expect("finish archive");
        }
        buffer
    }

    fn read_entry(zip: &[u8], name: &str) -> Vec<u8> {
        let mut archive =
            ZipArchive::new(std::io::Cursor::new(zip)).expect("a readable backup archive");
        let mut entry = archive.by_name(name).expect("the entry is in the archive");
        let mut out = Vec::new();
        entry.read_to_end(&mut out).expect("a readable entry");
        out
    }

    /// The defect: the backup list built `settings.json` from
    /// `dirs::config_dir()`, ignoring `QONTINUI_CONFIG_DIR`. It must name the
    /// file the settings module itself resolves, which is this runner's own.
    #[test]
    fn settings_json_is_located_through_the_per_instance_config_dir() {
        let runner = per_instance_runner();
        let own = runner.config_dir.join("settings.json");

        settings_entry_asserting(&own);
        assert_eq!(
            resolve_settings_path(),
            Ok(own.clone()),
            "fixture: the settings module must resolve the per-instance file"
        );
        if let Ok(primary) = resolve_primary_settings_path() {
            assert_ne!(
                primary, own,
                "fixture: the per-instance file must differ from the platform default"
            );
        }
    }

    /// End to end over the real entry: with `QONTINUI_CONFIG_DIR` set, a
    /// backup READS `<that dir>/settings.json` and a restore WRITES it, never
    /// the platform-default file. The primary's file is what a restore on a
    /// temp, named or subject runner used to overwrite.
    #[test]
    fn restore_writes_and_backup_reads_the_per_instance_settings_json() {
        let runner = per_instance_runner();
        let own = runner.config_dir.join("settings.json");

        // Backup reads this runner's own file.
        let own_bytes: &[u8] = br#"{"local_user_id":"per-instance-own"}"#;
        settings_entry_asserting(&own);
        std::fs::write(&own, own_bytes).expect("seed the per-instance settings.json");
        let (zip, backed_up) =
            create_backup_of(settings_entry_asserting(&own)).expect("backup succeeds");
        assert_eq!(backed_up.files_backed_up, vec!["settings.json".to_string()]);
        assert!(backed_up.errors.is_empty(), "{:?}", backed_up.errors);
        let archived = read_entry(&zip, SETTINGS_ARCHIVE_NAME);
        // Compared without printing: had the entry come from anywhere else, its
        // bytes could carry the operator's runner token.
        assert!(
            archived == own_bytes,
            "the backup must carry this runner's own settings.json ({} bytes archived, {} expected)",
            archived.len(),
            own_bytes.len()
        );
        let manifest = get_backup_info(&zip).expect("the backup carries a manifest");
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].original_path, own.to_string_lossy());

        // Restore writes this runner's own file.
        let restored_bytes: &[u8] = br#"{"local_user_id":"restored-into-the-instance"}"#;
        let restored = restore_backup_into(
            &archive_with_settings(restored_bytes),
            settings_entry_asserting(&own),
        )
        .expect("restore succeeds");
        assert!(restored.success, "{:?}", restored.errors);
        assert_eq!(restored.files_restored, vec!["settings.json".to_string()]);
        assert_eq!(
            std::fs::read(&own).expect("the per-instance settings.json"),
            restored_bytes,
            "the restore must land in <QONTINUI_CONFIG_DIR>/settings.json"
        );

        // And never the platform default, wherever the fixture can see it
        // (Linux and macOS). On Windows it is unresolvable here or the
        // operator's real file, so it is not even stat'ed.
        if let Ok(primary) = resolve_primary_settings_path() {
            if primary.starts_with(runner.amb.dir()) {
                assert!(
                    !primary.exists(),
                    "the restore wrote the platform-default {}",
                    primary.display()
                );
            }
        }
    }

    /// A restore replaces `settings.json` in-process, so it owes the settings
    /// module the cache invalidation `save_settings` performs. The parse cache
    /// is keyed on mtime + size, which a same-tick, same-length replacement
    /// slips past. Pinned deterministically: restore a same-length document,
    /// then wind the file's mtime back to the cached one.
    #[test]
    fn restore_invalidates_the_settings_parse_cache() {
        let runner = per_instance_runner();
        let own = runner.config_dir.join("settings.json");
        let before: &[u8] = br#"{"local_user_id":"before-restore"}"#;
        let after: &[u8] = br#"{"local_user_id":"after--restore"}"#;
        assert_eq!(
            before.len(),
            after.len(),
            "same length, or the size check alone would expose the change"
        );

        settings_entry_asserting(&own);
        std::fs::write(&own, before).expect("seed the per-instance settings.json");
        let mtime = std::fs::metadata(&own)
            .and_then(|m| m.modified())
            .expect("an mtime");
        let warmed = read_settings_from_disk();
        assert_eq!(warmed.provenance, SettingsProvenance::Loaded);
        assert_eq!(warmed.settings.local_user_id, "before-restore");

        let restored = restore_backup_into(
            &archive_with_settings(after),
            settings_entry_asserting(&own),
        )
        .expect("restore succeeds");
        assert!(restored.success, "{:?}", restored.errors);
        std::fs::File::options()
            .write(true)
            .open(&own)
            .and_then(|f| f.set_modified(mtime))
            .expect("wind the mtime back");

        assert_eq!(
            read_settings_from_disk().settings.local_user_id,
            "after--restore",
            "a settings load after a restore must see the restored document"
        );
    }
}
