//! Scope resolution for the code-semantics observer.
//!
//! A "scope" is a (repo, language) selector. The TS `Ξ_Type` language service
//! uses the tsconfig-anchored default scope (the runner's own frontend project).
//! The multi-language `Ξ_AST` code-graph surface additionally resolves a scope
//! to ANY sibling repo checkout via [`repo_dir`] (a repo→dir registry), so
//! `coord_diff_impact` / `coord_change_conflict` can answer for coord (Rust),
//! web (Python), etc. — not only the runner's TS frontend.

use std::collections::HashMap;
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

/// The workspace root holding the sibling repo checkouts. Resolution order:
///   1. env `QONTINUI_WORKSPACE_ROOT` (explicit, for non-standard layouts /
///      deployed binaries),
///   2. the runner checkout's grandparent (`CARGO_MANIFEST_DIR/../..` — the dir
///      that contains `qontinui-runner` and its sibling repos, the dev layout).
pub fn workspace_root() -> Option<PathBuf> {
    if let Ok(r) = std::env::var("QONTINUI_WORKSPACE_ROOT") {
        let p = PathBuf::from(r);
        if p.is_dir() {
            return Some(p);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // qontinui-runner
        .and_then(Path::parent) // workspace root (contains the sibling repos)
        .map(Path::to_path_buf)
}

/// Explicit `repo-name → dir` overrides parsed from `QONTINUI_CODE_GRAPH_ROOTS`
/// (`name=dir,name=dir,…`). Takes precedence over the sibling convention so a
/// non-standard checkout layout can still be mapped.
fn registry_overrides() -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    if let Ok(raw) = std::env::var("QONTINUI_CODE_GRAPH_ROOTS") {
        for entry in raw.split(',') {
            if let Some((name, dir)) = entry.split_once('=') {
                let (name, dir) = (name.trim(), dir.trim());
                if !name.is_empty() && !dir.is_empty() {
                    map.insert(name.to_string(), PathBuf::from(dir));
                }
            }
        }
    }
    map
}

/// Resolve a repo selector to a local checkout directory for the multi-language
/// `Ξ_AST` code graph. Accepts a bare repo name (`qontinui-coord`, `coord`) or an
/// `owner/name` slug (`qontinui/qontinui-coord`). Resolution: the
/// `QONTINUI_CODE_GRAPH_ROOTS` override, then `<workspace_root>/<name>`, then
/// `<workspace_root>/qontinui-<name>`. Returns `None` for an unknown repo or any
/// path-like selector (so the caller resolves real paths as directories and an
/// unknown selector falls back to the default scope — never an error).
pub fn repo_dir(selector: &str) -> Option<PathBuf> {
    repo_dir_with(selector, &registry_overrides(), workspace_root().as_deref())
}

/// Pure core of [`repo_dir`] (the overrides + workspace root are injected so the
/// resolution rules are unit-testable against a temp layout, no env / no real
/// sibling checkouts).
fn repo_dir_with(
    selector: &str,
    overrides: &HashMap<String, PathBuf>,
    root: Option<&Path>,
) -> Option<PathBuf> {
    let sel = selector.trim();
    // Reject path-like selectors — those are resolved as directories by the
    // caller, not as repo names. (`:` rejects Windows drive paths like `D:/…`.)
    if sel.is_empty()
        || sel.starts_with('.')
        || sel.starts_with('/')
        || sel.contains('\\')
        || sel.contains(':')
    {
        return None;
    }
    // Accept an `owner/name` slug by taking the trailing name component.
    let name = sel.rsplit('/').next().unwrap_or(sel);
    if name.is_empty() {
        return None;
    }
    if let Some(dir) = overrides.get(name) {
        if dir.is_dir() {
            return Some(dir.clone());
        }
    }
    let root = root?;
    let direct = root.join(name);
    if direct.is_dir() {
        return Some(direct);
    }
    let prefixed = root.join(format!("qontinui-{name}"));
    if prefixed.is_dir() {
        return Some(prefixed);
    }
    None
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

    #[test]
    fn repo_dir_rejects_path_like_selectors() {
        let empty = HashMap::new();
        let root = std::env::temp_dir();
        for sel in [
            "",
            "D:/qontinui-root/qontinui-coord", // Windows drive path
            "/abs/path",
            "./rel",
            "a\\b",
            "C:\\x",
        ] {
            assert_eq!(
                repo_dir_with(sel, &empty, Some(&root)),
                None,
                "should reject path-like selector {sel:?}"
            );
        }
    }

    #[test]
    fn repo_dir_resolves_name_slug_prefix_and_override() {
        // A temp workspace: a `qontinui-coord` sibling + a `qontinui-web` override
        // pointing at a non-conventional dir.
        let base = std::env::temp_dir().join("qontinui_xrepo_scope_test");
        let coord = base.join("qontinui-coord");
        std::fs::create_dir_all(&coord).unwrap();
        let web = base.join("custom-web-dir");
        std::fs::create_dir_all(&web).unwrap();
        let mut overrides = HashMap::new();
        overrides.insert("qontinui-web".to_string(), web.clone());

        // bare exact name → <root>/qontinui-coord
        assert_eq!(
            repo_dir_with("qontinui-coord", &overrides, Some(&base)).as_deref(),
            Some(coord.as_path())
        );
        // short name → <root>/qontinui-<name> (prefix convention)
        assert_eq!(
            repo_dir_with("coord", &overrides, Some(&base)).as_deref(),
            Some(coord.as_path())
        );
        // owner/name slug → trailing component
        assert_eq!(
            repo_dir_with("qontinui/qontinui-coord", &overrides, Some(&base)).as_deref(),
            Some(coord.as_path())
        );
        // override beats the sibling convention
        assert_eq!(
            repo_dir_with("qontinui-web", &overrides, Some(&base)).as_deref(),
            Some(web.as_path())
        );
        // unknown repo → None (honest miss; caller falls back to default scope)
        assert_eq!(
            repo_dir_with("definitely-absent-repo", &overrides, Some(&base)),
            None
        );
        // no workspace root + no override → None
        assert_eq!(repo_dir_with("coord", &HashMap::new(), None), None);
    }
}
