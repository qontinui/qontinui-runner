//! Scope resolution for the code-semantics observer.
//!
//! A "scope" is a (repo, language) selector. v1 supports a single default TS
//! scope (the runner's own frontend project), but the code is structured for a
//! registry: given a file path, resolve the nearest enclosing `tsconfig.json`
//! and use its directory as the scope key.

use std::path::{Path, PathBuf};

/// A resolved scope: the language and the project root (tsconfig path).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    /// Stable key (the resolved tsconfig.json absolute path, normalized).
    pub key: String,
    pub language: String,
    /// The project descriptor passed to the helper `init` (tsconfig path).
    pub project: String,
}

impl Scope {
    pub fn ts(tsconfig: &Path) -> Self {
        let key = normalize(tsconfig);
        Scope {
            key: key.clone(),
            language: "typescript".to_string(),
            project: key,
        }
    }
}

/// Normalize a path to forward slashes for stable cross-platform keys.
pub fn normalize(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// The default TS scope = the runner's own frontend project. The frontend root
/// is `CARGO_MANIFEST_DIR/..` (src-tauri's parent); its tsconfig.json is the
/// scope descriptor.
pub fn default_ts_scope() -> Option<Scope> {
    let frontend_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?;
    let tsconfig = frontend_root.join("tsconfig.json");
    if tsconfig.exists() {
        Some(Scope::ts(&tsconfig))
    } else {
        None
    }
}

/// Resolve a scope from an optional explicit `scope` selector and/or a file
/// path. Resolution order:
///   1. explicit `scope` (interpreted as a project dir or tsconfig path),
///   2. the file's nearest enclosing `tsconfig.json`,
///   3. the default TS scope.
pub fn resolve_scope(scope: Option<&str>, file: Option<&str>) -> Option<Scope> {
    if let Some(s) = scope {
        let p = PathBuf::from(s);
        let tsconfig =
            if p.is_file() && p.file_name().map(|n| n == "tsconfig.json").unwrap_or(false) {
                p
            } else {
                p.join("tsconfig.json")
            };
        if tsconfig.exists() {
            return Some(Scope::ts(&tsconfig));
        }
        // Fall through to file-based / default resolution if explicit fails.
    }

    if let Some(f) = file {
        if let Some(tsconfig) = nearest_tsconfig(Path::new(f)) {
            return Some(Scope::ts(&tsconfig));
        }
    }

    default_ts_scope()
}

/// Walk up from `file` to find the nearest `tsconfig.json`.
pub fn nearest_tsconfig(file: &Path) -> Option<PathBuf> {
    let mut dir = if file.is_dir() {
        Some(file.to_path_buf())
    } else {
        file.parent().map(|p| p.to_path_buf())
    };
    while let Some(d) = dir {
        let candidate = d.join("tsconfig.json");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_dir_scope_resolves_when_tsconfig_exists() {
        // The frontend root contains a real tsconfig.json.
        let frontend_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let s = resolve_scope(Some(frontend_root.to_str().unwrap()), None);
        assert!(s.is_some(), "expected scope from explicit frontend dir");
        let s = s.unwrap();
        assert_eq!(s.language, "typescript");
        assert!(s.key.ends_with("tsconfig.json"));
    }

    #[test]
    fn file_path_resolves_to_nearest_tsconfig() {
        // A file inside the frontend src/ resolves to the frontend tsconfig.
        let frontend_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let file = frontend_root.join("src").join("lib").join("appInfo.ts");
        let s = resolve_scope(None, file.to_str());
        assert!(s.is_some(), "expected scope from file path");
        assert!(s.unwrap().key.ends_with("tsconfig.json"));
    }

    #[test]
    fn nonexistent_explicit_scope_falls_back_to_default() {
        let s = resolve_scope(Some("/no/such/dir/anywhere"), None);
        // Falls through to the default TS scope (frontend tsconfig exists).
        assert!(s.is_some());
    }

    #[test]
    fn normalize_uses_forward_slashes() {
        let p = Path::new("C:\\foo\\bar\\tsconfig.json");
        assert_eq!(normalize(p), "C:/foo/bar/tsconfig.json");
    }

    #[test]
    fn default_ts_scope_exists() {
        // The runner frontend has a tsconfig.json.
        assert!(default_ts_scope().is_some());
    }
}
