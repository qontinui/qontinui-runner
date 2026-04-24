//! Saved Projects commands
//!
//! User-curated registry of projects the wizard (or other UI flows) has
//! told us about. Persisted to `settings.json` under the `saved_projects`
//! key via the existing `settings` module.
//!
//! # Contract (Phase 1 of a 3-phase feature)
//!
//! Phases 2 and 3 consume these four commands in parallel, so the signatures
//! below are a strict interface:
//!
//! - [`list_saved_projects`] — returns the persisted list (empty on first run).
//! - [`save_saved_projects`] — atomic replace (wizard "Next" commits here).
//! - [`add_saved_project`] — idempotent append by normalized path.
//! - [`remove_saved_project`] — remove by normalized path; no error if absent.
//!
//! # Path normalization
//!
//! Idempotency and removal both hinge on path equality, so we normalize
//! before comparing: trim trailing `\`/`/`, and on Windows fold the drive
//! letter to uppercase and unify slashes. `std::fs::canonicalize` is
//! avoided because it requires the path to exist on disk — the wizard can
//! legitimately save a project whose directory was renamed later, and we
//! still want `remove_saved_project` to match.

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::settings::{self, SavedProject};

// Re-export so downstream code can `use commands::saved_projects::SavedProject`
// without reaching into the settings module.
pub use crate::settings::SavedProject as SavedProjectModel;

/// Serde-camelCase view used purely for clarity in this module's public
/// surface. The underlying persisted type is [`crate::settings::SavedProject`];
/// this alias exists so module consumers see a single canonical name.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedProjectMirror {
    path: String,
    name: String,
    project_type: String,
    manifest: String,
}

// ============================================================================
// Path normalization
// ============================================================================

/// Normalize a project path for equality comparison.
///
/// - Trims trailing path separators (`\` and `/`).
/// - On Windows: folds the drive letter to uppercase and replaces `/` with `\`
///   so `d:/foo/` and `D:\foo` compare equal.
/// - Leaves the rest of the path alone (no canonicalize; path need not exist).
fn normalize_path(path: &str) -> String {
    let trimmed = path.trim_end_matches(['\\', '/']);

    #[cfg(windows)]
    {
        let mut s = trimmed.replace('/', "\\");
        // Uppercase drive letter if present (e.g. "d:\foo" -> "D:\foo").
        let bytes = unsafe { s.as_bytes_mut() };
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            bytes[0] = bytes[0].to_ascii_uppercase();
        }
        s
    }

    #[cfg(not(windows))]
    {
        trimmed.to_string()
    }
}

/// Produce a clone of the input `SavedProject` with its `path` field
/// normalized. Used before any write to the persisted list so every stored
/// entry has a canonical path.
fn normalize_project(mut project: SavedProject) -> SavedProject {
    project.path = normalize_path(&project.path);
    project
}

// ============================================================================
// Tauri commands
// ============================================================================

/// Return the persisted saved-projects list.
///
/// Always `Ok`. First-run (no entry in `settings.json`) yields an empty vec.
#[tauri::command]
pub fn list_saved_projects() -> Result<Vec<SavedProject>, String> {
    Ok(settings::get_saved_projects())
}

/// Atomically replace the persisted saved-projects list.
///
/// Used by the setup wizard on "Next" when the user commits their
/// project selection. All incoming paths are normalized before persisting.
#[tauri::command]
pub fn save_saved_projects(projects: Vec<SavedProject>) -> Result<(), String> {
    let normalized: Vec<SavedProject> = projects.into_iter().map(normalize_project).collect();
    info!("Saving {} saved project(s)", normalized.len());
    settings::save_saved_projects(normalized)
}

/// Idempotently add a project to the persisted list.
///
/// If a project with the same normalized `path` is already present, this is
/// a no-op and returns `Ok(())` without touching disk. Otherwise appends
/// and persists.
#[tauri::command]
pub fn add_saved_project(project: SavedProject) -> Result<(), String> {
    let project = normalize_project(project);
    let mut current = settings::get_saved_projects();

    if current.iter().any(|p| normalize_path(&p.path) == project.path) {
        // Already present — idempotent no-op.
        info!(
            "Saved project already present, skipping add: {}",
            project.path
        );
        return Ok(());
    }

    info!("Adding saved project: {}", project.path);
    current.push(project);
    settings::save_saved_projects(current)
}

/// Remove a project from the persisted list by path.
///
/// Path is normalized before comparison. If no entry matches, this is a
/// no-op and returns `Ok(())`.
#[tauri::command]
pub fn remove_saved_project(path: String) -> Result<(), String> {
    let target = normalize_path(&path);
    let mut current = settings::get_saved_projects();
    let before = current.len();
    current.retain(|p| normalize_path(&p.path) != target);
    if current.len() == before {
        // Not found — per contract, no error.
        info!("remove_saved_project: path not present, no-op: {}", target);
        return Ok(());
    }
    info!(
        "Removed saved project ({} -> {} entries): {}",
        before,
        current.len(),
        target
    );
    settings::save_saved_projects(current)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(path: &str) -> SavedProject {
        SavedProject {
            path: path.to_string(),
            name: "demo".to_string(),
            project_type: "node".to_string(),
            manifest: "package.json".to_string(),
        }
    }

    #[test]
    fn normalize_strips_trailing_separators() {
        #[cfg(windows)]
        {
            assert_eq!(normalize_path("D:\\foo\\"), "D:\\foo");
            assert_eq!(normalize_path("D:/foo/"), "D:\\foo");
            assert_eq!(normalize_path("d:/foo"), "D:\\foo");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(normalize_path("/home/me/"), "/home/me");
            assert_eq!(normalize_path("/home/me"), "/home/me");
        }
    }

    #[test]
    fn add_is_idempotent_on_same_path() {
        // Pure in-memory logic: mimic add_saved_project's dedupe check.
        let existing = vec![sample("/tmp/proj")];
        let incoming = normalize_project(sample("/tmp/proj/"));
        let already_present = existing
            .iter()
            .any(|p| normalize_path(&p.path) == incoming.path);
        assert!(
            already_present,
            "normalized trailing-slash variant should be treated as present"
        );
    }

    #[test]
    fn remove_missing_is_no_op() {
        // Pure in-memory logic: mimic remove_saved_project's retain-count check.
        let mut current = vec![sample("/tmp/a"), sample("/tmp/b")];
        let target = normalize_path("/tmp/missing");
        let before = current.len();
        current.retain(|p| normalize_path(&p.path) != target);
        assert_eq!(
            current.len(),
            before,
            "retain must not drop anything when target is absent"
        );
    }

    #[test]
    fn remove_matches_after_normalization() {
        let mut current = vec![sample("/tmp/a/"), sample("/tmp/b")];
        let target = normalize_path("/tmp/a");
        current.retain(|p| normalize_path(&p.path) != target);
        assert_eq!(current.len(), 1, "trailing-slash variant should still match");
        assert_eq!(current[0].path, "/tmp/b");
    }
}
