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

/// This repo's checkout directory name under the workspace root.
const RUNNER_REPO_DIR: &str = "qontinui-runner";

/// The default TS scope = the runner's own frontend project, whose
/// `tsconfig.json` sits at the runner **repo root**
/// (`<workspace-root>/qontinui-runner/tsconfig.json`).
///
/// It used to be `env!("CARGO_MANIFEST_DIR")/..` — a **compile-time** constant
/// naming the src-tauri parent on whichever machine built the binary, invisible
/// to any grep for a drive-letter literal. The plan's author-time table filed
/// this as a bundled resource; that was corrected at re-vet, because
/// `tsconfig.json` is **source**, which a packaged install has never contained
/// and never should. So this is the same sibling-repo resolution its neighbour
/// [`repo_dir`] already took, and it goes to the crate's one door
/// [`crate::workspace_paths`] (plan
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 5
/// Phase 7 — class 2).
///
/// Fail-soft, unchanged: an unresolved workspace root or an absent
/// `tsconfig.json` both yield `None`, and every caller already treats that as
/// "no default scope" rather than an error.
pub fn default_ts_scope() -> Option<Scope> {
    let root = crate::workspace_paths::workspace_root();
    default_ts_scope_in(root.as_deref())
}

/// Pure core of [`default_ts_scope`] with the workspace root injected, so the
/// layout rule is unit-testable against a synthetic tree instead of whatever
/// this machine happens to resolve to. Same wrapper/core split as
/// [`repo_dir`] / [`repo_dir_with`].
fn default_ts_scope_in(root: Option<&Path>) -> Option<Scope> {
    let tsconfig = root?.join(RUNNER_REPO_DIR).join("tsconfig.json");
    if tsconfig.exists() {
        Some(Scope::ts(&tsconfig))
    } else {
        None
    }
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
/// `QONTINUI_CODE_GRAPH_ROOTS` override, then `<workspace-root>/<name>`, then
/// `<workspace-root>/qontinui-<name>`. Returns `None` for an unknown repo or any
/// path-like selector (so the caller resolves real paths as directories and an
/// unknown selector falls back to the default scope — never an error).
///
/// **This module no longer answers "where is the workspace root".** It used to,
/// with a private `workspace_root()` whose rung 2 was
/// `env!("CARGO_MANIFEST_DIR")/../..` — the *build* machine's source tree baked
/// into the shipped binary, invisible to any grep for a `D:` literal. That
/// function is deleted, not wrapped, and the question now goes to
/// [`crate::workspace_paths`] (plan
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 1). Its
/// `QONTINUI_WORKSPACE_ROOT` env var survives as a recognised alias inside
/// `qontinui_types::paths`, so anyone who set it is unaffected.
pub fn repo_dir(selector: &str) -> Option<PathBuf> {
    let root = crate::workspace_paths::runner_workspace_root().into_root();
    repo_dir_with(selector, &registry_overrides(), root.as_deref())
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
    resolve_scope_with(scope, file, default_ts_scope)
}

/// Pure core of [`resolve_scope`] with the last rung injected, matching the
/// wrapper/core split used by [`default_ts_scope`] / [`default_ts_scope_in`] and
/// [`repo_dir`] / [`repo_dir_with`].
///
/// The default is injected because otherwise the fall-through rule cannot be
/// tested with teeth: on a machine with no resolvable workspace root
/// `default_ts_scope()` is legitimately `None`, so
/// `assert_eq!(resolve_scope(Some("/no/such/dir"), None), default_ts_scope())`
/// passes with `None == None` — it stops distinguishing "fell through to the
/// default" from "returned None because the explicit scope was bad", and it
/// reads the operator's real settings to do it.
///
/// A closure rather than a value so the rung stays LAZY: rungs 1 and 2 must not
/// pay a workspace-root resolution (a settings read plus directory probes, and a
/// WARN on a rejected probe) they never use.
fn resolve_scope_with(
    scope: Option<&str>,
    file: Option<&str>,
    default: impl FnOnce() -> Option<Scope>,
) -> Option<Scope> {
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

    default()
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

    /// An explicit scope that does not exist must fall THROUGH to the default
    /// scope rather than erroring.
    ///
    /// Driven through [`resolve_scope_with`] against an INJECTED default. The
    /// obvious spelling — `assert_eq!(resolve_scope(Some("/no/such/dir"), None),
    /// default_ts_scope())` — is vacuous on any machine with no resolvable
    /// workspace root, where both sides are `None`: it no longer distinguishes
    /// "fell through to the default" from "returned None because the explicit
    /// scope was bad", and it reads the operator's real settings to reach that
    /// non-verdict. A non-`None` injected default gives the assertion teeth
    /// everywhere and removes the last ambient read from these tests.
    #[test]
    fn nonexistent_explicit_scope_falls_back_to_default() {
        let injected = Scope::ts(Path::new("/synthetic/project/tsconfig.json"));
        let expected = injected.clone();
        assert_eq!(
            resolve_scope_with(Some("/no/such/dir/anywhere"), None, move || Some(injected)),
            Some(expected)
        );
    }

    /// …and the same fall-through with no default available is `None`, not an
    /// error — every caller already treats that as "no scope".
    #[test]
    fn nonexistent_explicit_scope_with_no_default_is_none() {
        assert_eq!(
            resolve_scope_with(Some("/no/such/dir/anywhere"), None, || None),
            None
        );
    }

    /// The default rung is LAZY: an explicit scope that resolves must not pay a
    /// workspace-root resolution. Proven by a closure that panics if called.
    #[test]
    fn a_resolving_explicit_scope_never_evaluates_the_default() {
        let f = fixture(true);
        let project = f.root.join(RUNNER_REPO_DIR);
        let scope = resolve_scope_with(project.to_str(), None, || {
            panic!("the default rung must not be evaluated when the explicit scope resolves")
        })
        .expect("the explicit project dir carries a tsconfig.json");
        assert_eq!(
            scope.key,
            normalize(&project.join("tsconfig.json")),
            "the explicit scope, not the default, must win"
        );
    }

    #[test]
    fn normalize_uses_forward_slashes() {
        let p = Path::new("C:\\foo\\bar\\tsconfig.json");
        assert_eq!(normalize(p), "C:/foo/bar/tsconfig.json");
    }

    /// A synthetic workspace root holding `qontinui-runner/tsconfig.json` —
    /// never this machine's layout, so the verdict holds on a fresh checkout and
    /// on a non-operator machine. pid + counter scoped because several worktrees
    /// run `cargo test` on this box at once; `Drop` cleans up even when an
    /// assertion fails. Same shape as `workspace_paths::tests::Fixture`.
    struct Fixture {
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fixture(with_tsconfig: bool) -> Fixture {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "qontinui_default_ts_scope_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(RUNNER_REPO_DIR)).unwrap();
        if with_tsconfig {
            std::fs::write(root.join(RUNNER_REPO_DIR).join("tsconfig.json"), "{}").unwrap();
        }
        Fixture { root }
    }

    /// The layout rule after slice 5 Phase 7: the default scope's tsconfig is
    /// `<workspace-root>/qontinui-runner/tsconfig.json` — the runner REPO root,
    /// which is source, not the build machine's `CARGO_MANIFEST_DIR` parent.
    #[test]
    fn default_ts_scope_is_the_runner_repo_tsconfig_under_the_workspace_root() {
        let f = fixture(true);
        let scope = default_ts_scope_in(Some(&f.root)).expect("tsconfig exists under the root");
        assert_eq!(scope.language, "typescript");
        assert_eq!(
            scope.key,
            normalize(&f.root.join(RUNNER_REPO_DIR).join("tsconfig.json"))
        );
        assert_eq!(scope.project, scope.key);
    }

    /// Fail-soft, both ways: a resolved root whose runner checkout carries no
    /// `tsconfig.json` yields `None`, and so does an unresolved root. Neither is
    /// an error, and neither invents a path.
    #[test]
    fn default_ts_scope_is_none_without_a_tsconfig_or_without_a_root() {
        let f = fixture(false);
        assert_eq!(default_ts_scope_in(Some(&f.root)), None);
        assert_eq!(default_ts_scope_in(None), None);
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
