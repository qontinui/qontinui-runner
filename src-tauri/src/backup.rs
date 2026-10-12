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
use std::path::{Path, PathBuf};
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
    /// Files the archive carried that this runner deliberately did NOT
    /// restore, each with the reason (the machine-global roster on a
    /// secondary). A skip is not a failure, so it never lands in `errors`
    /// and never turns `success` false.
    pub skipped: Vec<String>,
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
    /// The ONE directory a restore may write this file under, resolved
    /// independently of `path`, so a regressed `path` is refused rather than
    /// written.
    base: Result<PathBuf, String>,
}

/// Archive name of the machine-global Claude account roster.
const ROSTER_ARCHIVE_NAME: &str = "claude-accounts.json";

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
/// destination's directory only for a file the archive actually carries, and
/// only after the destination passed its base check.
///
/// # Each file has its OWN restore base
///
/// - `settings.json` may only be written under the config dir
///   [`crate::settings::resolve_config_dir`] answers. Under
///   `QONTINUI_CONFIG_DIR` that is the per-instance dir alone, so a settings
///   entry that regressed to the platform file is refused, even though the
///   platform dir CONTAINS a supervisor runner's
///   `com.qontinui.runner/instances/<id>` dir. The old shared allow-list
///   (platform config dir + platform data-local dir, for every file) could not
///   see that regression, because both files lie under the platform dir.
/// - The other three may only be written under the platform
///   `com.qontinui.runner` dir ([`crate::settings::resolve_platform_config_dir`]).
///   Their modules join that dir themselves, so this base is an independent
///   statement of where they must land, not a derivation of their paths.
fn get_backup_files() -> Vec<BackupFile> {
    let settings_base = crate::settings::resolve_config_dir().map(|(dir, _source)| dir);
    let platform_base = crate::settings::resolve_platform_config_dir;
    vec![
        BackupFile {
            archive_name: SETTINGS_ARCHIVE_NAME,
            path: crate::settings::resolve_settings_path(),
            base: settings_base,
        },
        BackupFile {
            archive_name: "prompts.json",
            path: crate::prompts::resolve_prompts_path(),
            base: platform_base(),
        },
        BackupFile {
            archive_name: "playwright-tests.json",
            path: crate::playwright::resolve_tests_path(),
            base: platform_base(),
        },
        BackupFile {
            archive_name: ROSTER_ARCHIVE_NAME,
            path: crate::claude_accounts::claude_accounts_file_path()
                .ok_or_else(|| "Failed to get config directory".to_string()),
            base: platform_base(),
        },
    ]
}

/// `path` canonicalized WITHOUT requiring it to exist and without creating
/// anything: the nearest existing ancestor is canonicalized and the missing
/// tail re-joined. `None` when no ancestor exists or the tail is not plain
/// names (a `..` or `.` there could walk out of the canonical prefix).
fn canonicalize_creating_nothing(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            return Some(tail.iter().rev().fold(canonical, |acc, c| acc.join(c)));
        }
        match existing.components().next_back() {
            Some(std::path::Component::Normal(name)) => tail.push(name.to_os_string()),
            _ => return None,
        }
        existing = existing.parent()?;
    }
}

/// Is `dest` inside `base`? Decided before anything is created, so a refused
/// destination leaves no directory behind (a typo'd `QONTINUI_CONFIG_DIR`
/// used to be `create_dir_all`ed before this check ran).
///
/// Two deliberate behaviour changes follow from canonicalizing BOTH sides:
///
/// - A `dest` that is a symlink (or junction) pointing OUTSIDE `base` is
///   refused. The code before this series (`File::create`) followed the
///   link; a link pointing outside `base` is now refused, and inside `base`
///   the atomic rename replaces the link rather than following it.
/// - A RELATIVE `QONTINUI_CONFIG_DIR` whose directory does not exist yet has
///   no existing ancestor to canonicalize from (the walk stops at the empty
///   path), so its destinations are refused rather than created relative to
///   the current directory.
fn destination_is_within(dest: &Path, base: &Path) -> bool {
    match (
        canonicalize_creating_nothing(dest),
        canonicalize_creating_nothing(base),
    ) {
        (Some(dest), Some(base)) => dest.starts_with(base),
        _ => false,
    }
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
    for BackupFile {
        archive_name, path, ..
    } in files
    {
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
    let mut skipped = Vec::new();
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

    // Restore each file
    for BackupFile {
        archive_name,
        path: dest_path,
        base,
    } in files
    {
        if archive_name == MANIFEST_FILE {
            continue;
        }

        match archive.by_name(archive_name) {
            // The roster is machine-global: it overrides the roster fields of
            // EVERY instance's settings, the primary's included
            // (`claude_accounts::apply_roster_overlay`). A secondary restoring
            // it would rewrite the primary's account selection, so it is
            // skipped there and reported in `skipped`, not `errors`.
            Ok(_) if archive_name == ROSTER_ARCHIVE_NAME && crate::instance::is_secondary() => {
                let msg = format!(
                    "Skipped {}: it is the machine-global Claude account roster, which a \
                     secondary runner must not replace; restore it on the primary",
                    archive_name
                );
                warn!("{}", msg);
                skipped.push(msg);
            }
            Ok(mut file) => match dest_path {
                Ok(dest_path) => {
                    let inside = base
                        .as_ref()
                        .is_ok_and(|base| destination_is_within(&dest_path, base));
                    if !inside {
                        let msg = format!(
                            "Destination path for {} is outside allowed directories",
                            archive_name
                        );
                        error!("{}", msg);
                        errors.push(msg);
                        continue;
                    }

                    if let Some(parent) = dest_path.parent() {
                        if let Err(e) = fs::create_dir_all(parent) {
                            let msg =
                                format!("Failed to create directory for {}: {}", archive_name, e);
                            warn!("{}", msg);
                            errors.push(msg);
                            continue;
                        }
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
        skipped,
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
        archive_with(SETTINGS_ARCHIVE_NAME, settings)
    }

    /// A backup archive carrying one entry (and a manifest) only.
    fn archive_with(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: FileOptions<'_, ()> = FileOptions::<'_, ()>::default();
            zip.start_file(name, options).expect("start the entry");
            zip.write_all(bytes).expect("write the entry");
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

    /// Each file's restore base is resolved independently of its path, and
    /// under `QONTINUI_CONFIG_DIR` the settings base is the per-instance dir
    /// ALONE. A base that admitted the platform dir would also admit the
    /// primary's `settings.json`, which a regressed settings path resolves to,
    /// because the platform dir contains a supervisor runner's
    /// `instances/<id>` dir. No I/O.
    #[test]
    fn settings_restore_base_is_the_per_instance_config_dir_alone() {
        let runner = per_instance_runner();
        let settings = get_backup_files()
            .into_iter()
            .find(|f| f.archive_name == SETTINGS_ARCHIVE_NAME)
            .expect("the backup list carries settings.json");
        assert_eq!(
            settings.base,
            Ok(runner.config_dir.clone()),
            "settings.json may only be restored under QONTINUI_CONFIG_DIR"
        );
    }

    /// A settings destination outside its base (here: the platform file a
    /// regressed resolver would name, placed in the fixture) is refused, and
    /// refused BEFORE anything is created, so no directory is left behind.
    #[test]
    fn restore_refuses_a_settings_destination_outside_its_base_and_creates_nothing() {
        let runner = per_instance_runner();
        let own = runner.config_dir.join("settings.json");
        let platform_dir = runner.amb.dir().join("platform-config");
        let regressed = platform_dir
            .join("com.qontinui.runner")
            .join("settings.json");
        let mut entries = settings_entry_asserting(&own);
        entries[0].path = Ok(regressed.clone());

        let restored =
            restore_backup_into(&archive_with_settings(b"{}"), entries).expect("restore runs");
        assert!(!restored.success);
        assert!(restored.files_restored.is_empty());
        assert!(
            restored
                .errors
                .iter()
                .any(|e| e.contains("outside allowed directories")),
            "{:?}",
            restored.errors
        );
        assert!(!regressed.exists(), "the refused file was written");
        assert!(
            !platform_dir.exists(),
            "the refused destination's directory was created before the check"
        );
    }

    /// A secondary must not replace the machine-global roster: it overrides
    /// the roster fields of every instance, the primary's included. The skip
    /// keys on the archive name, so the entry is pointed into the fixture and a
    /// regression could only ever write there.
    #[test]
    fn a_secondary_restore_skips_the_machine_global_roster() {
        let runner = per_instance_runner();
        std::env::set_var("QONTINUI_INSTANCE_NAME", "backup-roster-test");
        let roster_dir = runner.amb.dir().join("roster");
        let roster = roster_dir.join(ROSTER_ARCHIVE_NAME);
        let entries = vec![BackupFile {
            archive_name: ROSTER_ARCHIVE_NAME,
            path: Ok(roster.clone()),
            base: Ok(roster_dir.clone()),
        }];

        let restored = restore_backup_into(
            &archive_with(ROSTER_ARCHIVE_NAME, br#"{"claude_config_dirs":[]}"#),
            entries,
        )
        .expect("restore runs");
        assert!(restored.files_restored.is_empty(), "{:?}", restored);
        assert!(
            restored
                .skipped
                .iter()
                .any(|e| e.starts_with("Skipped claude-accounts.json")),
            "the skip must be reported in `skipped`: {:?}",
            restored
        );
        assert!(
            restored.errors.is_empty(),
            "a skip is not a failure, so it must not land in `errors`: {:?}",
            restored.errors
        );
        assert!(!roster.exists(), "a secondary replaced the roster");
    }

    /// Prompts, Playwright tests and the roster land where their stores keep
    /// them. The expected paths are built HERE, from the platform config dir
    /// and the layout each store documents, not by calling the resolvers
    /// `get_backup_files` calls, so a copy that drifted in either place shows
    /// up as a mismatch. `instance::scope_path` is shared on purpose: it is the
    /// per-instance layer below every store, not one of the resolvers.
    ///
    /// LIMIT, stated rather than hidden: on Windows under this fixture the
    /// platform config dir does not resolve at all (the known-folder lookup
    /// expands the fixture's `USERPROFILE`), so there the test can only check
    /// that all three entries report an unresolvable path. The path comparison
    /// runs on Linux and macOS, where the fixture redirects the platform dir.
    /// No I/O.
    #[test]
    fn non_settings_files_land_where_their_stores_keep_them() {
        let _runner = per_instance_runner();
        let by_name = |name: &str| {
            get_backup_files()
                .into_iter()
                .find(|f| f.archive_name == name)
                .unwrap_or_else(|| panic!("the backup list carries {name}"))
                .path
        };
        let (prompts, playwright, roster) = (
            by_name("prompts.json"),
            by_name("playwright-tests.json"),
            by_name(ROSTER_ARCHIVE_NAME),
        );
        match dirs::config_dir() {
            Some(config) => {
                let app = config.join("com.qontinui.runner");
                let scoped = crate::instance::scope_path(&app);
                assert_eq!(prompts, Ok(scoped.join("prompts.json")));
                assert_eq!(
                    playwright,
                    Ok(scoped.join("playwright").join("playwright-tests.json"))
                );
                assert_eq!(roster, Ok(app.join("claude-accounts.json")));
            }
            None => {
                assert!(prompts.is_err(), "{prompts:?}");
                assert!(playwright.is_err(), "{playwright:?}");
                assert!(roster.is_err(), "{roster:?}");
            }
        }
    }
}
