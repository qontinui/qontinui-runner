//! Declare-time **sibling-manifest walk** (plan decision 6): from the runner's
//! flat sibling-checkout layout (`<root>/<name>/`, see
//! [`crate::agent_worktree::canonical_paths`]), collect the manifest TEXT of the
//! installing repo's sibling checkouts so coord can run its pure cross-repo
//! peer-dep conflict walk (`compute_peer_dep_conflicts`). Coord does the
//! arithmetic — the runner only supplies `repo → manifest text`.
//!
//! This is a bounded, TEXT-ONLY walk:
//! - it looks ONLY at the immediate siblings of `repo_path` (its parent dir's
//!   subdirectories), never recursing into them,
//! - it reads ONE ecosystem manifest per sibling (`package.json` for Node,
//!   `Cargo.toml` for Rust — pip/poetry have no peer-dep concept coord walks, so
//!   they yield nothing),
//! - it excludes the installing repo itself (a repo never conflicts with
//!   itself; coord assumes the caller excludes it),
//! - it is capped at [`MAX_SIBLINGS`] manifests and [`MAX_MANIFEST_BYTES`] each.
//!
//! Pure over the filesystem snapshot; never mutates, never shells out.

use std::collections::BTreeMap;
use std::path::Path;

use super::types::PackageManager;

/// Max sibling manifests collected. The `@qontinui/*` workspace is ~a dozen
/// repos; 64 is generous headroom while bounding a pathological flat root.
pub const MAX_SIBLINGS: usize = 64;

/// Max bytes read per sibling manifest. A manifest is small; 256 KiB bounds a
/// hand-crafted huge file.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// The manifest filename coord's peer-dep walker can parse for `pm`'s ecosystem.
/// Node (`package.json`) is the one coord's `parse_manifest` + `range_contains`
/// fully model; Cargo manifests are collected too (coord's parser handles the
/// `[dependencies]` shape). pip/poetry have no peer-dep range walk in coord ⇒
/// `None` (no manifests collected — honest empty `coord` dimension).
fn manifest_filename(pm: PackageManager) -> Option<&'static str> {
    match pm {
        PackageManager::Npm | PackageManager::Yarn | PackageManager::Pnpm => Some("package.json"),
        PackageManager::Cargo => Some("Cargo.toml"),
        PackageManager::Pip | PackageManager::Poetry => None,
    }
}

/// Walk the siblings of `repo_path` and collect `repo_name → manifest text` for
/// the manifest matching `pm`'s ecosystem. Excludes the installing repo (matched
/// by directory name). Returns an empty map when `pm` has no peer-dep manifest,
/// `repo_path` has no parent, or no sibling carries a matching manifest.
pub fn collect_sibling_manifests(pm: PackageManager, repo_path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(manifest_name) = manifest_filename(pm) else {
        return out;
    };
    let Some(parent) = repo_path.parent() else {
        return out;
    };
    // The installing repo's own dir name — excluded from the sibling set.
    let self_name = repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());

    let Ok(entries) = std::fs::read_dir(parent) else {
        return out;
    };
    // Deterministic order so the cap truncates predictably.
    let mut dirs: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        if out.len() >= MAX_SIBLINGS {
            break;
        }
        let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };
        if Some(&name) == self_name.as_ref() {
            continue; // the installing repo — never conflict with self
        }
        let manifest_path = dir.join(manifest_name);
        if let Some(text) = read_capped(&manifest_path) {
            out.insert(name, text);
        }
    }
    out
}

/// Read up to [`MAX_MANIFEST_BYTES`] of a manifest as lossy UTF-8. `None` if the
/// file is absent/unreadable (the sibling simply isn't of this ecosystem).
fn read_capped(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(MAX_MANIFEST_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn collects_node_siblings_excludes_self_and_non_node() {
        let root = tempdir().unwrap();
        let r = root.path();
        // installing repo
        let install = r.join("widget");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("package.json"), "{\"name\":\"widget\"}").unwrap();
        // sibling A — node
        std::fs::create_dir_all(r.join("sib-a")).unwrap();
        std::fs::write(
            r.join("sib-a/package.json"),
            "{\"name\":\"sib-a\",\"dependencies\":{\"left-pad\":\"^1.0.0\"}}",
        )
        .unwrap();
        // sibling B — rust only (no package.json) ⇒ skipped for npm
        std::fs::create_dir_all(r.join("sib-b")).unwrap();
        std::fs::write(r.join("sib-b/Cargo.toml"), "[package]\nname=\"sib-b\"").unwrap();

        let map = collect_sibling_manifests(PackageManager::Npm, &install);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("sib-a"));
        assert!(!map.contains_key("widget"), "self excluded");
        assert!(!map.contains_key("sib-b"), "non-node sibling skipped");
        assert!(map["sib-a"].contains("left-pad"));
    }

    #[test]
    fn collects_cargo_siblings() {
        let root = tempdir().unwrap();
        let r = root.path();
        let install = r.join("my-crate");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("Cargo.toml"), "[package]\nname=\"my-crate\"").unwrap();
        std::fs::create_dir_all(r.join("dep-crate")).unwrap();
        std::fs::write(
            r.join("dep-crate/Cargo.toml"),
            "[package]\nname=\"dep-crate\"\n[dependencies]\nserde=\"1\"",
        )
        .unwrap();

        let map = collect_sibling_manifests(PackageManager::Cargo, &install);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("dep-crate"));
        assert!(map["dep-crate"].contains("serde"));
    }

    #[test]
    fn pip_poetry_collect_nothing() {
        let root = tempdir().unwrap();
        let install = root.path().join("pyrepo");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(root.path().join("sib")).unwrap();
        std::fs::write(root.path().join("sib/package.json"), "{}").unwrap();
        assert!(collect_sibling_manifests(PackageManager::Pip, &install).is_empty());
        assert!(collect_sibling_manifests(PackageManager::Poetry, &install).is_empty());
    }

    #[test]
    fn no_parent_is_empty() {
        // A root-only path with no usable parent yields nothing (no panic).
        let map = collect_sibling_manifests(PackageManager::Npm, Path::new("/"));
        assert!(map.is_empty());
    }
}
