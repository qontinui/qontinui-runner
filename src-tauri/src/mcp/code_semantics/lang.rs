//! Language dispatch for the code-semantics `typecheck` surface (Phase A).
//!
//! The `/code-semantics/typecheck` endpoint dispatches by the target file's
//! extension to one of three checkers:
//!   - TypeScript / JavaScript → the TS Language Service (true in-memory overlay),
//!   - Python (`.py`/`.pyi`) → `mypy` with `--shadow-file` (true overlay),
//!   - Rust (`.rs`) → `cargo check` (full-fidelity observe; syntactic predict for
//!     overlay, coverage 0.5).
//!
//! Project-root resolution walks up from the file:
//!   - Rust: nearest enclosing `Cargo.toml` (the crate, not the workspace root —
//!     `cargo check --manifest-path` points at that crate's manifest).
//!   - Python: nearest dir containing `pyproject.toml`, `mypy.ini`, `setup.cfg`,
//!     or `setup.py`; else the file's parent dir.

use std::path::{Path, PathBuf};

/// The language a `typecheck` request dispatches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// TypeScript / JavaScript (the existing Language-Service path).
    Ts,
    /// Rust (`cargo check`).
    Rust,
    /// Python (`mypy`).
    Python,
}

/// Detect the dispatch language from a file's extension. Anything that is not
/// recognized as Rust or Python is treated as `Ts`, which exactly preserves the
/// pre-Phase-A behavior for every non-Rust/Python file.
pub fn detect_language(file: &Path) -> Lang {
    match file
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("rs") => Lang::Rust,
        Some("py") | Some("pyi") => Lang::Python,
        _ => Lang::Ts,
    }
}

/// Walk up from `file` to the nearest enclosing `Cargo.toml` (the crate
/// manifest). Returns the manifest path, or `None` if none is found.
pub fn nearest_cargo_manifest(file: &Path) -> Option<PathBuf> {
    let mut dir = start_dir(file);
    while let Some(d) = dir {
        let candidate = d.join("Cargo.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

/// Markers that delimit a Python project root.
const PY_PROJECT_MARKERS: [&str; 4] = ["pyproject.toml", "mypy.ini", "setup.cfg", "setup.py"];

/// Walk up from `file` to the nearest dir containing a Python project marker.
/// Falls back to the file's parent dir when no marker is found.
pub fn python_project_root(file: &Path) -> PathBuf {
    let mut dir = start_dir(file);
    while let Some(d) = &dir {
        if PY_PROJECT_MARKERS.iter().any(|m| d.join(m).exists()) {
            return d.clone();
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    // No marker anywhere up the tree → the file's parent dir.
    file.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The directory to start an upward walk from (the file's parent, or the dir
/// itself if `file` is a directory).
fn start_dir(file: &Path) -> Option<PathBuf> {
    if file.is_dir() {
        Some(file.to_path_buf())
    } else {
        file.parent().map(|p| p.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_language_extension_table() {
        assert_eq!(detect_language(Path::new("/a/b/foo.rs")), Lang::Rust);
        assert_eq!(detect_language(Path::new("/a/b/foo.py")), Lang::Python);
        assert_eq!(detect_language(Path::new("/a/b/foo.pyi")), Lang::Python);
        assert_eq!(detect_language(Path::new("/a/b/foo.PY")), Lang::Python);
        assert_eq!(detect_language(Path::new("/a/b/foo.RS")), Lang::Rust);
        // Everything else is the (unchanged) TS path.
        assert_eq!(detect_language(Path::new("/a/b/foo.ts")), Lang::Ts);
        assert_eq!(detect_language(Path::new("/a/b/foo.tsx")), Lang::Ts);
        assert_eq!(detect_language(Path::new("/a/b/foo.js")), Lang::Ts);
        assert_eq!(detect_language(Path::new("/a/b/foo")), Lang::Ts);
        assert_eq!(detect_language(Path::new("/a/b/foo.txt")), Lang::Ts);
    }

    #[test]
    fn python_root_falls_back_to_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("lonely.py");
        std::fs::write(&file, "x = 1\n").unwrap();
        // No marker anywhere → the file's parent dir.
        let root = python_project_root(&file);
        assert_eq!(root, tmp.path());
    }

    #[test]
    fn python_root_finds_nearest_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(tmp.path().join("pyproject.toml"), "[tool.mypy]\n").unwrap();
        let file = pkg.join("mod.py");
        std::fs::write(&file, "x = 1\n").unwrap();
        let root = python_project_root(&file);
        assert_eq!(root, tmp.path());
    }

    #[test]
    fn cargo_manifest_picks_nearest_crate_not_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        // workspace root Cargo.toml
        std::fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let crate_dir = tmp.path().join("crates").join("inner");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"inner\"\n").unwrap();
        let file = crate_dir.join("src").join("lib.rs");
        std::fs::write(&file, "pub fn f() {}\n").unwrap();
        let manifest = nearest_cargo_manifest(&file).unwrap();
        // Nearest = the inner crate manifest, NOT the workspace root.
        assert_eq!(manifest, crate_dir.join("Cargo.toml"));
    }

    #[test]
    fn cargo_manifest_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("orphan.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        assert!(nearest_cargo_manifest(&file).is_none());
    }
}
